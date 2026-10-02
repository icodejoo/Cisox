//! 把截图写成单页 PDF（手写最小 PDF 1.7 写出器，不引入第三方 PDF 库）。
//!
//! 版面规则与旧 Qt 版 `screenshotpdfexport.cpp` 一致：按 96 dpi 换算页面、A4 纵 / 横居中等比缩放；
//! 质量 100 无损（Flate），小于 100 为 JPEG（DCT）。无损流直接复用 PNG 的 IDAT（PDF 的 PNG 预测器），
//! 因此不需要额外的压缩依赖。

use image::codecs::jpeg::JpegEncoder;
use image::codecs::png::{CompressionType, FilterType, PngEncoder};
use image::{ExtendedColorType, ImageEncoder};
use snow_app_core::PRODUCT_NAME;
use snow_app_core::command::PdfPageSize;
use snow_platform::local_time::LocalDateTime;

/// PDF 点数换算：1 像素 = 72 / 96 点。
const POINTS_PER_PIXEL: f64 = 72.0 / 96.0;
/// A4 宽（毫米）。
const A4_WIDTH_MM: f64 = 210.0;
/// A4 高（毫米）。
const A4_HEIGHT_MM: f64 = 297.0;
/// 每英寸毫米数。
const MM_PER_INCH: f64 = 25.4;
/// 每英寸点数。
const POINTS_PER_INCH: f64 = 72.0;
/// 页面边长超过该点数时用 `/UserUnit` 放大（PDF 规范建议上限）。
const MAX_PAGE_POINTS: f64 = 14_400.0;
/// 无损质量阈值：达到该值走 Flate 无损。
const LOSSLESS_QUALITY: u8 = 100;
/// 标题最大字符数。
pub const MAX_TITLE_CHARS: usize = 1024;
/// 图像边长上限（与旧版一致）。
const MAX_SIDE: u32 = 1_000_000;
/// PNG 文件签名长度。
const PNG_SIGNATURE_LEN: usize = 8;
/// PNG 块头（长度 + 类型）与 CRC 的字节数。
const PNG_CHUNK_OVERHEAD: usize = 12;
/// RGBA 每像素字节数。
const RGBA_BYTES: usize = 4;

/// PDF 写出参数。
#[derive(Debug, Clone)]
pub struct PdfOptions {
    /// 页面尺寸策略。
    pub page: PdfPageSize,
    /// 质量 1..=100；100 为无损。
    pub quality: u8,
    /// 文档标题（超过 [`MAX_TITLE_CHARS`] 会被截断）。
    pub title: String,
    /// 创建时间（写入信息字典）。
    pub created: LocalDateTime,
}

/// 页面与图片位置（单位：点，原点在页面左上，y 向下）。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Layout {
    /// 页面宽。
    pub page_width: f64,
    /// 页面高。
    pub page_height: f64,
    /// 图片左边距。
    pub image_x: f64,
    /// 图片上边距。
    pub image_y: f64,
    /// 图片宽。
    pub image_width: f64,
    /// 图片高。
    pub image_height: f64,
}

/// 计算页面与图片排版。
///
/// # 参数
/// - `width` / `height`：图片像素尺寸（须大于 0）。
/// - `page`：页面尺寸策略。
///
/// # 返回
/// 排版结果；图片尺寸为 0 时返回 `None`。`ImageSize` 页面按 96 dpi 换算，A4 页面内等比缩放并居中。
///
/// ```ignore
/// let l = page_layout(960, 480, PdfPageSize::ImageSize).unwrap();
/// assert_eq!(l.page_width, 720.0);
/// ```
pub fn page_layout(width: u32, height: u32, page: PdfPageSize) -> Option<Layout> {
    if width == 0 || height == 0 {
        return None;
    }
    let (w, h) = (f64::from(width), f64::from(height));
    let (page_width, page_height) = match page {
        PdfPageSize::ImageSize => (w * POINTS_PER_PIXEL, h * POINTS_PER_PIXEL),
        PdfPageSize::A4Portrait | PdfPageSize::A4Landscape => {
            let a4 = (
                A4_WIDTH_MM * POINTS_PER_INCH / MM_PER_INCH,
                A4_HEIGHT_MM * POINTS_PER_INCH / MM_PER_INCH,
            );
            if page == PdfPageSize::A4Landscape {
                (a4.1, a4.0)
            } else {
                a4
            }
        }
    };
    let scale = (page_width / w).min(page_height / h);
    let (image_width, image_height) = (w * scale, h * scale);
    Some(Layout {
        page_width,
        page_height,
        image_x: (page_width - image_width) / 2.0,
        image_y: (page_height - image_height) / 2.0,
        image_width,
        image_height,
    })
}

/// 把 RGBA 按白底合成成 RGB（含隐藏的 RGB 分量）。
///
/// # 参数
/// - `rgba`：RGBA 像素。
///
/// # 返回
/// 与像素数等长的 RGB 数据。
pub fn flatten_on_white(rgba: &[u8]) -> Vec<u8> {
    let mut rgb = Vec::with_capacity(rgba.len() / RGBA_BYTES * 3);
    for px in rgba.chunks_exact(RGBA_BYTES) {
        let a = u32::from(px[3]);
        for &c in &px[..3] {
            rgb.push(((u32::from(c) * a + 255 * (255 - a) + 127) / 255) as u8);
        }
    }
    rgb
}

/// 取出 PNG 里所有 IDAT 块的数据并拼接（即完整的 zlib 流）。
///
/// # 参数
/// - `png`：完整 PNG 字节。
///
/// # 返回
/// 拼接后的 IDAT 数据；格式不合法返回错误说明。
pub fn extract_idat(png: &[u8]) -> Result<Vec<u8>, String> {
    let mut pos = PNG_SIGNATURE_LEN;
    let mut out = Vec::new();
    while pos + PNG_CHUNK_OVERHEAD <= png.len() {
        let len = u32::from_be_bytes([png[pos], png[pos + 1], png[pos + 2], png[pos + 3]]) as usize;
        let kind = &png[pos + 4..pos + 8];
        let data_end = pos + 8 + len;
        if data_end + 4 > png.len() {
            return Err("PNG chunk length out of range".to_string());
        }
        if kind == b"IDAT" {
            out.extend_from_slice(&png[pos + 8..data_end]);
        }
        if kind == b"IEND" {
            break;
        }
        pos = data_end + 4;
    }
    if out.is_empty() {
        Err("PNG has no image data".to_string())
    } else {
        Ok(out)
    }
}

/// 把单通道或三通道 8 位数据编码成 PNG 并取出其 IDAT。
fn png_idat(
    data: &[u8],
    width: u32,
    height: u32,
    color: ExtendedColorType,
) -> Result<Vec<u8>, String> {
    let mut png = Vec::new();
    PngEncoder::new_with_quality(&mut png, CompressionType::Default, FilterType::Adaptive)
        .write_image(data, width, height, color)
        .map_err(|e| format!("PDF image compression failed: {e}"))?;
    extract_idat(&png)
}

/// PDF 数字文本（固定 8 位小数）。
fn number(value: f64) -> String {
    format!("{value:.8}")
}

/// PDF 文本串：UTF-16BE + BOM 的十六进制写法。
fn pdf_string(text: &str) -> String {
    let mut hex = String::from("<FEFF");
    for unit in text.encode_utf16() {
        hex.push_str(&format!("{unit:04X}"));
    }
    hex.push('>');
    hex
}

/// 简单 PDF 对象写出器：记录每个对象的偏移用于交叉引用表。
struct Writer {
    /// 已写出的字节。
    buf: Vec<u8>,
    /// 各对象偏移；下标 0 是保留的空闲对象。
    offsets: Vec<usize>,
}

impl Writer {
    /// 创建并写入文件头（含二进制标记行）。
    fn new() -> Self {
        let mut buf = b"%PDF-1.7\n".to_vec();
        buf.extend_from_slice(&[b'%', 0xE2, 0xE3, 0xCF, 0xD3, b'\n']);
        Self {
            buf,
            offsets: vec![0],
        }
    }

    /// 下一个对象编号。
    fn next_id(&self) -> usize {
        self.offsets.len()
    }

    /// 写一个普通对象，返回编号。
    fn object(&mut self, body: &str) -> usize {
        let id = self.next_id();
        self.offsets.push(self.buf.len());
        self.buf
            .extend_from_slice(format!("{id} 0 obj\n{body}\nendobj\n").as_bytes());
        id
    }

    /// 写一个流对象，返回编号。
    fn stream(&mut self, dictionary: &str, data: &[u8]) -> usize {
        let id = self.next_id();
        self.offsets.push(self.buf.len());
        self.buf.extend_from_slice(
            format!(
                "{id} 0 obj\n<< {dictionary} /Length {} >>\nstream\n",
                data.len()
            )
            .as_bytes(),
        );
        self.buf.extend_from_slice(data);
        self.buf.extend_from_slice(b"\nendstream\nendobj\n");
        id
    }

    /// 写交叉引用表与文件尾，返回完整文件。
    fn finish(mut self, root: usize, info: usize) -> Vec<u8> {
        let start = self.buf.len();
        let mut tail = format!("xref\n0 {}\n0000000000 65535 f \n", self.offsets.len());
        for offset in &self.offsets[1..] {
            tail.push_str(&format!("{offset:010} 00000 n \n"));
        }
        tail.push_str(&format!(
            "trailer\n<< /Size {} /Root {root} 0 R /Info {info} 0 R >>\nstartxref\n{start}\n%%EOF\n",
            self.offsets.len()
        ));
        self.buf.extend_from_slice(tail.as_bytes());
        self.buf
    }
}

/// 生成单页 PDF。
///
/// # 参数
/// - `width` / `height`：图片像素尺寸。
/// - `rgba`：RGBA 像素（长度须为 `宽 * 高 * 4`）。
/// - `options`：页面、质量、标题与创建时间。
///
/// # 返回
/// PDF 文件字节；参数不合法或压缩失败返回错误说明。
///
/// ```ignore
/// let pdf = build_pdf(2, 2, &[255; 16], &options)?;
/// assert!(pdf.starts_with(b"%PDF-1.7"));
/// ```
pub fn build_pdf(
    width: u32,
    height: u32,
    rgba: &[u8],
    options: &PdfOptions,
) -> Result<Vec<u8>, String> {
    let expected = (width as usize)
        .checked_mul(height as usize)
        .and_then(|n| n.checked_mul(RGBA_BYTES));
    if width == 0
        || height == 0
        || width > MAX_SIDE
        || height > MAX_SIDE
        || expected != Some(rgba.len())
    {
        return Err(format!(
            "invalid PDF image size or buffer: {width}x{height}"
        ));
    }
    let layout = page_layout(width, height, options.page).ok_or("PDF page size is empty")?;
    let unit = (layout.page_width.max(layout.page_height) / MAX_PAGE_POINTS)
        .ceil()
        .max(1.0);
    let (page_w, page_h) = (layout.page_width / unit, layout.page_height / unit);
    let scale = layout.image_width / f64::from(width) / unit;
    let drawn_w = f64::from(width) * scale;
    let drawn_h = f64::from(height) * scale;
    let left = layout.image_x / unit;
    let bottom = page_h - layout.image_y / unit - drawn_h;

    let mut writer = Writer::new();
    let dimensions = format!(" /Width {width} /Height {height} /BitsPerComponent 8");
    let lossless = options.quality >= LOSSLESS_QUALITY;
    let image_id = if lossless {
        let opaque = rgba.chunks_exact(RGBA_BYTES).all(|p| p[3] == u8::MAX);
        let rgb: Vec<u8> = rgba
            .chunks_exact(RGBA_BYTES)
            .flat_map(|p| [p[0], p[1], p[2]])
            .collect();
        let color = png_idat(&rgb, width, height, ExtendedColorType::Rgb8)?;
        let mask = if opaque {
            None
        } else {
            let alpha: Vec<u8> = rgba.chunks_exact(RGBA_BYTES).map(|p| p[3]).collect();
            let data = png_idat(&alpha, width, height, ExtendedColorType::L8)?;
            Some(writer.stream(
                &format!(
                    "/Type /XObject /Subtype /Image /ColorSpace /DeviceGray /Filter /FlateDecode \
                     /DecodeParms << /Predictor 15 /Colors 1 /BitsPerComponent 8 /Columns {width} >>{dimensions}"
                ),
                &data,
            ))
        };
        writer.stream(
            &format!(
                "/Type /XObject /Subtype /Image /ColorSpace /DeviceRGB /Interpolate false /Filter /FlateDecode \
                 /DecodeParms << /Predictor 15 /Colors 3 /BitsPerComponent 8 /Columns {width} >>{dimensions}{}",
                mask.map(|m| format!(" /SMask {m} 0 R")).unwrap_or_default()
            ),
            &color,
        )
    } else {
        let rgb = flatten_on_white(rgba);
        let mut jpeg = Vec::new();
        JpegEncoder::new_with_quality(&mut jpeg, options.quality.clamp(1, LOSSLESS_QUALITY))
            .write_image(&rgb, width, height, ExtendedColorType::Rgb8)
            .map_err(|e| format!("PDF JPEG encode failed: {e}"))?;
        writer.stream(
            &format!(
                "/Type /XObject /Subtype /Image /ColorSpace /DeviceRGB /Interpolate false /Filter /DCTDecode{dimensions}"
            ),
            &jpeg,
        )
    };
    let name = format!("/Im{image_id}");
    let content = format!(
        "1 1 1 rg\n0 0 {} {} re f\nq\n{} 0 0 {} {} {} cm\n{name} Do\nQ\n",
        number(page_w),
        number(page_h),
        number(drawn_w),
        number(drawn_h),
        number(left),
        number(bottom),
    );
    let contents_id = writer.stream("", content.as_bytes());
    let page_id = writer.next_id();
    writer.object(&format!(
        "<< /Type /Page /Parent {} 0 R /MediaBox [0 0 {} {}] /UserUnit {} \
         /Resources << /XObject << {name} {image_id} 0 R >> >> /Contents {contents_id} 0 R >>",
        page_id + 1,
        number(page_w),
        number(page_h),
        number(unit),
    ));
    let pages_id = writer.object(&format!(
        "<< /Type /Pages /Count 1 /Kids [{page_id} 0 R] >>"
    ));
    let root_id = writer.object(&format!("<< /Type /Catalog /Pages {pages_id} 0 R >>"));
    let title: String = options.title.chars().take(MAX_TITLE_CHARS).collect();
    let created = options.created;
    let info_id = writer.object(&format!(
        "<< /Title {} /Creator {} /CreationDate (D:{:04}{:02}{:02}{:02}{:02}{:02}) >>",
        pdf_string(&title),
        pdf_string(PRODUCT_NAME),
        created.year,
        created.month,
        created.day,
        created.hour,
        created.minute,
        created.second,
    ));
    Ok(writer.finish(root_id, info_id))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 固定创建时间。
    fn created() -> LocalDateTime {
        LocalDateTime {
            year: 2026,
            month: 8,
            day: 14,
            hour: 9,
            minute: 7,
            second: 6,
        }
    }

    /// 构造测试参数。
    fn options(page: PdfPageSize, quality: u8) -> PdfOptions {
        PdfOptions {
            page,
            quality,
            title: "标题 t".to_string(),
            created: created(),
        }
    }

    /// 生成带渐变的 RGBA 图。
    fn gradient(w: u32, h: u32, alpha: u8) -> Vec<u8> {
        let mut data = Vec::new();
        for y in 0..h {
            for x in 0..w {
                data.extend_from_slice(&[
                    (x * 7 % 256) as u8,
                    (y * 5 % 256) as u8,
                    ((x ^ y) % 256) as u8,
                    alpha,
                ]);
            }
        }
        data
    }

    /// 在字节串里找子串位置。
    fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
        haystack.windows(needle.len()).position(|w| w == needle)
    }

    /// 页面排版：与图同尺寸、A4 纵 / 横居中。
    #[test]
    fn layout_rules() {
        let l = page_layout(960, 480, PdfPageSize::ImageSize).unwrap();
        assert_eq!((l.page_width, l.page_height), (720.0, 360.0));
        assert_eq!((l.image_x, l.image_y), (0.0, 0.0));
        let p = page_layout(100, 100, PdfPageSize::A4Portrait).unwrap();
        assert!((p.page_width - 595.2756).abs() < 0.01 && (p.page_height - 841.8898).abs() < 0.01);
        assert!((p.image_width - p.page_width).abs() < 1e-9);
        assert!((p.image_y - (p.page_height - p.image_height) / 2.0).abs() < 1e-9);
        let ls = page_layout(100, 100, PdfPageSize::A4Landscape).unwrap();
        assert!(ls.page_width > ls.page_height);
        assert!((ls.image_height - ls.page_height).abs() < 1e-9);
        assert!(page_layout(0, 5, PdfPageSize::ImageSize).is_none());
    }

    /// PDF 头、尾、页面尺寸、标题与交叉引用偏移都正确（无损）。
    #[test]
    fn lossless_pdf_structure() {
        let pdf = build_pdf(
            40,
            20,
            &gradient(40, 20, 255),
            &options(PdfPageSize::ImageSize, 100),
        )
        .unwrap();
        assert!(pdf.starts_with(b"%PDF-1.7\n"));
        assert!(pdf.ends_with(b"%%EOF\n"));
        // 40x20 像素 -> 30x15 点
        assert!(find(&pdf, b"/MediaBox [0 0 30.00000000 15.00000000]").is_some());
        assert!(find(&pdf, b"/FlateDecode").is_some() && find(&pdf, b"/DCTDecode").is_none());
        assert!(find(&pdf, b"/Predictor 15").is_some());
        assert!(find(&pdf, b"/SMask").is_none(), "不透明图不应带 SMask");
        // 标题 UTF-16BE：'标' = 6807, 题 = 9898, 空格 = 0020, 't' = 0074
        assert!(find(&pdf, b"/Title <FEFF6807989800200074>").is_some());
        // xref 偏移指向对应对象
        let start = String::from_utf8_lossy(&pdf[find(&pdf, b"startxref\n").unwrap() + 10..])
            .lines()
            .next()
            .unwrap()
            .parse::<usize>()
            .unwrap();
        assert!(pdf[start..].starts_with(b"xref\n0 "));
        let table = String::from_utf8_lossy(&pdf[start..]).to_string();
        let entries: Vec<&str> = table
            .lines()
            .skip(3)
            .take_while(|l| l.ends_with(" n "))
            .collect();
        assert!(!entries.is_empty());
        for (index, entry) in entries.iter().enumerate() {
            let offset: usize = entry[..10].parse().unwrap();
            let expect = format!("{} 0 obj", index + 1);
            assert!(
                pdf[offset..].starts_with(expect.as_bytes()),
                "对象 {} 偏移错误",
                index + 1
            );
        }
    }

    /// 有透明度时输出 SMask；A4 页面写入 A4 点数。
    #[test]
    fn alpha_adds_smask_and_a4_media_box() {
        let pdf = build_pdf(
            8,
            8,
            &gradient(8, 8, 128),
            &options(PdfPageSize::A4Portrait, 100),
        )
        .unwrap();
        assert!(find(&pdf, b"/SMask").is_some());
        assert!(find(&pdf, b"/MediaBox [0 0 595.27559055 841.88976378]").is_some());
        let landscape = build_pdf(
            8,
            8,
            &gradient(8, 8, 255),
            &options(PdfPageSize::A4Landscape, 100),
        )
        .unwrap();
        assert!(find(&landscape, b"/MediaBox [0 0 841.88976378 595.27559055]").is_some());
    }

    /// 质量小于 100 走 JPEG（DCT），且质量越低体积越小。
    #[test]
    fn lossy_quality_uses_jpeg_and_changes_size() {
        let data = gradient(96, 96, 255);
        let low = build_pdf(96, 96, &data, &options(PdfPageSize::ImageSize, 10)).unwrap();
        let high = build_pdf(96, 96, &data, &options(PdfPageSize::ImageSize, 95)).unwrap();
        assert!(find(&low, b"/DCTDecode").is_some() && find(&low, b"/FlateDecode").is_none());
        assert!(
            low.len() < high.len(),
            "质量参数应影响体积: {} vs {}",
            low.len(),
            high.len()
        );
    }

    /// 超长标题被截断到上限；缓冲不符或尺寸为 0 报错。
    #[test]
    fn title_truncation_and_errors() {
        let mut opts = options(PdfPageSize::ImageSize, 100);
        opts.title = "a".repeat(MAX_TITLE_CHARS + 50);
        let pdf = build_pdf(2, 2, &gradient(2, 2, 255), &opts).unwrap();
        let text = String::from_utf8_lossy(&pdf);
        let title = text
            .split("/Title <")
            .nth(1)
            .unwrap()
            .split('>')
            .next()
            .unwrap();
        assert_eq!(title.len(), 4 + MAX_TITLE_CHARS * 4);
        assert!(build_pdf(2, 2, &[0; 4], &opts).is_err());
        assert!(build_pdf(0, 2, &[], &opts).is_err());
    }

    /// 取 IDAT：拼接多块并拒绝损坏数据。
    #[test]
    fn idat_extraction() {
        let mut png = Vec::new();
        PngEncoder::new(&mut png)
            .write_image(&[1, 2, 3], 1, 1, ExtendedColorType::Rgb8)
            .unwrap();
        assert!(!extract_idat(&png).unwrap().is_empty());
        assert!(extract_idat(&png[..20]).is_err());
        assert!(extract_idat(b"not a png at all, really").is_err());
    }

    /// 白底合成：全透明变白，不透明不变。
    #[test]
    fn flatten_blends_on_white() {
        assert_eq!(flatten_on_white(&[10, 20, 30, 0]), vec![255, 255, 255]);
        assert_eq!(flatten_on_white(&[10, 20, 30, 255]), vec![10, 20, 30]);
    }
}
