//! 提取与门禁的端到端测试：用临时目录调用 `snow-i18n-tool`。

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

/// 工具可执行文件路径。
const TOOL: &str = env!("CARGO_BIN_EXE_snow-i18n-tool");

/// 建立唯一临时目录。
fn temp_dir(tag: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("snow-i18n-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

/// 写文件（自动建父目录）。
fn write(p: &Path, text: &str) {
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(p, text).unwrap();
}

/// 构造 locales 与源码，返回根目录。
fn fixture(tag: &str, code: &str) -> PathBuf {
    let root = temp_dir(tag);
    write(&root.join("loc/en-US/m.ftl"), "have = A\nunused = B\n");
    write(&root.join("loc/zh-CN/m.ftl"), "have = 甲\nunused = 乙\n");
    write(&root.join("src/a.rs"), code);
    root
}

/// 运行工具。
fn run(args: &[&str]) -> Output {
    Command::new(TOOL).args(args).output().unwrap()
}

/// 取 stdout 文本。
fn out(o: &Output) -> String {
    String::from_utf8_lossy(&o.stdout).into_owned()
}

/// 引用齐全时严格模式通过，孤儿只报数量。
#[test]
fn extract_passes_and_reports_orphans() {
    let root = fixture("ok", "fn f(i: &I) { let _ = t!(i, \"have\"); }");
    let (loc, src) = (root.join("loc"), root.join("src"));
    let o = run(&[
        "extract",
        src.to_str().unwrap(),
        "--locales",
        loc.to_str().unwrap(),
        "--strict-refs",
    ]);
    assert!(o.status.success(), "{}", out(&o));
    assert!(out(&o).contains("孤儿）1 个"), "{}", out(&o));
}

/// 故意缺失：严格模式失败并指出位置，非严格模式仍通过。
#[test]
fn extract_fails_on_missing_when_strict() {
    let root = fixture("miss", "fn f(i: &I) {\n let _ = i.tr(\"nope\");\n}");
    let (loc, src) = (root.join("loc"), root.join("src"));
    let (l, s) = (loc.to_str().unwrap(), src.to_str().unwrap());
    let strict = run(&["extract", s, "--locales", l, "--strict-refs"]);
    assert_eq!(strict.status.code(), Some(1));
    assert!(out(&strict).contains("a.rs:2") && out(&strict).contains("nope"));
    assert!(run(&["extract", s, "--locales", l]).status.success());
}

/// `--exclude` 可排除路径。
#[test]
fn exclude_skips_paths() {
    let root = fixture("excl", "fn f(i: &I) { i.tr(\"nope\"); }");
    let (loc, src) = (root.join("loc"), root.join("src"));
    let o = run(&[
        "extract",
        src.to_str().unwrap(),
        "--locales",
        loc.to_str().unwrap(),
        "--exclude",
        "a.rs",
        "--strict-refs",
    ]);
    assert!(o.status.success(), "{}", out(&o));
}

/// check 的门禁：id 对齐通过；--strict-refs 遇缺失失败；缺 --src 报用法错误。
#[test]
fn check_gate() {
    let root = fixture("check", "fn f(i: &I) { i.tr(\"nope\"); }");
    let (loc, src) = (root.join("loc"), root.join("src"));
    let (l, s) = (loc.to_str().unwrap(), src.to_str().unwrap());
    assert!(run(&["check", l]).status.success());
    assert_eq!(
        run(&["check", l, "--src", s, "--strict-refs"])
            .status
            .code(),
        Some(1)
    );
    assert_eq!(run(&["check", l, "--strict-refs"]).status.code(), Some(2));
    // 语言间 id 不一致仍失败
    write(&loc.join("zh-CN/m.ftl"), "have = 甲\n");
    assert_eq!(run(&["check", l]).status.code(), Some(1));
}
