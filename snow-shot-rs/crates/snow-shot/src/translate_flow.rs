//! 覆盖窗里的“文字翻译”交互状态与展示文案（纯数据，不依赖 GPUI，便于离屏测试）。
//!
//! 状态流转：`Idle → Running → Done | Failed`；缺 onnxruntime 运行时时 `Failed{can_download}` 可按 D 进入
//! `Downloading`，下载完成回到 `Idle`（提示用户再次点击“翻译”）。缺模型、缺 OCR 资产等不能就地解决的
//! 问题只给出清晰说明，绝不显示假译文。

use crate::ocr_client::OcrError;
use crate::translate_service::{TranslateFlowError, TranslateOutcome, TranslateStage};
use snow_translate::TranslateError;

/// 结果面板最多显示的段落数。
pub const PANEL_MAX_PARAGRAPHS: usize = 8;
/// 结果面板每个段落最多显示的字符数。
pub const PANEL_MAX_CHARS: usize = 160;
/// 失败说明最多显示的字符数。
const FAILURE_MAX_CHARS: usize = 320;

/// 翻译在覆盖窗里的状态。
#[derive(Debug, Clone, PartialEq)]
pub enum TranslateUiState {
    /// 未启用。
    Idle,
    /// 进行中（附当前阶段文案）。
    Running(String),
    /// 完成。
    Done {
        /// 原文（按段落）。
        source: String,
        /// 译文（按段落，行间 `\n`）。
        translated: String,
        /// 原文与译文逐段对照。
        pairs: Vec<(String, String)>,
        /// 后端与模型展示名。
        label: String,
        /// 译文是否已复制到剪贴板。
        copied: bool,
    },
    /// 失败。
    Failed {
        /// 用户可读的原因。
        message: String,
        /// 是否可以按 D 下载运行时。
        can_download: bool,
    },
    /// 正在下载 onnxruntime 运行时。
    Downloading(String),
}

/// 阶段对应的进度文案。
///
/// # 参数
/// - `stage`：流程阶段。
///
/// ```ignore
/// assert_eq!(stage_text(TranslateStage::Translating), "正在翻译…");
/// ```
pub fn stage_text(stage: TranslateStage) -> &'static str {
    match stage {
        TranslateStage::Recognizing => "正在识别文字…",
        TranslateStage::Translating => "正在翻译…（首次会加载模型，稍等片刻）",
    }
}

/// 把翻译流程的失败转成用户可读说明，并标明能否靠下载运行时解决。
///
/// # 参数
/// - `error`：流程失败原因。
///
/// # 返回
/// `(说明, 是否可按 D 下载运行时)`。
///
/// ```ignore
/// let (msg, can_download) = failure_message(&TranslateFlowError::NoText);
/// assert!(!can_download && msg.contains("文字"));
/// ```
pub fn failure_message(error: &TranslateFlowError) -> (String, bool) {
    match error {
        TranslateFlowError::Ocr(e) => {
            let message = e.message();
            if matches!(e, OcrError::Unavailable(_)) && e.can_download() {
                (format!("{message}。请先点击“OCR”按钮并按 D 下载 OCR 组件，再回来翻译"), false)
            } else {
                (message, false)
            }
        }
        TranslateFlowError::NoText => ("未识别到可翻译的文字".to_string(), false),
        TranslateFlowError::AlreadyTarget(lang) => (
            format!("原文已经是{}，无需翻译（可在设置里更改目标语言）", lang.display_name()),
            false,
        ),
        TranslateFlowError::Translate(e) => (e.to_string(), matches!(e, TranslateError::RuntimeMissing(_))),
    }
}

impl TranslateUiState {
    /// 是否有翻译界面需要显示（非 Idle）。
    pub fn is_visible(&self) -> bool {
        !matches!(self, Self::Idle)
    }

    /// 是否正忙（翻译或下载中，不接受新的翻译请求）。
    pub fn is_busy(&self) -> bool {
        matches!(self, Self::Running(_) | Self::Downloading(_))
    }

    /// 由流程产出得到完成态。
    ///
    /// # 参数
    /// - `outcome`：翻译结果。
    /// - `copied`：译文是否已复制到剪贴板。
    pub fn from_outcome(outcome: &TranslateOutcome, copied: bool) -> Self {
        Self::Done {
            source: outcome.source.clone(),
            translated: outcome.translated.clone(),
            pairs: outcome.pairs.clone(),
            label: outcome.label.clone(),
            copied,
        }
    }

    /// 由流程失败得到失败态。
    ///
    /// # 参数
    /// - `error`：失败原因。
    pub fn from_error(error: &TranslateFlowError) -> Self {
        let (message, can_download) = failure_message(error);
        Self::Failed { message, can_download }
    }

    /// 底部状态条文案。
    pub fn status_text(&self) -> Option<String> {
        match self {
            Self::Idle => None,
            Self::Running(step) | Self::Downloading(step) => Some(step.clone()),
            Self::Done { translated, copied: true, .. } if !translated.is_empty() => {
                Some("已翻译，译文已复制到剪贴板".to_string())
            }
            Self::Done { copied: false, .. } => Some("已翻译（复制到剪贴板失败）".to_string()),
            Self::Done { .. } => Some("译文为空".to_string()),
            Self::Failed { message, can_download: true } => Some(format!("{message} · 按 D 下载")),
            Self::Failed { message, .. } => Some(message.clone()),
        }
    }
}

/// 把文字截到指定字符数（超出加省略号）。
///
/// # 参数
/// - `text`：原文。
/// - `max_chars`：最多字符数。
pub fn truncate_text(text: &str, max_chars: usize) -> String {
    if text.chars().count() <= max_chars {
        text.to_string()
    } else {
        let head: String = text.chars().take(max_chars).collect();
        format!("{head}…")
    }
}

/// 结果面板要显示的文字行。
///
/// # 参数
/// - `state`：翻译状态。
///
/// # 返回
/// 空表示不显示面板；否则依次是标题 / 正文 / 操作提示。
///
/// ```ignore
/// assert!(panel_lines(&TranslateUiState::Idle).is_empty());
/// ```
pub fn panel_lines(state: &TranslateUiState) -> Vec<String> {
    match state {
        TranslateUiState::Idle => Vec::new(),
        TranslateUiState::Running(step) => vec![step.clone()],
        TranslateUiState::Downloading(step) => vec![step.clone(), "下载完成后请再次点击“翻译”".to_string()],
        TranslateUiState::Done { translated, .. } if translated.trim().is_empty() => {
            vec!["译文为空".to_string(), "Esc 返回".to_string()]
        }
        TranslateUiState::Done { translated, label, copied, .. } => {
            let all: Vec<&str> = translated.lines().collect();
            let mut lines = vec![format!("译文 · {label}")];
            lines.extend(all.iter().take(PANEL_MAX_PARAGRAPHS).map(|l| truncate_text(l, PANEL_MAX_CHARS)));
            if all.len() > PANEL_MAX_PARAGRAPHS {
                lines.push(format!("…（另有 {} 段）", all.len() - PANEL_MAX_PARAGRAPHS));
            }
            lines.push(
                if *copied {
                    "已复制 · Enter 复制并关闭 · Esc 返回"
                } else {
                    "复制失败 · Enter 重试并关闭 · Esc 返回"
                }
                .to_string(),
            );
            lines
        }
        TranslateUiState::Failed { message, can_download } => {
            let mut lines = vec![truncate_text(message, FAILURE_MAX_CHARS)];
            if *can_download {
                lines.push("按 D 下载 onnxruntime 运行时（约 14 MB，官方发布并校验哈希）".to_string());
            }
            lines.push("Esc 返回".to_string());
            lines
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ocr_assets::OcrUnavailable;
    use snow_translate::Lang;

    /// 造翻译产出。
    fn outcome(translated: &[&str]) -> TranslateOutcome {
        TranslateOutcome {
            source: "src".into(),
            translated: translated.join("\n"),
            pairs: translated.iter().map(|t| ("s".to_string(), (*t).to_string())).collect(),
            label: "OPUS-MT".into(),
            ocr_ms: 1,
            translate_ms: 2,
        }
    }

    /// 可见性、忙碌、阶段文案。
    #[test]
    fn visibility_busy_and_stages() {
        assert!(!TranslateUiState::Idle.is_visible());
        assert!(TranslateUiState::Running("x".into()).is_busy());
        assert!(TranslateUiState::Downloading("x".into()).is_busy());
        assert!(!TranslateUiState::from_outcome(&outcome(&["a"]), true).is_busy());
        assert!(TranslateUiState::Idle.status_text().is_none());
        assert_ne!(stage_text(TranslateStage::Recognizing), stage_text(TranslateStage::Translating));
    }

    /// 完成态：状态条、面板标题含模型名、提示随复制结果变化。
    #[test]
    fn done_state_texts() {
        let done = TranslateUiState::from_outcome(&outcome(&["你好", "再见"]), true);
        assert_eq!(done.status_text().as_deref(), Some("已翻译，译文已复制到剪贴板"));
        let lines = panel_lines(&done);
        assert_eq!(lines[0], "译文 · OPUS-MT");
        assert_eq!(&lines[1..3], ["你好", "再见"]);
        assert!(lines.last().is_some_and(|l| l.contains("Enter")));
        let failed_copy = TranslateUiState::from_outcome(&outcome(&["a"]), false);
        assert!(failed_copy.status_text().is_some_and(|s| s.contains("失败")));
        assert!(panel_lines(&failed_copy).last().is_some_and(|l| l.contains("重试")));
        let empty = TranslateUiState::from_outcome(&outcome(&[""]), true);
        assert_eq!(empty.status_text().as_deref(), Some("译文为空"));
    }

    /// 面板段落数与长度受限。
    #[test]
    fn panel_is_bounded() {
        let many: Vec<String> = (0..12).map(|i| format!("段{i}")).collect();
        let refs: Vec<&str> = many.iter().map(String::as_str).collect();
        let lines = panel_lines(&TranslateUiState::from_outcome(&outcome(&refs), true));
        assert_eq!(lines.len(), PANEL_MAX_PARAGRAPHS + 3);
        assert!(lines[PANEL_MAX_PARAGRAPHS + 1].contains("另有 4 段"));
        let long = "长".repeat(PANEL_MAX_CHARS + 20);
        let lines = panel_lines(&TranslateUiState::from_outcome(&outcome(&[long.as_str()]), true));
        assert_eq!(lines[1].chars().count(), PANEL_MAX_CHARS + 1);
        assert_eq!(truncate_text("abc", 5), "abc");
    }

    /// 各类失败文案不同：缺运行时可下载，其余不可；OCR 缺资产指引去点 OCR 按钮。
    #[test]
    fn failure_messages_are_distinct() {
        let runtime = TranslateUiState::from_error(&TranslateFlowError::Translate(TranslateError::RuntimeMissing(
            "未安装 onnxruntime 运行时，请先下载".into(),
        )));
        assert!(matches!(runtime, TranslateUiState::Failed { can_download: true, .. }));
        assert!(runtime.status_text().is_some_and(|s| s.contains("按 D 下载")));
        assert!(panel_lines(&runtime).iter().any(|l| l.contains("14 MB")));
        let no_model = TranslateUiState::from_error(&TranslateFlowError::Translate(TranslateError::NoModelFound(
            "模型目录 D:/m 里没有可用的翻译模型".into(),
        )));
        assert!(matches!(no_model, TranslateUiState::Failed { can_download: false, .. }));
        assert!(no_model.status_text().is_some_and(|s| s.contains("D:/m")));
        let ocr = TranslateUiState::from_error(&TranslateFlowError::Ocr(OcrError::Unavailable(OcrUnavailable::NoRuntime)));
        assert!(ocr.status_text().is_some_and(|s| s.contains("OCR") && !s.contains("按 D 下载 ·")));
        let (msg, dl) = failure_message(&TranslateFlowError::AlreadyTarget(Lang::ZhHans));
        assert!(msg.contains("简体中文") && !dl);
        let (msg, _) = failure_message(&TranslateFlowError::NoText);
        assert!(msg.contains("文字"));
        let timeout = TranslateUiState::from_error(&TranslateFlowError::Translate(TranslateError::Timeout));
        assert_ne!(timeout.status_text(), no_model.status_text());
    }

    /// 下载与运行中的文案。
    #[test]
    fn progress_states() {
        assert_eq!(
            TranslateUiState::Running("正在翻译…".into()).status_text().as_deref(),
            Some("正在翻译…")
        );
        let d = TranslateUiState::Downloading("正在下载 onnxruntime 运行时…".into());
        assert_eq!(panel_lines(&d).len(), 2);
        assert!(panel_lines(&TranslateUiState::Idle).is_empty());
    }
}
