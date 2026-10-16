//! 导出专用的一次性录制进程调用封装：按需拉起独立的 `cisox-recorder`，发一条 Edit，用完即退。
//!
//! 与常驻的编辑会话（见 [`crate::video_editor::session`]）分开，导出期间预览和缩略图不受影响。
//! 调用方周期性 [`ExportWorker::poll`]；丢弃 [`ExportWorker`] 会在后台线程里收尾子进程。

use crate::recording::client::{ProcessRecorderLink, locate_recorder_exe};
use crate::recording::runtime::{LinkEvent, RecorderLink};
use snow_recorder_protocol::{Command, EditOp, EditRequest, EngineKind, Event};
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// 导出进程对调用方回报的事件。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExportEvent {
    /// 编辑进度（`total` 为 0 表示总量未知）。
    Progress {
        /// 已完成数量。
        done: u64,
        /// 总数。
        total: u64,
    },
    /// 完成，`path` 为输出文件。
    Finished {
        /// 输出路径。
        path: PathBuf,
    },
    /// 失败（原因为单行说明，进程已不可用）。
    Failed(String),
}

/// 按保留区间生成编辑操作：单段走关键帧裁剪，多段走多段裁剪拼接。
///
/// # 参数
/// - `ranges`：保留区间 `(start, end)`（毫秒，升序、不重叠）。
///
/// # 返回
/// 对应的 [`EditOp`]；`ranges` 为空返回 `None`。
///
/// # 示例
/// ```ignore
/// let op = export_op(&[(0, 1000), (2000, 3000)]);
/// ```
pub fn export_op(ranges: &[(u64, u64)]) -> Option<EditOp> {
    match ranges {
        [] => None,
        [(start_ms, end_ms)] => Some(EditOp::TrimKeyframe {
            start_ms: *start_ms,
            end_ms: *end_ms,
        }),
        many => Some(EditOp::TrimSegments {
            segments: many.to_vec(),
        }),
    }
}

/// 一次性导出进程句柄。
pub struct ExportWorker {
    /// 进程通道；丢弃时移交后台线程收尾。
    link: Option<ProcessRecorderLink>,
    /// 是否已见到终态事件（完成或失败）。
    done: bool,
}

impl ExportWorker {
    /// 拉起进程并提交导出。
    ///
    /// # 参数
    /// - `input`：原视频路径。
    /// - `ranges`：保留区间（毫秒）。
    /// - `output`：输出文件（调用方须保证不覆盖已有文件）。
    ///
    /// # 返回
    /// 句柄；区间为空、找不到录制进程或启动 / 写入失败时返回错误说明。
    pub fn start(input: &Path, ranges: &[(u64, u64)], output: &Path) -> Result<Self, String> {
        let op = export_op(ranges).ok_or("nothing to export")?;
        let exe = locate_recorder_exe().ok_or("cisox-recorder not found")?;
        let mut link =
            ProcessRecorderLink::spawn(&exe, Arc::new(|| {})).map_err(|e| format!("{e:?}"))?;
        link.send(&Command::Edit(EditRequest {
            engine: EngineKind::Auto,
            op,
            input: input.to_path_buf(),
            output: output.to_path_buf(),
        }))?;
        Ok(Self {
            link: Some(link),
            done: false,
        })
    }

    /// 取走已到达的事件（非阻塞）。进程无终态地退出时补一条 `Failed`。
    pub fn poll(&mut self) -> Vec<ExportEvent> {
        let Some(link) = self.link.as_mut() else {
            return Vec::new();
        };
        let mut out = Vec::new();
        for event in link.poll() {
            if self.done {
                break;
            }
            match event {
                LinkEvent::Event(Event::EditProgress { done, total, .. }) => {
                    out.push(ExportEvent::Progress { done, total });
                }
                LinkEvent::Event(Event::EditFinished { path, .. }) => {
                    self.done = true;
                    out.push(ExportEvent::Finished { path });
                }
                LinkEvent::Event(Event::Error { reason }) => {
                    self.done = true;
                    out.push(ExportEvent::Failed(reason));
                }
                LinkEvent::Event(_) => {}
                LinkEvent::Exited { code } => {
                    self.done = true;
                    out.push(ExportEvent::Failed(format!(
                        "recorder exited unexpectedly (code {code:?})"
                    )));
                }
            }
        }
        out
    }
}

impl Drop for ExportWorker {
    /// 把进程通道交给后台线程关闭（其 `drop` 会等子进程退出，最长数秒）。
    fn drop(&mut self) {
        if let Some(link) = self.link.take() {
            std::thread::spawn(move || drop(link));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 单段走关键帧裁剪，多段走多段裁剪，空区间不导出。
    #[test]
    fn export_op_picks_variant() {
        assert_eq!(export_op(&[]), None);
        assert_eq!(
            export_op(&[(100, 900)]),
            Some(EditOp::TrimKeyframe {
                start_ms: 100,
                end_ms: 900
            })
        );
        assert_eq!(
            export_op(&[(0, 1_000), (2_000, 3_000)]),
            Some(EditOp::TrimSegments {
                segments: vec![(0, 1_000), (2_000, 3_000)]
            })
        );
    }
}
