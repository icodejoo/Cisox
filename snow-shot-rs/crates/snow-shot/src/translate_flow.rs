//! 覆盖窗里的“文字翻译”交互状态与展示文案（纯数据，不依赖 GPUI，便于离屏测试）。
//!
//! 状态流转：`Idle → Running → Done | Failed`；缺 onnxruntime 运行时时 `Failed{can_download}` 可按 D 进入
//! `Downloading`，下载完成回到 `Idle`（提示用户再次点击“翻译”）。缺模型、缺 OCR 资产等不能就地解决的
//! 问题只给出清晰说明，绝不显示假译文。

use crate::ocr_client::OcrError;
use crate::ocr_flow::truncate_line;
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

/// 悬停联动要求列表滚动到可视区正中的请求：哪个列表滚到哪一段。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ScrollRequest {
    /// 原文列表要居中的条目（悬停就发生在原文列表里时为 `None`，免得条目在鼠标下跑掉）。
    pub source: Option<usize>,
    /// 译文列表要居中的条目（同理）。
    pub target: Option<usize>,
}

/// 截图上的识别区、原文条目、译文条目三方的悬停联动状态：悬停任一方，另两方对应的一段高亮。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct LinkHover {
    /// 悬停的译文条目（段落下标）。
    paragraph: Option<usize>,
    /// 悬停的原文条目（段落下标）。
    source: Option<usize>,
    /// 悬停的行框下标。
    box_ix: Option<usize>,
    /// 上一次已发出滚动请求的段落；段落没变就不重复滚动。
    scrolled_for: Option<usize>,
}

/// 更新一个悬停槽：进入时记录，离开时只清除自己（避免晚到的“离开”清掉新的“进入”）。
fn set_hover(slot: &mut Option<usize>, index: usize, hovered: bool) {
    if hovered {
        *slot = Some(index);
    } else if *slot == Some(index) {
        *slot = None;
    }
}

impl LinkHover {
    /// 更新译文条目的悬停：进入时记录，离开时只清除自己（避免晚到的“离开”清掉新的“进入”）。
    ///
    /// # 参数
    /// - `index`：段落下标。
    /// - `hovered`：鼠标是进入（`true`）还是离开（`false`）。
    pub fn set_paragraph(&mut self, index: usize, hovered: bool) {
        set_hover(&mut self.paragraph, index, hovered);
    }

    /// 更新原文条目的悬停，规则同 [`Self::set_paragraph`]。
    ///
    /// # 参数
    /// - `index`：段落下标。
    /// - `hovered`：鼠标是进入还是离开。
    pub fn set_source(&mut self, index: usize, hovered: bool) {
        set_hover(&mut self.source, index, hovered);
    }

    /// 更新行框的悬停，规则同 [`Self::set_paragraph`]。
    ///
    /// # 参数
    /// - `index`：行框下标。
    /// - `hovered`：鼠标是进入还是离开。
    pub fn set_box(&mut self, index: usize, hovered: bool) {
        set_hover(&mut self.box_ix, index, hovered);
    }

    /// 清空悬停（界面切换或关闭时用）。
    pub fn clear(&mut self) {
        *self = Self::default();
    }

    /// 当前该高亮的段落：悬停的译文条目、原文条目，或悬停行框所属的段落。
    ///
    /// # 参数
    /// - `pairs`：逐段对照。
    pub fn active_paragraph(&self, pairs: &[TranslatedParagraph]) -> Option<usize> {
        self.paragraph.or(self.source).or_else(|| {
            let ix = self.box_ix?;
            pairs.iter().position(|p| p.box_indices.contains(&ix))
        })
    }

    /// 原文条目是否该高亮。
    ///
    /// # 参数
    /// - `pairs`：逐段对照。
    /// - `index`：段落下标。
    pub fn source_active(&self, pairs: &[TranslatedParagraph], index: usize) -> bool {
        self.active_paragraph(pairs) == Some(index)
    }

    /// 译文条目是否该高亮。
    ///
    /// # 参数
    /// - `pairs`：逐段对照。
    /// - `index`：段落下标。
    pub fn target_active(&self, pairs: &[TranslatedParagraph], index: usize) -> bool {
        self.active_paragraph(pairs) == Some(index)
    }

    /// 某个行框自己是否被悬停（OCR 结果弹窗里一行对应一个框，没有段落对照）。
    ///
    /// # 参数
    /// - `box_ix`：行框下标。
    ///
    /// ```ignore
    /// hover.set_box(2, true);
    /// assert!(hover.box_hovered(2));
    /// ```
    pub fn box_hovered(&self, box_ix: usize) -> bool {
        self.box_ix == Some(box_ix)
    }

    /// 某个行框是否该高亮：它自己被悬停，或它属于被悬停条目对应的段落。
    ///
    /// # 参数
    /// - `pairs`：逐段对照。
    /// - `box_ix`：行框下标。
    pub fn box_active(&self, pairs: &[TranslatedParagraph], box_ix: usize) -> bool {
        self.box_hovered(box_ix)
            || self
                .paragraph
                .or(self.source)
                .and_then(|p| pairs.get(p))
                .is_some_and(|p| p.box_indices.contains(&box_ix))
    }

    /// 取出待执行的滚动请求：高亮段落变了才有，让另外两方列表把该条目滚到可视区正中。
    ///
    /// 悬停发生在哪个列表，就不滚那个列表（它的条目已在鼠标下，再滚会让条目从鼠标下跑掉）；
    /// 悬停在截图识别区上时两个列表都滚。悬停离开后清除记忆，下次再悬停同一段会重新滚动。
    ///
    /// # 参数
    /// - `pairs`：逐段对照。
    ///
    /// # 返回
    /// 无需滚动为 `None`。
    pub fn take_scroll(&mut self, pairs: &[TranslatedParagraph]) -> Option<ScrollRequest> {
        let active = self.active_paragraph(pairs);
        if active == self.scrolled_for {
            return None;
        }
        self.scrolled_for = active;
        let p = active?;
        let from_target = self.paragraph.is_some();
        let from_source = !from_target && self.source.is_some();
        Some(ScrollRequest {
            source: (!from_source).then_some(p),
            target: (!from_target).then_some(p),
        })
    }
}

/// 翻译在覆盖窗里的状态。
#[derive(Debug, Clone, PartialEq)]
pub enum TranslateUiState {
    /// 未启用。
    Idle,
    /// 进行中。
    Running {
        /// 当前阶段（识别阶段在识别区叠扫描动画，翻译阶段不叠）。
        stage: TranslateStage,
        /// 当前阶段的进度文案。
        text: String,
    },
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
        TranslateFlowError::Cancelled => (i18n.tr("translate-flow-cancelled"), false),
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
    /// 进入某个阶段的“进行中”状态，文案取自该阶段。
    ///
    /// # 参数
    /// - `stage`：流程阶段。
    /// - `i18n`：界面语料。
    ///
    /// ```ignore
    /// let state = TranslateUiState::running(TranslateStage::Recognizing, i18n);
    /// assert!(state.is_recognizing());
    /// ```
    pub fn running(stage: TranslateStage, i18n: &I18n) -> Self {
        Self::Running {
            stage,
            text: stage_text(stage, i18n),
        }
    }

    /// 是否处于“识别文字”阶段（此时在识别区叠扫描动画）。
    pub fn is_recognizing(&self) -> bool {
        matches!(
            self,
            Self::Running {
                stage: TranslateStage::Recognizing,
                ..
            }
        )
    }

    /// 是否有翻译界面需要显示（非 Idle）。
    pub fn is_visible(&self) -> bool {
        !matches!(self, Self::Idle)
    }

    /// 是否正忙（翻译或下载中，不接受新的翻译请求）。
    pub fn is_busy(&self) -> bool {
        matches!(self, Self::Running { .. } | Self::Downloading(_))
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
            Self::Running { text: step, .. } | Self::Downloading(step) => Some(step.clone()),
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
        TranslateUiState::Running { text, .. } => vec![text.clone()],
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
                    .map(|l| truncate_line(l, PANEL_MAX_CHARS)),
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
            let mut lines = vec![truncate_line(message, FAILURE_MAX_CHARS)];
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

    /// OCR 弹窗的行悬停：只亮自己对应的框（不依赖段落对照），晚到的“离开”不清新的“进入”。
    #[test]
    fn box_hovered_tracks_only_the_hovered_box() {
        let mut hover = LinkHover::default();
        assert!(!hover.box_hovered(0));
        hover.set_box(0, true);
        assert!(hover.box_hovered(0) && !hover.box_hovered(1));
        hover.set_box(1, true);
        hover.set_box(0, false);
        assert!(hover.box_hovered(1) && !hover.box_hovered(0));
        hover.clear();
        assert!(!hover.box_hovered(1));
    }

    /// 三方联动：悬停原文 / 译文条目 / 识别区任一方，另两方对应高亮；离开后清除。
    #[test]
    fn three_way_hover_highlights_the_other_two() {
        let pairs = two_pairs();
        let mut hover = LinkHover::default();
        // 悬停原文条目 1：译文条目 1 与行框 2 高亮，别的不亮
        hover.set_source(1, true);
        assert!(hover.source_active(&pairs, 1) && hover.target_active(&pairs, 1));
        assert!(hover.box_active(&pairs, 2));
        assert!(!hover.target_active(&pairs, 0) && !hover.box_active(&pairs, 0));
        hover.set_source(1, false);
        assert_eq!(hover.active_paragraph(&pairs), None);
        // 悬停译文条目 0：原文条目 0 与行框 0、1 高亮
        hover.set_paragraph(0, true);
        assert!(hover.source_active(&pairs, 0));
        assert!(hover.box_active(&pairs, 0) && hover.box_active(&pairs, 1));
        hover.set_paragraph(0, false);
        // 悬停识别区行框 1：原文 / 译文条目 0 高亮，同段的行框 0 不跟着亮（只亮自己）
        hover.set_box(1, true);
        assert!(hover.source_active(&pairs, 0) && hover.target_active(&pairs, 0));
        assert!(hover.box_active(&pairs, 1) && !hover.box_active(&pairs, 0));
        hover.set_box(1, false);
        assert!(!hover.source_active(&pairs, 0) && !hover.box_active(&pairs, 1));
    }

    /// 滚动请求：悬停原文只滚译文列表、悬停译文只滚原文列表、悬停识别区两个都滚；
    /// 同一段不重复请求，离开后再进入重新请求。
    #[test]
    fn hover_scroll_requests_skip_the_origin_list() {
        let pairs = two_pairs();
        let mut hover = LinkHover::default();
        assert_eq!(hover.take_scroll(&pairs), None);

        hover.set_source(1, true);
        assert_eq!(
            hover.take_scroll(&pairs),
            Some(ScrollRequest {
                source: None,
                target: Some(1)
            })
        );
        assert_eq!(hover.take_scroll(&pairs), None, "同一段不重复滚动");
        hover.set_source(1, false);
        assert_eq!(hover.take_scroll(&pairs), None, "离开不触发滚动");

        hover.set_paragraph(0, true);
        assert_eq!(
            hover.take_scroll(&pairs),
            Some(ScrollRequest {
                source: Some(0),
                target: None
            })
        );
        hover.set_paragraph(0, false);
        let _ = hover.take_scroll(&pairs);

        hover.set_box(2, true);
        assert_eq!(
            hover.take_scroll(&pairs),
            Some(ScrollRequest {
                source: Some(1),
                target: Some(1)
            })
        );
        // 离开后再次悬停同一个行框，重新请求
        hover.set_box(2, false);
        let _ = hover.take_scroll(&pairs);
        hover.set_box(2, true);
        assert!(hover.take_scroll(&pairs).is_some());
        // 直接换到另一段：立即请求新的一段
        hover.set_box(2, false);
        hover.set_box(0, true);
        assert_eq!(
            hover.take_scroll(&pairs),
            Some(ScrollRequest {
                source: Some(0),
                target: Some(0)
            })
        );
        hover.clear();
        assert_eq!(hover.take_scroll(&pairs), None);
    }

    /// 可见性、忙碌、阶段文案。
    #[test]
    fn visibility_busy_and_stages() {
        assert!(!TranslateUiState::Idle.is_visible());
        assert!(
            TranslateUiState::Running {
                stage: TranslateStage::Recognizing,
                text: "x".into()
            }
            .is_busy()
        );
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
        assert_eq!(truncate_line("abc", 5), "abc");
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
        let (msg, dl) = failure_message(&TranslateFlowError::Cancelled, zh());
        assert!(msg.contains("取消") && !dl);
        let timeout = TranslateUiState::from_error(
            &TranslateFlowError::Translate(TranslateError::Timeout),
            zh(),
        );
        assert_ne!(timeout.status_text(zh()), no_model.status_text(zh()));
        // 英文界面：结构化错误换成英文说明，没有中文残留
        for error in [
            TranslateFlowError::NoText,
            TranslateFlowError::Cancelled,
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

    /// 只有识别阶段的“进行中”才算识别中（驱动扫描动画）。
    #[test]
    fn recognizing_only_in_recognize_stage() {
        let recognizing = TranslateUiState::running(TranslateStage::Recognizing, zh());
        let translating = TranslateUiState::running(TranslateStage::Translating, zh());
        assert!(recognizing.is_recognizing());
        assert!(recognizing.is_busy());
        assert!(!translating.is_recognizing());
        assert!(translating.is_busy());
        assert!(!TranslateUiState::Idle.is_recognizing());
        assert!(!TranslateUiState::Downloading("x".into()).is_recognizing());
        assert_eq!(
            recognizing.status_text(zh()).as_deref(),
            Some(stage_text(TranslateStage::Recognizing, zh()).as_str())
        );
    }

    /// 下载与运行中的文案。
    #[test]
    fn progress_states() {
        assert_eq!(
            TranslateUiState::Running {
                stage: TranslateStage::Translating,
                text: "正在翻译…".into()
            }
            .status_text(zh())
            .as_deref(),
            Some("正在翻译…")
        );
        let d = TranslateUiState::Downloading("正在下载 onnxruntime 运行时…".into());
        assert_eq!(panel_lines(&d, zh()).len(), 2);
        assert!(panel_lines(&TranslateUiState::Idle, zh()).is_empty());
    }
}
