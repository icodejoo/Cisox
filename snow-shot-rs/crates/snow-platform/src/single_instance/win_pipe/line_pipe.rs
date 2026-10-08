//! 按行（`\n` 分隔）收发文本的命名管道：服务端与客户端。
//!
//! 与单实例命令通道共用同一套安全边界（当前用户 ACL、拒绝远程客户端、首实例防抢占、
//! 全程带超时的重叠 IO），但连接是长连接、串行服务（同一时刻只接一个客户端）。
//! 本模块不理解行内容，协议由调用方的 [`LineSession`] 决定。

use super::*;

/// 单行最大字节数（超过即断开）。
pub const MAX_LINE_BYTES: usize = 1024 * 1024;
/// 服务端读空闲上限（毫秒）；客户端超过该时长不发数据即断开，让出管道。
const SERVER_IDLE_TIMEOUT_MS: u32 = 5 * 60 * 1000;
/// 服务端写应答超时（毫秒）。
const LINE_WRITE_TIMEOUT_MS: u32 = 2000;
/// 单次读取块大小。
const READ_CHUNK_BYTES: usize = 8192;
/// 客户端连接总时限。
const CLIENT_CONNECT_TIMEOUT: Duration = Duration::from_secs(2);

/// 会话对一行输入的回应。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LineReply {
    /// 要回写给客户端的一行（不含换行）；`None` 表示不回。
    pub text: Option<String>,
    /// 回写后是否断开该连接。
    pub close: bool,
}

/// 一个连接的会话：每收到一行调用一次；会话对象随连接创建、断开即丢弃。
pub trait LineSession: Send {
    /// 处理一行输入（已去掉换行）。
    ///
    /// # 参数
    /// - `line`：收到的一行文本。
    ///
    /// # 返回
    /// 回应与是否断开。
    fn on_line(&mut self, line: &str) -> LineReply;
}

/// 为每个新连接创建会话的工厂。
pub type SessionFactory = Box<dyn Fn() -> Box<dyn LineSession> + Send>;

/// 计算当前用户专属的管道完整名：`\\.\pipe\<app_id>-<SID>-<会话号>`。
///
/// # 参数
/// - `app_id`：仅允许字母、数字、`.`、`_`、`-`。
///
/// # 返回
/// 管道完整名；标识非法或系统调用失败返回原因。
pub fn pipe_name_for(app_id: &str) -> Result<String, String> {
    pipe_name(app_id)
}

/// 行缓冲：把字节流切成行。
#[derive(Default)]
struct LineBuffer {
    /// 尚未取走的字节。
    pending: Vec<u8>,
}

impl LineBuffer {
    /// 追加收到的字节。
    fn push(&mut self, bytes: &[u8]) {
        self.pending.extend_from_slice(bytes);
    }

    /// 取出下一整行（去掉 `\n` 与可选的 `\r`）；没有整行返回 `None`。
    fn next_line(&mut self) -> Option<Vec<u8>> {
        let end = self.pending.iter().position(|b| *b == b'\n')?;
        let mut line: Vec<u8> = self.pending.drain(..=end).collect();
        line.pop();
        if line.last() == Some(&b'\r') {
            line.pop();
        }
        Some(line)
    }

    /// 缓冲中未成行的字节数。
    fn len(&self) -> usize {
        self.pending.len()
    }
}

/// 行管道服务端：后台线程串行接入连接；丢弃即停止并回收线程。
pub struct LinePipeServer {
    /// 停止事件。
    stop: Arc<OwnedHandle>,
    /// 后台线程。
    join: Option<JoinHandle<()>>,
}

impl LinePipeServer {
    /// 创建首个管道实例（同步返回错误）并启动监听线程。
    ///
    /// # 参数
    /// - `name`：管道完整名（见 [`pipe_name_for`]）。
    /// - `factory`：每个新连接调用一次，产出该连接的会话。
    ///
    /// # 返回
    /// 服务端句柄；名字被占用等失败返回原因。
    pub fn start(name: &str, factory: SessionFactory) -> Result<Self, String> {
        let sid = current_user_sid()?;
        let acl = UserOnlyAcl::for_sid(&sid)?;
        let pipe = create_pipe_instance(name, true, &acl)?;
        let io_event = create_event()?;
        let stop = Arc::new(create_event()?);
        let thread_stop = Arc::clone(&stop);
        let join = std::thread::Builder::new()
            .name("line-pipe".into())
            .spawn(move || {
                let _acl = acl;
                serve_lines(&pipe, &io_event, &thread_stop, &factory);
            })
            .map_err(|e| format!("创建管道线程失败: {e}"))?;
        Ok(Self {
            stop,
            join: Some(join),
        })
    }
}

impl Drop for LinePipeServer {
    /// 置位停止事件并等待线程退出（会立刻断开当前连接）。
    fn drop(&mut self) {
        unsafe {
            let _ = SetEvent(self.stop.0);
        }
        if let Some(j) = self.join.take() {
            let _ = j.join();
        }
    }
}

/// 监听主循环：等连接 → 服务到断开 → 再等下一个，直到收到停止信号。
fn serve_lines(
    pipe: &OwnedHandle,
    io_event: &OwnedHandle,
    stop: &OwnedHandle,
    factory: &SessionFactory,
) {
    let mut failures = 0u32;
    loop {
        let connected = unsafe {
            overlapped_io(pipe.0, io_event.0, stop.0, WAIT_FOREVER_MS, |ov| {
                ConnectNamedPipe(pipe.0, Some(ov))
            })
        };
        match connected {
            IoOutcome::Done(_) => {
                failures = 0;
                let mut session = factory();
                serve_connection(pipe.0, io_event.0, stop.0, session.as_mut());
            }
            IoOutcome::Stopped => break,
            other => {
                failures += 1;
                tracing::warn!(reason = %outcome_text(&other), failures, "行管道接入失败");
                if failures >= MAX_CONSECUTIVE_ACCEPT_FAILURES {
                    tracing::error!("行管道连续接入失败，监听线程退出");
                    break;
                }
                std::thread::sleep(ACCEPT_FAILURE_BACKOFF);
            }
        }
        unsafe {
            let _ = DisconnectNamedPipe(pipe.0);
        }
    }
    unsafe {
        let _ = DisconnectNamedPipe(pipe.0);
    }
}

/// 服务一个已连接的客户端：读块 → 切行 → 交会话 → 写回应。
fn serve_connection(pipe: HANDLE, event: HANDLE, stop: HANDLE, session: &mut dyn LineSession) {
    let mut buffer = LineBuffer::default();
    let mut chunk = vec![0u8; READ_CHUNK_BYTES];
    loop {
        let outcome = unsafe {
            overlapped_io(pipe, event, stop, SERVER_IDLE_TIMEOUT_MS, |ov| {
                ReadFile(pipe, Some(&mut chunk), None, Some(ov))
            })
        };
        match outcome {
            IoOutcome::Done(0) | IoOutcome::Closed | IoOutcome::Stopped => return,
            IoOutcome::Done(n) => buffer.push(&chunk[..n as usize]),
            other => {
                tracing::debug!(reason = %outcome_text(&other), "行管道读取结束");
                return;
            }
        }
        while let Some(raw) = buffer.next_line() {
            let reply = match std::str::from_utf8(&raw) {
                Ok(line) => session.on_line(line),
                Err(_) => LineReply {
                    text: None,
                    close: true,
                },
            };
            if let Some(text) = reply.text {
                let mut bytes = text.into_bytes();
                bytes.push(b'\n');
                if let Err(o) = write_all(pipe, event, stop, &bytes, LINE_WRITE_TIMEOUT_MS) {
                    tracing::debug!(reason = %outcome_text(&o), "行管道写回应失败");
                    return;
                }
            }
            if reply.close {
                drain_before_disconnect(pipe, event, stop);
                return;
            }
        }
        if buffer.len() > MAX_LINE_BYTES {
            tracing::warn!("行管道收到超长行，已断开");
            return;
        }
    }
}

/// 等客户端读走最后的回应（否则断开会丢弃未读数据）。
fn drain_before_disconnect(pipe: HANDLE, event: HANDLE, stop: HANDLE) {
    let mut probe = [0u8; 1];
    let _ = unsafe {
        overlapped_io(pipe, event, stop, SERVER_DRAIN_TIMEOUT_MS, |ov| {
            ReadFile(pipe, Some(&mut probe), None, Some(ov))
        })
    };
}

/// 行管道客户端：连接后可发一行、收一行（测试与桥接进程用）。
pub struct LinePipeClient {
    /// 管道句柄。
    pipe: OwnedHandle,
    /// IO 事件。
    event: OwnedHandle,
    /// 永不置位的占位停止事件。
    never: OwnedHandle,
    /// 行缓冲。
    buffer: LineBuffer,
}

impl LinePipeClient {
    /// 连接服务端管道（忙或尚未创建时短间隔重试，整体 2 秒）。
    ///
    /// # 参数
    /// - `name`：管道完整名。
    ///
    /// # 返回
    /// 客户端；超时或失败返回原因。
    pub fn connect(name: &str) -> Result<Self, String> {
        let deadline = Instant::now() + CLIENT_CONNECT_TIMEOUT;
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
                        return Err(format!("连接管道失败: {}", describe(&e)));
                    }
                }
                Err(e) => return Err(format!("连接管道超时: {}", describe(&e))),
            }
        };
        Ok(Self {
            pipe,
            event: create_event()?,
            never: create_event()?,
            buffer: LineBuffer::default(),
        })
    }

    /// 发送一行（自动补换行）。
    ///
    /// # 参数
    /// - `line`：不含换行的文本。
    pub fn send_line(&mut self, line: &str) -> Result<(), String> {
        let mut bytes = line.as_bytes().to_vec();
        bytes.push(b'\n');
        write_all(
            self.pipe.0,
            self.event.0,
            self.never.0,
            &bytes,
            LINE_WRITE_TIMEOUT_MS,
        )
        .map_err(|o| format!("发送失败: {}", outcome_text(&o)))
    }

    /// 读取一行回应。
    ///
    /// # 参数
    /// - `timeout`：等待上限。
    ///
    /// # 返回
    /// 一行文本；超时、对端断开或非 UTF-8 返回原因。
    pub fn read_line(&mut self, timeout: Duration) -> Result<String, String> {
        let deadline = Instant::now() + timeout;
        let mut chunk = vec![0u8; READ_CHUNK_BYTES];
        loop {
            if let Some(raw) = self.buffer.next_line() {
                return String::from_utf8(raw).map_err(|_| "回应不是 UTF-8".to_string());
            }
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Err("等待回应超时".into());
            }
            let outcome = unsafe {
                overlapped_io(
                    self.pipe.0,
                    self.event.0,
                    self.never.0,
                    remaining.as_millis() as u32,
                    |ov| ReadFile(self.pipe.0, Some(&mut chunk), None, Some(ov)),
                )
            };
            match outcome {
                IoOutcome::Done(0) | IoOutcome::Closed => return Err("对端已断开".into()),
                IoOutcome::Done(n) => self.buffer.push(&chunk[..n as usize]),
                other => return Err(format!("读取失败: {}", outcome_text(&other))),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 回声会话：`bye` 回 `bye` 并断开，其它原样回。
    struct Echo;

    impl LineSession for Echo {
        fn on_line(&mut self, line: &str) -> LineReply {
            LineReply {
                text: Some(line.to_string()),
                close: line == "bye",
            }
        }
    }

    /// 生成唯一管道名。
    fn unique_name(tag: &str) -> String {
        format!(
            "{PIPE_NAMESPACE}cisox-line-test-{tag}-{}",
            std::process::id()
        )
    }

    /// 行缓冲：跨块拼接、CRLF、多行一次到。
    #[test]
    fn line_buffer_splits_lines() {
        let mut b = LineBuffer::default();
        b.push(b"ab");
        assert!(b.next_line().is_none());
        b.push(b"c\r\nd\ne");
        assert_eq!(b.next_line().unwrap(), b"abc");
        assert_eq!(b.next_line().unwrap(), b"d");
        assert!(b.next_line().is_none());
        assert_eq!(b.len(), 1);
    }

    /// 真实管道往返，且同名第二个服务端被拒（首实例防抢占）。
    #[test]
    fn echo_roundtrip_and_name_squatting_rejected() {
        let name = unique_name("echo");
        let server = LinePipeServer::start(&name, Box::new(|| Box::new(Echo))).unwrap();
        assert!(LinePipeServer::start(&name, Box::new(|| Box::new(Echo))).is_err());
        let mut client = LinePipeClient::connect(&name).unwrap();
        client.send_line("hello").unwrap();
        assert_eq!(client.read_line(Duration::from_secs(2)).unwrap(), "hello");
        client.send_line("bye").unwrap();
        assert_eq!(client.read_line(Duration::from_secs(2)).unwrap(), "bye");
        drop(client);
        // 断开后可再接入
        let mut again = LinePipeClient::connect(&name).unwrap();
        again.send_line("x").unwrap();
        assert_eq!(again.read_line(Duration::from_secs(2)).unwrap(), "x");
        drop(server);
    }
}
