//! 源码扫描：提取 `t!` / `tr` / `tr_with` / `tr_checked` 的字面量 id，并与 `.ftl` 对齐。
//!
//! 采用标准库词法扫描（不做完整解析）。已知限制见 [`extract_refs`]。

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

/// 会被识别为 id 引用的调用名（函数/方法形态，首个参数为 id 字面量）。
const FN_NAMES: [&str; 3] = ["tr", "tr_with", "tr_checked"];

/// 一处 id 引用。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IdRef {
    /// 消息 id。
    pub id: String,
    /// 所在行（从 1 起）。
    pub line: usize,
}

/// 词法单元。
#[derive(Debug, Clone, PartialEq)]
enum Tok {
    /// 标识符。
    Ident(String),
    /// 字符串字面量（含转义时内容为 `None`）。
    Str(Option<String>),
    /// 单个标点。
    Punct(char),
    /// 其他（数字、字符字面量、字节串等）。
    Other,
}

/// 词法扫描：跳过注释与空白，字符串/字符字面量整体成单元。返回（单元, 行号）。
fn lex(src: &str) -> Vec<(Tok, usize)> {
    let c: Vec<char> = src.chars().collect();
    let mut out = Vec::new();
    let (mut i, mut line) = (0, 1);
    while i < c.len() {
        let ch = c[i];
        if ch == '\n' {
            line += 1;
            i += 1;
        } else if ch.is_whitespace() {
            i += 1;
        } else if ch == '/' && c.get(i + 1) == Some(&'/') {
            while i < c.len() && c[i] != '\n' {
                i += 1;
            }
        } else if ch == '/' && c.get(i + 1) == Some(&'*') {
            let mut depth = 1;
            i += 2;
            while i < c.len() && depth > 0 {
                if c[i] == '/' && c.get(i + 1) == Some(&'*') {
                    depth += 1;
                    i += 2;
                } else if c[i] == '*' && c.get(i + 1) == Some(&'/') {
                    depth -= 1;
                    i += 2;
                } else {
                    if c[i] == '\n' {
                        line += 1;
                    }
                    i += 1;
                }
            }
        } else if ch == '"' {
            let (tok, next, lines) = lex_string(&c, i + 1);
            out.push((tok, line));
            i = next;
            line += lines;
        } else if let Some((hashes, body)) = raw_string_start(&c, i) {
            let (tok, next, lines) = lex_raw(&c, body, hashes);
            out.push((tok, line));
            i = next;
            line += lines;
        } else if ch == 'b' && c.get(i + 1) == Some(&'"') {
            let (_, next, lines) = lex_string(&c, i + 2);
            out.push((Tok::Other, line));
            i = next;
            line += lines;
        } else if ch == '\'' {
            i = skip_quote(&c, i);
            out.push((Tok::Other, line));
        } else if ch.is_alphabetic() || ch == '_' {
            let s = i;
            while i < c.len() && (c[i].is_alphanumeric() || c[i] == '_') {
                i += 1;
            }
            out.push((Tok::Ident(c[s..i].iter().collect()), line));
        } else if ch.is_ascii_digit() {
            while i < c.len()
                && (c[i].is_alphanumeric()
                    || c[i] == '_'
                    || (c[i] == '.' && c.get(i + 1).is_some_and(|n| n.is_ascii_digit())))
            {
                i += 1;
            }
            out.push((Tok::Other, line));
        } else {
            out.push((Tok::Punct(ch), line));
            i += 1;
        }
    }
    out
}

/// 若 `i` 处是 `r"` / `r#"`（或 `br`）起始，返回（`#` 个数, 正文起点）。
fn raw_string_start(c: &[char], i: usize) -> Option<(usize, usize)> {
    let mut j = i;
    if c[j] == 'b' {
        j += 1;
    }
    if c.get(j) != Some(&'r') {
        return None;
    }
    j += 1;
    let mut hashes = 0;
    while c.get(j) == Some(&'#') {
        hashes += 1;
        j += 1;
    }
    (c.get(j) == Some(&'"')).then_some((hashes, j + 1))
}

/// 读取普通字符串正文，返回（单元, 结束后位置, 跨越的换行数）。
fn lex_string(c: &[char], mut i: usize) -> (Tok, usize, usize) {
    let mut s = String::new();
    let (mut escaped, mut lines) = (false, 0);
    while i < c.len() {
        match c[i] {
            '\\' => {
                escaped = true;
                if c.get(i + 1) == Some(&'\n') {
                    lines += 1;
                }
                i += 2;
                continue;
            }
            '"' => {
                i += 1;
                break;
            }
            '\n' => {
                lines += 1;
                s.push('\n');
            }
            ch => s.push(ch),
        }
        i += 1;
    }
    (Tok::Str((!escaped).then_some(s)), i, lines)
}

/// 读取原始字符串正文，返回（单元, 结束后位置, 跨越的换行数）。
fn lex_raw(c: &[char], mut i: usize, hashes: usize) -> (Tok, usize, usize) {
    let mut s = String::new();
    let mut lines = 0;
    while i < c.len() {
        if c[i] == '"' && (0..hashes).all(|k| c.get(i + 1 + k) == Some(&'#')) {
            i += 1 + hashes;
            break;
        }
        if c[i] == '\n' {
            lines += 1;
        }
        s.push(c[i]);
        i += 1;
    }
    (Tok::Str(Some(s)), i, lines)
}

/// 跳过 `'` 开头的字符字面量或生命周期，返回结束后位置。
fn skip_quote(c: &[char], i: usize) -> usize {
    if c.get(i + 1) == Some(&'\\') {
        let mut j = i + 2;
        while j < c.len() && c[j] != '\'' {
            j += 1;
        }
        return j + 1;
    }
    if c.get(i + 2) == Some(&'\'') {
        return i + 3;
    }
    i + 1
}

/// 判断是否为合法 id（Fluent 标识符子集）。
fn valid_id(s: &str) -> bool {
    s.starts_with(|c: char| c.is_ascii_alphabetic())
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

/// 判断 `toks[i]` 是否为给定标点。
fn is_punct(toks: &[(Tok, usize)], i: usize, p: char) -> bool {
    matches!(toks.get(i), Some((Tok::Punct(x), _)) if *x == p)
}

/// 从 `t!(` 后开始跳过首个参数，返回顶层逗号之后的位置；无逗号返回 `None`。
fn skip_first_arg(toks: &[(Tok, usize)], mut p: usize) -> Option<usize> {
    let mut depth = 0i32;
    while let Some((t, _)) = toks.get(p) {
        match t {
            Tok::Punct('(' | '[' | '{') => depth += 1,
            Tok::Punct(')' | ']' | '}') if depth == 0 => return None,
            Tok::Punct(')' | ']' | '}') => depth -= 1,
            Tok::Punct(',') if depth == 0 => return Some(p + 1),
            _ => {}
        }
        p += 1;
    }
    None
}

/// 跳过函数名后可选的 `::<...>`，返回其后的位置；尖括号未配平返回 `None`。
fn skip_turbofish(toks: &[(Tok, usize)], p: usize) -> Option<usize> {
    if !(is_punct(toks, p, ':') && is_punct(toks, p + 1, ':') && is_punct(toks, p + 2, '<')) {
        return Some(p);
    }
    let mut depth = 0i32;
    let mut i = p + 2;
    while let Some((t, _)) = toks.get(i) {
        match t {
            Tok::Punct('<') => depth += 1,
            Tok::Punct('>') => {
                depth -= 1;
                if depth == 0 {
                    return Some(i + 1);
                }
            }
            _ => {}
        }
        i += 1;
    }
    None
}

/// 从源码提取 id 引用。
///
/// 识别 `t!(expr, "id" ...)`（含 `t![..]`、`t!{..}`）与 `.tr("id")` / `tr_with("id", ..)` / `tr_checked("id", ..)`；
/// 忽略注释与字符串内的假匹配。
///
/// # 参数
/// - `src`：Rust 源码文本。
///
/// # 返回
/// 按出现顺序的引用列表。
///
/// # 已知限制
/// - 只认字符串字面量：变量、`concat!`、`format!` 拼出的 id 不会被识别。
/// - 含转义的字面量、不符合 Fluent 标识符的字面量被忽略。
/// - 文档注释与 doctest 视为注释，不扫描。
/// - `t!` 的首参数按括号配平后到顶层逗号截止；首参数内的泛型尖括号逗号（如 `f::<A, B>()`）会误截断。
/// - 不识别经 `use ... as` 改名的调用，也不区分同名的非本 crate `tr` 方法。
/// - `#[cfg(test)]` 里的调用与普通代码同等对待，需用 `--exclude` 排除。
///
/// # 示例
/// ```
/// use snow_i18n::extract::extract_refs;
/// let refs = extract_refs(r#"let s = t!(i18n, "core-ok", n = 1);"#);
/// assert_eq!(refs[0].id, "core-ok");
/// ```
pub fn extract_refs(src: &str) -> Vec<IdRef> {
    let toks = lex(src);
    let mut refs = Vec::new();
    for (k, (tok, _)) in toks.iter().enumerate() {
        let Tok::Ident(name) = tok else { continue };
        let is_macro_open = |i: usize| {
            is_punct(&toks, i, '(') || is_punct(&toks, i, '[') || is_punct(&toks, i, '{')
        };
        let start = if name == "t" && is_punct(&toks, k + 1, '!') && is_macro_open(k + 2) {
            match skip_first_arg(&toks, k + 3) {
                Some(p) => p,
                None => continue,
            }
        } else if FN_NAMES.contains(&name.as_str()) {
            match skip_turbofish(&toks, k + 1) {
                Some(p) if is_punct(&toks, p, '(') => p + 1,
                _ => continue,
            }
        } else {
            continue;
        };
        if let Some((Tok::Str(Some(id)), line)) = toks.get(start)
            && [',', ')', ']', '}']
                .iter()
                .any(|&p| is_punct(&toks, start + 1, p))
            && valid_id(id)
        {
            refs.push(IdRef {
                id: id.clone(),
                line: *line,
            });
        }
    }
    refs
}

/// 递归收集 `.rs` 文件（跳过 `target`、隐藏目录与路径含 `excludes` 子串者）。
///
/// # 参数
/// - `path`：文件或目录。
/// - `excludes`：路径子串（统一以 `/` 比较）。
/// - `out`：结果累加。
///
/// # 返回
/// 读取目录失败时返回 IO 错误。
pub fn collect_rs(path: &Path, excludes: &[String], out: &mut Vec<PathBuf>) -> std::io::Result<()> {
    let norm = path.to_string_lossy().replace('\\', "/");
    if excludes.iter().any(|e| norm.contains(e.as_str())) {
        return Ok(());
    }
    if path.is_dir() {
        let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
        if name == "target" || (name.starts_with('.') && name.len() > 1) {
            return Ok(());
        }
        let mut entries: Vec<_> = std::fs::read_dir(path)?.collect::<Result<_, _>>()?;
        entries.sort_by_key(|e| e.path());
        for e in entries {
            collect_rs(&e.path(), excludes, out)?;
        }
    } else if path.extension().is_some_and(|e| e == "rs") {
        out.push(path.to_path_buf());
    }
    Ok(())
}

/// 对齐报告。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RefReport {
    /// 代码引用但基准语言缺失：`(id, 文件, 行)`。
    pub missing: Vec<(String, String, usize)>,
    /// 基准语言存在但代码未引用的 id。
    pub orphans: BTreeSet<String>,
    /// 引用总数（含重复）。
    pub total_refs: usize,
}

/// 对比引用与基准语言 id。
///
/// # 参数
/// - `refs`：`文件 -> 引用列表`。
/// - `base_ids`：基准语言（en-US）全部 id。
///
/// # 返回
/// [`RefReport`]。
pub fn compare_refs(refs: &BTreeMap<String, Vec<IdRef>>, base_ids: &BTreeSet<String>) -> RefReport {
    let mut report = RefReport::default();
    let mut used = BTreeSet::new();
    for (file, list) in refs {
        for r in list {
            report.total_refs += 1;
            used.insert(r.id.as_str());
            if !base_ids.contains(&r.id) {
                report.missing.push((r.id.clone(), file.clone(), r.line));
            }
        }
    }
    report.orphans = base_ids
        .iter()
        .filter(|id| !used.contains(id.as_str()))
        .cloned()
        .collect();
    report
}

/// 读取基准语言目录下全部 `.ftl` 的 id。
///
/// # 参数
/// - `locales`：locales 根目录。
/// - `base_lang`：基准语言目录名，如 `en-US`。
///
/// # 返回
/// id 集合；目录不可读时返回 IO 错误。
pub fn base_ids(locales: &Path, base_lang: &str) -> std::io::Result<BTreeSet<String>> {
    let mut ids = BTreeSet::new();
    for e in std::fs::read_dir(locales.join(base_lang))? {
        let p = e?.path();
        if p.extension().is_some_and(|x| x == "ftl") {
            ids.extend(crate::convert::ftl_message_ids(&std::fs::read_to_string(
                &p,
            )?));
        }
    }
    Ok(ids)
}

/// 扫描源码目录，返回 `文件 -> 引用`（无引用的文件不出现）。
///
/// # 参数
/// - `dirs`：源码目录或文件。
/// - `excludes`：排除的路径子串。
///
/// # 返回
/// 扫描结果；IO 失败时返回错误。
pub fn scan(
    dirs: &[PathBuf],
    excludes: &[String],
) -> std::io::Result<BTreeMap<String, Vec<IdRef>>> {
    let mut files = Vec::new();
    for d in dirs {
        collect_rs(d, excludes, &mut files)?;
    }
    let mut map = BTreeMap::new();
    for f in files {
        let refs = extract_refs(&std::fs::read_to_string(&f)?);
        if !refs.is_empty() {
            map.insert(f.to_string_lossy().replace('\\', "/"), refs);
        }
    }
    Ok(map)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 取 id 列表。
    fn ids(src: &str) -> Vec<String> {
        extract_refs(src).into_iter().map(|r| r.id).collect()
    }

    /// 基本写法：宏、方法、函数、带参、多行。
    #[test]
    fn recognizes_forms() {
        let src = "
let a = t!(i18n, \"a-one\");
let b = t!(self.i18n(), \"b-two\", n = 3, arg1 = \"x\");
let c = i.tr(\"c-three\");
let d = i.tr_with(\"d-four\", &Args::new());
let e = i.tr_checked(
    \"e-five\",
    &args,
);
let f = t!(
    i18n,
    \"f-six\",
    arg1 = foo(1, 2),
);
let g = t!(i18n, r#\"g-seven\"#);
";
        let refs = extract_refs(src);
        let got: Vec<&str> = refs.iter().map(|r| r.id.as_str()).collect();
        assert_eq!(
            got,
            [
                "a-one", "b-two", "c-three", "d-four", "e-five", "f-six", "g-seven"
            ]
        );
        assert_eq!(refs[4].line, 7);
        assert_eq!(refs[5].line, 12);
    }

    /// 方括号/花括号宏、turbofish 与 `self.0.tr` 写法也要识别。
    #[test]
    fn recognizes_extra_forms() {
        let src = "
let a = t![i18n, \"a-one\"];
let b = t!{i18n, \"b-two\"};
let c = self.0.tr(\"c-three\");
let d = i.tr::<Vec<u8>>(\"d-four\");
";
        assert_eq!(ids(src), ["a-one", "b-two", "c-three", "d-four"]);
    }

    /// 注释与字符串里的假匹配不算。
    #[test]
    fn ignores_fake_matches() {
        let src = "
// t!(i18n, \"in-line-comment\")
/* i.tr(\"in-block\") /* nested t!(i, \"x\") */ */
/// doc: i.tr(\"in-doc\")
let s = \"t!(i18n, \\\"in-string\\\")\";
let r = r#\"i.tr(\"in-raw\")\"#;
let ch = '\"'; let lt: &'static str = \"ok\";
let real = i.tr(\"real-one\");
";
        assert_eq!(ids(src), ["real-one"]);
    }

    /// 非字面量、拼接、定义处与非法 id 不算。
    #[test]
    fn ignores_non_literals() {
        let src = "
fn tr(&self, id: &str) -> String { self.tr_with(id, &Args::new()) }
let a = i.tr(name);
let b = i.tr(\"a-b\".to_string());
let c = i.tr(concat!(\"x\", \"y\"));
let d = i.tr(\"has space\");
let e = t!(i18n, id);
let f = i.tr(\"esc\\n\");
let g = i.tr(\"ok-id\") + \"tail\";
";
        assert_eq!(ids(src), ["ok-id"]);
    }

    /// 对齐报告：缺失与孤儿。
    #[test]
    fn compare_reports_missing_and_orphans() {
        let mut refs = BTreeMap::new();
        refs.insert(
            "a.rs".to_string(),
            extract_refs("i.tr(\"have\");\ni.tr(\"lack\");"),
        );
        let base: BTreeSet<String> = ["have", "unused"].map(String::from).into();
        let r = compare_refs(&refs, &base);
        assert_eq!(r.missing, [("lack".to_string(), "a.rs".to_string(), 2)]);
        assert_eq!(r.orphans, BTreeSet::from(["unused".to_string()]));
        assert_eq!(r.total_refs, 2);
    }
}
