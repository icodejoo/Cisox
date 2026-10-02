//! 系统引擎抽帧：MF 精确 seek 解码 + WIC 并行编码 PNG / JPEG。
//!
//! 中间目录、发布与取消清理规则与 FFmpeg 引擎一致：先写进 `.snow-recording-<pid>`，
//! 全部成功后再移入输出目录。无损 WebP 系统做不了，直接给出可回落的"不支持"错误。

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::sync_channel;
use std::sync::{Arc, Mutex};

use snow_recorder_protocol::{ExtractMode, ImageFormat, scratch_dir};

use super::MfSession;
use super::reader::{Frame, MfSource, hns_to_ms, scan};
use crate::edit::extract::{encode_threads, frame_file_name, plan_frames, publish};
use crate::edit::image::BgrWriter;
use crate::edit::{EditError, TaskCtl};

/// 进度阶段名。
const STAGE_EXTRACT: &str = "extract";

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

/// 交给编码线程的一项任务。
struct Job {
    /// 解码帧（BGR24）。
    frame: Arc<Frame>,
    /// 输出路径。
    path: PathBuf,
}

/// 执行抽帧。
///
/// # 参数
/// - `params`：抽帧参数。
/// - `ctl`：进度与取消控制。
///
/// # 返回
/// 导出的张数；取消返回 `cancelled` 错误。失败或取消时不留下中间目录。
pub fn run(params: &ExtractParams<'_>, ctl: &TaskCtl) -> Result<u64, EditError> {
    if params.format == ImageFormat::WebpLossless {
        return Err(EditError::unsupported("系统引擎不支持无损 WebP"));
    }
    let _session = MfSession::start()?;
    let scanned = scan(params.input)?;
    let plan = plan_frames(params.mode, &scanned.info, &scanned.keys, hns_to_ms)?;
    let mut src = MfSource::open(params.input, &scanned)?;
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
    src: &mut MfSource,
    plan: &[crate::edit::extract::PlannedFrame],
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
            let jpeg = params.format == ImageFormat::Jpeg;
            let quality = params.quality;
            let (done, record_failure) = (&done, &record_failure);
            scope.spawn(move || {
                let writer = match BgrWriter::new(jpeg, quality) {
                    Ok(w) => w,
                    Err(e) => return record_failure(e),
                };
                loop {
                    let job = match rx.lock() {
                        Ok(guard) => guard.recv(),
                        Err(_) => return,
                    };
                    let Ok(job) = job else { return };
                    let f = &job.frame;
                    let stride = f.width as usize * 3;
                    if let Err(e) = writer.write((f.width, f.height), stride, &f.bgr, &job.path) {
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
                Some(p) => src.frame_at_hns(p),
                None => src.frame_at_ms(item.ms),
            };
            let decoded = match decoded {
                Ok(d) => d,
                Err(e) => {
                    record_failure(e);
                    break;
                }
            };
            let path = scratch.join(frame_file_name(i, decoded.ms(), params.format));
            if tx
                .send(Job {
                    frame: decoded,
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
