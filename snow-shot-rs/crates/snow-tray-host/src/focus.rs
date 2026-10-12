//! 前台焦点授权的决策（方案 §9“焦点抢占”、§12.3 的焦点矩阵结论）。
//!
//! Windows 只允许“刚收到用户输入”的进程把窗口抢到前台。链条是：`--cmd` 从属实例（由用户的终端 /
//! 快捷方式拉起，自带前台权）先对宿主 PID 调 `AllowSetForegroundWindow`（`snow-platform` 的
//! 单实例客户端已经做了），宿主再对 UI 进程 PID 授权，UI 的覆盖窗才拿得到键盘焦点。
//! 热键路径本来就免授权，仍照做（成本 0.02ms，实测无害）。
//!
//! 这里只放“该不该授权”的纯判定，真正的系统调用在 `win` 模块里。

use crate::queue::Outbound;
use snow_host_protocol::message::GesturePhase;

/// 宿主刚拉起 UI 进程时是否授权：总是授权（UI 一启动就要开窗，热键 / 托盘 / `--cmd` 三条路径都一样）。
pub const GRANT_ON_SPAWN: bool = true;

/// 把一条消息转给 **已经在运行** 的 UI 进程时，是否先授权它抢前台。
///
/// 动作（会开窗：截图覆盖窗、设置窗……）和手势的 `begin`（按下鼠标就要开截图覆盖窗，钩子在宿主里，
/// 用户输入落在宿主这边）需要；恢复贴图、改配置、手势的 `update / finish / cancel` 不该抢走用户当前的焦点。
///
/// # 参数
/// - `message`：要转给 UI 的消息。
///
/// # 返回
/// 需要授权返回 `true`。
///
/// ```rust
/// use snow_tray_host::focus::grant_for_forward;
/// use snow_tray_host::queue::Outbound;
/// let capture = Outbound::Action { id: "capture".into(), origin: None };
/// assert!(grant_for_forward(&capture));
/// assert!(!grant_for_forward(&Outbound::RestorePins));
/// ```
pub fn grant_for_forward(message: &Outbound) -> bool {
    matches!(
        message,
        Outbound::Action { .. }
            | Outbound::Gesture {
                phase: GesturePhase::Begin,
                ..
            }
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use snow_host_protocol::message::{ActionOrigin, GesturePhase};

    /// 动作（无论来源）授权，其余消息不授权。
    #[test]
    fn only_actions_grant_foreground() {
        for origin in [
            None,
            Some(ActionOrigin::Hotkey),
            Some(ActionOrigin::Tray),
            Some(ActionOrigin::Ipc),
        ] {
            let action = Outbound::Action {
                id: "settings".into(),
                origin,
            };
            assert!(grant_for_forward(&action), "{origin:?}");
        }
        assert!(!grant_for_forward(&Outbound::RestorePins));
        assert!(!grant_for_forward(&Outbound::SetConfig {
            key: "k".into(),
            value: json!(true)
        }));
        let gesture = |phase| Outbound::Gesture {
            phase,
            id: 1,
            action: None,
            x: 0,
            y: 0,
        };
        // 手势按下要开覆盖窗，授权；后续的更新 / 完成 / 取消不抢焦点
        assert!(grant_for_forward(&gesture(GesturePhase::Begin)));
        for phase in [
            GesturePhase::Update,
            GesturePhase::Finish,
            GesturePhase::Cancel,
        ] {
            assert!(!grant_for_forward(&gesture(phase)), "{phase:?}");
        }
    }
}
