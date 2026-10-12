//! 托盘宿主与 UI 进程之间的协议（方案：`docs/design/tray-host-split.md` §2、§4）。
//!
//! 只依赖 `serde` 与 `serde_json`：宿主进程的依赖闭包有严格上限（`workspace-guard` 检查），
//! 所以快照、消息、编解码、序号握手全部放在这个无运行时的 crate 里，UI 进程与宿主共用同一份定义。
//!
//! # 模块
//! - [`spec`]：`host/spec.json` 快照（热键表、托盘菜单、鼠标手势、开关、提示文案、保活标志）。
//! - [`message`]：会话管道上的消息（宿主→UI 的 [`message::HostMessage`]、UI→宿主的 [`message::UiMessage`]）。
//! - [`frame`]：JSON 行编解码与按字节流切行的 [`frame::FrameReader`]。
//! - [`seq`]：命令序号、UI 侧去重与 `exit_request` 握手的仲裁（宿主是唯一仲裁方）。
//! - [`base64`]：快照里图标像素用的最小 base64（不引入第三方依赖）。
//!
//! 本阶段（阶段 1）只提供定义与单测，不接入运行时。

pub mod base64;
pub mod frame;
pub mod message;
pub mod seq;
pub mod spec;

use std::time::Duration;

/// 会话协议版本；`hello` 里带上，两端不一致就拒绝连接。
pub const PROTOCOL_VERSION: u32 = 1;

/// 快照文件格式版本；宿主遇到不认识的版本时退回内置最小菜单。
pub const SPEC_VERSION: u32 = 1;

/// 单行消息的最大字节数（含不含换行都按内容长度算）；超出视为协议错误并丢弃该行。
pub const MAX_LINE_BYTES: usize = 64 * 1024;

/// 会话令牌的字节数（编码成十六进制后长度翻倍）。
pub const TOKEN_BYTES: usize = 32;

/// 宿主命令队列的上限条数。
pub const MAX_QUEUED_COMMANDS: usize = 64;

/// 队列里的命令过期时间。
pub const COMMAND_EXPIRY: Duration = Duration::from_secs(30);

/// 等待 `ack` 的超时；超时后重发一次，再超时就丢弃并记日志。
pub const ACK_TIMEOUT: Duration = Duration::from_millis(1500);

/// 等待 UI 进程发来 `hello` 的超时。
pub const HELLO_TIMEOUT: Duration = Duration::from_secs(10);

/// `exit_ok` 之后等旧 UI 进程退出的上限，超过才强制结束。
pub const EXIT_WAIT_TIMEOUT: Duration = Duration::from_secs(10);

/// 设置页改热键时等待宿主重注册结果的超时，超时按失败处理。
pub const HOTKEYS_APPLY_TIMEOUT: Duration = Duration::from_secs(2);

/// 熔断：统计窗口内允许的崩溃次数，达到即进入退避。
pub const CRASH_BREAKER_THRESHOLD: u32 = 3;

/// 熔断：崩溃计数的统计窗口。
pub const CRASH_BREAKER_WINDOW: Duration = Duration::from_secs(60);

/// 熔断：进入退避后的静默时长。
pub const CRASH_BACKOFF: Duration = Duration::from_secs(300);

/// 动作 id：暂停 / 恢复全部全局热键（宿主自己处理，状态存宿主内存）。
pub const ACTION_TOGGLE_GLOBAL_HOTKEYS: &str = "quick.toggle_global_hotkeys";

/// 动作 id：切换“前台全屏窗口时停用热键”（宿主自己改勾选，并经 `set_config` 落配置）。
pub const ACTION_TOGGLE_FULLSCREEN_GATE: &str = "quick.toggle_disable_on_focused_fullscreen";

/// 动作 id：退出（宿主通知 UI 优雅退出后自己也退出）。
pub const ACTION_QUIT: &str = "quit";

/// 快照提示文案里“失败原因”的占位符；宿主把它替换成实际的失败描述。
pub const PLACEHOLDER_FAILURES: &str = "{failures}";

/// 快照里的保活原因：`mcp/enabled` 为真时 UI 常驻。
pub const KEEP_ALIVE_MCP: &str = "mcp";

/// 快照里的保活原因：`text_recognition/model_hot_start` 为真时 UI 常驻。
pub const KEEP_ALIVE_OCR_HOT_START: &str = "ocr_hot_start";

#[cfg(test)]
mod tests {
    use super::*;

    /// 协议常量与方案 §4 的取值一致。
    #[test]
    fn constants_follow_design() {
        assert_eq!(MAX_QUEUED_COMMANDS, 64);
        assert_eq!(COMMAND_EXPIRY, Duration::from_secs(30));
        assert_eq!(ACK_TIMEOUT, Duration::from_millis(1500));
        assert_eq!(HELLO_TIMEOUT, Duration::from_secs(10));
        assert_eq!(EXIT_WAIT_TIMEOUT, Duration::from_secs(10));
        assert_eq!(HOTKEYS_APPLY_TIMEOUT, Duration::from_secs(2));
        assert_eq!(CRASH_BREAKER_THRESHOLD, 3);
        assert_eq!(CRASH_BREAKER_WINDOW, Duration::from_secs(60));
        assert_eq!(CRASH_BACKOFF, Duration::from_secs(300));
        assert_eq!(TOKEN_BYTES, 32);
    }
}
