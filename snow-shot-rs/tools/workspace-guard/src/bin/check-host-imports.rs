//! 命令行：检查一个 exe 的 PE 导入表是否在宿主白名单内。
//!
//! 用法：`check-host-imports <exe 路径> [--list]`。通过退出码 0；有违规退出码 1；参数或文件问题退出码 2。
//! `--list` 只列出导入的 DLL，不判定（定稿白名单时用）。

use std::process::ExitCode;
use workspace_guard::imports::{check_imports, parse_imports};

/// 参数或文件问题的退出码。
const EXIT_USAGE: u8 = 2;

/// 有违规的退出码。
const EXIT_VIOLATION: u8 = 1;

/// 入口。
fn main() -> ExitCode {
    let mut args = std::env::args().skip(1);
    let Some(path) = args.next() else {
        eprintln!("用法: check-host-imports <exe 路径> [--list]");
        return ExitCode::from(EXIT_USAGE);
    };
    let list_only = args.any(|a| a == "--list");
    let bytes = match std::fs::read(&path) {
        Ok(bytes) => bytes,
        Err(e) => {
            eprintln!("读取 {path} 失败: {e}");
            return ExitCode::from(EXIT_USAGE);
        }
    };
    let imports = match parse_imports(&bytes) {
        Ok(imports) => imports,
        Err(e) => {
            eprintln!("解析 {path} 失败: {e}");
            return ExitCode::from(EXIT_USAGE);
        }
    };
    println!("{path}（{} 字节）", bytes.len());
    println!(
        "静态导入（{}）: {}",
        imports.static_dlls.len(),
        imports.static_dlls.join(", ")
    );
    println!(
        "延迟导入（{}）: {}",
        imports.delay_dlls.len(),
        imports.delay_dlls.join(", ")
    );
    if list_only {
        return ExitCode::SUCCESS;
    }
    let report = check_imports(&imports);
    if report.is_clean() {
        println!("导入表闸门通过");
        return ExitCode::SUCCESS;
    }
    for name in &report.forbidden {
        eprintln!("禁止的导入: {name}（宿主不得链接图形 / 输入法 / 自动化 / 图像编解码）");
    }
    for name in &report.unlisted {
        eprintln!(
            "不在白名单的导入: {name}（确认确有必要后更新 workspace-guard 的白名单，并写明理由）"
        );
    }
    ExitCode::from(EXIT_VIOLATION)
}
