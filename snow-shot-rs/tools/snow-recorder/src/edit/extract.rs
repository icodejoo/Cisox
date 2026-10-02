//! 抽帧导出：单帧 / 按间隔 / 仅关键帧，输出 PNG、JPEG、无损 WebP。
//!
//! 解码在调用线程里顺序进行（用 `VideoSource` 的精确 seek），图片编码放到小线程池并行，
//! 解码帧以 YUV 形式直通给编码线程，不先转 RGBA。
//! 先写进中间目录（沿用录制的 `.snow-recording-<pid>` 规则），全部成功后再移入输出目录；
//! 取消或失败时清掉中间目录，用户目录里不会留下半截文件。

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::sync_channel;
use std::sync::{Arc, Mutex};

use ffmpeg_next::frame;
use snow_recorder_protocol::{ExtractMode, ImageFormat, ProbeInfo, scratch_dir};

use super::image::make_encoder;
use super::source::VideoSource;
use super::{EditError, TaskCtl};

/// 单次抽帧最多导出的张数，防止误填极小间隔写出海量文件。
pub const MAX_EXTRACT_FRAMES: usize = 100_000;
/// 环境变量：图片编码线程数（1..=16）。
const ENV_EXTRACT_THREADS: &str = "SNOW_RECORDER_EDIT_THREADS";
/// 编码线程数上限（默认值）。
const MAX_DEFAULT_THREADS: usize = 4;
/// 进度阶段名。
const STAGE_EXTRACT: &str = "extract";
/// 单帧模式允许超出时长的容差（毫秒），约一帧。
const SINGLE_TOLERANCE_MS: u64 = 100;

/// 计划中的一帧。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PlannedFrame {
    /// 目标毫秒（相对视频起点）。
    pub ms: u64,
    /// 已知精确 PTS（关键帧模式）；`None` 表示按毫秒定位。
    pub pts: Option<i64>,
}

/// 根据抽帧模式生成要导出的帧列表（纯计算，便于测试）。
///
/// # 参数
/// - `mode`：抽帧模式。
/// - `info`：视频探测信息。
/// - `key_pts`：关键帧的相对 PTS 升序列表。
/// - `pts_to_ms`：PTS 到毫秒的换算。
///
/// # 返回
/// 计划帧列表；单帧超出时长、结果为空或超过 [`MAX_EXTRACT_FRAMES`] 时返回错误。
pub fn plan_frames(
    mode: ExtractMode,
    info: &ProbeInfo,
    key_pts: &[i64],
    pts_to_ms: impl Fn(i64) -> i64,
) -> Result<Vec<PlannedFrame>, EditError> {
    let plan: Vec<PlannedFrame> = match mode {
        ExtractMode::Single { at_ms } => {
            if at_ms > info.duration_ms + SINGLE_TOLERANCE_MS {
                return Err(EditError::new(format!(
                    "目标时间 {at_ms}ms 超出视频时长 {}ms",
                    info.duration_ms
                )));
            }
            vec![PlannedFrame {
                ms: at_ms,
                pts: None,
            }]
        }
        ExtractMode::Interval { every_ms } => {
            let every = every_ms.max(1);
            let count = info.duration_ms.div_ceil(every).max(1);
            if count > MAX_EXTRACT_FRAMES as u64 {
                return Err(EditError::new(format!(
                    "按此间隔会导出 {count} 张，超过上限 {MAX_EXTRACT_FRAMES}"
                )));
            }
            (0..count)
                .map(|i| PlannedFrame {
                    ms: i * every,
                    pts: None,
                })
                .collect()
        }
        ExtractMode::Keyframes => {
            if key_pts.len() > MAX_EXTRACT_FRAMES {
                return Err(EditError::new(format!(
                    "关键帧共 {} 个，超过上限 {MAX_EXTRACT_FRAMES}",
                    key_pts.len()
                )));
            }
            key_pts
                .iter()
                .map(|p| PlannedFrame {
                    ms: pts_to_ms(*p).max(0) as u64,
                    pts: Some(*p),
                })
                .collect()
        }
    };
    if plan.is_empty() {
        return Err(EditError::new("没有可导出的帧"));
    }
    Ok(plan)
}

/// 输出文件名：`frame_<序号>_t<毫秒>ms.<扩展名>`。
///
/// # 参数
/// - `index`：从 0 起的序号。
/// - `ms`：该帧实际时间（毫秒）。
/// - `format`：图片格式。
pub fn frame_file_name(index: usize, ms: i64, format: ImageFormat) -> String {
    format!(
        "frame_{:05}_t{}ms.{}",
        index + 1,
        ms.max(0),
        format.extension()
    )
}

/// 图片编码线程数：环境变量优先，否则逻辑核数的一半，限制在 1..=4。
fn encode_threads() -> usize {
    if let Some(n) = std::env::var(ENV_EXTRACT_THREADS)
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
        .filter(|n| (1..=16).contains(n))
    {
        return n;
    }
    let cores = std::thread::available_parallelism().map_or(2, |n| n.get());
    (cores / 2).clamp(1, MAX_DEFAULT_THREADS)
}

/// 交给编码线程的一项任务。
struct Job {
    /// 解码帧（YUV 直通）。
    frame: frame::Video,
    /// 输出路径。
    path: PathBuf,
}

/// 抽帧参数。
pub struct ExtractParams<'a> {
    /// 输入视频。
    pub input: &'a Path,
    /// 输出目录（可不存在）。
    pub out_dir: &'a Path,
    /// 抽帧模式。
    pub mode: ExtractMode,
    /// 图片格式。
    pub format: ImageFormat,
    /// JPEG 质量。
    pub quality: u8,
}

/// 执行抽帧。
///
/// # 参数
/// - `params`：抽帧参数。
/// - `ctl`：进度与取消控制。
///
/// # 返回
/// 导出的张数；取消返回 `cancelled` 错误，失败返回可读错误。两种情况下都不留下中间目录。
///
/// # 示例
/// ```ignore
/// let n = run(&ExtractParams { input, out_dir, mode: ExtractMode::Keyframes,
///     format: ImageFormat::Png, quality: 90 }, &ctl)?;
/// ```
pub fn run(params: &ExtractParams<'_>, ctl: &TaskCtl) -> Result<u64, EditError> {
    let mut src = VideoSource::open(params.input)?;
    let (info, keys) = src.scan()?;
    let plan = plan_frames(params.mode, &info, &keys, |p| src.pts_to_ms(p))?;
    let scratch = scratch_dir(params.out_dir, std::process::id());
    std::fs::create_dir_all(&scratch)
        .map_err(|e| EditError::new(format!("创建中间目录失败: {e}")))?;
    let result = encode_all(&mut src, &plan, params, ctl, &scratch);
    let moved = result.and_then(|n| {
        publish(&scratch, params.out_dir)?;
        Ok(n)
    });
    let _ = std::fs::remove_dir_all(&scratch);
    moved
}

/// 解码并并行编码所有计划帧到中间目录。
fn encode_all(
    src: &mut VideoSource,
    plan: &[PlannedFrame],
    params: &ExtractParams<'_>,
    ctl: &TaskCtl,
    scratch: &Path,
) -> Result<u64, EditError> {
    let total = plan.len() as u64;
    let threads = encode_threads().min(plan.len()).max(1);
    let failure: Mutex<Option<EditError>> = Mutex::new(None);
    let stop = AtomicBool::new(false);
    let done = AtomicU64::new(0);
    let (tx, rx) = sync_channel::<Job>(threads * 2);
    let rx = Arc::new(Mutex::new(rx));
    let record_failure = |e: EditError| {
        if let Ok(mut slot) = failure.lock() {
            slot.get_or_insert(e);
        }
        stop.store(true, Ordering::SeqCst);
    };
    std::thread::scope(|scope| {
        for _ in 0..threads {
            let rx = Arc::clone(&rx);
            let (format, quality) = (params.format, params.quality);
            let (done, record_failure) = (&done, &record_failure);
            scope.spawn(move || {
                let mut encoder = match make_encoder(format, quality) {
                    Ok(e) => e,
                    Err(e) => return record_failure(e),
                };
                loop {
                    let job = match rx.lock() {
                        Ok(guard) => guard.recv(),
                        Err(_) => return,
                    };
                    let Ok(job) = job else { return };
                    if let Err(e) = encoder.encode(&job.frame, &job.path) {
                        return record_failure(e);
                    }
                    let n = done.fetch_add(1, Ordering::SeqCst) + 1;
                    ctl.report(n, total, STAGE_EXTRACT);
                }
            });
        }
        drop(rx);
        for (i, item) in plan.iter().enumerate() {
            if ctl.is_cancelled() || stop.load(Ordering::SeqCst) {
                break;
            }
            let decoded = match item.pts {
                Some(p) => src.frame_at_pts(p),
                None => src.frame_at_ms(item.ms),
            };
            let decoded = match decoded {
                Ok(d) => d,
                Err(e) => {
                    record_failure(e);
                    break;
                }
            };
            let path = scratch.join(frame_file_name(i, decoded.ms, params.format));
            if tx
                .send(Job {
                    frame: decoded.frame,
                    path,
                })
                .is_err()
            {
                break;
            }
        }
        drop(tx);
    });
    if ctl.is_cancelled() {
        return Err(EditError::cancelled());
    }
    if let Some(e) = failure.into_inner().ok().flatten() {
        return Err(e);
    }
    Ok(done.load(Ordering::SeqCst))
}

/// 把中间目录里的文件移入输出目录（输出目录不存在则创建，同名文件覆盖）。
fn publish(scratch: &Path, out_dir: &Path) -> Result<(), EditError> {
    std::fs::create_dir_all(out_dir)
        .map_err(|e| EditError::new(format!("创建输出目录失败: {e}")))?;
    let entries =
        std::fs::read_dir(scratch).map_err(|e| EditError::new(format!("读取中间目录失败: {e}")))?;
    for entry in entries.flatten() {
        let target = out_dir.join(entry.file_name());
        std::fs::rename(entry.path(), &target)
            .map_err(|e| EditError::new(format!("移动输出文件失败 {}: {e}", target.display())))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::edit::testclip::{self, Clip};
    use std::sync::atomic::AtomicBool;

    /// 构造一个静默的任务控制。
    fn quiet_ctl() -> TaskCtl {
        TaskCtl::new(Arc::new(AtomicBool::new(false)), Box::new(|_| {}))
    }

    /// 构造探测信息。
    fn info(duration_ms: u64) -> ProbeInfo {
        ProbeInfo {
            width: 64,
            height: 48,
            duration_ms,
            fps_milli: 25_000,
            frames: 0,
            keyframes: 0,
        }
    }

    /// 间隔模式：向上取整张数，且从 0 起。
    #[test]
    fn plan_interval_counts() {
        let p = plan_frames(
            ExtractMode::Interval { every_ms: 500 },
            &info(2000),
            &[],
            |p| p,
        )
        .unwrap();
        assert_eq!(
            p.iter().map(|f| f.ms).collect::<Vec<_>>(),
            [0, 500, 1000, 1500]
        );
        let p = plan_frames(
            ExtractMode::Interval { every_ms: 600 },
            &info(2000),
            &[],
            |p| p,
        )
        .unwrap();
        assert_eq!(p.len(), 4);
        // 极小间隔触发上限
        assert!(
            plan_frames(
                ExtractMode::Interval { every_ms: 1 },
                &info(10_000_000),
                &[],
                |p| p
            )
            .is_err()
        );
    }

    /// 单帧越界报错，关键帧用精确 PTS，空结果报错。
    #[test]
    fn plan_single_and_keyframes() {
        assert!(plan_frames(ExtractMode::Single { at_ms: 5000 }, &info(2000), &[], |p| p).is_err());
        let p = plan_frames(ExtractMode::Single { at_ms: 2050 }, &info(2000), &[], |p| p).unwrap();
        assert_eq!(
            p,
            [PlannedFrame {
                ms: 2050,
                pts: None
            }]
        );
        let p = plan_frames(ExtractMode::Keyframes, &info(2000), &[0, 1000], |p| p / 10).unwrap();
        assert_eq!(
            p[1],
            PlannedFrame {
                ms: 100,
                pts: Some(1000)
            }
        );
        assert!(plan_frames(ExtractMode::Keyframes, &info(2000), &[], |p| p).is_err());
    }

    /// 文件名带序号与时间，序号从 1 起。
    #[test]
    fn file_names() {
        assert_eq!(
            frame_file_name(0, 0, ImageFormat::Png),
            "frame_00001_t0ms.png"
        );
        assert_eq!(
            frame_file_name(11, 1500, ImageFormat::Jpeg),
            "frame_00012_t1500ms.jpg"
        );
        assert_eq!(
            frame_file_name(1, 40, ImageFormat::WebpLossless),
            "frame_00002_t40ms.webp"
        );
    }

    /// 用样片抽三种格式：文件存在、签名正确，像素对应目标帧。
    #[test]
    fn extract_single_all_formats() {
        let dir = testclip::temp_dir("extract-single");
        let clip = Clip::default();
        let input = dir.join("in.mp4");
        testclip::make_clip(&input, &clip).unwrap();
        let ctl = quiet_ctl();
        for format in [
            ImageFormat::Png,
            ImageFormat::Jpeg,
            ImageFormat::WebpLossless,
        ] {
            let out = dir.join(format!("out-{}", format.as_str()));
            let n = run(
                &ExtractParams {
                    input: &input,
                    out_dir: &out,
                    mode: ExtractMode::Single { at_ms: 1000 },
                    format,
                    quality: 90,
                },
                &ctl,
            )
            .unwrap();
            assert_eq!(n, 1);
            let files: Vec<_> = std::fs::read_dir(&out).unwrap().flatten().collect();
            assert_eq!(files.len(), 1, "{format:?}");
            let bytes = std::fs::read(files[0].path()).unwrap();
            // 1000ms @25fps -> 第 25 帧
            let expect_gray = testclip::gray_of_frame(25);
            match format {
                ImageFormat::Png => {
                    assert_eq!(&bytes[..8], b"\x89PNG\r\n\x1a\n");
                    let (w, h, bgr) = testclip::decode_bgr(&files[0].path());
                    assert_eq!((w, h), (clip.width, clip.height));
                    assert_gray(&bgr, expect_gray, 2, "png");
                }
                ImageFormat::Jpeg => {
                    assert_eq!(&bytes[..2], [0xFF, 0xD8]);
                    assert_eq!(testclip::jpeg_size(&bytes), Some((clip.width, clip.height)));
                    let (_, _, bgr) = testclip::decode_bgr(&files[0].path());
                    assert_gray(&bgr, expect_gray, 6, "jpeg");
                }
                ImageFormat::WebpLossless => {
                    assert_eq!(&bytes[..4], b"RIFF");
                    assert_eq!(&bytes[8..12], b"WEBP");
                    // 单帧无损 WebP 应是静态图（VP8L 块），不是动画容器
                    assert_eq!(&bytes[12..16], b"VP8L", "首块类型");
                    let px = testclip::decode_webp_first_pixel(&files[0].path());
                    assert!(
                        (i32::from(px) - i32::from(expect_gray)).abs() <= 4,
                        "webp {px} vs {expect_gray}"
                    );
                }
            }
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 断言 BGR 缓冲首像素三通道都接近期望灰度。
    fn assert_gray(bgr: &[u8], want: u8, tolerance: i32, what: &str) {
        for c in &bgr[..3] {
            assert!(
                (i32::from(*c) - i32::from(want)).abs() <= tolerance,
                "{what}: {c} vs {want}"
            );
        }
    }

    /// 间隔与关键帧模式的张数正确，且不留下中间目录。
    #[test]
    fn extract_interval_and_keyframes() {
        let dir = testclip::temp_dir("extract-multi");
        let clip = Clip::default();
        let input = dir.join("in.mp4");
        testclip::make_clip(&input, &clip).unwrap();
        let ctl = quiet_ctl();
        let out = dir.join("interval");
        let n = run(
            &ExtractParams {
                input: &input,
                out_dir: &out,
                mode: ExtractMode::Interval { every_ms: 400 },
                format: ImageFormat::Png,
                quality: 90,
            },
            &ctl,
        )
        .unwrap();
        // 72 帧 @25fps = 2880ms，每 400ms 一张 -> 8 张
        assert_eq!(n, 8);
        assert_eq!(std::fs::read_dir(&out).unwrap().count(), 8);
        let out = dir.join("keys");
        let n = run(
            &ExtractParams {
                input: &input,
                out_dir: &out,
                mode: ExtractMode::Keyframes,
                format: ImageFormat::Jpeg,
                quality: 80,
            },
            &ctl,
        )
        .unwrap();
        assert_eq!(n, u64::from(clip.frames.div_ceil(clip.gop)));
        // 中间目录已清理
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

    /// 取消：返回取消错误，输出目录与中间目录都不存在。
    #[test]
    fn cancel_leaves_nothing() {
        let dir = testclip::temp_dir("extract-cancel");
        let input = dir.join("in.mp4");
        testclip::make_clip(&input, &Clip::default()).unwrap();
        let flag = Arc::new(AtomicBool::new(true));
        let ctl = TaskCtl::new(flag, Box::new(|_| {}));
        let out = dir.join("out");
        let err = run(
            &ExtractParams {
                input: &input,
                out_dir: &out,
                mode: ExtractMode::Interval { every_ms: 100 },
                format: ImageFormat::Png,
                quality: 90,
            },
            &ctl,
        )
        .unwrap_err();
        assert!(err.is_cancelled());
        assert!(!out.exists());
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

    /// 损坏输入返回可读错误而不是 panic。
    #[test]
    fn corrupt_input_is_error() {
        let dir = testclip::temp_dir("extract-corrupt");
        let input = dir.join("bad.mp4");
        std::fs::write(&input, b"this is not a video").unwrap();
        let err = run(
            &ExtractParams {
                input: &input,
                out_dir: &dir.join("out"),
                mode: ExtractMode::Keyframes,
                format: ImageFormat::Png,
                quality: 90,
            },
            &quiet_ctl(),
        )
        .unwrap_err();
        assert!(!err.is_cancelled());
        assert!(!err.to_string().is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
