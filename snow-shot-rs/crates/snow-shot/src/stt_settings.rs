//! 语音转文字设置页的纯逻辑：模型下拉选项、联动与置灰条件、状态文案、下载面板模型。
//!
//! 不依赖 GPUI，视图层只负责把这里的结果画出来；文案全部来自 i18n 语料。

use crate::dictation::config::DictationConfig;
use crate::dictation::status::issue_message;
use crate::dictation::translate::{ModelSupport, assess, resolve_pair};
use crate::ocr_backend::i18n_for;
use crate::stt_download::{Progress, Stage};
use crate::stt_models::{self, Dimension, Role, SttModelSpec, mode_from_config};
use crate::translate_service::{TranslateConfig, default_models_dir};
use serde_json::Value;
use snow_config::extensions::{
    DICTATION_BACKEND_SYSTEM, KEY_DICTATION_BACKEND, KEY_DICTATION_LANGUAGE_DIMENSION,
    KEY_DICTATION_MODEL_DIR, KEY_DICTATION_MODEL_ID, KEY_DICTATION_RECOGNITION_MODE,
    KEY_DICTATION_TRANSLATE_ENABLED, KEY_DICTATION_TRANSLATE_TARGET, KEY_LOCAL_MODEL_ID,
    KEY_LOCAL_MODELS_DIR, KEY_TRANSLATION_BACKEND,
};
use snow_i18n::Args;
use snow_stt_protocol::{ModelKind, RecognitionMode};
use snow_translate::{Lang, ModelScanner};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

/// 默认模型在下拉里对应的配置值（空串表示跟随默认）。
pub const DEFAULT_OPTION_VALUE: &str = "";
/// 语音转文字所在的设置分组 id。
pub const DICTATION_GROUP_ID: &str = "dictation";
/// 中文样句（探测中英混合维度下的中文方向）。
const SAMPLE_ZH: &str = "你";
/// 英文样句（探测中英混合维度下的英文方向）。
const SAMPLE_EN: &str = "a";
/// 多个语言方向之间的分隔。
const PAIR_SEPARATOR: &str = " / ";
/// 字节换算成 MB 的除数。
const MIB: u64 = 1024 * 1024;
/// 模型名称 message id 的前缀（后接模型 ID）。
const MODEL_NAME_PREFIX: &str = "stt-model-";

/// 可跨线程共享、可比较的取消标记（便于放进 `UiEvent`）。
#[derive(Debug, Clone, Default)]
pub struct CancelFlag(pub Arc<AtomicBool>);

impl CancelFlag {
    /// 置位取消。
    pub fn cancel(&self) {
        self.0.store(true, Ordering::Relaxed);
    }

    /// 是否已被取消。
    pub fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::Relaxed)
    }
}

impl PartialEq for CancelFlag {
    /// 指向同一个标记才算相等。
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
}

/// 设置页向上层请求下载的入口与数据根目录。
#[derive(Clone)]
pub struct SttHooks {
    /// 应用数据根目录（用于判断安装状态与落盘）。
    pub data_root: PathBuf,
    /// 请求下载：参数为模型 ID 与取消标记，由上层起后台线程。
    pub request: Arc<dyn Fn(String, CancelFlag)>,
}

/// 三项选择被置灰的原因。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LockReason {
    /// 引擎为系统语音，不使用这些模型。
    SystemBackend,
    /// 已指定手动模型目录。
    ManualDir,
}

/// 设置页读到的语音转文字相关配置值。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SttInputs {
    /// 引擎是否为系统语音。
    pub system_backend: bool,
    /// 手动模型目录是否非空。
    pub manual_dir: bool,
    /// 语言维度。
    pub dimension: Dimension,
    /// 识别模式。
    pub mode: RecognitionMode,
    /// 配置里的模型 ID（空串为默认）。
    pub model_id: String,
}

/// 受「切换后需清空模型 ID」影响的键。
pub fn resets_model_id(key: &str) -> bool {
    key == KEY_DICTATION_RECOGNITION_MODE || key == KEY_DICTATION_LANGUAGE_DIMENSION
}

/// 写入后会改变本分组条目显隐的键。
pub fn affects_layout(key: &str) -> bool {
    resets_model_id(key)
        || key == KEY_DICTATION_MODEL_ID
        || key == KEY_DICTATION_MODEL_DIR
        || key == KEY_DICTATION_BACKEND
}

/// 模式 / 维度 / 模型三项是否受后端与手动目录联动影响。
pub fn is_selector_key(key: &str) -> bool {
    resets_model_id(key) || key == KEY_DICTATION_MODEL_ID
}

impl SttInputs {
    /// 从配置取值函数构建。
    ///
    /// # 参数
    /// - `get`：按键取配置值。
    ///
    /// ```ignore
    /// let inputs = SttInputs::from_lookup(|key| store.value(key));
    /// ```
    pub fn from_lookup(get: impl Fn(&str) -> Value) -> Self {
        let text = |key: &str| get(key).as_str().unwrap_or_default().trim().to_string();
        Self {
            system_backend: text(KEY_DICTATION_BACKEND) == DICTATION_BACKEND_SYSTEM,
            manual_dir: !text(KEY_DICTATION_MODEL_DIR).is_empty(),
            dimension: Dimension::from_config(&text(KEY_DICTATION_LANGUAGE_DIMENSION)),
            mode: mode_from_config(&text(KEY_DICTATION_RECOGNITION_MODE)),
            model_id: text(KEY_DICTATION_MODEL_ID),
        }
    }

    /// 三项选择置灰的原因；可用时为 `None`。系统语音优先于手动目录。
    pub fn lock_reason(&self) -> Option<LockReason> {
        if self.system_backend {
            Some(LockReason::SystemBackend)
        } else if self.manual_dir {
            Some(LockReason::ManualDir)
        } else {
            None
        }
    }

    /// 手动目录与离线模式的无效组合（本地后端下）。
    pub fn manual_dir_conflict(&self) -> bool {
        !self.system_backend && self.manual_dir && self.mode == RecognitionMode::Offline
    }

    /// 当前维度与模式下的候选模型（默认在前）。
    pub fn options(&self) -> Vec<&'static SttModelSpec> {
        stt_models::list_for(self.dimension, self.mode)
    }

    /// 当前实际选中的模型（配置里的 ID 失效时回落默认）；清单无候选为 `None`。
    pub fn selected(&self) -> Option<&'static SttModelSpec> {
        stt_models::resolve(self.dimension, self.mode, &self.model_id).ok()
    }

    /// 下拉当前应选中的值。
    pub fn selected_value(&self) -> Option<&'static str> {
        self.selected().map(option_value)
    }

    /// 是否显示 SenseVoice 的 itn 开关：未被联动置灰且选中的是 SenseVoice。
    pub fn itn_visible(&self) -> bool {
        self.lock_reason().is_none()
            && self
                .selected()
                .is_some_and(|spec| spec.kind == ModelKind::OfflineSenseVoice)
    }
}

/// 翻译提示行挂在哪个配置键上（目标语言行，位于开关下方）。
pub fn is_translate_note_key(key: &str) -> bool {
    key == KEY_DICTATION_TRANSLATE_TARGET
}

/// 写入该键后是否需要重新扫描翻译模型（开关、翻译后端、模型目录、自定义模型）。
pub fn rescans_translate_support(key: &str) -> bool {
    matches!(
        key,
        KEY_DICTATION_TRANSLATE_ENABLED
            | KEY_TRANSLATION_BACKEND
            | KEY_LOCAL_MODELS_DIR
            | KEY_LOCAL_MODEL_ID
            | crate::translate_service::KEY_CUSTOM_MODEL
            | crate::translate_service::KEY_CUSTOM_MODELS
    )
}

/// 扫描磁盘得到翻译后端能力（访问磁盘，调用方负责缓存）。
///
/// # 参数
/// - `config`：翻译配置（后端与模型目录）。
/// - `data_root`：应用数据根目录（模型目录为默认值时用）。
///
/// ```ignore
/// let support = scan_translate_support(&tcfg, &data_root);
/// ```
pub fn scan_translate_support(config: &TranslateConfig, data_root: &Path) -> ModelSupport {
    let dir = config
        .models_dir
        .clone()
        .unwrap_or_else(|| default_models_dir(data_root));
    ModelSupport::from_config(config, || ModelScanner::new(&dir).scan())
}

/// 当前维度与目标偏好下会用到的翻译方向（去重，中文在前）。
///
/// # 参数
/// - `config`：听写配置（取维度与目标）。
pub fn translate_preview_pairs(config: &DictationConfig) -> Vec<(Lang, Lang)> {
    let mut pairs: Vec<(Lang, Lang)> = Vec::new();
    for sample in [SAMPLE_ZH, SAMPLE_EN] {
        if let Some(pair) = resolve_pair(config.dimension, config.translate_target, sample)
            && !pairs.contains(&pair)
        {
            pairs.push(pair);
        }
    }
    pairs
}

/// 语言的界面名称（中 / 英走听写语料，其它用内置名）。
fn lang_label(lang: Lang, locale: &str) -> String {
    let id = match lang {
        Lang::ZhHans => "dictation-lang-zh-hans",
        Lang::En => "dictation-lang-en",
        other => return other.display_name().to_string(),
    };
    i18n_for(locale).tr(id)
}

/// 翻译提示行：可用时预览语言方向，不可用时给原因，开关关闭或尚未扫描时不显示。
///
/// # 参数
/// - `config`：听写配置。
/// - `support`：缓存的翻译后端能力；`None` 表示尚未扫描。
/// - `locale`：界面语言代码。
///
/// # 返回
/// `(文案, 是否为警示)`。
///
/// ```ignore
/// let note = translate_note(&config, Some(&support), "zh-CN");
/// ```
pub fn translate_note(
    config: &DictationConfig,
    support: Option<&ModelSupport>,
    locale: &str,
) -> Option<(String, bool)> {
    if !config.translate_enabled {
        return None;
    }
    let availability = assess(config, support?);
    if let Some(issue) = availability.issue() {
        return Some((issue_message(&issue, locale), true));
    }
    let i18n = i18n_for(locale);
    let pairs = translate_preview_pairs(config)
        .into_iter()
        .map(|(src, tgt)| {
            i18n.tr_with(
                "stt-ui-translate-pair",
                &Args::new()
                    .named("src", lang_label(src, locale))
                    .named("tgt", lang_label(tgt, locale)),
            )
        })
        .collect::<Vec<_>>()
        .join(PAIR_SEPARATOR);
    Some((
        i18n.tr_with(
            "stt-ui-translate-preview",
            &Args::new().named("pairs", pairs),
        ),
        false,
    ))
}

/// 模型对应的下拉配置值：默认写空串，其余写 ID。
pub fn option_value(spec: &'static SttModelSpec) -> &'static str {
    if spec.role == Role::Default {
        DEFAULT_OPTION_VALUE
    } else {
        spec.id.as_str()
    }
}

/// 字节数的 MB 文本（四舍五入，至少 1）。
pub fn format_size(bytes: u64) -> String {
    format!("{} MB", ((bytes + MIB / 2) / MIB).max(1))
}

/// 下拉标签可容纳的最大字符数（对应设置页模型下拉的宽度）。
#[cfg(test)]
const MAX_OPTION_LABEL_CHARS: usize = 66;

/// 模型的可读名称；语料缺失时回退为模型 ID。
///
/// # 参数
/// - `spec`：模型。
/// - `locale`：界面语言代码。
pub fn model_name(spec: &SttModelSpec, locale: &str) -> String {
    let id = format!("{MODEL_NAME_PREFIX}{}", spec.id);
    let i18n = i18n_for(locale);
    if i18n.has(&id) {
        i18n.tr(&id)
    } else {
        spec.id.clone()
    }
}

/// 下拉选项标签：名称加推荐 / 旧版标记（体积与安装状态见模型面板）。
///
/// # 参数
/// - `spec`：模型。
/// - `locale`：界面语言代码。
pub fn option_label(spec: &SttModelSpec, locale: &str) -> String {
    let id = match spec.role {
        Role::Default => "stt-ui-label-recommended",
        Role::Alternate => "stt-ui-label-alternate",
        Role::Legacy => "stt-ui-label-legacy",
    };
    i18n_for(locale).tr_with(id, &Args::new().named("name", model_name(spec, locale)))
}

/// 三项选择行的说明（置灰原因、无效组合提示）；无特别说明返回 `None`，由调用方用默认说明。
///
/// # 参数
/// - `inputs`：当前配置。
/// - `key`：配置键（模式 / 维度 / 模型之一）。
/// - `locale`：界面语言代码。
///
/// # 返回
/// `(文案, 是否为警示)`。
pub fn selector_note(inputs: &SttInputs, key: &str, locale: &str) -> Option<(String, bool)> {
    if !is_selector_key(key) {
        return None;
    }
    let i18n = i18n_for(locale);
    if inputs.manual_dir_conflict() && key == KEY_DICTATION_RECOGNITION_MODE {
        return Some((i18n.tr("stt-ui-manual-dir-offline"), true));
    }
    inputs.lock_reason().map(|reason| {
        let id = match reason {
            LockReason::SystemBackend => "stt-ui-lock-system",
            LockReason::ManualDir => "stt-ui-lock-manual-dir",
        };
        (i18n.tr(id), false)
    })
}

/// 模型行的一句话状态（已安装 / 未安装与体积）。
///
/// # 参数
/// - `spec`：当前选中的模型。
/// - `installed`：是否已安装。
/// - `locale`：界面语言代码。
pub fn model_row_status(spec: &SttModelSpec, installed: bool, locale: &str) -> String {
    let i18n = i18n_for(locale);
    if installed {
        i18n.tr_with(
            "stt-ui-row-installed",
            &Args::new().named("size", format_size(spec.size_bytes)),
        )
    } else {
        i18n.tr_with(
            "stt-ui-row-missing",
            &Args::new().named("archive", format_size(spec.archive.size)),
        )
    }
}

/// 下载任务的界面状态。
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum DownloadState {
    /// 空闲。
    #[default]
    Idle,
    /// 进行中。
    Running {
        /// 正在安装的模型 ID。
        model_id: String,
        /// 最近一次进度。
        progress: Option<Progress>,
    },
    /// 失败（含用户取消）。
    Failed {
        /// 模型 ID。
        model_id: String,
        /// 错误说明；用户取消时为 `None`。
        message: Option<String>,
    },
    /// 成功。
    Done {
        /// 模型 ID。
        model_id: String,
    },
}

/// 下载面板上主按钮的形态。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PanelAction {
    /// 可下载（未安装，或离线模式缺共享 VAD）。
    Download,
    /// 正在下载本模型，可取消。
    Cancel,
    /// 已就绪。
    Installed,
    /// 正在下载别的模型，暂不可操作。
    Busy,
}

/// 下载面板：选中模型的详情、主按钮与下载结果提示。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PanelModel {
    /// 标题。
    pub title: String,
    /// 说明行。
    pub lines: Vec<String>,
    /// 主按钮形态。
    pub action: PanelAction,
    /// 主按钮文案。
    pub action_label: String,
    /// 按钮旁提示：（文案, 是否为错误）。
    pub notice: Option<(String, bool)>,
    /// 当前选中的模型 ID（点下载时使用）。
    pub model_id: String,
}

/// 进度里展示的资产名：模型压缩包显示模型可读名称，其它（VAD 文件）显示文件名。
///
/// # 参数
/// - `asset`：进度里的资产文件名。
/// - `locale`：界面语言代码。
pub fn asset_display_name(asset: &str, locale: &str) -> String {
    stt_models::manifest()
        .models
        .iter()
        .find(|spec| spec.archive.name == asset)
        .map_or_else(|| asset.to_string(), |spec| model_name(spec, locale))
}

/// 进度文案：阶段、资产名（模型显示可读名称）与百分比。
///
/// # 参数
/// - `progress`：进度快照。
/// - `locale`：界面语言代码。
pub fn progress_text(progress: &Progress, locale: &str) -> String {
    let i18n = i18n_for(locale);
    let stage = i18n.tr(match progress.stage {
        Stage::Downloading => "stt-ui-stage-downloading",
        Stage::Verifying => "stt-ui-stage-verifying",
        Stage::Extracting => "stt-ui-stage-extracting",
    });
    let args = Args::new()
        .named("stage", stage)
        .named("asset", asset_display_name(&progress.asset, locale));
    if progress.total == 0 {
        return i18n.tr_with("stt-ui-progress-no-total", &args);
    }
    let percent = (progress.done.min(progress.total) * 100 / progress.total).to_string();
    i18n.tr_with("stt-ui-progress", &args.named("percent", percent))
}

/// 构建下载面板；后端为系统语音、手动目录或清单无模型时不显示（`None`）。
///
/// # 参数
/// - `inputs`：当前配置。
/// - `installed`：模型是否已安装。
/// - `vad_installed`：共享 VAD 是否已安装。
/// - `download`：下载任务状态。
/// - `locale`：界面语言代码。
pub fn build_panel(
    inputs: &SttInputs,
    installed: impl Fn(&SttModelSpec) -> bool,
    vad_installed: bool,
    download: &DownloadState,
    locale: &str,
) -> Option<PanelModel> {
    if inputs.lock_reason().is_some() {
        return None;
    }
    let spec = inputs.selected()?;
    let i18n = i18n_for(locale);
    let ready = installed(spec);
    let needs_vad = spec.kind.is_offline() && !vad_installed;
    let mut lines = vec![
        i18n.tr_with(
            if ready {
                "stt-ui-line-installed"
            } else {
                "stt-ui-line-missing"
            },
            &Args::new()
                .named("size", format_size(spec.size_bytes))
                .named("archive", format_size(spec.archive.size))
                .named("mem", spec.peak_mem_mb.to_string()),
        ),
    ];
    if let Some(key) = spec.notes_key.as_deref().filter(|k| i18n.has(k)) {
        lines.push(i18n.tr(key));
    }
    if spec.kind.is_offline() {
        lines.push(if needs_vad {
            i18n.tr_with(
                "stt-ui-line-vad-missing",
                &Args::new().named("size", format_size(stt_models::manifest().vad.size)),
            )
        } else {
            i18n.tr("stt-ui-line-vad-ok")
        });
    }
    let license = if spec.license == "unverified" {
        i18n.tr("stt-ui-license-unverified")
    } else {
        spec.license.clone()
    };
    lines.push(i18n.tr_with(
        "stt-ui-line-license",
        &Args::new().named("license", license),
    ));
    if spec.archive.sha256.is_empty() {
        lines.push(i18n.tr("stt-ui-line-unpinned"));
    }
    let running = match download {
        DownloadState::Running { model_id, .. } => Some(model_id.as_str()),
        _ => None,
    };
    let action = match running {
        Some(id) if id == spec.id => PanelAction::Cancel,
        Some(_) => PanelAction::Busy,
        None if ready && !needs_vad => PanelAction::Installed,
        None => PanelAction::Download,
    };
    let action_label = i18n.tr(match action {
        PanelAction::Download => "stt-ui-action-download",
        PanelAction::Cancel => "stt-ui-action-cancel",
        PanelAction::Installed => "stt-ui-action-installed",
        PanelAction::Busy => "stt-ui-action-busy",
    });
    let notice = match download {
        DownloadState::Running {
            model_id,
            progress: Some(p),
        } if *model_id == spec.id => Some((progress_text(p, locale), false)),
        DownloadState::Failed { model_id, message } if *model_id == spec.id => {
            Some(match message {
                Some(detail) => (
                    i18n.tr_with(
                        "stt-ui-download-failed",
                        &Args::new().named("detail", detail.as_str()),
                    ),
                    true,
                ),
                None => (i18n.tr("stt-ui-download-cancelled"), false),
            })
        }
        DownloadState::Done { model_id } if *model_id == spec.id => {
            Some((i18n.tr("stt-ui-download-done"), false))
        }
        _ => None,
    };
    Some(PanelModel {
        title: i18n.tr_with(
            "stt-ui-panel-title",
            &Args::new().named("name", model_name(spec, locale)),
        ),
        lines,
        action,
        action_label,
        notice,
        model_id: spec.id.clone(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dictation::translate::TranslateTarget;
    use serde_json::json;

    /// 用键值对造配置。
    fn inputs(pairs: &[(&str, Value)]) -> SttInputs {
        SttInputs::from_lookup(|key| {
            pairs
                .iter()
                .find(|(k, _)| *k == key)
                .map(|(_, v)| v.clone())
                .unwrap_or(Value::Null)
        })
    }

    /// 默认值：双语 + 流式，无锁。
    fn base() -> SttInputs {
        inputs(&[
            (KEY_DICTATION_RECOGNITION_MODE, json!("streaming")),
            (KEY_DICTATION_LANGUAGE_DIMENSION, json!("bilingual")),
        ])
    }

    /// 选项列表默认在前，默认项写空串，备选写 ID，选中值与配置对应。
    #[test]
    fn options_and_selected_value_mapping() {
        let mut cfg = base();
        let options = cfg.options();
        assert_eq!(options[0].role, Role::Default);
        assert_eq!(option_value(options[0]), DEFAULT_OPTION_VALUE);
        assert_eq!(cfg.selected_value(), Some(DEFAULT_OPTION_VALUE));
        let alt = options.iter().find(|s| s.role == Role::Alternate).unwrap();
        cfg.model_id = alt.id.clone();
        assert_eq!(cfg.selected_value(), Some(alt.id.as_str()));
        cfg.model_id = "no-such-model".into();
        assert_eq!(cfg.selected_value(), Some(DEFAULT_OPTION_VALUE));
    }

    /// 每个维度与模式都有候选，不会出现空列表。
    #[test]
    fn every_combination_lists_models() {
        for dimension in Dimension::ALL {
            for mode in [RecognitionMode::Streaming, RecognitionMode::Offline] {
                let cfg = SttInputs {
                    dimension,
                    mode,
                    ..base()
                };
                assert!(!cfg.options().is_empty());
            }
        }
    }

    /// 置灰原因：系统后端优先，其次手动目录。
    #[test]
    fn lock_reason_priority() {
        assert_eq!(base().lock_reason(), None);
        let manual = inputs(&[(KEY_DICTATION_MODEL_DIR, json!(" D:/m "))]);
        assert_eq!(manual.lock_reason(), Some(LockReason::ManualDir));
        let both = inputs(&[
            (KEY_DICTATION_MODEL_DIR, json!("D:/m")),
            (KEY_DICTATION_BACKEND, json!("system")),
        ]);
        assert_eq!(both.lock_reason(), Some(LockReason::SystemBackend));
    }

    /// 手动目录 + 离线是无效组合；系统后端下不提示。
    #[test]
    fn manual_dir_conflict_only_offline_local() {
        let offline_manual = inputs(&[
            (KEY_DICTATION_MODEL_DIR, json!("D:/m")),
            (KEY_DICTATION_RECOGNITION_MODE, json!("offline")),
        ]);
        assert!(offline_manual.manual_dir_conflict());
        let note = selector_note(&offline_manual, KEY_DICTATION_RECOGNITION_MODE, "zh-CN").unwrap();
        assert!(note.1 && !note.0.is_empty());
        let system = SttInputs {
            system_backend: true,
            ..offline_manual.clone()
        };
        assert!(!system.manual_dir_conflict());
        assert!(!base().manual_dir_conflict());
    }

    /// 置灰说明只对三项选择键给出，且两种语言非空。
    #[test]
    fn selector_note_scope() {
        let manual = inputs(&[(KEY_DICTATION_MODEL_DIR, json!("D:/m"))]);
        for locale in ["en-US", "zh-CN"] {
            for key in [
                KEY_DICTATION_MODEL_ID,
                KEY_DICTATION_LANGUAGE_DIMENSION,
                KEY_DICTATION_RECOGNITION_MODE,
            ] {
                let (text, danger) = selector_note(&manual, key, locale).unwrap();
                assert!(!text.is_empty() && !danger);
            }
            assert!(selector_note(&manual, KEY_DICTATION_MODEL_DIR, locale).is_none());
        }
        assert!(selector_note(&base(), KEY_DICTATION_MODEL_ID, "en-US").is_none());
    }

    /// itn 开关只在选中 SenseVoice 且未置灰时显示。
    #[test]
    fn itn_visible_only_for_sense_voice() {
        let sense = stt_models::list_for(Dimension::Bilingual, RecognitionMode::Offline)
            .into_iter()
            .find(|s| s.kind == ModelKind::OfflineSenseVoice)
            .expect("清单含 SenseVoice");
        let cfg = SttInputs {
            mode: RecognitionMode::Offline,
            model_id: sense.id.clone(),
            ..base()
        };
        assert!(cfg.itn_visible());
        assert!(
            !SttInputs {
                model_id: String::new(),
                ..cfg.clone()
            }
            .itn_visible()
        );
        assert!(
            !SttInputs {
                manual_dir: true,
                ..cfg.clone()
            }
            .itn_visible()
        );
        assert!(
            !SttInputs {
                system_backend: true,
                ..cfg
            }
            .itn_visible()
        );
        assert!(!base().itn_visible());
    }

    /// 联动键判定。
    #[test]
    fn key_predicates() {
        assert!(resets_model_id(KEY_DICTATION_RECOGNITION_MODE));
        assert!(resets_model_id(KEY_DICTATION_LANGUAGE_DIMENSION));
        assert!(!resets_model_id(KEY_DICTATION_MODEL_ID));
        assert!(affects_layout(KEY_DICTATION_MODEL_ID));
        assert!(affects_layout(KEY_DICTATION_BACKEND));
        assert!(!affects_layout("dictation/threads"));
    }

    /// 体积文本：四舍五入且至少 1 MB。
    #[test]
    fn size_formatting() {
        assert_eq!(format_size(0), "1 MB");
        assert_eq!(format_size(76_326_475), "73 MB");
    }

    /// 进度里的模型压缩包显示可读名称，VAD 文件仍显示文件名。
    #[test]
    fn asset_display_name_prefers_model_name() {
        let spec = &stt_models::manifest().models[0];
        for locale in ["en-US", "zh-CN"] {
            assert_eq!(
                asset_display_name(&spec.archive.name, locale),
                model_name(spec, locale)
            );
        }
        assert_eq!(asset_display_name("silero_vad.onnx", "en-US"), "silero_vad.onnx");
        let p = Progress {
            stage: Stage::Downloading,
            asset: spec.archive.name.clone(),
            done: 1,
            total: 4,
        };
        assert!(!progress_text(&p, "en-US").contains(".tar.bz2"));
    }

    /// 清单里每个模型两种语言都有名称，选项标签含名称且不超过下拉宽度可容纳的字符数。
    #[test]
    fn every_model_has_name_and_label() {
        for spec in &stt_models::manifest().models {
            for locale in ["en-US", "zh-CN"] {
                let name = model_name(spec, locale);
                assert_ne!(name, spec.id, "{} 缺 {locale} 名称", spec.id);
                let label = option_label(spec, locale);
                assert!(label.contains(&name), "{label}");
                assert!(
                    label.chars().count() <= MAX_OPTION_LABEL_CHARS,
                    "下拉标签过长: {label}"
                );
            }
        }
    }

    /// 清单里所有 notes_key 两种语言都有文案。
    #[test]
    fn notes_keys_resolve() {
        for spec in &stt_models::manifest().models {
            if let Some(key) = &spec.notes_key {
                for locale in ["en-US", "zh-CN"] {
                    assert!(i18n_for(locale).has(key), "{key} 缺 {locale}");
                }
            }
        }
    }

    /// 进度文案：有总量带百分比，无总量不带。
    #[test]
    fn progress_text_percent() {
        let p = Progress {
            stage: Stage::Downloading,
            asset: "a.tar.bz2".into(),
            done: 50,
            total: 200,
        };
        assert!(progress_text(&p, "en-US").contains("25%"));
        let none = Progress { total: 0, ..p };
        assert!(!progress_text(&none, "en-US").contains('%'));
    }

    /// 面板：置灰时无面板；未安装可下载；下载中本模型可取消；他模型忙；失败 / 取消 / 完成提示。
    #[test]
    fn panel_actions_and_notices() {
        let cfg = base();
        let spec = cfg.selected().unwrap();
        assert!(
            build_panel(
                &SttInputs {
                    manual_dir: true,
                    ..cfg.clone()
                },
                |_| false,
                true,
                &DownloadState::Idle,
                "en-US"
            )
            .is_none()
        );
        let panel = |ready: bool, vad: bool, dl: &DownloadState| {
            build_panel(&cfg, |_| ready, vad, dl, "en-US").unwrap()
        };
        assert_eq!(
            panel(false, true, &DownloadState::Idle).action,
            PanelAction::Download
        );
        assert_eq!(
            panel(true, true, &DownloadState::Idle).action,
            PanelAction::Installed
        );
        let running = DownloadState::Running {
            model_id: spec.id.clone(),
            progress: None,
        };
        assert_eq!(panel(false, true, &running).action, PanelAction::Cancel);
        let other = DownloadState::Running {
            model_id: "other".into(),
            progress: None,
        };
        assert_eq!(panel(false, true, &other).action, PanelAction::Busy);
        let failed = DownloadState::Failed {
            model_id: spec.id.clone(),
            message: Some("boom".into()),
        };
        let notice = panel(false, true, &failed).notice.unwrap();
        assert!(notice.1 && notice.0.contains("boom"));
        let cancelled = DownloadState::Failed {
            model_id: spec.id.clone(),
            message: None,
        };
        assert!(!panel(false, true, &cancelled).notice.unwrap().1);
        assert!(
            panel(
                true,
                true,
                &DownloadState::Done {
                    model_id: spec.id.clone()
                }
            )
            .notice
            .is_some()
        );
        assert!(
            panel(
                true,
                true,
                &DownloadState::Done {
                    model_id: "x".into()
                }
            )
            .notice
            .is_none()
        );
    }

    /// 离线模型缺 VAD 时即使模型已装也要下载；面板含 VAD 行与校验值待固定提示。
    #[test]
    fn offline_panel_needs_vad() {
        let cfg = SttInputs {
            mode: RecognitionMode::Offline,
            ..base()
        };
        let missing = build_panel(&cfg, |_| true, false, &DownloadState::Idle, "zh-CN").unwrap();
        assert_eq!(missing.action, PanelAction::Download);
        let ok = build_panel(&cfg, |_| true, true, &DownloadState::Idle, "zh-CN").unwrap();
        assert_eq!(ok.action, PanelAction::Installed);
        assert_ne!(missing.lines, ok.lines);
        let spec = cfg.selected().unwrap();
        let unpinned = build_panel(&cfg, |_| true, true, &DownloadState::Idle, "zh-CN")
            .unwrap()
            .lines
            .len();
        let expect =
            3 + usize::from(spec.notes_key.is_some()) + usize::from(spec.archive.sha256.is_empty());
        assert_eq!(unpinned, expect);
    }

    /// 造一份听写配置。
    fn dict(enabled: bool, dimension: Dimension, target: TranslateTarget) -> DictationConfig {
        let mut config = DictationConfig::from_document(
            &snow_config::document::ConfigDocument::from_bytes(None),
        );
        config.translate_enabled = enabled;
        config.dimension = dimension;
        config.translate_target = target;
        config
    }

    /// 语言方向预览：单语言维度一个方向，混合维度两个，同语言为空。
    #[test]
    fn preview_pairs_per_dimension() {
        let auto = |d| translate_preview_pairs(&dict(true, d, TranslateTarget::Auto));
        assert_eq!(auto(Dimension::Zh), vec![(Lang::ZhHans, Lang::En)]);
        assert_eq!(auto(Dimension::En), vec![(Lang::En, Lang::ZhHans)]);
        assert_eq!(
            auto(Dimension::Bilingual),
            vec![(Lang::ZhHans, Lang::En), (Lang::En, Lang::ZhHans)]
        );
        let fixed = translate_preview_pairs(&dict(true, Dimension::Bilingual, TranslateTarget::En));
        assert_eq!(fixed, vec![(Lang::ZhHans, Lang::En)]);
        let same = translate_preview_pairs(&dict(true, Dimension::Zh, TranslateTarget::ZhHans));
        assert!(same.is_empty());
    }

    /// 提示行：关闭或未扫描不显示；各可用性分支的文案与警示标记。
    #[test]
    fn translate_note_branches() {
        let on = dict(true, Dimension::Bilingual, TranslateTarget::Auto);
        let off = dict(false, Dimension::Bilingual, TranslateTarget::Auto);
        let any = ModelSupport::Any;
        for locale in ["en-US", "zh-CN"] {
            assert!(translate_note(&off, Some(&any), locale).is_none());
            assert!(translate_note(&on, None, locale).is_none());
            let (ready, danger) = translate_note(&on, Some(&any), locale).unwrap();
            assert!(!danger && !ready.is_empty() && !ready.contains('{'));
            let (none, danger) =
                translate_note(&on, Some(&ModelSupport::Unavailable), locale).unwrap();
            assert!(danger && none != ready);
            let only_ja = ModelSupport::Pairs(vec![(Lang::Ja, Lang::En)]);
            let (unsupported, danger) = translate_note(&on, Some(&only_ja), locale).unwrap();
            assert!(danger && unsupported != none);
            let same = dict(true, Dimension::Zh, TranslateTarget::ZhHans);
            let (same_note, danger) = translate_note(&same, Some(&any), locale).unwrap();
            assert!(danger && same_note != ready);
        }
        let (zh, _) = translate_note(&on, Some(&any), "zh-CN").unwrap();
        assert!(zh.contains("中文译为英文") && zh.contains("英文译为中文"));
    }

    /// 空目录扫描得到不可用；开关关闭时仍可扫描（由调用方决定是否调用）。
    #[test]
    fn scan_empty_dir_is_unavailable() {
        let root = std::env::temp_dir().join("cisox-stt-settings-no-models");
        let tcfg = crate::translate_service::TranslateConfig::from_document(
            &snow_config::document::ConfigDocument::from_bytes(None),
            "en-US",
        );
        assert_eq!(
            scan_translate_support(&tcfg, &root),
            ModelSupport::Unavailable
        );
    }

    /// 只有目标语言行挂提示；重扫键覆盖开关与翻译后端 / 模型目录。
    #[test]
    fn translate_key_predicates() {
        assert!(is_translate_note_key(KEY_DICTATION_TRANSLATE_TARGET));
        assert!(!is_translate_note_key(KEY_DICTATION_TRANSLATE_ENABLED));
        for key in [
            KEY_DICTATION_TRANSLATE_ENABLED,
            KEY_TRANSLATION_BACKEND,
            KEY_LOCAL_MODELS_DIR,
            KEY_LOCAL_MODEL_ID,
        ] {
            assert!(rescans_translate_support(key), "{key}");
        }
        assert!(!rescans_translate_support(KEY_DICTATION_TRANSLATE_TARGET));
        assert!(!rescans_translate_support(KEY_DICTATION_LANGUAGE_DIMENSION));
    }

    /// 目标语言选项与配置值一一对应，且两种语言都有标签。
    #[test]
    fn target_option_values_map_to_enum() {
        use snow_config::extensions::{
            DICTATION_TARGET_AUTO, DICTATION_TARGET_EN, DICTATION_TARGET_ZH_HANS,
        };
        assert_eq!(
            TranslateTarget::from_config(DICTATION_TARGET_AUTO),
            TranslateTarget::Auto
        );
        assert_eq!(
            TranslateTarget::from_config(DICTATION_TARGET_ZH_HANS),
            TranslateTarget::ZhHans
        );
        assert_eq!(
            TranslateTarget::from_config(DICTATION_TARGET_EN),
            TranslateTarget::En
        );
        for locale in ["en-US", "zh-CN"] {
            for option in [
                DICTATION_TARGET_AUTO,
                DICTATION_TARGET_ZH_HANS,
                DICTATION_TARGET_EN,
            ] {
                assert!(
                    crate::settings_text::option_text(
                        locale,
                        KEY_DICTATION_TRANSLATE_TARGET,
                        option
                    )
                    .is_some(),
                    "{option} 缺 {locale} 标签"
                );
            }
        }
    }

    /// 取消标记按指针比较相等。
    #[test]
    fn cancel_flag_semantics() {
        let a = CancelFlag::default();
        let b = a.clone();
        assert_eq!(a, b);
        assert_ne!(a, CancelFlag::default());
        assert!(!b.is_cancelled());
        a.cancel();
        assert!(b.is_cancelled());
    }
}
