//! 硬件编码：D3D11 NV12 帧池 + 厂商 H.264 编码器（QSV/NVENC/AMF）+ MP4 封装。
//!
//! 帧池与编码器共用采集设备（零拷贝）；QSV 走 D3D11VA 帧池派生的 QSV 帧上下文。
//! 这里的实现参照上游 `snow-recording-export` 的 GPU 输入路径，但编码参数按实测调优
//! （QSV `async_depth=2`、`preset=veryfast`），并把"持有上一帧算时长"改成"持有上一包算时长"，
//! 送帧不再被时长决定拖延一帧。

use std::ffi::c_void;
use std::path::PathBuf;
use std::ptr;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use ffmpeg::ffi::*;
use ffmpeg_next as ffmpeg;
use snow_d3d11::SharedDevice;
use windows::Win32::Graphics::Direct3D11::{D3D11_BIND_RENDER_TARGET, ID3D11Texture2D};
use windows::core::Interface;

/// 结果类型：错误为单行原因文本。
pub type HwResult<T> = Result<T, String>;

use crate::pipeline::{EncoderStats, VideoEncoder};

/// 帧池默认容量（送编中 2~3 帧 + 编码器驱动引用 + 正在合成 1 帧，再留出吸收编码耗时尖峰的余量）。
pub const DEFAULT_POOL_CAPACITY: usize = 12;
/// QSV 默认 async_depth（实测 1→2 收益最大，2→4 持平）。
pub const QSV_ASYNC_DEPTH: u32 = 2;
/// QSV 默认预设。
pub const QSV_PRESET: &str = "veryfast";
/// 关键帧间隔（秒）。
const GOP_SECONDS: u32 = 2;
/// NV12 纹理宽高对齐。
const SURFACE_ALIGN: u32 = 16;

/// 把 FFmpeg 返回码转成错误文本。
fn check(code: i32, what: &str) -> HwResult<()> {
    if code < 0 { Err(format!("{what}: {}", ffmpeg::Error::from(code))) } else { Ok(()) }
}

/// AVBufferRef 的所有者（析构时解引用）。
struct Buf(*mut AVBufferRef);

impl Buf {
    /// 包装非空指针。
    fn new(p: *mut AVBufferRef, what: &str) -> HwResult<Self> {
        if p.is_null() { Err(format!("{what} 分配失败")) } else { Ok(Self(p)) }
    }

    /// 新增一份引用。
    fn reference(&self) -> HwResult<*mut AVBufferRef> {
        let r = unsafe { av_buffer_ref(self.0) };
        if r.is_null() { Err("av_buffer_ref 失败".into()) } else { Ok(r) }
    }
}

impl Drop for Buf {
    /// 释放引用。
    fn drop(&mut self) {
        unsafe { av_buffer_unref(&mut self.0) };
    }
}

/// D3D11VA 设备锁回调：进入（与录制设备共用同一把多线程保护锁）。
unsafe extern "C" fn lock_device(opaque: *mut c_void) {
    // SAFETY: opaque 是 `HwContext::new` 装箱的 SharedDevice，设备上下文存续期间有效。
    unsafe { (&*opaque.cast::<SharedDevice>()).enter() };
}

/// D3D11VA 设备锁回调：离开。
unsafe extern "C" fn unlock_device(opaque: *mut c_void) {
    // SAFETY: 同 `lock_device`，与其成对调用。
    unsafe { (&*opaque.cast::<SharedDevice>()).leave() };
}

/// 设备上下文释放回调：回收装箱的 SharedDevice。
unsafe extern "C" fn free_device(context: *mut AVHWDeviceContext) {
    // SAFETY: user_opaque 由 `Box::into_raw` 产生，仅此处释放一次。
    unsafe { drop(Box::from_raw((*context).user_opaque.cast::<SharedDevice>())) };
}

/// 帧池占用许可：随帧的 AVBuffer 引用一起释放时归还名额。
struct Permit(Arc<AtomicUsize>);

impl Drop for Permit {
    /// 归还名额。
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::AcqRel);
    }
}

/// AVBuffer 释放回调：销毁许可。
unsafe extern "C" fn free_permit(_opaque: *mut c_void, data: *mut u8) {
    // SAFETY: data 由 `Box::into_raw(Box<Permit>)` 产生，仅此处释放一次。
    unsafe { drop(Box::from_raw(data.cast::<Permit>())) };
}

/// 编码用 NV12 表面：合成线程写入，编码线程送入编码器。
pub struct Surface {
    /// FFmpeg 帧（持有池许可）。
    native: ffmpeg::frame::Video,
    /// 帧底层纹理。
    texture: ID3D11Texture2D,
    /// 纹理数组切片号。
    slice: u32,
}

impl Surface {
    /// 底层 NV12 纹理。
    pub fn texture(&self) -> &ID3D11Texture2D {
        &self.texture
    }

    /// 纹理数组切片号。
    pub fn slice(&self) -> u32 {
        self.slice
    }
}

// SAFETY: 帧内部是引用计数的 AVBuffer，纹理由设备的多线程保护锁串行访问。
unsafe impl Send for Surface {}

/// 硬件上下文：设备、NV12 帧池与（QSV 时）派生上下文，可跨线程共享。
pub struct HwContext {
    /// 采集设备（保持存活，锁回调与帧池都依赖它）。
    _device: SharedDevice,
    /// D3D11VA 设备上下文。
    device_ref: Buf,
    /// NV12 帧池上下文。
    native: Buf,
    /// QSV 设备上下文（仅 QSV）。
    qsv_device: Option<Buf>,
    /// 映射到 QSV 的帧上下文（仅 QSV）。
    mapped: Option<Buf>,
    /// 视频尺寸。
    size: (u32, u32),
    /// 在用表面数。
    outstanding: Arc<AtomicUsize>,
    /// 池容量。
    capacity: usize,
    /// 编码器名。
    codec_name: &'static str,
}

// SAFETY: AVBufferRef 引用计数为原子操作；其余字段是 Arc/原子量。
unsafe impl Send for HwContext {}
// SAFETY: 同上；池分配与映射在 FFmpeg 内部加锁。
unsafe impl Sync for HwContext {}

impl HwContext {
    /// 创建硬件上下文。
    ///
    /// # 参数
    /// - `device`：采集所在的 D3D11 设备（编码器必须与采集同适配器）。
    /// - `size`：视频尺寸（宽、高）。
    /// - `capacity`：帧池容量。
    ///
    /// # 返回
    /// 上下文；适配器无受支持的硬件编码器或初始化失败返回原因。
    pub fn new(device: SharedDevice, size: (u32, u32), capacity: usize) -> HwResult<Self> {
        let codec_name = snow_d3d11::h264_encoder(device.identity().vendor)
            .ok_or("采集适配器没有受支持的硬件编码器")?;
        let device_ref = Buf::new(unsafe { av_hwdevice_ctx_alloc(AVHWDeviceType::AV_HWDEVICE_TYPE_D3D11VA) }, "hwdevice")?;
        // SAFETY: device_ref 刚分配且未初始化；字段按 FFmpeg 约定填写后再 init。
        unsafe {
            let context = (*device_ref.0).data.cast::<AVHWDeviceContext>();
            let d3d = (*context).hwctx.cast::<AVD3D11VADeviceContext>();
            (*d3d).device = device.device().clone().into_raw().cast();
            (*d3d).device_context = device.context().clone().into_raw().cast();
            let owner = Box::into_raw(Box::new(device.clone())).cast::<c_void>();
            (*context).user_opaque = owner;
            (*context).free = Some(free_device);
            (*d3d).lock_ctx = owner;
            (*d3d).lock = Some(lock_device);
            (*d3d).unlock = Some(unlock_device);
            check(av_hwdevice_ctx_init(device_ref.0), "hwdevice init")?;
        }
        let native = Buf::new(unsafe { av_hwframe_ctx_alloc(device_ref.0) }, "hwframes")?;
        // SAFETY: native 刚分配，填写后 init。
        unsafe {
            let context = (*native.0).data.cast::<AVHWFramesContext>();
            (*context).format = AVPixelFormat::AV_PIX_FMT_D3D11;
            (*context).sw_format = AVPixelFormat::AV_PIX_FMT_NV12;
            (*context).width = i32::try_from(size.0.div_ceil(SURFACE_ALIGN) * SURFACE_ALIGN).map_err(|e| e.to_string())?;
            (*context).height = i32::try_from(size.1.div_ceil(SURFACE_ALIGN) * SURFACE_ALIGN).map_err(|e| e.to_string())?;
            // 每帧独立纹理，避免 AMF 数组索引元数据共享。
            (*context).initial_pool_size = 0;
            let d3d = (*context).hwctx.cast::<AVD3D11VAFramesContext>();
            (*d3d).BindFlags = D3D11_BIND_RENDER_TARGET.0 as u32;
            check(av_hwframe_ctx_init(native.0), "hwframes init")?;
        }
        let (qsv_device, mapped) = if codec_name == "h264_qsv" {
            let mut qsv = ptr::null_mut();
            // SAFETY: 输出指针指向局部变量，device_ref 已初始化。
            check(
                unsafe { av_hwdevice_ctx_create_derived(&mut qsv, AVHWDeviceType::AV_HWDEVICE_TYPE_QSV, device_ref.0, 0) },
                "派生 QSV 设备",
            )?;
            let qsv = Buf::new(qsv, "QSV 设备")?;
            let mut mapped = ptr::null_mut();
            // SAFETY: 同上。
            check(
                unsafe {
                    av_hwframe_ctx_create_derived(
                        &mut mapped,
                        AVPixelFormat::AV_PIX_FMT_QSV,
                        qsv.0,
                        native.0,
                        AV_HWFRAME_MAP_READ as i32 | AV_HWFRAME_MAP_DIRECT as i32,
                    )
                },
                "派生 QSV 帧上下文",
            )?;
            (Some(qsv), Some(Buf::new(mapped, "QSV 帧上下文")?))
        } else {
            (None, None)
        };
        Ok(Self {
            _device: device,
            device_ref,
            native,
            qsv_device,
            mapped,
            size,
            outstanding: Arc::new(AtomicUsize::new(0)),
            capacity,
            codec_name,
        })
    }

    /// 编码器名（如 `h264_qsv`）。
    pub fn codec_name(&self) -> &'static str {
        self.codec_name
    }

    /// 编码器输入像素格式。
    fn pixel_format(&self) -> ffmpeg::format::Pixel {
        if self.mapped.is_some() { AVPixelFormat::AV_PIX_FMT_QSV.into() } else { AVPixelFormat::AV_PIX_FMT_D3D11.into() }
    }

    /// 从池中取一张 NV12 表面；池耗尽返回 `None`（调用方丢帧）。
    pub fn allocate(&self) -> HwResult<Option<Surface>> {
        if self
            .outstanding
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |n| (n < self.capacity).then_some(n + 1))
            .is_err()
        {
            return Ok(None);
        }
        let permit = Box::into_raw(Box::new(Permit(Arc::clone(&self.outstanding)))).cast::<u8>();
        let mut native = ffmpeg::frame::Video::empty();
        // SAFETY: native.native 已初始化；失败路径会归还许可。
        let code = unsafe { av_hwframe_get_buffer(self.native.0, native.as_mut_ptr(), 0) };
        if code < 0 {
            // SAFETY: permit 尚未交给任何 AVBuffer，由此处回收。
            unsafe { drop(Box::from_raw(permit.cast::<Permit>())) };
            return Err(format!("取帧池表面失败: {}", ffmpeg::Error::from(code)));
        }
        // SAFETY: permit 指向有效 Permit，释放回调负责销毁。
        let reference = unsafe {
            av_buffer_create(permit, size_of::<Permit>(), Some(free_permit), ptr::null_mut(), 0)
        };
        if reference.is_null() {
            // SAFETY: 引用创建失败，许可仍归此处所有。
            unsafe { drop(Box::from_raw(permit.cast::<Permit>())) };
            return Err("表面生命周期引用分配失败".into());
        }
        // SAFETY: opaque_ref 随帧释放而解引用，从而归还许可。
        unsafe { (*native.as_mut_ptr()).opaque_ref = reference };
        native.set_width(self.size.0);
        native.set_height(self.size.1);
        // SAFETY: data[0] 是 D3D11 纹理指针，data[1] 是数组切片号（D3D11VA 约定）。
        let (texture, slice) = unsafe {
            let raw = (*native.as_ptr()).data[0].cast::<c_void>();
            let texture = ID3D11Texture2D::from_raw_borrowed(&raw).cloned().ok_or("帧缺少 D3D11 纹理")?;
            (texture, (*native.as_ptr()).data[1] as usize as u32)
        };
        Ok(Some(Surface { native, texture, slice }))
    }

    /// 预热帧池：先取出再放回 `count` 张表面，让驱动提前分配纹理（首次分配 NV12 纹理可能耗时数十毫秒，
    /// 放到录制中途会卡住合成线程）。
    ///
    /// # 参数
    /// - `count`：预热的表面数（不超过池容量）。
    pub fn prewarm(&self, count: usize) -> HwResult<()> {
        let mut held = Vec::new();
        for _ in 0..count.min(self.capacity) {
            match self.allocate()? {
                Some(surface) => held.push(surface),
                None => break,
            }
        }
        Ok(())
    }

    /// 在用表面数（诊断用）。
    pub fn outstanding(&self) -> usize {
        self.outstanding.load(Ordering::Acquire)
    }

    /// 把表面转换成可送编码器的帧（QSV 需映射）。
    fn encode_frame(&self, surface: Surface) -> HwResult<ffmpeg::frame::Video> {
        let Some(mapped) = &self.mapped else { return Ok(surface.native) };
        let mut out = ffmpeg::frame::Video::empty();
        // SAFETY: out 是空帧；映射会持有源帧引用直到 out 释放。
        unsafe {
            (*out.as_mut_ptr()).format = AVPixelFormat::AV_PIX_FMT_QSV as i32;
            (*out.as_mut_ptr()).hw_frames_ctx = mapped.reference()?;
            check(
                av_hwframe_map(out.as_mut_ptr(), surface.native.as_ptr(), AV_HWFRAME_MAP_READ as i32 | AV_HWFRAME_MAP_DIRECT as i32),
                "hwframe 映射",
            )?;
            check(av_frame_copy_props(out.as_mut_ptr(), surface.native.as_ptr()), "复制帧属性")?;
        }
        Ok(out)
    }
}

/// 硬件编码配置。
#[derive(Debug, Clone)]
pub struct HwConfig {
    /// 输出文件（扩展名 mp4）。
    pub path: PathBuf,
    /// 视频宽。
    pub width: u32,
    /// 视频高。
    pub height: u32,
    /// 帧率（时间基 1/fps，pts 为槽号）。
    pub fps: u32,
    /// 质量参数（QSV global_quality / NVENC qp / AMF qp）。
    pub quality: u32,
    /// QSV async_depth。
    pub async_depth: u32,
    /// QSV 预设。
    pub preset: String,
}

/// 持有上一包、到下一包才确定其时长的缝合器。
///
/// 无 B 帧时包按 pts 升序到达：上一包时长 = 下一包 pts - 上一包 pts，末包时长取终点 - pts。
#[derive(Debug)]
pub struct DurationStitcher<T> {
    /// 持有的上一项：`(pts, 载荷)`。
    held: Option<(i64, T)>,
}

impl<T> Default for DurationStitcher<T> {
    /// 空缝合器。
    fn default() -> Self {
        Self { held: None }
    }
}

impl<T> DurationStitcher<T> {
    /// 放入新项。
    ///
    /// # 参数
    /// - `pts`：新项时间戳。
    /// - `item`：载荷。
    ///
    /// # 返回
    /// 上一项及其时长（至少 1）；这是第一项时返回 `None`。
    ///
    /// # 示例
    /// ```ignore
    /// let mut s = DurationStitcher::default();
    /// assert!(s.push(0, "a").is_none());
    /// assert_eq!(s.push(3, "b"), Some(("a", 3)));
    /// ```
    pub fn push(&mut self, pts: i64, item: T) -> Option<(T, i64)> {
        self.held.replace((pts, item)).map(|(p, t)| (t, (pts - p).max(1)))
    }

    /// 结束：吐出最后一项及其时长。
    ///
    /// # 参数
    /// - `end`：排他终点（与 pts 同单位）。
    pub fn finish(&mut self, end: i64) -> Option<(T, i64)> {
        self.held.take().map(|(p, t)| (t, (end - p).max(1)))
    }
}

/// 编码统计。
#[derive(Debug, Default, Clone)]
pub struct HwStats {
    /// 已送入编码器的帧数。
    pub frames: u64,
    /// 已写出的视频包数。
    pub packets: u64,
    /// 每帧 `映射+send_frame` 耗时样本（毫秒）。
    pub send_ms: Vec<f32>,
}

/// 硬件编码器：只在编码线程内使用。
pub struct HwEncoder {
    /// 共享硬件上下文。
    ctx: Arc<HwContext>,
    /// 已打开的编码器。
    encoder: ffmpeg::encoder::video::Encoder,
    /// 输出封装。
    output: ffmpeg::format::context::Output,
    /// 视频流序号。
    stream_index: usize,
    /// 封装层时间基。
    stream_time_base: ffmpeg::Rational,
    /// 包时长缝合器。
    stitcher: DurationStitcher<ffmpeg::Packet>,
    /// 统计。
    pub stats: HwStats,
}

/// 构造 QSV/NVENC/AMF 的编码选项。
///
/// # 参数
/// - `codec`：编码器名。
/// - `cfg`：配置。
pub fn encoder_options(codec: &str, cfg: &HwConfig) -> ffmpeg::Dictionary<'static> {
    let q = cfg.quality.to_string();
    let mut o = ffmpeg::Dictionary::new();
    o.set("bf", "0");
    match codec {
        "h264_nvenc" => {
            o.set("preset", "p4");
            o.set("tune", "ull");
            o.set("rc", "constqp");
            o.set("qp", &q);
            o.set("rc-lookahead", "0");
            o.set("delay", "0");
            o.set("surfaces", &DEFAULT_POOL_CAPACITY.to_string());
        }
        "h264_amf" => {
            o.set("usage", "ultralowlatency");
            o.set("quality", "balanced");
            o.set("rc", "cqp");
            o.set("qp_i", &q);
            o.set("qp_p", &q);
            o.set("preanalysis", "0");
            o.set("preencode", "0");
            o.set("query_timeout", "100");
        }
        _ => {
            o.set("preset", &cfg.preset);
            o.set("look_ahead", "0");
            o.set("async_depth", &cfg.async_depth.to_string());
            o.set("global_quality", &q);
        }
    }
    o
}

impl HwEncoder {
    /// 打开编码器并写 MP4 头。
    ///
    /// # 参数
    /// - `ctx`：硬件上下文（决定编码器厂商）。
    /// - `cfg`：编码配置。
    ///
    /// # 返回
    /// 编码器；编码器缺失/打开失败返回原因。
    pub fn open(ctx: Arc<HwContext>, cfg: &HwConfig) -> HwResult<Self> {
        ffmpeg::init().map_err(|e| e.to_string())?;
        let codec = ffmpeg::encoder::find_by_name(ctx.codec_name()).ok_or_else(|| format!("FFmpeg 缺少 {}", ctx.codec_name()))?;
        let mut output = ffmpeg::format::output(&cfg.path).map_err(|e| format!("创建输出失败 {}: {e}", cfg.path.display()))?;
        let global_header = output.format().flags().contains(ffmpeg::format::Flags::GLOBAL_HEADER);
        let fps = i32::try_from(cfg.fps).map_err(|e| e.to_string())?;
        let mut video = ffmpeg::codec::context::Context::new_with_codec(codec)
            .encoder()
            .video()
            .map_err(|e| format!("创建编码器上下文失败: {e}"))?;
        video.set_width(cfg.width);
        video.set_height(cfg.height);
        video.set_format(ctx.pixel_format());
        video.set_time_base(ffmpeg::Rational(1, fps));
        video.set_frame_rate(Some(ffmpeg::Rational(fps, 1)));
        video.set_gop(cfg.fps * GOP_SECONDS);
        // SAFETY: 编码器上下文尚未打开；引用计数的硬件上下文交给它持有。
        unsafe {
            let c = video.as_mut_ptr();
            (*c).hw_device_ctx = ctx.qsv_device.as_ref().unwrap_or(&ctx.device_ref).reference()?;
            (*c).hw_frames_ctx = ctx.mapped.as_ref().unwrap_or(&ctx.native).reference()?;
            (*c).max_b_frames = 0;
            (*c).color_range = AVColorRange::AVCOL_RANGE_MPEG;
            (*c).colorspace = AVColorSpace::AVCOL_SPC_BT709;
            (*c).color_primaries = AVColorPrimaries::AVCOL_PRI_BT709;
            (*c).color_trc = AVColorTransferCharacteristic::AVCOL_TRC_BT709;
        }
        if global_header {
            video.set_flags(ffmpeg::codec::Flags::GLOBAL_HEADER);
        }
        let encoder = video
            .open_as_with(codec, encoder_options(ctx.codec_name(), cfg))
            .map_err(|e| format!("打开 {} 失败: {e}", ctx.codec_name()))?;
        let stream_index = {
            let mut stream = output.add_stream(codec).map_err(|e| format!("添加视频轨失败: {e}"))?;
            stream.set_time_base(ffmpeg::Rational(1, fps));
            stream.set_rate(ffmpeg::Rational(fps, 1));
            stream.set_avg_frame_rate(ffmpeg::Rational(fps, 1));
            stream.set_parameters(&encoder);
            stream.index()
        };
        output.write_header().map_err(|e| format!("写文件头失败: {e}"))?;
        let stream_time_base = output.stream(stream_index).map(|s| s.time_base()).ok_or("写头后视频轨丢失")?;
        Ok(Self {
            ctx,
            encoder,
            output,
            stream_index,
            stream_time_base,
            stitcher: DurationStitcher::default(),
            stats: HwStats::default(),
        })
    }

    /// 送入一帧并取走已完成的包。
    ///
    /// # 参数
    /// - `surface`：已合成完毕（且已 Flush）的 NV12 表面。
    /// - `pts`：槽号（严格递增）。
    pub fn submit(&mut self, surface: Surface, pts: i64) -> HwResult<()> {
        let started = std::time::Instant::now();
        let mut frame = self.ctx.encode_frame(surface)?;
        frame.set_pts(Some(pts));
        // SAFETY: 帧已由 FFmpeg 分配，仅设置色彩标记。
        unsafe {
            let f = frame.as_mut_ptr();
            (*f).color_range = AVColorRange::AVCOL_RANGE_MPEG;
            (*f).colorspace = AVColorSpace::AVCOL_SPC_BT709;
            (*f).color_primaries = AVColorPrimaries::AVCOL_PRI_BT709;
            (*f).color_trc = AVColorTransferCharacteristic::AVCOL_TRC_BT709;
        }
        match self.encoder.send_frame(&frame) {
            Ok(()) => {}
            Err(e) if is_eagain(&e) => {
                self.drain()?;
                self.encoder.send_frame(&frame).map_err(|e| format!("送帧重试失败: {e}"))?;
            }
            Err(e) => return Err(format!("送帧失败: {e}")),
        }
        self.stats.frames += 1;
        if self.stats.send_ms.len() < STAT_SAMPLE_LIMIT {
            self.stats.send_ms.push(started.elapsed().as_secs_f32() * 1000.0);
        }
        self.drain()
    }

    /// 取走编码器里已完成的包并写入封装。
    fn drain(&mut self) -> HwResult<()> {
        loop {
            let mut packet = ffmpeg::Packet::empty();
            match self.encoder.receive_packet(&mut packet) {
                Ok(()) => {
                    let pts = packet.pts().or(packet.dts()).unwrap_or(0);
                    if let Some((prev, duration)) = self.stitcher.push(pts, packet) {
                        self.write(prev, duration)?;
                    }
                }
                Err(ffmpeg::Error::Eof) => return Ok(()),
                Err(e) if is_eagain(&e) => return Ok(()),
                Err(e) => return Err(format!("取包失败: {e}")),
            }
        }
    }

    /// 写一个视频包（按时长与时间基换算）。
    fn write(&mut self, mut packet: ffmpeg::Packet, duration: i64) -> HwResult<()> {
        packet.set_duration(duration);
        packet.set_stream(self.stream_index);
        packet.rescale_ts(self.encoder.time_base(), self.stream_time_base);
        packet.write_interleaved(&mut self.output).map_err(|e| format!("写包失败: {e}"))?;
        self.stats.packets += 1;
        Ok(())
    }

    /// 结束录制：冲刷编码器、写末包与文件尾。
    ///
    /// # 参数
    /// - `end_pts`：排他终点槽号（末帧时长 = 终点 - 末帧 pts）。
    pub fn finish_stream(mut self, end_pts: i64) -> HwResult<HwStats> {
        self.encoder.send_eof().map_err(|e| format!("送 EOF 失败: {e}"))?;
        self.drain()?;
        if let Some((last, duration)) = self.stitcher.finish(end_pts) {
            self.write(last, duration)?;
        }
        self.output.write_trailer().map_err(|e| format!("写文件尾失败: {e}"))?;
        Ok(self.stats)
    }
}

impl VideoEncoder for HwEncoder {
    type Surface = Surface;

    /// 送入一帧并取走已完成的包。
    fn submit(&mut self, surface: Surface, pts: i64) -> Result<(), String> {
        HwEncoder::submit(self, surface, pts)
    }

    /// 冲刷编码器、写末包与文件尾。
    fn finish(self, end_pts: i64) -> Result<EncoderStats, String> {
        let stats = self.finish_stream(end_pts)?;
        Ok(EncoderStats { frames: stats.frames, send_ms: stats.send_ms })
    }
}

/// 统计样本上限（避免长录制无限增长）。
pub const STAT_SAMPLE_LIMIT: usize = 20_000;

/// 是否为 EAGAIN。
fn is_eagain(e: &ffmpeg::Error) -> bool {
    matches!(e, ffmpeg::Error::Other { errno } if *errno == ffmpeg::error::EAGAIN)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 缝合器：时长为相邻 pts 之差，末项用终点，且至少为 1。
    #[test]
    fn stitcher_assigns_durations() {
        let mut s = DurationStitcher::default();
        assert!(s.push(0, 'a').is_none());
        assert_eq!(s.push(3, 'b'), Some(('a', 3)));
        assert_eq!(s.push(4, 'c'), Some(('b', 1)));
        assert_eq!(s.finish(10), Some(('c', 6)));
        assert!(s.finish(10).is_none());
    }

    /// 重复 pts 或终点不足时时长钳到 1。
    #[test]
    fn stitcher_clamps_to_one() {
        let mut s = DurationStitcher::default();
        s.push(5, 1);
        assert_eq!(s.push(5, 2), Some((1, 1)));
        assert_eq!(s.finish(5), Some((2, 1)));
    }

    /// QSV 选项：async_depth 与预设按配置写入，且关闭 B 帧与前瞻。
    #[test]
    fn qsv_options_follow_config() {
        let cfg = HwConfig {
            path: PathBuf::from("a.mp4"),
            width: 64,
            height: 64,
            fps: 30,
            quality: 18,
            async_depth: 2,
            preset: "veryfast".into(),
        };
        let o = encoder_options("h264_qsv", &cfg);
        assert_eq!(o.get("async_depth"), Some("2"));
        assert_eq!(o.get("preset"), Some("veryfast"));
        assert_eq!(o.get("look_ahead"), Some("0"));
        assert_eq!(o.get("bf"), Some("0"));
        assert_eq!(o.get("global_quality"), Some("18"));
        assert_eq!(encoder_options("h264_nvenc", &cfg).get("rc"), Some("constqp"));
    }
}
