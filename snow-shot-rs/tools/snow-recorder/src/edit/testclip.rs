//! 测试辅助：现场生成确定性的合成样片（H.264 MP4），并解码/检查输出图片。
//!
//! 样片第 k 帧是纯灰色，亮度 `16 + 3k`，因此解码后读一个像素就能反推帧序号。
//! 样片只放在系统临时目录，不碰上游数据目录。

use std::path::{Path, PathBuf};

use ffmpeg_next as ffmpeg;
use ffmpeg_next::format::Pixel;
use ffmpeg_next::{Dictionary, Rational, codec, encoder, format, frame, software};

/// 每帧亮度的步进。
const LUMA_STEP: u32 = 3;
/// 亮度起点（BT.709 限幅下的黑电平）。
const LUMA_BASE: u32 = 16;
/// x264 质量（越小越接近无损，保证平坦帧的亮度误差在半个步进内）。
const CRF: &str = "6";

/// 合成样片参数。
#[derive(Debug, Clone)]
pub struct Clip {
    /// 宽。
    pub width: u32,
    /// 高。
    pub height: u32,
    /// 总帧数。
    pub frames: u32,
    /// 帧率。
    pub fps: u32,
    /// 关键帧间隔。
    pub gop: u32,
    /// B 帧数（用于验证重排序下的 PTS 处理）。
    pub bframes: usize,
    /// 是否写 MP4 编辑列表；写了之后复用器可能把末帧裁出列表，解码器不再输出它。
    pub editlist: bool,
}

impl Default for Clip {
    /// 64x48、72 帧（2.88 秒）、25fps、GOP 24、无 B 帧。
    ///
    /// 帧数取 72 是为了让亮度 `16 + 3k` 不超过限幅白电平 235（x264 会钳位）。
    fn default() -> Self {
        Self {
            width: 64,
            height: 48,
            frames: 72,
            fps: 25,
            gop: 24,
            bframes: 0,
            editlist: false,
        }
    }
}

/// 在系统临时目录下建一个干净的测试目录。
///
/// # 参数
/// - `name`：测试名，区分不同测试的目录。
pub fn temp_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir()
        .join("snow-recorder-edit-tests")
        .join(format!("{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("创建测试目录");
    dir
}

/// 第 k 帧编码时的亮度值。
fn luma_of_frame(k: u32) -> u8 {
    (LUMA_BASE + LUMA_STEP * k).min(254) as u8
}

/// 第 k 帧按 BT.709 限幅转成 RGB 后的灰度值（三通道相同）。
pub fn gray_of_frame(k: u32) -> u8 {
    let y = f32::from(luma_of_frame(k));
    ((y - 16.0) * 255.0 / 219.0).round().clamp(0.0, 255.0) as u8
}

/// 由解码帧的亮度反推帧序号。
///
/// # 参数
/// - `f`：解码出的 YUV 帧。
pub fn frame_index_of(f: &frame::Video) -> i32 {
    let y = f32::from(f.data(0)[0]);
    ((y - LUMA_BASE as f32) / LUMA_STEP as f32).round() as i32
}

/// 生成合成样片。
///
/// # 参数
/// - `path`：输出 MP4 路径。
/// - `clip`：样片参数。
pub fn make_clip(path: &Path, clip: &Clip) -> Result<(), String> {
    let err = |what: &str, e: ffmpeg::Error| format!("{what}: {e}");
    ffmpeg::init().map_err(|e| err("init", e))?;
    let codec = encoder::find_by_name("libx264").ok_or("缺少 libx264")?;
    let mut out = format::output(path).map_err(|e| err("output", e))?;
    let fps = i32::try_from(clip.fps).map_err(|e| e.to_string())?;
    let tb = Rational(1, fps);
    let mut enc = codec::context::Context::new_with_codec(codec)
        .encoder()
        .video()
        .map_err(|e| err("encoder", e))?;
    enc.set_width(clip.width);
    enc.set_height(clip.height);
    enc.set_format(Pixel::YUV420P);
    enc.set_time_base(tb);
    enc.set_frame_rate(Some(Rational(fps, 1)));
    enc.set_gop(clip.gop);
    enc.set_max_b_frames(clip.bframes);
    // SAFETY: enc 尚未打开，独占持有；只写色彩标注字段。
    unsafe {
        let ctx = enc.as_mut_ptr();
        (*ctx).color_range = ffmpeg::ffi::AVColorRange::AVCOL_RANGE_MPEG;
        (*ctx).colorspace = ffmpeg::ffi::AVColorSpace::AVCOL_SPC_BT709;
    }
    if out.format().flags().contains(format::Flags::GLOBAL_HEADER) {
        enc.set_flags(codec::Flags::GLOBAL_HEADER);
    }
    let mut opts = Dictionary::new();
    opts.set("preset", "ultrafast");
    opts.set("crf", CRF);
    opts.set(
        "x264-params",
        &format!("scenecut=0:keyint={g}:min-keyint={g}", g = clip.gop),
    );
    let mut enc = enc.open_with(opts).map_err(|e| err("open", e))?;
    {
        let mut stream = out.add_stream(codec).map_err(|e| err("stream", e))?;
        stream.set_parameters(&enc);
    }
    let mut header_opts = Dictionary::new();
    if !clip.editlist {
        header_opts.set("use_editlist", "0");
    }
    out.write_header_with(header_opts)
        .map_err(|e| err("header", e))?;
    let out_tb = out.stream(0).ok_or("无输出流")?.time_base();
    let drain = |enc: &mut encoder::video::Encoder, out: &mut format::context::Output| {
        let mut pkt = ffmpeg::Packet::empty();
        while enc.receive_packet(&mut pkt).is_ok() {
            pkt.set_stream(0);
            pkt.rescale_ts(tb, out_tb);
            pkt.write_interleaved(out).map_err(|e| err("write", e))?;
        }
        Ok::<(), String>(())
    };
    for k in 0..clip.frames {
        let mut f = frame::Video::new(Pixel::YUV420P, clip.width, clip.height);
        f.data_mut(0).fill(luma_of_frame(k));
        f.data_mut(1).fill(128);
        f.data_mut(2).fill(128);
        f.set_pts(Some(i64::from(k)));
        enc.send_frame(&f).map_err(|e| err("send", e))?;
        drain(&mut enc, &mut out)?;
    }
    enc.send_eof().map_err(|e| err("eof", e))?;
    drain(&mut enc, &mut out)?;
    out.write_trailer().map_err(|e| err("trailer", e))
}

/// 用 FFmpeg 的 WebP 解码器解码一张静态 WebP，返回首像素的 R 通道。
///
/// 白名单里的 WebP 解复用器只认动画，这里直接把整个文件当一个包送进解码器。
///
/// # 参数
/// - `path`：WebP 文件路径。
pub fn decode_webp_first_pixel(path: &Path) -> u8 {
    ffmpeg::init().expect("init");
    let bytes = std::fs::read(path).expect("读文件");
    let codec = ffmpeg::decoder::find(codec::Id::WEBP).expect("WebP 解码器");
    let mut dec = codec::context::Context::new_with_codec(codec)
        .decoder()
        .video()
        .expect("打开解码器");
    dec.send_packet(&ffmpeg::Packet::copy(&bytes))
        .expect("送包");
    let mut raw = frame::Video::empty();
    dec.receive_frame(&mut raw).expect("解码");
    let mut rgb = frame::Video::new(Pixel::RGB24, raw.width(), raw.height());
    let mut ctx = software::scaling::Context::get(
        raw.format(),
        raw.width(),
        raw.height(),
        Pixel::RGB24,
        raw.width(),
        raw.height(),
        software::scaling::Flags::BILINEAR,
    )
    .expect("转换器");
    ctx.run(&raw, &mut rgb).expect("转换");
    rgb.data(0)[0]
}

/// 用 Windows WIC 解码 PNG / JPEG，返回 `(宽, 高, BGR24 像素)`（行距 = 宽 x 3）。
///
/// # 参数
/// - `path`：图片路径。
pub fn decode_bgr(path: &Path) -> (u32, u32, Vec<u8>) {
    use windows::Win32::Foundation::GENERIC_READ;
    use windows::Win32::Graphics::Imaging::{
        CLSID_WICImagingFactory, GUID_WICPixelFormat24bppBGR, IWICImagingFactory,
        WICConvertBitmapSource, WICDecodeMetadataCacheOnDemand,
    };
    use windows::Win32::System::Com::{
        CLSCTX_INPROC_SERVER, COINIT_MULTITHREADED, CoCreateInstance, CoInitializeEx,
    };
    use windows::core::HSTRING;
    // SAFETY: 测试线程里的标准 WIC 解码流程，缓冲区大小与 stride 一致。
    unsafe {
        let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
        let factory: IWICImagingFactory =
            CoCreateInstance(&CLSID_WICImagingFactory, None, CLSCTX_INPROC_SERVER)
                .expect("WIC 工厂");
        let decoder = factory
            .CreateDecoderFromFilename(
                &HSTRING::from(path.as_os_str()),
                None,
                GENERIC_READ,
                WICDecodeMetadataCacheOnDemand,
            )
            .expect("WIC 解码器");
        let frame = decoder.GetFrame(0).expect("取帧");
        let bgr = WICConvertBitmapSource(&GUID_WICPixelFormat24bppBGR, &frame).expect("转换");
        let (mut w, mut h) = (0u32, 0u32);
        bgr.GetSize(&mut w, &mut h).expect("尺寸");
        let mut buf = vec![0u8; (w * h * 3) as usize];
        bgr.CopyPixels(std::ptr::null(), w * 3, &mut buf)
            .expect("拷贝像素");
        (w, h, buf)
    }
}

/// 从 JPEG 字节里解析宽高（读 SOF 标记）。
///
/// # 参数
/// - `bytes`：JPEG 文件内容。
pub fn jpeg_size(bytes: &[u8]) -> Option<(u32, u32)> {
    let mut i = 2;
    while i + 9 < bytes.len() {
        if bytes[i] != 0xFF {
            i += 1;
            continue;
        }
        let marker = bytes[i + 1];
        if matches!(marker, 0xC0..=0xC3) {
            let h = u32::from(bytes[i + 5]) << 8 | u32::from(bytes[i + 6]);
            let w = u32::from(bytes[i + 7]) << 8 | u32::from(bytes[i + 8]);
            return Some((w, h));
        }
        let len = usize::from(bytes[i + 2]) << 8 | usize::from(bytes[i + 3]);
        i += 2 + len;
    }
    None
}
