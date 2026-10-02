//! 抽帧图片编码：PNG / JPEG 走 Windows WIC（系统自带），无损 WebP 走 FFmpeg 已含的 libwebp。
//!
//! 不新增任何第三方依赖：WIC 是系统能力；FFmpeg 白名单里没有静态图编码器，
//! 无损 WebP 复用 `libwebp_anim` 编码一帧、写成单帧 WebP 文件。
//! 不提供有损 WebP（MVP 设计 §11 第 4 条）。
//! 编码器内部从解码帧的 YUV 平面一步转换到 BGR24 / BGRA，不经过 RGBA 中转。

use std::path::Path;

use ffmpeg_next as ffmpeg;
use ffmpeg_next::format::Pixel;
use ffmpeg_next::{Dictionary, Rational, codec, encoder, format, frame};
use snow_recorder_protocol::ImageFormat;
use windows::Win32::Foundation::GENERIC_WRITE;
use windows::Win32::Graphics::Imaging::{
    CLSID_WICImagingFactory, GUID_ContainerFormatJpeg, GUID_ContainerFormatPng,
    GUID_WICPixelFormat24bppBGR, IWICImagingFactory, WICBitmapEncoderNoCache,
};
use windows::Win32::System::Com::StructuredStorage::{IPropertyBag2, PROPBAG2};
use windows::Win32::System::Com::{
    CLSCTX_INPROC_SERVER, COINIT_MULTITHREADED, CoCreateInstance, CoInitializeEx, CoUninitialize,
};
use windows::Win32::System::Variant::VARIANT;
use windows::core::{HSTRING, PWSTR};

use super::EditError;
use super::yuv::RgbConverter;

/// WIC 中 JPEG 质量属性的名字。
const WIC_IMAGE_QUALITY: &str = "ImageQuality";
/// 编码单帧 WebP 用的时间基（只有一帧，取值无意义）。
const WEBP_TIME_BASE: Rational = Rational(1, 1000);

/// 单张图片编码器；每个工作线程各持有一个，不跨线程共享。
pub trait ImageEncoder {
    /// 把解码帧（YUV 直通）编码并写到 `path`。
    ///
    /// # 参数
    /// - `frame`：解码帧。
    /// - `path`：输出文件路径（覆盖已有文件）。
    fn encode(&mut self, frame: &frame::Video, path: &Path) -> Result<(), EditError>;
}

/// 创建某格式的图片编码器（须在将要使用它的线程里调用）。
///
/// # 参数
/// - `format`：图片格式。
/// - `quality`：JPEG 质量 1..=100，其余格式忽略。
///
/// # 返回
/// 编码器；系统组件不可用时返回可读错误。
pub fn make_encoder(format: ImageFormat, quality: u8) -> Result<Box<dyn ImageEncoder>, EditError> {
    match format {
        ImageFormat::Png => Ok(Box::new(WicEncoder::new(false, quality)?)),
        ImageFormat::Jpeg => Ok(Box::new(WicEncoder::new(true, quality)?)),
        ImageFormat::WebpLossless => Ok(Box::new(WebpEncoder::new()?)),
    }
}

/// 线程级 COM 初始化守卫。
pub(crate) struct ComGuard(bool);

impl ComGuard {
    /// 以多线程套间初始化当前线程的 COM。
    pub(crate) fn init() -> Self {
        // SAFETY: 标准 COM 初始化；成功才在 Drop 时配对反初始化。
        let ok = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) }.is_ok();
        Self(ok)
    }
}

impl Drop for ComGuard {
    /// 配对反初始化。
    fn drop(&mut self) {
        if self.0 {
            // SAFETY: 与成功的 CoInitializeEx 配对。
            unsafe { CoUninitialize() };
        }
    }
}

/// WIC 编码器（PNG / JPEG），输入 BGR24 的原始字节。
pub(crate) struct BgrWriter {
    /// 为 true 编码 JPEG，否则 PNG。
    jpeg: bool,
    /// JPEG 质量 0.0..=1.0。
    quality: f32,
    /// WIC 工厂。
    factory: IWICImagingFactory,
    /// COM 守卫，须在 `factory` 之后释放。
    _com: ComGuard,
}

/// 把 WIC / 其它 windows 错误转成可读错误。
fn wic_err(what: &str, e: windows::core::Error) -> EditError {
    EditError::new(format!("{what}: {e}"))
}

impl BgrWriter {
    /// 创建 WIC 工厂（须在将要使用它的线程里调用）。
    ///
    /// # 参数
    /// - `jpeg`：为 true 写 JPEG，否则 PNG。
    /// - `quality`：JPEG 质量 1..=100，PNG 忽略。
    pub(crate) fn new(jpeg: bool, quality: u8) -> Result<Self, EditError> {
        let com = ComGuard::init();
        // SAFETY: COM 已在本线程初始化。
        let factory: IWICImagingFactory =
            unsafe { CoCreateInstance(&CLSID_WICImagingFactory, None, CLSCTX_INPROC_SERVER) }
                .map_err(|e| wic_err("创建 WIC 工厂失败", e))?;
        Ok(Self {
            jpeg,
            quality: f32::from(quality.clamp(1, 100)) / 100.0,
            factory,
            _com: com,
        })
    }

    /// 把 BGR24 像素写成图片文件。
    ///
    /// # 参数
    /// - `size`：宽高。
    /// - `stride`：行字节数。
    /// - `data`：像素数据（自上而下）。
    /// - `path`：输出路径（覆盖已有文件）。
    pub(crate) fn write(
        &self,
        size: (u32, u32),
        stride: usize,
        data: &[u8],
        path: &Path,
    ) -> Result<(), EditError> {
        write_bgr(
            &self.factory,
            self.jpeg,
            self.quality,
            size,
            stride,
            data,
            path,
        )
    }
}

/// FFmpeg 引擎用的 WIC 编码器（先把 YUV 一步转 BGR24）。
struct WicEncoder {
    /// 底层写入器。
    writer: BgrWriter,
    /// YUV -> BGR24 转换器。
    conv: RgbConverter,
}

impl WicEncoder {
    /// 创建 WIC 编码器。
    fn new(jpeg: bool, quality: u8) -> Result<Self, EditError> {
        Ok(Self {
            writer: BgrWriter::new(jpeg, quality)?,
            conv: RgbConverter::new(Pixel::BGR24),
        })
    }
}

/// 把一张 BGR24 位图经 WIC 写成 PNG 或 JPEG 文件。
///
/// # 参数
/// - `factory`：WIC 工厂。
/// - `jpeg`：为 true 写 JPEG，否则 PNG。
/// - `quality`：JPEG 质量 0.0..=1.0。
/// - `size`：宽高。
/// - `stride`：行字节数。
/// - `data`：BGR24 像素（自上而下）。
/// - `path`：输出路径。
fn write_bgr(
    factory: &IWICImagingFactory,
    jpeg: bool,
    quality: f32,
    size: (u32, u32),
    stride: usize,
    data: &[u8],
    path: &Path,
) -> Result<(), EditError> {
    let (width, height) = size;
    let name = HSTRING::from(path.as_os_str());
    let container = if jpeg {
        GUID_ContainerFormatJpeg
    } else {
        GUID_ContainerFormatPng
    };
    // SAFETY: 以下均为 WIC 的标准编码流程；所有指针/切片在调用期间有效。
    unsafe {
        let stream = factory
            .CreateStream()
            .map_err(|e| wic_err("创建流失败", e))?;
        stream
            .InitializeFromFilename(&name, GENERIC_WRITE.0)
            .map_err(|e| wic_err("打开输出文件失败", e))?;
        let encoder = factory
            .CreateEncoder(&container, std::ptr::null())
            .map_err(|e| wic_err("创建图片编码器失败", e))?;
        encoder
            .Initialize(&stream, WICBitmapEncoderNoCache)
            .map_err(|e| wic_err("初始化图片编码器失败", e))?;
        let mut frame_enc = None;
        let mut props: Option<IPropertyBag2> = None;
        encoder
            .CreateNewFrame(&mut frame_enc, &mut props)
            .map_err(|e| wic_err("创建图片帧失败", e))?;
        let frame_enc = frame_enc.ok_or_else(|| EditError::new("WIC 未返回图片帧"))?;
        if jpeg && let Some(props) = &props {
            write_quality(props, quality)?;
        }
        frame_enc
            .Initialize(props.as_ref())
            .map_err(|e| wic_err("初始化图片帧失败", e))?;
        frame_enc
            .SetSize(width, height)
            .map_err(|e| wic_err("设置尺寸失败", e))?;
        let mut fmt = GUID_WICPixelFormat24bppBGR;
        frame_enc
            .SetPixelFormat(&mut fmt)
            .map_err(|e| wic_err("设置像素格式失败", e))?;
        if fmt != GUID_WICPixelFormat24bppBGR {
            return Err(EditError::new("WIC 编码器不接受 BGR24 输入"));
        }
        // 缓冲末行可能不满一个 stride，切到实际需要的长度
        let need = stride * (height as usize - 1) + width as usize * 3;
        let pixels = data
            .get(..need)
            .ok_or_else(|| EditError::new("像素缓冲不足"))?;
        frame_enc
            .WritePixels(height, stride as u32, pixels)
            .map_err(|e| wic_err("写入像素失败", e))?;
        frame_enc
            .Commit()
            .map_err(|e| wic_err("提交图片帧失败", e))?;
        encoder.Commit().map_err(|e| wic_err("提交图片失败", e))?;
    }
    Ok(())
}

/// 向 JPEG 属性包写入质量。
///
/// # 参数
/// - `props`：编码帧的属性包。
/// - `quality`：0.0..=1.0。
fn write_quality(props: &IPropertyBag2, quality: f32) -> Result<(), EditError> {
    let mut name: Vec<u16> = WIC_IMAGE_QUALITY.encode_utf16().chain(Some(0)).collect();
    let bag = PROPBAG2 {
        pstrName: PWSTR(name.as_mut_ptr()),
        ..Default::default()
    };
    let value = VARIANT::from(quality);
    // SAFETY: bag 与 value 在调用期间有效，数量均为 1。
    unsafe { props.Write(1, &bag, &value) }.map_err(|e| wic_err("设置 JPEG 质量失败", e))
}

impl ImageEncoder for WicEncoder {
    /// 见 trait 说明；先一步转 BGR24 再交给 WIC。
    fn encode(&mut self, frame: &frame::Video, path: &Path) -> Result<(), EditError> {
        let bgr = self.conv.convert(frame)?;
        self.writer.write(
            (bgr.width(), bgr.height()),
            bgr.stride(0),
            bgr.data(0),
            path,
        )
    }
}

/// 无损 WebP 编码器（FFmpeg libwebp）。
struct WebpEncoder {
    /// YUV -> BGRA 转换器（libwebp 无损需要 ARGB，且避免 BT.601/709 误解读）。
    conv: RgbConverter,
}

impl WebpEncoder {
    /// 检查 libwebp 编码器可用。
    fn new() -> Result<Self, EditError> {
        ffmpeg::init().map_err(|e| EditError::new(format!("初始化 FFmpeg 失败: {e}")))?;
        if encoder::find_by_name("libwebp_anim").is_none() {
            return Err(EditError::new(
                "FFmpeg 缺少 libwebp 编码器，无法导出无损 WebP",
            ));
        }
        Ok(Self {
            conv: RgbConverter::new(Pixel::BGRA),
        })
    }
}

impl ImageEncoder for WebpEncoder {
    /// 见 trait 说明；每张图独立开一个单帧 WebP 复用器。
    fn encode(&mut self, frame: &frame::Video, path: &Path) -> Result<(), EditError> {
        let bgra = self.conv.convert(frame)?;
        let fail = |what: &str, e: ffmpeg::Error| EditError::new(format!("{what}: {e}"));
        let codec = encoder::find_by_name("libwebp_anim")
            .ok_or_else(|| EditError::new("FFmpeg 缺少 libwebp 编码器"))?;
        let mut enc = codec::context::Context::new_with_codec(codec)
            .encoder()
            .video()
            .map_err(|e| fail("创建 WebP 编码器失败", e))?;
        enc.set_width(bgra.width());
        enc.set_height(bgra.height());
        enc.set_format(Pixel::BGRA);
        enc.set_time_base(WEBP_TIME_BASE);
        let mut opts = Dictionary::new();
        opts.set("lossless", "1");
        let mut enc = enc
            .open_with(opts)
            .map_err(|e| fail("打开 WebP 编码器失败", e))?;
        let mut out = format::output_as(path, "webp").map_err(|e| fail("创建 WebP 输出失败", e))?;
        {
            let mut stream = out
                .add_stream(codec)
                .map_err(|e| fail("添加 WebP 流失败", e))?;
            stream.set_time_base(WEBP_TIME_BASE);
            stream.set_parameters(&enc);
        }
        out.write_header().map_err(|e| fail("写 WebP 头失败", e))?;
        let mut input = bgra.clone();
        input.set_pts(Some(0));
        enc.send_frame(&input)
            .map_err(|e| fail("送入 WebP 编码器失败", e))?;
        enc.send_eof().map_err(|e| fail("结束 WebP 编码失败", e))?;
        let mut packet = ffmpeg::Packet::empty();
        while enc.receive_packet(&mut packet).is_ok() {
            packet.set_stream(0);
            packet
                .write_interleaved(&mut out)
                .map_err(|e| fail("写 WebP 数据失败", e))?;
        }
        out.write_trailer().map_err(|e| fail("写 WebP 尾失败", e))?;
        Ok(())
    }
}
