//! `.ts` 解析快照测试：全部真实 `.ts` 的解析产物必须与基线完全一致。

mod common;

use snow_i18n::ts::parse_ts;
use std::path::Path;

/// 基线快照路径。
const SNAPSHOT: &str = "tests/fixtures/ts_snapshot.txt";

/// 生成全部真实 `.ts` 的调试串（按路径排序）。
fn dump_all() -> String {
    let mut out = String::new();
    for f in common::legacy_ts_files() {
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
