//! 公共 GPU 基础设施：设备创建、合成输入纹理、GPU 计时、纹理回读。

use std::error::Error;

use windows::Win32::Foundation::{CloseHandle, HANDLE, HMODULE};
use windows::Win32::System::Threading::{CreateEventW, INFINITE, WaitForSingleObject};
use windows::Win32::Graphics::Direct3D::*;
use windows::Win32::Graphics::Direct3D11::*;
use windows::Win32::Graphics::Dxgi::Common::*;
use windows::Win32::Graphics::Dxgi::*;
use windows::core::Interface;

/// 通用结果类型。
pub type Res<T> = Result<T, Box<dyn Error>>;

/// 把任意可显示错误包成 Box<dyn Error>。
pub fn err<T>(msg: impl Into<String>) -> Res<T> {
    Err(msg.into().into())
}

/// 一个 D3D11 设备及其上下文。
pub struct Gpu {
    /// D3D11 设备。
    pub dev: ID3D11Device,
    /// 立即上下文。
    pub ctx: ID3D11DeviceContext,
    /// 多线程保护接口（与上游一致：开启保护）。
    pub mt: ID3D11Multithread,
    /// 所选适配器。
    pub adapter: IDXGIAdapter1,
    /// 适配器描述。
    pub name: String,
    /// 实际获得的 Feature Level。
    pub feature_level: i32,
}

impl Gpu {
    /// 创建设备：优先 Intel 适配器（本机核显），否则第 0 个。
    /// 示例：`let gpu = Gpu::new()?;`
    pub fn new() -> Res<Self> {
        let factory: IDXGIFactory1 = unsafe { CreateDXGIFactory1() }?;
        let mut pick: Option<(IDXGIAdapter1, DXGI_ADAPTER_DESC1)> = None;
        let mut i = 0;
        while let Ok(a) = unsafe { factory.EnumAdapters1(i) } {
            let d = unsafe { a.GetDesc1() }?;
            if pick.is_none() || d.VendorId == 0x8086 {
                if d.VendorId == 0x8086 || pick.is_none() {
                    pick = Some((a, d));
                }
            }
            i += 1;
        }
        let (adapter, desc) = pick.ok_or("没有 DXGI 适配器")?;
        let name = String::from_utf16_lossy(
            &desc.Description[..desc.Description.iter().position(|c| *c == 0).unwrap_or(128)],
        );
        let mut dev = None;
        let mut ctx = None;
        let mut fl = D3D_FEATURE_LEVEL_11_0;
        unsafe {
            D3D11CreateDevice(
                &adapter,
                D3D_DRIVER_TYPE_UNKNOWN,
                HMODULE::default(),
                D3D11_CREATE_DEVICE_BGRA_SUPPORT | D3D11_CREATE_DEVICE_VIDEO_SUPPORT,
                Some(&[D3D_FEATURE_LEVEL_11_1, D3D_FEATURE_LEVEL_11_0]),
                D3D11_SDK_VERSION,
                Some(&mut dev),
                Some(&mut fl),
                Some(&mut ctx),
            )?;
        }
        let dev = dev.ok_or("无设备")?;
        let ctx = ctx.ok_or("无上下文")?;
        let mt: ID3D11Multithread = ctx.cast()?;
        unsafe {
            let _ = mt.SetMultithreadProtected(true);
        }
        Ok(Self { dev, ctx, mt, adapter, name, feature_level: fl.0 })
    }

    /// 创建 2D 纹理（可带初始数据）。
    /// 参数：格式、绑定标志、初始像素（行距 = 宽*4，仅 BGRA）。
    pub fn tex(
        &self,
        w: u32,
        h: u32,
        fmt: DXGI_FORMAT,
        bind: u32,
        init: Option<&[u8]>,
    ) -> Res<ID3D11Texture2D> {
        let desc = D3D11_TEXTURE2D_DESC {
            Width: w,
            Height: h,
            MipLevels: 1,
            ArraySize: 1,
            Format: fmt,
            SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
            Usage: D3D11_USAGE_DEFAULT,
            BindFlags: bind,
            ..Default::default()
        };
        let data = init.map(|p| D3D11_SUBRESOURCE_DATA {
            pSysMem: p.as_ptr().cast(),
            SysMemPitch: w * 4,
            SysMemSlicePitch: 0,
        });
        let mut t = None;
        unsafe {
            self.dev.CreateTexture2D(
                &desc,
                data.as_ref().map(|d| d as *const _),
                Some(&mut t),
            )?;
        }
        Ok(t.ok_or("CreateTexture2D 返回空")?)
    }

    /// 等待 GPU 空闲（D3D11 Fence，阻塞等待，不空转）。
    pub fn wait_idle(&self) -> Res<()> {
        let mut f = Fence::new(self)?;
        let v = f.signal();
        f.wait(v);
        Ok(())
    }
}

/// D3D11 Fence 封装：Signal 后可阻塞等待或轮询。
pub struct Fence {
    ctx: ID3D11DeviceContext4,
    fence: ID3D11Fence,
    event: HANDLE,
    value: u64,
}

impl Fence {
    /// 在设备上创建 Fence（需要 D3D11.4 / Win10 1703+）。
    pub fn new(gpu: &Gpu) -> Res<Self> {
        let dev: ID3D11Device5 = gpu.dev.cast()?;
        let ctx: ID3D11DeviceContext4 = gpu.ctx.cast()?;
        let mut fence: Option<ID3D11Fence> = None;
        unsafe { dev.CreateFence(0, D3D11_FENCE_FLAG_NONE, &mut fence) }?;
        let fence = fence.ok_or("CreateFence 返回空")?;
        let event = unsafe { CreateEventW(None, false, false, None) }?;
        Ok(Self { ctx, fence, event, value: 0 })
    }

    /// 在当前命令流末尾插入信号，返回其值。
    pub fn signal(&mut self) -> u64 {
        self.value += 1;
        unsafe {
            let _ = self.ctx.Signal(&self.fence, self.value);
            self.ctx.Flush();
        }
        self.value
    }

    /// 阻塞等待信号值完成。
    pub fn wait(&self, v: u64) {
        unsafe {
            if self.fence.GetCompletedValue() < v {
                let _ = self.fence.SetEventOnCompletion(v, self.event);
                WaitForSingleObject(self.event, INFINITE);
            }
        }
    }

    /// 轮询：信号值是否已完成。
    pub fn done(&self, v: u64) -> bool {
        unsafe { self.fence.GetCompletedValue() >= v }
    }
}

impl Drop for Fence {
    fn drop(&mut self) {
        unsafe {
            let _ = CloseHandle(self.event);
        }
    }
}

/// 生成一帧合成 BGRA 内容：渐变背景 + 随帧移动的纯色条 + 饱和色块；`noise` 为真时下 1/3 为随机噪声（最坏情况）。
/// 参数：宽、高、帧序号（决定内容）、是否含噪声。返回：w*h*4 字节。
pub fn gen_frame(w: u32, h: u32, idx: u32, noise: bool) -> Vec<u8> {
    let mut px = vec![0u8; (w * h * 4) as usize];
    let mut s: u32 = 0x9E37_79B9 ^ idx.wrapping_mul(2_654_435_761);
    let bar_w = (w / 16).max(1);
    let shift = (idx * 37) % w;
    for y in 0..h {
        for x in 0..w {
            let o = ((y * w + x) * 4) as usize;
            let (mut r, mut g, mut b) =
                ((x * 255 / w) as u8, (y * 255 / h) as u8, (idx.wrapping_mul(37) & 255) as u8);
            if y < h / 3 {
                let k = ((x + shift) / bar_w) % 8;
                let c = [
                    (255, 0, 0),
                    (0, 255, 0),
                    (0, 0, 255),
                    (255, 255, 0),
                    (0, 255, 255),
                    (255, 0, 255),
                    (255, 255, 255),
                    (0, 0, 0),
                ][k as usize];
                (r, g, b) = c;
            } else if noise && y >= h * 2 / 3 {
                s ^= s << 13;
                s ^= s >> 17;
                s ^= s << 5;
                r = s as u8;
                g = (s >> 8) as u8;
                b = (s >> 16) as u8;
            }
            px[o] = b;
            px[o + 1] = g;
            px[o + 2] = r;
            px[o + 3] = 255;
        }
    }
    px
}

/// 合成输入池：K 张内容各异的 BGRA 纹理，按帧序号循环，避免任何缓存命中。
pub struct SourcePool {
    /// 纹理列表。
    pub tex: Vec<ID3D11Texture2D>,
    /// 宽。
    pub w: u32,
    /// 高。
    pub h: u32,
    /// 可选保留的 CPU 像素（正确性校验用）。
    pub pixels: Vec<Vec<u8>>,
}

impl SourcePool {
    /// 创建输入池；`keep` 为真时保留 CPU 像素。
    pub fn new(gpu: &Gpu, w: u32, h: u32, count: u32, keep: bool, noise: bool) -> Res<Self> {
        let bind = (D3D11_BIND_SHADER_RESOURCE | D3D11_BIND_RENDER_TARGET).0 as u32;
        let mut tex = Vec::new();
        let mut pixels = Vec::new();
        for i in 0..count {
            let p = gen_frame(w, h, i, noise);
            tex.push(gpu.tex(w, h, DXGI_FORMAT_B8G8R8A8_UNORM, bind, Some(&p))?);
            if keep {
                pixels.push(p);
            }
        }
        Ok(Self { tex, w, h, pixels })
    }
}

/// 一个计时槽位。
struct Slot {
    disjoint: ID3D11Query,
    t0: ID3D11Query,
    t1: ID3D11Query,
    pending: bool,
}

/// GPU 时间戳计时环（非阻塞取回，避免读回引入停顿）。
pub struct GpuTimer {
    slots: Vec<Slot>,
    next: usize,
}

impl GpuTimer {
    /// 创建含 `n` 个槽位的计时环。
    pub fn new(dev: &ID3D11Device, n: usize) -> Res<Self> {
        let mk = |q| -> Res<ID3D11Query> {
            let d = D3D11_QUERY_DESC { Query: q, MiscFlags: 0 };
            let mut o = None;
            unsafe { dev.CreateQuery(&d, Some(&mut o))? };
            Ok(o.ok_or("CreateQuery 失败")?)
        };
        let mut slots = Vec::new();
        for _ in 0..n {
            slots.push(Slot {
                disjoint: mk(D3D11_QUERY_TIMESTAMP_DISJOINT)?,
                t0: mk(D3D11_QUERY_TIMESTAMP)?,
                t1: mk(D3D11_QUERY_TIMESTAMP)?,
                pending: false,
            });
        }
        Ok(Self { slots, next: 0 })
    }

    /// 开始计时；若槽位仍被占用先阻塞取回（结果追加到 `out`）。
    pub fn begin(&mut self, ctx: &ID3D11DeviceContext, out: &mut Vec<f64>) {
        if self.slots[self.next].pending {
            self.collect_slot(ctx, self.next, true, out);
        }
        let s = &self.slots[self.next];
        unsafe {
            ctx.Begin(&s.disjoint);
            ctx.End(&s.t0);
        }
    }

    /// 结束计时并推进槽位。
    pub fn end(&mut self, ctx: &ID3D11DeviceContext) {
        let s = &mut self.slots[self.next];
        unsafe {
            ctx.End(&s.t1);
            ctx.End(&s.disjoint);
        }
        s.pending = true;
        self.next = (self.next + 1) % self.slots.len();
    }

    /// 取回全部已完成槽位（`block` 为真则等待全部完成）。
    pub fn drain(&mut self, ctx: &ID3D11DeviceContext, block: bool, out: &mut Vec<f64>) {
        for i in 0..self.slots.len() {
            let idx = (self.next + i) % self.slots.len();
            if self.slots[idx].pending {
                self.collect_slot(ctx, idx, block, out);
            }
        }
    }

    fn collect_slot(&mut self, ctx: &ID3D11DeviceContext, i: usize, block: bool, out: &mut Vec<f64>) {
        let s = &mut self.slots[i];
        loop {
            let mut dj = D3D11_QUERY_DATA_TIMESTAMP_DISJOINT::default();
            // 哨兵：Frequency 为 0 视为未就绪
            let _ = unsafe {
                ctx.GetData(
                    &s.disjoint,
                    Some((&mut dj as *mut D3D11_QUERY_DATA_TIMESTAMP_DISJOINT).cast()),
                    std::mem::size_of::<D3D11_QUERY_DATA_TIMESTAMP_DISJOINT>() as u32,
                    0,
                )
            };
            let (mut a, mut b) = (0u64, 0u64);
            let _ = unsafe {
                ctx.GetData(&s.t0, Some((&mut a as *mut u64).cast()), 8, 0)
            };
            let _ = unsafe {
                ctx.GetData(&s.t1, Some((&mut b as *mut u64).cast()), 8, 0)
            };
            if dj.Frequency != 0 && a != 0 && b != 0 {
                if !dj.Disjoint.as_bool() {
                    out.push((b - a) as f64 * 1000.0 / dj.Frequency as f64);
                }
                s.pending = false;
                return;
            }
            if !block {
                return;
            }
            unsafe { ctx.Flush() };
            std::hint::spin_loop();
        }
    }
}

/// NV12 输出环：可选绑定标志。
pub fn nv12_ring(gpu: &Gpu, w: u32, h: u32, n: usize, bind: u32) -> Res<Vec<ID3D11Texture2D>> {
    let mut v = Vec::new();
    for _ in 0..n {
        v.push(gpu.tex(w, h, DXGI_FORMAT_NV12, bind, None)?);
    }
    Ok(v)
}

/// NV12 回读结果。
pub struct Nv12 {
    /// 宽。
    pub w: u32,
    /// 高。
    pub h: u32,
    /// Y 平面（w*h，紧凑）。
    pub y: Vec<u8>,
    /// UV 平面（w*(h/2)，紧凑，UVUV 交错）。
    pub uv: Vec<u8>,
}

/// 回读 NV12 纹理的有效区域 w*h。
pub fn read_nv12(gpu: &Gpu, tex: &ID3D11Texture2D, w: u32, h: u32) -> Res<Nv12> {
    let mut desc = D3D11_TEXTURE2D_DESC::default();
    unsafe { tex.GetDesc(&mut desc) };
    let mut sd = desc;
    sd.BindFlags = 0;
    sd.MiscFlags = 0;
    sd.Usage = D3D11_USAGE_STAGING;
    sd.CPUAccessFlags = D3D11_CPU_ACCESS_READ.0 as u32;
    let mut st = None;
    unsafe { gpu.dev.CreateTexture2D(&sd, None, Some(&mut st))? };
    let st = st.ok_or("staging 失败")?;
    unsafe { gpu.ctx.CopyResource(&st, tex) };
    let mut m = D3D11_MAPPED_SUBRESOURCE::default();
    unsafe { gpu.ctx.Map(&st, 0, D3D11_MAP_READ, 0, Some(&mut m))? };
    let pitch = m.RowPitch as usize;
    let base = m.pData.cast::<u8>();
    let mut y = Vec::with_capacity((w * h) as usize);
    let mut uv = Vec::with_capacity((w * h / 2) as usize);
    unsafe {
        for r in 0..h as usize {
            y.extend_from_slice(std::slice::from_raw_parts(base.add(r * pitch), w as usize));
        }
        // UV 平面紧跟在完整纹理高度的 Y 平面之后
        let uv_base = base.add(pitch * desc.Height as usize);
        for r in 0..(h / 2) as usize {
            uv.extend_from_slice(std::slice::from_raw_parts(uv_base.add(r * pitch), w as usize));
        }
        gpu.ctx.Unmap(&st, 0);
    }
    Ok(Nv12 { w, h, y, uv })
}

/// 分位数统计结果。
#[derive(Clone, Debug, Default)]
pub struct Stats {
    /// 样本数。
    pub n: usize,
    /// 平均。
    pub mean: f64,
    /// P50。
    pub p50: f64,
    /// P95。
    pub p95: f64,
    /// P99。
    pub p99: f64,
    /// 最大。
    pub max: f64,
}

impl Stats {
    /// 由样本计算统计（会排序副本）。
    /// 示例：`Stats::from(&[1.0, 2.0, 3.0]).p50 == 2.0`
    pub fn from(v: &[f64]) -> Self {
        if v.is_empty() {
            return Self::default();
        }
        let mut s = v.to_vec();
        s.sort_by(|a, b| a.partial_cmp(b).unwrap());
        let pct = |p: f64| s[(((s.len() - 1) as f64) * p).round() as usize];
        Self {
            n: s.len(),
            mean: s.iter().sum::<f64>() / s.len() as f64,
            p50: pct(0.5),
            p95: pct(0.95),
            p99: pct(0.99),
            max: *s.last().unwrap(),
        }
    }

    /// 序列化为 JSON 对象。
    pub fn json(&self) -> String {
        format!(
            "{{\"n\":{},\"mean\":{:.3},\"p50\":{:.3},\"p95\":{:.3},\"p99\":{:.3},\"max\":{:.3}}}",
            self.n, self.mean, self.p50, self.p95, self.p99, self.max
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stats_percentiles() {
        let v: Vec<f64> = (1..=100).map(|x| x as f64).collect();
        let s = Stats::from(&v);
        assert_eq!(s.n, 100);
        assert!((s.mean - 50.5).abs() < 1e-9);
        assert_eq!(s.p50, 51.0);
        assert_eq!(s.max, 100.0);
        assert_eq!(Stats::from(&[]).n, 0);
    }

    #[test]
    fn frames_differ_between_indices() {
        let a = gen_frame(64, 48, 0, true);
        let b = gen_frame(64, 48, 1, true);
        assert_eq!(a.len(), 64 * 48 * 4);
        assert_ne!(a, b);
        assert_eq!(a, gen_frame(64, 48, 0, true));
        assert_ne!(gen_frame(64, 48, 0, false), a);
        assert!(a.chunks_exact(4).all(|p| p[3] == 255));
    }
}
