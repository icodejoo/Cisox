//! 语音转文字工作进程客户端：定位并拉起 `snow-stt`，按行协议通信。
//!
//! 写法沿用录制进程客户端：读线程把 stdout 事件转发到通道并唤醒主线程；
//! stderr 由独立线程排空（否则日志写满管道会卡死子进程）；结束进程只动自己持有的子进程句柄，
//! 绝不按进程名查杀。

use super::engine::{LinkEvent, SttLink};
use snow_stt_protocol::{Command, Event};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Stdio};
use std::sync::Arc;
use std::sync::mpsc::{Receiver, Sender, channel};
use std::time::{Duration, Instant};

/// 工作进程可执行文件名。
#[cfg(windows)]
pub const STT_EXE_NAME: &str = "snow-stt.exe";
/// 工作进程可执行文件名。
#[cfg(not(windows))]
pub const STT_EXE_NAME: &str = "snow-stt";
/// 指定工作进程路径的环境变量（优先级最高）。
pub const ENV_STT_EXE: &str = "SNOW_STT_EXE";
/// 开发布局下相对仓库根的构建产物目录（`scripts/build-snow-stt.ps1` 的默认输出）。
const DEV_BUILD_DIRS: [[&str; 3]; 2] = [["build", "stt", "release"], ["build", "stt", "debug"]];
/// 开发布局下相对仓库根的另一种位置（工具工作区自己的 target）。
const DEV_TOOL_DIR: [&str; 5] = ["snow-shot-rs", "tools", "snow-stt", "target", "release"];
/// 轮询子进程退出的间隔。
const EXIT_POLL_INTERVAL: Duration = Duration::from_millis(20);
/// 读线程见到 EOF 后等待退出码的最长时间。
const EXIT_CODE_WAIT: Duration = Duration::from_millis(500);
/// Windows：不为子进程创建控制台窗口。
#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// 查找工作进程可执行文件。
///
/// 顺序：环境变量（指向不存在的文件不再回退，显式配置错误要暴露）→ 与主程序同目录 →
/// 沿主程序目录逐级向上的开发布局（`build/stt/{release,debug}/`、`snow-shot-rs/tools/snow-stt/target/release/`）。
///
/// # 参数
/// - `env_value`：环境变量 [`ENV_STT_EXE`] 的值。
/// - `current_exe`：主程序路径。
/// - `exists`：文件存在性判断（便于测试）。
///
/// # 返回
/// 找到的路径；都不存在返回 `None`。
///
/// ```ignore
/// let exe = find_stt_exe(None, Path::new("C:/app/snow-shot.exe"), |p| p.exists());
/// ```
pub fn find_stt_exe(
    env_value: Option<&str>,
    current_exe: &Path,
    exists: impl Fn(&Path) -> bool,
) -> Option<PathBuf> {
    if let Some(text) = env_value.map(str::trim).filter(|t| !t.is_empty()) {
        let path = PathBuf::from(text);
        return exists(&path).then_some(path);
    }
    let dir = current_exe.parent()?;
    let beside = dir.join(STT_EXE_NAME);
    if exists(&beside) {
        return Some(beside);
    }
    for ancestor in dir.ancestors() {
        let mut candidates: Vec<PathBuf> = DEV_BUILD_DIRS
            .iter()
            .map(|parts| {
                let mut p = ancestor.to_path_buf();
                p.extend(parts);
                p
            })
            .collect();
        let mut tool = ancestor.to_path_buf();
        tool.extend(DEV_TOOL_DIR);
        candidates.push(tool);
        for mut candidate in candidates {
            candidate.push(STT_EXE_NAME);
            if exists(&candidate) {
                return Some(candidate);
            }
        }
    }
    None
}

/// 按当前进程环境查找工作进程。
///
/// # 返回
/// 路径；找不到返回 `None`。
pub fn locate_stt_exe() -> Option<PathBuf> {
    let env_value = std::env::var(ENV_STT_EXE).ok();
    let current = std::env::current_exe().ok()?;
    find_stt_exe(env_value.as_deref(), &current, Path::exists)
}

/// 真实的工作进程通道。
pub struct ProcessSttLink {
    /// 子进程句柄（结束进程只通过它）。
    child: Child,
    /// 子进程标准输入；关闭它等价于通知 worker 中止。
    stdin: Option<ChildStdin>,
    /// 读线程转发的事件。
    events: Receiver<LinkEvent>,
    /// 是否已收尾。
    closed: bool,
}

impl ProcessSttLink {
    /// 拉起工作进程。
    ///
    /// # 参数
    /// - `exe`：工作进程可执行文件。
    /// - `wake`：有新事件时调用（须线程安全，通常是向主线程收件箱投递一条轮询事件）。
    ///
    /// # 返回
    /// 通道；进程无法启动返回错误说明。
    ///
    /// ```ignore
    /// let link = ProcessSttLink::spawn(&exe, Arc::new(|| inbox.push(UiEvent::DictationPoll)))?;
    /// ```
    pub fn spawn(exe: &Path, wake: Arc<dyn Fn() + Send + Sync>) -> Result<Self, String> {
        Self::spawn_with_args(exe, &[], wake)
    }

    /// 带额外命令行参数拉起（联调测试用，例如 `--wav a.wav` 让 worker 用 wav 代替麦克风）。
    ///
    /// # 参数
    /// - `exe`：工作进程可执行文件。
    /// - `args`：额外命令行参数。
    /// - `wake`：有新事件时调用。
    pub fn spawn_with_args(
        exe: &Path,
        args: &[String],
        wake: Arc<dyn Fn() + Send + Sync>,
    ) -> Result<Self, String> {
        let mut command = std::process::Command::new(exe);
        command
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        // 工作进程的 DLL 与 exe 同目录；把工作目录设到那里，避免依赖调用方的当前目录
        if let Some(dir) = exe.parent() {
            command.current_dir(dir);
        }
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            command.creation_flags(CREATE_NO_WINDOW);
        }
        let mut child = command
            .spawn()
            .map_err(|e| format!("{}: {e}", exe.display()))?;
        let stdin = child.stdin.take();
        let (tx, rx) = channel();
        if let Some(stdout) = child.stdout.take() {
            std::thread::spawn(move || read_events(stdout, tx, wake));
        }
        if let Some(stderr) = child.stderr.take() {
            std::thread::spawn(move || drain_stderr(stderr));
        }
        tracing::info!(pid = child.id(), exe = %exe.display(), "语音转文字进程已启动");
        Ok(Self {
            child,
            stdin,
            events: rx,
            closed: false,
        })
    }

    /// 等待子进程退出并取得退出码（最多 `limit`）；仍在运行返回 `None`。
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
            Err(e) => tracing::warn!(line = %line, error = %e, "语音转文字进程输出无法解析"),
        }
    }
    let _ = tx.send(LinkEvent::Exited { code: None });
    wake();
}

/// stderr 排空线程：内容只进 debug 日志。
fn drain_stderr(stderr: impl std::io::Read) {
    for line in BufReader::new(stderr).lines() {
        let Ok(line) = line else { break };
        tracing::debug!(target: "snow_stt", "{line}");
    }
}

impl SttLink for ProcessSttLink {
    /// 写一行命令到子进程 stdin。
    fn send(&mut self, command: &Command) -> Result<(), String> {
        let stdin = self.stdin.as_mut().ok_or("语音转文字进程输入已关闭")?;
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

    /// 关闭 stdin（worker 视为中止）→ 宽限期内等待退出 → 仍在运行则按自己的句柄强制结束。
    fn shutdown(&mut self, grace: Duration) {
        if self.closed {
            return;
        }
        self.closed = true;
        drop(self.stdin.take());
        if self.wait_exit_code(grace).is_none() && matches!(self.child.try_wait(), Ok(None)) {
            tracing::warn!(
                pid = self.child.id(),
                "语音转文字进程未在宽限期内退出，强制结束"
            );
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }
}

impl Drop for ProcessSttLink {
    /// 丢弃通道时确保子进程不会遗留。
    fn drop(&mut self) {
        self.shutdown(Duration::ZERO);
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

    /// 环境变量优先；指向不存在的文件则不回退。
    #[test]
    fn env_override_wins() {
        let exe = Path::new("C:/app/snow-shot.exe");
        let hit = find_stt_exe(Some("D:/x/stt.exe"), exe, fs(&["D:/x/stt.exe"]));
        assert_eq!(hit, Some(PathBuf::from("D:/x/stt.exe")));
        assert_eq!(find_stt_exe(Some("D:/none.exe"), exe, fs(&[])), None);
        assert_eq!(find_stt_exe(Some("  "), exe, fs(&[])), None);
    }

    /// 同目录优先于开发布局。
    #[test]
    fn beside_exe_preferred() {
        let exe = Path::new("C:/app/snow-shot.exe");
        let beside = format!("C:/app/{STT_EXE_NAME}");
        let dev = format!("C:/build/stt/release/{STT_EXE_NAME}");
        assert_eq!(
            find_stt_exe(None, exe, fs(&[&beside, &dev])),
            Some(PathBuf::from(beside))
        );
    }

    /// 开发布局：沿目录向上找 build/stt/release，其次工具自己的 target。
    #[test]
    fn dev_layout_found_via_ancestors() {
        let exe = Path::new("C:/repo/build/cargo/debug/snow-shot.exe");
        let built = format!("C:/repo/build/stt/release/{STT_EXE_NAME}");
        assert_eq!(
            find_stt_exe(None, exe, fs(&[&built])),
            Some(PathBuf::from(built))
        );
        let tool = format!("C:/repo/snow-shot-rs/tools/snow-stt/target/release/{STT_EXE_NAME}");
        assert_eq!(
            find_stt_exe(None, exe, fs(&[&tool])),
            Some(PathBuf::from(tool))
        );
        assert_eq!(find_stt_exe(None, exe, fs(&[])), None);
    }

    /// 真实 worker 联调（默认忽略）：用 `--wav` 代替麦克风，经引擎跑完整一轮，检查事件序列与文本累积。
    ///
    /// 环境变量：`SNOW_STT_EXE`（工作进程）、`SNOW_STT_TEST_MODEL_DIR`（模型目录）、`SNOW_STT_TEST_WAV`（16k 单声道 wav）。
    /// 运行：`cargo test -p snow-shot real_worker_wav_session -- --ignored --nocapture`。
    #[test]
    #[ignore = "需要真实 snow-stt、模型与 wav，见文档注释里的环境变量"]
    fn real_worker_wav_session() {
        use crate::dictation::engine::{Effect, Engine, Launch};
        use crate::dictation::overlay_model::OverlayModel;
        use crate::dictation::text::Transcript;
        use snow_stt_protocol::{EndpointRules, StartRequest};

        let var =
            |name: &str| std::env::var(name).unwrap_or_else(|_| panic!("缺少环境变量 {name}"));
        let exe = PathBuf::from(var(ENV_STT_EXE));
        let request = StartRequest {
            language: "auto".into(),
            threads: 2,
            endpoint: EndpointRules::default(),
            max_seconds: 0,
            model_dir: var("SNOW_STT_TEST_MODEL_DIR"),
        };
        let args = vec![
            "--wav".to_string(),
            var("SNOW_STT_TEST_WAV"),
            "--wav-pad-ms".to_string(),
            "3000".to_string(),
        ];
        let link =
            ProcessSttLink::spawn_with_args(&exe, &args, Arc::new(|| {})).expect("拉起 snow-stt");
        let mut engine = Engine::default();
        let begin = Instant::now();
        let mut effects = engine.start(
            begin,
            Ok(Launch {
                link: Box::new(link),
                request,
            }),
        );
        let mut transcript = Transcript::default();
        let mut overlay = OverlayModel::default();
        let mut log: Vec<String> = Vec::new();
        let mut ended = None;
        while ended.is_none() && begin.elapsed() < Duration::from_secs(180) {
            for effect in effects.drain(..) {
                log.push(format!("{:>6}ms {effect:?}", begin.elapsed().as_millis()));
                match effect {
                    Effect::Partial(t) => {
                        transcript.set_partial(&t);
                        overlay.push_partial(&t);
                    }
                    Effect::Final(t) => {
                        transcript.push_final(&t);
                        overlay.push_final(&t);
                    }
                    Effect::Done | Effect::Failed(_) => ended = Some(effect),
                    _ => {}
                }
            }
            if ended.is_none() {
                std::thread::sleep(Duration::from_millis(50));
                effects = engine.poll(Instant::now());
            }
        }
        println!("{}", log.join("\n"));
        println!("全文: {}", transcript.full());
        assert_eq!(ended, Some(Effect::Done), "应正常结束");
        assert!(!engine.active(), "结束后进程应已释放");
        assert!(
            log.iter().any(|l| l.contains("Listening")),
            "应出现“正在听”"
        );
        assert!(!transcript.finals().is_empty(), "应有落定文本");
        let shown = overlay.take_text("").unwrap_or_default();
        assert_eq!(shown, transcript.finals(), "浮窗文本区内容应与落定文本一致");
    }

    /// 启动不存在的可执行文件返回错误而非 panic。
    #[test]
    fn spawn_missing_exe_errors() {
        assert!(
            ProcessSttLink::spawn(Path::new("Z:/definitely/missing.exe"), Arc::new(|| {})).is_err()
        );
    }
}
