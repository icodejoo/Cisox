//! 翻译服务层（ADR-5）：本地 NMT 模型清单、后端抽象、独立 worker 客户端与 OpenAI 兼容通道。
//!
//! - [`ModelScanner`] 扫描 `<模型根>/*/model.json`，与 `tools/snow-translator` 使用同一份清单；
//! - [`worker::WorkerEngine`] 管理本地 NMT 工作进程（按需拉起、空闲卸载、崩溃/卡死复位）；
//! - [`openai::OpenAiEngine`] 是可选的第二后端（OpenAI 兼容的聊天补全端点）；
//! - [`TranslationService`] 持有当前后端并做结果缓存。
//!
//! 没有任何“假翻译”：没有模型、没有运行时、后端失败时一律返回 [`TranslateError`]。

pub mod openai;
pub mod protocol;
pub mod router;
pub mod script_split;
pub mod segment;
pub mod worker;

pub use openai::{OpenAiCompatibleConfig, OpenAiEngine};
pub use worker::{WorkerConfig, WorkerEngine};

use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet, VecDeque};
use std::fmt;
use std::fs;
use std::path::{Component, Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError, RwLock};

/// 清单文件名。
pub const MANIFEST_FILE: &str = "model.json";
/// 当前唯一支持的清单版本。
pub const SUPPORTED_SCHEMA_VERSION: u32 = 1;
/// 结果缓存的最大条目数（超出后淘汰最早写入的）。
pub const CACHE_CAPACITY: usize = 256;
/// 清单缺省的最大输入 token 数。
const DEFAULT_MAX_INPUT_TOKENS: usize = 512;

/// 翻译支持的语言种类枚举。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Lang {
    /// 自动检测。
    Auto,
    /// 简体中文。
    ZhHans,
    /// 繁体中文。
    ZhHant,
    /// 英语。
    En,
    /// 日语。
    Ja,
    /// 韩语。
    Ko,
    /// 法语。
    Fr,
    /// 德语。
    De,
    /// 西班牙语。
    Es,
    /// 俄语。
    Ru,
    /// 意大利语。
    It,
    /// 葡萄牙语。
    Pt,
    /// 土耳其语。
    Tr,
    /// 阿拉伯语。
    Ar,
}

impl Lang {
    /// 标准 BCP-47 / ISO 语言代码（与模型清单 `pairs` 使用的写法一致）。
    ///
    /// # 返回
    /// 语言代号字符串切片。
    ///
    /// # 示例
    /// ```rust
    /// use snow_translate::Lang;
    /// assert_eq!(Lang::ZhHans.code(), "zh-CN");
    /// assert_eq!(Lang::En.code(), "en");
    /// ```
    pub const fn code(&self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::ZhHans => "zh-CN",
            Self::ZhHant => "zh-TW",
            Self::En => "en",
            Self::Ja => "ja",
            Self::Ko => "ko",
            Self::Fr => "fr",
            Self::De => "de",
            Self::Es => "es",
            Self::Ru => "ru",
            Self::It => "it",
            Self::Pt => "pt",
            Self::Tr => "tr",
            Self::Ar => "ar",
        }
    }

    /// 中文显示名称。
    ///
    /// # 返回
    /// 显示名称切片。
    ///
    /// # 示例
    /// ```rust
    /// use snow_translate::Lang;
    /// assert_eq!(Lang::ZhHans.display_name(), "简体中文");
    /// assert_eq!(Lang::En.display_name(), "英语");
    /// ```
    pub const fn display_name(&self) -> &'static str {
        match self {
            Self::Auto => "自动检测",
            Self::ZhHans => "简体中文",
            Self::ZhHant => "繁体中文",
            Self::En => "英语",
            Self::Ja => "日语",
            Self::Ko => "韩语",
            Self::Fr => "法语",
            Self::De => "德语",
            Self::Es => "西班牙语",
            Self::Ru => "俄语",
            Self::It => "意大利语",
            Self::Pt => "葡萄牙语",
            Self::Tr => "土耳其语",
            Self::Ar => "阿拉伯语",
        }
    }

    /// 从语言代码字符串解析（大小写不敏感，兼容配置里的 `zh-Hans` 与 NLLB 风格代码）。
    ///
    /// # 参数
    /// - `code`: 语言代号字符串。
    ///
    /// # 返回
    /// 匹配到的语言变体；若无法识别则返回 `None`。
    ///
    /// # 示例
    /// ```rust
    /// use snow_translate::Lang;
    /// assert_eq!(Lang::from_code("en"), Some(Lang::En));
    /// assert_eq!(Lang::from_code("zh-Hans"), Some(Lang::ZhHans));
    /// assert_eq!(Lang::from_code("unknown"), None);
    /// ```
    pub fn from_code(code: &str) -> Option<Self> {
        let normalized = code.trim().to_lowercase();
        match normalized.as_str() {
            "auto" => Some(Self::Auto),
            "zh" | "zh-cn" | "zh-hans" | "zho_hans" => Some(Self::ZhHans),
            "zh-tw" | "zh-hk" | "zh-hant" | "zho_hant" => Some(Self::ZhHant),
            "en" | "en-us" | "en-gb" | "eng_latn" => Some(Self::En),
            "ja" | "jp" | "jpn_jpan" => Some(Self::Ja),
            "ko" | "kr" | "kor_hang" => Some(Self::Ko),
            "fr" | "fra_latn" => Some(Self::Fr),
            "de" | "deu_latn" => Some(Self::De),
            "es" | "spa_latn" => Some(Self::Es),
            "ru" | "rus_cyrl" => Some(Self::Ru),
            "it" | "ita_latn" => Some(Self::It),
            "pt" | "pt-br" | "por_latn" => Some(Self::Pt),
            "tr" | "tur_latn" => Some(Self::Tr),
            "ar" | "arb_arab" => Some(Self::Ar),
            _ => None,
        }
    }

    /// 从系统/界面 locale 标记解析语言（`zh-CN`、`ja-JP`、`en_US` 等）；未知返回 `None`。
    /// 繁体系（Hant/TW/HK/MO）按项目决定不支持，返回 `None`。
    ///
    /// # 参数
    /// - `locale`: locale 字符串，分隔符 `-` 或 `_` 均可。
    ///
    /// # 示例
    /// ```rust
    /// use snow_translate::Lang;
    /// assert_eq!(Lang::from_locale("zh-HK"), None);
    /// assert_eq!(Lang::from_locale("ja-JP"), Some(Lang::Ja));
    /// ```
    pub fn from_locale(locale: &str) -> Option<Self> {
        let lower = locale.trim().to_lowercase().replace('_', "-");
        let primary = lower.split('-').next().unwrap_or("");
        if primary == "zh" {
            let hant = lower
                .split('-')
                .skip(1)
                .any(|t| matches!(t, "hant" | "tw" | "hk" | "mo"));
            return if hant { None } else { Some(Self::ZhHans) };
        }
        match Self::from_code(primary) {
            Some(Self::Auto) | None => None,
            other => other,
        }
    }
}

/// 翻译模块错误类型。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TranslateError {
    /// 未找到任何已安装的模型。
    NoModelFound(String),
    /// 不支持的语言转换对。
    UnsupportedLanguagePair(Lang, Lang),
    /// 请求参数不合法。
    InvalidRequest(String),
    /// 网络或 API 端点错误。
    Network(String),
    /// 本地模型或文件系统 I/O 错误。
    Io(String),
    /// 推理或执行超时。
    Timeout,
    /// onnxruntime 运行时缺失或无法加载（可通过下载运行时解决）。
    RuntimeMissing(String),
    /// 翻译工作进程程序缺失或无法启动。
    WorkerUnavailable(String),
    /// 翻译工作进程异常退出（附 stderr 摘要）。
    WorkerDied(String),
    /// 模型加载或校验失败（文件损坏、清单非法、哈希不符等）。
    ModelLoad(String),
    /// 推理或解码失败。
    Inference(String),
    /// 内存不足。
    OutOfMemory(String),
}

impl TranslateError {
    /// 是否可以通过下载 onnxruntime 运行时解决。
    ///
    /// # 示例
    /// ```rust
    /// use snow_translate::TranslateError;
    /// assert!(TranslateError::RuntimeMissing("x".into()).can_download_runtime());
    /// assert!(!TranslateError::Timeout.can_download_runtime());
    /// ```
    pub fn can_download_runtime(&self) -> bool {
        matches!(self, Self::RuntimeMissing(_))
    }
}

impl fmt::Display for TranslateError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoModelFound(msg) => write!(f, "未检测到可用翻译模型: {msg}"),
            Self::UnsupportedLanguagePair(src, tgt) => {
                write!(f, "不支持的翻译语言对: {} -> {}", src.display_name(), tgt.display_name())
            }
            Self::InvalidRequest(msg) => write!(f, "翻译请求不合法: {msg}"),
            Self::Network(msg) => write!(f, "翻译端点连接失败: {msg}"),
            Self::Io(msg) => write!(f, "模型文件读取错误: {msg}"),
            Self::Timeout => write!(f, "翻译执行超时"),
            Self::RuntimeMissing(msg) => write!(f, "缺少 onnxruntime 运行时: {msg}"),
            Self::WorkerUnavailable(msg) => write!(f, "翻译组件不可用: {msg}"),
            Self::WorkerDied(msg) => write!(f, "翻译进程异常退出: {msg}"),
            Self::ModelLoad(msg) => write!(f, "翻译模型加载失败: {msg}"),
            Self::Inference(msg) => write!(f, "翻译推理失败: {msg}"),
            Self::OutOfMemory(msg) => write!(f, "翻译内存不足: {msg}"),
        }
    }
}

impl std::error::Error for TranslateError {}

/// 本地 NMT 模型清单规范（`model.json`，见 ADR-5）。
///
/// 与 `tools/snow-translator` 读取的是同一个文件：基础字段沿用 schema_version=1，
/// `pairs`、`lang_tokens`、`source_prefix`、`sha256` 是向后兼容的可选扩展（缺省即旧格式）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ModelManifest {
    /// 清单规范版本号。
    pub schema_version: u32,
    /// 模型唯一标识符。
    pub id: String,
    /// 界面可读展示名（缺省用 id）。
    #[serde(default)]
    pub display_name: String,
    /// 模型架构族属（例如 `nllb`、`marian`、`m2m100`）。
    pub family: String,
    /// 量化类型（例如 `int8`、`fp16`、`fp32`）。
    #[serde(default)]
    pub quantization: String,
    /// 依赖的模型与分词器文件路径映射表（相对模型目录）。
    #[serde(default)]
    pub files: HashMap<String, String>,
    /// 支持的语言代码列表（`pairs` 缺省时取其有向全排列）。
    #[serde(default)]
    pub languages: Vec<String>,
    /// 有向语言对（源, 目标）；缺省表示 `languages` 的全排列。
    #[serde(default)]
    pub pairs: Vec<(String, String)>,
    /// 目标语言到模型 token 的映射（worker 使用，这里只做透传）。
    #[serde(default)]
    pub lang_tokens: HashMap<String, String>,
    /// 源文本前缀模板（worker 使用，这里只做透传）。
    #[serde(default)]
    pub source_prefix: String,
    /// 文件 SHA-256（worker 在加载前校验，这里只做透传）。
    #[serde(default)]
    pub sha256: HashMap<String, String>,
    /// 最大允许输入 token 数量。
    #[serde(default = "default_max_input_tokens")]
    pub max_input_tokens: usize,
    /// 是否参与默认选包：`false` 的包（如 Hy-MT2 可选包）只在用户显式指定时使用，
    /// 且只有在没有任何可默认选用的包支持该语言对时才作为兜底。缺省 `true`。
    #[serde(default = "default_true")]
    pub default_eligible: bool,
}

/// serde 缺省值：最大输入 token 数。
fn default_max_input_tokens() -> usize {
    DEFAULT_MAX_INPUT_TOKENS
}

/// serde 缺省值：`true`。
fn default_true() -> bool {
    true
}

impl ModelManifest {
    /// 校验清单文件字段是否完整且合法（不检查磁盘文件，见 [`Self::validate_files`]）。
    ///
    /// # 返回
    /// 合法返回 `Ok(())`，缺少关键字段返回描述错误。
    ///
    /// # 示例
    /// ```rust
    /// use snow_translate::ModelManifest;
    /// let manifest: ModelManifest = serde_json::from_str(
    ///     r#"{"schema_version":1,"id":"m","family":"marian","languages":["en","zh-CN"]}"#,
    /// ).unwrap();
    /// assert!(manifest.validate().is_ok());
    /// ```
    pub fn validate(&self) -> Result<(), TranslateError> {
        if self.schema_version != SUPPORTED_SCHEMA_VERSION {
            return Err(TranslateError::InvalidRequest(format!(
                "不支持的清单版本: {}",
                self.schema_version
            )));
        }
        if self.id.trim().is_empty() {
            return Err(TranslateError::InvalidRequest("模型 ID 不能为空".into()));
        }
        if self.languages.is_empty() && self.pairs.is_empty() {
            return Err(TranslateError::InvalidRequest("模型支持语言列表不能为空".into()));
        }
        Ok(())
    }

    /// 校验 `files` 里的路径：必须是模型目录内的相对路径且文件存在。
    ///
    /// # 参数
    /// - `dir`：模型目录。
    ///
    /// # 返回
    /// 通过返回 `Ok(())`；否则返回第一个问题的说明（绝对路径、`..`、文件不存在）。
    pub fn validate_files(&self, dir: &Path) -> Result<(), String> {
        let mut names: Vec<&String> = self.files.keys().collect();
        names.sort();
        for name in names {
            let relative = Path::new(&self.files[name]);
            let escapes = relative.is_absolute()
                || relative.components().any(|c| {
                    matches!(c, Component::ParentDir | Component::RootDir | Component::Prefix(_))
                });
            if escapes {
                return Err(format!("文件 {name} 的路径必须是模型目录内的相对路径"));
            }
            if !dir.join(relative).is_file() {
                return Err(format!("文件 {name} 不存在: {}", self.files[name]));
            }
        }
        Ok(())
    }

    /// 模型支持的有向语言对（`pairs` 优先；缺省取 `languages` 的全排列，不含 `Auto`）。
    ///
    /// # 返回
    /// 去重后的 `(源, 目标)` 列表，无法识别的语言代码被忽略。
    ///
    /// # 示例
    /// ```rust
    /// use snow_translate::{Lang, ModelManifest};
    /// let manifest: ModelManifest = serde_json::from_str(
    ///     r#"{"schema_version":1,"id":"m","family":"marian","pairs":[["en","zh-CN"]]}"#,
    /// ).unwrap();
    /// assert_eq!(manifest.supported_pairs(), vec![(Lang::En, Lang::ZhHans)]);
    /// ```
    pub fn supported_pairs(&self) -> Vec<(Lang, Lang)> {
        let mut out: Vec<(Lang, Lang)> = Vec::new();
        let mut push = |src: Lang, tgt: Lang| {
            if src != tgt && src != Lang::Auto && tgt != Lang::Auto && !out.contains(&(src, tgt)) {
                out.push((src, tgt));
            }
        };
        if self.pairs.is_empty() {
            let langs: Vec<Lang> = self.languages.iter().filter_map(|c| Lang::from_code(c)).collect();
            for src in &langs {
                for tgt in &langs {
                    push(*src, *tgt);
                }
            }
        } else {
            for (src, tgt) in &self.pairs {
                if let (Some(s), Some(t)) = (Lang::from_code(src), Lang::from_code(tgt)) {
                    push(s, t);
                }
            }
        }
        out
    }

    /// 是否专用包：清单用 `pairs` 显式声明了有向语言对（通用多语包只写 `languages`）。
    ///
    /// # 示例
    /// ```rust
    /// use snow_translate::ModelManifest;
    /// let opus: ModelManifest = serde_json::from_str(
    ///     r#"{"schema_version":1,"id":"o","family":"marian","pairs":[["en","zh-CN"]]}"#,
    /// ).unwrap();
    /// let nllb: ModelManifest = serde_json::from_str(
    ///     r#"{"schema_version":1,"id":"n","family":"m2m100","languages":["en","zh-CN"]}"#,
    /// ).unwrap();
    /// assert!(opus.is_specialized() && !nllb.is_specialized());
    /// ```
    pub fn is_specialized(&self) -> bool {
        !self.pairs.is_empty()
    }

    /// 是否支持该语言对；源语言为 `Auto` 时只要有任何源语言能翻到目标即可。
    ///
    /// # 参数
    /// - `src`：源语言。
    /// - `tgt`：目标语言。
    pub fn supports(&self, src: Lang, tgt: Lang) -> bool {
        self.resolve_source(src, tgt).is_some()
    }

    /// 把 `Auto` 源语言解析为具体源语言：取支持该目标的第一个语言对的源语言。
    ///
    /// # 参数
    /// - `src`：源语言（非 `Auto` 时原样返回，前提是支持）。
    /// - `tgt`：目标语言。
    ///
    /// # 返回
    /// 具体源语言；模型不支持时返回 `None`。
    pub fn resolve_source(&self, src: Lang, tgt: Lang) -> Option<Lang> {
        let pairs = self.supported_pairs();
        if src == Lang::Auto {
            return pairs.iter().find(|(_, t)| *t == tgt).map(|(s, _)| *s);
        }
        pairs.contains(&(src, tgt)).then_some(src)
    }
}

/// 扫描到的一个模型：清单与所在目录。
#[derive(Debug, Clone, PartialEq)]
pub struct ScannedModel {
    /// 模型清单。
    pub manifest: ModelManifest,
    /// 模型目录（含 `model.json`）。
    pub dir: PathBuf,
}

/// 一份无法使用的模型清单及原因（设置页展示，避免“放了模型却不显示”）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ManifestIssue {
    /// 模型目录名。
    pub dir_name: String,
    /// 原因说明。
    pub reason: String,
}

/// 一次扫描的完整结果。
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ScanReport {
    /// 可用模型（按 id 排序，保证选择稳定）。
    pub models: Vec<ScannedModel>,
    /// 无法使用的清单（按目录名排序）。
    pub issues: Vec<ManifestIssue>,
}

/// 本地翻译模型扫描器。
pub struct ModelScanner {
    /// 模型根目录路径。
    models_dir: PathBuf,
}

impl ModelScanner {
    /// 构造模型扫描器。
    ///
    /// # 参数
    /// - `models_dir`: 存放翻译模型的根目录。
    ///
    /// # 示例
    /// ```rust
    /// use std::path::Path;
    /// use snow_translate::ModelScanner;
    /// let scanner = ModelScanner::new(Path::new("D:/models/translate"));
    /// assert_eq!(scanner.models_dir(), Path::new("D:/models/translate"));
    /// ```
    pub fn new(models_dir: &Path) -> Self {
        Self {
            models_dir: models_dir.to_path_buf(),
        }
    }

    /// 模型根目录。
    pub fn models_dir(&self) -> &Path {
        &self.models_dir
    }

    /// 扫描目录下全部模型，并收集无法使用的清单及原因。
    ///
    /// 没有 `model.json` 的子目录视为无关目录，不计入问题；清单解析失败、版本不符、
    /// 文件缺失或路径越界都会记成 [`ManifestIssue`]。
    ///
    /// # 返回
    /// [`ScanReport`]；根目录不存在或不可读时为空报告。
    pub fn scan(&self) -> ScanReport {
        let mut report = ScanReport::default();
        let Ok(entries) = fs::read_dir(&self.models_dir) else {
            return report;
        };
        for entry in entries.flatten() {
            let dir = entry.path();
            if !dir.is_dir() {
                continue;
            }
            let manifest_path = dir.join(MANIFEST_FILE);
            if !manifest_path.is_file() {
                continue;
            }
            let dir_name = entry.file_name().to_string_lossy().into_owned();
            match Self::load_manifest(&manifest_path, &dir) {
                Ok(manifest) => report.models.push(ScannedModel { manifest, dir }),
                Err(reason) => report.issues.push(ManifestIssue { dir_name, reason }),
            }
        }
        report.models.sort_by(|a, b| a.manifest.id.cmp(&b.manifest.id));
        report.issues.sort_by(|a, b| a.dir_name.cmp(&b.dir_name));
        report
    }

    /// 读取并校验一份清单。
    fn load_manifest(path: &Path, dir: &Path) -> Result<ModelManifest, String> {
        let text = fs::read_to_string(path).map_err(|e| format!("无法读取 {MANIFEST_FILE}: {e}"))?;
        let manifest: ModelManifest = serde_json::from_str(text.trim_start_matches('\u{feff}'))
            .map_err(|e| format!("{MANIFEST_FILE} 解析失败: {e}"))?;
        manifest.validate().map_err(|e| e.to_string())?;
        manifest.validate_files(dir)?;
        Ok(manifest)
    }

    /// 扫描目录下的全部可用模型清单（忽略问题清单）。
    ///
    /// # 返回
    /// 成功找到的模型清单列表；若目录不存在或为空返回空列表。
    pub fn scan_models(&self) -> Vec<ModelManifest> {
        self.scan().models.into_iter().map(|m| m.manifest).collect()
    }
}

/// 在已扫描的模型里选出适合某语言对的一个，并把 `Auto` 源语言解析为具体语言（指定包模式）。
///
/// 优先用 `preferred_id`（配置里的默认模型）；它不存在或不支持该语言对时，退回到
/// 第一个（按 id 排序）支持该语言对的模型。需要“专用包优先”时用 [`pick_model_routed`]。
///
/// # 参数
/// - `models`：扫描到的可用模型。
/// - `preferred_id`：配置里的默认模型 ID，空串表示无偏好。
/// - `src` / `tgt`：语言对（`src` 可为 `Auto`）。
///
/// # 返回
/// `(模型, 具体源语言)`；没有任何模型返回 `NoModelFound`，有模型但都不支持返回
/// `UnsupportedLanguagePair`。
///
/// # 示例
/// ```rust
/// use snow_translate::{Lang, pick_model};
/// let err = pick_model(&[], "", Lang::Auto, Lang::ZhHans).unwrap_err();
/// assert!(matches!(err, snow_translate::TranslateError::NoModelFound(_)));
/// ```
pub fn pick_model<'a>(
    models: &'a [ScannedModel],
    preferred_id: &str,
    src: Lang,
    tgt: Lang,
) -> Result<(&'a ScannedModel, Lang), TranslateError> {
    pick_model_routed(models, preferred_id, src, tgt, router::RouteMode::Single)
}

/// 按路由模式选包：`Single` 同 [`pick_model`]；`SpecializedFirst` / `MixedSplit` 在未指定包
/// （或指定包不支持）时优先选显式声明语言对的专用包，规则见 [`router::pick_index`]。
///
/// # 参数
/// - `models`：扫描到的可用模型。
/// - `preferred_id`：指定的模型 ID，空串表示无偏好。
/// - `src` / `tgt`：语言对（`src` 可为 `Auto`）。
/// - `mode`：路由模式。
///
/// # 返回
/// `(模型, 具体源语言)`；错误同 [`pick_model`]。
///
/// # 示例
/// ```rust
/// use snow_translate::router::RouteMode;
/// use snow_translate::{Lang, pick_model_routed};
/// let err = pick_model_routed(&[], "", Lang::En, Lang::ZhHans, RouteMode::SpecializedFirst).unwrap_err();
/// assert!(matches!(err, snow_translate::TranslateError::NoModelFound(_)));
/// ```
pub fn pick_model_routed<'a>(
    models: &'a [ScannedModel],
    preferred_id: &str,
    src: Lang,
    tgt: Lang,
    mode: router::RouteMode,
) -> Result<(&'a ScannedModel, Lang), TranslateError> {
    if models.is_empty() {
        return Err(TranslateError::NoModelFound("模型目录里没有可用的模型".into()));
    }
    let manifests: Vec<&ModelManifest> = models.iter().map(|m| &m.manifest).collect();
    let index = router::pick_index(&manifests, preferred_id, src, tgt, mode)
        .ok_or(TranslateError::UnsupportedLanguagePair(src, tgt))?;
    let model = &models[index];
    let resolved = model.manifest.resolve_source(src, tgt).ok_or(TranslateError::UnsupportedLanguagePair(src, tgt))?;
    Ok((model, resolved))
}

/// 翻译引擎后端统一接口（ADR-5）。
pub trait TranslationEngine: Send + Sync {
    /// 执行文本翻译。
    ///
    /// # 参数
    /// - `text`: 待翻译的原始文本。
    /// - `src`: 源语言。
    /// - `tgt`: 目标语言。
    ///
    /// # 返回
    /// 翻译产出文本，或返回对应错误。
    fn translate(&self, text: &str, src: Lang, tgt: Lang) -> Result<String, TranslateError>;

    /// 批量翻译：结果与 `texts` 一一对应。默认逐条调用 [`Self::translate`]。
    ///
    /// # 参数
    /// - `texts`: 待翻译文本列表。
    /// - `src` / `tgt`: 语言对。
    fn translate_batch(
        &self,
        texts: &[String],
        src: Lang,
        tgt: Lang,
    ) -> Result<Vec<String>, TranslateError> {
        texts.iter().map(|t| self.translate(t, src, tgt)).collect()
    }

    /// 返回当前后端支持的语言对组合。
    fn supported_pairs(&self) -> Vec<(Lang, Lang)>;

    /// 引擎类型标识名称。
    fn engine_name(&self) -> &'static str;

    /// 缓存标识：同一后端换模型或换参数时必须变化，避免读到旧译文。
    fn cache_id(&self) -> String {
        self.engine_name().to_string()
    }
}

/// 有上限的翻译结果缓存（先进先出淘汰）。
#[derive(Debug, Default)]
struct TranslationCache {
    /// 键到译文。
    entries: HashMap<String, String>,
    /// 写入顺序，用于淘汰。
    order: VecDeque<String>,
}

impl TranslationCache {
    /// 取缓存。
    fn get(&self, key: &str) -> Option<&String> {
        self.entries.get(key)
    }

    /// 写缓存，超出容量淘汰最早的。
    fn put(&mut self, key: String, value: String) {
        if self.entries.insert(key.clone(), value).is_none() {
            self.order.push_back(key);
        }
        while self.order.len() > CACHE_CAPACITY {
            if let Some(old) = self.order.pop_front() {
                self.entries.remove(&old);
            }
        }
    }

    /// 清空。
    fn clear(&mut self) {
        self.entries.clear();
        self.order.clear();
    }
}

/// 翻译服务总控制器：持有当前后端与内存结果缓存，可在多线程间共享。
#[derive(Default)]
pub struct TranslationService {
    /// 当前生效的翻译引擎（未设置时翻译一律返回 `NoModelFound`）。
    engine: RwLock<Option<Arc<dyn TranslationEngine>>>,
    /// 翻译结果缓存。
    cache: Mutex<TranslationCache>,
}

impl TranslationService {
    /// 构造没有后端的翻译服务。
    ///
    /// # 示例
    /// ```rust
    /// use snow_translate::{Lang, TranslationService};
    /// let svc = TranslationService::new();
    /// assert!(svc.translate("hello", Lang::En, Lang::ZhHans).is_err());
    /// ```
    pub fn new() -> Self {
        Self::default()
    }

    /// 切换底层翻译引擎并清空缓存；传 `None` 表示没有后端。
    ///
    /// # 参数
    /// - `engine`：新的后端。旧后端的最后一个引用释放时，其 worker 进程随之退出。
    pub fn set_engine(&self, engine: Option<Arc<dyn TranslationEngine>>) {
        *self.engine.write().unwrap_or_else(PoisonError::into_inner) = engine;
        self.cache.lock().unwrap_or_else(PoisonError::into_inner).clear();
    }

    /// 当前引擎（克隆的共享引用）。
    pub fn engine(&self) -> Option<Arc<dyn TranslationEngine>> {
        self.engine.read().unwrap_or_else(PoisonError::into_inner).clone()
    }

    /// 当前翻译引擎名称；没有后端时为 `None`。
    pub fn current_engine_name(&self) -> Option<&'static str> {
        self.engine().map(|e| e.engine_name())
    }

    /// 执行翻译并自动走缓存。
    ///
    /// # 参数
    /// - `text`：待翻译文本。
    /// - `src` / `tgt`：语言对。
    pub fn translate(&self, text: &str, src: Lang, tgt: Lang) -> Result<String, TranslateError> {
        let mut out = self.translate_batch(&[text.to_string()], src, tgt)?;
        out.pop()
            .ok_or_else(|| TranslateError::Inference("后端没有返回译文".into()))
    }

    /// 批量翻译：命中缓存的条目不再请求后端，其余合并成一次后端调用。
    ///
    /// # 参数
    /// - `texts`：待翻译文本。
    /// - `src` / `tgt`：语言对。
    ///
    /// # 返回
    /// 与 `texts` 一一对应的译文；没有后端返回 `NoModelFound`。
    pub fn translate_batch(
        &self,
        texts: &[String],
        src: Lang,
        tgt: Lang,
    ) -> Result<Vec<String>, TranslateError> {
        let engine = self
            .engine()
            .ok_or_else(|| TranslateError::NoModelFound("尚未配置翻译后端".into()))?;
        let id = engine.cache_id();
        let key_of = |text: &str| format!("{id}|{}>{}|{}", src.code(), tgt.code(), text.trim());
        let mut results: Vec<Option<String>> = vec![None; texts.len()];
        let mut misses: Vec<usize> = Vec::new();
        {
            let cache = self.cache.lock().unwrap_or_else(PoisonError::into_inner);
            for (index, text) in texts.iter().enumerate() {
                match cache.get(&key_of(text)) {
                    Some(hit) => results[index] = Some(hit.clone()),
                    None => misses.push(index),
                }
            }
        }
        if !misses.is_empty() {
            // 相同原文只翻译一次
            let mut unique: Vec<String> = Vec::new();
            let mut seen: HashSet<&str> = HashSet::new();
            for &index in &misses {
                if seen.insert(texts[index].as_str()) {
                    unique.push(texts[index].clone());
                }
            }
            let translated = engine.translate_batch(&unique, src, tgt)?;
            if translated.len() != unique.len() {
                return Err(TranslateError::Inference("后端返回的译文条数不符".into()));
            }
            let mut cache = self.cache.lock().unwrap_or_else(PoisonError::into_inner);
            for (source, target) in unique.iter().zip(&translated) {
                cache.put(key_of(source), target.clone());
            }
            for &index in &misses {
                let position = unique.iter().position(|u| *u == texts[index]);
                results[index] = position.and_then(|p| translated.get(p).cloned());
            }
        }
        results
            .into_iter()
            .map(|r| r.ok_or_else(|| TranslateError::Inference("缺少译文".into())))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// locale 映射：简繁中文、常见语言与未知值。
    #[test]
    fn lang_from_locale_cases() {
        for l in ["zh-CN", "zh-Hans-CN", "zh", "zh_SG"] {
            assert_eq!(Lang::from_locale(l), Some(Lang::ZhHans), "{l}");
        }
        for l in ["zh-TW", "zh-Hant-TW", "zh-HK", "zh_MO"] {
            assert_eq!(Lang::from_locale(l), None, "{l} 繁体不支持");
        }
        assert_eq!(Lang::from_locale("en-US"), Some(Lang::En));
        assert_eq!(Lang::from_locale("ja-JP"), Some(Lang::Ja));
        assert_eq!(Lang::from_locale("pt-BR"), Some(Lang::Pt));
        assert_eq!(Lang::from_locale("xx-YY"), None);
        assert_eq!(Lang::from_locale("auto"), None);
        assert_eq!(Lang::from_locale(""), None);
    }

    /// 唯一临时目录。
    fn temp_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("snow-translate-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).expect("建目录");
        dir
    }

    /// 写一份模型目录（含占位文件）。
    fn write_model(root: &Path, dir: &str, manifest: &str, files: &[&str]) {
        let model_dir = root.join(dir);
        fs::create_dir_all(&model_dir).expect("建模型目录");
        fs::write(model_dir.join(MANIFEST_FILE), manifest).expect("写清单");
        for file in files {
            fs::write(model_dir.join(file), b"x").expect("写文件");
        }
    }

    /// 最小可用清单文本。
    fn manifest_text(id: &str, extra: &str) -> String {
        format!(
            r#"{{"schema_version":1,"id":"{id}","family":"marian","files":{{"encoder":"e.onnx","decoder":"d.onnx","tokenizer":"t.json"}},"languages":["en","zh-CN"]{extra}}}"#
        )
    }

    /// 语言码解析与格式化（含新增语言与配置里的写法）。
    #[test]
    fn test_lang_code_and_display() {
        assert_eq!(Lang::ZhHans.code(), "zh-CN");
        assert_eq!(Lang::from_code("zh-cn"), Some(Lang::ZhHans));
        assert_eq!(Lang::from_code("zh-Hans"), Some(Lang::ZhHans));
        assert_eq!(Lang::from_code("zh-Hant"), Some(Lang::ZhHant));
        assert_eq!(Lang::from_code("en"), Some(Lang::En));
        assert_eq!(Lang::from_code("pt"), Some(Lang::Pt));
        assert_eq!(Lang::from_code("tr"), Some(Lang::Tr));
        assert_eq!(Lang::Auto.display_name(), "自动检测");
        // 配置白名单里的每种语言都必须能解析
        for code in ["auto", "ar", "de", "en", "es", "fr", "it", "ja", "pt", "ru", "tr", "zh-Hans", "zh-Hant"] {
            assert!(Lang::from_code(code).is_some(), "{code}");
        }
    }

    /// 旧格式清单（无扩展字段）仍可解析；缺省字段有合理默认。
    #[test]
    fn old_manifest_still_parses() {
        let manifest: ModelManifest = serde_json::from_str(
            r#"{"schema_version":1,"id":"nllb","display_name":"N","family":"nllb","quantization":"int8",
                "files":{"encoder":"encoder.onnx"},"languages":["zh-CN","en"],"max_input_tokens":512}"#,
        )
        .expect("旧格式");
        assert!(manifest.validate().is_ok());
        assert!(manifest.pairs.is_empty() && manifest.sha256.is_empty());
        let minimal: ModelManifest =
            serde_json::from_str(r#"{"schema_version":1,"id":"m","family":"marian","languages":["en"]}"#)
                .expect("最小清单");
        assert_eq!(minimal.max_input_tokens, 512);
        assert!(minimal.display_name.is_empty());
    }

    /// 校验：版本、空 id、空语言都被拒绝。
    #[test]
    fn manifest_validation() {
        let mut manifest: ModelManifest =
            serde_json::from_str(&manifest_text("m", "")).expect("清单");
        assert!(manifest.validate().is_ok());
        manifest.schema_version = 2;
        assert!(manifest.validate().is_err());
        manifest.schema_version = 1;
        manifest.id = "  ".into();
        assert!(manifest.validate().is_err());
        manifest.id = "m".into();
        manifest.languages.clear();
        assert!(manifest.validate().is_err());
    }

    /// 语言对：pairs 优先；缺省取 languages 全排列；Auto 解析为具体源语言。
    #[test]
    fn pairs_expansion_and_auto_resolution() {
        let plain: ModelManifest = serde_json::from_str(&manifest_text("m", "")).expect("清单");
        assert_eq!(
            plain.supported_pairs(),
            vec![(Lang::En, Lang::ZhHans), (Lang::ZhHans, Lang::En)]
        );
        let directed: ModelManifest = serde_json::from_str(&manifest_text(
            "m",
            r#","pairs":[["en","zh-CN"],["en","zh-TW"],["xx","en"]]"#,
        ))
        .expect("清单");
        assert_eq!(
            directed.supported_pairs(),
            vec![(Lang::En, Lang::ZhHans), (Lang::En, Lang::ZhHant)]
        );
        assert!(directed.supports(Lang::En, Lang::ZhHant));
        assert!(!directed.supports(Lang::ZhHans, Lang::En));
        assert_eq!(directed.resolve_source(Lang::Auto, Lang::ZhHans), Some(Lang::En));
        assert_eq!(directed.resolve_source(Lang::Auto, Lang::Ja), None);
        assert!(!directed.supports(Lang::Auto, Lang::En));
    }

    /// 文件校验：缺文件、绝对路径、`..` 越界都给出明确原因。
    #[test]
    fn validate_files_reports_reasons() {
        let root = temp_dir("files");
        write_model(&root, "ok", &manifest_text("ok", ""), &["e.onnx", "d.onnx", "t.json"]);
        let ok: ModelManifest = serde_json::from_str(&manifest_text("ok", "")).expect("清单");
        assert!(ok.validate_files(&root.join("ok")).is_ok());
        assert!(ok.validate_files(&root).unwrap_err().contains("不存在"));
        let mut escaping = ok.clone();
        escaping.files.insert("encoder".into(), "../e.onnx".into());
        assert!(escaping.validate_files(&root.join("ok")).unwrap_err().contains("相对路径"));
        let mut absolute = ok.clone();
        absolute.files.insert("encoder".into(), "C:/Windows/notepad.exe".into());
        assert!(absolute.validate_files(&root.join("ok")).unwrap_err().contains("相对路径"));
        let _ = fs::remove_dir_all(&root);
    }

    /// NLLB（m2m100）模型包清单：带外部数据文件与 generation/execution 扩展字段仍可解析，
    /// 外部数据文件缺失会被扫描报告；暂无枚举的语言（vi/id）被忽略而不报错。
    #[test]
    fn m2m100_pack_manifest_scans() {
        let text = r#"{"schema_version":1,"id":"nllb","family":"m2m100","quantization":"int4",
            "files":{"encoder":"encoder.onnx","encoder_data":"encoder.onnx_data","decoder":"decoder.onnx","decoder_data":"decoder.onnx_data","tokenizer":"tokenizer.json"},
            "languages":["zh-CN","en","vi"],"lang_tokens":{"zh-CN":"zho_Hans","en":"eng_Latn","vi":"vie_Latn"},
            "generation":{"num_beams":2,"bad_token_ids":[],"min_length_ratio":0.5},
            "execution":{"prepacking":true}}"#;
        let manifest: ModelManifest = serde_json::from_str(text).expect("m2m100 清单");
        assert!(manifest.validate().is_ok());
        assert_eq!(manifest.supported_pairs(), vec![(Lang::ZhHans, Lang::En), (Lang::En, Lang::ZhHans)]);
        let root = temp_dir("m2m100");
        let all = ["encoder.onnx", "encoder.onnx_data", "decoder.onnx", "decoder.onnx_data", "tokenizer.json"];
        write_model(&root, "full", text, &all);
        write_model(&root, "no-data", text, &["encoder.onnx", "decoder.onnx", "tokenizer.json"]);
        let report = ModelScanner::new(&root).scan();
        assert_eq!(report.models.len(), 1);
        assert_eq!(report.models[0].manifest.family, "m2m100");
        assert_eq!(report.issues.len(), 1);
        assert_eq!(report.issues[0].dir_name, "no-data");
        let _ = fs::remove_dir_all(&root);
    }

    /// 扫描：可用模型与问题清单分开报告，无清单目录被忽略，结果排序稳定。
    #[test]
    fn scanner_reports_models_and_issues() {
        let root = temp_dir("scan");
        write_model(&root, "b-model", &manifest_text("b", ""), &["e.onnx", "d.onnx", "t.json"]);
        write_model(&root, "a-model", &manifest_text("a", ""), &["e.onnx", "d.onnx", "t.json"]);
        write_model(&root, "broken-json", "{not json", &[]);
        write_model(&root, "missing-file", &manifest_text("mf", ""), &["e.onnx"]);
        write_model(&root, "bad-version", &manifest_text("bv", "").replace("\"schema_version\":1", "\"schema_version\":9"), &[]);
        fs::create_dir_all(root.join("unrelated")).expect("无关目录");
        fs::write(root.join("stray.txt"), b"x").expect("散文件");
        let report = ModelScanner::new(&root).scan();
        let ids: Vec<&str> = report.models.iter().map(|m| m.manifest.id.as_str()).collect();
        assert_eq!(ids, ["a", "b"]);
        let names: Vec<&str> = report.issues.iter().map(|i| i.dir_name.as_str()).collect();
        assert_eq!(names, ["bad-version", "broken-json", "missing-file"]);
        assert!(report.issues.iter().any(|i| i.reason.contains("解析失败")));
        assert!(report.issues.iter().any(|i| i.reason.contains("不存在")));
        assert_eq!(ModelScanner::new(&root).scan_models().len(), 2);
        assert_eq!(report.models[0].dir, root.join("a-model"));
        assert_eq!(ModelScanner::new(&root.join("nope")).scan(), ScanReport::default());
        let _ = fs::remove_dir_all(&root);
    }

    /// BOM 开头的清单（记事本保存）也能解析。
    #[test]
    fn manifest_with_bom_is_accepted() {
        let root = temp_dir("bom");
        let text = format!("\u{feff}{}", manifest_text("bom", ""));
        write_model(&root, "m", &text, &["e.onnx", "d.onnx", "t.json"]);
        assert_eq!(ModelScanner::new(&root).scan().models.len(), 1);
        let _ = fs::remove_dir_all(&root);
    }

    /// 选模型：偏好优先；偏好不支持时退回第一个支持的；Auto 被解析；无模型/不支持分别报错。
    #[test]
    fn pick_model_rules() {
        let en_zh: ModelManifest = serde_json::from_str(&manifest_text("en-zh", r#","pairs":[["en","zh-CN"]]"#)).expect("清单");
        let en_zh_b: ModelManifest = serde_json::from_str(&manifest_text("z-en-zh", r#","pairs":[["en","zh-CN"]]"#)).expect("清单");
        let zh_en: ModelManifest = serde_json::from_str(&manifest_text("zh-en", r#","pairs":[["zh-CN","en"]]"#)).expect("清单");
        let models: Vec<ScannedModel> = [en_zh, en_zh_b, zh_en]
            .into_iter()
            .map(|manifest| ScannedModel { manifest, dir: PathBuf::from("d") })
            .collect();
        let (m, src) = pick_model(&models, "", Lang::Auto, Lang::ZhHans).expect("auto");
        assert_eq!((m.manifest.id.as_str(), src), ("en-zh", Lang::En));
        let (m, _) = pick_model(&models, "z-en-zh", Lang::En, Lang::ZhHans).expect("偏好");
        assert_eq!(m.manifest.id, "z-en-zh");
        let (m, _) = pick_model(&models, "zh-en", Lang::En, Lang::ZhHans).expect("偏好不支持时退回");
        assert_eq!(m.manifest.id, "en-zh");
        assert!(matches!(
            pick_model(&models, "", Lang::En, Lang::Ja),
            Err(TranslateError::UnsupportedLanguagePair(Lang::En, Lang::Ja))
        ));
        assert!(matches!(pick_model(&[], "", Lang::En, Lang::ZhHans), Err(TranslateError::NoModelFound(_))));
    }

    /// 按路由模式选包：专用包（显式 pairs）优先于通用多语包；指定包仍被尊重；single 同旧行为。
    #[test]
    fn pick_model_routed_prefers_specialized() {
        let general: ModelManifest =
            serde_json::from_str(&manifest_text("a-general", "")).expect("清单");
        let opus: ModelManifest =
            serde_json::from_str(&manifest_text("z-opus", r#","pairs":[["en","zh-CN"]]"#)).expect("清单");
        let models: Vec<ScannedModel> = [general, opus]
            .into_iter()
            .map(|manifest| ScannedModel { manifest, dir: PathBuf::from("d") })
            .collect();
        let id = |mode, preferred: &str, src, tgt| {
            pick_model_routed(&models, preferred, src, tgt, mode).map(|(m, s)| (m.manifest.id.clone(), s))
        };
        use router::RouteMode::{MixedSplit, Single, SpecializedFirst};
        assert_eq!(id(Single, "", Lang::En, Lang::ZhHans).unwrap().0, "a-general");
        assert_eq!(id(SpecializedFirst, "", Lang::En, Lang::ZhHans).unwrap(), ("z-opus".into(), Lang::En));
        assert_eq!(id(SpecializedFirst, "", Lang::Auto, Lang::ZhHans).unwrap(), ("z-opus".into(), Lang::En));
        assert_eq!(id(SpecializedFirst, "a-general", Lang::En, Lang::ZhHans).unwrap().0, "a-general");
        assert_eq!(id(MixedSplit, "a-general", Lang::En, Lang::ZhHans).unwrap().0, "z-opus");
        assert_eq!(id(SpecializedFirst, "", Lang::ZhHans, Lang::En).unwrap().0, "a-general", "专用包不覆盖时用通用包");
        assert!(matches!(
            id(SpecializedFirst, "", Lang::En, Lang::Ja),
            Err(TranslateError::UnsupportedLanguagePair(Lang::En, Lang::Ja))
        ));
    }

    /// 计数用假引擎：把文本加前缀返回。
    struct CountingEngine {
        /// 后端被调用的文本条数。
        calls: AtomicUsize,
        /// 缓存标识。
        id: String,
    }

    impl TranslationEngine for CountingEngine {
        fn translate(&self, text: &str, _src: Lang, _tgt: Lang) -> Result<String, TranslateError> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            Ok(format!("T:{text}"))
        }

        fn supported_pairs(&self) -> Vec<(Lang, Lang)> {
            vec![(Lang::En, Lang::ZhHans)]
        }

        fn engine_name(&self) -> &'static str {
            "counting"
        }

        fn cache_id(&self) -> String {
            self.id.clone()
        }
    }

    /// 服务：没有后端报 NoModelFound；缓存命中不再调用后端；同批相同原文只翻译一次。
    #[test]
    fn service_cache_and_dedup() {
        let svc = TranslationService::new();
        assert!(matches!(
            svc.translate("a", Lang::En, Lang::ZhHans),
            Err(TranslateError::NoModelFound(_))
        ));
        let engine = Arc::new(CountingEngine { calls: AtomicUsize::new(0), id: "m1".into() });
        svc.set_engine(Some(engine.clone()));
        assert_eq!(svc.translate("copy", Lang::En, Lang::ZhHans).unwrap(), "T:copy");
        assert_eq!(svc.translate(" copy ", Lang::En, Lang::ZhHans).unwrap(), "T:copy");
        assert_eq!(engine.calls.load(Ordering::SeqCst), 1);
        let batch = vec!["x".to_string(), "copy".to_string(), "x".to_string(), "y".to_string()];
        let out = svc.translate_batch(&batch, Lang::En, Lang::ZhHans).unwrap();
        assert_eq!(out, ["T:x", "T:copy", "T:x", "T:y"]);
        assert_eq!(engine.calls.load(Ordering::SeqCst), 3, "只新增 x 与 y 两次调用");
        assert_eq!(svc.current_engine_name(), Some("counting"));
    }

    /// 换后端清空缓存；cache_id 不同的引擎互不串味。
    #[test]
    fn service_cache_is_per_engine() {
        let svc = TranslationService::new();
        let first = Arc::new(CountingEngine { calls: AtomicUsize::new(0), id: "m1".into() });
        svc.set_engine(Some(first));
        svc.translate("a", Lang::En, Lang::ZhHans).unwrap();
        let second = Arc::new(CountingEngine { calls: AtomicUsize::new(0), id: "m2".into() });
        svc.set_engine(Some(second.clone()));
        svc.translate("a", Lang::En, Lang::ZhHans).unwrap();
        assert_eq!(second.calls.load(Ordering::SeqCst), 1);
        svc.set_engine(None);
        assert!(svc.translate("a", Lang::En, Lang::ZhHans).is_err());
    }

    /// 缓存有上限，淘汰最早写入的条目。
    #[test]
    fn cache_is_bounded() {
        let mut cache = TranslationCache::default();
        for i in 0..(CACHE_CAPACITY + 10) {
            cache.put(format!("k{i}"), "v".into());
        }
        assert_eq!(cache.entries.len(), CACHE_CAPACITY);
        assert!(cache.get("k0").is_none());
        assert!(cache.get(&format!("k{}", CACHE_CAPACITY + 9)).is_some());
        cache.put("k20".into(), "new".into());
        assert_eq!(cache.entries.len(), CACHE_CAPACITY, "覆盖已有键不增加条目");
        cache.clear();
        assert!(cache.entries.is_empty() && cache.order.is_empty());
    }

    /// 错误文案非空；只有运行时缺失可以下载解决。
    #[test]
    fn error_messages_and_download_flag() {
        let errors = [
            TranslateError::NoModelFound("x".into()),
            TranslateError::UnsupportedLanguagePair(Lang::En, Lang::Ja),
            TranslateError::Timeout,
            TranslateError::RuntimeMissing("dll".into()),
            TranslateError::WorkerDied("boom".into()),
        ];
        for e in &errors {
            assert!(!e.to_string().is_empty());
        }
        assert!(errors[3].can_download_runtime());
        assert!(!errors[0].can_download_runtime());
        assert!(errors[1].to_string().contains("英语"));
    }
}
