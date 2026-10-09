//! 公式识别的资产定位与引导：模型由用户自行下载、放进“公式模型目录”，程序只在该目录里按文件名查找并做
//! 文件齐全与大小下限校验，不内置、不托管、不下载模型（也没有内置哈希清单）。
//!
//! 同时提供设置页“公式模型”面板的文案与状态，以及缺模型时覆盖窗里显示的本地化引导。

use crate::ort_runtime;
use serde_json::Value;
use snow_config::document::ConfigDocument;
use snow_config::extensions::KEY_LATEX_MODEL_DIR;
use snow_i18n::{Args, I18n};
use std::path::{Path, PathBuf};

/// 环境变量：直接指定 `snow-latex.exe`（开发 / 自测用）。
pub const ENV_LATEX_EXE: &str = "SNOW_LATEX_EXE";
/// 工作进程可执行文件名。
pub const LATEX_WORKER_EXE_NAME: &str = "snow-latex.exe";
/// 设置页里承载“公式模型”面板的分组 id。
pub const LATEX_GROUP_ID: &str = "screenshot_conversion";
/// 官方来源地址在语料里的 message id（文档性文本，随语言可调）。
const SOURCE_URL_MESSAGE: &str = "latex-source-url";

/// 模型目录里必须有的一个文件。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ModelFile {
    /// 文件名（官方发布包里的原名）。
    pub name: &'static str,
    /// 大小下限（字节）：官方文件的一半左右，只用来挡住空文件与下载中断的半截文件。
    pub min_size: u64,
}

/// 宽度分类器。
pub const FILE_RESIZER: ModelFile = ModelFile {
    name: "image_resizer.onnx",
    min_size: 20_000_000,
};
/// 编码器。
pub const FILE_ENCODER: ModelFile = ModelFile {
    name: "encoder.onnx",
    min_size: 40_000_000,
};
/// 解码器。
pub const FILE_DECODER: ModelFile = ModelFile {
    name: "decoder.onnx",
    min_size: 25_000_000,
};
/// 词表。
pub const FILE_TOKENIZER: ModelFile = ModelFile {
    name: "tokenizer.json",
    min_size: 10_000,
};
/// 需要的全部文件。
pub const MODEL_FILES: [ModelFile; 4] = [FILE_RESIZER, FILE_ENCODER, FILE_DECODER, FILE_TOKENIZER];

/// 模型目录里四个文件的路径。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelPaths {
    /// 宽度分类器。
    pub resizer: PathBuf,
    /// 编码器。
    pub encoder: PathBuf,
    /// 解码器。
    pub decoder: PathBuf,
    /// 词表。
    pub tokenizer: PathBuf,
}

/// 已就绪的公式识别资产。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LatexAssets {
    /// `snow-latex` 工作进程。
    pub exe: PathBuf,
    /// 模型文件。
    pub models: ModelPaths,
    /// `onnxruntime.dll`。
    pub ort_dll: PathBuf,
}

/// 公式识别资产不可用的原因。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LatexUnavailable {
    /// 目录里缺文件或文件不完整（附目录与文件名）。
    Missing {
        /// 设置的目录。
        dir: String,
        /// 缺失或过小的文件名。
        files: Vec<&'static str>,
    },
    /// onnxruntime 运行时未安装。
    NoRuntime,
    /// 找不到 `snow-latex` 工作进程或运行时路径有误（附说明）。
    NoWorker(String),
}

impl LatexUnavailable {
    /// 面向用户的引导文案：说明要自行下载哪些文件、放到哪个目录、如何设置，并附官方来源链接。
    ///
    /// # 参数
    /// - `i18n`：界面语料。
    pub fn message(&self, i18n: &I18n) -> String {
        let args = |files: String, dir: &str| {
            Args::new()
                .named("files", files)
                .named("dir", dir)
                .named("url", i18n.tr(SOURCE_URL_MESSAGE))
        };
        match self {
            Self::Missing { dir, files } => {
                i18n.tr_with("latex-guide-missing", &args(file_list(files), dir))
            }
            Self::NoRuntime => i18n.tr("latex-guide-no-runtime"),
            Self::NoWorker(detail) => i18n.tr_with(
                "latex-guide-no-worker",
                &Args::new().named("detail", detail.as_str()),
            ),
        }
    }

    /// 是否可以通过下载解决（只有缺 onnxruntime 运行时可以；模型由用户自行下载）。
    pub fn can_download(&self) -> bool {
        matches!(self, Self::NoRuntime)
    }
}

/// 文件名列表拼成 `a, b, c`。
fn file_list(names: &[&str]) -> String {
    names.join(", ")
}

/// 校验模型目录：四个文件都在且不小于大小下限。
///
/// # 参数
/// - `dir`：公式模型目录。
///
/// # 返回
/// 全部就绪返回路径；否则返回缺失或过小的文件名。
///
/// ```ignore
/// let paths = check_model_dir(Path::new("D:/models/latex"))?;
/// ```
pub fn check_model_dir(dir: &Path) -> Result<ModelPaths, Vec<&'static str>> {
    let bad: Vec<&'static str> = MODEL_FILES
        .iter()
        .filter(|f| {
            !std::fs::metadata(dir.join(f.name)).is_ok_and(|m| m.is_file() && m.len() >= f.min_size)
        })
        .map(|f| f.name)
        .collect();
    if bad.is_empty() {
        Ok(ModelPaths {
            resizer: dir.join(FILE_RESIZER.name),
            encoder: dir.join(FILE_ENCODER.name),
            decoder: dir.join(FILE_DECODER.name),
            tokenizer: dir.join(FILE_TOKENIZER.name),
        })
    } else {
        Err(bad)
    }
}

/// 读取配置里的公式模型目录（去掉首尾空白，空串表示没设置）。
///
/// # 参数
/// - `document`：配置文档。
pub fn model_dir_from_document(document: &ConfigDocument) -> String {
    match document.value(KEY_LATEX_MODEL_DIR) {
        Value::String(s) => s.trim().to_string(),
        _ => String::new(),
    }
}

/// 公式模型的读取目录：配置里用户自设的目录优先，其后是统一目录 `<数据根>/models/latex`；没设置时只有统一目录。
///
/// # 参数
/// - `data_root`：应用数据根目录。
/// - `cfg_dir`：配置里的公式模型目录（空白表示没设置）。
pub fn model_dirs(data_root: &Path, cfg_dir: &str) -> Vec<PathBuf> {
    crate::model_catalog::read_dirs(
        crate::model_catalog::Feature::Latex,
        data_root,
        cfg_dir,
        None,
        None,
    )
}

/// 在读取目录里按选中的模型找四个文件齐全的文件夹：选中项优先，为空或已不存在时用第一个可用的。
///
/// # 参数
/// - `dirs`：读取目录（主目录在前，见 [`model_dirs`]）。
/// - `selected`：配置里选中的模型（文件夹名；空串表示自动）。
///
/// # 返回
/// 齐全时返回文件路径；没有任何可用模型时返回主目录与它缺的文件名。
pub fn find_models(
    dirs: &[PathBuf],
    selected: &str,
) -> Result<ModelPaths, (PathBuf, Vec<&'static str>)> {
    let options = crate::model_pick::latex_options(dirs);
    if let Some(found) = crate::model_pick::pick(&options, selected, &[])
        && let Ok(paths) = check_model_dir(&found.path)
    {
        return Ok(paths);
    }
    let primary = dirs.first().cloned().unwrap_or_default();
    let files = check_model_dir(&primary).err().unwrap_or_default();
    Err((primary, files))
}

/// 定位公式识别资产：先看模型目录，再看 onnxruntime，最后看工作进程。
///
/// # 参数
/// - `model_dir`：配置里的公式模型目录（空白表示没设置，回退统一目录 `<数据根>/models/latex`）。
/// - `selected`：配置里选中的公式模型（空串表示自动）。
/// - `data_root`：应用数据根目录（找 onnxruntime 用）。
/// - `ort_env`：`SNOW_ORT_DYLIB` 的值。
/// - `exe_override`：`SNOW_LATEX_EXE` 的值（存在时优先）。
/// - `beside_exe`：主程序所在目录（找同目录的工作进程用）。
///
/// # 返回
/// 全部就绪返回路径集合；否则返回具体缺什么。
pub fn resolve_assets(
    model_dir: &str,
    selected: &str,
    data_root: &Path,
    ort_env: Option<&str>,
    exe_override: Option<&Path>,
    beside_exe: Option<&Path>,
) -> Result<LatexAssets, LatexUnavailable> {
    let models =
        find_models(&model_dirs(data_root, model_dir), selected).map_err(|(dir, files)| {
            LatexUnavailable::Missing {
                dir: dir.display().to_string(),
                files,
            }
        })?;
    let ort_dll = ort_runtime::resolve_ort_dylib(data_root, ort_env).map_err(|e| match e {
        ort_runtime::OrtUnavailable::NotInstalled => LatexUnavailable::NoRuntime,
        other => LatexUnavailable::NoWorker(other.detail()),
    })?;
    let exe = locate_worker(exe_override, beside_exe)?;
    Ok(LatexAssets {
        exe,
        models,
        ort_dll,
    })
}

/// 定位工作进程：环境变量指定的文件优先，其次主程序同目录。
fn locate_worker(
    exe_override: Option<&Path>,
    beside_exe: Option<&Path>,
) -> Result<PathBuf, LatexUnavailable> {
    if let Some(path) = exe_override.filter(|p| !p.as_os_str().is_empty()) {
        return if path.is_file() {
            Ok(path.to_path_buf())
        } else {
            Err(LatexUnavailable::NoWorker(format!(
                "{ENV_LATEX_EXE} points to a file that does not exist: {}",
                path.display()
            )))
        };
    }
    match beside_exe.map(|dir| dir.join(LATEX_WORKER_EXE_NAME)) {
        Some(path) if path.is_file() => Ok(path),
        Some(path) => Err(LatexUnavailable::NoWorker(format!(
            "{LATEX_WORKER_EXE_NAME} not found (expected at {})",
            path.display()
        ))),
        None => Err(LatexUnavailable::NoWorker(format!(
            "{LATEX_WORKER_EXE_NAME} not found"
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 唯一临时目录。
    fn temp_root(tag: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("snow-latex-assets-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("建根目录");
        dir
    }

    /// 在目录里放占位文件（稀疏设长，不真写数据）。
    fn put(dir: &Path, file: ModelFile, size: u64) {
        std::fs::File::create(dir.join(file.name))
            .and_then(|h| h.set_len(size))
            .expect("占位");
    }

    /// 缺文件、文件过小、齐全三种目录状态。
    #[test]
    fn check_dir_progression() {
        let dir = temp_root("check");
        assert_eq!(check_model_dir(&dir).unwrap_err().len(), 4);
        for f in MODEL_FILES {
            put(&dir, f, f.min_size);
        }
        put(&dir, FILE_DECODER, FILE_DECODER.min_size - 1);
        assert_eq!(check_model_dir(&dir), Err(vec![FILE_DECODER.name]));
        put(&dir, FILE_DECODER, FILE_DECODER.min_size);
        let paths = check_model_dir(&dir).expect("齐全");
        assert!(paths.tokenizer.ends_with("tokenizer.json"));
        assert!(check_model_dir(&dir.join("nope")).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 解析顺序：没设目录（统一目录里也没有）-> 缺文件 -> 缺运行时 -> 缺工作进程 -> 就绪。
    #[test]
    fn resolve_progression() {
        let root = temp_root("resolve");
        let models = root.join("models");
        std::fs::create_dir_all(&models).expect("建目录");
        let dir = models.to_str().expect("路径");
        let beside = root.join("bin");
        std::fs::create_dir_all(&beside).expect("建目录");
        assert!(matches!(
            resolve_assets("  ", "", &root, None, None, Some(&beside)),
            Err(LatexUnavailable::Missing { dir, files })
                if files.len() == 4 && Path::new(&dir) == root.join("models").join("latex")
        ));
        assert!(matches!(
            resolve_assets(dir, "", &root, None, None, Some(&beside)),
            Err(LatexUnavailable::Missing { files, .. }) if files.len() == 4
        ));
        for f in MODEL_FILES {
            put(&models, f, f.min_size);
        }
        assert_eq!(
            resolve_assets(dir, "", &root, None, None, Some(&beside)),
            Err(LatexUnavailable::NoRuntime)
        );
        let dll = root.join("onnxruntime.dll");
        std::fs::write(&dll, b"x").expect("写 dll");
        assert!(matches!(
            resolve_assets(dir, "", &root, dll.to_str(), None, Some(&beside)),
            Err(LatexUnavailable::NoWorker(_))
        ));
        std::fs::write(beside.join(LATEX_WORKER_EXE_NAME), b"x").expect("写 exe");
        let assets =
            resolve_assets(dir, "", &root, dll.to_str(), None, Some(&beside)).expect("齐全");
        assert_eq!(assets.ort_dll, dll);
        assert!(assets.exe.ends_with(LATEX_WORKER_EXE_NAME));
        assert!(assets.models.encoder.ends_with("encoder.onnx"));
        let _ = std::fs::remove_dir_all(&root);
    }

    /// 只有缺运行时可下载；环境变量指向不存在的 worker 报错。
    #[test]
    fn download_flag_and_worker_override() {
        assert!(LatexUnavailable::NoRuntime.can_download());
        assert!(
            !LatexUnavailable::Missing {
                dir: "x".into(),
                files: vec![]
            }
            .can_download()
        );
        assert!(matches!(
            locate_worker(Some(Path::new("Z:/nope/snow-latex.exe")), None),
            Err(LatexUnavailable::NoWorker(_))
        ));
    }

    /// 缺模型引导卡片：两种语言都说明要下载哪些文件、放到哪里、怎么设置，并附官方仓库链接；英文版纯 ASCII。
    #[test]
    fn guide_cards_are_localized() {
        let zh = crate::ocr_backend::i18n_for("zh-CN");
        let en = crate::ocr_backend::i18n_for("en-US");
        let cases = [LatexUnavailable::Missing {
            dir: "D:/m".into(),
            files: vec!["encoder.onnx", "tokenizer.json"],
        }];
        for case in &cases {
            for i18n in [zh, en] {
                let text = case.message(i18n);
                assert!(text.contains("RapidLaTeXOCR"), "{text}");
                assert!(text.contains("https://"), "{text}");
                assert!(!text.contains("{ $"), "{text}");
            }
            assert!(case.message(en).is_ascii(), "{case:?}");
            let text = case.message(zh);
            assert!(text.contains("设置"), "{text}");
        }
        let missing = cases[0].message(en);
        assert!(
            missing.contains("D:/m") && missing.contains("tokenizer.json"),
            "{missing}"
        );
        assert!(
            !missing.contains("image_resizer.onnx, encoder.onnx"),
            "只列缺的文件：{missing}"
        );
        assert!(
            LatexUnavailable::NoRuntime
                .message(zh)
                .contains("onnxruntime")
        );
        assert!(
            LatexUnavailable::NoWorker("x".into())
                .message(en)
                .contains("x")
        );
    }

    /// 未设目录时读统一目录 `models/latex`（手动放入四个文件即可）；自设目录优先，自设目录缺文件时回退统一目录。
    #[test]
    fn default_dir_manual_files_and_priority() {
        let data = temp_root("default-dir");
        let unified = data.join("models").join("latex");
        std::fs::create_dir_all(&unified).expect("建");
        assert!(find_models(&model_dirs(&data, ""), "").is_err());
        for f in MODEL_FILES {
            put(&unified, f, f.min_size);
        }
        let paths = find_models(&model_dirs(&data, "  "), "").expect("统一目录齐全");
        assert!(paths.encoder.starts_with(&unified));
        // 自设目录齐全时优先
        let custom = temp_root("custom-dir");
        for f in MODEL_FILES {
            put(&custom, f, f.min_size);
        }
        let paths =
            find_models(&model_dirs(&data, custom.to_str().expect("路径")), "").expect("自设");
        assert!(paths.encoder.starts_with(&custom));
        // 自设目录缺文件，统一目录齐全：回退
        let empty = temp_root("empty-dir");
        let paths =
            find_models(&model_dirs(&data, empty.to_str().expect("路径")), "").expect("回退");
        assert!(paths.encoder.starts_with(&unified));
        // 都不齐全：报主目录（自设目录）缺的文件
        std::fs::remove_dir_all(&unified).expect("删");
        let (dir, files) =
            find_models(&model_dirs(&data, empty.to_str().expect("路径")), "").expect_err("都缺");
        assert_eq!(dir, empty);
        assert_eq!(files.len(), 4);
        for d in [data, custom, empty] {
            let _ = std::fs::remove_dir_all(&d);
        }
    }

    /// 选中的公式模型：文件夹名匹配就用它；为空或已不存在时用第一个可用的。
    #[test]
    fn selected_model_with_fallback() {
        let data = temp_root("selected");
        let unified = data.join("models").join("latex");
        for name in ["alpha", "beta"] {
            std::fs::create_dir_all(unified.join(name)).expect("建");
            for f in MODEL_FILES {
                put(&unified.join(name), f, f.min_size);
            }
        }
        let dirs = model_dirs(&data, "");
        let beta = find_models(&dirs, "beta").expect("beta");
        assert!(beta.encoder.starts_with(unified.join("beta")));
        let auto = find_models(&dirs, "").expect("自动");
        assert!(
            auto.encoder.starts_with(unified.join("alpha")),
            "自动用第一个"
        );
        let gone = find_models(&dirs, "gone").expect("回退");
        assert!(gone.encoder.starts_with(unified.join("alpha")));
        let _ = std::fs::remove_dir_all(&data);
    }

    /// 从配置文档读目录：默认空，设置后去空白。
    #[test]
    fn dir_from_document() {
        let mut doc = ConfigDocument::from_bytes(None);
        assert_eq!(model_dir_from_document(&doc), "");
        doc.set_value(KEY_LATEX_MODEL_DIR, serde_json::json!(" D:/m "))
            .expect("目录");
        assert_eq!(model_dir_from_document(&doc), "D:/m");
    }
}
