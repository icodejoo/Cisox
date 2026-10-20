//! UI 进程入口（产物 `cisox-ui.exe`）：命令行、存储解析、日志初始化，随后进入 GPUI 事件循环。
//!
//! 四种运行方式（方案 `docs/design/tray-host-split.md` §3）：
//! - **转调宿主**（无参数或只带 `--cmd`，阶段 4 起的默认）：不在本进程里干活，拉起同目录的托盘宿主
//!   `cisox.exe`（`--cmd` 原样转给它）后退出；老用户注册表里指向本 exe 的开机自启也走这条。
//! - **单进程常驻**（显式 `--resident`，回退开关）：本进程自带托盘 / 热键 / IPC 监听并常驻，
//!   从属实例（`--resident --cmd`）把命令交给它后退出。
//! - **双进程 UI**（`--ui --session <名> --token <令牌> [--hint dictation]`，由托盘宿主 `cisox.exe` 拉起）：
//!   不持托盘与热键，连宿主的会话管道收动作、写 `host/spec.json` 快照，所有窗口关闭且无活动后请求退出。
//!   带 `--hint dictation` 时（宿主队首命令是语音转文字）在 `main` 最早处并行预拉起语音 worker。
//! - **无窗口任务**（`--headless emit-spec | update-check | set-config`）：不初始化 GPUI，做完就退（见 [`headless`]）。

// 发布形态使用 GUI 子系统（不弹控制台窗口）；调试构建保留控制台便于看日志
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use app_runtime::{AppState, HostSession, UiEvent};
use host_link::HostEvent;
use host_spec_sync::SpecSync;
use snow_app_core::PRODUCT_NAME;
use snow_app_core::bus::CommandBus;
use snow_app_core::logging::{CRASH_DIR_NAME, LOG_DIR_NAME, LogConfig, LogGuard, init_logging};
use snow_capability::CapabilityRegistry;
use snow_config::paths::{
    StorageDirectorySelection, default_app_data_directory, resolve_directory,
};
use snow_data_paths::{UI_INSTANCE_APP_ID, host_spec_path};
use snow_host_protocol::launch::{
    FLAG_HEADLESS, FLAG_HINT, FLAG_KEY, FLAG_RESIDENT, FLAG_SESSION, FLAG_TOKEN, FLAG_UI,
    FLAG_VALUE, HOST_EXE_NAME, HeadlessJob, UiHint, UiLaunch,
};
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

pub mod activity;
pub mod annotation;
pub mod annotation_style;
pub mod app_runtime;
pub mod auto_filter;
pub mod capture_flow;
pub mod config_transfer;
pub mod conversion_guide;
pub mod custom_models_edit;
pub mod decoration_style;
pub mod desktop_frames;
pub mod dictation;
pub mod direct_capture;
pub mod direct_preview;
pub mod export_format;
pub mod export_naming;
pub mod export_pdf;
pub mod frozen_frame;
pub mod fullscreen_gate;
pub mod headless;
pub mod history_nav;
pub mod history_store;
pub mod history_view;
pub mod host_link;
pub mod host_spec;
pub mod host_spec_sync;
pub mod hotkey_plan;
pub mod language_names;
pub mod latex_assets;
pub mod latex_ocr;
pub mod latex_service;
pub mod mcp_host;
pub mod mcp_settings;
pub mod mem_diag;
pub mod model_catalog;
pub mod model_pick;
pub mod mouse_gesture;
pub mod net_settings;
pub mod ocr_assets;
pub mod ocr_backend;
#[cfg(test)]
mod ocr_box_probe;
pub mod ocr_client;
pub mod ocr_download;
pub mod ocr_flow;
pub mod ocr_popup;
pub mod ocr_service;
pub mod ort_runtime;
pub mod overlay_keymap;
pub mod overlay_probe;
pub mod overlay_view;
mod pin_action_bar;
pub mod pinned_controls;
pub mod pinned_keymap;
pub mod pinned_manage;
pub mod pinned_manage_view;
pub mod pinned_manager;
pub mod pinned_model;
pub mod pinned_shared;
pub mod pinned_view;
pub mod previous_selection;
pub mod qr_decode;
pub mod quick_actions;
pub mod recognition_view;
pub mod recording;
pub mod recording_flow;
pub mod region_select;
pub mod scan_animation;
pub mod screenshot_output;
pub mod scroll_capture;
pub mod scroll_view;
pub mod select_kit;
pub mod settings_model;
pub mod settings_pages;
pub mod settings_state;
pub mod settings_text;
pub mod settings_view;
pub mod shared_keys;
#[cfg(test)]
mod stitch_audit_tests;
pub mod stitch_service;
pub mod stt_download;
pub mod stt_models;
pub mod stt_optimize;
pub mod stt_settings;
pub mod sys_prefs;
pub mod system_settings;
pub mod table_assets;
pub mod table_service;
pub mod table_structure;
#[cfg(test)]
mod test_util;
pub mod timing_diag;
pub mod translate_flow;
pub mod translate_history;
pub mod translate_input;
pub mod translate_input_session;
pub mod translate_input_view;
pub mod translate_layout;
pub mod translate_page;
pub mod translate_page_view;
pub mod translate_paint;
pub mod translate_render;
#[cfg(test)]
mod translate_render_scene;
pub mod translate_service;
pub mod translate_settings;
pub mod translate_typeset;
pub mod tray_config;
pub mod video_editor;
pub mod window_geometry;
pub mod window_pick;

/// 单实例互斥体 / 管道名使用的应用标识（与托盘宿主共用，所以 `--resident` 与宿主互斥）。
pub const SINGLE_INSTANCE_APP_ID: &str = snow_data_paths::SINGLE_INSTANCE_APP_ID;

/// 命令行参数：向主实例发送的命令。
const CLI_FLAG_COMMAND: &str = "--cmd";

/// 从属实例发送失败时的退出码。
const EXIT_CODE_IPC_FAILED: u8 = 1;

/// 命令行参数非法时的退出码。
const EXIT_CODE_BAD_ARGS: u8 = 2;

/// 已有 UI 进程在运行，本进程直接退出的退出码。
const EXIT_CODE_UI_DUPLICATE: u8 = 3;

/// 连不上宿主会话管道的退出码。
const EXIT_CODE_HOST_LINK_FAILED: u8 = 4;

/// 无窗口任务失败的退出码。
const EXIT_CODE_HEADLESS_FAILED: u8 = 5;

/// 拉起托盘宿主失败的退出码。
const EXIT_CODE_HOST_LAUNCH_FAILED: u8 = 6;

/// 运行方式。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RunMode {
    /// 转去启动同目录的托盘宿主后退出（默认：无参数、或只带 `--cmd`）。
    Host,
    /// 单进程常驻（只有显式 `--resident` 才进，回退用）。
    Resident,
    /// 宿主拉起的 UI 进程（`--ui --session <名> --token <令牌>`）。
    Ui(UiLaunch),
    /// 无窗口任务（`--headless <任务>`）。
    Headless(HeadlessJob),
}

/// 命令行选项。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CliOptions {
    /// 本进程若是从属实例，要交给主实例的命令（默认唤醒）。
    pub command: IpcCommand,
    /// 运行方式。
    pub mode: RunMode,
}

/// 解析命令行参数（不含程序名）。
///
/// # 参数
/// - `args`：参数序列（无运行方式开关时是 [`RunMode::Host`]）；支持 `--cmd <screenshot|recording|scroll-capture|pin-clipboard|settings|show|quit>`、
///   `--resident`、`--ui --session <名> --token <令牌> [--hint dictation]`、`--headless emit-spec | update-check`、
///   `--headless set-config --key <配置键> --value <JSON>`。
///
/// # 返回
/// 选项；出现未知参数、非法命令名、互相冲突的运行方式或无效的会话参数返回错误文本。
///
/// ```
/// use snow_platform::single_instance::IpcCommand;
/// let opts = snow_shot::parse_cli(["--cmd".to_string(), "quit".to_string()]).unwrap();
/// assert_eq!(opts.command, IpcCommand::Quit);
/// ```
pub fn parse_cli(args: impl IntoIterator<Item = String>) -> Result<CliOptions, String> {
    let mut command = IpcCommand::ShowMainWindow;
    let (mut ui_flag, mut resident) = (false, false);
    let (mut session, mut token, mut headless) = (None, None, None);
    let (mut key, mut value) = (None, None);
    let mut hint = None;
    let mut iter = args.into_iter();
    let value_of = |iter: &mut dyn Iterator<Item = String>, flag: &str| {
        iter.next().ok_or_else(|| format!("{flag} 缺少参数"))
    };
    while let Some(arg) = iter.next() {
        match arg.as_str() {
            CLI_FLAG_COMMAND => {
                let name = value_of(&mut iter, CLI_FLAG_COMMAND)?;
                command =
                    IpcCommand::from_cli_name(&name).ok_or_else(|| format!("未知命令: {name}"))?;
            }
            FLAG_RESIDENT => resident = true,
            FLAG_UI => ui_flag = true,
            FLAG_SESSION => session = Some(value_of(&mut iter, FLAG_SESSION)?),
            FLAG_TOKEN => token = Some(value_of(&mut iter, FLAG_TOKEN)?),
            FLAG_HINT => hint = Some(value_of(&mut iter, FLAG_HINT)?),
            FLAG_HEADLESS => headless = Some(value_of(&mut iter, FLAG_HEADLESS)?),
            FLAG_KEY => key = Some(value_of(&mut iter, FLAG_KEY)?),
            FLAG_VALUE => value = Some(value_of(&mut iter, FLAG_VALUE)?),
            _ => return Err(format!("未知参数: {arg}")),
        }
    }
    if !ui_flag && (session.is_some() || token.is_some()) {
        return Err(format!(
            "{FLAG_SESSION} / {FLAG_TOKEN} 只能和 {FLAG_UI} 一起用"
        ));
    }
    if !ui_flag && hint.is_some() {
        return Err(format!("{FLAG_HINT} 只能和 {FLAG_UI} 一起用"));
    }
    if headless.is_none() && (key.is_some() || value.is_some()) {
        return Err(format!(
            "{FLAG_KEY} / {FLAG_VALUE} 只能和 {FLAG_HEADLESS} 一起用"
        ));
    }
    let selected = [ui_flag, resident, headless.is_some()]
        .iter()
        .filter(|on| **on)
        .count();
    if selected > 1 {
        return Err(format!(
            "{FLAG_UI} / {FLAG_RESIDENT} / {FLAG_HEADLESS} 只能选一个"
        ));
    }
    let mode = if ui_flag {
        let hint = match hint {
            Some(text) => UiHint::parse(&text).map_err(|e| e.to_string())?,
            None => UiHint::None,
        };
        RunMode::Ui(
            UiLaunch::new(session, token)
                .map_err(|e| e.to_string())?
                .with_hint(hint),
        )
    } else if let Some(job) = headless {
        RunMode::Headless(HeadlessJob::parse(&job, key, value).map_err(|e| e.to_string())?)
    } else if resident {
        RunMode::Resident
    } else {
        RunMode::Host
    };
    Ok(CliOptions { command, mode })
}

/// 运行时启动上下文。
pub struct AppBootstrap {
    /// 存储目录解析结果。
    pub storage: StorageDirectorySelection,
    /// 实际生效的数据根目录。
    pub data_root: PathBuf,
    /// 全局日志守卫（必须在进程存活期间持有）；从属实例不初始化，为 `None`。
    pub log_guard: Option<LogGuard>,
    /// 本地崩溃转储守卫（若安装成功）；从属实例不安装，为 `None`。
    pub crash_guard: Option<CrashGuard>,
    /// 当前平台能力表；仅主实例构建，从属实例为 `None`。
    pub capabilities: Option<CapabilityRegistry>,
    /// 单实例独占守卫；`None` 表示已有主实例在运行（本进程为从属实例），
    /// 或本进程是宿主拉起的 UI / 无窗口任务（不争主实例互斥体，它归宿主）。
    pub single_instance: Option<SingleInstanceGuard>,
}

/// 执行应用基础运行时引导（单进程常驻 / 从属实例用）。
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
    bootstrap_with(exe_dir, true)
}

/// 同 [`bootstrap`]，可选择是否争抢主实例互斥体。
///
/// 宿主拉起的 UI 进程与无窗口任务不争（互斥体归宿主，两种模式因此互斥），但仍要初始化日志、
/// 崩溃转储与能力表。
///
/// # 参数
/// - `exe_dir`：当前可执行文件所在目录，若为空则自动通过进程环境定位。
/// - `acquire_primary`：是否争抢主实例互斥体。
pub fn bootstrap_with(exe_dir: Option<&Path>, acquire_primary: bool) -> AppBootstrap {
    let current_exe_dir = exe_dir
        .map(|p| p.to_path_buf())
        .or_else(|| {
            std::env::current_exe()
                .ok()
                .and_then(|p| p.parent().map(|d| d.to_path_buf()))
        })
        .unwrap_or_else(|| PathBuf::from("."));

    let app_data_dir = default_app_data_directory().unwrap_or_else(|| current_exe_dir.join("data"));

    let storage = resolve_directory(&current_exe_dir, &app_data_dir);
    let data_root = storage
        .effective_directory
        .clone()
        .unwrap_or_else(|| current_exe_dir.clone());

    // 先抢单实例锁：从属实例只转发命令就退出，不必白做日志/崩溃处理器/能力表初始化
    let single_instance = if acquire_primary {
        match SingleInstanceManager::acquire(SINGLE_INSTANCE_APP_ID) {
            SingleInstanceStatus::Primary(guard) => Some(guard),
            SingleInstanceStatus::Secondary => None,
        }
    } else {
        None
    };
    // 抢到了主实例，或本来就不争（宿主拉起的 UI / 无窗口任务）：都要完整初始化
    let initializes_runtime = single_instance.is_some() || !acquire_primary;

    let (log_guard, crash_guard, capabilities) = if initializes_runtime {
        let log_guard = init_logging(LogConfig::new(&data_root));

        let crash_dir = data_root.join(LOG_DIR_NAME).join(CRASH_DIR_NAME);
        let crash_config = CrashDumpConfig {
            crash_dir,
            app_name: PRODUCT_NAME.to_string(),
            app_version: env!("CARGO_PKG_VERSION").to_string(),
            max_reports: DEFAULT_MAX_REPORTS,
        };
        let crash_guard = install_crash_handler(crash_config).ok();
        let capabilities = CapabilityRegistry::for_current_platform();

        tracing::info!(
            app = PRODUCT_NAME,
            version = env!("CARGO_PKG_VERSION"),
            data_root = %data_root.display(),
            storage_mode = ?storage.mode,
            is_primary = acquire_primary,
            "runtime bootstrap completed"
        );
        (Some(log_guard), crash_guard, Some(capabilities))
    } else {
        // 从属实例无 tracing 订阅者，日志调用为空操作；关键错误由 eprintln 与退出码承载
        (None, None, None)
    };

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
    tracing::info!(
        ?command,
        "secondary instance: delegating command to primary"
    );
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
    if let Err(e) =
        SingleInstanceManager::start_listener(guard, move |cmd| match app_runtime::map_ipc_command(
            &cmd,
        ) {
            Some(event) => {
                ipc_inbox.push(event);
            }
            None => tracing::warn!(?cmd, "IPC 命令无对应动作，已忽略"),
        })
    {
        // 不静默：无 IPC 时第二实例无法唤醒本实例，但本实例仍可正常运行
        tracing::error!(error = %e, "单实例 IPC 不可用，第二实例将无法唤醒本实例");
    }

    let caps = ctx.capabilities.clone().expect("主实例引导必须构建能力表");
    let data_root = ctx.data_root.clone();
    ui::run_resident(move |cx| {
        let bus = CommandBus::new();
        app_runtime::register_bus_handlers(&bus, &inbox);
        let state_inbox = inbox.clone();
        let restore_inbox = inbox.clone();
        let window_inbox = inbox.clone();
        let config = app_runtime::open_shared_config(&data_root);
        system_settings::apply(config.borrow().document());
        net_settings::apply_proxy(config.borrow().document());
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
        app_runtime::start_mouse_gesture(&mut state);
        state.sync_mcp();
        // 窗口关闭转成事件：输入框翻译浮窗据此回收没用上的预热
        cx.on_window_closed(move || {
            window_inbox.push(app_runtime::UiEvent::WindowClosed);
        });
        tracing::info!("event loop started (resident, QuitMode::Explicit)");
        cx.run_inbox(inbox, move |cx, event| {
            app_runtime::handle_event(cx, &mut state, event);
        });
        // 启动即恢复上次遗留的贴图窗口
        restore_inbox.push(app_runtime::UiEvent::RestorePins);
        // 后台自动检查更新（按 updates/mode 与最小间隔，不阻塞启动）
        restore_inbox.push(app_runtime::UiEvent::AutoUpdateCheck);
        // 模型热启动：开启时后台预加载 OCR 模型
        restore_inbox.push(app_runtime::UiEvent::OcrWarmUp);
    });
    tracing::info!("event loop ended");
    ExitCode::SUCCESS
}

/// 转调宿主：拉起同目录的 `cisox.exe` 并把命令行原样交给它，本进程随即退出（不等宿主）。
///
/// # 参数
/// - `host`：宿主 exe 路径。
/// - `args`：要转给宿主的参数（本进程的命令行，此时只可能有 `--cmd <命令>`）。
///
/// # 返回
/// 拉起成功为 `Ok`；宿主不存在或无法启动返回错误。
fn launch_host(host: &Path, args: &[String]) -> std::io::Result<()> {
    snow_net::quiet_command(host).args(args).spawn().map(|_| ())
}

/// 转调宿主模式的入口：定位同目录宿主并拉起，失败给出退出码。
///
/// # 参数
/// - `args`：本进程的命令行参数（不含程序名）。
fn run_host_forward(args: &[String]) -> ExitCode {
    let host = match std::env::current_exe() {
        Ok(exe) => exe.with_file_name(HOST_EXE_NAME),
        Err(e) => {
            eprintln!("取不到自身路径，无法定位 {HOST_EXE_NAME}: {e}");
            return ExitCode::from(EXIT_CODE_HOST_LAUNCH_FAILED);
        }
    };
    match launch_host(&host, args) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("拉起 {} 失败: {e}", host.display());
            ExitCode::from(EXIT_CODE_HOST_LAUNCH_FAILED)
        }
    }
}

/// 双进程 UI：连宿主会话管道，不持托盘 / 热键，所有窗口关闭且无活动后请求退出（宿主同意后立即退出）。
fn run_ui(ctx: &AppBootstrap, launch: &UiLaunch) -> ExitCode {
    // UI 进程自己的互斥体：已有 UI 在运行就让新来的直接退出（宿主有 10 秒握手超时兜底）
    let _ui_instance = match SingleInstanceManager::acquire(UI_INSTANCE_APP_ID) {
        SingleInstanceStatus::Primary(guard) => guard,
        SingleInstanceStatus::Secondary => {
            tracing::error!("已有 UI 进程在运行，本进程退出");
            return ExitCode::from(EXIT_CODE_UI_DUPLICATE);
        }
    };
    let inbox: MainThreadInbox<UiEvent> = MainThreadInbox::new();
    let link_inbox = inbox.clone();
    let link = match host_link::connect(launch, move |event| {
        link_inbox.push(UiEvent::Host(event));
    }) {
        Ok(link) => link,
        Err(e) => {
            tracing::error!(error = %e, "连不上托盘宿主的会话管道");
            return ExitCode::from(EXIT_CODE_HOST_LINK_FAILED);
        }
    };
    let caps = ctx.capabilities.clone().expect("UI 进程引导必须构建能力表");
    let data_root = ctx.data_root.clone();
    ui::run_resident(move |cx| {
        let bus = CommandBus::new();
        app_runtime::register_bus_handlers(&bus, &inbox);
        let state_inbox = inbox.clone();
        let window_inbox = inbox.clone();
        let startup_inbox = inbox.clone();
        let config = app_runtime::open_shared_config(&data_root);
        system_settings::apply(config.borrow().document());
        net_settings::apply_proxy(config.borrow().document());
        // 托盘、热键、鼠标手势都在宿主：这里不启动 `start_services`，也不装手势钩子
        let mut state = AppState::new(
            config,
            state_inbox,
            caps.clone(),
            None,
            None,
            Vec::new(),
            &data_root,
        );
        state.attach_host(HostSession::new(
            link,
            bus,
            SpecSync::open(host_spec_path(&data_root)),
        ));
        state.sync_mcp();
        cx.on_window_closed(move || {
            // 先投 WindowClosed：回收预热的守卫要在“能否退出”的判断之前拿到
            window_inbox.push(UiEvent::WindowClosed);
            window_inbox.push(UiEvent::Host(HostEvent::WindowsChanged));
        });
        tracing::info!("event loop started (ui session, QuitMode::Explicit)");
        cx.run_inbox(inbox, move |cx, event| {
            app_runtime::handle_event(cx, &mut state, event);
            state.after_event(cx);
        });
        // 启动完成：先做一次空闲判断。宿主在握手时已把排队的命令先于任何 exit_request 投递，
        // 所以这里即使判定空闲，也会被宿主按序号仲裁拒绝，不会丢命令
        startup_inbox.push(UiEvent::Host(HostEvent::WindowsChanged));
        // 模型热启动：开启时后台预加载 OCR 模型（事件内部会检查开关）
        startup_inbox.push(UiEvent::OcrWarmUp);
    });
    tracing::info!("event loop ended");
    ExitCode::SUCCESS
}

/// 无窗口任务：做完就退；更新检查发现新版本时把提示文案写到标准输出第一行（宿主放进托盘悬停提示）。
fn run_headless(ctx: &AppBootstrap, job: &HeadlessJob) -> ExitCode {
    match headless::run(&ctx.data_root, job) {
        Ok(notice) => {
            if let Some(text) = notice {
                use std::io::Write;
                // 宿主可能已不读（它退出了）：写失败不算任务失败
                let _ = writeln!(std::io::stdout(), "{text}");
            }
            ExitCode::SUCCESS
        }
        Err(e) => {
            tracing::error!(error = %e, job = job.name(), "无窗口任务失败");
            ExitCode::from(EXIT_CODE_HEADLESS_FAILED)
        }
    }
}

/// 程序入口。
fn main() -> ExitCode {
    // GUI 子系统没有控制台：带命令行参数（如 `--cmd`）时附着父控制台，让用法提示和错误输出可见；
    // 宿主拉起的 `--ui` / `--headless` 没有可附着的控制台，不必尝试
    let first = std::env::args_os().nth(1);
    let internal = first
        .as_ref()
        .is_some_and(|a| a == FLAG_UI || a == FLAG_HEADLESS);
    if !cfg!(debug_assertions) && first.is_some() && !internal {
        snow_platform::console::attach_parent_console();
    }
    let options = match parse_cli(std::env::args().skip(1)) {
        Ok(o) => o,
        Err(e) => {
            eprintln!(
                "{e}\n用法: cisox-ui [{CLI_FLAG_COMMAND} screenshot|recording|scroll-capture|pin-clipboard|settings|show|quit] [{FLAG_RESIDENT}]\n（不带 {FLAG_RESIDENT} 时转交给同目录的 {HOST_EXE_NAME}）"
            );
            return ExitCode::from(EXIT_CODE_BAD_ARGS);
        }
    };
    match options.mode {
        RunMode::Host => run_host_forward(&std::env::args().skip(1).collect::<Vec<_>>()),
        RunMode::Resident => {
            println!("{}", banner());
            let ctx = bootstrap(None);
            match &ctx.single_instance {
                Some(guard) => run_primary(&ctx, guard),
                None => run_secondary(&options.command),
            }
        }
        RunMode::Ui(launch) => {
            let ctx = bootstrap_with(None, false);
            // 宿主提示队首命令是听写：趁 UI 还没抢互斥体、连管道、初始化 GPUI，先并行拉起语音 worker
            if launch.hint == UiHint::Dictation {
                dictation::prespawn::begin_early(ctx.data_root.clone());
            }
            run_ui(&ctx, &launch)
        }
        RunMode::Headless(job) => run_headless(&bootstrap_with(None, false), &job),
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
        // 本机若已有主实例在运行，本进程是从属实例，不构建能力表
        if let Some(caps) = &ctx.capabilities {
            assert!(
                caps.query(snow_capability::Capability::CrashDump)
                    .is_usable()
            );
        } else {
            assert!(ctx.single_instance.is_none());
        }
        let _ = std::fs::remove_dir_all(&temp);
    }

    /// 无参数默认转调宿主，只带 `--cmd` 也是（命令交给宿主）；只有显式 `--resident` 才是单进程常驻。
    #[test]
    fn cli_defaults_to_host_and_resident_is_explicit() {
        assert_eq!(parse_cli([]).unwrap().mode, RunMode::Host);
        let cmd = parse_cli(["--cmd".to_string(), "quit".to_string()]).unwrap();
        assert_eq!(cmd.mode, RunMode::Host);
        assert_eq!(cmd.command, IpcCommand::Quit);
        assert_eq!(
            parse_cli(["--resident".to_string()]).unwrap().mode,
            RunMode::Resident
        );
        let resident_cmd = parse_cli(
            ["--resident", "--cmd", "show"]
                .iter()
                .map(|s| s.to_string())
                .collect::<Vec<_>>(),
        )
        .unwrap();
        assert_eq!(resident_cmd.mode, RunMode::Resident);
    }

    /// 宿主 exe 不存在时转调失败并返回错误（不 panic、不假装成功）。
    #[test]
    fn launch_host_reports_missing_exe() {
        let missing = std::env::temp_dir().join("cisox-no-such-host.exe");
        assert!(launch_host(&missing, &[]).is_err());
    }

    /// `--ui` 需要会话名与令牌，缺一不可，格式要对；解析后字段原样带出。
    #[test]
    fn cli_parses_ui_launch() {
        let token = "ab".repeat(32);
        let args = |items: &[&str]| items.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        let opts = parse_cli(args(&["--ui", "--session", "cisox.s.1", "--token", &token])).unwrap();
        assert_eq!(
            opts.mode,
            RunMode::Ui(UiLaunch::new(Some("cisox.s.1".into()), Some(token.clone())).unwrap())
        );
        assert!(parse_cli(args(&["--ui"])).is_err());
        assert!(parse_cli(args(&["--ui", "--session", "s"])).is_err());
        // 启动提示：带了就解析进 UiLaunch；不认识的取值、没有 --ui 都报错
        let hinted = parse_cli(args(&[
            "--ui",
            "--session",
            "cisox.s.1",
            "--token",
            &token,
            "--hint",
            "dictation",
        ]))
        .unwrap();
        let RunMode::Ui(launch) = hinted.mode else {
            panic!("应为 Ui");
        };
        assert_eq!(launch.hint, UiHint::Dictation);
        assert!(
            parse_cli(args(&[
                "--ui",
                "--session",
                "s",
                "--token",
                &token,
                "--hint",
                "bogus",
            ]))
            .is_err()
        );
        assert!(parse_cli(args(&["--hint", "dictation"])).is_err());
        assert!(parse_cli(args(&["--ui", "--session", "s", "--token", "短"])).is_err());
        assert!(
            parse_cli(args(&["--session", "s", "--token", &token])).is_err(),
            "没有 --ui"
        );
    }

    /// 运行方式互斥；未知无窗口任务报错；三种无窗口任务各自解析；`set-config` 要带键和 JSON 值。
    #[test]
    fn cli_modes_are_exclusive() {
        let args = |items: &[&str]| items.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert_eq!(
            parse_cli(args(&["--headless", "emit-spec"])).unwrap().mode,
            RunMode::Headless(HeadlessJob::EmitSpec)
        );
        assert_eq!(
            parse_cli(args(&["--headless", "update-check"]))
                .unwrap()
                .mode,
            RunMode::Headless(HeadlessJob::UpdateCheck)
        );
        assert_eq!(
            parse_cli(args(&[
                "--headless",
                "set-config",
                "--key",
                "a/b",
                "--value",
                r#"{"x":[1]}"#
            ]))
            .unwrap()
            .mode,
            RunMode::Headless(HeadlessJob::SetConfig {
                key: "a/b".into(),
                value: serde_json::json!({"x": [1]}),
            })
        );
        assert!(parse_cli(args(&["--headless", "set-config", "--value", "1"])).is_err());
        assert!(parse_cli(args(&["--headless", "set-config", "--key", "k"])).is_err());
        assert!(
            parse_cli(args(&[
                "--headless",
                "set-config",
                "--key",
                "k",
                "--value",
                "{oops"
            ]))
            .is_err()
        );
        assert!(
            parse_cli(args(&["--key", "k", "--value", "1"])).is_err(),
            "没有 --headless"
        );
        assert!(parse_cli(args(&["--headless", "nope"])).is_err());
        assert!(parse_cli(args(&["--headless"])).is_err());
        let token = "ab".repeat(32);
        assert!(
            parse_cli(args(&[
                "--resident",
                "--ui",
                "--session",
                "s",
                "--token",
                &token
            ]))
            .is_err()
        );
        assert!(parse_cli(args(&["--resident", "--headless", "emit-spec"])).is_err());
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
