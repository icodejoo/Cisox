//! 宿主进程的入口流程（Windows）：命令行、单实例、日志与崩溃转储、主事件循环。
//!
//! 主线程不泵 Win32 消息：托盘与热键各有自己的后台线程，管道读线程、进程等待线程也都只往
//! 同一个通道投事件；主线程 `recv_timeout(下一个截止时刻)`，没有待办就无限期阻塞（不轮询）。

use crate::cli::{FLAG_COMMAND, HostCli, USAGE, parse_cli};
use crate::harden::{HardenReport, extension_points_disabled};
use crate::host::{Event, HostCore};
use crate::ipc_map::{FirstRun, action_id_for, first_run_plan};
use crate::mailbox::Mailbox;
use crate::win::{ENV_UI_EXE, WinPlatform};
use snow_data_paths::paths::{default_app_data_directory, resolve_directory};
use snow_data_paths::{
    CRASH_DIR_NAME, LOG_DIR_NAME, PRODUCT_NAME, SINGLE_INSTANCE_APP_ID, host_log_path,
};
use snow_host_protocol::launch::{FLAG_RESIDENT, UI_EXE_NAME};
use snow_host_protocol::message::ActionOrigin;
use snow_platform::crash::{
    CrashDumpConfig, DEFAULT_MAX_REPORTS, install as install_crash_handler,
};
use snow_platform::single_instance::{IpcCommand, SingleInstanceManager, SingleInstanceStatus};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};
use std::sync::mpsc::RecvTimeoutError;
use std::time::Instant;

/// 命令行参数非法时的退出码。
const EXIT_CODE_BAD_ARGS: u8 = 2;

/// 从属实例发送失败 / 回退拉起失败时的退出码。
const EXIT_CODE_FAILED: u8 = 1;

/// 宿主进程入口。
///
/// # 参数
/// - `args`：命令行参数（不含程序名）。
/// - `harden`：进程加固结果（`main` 第一行调用 [`harden_process`](crate::harden::harden_process) 得到）。
///
/// # 返回
/// 进程退出码。
pub fn run(args: Vec<String>, harden: HardenReport) -> ExitCode {
    // GUI 子系统没有控制台：带命令行参数时附着父控制台，让用法提示和错误输出可见
    if !cfg!(debug_assertions) && !args.is_empty() {
        snow_platform::console::attach_parent_console();
    }
    let cli = match parse_cli(args) {
        Ok(cli) => cli,
        Err(e) => {
            eprintln!("{e}\n{USAGE}");
            return ExitCode::from(EXIT_CODE_BAD_ARGS);
        }
    };
    match cli {
        HostCli::RestartAfter(pid) => match crate::win::restart_after(pid) {
            Ok(()) => ExitCode::SUCCESS,
            Err(error) => {
                eprintln!("{error}");
                ExitCode::from(EXIT_CODE_FAILED)
            }
        },
        HostCli::SelfCheckHarden => run_self_check(harden),
        HostCli::Resident => run_resident_fallback(),
        HostCli::Run { command } => run_host(command, harden),
    }
}

/// 自检：加固是否真的生效（离屏测试用，不创建窗口 / 线程）。
fn run_self_check(harden: HardenReport) -> ExitCode {
    let policy_ok = extension_points_disabled();
    println!(
        "extension_points_disabled={policy_ok} ime_disabled={} dpi_aware={}",
        harden.ime_disabled, harden.dpi_aware
    );
    if policy_ok && harden.extension_points_disabled && harden.ime_disabled {
        ExitCode::SUCCESS
    } else {
        ExitCode::from(EXIT_CODE_FAILED)
    }
}

/// UI 进程 exe 路径：环境变量覆盖，否则是本 exe 同目录的 `cisox-ui.exe`。
fn ui_exe_path() -> Option<PathBuf> {
    std::env::var_os(ENV_UI_EXE)
        .map(PathBuf::from)
        .or_else(|| Some(std::env::current_exe().ok()?.with_file_name(UI_EXE_NAME)))
}

/// `--resident`：不跑宿主，改拉起旧的单进程 UI（回退用）。
fn run_resident_fallback() -> ExitCode {
    let Some(exe) = ui_exe_path() else {
        eprintln!("取不到自身路径，无法定位 {UI_EXE_NAME}");
        return ExitCode::from(EXIT_CODE_FAILED);
    };
    match Command::new(&exe).arg(FLAG_RESIDENT).spawn() {
        Ok(_) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("拉起 {} 失败: {e}", exe.display());
            ExitCode::from(EXIT_CODE_FAILED)
        }
    }
}

/// 数据根目录：与 UI 进程用同一套解析（便携标记、AppData 目录、upstream 目录保护）。
fn resolve_data_root() -> PathBuf {
    let exe_dir = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(Path::to_path_buf))
        .unwrap_or_else(|| PathBuf::from("."));
    let app_data_dir = default_app_data_directory().unwrap_or_else(|| exe_dir.join("data"));
    let storage = resolve_directory(&exe_dir, &app_data_dir);
    storage.effective_directory.unwrap_or(exe_dir)
}

/// 宿主主流程：抢单实例；抢不到就当从属实例把命令（没给就唤醒）交给运行中的实例；
/// 抢到了就常驻，并在起来后执行命令行里显式给出的 `--cmd`（`quit` 除外：没有实例可退）。
fn run_host(command: Option<IpcCommand>, harden: HardenReport) -> ExitCode {
    let guard = match SingleInstanceManager::acquire(SINGLE_INSTANCE_APP_ID) {
        SingleInstanceStatus::Primary(guard) => guard,
        SingleInstanceStatus::Secondary => {
            return run_secondary(&command.unwrap_or(IpcCommand::ShowMainWindow));
        }
    };
    let first_run = first_run_plan(command.as_ref());
    if first_run == FirstRun::Quit {
        return ExitCode::SUCCESS;
    }
    let data_root = resolve_data_root();
    let logging = crate::host_log::init(&host_log_path(&data_root));
    let _crash_guard = install_crash_handler(CrashDumpConfig {
        crash_dir: data_root.join(LOG_DIR_NAME).join(CRASH_DIR_NAME),
        app_name: format!("{PRODUCT_NAME} host"),
        app_version: env!("CARGO_PKG_VERSION").to_string(),
        max_reports: DEFAULT_MAX_REPORTS,
    })
    .ok();
    tracing::info!(
        version = env!("CARGO_PKG_VERSION"),
        data_root = %data_root.display(),
        logging,
        extension_points_disabled = harden.extension_points_disabled,
        ime_disabled = harden.ime_disabled,
        dpi_aware = harden.dpi_aware,
        "托盘宿主启动"
    );
    let tx = Mailbox::<Event>::new();
    let rx = tx.clone();
    let ipc_tx = tx.clone();
    let startup_tx = tx.clone();
    if let Err(e) =
        SingleInstanceManager::start_listener(&guard, move |cmd| match action_id_for(&cmd) {
            Some(id) => {
                let _ = ipc_tx.send(Event::Action {
                    id: id.to_string(),
                    origin: ActionOrigin::Ipc,
                });
            }
            None => tracing::warn!(?cmd, "命令没有对应动作，已忽略"),
        })
    {
        // 不静默：没有控制管道时第二实例无法唤醒本实例，但宿主仍可正常运行
        tracing::error!(error = %e, "单实例控制管道不可用，--cmd 将无法唤醒本实例");
    }
    let host_exe = std::env::current_exe().unwrap_or_else(|_| PathBuf::from(UI_EXE_NAME));
    let mut core = HostCore::new(WinPlatform::new(tx, data_root, host_exe));
    core.start(Instant::now());
    if let FirstRun::Action(id) = first_run {
        let _ = startup_tx.send(Event::Action {
            id: id.to_string(),
            origin: ActionOrigin::Ipc,
        });
    }
    event_loop(&mut core, &rx);
    tracing::info!("托盘宿主退出");
    core.finish();
    ExitCode::SUCCESS
}

/// 主事件循环：处理到点的事，然后等下一个事件或下一个截止时刻；宿主该退出时返回。
fn event_loop(core: &mut HostCore<WinPlatform>, rx: &Mailbox<Event>) {
    loop {
        let now = Instant::now();
        core.tick(now);
        if core.exit_requested().is_some() {
            return;
        }
        let received = match core.next_deadline() {
            Some(deadline) => rx.recv_timeout(deadline.saturating_duration_since(now)),
            None => rx.recv(),
        };
        match received {
            Ok(event) => core.handle(Instant::now(), event),
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => return,
        }
    }
}

/// 从属实例：把命令交给运行中的实例，返回进程退出码。
fn run_secondary(command: &IpcCommand) -> ExitCode {
    match SingleInstanceManager::send_command_to_primary(SINGLE_INSTANCE_APP_ID, command) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("无法联系运行中的实例: {e}（用法见 {FLAG_COMMAND}）");
            ExitCode::from(EXIT_CODE_FAILED)
        }
    }
}
