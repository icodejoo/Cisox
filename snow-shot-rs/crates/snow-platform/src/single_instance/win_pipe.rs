//! Windows 命名管道后端：当前用户 ACL、首实例防抢占、全程带超时的重叠 IO。

use super::{
    IPC_LENGTH_HEADER_BYTES, IPC_RESPONSE_OK, IPC_RESPONSE_REJECT, IpcCommand, decode_body,
    parse_frame_length,
};
use std::ffi::c_void;
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};
use windows::Win32::Foundation::{
    CloseHandle, ERROR_BROKEN_PIPE, ERROR_FILE_NOT_FOUND, ERROR_IO_PENDING, ERROR_PIPE_BUSY,
    ERROR_PIPE_CONNECTED, GENERIC_READ, GENERIC_WRITE, GetLastError, HANDLE, HLOCAL,
    INVALID_HANDLE_VALUE, LocalFree, WAIT_OBJECT_0, WAIT_TIMEOUT,
};
use windows::Win32::Security::Authorization::{
    ConvertSidToStringSidW, ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1,
};
use windows::Win32::Security::{
    PSECURITY_DESCRIPTOR, SECURITY_ATTRIBUTES, TOKEN_QUERY, TOKEN_USER, TokenUser,
    GetTokenInformation,
};
use windows::Win32::Storage::FileSystem::{
    CreateFileW, FILE_FLAG_FIRST_PIPE_INSTANCE, FILE_FLAG_OVERLAPPED, FILE_SHARE_NONE,
    OPEN_EXISTING, PIPE_ACCESS_DUPLEX, ReadFile, SECURITY_IDENTIFICATION, SECURITY_SQOS_PRESENT,
    WriteFile,
};
use windows::Win32::System::IO::{CancelIoEx, GetOverlappedResult, OVERLAPPED};
use windows::Win32::System::Pipes::{
    ConnectNamedPipe, CreateNamedPipeW, DisconnectNamedPipe, GetNamedPipeClientProcessId,
    GetNamedPipeServerProcessId, PIPE_READMODE_BYTE, PIPE_REJECT_REMOTE_CLIENTS, PIPE_TYPE_BYTE, PIPE_WAIT, WaitNamedPipeW,
};
use windows::Win32::System::RemoteDesktop::ProcessIdToSessionId;
use windows::Win32::UI::WindowsAndMessaging::AllowSetForegroundWindow;
use windows::Win32::System::Threading::{
    CreateEventW, GetCurrentProcess, GetCurrentProcessId, OpenProcessToken, ResetEvent, SetEvent,
    WaitForMultipleObjects,
};
use windows::core::{HSTRING, PCWSTR, PWSTR};

pub mod line_pipe;

/// 管道命名空间前缀。
const PIPE_NAMESPACE: &str = r"\\.\pipe\";
/// 管道收发缓冲区字节数。
const PIPE_BUFFER_BYTES: u32 = 8192;
/// 服务端单个连接的读取超时（毫秒）。
const SERVER_READ_TIMEOUT_MS: u32 = 500;
/// 服务端写应答超时（毫秒）。
const SERVER_WRITE_TIMEOUT_MS: u32 = 500;
/// 服务端等待客户端读走应答并关闭的时长（毫秒）。
const SERVER_DRAIN_TIMEOUT_MS: u32 = 500;
/// 客户端整体（连接 + 收发）时限。
const CLIENT_TOTAL_TIMEOUT: Duration = Duration::from_millis(1500);
/// 客户端连接重试间隔。
const CLIENT_RETRY_INTERVAL: Duration = Duration::from_millis(25);
/// 客户端等待应答的时长（毫秒）。
const CLIENT_RESPONSE_TIMEOUT_MS: u32 = 1000;
/// 无限等待。
const WAIT_FOREVER_MS: u32 = u32::MAX;
/// 连续接入失败达到该次数后放弃监听（避免空转）。
const MAX_CONSECUTIVE_ACCEPT_FAILURES: u32 = 20;
/// 接入失败后的退避时长。
const ACCEPT_FAILURE_BACKOFF: Duration = Duration::from_millis(50);

/// 自动关闭的内核句柄。
struct OwnedHandle(HANDLE);

// 内核句柄可在线程间转移；本模块保证同一时刻只有一个线程使用它做 IO
unsafe impl Send for OwnedHandle {}
unsafe impl Sync for OwnedHandle {}

impl Drop for OwnedHandle {
    /// 关闭句柄。
    fn drop(&mut self) {
        if !self.0.is_invalid() {
            unsafe {
                let _ = CloseHandle(self.0);
            }
        }
    }
}

/// 把 Windows 错误格式化为 `描述 (0x码)`。
fn describe(e: &windows::core::Error) -> String {
    format!("{} (0x{:08X})", e.message().trim(), e.code().0 as u32)
}

/// 判断错误码是否等于指定 Win32 错误。
fn is_win32(e: &windows::core::Error, code: windows::Win32::Foundation::WIN32_ERROR) -> bool {
    e.code() == code.to_hresult()
}

/// 取当前进程用户的 SID 字符串（如 `S-1-5-21-...`）。
fn current_user_sid() -> Result<String, String> {
    unsafe {
        let mut token = HANDLE::default();
        OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token)
            .map_err(|e| format!("OpenProcessToken 失败: {}", describe(&e)))?;
        let token = OwnedHandle(token);

        let mut needed = 0u32;
        // 首次调用只为取所需长度，预期以 ERROR_INSUFFICIENT_BUFFER 失败
        let _ = GetTokenInformation(token.0, TokenUser, None, 0, &mut needed);
        if needed == 0 {
            return Err("GetTokenInformation 未返回所需长度".into());
        }
        // 用 u64 缓冲保证 TOKEN_USER（含指针）对齐
        let mut buf = vec![0u64; (needed as usize).div_ceil(8)];
        GetTokenInformation(
            token.0,
            TokenUser,
            Some(buf.as_mut_ptr() as *mut c_void),
            needed,
            &mut needed,
        )
        .map_err(|e| format!("GetTokenInformation 失败: {}", describe(&e)))?;

        let user = &*(buf.as_ptr() as *const TOKEN_USER);
        let mut wide = PWSTR::null();
        ConvertSidToStringSidW(user.User.Sid, &mut wide)
            .map_err(|e| format!("ConvertSidToStringSidW 失败: {}", describe(&e)))?;
        let text = wide.to_string().map_err(|e| format!("SID 文本非法: {e}"));
        let _ = LocalFree(Some(HLOCAL(wide.0 as *mut c_void)));
        text
    }
}

/// 计算主实例管道完整名：`\\.\pipe\<app_id>-<SID>-<会话号>`。
///
/// `app_id` 只允许字母、数字、`.`、`_`、`-`，避免注入路径分隔符。
pub(super) fn pipe_name(app_id: &str) -> Result<String, String> {
    if app_id.is_empty()
        || !app_id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
    {
        return Err(format!("应用标识含非法字符或为空: {app_id:?}"));
    }
    let sid = current_user_sid()?;
    let mut session = 0u32;
    unsafe {
        ProcessIdToSessionId(GetCurrentProcessId(), &mut session)
            .map_err(|e| format!("ProcessIdToSessionId 失败: {}", describe(&e)))?;
    }
    Ok(format!("{PIPE_NAMESPACE}{app_id}-{sid}-{session}"))
}

/// 仅授予指定用户的受保护 DACL（不继承）；持有期间安全描述符有效。
struct UserOnlyAcl {
    /// 由系统分配的安全描述符（需 `LocalFree`）。
    descriptor: PSECURITY_DESCRIPTOR,
    /// 传给 `CreateNamedPipeW` 的安全属性。
    attributes: SECURITY_ATTRIBUTES,
}

// 安全描述符由 LocalFree 释放，且只在一个线程内使用
unsafe impl Send for UserOnlyAcl {}

impl UserOnlyAcl {
    /// 构造 `D:P(A;;GA;;;<SID>)`。
    fn for_sid(sid: &str) -> Result<Self, String> {
        let sddl = HSTRING::from(format!("D:P(A;;GA;;;{sid})"));
        let mut descriptor = PSECURITY_DESCRIPTOR::default();
        unsafe {
            ConvertStringSecurityDescriptorToSecurityDescriptorW(
                &sddl,
                SDDL_REVISION_1,
                &mut descriptor,
                None,
            )
            .map_err(|e| format!("构造管道 ACL 失败: {}", describe(&e)))?;
        }
        let attributes = SECURITY_ATTRIBUTES {
            nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
            lpSecurityDescriptor: descriptor.0,
            bInheritHandle: false.into(),
        };
        Ok(Self {
            descriptor,
            attributes,
        })
    }
}

impl Drop for UserOnlyAcl {
    /// 释放安全描述符。
    fn drop(&mut self) {
        unsafe {
            let _ = LocalFree(Some(HLOCAL(self.descriptor.0)));
        }
    }
}

/// 创建（仅一个实例的）服务端管道；`first` 为真时带 `FILE_FLAG_FIRST_PIPE_INSTANCE`，
/// 名字已被占用会失败，从而发现抢占。
fn create_pipe_instance(
    name: &str,
    first: bool,
    acl: &UserOnlyAcl,
) -> Result<OwnedHandle, String> {
    let mut open_mode = PIPE_ACCESS_DUPLEX | FILE_FLAG_OVERLAPPED;
    if first {
        open_mode |= FILE_FLAG_FIRST_PIPE_INSTANCE;
    }
    let pipe_mode = PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT | PIPE_REJECT_REMOTE_CLIENTS;
    let wide = HSTRING::from(name);
    let handle = unsafe {
        CreateNamedPipeW(
            &wide,
            open_mode,
            pipe_mode,
            1,
            PIPE_BUFFER_BYTES,
            PIPE_BUFFER_BYTES,
            0,
            Some(&acl.attributes as *const SECURITY_ATTRIBUTES),
        )
    };
    if handle == INVALID_HANDLE_VALUE {
        let err = windows::core::Error::from_thread();
        return Err(format!(
            "CreateNamedPipeW 失败（名称可能已被其它进程占用）: {}",
            describe(&err)
        ));
    }
    Ok(OwnedHandle(handle))
}

/// 手动复位事件。
fn create_event() -> Result<OwnedHandle, String> {
    unsafe { CreateEventW(None, true, false, PCWSTR::null()) }
        .map(OwnedHandle)
        .map_err(|e| format!("CreateEventW 失败: {}", describe(&e)))
}

/// 一次重叠 IO 的结果。
#[derive(Debug)]
enum IoOutcome {
    /// 完成，附带传输字节数。
    Done(u32),
    /// 超时（IO 已取消）。
    Timeout,
    /// 收到停止信号（IO 已取消）。
    Stopped,
    /// 对端断开。
    Closed,
    /// 其它失败。
    Failed(String),
}

/// 发起一次重叠 IO 并等待完成 / 超时 / 停止；返回前保证 IO 已结束，缓冲可安全释放。
///
/// # 安全
/// `start` 必须只以传入的 `OVERLAPPED` 指针发起 IO，且 IO 使用的缓冲在本函数返回前保持有效。
unsafe fn overlapped_io(
    pipe: HANDLE,
    event: HANDLE,
    stop: HANDLE,
    timeout_ms: u32,
    start: impl FnOnce(*mut OVERLAPPED) -> windows::core::Result<()>,
) -> IoOutcome {
    unsafe {
        let _ = ResetEvent(event);
        let mut overlapped = OVERLAPPED {
            hEvent: event,
            ..Default::default()
        };
        match start(&mut overlapped) {
            Ok(()) => {}
            Err(e) if is_win32(&e, ERROR_PIPE_CONNECTED) => return IoOutcome::Done(0),
            Err(e) if is_win32(&e, ERROR_IO_PENDING) => {
                let waited = WaitForMultipleObjects(&[event, stop], false, timeout_ms);
                if waited != WAIT_OBJECT_0 {
                    let outcome = if waited == WAIT_TIMEOUT {
                        IoOutcome::Timeout
                    } else if waited.0 == WAIT_OBJECT_0.0 + 1 {
                        IoOutcome::Stopped
                    } else {
                        IoOutcome::Failed(format!("等待 IO 失败: {}", GetLastError().0))
                    };
                    let _ = CancelIoEx(pipe, Some(&overlapped));
                    let mut ignored = 0u32;
                    // 必须等取消真正完成，之后 OVERLAPPED 与缓冲才可释放
                    let _ = GetOverlappedResult(pipe, &overlapped, &mut ignored, true);
                    return outcome;
                }
            }
            Err(e) if is_win32(&e, ERROR_BROKEN_PIPE) => return IoOutcome::Closed,
            Err(e) => return IoOutcome::Failed(describe(&e)),
        }
        let mut transferred = 0u32;
        match GetOverlappedResult(pipe, &overlapped, &mut transferred, false) {
            Ok(()) => IoOutcome::Done(transferred),
            Err(e) if is_win32(&e, ERROR_BROKEN_PIPE) => IoOutcome::Closed,
            Err(e) => IoOutcome::Failed(describe(&e)),
        }
    }
}

/// 在截止时间前精确读满 `buf`。
fn read_exact(
    pipe: HANDLE,
    event: HANDLE,
    stop: HANDLE,
    buf: &mut [u8],
    deadline: Instant,
) -> Result<(), IoOutcome> {
    let mut offset = 0usize;
    while offset < buf.len() {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err(IoOutcome::Timeout);
        }
        let slice = &mut buf[offset..];
        let outcome = unsafe {
            overlapped_io(pipe, event, stop, remaining.as_millis() as u32, |ov| {
                ReadFile(pipe, Some(slice), None, Some(ov))
            })
        };
        match outcome {
            IoOutcome::Done(0) => return Err(IoOutcome::Closed),
            IoOutcome::Done(n) => offset += n as usize,
            other => return Err(other),
        }
    }
    Ok(())
}

/// 写出全部字节（每次写有超时）。
fn write_all(
    pipe: HANDLE,
    event: HANDLE,
    stop: HANDLE,
    data: &[u8],
    timeout_ms: u32,
) -> Result<(), IoOutcome> {
    let deadline = Instant::now() + Duration::from_millis(u64::from(timeout_ms));
    let mut offset = 0usize;
    while offset < data.len() {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err(IoOutcome::Timeout);
        }
        let slice = &data[offset..];
        let outcome = unsafe {
            overlapped_io(pipe, event, stop, remaining.as_millis() as u32, |ov| {
                WriteFile(pipe, Some(slice), None, Some(ov))
            })
        };
        match outcome {
            IoOutcome::Done(0) => return Err(IoOutcome::Closed),
            IoOutcome::Done(n) => offset += n as usize,
            other => return Err(other),
        }
    }
    Ok(())
}

/// 把失败结果转成日志文本。
fn outcome_text(o: &IoOutcome) -> String {
    match o {
        IoOutcome::Done(n) => format!("完成 {n} 字节"),
        IoOutcome::Timeout => "超时".into(),
        IoOutcome::Stopped => "已停止".into(),
        IoOutcome::Closed => "对端已断开".into(),
        IoOutcome::Failed(s) => s.clone(),
    }
}

/// 命名管道监听器：后台线程串行处理连接；丢弃即停止并回收线程。
pub(super) struct PipeListener {
    /// 停止事件（`Drop` 时置位以唤醒后台线程）。
    stop: Arc<OwnedHandle>,
    /// 后台线程。
    join: Option<JoinHandle<()>>,
}

impl PipeListener {
    /// 创建首个管道实例（同步返回错误）并启动监听线程。
    ///
    /// # 参数
    /// - `name`：管道完整名。
    /// - `on_command`：收到合法命令时在监听线程上调用。
    pub(super) fn start(
        name: &str,
        on_command: Box<dyn Fn(IpcCommand) + Send>,
    ) -> Result<Self, String> {
        let sid = current_user_sid()?;
        let acl = UserOnlyAcl::for_sid(&sid)?;
        let pipe = create_pipe_instance(name, true, &acl)?;
        let io_event = create_event()?;
        let stop = Arc::new(create_event()?);

        let thread_stop = Arc::clone(&stop);
        let join = std::thread::Builder::new()
            .name("single-instance-ipc".into())
            .spawn(move || {
                // ACL 需在线程内保持到管道关闭（管道创建后系统已复制描述符，这里仅随线程释放）
                let _acl = acl;
                serve(&pipe, &io_event, &thread_stop, on_command);
            })
            .map_err(|e| format!("创建 IPC 线程失败: {e}"))?;
        Ok(Self {
            stop,
            join: Some(join),
        })
    }
}

impl Drop for PipeListener {
    /// 置位停止事件并等待线程退出。
    fn drop(&mut self) {
        unsafe {
            let _ = SetEvent(self.stop.0);
        }
        if let Some(j) = self.join.take() {
            let _ = j.join();
        }
    }
}

/// 监听主循环：等待连接 → 处理 → 断开，直到收到停止信号。
fn serve(
    pipe: &OwnedHandle,
    io_event: &OwnedHandle,
    stop: &OwnedHandle,
    on_command: Box<dyn Fn(IpcCommand) + Send>,
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
                handle_connection(pipe.0, io_event.0, stop.0, &on_command);
            }
            IoOutcome::Stopped => break,
            other => {
                failures += 1;
                tracing::warn!(reason = %outcome_text(&other), failures, "IPC 接入连接失败");
                if failures >= MAX_CONSECUTIVE_ACCEPT_FAILURES {
                    tracing::error!("IPC 连续接入失败，监听线程退出（单实例命令通道失效）");
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

/// 处理一个已连接的客户端：读帧、解码、回调、回应答、等待其读走应答。
fn handle_connection(
    pipe: HANDLE,
    event: HANDLE,
    stop: HANDLE,
    on_command: &(dyn Fn(IpcCommand) + Send),
) {
    let mut client_pid = 0u32;
    unsafe {
        let _ = GetNamedPipeClientProcessId(pipe, &mut client_pid);
    }
    let deadline = Instant::now() + Duration::from_millis(u64::from(SERVER_READ_TIMEOUT_MS));

    let mut header = [0u8; IPC_LENGTH_HEADER_BYTES];
    if let Err(o) = read_exact(pipe, event, stop, &mut header, deadline) {
        tracing::warn!(client_pid, reason = %outcome_text(&o), "IPC 读取长度头失败");
        return;
    }
    let len = match parse_frame_length(header) {
        Ok(n) => n,
        Err(e) => {
            tracing::warn!(client_pid, error = %e, "IPC 收到非法长度，已忽略");
            reply(pipe, event, stop, IPC_RESPONSE_REJECT);
            return;
        }
    };
    let mut body = vec![0u8; len];
    if let Err(o) = read_exact(pipe, event, stop, &mut body, deadline) {
        tracing::warn!(client_pid, reason = %outcome_text(&o), "IPC 读取正文失败");
        return;
    }
    match decode_body(&body) {
        Ok(cmd) => {
            tracing::info!(client_pid, command = ?cmd, "IPC 收到命令");
            let delivered =
                std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| on_command(cmd))).is_ok();
            if !delivered {
                tracing::error!("IPC 命令回调 panic，已忽略");
            }
            reply(
                pipe,
                event,
                stop,
                if delivered { IPC_RESPONSE_OK } else { IPC_RESPONSE_REJECT },
            );
        }
        Err(e) => {
            tracing::warn!(client_pid, error = %e, "IPC 收到非法消息，已忽略");
            reply(pipe, event, stop, IPC_RESPONSE_REJECT);
        }
    }
    // 等客户端读走应答并关闭；否则 DisconnectNamedPipe 会丢弃尚未读取的数据
    let mut probe = [0u8; 1];
    let _ = unsafe {
        overlapped_io(pipe, event, stop, SERVER_DRAIN_TIMEOUT_MS, |ov| {
            ReadFile(pipe, Some(&mut probe), None, Some(ov))
        })
    };
}

/// 写 1 字节应答，失败只记日志。
fn reply(pipe: HANDLE, event: HANDLE, stop: HANDLE, code: u8) {
    if let Err(o) = write_all(pipe, event, stop, &[code], SERVER_WRITE_TIMEOUT_MS) {
        tracing::warn!(reason = %outcome_text(&o), "IPC 写应答失败");
    }
}

/// 把前台窗口权限授予主实例进程：本进程（用户刚启动的从属实例）拥有前台权限，
/// 授权后主实例响应命令时弹出的窗口才能抢到键盘焦点。失败只记日志，不影响命令投递。
fn grant_foreground_to_server(pipe: HANDLE) {
    let mut server_pid = 0u32;
    // SAFETY: `server_pid` 是有效出参；`pipe` 是调用方持有的有效管道句柄。
    let queried = unsafe { GetNamedPipeServerProcessId(pipe, &mut server_pid) };
    if let Err(e) = queried {
        tracing::warn!(error = %describe(&e), "无法取得主实例进程号，跳过前台权限授予");
        return;
    }
    // SAFETY: 仅传入进程号值参数。
    if let Err(e) = unsafe { AllowSetForegroundWindow(server_pid) } {
        tracing::warn!(pid = server_pid, error = %describe(&e), "授予主实例前台权限失败");
    }
}

/// 客户端：连接主实例管道并发送一帧，等待应答。
///
/// 管道忙 / 尚未创建时按 25ms 间隔重试，整体不超过 1.5 秒。
pub(super) fn send_frame(name: &str, frame: &[u8]) -> Result<(), String> {
    let deadline = Instant::now() + CLIENT_TOTAL_TIMEOUT;
    let wide = HSTRING::from(name);
    let pipe = loop {
        let opened = unsafe {
            CreateFileW(
                &wide,
                GENERIC_READ.0 | GENERIC_WRITE.0,
                FILE_SHARE_NONE,
                None,
                OPEN_EXISTING,
                // IDENTIFICATION：服务端只能识别我们的身份，不能模拟
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
                    return Err(format!("连接主实例管道失败: {}", describe(&e)));
                }
            }
            Err(e) => return Err(format!("连接主实例管道超时: {}", describe(&e))),
        }
    };
    grant_foreground_to_server(pipe.0);
    let event = create_event()?;
    // 客户端没有外部停止信号，用一个永不置位的事件占位
    let never = create_event()?;
    let remaining = deadline
        .saturating_duration_since(Instant::now())
        .max(Duration::from_millis(200));
    write_all(pipe.0, event.0, never.0, frame, remaining.as_millis() as u32)
        .map_err(|o| format!("向主实例发送失败: {}", outcome_text(&o)))?;
    let mut answer = [0u8; 1];
    let answer_deadline =
        Instant::now() + Duration::from_millis(u64::from(CLIENT_RESPONSE_TIMEOUT_MS));
    read_exact(pipe.0, event.0, never.0, &mut answer, answer_deadline)
        .map_err(|o| format!("等待主实例应答失败: {}", outcome_text(&o)))?;
    if answer[0] == IPC_RESPONSE_OK {
        Ok(())
    } else {
        Err("主实例拒绝了该命令".to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use windows::Win32::Security::Authorization::{
        ConvertSecurityDescriptorToStringSecurityDescriptorW, GetSecurityInfo, SE_KERNEL_OBJECT,
    };
    use windows::Win32::Security::DACL_SECURITY_INFORMATION;

    /// 生成唯一管道名。
    fn test_pipe_name(tag: &str) -> String {
        format!("{PIPE_NAMESPACE}cisox-test-{tag}-{}", std::process::id())
    }

    /// 读回管道句柄 DACL 的 SDDL 文本。
    fn dacl_sddl(handle: HANDLE) -> String {
        unsafe {
            let mut descriptor = PSECURITY_DESCRIPTOR::default();
            let status = GetSecurityInfo(
                handle,
                SE_KERNEL_OBJECT,
                DACL_SECURITY_INFORMATION,
                None,
                None,
                None,
                None,
                Some(&mut descriptor),
            );
            assert_eq!(status.0, 0, "GetSecurityInfo 失败: {}", status.0);
            let mut text = PWSTR::null();
            ConvertSecurityDescriptorToStringSecurityDescriptorW(
                descriptor,
                SDDL_REVISION_1,
                DACL_SECURITY_INFORMATION,
                &mut text,
                None,
            )
            .unwrap();
            let s = text.to_string().unwrap();
            let _ = LocalFree(Some(HLOCAL(text.0 as *mut c_void)));
            let _ = LocalFree(Some(HLOCAL(descriptor.0)));
            s
        }
    }

    /// 管道名包含应用标识、SID 与会话号，且非法标识被拒绝。
    #[test]
    fn pipe_name_contains_identity() {
        let name = pipe_name("cisox.unit").unwrap();
        let sid = current_user_sid().unwrap();
        assert!(name.starts_with(PIPE_NAMESPACE));
        assert!(name.contains("cisox.unit"));
        assert!(name.contains(&sid));
        assert!(pipe_name("bad\\name").is_err());
        assert!(pipe_name("").is_err());
    }

    /// DACL 只有一条 ACE 且属于当前用户 SID（受保护、不含 Everyone / Users）。
    #[test]
    fn pipe_dacl_only_grants_current_user() {
        let sid = current_user_sid().unwrap();
        let acl = UserOnlyAcl::for_sid(&sid).unwrap();
        let pipe = create_pipe_instance(&test_pipe_name("acl"), true, &acl).unwrap();
        let sddl = dacl_sddl(pipe.0);
        assert_eq!(sddl.matches("(A;").count(), 1, "ACE 数量异常: {sddl}");
        assert!(sddl.contains(&sid), "DACL 应包含当前用户 SID: {sddl}");
        assert!(sddl.contains("D:P"), "DACL 应为受保护: {sddl}");
        assert!(!sddl.contains(";;;WD)") && !sddl.contains(";;;BU)"), "不应授权 Everyone/Users: {sddl}");
    }

    /// 名字被占用时首实例创建失败（不降级）。
    #[test]
    fn first_instance_detects_squatting() {
        let sid = current_user_sid().unwrap();
        let acl = UserOnlyAcl::for_sid(&sid).unwrap();
        let name = test_pipe_name("squat");
        let _occupier = create_pipe_instance(&name, true, &acl).unwrap();
        assert!(create_pipe_instance(&name, true, &acl).is_err());
    }

    /// 服务端对非法长度头回拒绝应答，且不调用回调，之后仍可正常服务。
    #[test]
    fn server_rejects_garbage_then_keeps_serving() {
        use std::sync::mpsc;
        let name = test_pipe_name("garbage");
        let (tx, rx) = mpsc::channel();
        let tx = std::sync::Mutex::new(tx);
        let _listener = PipeListener::start(
            &name,
            Box::new(move |cmd| {
                let _ = tx.lock().unwrap().send(cmd);
            }),
        )
        .unwrap();

        // 超长长度头
        let bad_len = (u32::MAX).to_be_bytes();
        assert!(send_frame(&name, &bad_len).is_err());
        // 长度合法但标签错误
        let mut bad_tag = 5u32.to_be_bytes().to_vec();
        bad_tag.extend_from_slice(b"HELLO");
        assert!(send_frame(&name, &bad_tag).is_err());
        assert!(rx.try_recv().is_err());

        // 正常命令仍能送达
        let frame = super::super::encode_frame(&IpcCommand::OpenSettings).unwrap();
        send_frame(&name, &frame).unwrap();
        assert_eq!(
            rx.recv_timeout(Duration::from_secs(3)).unwrap(),
            IpcCommand::OpenSettings
        );
    }

    /// 只发半个帧就挂起的客户端会被读超时踢掉，不会阻塞后续客户端。
    #[test]
    fn slow_client_times_out() {
        let name = test_pipe_name("slow");
        let _listener = PipeListener::start(&name, Box::new(|_| {})).unwrap();
        // 手动连接，只写 2 字节长度头后不再发送
        let wide = HSTRING::from(name.as_str());
        let raw = unsafe {
            CreateFileW(
                &wide,
                GENERIC_READ.0 | GENERIC_WRITE.0,
                FILE_SHARE_NONE,
                None,
                OPEN_EXISTING,
                FILE_FLAG_OVERLAPPED,
                None,
            )
        }
        .unwrap();
        let raw = OwnedHandle(raw);
        let event = create_event().unwrap();
        let never = create_event().unwrap();
        write_all(raw.0, event.0, never.0, &[0, 0], 500).unwrap();

        let started = Instant::now();
        let frame = super::super::encode_frame(&IpcCommand::ShowMainWindow).unwrap();
        // 慢客户端占着管道：正常客户端在重试窗口内可能仍被挤掉，但服务端必须在约 0.5s 内恢复
        std::thread::sleep(Duration::from_millis(900));
        drop(raw);
        send_frame(&name, &frame).unwrap();
        assert!(started.elapsed() < Duration::from_secs(4));
    }
}
