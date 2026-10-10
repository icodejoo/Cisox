//! 跨视图共享的按键动作表：取消/关闭（Dismiss）与粘贴（Paste）。
//! 纯逻辑，不接触界面；各视图用 [`resolve`] 把按键映射成动作，不再自己比较按键字符串。
//! 以后做重绑或"输入法组合中不关闭"之类的统一规则，只改这里。

/// 共享按键动作。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SharedKeyAction {
    /// 取消 / 关闭当前输入、弹窗或窗口。
    Dismiss,
    /// 粘贴剪贴板文本。
    Paste,
}

/// 取消/关闭的默认绑定主键（不限修饰键，与旧行为一致）。
const DISMISS_KEY: &str = "escape";
/// 粘贴的默认绑定主键（需配合 Ctrl）。
const PASTE_KEY: &str = "v";

/// 把一次按键解析成共享动作。
///
/// # 参数
/// - `key`：GPUI 键名，如 `escape`、`v`。
/// - `ctrl`：是否按住 Ctrl。
///
/// # 返回
/// 命中返回对应动作，否则 `None`。
///
/// ```ignore
/// assert_eq!(resolve("escape", false), Some(SharedKeyAction::Dismiss));
/// assert_eq!(resolve("v", true), Some(SharedKeyAction::Paste));
/// ```
pub fn resolve(key: &str, ctrl: bool) -> Option<SharedKeyAction> {
    if key == DISMISS_KEY {
        Some(SharedKeyAction::Dismiss)
    } else if ctrl && key == PASTE_KEY {
        Some(SharedKeyAction::Paste)
    } else {
        None
    }
}

/// 是否为取消/关闭动作。
///
/// # 参数
/// - `key`：GPUI 键名。
pub fn is_dismiss(key: &str) -> bool {
    resolve(key, false) == Some(SharedKeyAction::Dismiss)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Esc 无论是否带 Ctrl 都是 Dismiss。
    #[test]
    fn escape_is_dismiss() {
        assert_eq!(resolve("escape", false), Some(SharedKeyAction::Dismiss));
        assert_eq!(resolve("escape", true), Some(SharedKeyAction::Dismiss));
        assert!(is_dismiss("escape"));
    }

    /// 粘贴必须带 Ctrl，单独的 v 不触发。
    #[test]
    fn paste_needs_ctrl() {
        assert_eq!(resolve("v", true), Some(SharedKeyAction::Paste));
        assert_eq!(resolve("v", false), None);
        assert!(!is_dismiss("v"));
    }

    /// 其他键不映射。
    #[test]
    fn other_keys_unmapped() {
        assert_eq!(resolve("enter", true), None);
        assert_eq!(resolve("c", true), None);
    }
}
