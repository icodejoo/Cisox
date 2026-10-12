//! 全局热键的“计划”：由配置算出要注册哪些热键，注册与序列化是另一步。
//!
//! 单进程模式里，`app_runtime` 按计划逐条注册到 `HotkeyService`；双进程模式里，同一份计划
//! 序列化进 `host/spec.json`（见 [`crate::host_spec`]）交给宿主注册。计划本身是纯函数，
//! 不接触系统调用，可离屏测试，两条路径的分叉点只有“注册还是序列化”（方案 §9 回退）。

use crate::app_runtime::{
    DICTATION_HOLD_HOTKEY_CONFIG_KEY, DICTATION_TOGGLE_HOTKEY_CONFIG_KEY,
    PIN_CLIPBOARD_HOTKEY_CONFIG_KEY, RECORDING_HOTKEY_CONFIG_KEY, SCREENSHOT_HOTKEY_CONFIG_KEY,
    TRANSLATE_INPUT_HOTKEY_CONFIG_KEY, shortcut_strings,
};
use crate::dictation::config::DictationConfig;
use crate::quick_actions::{QUICK_ACTION_KEYS, stays_registered_when_paused};
use crate::settings_model::portable_to_hotkey_text;
use snow_app_core::command::{AppCommand, CaptureRequest, RecordingConfig};
use snow_config::document::ConfigDocument;
use snow_ui::shell::hotkey::Hotkey;

/// 计划里的一条热键。
#[derive(Debug, Clone, PartialEq)]
pub struct PlannedHotkey {
    /// 来源配置键（注册成败按它回报给设置页）。
    pub config_key: &'static str,
    /// 日志里的功能名。
    pub label: &'static str,
    /// 配置里的热键原文。
    pub text: String,
    /// 解析结果；格式无效时是错误描述，注册这一步把它记成失败。
    pub parsed: Result<Hotkey, String>,
    /// 按下时触发的命令。
    pub command: AppCommand,
    /// 松开时触发的命令（按住说话）；`None` 表示忽略松开。
    pub on_release: Option<AppCommand>,
    /// 热键暂停期间是否仍保持注册（只有“暂停 / 恢复”开关本身）。
    pub stays_when_paused: bool,
}

/// 同构的“单键单命令”热键表：（配置键，日志功能名，按下时的命令）；覆盖截图、录屏、贴图剪贴板内容、输入框翻译四类。
pub fn simple_hotkey_table() -> [(&'static str, &'static str, AppCommand); 4] {
    [
        (
            SCREENSHOT_HOTKEY_CONFIG_KEY,
            "screenshot",
            AppCommand::Capture(CaptureRequest::default()),
        ),
        (
            RECORDING_HOTKEY_CONFIG_KEY,
            "recording",
            AppCommand::StartRecording(RecordingConfig::default()),
        ),
        (
            PIN_CLIPBOARD_HOTKEY_CONFIG_KEY,
            "pin_clipboard",
            AppCommand::PinSelection,
        ),
        (
            TRANSLATE_INPUT_HOTKEY_CONFIG_KEY,
            "translate_input",
            AppCommand::OpenTranslateInput,
        ),
    ]
}

/// 读出某个配置键下的全部热键文本，每条生成一项计划（未绑定则为空）。
///
/// # 参数
/// - `document`：配置文档。
/// - `key`：热键配置键。
/// - `label`：日志里的功能名。
/// - `command`：按下时触发的命令。
/// - `on_release`：松开时触发的命令，`None` 表示忽略松开。
fn plan_key(
    document: &ConfigDocument,
    key: &'static str,
    label: &'static str,
    command: &AppCommand,
    on_release: Option<&AppCommand>,
) -> Vec<PlannedHotkey> {
    shortcut_strings(&document.value(key))
        .into_iter()
        .map(|text| PlannedHotkey {
            config_key: key,
            label,
            parsed: Hotkey::parse(&portable_to_hotkey_text(&text)).map_err(|e| e.to_string()),
            text,
            command: command.clone(),
            on_release: on_release.cloned(),
            stays_when_paused: stays_registered_when_paused(key),
        })
        .collect()
}

/// 截图、录屏、贴图剪贴板内容、输入框翻译四类热键的计划。
///
/// # 参数
/// - `document`：配置文档。
pub fn simple_plan(document: &ConfigDocument) -> Vec<PlannedHotkey> {
    simple_hotkey_table()
        .iter()
        .flat_map(|&(key, label, ref command)| plan_key(document, key, label, command, None))
        .collect()
}

/// 语音转文字两个热键的计划：切换式（按一下开始、再按一下结束）与按住说话（按下开始、松开结束）。
///
/// 触发模式（`dictation/trigger_mode`）决定哪个生效；未绑定或被模式排除的不进计划。
///
/// # 参数
/// - `document`：配置文档。
pub fn dictation_plan(document: &ConfigDocument) -> Vec<PlannedHotkey> {
    let trigger = DictationConfig::from_document(document).trigger;
    let mut plan = Vec::new();
    if trigger.toggle_enabled() {
        plan.extend(plan_key(
            document,
            DICTATION_TOGGLE_HOTKEY_CONFIG_KEY,
            "dictation_toggle",
            &AppCommand::ToggleDictation,
            None,
        ));
    }
    if trigger.hold_enabled() {
        plan.extend(plan_key(
            document,
            DICTATION_HOLD_HOTKEY_CONFIG_KEY,
            "dictation_hold",
            &AppCommand::StartDictation,
            Some(&AppCommand::StopDictation),
        ));
    }
    plan
}

/// 全部“快捷动作”热键（直接截图、延迟截图、打开设置、暂停热键等）的计划；未绑定的不进计划。
///
/// # 参数
/// - `document`：配置文档。
/// - `paused`：热键是否处于暂停状态；暂停时只保留“暂停 / 恢复”开关本身。
pub fn quick_action_plan(document: &ConfigDocument, paused: bool) -> Vec<PlannedHotkey> {
    QUICK_ACTION_KEYS
        .iter()
        .filter(|(key, _)| !paused || stays_registered_when_paused(key))
        .flat_map(|&(key, action)| {
            plan_key(
                document,
                key,
                "quick_action",
                &AppCommand::QuickAction(action),
                None,
            )
        })
        .collect()
}

/// 全部已接线的全局热键计划：截图 + 录屏 + 贴图剪贴板内容 + 输入框翻译 + 语音转文字 + 快捷动作。
///
/// 暂停时只剩“暂停 / 恢复”开关本身，与旧的注册流程一致。
///
/// # 参数
/// - `document`：配置文档。
/// - `paused`：是否处于暂停状态。
///
/// # 返回
/// 按注册顺序排列的计划。
///
/// ```ignore
/// let plan = hotkey_plan(&ConfigDocument::from_bytes(None), false);
/// assert!(plan.iter().all(|p| p.parsed.is_ok()));
/// ```
pub fn hotkey_plan(document: &ConfigDocument, paused: bool) -> Vec<PlannedHotkey> {
    if paused {
        return quick_action_plan(document, true);
    }
    let mut plan = simple_plan(document);
    plan.extend(dictation_plan(document));
    plan.extend(quick_action_plan(document, false));
    plan
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use snow_app_core::command::QuickAction;

    /// 旧注册流程的“金标准”：按旧 `register_all_hotkeys_gated` 的顺序，逐项展开成
    /// `(配置键, 热键文本, 按下命令, 松开命令)`。独立于被测代码手写，用来对照计划。
    fn legacy_registration_set(
        document: &ConfigDocument,
        paused: bool,
    ) -> Vec<(String, String, AppCommand, Option<AppCommand>)> {
        let texts = |key: &str| shortcut_strings(&document.value(key));
        let mut out = Vec::new();
        let mut push = |key: &str, command: AppCommand, release: Option<AppCommand>| {
            for text in texts(key) {
                out.push((key.to_string(), text, command.clone(), release.clone()));
            }
        };
        let quick = |emit: &mut dyn FnMut(&str, AppCommand, Option<AppCommand>), paused: bool| {
            for (key, action) in QUICK_ACTION_KEYS {
                if paused && *key != "global_shortcuts/toggle_global_hotkeys" {
                    continue;
                }
                emit(key, AppCommand::QuickAction(*action), None);
            }
        };
        if paused {
            quick(&mut push, true);
            return out;
        }
        push(
            "global_shortcuts/screenshot",
            AppCommand::Capture(CaptureRequest::default()),
            None,
        );
        push(
            "global_shortcuts/screen_record",
            AppCommand::StartRecording(RecordingConfig::default()),
            None,
        );
        push(
            "global_shortcuts/pin_clipboard_content",
            AppCommand::PinSelection,
            None,
        );
        push(
            TRANSLATE_INPUT_HOTKEY_CONFIG_KEY,
            AppCommand::OpenTranslateInput,
            None,
        );
        let trigger = DictationConfig::from_document(document).trigger;
        if trigger.toggle_enabled() {
            push(
                DICTATION_TOGGLE_HOTKEY_CONFIG_KEY,
                AppCommand::ToggleDictation,
                None,
            );
        }
        if trigger.hold_enabled() {
            push(
                DICTATION_HOLD_HOTKEY_CONFIG_KEY,
                AppCommand::StartDictation,
                Some(AppCommand::StopDictation),
            );
        }
        quick(&mut push, false);
        out
    }

    /// 把计划展开成与金标准同形的元组。
    fn plan_tuples(
        plan: &[PlannedHotkey],
    ) -> Vec<(String, String, AppCommand, Option<AppCommand>)> {
        plan.iter()
            .map(|p| {
                (
                    p.config_key.to_string(),
                    p.text.clone(),
                    p.command.clone(),
                    p.on_release.clone(),
                )
            })
            .collect()
    }

    /// 给文档绑上一批热键：覆盖四类简单热键、两个听写热键与若干快捷动作（含多个绑定的键）。
    fn document_with_many_bindings() -> ConfigDocument {
        let mut doc = ConfigDocument::from_bytes(None);
        for (key, value) in [
            (SCREENSHOT_HOTKEY_CONFIG_KEY, json!(["Ctrl+Alt+A", "F1"])),
            (RECORDING_HOTKEY_CONFIG_KEY, json!(["Ctrl+Alt+R"])),
            (TRANSLATE_INPUT_HOTKEY_CONFIG_KEY, json!(["Ctrl+Alt+T"])),
            (DICTATION_TOGGLE_HOTKEY_CONFIG_KEY, json!(["Ctrl+Alt+D"])),
            (DICTATION_HOLD_HOTKEY_CONFIG_KEY, json!(["F9"])),
            ("global_shortcuts/open_settings", json!(["Ctrl+Alt+O"])),
            (
                "global_shortcuts/toggle_global_hotkeys",
                json!([{"portable": "Ctrl+Alt+P"}]),
            ),
            ("global_shortcuts/screenshot_copy", json!(["Ctrl+Shift+C"])),
        ] {
            doc.set_value(key, value).unwrap();
        }
        doc
    }

    /// 对照：默认配置下，计划与旧注册流程的注册集合逐项一致（含顺序），暂停与否都一样。
    #[test]
    fn plan_matches_legacy_registration_for_default_config() {
        let doc = ConfigDocument::from_bytes(None);
        for paused in [false, true] {
            assert_eq!(
                plan_tuples(&hotkey_plan(&doc, paused)),
                legacy_registration_set(&doc, paused),
                "paused = {paused}"
            );
        }
    }

    /// 对照：多处绑定下，计划与旧注册流程逐项一致；暂停时只剩“暂停 / 恢复”开关。
    #[test]
    fn plan_matches_legacy_registration_with_many_bindings() {
        let doc = document_with_many_bindings();
        let running = hotkey_plan(&doc, false);
        assert_eq!(plan_tuples(&running), legacy_registration_set(&doc, false));
        assert!(
            running.len() >= 8,
            "金标准应覆盖足够多的绑定: {}",
            running.len()
        );
        let paused = hotkey_plan(&doc, true);
        assert_eq!(plan_tuples(&paused), legacy_registration_set(&doc, true));
        assert_eq!(paused.len(), 1);
        assert_eq!(paused[0].text, "Ctrl+Alt+P");
        assert!(paused[0].stays_when_paused);
        assert_eq!(
            paused[0].command,
            AppCommand::QuickAction(QuickAction::ToggleGlobalHotkeys)
        );
    }

    /// 听写触发模式只让对应的热键进计划；按住说话带松开命令。
    #[test]
    fn dictation_plan_follows_trigger_mode() {
        let mut doc = document_with_many_bindings();
        let both = dictation_plan(&doc);
        assert_eq!(both.len(), 2);
        let hold = both
            .iter()
            .find(|p| p.config_key == DICTATION_HOLD_HOTKEY_CONFIG_KEY)
            .unwrap();
        assert_eq!(hold.command, AppCommand::StartDictation);
        assert_eq!(hold.on_release, Some(AppCommand::StopDictation));
        for (mode, expected) in [
            ("toggle", DICTATION_TOGGLE_HOTKEY_CONFIG_KEY),
            ("hold", DICTATION_HOLD_HOTKEY_CONFIG_KEY),
        ] {
            doc.set_value(
                crate::app_runtime::DICTATION_TRIGGER_MODE_CONFIG_KEY,
                json!(mode),
            )
            .unwrap();
            let only = dictation_plan(&doc);
            assert_eq!(only.len(), 1, "{mode}");
            assert_eq!(only[0].config_key, expected, "{mode}");
            assert_eq!(
                plan_tuples(&hotkey_plan(&doc, false)),
                legacy_registration_set(&doc, false),
                "{mode}"
            );
        }
    }

    /// 只有“暂停 / 恢复”开关标记为暂停期间保持注册；有效热键的解析结果与配置原文一致。
    #[test]
    fn stay_flags_and_parsed_hotkeys() {
        let doc = document_with_many_bindings();
        let plan = hotkey_plan(&doc, false);
        let staying: Vec<_> = plan
            .iter()
            .filter(|p| p.stays_when_paused)
            .map(|p| p.config_key)
            .collect();
        assert_eq!(staying, vec!["global_shortcuts/toggle_global_hotkeys"]);
        // 配置层会在写入时过滤掉格式非法的热键，所以正常配置下计划里不会有解析失败项
        assert!(plan.iter().all(|p| p.parsed.is_ok()));
        let first = plan
            .iter()
            .find(|p| p.text == "Ctrl+Alt+A")
            .expect("绑定的截图热键在计划里");
        assert_eq!(first.parsed.as_ref().unwrap().to_string(), "Ctrl+Alt+A");
    }

    /// 同构热键表覆盖截图 / 录屏 / 贴图剪贴板 / 输入框翻译四类，键与功能名互不重复。
    #[test]
    fn simple_hotkey_table_covers_four_kinds() {
        let table = simple_hotkey_table();
        let keys: Vec<_> = table.iter().map(|(k, _, _)| *k).collect();
        assert_eq!(
            keys,
            [
                SCREENSHOT_HOTKEY_CONFIG_KEY,
                RECORDING_HOTKEY_CONFIG_KEY,
                PIN_CLIPBOARD_HOTKEY_CONFIG_KEY,
                TRANSLATE_INPUT_HOTKEY_CONFIG_KEY,
            ]
        );
        let labels: std::collections::HashSet<_> = table.iter().map(|(_, l, _)| *l).collect();
        assert_eq!(labels.len(), 4);
    }
}
