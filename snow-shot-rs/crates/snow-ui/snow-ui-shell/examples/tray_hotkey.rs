//! 托盘 + 全局热键验证（不依赖 gpui 窗口）。
//!
//! 两种模式：
//! - `cargo run -p snow-ui-shell --example tray_hotkey -- --selfcheck`：**自动**自检，
//!   不需要任何人工操作，数秒内退出。检查服务能启动、热键注册/重复冲突/注销、托盘能创建。
//! - `cargo run -p snow-ui-shell --example tray_hotkey`：**手动**验证（真人操作，程序判定）。
//!
//! 手动步骤：
//! 1. 启动后托盘区出现蓝色方块图标（可能在“隐藏图标”折叠里）；
//! 2. 按 `Ctrl+Alt+Shift+F9`（命令 Cancel，来源 Hotkey）至少 1 次；
//! 3. 右键托盘图标，点“触发 Undo”（命令 Undo，来源 Tray）至少 1 次；
//! 4. 右键托盘图标点“退出”结束（60 秒无操作自动退出）。
//!
//! 判定：总线 handler 收到的 `(来源, 命令)` 写入 `%TEMP%\snow-shell-tray-hotkey.log`，
//! 通过条件为 Hotkey/Cancel 与 Tray/Undo 各 >=1 次，末行 `VERDICT PASS|FAIL`。
//! 禁止用脚本模拟按键；自检模式不触发热键，只验证注册链路。

use snow_app_core::bus::{CommandBus, CommandOutcome};
use snow_app_core::command::{AppCommand, CommandKind, CommandSource};
use snow_capability::CapabilityRegistry;
use snow_ui_shell::dispatch::Dispatcher;
use snow_ui_shell::error::ShellError;
use snow_ui_shell::hotkey::{Hotkey, HotkeyBinding, HotkeyService};
use snow_ui_shell::tray::{TrayAction, TrayIconImage, TrayMenuEntry, TrayService, TraySpec};
use std::io::Write;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// 演示热键。
const DEMO_HOTKEY: &str = "Ctrl+Alt+Shift+F9";
/// 手动模式无操作自动退出秒数。
const AUTO_QUIT_SECS: u64 = 60;
/// 托盘“退出”菜单发出的信号名。
const SIGNAL_QUIT: &str = "quit";

/// 写日志并打印。
fn log(path: &std::path::Path, line: &str) {
    println!("{line}");
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
    {
        let _ = writeln!(f, "{line}");
    }
}

/// 构造演示托盘描述。
fn demo_tray_spec() -> Result<TraySpec, ShellError> {
    Ok(TraySpec {
        tooltip: "snow-ui-shell 验证".into(),
        icon: TrayIconImage::solid(32, 32, [0, 90, 255, 255])?,
        menu: vec![
            TrayMenuEntry::Item {
                label: "触发 Undo".into(),
                enabled: true,
                action: TrayAction::Command(AppCommand::Undo(Default::default())),
            },
            TrayMenuEntry::Separator,
            TrayMenuEntry::Item {
                label: "退出".into(),
                enabled: true,
                action: TrayAction::Signal(SIGNAL_QUIT.into()),
            },
        ],
        on_left_click: None,
        on_double_click: None,
    })
}

/// 自动自检：返回是否全部通过。
fn selfcheck(caps: &CapabilityRegistry) -> bool {
    let mut ok = true;
    let mut check = |cond: bool, what: &str| {
        println!("[{}] {what}", if cond { "ok  " } else { "FAIL" });
        ok &= cond;
    };
    let dispatcher = Dispatcher::from_fn(|_, _| {});
    // 自检用不常见组合，避免撞上用户已有热键
    let binding = |s: &str| {
        HotkeyBinding::new(
            Hotkey::parse(s).expect("自检热键应可解析"),
            AppCommand::Cancel(Default::default()),
        )
    };
    match HotkeyService::start(caps, dispatcher.clone()) {
        Ok(svc) => {
            check(true, "HotkeyService 启动");
            check(
                matches!(
                    HotkeyService::start(caps, dispatcher.clone()),
                    Err(ShellError::AlreadyRunning(_))
                ),
                "重复启动被拒绝（进程内单例）",
            );
            let h = svc.register(binding("Ctrl+Alt+Shift+F23"));
            check(h.is_ok(), &format!("注册 Ctrl+Alt+Shift+F23: {h:?}"));
            let dup = svc.register(binding("alt+ctrl+shift+f23"));
            check(
                matches!(dup, Err(ShellError::HotkeyConflict(_))),
                &format!("等价写法重复注册被判冲突: {dup:?}"),
            );
            // Win+L 由系统保留，注册应失败并且不 panic
            let os = svc.register(binding("Win+L"));
            check(os.is_err(), &format!("系统保留热键 Win+L 注册失败: {os:?}"));
            check(
                svc.registered().map(|v| v.len()).ok() == Some(1),
                "登记表仅含 1 项",
            );
            if let Ok(h) = h {
                check(svc.unregister(h).is_ok(), "注销成功");
                check(svc.unregister(h).is_err(), "重复注销返回错误");
                check(
                    svc.register(binding("Ctrl+Alt+Shift+F23")).is_ok(),
                    "注销后可重新注册",
                );
            }
        }
        Err(e) => check(false, &format!("HotkeyService 启动: {e}")),
    }
    // 服务已 drop，单例标记应释放
    check(
        HotkeyService::start(caps, dispatcher.clone()).is_ok(),
        "drop 后可重新启动热键服务",
    );
    match demo_tray_spec().and_then(|spec| TrayService::start(caps, spec, dispatcher)) {
        Ok(tray) => {
            check(true, "TrayService 创建");
            check(tray.set_tooltip("自检中").is_ok(), "更新提示");
            std::thread::sleep(Duration::from_millis(300));
        }
        Err(e) => check(false, &format!("TrayService 创建: {e}")),
    }
    ok
}

/// 手动验证：返回是否通过。
fn manual(caps: &CapabilityRegistry) -> bool {
    let path = std::env::temp_dir().join("snow-shell-tray-hotkey.log");
    let _ = std::fs::remove_file(&path);
    let hotkey_hits = Arc::new(AtomicUsize::new(0));
    let tray_hits = Arc::new(AtomicUsize::new(0));
    let bus = CommandBus::new();
    let seen = Arc::new(Mutex::new(path.clone()));
    for kind in [CommandKind::Cancel, CommandKind::Undo] {
        let (seen, hk, tr) = (seen.clone(), hotkey_hits.clone(), tray_hits.clone());
        bus.register(
            kind,
            Arc::new(move |ctx, cmd| {
                if let Ok(p) = seen.lock() {
                    log(
                        &p,
                        &format!("COMMAND source={:?} kind={:?}", ctx.source, cmd.kind()),
                    );
                }
                match (ctx.source, cmd.kind()) {
                    (CommandSource::Hotkey, CommandKind::Cancel) => {
                        hk.fetch_add(1, Ordering::SeqCst)
                    }
                    (CommandSource::Tray, CommandKind::Undo) => tr.fetch_add(1, Ordering::SeqCst),
                    _ => 0,
                };
                Ok(CommandOutcome::Done)
            }),
        );
    }
    let dispatcher = Dispatcher::from_bus(bus);
    let hotkeys = match HotkeyService::start(caps, dispatcher.clone()) {
        Ok(s) => s,
        Err(e) => {
            log(&path, &format!("热键服务启动失败（降级）: {e}"));
            return false;
        }
    };
    let binding = HotkeyBinding::new(
        Hotkey::parse(DEMO_HOTKEY).expect("演示热键可解析"),
        AppCommand::Cancel(Default::default()),
    );
    if let Err(e) = hotkeys.register(binding) {
        log(&path, &format!("注册 {DEMO_HOTKEY} 失败: {e}"));
        return false;
    }
    let tray = match demo_tray_spec().and_then(|s| TrayService::start(caps, s, dispatcher)) {
        Ok(t) => t,
        Err(e) => {
            log(&path, &format!("托盘创建失败（降级）: {e}"));
            return false;
        }
    };
    log(
        &path,
        &format!("READY 热键 {DEMO_HOTKEY} 已注册，托盘已创建；请按步骤操作"),
    );
    let deadline = Instant::now() + Duration::from_secs(AUTO_QUIT_SECS);
    while Instant::now() < deadline {
        if let Ok(sig) = tray.signals().recv_timeout(Duration::from_millis(200))
            && sig == SIGNAL_QUIT
        {
            log(&path, "SIGNAL quit");
            break;
        }
    }
    let (hk, tr) = (
        hotkey_hits.load(Ordering::SeqCst),
        tray_hits.load(Ordering::SeqCst),
    );
    // 给派发线程一点时间落日志
    std::thread::sleep(Duration::from_millis(200));
    let pass = hk >= 1 && tr >= 1;
    log(&path, &format!("SUMMARY hotkey_cancel={hk} tray_undo={tr}"));
    log(&path, if pass { "VERDICT PASS" } else { "VERDICT FAIL" });
    pass
}

fn main() {
    let caps = CapabilityRegistry::for_current_platform();
    let selfcheck_mode = std::env::args().any(|a| a == "--selfcheck");
    let ok = if selfcheck_mode {
        selfcheck(&caps)
    } else {
        manual(&caps)
    };
    if selfcheck_mode {
        println!(
            "{}",
            if ok {
                "SELFCHECK PASS"
            } else {
                "SELFCHECK FAIL"
            }
        );
    }
    std::process::exit(if ok { 0 } else { 1 });
}
