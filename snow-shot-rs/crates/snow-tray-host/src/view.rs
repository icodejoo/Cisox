//! 从快照算出宿主“实际要显示 / 要注册”的东西（纯函数）。
//!
//! 快照里的菜单勾选、热键表是 UI 写入时的状态；宿主自己持有“热键暂停”“前台全屏闸门”两个开关，
//! 所以真正显示的菜单勾选、真正注册的热键要在快照之上再叠一层宿主状态。

use serde_json::Value;
use snow_host_protocol::message::HotkeyResult;
use snow_host_protocol::spec::{GateSpec, HostFlags, HostStrings, TrayTheme};
use snow_host_protocol::spec::{HostSpec, HotkeySpec, IconRgba, MenuItemSpec, TraySection};
use snow_host_protocol::{
    ACTION_QUIT, ACTION_TOGGLE_FULLSCREEN_GATE, ACTION_TOGGLE_GLOBAL_HOTKEYS,
    CONFIG_KEY_DISABLE_ON_FULLSCREEN, PLACEHOLDER_FAILURES, SPEC_VERSION,
};

/// 内置最小菜单里“退出”的文案：快照缺失 / 损坏时唯一写死的界面文字（不进 i18n，方案 §2）。
pub const FALLBACK_QUIT_LABEL: &str = "Quit";

/// 内置兜底托盘的悬停提示（产品名，不可翻译）。
pub const FALLBACK_TOOLTIP: &str = "Cisox";

/// 内置兜底：界面进程启动失败的提示（快照里的本地化文案为空时用）。
pub const FALLBACK_NOTICE_UI_START_FAILED: &str = "The UI process failed to start";

/// 内置兜底：界面进程反复异常退出的提示。
pub const FALLBACK_NOTICE_UI_CRASH_LOOP: &str = "The UI process keeps crashing; retrying later";

/// 内置兜底托盘图标的边长与颜色（纯色方块，宿主没有图标资源）。
const FALLBACK_ICON_SIZE: u32 = 32;

/// 内置兜底托盘图标的颜色。
const FALLBACK_ICON_RGBA: [u8; 4] = [0x1f, 0x6f, 0xeb, 0xff];

/// 悬停提示里放提示文案时的最大字符数（系统限制 127 个 UTF-16 单元，留出余量）。
pub const NOTICE_TOOLTIP_MAX_CHARS: usize = 100;

/// 内置兜底快照：只有一个“退出”菜单项的托盘，没有热键。
///
/// # 返回
/// 通过 [`HostSpec::validate`] 的快照（`rev` 为 0，任何真实快照都比它新）。
///
/// ```rust
/// let spec = snow_tray_host::view::fallback_spec();
/// assert!(spec.validate().is_ok());
/// assert!(spec.hotkeys.is_empty());
/// ```
pub fn fallback_spec() -> HostSpec {
    let pixels = (FALLBACK_ICON_SIZE * FALLBACK_ICON_SIZE) as usize;
    HostSpec {
        spec_version: SPEC_VERSION,
        rev: 0,
        hotkeys: Vec::new(),
        tray: TraySection {
            enabled: true,
            tooltip: FALLBACK_TOOLTIP.to_string(),
            paused_tooltip: FALLBACK_TOOLTIP.to_string(),
            icon_rgba: IconRgba {
                width: FALLBACK_ICON_SIZE,
                height: FALLBACK_ICON_SIZE,
                rgba: FALLBACK_ICON_RGBA.repeat(pixels),
            },
            menu: vec![MenuItemSpec::Item {
                label: FALLBACK_QUIT_LABEL.to_string(),
                enabled: true,
                checked: None,
                icon_rgba: None,
                action_id: ACTION_QUIT.to_string(),
            }],
            on_left: None,
            on_middle: None,
            on_double: None,
            theme: TrayTheme::System,
        },
        mouse: Vec::new(),
        gate: GateSpec::default(),
        strings: HostStrings::default(),
        flags: HostFlags::default(),
    }
}

/// 叠上宿主状态后的托盘菜单：两个开关项的勾选以宿主自己的状态为准。
///
/// 只改本来就带复选框的项（快照里 `checked` 为 `Some`），不给别的项凭空加勾选。
///
/// # 参数
/// - `menu`：快照里的菜单。
/// - `paused`：热键是否被暂停。
/// - `gate_on`：“前台全屏停用热键”是否打开。
///
/// # 返回
/// 实际要显示的菜单。
///
/// ```rust
/// use snow_host_protocol::spec::MenuItemSpec;
/// use snow_host_protocol::ACTION_TOGGLE_GLOBAL_HOTKEYS;
/// use snow_tray_host::view::effective_menu;
/// let menu = vec![MenuItemSpec::Item {
///     label: "pause".into(), enabled: true, checked: Some(false),
///     icon_rgba: None, action_id: ACTION_TOGGLE_GLOBAL_HOTKEYS.into(),
/// }];
/// let shown = effective_menu(&menu, true, false);
/// assert!(matches!(&shown[0], MenuItemSpec::Item { checked: Some(true), .. }));
/// ```
pub fn effective_menu(menu: &[MenuItemSpec], paused: bool, gate_on: bool) -> Vec<MenuItemSpec> {
    menu.iter()
        .map(|item| match item {
            MenuItemSpec::Item {
                label,
                enabled,
                checked: Some(_),
                icon_rgba,
                action_id,
            } if action_id == ACTION_TOGGLE_GLOBAL_HOTKEYS
                || action_id == ACTION_TOGGLE_FULLSCREEN_GATE =>
            {
                let state = if action_id == ACTION_TOGGLE_GLOBAL_HOTKEYS {
                    paused
                } else {
                    gate_on
                };
                MenuItemSpec::Item {
                    label: label.clone(),
                    enabled: *enabled,
                    checked: Some(state),
                    icon_rgba: icon_rgba.clone(),
                    action_id: action_id.clone(),
                }
            }
            other => other.clone(),
        })
        .collect()
}

/// 暂停状态下实际要注册的热键：暂停时只保留“暂停 / 恢复”开关本身。
///
/// # 参数
/// - `hotkeys`：快照里的热键表。
/// - `paused`：是否暂停。
///
/// # 返回
/// 要注册的热键（保持快照里的顺序）。
///
/// ```rust
/// use snow_host_protocol::spec::HotkeySpec;
/// use snow_tray_host::view::effective_hotkeys;
/// let key = |id: &str, stays| HotkeySpec {
///     id: id.into(), config_key: id.into(), mods: 0, vk: 0x70, text: "F1".into(),
///     on_release_id: None, stays_when_paused: stays,
/// };
/// let all = vec![key("a", false), key("pause", true)];
/// assert_eq!(effective_hotkeys(&all, true).len(), 1);
/// assert_eq!(effective_hotkeys(&all, false).len(), 2);
/// ```
pub fn effective_hotkeys(hotkeys: &[HotkeySpec], paused: bool) -> Vec<HotkeySpec> {
    hotkeys
        .iter()
        .filter(|hotkey| !paused || hotkey.stays_when_paused)
        .cloned()
        .collect()
}

/// 把逐条热键的注册结果合并成“每个配置键一条”的回报（同一配置键有多个热键时，任一失败即失败）。
///
/// # 参数
/// - `hotkeys`：注册时用的热键表。
/// - `outcomes`：与 `hotkeys` 一一对应的注册结果（`Err` 为失败原因）。
///
/// # 返回
/// 按配置键首次出现的顺序排列的结果；失败原因形如 `Ctrl+F1: 已被占用`，多个以 `; ` 连接。
///
/// ```rust
/// use snow_host_protocol::spec::HotkeySpec;
/// use snow_tray_host::view::aggregate_results;
/// let hk = |key: &str, text: &str| HotkeySpec {
///     id: "x".into(), config_key: key.into(), mods: 0, vk: 1, text: text.into(),
///     on_release_id: None, stays_when_paused: false,
/// };
/// let list = vec![hk("a", "F1"), hk("a", "F2"), hk("b", "F3")];
/// let results = aggregate_results(&list, &[Ok(()), Err("占用".into()), Ok(())]);
/// assert_eq!(results.len(), 2);
/// assert!(!results[0].ok);
/// assert!(results[1].ok);
/// ```
pub fn aggregate_results(
    hotkeys: &[HotkeySpec],
    outcomes: &[Result<(), String>],
) -> Vec<HotkeyResult> {
    let mut results: Vec<HotkeyResult> = Vec::new();
    for (hotkey, outcome) in hotkeys.iter().zip(outcomes) {
        let failure = outcome
            .as_ref()
            .err()
            .map(|reason| format!("{}: {reason}", hotkey.text));
        match results
            .iter_mut()
            .find(|r| r.config_key == hotkey.config_key)
        {
            Some(existing) => {
                if let Some(failure) = failure {
                    existing.ok = false;
                    existing.error = Some(match existing.error.take() {
                        Some(previous) => format!("{previous}; {failure}"),
                        None => failure,
                    });
                }
            }
            None => results.push(HotkeyResult {
                config_key: hotkey.config_key.clone(),
                ok: failure.is_none(),
                error: failure,
            }),
        }
    }
    results
}

/// 汇总全部失败原因（给“恢复热键时有失败”的提示用）；没有失败返回空串。
///
/// # 参数
/// - `results`：[`aggregate_results`] 的结果。
pub fn describe_failures(results: &[HotkeyResult]) -> String {
    results
        .iter()
        .filter_map(|r| r.error.as_deref())
        .collect::<Vec<_>>()
        .join("; ")
}

/// 把“失败原因”填进提示文案的占位符。
///
/// # 参数
/// - `template`：快照里的提示文案（含 [`PLACEHOLDER_FAILURES`]）。
/// - `failures`：失败原因文本。
///
/// ```rust
/// use snow_tray_host::view::fill_failures;
/// assert_eq!(fill_failures("failed: {failures}", "F1: busy"), "failed: F1: busy");
/// ```
pub fn fill_failures(template: &str, failures: &str) -> String {
    template.replace(PLACEHOLDER_FAILURES, failures)
}

/// 悬停提示里放一条提示：`产品名: 文案`，超长截断。
///
/// # 参数
/// - `base`：常态悬停提示（产品名）。
/// - `text`：提示文案。
///
/// ```rust
/// use snow_tray_host::view::compose_tooltip;
/// assert_eq!(compose_tooltip("Cisox", "paused"), "Cisox: paused");
/// assert_eq!(compose_tooltip("Cisox", &"x".repeat(500)).chars().count(), 100);
/// ```
pub fn compose_tooltip(base: &str, text: &str) -> String {
    format!("{base}: {text}")
        .chars()
        .take(NOTICE_TOOLTIP_MAX_CHARS)
        .collect()
}

/// 切换前台全屏闸门后要让 UI 落盘的配置项。
///
/// # 参数
/// - `enabled`：开关新状态。
///
/// # 返回
/// `(配置键, 值)`。
pub fn gate_config_change(enabled: bool) -> (&'static str, Value) {
    (CONFIG_KEY_DISABLE_ON_FULLSCREEN, Value::Bool(enabled))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 构造带复选框的开关项。
    fn toggle(action: &str, checked: bool) -> MenuItemSpec {
        MenuItemSpec::Item {
            label: action.to_string(),
            enabled: true,
            checked: Some(checked),
            icon_rgba: None,
            action_id: action.to_string(),
        }
    }

    /// 构造热键。
    fn hotkey(config_key: &str, text: &str, stays: bool) -> HotkeySpec {
        HotkeySpec {
            id: config_key.to_string(),
            config_key: config_key.to_string(),
            mods: 0,
            vk: 0x70,
            text: text.to_string(),
            on_release_id: None,
            stays_when_paused: stays,
        }
    }

    /// 兜底快照合法，只有“退出”一项，没有热键。
    #[test]
    fn fallback_spec_is_minimal_and_valid() {
        let spec = fallback_spec();
        assert!(spec.validate().is_ok());
        assert!(spec.hotkeys.is_empty());
        assert_eq!(spec.tray.menu.len(), 1);
        assert!(matches!(
            &spec.tray.menu[0],
            MenuItemSpec::Item { action_id, .. } if action_id == ACTION_QUIT
        ));
        assert_eq!(spec.rev, 0);
        assert_eq!(
            spec.tray.icon_rgba.rgba.len(),
            (FALLBACK_ICON_SIZE * FALLBACK_ICON_SIZE * 4) as usize
        );
    }

    /// 两个开关项的勾选取宿主状态；别的项、没有复选框的开关项原样保留。
    #[test]
    fn effective_menu_overrides_only_toggle_checks() {
        let plain = MenuItemSpec::Item {
            label: "capture".into(),
            enabled: true,
            checked: Some(true),
            icon_rgba: None,
            action_id: "capture".into(),
        };
        let no_checkbox = MenuItemSpec::Item {
            label: "pause".into(),
            enabled: true,
            checked: None,
            icon_rgba: None,
            action_id: ACTION_TOGGLE_GLOBAL_HOTKEYS.into(),
        };
        let menu = vec![
            toggle(ACTION_TOGGLE_GLOBAL_HOTKEYS, false),
            toggle(ACTION_TOGGLE_FULLSCREEN_GATE, false),
            MenuItemSpec::Sep,
            plain.clone(),
            no_checkbox.clone(),
        ];
        let shown = effective_menu(&menu, true, true);
        assert_eq!(shown[0], toggle(ACTION_TOGGLE_GLOBAL_HOTKEYS, true));
        assert_eq!(shown[1], toggle(ACTION_TOGGLE_FULLSCREEN_GATE, true));
        assert_eq!(shown[2], MenuItemSpec::Sep);
        assert_eq!(shown[3], plain);
        assert_eq!(shown[4], no_checkbox);
        let shown = effective_menu(&menu, false, true);
        assert_eq!(shown[0], toggle(ACTION_TOGGLE_GLOBAL_HOTKEYS, false));
        assert_eq!(shown[1], toggle(ACTION_TOGGLE_FULLSCREEN_GATE, true));
    }

    /// 暂停时只留“暂停 / 恢复”开关本身，顺序不变。
    #[test]
    fn paused_keeps_only_the_toggle_hotkey() {
        let all = vec![
            hotkey("a", "F1", false),
            hotkey("pause", "F2", true),
            hotkey("b", "F3", false),
        ];
        let kept = effective_hotkeys(&all, true);
        assert_eq!(kept.len(), 1);
        assert_eq!(kept[0].config_key, "pause");
        assert_eq!(effective_hotkeys(&all, false), all);
    }

    /// 同一配置键多个热键：任一失败即整体失败，失败原因按出现顺序拼接；成功的键不带错误。
    #[test]
    fn results_aggregate_per_config_key() {
        let list = vec![
            hotkey("a", "F1", false),
            hotkey("a", "F2", false),
            hotkey("a", "F4", false),
            hotkey("b", "F3", false),
        ];
        let results = aggregate_results(
            &list,
            &[
                Err("占用".to_string()),
                Ok(()),
                Err("无效".to_string()),
                Ok(()),
            ],
        );
        assert_eq!(results.len(), 2);
        assert_eq!(results[0].config_key, "a");
        assert!(!results[0].ok);
        assert_eq!(results[0].error.as_deref(), Some("F1: 占用; F4: 无效"));
        assert_eq!(results[1].config_key, "b");
        assert!(results[1].ok);
        assert_eq!(results[1].error, None);
        assert_eq!(describe_failures(&results), "F1: 占用; F4: 无效");
        assert_eq!(describe_failures(&results[1..]), "");
    }

    /// 占位符替换与悬停提示截断。
    #[test]
    fn tooltip_and_placeholder_helpers() {
        assert_eq!(fill_failures("x {failures} y", "boom"), "x boom y");
        assert_eq!(fill_failures("no placeholder", "boom"), "no placeholder");
        assert_eq!(compose_tooltip("Cisox", "ok"), "Cisox: ok");
        let long = compose_tooltip("Cisox", &"字".repeat(300));
        assert_eq!(long.chars().count(), NOTICE_TOOLTIP_MAX_CHARS);
    }

    /// 闸门开关落配置用的键与值。
    #[test]
    fn gate_config_change_uses_protocol_key() {
        let (key, value) = gate_config_change(true);
        assert_eq!(key, CONFIG_KEY_DISABLE_ON_FULLSCREEN);
        assert_eq!(value, Value::Bool(true));
    }
}
