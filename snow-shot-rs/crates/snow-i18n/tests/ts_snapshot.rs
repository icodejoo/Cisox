//! `.ts` 解析快照测试：全部真实 `.ts` 的解析产物必须与基线完全一致。

use snow_i18n::ts::parse_ts;
use std::path::{Path, PathBuf};

/// 基线快照路径。
const SNAPSHOT: &str = "tests/fixtures/ts_snapshot.txt";

/// 仓库根目录。
fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../..")
}

/// 递归找 `.ts`。
fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
    for e in std::fs::read_dir(dir).unwrap() {
        let p = e.unwrap().path();
        if p.is_dir() {
            walk(&p, out);
        } else if p.extension().is_some_and(|x| x == "ts") {
            out.push(p);
        }
    }
}

/// 生成全部真实 `.ts` 的调试串（按相对路径排序）。
fn dump_all() -> String {
    let root = repo_root();
    let mut files = Vec::new();
    walk(&root.join("snow_shot/i18n"), &mut files);
    walk(
        &root.join("ant_design_qt/packages/ant_design_qt/i18n"),
        &mut files,
    );
    // 繁体已不支持：上游目录里的 zh_TW 的 .ts 不再参与解析基线。
    files.retain(|f| !f.to_string_lossy().ends_with("_zh_TW.ts"));
    files.sort();
    assert!(
        files.len() >= 20,
        "应至少找到 20 个 .ts，实际 {}",
        files.len()
    );
    let mut out = String::new();
    for f in files {
        let name = f.file_name().unwrap().to_string_lossy().to_string();
        let text = std::fs::read_to_string(&f).unwrap();
        out.push_str(&format!("== {name}\n{:#?}\n", parse_ts(&text).unwrap()));
    }
    out
}

/// 解析产物与已提交基线逐字一致；设置 `UPDATE_TS_SNAPSHOT=1` 可重写基线。
#[test]
fn real_ts_parse_matches_snapshot() {
    let actual = dump_all();
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join(SNAPSHOT);
    if std::env::var_os("UPDATE_TS_SNAPSHOT").is_some() {
        std::fs::write(&path, &actual).unwrap();
        return;
    }
    let expected = std::fs::read_to_string(&path)
        .unwrap()
        .replace("\r\n", "\n");
    assert!(actual == expected, "解析产物与基线不一致");
}
