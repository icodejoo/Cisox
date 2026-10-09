//! 推荐模型清单：各功能（OCR / 翻译 / 语音 / 表格 / 公式）的下载入口数据、模型目录与“已下载”判定。
//!
//! 清单是内置 JSON（`resources/model-catalog.json`），新增条目只改数据、不改逻辑。
//! 模型就是目录里的文件或文件夹，显示名 = 文件（夹）名。下载由用户用浏览器完成并自行解压，
//! 应用内下载器（OCR / 语音 / Hy-MT2）不受影响。
//!
//! 统一目录：`<数据根>/models/<功能>/`（功能目录名见 [`Feature::dir_name`]）。公式里用户自设的目录优先。

use crate::ocr_assets::ocr_root;
use crate::table_assets::table_root;
use serde::Deserialize;
use snow_i18n::I18n;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

/// 内置清单。
const CATALOG_JSON: &str = include_str!("../resources/model-catalog.json");
/// 统一模型目录名（位于数据根下，其下每个功能一个子目录）。
pub const MODELS_DIR: &str = "models";
/// 环境变量覆盖 OCR / 表格资产根时，模型所在的子目录名（`<覆盖目录>/models`）。
const OVERRIDE_MODELS_DIR: &str = "models";
/// 清单里表示“许可未核对”的许可串。
const LICENSE_UNVERIFIED: &str = "unverified";
/// 一 MiB 的字节数。
const MIB: f64 = 1024.0 * 1024.0;
/// 一 GiB 的字节数。
const GIB: f64 = MIB * 1024.0;
/// 目录扫描时忽略的未完成下载后缀。
const TEMP_SUFFIXES: [&str; 3] = [".part", ".tmp", ".partial"];

/// 能提供模型的功能。
#[derive(Debug, Clone, Copy, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "lowercase")]
pub enum Feature {
    /// 文字识别。
    Ocr,
    /// 截图翻译。
    Translate,
    /// 语音转文字。
    Stt,
    /// 表格识别。
    Table,
    /// 公式识别。
    Latex,
}

impl Feature {
    /// 该功能在统一模型目录下的子目录名。
    pub fn dir_name(self) -> &'static str {
        match self {
            Feature::Ocr => "ocr",
            Feature::Translate => "translate",
            Feature::Stt => "stt",
            Feature::Table => "table",
            Feature::Latex => "latex",
        }
    }

    /// 功能标题的 i18n 键（弹窗标题用）。
    pub fn title_key(self) -> &'static str {
        match self {
            Feature::Ocr => "model-catalog-title-ocr",
            Feature::Translate => "model-catalog-title-translate",
            Feature::Stt => "model-catalog-title-stt",
            Feature::Table => "model-catalog-title-table",
            Feature::Latex => "model-catalog-title-latex",
        }
    }
}

/// 条目附带的单个文件下载地址（给想逐个下载的人看；界面暂不展示）。
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct ExtraFile {
    /// 文件名。
    pub name: String,
    /// 直链；上游只给了发布页时为空。
    #[serde(default)]
    pub url: String,
    /// 文件字节数。
    #[serde(default)]
    pub size_bytes: u64,
}

/// 条目 `file_name` 相对哪个目录判定“已下载”。
#[derive(Debug, Clone, Copy, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Anchor {
    /// 该功能的模型目录（统一目录及其回退目录）。
    #[default]
    Models,
    /// OCR 资产根（运行时不在统一目录范围内，仍在 `assets/ocr/runtimes`）。
    OcrRuntime,
}

/// 清单条目。
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct CatalogEntry {
    /// 所属功能。
    pub feature: Feature,
    /// 条目 ID。
    pub id: String,
    /// 显示名（型号名，不翻译）。
    pub name: String,
    /// 用于判定“已下载”的文件（夹）名，相对 [`Anchor`] 指的目录。
    pub file_name: String,
    /// `file_name` 相对哪个目录。
    #[serde(default)]
    pub anchor: Anchor,
    /// 与 `file_name` 等价的其它名字（如解压后带 `-pack` 后缀的文件夹）。
    #[serde(default)]
    pub aliases: Vec<String>,
    /// 还必须同时存在的同级文件（多文件模型用，全部在才算已下载）。
    #[serde(default)]
    pub also_files: Vec<String>,
    /// 下载大小（字节）；0 表示未知。
    #[serde(default)]
    pub size_bytes: u64,
    /// 大小只是文档里的约数（显示时带“约”）。
    #[serde(default)]
    pub size_approx: bool,
    /// 许可证；`unverified` 表示尚未核对。
    pub license: String,
    /// 下载地址（用系统浏览器打开）。
    pub url: String,
    /// 一句话说明的 i18n 键。
    pub note_key: String,
    /// 是否为该功能的默认推荐。
    #[serde(default)]
    pub default: bool,
    /// 逐文件下载地址。
    #[serde(default)]
    pub extra: Vec<ExtraFile>,
}

/// 清单文件的根。
#[derive(Debug, Deserialize)]
struct CatalogFile {
    /// 条目列表。
    entries: Vec<CatalogEntry>,
}

/// 解析内置清单（只解析一次）。
///
/// # 返回
/// 全部条目；内置 JSON 损坏时返回解析器给出的原因（技术信息，不翻译）。
///
/// # 示例
/// ```ignore
/// assert!(!entries().unwrap().is_empty());
/// ```
pub fn entries() -> Result<&'static [CatalogEntry], String> {
    static CELL: OnceLock<Result<Vec<CatalogEntry>, String>> = OnceLock::new();
    CELL.get_or_init(|| {
        serde_json::from_str::<CatalogFile>(CATALOG_JSON)
            .map(|f| f.entries)
            .map_err(|e| e.to_string())
    })
    .as_ref()
    .map(Vec::as_slice)
    .map_err(Clone::clone)
}

/// 某功能的全部条目（保持清单顺序）。
///
/// # 参数
/// - `feature`：功能。
///
/// # 返回
/// 条目引用列表；清单损坏时为空。
pub fn entries_for(feature: Feature) -> Vec<&'static CatalogEntry> {
    entries()
        .map(|all| all.iter().filter(|e| e.feature == feature).collect())
        .unwrap_or_default()
}

/// 统一模型目录：`<数据根>/models/<功能>`。
///
/// # 参数
/// - `data_root`：应用数据根目录。
/// - `feature`：功能。
///
/// # 示例
/// ```ignore
/// assert!(unified_dir(Path::new("D"), Feature::Stt).ends_with("stt"));
/// ```
pub fn unified_dir(data_root: &Path, feature: Feature) -> PathBuf {
    data_root.join(MODELS_DIR).join(feature.dir_name())
}

/// 环境变量值是否算“已设置”（去空白后非空）。
fn env_set(value: Option<&str>) -> bool {
    value.is_some_and(|v| !v.trim().is_empty())
}

/// 某功能读取模型的目录列表：第一项是主目录（下载落盘、打开文件夹、界面显示的都是它）；公式自设目录时后面多一个统一目录作回退。
///
/// # 参数
/// - `feature`：功能。
/// - `data_root`：应用数据根目录。
/// - `latex_dir`：配置里的公式模型目录（空白表示没设置）。
/// - `ocr_env`：`SNOW_OCR_ASSET_DIR` 的值（设置后只用 `<该目录>/models`，保持原覆盖行为）。
/// - `table_env`：`SNOW_TABLE_ASSET_DIR` 的值（同上）。
///
/// # 返回
/// 非空目录列表（目录本身可能尚不存在）。
///
/// # 示例
/// ```ignore
/// let dirs = read_dirs(Feature::Ocr, Path::new("D"), "", None, None);
/// assert_eq!(dirs.len(), 1); // 只有统一目录 models/ocr
/// ```
pub fn read_dirs(
    feature: Feature,
    data_root: &Path,
    latex_dir: &str,
    ocr_env: Option<&str>,
    table_env: Option<&str>,
) -> Vec<PathBuf> {
    let unified = unified_dir(data_root, feature);
    // 环境变量覆盖资产根时只认 `<覆盖目录>/models`，否则只认统一目录
    let by_env = |env: Option<&str>, root: PathBuf| {
        if env_set(env) {
            vec![root.join(OVERRIDE_MODELS_DIR)]
        } else {
            vec![unified.clone()]
        }
    };
    match feature {
        Feature::Ocr => by_env(ocr_env, ocr_root(data_root, ocr_env)),
        Feature::Table => by_env(table_env, table_root(data_root, table_env)),
        Feature::Latex => match latex_dir.trim() {
            "" => vec![unified],
            custom => vec![PathBuf::from(custom), unified],
        },
        Feature::Translate | Feature::Stt => vec![unified],
    }
}

/// 一次判定 / 扫描要用的目录集合。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelDirs {
    /// 模型目录列表（主目录在前，见 [`read_dirs`]）。
    pub read: Vec<PathBuf>,
    /// OCR 资产根（判定运行时条目用）。
    pub ocr_root: PathBuf,
}

impl ModelDirs {
    /// 按功能与当前环境组装目录集合。
    ///
    /// # 参数
    /// 同 [`read_dirs`]。
    pub fn resolve(
        feature: Feature,
        data_root: &Path,
        latex_dir: &str,
        ocr_env: Option<&str>,
        table_env: Option<&str>,
    ) -> Self {
        Self {
            read: read_dirs(feature, data_root, latex_dir, ocr_env, table_env),
            ocr_root: ocr_root(data_root, ocr_env),
        }
    }

    /// 主目录（下载落盘、“打开文件夹”、界面显示用）。
    pub fn primary(&self) -> &Path {
        self.read.first().map_or(Path::new(""), PathBuf::as_path)
    }
}

/// 条目在某个目录里是否齐全：主文件（夹）或任一别名存在，且所有同级附加文件都在。
fn present_in(dir: &Path, entry: &CatalogEntry) -> bool {
    let main_found = std::iter::once(&entry.file_name)
        .chain(entry.aliases.iter())
        .any(|name| dir.join(name).exists());
    main_found && entry.also_files.iter().all(|name| dir.join(name).exists())
}

/// 条目是否已下载：在主目录或任一回退目录里齐全即算。
///
/// # 参数
/// - `dirs`：目录集合。
/// - `entry`：清单条目。
///
/// # 返回
/// 已下载返回 `true`。
pub fn is_downloaded(dirs: &ModelDirs, entry: &CatalogEntry) -> bool {
    match entry.anchor {
        Anchor::Models => dirs.read.iter().any(|dir| present_in(dir, entry)),
        Anchor::OcrRuntime => present_in(&dirs.ocr_root, entry),
    }
}

/// 扫描时哪些条目算一个模型。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScanKind {
    /// 文件夹与文件各算一个模型。
    Any,
    /// 只有文件夹算模型（翻译包、OCR 的 det/rec/dict 组都是文件夹）。
    FoldersOnly,
}

/// 扫描模型目录，返回里面的模型名（一个文件夹 = 一个模型，一个文件 = 一个模型；显示名就是文件（夹）名）。
///
/// # 参数
/// - `dirs`：要扫的目录（多个目录的结果合并、去重）；不存在或读取失败的视为空。
/// - `kind`：哪些条目算模型。
///
/// # 返回
/// 按名字排序的名称列表；隐藏项（`.` 开头）与未完成下载的临时文件不计。
///
/// # 示例
/// ```ignore
/// let names = scan_models(&[PathBuf::from("D:/data/models/stt")], ScanKind::Any);
/// ```
pub fn scan_models(dirs: &[PathBuf], kind: ScanKind) -> Vec<String> {
    let mut names: Vec<String> = dirs
        .iter()
        .filter_map(|dir| std::fs::read_dir(dir).ok())
        .flatten()
        .filter_map(Result::ok)
        .filter(|entry| match kind {
            ScanKind::Any => true,
            ScanKind::FoldersOnly => entry.file_type().is_ok_and(|ty| ty.is_dir()),
        })
        .filter_map(|entry| entry.file_name().into_string().ok())
        .filter(|name| {
            !name.starts_with('.') && !TEMP_SUFFIXES.iter().any(|suffix| name.ends_with(suffix))
        })
        .collect();
    names.sort();
    names.dedup();
    names
}

/// “默认模型”下拉的一个选项。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelOption {
    /// 写入配置的值。
    pub value: String,
    /// 显示名（文件夹名；取不到时为空）。
    pub label: String,
}

/// 翻译“默认模型”下拉的选项：扫描模型目录里的所有文件夹（一个文件夹 = 一个模型），显示名是文件夹名。
///
/// 配置里存的是模型包清单里的 ID：文件夹里有可用的 `model.json` 时值取它的 ID，否则退回文件夹名。
///
/// # 参数
/// - `dir`：翻译模型目录；不存在视为空。
///
/// # 返回
/// 按文件夹名排序的选项。
///
/// # 示例
/// ```ignore
/// let options = translate_options(Path::new("D:/data/models/translate"));
/// ```
pub fn translate_options(dir: &Path) -> Vec<ModelOption> {
    let ids: std::collections::HashMap<String, String> = snow_translate::ModelScanner::new(dir)
        .scan()
        .models
        .into_iter()
        .filter_map(|m| {
            let folder = m.dir.file_name()?.to_string_lossy().into_owned();
            Some((folder, m.manifest.id))
        })
        .collect();
    scan_models(&[dir.to_path_buf()], ScanKind::FoldersOnly)
        .into_iter()
        .map(|folder| ModelOption {
            value: ids.get(&folder).cloned().unwrap_or_else(|| folder.clone()),
            label: folder,
        })
        .collect()
}

/// 把字节数格式化成 `MiB` / `GiB` 文本；0 视为未知，返回 `None`。
///
/// # 示例
/// ```ignore
/// assert_eq!(format_size(0), None);
/// assert_eq!(format_size(7_758_305).as_deref(), Some("7.4 MiB"));
/// ```
pub fn format_size(bytes: u64) -> Option<String> {
    if bytes == 0 {
        return None;
    }
    let size = bytes as f64;
    Some(if size >= GIB {
        format!("{:.1} GiB", size / GIB)
    } else {
        format!("{:.1} MiB", size / MIB)
    })
}

/// 弹窗里的一行（已按语言整理好的展示数据）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CatalogRow {
    /// 型号名。
    pub name: String,
    /// 大小文本；未知时为本地化的“大小未知”。
    pub size: String,
    /// 许可文本；未核对时为本地化提示。
    pub license: String,
    /// 一句话说明。
    pub note: String,
    /// 下载地址。
    pub url: String,
    /// 是否为默认推荐。
    pub default: bool,
    /// 是否已在模型目录里找到。
    pub downloaded: bool,
}

/// 生成某功能的弹窗行。
///
/// # 参数
/// - `feature`：功能。
/// - `dirs`：该功能的目录集合（用来判定已下载）。
/// - `i18n`：当前语言的文案。
///
/// # 返回
/// 与清单顺序一致的行。
pub fn build_rows(feature: Feature, dirs: &ModelDirs, i18n: &I18n) -> Vec<CatalogRow> {
    entries_for(feature)
        .into_iter()
        .map(|entry| CatalogRow {
            name: entry.name.clone(),
            size: match format_size(entry.size_bytes) {
                None => i18n.tr("model-catalog-size-unknown"),
                Some(size) if entry.size_approx => i18n.tr_with(
                    "model-catalog-size-approx",
                    &snow_i18n::Args::new().named("size", size),
                ),
                Some(size) => size,
            },
            license: if entry.license == LICENSE_UNVERIFIED {
                i18n.tr("model-catalog-license-unverified")
            } else {
                entry.license.clone()
            },
            note: i18n.tr(&entry.note_key),
            url: entry.url.clone(),
            default: entry.default,
            downloaded: is_downloaded(dirs, entry),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 建一个独立的临时目录。
    fn temp_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "cisox-model-catalog-{tag}-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("建目录");
        dir
    }

    /// 内置清单可解析，五个功能都有条目，且各功能至多一个默认以外的字段齐全。
    #[test]
    fn embedded_catalog_parses_and_covers_features() {
        let all = entries().expect("清单应可解析");
        for feature in [
            Feature::Ocr,
            Feature::Translate,
            Feature::Stt,
            Feature::Table,
            Feature::Latex,
        ] {
            assert!(!entries_for(feature).is_empty(), "{feature:?} 没有条目");
        }
        assert_eq!(entries_for(Feature::Translate).len(), 5);
        assert_eq!(entries_for(Feature::Stt).len(), 14, "13 个模型 + VAD");
        for e in all {
            assert!(!e.file_name.is_empty() && !e.id.is_empty() && !e.note_key.is_empty());
            assert!(
                snow_platform::shell::web_link(&e.url).is_some(),
                "{} 的地址不合格",
                e.id
            );
        }
        let ids: std::collections::HashSet<_> = all.iter().map(|e| &e.id).collect();
        assert_eq!(ids.len(), all.len(), "条目 ID 不能重复");
    }

    /// 每个条目的说明键在所有语言里都存在，大小 / 许可提示键也在。
    #[test]
    fn notes_exist_in_every_locale() {
        for info in snow_i18n::locales() {
            let i18n = crate::ocr_backend::i18n_for(info.code);
            for key in [
                "model-catalog-size-unknown",
                "model-catalog-size-approx",
                "model-catalog-license-unverified",
            ] {
                assert!(i18n.has(key), "{} 缺 {key}", info.code);
            }
            for e in entries().expect("清单") {
                assert!(i18n.has(&e.note_key), "{} 缺 {}", info.code, e.note_key);
            }
        }
    }

    /// 只含一个目录的目录集合（测试用）。
    fn dirs_of(dir: &Path) -> ModelDirs {
        ModelDirs {
            read: vec![dir.to_path_buf()],
            ocr_root: dir.to_path_buf(),
        }
    }

    /// 单文件 / 文件夹条目：主名或别名存在即已下载。
    #[test]
    fn downloaded_by_name_or_alias() {
        let dir = temp_dir("alias");
        let entry = entries_for(Feature::Translate)[0];
        assert!(!is_downloaded(&dirs_of(&dir), entry));
        std::fs::create_dir_all(dir.join(&entry.aliases[0])).expect("建别名目录");
        assert!(is_downloaded(&dirs_of(&dir), entry));
        std::fs::remove_dir_all(&dir).ok();
        let dir = temp_dir("main");
        std::fs::create_dir_all(dir.join(&entry.file_name)).expect("建目录");
        assert!(is_downloaded(&dirs_of(&dir), entry));
        std::fs::remove_dir_all(&dir).ok();
    }

    /// 公式模型要四个文件都在才算已下载。
    #[test]
    fn latex_needs_all_four_files() {
        let dir = temp_dir("latex");
        let entry = entries_for(Feature::Latex)[0];
        let names = [
            "image_resizer.onnx",
            "encoder.onnx",
            "decoder.onnx",
            "tokenizer.json",
        ];
        for name in names {
            assert!(!is_downloaded(&dirs_of(&dir), entry), "缺文件时不应已下载");
            std::fs::write(dir.join(name), b"x").expect("写");
        }
        assert!(is_downloaded(&dirs_of(&dir), entry));
        std::fs::remove_file(dir.join("decoder.onnx")).expect("删");
        assert!(!is_downloaded(&dirs_of(&dir), entry));
        std::fs::remove_dir_all(&dir).ok();
    }

    /// 表格模型：文件夹或单个 onnx 文件都认。
    #[test]
    fn table_accepts_folder_or_file() {
        let dir = temp_dir("table");
        let entry = entries_for(Feature::Table)[0];
        std::fs::write(dir.join("slanet-plus.onnx"), b"x").expect("写");
        assert!(is_downloaded(&dirs_of(&dir), entry));
        std::fs::remove_dir_all(&dir).ok();
    }

    /// 表格只认统一目录；OCR 运行时按资产根判定。
    #[test]
    fn unified_only_and_runtime_anchor() {
        let root = temp_dir("anchor");
        let table = entries_for(Feature::Table)[0];
        let dirs = ModelDirs::resolve(Feature::Table, &root, "", None, None);
        assert!(!is_downloaded(&dirs, table));
        let old = root.join("assets").join("table").join("models");
        std::fs::create_dir_all(&old).expect("建");
        std::fs::write(old.join("slanet-plus.onnx"), b"x").expect("写");
        assert!(!is_downloaded(&dirs, table), "旧位置不再读取");
        let unified = root.join("models").join("table");
        std::fs::create_dir_all(&unified).expect("建");
        std::fs::write(unified.join("slanet-plus.onnx"), b"x").expect("写");
        assert!(is_downloaded(&dirs, table));
        let runtime = entries_for(Feature::Ocr)
            .into_iter()
            .find(|e| e.anchor == Anchor::OcrRuntime)
            .expect("运行时条目");
        let ocr_dirs = ModelDirs::resolve(Feature::Ocr, &root, "", None, None);
        assert!(!is_downloaded(&ocr_dirs, runtime));
        std::fs::create_dir_all(root.join("assets/ocr").join(&runtime.file_name)).expect("建");
        assert!(is_downloaded(&ocr_dirs, runtime));
        std::fs::remove_dir_all(&root).ok();
    }

    /// 扫描：只返回文件（夹）名，排序去重，忽略隐藏项与未完成下载，目录不存在为空；只要文件夹时不含文件。
    #[test]
    fn scan_uses_file_names_only() {
        let dir = temp_dir("scan");
        let other = temp_dir("scan-other");
        std::fs::create_dir_all(dir.join("b-model")).expect("建");
        std::fs::create_dir_all(other.join("b-model")).expect("建");
        std::fs::write(dir.join("a.onnx"), b"x").expect("写");
        std::fs::write(dir.join(".hidden"), b"x").expect("写");
        std::fs::write(dir.join("c.onnx.part"), b"x").expect("写");
        let both = [dir.clone(), other.clone(), dir.join("missing")];
        assert_eq!(scan_models(&both, ScanKind::Any), vec!["a.onnx", "b-model"]);
        assert_eq!(scan_models(&both, ScanKind::FoldersOnly), vec!["b-model"]);
        assert!(scan_models(&[dir.join("missing")], ScanKind::Any).is_empty());
        std::fs::remove_dir_all(&dir).ok();
        std::fs::remove_dir_all(&other).ok();
    }

    /// 翻译下拉选项：只认文件夹，显示名是文件夹名，值优先取 model.json 里的 ID。
    #[test]
    fn translate_options_use_folder_names() {
        let dir = temp_dir("topts");
        std::fs::create_dir_all(dir.join("plain-folder")).expect("建");
        std::fs::create_dir_all(dir.join("with-manifest-pack")).expect("建");
        std::fs::write(dir.join("loose.zip"), b"x").expect("写");
        let options = translate_options(&dir);
        let labels: Vec<_> = options.iter().map(|o| o.label.as_str()).collect();
        assert_eq!(
            labels,
            vec!["plain-folder", "with-manifest-pack"],
            "文件不算模型"
        );
        assert!(
            options.iter().all(|o| o.value == o.label),
            "没有清单时值就是文件夹名"
        );
        assert!(translate_options(&dir.join("missing")).is_empty());
        std::fs::remove_dir_all(&dir).ok();
    }

    /// 统一目录：各功能都是 `<数据根>/models/<功能>`，且与翻译 / 语音现有读取目录一致。
    #[test]
    fn unified_dirs_match_readers() {
        let root = Path::new("D:/data");
        for feature in [
            Feature::Ocr,
            Feature::Translate,
            Feature::Stt,
            Feature::Table,
            Feature::Latex,
        ] {
            assert_eq!(
                unified_dir(root, feature),
                root.join("models").join(feature.dir_name())
            );
        }
        assert_eq!(
            unified_dir(root, Feature::Translate),
            crate::translate_service::default_models_dir(root)
        );
        assert_eq!(
            unified_dir(root, Feature::Stt),
            crate::stt_models::models_root(root)
        );
    }

    /// 读取目录：OCR / 表格只有统一目录；环境变量覆盖只认覆盖目录；公式自设目录优先、统一目录回退。
    #[test]
    fn read_dirs_order_and_overrides() {
        let root = Path::new("D:/data");
        assert_eq!(
            read_dirs(Feature::Ocr, root, "", None, None),
            vec![root.join("models").join("ocr")]
        );
        assert_eq!(
            read_dirs(Feature::Ocr, root, "", Some("E:/ocr"), None),
            vec![Path::new("E:/ocr").join("models")]
        );
        assert_eq!(
            read_dirs(Feature::Ocr, root, "", Some("  "), None),
            vec![root.join("models").join("ocr")],
            "空白环境变量视为未设置"
        );
        assert_eq!(
            read_dirs(Feature::Table, root, "", None, None),
            vec![root.join("models").join("table")]
        );
        assert_eq!(
            read_dirs(Feature::Table, root, "", None, Some("E:/t")),
            vec![Path::new("E:/t").join("models")]
        );
        assert_eq!(
            read_dirs(Feature::Latex, root, "  ", None, None),
            vec![root.join("models").join("latex")]
        );
        assert_eq!(
            read_dirs(Feature::Latex, root, " E:/tex ", None, None),
            vec![PathBuf::from("E:/tex"), root.join("models").join("latex")]
        );
        let dirs = ModelDirs::resolve(Feature::Stt, root, "", None, None);
        assert_eq!(dirs.primary(), root.join("models").join("stt"));
    }

    /// 大小格式：0 未知，MiB / GiB 一位小数。
    #[test]
    fn size_formatting() {
        assert_eq!(format_size(0), None);
        assert_eq!(format_size(7_758_305).as_deref(), Some("7.4 MiB"));
        assert_eq!(
            format_size(3 * 1024 * 1024 * 1024).as_deref(),
            Some("3.0 GiB")
        );
    }

    /// 行数据：已下载标记跟目录内容走，大小未知与许可未核对用本地化提示。
    #[test]
    fn rows_reflect_directory() {
        let dir = temp_dir("rows");
        std::fs::write(dir.join("silero_vad.onnx"), b"x").expect("写");
        let i18n = crate::ocr_backend::i18n_for("en-US");
        let rows = build_rows(Feature::Stt, &dirs_of(&dir), i18n);
        assert_eq!(rows.len(), 14);
        let vad = rows
            .iter()
            .find(|r| r.name == "Silero VAD")
            .expect("VAD 行");
        assert!(vad.downloaded);
        assert_eq!(rows.iter().filter(|r| r.downloaded).count(), 1);
        assert!(
            rows.iter()
                .any(|r| r.license == i18n.tr("model-catalog-license-unverified"))
        );
        let translate = build_rows(Feature::Translate, &dirs_of(&dir), i18n);
        assert!(
            translate.iter().all(|r| r.size.starts_with('~')),
            "翻译包大小是文档里的约数，应带约号"
        );
        std::fs::remove_dir_all(&dir).ok();
    }
}
