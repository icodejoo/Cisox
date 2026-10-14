//! 文字翻译的宿主服务：配置解析、后端装配（本地 NMT worker / OpenAI 兼容）与“选区 → OCR → 翻译”流程。
//!
//! 后端由 `snow-translate` 提供；这里负责把设置页的配置变成具体引擎，并在配置变化时换掉旧引擎
//! （旧引擎释放即结束其 worker 进程）。没有模型、没有运行时、OCR 缺资产时一律返回明确错误，
//! 不会产出任何假译文。翻译是阻塞调用，必须在后台线程里执行。

use crate::ocr_client::OcrError;
use crate::ocr_service::{OcrResult, OcrTextBox};
use crate::ort_runtime::{OrtUnavailable, resolve_ort_dylib};
use crate::translate_layout::{LayoutMode, paragraphs_from_boxes};
use serde_json::Value;
use snow_config::custom_models::{CustomAiModel, custom_ai_models_from_json};
use snow_config::document::ConfigDocument;
use snow_config::extensions::{
    BACKEND_OPENAI, DEFAULT_IDLE_SECONDS, DEFAULT_MAX_RESIDENT, DEFAULT_NUM_BEAMS,
    KEY_LOCAL_IDLE_SECONDS, KEY_LOCAL_LOW_MEMORY, KEY_LOCAL_MAX_RESIDENT, KEY_LOCAL_MODEL_ID,
    KEY_LOCAL_MODELS_DIR, KEY_LOCAL_NUM_BEAMS, KEY_LOCAL_ROUTE_MODE, KEY_TRANSLATION_BACKEND,
    MAX_MAX_RESIDENT, MAX_NUM_BEAMS,
};
use snow_translate::openai::{OpenAiCompatibleConfig, OpenAiEngine};
use snow_translate::protocol::MAX_BEAMS;
use snow_translate::router::{PooledEngine, RouteMode, RoutePolicy, RoutedEngine, RoutedSlot};
use snow_translate::worker::{
    MemorySnapshot, ProcessLauncher, Timeouts, WORKER_EXE_NAME, WorkerConfig, WorkerEngine,
    WorkerLauncher,
};
use snow_translate::{
    Lang, ManifestIssue, ModelScanner, ScanReport, ScannedModel, TranslateError, TranslationEngine,
    TranslationService, pick_model_routed,
};
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

/// 配置键：源语言。
pub const KEY_SOURCE_LANGUAGE: &str = "screenshot_translation/source_language";
/// 配置键：目标语言。
pub const KEY_TARGET_LANGUAGE: &str = "screenshot_translation/target_language";
/// 配置键：版式处理。
pub const KEY_LAYOUT: &str = "screenshot_translation/layout_processing";
/// 配置键：OpenAI 兼容通道使用的自定义模型 ID（沿用 Qt 版的键）。
pub const KEY_CUSTOM_MODEL: &str = "screenshot_translation/model";
/// 配置键：自定义模型列表。
pub const KEY_CUSTOM_MODELS: &str = "api_configuration/custom_models";
/// 环境变量：直接指定 `cisox-translator.exe`（开发 / 自测用）。
pub const ENV_TRANSLATOR_EXE: &str = "SNOW_TRANSLATOR_EXE";
/// 模型根目录相对数据根的路径。
const MODELS_SUBDIR: [&str; 2] = ["models", "translate"];
/// 说明里最多列出的问题清单条数。
const MAX_ISSUES_SHOWN: usize = 3;
/// 混合拆分时多个包展示名之间的连接符。
const LABEL_JOINER: &str = " + ";
/// 判定“原文已是中文”的汉字占比阈值。
const CJK_MAJORITY: f32 = 0.6;
/// 常见汉字区间。
const CJK_RANGE: std::ops::RangeInclusive<char> = '\u{4e00}'..='\u{9fff}';

/// 翻译后端选择。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Backend {
    /// 本地 NMT（独立 worker 进程）。
    Local,
    /// OpenAI 兼容通道。
    OpenAi,
}

/// 一次翻译请求的配置（来自设置页）。
#[derive(Debug, Clone, PartialEq)]
pub struct TranslateConfig {
    /// 后端。
    pub backend: Backend,
    /// 本地模型根目录；`None` 表示 `<数据根>/models/translate`。
    pub models_dir: Option<PathBuf>,
    /// 本地默认模型 ID，空串不指定。
    pub model_id: String,
    /// worker 空闲卸载时间。
    pub idle: Duration,
    /// 配置的束宽。
    pub beams: usize,
    /// 低内存模式（强制贪心 + 请求后收缩）。
    pub low_memory: bool,
    /// 本地多包路由模式。
    pub route_mode: RouteMode,
    /// 最多同时常驻内存的翻译包数。
    pub max_resident: usize,
    /// 源语言（可为 `Auto`）。
    pub source: Lang,
    /// 目标语言。
    pub target: Lang,
    /// 版式处理。
    pub layout: LayoutMode,
    /// OpenAI 通道选中的自定义模型 ID。
    pub custom_model_id: String,
    /// 全部自定义模型。
    pub custom_models: Vec<CustomAiModel>,
}

/// 设置页提供的具体目标语言及其配置值拼写（不含 `auto`、韩语与繁体）。
pub(crate) const SUPPORTED_TARGETS: [(Lang, &str); 11] = [
    (Lang::Ar, "ar"),
    (Lang::De, "de"),
    (Lang::En, "en"),
    (Lang::Es, "es"),
    (Lang::Fr, "fr"),
    (Lang::It, "it"),
    (Lang::Ja, "ja"),
    (Lang::Pt, "pt"),
    (Lang::Ru, "ru"),
    (Lang::Tr, "tr"),
    (Lang::ZhHans, "zh-Hans"),
];

/// 截图工具栏「翻译」二级菜单的目标语言：固定 14 种（不含自动与繁体），顺序即菜单顺序。
pub const TOOLBAR_TARGET_LANGS: [Lang; 14] = [
    Lang::ZhHans,
    Lang::En,
    Lang::Fr,
    Lang::Es,
    Lang::Ru,
    Lang::Ar,
    Lang::De,
    Lang::Ja,
    Lang::Ko,
    Lang::Pt,
    Lang::It,
    Lang::Tr,
    Lang::Vi,
    Lang::Id,
];

/// 把语言映射到支持集合内的配置值拼写；不在集合内返回 `None`。
fn target_code(lang: Lang) -> Option<&'static str> {
    SUPPORTED_TARGETS
        .iter()
        .find(|(l, _)| *l == lang)
        .map(|(_, code)| *code)
}

/// 目标语言的生效值：已保存的具体值原样保留；缺失/空串时取系统语言，
/// 映射不到或不在支持集合内则退英语。纯函数，不写回配置。
///
/// # 参数
/// - `config_value`：配置里已保存的目标语言；`None` 或空串表示用户没选过。
/// - `system_locale`：系统界面语言标记（如 `ja-JP`）。
///
/// # 返回
/// 目标语言配置值拼写（如 `zh-Hans`、`ja`），恒为设置页下拉里的合法项。
///
/// ```ignore
/// assert_eq!(effective_target_language(None, "ja-JP"), "ja");
/// assert_eq!(effective_target_language(Some("fr"), "ja-JP"), "fr");
/// ```
pub fn effective_target_language(config_value: Option<&str>, system_locale: &str) -> &'static str {
    let saved = config_value.map(str::trim).filter(|v| !v.is_empty());
    saved
        .and_then(Lang::from_code)
        .and_then(target_code)
        .or_else(|| Lang::from_locale(system_locale).and_then(target_code))
        .unwrap_or("en")
}

/// 界面语言的生效值：只支持内置语言（`locale.toml` 发现，目前 `en_US` / `zh_CN`）。已保存的支持值（拼写宽松）
/// 原样生效；缺失、空串、`system`、`auto`、旧繁体 `zh_TW` / `zh-Hant` 等都视为没保存，
/// 取系统语言映射，映射不到退 `en_US`。纯函数，不写回配置。
///
/// # 参数
/// - `config_value`：`interface/language` 已保存的值，`None` 表示没有。
/// - `system_locale`：系统界面语言标记（如 `zh-CN`）。
///
/// # 返回
/// 内置语言写入配置的取值，如 `"en_US"` / `"zh_CN"`。
///
/// ```ignore
/// assert_eq!(effective_interface_language(None, "zh-CN"), "zh_CN");
/// assert_eq!(effective_interface_language(Some("zh_TW"), "ja-JP"), "en_US");
/// ```
pub fn effective_interface_language(
    config_value: Option<&str>,
    system_locale: &str,
) -> &'static str {
    let saved = config_value.map(str::trim).filter(|v| !v.is_empty());
    saved
        .and_then(snow_i18n::match_locale)
        .or_else(|| snow_i18n::match_locale(system_locale))
        .or_else(|| snow_i18n::match_locale(snow_i18n::FALLBACK_LOCALE))
        .map_or("en_US", |info| info.config_value)
}

impl TranslateConfig {
    /// 从配置文档读取（缺失时用 schema 默认值）。
    ///
    /// # 参数
    /// - `document`：配置文档。
    /// - `system_language`：系统语言标记（目标语言留空时推导默认值）。
    ///
    /// ```ignore
    /// let cfg = TranslateConfig::from_document(store.document(), "zh-CN");
    /// ```
    pub fn from_document(document: &ConfigDocument, system_language: &str) -> Self {
        let text = |key: &str| match document.value(key) {
            Value::String(s) => s.trim().to_string(),
            _ => String::new(),
        };
        let number = |key: &str, default: i32| {
            document
                .value(key)
                .as_i64()
                .and_then(|n| i32::try_from(n).ok())
                .unwrap_or(default)
        };
        let models_dir = Some(text(KEY_LOCAL_MODELS_DIR))
            .filter(|d| !d.is_empty())
            .map(PathBuf::from);
        let saved = text(KEY_TARGET_LANGUAGE);
        let target = Lang::from_code(effective_target_language(Some(&saved), system_language))
            .unwrap_or(Lang::En);
        let (custom_models, _) = custom_ai_models_from_json(&document.value(KEY_CUSTOM_MODELS));
        Self {
            backend: if text(KEY_TRANSLATION_BACKEND) == BACKEND_OPENAI {
                Backend::OpenAi
            } else {
                Backend::Local
            },
            models_dir,
            model_id: text(KEY_LOCAL_MODEL_ID),
            idle: Duration::from_secs(
                number(KEY_LOCAL_IDLE_SECONDS, DEFAULT_IDLE_SECONDS).max(1) as u64
            ),
            beams: number(KEY_LOCAL_NUM_BEAMS, DEFAULT_NUM_BEAMS).clamp(1, MAX_NUM_BEAMS) as usize,
            low_memory: document
                .value(KEY_LOCAL_LOW_MEMORY)
                .as_bool()
                .unwrap_or(false),
            route_mode: RouteMode::from_code(&text(KEY_LOCAL_ROUTE_MODE)).unwrap_or_default(),
            max_resident: number(KEY_LOCAL_MAX_RESIDENT, DEFAULT_MAX_RESIDENT)
                .clamp(1, MAX_MAX_RESIDENT) as usize,
            source: Lang::from_code(&text(KEY_SOURCE_LANGUAGE)).unwrap_or(Lang::Auto),
            target,
            layout: LayoutMode::from_config(&text(KEY_LAYOUT)),
            custom_model_id: text(KEY_CUSTOM_MODEL),
            custom_models,
        }
    }

    /// 路由策略（模式、指定包、常驻上限），变化时不需要重建引擎。
    pub fn route_policy(&self) -> RoutePolicy {
        RoutePolicy {
            mode: self.route_mode,
            preferred_id: self.model_id.clone(),
            max_resident: self.max_resident,
        }
    }

    /// 实际使用的束宽：低内存模式强制贪心。
    pub fn effective_beams(&self) -> usize {
        if self.low_memory {
            1
        } else {
            self.beams.clamp(1, MAX_BEAMS)
        }
    }
}

/// 一次翻译的产出。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Translated {
    /// 译文，与请求文本一一对应。
    pub texts: Vec<String>,
    /// 后端与模型的展示名（写进结果面板）。
    pub label: String,
}

/// 可翻译的后端抽象（真实实现是 [`TranslateHost`]，测试里用假实现）。
pub trait Translator {
    /// 翻译一批段落。
    ///
    /// # 参数
    /// - `config`：本次配置。
    /// - `texts`：待翻译段落。
    fn translate(
        &self,
        config: &TranslateConfig,
        texts: &[String],
    ) -> Result<Translated, TranslateError>;

    /// 回收没被真实请求用上的预热（默认什么都不做）；会等进行中的预热结束，应在后台线程里调用。
    fn release_unused_warm(&self) {}
}

/// 已装配好的引擎。
struct Built {
    /// 装配参数指纹，变化则重建（不含路由模式、指定包、常驻上限：它们由路由器热更新）。
    key: String,
    /// 引擎。
    engine: Arc<dyn TranslationEngine>,
    /// 本地多包路由器（探针与策略热更新用）。
    router: Option<Arc<RoutedEngine>>,
    /// 包 ID → 展示名（把路由器记录的实际用包翻成标签；OpenAI 通道为空）。
    labels: LabelTable,
}

/// 包 ID → 展示名的对照表。
type LabelTable = Vec<(String, String)>;

/// 一次装配的结论：引擎展示名、解析出的源语言、目标语言。
type Prepared = (String, Lang, Lang);

/// 翻译宿主：持有当前引擎与结果缓存。
pub struct TranslateHost {
    /// 应用数据根目录。
    data_root: PathBuf,
    /// 翻译服务（缓存 + 当前引擎）。
    service: TranslationService,
    /// 当前装配。
    built: Mutex<Option<Built>>,
    /// `SNOW_ORT_DYLIB` 的值（构造时读取，便于测试注入）。
    ort_env: Option<String>,
    /// `SNOW_TRANSLATOR_EXE` 的值。
    exe_env: Option<PathBuf>,
    /// worker 的拉起方式（生产用子进程，测试注入假 worker）。
    launcher: Arc<dyn WorkerLauncher>,
}

/// 本地模型不可用时的技术细节（英文，界面层再套上本地化的引导语）：目录与有问题的清单。
fn no_model_message(dir: &Path, issues: &[ManifestIssue]) -> String {
    let mut text = format!("no usable translation model in {}", dir.display());
    for issue in issues.iter().take(MAX_ISSUES_SHOWN) {
        text.push_str(&format!("; {} unusable: {}", issue.dir_name, issue.reason));
    }
    if issues.len() > MAX_ISSUES_SHOWN {
        text.push_str(&format!(
            "; {} more unusable models",
            issues.len() - MAX_ISSUES_SHOWN
        ));
    }
    text
}

/// 模型包的展示名：清单里的 `display_name`，缺省用 id。
pub(crate) fn model_label(model: &ScannedModel) -> String {
    if model.manifest.display_name.trim().is_empty() {
        model.manifest.id.clone()
    } else {
        model.manifest.display_name.clone()
    }
}

/// 混合拆分的预选标签：覆盖目标语言的包名，`default_eligible=false` 的可选包默认不参与，不列入；
/// 只有可选包覆盖时才退回列它们（与选包逻辑一致）。没有任何包覆盖返回空。
fn mixed_preselect_names(models: &[ScannedModel], target: Lang) -> Vec<String> {
    let covering: Vec<&ScannedModel> = models
        .iter()
        .filter(|m| m.manifest.supports(Lang::Auto, target))
        .collect();
    let eligible: Vec<&ScannedModel> = covering
        .iter()
        .copied()
        .filter(|m| m.manifest.default_eligible)
        .collect();
    let pool = if eligible.is_empty() {
        covering
    } else {
        eligible
    };
    pool.into_iter().map(model_label).collect()
}

/// 由实际参与翻译的包 ID 生成标签（去重保序，`" + "` 连接）；没有实际使用记录（缓存命中等）时用预选标签。
///
/// # 参数
/// - `used_ids`：路由器记录的实际用包 ID。
/// - `labels`：包 ID → 展示名。
/// - `fallback`：预选标签。
fn actual_label(used_ids: &[String], labels: &[(String, String)], fallback: &str) -> String {
    let mut names: Vec<&str> = Vec::new();
    for id in used_ids {
        let name = labels
            .iter()
            .find(|(k, _)| k == id)
            .map_or(id.as_str(), |(_, v)| v.as_str());
        if !names.contains(&name) {
            names.push(name);
        }
    }
    if names.is_empty() {
        fallback.to_string()
    } else {
        names.join(LABEL_JOINER)
    }
}

/// 由数据根得到默认模型目录。
///
/// # 参数
/// - `data_root`：应用数据根目录。
///
/// ```ignore
/// assert!(default_models_dir(Path::new("D")).ends_with("translate"));
/// ```
pub fn default_models_dir(data_root: &Path) -> PathBuf {
    MODELS_SUBDIR
        .iter()
        .fold(data_root.to_path_buf(), |dir, part| dir.join(part))
}

/// 定位 `cisox-translator.exe`：环境变量优先，其次主程序同目录。
///
/// # 参数
/// - `env_exe`：`SNOW_TRANSLATOR_EXE` 的值。
///
/// # 返回
/// 可执行文件路径；找不到返回 `WorkerUnavailable`（说明查找位置）。
pub fn locate_worker_exe(env_exe: Option<&Path>) -> Result<PathBuf, TranslateError> {
    let beside_exe = std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(Path::to_path_buf));
    crate::ocr_assets::locate_worker_exe(
        env_exe,
        beside_exe.as_deref(),
        ENV_TRANSLATOR_EXE,
        WORKER_EXE_NAME,
    )
    .map_err(TranslateError::WorkerUnavailable)
}

/// 字符串指纹（配置指纹里避免明文放密钥）。
fn fingerprint(value: &str) -> u64 {
    let mut hasher = DefaultHasher::new();
    value.hash(&mut hasher);
    hasher.finish()
}

impl TranslateHost {
    /// 创建宿主（此时不启动任何进程）。
    ///
    /// # 参数
    /// - `data_root`：应用数据根目录。
    ///
    /// ```ignore
    /// let host = TranslateHost::new(&data_root);
    /// ```
    pub fn new(data_root: &Path) -> Self {
        Self::with_env(
            data_root,
            std::env::var(snow_translate::worker::ENV_ORT_DYLIB).ok(),
            std::env::var_os(ENV_TRANSLATOR_EXE)
                .filter(|v| !v.is_empty())
                .map(PathBuf::from),
        )
    }

    /// 用显式环境值创建宿主（测试注入，避免读进程环境）。
    ///
    /// # 参数
    /// - `data_root`：数据根目录。
    /// - `ort_env`：`SNOW_ORT_DYLIB` 的值。
    /// - `exe_env`：`SNOW_TRANSLATOR_EXE` 的值。
    pub fn with_env(data_root: &Path, ort_env: Option<String>, exe_env: Option<PathBuf>) -> Self {
        Self {
            data_root: data_root.to_path_buf(),
            service: TranslationService::new(),
            built: Mutex::new(None),
            ort_env,
            exe_env,
            launcher: Arc::new(ProcessLauncher),
        }
    }

    /// 换上自定义的 worker 拉起方式（测试注入假 worker）。
    #[cfg(test)]
    fn with_launcher(mut self, launcher: Arc<dyn WorkerLauncher>) -> Self {
        self.launcher = launcher;
        self
    }

    /// 模型根目录（配置为空用默认位置）。
    ///
    /// # 参数
    /// - `config`：翻译配置。
    pub fn models_dir(&self, config: &TranslateConfig) -> PathBuf {
        config
            .models_dir
            .clone()
            .unwrap_or_else(|| default_models_dir(&self.data_root))
    }

    /// 扫描模型目录（设置页 / 诊断用）。
    ///
    /// # 参数
    /// - `config`：翻译配置。
    pub fn scan(&self, config: &TranslateConfig) -> ScanReport {
        ModelScanner::new(&self.models_dir(config)).scan()
    }

    /// 装配本地 NMT 路由器：校验语言对 → 找 worker → 找 onnxruntime → 每个包建一个懒加载引擎（不拉起进程）。
    fn build_local(&self, config: &TranslateConfig) -> Result<(Built, Prepared), TranslateError> {
        let dir = self.models_dir(config);
        let report = ModelScanner::new(&dir).scan();
        if report.models.is_empty() {
            return Err(TranslateError::NoModelFound(no_model_message(
                &dir,
                &report.issues,
            )));
        }
        let (label, src) = match config.route_mode {
            RouteMode::MixedSplit => {
                let names = mixed_preselect_names(&report.models, config.target);
                if names.is_empty() {
                    return Err(TranslateError::UnsupportedLanguagePair(
                        config.source,
                        config.target,
                    ));
                }
                (names.join(LABEL_JOINER), config.source)
            }
            mode => {
                let (model, src) = pick_model_routed(
                    &report.models,
                    &config.model_id,
                    config.source,
                    config.target,
                    mode,
                )?;
                // Auto 保持 Auto：由路由器逐条识别语言再选包，不能按专用包钉成其方向的源语言
                (
                    model_label(model),
                    if config.source == Lang::Auto {
                        Lang::Auto
                    } else {
                        src
                    },
                )
            }
        };
        let exe = locate_worker_exe(self.exe_env.as_deref())?;
        let dylib =
            resolve_ort_dylib(&self.data_root, self.ort_env.as_deref()).map_err(|e| match e {
                OrtUnavailable::NotInstalled => TranslateError::RuntimeMissing(e.detail()),
                other => TranslateError::WorkerUnavailable(other.detail()),
            })?;
        let beams = config.effective_beams();
        let models_fingerprint: Vec<String> = report
            .models
            .iter()
            .map(|m| format!("{}@{}", m.manifest.id, m.dir.display()))
            .collect();
        let key = format!(
            "local|{}|{}|{}|{beams}|{}|{}",
            exe.display(),
            models_fingerprint.join(","),
            dylib.display(),
            config.low_memory,
            config.idle.as_secs()
        );
        let slots: Vec<RoutedSlot> = report
            .models
            .iter()
            .map(|model| {
                let worker = WorkerEngine::with_launcher(
                    WorkerConfig {
                        exe: exe.clone(),
                        model_dir: model.dir.clone(),
                        model_id: model.manifest.id.clone(),
                        pairs: model.manifest.supported_pairs(),
                        ort_dylib: Some(dylib.clone()),
                        num_beams: beams,
                        trim_after_request: true,
                        idle_timeout: config.idle,
                        timeouts: Timeouts::default(),
                    },
                    Arc::clone(&self.launcher),
                );
                let engine: Arc<dyn PooledEngine> = Arc::new(worker);
                RoutedSlot {
                    manifest: model.manifest.clone(),
                    engine,
                }
            })
            .collect();
        let router = Arc::new(RoutedEngine::new(slots, config.route_policy()));
        let engine: Arc<dyn TranslationEngine> = router.clone();
        let labels = report
            .models
            .iter()
            .map(|m| (m.manifest.id.clone(), model_label(m)))
            .collect();
        Ok((
            Built {
                key,
                engine,
                router: Some(router),
                labels,
            },
            (label, src, config.target),
        ))
    }

    /// 装配 OpenAI 兼容引擎：取配置里选中的自定义模型。
    fn build_openai(&self, config: &TranslateConfig) -> Result<(Built, Prepared), TranslateError> {
        if config.custom_models.is_empty() {
            return Err(TranslateError::NoCustomModel);
        }
        let model = config
            .custom_models
            .iter()
            .find(|m| m.id == config.custom_model_id)
            .ok_or(TranslateError::CustomModelNotSelected)?;
        let key = format!(
            "openai|{}|{}|{}",
            model.base_url,
            model.model,
            fingerprint(&model.api_key)
        );
        let engine: Arc<dyn TranslationEngine> =
            Arc::new(OpenAiEngine::new(OpenAiCompatibleConfig {
                base_url: model.base_url.clone(),
                api_key: model.api_key.clone(),
                model: model.model.clone(),
            }));
        Ok((
            Built {
                key,
                engine,
                router: None,
                labels: Vec::new(),
            },
            (model.name.clone(), config.source, config.target),
        ))
    }

    /// 按配置装配引擎；参数没变就复用，变了就换掉旧引擎（旧 worker 随之退出）。
    ///
    /// 路由模式、指定包、常驻上限不属于装配参数：它们热更新到现有路由器上，不会重启已加载的 worker。
    ///
    /// # 参数
    /// - `config`：翻译配置。
    ///
    /// # 返回
    /// `(引擎标签, 解析出的源语言, 目标语言)`。
    pub fn prepare(
        &self,
        config: &TranslateConfig,
    ) -> Result<(String, Lang, Lang), TranslateError> {
        let (fresh, prepared) = match config.backend {
            Backend::Local => self.build_local(config)?,
            Backend::OpenAi => self.build_openai(config)?,
        };
        let mut built = self.built.lock().unwrap_or_else(PoisonError::into_inner);
        match built.as_mut() {
            Some(current) if current.key == fresh.key => {
                // 同参数：沿用已有引擎（丢弃刚构造的、尚未启动的新引擎），只热更新路由策略
                if let (Some(current_router), Some(fresh_router)) = (&current.router, &fresh.router)
                {
                    current_router.set_policy(fresh_router.policy());
                }
            }
            _ => {
                self.service.set_engine(Some(Arc::clone(&fresh.engine)));
                *built = Some(fresh);
            }
        }
        Ok(prepared)
    }

    /// 当前路由器（没有本地装配时为 `None`）。
    fn router(&self) -> Option<Arc<RoutedEngine>> {
        self.built
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .as_ref()?
            .router
            .clone()
    }

    /// 当前路由器与包展示名表（没有本地装配时为 `None`）。
    fn routing(&self) -> Option<(Arc<RoutedEngine>, LabelTable)> {
        let built = self.built.lock().unwrap_or_else(PoisonError::into_inner);
        let current = built.as_ref()?;
        Some((current.router.clone()?, current.labels.clone()))
    }

    /// 是否有本地 worker 正在运行（探针用）。
    pub fn worker_running(&self) -> bool {
        self.router().is_some_and(|r| r.any_resident())
    }

    /// 当前常驻内存的翻译包 ID（探针 / 诊断用）。
    pub fn resident_models(&self) -> Vec<String> {
        self.router().map(|r| r.resident_ids()).unwrap_or_default()
    }

    /// 向运行中的本地 worker 取内存快照（探针用）。
    pub fn worker_memory(&self) -> Option<MemorySnapshot> {
        self.router()?.memory_snapshot()
    }

    /// 本地 worker 累计拉起次数（探针用）。
    pub fn worker_launches(&self) -> u32 {
        self.router().map_or(0, |r| r.launch_total())
    }

    /// 预热本地翻译 worker：不看 OCR 文本就能定包时，提前拉起进程并加载模型（与 OCR 并行）。
    ///
    /// 只在本地后端预热；选包规则见 [`snow_translate::router::warm_target`]（能确定用哪个包就预热，
    /// 源语言为自动时也一样，因为预热只加载模型、语言对随翻译请求指定；选不出包就不猜）。
    /// 装配或加载失败只记日志，真实请求会走正常路径并给出明确错误；
    /// 猜错包不会出错（真实请求按实际语种选包，旧包按常驻上限被换掉）。
    /// 阻塞调用，应在后台线程里执行；同一个包上的真实请求会排在预热后面，不会重复拉进程或重复加载。
    ///
    /// # 参数
    /// - `config`：本次翻译配置。
    /// - `abort`：放弃判定；为真时尽早停手，预热完成后才发现被放弃则立刻回收。
    ///
    /// # 返回
    /// 预热成功的包 ID；没预热（非本地后端、选不出包、被放弃、失败）返回 `None`。
    ///
    /// ```ignore
    /// let warmed = host.warm(&config, &|| cancelled.load(Ordering::Relaxed));
    /// ```
    pub fn warm(&self, config: &TranslateConfig, abort: &dyn Fn() -> bool) -> Option<String> {
        if config.backend != Backend::Local || abort() {
            return None;
        }
        if let Err(error) = self.prepare(config) {
            tracing::debug!(%error, "翻译预热跳过：装配失败");
            return None;
        }
        let router = self.router()?;
        let warmed = match router.warm(config.source, config.target, abort) {
            Ok(warmed) => warmed,
            Err(error) => {
                tracing::debug!(%error, "翻译预热失败，真实请求将走正常路径");
                None
            }
        };
        if abort() {
            router.cancel_warm();
            return None;
        }
        warmed
    }

    /// 结束全部 worker 并丢弃引擎（应用退出时调用）。
    pub fn shutdown(&self) {
        let taken = self
            .built
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take();
        if let Some(built) = taken
            && let Some(router) = &built.router
        {
            router.shutdown();
        }
        self.service.set_engine(None);
    }
}

impl Translator for TranslateHost {
    /// 装配引擎后经缓存翻译。
    fn translate(
        &self,
        config: &TranslateConfig,
        texts: &[String],
    ) -> Result<Translated, TranslateError> {
        let (label, src, tgt) = self.prepare(config)?;
        let routing = self.routing();
        if let Some((router, _)) = &routing {
            router.reset_used();
        }
        let texts = self.service.translate_batch(texts, src, tgt)?;
        // 标签反映实际参与翻译的包；缓存命中等没有用包记录时沿用预选标签
        let label = match routing {
            Some((router, labels)) => actual_label(&router.used_ids(), &labels, &label),
            None => label,
        };
        Ok(Translated { texts, label })
    }

    /// 没被用上的预热立刻卸载（用上了的不动）。
    fn release_unused_warm(&self) {
        if let Some(router) = self.router() {
            router.cancel_warm();
        }
    }
}

/// 预热触发来源：翻译流程 / 输入框浮窗建成时与其余工作并行预热。
const WARM_TRIGGER_START: &str = "start";
/// 预热触发来源：输入框里开始输入时发现包已不在内存，补一次。
const WARM_TRIGGER_FIRST_INPUT: &str = "first_input";

/// 一次翻译请求的预热句柄：与 OCR 并行预热本地翻译 worker，流程结束后回收没用上的预热。
pub struct WarmHandle {
    /// 流程已结束（或取消）：预热线程据此放弃。
    done: Arc<AtomicBool>,
}

impl WarmHandle {
    /// 在后台线程开始预热（非本地后端不开线程）。
    ///
    /// # 参数
    /// - `host`：翻译宿主。
    /// - `config`：本次翻译配置。
    /// - `cancel`：用户取消标志；为真时预热放弃并回收。
    ///
    /// ```ignore
    /// let warm = WarmHandle::start(Arc::clone(&host), config.clone(), Arc::clone(&cancel));
    /// // ……跑完整个流程后：
    /// warm.finish(host.as_ref());
    /// ```
    pub fn start(
        host: Arc<TranslateHost>,
        config: TranslateConfig,
        cancel: Arc<AtomicBool>,
    ) -> Self {
        Self::spawn(host, config, cancel, WARM_TRIGGER_START, false)
    }

    /// 输入框里开始输入时补一次预热：线程里先看包还在不在内存（含正在加载），在就什么都不做，
    /// 不在（预热被 TTL 回收或从未预热成功）才重新预热，让加载与防抖等待重叠。
    ///
    /// 非本地后端不开线程。返回的句柄与浮窗里其余预热句柄一起，关窗时由 [`Self::finish`] 回收。
    ///
    /// # 参数
    /// - `host`：翻译宿主。
    /// - `config`：与浮窗首次真实请求一致的翻译配置。
    /// - `cancel`：取消标志；为真时预热放弃并回收。
    ///
    /// ```ignore
    /// let warm = WarmHandle::rewarm_on_input(Arc::clone(&host), config, Arc::new(AtomicBool::new(false)));
    /// ```
    pub fn rewarm_on_input(
        host: Arc<TranslateHost>,
        config: TranslateConfig,
        cancel: Arc<AtomicBool>,
    ) -> Self {
        Self::spawn(host, config, cancel, WARM_TRIGGER_FIRST_INPUT, true)
    }

    /// 起预热线程的共同实现。
    ///
    /// # 参数
    /// - `host` / `config` / `cancel`：同 [`Self::start`]。
    /// - `trigger`：触发来源，写进日志便于对照收益。
    /// - `only_if_cold`：为真时包还在内存就不预热（补热用）。
    fn spawn(
        host: Arc<TranslateHost>,
        config: TranslateConfig,
        cancel: Arc<AtomicBool>,
        trigger: &'static str,
        only_if_cold: bool,
    ) -> Self {
        let done = Arc::new(AtomicBool::new(false));
        if config.backend == Backend::Local {
            let finished = Arc::clone(&done);
            let spawned = std::thread::Builder::new()
                .name("snow-translate-warm".into())
                .spawn(move || {
                    let started = Instant::now();
                    if only_if_cold && host.worker_running() {
                        tracing::debug!(trigger, "翻译包仍在内存，不补预热");
                        return;
                    }
                    let abort = || finished.load(Ordering::SeqCst) || cancel.load(Ordering::SeqCst);
                    match host.warm(&config, &abort) {
                        Some(model) => tracing::info!(
                            trigger,
                            model,
                            warm_ms = started.elapsed().as_millis() as u64,
                            "翻译模型已预热"
                        ),
                        None => tracing::debug!(trigger, "本次未预热翻译模型"),
                    }
                });
            if let Err(error) = spawned {
                tracing::warn!(trigger, %error, "无法创建翻译预热线程，跳过预热");
            }
        }
        Self { done }
    }

    /// 流程结束（成功、失败或取消）：让还没开始的预热放弃，并把已预热却没用上的 worker 立刻卸载。
    ///
    /// 会等进行中的预热释放引擎，应在后台线程里调用。
    ///
    /// # 参数
    /// - `translator`：本次用的翻译后端。
    pub fn finish(&self, translator: &dyn Translator) {
        self.done.store(true, Ordering::SeqCst);
        translator.release_unused_warm();
    }
}

impl Drop for WarmHandle {
    /// 句柄被丢弃（含作业线程没起来、panic）时至少让预热线程放弃，免得它白占内存。
    fn drop(&mut self) {
        self.done.store(true, Ordering::SeqCst);
    }
}

/// 在飞的那一次翻译请求的取消标志（同一时刻界面只有一次翻译，所以只记最近一次）。
#[derive(Default)]
pub struct CancelSlot {
    /// 最近一次请求的序号与取消标志。
    current: Option<(u64, Arc<AtomicBool>)>,
}

impl CancelSlot {
    /// 登记新请求，返回它的取消标志（取代上一次登记）。
    ///
    /// # 参数
    /// - `serial`：请求序号。
    pub fn begin(&mut self, serial: u64) -> Arc<AtomicBool> {
        let flag = Arc::new(AtomicBool::new(false));
        self.current = Some((serial, Arc::clone(&flag)));
        flag
    }

    /// 取消指定序号的请求；序号不是最近一次登记的（已被新请求取代）就忽略。
    ///
    /// # 参数
    /// - `serial`：被取消的请求序号。
    ///
    /// # 返回
    /// 是否命中并置位。
    pub fn cancel(&mut self, serial: u64) -> bool {
        match &self.current {
            Some((current, flag)) if *current == serial => {
                flag.store(true, Ordering::SeqCst);
                true
            }
            _ => false,
        }
    }
}

/// 翻译流程的阶段（用于界面进度文案）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TranslateStage {
    /// 正在识别文字。
    Recognizing,
    /// 正在翻译。
    Translating,
}

/// 一个段落的原文、译文与它对应的 OCR 行框下标。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TranslatedParagraph {
    /// 原文段。
    pub source: String,
    /// 译文段。
    pub translated: String,
    /// 组成该段的 OCR 行框下标（对应 `OcrResult::boxes`）。
    pub box_indices: Vec<usize>,
}

/// 翻译流程的产出。
#[derive(Debug, Clone, PartialEq)]
pub struct TranslateOutcome {
    /// 参与翻译的原文（按段落，行间 `\n`）。
    pub source: String,
    /// 译文（按段落，行间 `\n`）。
    pub translated: String,
    /// 原文与译文的逐段对照（含行框下标）。
    pub pairs: Vec<TranslatedParagraph>,
    /// OCR 行框（选区内图像坐标，供浮层与结果窗高亮）。
    pub boxes: Vec<OcrTextBox>,
    /// 后端与模型展示名。
    pub label: String,
    /// OCR 耗时（毫秒）。
    pub ocr_ms: u64,
    /// 翻译耗时（毫秒，含首次拉起与加载）。
    pub translate_ms: u64,
}

/// 翻译流程的失败原因。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TranslateFlowError {
    /// OCR 失败（含资产缺失）。
    Ocr(OcrError),
    /// 选区里没有识别到文字。
    NoText,
    /// 原文已经是目标语言（如中文翻中文），不翻译以免产出乱码。
    AlreadyTarget(Lang),
    /// 翻译失败。
    Translate(TranslateError),
    /// 用户在识别期间撤销了翻译（结果不会展示）。
    Cancelled,
}

/// 文本里汉字占字母类字符的比例是否已过半（用于识别“原文已是中文”）。
///
/// # 参数
/// - `text`：原文。
///
/// ```ignore
/// assert!(is_mostly_cjk("今天天气很好 ok"));
/// assert!(!is_mostly_cjk("Hello 世"));
/// ```
pub fn is_mostly_cjk(text: &str) -> bool {
    let (mut cjk, mut letters) = (0usize, 0usize);
    for c in text.chars().filter(|c| c.is_alphabetic()) {
        letters += 1;
        if CJK_RANGE.contains(&c) {
            cjk += 1;
        }
    }
    letters > 0 && cjk as f32 / letters as f32 >= CJK_MAJORITY
}

/// 选区到译文的完整流程：OCR → 版式整理 → 翻译。
///
/// # 参数
/// - `recognize`：执行 OCR 的闭包（放进闭包便于测试注入）。
/// - `translator`：翻译后端。
/// - `config`：翻译配置。
/// - `on_stage`：阶段回调（界面进度）。
///
/// # 返回
/// 原文与译文；OCR 失败、无文字、原文已是目标语言、翻译失败分别返回对应错误。
///
/// ```ignore
/// let outcome = run_flow(|| ocr.recognize_rgba(&cfg, w, h, &rgba), &host, &tcfg, |_| {})?;
/// ```
pub fn run_flow(
    recognize: impl FnOnce() -> Result<OcrResult, OcrError>,
    translator: &dyn Translator,
    config: &TranslateConfig,
    on_stage: impl FnMut(TranslateStage),
) -> Result<TranslateOutcome, TranslateFlowError> {
    run_flow_cancellable(recognize, translator, config, on_stage, || false)
}

/// 同 [`run_flow`]，多一个取消判定：识别完成后若已被取消就不再翻译，返回 `Cancelled`。
///
/// # 参数
/// - `recognize` / `translator` / `config` / `on_stage`：同 [`run_flow`]。
/// - `cancelled`：取消判定（识别结束后检查一次）。
///
/// # 返回
/// 同 [`run_flow`]；被取消返回 `Cancelled`（不调用翻译后端）。
///
/// ```ignore
/// let outcome = run_flow_cancellable(|| ocr.recognize_rgba(&cfg, w, h, &rgba), &host, &tcfg, |_| {}, || flag.load(Ordering::Relaxed));
/// ```
pub fn run_flow_cancellable(
    recognize: impl FnOnce() -> Result<OcrResult, OcrError>,
    translator: &dyn Translator,
    config: &TranslateConfig,
    mut on_stage: impl FnMut(TranslateStage),
    cancelled: impl Fn() -> bool,
) -> Result<TranslateOutcome, TranslateFlowError> {
    on_stage(TranslateStage::Recognizing);
    let ocr = recognize().map_err(TranslateFlowError::Ocr)?;
    if cancelled() {
        return Err(TranslateFlowError::Cancelled);
    }
    let paragraphs_with_indices = paragraphs_from_boxes(&ocr.boxes, config.layout);
    if paragraphs_with_indices.is_empty() {
        return Err(TranslateFlowError::NoText);
    }
    let paragraphs: Vec<String> = paragraphs_with_indices
        .iter()
        .map(|(s, _)| s.clone())
        .collect();
    let joined = paragraphs.join("\n");
    if matches!(config.target, Lang::ZhHans | Lang::ZhHant) && is_mostly_cjk(&joined) {
        return Err(TranslateFlowError::AlreadyTarget(config.target));
    }
    on_stage(TranslateStage::Translating);
    let started = std::time::Instant::now();
    let translated = translator
        .translate(config, &paragraphs)
        .map_err(TranslateFlowError::Translate)?;
    let translate_ms = started.elapsed().as_millis() as u64;
    let translated_text = translated.texts.join("\n");
    let pairs: Vec<TranslatedParagraph> = paragraphs_with_indices
        .into_iter()
        .zip(translated.texts)
        .map(|((source, box_indices), translated)| TranslatedParagraph {
            source,
            translated,
            box_indices,
        })
        .collect();
    Ok(TranslateOutcome {
        source: joined,
        translated: translated_text,
        pairs,
        boxes: ocr.boxes,
        label: translated.label,
        ocr_ms: ocr.elapsed_ms,
        translate_ms,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ocr_assets::OcrUnavailable;
    use crate::ocr_service::OcrTextBox;
    use snow_config::extensions::{ROUTE_MIXED_SPLIT, ROUTE_SINGLE, ROUTE_SPECIALIZED_FIRST};
    use snow_ui::shell::geometry::PhysicalRect;

    /// 唯一临时目录。
    fn temp_root(tag: &str) -> PathBuf {
        crate::test_util::unique_temp_dir("snow-translate-host", tag)
    }

    /// 在模型根下写一份最小模型（占位文件）。
    fn write_model(models: &Path, id: &str, pairs: &str) {
        let dir = models.join(id);
        std::fs::create_dir_all(&dir).expect("建模型目录");
        let manifest = format!(
            r#"{{"schema_version":1,"id":"{id}","display_name":"Model {id}","family":"marian","files":{{"encoder":"e.onnx","decoder":"d.onnx","tokenizer":"t.json"}},"languages":["en","zh-CN"],"pairs":{pairs}}}"#
        );
        std::fs::write(dir.join("model.json"), manifest).expect("写清单");
        for f in ["e.onnx", "d.onnx", "t.json"] {
            std::fs::write(dir.join(f), b"x").expect("写文件");
        }
    }

    /// 在模型根下写一份通用多语包（只写 `languages`，不写 `pairs`）。
    fn write_general_model(models: &Path, id: &str) {
        let dir = models.join(id);
        std::fs::create_dir_all(&dir).expect("建模型目录");
        let manifest = format!(
            r#"{{"schema_version":1,"id":"{id}","display_name":"Model {id}","family":"m2m100","files":{{"encoder":"e.onnx","decoder":"d.onnx","tokenizer":"t.json"}},"languages":["en","zh-CN","ja"]}}"#
        );
        std::fs::write(dir.join("model.json"), manifest).expect("写清单");
        for f in ["e.onnx", "d.onnx", "t.json"] {
            std::fs::write(dir.join(f), b"x").expect("写文件");
        }
    }

    /// 默认配置（本地后端，英→简中）。
    fn config() -> TranslateConfig {
        TranslateConfig::from_document(&ConfigDocument::from_bytes(None), "en-US")
    }

    /// 造一个带假 exe 与假 dll 的宿主（不会真的拉起进程）。
    fn host_with_files(root: &Path) -> (TranslateHost, PathBuf) {
        let exe = root.join("cisox-translator.exe");
        let dll = root.join("onnxruntime.dll");
        std::fs::write(&exe, b"x").expect("写 exe");
        std::fs::write(&dll, b"x").expect("写 dll");
        let host =
            TranslateHost::with_env(root, Some(dll.to_string_lossy().into_owned()), Some(exe));
        (host, root.join("models").join("translate"))
    }

    /// 配置解析：全默认 → 本地后端、贪心以外的默认束宽 4、空闲 120 秒、目标语言随界面语言。
    #[test]
    fn config_defaults() {
        let cfg = config();
        assert_eq!(cfg.backend, Backend::Local);
        assert_eq!(cfg.beams, 2);
        assert_eq!(cfg.effective_beams(), 2);
        assert_eq!(cfg.idle, Duration::from_secs(120));
        assert_eq!(cfg.source, Lang::Auto);
        assert_eq!(cfg.target, Lang::En, "英文系统默认翻成英文");
        assert!(cfg.models_dir.is_none() && cfg.model_id.is_empty() && !cfg.low_memory);
        assert_eq!(cfg.layout, LayoutMode::SmartMerge);
        assert_eq!(
            (cfg.route_mode, cfg.max_resident),
            (RouteMode::SpecializedFirst, 1)
        );
        let zh = TranslateConfig::from_document(&ConfigDocument::from_bytes(None), "zh-CN");
        assert_eq!(zh.target, Lang::ZhHans);
    }

    /// 配置解析：读取各扩展项；低内存强制贪心；后端切换；自定义模型列表。
    #[test]
    fn config_reads_extension_keys() {
        let mut doc = ConfigDocument::from_bytes(None);
        doc.set_value(KEY_LOCAL_MODELS_DIR, serde_json::json!("D:/my models"))
            .expect("目录");
        doc.set_value(KEY_LOCAL_MODEL_ID, serde_json::json!("opus"))
            .expect("模型");
        doc.set_value(KEY_LOCAL_IDLE_SECONDS, serde_json::json!(30))
            .expect("空闲");
        doc.set_value(KEY_LOCAL_NUM_BEAMS, serde_json::json!(2))
            .expect("束宽");
        doc.set_value(KEY_TARGET_LANGUAGE, serde_json::json!("ja"))
            .expect("目标");
        doc.set_value(KEY_SOURCE_LANGUAGE, serde_json::json!("en"))
            .expect("源");
        doc.set_value(KEY_LAYOUT, serde_json::json!("original"))
            .expect("版式");
        doc.set_value(KEY_LOCAL_ROUTE_MODE, serde_json::json!(ROUTE_MIXED_SPLIT))
            .expect("路由");
        doc.set_value(KEY_LOCAL_MAX_RESIDENT, serde_json::json!(2))
            .expect("常驻数");
        let cfg = TranslateConfig::from_document(&doc, "en-US");
        assert_eq!(
            (cfg.route_mode, cfg.max_resident),
            (RouteMode::MixedSplit, 2)
        );
        assert_eq!(cfg.route_policy().preferred_id, "opus");
        assert_eq!(cfg.models_dir, Some(PathBuf::from("D:/my models")));
        assert_eq!(
            (cfg.model_id.as_str(), cfg.beams, cfg.idle),
            ("opus", 2, Duration::from_secs(30))
        );
        assert_eq!(
            (cfg.source, cfg.target, cfg.layout),
            (Lang::En, Lang::Ja, LayoutMode::Original)
        );
        doc.set_value(KEY_LOCAL_LOW_MEMORY, serde_json::json!(true))
            .expect("低内存");
        assert_eq!(
            TranslateConfig::from_document(&doc, "en-US").effective_beams(),
            1
        );
        doc.set_value(KEY_TRANSLATION_BACKEND, serde_json::json!("openai"))
            .expect("后端");
        assert_eq!(
            TranslateConfig::from_document(&doc, "en-US").backend,
            Backend::OpenAi
        );
    }

    /// 配置里的路由模式取值与路由器认的代号一一对应（两个 crate 各写一份字面量，这里守住一致）。
    #[test]
    fn route_mode_codes_match_config_values() {
        assert_eq!(RouteMode::from_code(ROUTE_SINGLE), Some(RouteMode::Single));
        assert_eq!(
            RouteMode::from_code(ROUTE_SPECIALIZED_FIRST),
            Some(RouteMode::SpecializedFirst)
        );
        assert_eq!(
            RouteMode::from_code(ROUTE_MIXED_SPLIT),
            Some(RouteMode::MixedSplit)
        );
    }

    /// 专用包优先：同时有通用包与显式声明语言对的专用包时，未指定包选专用包；指定通用包仍尊重；
    /// single 模式按顺序取第一个（通用包）。
    #[test]
    fn specialized_pack_wins_unless_pinned() {
        let root = temp_root("special");
        let (host, models) = host_with_files(&root);
        write_general_model(&models, "a-nllb");
        write_model(&models, "z-opus", r#"[["en","zh-CN"]]"#);
        let mut cfg = config();
        cfg.target = Lang::ZhHans;
        let (label, src, _) = host.prepare(&cfg).expect("默认专用包优先");
        assert_eq!((label.as_str(), src), ("Model z-opus", Lang::Auto));
        cfg.model_id = "a-nllb".into();
        assert_eq!(host.prepare(&cfg).expect("指定通用包").0, "Model a-nllb");
        cfg.model_id = String::new();
        cfg.route_mode = RouteMode::Single;
        assert_eq!(host.prepare(&cfg).expect("single").0, "Model a-nllb");
        cfg.source = Lang::Ja;
        cfg.route_mode = RouteMode::SpecializedFirst;
        assert_eq!(
            host.prepare(&cfg).expect("日译中只有通用包").0,
            "Model a-nllb"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    /// 混合拆分：标签列出所有覆盖目标语言的包；没有任何包覆盖目标语言时报不支持。
    #[test]
    fn mixed_split_label_and_unsupported_target() {
        let root = temp_root("mixed");
        let (host, models) = host_with_files(&root);
        write_general_model(&models, "a-nllb");
        write_model(&models, "z-opus", r#"[["en","zh-CN"]]"#);
        let mut cfg = config();
        cfg.target = Lang::ZhHans;
        cfg.route_mode = RouteMode::MixedSplit;
        let (label, src, tgt) = host.prepare(&cfg).expect("混合拆分");
        assert_eq!(
            (label.as_str(), src, tgt),
            ("Model a-nllb + Model z-opus", Lang::Auto, Lang::ZhHans)
        );
        cfg.target = Lang::Ko;
        assert!(matches!(
            host.prepare(&cfg),
            Err(TranslateError::UnsupportedLanguagePair(
                Lang::Auto,
                Lang::Ko
            ))
        ));
        let _ = std::fs::remove_dir_all(&root);
    }

    /// 实际标签：按实际用包去重保序；没有记录时用预选标签；未知 ID 退回 ID 本身。
    #[test]
    fn actual_label_dedups_and_falls_back() {
        let labels = vec![
            ("a".to_string(), "Model A".to_string()),
            ("b".to_string(), "Model B".to_string()),
        ];
        let ids = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert_eq!(
            actual_label(&ids(&["b", "a", "b"]), &labels, "pre"),
            "Model B + Model A"
        );
        assert_eq!(
            actual_label(&ids(&["b"]), &labels, "Model A"),
            "Model B",
            "预选 A 实际 B，显示 B"
        );
        assert_eq!(actual_label(&[], &labels, "pre"), "pre");
        assert_eq!(actual_label(&ids(&["x"]), &labels, "pre"), "x");
    }

    /// 混合拆分预选标签：可选包（default_eligible=false）有别的包覆盖时不列入；只有它覆盖时才退回。
    #[test]
    fn mixed_preselect_skips_optional_packs() {
        let root = temp_root("mixed-optional");
        let (host, models) = host_with_files(&root);
        write_general_model(&models, "a-nllb");
        write_model(&models, "b-opt", r#"[["en","zh-CN"]]"#);
        let manifest_path = models.join("b-opt").join("model.json");
        let text = std::fs::read_to_string(&manifest_path).expect("读清单");
        std::fs::write(
            &manifest_path,
            text.replacen(
                "{\"schema_version\":1,",
                "{\"schema_version\":1,\"default_eligible\":false,",
                1,
            ),
        )
        .expect("写清单");
        let report = host.scan(&config());
        assert_eq!(report.models.len(), 2, "{:?}", report.issues);
        assert!(
            !report
                .models
                .iter()
                .find(|m| m.manifest.id == "b-opt")
                .unwrap()
                .manifest
                .default_eligible
        );
        assert_eq!(
            mixed_preselect_names(&report.models, Lang::ZhHans),
            ["Model a-nllb"]
        );
        let only_opt: Vec<ScannedModel> = report
            .models
            .into_iter()
            .filter(|m| m.manifest.id == "b-opt")
            .collect();
        assert_eq!(
            mixed_preselect_names(&only_opt, Lang::ZhHans),
            ["Model b-opt"]
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    /// 换路由模式 / 指定包 / 常驻上限不会重建引擎（不误杀已加载的 worker）；换束宽才重建。
    #[test]
    fn policy_changes_do_not_rebuild_engine() {
        let root = temp_root("policy");
        let (host, models) = host_with_files(&root);
        write_general_model(&models, "a-nllb");
        write_model(&models, "z-opus", r#"[["en","zh-CN"]]"#);
        let engine_ptr = |host: &TranslateHost| {
            host.built
                .lock()
                .unwrap()
                .as_ref()
                .map(|b| Arc::as_ptr(&b.engine) as *const () as usize)
        };
        let mut cfg = config();
        cfg.target = Lang::ZhHans;
        host.prepare(&cfg).expect("装配");
        let first = engine_ptr(&host);
        cfg.route_mode = RouteMode::MixedSplit;
        cfg.model_id = "a-nllb".into();
        cfg.max_resident = 2;
        host.prepare(&cfg).expect("换策略");
        assert_eq!(first, engine_ptr(&host), "策略变化应热更新而不是重建");
        let policy = host.router().expect("路由器").policy();
        assert_eq!(
            (
                policy.mode,
                policy.preferred_id.as_str(),
                policy.max_resident
            ),
            (RouteMode::MixedSplit, "a-nllb", 2)
        );
        cfg.beams = 3;
        host.prepare(&cfg).expect("换束宽");
        assert_ne!(first, engine_ptr(&host), "束宽变化应重建");
        assert!(host.resident_models().is_empty() && host.worker_launches() == 0);
        host.shutdown();
        let _ = std::fs::remove_dir_all(&root);
    }

    /// 目标语言生效值：已保存值不变；缺失/空串走 系统→英语 回退链。
    #[test]
    fn effective_target_rules() {
        assert_eq!(effective_target_language(None, "ja-JP"), "ja");
        assert_eq!(effective_target_language(Some(""), "zh-Hant-TW"), "en");
        assert_eq!(effective_target_language(Some("  "), "zh-CN"), "zh-Hans");
        assert_eq!(effective_target_language(None, "ko-KR"), "en");
        assert_eq!(effective_target_language(None, ""), "en");
        assert_eq!(effective_target_language(None, "xx"), "en");
        assert_eq!(
            effective_target_language(Some("fr"), "ja-JP"),
            "fr",
            "已保存值不变"
        );
        assert_eq!(
            effective_target_language(Some("zh-Hant"), "zh-CN"),
            "zh-Hans",
            "旧繁体值视同没保存"
        );
        assert_eq!(
            effective_target_language(Some("zh-Hans"), "ja-JP"),
            "zh-Hans"
        );
        assert_eq!(effective_target_language(Some("bogus"), "de-DE"), "de");
    }

    /// 界面语言生效值：旧 system/auto/空串/繁体视同没保存，en_US 与 zh_CN 不变，映射不到退 en_US。
    #[test]
    fn effective_interface_rules() {
        for old in [
            None,
            Some(""),
            Some("system"),
            Some("AUTO"),
            Some("zh_TW"),
            Some("zh-Hant"),
        ] {
            assert_eq!(
                effective_interface_language(old, "zh-CN"),
                "zh_CN",
                "{old:?}"
            );
            assert_eq!(
                effective_interface_language(old, "en-US"),
                "en_US",
                "{old:?}"
            );
            assert_eq!(
                effective_interface_language(old, "zh-Hant-TW"),
                "en_US",
                "{old:?} 繁体系统回退英语"
            );
            assert_eq!(effective_interface_language(old, "ja-JP"), "en_US");
            assert_eq!(effective_interface_language(old, ""), "en_US");
        }
        assert_eq!(
            effective_interface_language(Some("zh_CN"), "en-US"),
            "zh_CN"
        );
        assert_eq!(
            effective_interface_language(Some("en_US"), "zh-CN"),
            "en_US"
        );
    }

    /// 调用点：配置里没有保存目标语言（默认空串）时 TranslateConfig 取系统语言；已保存值不被覆盖。
    #[test]
    fn config_unset_target_uses_system_language() {
        let mut doc = ConfigDocument::from_bytes(None);
        assert_eq!(
            TranslateConfig::from_document(&doc, "ja-JP").target,
            Lang::Ja
        );
        doc.set_value(KEY_TARGET_LANGUAGE, serde_json::json!("fr"))
            .expect("目标");
        assert_eq!(
            TranslateConfig::from_document(&doc, "de-DE").target,
            Lang::Fr
        );
    }

    /// 模型根目录：配置为空用数据根下的默认位置。
    #[test]
    fn models_dir_resolution() {
        let host = TranslateHost::with_env(Path::new("D:/data"), None, None);
        let mut cfg = config();
        assert_eq!(
            host.models_dir(&cfg),
            Path::new("D:/data").join("models").join("translate")
        );
        cfg.models_dir = Some(PathBuf::from("E:/m"));
        assert_eq!(host.models_dir(&cfg), PathBuf::from("E:/m"));
    }

    /// 没有模型：NoModelFound，说明里带目录路径与问题清单；不启动任何进程。
    #[test]
    fn no_model_gives_guidance_with_issues() {
        let root = temp_root("nomodel");
        let (host, models) = host_with_files(&root);
        let mut cfg = config();
        cfg.target = Lang::ZhHans;
        let err = host.prepare(&cfg).unwrap_err();
        let TranslateError::NoModelFound(message) = err else {
            panic!("应为 NoModelFound: {err:?}")
        };
        assert!(message.contains(&models.display().to_string()), "{message}");
        std::fs::create_dir_all(models.join("broken")).expect("建目录");
        std::fs::write(models.join("broken").join("model.json"), "{oops").expect("写坏清单");
        let TranslateError::NoModelFound(message) = host.prepare(&cfg).unwrap_err() else {
            panic!("类型")
        };
        assert!(
            message.contains("broken") && message.contains("解析失败"),
            "{message}"
        );
        assert_eq!(host.worker_launches(), 0);
        let _ = std::fs::remove_dir_all(&root);
    }

    /// 语言对不支持：有模型但不支持时报 UnsupportedLanguagePair。
    #[test]
    fn unsupported_pair_is_reported() {
        let root = temp_root("pair");
        let (host, models) = host_with_files(&root);
        write_model(&models, "en-zh", r#"[["en","zh-CN"]]"#);
        let mut cfg = config();
        cfg.target = Lang::Ja;
        assert!(matches!(
            host.prepare(&cfg),
            Err(TranslateError::UnsupportedLanguagePair(_, Lang::Ja))
        ));
        let _ = std::fs::remove_dir_all(&root);
    }

    /// 缺运行时：RuntimeMissing（可下载）；环境变量指错文件：WorkerUnavailable（下载无用）；缺 worker：WorkerUnavailable。
    #[test]
    fn missing_runtime_and_worker_are_distinguished() {
        let root = temp_root("degrade");
        let models = root.join("models").join("translate");
        write_model(&models, "en-zh", r#"[["en","zh-CN"]]"#);
        let mut cfg = config();
        cfg.target = Lang::ZhHans;
        let exe = root.join("cisox-translator.exe");
        std::fs::write(&exe, b"x").expect("写 exe");
        let no_runtime = TranslateHost::with_env(&root, None, Some(exe.clone()));
        let err = no_runtime.prepare(&cfg).unwrap_err();
        assert!(err.can_download_runtime(), "{err:?}");
        let bad_env =
            TranslateHost::with_env(&root, Some("Z:/nope/onnxruntime.dll".into()), Some(exe));
        let err = bad_env.prepare(&cfg).unwrap_err();
        assert!(matches!(err, TranslateError::WorkerUnavailable(_)) && !err.can_download_runtime());
        let dll = root.join("onnxruntime.dll");
        std::fs::write(&dll, b"x").expect("写 dll");
        let no_exe = TranslateHost::with_env(
            &root,
            Some(dll.to_string_lossy().into_owned()),
            Some(root.join("missing.exe")),
        );
        assert!(
            matches!(no_exe.prepare(&cfg), Err(TranslateError::WorkerUnavailable(m)) if m.contains("does not exist"))
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    /// 装配成功：解析 Auto 源语言并返回展示名；同参数复用引擎，束宽变化则重建；不拉起进程。
    #[test]
    fn prepare_reuses_engine_until_config_changes() {
        let root = temp_root("prepare");
        let (host, models) = host_with_files(&root);
        write_model(&models, "en-zh", r#"[["en","zh-CN"]]"#);
        let mut cfg = config();
        cfg.target = Lang::ZhHans;
        let (label, src, tgt) = host.prepare(&cfg).expect("装配");
        assert_eq!(
            (label.as_str(), src, tgt),
            ("Model en-zh", Lang::Auto, Lang::ZhHans)
        );
        let first = host
            .built
            .lock()
            .unwrap()
            .as_ref()
            .map(|b| Arc::as_ptr(&b.engine) as *const () as usize);
        host.prepare(&cfg).expect("再次装配");
        let second = host
            .built
            .lock()
            .unwrap()
            .as_ref()
            .map(|b| Arc::as_ptr(&b.engine) as *const () as usize);
        assert_eq!(first, second, "同参数应复用");
        cfg.beams = 3;
        host.prepare(&cfg).expect("换束宽");
        let third = host
            .built
            .lock()
            .unwrap()
            .as_ref()
            .map(|b| Arc::as_ptr(&b.engine) as *const () as usize);
        assert_ne!(first, third, "束宽变化应重建");
        assert_eq!(host.worker_launches(), 0);
        assert!(!host.worker_running());
        host.shutdown();
        let _ = std::fs::remove_dir_all(&root);
    }

    /// OpenAI 通道：没配模型 / 没选模型时给出明确提示；选中后可装配。
    #[test]
    fn openai_backend_requires_a_selected_model() {
        let root = temp_root("openai");
        let host = TranslateHost::with_env(&root, None, None);
        let mut cfg = config();
        cfg.backend = Backend::OpenAi;
        assert!(matches!(
            host.prepare(&cfg),
            Err(TranslateError::NoCustomModel)
        ));
        cfg.custom_models = vec![CustomAiModel {
            id: "11111111-1111-1111-1111-111111111111".into(),
            name: "本地 Ollama".into(),
            base_url: "http://localhost:11434/v1".into(),
            api_key: "k".into(),
            model: "qwen2.5".into(),
            supports_vision: false,
            supports_reasoning: false,
        }];
        assert!(matches!(
            host.prepare(&cfg),
            Err(TranslateError::CustomModelNotSelected)
        ));
        cfg.custom_model_id = "11111111-1111-1111-1111-111111111111".into();
        let (label, src, _) = host.prepare(&cfg).expect("装配");
        assert_eq!((label.as_str(), src), ("本地 Ollama", Lang::Auto));
        let _ = std::fs::remove_dir_all(&root);
    }

    /// 假翻译后端：加前缀，记录调用。
    struct FakeTranslator {
        /// 收到的段落。
        seen: Mutex<Vec<Vec<String>>>,
        /// 预设失败。
        fail: Option<TranslateError>,
    }

    impl Translator for FakeTranslator {
        fn translate(
            &self,
            _config: &TranslateConfig,
            texts: &[String],
        ) -> Result<Translated, TranslateError> {
            self.seen.lock().unwrap().push(texts.to_vec());
            if let Some(e) = &self.fail {
                return Err(e.clone());
            }
            Ok(Translated {
                texts: texts.iter().map(|t| format!("译:{t}")).collect(),
                label: "fake".into(),
            })
        }
    }

    /// 造 OCR 结果。
    fn ocr(lines: &[(&str, i32)]) -> OcrResult {
        let boxes: Vec<OcrTextBox> = lines
            .iter()
            .map(|(t, y)| OcrTextBox {
                rect: PhysicalRect::new(0, *y, 200, 20),
                text: (*t).to_string(),
                confidence: Some(0.9),
            })
            .collect();
        OcrResult {
            full_text: lines.iter().map(|l| l.0).collect::<Vec<_>>().join("\n"),
            boxes,
            elapsed_ms: 7,
            table: None,
            latex: None,
        }
    }

    /// 流程：OCR → 合并段落 → 翻译，阶段回调按序触发，结果含逐段对照。
    #[test]
    fn flow_merges_translates_and_reports_stages() {
        let translator = FakeTranslator {
            seen: Mutex::new(Vec::new()),
            fail: None,
        };
        let mut cfg = config();
        cfg.target = Lang::ZhHans;
        let mut stages = Vec::new();
        let outcome = run_flow(
            || {
                Ok(ocr(&[
                    ("Hello there", 0),
                    ("my friend", 22),
                    ("Second para.", 100),
                ]))
            },
            &translator,
            &cfg,
            |s| stages.push(s),
        )
        .expect("流程");
        assert_eq!(
            stages,
            [TranslateStage::Recognizing, TranslateStage::Translating]
        );
        assert_eq!(
            translator.seen.lock().unwrap()[0],
            ["Hello there my friend", "Second para."]
        );
        assert_eq!(
            outcome.translated,
            "译:Hello there my friend\n译:Second para."
        );
        assert_eq!(outcome.pairs.len(), 2);
        assert_eq!(outcome.pairs[0].box_indices, vec![0, 1]);
        assert_eq!(outcome.pairs[1].box_indices, vec![2]);
        assert_eq!(outcome.boxes.len(), 3);
        assert_eq!((outcome.ocr_ms, outcome.label.as_str()), (7, "fake"));
    }

    /// 流程失败分支：OCR 缺资产、没识别到文字、原文已是中文（不调用翻译）、翻译失败。
    #[test]
    fn flow_failure_branches() {
        let translator = FakeTranslator {
            seen: Mutex::new(Vec::new()),
            fail: None,
        };
        let mut cfg = config();
        cfg.target = Lang::ZhHans;
        let err = run_flow(
            || Err(OcrError::Unavailable(OcrUnavailable::NoRuntime)),
            &translator,
            &cfg,
            |_| {},
        )
        .unwrap_err();
        assert_eq!(
            err,
            TranslateFlowError::Ocr(OcrError::Unavailable(OcrUnavailable::NoRuntime))
        );
        assert_eq!(
            run_flow(|| Ok(ocr(&[("   ", 0)])), &translator, &cfg, |_| {}).unwrap_err(),
            TranslateFlowError::NoText
        );
        assert_eq!(
            run_flow(
                || Ok(ocr(&[("今天天气很好，我们出去玩吧", 0)])),
                &translator,
                &cfg,
                |_| {}
            )
            .unwrap_err(),
            TranslateFlowError::AlreadyTarget(Lang::ZhHans)
        );
        assert!(
            translator.seen.lock().unwrap().is_empty(),
            "以上三种都不应调用翻译"
        );
        cfg.target = Lang::En;
        assert!(
            run_flow(
                || Ok(ocr(&[("今天天气很好", 0)])),
                &translator,
                &cfg,
                |_| {}
            )
            .is_ok(),
            "翻成英文时中文原文正常"
        );
        let failing = FakeTranslator {
            seen: Mutex::new(Vec::new()),
            fail: Some(TranslateError::Timeout),
        };
        assert_eq!(
            run_flow(|| Ok(ocr(&[("Hello", 0)])), &failing, &cfg, |_| {}).unwrap_err(),
            TranslateFlowError::Translate(TranslateError::Timeout)
        );
    }

    /// 假 worker 的可观察状态。
    #[derive(Default)]
    struct FakeWorkerState {
        /// 待接收的行。
        queue: std::collections::VecDeque<String>,
        /// 收到过的命令。
        sent: Vec<snow_translate::protocol::Command>,
        /// 通道已关闭（进程退出）。
        closed: bool,
        /// 是否收到过 unload。
        unloaded: bool,
    }

    /// 假 worker 通道：同步应答，不开线程。
    struct FakeWorkerTransport {
        /// 共享状态。
        state: Arc<Mutex<FakeWorkerState>>,
        /// 处理到 `Load` 时置位的标志（测试里模拟“加载期间用户取消”）。
        load_flag: Option<Arc<AtomicBool>>,
    }

    impl snow_translate::worker::Transport for FakeWorkerTransport {
        fn send(&mut self, line: &str) -> std::io::Result<()> {
            use snow_translate::protocol::{Command, Event};
            let command: Command = serde_json::from_str(line).map_err(std::io::Error::other)?;
            let mut state = self.state.lock().unwrap();
            state.sent.push(command.clone());
            let event = match command {
                Command::Load { .. } => {
                    if let Some(flag) = &self.load_flag {
                        flag.store(true, Ordering::SeqCst);
                    }
                    Some(Event::Loaded {
                        model_id: "fake".into(),
                        load_ms: 1,
                        mem_bytes: 10,
                    })
                }
                Command::Translate { id, texts, .. } => Some(Event::Result {
                    id,
                    texts: texts.iter().map(|t| format!("译:{t}")).collect(),
                    elapsed_ms: 1,
                }),
                Command::Ping => Some(Event::Pong {
                    loaded: true,
                    mem_bytes: 5,
                    peak_bytes: 9,
                }),
                Command::Unload => {
                    state.unloaded = true;
                    state.closed = true;
                    Some(Event::Unloaded)
                }
            };
            if let Some(event) = event {
                state
                    .queue
                    .push_back(serde_json::to_string(&event).unwrap());
            }
            Ok(())
        }

        fn recv(&mut self, timeout: Duration) -> snow_translate::worker::Recv {
            use snow_translate::worker::Recv;
            let mut state = self.state.lock().unwrap();
            if let Some(line) = state.queue.pop_front() {
                return Recv::Line(line);
            }
            if state.closed {
                return Recv::Closed;
            }
            drop(state);
            std::thread::sleep(timeout.min(Duration::from_millis(10)));
            Recv::Timeout
        }

        fn diagnostics(&mut self) -> String {
            "fake exit".into()
        }

        fn terminate(&mut self) {
            self.state.lock().unwrap().closed = true;
        }

        fn wait_exit(&mut self, _timeout: Duration) -> bool {
            self.state.lock().unwrap().closed
        }
    }

    /// 假拉起方式：每次拉起建一个假 worker 并记下它的状态。
    #[derive(Default)]
    struct FakeWorkers {
        /// 已拉起会话的状态。
        states: Mutex<Vec<Arc<Mutex<FakeWorkerState>>>>,
        /// 传给每个假通道的 `Load` 标志。
        load_flag: Option<Arc<AtomicBool>>,
    }

    impl FakeWorkers {
        /// 拉起次数。
        fn launches(&self) -> usize {
            self.states.lock().unwrap().len()
        }

        /// 全部会话收到的 `Load` 条数。
        fn loads(&self) -> usize {
            self.states
                .lock()
                .unwrap()
                .iter()
                .map(|s| {
                    s.lock()
                        .unwrap()
                        .sent
                        .iter()
                        .filter(|c| matches!(c, snow_translate::protocol::Command::Load { .. }))
                        .count()
                })
                .sum()
        }

        /// 第 n 次拉起的会话是否收到过 unload。
        fn unloaded(&self, n: usize) -> bool {
            self.states.lock().unwrap()[n].lock().unwrap().unloaded
        }
    }

    impl WorkerLauncher for FakeWorkers {
        fn launch(
            &self,
            _config: &WorkerConfig,
        ) -> Result<Box<dyn snow_translate::worker::Transport>, TranslateError> {
            let mut state = FakeWorkerState::default();
            let ready = snow_translate::protocol::Event::Ready {
                protocol: snow_translate::protocol::PROTOCOL_VERSION,
                pid: 4242,
            };
            state
                .queue
                .push_back(serde_json::to_string(&ready).unwrap());
            let state = Arc::new(Mutex::new(state));
            self.states.lock().unwrap().push(Arc::clone(&state));
            Ok(Box::new(FakeWorkerTransport {
                state,
                load_flag: self.load_flag.clone(),
            }))
        }
    }

    /// 造带假 worker 的宿主：只有一个英→中专用包，返回宿主、假拉起器与模型根。
    fn warm_host(
        root: &Path,
        load_flag: Option<Arc<AtomicBool>>,
    ) -> (Arc<TranslateHost>, Arc<FakeWorkers>) {
        let (host, models) = host_with_files(root);
        write_model(&models, "z-opus", r#"[["en","zh-CN"]]"#);
        let workers = Arc::new(FakeWorkers {
            load_flag,
            ..FakeWorkers::default()
        });
        let host = host.with_launcher(workers.clone());
        (Arc::new(host), workers)
    }

    /// 默认目标简体中文、源语言自动的本地配置（装一个英→中包时目标语言能唯一定包）。
    fn warm_config() -> TranslateConfig {
        let mut cfg = config();
        cfg.target = Lang::ZhHans;
        cfg
    }

    /// 轮询等条件成立（最多 3 秒）。
    fn wait_until(mut condition: impl FnMut() -> bool) -> bool {
        let deadline = Instant::now() + Duration::from_secs(3);
        while Instant::now() < deadline {
            if condition() {
                return true;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        condition()
    }

    /// 预热：能确定用哪个包时提前拉起并加载；随后的真实翻译不再拉起也不再加载，且不会被回收。
    #[test]
    fn warm_preloads_and_translate_reuses_worker() {
        let root = temp_root("warm-reuse");
        let (host, workers) = warm_host(&root, None);
        let cfg = warm_config();
        assert_eq!(host.warm(&cfg, &|| false), Some("z-opus".to_string()));
        assert_eq!((workers.launches(), workers.loads()), (1, 1));
        assert!(host.worker_running());
        let out = host.translate(&cfg, &["Hello".to_string()]).expect("翻译");
        assert_eq!(out.texts, ["译:Hello"]);
        assert_eq!(out.label, "Model z-opus");
        assert_eq!(
            (workers.launches(), workers.loads()),
            (1, 1),
            "沿用预热的进程"
        );
        host.release_unused_warm();
        assert!(host.worker_running(), "用上了就不回收");
        host.shutdown();
        let _ = std::fs::remove_dir_all(&root);
    }

    /// 预热后从没被用上：release_unused_warm 立刻卸载，进程退出。
    #[test]
    fn release_unused_warm_unloads_worker() {
        let root = temp_root("warm-release");
        let (host, workers) = warm_host(&root, None);
        host.warm(&warm_config(), &|| false);
        assert!(host.worker_running());
        host.release_unused_warm();
        assert!(!host.worker_running());
        assert!(workers.unloaded(0));
        let _ = std::fs::remove_dir_all(&root);
    }

    /// 不该预热的情况都不拉进程：OpenAI 后端、没有模型、已放弃。
    #[test]
    fn warm_skips_when_not_applicable() {
        let root = temp_root("warm-skip");
        let (host, workers) = warm_host(&root, None);
        let mut cfg = warm_config();
        assert_eq!(host.warm(&cfg, &|| true), None, "已放弃");
        cfg.backend = Backend::OpenAi;
        assert_eq!(host.warm(&cfg, &|| false), None, "非本地后端");
        let empty_root = temp_root("warm-skip-empty");
        let (empty_host, empty_workers) = warm_host(&empty_root, None);
        std::fs::remove_dir_all(empty_host.models_dir(&warm_config())).expect("清空模型");
        assert_eq!(empty_host.warm(&warm_config(), &|| false), None, "没有模型");
        assert_eq!((workers.launches(), empty_workers.launches()), (0, 0));
        for dir in [root, empty_root] {
            let _ = std::fs::remove_dir_all(dir);
        }
    }

    /// 通用多语包 + 自动源语言（语言对不唯一）：照样预热该包；随后换语言对翻译也不重新加载。
    #[test]
    fn warm_auto_source_with_general_pack() {
        let root = temp_root("warm-general");
        let (host, models) = host_with_files(&root);
        write_general_model(&models, "a-nllb");
        let workers = Arc::new(FakeWorkers::default());
        let host = host.with_launcher(workers.clone());
        assert_eq!(
            host.warm(&warm_config(), &|| false),
            Some("a-nllb".to_string())
        );
        assert_eq!((workers.launches(), workers.loads()), (1, 1));
        let mut cfg = warm_config();
        cfg.source = Lang::En;
        host.translate(&cfg, &["Hello".to_string()]).expect("翻译");
        cfg.source = Lang::Ja;
        host.translate(&cfg, &["こんにちは".to_string()])
            .expect("换语言对翻译");
        assert_eq!(
            (workers.launches(), workers.loads()),
            (1, 1),
            "换语言对不重新拉起也不重新加载"
        );
        host.shutdown();
        let _ = std::fs::remove_dir_all(&root);
    }

    /// 源语言固定时按配置定包；目标语言没有任何包覆盖时不预热。
    #[test]
    fn warm_respects_fixed_source_and_uncovered_target() {
        let root = temp_root("warm-fixed");
        let (host, workers) = warm_host(&root, None);
        let mut cfg = warm_config();
        cfg.source = Lang::En;
        assert_eq!(host.warm(&cfg, &|| false), Some("z-opus".to_string()));
        assert_eq!(workers.launches(), 1);
        cfg.target = Lang::Ja;
        assert_eq!(host.warm(&cfg, &|| false), None, "没有包覆盖日语");
        assert_eq!(workers.launches(), 1);
        host.shutdown();
        let _ = std::fs::remove_dir_all(&root);
    }

    /// 加载期间被放弃：预热完成后立刻发现并卸载，不留进程。
    #[test]
    fn warm_aborted_during_load_is_recycled() {
        let root = temp_root("warm-abort");
        let flag = Arc::new(AtomicBool::new(false));
        let (host, workers) = warm_host(&root, Some(Arc::clone(&flag)));
        let abort_flag = Arc::clone(&flag);
        let warmed = host.warm(&warm_config(), &|| abort_flag.load(Ordering::SeqCst));
        assert_eq!(warmed, None);
        assert!(!host.worker_running());
        assert!(workers.unloaded(0));
        let _ = std::fs::remove_dir_all(&root);
    }

    /// WarmHandle：与流程并行预热；流程结束时没用上的预热被卸载。
    #[test]
    fn warm_handle_recycles_unused_warm_on_finish() {
        let root = temp_root("warm-handle");
        let (host, workers) = warm_host(&root, None);
        let handle = WarmHandle::start(
            Arc::clone(&host),
            warm_config(),
            Arc::new(AtomicBool::new(false)),
        );
        assert!(wait_until(|| host.worker_running()), "应在后台预热起来");
        handle.finish(host.as_ref());
        assert!(!host.worker_running());
        assert!(workers.unloaded(0));
        assert_eq!((workers.launches(), workers.loads()), (1, 1));
        let _ = std::fs::remove_dir_all(&root);
    }

    /// WarmHandle：预热被真实翻译用上后，流程结束不回收。
    #[test]
    fn warm_handle_keeps_worker_that_was_used() {
        let root = temp_root("warm-handle-used");
        let (host, workers) = warm_host(&root, None);
        let cfg = warm_config();
        let handle = WarmHandle::start(
            Arc::clone(&host),
            cfg.clone(),
            Arc::new(AtomicBool::new(false)),
        );
        // 翻译与预热并发：谁先到都只有一次拉起、一次加载
        host.translate(&cfg, &["Hi".to_string()]).expect("翻译");
        handle.finish(host.as_ref());
        assert!(host.worker_running());
        assert_eq!((workers.launches(), workers.loads()), (1, 1));
        host.shutdown();
        let _ = std::fs::remove_dir_all(&root);
    }

    /// WarmHandle：已取消的请求不预热；流程一结束就 finish，不论预热线程跑到哪一步都不会留下进程。
    #[test]
    fn warm_handle_cancelled_or_finished_early_leaves_nothing() {
        let root = temp_root("warm-handle-cancel");
        let (host, workers) = warm_host(&root, None);
        let cancelled = WarmHandle::start(
            Arc::clone(&host),
            warm_config(),
            Arc::new(AtomicBool::new(true)),
        );
        cancelled.finish(host.as_ref());
        let early = WarmHandle::start(
            Arc::clone(&host),
            warm_config(),
            Arc::new(AtomicBool::new(false)),
        );
        early.finish(host.as_ref());
        std::thread::sleep(Duration::from_millis(300));
        assert!(!host.worker_running());
        assert!(workers.launches() <= 1, "最多因抢在 finish 前而拉起一次");
        let dropped = WarmHandle::start(
            Arc::clone(&host),
            warm_config(),
            Arc::new(AtomicBool::new(true)),
        );
        drop(dropped);
        let _ = std::fs::remove_dir_all(&root);
    }

    /// 补预热：包被回收后重新预热（再拉起一次）；包还在内存时什么都不做。
    #[test]
    fn rewarm_on_input_reloads_only_after_recycle() {
        let root = temp_root("rewarm-on-input");
        let (host, workers) = warm_host(&root, None);
        let first = WarmHandle::start(
            Arc::clone(&host),
            warm_config(),
            Arc::new(AtomicBool::new(false)),
        );
        assert!(wait_until(|| host.worker_running()), "应在后台预热起来");
        // 包还在内存：补热不应再拉起
        let idle = WarmHandle::rewarm_on_input(
            Arc::clone(&host),
            warm_config(),
            Arc::new(AtomicBool::new(false)),
        );
        std::thread::sleep(Duration::from_millis(200));
        assert_eq!((workers.launches(), workers.loads()), (1, 1));
        // 预热被回收（TTL / 关窗）后：补热重新拉起并加载
        first.finish(host.as_ref());
        assert!(!host.worker_running());
        let again = WarmHandle::rewarm_on_input(
            Arc::clone(&host),
            warm_config(),
            Arc::new(AtomicBool::new(false)),
        );
        assert!(wait_until(|| host.worker_running()), "应重新预热起来");
        assert_eq!((workers.launches(), workers.loads()), (2, 2));
        // 关窗：全部句柄回收，不留进程
        idle.finish(host.as_ref());
        again.finish(host.as_ref());
        assert!(!host.worker_running());
        let _ = std::fs::remove_dir_all(&root);
    }

    /// 非本地后端不开预热线程。
    #[test]
    fn warm_handle_skips_openai_backend() {
        let root = temp_root("warm-handle-openai");
        let (host, workers) = warm_host(&root, None);
        let mut cfg = warm_config();
        cfg.backend = Backend::OpenAi;
        let handle = WarmHandle::start(Arc::clone(&host), cfg, Arc::new(AtomicBool::new(false)));
        handle.finish(host.as_ref());
        std::thread::sleep(Duration::from_millis(100));
        assert_eq!(workers.launches(), 0);
        let _ = std::fs::remove_dir_all(&root);
    }

    /// 取消标志登记：只认最近一次登记的序号；序号对不上（已被新请求取代）的取消被忽略。
    #[test]
    fn cancel_slot_matches_latest_serial_only() {
        let mut slot = CancelSlot::default();
        assert!(!slot.cancel(1), "没登记过");
        let first = slot.begin(1);
        let second = slot.begin(2);
        assert!(!slot.cancel(1), "1 已被 2 取代");
        assert!(!first.load(Ordering::SeqCst) && !second.load(Ordering::SeqCst));
        assert!(slot.cancel(2));
        assert!(second.load(Ordering::SeqCst) && !first.load(Ordering::SeqCst));
    }

    /// 取消的流程：识别结束后发现已取消就不调用翻译，返回 Cancelled；未取消照常。
    #[test]
    fn flow_cancelled_after_recognition_skips_translation() {
        let translator = FakeTranslator {
            seen: Mutex::new(Vec::new()),
            fail: None,
        };
        let cfg = warm_config();
        let mut stages = Vec::new();
        let err = run_flow_cancellable(
            || Ok(ocr(&[("Hello", 0)])),
            &translator,
            &cfg,
            |s| stages.push(s),
            || true,
        )
        .unwrap_err();
        assert_eq!(err, TranslateFlowError::Cancelled);
        assert_eq!(stages, [TranslateStage::Recognizing]);
        assert!(translator.seen.lock().unwrap().is_empty());
        let ok = run_flow_cancellable(
            || Ok(ocr(&[("Hello", 0)])),
            &translator,
            &cfg,
            |_| {},
            || false,
        );
        assert!(ok.is_ok());
    }

    /// 探针（环境变量齐全才运行）：真实模型下的拉起 / 首次加载 / 单句延迟 / 翻译期间内存 / 空闲卸载后的回收。
    ///
    /// 需要：`SNOW_TRANSLATOR_EXE`、`SNOW_ORT_DYLIB`、`SNOW_TRANSLATOR_TEST_MODEL_DIR`（模型目录，其父目录作为模型根）。
    /// 用 `cargo test --release -p snow-shot probe_real -- --nocapture` 查看 `PROBE` 行。
    #[test]
    fn probe_real_translation_memory_and_idle() {
        use snow_platform::process_mem::{current_process_memory, process_memory};
        use std::time::Instant;
        let (Some(exe), Some(dll), Some(model_dir)) = (
            std::env::var_os(ENV_TRANSLATOR_EXE),
            std::env::var(snow_translate::worker::ENV_ORT_DYLIB).ok(),
            std::env::var_os("SNOW_TRANSLATOR_TEST_MODEL_DIR"),
        ) else {
            eprintln!(
                "跳过探针：未设置 SNOW_TRANSLATOR_EXE / SNOW_ORT_DYLIB / SNOW_TRANSLATOR_TEST_MODEL_DIR"
            );
            return;
        };
        let model_dir = PathBuf::from(model_dir);
        let root = temp_root("probe");
        let host = TranslateHost::with_env(&root, Some(dll), Some(PathBuf::from(exe)));
        let mib = |bytes: u64| bytes as f64 / (1024.0 * 1024.0);
        let sentences = [
            "Hello world.",
            "The quick brown fox jumps over the lazy dog.",
            "Please save your work before closing the application.",
            "Where is the nearest train station?",
            "I would like to order a cup of coffee and a piece of cake.",
            "The meeting has been rescheduled to next Tuesday at 3 pm.",
        ];
        for (mode, low_memory, beams) in [("beam4", false, 4usize), ("greedy-low-memory", true, 1)]
        {
            let mut cfg = config();
            cfg.target = Lang::ZhHans;
            cfg.models_dir = model_dir.parent().map(Path::to_path_buf);
            cfg.model_id = String::new();
            cfg.idle = Duration::from_secs(3);
            cfg.beams = beams;
            cfg.low_memory = low_memory;
            let main_before = current_process_memory().map(|m| m.working_set).unwrap_or(0);
            let started = Instant::now();
            let first = host
                .translate(&cfg, &[sentences[0].to_string()])
                .expect("首句翻译");
            let cold_ms = started.elapsed().as_millis();
            let pid = host.worker_memory().map(|m| m.pid).unwrap_or(0);
            eprintln!(
                "PROBE [{mode}] cold(启动+加载+首句)={cold_ms}ms 首句={:?} worker_pid={pid}",
                first.texts
            );
            let mut times = Vec::new();
            let mut peak = 0u64;
            for text in &sentences[1..] {
                let t = Instant::now();
                let out = host.translate(&cfg, &[text.to_string()]).expect("翻译");
                times.push(t.elapsed().as_millis());
                if let Some(m) = process_memory(pid) {
                    peak = peak.max(m.peak_working_set);
                }
                eprintln!(
                    "PROBE [{mode}] {}ms  {text} -> {}",
                    times.last().copied().unwrap_or(0),
                    out.texts[0]
                );
            }
            let snap = host.worker_memory();
            let worker_now = process_memory(pid).map(|m| m.working_set).unwrap_or(0);
            let main_during = current_process_memory().map(|m| m.working_set).unwrap_or(0);
            eprintln!(
                "PROBE [{mode}] 单句延迟(ms)={times:?} worker 当前={:.0}MiB 峰值={:.0}MiB (ping: {:?}) 主进程 前={:.0}MiB 中={:.0}MiB",
                mib(worker_now),
                mib(peak),
                snap.map(|m| (mib(m.mem_bytes) as u64, mib(m.peak_bytes) as u64)),
                mib(main_before),
                mib(main_during)
            );
            let idle_started = Instant::now();
            while host.worker_running() && idle_started.elapsed() < Duration::from_secs(15) {
                std::thread::sleep(Duration::from_millis(100));
            }
            let gone_after = idle_started.elapsed().as_millis();
            std::thread::sleep(Duration::from_millis(500));
            let alive = process_memory(pid).is_some();
            let main_after = current_process_memory().map(|m| m.working_set).unwrap_or(0);
            eprintln!(
                "PROBE [{mode}] 空闲卸载: running={} 用时={gone_after}ms 进程仍在={alive} 主进程 卸载后={:.0}MiB",
                host.worker_running(),
                mib(main_after)
            );
            assert!(
                !host.worker_running() && !alive,
                "空闲后 worker 进程必须已退出"
            );
            let t = Instant::now();
            // 换一句没翻译过的，避免命中结果缓存而没有真正重新拉起
            let fresh = format!("Thank you very much for your help, mode {mode}.");
            let again = host.translate(&cfg, &[fresh]).expect("卸载后重新拉起");
            eprintln!(
                "PROBE [{mode}] 卸载后再次翻译（重新拉起+加载+翻译）={}ms 累计拉起={} 译文={:?}",
                t.elapsed().as_millis(),
                host.worker_launches(),
                again.texts
            );
            assert_eq!(host.worker_launches(), 2, "卸载后应重新拉起一次");
            host.shutdown();
        }
        let _ = std::fs::remove_dir_all(&root);
    }

    /// 汉字占比判断：纯中文、混排、英文、无字母。
    #[test]
    fn cjk_ratio() {
        assert!(is_mostly_cjk("你好世界"));
        assert!(is_mostly_cjk("你好世界 ok"));
        assert!(!is_mostly_cjk("Hello 世"));
        assert!(!is_mostly_cjk("12345 !!!"));
        assert!(!is_mostly_cjk(""));
    }

    /// worker 可执行文件定位：环境变量指向不存在的文件报错；存在则直接用。
    #[test]
    fn worker_exe_lookup() {
        let root = temp_root("exe");
        let exe = root.join("t.exe");
        std::fs::write(&exe, b"x").expect("写");
        assert_eq!(locate_worker_exe(Some(&exe)).unwrap(), exe);
        assert!(matches!(
            locate_worker_exe(Some(&root.join("missing.exe"))),
            Err(TranslateError::WorkerUnavailable(m)) if m.contains(ENV_TRANSLATOR_EXE)
        ));
        let _ = std::fs::remove_dir_all(&root);
    }
}
