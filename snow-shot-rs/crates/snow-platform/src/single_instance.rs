//! 单实例检测与跨进程 IPC 通信（Single Instance & IPC）。
//!
//! - 互斥体（`Local\<app_id>`）保证只有一个主实例；
//! - 从属实例通过 **Windows 命名管道** 把命令交给主实例：管道名含用户 SID 与会话号，
//!   DACL 仅授予当前用户，拒绝远程客户端，首个实例带 `FILE_FLAG_FIRST_PIPE_INSTANCE`
//!   防抢占；不再监听任何 TCP 端口。
//! - macOS / Linux 暂未实现 IPC：`start_listener` / `send_command_to_primary` 返回明确错误，
//!   由调用方记录日志并降级（不会静默）。
//!
//! # 线协议
//! 帧 = `u32` 大端长度（1..=[`MAX_IPC_MESSAGE_BYTES`]）+ UTF-8 文本 `CISOX1 <命令>`；
//! 服务端回 1 字节：`0x01` 已接受 / `0x00` 拒绝。非法长度、非 UTF-8、未知命令一律拒绝并忽略。

use std::fmt;
use std::sync::Mutex;

#[cfg(windows)]
mod win_pipe;

/// 按行收发文本的命名管道（复用本模块的当前用户 ACL 与重叠 IO；MCP 等长连接用）。
#[cfg(windows)]
pub use win_pipe::line_pipe;

#[cfg(windows)]
use windows::Win32::Foundation::{CloseHandle, ERROR_ALREADY_EXISTS, GetLastError, HANDLE};
#[cfg(windows)]
use windows::Win32::System::Threading::CreateMutexW;
#[cfg(windows)]
use windows::core::HSTRING;

/// 协议标签（版本 1）；消息文本以它开头。
pub const IPC_PROTOCOL_TAG: &str = "CISOX1";

/// 单条消息（不含 4 字节长度头）的最大字节数。
pub const MAX_IPC_MESSAGE_BYTES: usize = 4096;

/// 长度头字节数。
pub const IPC_LENGTH_HEADER_BYTES: usize = 4;

/// 服务端应答：已接受。
pub const IPC_RESPONSE_OK: u8 = 1;

/// 服务端应答：拒绝（非法消息）。
pub const IPC_RESPONSE_REJECT: u8 = 0;

/// 命令文本：截图。
const CMD_SCREENSHOT: &str = "SCREENSHOT";
/// 命令文本：录制。
const CMD_RECORDING: &str = "RECORDING";
/// 命令文本：长截图。
const CMD_SCROLL_CAPTURE: &str = "SCROLL";
/// 命令文本：从剪贴板贴图。
const CMD_PIN_CLIPBOARD: &str = "PINCLIP";
/// 命令文本：打开设置。
const CMD_SETTINGS: &str = "SETTINGS";
/// 命令文本：唤醒主窗口。
const CMD_SHOW: &str = "SHOW";
/// 命令文本：退出主实例。
const CMD_QUIT: &str = "QUIT";
/// 命令文本前缀：自定义参数。
const CMD_CUSTOM_PREFIX: &str = "CUSTOM:";

/// 跨进程 IPC 控制指令。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IpcCommand {
    /// 触发截图覆盖窗。
    TriggerScreenshot,
    /// 触发屏幕录制。
    TriggerRecording,
    /// 触发长截图（先框选滚动区域）。
    ScrollCapture,
    /// 把剪贴板里的图像贴到屏幕上。
    PinClipboard,
    /// 打开首选项设置窗口。
    OpenSettings,
    /// 唤醒主窗口并置顶。
    ShowMainWindow,
    /// 请求主实例退出（仅同一用户可发送）。
    Quit,
    /// 自定义命令参数字符串（非空、无控制字符）。
    Custom(String),
}

/// IPC 协议错误。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IpcProtocolError {
    /// 长度头为 0 或超过上限。
    BadLength(usize),
    /// 正文不是合法 UTF-8。
    BadUtf8,
    /// 缺少或不匹配协议标签。
    BadTag,
    /// 无法识别的命令。
    UnknownCommand,
    /// 自定义参数为空、含控制字符或使消息超长。
    BadArgument,
}

impl fmt::Display for IpcProtocolError {
    /// 输出可读描述。
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::BadLength(n) => write!(f, "消息长度非法: {n}"),
            Self::BadUtf8 => write!(f, "消息不是合法 UTF-8"),
            Self::BadTag => write!(f, "协议标签不匹配"),
            Self::UnknownCommand => write!(f, "未知命令"),
            Self::BadArgument => write!(f, "命令参数非法"),
        }
    }
}

impl std::error::Error for IpcProtocolError {}

impl IpcCommand {
    /// 命令的传输文本（不含协议标签）。
    ///
    /// ```rust
    /// use snow_platform::single_instance::IpcCommand;
    /// assert_eq!(IpcCommand::OpenSettings.to_payload(), "SETTINGS");
    /// ```
    pub fn to_payload(&self) -> String {
        match self {
            Self::TriggerScreenshot => CMD_SCREENSHOT.to_string(),
            Self::TriggerRecording => CMD_RECORDING.to_string(),
            Self::ScrollCapture => CMD_SCROLL_CAPTURE.to_string(),
            Self::PinClipboard => CMD_PIN_CLIPBOARD.to_string(),
            Self::OpenSettings => CMD_SETTINGS.to_string(),
            Self::ShowMainWindow => CMD_SHOW.to_string(),
            Self::Quit => CMD_QUIT.to_string(),
            Self::Custom(s) => format!("{CMD_CUSTOM_PREFIX}{s}"),
        }
    }

    /// 从传输文本（不含协议标签）解析命令；无法识别返回 `None`。
    ///
    /// ```rust
    /// use snow_platform::single_instance::IpcCommand;
    /// assert_eq!(IpcCommand::from_payload("SHOW"), Some(IpcCommand::ShowMainWindow));
    /// assert_eq!(IpcCommand::from_payload("rm -rf"), None);
    /// ```
    pub fn from_payload(s: &str) -> Option<Self> {
        let trimmed = s.trim();
        match trimmed {
            CMD_SCREENSHOT => Some(Self::TriggerScreenshot),
            CMD_RECORDING => Some(Self::TriggerRecording),
            CMD_SCROLL_CAPTURE => Some(Self::ScrollCapture),
            CMD_PIN_CLIPBOARD => Some(Self::PinClipboard),
            CMD_SETTINGS => Some(Self::OpenSettings),
            CMD_SHOW => Some(Self::ShowMainWindow),
            CMD_QUIT => Some(Self::Quit),
            _ => trimmed
                .strip_prefix(CMD_CUSTOM_PREFIX)
                .filter(|arg| is_valid_custom_argument(arg))
                .map(|arg| Self::Custom(arg.to_string())),
        }
    }

    /// 从命令行参数文本解析（大小写不敏感，如 `screenshot`、`settings`、`quit`）。
    ///
    /// ```rust
    /// use snow_platform::single_instance::IpcCommand;
    /// assert_eq!(IpcCommand::from_cli_name("quit"), Some(IpcCommand::Quit));
    /// ```
    pub fn from_cli_name(name: &str) -> Option<Self> {
        match name.trim().to_ascii_lowercase().as_str() {
            "screenshot" => Some(Self::TriggerScreenshot),
            "recording" => Some(Self::TriggerRecording),
            "scroll-capture" => Some(Self::ScrollCapture),
            "pin-clipboard" => Some(Self::PinClipboard),
            "settings" => Some(Self::OpenSettings),
            "show" => Some(Self::ShowMainWindow),
            "quit" => Some(Self::Quit),
            _ => None,
        }
    }
}

/// 自定义参数合法性：非空且不含控制字符。
fn is_valid_custom_argument(arg: &str) -> bool {
    !arg.is_empty() && !arg.chars().any(char::is_control)
}

/// 把命令编码为完整帧（长度头 + `CISOX1 <命令>`）。
///
/// # 返回
/// 帧字节；自定义参数非法或消息超长返回 [`IpcProtocolError`]。
///
/// ```rust
/// use snow_platform::single_instance::{IpcCommand, encode_frame};
/// let frame = encode_frame(&IpcCommand::ShowMainWindow).unwrap();
/// assert_eq!(&frame[4..], b"CISOX1 SHOW");
/// ```
pub fn encode_frame(command: &IpcCommand) -> Result<Vec<u8>, IpcProtocolError> {
    if let IpcCommand::Custom(arg) = command
        && !is_valid_custom_argument(arg)
    {
        return Err(IpcProtocolError::BadArgument);
    }
    let body = format!("{IPC_PROTOCOL_TAG} {}", command.to_payload());
    if body.len() > MAX_IPC_MESSAGE_BYTES {
        return Err(IpcProtocolError::BadArgument);
    }
    let mut frame = Vec::with_capacity(IPC_LENGTH_HEADER_BYTES + body.len());
    frame.extend_from_slice(&(body.len() as u32).to_be_bytes());
    frame.extend_from_slice(body.as_bytes());
    Ok(frame)
}

/// 解析长度头。
///
/// # 返回
/// 正文长度；为 0 或超过 [`MAX_IPC_MESSAGE_BYTES`] 返回 `BadLength`。
///
/// ```rust
/// use snow_platform::single_instance::parse_frame_length;
/// assert_eq!(parse_frame_length([0, 0, 0, 5]), Ok(5));
/// assert!(parse_frame_length([0, 0, 0, 0]).is_err());
/// ```
pub fn parse_frame_length(header: [u8; IPC_LENGTH_HEADER_BYTES]) -> Result<usize, IpcProtocolError> {
    let len = u32::from_be_bytes(header) as usize;
    if len == 0 || len > MAX_IPC_MESSAGE_BYTES {
        return Err(IpcProtocolError::BadLength(len));
    }
    Ok(len)
}

/// 解码帧正文（不含长度头）。
///
/// ```rust
/// use snow_platform::single_instance::{IpcCommand, decode_body};
/// assert_eq!(decode_body(b"CISOX1 QUIT"), Ok(IpcCommand::Quit));
/// assert!(decode_body(b"HELLO").is_err());
/// ```
pub fn decode_body(body: &[u8]) -> Result<IpcCommand, IpcProtocolError> {
    let text = std::str::from_utf8(body).map_err(|_| IpcProtocolError::BadUtf8)?;
    let rest = text
        .strip_prefix(IPC_PROTOCOL_TAG)
        .and_then(|r| r.strip_prefix(' '))
        .ok_or(IpcProtocolError::BadTag)?;
    IpcCommand::from_payload(rest).ok_or(IpcProtocolError::UnknownCommand)
}

/// 单实例获取判定结果。
pub enum SingleInstanceStatus {
    /// 当前为主实例（已独占互斥体，并持有守卫）。
    Primary(SingleInstanceGuard),
    /// 已有其他实例正在运行（作为从属实例）。
    Secondary,
}

/// 平台监听器句柄（丢弃即停止监听）。
#[cfg(windows)]
type ListenerHandle = win_pipe::PipeListener;
/// 平台监听器句柄（非 Windows 暂无实现，永不构造）。
#[cfg(not(windows))]
type ListenerHandle = ();

/// 主实例所有权守卫。释放时先停止 IPC 监听，再关闭互斥句柄。
pub struct SingleInstanceGuard {
    /// 互斥体标识名（同时用于派生管道名）。
    pub name: String,
    #[cfg(windows)]
    handle: HANDLE,
    /// 已启动的 IPC 监听器。
    listener: Mutex<Option<ListenerHandle>>,
}

// Windows HANDLE 在句柄独占保护下是安全的
unsafe impl Send for SingleInstanceGuard {}
unsafe impl Sync for SingleInstanceGuard {}

impl Drop for SingleInstanceGuard {
    /// 停止监听并释放互斥体。
    fn drop(&mut self) {
        if let Ok(mut slot) = self.listener.lock() {
            slot.take();
        }
        #[cfg(windows)]
        unsafe {
            if !self.handle.is_invalid() {
                let _ = CloseHandle(self.handle);
            }
        }
    }
}

/// 非 Windows 平台 IPC 未实现的统一错误文案。
#[cfg(not(windows))]
const IPC_UNSUPPORTED_MESSAGE: &str = "当前平台尚未实现单实例 IPC（仅 Windows 命名管道已实现）";

/// 单实例探测与锁定器。
pub struct SingleInstanceManager;

impl SingleInstanceManager {
    /// 尝试以指定应用名获取主实例所有权。
    ///
    /// # 参数
    /// - `app_id`：全局唯一标识符（例如 `"cisox.snow_shot.single_instance"`），
    ///   仅允许字母、数字、`.`、`_`、`-`（同时用于管道名）。
    ///
    /// # 返回
    /// 主实例守卫或从属实例标记；互斥体创建失败按从属处理。
    ///
    /// ```no_run
    /// use snow_platform::single_instance::{SingleInstanceManager, SingleInstanceStatus};
    /// match SingleInstanceManager::acquire("cisox.demo") {
    ///     SingleInstanceStatus::Primary(_guard) => println!("primary"),
    ///     SingleInstanceStatus::Secondary => println!("secondary"),
    /// }
    /// ```
    pub fn acquire(app_id: &str) -> SingleInstanceStatus {
        #[cfg(windows)]
        {
            let wide_name = HSTRING::from(format!("Local\\{app_id}"));
            unsafe {
                let handle = match CreateMutexW(None, true, &wide_name) {
                    Ok(h) => h,
                    Err(_) => return SingleInstanceStatus::Secondary,
                };

                if GetLastError() == ERROR_ALREADY_EXISTS {
                    let _ = CloseHandle(handle);
                    SingleInstanceStatus::Secondary
                } else {
                    SingleInstanceStatus::Primary(SingleInstanceGuard {
                        name: app_id.to_string(),
                        handle,
                        listener: Mutex::new(None),
                    })
                }
            }
        }

        #[cfg(not(windows))]
        {
            // 非 Windows 平台暂无互斥实现：恒为主实例
            SingleInstanceStatus::Primary(SingleInstanceGuard {
                name: app_id.to_string(),
                listener: Mutex::new(None),
            })
        }
    }

    /// 作为从属实例向主实例发送控制命令（1.5 秒内忙则重试，读写均有超时）。
    ///
    /// # 参数
    /// - `app_id`：与主实例 `acquire` 相同的标识符。
    /// - `command`：要投递的命令。
    ///
    /// # 返回
    /// 主实例确认接受返回 `Ok(())`；连接失败、超时或被拒绝返回错误描述。
    ///
    /// ```no_run
    /// use snow_platform::single_instance::{IpcCommand, SingleInstanceManager};
    /// let _ = SingleInstanceManager::send_command_to_primary("cisox.demo", &IpcCommand::ShowMainWindow);
    /// ```
    pub fn send_command_to_primary(app_id: &str, command: &IpcCommand) -> Result<(), String> {
        let frame = encode_frame(command).map_err(|e| format!("命令编码失败: {e}"))?;
        #[cfg(windows)]
        {
            let name = win_pipe::pipe_name(app_id)?;
            win_pipe::send_frame(&name, &frame)
        }
        #[cfg(not(windows))]
        {
            let _ = (app_id, frame);
            Err(IPC_UNSUPPORTED_MESSAGE.to_string())
        }
    }

    /// 在主实例后台线程启动 IPC 监听（每个守卫只能启动一次）。
    ///
    /// # 参数
    /// - `guard`：当前持有的主实例守卫，监听随守卫释放而停止。
    /// - `on_command`：收到合法命令时在监听线程上调用；应只做轻量转发（如投递到主线程收件箱）。
    ///
    /// # 返回
    /// 成功返回 `Ok(())`；管道名被他人占用、ACL 创建失败或平台不支持返回错误
    /// （不会静默降级到其它通道，调用方应记录日志）。
    ///
    /// ```no_run
    /// use snow_platform::single_instance::{SingleInstanceManager, SingleInstanceStatus};
    /// if let SingleInstanceStatus::Primary(guard) = SingleInstanceManager::acquire("cisox.demo") {
    ///     SingleInstanceManager::start_listener(&guard, |cmd| println!("{cmd:?}")).unwrap();
    /// }
    /// ```
    pub fn start_listener<F>(guard: &SingleInstanceGuard, on_command: F) -> Result<(), String>
    where
        F: Fn(IpcCommand) + Send + 'static,
    {
        #[cfg(windows)]
        {
            let mut slot = guard
                .listener
                .lock()
                .map_err(|_| "监听器状态锁已损坏".to_string())?;
            if slot.is_some() {
                return Err("IPC 监听器已启动".to_string());
            }
            let name = win_pipe::pipe_name(&guard.name)?;
            match win_pipe::PipeListener::start(&name, Box::new(on_command)) {
                Ok(listener) => {
                    tracing::info!(pipe = %name, "单实例 IPC 命名管道已就绪");
                    *slot = Some(listener);
                    Ok(())
                }
                Err(e) => {
                    tracing::error!(pipe = %name, error = %e, "创建单实例 IPC 命名管道失败");
                    Err(e)
                }
            }
        }
        #[cfg(not(windows))]
        {
            let _ = (guard, on_command);
            tracing::warn!("{IPC_UNSUPPORTED_MESSAGE}");
            Err(IPC_UNSUPPORTED_MESSAGE.to_string())
        }
    }

    /// 主实例管道的完整名称（诊断与自验证使用）。
    ///
    /// # 返回
    /// Windows 上形如 `\\.\pipe\<app_id>-<SID>-<会话号>`；其它平台返回错误。
    ///
    /// ```no_run
    /// let name = snow_platform::single_instance::SingleInstanceManager::pipe_name("cisox.demo");
    /// println!("{name:?}");
    /// ```
    pub fn pipe_name(app_id: &str) -> Result<String, String> {
        #[cfg(windows)]
        {
            win_pipe::pipe_name(app_id)
        }
        #[cfg(not(windows))]
        {
            let _ = app_id;
            Err(IPC_UNSUPPORTED_MESSAGE.to_string())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::sync::mpsc;
    use std::time::Duration;

    /// 为每个测试生成互不冲突的应用标识。
    fn unique_app_id(tag: &str) -> String {
        format!("cisox.test.{tag}.{}", std::process::id())
    }

    /// 验证 IPC 命令文本编解码对称性。
    #[test]
    fn ipc_command_serialization() {
        let commands = [
            IpcCommand::TriggerScreenshot,
            IpcCommand::TriggerRecording,
            IpcCommand::ScrollCapture,
            IpcCommand::PinClipboard,
            IpcCommand::OpenSettings,
            IpcCommand::ShowMainWindow,
            IpcCommand::Quit,
            IpcCommand::Custom("arg_value".into()),
        ];
        for cmd in &commands {
            let parsed = IpcCommand::from_payload(&cmd.to_payload());
            assert_eq!(parsed.as_ref(), Some(cmd));
        }
    }

    /// 帧编码后能按长度头 + 正文还原。
    #[test]
    fn frame_roundtrip() {
        for cmd in [
            IpcCommand::TriggerScreenshot,
            IpcCommand::Quit,
            IpcCommand::Custom("a b c".into()),
        ] {
            let frame = encode_frame(&cmd).unwrap();
            let header: [u8; 4] = frame[..4].try_into().unwrap();
            let len = parse_frame_length(header).unwrap();
            assert_eq!(len, frame.len() - 4);
            assert_eq!(decode_body(&frame[4..]), Ok(cmd));
        }
    }

    /// 非法长度：0 与超限都被拒绝。
    #[test]
    fn rejects_bad_length() {
        assert_eq!(parse_frame_length([0, 0, 0, 0]), Err(IpcProtocolError::BadLength(0)));
        let too_big = ((MAX_IPC_MESSAGE_BYTES + 1) as u32).to_be_bytes();
        assert!(parse_frame_length(too_big).is_err());
        assert!(parse_frame_length((MAX_IPC_MESSAGE_BYTES as u32).to_be_bytes()).is_ok());
        assert!(parse_frame_length(u32::MAX.to_be_bytes()).is_err());
    }

    /// 非法正文：非 UTF-8、错标签、未知命令、非法自定义参数。
    #[test]
    fn rejects_bad_body() {
        assert_eq!(decode_body(&[0xff, 0xfe]), Err(IpcProtocolError::BadUtf8));
        assert_eq!(decode_body(b"CISOX2 SHOW"), Err(IpcProtocolError::BadTag));
        assert_eq!(decode_body(b"CISOX1SHOW"), Err(IpcProtocolError::BadTag));
        assert_eq!(decode_body(b"CISOX1 FORMAT_C"), Err(IpcProtocolError::UnknownCommand));
        assert_eq!(decode_body(b"CISOX1 CUSTOM:"), Err(IpcProtocolError::UnknownCommand));
        assert_eq!(decode_body(b"CISOX1 CUSTOM:a\x07b"), Err(IpcProtocolError::UnknownCommand));
        assert!(encode_frame(&IpcCommand::Custom(String::new())).is_err());
        assert!(encode_frame(&IpcCommand::Custom("x".repeat(MAX_IPC_MESSAGE_BYTES))).is_err());
    }

    /// 命令行名称解析。
    #[test]
    fn cli_names() {
        assert_eq!(IpcCommand::from_cli_name("Screenshot"), Some(IpcCommand::TriggerScreenshot));
        assert_eq!(IpcCommand::from_cli_name(" quit "), Some(IpcCommand::Quit));
        assert_eq!(IpcCommand::from_cli_name("Pin-Clipboard"), Some(IpcCommand::PinClipboard));
        assert_eq!(IpcCommand::from_payload("PINCLIP"), Some(IpcCommand::PinClipboard));
        assert_eq!(IpcCommand::from_cli_name("nope"), None);
    }

    /// 验证单实例互斥判定（主实例占有后，从属实例应判定冲突）。
    #[test]
    fn single_instance_acquisition() {
        let app_id = unique_app_id("lock");
        let SingleInstanceStatus::Primary(guard) = SingleInstanceManager::acquire(&app_id) else {
            panic!("首次获取应为主实例");
        };
        assert_eq!(guard.name, app_id);
        #[cfg(windows)]
        assert!(matches!(
            SingleInstanceManager::acquire(&app_id),
            SingleInstanceStatus::Secondary
        ));
        drop(guard);
    }

    /// 主从命名管道往返：多条命令全部到达且顺序一致。
    #[cfg(windows)]
    #[test]
    fn pipe_roundtrip_delivers_commands_in_order() {
        let app_id = unique_app_id("roundtrip");
        let SingleInstanceStatus::Primary(guard) = SingleInstanceManager::acquire(&app_id) else {
            panic!("应为主实例");
        };
        let (tx, rx) = mpsc::channel();
        let tx = Mutex::new(tx);
        SingleInstanceManager::start_listener(&guard, move |cmd| {
            let _ = tx.lock().unwrap().send(cmd);
        })
        .unwrap();

        let sent = [
            IpcCommand::TriggerScreenshot,
            IpcCommand::TriggerRecording,
            IpcCommand::ScrollCapture,
            IpcCommand::OpenSettings,
            IpcCommand::ShowMainWindow,
            IpcCommand::Custom("hello world".into()),
        ];
        for cmd in &sent {
            SingleInstanceManager::send_command_to_primary(&app_id, cmd).unwrap();
        }
        for cmd in &sent {
            let got = rx.recv_timeout(Duration::from_secs(3)).unwrap();
            assert_eq!(&got, cmd);
        }
    }

    /// 并发多个客户端同时发送：服务端串行处理，全部送达。
    #[cfg(windows)]
    #[test]
    fn pipe_handles_concurrent_clients() {
        let app_id = unique_app_id("concurrent");
        let SingleInstanceStatus::Primary(guard) = SingleInstanceManager::acquire(&app_id) else {
            panic!("应为主实例");
        };
        let (tx, rx) = mpsc::channel();
        let tx = Mutex::new(tx);
        SingleInstanceManager::start_listener(&guard, move |cmd| {
            let _ = tx.lock().unwrap().send(cmd);
        })
        .unwrap();
        let app_id = Arc::new(app_id);
        let handles: Vec<_> = (0..6)
            .map(|i| {
                let id = Arc::clone(&app_id);
                std::thread::spawn(move || {
                    SingleInstanceManager::send_command_to_primary(
                        &id,
                        &IpcCommand::Custom(format!("c{i}")),
                    )
                })
            })
            .collect();
        for h in handles {
            h.join().unwrap().unwrap();
        }
        let mut got: Vec<String> = (0..6)
            .map(|_| match rx.recv_timeout(Duration::from_secs(3)).unwrap() {
                IpcCommand::Custom(s) => s,
                other => panic!("意外命令 {other:?}"),
            })
            .collect();
        got.sort();
        assert_eq!(got, ["c0", "c1", "c2", "c3", "c4", "c5"]);
    }

    /// 没有主实例监听时，客户端在限时内返回错误而不是卡死。
    #[cfg(windows)]
    #[test]
    fn send_without_listener_fails_fast() {
        let start = std::time::Instant::now();
        let res = SingleInstanceManager::send_command_to_primary(
            &unique_app_id("nobody"),
            &IpcCommand::ShowMainWindow,
        );
        assert!(res.is_err());
        assert!(start.elapsed() < Duration::from_secs(4));
    }

    /// 同一守卫重复启动监听被拒绝。
    #[cfg(windows)]
    #[test]
    fn listener_cannot_start_twice() {
        let app_id = unique_app_id("twice");
        let SingleInstanceStatus::Primary(guard) = SingleInstanceManager::acquire(&app_id) else {
            panic!("应为主实例");
        };
        SingleInstanceManager::start_listener(&guard, |_| {}).unwrap();
        assert!(SingleInstanceManager::start_listener(&guard, |_| {}).is_err());
    }

    /// 守卫释放后监听停止，管道不再可连。
    #[cfg(windows)]
    #[test]
    fn dropping_guard_stops_listener() {
        let app_id = unique_app_id("stop");
        let SingleInstanceStatus::Primary(guard) = SingleInstanceManager::acquire(&app_id) else {
            panic!("应为主实例");
        };
        SingleInstanceManager::start_listener(&guard, |_| {}).unwrap();
        SingleInstanceManager::send_command_to_primary(&app_id, &IpcCommand::ShowMainWindow)
            .unwrap();
        drop(guard);
        assert!(
            SingleInstanceManager::send_command_to_primary(&app_id, &IpcCommand::ShowMainWindow)
                .is_err()
        );
    }

    /// 非 Windows：IPC 明确报告不支持。
    #[cfg(not(windows))]
    #[test]
    fn unsupported_platform_reports_error() {
        assert!(
            SingleInstanceManager::send_command_to_primary("x", &IpcCommand::ShowMainWindow)
                .is_err()
        );
    }
}
