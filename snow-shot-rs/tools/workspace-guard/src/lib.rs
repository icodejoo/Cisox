//! workspace 守卫：`gpui` 只允许出现在 snow-ui-shell（方案 ADR-1 / 约定 4）。
//!
//! 所属阶段：P1。`cargo test -p workspace-guard` 会扫描真实源码树。

use std::fs;
use std::path::{Path, PathBuf};

/// 允许使用 gpui 的 crate 目录（相对扫描根）。
pub const ALLOWED_CRATE_DIR: &str = "crates/snow-ui/snow-ui-shell";

/// 守卫自身目录（相对扫描根；其测试样例含违规字面量，需跳过）。
const GUARD_DIR: &str = "tools/workspace-guard";

/// 需要跳过的目录（相对扫描根，仅精确匹配根下的位置，不按任意层级同名跳过）。
const SKIP_DIRS: [&str; 3] = ["target", "vendor", ".git"];

/// 被守卫的库名。
const GUARDED_CRATE: &str = "gpui";

/// TOML 中 `package` 键名。
const PACKAGE_KEY: &str = "package";

/// 一条违规记录。
#[derive(Debug, PartialEq, Eq)]
pub struct Violation {
    /// 违规文件路径。
    pub file: PathBuf,
    /// 违规行号（从 1 开始）。
    pub line: usize,
}

/// 是否为标识符字符。
fn is_ident_char(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

/// 若 `chars[i]` 起始处是字符字面量（`'x'` 或 `'\x'`），返回其字符数；否则（如生命周期）返回 `None`。
fn char_literal_len(chars: &[char], i: usize) -> Option<usize> {
    match (chars.get(i + 1), chars.get(i + 2), chars.get(i + 3)) {
        (Some('\\'), Some(_), Some('\'')) => Some(4),
        (Some(c), Some('\''), _) if *c != '\\' => Some(3),
        _ => None,
    }
}

/// 去掉 Rust 源码中的行注释与（可嵌套的）块注释，保留换行以维持行号。
///
/// 字符串字面量内的 `//`、`/*` 不视为注释起点。
///
/// # 参数
/// - `text`：源码文本
///
/// # 返回
/// 注释被替换为空格（换行保留）后的文本。
///
/// # 示例
/// ```
/// use workspace_guard::strip_rust_comments;
/// assert_eq!(strip_rust_comments("a /* x */ b"), "a         b");
/// ```
pub fn strip_rust_comments(text: &str) -> String {
    let chars: Vec<char> = text.chars().collect();
    let mut out = String::with_capacity(text.len());
    let (mut i, mut depth) = (0, 0usize);
    let mut in_string = false;
    while i < chars.len() {
        let c = chars[i];
        let next = chars.get(i + 1).copied();
        if depth > 0 {
            if c == '/' && next == Some('*') {
                depth += 1;
                out.push_str("  ");
                i += 2;
            } else if c == '*' && next == Some('/') {
                depth -= 1;
                out.push_str("  ");
                i += 2;
            } else {
                out.push(if c == '\n' { '\n' } else { ' ' });
                i += 1;
            }
        } else if in_string {
            out.push(c);
            if c == '\\' && next.is_some() {
                out.push(next.unwrap_or(' '));
                i += 1;
            } else if c == '"' {
                in_string = false;
            }
            i += 1;
        } else if c == '\'' && char_literal_len(&chars, i).is_some() {
            // 字符字面量（如 `'"'`）原样输出，避免误判为字符串起点
            let len = char_literal_len(&chars, i).unwrap_or(1);
            out.extend(&chars[i..i + len]);
            i += len;
        } else if c == '"' {
            in_string = true;
            out.push(c);
            i += 1;
        } else if c == '/' && next == Some('/') {
            while i < chars.len() && chars[i] != '\n' {
                out.push(' ');
                i += 1;
            }
        } else if c == '/' && next == Some('*') {
            depth = 1;
            out.push_str("  ");
            i += 2;
        } else {
            out.push(c);
            i += 1;
        }
    }
    out
}

/// 判断（已去注释的）Rust 代码行是否引用 gpui：
/// `gpui::…`、`use gpui…`（含 `as` 改名、`use {gpui, …}`）、`extern crate gpui`。
fn rust_line_violates(line: &str) -> bool {
    let mut search_from = 0;
    while let Some(found) = line[search_from..].find(GUARDED_CRATE) {
        let start = search_from + found;
        let end = start + GUARDED_CRATE.len();
        search_from = end;
        let before = &line[..start];
        let after = &line[end..];
        // 只匹配完整标识符 `gpui`（排除 gpui_x、xgpui）
        if before.chars().next_back().is_some_and(is_ident_char) || after.starts_with(is_ident_char)
        {
            continue;
        }
        let head = before.trim_end();
        // `foo::gpui` 是别的模块；行首的 `::gpui` 是全局路径
        if let Some(prefix) = head.strip_suffix("::")
            && prefix.chars().next_back().is_some_and(is_ident_char)
        {
            continue;
        }
        let is_use = head
            .strip_suffix("use")
            .is_some_and(|rest| !rest.ends_with(is_ident_char))
            || head.ends_with("extern crate")
            || (head.ends_with('{') && head.contains("use"));
        if after.trim_start().starts_with("::") || is_use {
            return true;
        }
    }
    false
}

/// 去掉 TOML 行尾注释（忽略引号内的 `#`）。
fn strip_toml_comment(line: &str) -> &str {
    let mut quote: Option<char> = None;
    for (idx, c) in line.char_indices() {
        match (quote, c) {
            (None, '"' | '\'') => quote = Some(c),
            (Some(q), _) if c == q => quote = None,
            (None, '#') => return &line[..idx],
            _ => {}
        }
    }
    line
}

/// 判断 manifest 表头是否声明 gpui 依赖，如 `[dependencies.gpui]`、
/// `[workspace.dependencies.gpui-x]`、`[target.'cfg(..)'.dependencies.gpui]`。
fn table_header_declares_gpui(header: &str) -> bool {
    let inner = header.trim().trim_start_matches('[').trim_end_matches(']');
    match inner.rsplit_once('.') {
        Some((table, name)) => {
            table.trim_end().ends_with("dependencies")
                && name
                    .trim()
                    .trim_matches(['"', '\''])
                    .starts_with(GUARDED_CRATE)
        }
        None => false,
    }
}

/// 判断 manifest 行是否声明 gpui：键以 `gpui` 开头，或 `package = "gpui…"`
/// （任意空白，单/双引号）。
fn manifest_line_violates(line: &str) -> bool {
    let code = strip_toml_comment(line);
    if code
        .trim_start()
        .trim_start_matches(['"', '\''])
        .starts_with(GUARDED_CRATE)
    {
        return true;
    }
    let mut rest = code;
    while let Some(idx) = rest.find(PACKAGE_KEY) {
        let before = &rest[..idx];
        rest = &rest[idx + PACKAGE_KEY.len()..];
        if before.chars().next_back().is_some_and(is_ident_char) {
            continue;
        }
        if let Some(value) = rest.trim_start().strip_prefix('=')
            && value
                .trim_start()
                .trim_start_matches(['"', '\''])
                .starts_with(GUARDED_CRATE)
        {
            return true;
        }
    }
    false
}

/// 判断单行文本是否违规。
///
/// - `.rs`：代码中的 `gpui::`、`use gpui…`、`use gpui as x`、`extern crate gpui`
///   （行内注释放行；跨行块注释需先用 [`strip_rust_comments`] 处理整段文本）。
/// - `Cargo.toml`：键以 `gpui` 开头，或 `package = "gpui…"`（含无空格、单引号写法）。
pub fn line_violates(is_manifest: bool, line: &str) -> bool {
    if is_manifest {
        manifest_line_violates(line)
    } else {
        rust_line_violates(&strip_rust_comments(line))
    }
}

/// 扫描 `root` 下除允许目录外的全部 `.rs` 与 `Cargo.toml`，返回违规列表。
pub fn scan(root: &Path) -> Vec<Violation> {
    let mut out = Vec::new();
    walk(root, root, &mut out);
    out
}

/// 目录是否应跳过：仅当它恰好位于根下的约定位置。
fn skip_dir(root: &Path, dir: &Path) -> bool {
    let Ok(relative) = dir.strip_prefix(root) else {
        return false;
    };
    relative == Path::new(ALLOWED_CRATE_DIR)
        || relative == Path::new(GUARD_DIR)
        || SKIP_DIRS.iter().any(|name| relative == Path::new(name))
}

/// 递归遍历目录并收集违规。
fn walk(root: &Path, dir: &Path, out: &mut Vec<Violation>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().into_owned();
        if path.is_dir() {
            if !skip_dir(root, &path) {
                walk(root, &path, out);
            }
        } else if name.ends_with(".rs") || name == "Cargo.toml" {
            check_file(&path, name == "Cargo.toml", out);
        }
    }
}

/// 检查单个文件。
fn check_file(path: &Path, is_manifest: bool, out: &mut Vec<Violation>) {
    let Ok(text) = fs::read_to_string(path) else {
        return;
    };
    let mut push = |idx: usize| {
        out.push(Violation {
            file: path.to_path_buf(),
            line: idx + 1,
        })
    };
    if !is_manifest {
        for (idx, line) in strip_rust_comments(&text).lines().enumerate() {
            if rust_line_violates(line) {
                push(idx);
            }
        }
        return;
    }
    // 当前是否处于 `[patch.*]` 段：根 manifest 的 vendor 锁定声明允许出现 gpui
    let mut in_patch = false;
    for (idx, line) in text.lines().enumerate() {
        let trimmed = strip_toml_comment(line).trim_start();
        if trimmed.starts_with('[') {
            in_patch = trimmed.starts_with("[patch");
            if !in_patch && table_header_declares_gpui(trimmed) {
                push(idx);
            }
            continue;
        }
        if !in_patch && manifest_line_violates(line) {
            push(idx);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 构造临时目录树的辅助函数。
    fn temp_tree(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("snow-guard-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// 行级判定：代码违规、注释放行、manifest 依赖违规。
    #[test]
    fn line_rules() {
        assert!(line_violates(false, "    use gpui::App;"));
        assert!(!line_violates(false, "    // 不要用 gpui::App"));
        assert!(!line_violates(false, "let x = 1;"));
        assert!(line_violates(true, "gpui = \"0.2\""));
        assert!(!line_violates(true, "serde = \"1\""));
    }

    /// 外部 crate 出现 gpui:: 应被发现，shell 内应放行。
    #[test]
    fn detects_outside_shell_only() {
        let root = temp_tree("detect");
        let bad = root.join("crates/snow-foo/src");
        let ok = root.join("crates/snow-ui/snow-ui-shell/src");
        fs::create_dir_all(&bad).unwrap();
        fs::create_dir_all(&ok).unwrap();
        fs::write(bad.join("lib.rs"), "fn a() {}\nuse gpui::App;\n").unwrap();
        fs::write(ok.join("lib.rs"), "use gpui::App;\n").unwrap();
        let found = scan(&root);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].line, 2);
        let _ = fs::remove_dir_all(&root);
    }

    /// 非 shell 的 Cargo.toml 声明 gpui 依赖（含改名、workspace 依赖表）应被发现，
    /// `[patch.crates-io]` 段与 shell 自身放行。
    #[test]
    fn manifest_outside_shell_fails() {
        let root = temp_tree("manifest");
        let bad = root.join("crates/snow-foo");
        let bad2 = root.join("crates/snow-bar");
        let ok = root.join("crates/snow-ui/snow-ui-shell");
        for d in [&bad, &bad2, &ok] {
            fs::create_dir_all(d).unwrap();
        }
        fs::write(
            bad.join("Cargo.toml"),
            "[dependencies]
gpui-kit = \"0.7\"
",
        )
        .unwrap();
        fs::write(
            bad2.join("Cargo.toml"),
            "[dependencies]
ui = { package = \"gpui-pre\", version = \"0.3\" }
",
        )
        .unwrap();
        fs::write(
            ok.join("Cargo.toml"),
            "[dependencies]
gpui-kit = \"0.7\"
",
        )
        .unwrap();
        fs::write(
            root.join("Cargo.toml"),
            "[workspace]
[patch.crates-io]
gpui-pre = { path = \"vendor/x\" }
",
        )
        .unwrap();
        let found = scan(&root);
        assert_eq!(found.len(), 2, "{found:?}");
        assert!(found.iter().all(|v| v.file.ends_with("Cargo.toml")));
        let _ = fs::remove_dir_all(&root);
    }

    /// 真实源码树必须零违规。
    #[test]
    fn real_workspace_is_clean() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let found = scan(&root);
        assert!(found.is_empty(), "发现越界 gpui 引用: {found:?}");
    }

    /// 漏报形式：改名/别名/extern crate/表写法/单引号与无空格的 package。
    #[test]
    fn previously_missed_forms_are_detected() {
        for line in [
            "use gpui as g;",
            "use gpui;",
            "pub use gpui::prelude::*;",
            "extern crate gpui;",
            "use {gpui, other};",
            "let x = ::gpui::App::new();",
        ] {
            assert!(line_violates(false, line), "{line}");
        }
        for line in [
            "ui = { package=\"gpui-pre\", version = \"1\" }",
            "ui = { package = 'gpui-pre' }",
            "gpui.workspace = true",
        ] {
            assert!(line_violates(true, line), "{line}");
        }
        for line in [
            "use gpui_component::Button;",
            "let gpuis = 1;",
            "use foo::gpui::X;",
        ] {
            assert!(!line_violates(false, line), "{line}");
        }
        assert!(!line_violates(true, "# package = \"gpui\""));
    }

    /// `[dependencies.gpui]` 表写法在表头处被发现；`[patch]` 段放行。
    #[test]
    fn table_style_dependency_is_detected() {
        let root = temp_tree("table");
        let bad = root.join("crates/snow-foo");
        fs::create_dir_all(&bad).unwrap();
        fs::write(
            bad.join("Cargo.toml"),
            "[package]\nname = \"x\"\n\n[dependencies.gpui]\nversion = \"0.2\"\n",
        )
        .unwrap();
        fs::write(
            root.join("Cargo.toml"),
            "[workspace.dependencies.serde]\nversion = \"1\"\n[patch.crates-io]\ngpui-pre = { path = \"v\" }\n",
        )
        .unwrap();
        let found = scan(&root);
        assert_eq!(found.len(), 1, "{found:?}");
        assert_eq!(found[0].line, 4);
        let _ = fs::remove_dir_all(&root);
    }

    /// 块注释里的 `gpui::` 不误报，块注释外仍报。
    #[test]
    fn block_comments_are_ignored() {
        let root = temp_tree("block");
        let dir = root.join("crates/snow-foo/src");
        fs::create_dir_all(&dir).unwrap();
        fs::write(
            dir.join("lib.rs"),
            "/* 说明\n use gpui::App;\n /* nested gpui::X */\n*/\nlet q = '\"';\nfn a() {}\nuse gpui::Real;\nlet s = \"/* gpui::str */\";\n",
        )
        .unwrap();
        let found = scan(&root);
        assert_eq!(found.iter().map(|v| v.line).collect::<Vec<_>>(), vec![7, 8]);
        let _ = fs::remove_dir_all(&root);
    }

    /// 任意层级同名的 snow-ui-shell / vendor 目录不再被整体放行，只放行约定位置。
    #[test]
    fn same_named_dirs_elsewhere_are_scanned() {
        let root = temp_tree("names");
        for dir in [
            "crates/snow-foo/snow-ui-shell/src",
            "crates/snow-foo/vendor/src",
            "vendor/pkg/src",
            "crates/snow-ui/snow-ui-shell/src",
        ] {
            fs::create_dir_all(root.join(dir)).unwrap();
            fs::write(root.join(dir).join("lib.rs"), "use gpui::App;\n").unwrap();
        }
        let found = scan(&root);
        assert_eq!(found.len(), 2, "{found:?}");
        assert!(
            found
                .iter()
                .all(|v| v.file.to_string_lossy().contains("snow-foo"))
        );
        let _ = fs::remove_dir_all(&root);
    }
}
