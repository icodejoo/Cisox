//! 纯转换基准循环 + 进程/显存指标。

use std::time::Instant;

use windows::Win32::Foundation::FILETIME;
use windows::Win32::Graphics::Direct3D11::ID3D11Texture2D;
use windows::Win32::Graphics::Dxgi::*;
use windows::Win32::System::ProcessStatus::{GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS};
use windows::Win32::System::Threading::{GetCurrentProcess, GetProcessTimes};
use windows::core::Interface;

use crate::gpu::{Fence, Gpu, GpuTimer, Res, Stats};

/// 转换器统一接口：按序号取输入纹理，输出到给定 NV12 纹理，提交一次 BGRA->NV12。
pub trait Conv {
    /// 方案名。
    fn name(&self) -> String;
    /// 提交一帧转换（`src` 会对池大小取模）。
    fn run(&mut self, src: usize, dst: &ID3D11Texture2D) -> Res<()>;
}

fn ft(f: FILETIME) -> u64 {
    ((f.dwHighDateTime as u64) << 32) | f.dwLowDateTime as u64
}

/// 进程累计 CPU 时间（内核+用户，毫秒）。
pub fn cpu_ms() -> f64 {
    let (mut c, mut e, mut k, mut u) = (FILETIME::default(), FILETIME::default(), FILETIME::default(), FILETIME::default());
    unsafe {
        let _ = GetProcessTimes(GetCurrentProcess(), &mut c, &mut e, &mut k, &mut u);
    }
    (ft(k) + ft(u)) as f64 / 10_000.0
}

/// 进程峰值工作集（MB）。
pub fn peak_ws_mb() -> f64 {
    let mut m = PROCESS_MEMORY_COUNTERS::default();
    m.cb = std::mem::size_of::<PROCESS_MEMORY_COUNTERS>() as u32;
    unsafe {
        let _ = GetProcessMemoryInfo(GetCurrentProcess(), &mut m, m.cb);
    }
    m.PeakWorkingSetSize as f64 / 1048576.0
}

/// 本进程在适配器上的显存占用（local, non-local，MB）。核显下 local 也是共享内存的一部分。
pub fn gpu_mem_mb(gpu: &Gpu) -> (f64, f64) {
    let Ok(a3) = gpu.adapter.cast::<IDXGIAdapter3>() else { return (0.0, 0.0) };
    let q = |seg| {
        let mut i = DXGI_QUERY_VIDEO_MEMORY_INFO::default();
        match unsafe { a3.QueryVideoMemoryInfo(0, seg, &mut i) } {
            Ok(()) => i.CurrentUsage as f64 / 1048576.0,
            Err(_) => 0.0,
        }
    };
    (q(DXGI_MEMORY_SEGMENT_GROUP_LOCAL), q(DXGI_MEMORY_SEGMENT_GROUP_NON_LOCAL))
}

/// 纯转换基准结果。
pub struct ConvResult {
    /// GPU 耗时（ms）。
    pub gpu: Stats,
    /// CPU 提交耗时（ms）。
    pub cpu: Stats,
    /// 串行单帧墙钟耗时（提交 + 等 GPU 完成，ms）。
    pub sync: Stats,
    /// 吞吐（帧/秒，受 GPU 完成节流）。
    pub fps: f64,
    /// 进程 CPU 占用（单核当量，%）。
    pub proc_cpu_pct: f64,
    /// 峰值工作集（MB）。
    pub peak_ws: f64,
    /// 显存（local, non-local）。
    pub mem: (f64, f64),
}

/// 运行纯转换基准：丢弃 `warmup` 帧，测 `frames` 帧；GPU 队列深度限制为 3 帧。
pub fn bench_convert(gpu: &Gpu, conv: &mut dyn Conv, dsts: &[ID3D11Texture2D], warmup: usize, frames: usize) -> Res<ConvResult> {
    const DEPTH: u64 = 3;
    let mut timer = GpuTimer::new(&gpu.dev, 8)?;
    let mut fence = Fence::new(gpu)?;
    let (mut gpu_ms, mut cpu_ms_v) = (Vec::new(), Vec::new());
    let (mut wall, mut cpu0) = (Instant::now(), 0.0);
    let mut mem = (0.0f64, 0.0f64);
    for i in 0..warmup + frames {
        let measured = i >= warmup;
        if i == warmup {
            gpu.wait_idle()?;
            wall = Instant::now();
            cpu0 = cpu_ms();
        }
        if measured {
            timer.begin(&gpu.ctx, &mut gpu_ms);
        }
        let t = Instant::now();
        conv.run(i, &dsts[i % dsts.len()])?;
        let dt = t.elapsed().as_secs_f64() * 1000.0;
        if measured {
            timer.end(&gpu.ctx);
            cpu_ms_v.push(dt);
        }
        // 节流：GPU 队列最多领先 DEPTH 帧（阻塞等待，不空转）
        let v = fence.signal();
        if v > DEPTH {
            fence.wait(v - DEPTH);
        }
        if i % 100 == 0 {
            let m = gpu_mem_mb(gpu);
            mem = (mem.0.max(m.0), mem.1.max(m.1));
        }
    }
    gpu.wait_idle()?;
    let secs = wall.elapsed().as_secs_f64();
    let cpu_used = cpu_ms() - cpu0;
    timer.drain(&gpu.ctx, true, &mut gpu_ms);
    // 串行阶段：每帧“提交 + 等 GPU 完成”的墙钟时间（VP 引擎不被时间戳覆盖时，这是可比的单帧耗时）
    let mut sync_ms = Vec::new();
    for i in 0..frames.min(300) {
        let t = Instant::now();
        conv.run(i, &dsts[i % dsts.len()])?;
        let v = fence.signal();
        fence.wait(v);
        sync_ms.push(t.elapsed().as_secs_f64() * 1000.0);
    }
    let m = gpu_mem_mb(gpu);
    mem = (mem.0.max(m.0), mem.1.max(m.1));
    Ok(ConvResult {
        sync: Stats::from(&sync_ms),
        gpu: Stats::from(&gpu_ms),
        cpu: Stats::from(&cpu_ms_v),
        fps: frames as f64 / secs,
        proc_cpu_pct: cpu_used / (secs * 1000.0) * 100.0,
        peak_ws: peak_ws_mb(),
        mem,
    })
}

impl ConvResult {
    /// 序列化为 JSON 片段（不含外层花括号）。
    pub fn json(&self) -> String {
        format!(
            "\"gpu_ms\":{},\"cpu_ms\":{},\"sync_ms\":{},\"fps\":{:.1},\"proc_cpu_pct\":{:.1},\"peak_ws_mb\":{:.0},\"gpu_local_mb\":{:.0},\"gpu_nonlocal_mb\":{:.0}",
            self.gpu.json(),
            self.cpu.json(),
            self.sync.json(),
            self.fps,
            self.proc_cpu_pct,
            self.peak_ws,
            self.mem.0,
            self.mem.1
        )
    }
}
