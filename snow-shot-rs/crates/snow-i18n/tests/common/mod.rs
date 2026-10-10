//! 集成测试共用夹具：定位上游真实 `.ts` 文件。

use std::path::{Path, PathBuf};

/// 至少应找到的 `.ts` 文件数，防止目录改名后测试空转。
const MIN_TS_FILES: usize = 20;

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

/// 收集上游全部真实 `.ts`（按路径排序）。
///
/// 繁体已不支持，上游 `zh_TW` 的 `.ts` 一律剔除；少于 [`MIN_TS_FILES`] 个时直接失败。
pub fn legacy_ts_files() -> Vec<PathBuf> {
    let root = repo_root();
    let mut files = Vec::new();
    walk(&root.join("snow_shot/i18n"), &mut files);
    walk(
        &root.join("ant_design_qt/packages/ant_design_qt/i18n"),
        &mut files,
    );
    files.retain(|f| !f.to_string_lossy().ends_with("_zh_TW.ts"));
    files.sort();
    assert!(
        files.len() >= MIN_TS_FILES,
        "应至少找到 {MIN_TS_FILES} 个 .ts，实际 {}",
        files.len()
    );
    files
}

/// 读取全部真实 `.ts`，返回 `(文件名, 原文)`（顺序同 [`legacy_ts_files`]）。
pub fn legacy_ts_sources() -> Vec<(String, String)> {
    legacy_ts_files()
        .into_iter()
        .map(|f| {
            let name = f.file_name().unwrap().to_string_lossy().to_string();
            (name, std::fs::read_to_string(&f).unwrap())
        })
        .collect()
}
