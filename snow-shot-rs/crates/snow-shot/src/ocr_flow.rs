//! 覆盖窗里的 OCR 交互状态与展示文案（纯数据，不依赖 GPUI，便于离屏测试）。
//!
//! 状态流转：`Idle → Running → Done | Failed`；缺资产时 `Failed{can_download}` 可按 D 进入
//! `Downloading`，下载完成回到 `Idle`（提示用户再次点击 OCR）。

use crate::ocr_client::OcrError;
use crate::ocr_service::{OcrResult, OcrTextBox};
use snow_i18n::{Args, I18n};

/// 文字识别完成后的自动动作（配置 `screenshot/auto_execute_after_text_recognition`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum OcrAutoAction {
    /// 什么都不做（识别结果留在面板里，按 Enter 才复制）。
    #[default]
    NoAction,
    /// 复制识别文本。
    CopyText,
    /// 复制识别文本并结束截图。
    CopyTextAndEnd,
    /// 仅“快速文字识别”时复制（本程序没有快速识别入口，等同什么都不做）。
    QuickCopyText,
    /// 仅“快速文字识别”时复制并结束（同上）。
    QuickCopyTextAndEnd,
    /// 打开可编辑的识别结果窗。
    EnableEditMode,
}

impl OcrAutoAction {
    /// 解析配置值。
    ///
    /// # 参数
    /// - `value`：配置里的字符串。
    ///
    /// # 返回
    /// 对应动作；不认识的值返回 `None`。
    ///
    /// ```ignore
    /// assert_eq!(OcrAutoAction::parse("copy_text"), Some(OcrAutoAction::CopyText));
    /// ```
    pub fn parse(value: &str) -> Option<Self> {
        Some(match value {
            "no_action" => Self::NoAction,
            "copy_text" => Self::CopyText,
            "copy_text_and_end_screenshot" => Self::CopyTextAndEnd,
            "quick_copy_text" => Self::QuickCopyText,
            "quick_copy_text_and_end_screenshot" => Self::QuickCopyTextAndEnd,
            "enable_edit_mode" => Self::EnableEditMode,
            _ => return None,
        })
    }

    /// 识别完成后是否自动复制文本（快速变体只在快速识别时生效，这里恒为否）。
    pub fn copies(self) -> bool {
        matches!(self, Self::CopyText | Self::CopyTextAndEnd)
    }

    /// 复制成功后是否结束截图。
    pub fn ends_screenshot(self) -> bool {
        matches!(self, Self::CopyTextAndEnd)
    }
}

/// 结果弹窗里失败说明最多显示的字符数的基数（失败说明取它的两倍）。
pub const PANEL_MAX_CHARS: usize = 56;

/// OCR 在覆盖窗里的状态。
#[derive(Debug, Clone, PartialEq)]
pub enum OcrUiState {
    /// 未启用。
    Idle,
    /// 识别进行中。
    Running,
    /// 识别完成。
    Done {
        /// 按行拼接的完整文本（可能为空）。
        text: String,
        /// 文本块（选区内图像坐标）。
        boxes: Vec<OcrTextBox>,
        /// 是否已成功复制到剪贴板。
        copied: bool,
        /// 按配置没有自动复制（`copied` 为 false 但不是失败；按 Enter 才复制）。
        skipped: bool,
    },
    /// 识别失败（含资产缺失）。
    Failed {
        /// 用户可读的原因。
        message: String,
        /// 是否可以按 D 触发下载。
        can_download: bool,
    },
    /// 正在下载 OCR 组件。
    Downloading(String),
}

impl OcrUiState {
    /// 是否有 OCR 界面需要显示（非 Idle）。
    pub fn is_visible(&self) -> bool {
        !matches!(self, Self::Idle)
    }

    /// 是否正忙（识别或下载中，不接受新的 OCR 请求）。
    pub fn is_busy(&self) -> bool {
        matches!(self, Self::Running | Self::Downloading(_))
    }

    /// 由识别结果得到完成态（文本为空表示“未识别到文字”）。
    ///
    /// # 参数
    /// - `result`：识别结果。
    /// - `copied`：文本是否已复制到剪贴板。
    pub fn from_result(result: &OcrResult, copied: bool) -> Self {
        Self::Done {
            text: result.full_text.clone(),
            boxes: result.boxes.clone(),
            copied,
            skipped: false,
        }
    }

    /// 把完成态标成“按配置没有自动复制”（不是复制失败）；其它状态原样返回。
    ///
    /// # 返回
    /// `Done` 时 `copied = false`、`skipped = true`。
    pub fn without_auto_copy(self) -> Self {
        match self {
            Self::Done { text, boxes, .. } => Self::Done {
                text,
                boxes,
                copied: false,
                skipped: true,
            },
            other => other,
        }
    }

    /// 由错误得到失败态。
    ///
    /// # 参数
    /// - `error`：识别错误。
    /// - `i18n`：界面语料（在界面边界把结构化错误翻成文案）。
    pub fn from_error(error: &OcrError, i18n: &I18n) -> Self {
        Self::Failed {
            message: error.message(i18n),
            can_download: error.can_download(),
        }
    }

    /// 底部状态条文案（识别中没有）。
    ///
    /// # 参数
    /// - `i18n`：界面语料。
    pub fn status_text(&self, i18n: &I18n) -> Option<String> {
        let count = |boxes: &[OcrTextBox]| Args::new().named("count", boxes.len().to_string());
        match self {
            // 识别中只有扫描动画，不出任何等待文字
            Self::Idle | Self::Running => None,
            Self::Done { text, .. } if text.is_empty() => Some(i18n.tr("ocr-panel-empty")),
            Self::Done {
                boxes,
                copied: true,
                ..
            } => Some(i18n.tr_with("ocr-panel-done-copied", &count(boxes))),
            Self::Done {
                boxes,
                skipped: true,
                ..
            } => Some(i18n.tr_with("ocr-panel-done-not-copied", &count(boxes))),
            Self::Done {
                boxes,
                copied: false,
                ..
            } => Some(i18n.tr_with("ocr-panel-done-copy-failed", &count(boxes))),
            Self::Failed {
                message,
                can_download: true,
            } => Some(i18n.tr_with(
                "ocr-panel-press-d",
                &Args::new().named("message", message.as_str()),
            )),
            Self::Failed { message, .. } => Some(message.clone()),
            Self::Downloading(step) => Some(step.clone()),
        }
    }
}

/// 把一行文字截到面板宽度（按字符计，超出加省略号；OCR 与翻译面板共用）。
///
/// # 参数
/// - `line`：原文。
/// - `max_chars`：最多字符数。
pub fn truncate_line(line: &str, max_chars: usize) -> String {
    match line.char_indices().nth(max_chars) {
        Some((end, _)) => format!("{}…", &line[..end]),
        None => line.to_string(),
    }
}

/// 结果弹窗内容区要显示的内容。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PopupBody {
    /// 每个文本框一行（下标与 `boxes` 一一对应，悬停某行高亮截图上对应的识别框）。
    Boxes(Vec<String>),
    /// 只有文本没有文本框（如 LaTeX / 表格模式），按文本行显示，无高亮。
    Text(Vec<String>),
    /// 提示文字（未识别到文字 / 失败 / 下载中），无高亮。
    Notice(Vec<String>),
}

impl PopupBody {
    /// 内容区的所有行文字（估算弹窗高度用）。
    pub fn lines(&self) -> &[String] {
        match self {
            Self::Boxes(lines) | Self::Text(lines) | Self::Notice(lines) => lines,
        }
    }
}

/// 结果弹窗内容区的内容。
///
/// # 参数
/// - `state`：OCR 状态。
/// - `i18n`：界面语料。
///
/// # 返回
/// `None` 表示不显示弹窗（未启用，或识别进行中只有扫描动画）。
///
/// ```ignore
/// assert!(popup_body(&OcrUiState::Running, i18n).is_none());
/// ```
pub fn popup_body(state: &OcrUiState, i18n: &I18n) -> Option<PopupBody> {
    Some(match state {
        OcrUiState::Idle | OcrUiState::Running => return None,
        OcrUiState::Downloading(step) => {
            PopupBody::Notice(vec![step.clone(), i18n.tr("ocr-panel-downloading-hint")])
        }
        OcrUiState::Done { text, .. } if text.is_empty() => {
            PopupBody::Notice(vec![i18n.tr("ocr-panel-empty"), i18n.tr("ocr-panel-esc")])
        }
        OcrUiState::Done { boxes, .. } if !boxes.is_empty() => {
            PopupBody::Boxes(boxes.iter().map(|b| b.text.clone()).collect())
        }
        OcrUiState::Done { text, .. } => {
            PopupBody::Text(text.lines().map(str::to_string).collect())
        }
        OcrUiState::Failed {
            message,
            can_download,
        } => {
            let mut lines = vec![truncate_line(message, PANEL_MAX_CHARS * 2)];
            if *can_download {
                lines.push(i18n.tr("ocr-panel-download-hint"));
            }
            lines.push(i18n.tr("ocr-panel-esc"));
            PopupBody::Notice(lines)
        }
    })
}

/// 弹窗的“复制”是否可用：识别完成且有文字。
///
/// # 参数
/// - `state`：OCR 状态。
///
/// ```ignore
/// assert!(!popup_copyable(&OcrUiState::Idle));
/// ```
pub fn popup_copyable(state: &OcrUiState) -> bool {
    matches!(state, OcrUiState::Done { text, .. } if !text.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ocr_assets::OcrUnavailable;
    use snow_ui::shell::geometry::PhysicalRect;

    /// 中文语料。
    fn zh() -> &'static I18n {
        crate::ocr_backend::i18n_for("zh-CN")
    }

    /// 英文语料。
    fn en() -> &'static I18n {
        crate::ocr_backend::i18n_for("en-US")
    }

    /// 构造识别结果。
    fn result(lines: &[&str]) -> OcrResult {
        let boxes: Vec<OcrTextBox> = lines
            .iter()
            .enumerate()
            .map(|(i, t)| OcrTextBox {
                rect: PhysicalRect::new(0, i as i32 * 20, 100, 18),
                text: (*t).to_string(),
                confidence: Some(0.9),
            })
            .collect();
        OcrResult {
            full_text: lines.join("\n"),
            boxes,
            elapsed_ms: 1,
            table: None,
            latex: None,
        }
    }

    /// 状态可见性与忙碌判定。
    #[test]
    fn visibility_and_busy() {
        assert!(!OcrUiState::Idle.is_visible());
        assert!(OcrUiState::Running.is_visible() && OcrUiState::Running.is_busy());
        assert!(OcrUiState::Downloading("x".into()).is_busy());
        assert!(!OcrUiState::from_result(&result(&["a"]), true).is_busy());
        assert!(OcrUiState::Idle.status_text(zh()).is_none());
    }

    /// 结果态：有文字 / 无文字的状态条与面板。
    #[test]
    fn done_state_texts() {
        let done = OcrUiState::from_result(&result(&["第一行", "second"]), true);
        assert_eq!(
            done.status_text(zh()).as_deref(),
            Some("已识别 2 行，文字已复制到剪贴板")
        );
        // 有文本框：每个框一行，原样不截断
        assert_eq!(
            popup_body(&done, zh()),
            Some(PopupBody::Boxes(vec!["第一行".into(), "second".into()]))
        );
        let failed_copy = OcrUiState::from_result(&result(&["a"]), false);
        assert!(
            failed_copy
                .status_text(zh())
                .is_some_and(|s| s.contains("失败"))
        );
        let empty = OcrUiState::from_result(&result(&[]), true);
        assert_eq!(empty.status_text(zh()).as_deref(), Some("未识别到文字"));
        assert_eq!(popup_body(&empty, zh()).unwrap().lines()[0], "未识别到文字");
        assert_eq!(empty.status_text(en()).as_deref(), Some("No text found"));
    }

    /// 内容区不截断也不限行数（弹窗内可滚动）；没有文本框时按文本行显示；失败说明仍有长度上限。
    #[test]
    fn popup_body_variants() {
        let many: Vec<String> = (0..12).map(|i| format!("line{i}")).collect();
        let refs: Vec<&str> = many.iter().map(String::as_str).collect();
        let done = OcrUiState::from_result(&result(&refs), true);
        assert_eq!(popup_body(&done, zh()).unwrap().lines().len(), 12);
        let long = "长".repeat(PANEL_MAX_CHARS * 3);
        let done = OcrUiState::from_result(&result(&[long.as_str()]), true);
        assert_eq!(popup_body(&done, zh()).unwrap().lines()[0], long);
        // LaTeX / 表格：只有文本没有文本框
        let text_only = OcrUiState::Done {
            text: "a\nb".to_string(),
            boxes: Vec::new(),
            copied: false,
            skipped: true,
        };
        assert_eq!(
            popup_body(&text_only, zh()),
            Some(PopupBody::Text(vec!["a".into(), "b".into()]))
        );
        let failed = OcrUiState::Failed {
            message: "长".repeat(PANEL_MAX_CHARS * 3),
            can_download: false,
        };
        let body = popup_body(&failed, zh()).unwrap();
        assert_eq!(body.lines()[0].chars().count(), PANEL_MAX_CHARS * 2 + 1);
        assert_eq!(truncate_line("abc", 5), "abc");
    }

    /// 识别中与未启用不出弹窗；“复制”只在有文字的完成态可用。
    #[test]
    fn popup_hidden_while_running_and_copy_gate() {
        assert_eq!(popup_body(&OcrUiState::Idle, zh()), None);
        assert_eq!(popup_body(&OcrUiState::Running, zh()), None);
        assert_eq!(popup_body(&OcrUiState::Running, en()), None);
        assert!(popup_copyable(&OcrUiState::from_result(
            &result(&["a"]),
            true
        )));
        assert!(!popup_copyable(&OcrUiState::from_result(
            &result(&[]),
            true
        )));
        assert!(!popup_copyable(&OcrUiState::Running));
        assert!(!popup_copyable(&OcrUiState::Downloading("x".into())));
        assert!(!popup_copyable(&OcrUiState::Failed {
            message: "x".into(),
            can_download: false,
        }));
    }

    /// 各类失败给出不同文案；缺资产时提示按 D 下载，其它失败不提示。
    #[test]
    fn failure_states() {
        let no_runtime =
            OcrUiState::from_error(&OcrError::Unavailable(OcrUnavailable::NoRuntime), zh());
        assert!(
            no_runtime
                .status_text(zh())
                .is_some_and(|s| s.contains("按 D 下载"))
        );
        assert!(
            popup_body(&no_runtime, zh())
                .unwrap()
                .lines()
                .iter()
                .any(|l| l.contains("按 D 下载"))
        );
        let no_runtime_en =
            OcrUiState::from_error(&OcrError::Unavailable(OcrUnavailable::NoRuntime), en());
        assert!(
            popup_body(&no_runtime_en, en())
                .unwrap()
                .lines()
                .iter()
                .all(|l| l.is_ascii())
        );
        let died = OcrUiState::from_error(&OcrError::ProcessDied("crash".into()), zh());
        assert!(
            died.status_text(zh())
                .is_some_and(|s| s.contains("crash") && !s.contains("按 D"))
        );
        assert!(
            !popup_body(&died, zh())
                .unwrap()
                .lines()
                .iter()
                .any(|l| l.contains("按 D"))
        );
        assert_ne!(no_runtime.status_text(zh()), died.status_text(zh()));
    }

    /// 下载中有进度文案；运行中没有任何等待文字。
    #[test]
    fn progress_states() {
        assert_eq!(OcrUiState::Running.status_text(zh()), None);
        assert_eq!(OcrUiState::Running.status_text(en()), None);
        let d = OcrUiState::Downloading("正在下载 OCR 模型 (1/3)…".into());
        assert_eq!(
            d.status_text(zh()).as_deref(),
            Some("正在下载 OCR 模型 (1/3)…")
        );
        assert_eq!(popup_body(&d, zh()).unwrap().lines().len(), 2);
    }

    /// 自动动作配置：六个值都能解析，只有“复制”两种会自动复制，只有“复制并结束”会结束截图。
    #[test]
    fn auto_action_parses_all_values() {
        let table = [
            ("no_action", OcrAutoAction::NoAction, false, false),
            ("copy_text", OcrAutoAction::CopyText, true, false),
            (
                "copy_text_and_end_screenshot",
                OcrAutoAction::CopyTextAndEnd,
                true,
                true,
            ),
            (
                "quick_copy_text",
                OcrAutoAction::QuickCopyText,
                false,
                false,
            ),
            (
                "quick_copy_text_and_end_screenshot",
                OcrAutoAction::QuickCopyTextAndEnd,
                false,
                false,
            ),
            (
                "enable_edit_mode",
                OcrAutoAction::EnableEditMode,
                false,
                false,
            ),
        ];
        for (value, action, copies, ends) in table {
            assert_eq!(OcrAutoAction::parse(value), Some(action), "{value}");
            assert_eq!(
                (action.copies(), action.ends_screenshot()),
                (copies, ends),
                "{value}"
            );
        }
        assert_eq!(OcrAutoAction::parse("bogus"), None);
        assert_eq!(OcrAutoAction::default(), OcrAutoAction::NoAction);
    }

    /// 没有自动复制的完成态：状态条是“未复制”文案，不是“复制失败”，中英文都有。
    #[test]
    fn skipped_copy_has_its_own_texts() {
        let state = OcrUiState::from_result(&result(&["abc"]), true).without_auto_copy();
        assert!(matches!(
            state,
            OcrUiState::Done {
                copied: false,
                skipped: true,
                ..
            }
        ));
        for i18n in [zh(), en()] {
            let status = state.status_text(i18n).unwrap();
            assert!(
                !status.contains("失败") && !status.contains("failed"),
                "{status}"
            );
        }
        assert_eq!(OcrUiState::Idle.without_auto_copy(), OcrUiState::Idle);
    }
}
