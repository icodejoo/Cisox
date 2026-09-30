//! 翻译前的版式整理：把 OCR 按物理行切分的文本块合并成段落，避免一句话被拆开逐行翻译。

use crate::ocr_service::OcrTextBox;

/// 垂直间距过大的判断系数（相对较大行高）。
const VERTICAL_GAP_THRESHOLD: f32 = 0.6;
/// 行高差异过大的判断系数（最大行高 / 最小行高）。
const LINE_HEIGHT_DIFF_THRESHOLD: f32 = 1.6;
/// 左边界错位过大的判断系数（相对较大行高）。
const X_OFFSET_THRESHOLD: f32 = 3.0;
/// 行高非正时使用的最小行高。
const MIN_LINE_HEIGHT: f32 = 1.0;
/// 判断垂直中线时的除数。
const HALF: f32 = 2.0;
/// 配置中表示原始版式的取值。
const ORIGINAL_CONFIG_VALUE: &str = "original";

/// 版式处理模式（对应配置 screenshot_translation/layout_processing）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LayoutMode {
    /// 智能合并：把相邻行合并成段落。
    SmartMerge,
    /// 原始版式：逐行输出。
    Original,
}

impl LayoutMode {
    /// 从配置字符串解析版式处理模式。
    ///
    /// # 参数
    /// * `value` - 配置值字符串
    ///
    /// # 返回
    /// `"original"` 返回 `Original`，其它取值（含空串、未知值）返回 `SmartMerge`。
    ///
    /// # 示例
    /// ```ignore
    /// let mode = LayoutMode::from_config("original");
    /// assert_eq!(mode, LayoutMode::Original);
    /// ```
    pub fn from_config(value: &str) -> Self {
        if value == ORIGINAL_CONFIG_VALUE {
            Self::Original
        } else {
            Self::SmartMerge
        }
    }
}

/// 判断字符是否为句末标点。
fn is_sentence_punctuation(c: char) -> bool {
    matches!(c, '.' | '!' | '?' | '。' | '！' | '？' | '…' | ':' | '：' | ';' | '；')
}

/// 判断字符是否为收尾引号或括号。
fn is_closing_mark(c: char) -> bool {
    matches!(c, '”' | '’' | '"' | '\'' | ')' | '）' | '】')
}

/// 判断文本是否以句末标点结尾（标点后允许跟收尾引号或括号）。
fn ends_with_sentence_punctuation(text: &str) -> bool {
    text.chars()
        .rev()
        .find(|c| !is_closing_mark(*c))
        .is_some_and(is_sentence_punctuation)
}

/// 行高转 f32，非正值按最小行高处理。
fn clamped_height(height: i32) -> f32 {
    (height as f32).max(MIN_LINE_HEIGHT)
}

/// 判断相邻两块之间是否应断段。
fn should_break_paragraph(prev: &OcrTextBox, cur: &OcrTextBox, prev_text: &str) -> bool {
    // (a) 上一块以句末标点结尾
    if ends_with_sentence_punctuation(prev_text) {
        return true;
    }

    let prev_h = prev.rect.height as f32;
    let cur_h = cur.rect.height as f32;
    let max_h = prev_h.max(cur_h);
    let prev_bottom = prev.rect.y as f32 + prev_h;
    let cur_y = cur.rect.y as f32;

    // (b) 垂直间距过大
    if cur_y - prev_bottom > VERTICAL_GAP_THRESHOLD * max_h {
        return true;
    }

    // (c) cur 中线未落到 prev 下沿以下
    if cur_y + cur_h / HALF < prev_bottom {
        return true;
    }

    // (d) 行高差异过大
    let (ph, ch) = (clamped_height(prev.rect.height), clamped_height(cur.rect.height));
    if ph.max(ch) / ph.min(ch) > LINE_HEIGHT_DIFF_THRESHOLD {
        return true;
    }

    // (e) 左边界错位
    (cur.rect.x as f32 - prev.rect.x as f32).abs() > X_OFFSET_THRESHOLD * max_h
}

/// 合并两段文本：英文断词去连字符，CJK 交界不加空格，其余加空格。
fn merge_texts(prev: &str, cur: &str) -> String {
    let mut rev = prev.chars().rev();
    let last = rev.next();
    let second_last = rev.next();
    let cur_first = cur.chars().next();

    if let (Some('-'), Some(sl), Some(cf)) = (last, second_last, cur_first)
        && sl.is_ascii_alphabetic()
        && cf.is_ascii_lowercase()
    {
        return format!("{}{}", &prev[..prev.len() - 1], cur);
    }

    if let (Some(l), Some(cf)) = (last, cur_first)
        && !l.is_ascii()
        && !cf.is_ascii()
    {
        return format!("{prev}{cur}");
    }

    format!("{prev} {cur}")
}

/// 把 OCR 文本块整理成待翻译段落。
///
/// # 参数
/// * `boxes` - OCR 按行切分的文本块
/// * `mode` - 版式处理模式
///
/// # 返回
/// 段落列表，顺序与输入一致；空白块会被丢弃。
///
/// # 示例
/// ```ignore
/// let paragraphs = paragraphs_from_boxes(&boxes, LayoutMode::SmartMerge);
/// ```
pub fn paragraphs_from_boxes(boxes: &[OcrTextBox], mode: LayoutMode) -> Vec<String> {
    let mut paragraphs = Vec::new();
    let mut current = String::new();
    let mut prev_box: Option<&OcrTextBox> = None;

    for bx in boxes {
        let text = bx.text.trim();
        if text.is_empty() {
            continue;
        }

        match prev_box {
            None => current.push_str(text),
            Some(prev) => {
                let split = match mode {
                    LayoutMode::Original => true,
                    LayoutMode::SmartMerge => should_break_paragraph(prev, bx, &current),
                };
                if split {
                    paragraphs.push(std::mem::replace(&mut current, text.to_string()));
                } else {
                    current = merge_texts(&current, text);
                }
            }
        }
        prev_box = Some(bx);
    }

    if !current.is_empty() {
        paragraphs.push(current);
    }
    paragraphs
}

#[cfg(test)]
mod tests {
    use super::*;
    use snow_ui::shell::geometry::PhysicalRect;

    /// 构造测试用文本块。
    fn b(x: i32, y: i32, w: i32, h: i32, text: &str) -> OcrTextBox {
        OcrTextBox {
            rect: PhysicalRect::new(x, y, w, h),
            text: text.to_string(),
            confidence: 0.9,
        }
    }

    /// from_config 三种取值。
    #[test]
    fn from_config_cases() {
        assert_eq!(LayoutMode::from_config("original"), LayoutMode::Original);
        assert_eq!(LayoutMode::from_config("smart_merge"), LayoutMode::SmartMerge);
        assert_eq!(LayoutMode::from_config(""), LayoutMode::SmartMerge);
        assert_eq!(LayoutMode::from_config("whatever"), LayoutMode::SmartMerge);
    }

    /// 空输入返回空 Vec。
    #[test]
    fn empty_input_returns_empty() {
        assert!(paragraphs_from_boxes(&[], LayoutMode::SmartMerge).is_empty());
        assert!(paragraphs_from_boxes(&[], LayoutMode::Original).is_empty());
    }

    /// 空白块被丢弃。
    #[test]
    fn blank_blocks_are_dropped() {
        let boxes = vec![
            b(0, 0, 100, 20, "Line 1"),
            b(0, 20, 100, 20, "  \t "),
            b(0, 25, 100, 20, "Line 2"),
        ];
        assert_eq!(paragraphs_from_boxes(&boxes, LayoutMode::SmartMerge), vec!["Line 1 Line 2"]);
        assert_eq!(paragraphs_from_boxes(&boxes, LayoutMode::Original), vec!["Line 1", "Line 2"]);
    }

    /// Original 保持逐行。
    #[test]
    fn original_keeps_lines() {
        let boxes = vec![b(0, 0, 100, 20, "Hello"), b(0, 22, 100, 20, "world")];
        assert_eq!(paragraphs_from_boxes(&boxes, LayoutMode::Original), vec!["Hello", "world"]);
    }

    /// 同段落两行英文合并并加空格。
    #[test]
    fn merges_english_lines_with_space() {
        let boxes = vec![b(0, 0, 100, 20, "Hello"), b(0, 22, 100, 20, "world")];
        assert_eq!(paragraphs_from_boxes(&boxes, LayoutMode::SmartMerge), vec!["Hello world"]);
    }

    /// 中文行合并不加空格。
    #[test]
    fn merges_chinese_without_space() {
        let boxes = vec![b(0, 0, 100, 20, "今天天气"), b(0, 22, 100, 20, "非常不错")];
        assert_eq!(paragraphs_from_boxes(&boxes, LayoutMode::SmartMerge), vec!["今天天气非常不错"]);
    }

    /// 连字符断词拼接。
    #[test]
    fn joins_hyphenated_word() {
        let boxes = vec![b(0, 0, 100, 20, "out-"), b(0, 22, 100, 20, "standing")];
        assert_eq!(paragraphs_from_boxes(&boxes, LayoutMode::SmartMerge), vec!["outstanding"]);
    }

    /// 句末标点（含收尾引号）断段。
    #[test]
    fn sentence_end_breaks() {
        let boxes = vec![b(0, 0, 100, 20, "First."), b(0, 22, 100, 20, "Second")];
        assert_eq!(paragraphs_from_boxes(&boxes, LayoutMode::SmartMerge), vec!["First.", "Second"]);

        let quoted = vec![b(0, 0, 100, 20, "He said: “Stop!”"), b(0, 22, 100, 20, "Then left")];
        assert_eq!(
            paragraphs_from_boxes(&quoted, LayoutMode::SmartMerge),
            vec!["He said: “Stop!”", "Then left"]
        );
    }

    /// 垂直大间距断段。
    #[test]
    fn large_vertical_gap_breaks() {
        let boxes = vec![b(0, 0, 100, 20, "Para one"), b(0, 40, 100, 20, "Para two")];
        assert_eq!(
            paragraphs_from_boxes(&boxes, LayoutMode::SmartMerge),
            vec!["Para one", "Para two"]
        );
    }

    /// 同行或上方的块断段。
    #[test]
    fn same_row_breaks() {
        let boxes = vec![b(0, 0, 100, 20, "Left"), b(110, 5, 100, 20, "Right")];
        assert_eq!(paragraphs_from_boxes(&boxes, LayoutMode::SmartMerge), vec!["Left", "Right"]);
    }

    /// 行高差异断段。
    #[test]
    fn line_height_difference_breaks() {
        let boxes = vec![b(0, 0, 100, 10, "Small"), b(0, 12, 100, 30, "Large")];
        assert_eq!(paragraphs_from_boxes(&boxes, LayoutMode::SmartMerge), vec!["Small", "Large"]);
    }

    /// 左边界错位断段。
    #[test]
    fn left_misalignment_breaks() {
        let boxes = vec![b(0, 0, 100, 20, "Normal"), b(80, 22, 100, 20, "Indented")];
        assert_eq!(
            paragraphs_from_boxes(&boxes, LayoutMode::SmartMerge),
            vec!["Normal", "Indented"]
        );
    }
}
