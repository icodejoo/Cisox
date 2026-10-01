//! 自建 DXGI 桌面复制采集（捕获阶段的 Windows 实现）。
//!
//! 设计要点：
//! - 采集独占一个 D3D11 设备 A，合成与编码用另一个设备 B：`AcquireNextFrame` 等在多线程保护的设备锁上，
//!   和 VideoProcessor/编码共用一个设备时，对方持锁的几毫秒就会让采集漏掉呈现；分设备后互不阻塞。
//! - 桌面内容复制进共享纹理池（NT 句柄共享），设备间用 D3D11 栅栏在 GPU 侧同步：
//!   A 复制完 `Signal`，B 合成前 `Wait`，B 合成完再 `Signal`，A 复用该槽前 `Wait`；CPU 不阻塞。
//! - 只复制选区那一块（不是整屏），选区小于显示器时带宽更省。
//! - 不用阻塞式 `AcquireNextFrame`（它在等待期间占着设备锁）：零超时取帧 + 亚毫秒睡眠轮询。
//!
//! 只支持单显示器内的选区、SDR（BGRA）、不旋转的输出；其余情形返回错误，由调用方回落软件路径。

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use snow_cursor::{AttachedCursorSample, CursorProjector, CursorSampler, CursorShape, CursorShapeState, CursorTargetInfo};
use snow_d3d11::SharedDevice;
use windows::Win32::Foundation::{CloseHandle, HANDLE};
use windows::Win32::Graphics::Direct3D11::*;
use windows::Win32::Graphics::Dxgi::Common::{DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_SAMPLE_DESC};
use windows::Win32::Graphics::Dxgi::{
    CreateDXGIFactory1, DXGI_ERROR_ACCESS_LOST, DXGI_ERROR_WAIT_TIMEOUT, DXGI_OUTDUPL_FRAME_INFO, DXGI_SHARED_RESOURCE_READ,
    DXGI_SHARED_RESOURCE_WRITE, IDXGIAdapter, IDXGIAdapter1, IDXGIFactory1, IDXGIOutput, IDXGIOutput1, IDXGIOutputDuplication, IDXGIResource, IDXGIResource1,
};
use windows::core::{Interface, PCWSTR};

use crate::frametrace::{IterStage, Thread, TraceBuf, code};
use crate::geom::Rect;
use crate::pipeline::{CaptureDiag, CaptureFault, CaptureSource, CaptureStats, Captured};
use crate::settings::{AdapterInfo, EncoderPreference, rank_adapters};
use crate::timeline::QpcAnchor;

/// 共享纹理池容量（采集队列 + 待输出队列 + 最近帧 + 合成中都占用名额）。
pub const POOL_SIZE: usize = 10;
/// 零超时轮询之间的睡眠：阻塞式 `AcquireNextFrame` 会在等待期间占着设备锁，所以改成"零超时取帧 + 亚毫秒睡眠"。
pub const POLL_SLEEP: Duration = Duration::from_micros(400);
/// 诊断样本上限。
pub(crate) const DIAG_LIMIT: usize = 20_000;
/// 栅栏共享句柄的访问权限（GENERIC_ALL）。
const FENCE_ACCESS: u32 = 0x1000_0000;

/// 共享纹理槽：同一块显存在设备 A、B 上各有一个视图，两边各有一个栅栏视图。
pub struct SharedSlot {
    /// 设备 A 上的纹理（采集写入）。
    tex_a: ID3D11Texture2D,
    /// 设备 B 上的纹理（合成读取）。
    tex_b: ID3D11Texture2D,
    /// 设备 A 上的栅栏。
    fence_a: ID3D11Fence,
    /// 设备 B 上的栅栏（与 A 的栅栏是同一个）。
    fence_b: ID3D11Fence,
    /// 栅栏当前值（最后一次 Signal 的值；只在槽空闲或独占使用时更新）。
    value: AtomicU64,
}

impl SharedSlot {
    /// 设备 B 上的纹理（合成用）。
    pub fn texture_b(&self) -> &ID3D11Texture2D {
        &self.tex_b
    }

    /// 设备 B 上的栅栏。
    pub fn fence_b(&self) -> &ID3D11Fence {
        &self.fence_b
    }

    /// 当前栅栏值。
    pub fn value(&self) -> u64 {
        self.value.load(Ordering::Acquire)
    }

    /// 记录新的栅栏值。
    pub fn set_value(&self, value: u64) {
        self.value.store(value, Ordering::Release);
    }
}

/// 跨屏帧里的一块：某个显示器落入选区的裁剪块（槽尺寸 = 块尺寸）。
#[derive(Clone)]
pub struct Tile {
    /// 共享槽。
    pub slot: Arc<SharedSlot>,
    /// 块在选区内的矩形（选区坐标，1:1 物理像素）。
    pub rect: Rect,
}

/// 采集帧句柄：引用计数的共享槽；所有持有者释放后，采集才会复用该槽。
#[derive(Clone)]
pub struct GpuFrame {
    /// 共享槽（跨屏时指向 `tiles` 的第一块，仅作占位）。
    pub slot: Arc<SharedSlot>,
    /// 跨屏时每块显示器一项；`None` 表示 `slot` 覆盖整个选区（单屏）。
    pub tiles: Option<Arc<[Tile]>>,
}

impl GpuFrame {
    /// 单屏帧：一个槽覆盖整个选区。
    ///
    /// # 参数
    /// - `slot`：覆盖整个选区的共享槽。
    pub fn single(slot: Arc<SharedSlot>) -> Self {
        Self { slot, tiles: None }
    }
}

/// 采集使用的设备对：A 给采集，B 给合成与编码。
pub struct DevicePair {
    /// 采集设备。
    pub capture: SharedDevice,
    /// 合成与编码设备。
    pub compose: SharedDevice,
}

/// 创建一个共享纹理槽（A 侧创建，B 侧打开）。
pub(crate) fn create_slot(a: &SharedDevice, b: &SharedDevice, size: (u32, u32)) -> Result<SharedSlot, String> {
    let desc = D3D11_TEXTURE2D_DESC {
        Width: size.0,
        Height: size.1,
        MipLevels: 1,
        ArraySize: 1,
        Format: DXGI_FORMAT_B8G8R8A8_UNORM,
        SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
        Usage: D3D11_USAGE_DEFAULT,
        BindFlags: (D3D11_BIND_SHADER_RESOURCE | D3D11_BIND_RENDER_TARGET).0 as u32,
        MiscFlags: (D3D11_RESOURCE_MISC_SHARED | D3D11_RESOURCE_MISC_SHARED_NTHANDLE).0 as u32,
        ..Default::default()
    };
    let mut tex_a = None;
    // SAFETY: desc 有效，输出指针指向局部变量。
    unsafe { a.device().CreateTexture2D(&desc, None, Some(&mut tex_a)) }.map_err(|e| format!("创建共享纹理失败: {e}"))?;
    let tex_a = tex_a.ok_or("共享纹理为空")?;
    let resource: IDXGIResource1 = tex_a.cast().map_err(|e| e.to_string())?;
    // SAFETY: 资源带 SHARED_NTHANDLE 标志创建，句柄打开后立即关闭（两个纹理各自持有底层资源）。
    let tex_b = unsafe {
        let handle = resource
            .CreateSharedHandle(None, DXGI_SHARED_RESOURCE_READ.0 | DXGI_SHARED_RESOURCE_WRITE.0, PCWSTR::null())
            .map_err(|e| format!("创建共享句柄失败: {e}"))?;
        let device1: ID3D11Device1 = b.device().cast().map_err(|e| e.to_string())?;
        let opened = device1.OpenSharedResource1::<ID3D11Texture2D>(handle);
        let _ = CloseHandle(handle);
        opened.map_err(|e| format!("在合成设备上打开共享纹理失败: {e}"))?
    };
    let device5_a: ID3D11Device5 = a.device().cast().map_err(|e| format!("采集设备不支持栅栏: {e}"))?;
    let device5_b: ID3D11Device5 = b.device().cast().map_err(|e| format!("合成设备不支持栅栏: {e}"))?;
    // SAFETY: 栅栏带 SHARED 标志创建，共享句柄打开后立即关闭。
    let (fence_a, fence_b) = unsafe {
        let mut created: Option<ID3D11Fence> = None;
        device5_a.CreateFence(0, D3D11_FENCE_FLAG_SHARED, &mut created).map_err(|e| format!("创建栅栏失败: {e}"))?;
        let fence_a = created.ok_or("栅栏为空")?;
        let handle: HANDLE = fence_a.CreateSharedHandle(None, FENCE_ACCESS, PCWSTR::null()).map_err(|e| format!("创建栅栏句柄失败: {e}"))?;
        let mut opened: Option<ID3D11Fence> = None;
        let result = device5_b.OpenSharedFence(handle, &mut opened);
        let _ = CloseHandle(handle);
        result.map_err(|e| format!("在合成设备上打开栅栏失败: {e}"))?;
        (fence_a, opened.ok_or("合成设备栅栏为空")?)
    };
    Ok(SharedSlot { tex_a, tex_b, fence_a, fence_b, value: AtomicU64::new(0) })
}

/// GPU 线程优先级（-7..=7）：合成/编码设备调到最低，让 DWM 合成与桌面复制优先拿到 GPU 时间，
/// 否则 VideoProcessor/编码占着引擎时，`AcquireNextFrame` 会被拖长十几毫秒而漏掉呈现。
pub(crate) const COMPOSE_GPU_PRIORITY: i32 = -7;

/// 环境变量：采集设备的 GPU 线程优先级（-7..=7，实验；缺省不设置）。
pub const ENV_CAPTURE_GPU_PRIORITY: &str = "SNOW_RECORDER_CAPTURE_GPU_PRIORITY";
/// 环境变量：合成设备的 GPU 线程优先级（-7..=7，缺省 -7；设 0 等于不降级）。
pub const ENV_COMPOSE_GPU_PRIORITY: &str = "SNOW_RECORDER_COMPOSE_GPU_PRIORITY";
/// 环境变量：采集模式（实验）：`wgc` = 用 Windows Graphics Capture 取帧。
pub const ENV_CAPTURE_MODE: &str = "SNOW_RECORDER_CAPTURE_MODE";

/// 读取环境变量里的 GPU 优先级（限制在 -7..=7）；未设置或非法返回 `None`。
pub(crate) fn env_priority(name: &str) -> Option<i32> {
    std::env::var(name).ok().and_then(|v| v.trim().parse::<i32>().ok()).map(|p| p.clamp(-7, 7))
}

/// 设置设备的 GPU 线程优先级；失败忽略（只是少一层保护）。
pub(crate) fn set_gpu_priority(device: &SharedDevice, priority: i32) {
    if let Ok(dxgi) = device.device().cast::<windows::Win32::Graphics::Dxgi::IDXGIDevice>() {
        // SAFETY: 只设置设备的调度优先级。
        let _ = unsafe { dxgi.SetGPUThreadPriority(priority) };
    }
}

/// 是否用 WGC 取帧（实验对照）。
pub fn wgc_mode() -> bool {
    std::env::var(ENV_CAPTURE_MODE).as_deref() == Ok("wgc")
}

/// 选区是否完整落在显示器范围内。
fn contains(monitor: (i32, i32, i32, i32), region: (i32, i32, u32, u32)) -> bool {
    let (l, t, r, b) = monitor;
    let (x, y, w, h) = region;
    i64::from(x) >= i64::from(l) && i64::from(y) >= i64::from(t) && i64::from(x) + i64::from(w) <= i64::from(r) && i64::from(y) + i64::from(h) <= i64::from(b)
}

/// 离开作用域时释放已取得的桌面帧。
pub(crate) struct FrameGuard<'a>(pub(crate) &'a IDXGIOutputDuplication);

impl Drop for FrameGuard<'_> {
    /// 释放帧。
    fn drop(&mut self) {
        // SAFETY: 只在 AcquireNextFrame 成功后创建，成对释放。
        let _ = unsafe { self.0.ReleaseFrame() };
    }
}

/// 桌面复制采集器。
pub struct DdaCapture {
    /// 采集设备 A。
    device: SharedDevice,
    /// 设备 A 的带栅栏上下文。
    context4: ID3D11DeviceContext4,
    /// 所在显示器输出（重建复制用）。
    output: IDXGIOutput1,
    /// 桌面复制接口。
    duplication: IDXGIOutputDuplication,
    /// 共享纹理池。
    slots: Vec<Arc<SharedSlot>>,
    /// 选区（虚拟桌面坐标）。
    region: (i32, i32, u32, u32),
    /// 选区在显示器内的裁剪框。
    crop: D3D11_BOX,
    /// 光标采样器。
    sampler: CursorSampler,
    /// 光标投影器。
    projector: CursorProjector,
    /// 最近见过的光标形状（`Cached` 状态补全用）。
    retained_shape: Option<CursorShape>,
    /// 最近一张带桌面内容的帧（光标单独移动时复用）。
    latest: Option<Captured<GpuFrame>>,
    /// QPC 锚点（换算呈现时间）。
    anchor: Option<QpcAnchor>,
    /// 计数。
    stats: CaptureStats,
    /// 诊断：相邻两次 `AcquireNextFrame` 调用的间隔（毫秒）。
    acquire_gap_ms: Vec<f32>,
    /// 诊断：每帧复制耗时（含等设备锁，毫秒）。
    copy_ms: Vec<f32>,
    /// 诊断：单次 `AcquireNextFrame` 调用本身的耗时（只记录超过 1ms 的）。
    slow_acquire_ms: Vec<f32>,
    /// 上一次调用 `AcquireNextFrame` 的时刻。
    last_acquire: Option<Instant>,
    /// WGC 取帧源（实验对照）：有值时不用 `AcquireNextFrame`。
    wgc: Option<crate::win::wgc::WgcSource>,
    /// 帧追踪缓冲（未开启时是空操作）。
    trace: TraceBuf,
}

impl DdaCapture {
    /// 改用 Windows Graphics Capture 取帧（实验对照）；不可用返回原因。
    pub fn enable_wgc(&mut self) -> Result<(), String> {
        // SAFETY: 只读取输出描述。
        let monitor = unsafe { self.output.GetDesc() }.map_err(|e| e.to_string())?.Monitor;
        let dxgi: windows::Win32::Graphics::Dxgi::IDXGIDevice = self.device.device().cast().map_err(|e| e.to_string())?;
        self.wgc = Some(crate::win::wgc::WgcSource::start(&dxgi, monitor)?);
        Ok(())
    }

    /// WGC 路径的取帧：信箱里的最新整屏纹理复制进共享槽（与桌面复制同一条下游路径）。
    fn next_wgc(&mut self, timeout: Duration, want: bool) -> Result<Option<Captured<GpuFrame>>, CaptureFault> {
        let Some(wgc) = &self.wgc else { return Ok(None) };
        let Some((source, frame, arrived)) = wgc.take(timeout) else { return Ok(None) };
        if !want {
            return Ok(None);
        }
        let Some(slot) = self.slots.iter().find(|s| Arc::strong_count(s) == 1).cloned() else {
            self.stats.pool_drops += 1;
            return Ok(None);
        };
        let copy_started = Instant::now();
        self.copy_into(&slot, &source, false)?;
        if self.copy_ms.len() < DIAG_LIMIT {
            self.copy_ms.push(copy_started.elapsed().as_secs_f32() * 1000.0);
        }
        self.flush(false);
        // 复制命令已提交，帧可以归还帧池（GPU 侧顺序由驱动保证）
        drop(frame);
        let cursor = self.sample_cursor();
        let captured = Captured { frame: GpuFrame::single(slot), cursor, present: arrived, captured_at: Instant::now(), fresh: true, id: self.stats.frames + 1 };
        self.stats.frames += 1;
        self.latest = Some(captured.clone());
        Ok(Some(captured))
    }

    /// 打开采集：找到完整包含选区的显示器，创建设备对、共享纹理池与桌面复制。
    ///
    /// 显示器接在哪块 GPU 上就用哪块（桌面复制只能在输出所属适配器上做）；同一选区出现在多块适配器时，
    /// 按编码器偏好排序后依次尝试，第一个成功的生效。
    ///
    /// # 参数
    /// - `region`：选区 `(x, y, 宽, 高)`（虚拟桌面坐标）。
    /// - `anchor`：QPC 与 `Instant` 的对应，用于换算呈现时间。
    /// - `preference`：编码器偏好（只用于多适配器时的尝试顺序）。
    ///
    /// # 返回
    /// 采集器与合成用设备 B；选区跨显示器/显示器旋转/设备不支持栅栏与共享等返回原因。
    pub fn open(region: (i32, i32, u32, u32), anchor: Option<QpcAnchor>, preference: EncoderPreference) -> Result<(Self, SharedDevice), String> {
        let mut candidates: Vec<(IDXGIAdapter1, IDXGIOutput, (i32, i32))> = Vec::new();
        let mut infos: Vec<AdapterInfo> = Vec::new();
        let mut errors: Vec<String> = Vec::new();
        for (adapter, info) in list_adapters()? {
            let mut o = 0;
            // SAFETY: 只调用 DXGI 枚举接口，接口对象由 windows crate 管理生命周期。
            while let Ok(output) = unsafe { adapter.EnumOutputs(o) } {
                o += 1;
                // SAFETY: 同上。
                let Ok(desc) = (unsafe { output.GetDesc() }) else { continue };
                let rect = desc.DesktopCoordinates;
                if !contains((rect.left, rect.top, rect.right, rect.bottom), region) {
                    continue;
                }
                if desc.Rotation.0 > 1 {
                    errors.push("显示器被旋转，自建 GPU 采集不支持".into());
                    continue;
                }
                candidates.push((adapter.clone(), output, (rect.left, rect.top)));
                infos.push(info.clone());
            }
        }
        for index in rank_adapters(&infos, preference) {
            let (adapter, output, origin) = &candidates[index];
            match Self::open_on(adapter, output, region, *origin, anchor) {
                Ok(opened) => return Ok(opened),
                Err(e) => errors.push(format!("{}: {e}", infos[index].description)),
            }
        }
        Err(if errors.is_empty() { "选区不在单个显示器内（跨显示器选区走软件路径）".into() } else { errors.join("; ") })
    }

    /// 在指定适配器的输出上创建设备对并建立采集。
    fn open_on(
        adapter: &IDXGIAdapter1,
        output: &IDXGIOutput,
        region: (i32, i32, u32, u32),
        origin: (i32, i32),
        anchor: Option<QpcAnchor>,
    ) -> Result<(Self, SharedDevice), String> {
        let base: IDXGIAdapter = adapter.cast().map_err(|e| e.to_string())?;
        let compose = SharedDevice::create(&base).map_err(|e| format!("创建合成设备失败: {e:#}"))?;
        let pair = DevicePair {
            capture: SharedDevice::create(&base).map_err(|e| format!("创建采集设备失败: {e:#}"))?,
            compose,
        };
        let output1: IDXGIOutput1 = output.cast().map_err(|e| format!("显示器不支持桌面复制: {e}"))?;
        set_gpu_priority(&pair.compose, env_priority(ENV_COMPOSE_GPU_PRIORITY).unwrap_or(COMPOSE_GPU_PRIORITY));
        if let Some(priority) = env_priority(ENV_CAPTURE_GPU_PRIORITY) {
            set_gpu_priority(&pair.capture, priority);
        }
        let capture = Self::build(pair.capture, &pair.compose, output1, region, origin, anchor)?;
        Ok((capture, pair.compose))
    }

    /// 建立纹理池与桌面复制。
    fn build(
        device: SharedDevice,
        compose: &SharedDevice,
        output: IDXGIOutput1,
        region: (i32, i32, u32, u32),
        origin: (i32, i32),
        anchor: Option<QpcAnchor>,
    ) -> Result<Self, String> {
        let slots = (0..POOL_SIZE).map(|_| create_slot(&device, compose, (region.2, region.3)).map(Arc::new)).collect::<Result<Vec<_>, _>>()?;
        // SAFETY: 设备由 SharedDevice 持有，复制接口随采集器存续。
        let duplication = unsafe { output.DuplicateOutput(device.device()) }.map_err(|e| format!("DuplicateOutput 失败: {e}"))?;
        let context4: ID3D11DeviceContext4 = device.context().cast().map_err(|e| format!("上下文不支持栅栏: {e}"))?;
        let left = u32::try_from(region.0 - origin.0).map_err(|e| e.to_string())?;
        let top = u32::try_from(region.1 - origin.1).map_err(|e| e.to_string())?;
        Ok(Self {
            device,
            context4,
            output,
            duplication,
            slots,
            region,
            crop: D3D11_BOX { left, top, front: 0, right: left + region.2, bottom: top + region.3, back: 1 },
            sampler: CursorSampler::new().map_err(|e| format!("光标采样器不可用: {e}"))?,
            projector: CursorProjector::new(),
            retained_shape: None,
            latest: None,
            anchor,
            stats: CaptureStats::default(),
            acquire_gap_ms: Vec::new(),
            copy_ms: Vec::new(),
            slow_acquire_ms: Vec::new(),
            last_acquire: None,
            wgc: None,
            trace: TraceBuf::new(Thread::Capture),
        })
    }

    /// 采样并投影当前光标（失败返回 `None`）。
    fn sample_cursor(&mut self) -> Option<AttachedCursorSample> {
        let snapshot = self.sampler.sample().ok()?;
        if let Some(shape) = snapshot.shape.shape() {
            self.retained_shape = Some(shape.clone());
        }
        let target = CursorTargetInfo { origin_x: self.region.0, origin_y: self.region.1, width: self.region.2, height: self.region.3 };
        let mut cursor = self.projector.project(&target, snapshot);
        if let Some(shape) = self.retained_shape.as_ref().filter(|s| cursor.shape.shape_id() == Some(s.shape_id)) {
            cursor.shape = CursorShapeState::Embedded(shape.clone());
        }
        Some(cursor)
    }

    /// DXGI 呈现时刻换算成 `Instant`（只在追踪开启时用，口径与取帧路径一致）。
    fn present_instant(&self, info: &DXGI_OUTDUPL_FRAME_INFO, captured_at: Instant) -> Instant {
        match self.anchor {
            Some(a) => a.to_instant(info.LastPresentTime).min(captured_at),
            None => captured_at,
        }
    }

    /// 把设备 A 上已记录的命令提交给 GPU；`timed` 为真时返回等设备锁的耗时，否则不读时钟、返回零。
    fn flush(&self, timed: bool) -> Duration {
        let lock_started = timed.then(Instant::now);
        let _lock = self.device.lock();
        let waited = lock_started.map_or(Duration::ZERO, |s| s.elapsed());
        // SAFETY: 持有设备锁；Flush 只提交已记录的命令。
        unsafe { self.device.context().Flush() };
        waited
    }

    /// 在设备 A 上把桌面选区复制进共享槽；`timed` 含义同 [`copy_into_slot`]。
    fn copy_into(&self, slot: &SharedSlot, source: &ID3D11Texture2D, timed: bool) -> Result<Duration, CaptureFault> {
        copy_into_slot(&self.device, &self.context4, slot, source, &self.crop, timed)
    }
}

/// 在设备 A 上把 `source` 的 `crop` 区域复制进共享槽（栅栏排序：先等 B 读完上次内容，复制后 Signal；调用方负责随后 Flush）。
///
/// # 参数
/// - `device`：采集设备 A。
/// - `context4`：设备 A 的带栅栏上下文。
/// - `slot`：目标槽（调用时不能有其他持有者）。
/// - `source`：源纹理（设备 A 上）。
/// - `crop`：源里要复制的区域。
/// - `timed`：为真时统计等设备锁的耗时（慢迭代追踪用），否则不读时钟。
///
/// # 返回
/// 等设备锁的耗时（`timed` 为假时为零）。
pub(crate) fn copy_into_slot(
    device: &SharedDevice,
    context4: &ID3D11DeviceContext4,
    slot: &SharedSlot,
    source: &ID3D11Texture2D,
    crop: &D3D11_BOX,
    timed: bool,
) -> Result<Duration, CaptureFault> {
    let lock_started = timed.then(Instant::now);
    let _lock = device.lock();
    let waited = lock_started.map_or(Duration::ZERO, |s| s.elapsed());
    let last = slot.value();
    let fail = |what: &str, e: windows::core::Error| CaptureFault::Other(format!("{what}: {e}"));
    // SAFETY: 持有设备锁；纹理与栅栏都属于设备 A（或其共享视图），槽此刻没有其他持有者。
    unsafe {
        if last > 0 {
            context4.Wait(&slot.fence_a, last).map_err(|e| fail("栅栏等待失败", e))?;
        }
        context4.CopySubresourceRegion(&slot.tex_a, 0, 0, 0, 0, source, 0, Some(crop));
        context4.Signal(&slot.fence_a, last + 1).map_err(|e| fail("栅栏 Signal 失败", e))?;
    }
    slot.set_value(last + 1);
    Ok(waited)
}

/// 枚举所有 DXGI 适配器及其描述（含软件适配器，由选择逻辑按厂商号过滤）。
pub(crate) fn list_adapters() -> Result<Vec<(IDXGIAdapter1, AdapterInfo)>, String> {
    // SAFETY: 只调用 DXGI 枚举接口。
    let factory: IDXGIFactory1 = unsafe { CreateDXGIFactory1() }.map_err(|e| format!("创建 DXGI 工厂失败: {e}"))?;
    let mut out = Vec::new();
    let mut index = 0;
    // SAFETY: 同上。
    while let Ok(adapter) = unsafe { factory.EnumAdapters1(index) } {
        index += 1;
        // SAFETY: 同上。
        let Ok(desc) = (unsafe { adapter.GetDesc1() }) else { continue };
        let end = desc.Description.iter().position(|c| *c == 0).unwrap_or(desc.Description.len());
        out.push((adapter, AdapterInfo { vendor: desc.VendorId, description: String::from_utf16_lossy(&desc.Description[..end]) }));
    }
    Ok(out)
}

/// 在同一适配器上创建（采集设备 A，合成设备 B）——仅测试用（真实路径在 `DdaCapture::open` 里按显示器所属适配器创建）。
///
/// # 参数
/// - `preference`：编码器偏好；按它排出最合适的适配器（默认 Auto 时取第一个有硬编的适配器）。
///
/// # 返回
/// 两个设备；没有适配器或创建失败返回原因。
#[cfg(test)]
pub(crate) fn create_device_pair(preference: EncoderPreference) -> Result<(SharedDevice, SharedDevice), String> {
    let adapters = list_adapters()?;
    let infos: Vec<AdapterInfo> = adapters.iter().map(|(_, info)| info.clone()).collect();
    let index = rank_adapters(&infos, preference).into_iter().next().ok_or("没有 DXGI 适配器")?;
    let base: IDXGIAdapter = adapters[index].0.cast().map_err(|e| e.to_string())?;
    let a = SharedDevice::create(&base).map_err(|e| format!("{e:#}"))?;
    let b = SharedDevice::create(&base).map_err(|e| format!("{e:#}"))?;
    Ok((a, b))
}

impl DdaCapture {
    /// 取帧轮询主体（`next` 的实现）：每圈开始/睡眠前由慢迭代追踪计时，返回前的收尾由 `next` 负责。
    fn next_poll(&mut self, timeout: Duration, want: bool) -> Result<Option<Captured<GpuFrame>>, CaptureFault> {
        if self.wgc.is_some() {
            return self.next_wgc(timeout, want);
        }
        let mut info = DXGI_OUTDUPL_FRAME_INFO::default();
        let mut resource: Option<IDXGIResource> = None;
        let deadline = Instant::now() + timeout;
        loop {
            let now = Instant::now();
            self.trace.iter_begin(now, 0);
            if let Some(prev) = self.last_acquire.replace(now)
                && self.acquire_gap_ms.len() < DIAG_LIMIT
            {
                self.acquire_gap_ms.push(now.saturating_duration_since(prev).as_secs_f32() * 1000.0);
            }
            // SAFETY: 输出指针指向局部变量。
            let call = unsafe { self.duplication.AcquireNextFrame(0, &mut info, &mut resource) };
            let took_dur = now.elapsed();
            self.trace.iter_add(IterStage::Acquire, took_dur);
            let took = took_dur.as_secs_f32() * 1000.0;
            if took > 1.0 && self.slow_acquire_ms.len() < DIAG_LIMIT {
                self.slow_acquire_ms.push(took);
            }
            match call {
                Ok(()) => break,
                Err(e) if e.code() == DXGI_ERROR_WAIT_TIMEOUT => {
                    if Instant::now() >= deadline {
                        self.trace.idle(0, timeout);
                        return Ok(None);
                    }
                    self.trace.iter_sleep(POLL_SLEEP);
                    std::thread::sleep(POLL_SLEEP);
                }
                Err(e) if e.code() == DXGI_ERROR_ACCESS_LOST => {
                    self.trace.lost(0);
                    return Err(CaptureFault::Lost);
                }
                Err(e) => return Err(CaptureFault::Other(format!("AcquireNextFrame 失败: {e}"))),
            }
        }
        let captured_at = Instant::now();
        let duplication = self.duplication.clone();
        let guard = FrameGuard(&duplication);
        if !want {
            self.trace.acquire(0, captured_at, None, info.AccumulatedFrames, 0, code::ACQ_UNWANTED);
            return Ok(None);
        }
        if info.LastPresentTime == 0 {
            drop(guard);
            self.trace.acquire(0, captured_at, None, info.AccumulatedFrames, 0, code::ACQ_CURSOR_ONLY);
            let cursor = self.sample_cursor();
            // 只有光标移动：沿用上一张桌面图
            return Ok(self.latest.as_ref().map(|f| Captured { cursor, captured_at, fresh: false, ..f.clone() }));
        }
        self.stats.coalesced += u64::from(info.AccumulatedFrames.saturating_sub(1));
        let Some(resource) = resource else { return Ok(None) };
        let source: ID3D11Texture2D = resource.cast().map_err(|e| CaptureFault::Other(format!("桌面资源不是纹理: {e}")))?;
        let mut desc = D3D11_TEXTURE2D_DESC::default();
        // SAFETY: desc 是局部变量。
        unsafe { source.GetDesc(&mut desc) };
        if desc.Format != DXGI_FORMAT_B8G8R8A8_UNORM {
            return Err(CaptureFault::Other(format!("桌面格式 {:?} 不是 BGRA8（HDR 走软件路径）", desc.Format)));
        }
        let copy_started = Instant::now();
        let Some(slot) = self.slots.iter().find(|s| Arc::strong_count(s) == 1).cloned() else {
            self.stats.pool_drops += 1;
            if self.trace.enabled() {
                let present = self.present_instant(&info, captured_at);
                self.trace.acquire(0, captured_at, Some(present), info.AccumulatedFrames, 0, code::ACQ_POOL_DROP);
            }
            return Ok(None);
        };
        let traced = self.trace.enabled();
        let lock_wait = self.copy_into(&slot, &source, traced)?;
        if self.copy_ms.len() < DIAG_LIMIT {
            self.copy_ms.push(copy_started.elapsed().as_secs_f32() * 1000.0);
        }
        if traced {
            self.trace.iter_add(IterStage::Copy, copy_started.elapsed());
            self.trace.iter_add(IterStage::Lock, lock_wait);
        }
        // 复制一记录完就立刻释放桌面帧（持有越久，下一次呈现越容易被 DXGI 合并），光标采样与 Flush 都放到释放之后
        drop(guard);
        let flush_started = self.trace.mark();
        let flush_wait = self.flush(traced);
        self.trace.iter_since(IterStage::Copy, flush_started);
        self.trace.iter_add(IterStage::Lock, flush_wait);
        let cursor = self.sample_cursor();
        let present = match self.anchor {
            Some(a) => a.to_instant(info.LastPresentTime),
            None => captured_at,
        };
        let captured = Captured { frame: GpuFrame::single(slot), cursor, present: present.min(captured_at), captured_at, fresh: true, id: self.stats.frames + 1 };
        self.trace.acquire(0, captured_at, Some(captured.present), info.AccumulatedFrames, captured.id, code::ACQ_FRESH);
        self.stats.frames += 1;
        self.latest = Some(captured.clone());
        Ok(Some(captured))
    }
}

impl CaptureSource for DdaCapture {
    type Frame = GpuFrame;

    /// 零超时取帧 + 亚毫秒睡眠，直到有新桌面内容/光标移动或超时。
    fn next(&mut self, timeout: Duration, want: bool) -> Result<Option<Captured<GpuFrame>>, CaptureFault> {
        let result = self.next_poll(timeout, want);
        self.trace.iter_finish();
        result
    }

    /// 重建桌面复制（权限丢失后）。
    fn recreate(&mut self) -> Result<(), String> {
        // SAFETY: 设备与输出在采集器存续期间有效。
        self.duplication = unsafe { self.output.DuplicateOutput(self.device.device()) }.map_err(|e| format!("重建桌面复制失败: {e}"))?;
        self.latest = None;
        Ok(())
    }

    /// 当前计数。
    fn stats(&self) -> CaptureStats {
        self.stats
    }

    /// 取走诊断样本。
    fn take_diag(&mut self) -> CaptureDiag {
        CaptureDiag {
            samples: vec![
                ("采集取帧间隔ms", std::mem::take(&mut self.acquire_gap_ms)),
                ("采集复制ms", std::mem::take(&mut self.copy_ms)),
                ("采集慢取帧ms(>1ms的调用耗时)", std::mem::take(&mut self.slow_acquire_ms)),
            ],
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::geom::Rect;
    use crate::win::vp::{VideoBlitter, VpLayer};
    use windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT_NV12;

    /// 选区必须完整落在显示器内。
    #[test]
    fn contains_requires_full_containment() {
        let monitor = (2560, 0, 5120, 1440);
        assert!(contains(monitor, (2560, 0, 2560, 1440)));
        assert!(contains(monitor, (3000, 100, 1920, 1080)));
        assert!(!contains(monitor, (2559, 0, 100, 100)));
        assert!(!contains(monitor, (4000, 0, 2000, 100)));
        assert!(!contains(monitor, (0, 0, 10, 10)));
    }

    /// GPU 耗时（毫秒/次）：`iterations` 次 `work` 夹在时间戳查询里，读回 disjoint 频率换算。
    fn gpu_ms(device: &SharedDevice, iterations: u32, mut work: impl FnMut()) -> Option<f64> {
        let dev = device.device();
        let ctx = device.context();
        let make = |kind| -> Option<ID3D11Query> {
            let mut q = None;
            // SAFETY: 描述符是局部值。
            unsafe { dev.CreateQuery(&D3D11_QUERY_DESC { Query: kind, MiscFlags: 0 }, Some(&mut q)) }.ok()?;
            q
        };
        let (disjoint, start, end) = (make(D3D11_QUERY_TIMESTAMP_DISJOINT)?, make(D3D11_QUERY_TIMESTAMP)?, make(D3D11_QUERY_TIMESTAMP)?);
        // SAFETY: 查询都属于该设备；调用线程独占此设备。
        unsafe {
            ctx.Begin(&disjoint);
            ctx.End(&start);
            for _ in 0..iterations {
                work();
            }
            ctx.End(&end);
            ctx.End(&disjoint);
            ctx.Flush();
            // GetData 的 S_FALSE（未就绪）也映射成 Ok，所以用"输出被写成非零"判断就绪（输出事先清零）
            let read = |q: &ID3D11Query, out: *mut core::ffi::c_void, size: usize, ready: &dyn Fn() -> bool| {
                for _ in 0..2000 {
                    if ctx.GetData(q, Some(out), size as u32, 0).is_ok() && ready() {
                        return true;
                    }
                    std::thread::sleep(Duration::from_millis(1));
                }
                false
            };
            let mut info = D3D11_QUERY_DATA_TIMESTAMP_DISJOINT::default();
            let (mut t0, mut t1) = (0u64, 0u64);
            let info_ptr: *mut D3D11_QUERY_DATA_TIMESTAMP_DISJOINT = &mut info;
            let (t0_ptr, t1_ptr): (*mut u64, *mut u64) = (&mut t0, &mut t1);
            // SAFETY: 三个指针指向上面的局部变量，闭包只在本函数内读取。
            let ok = read(&disjoint, info_ptr.cast(), size_of::<D3D11_QUERY_DATA_TIMESTAMP_DISJOINT>(), &|| (*info_ptr).Frequency != 0)
                && read(&start, t0_ptr.cast(), 8, &|| *t0_ptr != 0)
                && read(&end, t1_ptr.cast(), 8, &|| *t1_ptr != 0);
            if !ok || info.Disjoint.as_bool() {
                return None;
            }
            Some((t1 - t0) as f64 / info.Frequency as f64 * 1000.0 / f64::from(iterations))
        }
    }

    /// 方案 Z 探针（真机，无环境时跳过）：
    /// 1. 设备 A 上创建可共享的 NV12 纹理，在设备 B 上打开，A 用 VideoProcessor 写入，栅栏同步后两侧回读内容必须一致；
    /// 2. 量出 Z 能省掉的那次 BGRA 整帧拷贝（1440p）的 GPU 耗时，以及 VideoProcessor 本身的耗时作对照。
    #[test]
    fn scheme_z_cross_device_nv12_share_probe() {
        let size = (2560u32, 1440u32);
        let Ok(factory) = (unsafe { CreateDXGIFactory1::<IDXGIFactory1>() }) else { return };
        let Ok(adapter) = (unsafe { factory.EnumAdapters1(0) }) else { return };
        let Ok(base) = adapter.cast::<IDXGIAdapter>() else { return };
        let (Ok(a), Ok(b)) = (SharedDevice::create(&base), SharedDevice::create(&base)) else { return };
        // 1. 共享 NV12 纹理
        let desc = D3D11_TEXTURE2D_DESC {
            Width: size.0,
            Height: size.1,
            MipLevels: 1,
            ArraySize: 1,
            Format: DXGI_FORMAT_NV12,
            SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
            Usage: D3D11_USAGE_DEFAULT,
            BindFlags: D3D11_BIND_RENDER_TARGET.0 as u32,
            MiscFlags: (D3D11_RESOURCE_MISC_SHARED | D3D11_RESOURCE_MISC_SHARED_NTHANDLE).0 as u32,
            ..Default::default()
        };
        let mut nv12_a = None;
        // SAFETY: desc 有效。
        if let Err(e) = unsafe { a.device().CreateTexture2D(&desc, None, Some(&mut nv12_a)) } {
            eprintln!("方案 Z 探针: 不可行——设备 A 无法创建可共享 NV12 纹理: {e}");
            return;
        }
        let nv12_a = nv12_a.unwrap();
        let resource: IDXGIResource1 = nv12_a.cast().unwrap();
        // SAFETY: 资源带 SHARED_NTHANDLE 创建；句柄打开后立即关闭。
        let nv12_b = unsafe {
            let handle = match resource.CreateSharedHandle(None, DXGI_SHARED_RESOURCE_READ.0 | DXGI_SHARED_RESOURCE_WRITE.0, PCWSTR::null()) {
                Ok(h) => h,
                Err(e) => {
                    eprintln!("方案 Z 探针: 不可行——NV12 纹理创建共享句柄失败: {e}");
                    return;
                }
            };
            let device1: ID3D11Device1 = b.device().cast().unwrap();
            let opened = device1.OpenSharedResource1::<ID3D11Texture2D>(handle);
            let _ = CloseHandle(handle);
            match opened {
                Ok(t) => t,
                Err(e) => {
                    eprintln!("方案 Z 探针: 不可行——设备 B 打开共享 NV12 失败: {e}");
                    return;
                }
            }
        };
        // 栅栏（复用 BGRA 槽的创建逻辑拿一对共享栅栏）
        let slot = create_slot(&a, &b, (64, 64)).expect("栅栏槽");
        // A 侧：噪声 BGRA 源 -> VideoProcessor -> 共享 NV12
        let binds = (D3D11_BIND_SHADER_RESOURCE | D3D11_BIND_RENDER_TARGET).0 as u32;
        let source = a.texture(size.0, size.1, DXGI_FORMAT_B8G8R8A8_UNORM, binds).expect("源纹理");
        let pixels: Vec<u8> = (0..size.0 * size.1 * 4).map(|i| (i.wrapping_mul(2654435761) >> 13) as u8).collect();
        {
            let _lock = a.lock();
            // SAFETY: 缓冲长度 = 宽*高*4。
            unsafe { a.context().UpdateSubresource(source.raw(), 0, None, pixels.as_ptr().cast(), size.0 * 4, 0) };
        }
        let mut blitter = VideoBlitter::new(a.device(), a.context(), size, size, 60).expect("视频处理器");
        let layer = VpLayer { texture: source.raw().clone(), source: Rect::full(size), destination: Rect::full(size), alpha: false };
        let ctx_a: ID3D11DeviceContext4 = a.context().cast().unwrap();
        let ctx_b: ID3D11DeviceContext4 = b.context().cast().unwrap();
        {
            let _lock = a.lock();
            blitter.blit(std::slice::from_ref(&layer), &nv12_a, 0).expect("VP 写共享 NV12");
            // SAFETY: 持有设备锁。
            unsafe {
                ctx_a.Signal(&slot.fence_a, 1).unwrap();
                a.context().Flush();
            }
        }
        // 两侧回读：A 直接读，B 等栅栏后读
        let readback = |dev: &SharedDevice, tex: &ID3D11Texture2D| -> Vec<u8> {
            let mut sdesc = desc;
            sdesc.Usage = D3D11_USAGE_STAGING;
            sdesc.BindFlags = 0;
            sdesc.MiscFlags = 0;
            sdesc.CPUAccessFlags = D3D11_CPU_ACCESS_READ.0 as u32;
            let mut staging = None;
            let _lock = dev.lock();
            // SAFETY: 描述符有效；Map 出的指针在 Unmap 前有效，读取长度 = 行距 * (高度 * 3/2)。
            unsafe {
                dev.device().CreateTexture2D(&sdesc, None, Some(&mut staging)).expect("staging");
                let staging = staging.unwrap();
                dev.context().CopyResource(&staging, tex);
                let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
                dev.context().Map(&staging, 0, D3D11_MAP_READ, 0, Some(&mut mapped)).expect("Map");
                let len = mapped.RowPitch as usize * (size.1 as usize * 3 / 2);
                let out = std::slice::from_raw_parts(mapped.pData.cast::<u8>(), len).to_vec();
                dev.context().Unmap(&staging, 0);
                out
            }
        };
        let from_a = readback(&a, &nv12_a);
        // SAFETY: B 侧等待 A 的 Signal。
        unsafe {
            let _lock = b.lock();
            ctx_b.Wait(&slot.fence_b, 1).unwrap();
        }
        let from_b = readback(&b, &nv12_b);
        assert!(from_a.iter().any(|&v| v != 0), "共享 NV12 内容全零，VP 没写进去");
        let identical = from_a == from_b;
        eprintln!("方案 Z 探针: 跨设备共享 NV12 可创建/可打开；A 写、B 栅栏后读 内容一致={identical}（{} 字节）", from_a.len());
        assert!(identical, "跨设备读到的 NV12 与 A 侧不一致");

        // 2. GPU 耗时对照
        let copy_dst = a.texture(size.0, size.1, DXGI_FORMAT_B8G8R8A8_UNORM, binds).expect("目的纹理");
        let copy_ms = gpu_ms(&a, 200, || {
            let _lock = a.lock();
            // SAFETY: 持有设备锁；同设备同尺寸整帧拷贝。
            unsafe { a.context().CopyResource(copy_dst.raw(), source.raw()) };
        });
        let vp_ms = gpu_ms(&a, 200, || {
            let _lock = a.lock();
            let _ = blitter.blit(std::slice::from_ref(&layer), &nv12_a, 0);
        });
        eprintln!("方案 Z 探针: 1440p 整帧 BGRA 拷贝（Z 能省掉的）GPU 耗时 {copy_ms:?} ms/帧；BGRA->NV12 VideoProcessor GPU 耗时 {vp_ms:?} ms/帧");
    }

    /// 真机：能建立设备对、共享纹理与栅栏，并在两个设备间走完一次 Wait/Signal（无显示器/驱动不支持时跳过）。
    #[test]
    fn shared_slot_roundtrip_across_devices_when_supported() {
        let Ok(factory) = (unsafe { CreateDXGIFactory1::<IDXGIFactory1>() }) else { return };
        let Ok(adapter) = (unsafe { factory.EnumAdapters1(0) }) else { return };
        let Ok(base) = adapter.cast::<IDXGIAdapter>() else { return };
        let (Ok(a), Ok(b)) = (SharedDevice::create(&base), SharedDevice::create(&base)) else { return };
        let slot = match create_slot(&a, &b, (64, 48)) {
            Ok(slot) => slot,
            Err(e) => {
                eprintln!("跳过（本机不支持设备间共享/栅栏）: {e}");
                return;
            }
        };
        let ctx_a: ID3D11DeviceContext4 = a.context().cast().unwrap();
        let ctx_b: ID3D11DeviceContext4 = b.context().cast().unwrap();
        // SAFETY: 两个设备各自只在本线程使用。
        unsafe {
            ctx_a.Signal(&slot.fence_a, 1).unwrap();
            a.context().Flush();
            ctx_b.Wait(&slot.fence_b, 1).unwrap();
            ctx_b.Signal(&slot.fence_b, 2).unwrap();
            b.context().Flush();
            ctx_a.Wait(&slot.fence_a, 2).unwrap();
            a.context().Flush();
        }
        let mut desc = D3D11_TEXTURE2D_DESC::default();
        // SAFETY: desc 是局部变量。
        unsafe { slot.texture_b().GetDesc(&mut desc) };
        assert_eq!((desc.Width, desc.Height), (64, 48));
        assert_ne!(slot.tex_a.as_raw(), slot.texture_b().as_raw());
    }
}
