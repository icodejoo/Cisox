//! 常驻编辑会话：窗口打开时拉起一个 `cisox-recorder`（`OPEN`），窗口关闭时 `CLOSE` 并回收。
//!
//! 负责探测信息、关键帧表、缩略图（批次，新批次打断旧批次）和（引擎不可用时的）预览取帧。
//! 帧文件是纯 BGRA 小文件，事件到达时立即读走（旧文件会被 recorder 清理）。
//! 调用方周期性 [`Session::poll`]；丢弃 [`Session`] 在后台线程里收尾，不阻塞界面。

use crate::recording::client::{ProcessRecorderLink, locate_recorder_exe};
use crate::recording::runtime::{LinkEvent, RecorderLink};
use snow_recorder_protocol::{Command, Event, ProbeInfo, session_scratch_dir};
use std::path::Path;
use std::sync::Arc;

/// 会话对调用方回报的事件。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SessionEvent {
    /// 探测结果。
    Probed(ProbeInfo),
    /// 关键帧时间表（毫秒，升序）。
    Keyframes(Vec<u64>),
    /// 一张缩略图就绪（BGRA 像素已读入内存）。
    Thumb {
        /// 批次号。
        id: u64,
        /// 批内序号。
        index: u64,
        /// 实际帧时间（毫秒）。
        at_ms: u64,
        /// 宽。
        w: u32,
        /// 高。
        h: u32,
        /// BGRA 像素。
        bgra: Vec<u8>,
    },
    /// 预览帧就绪（BGRA 像素已读入内存）。
    Frame {
        /// 请求序号。
        seq: u64,
        /// 实际帧时间（毫秒）。
        at_ms: u64,
        /// 宽。
        w: u32,
        /// 高。
        h: u32,
        /// BGRA 像素。
        bgra: Vec<u8>,
    },
    /// 失败（会话已不可用）。
    Failed(String),
}

/// 读一张 BGRA 帧文件并校验长度。
///
/// # 参数
/// - `path`：帧文件。
/// - `w`、`h`：事件里给出的宽高。
///
/// # 返回
/// 像素；读不到或长度不是 `w*h*4` 返回 `None`。
fn read_bgra(path: &Path, w: u32, h: u32) -> Option<Vec<u8>> {
    let bytes = std::fs::read(path).ok()?;
    (bytes.len() == w as usize * h as usize * 4).then_some(bytes)
}

/// 编辑会话句柄。
pub struct Session {
    /// 进程通道；丢弃时移交后台线程收尾。
    link: Option<ProcessRecorderLink>,
    /// 子进程 ID（结束后兜底清理 scratch 目录）。
    pid: u32,
}

impl Session {
    /// 拉起会话并发送 `OPEN`。
    ///
    /// # 参数
    /// - `input`：视频路径。
    ///
    /// # 返回
    /// 句柄；找不到录制进程或启动 / 写入失败时返回错误说明。
    pub fn open(input: &Path) -> Result<Self, String> {
        let exe = locate_recorder_exe().ok_or("cisox-recorder not found")?;
        let mut link =
            ProcessRecorderLink::spawn(&exe, Arc::new(|| {})).map_err(|e| format!("{e:?}"))?;
        let pid = link.pid();
        link.send(&Command::Open {
            input: input.to_path_buf(),
        })?;
        Ok(Self {
            link: Some(link),
            pid,
        })
    }

    /// 请求一批缩略图；会打断尚未完成的上一批。
    ///
    /// # 参数
    /// - `id`：批次号（事件原样带回）。
    /// - `width`、`height`：目标框（像素）。
    /// - `times_ms`：要取帧的时间。
    pub fn thumbs(&mut self, id: u64, width: u32, height: u32, times_ms: Vec<u64>) {
        self.send(&Command::Thumbs {
            id,
            width,
            height,
            times_ms,
        });
    }

    /// 请求某时刻的预览帧（引擎不可用时的回落）；会打断缩略图批次。
    pub fn frame(&mut self, seq: u64, at_ms: u64, max_w: u32) {
        self.send(&Command::Frame { seq, at_ms, max_w });
    }

    /// 发送命令；失败视为会话已死。
    fn send(&mut self, command: &Command) {
        let Some(link) = self.link.as_mut() else {
            return;
        };
        if let Err(e) = link.send(command) {
            tracing::debug!(error = %e, "编辑会话写入失败");
        }
    }

    /// 取走已到达的事件（非阻塞），帧文件在此读入内存。
    pub fn poll(&mut self) -> Vec<SessionEvent> {
        let Some(link) = self.link.as_mut() else {
            return Vec::new();
        };
        let mut out = Vec::new();
        for event in link.poll() {
            match event {
                LinkEvent::Event(Event::ProbeResult(info)) => out.push(SessionEvent::Probed(info)),
                LinkEvent::Event(Event::Keyframes { ms }) => out.push(SessionEvent::Keyframes(ms)),
                LinkEvent::Event(Event::ThumbReady {
                    id,
                    index,
                    at_ms,
                    w,
                    h,
                    path,
                }) => {
                    if let Some(bgra) = read_bgra(&path, w, h) {
                        out.push(SessionEvent::Thumb {
                            id,
                            index,
                            at_ms,
                            w,
                            h,
                            bgra,
                        });
                    }
                }
                LinkEvent::Event(Event::FrameReady {
                    seq,
                    at_ms,
                    w,
                    h,
                    path,
                }) => {
                    if let Some(bgra) = read_bgra(&path, w, h) {
                        out.push(SessionEvent::Frame {
                            seq,
                            at_ms,
                            w,
                            h,
                            bgra,
                        });
                    }
                }
                LinkEvent::Event(Event::Error { reason }) => {
                    out.push(SessionEvent::Failed(reason));
                }
                LinkEvent::Event(_) => {}
                LinkEvent::Exited { code } => {
                    out.push(SessionEvent::Failed(format!(
                        "recorder exited unexpectedly (code {code:?})"
                    )));
                }
            }
        }
        out
    }
}

impl Drop for Session {
    /// 发 `CLOSE`，再把进程通道交给后台线程收尾，并兜底删除 scratch 目录。
    fn drop(&mut self) {
        if let Some(mut link) = self.link.take() {
            let _ = link.send(&Command::Close);
            let pid = self.pid;
            std::thread::spawn(move || {
                drop(link);
                let _ = std::fs::remove_dir_all(session_scratch_dir(pid));
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// BGRA 文件长度必须等于 w*h*4，否则丢弃。
    #[test]
    fn read_bgra_checks_length() {
        let dir = std::env::temp_dir().join(format!("cisox-session-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let ok = dir.join("ok.bgra");
        std::fs::write(&ok, vec![7u8; 2 * 3 * 4]).unwrap();
        assert_eq!(read_bgra(&ok, 2, 3).map(|v| v.len()), Some(24));
        assert_eq!(read_bgra(&ok, 3, 3), None);
        assert_eq!(read_bgra(&dir.join("missing.bgra"), 2, 3), None);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
