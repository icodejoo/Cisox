//! 语音转文字的配置读取与启动前检查（纯逻辑，可离屏单测）。

use super::output::OutputMode;
use super::status::Failure;
use snow_config::document::ConfigDocument;
use snow_config::extensions::{
    DICTATION_BACKEND_SYSTEM, DICTATION_MODE_HOLD, DICTATION_MODE_TOGGLE, KEY_DICTATION_BACKEND,
    KEY_DICTATION_LANGUAGE, KEY_DICTATION_MAX_SECONDS, KEY_DICTATION_MODEL_DIR,
    KEY_DICTATION_OUTPUT_MODE, KEY_DICTATION_THREADS, KEY_DICTATION_TRIGGER_MODE,
    KEY_DICTATION_TYPE_WITH_OVERLAY,
};
use snow_stt_protocol::{BackendKind, EndpointRules, StartRequest};
use std::path::{Path, PathBuf};

/// 数据根下的默认模型目录（相对路径各段）。
const DEFAULT_MODEL_SUBDIR: [&str; 2] = ["models", "stt"];

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
    /// 模型目录（配置原值，空串表示默认目录）。
    pub model_dir: String,
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
            language: text(KEY_DICTATION_LANGUAGE),
            threads: number(KEY_DICTATION_THREADS).max(1),
            max_seconds: number(KEY_DICTATION_MAX_SECONDS),
            output: OutputMode::from_config(&text(KEY_DICTATION_OUTPUT_MODE)),
            type_with_overlay: document
                .value(KEY_DICTATION_TYPE_WITH_OVERLAY)
                .as_bool()
                .unwrap_or(false),
        }
    }

    /// 实际使用的模型目录：配置为空时取 `<数据根>/models/stt`。
    ///
    /// # 参数
    /// - `data_root`：应用数据根目录。
    pub fn resolved_model_dir(&self, data_root: &Path) -> PathBuf {
        if self.model_dir.is_empty() {
            let mut dir = data_root.to_path_buf();
            dir.extend(DEFAULT_MODEL_SUBDIR);
            dir
        } else {
            PathBuf::from(&self.model_dir)
        }
    }
}

/// 启动前检查：可执行文件、（本地模型时的）模型目录都满足才返回 START 请求，否则给出可读的失败原因。
///
/// # 参数
/// - `config`：配置快照。
/// - `data_root`：数据根目录。
/// - `exe`：已定位的工作进程路径，找不到为 `None`。
/// - `dir_exists`：判断目录是否存在（便于测试）。
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
    // 系统语音不需要模型目录
    let (backend, model_dir) = if config.backend == Backend::System {
        (BackendKind::System, String::new())
    } else {
        let dir = config.resolved_model_dir(data_root);
        if !dir_exists(&dir) {
            return Err(Failure::ModelDirMissing(dir.display().to_string()));
        }
        (BackendKind::Local, dir.display().to_string())
    };
    let language = if config.language.is_empty() {
        "auto".to_string()
    } else {
        config.language.clone()
    };
    let request = StartRequest {
        backend,
        language,
        threads: config.threads,
        endpoint: EndpointRules::default(),
        max_seconds: config.max_seconds,
        model_dir,
    };
    Ok((exe, request))
}

#[cfg(test)]
mod tests {
    use super::*;
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

    /// 启动前检查：找不到 exe → 可读错误；本地模型目录不存在 → 带路径的错误；系统后端不查模型目录；齐全 → 请求。
    #[test]
    fn launch_preconditions() {
        let root = Path::new("D:/data");
        let mut config = DictationConfig::from_document(&ConfigDocument::from_bytes(None));
        let exe = Some(PathBuf::from("D:/stt/snow-stt.exe"));

        // 系统语音：不看模型目录，请求里后端为 System、模型目录为空
        config.backend = Backend::System;
        let (path, sys) = prepare_launch(&config, root, exe.clone(), |_| false).unwrap();
        assert_eq!(Some(path), exe);
        assert_eq!(sys.backend, BackendKind::System);
        assert!(sys.model_dir.is_empty());
        assert_eq!(
            prepare_launch(&config, root, None, |_| true).unwrap_err(),
            Failure::WorkerMissing
        );

        config.backend = Backend::LocalModel;
        assert_eq!(
            prepare_launch(&config, root, None, |_| true).unwrap_err(),
            Failure::WorkerMissing
        );
        match prepare_launch(&config, root, exe.clone(), |_| false).unwrap_err() {
            Failure::ModelDirMissing(path) => assert!(path.contains("models"), "{path}"),
            other => panic!("意外的失败原因：{other:?}"),
        }

        config.language = String::new();
        config.max_seconds = 90;
        let (path, request) = prepare_launch(&config, root, exe.clone(), |_| true).unwrap();
        assert_eq!(Some(path), exe);
        assert_eq!(request.backend, BackendKind::Local);
        assert_eq!(request.language, "auto");
        assert_eq!((request.threads, request.max_seconds), (2, 90));
        assert!(request.model_dir.contains("stt"));
    }
}
