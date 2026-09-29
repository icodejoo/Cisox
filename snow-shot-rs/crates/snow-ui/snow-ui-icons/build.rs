//! 构建脚本：读取 ant_design_qt 的 Ant 图标模板，按 C++ 生成器规则规范化槽位，
//! 写入 `$OUT_DIR` 并生成 `include_str!` 资源表。
//!
//! 资源原地引用，不复制、不修改 ant_design_qt 目录（迁移约定 2/9）。
//! 规范化逻辑与 `ant_design_qt/tools/generate_icon_pack.py` 的 `_normalize_slots` 一一对应。

use std::env;
use std::fmt::Write as _;
use std::fs;
use std::path::{Path, PathBuf};

/// 模板目录相对本 crate 的位置（含 slot 标记的 SVG）。
const TEMPLATES_REL: &str =
    "../../../../ant_design_qt/packages/ant_design_icons_qt/resources/templates";

/// 三种主题子目录名（按字典序，binary_search 依赖此顺序）。
const THEMES: [&str; 3] = ["filled", "outlined", "twotone"];

/// 主色占位符。
const PRIMARY: &str = "__ADQT_SLOT_PRIMARY__";
/// 副色占位符。
const SECONDARY: &str = "__ADQT_SLOT_SECONDARY__";
/// 三色占位符。
const TERTIARY: &str = "__ADQT_SLOT_TERTIARY__";
/// 槽位属性名。
const SLOT_ATTR: &str = "data-adqt-slot";

/// 入口：生成 `$OUT_DIR/icon_table.rs` 与规范化后的 SVG 文件。
fn main() {
    let manifest = PathBuf::from(env::var("CARGO_MANIFEST_DIR").expect("缺少 CARGO_MANIFEST_DIR"));
    let out_dir = PathBuf::from(env::var("OUT_DIR").expect("缺少 OUT_DIR"));
    let root = manifest.join(TEMPLATES_REL);
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed={}", root.display());

    let mut table = String::from(
        "/// 编译期嵌入的图标模板表：(主题目录, 名称, 规范化 SVG)，已按前两项排序。\n",
    );
    table.push_str("pub(crate) static TEMPLATES: &[(&str, &str, &str)] = &[\n");
    for theme in THEMES {
        let dir = root.join(theme);
        println!("cargo:rerun-if-changed={}", dir.display());
        let dest_dir = out_dir.join("svg").join(theme);
        fs::create_dir_all(&dest_dir).expect("创建输出目录失败");
        for (name, path) in list_svgs(&dir) {
            println!("cargo:rerun-if-changed={}", path.display());
            let raw = fs::read_to_string(&path)
                .unwrap_or_else(|e| panic!("读取 {} 失败: {e}", path.display()));
            let source = raw
                .trim_start_matches('\u{feff}')
                .replace("\r\n", "\n")
                .replace('\r', "\n");
            let normalized = normalize_slots(source.trim(), theme == "twotone")
                .unwrap_or_else(|e| panic!("{theme}/{name}: {e}"));
            let dest = dest_dir.join(format!("{name}.svg"));
            fs::write(&dest, normalized).expect("写入规范化 SVG 失败");
            let p = dest.to_string_lossy().replace('\\', "/");
            writeln!(
                table,
                "    (\"{theme}\", \"{name}\", include_str!(r\"{p}\")),"
            )
            .unwrap();
        }
    }
    table.push_str("];\n");
    fs::write(out_dir.join("icon_table.rs"), table).expect("写入 icon_table.rs 失败");
}

/// 列出目录下全部 `.svg`，返回按名称排序的 (名称, 路径)。
fn list_svgs(dir: &Path) -> Vec<(String, PathBuf)> {
    let rd =
        fs::read_dir(dir).unwrap_or_else(|e| panic!("无法读取图标目录 {}: {e}", dir.display()));
    let mut v: Vec<(String, PathBuf)> = rd
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "svg"))
        .map(|p| (p.file_stem().unwrap().to_string_lossy().into_owned(), p))
        .collect();
    v.sort();
    v
}

/// 规范化一份模板：槽位属性换成颜色占位符，末尾追加换行。
///
/// `two_tone` 为真时要求恰有主色+副色槽位，否则只允许主色槽位。
fn normalize_slots(source: &str, two_tone: bool) -> Result<String, String> {
    let mut out = String::with_capacity(source.len() + 64);
    let mut rest = source;
    while let Some(lt) = rest.find('<') {
        out.push_str(&rest[..lt]);
        let after = &rest[lt..];
        let Some(gt) = after.find('>') else {
            out.push_str(after);
            rest = "";
            break;
        };
        out.push_str(&rewrite_tag(&after[..=gt]));
        rest = &after[gt + 1..];
    }
    out.push_str(rest);
    let mut normalized = replace_ci(&out, "currentColor", PRIMARY);

    let has = |p: &str| normalized.contains(p);
    let mut present = (has(PRIMARY), has(SECONDARY), has(TERTIARY));
    if present == (false, false, false) && !two_tone {
        // 单色且没有任何槽位：给根节点补 fill
        if let Some(s) = normalized.to_ascii_lowercase().find("<svg") {
            let end = normalized[s..]
                .find('>')
                .map(|i| s + i)
                .ok_or("svg 标签未闭合")?;
            normalized.insert_str(end, &format!(" fill=\"{PRIMARY}\""));
            present.0 = true;
        }
    }
    let expected = (true, two_tone, false);
    if present != expected {
        return Err(format!("槽位不符：期望 {expected:?}，实际 {present:?}"));
    }
    normalized.push('\n');
    Ok(normalized)
}

/// 处理单个标签文本（含 `<` 与 `>`）；不含槽位属性则原样返回。
fn rewrite_tag(tag: &str) -> String {
    let Some((a_start, a_end, slot)) = find_slot_attr(tag) else {
        return tag.to_owned();
    };
    let placeholder = match slot.as_str() {
        "primary" => PRIMARY,
        "secondary" => SECONDARY,
        _ => TERTIARY,
    };
    // 连同前导空白一起删掉槽位属性
    let ws = tag[..a_start].len() - tag[..a_start].trim_end().len();
    let mut t = format!("{}{}", &tag[..a_start - ws], &tag[a_end..]);
    let mut changed = false;
    for attr in ["fill", "stroke"] {
        if let Some((s, e, vs, ve)) = find_attr(&t, attr)
            && !t[vs..ve].eq_ignore_ascii_case("none")
        {
            t.replace_range(s..e, &format!("{attr}=\"{placeholder}\""));
            changed = true;
        }
    }
    if t.to_ascii_lowercase().contains("currentcolor") {
        t = replace_ci(&t, "currentColor", placeholder);
        changed = true;
    }
    if !changed {
        let mut pos = t.rfind('>').unwrap_or(t.len());
        if pos > 0 && t.as_bytes()[pos - 1] == b'/' {
            pos -= 1;
        }
        t.insert_str(pos, &format!(" fill=\"{placeholder}\""));
    }
    t
}

/// 是否单词字符（用于 `\b` 判断）。
fn is_word(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}

/// 查找 `name = "value"` 属性，返回 (属性起点, 属性终点, 值起点, 值终点)。
fn find_attr(tag: &str, name: &str) -> Option<(usize, usize, usize, usize)> {
    let lower = tag.to_ascii_lowercase();
    let bytes = lower.as_bytes();
    let mut from = 0;
    while let Some(i) = lower[from..].find(name) {
        let s = from + i;
        from = s + name.len();
        if s > 0 && is_word(bytes[s - 1]) {
            continue;
        }
        let mut p = s + name.len();
        while p < bytes.len() && bytes[p].is_ascii_whitespace() {
            p += 1;
        }
        if p >= bytes.len() || bytes[p] != b'=' {
            continue;
        }
        p += 1;
        while p < bytes.len() && bytes[p].is_ascii_whitespace() {
            p += 1;
        }
        if p >= bytes.len() || (bytes[p] != b'"' && bytes[p] != b'\'') {
            continue;
        }
        let quote = bytes[p];
        let vs = p + 1;
        // Python 原式值内不允许出现任一种引号
        if let Some(len) = bytes[vs..].iter().position(|&b| b == b'"' || b == b'\'')
            && bytes[vs + len] == quote
        {
            return Some((s, vs + len + 1, vs, vs + len));
        }
    }
    None
}

/// 查找合法的槽位属性，返回 (起点, 终点, 小写槽位名)。
fn find_slot_attr(tag: &str) -> Option<(usize, usize, String)> {
    let (s, e, vs, ve) = find_attr(tag, SLOT_ATTR)?;
    let v = tag[vs..ve].to_ascii_lowercase();
    matches!(v.as_str(), "primary" | "secondary" | "tertiary").then_some((s, e, v))
}

/// 忽略大小写替换（ASCII）。
fn replace_ci(text: &str, from: &str, to: &str) -> String {
    let lower = text.to_ascii_lowercase();
    let needle = from.to_ascii_lowercase();
    let mut out = String::with_capacity(text.len());
    let mut last = 0;
    for (i, _) in lower.match_indices(&needle) {
        out.push_str(&text[last..i]);
        out.push_str(to);
        last = i + from.len();
    }
    out.push_str(&text[last..]);
    out
}
