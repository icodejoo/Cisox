//! 覆盖窗里的“文字翻译”交互状态与展示文案（纯数据，不依赖 GPUI，便于离屏测试）。
//!
//! 状态流转：`Idle → Running → Done | Failed`；缺 onnxruntime 运行时时 `Failed{can_download}` 可按 D 进入
//! `Downloading`，下载完成回到 `Idle`（提示用户再次点击“翻译”）。缺模型、缺 OCR 资产等不能就地解决的
//! 问题只给出清晰说明，绝不显示假译文。

use crate::ocr_client::OcrError;
use crate::ocr_service::OcrTextBox;
use crate::translate_service::{
    TranslateFlowError, TranslateOutcome, TranslateStage, TranslatedParagraph,
};
use snow_i18n::{Args, I18n};
use snow_translate::{Lang, TranslateError};

/// 结果面板最多显示的段落数。
pub const PANEL_MAX_PARAGRAPHS: usize = 8;
/// 结果面板每个段落最多显示的字符数。
pub const PANEL_MAX_CHARS: usize = 160;
/// 失败说明最多显示的字符数。
const FAILURE_MAX_CHARS: usize = 320;

/// 段落与行框联动高亮的填充色（半透明黄）。
pub const LINK_FILL: u32 = 0xFADB1473;
/// 段落与行框联动高亮的描边色（黄）。
pub const LINK_BORDER: u32 = 0xFAAD14FF;

/// 译文段落与 OCR 行框的悬停联动状态：鼠标在某段译文或某个行框上。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct LinkHover {
    /// 悬停的译文段落下标。
    paragraph: Option<usize>,
    /// 悬停的行框下标。
    box_ix: Option<usize>,
}

impl LinkHover {
    /// 更新译文段落的悬停：进入时记录，离开时只清除自己（避免晚到的“离开”清掉新的“进入”）。
    ///
    /// # 参数
    /// - `index`：段落下标。
    /// - `hovered`：鼠标是进入（`true`）还是离开（`false`）。
    pub fn set_paragraph(&mut self, index: usize, hovered: bool) {
        if hovered {
            self.paragraph = Some(index);
        } else if self.paragraph == Some(index) {
            self.paragraph = None;
        }
    }

    /// 更新行框的悬停，规则同 [`Self::set_paragraph`]。
    ///
    /// # 参数
    /// - `index`：行框下标。
    /// - `hovered`：鼠标是进入还是离开。
    pub fn set_box(&mut self, index: usize, hovered: bool) {
        if hovered {
            self.box_ix = Some(index);
        } else if self.box_ix == Some(index) {
            self.box_ix = None;
        }
    }

    /// 清空悬停（界面切换或关闭时用）。
    pub fn clear(&mut self) {
        *self = Self::default();
    }

    /// 当前该高亮的译文段落：悬停段落本身，或悬停行框所属的段落。
    ///
    /// # 参数
    /// - `pairs`：逐段对照。
    pub fn active_paragraph(&self, pairs: &[TranslatedParagraph]) -> Option<usize> {
        self.paragraph.or_else(|| {
            let ix = self.box_ix?;
            pairs.iter().position(|p| p.box_indices.contains(&ix))
        })
    }

    /// 某个行框是否该高亮：它属于悬停的段落，或它自己被悬停。
    ///
    /// # 参数
    /// - `pairs`：逐段对照。
    /// - `box_ix`：行框下标。
    pub fn box_active(&self, pairs: &[TranslatedParagraph], box_ix: usize) -> bool {
        self.box_ix == Some(box_ix)
            || self
                .paragraph
                .and_then(|p| pairs.get(p))
                .is_some_and(|p| p.box_indices.contains(&box_ix))
    }
}

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
        /// 原文与译文逐段对照（含行框下标）。
        pairs: Vec<TranslatedParagraph>,
        /// OCR 行框（选区内图像坐标）。
        boxes: Vec<OcrTextBox>,
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
/// - `i18n`：界面语料。
///
/// ```ignore
/// let text = stage_text(TranslateStage::Translating, i18n);
/// ```
pub fn stage_text(stage: TranslateStage, i18n: &I18n) -> String {
    match stage {
        TranslateStage::Recognizing => i18n.tr("translate-flow-stage-recognizing"),
        TranslateStage::Translating => i18n.tr("translate-flow-stage-translating"),
    }
}

/// 语言在界面里的显示名（各语言自称，不随界面语言变化）。
fn lang_name(lang: Lang) -> String {
    crate::language_names::endonym(lang.code())
        .map_or_else(|| lang.code().to_string(), str::to_string)
}

/// 把翻译后端的结构化错误翻成界面语言下的说明（技术细节原样附带）。
///
/// # 参数
/// - `error`：后端错误。
/// - `i18n`：界面语料。
fn translate_error_text(error: &TranslateError, i18n: &I18n) -> String {
    let detail = |id: &str, text: &str| i18n.tr_with(id, &Args::new().named("detail", text));
    match error {
        TranslateError::NoModelFound(d) => detail("translate-flow-error-no-model", d),
        TranslateError::UnsupportedLanguagePair(source, target) => i18n.tr_with(
            "translate-flow-error-pair",
            &Args::new()
                .named("source", lang_name(*source))
                .named("target", lang_name(*target)),
        ),
        TranslateError::InvalidRequest(d) => detail("translate-flow-error-invalid-request", d),
        TranslateError::Network(d) => detail("translate-flow-error-network", d),
        TranslateError::Io(d) => detail("translate-flow-error-io", d),
        TranslateError::Timeout => i18n.tr("translate-flow-error-timeout"),
        TranslateError::RuntimeMissing(d) => detail("translate-flow-error-runtime-missing", d),
        TranslateError::WorkerUnavailable(d) => {
            detail("translate-flow-error-worker-unavailable", d)
        }
        TranslateError::WorkerDied(d) => detail("translate-flow-error-worker-died", d),
        TranslateError::ModelLoad(d) => detail("translate-flow-error-model-load", d),
        TranslateError::Inference(d) => detail("translate-flow-error-inference", d),
        TranslateError::OutOfMemory(d) => detail("translate-flow-error-oom", d),
        TranslateError::NoCustomModel => i18n.tr("translate-flow-error-no-custom-model"),
        TranslateError::CustomModelNotSelected => {
            i18n.tr("translate-flow-error-custom-model-not-selected")
        }
    }
}

/// 把翻译流程的失败转成用户可读说明，并标明能否靠下载运行时解决。
///
/// # 参数
/// - `error`：流程失败原因。
/// - `i18n`：界面语料。
///
/// # 返回
/// `(说明, 是否可按 D 下载运行时)`。
///
/// ```ignore
/// let (msg, can_download) = failure_message(&TranslateFlowError::NoText, i18n);
/// assert!(!can_download && !msg.is_empty());
/// ```
pub fn failure_message(error: &TranslateFlowError, i18n: &I18n) -> (String, bool) {
    match error {
        TranslateFlowError::Ocr(e) => {
            let message = e.message(i18n);
            if matches!(e, OcrError::Unavailable(_)) && e.can_download() {
                (
                    i18n.tr_with(
                        "translate-flow-ocr-hint",
                        &Args::new().named("message", message),
                    ),
                    false,
                )
            } else {
                (message, false)
            }
        }
        TranslateFlowError::NoText => (i18n.tr("translate-flow-no-text"), false),
        TranslateFlowError::AlreadyTarget(lang) => (
            i18n.tr_with(
                "translate-flow-already-target",
                &Args::new().named("lang", lang_name(*lang)),
            ),
            false,
        ),
        TranslateFlowError::Translate(e) => (
            translate_error_text(e, i18n),
            matches!(e, TranslateError::RuntimeMissing(_)),
        ),
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
            boxes: outcome.boxes.clone(),
            label: outcome.label.clone(),
            copied,
        }
    }

    /// 由流程失败得到失败态。
    ///
    /// # 参数
    /// - `error`：失败原因。
    /// - `i18n`：界面语料（在界面边界把结构化错误翻成文案）。
    pub fn from_error(error: &TranslateFlowError, i18n: &I18n) -> Self {
        let (message, can_download) = failure_message(error, i18n);
        Self::Failed {
            message,
            can_download,
        }
    }

    /// 底部状态条文案。
    ///
    /// # 参数
    /// - `i18n`：界面语料。
    pub fn status_text(&self, i18n: &I18n) -> Option<String> {
        match self {
            Self::Idle => None,
            Self::Running(step) | Self::Downloading(step) => Some(step.clone()),
            Self::Done {
                translated,
                copied: true,
                ..
            } if !translated.is_empty() => Some(i18n.tr("translate-flow-done-copied")),
            Self::Done { copied: false, .. } => Some(i18n.tr("translate-flow-done-copy-failed")),
            Self::Done { .. } => Some(i18n.tr("translate-flow-empty")),
            Self::Failed {
                message,
                can_download: true,
            } => Some(i18n.tr_with(
                "translate-flow-press-d",
                &Args::new().named("message", message.as_str()),
            )),
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
/// - `i18n`：界面语料。
///
/// # 返回
/// 空表示不显示面板；否则依次是标题 / 正文 / 操作提示。
///
/// ```ignore
/// assert!(panel_lines(&TranslateUiState::Idle, i18n).is_empty());
/// ```
pub fn panel_lines(state: &TranslateUiState, i18n: &I18n) -> Vec<String> {
    match state {
        TranslateUiState::Idle => Vec::new(),
        TranslateUiState::Running(step) => vec![step.clone()],
        TranslateUiState::Downloading(step) => {
            vec![step.clone(), i18n.tr("translate-flow-downloading-hint")]
        }
        TranslateUiState::Done { translated, .. } if translated.trim().is_empty() => {
            vec![
                i18n.tr("translate-flow-empty"),
                i18n.tr("translate-flow-esc"),
            ]
        }
        TranslateUiState::Done {
            translated,
            pairs,
            label,
            copied,
            ..
        } => {
            let all: Vec<&str> = if pairs.is_empty() {
                translated.lines().collect()
            } else {
                pairs.iter().map(|p| p.translated.as_str()).collect()
            };
            let mut lines = vec![i18n.tr_with(
                "translate-flow-title",
                &Args::new().named("label", label.as_str()),
            )];
            lines.extend(
                all.iter()
                    .take(PANEL_MAX_PARAGRAPHS)
                    .map(|l| truncate_text(l, PANEL_MAX_CHARS)),
            );
            if all.len() > PANEL_MAX_PARAGRAPHS {
                lines.push(i18n.tr_with(
                    "translate-flow-more-paragraphs",
                    &Args::new().named("count", (all.len() - PANEL_MAX_PARAGRAPHS).to_string()),
                ));
            }
            lines.push(i18n.tr(if *copied {
                "translate-flow-footer-copied"
            } else {
                "translate-flow-footer-copy-failed"
            }));
            lines
        }
        TranslateUiState::Failed {
            message,
            can_download,
        } => {
            let mut lines = vec![truncate_text(message, FAILURE_MAX_CHARS)];
            if *can_download {
                lines.push(i18n.tr("translate-flow-download-hint"));
            }
            lines.push(i18n.tr("translate-flow-esc"));
            lines
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ocr_assets::OcrUnavailable;
    use snow_translate::Lang;

    /// 中文语料。
    fn zh() -> &'static I18n {
        crate::ocr_backend::i18n_for("zh-CN")
    }

    /// 英文语料。
    fn en() -> &'static I18n {
        crate::ocr_backend::i18n_for("en-US")
    }

    /// 造翻译产出。
    fn outcome(translated: &[&str]) -> TranslateOutcome {
        TranslateOutcome {
            source: "src".into(),
            translated: translated.join("\n"),
            pairs: translated
                .iter()
                .map(|t| TranslatedParagraph {
                    source: "s".to_string(),
                    translated: (*t).to_string(),
                    box_indices: vec![],
                })
                .collect(),
            boxes: vec![],
            label: "OPUS-MT".into(),
            ocr_ms: 1,
            translate_ms: 2,
        }
    }

    /// 造两段对照：段 0 含行框 0、1，段 1 含行框 2。
    fn two_pairs() -> Vec<TranslatedParagraph> {
        vec![
            TranslatedParagraph {
                source: "a".into(),
                translated: "A".into(),
                box_indices: vec![0, 1],
            },
            TranslatedParagraph {
                source: "b".into(),
                translated: "B".into(),
                box_indices: vec![2],
            },
        ]
    }

    /// 悬停译文段落：高亮该段全部行框；悬停行框：高亮所属段落（与它自己）。
    #[test]
    fn link_hover_maps_both_ways() {
        let pairs = two_pairs();
        let mut hover = LinkHover::default();
        assert_eq!(hover.active_paragraph(&pairs), None);
        hover.set_paragraph(0, true);
        assert!(hover.box_active(&pairs, 0) && hover.box_active(&pairs, 1));
        assert!(!hover.box_active(&pairs, 2));
        hover.set_paragraph(0, false);
        assert!(!hover.box_active(&pairs, 0));

        hover.set_box(2, true);
        assert_eq!(hover.active_paragraph(&pairs), Some(1));
        assert!(hover.box_active(&pairs, 2) && !hover.box_active(&pairs, 0));
        // 晚到的“离开”不会清掉新的悬停
        hover.set_box(1, true);
        hover.set_box(2, false);
        assert_eq!(hover.active_paragraph(&pairs), Some(0));
        hover.clear();
        assert_eq!(hover, LinkHover::default());
    }

    /// 可见性、忙碌、阶段文案。
    #[test]
    fn visibility_busy_and_stages() {
        assert!(!TranslateUiState::Idle.is_visible());
        assert!(TranslateUiState::Running("x".into()).is_busy());
        assert!(TranslateUiState::Downloading("x".into()).is_busy());
        assert!(!TranslateUiState::from_outcome(&outcome(&["a"]), true).is_busy());
        assert!(TranslateUiState::Idle.status_text(zh()).is_none());
        assert_ne!(
            stage_text(TranslateStage::Recognizing, zh()),
            stage_text(TranslateStage::Translating, zh())
        );
    }

    /// 完成态：状态条、面板标题含模型名、提示随复制结果变化。
    #[test]
    fn done_state_texts() {
        let done = TranslateUiState::from_outcome(&outcome(&["你好", "再见"]), true);
        assert_eq!(
            done.status_text(zh()).as_deref(),
            Some("已翻译，译文已复制到剪贴板")
        );
        let lines = panel_lines(&done, zh());
        assert_eq!(lines[0], "译文 · OPUS-MT");
        assert_eq!(&lines[1..3], ["你好", "再见"]);
        assert!(lines.last().is_some_and(|l| l.contains("Enter")));
        let failed_copy = TranslateUiState::from_outcome(&outcome(&["a"]), false);
        assert!(
            failed_copy
                .status_text(zh())
                .is_some_and(|s| s.contains("失败"))
        );
        assert!(
            panel_lines(&failed_copy, zh())
                .last()
                .is_some_and(|l| l.contains("重试"))
        );
        let empty = TranslateUiState::from_outcome(&outcome(&[""]), true);
        assert_eq!(empty.status_text(zh()).as_deref(), Some("译文为空"));
        assert_eq!(
            empty.status_text(en()).as_deref(),
            Some("The translation is empty")
        );
    }

    /// 面板段落数与长度受限。
    #[test]
    fn panel_is_bounded() {
        let many: Vec<String> = (0..12).map(|i| format!("段{i}")).collect();
        let refs: Vec<&str> = many.iter().map(String::as_str).collect();
        let lines = panel_lines(&TranslateUiState::from_outcome(&outcome(&refs), true), zh());
        assert_eq!(lines.len(), PANEL_MAX_PARAGRAPHS + 3);
        assert!(lines[PANEL_MAX_PARAGRAPHS + 1].contains("另有 4 段"));
        let long = "长".repeat(PANEL_MAX_CHARS + 20);
        let lines = panel_lines(
            &TranslateUiState::from_outcome(&outcome(&[long.as_str()]), true),
            zh(),
        );
        assert_eq!(lines[1].chars().count(), PANEL_MAX_CHARS + 1);
        assert_eq!(truncate_text("abc", 5), "abc");
    }

    /// 各类失败文案不同：缺运行时可下载，其余不可；OCR 缺资产指引去点 OCR 按钮。
    #[test]
    fn failure_messages_are_distinct() {
        let runtime = TranslateUiState::from_error(
            &TranslateFlowError::Translate(TranslateError::RuntimeMissing(
                "the onnxruntime runtime is not installed".into(),
            )),
            zh(),
        );
        assert!(matches!(
            runtime,
            TranslateUiState::Failed {
                can_download: true,
                ..
            }
        ));
        assert!(
            runtime
                .status_text(zh())
                .is_some_and(|s| s.contains("按 D 下载"))
        );
        assert!(
            panel_lines(&runtime, zh())
                .iter()
                .any(|l| l.contains("14 MB"))
        );
        let no_model = TranslateUiState::from_error(
            &TranslateFlowError::Translate(TranslateError::NoModelFound(
                "no usable translation model in D:/m".into(),
            )),
            zh(),
        );
        assert!(matches!(
            no_model,
            TranslateUiState::Failed {
                can_download: false,
                ..
            }
        ));
        assert!(
            no_model
                .status_text(zh())
                .is_some_and(|s| s.contains("D:/m"))
        );
        let ocr = TranslateUiState::from_error(
            &TranslateFlowError::Ocr(OcrError::Unavailable(OcrUnavailable::NoRuntime)),
            zh(),
        );
        assert!(
            ocr.status_text(zh())
                .is_some_and(|s| s.contains("OCR") && !s.contains("按 D 下载 ·"))
        );
        let (msg, dl) = failure_message(&TranslateFlowError::AlreadyTarget(Lang::ZhHans), zh());
        assert!(msg.contains("简体中文") && !dl);
        let (msg, _) = failure_message(&TranslateFlowError::NoText, zh());
        assert!(msg.contains("文字"));
        let timeout = TranslateUiState::from_error(
            &TranslateFlowError::Translate(TranslateError::Timeout),
            zh(),
        );
        assert_ne!(timeout.status_text(zh()), no_model.status_text(zh()));
        // 英文界面：结构化错误换成英文说明，没有中文残留
        for error in [
            TranslateFlowError::NoText,
            TranslateFlowError::Translate(TranslateError::NoCustomModel),
            TranslateFlowError::Translate(TranslateError::CustomModelNotSelected),
            TranslateFlowError::Translate(TranslateError::UnsupportedLanguagePair(
                Lang::En,
                Lang::Ja,
            )),
            TranslateFlowError::Translate(TranslateError::Timeout),
        ] {
            let (text, _) = failure_message(&error, en());
            assert!(text.is_ascii() || text.contains("日本語"), "{text}");
            assert!(!text.contains("[!"), "{text}");
        }
    }

    /// 下载与运行中的文案。
    #[test]
    fn progress_states() {
        assert_eq!(
            TranslateUiState::Running("正在翻译…".into())
                .status_text(zh())
                .as_deref(),
            Some("正在翻译…")
        );
        let d = TranslateUiState::Downloading("正在下载 onnxruntime 运行时…".into());
        assert_eq!(panel_lines(&d, zh()).len(), 2);
        assert!(panel_lines(&TranslateUiState::Idle, zh()).is_empty());
    }
}
