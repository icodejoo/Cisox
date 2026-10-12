//! UI 进程这一侧的会话客户端：连上托盘宿主的会话管道、收发消息、跟踪命令序号与退出握手
//! （方案 `docs/design/tray-host-split.md` §4、§5）。
//!
//! 本模块只放与 GPUI 无关的部分：管道读线程把宿主消息投进主线程收件箱；序号去重、退出请求的
//! 状态（一次只发一个 `exit_request`，被拒绝后等下一个事件再试）、设置页改热键时“等宿主回报”的登记，
//! 都是纯逻辑，用假的写端离屏测试。与 `AppState` 的接线在 `app_runtime::host_session`。

use serde_json::Value;
use snow_host_protocol::PROTOCOL_VERSION;
use snow_host_protocol::frame::{decode_line, encode_line};
use snow_host_protocol::launch::UiLaunch;
use snow_host_protocol::message::{HostMessage, HotkeyResult, UiMessage};
use snow_host_protocol::seq::{SeqCheck, UiSeq};
use snow_platform::single_instance::line_pipe::pipe_name_for;
use snow_platform::single_instance::session_pipe::{PipeWriter, SessionPipeClient};
use std::cell::{Cell, RefCell};
use std::collections::HashMap;

/// 写端：能写一行即可（真实实现是会话管道，测试里换成记录器）。
pub trait LineSink {
    /// 写一行（不含换行）。
    fn send_line(&self, line: &str) -> Result<(), String>;
}

impl LineSink for PipeWriter {
    /// 转给会话管道的写端。
    fn send_line(&self, line: &str) -> Result<(), String> {
        PipeWriter::send_line(self, line)
    }
}

/// 宿主侧事件（读线程、定时器、窗口回调投进主线程收件箱）。
#[derive(Debug, Clone, PartialEq)]
pub enum HostEvent {
    /// 宿主发来一条消息。
    Message(HostMessage),
    /// 会话断开（宿主退出或崩溃）。
    Lost,
    /// 设置页改热键后，等宿主回报超时。
    HotkeysApplyTimeout {
        /// 对应的快照修订号。
        rev: u64,
    },
    /// 需要重新生成并发布快照（配置、托盘分组变了；多次请求合并成一次）。
    SyncSpec,
    /// 有窗口关闭了（重新判断能否请求退出）。
    WindowsChanged,
}

/// 退出请求的进度。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ExitState {
    /// 没有未决的退出请求。
    Idle,
    /// 已发出 `exit_request`，等宿主裁决。
    Requested,
}

/// 设置页改热键时登记的待回报项。
#[derive(Debug, Clone, PartialEq)]
pub struct PendingApply {
    /// 变更的配置键。
    pub config_key: &'static str,
    /// 变更前的值（失败时回滚用）。
    pub previous: Value,
}

/// 会话客户端状态。
pub struct HostLink<W: LineSink> {
    /// 写端。
    sink: W,
    /// 命令序号跟踪（去重重发、退出请求带的 `last_seq`）。
    seq: RefCell<UiSeq>,
    /// 退出请求进度。
    exit: Cell<ExitState>,
    /// 宿主已通知关闭（`shutdown` / `exit_ok`）：此后会话断开是预期内的，不当作宿主崩溃。
    closing: Cell<bool>,
    /// 等宿主回报的热键重注册。
    pending: RefCell<HashMap<u64, PendingApply>>,
}

impl<W: LineSink> HostLink<W> {
    /// 新建（尚未处理任何命令）。
    ///
    /// # 参数
    /// - `sink`：写端。
    pub fn new(sink: W) -> Self {
        Self {
            sink,
            seq: RefCell::new(UiSeq::new()),
            exit: Cell::new(ExitState::Idle),
            closing: Cell::new(false),
            pending: RefCell::new(HashMap::new()),
        }
    }

    /// 发送一条 UI 消息；失败只记日志（会话断开会由读线程报 `Lost`）。
    ///
    /// # 参数
    /// - `message`：UI 消息。
    pub fn send(&self, message: &UiMessage) {
        match encode_line(message) {
            Ok(line) => {
                if let Err(e) = self.sink.send_line(line.trim_end_matches('\n')) {
                    tracing::warn!(error = %e, "向宿主发送消息失败");
                }
            }
            Err(e) => tracing::error!(error = %e, "编码给宿主的消息失败"),
        }
    }

    /// 登记收到的命令序号并回 `ack`。
    ///
    /// # 参数
    /// - `seq`：宿主命令的序号。
    ///
    /// # 返回
    /// [`SeqCheck::Duplicate`] 表示宿主因确认超时而重发的旧命令，调用方不应再处理；其余应处理。
    pub fn accept(&self, seq: u64) -> SeqCheck {
        let check = self.seq.borrow_mut().accept(seq);
        if check == SeqCheck::Gap {
            tracing::warn!(seq, "宿主命令序号跳号");
        }
        self.send(&UiMessage::Ack { seq });
        check
    }

    /// 在“收件箱已空、没有窗口、没有活动”时请求退出：同一时刻只保留一个未决请求。
    ///
    /// # 返回
    /// 本次是否发出了请求。
    pub fn request_exit(&self) -> bool {
        if self.exit.get() == ExitState::Requested || self.closing.get() {
            return false;
        }
        let request = self.seq.borrow().exit_request();
        self.exit.set(ExitState::Requested);
        self.send(&request);
        true
    }

    /// 宿主拒绝了退出：回到空闲，等下一个事件处理完再判断（被拒绝说明宿主还有命令在路上）。
    pub fn exit_denied(&self) {
        self.exit.set(ExitState::Idle);
    }

    /// 宿主通知关闭（同意退出或要求关闭）：此后会话断开是预期内的。
    pub fn begin_closing(&self) {
        self.closing.set(true);
    }

    /// 是否处于预期内的关闭中。
    pub fn is_closing(&self) -> bool {
        self.closing.get()
    }

    /// 登记一次热键重注册请求（等宿主回报）。
    ///
    /// # 参数
    /// - `rev`：发给宿主的快照修订号。
    /// - `pending`：变更的配置键与旧值。
    pub fn register_apply(&self, rev: u64, pending: PendingApply) {
        self.pending.borrow_mut().insert(rev, pending);
    }

    /// 取走一次待回报项（收到回报或等待超时时调用）；没有登记返回 `None`。
    ///
    /// # 参数
    /// - `rev`：快照修订号。
    pub fn take_apply(&self, rev: u64) -> Option<PendingApply> {
        self.pending.borrow_mut().remove(&rev)
    }

    /// 写端的只读引用（测试读取记录用）。
    pub fn sink(&self) -> &W {
        &self.sink
    }
}

/// 在宿主回报里找某个配置键的失败原因。
///
/// 语音转文字的触发模式键本身不是热键，它的失败看两个语音热键（与单进程模式 `hotkey_attempt_failed` 一致）。
///
/// # 参数
/// - `results`：宿主回报的逐配置键结果。
/// - `config_key`：设置页变更的配置键。
///
/// # 返回
/// 有失败返回原因文本（多个以 `; ` 连接，宿主没给原因用占位文案）；没有失败返回 `None`。
pub fn apply_failure(results: &[HotkeyResult], config_key: &str) -> Option<String> {
    use crate::app_runtime::{
        DICTATION_HOLD_HOTKEY_CONFIG_KEY, DICTATION_TOGGLE_HOTKEY_CONFIG_KEY,
        DICTATION_TRIGGER_MODE_CONFIG_KEY,
    };
    let related: &[&str] = if config_key == DICTATION_TRIGGER_MODE_CONFIG_KEY {
        &[
            DICTATION_TOGGLE_HOTKEY_CONFIG_KEY,
            DICTATION_HOLD_HOTKEY_CONFIG_KEY,
        ]
    } else {
        &[]
    };
    let failures: Vec<String> = results
        .iter()
        .filter(|r| {
            !r.ok && (r.config_key == config_key || related.contains(&r.config_key.as_str()))
        })
        .map(|r| {
            r.error
                .clone()
                .unwrap_or_else(|| MISSING_REASON.to_string())
        })
        .collect();
    (!failures.is_empty()).then(|| failures.join("; "))
}

/// 宿主没给失败原因时用的占位文案。
const MISSING_REASON: &str = "register failed";

/// 连接宿主的会话管道并发出 `hello`；成功后起一个读线程，把宿主消息投进主线程收件箱。
///
/// # 参数
/// - `launch`：宿主传来的会话参数（`--ui --session <名> --token <令牌>`）。
/// - `on_event`：读线程里收到宿主事件时调用（应只做轻量转发，如 `inbox.push`）。
///
/// # 返回
/// 会话客户端（写端）；连接失败或 `hello` 发不出去返回原因。
pub fn connect(
    launch: &UiLaunch,
    on_event: impl Fn(HostEvent) + Send + 'static,
) -> Result<HostLink<PipeWriter>, String> {
    let pipe = pipe_name_for(&launch.session)?;
    let mut client = SessionPipeClient::connect(&pipe)?;
    let writer = client.writer();
    let link = HostLink::new(writer.clone());
    link.send(&UiMessage::Hello {
        pid: std::process::id(),
        proto: PROTOCOL_VERSION,
        token: launch.token.clone(),
    });
    std::thread::Builder::new()
        .name("host-link".into())
        .spawn(move || {
            loop {
                match client.read_line(None) {
                    Ok(Some(line)) => match decode_line::<HostMessage>(&line) {
                        Ok(message) => on_event(HostEvent::Message(message)),
                        Err(e) => tracing::warn!(error = %e, "宿主消息无法解析，已忽略"),
                    },
                    Ok(None) => {}
                    Err(e) => {
                        tracing::info!(reason = %e, "宿主会话已断开");
                        on_event(HostEvent::Lost);
                        return;
                    }
                }
            }
        })
        .map_err(|e| format!("创建会话读线程失败: {e}"))?;
    Ok(link)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    /// 记录写出的行。
    #[derive(Default)]
    struct Recorder {
        /// 写出的行。
        lines: RefCell<Vec<String>>,
    }

    impl LineSink for Recorder {
        fn send_line(&self, line: &str) -> Result<(), String> {
            self.lines.borrow_mut().push(line.to_string());
            Ok(())
        }
    }

    /// 取已写出的消息（解码回 UiMessage）。
    fn sent(link: &HostLink<Recorder>) -> Vec<UiMessage> {
        link.sink()
            .lines
            .borrow()
            .iter()
            .map(|line| decode_line::<UiMessage>(line).unwrap())
            .collect()
    }

    /// 收到命令：先登记序号再回 ack；宿主重发的旧序号是 Duplicate（仍回 ack，不重复处理）。
    #[test]
    fn accept_acks_and_dedupes() {
        let link = HostLink::new(Recorder::default());
        assert_eq!(link.accept(1), SeqCheck::Fresh);
        assert_eq!(link.accept(2), SeqCheck::Fresh);
        assert_eq!(link.accept(2), SeqCheck::Duplicate);
        assert_eq!(link.accept(5), SeqCheck::Gap);
        assert_eq!(
            sent(&link),
            vec![
                UiMessage::Ack { seq: 1 },
                UiMessage::Ack { seq: 2 },
                UiMessage::Ack { seq: 2 },
                UiMessage::Ack { seq: 5 },
            ]
        );
    }

    /// 退出请求：带上已处理的最后序号；未决期间不重复发；被拒绝后可以再请求。
    #[test]
    fn exit_request_is_single_flight_and_retries_after_denial() {
        let link = HostLink::new(Recorder::default());
        link.accept(1);
        assert!(link.request_exit());
        assert!(!link.request_exit(), "未决请求期间不重复发");
        link.exit_denied();
        link.accept(2);
        assert!(link.request_exit());
        let requests: Vec<_> = sent(&link)
            .into_iter()
            .filter(|m| matches!(m, UiMessage::ExitRequest { .. }))
            .collect();
        assert_eq!(
            requests,
            vec![
                UiMessage::ExitRequest { last_seq: 1 },
                UiMessage::ExitRequest { last_seq: 2 }
            ]
        );
    }

    /// 宿主已通知关闭后不再请求退出，且会话断开被视为预期内。
    #[test]
    fn closing_suppresses_exit_requests() {
        let link = HostLink::new(Recorder::default());
        assert!(!link.is_closing());
        link.begin_closing();
        assert!(link.is_closing());
        assert!(!link.request_exit());
    }

    /// 待回报登记：取一次就没了；没登记的修订号取不到。
    #[test]
    fn pending_apply_is_taken_once() {
        let link = HostLink::new(Recorder::default());
        link.register_apply(
            7,
            PendingApply {
                config_key: "global_shortcuts/screenshot",
                previous: serde_json::json!(["F1"]),
            },
        );
        assert!(link.take_apply(8).is_none());
        let taken = link.take_apply(7).unwrap();
        assert_eq!(taken.config_key, "global_shortcuts/screenshot");
        assert!(link.take_apply(7).is_none());
    }

    /// 在宿主回报里按配置键找失败：普通键只看自己；触发模式键看两个语音热键；成功返回 None。
    #[test]
    fn apply_failure_matches_related_keys() {
        use crate::app_runtime::{
            DICTATION_HOLD_HOTKEY_CONFIG_KEY, DICTATION_TOGGLE_HOTKEY_CONFIG_KEY,
            DICTATION_TRIGGER_MODE_CONFIG_KEY, SCREENSHOT_HOTKEY_CONFIG_KEY,
        };
        let result = |key: &str, ok: bool, error: Option<&str>| HotkeyResult {
            config_key: key.to_string(),
            ok,
            error: error.map(str::to_string),
        };
        let results = vec![
            result(SCREENSHOT_HOTKEY_CONFIG_KEY, false, Some("F1: 被占用")),
            result(
                DICTATION_TOGGLE_HOTKEY_CONFIG_KEY,
                false,
                Some("F9: 被占用"),
            ),
            result(DICTATION_HOLD_HOTKEY_CONFIG_KEY, true, None),
            result("global_shortcuts/other", true, None),
        ];
        assert_eq!(
            apply_failure(&results, SCREENSHOT_HOTKEY_CONFIG_KEY).as_deref(),
            Some("F1: 被占用")
        );
        assert_eq!(apply_failure(&results, "global_shortcuts/other"), None);
        assert_eq!(
            apply_failure(&results, DICTATION_TRIGGER_MODE_CONFIG_KEY).as_deref(),
            Some("F9: 被占用")
        );
        let no_reason = vec![result(SCREENSHOT_HOTKEY_CONFIG_KEY, false, None)];
        assert_eq!(
            apply_failure(&no_reason, SCREENSHOT_HOTKEY_CONFIG_KEY).as_deref(),
            Some(MISSING_REASON)
        );
        assert_eq!(apply_failure(&[], SCREENSHOT_HOTKEY_CONFIG_KEY), None);
    }

    /// 真实管道往返：宿主端推送命令，客户端读线程把它变成 `HostEvent`；客户端的 hello 到达宿主端；
    /// 宿主端关闭后客户端收到 `Lost`。
    #[test]
    fn connect_roundtrip_over_real_pipe() {
        use snow_platform::single_instance::session_pipe::{SessionEvent, SessionPipeServer};
        use std::sync::Mutex;
        use std::sync::mpsc::channel;
        use std::time::Duration;
        let app_id = format!("cisox.test.host-link.{}", std::process::id());
        let name = pipe_name_for(&app_id).unwrap();
        let (server_tx, server_rx) = channel();
        let server_tx = Mutex::new(server_tx);
        let server = SessionPipeServer::start(&name, move |event| {
            let _ = server_tx.lock().unwrap().send(event);
        })
        .unwrap();
        let token = "ef".repeat(32);
        let launch = UiLaunch::new(Some(app_id), Some(token.clone())).unwrap();
        let (event_tx, event_rx) = channel();
        let event_tx = Mutex::new(event_tx);
        let link = connect(&launch, move |event| {
            let _ = event_tx.lock().unwrap().send(event);
        })
        .unwrap();
        // 宿主端先看到连接，再看到 hello
        let wait = Duration::from_secs(3);
        assert!(matches!(
            server_rx.recv_timeout(wait).unwrap(),
            SessionEvent::Connected { .. }
        ));
        let SessionEvent::Line(hello) = server_rx.recv_timeout(wait).unwrap() else {
            panic!("第二个事件应是 hello 行");
        };
        assert_eq!(
            decode_line::<UiMessage>(&hello).unwrap(),
            UiMessage::Hello {
                pid: std::process::id(),
                proto: PROTOCOL_VERSION,
                token
            }
        );
        // 宿主推送一条命令
        let command = HostMessage::Command {
            seq: 1,
            id: "capture".into(),
            origin: None,
        };
        server
            .writer()
            .send_line(encode_line(&command).unwrap().trim_end())
            .unwrap();
        assert_eq!(
            event_rx.recv_timeout(wait).unwrap(),
            HostEvent::Message(command)
        );
        // 客户端回 ack
        assert_eq!(link.accept(1), SeqCheck::Fresh);
        let SessionEvent::Line(ack) = server_rx.recv_timeout(wait).unwrap() else {
            panic!("应收到 ack 行");
        };
        assert_eq!(
            decode_line::<UiMessage>(&ack).unwrap(),
            UiMessage::Ack { seq: 1 }
        );
        // 宿主端消失：客户端上报 Lost
        drop(server);
        assert_eq!(event_rx.recv_timeout(wait).unwrap(), HostEvent::Lost);
    }
}
