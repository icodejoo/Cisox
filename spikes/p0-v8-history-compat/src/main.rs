//! P0-V8：验证 Rust 能读写参照版 Snow Shot 的截图历史 index.json 格式。
use std::path::Path;

use p0_v8_history_compat::{load_index, validate_index_on_disk};

/// 入口：读取样本目录，校验并打印摘要。用法：`cargo run -- <capture_history 目录>`
fn main() {
    let root = std::env::args().nth(1).unwrap_or_else(|| "sample".into());
    let root = Path::new(&root);
    let index = load_index(&std::fs::read(root.join("index.json")).expect("读取 index.json"))
        .expect("解析 index.json");
    println!(
        "format_version={} records={} pending={}",
        index.format_version,
        index.records.len(),
        index.pending_deletions.len()
    );
    match validate_index_on_disk(&index, root) {
        Ok(()) => println!("磁盘校验通过"),
        Err(e) => println!("磁盘校验失败: {e}"),
    }
}
