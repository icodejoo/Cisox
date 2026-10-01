//! 覆盖窗里的 OCR 交互状态与展示文案（纯数据，不依赖 GPUI，便于离屏测试）。
//!
//! 状态流转：`Idle → Running → Done | Failed`；缺资产时 `Failed{can_download}` 可按 D 进入
//! `Downloading`，下载完成回到 `Idle`（提示用户再次点击 OCR）。

use crate::ocr_client::OcrError;
use crate::ocr_service::{OcrResult, OcrTextBox};

/// 结果面板最多显示的行数。
pub const PANEL_MAX_LINES: usize = 8;
/// 结果面板每行最多显示的字符数。
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
        }
    }

    /// 由错误得到失败态。
    ///
    /// # 参数
    /// - `error`：识别错误。
    pub fn from_error(error: &OcrError) -> Self {
        Self::Failed {
            message: error.message(),
            can_download: error.can_download(),
        }
    }

    /// 底部状态条文案。
    pub fn status_text(&self) -> Option<String> {
        match self {
            Self::Idle => None,
            Self::Running => Some("正在识别文字…".to_string()),
            Self::Done { text, .. } if text.is_empty() => Some("未识别到文字".to_string()),
            Self::Done { boxes, copied: true, .. } => Some(format!("已识别 {} 行，文字已复制到剪贴板", boxes.len())),
            Self::Done { boxes, copied: false, .. } => Some(format!("已识别 {} 行（复制到剪贴板失败）", boxes.len())),
            Self::Failed { message, can_download: true } => Some(format!("{message} · 按 D 下载")),
            Self::Failed { message, .. } => Some(message.clone()),
            Self::Downloading(step) => Some(step.clone()),
        }
    }
}

/// 把一行文字截到面板宽度（按字符计，超出加省略号）。
///
/// # 参数
/// - `line`：原文。
/// - `max_chars`：最多字符数。
pub fn truncate_line(line: &str, max_chars: usize) -> String {
    if line.chars().count() <= max_chars {
        line.to_string()
    } else {
        let head: String = line.chars().take(max_chars).collect();
        format!("{head}…")
    }
}

/// 结果面板要显示的文字行。
///
/// # 参数
/// - `state`：OCR 状态。
///
/// # 返回
/// 空表示不显示面板；否则依次是标题 / 正文 / 操作提示。
///
/// ```ignore
/// assert!(panel_lines(&OcrUiState::Idle).is_empty());
/// ```
pub fn panel_lines(state: &OcrUiState) -> Vec<String> {
    match state {
        OcrUiState::Idle => Vec::new(),
        OcrUiState::Running => vec!["正在识别文字…".to_string()],
        OcrUiState::Downloading(step) => vec![step.clone(), "下载完成后请再次点击“OCR”".to_string()],
        OcrUiState::Done { text, .. } if text.is_empty() => {
            vec!["未识别到文字".to_string(), "Esc 返回".to_string()]
        }
        OcrUiState::Done { text, copied, .. } => {
            let all: Vec<&str> = text.lines().collect();
            let mut lines: Vec<String> = all
                .iter()
                .take(PANEL_MAX_LINES)
                .map(|l| truncate_line(l, PANEL_MAX_CHARS))
                .collect();
            if all.len() > PANEL_MAX_LINES {
                lines.push(format!("…（另有 {} 行）", all.len() - PANEL_MAX_LINES));
            }
            let footer = if *copied {
                "已复制 · Enter 复制并关闭 · Esc 返回"
            } else {
                "复制失败 · Enter 重试并关闭 · Esc 返回"
            };
            lines.push(footer.to_string());
            lines
        }
        OcrUiState::Failed { message, can_download } => {
            let mut lines = vec![truncate_line(message, PANEL_MAX_CHARS * 2)];
            if *can_download {
                lines.push("按 D 下载 OCR 组件（运行时约 17 MB + 模型约 31 MB）".to_string());
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
    use snow_ui::shell::geometry::PhysicalRect;

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
        }
    }

    /// 状态可见性与忙碌判定。
    #[test]
    fn visibility_and_busy() {
        assert!(!OcrUiState::Idle.is_visible());
        assert!(OcrUiState::Running.is_visible() && OcrUiState::Running.is_busy());
        assert!(OcrUiState::Downloading("x".into()).is_busy());
        assert!(!OcrUiState::from_result(&result(&["a"]), true).is_busy());
        assert!(OcrUiState::Idle.status_text().is_none());
    }

    /// 结果态：有文字 / 无文字的状态条与面板。
    #[test]
    fn done_state_texts() {
        let done = OcrUiState::from_result(&result(&["第一行", "second"]), true);
        assert_eq!(done.status_text().as_deref(), Some("已识别 2 行，文字已复制到剪贴板"));
        let lines = panel_lines(&done);
        assert_eq!(lines[0], "第一行");
        assert_eq!(lines[1], "second");
        assert!(lines.last().is_some_and(|l| l.contains("Enter")));
        let failed_copy = OcrUiState::from_result(&result(&["a"]), false);
        assert!(failed_copy.status_text().is_some_and(|s| s.contains("失败")));
        let empty = OcrUiState::from_result(&result(&[]), true);
        assert_eq!(empty.status_text().as_deref(), Some("未识别到文字"));
        assert_eq!(panel_lines(&empty)[0], "未识别到文字");
    }

    /// 面板行数与行宽受限；超出行数给出“另有 N 行”。
    #[test]
    fn panel_is_bounded() {
        let long = "长".repeat(PANEL_MAX_CHARS + 10);
        let many: Vec<String> = (0..12).map(|i| format!("line{i}")).collect();
        let refs: Vec<&str> = many.iter().map(String::as_str).collect();
        let lines = panel_lines(&OcrUiState::from_result(&result(&refs), true));
        assert_eq!(lines.len(), PANEL_MAX_LINES + 2);
        assert!(lines[PANEL_MAX_LINES].contains("另有 4 行"));
        let lines = panel_lines(&OcrUiState::from_result(&result(&[long.as_str()]), true));
        assert_eq!(lines[0].chars().count(), PANEL_MAX_CHARS + 1);
        assert!(lines[0].ends_with('…'));
        assert_eq!(truncate_line("abc", 5), "abc");
    }

    /// 各类失败给出不同文案；缺资产时提示按 D 下载，其它失败不提示。
    #[test]
    fn failure_states() {
        let no_runtime = OcrUiState::from_error(&OcrError::Unavailable(OcrUnavailable::NoRuntime));
        assert!(no_runtime.status_text().is_some_and(|s| s.contains("按 D 下载")));
        assert!(panel_lines(&no_runtime).iter().any(|l| l.contains("按 D 下载")));
        let died = OcrUiState::from_error(&OcrError::ProcessDied("crash".into()));
        assert!(died.status_text().is_some_and(|s| s.contains("crash") && !s.contains("按 D")));
        assert!(!panel_lines(&died).iter().any(|l| l.contains("按 D")));
        assert_ne!(no_runtime.status_text(), died.status_text());
    }

    /// 下载中与运行中的文案。
    #[test]
    fn progress_states() {
        assert_eq!(OcrUiState::Running.status_text().as_deref(), Some("正在识别文字…"));
        let d = OcrUiState::Downloading("正在下载 OCR 模型 (1/3)…".into());
        assert_eq!(d.status_text().as_deref(), Some("正在下载 OCR 模型 (1/3)…"));
        assert_eq!(panel_lines(&d).len(), 2);
    }
}
