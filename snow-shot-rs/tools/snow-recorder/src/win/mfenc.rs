//! Media Foundation 硬件 H.264 编码后端。
//!
//! 复用自建流水线的 D3D11 NV12 帧池：NV12 纹理经 `MFCreateDXGISurfaceBuffer` 零拷贝包成样本，
//! 交给带 DXGI 设备管理器的 SinkWriter（系统自带，自动挑厂商硬件 MFT 并直接封装 MP4）。
//! 零第三方依赖（只用 `windows` 已有特性与其自带的 `windows-core`）。由 `SNOW_RECORDER_HARDWARE=mf` 或默认模式启用。
//!
//! - 打开前用 `MFTEnum2(HARDWARE)` 按采集适配器 LUID 探测硬件 H.264 编码 MFT，没有就报错让上层回落，
//!   并在 SinkWriter 建好后核对实际选中的编码 MFT 确为硬件 MFT（不静默用微软软件 MFT）。
//! - 码率控制默认平均码率（NVIDIA MFT 实测不接受恒定质量/QP/VBR 设置，只认平均码率）；
//!   `SNOW_RECORDER_MF_QUALITY` 可选恒定质量，经 `ICodecAPI` 设置并读回确认，不被接受则回落平均码率。
//! - 输入/输出媒体类型标注限幅 BT.709（与 VideoProcessor 输出一致），否则 MFT 会按全幅再压一次范围，画面偏灰。
//! - 表面归还是严格的：每个样本挂一个持有表面的 COM 对象，MFT 释放样本（输入已消费）时才归还帧池。

use std::sync::{Arc, Mutex};
use std::time::Instant;

use windows::Win32::Foundation::{LUID, RPC_E_CHANGED_MODE};
use windows::Win32::Graphics::Direct3D11::ID3D11Texture2D;
use windows::Win32::Graphics::Dxgi::IDXGIDevice;
use windows::Win32::Media::MediaFoundation::*;
use windows::Win32::System::Com::{COINIT_MULTITHREADED, CoInitializeEx, CoTaskMemFree};
use windows::Win32::System::Variant::VARIANT;
use windows::core::{GUID, IUnknown, Interface, PCWSTR, implement};

use crate::pipeline::{EncoderStats, VideoEncoder};
use crate::win::hwenc::{HwContext, HwResult, STAT_SAMPLE_LIMIT, Surface};

/// 100ns 时间单位每秒的数量。
const HNS_PER_SECOND: i64 = 10_000_000;
/// 平均码率系数：每像素每帧的比特数（1080p60 约 15Mbps）。类桌面合成内容上，0.12 的 PSNR/SSIM 与 NVENC qp24 相当
/// （45.5dB/0.988），但码率约为其 1.5~1.7 倍；NVENC qp18（47dB）约需 0.24。实测数据见实验台账。
const BITS_PER_PIXEL: f64 = 0.12;
/// 关键帧间隔（秒）。
const GOP_SECONDS: u32 = 2;
/// 环境变量：MF 恒定质量（1..=100，仅在 MFT 读回确认接受时生效；缺省/其它值用平均码率）。
/// NVIDIA MFT 不接受，其他厂商未验证，所以默认不开。
pub const ENV_MF_QUALITY: &str = "SNOW_RECORDER_MF_QUALITY";
/// 环境变量：平均码率系数（每像素每帧比特数，0.02..=0.5，缺省 0.12）。
pub const ENV_MF_BPP: &str = "SNOW_RECORDER_MF_BPP";
/// 环境变量：非空时禁用 Media Foundation 编码（现场出问题时的开关，自动模式会改用 FFmpeg 厂商硬编）。
pub const ENV_MF_DISABLE: &str = "SNOW_RECORDER_MF_DISABLE";
/// 环境变量：非空时测端到端延迟（送帧到成品文件写入）并在结束时打印。
pub const ENV_MF_LATENCY: &str = "SNOW_RECORDER_MF_LATENCY";
/// 挂在样本上的表面持有者属性键（任意唯一 GUID）。
const HOLDER_KEY: GUID = GUID::from_u128(0x5a0e_7c31_94d2_4b6f_8a11_2c7d_90e3_b4f1);
/// MF 路径的帧池容量：MFT 会积压十几帧输入才吐出第一批输出（NVIDIA 实测约 18 帧），
/// 样本释放前表面不能复用；容量留足余量，帧池按需分配，用不到的部分不占显存。
pub const MF_POOL_CAPACITY: usize = 64;
/// 诊断样本上限。
const LATENCY_SAMPLE_LIMIT: usize = 20_000;
/// 文件长度轮询间隔。
const POLL_INTERVAL: std::time::Duration = std::time::Duration::from_micros(300);

/// 码率控制方式。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RateControl {
    /// 恒定质量（0..=100）。
    Quality(u32),
    /// 平均码率（按像素*帧率估算）。
    AverageBitrate,
}

/// 解析 `SNOW_RECORDER_MF_QUALITY`：1..=100 取恒定质量（超出夹到边界），缺省或无法解析取平均码率。
///
/// # 参数
/// - `text`：环境变量值。
///
/// # 示例
/// ```ignore
/// assert_eq!(parse_rate_control(Some("85")), RateControl::Quality(85));
/// ```
pub fn parse_rate_control(text: Option<&str>) -> RateControl {
    match text.and_then(|t| t.trim().parse::<u32>().ok()) {
        Some(q) if q > 0 => RateControl::Quality(q.min(100)),
        _ => RateControl::AverageBitrate,
    }
}

/// 把 MF 整数对（宽高、帧率、宽高比）打包成属性所需的 64 位值。
///
/// # 参数
/// - `high`：高 32 位。
/// - `low`：低 32 位。
pub fn pack_pair(high: u32, low: u32) -> u64 {
    (u64::from(high) << 32) | u64::from(low)
}

/// 目标平均码率(bps):按像素数、帧率与系数线性估算,至少 1Mbps。
///
/// # 参数
/// - `size`:输出尺寸。
/// - `fps`:帧率。
/// - `bpp`:每像素每帧比特数。
pub fn target_bitrate(size: (u32, u32), fps: u32, bpp: f64) -> u32 {
    let bits = f64::from(size.0) * f64::from(size.1) * f64::from(fps) * bpp;
    (bits as u32).max(1_000_000)
}

/// 读取生效的码率系数：环境变量（限制在 0.02..=0.5）优先，否则默认值。
pub fn configured_bpp() -> f64 {
    std::env::var(ENV_MF_BPP).ok().and_then(|v| v.trim().parse::<f64>().ok()).map_or(BITS_PER_PIXEL, |v| v.clamp(0.02, 0.5))
}

/// 槽号换算成 100ns 时间戳。
///
/// # 参数
/// - `pts`：槽号。
/// - `fps`：帧率。
pub fn slot_to_hns(pts: i64, fps: u32) -> i64 {
    pts * HNS_PER_SECOND / i64::from(fps)
}

/// 挂在样本上的表面持有者：样本被 MFT 释放（输入已消费）时才销毁，从而严格归还帧池名额。
#[implement(IMFAsyncCallback)]
struct SurfaceHolder {
    /// 被持有的表面（销毁时归还帧池）。
    _surface: Surface,
    /// 送帧时刻。
    sent: Instant,
    /// 输入被消费的耗时样本（毫秒）。
    consumed_ms: Arc<Mutex<Vec<f32>>>,
}

impl Drop for SurfaceHolder {
    /// 记录"送帧到 MFT 释放样本"的耗时。
    fn drop(&mut self) {
        let ms = self.sent.elapsed().as_secs_f32() * 1000.0;
        if let Ok(mut v) = self.consumed_ms.lock()
            && v.len() < LATENCY_SAMPLE_LIMIT
        {
            v.push(ms);
        }
    }
}

// 持有者只借用 IMFAsyncCallback 作为 COM 外壳，从不被当作回调调用。
impl IMFAsyncCallback_Impl for SurfaceHolder_Impl {
    fn GetParameters(&self, _flags: *mut u32, _queue: *mut u32) -> windows::core::Result<()> {
        Err(windows::Win32::Foundation::E_NOTIMPL.into())
    }

    fn Invoke(&self, _result: windows::core::Ref<IMFAsyncResult>) -> windows::core::Result<()> {
        Ok(())
    }
}

/// 端到端延迟探针：轮询成品文件长度，成品落盘后用封装包偏移换算每帧的写入时刻。
struct LatencyProbe {
    /// 每帧送帧时刻（按送帧顺序）。
    submits: Vec<Instant>,
    /// 停止轮询标志。
    stop: Arc<std::sync::atomic::AtomicBool>,
    /// 轮询线程（返回 `(时刻, 文件长度)` 变化序列）。
    poller: Option<std::thread::JoinHandle<Vec<(Instant, u64)>>>,
}

impl LatencyProbe {
    /// 开始轮询 `path` 的长度。
    fn start(path: std::path::PathBuf) -> Self {
        let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let flag = Arc::clone(&stop);
        let poller = std::thread::spawn(move || {
            let mut log: Vec<(Instant, u64)> = Vec::new();
            let mut last = 0u64;
            while !flag.load(std::sync::atomic::Ordering::Acquire) {
                if let Ok(meta) = std::fs::metadata(&path)
                    && meta.len() != last
                {
                    last = meta.len();
                    log.push((Instant::now(), last));
                }
                std::thread::sleep(POLL_INTERVAL);
            }
            log
        });
        Self { submits: Vec::new(), stop, poller: Some(poller) }
    }

    /// 停止轮询并按成品里每个视频包的结束偏移算出延迟样本（毫秒）。
    fn finish(mut self, path: &std::path::Path) -> Vec<f32> {
        self.stop.store(true, std::sync::atomic::Ordering::Release);
        let log = self.poller.take().and_then(|h| h.join().ok()).unwrap_or_default();
        let Ok(mut input) = ffmpeg_next::format::input(path) else { return Vec::new() };
        let mut ends: Vec<u64> = input
            .packets()
            .filter(|(s, _)| s.parameters().medium() == ffmpeg_next::media::Type::Video)
            .map(|(_, p)| (p.position().max(0) as u64) + p.size() as u64)
            .collect();
        // 文件顺序即送帧顺序（低延迟、无 B 帧）
        ends.truncate(self.submits.len());
        ends.iter()
            .zip(&self.submits)
            .filter_map(|(end, sent)| log.iter().find(|(_, len)| len >= end).map(|(t, _)| t.saturating_duration_since(*sent).as_secs_f32() * 1000.0))
            .collect()
    }
}

/// Media Foundation 编码器：只在编码线程内使用。
pub struct MfEncoder {
    /// 帧池（保持存活）。
    _ctx: Arc<HwContext>,
    /// DXGI 设备管理器（保持存活）。
    _manager: IMFDXGIDeviceManager,
    /// 带硬件 MFT 的 SinkWriter。
    writer: IMFSinkWriter,
    /// 视频流序号。
    stream: u32,
    /// 帧率。
    fps: u32,
    /// 送入的帧数。
    frames: u64,
    /// NV12 样本字节数（宽*高*3/2）。
    frame_bytes: u32,
    /// 是否已初始化当前线程的 COM。
    com_ready: bool,
    /// 送帧耗时样本（毫秒）。
    send_ms: Vec<f32>,
    /// 样本被 MFT 释放（输入已消费）的耗时样本（毫秒）。
    consumed_ms: Arc<Mutex<Vec<f32>>>,
    /// 端到端延迟探针（仅设了 `SNOW_RECORDER_MF_LATENCY` 时存在）。
    probe: Option<LatencyProbe>,
    /// 输出路径。
    path: std::path::PathBuf,
    /// 实际选中的编码 MFT 名称与码率控制描述（诊断用）。
    description: String,
}

// SAFETY: SinkWriter/设备管理器是自由线程（MTA）对象；编码器只由创建者移交给唯一的编码线程使用。
unsafe impl Send for MfEncoder {}

/// 把 Windows 错误转成带上下文的文本。
fn fail(what: &str, e: windows::core::Error) -> String {
    format!("{what}: {e}")
}

/// 初始化当前线程的 COM 为 MTA（线程已是别的模式时视为可用）。
fn init_com() {
    // SAFETY: 重复调用无害。
    let hr = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) };
    if hr.is_err() && hr != RPC_E_CHANGED_MODE {
        eprintln!("CoInitializeEx 失败: {hr:?}");
    }
}

/// 读取激活对象的友好名称。
fn friendly_name(activate: &IMFActivate) -> String {
    let mut buf = [0u16; 256];
    let mut len = 0u32;
    // SAFETY: buf 足够大，len 是有效输出指针。
    match unsafe { activate.GetString(&MFT_FRIENDLY_NAME_Attribute, &mut buf, Some(&mut len)) } {
        Ok(()) => String::from_utf16_lossy(&buf[..(len as usize).min(buf.len())]),
        Err(_) => "(未知)".into(),
    }
}

/// 设备所在适配器的 LUID。
fn adapter_luid(ctx: &HwContext) -> Option<LUID> {
    let dxgi: IDXGIDevice = ctx.device().device().cast().ok()?;
    // SAFETY: 只读查询。
    unsafe { dxgi.GetAdapter().ok()?.GetDesc().ok().map(|d| d.AdapterLuid) }
}

/// 枚举给定适配器上的硬件 H.264 编码 MFT。
///
/// # 参数
/// - `luid`：适配器 LUID；`None` 不限适配器。
///
/// # 返回
/// 硬件 MFT 友好名称列表（按系统排序，空表示没有）。需先 `MFStartup`。
pub fn enumerate_hardware_h264(luid: Option<LUID>) -> Result<Vec<String>, String> {
    let output = MFT_REGISTER_TYPE_INFO { guidMajorType: MFMediaType_Video, guidSubtype: MFVideoFormat_H264 };
    let mut list: *mut Option<IMFActivate> = std::ptr::null_mut();
    let mut count = 0u32;
    // SAFETY: 输出指针指向局部变量；返回的数组由 CoTaskMem 分配，下面负责释放。
    let result = unsafe {
        let mut attrs: Option<IMFAttributes> = None;
        if let Some(l) = luid {
            MFCreateAttributes(&mut attrs, 1).map_err(|e| fail("创建枚举属性", e))?;
            if let Some(a) = &attrs {
                a.SetUINT64(&MFT_ENUM_ADAPTER_LUID, u64::from(l.LowPart) | (u64::from(l.HighPart as u32) << 32)).map_err(|e| e.to_string())?;
            }
        }
        MFTEnum2(MFT_CATEGORY_VIDEO_ENCODER, MFT_ENUM_FLAG_HARDWARE | MFT_ENUM_FLAG_SORTANDFILTER, None, Some(&output), attrs.as_ref(), &mut list, &mut count)
    };
    result.map_err(|e| fail("MFTEnum2(HARDWARE)", e))?;
    let mut names = Vec::new();
    // SAFETY: list 指向 count 个元素；取出后交给 Rust 管理，再释放数组本体。
    unsafe {
        for i in 0..count as usize {
            if let Some(activate) = (*list.add(i)).take() {
                names.push(friendly_name(&activate));
            }
        }
        CoTaskMemFree(Some(list.cast()));
    }
    Ok(names)
}

/// 核对 SinkWriter 实际选中的视频编码 MFT 是硬件 MFT；软件 MFT 或查询不到返回原因。
fn verify_hardware_transform(writer: &IMFSinkWriter, stream: u32) -> Result<(String, IMFTransform), String> {
    let ex: IMFSinkWriterEx = writer.cast().map_err(|e| fail("SinkWriterEx", e))?;
    for index in 0..8 {
        let mut category = GUID::zeroed();
        let mut transform: Option<IMFTransform> = None;
        // SAFETY: 输出指针指向局部变量。
        if unsafe { ex.GetTransformForStream(stream, index, Some(&mut category), &mut transform) }.is_err() {
            break;
        }
        if category != MFT_CATEGORY_VIDEO_ENCODER {
            continue;
        }
        let transform = transform.ok_or("编码 MFT 为空")?;
        // SAFETY: 只读取属性。
        let attrs = unsafe { transform.GetAttributes() }.map_err(|e| fail("读取 MFT 属性", e))?;
        // 硬件 MFT 带 `MFT_ENUM_HARDWARE_URL_Attribute`；微软软件 MFT 没有
        // SAFETY: 只读取属性长度。
        return if unsafe { attrs.GetStringLength(&MFT_ENUM_HARDWARE_URL_Attribute) }.is_ok() {
            let mut buf = [0u16; 256];
            let mut len = 0u32;
            // SAFETY: buf 足够大。
            let name = match unsafe { attrs.GetString(&MFT_FRIENDLY_NAME_Attribute, &mut buf, Some(&mut len)) } {
                Ok(()) => String::from_utf16_lossy(&buf[..(len as usize).min(buf.len())]),
                Err(_) => "(未知)".into(),
            };
            Ok((name, transform))
        } else {
            Err("SinkWriter 选中的是软件 H.264 编码 MFT，拒绝使用".into())
        };
    }
    Err("查询不到 SinkWriter 的编码 MFT".into())
}

impl MfEncoder {
    /// 打开 SinkWriter 并开始写入。
    ///
    /// # 参数
    /// - `ctx`：硬件上下文（提供设备与 NV12 帧池，纹理尺寸须与 `size` 一致）。
    /// - `path`：输出 mp4 路径。
    /// - `size`：输出尺寸（偶数）。
    /// - `fps`：帧率。
    /// - `rate_control`：码率控制方式。
    ///
    /// # 返回
    /// 编码器；系统没有匹配适配器的硬件 H.264 MFT、选中了软件 MFT 或初始化失败返回原因。
    pub fn open(ctx: Arc<HwContext>, path: &std::path::Path, size: (u32, u32), fps: u32, rate_control: RateControl) -> HwResult<Self> {
        if std::env::var_os(ENV_MF_DISABLE).is_some() {
            return Err(format!("{ENV_MF_DISABLE} 已禁用 Media Foundation 编码"));
        }
        init_com();
        // SAFETY: 以下均为 Media Foundation 初始化与对象创建的 FFI 调用，参数都是局部有效值。
        unsafe {
            MFStartup(MF_VERSION, MFSTARTUP_FULL).map_err(|e| fail("MFStartup", e))?;
            let luid = adapter_luid(&ctx);
            let names = enumerate_hardware_h264(luid)?;
            if names.is_empty() {
                return Err("系统没有硬件 H.264 编码 MFT".into());
            }
            if std::env::var_os(ENV_MF_LATENCY).is_some() {
                eprintln!("硬件 H.264 MFT 枚举(LUID {:?}): {names:?}", luid.map(|l| (l.HighPart, l.LowPart)));
            }
            let mut token = 0u32;
            let mut manager = None;
            MFCreateDXGIDeviceManager(&mut token, &mut manager).map_err(|e| fail("创建 DXGI 设备管理器", e))?;
            let manager = manager.ok_or("DXGI 设备管理器为空")?;
            manager.ResetDevice(ctx.device().device(), token).map_err(|e| fail("设备管理器绑定设备", e))?;

            let mut attrs = None;
            MFCreateAttributes(&mut attrs, 4).map_err(|e| fail("创建属性", e))?;
            let attrs: IMFAttributes = attrs.ok_or("属性为空")?;
            attrs.SetUINT32(&MF_READWRITE_ENABLE_HARDWARE_TRANSFORMS, 1).map_err(|e| e.to_string())?;
            attrs.SetUINT32(&MF_LOW_LATENCY, 1).map_err(|e| e.to_string())?;
            attrs.SetUINT32(&MF_SINK_WRITER_DISABLE_THROTTLING, 1).map_err(|e| e.to_string())?;
            attrs.SetUnknown(&MF_SINK_WRITER_D3D_MANAGER, &manager).map_err(|e| e.to_string())?;

            let wide: Vec<u16> = path.to_string_lossy().encode_utf16().chain(std::iter::once(0)).collect();
            let writer = MFCreateSinkWriterFromURL(PCWSTR(wide.as_ptr()), None, &attrs).map_err(|e| fail("创建 SinkWriter", e))?;

            let common = |media: &IMFMediaType, subtype: &GUID| -> Result<(), String> {
                media.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video).map_err(|e| e.to_string())?;
                media.SetGUID(&MF_MT_SUBTYPE, subtype).map_err(|e| e.to_string())?;
                media.SetUINT32(&MF_MT_INTERLACE_MODE, MFVideoInterlace_Progressive.0 as u32).map_err(|e| e.to_string())?;
                media.SetUINT64(&MF_MT_FRAME_SIZE, pack_pair(size.0, size.1)).map_err(|e| e.to_string())?;
                media.SetUINT64(&MF_MT_FRAME_RATE, pack_pair(fps, 1)).map_err(|e| e.to_string())?;
                media.SetUINT64(&MF_MT_PIXEL_ASPECT_RATIO, pack_pair(1, 1)).map_err(|e| e.to_string())?;
                // 色彩标记：与 VideoProcessor 输出（限幅 BT.709）一致。缺省时 MFT 会按全幅输入再压一次范围，画面偏灰
                media.SetUINT32(&MF_MT_VIDEO_NOMINAL_RANGE, MFNominalRange_16_235.0 as u32).map_err(|e| e.to_string())?;
                media.SetUINT32(&MF_MT_YUV_MATRIX, MFVideoTransferMatrix_BT709.0 as u32).map_err(|e| e.to_string())?;
                media.SetUINT32(&MF_MT_VIDEO_PRIMARIES, MFVideoPrimaries_BT709.0 as u32).map_err(|e| e.to_string())?;
                media.SetUINT32(&MF_MT_TRANSFER_FUNCTION, MFVideoTransFunc_709.0 as u32).map_err(|e| e.to_string())
            };
            let output = MFCreateMediaType().map_err(|e| e.to_string())?;
            common(&output, &MFVideoFormat_H264)?;
            output.SetUINT32(&MF_MT_AVG_BITRATE, target_bitrate(size, fps, configured_bpp())).map_err(|e| e.to_string())?;
            output.SetUINT32(&MF_MT_MPEG2_PROFILE, eAVEncH264VProfile_High.0 as u32).map_err(|e| e.to_string())?;
            output.SetUINT32(&MF_MT_MAX_KEYFRAME_SPACING, fps * GOP_SECONDS).map_err(|e| e.to_string())?;
            let stream = writer.AddStream(&output).map_err(|e| fail("添加视频流", e))?;
            let mut described = "平均码率".to_string();
            let input = MFCreateMediaType().map_err(|e| e.to_string())?;
            common(&input, &MFVideoFormat_NV12)?;
            writer.SetInputMediaType(stream, &input, None).map_err(|e| fail("设置输入类型(NV12)", e))?;
            let (name, transform) = verify_hardware_transform(&writer, stream)?;
            if let RateControl::Quality(quality) = rate_control {
                match Self::apply_quality(&transform, quality) {
                    Ok(()) => described = format!("恒定质量 {quality}"),
                    Err(e) => eprintln!("MF 恒定质量不可用，沿用平均码率: {e}"),
                }
            }
            writer.BeginWriting().map_err(|e| fail("BeginWriting", e))?;
            let probe = std::env::var_os(ENV_MF_LATENCY).is_some().then(|| LatencyProbe::start(path.to_path_buf()));
            Ok(Self {
                _ctx: ctx,
                _manager: manager,
                writer,
                stream,
                fps,
                frames: 0,
                frame_bytes: size.0 * size.1 * 3 / 2,
                com_ready: true,
                send_ms: Vec::new(),
                consumed_ms: Arc::new(Mutex::new(Vec::new())),
                probe,
                path: path.to_path_buf(),
                description: format!("{name}；{described}"),
            })
        }
    }

    /// 通过编码 MFT 的 `ICodecAPI` 设置恒定质量模式（须在 `BeginWriting` 之前调用），并读回确认 MFT 真的接受。
    ///
    /// NVIDIA MFT（驱动 566.26）对这些设置返回成功但读回仍是 CBR，此时返回错误让调用方沿用平均码率。
    fn apply_quality(transform: &IMFTransform, quality: u32) -> Result<(), String> {
        let api: ICodecAPI = transform.cast().map_err(|e| fail("MFT 不支持 ICodecAPI", e))?;
        let set = |key: &GUID, value: u32, what: &str| -> Result<(), String> {
            let variant = VARIANT::from(value);
            // SAFETY: variant 是局部有效值。
            unsafe { api.SetValue(key, &variant) }.map_err(|e| fail(what, e))
        };
        let read = |key: &GUID| -> Option<u32> {
            // SAFETY: 只读查询。
            unsafe { api.GetValue(key) }.ok().and_then(|v| u32::try_from(&v).ok())
        };
        let mode = eAVEncCommonRateControlMode_Quality.0 as u32;
        set(&CODECAPI_AVEncCommonRateControlMode, mode, "设置码率控制=质量")?;
        set(&CODECAPI_AVEncCommonQuality, quality, "设置质量值")?;
        match read(&CODECAPI_AVEncCommonRateControlMode) {
            Some(m) if m == mode => Ok(()),
            other => Err(format!("MFT 接受了设置但码率控制读回为 {other:?}（不是质量模式）")),
        }
    }

    /// 把表面包成样本写入 SinkWriter；表面随样本释放才归还帧池。
    fn write(&mut self, surface: Surface, pts: i64) -> HwResult<()> {
        if !self.com_ready {
            init_com();
            self.com_ready = true;
        }
        let texture: ID3D11Texture2D = surface.texture().clone();
        let slice = surface.slice();
        let sent = Instant::now();
        // SAFETY: 纹理有效；表面挂在样本上，样本被释放前纹理不会被复用。
        unsafe {
            let buffer = MFCreateDXGISurfaceBuffer(&ID3D11Texture2D::IID, &texture, slice, false).map_err(|e| fail("包装 DXGI 缓冲", e))?;
            buffer.SetCurrentLength(self.frame_bytes).map_err(|e| e.to_string())?;
            let sample = MFCreateSample().map_err(|e| e.to_string())?;
            sample.AddBuffer(&buffer).map_err(|e| e.to_string())?;
            sample.SetSampleTime(slot_to_hns(pts, self.fps)).map_err(|e| e.to_string())?;
            sample.SetSampleDuration(HNS_PER_SECOND / i64::from(self.fps)).map_err(|e| e.to_string())?;
            let holder: IUnknown = SurfaceHolder { _surface: surface, sent, consumed_ms: Arc::clone(&self.consumed_ms) }.into();
            sample.SetUnknown(&HOLDER_KEY, &holder).map_err(|e| e.to_string())?;
            if let Some(probe) = &mut self.probe {
                probe.submits.push(sent);
            }
            self.writer.WriteSample(self.stream, &sample).map_err(|e| fail("WriteSample", e))?;
        }
        Ok(())
    }
}

/// 分位数摘要文本。
fn summarize(label: &str, samples: &[f32]) -> String {
    if samples.is_empty() {
        return format!("{label}: 无样本");
    }
    let mut v = samples.to_vec();
    v.sort_by(|a, b| a.total_cmp(b));
    let at = |q: f32| v[((v.len() - 1) as f32 * q).round() as usize];
    format!("{label}: n={} p50={:.2} p95={:.2} max={:.2} (ms)", v.len(), at(0.5), at(0.95), at(1.0))
}

impl VideoEncoder for MfEncoder {
    type Surface = Surface;

    /// 送入一帧。
    fn submit(&mut self, surface: Surface, pts: i64) -> Result<(), String> {
        let started = Instant::now();
        self.write(surface, pts)?;
        self.frames += 1;
        if self.send_ms.len() < STAT_SAMPLE_LIMIT {
            self.send_ms.push(started.elapsed().as_secs_f32() * 1000.0);
        }
        Ok(())
    }

    /// 冲刷编码器并写完 MP4。
    fn finish(mut self, _end_pts: i64) -> Result<EncoderStats, String> {
        // SAFETY: SinkWriter 有效，Finalize 阻塞到封装完成。
        unsafe { self.writer.Finalize() }.map_err(|e| fail("Finalize", e))?;
        eprintln!("MF 编码器: {}", self.description);
        if let Ok(consumed) = self.consumed_ms.lock() {
            eprintln!("  {}", summarize("MF 输入被消费(送帧到样本释放)", &consumed));
        }
        if let Some(probe) = self.probe.take() {
            let latency = probe.finish(&self.path);
            eprintln!("  {}", summarize("MF 端到端(送帧到成品文件写入)", &latency));
        }
        Ok(EncoderStats { frames: self.frames, send_ms: std::mem::take(&mut self.send_ms) })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 打包：高 32 位在前，低 32 位在后。
    #[test]
    fn pack_pair_puts_high_first() {
        assert_eq!(pack_pair(1920, 1080), (1920u64 << 32) | 1080);
        assert_eq!(pack_pair(60, 1) >> 32, 60);
    }

    /// 槽号换算：60fps 下 60 槽恰好 1 秒，30fps 下第 1 槽是 1/30 秒。
    #[test]
    fn slot_to_hns_matches_frame_rate() {
        assert_eq!(slot_to_hns(60, 60), HNS_PER_SECOND);
        assert_eq!(slot_to_hns(1, 30), HNS_PER_SECOND / 30);
        assert_eq!(slot_to_hns(0, 30), 0);
    }

    /// 不开硬件转换时 SinkWriter 会选微软软件 H.264 MFT：核对函数必须拒绝它（系统不支持 MF 时跳过）。
    #[test]
    fn software_transform_is_rejected() {
        init_com();
        // SAFETY: 局部 COM 对象的创建与配置。
        let outcome = unsafe {
            (|| -> Result<Result<String, String>, String> {
                MFStartup(MF_VERSION, MFSTARTUP_FULL).map_err(|e| e.to_string())?;
                let path = std::env::temp_dir().join("snow-mf-soft-check.mp4");
                let wide: Vec<u16> = path.to_string_lossy().encode_utf16().chain(std::iter::once(0)).collect();
                let writer = MFCreateSinkWriterFromURL(PCWSTR(wide.as_ptr()), None, None).map_err(|e| e.to_string())?;
                let media = |subtype: &GUID| -> Result<IMFMediaType, String> {
                    let m = MFCreateMediaType().map_err(|e| e.to_string())?;
                    m.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video).map_err(|e| e.to_string())?;
                    m.SetGUID(&MF_MT_SUBTYPE, subtype).map_err(|e| e.to_string())?;
                    m.SetUINT32(&MF_MT_INTERLACE_MODE, MFVideoInterlace_Progressive.0 as u32).map_err(|e| e.to_string())?;
                    m.SetUINT64(&MF_MT_FRAME_SIZE, pack_pair(640, 480)).map_err(|e| e.to_string())?;
                    m.SetUINT64(&MF_MT_FRAME_RATE, pack_pair(30, 1)).map_err(|e| e.to_string())?;
                    Ok(m)
                };
                let output = media(&MFVideoFormat_H264)?;
                output.SetUINT32(&MF_MT_AVG_BITRATE, 2_000_000).map_err(|e| e.to_string())?;
                let stream = writer.AddStream(&output).map_err(|e| e.to_string())?;
                writer.SetInputMediaType(stream, &media(&MFVideoFormat_NV12)?, None).map_err(|e| e.to_string())?;
                writer.BeginWriting().map_err(|e| e.to_string())?;
                let verdict = verify_hardware_transform(&writer, stream).map(|(name, _)| name);
                drop(writer);
                let _ = std::fs::remove_file(&path);
                Ok(verdict)
            })()
        };
        match outcome {
            Ok(verdict) => assert!(verdict.is_err(), "软件 MFT 不应通过硬件核对: {verdict:?}"),
            Err(e) => eprintln!("跳过（本机无法创建软件 H.264 SinkWriter）: {e}"),
        }
    }

    /// 硬件 MFT 枚举不应出错（结果可以为空）。
    #[test]
    fn hardware_enumeration_does_not_fail() {
        init_com();
        // SAFETY: MFStartup 无前置条件。
        if unsafe { MFStartup(MF_VERSION, MFSTARTUP_FULL) }.is_err() {
            return;
        }
        let names = enumerate_hardware_h264(None).expect("枚举失败");
        eprintln!("硬件 H.264 MFT: {names:?}");
    }

    /// 码率控制解析：缺省/0/非数字取平均码率，数值夹在 1..=100。
    #[test]
    fn rate_control_parsing() {
        assert_eq!(parse_rate_control(None), RateControl::AverageBitrate);
        assert_eq!(parse_rate_control(Some("vbr")), RateControl::AverageBitrate);
        assert_eq!(parse_rate_control(Some("0")), RateControl::AverageBitrate);
        assert_eq!(parse_rate_control(Some(" 85 ")), RateControl::Quality(85));
        assert_eq!(parse_rate_control(Some("999")), RateControl::Quality(100));
    }

    /// 码率：随像素与帧率线性增长，且不低于 1Mbps。
    #[test]
    fn bitrate_scales_with_pixels_and_fps() {
        assert!(target_bitrate((2560, 1440), 60, BITS_PER_PIXEL) > target_bitrate((1920, 1080), 60, BITS_PER_PIXEL));
        assert!(target_bitrate((1920, 1080), 60, BITS_PER_PIXEL) > target_bitrate((1920, 1080), 30, BITS_PER_PIXEL));
        assert_eq!(target_bitrate((16, 16), 1, BITS_PER_PIXEL), 1_000_000);
        assert_eq!(target_bitrate((1920, 1080), 60, 0.12), 14_929_920);
    }
}
