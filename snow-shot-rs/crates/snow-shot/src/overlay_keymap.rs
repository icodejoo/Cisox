//! 覆盖窗键位表：按 C++ `screenshot_shortcuts/*`（27 键）与 `drawing_shortcuts/*`（10 键）解析配置，
//! 把一次按键映射成覆盖窗动作。纯逻辑，不接触界面；缺键时由配置层补 C++ 默认值。
//!
//! 不支持的写法（只有修饰键的绑定如 `Shift`、带 Win 键的绑定）会被跳过；
//! Enter 与 Shift+Z 重做是固定键位，不在配置里。

use crate::app_runtime::shortcut_strings;
use snow_config::document::ConfigDocument;

/// 光标微移方向。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Dir {
    /// 上。
    Up,
    /// 下。
    Down,
    /// 左。
    Left,
    /// 右。
    Right,
}

impl Dir {
    /// 方向对应的 1 像素位移 `(dx, dy)`。
    pub const fn delta(self) -> (i32, i32) {
        match self {
            Self::Up => (0, -1),
            Self::Down => (0, 1),
            Self::Left => (-1, 0),
            Self::Right => (1, 0),
        }
    }
}

/// 绘制工具快捷键（`drawing_shortcuts/*`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DrawingKey {
    /// 选择。
    Select,
    /// 形状。
    Shape,
    /// 箭头。
    Arrow,
    /// 画笔。
    Brush,
    /// 高亮。
    Highlight,
    /// 文字。
    Text,
    /// 序号。
    SerialNumber,
    /// 滤镜。
    Filter,
    /// 橡皮擦。
    Eraser,
    /// 水印。
    Watermark,
}

/// 覆盖窗内的键位动作。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OverlayKeyAction {
    /// 取消截图。
    Cancel,
    /// 复制到剪贴板。
    CopyToClipboard,
    /// 另存为文件。
    SaveAsFile,
    /// 贴到屏幕。
    PinToScreen,
    /// 开始录屏。
    VideoRecording,
    /// 文字识别。
    TextRecognition,
    /// 文字翻译。
    TextTranslation,
    /// 长截图。
    ScrollingScreenshot,
    /// 撤销。
    Undo,
    /// 重做。
    Redo,
    /// 复制光标处颜色。
    CopyColor,
    /// 切回移动（无标注工具）。
    MoveTool,
    /// 光标微移 1 像素。
    MoveCursor(Dir),
    /// 切换标注工具。
    Tool(DrawingKey),
    /// 尚未实现：携带配置键，用来取动作名给出提示。
    Unimplemented(&'static str),
}

/// 一个按键组合：主键（GPUI 小写键名）与修饰键。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Chord {
    /// 主键，如 `escape`、`c`、`up`。
    pub key: String,
    /// 是否要求 Ctrl。
    pub ctrl: bool,
    /// 是否要求 Shift。
    pub shift: bool,
    /// 是否要求 Alt。
    pub alt: bool,
}

/// 把主键写法规范成 GPUI 键名（小写，别名归一）。
fn normalize_key(raw: &str) -> String {
    let lower = raw.to_ascii_lowercase();
    match lower.as_str() {
        "esc" => "escape".into(),
        "return" => "enter".into(),
        "del" => "delete".into(),
        "pgup" => "pageup".into(),
        "pgdown" | "pgdn" => "pagedown".into(),
        _ => lower,
    }
}

impl Chord {
    /// 解析 `Ctrl+Shift+S` 这类写法。
    ///
    /// # 参数
    /// - `text`：配置里的快捷键文本。
    ///
    /// # 返回
    /// 组合；只有修饰键、含 Win/Meta 键或为空时返回 `None`。
    ///
    /// ```ignore
    /// let c = Chord::parse("Ctrl+Shift+S").unwrap();
    /// assert!(c.ctrl && c.shift && c.key == "s");
    /// ```
    pub fn parse(text: &str) -> Option<Chord> {
        let text = text.trim();
        // 主键本身是 “+” 时写作 “Ctrl++”，按末位字符特判
        let (mods_part, key_part) = match text.rfind('+') {
            Some(pos) if pos + 1 == text.len() && pos > 0 => (&text[..pos - 1], "+"),
            Some(pos) => (&text[..pos], &text[pos + 1..]),
            None => ("", text),
        };
        let mut chord = Chord { key: String::new(), ctrl: false, shift: false, alt: false };
        for token in mods_part.split('+').map(str::trim).filter(|t| !t.is_empty()) {
            match token.to_ascii_lowercase().as_str() {
                "ctrl" | "control" => chord.ctrl = true,
                "shift" => chord.shift = true,
                "alt" => chord.alt = true,
                _ => return None,
            }
        }
        let key = key_part.trim();
        if key.is_empty() || matches!(key.to_ascii_lowercase().as_str(), "ctrl" | "control" | "shift" | "alt" | "meta" | "win" | "windows" | "cmd") {
            return None;
        }
        chord.key = normalize_key(key);
        Some(chord)
    }

    /// 是否与一次按键完全匹配（修饰键必须一致）。
    pub fn matches(&self, key: &str, ctrl: bool, shift: bool, alt: bool) -> bool {
        self.key == key && self.ctrl == ctrl && self.shift == shift && self.alt == alt
    }
}

/// 截图键位表：配置键 → 动作，顺序即优先级（截图键位先于绘制键位）。
const SCREENSHOT_KEYS: &[(&str, OverlayKeyAction)] = &[
    ("screenshot_shortcuts/cancel_screenshot", OverlayKeyAction::Cancel),
    ("screenshot_shortcuts/copy_to_clipboard", OverlayKeyAction::CopyToClipboard),
    ("screenshot_shortcuts/save_as_file", OverlayKeyAction::SaveAsFile),
    ("screenshot_shortcuts/quick_save", OverlayKeyAction::Unimplemented("screenshot_shortcuts/quick_save")),
    ("screenshot_shortcuts/pin_to_screen", OverlayKeyAction::PinToScreen),
    ("screenshot_shortcuts/video_recording", OverlayKeyAction::VideoRecording),
    ("screenshot_shortcuts/text_recognition", OverlayKeyAction::TextRecognition),
    ("screenshot_shortcuts/text_translation", OverlayKeyAction::TextTranslation),
    ("screenshot_shortcuts/scrolling_screenshot", OverlayKeyAction::ScrollingScreenshot),
    ("screenshot_shortcuts/table_recognition", OverlayKeyAction::Unimplemented("screenshot_shortcuts/table_recognition")),
    ("screenshot_shortcuts/qr_code_recognition", OverlayKeyAction::Unimplemented("screenshot_shortcuts/qr_code_recognition")),
    ("screenshot_shortcuts/undo", OverlayKeyAction::Undo),
    ("screenshot_shortcuts/redo", OverlayKeyAction::Redo),
    ("screenshot_shortcuts/copy_color", OverlayKeyAction::CopyColor),
    ("screenshot_shortcuts/move_tool", OverlayKeyAction::MoveTool),
    ("screenshot_shortcuts/move_cursor_up", OverlayKeyAction::MoveCursor(Dir::Up)),
    ("screenshot_shortcuts/move_cursor_down", OverlayKeyAction::MoveCursor(Dir::Down)),
    ("screenshot_shortcuts/move_cursor_left", OverlayKeyAction::MoveCursor(Dir::Left)),
    ("screenshot_shortcuts/move_cursor_right", OverlayKeyAction::MoveCursor(Dir::Right)),
    ("screenshot_shortcuts/move_entire_selection", OverlayKeyAction::Unimplemented("screenshot_shortcuts/move_entire_selection")),
    ("screenshot_shortcuts/keep_selection_width_and_height_consistent", OverlayKeyAction::Unimplemented("screenshot_shortcuts/keep_selection_width_and_height_consistent")),
    ("screenshot_shortcuts/switch_selection_between_window_and_window_sub_element", OverlayKeyAction::Unimplemented("screenshot_shortcuts/switch_selection_between_window_and_window_sub_element")),
    ("screenshot_shortcuts/previous_screenshot_history", OverlayKeyAction::Unimplemented("screenshot_shortcuts/previous_screenshot_history")),
    ("screenshot_shortcuts/next_screenshot_history", OverlayKeyAction::Unimplemented("screenshot_shortcuts/next_screenshot_history")),
    ("screenshot_shortcuts/select_previously_selected_area", OverlayKeyAction::Unimplemented("screenshot_shortcuts/select_previously_selected_area")),
    ("screenshot_shortcuts/recapture", OverlayKeyAction::Unimplemented("screenshot_shortcuts/recapture")),
    ("screenshot_shortcuts/toggle_coordinate_mode", OverlayKeyAction::Unimplemented("screenshot_shortcuts/toggle_coordinate_mode")),
];

/// 绘制工具键位表：配置键 → 工具。
const DRAWING_KEYS: &[(&str, DrawingKey)] = &[
    ("drawing_shortcuts/select", DrawingKey::Select),
    ("drawing_shortcuts/shape", DrawingKey::Shape),
    ("drawing_shortcuts/arrow", DrawingKey::Arrow),
    ("drawing_shortcuts/brush", DrawingKey::Brush),
    ("drawing_shortcuts/highlight", DrawingKey::Highlight),
    ("drawing_shortcuts/text", DrawingKey::Text),
    ("drawing_shortcuts/serial_number", DrawingKey::SerialNumber),
    ("drawing_shortcuts/filter", DrawingKey::Filter),
    ("drawing_shortcuts/eraser", DrawingKey::Eraser),
    ("drawing_shortcuts/watermark", DrawingKey::Watermark),
];

/// 覆盖窗键位表（由配置构造）。
#[derive(Debug, Clone, PartialEq)]
pub struct OverlayKeymap {
    /// 按优先级排列的 `(组合, 动作)`。
    bindings: Vec<(Chord, OverlayKeyAction)>,
}

impl OverlayKeymap {
    /// 从配置文档构造；缺键用 schema 默认值。
    ///
    /// # 参数
    /// - `document`：配置文档。
    ///
    /// ```ignore
    /// let map = OverlayKeymap::from_document(&document);
    /// assert_eq!(map.resolve("escape", false, false, false), Some(OverlayKeyAction::Cancel));
    /// ```
    pub fn from_document(document: &ConfigDocument) -> Self {
        let mut bindings = Vec::new();
        let rows = SCREENSHOT_KEYS
            .iter()
            .map(|(k, a)| (*k, *a))
            .chain(DRAWING_KEYS.iter().map(|(k, t)| (*k, OverlayKeyAction::Tool(*t))));
        for (key, action) in rows {
            for text in shortcut_strings(&document.value(key)) {
                if let Some(chord) = Chord::parse(&text) {
                    bindings.push((chord, action));
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
    pub fn resolve(&self, key: &str, ctrl: bool, shift: bool, alt: bool) -> Option<OverlayKeyAction> {
        self.bindings
            .iter()
            .find(|(chord, _)| chord.matches(key, ctrl, shift, alt))
            .map(|(_, action)| *action)
    }
}

impl Default for OverlayKeymap {
    /// 全部默认值（C++ 键位表）。
    fn default() -> Self {
        Self::from_document(&ConfigDocument::from_bytes(None))
    }
}

/// 键位表里涉及的全部配置键（供“已接线键清单”核对与测试）。
pub fn wired_config_keys() -> Vec<&'static str> {
    SCREENSHOT_KEYS
        .iter()
        .map(|(k, _)| *k)
        .chain(DRAWING_KEYS.iter().map(|(k, _)| *k))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 组合解析：修饰键、别名、符号键、只有修饰键与 Win 键。
    #[test]
    fn chord_parsing() {
        let c = Chord::parse("Ctrl+Shift+S").unwrap();
        assert!(c.ctrl && c.shift && !c.alt && c.key == "s");
        assert_eq!(Chord::parse("Esc").unwrap().key, "escape");
        assert_eq!(Chord::parse(",").unwrap().key, ",");
        assert_eq!(Chord::parse("Alt+R").unwrap(), Chord { key: "r".into(), ctrl: false, shift: false, alt: true });
        assert!(Chord::parse("Shift").is_none());
        assert!(Chord::parse("Win+A").is_none());
        assert!(Chord::parse("").is_none());
    }

    /// 默认键位与 C++ 键位表一致。
    #[test]
    fn default_map_matches_cpp_table() {
        use OverlayKeyAction::*;
        let map = OverlayKeymap::default();
        let cases: &[(&str, bool, bool, bool, OverlayKeyAction)] = &[
            ("escape", false, false, false, Cancel),
            ("c", true, false, false, CopyToClipboard),
            ("s", true, false, false, SaveAsFile),
            ("f", true, false, false, PinToScreen),
            ("r", true, false, false, VideoRecording),
            ("d", true, false, false, TextRecognition),
            ("t", true, false, false, TextTranslation),
            ("l", false, false, false, ScrollingScreenshot),
            ("z", true, false, false, Undo),
            ("y", true, false, false, Redo),
            ("c", false, false, false, CopyColor),
            ("m", false, false, false, MoveTool),
            ("e", true, false, false, MoveTool),
            ("w", false, false, false, MoveCursor(Dir::Up)),
            ("up", false, false, false, MoveCursor(Dir::Up)),
            ("d", false, false, false, MoveCursor(Dir::Right)),
            ("v", false, false, false, Tool(DrawingKey::Select)),
            ("1", false, false, false, Tool(DrawingKey::Shape)),
            ("p", false, false, false, Tool(DrawingKey::Brush)),
            ("9", false, false, false, Tool(DrawingKey::Watermark)),
            ("s", true, true, false, Unimplemented("screenshot_shortcuts/quick_save")),
            ("r", false, false, true, Unimplemented("screenshot_shortcuts/recapture")),
            ("x", true, false, false, Unimplemented("screenshot_shortcuts/table_recognition")),
        ];
        for (key, ctrl, shift, alt, want) in cases {
            assert_eq!(map.resolve(key, *ctrl, *shift, *alt), Some(*want), "{key}");
        }
        assert_eq!(map.resolve("q", false, false, false), None);
        // 修饰键必须完全一致
        assert_eq!(map.resolve("z", true, true, false), None);
    }

    /// 配置改键后生效；缺键补默认；整张表的键都在 schema 里。
    #[test]
    fn config_overrides_and_missing_keys() {
        let doc = ConfigDocument::from_bytes(Some(
            br#"{"screenshot_shortcuts":{"cancel_screenshot":["Q"]},"drawing_shortcuts":{"arrow":["Ctrl+2"]}}"#,
        ));
        let map = OverlayKeymap::from_document(&doc);
        assert_eq!(map.resolve("q", false, false, false), Some(OverlayKeyAction::Cancel));
        assert_eq!(map.resolve("escape", false, false, false), None);
        assert_eq!(map.resolve("2", true, false, false), Some(OverlayKeyAction::Tool(DrawingKey::Arrow)));
        assert_eq!(map.resolve("c", true, false, false), Some(OverlayKeyAction::CopyToClipboard));
        let defaults = ConfigDocument::from_bytes(None);
        for key in wired_config_keys() {
            assert!(!shortcut_strings(&defaults.value(key)).is_empty(), "{key} 缺默认值");
        }
        assert_eq!(wired_config_keys().len(), 27 + 10);
    }

    /// 方向位移。
    #[test]
    fn dir_delta() {
        assert_eq!(Dir::Up.delta(), (0, -1));
        assert_eq!(Dir::Right.delta(), (1, 0));
    }
}
