//! 语音翻译级联：STT 定稿句异步送翻译，译文回到主线程与原文同屏显示。
//!
//! 分工：纯逻辑（语言对解析、可用性判断、按句译文跟踪）都在这里，可离屏单测；真正的翻译调用抽成
//! [`SentenceTranslator`]，生产实现包住 `TranslateHost`（自带结果缓存），测试用假实现。
//! 翻译跑在后台线程，绝不阻塞主线程 `tick`；只有定稿句（`Final`）才会翻译，未落定的 `Partial` 不翻译。
//! 首版不带上下文：每句独立翻译，`snow-translate` 的接口也不支持上下文。

use super::config::DictationConfig;
use super::text::sanitize;
use crate::stt_models::Dimension;
use crate::translate_service::{
    Backend, TranslateConfig, TranslateHost, Translator, default_models_dir, is_mostly_cjk,
};
use snow_config::extensions::{DICTATION_TARGET_EN, DICTATION_TARGET_ZH_HANS};
use snow_translate::{Lang, ModelScanner, ScanReport};
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Sender, channel};

/// 翻译线程名。
const WORKER_THREAD_NAME: &str = "snow-dictation-translate";

/// 译文目标语言偏好（对应配置 `dictation/translate_target`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TranslateTarget {
    /// 自动：中文译成英文，英文译成简体中文。
    Auto,
    /// 固定译成简体中文。
    ZhHans,
    /// 固定译成英文。
    En,
}

impl TranslateTarget {
    /// 由配置值解析；未知值按自动。
    ///
    /// # 参数
    /// - `value`：配置里的取值（`auto` / `zh-Hans` / `en`）。
    ///
    /// ```ignore
    /// assert_eq!(TranslateTarget::from_config("en"), TranslateTarget::En);
    /// ```
    pub fn from_config(value: &str) -> Self {
        match value {
            DICTATION_TARGET_ZH_HANS => Self::ZhHans,
            DICTATION_TARGET_EN => Self::En,
            _ => Self::Auto,
        }
    }
}

/// 本轮翻译不可用的原因（用于状态提示）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TranslateIssue {
    /// 没有找到任何可用的翻译模型。
    NoModel,
    /// 已装模型都不支持所需语言对。
    UnsupportedPair {
        /// 源语言。
        src: Lang,
        /// 目标语言。
        tgt: Lang,
    },
    /// 源语言与目标语言相同，无需翻译。
    SameLanguage,
}

/// 翻译可用性。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TranslationAvailability {
    /// 可以翻译。
    Ready,
    /// 开关关闭。
    Disabled,
    /// 没有找到任何翻译模型。
    NoModel,
    /// 已装模型不支持所需语言对（携带第一个缺失的方向）。
    UnsupportedPair {
        /// 源语言。
        src: Lang,
        /// 目标语言。
        tgt: Lang,
    },
    /// 源语言与目标语言相同，没有要翻译的方向。
    SameLanguage,
}

impl TranslationAvailability {
    /// 是否可以翻译。
    pub fn is_ready(&self) -> bool {
        matches!(self, Self::Ready)
    }

    /// 需要提示用户的原因；`Ready` 与 `Disabled` 没有。
    pub fn issue(&self) -> Option<TranslateIssue> {
        match self {
            Self::Ready | Self::Disabled => None,
            Self::NoModel => Some(TranslateIssue::NoModel),
            Self::UnsupportedPair { src, tgt } => Some(TranslateIssue::UnsupportedPair {
                src: *src,
                tgt: *tgt,
            }),
            Self::SameLanguage => Some(TranslateIssue::SameLanguage),
        }
    }
}

/// 已装翻译后端能做的事。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ModelSupport {
    /// 没有任何可用模型。
    Unavailable,
    /// 不限语言对（在线通道）。
    Any,
    /// 只支持这些有向语言对（本地模型）。
    Pairs(Vec<(Lang, Lang)>),
}

impl ModelSupport {
    /// 由扫描结果汇总本地模型支持的语言对。
    ///
    /// # 参数
    /// - `report`：模型目录扫描结果。
    pub fn from_scan(report: &ScanReport) -> Self {
        if report.models.is_empty() {
            return Self::Unavailable;
        }
        let mut pairs: Vec<(Lang, Lang)> = Vec::new();
        for model in &report.models {
            for pair in model.manifest.supported_pairs() {
                if !pairs.contains(&pair) {
                    pairs.push(pair);
                }
            }
        }
        Self::Pairs(pairs)
    }

    /// 按翻译配置汇总：本地后端用 `scan` 的结果，在线通道看是否选好了自定义模型。
    ///
    /// # 参数
    /// - `config`：翻译配置。
    /// - `scan`：扫描模型目录（仅本地后端才调用）。
    pub fn from_config(config: &TranslateConfig, scan: impl FnOnce() -> ScanReport) -> Self {
        match config.backend {
            Backend::Local => Self::from_scan(&scan()),
            Backend::OpenAi => {
                if config
                    .custom_models
                    .iter()
                    .any(|m| m.id == config.custom_model_id)
                {
                    Self::Any
                } else {
                    Self::Unavailable
                }
            }
        }
    }

    /// 是否支持某个有向语言对。
    fn supports(&self, pair: (Lang, Lang)) -> bool {
        match self {
            Self::Unavailable => false,
            Self::Any => true,
            Self::Pairs(pairs) => pairs.contains(&pair),
        }
    }
}

/// 依据源语言与目标偏好推出目标语言；源与目标相同返回 `None`。
fn target_for(src: Lang, target: TranslateTarget) -> Option<Lang> {
    let tgt = match target {
        TranslateTarget::Auto if src == Lang::ZhHans => Lang::En,
        TranslateTarget::Auto => Lang::ZhHans,
        TranslateTarget::ZhHans => Lang::ZhHans,
        TranslateTarget::En => Lang::En,
    };
    (tgt != src).then_some(tgt)
}

/// 解析一句话的翻译方向。
///
/// 中文 / 英文维度直接定源语言；中英混合逐句按汉字占比判断。源与目标相同返回 `None`。
///
/// # 参数
/// - `dimension`：识别的语言维度。
/// - `target`：目标语言偏好。
/// - `text`：这一句的文本。
///
/// ```ignore
/// assert_eq!(resolve_pair(Dimension::Bilingual, TranslateTarget::Auto, "你好"), Some((Lang::ZhHans, Lang::En)));
/// ```
pub fn resolve_pair(
    dimension: Dimension,
    target: TranslateTarget,
    text: &str,
) -> Option<(Lang, Lang)> {
    let src = match dimension {
        Dimension::Zh => Lang::ZhHans,
        Dimension::En => Lang::En,
        Dimension::Bilingual if is_mostly_cjk(text) => Lang::ZhHans,
        Dimension::Bilingual => Lang::En,
    };
    target_for(src, target).map(|tgt| (src, tgt))
}

/// 该维度与目标偏好可能用到的全部方向。
fn needed_pairs(dimension: Dimension, target: TranslateTarget) -> Vec<(Lang, Lang)> {
    let sources: &[Lang] = match dimension {
        Dimension::Zh => &[Lang::ZhHans],
        Dimension::En => &[Lang::En],
        Dimension::Bilingual => &[Lang::ZhHans, Lang::En],
    };
    sources
        .iter()
        .filter_map(|src| target_for(*src, target).map(|tgt| (*src, tgt)))
        .collect()
}

/// 判断翻译可用性（纯函数）。
///
/// 需要的方向里只要有一个被支持就算可用；本轮不被支持的方向的句子只显示原文。
///
/// # 参数
/// - `config`：听写配置。
/// - `support`：翻译后端能力。
pub fn assess(config: &DictationConfig, support: &ModelSupport) -> TranslationAvailability {
    if !config.translate_enabled {
        return TranslationAvailability::Disabled;
    }
    let needed = needed_pairs(config.dimension, config.translate_target);
    let Some(first) = needed.first().copied() else {
        return TranslationAvailability::SameLanguage;
    };
    if matches!(support, ModelSupport::Unavailable) {
        return TranslationAvailability::NoModel;
    }
    if needed.iter().any(|pair| support.supports(*pair)) {
        TranslationAvailability::Ready
    } else {
        TranslationAvailability::UnsupportedPair {
            src: first.0,
            tgt: first.1,
        }
    }
}

/// 翻译可用性（默认模型目录、本地后端）。
///
/// # 参数
/// - `config`：听写配置。
/// - `data_root`：应用数据根目录。
///
/// # 返回
/// 可用性；开关关闭时不扫描目录，直接 `Disabled`。
///
/// ```ignore
/// let ok = translation_availability(&config, &data_root).is_ready();
/// ```
pub fn translation_availability(
    config: &DictationConfig,
    data_root: &Path,
) -> TranslationAvailability {
    if !config.translate_enabled {
        return TranslationAvailability::Disabled;
    }
    let report = ModelScanner::new(&default_models_dir(data_root)).scan();
    assess(config, &ModelSupport::from_scan(&report))
}

/// 一句话的翻译状态。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TranslationState {
    /// 已送出，等待结果。
    Pending,
    /// 译文。
    Done(String),
    /// 翻译失败。
    Failed,
}

/// 翻译结果（经收件箱回主线程）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TranslationOutcome {
    /// 译文。
    Done(String),
    /// 失败。
    Failed,
}

/// 单句翻译器。
pub trait SentenceTranslator: Send + Sync {
    /// 翻译一句话（阻塞，只在后台线程调用）。
    ///
    /// # 参数
    /// - `text`：原文。
    /// - `src` / `tgt`：源 / 目标语言。
    ///
    /// # 返回
    /// 译文；失败返回原因说明。
    fn translate(&self, text: &str, src: Lang, tgt: Lang) -> Result<String, String>;
}

/// 生产实现：包住应用共享的 `TranslateHost`，沿用用户的翻译设置，只覆盖语言对。
pub struct HostTranslator {
    /// 翻译宿主（含结果缓存）。
    host: Arc<TranslateHost>,
    /// 本轮翻译配置快照。
    base: TranslateConfig,
}

impl HostTranslator {
    /// 创建。
    ///
    /// # 参数
    /// - `host`：翻译宿主。
    /// - `base`：翻译配置（语言对会被逐句覆盖）。
    pub fn new(host: Arc<TranslateHost>, base: TranslateConfig) -> Self {
        Self { host, base }
    }
}

impl SentenceTranslator for HostTranslator {
    /// 经宿主翻译单句。
    fn translate(&self, text: &str, src: Lang, tgt: Lang) -> Result<String, String> {
        let mut config = self.base.clone();
        config.source = src;
        config.target = tgt;
        let translated = self
            .host
            .translate(&config, &[text.to_string()])
            .map_err(|e| format!("{e:?}"))?;
        translated
            .texts
            .into_iter()
            .next()
            .ok_or_else(|| "empty result".to_string())
    }
}

/// 送往翻译线程的任务。
struct Job {
    /// 轮次。
    round: u64,
    /// 句序号。
    seq: usize,
    /// 原文。
    text: String,
    /// 源语言。
    src: Lang,
    /// 目标语言。
    tgt: Lang,
}

/// 结果回调：`(轮次, 句序号, 结果)`，在翻译线程里调用。
pub type OnTranslated = Arc<dyn Fn(u64, usize, TranslationOutcome) + Send + Sync>;

/// 后台翻译线程：按提交顺序逐句翻译；丢弃本对象即收尾（线程做完手上的一句后退出）。
pub struct TranslateWorker {
    /// 任务通道；丢弃后线程退出。
    tx: Sender<Job>,
    /// 线程句柄（仅测试等待用，平时分离）。
    #[cfg(test)]
    handle: Option<std::thread::JoinHandle<()>>,
}

impl TranslateWorker {
    /// 启动线程。
    ///
    /// # 参数
    /// - `translator`：单句翻译器。
    /// - `on_done`：结果回调。
    ///
    /// # 返回
    /// 线程创建失败返回 `None`（已记日志）。
    pub fn spawn(translator: Arc<dyn SentenceTranslator>, on_done: OnTranslated) -> Option<Self> {
        let (tx, rx) = channel::<Job>();
        let warned = AtomicBool::new(false);
        let spawned = std::thread::Builder::new()
            .name(WORKER_THREAD_NAME.into())
            .spawn(move || {
                for job in rx {
                    let outcome = match translator.translate(&job.text, job.src, job.tgt) {
                        Ok(text) if !text.trim().is_empty() => TranslationOutcome::Done(text),
                        other => {
                            // 只在第一次失败时写 warn，避免整轮刷屏
                            let reason = other.err().unwrap_or_else(|| "empty result".into());
                            if warned.swap(true, Ordering::Relaxed) {
                                tracing::debug!(%reason, "语音翻译失败");
                            } else {
                                tracing::warn!(%reason, "语音翻译失败，该句只显示原文");
                            }
                            TranslationOutcome::Failed
                        }
                    };
                    on_done(job.round, job.seq, outcome);
                }
            });
        match spawned {
            Ok(_handle) => Some(Self {
                tx,
                #[cfg(test)]
                handle: Some(_handle),
            }),
            Err(e) => {
                tracing::error!(error = %e, "无法创建语音翻译线程，本轮不翻译");
                None
            }
        }
    }

    /// 提交一句；线程已退出返回 `false`。
    fn submit(&self, job: Job) -> bool {
        self.tx.send(job).is_ok()
    }

    /// 关闭任务通道并等待线程结束（测试用）。
    #[cfg(test)]
    fn finish(mut self) {
        let handle = self.handle.take();
        drop(self);
        if let Some(handle) = handle {
            let _ = handle.join();
        }
    }
}

/// 本轮的翻译计划。
struct RoundPlan {
    /// 语言维度。
    dimension: Dimension,
    /// 目标偏好。
    target: TranslateTarget,
    /// 后端支持的方向。
    support: ModelSupport,
}

impl RoundPlan {
    /// 这一句要翻译的方向；方向相同或后端不支持返回 `None`。
    fn pair_for(&self, text: &str) -> Option<(Lang, Lang)> {
        resolve_pair(self.dimension, self.target, text).filter(|pair| self.support.supports(*pair))
    }
}

/// 一轮的按句译文跟踪：登记句序号、派发翻译、按 `(轮次, 序号)` 接收结果。
///
/// 序号规则与浮窗模型一致：清洗后非空的定稿句依次编号，译文乱序到达也按序号对位。
pub struct TranslationTracker {
    /// 当前轮次。
    round: u64,
    /// 按句译文状态；`None` 表示该句没有翻译。
    entries: Vec<Option<TranslationState>>,
    /// 翻译计划与线程；本轮不翻译时为 `None`。
    active: Option<(RoundPlan, TranslateWorker)>,
    /// 本轮不翻译的原因。
    issue: Option<TranslateIssue>,
}

impl Default for TranslationTracker {
    /// 空跟踪器（不翻译）。
    fn default() -> Self {
        Self {
            round: 0,
            entries: Vec::new(),
            active: None,
            issue: None,
        }
    }
}

impl TranslationTracker {
    /// 开始新一轮：清空旧状态并丢弃旧线程（旧轮迟到的结果会被轮次比较丢弃）。
    ///
    /// # 参数
    /// - `round`：新轮次编号。
    /// - `config`：听写配置。
    /// - `support`：翻译后端能力（本轮开始时算一次）。
    /// - `translator`：单句翻译器。
    /// - `on_done`：结果回调。
    ///
    /// # 返回
    /// 本轮的可用性；不可用时 [`TranslationTracker::issue`] 给出原因。
    pub fn begin(
        &mut self,
        round: u64,
        config: &DictationConfig,
        support: ModelSupport,
        translator: Arc<dyn SentenceTranslator>,
        on_done: OnTranslated,
    ) -> TranslationAvailability {
        self.reset(round);
        let availability = assess(config, &support);
        self.issue = availability.issue();
        if availability.is_ready()
            && let Some(worker) = TranslateWorker::spawn(translator, on_done)
        {
            self.active = Some((
                RoundPlan {
                    dimension: config.dimension,
                    target: config.translate_target,
                    support,
                },
                worker,
            ));
        }
        availability
    }

    /// 清空并进入新一轮（不翻译）。
    ///
    /// # 参数
    /// - `round`：新轮次编号。
    pub fn reset(&mut self, round: u64) {
        self.round = round;
        self.entries.clear();
        self.active = None;
        self.issue = None;
    }

    /// 本轮不翻译的原因（开关关闭或可用时为 `None`）。
    pub fn issue(&self) -> Option<&TranslateIssue> {
        self.issue.as_ref()
    }

    /// 各句译文状态（与定稿句序对位）。
    pub fn entries(&self) -> &[Option<TranslationState>] {
        &self.entries
    }

    /// 登记一句定稿但不翻译（如收尾并入的残余文字）。
    ///
    /// # 返回
    /// 句序号；清洗后为空不登记，返回 `None`。
    pub fn register(&mut self, text: &str) -> Option<usize> {
        if sanitize(text).is_empty() {
            return None;
        }
        self.entries.push(None);
        Some(self.entries.len() - 1)
    }

    /// 登记一句定稿并按需送翻译。
    ///
    /// # 参数
    /// - `text`：定稿原文。
    ///
    /// # 返回
    /// `(句序号, 是否已送翻译)`；清洗后为空返回 `None`。
    pub fn on_final(&mut self, text: &str) -> Option<(usize, bool)> {
        let seq = self.register(text)?;
        let clean = sanitize(text);
        let sent = self.active.as_ref().is_some_and(|(plan, worker)| {
            plan.pair_for(&clean).is_some_and(|(src, tgt)| {
                worker.submit(Job {
                    round: self.round,
                    seq,
                    text: clean,
                    src,
                    tgt,
                })
            })
        });
        if sent {
            self.entries[seq] = Some(TranslationState::Pending);
        }
        Some((seq, sent))
    }

    /// 接收翻译结果：轮次不符或序号越界的丢弃。
    ///
    /// # 参数
    /// - `round`：结果所属轮次。
    /// - `seq`：句序号。
    /// - `outcome`：结果。
    ///
    /// # 返回
    /// 更新后的 `(序号, 状态)`；被丢弃返回 `None`。
    pub fn on_result(
        &mut self,
        round: u64,
        seq: usize,
        outcome: TranslationOutcome,
    ) -> Option<(usize, TranslationState)> {
        if round != self.round {
            return None;
        }
        let slot = self.entries.get_mut(seq)?;
        let state = match outcome {
            TranslationOutcome::Done(text) => TranslationState::Done(text),
            TranslationOutcome::Failed => TranslationState::Failed,
        };
        *slot = Some(state.clone());
        Some((seq, state))
    }

    /// 结束翻译线程（应用退出时用；丢弃线程通道，线程做完手上一句即退出）。
    pub fn shutdown(&mut self) {
        self.active = None;
    }

    /// 等待翻译线程收尾（测试用）。
    #[cfg(test)]
    fn finish_worker(&mut self) {
        if let Some((_, worker)) = self.active.take() {
            worker.finish();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use snow_config::document::ConfigDocument;
    use std::sync::Mutex;

    /// 假翻译器：记录调用，译文为 `[tgt]原文`，含 `FAIL` 的句子失败。
    #[derive(Default)]
    struct Fake {
        /// 调用记录。
        calls: Mutex<Vec<(String, Lang, Lang)>>,
    }

    impl SentenceTranslator for Fake {
        /// 记录并返回假译文。
        fn translate(&self, text: &str, src: Lang, tgt: Lang) -> Result<String, String> {
            self.calls
                .lock()
                .unwrap()
                .push((text.to_string(), src, tgt));
            if text.contains("FAIL") {
                Err("boom".into())
            } else {
                Ok(format!("[{}]{text}", tgt.code()))
            }
        }
    }

    /// 造配置。
    fn config(enabled: bool, dimension: Dimension, target: TranslateTarget) -> DictationConfig {
        let mut c = DictationConfig::from_document(&ConfigDocument::from_bytes(None));
        c.translate_enabled = enabled;
        c.dimension = dimension;
        c.translate_target = target;
        c
    }

    /// 全方向可用。
    fn any() -> ModelSupport {
        ModelSupport::Any
    }

    /// 结果收集器。
    type Collected = Arc<Mutex<Vec<(u64, usize, TranslationOutcome)>>>;

    /// 开一轮并返回收集结果的缓冲。
    fn start(
        tracker: &mut TranslationTracker,
        round: u64,
        cfg: &DictationConfig,
        support: ModelSupport,
        fake: Arc<Fake>,
    ) -> (TranslationAvailability, Collected) {
        let out: Collected = Arc::default();
        let sink = Arc::clone(&out);
        let availability = tracker.begin(
            round,
            cfg,
            support,
            fake,
            Arc::new(move |r, s, o| sink.lock().unwrap().push((r, s, o))),
        );
        (availability, out)
    }

    /// 语言对解析：auto 各分支、显式目标、同语言跳过、混合逐句判断。
    #[test]
    fn pair_resolution_rules() {
        use Dimension::{Bilingual, En as DEn, Zh};
        use TranslateTarget::{Auto, En as TEn, ZhHans};
        let zh = Lang::ZhHans;
        let en = Lang::En;
        assert_eq!(resolve_pair(Zh, Auto, "你好"), Some((zh, en)));
        assert_eq!(resolve_pair(DEn, Auto, "hello"), Some((en, zh)));
        assert_eq!(resolve_pair(Zh, ZhHans, "你好"), None);
        assert_eq!(resolve_pair(DEn, TEn, "hello"), None);
        assert_eq!(resolve_pair(Zh, TEn, "你好"), Some((zh, en)));
        assert_eq!(resolve_pair(DEn, ZhHans, "hello"), Some((en, zh)));
        assert_eq!(
            resolve_pair(Bilingual, Auto, "今天天气不错"),
            Some((zh, en))
        );
        assert_eq!(
            resolve_pair(Bilingual, Auto, "nice weather"),
            Some((en, zh))
        );
        assert_eq!(resolve_pair(Bilingual, ZhHans, "今天天气不错"), None);
        assert_eq!(
            resolve_pair(Bilingual, ZhHans, "nice weather"),
            Some((en, zh))
        );
        assert_eq!(TranslateTarget::from_config("en"), TEn);
        assert_eq!(TranslateTarget::from_config("zh-Hans"), ZhHans);
        assert_eq!(TranslateTarget::from_config("x"), Auto);
    }

    /// 可用性各分支。
    #[test]
    fn availability_branches() {
        use Dimension::*;
        let cfg = |en, d, t| config(en, d, t);
        let auto = TranslateTarget::Auto;
        assert_eq!(
            assess(&cfg(false, Zh, auto), &any()),
            TranslationAvailability::Disabled
        );
        assert_eq!(
            assess(&cfg(true, Zh, auto), &ModelSupport::Unavailable),
            TranslationAvailability::NoModel
        );
        assert_eq!(
            assess(&cfg(true, Zh, TranslateTarget::ZhHans), &any()),
            TranslationAvailability::SameLanguage
        );
        let only_en_zh = ModelSupport::Pairs(vec![(Lang::En, Lang::ZhHans)]);
        assert_eq!(
            assess(&cfg(true, Zh, auto), &only_en_zh),
            TranslationAvailability::UnsupportedPair {
                src: Lang::ZhHans,
                tgt: Lang::En
            }
        );
        assert!(assess(&cfg(true, En, auto), &only_en_zh).is_ready());
        // 混合：只要有一个方向被支持就可用
        assert!(assess(&cfg(true, Bilingual, auto), &only_en_zh).is_ready());
        assert_eq!(
            assess(&cfg(true, Bilingual, auto), &ModelSupport::Pairs(vec![])),
            TranslationAvailability::UnsupportedPair {
                src: Lang::ZhHans,
                tgt: Lang::En
            }
        );
        assert_eq!(
            assess(&cfg(true, Zh, auto), &ModelSupport::Unavailable).issue(),
            Some(TranslateIssue::NoModel)
        );
        assert!(TranslationAvailability::Disabled.issue().is_none());
    }

    /// 空目录（没有模型）与关闭开关的对外入口。
    #[test]
    fn availability_entry_with_empty_dir() {
        let root = std::env::temp_dir().join(format!("snow-dict-tr-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&root);
        let on = config(true, Dimension::Zh, TranslateTarget::Auto);
        assert_eq!(
            translation_availability(&on, &root),
            TranslationAvailability::NoModel
        );
        let off = config(false, Dimension::Zh, TranslateTarget::Auto);
        assert_eq!(
            translation_availability(&off, &root),
            TranslationAvailability::Disabled
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    /// 只有定稿句触发翻译；译文按序号回填。
    #[test]
    fn final_triggers_translation() {
        let fake = Arc::new(Fake::default());
        let mut t = TranslationTracker::default();
        let cfg = config(true, Dimension::Bilingual, TranslateTarget::Auto);
        let (av, out) = start(&mut t, 1, &cfg, any(), Arc::clone(&fake));
        assert!(av.is_ready());
        assert_eq!(t.on_final("你好"), Some((0, true)));
        assert_eq!(t.on_final("hello"), Some((1, true)));
        assert_eq!(t.entries()[0], Some(TranslationState::Pending));
        t.finish_worker();
        let calls = fake.calls.lock().unwrap().clone();
        assert_eq!(
            calls,
            vec![
                ("你好".into(), Lang::ZhHans, Lang::En),
                ("hello".into(), Lang::En, Lang::ZhHans)
            ]
        );
        let results = out.lock().unwrap().clone();
        assert_eq!(results.len(), 2);
        let (r, s, o) = results[0].clone();
        assert_eq!(
            t.on_result(r, s, o),
            Some((0, TranslationState::Done("[en]你好".into())))
        );
    }

    /// 开关关闭不翻译，但仍登记句序号。
    #[test]
    fn disabled_does_not_translate() {
        let fake = Arc::new(Fake::default());
        let mut t = TranslationTracker::default();
        let cfg = config(false, Dimension::Zh, TranslateTarget::Auto);
        let (av, _) = start(&mut t, 1, &cfg, any(), Arc::clone(&fake));
        assert_eq!(av, TranslationAvailability::Disabled);
        assert_eq!(t.on_final("你好"), Some((0, false)));
        assert_eq!(t.entries(), &[None]);
        assert!(fake.calls.lock().unwrap().is_empty());
        assert!(t.issue().is_none());
    }

    /// 没模型 / 语言对不支持：不翻译，并给出原因。
    #[test]
    fn unavailable_skips_with_issue() {
        let fake = Arc::new(Fake::default());
        let mut t = TranslationTracker::default();
        let cfg = config(true, Dimension::Zh, TranslateTarget::Auto);
        let (av, _) = start(
            &mut t,
            1,
            &cfg,
            ModelSupport::Unavailable,
            Arc::clone(&fake),
        );
        assert_eq!(av, TranslationAvailability::NoModel);
        assert_eq!(t.issue(), Some(&TranslateIssue::NoModel));
        assert_eq!(t.on_final("你好"), Some((0, false)));
        let unsupported = ModelSupport::Pairs(vec![(Lang::En, Lang::ZhHans)]);
        let (av, _) = start(&mut t, 2, &cfg, unsupported, Arc::clone(&fake));
        assert!(matches!(
            av,
            TranslationAvailability::UnsupportedPair { .. }
        ));
        assert_eq!(t.on_final("你好"), Some((0, false)));
        assert!(fake.calls.lock().unwrap().is_empty());
    }

    /// 同语言不翻译；混合维度下被支持的方向照常翻译、不支持的方向跳过。
    #[test]
    fn same_language_and_unsupported_direction_skip() {
        let fake = Arc::new(Fake::default());
        let mut t = TranslationTracker::default();
        let cfg = config(true, Dimension::Bilingual, TranslateTarget::Auto);
        let only_en_zh = ModelSupport::Pairs(vec![(Lang::En, Lang::ZhHans)]);
        start(&mut t, 1, &cfg, only_en_zh, Arc::clone(&fake));
        assert_eq!(t.on_final("你好"), Some((0, false)));
        assert_eq!(t.on_final("hi there"), Some((1, true)));
        t.finish_worker();
        assert_eq!(fake.calls.lock().unwrap().len(), 1);
    }

    /// 旧轮结果被丢弃；乱序到达按序号对位；越界序号被丢弃。
    #[test]
    fn stale_rounds_dropped_and_out_of_order_aligned() {
        let fake = Arc::new(Fake::default());
        let mut t = TranslationTracker::default();
        let cfg = config(true, Dimension::En, TranslateTarget::Auto);
        start(&mut t, 1, &cfg, any(), Arc::clone(&fake));
        t.on_final("one");
        t.on_final("two");
        t.on_final("three");
        assert_eq!(
            t.on_result(1, 2, TranslationOutcome::Done("三".into())),
            Some((2, TranslationState::Done("三".into())))
        );
        assert_eq!(t.entries()[0], Some(TranslationState::Pending));
        assert_eq!(
            t.on_result(1, 0, TranslationOutcome::Done("一".into())),
            Some((0, TranslationState::Done("一".into())))
        );
        assert!(t.on_result(1, 9, TranslationOutcome::Failed).is_none());
        t.finish_worker();
        // 新一轮开始后，旧轮结果迟到
        start(&mut t, 2, &cfg, any(), Arc::clone(&fake));
        t.on_final("fresh");
        assert!(
            t.on_result(1, 0, TranslationOutcome::Done("旧".into()))
                .is_none()
        );
        assert_eq!(t.entries()[0], Some(TranslationState::Pending));
    }

    /// 翻译失败只标记该句，其余句子照常。
    #[test]
    fn failure_marks_only_that_sentence() {
        let fake = Arc::new(Fake::default());
        let mut t = TranslationTracker::default();
        let cfg = config(true, Dimension::En, TranslateTarget::Auto);
        let (_, out) = start(&mut t, 1, &cfg, any(), Arc::clone(&fake));
        t.on_final("FAIL this");
        t.on_final("fine");
        t.finish_worker();
        for (r, s, o) in out.lock().unwrap().clone() {
            t.on_result(r, s, o);
        }
        assert_eq!(t.entries()[0], Some(TranslationState::Failed));
        assert!(matches!(t.entries()[1], Some(TranslationState::Done(_))));
    }

    /// 空句不登记；收尾并入的残余文字登记但不翻译。
    #[test]
    fn empty_not_registered_and_leftover_untranslated() {
        let fake = Arc::new(Fake::default());
        let mut t = TranslationTracker::default();
        let cfg = config(true, Dimension::En, TranslateTarget::Auto);
        start(&mut t, 1, &cfg, any(), Arc::clone(&fake));
        assert_eq!(t.on_final("  \n "), None);
        assert_eq!(t.register("tail"), Some(0));
        assert_eq!(t.entries(), &[None]);
        t.finish_worker();
        assert!(fake.calls.lock().unwrap().is_empty());
    }

    /// 关闭线程后提交不会崩，且收尾可重复。
    #[test]
    fn shutdown_is_safe() {
        let fake = Arc::new(Fake::default());
        let mut t = TranslationTracker::default();
        let cfg = config(true, Dimension::En, TranslateTarget::Auto);
        start(&mut t, 1, &cfg, any(), fake);
        t.shutdown();
        t.shutdown();
        assert_eq!(t.on_final("late"), Some((0, false)));
    }
}
