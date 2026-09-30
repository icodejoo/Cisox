//! 文本光栅化：用系统 GDI 把（多行）文本绘成 8 位覆盖率位图，并提供尺寸测量。
//!
//! 标注文字的最终合成需要在离屏缓冲里画字，仓库内没有字体光栅化库，
//! 这里直接复用系统 GDI（灰度抗锯齿），不引入新依赖。非 Windows 平台返回错误。

/// 默认字体族（含中日韩字形）。
pub const DEFAULT_FONT_FAMILY: &str = "Microsoft YaHei UI";
/// 字号下限（像素），防止 0 或负值创建字体失败。
pub const MIN_FONT_PX: f32 = 4.0;
/// 字号上限（像素），防止异常值造成巨型位图。
pub const MAX_FONT_PX: f32 = 512.0;
/// 单个位图边长上限（像素）。
pub const MAX_BITMAP_SIDE: u32 = 8192;

/// 文本测量结果（像素）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TextMetrics {
    /// 宽度。
    pub width: u32,
    /// 高度。
    pub height: u32,
}

/// 文本覆盖率位图：每像素 1 字节（0 = 无墨迹，255 = 完全覆盖）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TextBitmap {
    /// 宽度。
    pub width: u32,
    /// 高度。
    pub height: u32,
    /// 行紧密排列的覆盖率，长度 `width * height`。
    pub coverage: Vec<u8>,
}

/// 把字号夹到合法范围（非有限值回落为最小值）。
///
/// # 参数
/// - `font_px`：请求的字号（像素）。
///
/// # 返回
/// 合法字号。
///
/// ```
/// use snow_platform::text_raster::{clamp_font_px, MAX_FONT_PX, MIN_FONT_PX};
/// assert_eq!(clamp_font_px(f32::NAN), MIN_FONT_PX);
/// assert_eq!(clamp_font_px(9999.0), MAX_FONT_PX);
/// assert_eq!(clamp_font_px(20.0), 20.0);
/// ```
pub fn clamp_font_px(font_px: f32) -> f32 {
    if font_px.is_finite() {
        font_px.clamp(MIN_FONT_PX, MAX_FONT_PX)
    } else {
        MIN_FONT_PX
    }
}

/// 测量文本的排版尺寸（`\n` 分行，不自动折行）。
///
/// # 参数
/// - `text`：文本；空串返回单行高度、宽度 0。
/// - `font_family`：字体族名。
/// - `font_px`：字号（像素）。
/// - `bold`：是否加粗。
///
/// # 返回
/// 尺寸；GDI 调用失败返回错误说明。
///
/// ```no_run
/// let m = snow_platform::text_raster::measure_text("你好", "Microsoft YaHei UI", 24.0, false).unwrap();
/// assert!(m.width > 0 && m.height > 0);
/// ```
pub fn measure_text(
    text: &str,
    font_family: &str,
    font_px: f32,
    bold: bool,
) -> Result<TextMetrics, String> {
    imp::measure(text, font_family, clamp_font_px(font_px), bold)
}

/// 把文本绘成覆盖率位图（尺寸等于 [`measure_text`] 的结果）。
///
/// # 参数
/// - `text`：文本；空白文本返回全 0 位图。
/// - `font_family`：字体族名。
/// - `font_px`：字号（像素）。
/// - `bold`：是否加粗。
///
/// # 返回
/// 覆盖率位图；失败返回错误说明。
///
/// ```no_run
/// let bmp = snow_platform::text_raster::rasterize_text("Hi", "Microsoft YaHei UI", 24.0, false).unwrap();
/// assert!(bmp.coverage.iter().any(|&c| c > 0));
/// ```
pub fn rasterize_text(
    text: &str,
    font_family: &str,
    font_px: f32,
    bold: bool,
) -> Result<TextBitmap, String> {
    imp::rasterize(text, font_family, clamp_font_px(font_px), bold)
}

#[cfg(windows)]
mod imp {
    use super::{MAX_BITMAP_SIDE, TextBitmap, TextMetrics};
    use windows::Win32::Foundation::{COLORREF, RECT};
    use windows::Win32::Graphics::Gdi::{
        ANTIALIASED_QUALITY, BI_RGB, BITMAPINFO, BITMAPINFOHEADER, BLACKNESS, CLIP_DEFAULT_PRECIS,
        CreateCompatibleBitmap, CreateCompatibleDC, CreateFontW, DEFAULT_CHARSET, DIB_RGB_COLORS,
        DT_CALCRECT, DT_EXPANDTABS, DT_LEFT, DT_NOPREFIX, DT_TOP, DeleteDC, DeleteObject,
        DrawTextW, FF_DONTCARE, FW_BOLD, FW_NORMAL, GetDC, GetDIBits, HDC, HFONT, OUT_DEFAULT_PRECIS,
        PatBlt, ReleaseDC, SelectObject, SetBkMode, SetTextColor, TRANSPARENT,
    };
    use windows::core::PCWSTR;

    /// 每像素字节数。
    const BYTES_PER_PIXEL: usize = 4;
    /// 文本绘制标志：左上对齐、展开制表符、不解析 `&`。
    const DRAW_FLAGS: windows::Win32::Graphics::Gdi::DRAW_TEXT_FORMAT =
        windows::Win32::Graphics::Gdi::DRAW_TEXT_FORMAT(
            DT_LEFT.0 | DT_TOP.0 | DT_NOPREFIX.0 | DT_EXPANDTABS.0,
        );

    /// 转成以 0 结尾的 UTF-16。
    fn wide(text: &str) -> Vec<u16> {
        text.encode_utf16().chain(std::iter::once(0)).collect()
    }

    /// 创建 GDI 字体。
    unsafe fn make_font(family: &str, px: f32, bold: bool) -> Result<HFONT, String> {
        let name = wide(family);
        // SAFETY: name 以 0 结尾且在调用期间存活。
        let font = unsafe {
            CreateFontW(
                -(px.round() as i32),
                0,
                0,
                0,
                if bold { FW_BOLD.0 } else { FW_NORMAL.0 } as i32,
                0,
                0,
                0,
                DEFAULT_CHARSET,
                OUT_DEFAULT_PRECIS,
                CLIP_DEFAULT_PRECIS,
                ANTIALIASED_QUALITY,
                FF_DONTCARE.0 as u32,
                PCWSTR(name.as_ptr()),
            )
        };
        if font.0.is_null() {
            Err("创建字体失败".into())
        } else {
            Ok(font)
        }
    }

    /// 在内存 DC 上测量文本包围尺寸。
    unsafe fn measure_with(hdc: HDC, text: &[u16]) -> (i32, i32) {
        let mut rect = RECT::default();
        let mut buf = text.to_vec();
        // SAFETY: buf 为有效 UTF-16 缓冲，DT_CALCRECT 只写 rect。
        unsafe { DrawTextW(hdc, &mut buf, &mut rect, DRAW_FLAGS | DT_CALCRECT) };
        (rect.right - rect.left, rect.bottom - rect.top)
    }

    /// 测量文本（含字体创建与释放）。
    pub fn measure(text: &str, family: &str, px: f32, bold: bool) -> Result<TextMetrics, String> {
        let probe: &str = if text.is_empty() { " " } else { text };
        // SAFETY: 全部 GDI 对象在本函数内创建并释放。
        unsafe {
            let screen = GetDC(None);
            if screen.0.is_null() {
                return Err("获取屏幕 DC 失败".into());
            }
            let hdc = CreateCompatibleDC(Some(screen));
            let font = make_font(family, px, bold);
            let result = match (&font, hdc.0.is_null()) {
                (Ok(font), false) => {
                    let old = SelectObject(hdc, (*font).into());
                    let units: Vec<u16> = probe.encode_utf16().collect();
                    let (w, h) = measure_with(hdc, &units);
                    SelectObject(hdc, old);
                    let width = if text.is_empty() { 0 } else { w.max(0) as u32 };
                    Ok(TextMetrics {
                        width,
                        height: h.max(1) as u32,
                    })
                }
                (Err(e), _) => Err(e.clone()),
                _ => Err("创建内存 DC 失败".into()),
            };
            if let Ok(font) = font {
                let _ = DeleteObject(font.into());
            }
            if !hdc.0.is_null() {
                let _ = DeleteDC(hdc);
            }
            let _ = ReleaseDC(None, screen);
            result
        }
    }

    /// 光栅化文本为覆盖率位图。
    pub fn rasterize(text: &str, family: &str, px: f32, bold: bool) -> Result<TextBitmap, String> {
        let metrics = measure(text, family, px, bold)?;
        let (w, h) = (metrics.width, metrics.height);
        if w == 0 || text.trim().is_empty() {
            return Ok(TextBitmap {
                width: w.max(1),
                height: h,
                coverage: vec![0; (w.max(1) * h) as usize],
            });
        }
        if w > MAX_BITMAP_SIDE || h > MAX_BITMAP_SIDE {
            return Err(format!("文本位图过大: {w}x{h}"));
        }
        // SAFETY: 全部 GDI 对象在本函数内创建并释放，缓冲长度与 GetDIBits 请求一致。
        unsafe {
            let screen = GetDC(None);
            if screen.0.is_null() {
                return Err("获取屏幕 DC 失败".into());
            }
            let hdc = CreateCompatibleDC(Some(screen));
            let bitmap = CreateCompatibleBitmap(screen, w as i32, h as i32);
            let font = make_font(family, px, bold);
            let mut out: Result<TextBitmap, String> = Err("创建 GDI 对象失败".into());
            if !hdc.0.is_null()
                && !bitmap.0.is_null()
                && let Ok(font_handle) = &font
            {
                let old_bitmap = SelectObject(hdc, bitmap.into());
                let old_font = SelectObject(hdc, (*font_handle).into());
                let _ = PatBlt(hdc, 0, 0, w as i32, h as i32, BLACKNESS);
                SetBkMode(hdc, TRANSPARENT);
                SetTextColor(hdc, COLORREF(0x00FF_FFFF));
                let mut buf: Vec<u16> = text.encode_utf16().collect();
                let mut rect = RECT {
                    left: 0,
                    top: 0,
                    right: w as i32,
                    bottom: h as i32,
                };
                DrawTextW(hdc, &mut buf, &mut rect, DRAW_FLAGS);
                SelectObject(hdc, old_font);
                SelectObject(hdc, old_bitmap);

                let mut bmi = BITMAPINFO {
                    bmiHeader: BITMAPINFOHEADER {
                        biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                        biWidth: w as i32,
                        biHeight: -(h as i32),
                        biPlanes: 1,
                        biBitCount: 32,
                        biCompression: BI_RGB.0,
                        ..Default::default()
                    },
                    ..Default::default()
                };
                let mut pixels = vec![0u8; w as usize * h as usize * BYTES_PER_PIXEL];
                let lines = GetDIBits(
                    screen,
                    bitmap,
                    0,
                    h,
                    Some(pixels.as_mut_ptr() as _),
                    &mut bmi,
                    DIB_RGB_COLORS,
                );
                if lines == h as i32 {
                    // 灰度抗锯齿下三个通道相同，取绿色通道作为覆盖率
                    let coverage = pixels
                        .chunks_exact(BYTES_PER_PIXEL)
                        .map(|p| p[1])
                        .collect();
                    out = Ok(TextBitmap {
                        width: w,
                        height: h,
                        coverage,
                    });
                } else {
                    out = Err("读取文本位图失败".into());
                }
            }
            if let Ok(font_handle) = font {
                let _ = DeleteObject(font_handle.into());
            }
            if !bitmap.0.is_null() {
                let _ = DeleteObject(bitmap.into());
            }
            if !hdc.0.is_null() {
                let _ = DeleteDC(hdc);
            }
            let _ = ReleaseDC(None, screen);
            out
        }
    }
}

#[cfg(not(windows))]
mod imp {
    use super::{TextBitmap, TextMetrics};

    /// 非 Windows：不支持。
    pub fn measure(_: &str, _: &str, _: f32, _: bool) -> Result<TextMetrics, String> {
        Err("当前平台不支持文本光栅化".into())
    }

    /// 非 Windows：不支持。
    pub fn rasterize(_: &str, _: &str, _: f32, _: bool) -> Result<TextBitmap, String> {
        Err("当前平台不支持文本光栅化".into())
    }
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;

    /// 字号被夹到合法范围。
    #[test]
    fn font_size_is_clamped() {
        assert_eq!(clamp_font_px(-3.0), MIN_FONT_PX);
        assert_eq!(clamp_font_px(f32::INFINITY), MIN_FONT_PX);
        assert_eq!(clamp_font_px(1000.0), MAX_FONT_PX);
    }

    /// 有文字时尺寸非零，位图含墨迹且长度自洽。
    #[test]
    fn rasterizes_visible_ink() {
        let m = measure_text("Snow", DEFAULT_FONT_FAMILY, 24.0, false).unwrap();
        assert!(m.width > 20 && m.height >= 24, "尺寸异常: {m:?}");
        let bmp = rasterize_text("Snow", DEFAULT_FONT_FAMILY, 24.0, false).unwrap();
        assert_eq!((bmp.width, bmp.height), (m.width, m.height));
        assert_eq!(bmp.coverage.len(), (m.width * m.height) as usize);
        assert!(bmp.coverage.iter().filter(|&&c| c > 128).count() > 30);
    }

    /// 多行文本高度大于单行；中文有墨迹。
    #[test]
    fn multiline_and_cjk() {
        let one = measure_text("你好", DEFAULT_FONT_FAMILY, 20.0, false).unwrap();
        let two = measure_text("你好\n世界", DEFAULT_FONT_FAMILY, 20.0, false).unwrap();
        assert!(two.height >= one.height * 2 - 2, "{one:?} {two:?}");
        let bmp = rasterize_text("你好", DEFAULT_FONT_FAMILY, 20.0, false).unwrap();
        assert!(bmp.coverage.iter().any(|&c| c > 128));
    }

    /// 空串宽度为 0，空白文本位图全 0，不报错。
    #[test]
    fn empty_and_blank_text() {
        let m = measure_text("", DEFAULT_FONT_FAMILY, 20.0, false).unwrap();
        assert_eq!(m.width, 0);
        let bmp = rasterize_text("   ", DEFAULT_FONT_FAMILY, 20.0, false).unwrap();
        assert!(bmp.coverage.iter().all(|&c| c == 0));
    }
}
