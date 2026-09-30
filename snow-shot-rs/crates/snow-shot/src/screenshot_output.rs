//! 截图输出：保存目录解析、PNG 编码与落盘（保存按钮 / 快捷键使用）。

use image::codecs::png::PngEncoder;
use image::{ExtendedColorType, ImageEncoder};
use serde_json::Value;
use snow_config::document::ConfigDocument;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

/// 保存目录的配置键。
pub const SAVE_DIRECTORY_CONFIG_KEY: &str = "screenshot/image_save_directory";
/// 图片格式的配置键。
pub const IMAGE_FORMAT_CONFIG_KEY: &str = "screenshot/image_format";
/// 当前唯一支持的输出格式（`image` 只开了 png）。
const SUPPORTED_FORMAT: &str = "png";
/// 输出文件名前缀。
const FILE_NAME_PREFIX: &str = "snow-shot";
/// 输出文件扩展名。
const FILE_EXTENSION: &str = "png";
/// 默认图片子目录名（位于用户目录下）。
const PICTURES_DIR_NAME: &str = "Pictures";
/// RGBA 每像素字节数。
const RGBA_BYTES_PER_PIXEL: usize = 4;
/// 同一毫秒内重名时最多尝试的序号。
const MAX_NAME_ATTEMPTS: u32 = 1000;

/// 解析截图保存目录：优先取配置，为空则退回“用户目录/Pictures”，都没有则用系统临时目录。
///
/// # 参数
/// - `document`：配置文档。
/// - `home`：用户目录（一般来自 `USERPROFILE` / `HOME`）。
///
/// # 返回
/// 目录路径与来源说明（用于日志）。
///
/// ```ignore
/// let (dir, source) = resolve_save_directory(&doc, Some(Path::new("C:/Users/a")));
/// ```
pub fn resolve_save_directory(
    document: &ConfigDocument,
    home: Option<&Path>,
) -> (PathBuf, &'static str) {
    if let Value::String(text) = document.value(SAVE_DIRECTORY_CONFIG_KEY) {
        let text = text.trim();
        if !text.is_empty() {
            return (PathBuf::from(text), "config");
        }
    }
    match home {
        Some(home) => (home.join(PICTURES_DIR_NAME), "default-pictures"),
        None => (std::env::temp_dir(), "temp-fallback"),
    }
}

/// 读取当前用户目录（`USERPROFILE`，其次 `HOME`）。
pub fn home_directory() -> Option<PathBuf> {
    std::env::var_os("USERPROFILE")
        .or_else(|| std::env::var_os("HOME"))
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
}

/// 配置的输出格式是否被支持；不支持时调用方应改用 PNG 并记日志。
///
/// # 参数
/// - `document`：配置文档。
///
/// # 返回
/// 配置里的格式名（小写）与是否支持。
pub fn configured_format(document: &ConfigDocument) -> (String, bool) {
    let name = document
        .value(IMAGE_FORMAT_CONFIG_KEY)
        .as_str()
        .unwrap_or(SUPPORTED_FORMAT)
        .to_ascii_lowercase();
    let supported = name == SUPPORTED_FORMAT;
    (name, supported)
}

/// 把 RGBA 像素编码为 PNG 字节。
///
/// # 参数
/// - `width` / `height`：图像尺寸。
/// - `rgba`：RGBA 像素（长度须为 `宽 * 高 * 4`）。
///
/// # 返回
/// PNG 字节；参数不合法或编码失败返回错误说明。
///
/// ```ignore
/// let png = encode_png(1, 1, &[255, 0, 0, 255]).unwrap();
/// assert_eq!(&png[1..4], b"PNG");
/// ```
pub fn encode_png(width: u32, height: u32, rgba: &[u8]) -> Result<Vec<u8>, String> {
    // `image` 在缓冲长度不符时会 panic，这里先校验并返回错误
    let expected = (width as usize)
        .checked_mul(height as usize)
        .and_then(|n| n.checked_mul(RGBA_BYTES_PER_PIXEL))
        .filter(|n| *n > 0);
    if expected != Some(rgba.len()) {
        return Err(format!(
            "像素缓冲长度不符: {width}x{height} 需要 {expected:?} 字节, 实际 {}",
            rgba.len()
        ));
    }
    let mut out = Vec::new();
    PngEncoder::new(&mut out)
        .write_image(rgba, width, height, ExtendedColorType::Rgba8)
        .map_err(|e| format!("PNG 编码失败: {e}"))?;
    Ok(out)
}

/// 生成不与已有文件冲突的输出路径：`snow-shot-<毫秒时间戳>[-序号].png`。
///
/// # 参数
/// - `dir`：输出目录（需已存在）。
/// - `unix_millis`：时间戳（毫秒）。
///
/// # 返回
/// 尚不存在的文件路径；尝试次数耗尽返回错误。
pub fn unique_output_path(dir: &Path, unix_millis: u128) -> Result<PathBuf, String> {
    for attempt in 0..MAX_NAME_ATTEMPTS {
        let name = if attempt == 0 {
            format!("{FILE_NAME_PREFIX}-{unix_millis}.{FILE_EXTENSION}")
        } else {
            format!("{FILE_NAME_PREFIX}-{unix_millis}-{attempt}.{FILE_EXTENSION}")
        };
        let path = dir.join(name);
        if !path.exists() {
            return Ok(path);
        }
    }
    Err("无法生成不冲突的文件名".to_string())
}

/// 把 RGBA 像素保存为 PNG 文件（必要时创建目录）。
///
/// # 参数
/// - `dir`：输出目录。
/// - `width` / `height` / `rgba`：图像尺寸与像素。
///
/// # 返回
/// 成功返回写入的文件路径，失败返回错误说明。
///
/// ```ignore
/// let path = save_png(Path::new("C:/tmp"), 1, 1, &[0, 0, 0, 255])?;
/// ```
pub fn save_png(dir: &Path, width: u32, height: u32, rgba: &[u8]) -> Result<PathBuf, String> {
    let png = encode_png(width, height, rgba)?;
    std::fs::create_dir_all(dir).map_err(|e| format!("创建保存目录失败 {}: {e}", dir.display()))?;
    let millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or_default();
    let path = unique_output_path(dir, millis)?;
    std::fs::write(&path, png).map_err(|e| format!("写入文件失败 {}: {e}", path.display()))?;
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 生成一个唯一的临时目录路径（不创建）。
    fn temp_dir(tag: &str) -> PathBuf {
        std::env::temp_dir().join(format!("snow-shot-output-{tag}-{}", std::process::id()))
    }

    /// 配置为空时退回用户图片目录；无用户目录退回临时目录。
    #[test]
    fn save_directory_fallbacks() {
        let mut doc = ConfigDocument::from_bytes(None);
        doc.set_value(SAVE_DIRECTORY_CONFIG_KEY, Value::String(String::new()))
            .unwrap();
        let (dir, source) = resolve_save_directory(&doc, Some(Path::new("C:/Users/a")));
        assert_eq!(dir, Path::new("C:/Users/a").join("Pictures"));
        assert_eq!(source, "default-pictures");
        let (dir, source) = resolve_save_directory(&doc, None);
        assert_eq!(dir, std::env::temp_dir());
        assert_eq!(source, "temp-fallback");
    }

    /// 配置了目录时优先使用配置。
    #[test]
    fn save_directory_prefers_config() {
        let mut doc = ConfigDocument::from_bytes(None);
        doc.set_value(SAVE_DIRECTORY_CONFIG_KEY, Value::String("D:/shots".into()))
            .unwrap();
        let (dir, source) = resolve_save_directory(&doc, Some(Path::new("C:/Users/a")));
        assert_eq!(dir, PathBuf::from("D:/shots"));
        assert_eq!(source, "config");
    }

    /// 默认配置的格式为 png 且受支持；其它格式判定为不支持。
    #[test]
    fn format_support_detection() {
        let doc = ConfigDocument::from_bytes(None);
        assert_eq!(configured_format(&doc), ("png".to_string(), true));
        let mut doc = doc;
        doc.set_value(IMAGE_FORMAT_CONFIG_KEY, Value::String("webp".into()))
            .unwrap();
        assert_eq!(configured_format(&doc), ("webp".to_string(), false));
    }

    /// PNG 编码结果可被解码回原像素；长度不符报错。
    #[test]
    fn png_roundtrip() {
        let rgba = [255u8, 0, 0, 255, 0, 255, 0, 255];
        let png = encode_png(2, 1, &rgba).unwrap();
        assert_eq!(&png[1..4], b"PNG");
        let decoded = image::load_from_memory_with_format(&png, image::ImageFormat::Png)
            .unwrap()
            .to_rgba8();
        assert_eq!(decoded.dimensions(), (2, 1));
        assert_eq!(decoded.as_raw().as_slice(), &rgba);
        assert!(encode_png(2, 2, &rgba).is_err());
    }

    /// 保存会创建目录、写出可解码的文件，重名时自动加序号。
    #[test]
    fn save_creates_dir_and_avoids_collision() {
        let dir = temp_dir("save");
        let _ = std::fs::remove_dir_all(&dir);
        let rgba = vec![10u8; 4 * 4 * 4];
        let first = save_png(&dir, 4, 4, &rgba).unwrap();
        assert!(first.exists());
        // 强制同一毫秒：直接验证 unique_output_path 对已存在文件的处理
        let stem = first.file_stem().unwrap().to_string_lossy().to_string();
        let millis: u128 = stem.trim_start_matches("snow-shot-").parse().unwrap();
        let next = unique_output_path(&dir, millis).unwrap();
        assert_ne!(next, first);
        assert!(next.to_string_lossy().contains("-1."));
        let decoded = image::open(&first).unwrap().to_rgba8();
        assert_eq!(decoded.dimensions(), (4, 4));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
