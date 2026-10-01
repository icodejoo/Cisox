//! 文字翻译的宿主服务：配置解析、后端装配（本地 NMT worker / OpenAI 兼容）与“选区 → OCR → 翻译”流程。
//!
//! 后端由 `snow-translate` 提供；这里负责把设置页的配置变成具体引擎，并在配置变化时换掉旧引擎
//! （旧引擎释放即结束其 worker 进程）。没有模型、没有运行时、OCR 缺资产时一律返回明确错误，
//! 不会产出任何假译文。翻译是阻塞调用，必须在后台线程里执行。

use crate::ocr_client::OcrError;
use crate::ocr_service::OcrResult;
use crate::ort_runtime::{OrtUnavailable, resolve_ort_dylib};
use crate::translate_layout::{LayoutMode, paragraphs_from_boxes};
use serde_json::Value;
use snow_config::custom_models::{CustomAiModel, custom_ai_models_from_json};
use snow_config::document::ConfigDocument;
use snow_config::extensions::{
    BACKEND_OPENAI, DEFAULT_IDLE_SECONDS, DEFAULT_NUM_BEAMS, KEY_LOCAL_IDLE_SECONDS, KEY_LOCAL_LOW_MEMORY,
    KEY_LOCAL_MODEL_ID, KEY_LOCAL_MODELS_DIR, KEY_LOCAL_NUM_BEAMS, KEY_TRANSLATION_BACKEND, MAX_NUM_BEAMS,
};
use snow_translate::openai::{OpenAiCompatibleConfig, OpenAiEngine};
use snow_translate::protocol::MAX_BEAMS;
use snow_translate::worker::{MemorySnapshot, Timeouts, WORKER_EXE_NAME, WorkerConfig, WorkerEngine};
use snow_translate::{
    Lang, ManifestIssue, ModelScanner, ScanReport, TranslateError, TranslationEngine, TranslationService, pick_model,
};
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

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
/// 配置键：界面语言。
pub const KEY_INTERFACE_LANGUAGE: &str = "interface/language";
/// 环境变量：直接指定 `snow-translator.exe`（开发 / 自测用）。
pub const ENV_TRANSLATOR_EXE: &str = "SNOW_TRANSLATOR_EXE";
/// 模型根目录相对数据根的路径。
const MODELS_SUBDIR: [&str; 2] = ["models", "translate"];
/// 说明里最多列出的问题清单条数。
const MAX_ISSUES_SHOWN: usize = 3;
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

/// 由界面语言得到默认目标语言：简体/繁体中文界面翻成对应中文，其它翻成英文。
///
/// # 参数
/// - `configured`：配置里的界面语言（`system` / `en_US` / `zh_CN` / `zh_TW`）。
/// - `system_language`：系统语言标记（如 `zh-CN`），仅在 `system` 时使用。
///
/// ```ignore
/// assert_eq!(default_target("zh_TW", "en-US"), Lang::ZhHant);
/// ```
pub fn default_target(configured: &str, system_language: &str) -> Lang {
    let value = if configured.trim().eq_ignore_ascii_case("system") { system_language } else { configured };
    let lower = value.trim().to_lowercase();
    if lower.starts_with("zh") {
        if ["tw", "hk", "mo", "hant"].iter().any(|tag| lower.contains(tag)) { Lang::ZhHant } else { Lang::ZhHans }
    } else {
        Lang::En
    }
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
            document.value(key).as_i64().and_then(|n| i32::try_from(n).ok()).unwrap_or(default)
        };
        let models_dir = Some(text(KEY_LOCAL_MODELS_DIR)).filter(|d| !d.is_empty()).map(PathBuf::from);
        let target = Lang::from_code(&text(KEY_TARGET_LANGUAGE))
            .filter(|l| *l != Lang::Auto)
            .unwrap_or_else(|| default_target(&text(KEY_INTERFACE_LANGUAGE), system_language));
        let (custom_models, _) = custom_ai_models_from_json(&document.value(KEY_CUSTOM_MODELS));
        Self {
            backend: if text(KEY_TRANSLATION_BACKEND) == BACKEND_OPENAI { Backend::OpenAi } else { Backend::Local },
            models_dir,
            model_id: text(KEY_LOCAL_MODEL_ID),
            idle: Duration::from_secs(number(KEY_LOCAL_IDLE_SECONDS, DEFAULT_IDLE_SECONDS).max(1) as u64),
            beams: number(KEY_LOCAL_NUM_BEAMS, DEFAULT_NUM_BEAMS).clamp(1, MAX_NUM_BEAMS) as usize,
            low_memory: document.value(KEY_LOCAL_LOW_MEMORY).as_bool().unwrap_or(false),
            source: Lang::from_code(&text(KEY_SOURCE_LANGUAGE)).unwrap_or(Lang::Auto),
            target,
            layout: LayoutMode::from_config(&text(KEY_LAYOUT)),
            custom_model_id: text(KEY_CUSTOM_MODEL),
            custom_models,
        }
    }

    /// 实际使用的束宽：低内存模式强制贪心。
    pub fn effective_beams(&self) -> usize {
        if self.low_memory { 1 } else { self.beams.clamp(1, MAX_BEAMS) }
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
    fn translate(&self, config: &TranslateConfig, texts: &[String]) -> Result<Translated, TranslateError>;
}

/// 已装配好的引擎。
struct Built {
    /// 装配参数指纹，变化则重建。
    key: String,
    /// 引擎。
    engine: Arc<dyn TranslationEngine>,
    /// 本地 worker 引擎（探针用）。
    worker: Option<Arc<WorkerEngine>>,
    /// 展示名。
    label: String,
    /// 解析出的源语言。
    src: Lang,
    /// 目标语言。
    tgt: Lang,
}

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
}

/// 本地模型不可用时给用户的说明：放哪里、哪些清单有问题。
fn no_model_message(dir: &Path, issues: &[ManifestIssue]) -> String {
    let mut text = format!(
        "模型目录 {} 里没有可用的翻译模型。请把模型放到该目录的子文件夹里（含 model.json、encoder/decoder onnx 与 tokenizer.json）",
        dir.display()
    );
    for issue in issues.iter().take(MAX_ISSUES_SHOWN) {
        text.push_str(&format!("；{} 无法使用: {}", issue.dir_name, issue.reason));
    }
    if issues.len() > MAX_ISSUES_SHOWN {
        text.push_str(&format!("；另有 {} 个模型无法使用", issues.len() - MAX_ISSUES_SHOWN));
    }
    text
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
    MODELS_SUBDIR.iter().fold(data_root.to_path_buf(), |dir, part| dir.join(part))
}

/// 定位 `snow-translator.exe`：环境变量优先，其次主程序同目录。
///
/// # 参数
/// - `env_exe`：`SNOW_TRANSLATOR_EXE` 的值。
///
/// # 返回
/// 可执行文件路径；找不到返回 `WorkerUnavailable`（说明查找位置）。
pub fn locate_worker_exe(env_exe: Option<&Path>) -> Result<PathBuf, TranslateError> {
    if let Some(path) = env_exe.filter(|p| !p.as_os_str().is_empty()) {
        return if path.is_file() {
            Ok(path.to_path_buf())
        } else {
            Err(TranslateError::WorkerUnavailable(format!(
                "{ENV_TRANSLATOR_EXE} 指向的文件不存在: {}",
                path.display()
            )))
        };
    }
    let beside = std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(|dir| dir.join(WORKER_EXE_NAME)));
    match beside {
        Some(path) if path.is_file() => Ok(path),
        Some(path) => Err(TranslateError::WorkerUnavailable(format!(
            "未找到翻译组件 {WORKER_EXE_NAME}（应位于 {}）",
            path.display()
        ))),
        None => Err(TranslateError::WorkerUnavailable(format!("未找到翻译组件 {WORKER_EXE_NAME}"))),
    }
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
            std::env::var_os(ENV_TRANSLATOR_EXE).filter(|v| !v.is_empty()).map(PathBuf::from),
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
        }
    }

    /// 模型根目录（配置为空用默认位置）。
    ///
    /// # 参数
    /// - `config`：翻译配置。
    pub fn models_dir(&self, config: &TranslateConfig) -> PathBuf {
        config.models_dir.clone().unwrap_or_else(|| default_models_dir(&self.data_root))
    }

    /// 扫描模型目录（设置页 / 诊断用）。
    ///
    /// # 参数
    /// - `config`：翻译配置。
    pub fn scan(&self, config: &TranslateConfig) -> ScanReport {
        ModelScanner::new(&self.models_dir(config)).scan()
    }

    /// 装配本地 NMT 引擎：选模型 → 找 worker → 找 onnxruntime → 构造（不拉起进程）。
    fn build_local(&self, config: &TranslateConfig) -> Result<(String, Built), TranslateError> {
        let dir = self.models_dir(config);
        let report = ModelScanner::new(&dir).scan();
        if report.models.is_empty() {
            return Err(TranslateError::NoModelFound(no_model_message(&dir, &report.issues)));
        }
        let (model, src) = pick_model(&report.models, &config.model_id, config.source, config.target)?;
        let exe = locate_worker_exe(self.exe_env.as_deref())?;
        let dylib = resolve_ort_dylib(&self.data_root, self.ort_env.as_deref()).map_err(|e| match e {
            OrtUnavailable::NotInstalled => TranslateError::RuntimeMissing(e.message()),
            other => TranslateError::WorkerUnavailable(other.message()),
        })?;
        let beams = config.effective_beams();
        let key = format!(
            "local|{}|{}|{}|{}|{beams}|{}|{}",
            exe.display(),
            model.dir.display(),
            model.manifest.id,
            dylib.display(),
            config.low_memory,
            config.idle.as_secs()
        );
        let label = if model.manifest.display_name.trim().is_empty() {
            model.manifest.id.clone()
        } else {
            model.manifest.display_name.clone()
        };
        let worker_config = WorkerConfig {
            exe,
            model_dir: model.dir.clone(),
            model_id: model.manifest.id.clone(),
            pairs: model.manifest.supported_pairs(),
            ort_dylib: Some(dylib),
            num_beams: beams,
            trim_after_request: true,
            idle_timeout: config.idle,
            timeouts: Timeouts::default(),
        };
        let worker = Arc::new(WorkerEngine::new(worker_config));
        let engine: Arc<dyn TranslationEngine> = worker.clone();
        Ok((
            key.clone(),
            Built { key, engine, worker: Some(worker), label, src, tgt: config.target },
        ))
    }

    /// 装配 OpenAI 兼容引擎：取配置里选中的自定义模型。
    fn build_openai(&self, config: &TranslateConfig) -> Result<(String, Built), TranslateError> {
        if config.custom_models.is_empty() {
            return Err(TranslateError::NoModelFound(
                "尚未配置自定义 AI 模型，请先在“自定义模型”里添加一个 OpenAI 兼容的端点".into(),
            ));
        }
        let model = config
            .custom_models
            .iter()
            .find(|m| m.id == config.custom_model_id)
            .ok_or_else(|| {
                TranslateError::NoModelFound("请在设置里为“文字翻译”选择一个自定义 AI 模型（screenshot_translation/model）".into())
            })?;
        let key = format!("openai|{}|{}|{}", model.base_url, model.model, fingerprint(&model.api_key));
        let engine: Arc<dyn TranslationEngine> = Arc::new(OpenAiEngine::new(OpenAiCompatibleConfig {
            base_url: model.base_url.clone(),
            api_key: model.api_key.clone(),
            model: model.model.clone(),
        }));
        Ok((
            key.clone(),
            Built { key, engine, worker: None, label: model.name.clone(), src: config.source, tgt: config.target },
        ))
    }

    /// 按配置装配引擎；参数没变就复用，变了就换掉旧引擎（旧 worker 随之退出）。
    ///
    /// # 参数
    /// - `config`：翻译配置。
    ///
    /// # 返回
    /// `(引擎标签, 解析出的源语言, 目标语言)`。
    pub fn prepare(&self, config: &TranslateConfig) -> Result<(String, Lang, Lang), TranslateError> {
        let (key, fresh) = match config.backend {
            Backend::Local => self.build_local(config)?,
            Backend::OpenAi => self.build_openai(config)?,
        };
        let mut built = self.built.lock().unwrap_or_else(PoisonError::into_inner);
        let (label, src, tgt) = match built.as_mut() {
            Some(current) if current.key == key => {
                // 同参数：沿用已有引擎（丢弃刚构造的、尚未启动的新引擎）；语言对可能变化
                current.src = fresh.src;
                current.tgt = fresh.tgt;
                (current.label.clone(), current.src, current.tgt)
            }
            _ => {
                let summary = (fresh.label.clone(), fresh.src, fresh.tgt);
                self.service.set_engine(Some(Arc::clone(&fresh.engine)));
                *built = Some(fresh);
                summary
            }
        };
        Ok((label, src, tgt))
    }

    /// 本地 worker 是否正在运行（探针用）。
    pub fn worker_running(&self) -> bool {
        self.built
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .as_ref()
            .and_then(|b| b.worker.as_ref().map(|w| w.is_running()))
            .unwrap_or(false)
    }

    /// 向本地 worker 取内存快照（探针用）。
    pub fn worker_memory(&self) -> Option<MemorySnapshot> {
        let worker = self.built.lock().unwrap_or_else(PoisonError::into_inner).as_ref()?.worker.clone()?;
        worker.memory_snapshot()
    }

    /// 本地 worker 累计拉起次数（探针用）。
    pub fn worker_launches(&self) -> u32 {
        self.built
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .as_ref()
            .and_then(|b| b.worker.as_ref().map(|w| w.launch_count()))
            .unwrap_or(0)
    }

    /// 结束 worker 并丢弃引擎（应用退出时调用）。
    pub fn shutdown(&self) {
        let taken = self.built.lock().unwrap_or_else(PoisonError::into_inner).take();
        if let Some(built) = taken
            && let Some(worker) = &built.worker
        {
            worker.shutdown();
        }
        self.service.set_engine(None);
    }
}

impl Translator for TranslateHost {
    /// 装配引擎后经缓存翻译。
    fn translate(&self, config: &TranslateConfig, texts: &[String]) -> Result<Translated, TranslateError> {
        let (label, src, tgt) = self.prepare(config)?;
        let texts = self.service.translate_batch(texts, src, tgt)?;
        Ok(Translated { texts, label })
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

/// 翻译流程的产出。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TranslateOutcome {
    /// 参与翻译的原文（按段落，行间 `\n`）。
    pub source: String,
    /// 译文（按段落，行间 `\n`）。
    pub translated: String,
    /// 原文与译文的逐段对照。
    pub pairs: Vec<(String, String)>,
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
    mut on_stage: impl FnMut(TranslateStage),
) -> Result<TranslateOutcome, TranslateFlowError> {
    on_stage(TranslateStage::Recognizing);
    let ocr = recognize().map_err(TranslateFlowError::Ocr)?;
    let paragraphs = paragraphs_from_boxes(&ocr.boxes, config.layout);
    if paragraphs.is_empty() {
        return Err(TranslateFlowError::NoText);
    }
    let joined = paragraphs.join("\n");
    if matches!(config.target, Lang::ZhHans | Lang::ZhHant) && is_mostly_cjk(&joined) {
        return Err(TranslateFlowError::AlreadyTarget(config.target));
    }
    on_stage(TranslateStage::Translating);
    let started = std::time::Instant::now();
    let translated = translator.translate(config, &paragraphs).map_err(TranslateFlowError::Translate)?;
    let translate_ms = started.elapsed().as_millis() as u64;
    let pairs: Vec<(String, String)> = paragraphs.iter().cloned().zip(translated.texts.iter().cloned()).collect();
    Ok(TranslateOutcome {
        source: joined,
        translated: translated.texts.join("\n"),
        pairs,
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
    use snow_ui::shell::geometry::PhysicalRect;

    /// 唯一临时目录。
    fn temp_root(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("snow-translate-host-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("建目录");
        dir
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

    /// 默认配置（本地后端，英→简中）。
    fn config() -> TranslateConfig {
        TranslateConfig::from_document(&ConfigDocument::from_bytes(None), "en-US")
    }

    /// 造一个带假 exe 与假 dll 的宿主（不会真的拉起进程）。
    fn host_with_files(root: &Path) -> (TranslateHost, PathBuf) {
        let exe = root.join("snow-translator.exe");
        let dll = root.join("onnxruntime.dll");
        std::fs::write(&exe, b"x").expect("写 exe");
        std::fs::write(&dll, b"x").expect("写 dll");
        let host = TranslateHost::with_env(root, Some(dll.to_string_lossy().into_owned()), Some(exe));
        (host, root.join("models").join("translate"))
    }

    /// 配置解析：全默认 → 本地后端、贪心以外的默认束宽 4、空闲 120 秒、目标语言随界面语言。
    #[test]
    fn config_defaults() {
        let cfg = config();
        assert_eq!(cfg.backend, Backend::Local);
        assert_eq!(cfg.beams, 4);
        assert_eq!(cfg.effective_beams(), 4);
        assert_eq!(cfg.idle, Duration::from_secs(120));
        assert_eq!(cfg.source, Lang::Auto);
        assert_eq!(cfg.target, Lang::En, "英文系统默认翻成英文");
        assert!(cfg.models_dir.is_none() && cfg.model_id.is_empty() && !cfg.low_memory);
        assert_eq!(cfg.layout, LayoutMode::SmartMerge);
        let zh = TranslateConfig::from_document(&ConfigDocument::from_bytes(None), "zh-CN");
        assert_eq!(zh.target, Lang::ZhHans);
    }

    /// 配置解析：读取各扩展项；低内存强制贪心；后端切换；自定义模型列表。
    #[test]
    fn config_reads_extension_keys() {
        let mut doc = ConfigDocument::from_bytes(None);
        doc.set_value(KEY_LOCAL_MODELS_DIR, serde_json::json!("D:/my models")).expect("目录");
        doc.set_value(KEY_LOCAL_MODEL_ID, serde_json::json!("opus")).expect("模型");
        doc.set_value(KEY_LOCAL_IDLE_SECONDS, serde_json::json!(30)).expect("空闲");
        doc.set_value(KEY_LOCAL_NUM_BEAMS, serde_json::json!(2)).expect("束宽");
        doc.set_value(KEY_TARGET_LANGUAGE, serde_json::json!("zh-Hant")).expect("目标");
        doc.set_value(KEY_SOURCE_LANGUAGE, serde_json::json!("en")).expect("源");
        doc.set_value(KEY_LAYOUT, serde_json::json!("original")).expect("版式");
        let cfg = TranslateConfig::from_document(&doc, "en-US");
        assert_eq!(cfg.models_dir, Some(PathBuf::from("D:/my models")));
        assert_eq!((cfg.model_id.as_str(), cfg.beams, cfg.idle), ("opus", 2, Duration::from_secs(30)));
        assert_eq!((cfg.source, cfg.target, cfg.layout), (Lang::En, Lang::ZhHant, LayoutMode::Original));
        doc.set_value(KEY_LOCAL_LOW_MEMORY, serde_json::json!(true)).expect("低内存");
        assert_eq!(TranslateConfig::from_document(&doc, "en-US").effective_beams(), 1);
        doc.set_value(KEY_TRANSLATION_BACKEND, serde_json::json!("openai")).expect("后端");
        assert_eq!(TranslateConfig::from_document(&doc, "en-US").backend, Backend::OpenAi);
    }

    /// 默认目标语言：简/繁中文界面对应中文，system 时用系统语言，其它为英文。
    #[test]
    fn default_target_rules() {
        assert_eq!(default_target("zh_CN", "en-US"), Lang::ZhHans);
        assert_eq!(default_target("zh_TW", "en-US"), Lang::ZhHant);
        assert_eq!(default_target("en_US", "zh-CN"), Lang::En);
        assert_eq!(default_target("system", "zh-TW"), Lang::ZhHant);
        assert_eq!(default_target("system", "zh-Hans-CN"), Lang::ZhHans);
        assert_eq!(default_target("system", "ja-JP"), Lang::En);
    }

    /// 模型根目录：配置为空用数据根下的默认位置。
    #[test]
    fn models_dir_resolution() {
        let host = TranslateHost::with_env(Path::new("D:/data"), None, None);
        let mut cfg = config();
        assert_eq!(host.models_dir(&cfg), Path::new("D:/data").join("models").join("translate"));
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
        let TranslateError::NoModelFound(message) = err else { panic!("应为 NoModelFound: {err:?}") };
        assert!(message.contains(&models.display().to_string()), "{message}");
        std::fs::create_dir_all(models.join("broken")).expect("建目录");
        std::fs::write(models.join("broken").join("model.json"), "{oops").expect("写坏清单");
        let TranslateError::NoModelFound(message) = host.prepare(&cfg).unwrap_err() else { panic!("类型") };
        assert!(message.contains("broken") && message.contains("解析失败"), "{message}");
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
        assert!(matches!(host.prepare(&cfg), Err(TranslateError::UnsupportedLanguagePair(_, Lang::Ja))));
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
        let exe = root.join("snow-translator.exe");
        std::fs::write(&exe, b"x").expect("写 exe");
        let no_runtime = TranslateHost::with_env(&root, None, Some(exe.clone()));
        let err = no_runtime.prepare(&cfg).unwrap_err();
        assert!(err.can_download_runtime(), "{err:?}");
        let bad_env = TranslateHost::with_env(&root, Some("Z:/nope/onnxruntime.dll".into()), Some(exe));
        let err = bad_env.prepare(&cfg).unwrap_err();
        assert!(matches!(err, TranslateError::WorkerUnavailable(_)) && !err.can_download_runtime());
        let dll = root.join("onnxruntime.dll");
        std::fs::write(&dll, b"x").expect("写 dll");
        let no_exe = TranslateHost::with_env(&root, Some(dll.to_string_lossy().into_owned()), Some(root.join("missing.exe")));
        assert!(matches!(no_exe.prepare(&cfg), Err(TranslateError::WorkerUnavailable(m)) if m.contains("不存在")));
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
        assert_eq!((label.as_str(), src, tgt), ("Model en-zh", Lang::En, Lang::ZhHans));
        let first = host.built.lock().unwrap().as_ref().map(|b| Arc::as_ptr(&b.engine) as *const () as usize);
        host.prepare(&cfg).expect("再次装配");
        let second = host.built.lock().unwrap().as_ref().map(|b| Arc::as_ptr(&b.engine) as *const () as usize);
        assert_eq!(first, second, "同参数应复用");
        cfg.beams = 3;
        host.prepare(&cfg).expect("换束宽");
        let third = host.built.lock().unwrap().as_ref().map(|b| Arc::as_ptr(&b.engine) as *const () as usize);
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
        assert!(matches!(host.prepare(&cfg), Err(TranslateError::NoModelFound(m)) if m.contains("自定义 AI 模型")));
        cfg.custom_models = vec![CustomAiModel {
            id: "11111111-1111-1111-1111-111111111111".into(),
            name: "本地 Ollama".into(),
            base_url: "http://localhost:11434/v1".into(),
            api_key: "k".into(),
            model: "qwen2.5".into(),
            supports_vision: false,
            supports_reasoning: false,
        }];
        assert!(matches!(host.prepare(&cfg), Err(TranslateError::NoModelFound(m)) if m.contains("选择")));
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
        fn translate(&self, _config: &TranslateConfig, texts: &[String]) -> Result<Translated, TranslateError> {
            self.seen.lock().unwrap().push(texts.to_vec());
            if let Some(e) = &self.fail {
                return Err(e.clone());
            }
            Ok(Translated { texts: texts.iter().map(|t| format!("译:{t}")).collect(), label: "fake".into() })
        }
    }

    /// 造 OCR 结果。
    fn ocr(lines: &[(&str, i32)]) -> OcrResult {
        let boxes: Vec<OcrTextBox> = lines
            .iter()
            .map(|(t, y)| OcrTextBox { rect: PhysicalRect::new(0, *y, 200, 20), text: (*t).to_string(), confidence: Some(0.9) })
            .collect();
        OcrResult { full_text: lines.iter().map(|l| l.0).collect::<Vec<_>>().join("\n"), boxes, elapsed_ms: 7 }
    }

    /// 流程：OCR → 合并段落 → 翻译，阶段回调按序触发，结果含逐段对照。
    #[test]
    fn flow_merges_translates_and_reports_stages() {
        let translator = FakeTranslator { seen: Mutex::new(Vec::new()), fail: None };
        let mut cfg = config();
        cfg.target = Lang::ZhHans;
        let mut stages = Vec::new();
        let outcome = run_flow(
            || Ok(ocr(&[("Hello there", 0), ("my friend", 22), ("Second para.", 100)])),
            &translator,
            &cfg,
            |s| stages.push(s),
        )
        .expect("流程");
        assert_eq!(stages, [TranslateStage::Recognizing, TranslateStage::Translating]);
        assert_eq!(translator.seen.lock().unwrap()[0], ["Hello there my friend", "Second para."]);
        assert_eq!(outcome.translated, "译:Hello there my friend\n译:Second para.");
        assert_eq!(outcome.pairs.len(), 2);
        assert_eq!((outcome.ocr_ms, outcome.label.as_str()), (7, "fake"));
    }

    /// 流程失败分支：OCR 缺资产、没识别到文字、原文已是中文（不调用翻译）、翻译失败。
    #[test]
    fn flow_failure_branches() {
        let translator = FakeTranslator { seen: Mutex::new(Vec::new()), fail: None };
        let mut cfg = config();
        cfg.target = Lang::ZhHans;
        let err = run_flow(
            || Err(OcrError::Unavailable(OcrUnavailable::NoRuntime)),
            &translator,
            &cfg,
            |_| {},
        )
        .unwrap_err();
        assert_eq!(err, TranslateFlowError::Ocr(OcrError::Unavailable(OcrUnavailable::NoRuntime)));
        assert_eq!(run_flow(|| Ok(ocr(&[("   ", 0)])), &translator, &cfg, |_| {}).unwrap_err(), TranslateFlowError::NoText);
        assert_eq!(
            run_flow(|| Ok(ocr(&[("今天天气很好，我们出去玩吧", 0)])), &translator, &cfg, |_| {}).unwrap_err(),
            TranslateFlowError::AlreadyTarget(Lang::ZhHans)
        );
        assert!(translator.seen.lock().unwrap().is_empty(), "以上三种都不应调用翻译");
        cfg.target = Lang::En;
        assert!(run_flow(|| Ok(ocr(&[("今天天气很好", 0)])), &translator, &cfg, |_| {}).is_ok(), "翻成英文时中文原文正常");
        let failing = FakeTranslator { seen: Mutex::new(Vec::new()), fail: Some(TranslateError::Timeout) };
        assert_eq!(
            run_flow(|| Ok(ocr(&[("Hello", 0)])), &failing, &cfg, |_| {}).unwrap_err(),
            TranslateFlowError::Translate(TranslateError::Timeout)
        );
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
            eprintln!("跳过探针：未设置 SNOW_TRANSLATOR_EXE / SNOW_ORT_DYLIB / SNOW_TRANSLATOR_TEST_MODEL_DIR");
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
        for (mode, low_memory, beams) in [("beam4", false, 4usize), ("greedy-low-memory", true, 1)] {
            let mut cfg = config();
            cfg.target = Lang::ZhHans;
            cfg.models_dir = model_dir.parent().map(Path::to_path_buf);
            cfg.model_id = String::new();
            cfg.idle = Duration::from_secs(3);
            cfg.beams = beams;
            cfg.low_memory = low_memory;
            let main_before = current_process_memory().map(|m| m.working_set).unwrap_or(0);
            let started = Instant::now();
            let first = host.translate(&cfg, &[sentences[0].to_string()]).expect("首句翻译");
            let cold_ms = started.elapsed().as_millis();
            let pid = host.worker_memory().map(|m| m.pid).unwrap_or(0);
            eprintln!("PROBE [{mode}] cold(启动+加载+首句)={cold_ms}ms 首句={:?} worker_pid={pid}", first.texts);
            let mut times = Vec::new();
            let mut peak = 0u64;
            for text in &sentences[1..] {
                let t = Instant::now();
                let out = host.translate(&cfg, &[text.to_string()]).expect("翻译");
                times.push(t.elapsed().as_millis());
                if let Some(m) = process_memory(pid) {
                    peak = peak.max(m.peak_working_set);
                }
                eprintln!("PROBE [{mode}] {}ms  {text} -> {}", times.last().copied().unwrap_or(0), out.texts[0]);
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
            assert!(!host.worker_running() && !alive, "空闲后 worker 进程必须已退出");
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
