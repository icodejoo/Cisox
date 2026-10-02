//! 脚本分段识别器：按 Unicode 码点所属文字系统切分混合语言文本，不依赖任何第三方库。
//!
//! 规则（单遍扫描，不用正则）：
//! - 汉字 -> 中文；假名 -> 日语；谚文 -> 韩语；西里尔 -> 俄语；阿拉伯文 -> 阿拉伯语；
//!   希腊文、希伯来文、天城文、泰文等 `Lang` 里没有的文字 -> 未知语言（`None`）。
//! - 汉字块只要紧邻假名片段（前或后），整块判日语；纯汉字块判中文（用户指定源语言为日语时判日语）。
//! - 拉丁字母默认判英语。用户指定了拉丁语系源语言（法 / 德 / 西 / 意 / 葡 / 土 / 英）时直接用它；
//!   否则只有片段里的“特征字母”足够多才判非英语（见 [`LATIN_MIN_MARKS`]、[`LATIN_MARK_RATIO`]），
//!   特征字母集合很窄（宁可误判英语）：德语 ä ö ü ß，西语 ñ ¿ ¡，葡语 ã õ，
//!   法语 è ê à â î ô û ù ë ï œ ç，土耳其语 ı ğ ş；é 与 á í ó ú 等多语共用的字母不计分。
//! - 数字、空白、标点、符号、emoji 不单独成段：归属前一个文字片段，开头的并入后一个；
//!   整段都是中性字符时返回一个语言未知的片段。
//!
//! 短片段（OK、人名、型号）的并入由调用方用 [`crate::segment::merge_short_segments`] 完成。

use crate::Lang;
use crate::segment::{Segment, SegmentSplitter};

/// 拉丁片段判为非英语所需的最少特征字母数。
const LATIN_MIN_MARKS: usize = 2;
/// 特征字母数 * 该比例 >= 字母总数，才判为非英语（避免长英文里夹两个 café 就翻车）。
const LATIN_MARK_RATIO: usize = 16;
/// 拉丁语言候选数（德 / 西 / 葡 / 法 / 土）。
const LATIN_CANDIDATES: usize = 5;
/// 候选下标：德语。
const MARK_DE: usize = 0;
/// 候选下标：西班牙语。
const MARK_ES: usize = 1;
/// 候选下标：葡萄牙语。
const MARK_PT: usize = 2;
/// 候选下标：法语。
const MARK_FR: usize = 3;
/// 候选下标：土耳其语。
const MARK_TR: usize = 4;
/// 与候选下标一一对应的语言。
const MARK_LANGS: [Lang; LATIN_CANDIDATES] = [Lang::De, Lang::Es, Lang::Pt, Lang::Fr, Lang::Tr];

/// 汉字找假名时可跳过的拉丁片段最大字符数（去掉首尾空白后，约一个词，如 OK、PDF）。
const KANA_BRIDGE_MAX_CHARS: usize = 6;
/// 西里尔文片段可遵守 hint 的语言（`Lang` 目前只有俄语；新增乌克兰语等时在此补充）。
const CYRILLIC_LANGS: &[Lang] = &[Lang::Ru];
/// 阿拉伯文片段可遵守 hint 的语言（`Lang` 目前只有阿拉伯语；新增波斯语等时在此补充）。
const ARABIC_LANGS: &[Lang] = &[Lang::Ar];

/// 字符所属的文字系统。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Script {
    /// 汉字。
    Han,
    /// 日文假名。
    Kana,
    /// 谚文。
    Hangul,
    /// 西里尔文。
    Cyrillic,
    /// 阿拉伯文。
    Arabic,
    /// 拉丁字母（含带变音符的）。
    Latin,
    /// 其余字母文字（希腊、希伯来、天城文、泰文等），没有对应语言。
    Other,
}

/// 判断码点所属文字系统；数字、空白、标点、符号、组合附加符号等返回 `None`（中性）。
fn classify(c: char) -> Option<Script> {
    if c.is_ascii() {
        return c.is_ascii_alphabetic().then_some(Script::Latin);
    }
    if !c.is_alphabetic() {
        return None;
    }
    Some(match c {
        '\u{3400}'..='\u{4dbf}'
        | '\u{4e00}'..='\u{9fff}'
        | '\u{f900}'..='\u{faff}'
        | '\u{20000}'..='\u{2fa1f}' => Script::Han,
        '\u{3040}'..='\u{30ff}' | '\u{31f0}'..='\u{31ff}' | '\u{ff66}'..='\u{ff9f}' => Script::Kana,
        '\u{1100}'..='\u{11ff}'
        | '\u{3130}'..='\u{318f}'
        | '\u{a960}'..='\u{a97f}'
        | '\u{ac00}'..='\u{d7ff}'
        | '\u{ffa0}'..='\u{ffdc}' => Script::Hangul,
        '\u{0400}'..='\u{052f}'
        | '\u{1c80}'..='\u{1c8f}'
        | '\u{2de0}'..='\u{2dff}'
        | '\u{a640}'..='\u{a69f}' => Script::Cyrillic,
        '\u{0600}'..='\u{06ff}'
        | '\u{0750}'..='\u{077f}'
        | '\u{08a0}'..='\u{08ff}'
        | '\u{fb50}'..='\u{fdff}'
        | '\u{fe70}'..='\u{feff}' => Script::Arabic,
        '\u{00c0}'..='\u{024f}' | '\u{1e00}'..='\u{1eff}' | '\u{00aa}' | '\u{00ba}' => {
            Script::Latin
        }
        _ => Script::Other,
    })
}

/// 单个字符对应的拉丁特征语言候选下标；没有特征返回 `None`。
fn latin_mark(c: char) -> Option<usize> {
    match c.to_lowercase().next()? {
        'ä' | 'ö' | 'ü' | 'ß' => Some(MARK_DE),
        'ñ' | '¿' | '¡' => Some(MARK_ES),
        'ã' | 'õ' => Some(MARK_PT),
        'è' | 'ê' | 'à' | 'â' | 'î' | 'ô' | 'û' | 'ù' | 'ë' | 'ï' | 'œ' | 'ç' => {
            Some(MARK_FR)
        }
        'ı' | 'ğ' | 'ş' => Some(MARK_TR),
        _ => None,
    }
}

/// 当前拉丁片段的统计：字母总数与各语言特征字母数。
#[derive(Debug, Clone, Copy, Default)]
struct LatinStats {
    /// 字母总数。
    letters: usize,
    /// 各候选语言的特征字母数。
    marks: [usize; LATIN_CANDIDATES],
}

impl LatinStats {
    /// 记入一个字符（中性字符里的 ¿ ¡ 也计分）。
    fn note(&mut self, c: char) {
        self.letters += usize::from(c.is_alphabetic());
        if let Some(index) = latin_mark(c) {
            self.marks[index] += 1;
        }
    }

    /// 按保守规则给出语言：特征字母够多、占比够高且唯一领先才判非英语，否则英语。
    fn lang(&self) -> Lang {
        let (best, &count) = self
            .marks
            .iter()
            .enumerate()
            .max_by_key(|(_, count)| **count)
            .unwrap_or((0, &0));
        let unique = self.marks.iter().filter(|&&n| n == count).count() == 1;
        if unique && count >= LATIN_MIN_MARKS && count * LATIN_MARK_RATIO >= self.letters {
            MARK_LANGS[best]
        } else {
            Lang::En
        }
    }
}

/// 扫描中的一个文字片段（终点即下一个片段的起点）。
struct Span {
    /// 起始字节下标。
    start: usize,
    /// 文字系统；整段中性时为 `None`。
    script: Option<Script>,
    /// 拉丁片段已解析的语言（其他文字系统忽略）。
    latin: Lang,
}

/// 用户指定的源语言是否属于拉丁语系（可直接作为拉丁片段的语言）。
fn is_latin_lang(lang: Lang) -> bool {
    matches!(
        lang,
        Lang::En | Lang::Fr | Lang::De | Lang::Es | Lang::It | Lang::Pt | Lang::Tr
    )
}

/// 脚本分段识别器：零依赖、单遍扫描，规则见模块文档。
///
/// # 示例
/// ```rust
/// use snow_translate::script_split::ScriptSplitter;
/// use snow_translate::segment::SegmentSplitter;
/// use snow_translate::Lang;
/// let parts = ScriptSplitter.split("你好 hello world こんにちは", Lang::Auto);
/// let langs: Vec<_> = parts.iter().map(|p| p.lang).collect();
/// assert_eq!(langs, [Some(Lang::ZhHans), Some(Lang::En), Some(Lang::Ja)]);
/// ```
#[derive(Debug, Clone, Copy, Default)]
pub struct ScriptSplitter;

impl ScriptSplitter {
    /// 识别器标识。
    const NAME: &'static str = "script";

    /// 汉字片段的语言：指定中文 / 日语时跟随，否则判简体中文。
    fn han_lang(hint: Lang) -> Lang {
        match hint {
            Lang::ZhHans | Lang::ZhHant | Lang::Ja => hint,
            _ => Lang::ZhHans,
        }
    }

    /// hint 属于该文字系统的语言集合时遵守 hint，否则用默认语言。
    fn hinted(hint: Lang, family: &[Lang], default: Lang) -> Lang {
        if family.contains(&hint) {
            hint
        } else {
            default
        }
    }

    /// 收尾一个片段，拉丁片段在此决定语言。
    fn close(start: usize, script: Option<Script>, stats: &LatinStats, hint: Lang) -> Span {
        let latin = if is_latin_lang(hint) {
            hint
        } else {
            stats.lang()
        };
        Span {
            start,
            script,
            latin,
        }
    }
}

impl SegmentSplitter for ScriptSplitter {
    /// 单遍扫描切成单语片段；空文本返回空列表，拼接结果与原文逐字一致。
    fn split(&self, text: &str, hint: Lang) -> Vec<Segment> {
        if text.is_empty() {
            return Vec::new();
        }
        let mut spans: Vec<Span> = Vec::new();
        let mut current: Option<Script> = None;
        let mut start = 0;
        let mut stats = LatinStats::default();
        for (i, c) in text.char_indices() {
            if let Some(script) = classify(c)
                && current != Some(script)
            {
                if current.is_some() {
                    spans.push(Self::close(start, current, &stats, hint));
                    start = i;
                }
                current = Some(script);
                stats = LatinStats::default();
            }
            if current == Some(Script::Latin) {
                stats.note(c);
            }
        }
        spans.push(Self::close(start, current, &stats, hint));

        // 沿某方向找最近的假名：跳过无语言片段和短拉丁片段（如 "日本語 OK です" 里的 OK）。
        let kana_toward = |from: usize, forward: bool| -> bool {
            let mut i = from;
            loop {
                i = if forward {
                    i + 1
                } else {
                    match i.checked_sub(1) {
                        Some(v) => v,
                        None => return false,
                    }
                };
                let Some(s) = spans.get(i) else {
                    return false;
                };
                match s.script {
                    Some(Script::Kana) => return true,
                    None => {}
                    Some(Script::Latin) => {
                        let end = spans.get(i + 1).map_or(text.len(), |n| n.start);
                        if text[s.start..end].trim().chars().count() > KANA_BRIDGE_MAX_CHARS {
                            return false;
                        }
                    }
                    Some(_) => return false,
                }
            }
        };
        let mut out: Vec<Segment> = Vec::with_capacity(spans.len());
        for (index, span) in spans.iter().enumerate() {
            let next = spans.get(index + 1);
            let end = next.map_or(text.len(), |n| n.start);
            let lang = match span.script {
                None | Some(Script::Other) => None,
                Some(Script::Han) => {
                    // 纯汉字无假名时脚本无法区分中日文，判中文（已知局限，可用 hint 指定日语）。
                    if kana_toward(index, false) || kana_toward(index, true) {
                        Some(Lang::Ja)
                    } else {
                        Some(Self::han_lang(hint))
                    }
                }
                Some(Script::Kana) => Some(Lang::Ja),
                Some(Script::Hangul) => Some(Lang::Ko),
                Some(Script::Cyrillic) => Some(Self::hinted(hint, CYRILLIC_LANGS, Lang::Ru)),
                Some(Script::Arabic) => Some(Self::hinted(hint, ARABIC_LANGS, Lang::Ar)),
                Some(Script::Latin) => Some(span.latin),
            };
            let piece = &text[span.start..end];
            match out.last_mut() {
                Some(last) if last.lang == lang => last.text.push_str(piece),
                _ => out.push(Segment {
                    text: piece.to_string(),
                    lang,
                }),
            }
        }
        out
    }

    /// 识别器名称（参与缓存键）。
    fn name(&self) -> &'static str {
        Self::NAME
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::segment::{DEFAULT_MIN_SEGMENT_WEIGHT, merge_short_segments};

    /// 切分并断言拼接还原原文，返回 (文本, 语言) 列表。
    fn run(text: &str, hint: Lang) -> Vec<(String, Option<Lang>)> {
        let parts = ScriptSplitter.split(text, hint);
        let joined: String = parts.iter().map(|p| p.text.as_str()).collect();
        assert_eq!(joined, text, "拼接必须还原原文");
        parts.into_iter().map(|p| (p.text, p.lang)).collect()
    }

    /// 只取语言序列。
    fn langs(text: &str, hint: Lang) -> Vec<Option<Lang>> {
        run(text, hint).into_iter().map(|(_, l)| l).collect()
    }

    /// 中英日三段混写。
    #[test]
    fn chinese_english_japanese() {
        assert_eq!(
            run("你好 hello world こんにちは", Lang::Auto),
            [
                ("你好 ".to_string(), Some(Lang::ZhHans)),
                ("hello world ".to_string(), Some(Lang::En)),
                ("こんにちは".to_string(), Some(Lang::Ja)),
            ]
        );
    }

    /// 纯单语文本只有一个片段。
    #[test]
    fn pure_texts() {
        assert_eq!(langs("Hello there, friend.", Lang::Auto), [Some(Lang::En)]);
        assert_eq!(langs("今天天气很好。", Lang::Auto), [Some(Lang::ZhHans)]);
        assert_eq!(langs("안녕하세요!", Lang::Auto), [Some(Lang::Ko)]);
        assert_eq!(langs("Привет, мир", Lang::Auto), [Some(Lang::Ru)]);
        assert_eq!(langs("مرحبا بالعالم", Lang::Auto), [Some(Lang::Ar)]);
    }

    /// 日文汉字假名混写整块判日语，包括汉字块在假名之后。
    #[test]
    fn japanese_mixed_is_one_block() {
        assert_eq!(langs("私は学生です。", Lang::Auto), [Some(Lang::Ja)]);
        assert_eq!(langs("カタカナと漢字", Lang::Auto), [Some(Lang::Ja)]);
        assert_eq!(langs("ﾊﾝｶｸ漢字", Lang::Auto), [Some(Lang::Ja)]);
    }

    /// 指定繁体 / 日语源语言时汉字块跟随。
    #[test]
    fn han_follows_hint() {
        assert_eq!(langs("漢字", Lang::ZhHant), [Some(Lang::ZhHant)]);
        assert_eq!(langs("漢字", Lang::Ja), [Some(Lang::Ja)]);
        assert_eq!(langs("漢字", Lang::Ko), [Some(Lang::ZhHans)]);
    }

    /// 韩英混合。
    #[test]
    fn korean_english() {
        assert_eq!(
            run("안녕하세요 Hello world", Lang::Auto),
            [
                ("안녕하세요 ".to_string(), Some(Lang::Ko)),
                ("Hello world".to_string(), Some(Lang::En)),
            ]
        );
    }

    /// 中文夹英文术语，英文片段带着前后空格。
    #[test]
    fn chinese_with_inline_english() {
        let text = "今天我们讨论 machine learning 在医疗领域的应用";
        assert_eq!(
            run(text, Lang::Auto),
            [
                ("今天我们讨论 ".to_string(), Some(Lang::ZhHans)),
                ("machine learning ".to_string(), Some(Lang::En)),
                ("在医疗领域的应用".to_string(), Some(Lang::ZhHans)),
            ]
        );
    }

    /// 数字与标点归前一个片段，CJK 全角标点留在汉字片段里，开头的中性字符并入后一个。
    #[test]
    fn neutrals_attach_to_neighbours() {
        assert_eq!(
            run("2024年，我买了3台 MacBook Pro!", Lang::Auto),
            [
                ("2024年，我买了3台 ".to_string(), Some(Lang::ZhHans)),
                ("MacBook Pro!".to_string(), Some(Lang::En)),
            ]
        );
        assert_eq!(
            run("  (Hello) 你好", Lang::Auto),
            [
                ("  (Hello) ".to_string(), Some(Lang::En)),
                ("你好".to_string(), Some(Lang::ZhHans)),
            ]
        );
    }

    /// 空串、只有标点数字、只有 emoji：空列表或一个未知片段。
    #[test]
    fn degenerate_inputs() {
        assert!(ScriptSplitter.split("", Lang::Auto).is_empty());
        assert_eq!(langs("!?, 123 ...", Lang::Auto), [None]);
        assert_eq!(langs("😀🎉", Lang::Auto), [None]);
        assert_eq!(
            run("Great 👍 你好", Lang::Auto),
            [
                ("Great 👍 ".to_string(), Some(Lang::En)),
                ("你好".to_string(), Some(Lang::ZhHans)),
            ]
        );
    }

    /// 没有对应语言的文字系统归未知，且与相邻已知语言分开。
    #[test]
    fn unsupported_scripts_are_unknown() {
        assert_eq!(
            langs("Hello Γειά σου สวัสดี नमस्ते שלום", Lang::Auto),
            [Some(Lang::En), None]
        );
    }

    /// 拉丁启发式：特征字母足够多才判非英语。
    #[test]
    fn latin_heuristic_is_conservative() {
        assert_eq!(
            langs("Die Straße ist größer als früher", Lang::Auto),
            [Some(Lang::De)]
        );
        assert_eq!(
            langs("¿Qué hora es? ¡Mañana!", Lang::Auto),
            [Some(Lang::Es)]
        );
        assert_eq!(
            langs("Ça va très bien, où êtes-vous", Lang::Auto),
            [Some(Lang::Fr)]
        );
        assert_eq!(langs("não coração", Lang::Auto), [Some(Lang::Pt)]);
        // 单个变音字母、é 等共用字母、占比太低都保持英语
        assert_eq!(langs("Let us go to the café", Lang::Auto), [Some(Lang::En)]);
        assert_eq!(langs("résumé", Lang::Auto), [Some(Lang::En)]);
        let long = format!("{} über Müll", "plain english words ".repeat(20));
        assert_eq!(langs(&long, Lang::Auto), [Some(Lang::En)]);
        // 德法特征持平时保持英语
        assert_eq!(langs("ää èè", Lang::Auto), [Some(Lang::En)]);
    }

    /// 用户指定拉丁语系源语言时，拉丁片段直接用它。
    #[test]
    fn latin_follows_latin_hint() {
        assert_eq!(langs("Bonjour tout le monde", Lang::Fr), [Some(Lang::Fr)]);
        assert_eq!(
            langs("Hola 你好", Lang::Es),
            [Some(Lang::Es), Some(Lang::ZhHans)]
        );
    }

    /// 与短片段合并配合：夹在中文里的 OK 并入中文，三段混写保持三段。
    #[test]
    fn works_with_short_segment_merge() {
        let merged = merge_short_segments(
            ScriptSplitter.split("今天天气很好 OK 我们出去玩", Lang::Auto),
            DEFAULT_MIN_SEGMENT_WEIGHT,
        );
        assert_eq!(merged.len(), 1);
        assert_eq!(merged[0].text, "今天天气很好 OK 我们出去玩");
        let three = merge_short_segments(
            ScriptSplitter.split("你好 hello world こんにちは", Lang::Auto),
            DEFAULT_MIN_SEGMENT_WEIGHT,
        );
        assert_eq!(three.len(), 3);
    }

    /// 汉字与假名隔着短拉丁片段仍判日语；纯汉字无假名判中文（已知局限）。
    #[test]
    fn han_bridges_short_latin_to_kana() {
        let parts = run("日本語 OK です", Lang::Auto);
        assert_eq!(parts[0], ("日本語 ".to_string(), Some(Lang::Ja)));
        assert_eq!(langs("日本語", Lang::Auto), [Some(Lang::ZhHans)]);
        // 长拉丁片段不桥接
        assert_eq!(
            langs("日本語 international です", Lang::Auto)[0],
            Some(Lang::ZhHans)
        );
    }

    /// 西里尔 / 阿拉伯文 hint 属于本系统时遵守，否则用默认语言。
    #[test]
    fn cyrillic_arabic_hint() {
        assert_eq!(langs("Україна", Lang::Auto), [Some(Lang::Ru)]);
        assert_eq!(langs("Україна", Lang::Ru), [Some(Lang::Ru)]);
        assert_eq!(langs("Україна", Lang::En), [Some(Lang::Ru)]);
        assert_eq!(langs("مرحبا", Lang::Fr), [Some(Lang::Ar)]);
    }

    /// 空串、纯空白、emoji、全角数字标点的结果固定。
    #[test]
    fn neutral_only_inputs() {
        assert!(ScriptSplitter.split("", Lang::Auto).is_empty());
        assert_eq!(langs("   ", Lang::Auto), [None]);
        assert_eq!(langs("😀😀", Lang::Auto), [None]);
        assert_eq!(langs("１２３，。", Lang::Auto), [None]);
    }

    /// 识别器名称稳定。
    #[test]
    fn name_is_stable() {
        assert_eq!(ScriptSplitter.name(), "script");
    }
}
