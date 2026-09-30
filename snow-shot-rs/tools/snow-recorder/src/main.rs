//! Snow Shot 录制工作进程。
//!
//! 由主程序拉起：从 stdin 读取行协议命令，向 stdout 回报事件（见 `snow-recorder-protocol`）。
//! 一个进程只做一次录制；stdin 被关闭（主程序退出或崩溃）视为取消，并清理半截文件。
//! 采集/转换/编码在后端的工作线程里进行（见 `backend`），本进程的主线程只做命令分发与状态回报。

mod backend;
mod clock;
mod geom;
mod os;
mod pipeline;
mod plan;
mod settings;
mod soft;
mod tailfix;
mod timeline;
#[cfg(windows)]
mod win;

use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::time::{Duration, Instant};

use snow_recorder_protocol::{Command, Event, MediaFormat, StartRequest, scratch_dir, scratch_file};

use backend::RecordingBackend;
use clock::ActiveClock;

/// 状态回报间隔。
const TICK: Duration = Duration::from_millis(500);
/// 进程退出码：正常结束。
const EXIT_OK: i32 = 0;
/// 进程退出码：录制失败。
const EXIT_FAILED: i32 = 1;

/// 一次进行中的录制。
struct Active {
    /// 录制后端（stop 会阻塞，仅在本线程调用）。
    session: Box<dyn RecordingBackend>,
    /// 有效时长时钟。
    clock: ActiveClock,
    /// 帧率，用于估算帧数。
    fps: u32,
    /// 输出格式（动图需要收尾修补时长）。
    format: MediaFormat,
    /// 最终输出路径。
    final_path: PathBuf,
    /// 中间产物文件路径（位于中间目录内）。
    partial: PathBuf,
}

/// 向 stdout 写一条事件并刷新；管道断开时静默（随后 stdin EOF 会触发清理）。
fn emit(event: &Event) {
    let mut out = std::io::stdout().lock();
    let _ = writeln!(out, "{}", event.to_line());
    let _ = out.flush();
}

/// 删除本进程的中间目录（含半截文件；目录不存在则忽略）。
///
/// # 参数
/// - `final_path`：最终输出路径，用于推导中间目录。
fn remove_scratch(final_path: &Path) {
    let _ = std::fs::remove_dir_all(scratch_dir(final_path, std::process::id()));
}

/// 以失败状态收尾：清理、回报、退出。
fn fail(reason: String, final_path: &Path) -> ! {
    remove_scratch(final_path);
    emit(&Event::Error { reason });
    std::process::exit(EXIT_FAILED);
}

/// 启动一次录制。
///
/// # 参数
/// - `request`：开始请求。
///
/// # 返回
/// 进行中的录制；失败返回原因文本。
fn start_recording(request: &StartRequest) -> Result<Active, String> {
    let pid = std::process::id();
    let scratch = scratch_dir(&request.output, pid);
    std::fs::create_dir_all(&scratch).map_err(|e| format!("创建输出目录失败: {e}"))?;
    let partial = scratch_file(&request.output, pid);
    let session = backend::start(request, &partial)?;
    eprintln!("录制后端: {}", session.name());
    let mut clock = ActiveClock::new();
    clock.start(Instant::now());
    Ok(Active {
        session,
        clock,
        fps: request.fps,
        format: request.format,
        final_path: request.output.clone(),
        partial,
    })
}

/// 停止录制并把临时文件改名为最终文件，回报 `Finished`。
fn finish(active: Active) -> ! {
    let Active {
        session,
        clock,
        format,
        final_path,
        partial,
        ..
    } = active;
    // 先取有效时长再 stop（stop 会阻塞收尾，不应计入）
    let target_ms = u64::try_from(clock.elapsed(Instant::now()).as_millis()).unwrap_or(u64::MAX);
    let report = match session.stop() {
        Ok(report) => report,
        Err(e) => fail(e, &final_path),
    };
    // 诊断信息走 stderr（主程序只写入 debug 日志）
    eprintln!("{}", report.diagnostics);
    // 动图容器丢失静止尾段：发布前补足最后一帧时长，失败不影响成品
    match tailfix::extend_file(&partial, format, target_ms) {
        Ok(Some(r)) => eprintln!("动图尾段修补: {r:?} 目标={target_ms}ms"),
        Ok(None) => {}
        Err(e) => eprintln!("动图尾段修补失败（保留原文件）: {e}"),
    }
    if let Err(e) = std::fs::rename(&partial, &final_path) {
        fail(format!("移动输出文件失败: {e}"), &final_path);
    }
    remove_scratch(&final_path);
    emit(&Event::Finished { path: final_path, frames: report.frames, dropped: report.dropped });
    std::process::exit(EXIT_OK);
}

/// 取消录制：终止后端（内部会终止工作线程）并清理中间目录，正常退出。
fn cancel(active: Active) -> ! {
    let Active {
        session,
        final_path,
        ..
    } = active;
    session.cancel();
    remove_scratch(&final_path);
    std::process::exit(EXIT_OK);
}

/// 无录制进行时收到 EOF/取消：直接退出。
fn exit_idle() -> ! {
    std::process::exit(EXIT_OK);
}

/// 进程入口：命令循环。
fn main() {
    let (tx, rx) = mpsc::channel::<Option<String>>();
    std::thread::spawn(move || {
        let stdin = std::io::stdin();
        for line in stdin.lock().lines() {
            match line {
                Ok(l) => {
                    if tx.send(Some(l)).is_err() {
                        return;
                    }
                }
                Err(_) => break,
            }
        }
        let _ = tx.send(None);
    });

    emit(&Event::Ready);
    let mut active: Option<Active> = None;
    loop {
        match rx.recv_timeout(TICK) {
            Ok(Some(line)) => {
                if line.trim().is_empty() {
                    continue;
                }
                match Command::parse(line.trim_start_matches('\u{feff}')) {
                    Ok(cmd) => active = handle(cmd, active),
                    Err(e) => eprintln!("忽略无法解析的命令: {e}"),
                }
            }
            // 主程序关闭了管道（退出或崩溃）：取消并清理
            Ok(None) | Err(RecvTimeoutError::Disconnected) => match active {
                Some(a) => cancel(a),
                None => exit_idle(),
            },
            Err(RecvTimeoutError::Timeout) => {}
        }
        if let Some(a) = &active
            && a.clock.is_running()
        {
            let elapsed_ms = u64::try_from(a.clock.elapsed(Instant::now()).as_millis())
                .unwrap_or(u64::MAX);
            emit(&Event::Recording {
                elapsed_ms,
                frames: elapsed_ms.saturating_mul(u64::from(a.fps)) / 1000,
            });
        }
    }
}

/// 处理一条命令，返回新的录制状态。
fn handle(cmd: Command, active: Option<Active>) -> Option<Active> {
    match (cmd, active) {
        (Command::Start(req), None) => match start_recording(&req) {
            Ok(a) => Some(a),
            Err(reason) => fail(reason, &req.output),
        },
        (Command::Start(_), Some(a)) => {
            eprintln!("忽略重复的 START");
            Some(a)
        }
        (Command::Pause, Some(mut a)) => {
            match a.session.pause() {
                Ok(()) => {
                    a.clock.pause(Instant::now());
                    emit(&Event::Paused);
                }
                Err(e) => eprintln!("暂停失败: {e}"),
            }
            Some(a)
        }
        (Command::Resume, Some(mut a)) => {
            match a.session.resume() {
                Ok(()) => {
                    a.clock.resume(Instant::now());
                    emit(&Event::Resumed);
                }
                Err(e) => eprintln!("恢复失败: {e}"),
            }
            Some(a)
        }
        (Command::Stop, Some(a)) => finish(a),
        (Command::Cancel, Some(a)) => cancel(a),
        (Command::Cancel, None) => exit_idle(),
        (other, None) => {
            eprintln!("未开始录制，忽略命令: {other:?}");
            None
        }
    }
}
