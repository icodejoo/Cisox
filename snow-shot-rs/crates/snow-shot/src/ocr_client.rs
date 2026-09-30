//! `snow-ocr-process` 协议 v4 客户端：拉起 worker、握手、准备会话、经临时文件传图并取回识别结果。
//!
//! 一次识别的时序：`AttachBuffer → Submit → ImageConsumed → Recognize → Complete → DetachBuffer`。
//! 像素走普通临时文件（RGBA，32 字节槽头 + 紧凑像素），worker 按路径只读映射，客户端不映射内存。
//! 传输层是泛型的（任意 `Read` / `Write`），测试用管道 + 假 worker 线程驱动，不需要真实模型。

use crate::ocr_assets::{OcrAssets, OcrUnavailable};
use snow_ocr_protocol::{
    CompleteResult, Frame, FrameReader, Kind, MAX_PIXELS, OcrLine, ProtocolError, SLOT_HEADER_LEN,
    VERSION, attach_buffer_payload, decode_complete, decode_image_consumed, decode_ready,
    decode_session_ready, encode_frame, hello_payload, prepare_session_payload, submit_payload,
    write_slot_header,
};
use std::io::{BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError, channel};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// Windows 不弹控制台窗口的进程创建标志。
#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;
/// 每像素字节数（RGBA）。
const BYTES_PER_PIXEL: usize = 4;
/// stderr 只保留末尾这么多字节（用于错误说明）。
const STDERR_KEEP_BYTES: usize = 2048;
/// 关机应答的等待时间。
const SHUTDOWN_ACK_WAIT: Duration = Duration::from_secs(2);
/// 临时映射文件名前缀。
const TEMP_FILE_PREFIX: &str = "snow-shot-ocr";

/// 各阶段超时。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Timeouts {
    /// 等待 `Ready`（上游为 5 秒）。
    pub ready: Duration,
    /// 等待会话准备完成（含模型加载）。
    pub prepare: Duration,
    /// 等待 Attach / Consumed / Detach 这类快速应答。
    pub step: Duration,
    /// 等待识别完成。
    pub recognize: Duration,
}

impl Default for Timeouts {
    /// 生产用超时：Ready 5 秒、准备 90 秒、快速应答 10 秒、识别 120 秒。
    fn default() -> Self {
        Self {
            ready: Duration::from_secs(5),
            prepare: Duration::from_secs(90),
            step: Duration::from_secs(10),
            recognize: Duration::from_secs(120),
        }
    }
}

/// OCR 会话配置（模型路径 + 加速选项）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionConfig {
    /// 请求 DirectML 加速。
    pub directml: bool,
    /// 检测缩放策略（0 = max，1 = min）。
    pub resize_policy: u8,
    /// 检测模型。
    pub detector: PathBuf,
    /// 识别模型。
    pub recognizer: PathBuf,
    /// 字典。
    pub dictionary: PathBuf,
}

/// OCR 失败原因。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OcrError {
    /// 资产不可用（缺运行时 / 模型）。
    Unavailable(OcrUnavailable),
    /// 拉起进程失败。
    SpawnFailed(String),
    /// 在时限内没有收到 `Ready`。
    ReadyTimeout,
    /// worker 协议版本与客户端不一致。
    VersionMismatch {
        /// 客户端协议版本。
        expected: u32,
        /// worker 报告的版本。
        actual: u32,
    },
    /// 进程中途退出（附 stderr 末尾内容）。
    ProcessDied(String),
    /// 会话准备失败（模型加载失败、onnxruntime 缺失等）。
    SessionNotReady,
    /// worker 报告识别失败。
    Failed(String),
    /// worker 报告识别被取消。
    Cancelled(String),
    /// 某一步超时。
    Timeout(&'static str),
    /// 协议错误（帧非法、序号不符等）。
    Protocol(String),
    /// 本地 IO 错误（临时文件等）。
    Io(String),
    /// 输入图像不合法。
    InvalidImage(String),
}

impl OcrError {
    /// 面向用户的提示文案。
    ///
    /// # 返回
    /// 一句中文说明。
    pub fn message(&self) -> String {
        match self {
            Self::Unavailable(u) => u.message(),
            Self::SpawnFailed(e) => format!("无法启动 OCR 进程: {e}"),
            Self::ReadyTimeout => "OCR 进程启动超时".to_string(),
            Self::VersionMismatch { expected, actual } => {
                format!("OCR 运行时版本不匹配（需要协议 {expected}，实际 {actual}），请重新下载运行时")
            }
            Self::ProcessDied(tail) if tail.is_empty() => "OCR 进程意外退出".to_string(),
            Self::ProcessDied(tail) => format!("OCR 进程意外退出: {tail}"),
            Self::SessionNotReady => {
                "OCR 模型加载失败（模型文件损坏或缺少 onnxruntime），请重新下载模型".to_string()
            }
            Self::Failed(e) => format!("识别失败: {e}"),
            Self::Cancelled(_) => "识别已取消".to_string(),
            Self::Timeout(step) => format!("OCR 超时（{step}）"),
            Self::Protocol(e) => format!("OCR 通信错误: {e}"),
            Self::Io(e) => format!("OCR 临时文件错误: {e}"),
            Self::InvalidImage(e) => format!("无法识别该图像: {e}"),
        }
    }

    /// 是否可以通过下载资产解决。
    pub fn can_download(&self) -> bool {
        matches!(self, Self::Unavailable(u) if u.can_download())
    }

    /// 出错后 worker 是否已不可继续使用（需要丢弃并重启）。
    pub fn is_fatal_for_worker(&self) -> bool {
        !matches!(
            self,
            Self::Failed(_) | Self::Cancelled(_) | Self::InvalidImage(_) | Self::Unavailable(_)
        )
    }
}

impl From<ProtocolError> for OcrError {
    /// 协议编解码错误统一映射为 `Protocol`（EOF 视为进程退出）。
    fn from(e: ProtocolError) -> Self {
        match e {
            ProtocolError::Eof => Self::ProcessDied(String::new()),
            other => Self::Protocol(other.to_string()),
        }
    }
}

/// 读线程送来的消息。
enum Msg {
    /// 一帧应答。
    Frame(Frame),
    /// 流关闭（进程退出或协议错误）。
    Closed,
}

/// 临时映射文件：离开作用域时尽力删除。
struct TempFile(PathBuf);

impl Drop for TempFile {
    /// 删除临时文件（失败忽略，系统临时目录会被清理）。
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

/// 全局递增的临时文件序号。
static TEMP_SEQ: AtomicU64 = AtomicU64::new(0);

/// 一个已握手的 OCR worker 连接。
pub struct OcrWorker {
    /// 发往 worker 的命令流。
    writer: Box<dyn Write + Send>,
    /// 读线程送来的应答。
    rx: Receiver<Msg>,
    /// 子进程句柄（测试用管道传输时没有）。
    child: Option<Child>,
    /// worker 的 stderr 末尾内容。
    stderr_tail: Arc<Mutex<String>>,
    /// 下一个操作号（会话 / 映射代号 / 识别 token 共用递增序列）。
    next_id: u64,
    /// 当前已准备好的会话配置。
    prepared: Option<SessionConfig>,
    /// 各阶段超时。
    timeouts: Timeouts,
}

impl OcrWorker {
    /// 拉起 `snow-ocr-process` 并完成握手。
    ///
    /// # 参数
    /// - `assets`：已就绪的资产（用到 exe 与能力缓存目录）。
    /// - `timeouts`：各阶段超时。
    ///
    /// # 返回
    /// 已握手的连接；拉起失败、Ready 超时或协议版本不符时返回错误。
    ///
    /// ```ignore
    /// let mut worker = OcrWorker::spawn(&assets, Timeouts::default())?;
    /// ```
    pub fn spawn(assets: &OcrAssets, timeouts: Timeouts) -> Result<Self, OcrError> {
        let _ = std::fs::create_dir_all(&assets.state_dir);
        let mut command = Command::new(&assets.exe);
        command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        if let Some(dir) = assets.exe.parent() {
            command.current_dir(dir);
        }
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            command.creation_flags(CREATE_NO_WINDOW);
        }
        let mut child = command.spawn().map_err(|e| OcrError::SpawnFailed(e.to_string()))?;
        let (Some(stdin), Some(stdout), Some(stderr)) =
            (child.stdin.take(), child.stdout.take(), child.stderr.take())
        else {
            let _ = child.kill();
            return Err(OcrError::SpawnFailed("无法接管子进程管道".to_string()));
        };
        let stderr_tail = Arc::new(Mutex::new(String::new()));
        spawn_stderr_pump(stderr, Arc::clone(&stderr_tail));
        let mut worker = Self::from_transport(stdout, stdin, Some(child), stderr_tail, timeouts);
        worker.handshake(&assets.state_dir)?;
        Ok(worker)
    }

    /// 用任意传输构造连接（不握手）；读线程立即启动。
    ///
    /// # 参数
    /// - `reader` / `writer`：worker 的应答流与命令流。
    /// - `child`：子进程（管道传输时为 `None`）。
    /// - `stderr_tail`：stderr 末尾缓冲（共享给 stderr 读线程）。
    /// - `timeouts`：各阶段超时。
    pub fn from_transport(
        reader: impl Read + Send + 'static,
        writer: impl Write + Send + 'static,
        child: Option<Child>,
        stderr_tail: Arc<Mutex<String>>,
        timeouts: Timeouts,
    ) -> Self {
        let (tx, rx) = channel();
        std::thread::spawn(move || {
            let mut frames = FrameReader::new(BufReader::new(reader));
            loop {
                match frames.read_frame() {
                    Ok(frame) => {
                        if tx.send(Msg::Frame(frame)).is_err() {
                            break;
                        }
                    }
                    Err(_) => {
                        let _ = tx.send(Msg::Closed);
                        break;
                    }
                }
            }
        });
        Self {
            writer: Box::new(writer),
            rx,
            child,
            stderr_tail,
            next_id: 1,
            prepared: None,
            timeouts,
        }
    }

    /// 发送 `Hello` 并等待 `Ready`，校验协议版本。
    ///
    /// # 参数
    /// - `state_dir`：可写的能力缓存目录。
    pub fn handshake(&mut self, state_dir: &Path) -> Result<(), OcrError> {
        self.send(Kind::Hello, 0, &hello_payload(&state_dir.to_string_lossy()))?;
        let frame = self.wait(Kind::Ready, 0, self.timeouts.ready, OcrError::ReadyTimeout)?;
        let ready = decode_ready(&frame.payload)?;
        if !ready.success {
            return Err(OcrError::Protocol("worker 报告初始化失败".to_string()));
        }
        if ready.protocol != u32::from(VERSION) {
            return Err(OcrError::VersionMismatch {
                expected: u32::from(VERSION),
                actual: ready.protocol,
            });
        }
        Ok(())
    }

    /// 准备（或复用）识别会话；配置未变时直接返回。
    ///
    /// # 参数
    /// - `config`：会话配置。
    pub fn prepare(&mut self, config: &SessionConfig) -> Result<(), OcrError> {
        if self.prepared.as_ref() == Some(config) {
            return Ok(());
        }
        let id = self.take_id();
        let payload = prepare_session_payload(
            config.directml,
            config.resize_policy,
            &config.detector.to_string_lossy(),
            &config.recognizer.to_string_lossy(),
            &config.dictionary.to_string_lossy(),
        );
        self.send(Kind::PrepareSession, id, &payload)?;
        let frame = self.wait(Kind::SessionReady, id, self.timeouts.prepare, OcrError::Timeout("加载模型"))?;
        if !decode_session_ready(&frame.payload)? {
            self.prepared = None;
            return Err(OcrError::SessionNotReady);
        }
        self.prepared = Some(config.clone());
        Ok(())
    }

    /// 识别一张 RGBA 图像。
    ///
    /// # 参数
    /// - `width` / `height`：图像尺寸（像素总数不得超过 3840×2160，超限请先缩放）。
    /// - `rgba`：紧凑 RGBA 像素，长度须为 `宽 * 高 * 4`。
    ///
    /// # 返回
    /// 识别出的行（坐标是提交图像的像素坐标）；会话须已 [`OcrWorker::prepare`]。
    pub fn recognize(&mut self, width: u32, height: u32, rgba: &[u8]) -> Result<Vec<OcrLine>, OcrError> {
        let pixels = (width as usize).checked_mul(height as usize);
        let expected = pixels.and_then(|p| p.checked_mul(BYTES_PER_PIXEL));
        if width == 0 || height == 0 || pixels.is_none_or(|p| p > MAX_PIXELS) || expected != Some(rgba.len()) {
            return Err(OcrError::InvalidImage(format!(
                "尺寸 {width}x{height} 与像素长度 {} 不符或超出 {MAX_PIXELS} 像素上限",
                rgba.len()
            )));
        }
        let file = write_mapping_file(width, height, rgba)?;
        let generation = self.take_id();
        let result = self.run_recognition(&file, generation, width, height);
        // 无论成败都尽力解除映射，才能删除文件；进程已死则跳过
        if !result.as_ref().err().is_some_and(OcrError::is_fatal_for_worker) {
            let _ = self.detach(generation);
        }
        result
    }

    /// 关闭 worker：发 `Shutdown`，短暂等待应答，然后确保进程结束。
    pub fn shutdown(&mut self) {
        let _ = self.send(Kind::Shutdown, 0, &[]);
        let _ = self.wait(Kind::ShutdownAck, 0, SHUTDOWN_ACK_WAIT, OcrError::Timeout("关闭"));
        if let Some(mut child) = self.child.take() {
            let deadline = Instant::now() + SHUTDOWN_ACK_WAIT;
            while Instant::now() < deadline && child.try_wait().ok().flatten().is_none() {
                std::thread::sleep(Duration::from_millis(20));
            }
            let _ = child.kill();
            let _ = child.wait();
        }
    }

    /// 子进程 PID（管道传输时为 `None`）。
    pub fn pid(&self) -> Option<u32> {
        self.child.as_ref().map(Child::id)
    }

    /// 执行 Attach → Submit → Recognize → Complete。
    fn run_recognition(&mut self, file: &TempFile, generation: u64, width: u32, height: u32) -> Result<Vec<OcrLine>, OcrError> {
        let total = std::fs::metadata(&file.0).map_err(|e| OcrError::Io(e.to_string()))?.len();
        self.send(Kind::AttachBuffer, generation, &attach_buffer_payload(&file.0.to_string_lossy(), total))?;
        self.wait(Kind::BufferAttached, generation, self.timeouts.step, OcrError::Timeout("挂接图像"))?;
        let token = self.take_id();
        let stride = width * BYTES_PER_PIXEL as u32;
        self.send(Kind::Submit, token, &submit_payload(generation, width, height, stride, 1))?;
        let frame = self.wait(Kind::ImageConsumed, token, self.timeouts.step, OcrError::Timeout("传输图像"))?;
        if decode_image_consumed(&frame.payload)? != (generation, 1) {
            return Err(OcrError::Protocol("ImageConsumed 的代号或序号不符".to_string()));
        }
        self.send(Kind::Recognize, token, &[])?;
        let frame = self.wait(Kind::Complete, token, self.timeouts.recognize, OcrError::Timeout("识别"))?;
        match decode_complete(&frame.payload)? {
            CompleteResult::Success(lines) => Ok(lines),
            CompleteResult::Failed(message) => Err(OcrError::Failed(message)),
            CompleteResult::Cancelled(message) => Err(OcrError::Cancelled(message)),
        }
    }

    /// 解除映射并等待确认。
    fn detach(&mut self, generation: u64) -> Result<(), OcrError> {
        self.send(Kind::DetachBuffer, generation, &[])?;
        self.wait(Kind::BufferDetached, generation, self.timeouts.step, OcrError::Timeout("解除映射"))?;
        Ok(())
    }

    /// 取下一个操作号。
    fn take_id(&mut self) -> u64 {
        let id = self.next_id;
        self.next_id += 1;
        id
    }

    /// 发送一帧命令。
    fn send(&mut self, kind: Kind, id: u64, payload: &[u8]) -> Result<(), OcrError> {
        let bytes = encode_frame(kind, id, payload)?;
        self.writer
            .write_all(&bytes)
            .and_then(|()| self.writer.flush())
            .map_err(|_| self.died())
    }

    /// 等待指定类型与操作号的应答；其它帧视为协议错误。
    fn wait(&mut self, kind: Kind, id: u64, timeout: Duration, on_timeout: OcrError) -> Result<Frame, OcrError> {
        match self.rx.recv_timeout(timeout) {
            Ok(Msg::Frame(frame)) if frame.kind == kind && frame.id == id => Ok(frame),
            Ok(Msg::Frame(frame)) => Err(OcrError::Protocol(format!(
                "期望 {kind:?}#{id}，收到 {:?}#{}",
                frame.kind, frame.id
            ))),
            Ok(Msg::Closed) | Err(RecvTimeoutError::Disconnected) => Err(self.died()),
            Err(RecvTimeoutError::Timeout) => Err(on_timeout),
        }
    }

    /// 构造“进程已死”错误，附 stderr 末尾内容。
    fn died(&self) -> OcrError {
        let tail = self.stderr_tail.lock().map(|t| t.trim().to_string()).unwrap_or_default();
        OcrError::ProcessDied(tail)
    }
}

impl Drop for OcrWorker {
    /// 丢弃连接时确保子进程被回收（管道关闭后 worker 自行退出，这里再兜底强杀）。
    fn drop(&mut self) {
        if let Some(mut child) = self.child.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

/// 启动 stderr 读线程，只保留末尾若干字节。
fn spawn_stderr_pump(stderr: impl Read + Send + 'static, tail: Arc<Mutex<String>>) {
    std::thread::spawn(move || {
        let mut reader = BufReader::new(stderr);
        let mut buffer = [0u8; 512];
        while let Ok(n) = reader.read(&mut buffer) {
            if n == 0 {
                break;
            }
            if let Ok(mut text) = tail.lock() {
                text.push_str(&String::from_utf8_lossy(&buffer[..n]));
                if text.len() > STDERR_KEEP_BYTES {
                    let mut cut = text.len() - STDERR_KEEP_BYTES;
                    while !text.is_char_boundary(cut) {
                        cut += 1;
                    }
                    text.drain(..cut);
                }
            }
        }
    });
}

/// 写映射文件：32 字节槽头 + 紧凑 RGBA；返回自动清理的临时文件。
fn write_mapping_file(width: u32, height: u32, rgba: &[u8]) -> Result<TempFile, OcrError> {
    let mut slot = [0u8; SLOT_HEADER_LEN];
    write_slot_header(&mut slot, 1, width, height)?;
    let seq = TEMP_SEQ.fetch_add(1, Ordering::Relaxed);
    let path = std::env::temp_dir().join(format!("{TEMP_FILE_PREFIX}-{}-{seq}.bin", std::process::id()));
    let file = TempFile(path);
    let mut out = std::fs::File::create(&file.0).map_err(|e| OcrError::Io(e.to_string()))?;
    out.write_all(&slot)
        .and_then(|()| out.write_all(rgba))
        .and_then(|()| out.flush())
        .map_err(|e| OcrError::Io(e.to_string()))?;
    Ok(file)
}

#[cfg(test)]
pub(crate) mod fake {
    //! 协议级假 worker：在线程里按 v4 协议应答，并核对客户端写的映射文件。

    use super::*;
    use snow_ocr_protocol::{Ready, complete_payload, image_consumed_payload, ready_payload};
    use std::io::{PipeReader, PipeWriter, pipe};

    /// 假 worker 的 Ready 行为。
    #[derive(Clone, Copy, PartialEq, Eq)]
    pub enum ReadyMode {
        /// 正常应答。
        Normal,
        /// 不应答（模拟启动卡死）。
        Silent,
        /// 报告另一个协议版本。
        WrongProtocol(u32),
    }

    /// 假 worker 脚本。
    #[derive(Clone)]
    pub struct FakeScript {
        /// Ready 行为。
        pub ready: ReadyMode,
        /// 会话准备是否成功。
        pub prepare_ok: bool,
        /// 收到该类命令时直接退出（模拟崩溃）。
        pub die_on: Option<Kind>,
        /// 识别结果。
        pub result: CompleteResult,
        /// 识别耗时。
        pub recognize_delay: Duration,
        /// 记录：收到的命令类型与核对结论。
        pub log: Arc<Mutex<Vec<String>>>,
    }

    impl FakeScript {
        /// 默认脚本：正常应答，识别出一行文字。
        pub fn ok() -> Self {
            Self {
                ready: ReadyMode::Normal,
                prepare_ok: true,
                die_on: None,
                result: CompleteResult::Success(vec![OcrLine {
                    text: "hello".to_string(),
                    score: 0.9,
                    quad: [[10.0, 20.0], [110.0, 20.0], [110.0, 60.0], [10.0, 60.0]],
                }]),
                recognize_delay: Duration::ZERO,
                log: Arc::new(Mutex::new(Vec::new())),
            }
        }

        /// 已记录的日志快照。
        pub fn entries(&self) -> Vec<String> {
            self.log.lock().map(|l| l.clone()).unwrap_or_default()
        }
    }

    /// 读一个 u32 长度前缀字符串。
    fn read_str(data: &mut &[u8]) -> String {
        let len = u32::from_le_bytes(data[..4].try_into().unwrap_or([0; 4])) as usize;
        let s = String::from_utf8_lossy(&data[4..4 + len]).into_owned();
        *data = &data[4 + len..];
        s
    }

    /// 启动假 worker，返回客户端连接（未握手）。
    pub fn spawn_fake(script: FakeScript, timeouts: Timeouts) -> OcrWorker {
        let (cmd_reader, cmd_writer) = pipe().expect("命令管道");
        let (ack_reader, ack_writer) = pipe().expect("应答管道");
        let s = script.clone();
        std::thread::spawn(move || run(s, cmd_reader, ack_writer));
        OcrWorker::from_transport(ack_reader, cmd_writer, None, Arc::new(Mutex::new(String::new())), timeouts)
    }

    /// 假 worker 主循环。
    fn run(script: FakeScript, cmd: PipeReader, mut ack: PipeWriter) {
        let note = |text: String| {
            if let Ok(mut l) = script.log.lock() {
                l.push(text);
            }
        };
        let mut reader = FrameReader::new(BufReader::new(cmd));
        let mut mapping: Option<(PathBuf, u64)> = None;
        let mut reply = |kind: Kind, id: u64, payload: &[u8]| {
            if let Ok(bytes) = encode_frame(kind, id, payload) {
                let _ = ack.write_all(&bytes).and_then(|()| ack.flush());
            }
        };
        while let Ok(frame) = reader.read_frame() {
            note(format!("{:?}", frame.kind));
            if script.die_on == Some(frame.kind) {
                return;
            }
            match frame.kind {
                Kind::Hello => match script.ready {
                    ReadyMode::Silent => {}
                    ReadyMode::Normal | ReadyMode::WrongProtocol(_) => {
                        let protocol = match script.ready {
                            ReadyMode::WrongProtocol(v) => v,
                            _ => u32::from(VERSION),
                        };
                        let ready = Ready {
                            success: true,
                            capability: 0,
                            provider: "unloaded".into(),
                            runtime_version: "fake".into(),
                            protocol,
                        };
                        reply(Kind::Ready, 0, &ready_payload(&ready));
                    }
                },
                Kind::PrepareSession => {
                    let mut p = &frame.payload[..];
                    let (directml, policy) = (p[0], p[1]);
                    p = &p[2..];
                    let det = read_str(&mut p);
                    note(format!("prepare directml={directml} policy={policy} det={det}"));
                    reply(Kind::SessionReady, frame.id, &[u8::from(script.prepare_ok)]);
                }
                Kind::AttachBuffer => {
                    let mut p = &frame.payload[..];
                    let path = read_str(&mut p);
                    let total = u64::from_le_bytes(p[..8].try_into().unwrap_or([0; 8]));
                    mapping = Some((PathBuf::from(path), total));
                    reply(Kind::BufferAttached, frame.id, &[]);
                }
                Kind::Submit => {
                    let p = &frame.payload;
                    let generation = u64::from_le_bytes(p[0..8].try_into().unwrap_or([0; 8]));
                    let w = u32::from_le_bytes(p[8..12].try_into().unwrap_or([0; 4]));
                    let h = u32::from_le_bytes(p[12..16].try_into().unwrap_or([0; 4]));
                    let stride = u32::from_le_bytes(p[16..20].try_into().unwrap_or([0; 4]));
                    let seq = u64::from_le_bytes(p[20..28].try_into().unwrap_or([0; 8]));
                    if let Some((path, total)) = &mapping {
                        let bytes = std::fs::read(path).unwrap_or_default();
                        let header_ok = bytes.len() as u64 == *total
                            && bytes.len() >= SLOT_HEADER_LEN
                            && u64::from_le_bytes(bytes[0..8].try_into().unwrap_or([0; 8])) == seq
                            && u32::from_le_bytes(bytes[8..12].try_into().unwrap_or([0; 4])) == 1
                            && u32::from_le_bytes(bytes[12..16].try_into().unwrap_or([0; 4])) == w
                            && u32::from_le_bytes(bytes[16..20].try_into().unwrap_or([0; 4])) == h
                            && u32::from_le_bytes(bytes[20..24].try_into().unwrap_or([0; 4])) == stride
                            && stride == w * 4;
                        let first: Vec<u8> = bytes.get(SLOT_HEADER_LEN..SLOT_HEADER_LEN + 4).map(<[u8]>::to_vec).unwrap_or_default();
                        note(format!("slot header_ok={header_ok} first_pixel={first:?} size={w}x{h}"));
                    }
                    reply(Kind::ImageConsumed, frame.id, &image_consumed_payload(generation, seq));
                }
                Kind::Recognize => {
                    std::thread::sleep(script.recognize_delay);
                    reply(Kind::Complete, frame.id, &complete_payload(&script.result));
                }
                Kind::DetachBuffer => {
                    mapping = None;
                    reply(Kind::BufferDetached, frame.id, &[]);
                }
                Kind::Shutdown => {
                    reply(Kind::ShutdownAck, 0, &[]);
                    return;
                }
                _ => {}
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::fake::{FakeScript, ReadyMode, spawn_fake};
    use super::*;

    /// 快速超时（测试用）。
    fn fast() -> Timeouts {
        Timeouts {
            ready: Duration::from_millis(300),
            prepare: Duration::from_secs(5),
            step: Duration::from_secs(5),
            recognize: Duration::from_secs(5),
        }
    }

    /// 测试用会话配置。
    fn config() -> SessionConfig {
        SessionConfig {
            directml: false,
            resize_policy: 0,
            detector: PathBuf::from("det.onnx"),
            recognizer: PathBuf::from("rec.onnx"),
            dictionary: PathBuf::from("dict.txt"),
        }
    }

    /// 2x1 的 RGBA 测试图。
    fn tiny_image() -> Vec<u8> {
        vec![11, 22, 33, 255, 44, 55, 66, 255]
    }

    /// 完整时序：握手、准备、识别；假 worker 核对了槽头与首像素（RGBA 原样），文件随后被清理。
    #[test]
    fn full_recognition_flow() {
        let script = FakeScript::ok();
        let mut worker = spawn_fake(script.clone(), fast());
        worker.handshake(Path::new("state")).expect("握手");
        worker.prepare(&config()).expect("准备");
        let lines = worker.recognize(2, 1, &tiny_image()).expect("识别");
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].text, "hello");
        let log = script.entries();
        assert!(log.contains(&"slot header_ok=true first_pixel=[11, 22, 33, 255] size=2x1".to_string()), "{log:?}");
        let kinds: Vec<&str> = log.iter().map(String::as_str).filter(|s| !s.contains(' ')).collect();
        assert_eq!(
            kinds,
            ["Hello", "PrepareSession", "AttachBuffer", "Submit", "Recognize", "DetachBuffer"]
        );
        worker.shutdown();
    }

    /// 相同配置不重复准备会话。
    #[test]
    fn prepare_is_idempotent() {
        let script = FakeScript::ok();
        let mut worker = spawn_fake(script.clone(), fast());
        worker.handshake(Path::new("s")).expect("握手");
        worker.prepare(&config()).expect("准备");
        worker.prepare(&config()).expect("再次准备");
        let prepares = script.entries().iter().filter(|e| e.as_str() == "PrepareSession").count();
        assert_eq!(prepares, 1);
        let mut other = config();
        other.directml = true;
        worker.prepare(&other).expect("换配置重新准备");
        assert_eq!(script.entries().iter().filter(|e| e.as_str() == "PrepareSession").count(), 2);
    }

    /// worker 不应答 Ready：超时返回 ReadyTimeout。
    #[test]
    fn ready_timeout() {
        let script = FakeScript { ready: ReadyMode::Silent, ..FakeScript::ok() };
        let mut worker = spawn_fake(script, fast());
        assert_eq!(worker.handshake(Path::new("s")), Err(OcrError::ReadyTimeout));
    }

    /// 协议版本不符：给出版本不匹配错误。
    #[test]
    fn protocol_version_mismatch() {
        let script = FakeScript { ready: ReadyMode::WrongProtocol(3), ..FakeScript::ok() };
        let mut worker = spawn_fake(script, fast());
        assert_eq!(
            worker.handshake(Path::new("s")),
            Err(OcrError::VersionMismatch { expected: 4, actual: 3 })
        );
    }

    /// 会话准备失败（模型加载失败）：返回 SessionNotReady，不是假文本。
    #[test]
    fn prepare_failure_is_reported() {
        let script = FakeScript { prepare_ok: false, ..FakeScript::ok() };
        let mut worker = spawn_fake(script, fast());
        worker.handshake(Path::new("s")).expect("握手");
        assert_eq!(worker.prepare(&config()), Err(OcrError::SessionNotReady));
        assert!(OcrError::SessionNotReady.message().contains("模型加载失败"));
    }

    /// 进程在识别中途退出：返回 ProcessDied，且判定 worker 不可继续使用。
    #[test]
    fn process_death_mid_recognition() {
        let script = FakeScript { die_on: Some(Kind::Recognize), ..FakeScript::ok() };
        let mut worker = spawn_fake(script, fast());
        worker.handshake(Path::new("s")).expect("握手");
        worker.prepare(&config()).expect("准备");
        let err = worker.recognize(2, 1, &tiny_image()).unwrap_err();
        assert!(matches!(err, OcrError::ProcessDied(_)), "{err:?}");
        assert!(err.is_fatal_for_worker());
    }

    /// worker 报告识别失败 / 取消：原样带回，worker 仍可继续使用。
    #[test]
    fn worker_reported_failure_and_cancel() {
        let script = FakeScript { result: CompleteResult::Failed("boom".into()), ..FakeScript::ok() };
        let mut worker = spawn_fake(script, fast());
        worker.handshake(Path::new("s")).expect("握手");
        worker.prepare(&config()).expect("准备");
        let err = worker.recognize(2, 1, &tiny_image()).unwrap_err();
        assert_eq!(err, OcrError::Failed("boom".into()));
        assert!(!err.is_fatal_for_worker());
        // 失败后还能继续下一次
        assert!(worker.recognize(2, 1, &tiny_image()).is_err());

        let script = FakeScript { result: CompleteResult::Cancelled("cancelled".into()), ..FakeScript::ok() };
        let mut worker = spawn_fake(script, fast());
        worker.handshake(Path::new("s")).expect("握手");
        worker.prepare(&config()).expect("准备");
        assert!(matches!(worker.recognize(2, 1, &tiny_image()), Err(OcrError::Cancelled(_))));
    }

    /// 识别超时：返回 Timeout。
    #[test]
    fn recognition_timeout() {
        let mut t = fast();
        t.recognize = Duration::from_millis(100);
        let script = FakeScript { recognize_delay: Duration::from_millis(600), ..FakeScript::ok() };
        let mut worker = spawn_fake(script, t);
        worker.handshake(Path::new("s")).expect("握手");
        worker.prepare(&config()).expect("准备");
        assert_eq!(worker.recognize(2, 1, &tiny_image()), Err(OcrError::Timeout("识别")));
    }

    /// 非法图像：零尺寸、长度不符、超上限、乘法溢出都在发送前被拒绝。
    #[test]
    fn invalid_images_are_rejected_locally() {
        let mut worker = spawn_fake(FakeScript::ok(), fast());
        worker.handshake(Path::new("s")).expect("握手");
        assert!(matches!(worker.recognize(0, 1, &[]), Err(OcrError::InvalidImage(_))));
        assert!(matches!(worker.recognize(2, 1, &[0; 7]), Err(OcrError::InvalidImage(_))));
        assert!(matches!(worker.recognize(3841, 2161, &[0; 16]), Err(OcrError::InvalidImage(_))));
        assert!(matches!(worker.recognize(u32::MAX, u32::MAX, &[0; 4]), Err(OcrError::InvalidImage(_))));
    }

    /// 真实进程：exe 不存在时拉起失败，不 panic。
    #[test]
    fn spawn_missing_exe_fails() {
        let assets = OcrAssets {
            exe: PathBuf::from("Z:/no/such/snow-ocr-process.exe"),
            detector: PathBuf::new(),
            recognizer: PathBuf::new(),
            dictionary: PathBuf::new(),
            state_dir: std::env::temp_dir().join("snow-ocr-missing-state"),
            model_id: "x".into(),
        };
        assert!(matches!(OcrWorker::spawn(&assets, fast()), Err(OcrError::SpawnFailed(_))));
    }

    /// 错误文案：区分各类原因且可下载性正确。
    #[test]
    fn error_messages_are_distinct() {
        let all = [
            OcrError::Unavailable(OcrUnavailable::NoRuntime),
            OcrError::SpawnFailed("x".into()),
            OcrError::ReadyTimeout,
            OcrError::VersionMismatch { expected: 4, actual: 3 },
            OcrError::ProcessDied("stderr tail".into()),
            OcrError::SessionNotReady,
            OcrError::Failed("f".into()),
            OcrError::Timeout("识别"),
            OcrError::Protocol("p".into()),
            OcrError::Io("i".into()),
            OcrError::InvalidImage("v".into()),
        ];
        let messages: std::collections::HashSet<String> = all.iter().map(OcrError::message).collect();
        assert_eq!(messages.len(), all.len());
        assert!(OcrError::Unavailable(OcrUnavailable::NoRuntime).can_download());
        assert!(!OcrError::ReadyTimeout.can_download());
        assert!(OcrError::ProcessDied("stderr tail".into()).message().contains("stderr tail"));
    }
}
