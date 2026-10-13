//! 控制管道命令（`--cmd`）到动作 id 的映射。
//!
//! 命令行入口不变：从属实例经 `CISOX1` 控制管道把 [`IpcCommand`] 交给宿主，宿主把它翻译成
//! 现有的动作 id（和托盘、热键用同一套），转给 UI 进程；`quit` 由宿主自己处理。

use snow_host_protocol::{
    ACTION_CAPTURE, ACTION_MAIN_WINDOW, ACTION_PIN_CLIPBOARD, ACTION_QUIT, ACTION_SCROLL_CAPTURE,
    ACTION_SETTINGS, ACTION_START_RECORDING,
};
use snow_platform::single_instance::IpcCommand;

/// 把控制管道命令映射成动作 id。
///
/// # 参数
/// - `command`：从属实例发来的命令。
///
/// # 返回
/// 动作 id；自定义参数（`Custom`）没有对应动作，返回 `None`（调用方记日志并忽略）。
///
/// ```rust
/// use snow_platform::single_instance::IpcCommand;
/// use snow_tray_host::ipc_map::action_id_for;
/// assert_eq!(action_id_for(&IpcCommand::TriggerScreenshot), Some("capture"));
/// assert_eq!(action_id_for(&IpcCommand::Quit), Some("quit"));
/// assert_eq!(action_id_for(&IpcCommand::Custom("x".into())), None);
/// ```
pub fn action_id_for(command: &IpcCommand) -> Option<&'static str> {
    match command {
        IpcCommand::TriggerScreenshot => Some(ACTION_CAPTURE),
        IpcCommand::TriggerRecording => Some(ACTION_START_RECORDING),
        IpcCommand::ScrollCapture => Some(ACTION_SCROLL_CAPTURE),
        IpcCommand::PinClipboard => Some(ACTION_PIN_CLIPBOARD),
        IpcCommand::OpenSettings => Some(ACTION_SETTINGS),
        IpcCommand::ShowMainWindow => Some(ACTION_MAIN_WINDOW),
        IpcCommand::Quit => Some(ACTION_QUIT),
        IpcCommand::Custom(_) => None,
    }
}

/// 本进程成为第一个实例（此前没有运行中的宿主）时，对命令行里 `--cmd` 的处理方式。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FirstRun {
    /// 没带 `--cmd`（双击、开机自启）：只常驻托盘，不弹任何窗口。
    Idle,
    /// `--cmd quit` 但根本没有实例在跑：没有可退出的对象，直接结束，不白起一个宿主。
    Quit,
    /// 其它命令：宿主起来后立刻执行它（动作 id 同托盘 / 热键的一套）。
    Action(&'static str),
}

/// 决定第一个实例该怎么处理 `--cmd`（阶段 4 起 `--cmd` 在没有实例时也生效，不再被忽略）。
///
/// # 参数
/// - `command`：命令行显式给出的 `--cmd`；没给传 `None`。
///
/// # 返回
/// 处理方式；没有对应动作的自定义命令按 [`FirstRun::Idle`] 处理。
///
/// ```rust
/// use snow_platform::single_instance::IpcCommand;
/// use snow_tray_host::ipc_map::{FirstRun, first_run_plan};
/// assert_eq!(first_run_plan(None), FirstRun::Idle);
/// assert_eq!(first_run_plan(Some(&IpcCommand::Quit)), FirstRun::Quit);
/// assert_eq!(first_run_plan(Some(&IpcCommand::TriggerScreenshot)), FirstRun::Action("capture"));
/// ```
pub fn first_run_plan(command: Option<&IpcCommand>) -> FirstRun {
    match command {
        None => FirstRun::Idle,
        Some(IpcCommand::Quit) => FirstRun::Quit,
        Some(other) => action_id_for(other).map_or(FirstRun::Idle, FirstRun::Action),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 第一个实例：无 `--cmd` 只常驻；`quit` 直接结束；其余命令起来就执行。
    #[test]
    fn first_run_executes_explicit_command_only() {
        assert_eq!(first_run_plan(None), FirstRun::Idle);
        assert_eq!(first_run_plan(Some(&IpcCommand::Quit)), FirstRun::Quit);
        assert_eq!(
            first_run_plan(Some(&IpcCommand::ShowMainWindow)),
            FirstRun::Action("main_window")
        );
        assert_eq!(
            first_run_plan(Some(&IpcCommand::TriggerRecording)),
            FirstRun::Action("start_recording")
        );
        assert_eq!(
            first_run_plan(Some(&IpcCommand::Custom("x".into()))),
            FirstRun::Idle
        );
    }

    /// 七个 `--cmd` 命令都有映射，id 互不相同；自定义参数没有映射。
    #[test]
    fn all_seven_commands_map_to_distinct_actions() {
        let commands = [
            IpcCommand::TriggerScreenshot,
            IpcCommand::TriggerRecording,
            IpcCommand::ScrollCapture,
            IpcCommand::PinClipboard,
            IpcCommand::OpenSettings,
            IpcCommand::ShowMainWindow,
            IpcCommand::Quit,
        ];
        let ids: Vec<_> = commands
            .iter()
            .map(|c| action_id_for(c).expect("应有映射"))
            .collect();
        let mut unique = ids.clone();
        unique.sort_unstable();
        unique.dedup();
        assert_eq!(unique.len(), commands.len());
        assert_eq!(action_id_for(&IpcCommand::Custom("a".into())), None);
    }

    /// 与旧入口 `map_ipc_command` 的语义逐项对应（截图 / 录屏 / 长截图 / 贴图 / 设置 / 主窗口 / 退出）。
    #[test]
    fn ids_match_ui_vocabulary() {
        assert_eq!(
            action_id_for(&IpcCommand::TriggerScreenshot),
            Some("capture")
        );
        assert_eq!(
            action_id_for(&IpcCommand::TriggerRecording),
            Some("start_recording")
        );
        assert_eq!(
            action_id_for(&IpcCommand::ScrollCapture),
            Some("scroll_capture")
        );
        assert_eq!(
            action_id_for(&IpcCommand::PinClipboard),
            Some("pin_clipboard")
        );
        assert_eq!(action_id_for(&IpcCommand::OpenSettings), Some("settings"));
        assert_eq!(
            action_id_for(&IpcCommand::ShowMainWindow),
            Some("main_window")
        );
        assert_eq!(action_id_for(&IpcCommand::Quit), Some("quit"));
    }
}
