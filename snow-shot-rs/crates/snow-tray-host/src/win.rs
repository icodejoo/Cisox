//! 真实平台（Windows）：会话管道、UI 进程、托盘、热键、前台授权、工作集修剪。
//!
//! 宿主自己的 Win32 调用只用 `windows-sys`（拉起 / 等待 / 结束 UI 进程、前台授权、`EmptyWorkingSet`），
//! 托盘与热键走 `snow-shell-core`（`tray-icon` / `global-hotkey`），管道走 `snow-platform`。
//! 所有后台线程（托盘、热键、管道读、进程等待）只往 [`Event`] 通道里投事件，状态都在主线程的
//! [`HostCore`](crate::host::HostCore) 里。

use crate::convert::{binding_from_spec, origin_from_shell, tray_menu, tray_spec_from_plan};
use crate::host::{Event, Platform, SpawnedUi, TrayPlan};
use snow_host_protocol::TOKEN_BYTES;
use snow_host_protocol::frame::encode_line;
use snow_host_protocol::launch::{FLAG_HEADLESS, HEADLESS_EMIT_SPEC, UiLaunch};
use snow_host_protocol::message::{HostMessage, hex_lower};
use snow_host_protocol::spec::{HostSpec, HotkeySpec, MenuItemSpec, TrayTheme};
use snow_host_protocol::spec_file::{SpecFileError, read_spec};
use snow_platform::CREATE_NO_WINDOW;
use snow_platform::single_instance::line_pipe::pipe_name_for;
use snow_platform::single_instance::session_pipe::{PipeWriter, SessionEvent, SessionPipeServer};
use snow_shell_core::dispatch::Dispatcher;
use snow_shell_core::hotkey::{HotkeyHandle, HotkeyService};
use snow_shell_core::native::set_popup_menu_dark;
use snow_shell_core::tray::TrayService;
use std::os::windows::process::CommandExt;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::mpsc::Sender;
use windows_sys::Win32::Foundation::{CloseHandle, HANDLE};
use windows_sys::Win32::Security::Cryptography::ProcessPrng;
use windows_sys::Win32::System::ProcessStatus::K32EmptyWorkingSet;
use windows_sys::Win32::System::Threading::{
    GetCurrentProcess, INFINITE, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION,
    PROCESS_SYNCHRONIZE, PROCESS_TERMINATE, TerminateProcess, WaitForSingleObject,
};
use windows_sys::Win32::UI::WindowsAndMessaging::AllowSetForegroundWindow;

/// UI 进程的 exe 文件名（与宿主同目录）。
pub const UI_EXE_NAME: &str = "snow-shot-ui.exe";

/// 环境变量：覆盖 UI 进程 exe 的路径（开发与验收脚本用；正式安装不设置）。
pub const ENV_UI_EXE: &str = "SNOW_UI_EXE";

/// 随机会话标识的字节数（编码成十六进制后进入管道名）。
const LAUNCH_ID_BYTES: usize = 8;

/// 强制结束 UI 进程时给的退出码。
const KILL_EXIT_CODE: u32 = 1;

/// 重启时，旧宿主退出后等多久（`ping` 次数，每次约 1 秒）再拉起新宿主，保证互斥体已释放。
const RESTART_WAIT_PINGS: u32 = 3;

/// 自己持有的内核进程句柄（关闭时自动释放）。
struct ProcessHandle(HANDLE);

// 进程句柄是内核对象句柄，可在线程间转移；本模块保证关闭前不再使用
unsafe impl Send for ProcessHandle {}

impl ProcessHandle {
    /// 按进程 ID 打开（要能等待、能结束、能查询）。
    fn open(pid: u32) -> Result<Self, String> {
        // SAFETY: 只传值参数；失败返回空句柄。
        let handle = unsafe {
            OpenProcess(
                PROCESS_SYNCHRONIZE | PROCESS_TERMINATE | PROCESS_QUERY_LIMITED_INFORMATION,
                0,
                pid,
            )
        };
        if handle.is_null() {
            Err(format!(
                "OpenProcess({pid}) 失败: {}",
                std::io::Error::last_os_error()
            ))
        } else {
            Ok(Self(handle))
        }
    }
}

impl Drop for ProcessHandle {
    /// 关闭句柄。
    fn drop(&mut self) {
        // SAFETY: 句柄由本对象独占持有，只关闭一次。
        unsafe {
            CloseHandle(self.0);
        }
    }
}

/// 当前这一代 UI 会话。
struct Session {
    /// 会话代数。
    generation: u64,
    /// 管道服务端（丢弃即停止并回收读线程）。
    server: SessionPipeServer,
    /// 写端。
    writer: PipeWriter,
    /// UI 进程 ID。
    pid: u32,
    /// 用于强制结束的进程句柄（等待用的是另一份，见 `watch_process`）。
    process: ProcessHandle,
}

/// 真实平台。
pub struct WinPlatform {
    /// 事件通道发送端（克隆给各后台线程）。
    tx: Sender<Event>,
    /// 数据根（读快照）。
    data_root: PathBuf,
    /// UI 进程 exe 路径。
    ui_exe: PathBuf,
    /// 本宿主 exe 路径（重启用）。
    host_exe: PathBuf,
    /// 当前 UI 会话。
    session: Option<Session>,
    /// 托盘服务。
    tray: Option<TrayService<String>>,
    /// 热键服务（第一次注册时才启动）。
    hotkeys: Option<HotkeyService<String>>,
    /// 已注册的热键句柄。
    hotkey_handles: Vec<HotkeyHandle>,
}

impl WinPlatform {
    /// 创建平台。
    ///
    /// # 参数
    /// - `tx`：事件通道发送端。
    /// - `data_root`：数据根目录。
    /// - `host_exe`：本宿主 exe 路径；UI 进程默认是它同目录的 `snow-shot-ui.exe`。
    ///
    /// # 返回
    /// 平台；UI exe 路径可用环境变量 [`ENV_UI_EXE`] 覆盖。
    pub fn new(tx: Sender<Event>, data_root: PathBuf, host_exe: PathBuf) -> Self {
        let ui_exe = std::env::var_os(ENV_UI_EXE)
            .map(PathBuf::from)
            .unwrap_or_else(|| host_exe.with_file_name(UI_EXE_NAME));
        Self {
            tx,
            data_root,
            ui_exe,
            host_exe,
            session: None,
            tray: None,
            hotkeys: None,
            hotkey_handles: Vec::new(),
        }
    }

    /// 取（必要时启动）热键服务。
    fn hotkey_service(&mut self) -> Result<&HotkeyService<String>, String> {
        if self.hotkeys.is_none() {
            let tx = self.tx.clone();
            let dispatcher = Dispatcher::from_fn(move |origin, id: String| {
                let _ = tx.send(Event::Action {
                    id,
                    origin: origin_from_shell(origin),
                });
            });
            self.hotkeys = Some(HotkeyService::start(dispatcher).map_err(|e| e.to_string())?);
        }
        self.hotkeys
            .as_ref()
            .ok_or_else(|| "热键服务未启动".to_string())
    }

    /// 起一个线程等进程退出，退出后投递 [`Event::UiExited`]（线程自己持有一份进程句柄）。
    fn watch_process(&self, generation: u64, pid: u32) -> Result<(), String> {
        let waiter = ProcessHandle::open(pid)?;
        let tx = self.tx.clone();
        std::thread::Builder::new()
            .name("ui-watch".into())
            .spawn(move || {
                // SAFETY: 句柄由 `waiter` 持有，等待期间有效。
                unsafe {
                    WaitForSingleObject(waiter.0, INFINITE);
                }
                drop(waiter);
                let _ = tx.send(Event::UiExited { generation });
            })
            .map(|_| ())
            .map_err(|e| format!("创建进程等待线程失败: {e}"))
    }
}

/// 生成 `n` 个随机字节的十六进制文本（令牌、一次性会话标识用）。
///
/// 走 `ProcessPrng`（`bcryptprimitives.dll`，Rust 标准库本来就导入它）：`snow_platform::random`
/// 用的 `BCryptGenRandom` 在 `bcrypt.dll`，会让宿主导入表多出一个 DLL。
fn random_hex(n: usize) -> Result<String, String> {
    let mut bytes = vec![0u8; n];
    // SAFETY: 缓冲区是有效的可写切片，长度与参数一致。
    let ok = unsafe { ProcessPrng(bytes.as_mut_ptr(), bytes.len()) } != 0;
    if !ok {
        return Err("ProcessPrng 失败".to_string());
    }
    Ok(hex_lower(&bytes))
}

impl Platform for WinPlatform {
    fn spawn_ui(&mut self, generation: u64) -> Result<SpawnedUi, String> {
        // 旧会话（已退出、等着被替换）先释放，避免两个管道服务端并存
        self.session = None;
        let app_id = format!(
            "{}.{}",
            snow_data_paths::SESSION_PIPE_APP_ID_PREFIX,
            random_hex(LAUNCH_ID_BYTES)?
        );
        let pipe = pipe_name_for(&app_id)?;
        let tx = self.tx.clone();
        // 先建管道、再拉起进程：UI 一启动就连得上，不存在“连不上”的窗口
        let server = SessionPipeServer::start(&pipe, move |event| {
            let message = match event {
                SessionEvent::Connected { client_pid } => Event::SessionConnected {
                    generation,
                    client_pid,
                },
                SessionEvent::Line(line) => Event::SessionLine { generation, line },
                SessionEvent::Disconnected => Event::SessionClosed { generation },
            };
            let _ = tx.send(message);
        })?;
        let token = random_hex(TOKEN_BYTES)?;
        let launch = UiLaunch::new(Some(app_id), Some(token.clone())).map_err(|e| e.to_string())?;
        let child = Command::new(&self.ui_exe)
            .args(launch.to_args())
            .creation_flags(CREATE_NO_WINDOW)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|e| format!("拉起 {} 失败: {e}", self.ui_exe.display()))?;
        let pid = child.id();
        // 趁 `child` 还持有进程，立刻按 PID 打开自己的句柄；之后 `child` 可以丢弃
        let process = ProcessHandle::open(pid)?;
        self.watch_process(generation, pid)?;
        drop(child);
        let writer = server.writer();
        self.session = Some(Session {
            generation,
            server,
            writer,
            pid,
            process,
        });
        tracing::info!(generation, pid, ui = %self.ui_exe.display(), "UI 进程已拉起");
        Ok(SpawnedUi { pid, token })
    }

    fn send(&mut self, generation: u64, message: &HostMessage) -> Result<(), String> {
        let session = self
            .session
            .as_ref()
            .filter(|s| s.generation == generation)
            .ok_or_else(|| format!("没有第 {generation} 代会话"))?;
        let line = encode_line(message).map_err(|e| e.to_string())?;
        session.writer.send_line(line.trim_end_matches('\n'))
    }

    fn close_session(&mut self, generation: u64) {
        if self
            .session
            .as_ref()
            .is_some_and(|s| s.generation == generation)
            && let Some(session) = self.session.take()
        {
            session.writer.close();
            // 先停读线程再释放句柄
            drop(session.server);
            drop(session.process);
        }
    }

    fn kill_ui(&mut self, pid: u32) {
        let Some(session) = self.session.as_ref().filter(|s| s.pid == pid) else {
            tracing::warn!(pid, "要结束的 UI 进程不在当前会话里，忽略");
            return;
        };
        // SAFETY: 句柄由会话持有，仍然有效。
        let ok = unsafe { TerminateProcess(session.process.0, KILL_EXIT_CODE) } != 0;
        tracing::warn!(pid, ok, "已强制结束 UI 进程");
    }

    fn grant_foreground(&mut self, pid: u32) {
        // SAFETY: 只传进程号值参数；失败只记日志，不影响命令投递。
        let ok = unsafe { AllowSetForegroundWindow(pid) } != 0;
        if !ok {
            tracing::debug!(pid, "授予 UI 前台权限失败");
        }
    }

    fn read_spec(&mut self) -> Result<HostSpec, SpecFileError> {
        read_spec(&snow_data_paths::host_spec_path(&self.data_root))
    }

    fn spawn_emit_spec(&mut self) -> Result<(), String> {
        let mut child = Command::new(&self.ui_exe)
            .args([FLAG_HEADLESS, HEADLESS_EMIT_SPEC])
            .creation_flags(CREATE_NO_WINDOW)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|e| format!("拉起 {} 失败: {e}", self.ui_exe.display()))?;
        let tx = self.tx.clone();
        std::thread::Builder::new()
            .name("emit-spec-watch".into())
            .spawn(move || {
                let status = child.wait();
                tracing::info!(?status, "快照生成任务已结束");
                let _ = tx.send(Event::SpecEmitted);
            })
            .map(|_| ())
            .map_err(|e| format!("创建等待线程失败: {e}"))
    }

    fn start_tray(&mut self, plan: &TrayPlan) -> Result<(), String> {
        let spec = tray_spec_from_plan(plan)?;
        // 旧托盘先释放（单例标记随之释放），再建新的
        self.tray = None;
        let tx = self.tx.clone();
        let dispatcher = Dispatcher::from_fn(move |origin, id: String| {
            let _ = tx.send(Event::Action {
                id,
                origin: origin_from_shell(origin),
            });
        });
        self.tray = Some(TrayService::start(spec, dispatcher).map_err(|e| e.to_string())?);
        Ok(())
    }

    fn set_tray_menu(&mut self, menu: &[MenuItemSpec]) -> Result<(), String> {
        let entries = tray_menu(menu)?;
        self.tray
            .as_ref()
            .ok_or_else(|| "托盘未创建".to_string())?
            .set_menu(entries)
            .map_err(|e| e.to_string())
    }

    fn set_tooltip(&mut self, text: &str) {
        if let Some(tray) = self.tray.as_ref()
            && let Err(e) = tray.set_tooltip(text)
        {
            tracing::warn!(error = %e, "更新托盘提示失败");
        }
    }

    fn stop_tray(&mut self) {
        self.tray = None;
    }

    fn set_popup_theme(&mut self, theme: TrayTheme) {
        let dark = match theme {
            TrayTheme::Dark => Some(true),
            TrayTheme::Light => Some(false),
            // 跟随系统：交给系统在弹菜单时按“应用模式”决定，不用宿主读注册表
            TrayTheme::System => None,
        };
        if let Err(e) = set_popup_menu_dark(dark) {
            tracing::warn!(error = %e, "设置托盘菜单主题失败");
        }
    }

    fn register_hotkeys(&mut self, hotkeys: &[HotkeySpec]) -> Vec<Result<(), String>> {
        let mut stale = std::mem::take(&mut self.hotkey_handles);
        let service = match self.hotkey_service() {
            Ok(service) => service,
            Err(e) => return hotkeys.iter().map(|_| Err(e.clone())).collect(),
        };
        for handle in stale.drain(..) {
            if let Err(e) = service.unregister(handle) {
                tracing::warn!(error = %e, "注销旧热键失败");
            }
        }
        let mut handles = Vec::new();
        let outcomes = hotkeys
            .iter()
            .map(|spec| {
                let binding = binding_from_spec(spec)?;
                let handle = service.register(binding).map_err(|e| e.to_string())?;
                handles.push(handle);
                Ok(())
            })
            .collect();
        self.hotkey_handles = handles;
        outcomes
    }

    fn fullscreen_focused(&mut self) -> bool {
        snow_platform::window_rect::focused_fullscreen_window_exists()
    }

    fn trim_working_set(&mut self) {
        // SAFETY: 伪句柄 `GetCurrentProcess` 无需关闭；失败只影响工作集，不影响功能。
        unsafe {
            K32EmptyWorkingSet(GetCurrentProcess());
        }
    }

    fn restart_host(&mut self) -> Result<(), String> {
        let line = format!(
            "/C ping -n {RESTART_WAIT_PINGS} 127.0.0.1 >NUL & start \"\" \"{}\"",
            self.host_exe.display()
        );
        Command::new("cmd")
            .raw_arg(line)
            .creation_flags(CREATE_NO_WINDOW)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .map(|_| ())
            .map_err(|e| format!("拉起重启助手失败: {e}"))
    }
}

#[cfg(test)]
mod tests;
