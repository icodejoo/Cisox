//! 与模型无关的纯逻辑：分句、超长切块、贪心选词、重复检测、译文拼接。

/// 句末标点（ASCII 版需后随空白才算句末，避免切开 `3.14`）。
const ASCII_TERMINATORS: [char; 3] = ['.', '?', '!'];
/// 句末标点（全角版，后面不需要空白）。
const CJK_TERMINATORS: [char; 4] = ['。', '？', '！', '…'];
/// 输出 token 数上限相对输入的倍数。
const OUTPUT_TOKEN_RATIO: usize = 3;
/// 输出 token 数上限的固定余量。
const OUTPUT_TOKEN_SLACK: usize = 16;
/// 尾部重复检测：最大重复周期。
const REPEAT_MAX_PERIOD: usize = 4;
/// 尾部重复检测：判定退化所需的连续重复次数。
const REPEAT_MIN_TIMES: usize = 8;

/// 一个待翻译片段及其后随的分隔符（换行、空格等）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Segment {
    /// 片段正文（不含首尾空白）。
    pub text: String,
    /// 片段后面紧跟的空白分隔符。
    pub sep: String,
}

/// 收尾引号/括号，可紧跟在句末标点之后。
const CLOSERS: [char; 7] = ['"', '\'', ')', '”', '’', '」', '）'];

/// 判断 `chars[idx]` 是否为句末标点；ASCII 标点要求（跳过收尾引号后）下一字符是空白或已到结尾。
fn is_sentence_end(chars: &[char], idx: usize) -> bool {
    let c = chars[idx];
    if CJK_TERMINATORS.contains(&c) {
        return true;
    }
    if !ASCII_TERMINATORS.contains(&c) {
        return false;
    }
    let mut j = idx + 1;
    while j < chars.len() && CLOSERS.contains(&chars[j]) {
        j += 1;
    }
    chars.get(j).is_none_or(|n| n.is_whitespace())
}

/// 按段落与句末标点把文本切成片段，保留原分隔符以便还原版式。
///
/// # 参数
/// - `input`：原文。
///
/// # 返回
/// 片段列表；空白与空文本产生空列表，全部拼回（正文+分隔符）与原文仅差首部空白。
///
/// # 示例
/// ```ignore
/// let segs = split_sentences("Hi there. How are you?\nFine.");
/// assert_eq!(segs.len(), 3);
/// assert_eq!(segs[1].sep, "\n");
/// ```
pub fn split_sentences(input: &str) -> Vec<Segment> {
    let chars: Vec<char> = input.chars().collect();
    let mut segments = Vec::new();
    let mut cur = String::new();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if c == '\n' || c == '\r' {
            // 换行一定断句；连续空白并入分隔符
            let mut sep = String::new();
            while i < chars.len() && chars[i].is_whitespace() {
                sep.push(chars[i]);
                i += 1;
            }
            push_segment(&mut segments, &mut cur, sep);
            continue;
        }
        cur.push(c);
        let ended = is_sentence_end(&chars, i);
        i += 1;
        if ended {
            // 吞掉紧随的收尾引号/括号，再吞空白
            while i < chars.len() && CLOSERS.contains(&chars[i]) {
                cur.push(chars[i]);
                i += 1;
            }
            let mut sep = String::new();
            while i < chars.len() && chars[i].is_whitespace() {
                sep.push(chars[i]);
                i += 1;
            }
            push_segment(&mut segments, &mut cur, sep);
        }
    }
    push_segment(&mut segments, &mut cur, String::new());
    segments
}

/// 把当前缓冲收成一个片段；缓冲只有空白时，把分隔符并入上一个片段。
fn push_segment(segments: &mut Vec<Segment>, cur: &mut String, sep: String) {
    let text = cur.trim().to_string();
    cur.clear();
    if text.is_empty() {
        if let Some(last) = segments.last_mut() {
            last.sep.push_str(&sep);
        }
        return;
    }
    segments.push(Segment { text, sep });
}

/// 把 token id 序列切成不超过 `limit` 的块，尽量在词首边界切开。
///
/// # 参数
/// - `ids`：token id 序列（不含结束符）。
/// - `limit`：每块最大长度，为 0 时按 1 处理。
/// - `is_word_start`：判断某 id 是否是词首（Metaspace 的 `▁` 开头）。
///
/// # 返回
/// 连续且不重叠的块；拼接后等于输入。
///
/// # 示例
/// ```ignore
/// let chunks = chunk_ids(&[1, 2, 3, 4, 5], 2, |_| true);
/// assert_eq!(chunks, vec![vec![1, 2], vec![3, 4], vec![5]]);
/// ```
pub fn chunk_ids(ids: &[u32], limit: usize, is_word_start: impl Fn(u32) -> bool) -> Vec<Vec<u32>> {
    let limit = limit.max(1);
    let mut chunks = Vec::new();
    let mut start = 0;
    while start < ids.len() {
        let hard_end = (start + limit).min(ids.len());
        if hard_end == ids.len() {
            chunks.push(ids[start..].to_vec());
            break;
        }
        // 从硬上限回退到最近的词首（回退不超过一半，避免块过碎）
        let floor = start + limit / 2;
        let end = (floor.max(start + 1)..=hard_end)
            .rev()
            .find(|&e| e < ids.len() && is_word_start(ids[e]))
            .unwrap_or(hard_end);
        chunks.push(ids[start..end].to_vec());
        start = end;
    }
    chunks
}

/// 在 logits 中选最大值的下标，屏蔽 `banned` 中的 id。
///
/// # 参数
/// - `logits`：词表维度的分数。
/// - `banned`：禁止选择的 id。
///
/// # 返回
/// 最大分数的 id；空输入或全被屏蔽返回 `None`。NaN 视为最小。
///
/// # 示例
/// ```ignore
/// assert_eq!(argmax_masked(&[0.1, 0.9, 0.5], &[1]), Some(2));
/// ```
pub fn argmax_masked(logits: &[f32], banned: &[i64]) -> Option<usize> {
    let mut best: Option<(usize, f32)> = None;
    for (i, &v) in logits.iter().enumerate() {
        if v.is_nan() || banned.contains(&(i as i64)) {
            continue;
        }
        if best.is_none_or(|(_, b)| v > b) {
            best = Some((i, v));
        }
    }
    best.map(|(i, _)| i)
}

/// 计算一个片段允许生成的最大 token 数。
///
/// # 参数
/// - `input_tokens`：该片段的输入 token 数。
/// - `user_cap`：调用方/清单给出的上限。
///
/// # 返回
/// `min(user_cap, 输入×3+余量)`，至少为 1。
///
/// # 示例
/// ```ignore
/// assert_eq!(output_token_budget(10, 512), 46);
/// ```
pub fn output_token_budget(input_tokens: usize, user_cap: usize) -> usize {
    let by_input = input_tokens
        .saturating_mul(OUTPUT_TOKEN_RATIO)
        .saturating_add(OUTPUT_TOKEN_SLACK);
    by_input.min(user_cap).max(1)
}

/// 检测并裁剪生成序列尾部的短周期重复（int8 模型常见退化）。
///
/// # 参数
/// - `tokens`：已生成的 token，命中时被就地裁短，只保留一个周期。
///
/// # 返回
/// 尾部存在周期 ≤4、连续重复 ≥8 次的模式时返回 `true`（并已裁剪）。
///
/// # 示例
/// ```ignore
/// let mut t = vec![5; 10];
/// assert!(trim_tail_repeat(&mut t));
/// assert_eq!(t, vec![5]);
/// ```
pub fn trim_tail_repeat(tokens: &mut Vec<u32>) -> bool {
    for period in 1..=REPEAT_MAX_PERIOD {
        let need = period * REPEAT_MIN_TIMES;
        if tokens.len() < need {
            continue;
        }
        let tail = &tokens[tokens.len() - need..];
        if tail.chunks(period).all(|c| c == &tail[..period]) {
            tokens.truncate(tokens.len() - period * (REPEAT_MIN_TIMES - 1));
            return true;
        }
    }
    false
}

/// 半角标点 → 全角标点（仅当前一字符是汉字时转换）。
const HALF_TO_FULL: [(char, char); 5] = [
    (',', '，'),
    ('?', '？'),
    ('!', '！'),
    (':', '：'),
    (';', '；'),
];

/// 汉字/假名/谚文（不含标点）。
fn is_cjk_letter(c: char) -> bool {
    matches!(c, '\u{4e00}'..='\u{9fff}' | '\u{3400}'..='\u{4dbf}' | '\u{3040}'..='\u{30ff}' | '\u{ac00}'..='\u{d7af}')
}

/// CJK 标点与全角符号。
fn is_cjk_punct(c: char) -> bool {
    matches!(c, '\u{3000}'..='\u{303f}' | '\u{ff00}'..='\u{ffef}')
}

/// 整理 Marian 中文输出的排版：汉字后的半角 `,?!:;` 转全角；
/// 全角标点两侧、汉字与汉字之间的多余空格删除；汉字与拉丁字母/数字之间的空格保留。
///
/// # 参数
/// - `s`：模型解码出的中文文本。
///
/// # 返回
/// 整理后的文本。
///
/// # 示例
/// ```ignore
/// assert_eq!(tidy_cjk_spacing("请保存 。"), "请保存。");
/// ```
pub fn tidy_cjk_spacing(s: &str) -> String {
    let chars: Vec<char> = s.chars().collect();
    let mut out: Vec<char> = Vec::with_capacity(chars.len());
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if c.is_whitespace() {
            let mut j = i;
            while j < chars.len() && chars[j].is_whitespace() {
                j += 1;
            }
            let prev = out.last().copied();
            let next = chars.get(j).copied();
            let drop = match (prev, next) {
                (Some(p), Some(n)) => {
                    is_cjk_punct(p) || is_cjk_punct(n) || (is_cjk_letter(p) && is_cjk_letter(n))
                }
                _ => false,
            };
            if !drop {
                out.extend_from_slice(&chars[i..j]);
            }
            i = j;
            continue;
        }
        let converted = HALF_TO_FULL
            .iter()
            .find(|(half, _)| *half == c)
            .filter(|_| out.last().is_some_and(|p| is_cjk_letter(*p)))
            .map(|(_, full)| *full);
        out.push(converted.unwrap_or(c));
        i += 1;
    }
    out.into_iter().collect()
}

/// 判断语言代码是否为中日韩（译文里句间不需要空格）。
///
/// # 参数
/// - `lang`：语言代码，如 `zh-CN`。
///
/// # 返回
/// 前缀为 zh/ja/ko 返回 `true`。
///
/// # 示例
/// ```ignore
/// assert!(is_cjk_lang("zh-TW"));
/// ```
pub fn is_cjk_lang(lang: &str) -> bool {
    let lower = lang.to_ascii_lowercase();
    ["zh", "ja", "ko"].iter().any(|p| lower.starts_with(p))
}

/// 把译文片段按原分隔符拼回；目标为 CJK 时，纯空格分隔符省略。
///
/// # 参数
/// - `parts`：`(译文, 原分隔符)` 列表。
/// - `cjk_target`：目标语言是否为 CJK。
///
/// # 返回
/// 拼接后的完整译文。
///
/// # 示例
/// ```ignore
/// let out = join_translated(&[("你好。".into(), " ".into()), ("再见。".into(), "".into())], true);
/// assert_eq!(out, "你好。再见。");
/// ```
pub fn join_translated(parts: &[(String, String)], cjk_target: bool) -> String {
    let mut out = String::new();
    for (text, sep) in parts {
        out.push_str(text);
        let only_spaces = sep.chars().all(|c| c == ' ' || c == '\t');
        if cjk_target && only_spaces {
            continue;
        }
        out.push_str(sep);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 取片段正文列表，方便断言。
    fn texts(segs: &[Segment]) -> Vec<&str> {
        segs.iter().map(|s| s.text.as_str()).collect()
    }

    /// 基本分句与分隔符保留。
    #[test]
    fn splits_on_terminators_and_newlines() {
        let segs = split_sentences("Hi there. How are you?\nFine!  Thanks.");
        assert_eq!(
            texts(&segs),
            ["Hi there.", "How are you?", "Fine!", "Thanks."]
        );
        assert_eq!(segs[0].sep, " ");
        assert_eq!(segs[1].sep, "\n");
        assert_eq!(segs[2].sep, "  ");
    }

    /// 小数与缩写内部的点不切分（点后无空白）。
    #[test]
    fn keeps_decimals_intact() {
        let segs = split_sentences("Pi is 3.14 today.");
        assert_eq!(texts(&segs), ["Pi is 3.14 today."]);
    }

    /// 全角句末标点后不需要空白也切分；收尾引号并入前一句。
    #[test]
    fn cjk_terminators_and_quotes() {
        let segs = split_sentences("你好。再见！He said \"go.\" Next.");
        assert_eq!(
            texts(&segs),
            ["你好。", "再见！", "He said \"go.\"", "Next."]
        );
    }

    /// 空文本、纯空白、连续空行不产生空片段。
    #[test]
    fn empty_and_blank_inputs() {
        assert!(split_sentences("").is_empty());
        assert!(split_sentences("  \n\n  ").is_empty());
        let segs = split_sentences("A.\n\n\nB.");
        assert_eq!(texts(&segs), ["A.", "B."]);
        assert_eq!(segs[0].sep, "\n\n\n");
    }

    /// 没有句末标点的长文本整体作为一个片段。
    #[test]
    fn no_terminator_single_segment() {
        let segs = split_sentences("no punctuation here");
        assert_eq!(texts(&segs), ["no punctuation here"]);
    }

    /// 切块：每块不超限，拼接后还原。
    #[test]
    fn chunk_ids_respects_limit_and_roundtrips() {
        let ids: Vec<u32> = (0..23).collect();
        let chunks = chunk_ids(&ids, 5, |_| false);
        assert!(chunks.iter().all(|c| c.len() <= 5 && !c.is_empty()));
        let flat: Vec<u32> = chunks.concat();
        assert_eq!(flat, ids);
    }

    /// 切块优先在词首边界断开。
    #[test]
    fn chunk_ids_prefers_word_boundary() {
        // 词首为 3 的倍数：[0,1,2 | 3,4,5 | 6,7,8]，limit=5 应回退到 3 / 6
        let ids: Vec<u32> = (0..9).collect();
        let chunks = chunk_ids(&ids, 5, |id| id % 3 == 0);
        assert_eq!(chunks, vec![vec![0, 1, 2], vec![3, 4, 5], vec![6, 7, 8]]);
    }

    /// 切块边界：空输入、limit 为 0 不 panic。
    #[test]
    fn chunk_ids_edge_cases() {
        assert!(chunk_ids(&[], 4, |_| true).is_empty());
        let chunks = chunk_ids(&[1, 2, 3], 0, |_| false);
        assert_eq!(chunks.concat(), vec![1, 2, 3]);
        assert_eq!(chunk_ids(&[1, 2, 3], 10, |_| true), vec![vec![1, 2, 3]]);
    }

    /// argmax：屏蔽、NaN、空输入。
    #[test]
    fn argmax_masked_cases() {
        assert_eq!(argmax_masked(&[0.1, 0.9, 0.5], &[]), Some(1));
        assert_eq!(argmax_masked(&[0.1, 0.9, 0.5], &[1]), Some(2));
        assert_eq!(argmax_masked(&[f32::NAN, 0.2], &[]), Some(1));
        assert_eq!(argmax_masked(&[], &[]), None);
        assert_eq!(argmax_masked(&[1.0], &[0]), None);
    }

    /// 输出预算：随输入放大且被上限截断，最小为 1。
    #[test]
    fn output_budget() {
        assert_eq!(output_token_budget(10, 512), 46);
        assert_eq!(output_token_budget(1000, 512), 512);
        assert_eq!(output_token_budget(0, 0), 1);
    }

    /// 重复检测：短周期重复命中并裁剪，正常序列不命中。
    #[test]
    fn tail_repeat_detection() {
        let mut t = vec![5; 8];
        assert!(trim_tail_repeat(&mut t));
        assert_eq!(t, vec![5]);
        let mut t = vec![1, 2, 3, 4, 5, 6, 5, 6, 5, 6, 5, 6, 5, 6, 5, 6, 5, 6, 5, 6];
        assert!(trim_tail_repeat(&mut t));
        assert_eq!(t, vec![1, 2, 3, 4, 5, 6]);
        let mut short = vec![5; 7];
        assert!(!trim_tail_repeat(&mut short));
        let mut seq: Vec<u32> = (0..100).collect();
        assert!(!trim_tail_repeat(&mut seq));
        assert!(!trim_tail_repeat(&mut Vec::new()));
    }

    /// 中文排版整理：全角标点两侧去空格、汉字后半角标点转全角、汉字与拉丁之间空格保留。
    #[test]
    fn tidy_cjk_spacing_rules() {
        assert_eq!(tidy_cjk_spacing("请保存 。"), "请保存。");
        assert_eq!(
            tidy_cjk_spacing("错误: 文件无法打开, 因为它"),
            "错误：文件无法打开，因为它"
        );
        assert_eq!(
            tidy_cjk_spacing("非常感谢你的帮助 我真的很感激!"),
            "非常感谢你的帮助我真的很感激！"
        );
        assert_eq!(tidy_cjk_spacing("使用 Python 编程"), "使用 Python 编程");
        assert_eq!(
            tidy_cjk_spacing("Hello, world: 12:30"),
            "Hello, world: 12:30"
        );
        assert_eq!(tidy_cjk_spacing(""), "");
        assert_eq!(tidy_cjk_spacing("  "), "  ");
    }

    /// CJK 语言判定。
    #[test]
    fn cjk_lang_detection() {
        assert!(is_cjk_lang("zh-CN"));
        assert!(is_cjk_lang("JA"));
        assert!(!is_cjk_lang("en"));
    }

    /// 拼接：CJK 目标去掉纯空格分隔符，保留换行。
    #[test]
    fn join_rules() {
        let parts = vec![
            ("你好。".to_string(), " ".to_string()),
            ("再见。".to_string(), "\n".to_string()),
            ("好。".to_string(), String::new()),
        ];
        assert_eq!(join_translated(&parts, true), "你好。再见。\n好。");
        assert_eq!(join_translated(&parts, false), "你好。 再见。\n好。");
        assert_eq!(join_translated(&[], true), "");
    }
}
