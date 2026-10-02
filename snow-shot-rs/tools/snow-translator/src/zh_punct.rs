//! 中日文译文的全半角标点后处理（NLLB 家族专用）。
//!
//! NLLB 对中日文目标输出半角标点（`, . : ; ! ? " ( )`），而习惯用全角。这里按上下文转换，
//! 并保持小数点、千分位、英文缩写、英文成句里的标点、括号里的纯英文不动。
//! 规则逐点移植自 `eval/zh_punct.py`，用 `tests/fixtures/zh_punct_golden.tsv` 与 Python 输出逐字对拍。

/// 简体中文目标的 FLORES 码。
pub const TGT_ZHO_HANS: &str = "zho_Hans";
/// 繁体中文目标的 FLORES 码。
pub const TGT_ZHO_HANT: &str = "zho_Hant";
/// 日文目标的 FLORES 码。
pub const TGT_JPN_JPAN: &str = "jpn_Jpan";

/// 全角左括号。
const FULL_LPAREN: char = '（';
/// 全角右括号。
const FULL_RPAREN: char = '）';
/// 其前面的空白要被吃掉的全角标点与收尾符。
const CLOSERS: [char; 9] = ['，', '。', '：', '；', '！', '？', '、', '”', '」'];
/// 其后面的空白要被吃掉的全角标点与起首符。
const OPENERS: [char; 9] = ['，', '。', '：', '；', '！', '？', '、', '“', '「'];

/// 目标语言是否需要全角标点后处理。
///
/// # 参数
/// - `tgt`：目标语言的 FLORES 码，如 `zho_Hans`。
///
/// # 返回
/// 简体中文、繁体中文、日文返回 `true`。
///
/// # 示例
/// ```ignore
/// assert!(needs_fullwidth("jpn_Jpan"));
/// assert!(!needs_fullwidth("eng_Latn"));
/// ```
pub fn needs_fullwidth(tgt: &str) -> bool {
    matches!(tgt, TGT_ZHO_HANS | TGT_ZHO_HANT | TGT_JPN_JPAN)
}

/// 半角标点对应的全角形式（日文没有 `:` `;`）。
fn map_punct(c: char, japanese: bool) -> Option<char> {
    match (c, japanese) {
        (',', false) => Some('，'),
        (',', true) => Some('、'),
        ('.', _) => Some('。'),
        (':', false) => Some('：'),
        (';', false) => Some('；'),
        ('!', _) => Some('！'),
        ('?', _) => Some('？'),
        _ => None,
    }
}

/// 是否 ASCII 字母或数字。
fn is_alnum(c: Option<char>) -> bool {
    c.is_some_and(|c| c.is_ascii_alphanumeric())
}

/// 是否 ASCII 字母。
fn is_alpha(c: Option<char>) -> bool {
    c.is_some_and(|c| c.is_ascii_alphabetic())
}

/// 是否中日文字符或全角标点（与 Python 版的字符区间一致）。
fn is_cjk(c: char) -> bool {
    matches!(c as u32,
        0x3040..=0x30FF | 0x3400..=0x4DBF | 0x4E00..=0x9FFF | 0xF900..=0xFAFF
        | 0xFF00..=0xFFEF | 0x3000..=0x303F)
}

/// 空白判定，对齐 Python 正则 `\s`（比 Rust 多 0x1C..=0x1F）。
fn is_space(c: char) -> bool {
    c.is_whitespace() || ('\u{1c}'..='\u{1f}').contains(&c)
}

/// 句点是否属于英文缩写：前一个是 ASCII 字母，且（再前一个是句点，或后面紧跟 `) ] '`，或已到结尾）。
///
/// 已到结尾也算缩写，对齐 Python 里 `"" in ")]'"` 为真的行为。
fn is_abbrev(chars: &[char], i: usize) -> bool {
    if chars[i] != '.' || i == 0 || !is_alpha(Some(chars[i - 1])) {
        return false;
    }
    let next = chars.get(i + 1).copied();
    (i >= 2 && chars[i - 2] == '.') || next.is_none_or(|n| matches!(n, ')' | ']' | '\''))
}

/// 第 `i` 个字符（半角标点）是否应保持半角：
/// 夹在 ASCII 字母数字之间（小数点、千分位）、英文缩写，或处于英文成句中（`Hello, world`）。
fn keep_halfwidth(chars: &[char], i: usize) -> bool {
    let prev = i.checked_sub(1).map(|p| chars[p]);
    let next = chars.get(i + 1).copied();
    if is_alnum(prev) && is_alnum(next) {
        return true;
    }
    if is_abbrev(chars, i) {
        return true;
    }
    is_alnum(prev) && next == Some(' ') && is_alpha(chars.get(i + 2).copied())
}

/// 括号内含中日文字符的一对 `( )` 转全角；纯英文/数字的保持。
fn convert_parens(chars: &[char]) -> Vec<char> {
    let mut out = chars.to_vec();
    let mut i = 0;
    while i < chars.len() {
        if chars[i] == '('
            && let Some(rel) = chars[i + 1..].iter().position(|&c| c == ')')
        {
            let j = i + 1 + rel;
            let inner = &chars[i + 1..j];
            if inner.iter().any(|&c| is_cjk(c)) && !inner.contains(&'(') {
                out[i] = FULL_LPAREN;
                out[j] = FULL_RPAREN;
                i = j;
            }
        }
        i += 1;
    }
    out
}

/// 去掉全角收尾符之前、全角起首符之后的空白（对应 Python 的两条 `re.sub`）。
fn strip_punct_spaces(chars: &[char]) -> String {
    let closers: Vec<char> = CLOSERS.iter().copied().chain([FULL_RPAREN]).collect();
    // 先吃“空白 + 收尾符”里的空白
    let mut first = String::with_capacity(chars.len());
    let mut i = 0;
    while i < chars.len() {
        if is_space(chars[i]) {
            let mut j = i;
            while j < chars.len() && is_space(chars[j]) {
                j += 1;
            }
            if j < chars.len() && closers.contains(&chars[j]) {
                i = j;
            } else {
                first.extend(&chars[i..j]);
                i = j;
            }
        } else {
            first.push(chars[i]);
            i += 1;
        }
    }
    // 再吃“起首符 + 空白”里的空白
    let openers: Vec<char> = OPENERS.iter().copied().chain([FULL_LPAREN]).collect();
    let mut out = String::with_capacity(first.len());
    let mut skipping = false;
    for c in first.chars() {
        if skipping && is_space(c) {
            continue;
        }
        skipping = openers.contains(&c);
        out.push(c);
    }
    out
}

/// 把中日文目标译文里的半角标点按上下文转成全角。
///
/// # 参数
/// - `text`：译文。
/// - `tgt`：目标语言 FLORES 码；不是 `zho_Hans`/`zho_Hant`/`jpn_Jpan` 时原样返回。
///
/// # 返回
/// 转换后的字符串：`,.:;!?` 转全角（日文逗号为 `、`，无 `:` `;`），`"` 按出现顺序成对转 `“”`（日文 `「」`），
/// 含中日文的 `( )` 转全角；小数点、千分位、缩写、英文成句标点、纯英文括号保持不动。
///
/// # 示例
/// ```ignore
/// assert_eq!(to_fullwidth("4.5个月,好.", "zho_Hans"), "4.5个月，好。");
/// assert_eq!(to_fullwidth("Hello, world.", "eng_Latn"), "Hello, world.");
/// ```
#[cfg(test)]
pub fn to_fullwidth(text: &str, tgt: &str) -> String {
    to_fullwidth_stateful(text, tgt, &mut 0)
}

/// 同 [`to_fullwidth`]，但引号奇偶计数由调用方持有，用于多个片段连续处理时让引号跨片段成对。
///
/// # 参数
/// - `text`：本片段译文。
/// - `tgt`：目标语言 FLORES 码。
/// - `quotes`：此前已转换的 `"` 个数，本函数会累加；偶数表示下一个引号是开引号。
///
/// # 返回
/// 转换后的字符串。
///
/// # 示例
/// ```ignore
/// let mut q = 0;
/// assert_eq!(to_fullwidth_stateful("他说:\"好.", "zho_Hans", &mut q), "他说：“好。");
/// assert_eq!(to_fullwidth_stateful("再见\"", "zho_Hans", &mut q), "再见”");
/// ```
pub fn to_fullwidth_stateful(text: &str, tgt: &str, quotes: &mut usize) -> String {
    if !needs_fullwidth(tgt) {
        return text.to_string();
    }
    let japanese = tgt == TGT_JPN_JPAN;
    let (open_q, close_q) = if japanese {
        ('「', '」')
    } else {
        ('“', '”')
    };
    let chars: Vec<char> = text.chars().collect();
    let mut mapped = Vec::with_capacity(chars.len());
    for (i, &c) in chars.iter().enumerate() {
        match map_punct(c, japanese) {
            Some(full) if !keep_halfwidth(&chars, i) => mapped.push(full),
            _ if c == '"' => {
                mapped.push(if (*quotes).is_multiple_of(2) {
                    open_q
                } else {
                    close_q
                });
                *quotes += 1;
            }
            _ => mapped.push(c),
        }
    }
    strip_punct_spaces(&convert_parens(&mapped))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 简写：简体中文目标。
    fn zh(s: &str) -> String {
        to_fullwidth(s, TGT_ZHO_HANS)
    }

    /// 基本逗号句号。
    #[test]
    fn basic_punctuation() {
        assert_eq!(zh("他说,好."), "他说，好。");
        assert_eq!(zh("真的吗?是的!好;行."), "真的吗？是的！好；行。");
        assert_eq!(zh("注意:小心"), "注意：小心");
    }

    /// 小数点、千分位保持半角。
    #[test]
    fn decimal_and_thousands_kept() {
        assert_eq!(zh("有4.5个月,共1,000人."), "有4.5个月，共1,000人。");
    }

    /// 缩写与英文成句里的标点保持半角。
    #[test]
    fn abbreviation_and_english_run_kept() {
        assert_eq!(
            zh("美国(U.S.)的Dr. Ehud, a教授."),
            "美国(U.S.)的Dr. Ehud, a教授。"
        );
        // 句末紧跟 ASCII 字母的句点按缩写处理（与 Python 版一致）
        assert_eq!(zh("用了iPhone."), "用了iPhone.");
    }

    /// 引号按出现顺序成对；日文用「」。
    #[test]
    fn quotes_are_paired() {
        assert_eq!(zh("他说:\"你好,世界.\""), "他说：“你好，世界。”");
        assert_eq!(
            to_fullwidth("他说\"好\"和\"坏\"", TGT_JPN_JPAN),
            "他说「好」和「坏」"
        );
        // 奇数个引号：最后一个保持为开引号
        assert_eq!(zh("他说\"好"), "他说“好");
    }

    /// 括号内含中文才转全角，纯英文保持。
    #[test]
    fn parens_only_when_cjk_inside() {
        assert_eq!(zh("苹果(水果)和(iPhone)"), "苹果（水果）和(iPhone)");
        assert_eq!(zh("(嵌套(水果))"), "(嵌套（水果）)");
    }

    /// 日文：逗号为顿号，冒号分号不转。
    #[test]
    fn japanese_rules() {
        assert_eq!(
            to_fullwidth("彼は言った,行こう.", TGT_JPN_JPAN),
            "彼は言った、行こう。"
        );
        assert_eq!(to_fullwidth("注意:あ;い", TGT_JPN_JPAN), "注意:あ;い");
    }

    /// 其它目标语言原样返回；繁体同样处理。
    #[test]
    fn target_gate() {
        assert_eq!(to_fullwidth("Hello, world.", "eng_Latn"), "Hello, world.");
        assert!(needs_fullwidth(TGT_ZHO_HANT) && !needs_fullwidth("fra_Latn"));
        assert_eq!(to_fullwidth("你好,", TGT_ZHO_HANT), "你好，");
    }

    /// 引号计数跨片段延续：前一片段开引号，后一片段的引号是闭引号。
    #[test]
    fn quote_parity_spans_segments() {
        let mut q = 0;
        assert_eq!(
            to_fullwidth_stateful("他说:\"现在不行.", TGT_ZHO_HANS, &mut q),
            "他说：“现在不行。"
        );
        assert_eq!(
            to_fullwidth_stateful("再说一遍.\"", TGT_ZHO_HANS, &mut q),
            "再说一遍。”"
        );
    }

    /// 全角标点两侧的空格被吃掉。
    #[test]
    fn spaces_around_fullwidth_removed() {
        assert_eq!(zh("你好 , 世界 ."), "你好，世界。");
        assert_eq!(zh("他说\" 你好 \""), "他说“你好”");
    }

    /// 与 Python 版逐字对拍：夹具每行 `目标码\t输入\t期望`。
    #[test]
    fn matches_python_golden() {
        let raw = include_str!("../tests/fixtures/zh_punct_golden.tsv");
        let mut n = 0;
        for line in raw.lines().filter(|l| !l.is_empty()) {
            let mut it = line.splitn(3, '\t');
            let (tgt, input, want) = (it.next().unwrap(), it.next().unwrap(), it.next().unwrap());
            assert_eq!(to_fullwidth(input, tgt), want, "输入: {input}");
            n += 1;
        }
        assert!(n >= 80, "夹具条数不足: {n}");
    }
}
