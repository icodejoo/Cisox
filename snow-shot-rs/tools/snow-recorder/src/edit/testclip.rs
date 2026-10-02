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
    /// 是否附带一条 AAC 静音音轨（时长与视频一致）。
    pub audio: bool,
    /// 是否在帧内加纵向亮度渐变和横向色差渐变（首像素仍是帧序号亮度），用来发现上下翻转/通道互换。
    pub gradient: bool,
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
            audio: false,
            gradient: false,
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
    let mut aenc = if clip.audio {
        Some(add_audio_stream(&mut out).map_err(|e| err("audio", e))?)
    } else {
        None
    };
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
        if clip.gradient {
            fill_gradient(&mut f, clip);
        }
        f.set_pts(Some(i64::from(k)));
        enc.send_frame(&f).map_err(|e| err("send", e))?;
        drain(&mut enc, &mut out)?;
    }
    enc.send_eof().map_err(|e| err("eof", e))?;
    drain(&mut enc, &mut out)?;
    if let Some(a) = aenc.as_mut() {
        let samples = u64::from(clip.frames) * u64::from(AUDIO_RATE) / u64::from(clip.fps);
        write_silence(a, &mut out, samples).map_err(|e| err("audio write", e))?;
    }
    out.write_trailer().map_err(|e| err("trailer", e))
}

/// 给帧叠加梯度：亮度随行增加（首行不变），V 色差随列增加。
fn fill_gradient(f: &mut frame::Video, clip: &Clip) {
    let (w, h) = (clip.width as usize, clip.height as usize);
    let stride = f.stride(0);
    let rows = f.data_mut(0);
    for y in 0..h {
        for x in 0..w {
            rows[y * stride + x] = rows[y * stride + x].saturating_add((y * 40 / h) as u8);
        }
    }
    let cs = f.stride(2);
    let v = f.data_mut(2);
    for y in 0..h / 2 {
        for x in 0..w / 2 {
            v[y * cs + x] = 128 + (x * 60 / (w / 2).max(1)) as u8;
        }
    }
}

/// 输出文件的音频概况。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AudioFacts {
    /// 音频包数。
    pub packets: u64,
    /// 音频总时长（毫秒，按包时长累加）。
    pub duration_ms: i64,
    /// 采样率。
    pub rate: u32,
}

/// 读取文件的音频概况；没有音频流返回 `None`。
///
/// # 参数
/// - `path`：媒体文件路径。
pub fn audio_facts(path: &Path) -> Option<AudioFacts> {
    ffmpeg::init().ok()?;
    let mut input = format::input(path).ok()?;
    let (idx, tb, rate) = {
        let s = input.streams().best(ffmpeg::media::Type::Audio)?;
        // SAFETY: 参数指针由流持有，仅读取采样率字段（白名单里没有 AAC 解码器，不能开解码器）。
        let rate = unsafe { (*s.parameters().as_ptr()).sample_rate } as u32;
        (s.index(), s.time_base(), rate)
    };
    let (mut packets, mut ticks) = (0u64, 0i64);
    let mut pkt = ffmpeg::Packet::empty();
    while pkt.read(&mut input).is_ok() {
        if pkt.stream() == idx {
            packets += 1;
            ticks += pkt.duration();
        }
    }
    let duration_ms = ticks * 1000 * i64::from(tb.numerator()) / i64::from(tb.denominator());
    Some(AudioFacts {
        packets,
        duration_ms,
        rate,
    })
}

/// 音频采样率。
const AUDIO_RATE: u32 = 44_100;
/// AAC 编码器的预滚采样数。
const AAC_PRIMING: i64 = 1024;
/// 输出音频流下标（视频之后加入）。
const AUDIO_OUT_INDEX: usize = 1;

/// 加一条 AAC 单声道音轨并打开编码器。
fn add_audio_stream(
    out: &mut format::context::Output,
) -> Result<encoder::audio::Encoder, ffmpeg::Error> {
    let codec = encoder::find(codec::Id::AAC).ok_or(ffmpeg::Error::EncoderNotFound)?;
    let mut enc = codec::context::Context::new_with_codec(codec)
        .encoder()
        .audio()?;
    enc.set_rate(AUDIO_RATE as i32);
    enc.set_channel_layout(ffmpeg::ChannelLayout::MONO);
    enc.set_format(ffmpeg::format::Sample::F32(
        ffmpeg::format::sample::Type::Planar,
    ));
    enc.set_time_base(Rational(1, AUDIO_RATE as i32));
    if out.format().flags().contains(format::Flags::GLOBAL_HEADER) {
        enc.set_flags(codec::Flags::GLOBAL_HEADER);
    }
    let enc = enc.open()?;
    let mut stream = out.add_stream(codec)?;
    stream.set_parameters(&enc);
    Ok(enc)
}

/// 写入指定采样数的静音并收尾。
fn write_silence(
    enc: &mut encoder::audio::Encoder,
    out: &mut format::context::Output,
    total: u64,
) -> Result<(), ffmpeg::Error> {
    let tb = Rational(1, AUDIO_RATE as i32);
    let out_tb = out
        .stream(AUDIO_OUT_INDEX)
        .ok_or(ffmpeg::Error::StreamNotFound)?
        .time_base();
    let chunk = 1024u64;
    let mut pos = 0u64;
    let drain = |enc: &mut encoder::audio::Encoder, out: &mut format::context::Output| {
        let mut pkt = ffmpeg::Packet::empty();
        while enc.receive_packet(&mut pkt).is_ok() {
            pkt.set_stream(AUDIO_OUT_INDEX);
            // AAC 编码器的首包 PTS 是 -1024（预滚），整体后移让音轨与视频同起点，
            // 否则 MP4 复用器会把视频首帧拉长去对齐，样片时间轴就不均匀了
            pkt.set_pts(pkt.pts().map(|p| p + AAC_PRIMING));
            pkt.set_dts(pkt.dts().map(|p| p + AAC_PRIMING));
            pkt.rescale_ts(tb, out_tb);
            pkt.write_interleaved(out)?;
        }
        Ok::<(), ffmpeg::Error>(())
    };
    while pos < total {
        let n = chunk.min(total - pos) as usize;
        let mut f = frame::Audio::new(
            ffmpeg::format::Sample::F32(ffmpeg::format::sample::Type::Planar),
            n,
            ffmpeg::ChannelLayout::MONO,
        );
        f.set_rate(AUDIO_RATE);
        f.set_pts(Some(pos as i64));
        f.data_mut(0).fill(0);
        enc.send_frame(&f)?;
        drain(enc, out)?;
        pos += n as u64;
    }
    enc.send_eof()?;
    drain(enc, out)
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
