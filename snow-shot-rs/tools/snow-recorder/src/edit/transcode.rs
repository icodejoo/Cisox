//! 重编码类操作：降 fps 与缩放（共用一条 libx264 流水线），音频包直通拷贝。
//!
//! 解码帧保持 YUV，需要缩放或换格式时用 swscale 一步转成 YUV420P 再送编码器；
//! 输出只做 H.264，先写进中间文件（沿用 `.snow-recording-<pid>` 规则），成功后原子改名。

use std::path::Path;

use ffmpeg_next as ffmpeg;
use ffmpeg_next::format::Pixel;
use ffmpeg_next::software::scaling;
use ffmpeg_next::{Dictionary, Rational, codec, encoder, format, frame, media, picture};
use snow_recorder_protocol::{EditOp, ProbeInfo, scratch_dir, scratch_file};
use snow_screen_recorder::{ExportFormat, scaled_output_dimensions};

use super::source::{Decoded, VideoSource};
use super::{EditError, TaskCtl};

/// 进度阶段名。
const STAGE_TRANSCODE: &str = "transcode";
/// x264 预设：偏向速度（性能优先）。
const X264_PRESET: &str = "veryfast";
/// x264 恒定质量参数。
const X264_CRF: &str = "20";
/// 关键帧间隔（秒）。
const GOP_SECONDS: u32 = 2;
/// 微秒时间基。
const MICRO_TB: Rational = Rational(1, 1_000_000);
/// 输出视频流下标。
const OUT_VIDEO: usize = 0;

/// 重编码参数。
pub struct TranscodeParams<'a> {
    /// 输入视频。
    pub input: &'a Path,
    /// 输出文件（.mp4）。
    pub output: &'a Path,
    /// 操作，只接受 `ReduceFps` 与 `Scale`。
    pub op: EditOp,
}

/// 输出规格：尺寸与（可选的）目标帧率。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Target {
    /// 输出宽（偶数）。
    pub width: u32,
    /// 输出高（偶数）。
    pub height: u32,
    /// 目标帧率；`None` 表示保持源时间戳。
    pub fps: Option<u32>,
}

/// 由操作和源信息算出输出规格。
///
/// # 参数
/// - `op`：`ReduceFps` 或 `Scale`。
/// - `info`：源视频探测信息。
///
/// # 返回
/// 输出规格；降 fps 目标不低于源帧率、目标为 0 时返回错误。
pub fn plan_target(op: EditOp, info: &ProbeInfo) -> Result<Target, EditError> {
    let even = scaled_output_dimensions(info.width, info.height, None, None, ExportFormat::Mp4);
    match op {
        EditOp::ReduceFps { target_fps } => {
            if target_fps == 0 {
                return Err(EditError::new("目标帧率必须大于 0"));
            }
            if u64::from(target_fps) * 1000 >= u64::from(info.fps_milli) {
                return Err(EditError::new(format!(
                    "目标帧率 {target_fps} 不低于源帧率 {:.2}，无需降帧",
                    f64::from(info.fps_milli) / 1000.0
                )));
            }
            Ok(Target {
                width: even.0,
                height: even.1,
                fps: Some(target_fps),
            })
        }
        EditOp::Scale { width, height } => {
            if width == 0 || height == 0 {
                return Err(EditError::new("目标尺寸必须大于 0"));
            }
            let (width, height) = scaled_output_dimensions(
                info.width,
                info.height,
                Some(width),
                Some(height),
                ExportFormat::Mp4,
            );
            Ok(Target {
                width,
                height,
                fps: None,
            })
        }
        _ => Err(EditError::new("该操作不是重编码操作")),
    }
}

/// 降 fps 的帧选择器：按 PTS 落入的目标网格槽位选帧，每个槽位只留第一帧，不插值。
pub struct FpsGrid {
    /// 目标帧率。
    fps: u32,
    /// 源 PTS 的时间基。
    tb: Rational,
    /// 已输出的最大槽位。
    last: Option<i64>,
}

impl FpsGrid {
    /// 创建选择器。
    ///
    /// # 参数
    /// - `fps`：目标帧率。
    /// - `tb`：源 PTS 的时间基。
    pub fn new(fps: u32, tb: Rational) -> Self {
        Self {
            fps,
            tb,
            last: None,
        }
    }

    /// 判断一帧是否保留。
    ///
    /// # 参数
    /// - `pts`：已扣除流起点的 PTS。
    ///
    /// # 返回
    /// 保留时返回其输出槽位号（即以 `1/fps` 为时间基的输出 PTS）。
    pub fn pick(&mut self, pts: i64) -> Option<i64> {
        let num = i128::from(self.tb.numerator()) * i128::from(self.fps);
        let den = i128::from(self.tb.denominator()).max(1);
        // 槽位 = round(pts * tb * fps)
        let slot = ((i128::from(pts) * num * 2 + den).div_euclid(den * 2)) as i64;
        if self.last.is_some_and(|l| slot <= l) {
            return None;
        }
        self.last = Some(slot);
        Some(slot)
    }
}

/// 已打开的 H.264 编码器与缩放器。
struct Pipeline {
    /// 编码器。
    enc: encoder::video::Encoder,
    /// 编码器时间基。
    enc_tb: Rational,
    /// 缩放器；源格式与尺寸都已满足时为 `None`。
    scaler: Option<scaling::Context>,
    /// 缩放输出帧（复用）。
    scaled: Option<frame::Video>,
    /// 最近一帧的时长（编码器时间基），用来补全时长为 0 的包。
    last_duration: i64,
}

/// 读取帧时长（帧时间基单位，未知为 0）。
fn frame_duration(f: &frame::Video) -> i64 {
    // SAFETY: f 持有有效的 AVFrame，只读 duration 字段。
    unsafe { (*f.as_ptr()).duration }
}

/// 写入帧时长（编码器时间基单位）。
fn set_frame_duration(f: &mut frame::Video, duration: i64) {
    // SAFETY: f 持有有效的 AVFrame，独占可写。
    unsafe { (*f.as_mut_ptr()).duration = duration }
}

/// 创建 libx264 编码器与缩放器（首帧到达后才知道源格式与色彩标注）。
fn open_pipeline(
    first: &frame::Video,
    target: Target,
    enc_tb: Rational,
    frame_rate: Rational,
    global_header: bool,
) -> Result<Pipeline, EditError> {
    let mut enc = codec::context::Context::new_with_codec(h264_codec()?)
        .encoder()
        .video()
        .map_err(|e| EditError::new(format!("创建编码器失败: {e}")))?;
    enc.set_width(target.width);
    enc.set_height(target.height);
    enc.set_format(Pixel::YUV420P);
    enc.set_time_base(enc_tb);
    enc.set_frame_rate(Some(frame_rate));
    let out_fps = (frame_rate.numerator() / frame_rate.denominator().max(1)).max(1) as u32;
    enc.set_gop(out_fps * GOP_SECONDS);
    // SAFETY: 编码器尚未打开、独占持有；只写色彩标注字段。
    unsafe {
        let p = enc.as_mut_ptr();
        (*p).color_range = first.color_range().into();
        (*p).colorspace = first.color_space().into();
        (*p).color_primaries = first.color_primaries().into();
        (*p).color_trc = first.color_transfer_characteristic().into();
    }
    if global_header {
        enc.set_flags(codec::Flags::GLOBAL_HEADER);
    }
    let mut opts = Dictionary::new();
    opts.set("preset", X264_PRESET);
    opts.set("crf", X264_CRF);
    let enc = enc
        .open_with(opts)
        .map_err(|e| EditError::new(format!("打开 H.264 编码器失败: {e}")))?;
    let need_scale = first.format() != Pixel::YUV420P
        || first.width() != target.width
        || first.height() != target.height;
    let (scaler, scaled) = if need_scale {
        let ctx = scaling::Context::get(
            first.format(),
            first.width(),
            first.height(),
            Pixel::YUV420P,
            target.width,
            target.height,
            scaling::Flags::BICUBIC,
        )
        .map_err(|e| EditError::new(format!("创建缩放器失败: {e}")))?;
        let out = frame::Video::new(Pixel::YUV420P, target.width, target.height);
        (Some(ctx), Some(out))
    } else {
        (None, None)
    };
    Ok(Pipeline {
        enc,
        enc_tb,
        scaler,
        scaled,
        last_duration: 0,
    })
}

impl Pipeline {
    /// 送一帧给编码器（必要时先缩放）。
    fn send(&mut self, d: Decoded, pts: i64, duration: Option<i64>) -> Result<(), EditError> {
        let err = |e: ffmpeg::Error| EditError::new(format!("编码失败: {e}"));
        // 帧时长必须带上：缺了的话末包时长为 0，MP4 的编辑列表会把最后一帧裁掉
        let duration = duration.unwrap_or_else(|| frame_duration(&d.frame));
        if duration > 0 {
            self.last_duration = duration;
        }
        match (&mut self.scaler, &mut self.scaled) {
            (Some(sc), Some(out)) => {
                sc.run(&d.frame, out)
                    .map_err(|e| EditError::new(format!("缩放失败: {e}")))?;
                out.set_pts(Some(pts));
                set_frame_duration(out, duration);
                self.enc.send_frame(out).map_err(err)
            }
            _ => {
                let mut f = d.frame;
                f.set_pts(Some(pts));
                // 清掉解码帧的 I/P/B 标记，避免强制编码器插关键帧
                f.set_kind(picture::Type::None);
                set_frame_duration(&mut f, duration);
                self.enc.send_frame(&f).map_err(err)
            }
        }
    }

    /// 取出所有已就绪的包写入容器。
    fn drain(
        &mut self,
        out: &mut format::context::Output,
        out_tb: Rational,
    ) -> Result<(), EditError> {
        let mut pkt = ffmpeg::Packet::empty();
        while self.enc.receive_packet(&mut pkt).is_ok() {
            // libx264 给出的包时长可能是 0，末包为 0 时 MP4 编辑列表会裁掉最后一帧
            if pkt.duration() == 0 {
                pkt.set_duration(self.last_duration);
            }
            pkt.set_stream(OUT_VIDEO);
            pkt.rescale_ts(self.enc_tb, out_tb);
            pkt.write_interleaved(out)
                .map_err(|e| EditError::new(format!("写入视频包失败: {e}")))?;
        }
        Ok(())
    }
}

/// 音频直通的流映射。
#[derive(Clone, Copy)]
struct AudioMap {
    /// 输入时间基。
    in_tb: Rational,
    /// 输出时间基。
    out_tb: Rational,
    /// 输出流下标。
    out_index: usize,
    /// 需要平移的量（输入时间基单位，对齐视频流起点）。
    shift: i64,
}

/// 把已收集的旁路（音频）包原样写出（只平移、换时间基，不重编码）。
fn flush_audio(
    src: &mut VideoSource,
    map: Option<AudioMap>,
    out: &mut format::context::Output,
) -> Result<(), EditError> {
    while let Some(mut pkt) = src.take_side() {
        let Some(m) = map else { continue };
        pkt.set_pts(pkt.pts().map(|p| p - m.shift));
        pkt.set_dts(pkt.dts().map(|p| p - m.shift));
        pkt.rescale_ts(m.in_tb, m.out_tb);
        pkt.set_stream(m.out_index);
        pkt.set_position(-1);
        pkt.write_interleaved(out)
            .map_err(|e| EditError::new(format!("写入音频包失败: {e}")))?;
    }
    Ok(())
}

/// 取 libx264 编码器。
fn h264_codec() -> Result<ffmpeg::Codec, EditError> {
    encoder::find_by_name("libx264").ok_or_else(|| EditError::new("FFmpeg 缺少 libx264 编码器"))
}

/// 把时间戳从一个时间基换到另一个（四舍五入）。
pub fn rescale(value: i64, from: Rational, to: Rational) -> i64 {
    // SAFETY: 纯数值换算。
    unsafe { ffmpeg::ffi::av_rescale_q(value, from.into(), to.into()) }
}

/// 执行降 fps / 缩放。
///
/// # 参数
/// - `params`：输入、输出与操作。
/// - `ctl`：进度与取消控制。
///
/// # 返回
/// 输出视频帧数；取消返回 `cancelled`。失败或取消时不留下中间文件。
pub fn run(params: &TranscodeParams<'_>, ctl: &TaskCtl) -> Result<u64, EditError> {
    let pid = std::process::id();
    let dir = scratch_dir(params.output, pid);
    std::fs::create_dir_all(&dir).map_err(|e| EditError::new(format!("创建中间目录失败: {e}")))?;
    let tmp = scratch_file(params.output, pid);
    let result = transcode_to(params, ctl, &tmp).and_then(|n| {
        std::fs::rename(&tmp, params.output)
            .map_err(|e| EditError::new(format!("移动输出文件失败: {e}")))?;
        Ok(n)
    });
    let _ = std::fs::remove_dir_all(&dir);
    result
}

/// 转码主体：写到中间文件。
fn transcode_to(params: &TranscodeParams<'_>, ctl: &TaskCtl, tmp: &Path) -> Result<u64, EditError> {
    let mut src = VideoSource::open(params.input)?;
    let (info, _) = src.scan()?;
    let target = plan_target(params.op, &info)?;
    let src_tb = src.time_base();
    let audio = src
        .input()
        .streams()
        .best(media::Type::Audio)
        .map(|s| (s.index(), s.time_base()));
    if let Some((idx, _)) = audio {
        src.tap_stream(idx);
    }
    let (enc_tb, frame_rate) = match target.fps {
        Some(f) => (Rational(1, f as i32), Rational(f as i32, 1)),
        None => (src_tb, Rational(info.fps_milli as i32, 1000)),
    };
    let mut grid = target.fps.map(|f| FpsGrid::new(f, src_tb));
    // 首帧到达后才知道源格式与色彩标注，先取它再开编码器与输出流
    let first = src
        .pull()?
        .ok_or_else(|| EditError::new("视频中没有可解码的帧"))?;
    let mut out = format::output_as(tmp, "mp4")
        .map_err(|e| EditError::new(format!("创建输出文件失败: {e}")))?;
    let global_header = out.format().flags().contains(format::Flags::GLOBAL_HEADER);
    let mut pipe = open_pipeline(&first.frame, target, enc_tb, frame_rate, global_header)?;
    {
        let mut vs = out
            .add_stream(h264_codec()?)
            .map_err(|e| EditError::new(format!("创建视频流失败: {e}")))?;
        vs.set_parameters(&pipe.enc);
    }
    let audio_in = match audio {
        Some((idx, tb)) => {
            let ist = src
                .input()
                .stream(idx)
                .ok_or_else(|| EditError::new("音频流丢失"))?;
            let mut ost = out
                .add_stream(None::<ffmpeg::Codec>)
                .map_err(|e| EditError::new(format!("创建音频流失败: {e}")))?;
            ost.set_parameters(ist.parameters());
            // SAFETY: 流刚创建、独占持有；清空 codec_tag 让 MP4 复用器自行选择。
            unsafe {
                (*(*ost.as_mut_ptr()).codecpar).codec_tag = 0;
            }
            Some((ost.index(), tb))
        }
        None => None,
    };
    out.write_header()
        .map_err(|e| EditError::new(format!("写文件头失败: {e}")))?;
    let out_video_tb = out
        .stream(OUT_VIDEO)
        .map(|s| s.time_base())
        .ok_or_else(|| EditError::new("输出视频流丢失"))?;
    let audio_map = match audio_in {
        Some((out_index, in_tb)) => {
            let out_tb = out
                .stream(out_index)
                .map(|s| s.time_base())
                .ok_or_else(|| EditError::new("输出音频流丢失"))?;
            let start_us = rescale(src.start_pts(), src_tb, MICRO_TB);
            Some(AudioMap {
                in_tb,
                out_tb,
                out_index,
                shift: rescale(start_us, MICRO_TB, in_tb),
            })
        }
        None => None,
    };
    let total = info.frames;
    let (mut consumed, mut written) = (0u64, 0u64);
    let mut next = Some(first);
    while let Some(d) = next {
        if ctl.is_cancelled() {
            return Err(EditError::cancelled());
        }
        consumed += 1;
        let pts = match grid.as_mut() {
            Some(g) => g.pick(d.pts),
            None => Some(d.pts),
        };
        if let Some(p) = pts {
            // 降 fps 时每帧占一个网格槽位；缩放沿用源帧时长
            pipe.send(d, p, grid.as_ref().map(|_| 1))?;
            written += 1;
            pipe.drain(&mut out, out_video_tb)?;
        }
        flush_audio(&mut src, audio_map, &mut out)?;
        ctl.report(
            consumed.min(total.saturating_sub(1)),
            total,
            STAGE_TRANSCODE,
        );
        next = src.pull()?;
    }
    flush_audio(&mut src, audio_map, &mut out)?;
    pipe.enc
        .send_eof()
        .map_err(|e| EditError::new(format!("结束编码失败: {e}")))?;
    pipe.drain(&mut out, out_video_tb)?;
    out.write_trailer()
        .map_err(|e| EditError::new(format!("写文件尾失败: {e}")))?;
    ctl.report(total, total, STAGE_TRANSCODE);
    Ok(written)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::edit::testclip::{self, Clip};
    use std::sync::Arc;
    use std::sync::atomic::AtomicBool;

    /// 静默的任务控制。
    fn ctl(cancelled: bool) -> TaskCtl {
        TaskCtl::new(Arc::new(AtomicBool::new(cancelled)), Box::new(|_| {}))
    }

    /// 探测输出文件。
    fn probe(path: &Path) -> ProbeInfo {
        VideoSource::open(path).unwrap().scan().unwrap().0
    }

    /// 生成样片，返回（目录, 路径）。
    fn make(name: &str, clip: &Clip) -> (std::path::PathBuf, std::path::PathBuf) {
        let dir = testclip::temp_dir(name);
        let path = dir.join("in.mp4");
        testclip::make_clip(&path, clip).unwrap();
        (dir, path)
    }

    /// 目录里没有残留的中间目录。
    fn no_scratch(dir: &Path) -> bool {
        std::fs::read_dir(dir).unwrap().flatten().all(|e| {
            !e.file_name()
                .to_string_lossy()
                .starts_with(".snow-recording-")
        })
    }

    /// 构造重编码参数。
    fn params<'a>(input: &'a Path, output: &'a Path, op: EditOp) -> TranscodeParams<'a> {
        TranscodeParams { input, output, op }
    }

    /// 槽位选择：25fps 降到 10fps，每个槽位只留第一帧，槽位严格递增。
    #[test]
    fn fps_grid_picks_first_frame_per_slot() {
        let mut g = FpsGrid::new(10, Rational(1, 25));
        let slots: Vec<Option<i64>> = (0..8).map(|k| g.pick(k)).collect();
        assert_eq!(
            slots,
            vec![Some(0), None, Some(1), None, Some(2), None, None, Some(3)]
        );
    }

    /// 规格计算：偶数对齐、不放大、按比例缩进目标框；非法参数报错。
    #[test]
    fn plan_target_rules() {
        let info = ProbeInfo {
            width: 64,
            height: 48,
            duration_ms: 1000,
            fps_milli: 25_000,
            frames: 25,
            keyframes: 1,
        };
        let scale = |w, h| {
            plan_target(
                EditOp::Scale {
                    width: w,
                    height: h,
                },
                &info,
            )
        };
        let t = scale(50, 50).unwrap();
        assert_eq!((t.width, t.height, t.fps), (50, 36, None));
        let t = scale(640, 480).unwrap();
        assert_eq!((t.width, t.height), (64, 48));
        assert!(scale(0, 10).is_err());
        assert!(plan_target(EditOp::ReduceFps { target_fps: 25 }, &info).is_err());
        assert!(plan_target(EditOp::ReduceFps { target_fps: 0 }, &info).is_err());
        let t = plan_target(EditOp::ReduceFps { target_fps: 10 }, &info).unwrap();
        assert_eq!((t.width, t.height, t.fps), (64, 48, Some(10)));
    }

    /// 降 fps：帧数、分辨率、时长符合网格；音频包原样保留；首帧内容正确。
    #[test]
    fn reduce_fps_end_to_end() {
        for bframes in [0usize, 2] {
            let clip = Clip {
                audio: true,
                bframes,
                ..Clip::default()
            };
            let (dir, input) = make("transcode-fps", &clip);
            let output = dir.join("out.mp4");
            let op = EditOp::ReduceFps { target_fps: 10 };
            let n = run(&params(&input, &output, op), &ctl(false)).unwrap();
            let info = probe(&output);
            // 72 帧 25fps -> 槽位 0..=28
            assert_eq!(n, 29, "bframes={bframes}");
            assert_eq!(info.frames, 29);
            assert_eq!((info.width, info.height), (clip.width, clip.height));
            assert!(
                (2850..=2950).contains(&info.duration_ms),
                "时长 {}",
                info.duration_ms
            );
            assert!(
                (9_500..=10_500).contains(&info.fps_milli),
                "fps {}",
                info.fps_milli
            );
            let a_in = testclip::audio_facts(&input).expect("样片应有音频");
            let a_out = testclip::audio_facts(&output).expect("输出应保留音频");
            assert_eq!(a_in.packets, a_out.packets);
            assert_eq!(a_in.rate, a_out.rate);
            assert!((a_in.duration_ms - a_out.duration_ms).abs() <= 1);
            let mut out = VideoSource::open(&output).unwrap();
            let first = out.frame_at_ms(0).unwrap();
            assert_eq!(testclip::frame_index_of(&first.frame), 0);
            assert!(no_scratch(&dir));
            let _ = std::fs::remove_dir_all(&dir);
        }
    }

    /// 缩放：尺寸、帧数、时长不变（时间戳直通），音频保留，内容仍是同一时刻的灰阶。
    #[test]
    fn scale_end_to_end() {
        let clip = Clip {
            audio: true,
            ..Clip::default()
        };
        let (dir, input) = make("transcode-scale", &clip);
        let output = dir.join("out.mp4");
        let op = EditOp::Scale {
            width: 32,
            height: 24,
        };
        let n = run(&params(&input, &output, op), &ctl(false)).unwrap();
        let (src, out) = (probe(&input), probe(&output));
        assert_eq!(n, u64::from(clip.frames));
        assert_eq!((out.width, out.height), (32, 24));
        assert_eq!(out.frames, src.frames);
        assert!((out.duration_ms as i64 - src.duration_ms as i64).abs() <= 40);
        assert!(testclip::audio_facts(&output).is_some());
        let mut s = VideoSource::open(&output).unwrap();
        let d = s.frame_at_ms(1000).unwrap();
        assert_eq!((d.frame.width(), d.frame.height()), (32, 24));
        let idx = testclip::frame_index_of(&d.frame);
        assert!((24..=26).contains(&idx), "帧序号 {idx}");
        assert!(no_scratch(&dir));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 无音轨的输入输出也没有音轨；非法参数不产生输出。
    #[test]
    fn no_audio_stays_no_audio_and_bad_args_leave_nothing() {
        let (dir, input) = make("transcode-noaudio", &Clip::default());
        let output = dir.join("out.mp4");
        let op = EditOp::Scale {
            width: 32,
            height: 24,
        };
        run(&params(&input, &output, op), &ctl(false)).unwrap();
        assert!(testclip::audio_facts(&output).is_none());
        let bad = dir.join("bad.mp4");
        let op = EditOp::ReduceFps { target_fps: 60 };
        let err = run(&params(&input, &bad, op), &ctl(false)).unwrap_err();
        assert!(err.to_string().contains("无需降帧"));
        assert!(!bad.exists() && no_scratch(&dir));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 取消：返回 cancelled，不留输出与中间目录。
    #[test]
    fn cancel_leaves_nothing() {
        let clip = Clip {
            audio: true,
            ..Clip::default()
        };
        let (dir, input) = make("transcode-cancel", &clip);
        let output = dir.join("out.mp4");
        let op = EditOp::Scale {
            width: 32,
            height: 24,
        };
        let err = run(&params(&input, &output, op), &ctl(true)).unwrap_err();
        assert!(err.is_cancelled());
        assert!(!output.exists() && no_scratch(&dir));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
