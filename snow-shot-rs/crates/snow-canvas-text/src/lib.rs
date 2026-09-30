//! 标注文本布局、IME、光标与选区。
//!
//! 所属阶段：P2。提供标注文本草稿管理、样式模型、多行排版测量与 GPUI 输入法连接器。

pub mod draft;
pub mod input;
pub mod layout;
pub mod style;

pub use draft::TextDraft;
pub use input::{CanvasTextInput, EditKeyOutcome, apply_edit_key};
pub use layout::{
    TextCharMetric, TextLayoutResult, TextLineLayout, TextPoint, TextRect, TextSize,
};
pub use style::{
    CanvasTextStyle, FONT_SIZE_STEPS, TextAlignment, stepped_font_size, style_mask,
};

/// 本 crate 的阶段标记。
pub const PHASE: &str = "P2";

#[cfg(test)]
mod tests {
    use super::*;

    /// 阶段标记不应为空。
    #[test]
    fn phase_not_empty() {
        assert_eq!(PHASE, "P2");
    }

    /// 测试文本草稿的基本插入与删除。
    #[test]
    fn draft_basic_editing() {
        let mut draft = TextDraft::new();
        draft.insert_text("Hello");
        assert_eq!(draft.text(), "Hello");
        assert_eq!(draft.cursor(), 5);

        draft.insert_text(" World");
        assert_eq!(draft.text(), "Hello World");

        assert!(draft.delete_backward());
        assert_eq!(draft.text(), "Hello Worl");

        draft.set_cursor(5, false);
        assert!(draft.delete_forward());
        assert_eq!(draft.text(), "HelloWorl");

        assert!(draft.clear());
        assert_eq!(draft.text(), "");
        assert_eq!(draft.cursor(), 0);
    }

    /// 测试文本选区与选区替换。
    #[test]
    fn draft_selection_and_replace() {
        let mut draft = TextDraft::with_text("The quick brown fox");
        assert_eq!(draft.text(), "The quick brown fox");

        // 选中 "quick " (4..10)
        draft.set_cursor(4, false);
        draft.set_cursor(10, true);
        assert!(draft.has_selection());
        assert_eq!(draft.selected_text(), "quick ");

        // 替换选区
        draft.insert_text("slow ");
        assert_eq!(draft.text(), "The slow brown fox");
        assert!(!draft.has_selection());

        // 全选测试
        draft.select_all();
        assert!(draft.has_selection());
        assert_eq!(draft.selected_text(), "The slow brown fox");
    }

    /// 测试多语言中文字符与复合字形团的光标导航。
    #[test]
    fn draft_cjk_and_grapheme_navigation() {
        let mut draft = TextDraft::with_text("你好，世界！");
        assert_eq!(draft.cursor(), "你好，世界！".len());

        // 向左移动一个中文字符
        draft.move_left(false);
        assert_eq!(draft.cursor(), "你好，世界".len());

        // 回到行首
        draft.move_home(false);
        assert_eq!(draft.cursor(), 0);

        // 向右移动一个字符
        draft.move_right(false);
        assert_eq!(draft.cursor(), "你".len());

        // 移动到末尾
        draft.move_end(false);
        assert_eq!(draft.cursor(), "你好，世界！".len());
    }

    /// 测试撤销与重做操作。
    #[test]
    fn draft_undo_redo() {
        let mut draft = TextDraft::new();
        draft.insert_text("A");
        draft.insert_text("B");
        draft.insert_text("C");
        assert_eq!(draft.text(), "ABC");

        assert!(draft.undo());
        assert_eq!(draft.text(), "AB");

        assert!(draft.undo());
        assert_eq!(draft.text(), "A");

        assert!(draft.redo());
        assert_eq!(draft.text(), "AB");

        assert!(draft.redo());
        assert_eq!(draft.text(), "ABC");

        assert!(!draft.redo());
    }

    /// 测试 UTF-8 与 UTF-16 偏移互转（覆盖 BMP 与增补平面字符）。
    #[test]
    fn draft_utf16_conversions() {
        // "A" (1字节/1单元) + "好" (3字节/1单元) + "🚀" (4字节/2单元)
        let draft = TextDraft::with_text("A好🚀");
        assert_eq!(draft.to_utf16(0), 0);
        assert_eq!(draft.to_utf16(1), 1); // "A" 之后
        assert_eq!(draft.to_utf16(4), 2); // "好" 之后 (1+3=4字节)
        assert_eq!(draft.to_utf16(8), 4); // "🚀" 之后 (4+4=8字节，UTF-16 占 2 单元)

        assert_eq!(draft.from_utf16(0), 0);
        assert_eq!(draft.from_utf16(1), 1);
        assert_eq!(draft.from_utf16(2), 4);
        assert_eq!(draft.from_utf16(4), 8);
    }

    /// 测试 IME 预编辑与提交生命周期。
    #[test]
    fn draft_ime_lifecycle() {
        let mut draft = TextDraft::with_text("Hello");
        draft.set_cursor(5, false);

        // 输入拼音 "nihao"
        draft.ime_replace_and_mark(None, "nihao", None);
        assert!(draft.has_preedit());
        assert_eq!(draft.preedit_text(), "nihao");
        assert_eq!(draft.display_text(), "Hellonihao");

        // 提交候选字 "你好"
        draft.ime_replace_text(None, "你好");
        assert!(!draft.has_preedit());
        assert_eq!(draft.text(), "Hello你好");
        assert_eq!(draft.display_text(), "Hello你好");
    }

    /// 测试文本样式属性限制与字号梯阶。
    #[test]
    fn text_style_and_steps() {
        let style = CanvasTextStyle::default();
        assert_eq!(style.font_size, 16.0);

        // 放大两档
        let s1 = stepped_font_size(style.font_size, true);
        assert_eq!(s1, 18.0);
        let s2 = stepped_font_size(s1, true);
        assert_eq!(s2, 20.0);

        // 缩小一档
        let s3 = stepped_font_size(s2, false);
        assert_eq!(s3, 18.0);

        // 边界限制
        assert_eq!(CanvasTextStyle::clamp_font_size(5.0), CanvasTextStyle::MIN_FONT_SIZE);
        assert_eq!(CanvasTextStyle::clamp_font_size(500.0), CanvasTextStyle::MAX_FONT_SIZE);
    }

    /// 测试文本样式局部修补。
    #[test]
    fn text_style_patch() {
        let base = CanvasTextStyle::default();
        let mut target = base.clone();
        target.font_size = 32.0;
        target.bold = true;
        target.color = [0, 0, 255, 255];

        let patched = base.patch(&target, style_mask::FONT_SIZE | style_mask::BOLD);
        assert_eq!(patched.font_size, 32.0);
        assert!(patched.bold);
        assert_eq!(patched.color, base.color); // 颜色未在掩码中，保持原值
    }

    /// 测试排版测量、折行与命中测试。
    #[test]
    fn text_layout_and_hit_test() {
        let style = CanvasTextStyle::default();
        let text = "Line1\nLine2";
        let layout = TextLayoutResult::layout_text(text, &style, None);
        assert_eq!(layout.lines.len(), 2);
        assert_eq!(layout.lines[0].text, "Line1");
        assert_eq!(layout.lines[1].text, "Line2");

        // 命中第一行首
        let p_start = TextPoint::new(0.0, 5.0);
        assert_eq!(layout.hit_test(p_start), 0);

        // 光标矩形测试
        let caret = layout.cursor_rect(0, 2.0);
        assert_eq!(caret.origin.x, 0.0);
        assert_eq!(caret.size.width, 2.0);

        // 选区矩形测试 (选中跨行)
        let rects = layout.selection_rects(0..text.len());
        assert_eq!(rects.len(), 2);
    }
}
