//! 关键帧裁剪：包级拷贝，不解码不重编码（视频、音频都原样直通）。
//!
//! 对齐规则：起点落在"不晚于起点的最后一个关键帧"，终点落在"不早于终点的第一个关键帧"
//! （没有则到文件末尾），所以输出一定完整覆盖请求区间，只会多出不超过一个 GOP 的内容。
//!
//! 多段裁剪：每段各自对齐后，对齐结果重叠或相接的并成一段；各段的包按顺序写入同一个输出，
//! 后一段的时间戳整体前移到前一段结尾处，所以输出连续、无重叠、无需重编码。

use std::path::Path;

use ffmpeg_next as ffmpeg;
use ffmpeg_next::{Rational, format, media};

use snow_recorder_protocol::validate_segments;

use super::source::VideoSource;
use super::transcode::{MICRO_TB, rescale};
use super::{EditError, TaskCtl, run_via_scratch};

/// 进度阶段名。
const STAGE_TRIM: &str = "trim";

/// 裁剪参数。
pub struct TrimParams<'a> {
    /// 输入视频。
    pub input: &'a Path,
    /// 输出文件（.mp4）。
    pub output: &'a Path,
    /// 起点（毫秒）。
    pub start_ms: u64,
    /// 终点（毫秒）。
    pub end_ms: u64,
}

/// 对齐后的区间，单位为"相对视频起点的 PTS 刻度"。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Span {
    /// 起点（含），一定是关键帧。
    pub start: i64,
    /// 终点（不含）；`None` 表示到文件末尾。
    pub end: Option<i64>,
}

/// 把请求区间对齐到关键帧。
///
/// # 参数
/// - `keys`：关键帧相对 PTS，升序。
/// - `start`：请求起点（刻度）。
/// - `end`：请求终点（刻度）。
///
/// # 返回
/// 对齐后的区间；没有关键帧时返回错误。
pub fn align_span(keys: &[i64], start: i64, end: i64) -> Result<Span, EditError> {
    let first = *keys
        .first()
        .ok_or_else(|| EditError::new("视频中没有关键帧，无法裁剪"))?;
    let s = keys
        .iter()
        .rev()
        .find(|&&k| k <= start)
        .copied()
        .unwrap_or(first);
    let e = keys.iter().find(|&&k| k >= end).copied();
    Ok(Span { start: s, end: e })
}

/// 执行关键帧裁剪。
///
/// # 参数
/// - `params`：输入、输出与区间。
/// - `ctl`：进度与取消控制。
///
/// # 返回
/// 输出的视频帧数；取消返回 `cancelled`。失败或取消时不留下中间文件。
pub fn run(params: &TrimParams<'_>, ctl: &TaskCtl) -> Result<u64, EditError> {
    if params.end_ms <= params.start_ms {
        return Err(EditError::new("裁剪终点必须晚于起点"));
    }
    let ranges = [(params.start_ms, params.end_ms)];
    run_via_scratch(params.output, |tmp| {
        trim_to(params.input, &ranges, ctl, tmp)
    })
}

/// 执行多段关键帧裁剪并拼接。
///
/// # 参数
/// - `input`：输入视频。
/// - `output`：输出文件（.mp4）。
/// - `segments`：保留区间（毫秒，`[start, end)`），升序、不重叠、每段 start<end。
/// - `ctl`：进度与取消控制。
///
/// # 返回
/// 输出的视频帧数；区间非法返回错误，取消返回 `cancelled`。失败或取消时不留下中间文件。
pub fn run_segments(
    input: &Path,
    output: &Path,
    segments: &[(u64, u64)],
    ctl: &TaskCtl,
) -> Result<u64, EditError> {
    validate_segments(segments).map_err(|e| EditError::new(e.to_string()))?;
    run_via_scratch(output, |tmp| trim_to(input, segments, ctl, tmp))
}

/// 合并并对齐后的一段保留区间，时间为绝对微秒。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct KeepSpan {
    /// 输入里的起点（含）。
    origin_us: i64,
    /// 输入里的终点（不含）；`None` 到文件末尾。
    end_us: Option<i64>,
    /// 这一段在输出时间线上的起点（相对输出开头）。
    offset_us: i64,
}

/// 把各段对齐到关键帧，重叠或相接的并成一段，并算出每段在输出里的起点。
///
/// # 参数
/// - `keys`：关键帧相对 PTS，升序。
/// - `ranges_ticks`：各请求区间（刻度），已升序。
/// - `to_us`：把"相对 PTS 刻度"换算成绝对微秒。
fn plan_spans(
    keys: &[i64],
    ranges_ticks: &[(i64, i64)],
    to_us: impl Fn(i64) -> i64,
) -> Result<Vec<KeepSpan>, EditError> {
    let mut aligned: Vec<Span> = Vec::new();
    for &(s, e) in ranges_ticks {
        let span = align_span(keys, s, e)?;
        match aligned.last_mut() {
            Some(prev) if prev.end.is_none_or(|pe| span.start <= pe) => {
                prev.end = match (prev.end, span.end) {
                    (Some(a), Some(b)) => Some(a.max(b)),
                    _ => None,
                };
            }
            _ => aligned.push(span),
        }
    }
    let mut out = Vec::with_capacity(aligned.len());
    let mut offset_us = 0i64;
    for sp in aligned {
        let origin_us = to_us(sp.start);
        let end_us = sp.end.map(&to_us);
        out.push(KeepSpan {
            origin_us,
            end_us,
            offset_us,
        });
        if let Some(e) = end_us {
            offset_us += e - origin_us;
        }
    }
    Ok(out)
}

/// 输入流到输出流的映射。
struct Map {
    /// 输入时间基。
    in_tb: Rational,
    /// 输出时间基。
    out_tb: Rational,
    /// 是否视频流。
    video: bool,
}

/// 视频 DTS 补偿：B 帧流里后一段首个包的 DTS 可能不大于前一段末尾包，
/// 每进入新的一段就按需算出一个段内恒定的最小补偿量（只动 DTS，不动 PTS，补偿越小越不顶到 PTS）。
#[derive(Debug, Default)]
struct DtsFix {
    /// 上一个已写出视频包的 DTS（输出时间基）。
    last_dts: Option<i64>,
    /// 当前所在段下标。
    span: Option<usize>,
    /// 当前段的 DTS 补偿量。
    bump: i64,
}

impl DtsFix {
    /// 修正一个已换算到输出时间基的视频包。
    ///
    /// # 参数
    /// - `pkt`：待写出的视频包。
    /// - `span`：包所在的保留区间下标。
    fn apply(&mut self, pkt: &mut ffmpeg::Packet, span: usize) {
        let Some(dts) = pkt.dts() else {
            return;
        };
        if self.span != Some(span) {
            self.span = Some(span);
            self.bump = match self.last_dts {
                Some(last) if dts <= last => last + 1 - dts,
                _ => 0,
            };
        }
        let mut fixed = dts + self.bump;
        if let Some(pts) = pkt.pts() {
            fixed = fixed.min(pts);
        }
        pkt.set_dts(Some(fixed));
        self.last_dts = Some(fixed);
    }
}

/// 裁剪主体：按保留区间写到中间文件。
fn trim_to(
    input_path: &Path,
    ranges: &[(u64, u64)],
    ctl: &TaskCtl,
    tmp: &Path,
) -> Result<u64, EditError> {
    let mut probe = VideoSource::open(input_path)?;
    let (info, keys) = probe.scan()?;
    let video_tb = probe.time_base();
    let start_pts = probe.start_pts();
    let ranges_ticks: Vec<(i64, i64)> = ranges
        .iter()
        .map(|&(s, e)| (probe.ms_to_pts(s), probe.ms_to_pts(e)))
        .collect();
    drop(probe);
    let spans = plan_spans(&keys, &ranges_ticks, |t| {
        rescale(start_pts + t, video_tb, MICRO_TB)
    })?;

    let mut input =
        format::input(input_path).map_err(|e| EditError::new(format!("无法打开输入视频: {e}")))?;
    let video_idx = input
        .streams()
        .best(media::Type::Video)
        .map(|s| s.index())
        .ok_or_else(|| EditError::new("输入文件没有视频流"))?;
    let audio_idx = input.streams().best(media::Type::Audio).map(|s| s.index());
    let mut out = format::output_as(tmp, "mp4")
        .map_err(|e| EditError::new(format!("创建输出文件失败: {e}")))?;
    // 输入流下标 -> 输出流下标（视频在前）
    let mut index_map: Vec<Option<usize>> = vec![None; input.streams().count()];
    for idx in [Some(video_idx), audio_idx].into_iter().flatten() {
        let ist = input
            .stream(idx)
            .ok_or_else(|| EditError::new("输入流丢失"))?;
        let mut ost = out
            .add_stream(None::<ffmpeg::Codec>)
            .map_err(|e| EditError::new(format!("创建输出流失败: {e}")))?;
        ost.set_parameters(ist.parameters());
        // SAFETY: 流刚创建、独占持有；清空 codec_tag 让 MP4 复用器自行选择。
        unsafe {
            (*(*ost.as_mut_ptr()).codecpar).codec_tag = 0;
        }
        index_map[idx] = Some(ost.index());
    }
    out.write_header()
        .map_err(|e| EditError::new(format!("写文件头失败: {e}")))?;
    let mut maps: Vec<Option<Map>> = Vec::with_capacity(index_map.len());
    for (i, o) in index_map.iter().enumerate() {
        maps.push(match o {
            Some(oi) => {
                let in_tb = input.stream(i).map(|s| s.time_base());
                let out_tb = out.stream(*oi).map(|s| s.time_base());
                match (in_tb, out_tb) {
                    (Some(in_tb), Some(out_tb)) => Some(Map {
                        in_tb,
                        out_tb,
                        video: i == video_idx,
                    }),
                    _ => return Err(EditError::new("输出流丢失")),
                }
            }
            None => None,
        });
    }
    // 区间（绝对时间，微秒）：音频按时间截取，视频按 PTS 截取
    let (mut done, mut written) = (0u64, 0u64);
    let mut last_video_dur = 0i64;
    let mut dts_fix = DtsFix::default();
    let mut pkt = ffmpeg::Packet::empty();
    loop {
        match pkt.read(&mut input) {
            Ok(()) => {}
            Err(ffmpeg::Error::Eof) => break,
            Err(e) => return Err(EditError::new(format!("读取包失败: {e}"))),
        }
        if ctl.is_cancelled() {
            return Err(EditError::cancelled());
        }
        let in_stream = pkt.stream();
        let Some(Some(m)) = maps.get(in_stream) else {
            continue;
        };
        let ts = pkt.pts().or(pkt.dts()).unwrap_or(0);
        let ts_us = rescale(ts, m.in_tb, MICRO_TB);
        let hit_idx = spans
            .iter()
            .position(|sp| ts_us >= sp.origin_us && sp.end_us.is_none_or(|e| ts_us < e));
        if m.video {
            done += 1;
            // 末包时长常为 0（如 B 帧流），MP4 编辑列表会据此裁掉末帧，沿用上一个时长补上
            if pkt.duration() > 0 {
                last_video_dur = pkt.duration();
            } else {
                pkt.set_duration(last_video_dur);
            }
        }
        if let Some(idx) = hit_idx {
            let sp = &spans[idx];
            // 前移量：本段起点落到它在输出时间线上的位置
            let shift = rescale(sp.origin_us - sp.offset_us, MICRO_TB, m.in_tb);
            pkt.set_pts(pkt.pts().map(|p| p - shift));
            pkt.set_dts(pkt.dts().map(|p| p - shift));
            pkt.rescale_ts(m.in_tb, m.out_tb);
            if m.video {
                dts_fix.apply(&mut pkt, idx);
            }
            pkt.set_stream(index_map[in_stream].unwrap_or(0));
            pkt.set_position(-1);
            pkt.write_interleaved(&mut out)
                .map_err(|e| EditError::new(format!("写入包失败: {e}")))?;
            if m.video {
                written += 1;
            }
        }
        ctl.report(
            done.min(info.frames.saturating_sub(1)),
            info.frames,
            STAGE_TRIM,
        );
    }
    out.write_trailer()
        .map_err(|e| EditError::new(format!("写文件尾失败: {e}")))?;
    ctl.report(info.frames, info.frames, STAGE_TRIM);
    Ok(written)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::edit::testclip::{self, Clip};
    use crate::edit::testclip::{no_scratch, quiet_ctl as ctl};
    use snow_recorder_protocol::ProbeInfo;

    /// 探测文件。
    fn probe(path: &Path) -> ProbeInfo {
        VideoSource::open(path).unwrap().scan().unwrap().0
    }

    /// 构造裁剪参数。
    fn params<'a>(input: &'a Path, output: &'a Path, start_ms: u64, end_ms: u64) -> TrimParams<'a> {
        TrimParams {
            input,
            output,
            start_ms,
            end_ms,
        }
    }

    /// 对齐规则：起点向前取关键帧，终点向后取关键帧，超出末尾为 EOF。
    #[test]
    fn align_span_rules() {
        let keys = [0, 24, 48];
        let span = |start, end| Span { start, end };
        assert_eq!(align_span(&keys, 30, 40).unwrap(), span(24, Some(48)));
        assert_eq!(align_span(&keys, 24, 48).unwrap(), span(24, Some(48)));
        assert_eq!(align_span(&keys, 25, 49).unwrap(), span(24, None));
        assert_eq!(align_span(&keys, 0, 1).unwrap(), span(0, Some(24)));
        assert!(align_span(&[], 0, 1).is_err());
    }

    /// 裁剪：起点前移到关键帧、帧数与时长正确、内容未被重编码、音频被同步截取。
    #[test]
    fn trim_end_to_end() {
        for bframes in [0usize, 2] {
            let clip = Clip {
                audio: true,
                bframes,
                ..Clip::default()
            };
            let dir = testclip::temp_dir("trim-e2e");
            let input = dir.join("in.mp4");
            testclip::make_clip(&input, &clip).unwrap();
            // 关键帧在 0 / 960 / 1920 ms；1000..1900 对齐为 960..1920（帧 24..47）
            let output = dir.join("out.mp4");
            let n = run(&params(&input, &output, 1000, 1900), &ctl(false)).unwrap();
            let info = probe(&output);
            assert_eq!(n, 24, "bframes={bframes}");
            assert_eq!(info.frames, 24);
            assert_eq!((info.width, info.height), (clip.width, clip.height));
            assert_eq!(info.keyframes, 1);
            assert!(
                (950..=970).contains(&info.duration_ms),
                "时长 {}",
                info.duration_ms
            );
            let mut src = VideoSource::open(&output).unwrap();
            let first = src.frame_at_ms(0).unwrap();
            assert_eq!(testclip::frame_index_of(&first.frame), 24);
            let last = src.frame_at_ms(930).unwrap();
            // 有 B 帧时样片的 B 帧画质略低，亮度会偏几个台阶，放宽到 3 帧
            let tol = if bframes > 0 { 3 } else { 0 };
            let got = testclip::frame_index_of(&last.frame);
            assert!((got - 47).abs() <= tol, "末帧序号 {got}, bframes={bframes}");
            let audio = testclip::audio_facts(&output).expect("应保留音频");
            assert!(
                (audio.duration_ms - 960).abs() <= 60,
                "音频时长 {}",
                audio.duration_ms
            );
            let _ = std::fs::remove_dir_all(&dir);
        }
    }

    /// 终点超过最后一个关键帧时一直裁到文件末尾。
    #[test]
    fn trim_to_end_of_file() {
        let dir = testclip::temp_dir("trim-eof");
        let input = dir.join("in.mp4");
        testclip::make_clip(&input, &Clip::default()).unwrap();
        let output = dir.join("out.mp4");
        let n = run(&params(&input, &output, 1000, 2000), &ctl(false)).unwrap();
        assert_eq!(n, 48);
        assert_eq!(probe(&output).frames, 48);
        assert!(testclip::audio_facts(&output).is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 非法区间与取消都不留输出和中间目录。
    #[test]
    fn invalid_and_cancel_leave_nothing() {
        let dir = testclip::temp_dir("trim-bad");
        let input = dir.join("in.mp4");
        testclip::make_clip(&input, &Clip::default()).unwrap();
        let output = dir.join("out.mp4");
        assert!(run(&params(&input, &output, 500, 500), &ctl(false)).is_err());
        let err = run(&params(&input, &output, 0, 500), &ctl(true)).unwrap_err();
        assert!(err.is_cancelled());
        assert!(!output.exists());
        assert!(no_scratch(&dir));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 解出文件里所有帧：返回（样片帧序号, 毫秒）。
    fn decode_all(path: &Path) -> Vec<(i32, i64)> {
        let mut src = VideoSource::open(path).unwrap();
        let mut v = Vec::new();
        while let Some(d) = src.pull().unwrap() {
            v.push((testclip::frame_index_of(&d.frame), d.ms));
        }
        v
    }

    /// 多段样片：25fps、GOP 8（320ms）、72 帧。
    fn seg_clip(bframes: usize) -> Clip {
        Clip {
            gop: 8,
            audio: true,
            bframes,
            ..Clip::default()
        }
    }

    /// 多段对齐：重叠或相接的对齐结果并成一段，输出起点逐段累加。
    #[test]
    fn plan_spans_merges_and_offsets() {
        let keys = [0, 8, 16, 24, 32, 40];
        let id = |t: i64| t * 10;
        // (0,3)->[0,8)；(7,10)->[0,16) 与前一段重叠并入；(30,31)->[24,32)
        let spans = plan_spans(&keys, &[(0, 3), (7, 10), (30, 31)], id).unwrap();
        assert_eq!(spans.len(), 2);
        assert_eq!((spans[0].origin_us, spans[0].end_us), (0, Some(160)));
        assert_eq!(spans[0].offset_us, 0);
        assert_eq!((spans[1].origin_us, spans[1].end_us), (240, Some(320)));
        assert_eq!(spans[1].offset_us, 160);
        // 终点超过最后关键帧：该段到文件末尾
        let spans = plan_spans(&keys, &[(0, 3), (41, 50)], id).unwrap();
        assert_eq!(spans[1].end_us, None);
        assert_eq!(spans[1].offset_us, 80);
    }

    /// 三段拼接（无 B 帧）：帧序列、时长、PTS 均匀递增、无重复帧、可重新打开。
    #[test]
    fn segments_concat_three() {
        let dir = testclip::temp_dir("trim-segs3");
        let input = dir.join("in.mp4");
        testclip::make_clip(&input, &seg_clip(0)).unwrap();
        let output = dir.join("out.mp4");
        // 对齐后 [0,8)、[16,32)、[40,56) 帧，共 8+16+16
        let segs = [(0, 300), (700, 1000), (1800, 2000)];
        let n = run_segments(&input, &output, &segs, &ctl(false)).unwrap();
        assert_eq!(n, 40);
        let info = probe(&output);
        assert_eq!(info.frames, 40);
        assert!(
            (1580..=1620).contains(&info.duration_ms),
            "时长 {}",
            info.duration_ms
        );
        let got = decode_all(&output);
        let idx: Vec<i32> = got.iter().map(|g| g.0).collect();
        let want: Vec<i32> = (0..8).chain(16..32).chain(40..56).collect();
        assert_eq!(idx, want);
        // 每帧间隔都是 40ms（含拼接处），说明连续、无重叠
        for w in got.windows(2) {
            assert_eq!(w[1].1 - w[0].1, 40, "{w:?}");
        }
        let audio = testclip::audio_facts(&output).expect("应保留音频");
        assert!(
            (audio.duration_ms - 1600).abs() <= 120,
            "音频时长 {}",
            audio.duration_ms
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 两段拼接（有 B 帧）：帧数、时长、PTS 严格递增、无重复。
    #[test]
    fn segments_concat_with_bframes() {
        let dir = testclip::temp_dir("trim-segs-b");
        let input = dir.join("in.mp4");
        testclip::make_clip(&input, &seg_clip(2)).unwrap();
        let output = dir.join("out.mp4");
        let n = run_segments(&input, &output, &[(0, 300), (1800, 2000)], &ctl(false)).unwrap();
        assert_eq!(n, 24);
        let got = decode_all(&output);
        assert_eq!(got.len(), 24);
        for w in got.windows(2) {
            assert_eq!(w[1].1 - w[0].1, 40, "{w:?}");
        }
        let mut seen = std::collections::HashSet::new();
        assert!(got.iter().all(|g| seen.insert(g.1)), "存在重复时间戳");
        let audio = testclip::audio_facts(&output).expect("应保留音频");
        assert!(
            (audio.duration_ms - 960).abs() <= 120,
            "音频时长 {}",
            audio.duration_ms
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 单段拼接与关键帧裁剪结果等价（帧数、时长、帧序列一致）。
    #[test]
    fn single_segment_equals_trim_keyframe() {
        let dir = testclip::temp_dir("trim-segs1");
        let input = dir.join("in.mp4");
        testclip::make_clip(&input, &seg_clip(0)).unwrap();
        let (a, b) = (dir.join("a.mp4"), dir.join("b.mp4"));
        let na = run(&params(&input, &a, 700, 1500), &ctl(false)).unwrap();
        let nb = run_segments(&input, &b, &[(700, 1500)], &ctl(false)).unwrap();
        assert_eq!(na, nb);
        assert_eq!(probe(&a), probe(&b));
        assert_eq!(decode_all(&a), decode_all(&b));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 非法区间与取消不留输出。
    #[test]
    fn segments_invalid_and_cancel() {
        let dir = testclip::temp_dir("trim-segs-bad");
        let input = dir.join("in.mp4");
        testclip::make_clip(&input, &Clip::default()).unwrap();
        let output = dir.join("out.mp4");
        assert!(run_segments(&input, &output, &[], &ctl(false)).is_err());
        assert!(run_segments(&input, &output, &[(0, 500), (400, 900)], &ctl(false)).is_err());
        let err = run_segments(&input, &output, &[(0, 500)], &ctl(true)).unwrap_err();
        assert!(err.is_cancelled());
        assert!(!output.exists());
        assert!(no_scratch(&dir));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
