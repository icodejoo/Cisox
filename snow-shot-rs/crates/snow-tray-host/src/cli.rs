//! 宿主的命令行：`--cmd <命令>`（交给运行中的实例；没有实例时宿主起来后执行它）、`--resident`（旧单进程回退）。
//!
//! 命令名、默认行为与 UI 进程的旧入口完全一致（`snow-shot --cmd screenshot` 等照常可用）。

use snow_host_protocol::launch::FLAG_RESIDENT;
use snow_platform::single_instance::IpcCommand;

/// 命令行参数：向运行中实例发送的命令。
pub const FLAG_COMMAND: &str = "--cmd";

/// 自检开关：应用进程加固后读回缓解策略，自己退出（离屏测试用，不创建任何窗口 / 线程）。
pub const FLAG_SELF_CHECK_HARDEN: &str = "--self-check-harden";

/// 用法提示。
pub const USAGE: &str = "用法: snow-shot [--cmd screenshot|recording|scroll-capture|pin-clipboard|settings|show|quit] [--resident]";

/// 解析后的命令行。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HostCli {
    /// 常驻宿主（已有实例在运行时，作为从属实例把 `command` 交给它后退出）。
    Run {
        /// 命令行显式给出的 `--cmd`；没给为 `None`。从属实例时 `None` 按唤醒处理；
        /// 本进程成为第一个实例时，显式命令在宿主起来后执行（`None` 只常驻托盘）。
        command: Option<IpcCommand>,
    },
    /// `--resident`：不跑宿主，改拉起旧的单进程 UI（`snow-shot-ui.exe --resident`）。
    Resident,
    /// `--self-check-harden`：只验证进程加固是否生效。
    SelfCheckHarden,
}

/// 解析命令行（不含程序名）。
///
/// # 参数
/// - `args`：参数序列。
///
/// # 返回
/// 解析结果；未知参数、缺参数、非法命令名返回错误文本（调用方连同 [`USAGE`] 一起输出）。
///
/// ```rust
/// use snow_platform::single_instance::IpcCommand;
/// use snow_tray_host::cli::{HostCli, parse_cli};
/// let cli = parse_cli(["--cmd".to_string(), "quit".to_string()]).unwrap();
/// assert_eq!(cli, HostCli::Run { command: Some(IpcCommand::Quit) });
/// assert_eq!(parse_cli(["--resident".to_string()]).unwrap(), HostCli::Resident);
/// ```
pub fn parse_cli(args: impl IntoIterator<Item = String>) -> Result<HostCli, String> {
    let mut command = None;
    let mut resident = false;
    let mut self_check = false;
    let mut iter = args.into_iter();
    while let Some(arg) = iter.next() {
        match arg.as_str() {
            FLAG_COMMAND => {
                let name = iter
                    .next()
                    .ok_or_else(|| format!("{FLAG_COMMAND} 缺少参数"))?;
                command = Some(
                    IpcCommand::from_cli_name(&name).ok_or_else(|| format!("未知命令: {name}"))?,
                );
            }
            FLAG_RESIDENT => resident = true,
            FLAG_SELF_CHECK_HARDEN => self_check = true,
            other => return Err(format!("未知参数: {other}")),
        }
    }
    Ok(if self_check {
        HostCli::SelfCheckHarden
    } else if resident {
        HostCli::Resident
    } else {
        HostCli::Run { command }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 把字符串切片转成参数序列。
    fn args(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| s.to_string()).collect()
    }

    /// 无参数没有显式命令；`--cmd` 支持旧入口的全部命令名（大小写不敏感）。
    #[test]
    fn defaults_and_all_commands() {
        assert_eq!(
            parse_cli(args(&[])).unwrap(),
            HostCli::Run { command: None }
        );
        for (name, want) in [
            ("screenshot", IpcCommand::TriggerScreenshot),
            ("recording", IpcCommand::TriggerRecording),
            ("scroll-capture", IpcCommand::ScrollCapture),
            ("pin-clipboard", IpcCommand::PinClipboard),
            ("settings", IpcCommand::OpenSettings),
            ("show", IpcCommand::ShowMainWindow),
            ("QUIT", IpcCommand::Quit),
        ] {
            assert_eq!(
                parse_cli(args(&["--cmd", name])).unwrap(),
                HostCli::Run {
                    command: Some(want)
                },
                "{name}"
            );
        }
    }

    /// `--resident` 与自检开关；自检优先。
    #[test]
    fn mode_switches() {
        assert_eq!(parse_cli(args(&["--resident"])).unwrap(), HostCli::Resident);
        assert_eq!(
            parse_cli(args(&["--self-check-harden"])).unwrap(),
            HostCli::SelfCheckHarden
        );
        assert_eq!(
            parse_cli(args(&["--resident", "--self-check-harden"])).unwrap(),
            HostCli::SelfCheckHarden
        );
    }

    /// 缺参数、未知命令、未知参数都报错。
    #[test]
    fn rejects_bad_input() {
        assert!(parse_cli(args(&["--cmd"])).is_err());
        assert!(parse_cli(args(&["--cmd", "boom"])).is_err());
        assert!(parse_cli(args(&["--wat"])).is_err());
        assert!(
            parse_cli(args(&["--ui"])).is_err(),
            "--ui 是 UI 进程的开关，宿主不认"
        );
    }
}
