//! 混合语言文本的分段接口：识别器可替换，这里只定义片段结构、识别器 trait 与短片段后处理。
//!
//! 具体的语言识别（把一段文字切成单语片段）交给第三方库实现 [`SegmentSplitter`]；本模块不含任何
//! 脚本区间切分。没有装识别器时用 [`NoSplit`] 占位：整段文本当作一个“语言未知”的片段，
//! 混合拆分模式随之退化为“专用包优先”的整段翻译。

use crate::Lang;

/// 片段太短时并入邻居的权重阈值（见 [`segment_weight`]）。
pub const DEFAULT_MIN_SEGMENT_WEIGHT: usize = 3;
/// 汉字 / 假名 / 谚文单字的权重（一个字承载的信息约等于拉丁文字的一个短词）。
const CJK_CHAR_WEIGHT: usize = 3;
/// 汉字区间。
const HAN_RANGE: std::ops::RangeInclusive<char> = '\u{4e00}'..='\u{9fff}';
/// 日文假名区间。
const KANA_RANGE: std::ops::RangeInclusive<char> = '\u{3040}'..='\u{30ff}';
/// 谚文音节区间。
const HANGUL_RANGE: std::ops::RangeInclusive<char> = '\u{ac00}'..='\u{d7af}';

/// 单语片段：原文中连续的一段文字（含首尾空白与标点）及其语言。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Segment {
    /// 片段原文；所有片段按顺序拼接必须还原整段文本。
    pub text: String,
    /// 识别出的语言；`None` 表示识别器没有把握。
    pub lang: Option<Lang>,
}

/// 语言识别 / 分段接口：把一段文本切成按顺序拼接可还原原文的单语片段。
///
/// 实现负责自己的内存策略：识别器应在首次使用时才构造，空闲时释放，不要常驻。
///
/// # 示例
/// ```rust
/// use snow_translate::segment::{NoSplit, SegmentSplitter};
/// use snow_translate::Lang;
/// let parts = NoSplit.split("你好 hello", Lang::Auto);
/// assert_eq!(parts.len(), 1);
/// assert_eq!(parts[0].text, "你好 hello");
/// ```
pub trait SegmentSplitter: Send + Sync {
    /// 把文本切成单语片段。
    ///
    /// # 参数
    /// - `text`：待切分文本。
    /// - `hint`：用户配置的源语言（可为 `Auto`），识别器可用来消歧（例如简繁、拉丁语系）。
    ///
    /// # 返回
    /// 片段列表，按顺序拼接等于 `text`；空文本返回空列表。
    fn split(&self, text: &str, hint: Lang) -> Vec<Segment>;

    /// 识别器标识，参与翻译结果缓存键（换识别器后不复用旧译文）。
    fn name(&self) -> &'static str;
}

/// 占位识别器：不做任何识别，整段文本作为一个语言未知的片段。
#[derive(Debug, Clone, Copy, Default)]
pub struct NoSplit;

impl SegmentSplitter for NoSplit {
    /// 整段作为一个片段（空文本返回空列表）。
    fn split(&self, text: &str, _hint: Lang) -> Vec<Segment> {
        if text.is_empty() {
            return Vec::new();
        }
        vec![Segment {
            text: text.to_string(),
            lang: None,
        }]
    }

    /// 占位识别器名称。
    fn name(&self) -> &'static str {
        "none"
    }
}

/// 是否同一种语言（简繁中文视为同一种，不做简繁转换）。
///
/// # 参数
/// - `a` / `b`：两种语言。
///
/// # 示例
/// ```rust
/// use snow_translate::segment::same_language;
/// use snow_translate::Lang;
/// assert!(same_language(Lang::ZhHans, Lang::ZhHant));
/// assert!(!same_language(Lang::En, Lang::Auto));
/// ```
pub fn same_language(a: Lang, b: Lang) -> bool {
    let chinese = |lang: Lang| matches!(lang, Lang::ZhHans | Lang::ZhHant);
    a == b || (chinese(a) && chinese(b))
}

/// 把片段拆成 `(前导空白, 正文, 尾部空白)`，翻译只送正文，空白原样保留。
///
/// # 示例
/// ```rust
/// use snow_translate::segment::split_whitespace_edges;
/// assert_eq!(split_whitespace_edges("  hi \n"), ("  ", "hi", " \n"));
/// assert_eq!(split_whitespace_edges("   "), ("   ", "", ""));
/// ```
pub fn split_whitespace_edges(text: &str) -> (&str, &str, &str) {
    let core = text.trim();
    if core.is_empty() {
        return (text, "", "");
    }
    let start = text.len() - text.trim_start().len();
    let end = text.trim_end().len();
    (&text[..start], core, &text[end..])
}

/// 片段“分量”：字母类字符数，汉字 / 假名 / 谚文每字按 [`CJK_CHAR_WEIGHT`] 计。
///
/// # 示例
/// ```rust
/// use snow_translate::segment::segment_weight;
/// assert_eq!(segment_weight("OK, "), 2);
/// assert_eq!(segment_weight("你好"), 6);
/// ```
pub fn segment_weight(text: &str) -> usize {
    text.chars()
        .map(|c| {
            if HAN_RANGE.contains(&c) || KANA_RANGE.contains(&c) || HANGUL_RANGE.contains(&c) {
                CJK_CHAR_WEIGHT
            } else {
                usize::from(c.is_alphabetic())
            }
        })
        .sum()
}

/// 对识别器输出做后处理：分量低于阈值的短片段（OK、人名、型号）并入相邻片段，相邻同语言片段合并。
///
/// 每轮取分量最小的短片段并入分量更大的邻居（并列时取前一个），直到没有短片段或只剩一个。
/// 并入后沿用邻居的语言；拼接顺序不变，因此整体仍可还原原文。
///
/// # 参数
/// - `segments`：识别器给出的片段。
/// - `min_weight`：分量阈值，`0` 表示不并入。
///
/// # 返回
/// 处理后的片段列表。
///
/// # 示例
/// ```rust
/// use snow_translate::segment::{Segment, merge_short_segments};
/// use snow_translate::Lang;
/// let parts = vec![
///     Segment { text: "OK ".into(), lang: Some(Lang::En) },
///     Segment { text: "你好世界".into(), lang: Some(Lang::ZhHans) },
/// ];
/// let merged = merge_short_segments(parts, 3);
/// assert_eq!(merged.len(), 1);
/// assert_eq!(merged[0].text, "OK 你好世界");
/// ```
pub fn merge_short_segments(segments: Vec<Segment>, min_weight: usize) -> Vec<Segment> {
    let mut parts = coalesce(segments);
    while parts.len() > 1 {
        let weights: Vec<usize> = parts.iter().map(|p| segment_weight(&p.text)).collect();
        let Some((short, _)) = weights
            .iter()
            .enumerate()
            .filter(|(_, w)| **w < min_weight)
            .min_by_key(|(i, w)| (**w, *i))
        else {
            break;
        };
        let into_previous = match (short.checked_sub(1), short + 1 < parts.len()) {
            (Some(prev), true) => weights[prev] >= weights[short + 1],
            (Some(_), false) => true,
            (None, _) => false,
        };
        let target = if into_previous { short - 1 } else { short + 1 };
        let absorbed = parts.remove(short);
        // 并入前一个：追加到其尾部；并入后一个：补到其头部（移除后下标不变）
        let neighbour = if into_previous {
            &mut parts[target]
        } else {
            &mut parts[short]
        };
        if into_previous {
            neighbour.text.push_str(&absorbed.text);
        } else {
            neighbour.text.insert_str(0, &absorbed.text);
        }
        parts = coalesce(parts);
    }
    parts
}

/// 合并相邻且语言相同的片段，并丢弃空片段。
fn coalesce(segments: Vec<Segment>) -> Vec<Segment> {
    let mut out: Vec<Segment> = Vec::with_capacity(segments.len());
    for segment in segments.into_iter().filter(|s| !s.text.is_empty()) {
        match out.last_mut() {
            Some(last) if last.lang == segment.lang => last.text.push_str(&segment.text),
            _ => out.push(segment),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 造片段。
    fn seg(text: &str, lang: Lang) -> Segment {
        Segment {
            text: text.into(),
            lang: Some(lang),
        }
    }

    /// 拼接所有片段。
    fn joined(parts: &[Segment]) -> String {
        parts.iter().map(|p| p.text.as_str()).collect()
    }

    /// 占位识别器：整段一个片段，空文本无片段。
    #[test]
    fn no_split_is_single_unknown_segment() {
        let parts = NoSplit.split("你好 hello", Lang::En);
        assert_eq!(
            parts,
            vec![Segment {
                text: "你好 hello".into(),
                lang: None
            }]
        );
        assert!(NoSplit.split("", Lang::Auto).is_empty());
    }

    /// 简繁中文同语言；Auto 与任何语言都不同。
    #[test]
    fn same_language_rules() {
        assert!(same_language(Lang::En, Lang::En));
        assert!(same_language(Lang::ZhHans, Lang::ZhHant));
        assert!(!same_language(Lang::ZhHans, Lang::Ja));
        assert!(!same_language(Lang::Auto, Lang::En));
    }

    /// 首尾空白被剥离且可无损还原；纯空白整体归前导。
    #[test]
    fn whitespace_edges_round_trip() {
        for text in ["  hi \n", "hi", " a b ", "   ", ""] {
            let (lead, core, trail) = split_whitespace_edges(text);
            assert_eq!(format!("{lead}{core}{trail}"), text);
        }
        assert_eq!(split_whitespace_edges(" a b "), (" ", "a b", " "));
    }

    /// 分量：标点数字不计，汉字按权重。
    #[test]
    fn weight_counts_letters_and_cjk() {
        assert_eq!(segment_weight("OK, 123!"), 2);
        assert_eq!(segment_weight("你好"), 2 * CJK_CHAR_WEIGHT);
        assert_eq!(segment_weight("こん한"), 3 * CJK_CHAR_WEIGHT);
        assert_eq!(segment_weight(""), 0);
    }

    /// 夹在中文里的短英文并入中文，并保持原文顺序与标点。
    #[test]
    fn short_latin_between_chinese_merges() {
        let parts = vec![
            seg("我觉得 ", Lang::ZhHans),
            seg("OK ", Lang::En),
            seg("没问题。", Lang::ZhHans),
        ];
        let merged = merge_short_segments(parts, DEFAULT_MIN_SEGMENT_WEIGHT);
        assert_eq!(merged, vec![seg("我觉得 OK 没问题。", Lang::ZhHans)]);
    }

    /// 开头的短片段并入后一个；结尾的短片段并入前一个。
    #[test]
    fn short_edges_merge_inward() {
        let head = merge_short_segments(
            vec![seg("Hi ", Lang::En), seg("今天天气很好。", Lang::ZhHans)],
            4,
        );
        assert_eq!(head, vec![seg("Hi 今天天气很好。", Lang::ZhHans)]);
        let tail = merge_short_segments(
            vec![seg("The weather is nice. ", Lang::En), seg("ok", Lang::Fr)],
            3,
        );
        assert_eq!(tail, vec![seg("The weather is nice. ok", Lang::En)]);
    }

    /// 足够长的片段不并入；三段混合保持三段。
    #[test]
    fn long_segments_stay_split() {
        let parts = vec![
            seg("你好 ", Lang::ZhHans),
            seg("hello world ", Lang::En),
            seg("こんにちは", Lang::Ja),
        ];
        let merged = merge_short_segments(parts.clone(), DEFAULT_MIN_SEGMENT_WEIGHT);
        assert_eq!(merged, parts);
        assert_eq!(joined(&merged), "你好 hello world こんにちは");
    }

    /// 短片段并入分量更大的邻居；并入后相邻同语言片段合并。
    #[test]
    fn merges_into_heavier_neighbour_and_coalesces() {
        let parts = vec![
            seg("Good morning everyone, ", Lang::En),
            seg("ok ", Lang::De),
            seg("今天开会。", Lang::ZhHans),
        ];
        // “ok ” 分量 2，前邻居 19 > 后邻居 12，并入前面的英文
        let merged = merge_short_segments(parts, DEFAULT_MIN_SEGMENT_WEIGHT);
        assert_eq!(
            merged,
            vec![
                seg("Good morning everyone, ok ", Lang::En),
                seg("今天开会。", Lang::ZhHans)
            ]
        );
        // 相邻同语言片段直接合并
        let same =
            merge_short_segments(vec![seg("abc def ", Lang::En), seg("ghi jkl", Lang::En)], 0);
        assert_eq!(same, vec![seg("abc def ghi jkl", Lang::En)]);
    }

    /// 阈值为 0 不并入；只有一个片段、空输入、全是短片段都不会死循环或丢文本。
    #[test]
    fn degenerate_inputs_are_safe() {
        let parts = vec![seg("a ", Lang::En), seg("é", Lang::Fr)];
        assert_eq!(merge_short_segments(parts.clone(), 0), parts);
        assert_eq!(
            merge_short_segments(vec![seg("a", Lang::En)], 99),
            vec![seg("a", Lang::En)]
        );
        assert!(merge_short_segments(Vec::new(), 3).is_empty());
        let all_short = merge_short_segments(
            vec![seg("a ", Lang::En), seg("é ", Lang::Fr), seg("ü", Lang::De)],
            99,
        );
        assert_eq!(all_short.len(), 1);
        assert_eq!(all_short[0].text, "a é ü");
    }
}
