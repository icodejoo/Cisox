//! 录屏控制条键位表：按 `screen_recording_shortcuts/*`（4 键）解析配置，把一次按键映射成录屏动作。
//! 纯逻辑，不接触界面；组合解析与覆盖窗共用 [`Chord`]。快捷键只在控制条拿到焦点时生效
//! （点一下控制条即可），不注册全局热键，避免录屏期间劫持其它程序的 Ctrl+C / Esc。

use crate::app_runtime::shortcut_strings;
use crate::overlay_keymap::Chord;
use snow_config::document::ConfigDocument;

/// 录屏控制条上的键位动作。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecordKeyAction {
    /// 停止录制并保存。
    Export,
    /// 暂停 / 继续录制。
    ToggleRecording,
    /// 停止录制并把录制文件复制到剪贴板。
    CopyToClipboard,
    /// 结束并关闭录制窗（倒计时 / 出错时放弃；录制中不响应，避免误丢录像）。
    EndRecording,
}

/// 键位表：配置键 → 动作，顺序即优先级。
const RECORD_KEYS: &[(&str, RecordKeyAction)] = &[
    ("screen_recording_shortcuts/export", RecordKeyAction::Export),
    ("screen_recording_shortcuts/toggle_recording", RecordKeyAction::ToggleRecording),
    ("screen_recording_shortcuts/copy_to_clipboard", RecordKeyAction::CopyToClipboard),
    ("screen_recording_shortcuts/end_recording", RecordKeyAction::EndRecording),
];

/// 录屏键位表（由配置构造）。
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct RecordKeymap {
    /// 按优先级排列的 `(组合, 动作)`。
    bindings: Vec<(Chord, RecordKeyAction)>,
}

impl RecordKeymap {
    /// 从配置文档构造；缺键用 schema 默认值。
    ///
    /// # 参数
    /// - `document`：配置文档。
    ///
    /// ```ignore
    /// let map = RecordKeymap::from_document(&document);
    /// assert_eq!(map.resolve("e", true, false, false), Some(RecordKeyAction::Export));
    /// ```
    pub fn from_document(document: &ConfigDocument) -> Self {
        let mut bindings = Vec::new();
        for (key, action) in RECORD_KEYS {
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
    pub fn resolve(&self, key: &str, ctrl: bool, shift: bool, alt: bool) -> Option<RecordKeyAction> {
        self.bindings
            .iter()
            .find(|(chord, _)| chord.matches(key, ctrl, shift, alt))
            .map(|(_, action)| *action)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 默认键位与旧版一致，改键后生效。
    #[test]
    fn defaults_and_overrides() {
        let mut doc = ConfigDocument::from_bytes(None);
        let map = RecordKeymap::from_document(&doc);
        assert_eq!(map.resolve("e", true, false, false), Some(RecordKeyAction::Export));
        assert_eq!(map.resolve("s", true, false, false), Some(RecordKeyAction::ToggleRecording));
        assert_eq!(map.resolve("c", true, false, false), Some(RecordKeyAction::CopyToClipboard));
        assert_eq!(map.resolve("escape", false, false, false), Some(RecordKeyAction::EndRecording));
        assert_eq!(map.resolve("e", false, false, false), None);

        doc.set_value("screen_recording_shortcuts/export", serde_json::json!([{"portable": "F9"}])).unwrap();
        let map = RecordKeymap::from_document(&doc);
        assert_eq!(map.resolve("f9", false, false, false), Some(RecordKeyAction::Export));
        assert_eq!(map.resolve("e", true, false, false), None);
    }
}
