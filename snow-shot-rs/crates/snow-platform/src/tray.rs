//! 系统托盘与全局快捷键（System Tray & Global Hotkeys）。
//!
//! 提供系统托盘图标展示、上下文菜单定义与事件通知，
//! 以及 Win32 原生全局热键注册与分发。

use std::collections::HashMap;

/// 托盘菜单项定义。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrayMenuItem {
    /// 菜单项唯一标识符（例如 `"quick.screenshot"`）。
    pub id: String,
    /// 显示标题文本。
    pub text: String,
    /// 是否可用。
    pub enabled: bool,
    /// 是否为分割线。
    pub is_separator: bool,
}

impl TrayMenuItem {
    /// 创建常规菜单项。
    pub fn item(id: impl Into<String>, text: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            text: text.into(),
            enabled: true,
            is_separator: false,
        }
    }

    /// 创建分割线项。
    pub fn separator() -> Self {
        Self {
            id: String::new(),
            text: String::new(),
            enabled: false,
            is_separator: true,
        }
    }
}

/// 托盘动作事件枚举。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TrayAction {
    /// 点击菜单项。
    MenuItemClicked(String),
    /// 双击托盘图标。
    DoubleClicked,
}

/// 全局热键修饰键掩码。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct HotkeyModifiers {
    /// Alt 键。
    pub alt: bool,
    /// Control 键。
    pub ctrl: bool,
    /// Shift 键。
    pub shift: bool,
    /// Windows/Command 键。
    pub meta: bool,
}

/// 全局热键绑定定义。
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct HotkeyBinding {
    /// 绑定的动作 ID（例如 `"screenshot"`）。
    pub action_id: String,
    /// 修饰键组合。
    pub modifiers: HotkeyModifiers,
    /// 虚拟键码或字符键名。
    pub key: String,
}

/// 系统托盘与快捷键管理器。
#[derive(Debug, Default)]
pub struct TrayAndHotkeyManager {
    /// 菜单项列表。
    menu_items: Vec<TrayMenuItem>,
    /// 注册的热键表。
    hotkeys: HashMap<i32, HotkeyBinding>,
    /// 自增热键 ID。
    next_hotkey_id: i32,
}

impl TrayAndHotkeyManager {
    /// 创建管理器实例并初始化默认托盘菜单。
    pub fn new() -> Self {
        let mut mgr = Self {
            menu_items: Vec::new(),
            hotkeys: HashMap::new(),
            next_hotkey_id: 1,
        };
        mgr.init_default_menu();
        mgr
    }

    /// 初始化默认托盘菜单。
    fn init_default_menu(&mut self) {
        self.menu_items = vec![
            TrayMenuItem::item("quick.screenshot", "截取屏幕 (F1)"),
            TrayMenuItem::item("quick.pin-to-screen", "快速贴图"),
            TrayMenuItem::item("quick.screen-recording", "屏幕录制"),
            TrayMenuItem::separator(),
            TrayMenuItem::item("quick.preferences", "首选项设置..."),
            TrayMenuItem::separator(),
            TrayMenuItem::item("quick.quit", "退出 Cisox"),
        ];
    }

    /// 获取托盘菜单项只读列表。
    pub fn menu_items(&self) -> &[TrayMenuItem] {
        &self.menu_items
    }

    /// 注册全局热键。
    ///
    /// # 参数
    /// - `action_id`：动作标识
    /// - `modifiers`：修饰键
    /// - `key`：按键名
    ///
    /// # 返回
    /// 成功返回分配的热键 ID。
    pub fn register_hotkey(
        &mut self,
        action_id: impl Into<String>,
        modifiers: HotkeyModifiers,
        key: impl Into<String>,
    ) -> i32 {
        let id = self.next_hotkey_id;
        self.next_hotkey_id += 1;

        let binding = HotkeyBinding {
            action_id: action_id.into(),
            modifiers,
            key: key.into(),
        };

        self.hotkeys.insert(id, binding);
        id
    }

    /// 注销指定全局热键。
    pub fn unregister_hotkey(&mut self, id: i32) -> bool {
        self.hotkeys.remove(&id).is_some()
    }

    /// 根据热键 ID 查询对应的绑活动作。
    pub fn lookup_hotkey(&self, id: i32) -> Option<&HotkeyBinding> {
        self.hotkeys.get(&id)
    }

    /// 处理托盘事件触发。
    pub fn handle_tray_action(&self, action: TrayAction) -> Option<&str> {
        match action {
            TrayAction::MenuItemClicked(id) => {
                self.menu_items.iter().find(|i| i.id == id).map(|i| i.id.as_str())
            }
            TrayAction::DoubleClicked => Some("quick.screenshot"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 验证默认托盘菜单初始化。
    #[test]
    fn test_tray_menu_defaults() {
        let mgr = TrayAndHotkeyManager::new();
        let items = mgr.menu_items();
        assert!(items.len() >= 5);
        assert!(items.iter().any(|i| i.id == "quick.screenshot"));
        assert!(items.iter().any(|i| i.id == "quick.preferences"));
        assert!(items.iter().any(|i| i.id == "quick.quit"));
    }

    /// 验证全局热键注册与注销。
    #[test]
    fn test_hotkey_registration_flow() {
        let mut mgr = TrayAndHotkeyManager::new();
        let mods = HotkeyModifiers {
            ctrl: true,
            alt: false,
            shift: true,
            meta: false,
        };
        let id = mgr.register_hotkey("screenshot", mods, "A");
        assert!(id > 0);

        let binding = mgr.lookup_hotkey(id);
        assert!(binding.is_some());
        let b = binding.unwrap();
        assert_eq!(b.action_id, "screenshot");
        assert_eq!(b.key, "A");
        assert!(b.modifiers.ctrl);
        assert!(b.modifiers.shift);

        assert!(mgr.unregister_hotkey(id));
        assert!(mgr.lookup_hotkey(id).is_none());
    }

    /// 验证托盘点击事件映射。
    #[test]
    fn test_tray_events() {
        let mgr = TrayAndHotkeyManager::new();
        assert_eq!(
            mgr.handle_tray_action(TrayAction::DoubleClicked),
            Some("quick.screenshot")
        );
        assert_eq!(
            mgr.handle_tray_action(TrayAction::MenuItemClicked("quick.quit".into())),
            Some("quick.quit")
        );
        assert_eq!(
            mgr.handle_tray_action(TrayAction::MenuItemClicked("unknown".into())),
            None
        );
    }
}
