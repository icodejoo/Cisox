//! 构建脚本：给 cisox-latex.exe 嵌入版本资源（FileDescription 等），实现见 `tools/common/worker_version.rs`。

#[path = "../common/worker_version.rs"]
mod worker_version;

/// 构建脚本入口。
fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    worker_version::embed_worker_version("Cisox LaTeX", "cisox-latex.exe");
}
