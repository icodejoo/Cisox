//! 语音转文字的配置读取与启动前检查（纯逻辑，可离屏单测）。

use super::output::OutputMode;
use super::status::Failure;
use super::translate::TranslateTarget;
use crate::stt_download::{is_installed, is_vad_installed};
use crate::stt_models::{self, Dimension, mode_from_config};
use snow_config::document::ConfigDocument;
use snow_config::extensions::{
    DICTATION_BACKEND_SYSTEM, DICTATION_MODE_HOLD, DICTATION_MODE_TOGGLE, KEY_DICTATION_BACKEND,
    KEY_DICTATION_LANGUAGE, KEY_DICTATION_LANGUAGE_DIMENSION, KEY_DICTATION_MAX_SECONDS,
    KEY_DICTATION_MODEL_DIR, KEY_DICTATION_MODEL_ID, KEY_DICTATION_OUTPUT_MODE,
    KEY_DICTATION_RECOGNITION_MODE, KEY_DICTATION_SENSEVOICE_ITN, KEY_DICTATION_THREADS,
    KEY_DICTATION_TRANSLATE_ENABLED, KEY_DICTATION_TRANSLATE_TARGET, KEY_DICTATION_TRIGGER_MODE,
    KEY_DICTATION_TYPE_WITH_OVERLAY,
};
use snow_stt_protocol::{
    BackendKind, EndpointRules, ModelKind, RecognitionMode, StartRequest, VAD_MODEL_FILE_NAME,
};
use std::path::{Path, PathBuf};

/// 旧版平铺布局的标志文件：模型文件直接放在 `<数据根>/models/stt` 下时，该目录里有它。
const LEGACY_FLAT_MARKER: &str = "tokens.txt";

/// 引擎后端。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Backend {
    /// 本地模型（snow-stt 工作进程）。
    LocalModel,
    /// 系统语音（Windows `SpeechRecognizer`，同样由 snow-stt 工作进程承载）。
    System,
}

/// 触发模式：决定哪几个热键生效。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TriggerMode {
    /// 切换式与按住说话两个热键都生效。
    Both,
    /// 只有切换式热键生效。
    Toggle,
    /// 只有按住说话热键生效。
    Hold,
}

impl TriggerMode {
    /// 由配置值解析；未知值按两个都生效。
    ///
    /// # 参数
    /// - `value`：配置里的取值。
    pub fn from_config(value: &str) -> Self {
        match value {
            DICTATION_MODE_TOGGLE => Self::Toggle,
            DICTATION_MODE_HOLD => Self::Hold,
            _ => Self::Both,
        }
    }

    /// 切换式热键是否生效。
    pub fn toggle_enabled(self) -> bool {
        matches!(self, Self::Both | Self::Toggle)
    }

    /// 按住说话热键是否生效。
    pub fn hold_enabled(self) -> bool {
        matches!(self, Self::Both | Self::Hold)
    }
}

/// 一轮识别用到的配置快照。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DictationConfig {
    /// 引擎后端。
    pub backend: Backend,
    /// 触发模式。
    pub trigger: TriggerMode,
    /// 手动指定的模型目录（配置原值；非空时优先于清单模型，空串表示用内置清单）。
    pub model_dir: String,
    /// 识别模式（流式 / 离线）。
    pub recognition: RecognitionMode,
    /// 语言维度。
    pub dimension: Dimension,
    /// 所选备选模型 ID，空串表示默认模型。
    pub model_id: String,
    /// SenseVoice 是否启用逆文本规整。
    pub sensevoice_itn: bool,
    /// 语言提示。
    pub language: String,
    /// 推理线程数。
    pub threads: u32,
    /// 单次最长秒数（0 为不限）。
    pub max_seconds: u32,
    /// 输出方式。
    pub output: OutputMode,
    /// 键入时是否同时显示浮窗。
    pub type_with_overlay: bool,
    /// 定稿句是否同时翻译。
    pub translate_enabled: bool,
    /// 译文目标语言偏好。
    pub translate_target: TranslateTarget,
}

impl DictationConfig {
    /// 从配置文档读取（缺键 / 非法值已由文档层补默认）。
    ///
    /// # 参数
    /// - `document`：配置文档。
    pub fn from_document(document: &ConfigDocument) -> Self {
        let text = |key: &str| {
            document
                .value(key)
                .as_str()
                .unwrap_or_default()
                .trim()
                .to_string()
        };
        let number = |key: &str| document.value(key).as_u64().unwrap_or_default() as u32;
        Self {
            backend: if text(KEY_DICTATION_BACKEND) == DICTATION_BACKEND_SYSTEM {
                Backend::System
            } else {
                Backend::LocalModel
            },
            trigger: TriggerMode::from_config(&text(KEY_DICTATION_TRIGGER_MODE)),
            model_dir: text(KEY_DICTATION_MODEL_DIR),
            recognition: mode_from_config(&text(KEY_DICTATION_RECOGNITION_MODE)),
            dimension: Dimension::from_config(&text(KEY_DICTATION_LANGUAGE_DIMENSION)),
            model_id: text(KEY_DICTATION_MODEL_ID),
            sensevoice_itn: document
                .value(KEY_DICTATION_SENSEVOICE_ITN)
                .as_bool()
                .unwrap_or(true),
            language: text(KEY_DICTATION_LANGUAGE),
            threads: number(KEY_DICTATION_THREADS).max(1),
            max_seconds: number(KEY_DICTATION_MAX_SECONDS),
            output: OutputMode::from_config(&text(KEY_DICTATION_OUTPUT_MODE)),
            type_with_overlay: document
                .value(KEY_DICTATION_TYPE_WITH_OVERLAY)
                .as_bool()
                .unwrap_or(false),
            translate_enabled: document
                .value(KEY_DICTATION_TRANSLATE_ENABLED)
                .as_bool()
                .unwrap_or(false),
            translate_target: TranslateTarget::from_config(&text(KEY_DICTATION_TRANSLATE_TARGET)),
        }
    }

    /// 手动目录或旧版默认目录：配置为空时取 `<数据根>/models/stt`（清单模型在其子目录里，不走这里）。
    ///
    /// # 参数
    /// - `data_root`：应用数据根目录。
    pub fn resolved_model_dir(&self, data_root: &Path) -> PathBuf {
        if self.model_dir.is_empty() {
            stt_models::models_root(data_root)
        } else {
            PathBuf::from(&self.model_dir)
        }
    }
}

/// 本地模型的解析结果。
struct LocalChoice {
    /// 传给 worker 的模型目录。
    dir: PathBuf,
    /// 识别模式。
    mode: RecognitionMode,
    /// 模型类型。
    kind: ModelKind,
    /// 是否启用逆文本规整。
    itn: bool,
}

/// 解析本地模型。优先级：
/// 1. `model_dir` 非空：手动目录，视为旧行为（流式、`online-transducer`）；离线模式下无法由目录推断模型类型，
///    返回 [`Failure::ManualDirNeedsStreaming`]，不猜。
/// 2. 旧版平铺布局（`<数据根>/models/stt` 下直接有 `tokens.txt`）且为流式、未选备选：沿用该目录，旧用户无需重新下载。
/// 3. 否则按（语言维度, 模式, `model_id`）在清单里取模型，要求已安装；离线模式还要求共享 VAD 已安装。
fn select_local_model(
    config: &DictationConfig,
    data_root: &Path,
    dir_exists: &impl Fn(&Path) -> bool,
) -> Result<LocalChoice, Failure> {
    let legacy = |dir: PathBuf| LocalChoice {
        dir,
        mode: RecognitionMode::Streaming,
        kind: ModelKind::OnlineTransducer,
        itn: false,
    };
    let offline = config.recognition == RecognitionMode::Offline;
    if !config.model_dir.is_empty() {
        let dir = PathBuf::from(&config.model_dir);
        if offline {
            return Err(Failure::ManualDirNeedsStreaming);
        }
        if !dir_exists(&dir) {
            return Err(Failure::ModelDirMissing(dir.display().to_string()));
        }
        return Ok(legacy(dir));
    }
    let root = stt_models::models_root(data_root);
    if !offline
        && config.model_id.is_empty()
        && dir_exists(&root)
        && root.join(LEGACY_FLAT_MARKER).is_file()
    {
        return Ok(legacy(root));
    }
    let spec = stt_models::resolve(config.dimension, config.recognition, &config.model_id)
        .map_err(Failure::ModelUnavailable)?;
    if !is_installed(spec, data_root) {
        return Err(Failure::ModelNotInstalled(spec.id.clone()));
    }
    if spec.kind.is_offline() && !is_vad_installed(data_root) {
        return Err(Failure::ModelNotInstalled(VAD_MODEL_FILE_NAME.to_string()));
    }
    Ok(LocalChoice {
        dir: stt_models::model_dir(spec, data_root),
        mode: spec.mode,
        kind: spec.kind,
        itn: config.sensevoice_itn && spec.kind == ModelKind::OfflineSenseVoice,
    })
}

/// 启动前检查：可执行文件、（本地模型时的）模型都满足才返回 START 请求，否则给出可读的失败原因。
/// 本地模型的解析顺序见 [`select_local_model`]。
///
/// # 参数
/// - `config`：配置快照。
/// - `data_root`：数据根目录。
/// - `exe`：已定位的工作进程路径，找不到为 `None`。
/// - `dir_exists`：判断目录是否存在（便于测试）；模型是否装好直接看数据根下的真实文件。
///
/// # 返回
/// 工作进程路径与 START 请求。
///
/// ```ignore
/// let (exe, request) = prepare_launch(&config, root, Some(exe), |p| p.is_dir())?;
/// ```
pub fn prepare_launch(
    config: &DictationConfig,
    data_root: &Path,
    exe: Option<PathBuf>,
    dir_exists: impl Fn(&Path) -> bool,
) -> Result<(PathBuf, StartRequest), Failure> {
    let exe = exe.ok_or(Failure::WorkerMissing)?;
    let language = if config.language.is_empty() {
        "auto".to_string()
    } else {
        config.language.clone()
    };
    let mut request = StartRequest {
        language,
        threads: config.threads,
        endpoint: EndpointRules::default(),
        max_seconds: config.max_seconds,
        ..Default::default()
    };
    // 系统语音不需要模型
    if config.backend == Backend::System {
        request.backend = BackendKind::System;
    } else {
        let choice = select_local_model(config, data_root, &dir_exists)?;
        request.backend = BackendKind::Local;
        request.model_dir = choice.dir.display().to_string();
        request.mode = choice.mode;
        request.kind = choice.kind;
        request.itn = choice.itn;
    }
    Ok((exe, request))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::stt_models::SttModelSpec;
    use serde_json::json;

    /// 默认配置：本地模型、两个热键都生效、自动输出、不同时显示浮窗。
    #[test]
    fn defaults_from_fresh_document() {
        let config = DictationConfig::from_document(&ConfigDocument::from_bytes(None));
        assert_eq!(config.backend, Backend::LocalModel);
        assert_eq!(config.trigger, TriggerMode::Both);
        assert_eq!(config.output, OutputMode::Auto);
        assert!(!config.type_with_overlay);
        assert_eq!(config.threads, 2);
        assert_eq!(config.language, "auto");
        assert_eq!(config.model_dir, "");
    }

    /// 配置值映射：后端、触发模式、输出方式、线程与最长秒数。
    #[test]
    fn values_are_mapped() {
        let mut doc = ConfigDocument::from_bytes(None);
        doc.set_value(KEY_DICTATION_BACKEND, json!("system"))
            .unwrap();
        doc.set_value(KEY_DICTATION_TRIGGER_MODE, json!("hold"))
            .unwrap();
        doc.set_value(KEY_DICTATION_OUTPUT_MODE, json!("overlay"))
            .unwrap();
        doc.set_value(KEY_DICTATION_TYPE_WITH_OVERLAY, json!(true))
            .unwrap();
        doc.set_value(KEY_DICTATION_THREADS, json!(4)).unwrap();
        doc.set_value(KEY_DICTATION_MAX_SECONDS, json!(0)).unwrap();
        let config = DictationConfig::from_document(&doc);
        assert_eq!(config.backend, Backend::System);
        assert_eq!(config.trigger, TriggerMode::Hold);
        assert_eq!(config.output, OutputMode::Overlay);
        assert!(config.type_with_overlay);
        assert_eq!((config.threads, config.max_seconds), (4, 0));
    }

    /// 触发模式决定哪个热键生效。
    #[test]
    fn trigger_mode_enables_hotkeys() {
        assert!(TriggerMode::Both.toggle_enabled() && TriggerMode::Both.hold_enabled());
        assert!(TriggerMode::Toggle.toggle_enabled() && !TriggerMode::Toggle.hold_enabled());
        assert!(!TriggerMode::Hold.toggle_enabled() && TriggerMode::Hold.hold_enabled());
        assert_eq!(TriggerMode::from_config("???"), TriggerMode::Both);
    }

    /// 模型目录：为空取数据根下默认目录，否则用配置值。
    #[test]
    fn model_dir_resolution() {
        let mut config = DictationConfig::from_document(&ConfigDocument::from_bytes(None));
        let root = Path::new("D:/data");
        assert_eq!(
            config.resolved_model_dir(root),
            PathBuf::from("D:/data").join("models").join("stt")
        );
        config.model_dir = "E:/my models".into();
        assert_eq!(
            config.resolved_model_dir(root),
            PathBuf::from("E:/my models")
        );
    }

    /// 唯一临时数据根。
    fn temp_root(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("snow-dict-cfg-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("建目录");
        dir
    }

    /// 在数据根下伪造一个已安装的清单模型（文件齐全 + 完成标记）。
    fn fake_install(spec: &SttModelSpec, root: &Path) {
        let dir = stt_models::model_dir(spec, root);
        std::fs::create_dir_all(&dir).expect("建目录");
        for f in &spec.files {
            std::fs::write(dir.join(f), b"x").expect("写");
        }
        std::fs::write(dir.join(crate::ocr_assets::COMPLETE_MARKER), b"{}").expect("写");
    }

    /// 伪造共享 VAD（大小与清单一致）。
    fn fake_vad(root: &Path) {
        let vad = stt_models::manifest().vad.clone();
        std::fs::create_dir_all(stt_models::models_root(root)).expect("建目录");
        std::fs::write(
            stt_models::vad_path(root),
            vec![0u8; usize::try_from(vad.size).expect("大小")],
        )
        .expect("写");
    }

    /// 默认配置（本地模型）。
    fn local_config() -> DictationConfig {
        DictationConfig::from_document(&ConfigDocument::from_bytes(None))
    }

    /// 测试用的 worker 路径。
    fn exe() -> Option<PathBuf> {
        Some(PathBuf::from("D:/stt/snow-stt.exe"))
    }

    /// 启动前检查：找不到 exe → 可读错误；系统后端不查模型；本地模型没装 → 未安装；旧版平铺目录 → 沿用旧行为。
    #[test]
    fn launch_preconditions() {
        let root = temp_root("pre");
        let mut config = local_config();

        // 系统语音：不看模型，请求里后端为 System、模型目录为空
        config.backend = Backend::System;
        let (path, sys) = prepare_launch(&config, &root, exe(), |_| false).unwrap();
        assert_eq!(Some(path), exe());
        assert_eq!(sys.backend, BackendKind::System);
        assert!(sys.model_dir.is_empty());
        assert_eq!(
            prepare_launch(&config, &root, None, |_| true).unwrap_err(),
            Failure::WorkerMissing
        );

        // 本地：什么都没装 → 提示默认模型未安装
        config.backend = Backend::LocalModel;
        assert_eq!(
            prepare_launch(&config, &root, None, |_| true).unwrap_err(),
            Failure::WorkerMissing
        );
        let default = stt_models::default_for(Dimension::Bilingual, RecognitionMode::Streaming)
            .expect("默认");
        assert_eq!(
            prepare_launch(&config, &root, exe(), |p| p.is_dir()).unwrap_err(),
            Failure::ModelNotInstalled(default.id.clone())
        );

        // 旧版平铺布局：tokens.txt 直接在 models/stt 下，行为与旧版一致
        let flat = stt_models::models_root(&root);
        std::fs::create_dir_all(&flat).expect("建目录");
        std::fs::write(flat.join("tokens.txt"), b"a").expect("写");
        config.language = String::new();
        config.max_seconds = 90;
        let (path, request) = prepare_launch(&config, &root, exe(), |p| p.is_dir()).unwrap();
        assert_eq!(Some(path), exe());
        assert_eq!(request.backend, BackendKind::Local);
        assert_eq!(request.language, "auto");
        assert_eq!((request.threads, request.max_seconds), (2, 90));
        assert_eq!(request.model_dir, flat.display().to_string());
        assert_eq!(request.mode, RecognitionMode::Streaming);
        assert_eq!(request.kind, ModelKind::OnlineTransducer);
        assert!(!request.itn && request.vad.is_none());
        let _ = std::fs::remove_dir_all(&root);
    }

    /// 旧配置（没有任何新键）：手动模型目录行为不变——目录存在即用，流式、online-transducer；不存在报目录缺失。
    #[test]
    fn legacy_manual_dir_is_unchanged() {
        let root = temp_root("manual");
        let mut config = local_config();
        config.model_dir = "E:/my models".into();
        match prepare_launch(&config, &root, exe(), |_| false).unwrap_err() {
            Failure::ModelDirMissing(path) => assert!(path.contains("my models"), "{path}"),
            other => panic!("意外的失败原因：{other:?}"),
        }
        let (_, request) = prepare_launch(&config, &root, exe(), |_| true).unwrap();
        assert_eq!(request.model_dir, "E:/my models");
        assert_eq!(request.mode, RecognitionMode::Streaming);
        assert_eq!(request.kind, ModelKind::OnlineTransducer);
        assert!(!request.itn);
        // 换了语言维度，手动目录也优先
        config.dimension = Dimension::Zh;
        let (_, request) = prepare_launch(&config, &root, exe(), |_| true).unwrap();
        assert_eq!(request.model_dir, "E:/my models");
        let _ = std::fs::remove_dir_all(&root);
    }

    /// 手动目录 + 离线模式：模型类型无法推断，返回明确的失败而不是猜。
    #[test]
    fn manual_dir_with_offline_mode_fails_clearly() {
        let root = temp_root("manual-off");
        let mut config = local_config();
        config.model_dir = "E:/my models".into();
        config.recognition = RecognitionMode::Offline;
        assert_eq!(
            prepare_launch(&config, &root, exe(), |_| true).unwrap_err(),
            Failure::ManualDirNeedsStreaming
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    /// 流式：各语言维度解析到各自的默认模型目录，请求带清单里的模式与类型。
    #[test]
    fn streaming_defaults_per_dimension() {
        let root = temp_root("stream");
        let mut config = local_config();
        for dimension in Dimension::ALL {
            let spec =
                stt_models::default_for(dimension, RecognitionMode::Streaming).expect("默认");
            fake_install(spec, &root);
            config.dimension = dimension;
            let (_, request) = prepare_launch(&config, &root, exe(), |_| true).unwrap();
            assert_eq!(
                request.model_dir,
                stt_models::model_dir(spec, &root).display().to_string()
            );
            assert_eq!(request.mode, RecognitionMode::Streaming);
            assert_eq!(request.kind, ModelKind::OnlineTransducer);
            assert!(request.vad.is_none() && !request.itn);
        }
        let _ = std::fs::remove_dir_all(&root);
    }

    /// 备选：`model_id` 命中备选就用备选；未安装则提示该备选未安装；不属于当前组合则回落默认。
    #[test]
    fn model_id_selects_alternate() {
        let root = temp_root("alt");
        let mut config = local_config();
        config.dimension = Dimension::Zh;
        let alt = stt_models::alternates_for(Dimension::Zh, RecognitionMode::Streaming)[0];
        config.model_id = alt.id.clone();
        assert_eq!(
            prepare_launch(&config, &root, exe(), |_| true).unwrap_err(),
            Failure::ModelNotInstalled(alt.id.clone())
        );
        fake_install(alt, &root);
        let (_, request) = prepare_launch(&config, &root, exe(), |_| true).unwrap();
        assert!(request.model_dir.ends_with(&alt.id));
        // 换到英文维度：该 ID 不属于英文，回落到英文默认模型
        config.dimension = Dimension::En;
        let en = stt_models::default_for(Dimension::En, RecognitionMode::Streaming).expect("默认");
        assert_eq!(
            prepare_launch(&config, &root, exe(), |_| true).unwrap_err(),
            Failure::ModelNotInstalled(en.id.clone())
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    /// 离线：模型没装报模型未安装；VAD 没装报 VAD 未安装；都装好后请求带离线模式与类型。
    #[test]
    fn offline_requires_vad() {
        let root = temp_root("offline");
        let mut config = local_config();
        config.dimension = Dimension::Zh;
        config.recognition = RecognitionMode::Offline;
        let spec = stt_models::default_for(Dimension::Zh, RecognitionMode::Offline).expect("默认");
        assert_eq!(
            prepare_launch(&config, &root, exe(), |_| true).unwrap_err(),
            Failure::ModelNotInstalled(spec.id.clone())
        );
        fake_install(spec, &root);
        assert_eq!(
            prepare_launch(&config, &root, exe(), |_| true).unwrap_err(),
            Failure::ModelNotInstalled(VAD_MODEL_FILE_NAME.to_string())
        );
        fake_vad(&root);
        let (_, request) = prepare_launch(&config, &root, exe(), |_| true).unwrap();
        assert_eq!(request.mode, RecognitionMode::Offline);
        assert_eq!(request.kind, ModelKind::OfflineParaformer);
        assert!(!request.itn && request.vad.is_none());
        let _ = std::fs::remove_dir_all(&root);
    }

    /// SenseVoice：itn 取配置，且仅该模型为 true；其它离线模型即使开着也是 false。
    #[test]
    fn itn_only_for_sense_voice() {
        let root = temp_root("itn");
        fake_vad(&root);
        let mut config = local_config();
        config.recognition = RecognitionMode::Offline;
        config.dimension = Dimension::Bilingual;
        let sv = stt_models::alternates_for(Dimension::Bilingual, RecognitionMode::Offline)
            .into_iter()
            .find(|s| s.kind == ModelKind::OfflineSenseVoice)
            .expect("SenseVoice");
        fake_install(sv, &root);
        config.model_id = sv.id.clone();
        config.sensevoice_itn = true;
        let (_, request) = prepare_launch(&config, &root, exe(), |_| true).unwrap();
        assert_eq!(request.kind, ModelKind::OfflineSenseVoice);
        assert!(request.itn);
        config.sensevoice_itn = false;
        let (_, request) = prepare_launch(&config, &root, exe(), |_| true).unwrap();
        assert!(!request.itn);
        // 默认 x-asr 离线模型：itn 恒为 false
        config.sensevoice_itn = true;
        config.model_id = String::new();
        let xasr =
            stt_models::default_for(Dimension::Bilingual, RecognitionMode::Offline).expect("默认");
        fake_install(xasr, &root);
        let (_, request) = prepare_launch(&config, &root, exe(), |_| true).unwrap();
        assert_eq!(request.kind, ModelKind::OfflineTransducer);
        assert!(!request.itn);
        let _ = std::fs::remove_dir_all(&root);
    }

    /// 新配置键的默认值与读取：默认中英混合 + 流式 + 空模型 ID + itn 开；写入后能读回。
    #[test]
    fn new_keys_defaults_and_values() {
        let config = local_config();
        assert_eq!(config.recognition, RecognitionMode::Streaming);
        assert_eq!(config.dimension, Dimension::Bilingual);
        assert_eq!(config.model_id, "");
        assert!(config.sensevoice_itn);
        let mut doc = ConfigDocument::from_bytes(None);
        doc.set_value(KEY_DICTATION_RECOGNITION_MODE, json!("offline"))
            .unwrap();
        doc.set_value(KEY_DICTATION_LANGUAGE_DIMENSION, json!("zh"))
            .unwrap();
        doc.set_value(KEY_DICTATION_MODEL_ID, json!("abc")).unwrap();
        doc.set_value(KEY_DICTATION_SENSEVOICE_ITN, json!(false))
            .unwrap();
        let config = DictationConfig::from_document(&doc);
        assert_eq!(config.recognition, RecognitionMode::Offline);
        assert_eq!(config.dimension, Dimension::Zh);
        assert_eq!(config.model_id, "abc");
        assert!(!config.sensevoice_itn);
    }

    /// 翻译配置：默认关闭 + 自动目标；写入后能读回。
    #[test]
    fn translate_keys_defaults_and_values() {
        let config = local_config();
        assert!(!config.translate_enabled);
        assert_eq!(config.translate_target, TranslateTarget::Auto);
        let mut doc = ConfigDocument::from_bytes(None);
        doc.set_value(KEY_DICTATION_TRANSLATE_ENABLED, json!(true))
            .unwrap();
        doc.set_value(KEY_DICTATION_TRANSLATE_TARGET, json!("en"))
            .unwrap();
        let config = DictationConfig::from_document(&doc);
        assert!(config.translate_enabled);
        assert_eq!(config.translate_target, TranslateTarget::En);
    }
}
