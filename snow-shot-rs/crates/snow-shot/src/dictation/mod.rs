//! 语音转文字（听写）：热键唤起独立的 `snow-stt` 工作进程，实时识别，结束即退出并释放。
//!
//! 输出有两条去向，按“输出方式”设置决定：有可输入焦点就逐字符键入当前输入框，否则（或设置为只浮窗）
//! 在光标所在显示器右下角弹出可编辑、可复制的小浮窗。自动模式在开始识别时判定一次，整轮不变。
//!
//! 分层：`engine`（进程生命周期状态机）、`typing` / `focus` / `output`（键入差异、焦点判定、去向决策）、
//! `text` / `overlay_model`（文本合并）都是纯逻辑，可离屏单测；`client`、`flow`、`view` 负责接系统。

pub mod client;
pub mod config;
pub mod engine;
pub mod flow;
pub mod focus;
pub mod output;
pub mod overlay_model;
pub mod status;
pub mod text;
pub mod typing;
pub mod view;

pub use flow::DictationHost;

/// 热键 / 总线发来的听写命令。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DictationCommand {
    /// 切换：未在进行则开始，在进行则结束。
    Toggle,
    /// 开始（已在进行则忽略）。
    Start,
    /// 结束（未在进行则忽略）。
    Stop,
}
