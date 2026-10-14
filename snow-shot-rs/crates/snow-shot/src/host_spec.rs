//! 托盘宿主要用的快照 `host/spec.json` 的生成（纯函数）与动作 id 映射。
//!
//! 宿主不读 `config.json`、不带 i18n、不画图标，所以 UI 进程把“宿主要用的东西”——热键表、
//! 托盘菜单（已本地化文案与 16px 图标）、鼠标手势绑定、开关、提示文案、保活标志——一次性算成
//! [`HostSpec`]（方案 `docs/design/tray-host-split.md` §2）。UI 进程（`--ui`）在启动空闲时与配置变化后
//! 生成并经 `host_spec_sync` 原子写入 `host/spec.json`，托盘宿主读取它；`--headless emit-spec` 也用它。
//!
//! 动作 id 不发明新名字：托盘信号沿用现有字符串（`quit`、`settings`、`group:<id>` …），
//! `AppCommand` / `QuickAction` 用稳定的短名（`capture`、`quick.screenshot_delay` …）。

use crate::app_runtime::{TRAY_TOOLTIP, build_tray_spec_with_groups};
use crate::hotkey_plan::{PlannedHotkey, hotkey_plan};
use crate::quick_actions::{
    NOTICE_FULLSCREEN_GATE_OFF, NOTICE_FULLSCREEN_GATE_ON, QUICK_ACTION_KEYS,
};
use snow_app_core::command::{AppCommand, CaptureRequest, QuickAction, RecordingConfig};
use snow_config::document::ConfigDocument;
use snow_host_protocol::spec::{
    GateSpec, HostFlags, HostSpec, HostStrings, HotkeySpec, IconRgba, MenuItemSpec,
    MouseBindingSpec, TraySection, TrayTheme,
};
use snow_host_protocol::{
    ACTION_TOGGLE_FULLSCREEN_GATE, ACTION_TOGGLE_GLOBAL_HOTKEYS, KEEP_ALIVE_MCP,
    KEEP_ALIVE_OCR_HOT_START, PLACEHOLDER_COUNT, PLACEHOLDER_FAILURES, SPEC_VERSION,
};
use snow_i18n::Args;
use snow_ui::shell::tray::{TrayAction, TrayIconImage, TrayMenuEntry};

/// 命令 id：发起截图。
const COMMAND_CAPTURE: &str = snow_host_protocol::ACTION_CAPTURE;
/// 命令 id：开始录屏。
const COMMAND_START_RECORDING: &str = snow_host_protocol::ACTION_START_RECORDING;
/// 命令 id：把剪贴板内容贴到屏幕。
const COMMAND_PIN_SELECTION: &str = "pin_selection";
/// 命令 id：打开输入框翻译浮窗。
const COMMAND_OPEN_TRANSLATE_INPUT: &str = "open_translate_input";
/// 命令 id：切换式语音转文字。
const COMMAND_TOGGLE_DICTATION: &str = snow_host_protocol::ACTION_TOGGLE_DICTATION;
/// 命令 id：开始语音转文字（按住说话按下）。
const COMMAND_START_DICTATION: &str = snow_host_protocol::ACTION_START_DICTATION;
/// 命令 id：结束语音转文字（按住说话松开）。
const COMMAND_STOP_DICTATION: &str = "stop_dictation";

/// 主题配置值：深色。
const THEME_DARK: &str = "dark";
/// 主题配置值：浅色。
const THEME_LIGHT: &str = "light";

/// 提示文案 id：热键已暂停。
const NOTICE_HOTKEYS_PAUSED: &str = "quick-notice-hotkeys-paused";
/// 提示文案 id：热键已恢复。
const NOTICE_HOTKEYS_RESUMED: &str = "quick-notice-hotkeys-resumed";
/// 提示文案 id：恢复热键时有失败。
const NOTICE_HOTKEYS_RESUME_FAILED: &str = "quick-notice-hotkeys-resume-failed";
/// 提示文案 id：界面进程启动失败。
const NOTICE_UI_START_FAILED: &str = "tray-notice-ui-start-failed";
/// 提示文案 id：界面进程反复异常退出。
const NOTICE_UI_CRASH_LOOP: &str = "tray-notice-ui-crash-loop";
/// 提示文案 id：连续误触发保护的确认框正文（`$arg1` 次数占位符，产品名由运行时注入 `$product`）。
const NOTICE_RAPID_CAPTURE: &str = "tray-notice-rapid-capture";
/// 提示文案 id：热键暂停期间的托盘悬停提示。
const TOOLTIP_PAUSED: &str = "tray-tooltip-paused";
/// 文案里“失败原因”参数的序号。
const FAILURES_ARG_INDEX: u8 = 1;
/// 连续误触发文案里“次数占位符”参数的序号。
const RAPID_COUNT_ARG_INDEX: u8 = 1;

/// 快捷动作的稳定 id（`quick.<名>`）；两个由宿主自己处理的开关用协议里的常量。
///
/// # 参数
/// - `action`：快捷动作。
///
/// ```ignore
/// assert_eq!(quick_action_id(QuickAction::ScreenshotDelay), "quick.screenshot_delay");
/// ```
pub fn quick_action_id(action: QuickAction) -> &'static str {
    match action {
        QuickAction::ScreenshotDelay => "quick.screenshot_delay",
        QuickAction::ScreenshotFixed => "quick.screenshot_fixed",
        QuickAction::ScreenshotOcr => "quick.screenshot_ocr",
        QuickAction::ScreenshotTranslation => "quick.screenshot_translation",
        QuickAction::ScreenshotCopy => "quick.screenshot_copy",
        QuickAction::ScreenshotFullScreen => "quick.screenshot_full_screen",
        QuickAction::ScreenshotFocusedWindow => "quick.screenshot_focused_window",
        QuickAction::ScreenRecordCopy => "quick.screen_record_copy",
        QuickAction::OpenScreenRecordingFolder => "quick.open_screen_recording_folder",
        QuickAction::OpenCaptureHistory => "quick.open_capture_history",
        QuickAction::OpenPinManagement => "quick.open_pin_management",
        QuickAction::OpenSettings => "quick.open_settings",
        QuickAction::TranslateSelectedText => "quick.translate_selected_text",
        QuickAction::PinSelectedFiles => "quick.pin_selected_files",
        QuickAction::RestoreLastClosedWindows => "quick.restore_last_closed_windows",
        QuickAction::ToggleGlobalHotkeys => ACTION_TOGGLE_GLOBAL_HOTKEYS,
        QuickAction::ToggleDisableOnFocusedFullscreen => ACTION_TOGGLE_FULLSCREEN_GATE,
    }
}

/// 命令的稳定 id；快照里不会出现的命令（截图会话内部命令等）返回 `None`。
///
/// # 参数
/// - `command`：热键或托盘绑定的命令。
///
/// ```ignore
/// assert_eq!(command_id(&AppCommand::PinSelection), Some("pin_selection"));
/// ```
pub fn command_id(command: &AppCommand) -> Option<&'static str> {
    Some(match command {
        AppCommand::Capture(_) => COMMAND_CAPTURE,
        AppCommand::StartRecording(_) => COMMAND_START_RECORDING,
        AppCommand::PinSelection => COMMAND_PIN_SELECTION,
        AppCommand::OpenTranslateInput => COMMAND_OPEN_TRANSLATE_INPUT,
        AppCommand::ToggleDictation => COMMAND_TOGGLE_DICTATION,
        AppCommand::StartDictation => COMMAND_START_DICTATION,
        AppCommand::StopDictation => COMMAND_STOP_DICTATION,
        AppCommand::QuickAction(action) => quick_action_id(*action),
        _ => return None,
    })
}

/// [`command_id`] 的逆映射：UI 进程收到宿主转发的动作 id 后还原成命令。
///
/// # 参数
/// - `id`：动作 id；托盘信号（`quit`、`group:x` …）不是命令，返回 `None`。
///
/// # 返回
/// 命令（请求参数取默认值，与托盘 / 热键里绑定的完全一致）。
///
/// ```ignore
/// assert_eq!(command_from_id("quick.open_settings"), Some(AppCommand::QuickAction(QuickAction::OpenSettings)));
/// ```
pub fn command_from_id(id: &str) -> Option<AppCommand> {
    let simple = match id {
        COMMAND_CAPTURE => Some(AppCommand::Capture(CaptureRequest::default())),
        COMMAND_START_RECORDING => Some(AppCommand::StartRecording(RecordingConfig::default())),
        COMMAND_PIN_SELECTION => Some(AppCommand::PinSelection),
        COMMAND_OPEN_TRANSLATE_INPUT => Some(AppCommand::OpenTranslateInput),
        COMMAND_TOGGLE_DICTATION => Some(AppCommand::ToggleDictation),
        COMMAND_START_DICTATION => Some(AppCommand::StartDictation),
        COMMAND_STOP_DICTATION => Some(AppCommand::StopDictation),
        _ => None,
    };
    simple.or_else(|| {
        QUICK_ACTION_KEYS
            .iter()
            .map(|(_, action)| *action)
            .find(|action| quick_action_id(*action) == id)
            .map(AppCommand::QuickAction)
    })
}

/// 托盘动作的 id：命令取 [`command_id`]，信号原样。
///
/// # 参数
/// - `action`：托盘菜单项或点击动作。
fn tray_action_id(action: &TrayAction) -> Option<String> {
    match action {
        TrayAction::Command(command) => command_id(command).map(str::to_string),
        TrayAction::Signal(signal) => Some(signal.clone()),
    }
}

/// 托盘图标转快照里的像素。
fn icon_rgba(icon: &TrayIconImage) -> IconRgba {
    let (width, height) = icon.size();
    IconRgba {
        width,
        height,
        rgba: icon.rgba().into(),
    }
}

/// 托盘菜单项转快照菜单项；动作没有 id（不应出现）的项被丢弃并记日志。
fn menu_item_spec(entry: &TrayMenuEntry) -> Option<MenuItemSpec> {
    match entry {
        TrayMenuEntry::Separator => Some(MenuItemSpec::Sep),
        TrayMenuEntry::Item {
            label,
            enabled,
            checked,
            icon,
            action,
        } => {
            let Some(action_id) = tray_action_id(action) else {
                tracing::warn!(%label, "托盘菜单项的动作没有稳定 id，已从快照里丢弃");
                return None;
            };
            Some(MenuItemSpec::Item {
                label: label.clone(),
                enabled: *enabled,
                checked: *checked,
                icon_rgba: icon.as_ref().map(icon_rgba),
                action_id,
            })
        }
    }
}

/// 热键计划项转快照热键；格式无效、没有稳定 id 或没有键码的项返回 `None`（注册阶段本来就会失败）。
fn hotkey_spec(planned: &PlannedHotkey) -> Option<HotkeySpec> {
    let hotkey = planned.parsed.as_ref().ok()?;
    let Some(id) = command_id(&planned.command) else {
        tracing::warn!(shortcut = %planned.text, "热键命令没有稳定 id，已从快照里丢弃");
        return None;
    };
    Some(HotkeySpec {
        id: id.to_string(),
        config_key: planned.config_key.to_string(),
        mods: hotkey.modifiers.win_bits(),
        vk: hotkey.key.win_vk()?,
        text: hotkey.to_string(),
        on_release_id: planned
            .on_release
            .as_ref()
            .and_then(command_id)
            .map(str::to_string),
        stays_when_paused: planned.stays_when_paused,
    })
}

/// 读出全部有效的鼠标手势绑定（沿用配置里的名字，宿主用 `snow-platform` 的解析函数还原）。
fn mouse_specs(document: &ConfigDocument) -> Vec<MouseBindingSpec> {
    crate::mouse_gesture::ACTIONS
        .iter()
        .filter_map(|action| {
            let value = document.value(&format!("global_mouse/{action}"));
            // 无效配置（空对象、没有修饰键、未知名字）与实际安装钩子时一样被忽略
            crate::mouse_gesture::binding_from_value(action, &value)?;
            let modifiers = crate::mouse_gesture::activation_names(&value)?
                .into_iter()
                .map(str::to_string)
                .collect();
            Some(MouseBindingSpec {
                action: (*action).to_string(),
                modifiers,
                button: value.get("mouse_button")?.as_str()?.to_string(),
            })
        })
        .collect()
}

/// 保活原因：用户显式要求常驻的功能（MCP、OCR 预热，方案 §10 第 2、3 条）；语音热键不保活（第 4 条）。
///
/// # 参数
/// - `document`：配置文档。
///
/// ```ignore
/// assert!(keep_alive_reasons(&ConfigDocument::from_bytes(None)).is_empty());
/// ```
pub fn keep_alive_reasons(document: &ConfigDocument) -> Vec<String> {
    let mut reasons = Vec::new();
    if document.bool_or(crate::mcp_host::KEY_ENABLED, false) {
        reasons.push(KEEP_ALIVE_MCP.to_string());
    }
    if document.bool_or(crate::ocr_service::KEY_HOT_START, false) {
        reasons.push(KEEP_ALIVE_OCR_HOT_START.to_string());
    }
    reasons
}

/// 弹出菜单的主题：配置显式指定深 / 浅就定死，其余交给宿主弹菜单前读系统设置。
fn tray_theme(document: &ConfigDocument) -> TrayTheme {
    match document.str_or(crate::settings_model::THEME_MODE_KEY, "") {
        THEME_DARK => TrayTheme::Dark,
        THEME_LIGHT => TrayTheme::Light,
        _ => TrayTheme::System,
    }
}

/// 生成快照所需、不在配置文档里的输入。
#[derive(Debug, Clone, Copy)]
pub struct HostSpecInputs<'a> {
    /// 界面语料语言代码（如 `zh-CN`）。
    pub locale: &'a str,
    /// 全局热键当前是否被暂停（决定“禁用全局热键”菜单项的勾选）。
    pub hotkeys_paused: bool,
    /// 贴图分组块的 `(分组 ID, 显示名)` 列表；为空则不显示分组块。
    pub groups: &'a [(String, String)],
    /// 当前激活的贴图分组 ID（打勾）。
    pub active_group: &'a str,
    /// 登录时需要恢复的贴图数量。
    pub pins_to_restore: u32,
    /// 下一次更新检查的到点时间（Unix 秒）。
    pub update_due_at: Option<u64>,
    /// 快照修订号（单调递增，由调用方维护）。
    pub rev: u64,
}

/// 按配置文档生成宿主快照（纯函数：不读系统状态、不写文件）。
///
/// 托盘被关闭时只带图标与空菜单，不渲染菜单（与单进程模式不建托盘一致）。
///
/// # 参数
/// - `document`：配置文档。
/// - `inputs`：不在配置里的输入（语言、暂停状态、贴图分组、修订号等）。
///
/// # 返回
/// 快照；托盘图标数据非法时返回错误文本。
///
/// ```ignore
/// let doc = ConfigDocument::from_bytes(None);
/// let spec = host_spec_from_document(&doc, &HostSpecInputs {
///     locale: "zh-CN", hotkeys_paused: false, groups: &[], active_group: "",
///     pins_to_restore: 0, update_due_at: None, rev: 1,
/// })?;
/// assert_eq!(HostSpec::from_json(&spec.to_json()?)?, spec);
/// ```
pub fn host_spec_from_document(
    document: &ConfigDocument,
    inputs: &HostSpecInputs<'_>,
) -> Result<HostSpec, String> {
    let i18n = crate::ocr_backend::i18n_for(inputs.locale);
    let enabled = crate::tray_config::tray_enabled(document);
    let paused_tooltip = i18n.tr(TOOLTIP_PAUSED);
    let tray = if enabled {
        let spec = build_tray_spec_with_groups(
            inputs.locale,
            document,
            inputs.hotkeys_paused,
            inputs.groups,
            inputs.active_group,
        )?;
        TraySection {
            enabled,
            tooltip: spec.tooltip.clone(),
            paused_tooltip,
            icon_rgba: icon_rgba(&spec.icon),
            menu: spec.menu.iter().filter_map(menu_item_spec).collect(),
            on_left: spec.on_left_click.as_ref().and_then(tray_action_id),
            on_middle: spec.on_middle_click.as_ref().and_then(tray_action_id),
            on_double: spec.on_double_click.as_ref().and_then(tray_action_id),
            theme: tray_theme(document),
        }
    } else {
        TraySection {
            enabled,
            tooltip: TRAY_TOOLTIP.to_string(),
            paused_tooltip,
            icon_rgba: icon_rgba(&crate::tray_config::tray_icon_image(document)),
            menu: Vec::new(),
            on_left: None,
            on_middle: None,
            on_double: None,
            theme: tray_theme(document),
        }
    };
    let failures = i18n.tr_with(
        NOTICE_HOTKEYS_RESUME_FAILED,
        &Args::new().arg(FAILURES_ARG_INDEX, PLACEHOLDER_FAILURES),
    );
    let spec = HostSpec {
        spec_version: SPEC_VERSION,
        rev: inputs.rev,
        hotkeys: hotkey_plan(document, false)
            .iter()
            .filter_map(hotkey_spec)
            .collect(),
        tray,
        mouse: mouse_specs(document),
        gate: GateSpec {
            fullscreen_disable: crate::fullscreen_gate::configured(document),
        },
        strings: HostStrings {
            notice_paused: i18n.tr(NOTICE_HOTKEYS_PAUSED),
            notice_resumed: i18n.tr(NOTICE_HOTKEYS_RESUMED),
            notice_resume_failed: failures,
            notice_gate_on: i18n.tr(NOTICE_FULLSCREEN_GATE_ON),
            notice_gate_off: i18n.tr(NOTICE_FULLSCREEN_GATE_OFF),
            notice_ui_start_failed: i18n.tr(NOTICE_UI_START_FAILED),
            notice_ui_crash_loop: i18n.tr(NOTICE_UI_CRASH_LOOP),
            notice_rapid_capture: i18n.tr_with(
                NOTICE_RAPID_CAPTURE,
                &Args::new().arg(RAPID_COUNT_ARG_INDEX, PLACEHOLDER_COUNT),
            ),
        },
        flags: HostFlags {
            keep_alive: keep_alive_reasons(document),
            pins_to_restore: inputs.pins_to_restore,
            update_due_at: inputs.update_due_at,
        },
    };
    spec.validate().map_err(|e| e.to_string())?;
    Ok(spec)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app_runtime::build_tray_spec;
    use serde_json::json;
    use snow_config::document::ConfigDocument;
    use std::collections::HashSet;

    /// 测试用的默认输入。
    fn inputs<'a>(groups: &'a [(String, String)], active: &'a str) -> HostSpecInputs<'a> {
        HostSpecInputs {
            locale: "zh-CN",
            hotkeys_paused: false,
            groups,
            active_group: active,
            pins_to_restore: 0,
            update_due_at: None,
            rev: 1,
        }
    }

    /// 宿主的“会开覆盖窗”清单与 UI 的实际行为一致：清单里的 id 都能还原成截图 / 录屏 / 覆盖窗类命令，
    /// 反过来所有走覆盖窗（含延迟倒计时、录屏选区）的快捷动作都在清单里；直接截图不在清单里。
    #[test]
    fn capture_action_ids_match_overlay_behaviour() {
        use crate::quick_actions::{QuickPlan, plan_for};
        use snow_host_protocol::{ACTION_SCROLL_CAPTURE, CAPTURE_ACTION_IDS};
        let opens_overlay = |action: QuickAction| {
            matches!(
                plan_for(action),
                QuickPlan::Overlay(_) | QuickPlan::Delayed | QuickPlan::RecordAndCopy
            )
        };
        for id in CAPTURE_ACTION_IDS {
            if *id == ACTION_SCROLL_CAPTURE {
                continue;
            }
            match command_from_id(id).unwrap_or_else(|| panic!("{id} 无法还原成命令")) {
                AppCommand::Capture(_) | AppCommand::StartRecording(_) => {}
                AppCommand::QuickAction(action) => assert!(opens_overlay(action), "{id}"),
                other => panic!("{id} 不该是 {other:?}"),
            }
        }
        for (_, action) in QUICK_ACTION_KEYS {
            assert_eq!(
                opens_overlay(*action),
                CAPTURE_ACTION_IDS.contains(&quick_action_id(*action)),
                "{action:?}"
            );
        }
    }

    /// 全部快捷动作都有 id：id 互不重复、都带命名空间前缀（两个宿主开关用协议常量），且能还原。
    #[test]
    fn quick_action_ids_are_unique_and_reversible() {
        let ids: Vec<_> = QUICK_ACTION_KEYS
            .iter()
            .map(|(_, action)| quick_action_id(*action))
            .collect();
        assert_eq!(
            ids.iter().collect::<HashSet<_>>().len(),
            QUICK_ACTION_KEYS.len()
        );
        for (_, action) in QUICK_ACTION_KEYS {
            let id = quick_action_id(*action);
            assert!(id.starts_with("quick."), "{id}");
            assert_eq!(
                command_from_id(id),
                Some(AppCommand::QuickAction(*action)),
                "{id}"
            );
        }
        assert_eq!(
            quick_action_id(QuickAction::ToggleGlobalHotkeys),
            ACTION_TOGGLE_GLOBAL_HOTKEYS
        );
        assert_eq!(
            quick_action_id(QuickAction::ToggleDisableOnFocusedFullscreen),
            ACTION_TOGGLE_FULLSCREEN_GATE
        );
    }

    /// 命令 id 往返；不在快照里出现的命令没有 id；托盘信号不是命令。
    #[test]
    fn command_ids_round_trip() {
        for command in [
            AppCommand::Capture(CaptureRequest::default()),
            AppCommand::StartRecording(RecordingConfig::default()),
            AppCommand::PinSelection,
            AppCommand::OpenTranslateInput,
            AppCommand::ToggleDictation,
            AppCommand::StartDictation,
            AppCommand::StopDictation,
        ] {
            let id = command_id(&command).expect("应有 id");
            assert_eq!(command_from_id(id), Some(command));
        }
        assert_eq!(command_id(&AppCommand::Recapture), None);
        for signal in ["quit", "settings", "group:abc", "pin_clipboard", ""] {
            assert_eq!(command_from_id(signal), None, "{signal}");
        }
        // 命令 id 与托盘信号 id 不会撞名
        for signal in [
            crate::app_runtime::TRAY_SIGNAL_PIN_CLIPBOARD,
            crate::app_runtime::TRAY_SIGNAL_HISTORY,
            crate::app_runtime::TRAY_SIGNAL_MAIN_WINDOW,
            crate::app_runtime::TRAY_SIGNAL_SETTINGS,
            crate::app_runtime::TRAY_SIGNAL_QUIT,
            crate::app_runtime::TRAY_SIGNAL_RESTART,
            crate::app_runtime::TRAY_SIGNAL_PIN_MANAGE,
            crate::app_runtime::TRAY_SIGNAL_GROUP_NEW,
            crate::app_runtime::TRAY_SIGNAL_GROUP_DELETE_EMPTY,
        ] {
            assert!(command_from_id(signal).is_none(), "{signal} 撞了命令 id");
        }
        assert_eq!(
            snow_host_protocol::ACTION_QUIT,
            crate::app_runtime::TRAY_SIGNAL_QUIT
        );
    }

    /// 默认配置：快照序列化再反序列化得到同一份，且通过校验。
    #[test]
    fn default_config_spec_round_trips() {
        let doc = ConfigDocument::from_bytes(None);
        let spec = host_spec_from_document(&doc, &inputs(&[], "")).unwrap();
        let json = spec.to_json().unwrap();
        assert_eq!(HostSpec::from_json(&json).unwrap(), spec);
        assert_eq!(spec.spec_version, SPEC_VERSION);
        assert!(spec.tray.enabled);
        assert_eq!(spec.tray.icon_rgba.width, crate::tray_config::ICON_SIZE);
        assert!(!spec.tray.menu.is_empty());
        assert!(
            spec.strings
                .notice_resume_failed
                .contains(PLACEHOLDER_FAILURES)
        );
        assert!(!spec.strings.notice_paused.is_empty());
        assert!(!spec.tray.paused_tooltip.is_empty());
        assert!(spec.flags.keep_alive.is_empty());
        assert_eq!(spec.tray.theme, TrayTheme::System);
    }

    /// 快照的托盘部分与单进程模式用的托盘描述一致：同样的菜单项数、文案、勾选、图标尺寸与点击动作。
    #[test]
    fn tray_section_matches_tray_spec() {
        let doc = ConfigDocument::from_bytes(None);
        let legacy = build_tray_spec("zh-CN", &doc, false).unwrap();
        let spec = host_spec_from_document(&doc, &inputs(&[], "")).unwrap();
        assert_eq!(spec.tray.menu.len(), legacy.menu.len());
        for (got, want) in spec.tray.menu.iter().zip(&legacy.menu) {
            match (got, want) {
                (MenuItemSpec::Sep, TrayMenuEntry::Separator) => {}
                (
                    MenuItemSpec::Item {
                        label,
                        enabled,
                        checked,
                        icon_rgba,
                        action_id,
                    },
                    TrayMenuEntry::Item {
                        label: want_label,
                        enabled: want_enabled,
                        checked: want_checked,
                        icon: want_icon,
                        action,
                    },
                ) => {
                    assert_eq!(label, want_label);
                    assert_eq!(enabled, want_enabled);
                    assert_eq!(checked, want_checked);
                    assert_eq!(
                        icon_rgba
                            .as_ref()
                            .map(|i| (i.width, i.height, i.rgba.clone())),
                        want_icon
                            .as_ref()
                            .map(|i| (i.size().0, i.size().1, i.rgba().into()))
                    );
                    assert_eq!(Some(action_id.clone()), tray_action_id(action));
                }
                other => panic!("菜单项种类不一致: {other:?}"),
            }
        }
        assert_eq!(
            spec.tray.on_left,
            legacy.on_left_click.as_ref().and_then(tray_action_id)
        );
        assert_eq!(
            spec.tray.on_middle,
            legacy.on_middle_click.as_ref().and_then(tray_action_id)
        );
        assert_eq!(spec.tray.tooltip, legacy.tooltip);
    }

    /// 热键表：每个有效热键都带键码，修饰键位、文本与解析结果一致；
    /// 与热键计划逐项对应（暂停开关标记保持注册）。
    #[test]
    fn hotkeys_follow_the_plan() {
        let mut doc = ConfigDocument::from_bytes(None);
        for (key, value) in [
            (
                crate::app_runtime::SCREENSHOT_HOTKEY_CONFIG_KEY,
                json!(["Ctrl+Alt+A"]),
            ),
            (
                crate::app_runtime::DICTATION_HOLD_HOTKEY_CONFIG_KEY,
                json!(["F9"]),
            ),
            (
                "global_shortcuts/toggle_global_hotkeys",
                json!(["Ctrl+Alt+P"]),
            ),
        ] {
            doc.set_value(key, value).unwrap();
        }
        let spec = host_spec_from_document(&doc, &inputs(&[], "")).unwrap();
        let plan = hotkey_plan(&doc, false);
        assert_eq!(spec.hotkeys.len(), plan.len(), "有效热键逐项进快照");
        // 格式无效的计划项（只可能来自磁盘上被手改的配置）不进快照
        let mut broken = plan[0].clone();
        broken.parsed = Err("无法识别的键".into());
        assert!(hotkey_spec(&broken).is_none());
        let ctrl_alt_a = spec
            .hotkeys
            .iter()
            .find(|h| h.text == "Ctrl+Alt+A")
            .unwrap();
        assert_eq!(ctrl_alt_a.id, COMMAND_CAPTURE);
        assert_eq!(ctrl_alt_a.mods, 0x1 | 0x2);
        assert_eq!(ctrl_alt_a.vk, 0x41);
        assert!(!ctrl_alt_a.stays_when_paused);
        let hold = spec.hotkeys.iter().find(|h| h.text == "F9").unwrap();
        assert_eq!(hold.id, COMMAND_START_DICTATION);
        assert_eq!(hold.on_release_id.as_deref(), Some(COMMAND_STOP_DICTATION));
        assert_eq!((hold.mods, hold.vk), (0, 0x78));
        let toggle = spec
            .hotkeys
            .iter()
            .find(|h| h.text == "Ctrl+Alt+P")
            .unwrap();
        assert_eq!(toggle.id, ACTION_TOGGLE_GLOBAL_HOTKEYS);
        assert!(toggle.stays_when_paused);
        assert_eq!(
            spec.hotkeys.iter().filter(|h| h.stays_when_paused).count(),
            1
        );
    }

    /// 鼠标手势：默认三项绑定进快照，保留配置里的名字；无效项不进。
    #[test]
    fn mouse_bindings_use_config_vocabulary() {
        let mut doc = ConfigDocument::from_bytes(None);
        let spec = host_spec_from_document(&doc, &inputs(&[], "")).unwrap();
        assert_eq!(spec.mouse.len(), 3);
        let copy = spec
            .mouse
            .iter()
            .find(|m| m.action == "screenshot_copy")
            .unwrap();
        assert_eq!(copy.button, "left_drag");
        assert!(!copy.modifiers.is_empty());
        for m in &spec.mouse {
            // 快照里的名字必须能被平台层原样还原
            let value = json!({"activation_key": m.modifiers, "mouse_button": m.button});
            assert!(crate::mouse_gesture::binding_from_value(&m.action, &value).is_some());
        }
        doc.set_value("global_mouse/screenshot_copy", json!({}))
            .unwrap();
        let spec = host_spec_from_document(&doc, &inputs(&[], "")).unwrap();
        assert_eq!(spec.mouse.len(), 2);
    }

    /// 配置重载后单个激活键是字符串：快照里仍要有手势绑定（只认数组会让宿主把全部手势钩子卸掉）。
    #[test]
    fn mouse_bindings_survive_string_activation_key() {
        let mut doc = ConfigDocument::from_bytes(None);
        for (action, button) in [
            ("screenshot_copy", "left_drag"),
            ("screenshot_fixed", "wheel_drag"),
            ("screenshot_ocr", "right_drag"),
        ] {
            doc.set_value(
                &format!("global_mouse/{action}"),
                json!({"activation_key": "windows", "mouse_button": button}),
            )
            .unwrap();
        }
        let spec = host_spec_from_document(&doc, &inputs(&[], "")).unwrap();
        assert_eq!(spec.mouse.len(), 3);
        assert!(spec.mouse.iter().all(|m| m.modifiers == ["windows"]));
    }

    /// 开关、保活原因、暂停勾选、分组与登录恢复字段都按输入与配置落进快照。
    #[test]
    fn gate_flags_and_inputs_land_in_spec() {
        let mut doc = ConfigDocument::from_bytes(None);
        doc.set_value(
            "global_shortcuts/disable_on_focused_fullscreen_window",
            json!(true),
        )
        .unwrap();
        doc.set_value(crate::mcp_host::KEY_ENABLED, json!(true))
            .unwrap();
        doc.set_value(crate::ocr_service::KEY_HOT_START, json!(true))
            .unwrap();
        doc.set_value(crate::settings_model::THEME_MODE_KEY, json!("dark"))
            .unwrap();
        let groups = vec![("g1".to_string(), "分组一".to_string())];
        let spec = host_spec_from_document(
            &doc,
            &HostSpecInputs {
                hotkeys_paused: true,
                pins_to_restore: 3,
                update_due_at: Some(1_900_000_000),
                rev: 42,
                ..inputs(&groups, "g1")
            },
        )
        .unwrap();
        assert!(spec.gate.fullscreen_disable);
        assert_eq!(spec.flags.keep_alive, vec!["mcp", "ocr_hot_start"]);
        assert_eq!(spec.flags.pins_to_restore, 3);
        assert_eq!(spec.flags.update_due_at, Some(1_900_000_000));
        assert_eq!(spec.rev, 42);
        assert_eq!(spec.tray.theme, TrayTheme::Dark);
        // 分组块进了菜单且激活项打勾；暂停勾选反映在“禁用全局热键”上
        let group = spec.tray.menu.iter().find_map(|m| match m {
            MenuItemSpec::Item {
                label,
                checked,
                action_id,
                ..
            } if action_id == "group:g1" => Some((label.clone(), *checked)),
            _ => None,
        });
        assert_eq!(group, Some(("分组一".to_string(), Some(true))));
        let paused_toggle = spec.tray.menu.iter().find_map(|m| match m {
            MenuItemSpec::Item {
                checked, action_id, ..
            } if action_id == ACTION_TOGGLE_GLOBAL_HOTKEYS => Some(*checked),
            _ => None,
        });
        assert_eq!(paused_toggle, Some(Some(true)));
        doc.set_value(crate::settings_model::THEME_MODE_KEY, json!("light"))
            .unwrap();
        let light = host_spec_from_document(&doc, &inputs(&[], "")).unwrap();
        assert_eq!(light.tray.theme, TrayTheme::Light);
    }

    /// 保活原因（§10 第 2、3 条）：默认全无；开 MCP / OCR 预热各加一项，顺序固定；
    /// 绑定语音热键不产生任何保活原因（第 4 条，2026-10-13 改判）。
    #[test]
    fn keep_alive_reasons_follow_config() {
        let mut doc = ConfigDocument::from_bytes(None);
        assert!(keep_alive_reasons(&doc).is_empty());
        doc.set_value(
            crate::app_runtime::DICTATION_TOGGLE_HOTKEY_CONFIG_KEY,
            json!(["Ctrl+Alt+D"]),
        )
        .unwrap();
        doc.set_value(
            crate::app_runtime::DICTATION_TRIGGER_MODE_CONFIG_KEY,
            json!("toggle"),
        )
        .unwrap();
        assert!(keep_alive_reasons(&doc).is_empty(), "绑语音热键不保活");
        let spec = host_spec_from_document(&doc, &inputs(&[], "")).unwrap();
        assert!(spec.flags.keep_alive.is_empty());
        assert!(!spec.flags.needs_resident_ui());
        doc.set_value(crate::mcp_host::KEY_ENABLED, json!(true))
            .unwrap();
        doc.set_value(crate::ocr_service::KEY_HOT_START, json!(true))
            .unwrap();
        assert_eq!(
            keep_alive_reasons(&doc),
            [KEEP_ALIVE_MCP, KEEP_ALIVE_OCR_HOT_START]
        );
    }

    /// 托盘在设置里关闭：快照里 `enabled = false`、菜单为空，但仍带合法图标与文案。
    #[test]
    fn disabled_tray_has_empty_menu() {
        let mut doc = ConfigDocument::from_bytes(None);
        doc.set_value(crate::tray_config::KEY_ENABLED, json!(false))
            .unwrap();
        let spec = host_spec_from_document(&doc, &inputs(&[], "")).unwrap();
        assert!(!spec.tray.enabled);
        assert!(spec.tray.menu.is_empty());
        assert!(spec.tray.icon_rgba.is_consistent());
        assert_eq!(HostSpec::from_json(&spec.to_json().unwrap()).unwrap(), spec);
    }

    /// 两种界面语言都能生成快照，文案随语言变化。
    #[test]
    fn strings_follow_locale() {
        let doc = ConfigDocument::from_bytes(None);
        let zh = host_spec_from_document(&doc, &inputs(&[], "")).unwrap();
        let en = host_spec_from_document(
            &doc,
            &HostSpecInputs {
                locale: "en-US",
                ..inputs(&[], "")
            },
        )
        .unwrap();
        assert_ne!(zh.strings.notice_paused, en.strings.notice_paused);
        assert!(
            en.strings
                .notice_resume_failed
                .contains(PLACEHOLDER_FAILURES)
        );
        // 连续误触发确认文案：次数占位符原样保留给宿主填，产品名已由运行时注入
        for spec in [&zh, &en] {
            let text = &spec.strings.notice_rapid_capture;
            assert!(text.contains(PLACEHOLDER_COUNT), "{text}");
            assert!(text.contains(snow_app_core::PRODUCT_NAME), "{text}");
            assert!(!text.contains("$product"), "{text}");
        }
        assert_ne!(
            zh.strings.notice_rapid_capture,
            en.strings.notice_rapid_capture
        );
    }
}
