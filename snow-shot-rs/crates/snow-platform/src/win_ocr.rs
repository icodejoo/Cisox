//! Windows 系统 OCR（`Windows.Media.Ocr`）封装：可用性探测与 RGBA 图识别。
//!
//! 系统 OCR 不返回置信度，只给行文本与词框；本模块把行文本规整（去掉 CJK 字间空格），
//! 行框取词框并集。WinRT 调用都放在独立的短命 MTA 线程里完成，调用方线程不会被初始化套间。
//! 非 Windows 平台所有入口返回“不支持”。

/// 一行识别结果。
#[derive(Debug, Clone, PartialEq)]
pub struct WinOcrLine {
    /// 行文本（已去掉 CJK 字间空格）。
    pub text: String,
    /// 行外接矩形 `[x, y, 宽, 高]`，提交图像的像素坐标。
    pub rect: [f32; 4],
}

/// 一次识别的输出。
#[derive(Debug, Clone, PartialEq)]
pub struct WinOcrOutput {
    /// 按系统返回顺序排列的行。
    pub lines: Vec<WinOcrLine>,
    /// 实际使用的识别语言标签（如 `zh-Hans-CN`）。
    pub language: String,
}

/// 系统 OCR 的可用性。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WinOcrStatus {
    /// 可用，附系统已装的 OCR 语言标签。
    Ready {
        /// 已安装的识别语言。
        languages: Vec<String>,
    },
    /// 没有可用于当前用户语言的 OCR 语言包。
    NoLanguagePack,
    /// 系统接口调用失败（附原因）。
    EngineFailed(String),
    /// 当前平台没有系统 OCR。
    UnsupportedPlatform,
}

/// 识别失败原因。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WinOcrError {
    /// 当前平台没有系统 OCR。
    UnsupportedPlatform,
    /// 没有可用于所选语言的 OCR 语言包。
    NoLanguagePack,
    /// 指定语言未安装 OCR 支持（附语言标签）。
    LanguageNotSupported(String),
    /// 图像边长超过系统上限。
    ImageTooLarge {
        /// 系统允许的最大边长。
        max: u32,
    },
    /// 图像尺寸与像素长度不符。
    InvalidImage(String),
    /// 其它系统调用失败。
    Failed(String),
}

impl std::fmt::Display for WinOcrError {
    /// 输出面向日志的英文说明。
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UnsupportedPlatform => write!(f, "system OCR is not supported on this platform"),
            Self::NoLanguagePack => write!(
                f,
                "no OCR language pack is installed for the user languages"
            ),
            Self::LanguageNotSupported(tag) => write!(f, "OCR is not available for language {tag}"),
            Self::ImageTooLarge { max } => {
                write!(f, "image side exceeds the system OCR limit of {max}px")
            }
            Self::InvalidImage(why) => write!(f, "invalid image: {why}"),
            Self::Failed(why) => write!(f, "system OCR call failed: {why}"),
        }
    }
}

impl std::error::Error for WinOcrError {}

/// 探测系统 OCR 是否可用（毫秒级；在短命线程里调用 WinRT）。
///
/// # 返回
/// 可用性；非 Windows 返回 [`WinOcrStatus::UnsupportedPlatform`]。
///
/// # 示例
/// ```ignore
/// if let WinOcrStatus::Ready { languages } = snow_platform::win_ocr::probe() {
///     println!("{languages:?}");
/// }
/// ```
pub fn probe() -> WinOcrStatus {
    imp::probe()
}

/// 系统允许的最大图像边长（像素）。
///
/// # 返回
/// 上限；非 Windows 或查询失败返回 `None`。
pub fn max_image_dimension() -> Option<u32> {
    imp::max_image_dimension()
}

/// 识别一张 RGBA 图（阻塞，须在后台线程调用）。
///
/// # 参数
/// - `width` / `height`：图像尺寸，边长不得超过 [`max_image_dimension`]。
/// - `rgba`：紧凑 RGBA 像素，长度须为 `宽 * 高 * 4`。
/// - `language`：BCP-47 语言标签；`None` 表示按用户档语言创建引擎。
///
/// # 返回
/// 行文本与行框；图中无文字时 `lines` 为空，不是错误。
///
/// # 示例
/// ```ignore
/// let out = snow_platform::win_ocr::recognize(w, h, &rgba, None)?;
/// for line in &out.lines {
///     println!("{}", line.text);
/// }
/// ```
pub fn recognize(
    width: u32,
    height: u32,
    rgba: &[u8],
    language: Option<&str>,
) -> Result<WinOcrOutput, WinOcrError> {
    check_dimensions(width, height, rgba.len())?;
    imp::recognize(width, height, rgba, language)
}

/// 校验尺寸与像素长度一致且非空。
fn check_dimensions(width: u32, height: u32, len: usize) -> Result<(), WinOcrError> {
    let expected = (width as usize)
        .checked_mul(height as usize)
        .and_then(|p| p.checked_mul(4));
    if width == 0 || height == 0 || expected != Some(len) {
        return Err(WinOcrError::InvalidImage(format!(
            "{width}x{height} does not match pixel length {len}"
        )));
    }
    Ok(())
}

/// 把 RGBA 转成系统要求的 BGRA（交换 R / B）。
///
/// # 参数
/// - `rgba`：RGBA 像素，长度应为 4 的倍数（多余尾部字节原样保留）。
pub fn rgba_to_bgra(rgba: &[u8]) -> Vec<u8> {
    let mut out = rgba.to_vec();
    for px in out.chunks_exact_mut(4) {
        px.swap(0, 2);
    }
    out
}

/// 字符是否属于 CJK 类（表意文字、假名、谚文、CJK 标点与全角形式）。
fn is_cjk(c: char) -> bool {
    matches!(u32::from(c),
        0x2E80..=0x9FFF | 0xAC00..=0xD7AF | 0xF900..=0xFAFF | 0xFF00..=0xFFEF | 0x20000..=0x2FFFF)
}

/// 去掉 CJK 字符之间的空格（系统引擎会把中文逐字用空格分开），保留拉丁词间空格与 CJK/拉丁边界处的空格。
///
/// # 参数
/// - `text`：系统返回的行文本。
///
/// # 示例
/// ```
/// use snow_platform::win_ocr::strip_cjk_spaces;
/// assert_eq!(strip_cjk_spaces("你 好 Hello World 世 界"), "你好 Hello World 世界");
/// ```
pub fn strip_cjk_spaces(text: &str) -> String {
    let chars: Vec<char> = text.chars().collect();
    let mut out = String::with_capacity(text.len());
    let mut i = 0;
    while i < chars.len() {
        if chars[i] == ' ' {
            let start = i;
            while i < chars.len() && chars[i] == ' ' {
                i += 1;
            }
            let prev_cjk = start > 0 && is_cjk(chars[start - 1]);
            let next_cjk = i < chars.len() && is_cjk(chars[i]);
            if !(prev_cjk && next_cjk) {
                out.extend(std::iter::repeat_n(' ', i - start));
            }
        } else {
            out.push(chars[i]);
            i += 1;
        }
    }
    out
}

/// 若图像边长超过 `max`，返回等比缩小后的尺寸；未超限返回 `None`。
///
/// # 参数
/// - `width` / `height`：原图尺寸。
/// - `max`：系统允许的最大边长。
///
/// # 示例
/// ```
/// use snow_platform::win_ocr::fit_dimension;
/// assert_eq!(fit_dimension(8000, 4000, 10000), None);
/// assert_eq!(fit_dimension(20000, 10000, 10000), Some((10000, 5000)));
/// ```
pub fn fit_dimension(width: u32, height: u32, max: u32) -> Option<(u32, u32)> {
    let longest = width.max(height);
    if longest <= max {
        return None;
    }
    let scale = f64::from(max) / f64::from(longest);
    let scaled = |v: u32| ((f64::from(v) * scale).floor() as u32).clamp(1, max);
    Some((scaled(width), scaled(height)))
}

/// 取一组矩形 `[x, y, 宽, 高]` 的并集；为空返回 `None`。
pub fn union_rect(rects: &[[f32; 4]]) -> Option<[f32; 4]> {
    let first = rects.first()?;
    let (mut x0, mut y0, mut x1, mut y1) =
        (first[0], first[1], first[0] + first[2], first[1] + first[3]);
    for r in &rects[1..] {
        x0 = x0.min(r[0]);
        y0 = y0.min(r[1]);
        x1 = x1.max(r[0] + r[2]);
        y1 = y1.max(r[1] + r[3]);
    }
    Some([x0, y0, x1 - x0, y1 - y0])
}

#[cfg(windows)]
mod imp {
    use super::{
        WinOcrError, WinOcrLine, WinOcrOutput, WinOcrStatus, rgba_to_bgra, strip_cjk_spaces,
        union_rect,
    };
    use std::sync::OnceLock;
    use windows::Globalization::Language;
    use windows::Graphics::Imaging::{BitmapPixelFormat, SoftwareBitmap};
    use windows::Media::Ocr::OcrEngine;
    use windows::Storage::Streams::DataWriter;
    use windows::Win32::System::Com::CoIncrementMTAUsage;
    use windows::core::HSTRING;

    /// 让进程拥有常驻的隐式 MTA（只做一次）；之后未初始化 COM 的新线程自动属于 MTA。
    ///
    /// 不能在每个短命线程里 `RoInitialize / RoUninitialize`：最后一个 MTA 引用释放后套间被拆除，
    /// windows crate 缓存的类工厂指针失效，下一次调用会访问违规。
    fn ensure_mta() -> Result<(), String> {
        static MTA: OnceLock<Result<(), String>> = OnceLock::new();
        MTA.get_or_init(|| {
            // SAFETY: 该调用只递增进程的 MTA 使用计数，不涉及指针；有意不递减，让 MTA 活到进程结束。
            unsafe { CoIncrementMTAUsage() }
                .map(|_cookie| ())
                .map_err(|e| e.to_string())
        })
        .clone()
    }

    /// 在专用线程里执行 `f`：该线程从未初始化 COM，因此属于隐式 MTA，不影响调用线程的套间。
    fn on_winrt_thread<T: Send>(f: impl FnOnce() -> T + Send) -> Result<T, String> {
        ensure_mta()?;
        std::thread::scope(|scope| {
            let handle = std::thread::Builder::new()
                .name("snow-win-ocr".into())
                .spawn_scoped(scope, f)
                .map_err(|e| e.to_string())?;
            handle
                .join()
                .map_err(|_| "system OCR thread panicked".to_string())
        })
    }

    /// 探测可用性。
    pub fn probe() -> WinOcrStatus {
        match on_winrt_thread(probe_inner) {
            Ok(status) => status,
            Err(why) => WinOcrStatus::EngineFailed(why),
        }
    }

    /// 线程内的探测实现。
    fn probe_inner() -> WinOcrStatus {
        let langs = match OcrEngine::AvailableRecognizerLanguages() {
            Ok(v) => v,
            Err(e) => return WinOcrStatus::EngineFailed(e.to_string()),
        };
        let languages: Vec<String> = langs
            .into_iter()
            .filter_map(|l| l.LanguageTag().ok())
            .map(|t| t.to_string())
            .collect();
        if languages.is_empty() {
            return WinOcrStatus::NoLanguagePack;
        }
        // 已装语言包但引擎创建失败，通常是用户档语言里没有任何一个带 OCR 支持
        match OcrEngine::TryCreateFromUserProfileLanguages() {
            Ok(_) => WinOcrStatus::Ready { languages },
            Err(_) => WinOcrStatus::NoLanguagePack,
        }
    }

    /// 查询最大边长。
    pub fn max_image_dimension() -> Option<u32> {
        on_winrt_thread(|| OcrEngine::MaxImageDimension().ok())
            .ok()
            .flatten()
    }

    /// 识别（线程包装）。
    pub fn recognize(
        width: u32,
        height: u32,
        rgba: &[u8],
        language: Option<&str>,
    ) -> Result<WinOcrOutput, WinOcrError> {
        on_winrt_thread(|| recognize_inner(width, height, rgba, language))
            .map_err(WinOcrError::Failed)?
    }

    /// 把任意系统错误包成 [`WinOcrError::Failed`]。
    fn failed(e: windows::core::Error) -> WinOcrError {
        WinOcrError::Failed(e.to_string())
    }

    /// 线程内的识别实现。
    fn recognize_inner(
        width: u32,
        height: u32,
        rgba: &[u8],
        language: Option<&str>,
    ) -> Result<WinOcrOutput, WinOcrError> {
        let max = OcrEngine::MaxImageDimension().map_err(failed)?;
        if width.max(height) > max {
            return Err(WinOcrError::ImageTooLarge { max });
        }
        let engine = match language {
            Some(tag) => {
                let lang = Language::CreateLanguage(&HSTRING::from(tag)).map_err(failed)?;
                if !OcrEngine::IsLanguageSupported(&lang).map_err(failed)? {
                    return Err(WinOcrError::LanguageNotSupported(tag.to_string()));
                }
                OcrEngine::TryCreateFromLanguage(&lang)
            }
            None => OcrEngine::TryCreateFromUserProfileLanguages(),
        }
        .map_err(|_| WinOcrError::NoLanguagePack)?;

        let writer = DataWriter::new().map_err(failed)?;
        writer.WriteBytes(&rgba_to_bgra(rgba)).map_err(failed)?;
        let buffer = writer.DetachBuffer().map_err(failed)?;
        let bitmap = SoftwareBitmap::CreateCopyFromBuffer(
            &buffer,
            BitmapPixelFormat::Bgra8,
            width as i32,
            height as i32,
        )
        .map_err(failed)?;
        let result = engine
            .RecognizeAsync(&bitmap)
            .map_err(failed)?
            .join()
            .map_err(failed)?;

        let mut lines = Vec::new();
        for line in result.Lines().map_err(failed)? {
            let text = strip_cjk_spaces(&line.Text().map_err(failed)?.to_string());
            let rects: Vec<[f32; 4]> = line
                .Words()
                .map_err(failed)?
                .into_iter()
                .filter_map(|w| w.BoundingRect().ok())
                .map(|r| [r.X, r.Y, r.Width, r.Height])
                .collect();
            if let Some(rect) = union_rect(&rects) {
                lines.push(WinOcrLine { text, rect });
            }
        }
        let language = engine
            .RecognizerLanguage()
            .and_then(|l| l.LanguageTag())
            .map(|t| t.to_string())
            .unwrap_or_default();
        Ok(WinOcrOutput { lines, language })
    }
}

#[cfg(not(windows))]
mod imp {
    use super::{WinOcrError, WinOcrOutput, WinOcrStatus};

    /// 非 Windows：不支持。
    pub fn probe() -> WinOcrStatus {
        WinOcrStatus::UnsupportedPlatform
    }

    /// 非 Windows：无上限信息。
    pub fn max_image_dimension() -> Option<u32> {
        None
    }

    /// 非 Windows：不支持。
    pub fn recognize(
        _: u32,
        _: u32,
        _: &[u8],
        _: Option<&str>,
    ) -> Result<WinOcrOutput, WinOcrError> {
        Err(WinOcrError::UnsupportedPlatform)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// RGBA 转 BGRA 只交换 R/B。
    #[test]
    fn bgra_swaps_red_and_blue() {
        assert_eq!(
            rgba_to_bgra(&[1, 2, 3, 4, 5, 6, 7, 8]),
            vec![3, 2, 1, 4, 7, 6, 5, 8]
        );
        assert!(rgba_to_bgra(&[]).is_empty());
    }

    /// CJK 字间空格被去掉，拉丁词间与中英边界的空格保留。
    #[test]
    fn cjk_spaces_are_stripped_but_latin_kept() {
        assert_eq!(strip_cjk_spaces("你 好 ， 世 界"), "你好，世界");
        assert_eq!(strip_cjk_spaces("Hello  World"), "Hello  World");
        assert_eq!(strip_cjk_spaces("测 试 abc def 完 成"), "测试 abc def 完成");
        assert_eq!(strip_cjk_spaces("日本 語"), "日本語");
        assert_eq!(strip_cjk_spaces(""), "");
        assert_eq!(strip_cjk_spaces(" a "), " a ");
    }

    /// 超限时等比缩小，未超限返回 None，最小边不为 0。
    #[test]
    fn fit_dimension_scales_longest_side() {
        assert_eq!(fit_dimension(100, 100, 100), None);
        assert_eq!(fit_dimension(200, 100, 100), Some((100, 50)));
        assert_eq!(fit_dimension(100, 400, 100), Some((25, 100)));
        assert_eq!(fit_dimension(100_000, 1, 100), Some((100, 1)));
    }

    /// 矩形并集覆盖全部输入；空输入为 None。
    #[test]
    fn union_rect_covers_all() {
        assert_eq!(union_rect(&[]), None);
        assert_eq!(
            union_rect(&[[10.0, 10.0, 5.0, 5.0]]),
            Some([10.0, 10.0, 5.0, 5.0])
        );
        assert_eq!(
            union_rect(&[[10.0, 20.0, 5.0, 5.0], [30.0, 18.0, 10.0, 4.0]]),
            Some([10.0, 18.0, 30.0, 7.0])
        );
    }

    /// 尺寸校验：零尺寸与长度不符都是 InvalidImage，且不触碰系统接口。
    #[test]
    fn recognize_rejects_bad_dimensions() {
        assert!(matches!(
            recognize(0, 1, &[], None),
            Err(WinOcrError::InvalidImage(_))
        ));
        assert!(matches!(
            recognize(2, 2, &[0; 3], None),
            Err(WinOcrError::InvalidImage(_))
        ));
    }

    /// 错误文案不为空且带关键信息。
    #[test]
    fn error_display_mentions_details() {
        assert!(
            WinOcrError::LanguageNotSupported("fr-FR".into())
                .to_string()
                .contains("fr-FR")
        );
        assert!(
            WinOcrError::ImageTooLarge { max: 10000 }
                .to_string()
                .contains("10000")
        );
    }

    /// 非 Windows 平台一律不支持。
    #[cfg(not(windows))]
    #[test]
    fn unsupported_off_windows() {
        assert_eq!(probe(), WinOcrStatus::UnsupportedPlatform);
        assert_eq!(max_image_dimension(), None);
        assert_eq!(
            recognize(1, 1, &[0; 4], None),
            Err(WinOcrError::UnsupportedPlatform)
        );
    }

    /// 真机冒烟：渲染一行英文，系统 OCR 应识别出关键词。
    /// 运行：`cargo test -p snow-platform win_ocr -- --ignored --nocapture`（需 Windows 10+ 且装有 OCR 语言包）。
    #[cfg(windows)]
    #[test]
    #[ignore = "需要真实 Windows 系统 OCR 与语言包"]
    fn real_system_ocr_reads_rendered_text() {
        use crate::text_raster::{DEFAULT_FONT_FAMILY, rasterize_text};
        println!("probe = {:?}", probe());
        let bmp = rasterize_text("Hello Snow Shot 12345", DEFAULT_FONT_FAMILY, 32.0, false)
            .expect("光栅化");
        let (margin, w, h) = (20u32, bmp.width + 40, bmp.height + 40);
        let mut rgba = vec![255u8; (w * h * 4) as usize];
        for y in 0..bmp.height {
            for x in 0..bmp.width {
                let v = 255 - bmp.coverage[(y * bmp.width + x) as usize];
                let at = (((margin + y) * w + margin + x) * 4) as usize;
                rgba[at..at + 4].copy_from_slice(&[v, v, v, 255]);
            }
        }
        let out = recognize(w, h, &rgba, None).expect("识别");
        println!("{out:?}");
        let joined: String = out
            .lines
            .iter()
            .map(|l| l.text.as_str())
            .collect::<Vec<_>>()
            .join(" ");
        assert!(
            joined.contains("Snow") || joined.contains("Hello"),
            "{joined}"
        );
    }
}
