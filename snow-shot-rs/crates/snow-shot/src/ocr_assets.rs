//! OCR 资产定位：运行时（`snow-ocr-process`）与模型的目录布局、清单解析与就绪判定。
//!
//! 运行时与状态目录与上游一致（不随包，按需下载到用户目录）：
//! `<数据根>/assets/ocr/runtimes/<版本>/<平台>/`、`state/<版本>/`。
//! 模型放在统一目录 `<数据根>/models/ocr/<模型 ID>/`；手动放入的文件不要求完成标记，文件齐全即可。
//! 清单是上游的原样副本（`resources/ocr-asset-manifest.json`），下载后的哈希校验以它为准。

use serde::Deserialize;
use snow_i18n::{Args, I18n};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

/// 环境变量：直接指定 `snow-ocr-process` 可执行文件（开发 / 自测用）。
pub const ENV_OCR_PROCESS_EXE: &str = "SNOW_OCR_PROCESS_EXE";
/// 环境变量：覆盖 OCR 资产根目录（默认 `<数据根>/assets/ocr`）。
pub const ENV_OCR_ASSET_DIR: &str = "SNOW_OCR_ASSET_DIR";
/// 资产目录名（位于数据根下）。
const ASSETS_DIR: &str = "assets";
/// OCR 子目录名。
const OCR_DIR: &str = "ocr";
/// 运行时子目录名。
const RUNTIMES_DIR: &str = "runtimes";
/// 模型子目录名。
const MODELS_DIR: &str = "models";
/// 能力缓存子目录名。
const STATE_DIR: &str = "state";
/// 校验完成标记文件名。
pub const COMPLETE_MARKER: &str = ".complete.json";
/// 可执行文件扩展名（清单里用它找到运行时主程序）。
const EXE_SUFFIX: &str = ".exe";
/// 内置清单（上游原样副本）。
const MANIFEST_JSON: &str = include_str!("../resources/ocr-asset-manifest.json");

/// 清单里的单个文件。
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct AssetFile {
    /// 文件名。
    pub name: String,
    /// 字节数。
    pub size: u64,
    /// SHA-256（小写十六进制）。
    pub sha256: String,
    /// 下载地址（运行时内的子文件没有）。
    #[serde(default)]
    pub url: String,
}

/// 运行时压缩包与其内含文件。
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct RuntimeSpec {
    /// 运行时版本。
    pub version: String,
    /// 平台标记。
    pub platform: String,
    /// 压缩包。
    pub archive: AssetFile,
    /// 解压后应有的文件。
    pub files: Vec<AssetFile>,
}

/// 一套模型（检测 + 识别 + 字典）。
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct ModelSpec {
    /// 配置里的模型类型键（`small` 等）。
    #[serde(rename = "type")]
    pub kind: String,
    /// 模型目录 ID。
    pub id: String,
    /// 检测模型文件名。
    pub detector: String,
    /// 识别模型文件名。
    pub recognizer: String,
    /// 字典文件名。
    pub dictionary: String,
    /// 全部文件。
    pub files: Vec<AssetFile>,
}

/// OCR 资产清单。
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct Manifest {
    /// 默认模型类型。
    pub default_model: String,
    /// 运行时。
    pub runtime: RuntimeSpec,
    /// 全部模型。
    pub models: Vec<ModelSpec>,
}

/// 解析内置清单（只解析一次）。
///
/// # 返回
/// 清单引用；内置 JSON 损坏时返回解析器给出的原因（技术信息，不翻译）。
///
/// ```ignore
/// assert_eq!(manifest().unwrap().default_model, "small");
/// ```
pub fn manifest() -> Result<&'static Manifest, String> {
    static CELL: OnceLock<Result<Manifest, String>> = OnceLock::new();
    CELL.get_or_init(|| serde_json::from_str(MANIFEST_JSON).map_err(|e| e.to_string()))
        .as_ref()
        .map_err(Clone::clone)
}

/// 已就绪的 OCR 资产路径。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OcrAssets {
    /// `snow-ocr-process` 可执行文件。
    pub exe: PathBuf,
    /// 检测模型。
    pub detector: PathBuf,
    /// 识别模型。
    pub recognizer: PathBuf,
    /// 字典。
    pub dictionary: PathBuf,
    /// 能力缓存目录（交给 worker 的 Hello）。
    pub state_dir: PathBuf,
    /// 模型目录 ID（日志用）。
    pub model_id: String,
}

/// OCR 资产不可用的原因。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OcrUnavailable {
    /// 没有运行时可执行文件。
    NoRuntime,
    /// 模型缺失或不完整。
    NoModel {
        /// 模型目录 ID。
        id: String,
    },
    /// 配置的模型类型不在清单里。
    UnknownModel(String),
    /// 清单本身损坏。
    Manifest(String),
}

impl OcrUnavailable {
    /// 面向用户的提示文案。
    ///
    /// # 参数
    /// - `i18n`：界面语料。
    ///
    /// # 返回
    /// 一句说明，含下一步（下载）指引。
    pub fn message(&self, i18n: &I18n) -> String {
        match self {
            Self::NoRuntime => i18n.tr("ocr-unavailable-no-runtime"),
            Self::NoModel { id } => i18n.tr_with(
                "ocr-unavailable-no-model",
                &Args::new().named("id", id.as_str()),
            ),
            Self::UnknownModel(kind) => i18n.tr_with(
                "ocr-unavailable-unknown-model",
                &Args::new().named("kind", kind.as_str()),
            ),
            Self::Manifest(detail) => i18n.tr_with(
                "fetch-manifest-corrupt",
                &Args::new()
                    .named("what", crate::ocr_download::OCR_MANIFEST_NAME)
                    .named("detail", detail.as_str()),
            ),
        }
    }

    /// 是否可以通过下载解决。
    pub fn can_download(&self) -> bool {
        matches!(self, Self::NoRuntime | Self::NoModel { .. })
    }
}

/// 计算 OCR 资产根目录：环境变量优先，否则 `<数据根>/assets/ocr`。
///
/// # 参数
/// - `data_root`：应用数据根目录。
/// - `env_override`：`SNOW_OCR_ASSET_DIR` 的值（为空视为未设置）。
///
/// ```ignore
/// assert!(ocr_root(Path::new("D"), None).ends_with("ocr"));
/// ```
pub fn ocr_root(data_root: &Path, env_override: Option<&str>) -> PathBuf {
    match env_override.map(str::trim).filter(|v| !v.is_empty()) {
        Some(dir) => PathBuf::from(dir),
        None => data_root.join(ASSETS_DIR).join(OCR_DIR),
    }
}

/// 运行时目录。
pub fn runtime_dir(root: &Path, runtime: &RuntimeSpec) -> PathBuf {
    root.join(RUNTIMES_DIR)
        .join(&runtime.version)
        .join(&runtime.platform)
}

/// 资产根下的 `models` 目录（环境变量覆盖资产根时的模型目录；测试里伪造资产也用它）。
pub fn models_dir(root: &Path) -> PathBuf {
    root.join(MODELS_DIR)
}

/// 某模型在资产根 `models` 下的目录（测试里伪造资产用）。
#[cfg(test)]
pub fn model_dir(root: &Path, model: &ModelSpec) -> PathBuf {
    models_dir(root).join(&model.id)
}

/// OCR 模型的读取目录列表：只有统一目录；设置了资产根环境变量时只认 `<覆盖目录>/models`。
///
/// # 参数
/// - `data_root`：应用数据根目录。
/// - `env_root`：`SNOW_OCR_ASSET_DIR` 的值。
///
/// # 示例
/// ```ignore
/// let dirs = model_search_dirs(Path::new("D"), None);
/// assert!(dirs[0].ends_with("ocr"));
/// ```
pub fn model_search_dirs(data_root: &Path, env_root: Option<&str>) -> Vec<PathBuf> {
    crate::model_catalog::read_dirs(
        crate::model_catalog::Feature::Ocr,
        data_root,
        "",
        env_root,
        None,
    )
}

/// 能力缓存目录。
pub fn state_dir(root: &Path, runtime: &RuntimeSpec) -> PathBuf {
    root.join(STATE_DIR).join(&runtime.version)
}

/// 目录里的文件是否齐全且已校验：有完成标记，且每个文件存在、大小与清单一致。
///
/// # 参数
/// - `dir`：目标目录。
/// - `files`：清单里应有的文件。
pub fn dir_complete(dir: &Path, files: &[AssetFile]) -> bool {
    dir.join(COMPLETE_MARKER).is_file()
        && files.iter().all(|f| {
            std::fs::metadata(dir.join(&f.name)).is_ok_and(|m| m.is_file() && m.len() == f.size)
        })
}

/// 模型目录里的文件是否齐全可用。
///
/// 有完成标记（应用内下载留下的）时按清单严格校验大小；没有标记（手动放入）时只要每个文件都存在且非空。
///
/// # 参数
/// - `dir`：目标目录。
/// - `files`：清单里应有的文件。
pub fn model_ready(dir: &Path, files: &[AssetFile]) -> bool {
    if dir.join(COMPLETE_MARKER).is_file() {
        return dir_complete(dir, files);
    }
    files
        .iter()
        .all(|f| std::fs::metadata(dir.join(&f.name)).is_ok_and(|m| m.is_file() && m.len() > 0))
}

/// 在若干模型目录里找齐全的模型目录（按顺序，先找到的优先）。
///
/// # 参数
/// - `dirs`：模型目录列表（统一目录在前）。
/// - `model`：模型。
///
/// # 返回
/// `<某目录>/<模型 ID>`；都没有齐全的返回 `None`。
pub fn find_model_dir(dirs: &[PathBuf], model: &ModelSpec) -> Option<PathBuf> {
    dirs.iter()
        .map(|dir| dir.join(&model.id))
        .find(|candidate| model_ready(candidate, &model.files))
}

/// 按模型类型查找模型；类型为空时用默认模型。
///
/// # 参数
/// - `manifest`：清单。
/// - `kind`：配置里的模型类型。
pub fn find_model<'a>(manifest: &'a Manifest, kind: &str) -> Result<&'a ModelSpec, OcrUnavailable> {
    let wanted = if kind.trim().is_empty() {
        manifest.default_model.as_str()
    } else {
        kind
    };
    manifest
        .models
        .iter()
        .find(|m| m.kind == wanted)
        .ok_or_else(|| OcrUnavailable::UnknownModel(wanted.to_string()))
}

/// 运行时主程序文件名（清单里唯一的 `.exe`）。
pub fn runtime_exe_name(runtime: &RuntimeSpec) -> Option<&str> {
    runtime
        .files
        .iter()
        .map(|f| f.name.as_str())
        .find(|name| name.ends_with(EXE_SUFFIX))
}

/// 定位 OCR 资产。
///
/// # 参数
/// - `root`：资产根目录（见 [`ocr_root`]，运行时与状态目录在它下面）。
/// - `model_dirs`：模型读取目录（见 [`model_search_dirs`]）。
/// - `exe_override`：`SNOW_OCR_PROCESS_EXE` 指定的可执行文件（存在时优先，且不要求运行时目录）。
/// - `model_kind`：配置里的模型类型。
///
/// # 返回
/// 全部就绪时返回路径集合；否则返回具体缺什么。
///
/// ```ignore
/// let assets = resolve_assets(&root, &model_dirs, None, "small")?;
/// ```
pub fn resolve_assets(
    root: &Path,
    model_dirs: &[PathBuf],
    exe_override: Option<&Path>,
    model_kind: &str,
) -> Result<OcrAssets, OcrUnavailable> {
    let manifest = manifest().map_err(OcrUnavailable::Manifest)?;
    let model = find_model(manifest, model_kind)?;
    let runtime = &manifest.runtime;
    let exe = match exe_override.filter(|p| p.is_file()) {
        Some(path) => path.to_path_buf(),
        None => {
            let dir = runtime_dir(root, runtime);
            let name = runtime_exe_name(runtime).ok_or(OcrUnavailable::NoRuntime)?;
            if !dir_complete(&dir, &runtime.files) {
                return Err(OcrUnavailable::NoRuntime);
            }
            dir.join(name)
        }
    };
    let dir = find_model_dir(model_dirs, model).ok_or_else(|| OcrUnavailable::NoModel {
        id: model.id.clone(),
    })?;
    Ok(OcrAssets {
        exe,
        detector: dir.join(&model.detector),
        recognizer: dir.join(&model.recognizer),
        dictionary: dir.join(&model.dictionary),
        state_dir: state_dir(root, runtime),
        model_id: model.id.clone(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 在临时目录里按清单伪造一套“完整”资产（文件内容为占位，但大小与清单一致）。
    fn fake_complete_dir(dir: &Path, files: &[AssetFile]) {
        std::fs::create_dir_all(dir).expect("建目录");
        for f in files {
            let file = std::fs::File::create(dir.join(&f.name)).expect("建文件");
            file.set_len(f.size).expect("定长");
        }
        std::fs::write(dir.join(COMPLETE_MARKER), b"{}").expect("标记");
    }

    /// 生成唯一临时目录。
    fn temp_root(tag: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("snow-ocr-assets-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("建根目录");
        dir
    }

    /// 只含资产根下 `models` 的读取目录（环境变量覆盖布局的测试用）。
    fn dirs(root: &Path) -> Vec<PathBuf> {
        vec![models_dir(root)]
    }

    /// 手动放入（无完成标记）的文件夹，只要三个文件都在就能被认出；缺一个或空文件不行。
    #[test]
    fn manual_files_without_marker_are_accepted() {
        let root = temp_root("manual");
        let m = manifest().expect("清单");
        let model = find_model(m, "small").expect("模型");
        let dir = model_dir(&root, model);
        std::fs::create_dir_all(&dir).expect("建");
        for f in &model.files {
            assert!(!model_ready(&dir, &model.files));
            std::fs::write(dir.join(&f.name), b"x").expect("写");
        }
        assert!(model_ready(&dir, &model.files), "无标记但文件齐全");
        std::fs::write(dir.join(&model.detector), b"").expect("清空");
        assert!(!model_ready(&dir, &model.files), "空文件不算");
        // 有标记时仍按清单严格校验大小（应用内下载路径）
        std::fs::write(dir.join(&model.detector), b"x").expect("写");
        std::fs::write(dir.join(COMPLETE_MARKER), b"{}").expect("标记");
        assert!(!model_ready(&dir, &model.files));
        let _ = std::fs::remove_dir_all(&root);
    }

    /// 只认统一目录 `models/ocr`（旧位置 `assets/ocr/models` 不再读取）；环境变量覆盖只认覆盖目录下的 models。
    #[test]
    fn unified_dir_only() {
        let data = temp_root("unified");
        let m = manifest().expect("清单");
        let model = find_model(m, "small").expect("模型");
        let dirs = model_search_dirs(&data, None);
        assert_eq!(dirs, vec![data.join("models").join("ocr")]);
        let put = |dir: &Path| {
            std::fs::create_dir_all(dir).expect("建");
            for f in &model.files {
                std::fs::write(dir.join(&f.name), b"x").expect("写");
            }
        };
        put(&data
            .join("assets")
            .join("ocr")
            .join("models")
            .join(&model.id));
        assert!(find_model_dir(&dirs, model).is_none(), "旧位置不再读取");
        let unified = dirs[0].join(&model.id);
        put(&unified);
        assert_eq!(find_model_dir(&dirs, model), Some(unified));
        let env = data.join("custom");
        let env_dirs = model_search_dirs(&data, env.to_str());
        assert_eq!(env_dirs, vec![env.join("models")]);
        let _ = std::fs::remove_dir_all(&data);
    }

    /// 内置清单可解析，含默认模型与全部 7 套模型，运行时有 exe。
    #[test]
    fn embedded_manifest_parses() {
        let m = manifest().expect("清单");
        assert_eq!(m.default_model, "small");
        assert_eq!(m.models.len(), 7);
        assert!(runtime_exe_name(&m.runtime).is_some());
        assert_eq!(m.runtime.archive.sha256.len(), 64);
        for model in &m.models {
            for f in &model.files {
                assert_eq!(f.sha256.len(), 64, "{}", f.name);
                assert!(f.url.starts_with("https://"), "{}", f.name);
            }
        }
    }

    /// 配置里的 7 个模型类型都能在清单里找到；未知类型报错；空串用默认。
    #[test]
    fn find_model_by_kind() {
        let m = manifest().expect("清单");
        for kind in [
            "extra_small",
            "small",
            "medium",
            "small_v5",
            "medium_v5",
            "small_v4",
            "medium_v4",
        ] {
            assert_eq!(find_model(m, kind).expect(kind).kind, kind);
        }
        assert_eq!(find_model(m, "").expect("默认").kind, "small");
        assert_eq!(
            find_model(m, "nope"),
            Err(OcrUnavailable::UnknownModel("nope".into()))
        );
    }

    /// 环境变量覆盖根目录；空白视为未设置。
    #[test]
    fn root_override() {
        assert_eq!(ocr_root(Path::new("D"), Some("X")), PathBuf::from("X"));
        assert_eq!(
            ocr_root(Path::new("D"), Some("  ")),
            Path::new("D").join("assets").join("ocr")
        );
        assert_eq!(
            ocr_root(Path::new("D"), None),
            Path::new("D").join("assets").join("ocr")
        );
    }

    /// 什么都没有：缺运行时；运行时齐全但没模型：缺模型；都齐：解析出路径。
    #[test]
    fn resolve_progression() {
        let root = temp_root("resolve");
        let m = manifest().expect("清单");
        assert_eq!(
            resolve_assets(&root, &dirs(&root), None, "small"),
            Err(OcrUnavailable::NoRuntime)
        );
        fake_complete_dir(&runtime_dir(&root, &m.runtime), &m.runtime.files);
        let model = find_model(m, "small").expect("模型");
        assert_eq!(
            resolve_assets(&root, &dirs(&root), None, "small"),
            Err(OcrUnavailable::NoModel {
                id: model.id.clone()
            })
        );
        fake_complete_dir(&model_dir(&root, model), &model.files);
        let assets = resolve_assets(&root, &dirs(&root), None, "small").expect("齐全");
        assert!(assets.exe.starts_with(runtime_dir(&root, &m.runtime)));
        assert!(assets.detector.ends_with(&model.detector));
        assert_eq!(assets.model_id, model.id);
        let _ = std::fs::remove_dir_all(&root);
    }

    /// 缺完成标记、或文件大小不符都算不完整。
    #[test]
    fn incomplete_dirs_are_rejected() {
        let root = temp_root("incomplete");
        let m = manifest().expect("清单");
        let model = find_model(m, "extra_small").expect("模型");
        let dir = model_dir(&root, model);
        fake_complete_dir(&dir, &model.files);
        assert!(dir_complete(&dir, &model.files));
        std::fs::remove_file(dir.join(COMPLETE_MARKER)).expect("删标记");
        assert!(!dir_complete(&dir, &model.files));
        std::fs::write(dir.join(COMPLETE_MARKER), b"{}").expect("标记");
        std::fs::write(dir.join(&model.detector), b"short").expect("改小");
        assert!(!dir_complete(&dir, &model.files));
        let _ = std::fs::remove_dir_all(&root);
    }

    /// 环境变量指定的 exe 存在时优先于运行时目录；不存在则忽略。
    #[test]
    fn exe_override_wins_when_present() {
        let root = temp_root("override");
        let m = manifest().expect("清单");
        let model = find_model(m, "small").expect("模型");
        fake_complete_dir(&model_dir(&root, model), &model.files);
        let exe = root.join("custom-ocr.exe");
        std::fs::write(&exe, b"x").expect("写 exe");
        assert_eq!(
            resolve_assets(&root, &dirs(&root), Some(&exe), "small")
                .expect("齐全")
                .exe,
            exe
        );
        let missing = root.join("missing.exe");
        assert_eq!(
            resolve_assets(&root, &dirs(&root), Some(&missing), "small"),
            Err(OcrUnavailable::NoRuntime)
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    /// 提示文案非空，且只有缺运行时 / 缺模型可下载。
    #[test]
    fn messages_and_download_flags() {
        assert!(OcrUnavailable::NoRuntime.can_download());
        assert!(OcrUnavailable::NoModel { id: "m".into() }.can_download());
        assert!(!OcrUnavailable::UnknownModel("x".into()).can_download());
        assert!(!OcrUnavailable::Manifest("e".into()).can_download());
        let zh = crate::ocr_backend::i18n_for("zh-CN");
        let en = crate::ocr_backend::i18n_for("en-US");
        assert!(OcrUnavailable::NoRuntime.message(zh).contains("运行时"));
        assert!(OcrUnavailable::NoRuntime.message(en).is_ascii());
        assert!(
            OcrUnavailable::NoModel { id: "m1".into() }
                .message(en)
                .contains("m1")
        );
    }
}
