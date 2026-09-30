//! Windows Graphics Capture（WGC）取帧源（实验对照，`SNOW_RECORDER_CAPTURE_MODE=wgc` 启用，默认不用）。
//!
//! 只负责"拿到显示器整幅纹理的最新一帧"：自由线程帧池的 `FrameArrived` 回调把最新帧放进信箱，
//! 采集线程从信箱取走后仍走与桌面复制相同的"复制进共享槽"路径，其余流水线完全复用。
//! 光标由流水线自己采样叠加（WGC 这里关闭系统光标），因此光标单独移动不会产生新帧。

use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use windows::Foundation::TypedEventHandler;
use windows::Graphics::Capture::{Direct3D11CaptureFrame, Direct3D11CaptureFramePool, GraphicsCaptureItem, GraphicsCaptureSession};
use windows::Graphics::DirectX::Direct3D11::IDirect3DDevice;
use windows::Graphics::DirectX::DirectXPixelFormat;
use windows::Win32::Graphics::Direct3D11::ID3D11Texture2D;
use windows::Win32::Graphics::Dxgi::IDXGIDevice;
use windows::Win32::Graphics::Gdi::HMONITOR;
use windows::Win32::System::Com::{COINIT_MULTITHREADED, CoInitializeEx};
use windows::Win32::System::WinRT::Direct3D11::{CreateDirect3D11DeviceFromDXGIDevice, IDirect3DDxgiInterfaceAccess};
use windows::Win32::System::WinRT::Graphics::Capture::IGraphicsCaptureItemInterop;
use windows::core::Interface;

/// 帧池缓冲数（信箱里最多占 1 张，采集线程处理中占 1 张，再留 1 张给新帧）。
const POOL_BUFFERS: i32 = 3;

/// 信箱里的一帧：WGC 帧对象（释放即归还帧池）与到达时刻。
type Mail = (Direct3D11CaptureFrame, Instant);

/// 信箱：最新帧覆盖旧帧（旧帧随之归还帧池）。
#[derive(Default)]
struct Mailbox {
    /// 最新一帧。
    slot: Mutex<Option<Mail>>,
    /// 有新帧时唤醒采集线程。
    ready: Condvar,
}

/// WGC 取帧源。
pub struct WgcSource {
    /// 帧池（保持存活）。
    pool: Direct3D11CaptureFramePool,
    /// 捕获会话。
    session: GraphicsCaptureSession,
    /// 信箱。
    mailbox: Arc<Mailbox>,
    /// `FrameArrived` 的注册令牌。
    token: i64,
}

impl WgcSource {
    /// 开始捕获整块显示器。
    ///
    /// # 参数
    /// - `device`：采集设备的 DXGI 接口（WGC 在该设备上产出帧）。
    /// - `monitor`：显示器句柄。
    ///
    /// # 返回
    /// 取帧源；系统不支持 WGC 或创建失败返回原因。
    pub fn start(device: &IDXGIDevice, monitor: HMONITOR) -> Result<Self, String> {
        // SAFETY: 重复初始化无害；WinRT 对象创建需要线程已进入 COM（MTA）。
        let _ = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) };
        let fail = |what: &str, e: windows::core::Error| format!("{what}: {e}");
        let interop: IGraphicsCaptureItemInterop = windows::core::factory::<GraphicsCaptureItem, IGraphicsCaptureItemInterop>().map_err(|e| fail("WGC 不可用", e))?;
        // SAFETY: monitor 来自 DXGI 输出描述，有效。
        let item: GraphicsCaptureItem = unsafe { interop.CreateForMonitor(monitor) }.map_err(|e| fail("创建捕获项", e))?;
        // SAFETY: device 有效。
        let inspectable = unsafe { CreateDirect3D11DeviceFromDXGIDevice(device) }.map_err(|e| fail("包装 D3D 设备", e))?;
        let d3d: IDirect3DDevice = inspectable.cast().map_err(|e| fail("IDirect3DDevice", e))?;
        let size = item.Size().map_err(|e| fail("捕获项尺寸", e))?;
        let pool = Direct3D11CaptureFramePool::CreateFreeThreaded(&d3d, DirectXPixelFormat::B8G8R8A8UIntNormalized, POOL_BUFFERS, size).map_err(|e| fail("创建帧池", e))?;
        let mailbox = Arc::new(Mailbox::default());
        let shared = Arc::clone(&mailbox);
        let token = pool
            .FrameArrived(&TypedEventHandler::<Direct3D11CaptureFramePool, windows::core::IInspectable>::new(move |sender, _| {
                if let Some(pool) = sender.as_ref()
                    && let Ok(frame) = pool.TryGetNextFrame()
                    && let Ok(mut slot) = shared.slot.lock()
                {
                    *slot = Some((frame, Instant::now()));
                    shared.ready.notify_one();
                }
                Ok(())
            }))
            .map_err(|e| fail("注册 FrameArrived", e))?;
        let session = pool.CreateCaptureSession(&item).map_err(|e| fail("创建捕获会话", e))?;
        // 光标由流水线叠加；边框与光标开关在旧系统上可能不存在，失败忽略
        let _ = session.SetIsCursorCaptureEnabled(false);
        let _ = session.SetIsBorderRequired(false);
        session.StartCapture().map_err(|e| fail("开始捕获", e))?;
        Ok(Self { pool, session, mailbox, token })
    }

    /// 取走信箱里的最新帧，最多等 `timeout`。
    ///
    /// # 参数
    /// - `timeout`：最长等待。
    ///
    /// # 返回
    /// 帧的 D3D11 纹理、保持帧存活的句柄（处理完须释放以归还帧池）、到达时刻；超时返回 `None`。
    pub fn take(&self, timeout: Duration) -> Option<(ID3D11Texture2D, Direct3D11CaptureFrame, Instant)> {
        let mut slot = self.mailbox.slot.lock().ok()?;
        if slot.is_none() {
            slot = self.mailbox.ready.wait_timeout(slot, timeout).ok()?.0;
        }
        let (frame, arrived) = slot.take()?;
        drop(slot);
        let surface = frame.Surface().ok()?;
        let access: IDirect3DDxgiInterfaceAccess = surface.cast().ok()?;
        // SAFETY: 表面由 WGC 在采集设备上创建，必为 D3D11 纹理。
        let texture: ID3D11Texture2D = unsafe { access.GetInterface() }.ok()?;
        Some((texture, frame, arrived))
    }
}

impl Drop for WgcSource {
    /// 停止捕获并注销回调。
    fn drop(&mut self) {
        let _ = self.pool.RemoveFrameArrived(self.token);
        let _ = self.session.Close();
        let _ = self.pool.Close();
    }
}
