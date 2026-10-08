//! 录制进程客户端：拉起 `snow-recorder` 子进程并按行协议通信。
//!
//! 读线程把 stdout 事件转发到通道并唤醒主线程；stderr 由独立线程排空（否则编码器日志会写满管道卡死子进程）。
//! 子进程崩溃或被杀时读线程见到 EOF，会话据此复位；`shutdown` 负责收尸并清理中间产物目录。

use crate::recording::model::RecordingFailure;
use crate::recording::runtime::{LinkEvent, RecorderLink};
use snow_recorder_protocol::{Command, Event, scratch_dir};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Stdio};
use std::sync::Arc;
use std::sync::mpsc::{Receiver, Sender, channel};
use std::time::{Duration, Instant};

/// 录制进程可执行文件名。
#[cfg(windows)]
pub const RECORDER_EXE_NAME: &str = "snow-recorder.exe";
/// 录制进程可执行文件名。
#[cfg(not(windows))]
pub const RECORDER_EXE_NAME: &str = "snow-recorder";
/// 指定录制进程路径的环境变量（优先级最高）。
pub const ENV_RECORDER_EXE: &str = "SNOW_RECORDER_EXE";
/// 开发布局下录制进程相对仓库工作区的构建产物目录。
const DEV_RELATIVE_DIR: [&str; 4] = ["tools", "snow-recorder", "target", "release"];
/// 开发布局的另一种形态：上层目录是仓库根，工作区在其 `snow-shot-rs` 子目录下。
const DEV_WORKSPACE_DIR: &str = "snow-shot-rs";
/// `shutdown` 等待子进程自行退出的宽限期。
const SHUTDOWN_GRACE: Duration = Duration::from_secs(3);
/// 轮询子进程退出的间隔。
const EXIT_POLL_INTERVAL: Duration = Duration::from_millis(30);
/// 读线程见到 EOF 后等待退出码的最长时间。
const EXIT_CODE_WAIT: Duration = Duration::from_millis(500);
/// Windows：不为子进程创建控制台窗口。
#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// 查找录制进程可执行文件。
///
/// 顺序：环境变量 → 与主程序同目录 → 沿主程序目录逐级向上的 `tools/snow-recorder/target/release/`（开发布局）。
///
/// # 参数
/// - `env_value`：环境变量 [`ENV_RECORDER_EXE`] 的值。
/// - `current_exe`：主程序路径。
/// - `exists`：文件存在性判断（便于测试）。
///
/// # 返回
/// 找到的路径；都不存在返回 `None`。
///
/// # 示例
/// ```ignore
/// let exe = find_recorder_exe(None, Path::new("C:/app/snow-shot.exe"), |p| p.exists());
/// ```
pub fn find_recorder_exe(
    env_value: Option<&str>,
    current_exe: &Path,
    exists: impl Fn(&Path) -> bool,
) -> Option<PathBuf> {
    if let Some(text) = env_value.map(str::trim).filter(|t| !t.is_empty()) {
        let path = PathBuf::from(text);
        return exists(&path).then_some(path);
    }
    let dir = current_exe.parent()?;
    let beside = dir.join(RECORDER_EXE_NAME);
    if exists(&beside) {
        return Some(beside);
    }
    for ancestor in dir.ancestors() {
        for base in [ancestor.to_path_buf(), ancestor.join(DEV_WORKSPACE_DIR)] {
            let mut candidate = base;
            candidate.extend(DEV_RELATIVE_DIR);
            candidate.push(RECORDER_EXE_NAME);
            if exists(&candidate) {
                return Some(candidate);
            }
        }
    }
    None
}

/// 按当前进程环境查找录制进程。
///
/// # 返回
/// 路径；找不到返回 `None`。
pub fn locate_recorder_exe() -> Option<PathBuf> {
    let env_value = std::env::var(ENV_RECORDER_EXE).ok();
    let current = std::env::current_exe().ok()?;
    find_recorder_exe(env_value.as_deref(), &current, Path::exists)
}

/// 真实的录制进程通道。
pub struct ProcessRecorderLink {
    /// 子进程句柄。
    child: Child,
    /// 子进程标准输入（`shutdown` 时关闭以触发其自行清理）。
    stdin: Option<ChildStdin>,
    /// 读线程转发的事件。
    events: Receiver<LinkEvent>,
    /// 最终输出路径（发出 START 后记录，用于推导中间目录）。
    output: Option<PathBuf>,
    /// 是否已完成收尾。
    closed: bool,
}

impl ProcessRecorderLink {
    /// 拉起录制进程。
    ///
    /// # 参数
    /// - `exe`：录制进程可执行文件。
    /// - `wake`：有新事件时调用（须线程安全，通常是向主线程收件箱投递一条轮询事件）。
    ///
    /// # 返回
    /// 通道；进程无法启动返回错误说明。
    ///
    /// # 示例
    /// ```ignore
    /// let link = ProcessRecorderLink::spawn(&exe, Arc::new(|| inbox.push(UiEvent::RecorderPoll)))?;
    /// ```
    pub fn spawn(exe: &Path, wake: Arc<dyn Fn() + Send + Sync>) -> Result<Self, RecordingFailure> {
        let mut command = std::process::Command::new(exe);
        command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            command.creation_flags(CREATE_NO_WINDOW);
        }
        let mut child = command.spawn().map_err(|e| RecordingFailure::SpawnFailed {
            exe: exe.display().to_string(),
            detail: e.to_string(),
        })?;
        let stdin = child.stdin.take();
        let (tx, rx) = channel();
        if let Some(stdout) = child.stdout.take() {
            std::thread::spawn(move || read_events(stdout, tx, wake));
        }
        if let Some(stderr) = child.stderr.take() {
            std::thread::spawn(move || drain_stderr(stderr));
        }
        tracing::info!(pid = child.id(), exe = %exe.display(), "录制进程已启动");
        Ok(Self {
            child,
            stdin,
            events: rx,
            output: None,
            closed: false,
        })
    }

    /// 等待子进程退出并取得退出码（最多 `limit`）。
    fn wait_exit_code(&mut self, limit: Duration) -> Option<i32> {
        let deadline = Instant::now() + limit;
        loop {
            match self.child.try_wait() {
                Ok(Some(status)) => return status.code(),
                Ok(None) if Instant::now() < deadline => std::thread::sleep(EXIT_POLL_INTERVAL),
                _ => return None,
            }
        }
    }

    /// 清理本进程留下的中间目录（崩溃 / 被杀时才会有残留）。
    fn remove_scratch(&self) {
        if let Some(output) = &self.output {
            let dir = scratch_dir(output, self.child.id());
            if dir.exists() {
                match std::fs::remove_dir_all(&dir) {
                    Ok(()) => tracing::info!(dir = %dir.display(), "已清理录制中间产物"),
                    Err(e) => {
                        tracing::warn!(dir = %dir.display(), error = %e, "清理录制中间产物失败")
                    }
                }
            }
        }
    }
}

/// 读线程：逐行解析 stdout，转发事件；EOF 时补一个 `Exited`。
fn read_events(
    stdout: impl std::io::Read,
    tx: Sender<LinkEvent>,
    wake: Arc<dyn Fn() + Send + Sync>,
) {
    for line in BufReader::new(stdout).lines() {
        let Ok(line) = line else { break };
        match Event::parse(&line) {
            Ok(event) => {
                if tx.send(LinkEvent::Event(event)).is_err() {
                    return;
                }
                wake();
            }
            Err(e) => tracing::warn!(line = %line, error = %e, "录制进程输出无法解析"),
        }
    }
    let _ = tx.send(LinkEvent::Exited { code: None });
    wake();
}

/// stderr 排空线程：内容只进 debug 日志。
fn drain_stderr(stderr: impl std::io::Read) {
    for line in BufReader::new(stderr).lines() {
        let Ok(line) = line else { break };
        tracing::debug!(target: "snow_recorder", "{line}");
    }
}

impl RecorderLink for ProcessRecorderLink {
    /// 写一行命令到子进程 stdin。
    fn send(&mut self, command: &Command) -> Result<(), String> {
        if let Command::Start(request) = command {
            self.output = Some(request.output.clone());
        }
        let stdin = self.stdin.as_mut().ok_or("the recorder stdin is closed")?;
        writeln!(stdin, "{}", command.to_line())
            .and_then(|()| stdin.flush())
            .map_err(|e| e.to_string())
    }

    /// 取走已收到的事件；`Exited` 事件补上真实退出码。
    fn poll(&mut self) -> Vec<LinkEvent> {
        let mut out = Vec::new();
        while let Ok(event) = self.events.try_recv() {
            match event {
                LinkEvent::Exited { .. } => {
                    let code = self.wait_exit_code(EXIT_CODE_WAIT);
                    out.push(LinkEvent::Exited { code });
                }
                other => out.push(other),
            }
        }
        out
    }

    /// 关闭 stdin → 宽限期内等待退出 → 超时强杀 → 清理中间目录。
    fn shutdown(&mut self) {
        if self.closed {
            return;
        }
        self.closed = true;
        drop(self.stdin.take());
        if self.wait_exit_code(SHUTDOWN_GRACE).is_none()
            && matches!(self.child.try_wait(), Ok(None))
        {
            tracing::warn!(pid = self.child.id(), "录制进程未在宽限期内退出，强制终止");
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
        self.remove_scratch();
    }
}

impl Drop for ProcessRecorderLink {
    /// 丢弃通道时确保子进程不会遗留。
    fn drop(&mut self) {
        self.shutdown();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    /// 用集合模拟文件系统。
    fn fs(paths: &[&str]) -> impl Fn(&Path) -> bool {
        let set: HashSet<PathBuf> = paths.iter().map(PathBuf::from).collect();
        move |p| set.contains(p)
    }

    /// 环境变量优先；指向不存在的文件则不回退（显式配置错误应暴露）。
    #[test]
    fn env_override_wins() {
        let exe = Path::new("C:/app/snow-shot.exe");
        let hit = find_recorder_exe(Some("D:/x/rec.exe"), exe, fs(&["D:/x/rec.exe"]));
        assert_eq!(hit, Some(PathBuf::from("D:/x/rec.exe")));
        assert_eq!(find_recorder_exe(Some("D:/none.exe"), exe, fs(&[])), None);
    }

    /// 同目录优先于开发布局。
    #[test]
    fn beside_exe_preferred() {
        let exe = Path::new("C:/app/snow-shot.exe");
        let beside = format!("C:/app/{RECORDER_EXE_NAME}");
        let dev = format!("C:/tools/snow-recorder/target/release/{RECORDER_EXE_NAME}");
        let hit = find_recorder_exe(None, exe, fs(&[&beside, &dev]));
        assert_eq!(hit, Some(PathBuf::from(beside)));
    }

    /// 开发布局：沿目录向上寻找 tools/snow-recorder/target/release。
    #[test]
    fn dev_layout_found_via_ancestors() {
        let exe = Path::new("C:/ws/snow-shot-rs/target/debug/snow-shot.exe");
        let dev =
            format!("C:/ws/snow-shot-rs/tools/snow-recorder/target/release/{RECORDER_EXE_NAME}");
        let hit = find_recorder_exe(None, exe, fs(&[&dev]));
        assert_eq!(hit, Some(PathBuf::from(dev)));
        // 仓库根下的 snow-shot-rs 子目录布局（构建目录在仓库根的 build/ 下时）
        let exe2 = Path::new("C:/repo/build/cargo/debug/snow-shot.exe");
        let dev2 =
            format!("C:/repo/snow-shot-rs/tools/snow-recorder/target/release/{RECORDER_EXE_NAME}");
        assert_eq!(
            find_recorder_exe(None, exe2, fs(&[&dev2])),
            Some(PathBuf::from(dev2))
        );
        assert_eq!(find_recorder_exe(None, exe, fs(&[])), None);
    }

    /// 启动不存在的可执行文件返回错误而非 panic。
    #[test]
    fn spawn_missing_exe_errors() {
        let result =
            ProcessRecorderLink::spawn(Path::new("Z:/definitely/missing.exe"), Arc::new(|| {}));
        assert!(result.is_err());
    }
}
