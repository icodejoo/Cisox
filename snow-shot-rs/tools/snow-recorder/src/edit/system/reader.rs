//! Media Foundation 读取端：探测（扫描压缩样本）与按时间戳精确取帧（解码成 BGR24）。
//!
//! 取帧语义与 FFmpeg 引擎的 `VideoSource` 一致：返回 PTS 不超过目标的最后一帧，
//! 目标早于首帧返回首帧、晚于末帧返回末帧；目标离当前位置很近时直接向前解码，不重复 seek。
//! 时间戳单位为 100ns（hns）。

use std::path::Path;
use std::sync::Arc;

use snow_recorder_protocol::ProbeInfo;
use windows::Win32::Media::MediaFoundation::*;
use windows::Win32::System::Com::StructuredStorage::PROPVARIANT;
use windows::core::{GUID, Interface, PCWSTR};

use super::{mf_err, mf_init_err};
use crate::edit::EditError;

/// 每毫秒的 hns 数。
pub const HNS_PER_MS: i64 = 10_000;
/// 向前解码而不重新 seek 的最大距离（毫秒）。
const FORWARD_LIMIT_MS: i64 = 2000;
/// 流下标枚举上限，防止异常文件无限循环。
const MAX_STREAMS: u32 = 64;
/// 色彩矩阵标注缺失时，高度不小于该值按 BT.709、否则按 BT.601。
const HD_MIN_HEIGHT: u32 = 720;

/// hns 换算成毫秒（四舍五入）。
///
/// # 参数
/// - `hns`：100ns 单位的时间。
pub fn hns_to_ms(hns: i64) -> i64 {
    (hns + HNS_PER_MS / 2).div_euclid(HNS_PER_MS)
}

/// 毫秒换算成 hns。
///
/// # 参数
/// - `ms`：毫秒。
pub fn ms_to_hns(ms: u64) -> i64 {
    i64::try_from(ms).unwrap_or(i64::MAX / HNS_PER_MS) * HNS_PER_MS
}

/// SourceReader 的视频处理级别。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Processing {
    /// 不插处理器（压缩样本直通）。
    None,
    /// 颜色转换（解码成 RGB32 / NV12）。
    Basic,
}

/// 输入文件里的流下标。
#[derive(Debug, Clone, Copy)]
pub struct Streams {
    /// 第一条视频流。
    pub video: u32,
    /// 第一条音频流。
    pub audio: Option<u32>,
}

/// 一次 `ReadSample` 的结果。
pub struct ReadOut {
    /// 实际来源流。
    pub stream: u32,
    /// 流标志（`MF_SOURCE_READER_FLAG`）。
    pub flags: u32,
    /// 样本时间戳（hns）。
    pub time: i64,
    /// 样本；流结束或只是 tick 时为空。
    pub sample: Option<IMFSample>,
}

impl ReadOut {
    /// 对应流是否已结束。
    pub fn ended(&self) -> bool {
        self.flags & (MF_SOURCE_READERF_ENDOFSTREAM.0 as u32) != 0
    }

    /// 当前媒体类型是否发生了变化。
    pub fn type_changed(&self) -> bool {
        self.flags & (MF_SOURCE_READERF_CURRENTMEDIATYPECHANGED.0 as u32) != 0
    }
}

/// 打开 SourceReader。
///
/// # 参数
/// - `path`：输入文件。
/// - `processing`：视频处理级别。
///
/// # 返回
/// 读取器；打不开返回可回落的错误。
pub fn open_reader(path: &Path, processing: Processing) -> Result<IMFSourceReader, EditError> {
    let wide: Vec<u16> = path
        .as_os_str()
        .to_string_lossy()
        .encode_utf16()
        .chain(Some(0))
        .collect();
    // SAFETY: 局部有效值；wide 以 0 结尾，调用期间存活。
    unsafe {
        let mut attrs = None;
        MFCreateAttributes(&mut attrs, 2).map_err(|e| mf_init_err("创建属性失败", e))?;
        let attrs: IMFAttributes = attrs.ok_or_else(|| EditError::unsupported("属性为空"))?;
        match processing {
            Processing::None => {}
            Processing::Basic => attrs
                .SetUINT32(&MF_SOURCE_READER_ENABLE_VIDEO_PROCESSING, 1)
                .map_err(|e| mf_init_err("开启视频处理失败", e))?,
        }
        MFCreateSourceReaderFromURL(PCWSTR(wide.as_ptr()), &attrs)
            .map_err(|e| mf_init_err(&format!("无法打开输入视频 {}", path.display()), e))
    }
}

/// 找出第一条视频流与音频流的下标，并把所有流先取消选中。
///
/// # 参数
/// - `reader`：读取器。
///
/// # 返回
/// 流下标；没有视频流返回错误。
pub fn find_streams(reader: &IMFSourceReader) -> Result<Streams, EditError> {
    let (mut video, mut audio) = (None, None);
    // SAFETY: 只读查询与选择；下标越界时 API 返回错误，用来结束枚举。
    unsafe {
        let _ = reader.SetStreamSelection(MF_SOURCE_READER_ALL_STREAMS.0 as u32, false);
        for index in 0..MAX_STREAMS {
            let Ok(native) = reader.GetNativeMediaType(index, 0) else {
                break;
            };
            match native.GetGUID(&MF_MT_MAJOR_TYPE) {
                Ok(major) if major == MFMediaType_Video && video.is_none() => video = Some(index),
                Ok(major) if major == MFMediaType_Audio && audio.is_none() => audio = Some(index),
                _ => {}
            }
        }
    }
    let video = video.ok_or_else(|| EditError::unsupported("输入文件没有视频流"))?;
    Ok(Streams { video, audio })
}

/// 选中一条流。
///
/// # 参数
/// - `reader`：读取器。
/// - `stream`：流下标。
pub fn select_stream(reader: &IMFSourceReader, stream: u32) -> Result<(), EditError> {
    // SAFETY: 普通 COM 调用。
    unsafe { reader.SetStreamSelection(stream, true) }.map_err(|e| mf_init_err("选择流失败", e))
}

/// 读一个样本。
///
/// # 参数
/// - `reader`：读取器。
/// - `stream`：流下标，或 `MF_SOURCE_READER_ANY_STREAM`。
pub fn read_sample(reader: &IMFSourceReader, stream: u32) -> Result<ReadOut, EditError> {
    let (mut actual, mut flags, mut time) = (0u32, 0u32, 0i64);
    let mut sample: Option<IMFSample> = None;
    // SAFETY: 输出指针指向局部变量。
    unsafe {
        reader
            .ReadSample(
                stream,
                0,
                Some(&mut actual),
                Some(&mut flags),
                Some(&mut time),
                Some(&mut sample),
            )
            .map_err(|e| mf_err("读取样本失败", e))?;
    }
    Ok(ReadOut {
        stream: actual,
        flags,
        time,
        sample,
    })
}

/// 把整数对解包（高 32 位在前）。
fn unpack_pair(v: u64) -> (u32, u32) {
    ((v >> 32) as u32, v as u32)
}

/// 视频流原生类型里的尺寸与帧率。
#[derive(Debug, Clone, Copy)]
pub struct NativeVideo {
    /// 宽。
    pub width: u32,
    /// 高。
    pub height: u32,
    /// 帧率分子（未知为 0）。
    pub rate_num: u32,
    /// 帧率分母。
    pub rate_den: u32,
}

/// 读取视频流的原生类型。
///
/// # 参数
/// - `reader`：读取器。
/// - `stream`：视频流下标。
///
/// # 返回
/// `(原生媒体类型, 尺寸与帧率)`。
pub fn native_video(
    reader: &IMFSourceReader,
    stream: u32,
) -> Result<(IMFMediaType, NativeVideo), EditError> {
    // SAFETY: 只读查询。
    unsafe {
        let native = reader
            .GetNativeMediaType(stream, 0)
            .map_err(|e| mf_init_err("读取视频类型失败", e))?;
        let (width, height) = unpack_pair(
            native
                .GetUINT64(&MF_MT_FRAME_SIZE)
                .map_err(|e| mf_init_err("读取视频尺寸失败", e))?,
        );
        let (rate_num, rate_den) = native
            .GetUINT64(&MF_MT_FRAME_RATE)
            .map(unpack_pair)
            .unwrap_or((0, 1));
        Ok((
            native,
            NativeVideo {
                width,
                height,
                rate_num,
                rate_den: rate_den.max(1),
            },
        ))
    }
}

/// 扫描结果。
pub struct Scan {
    /// 探测信息。
    pub info: ProbeInfo,
    /// 关键帧显示时间（已扣除起点，hns，升序）。
    pub keys: Vec<i64>,
    /// 视频起点（hns）：MP4 带 B 帧时首帧的显示时间不是 0，所有时间都要扣掉它，与 FFmpeg 引擎对齐。
    pub start: i64,
    /// 视频总长（已扣除起点，hns）。
    pub length: i64,
}

/// 扫描全部压缩视频样本（不解码），统计时长、帧数、关键帧，并返回关键帧时间表。
///
/// # 参数
/// - `path`：输入视频。
///
/// # 返回
/// 扫描结果；须在 `MfSession` 内调用。
pub fn scan(path: &Path) -> Result<Scan, EditError> {
    let reader = open_reader(path, Processing::None)?;
    let streams = find_streams(&reader)?;
    select_stream(&reader, streams.video)?;
    let (native, facts) = native_video(&reader, streams.video)?;
    // SAFETY: 设为原生类型 = 取压缩样本，不插解码器。
    unsafe { reader.SetCurrentMediaType(streams.video, None, &native) }
        .map_err(|e| mf_init_err("设置压缩直通失败", e))?;
    let (mut frames, mut max_end, mut start, mut prev_dur) = (0u64, 0i64, i64::MAX, 0i64);
    let mut keys: Vec<i64> = Vec::new();
    loop {
        let out = read_sample(&reader, streams.video)?;
        if out.ended() {
            break;
        }
        let Some(sample) = out.sample else { continue };
        // SAFETY: 只读查询样本属性。
        let (mut dur, key) = unsafe {
            (
                sample.GetSampleDuration().unwrap_or(0).max(0),
                sample.GetUINT32(&MFSampleExtension_CleanPoint).unwrap_or(0) != 0,
            )
        };
        // 个别文件的末样本时长读出来是 0，会把总时长算短一帧、帧率算偏；用上一帧的时长顶替
        if dur == 0 {
            dur = prev_dur;
        }
        prev_dur = dur;
        frames += 1;
        start = start.min(out.time);
        max_end = max_end.max(out.time + dur);
        if key {
            keys.push(out.time);
        }
    }
    if frames == 0 {
        return Err(EditError::unsupported("视频中没有可读取的样本"));
    }
    keys.sort_unstable();
    for k in &mut keys {
        *k -= start;
    }
    let length = (max_end - start).max(0);
    let duration_ms = hns_to_ms(length);
    let fps_milli = if frames > 1 && duration_ms > 0 {
        ((i128::from(frames) * 1_000_000 + i128::from(duration_ms) / 2) / i128::from(duration_ms))
            as u32
    } else if facts.rate_num > 0 {
        (u64::from(facts.rate_num) * 1000 / u64::from(facts.rate_den)) as u32
    } else {
        0
    };
    let info = ProbeInfo {
        width: facts.width,
        height: facts.height,
        duration_ms: duration_ms as u64,
        fps_milli,
        frames,
        keyframes: keys.len() as u64,
    };
    Ok(Scan {
        info,
        keys,
        start,
        length,
    })
}

/// 一帧解码结果（BGR24，自上而下紧排）。
pub struct Frame {
    /// 像素数据，行距为 `width * 3`。
    pub bgr: Vec<u8>,
    /// 宽。
    pub width: u32,
    /// 高。
    pub height: u32,
    /// 显示时间（hns）。
    pub hns: i64,
}

impl Frame {
    /// 相对视频起点的毫秒。
    pub fn ms(&self) -> i64 {
        hns_to_ms(self.hns)
    }
}

/// 解码出但还没转换颜色的 NV12 样本（取帧时只转最终选中的那一帧，被丢弃的中间帧不付转换代价）。
struct Raw {
    /// 解码样本。
    sample: IMFSample,
    /// 显示时间（hns，已扣除起点）。
    hns: i64,
}

/// YUV 到 RGB 的整数系数（8 位定点）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Coeffs {
    /// 亮度缩放。
    y: i32,
    /// 亮度偏移。
    y_off: i32,
    /// R 对 V 的系数。
    r_v: i32,
    /// G 对 U 的系数（取负后使用）。
    g_u: i32,
    /// G 对 V 的系数（取负后使用）。
    g_v: i32,
    /// B 对 U 的系数。
    b_u: i32,
}

/// 选择转换系数。
///
/// # 参数
/// - `bt709`：为 true 用 BT.709，否则 BT.601。
/// - `full`：为 true 是全范围，否则限幅（16-235）。
fn coeffs_for(bt709: bool, full: bool) -> Coeffs {
    let (y, y_off) = if full { (256, 0) } else { (298, 16) };
    // 限幅下色差系数含 255/224 的放大，全幅直接用 1.0 对应的系数
    let (r_v, g_u, g_v, b_u) = match (bt709, full) {
        (true, false) => (459, 55, 136, 541),
        (false, false) => (409, 100, 208, 516),
        (true, true) => (403, 48, 120, 475),
        (false, true) => (359, 88, 183, 454),
    };
    Coeffs {
        y,
        y_off,
        r_v,
        g_u,
        g_v,
        b_u,
    }
}

/// 把 NV12 转成 BGR24（色差取最近邻，不做色度插值）。
///
/// # 参数
/// - `y_plane`：亮度平面首行指针。
/// - `uv_plane`：交错色差平面首行指针。
/// - `pitch`：两个平面的行距（字节）。
/// - `size`：输出宽高（不超过缓冲）。
/// - `c`：转换系数。
///
/// # 安全
/// 两个指针须各自覆盖 `pitch * 行数` 字节（亮度 `size.1` 行，色差 `(size.1 + 1) / 2` 行，
/// 每行至少 `size.0` 向上取偶字节）。
unsafe fn nv12_to_bgr(
    y_plane: *const u8,
    uv_plane: *const u8,
    pitch: usize,
    size: (usize, usize),
    c: Coeffs,
) -> Vec<u8> {
    let (w, h) = size;
    let mut bgr = vec![0u8; w * h * 3];
    let clamp = |v: i32| v.clamp(0, 255) as u8;
    for row in 0..h {
        // SAFETY: 调用方保证平面覆盖这些行。
        let (ys, uvs) = unsafe {
            (
                std::slice::from_raw_parts(y_plane.add(row * pitch), w),
                std::slice::from_raw_parts(uv_plane.add((row / 2) * pitch), (w + 1) & !1),
            )
        };
        let out = &mut bgr[row * w * 3..(row + 1) * w * 3];
        for (x, px) in out.chunks_exact_mut(3).enumerate() {
            let u = i32::from(uvs[x & !1]) - 128;
            let v = i32::from(uvs[(x & !1) + 1]) - 128;
            let yy = (i32::from(ys[x]) - c.y_off) * c.y;
            px[0] = clamp((yy + c.b_u * u + 128) >> 8);
            px[1] = clamp((yy - c.g_u * u - c.g_v * v + 128) >> 8);
            px[2] = clamp((yy + c.r_v * v + 128) >> 8);
        }
    }
    bgr
}

/// 已打开的解码视频源：按时间戳精确取帧。
pub struct MfSource {
    /// 读取器。
    reader: IMFSourceReader,
    /// 视频流下标。
    stream: u32,
    /// 对外输出的宽高（显示尺寸；解码器常把高度对齐到 16 的倍数，这里裁掉对齐部分）。
    size: (u32, u32),
    /// 缓冲默认行距（字节）；类型里没有时为 0，改由宽度推算。
    default_stride: i32,
    /// YUV 转换系数。
    coeffs: Coeffs,
    /// 最近一次返回的帧，用于就近向前解码。
    held: Option<Raw>,
    /// 已解出但尚未消费的下一帧。
    lookahead: Option<Raw>,
    /// 视频起点（hns），帧时间 = 样本时间 - 起点。
    start: i64,
    /// 视频总长（hns）。
    length: i64,
    /// 媒体源报告的演示时长（hns，不含 B 帧带来的起点后移）；seek 的绝对位置必须小于它，否则系统拒绝。
    presentation: i64,
}

impl MfSource {
    /// 打开视频并要求输出 NV12（解码器原生格式；颜色转换推迟到选中的那一帧）。
    ///
    /// # 参数
    /// - `path`：输入视频；须在 `MfSession` 内调用。
    /// - `scan`：该视频的扫描结果（提供显示尺寸、起点与总长）。
    ///
    /// # 返回
    /// 视频源；打不开或系统解码器不可用返回可回落的错误。
    pub fn open(path: &Path, scan: &Scan) -> Result<Self, EditError> {
        let reader = open_reader(path, Processing::Basic)?;
        let streams = find_streams(&reader)?;
        select_stream(&reader, streams.video)?;
        // SAFETY: 局部有效值。
        unsafe {
            let want = MFCreateMediaType().map_err(|e| mf_init_err("创建媒体类型失败", e))?;
            want.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)
                .map_err(|e| mf_init_err("设置主类型失败", e))?;
            want.SetGUID(&MF_MT_SUBTYPE, &MFVideoFormat_NV12)
                .map_err(|e| mf_init_err("设置子类型失败", e))?;
            reader
                .SetCurrentMediaType(streams.video, None, &want)
                .map_err(|e| mf_init_err("系统解码器不支持输出 NV12", e))?;
        }
        let mut src = Self {
            reader,
            stream: streams.video,
            size: (scan.info.width, scan.info.height),
            default_stride: 0,
            coeffs: coeffs_for(true, false),
            held: None,
            lookahead: None,
            start: scan.start,
            length: scan.length,
            presentation: i64::MAX,
        };
        src.refresh_type()?;
        // SAFETY: 只读查询媒体源属性；取不到就不限制。
        if let Ok(pv) = unsafe {
            src.reader
                .GetPresentationAttribute(MF_SOURCE_READER_MEDIASOURCE.0 as u32, &MF_PD_DURATION)
        } && let Ok(d) = u64::try_from(&pv)
        {
            src.presentation = i64::try_from(d).unwrap_or(i64::MAX);
        }
        Ok(src)
    }

    /// 读取当前输出类型里的行距、色彩矩阵与范围。
    fn refresh_type(&mut self) -> Result<(), EditError> {
        // SAFETY: 只读查询。
        unsafe {
            let current = self
                .reader
                .GetCurrentMediaType(self.stream)
                .map_err(|e| mf_err("读取输出类型失败", e))?;
            self.default_stride = current
                .GetUINT32(&MF_MT_DEFAULT_STRIDE)
                .map_or(0, |v| v as i32);
            let bt709 = match current.GetUINT32(&MF_MT_YUV_MATRIX) {
                Ok(m) if m == MFVideoTransferMatrix_BT709.0 as u32 => true,
                Ok(m) if m == MFVideoTransferMatrix_BT601.0 as u32 => false,
                _ => self.size.1 >= HD_MIN_HEIGHT,
            };
            let full = current
                .GetUINT32(&MF_MT_VIDEO_NOMINAL_RANGE)
                .is_ok_and(|r| r == MFNominalRange_0_255.0 as u32);
            self.coeffs = coeffs_for(bt709, full);
        }
        Ok(())
    }

    /// 把样本转成 BGR24 帧（裁到显示尺寸）。
    fn convert(&self, raw: &Raw) -> Result<Frame, EditError> {
        let (w, h) = (self.size.0 as usize, self.size.1 as usize);
        // SAFETY: Lock/Unlock 配对；平面指针只在锁内读取，行数由缓冲长度和行距推算并校验。
        let bgr = unsafe {
            let buffer = raw
                .sample
                .ConvertToContiguousBuffer()
                .map_err(|e| mf_err("读取样本缓冲失败", e))?;
            let len = buffer
                .GetCurrentLength()
                .map_err(|e| mf_err("读取缓冲长度失败", e))? as usize;
            let mut lock2d = None;
            let (mut top, mut pitch) = (std::ptr::null_mut::<u8>(), 0i32);
            if let Ok(b2d) = buffer.cast::<IMF2DBuffer>()
                && b2d.Lock2D(&mut top, &mut pitch).is_ok()
            {
                lock2d = Some(b2d);
            } else {
                let mut max = 0u32;
                buffer
                    .Lock(&mut top, Some(&mut max), None)
                    .map_err(|e| mf_err("锁定缓冲失败", e))?;
                pitch = self.default_stride.abs();
                if pitch == 0 {
                    pitch = i32::try_from(w).unwrap_or(0);
                }
            }
            let result = (|| {
                let pitch = usize::try_from(pitch)
                    .ok()
                    .filter(|p| *p >= w)
                    .ok_or_else(|| EditError::new("解码缓冲行距异常"))?;
                // NV12 总长 = 行距 x 平面行数 x 3/2，平面行数可能含对齐填充
                let plane_rows = len * 2 / 3 / pitch;
                if plane_rows < h {
                    return Err(EditError::new("解码缓冲长度不足"));
                }
                Ok(nv12_to_bgr(
                    top,
                    top.add(pitch * plane_rows),
                    pitch,
                    (w, h),
                    self.coeffs,
                ))
            })();
            match lock2d {
                Some(b2d) => {
                    let _ = b2d.Unlock2D();
                }
                None => {
                    let _ = buffer.Unlock();
                }
            }
            result?
        };
        Ok(Frame {
            bgr,
            width: self.size.0,
            height: self.size.1,
            hns: raw.hns,
        })
    }

    /// 取下一帧（顺序解码，不 seek）；文件结束返回 `None`。
    fn pull(&mut self) -> Result<Option<Raw>, EditError> {
        loop {
            let out = read_sample(&self.reader, self.stream)?;
            if out.type_changed() {
                self.refresh_type()?;
            }
            if out.ended() {
                return Ok(None);
            }
            let Some(sample) = out.sample else { continue };
            return Ok(Some(Raw {
                sample,
                hns: out.time - self.start,
            }));
        }
    }

    /// 按毫秒取帧，语义同 [`Self::frame_at_hns`]。
    ///
    /// # 参数
    /// - `ms`：相对视频起点的毫秒。
    pub fn frame_at_ms(&mut self, ms: u64) -> Result<Arc<Frame>, EditError> {
        self.frame_at_hns(ms_to_hns(ms))
    }

    /// 按 hns 精确取帧：返回显示时间不超过目标的最后一帧。
    ///
    /// 目标早于首帧返回首帧，晚于末帧返回末帧。
    ///
    /// # 参数
    /// - `target`：目标时间（hns）。
    ///
    /// # 返回
    /// 解码帧；文件里没有可解码帧时返回错误。
    pub fn frame_at_hns(&mut self, target: i64) -> Result<Arc<Frame>, EditError> {
        let target = target.max(0);
        let reuse = self
            .held
            .as_ref()
            .is_some_and(|h| h.hns <= target && target - h.hns <= FORWARD_LIMIT_MS * HNS_PER_MS);
        let mut best = if reuse {
            self.held.take()
        } else {
            self.seek_to(target)?;
            None
        };
        loop {
            let next = match self.lookahead.take() {
                Some(f) => Some(f),
                None => self.pull()?,
            };
            match next {
                Some(f) if f.hns <= target => best = Some(f),
                Some(f) => {
                    self.lookahead = Some(f);
                    break;
                }
                None => break,
            }
        }
        let chosen = match (best, self.lookahead.take()) {
            (Some(b), la) => {
                self.lookahead = la;
                b
            }
            (None, Some(la)) => la,
            (None, None) => return Err(EditError::new("视频中没有可解码的帧")),
        };
        let frame = self.convert(&chosen)?;
        self.held = Some(chosen);
        Ok(Arc::new(frame))
    }

    /// 定位到不晚于目标的关键帧并清空缓存。
    fn seek_to(&mut self, target: i64) -> Result<(), EditError> {
        let abs = (self.start + target.min(self.length - 1).max(0)).min(self.presentation - 1);
        let position = PROPVARIANT::from(abs.max(0));
        // SAFETY: position 在调用期间有效。
        unsafe {
            self.reader
                .SetCurrentPosition(&GUID::zeroed(), &position)
                .map_err(|e| mf_err("seek 失败", e))?;
        }
        self.held = None;
        self.lookahead = None;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 灰色（U=V=128）在限幅与全幅下的转换：黑白电平正确，三通道相等。
    #[test]
    fn gray_levels() {
        let y = [16u8, 235, 128, 0];
        let uv = [128u8, 128, 128, 128];
        for bt709 in [true, false] {
            // SAFETY: 4 个亮度、1 行色差（4 字节）足够 4x1 的图。
            let out = unsafe {
                nv12_to_bgr(y.as_ptr(), uv.as_ptr(), 4, (4, 1), coeffs_for(bt709, false))
            };
            for (i, want) in [0i32, 255, 130].iter().enumerate() {
                assert!(
                    (i32::from(out[i * 3]) - want).abs() <= 1,
                    "bt709={bt709} px{i}: {} vs {want}",
                    out[i * 3]
                );
                assert_eq!(out[i * 3], out[i * 3 + 1]);
                assert_eq!(out[i * 3 + 1], out[i * 3 + 2]);
            }
        }
        // 全幅下亮度原样输出
        // SAFETY: 同上。
        let out =
            unsafe { nv12_to_bgr(y.as_ptr(), uv.as_ptr(), 4, (4, 1), coeffs_for(true, true)) };
        assert_eq!([out[0], out[3], out[6], out[9]], [16, 235, 128, 0]);
    }

    /// 红色（BT.709 限幅 Y=63, U=102, V=240）转出来红通道最大，BGR 顺序正确。
    #[test]
    fn channel_order_is_bgr() {
        let y = [63u8, 63];
        let uv = [102u8, 240];
        // SAFETY: 2 个亮度、1 行色差（2 字节）足够 2x1 的图。
        let out =
            unsafe { nv12_to_bgr(y.as_ptr(), uv.as_ptr(), 2, (2, 1), coeffs_for(true, false)) };
        assert!(out[2] > 240 && out[0] < 20 && out[1] < 20, "{out:?}");
    }
}
