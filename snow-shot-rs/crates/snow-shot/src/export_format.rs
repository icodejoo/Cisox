//! 截图导出的图片格式与编码：PNG / JPEG / BMP / WebP / PDF。
//!
//! 质量与压缩级别的映射沿用旧 Qt 版：PNG 与 WebP 取压缩级别，JPEG / WebP / PDF 取质量，
//! WebP 质量 100 为无损。JXL / AVIF 暂无编码器，调用方应回退到 PNG。

use crate::export_pdf::{PdfOptions, build_pdf, flatten_on_white};
use image::codecs::bmp::BmpEncoder;
use image::codecs::jpeg::JpegEncoder;
use image::codecs::png::{CompressionType, FilterType, PngEncoder};
use image::codecs::webp::WebPEncoder;
use image::{ExtendedColorType, ImageEncoder};
use snow_app_core::command::{CompressionLevel, Format, PdfPageSize};
use snow_platform::local_time::LocalDateTime;

/// 质量上限（也是“无损”取值）。
pub const QUALITY_MAX: u8 = 100;
/// 默认质量（与配置默认值一致）。
pub const DEFAULT_QUALITY: u8 = QUALITY_MAX;
/// RGBA 每像素字节数。
const RGBA_BYTES: usize = 4;

/// 能真正写出的图片格式。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ExportFormat {
    /// PNG（无损）。
    Png,
    /// JPEG（有损，取质量）。
    Jpeg,
    /// BMP（无压缩）。
    Bmp,
    /// WebP（目前只有无损编码器）。
    Webp,
    /// PDF 单页。
    Pdf,
}

impl ExportFormat {
    /// 全部格式，顺序即保存对话框里的过滤项顺序（与旧版一致，去掉暂不支持的 JXL / AVIF）。
    pub const ALL: [ExportFormat; 5] = [
        ExportFormat::Png,
        ExportFormat::Jpeg,
        ExportFormat::Bmp,
        ExportFormat::Webp,
        ExportFormat::Pdf,
    ];

    /// 由配置值解析（大小写不敏感，`jpg` 同 `jpeg`）；未知或暂不支持（jxl / avif）返回 `None`。
    ///
    /// ```ignore
    /// assert_eq!(ExportFormat::from_key("JPG"), Some(ExportFormat::Jpeg));
    /// assert_eq!(ExportFormat::from_key("avif"), None);
    /// ```
    pub fn from_key(key: &str) -> Option<Self> {
        match key.trim().to_ascii_lowercase().as_str() {
            "png" => Some(Self::Png),
            "jpeg" | "jpg" => Some(Self::Jpeg),
            "bmp" => Some(Self::Bmp),
            "webp" => Some(Self::Webp),
            "pdf" => Some(Self::Pdf),
            _ => None,
        }
    }

    /// 由命令层格式枚举转换；暂不支持的格式返回 `None`。
    pub fn from_command(format: Format) -> Option<Self> {
        match format {
            Format::Png => Some(Self::Png),
            Format::Jpeg => Some(Self::Jpeg),
            Format::Webp => Some(Self::Webp),
            Format::Bmp => Some(Self::Bmp),
            Format::Pdf => Some(Self::Pdf),
            Format::Avif | Format::Jxl => None,
        }
    }

    /// 由文件扩展名推断格式（不区分大小写，不含点）。
    ///
    /// ```ignore
    /// assert_eq!(ExportFormat::from_extension("JPEG"), Some(ExportFormat::Jpeg));
    /// ```
    pub fn from_extension(ext: &str) -> Option<Self> {
        Self::from_key(ext)
    }

    /// 配置里保存的键值（与 `screenshot/last_manual_save_format` 取值一致）。
    pub fn key(self) -> &'static str {
        match self {
            Self::Png => "png",
            Self::Jpeg => "jpeg",
            Self::Bmp => "bmp",
            Self::Webp => "webp",
            Self::Pdf => "pdf",
        }
    }

    /// 输出文件扩展名（JPEG 为 `jpg`，与旧版一致）。
    pub fn extension(self) -> &'static str {
        match self {
            Self::Jpeg => "jpg",
            other => other.key(),
        }
    }

    /// 保存对话框过滤项的匹配模式。
    pub fn pattern(self) -> &'static str {
        match self {
            Self::Png => "*.png",
            Self::Jpeg => "*.jpg;*.jpeg",
            Self::Bmp => "*.bmp",
            Self::Webp => "*.webp",
            Self::Pdf => "*.pdf",
        }
    }

    /// 是否使用质量参数。
    pub fn supports_quality(self) -> bool {
        matches!(self, Self::Jpeg | Self::Webp | Self::Pdf)
    }

    /// 是否使用压缩级别参数。
    pub fn supports_compression(self) -> bool {
        matches!(self, Self::Png | Self::Webp)
    }
}

/// 一次编码的参数（已合并配置与请求覆盖）。
#[derive(Debug, Clone)]
pub struct EncodeSettings {
    /// 质量 0..=100（JPEG / WebP / PDF 使用，100 为无损）。
    pub quality: u8,
    /// 压缩级别（PNG / WebP 使用）。
    pub compression: CompressionLevel,
    /// PDF 页面尺寸。
    pub pdf_page: PdfPageSize,
    /// PDF 标题。
    pub pdf_title: String,
    /// 创建时间（PDF 信息字典用）。
    pub created: LocalDateTime,
}

/// 校验 RGBA 缓冲长度。
fn check_buffer(width: u32, height: u32, rgba: &[u8]) -> Result<(), String> {
    let expected = (width as usize)
        .checked_mul(height as usize)
        .and_then(|n| n.checked_mul(RGBA_BYTES))
        .filter(|n| *n > 0);
    if expected != Some(rgba.len()) {
        return Err(format!(
            "pixel buffer length mismatch: {width}x{height} needs {expected:?} bytes, got {}",
            rgba.len()
        ));
    }
    Ok(())
}

/// 压缩级别到 PNG 压缩类型（低 = 最快，中 = 默认，高 = 最小）。
fn png_compression(level: CompressionLevel) -> CompressionType {
    match level {
        CompressionLevel::Low => CompressionType::Fast,
        CompressionLevel::Medium => CompressionType::Default,
        CompressionLevel::High => CompressionType::Best,
    }
}

/// 把 RGBA 编码成目标格式的字节。
///
/// # 参数
/// - `format`：目标格式。
/// - `width` / `height`：图像尺寸。
/// - `rgba`：RGBA 像素（长度须为 `宽 * 高 * 4`）。
/// - `settings`：质量、压缩级别与 PDF 参数。
///
/// # 返回
/// 编码后的文件字节；缓冲不合法或编码失败返回错误说明。JPEG 与有损 PDF 会把透明区域合成到白底。
/// WebP 目前只有无损编码，质量小于 100 时同样输出无损（调用方可据 [`webp_lossy_unsupported`] 提示）。
///
/// ```ignore
/// let bytes = encode(ExportFormat::Png, 1, 1, &[0, 0, 0, 255], &settings)?;
/// assert_eq!(&bytes[1..4], b"PNG");
/// ```
pub fn encode(
    format: ExportFormat,
    width: u32,
    height: u32,
    rgba: &[u8],
    settings: &EncodeSettings,
) -> Result<Vec<u8>, String> {
    check_buffer(width, height, rgba)?;
    let mut out = Vec::new();
    match format {
        ExportFormat::Png => {
            PngEncoder::new_with_quality(
                &mut out,
                png_compression(settings.compression),
                FilterType::Adaptive,
            )
            .write_image(rgba, width, height, ExtendedColorType::Rgba8)
            .map_err(|e| format!("PNG encode failed: {e}"))?;
        }
        ExportFormat::Jpeg => {
            let rgb = flatten_on_white(rgba);
            JpegEncoder::new_with_quality(&mut out, settings.quality.clamp(1, QUALITY_MAX))
                .write_image(&rgb, width, height, ExtendedColorType::Rgb8)
                .map_err(|e| format!("JPEG encode failed: {e}"))?;
        }
        ExportFormat::Bmp => {
            BmpEncoder::new(&mut out)
                .write_image(rgba, width, height, ExtendedColorType::Rgba8)
                .map_err(|e| format!("BMP encode failed: {e}"))?;
        }
        ExportFormat::Webp => {
            WebPEncoder::new_lossless(&mut out)
                .write_image(rgba, width, height, ExtendedColorType::Rgba8)
                .map_err(|e| format!("WebP encode failed: {e}"))?;
        }
        ExportFormat::Pdf => {
            out = build_pdf(
                width,
                height,
                rgba,
                &PdfOptions {
                    page: settings.pdf_page,
                    quality: settings.quality.clamp(1, QUALITY_MAX),
                    title: settings.pdf_title.clone(),
                    created: settings.created,
                },
            )?;
        }
    }
    if out.is_empty() {
        return Err("encoded output is empty".to_string());
    }
    Ok(out)
}

/// 请求的 WebP 质量是否超出了现有（仅无损）编码器的能力。
///
/// ```ignore
/// assert!(webp_lossy_unsupported(80));
/// assert!(!webp_lossy_unsupported(100));
/// ```
pub fn webp_lossy_unsupported(quality: u8) -> bool {
    quality < QUALITY_MAX
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 默认测试参数。
    fn settings(quality: u8, compression: CompressionLevel) -> EncodeSettings {
        EncodeSettings {
            quality,
            compression,
            pdf_page: PdfPageSize::ImageSize,
            pdf_title: "t".to_string(),
            created: LocalDateTime {
                year: 2026,
                month: 8,
                day: 14,
                hour: 9,
                minute: 7,
                second: 6,
            },
        }
    }

    /// 带噪声的纹理图：让有损质量和压缩级别都能拉开体积差距。
    fn noisy(w: u32, h: u32) -> Vec<u8> {
        let mut state = 0x1234_5678_u32;
        let mut data = Vec::new();
        for y in 0..h {
            for x in 0..w {
                state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                let n = (state >> 24) as u8 / 4;
                data.extend_from_slice(&[
                    ((x * 3) as u8).wrapping_add(n),
                    ((y * 3) as u8).wrapping_add(n),
                    ((x + y) as u8).wrapping_add(n),
                    255,
                ]);
            }
        }
        data
    }

    /// 平滑渐变图：PNG 压缩级别差异明显。
    fn smooth(w: u32, h: u32) -> Vec<u8> {
        let mut data = Vec::new();
        for y in 0..h {
            for x in 0..w {
                data.extend_from_slice(&[(x / 2) as u8, (y / 2) as u8, ((x + y) / 4) as u8, 255]);
            }
        }
        data
    }

    /// 各格式文件头校验。
    #[test]
    fn magic_bytes_per_format() {
        let img = noisy(16, 16);
        let s = settings(80, CompressionLevel::Medium);
        let png = encode(ExportFormat::Png, 16, 16, &img, &s).unwrap();
        assert_eq!(&png[..8], b"\x89PNG\r\n\x1a\n");
        let jpg = encode(ExportFormat::Jpeg, 16, 16, &img, &s).unwrap();
        assert_eq!(&jpg[..3], &[0xFF, 0xD8, 0xFF]);
        assert_eq!(&jpg[jpg.len() - 2..], &[0xFF, 0xD9]);
        let bmp = encode(ExportFormat::Bmp, 16, 16, &img, &s).unwrap();
        assert_eq!(&bmp[..2], b"BM");
        let webp = encode(ExportFormat::Webp, 16, 16, &img, &s).unwrap();
        assert_eq!(&webp[..4], b"RIFF");
        assert_eq!(&webp[8..12], b"WEBP");
        let pdf = encode(ExportFormat::Pdf, 16, 16, &img, &s).unwrap();
        assert!(pdf.starts_with(b"%PDF-1.7"));
    }

    /// 无损格式往返后像素一致，JPEG 往返后尺寸一致。
    #[test]
    fn roundtrip_decodes() {
        let img = noisy(9, 7);
        let s = settings(100, CompressionLevel::High);
        for (format, kind) in [
            (ExportFormat::Png, image::ImageFormat::Png),
            (ExportFormat::Webp, image::ImageFormat::WebP),
            (ExportFormat::Bmp, image::ImageFormat::Bmp),
        ] {
            let bytes = encode(format, 9, 7, &img, &s).unwrap();
            let decoded = image::load_from_memory_with_format(&bytes, kind)
                .unwrap()
                .to_rgba8();
            assert_eq!(decoded.dimensions(), (9, 7), "{format:?}");
            assert_eq!(decoded.as_raw(), &img, "{format:?} 应无损");
        }
        let jpg = encode(ExportFormat::Jpeg, 9, 7, &img, &s).unwrap();
        let decoded = image::load_from_memory_with_format(&jpg, image::ImageFormat::Jpeg).unwrap();
        assert_eq!((decoded.width(), decoded.height()), (9, 7));
    }

    /// 质量参数对 JPEG 生效：质量越低体积越小。
    #[test]
    fn jpeg_quality_changes_output() {
        let img = noisy(64, 64);
        let low = encode(
            ExportFormat::Jpeg,
            64,
            64,
            &img,
            &settings(10, CompressionLevel::Medium),
        )
        .unwrap();
        let high = encode(
            ExportFormat::Jpeg,
            64,
            64,
            &img,
            &settings(95, CompressionLevel::Medium),
        )
        .unwrap();
        assert!(low.len() < high.len(), "{} vs {}", low.len(), high.len());
    }

    /// 压缩级别对 PNG 生效：高 <= 中 <= 低（体积），且高与低的字节不同。
    #[test]
    fn png_compression_level_changes_output() {
        let img = smooth(128, 128);
        let size = |level| {
            encode(ExportFormat::Png, 128, 128, &img, &settings(100, level))
                .unwrap()
                .len()
        };
        let (low, medium, high) = (
            size(CompressionLevel::Low),
            size(CompressionLevel::Medium),
            size(CompressionLevel::High),
        );
        assert!(high <= medium && medium <= low, "{low} {medium} {high}");
        assert!(high < low, "最高与最低压缩应产生不同体积: {low} {high}");
    }

    /// JPEG 对透明区域按白底处理（不会出现黑块）。
    #[test]
    fn jpeg_flattens_transparency_on_white() {
        let rgba = [0u8, 0, 0, 0].repeat(8 * 8);
        let bytes = encode(
            ExportFormat::Jpeg,
            8,
            8,
            &rgba,
            &settings(100, CompressionLevel::Medium),
        )
        .unwrap();
        let decoded = image::load_from_memory_with_format(&bytes, image::ImageFormat::Jpeg)
            .unwrap()
            .to_rgb8();
        assert!(decoded.pixels().all(|p| p.0.iter().all(|c| *c > 240)));
    }

    /// 缓冲长度不符或尺寸为 0 时报错而不是 panic。
    #[test]
    fn invalid_buffer_is_rejected() {
        let s = settings(100, CompressionLevel::Medium);
        for format in ExportFormat::ALL {
            assert!(encode(format, 2, 2, &[0; 4], &s).is_err(), "{format:?}");
            assert!(encode(format, 0, 0, &[], &s).is_err(), "{format:?}");
        }
    }

    /// 格式键、扩展名与命令层枚举的互转（对照 Qt `formatForKey` / `extension`）。
    #[test]
    fn keys_and_extensions() {
        assert_eq!(ExportFormat::from_key("PDF"), Some(ExportFormat::Pdf));
        assert_eq!(ExportFormat::from_key(" jpg "), Some(ExportFormat::Jpeg));
        assert_eq!(ExportFormat::from_key("avif"), None);
        assert_eq!(ExportFormat::from_key("jxl"), None);
        assert_eq!(ExportFormat::from_key("nope"), None);
        assert_eq!(ExportFormat::Jpeg.extension(), "jpg");
        assert_eq!(ExportFormat::Jpeg.key(), "jpeg");
        assert_eq!(ExportFormat::from_command(Format::Avif), None);
        assert_eq!(
            ExportFormat::from_command(Format::Bmp),
            Some(ExportFormat::Bmp)
        );
        for format in ExportFormat::ALL {
            assert_eq!(ExportFormat::from_key(format.key()), Some(format));
            assert_eq!(
                ExportFormat::from_extension(format.extension()),
                Some(format)
            );
        }
        assert!(ExportFormat::Jpeg.supports_quality() && !ExportFormat::Png.supports_quality());
        assert!(
            ExportFormat::Png.supports_compression() && !ExportFormat::Jpeg.supports_compression()
        );
        assert!(webp_lossy_unsupported(99) && !webp_lossy_unsupported(100));
    }
}
