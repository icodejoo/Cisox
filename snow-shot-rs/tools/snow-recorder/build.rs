//! 为最终可执行文件补齐静态 FFmpeg 的传递依赖库（x264/x265/webp/zlib 等）。
//!
//! snow-recording-export 的 build.rs 同样会输出这些链接指令，但它们被打包进 rlib，
//! 对独立 bin 的最终链接不生效；这里在 bin 自己的 build.rs 里重新声明一遍。

use std::env;
use std::fs;
use std::path::PathBuf;

/// FFmpeg 自身的库名前缀集合（不需要重复链接）。
const FFMPEG_MODULES: [&str; 5] = [
    "libavformat",
    "libavcodec",
    "libswresample",
    "libswscale",
    "libavutil",
];

/// 构建脚本入口。
fn main() {
    println!("cargo:rerun-if-env-changed=FFMPEG_DIR");
    println!("cargo:rerun-if-env-changed=VCPKGRS_DYNAMIC");
    if env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }
    // 只有静态 FFmpeg 才需要补库
    if env::var("VCPKGRS_DYNAMIC").as_deref() != Ok("0") {
        return;
    }
    let Some(root) = env::var_os("FFMPEG_DIR").map(PathBuf::from) else {
        return;
    };
    let lib_dir = root.join("lib");
    println!("cargo:rustc-link-search=native={}", lib_dir.display());

    let mut linked: Vec<String> = Vec::new();
    for module in FFMPEG_MODULES {
        let pc = lib_dir.join("pkgconfig").join(format!("{module}.pc"));
        println!("cargo:rerun-if-changed={}", pc.display());
        let Ok(text) = fs::read_to_string(&pc) else {
            println!("cargo:warning=读取 {} 失败，跳过", pc.display());
            continue;
        };
        for line in text.lines() {
            let Some(libs) = line
                .strip_prefix("Libs:")
                .or_else(|| line.strip_prefix("Libs.private:"))
            else {
                continue;
            };
            for token in libs.split_whitespace() {
                let Some(name) = token.trim_matches('"').strip_prefix("-l") else {
                    continue;
                };
                let is_ffmpeg = FFMPEG_MODULES
                    .iter()
                    .any(|m| m.strip_prefix("lib") == Some(name));
                if !is_ffmpeg && !linked.iter().any(|l| l == name) {
                    linked.push(name.to_string());
                }
            }
        }
    }
    for name in linked {
        if lib_dir.join(format!("{name}.lib")).is_file() {
            println!("cargo:rustc-link-lib=static={name}");
        } else {
            println!("cargo:rustc-link-lib={name}");
        }
    }
}
