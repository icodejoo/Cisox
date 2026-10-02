//! 视频源：探测与按时间戳精确 seek。
//!
//! 只认本软件录制的 H.264 MP4。`VideoSource::frame_at_ms` 先定位到目标之前的关键帧，
//! 再解码并丢弃到目标 PTS，返回"目标时刻正在显示"的那一帧（PTS 不超过目标的最后一帧）。
//! 目标离当前解码位置很近时直接向前解码，不重复 seek，抽帧按间隔取多帧时不会反复解整个 GOP。
//! 解出的帧保持解码器原生像素格式（通常 YUV420P），这里不做任何 RGBA 中转。

use std::collections::VecDeque;

use ffmpeg_next as ffmpeg;
use ffmpeg_next::packet::Ref;
use ffmpeg_next::{Rational, codec, format, frame, media};
use snow_recorder_protocol::ProbeInfo;

use super::EditError;

/// 向前解码而不重新 seek 的最大距离（毫秒）；超过它重新 seek 通常更省。
const FORWARD_LIMIT_MS: i64 = 2000;
/// FFmpeg 的 AV_TIME_BASE（微秒）。
const AV_TIME_BASE: i64 = 1_000_000;
/// "无时间戳"标记，同 `AV_NOPTS_VALUE`。
const NOPTS: i64 = i64::MIN;
/// 包标志：被容器裁掉（如落在 MP4 编辑列表之外），解码器不会输出它，同 `AV_PKT_FLAG_DISCARD`。
const PKT_FLAG_DISCARD: i32 = 0x0004;

/// 一帧解码结果（保持解码器原生像素格式）。
#[derive(Clone)]
pub struct Decoded {
    /// 帧数据（YUV 平面直通，不做 RGBA 转换）。
    pub frame: frame::Video,
    /// 以流时间基为单位、已扣除流起点的 PTS。
    pub pts: i64,
    /// 相对视频起点的毫秒时间。
    pub ms: i64,
}

/// 已打开的视频源：持有解复用器与解码器。
pub struct VideoSource {
    /// 输入容器。
    input: format::context::Input,
    /// 视频解码器。
    decoder: ffmpeg::decoder::Video,
    /// 视频流下标。
    stream_index: usize,
    /// 视频流时间基。
    time_base: Rational,
    /// 流起点（时间基单位）。
    start_pts: i64,
    /// 最近一次返回的帧，用于就近向前解码。
    held: Option<Decoded>,
    /// 已解出但尚未被消费的下一帧。
    lookahead: Option<Decoded>,
    /// 是否已向解码器送过 EOF。
    eof_sent: bool,
    /// 需要旁路收集的非视频流（音频直通用）。
    tap_stream: Option<usize>,
    /// 读视频包时顺带读到的旁路包，等调用方取走。
    side: VecDeque<ffmpeg::Packet>,
}

/// 把毫秒换成时间基刻度（四舍五入）。
///
/// # 参数
/// - `ms`：毫秒。
/// - `tb`：时间基。
fn ms_to_ticks(ms: i64, tb: Rational) -> i64 {
    let (num, den) = (i128::from(tb.numerator()), i128::from(tb.denominator()));
    if num == 0 {
        return 0;
    }
    let denom = 1000 * num;
    ((i128::from(ms) * den + denom / 2).div_euclid(denom)) as i64
}

/// 把时间基刻度换成毫秒（四舍五入）。
///
/// # 参数
/// - `ticks`：刻度数。
/// - `tb`：时间基。
fn ticks_to_ms(ticks: i64, tb: Rational) -> i64 {
    let (num, den) = (i128::from(tb.numerator()), i128::from(tb.denominator()));
    if den == 0 {
        return 0;
    }
    ((i128::from(ticks) * 1000 * num + den / 2).div_euclid(den)) as i64
}

/// 把刻度换成 AV_TIME_BASE（微秒）单位。
fn ticks_to_us(ticks: i64, tb: Rational) -> i64 {
    let (num, den) = (i128::from(tb.numerator()), i128::from(tb.denominator()));
    if den == 0 {
        return 0;
    }
    ((i128::from(ticks) * i128::from(AV_TIME_BASE) * num).div_euclid(den)) as i64
}

/// 包是否被容器标记为丢弃（不会出现在解码输出里，统计帧数时要排除）。
///
/// # 参数
/// - `packet`：刚从解复用器读到的包。
fn packet_is_discarded(packet: &ffmpeg::Packet) -> bool {
    // SAFETY: packet 持有有效的 AVPacket，只读标志位。
    unsafe { (*packet.as_ptr()).flags & PKT_FLAG_DISCARD != 0 }
}

impl VideoSource {
    /// 打开视频文件并建立解码器。
    ///
    /// # 参数
    /// - `path`：输入视频路径。
    ///
    /// # 返回
    /// 视频源；没有视频流、不是 H.264 或文件损坏时返回可读错误。
    ///
    /// # 示例
    /// ```ignore
    /// let mut src = VideoSource::open(Path::new("a.mp4"))?;
    /// let d = src.frame_at_ms(1500)?;
    /// ```
    pub fn open(path: &std::path::Path) -> Result<Self, EditError> {
        ffmpeg::init().map_err(|e| EditError::new(format!("初始化 FFmpeg 失败: {e}")))?;
        let input = format::input(path)
            .map_err(|e| EditError::new(format!("无法打开输入视频 {}: {e}", path.display())))?;
        let stream = input
            .streams()
            .best(media::Type::Video)
            .ok_or_else(|| EditError::new("输入文件没有视频流"))?;
        let stream_index = stream.index();
        let time_base = stream.time_base();
        let start = stream.start_time();
        let start_pts = if start == NOPTS { 0 } else { start };
        let context = codec::context::Context::from_parameters(stream.parameters())
            .map_err(|e| EditError::new(format!("读取视频参数失败: {e}")))?;
        let decoder = context
            .decoder()
            .video()
            .map_err(|e| EditError::new(format!("无法创建视频解码器（仅支持 H.264）: {e}")))?;
        Ok(Self {
            input,
            decoder,
            stream_index,
            time_base,
            start_pts,
            held: None,
            lookahead: None,
            eof_sent: false,
            tap_stream: None,
            side: VecDeque::new(),
        })
    }

    /// 输入容器（用于读取其它流的参数）。
    pub fn input(&self) -> &format::context::Input {
        &self.input
    }

    /// 视频流时间基。
    pub fn time_base(&self) -> Rational {
        self.time_base
    }

    /// 流起点（时间基单位），"相对 PTS"就是绝对 PTS 减去它。
    pub fn start_pts(&self) -> i64 {
        self.start_pts
    }

    /// 开启旁路收集：之后顺序解码时，该流的包会被留存，可用 [`Self::take_side`] 取走。
    ///
    /// # 参数
    /// - `stream`：要收集的流下标（如音频流）。
    pub fn tap_stream(&mut self, stream: usize) {
        self.tap_stream = Some(stream);
    }

    /// 取走一个旁路包（按文件顺序）；没有则返回 `None`。
    pub fn take_side(&mut self) -> Option<ffmpeg::Packet> {
        self.side.pop_front()
    }

    /// 解码器输出的像素格式（直通格式）。
    #[cfg(test)]
    pub fn pixel_format(&self) -> format::Pixel {
        self.decoder.format()
    }

    /// 把毫秒换算成（已含流起点的）绝对 PTS。
    ///
    /// # 参数
    /// - `ms`：相对视频起点的毫秒。
    pub fn ms_to_pts(&self, ms: u64) -> i64 {
        ms_to_ticks(i64::try_from(ms).unwrap_or(i64::MAX), self.time_base)
    }

    /// 把相对 PTS 换算成毫秒。
    ///
    /// # 参数
    /// - `pts`：已扣除流起点的 PTS。
    pub fn pts_to_ms(&self, pts: i64) -> i64 {
        ticks_to_ms(pts, self.time_base)
    }

    /// 扫描全部视频包（不解码），统计时长、帧数、关键帧，并返回关键帧时间表。
    ///
    /// # 返回
    /// `(探测信息, 关键帧相对 PTS 列表)`，关键帧列表按显示时间升序。
    pub fn scan(&mut self) -> Result<(ProbeInfo, Vec<i64>), EditError> {
        let mut frames = 0u64;
        let mut keys: Vec<i64> = Vec::new();
        let mut max_end: i64 = 0;
        let mut packet = ffmpeg::Packet::empty();
        // 单独的 seek 回到起点，扫描完再恢复，避免打乱后续解码状态
        self.rewind()?;
        loop {
            match packet.read(&mut self.input) {
                Ok(()) => {}
                Err(ffmpeg::Error::Eof) => break,
                Err(e) => return Err(EditError::new(format!("读取视频包失败: {e}"))),
            }
            if packet.stream() != self.stream_index || packet_is_discarded(&packet) {
                continue;
            }
            frames += 1;
            let pts = packet.pts().or(packet.dts()).unwrap_or(0) - self.start_pts;
            max_end = max_end.max(pts + packet.duration().max(0));
            if packet.is_key() {
                keys.push(pts);
            }
        }
        keys.sort_unstable();
        self.rewind()?;
        let stream = self
            .input
            .stream(self.stream_index)
            .ok_or_else(|| EditError::new("视频流丢失"))?;
        // 时长以视频包为准（容器时长可能被音轨拉长）；帧率由帧数和时长推出，比容器标注更准
        let packet_ms = ticks_to_ms(max_end, self.time_base);
        let duration_ms = if packet_ms > 0 {
            packet_ms
        } else {
            self.input.duration().max(0) / 1000
        };
        let avg = stream.avg_frame_rate();
        let fps_milli = if frames > 1 && packet_ms > 0 {
            ((frames as i128 * 1_000_000 + packet_ms as i128 / 2) / packet_ms as i128) as u32
        } else if avg.denominator() > 0 {
            (i64::from(avg.numerator()) * 1000 / i64::from(avg.denominator())).max(0) as u32
        } else {
            0
        };
        let info = ProbeInfo {
            width: self.decoder.width(),
            height: self.decoder.height(),
            duration_ms: duration_ms.max(0) as u64,
            fps_milli,
            frames,
            keyframes: keys.len() as u64,
        };
        Ok((info, keys))
    }

    /// 回到文件开头并清空解码状态（含旁路包）。
    pub fn rewind(&mut self) -> Result<(), EditError> {
        self.input
            .seek(0, ..)
            .map_err(|e| EditError::new(format!("回到文件开头失败: {e}")))?;
        self.reset_decoder();
        Ok(())
    }

    /// 清空解码器与缓存帧。
    fn reset_decoder(&mut self) {
        self.decoder.flush();
        self.held = None;
        self.lookahead = None;
        self.eof_sent = false;
        self.side.clear();
    }

    /// 取下一帧解码结果（顺序解码，不做 seek）；文件结束返回 `None`。
    pub fn pull(&mut self) -> Result<Option<Decoded>, EditError> {
        loop {
            let mut raw = frame::Video::empty();
            match self.decoder.receive_frame(&mut raw) {
                Ok(()) => {
                    let ts = raw.timestamp().unwrap_or(self.start_pts);
                    let pts = ts - self.start_pts;
                    let ms = ticks_to_ms(pts, self.time_base);
                    return Ok(Some(Decoded {
                        frame: raw,
                        pts,
                        ms,
                    }));
                }
                Err(ffmpeg::Error::Eof) => return Ok(None),
                Err(ffmpeg::Error::Other {
                    errno: ffmpeg::error::EAGAIN,
                }) => {}
                Err(e) => return Err(EditError::new(format!("解码失败: {e}"))),
            }
            if self.eof_sent {
                return Ok(None);
            }
            let mut packet = ffmpeg::Packet::empty();
            match packet.read(&mut self.input) {
                Ok(()) => {
                    if packet.stream() == self.stream_index {
                        self.decoder
                            .send_packet(&packet)
                            .map_err(|e| EditError::new(format!("送入解码器失败: {e}")))?;
                    } else if self.tap_stream == Some(packet.stream()) {
                        self.side.push_back(packet);
                    }
                }
                Err(ffmpeg::Error::Eof) => {
                    self.decoder
                        .send_eof()
                        .map_err(|e| EditError::new(format!("结束解码失败: {e}")))?;
                    self.eof_sent = true;
                }
                Err(e) => return Err(EditError::new(format!("读取视频包失败: {e}"))),
            }
        }
    }

    /// 按时间戳精确取帧：返回 PTS 不超过目标的最后一帧。
    ///
    /// 目标早于首帧时返回首帧；目标晚于末帧时返回末帧。
    ///
    /// # 参数
    /// - `ms`：相对视频起点的毫秒。
    ///
    /// # 返回
    /// 解码帧（YUV 直通）；文件里没有任何可解码帧时返回错误。
    ///
    /// # 示例
    /// ```ignore
    /// let d = src.frame_at_ms(1500)?;
    /// assert!(d.ms <= 1500);
    /// ```
    pub fn frame_at_ms(&mut self, ms: u64) -> Result<Decoded, EditError> {
        let target = self.ms_to_pts(ms);
        self.frame_at_pts(target)
    }

    /// 按相对 PTS（时间基刻度）精确取帧，语义同 [`Self::frame_at_ms`]。
    ///
    /// # 参数
    /// - `target`：已扣除流起点的 PTS。
    pub fn frame_at_pts(&mut self, target: i64) -> Result<Decoded, EditError> {
        let target = target.max(0);
        let limit = ms_to_ticks(FORWARD_LIMIT_MS, self.time_base);
        let reuse = self
            .held
            .as_ref()
            .is_some_and(|h| h.pts <= target && target - h.pts <= limit);
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
                Some(f) if f.pts <= target => best = Some(f),
                Some(f) => {
                    self.lookahead = Some(f);
                    break;
                }
                None => break,
            }
        }
        let out = match (best, &self.lookahead) {
            (Some(b), _) => b,
            (None, Some(la)) => la.clone(),
            (None, None) => return Err(EditError::new("视频中没有可解码的帧")),
        };
        self.held = Some(out.clone());
        Ok(out)
    }

    /// 定位到不晚于 `target` 的关键帧并清空解码状态。
    fn seek_to(&mut self, target: i64) -> Result<(), EditError> {
        let us = ticks_to_us(target + self.start_pts, self.time_base);
        self.input
            .seek(us, ..us.saturating_add(1))
            .map_err(|e| EditError::new(format!("seek 失败: {e}")))?;
        self.reset_decoder();
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::edit::testclip::{self, Clip};

    /// 毫秒与刻度互转在常见时间基下可逆。
    #[test]
    fn tick_conversion_round_trips() {
        for tb in [
            Rational::new(1, 15360),
            Rational::new(1, 1000),
            Rational::new(1, 90000),
        ] {
            for ms in [0, 1, 33, 1000, 123_456] {
                assert_eq!(
                    ticks_to_ms(ms_to_ticks(ms, tb), tb),
                    ms,
                    "tb={tb:?} ms={ms}"
                );
            }
        }
        assert_eq!(ms_to_ticks(1000, Rational::new(0, 1)), 0);
        assert_eq!(ticks_to_us(15360, Rational::new(1, 15360)), AV_TIME_BASE);
    }

    /// 生成并打开样片。
    fn open_clip(name: &str, clip: &Clip) -> (VideoSource, std::path::PathBuf) {
        let dir = testclip::temp_dir(name);
        let path = dir.join("clip.mp4");
        testclip::make_clip(&path, clip).expect("生成样片");
        (VideoSource::open(&path).expect("打开样片"), dir)
    }

    /// 毫秒 -> 期望帧序号（25fps：每 40ms 一帧，取目标时刻正在显示的帧）。
    fn expect_index(ms: u64, clip: &Clip) -> i32 {
        ((ms * u64::from(clip.fps)) / 1000).min(u64::from(clip.frames) - 1) as i32
    }

    /// 探测：尺寸、帧数、关键帧数、帧率、时长与样片参数一致。
    #[test]
    fn scan_reports_clip_facts() {
        let clip = Clip::default();
        let (mut src, dir) = open_clip("source-scan", &clip);
        let (info, keys) = src.scan().unwrap();
        assert_eq!((info.width, info.height), (clip.width, clip.height));
        assert_eq!(info.frames, u64::from(clip.frames));
        assert_eq!(info.keyframes, u64::from(clip.frames.div_ceil(clip.gop)));
        assert_eq!(keys.len() as u64, info.keyframes);
        assert_eq!(info.fps_milli, clip.fps * 1000);
        assert!(
            (2870..=2890).contains(&info.duration_ms),
            "时长 {}",
            info.duration_ms
        );
        // 扫描后解码状态仍可用
        let d = src.frame_at_ms(0).unwrap();
        assert_eq!(testclip::frame_index_of(&d.frame), 0);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 精确 seek：GOP 起点、GOP 中间、GOP 末尾、帧边界前后，以及乱序（向前向后）。
    #[test]
    fn seek_is_frame_accurate_in_any_order() {
        for bframes in [0usize, 2] {
            let clip = Clip {
                bframes,
                ..Clip::default()
            };
            let (mut src, dir) = open_clip(&format!("source-seek-{bframes}"), &clip);
            for ms in [
                0u64, 39, 40, 41, 959, 960, 961, 1500, 1919, 1920, 2000, 2879, 700, 20, 2800,
            ] {
                let d = src.frame_at_ms(ms).unwrap();
                let want = expect_index(ms, &clip);
                assert_eq!(
                    testclip::frame_index_of(&d.frame),
                    want,
                    "bframes={bframes} ms={ms}"
                );
                assert!(d.ms <= ms as i64, "返回帧晚于目标: {} > {ms}", d.ms);
            }
            let _ = std::fs::remove_dir_all(&dir);
        }
    }

    /// 接近末尾与越过末尾都返回最后一帧；密集顺序取帧结果与逐个 seek 一致。
    #[test]
    fn seek_clamps_and_sequential_matches() {
        let clip = Clip::default();
        let (mut src, dir) = open_clip("source-clamp", &clip);
        let last = src.frame_at_ms(60_000).unwrap();
        assert_eq!(
            testclip::frame_index_of(&last.frame),
            (clip.frames - 1) as i32
        );
        let mut seq = Vec::new();
        for ms in (0..2880u64).step_by(100) {
            seq.push(testclip::frame_index_of(
                &src.frame_at_ms(ms).unwrap().frame,
            ));
        }
        let want: Vec<i32> = (0..2880u64)
            .step_by(100)
            .map(|ms| expect_index(ms, &clip))
            .collect();
        assert_eq!(seq, want);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 容器裁掉末帧（MP4 编辑列表）时：扫描的帧数与解码实际输出一致，越界 seek 返回最后一个可解码帧。
    #[test]
    fn trimmed_tail_is_consistent() {
        let clip = Clip {
            editlist: true,
            ..Clip::default()
        };
        let (mut src, dir) = open_clip("source-trim", &clip);
        let (info, _) = src.scan().unwrap();
        let mut decoded = 0u64;
        let mut last = -1;
        while let Some(d) = src.pull().unwrap() {
            decoded += 1;
            last = testclip::frame_index_of(&d.frame);
        }
        assert_eq!(info.frames, decoded, "扫描帧数应等于解码输出帧数");
        src.rewind().unwrap();
        let tail = src.frame_at_ms(60_000).unwrap();
        assert_eq!(testclip::frame_index_of(&tail.frame), last);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 解码帧保持 YUV 平面格式（直通），没有被转成 RGBA。
    #[test]
    fn decoded_frames_stay_yuv() {
        let (mut src, dir) = open_clip("source-yuv", &Clip::default());
        let d = src.frame_at_ms(500).unwrap();
        assert_eq!(d.frame.format(), ffmpeg::format::Pixel::YUV420P);
        assert_eq!(src.pixel_format(), ffmpeg::format::Pixel::YUV420P);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
