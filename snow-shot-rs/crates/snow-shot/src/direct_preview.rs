//! 直接截图的通知预览：缩略图与临时副本（点通知时打开的目录）。

use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

/// 缩略图边长（像素，正方形，居中留透明边）。
pub const THUMBNAIL_SIDE: u32 = 64;
/// 临时目录最多保留的截图张数。
pub const TEMP_KEEP_COUNT: usize = 20;
/// 临时目录相对系统临时目录的路径。
const TEMP_SUBDIR: [&str; 2] = ["Cisox", "captures"];
/// 临时截图文件名前缀。
const TEMP_FILE_PREFIX: &str = "capture-";
/// 临时截图文件扩展名。
const TEMP_FILE_EXT: &str = "png";
/// 文件名里毫秒时间戳的位数（补零，保证按名字排序等于按时间排序）。
const TIMESTAMP_DIGITS: usize = 13;

/// 通知用的缩略图（RGBA，行优先，无行间填充）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Thumbnail {
    /// 宽（像素）。
    pub width: u32,
    /// 高（像素）。
    pub height: u32,
    /// RGBA 像素。
    pub rgba: Vec<u8>,
}

/// 把图像等比缩进 `side`×`side` 的方块并居中，其余留透明；用最近邻取样，够小图标用。
///
/// # 参数
/// - `width` / `height` / `rgba`：原图尺寸与 RGBA 像素。
/// - `side`：缩略图边长。
///
/// # 返回
/// 缩略图；原图为空或像素长度不对时返回 `None`。
///
/// ```ignore
/// let thumb = make_thumbnail(2560, 1440, &rgba, THUMBNAIL_SIDE).unwrap();
/// assert_eq!((thumb.width, thumb.height), (64, 64));
/// ```
pub fn make_thumbnail(width: u32, height: u32, rgba: &[u8], side: u32) -> Option<Thumbnail> {
    if width == 0 || height == 0 || side == 0 || rgba.len() != width as usize * height as usize * 4
    {
        return None;
    }
    // 等比缩放后的内容尺寸，至少 1 像素
    let scale = (side as f64 / width as f64).min(side as f64 / height as f64);
    let content_w = ((width as f64 * scale).round() as u32).clamp(1, side);
    let content_h = ((height as f64 * scale).round() as u32).clamp(1, side);
    let offset_x = (side - content_w) / 2;
    let offset_y = (side - content_h) / 2;
    let mut out = vec![0u8; side as usize * side as usize * 4];
    for y in 0..content_h {
        let src_y = (u64::from(y) * u64::from(height) / u64::from(content_h)) as usize;
        for x in 0..content_w {
            let src_x = (u64::from(x) * u64::from(width) / u64::from(content_w)) as usize;
            let src = (src_y * width as usize + src_x) * 4;
            let dst = ((y + offset_y) as usize * side as usize + (x + offset_x) as usize) * 4;
            out[dst..dst + 4].copy_from_slice(&rgba[src..src + 4]);
        }
    }
    Some(Thumbnail {
        width: side,
        height: side,
        rgba: out,
    })
}

/// 临时截图目录：`<系统临时目录>/Cisox/captures`。
///
/// ```ignore
/// let dir = captures_dir();
/// ```
pub fn captures_dir() -> PathBuf {
    TEMP_SUBDIR
        .iter()
        .fold(std::env::temp_dir(), |path, part| path.join(part))
}

/// 把截图写成 PNG 放进 `dir`（目录不存在就创建），并清掉超出 `keep` 张的旧图。
///
/// # 参数
/// - `dir`：临时目录。
/// - `width` / `height` / `rgba`：图像尺寸与 RGBA 像素。
/// - `keep`：保留的最大张数（含本次）。
///
/// # 返回
/// 写入的文件路径；失败返回可读的原因。清理旧图失败只记日志，不算失败。
///
/// ```ignore
/// let path = save_temp_png(&captures_dir(), w, h, &rgba, TEMP_KEEP_COUNT)?;
/// ```
pub fn save_temp_png(
    dir: &Path,
    width: u32,
    height: u32,
    rgba: &[u8],
    keep: usize,
) -> Result<PathBuf, String> {
    std::fs::create_dir_all(dir).map_err(|e| format!("创建临时目录失败：{e}"))?;
    let millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_millis());
    let path = dir.join(format!(
        "{TEMP_FILE_PREFIX}{millis:0TIMESTAMP_DIGITS$}.{TEMP_FILE_EXT}"
    ));
    image::save_buffer(&path, rgba, width, height, image::ExtendedColorType::Rgba8)
        .map_err(|e| format!("写入临时截图失败：{e}"))?;
    prune_captures(dir, keep);
    Ok(path)
}

/// 只保留 `dir` 里最新的 `keep` 张临时截图（按文件名排序，名字里是毫秒时间戳）。
///
/// # 参数
/// - `dir`：临时目录。
/// - `keep`：保留张数。
pub fn prune_captures(dir: &Path, keep: usize) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let mut files: Vec<PathBuf> = entries
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| is_temp_capture(path))
        .collect();
    files.sort();
    let stale = files.len().saturating_sub(keep);
    for path in files.into_iter().take(stale) {
        if let Err(e) = std::fs::remove_file(&path) {
            tracing::debug!(error = %e, path = %path.display(), "清理旧临时截图失败");
        }
    }
}

/// 是不是本模块写出的临时截图（只清理这类文件，别的文件不碰）。
fn is_temp_capture(path: &Path) -> bool {
    let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
    name.starts_with(TEMP_FILE_PREFIX)
        && path.extension().and_then(|e| e.to_str()) == Some(TEMP_FILE_EXT)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 每个用例一个独立临时目录，避免互相干扰。
    fn scratch(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("cisox-preview-test-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    /// 宽图缩进正方形：内容等比、垂直居中、上下留透明。
    #[test]
    fn thumbnail_letterboxes_wide_image() {
        let rgba = vec![255u8; 200 * 100 * 4];
        let thumb = make_thumbnail(200, 100, &rgba, 64).unwrap();
        assert_eq!((thumb.width, thumb.height), (64, 64));
        let alpha = |x: usize, y: usize| thumb.rgba[(y * 64 + x) * 4 + 3];
        assert_eq!(alpha(32, 0), 0, "顶部是透明边");
        assert_eq!(alpha(32, 32), 255, "中间是内容");
        assert_eq!(alpha(32, 63), 0, "底部是透明边");
    }

    /// 长度对不上或尺寸为 0 时不出图。
    #[test]
    fn thumbnail_rejects_bad_input() {
        assert!(make_thumbnail(0, 10, &[], 64).is_none());
        assert!(make_thumbnail(2, 2, &[0; 4], 64).is_none());
    }

    /// 极端细长的图也至少有 1 像素内容。
    #[test]
    fn thumbnail_keeps_one_pixel_minimum() {
        let rgba = vec![255u8; 1000 * 4];
        let thumb = make_thumbnail(1000, 1, &rgba, 64).unwrap();
        assert!(thumb.rgba.chunks(4).any(|px| px[3] == 255));
    }

    /// 清理只留最新的几张，并且不碰别的文件。
    #[test]
    fn prune_keeps_newest_and_foreign_files() {
        let dir = scratch("prune");
        std::fs::create_dir_all(&dir).unwrap();
        for n in 1..=5 {
            std::fs::write(dir.join(format!("capture-{n:013}.png")), b"x").unwrap();
        }
        std::fs::write(dir.join("notes.txt"), b"x").unwrap();
        prune_captures(&dir, 2);
        let mut left: Vec<String> = std::fs::read_dir(&dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        left.sort();
        assert_eq!(
            left,
            [
                "capture-0000000000004.png",
                "capture-0000000000005.png",
                "notes.txt"
            ]
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 写出的是能被解回来的 PNG，且目录会被自动创建。
    #[test]
    fn save_temp_png_writes_decodable_file() {
        let dir = scratch("save").join("nested");
        let rgba = vec![200u8; 4 * 3 * 4];
        let path = save_temp_png(&dir, 4, 3, &rgba, TEMP_KEEP_COUNT).unwrap();
        let image = image::open(&path).unwrap();
        assert_eq!((image.width(), image.height()), (4, 3));
        let _ = std::fs::remove_dir_all(dir.parent().unwrap());
    }
}
