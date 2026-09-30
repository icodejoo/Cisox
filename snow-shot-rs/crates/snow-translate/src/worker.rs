//! 本地 NMT 工作进程客户端：按需拉起、串行请求、空闲卸载、崩溃/卡死复位。
//!
//! 一个 [`WorkerEngine`] 绑定一个模型目录与一组解码参数。第一次翻译时才拉起
//! `snow-translator` 子进程（等待其 `ready` 行）并加载模型；同一时刻只有一个请求在飞
//! （内部互斥），空闲超过 `idle_timeout` 后发 `unload` 让进程退出，内存全部回收。
//! 进程崩溃、卡死（超时）、协议版本不符都会得到明确的 [`TranslateError`]，并复位到“未启动”，
//! 下一次请求重新拉起；复用中的进程中途死亡时自动重启并重试一次。

use crate::protocol::{
    Command, ErrorKind, Event, MAX_BEAMS, PROTOCOL_VERSION, decode_event, encode_command,
};
use crate::{Lang, TranslateError, TranslationEngine};
use std::io::{self, BufRead, BufReader, Read, Write};
use std::path::PathBuf;
use std::process::{Child, ChildStdin, Command as ProcessCommand, Stdio};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::thread;
use std::time::{Duration, Instant};

/// 环境变量：交给 worker 的 onnxruntime 动态库路径。
pub const ENV_ORT_DYLIB: &str = "SNOW_ORT_DYLIB";
/// worker 可执行文件名。
pub const WORKER_EXE_NAME: &str = "snow-translator.exe";
/// 单行协议消息上限（超过则丢弃，防止异常输出撑爆内存）。
const MAX_LINE_BYTES: usize = 4 * 1024 * 1024;
/// 单次翻译请求携带的文本字节数上限（worker 单行上限 1 MiB，留足 JSON 转义余量）。
pub const MAX_REQUEST_BYTES: usize = 200_000;
/// 单次翻译请求携带的文本条数上限（请求越短，卡死检测越及时，也为将来的进度回报留出粒度）。
pub const MAX_REQUEST_TEXTS: usize = 16;
/// stderr 只保留末尾这么多字节用于诊断。
const STDERR_TAIL_BYTES: usize = 2048;
/// 等待未识别的杂行时最多忽略的行数。
const MAX_IGNORED_LINES: usize = 200;
/// 空闲监视线程的最短轮询间隔。
const MONITOR_MIN_TICK: Duration = Duration::from_millis(10);
/// 空闲监视线程的最长轮询间隔。
const MONITOR_MAX_TICK: Duration = Duration::from_millis(500);
/// 心跳应答等待时间。
const PING_TIMEOUT: Duration = Duration::from_secs(5);
/// 进程退出轮询间隔。
const EXIT_POLL: Duration = Duration::from_millis(10);
/// 读取退出原因时等待 stderr 收尾的时间。
const DIAGNOSTIC_SETTLE: Duration = Duration::from_millis(300);
/// Windows 不弹控制台窗口的进程创建标志。
#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// 各阶段超时。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Timeouts {
    /// 等待 `ready`。
    pub ready: Duration,
    /// 等待模型加载完成。
    pub load: Duration,
    /// 单个翻译请求的基础超时。
    pub request_base: Duration,
    /// 每个字符追加的超时。
    pub request_per_char: Duration,
    /// 单个翻译请求超时上限。
    pub request_max: Duration,
    /// 卸载时等待进程自行退出的宽限，超过则强杀。
    pub unload_grace: Duration,
}

impl Default for Timeouts {
    /// 生产默认值：冷启动与杀软扫描留足余量，翻译按文本长度放宽。
    fn default() -> Self {
        Self {
            ready: Duration::from_secs(20),
            load: Duration::from_secs(180),
            request_base: Duration::from_secs(30),
            request_per_char: Duration::from_millis(30),
            request_max: Duration::from_secs(900),
            unload_grace: Duration::from_secs(3),
        }
    }
}

impl Timeouts {
    /// 按文本总字符数计算单个翻译请求的超时。
    ///
    /// # 参数
    /// - `chars`：请求里所有文本的字符总数。
    ///
    /// # 示例
    /// ```
    /// use snow_translate::worker::Timeouts;
    /// let t = Timeouts::default();
    /// assert!(t.request_for(1000) > t.request_for(10));
    /// assert!(t.request_for(usize::MAX / 2) <= t.request_max);
    /// ```
    pub fn request_for(&self, chars: usize) -> Duration {
        let extra = self
            .request_per_char
            .checked_mul(u32::try_from(chars).unwrap_or(u32::MAX))
            .unwrap_or(self.request_max);
        self.request_base.saturating_add(extra).min(self.request_max)
    }
}

/// 本地 NMT 引擎配置。
#[derive(Debug, Clone)]
pub struct WorkerConfig {
    /// `snow-translator` 可执行文件。
    pub exe: PathBuf,
    /// 模型目录（含 `model.json`）。
    pub model_dir: PathBuf,
    /// 模型 ID（缓存标识用）。
    pub model_id: String,
    /// 模型支持的语言对（用于拒绝不支持的请求与解析 `Auto`）。
    pub pairs: Vec<(Lang, Lang)>,
    /// onnxruntime 动态库路径，经 `SNOW_ORT_DYLIB` 交给 worker；`None` 让 worker 自行查找。
    pub ort_dylib: Option<PathBuf>,
    /// 束宽，`1..=8`，1 即贪心。
    pub num_beams: usize,
    /// 请求后收缩内存（低内存模式）。
    pub trim_after_request: bool,
    /// 空闲多久后卸载（进程退出）。
    pub idle_timeout: Duration,
    /// 各阶段超时。
    pub timeouts: Timeouts,
}

/// 一次接收的结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Recv {
    /// 收到一行。
    Line(String),
    /// 超时，进程仍可能在运行。
    Timeout,
    /// 对端已关闭（进程退出）。
    Closed,
}

/// 与一个 worker 进程通信的通道（真实实现是子进程，测试里用内存假实现）。
pub trait Transport: Send {
    /// 发送一行（实现负责补换行并刷新）。
    fn send(&mut self, line: &str) -> io::Result<()>;
    /// 接收一行，最多等 `timeout`。
    fn recv(&mut self, timeout: Duration) -> Recv;
    /// 进程失败时的诊断摘要（退出码与 stderr 末尾）。
    fn diagnostics(&mut self) -> String;
    /// 强制结束并回收进程。
    fn terminate(&mut self);
    /// 等待进程自行退出，返回是否在 `timeout` 内退出。
    fn wait_exit(&mut self, timeout: Duration) -> bool;
}

/// worker 的拉起方式（便于测试注入假 worker）。
pub trait WorkerLauncher: Send + Sync {
    /// 拉起 worker 并返回通信通道（不等待 `ready`）。
    ///
    /// # 参数
    /// - `config`：引擎配置。
    fn launch(&self, config: &WorkerConfig) -> Result<Box<dyn Transport>, TranslateError>;
}

/// 真实子进程拉起。
pub struct ProcessLauncher;

impl WorkerLauncher for ProcessLauncher {
    /// 启动 `snow-translator` 子进程：管道 stdin/stdout/stderr，不弹控制台窗口。
    fn launch(&self, config: &WorkerConfig) -> Result<Box<dyn Transport>, TranslateError> {
        if !config.exe.is_file() {
            return Err(TranslateError::WorkerUnavailable(format!(
                "未找到翻译组件 {}",
                config.exe.display()
            )));
        }
        let mut command = ProcessCommand::new(&config.exe);
        command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        if let Some(dylib) = &config.ort_dylib {
            command.env(ENV_ORT_DYLIB, dylib);
        }
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            command.creation_flags(CREATE_NO_WINDOW);
        }
        let mut child = command.spawn().map_err(|e| {
            TranslateError::WorkerUnavailable(format!("无法启动 {}: {e}", config.exe.display()))
        })?;
        let (stdin, stdout, stderr) = match (child.stdin.take(), child.stdout.take(), child.stderr.take()) {
            (Some(i), Some(o), Some(e)) => (i, o, e),
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(TranslateError::WorkerUnavailable("无法接管子进程管道".into()));
            }
        };
        let (tx, rx) = mpsc::channel::<String>();
        thread::Builder::new()
            .name("snow-translate-stdout".into())
            .spawn(move || {
                let mut reader = BufReader::new(stdout);
                while let Ok(Some(line)) = read_line_bounded(&mut reader, MAX_LINE_BYTES) {
                    if tx.send(line).is_err() {
                        break;
                    }
                }
            })
            .map_err(|e| TranslateError::WorkerUnavailable(format!("无法创建读线程: {e}")))?;
        let tail = Arc::new(Mutex::new(String::new()));
        let tail_writer = Arc::clone(&tail);
        let _ = thread::Builder::new()
            .name("snow-translate-stderr".into())
            .spawn(move || pump_stderr(stderr, &tail_writer));
        Ok(Box::new(ProcessTransport {
            child,
            stdin: Some(stdin),
            lines: rx,
            stderr_tail: tail,
        }))
    }
}

/// 读一行（不含换行）；超过 `limit` 的行被丢弃并返回空串，EOF 返回 `None`。
///
/// # 参数
/// - `reader`：带缓冲输入。
/// - `limit`：单行最大字节数。
///
/// # 返回
/// `Some(行)`、`None`（输入结束）或 IO 错误；非 UTF-8 内容按有损转换。
///
/// # 示例
/// ```
/// use snow_translate::worker::read_line_bounded;
/// let mut input = std::io::Cursor::new(b"abc\r\n".to_vec());
/// assert_eq!(read_line_bounded(&mut input, 16).unwrap(), Some("abc".to_string()));
/// assert_eq!(read_line_bounded(&mut input, 16).unwrap(), None);
/// ```
pub fn read_line_bounded(reader: &mut impl BufRead, limit: usize) -> io::Result<Option<String>> {
    let mut buf = Vec::new();
    let read = reader.by_ref().take(limit as u64 + 1).read_until(b'\n', &mut buf)?;
    if read == 0 {
        return Ok(None);
    }
    let complete = buf.last() == Some(&b'\n');
    if !complete && buf.len() > limit {
        // 超长行：丢弃到行尾
        let mut sink = Vec::new();
        loop {
            sink.clear();
            let n = reader.by_ref().take(64 * 1024).read_until(b'\n', &mut sink)?;
            if n == 0 || sink.last() == Some(&b'\n') {
                break;
            }
        }
        return Ok(Some(String::new()));
    }
    while matches!(buf.last(), Some(b'\n' | b'\r')) {
        buf.pop();
    }
    if buf.len() > limit {
        return Ok(Some(String::new()));
    }
    Ok(Some(String::from_utf8_lossy(&buf).into_owned()))
}

/// 持续读 stderr，只保留末尾 [`STDERR_TAIL_BYTES`] 字节。
fn pump_stderr(mut stderr: impl Read, tail: &Mutex<String>) {
    let mut chunk = [0u8; 1024];
    while let Ok(n) = stderr.read(&mut chunk) {
        if n == 0 {
            break;
        }
        let text = String::from_utf8_lossy(&chunk[..n]).into_owned();
        let mut guard = tail.lock().unwrap_or_else(PoisonError::into_inner);
        guard.push_str(&text);
        if guard.len() > STDERR_TAIL_BYTES {
            let mut cut = guard.len() - STDERR_TAIL_BYTES;
            while !guard.is_char_boundary(cut) {
                cut += 1;
            }
            guard.drain(..cut);
        }
    }
}

/// 子进程通道。
struct ProcessTransport {
    /// 子进程。
    child: Child,
    /// 子进程 stdin（关闭即通知 worker 退出）。
    stdin: Option<ChildStdin>,
    /// stdout 行通道。
    lines: Receiver<String>,
    /// stderr 末尾内容。
    stderr_tail: Arc<Mutex<String>>,
}

impl Transport for ProcessTransport {
    /// 写一行并刷新。
    fn send(&mut self, line: &str) -> io::Result<()> {
        let stdin = self
            .stdin
            .as_mut()
            .ok_or_else(|| io::Error::new(io::ErrorKind::BrokenPipe, "stdin 已关闭"))?;
        stdin.write_all(line.as_bytes())?;
        stdin.write_all(b"\n")?;
        stdin.flush()
    }

    /// 从 stdout 行通道取一行。
    fn recv(&mut self, timeout: Duration) -> Recv {
        match self.lines.recv_timeout(timeout) {
            Ok(line) => Recv::Line(line),
            Err(RecvTimeoutError::Timeout) => Recv::Timeout,
            Err(RecvTimeoutError::Disconnected) => Recv::Closed,
        }
    }

    /// 退出码 + stderr 末尾。
    fn diagnostics(&mut self) -> String {
        let deadline = Instant::now() + DIAGNOSTIC_SETTLE;
        let status = loop {
            match self.child.try_wait() {
                Ok(Some(status)) => break Some(status.to_string()),
                Ok(None) if Instant::now() < deadline => thread::sleep(EXIT_POLL),
                _ => break None,
            }
        };
        thread::sleep(EXIT_POLL);
        let tail = self.stderr_tail.lock().unwrap_or_else(PoisonError::into_inner).trim().to_string();
        match (status, tail.is_empty()) {
            (Some(s), true) => s,
            (Some(s), false) => format!("{s}; stderr: {tail}"),
            (None, true) => "进程无响应".to_string(),
            (None, false) => format!("stderr: {tail}"),
        }
    }

    /// 关 stdin、强杀并回收。
    fn terminate(&mut self) {
        self.stdin = None;
        let _ = self.child.kill();
        let _ = self.child.wait();
    }

    /// 轮询等待退出。
    fn wait_exit(&mut self, timeout: Duration) -> bool {
        let deadline = Instant::now() + timeout;
        loop {
            match self.child.try_wait() {
                Ok(Some(_)) | Err(_) => return true,
                Ok(None) if Instant::now() >= deadline => return false,
                Ok(None) => thread::sleep(EXIT_POLL),
            }
        }
    }
}

/// 一个已拉起的 worker 会话。
struct Session {
    /// 通信通道。
    transport: Box<dyn Transport>,
    /// 已加载的语言对。
    loaded: Option<(Lang, Lang)>,
    /// worker 进程 ID。
    pid: u32,
}

/// 互斥保护的可变状态。
struct Inner {
    /// 当前会话（未启动、空闲卸载或出错后为空）。
    session: Option<Session>,
    /// 最近一次使用时间。
    last_used: Instant,
    /// 空闲监视线程是否在运行。
    monitor_running: bool,
    /// 下一个请求编号。
    next_id: u64,
}

/// 引擎与监视线程共享的部分。
struct Shared {
    /// 配置。
    config: WorkerConfig,
    /// 拉起方式。
    launcher: Arc<dyn WorkerLauncher>,
    /// 可变状态（持锁期间同一时刻只有一个请求）。
    inner: Mutex<Inner>,
    /// 累计拉起次数（探针与测试用）。
    launches: AtomicU32,
}

/// 加锁并忽略中毒（一次线程 panic 不应让翻译永久失效）。
fn lock(inner: &Mutex<Inner>) -> MutexGuard<'_, Inner> {
    inner.lock().unwrap_or_else(PoisonError::into_inner)
}

/// 内存快照（探针用）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MemorySnapshot {
    /// worker 进程 ID。
    pub pid: u32,
    /// 当前工作集（字节）。
    pub mem_bytes: u64,
    /// 峰值工作集（字节）。
    pub peak_bytes: u64,
}

/// 本地 NMT 翻译引擎（独立 worker 进程）。
pub struct WorkerEngine {
    /// 共享状态。
    shared: Arc<Shared>,
}

impl WorkerEngine {
    /// 用真实子进程创建引擎（此时不拉起进程，首次翻译才拉起）。
    ///
    /// # 参数
    /// - `config`：引擎配置。
    ///
    /// # 示例
    /// ```no_run
    /// use snow_translate::worker::{Timeouts, WorkerConfig, WorkerEngine};
    /// use snow_translate::{Lang, TranslationEngine};
    /// let engine = WorkerEngine::new(WorkerConfig {
    ///     exe: "snow-translator.exe".into(),
    ///     model_dir: "D:/models/translate/opus-mt-en-zh-int8".into(),
    ///     model_id: "opus-mt-en-zh-int8".into(),
    ///     pairs: vec![(Lang::En, Lang::ZhHans)],
    ///     ort_dylib: None,
    ///     num_beams: 4,
    ///     trim_after_request: true,
    ///     idle_timeout: std::time::Duration::from_secs(120),
    ///     timeouts: Timeouts::default(),
    /// });
    /// let text = engine.translate("Hello", Lang::En, Lang::ZhHans);
    /// ```
    pub fn new(config: WorkerConfig) -> Self {
        Self::with_launcher(config, Arc::new(ProcessLauncher))
    }

    /// 用自定义拉起方式创建引擎（测试注入假 worker）。
    ///
    /// # 参数
    /// - `config`：引擎配置（束宽会被夹到 `1..=8`）。
    /// - `launcher`：拉起方式。
    pub fn with_launcher(mut config: WorkerConfig, launcher: Arc<dyn WorkerLauncher>) -> Self {
        config.num_beams = config.num_beams.clamp(1, MAX_BEAMS);
        Self {
            shared: Arc::new(Shared {
                config,
                launcher,
                inner: Mutex::new(Inner {
                    session: None,
                    last_used: Instant::now(),
                    monitor_running: false,
                    next_id: 1,
                }),
                launches: AtomicU32::new(0),
            }),
        }
    }

    /// worker 进程当前是否在运行。
    pub fn is_running(&self) -> bool {
        lock(&self.shared.inner).session.is_some()
    }

    /// 累计拉起 worker 的次数。
    pub fn launch_count(&self) -> u32 {
        self.shared.launches.load(Ordering::SeqCst)
    }

    /// 向运行中的 worker 取内存快照（不刷新空闲计时）；未运行返回 `None`。
    pub fn memory_snapshot(&self) -> Option<MemorySnapshot> {
        let mut inner = lock(&self.shared.inner);
        let session = inner.session.as_mut()?;
        let pid = session.pid;
        session.transport.send(&encode_command(&Command::Ping).ok()?).ok()?;
        let deadline = Instant::now() + PING_TIMEOUT;
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            match session.transport.recv(remaining) {
                Recv::Line(line) => {
                    if let Ok(Event::Pong { mem_bytes, peak_bytes, .. }) = decode_event(&line) {
                        return Some(MemorySnapshot { pid, mem_bytes, peak_bytes });
                    }
                }
                Recv::Timeout | Recv::Closed => return None,
            }
        }
    }

    /// 立即卸载并结束 worker（应用退出或切换配置时调用）。
    pub fn shutdown(&self) {
        let mut inner = lock(&self.shared.inner);
        self.shared.unload_session(&mut inner);
    }
}

impl Drop for WorkerEngine {
    /// 引擎释放时结束 worker，避免遗留进程。
    fn drop(&mut self) {
        self.shutdown();
    }
}

/// 把 worker 的错误事件映射成宿主错误。
///
/// # 参数
/// - `kind`：错误类别。
/// - `message`：worker 给出的原因。
/// - `src` / `tgt`：本次语言对。
fn map_worker_error(kind: ErrorKind, message: String, src: Lang, tgt: Lang) -> TranslateError {
    match kind {
        ErrorKind::RuntimeMissing => TranslateError::RuntimeMissing(message),
        ErrorKind::ModelMissing
        | ErrorKind::ManifestInvalid
        | ErrorKind::LoadFailed
        | ErrorKind::ChecksumMismatch => TranslateError::ModelLoad(message),
        ErrorKind::UnsupportedPair => TranslateError::UnsupportedLanguagePair(src, tgt),
        ErrorKind::OutOfMemory => TranslateError::OutOfMemory(message),
        ErrorKind::DecodeFailed | ErrorKind::BadRequest | ErrorKind::NotLoaded => {
            TranslateError::Inference(message)
        }
    }
}

impl Shared {
    /// 结束当前会话：先礼貌卸载，超过宽限则强杀。
    fn unload_session(&self, inner: &mut Inner) {
        let Some(mut session) = inner.session.take() else {
            return;
        };
        if let Ok(line) = encode_command(&Command::Unload)
            && session.transport.send(&line).is_ok()
        {
            let grace = self.config.timeouts.unload_grace;
            if session.transport.wait_exit(grace) {
                session.transport.terminate();
                return;
            }
        }
        session.transport.terminate();
    }

    /// 等待满足条件的事件；杂行被忽略；进程关闭返回 `WorkerDied`，超时返回 `Timeout`。
    fn wait_event<T>(
        &self,
        session: &mut Session,
        timeout: Duration,
        mut pick: impl FnMut(Event) -> Option<Result<T, TranslateError>>,
    ) -> Result<T, TranslateError> {
        let deadline = Instant::now() + timeout;
        let mut ignored = 0usize;
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            match session.transport.recv(remaining) {
                Recv::Line(line) => match decode_event(&line) {
                    Ok(event) => {
                        if let Some(outcome) = pick(event) {
                            return outcome;
                        }
                    }
                    Err(reason) => {
                        ignored += 1;
                        tracing::debug!(%reason, "忽略无法解析的 worker 输出");
                        if ignored > MAX_IGNORED_LINES {
                            return Err(TranslateError::WorkerDied("worker 输出无法解析".into()));
                        }
                    }
                },
                Recv::Timeout => return Err(TranslateError::Timeout),
                Recv::Closed => {
                    return Err(TranslateError::WorkerDied(session.transport.diagnostics()));
                }
            }
        }
    }

    /// 拉起 worker 并等待 `ready`。
    fn start_session(&self) -> Result<Session, TranslateError> {
        let transport = self.launcher.launch(&self.config)?;
        self.launches.fetch_add(1, Ordering::SeqCst);
        let mut session = Session { transport, loaded: None, pid: 0 };
        let ready = self.wait_event(&mut session, self.config.timeouts.ready, |event| match event {
            Event::Ready { protocol, pid } if protocol == PROTOCOL_VERSION => Some(Ok(pid)),
            Event::Ready { protocol, .. } => Some(Err(TranslateError::WorkerUnavailable(format!(
                "翻译组件协议版本不符（组件 {protocol}，主程序 {PROTOCOL_VERSION}）"
            )))),
            _ => None,
        });
        match ready {
            Ok(pid) => {
                session.pid = pid;
                Ok(session)
            }
            Err(e) => {
                session.transport.terminate();
                Err(match e {
                    TranslateError::Timeout => {
                        TranslateError::WorkerUnavailable("翻译组件启动超时（未收到 ready）".into())
                    }
                    other => other,
                })
            }
        }
    }

    /// 确保已加载所需语言对（换语言对会让 worker 重新加载）。
    fn ensure_loaded(&self, session: &mut Session, src: Lang, tgt: Lang) -> Result<(), TranslateError> {
        if session.loaded == Some((src, tgt)) {
            return Ok(());
        }
        let command = Command::Load {
            model_dir: self.config.model_dir.to_string_lossy().into_owned(),
            src: src.code().to_string(),
            tgt: tgt.code().to_string(),
            trim_after_request: self.config.trim_after_request.then_some(true),
        };
        let line = encode_command(&command).map_err(TranslateError::Inference)?;
        session
            .transport
            .send(&line)
            .map_err(|e| TranslateError::WorkerDied(format!("发送加载命令失败: {e}")))?;
        let outcome = self.wait_event(session, self.config.timeouts.load, |event| match event {
            Event::Loaded { load_ms, mem_bytes, .. } => {
                tracing::info!(load_ms, mem_bytes, "翻译模型已加载");
                Some(Ok(()))
            }
            Event::Error { kind, message, .. } => Some(Err(map_worker_error(kind, message, src, tgt))),
            _ => None,
        });
        if outcome.is_ok() {
            session.loaded = Some((src, tgt));
        }
        outcome
    }

    /// 发送一个翻译请求并等待对应编号的结果。
    fn request(
        &self,
        session: &mut Session,
        id: u64,
        texts: Vec<String>,
        src: Lang,
        tgt: Lang,
    ) -> Result<Vec<String>, TranslateError> {
        let chars: usize = texts.iter().map(|t| t.chars().count()).sum();
        let expected = texts.len();
        let command = Command::Translate {
            id,
            texts,
            num_beams: Some(self.config.num_beams),
        };
        let line = encode_command(&command).map_err(TranslateError::Inference)?;
        session
            .transport
            .send(&line)
            .map_err(|e| TranslateError::WorkerDied(format!("发送翻译请求失败: {e}")))?;
        let timeout = self.config.timeouts.request_for(chars);
        self.wait_event(session, timeout, |event| match event {
            Event::Result { id: got, texts, .. } if got == id => Some(if texts.len() == expected {
                Ok(texts)
            } else {
                Err(TranslateError::Inference("worker 返回的译文条数不符".into()))
            }),
            Event::Error { id: got, kind, message } if got.is_none() || got == Some(id) => {
                Some(Err(map_worker_error(kind, message, src, tgt)))
            }
            // 其它编号的结果是过期应答，忽略
            _ => None,
        })
    }

    /// 是否必须丢弃会话：进程已死、卡死、内存不足，或错误发生在加载阶段。
    fn is_fatal(error: &TranslateError) -> bool {
        matches!(
            error,
            TranslateError::WorkerDied(_)
                | TranslateError::Timeout
                | TranslateError::OutOfMemory(_)
                | TranslateError::RuntimeMissing(_)
                | TranslateError::ModelLoad(_)
                | TranslateError::WorkerUnavailable(_)
        )
    }

    /// 一次完整尝试：确保会话与模型 → 分块发请求。出现致命错误时丢弃会话。
    fn attempt(
        &self,
        inner: &mut Inner,
        chunks: &[Vec<String>],
        src: Lang,
        tgt: Lang,
    ) -> Result<Vec<String>, TranslateError> {
        if inner.session.is_none() {
            inner.session = Some(self.start_session()?);
        }
        let Some(mut session) = inner.session.take() else {
            return Err(TranslateError::WorkerDied("会话丢失".into()));
        };
        let mut out: Vec<String> = Vec::new();
        let mut result = self.ensure_loaded(&mut session, src, tgt);
        if result.is_ok() {
            for chunk in chunks {
                let id = inner.next_id;
                inner.next_id += 1;
                match self.request(&mut session, id, chunk.clone(), src, tgt) {
                    Ok(texts) => out.extend(texts),
                    Err(e) => {
                        result = Err(e);
                        break;
                    }
                }
            }
        }
        match result {
            Ok(()) => {
                inner.session = Some(session);
                Ok(out)
            }
            Err(e) if Self::is_fatal(&e) => {
                session.transport.terminate();
                Err(e)
            }
            Err(e) => {
                inner.session = Some(session);
                Err(e)
            }
        }
    }
}

/// 把文本按字节数与条数上限分块；单条超限返回 `InvalidRequest`。
///
/// # 参数
/// - `texts`：待翻译文本。
/// - `limit`：每块字节数上限。
///
/// # 返回
/// 分块结果（保持顺序），空输入返回空。
fn chunk_texts(texts: &[String], limit: usize) -> Result<Vec<Vec<String>>, TranslateError> {
    let mut chunks: Vec<Vec<String>> = Vec::new();
    let mut current: Vec<String> = Vec::new();
    let mut size = 0usize;
    for text in texts {
        if text.len() > limit {
            return Err(TranslateError::InvalidRequest(format!(
                "单段文本过长（{} 字节，上限 {limit}）",
                text.len()
            )));
        }
        if !current.is_empty() && (size + text.len() > limit || current.len() >= MAX_REQUEST_TEXTS) {
            chunks.push(std::mem::take(&mut current));
            size = 0;
        }
        size += text.len();
        current.push(text.clone());
    }
    if !current.is_empty() {
        chunks.push(current);
    }
    Ok(chunks)
}

/// 启动空闲监视线程：空闲超过阈值就卸载 worker，然后自己退出（不常驻）。
fn spawn_idle_monitor(shared: Arc<Shared>) {
    let idle = shared.config.idle_timeout;
    let tick = (idle / 4).clamp(MONITOR_MIN_TICK, MONITOR_MAX_TICK);
    let spawned = thread::Builder::new().name("snow-translate-idle".into()).spawn({
        let shared = Arc::clone(&shared);
        move || {
            loop {
                thread::sleep(tick);
                let mut inner = lock(&shared.inner);
                if inner.session.is_none() {
                    inner.monitor_running = false;
                    return;
                }
                if inner.last_used.elapsed() >= idle {
                    tracing::info!("翻译 worker 空闲超时，卸载");
                    shared.unload_session(&mut inner);
                    inner.monitor_running = false;
                    return;
                }
            }
        }
    });
    if let Err(e) = spawned {
        tracing::warn!(error = %e, "无法创建空闲监视线程，worker 将常驻直到下次关闭");
        lock(&shared.inner).monitor_running = false;
    }
}

impl TranslationEngine for WorkerEngine {
    /// 翻译单条文本（走批量路径）。
    fn translate(&self, text: &str, src: Lang, tgt: Lang) -> Result<String, TranslateError> {
        let mut out = self.translate_batch(&[text.to_string()], src, tgt)?;
        out.pop()
            .ok_or_else(|| TranslateError::Inference("worker 没有返回译文".into()))
    }

    /// 批量翻译：空白文本原样返回不占用 worker；其余分块串行发给 worker。
    ///
    /// 复用中的 worker 中途死亡时自动重启并重试一次；卡死（超时）不重试，直接报错并复位。
    fn translate_batch(&self, texts: &[String], src: Lang, tgt: Lang) -> Result<Vec<String>, TranslateError> {
        let resolved = if src == Lang::Auto {
            self.shared.config.pairs.iter().find(|(_, t)| *t == tgt).map(|(s, _)| *s)
        } else {
            self.shared.config.pairs.contains(&(src, tgt)).then_some(src)
        }
        .ok_or(TranslateError::UnsupportedLanguagePair(src, tgt))?;
        let work: Vec<usize> = (0..texts.len()).filter(|&i| !texts[i].trim().is_empty()).collect();
        let mut results: Vec<String> = texts.to_vec();
        if work.is_empty() {
            return Ok(results);
        }
        let payload: Vec<String> = work.iter().map(|&i| texts[i].clone()).collect();
        let chunks = chunk_texts(&payload, MAX_REQUEST_BYTES)?;
        let mut inner = lock(&self.shared.inner);
        let mut retried = false;
        let translated = loop {
            let reused = inner.session.is_some();
            match self.shared.attempt(&mut inner, &chunks, resolved, tgt) {
                Err(TranslateError::WorkerDied(reason)) if reused && !retried => {
                    tracing::warn!(%reason, "翻译 worker 已死亡，重启后重试一次");
                    retried = true;
                }
                other => break other,
            }
        };
        inner.last_used = Instant::now();
        let translated = translated?;
        if translated.len() != work.len() {
            return Err(TranslateError::Inference("worker 返回的译文条数不符".into()));
        }
        for (slot, text) in work.iter().zip(translated) {
            results[*slot] = text;
        }
        if !inner.monitor_running && inner.session.is_some() {
            inner.monitor_running = true;
            spawn_idle_monitor(Arc::clone(&self.shared));
        }
        Ok(results)
    }

    /// 模型清单声明的语言对。
    fn supported_pairs(&self) -> Vec<(Lang, Lang)> {
        self.shared.config.pairs.clone()
    }

    /// 引擎名称。
    fn engine_name(&self) -> &'static str {
        "LocalNmt"
    }

    /// 缓存标识：模型 ID + 束宽（换模型或束宽后不复用旧译文）。
    fn cache_id(&self) -> String {
        format!("nmt:{}:b{}", self.shared.config.model_id, self.shared.config.num_beams)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;
    use std::sync::atomic::AtomicBool;

    /// 假 worker 的脚本：收到命令时决定回什么。
    type Script = Arc<dyn Fn(&Command, &mut FakeState) + Send + Sync>;

    /// 假 worker 的可观察状态。
    #[derive(Default)]
    struct FakeState {
        /// 待接收的行。
        queue: VecDeque<String>,
        /// 通道已关闭（进程退出）。
        closed: bool,
        /// 不再回应（卡死）。
        hang: bool,
        /// 收到过的命令。
        sent: Vec<Command>,
        /// 是否被强杀。
        terminated: bool,
        /// 是否收到过 unload。
        unloaded: bool,
    }

    /// 假通道：同步脚本，不开线程。
    struct FakeTransport {
        /// 共享状态。
        state: Arc<Mutex<FakeState>>,
        /// 脚本。
        script: Script,
    }

    impl Transport for FakeTransport {
        fn send(&mut self, line: &str) -> io::Result<()> {
            let command: Command = serde_json::from_str(line).map_err(io::Error::other)?;
            let mut state = self.state.lock().unwrap();
            if state.closed {
                return Err(io::Error::new(io::ErrorKind::BrokenPipe, "closed"));
            }
            state.sent.push(command.clone());
            let script = Arc::clone(&self.script);
            script(&command, &mut state);
            Ok(())
        }

        fn recv(&mut self, timeout: Duration) -> Recv {
            let mut state = self.state.lock().unwrap();
            if let Some(line) = state.queue.pop_front() {
                return Recv::Line(line);
            }
            if state.closed {
                return Recv::Closed;
            }
            drop(state);
            thread::sleep(timeout.min(Duration::from_millis(20)));
            Recv::Timeout
        }

        fn diagnostics(&mut self) -> String {
            "fake exit".to_string()
        }

        fn terminate(&mut self) {
            let mut state = self.state.lock().unwrap();
            state.terminated = true;
            state.closed = true;
        }

        fn wait_exit(&mut self, _timeout: Duration) -> bool {
            self.state.lock().unwrap().closed
        }
    }

    /// 假拉起方式：按顺序取脚本，记录每次拉起的状态。
    struct FakeLauncher {
        /// 待用脚本队列（用完后沿用最后一个）。
        scripts: Mutex<VecDeque<Script>>,
        /// 已拉起会话的状态。
        states: Mutex<Vec<Arc<Mutex<FakeState>>>>,
        /// 是否发 ready（false 模拟启动即崩溃）。
        send_ready: AtomicBool,
        /// ready 里的协议版本。
        protocol: AtomicU32,
        /// 完全静默（不发 ready 也不关闭，模拟启动卡死）。
        silent: AtomicBool,
    }

    impl FakeLauncher {
        /// 用一组脚本创建。
        fn new(scripts: Vec<Script>) -> Arc<Self> {
            Arc::new(Self {
                scripts: Mutex::new(scripts.into()),
                states: Mutex::new(Vec::new()),
                send_ready: AtomicBool::new(true),
                protocol: AtomicU32::new(PROTOCOL_VERSION),
                silent: AtomicBool::new(false),
            })
        }

        /// 第 n 次拉起的状态。
        fn state(&self, n: usize) -> Arc<Mutex<FakeState>> {
            Arc::clone(&self.states.lock().unwrap()[n])
        }

        /// 拉起次数。
        fn launches(&self) -> usize {
            self.states.lock().unwrap().len()
        }
    }

    impl WorkerLauncher for FakeLauncher {
        fn launch(&self, _config: &WorkerConfig) -> Result<Box<dyn Transport>, TranslateError> {
            let script = {
                let mut scripts = self.scripts.lock().unwrap();
                if scripts.len() > 1 {
                    scripts.pop_front().unwrap()
                } else {
                    Arc::clone(scripts.front().expect("至少一个脚本"))
                }
            };
            let mut state = FakeState::default();
            if self.silent.load(Ordering::SeqCst) {
                // 什么都不发
            } else if self.send_ready.load(Ordering::SeqCst) {
                let ready = Event::Ready { protocol: self.protocol.load(Ordering::SeqCst), pid: 4242 };
                state.queue.push_back(serde_json::to_string(&ready).unwrap());
            } else {
                state.closed = true;
            }
            let state = Arc::new(Mutex::new(state));
            self.states.lock().unwrap().push(Arc::clone(&state));
            Ok(Box::new(FakeTransport { state, script }))
        }
    }

    /// 往队列里塞一个事件。
    fn push(state: &mut FakeState, event: &Event) {
        state.queue.push_back(serde_json::to_string(event).unwrap());
    }

    /// 正常脚本：Load → Loaded；Translate → 每条加 `T:` 前缀；Ping → Pong；Unload → 退出。
    fn normal() -> Script {
        Arc::new(|command, state| match command {
            Command::Load { .. } => push(state, &Event::Loaded { model_id: "fake".into(), load_ms: 1, mem_bytes: 10 }),
            Command::Translate { id, texts, .. } => {
                if state.hang {
                    return;
                }
                let out = texts.iter().map(|t| format!("T:{t}")).collect();
                push(state, &Event::Result { id: *id, texts: out, elapsed_ms: 1 });
            }
            Command::Ping => push(state, &Event::Pong { loaded: true, mem_bytes: 5, peak_bytes: 9 }),
            Command::Unload => {
                state.unloaded = true;
                push(state, &Event::Unloaded);
                state.closed = true;
            }
        })
    }

    /// 测试用配置（毫秒级超时，en→zh-CN / zh-TW）。
    fn config() -> WorkerConfig {
        WorkerConfig {
            exe: PathBuf::from("fake.exe"),
            model_dir: PathBuf::from("D:/models/fake"),
            model_id: "fake".into(),
            pairs: vec![(Lang::En, Lang::ZhHans), (Lang::En, Lang::ZhHant)],
            ort_dylib: None,
            num_beams: 4,
            trim_after_request: true,
            idle_timeout: Duration::from_secs(60),
            timeouts: Timeouts {
                ready: Duration::from_millis(200),
                load: Duration::from_millis(300),
                request_base: Duration::from_millis(80),
                request_per_char: Duration::from_micros(10),
                request_max: Duration::from_millis(500),
                unload_grace: Duration::from_millis(100),
            },
        }
    }

    /// 造引擎与拉起器。
    fn engine_with(scripts: Vec<Script>, cfg: WorkerConfig) -> (WorkerEngine, Arc<FakeLauncher>) {
        let launcher = FakeLauncher::new(scripts);
        (WorkerEngine::with_launcher(cfg, launcher.clone()), launcher)
    }

    /// 取假状态里收到的命令。
    fn sent(launcher: &FakeLauncher, n: usize) -> Vec<Command> {
        launcher.state(n).lock().unwrap().sent.clone()
    }

    /// 懒拉起：构造引擎不启动进程；首次翻译才拉起、加载并翻译；同语言对不重复加载。
    #[test]
    fn lazy_launch_load_and_translate() {
        let (engine, launcher) = engine_with(vec![normal()], config());
        assert_eq!(launcher.launches(), 0);
        assert!(!engine.is_running());
        assert_eq!(engine.translate("Hello", Lang::En, Lang::ZhHans).unwrap(), "T:Hello");
        assert_eq!(engine.translate("Bye", Lang::En, Lang::ZhHans).unwrap(), "T:Bye");
        assert_eq!(launcher.launches(), 1);
        let commands = sent(&launcher, 0);
        assert!(matches!(commands[0], Command::Load { ref src, ref tgt, trim_after_request: Some(true), .. } if src == "en" && tgt == "zh-CN"));
        assert_eq!(commands.iter().filter(|c| matches!(c, Command::Load { .. })).count(), 1);
        assert!(matches!(commands[1], Command::Translate { id: 1, num_beams: Some(4), .. }));
        assert!(matches!(commands[2], Command::Translate { id: 2, .. }));
        assert!(engine.is_running());
    }

    /// 换语言对会重新发 load；`Auto` 源语言被解析为清单里的具体源语言。
    #[test]
    fn language_change_reloads_and_auto_is_resolved() {
        let (engine, launcher) = engine_with(vec![normal()], config());
        engine.translate("a", Lang::Auto, Lang::ZhHans).unwrap();
        engine.translate("a", Lang::En, Lang::ZhHant).unwrap();
        let loads: Vec<(String, String)> = sent(&launcher, 0)
            .into_iter()
            .filter_map(|c| match c {
                Command::Load { src, tgt, .. } => Some((src, tgt)),
                _ => None,
            })
            .collect();
        assert_eq!(loads, [("en".into(), "zh-CN".into()), ("en".into(), "zh-TW".into())]);
    }

    /// 不支持的语言对在拉起前就被拒绝；空白与空批量不占用 worker。
    #[test]
    fn unsupported_and_blank_inputs_never_launch() {
        let (engine, launcher) = engine_with(vec![normal()], config());
        assert_eq!(
            engine.translate("x", Lang::ZhHans, Lang::En),
            Err(TranslateError::UnsupportedLanguagePair(Lang::ZhHans, Lang::En))
        );
        assert!(engine.translate_batch(&[], Lang::En, Lang::ZhHans).unwrap().is_empty());
        let blanks = vec!["  ".to_string(), String::new()];
        assert_eq!(engine.translate_batch(&blanks, Lang::En, Lang::ZhHans).unwrap(), blanks);
        assert_eq!(launcher.launches(), 0);
        let mixed = vec!["a".to_string(), " ".to_string(), "b".to_string()];
        assert_eq!(engine.translate_batch(&mixed, Lang::En, Lang::ZhHans).unwrap(), ["T:a", " ", "T:b"]);
        assert_eq!(launcher.launches(), 1);
    }

    /// 并发请求被串行化，每个线程拿到自己的译文，请求编号唯一递增。
    #[test]
    fn concurrent_requests_are_serialized() {
        let (engine, launcher) = engine_with(vec![normal()], config());
        let engine = Arc::new(engine);
        let handles: Vec<_> = (0..8)
            .map(|i| {
                let engine = Arc::clone(&engine);
                thread::spawn(move || engine.translate(&format!("text{i}"), Lang::En, Lang::ZhHans))
            })
            .collect();
        for handle in handles {
            assert!(handle.join().unwrap().unwrap().starts_with("T:text"));
        }
        let ids: Vec<u64> = sent(&launcher, 0)
            .into_iter()
            .filter_map(|c| match c {
                Command::Translate { id, .. } => Some(id),
                _ => None,
            })
            .collect();
        assert_eq!(ids, (1..=8).collect::<Vec<u64>>());
        assert_eq!(launcher.launches(), 1);
    }

    /// 复用中的 worker 中途崩溃：自动重启并重试一次，调用方拿到正确译文。
    #[test]
    fn reused_worker_crash_restarts_once() {
        let crashing: Script = Arc::new(|command, state| match command {
            Command::Load { .. } => push(state, &Event::Loaded { model_id: "f".into(), load_ms: 1, mem_bytes: 1 }),
            Command::Translate { texts, .. } if texts[0] == "boom" => state.closed = true,
            other => normal()(other, state),
        });
        let (engine, launcher) = engine_with(vec![crashing, normal()], config());
        assert_eq!(engine.translate("ok", Lang::En, Lang::ZhHans).unwrap(), "T:ok");
        assert_eq!(engine.translate("boom", Lang::En, Lang::ZhHans).unwrap(), "T:boom");
        assert_eq!(launcher.launches(), 2);
        assert!(launcher.state(0).lock().unwrap().terminated);
    }

    /// 新拉起的 worker 一上来就崩溃：不重试，报 WorkerDied 并带诊断。
    #[test]
    fn fresh_worker_crash_is_reported_without_retry() {
        let dying: Script = Arc::new(|command, state| {
            if matches!(command, Command::Load { .. }) {
                state.closed = true;
            }
        });
        let (engine, launcher) = engine_with(vec![dying], config());
        let err = engine.translate("x", Lang::En, Lang::ZhHans).unwrap_err();
        assert!(matches!(&err, TranslateError::WorkerDied(m) if m.contains("fake exit")), "{err:?}");
        assert_eq!(launcher.launches(), 1);
        assert!(!engine.is_running());
    }

    /// 卡死：超时后强杀并复位，不重试；下一次请求重新拉起并成功。
    #[test]
    fn hang_times_out_kills_and_recovers() {
        let hanging: Script = Arc::new(|command, state| match command {
            Command::Load { .. } => push(state, &Event::Loaded { model_id: "f".into(), load_ms: 1, mem_bytes: 1 }),
            Command::Translate { .. } => {}
            other => normal()(other, state),
        });
        let (engine, launcher) = engine_with(vec![hanging, normal()], config());
        let started = Instant::now();
        assert_eq!(engine.translate("x", Lang::En, Lang::ZhHans), Err(TranslateError::Timeout));
        assert!(started.elapsed() < Duration::from_secs(3));
        assert!(launcher.state(0).lock().unwrap().terminated);
        assert!(!engine.is_running());
        assert_eq!(engine.translate("y", Lang::En, Lang::ZhHans).unwrap(), "T:y");
        assert_eq!(launcher.launches(), 2);
    }

    /// 运行时缺失映射为 RuntimeMissing（可下载），worker 被回收。
    #[test]
    fn runtime_missing_is_mapped_and_cleaned_up() {
        let script: Script = Arc::new(|command, state| {
            if matches!(command, Command::Load { .. }) {
                push(state, &Event::Error { id: None, kind: ErrorKind::RuntimeMissing, message: "no dll".into() });
            }
        });
        let (engine, launcher) = engine_with(vec![script], config());
        let err = engine.translate("x", Lang::En, Lang::ZhHans).unwrap_err();
        assert_eq!(err, TranslateError::RuntimeMissing("no dll".into()));
        assert!(err.can_download_runtime());
        assert!(launcher.state(0).lock().unwrap().terminated);
        assert!(!engine.is_running());
    }

    /// 加载阶段的各类错误映射为 ModelLoad / OutOfMemory / UnsupportedLanguagePair。
    #[test]
    fn load_errors_are_mapped() {
        for (kind, check) in [
            (ErrorKind::ChecksumMismatch, "model"),
            (ErrorKind::ModelMissing, "model"),
            (ErrorKind::ManifestInvalid, "model"),
            (ErrorKind::OutOfMemory, "oom"),
            (ErrorKind::UnsupportedPair, "pair"),
        ] {
            let script: Script = Arc::new(move |command, state| {
                if matches!(command, Command::Load { .. }) {
                    push(state, &Event::Error { id: None, kind, message: "m".into() });
                }
            });
            let (engine, _) = engine_with(vec![script], config());
            let err = engine.translate("x", Lang::En, Lang::ZhHans).unwrap_err();
            match check {
                "model" => assert!(matches!(err, TranslateError::ModelLoad(_)), "{kind:?}"),
                "oom" => assert!(matches!(err, TranslateError::OutOfMemory(_))),
                _ => assert!(matches!(err, TranslateError::UnsupportedLanguagePair(..))),
            }
        }
    }

    /// 解码失败不杀 worker（输入问题，进程仍可用）。
    #[test]
    fn decode_failure_keeps_worker_alive() {
        let script: Script = Arc::new(|command, state| match command {
            Command::Translate { id, texts, .. } if texts[0] == "bad" => {
                push(state, &Event::Error { id: Some(*id), kind: ErrorKind::DecodeFailed, message: "oops".into() });
            }
            other => normal()(other, state),
        });
        let (engine, launcher) = engine_with(vec![script], config());
        assert!(matches!(engine.translate("bad", Lang::En, Lang::ZhHans), Err(TranslateError::Inference(_))));
        assert!(engine.is_running());
        assert_eq!(engine.translate("good", Lang::En, Lang::ZhHans).unwrap(), "T:good");
        assert_eq!(launcher.launches(), 1);
    }

    /// 协议版本不符、没有 ready、启动即崩溃都给出明确错误。
    #[test]
    fn handshake_failures() {
        let (engine, launcher) = engine_with(vec![normal()], config());
        launcher.protocol.store(PROTOCOL_VERSION + 1, Ordering::SeqCst);
        assert!(matches!(
            engine.translate("x", Lang::En, Lang::ZhHans),
            Err(TranslateError::WorkerUnavailable(m)) if m.contains("协议版本")
        ));
        assert!(launcher.state(0).lock().unwrap().terminated);
        launcher.protocol.store(PROTOCOL_VERSION, Ordering::SeqCst);
        launcher.send_ready.store(false, Ordering::SeqCst);
        assert!(matches!(engine.translate("x", Lang::En, Lang::ZhHans), Err(TranslateError::WorkerDied(_))));
    }

    /// 启动后一直不发 ready：超时报 WorkerUnavailable 并强杀。
    #[test]
    fn missing_ready_times_out() {
        let (engine, launcher) = engine_with(vec![normal()], config());
        launcher.silent.store(true, Ordering::SeqCst);
        let err = engine.translate("x", Lang::En, Lang::ZhHans).unwrap_err();
        assert!(matches!(&err, TranslateError::WorkerUnavailable(m) if m.contains("ready")), "{err:?}");
        assert!(launcher.state(0).lock().unwrap().terminated);
        assert!(!engine.is_running());
    }

    /// 过期编号的结果被忽略，仍等到自己的结果。
    #[test]
    fn stale_results_are_ignored() {
        let script: Script = Arc::new(|command, state| match command {
            Command::Translate { id, texts, .. } => {
                push(state, &Event::Result { id: id + 100, texts: vec!["stale".into()], elapsed_ms: 1 });
                let out = texts.iter().map(|t| format!("T:{t}")).collect();
                push(state, &Event::Result { id: *id, texts: out, elapsed_ms: 1 });
            }
            other => normal()(other, state),
        });
        let (engine, _) = engine_with(vec![script], config());
        assert_eq!(engine.translate("a", Lang::En, Lang::ZhHans).unwrap(), "T:a");
    }

    /// 杂行（非 JSON）被忽略，不影响握手与翻译。
    #[test]
    fn garbage_lines_are_ignored() {
        let script: Script = Arc::new(|command, state| {
            state.queue.push_back("some banner text".into());
            normal()(command, state);
        });
        let (engine, _) = engine_with(vec![script], config());
        assert_eq!(engine.translate("a", Lang::En, Lang::ZhHans).unwrap(), "T:a");
    }

    /// 译文条数不符视为推理错误。
    #[test]
    fn wrong_result_count_is_an_error() {
        let script: Script = Arc::new(|command, state| match command {
            Command::Translate { id, .. } => push(state, &Event::Result { id: *id, texts: vec![], elapsed_ms: 1 }),
            other => normal()(other, state),
        });
        let (engine, _) = engine_with(vec![script], config());
        assert!(matches!(engine.translate("a", Lang::En, Lang::ZhHans), Err(TranslateError::Inference(_))));
    }

    /// 空闲超时后自动卸载（发 unload、进程退出）；再次翻译会重新拉起。
    #[test]
    fn idle_unload_and_relaunch() {
        let mut cfg = config();
        cfg.idle_timeout = Duration::from_millis(60);
        let (engine, launcher) = engine_with(vec![normal()], cfg);
        engine.translate("a", Lang::En, Lang::ZhHans).unwrap();
        assert!(engine.is_running());
        let deadline = Instant::now() + Duration::from_secs(3);
        while engine.is_running() && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(20));
        }
        assert!(!engine.is_running(), "空闲后应卸载");
        assert!(launcher.state(0).lock().unwrap().unloaded);
        assert_eq!(engine.translate("b", Lang::En, Lang::ZhHans).unwrap(), "T:b");
        assert_eq!(launcher.launches(), 2);
    }

    /// 持续使用期间不会被空闲卸载。
    #[test]
    fn busy_worker_is_not_unloaded() {
        let mut cfg = config();
        cfg.idle_timeout = Duration::from_millis(200);
        let (engine, launcher) = engine_with(vec![normal()], cfg);
        for _ in 0..6 {
            engine.translate("a", Lang::En, Lang::ZhHans).unwrap();
            thread::sleep(Duration::from_millis(50));
        }
        assert!(engine.is_running());
        assert_eq!(launcher.launches(), 1);
    }

    /// 引擎释放时结束 worker；shutdown 幂等。
    #[test]
    fn drop_terminates_worker() {
        let (engine, launcher) = engine_with(vec![normal()], config());
        engine.translate("a", Lang::En, Lang::ZhHans).unwrap();
        engine.shutdown();
        engine.shutdown();
        assert!(!engine.is_running());
        let state = launcher.state(0);
        assert!(state.lock().unwrap().unloaded);
        drop(engine);
        let (engine2, launcher2) = engine_with(vec![normal()], config());
        engine2.translate("a", Lang::En, Lang::ZhHans).unwrap();
        drop(engine2);
        assert!(launcher2.state(0).lock().unwrap().closed);
    }

    /// 内存快照走 ping，未运行返回 None，不刷新空闲计时。
    #[test]
    fn memory_snapshot_uses_ping() {
        let (engine, _) = engine_with(vec![normal()], config());
        assert_eq!(engine.memory_snapshot(), None);
        engine.translate("a", Lang::En, Lang::ZhHans).unwrap();
        assert_eq!(
            engine.memory_snapshot(),
            Some(MemorySnapshot { pid: 4242, mem_bytes: 5, peak_bytes: 9 })
        );
    }

    /// 大批量文本按字节上限分成多个请求，结果顺序不变；单条超限直接拒绝。
    #[test]
    fn large_batches_are_chunked() {
        let big = "x".repeat(150_000);
        let texts = vec![big.clone(), big.clone(), "tail".to_string()];
        let (engine, launcher) = engine_with(vec![normal()], config());
        let out = engine.translate_batch(&texts, Lang::En, Lang::ZhHans).unwrap();
        assert_eq!(out.len(), 3);
        assert_eq!(out[2], "T:tail");
        let translates = sent(&launcher, 0)
            .into_iter()
            .filter(|c| matches!(c, Command::Translate { .. }))
            .count();
        assert_eq!(translates, 2, "前两条各占一块，第三条与第二条同块");
        let too_big = vec!["y".repeat(MAX_REQUEST_BYTES + 1)];
        assert!(matches!(
            engine.translate_batch(&too_big, Lang::En, Lang::ZhHans),
            Err(TranslateError::InvalidRequest(_))
        ));
    }

    /// 条数上限：超过 [`MAX_REQUEST_TEXTS`] 条时即使字节很少也分块。
    #[test]
    fn chunk_texts_respects_item_limit() {
        let texts: Vec<String> = (0..(MAX_REQUEST_TEXTS * 2 + 3)).map(|i| i.to_string()).collect();
        let chunks = chunk_texts(&texts, MAX_REQUEST_BYTES).unwrap();
        assert_eq!(chunks.iter().map(Vec::len).collect::<Vec<_>>(), [MAX_REQUEST_TEXTS, MAX_REQUEST_TEXTS, 3]);
        assert_eq!(chunks.concat(), texts);
    }

    /// 分块函数本身：顺序保持、边界正确。
    #[test]
    fn chunk_texts_boundaries() {
        let texts: Vec<String> = ["aaaa", "bbbb", "cc", "dddddd"].iter().map(|s| s.to_string()).collect();
        let chunks = chunk_texts(&texts, 8).unwrap();
        assert_eq!(chunks, vec![vec!["aaaa", "bbbb"], vec!["cc", "dddddd"]]);
        let exact = chunk_texts(&["12345678".to_string(), "1".to_string()], 8).unwrap();
        assert_eq!(exact.len(), 2, "恰好占满一块后新文本另起一块");
        assert!(chunk_texts(&[], 8).unwrap().is_empty());
        assert!(chunk_texts(&["123456789".to_string()], 8).is_err());
    }

    /// 缓存标识含模型与束宽；束宽被夹到合法范围。
    #[test]
    fn cache_id_and_beam_clamp() {
        let mut cfg = config();
        cfg.num_beams = 99;
        let (engine, _) = engine_with(vec![normal()], cfg);
        assert_eq!(engine.cache_id(), "nmt:fake:b8");
        assert_eq!(engine.engine_name(), "LocalNmt");
        assert_eq!(engine.supported_pairs().len(), 2);
    }

    /// 超时随文本长度增长，且有上限。
    #[test]
    fn request_timeout_scales_and_caps() {
        let t = Timeouts::default();
        assert_eq!(t.request_for(0), t.request_base);
        assert!(t.request_for(1000) > t.request_for(100));
        assert_eq!(t.request_for(usize::MAX), t.request_max);
    }

    /// 有界读行：正常、CRLF、超长丢弃、EOF、无换行收尾。
    #[test]
    fn bounded_line_reader() {
        let mut input = io::Cursor::new(b"one\r\ntwo\n".to_vec());
        assert_eq!(read_line_bounded(&mut input, 16).unwrap(), Some("one".into()));
        assert_eq!(read_line_bounded(&mut input, 16).unwrap(), Some("two".into()));
        assert_eq!(read_line_bounded(&mut input, 16).unwrap(), None);
        let mut long = io::Cursor::new(format!("{}\nok\n", "z".repeat(100)).into_bytes());
        assert_eq!(read_line_bounded(&mut long, 16).unwrap(), Some(String::new()));
        assert_eq!(read_line_bounded(&mut long, 16).unwrap(), Some("ok".into()));
        let mut tail = io::Cursor::new(b"no newline".to_vec());
        assert_eq!(read_line_bounded(&mut tail, 64).unwrap(), Some("no newline".into()));
    }

    /// 可执行文件不存在：明确报“未找到翻译组件”，不 panic。
    #[test]
    fn missing_exe_is_reported() {
        let mut cfg = config();
        cfg.exe = PathBuf::from("Z:/definitely/missing/snow-translator.exe");
        let engine = WorkerEngine::new(cfg);
        let err = engine.translate("x", Lang::En, Lang::ZhHans).unwrap_err();
        assert!(matches!(&err, TranslateError::WorkerUnavailable(m) if m.contains("未找到翻译组件")), "{err:?}");
    }

    /// 真实进程：拉起一个不会说协议的进程（cmd.exe），ready 超时后报错并回收，不挂死。
    #[cfg(windows)]
    #[test]
    fn real_process_without_ready_times_out() {
        let system_root = std::env::var_os("SystemRoot").map(PathBuf::from).unwrap_or_default();
        let cmd = system_root.join("System32").join("cmd.exe");
        if !cmd.is_file() {
            return;
        }
        let mut cfg = config();
        cfg.exe = cmd;
        cfg.timeouts.ready = Duration::from_millis(400);
        let engine = WorkerEngine::new(cfg);
        let started = Instant::now();
        let err = engine.translate("x", Lang::En, Lang::ZhHans).unwrap_err();
        assert!(matches!(&err, TranslateError::WorkerUnavailable(m) if m.contains("ready")), "{err:?}");
        assert!(started.elapsed() < Duration::from_secs(5));
        assert!(!engine.is_running());
    }

    /// 真实 worker 端到端（需要环境变量，缺失则跳过）：翻译 → 空闲卸载 → 进程退出 → 再次翻译。
    ///
    /// 需要：`SNOW_TRANSLATOR_EXE`、`SNOW_TRANSLATOR_TEST_MODEL_DIR`、`SNOW_ORT_DYLIB`。
    #[test]
    fn real_worker_end_to_end_when_configured() {
        let (Some(exe), Some(model_dir), Some(dylib)) = (
            std::env::var_os("SNOW_TRANSLATOR_EXE"),
            std::env::var_os("SNOW_TRANSLATOR_TEST_MODEL_DIR"),
            std::env::var_os(ENV_ORT_DYLIB),
        ) else {
            eprintln!("跳过：未设置 SNOW_TRANSLATOR_EXE / SNOW_TRANSLATOR_TEST_MODEL_DIR / SNOW_ORT_DYLIB");
            return;
        };
        let mut cfg = config();
        cfg.exe = PathBuf::from(exe);
        cfg.model_dir = PathBuf::from(model_dir);
        cfg.ort_dylib = Some(PathBuf::from(dylib));
        cfg.timeouts = Timeouts::default();
        cfg.idle_timeout = Duration::from_millis(800);
        let engine = WorkerEngine::new(cfg);
        let out = engine
            .translate("Where is the nearest train station?", Lang::En, Lang::ZhHans)
            .expect("翻译");
        assert!(out.chars().any(|c| ('\u{4e00}'..='\u{9fff}').contains(&c)), "{out}");
        let snapshot = engine.memory_snapshot().expect("内存快照");
        assert!(snapshot.mem_bytes > 50 * 1024 * 1024);
        let deadline = Instant::now() + Duration::from_secs(10);
        while engine.is_running() && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(100));
        }
        assert!(!engine.is_running(), "空闲后 worker 应已卸载");
        let again = engine.translate("Thank you.", Lang::En, Lang::ZhHans).expect("重新拉起后翻译");
        assert!(!again.is_empty());
        assert_eq!(engine.launch_count(), 2);
    }
}
