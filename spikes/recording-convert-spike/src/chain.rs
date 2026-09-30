//! 上游 `DirectGpuCompositor::compose` 的等价复刻：5 次全分辨率 VideoProcessorBlt + 1 次 compute + 2 次覆盖层上传，
//! 并对每一段用 GPU 时间戳分段计时，验证“26.7ms 是多 pass 叠加”。

use std::time::Instant;

use windows::Win32::Graphics::Direct3D11::*;
use windows::Win32::Graphics::Dxgi::Common::*;
use windows::core::s;

use crate::bench::{cpu_ms, gpu_mem_mb, peak_ws_mb};
use crate::gpu::*;
use crate::shader::compile;
use crate::vp::VpBase;

const HLSL_HL: &str = r#"
Texture2D<float4> src : register(t0);
RWTexture2D<unorm float4> dst : register(u0);
cbuffer C : register(b0) { int w; int h; int cx; int cy; int r; int3 pad; };
[numthreads(16, 16, 1)]
void cs(uint3 id : SV_DispatchThreadID) {
    if (id.x >= (uint)w || id.y >= (uint)h) return;
    float4 c = src.Load(int3(id.xy, 0));
    float2 d = float2(id.xy) - float2(cx, cy);
    if (dot(d, d) < (float)(r * r)) c.rgb = lerp(c.rgb, float3(1, 1, 0), 0.3);
    dst[id.xy] = c;
}
"#;

const TILE: u32 = 256;

/// 复刻链路上下文。
struct Chain {
    gpu_ctx: ID3D11DeviceContext,
    vp: VpBase,
    scratch: [ID3D11Texture2D; 2],
    cursor_out: ID3D11Texture2D,
    overlay: ID3D11Texture2D,
    cs: ID3D11ComputeShader,
    cb: ID3D11Buffer,
    srv: Vec<ID3D11ShaderResourceView>,
    uav: ID3D11UnorderedAccessView,
    tile: Vec<u8>,
    srcs: Vec<ID3D11Texture2D>,
    dsts: Vec<ID3D11Texture2D>,
    size: (u32, u32),
}

impl Chain {
    fn new(gpu: &Gpu, size: (u32, u32), srcs: &[ID3D11Texture2D], dsts: &[ID3D11Texture2D]) -> Res<Self> {
        let rt = (D3D11_BIND_SHADER_RESOURCE | D3D11_BIND_RENDER_TARGET).0 as u32;
        let bgra = DXGI_FORMAT_B8G8R8A8_UNORM;
        let scratch = [gpu.tex(size.0, size.1, bgra, rt, None)?, gpu.tex(size.0, size.1, bgra, rt, None)?];
        let cursor_out =
            gpu.tex(size.0, size.1, bgra, rt | D3D11_BIND_UNORDERED_ACCESS.0 as u32, None)?;
        let overlay = gpu.tex(size.0, size.1, bgra, rt, None)?;
        let b = compile(HLSL_HL, s!("cs"), s!("cs_5_0"))?;
        let mut cs = None;
        unsafe { gpu.dev.CreateComputeShader(&b, None, Some(&mut cs))? };
        let bd = D3D11_BUFFER_DESC {
            ByteWidth: 32,
            Usage: D3D11_USAGE_DEFAULT,
            BindFlags: D3D11_BIND_CONSTANT_BUFFER.0 as u32,
            ..Default::default()
        };
        let mut cb = None;
        unsafe { gpu.dev.CreateBuffer(&bd, None, Some(&mut cb))? };
        let mut srv = Vec::new();
        for t in [&scratch[0], &scratch[1], &cursor_out] {
            let mut v = None;
            unsafe { gpu.dev.CreateShaderResourceView(t, None, Some(&mut v))? };
            srv.push(v.ok_or("SRV")?);
        }
        let mut uav = None;
        unsafe { gpu.dev.CreateUnorderedAccessView(&cursor_out, None, Some(&mut uav))? };
        let vp = VpBase::new(gpu, size, size, 60, vec![])?;
        Ok(Self {
            gpu_ctx: gpu.ctx.clone(),
            vp,
            scratch,
            cursor_out,
            overlay,
            cs: cs.ok_or("cs")?,
            cb: cb.ok_or("cb")?,
            srv,
            uav: uav.ok_or("uav")?,
            tile: vec![0x40; (TILE * TILE * 4) as usize],
            srcs: srcs.to_vec(),
            dsts: dsts.to_vec(),
            size,
        })
    }

    /// 执行第 `seg` 段（0..5）。
    fn segment(&self, seg: usize, i: usize) -> Res<()> {
        let c = &self.gpu_ctx;
        let src = &self.srcs[i % self.srcs.len()];
        let dst = &self.dsts[i % self.dsts.len()];
        match seg {
            0 => self.vp.blit(&[(src, false)], &self.scratch[0]),
            1 => {
                let k = [self.size.0 as i32, self.size.1 as i32, 1200 + (i as i32 % 50), 700, 40, 0, 0, 0];
                unsafe {
                    c.UpdateSubresource(&self.cb, 0, None, k.as_ptr().cast(), 0, 0);
                    c.CSSetShader(&self.cs, None);
                    c.CSSetConstantBuffers(0, Some(&[Some(self.cb.clone())]));
                    c.CSSetShaderResources(0, Some(&[Some(self.srv[0].clone())]));
                    c.CSSetUnorderedAccessViews(0, 1, Some(&Some(self.uav.clone())), None);
                    c.Dispatch(self.size.0.div_ceil(16), self.size.1.div_ceil(16), 1);
                    c.CSSetShaderResources(0, Some(&[None]));
                    c.CSSetUnorderedAccessViews(0, 1, Some(&None), None);
                    c.CSSetShader(None::<&ID3D11ComputeShader>, None);
                }
                self.vp.blit(&[(&self.cursor_out, false)], &self.scratch[1])
            }
            2 | 3 => {
                let b = D3D11_BOX { left: 64, top: 64, front: 0, right: 64 + TILE, bottom: 64 + TILE, back: 1 };
                unsafe {
                    c.UpdateSubresource(&self.overlay, 0, Some(&b), self.tile.as_ptr().cast(), TILE * 4, 0);
                }
                if seg == 2 {
                    self.vp.blit(&[(&self.scratch[1], false), (&self.overlay, true)], &self.scratch[0])
                } else {
                    self.vp.blit(&[(&self.scratch[0], false), (&self.overlay, true)], &self.scratch[1])
                }
            }
            _ => self.vp.blit(&[(&self.scratch[1], false)], dst),
        }
    }
}

const NAMES: [&str; 5] =
    ["desktop_blit", "highlight_cs+blit", "effects_upload+blit", "keyboard_upload+blit", "nv12_blit"];

/// 运行分段 + 整体两阶段基准，返回 JSON 行。
pub fn bench_chain(
    gpu: &Gpu,
    label: &str,
    size: (u32, u32),
    srcs: &[ID3D11Texture2D],
    dsts: &[ID3D11Texture2D],
    warmup: usize,
    frames: usize,
) -> Res<String> {
    let ch = Chain::new(gpu, size, srcs, dsts)?;
    const DEPTH: u64 = 2;
    let mut fence = Fence::new(gpu)?;
    // 阶段 A：分段计时。同时记录 GPU 时间戳与“串行提交+等完成”的墙钟时间
    // （VideoProcessor 跑在视频引擎上，时间戳常常测不到它，墙钟更可信）。
    let mut timers: Vec<GpuTimer> = (0..5).map(|_| GpuTimer::new(&gpu.dev, 8)).collect::<Res<_>>()?;
    let mut seg_ms: Vec<Vec<f64>> = vec![Vec::new(); 5];
    let mut seg_sync: Vec<Vec<f64>> = vec![Vec::new(); 5];
    for i in 0..warmup + frames {
        for s in 0..5 {
            let m = i >= warmup;
            if m {
                timers[s].begin(&gpu.ctx, &mut seg_ms[s]);
            }
            let t = Instant::now();
            ch.segment(s, i)?;
            if m {
                timers[s].end(&gpu.ctx);
            }
            let v = fence.signal();
            fence.wait(v);
            if m {
                seg_sync[s].push(t.elapsed().as_secs_f64() * 1000.0);
            }
        }
    }
    gpu.wait_idle()?;
    for s in 0..5 {
        timers[s].drain(&gpu.ctx, true, &mut seg_ms[s]);
    }
    // 阶段 B：整体（上游写法的真实总耗时，不做逐段同步）
    let mut total_timer = GpuTimer::new(&gpu.dev, 8)?;
    let (mut tot, mut cpu, mut serial) = (Vec::new(), Vec::new(), Vec::new());
    let (mut wall, mut cpu0) = (Instant::now(), 0.0);
    for i in 0..warmup + frames {
        if i == warmup {
            gpu.wait_idle()?;
            wall = Instant::now();
            cpu0 = cpu_ms();
        }
        if i >= warmup {
            total_timer.begin(&gpu.ctx, &mut tot);
        }
        let t = Instant::now();
        for s in 0..5 {
            ch.segment(s, i)?;
        }
        if i >= warmup {
            cpu.push(t.elapsed().as_secs_f64() * 1000.0);
            total_timer.end(&gpu.ctx);
        }
        let v = fence.signal();
        if v > DEPTH {
            fence.wait(v - DEPTH);
        }
    }
    gpu.wait_idle()?;
    let secs = wall.elapsed().as_secs_f64();
    let used = cpu_ms() - cpu0;
    total_timer.drain(&gpu.ctx, true, &mut tot);
    // 阶段 C：串行整体墙钟（一帧从提交到 GPU 全部完成）
    for i in 0..frames.min(300) {
        let t = Instant::now();
        for s in 0..5 {
            ch.segment(s, i)?;
        }
        let v = fence.signal();
        fence.wait(v);
        serial.push(t.elapsed().as_secs_f64() * 1000.0);
    }
    let segs: Vec<String> = (0..5)
        .map(|s| format!(
                "{{\"name\":\"{}\",\"gpu_ts_ms\":{},\"sync_ms\":{}}}",
                NAMES[s],
                Stats::from(&seg_ms[s]).json(),
                Stats::from(&seg_sync[s]).json()
            ))
        .collect();
    let sum_p50: f64 = (0..5).map(|s| Stats::from(&seg_sync[s]).p50).sum();
    let m = gpu_mem_mb(gpu);
    Ok(format!(
        "{{\"kind\":\"chain\",\"res\":\"{label}\",\"segments\":[{}],\"sum_sync_p50_ms\":{:.3},\"serial_total_ms\":{},\"total_gpu_ts_ms\":{},\"total_cpu_ms\":{},\"fps\":{:.1},\"proc_cpu_pct\":{:.1},\"peak_ws_mb\":{:.0},\"gpu_local_mb\":{:.0},\"gpu_nonlocal_mb\":{:.0}}}",
        segs.join(","),
        sum_p50,
        Stats::from(&serial).json(),
        Stats::from(&tot).json(),
        Stats::from(&cpu).json(),
        frames as f64 / secs,
        used / (secs * 1000.0) * 100.0,
        peak_ws_mb(),
        m.0,
        m.1
    ))
}
