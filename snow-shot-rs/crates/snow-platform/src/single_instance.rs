//! 单实例检测与跨进程 IPC 通信（Single Instance & IPC）。
//!
//! 在 Windows 原生环境下基于互斥体与本地回环通道实现：
//! - 互斥防止多个应用实例重复拉起；
//! - 后续从属实例通过本地通道将启动参数和唤醒指令传递给首个主实例。

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::Duration;

#[cfg(windows)]
use windows::Win32::Foundation::{CloseHandle, ERROR_ALREADY_EXISTS, GetLastError, HANDLE};
#[cfg(windows)]
use windows::Win32::System::Threading::CreateMutexW;
#[cfg(windows)]
use windows::core::HSTRING;

/// 跨进程 IPC 控制指令。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IpcCommand {
    /// 触发截图覆盖窗。
    TriggerScreenshot,
    /// 触发屏幕录制。
    TriggerRecording,
    /// 打开首选项设置窗口。
    OpenSettings,
    /// 唤醒主窗口并置顶。
    ShowMainWindow,
    /// 自定义命令参数字符串。
    Custom(String),
}

impl IpcCommand {
    /// 转换为传输文本协议。
    pub fn to_payload(&self) -> String {
        match self {
            Self::TriggerScreenshot => "SCREENSHOT\n".to_string(),
            Self::TriggerRecording => "RECORDING\n".to_string(),
            Self::OpenSettings => "SETTINGS\n".to_string(),
            Self::ShowMainWindow => "SHOW\n".to_string(),
            Self::Custom(s) => format!("CUSTOM:{s}\n"),
        }
    }

    /// 从传输文本反序列化指令。
    pub fn from_payload(s: &str) -> Option<Self> {
        let trimmed = s.trim();
        match trimmed {
            "SCREENSHOT" => Some(Self::TriggerScreenshot),
            "RECORDING" => Some(Self::TriggerRecording),
            "SETTINGS" => Some(Self::OpenSettings),
            "SHOW" => Some(Self::ShowMainWindow),
            _ if trimmed.starts_with("CUSTOM:") => {
                Some(Self::Custom(trimmed[7..].to_string()))
            }
            _ => None,
        }
    }
}

/// 单实例获取判定结果。
pub enum SingleInstanceStatus {
    /// 当前为主实例（已独占互斥体，并持有守卫）。
    Primary(SingleInstanceGuard),
    /// 已有其他实例正在运行（作为从属实例）。
    Secondary,
}

/// 主实例互斥所有权守卫。释放时关闭句柄。
pub struct SingleInstanceGuard {
    /// 互斥体标识名。
    pub name: String,
    #[cfg(windows)]
    handle: HANDLE,
    /// 监听服务停止标志。
    is_running: Arc<AtomicBool>,
}

// Windows HANDLE 在句柄独占保护下是安全的
unsafe impl Send for SingleInstanceGuard {}
unsafe impl Sync for SingleInstanceGuard {}

impl Drop for SingleInstanceGuard {
    fn drop(&mut self) {
        self.is_running.store(false, Ordering::SeqCst);
        #[cfg(windows)]
        unsafe {
            if !self.handle.is_invalid() {
                let _ = CloseHandle(self.handle);
            }
        }
    }
}

/// 单实例探测与锁定器。
pub struct SingleInstanceManager;

impl SingleInstanceManager {
    /// 尝试以指定应用名获取主实例所有权。
    ///
    /// # 参数
    /// - `app_id`：全局唯一互斥体标识符（例如 `"cisox.snow_shot.single_instance"`）。
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
                        is_running: Arc::new(AtomicBool::new(true)),
                    })
                }
            }
        }

        #[cfg(not(windows))]
        {
            // 非 Windows 平台的模拟实现
            SingleInstanceStatus::Primary(SingleInstanceGuard {
                name: app_id.to_string(),
                is_running: Arc::new(AtomicBool::new(true)),
            })
        }
    }

    /// 作为从属实例向主实例发送唤醒控制指令。
    ///
    /// # 参数
    /// - `port`：主实例绑定的本地监听端口。
    /// - `command`：需要投递的控制指令。
    pub fn send_command_to_primary(port: u16, command: &IpcCommand) -> Result<(), String> {
        let mut stream = TcpStream::connect(("127.0.0.1", port))
            .map_err(|e| format!("无法连接至运行中的主实例 (端口 {port}): {e}"))?;
        stream
            .set_write_timeout(Some(Duration::from_millis(500)))
            .map_err(|e| e.to_string())?;

        let payload = command.to_payload();
        stream
            .write_all(payload.as_bytes())
            .map_err(|e| format!("向主实例发送数据失败: {e}"))?;
        let _ = stream.flush();
        Ok(())
    }

    /// 在主实例后台线程启动本地 IPC 服务监听。
    ///
    /// # 参数
    /// - `guard`：当前持有的主实例守卫。
    /// - `port`：监听的固定端口。
    /// - `on_command`：接收到从属实例指令时的处理回调。
    pub fn start_listener<F>(
        guard: &SingleInstanceGuard,
        port: u16,
        on_command: F,
    ) -> Result<(), String>
    where
        F: Fn(IpcCommand) + Send + 'static,
    {
        let listener = TcpListener::bind(("127.0.0.1", port))
            .map_err(|e| format!("绑定主实例 IPC 端口 {port} 失败: {e}"))?;
        listener
            .set_nonblocking(true)
            .map_err(|e| e.to_string())?;

        let running = Arc::clone(&guard.is_running);

        thread::spawn(move || {
            let mut buf = [0u8; 512];
            while running.load(Ordering::SeqCst) {
                match listener.accept() {
                    Ok((mut stream, _)) => {
                        let _ = stream.set_read_timeout(Some(Duration::from_millis(300)));
                        let cmd_opt = stream.read(&mut buf).ok().and_then(|n| {
                            std::str::from_utf8(&buf[..n]).ok().and_then(IpcCommand::from_payload)
                        });
                        if let Some(cmd) = cmd_opt {
                            on_command(cmd);
                        }
                    }
                    Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(50));
                    }
                    Err(_) => {
                        break;
                    }
                }
            }
        });

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 验证 IPC 命令文本编解码对称性。
    #[test]
    fn test_ipc_command_serialization() {
        let commands = [
            IpcCommand::TriggerScreenshot,
            IpcCommand::TriggerRecording,
            IpcCommand::OpenSettings,
            IpcCommand::ShowMainWindow,
            IpcCommand::Custom("arg_value".into()),
        ];

        for cmd in &commands {
            let payload = cmd.to_payload();
            let parsed = IpcCommand::from_payload(&payload);
            assert_eq!(parsed.as_ref(), Some(cmd));
        }
    }

    /// 验证单实例互斥判定（主实例占有后，从属实例应判定冲突）。
    #[test]
    fn test_single_instance_acquisition() {
        let app_id = "cisox_test_single_instance_lock";
        let status1 = SingleInstanceManager::acquire(app_id);

        if let SingleInstanceStatus::Primary(guard) = status1 {
            assert_eq!(guard.name, app_id);

            // 第二次同名获取应当报告 Secondary
            #[cfg(windows)]
            {
                let status2 = SingleInstanceManager::acquire(app_id);
                assert!(matches!(status2, SingleInstanceStatus::Secondary));
            }

            // guard drop 后资源释放
            drop(guard);
        }
    }

    /// 验证主从 IPC 通信与消息接收。
    #[test]
    fn test_ipc_communication_roundtrip() {
        let app_id = "cisox_test_ipc_roundtrip";
        let status = SingleInstanceManager::acquire(app_id);
        let guard = match status {
            SingleInstanceStatus::Primary(g) => g,
            SingleInstanceStatus::Secondary => return,
        };

        let test_port = 49152; // Ephemeral port
        let received = Arc::new(std::sync::Mutex::new(None));
        let r_clone = Arc::clone(&received);

        let res = SingleInstanceManager::start_listener(&guard, test_port, move |cmd| {
            let mut lock = r_clone.lock().unwrap();
            *lock = Some(cmd);
        });

        if res.is_ok() {
            thread::sleep(Duration::from_millis(60));
            let send_res = SingleInstanceManager::send_command_to_primary(
                test_port,
                &IpcCommand::TriggerScreenshot,
            );
            assert!(send_res.is_ok());

            thread::sleep(Duration::from_millis(150));
            let lock = received.lock().unwrap();
            assert_eq!(*lock, Some(IpcCommand::TriggerScreenshot));
        }
    }
}
