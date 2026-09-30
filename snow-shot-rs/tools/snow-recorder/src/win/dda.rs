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
    DXGI_SHARED_RESOURCE_WRITE, IDXGIAdapter, IDXGIFactory1, IDXGIOutput1, IDXGIOutputDuplication, IDXGIResource, IDXGIResource1,
};
use windows::core::{Interface, PCWSTR};

use crate::pipeline::{CaptureDiag, CaptureFault, CaptureSource, CaptureStats, Captured};
use crate::timeline::QpcAnchor;

/// 共享纹理池容量（采集队列 + 待输出队列 + 最近帧 + 合成中都占用名额）。
pub const POOL_SIZE: usize = 10;
/// 零超时轮询之间的睡眠：阻塞式 `AcquireNextFrame` 会在等待期间占着设备锁，所以改成"零超时取帧 + 亚毫秒睡眠"。
pub const POLL_SLEEP: Duration = Duration::from_micros(400);
/// 诊断样本上限。
const DIAG_LIMIT: usize = 20_000;
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

/// 采集帧句柄：引用计数的共享槽；所有持有者释放后，采集才会复用该槽。
#[derive(Clone)]
pub struct GpuFrame {
    /// 共享槽。
    pub slot: Arc<SharedSlot>,
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

/// 选区是否完整落在显示器范围内。
fn contains(monitor: (i32, i32, i32, i32), region: (i32, i32, u32, u32)) -> bool {
    let (l, t, r, b) = monitor;
    let (x, y, w, h) = region;
    i64::from(x) >= i64::from(l) && i64::from(y) >= i64::from(t) && i64::from(x) + i64::from(w) <= i64::from(r) && i64::from(y) + i64::from(h) <= i64::from(b)
}

/// 离开作用域时释放已取得的桌面帧。
struct FrameGuard<'a>(&'a IDXGIOutputDuplication);

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
}

impl DdaCapture {
    /// 打开采集：找到完整包含选区的显示器，创建设备对、共享纹理池与桌面复制。
    ///
    /// # 参数
    /// - `region`：选区 `(x, y, 宽, 高)`（虚拟桌面坐标）。
    /// - `anchor`：QPC 与 `Instant` 的对应，用于换算呈现时间。
    ///
    /// # 返回
    /// 采集器与合成用设备 B；选区跨显示器/显示器旋转/设备不支持栅栏与共享等返回原因。
    pub fn open(region: (i32, i32, u32, u32), anchor: Option<QpcAnchor>) -> Result<(Self, SharedDevice), String> {
        // SAFETY: 只调用 DXGI 枚举接口，接口对象由 windows crate 管理生命周期。
        let factory: IDXGIFactory1 = unsafe { CreateDXGIFactory1() }.map_err(|e| format!("创建 DXGI 工厂失败: {e}"))?;
        let mut index = 0;
        // SAFETY: 同上。
        while let Ok(adapter) = unsafe { factory.EnumAdapters1(index) } {
            let mut o = 0;
            // SAFETY: 同上。
            while let Ok(output) = unsafe { adapter.EnumOutputs(o) } {
                o += 1;
                // SAFETY: 同上。
                let Ok(desc) = (unsafe { output.GetDesc() }) else { continue };
                let rect = desc.DesktopCoordinates;
                if !contains((rect.left, rect.top, rect.right, rect.bottom), region) {
                    continue;
                }
                if desc.Rotation.0 > 1 {
                    return Err("显示器被旋转，自建 GPU 采集不支持".into());
                }
                let base: IDXGIAdapter = adapter.cast().map_err(|e| e.to_string())?;
                let pair = DevicePair {
                    capture: SharedDevice::create(&base).map_err(|e| format!("创建采集设备失败: {e:#}"))?,
                    compose: SharedDevice::create(&base).map_err(|e| format!("创建合成设备失败: {e:#}"))?,
                };
                let output1: IDXGIOutput1 = output.cast().map_err(|e| format!("显示器不支持桌面复制: {e}"))?;
                let capture = Self::build(pair.capture, &pair.compose, output1, region, (rect.left, rect.top), anchor)?;
                return Ok((capture, pair.compose));
            }
            index += 1;
        }
        Err("选区不在单个显示器内（跨显示器选区走软件路径）".into())
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

    /// 在设备 A 上把桌面选区复制进共享槽。
    fn copy_into(&self, slot: &SharedSlot, source: &ID3D11Texture2D) -> Result<(), CaptureFault> {
        copy_into_slot(&self.device, &self.context4, slot, source, &self.crop)
    }
}

/// 在设备 A 上把 `source` 的 `crop` 区域复制进共享槽（栅栏排序：先等 B 读完上次内容，复制后 Signal 并 Flush）。
///
/// # 参数
/// - `device`：采集设备 A。
/// - `context4`：设备 A 的带栅栏上下文。
/// - `slot`：目标槽（调用时不能有其他持有者）。
/// - `source`：源纹理（设备 A 上）。
/// - `crop`：源里要复制的区域。
pub(crate) fn copy_into_slot(
    device: &SharedDevice,
    context4: &ID3D11DeviceContext4,
    slot: &SharedSlot,
    source: &ID3D11Texture2D,
    crop: &D3D11_BOX,
) -> Result<(), CaptureFault> {
    let _lock = device.lock();
    let last = slot.value();
    let fail = |what: &str, e: windows::core::Error| CaptureFault::Other(format!("{what}: {e}"));
    // SAFETY: 持有设备锁；纹理与栅栏都属于设备 A（或其共享视图），槽此刻没有其他持有者。
    unsafe {
        if last > 0 {
            context4.Wait(&slot.fence_a, last).map_err(|e| fail("栅栏等待失败", e))?;
        }
        context4.CopySubresourceRegion(&slot.tex_a, 0, 0, 0, 0, source, 0, Some(crop));
        context4.Signal(&slot.fence_a, last + 1).map_err(|e| fail("栅栏 Signal 失败", e))?;
        device.context().Flush();
    }
    slot.set_value(last + 1);
    Ok(())
}

/// 在同一适配器上创建（采集设备 A，合成设备 B）——仅测试用（真实路径在 `DdaCapture::open` 里按显示器所属适配器创建）。
#[cfg(test)]
///
/// # 返回
/// 两个设备；没有适配器或创建失败返回原因。
pub(crate) fn create_device_pair() -> Result<(SharedDevice, SharedDevice), String> {
    // SAFETY: 只调用 DXGI 枚举接口。
    let factory: IDXGIFactory1 = unsafe { CreateDXGIFactory1() }.map_err(|e| e.to_string())?;
    // SAFETY: 同上。
    let adapter = unsafe { factory.EnumAdapters1(0) }.map_err(|e| e.to_string())?;
    let base: IDXGIAdapter = adapter.cast().map_err(|e| e.to_string())?;
    let a = SharedDevice::create(&base).map_err(|e| format!("{e:#}"))?;
    let b = SharedDevice::create(&base).map_err(|e| format!("{e:#}"))?;
    Ok((a, b))
}

impl CaptureSource for DdaCapture {
    type Frame = GpuFrame;

    /// 零超时取帧 + 亚毫秒睡眠，直到有新桌面内容/光标移动或超时。
    fn next(&mut self, timeout: Duration, want: bool) -> Result<Option<Captured<GpuFrame>>, CaptureFault> {
        let mut info = DXGI_OUTDUPL_FRAME_INFO::default();
        let mut resource: Option<IDXGIResource> = None;
        let deadline = Instant::now() + timeout;
        loop {
            let now = Instant::now();
            if let Some(prev) = self.last_acquire.replace(now)
                && self.acquire_gap_ms.len() < DIAG_LIMIT
            {
                self.acquire_gap_ms.push(now.saturating_duration_since(prev).as_secs_f32() * 1000.0);
            }
            // SAFETY: 输出指针指向局部变量。
            let call = unsafe { self.duplication.AcquireNextFrame(0, &mut info, &mut resource) };
            let took = now.elapsed().as_secs_f32() * 1000.0;
            if took > 1.0 && self.slow_acquire_ms.len() < DIAG_LIMIT {
                self.slow_acquire_ms.push(took);
            }
            match call {
                Ok(()) => break,
                Err(e) if e.code() == DXGI_ERROR_WAIT_TIMEOUT => {
                    if Instant::now() >= deadline {
                        return Ok(None);
                    }
                    std::thread::sleep(POLL_SLEEP);
                }
                Err(e) if e.code() == DXGI_ERROR_ACCESS_LOST => return Err(CaptureFault::Lost),
                Err(e) => return Err(CaptureFault::Other(format!("AcquireNextFrame 失败: {e}"))),
            }
        }
        let captured_at = Instant::now();
        let duplication = self.duplication.clone();
        let guard = FrameGuard(&duplication);
        if !want {
            return Ok(None);
        }
        let cursor = self.sample_cursor();
        if info.LastPresentTime == 0 {
            drop(guard);
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
        let Some(slot) = self.slots.iter().find(|s| Arc::strong_count(s) == 1).cloned() else {
            self.stats.pool_drops += 1;
            return Ok(None);
        };
        let copy_started = Instant::now();
        self.copy_into(&slot, &source)?;
        if self.copy_ms.len() < DIAG_LIMIT {
            self.copy_ms.push(copy_started.elapsed().as_secs_f32() * 1000.0);
        }
        drop(guard);
        let present = match self.anchor {
            Some(a) => a.to_instant(info.LastPresentTime),
            None => captured_at,
        };
        let captured = Captured { frame: GpuFrame { slot }, cursor, present: present.min(captured_at), captured_at, fresh: true };
        self.stats.frames += 1;
        self.latest = Some(captured.clone());
        Ok(Some(captured))
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
