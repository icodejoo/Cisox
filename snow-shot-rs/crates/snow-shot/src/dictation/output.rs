//! 输出去向：按“输出方式”设置与焦点判定结果，决定这一轮是键入、弹浮窗还是两者都要。
//!
//! 自动模式在开始识别时判定一次，整轮不变；识别过程中焦点变化不会改变去向
//! （只有键入真的失败或目标窗口变了，才会退到浮窗兜底，见 [`OutputPlan::fall_back`]）。

use super::focus::{NoTypeReason, Verdict};
use snow_config::extensions::{DICTATION_OUTPUT_OVERLAY, DICTATION_OUTPUT_TYPE};

/// 输出方式（对应配置项 `dictation/output_mode`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutputMode {
    /// 有可输入焦点就键入，否则弹浮窗。
    Auto,
    /// 只键入。
    Type,
    /// 只弹浮窗。
    Overlay,
}

impl OutputMode {
    /// 由配置值解析；未知值按自动处理。
    ///
    /// # 参数
    /// - `value`：配置里的取值。
    pub fn from_config(value: &str) -> Self {
        match value {
            DICTATION_OUTPUT_TYPE => Self::Type,
            DICTATION_OUTPUT_OVERLAY => Self::Overlay,
            _ => Self::Auto,
        }
    }
}

/// 去向说明：用于状态提示。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RouteNote {
    /// 还在判定焦点。
    Pending,
    /// 正在键入到当前输入框。
    Typing,
    /// 键入，同时浮窗也显示。
    TypingWithOverlay,
    /// 设置为只浮窗。
    OverlayOnly,
    /// 自动模式下不能键入，改用浮窗。
    Fallback(NoTypeReason),
    /// 设置为只键入，但不能键入：给出提示并把文字留在浮窗，而不是静默丢字。
    Blocked(NoTypeReason),
    /// 键入中途目标窗口变了，已停止键入，文字留在浮窗。
    TargetLost,
    /// 键入中途发送失败，文字留在浮窗。
    SendFailed(String),
    /// 结束后迟迟键不进去（例如热键修饰键一直按着），文字留在浮窗。
    TypingStuck,
}

/// 一轮的输出方案。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutputPlan {
    /// 是否键入。
    pub typing: bool,
    /// 是否显示浮窗。
    pub overlay: bool,
    /// 去向说明。
    pub note: RouteNote,
}

impl OutputPlan {
    /// 判定前的占位方案：先不键入也不弹窗，等焦点判定结果。
    pub fn pending() -> Self {
        Self {
            typing: false,
            overlay: false,
            note: RouteNote::Pending,
        }
    }

    /// 只浮窗的方案。
    pub fn overlay_only() -> Self {
        Self {
            typing: false,
            overlay: true,
            note: RouteNote::OverlayOnly,
        }
    }

    /// 键入中途出问题时的兜底：停止键入，改在浮窗保留全部文字。
    ///
    /// # 参数
    /// - `note`：兜底原因（[`RouteNote::TargetLost`] 或 [`RouteNote::SendFailed`]）。
    pub fn fall_back(&mut self, note: RouteNote) {
        self.typing = false;
        self.overlay = true;
        self.note = note;
    }
}

/// 由输出方式与焦点判定得到方案。
///
/// # 参数
/// - `mode`：输出方式。
/// - `type_with_overlay`：键入时是否同时显示浮窗。
/// - `verdict`：焦点判定；`mode` 为只浮窗时可不判定，传 `None`；探测超时也传 `None`（按不确定处理）。
///
/// ```ignore
/// let plan = decide(OutputMode::Auto, false, Some(Verdict::Type));
/// assert!(plan.typing && !plan.overlay);
/// ```
pub fn decide(mode: OutputMode, type_with_overlay: bool, verdict: Option<Verdict>) -> OutputPlan {
    if mode == OutputMode::Overlay {
        return OutputPlan::overlay_only();
    }
    // 没有判定结果（探测超时）按“不确定”处理，宁可弹窗
    let verdict = verdict.unwrap_or(Verdict::NoType(NoTypeReason::Uncertain));
    match (verdict, mode) {
        (Verdict::Type, _) => OutputPlan {
            typing: true,
            overlay: type_with_overlay,
            note: if type_with_overlay {
                RouteNote::TypingWithOverlay
            } else {
                RouteNote::Typing
            },
        },
        (Verdict::NoType(reason), OutputMode::Type) => OutputPlan {
            typing: false,
            overlay: true,
            note: RouteNote::Blocked(reason),
        },
        (Verdict::NoType(reason), _) => OutputPlan {
            typing: false,
            overlay: true,
            note: RouteNote::Fallback(reason),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 配置值解析：三个取值，未知值按自动。
    #[test]
    fn mode_parsing() {
        assert_eq!(OutputMode::from_config("auto"), OutputMode::Auto);
        assert_eq!(OutputMode::from_config("type"), OutputMode::Type);
        assert_eq!(OutputMode::from_config("overlay"), OutputMode::Overlay);
        assert_eq!(OutputMode::from_config("???"), OutputMode::Auto);
    }

    /// 自动模式：能键入就只键入（浮窗默认不弹），开了“同时显示浮窗”才一起弹。
    #[test]
    fn auto_types_when_possible() {
        let plan = decide(OutputMode::Auto, false, Some(Verdict::Type));
        assert_eq!(
            plan,
            OutputPlan {
                typing: true,
                overlay: false,
                note: RouteNote::Typing
            }
        );
        let both = decide(OutputMode::Auto, true, Some(Verdict::Type));
        assert!(both.typing && both.overlay);
        assert_eq!(both.note, RouteNote::TypingWithOverlay);
    }

    /// 自动模式：不能键入或判定不出，一律弹浮窗，并带原因。
    #[test]
    fn auto_falls_back_to_overlay() {
        for reason in [
            NoTypeReason::NoFocus,
            NoTypeReason::NotEditable,
            NoTypeReason::Elevated,
            NoTypeReason::Uncertain,
        ] {
            let plan = decide(OutputMode::Auto, false, Some(Verdict::NoType(reason)));
            assert_eq!(
                plan,
                OutputPlan {
                    typing: false,
                    overlay: true,
                    note: RouteNote::Fallback(reason)
                }
            );
        }
        // 探测超时没有结论，同样走浮窗
        let timeout = decide(OutputMode::Auto, false, None);
        assert_eq!(timeout.note, RouteNote::Fallback(NoTypeReason::Uncertain));
    }

    /// 只键入：不能键入时不静默丢字，文字留在浮窗并给出“被拦住”的说明；能键入时与自动一致。
    #[test]
    fn type_only_never_drops_silently() {
        let blocked = decide(
            OutputMode::Type,
            false,
            Some(Verdict::NoType(NoTypeReason::Elevated)),
        );
        assert!(!blocked.typing && blocked.overlay);
        assert_eq!(blocked.note, RouteNote::Blocked(NoTypeReason::Elevated));
        assert!(decide(OutputMode::Type, false, Some(Verdict::Type)).typing);
    }

    /// 只浮窗：不看判定，直接浮窗。
    #[test]
    fn overlay_mode_ignores_verdict() {
        assert_eq!(
            decide(OutputMode::Overlay, true, None),
            OutputPlan::overlay_only()
        );
        assert_eq!(
            decide(OutputMode::Overlay, false, Some(Verdict::Type)),
            OutputPlan::overlay_only()
        );
    }

    /// 中途兜底：停止键入、打开浮窗、记下原因。
    #[test]
    fn fall_back_switches_to_overlay() {
        let mut plan = decide(OutputMode::Auto, false, Some(Verdict::Type));
        plan.fall_back(RouteNote::TargetLost);
        assert!(!plan.typing && plan.overlay);
        assert_eq!(plan.note, RouteNote::TargetLost);
    }
}
