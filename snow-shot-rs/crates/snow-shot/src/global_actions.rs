//! 全局热键动作表：21 个 `global_shortcuts/*` 键 + Rust 新增键（输入翻译、听写）到命令的映射、
//! 热键闸门（整体开关 / 前台全屏抑制）与占位动作的提示文案。
//!
//! 默认值以 C++ schema 为准（`snow-config` 已对齐）：`screenshot=F1`、`screenshot_copy=Ctrl+F1`、
//! `pin_clipboard_content=F3`、`restore_last_closed_windows=Ctrl+F3`，其余默认不绑定。

use crate::app_runtime::{
    DICTATION_HOLD_HOTKEY_CONFIG_KEY, DICTATION_TOGGLE_HOTKEY_CONFIG_KEY,
    PIN_CLIPBOARD_HOTKEY_CONFIG_KEY, RECORDING_HOTKEY_CONFIG_KEY, SCREENSHOT_HOTKEY_CONFIG_KEY,
    TRANSLATE_INPUT_HOTKEY_CONFIG_KEY, shortcut_strings,
};
use crate::dictation::config::DictationConfig;
use crate::settings_model::portable_to_hotkey_text;
use crate::settings_text::{Lang, item_label};
use crate::ocr_backend::i18n_for;
use snow_app_core::command::{
    AppCommand, CaptureRequest, DirectCaptureRequest, DirectOutput, DirectTarget, GlobalAction,
    RecordingConfig,
};
use snow_config::document::ConfigDocument;
use snow_i18n::Args;
use snow_ui::shell::dispatch::Dispatcher;
use snow_ui::shell::hotkey::Hotkey;
use serde_json::Value;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

/// 单个动作最多生效的快捷键数（与 C++ `MAX_SHORTCUTS_PER_ACTION` 一致）。
pub const MAX_BINDINGS_PER_ACTION: usize = 2;
/// “前台全屏窗口时禁用热键”开关的配置键。
pub const FULLSCREEN_SUPPRESSION_KEY: &str = "global_shortcuts/disable_on_focused_fullscreen_window";
/// 全局鼠标手势配置分组前缀（未实现，设置页只标注）。
pub const GLOBAL_MOUSE_PREFIX: &str = "global_mouse/";
/// 延时截图的延时秒数配置键。
pub const DELAY_SECONDS_KEY: &str = "screenshot/delay_seconds";
/// 延时截图秒数下限。
pub const DELAY_MIN_SECONDS: i64 = 1;
/// 延时截图秒数上限。
pub const DELAY_MAX_SECONDS: i64 = 10;

/// 动作表的一行：配置键、日志标签、构造命令的函数。
type Row = (&'static str, &'static str, fn() -> AppCommand);

/// 构造“复制到剪贴板”的直接截图命令。
fn direct_copy(target: DirectTarget) -> AppCommand {
    AppCommand::DirectCapture(DirectCaptureRequest {
        target,
        output: DirectOutput::Copy,
        capture_cursor: None,
        scale: None,
        path: None,
        automatic_path: None,
        format: None,
        quality: None,
        compression_level: None,
        pdf_page_size: None,
        pdf_title: None,
    })
}

/// 静态动作表，顺序同 C++ `ALL_ACTIONS`（同一热键冲突时靠前者生效）。
const TABLE: &[Row] = &[
    (SCREENSHOT_HOTKEY_CONFIG_KEY, "screenshot", || AppCommand::Capture(CaptureRequest::default())),
    ("global_shortcuts/screenshot_delay", "screenshot_delay", || AppCommand::Global(GlobalAction::DelayedCapture)),
    ("global_shortcuts/screenshot_fixed", "screenshot_fixed", || AppCommand::Global(GlobalAction::CaptureAndPin)),
    ("global_shortcuts/screenshot_ocr", "screenshot_ocr", || AppCommand::Global(GlobalAction::CaptureAndOcr)),
    ("global_shortcuts/screenshot_translation", "screenshot_translation", || AppCommand::Global(GlobalAction::CaptureAndTranslate)),
    ("global_shortcuts/screenshot_copy", "screenshot_copy", || AppCommand::Global(GlobalAction::CaptureAndCopy)),
    ("global_shortcuts/screenshot_full_screen", "screenshot_full_screen", || direct_copy(DirectTarget::CurrentMonitor)),
    ("global_shortcuts/screenshot_focused_window", "screenshot_focused_window", || direct_copy(DirectTarget::FocusedWindow)),
    (RECORDING_HOTKEY_CONFIG_KEY, "recording", || AppCommand::StartRecording(RecordingConfig::default())),
    ("global_shortcuts/screen_record_copy", "screen_record_copy", || AppCommand::Global(GlobalAction::RecordAndCopy)),
    ("global_shortcuts/open_screen_recording_folder", "open_recording_folder", || AppCommand::Global(GlobalAction::OpenRecordingFolder)),
    ("global_shortcuts/open_capture_history", "open_capture_history", || AppCommand::Global(GlobalAction::OpenCaptureHistory)),
    ("global_shortcuts/open_pin_to_screen_management", "open_pin_management", || AppCommand::Global(GlobalAction::OpenPinManagement)),
    ("global_shortcuts/open_settings", "open_settings", || AppCommand::Global(GlobalAction::OpenSettings)),
    (PIN_CLIPBOARD_HOTKEY_CONFIG_KEY, "pin_clipboard", || AppCommand::PinSelection),
    ("global_shortcuts/translate_selected_text", "translate_selected_text", || AppCommand::Global(GlobalAction::TranslateSelectedText)),
    ("global_shortcuts/pin_selected_files", "pin_selected_files", || AppCommand::Global(GlobalAction::PinSelectedFiles)),
    ("global_shortcuts/restore_last_closed_windows", "restore_closed_pin", || AppCommand::Global(GlobalAction::RestoreClosedPin)),
    ("global_shortcuts/toggle_global_hotkeys", "toggle_global_hotkeys", || AppCommand::Global(GlobalAction::ToggleGlobalHotkeys)),
    ("global_shortcuts/toggle_disable_on_focused_fullscreen_window", "toggle_fullscreen_suppression", || AppCommand::Global(GlobalAction::ToggleFullscreenSuppression)),
    (TRANSLATE_INPUT_HOTKEY_CONFIG_KEY, "translate_input", || AppCommand::OpenTranslateInput),
];

/// 一条待注册的热键动作。
#[derive(Debug, Clone, PartialEq)]
pub struct HotkeyEntry {
    /// 热键所属配置键。
    pub config_key: &'static str,
    /// 日志里的功能名。
    pub label: &'static str,
    /// 按下触发的命令。
    pub command: AppCommand,
    /// 松开触发的命令（按住说话）。
    pub on_release: Option<AppCommand>,
}

/// 按配置生成全部热键动作（听写两条按触发模式取舍）。
///
/// # 参数
/// - `document`：配置文档。
///
/// ```ignore
/// let entries = hotkey_entries(&document);
/// ```
pub fn hotkey_entries(document: &ConfigDocument) -> Vec<HotkeyEntry> {
    let mut list: Vec<HotkeyEntry> = TABLE
        .iter()
        .map(|(key, label, make)| HotkeyEntry {
            config_key: key,
            label,
            command: make(),
            on_release: None,
        })
        .collect();
    let trigger = DictationConfig::from_document(document).trigger;
    if trigger.toggle_enabled() {
        list.push(HotkeyEntry {
            config_key: DICTATION_TOGGLE_HOTKEY_CONFIG_KEY,
            label: "dictation_toggle",
            command: AppCommand::ToggleDictation,
            on_release: None,
        });
    }
    if trigger.hold_enabled() {
        list.push(HotkeyEntry {
            config_key: DICTATION_HOLD_HOTKEY_CONFIG_KEY,
            label: "dictation_hold",
            command: AppCommand::StartDictation,
            on_release: Some(AppCommand::StopDictation),
        });
    }
    list
}

/// 是否是会触发热键重新注册的配置键（所有热键键 + 听写触发模式）。
///
/// # 参数
/// - `key`：配置键。
pub fn is_hotkey_config_key(key: &str) -> bool {
    static_hotkey_key(key).is_some()
}

/// 取热键相关配置键的 `'static` 版本（回滚提示需要）；不是热键键返回 `None`。
///
/// # 参数
/// - `key`：配置键。
pub fn static_hotkey_key(key: &str) -> Option<&'static str> {
    let extra = [
        DICTATION_TOGGLE_HOTKEY_CONFIG_KEY,
        DICTATION_HOLD_HOTKEY_CONFIG_KEY,
        crate::app_runtime::DICTATION_TRIGGER_MODE_CONFIG_KEY,
    ];
    TABLE
        .iter()
        .map(|(k, _, _)| *k)
        .chain(extra)
        .find(|k| *k == key)
}

/// 取某个动作的有效快捷键文本：去重（按解析后相等）并截到 [`MAX_BINDINGS_PER_ACTION`] 个；
/// 无法解析的保留，交给注册流程报错。
///
/// # 参数
/// - `value`：配置值。
///
/// ```ignore
/// assert_eq!(effective_shortcuts(&json!(["F1", "f1", "F2", "F3"])), vec!["F1", "F2"]);
/// ```
pub fn effective_shortcuts(value: &Value) -> Vec<String> {
    let mut seen: Vec<Hotkey> = Vec::new();
    let mut out = Vec::new();
    for text in shortcut_strings(value) {
        if out.len() >= MAX_BINDINGS_PER_ACTION {
            break;
        }
        if let Ok(hotkey) = Hotkey::parse(&portable_to_hotkey_text(&text)) {
            if seen.contains(&hotkey) {
                continue;
            }
            seen.push(hotkey);
        }
        out.push(text);
    }
    out
}

/// 动作是否仍是占位（功能未实现或只实现了一部分）。
///
/// # 参数
/// - `action`：全局动作。
pub const fn is_placeholder(action: GlobalAction) -> bool {
    matches!(
        action,
        GlobalAction::RecordAndCopy
            | GlobalAction::OpenCaptureHistory
            | GlobalAction::OpenPinManagement
            | GlobalAction::TranslateSelectedText
            | GlobalAction::PinSelectedFiles
            | GlobalAction::RestoreClosedPin
    )
}

/// 动作对应的配置键（用来取设置页里的动作名）。
///
/// # 参数
/// - `action`：全局动作。
pub fn config_key_of(action: GlobalAction) -> Option<&'static str> {
    let wanted = AppCommand::Global(action);
    TABLE.iter().find(|(_, _, make)| make() == wanted).map(|(k, _, _)| *k)
}

/// 占位动作触发时的提示文字（带动作名）。
///
/// # 参数
/// - `action`：全局动作。
/// - `locale`：界面语言代码。
pub fn placeholder_hint(action: GlobalAction, locale: &str) -> String {
    let name = config_key_of(action)
        .map(|key| item_label(Lang::new(locale), key))
        .unwrap_or_default();
    i18n_for(locale).tr_with("global-hotkey-not-implemented", &Args::new().named("action", name))
}

/// 设置页行内说明：占位热键与全局鼠标手势键各有一句“未实现”提示；其它键为 `None`。
///
/// # 参数
/// - `key`：配置键。
/// - `locale`：界面语言代码。
pub fn placeholder_note(key: &str, locale: &str) -> Option<String> {
    let i18n = i18n_for(locale);
    if key.starts_with(GLOBAL_MOUSE_PREFIX) {
        return Some(i18n.tr("global-mouse-not-implemented-note"));
    }
    let wired_placeholder = TABLE.iter().any(|(k, _, make)| {
        *k == key && matches!(make(), AppCommand::Global(a) if is_placeholder(a))
    });
    wired_placeholder.then(|| i18n.tr("global-hotkey-placeholder-note"))
}

/// 延时截图秒数：读配置并夹到 1..=10。
///
/// # 参数
/// - `document`：配置文档。
pub fn delay_seconds(document: &ConfigDocument) -> u64 {
    let raw = document.value(DELAY_SECONDS_KEY).as_i64().unwrap_or(DELAY_MIN_SECONDS);
    raw.clamp(DELAY_MIN_SECONDS, DELAY_MAX_SECONDS) as u64
}

/// 热键闸门：整体开关（本次运行有效）与“前台全屏时抑制”，可跨线程共享。
#[derive(Clone)]
pub struct HotkeyGate {
    /// 全局热键是否开启。
    enabled: Arc<AtomicBool>,
    /// 前台全屏窗口时是否抑制热键。
    suppress_fullscreen: Arc<AtomicBool>,
}

impl Default for HotkeyGate {
    /// 默认开启、不抑制。
    fn default() -> Self {
        Self {
            enabled: Arc::new(AtomicBool::new(true)),
            suppress_fullscreen: Arc::new(AtomicBool::new(false)),
        }
    }
}

impl HotkeyGate {
    /// 当前是否开启。
    pub fn enabled(&self) -> bool {
        self.enabled.load(Ordering::SeqCst)
    }

    /// 翻转整体开关。
    ///
    /// # 返回
    /// 翻转后的状态。
    pub fn toggle_enabled(&self) -> bool {
        !self.enabled.fetch_xor(true, Ordering::SeqCst)
    }

    /// 是否在前台全屏时抑制热键。
    pub fn suppress_fullscreen(&self) -> bool {
        self.suppress_fullscreen.load(Ordering::SeqCst)
    }

    /// 设置“前台全屏时抑制”。
    ///
    /// # 参数
    /// - `on`：是否抑制。
    pub fn set_suppress_fullscreen(&self, on: bool) {
        self.suppress_fullscreen.store(on, Ordering::SeqCst);
    }

    /// 命令此刻能否放行：闸门控制动作与“结束听写”总是放行，其余看开关与全屏抑制。
    ///
    /// # 参数
    /// - `command`：热键触发的命令。
    /// - `fullscreen`：探测前台是否全屏（仅在需要时调用）。
    ///
    /// ```ignore
    /// let gate = HotkeyGate::default();
    /// assert!(gate.allows(&AppCommand::OpenTranslateInput, || false));
    /// ```
    pub fn allows(&self, command: &AppCommand, fullscreen: impl Fn() -> bool) -> bool {
        match command {
            AppCommand::Global(action) if action.controls_gate() => true,
            AppCommand::StopDictation => true,
            _ => self.enabled() && !(self.suppress_fullscreen() && fullscreen()),
        }
    }
}

/// 给热键出口套上闸门：被拦下的命令只记调试日志。
///
/// # 参数
/// - `inner`：真正的出口（通常 `Dispatcher::from_bus`）。
/// - `gate`：闸门。
/// - `fullscreen`：前台全屏探测函数。
pub fn gated_dispatcher(inner: Dispatcher, gate: HotkeyGate, fullscreen: fn() -> bool) -> Dispatcher {
    Dispatcher::from_fn(move |source, command| {
        if gate.allows(&command, fullscreen) {
            inner.send(source, command);
        } else {
            tracing::debug!(?command, "全局热键被闸门拦下");
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use snow_app_core::command::CommandSource;
    use std::sync::Mutex;

    /// 21 个原有键全部在表里，且与 C++ 默认值一致（只有四个默认绑定）。
    #[test]
    fn table_covers_cpp_keys_and_defaults() {
        let doc = ConfigDocument::from_bytes(None);
        let entries = hotkey_entries(&doc);
        let keys: Vec<_> = entries.iter().map(|e| e.config_key).collect();
        assert_eq!(TABLE.len(), 21);
        for key in [
            "global_shortcuts/open_settings",
            "global_shortcuts/toggle_disable_on_focused_fullscreen_window",
            "global_shortcuts/pin_selected_files",
        ] {
            assert!(keys.contains(&key), "{key}");
        }
        let bound: Vec<_> = entries
            .iter()
            .filter(|e| !shortcut_strings(&doc.value(e.config_key)).is_empty())
            .map(|e| (e.config_key, shortcut_strings(&doc.value(e.config_key))))
            .collect();
        assert_eq!(
            bound,
            vec![
                ("global_shortcuts/screenshot", vec!["F1".to_string()]),
                ("global_shortcuts/screenshot_copy", vec!["Ctrl+F1".to_string()]),
                ("global_shortcuts/pin_clipboard_content", vec!["F3".to_string()]),
                ("global_shortcuts/restore_last_closed_windows", vec!["Ctrl+F3".to_string()]),
            ]
        );
        assert_eq!(doc.value(FULLSCREEN_SUPPRESSION_KEY), Value::Bool(false));
        for e in &entries {
            for s in shortcut_strings(&doc.value(e.config_key)) {
                assert!(Hotkey::parse(&portable_to_hotkey_text(&s)).is_ok(), "{s}");
            }
        }
    }

    /// 旧配置缺键时补默认；多余与重复的快捷键被截掉。
    #[test]
    fn missing_keys_default_and_dedupe() {
        let doc = ConfigDocument::from_bytes(Some(br#"{"global_shortcuts":{"screenshot":["F2"]}}"#));
        assert_eq!(shortcut_strings(&doc.value("global_shortcuts/screenshot")), vec!["F2"]);
        assert_eq!(shortcut_strings(&doc.value("global_shortcuts/pin_clipboard_content")), vec!["F3"]);
        let v = serde_json::json!(["Ctrl+Alt+A", "alt+ctrl+a", "F2", "F3"]);
        assert_eq!(effective_shortcuts(&v), vec!["Ctrl+Alt+A", "F2"]);
    }

    /// 每个全局动作都能反查到配置键；占位集合与提示文案两种语言都有内容且带动作名。
    #[test]
    fn placeholder_hints_localized() {
        for action in [
            GlobalAction::OpenCaptureHistory,
            GlobalAction::TranslateSelectedText,
            GlobalAction::RestoreClosedPin,
        ] {
            assert!(is_placeholder(action));
            assert!(config_key_of(action).is_some());
            for locale in ["en-US", "zh-CN"] {
                let text = placeholder_hint(action, locale);
                assert!(!text.is_empty() && !text.starts_with("global-hotkey"), "{text}");
            }
        }
        assert!(placeholder_hint(GlobalAction::PinSelectedFiles, "zh-CN").contains("尚未实现"));
        assert!(!is_placeholder(GlobalAction::OpenSettings));
        assert!(placeholder_note("global_mouse/screenshot_copy", "zh-CN").is_some());
        assert!(placeholder_note("global_shortcuts/pin_selected_files", "en-US").is_some());
        assert!(placeholder_note("global_shortcuts/screenshot", "en-US").is_none());
    }

    /// 延时秒数夹在 1..=10，缺省取下限。
    #[test]
    fn delay_is_clamped() {
        let mut doc = ConfigDocument::from_bytes(None);
        doc.set_value(DELAY_SECONDS_KEY, serde_json::json!(99)).unwrap();
        assert_eq!(delay_seconds(&doc), 10);
        doc.set_value(DELAY_SECONDS_KEY, serde_json::json!(0)).unwrap();
        assert_eq!(delay_seconds(&doc), 1);
    }

    /// 闸门：关闭时只放行闸门控制动作与结束听写；全屏抑制按探测结果。
    #[test]
    fn gate_rules() {
        let gate = HotkeyGate::default();
        let capture = AppCommand::Capture(CaptureRequest::default());
        assert!(gate.allows(&capture, || true));
        gate.set_suppress_fullscreen(true);
        assert!(!gate.allows(&capture, || true));
        assert!(gate.allows(&capture, || false));
        assert!(gate.allows(&AppCommand::Global(GlobalAction::ToggleFullscreenSuppression), || true));
        assert!(!gate.toggle_enabled());
        assert!(!gate.allows(&capture, || false));
        assert!(gate.allows(&AppCommand::Global(GlobalAction::ToggleGlobalHotkeys), || false));
        assert!(gate.allows(&AppCommand::StopDictation, || false));
        assert!(gate.toggle_enabled());
    }

    /// 闸门出口：被拦的不转发，放行的带来源转发。
    #[test]
    fn gated_dispatcher_forwards_only_allowed() {
        let seen = Arc::new(Mutex::new(Vec::new()));
        let sink = Arc::clone(&seen);
        let inner = Dispatcher::from_fn(move |src, cmd| sink.lock().unwrap().push((src, cmd)));
        let gate = HotkeyGate::default();
        let d = gated_dispatcher(inner, gate.clone(), || false);
        d.send(CommandSource::Hotkey, AppCommand::OpenTranslateInput);
        gate.toggle_enabled();
        d.send(CommandSource::Hotkey, AppCommand::OpenTranslateInput);
        d.send(CommandSource::Hotkey, AppCommand::Global(GlobalAction::ToggleGlobalHotkeys));
        let got = seen.lock().unwrap();
        assert_eq!(got.len(), 2);
        assert_eq!(got[0].1, AppCommand::OpenTranslateInput);
    }

    /// 听写热键按触发模式取舍，按住说话带松开命令。
    #[test]
    fn dictation_entries_follow_trigger_mode() {
        let doc = ConfigDocument::from_bytes(None);
        let entries = hotkey_entries(&doc);
        let hold = entries.iter().find(|e| e.label == "dictation_hold");
        assert_eq!(hold.and_then(|e| e.on_release.clone()), Some(AppCommand::StopDictation));
        assert!(is_hotkey_config_key("global_shortcuts/open_settings"));
        assert!(is_hotkey_config_key(DICTATION_HOLD_HOTKEY_CONFIG_KEY));
        assert!(!is_hotkey_config_key(FULLSCREEN_SUPPRESSION_KEY));
    }
}
