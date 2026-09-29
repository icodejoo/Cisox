//! `.ts` → `.ftl` 转换规则：消息 id 命名、占位符、复数、转义。

use crate::ts::{TsCatalog, TsContext, TsMessage, TsStatus};
use std::collections::{BTreeMap, BTreeSet};

/// 产品名在 Fluent 中的变量名（约定 11）。
pub const PRODUCT_VAR: &str = "product";

/// 旧版文案里内嵌的产品名字面量，转换时替换为 `{ $product }`。
pub const LEGACY_PRODUCT_LITERAL: &str = "Snow Shot";

/// 复数参数变量名（对应 Qt 的 `%n`）。
pub const PLURAL_VAR: &str = "n";

/// slug 最大长度。
const SLUG_MAX: usize = 32;

/// 计算 FNV-1a 32 位哈希。
fn fnv1a(data: &[u8]) -> u32 {
    data.iter().fold(0x811c_9dc5u32, |h, b| {
        (h ^ u32::from(*b)).wrapping_mul(0x0100_0193)
    })
}

/// 把标识拆成小写 kebab：驼峰断词，非字母数字折叠为 `-`。
fn kebab(s: &str) -> String {
    let mut out = String::new();
    let mut prev_lower = false;
    for c in s.chars() {
        if c.is_ascii_alphanumeric() {
            if c.is_ascii_uppercase() && prev_lower && !out.ends_with('-') {
                out.push('-');
            }
            out.push(c.to_ascii_lowercase());
            prev_lower = c.is_ascii_lowercase() || c.is_ascii_digit();
        } else {
            if !out.is_empty() && !out.ends_with('-') {
                out.push('-');
            }
            prev_lower = false;
        }
    }
    out.trim_end_matches('-').to_string()
}

/// 由（上下文, 源文）生成稳定的消息 id。
///
/// 形如 `<上下文kebab>-<源文slug>-<8位哈希>`；哈希基于原始上下文与源文，
/// 因此同一源文永远得到同一 id，与目录里其他条目无关。
///
/// # 参数
/// - `context`：Qt 上下文名。
/// - `source`：英文源文。
///
/// # 返回
/// 合法的 Fluent 标识符。
///
/// # 示例
/// ```
/// let id = snow_i18n::convert::message_id("Settings", "Save");
/// assert!(id.starts_with("settings-save-"));
/// ```
pub fn message_id(context: &str, source: &str) -> String {
    let mut ctx = kebab(context);
    if ctx.is_empty() || !ctx.starts_with(|c: char| c.is_ascii_alphabetic()) {
        ctx = format!("c-{ctx}");
    }
    let mut slug = kebab(source);
    if slug.len() > SLUG_MAX {
        slug.truncate(SLUG_MAX);
        slug = slug.trim_end_matches('-').to_string();
    }
    if slug.is_empty() {
        slug = "msg".to_string();
    }
    let mut key = context.as_bytes().to_vec();
    key.push(0);
    key.extend_from_slice(source.as_bytes());
    format!("{ctx}-{slug}-{:08x}", fnv1a(&key))
}

/// 文本片段：普通文本或 Fluent 表达式。
enum Piece {
    Text(String),
    Expr(String),
}

/// 把 Qt 文案切分为片段，并映射占位符与产品名。
fn tokenize(text: &str, stats: &mut ValueInfo) -> Vec<Piece> {
    let mut pieces = Vec::new();
    let mut buf = String::new();
    let mut i = 0;
    while i < text.len() {
        let rest = &text[i..];
        let mut expr: Option<(String, usize)> = None;
        if rest.starts_with(LEGACY_PRODUCT_LITERAL) {
            stats.product += 1;
            expr = Some((format!("${PRODUCT_VAR}"), LEGACY_PRODUCT_LITERAL.len()));
        } else if let Some(r) = rest.strip_prefix('%') {
            let (skip_l, digits_src) = match r.strip_prefix('L') {
                Some(x) => (1, x),
                None => (0, r),
            };
            if digits_src.starts_with('n') {
                stats.has_n = true;
                expr = Some((format!("${PLURAL_VAR}"), 2 + skip_l));
            } else {
                let digits: String = digits_src
                    .chars()
                    .take(2)
                    .take_while(char::is_ascii_digit)
                    .collect();
                if !digits.is_empty() && digits != "0" && !digits.starts_with('0') {
                    stats.args.insert(digits.clone());
                    expr = Some((format!("$arg{digits}"), 1 + skip_l + digits.len()));
                }
            }
        }
        match expr {
            Some((e, used)) => {
                if !buf.is_empty() {
                    pieces.push(Piece::Text(std::mem::take(&mut buf)));
                }
                pieces.push(Piece::Expr(e));
                i += used;
            }
            None => {
                let c = rest.chars().next().unwrap_or_default();
                buf.push(c);
                i += c.len_utf8();
            }
        }
    }
    if !buf.is_empty() {
        pieces.push(Piece::Text(buf));
    }
    pieces
}

/// 一次值转换收集到的信息。
#[derive(Default)]
struct ValueInfo {
    /// 产品名替换次数。
    product: usize,
    /// 出现 `%n`。
    has_n: bool,
    /// 出现的 `%N` 编号。
    args: BTreeSet<String>,
}

/// 生成字符串字面量表达式 `{ "..." }`。
fn literal(s: &str) -> String {
    let mut out = String::from("{ \"");
    for c in s.chars() {
        if (c as u32) < 0x20 || c == '\u{7f}' {
            out.push_str(&format!("\\u{:04x}", c as u32));
        } else {
            out.push(c);
        }
    }
    out.push_str("\" }");
    out
}

/// 转义单行里的普通文本：花括号与控制字符走字面量。
fn escape_text(s: &str) -> String {
    let mut out = String::new();
    for c in s.chars() {
        match c {
            '{' | '}' => out.push_str(&literal(&c.to_string())),
            c if (c as u32) < 0x20 || c == '\u{7f}' => out.push_str(&literal(&c.to_string())),
            c => out.push(c),
        }
    }
    out
}

/// 把片段序列渲染为若干行 Fluent 内联文本。
fn render_lines(pieces: &[Piece]) -> Vec<String> {
    // 先把片段按 '\n' 切成行，每行是 (是否文本, 内容) 序列。
    let mut lines: Vec<Vec<(bool, String)>> = vec![Vec::new()];
    for p in pieces {
        match p {
            Piece::Expr(e) => lines
                .last_mut()
                .expect("至少一行")
                .push((false, format!("{{ {e} }}"))),
            Piece::Text(t) => {
                for (idx, part) in t.split('\n').enumerate() {
                    if idx > 0 {
                        lines.push(Vec::new());
                    }
                    if !part.is_empty() {
                        lines
                            .last_mut()
                            .expect("至少一行")
                            .push((true, part.to_string()));
                    }
                }
            }
        }
    }
    lines.into_iter().map(render_line).collect()
}

/// 渲染单行：保护首尾空白与行首特殊字符。
fn render_line(mut parts: Vec<(bool, String)>) -> String {
    if parts.is_empty() {
        return literal("");
    }
    let mut head = String::new();
    if let Some((true, first)) = parts.first_mut() {
        let trimmed = first.trim_start_matches(' ');
        let lead = first.len() - trimmed.len();
        if lead > 0 {
            head.push_str(&literal(&first[..lead]));
            *first = trimmed.to_string();
        }
        if let Some(c) = first
            .chars()
            .next()
            .filter(|c| matches!(c, '.' | '*' | '['))
        {
            head.push_str(&literal(&c.to_string()));
            first.remove(0);
        }
    }
    let mut tail = String::new();
    if let Some((true, last)) = parts.last_mut() {
        let trimmed = last.trim_end_matches(' ');
        let trail = last.len() - trimmed.len();
        if trail > 0 {
            tail = literal(&last[trimmed.len()..]);
            *last = trimmed.to_string();
        }
    }
    let body: String = parts
        .iter()
        .map(|(is_text, s)| if *is_text { escape_text(s) } else { s.clone() })
        .collect();
    format!("{head}{body}{tail}")
}

/// 转换问题类别。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IssueKind {
    /// 未完成条目，已跳过。
    SkippedUnfinished,
    /// 过时/消失条目，已跳过。
    SkippedObsolete,
    /// 译文为空，已跳过。
    SkippedEmpty,
    /// 同一（上下文, 源文）重复，保留首个。
    Duplicate,
    /// 复数形式数量无法映射，需人工处理。
    NeedsManualPlural,
    /// 消息 id 与另一条冲突，需人工处理。
    IdCollision,
    /// 源文与译文的占位符集合不一致（已输出，仅提示）。
    PlaceholderMismatch,
}

/// 转换过程中的一条问题记录。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConvertIssue {
    /// 问题类别。
    pub kind: IssueKind,
    /// 所属上下文。
    pub context: String,
    /// 源文。
    pub source: String,
}

/// 转换统计。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ConvertStats {
    /// 输入消息总数。
    pub total: usize,
    /// 成功输出的消息数。
    pub converted: usize,
    /// 其中复数消息数。
    pub plural: usize,
    /// 产品名被替换为变量的次数。
    pub product_replaced: usize,
    /// 含 `%N` 占位符的消息数。
    pub with_args: usize,
}

/// 转换结果。
#[derive(Debug, Clone, Default)]
pub struct ConvertOutput {
    /// 生成的 `.ftl` 文本（LF 换行）。
    pub ftl: String,
    /// 统计。
    pub stats: ConvertStats,
    /// 问题清单。
    pub issues: Vec<ConvertIssue>,
}

/// 复数形式数量对应的 CLDR 类别；无法映射返回 `None`。
fn plural_categories(count: usize) -> Option<&'static [&'static str]> {
    match count {
        2 => Some(&["one", "other"]),
        _ => None,
    }
}

/// 把 `.ts` 目录转换为 `.ftl`。
///
/// 规则：id 见 [`message_id`]；`%N` → `{ $argN }`；`%n` → `{ $n }`；
/// 两种复数形式 → `one/other` 选择器，单形式直接输出；`Snow Shot` → `{ $product }`；
/// `&` 视为字面量（`.ts` 中无 Qt 快捷键标记）；未完成/过时/空译文跳过并记录。
///
/// # 参数
/// - `cat`：解析后的 `.ts` 目录。
///
/// # 返回
/// [`ConvertOutput`]：`.ftl` 文本、统计与问题清单。
///
/// # 示例
/// ```
/// let xml = r#"<TS language="zh_CN"><context><name>A</name><message>
/// <source>Hi %1</source><translation>你好 %1</translation></message></context></TS>"#;
/// let cat = snow_i18n::ts::parse_ts(xml).unwrap();
/// let out = snow_i18n::convert::convert_catalog(&cat);
/// assert!(out.ftl.contains("你好 { $arg1 }"));
/// ```
pub fn convert_catalog(cat: &TsCatalog) -> ConvertOutput {
    let mut out = ConvertOutput::default();
    let mut seen: BTreeMap<String, (String, String)> = BTreeMap::new();
    for ctx in &cat.contexts {
        let mut section = String::new();
        for m in &ctx.messages {
            out.stats.total += 1;
            if let Some(text) = convert_message(ctx, m, &mut seen, &mut out) {
                section.push_str(&text);
            }
        }
        if !section.is_empty() {
            out.ftl
                .push_str(&format!("## 上下文：{}\n\n{section}", ctx.name));
        }
    }
    out
}

/// 转换单条消息，成功返回其 `.ftl` 文本块。
fn convert_message(
    ctx: &TsContext,
    m: &TsMessage,
    seen: &mut BTreeMap<String, (String, String)>,
    out: &mut ConvertOutput,
) -> Option<String> {
    let mut issue = |kind| {
        out.issues.push(ConvertIssue {
            kind,
            context: ctx.name.clone(),
            source: m.source.clone(),
        });
    };
    match m.status {
        TsStatus::Unfinished => return skip(&mut issue, IssueKind::SkippedUnfinished),
        TsStatus::Obsolete | TsStatus::Vanished => {
            return skip(&mut issue, IssueKind::SkippedObsolete);
        }
        TsStatus::Finished => {}
    }
    if m.translations.is_empty() || m.translations.iter().all(|t| t.trim().is_empty()) {
        return skip(&mut issue, IssueKind::SkippedEmpty);
    }
    let id = message_id(&ctx.name, &m.source);
    match seen.get(&id) {
        Some((c, s)) if *c == ctx.name && *s == m.source => {
            return skip(&mut issue, IssueKind::Duplicate);
        }
        Some(_) => return skip(&mut issue, IssueKind::IdCollision),
        None => {
            seen.insert(id.clone(), (ctx.name.clone(), m.source.clone()));
        }
    }
    let mut src_info = ValueInfo::default();
    tokenize(&m.source, &mut src_info);
    let mut info = ValueInfo::default();
    let body = if m.numerus && m.translations.len() > 1 {
        let Some(cats) = plural_categories(m.translations.len()) else {
            return skip(&mut issue, IssueKind::NeedsManualPlural);
        };
        let mut variants = Vec::new();
        for (cat, t) in cats.iter().zip(&m.translations) {
            let lines = render_lines(&tokenize(t, &mut info));
            if lines.len() > 1 {
                return skip(&mut issue, IssueKind::NeedsManualPlural);
            }
            let mark = if *cat == "other" { "*" } else { " " };
            variants.push(format!("       {mark}[{cat}] {}\n", lines[0]));
        }
        format!(
            "{id} =\n    {{ ${PLURAL_VAR} ->\n{}    }}\n",
            variants.concat()
        )
    } else {
        let lines = render_lines(&tokenize(&m.translations[0], &mut info));
        if lines.len() == 1 {
            format!("{id} = {}\n", lines[0])
        } else {
            let indented: Vec<String> = lines.iter().map(|l| format!("    {l}")).collect();
            format!("{id} =\n{}\n", indented.join("\n"))
        }
    };
    let mismatch = src_info.args != info.args;
    let (converted, plural, product, with_args) = (
        1,
        usize::from(m.numerus),
        info.product,
        usize::from(!info.args.is_empty()),
    );
    let comment_src: String = m
        .source
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect();
    let mut text = format!("# 源文：{comment_src}\n");
    if let Some(c) = m.comment.as_deref().filter(|c| !c.is_empty()) {
        text.push_str(&format!("# 注释：{}\n", c.replace('\n', " ")));
    }
    text.push_str(&body);
    text.push('\n');
    if mismatch {
        out.issues.push(ConvertIssue {
            kind: IssueKind::PlaceholderMismatch,
            context: ctx.name.clone(),
            source: m.source.clone(),
        });
    }
    out.stats.converted += converted;
    out.stats.plural += plural;
    out.stats.product_replaced += product;
    out.stats.with_args += with_args;
    Some(text)
}

/// 记录问题并返回 `None`。
fn skip(issue: &mut impl FnMut(IssueKind), kind: IssueKind) -> Option<String> {
    issue(kind);
    None
}

/// 用源文充当译文，合成源语言目录（用于缺少 en_US 的模块）。
///
/// # 参数
/// - `cat`：任一已有语言的目录（只取源文）。
/// - `lang`：合成目录的语言标记，如 `en_US`。
///
/// # 返回
/// 译文等于源文的目录；复数条目无法从源文推出，会被丢弃。
pub fn identity_catalog(cat: &TsCatalog, lang: &str) -> TsCatalog {
    let contexts = cat
        .contexts
        .iter()
        .map(|c| TsContext {
            name: c.name.clone(),
            messages: c
                .messages
                .iter()
                .filter(|m| !m.numerus && m.status == TsStatus::Finished)
                .map(|m| TsMessage {
                    translations: vec![m.source.clone()],
                    ..m.clone()
                })
                .collect(),
        })
        .collect();
    TsCatalog {
        language: lang.to_string(),
        source_language: Some(lang.to_string()),
        contexts,
    }
}

/// 扫描 `.ftl` 文本中的顶层消息 id（按行匹配，适用于本工具生成的文件）。
///
/// # 参数
/// - `ftl`：`.ftl` 文本。
///
/// # 返回
/// 消息 id 集合。
pub fn ftl_message_ids(ftl: &str) -> BTreeSet<String> {
    ftl.lines()
        .filter(|l| l.starts_with(|c: char| c.is_ascii_alphabetic()))
        .filter_map(|l| l.split_once('=').map(|(k, _)| k.trim().to_string()))
        .filter(|k| {
            k.chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
        })
        .collect()
}

/// 对比各语言的 id 集合，返回每种语言相对并集缺失的 id（CI 门禁语义）。
///
/// # 参数
/// - `sets`：`(语言, id 集合)` 列表。
///
/// # 返回
/// `(语言, 缺失 id 列表)`，仅包含有缺失的语言。
pub fn missing_ids(sets: &[(String, BTreeSet<String>)]) -> Vec<(String, Vec<String>)> {
    let union: BTreeSet<&String> = sets.iter().flat_map(|(_, s)| s.iter()).collect();
    sets.iter()
        .filter_map(|(lang, s)| {
            let miss: Vec<String> = union
                .iter()
                .filter(|id| !s.contains(**id))
                .map(|id| (*id).clone())
                .collect();
            (!miss.is_empty()).then(|| (lang.clone(), miss))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ts::parse_ts;

    /// 包装单条消息为 ts 文本并转换。
    fn conv(src: &str, forms: &[&str], numerus: bool) -> ConvertOutput {
        let tr: String = if numerus {
            let f: String = forms
                .iter()
                .map(|f| format!("<numerusform>{f}</numerusform>"))
                .collect();
            format!("<translation>{f}</translation>")
        } else {
            format!("<translation>{}</translation>", forms[0])
        };
        let n = if numerus { " numerus=\"yes\"" } else { "" };
        let xml = format!(
            "<TS language=\"x\"><context><name>MyCtx</name><message{n}><source>{src}</source>{tr}</message></context></TS>"
        );
        convert_catalog(&parse_ts(&xml).unwrap())
    }

    /// id 应稳定、合法且区分大小写不同的源文。
    #[test]
    fn id_rules() {
        let a = message_id("SettingsWidget", "Save %1 files");
        assert!(a.starts_with("settings-widget-save-1-files-"));
        assert_eq!(a, message_id("SettingsWidget", "Save %1 files"));
        assert_ne!(message_id("A", "x"), message_id("A", "X"));
        assert!(message_id("::9", "中文").starts_with("c-"));
    }

    /// `%1`/`%n`/产品名应映射为变量。
    #[test]
    fn placeholders_and_product() {
        let o = conv("Snow Shot %1 of %2", &["Snow Shot %2/%1"], false);
        assert!(
            o.ftl.contains("{ $product } { $arg2 }/{ $arg1 }"),
            "{}",
            o.ftl
        );
        assert_eq!(o.stats.product_replaced, 1);
    }

    /// 两形式复数应生成 one/other 选择器。
    #[test]
    fn plural_selector() {
        let o = conv("%n a(s)", &["%n a", "%n as"], true);
        assert!(o.ftl.contains("[one] { $n } a"));
        assert!(o.ftl.contains("*[other] { $n } as"));
        assert_eq!(o.stats.plural, 1);
    }

    /// 花括号、首尾空白、行首特殊字符与多行应被转义。
    #[test]
    fn escaping() {
        let o = conv("x", &[" {a} .b "], false);
        assert!(
            o.ftl.contains("{ \" \" }{ \"{\" }a{ \"}\" } .b{ \" \" }"),
            "{}",
            o.ftl
        );
        let o = conv("x", &["l1\n[l2\n\nl4"], false);
        assert!(
            o.ftl
                .contains("    l1\n    { \"[\" }l2\n    { \"\" }\n    l4"),
            "{}",
            o.ftl
        );
    }

    /// 未完成条目应被跳过并记录。
    #[test]
    fn skips_unfinished() {
        let xml = r#"<TS language="x"><context><name>A</name><message><source>a</source><translation type="unfinished">b</translation></message></context></TS>"#;
        let o = convert_catalog(&parse_ts(xml).unwrap());
        assert_eq!(o.stats.converted, 0);
        assert_eq!(o.issues[0].kind, IssueKind::SkippedUnfinished);
    }

    /// 门禁应报出缺失 id。
    #[test]
    fn parity_gate() {
        let a: BTreeSet<String> = ["x", "y"].iter().map(|s| s.to_string()).collect();
        let b: BTreeSet<String> = ["x"].iter().map(|s| s.to_string()).collect();
        let r = missing_ids(&[("en".into(), a), ("zh".into(), b)]);
        assert_eq!(r, vec![("zh".to_string(), vec!["y".to_string()])]);
    }
}
