//! 语音转文字的文本累积：把 PARTIAL / FINAL 序列合并成“已落定 + 未落定”两段。
//!
//! 全是纯逻辑：句子之间要不要补空格、控制字符怎么处理、整段文本是什么，都在这里定死并可离屏测试。

/// 句末哪些 ASCII 标点后面接英文单词时需要补空格。
const SPACE_AFTER_PUNCT: &[char] = &['.', ',', '!', '?', ';', ':'];

/// 清洗识别文本：换行与制表符变空格，其余控制字符丢弃，再折叠连续空白并去掉首尾空白。
///
/// 键入时换行会触发回车（可能提交表单），所以必须在进入累积前清洗。
///
/// # 参数
/// - `text`：识别原文。
///
/// ```ignore
/// assert_eq!(sanitize(" a\nb\u{7} "), "a b");
/// ```
pub fn sanitize(text: &str) -> String {
    let mapped: String = text
        .chars()
        .filter_map(|c| match c {
            '\n' | '\r' | '\t' => Some(' '),
            c if c.is_control() => None,
            c => Some(c),
        })
        .collect();
    // 折叠连续空白，避免换行变空格后留下多个空格
    mapped.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// 两段文字拼接处是否需要补一个空格：前一段以英文字母数字或句读结尾、后一段以英文字母数字开头。
///
/// 中日文之间、以及与中日文相邻的拼接不补空格。
///
/// # 参数
/// - `prev`：前一段。
/// - `next`：后一段。
pub fn needs_space(prev: &str, next: &str) -> bool {
    let (Some(last), Some(first)) = (prev.chars().last(), next.chars().next()) else {
        return false;
    };
    if last.is_whitespace() || first.is_whitespace() {
        return false;
    }
    (last.is_ascii_alphanumeric() || SPACE_AFTER_PUNCT.contains(&last))
        && first.is_ascii_alphanumeric()
}

/// 拼接两段文字，按需补空格；任一段为空时原样返回另一段。
///
/// # 参数
/// - `prev`：前一段。
/// - `next`：后一段。
///
/// ```ignore
/// assert_eq!(join("hello", "world"), "hello world");
/// assert_eq!(join("你好", "世界"), "你好世界");
/// ```
pub fn join(prev: &str, next: &str) -> String {
    if prev.is_empty() {
        return next.to_string();
    }
    if next.is_empty() {
        return prev.to_string();
    }
    if needs_space(prev, next) {
        format!("{prev} {next}")
    } else {
        format!("{prev}{next}")
    }
}

/// 一轮识别的文本累积。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Transcript {
    /// 已落定的文本（FINAL 依次拼接）。
    finals: String,
    /// 当前这句的未落定文本。
    partial: String,
}

impl Transcript {
    /// 清空，开始新一轮。
    pub fn clear(&mut self) {
        self.finals.clear();
        self.partial.clear();
    }

    /// 更新未落定文本（整句替换，不是追加）。
    ///
    /// # 参数
    /// - `text`：PARTIAL 原文。
    pub fn set_partial(&mut self, text: &str) {
        self.partial = sanitize(text);
    }

    /// 落定一句：拼到已落定文本末尾，并清掉未落定部分。空的 FINAL 只清未落定部分。
    ///
    /// # 参数
    /// - `text`：FINAL 原文。
    pub fn push_final(&mut self, text: &str) {
        let clean = sanitize(text);
        self.partial.clear();
        if !clean.is_empty() {
            self.finals = join(&self.finals, &clean);
        }
    }

    /// 已落定文本。
    pub fn finals(&self) -> &str {
        &self.finals
    }

    /// 未落定文本。
    pub fn partial(&self) -> &str {
        &self.partial
    }

    /// 当前应呈现的全文：已落定 + 未落定。
    pub fn full(&self) -> String {
        join(&self.finals, &self.partial)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 清洗：换行制表变空格、控制字符丢弃、首尾空白去掉。
    #[test]
    fn sanitize_rules() {
        assert_eq!(sanitize(" a\nb\tc\u{7}d "), "a b cd");
        assert_eq!(sanitize("\r\n"), "");
        assert_eq!(sanitize("你好"), "你好");
    }

    /// 补空格规则：英文接英文补、中文相邻不补、已有空白不重复补。
    #[test]
    fn spacing_rules() {
        assert_eq!(join("hello", "world"), "hello world");
        assert_eq!(join("done.", "Next"), "done. Next");
        assert_eq!(join("你好", "世界"), "你好世界");
        assert_eq!(join("你好", "world"), "你好world");
        assert_eq!(join("hello", "世界"), "hello世界");
        assert_eq!(join("hello ", "world"), "hello world");
        assert_eq!(join("", "x"), "x");
        assert_eq!(join("x", ""), "x");
        assert_eq!(join("(", "x"), "(x");
    }

    /// PARTIAL 整句替换，FINAL 落定后清掉未落定部分，全文始终是两者拼接。
    #[test]
    fn partial_final_sequence() {
        let mut t = Transcript::default();
        t.set_partial("你");
        t.set_partial("你好");
        assert_eq!(t.full(), "你好");
        assert_eq!(t.finals(), "");
        t.push_final("你好");
        assert_eq!((t.finals(), t.partial()), ("你好", ""));
        t.set_partial("世");
        assert_eq!(t.full(), "你好世");
        t.push_final("世界");
        assert_eq!(t.full(), "你好世界");
    }

    /// 英文句子之间补空格；空 FINAL 不改变已落定文本，只清未落定部分。
    #[test]
    fn english_sentences_and_empty_final() {
        let mut t = Transcript::default();
        t.push_final("hello there");
        t.set_partial("how are");
        assert_eq!(t.full(), "hello there how are");
        t.push_final("");
        assert_eq!(t.full(), "hello there");
        t.push_final("how are you");
        assert_eq!(t.full(), "hello there how are you");
    }

    /// 识别文本里的换行不会进入累积（否则键入时会按下回车）。
    #[test]
    fn newlines_never_reach_transcript() {
        let mut t = Transcript::default();
        t.push_final("a\nb");
        t.set_partial("c\r\nd");
        assert!(!t.full().contains(['\n', '\r']));
        assert_eq!(t.full(), "a b c d");
    }

    /// clear 开始新一轮。
    #[test]
    fn clear_resets() {
        let mut t = Transcript::default();
        t.push_final("x");
        t.set_partial("y");
        t.clear();
        assert_eq!(t, Transcript::default());
    }
}
