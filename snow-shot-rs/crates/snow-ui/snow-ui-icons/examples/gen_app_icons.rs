//! 一次性工具：把应用 logo（SVG）栅格化成托盘 PNG 与多尺寸 app.ico。
//!
//! 复现命令（在 snow-shot-rs/ 下）：
//! `cargo run -p snow-ui-icons --example gen_app_icons`
//! 可选参数：`gen_app_icons <logo.svg> <输出目录>`，默认读写 crates/snow-shot/assets。

use resvg::{tiny_skia, usvg};
use std::path::{Path, PathBuf};

/// ICO 内各帧的边长（像素）。
const ICO_SIZES: [u32; 7] = [16, 24, 32, 48, 64, 128, 256];
/// 托盘内置 PNG 的边长（像素）。
const TRAY_SIZE: u32 = 128;
/// 默认资源目录（相对 snow-shot-rs/）。
const DEFAULT_ASSETS: &str = "crates/snow-shot/assets";

/// 把 SVG 渲染成 `size`×`size` 的 PNG 字节（RGBA，保留圆角透明）。
fn render_png(tree: &usvg::Tree, size: u32) -> Vec<u8> {
    let mut pixmap = tiny_skia::Pixmap::new(size, size).expect("尺寸有效");
    let scale = size as f32 / tree.size().width();
    resvg::render(
        tree,
        tiny_skia::Transform::from_scale(scale, scale),
        &mut pixmap.as_mut(),
    );
    pixmap.encode_png().expect("PNG 编码失败")
}

/// 把若干 (边长, PNG 字节) 帧拼成 ICO 容器（PNG 压缩帧）。
fn build_ico(frames: &[(u32, Vec<u8>)]) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(&[0, 0, 1, 0]);
    out.extend_from_slice(&(frames.len() as u16).to_le_bytes());
    let mut offset = 6 + 16 * frames.len() as u32;
    for (size, png) in frames {
        // 256 在目录项里写作 0
        let dim = if *size >= 256 { 0 } else { *size as u8 };
        out.extend_from_slice(&[dim, dim, 0, 0]);
        out.extend_from_slice(&1u16.to_le_bytes());
        out.extend_from_slice(&32u16.to_le_bytes());
        out.extend_from_slice(&(png.len() as u32).to_le_bytes());
        out.extend_from_slice(&offset.to_le_bytes());
        offset += png.len() as u32;
    }
    for (_, png) in frames {
        out.extend_from_slice(png);
    }
    out
}

/// 入口：读取 SVG，写出 tray-icon.png 与 app.ico。
fn main() {
    let mut args = std::env::args().skip(1);
    let assets = PathBuf::from(DEFAULT_ASSETS);
    let svg_path = args.next().map(PathBuf::from).unwrap_or(assets.join("logo.svg"));
    let out_dir = args.next().map(PathBuf::from).unwrap_or(assets);
    let data = std::fs::read(&svg_path).expect("读取 SVG 失败");
    let tree = usvg::Tree::from_data(&data, &usvg::Options::default()).expect("解析 SVG 失败");

    write(&out_dir.join("tray-icon.png"), &render_png(&tree, TRAY_SIZE));
    let frames: Vec<_> = ICO_SIZES.iter().map(|&s| (s, render_png(&tree, s))).collect();
    write(&out_dir.join("app.ico"), &build_ico(&frames));
}

/// 写文件并打印路径与大小。
fn write(path: &Path, bytes: &[u8]) {
    std::fs::write(path, bytes).expect("写文件失败");
    println!("{} ({} 字节)", path.display(), bytes.len());
}
