//! 「前台全屏窗口停用热键」闸门：开关状态与热键拦截判定。
//!
//! 开关持久化在 `global_shortcuts/disable_on_focused_fullscreen_window`；打开后，只要前台是全屏窗口
//! （游戏、全屏视频等），来自全局热键的命令一律丢弃，两个开关热键本身除外，否则无法关回去。
//! 判定是纯函数，可离屏测试；前台全屏探测由调用方以闭包注入。

use serde_json::Value;
use snow_app_core::command::{AppCommand, CommandSource, QuickAction};
use snow_config::document::ConfigDocument;
use snow_ui::shell::dispatch::Dispatcher;
use std::sync::atomic::{AtomicBool, Ordering};

/// 开关状态的配置键（布尔）。
pub const DISABLE_ON_FULLSCREEN_CONFIG_KEY: &str =
    "global_shortcuts/disable_on_focused_fullscreen_window";

/// 开关的进程内镜像：热键线程读取，主线程在切换时写入。
static ENABLED: AtomicBool = AtomicBool::new(false);

/// 从配置文档读取开关状态。
///
/// # 参数
/// - `doc`：配置文档。
///
/// # 返回
/// 开关是否打开；键缺失或类型不对按关闭处理。
///
/// ```ignore
/// let doc = ConfigDocument::from_bytes(None);
/// assert!(!configured(&doc));
/// ```
pub fn configured(doc: &ConfigDocument) -> bool {
    doc.value(DISABLE_ON_FULLSCREEN_CONFIG_KEY)
        .as_bool()
        .unwrap_or(false)
}

/// 把开关状态同步到进程内镜像（启动与切换时调用）。
///
/// # 参数
/// - `enabled`：开关是否打开。
pub fn set_enabled(enabled: bool) {
    ENABLED.store(enabled, Ordering::Relaxed);
}

/// 进程内镜像当前是否打开。
pub fn is_enabled() -> bool {
    ENABLED.load(Ordering::Relaxed)
}

/// 配置值形式的开关状态，供写回配置。
///
/// # 参数
/// - `enabled`：开关是否打开。
pub fn config_value(enabled: bool) -> Value {
    Value::Bool(enabled)
}

/// 判断一条命令是否属于不受闸门约束的开关热键。
fn is_gate_toggle(cmd: &AppCommand) -> bool {
    matches!(
        cmd,
        AppCommand::QuickAction(
            QuickAction::ToggleGlobalHotkeys | QuickAction::ToggleDisableOnFocusedFullscreen
        )
    )
}

/// 判断是否应当丢弃这条命令。
///
/// # 参数
/// - `enabled`：开关是否打开。
/// - `source`：命令来源，只有热键受约束（托盘、IPC 不受影响）。
/// - `cmd`：命令。
/// - `fullscreen_focused`：前台是否为全屏窗口。
///
/// # 返回
/// `true` 表示丢弃。
///
/// ```ignore
/// assert!(should_drop(true, CommandSource::Hotkey, &AppCommand::OpenTranslateInput, true));
/// assert!(!should_drop(true, CommandSource::Tray, &AppCommand::OpenTranslateInput, true));
/// ```
pub fn should_drop(
    enabled: bool,
    source: CommandSource,
    cmd: &AppCommand,
    fullscreen_focused: bool,
) -> bool {
    enabled && source == CommandSource::Hotkey && !is_gate_toggle(cmd) && fullscreen_focused
}

/// 在热键派发器外包一层闸门。
///
/// # 参数
/// - `inner`：真正送进命令总线的派发器。
/// - `probe`：前台全屏探测；仅在开关打开且命令受约束时才调用，避免每次热键都查系统。
///
/// # 返回
/// 带闸门的派发器。
pub fn gate_dispatcher(
    inner: Dispatcher,
    probe: impl Fn() -> bool + Send + Sync + 'static,
) -> Dispatcher {
    Dispatcher::from_fn(move |source, cmd| {
        let candidate = is_enabled() && source == CommandSource::Hotkey && !is_gate_toggle(&cmd);
        if candidate && probe() {
            tracing::info!("前台为全屏窗口，已忽略全局热键");
            return;
        }
        inner.send(source, cmd);
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn drops_only_hotkey_commands_while_fullscreen() {
        let cmd = AppCommand::OpenTranslateInput;
        assert!(should_drop(true, CommandSource::Hotkey, &cmd, true));
        assert!(!should_drop(true, CommandSource::Hotkey, &cmd, false));
        assert!(!should_drop(false, CommandSource::Hotkey, &cmd, true));
        assert!(!should_drop(true, CommandSource::Tray, &cmd, true));
    }

    #[test]
    fn toggle_hotkeys_are_exempt() {
        for action in [
            QuickAction::ToggleGlobalHotkeys,
            QuickAction::ToggleDisableOnFocusedFullscreen,
        ] {
            let cmd = AppCommand::QuickAction(action);
            assert!(!should_drop(true, CommandSource::Hotkey, &cmd, true));
        }
    }

    #[test]
    fn configured_defaults_to_off_and_reads_value() {
        let mut doc = ConfigDocument::from_bytes(None);
        assert!(!configured(&doc));
        doc.set_value(DISABLE_ON_FULLSCREEN_CONFIG_KEY, config_value(true))
            .unwrap();
        assert!(configured(&doc));
    }

    #[test]
    fn gate_dispatcher_forwards_or_drops() {
        use std::sync::Arc;
        use std::sync::atomic::AtomicUsize;
        let seen = Arc::new(AtomicUsize::new(0));
        let counter = Arc::clone(&seen);
        let inner = Dispatcher::from_fn(move |_, _| {
            counter.fetch_add(1, Ordering::SeqCst);
        });
        let gated = gate_dispatcher(inner, || true);
        set_enabled(true);
        gated.send(CommandSource::Hotkey, AppCommand::OpenTranslateInput);
        assert_eq!(seen.load(Ordering::SeqCst), 0);
        gated.send(CommandSource::Tray, AppCommand::OpenTranslateInput);
        gated.send(
            CommandSource::Hotkey,
            AppCommand::QuickAction(QuickAction::ToggleGlobalHotkeys),
        );
        assert_eq!(seen.load(Ordering::SeqCst), 2);
        set_enabled(false);
        gated.send(CommandSource::Hotkey, AppCommand::OpenTranslateInput);
        assert_eq!(seen.load(Ordering::SeqCst), 3);
    }
}
