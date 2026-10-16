//! 基于 Media Foundation 媒体引擎（`IMFMediaEngine`，frame-server 无窗口模式）的静音预览播放。
//!
//! 取帧流程：`OnVideoStreamTick` 判新帧 → `TransferVideoFrame` 到 BGRA 纹理 → 拷到 STAGING → Map 读出。
//!
//! 线程与使用约束（实测）：
//! - 引擎、取帧都应放在同一个已初始化 COM（MTA）的专用线程里；回调只翻原子标志位，不做重活。
//! - frame-server 模式下必须持续轮询 [`Engine::try_frame`]，否则播放时钟不前进（暂停时可降频）。
//! - seek 只会取到“最新目标”的帧，中间目标的帧被丢弃，天然适合拖动。
//! - 含 B 帧的 mp4（如 x264 默认）里，引擎时间比画面超前约 4 帧（133ms）；本软件自己录制的
//!   视频没有 B 帧，偏移为 0（见 [`VIDEO_TIME_BIAS_MS`]）。

use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::time::{Duration, Instant};
use windows::Win32::Foundation::{E_FAIL, HMODULE, RECT};
use windows::Win32::Graphics::Direct3D::D3D_DRIVER_TYPE_HARDWARE;
use windows::Win32::Graphics::Direct3D10::ID3D10Multithread;
use windows::Win32::Graphics::Direct3D11::*;
use windows::Win32::Graphics::Dxgi::Common::{DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_SAMPLE_DESC};
use windows::Win32::Media::MediaFoundation::*;
use windows::Win32::System::Com::{CLSCTX_INPROC_SERVER, CoCreateInstance};
use windows::core::{BSTR, Interface, Result, implement};

/// 引擎时间相对画面的校正量（毫秒，画面时间 = 引擎时间 - 本值）。
///
/// 本软件录制的 mp4 为 I/P 帧、无 B 帧，实测与 ffmpeg 全量解码逐帧对齐，偏移为 0；
/// 外部含 B 帧文件约为 133，如需兼容把它调大即可。
pub const VIDEO_TIME_BIAS_MS: i64 = 0;
/// 等待元数据加载的最长时间。
const LOAD_TIMEOUT: Duration = Duration::from_secs(5);
/// 毫秒每秒。
const MS_PER_SEC: f64 = 1000.0;
/// BGRA 每像素字节数。
const BGRA_BYTES: usize = 4;

/// 回调线程与调用线程共享的状态（只用原子量，避免锁）。
#[derive(Default)]
struct Shared {
    /// 元数据已加载。
    loaded: AtomicBool,
    /// 播放到文件末尾。
    ended: AtomicBool,
    /// 最近一次错误码（0 = 无）。
    error: AtomicU32,
}

/// MF 事件回调对象。
#[implement(IMFMediaEngineNotify)]
struct Notify(Arc<Shared>);

impl IMFMediaEngineNotify_Impl for Notify_Impl {
    /// 回调：只翻标志位，不做任何重活。
    fn EventNotify(&self, event: u32, param1: usize, _param2: u32) -> Result<()> {
        let s = &self.0;
        match MF_MEDIA_ENGINE_EVENT(event as i32) {
            MF_MEDIA_ENGINE_EVENT_LOADEDMETADATA => s.loaded.store(true, Ordering::SeqCst),
            MF_MEDIA_ENGINE_EVENT_ENDED => s.ended.store(true, Ordering::SeqCst),
            MF_MEDIA_ENGINE_EVENT_ERROR => s.error.store(param1 as u32 + 1, Ordering::SeqCst),
            _ => {}
        }
        Ok(())
    }
}

/// 一帧 BGRA 画面。
pub struct VideoFrame {
    /// 宽（像素）。
    pub width: u32,
    /// 高（像素）。
    pub height: u32,
    /// BGRA 字节，紧密排列（stride = width * 4），自上而下。
    pub data: Vec<u8>,
}

/// 视频播放引擎（静音）。
pub struct Engine {
    /// 媒体引擎。
    engine: IMFMediaEngine,
    /// D3D 设备上下文。
    ctx: ID3D11DeviceContext,
    /// 取帧的渲染目标纹理。
    target: ID3D11Texture2D,
    /// CPU 可读的暂存纹理。
    staging: ID3D11Texture2D,
    /// 回调共享状态。
    shared: Arc<Shared>,
    /// 视频宽。
    w: u32,
    /// 视频高。
    h: u32,
    /// 上一帧的引擎时间戳（100ns），用于去重。
    last_pts: i64,
    /// 保活设备管理器。
    _mgr: IMFDXGIDeviceManager,
}

impl Engine {
    /// 打开本地视频并等到元数据就绪；调用线程须已初始化 COM（MTA）。
    ///
    /// # 参数
    /// - `path`：本地视频绝对路径。
    ///
    /// # 返回
    /// 引擎；失败返回带 HRESULT 的错误。丢弃引擎时会 `MFShutdown`。
    ///
    /// # 示例
    /// ```ignore
    /// let mut e = Engine::open(Path::new(r"C:\a.mp4"))?;
    /// e.set_muted(true)?;
    /// e.play()?;
    /// ```
    pub fn open(path: &Path) -> Result<Self> {
        // SAFETY: 全部是对 COM / D3D11 / MF 接口的调用，指针来自本函数内创建的对象。
        unsafe {
            MFStartup(MF_VERSION, MFSTARTUP_FULL)?;
            match Self::open_started(path) {
                Ok(e) => Ok(e),
                Err(e) => {
                    let _ = MFShutdown();
                    Err(e)
                }
            }
        }
    }

    /// `MFStartup` 之后的创建流程（失败由调用方补 `MFShutdown`）。
    ///
    /// # Safety
    /// 调用线程须已初始化 COM 并完成 `MFStartup`。
    unsafe fn open_started(path: &Path) -> Result<Self> {
        // SAFETY: 见函数级约束；所有out参数均指向局部变量。
        unsafe {
            let mut dev = None;
            let mut ctx = None;
            D3D11CreateDevice(
                None,
                D3D_DRIVER_TYPE_HARDWARE,
                HMODULE::default(),
                D3D11_CREATE_DEVICE_VIDEO_SUPPORT | D3D11_CREATE_DEVICE_BGRA_SUPPORT,
                None,
                D3D11_SDK_VERSION,
                Some(&mut dev),
                None,
                Some(&mut ctx),
            )?;
            let dev: ID3D11Device = dev.ok_or(E_FAIL)?;
            let ctx = ctx.ok_or(E_FAIL)?;
            // MF 会跨线程用设备，必须开多线程保护。
            let _ = dev
                .cast::<ID3D10Multithread>()?
                .SetMultithreadProtected(true);
            let mut token = 0u32;
            let mut mgr = None;
            MFCreateDXGIDeviceManager(&mut token, &mut mgr)?;
            let mgr: IMFDXGIDeviceManager = mgr.ok_or(E_FAIL)?;
            mgr.ResetDevice(&dev, token)?;

            let shared = Arc::new(Shared::default());
            let notify: IMFMediaEngineNotify = Notify(shared.clone()).into();
            let mut attrs = None;
            MFCreateAttributes(&mut attrs, 3)?;
            let attrs: IMFAttributes = attrs.ok_or(E_FAIL)?;
            attrs.SetUnknown(&MF_MEDIA_ENGINE_DXGI_MANAGER, &mgr)?;
            attrs.SetUnknown(&MF_MEDIA_ENGINE_CALLBACK, &notify)?;
            attrs.SetUINT32(
                &MF_MEDIA_ENGINE_VIDEO_OUTPUT_FORMAT,
                DXGI_FORMAT_B8G8R8A8_UNORM.0 as u32,
            )?;
            let factory: IMFMediaEngineClassFactory =
                CoCreateInstance(&CLSID_MFMediaEngineClassFactory, None, CLSCTX_INPROC_SERVER)?;
            let engine = factory.CreateInstance(0, &attrs)?;
            // 预览不要声音
            let _ = engine.SetMuted(true);

            let url = format!("file:///{}", path.display().to_string().replace('\\', "/"));
            engine.SetSource(&BSTR::from(url))?;
            let t0 = Instant::now();
            while !shared.loaded.load(Ordering::SeqCst) {
                if shared.error.load(Ordering::SeqCst) != 0 || t0.elapsed() > LOAD_TIMEOUT {
                    let _ = engine.Shutdown();
                    return Err(E_FAIL.into());
                }
                std::thread::sleep(Duration::from_millis(1));
            }
            let (mut w, mut h) = (0u32, 0u32);
            engine.GetNativeVideoSize(Some(&mut w), Some(&mut h))?;
            let make = |usage: D3D11_USAGE, bind: u32, cpu: u32| -> Result<ID3D11Texture2D> {
                let desc = D3D11_TEXTURE2D_DESC {
                    Width: w,
                    Height: h,
                    MipLevels: 1,
                    ArraySize: 1,
                    Format: DXGI_FORMAT_B8G8R8A8_UNORM,
                    SampleDesc: DXGI_SAMPLE_DESC {
                        Count: 1,
                        Quality: 0,
                    },
                    Usage: usage,
                    BindFlags: bind,
                    CPUAccessFlags: cpu,
                    MiscFlags: 0,
                };
                let mut t = None;
                dev.CreateTexture2D(&desc, None, Some(&mut t))?;
                t.ok_or(E_FAIL.into())
            };
            let target = make(D3D11_USAGE_DEFAULT, D3D11_BIND_RENDER_TARGET.0 as u32, 0)?;
            let staging = make(D3D11_USAGE_STAGING, 0, D3D11_CPU_ACCESS_READ.0 as u32)?;
            Ok(Self {
                engine,
                ctx,
                target,
                staging,
                shared,
                w,
                h,
                last_pts: i64::MIN,
                _mgr: mgr,
            })
        }
    }

    /// 当前播放位置（毫秒，已减去时间校正量）。
    pub fn current_ms(&self) -> u64 {
        // SAFETY: 只读查询。
        let secs = unsafe { self.engine.GetCurrentTime() };
        ((secs * MS_PER_SEC) as i64 - VIDEO_TIME_BIAS_MS).max(0) as u64
    }

    /// 开始播放。
    pub fn play(&self) -> Result<()> {
        self.shared.ended.store(false, Ordering::SeqCst);
        // SAFETY: 引擎接口调用。
        unsafe { self.engine.Play() }
    }

    /// 暂停。
    pub fn pause(&self) -> Result<()> {
        // SAFETY: 引擎接口调用。
        unsafe { self.engine.Pause() }
    }

    /// 跳转到指定毫秒（异步，之后的新帧从 [`Engine::try_frame`] 出来）。
    pub fn seek_ms(&mut self, ms: u64) -> Result<()> {
        self.last_pts = i64::MIN; // 允许 seek 后同时间戳的帧重新取
        let secs = (ms as i64 + VIDEO_TIME_BIAS_MS).max(0) as f64 / MS_PER_SEC;
        // SAFETY: 引擎接口调用。
        unsafe { self.engine.SetCurrentTime(secs) }
    }

    /// 是否已播放到文件末尾。
    pub fn ended(&self) -> bool {
        self.shared.ended.load(Ordering::SeqCst)
    }

    /// 引擎是否报过错。
    pub fn failed(&self) -> bool {
        self.shared.error.load(Ordering::SeqCst) != 0
    }

    /// 若有新帧则取出 BGRA 数据；无新帧返回 `None`。
    ///
    /// # 返回
    /// `Some(VideoFrame)` 或 `None`；取帧失败返回错误。
    pub fn try_frame(&mut self) -> Result<Option<VideoFrame>> {
        // SAFETY: 直接调 vtable 以区分 S_OK（有新帧）与 S_FALSE（无）；Map/Unmap 成对，读取范围
        // 在 RowPitch 与高度内。
        unsafe {
            let mut pts = 0i64;
            let hr = (Interface::vtable(&self.engine).OnVideoStreamTick)(
                Interface::as_raw(&self.engine),
                &mut pts,
            );
            if hr.0 != 0 {
                hr.ok()?;
                return Ok(None);
            }
            if pts == self.last_pts {
                return Ok(None);
            }
            self.last_pts = pts;
            let dst = RECT {
                left: 0,
                top: 0,
                right: self.w as i32,
                bottom: self.h as i32,
            };
            self.engine
                .TransferVideoFrame(&self.target, None, &dst, None)?;
            self.ctx.CopyResource(&self.staging, &self.target);
            let mut m = D3D11_MAPPED_SUBRESOURCE::default();
            self.ctx
                .Map(&self.staging, 0, D3D11_MAP_READ, 0, Some(&mut m))?;
            let row = self.w as usize * BGRA_BYTES;
            let mut data = vec![0u8; row * self.h as usize];
            for y in 0..self.h as usize {
                let src = (m.pData as *const u8).add(y * m.RowPitch as usize);
                std::ptr::copy_nonoverlapping(src, data.as_mut_ptr().add(y * row), row);
            }
            self.ctx.Unmap(&self.staging, 0);
            Ok(Some(VideoFrame {
                width: self.w,
                height: self.h,
                data,
            }))
        }
    }
}

impl Drop for Engine {
    /// 关闭引擎并 `MFShutdown`（与 `open` 里的 `MFStartup` 配对）。
    fn drop(&mut self) {
        // SAFETY: 引擎接口调用；Shutdown 后不再使用。
        unsafe {
            let _ = self.engine.Shutdown();
            let _ = MFShutdown();
        }
    }
}
