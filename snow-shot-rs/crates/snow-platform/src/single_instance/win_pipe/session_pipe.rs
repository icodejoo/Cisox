//! 双工会话管道：托盘宿主与 UI 进程之间的长连接，一行一条消息，两端都能随时主动写。
//!
//! 和 [`line_pipe`](super::line_pipe) 的区别：`line_pipe` 是“收一行、回一行”的请求应答，服务端
//! 没法主动推送；这里读写各用一个事件，读线程阻塞等行的同时，别的线程可以随时写。
//! 安全边界与单实例命令通道相同（当前用户 ACL、拒绝远程客户端、首实例防抢占、全程带超时的重叠 IO）。
//! 一个服务端只接一个连接：连接断开即结束，宿主每次拉起 UI 都新建一条会话管道。

use super::line_pipe::{LineBuffer, MAX_LINE_BYTES};
use super::*;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};

/// 写一行的超时（毫秒）：对端不读且缓冲写满时，写方在这之后放弃，不拖死调用线程。
const SESSION_WRITE_TIMEOUT_MS: u32 = 2000;
/// 单次读取块大小。
const SESSION_READ_CHUNK_BYTES: usize = 8192;
/// 客户端连接总时限（宿主先建管道再拉起 UI，所以正常情况下立即连得上）。
const SESSION_CONNECT_TIMEOUT: Duration = Duration::from_secs(5);

/// 服务端读线程上报的事件。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SessionEvent {
    /// 客户端连上了（带客户端进程 ID，宿主据此核对是自己拉起的那个 UI）。
    Connected {
        /// 客户端进程 ID。
        client_pid: u32,
    },
    /// 收到一整行（已去掉换行；非 UTF-8 的行在内部丢弃并记日志，不会上报）。
    Line(String),
    /// 连接结束（对端退出、管道被关、服务端被丢弃）；此后不再有事件。
    Disconnected,
}

/// 管道两端共享的状态：句柄、写锁与停止事件。
struct Shared {
    /// 管道句柄（读线程与写方共用，重叠 IO 允许同一句柄并发读写）。
    pipe: OwnedHandle,
    /// 写事件（读线程用自己的事件，两者互不干扰）。
    write_event: OwnedHandle,
    /// 停止事件：置位后阻塞中的读立即返回，之后的写直接失败。
    stop: OwnedHandle,
    /// 写锁：多线程同时写时，保证一行不被穿插。
    write_lock: Mutex<()>,
    /// 当前是否有活着的连接（服务端：连上后为真；断开后为假）。
    connected: AtomicBool,
}

/// 写端句柄：可克隆、可跨线程，随时写一行。
#[derive(Clone)]
pub struct PipeWriter {
    /// 共享状态。
    shared: Arc<Shared>,
}

impl PipeWriter {
    /// 写一行（自动补换行）。
    ///
    /// # 参数
    /// - `line`：不含换行的文本（紧凑 JSON 不含裸换行）。
    ///
    /// # 返回
    /// 成功为 `Ok(())`；尚未连接、已断开、超时或管道错误返回原因。
    ///
    /// ```no_run
    /// # fn demo(writer: &snow_platform::single_instance::session_pipe::PipeWriter) {
    /// writer.send_line("{\"type\":\"ack\",\"seq\":1}").unwrap();
    /// # }
    /// ```
    pub fn send_line(&self, line: &str) -> Result<(), String> {
        if !self.shared.connected.load(Ordering::SeqCst) {
            return Err("会话管道未连接".into());
        }
        let mut bytes = Vec::with_capacity(line.len() + 1);
        bytes.extend_from_slice(line.as_bytes());
        bytes.push(b'\n');
        let _guard = self
            .shared
            .write_lock
            .lock()
            .map_err(|_| "会话管道写锁已损坏".to_string())?;
        write_all(
            self.shared.pipe.0,
            self.shared.write_event.0,
            self.shared.stop.0,
            &bytes,
            SESSION_WRITE_TIMEOUT_MS,
        )
        .map_err(|o| format!("会话管道写入失败: {}", outcome_text(&o)))
    }

    /// 当前是否有活着的连接。
    pub fn is_connected(&self) -> bool {
        self.shared.connected.load(Ordering::SeqCst)
    }

    /// 主动关闭：置位停止事件，阻塞中的读立刻返回，之后的写失败。可重复调用。
    pub fn close(&self) {
        // SAFETY: 停止事件句柄由 `Shared` 持有，存活期间有效。
        unsafe {
            let _ = SetEvent(self.shared.stop.0);
        }
        self.shared.connected.store(false, Ordering::SeqCst);
    }
}

/// 读到的行转成文本；非 UTF-8 的行丢弃并记日志。
fn line_text(raw: Vec<u8>) -> Option<String> {
    match String::from_utf8(raw) {
        Ok(text) => Some(text),
        Err(_) => {
            tracing::warn!("会话管道收到非 UTF-8 的行，已丢弃");
            None
        }
    }
}

/// 创建共享状态。
fn new_shared(pipe: OwnedHandle, stop: OwnedHandle) -> Result<Arc<Shared>, String> {
    Ok(Arc::new(Shared {
        pipe,
        write_event: create_event()?,
        stop,
        write_lock: Mutex::new(()),
        connected: AtomicBool::new(false),
    }))
}

/// 会话管道服务端：后台线程等一个连接、读行并上报；丢弃即停止并回收线程。
pub struct SessionPipeServer {
    /// 共享状态（给写端和 `Drop` 用）。
    shared: Arc<Shared>,
    /// 读线程。
    join: Option<JoinHandle<()>>,
}

impl SessionPipeServer {
    /// 创建管道实例（同步返回错误，名字被占用会失败）并启动读线程。
    ///
    /// 宿主先调用它、再 `CreateProcess`，所以 UI 一启动就连得上。
    ///
    /// # 参数
    /// - `name`：管道完整名（见 [`line_pipe::pipe_name_for`](super::line_pipe::pipe_name_for)）。
    /// - `on_event`：在读线程上被调用，应只做轻量转发（如发到宿主的事件通道）。
    ///
    /// # 返回
    /// 服务端句柄；名字被占用、ACL 创建失败等返回原因。
    ///
    /// ```no_run
    /// use snow_platform::single_instance::line_pipe::pipe_name_for;
    /// use snow_platform::single_instance::session_pipe::{SessionEvent, SessionPipeServer};
    /// let name = pipe_name_for("cisox.demo.session").unwrap();
    /// let server = SessionPipeServer::start(&name, |event| println!("{event:?}")).unwrap();
    /// let writer = server.writer();
    /// let _ = writer.send_line("{}");
    /// ```
    pub fn start(
        name: &str,
        on_event: impl Fn(SessionEvent) + Send + 'static,
    ) -> Result<Self, String> {
        let sid = current_user_sid()?;
        let acl = UserOnlyAcl::for_sid(&sid)?;
        let pipe = create_pipe_instance(name, true, &acl)?;
        let read_event = create_event()?;
        let shared = new_shared(pipe, create_event()?)?;
        let thread_shared = Arc::clone(&shared);
        let join = std::thread::Builder::new()
            .name("session-pipe".into())
            .spawn(move || {
                // ACL 需在线程内保持到管道关闭（管道创建后系统已复制描述符，这里仅随线程释放）
                let _acl = acl;
                serve_session(&thread_shared, &read_event, &on_event);
            })
            .map_err(|e| format!("创建会话管道线程失败: {e}"))?;
        Ok(Self {
            shared,
            join: Some(join),
        })
    }

    /// 取写端句柄（可克隆给任意线程）；连接建立前写会失败。
    pub fn writer(&self) -> PipeWriter {
        PipeWriter {
            shared: Arc::clone(&self.shared),
        }
    }
}

impl Drop for SessionPipeServer {
    /// 置位停止事件并等待读线程退出。
    fn drop(&mut self) {
        // SAFETY: 停止事件句柄由 `Shared` 持有，存活期间有效。
        unsafe {
            let _ = SetEvent(self.shared.stop.0);
        }
        if let Some(j) = self.join.take() {
            let _ = j.join();
        }
    }
}

/// 读线程主体：等连接 → 上报 → 读行直到断开 → 上报断开。
fn serve_session(shared: &Shared, read_event: &OwnedHandle, on_event: &dyn Fn(SessionEvent)) {
    let pipe = shared.pipe.0;
    let stop = shared.stop.0;
    let connected = unsafe {
        overlapped_io(pipe, read_event.0, stop, WAIT_FOREVER_MS, |ov| {
            ConnectNamedPipe(pipe, Some(ov))
        })
    };
    if !matches!(connected, IoOutcome::Done(_)) {
        if !matches!(connected, IoOutcome::Stopped) {
            tracing::warn!(reason = %outcome_text(&connected), "会话管道接入失败");
        }
        on_event(SessionEvent::Disconnected);
        return;
    }
    let mut client_pid = 0u32;
    // SAFETY: `client_pid` 是有效出参；`pipe` 是本线程持有的有效管道句柄。
    unsafe {
        let _ = GetNamedPipeClientProcessId(pipe, &mut client_pid);
    }
    shared.connected.store(true, Ordering::SeqCst);
    on_event(SessionEvent::Connected { client_pid });
    read_lines(shared, read_event, |line| {
        on_event(SessionEvent::Line(line))
    });
    shared.connected.store(false, Ordering::SeqCst);
    // SAFETY: 管道句柄有效；断开后写方会因 `connected` 为假而直接失败。
    unsafe {
        let _ = DisconnectNamedPipe(pipe);
    }
    on_event(SessionEvent::Disconnected);
}

/// 阻塞读行，每凑齐一行调用一次 `on_line`，直到断开、停止或出错。
fn read_lines(shared: &Shared, read_event: &OwnedHandle, mut on_line: impl FnMut(String)) {
    let mut buffer = LineBuffer::default();
    let mut chunk = vec![0u8; SESSION_READ_CHUNK_BYTES];
    loop {
        let outcome = unsafe {
            overlapped_io(
                shared.pipe.0,
                read_event.0,
                shared.stop.0,
                WAIT_FOREVER_MS,
                |ov| ReadFile(shared.pipe.0, Some(&mut chunk), None, Some(ov)),
            )
        };
        match outcome {
            IoOutcome::Done(0) | IoOutcome::Closed | IoOutcome::Stopped => return,
            IoOutcome::Done(n) => buffer.push(&chunk[..n as usize]),
            other => {
                tracing::debug!(reason = %outcome_text(&other), "会话管道读取结束");
                return;
            }
        }
        while let Some(raw) = buffer.next_line() {
            if let Some(text) = line_text(raw) {
                on_line(text);
            }
        }
        if buffer.len() > MAX_LINE_BYTES {
            tracing::warn!("会话管道收到超长行，已断开");
            return;
        }
    }
}

/// 会话管道客户端（UI 进程用）：连上宿主预先建好的管道，读线程里阻塞读行，别的线程写。
pub struct SessionPipeClient {
    /// 共享状态。
    shared: Arc<Shared>,
    /// 读事件。
    read_event: OwnedHandle,
    /// 行缓冲。
    buffer: LineBuffer,
}

impl SessionPipeClient {
    /// 连接服务端管道（忙或尚未创建时短间隔重试，整体 5 秒）。
    ///
    /// # 参数
    /// - `name`：管道完整名。
    ///
    /// # 返回
    /// 客户端；超时或失败返回原因。
    ///
    /// ```no_run
    /// use snow_platform::single_instance::line_pipe::pipe_name_for;
    /// use snow_platform::single_instance::session_pipe::SessionPipeClient;
    /// let name = pipe_name_for("cisox.demo.session").unwrap();
    /// let client = SessionPipeClient::connect(&name).unwrap();
    /// client.writer().send_line("{}").unwrap();
    /// ```
    pub fn connect(name: &str) -> Result<Self, String> {
        let deadline = Instant::now() + SESSION_CONNECT_TIMEOUT;
        let wide = HSTRING::from(name);
        let pipe = loop {
            let opened = unsafe {
                CreateFileW(
                    &wide,
                    GENERIC_READ.0 | GENERIC_WRITE.0,
                    FILE_SHARE_NONE,
                    None,
                    OPEN_EXISTING,
                    FILE_FLAG_OVERLAPPED | SECURITY_SQOS_PRESENT | SECURITY_IDENTIFICATION,
                    None,
                )
            };
            match opened {
                Ok(h) => break OwnedHandle(h),
                Err(e) if Instant::now() < deadline => {
                    if is_win32(&e, ERROR_PIPE_BUSY) {
                        unsafe {
                            let _ = WaitNamedPipeW(&wide, CLIENT_RETRY_INTERVAL.as_millis() as u32);
                        }
                    } else if is_win32(&e, ERROR_FILE_NOT_FOUND) {
                        std::thread::sleep(CLIENT_RETRY_INTERVAL);
                    } else {
                        return Err(format!("连接会话管道失败: {}", describe(&e)));
                    }
                }
                Err(e) => return Err(format!("连接会话管道超时: {}", describe(&e))),
            }
        };
        let shared = new_shared(pipe, create_event()?)?;
        shared.connected.store(true, Ordering::SeqCst);
        Ok(Self {
            shared,
            read_event: create_event()?,
            buffer: LineBuffer::default(),
        })
    }

    /// 取写端句柄（可克隆给任意线程；`close` 可让阻塞中的 `read_line` 返回）。
    pub fn writer(&self) -> PipeWriter {
        PipeWriter {
            shared: Arc::clone(&self.shared),
        }
    }

    /// 阻塞读一行。
    ///
    /// # 参数
    /// - `timeout`：等待上限；`None` 表示一直等到有行、断开或被 `close`。
    ///
    /// # 返回
    /// `Ok(Some(行))`；超时 `Ok(None)`；对端断开、被关闭、出错或收到非 UTF-8 / 超长行返回 `Err`（此后不应再读）。
    ///
    /// ```no_run
    /// # fn demo(mut client: snow_platform::single_instance::session_pipe::SessionPipeClient) {
    /// while let Ok(Some(line)) = client.read_line(None) {
    ///     println!("{line}");
    /// }
    /// # }
    /// ```
    pub fn read_line(&mut self, timeout: Option<Duration>) -> Result<Option<String>, String> {
        let deadline = timeout.map(|t| Instant::now() + t);
        let mut chunk = vec![0u8; SESSION_READ_CHUNK_BYTES];
        loop {
            if let Some(raw) = self.buffer.next_line() {
                match line_text(raw) {
                    Some(text) => return Ok(Some(text)),
                    None => continue,
                }
            }
            if self.buffer.len() > MAX_LINE_BYTES {
                return Err("收到超长行".into());
            }
            let wait_ms = match deadline {
                None => WAIT_FOREVER_MS,
                Some(d) => {
                    let remaining = d.saturating_duration_since(Instant::now());
                    if remaining.is_zero() {
                        return Ok(None);
                    }
                    remaining.as_millis().min(u128::from(u32::MAX - 1)) as u32
                }
            };
            let pipe = self.shared.pipe.0;
            let outcome = unsafe {
                overlapped_io(pipe, self.read_event.0, self.shared.stop.0, wait_ms, |ov| {
                    ReadFile(pipe, Some(&mut chunk), None, Some(ov))
                })
            };
            match outcome {
                IoOutcome::Done(0) | IoOutcome::Closed => {
                    self.shared.connected.store(false, Ordering::SeqCst);
                    return Err("对端已断开".into());
                }
                IoOutcome::Done(n) => self.buffer.push(&chunk[..n as usize]),
                IoOutcome::Timeout => return Ok(None),
                IoOutcome::Stopped => {
                    self.shared.connected.store(false, Ordering::SeqCst);
                    return Err("会话已关闭".into());
                }
                IoOutcome::Failed(reason) => {
                    self.shared.connected.store(false, Ordering::SeqCst);
                    return Err(format!("读取失败: {reason}"));
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc::{Receiver, channel};

    /// 事件等待上限。
    const WAIT: Duration = Duration::from_secs(3);

    /// 生成唯一管道名。
    fn unique_name(tag: &str) -> String {
        format!(
            "{PIPE_NAMESPACE}cisox-session-test-{tag}-{}",
            std::process::id()
        )
    }

    /// 启动服务端并把事件转进通道。
    fn start_server(name: &str) -> (SessionPipeServer, Receiver<SessionEvent>) {
        let (tx, rx) = channel();
        let tx = Mutex::new(tx);
        let server = SessionPipeServer::start(name, move |event| {
            let _ = tx.lock().unwrap().send(event);
        })
        .unwrap();
        (server, rx)
    }

    /// 双工往返：服务端能主动推送，客户端能主动发送，顺序保持，连接事件带客户端进程 ID。
    #[test]
    fn duplex_push_and_reply_keep_order() {
        let name = unique_name("duplex");
        let (server, events) = start_server(&name);
        let mut client = SessionPipeClient::connect(&name).unwrap();
        assert_eq!(
            events.recv_timeout(WAIT).unwrap(),
            SessionEvent::Connected {
                client_pid: std::process::id()
            }
        );
        // 服务端不等任何请求，直接推两行
        let writer = server.writer();
        writer.send_line("push-1").unwrap();
        writer.send_line("push-2").unwrap();
        assert_eq!(
            client.read_line(Some(WAIT)).unwrap().as_deref(),
            Some("push-1")
        );
        assert_eq!(
            client.read_line(Some(WAIT)).unwrap().as_deref(),
            Some("push-2")
        );
        // 客户端回两行
        let client_writer = client.writer();
        client_writer.send_line("reply-1").unwrap();
        client_writer.send_line("reply-2").unwrap();
        assert_eq!(
            events.recv_timeout(WAIT).unwrap(),
            SessionEvent::Line("reply-1".into())
        );
        assert_eq!(
            events.recv_timeout(WAIT).unwrap(),
            SessionEvent::Line("reply-2".into())
        );
        // 管道句柄由客户端与它的写端克隆共享，两者都放掉连接才真正关闭
        drop(client_writer);
        drop(client);
        assert_eq!(
            events.recv_timeout(WAIT).unwrap(),
            SessionEvent::Disconnected
        );
        assert!(writer.send_line("late").is_err(), "断开后写入必须失败");
    }

    /// 读空闲时 `read_line` 带超时返回 `None`，不会卡死也不会丢连接。
    #[test]
    fn read_line_times_out_without_dropping_connection() {
        let name = unique_name("timeout");
        let (server, _events) = start_server(&name);
        let mut client = SessionPipeClient::connect(&name).unwrap();
        let start = Instant::now();
        assert_eq!(
            client.read_line(Some(Duration::from_millis(150))).unwrap(),
            None
        );
        assert!(start.elapsed() >= Duration::from_millis(100));
        server.writer().send_line("still-alive").unwrap();
        assert_eq!(
            client.read_line(Some(WAIT)).unwrap().as_deref(),
            Some("still-alive")
        );
    }

    /// 服务端被丢弃：客户端的阻塞读以错误返回。
    #[test]
    fn dropping_server_unblocks_client_read() {
        let name = unique_name("drop");
        let (server, events) = start_server(&name);
        let mut client = SessionPipeClient::connect(&name).unwrap();
        let _ = events.recv_timeout(WAIT).unwrap();
        let reader = std::thread::spawn(move || client.read_line(None));
        std::thread::sleep(Duration::from_millis(100));
        drop(server);
        let result = reader.join().unwrap();
        assert!(result.is_err(), "服务端消失后读应报断开: {result:?}");
    }

    /// 客户端 `close`：阻塞中的读立刻返回错误，写入随后失败。
    #[test]
    fn client_close_unblocks_its_own_read() {
        let name = unique_name("close");
        let (_server, events) = start_server(&name);
        let mut client = SessionPipeClient::connect(&name).unwrap();
        let _ = events.recv_timeout(WAIT).unwrap();
        let writer = client.writer();
        let reader = std::thread::spawn(move || client.read_line(None));
        std::thread::sleep(Duration::from_millis(100));
        writer.close();
        assert!(reader.join().unwrap().is_err());
        assert!(writer.send_line("x").is_err());
        assert!(!writer.is_connected());
    }

    /// 同名第二个服务端被拒（首实例防抢占）。
    #[test]
    fn name_squatting_is_rejected() {
        let name = unique_name("squat");
        let (_server, _events) = start_server(&name);
        assert!(SessionPipeServer::start(&name, |_| {}).is_err());
    }

    /// 多线程同时写：每一行都完整到达，不被穿插。
    #[test]
    fn concurrent_writers_do_not_interleave_lines() {
        const THREADS: usize = 4;
        const PER_THREAD: usize = 40;
        let name = unique_name("concurrent");
        let (server, _events) = start_server(&name);
        let mut client = SessionPipeClient::connect(&name).unwrap();
        // 等服务端把连接登记完
        let deadline = Instant::now() + WAIT;
        while !server.writer().is_connected() {
            assert!(Instant::now() < deadline, "服务端未登记连接");
            std::thread::sleep(Duration::from_millis(5));
        }
        let long = "x".repeat(300);
        let handles: Vec<_> = (0..THREADS)
            .map(|t| {
                let writer = server.writer();
                let long = long.clone();
                std::thread::spawn(move || {
                    for i in 0..PER_THREAD {
                        writer.send_line(&format!("{t}:{i}:{long}")).unwrap();
                    }
                })
            })
            .collect();
        let mut seen = [0usize; THREADS];
        for _ in 0..THREADS * PER_THREAD {
            let line = client.read_line(Some(WAIT)).unwrap().expect("按时收到");
            let mut parts = line.splitn(3, ':');
            let t: usize = parts.next().unwrap().parse().unwrap();
            let i: usize = parts.next().unwrap().parse().unwrap();
            assert_eq!(parts.next().unwrap(), long, "行内容被穿插");
            assert_eq!(i, seen[t], "同一线程的行必须保序");
            seen[t] += 1;
        }
        for h in handles {
            h.join().unwrap();
        }
        assert!(seen.iter().all(|n| *n == PER_THREAD));
    }

    /// 没有客户端连接时写入立即失败，不阻塞。
    #[test]
    fn write_before_connect_fails_fast() {
        let name = unique_name("early");
        let (server, _events) = start_server(&name);
        let start = Instant::now();
        assert!(server.writer().send_line("nobody").is_err());
        assert!(start.elapsed() < Duration::from_millis(200));
    }

    /// 非 UTF-8 的行被丢弃，之后的正常行照常上报。
    #[test]
    fn invalid_utf8_line_is_dropped() {
        let name = unique_name("utf8");
        let (_server, events) = start_server(&name);
        let client = SessionPipeClient::connect(&name).unwrap();
        let _ = events.recv_timeout(WAIT).unwrap();
        // 直接写原始字节：先一行坏数据，再一行好数据
        let writer = client.writer();
        let write_raw = |bytes: &[u8]| {
            let _guard = writer.shared.write_lock.lock().unwrap();
            write_all(
                writer.shared.pipe.0,
                writer.shared.write_event.0,
                writer.shared.stop.0,
                bytes,
                SESSION_WRITE_TIMEOUT_MS,
            )
            .unwrap();
        };
        write_raw(&[0xff, 0xfe, b'\n']);
        write_raw(b"good\n");
        assert_eq!(
            events.recv_timeout(WAIT).unwrap(),
            SessionEvent::Line("good".into())
        );
    }
}
