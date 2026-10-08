//! 贴图窗口键位表：按 C++ `pin_to_screen_shortcuts/*`（15 键）解析配置，把一次按键映射成贴图动作。
//! 纯逻辑，不接触界面；缺键时由配置层补 C++ 默认值。组合解析与覆盖窗共用 [`Chord`]。

use crate::app_runtime::shortcut_strings;
use crate::overlay_keymap::{Chord, Dir};
use snow_config::document::ConfigDocument;

/// 贴图窗口内的键位动作。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PinKeyAction {
    /// 复制（含标注的）图像。
    CopyToClipboard,
    /// 复制不含标注的原图。
    CopyOriginal,
    /// 另存为文件。
    SaveAsFile,
    /// 显示识别出的文字。
    ShowTextRecognition,
    /// 进入 / 退出二次标注模式。
    DrawingMode,
    /// 缩放到指定尺寸。
    ResizeWindow,
    /// 隐藏到屏幕顶部。
    HideToTop,
    /// 切换点击穿透。
    ToggleClickThrough,
    /// 切换缩略图模式。
    ThumbnailMode,
    /// 关闭窗口（退出标注模式优先）。
    CloseWindow,
    /// 销毁窗口（无视标注模式直接关闭）。
    DestroyWindow,
    /// 系统鼠标指针微移 1 像素。
    MoveCursor(Dir),
}

/// 贴图键位表：配置键 → 动作，顺序即优先级。
const PIN_KEYS: &[(&str, PinKeyAction)] = &[
    (
        "pin_to_screen_shortcuts/copy_to_clipboard",
        PinKeyAction::CopyToClipboard,
    ),
    (
        "pin_to_screen_shortcuts/copy_original_content",
        PinKeyAction::CopyOriginal,
    ),
    (
        "pin_to_screen_shortcuts/save_as_file",
        PinKeyAction::SaveAsFile,
    ),
    (
        "pin_to_screen_shortcuts/show_text_recognition_results",
        PinKeyAction::ShowTextRecognition,
    ),
    (
        "pin_to_screen_shortcuts/drawing_mode",
        PinKeyAction::DrawingMode,
    ),
    (
        "pin_to_screen_shortcuts/resize_window",
        PinKeyAction::ResizeWindow,
    ),
    (
        "pin_to_screen_shortcuts/hide_to_top",
        PinKeyAction::HideToTop,
    ),
    (
        "pin_to_screen_shortcuts/toggle_click_through",
        PinKeyAction::ToggleClickThrough,
    ),
    (
        "pin_to_screen_shortcuts/thumbnail_mode",
        PinKeyAction::ThumbnailMode,
    ),
    (
        "pin_to_screen_shortcuts/close_window",
        PinKeyAction::CloseWindow,
    ),
    (
        "pin_to_screen_shortcuts/destroy_window",
        PinKeyAction::DestroyWindow,
    ),
    (
        "pin_to_screen_shortcuts/move_cursor_up",
        PinKeyAction::MoveCursor(Dir::Up),
    ),
    (
        "pin_to_screen_shortcuts/move_cursor_down",
        PinKeyAction::MoveCursor(Dir::Down),
    ),
    (
        "pin_to_screen_shortcuts/move_cursor_left",
        PinKeyAction::MoveCursor(Dir::Left),
    ),
    (
        "pin_to_screen_shortcuts/move_cursor_right",
        PinKeyAction::MoveCursor(Dir::Right),
    ),
];

/// 贴图键位表（由配置构造）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PinKeymap {
    /// 按优先级排列的 `(组合, 动作)`。
    bindings: Vec<(Chord, PinKeyAction)>,
}

impl PinKeymap {
    /// 从配置文档构造；缺键用 schema 默认值。
    ///
    /// # 参数
    /// - `document`：配置文档。
    ///
    /// ```ignore
    /// let map = PinKeymap::from_document(&document);
    /// assert_eq!(map.resolve("escape", false, false, false), Some(PinKeyAction::CloseWindow));
    /// ```
    pub fn from_document(document: &ConfigDocument) -> Self {
        let mut bindings = Vec::new();
        for (key, action) in PIN_KEYS {
            for text in shortcut_strings(&document.value(key)) {
                if let Some(chord) = Chord::parse(&text) {
                    bindings.push((chord, *action));
                }
            }
        }
        Self { bindings }
    }

    /// 解析一次按键。
    ///
    /// # 参数
    /// - `key`：GPUI 键名（小写）。
    /// - `ctrl` / `shift` / `alt`：修饰键状态。
    ///
    /// # 返回
    /// 命中的动作；没有绑定返回 `None`。
    pub fn resolve(&self, key: &str, ctrl: bool, shift: bool, alt: bool) -> Option<PinKeyAction> {
        self.bindings
            .iter()
            .find(|(chord, _)| chord.matches(key, ctrl, shift, alt))
            .map(|(_, action)| *action)
    }
}

impl Default for PinKeymap {
    /// 全部默认值（C++ 键位表）。
    fn default() -> Self {
        Self::from_document(&ConfigDocument::from_bytes(None))
    }
}

/// 键位表里涉及的全部配置键（供测试核对 15 键都已接线）。
pub fn wired_config_keys() -> Vec<&'static str> {
    PIN_KEYS.iter().map(|(k, _)| *k).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 默认键位与 C++ 键位表一致。
    #[test]
    fn default_map_matches_cpp_table() {
        use PinKeyAction::*;
        let map = PinKeymap::default();
        let cases: &[(&str, bool, bool, PinKeyAction)] = &[
            ("c", true, false, CopyToClipboard),
            ("c", true, true, CopyOriginal),
            ("s", true, false, SaveAsFile),
            ("d", true, false, ShowTextRecognition),
            ("space", false, false, DrawingMode),
            ("m", false, false, ResizeWindow),
            ("h", false, false, HideToTop),
            ("m", true, false, ToggleClickThrough),
            ("r", false, false, ThumbnailMode),
            ("escape", false, false, CloseWindow),
            ("escape", false, true, DestroyWindow),
            ("w", false, false, MoveCursor(Dir::Up)),
            ("down", false, false, MoveCursor(Dir::Down)),
            ("a", false, false, MoveCursor(Dir::Left)),
            ("right", false, false, MoveCursor(Dir::Right)),
        ];
        for (key, ctrl, shift, want) in cases {
            assert_eq!(map.resolve(key, *ctrl, *shift, false), Some(*want), "{key}");
        }
        assert_eq!(map.resolve("q", false, false, false), None);
    }

    /// 改键生效、15 键都有默认值。
    #[test]
    fn config_overrides_and_all_keys_wired() {
        let mut doc = ConfigDocument::from_bytes(None);
        doc.set_value(
            "pin_to_screen_shortcuts/close_window",
            serde_json::json!([{"portable": "Q"}]),
        )
        .unwrap();
        let map = PinKeymap::from_document(&doc);
        assert_eq!(
            map.resolve("q", false, false, false),
            Some(PinKeyAction::CloseWindow)
        );
        assert_eq!(map.resolve("escape", false, false, false), None);
        let defaults = ConfigDocument::from_bytes(None);
        for key in wired_config_keys() {
            assert!(
                !shortcut_strings(&defaults.value(key)).is_empty(),
                "{key} 缺默认值"
            );
        }
        assert_eq!(wired_config_keys().len(), 15);
    }
}
