//! 表格识别资产：SLANet_plus 模型（官方 ONNX 发布包）、onnxruntime 动态库与 `snow-table` 工作进程的定位、
//! 清单解析、就绪判定与按需下载。
//!
//! 模型放在统一目录 `<数据根>/models/table/`（文件夹 `<模型 ID>/` 或直接放 `.onnx` 文件都认），
//! 不要求完成标记，文件齐全即可。
//! 模型不随包；下载复用 OCR 下载器（curl 断点续传 + SHA-256 校验 + 原子改名），onnxruntime 复用翻译的那份运行时。

use crate::ocr_assets::{AssetFile, COMPLETE_MARKER, model_ready};
use crate::ocr_download::{DownloadItem, DownloadStep, FetchError, fetch_verified, write_marker};
use crate::ort_runtime;
use serde::Deserialize;
use snow_i18n::{Args, I18n};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::sync::atomic::AtomicBool;

/// 环境变量：覆盖表格资产根目录（默认 `<数据根>/assets/table`）。
pub const ENV_TABLE_ASSET_DIR: &str = "SNOW_TABLE_ASSET_DIR";
/// 环境变量：直接指定 `snow-table.exe`（开发 / 自测用）。
pub const ENV_TABLE_EXE: &str = "SNOW_TABLE_EXE";
/// 工作进程可执行文件名。
pub const TABLE_WORKER_EXE_NAME: &str = "snow-table.exe";
/// 资产目录名（位于数据根下）。
const ASSETS_DIR: &str = "assets";
/// 表格子目录名。
const TABLE_DIR: &str = "table";
/// 表格清单的组件名（错误文案里的技术名词）。
const TABLE_MANIFEST_NAME: &str = "table";
/// 内置清单。
const MANIFEST_JSON: &str = include_str!("../resources/table-model-manifest.json");

/// 表格模型清单。
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct TableManifest {
    /// 模型目录 ID。
    pub id: String,
    /// 许可证（展示用）。
    #[serde(default)]
    pub license: String,
    /// 来源说明。
    #[serde(default)]
    pub source: String,
    /// 全部文件（本期只有一个 ONNX）。
    pub files: Vec<AssetFile>,
}

impl TableManifest {
    /// 模型 ONNX 的文件名（清单里第一个 `.onnx`）。
    pub fn model_file(&self) -> Option<&AssetFile> {
        self.files.iter().find(|f| f.name.ends_with(".onnx"))
    }

    /// 全部文件的总字节数（给“约 X MB”提示用）。
    pub fn total_size(&self) -> u64 {
        self.files.iter().map(|f| f.size).sum()
    }
}

/// 解析内置清单（只解析一次）。
///
/// # 返回
/// 清单引用；内置 JSON 损坏时返回解析器给出的原因（技术信息，不翻译）。
///
/// ```ignore
/// assert_eq!(manifest().unwrap().id, "slanet-plus");
/// ```
pub fn manifest() -> Result<&'static TableManifest, String> {
    static CELL: OnceLock<Result<TableManifest, String>> = OnceLock::new();
    CELL.get_or_init(|| serde_json::from_str(MANIFEST_JSON).map_err(|e| e.to_string()))
        .as_ref()
        .map_err(Clone::clone)
}

/// 计算表格资产根目录：环境变量优先，否则 `<数据根>/assets/table`。
///
/// # 参数
/// - `data_root`：应用数据根目录。
/// - `env_override`：`SNOW_TABLE_ASSET_DIR` 的值（空白视为未设置）。
pub fn table_root(data_root: &Path, env_override: Option<&str>) -> PathBuf {
    match env_override.map(str::trim).filter(|v| !v.is_empty()) {
        Some(dir) => PathBuf::from(dir),
        None => data_root.join(ASSETS_DIR).join(TABLE_DIR),
    }
}

/// 模型文件夹：`<模型根>/<模型 ID>`。
///
/// # 参数
/// - `models_root`：某个模型读取目录（如统一目录 `<数据根>/models/table`）。
/// - `manifest`：清单。
pub fn model_dir(models_root: &Path, manifest: &TableManifest) -> PathBuf {
    models_root.join(&manifest.id)
}

/// 表格模型的读取目录：只有统一目录；设置了环境变量时只认 `<覆盖目录>/models`。
///
/// # 参数
/// - `data_root`：应用数据根目录。
/// - `env_root`：`SNOW_TABLE_ASSET_DIR` 的值。
pub fn model_search_dirs(data_root: &Path, env_root: Option<&str>) -> Vec<PathBuf> {
    crate::model_catalog::read_dirs(
        crate::model_catalog::Feature::Table,
        data_root,
        "",
        None,
        env_root,
    )
}

/// 在读取目录里找可用的模型文件：先认 `<目录>/<模型 ID>/` 文件夹，再认直接放在目录里的文件。
///
/// # 参数
/// - `dirs`：读取目录（统一目录在前）。
/// - `manifest`：清单。
///
/// # 返回
/// 模型 `.onnx` 文件路径；都不齐全返回 `None`。
pub fn find_model_file(dirs: &[PathBuf], manifest: &TableManifest) -> Option<PathBuf> {
    let file = manifest.model_file()?;
    dirs.iter()
        .flat_map(|dir| [model_dir(dir, manifest), dir.clone()])
        .find(|candidate| model_ready(candidate, &manifest.files))
        .map(|candidate| candidate.join(&file.name))
}

/// 已就绪的表格识别资产路径。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TableAssets {
    /// `snow-table` 工作进程。
    pub exe: PathBuf,
    /// SLANet_plus 的 ONNX 文件。
    pub model: PathBuf,
    /// `onnxruntime.dll`。
    pub ort_dll: PathBuf,
}

/// 表格识别资产不可用的原因。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TableUnavailable {
    /// 模型未下载或不完整。
    NoModel {
        /// 约多少字节（给提示用）。
        size: u64,
    },
    /// onnxruntime 运行时未下载。
    NoRuntime,
    /// 找不到 `snow-table` 工作进程（随安装包提供，下载解决不了）。
    NoWorker(String),
    /// 清单损坏。
    Manifest(String),
}

impl TableUnavailable {
    /// 面向用户的“未配置 / 请下载”引导文案。
    ///
    /// # 参数
    /// - `i18n`：界面语料。
    pub fn message(&self, i18n: &I18n) -> String {
        match self {
            Self::NoModel { size } => i18n.tr_with(
                "table-unavailable-no-model",
                &Args::new().named("size", format_megabytes(*size)),
            ),
            Self::NoRuntime => i18n.tr("table-unavailable-no-runtime"),
            Self::NoWorker(detail) => i18n.tr_with(
                "table-unavailable-no-worker",
                &Args::new().named("detail", detail.as_str()),
            ),
            Self::Manifest(detail) => i18n.tr_with(
                "fetch-manifest-corrupt",
                &Args::new()
                    .named("what", TABLE_MANIFEST_NAME)
                    .named("detail", detail.as_str()),
            ),
        }
    }

    /// 是否可以通过下载解决。
    pub fn can_download(&self) -> bool {
        matches!(self, Self::NoModel { .. } | Self::NoRuntime)
    }
}

/// 字节数换成“x.y”MB 文本（一位小数，向上取整到 0.1）。
fn format_megabytes(bytes: u64) -> String {
    let tenths = (bytes * 10).div_ceil(1_000_000);
    format!("{}.{}", tenths / 10, tenths % 10)
}

/// 定位表格识别资产。
///
/// # 参数
/// - `data_root`：应用数据根目录。
/// - `env_root`：`SNOW_TABLE_ASSET_DIR` 的值。
/// - `selected`：配置里选中的表格模型（文件夹或 `.onnx` 文件名；空串表示自动，优先内置清单的 SLANet_plus）。
/// - `exe_override`：`SNOW_TABLE_EXE` 的值（存在时优先）。
/// - `ort_env`：`SNOW_ORT_DYLIB` 的值。
/// - `beside_exe`：主程序所在目录（找同目录的工作进程用）。
///
/// # 返回
/// 全部就绪时返回路径集合；否则返回具体缺什么。
///
/// ```ignore
/// let assets = resolve_assets(&root, None, None, None, Some(&exe_dir))?;
/// ```
pub fn resolve_assets(
    data_root: &Path,
    env_root: Option<&str>,
    selected: &str,
    exe_override: Option<&Path>,
    ort_env: Option<&str>,
    beside_exe: Option<&Path>,
) -> Result<TableAssets, TableUnavailable> {
    let manifest = manifest().map_err(TableUnavailable::Manifest)?;
    let exe = locate_worker(exe_override, beside_exe)?;
    let file = manifest
        .model_file()
        .ok_or_else(|| TableUnavailable::Manifest("no .onnx file in the manifest".into()))?;
    let options =
        crate::model_pick::table_options(&model_search_dirs(data_root, env_root), &file.name);
    let model = crate::model_pick::pick(&options, selected, &[&manifest.id, &file.name])
        .map(|o| o.path.clone())
        .ok_or(TableUnavailable::NoModel {
            size: manifest.total_size(),
        })?;
    let ort_dll = ort_runtime::resolve_ort_dylib(data_root, ort_env).map_err(|e| match e {
        ort_runtime::OrtUnavailable::NotInstalled => TableUnavailable::NoRuntime,
        other => TableUnavailable::NoWorker(other.detail()),
    })?;
    Ok(TableAssets {
        exe,
        model,
        ort_dll,
    })
}

/// 定位工作进程：环境变量指定的文件优先，其次主程序同目录。
fn locate_worker(
    exe_override: Option<&Path>,
    beside_exe: Option<&Path>,
) -> Result<PathBuf, TableUnavailable> {
    if let Some(path) = exe_override.filter(|p| !p.as_os_str().is_empty()) {
        return if path.is_file() {
            Ok(path.to_path_buf())
        } else {
            Err(TableUnavailable::NoWorker(format!(
                "{ENV_TABLE_EXE} points to a file that does not exist: {}",
                path.display()
            )))
        };
    }
    let candidate = beside_exe.map(|dir| dir.join(TABLE_WORKER_EXE_NAME));
    match candidate {
        Some(path) if path.is_file() => Ok(path),
        Some(path) => Err(TableUnavailable::NoWorker(format!(
            "{TABLE_WORKER_EXE_NAME} not found (expected at {})",
            path.display()
        ))),
        None => Err(TableUnavailable::NoWorker(format!(
            "{TABLE_WORKER_EXE_NAME} not found"
        ))),
    }
}

/// 下载并安装缺失的表格识别组件：SLANet_plus 模型，以及（如缺）onnxruntime 运行时。
///
/// # 参数
/// - `data_root`：应用数据根目录。
/// - `env_root`：`SNOW_TABLE_ASSET_DIR` 的值。
/// - `cancel`：取消开关。
/// - `progress`：进度回调（界面边界再翻译）。
///
/// # 返回
/// 全部就绪返回 `Ok`；任一步失败返回错误（半成品由下载器清理）。
pub fn download_missing(
    data_root: &Path,
    env_root: Option<&str>,
    cancel: &AtomicBool,
    mut progress: impl FnMut(DownloadStep),
) -> Result<(), FetchError> {
    let manifest = manifest().map_err(|detail| FetchError::Manifest {
        what: TABLE_MANIFEST_NAME,
        detail,
    })?;
    let dirs = model_search_dirs(data_root, env_root);
    if find_model_file(&dirs, manifest).is_none() {
        // 落盘到主目录（统一目录）
        let dir = model_dir(&dirs[0], manifest);
        progress(DownloadStep::TableModel);
        for file in &manifest.files {
            fetch_verified(
                &DownloadItem {
                    file: file.clone(),
                    dest_dir: dir.clone(),
                },
                cancel,
            )?;
        }
        write_marker(&dir)?;
    }
    if ort_runtime::resolve_ort_dylib(data_root, std::env::var(ENV_ORT).ok().as_deref()).is_err() {
        ort_runtime::install(data_root, cancel, &mut progress)?;
    }
    Ok(())
}

/// 环境变量：onnxruntime 动态库路径（与翻译共用）。
const ENV_ORT: &str = snow_translate::worker::ENV_ORT_DYLIB;

/// 完成标记文件名（供测试伪造就绪目录）。
pub const MARKER_NAME: &str = COMPLETE_MARKER;

#[cfg(test)]
mod tests {
    use super::*;

    /// 唯一临时目录。
    fn temp_root(tag: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("snow-table-assets-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("建根目录");
        dir
    }

    /// 内置清单可解析：官方 SLANet_plus 发布包的大小、哈希与地址都填全。
    #[test]
    fn embedded_manifest_parses() {
        let m = manifest().expect("清单");
        assert_eq!(m.id, "slanet-plus");
        assert_eq!(m.license, "Apache-2.0");
        let file = m.model_file().expect("onnx");
        assert_eq!(file.name, "slanet-plus.onnx");
        assert_eq!(file.size, 7_758_305);
        assert_eq!(file.sha256.len(), 64);
        assert!(file.url.starts_with("https://"));
        assert_eq!(m.total_size(), file.size);
    }

    /// 环境变量覆盖根目录；空白视为未设置。
    #[test]
    fn root_override() {
        assert_eq!(table_root(Path::new("D"), Some("X")), PathBuf::from("X"));
        assert_eq!(
            table_root(Path::new("D"), Some(" ")),
            Path::new("D").join("assets").join("table")
        );
    }

    /// 缺什么报什么：先工作进程，再模型，再运行时；全齐则解析出路径。
    #[test]
    fn resolve_progression() {
        let root = temp_root("resolve");
        let beside = root.join("bin");
        std::fs::create_dir_all(&beside).expect("建目录");
        let none = resolve_assets(&root, None, "", None, None, Some(&beside));
        assert!(matches!(none, Err(TableUnavailable::NoWorker(_))));
        std::fs::write(beside.join(TABLE_WORKER_EXE_NAME), b"x").expect("写 exe");
        let m = manifest().expect("清单");
        assert_eq!(
            resolve_assets(&root, None, "", None, None, Some(&beside)),
            Err(TableUnavailable::NoModel {
                size: m.total_size()
            })
        );
        // 统一目录里带标记的完整下载
        let dir = model_dir(&model_search_dirs(&root, None)[0], m);
        std::fs::create_dir_all(&dir).expect("建目录");
        for f in &m.files {
            std::fs::File::create(dir.join(&f.name))
                .and_then(|h| h.set_len(f.size))
                .expect("占位");
        }
        std::fs::write(dir.join(MARKER_NAME), b"{}").expect("标记");
        assert_eq!(
            resolve_assets(&root, None, "", None, None, Some(&beside)),
            Err(TableUnavailable::NoRuntime)
        );
        let dll = root.join("onnxruntime.dll");
        std::fs::write(&dll, b"x").expect("写 dll");
        let assets =
            resolve_assets(&root, None, "", None, dll.to_str(), Some(&beside)).expect("齐全");
        assert_eq!(assets.ort_dll, dll);
        assert!(assets.model.ends_with("slanet-plus.onnx"));
        assert!(assets.exe.ends_with(TABLE_WORKER_EXE_NAME));
        let _ = std::fs::remove_dir_all(&root);
    }

    /// 手动放入统一目录的文件夹或单个 onnx 文件（无标记）都认，文件夹形式优先，空文件不算；
    /// 旧位置不再读取；环境变量覆盖只认覆盖目录。
    #[test]
    fn unified_dir_manual_files_and_override() {
        let root = temp_root("manual");
        let m = manifest().expect("清单");
        let dirs = model_search_dirs(&root, None);
        assert_eq!(dirs, vec![root.join("models").join("table")]);
        assert!(find_model_file(&dirs, m).is_none());
        let old = root.join("assets").join("table").join("models");
        std::fs::create_dir_all(&old).expect("建");
        std::fs::write(old.join("slanet-plus.onnx"), b"x").expect("写");
        assert!(find_model_file(&dirs, m).is_none(), "旧位置不再读取");
        std::fs::create_dir_all(&dirs[0]).expect("建");
        std::fs::write(dirs[0].join("slanet-plus.onnx"), b"x").expect("写");
        assert_eq!(
            find_model_file(&dirs, m),
            Some(dirs[0].join("slanet-plus.onnx"))
        );
        let folder = model_dir(&dirs[0], m);
        std::fs::create_dir_all(&folder).expect("建");
        std::fs::write(folder.join("slanet-plus.onnx"), b"x").expect("写");
        assert_eq!(
            find_model_file(&dirs, m),
            Some(folder.join("slanet-plus.onnx"))
        );
        std::fs::write(folder.join("slanet-plus.onnx"), b"").expect("清空");
        assert_eq!(
            find_model_file(&dirs, m),
            Some(dirs[0].join("slanet-plus.onnx")),
            "文件夹里是空文件时退到直接放的文件"
        );
        let env = root.join("custom");
        assert_eq!(
            model_search_dirs(&root, env.to_str()),
            vec![env.join("models")]
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    /// 环境变量指向不存在的 worker 时报错；只有缺模型 / 运行时可下载。
    #[test]
    fn flags_and_messages() {
        let missing = Path::new("Z:/nope/snow-table.exe");
        assert!(matches!(
            locate_worker(Some(missing), None),
            Err(TableUnavailable::NoWorker(_))
        ));
        assert!(TableUnavailable::NoModel { size: 1 }.can_download());
        assert!(TableUnavailable::NoRuntime.can_download());
        assert!(!TableUnavailable::NoWorker("x".into()).can_download());
        assert!(!TableUnavailable::Manifest("x".into()).can_download());
        let zh = crate::ocr_backend::i18n_for("zh-CN");
        let en = crate::ocr_backend::i18n_for("en-US");
        let msg = TableUnavailable::NoModel { size: 7_758_305 };
        assert!(msg.message(zh).contains("7.8"));
        assert!(msg.message(en).is_ascii());
        assert_eq!(format_megabytes(7_758_305), "7.8");
    }
}
