//! 示例共用工具（`mod common;` 引入；该目录不会被当成独立示例）。

use std::io::Write;
use std::path::{Path, PathBuf};

/// 在系统临时目录下取日志路径，并先删掉上次运行遗留的同名文件。
pub fn fresh_log_path(file_name: &str) -> PathBuf {
    let path = std::env::temp_dir().join(file_name);
    let _ = std::fs::remove_file(&path);
    path
}

/// 打印一行并追加写入日志文件，文件打不开时只打印。
pub fn log(path: &Path, line: &str) {
    println!("{line}");
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
    {
        let _ = writeln!(f, "{line}");
    }
}
