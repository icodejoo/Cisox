//! 会话管道上的消息（宿主 ↔ 当前这个 UI 进程的长连接，JSON 行，方案 §4）。
//!
//! 命令集不发明新名字：`Command` 的 `id` 就是现有托盘信号与 `AppCommand` / `QuickAction` 名，
//! UI 侧按原样映射回去。每条要求确认的宿主消息都带 `seq`，UI 用 `ack{seq}` 回应。

use crate::{PROTOCOL_VERSION, TOKEN_BYTES};
use serde::{Deserialize, Serialize};

/// 鼠标手势事件的阶段。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GesturePhase {
    /// 手势按下。
    Begin,
    /// 位置更新（队列里只保留最新一条）。
    Update,
    /// 手势完成（松开）。
    Finish,
    /// 手势取消。
    Cancel,
}

/// 动作的触发来源（只用于 UI 侧日志与来源标签，不影响动作本身）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ActionOrigin {
    /// 全局热键。
    Hotkey,
    /// 托盘菜单或托盘点击。
    Tray,
    /// 命令行 `--cmd`（控制管道）。
    Ipc,
}

/// 通知气泡里的小图（RGBA，行优先，无行间填充）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BalloonIcon {
    /// 宽（像素）。
    pub width: u32,
    /// 高（像素）。
    pub height: u32,
    /// RGBA 像素，线上存成 base64。
    #[serde(with = "crate::base64::bytes")]
    pub rgba: Vec<u8>,
}

/// 单个 `config_key` 的热键注册结果。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HotkeyResult {
    /// 配置键。
    pub config_key: String,
    /// 是否注册成功。
    pub ok: bool,
    /// 失败原因（成功时没有）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// 宿主 → UI。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum HostMessage {
    /// 转发一个动作（托盘菜单项、热键、`--cmd`）；`id` 是托盘信号名或命令名。
    Command {
        /// 命令序号。
        seq: u64,
        /// 动作 id。
        id: String,
        /// 触发来源；旧宿主不带这个字段，按未知处理。
        #[serde(default, skip_serializing_if = "Option::is_none")]
        origin: Option<ActionOrigin>,
    },
    /// 恢复上次保存的贴图。
    RestorePins {
        /// 命令序号。
        seq: u64,
    },
    /// 鼠标手势事件（坐标为虚拟桌面物理像素）。
    Gesture {
        /// 命令序号。
        seq: u64,
        /// 阶段。
        phase: GesturePhase,
        /// 手势编号（同一次拖动内不变）。
        id: u64,
        /// 手势动作标识（只有 `begin` 带，如 `screenshot_copy`）；其余阶段省略。
        #[serde(default, skip_serializing_if = "Option::is_none")]
        action: Option<String>,
        /// 横坐标。
        x: i32,
        /// 纵坐标。
        y: i32,
    },
    /// 宿主需要改配置（UI 是配置唯一写者）。
    SetConfig {
        /// 命令序号。
        seq: u64,
        /// 配置键。
        key: String,
        /// 新值。
        value: serde_json::Value,
    },
    /// 优雅退出：UI 走完 `shutdown_app` 后退出进程。
    Shutdown,
    /// 同意 UI 退出：此后宿主的命令一律入队，等新 UI 起来再发。
    ExitOk,
    /// 拒绝 UI 退出：宿主还有 UI 没处理的命令，UI 继续处理。
    ExitDenied,
    /// 热键重注册的结果（对 `hotkeys_apply` 的回应）。
    HotkeysResult {
        /// 对应的快照修订号。
        rev: u64,
        /// 每个配置键的成败。
        results: Vec<HotkeyResult>,
    },
}

impl HostMessage {
    /// 这条消息的命令序号；不带序号的控制消息返回 `None`。
    ///
    /// # 返回
    /// 需要 UI `ack` 的消息返回其 `seq`。
    ///
    /// ```rust
    /// use snow_host_protocol::message::HostMessage;
    /// let m = HostMessage::Command { seq: 7, id: "capture".into(), origin: None };
    /// assert_eq!(m.seq(), Some(7));
    /// assert_eq!(HostMessage::Shutdown.seq(), None);
    /// ```
    pub fn seq(&self) -> Option<u64> {
        match self {
            HostMessage::Command { seq, .. }
            | HostMessage::RestorePins { seq }
            | HostMessage::Gesture { seq, .. }
            | HostMessage::SetConfig { seq, .. } => Some(*seq),
            HostMessage::Shutdown
            | HostMessage::ExitOk
            | HostMessage::ExitDenied
            | HostMessage::HotkeysResult { .. } => None,
        }
    }
}

/// UI → 宿主。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum UiMessage {
    /// 连上管道后的第一条：自报家门。
    Hello {
        /// UI 进程 ID。
        pid: u32,
        /// 协议版本。
        proto: u32,
        /// 启动参数里带的一次性令牌。
        token: String,
    },
    /// 确认已收到并处理（或已入自己的收件箱）到 `seq` 的命令。
    Ack {
        /// 被确认的命令序号。
        seq: u64,
    },
    /// 请宿主改托盘悬停提示。
    Tooltip {
        /// 提示文字。
        text: String,
    },
    /// 请宿主弹一条系统通知气泡（UI 随后可能立即退出，所以由宿主来弹）。
    Balloon {
        /// 气泡标题。
        title: String,
        /// 气泡正文。
        text: String,
        /// 气泡里显示的小图（如截图缩略图）；没有则用系统默认图标。
        #[serde(default, skip_serializing_if = "Option::is_none")]
        icon: Option<BalloonIcon>,
        /// 点气泡时用系统默认程序打开的路径（如截图文件）；没有则点了不做事。
        #[serde(default, skip_serializing_if = "Option::is_none")]
        open: Option<String>,
    },
    /// UI 已写好新快照（托盘菜单、手势等变了）。
    SpecChanged {
        /// 快照修订号。
        rev: u64,
    },
    /// UI 已写好含新热键的快照，请宿主立即重注册并回报结果。
    HotkeysApply {
        /// 快照修订号。
        rev: u64,
    },
    /// UI 放弃了这次鼠标手势（已有截图 / 录制在进行等）：请宿主让钩子取消，别再吞鼠标输入。
    CancelGesture {
        /// 被放弃的手势编号。
        id: u64,
    },
    /// UI 判定无窗口、无活动，请求退出。
    ExitRequest {
        /// UI 已处理的最后一条命令序号。
        last_seq: u64,
    },
    /// 活动状态（给崩溃恢复与日志用）。
    Activity {
        /// 打开的窗口数。
        windows: u32,
        /// 贴图数。
        pins: u32,
        /// 阻止退出的原因（录屏、下载等）。
        busy_reasons: Vec<String>,
    },
    /// 截图类会话已结束：没有采集在进行、没有倒计时、没有覆盖窗（宿主据此量“关闭 → 下次开启”的间隔，
    /// 用于连续误触发保护）。UI 每处理完一条截图类命令后、会话落到空闲时发一次；旧 UI 不发这条消息，
    /// 宿主退化为只在 UI 进程退出时记关闭时刻。
    CaptureIdle,
}

/// `hello` 校验不通过的原因。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HelloError {
    /// 第一条消息不是 `hello`。
    NotHello,
    /// 协议版本不一致。
    ProtocolMismatch,
    /// 进程 ID 与宿主拉起的子进程不符。
    PidMismatch,
    /// 令牌不符。
    TokenMismatch,
}

impl std::fmt::Display for HelloError {
    /// 输出面向日志的错误描述。
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let text = match self {
            HelloError::NotHello => "会话首条消息不是 hello",
            HelloError::ProtocolMismatch => "协议版本不一致",
            HelloError::PidMismatch => "进程 ID 与拉起的子进程不符",
            HelloError::TokenMismatch => "会话令牌不符",
        };
        write!(f, "{text}")
    }
}

impl std::error::Error for HelloError {}

/// 字节串转小写十六进制文本（令牌、一次性 launch_id 用）。
///
/// # 参数
/// - `bytes`：任意字节。
///
/// # 返回
/// 长度为 `bytes.len() * 2` 的小写十六进制字符串。
///
/// ```rust
/// use snow_host_protocol::message::hex_lower;
/// assert_eq!(hex_lower(&[0x0f, 0xa0]), "0fa0");
/// ```
pub fn hex_lower(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        out.push(DIGITS[usize::from(b >> 4)] as char);
        out.push(DIGITS[usize::from(b & 0x0f)] as char);
    }
    out
}

/// 令牌是否合法：恰好 `TOKEN_BYTES` 个字节的十六进制文本（小写）。
///
/// # 参数
/// - `token`：待检查的令牌。
///
/// ```rust
/// use snow_host_protocol::message::is_valid_token;
/// assert!(is_valid_token(&"ab".repeat(32)));
/// assert!(!is_valid_token("abc"));
/// ```
pub fn is_valid_token(token: &str) -> bool {
    token.len() == TOKEN_BYTES * 2
        && token
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// 恒定时间比较两个令牌，避免按前缀长度泄露差异。
fn tokens_equal(a: &str, b: &str) -> bool {
    a.len() == b.len()
        && a.bytes()
            .zip(b.bytes())
            .fold(0u8, |acc, (x, y)| acc | (x ^ y))
            == 0
}

/// 宿主校验 UI 的第一条消息：必须是 `hello`，协议版本、进程 ID、令牌都对得上。
///
/// # 参数
/// - `first`：会话里收到的第一条 UI 消息。
/// - `expected_pid`：宿主 `CreateProcess` 得到的子进程 ID。
/// - `expected_token`：宿主传给 UI 的启动令牌。
///
/// # 返回
/// 通过为 `Ok(())`，否则 [`HelloError`]（宿主应关闭管道）。
///
/// ```rust
/// use snow_host_protocol::{PROTOCOL_VERSION, message::{UiMessage, check_hello}};
/// let token = "cd".repeat(32);
/// let hello = UiMessage::Hello { pid: 10, proto: PROTOCOL_VERSION, token: token.clone() };
/// assert!(check_hello(&hello, 10, &token).is_ok());
/// assert!(check_hello(&hello, 11, &token).is_err());
/// ```
pub fn check_hello(
    first: &UiMessage,
    expected_pid: u32,
    expected_token: &str,
) -> Result<(), HelloError> {
    let UiMessage::Hello { pid, proto, token } = first else {
        return Err(HelloError::NotHello);
    };
    if *proto != PROTOCOL_VERSION {
        return Err(HelloError::ProtocolMismatch);
    }
    if *pid != expected_pid {
        return Err(HelloError::PidMismatch);
    }
    if !tokens_equal(token, expected_token) {
        return Err(HelloError::TokenMismatch);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// 每种宿主消息都有稳定的线上格式：`type` 为 snake_case，字段名固定。
    #[test]
    fn host_message_wire_format() {
        let cases = [
            (
                HostMessage::Command {
                    seq: 1,
                    id: "capture".into(),
                    origin: None,
                },
                json!({"type": "command", "seq": 1, "id": "capture"}),
            ),
            (
                HostMessage::Command {
                    seq: 10,
                    id: "settings".into(),
                    origin: Some(ActionOrigin::Ipc),
                },
                json!({"type": "command", "seq": 10, "id": "settings", "origin": "ipc"}),
            ),
            (
                HostMessage::RestorePins { seq: 2 },
                json!({"type": "restore_pins", "seq": 2}),
            ),
            (
                HostMessage::Gesture {
                    seq: 3,
                    phase: GesturePhase::Update,
                    id: 9,
                    action: None,
                    x: -10,
                    y: 20,
                },
                json!({"type": "gesture", "seq": 3, "phase": "update", "id": 9, "x": -10, "y": 20}),
            ),
            (
                HostMessage::Gesture {
                    seq: 11,
                    phase: GesturePhase::Begin,
                    id: 9,
                    action: Some("screenshot_copy".into()),
                    x: 5,
                    y: 6,
                },
                json!({"type": "gesture", "seq": 11, "phase": "begin", "id": 9,
                       "action": "screenshot_copy", "x": 5, "y": 6}),
            ),
            (
                HostMessage::SetConfig {
                    seq: 4,
                    key: "a/b".into(),
                    value: json!(true),
                },
                json!({"type": "set_config", "seq": 4, "key": "a/b", "value": true}),
            ),
            (HostMessage::Shutdown, json!({"type": "shutdown"})),
            (HostMessage::ExitOk, json!({"type": "exit_ok"})),
            (HostMessage::ExitDenied, json!({"type": "exit_denied"})),
            (
                HostMessage::HotkeysResult {
                    rev: 8,
                    results: vec![
                        HotkeyResult {
                            config_key: "k1".into(),
                            ok: true,
                            error: None,
                        },
                        HotkeyResult {
                            config_key: "k2".into(),
                            ok: false,
                            error: Some("占用".into()),
                        },
                    ],
                },
                json!({"type": "hotkeys_result", "rev": 8, "results": [
                    {"config_key": "k1", "ok": true},
                    {"config_key": "k2", "ok": false, "error": "占用"},
                ]}),
            ),
        ];
        for (message, wire) in cases {
            assert_eq!(serde_json::to_value(&message).unwrap(), wire);
            assert_eq!(
                serde_json::from_value::<HostMessage>(wire).unwrap(),
                message
            );
        }
    }

    /// 每种 UI 消息都有稳定的线上格式。
    #[test]
    fn ui_message_wire_format() {
        let cases = [
            (
                UiMessage::Hello {
                    pid: 5,
                    proto: 1,
                    token: "t".into(),
                },
                json!({"type": "hello", "pid": 5, "proto": 1, "token": "t"}),
            ),
            (UiMessage::Ack { seq: 6 }, json!({"type": "ack", "seq": 6})),
            (
                UiMessage::Tooltip { text: "hi".into() },
                json!({"type": "tooltip", "text": "hi"}),
            ),
            (
                UiMessage::Balloon {
                    title: "t".into(),
                    text: "x".into(),
                    icon: None,
                    open: None,
                },
                json!({"type": "balloon", "title": "t", "text": "x"}),
            ),
            (
                UiMessage::Balloon {
                    title: "t".into(),
                    text: "x".into(),
                    icon: Some(BalloonIcon {
                        width: 1,
                        height: 1,
                        rgba: vec![1, 2, 3, 4],
                    }),
                    open: Some("C:/a".into()),
                },
                json!({
                    "type": "balloon", "title": "t", "text": "x",
                    "icon": {"width": 1, "height": 1, "rgba": "AQIDBA=="},
                    "open": "C:/a"
                }),
            ),
            (
                UiMessage::SpecChanged { rev: 3 },
                json!({"type": "spec_changed", "rev": 3}),
            ),
            (
                UiMessage::HotkeysApply { rev: 4 },
                json!({"type": "hotkeys_apply", "rev": 4}),
            ),
            (
                UiMessage::CancelGesture { id: 4 },
                json!({"type": "cancel_gesture", "id": 4}),
            ),
            (
                UiMessage::ExitRequest { last_seq: 12 },
                json!({"type": "exit_request", "last_seq": 12}),
            ),
            (
                UiMessage::Activity {
                    windows: 1,
                    pins: 2,
                    busy_reasons: vec!["recording".into()],
                },
                json!({"type": "activity", "windows": 1, "pins": 2, "busy_reasons": ["recording"]}),
            ),
            (UiMessage::CaptureIdle, json!({"type": "capture_idle"})),
        ];
        for (message, wire) in cases {
            assert_eq!(serde_json::to_value(&message).unwrap(), wire);
            assert_eq!(serde_json::from_value::<UiMessage>(wire).unwrap(), message);
        }
    }

    /// 未知消息类型与缺字段都解析失败（交给调用方记日志），多余字段被忽略（向前兼容）。
    #[test]
    fn unknown_and_malformed_messages() {
        assert!(serde_json::from_value::<HostMessage>(json!({"type": "teleport"})).is_err());
        assert!(
            serde_json::from_value::<HostMessage>(json!({"type": "command", "seq": 1})).is_err()
        );
        assert!(serde_json::from_value::<UiMessage>(json!({"seq": 1})).is_err());
        let extra = json!({"type": "ack", "seq": 1, "future_field": "x"});
        assert_eq!(
            serde_json::from_value::<UiMessage>(extra).unwrap(),
            UiMessage::Ack { seq: 1 }
        );
    }

    /// 只有需要确认的消息带序号。
    #[test]
    fn seq_only_on_ackable_messages() {
        assert_eq!(HostMessage::RestorePins { seq: 4 }.seq(), Some(4));
        assert_eq!(
            HostMessage::SetConfig {
                seq: 5,
                key: "k".into(),
                value: json!(1)
            }
            .seq(),
            Some(5)
        );
        for control in [
            HostMessage::Shutdown,
            HostMessage::ExitOk,
            HostMessage::ExitDenied,
            HostMessage::HotkeysResult {
                rev: 1,
                results: vec![],
            },
        ] {
            assert_eq!(control.seq(), None);
        }
    }

    /// 十六进制编码：定长、小写、与令牌格式校验互相印证。
    #[test]
    fn hex_encoding_matches_token_format() {
        assert_eq!(hex_lower(&[]), "");
        assert_eq!(hex_lower(&[0x00, 0xff, 0x1a]), "00ff1a");
        let token = hex_lower(&[0xabu8; TOKEN_BYTES]);
        assert!(is_valid_token(&token));
    }

    /// 令牌格式：长度与字符集。
    #[test]
    fn token_format() {
        assert!(is_valid_token(&"0f".repeat(TOKEN_BYTES)));
        assert!(!is_valid_token(&"0f".repeat(TOKEN_BYTES - 1)));
        assert!(
            !is_valid_token(&"0F".repeat(TOKEN_BYTES)),
            "只收小写十六进制"
        );
        assert!(!is_valid_token(&"zz".repeat(TOKEN_BYTES)));
        assert!(!is_valid_token(""));
    }

    /// hello 校验：首条必须是 hello，版本、进程 ID、令牌三者都要对上。
    #[test]
    fn hello_checks() {
        let token = "ab".repeat(TOKEN_BYTES);
        let hello = |pid, proto, token: &str| UiMessage::Hello {
            pid,
            proto,
            token: token.to_string(),
        };
        assert_eq!(
            check_hello(&hello(7, PROTOCOL_VERSION, &token), 7, &token),
            Ok(())
        );
        assert_eq!(
            check_hello(&UiMessage::Ack { seq: 1 }, 7, &token),
            Err(HelloError::NotHello)
        );
        assert_eq!(
            check_hello(&hello(7, PROTOCOL_VERSION + 1, &token), 7, &token),
            Err(HelloError::ProtocolMismatch)
        );
        assert_eq!(
            check_hello(&hello(8, PROTOCOL_VERSION, &token), 7, &token),
            Err(HelloError::PidMismatch)
        );
        let other = "cd".repeat(TOKEN_BYTES);
        assert_eq!(
            check_hello(&hello(7, PROTOCOL_VERSION, &other), 7, &token),
            Err(HelloError::TokenMismatch)
        );
        assert_eq!(
            check_hello(&hello(7, PROTOCOL_VERSION, ""), 7, &token),
            Err(HelloError::TokenMismatch)
        );
    }
}
