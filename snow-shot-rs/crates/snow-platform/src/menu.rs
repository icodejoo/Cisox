//! 原生弹出菜单（Native Popup Menu）。
//!
//! 贴图窗口可能只有几十像素大，窗口内放不下右键菜单，所以用系统原生的弹出菜单：
//! 它是独立的顶层窗口，不受触发窗口尺寸限制。Windows 走 `TrackPopupMenuEx`，其它平台降级为不弹出。

/// 菜单项 ID 的保留值：表示用户没有选择任何项。
pub const NO_SELECTION: u32 = 0;

/// 一个可点击的菜单项。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MenuItem {
    /// 菜单项 ID（必须非 0，且在同一菜单内唯一）。
    pub id: u32,
    /// 显示文本。
    pub label: String,
    /// 是否显示勾选标记。
    pub checked: bool,
    /// 是否可点击（`false` 时置灰）。
    pub enabled: bool,
}

impl MenuItem {
    /// 创建一个启用、未勾选的菜单项。
    ///
    /// # 参数
    /// - `id`：菜单项 ID（非 0）。
    /// - `label`：显示文本。
    ///
    /// ```
    /// use snow_platform::menu::MenuItem;
    /// let item = MenuItem::new(1, "复制");
    /// assert!(item.enabled && !item.checked);
    /// ```
    pub fn new(id: u32, label: impl Into<String>) -> Self {
        Self {
            id,
            label: label.into(),
            checked: false,
            enabled: true,
        }
    }

    /// 设置勾选状态。
    pub fn checked(mut self, checked: bool) -> Self {
        self.checked = checked;
        self
    }

    /// 设置是否可点击。
    pub fn enabled(mut self, enabled: bool) -> Self {
        self.enabled = enabled;
        self
    }
}

/// 菜单里的一行：菜单项或分隔线。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MenuEntry {
    /// 菜单项。
    Item(MenuItem),
    /// 分隔线。
    Separator,
}

/// 校验菜单描述：ID 非 0 且不重复、文本非空。
///
/// # 参数
/// - `entries`：菜单行。
///
/// # 返回
/// 合法返回 `Ok(())`，否则返回问题描述。
///
/// ```
/// use snow_platform::menu::{MenuEntry, MenuItem, validate_entries};
/// let ok = [MenuEntry::Item(MenuItem::new(1, "a")), MenuEntry::Separator];
/// assert!(validate_entries(&ok).is_ok());
/// let dup = [MenuEntry::Item(MenuItem::new(1, "a")), MenuEntry::Item(MenuItem::new(1, "b"))];
/// assert!(validate_entries(&dup).is_err());
/// ```
pub fn validate_entries(entries: &[MenuEntry]) -> Result<(), String> {
    let mut seen = std::collections::BTreeSet::new();
    for entry in entries {
        if let MenuEntry::Item(item) = entry {
            if item.id == NO_SELECTION {
                return Err("菜单项 ID 不能为 0".to_string());
            }
            if item.label.trim().is_empty() {
                return Err("菜单项文本不能为空".to_string());
            }
            if !seen.insert(item.id) {
                return Err(format!("菜单项 ID 重复: {}", item.id));
            }
        }
    }
    Ok(())
}

/// 在屏幕坐标处弹出菜单并等待用户选择（阻塞，内部有模态消息循环）。
///
/// 调用方必须在 UI 线程、且不能持有会被窗口消息重入访问的借用（例如放进异步任务里调用）。
///
/// # 参数
/// - `owner`：拥有者窗口句柄（Windows 为 `HWND` 的整数值）。
/// - `x` / `y`：弹出位置（屏幕物理像素）。
/// - `entries`：菜单行。
///
/// # 返回
/// - `Ok(Some(id))`：用户选择了某项。
/// - `Ok(None)`：用户取消（点到菜单外 / Esc）或当前平台不支持。
/// - `Err(..)`：菜单描述非法或系统调用失败。
///
/// # 示例
/// ```no_run
/// use snow_platform::menu::{MenuEntry, MenuItem, show_popup_menu};
/// let entries = [MenuEntry::Item(MenuItem::new(1, "关闭"))];
/// let _choice = show_popup_menu(0, 100, 100, &entries);
/// ```
pub fn show_popup_menu(
    owner: isize,
    x: i32,
    y: i32,
    entries: &[MenuEntry],
) -> Result<Option<u32>, String> {
    validate_entries(entries)?;
    #[cfg(windows)]
    {
        win::show(owner, x, y, entries)
    }
    #[cfg(not(windows))]
    {
        let _ = (owner, x, y);
        Ok(None)
    }
}

#[cfg(windows)]
mod win {
    use super::{MenuEntry, NO_SELECTION};
    use std::ffi::c_void;
    use windows::Win32::Foundation::{HWND, POINT};
    use windows::Win32::UI::WindowsAndMessaging::{
        AppendMenuW, CreatePopupMenu, DestroyMenu, HMENU, MF_CHECKED, MF_GRAYED, MF_SEPARATOR,
        MF_STRING, PostMessageW, SetForegroundWindow, TPM_NONOTIFY, TPM_RETURNCMD, TPM_RIGHTBUTTON,
        TrackPopupMenuEx, WM_NULL,
    };
    use windows::core::PCWSTR;

    /// 菜单句柄的 RAII 包装，离开作用域时销毁。
    struct MenuGuard(HMENU);

    impl Drop for MenuGuard {
        /// 销毁菜单句柄。
        fn drop(&mut self) {
            // SAFETY: 句柄由 CreatePopupMenu 创建且仅在此销毁一次。
            unsafe {
                let _ = DestroyMenu(self.0);
            }
        }
    }

    /// 把文本转成以 0 结尾的 UTF-16。
    fn to_wide(text: &str) -> Vec<u16> {
        text.encode_utf16().chain(std::iter::once(0)).collect()
    }

    /// 创建菜单、弹出并返回选择结果。
    pub(super) fn show(
        owner: isize,
        x: i32,
        y: i32,
        entries: &[MenuEntry],
    ) -> Result<Option<u32>, String> {
        let hwnd = HWND(owner as *mut c_void);
        // SAFETY: 全部为句柄 / 值参数；菜单由 MenuGuard 负责销毁，宽字符串在调用期间存活。
        unsafe {
            let menu =
                MenuGuard(CreatePopupMenu().map_err(|e| format!("CreatePopupMenu 失败: {e}"))?);
            for entry in entries {
                match entry {
                    MenuEntry::Separator => {
                        AppendMenuW(menu.0, MF_SEPARATOR, 0, PCWSTR::null())
                            .map_err(|e| format!("AppendMenuW 失败: {e}"))?;
                    }
                    MenuEntry::Item(item) => {
                        let mut flags = MF_STRING;
                        if item.checked {
                            flags |= MF_CHECKED;
                        }
                        if !item.enabled {
                            flags |= MF_GRAYED;
                        }
                        let text = to_wide(&item.label);
                        AppendMenuW(menu.0, flags, item.id as usize, PCWSTR(text.as_ptr()))
                            .map_err(|e| format!("AppendMenuW 失败: {e}"))?;
                    }
                }
            }
            // 弹出菜单前窗口必须是前台窗口，否则点菜单外部菜单不会关闭
            let _ = SetForegroundWindow(hwnd);
            let point = POINT { x, y };
            let picked = TrackPopupMenuEx(
                menu.0,
                (TPM_RETURNCMD | TPM_RIGHTBUTTON | TPM_NONOTIFY).0,
                point.x,
                point.y,
                hwnd,
                None,
            );
            // 官方建议：菜单关闭后给拥有者投递一条空消息，避免下次点击时菜单立即消失
            let _ = PostMessageW(Some(hwnd), WM_NULL, Default::default(), Default::default());
            let id = picked.0 as u32;
            Ok((id != NO_SELECTION).then_some(id))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 合法菜单通过校验，分隔线不受 ID 规则约束。
    #[test]
    fn valid_menu_passes() {
        let entries = [
            MenuEntry::Item(MenuItem::new(1, "复制")),
            MenuEntry::Separator,
            MenuEntry::Separator,
            MenuEntry::Item(MenuItem::new(2, "关闭").checked(true).enabled(false)),
        ];
        assert!(validate_entries(&entries).is_ok());
        assert!(validate_entries(&[]).is_ok());
    }

    /// ID 为 0、重复、文本为空都会被拒绝。
    #[test]
    fn invalid_menu_rejected() {
        let zero = [MenuEntry::Item(MenuItem::new(0, "a"))];
        assert!(validate_entries(&zero).is_err());
        let dup = [
            MenuEntry::Item(MenuItem::new(3, "a")),
            MenuEntry::Item(MenuItem::new(3, "b")),
        ];
        assert!(validate_entries(&dup).is_err());
        let blank = [MenuEntry::Item(MenuItem::new(4, "  "))];
        assert!(validate_entries(&blank).is_err());
    }

    /// 非法菜单在弹出前就返回错误（不会触碰系统 API）。
    #[test]
    fn popup_rejects_invalid_before_native_call() {
        let zero = [MenuEntry::Item(MenuItem::new(0, "a"))];
        assert!(show_popup_menu(0, 0, 0, &zero).is_err());
    }

    /// 构造器的默认值与链式设置。
    #[test]
    fn item_builder_defaults() {
        let item = MenuItem::new(9, "x");
        assert!(item.enabled && !item.checked);
        let item = item.checked(true).enabled(false);
        assert!(item.checked && !item.enabled);
    }
}
