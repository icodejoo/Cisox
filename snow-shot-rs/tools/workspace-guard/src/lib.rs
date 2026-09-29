//! workspace 守卫：`gpui` 只允许出现在 snow-ui-shell（方案 ADR-1 / 约定 4）。
//!
//! 所属阶段：P1。`cargo test -p workspace-guard` 会扫描真实源码树。

use std::fs;
use std::path::{Path, PathBuf};

/// 允许使用 gpui 的 crate 目录名。
pub const ALLOWED_CRATE_DIR: &str = "snow-ui-shell";

/// 守卫自身目录名（其测试样例含违规字面量，需跳过）。
const GUARD_DIR: &str = "workspace-guard";

/// 需要跳过的目录名。
const SKIP_DIRS: [&str; 3] = ["target", "vendor", ".git"];

/// 一条违规记录。
#[derive(Debug, PartialEq, Eq)]
pub struct Violation {
    /// 违规文件路径。
    pub file: PathBuf,
    /// 违规行号（从 1 开始）。
    pub line: usize,
}

/// 判断单行文本是否违规：`.rs` 中的 `gpui::`（忽略 `//` 注释行），
/// 或 `Cargo.toml` 中以 `gpui` 开头（含 `package = "gpui…"` 改名）的依赖声明。
pub fn line_violates(is_manifest: bool, line: &str) -> bool {
    let trimmed = line.trim_start();
    if is_manifest {
        return !trimmed.starts_with('#')
            && (trimmed.starts_with("gpui") || trimmed.contains("package = \"gpui"));
    }
    !trimmed.starts_with("//") && trimmed.contains("gpui::")
}

/// 扫描 `root` 下除允许目录外的全部 `.rs` 与 `Cargo.toml`，返回违规列表。
pub fn scan(root: &Path) -> Vec<Violation> {
    let mut out = Vec::new();
    walk(root, &mut out);
    out
}

/// 递归遍历目录并收集违规。
fn walk(dir: &Path, out: &mut Vec<Violation>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().into_owned();
        if path.is_dir() {
            if name == ALLOWED_CRATE_DIR || name == GUARD_DIR || SKIP_DIRS.contains(&name.as_str())
            {
                continue;
            }
            walk(&path, out);
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
    // 当前是否处于 `[patch.*]` 段：根 manifest 的 vendor 锁定声明允许出现 gpui
    let mut in_patch = false;
    for (idx, line) in text.lines().enumerate() {
        let trimmed = line.trim_start();
        if is_manifest && trimmed.starts_with('[') {
            in_patch = trimmed.starts_with("[patch");
            continue;
        }
        if in_patch {
            continue;
        }
        if line_violates(is_manifest, line) {
            out.push(Violation {
                file: path.to_path_buf(),
                line: idx + 1,
            });
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
}
