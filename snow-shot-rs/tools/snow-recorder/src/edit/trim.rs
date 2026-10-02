//! 关键帧裁剪：包级拷贝，不解码不重编码（视频、音频都原样直通）。
//!
//! 对齐规则：起点落在"不晚于起点的最后一个关键帧"，终点落在"不早于终点的第一个关键帧"
//! （没有则到文件末尾），所以输出一定完整覆盖请求区间，只会多出不超过一个 GOP 的内容。

use std::path::Path;

use ffmpeg_next as ffmpeg;
use ffmpeg_next::{Rational, format, media};
use snow_recorder_protocol::{scratch_dir, scratch_file};

use super::source::VideoSource;
use super::transcode::rescale;
use super::{EditError, TaskCtl};

/// 进度阶段名。
const STAGE_TRIM: &str = "trim";
/// 微秒时间基。
const MICRO_TB: Rational = Rational(1, 1_000_000);

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
    let pid = std::process::id();
    let dir = scratch_dir(params.output, pid);
    std::fs::create_dir_all(&dir).map_err(|e| EditError::new(format!("创建中间目录失败: {e}")))?;
    let tmp = scratch_file(params.output, pid);
    let result = trim_to(params, ctl, &tmp).and_then(|n| {
        std::fs::rename(&tmp, params.output)
            .map_err(|e| EditError::new(format!("移动输出文件失败: {e}")))?;
        Ok(n)
    });
    let _ = std::fs::remove_dir_all(&dir);
    result
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

/// 裁剪主体：写到中间文件。
fn trim_to(params: &TrimParams<'_>, ctl: &TaskCtl, tmp: &Path) -> Result<u64, EditError> {
    let mut probe = VideoSource::open(params.input)?;
    let (info, keys) = probe.scan()?;
    let video_tb = probe.time_base();
    let start_pts = probe.start_pts();
    let span = align_span(
        &keys,
        probe.ms_to_pts(params.start_ms),
        probe.ms_to_pts(params.end_ms),
    )?;
    drop(probe);

    let mut input = format::input(params.input)
        .map_err(|e| EditError::new(format!("无法打开输入视频: {e}")))?;
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
    let origin_us = rescale(start_pts + span.start, video_tb, MICRO_TB);
    let end_us = span.end.map(|e| rescale(start_pts + e, video_tb, MICRO_TB));
    let (mut done, mut written) = (0u64, 0u64);
    let mut last_video_dur = 0i64;
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
        let keep = ts_us >= origin_us && end_us.is_none_or(|e| ts_us < e);
        if m.video {
            done += 1;
            // 末包时长常为 0（如 B 帧流），MP4 编辑列表会据此裁掉末帧，沿用上一个时长补上
            if pkt.duration() > 0 {
                last_video_dur = pkt.duration();
            } else {
                pkt.set_duration(last_video_dur);
            }
        }
        if keep {
            let shift = rescale(origin_us, MICRO_TB, m.in_tb);
            pkt.set_pts(pkt.pts().map(|p| p - shift));
            pkt.set_dts(pkt.dts().map(|p| p - shift));
            pkt.rescale_ts(m.in_tb, m.out_tb);
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
    use snow_recorder_protocol::ProbeInfo;
    use std::sync::Arc;
    use std::sync::atomic::AtomicBool;

    /// 静默的任务控制。
    fn ctl(cancelled: bool) -> TaskCtl {
        TaskCtl::new(Arc::new(AtomicBool::new(cancelled)), Box::new(|_| {}))
    }

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
        let leftovers = std::fs::read_dir(&dir)
            .unwrap()
            .flatten()
            .filter(|e| {
                e.file_name()
                    .to_string_lossy()
                    .starts_with(".snow-recording-")
            })
            .count();
        assert_eq!(leftovers, 0);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
