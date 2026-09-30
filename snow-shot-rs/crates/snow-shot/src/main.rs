//! 程序入口：命令行、单实例、存储解析、日志初始化，随后进入 GPUI 常驻事件循环。
//!
//! 所属阶段：A。主实例启动托盘 / 热键 / IPC 监听并常驻；从属实例把命令交给主实例后退出。

// 发布形态使用 GUI 子系统（不弹控制台窗口）；调试构建保留控制台便于看日志
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use snow_app_core::PRODUCT_NAME;
use snow_app_core::bus::CommandBus;
use snow_app_core::logging::{CRASH_DIR_NAME, LOG_DIR_NAME, LogConfig, LogGuard, init_logging};
use snow_capability::CapabilityRegistry;
use snow_config::paths::{StorageDirectorySelection, default_app_data_directory, resolve_directory};
use snow_platform::crash::{
    CrashDumpConfig, CrashGuard, DEFAULT_MAX_REPORTS, install as install_crash_handler,
};
use snow_platform::single_instance::{
    IpcCommand, SingleInstanceGuard, SingleInstanceManager, SingleInstanceStatus,
};
use snow_ui::shell::inbox::MainThreadInbox;
use snow_ui::ui;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

pub mod annotation;
pub mod app_runtime;
pub mod capture_flow;
pub mod frozen_frame;
pub mod ocr_assets;
pub mod ocr_client;
pub mod ocr_download;
pub mod ocr_flow;
pub mod ocr_service;
pub mod ort_runtime;
pub mod overlay_probe;
pub mod overlay_view;
pub mod pinned_manager;
pub mod pinned_model;
pub mod pinned_shared;
pub mod pinned_view;
pub mod recording;
pub mod recording_flow;
pub mod screenshot_output;
pub mod scroll_capture;
pub mod scroll_view;
pub mod settings_model;
pub mod settings_state;
pub mod settings_text;
pub mod settings_view;
pub mod stitch_service;
#[cfg(test)]
mod stitch_audit_tests;
pub mod sys_prefs;
pub mod translate_flow;
pub mod translate_layout;
pub mod translate_service;

/// 单实例互斥体 / 管道名使用的应用标识。
pub const SINGLE_INSTANCE_APP_ID: &str = "cisox.snow_shot.single_instance";

/// 命令行参数：向主实例发送的命令。
const CLI_FLAG_COMMAND: &str = "--cmd";

/// 从属实例发送失败时的退出码。
const EXIT_CODE_IPC_FAILED: u8 = 1;

/// 命令行参数非法时的退出码。
const EXIT_CODE_BAD_ARGS: u8 = 2;

/// 命令行选项。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CliOptions {
    /// 本进程若是从属实例，要交给主实例的命令（默认唤醒）。
    pub command: IpcCommand,
}

/// 解析命令行参数（不含程序名）。
///
/// # 参数
/// - `args`：参数序列；支持 `--cmd <screenshot|recording|scroll-capture|pin-clipboard|settings|show|quit>`。
///
/// # 返回
/// 选项；出现未知参数或非法命令名返回错误文本。
///
/// ```
/// use snow_platform::single_instance::IpcCommand;
/// let opts = snow_shot::parse_cli(["--cmd".to_string(), "quit".to_string()]).unwrap();
/// assert_eq!(opts.command, IpcCommand::Quit);
/// ```
pub fn parse_cli(args: impl IntoIterator<Item = String>) -> Result<CliOptions, String> {
    let mut command = IpcCommand::ShowMainWindow;
    let mut iter = args.into_iter();
    while let Some(arg) = iter.next() {
        if arg == CLI_FLAG_COMMAND {
            let name = iter
                .next()
                .ok_or_else(|| format!("{CLI_FLAG_COMMAND} 缺少参数"))?;
            command = IpcCommand::from_cli_name(&name)
                .ok_or_else(|| format!("未知命令: {name}"))?;
        } else {
            return Err(format!("未知参数: {arg}"));
        }
    }
    Ok(CliOptions { command })
}

/// 运行时启动上下文。
pub struct AppBootstrap {
    /// 存储目录解析结果。
    pub storage: StorageDirectorySelection,
    /// 实际生效的数据根目录。
    pub data_root: PathBuf,
    /// 全局日志守卫（必须在进程存活期间持有）。
    pub log_guard: LogGuard,
    /// 本地崩溃转储守卫（若安装成功）。
    pub crash_guard: Option<CrashGuard>,
    /// 当前平台能力表。
    pub capabilities: CapabilityRegistry,
    /// 单实例独占守卫；`None` 表示已有主实例在运行（本进程为从属实例）。
    pub single_instance: Option<SingleInstanceGuard>,
}

/// 执行应用基础运行时引导。
///
/// 解析可执行文件目录与标准应用数据目录，选择生效存储位置并初始化日志与崩溃转储，
/// 并尝试获取单实例所有权（不发送任何 IPC，是否转交命令由调用方决定）。
///
/// # 参数
/// - `exe_dir`：当前可执行文件所在目录，若为空则自动通过进程环境定位。
///
/// # 返回
/// 初始化的运行上下文，包含数据目录与生命周期守卫。
///
/// # 示例
/// ```no_run
/// use snow_shot::bootstrap;
/// let _ctx = bootstrap(None);
/// ```
pub fn bootstrap(exe_dir: Option<&Path>) -> AppBootstrap {
    let current_exe_dir = exe_dir
        .map(|p| p.to_path_buf())
        .or_else(|| {
            std::env::current_exe()
                .ok()
                .and_then(|p| p.parent().map(|d| d.to_path_buf()))
        })
        .unwrap_or_else(|| PathBuf::from("."));

    let app_data_dir =
        default_app_data_directory().unwrap_or_else(|| current_exe_dir.join("data"));

    let storage = resolve_directory(&current_exe_dir, &app_data_dir);
    let data_root = storage
        .effective_directory
        .clone()
        .unwrap_or_else(|| current_exe_dir.clone());

    let log_config = LogConfig::new(&data_root);
    let log_guard = init_logging(log_config);

    let crash_dir = data_root.join(LOG_DIR_NAME).join(CRASH_DIR_NAME);
    let crash_config = CrashDumpConfig {
        crash_dir,
        app_name: PRODUCT_NAME.to_string(),
        app_version: env!("CARGO_PKG_VERSION").to_string(),
        max_reports: DEFAULT_MAX_REPORTS,
    };
    let crash_guard = install_crash_handler(crash_config).ok();
    let capabilities = CapabilityRegistry::for_current_platform();

    let single_instance = match SingleInstanceManager::acquire(SINGLE_INSTANCE_APP_ID) {
        SingleInstanceStatus::Primary(guard) => Some(guard),
        SingleInstanceStatus::Secondary => None,
    };

    tracing::info!(
        app = PRODUCT_NAME,
        version = env!("CARGO_PKG_VERSION"),
        data_root = %data_root.display(),
        storage_mode = ?storage.mode,
        is_primary = single_instance.is_some(),
        "runtime bootstrap completed"
    );

    AppBootstrap {
        storage,
        data_root,
        log_guard,
        crash_guard,
        capabilities,
        single_instance,
    }
}

/// 生成启动横幅文本。
pub fn banner() -> String {
    format!("{} {}", PRODUCT_NAME, env!("CARGO_PKG_VERSION"))
}

/// 从属实例：把命令交给主实例，返回进程退出码。
fn run_secondary(command: &IpcCommand) -> ExitCode {
    tracing::info!(?command, "secondary instance: delegating command to primary");
    match SingleInstanceManager::send_command_to_primary(SINGLE_INSTANCE_APP_ID, command) {
        Ok(()) => {
            tracing::info!("secondary instance: command delivered, exiting");
            ExitCode::SUCCESS
        }
        Err(e) => {
            tracing::error!(error = %e, "secondary instance: 命令投递失败");
            eprintln!("无法联系运行中的实例: {e}");
            ExitCode::from(EXIT_CODE_IPC_FAILED)
        }
    }
}

/// 主实例：启动 IPC 监听并进入 GPUI 常驻事件循环，直到收到退出事件。
fn run_primary(ctx: &AppBootstrap, guard: &SingleInstanceGuard) -> ExitCode {
    let inbox: MainThreadInbox<app_runtime::UiEvent> = MainThreadInbox::new();

    let ipc_inbox = inbox.clone();
    if let Err(e) = SingleInstanceManager::start_listener(guard, move |cmd| {
        match app_runtime::map_ipc_command(&cmd) {
            Some(event) => {
                ipc_inbox.push(event);
            }
            None => tracing::warn!(?cmd, "IPC 命令无对应动作，已忽略"),
        }
    }) {
        // 不静默：无 IPC 时第二实例无法唤醒本实例，但本实例仍可正常运行
        tracing::error!(error = %e, "单实例 IPC 不可用，第二实例将无法唤醒本实例");
    }

    let caps = ctx.capabilities.clone();
    let data_root = ctx.data_root.clone();
    ui::run_resident(move |cx| {
        let bus = CommandBus::new();
        app_runtime::register_bus_handlers(&bus, &inbox);
        let state_inbox = inbox.clone();
        let restore_inbox = inbox.clone();
        let config = app_runtime::open_shared_config(&data_root);
        let (tray, hotkeys, hotkey_handles) =
            app_runtime::start_services(&caps, &bus, &inbox, config.borrow().document());
        let mut state = app_runtime::AppState::new(
            config,
            state_inbox,
            caps.clone(),
            tray,
            hotkeys,
            hotkey_handles,
            &data_root,
        );
        tracing::info!("event loop started (resident, QuitMode::Explicit)");
        cx.run_inbox(inbox, move |cx, event| {
            app_runtime::handle_event(cx, &mut state, event);
        });
        // 启动即恢复上次遗留的贴图窗口
        restore_inbox.push(app_runtime::UiEvent::RestorePins);
    });
    tracing::info!("event loop ended");
    ExitCode::SUCCESS
}

/// 程序入口。
fn main() -> ExitCode {
    // GUI 子系统没有控制台：带命令行参数（如 `--cmd`）时附着父控制台，让用法提示和错误输出可见
    if !cfg!(debug_assertions) && std::env::args_os().nth(1).is_some() {
        snow_platform::console::attach_parent_console();
    }
    let options = match parse_cli(std::env::args().skip(1)) {
        Ok(o) => o,
        Err(e) => {
            eprintln!("{e}\n用法: snow-shot [{CLI_FLAG_COMMAND} screenshot|recording|scroll-capture|pin-clipboard|settings|show|quit]");
            return ExitCode::from(EXIT_CODE_BAD_ARGS);
        }
    };
    println!("{}", banner());
    let ctx = bootstrap(None);
    match &ctx.single_instance {
        Some(guard) => run_primary(&ctx, guard),
        None => run_secondary(&options.command),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 横幅应包含产品名。
    #[test]
    fn banner_contains_product_name() {
        assert!(banner().starts_with(PRODUCT_NAME));
    }

    /// 在临时目录模拟引导，能正常产出有效数据根与能力表。
    #[test]
    fn bootstrap_in_temp_dir() {
        let temp = std::env::temp_dir().join(format!("snow-shot-test-{}", std::process::id()));
        let ctx = bootstrap(Some(&temp));
        assert!(!ctx.data_root.as_os_str().is_empty());
        assert!(ctx.capabilities.query(snow_capability::Capability::CrashDump).is_usable());
        let _ = std::fs::remove_dir_all(&temp);
    }

    /// 无参数时默认发送唤醒命令。
    #[test]
    fn cli_defaults_to_show() {
        assert_eq!(parse_cli([]).unwrap().command, IpcCommand::ShowMainWindow);
    }

    /// `--cmd` 支持全部命令名。
    #[test]
    fn cli_parses_commands() {
        for (name, want) in [
            ("screenshot", IpcCommand::TriggerScreenshot),
            ("recording", IpcCommand::TriggerRecording),
            ("scroll-capture", IpcCommand::ScrollCapture),
            ("pin-clipboard", IpcCommand::PinClipboard),
            ("settings", IpcCommand::OpenSettings),
            ("show", IpcCommand::ShowMainWindow),
            ("QUIT", IpcCommand::Quit),
        ] {
            let got = parse_cli(["--cmd".to_string(), name.to_string()]).unwrap();
            assert_eq!(got.command, want);
        }
    }

    /// 缺参数、未知命令、未知参数都报错。
    #[test]
    fn cli_rejects_bad_input() {
        assert!(parse_cli(["--cmd".to_string()]).is_err());
        assert!(parse_cli(["--cmd".to_string(), "boom".to_string()]).is_err());
        assert!(parse_cli(["--wat".to_string()]).is_err());
    }
}
