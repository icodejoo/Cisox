//! 把快照里的数据转成 `snow-shell-core` 的托盘 / 热键类型（纯函数，可离屏测试）。
//!
//! 宿主里动作负载是动作 id 字符串：托盘菜单项、点击、热键触发后都只带一个 id 出来，
//! 由核心决定自己处理还是转给 UI。

use crate::host::TrayPlan;
use snow_host_protocol::message::ActionOrigin;
use snow_host_protocol::spec::{HotkeySpec, IconRgba, MenuItemSpec};
use snow_shell_core::dispatch::ActionOrigin as ShellOrigin;
use snow_shell_core::hotkey::{Hotkey, HotkeyBinding};
use snow_shell_core::tray::{TrayAction, TrayIconImage, TrayMenuEntry, TraySpec};

/// 快照图标转托盘图标。
///
/// # 参数
/// - `icon`：快照里的图标（RGBA 像素）。
///
/// # 返回
/// 托盘图标；像素长度与尺寸不符返回错误文本。
fn tray_icon(icon: &IconRgba) -> Result<TrayIconImage, String> {
    TrayIconImage::new(icon.rgba.clone(), icon.width, icon.height).map_err(|e| e.to_string())
}

/// 快照菜单项转托盘菜单项（动作负载是动作 id）。
fn menu_entry(item: &MenuItemSpec) -> Result<TrayMenuEntry<String>, String> {
    match item {
        MenuItemSpec::Sep => Ok(TrayMenuEntry::Separator),
        MenuItemSpec::Item {
            label,
            enabled,
            checked,
            icon_rgba,
            action_id,
        } => Ok(TrayMenuEntry::Item {
            label: label.clone(),
            enabled: *enabled,
            checked: *checked,
            icon: icon_rgba.as_ref().map(tray_icon).transpose()?,
            action: TrayAction::Command(action_id.clone()),
        }),
    }
}

/// 快照里的菜单转托盘菜单（整体替换菜单用）。
///
/// # 参数
/// - `menu`：已叠上宿主状态的菜单。
///
/// # 返回
/// 托盘菜单项；任一项的图标像素非法就整体失败。
pub fn tray_menu(menu: &[MenuItemSpec]) -> Result<Vec<TrayMenuEntry<String>>, String> {
    menu.iter().map(menu_entry).collect()
}

/// 托盘计划转托盘描述（创建托盘用）。
///
/// # 参数
/// - `plan`：要显示的托盘。
///
/// # 返回
/// 托盘描述；图标像素非法返回错误文本。
///
/// ```rust
/// use snow_host_protocol::spec::IconRgba;
/// use snow_tray_host::convert::tray_spec_from_plan;
/// use snow_tray_host::host::TrayPlan;
/// let plan = TrayPlan {
///     icon: IconRgba { width: 1, height: 1, rgba: vec![0; 4] },
///     tooltip: "Cisox".into(),
///     menu: vec![],
///     on_left: Some("capture".into()),
///     on_middle: None,
///     on_double: None,
/// };
/// let spec = tray_spec_from_plan(&plan).unwrap();
/// assert_eq!(spec.tooltip, "Cisox");
/// assert!(spec.on_left_click.is_some());
/// ```
pub fn tray_spec_from_plan(plan: &TrayPlan) -> Result<TraySpec<String>, String> {
    let click = |id: &Option<String>| id.clone().map(TrayAction::Command);
    Ok(TraySpec {
        tooltip: plan.tooltip.clone(),
        icon: tray_icon(&plan.icon)?,
        menu: tray_menu(&plan.menu)?,
        on_left_click: click(&plan.on_left),
        on_double_click: click(&plan.on_double),
        on_middle_click: click(&plan.on_middle),
    })
}

/// 快照热键转热键绑定：按下触发 `id`，若有 `on_release_id` 则松开时触发它（按住说话）。
///
/// # 参数
/// - `hotkey`：快照里的热键（用规范化文本还原，宿主不再自带另一套解析）。
///
/// # 返回
/// 绑定；文本无法解析返回错误文本（该热键按注册失败处理）。
///
/// ```rust
/// use snow_host_protocol::spec::HotkeySpec;
/// use snow_tray_host::convert::binding_from_spec;
/// let spec = HotkeySpec {
///     id: "start_dictation".into(), config_key: "k".into(), mods: 0, vk: 0x78,
///     text: "F9".into(), on_release_id: Some("stop_dictation".into()), stays_when_paused: false,
/// };
/// let binding = binding_from_spec(&spec).unwrap();
/// assert_eq!(binding.command, "start_dictation");
/// assert_eq!(binding.on_release.as_deref(), Some("stop_dictation"));
/// ```
pub fn binding_from_spec(hotkey: &HotkeySpec) -> Result<HotkeyBinding<String>, String> {
    let parsed = Hotkey::parse(&hotkey.text).map_err(|e| e.to_string())?;
    let binding = HotkeyBinding::new(parsed, hotkey.id.clone());
    Ok(match &hotkey.on_release_id {
        Some(release) => binding.with_release(release.clone()),
        None => binding,
    })
}

/// 后端的触发来源转协议里的来源标签。
///
/// # 参数
/// - `origin`：`snow-shell-core` 派发器给出的来源。
pub fn origin_from_shell(origin: ShellOrigin) -> ActionOrigin {
    match origin {
        ShellOrigin::Hotkey => ActionOrigin::Hotkey,
        ShellOrigin::Tray => ActionOrigin::Tray,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use snow_host_protocol::{ACTION_QUIT, ACTION_TOGGLE_GLOBAL_HOTKEYS};

    /// 构造托盘计划。
    fn plan(menu: Vec<MenuItemSpec>) -> TrayPlan {
        TrayPlan {
            icon: IconRgba {
                width: 2,
                height: 2,
                rgba: vec![7; 16],
            },
            tooltip: "Cisox".into(),
            menu,
            on_left: Some("capture".into()),
            on_middle: Some("pin_clipboard".into()),
            on_double: None,
        }
    }

    /// 菜单项、分隔线、复选框、图标、点击动作都原样带过去，动作负载是 id。
    #[test]
    fn plan_converts_to_tray_spec() {
        let menu = vec![
            MenuItemSpec::Item {
                label: "截图".into(),
                enabled: true,
                checked: None,
                icon_rgba: Some(IconRgba {
                    width: 1,
                    height: 1,
                    rgba: vec![1, 2, 3, 4],
                }),
                action_id: "capture".into(),
            },
            MenuItemSpec::Sep,
            MenuItemSpec::Item {
                label: "暂停".into(),
                enabled: false,
                checked: Some(true),
                icon_rgba: None,
                action_id: ACTION_TOGGLE_GLOBAL_HOTKEYS.into(),
            },
            MenuItemSpec::Item {
                label: "退出".into(),
                enabled: true,
                checked: None,
                icon_rgba: None,
                action_id: ACTION_QUIT.into(),
            },
        ];
        let spec = tray_spec_from_plan(&plan(menu)).unwrap();
        assert_eq!(spec.tooltip, "Cisox");
        assert_eq!(spec.icon.size(), (2, 2));
        assert_eq!(spec.menu.len(), 4);
        assert_eq!(spec.menu[1], TrayMenuEntry::Separator);
        match &spec.menu[0] {
            TrayMenuEntry::Item {
                label,
                icon,
                action,
                checked,
                ..
            } => {
                assert_eq!(label, "截图");
                assert_eq!(icon.as_ref().map(TrayIconImage::size), Some((1, 1)));
                assert_eq!(action, &TrayAction::Command("capture".to_string()));
                assert_eq!(*checked, None);
            }
            other => panic!("意外的菜单项 {other:?}"),
        }
        match &spec.menu[2] {
            TrayMenuEntry::Item {
                enabled, checked, ..
            } => {
                assert!(!enabled);
                assert_eq!(*checked, Some(true));
            }
            other => panic!("意外的菜单项 {other:?}"),
        }
        assert_eq!(
            spec.on_left_click,
            Some(TrayAction::Command("capture".to_string()))
        );
        assert_eq!(
            spec.on_middle_click,
            Some(TrayAction::Command("pin_clipboard".to_string()))
        );
        assert_eq!(spec.on_double_click, None);
    }

    /// 图标像素长度与尺寸不符：整份转换失败而不是 panic。
    #[test]
    fn bad_icon_fails_conversion() {
        let mut bad = plan(vec![]);
        bad.icon.rgba.pop();
        assert!(tray_spec_from_plan(&bad).is_err());
        let bad_item = plan(vec![MenuItemSpec::Item {
            label: "x".into(),
            enabled: true,
            checked: None,
            icon_rgba: Some(IconRgba {
                width: 4,
                height: 4,
                rgba: vec![0; 3],
            }),
            action_id: "x".into(),
        }]);
        assert!(tray_spec_from_plan(&bad_item).is_err());
    }

    /// 热键：规范化文本能还原，按住说话带松开动作；非法文本报错。
    #[test]
    fn hotkey_bindings_parse_canonical_text() {
        let spec = |text: &str, release: Option<&str>| HotkeySpec {
            id: "capture".into(),
            config_key: "k".into(),
            mods: 0,
            vk: 1,
            text: text.into(),
            on_release_id: release.map(str::to_string),
            stays_when_paused: false,
        };
        let plain = binding_from_spec(&spec("Ctrl+Alt+S", None)).unwrap();
        assert_eq!(plain.command, "capture");
        assert!(plain.on_release.is_none());
        assert_eq!(plain.hotkey.to_string(), "Ctrl+Alt+S");
        let hold = binding_from_spec(&spec("F9", Some("stop"))).unwrap();
        assert_eq!(hold.on_release.as_deref(), Some("stop"));
        assert!(binding_from_spec(&spec("Ctrl+Banana", None)).is_err());
        assert!(binding_from_spec(&spec("", None)).is_err());
    }

    /// 来源转换。
    #[test]
    fn origins_map() {
        assert_eq!(origin_from_shell(ShellOrigin::Hotkey), ActionOrigin::Hotkey);
        assert_eq!(origin_from_shell(ShellOrigin::Tray), ActionOrigin::Tray);
    }
}
