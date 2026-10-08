//! 截图输出：导出配置读取、保存目录解析、格式编码落盘（快速保存 / 另存为 / 带请求覆盖的保存）。
//!
//! 保存按钮、复制后自动保存、直接截图与贴图 / 长截图保存都走这里，文件名模板、格式、质量、
//! 压缩级别、PDF 页面尺寸全部读配置（见 [`ExportSettings`]），不再写死前缀或格式。

use crate::export_format::{DEFAULT_QUALITY, EncodeSettings, ExportFormat, QUALITY_MAX, encode};
use crate::export_naming::{
    collision_safe_path, expand_template, is_valid_base_name, normalized_path,
};
use crate::export_pdf::MAX_TITLE_CHARS;
use crate::ocr_backend::i18n_for;
use image::codecs::png::PngEncoder;
use image::{ExtendedColorType, ImageEncoder};
use serde_json::Value;
use snow_app_core::command::{
    CompressionLevel, DirectCaptureRequest, DirectOutput, PdfPageSize, SaveRequest,
};
use snow_config::document::ConfigDocument;
use snow_config::store::ConfigStore;
use snow_i18n::Args;
use snow_platform::file_dialog::{FileFilter, SaveDialogRequest};
use snow_platform::local_time::LocalDateTime;
use std::path::{Path, PathBuf};

/// 保存目录的配置键。
pub const SAVE_DIRECTORY_CONFIG_KEY: &str = "screenshot/image_save_directory";
/// 图片格式的配置键。
pub const IMAGE_FORMAT_CONFIG_KEY: &str = "screenshot/image_format";
/// 图片质量的配置键。
pub const IMAGE_QUALITY_CONFIG_KEY: &str = "screenshot/image_quality";
/// 压缩级别的配置键。
pub const COMPRESSION_CONFIG_KEY: &str = "screenshot/compression_level";
/// PDF 页面尺寸的配置键。
pub const PDF_PAGE_SIZE_CONFIG_KEY: &str = "screenshot/pdf_page_size";
/// 手动保存文件名模板的配置键。
pub const MANUAL_FILENAME_CONFIG_KEY: &str = "screenshot/manual_save_filename_format";
/// 自动保存文件名模板的配置键。
pub const AUTO_FILENAME_CONFIG_KEY: &str = "screenshot/auto_save_filename_format";
/// 上次手动保存目录的配置键。
pub const LAST_MANUAL_DIRECTORY_CONFIG_KEY: &str = "screenshot/last_manual_save_directory";
/// 上次手动保存格式的配置键。
pub const LAST_MANUAL_FORMAT_CONFIG_KEY: &str = "screenshot/last_manual_save_format";
/// 复制后自动保存的配置键。
pub const AUTO_SAVE_AFTER_COPY_CONFIG_KEY: &str = "screenshot/auto_save_after_copy";
/// 复制图片文件到剪贴板的配置键。
pub const COPY_FILE_CONFIG_KEY: &str = "screenshot/copy_image_file_to_clipboard";
/// 另存为对话框种类的配置键。
pub const SAVE_DIALOG_CONFIG_KEY: &str = "screenshot/save_as_file_dialog";
/// 另存为对话框取值：自绘对话框（暂未实现，回退到系统对话框）。
pub const SAVE_DIALOG_CUSTOM: &str = "snow_shot";
/// 默认图片子目录名（位于用户目录下）。
const PICTURES_DIR_NAME: &str = "Pictures";
/// 默认文档子目录名（位于用户目录下）。
const DOCUMENTS_DIR_NAME: &str = "Documents";
/// RGBA 每像素字节数。
const RGBA_BYTES_PER_PIXEL: usize = 4;
/// 压缩级别配置值。
const LEVEL_LOW: &str = "low";
/// 压缩级别配置值。
const LEVEL_HIGH: &str = "high";
/// PDF 页面配置值。
const PAGE_IMAGE_SIZE: &str = "image_size";
/// PDF 页面配置值。
const PAGE_A4_LANDSCAPE: &str = "a4_landscape";

/// 导出失败原因（界面文案经 [`ExportError::message`] 按语言取得）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExportError {
    /// 没有选择输出文件。
    NoPath,
    /// 文件名模板展开后为空或含路径分隔符。
    InvalidFileName,
    /// 没有任何可用的保存目录。
    NoFolder,
    /// 输出目录无法创建（附系统原因）。
    CreateDir(String),
    /// 编码失败（附原因）。
    Encode(String),
    /// 写文件失败（附原因）。
    Write(String),
    /// 原生对话框失败（附原因）。
    Dialog(String),
    /// 写剪贴板失败（附原因）。
    Clipboard(String),
}

impl std::fmt::Display for ExportError {
    /// 面向日志的英文描述。
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NoPath => write!(f, "no output file selected"),
            Self::InvalidFileName => write!(f, "invalid screenshot filename format"),
            Self::NoFolder => write!(f, "no folder available for screenshots"),
            Self::CreateDir(e) => write!(f, "cannot create output directory: {e}"),
            Self::Encode(e) => write!(f, "encode failed: {e}"),
            Self::Write(e) => write!(f, "write failed: {e}"),
            Self::Dialog(e) => write!(f, "save dialog failed: {e}"),
            Self::Clipboard(e) => write!(f, "clipboard failed: {e}"),
        }
    }
}

impl ExportError {
    /// 按界面语言生成原因文案。
    ///
    /// # 参数
    /// - `locale`：界面语言代码（见 `locale.toml`），未知回退英文。
    ///
    /// # 返回
    /// 面向用户的原因文本（技术细节按原样附在后面）。
    ///
    /// ```ignore
    /// let text = ExportError::NoFolder.message("zh-CN");
    /// ```
    pub fn message(&self, locale: &str) -> String {
        let i18n = i18n_for(locale);
        match self {
            Self::NoPath => {
                i18n.tr("screenshot-image-file-service-no-output-file-was-selected-0def8b7f")
            }
            Self::InvalidFileName => i18n
                .tr("screenshot-recognition-file-export-the-screenshot-filename-format-i-6710b3ac"),
            Self::NoFolder => i18n.tr("screenshot-export-no-folder"),
            Self::CreateDir(detail) => format!(
                "{}: {detail}",
                i18n.tr("screenshot-image-file-service-the-output-directory-could-not-b-351d2c03")
            ),
            Self::Encode(detail)
            | Self::Write(detail)
            | Self::Dialog(detail)
            | Self::Clipboard(detail) => detail.clone(),
        }
    }

    /// 手动保存失败的完整提示（“截图保存失败：原因”）。
    pub fn manual_message(&self, locale: &str) -> String {
        i18n_for(locale).tr_with(
            "screenshot-controller-the-screenshot-could-not-be-save-cfe3d694",
            &Args::new().arg(1, self.message(locale)),
        )
    }

    /// 自动保存失败的完整提示（“自动保存截图失败：原因”）。
    pub fn auto_message(&self, locale: &str) -> String {
        i18n_for(locale).tr_with(
            "screenshot-controller-automatic-screenshot-saving-fail-86f01791",
            &Args::new().arg(1, self.message(locale)),
        )
    }
}

/// 一次导出用到的全部配置快照（可跨线程移动）。
#[derive(Debug, Clone, PartialEq)]
pub struct ExportSettings {
    /// 配置的图片格式键原文（小写）。
    pub format_key: String,
    /// 默认输出格式（配置值不受支持时回退 PNG）。
    pub format: ExportFormat,
    /// 质量 0..=100。
    pub quality: u8,
    /// 压缩级别。
    pub compression: CompressionLevel,
    /// PDF 页面尺寸。
    pub pdf_page: PdfPageSize,
    /// 配置的保存目录（可能为空）。
    pub save_directory: String,
    /// 手动保存文件名模板。
    pub manual_filename_format: String,
    /// 自动保存文件名模板。
    pub auto_filename_format: String,
    /// 上次手动保存目录。
    pub last_manual_directory: String,
    /// 上次手动保存格式。
    pub last_manual_format: ExportFormat,
    /// 复制后是否自动保存。
    pub auto_save_after_copy: bool,
    /// 是否把图片文件放进剪贴板（暂未接线到平台层）。
    pub copy_file_to_clipboard: bool,
    /// 另存为对话框种类（`system` / `snow_shot`）。
    pub save_dialog: String,
}

/// 读字符串配置，非字符串当空串。
fn text_of(document: &ConfigDocument, key: &str) -> String {
    document.value(key).as_str().unwrap_or_default().to_string()
}

impl ExportSettings {
    /// 从配置文档读取（缺失键由 schema 默认值补齐）。
    ///
    /// # 参数
    /// - `document`：配置文档。
    ///
    /// ```ignore
    /// let settings = ExportSettings::from_document(store.document());
    /// ```
    pub fn from_document(document: &ConfigDocument) -> Self {
        let format_key = text_of(document, IMAGE_FORMAT_CONFIG_KEY)
            .trim()
            .to_ascii_lowercase();
        let quality = document
            .value(IMAGE_QUALITY_CONFIG_KEY)
            .as_i64()
            .map_or(DEFAULT_QUALITY, |q| {
                q.clamp(0, i64::from(QUALITY_MAX)) as u8
            });
        Self {
            format: ExportFormat::from_key(&format_key).unwrap_or(ExportFormat::Png),
            format_key,
            quality,
            compression: compression_from_key(&text_of(document, COMPRESSION_CONFIG_KEY)),
            pdf_page: page_from_key(&text_of(document, PDF_PAGE_SIZE_CONFIG_KEY)),
            save_directory: text_of(document, SAVE_DIRECTORY_CONFIG_KEY),
            manual_filename_format: text_of(document, MANUAL_FILENAME_CONFIG_KEY),
            auto_filename_format: text_of(document, AUTO_FILENAME_CONFIG_KEY),
            last_manual_directory: text_of(document, LAST_MANUAL_DIRECTORY_CONFIG_KEY),
            last_manual_format: ExportFormat::from_key(&text_of(
                document,
                LAST_MANUAL_FORMAT_CONFIG_KEY,
            ))
            .unwrap_or(ExportFormat::Png),
            auto_save_after_copy: document
                .value(AUTO_SAVE_AFTER_COPY_CONFIG_KEY)
                .as_bool()
                .unwrap_or(false),
            copy_file_to_clipboard: document
                .value(COPY_FILE_CONFIG_KEY)
                .as_bool()
                .unwrap_or(false),
            save_dialog: text_of(document, SAVE_DIALOG_CONFIG_KEY),
        }
    }

    /// 配置的格式键不被支持（未知或 jxl / avif）时返回 `true`，调用方应记日志后按 PNG 保存。
    pub fn format_fallback(&self) -> bool {
        ExportFormat::from_key(&self.format_key).is_none()
    }

    /// 替换保存目录（验收脚本与测试用）。
    pub fn with_directory(mut self, directory: &Path) -> Self {
        self.save_directory = directory.to_string_lossy().to_string();
        self
    }

    /// 合并请求覆盖，得到本次编码参数。
    ///
    /// # 参数
    /// - `overrides`：请求里显式给出的覆盖项。
    /// - `title`：PDF 标题缺省值（一般取文件名主干）。
    /// - `created`：创建时间。
    pub fn encode_settings(
        &self,
        overrides: &ExportOverrides,
        title: &str,
        created: LocalDateTime,
    ) -> EncodeSettings {
        let title = overrides
            .pdf_title
            .as_deref()
            .filter(|t| !t.trim().is_empty())
            .unwrap_or(title);
        EncodeSettings {
            quality: overrides.quality.unwrap_or(self.quality),
            compression: overrides.compression.unwrap_or(self.compression),
            pdf_page: overrides.pdf_page.unwrap_or(self.pdf_page),
            pdf_title: title.chars().take(MAX_TITLE_CHARS).collect(),
            created,
        }
    }

    /// 本次输出格式：请求覆盖优先，否则用配置。
    pub fn effective_format(&self, overrides: &ExportOverrides) -> ExportFormat {
        overrides.format.unwrap_or(self.format)
    }
}

/// 压缩级别配置值解析（未知按“中”，与旧版一致）。
pub(crate) fn compression_from_key(key: &str) -> CompressionLevel {
    match key.trim().to_ascii_lowercase().as_str() {
        LEVEL_LOW => CompressionLevel::Low,
        LEVEL_HIGH => CompressionLevel::High,
        _ => CompressionLevel::Medium,
    }
}

/// PDF 页面配置值解析（未知按 A4 纵向，与旧版一致）。
fn page_from_key(key: &str) -> PdfPageSize {
    match key.trim() {
        PAGE_IMAGE_SIZE => PdfPageSize::ImageSize,
        PAGE_A4_LANDSCAPE => PdfPageSize::A4Landscape,
        _ => PdfPageSize::A4Portrait,
    }
}

/// 请求里显式给出的导出覆盖项（`None` 表示沿用配置）。
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ExportOverrides {
    /// 输出格式。
    pub format: Option<ExportFormat>,
    /// 质量。
    pub quality: Option<u8>,
    /// 压缩级别。
    pub compression: Option<CompressionLevel>,
    /// PDF 页面尺寸。
    pub pdf_page: Option<PdfPageSize>,
    /// PDF 标题。
    pub pdf_title: Option<String>,
}

impl ExportOverrides {
    /// 取自保存请求；不支持的格式（jxl / avif）视为未指定。
    pub fn from_save_request(request: &SaveRequest) -> Self {
        Self {
            format: request.format.and_then(ExportFormat::from_command),
            quality: request.quality.map(|q| q.min(u32::from(QUALITY_MAX)) as u8),
            compression: request.compression_level,
            pdf_page: request.pdf_page_size,
            pdf_title: request.pdf_title.clone(),
        }
    }

    /// 取自直接截图请求。
    pub fn from_direct(request: &DirectCaptureRequest) -> Self {
        Self {
            format: request.format.and_then(ExportFormat::from_command),
            quality: request.quality.map(|q| q.min(u32::from(QUALITY_MAX)) as u8),
            compression: request.compression_level,
            pdf_page: request.pdf_page_size,
            pdf_title: request.pdf_title.clone(),
        }
    }
}

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
    let settings = ExportSettings::from_document(document);
    let supported = !settings.format_fallback();
    (settings.format_key, supported)
}

/// 自动保存的候选目录，按优先级：配置目录、用户图片目录、用户文档目录；都没有则用系统临时目录。
///
/// # 参数
/// - `configured`：配置里的保存目录（可为空）。
/// - `home`：用户目录。
///
/// # 返回
/// 去重（忽略大小写）后的目录列表，保存时逐个尝试直到成功。
///
/// ```ignore
/// let dirs = automatic_candidates("D:/shots", Some(Path::new("C:/Users/a")));
/// ```
pub fn automatic_candidates(configured: &str, home: Option<&Path>) -> Vec<PathBuf> {
    /// 忽略大小写去重后追加。
    fn push_unique(list: &mut Vec<PathBuf>, path: PathBuf) {
        // 比较前统一分隔符，避免 `a/b` 与 `a\b` 被当成两个目录
        let key = |p: &Path| crate::export_naming::clean_path(&p.to_string_lossy()).to_lowercase();
        let wanted = key(&path);
        if !list.iter().any(|p| key(p) == wanted) {
            list.push(path);
        }
    }
    let mut list: Vec<PathBuf> = Vec::new();
    let configured = configured.trim();
    if !configured.is_empty() {
        push_unique(
            &mut list,
            PathBuf::from(crate::export_naming::clean_path(configured)),
        );
    }
    if let Some(home) = home {
        push_unique(&mut list, home.join(PICTURES_DIR_NAME));
        push_unique(&mut list, home.join(DOCUMENTS_DIR_NAME));
    }
    if list.is_empty() {
        push_unique(&mut list, std::env::temp_dir());
    }
    list
}

/// 把 RGBA 像素编码为 PNG 字节（贴图仓储等内部用途，固定默认压缩）。
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

/// 把 RGBA 像素按指定压缩级别编码为 PNG 字节（贴图历史按 `pinned_history/compression_level` 落盘用）。
///
/// # 参数
/// - `width` / `height`：图像尺寸。
/// - `rgba`：RGBA 像素（长度须为 `宽 * 高 * 4`）。
/// - `level`：压缩级别（低 = 最快，高 = 最小）。
///
/// # 返回
/// PNG 字节；参数不合法或编码失败返回错误说明。
///
/// ```ignore
/// let png = encode_png_with_level(1, 1, &[255, 0, 0, 255], CompressionLevel::High).unwrap();
/// assert_eq!(&png[1..4], b"PNG");
/// ```
pub fn encode_png_with_level(
    width: u32,
    height: u32,
    rgba: &[u8],
    level: CompressionLevel,
) -> Result<Vec<u8>, String> {
    let settings = crate::export_format::EncodeSettings {
        quality: u8::MAX,
        compression: level,
        pdf_page: PdfPageSize::ImageSize,
        pdf_title: String::new(),
        created: snow_platform::local_time::now(),
    };
    crate::export_format::encode(
        crate::export_format::ExportFormat::Png,
        width,
        height,
        rgba,
        &settings,
    )
}

/// 原子写文件：先写同目录临时文件再改名覆盖，失败时清理临时文件。
///
/// # 参数
/// - `path`：目标路径（父目录须已存在）。
/// - `bytes`：文件内容。
///
/// # 返回
/// 成功为 `Ok(())`，失败返回系统错误说明。
pub fn write_atomic(path: &Path, bytes: &[u8]) -> Result<(), String> {
    let mut temp_name = path
        .file_name()
        .map(|n| n.to_os_string())
        .unwrap_or_default();
    temp_name.push(format!(".{}.tmp", std::process::id()));
    let temp = path.with_file_name(temp_name);
    let result = std::fs::write(&temp, bytes).and_then(|()| std::fs::rename(&temp, path));
    if let Err(e) = result {
        let _ = std::fs::remove_file(&temp);
        return Err(format!("{}: {e}", path.display()));
    }
    Ok(())
}

/// 文件名主干（不含扩展名），用作 PDF 标题缺省值。
fn file_stem_of(path: &Path) -> String {
    path.file_stem()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_default()
}

/// 保存到指定路径：扩展名按格式归一，必要时创建目录，原子写入。
///
/// # 参数
/// - `path`：目标路径（扩展名会被改成与格式一致）。
/// - `format`：输出格式。
/// - `width` / `height` / `rgba`：图像尺寸与像素。
/// - `encode_settings`：编码参数（PDF 标题为空时取文件名主干）。
///
/// # 返回
/// 实际写入的路径；失败返回 [`ExportError`]。
///
/// ```ignore
/// let saved = save_to_path("C:/tmp/a", ExportFormat::Png, 1, 1, &px, &enc)?;
/// ```
pub fn save_to_path(
    path: &str,
    format: ExportFormat,
    width: u32,
    height: u32,
    rgba: &[u8],
    encode_settings: &EncodeSettings,
) -> Result<PathBuf, ExportError> {
    let normalized = normalized_path(path, format.extension());
    if normalized.is_empty() {
        return Err(ExportError::NoPath);
    }
    let target = PathBuf::from(&normalized);
    let mut enc = encode_settings.clone();
    if enc.pdf_title.is_empty() {
        enc.pdf_title = file_stem_of(&target);
    }
    let bytes = encode_checked(format, width, height, rgba, &enc)?;
    if let Some(parent) = target.parent().filter(|p| !p.as_os_str().is_empty()) {
        std::fs::create_dir_all(parent)
            .map_err(|e| ExportError::CreateDir(format!("{}: {e}", parent.display())))?;
    }
    write_atomic(&target, &bytes).map_err(ExportError::Write)?;
    Ok(target)
}

/// 编码并在 WebP 有损请求时记一条日志。
fn encode_checked(
    format: ExportFormat,
    width: u32,
    height: u32,
    rgba: &[u8],
    settings: &EncodeSettings,
) -> Result<Vec<u8>, ExportError> {
    if format == ExportFormat::Webp
        && crate::export_format::webp_lossy_unsupported(settings.quality)
    {
        tracing::warn!(
            quality = settings.quality,
            "WebP 暂无有损编码器，按无损输出"
        );
    }
    encode(format, width, height, rgba, settings).map_err(ExportError::Encode)
}

/// 快速保存：按配置的目录、文件名模板与格式直接落盘，目录不可用时依次尝试后备目录。
///
/// # 参数
/// - `settings`：导出配置快照。
/// - `overrides`：请求覆盖项。
/// - `width` / `height` / `rgba`：图像尺寸与像素。
/// - `home`：用户目录。
/// - `now`：用于展开文件名模板的本地时间。
///
/// # 返回
/// 写入的路径；全部候选目录失败时返回最后一个错误，没有候选目录返回 [`ExportError::NoFolder`]。
///
/// ```ignore
/// let path = save_automatic(&settings, &ExportOverrides::default(), w, h, &rgba, home, now)?;
/// ```
pub fn save_automatic(
    settings: &ExportSettings,
    overrides: &ExportOverrides,
    width: u32,
    height: u32,
    rgba: &[u8],
    home: Option<&Path>,
    now: LocalDateTime,
) -> Result<PathBuf, ExportError> {
    let format = settings.effective_format(overrides);
    let base = expand_template(&settings.auto_filename_format, &now);
    if !is_valid_base_name(&base) {
        return Err(ExportError::InvalidFileName);
    }
    let bytes = encode_checked(
        format,
        width,
        height,
        rgba,
        &settings.encode_settings(overrides, &base, now),
    )?;
    let mut last = ExportError::NoFolder;
    for dir in automatic_candidates(&settings.save_directory, home) {
        if let Err(e) = std::fs::create_dir_all(&dir) {
            last = ExportError::CreateDir(format!("{}: {e}", dir.display()));
            continue;
        }
        let Some(path) = collision_safe_path(&dir, &base, format.extension()) else {
            last = ExportError::Write(format!("{}: no free file name", dir.display()));
            continue;
        };
        match write_atomic(&path, &bytes) {
            Ok(()) => return Ok(path),
            Err(e) => last = ExportError::Write(e),
        }
    }
    Err(last)
}

/// 用当前配置快速保存（贴图 / 长截图等没有请求覆盖的场景）。
///
/// # 参数
/// - `document`：配置文档。
/// - `width` / `height` / `rgba`：图像尺寸与像素。
///
/// # 返回
/// 写入的路径或失败原因。
pub fn quick_save(
    document: &ConfigDocument,
    width: u32,
    height: u32,
    rgba: &[u8],
) -> Result<PathBuf, ExportError> {
    let settings = ExportSettings::from_document(document);
    save_automatic(
        &settings,
        &ExportOverrides::default(),
        width,
        height,
        rgba,
        home_directory().as_deref(),
        snow_platform::local_time::now(),
    )
}

/// 另存为对话框的初始状态。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DialogPlan {
    /// 初始目录。
    pub initial_dir: PathBuf,
    /// 预填文件名（含扩展名）。
    pub file_name: String,
    /// 初始选中的格式。
    pub format: ExportFormat,
}

/// 计算另存为对话框的初始目录、文件名与格式。
///
/// # 参数
/// - `settings`：导出配置快照。
/// - `home`：用户目录。
/// - `now`：本地时间。
///
/// # 返回
/// 初始状态：目录优先取“上次手动保存目录”（须仍存在），否则取自动保存的首选候选目录；
/// 格式取上次手动保存格式；文件名按手动保存模板展开。
///
/// ```ignore
/// let plan = dialog_plan(&settings, home, now);
/// ```
pub fn dialog_plan(
    settings: &ExportSettings,
    home: Option<&Path>,
    now: LocalDateTime,
) -> DialogPlan {
    let remembered = settings.last_manual_directory.trim();
    let initial_dir = if !remembered.is_empty() && Path::new(remembered).is_dir() {
        PathBuf::from(crate::export_naming::clean_path(remembered))
    } else {
        automatic_candidates(&settings.save_directory, home)
            .into_iter()
            .next()
            .unwrap_or_else(std::env::temp_dir)
    };
    let format = settings.last_manual_format;
    let base = expand_template(&settings.manual_filename_format, &now);
    DialogPlan {
        file_name: format!("{base}.{}", format.extension()),
        initial_dir,
        format,
    }
}

/// 过滤项在界面语言下的显示名对应的消息 id。
fn filter_label_id(format: ExportFormat) -> &'static str {
    match format {
        ExportFormat::Png => "screenshot-image-file-service-png-image-png-a7cceee3",
        ExportFormat::Jpeg => "screenshot-image-file-service-jpeg-image-jpg-jpeg-eca99480",
        ExportFormat::Bmp => "screenshot-image-file-service-bmp-image-bmp-db6a08db",
        ExportFormat::Webp => "screenshot-image-file-service-web-p-image-webp-04e7013f",
        ExportFormat::Pdf => "screenshot-image-file-service-pdf-document-pdf-8e176c5f",
    }
}

/// 构造另存为对话框请求（过滤项顺序与 [`ExportFormat::ALL`] 一致）。
///
/// # 参数
/// - `plan`：初始状态。
/// - `locale`：界面语言代码。
/// - `owner`：所属窗口句柄。
pub fn dialog_request(plan: &DialogPlan, locale: &str, owner: Option<isize>) -> SaveDialogRequest {
    let i18n = i18n_for(locale);
    SaveDialogRequest {
        title: i18n.tr("screenshot-controller-save-screenshot-1af916ca"),
        initial_dir: Some(plan.initial_dir.clone()),
        file_name: plan.file_name.clone(),
        filters: ExportFormat::ALL
            .iter()
            .map(|f| FileFilter {
                label: i18n.tr(filter_label_id(*f)),
                pattern: f.pattern().to_string(),
            })
            .collect(),
        filter_index: ExportFormat::ALL
            .iter()
            .position(|f| *f == plan.format)
            .unwrap_or(0),
        owner,
    }
}

/// 由对话框结果判定输出格式：路径扩展名优先，其次所选过滤项，最后 PNG（对应旧版 `formatForDialogSelection`）。
///
/// # 参数
/// - `path`：用户确认的路径。
/// - `filter_index`：所选过滤项下标。
///
/// ```ignore
/// assert_eq!(format_for_choice("a.bmp", 0), ExportFormat::Bmp);
/// ```
pub fn format_for_choice(path: &str, filter_index: usize) -> ExportFormat {
    Path::new(path)
        .extension()
        .and_then(|e| e.to_str())
        .and_then(ExportFormat::from_extension)
        .or_else(|| ExportFormat::ALL.get(filter_index).copied())
        .unwrap_or(ExportFormat::Png)
}

/// 记住这次手动保存的目录与格式（只在变化时写盘）。
///
/// # 参数
/// - `store`：配置存储。
/// - `saved`：实际写入的文件路径。
/// - `format`：本次格式。
///
/// # 返回
/// 写盘失败时返回错误说明；没有变化或成功返回 `Ok(())`。
pub fn remember_manual_save(
    store: &mut ConfigStore,
    saved: &Path,
    format: ExportFormat,
) -> Result<(), String> {
    let directory = saved
        .parent()
        .map(|p| std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf()))
        .map(|p| strip_verbatim(&p.to_string_lossy()))
        .unwrap_or_default();
    for (key, value) in [
        (LAST_MANUAL_DIRECTORY_CONFIG_KEY, directory),
        (LAST_MANUAL_FORMAT_CONFIG_KEY, format.key().to_string()),
    ] {
        if value.is_empty() || store.value(key).as_str() == Some(value.as_str()) {
            continue;
        }
        store
            .set_value(key, Value::String(value))
            .map_err(|e| e.to_string())?;
    }
    store.flush().map_err(|e| e.to_string())
}

/// 另存为的结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SaveOutcome {
    /// 已写入该路径。
    Saved(PathBuf),
    /// 用户在对话框里取消了。
    Cancelled,
}

/// 手动保存的方式。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SaveMode {
    /// 弹出系统保存对话框。
    Dialog,
    /// 写到请求里给定的路径。
    Path(String),
    /// 按自动保存规则落盘（不弹对话框）。
    Automatic,
}

/// 一次手动保存的完整输入（纯数据，可在不持有界面借用的情况下执行，对话框会进入模态循环）。
#[derive(Debug, Clone)]
pub struct ManualSaveJob {
    /// 导出配置快照。
    pub settings: ExportSettings,
    /// 请求覆盖项。
    pub overrides: ExportOverrides,
    /// 保存方式。
    pub mode: SaveMode,
    /// 界面语言代码（失败提示用）。
    pub locale: String,
    /// 对话框所有者窗口句柄。
    pub owner: Option<isize>,
    /// 用户目录。
    pub home: Option<PathBuf>,
    /// 用于展开文件名模板的本地时间。
    pub now: LocalDateTime,
}

/// 手动保存完成后的信息。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ManualSaveDone {
    /// 保存结果。
    pub outcome: SaveOutcome,
    /// 实际使用的格式。
    pub format: ExportFormat,
    /// 是否应记住这次的目录与格式（只有走了对话框才记）。
    pub remember: bool,
}

impl ManualSaveJob {
    /// 执行保存。
    ///
    /// # 参数
    /// - `width` / `height` / `rgba`：图像尺寸与像素。
    ///
    /// # 返回
    /// 保存结果；失败时是按界面语言生成的完整提示（可直接显示给用户）。
    ///
    /// ```ignore
    /// let done = job.run(w, h, &rgba)?;
    /// ```
    pub fn run(&self, width: u32, height: u32, rgba: &[u8]) -> Result<ManualSaveDone, String> {
        let locale = self.locale.as_str();
        let settings = &self.settings;
        match &self.mode {
            SaveMode::Path(path) => {
                let from_path = Path::new(path)
                    .extension()
                    .and_then(|e| e.to_str())
                    .and_then(ExportFormat::from_extension);
                let format = self
                    .overrides
                    .format
                    .or(from_path)
                    .unwrap_or(settings.format);
                let encode = settings.encode_settings(&self.overrides, "", self.now);
                let saved = save_to_path(path, format, width, height, rgba, &encode)
                    .map_err(|e| e.manual_message(locale))?;
                Ok(ManualSaveDone {
                    outcome: SaveOutcome::Saved(saved),
                    format,
                    remember: false,
                })
            }
            SaveMode::Automatic => {
                let format = settings.effective_format(&self.overrides);
                let saved = save_automatic(
                    settings,
                    &self.overrides,
                    width,
                    height,
                    rgba,
                    self.home.as_deref(),
                    self.now,
                )
                .map_err(|e| e.manual_message(locale))?;
                Ok(ManualSaveDone {
                    outcome: SaveOutcome::Saved(saved),
                    format,
                    remember: false,
                })
            }
            SaveMode::Dialog => {
                if settings.save_dialog == SAVE_DIALOG_CUSTOM {
                    tracing::info!("自绘另存为对话框尚未实现，改用系统对话框");
                }
                let plan = dialog_plan(settings, self.home.as_deref(), self.now);
                // 目录不存在时提前创建，保证对话框能定位到它（失败也不影响弹窗）
                let _ = std::fs::create_dir_all(&plan.initial_dir);
                let request = dialog_request(&plan, locale, self.owner);
                let choice = match snow_platform::file_dialog::show_save_dialog(&request) {
                    Ok(Some(choice)) => choice,
                    Ok(None) => {
                        return Ok(ManualSaveDone {
                            outcome: SaveOutcome::Cancelled,
                            format: plan.format,
                            remember: false,
                        });
                    }
                    Err(e) => return Err(ExportError::Dialog(e).manual_message(locale)),
                };
                let path = choice.path.to_string_lossy().to_string();
                let format = format_for_choice(&path, choice.filter_index);
                let encode = settings.encode_settings(&self.overrides, "", self.now);
                let saved = save_to_path(&path, format, width, height, rgba, &encode)
                    .map_err(|e| e.manual_message(locale))?;
                Ok(ManualSaveDone {
                    outcome: SaveOutcome::Saved(saved),
                    format,
                    remember: true,
                })
            }
        }
    }
}

/// 写剪贴板的回调：`(宽, 高, RGBA)`，失败返回原因。
pub type CopyImageFn<'a> = dyn FnMut(u32, u32, &[u8]) -> Result<(), String> + 'a;

/// 直接截图的导出结果。
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct DirectExportResult {
    /// 是否已复制到剪贴板。
    pub copied: bool,
    /// 写入的文件路径（保存，或复制后自动保存）。
    pub saved: Option<PathBuf>,
    /// 复制后自动保存失败的原因（不影响复制本身）。
    pub auto_save_error: Option<ExportError>,
}

/// 直接截图的导出：复制到剪贴板（可附带自动保存），或保存到文件（指定路径 / 自动路径）。
///
/// # 参数
/// - `settings`：导出配置快照。
/// - `request`：直接截图请求（取输出去向、路径与覆盖项）。
/// - `width` / `height` / `rgba`：图像尺寸与像素。
/// - `home` / `now`：用户目录与本地时间。
/// - `copy`：写剪贴板的回调（便于测试替换）。
///
/// # 返回
/// 导出结果；`Render` 输出不在此处理，返回 [`ExportError::NoPath`]。
///
/// ```ignore
/// let done = export_direct(&settings, &request, w, h, &rgba, home, now, &mut |w, h, p| copy(w, h, p))?;
/// ```
#[allow(clippy::too_many_arguments)]
pub fn export_direct(
    settings: &ExportSettings,
    request: &DirectCaptureRequest,
    width: u32,
    height: u32,
    rgba: &[u8],
    home: Option<&Path>,
    now: LocalDateTime,
    copy: &mut CopyImageFn<'_>,
) -> Result<DirectExportResult, ExportError> {
    let overrides = ExportOverrides::from_direct(request);
    match request.output {
        DirectOutput::Copy => {
            copy(width, height, rgba).map_err(ExportError::Clipboard)?;
            let mut result = DirectExportResult {
                copied: true,
                ..DirectExportResult::default()
            };
            if settings.auto_save_after_copy {
                match save_automatic(
                    settings,
                    &ExportOverrides::default(),
                    width,
                    height,
                    rgba,
                    home,
                    now,
                ) {
                    Ok(path) => result.saved = Some(path),
                    Err(e) => result.auto_save_error = Some(e),
                }
            }
            Ok(result)
        }
        DirectOutput::Save => {
            let path = request
                .path
                .as_deref()
                .map(str::trim)
                .filter(|p| !p.is_empty());
            let saved = match path {
                Some(path) if request.automatic_path != Some(true) => {
                    let from_path = Path::new(path)
                        .extension()
                        .and_then(|e| e.to_str())
                        .and_then(ExportFormat::from_extension);
                    let format = overrides.format.or(from_path).unwrap_or(settings.format);
                    let encode = settings.encode_settings(&overrides, "", now);
                    save_to_path(path, format, width, height, rgba, &encode)?
                }
                _ => save_automatic(settings, &overrides, width, height, rgba, home, now)?,
            };
            Ok(DirectExportResult {
                saved: Some(saved),
                ..DirectExportResult::default()
            })
        }
        DirectOutput::Render => Err(ExportError::NoPath),
    }
}

/// 去掉 Windows 规范化路径的 `\\?\` 前缀，避免把它写进配置。
fn strip_verbatim(path: &str) -> String {
    path.strip_prefix(r"\\?\").unwrap_or(path).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::export_pdf::MAX_TITLE_CHARS;

    /// 对照 Qt 测试用的固定时间：2026-08-14 09:07:06。
    fn golden_time() -> LocalDateTime {
        LocalDateTime {
            year: 2026,
            month: 8,
            day: 14,
            hour: 9,
            minute: 7,
            second: 6,
        }
    }

    /// 生成一个唯一的临时目录路径（不创建）。
    fn temp_dir(tag: &str) -> PathBuf {
        std::env::temp_dir().join(format!("cisox-output-{tag}-{}", std::process::id()))
    }

    /// 3x2 的测试图。
    fn image() -> Vec<u8> {
        (0..6u8)
            .flat_map(|i| [i * 40, 255 - i * 40, i * 10, 255])
            .collect()
    }

    /// 用默认配置派生设置，并按需改写若干键。
    fn settings_with(changes: &[(&str, Value)]) -> ExportSettings {
        let mut doc = ConfigDocument::from_bytes(None);
        for (key, value) in changes {
            doc.set_value(key, value.clone()).unwrap();
        }
        ExportSettings::from_document(&doc)
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

    /// 候选目录顺序：配置、图片、文档；忽略大小写去重；全空时用临时目录。
    #[test]
    fn candidate_directories_order_and_dedupe() {
        let home = Path::new("C:/Users/a");
        let dirs = automatic_candidates("D:/shots", Some(home));
        assert_eq!(
            dirs,
            vec![
                PathBuf::from("D:/shots"),
                home.join("Pictures"),
                home.join("Documents"),
            ]
        );
        let dup = automatic_candidates("c:/users/a/pictures", Some(home));
        assert_eq!(dup.len(), 2, "与图片目录重复的配置只保留一个: {dup:?}");
        assert_eq!(automatic_candidates("  ", None), vec![std::env::temp_dir()]);
    }

    /// 默认配置的格式为 png 且受支持；jxl / avif / 未知判定为不支持并回退 PNG。
    #[test]
    fn format_support_detection() {
        let doc = ConfigDocument::from_bytes(None);
        assert_eq!(configured_format(&doc), ("png".to_string(), true));
        for (key, supported, effective) in [
            ("webp", true, ExportFormat::Webp),
            ("jpeg", true, ExportFormat::Jpeg),
            ("pdf", true, ExportFormat::Pdf),
            ("avif", false, ExportFormat::Png),
            ("jxl", false, ExportFormat::Png),
        ] {
            let mut doc = ConfigDocument::from_bytes(None);
            doc.set_value(IMAGE_FORMAT_CONFIG_KEY, Value::String(key.into()))
                .unwrap();
            let settings = ExportSettings::from_document(&doc);
            assert_eq!(configured_format(&doc).1, supported, "{key}");
            assert_eq!(settings.format, effective, "{key}");
            assert_eq!(settings.format_fallback(), !supported, "{key}");
        }
    }

    /// 旧配置缺键时全部补默认：模板含产品名，质量 100，压缩中，PDF 为 A4 纵向。
    #[test]
    fn missing_keys_fall_back_to_defaults() {
        let old = br#"{"screenshot":{"capture_cursor":true}}"#;
        let doc = ConfigDocument::from_bytes(Some(old));
        let s = ExportSettings::from_document(&doc);
        let template = format!("{}_{{YYYY-MM-DD_HH-mm-ss}}", snow_app_core::PRODUCT_NAME);
        assert_eq!(s.auto_filename_format, template);
        assert_eq!(s.manual_filename_format, template);
        assert_eq!(s.format, ExportFormat::Png);
        assert_eq!(
            (s.quality, s.compression, s.pdf_page),
            (100, CompressionLevel::Medium, PdfPageSize::A4Portrait)
        );
        assert!(!s.auto_save_after_copy && !s.copy_file_to_clipboard);
        assert_eq!(s.last_manual_format, ExportFormat::Png);
        assert!(s.last_manual_directory.is_empty());
    }

    /// 配置值被逐项读取：质量、压缩、PDF 页面、模板、复制后自动保存。
    #[test]
    fn settings_read_every_key() {
        let s = settings_with(&[
            (IMAGE_FORMAT_CONFIG_KEY, Value::String("jpeg".into())),
            (IMAGE_QUALITY_CONFIG_KEY, Value::from(42)),
            (COMPRESSION_CONFIG_KEY, Value::String("high".into())),
            (
                PDF_PAGE_SIZE_CONFIG_KEY,
                Value::String("a4_landscape".into()),
            ),
            (
                AUTO_FILENAME_CONFIG_KEY,
                Value::String("Auto_{yyyyMMdd}".into()),
            ),
            (
                MANUAL_FILENAME_CONFIG_KEY,
                Value::String("Manual_{yyyyMMdd}".into()),
            ),
            (AUTO_SAVE_AFTER_COPY_CONFIG_KEY, Value::Bool(true)),
            (LAST_MANUAL_FORMAT_CONFIG_KEY, Value::String("webp".into())),
            (
                LAST_MANUAL_DIRECTORY_CONFIG_KEY,
                Value::String("D:/last".into()),
            ),
            (SAVE_DIALOG_CONFIG_KEY, Value::String("snow_shot".into())),
            (COPY_FILE_CONFIG_KEY, Value::Bool(true)),
        ]);
        assert_eq!(s.format, ExportFormat::Jpeg);
        assert_eq!(s.quality, 42);
        assert_eq!(s.compression, CompressionLevel::High);
        assert_eq!(s.pdf_page, PdfPageSize::A4Landscape);
        assert_eq!(s.auto_filename_format, "Auto_{yyyyMMdd}");
        assert_eq!(s.manual_filename_format, "Manual_{yyyyMMdd}");
        assert!(s.auto_save_after_copy && s.copy_file_to_clipboard);
        assert_eq!(s.last_manual_format, ExportFormat::Webp);
        assert_eq!(s.last_manual_directory, "D:/last");
        assert_eq!(s.save_dialog, SAVE_DIALOG_CUSTOM);
    }

    /// 请求覆盖优先于配置；PDF 标题缺省取文件名并按上限截断。
    #[test]
    fn overrides_beat_config() {
        let s = settings_with(&[(IMAGE_QUALITY_CONFIG_KEY, Value::from(50))]);
        let request = SaveRequest {
            quality: Some(77),
            compression_level: Some(CompressionLevel::Low),
            pdf_page_size: Some(PdfPageSize::ImageSize),
            pdf_title: Some("x".repeat(MAX_TITLE_CHARS + 5)),
            format: Some(snow_app_core::command::Format::Jpeg),
            ..SaveRequest::default()
        };
        let o = ExportOverrides::from_save_request(&request);
        assert_eq!(s.effective_format(&o), ExportFormat::Jpeg);
        let enc = s.encode_settings(&o, "stem", golden_time());
        assert_eq!(
            (enc.quality, enc.compression, enc.pdf_page),
            (77, CompressionLevel::Low, PdfPageSize::ImageSize)
        );
        assert_eq!(enc.pdf_title.chars().count(), MAX_TITLE_CHARS);
        let none = ExportOverrides::default();
        let enc = s.encode_settings(&none, "stem", golden_time());
        assert_eq!((enc.quality, enc.pdf_title.as_str()), (50, "stem"));
        let unsupported = SaveRequest {
            format: Some(snow_app_core::command::Format::Avif),
            ..SaveRequest::default()
        };
        assert_eq!(
            ExportOverrides::from_save_request(&unsupported).format,
            None
        );
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

    /// 快速保存：按模板命名、重名加 `_1`（对照 Qt 的 `SnowShot_...` / `_1.png` 用例）、按配置格式编码。
    #[test]
    fn automatic_save_names_and_collisions() {
        let dir = temp_dir("auto");
        let _ = std::fs::remove_dir_all(&dir);
        let s = settings_with(&[
            (
                AUTO_FILENAME_CONFIG_KEY,
                Value::String("Auto_{yyyyMMdd_HHmmss}".into()),
            ),
            (IMAGE_FORMAT_CONFIG_KEY, Value::String("bmp".into())),
        ])
        .with_directory(&dir);
        let first = save_automatic(
            &s,
            &ExportOverrides::default(),
            3,
            2,
            &image(),
            None,
            golden_time(),
        )
        .unwrap();
        assert_eq!(first, dir.join("Auto_20260814_090706.bmp"));
        assert_eq!(&std::fs::read(&first).unwrap()[..2], b"BM");
        let second = save_automatic(
            &s,
            &ExportOverrides::default(),
            3,
            2,
            &image(),
            None,
            golden_time(),
        )
        .unwrap();
        assert_eq!(second, dir.join("Auto_20260814_090706_1.bmp"));
        // 目录是自动创建的；临时文件不会残留
        let leftovers = std::fs::read_dir(&dir).unwrap().filter(|e| {
            e.as_ref()
                .unwrap()
                .file_name()
                .to_string_lossy()
                .ends_with(".tmp")
        });
        assert_eq!(leftovers.count(), 0);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 默认模板的文件名前缀来自产品名而不是写死的 `snow-shot`。
    #[test]
    fn default_prefix_is_product_name() {
        let dir = temp_dir("prefix");
        let _ = std::fs::remove_dir_all(&dir);
        let s = settings_with(&[]).with_directory(&dir);
        let path = save_automatic(
            &s,
            &ExportOverrides::default(),
            3,
            2,
            &image(),
            None,
            golden_time(),
        )
        .unwrap();
        let name = path.file_name().unwrap().to_string_lossy().to_string();
        assert_eq!(
            name,
            format!("{}_2026-08-14_09-07-06.png", snow_app_core::PRODUCT_NAME)
        );
        assert!(!name.contains("snow-shot"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 每种格式都能落盘，且文件头与扩展名匹配；请求覆盖可改格式。
    #[test]
    fn automatic_save_every_format() {
        let dir = temp_dir("formats");
        let _ = std::fs::remove_dir_all(&dir);
        let s = settings_with(&[]).with_directory(&dir);
        for (format, magic, ext) in [
            (ExportFormat::Png, &b"\x89PNG"[..], "png"),
            (ExportFormat::Jpeg, &[0xFF, 0xD8, 0xFF][..], "jpg"),
            (ExportFormat::Webp, &b"RIFF"[..], "webp"),
            (ExportFormat::Pdf, &b"%PDF-1.7"[..], "pdf"),
        ] {
            let o = ExportOverrides {
                format: Some(format),
                ..ExportOverrides::default()
            };
            let path = save_automatic(&s, &o, 3, 2, &image(), None, golden_time()).unwrap();
            assert_eq!(path.extension().unwrap(), ext);
            assert!(
                std::fs::read(&path).unwrap().starts_with(magic),
                "{format:?}"
            );
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// PDF 快速保存：标题取文件名主干，页面尺寸取配置。
    #[test]
    fn pdf_title_defaults_to_file_stem() {
        let dir = temp_dir("pdf");
        let _ = std::fs::remove_dir_all(&dir);
        let s = settings_with(&[
            (IMAGE_FORMAT_CONFIG_KEY, Value::String("pdf".into())),
            (PDF_PAGE_SIZE_CONFIG_KEY, Value::String("image_size".into())),
            (AUTO_FILENAME_CONFIG_KEY, Value::String("T".into())),
        ])
        .with_directory(&dir);
        let path = save_automatic(
            &s,
            &ExportOverrides::default(),
            40,
            20,
            &vec![200u8; 40 * 20 * 4],
            None,
            golden_time(),
        )
        .unwrap();
        let text = String::from_utf8_lossy(&std::fs::read(&path).unwrap()).to_string();
        assert!(text.contains("/Title <FEFF0054>"), "标题应为文件名主干 T");
        assert!(text.contains("/MediaBox [0 0 30.00000000 15.00000000]"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 配置目录不可用（路径被文件占用）时回退到后备目录；全部失败返回错误；模板非法报错。
    #[test]
    fn unwritable_directory_falls_back_then_errors() {
        let root = temp_dir("unwritable");
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let blocker = root.join("blocker");
        std::fs::write(&blocker, b"file").unwrap();
        let home = root.join("home");
        let s = settings_with(&[]).with_directory(&blocker.join("sub"));
        // 配置目录在文件之下，创建必然失败，应回退到 home/Pictures
        let ok = save_automatic(
            &s,
            &ExportOverrides::default(),
            3,
            2,
            &image(),
            Some(&home),
            golden_time(),
        )
        .unwrap();
        assert!(ok.starts_with(home.join("Pictures")), "{ok:?}");
        // 全部候选都不可用：后备也放在文件之下
        let bad_home = blocker.join("home");
        let err = save_automatic(
            &s,
            &ExportOverrides::default(),
            3,
            2,
            &image(),
            Some(&bad_home),
            golden_time(),
        )
        .unwrap_err();
        assert!(
            matches!(err, ExportError::CreateDir(_) | ExportError::Write(_)),
            "{err:?}"
        );
        // 模板展开后含路径分隔符
        // 配置层会拒绝带分隔符的模板，这里直接构造快照，验证导出层的第二道防线
        let bad_name = ExportSettings {
            auto_filename_format: "a/b".into(),
            ..settings_with(&[])
        };
        assert_eq!(
            save_automatic(
                &bad_name,
                &ExportOverrides::default(),
                3,
                2,
                &image(),
                None,
                golden_time()
            )
            .unwrap_err(),
            ExportError::InvalidFileName
        );
        // 缓冲不合法
        let dir = root.join("ok");
        let good = settings_with(&[]).with_directory(&dir);
        let err = save_automatic(
            &good,
            &ExportOverrides::default(),
            3,
            2,
            &[0; 4],
            None,
            golden_time(),
        )
        .unwrap_err();
        assert!(matches!(err, ExportError::Encode(_)));
        let _ = std::fs::remove_dir_all(&root);
    }

    /// 指定路径保存：扩展名被改成与格式一致，目录自动创建；目录不可写时报错。
    #[test]
    fn save_to_path_normalizes_and_reports_errors() {
        let root = temp_dir("topath");
        let _ = std::fs::remove_dir_all(&root);
        let s = settings_with(&[]);
        let enc = s.encode_settings(&ExportOverrides::default(), "", golden_time());
        let target = root.join("deep").join("pic.jpg");
        let saved = save_to_path(
            &target.to_string_lossy(),
            ExportFormat::Png,
            3,
            2,
            &image(),
            &enc,
        )
        .unwrap();
        assert_eq!(saved.file_name().unwrap(), "pic.png");
        assert!(std::fs::read(&saved).unwrap().starts_with(b"\x89PNG"));
        assert_eq!(
            save_to_path("  ", ExportFormat::Png, 3, 2, &image(), &enc).unwrap_err(),
            ExportError::NoPath
        );
        // 父目录被同名文件占用
        let blocker = root.join("file");
        std::fs::write(&blocker, b"x").unwrap();
        let err = save_to_path(
            &blocker.join("a.png").to_string_lossy(),
            ExportFormat::Png,
            3,
            2,
            &image(),
            &enc,
        )
        .unwrap_err();
        assert!(matches!(err, ExportError::CreateDir(_)), "{err:?}");
        let _ = std::fs::remove_dir_all(&root);
    }

    /// 另存为对话框初始状态：记住的目录须存在，否则取首选候选；格式取上次；文件名按手动模板。
    #[test]
    fn dialog_plan_rules() {
        let root = temp_dir("plan");
        let remembered = root.join("remembered");
        std::fs::create_dir_all(&remembered).unwrap();
        let s = settings_with(&[
            (
                MANUAL_FILENAME_CONFIG_KEY,
                Value::String("Manual_{yyyyMMdd}".into()),
            ),
            (LAST_MANUAL_FORMAT_CONFIG_KEY, Value::String("jpeg".into())),
            (
                LAST_MANUAL_DIRECTORY_CONFIG_KEY,
                Value::String(remembered.to_string_lossy().to_string()),
            ),
            (
                SAVE_DIRECTORY_CONFIG_KEY,
                Value::String("D:/configured".into()),
            ),
        ]);
        let plan = dialog_plan(&s, None, golden_time());
        assert_eq!(
            plan.initial_dir,
            PathBuf::from(crate::export_naming::clean_path(
                &remembered.to_string_lossy()
            ))
        );
        assert_eq!(plan.file_name, "Manual_20260814.jpg");
        assert_eq!(plan.format, ExportFormat::Jpeg);
        let missing = root.join("missing");
        let s = ExportSettings {
            last_manual_directory: missing.to_string_lossy().to_string(),
            ..s
        };
        assert_eq!(
            dialog_plan(&s, None, golden_time()).initial_dir,
            PathBuf::from("D:/configured")
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    /// 对话框请求：过滤项顺序、初始选中项、标题与过滤名按语言取得。
    #[test]
    fn dialog_request_localized() {
        let plan = DialogPlan {
            initial_dir: PathBuf::from("D:/x"),
            file_name: "a.jpg".into(),
            format: ExportFormat::Jpeg,
        };
        let en = dialog_request(&plan, "en-US", Some(7));
        assert_eq!(en.filters.len(), ExportFormat::ALL.len());
        assert_eq!(en.filter_index, 1);
        assert_eq!(en.owner, Some(7));
        assert_eq!(en.filters[0].label, "PNG image (*.png)");
        assert_eq!(en.filters[1].pattern, "*.jpg;*.jpeg");
        assert_eq!(en.title, "Save screenshot");
        let zh = dialog_request(&plan, "zh-CN", None);
        assert_eq!(zh.title, "保存截图");
        assert_ne!(zh.filters[0].label, en.filters[0].label);
    }

    /// 对照 Qt `formatForDialogSelection`：路径后缀优先，其次所选过滤项，最后 PNG。
    #[test]
    fn dialog_choice_format_rules() {
        assert_eq!(format_for_choice("capture.unknown", 1), ExportFormat::Jpeg);
        assert_eq!(format_for_choice("capture.bmp", 0), ExportFormat::Bmp);
        assert_eq!(format_for_choice("capture", 4), ExportFormat::Pdf);
        assert_eq!(format_for_choice("capture.PDF", 0), ExportFormat::Pdf);
        assert_eq!(format_for_choice("capture", 99), ExportFormat::Png);
    }

    /// 手动保存后记住目录与格式：写盘、幂等。
    #[test]
    fn remember_manual_save_persists() {
        let root = temp_dir("remember");
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let mut store = ConfigStore::open(root.join("config.json"));
        let saved = root.join("shots").join("a.webp");
        std::fs::create_dir_all(saved.parent().unwrap()).unwrap();
        remember_manual_save(&mut store, &saved, ExportFormat::Webp).unwrap();
        assert_eq!(
            store.value(LAST_MANUAL_FORMAT_CONFIG_KEY),
            Value::String("webp".into())
        );
        let dir = store.value(LAST_MANUAL_DIRECTORY_CONFIG_KEY);
        assert!(dir.as_str().unwrap().ends_with("shots"), "{dir:?}");
        assert!(!store.is_dirty());
        let reloaded = ConfigStore::open(root.join("config.json"));
        assert_eq!(
            reloaded.value(LAST_MANUAL_FORMAT_CONFIG_KEY),
            Value::String("webp".into())
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    /// 错误文案：两种语言都能取到，且带原因；缺失的消息 id 会被这里发现。
    #[test]
    fn error_messages_exist_in_both_locales() {
        let errors = [
            ExportError::NoPath,
            ExportError::InvalidFileName,
            ExportError::NoFolder,
            ExportError::CreateDir("x".into()),
            ExportError::Encode("boom".into()),
        ];
        for locale in ["en-US", "zh-CN"] {
            for e in &errors {
                for text in [
                    e.message(locale),
                    e.manual_message(locale),
                    e.auto_message(locale),
                ] {
                    assert!(
                        !text.is_empty() && !text.contains("[!"),
                        "{locale} {e:?}: {text}"
                    );
                }
            }
        }
        assert!(
            ExportError::Encode("boom".into())
                .manual_message("en-US")
                .contains("boom")
        );
        assert_ne!(
            ExportError::NoFolder.message("en-US"),
            ExportError::NoFolder.message("zh-CN")
        );
        assert!(ExportError::Write("w".into()).to_string().contains("w"));
    }

    /// 手动保存任务：指定路径模式按路径后缀选格式并不记忆；自动模式走自动规则；错误带本地化提示。
    #[test]
    fn manual_job_path_and_automatic_modes() {
        let root = temp_dir("job");
        let _ = std::fs::remove_dir_all(&root);
        let base = ManualSaveJob {
            settings: settings_with(&[]).with_directory(&root.join("auto")),
            overrides: ExportOverrides::default(),
            mode: SaveMode::Path(
                root.join("x")
                    .join("pic.jpeg")
                    .to_string_lossy()
                    .to_string(),
            ),
            locale: "en-US".into(),
            owner: None,
            home: None,
            now: golden_time(),
        };
        let done = base.run(3, 2, &image()).unwrap();
        assert_eq!(done.format, ExportFormat::Jpeg);
        assert!(!done.remember);
        let SaveOutcome::Saved(path) = done.outcome else {
            panic!("应已保存")
        };
        assert_eq!(path.file_name().unwrap(), "pic.jpg");
        assert!(std::fs::read(&path).unwrap().starts_with(&[0xFF, 0xD8]));

        let auto = ManualSaveJob {
            mode: SaveMode::Automatic,
            ..base.clone()
        };
        let done = auto.run(3, 2, &image()).unwrap();
        let SaveOutcome::Saved(path) = done.outcome else {
            panic!("应已保存")
        };
        assert!(path.starts_with(root.join("auto")));

        let bad = ManualSaveJob {
            mode: SaveMode::Path("  ".into()),
            ..base.clone()
        };
        let message = bad.run(3, 2, &image()).unwrap_err();
        assert!(
            message.contains("The screenshot could not be saved"),
            "{message}"
        );
        let zh = ManualSaveJob {
            mode: SaveMode::Path("  ".into()),
            locale: "zh-CN".into(),
            ..base
        };
        assert!(
            !zh.run(3, 2, &image())
                .unwrap_err()
                .contains("The screenshot")
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    /// 直接截图：复制可附带自动保存；保存支持指定路径与自动路径；剪贴板失败向上报告。
    #[test]
    fn direct_export_modes() {
        use snow_app_core::command::DirectTarget;
        let root = temp_dir("direct");
        let _ = std::fs::remove_dir_all(&root);
        let request = |output, path: Option<String>| DirectCaptureRequest {
            target: DirectTarget::CurrentMonitor,
            output,
            capture_cursor: None,
            scale: None,
            path,
            automatic_path: None,
            format: None,
            quality: None,
            compression_level: None,
            pdf_page_size: None,
            pdf_title: None,
        };
        let copies = std::cell::Cell::new(0usize);
        let mut copy = |w: u32, h: u32, _: &[u8]| -> Result<(), String> {
            let _ = (w, h);
            copies.set(copies.get() + 1);
            Ok(())
        };
        // 复制，未开自动保存：不落盘
        let plain = settings_with(&[]).with_directory(&root);
        let done = export_direct(
            &plain,
            &request(DirectOutput::Copy, None),
            3,
            2,
            &image(),
            None,
            golden_time(),
            &mut copy,
        )
        .unwrap();
        assert!(done.copied && done.saved.is_none() && done.auto_save_error.is_none());
        // 复制并自动保存
        let auto = settings_with(&[(AUTO_SAVE_AFTER_COPY_CONFIG_KEY, Value::Bool(true))])
            .with_directory(&root);
        let done = export_direct(
            &auto,
            &request(DirectOutput::Copy, None),
            3,
            2,
            &image(),
            None,
            golden_time(),
            &mut copy,
        )
        .unwrap();
        assert!(done.copied && done.saved.as_ref().is_some_and(|p| p.exists()));
        // 保存到指定路径，格式取自后缀
        let target = root.join("direct.webp").to_string_lossy().to_string();
        let done = export_direct(
            &plain,
            &request(DirectOutput::Save, Some(target)),
            3,
            2,
            &image(),
            None,
            golden_time(),
            &mut copy,
        )
        .unwrap();
        let saved = done.saved.unwrap();
        assert_eq!(saved.file_name().unwrap(), "direct.webp");
        assert!(std::fs::read(&saved).unwrap().starts_with(b"RIFF"));
        // 保存到自动路径
        let done = export_direct(
            &plain,
            &request(DirectOutput::Save, None),
            3,
            2,
            &image(),
            None,
            golden_time(),
            &mut copy,
        )
        .unwrap();
        assert!(done.saved.unwrap().starts_with(&root));
        assert_eq!(copies.get(), 2);
        // 剪贴板失败
        let mut failing = |_: u32, _: u32, _: &[u8]| -> Result<(), String> { Err("busy".into()) };
        let err = export_direct(
            &plain,
            &request(DirectOutput::Copy, None),
            3,
            2,
            &image(),
            None,
            golden_time(),
            &mut failing,
        )
        .unwrap_err();
        assert_eq!(err, ExportError::Clipboard("busy".into()));
        // 复制后自动保存失败只记在结果里，复制仍算成功
        let bad = ExportSettings {
            auto_filename_format: "a/b".into(),
            auto_save_after_copy: true,
            ..settings_with(&[])
        };
        let done = export_direct(
            &bad,
            &request(DirectOutput::Copy, None),
            3,
            2,
            &image(),
            None,
            golden_time(),
            &mut copy,
        )
        .unwrap();
        assert!(done.copied && done.auto_save_error == Some(ExportError::InvalidFileName));
        // Render 不在这里处理
        assert!(
            export_direct(
                &plain,
                &request(DirectOutput::Render, None),
                3,
                2,
                &image(),
                None,
                golden_time(),
                &mut copy
            )
            .is_err()
        );
        let _ = std::fs::remove_dir_all(&root);
    }
}
