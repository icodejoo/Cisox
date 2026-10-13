//! 状态机的确定性测试：时间、进程、管道全部由测试注入，不起真进程。

use super::*;
use serde_json::json;
use snow_host_protocol::message::ActionOrigin;
use snow_host_protocol::{
    ACTION_KEEP_ALIVE, COMMAND_EXPIRY, CRASH_BACKOFF, MAX_QUEUED_COMMANDS, PROTOCOL_VERSION,
    TOKEN_BYTES,
};
use std::time::Duration;

/// 测试用 UI 进程 ID。
const PID: u32 = 4242;

/// 测试用令牌（合法的小写十六进制）。
fn token() -> String {
    "ab".repeat(TOKEN_BYTES)
}

/// 构造动作消息。
fn action(id: &str) -> Outbound {
    Outbound::Action {
        id: id.to_string(),
        origin: Some(ActionOrigin::Hotkey),
    }
}

/// 构造合法的 hello。
fn hello(pid: u32) -> UiMessage {
    UiMessage::Hello {
        pid,
        proto: PROTOCOL_VERSION,
        token: token(),
    }
}

/// 取出效果里所有写管道的消息。
fn sent(effects: &[Effect]) -> Vec<HostMessage> {
    effects
        .iter()
        .filter_map(|e| match e {
            Effect::Send { message, .. } => Some(message.clone()),
            _ => None,
        })
        .collect()
}

/// 效果里是否包含某类效果。
fn has(effects: &[Effect], pred: impl Fn(&Effect) -> bool) -> bool {
    effects.iter().any(pred)
}

/// 把状态机推进到 Running：提交首条动作 → 拉起 → 握手；返回状态机、握手后的效果和代数。
fn running_with(first: &str, t0: Instant) -> (HostMachine, Vec<Effect>, u64) {
    let mut machine = HostMachine::new();
    let spawn = machine.submit(t0, action(first));
    let Effect::SpawnUi { generation } = spawn[0] else {
        panic!("首条命令应触发拉起: {spawn:?}");
    };
    machine.on_spawned(generation, PID, token());
    machine.on_pipe_connected(generation, PID);
    let effects = machine.on_ui_message(t0, generation, hello(PID));
    assert_eq!(machine.kind(), StateKind::Running);
    (machine, effects, generation)
}

/// 冷启动：UI 不在时第一条命令触发拉起并入队，拉起结果回来后授权前台。
#[test]
fn cold_submit_spawns_and_queues() {
    let t0 = Instant::now();
    let mut machine = HostMachine::new();
    let effects = machine.submit(t0, action("capture"));
    assert_eq!(effects, vec![Effect::SpawnUi { generation: 1 }]);
    assert_eq!(machine.kind(), StateKind::Starting);
    assert_eq!(machine.queued(), 1);
    let granted = machine.on_spawned(1, PID, token());
    assert_eq!(granted, vec![Effect::GrantForeground { pid: PID }]);
}

/// 拉起时的启动提示只看队首：语音命令在前才提示；先截图后语音不提示；没有拉起就保持上次的值。
#[test]
fn spawn_hint_only_looks_at_queue_head() {
    let t0 = Instant::now();
    let mut machine = HostMachine::new();
    assert_eq!(machine.spawn_hint(), UiHint::None);
    machine.submit(t0, action("toggle_dictation"));
    assert_eq!(machine.spawn_hint(), UiHint::Dictation);

    let mut machine = HostMachine::new();
    machine.submit(t0, action("capture"));
    machine.submit(t0, action("start_dictation"));
    assert_eq!(machine.spawn_hint(), UiHint::None, "队首是截图");

    let mut machine = HostMachine::new();
    machine.submit(t0, action("start_dictation"));
    machine.submit(t0, action("stop_dictation"));
    assert_eq!(machine.spawn_hint(), UiHint::Dictation);
}

/// UI 启动期间连发命令：全部入队保序，握手完成后按序号 1..n 依次写出，且不重复拉起。
#[test]
fn burst_during_startup_is_flushed_in_order() {
    let t0 = Instant::now();
    let mut machine = HostMachine::new();
    machine.submit(t0, action("a"));
    for id in ["b", "c", "d"] {
        let effects = machine.submit(t0, action(id));
        assert!(effects.is_empty(), "启动中只入队，不再拉起: {effects:?}");
    }
    assert_eq!(machine.queued(), 4);
    machine.on_spawned(1, PID, token());
    machine.on_pipe_connected(1, PID);
    let effects = machine.on_ui_message(t0, 1, hello(PID));
    let wire: Vec<_> = sent(&effects)
        .into_iter()
        .map(|m| match m {
            HostMessage::Command { seq, id, .. } => (seq, id),
            other => panic!("意外的消息 {other:?}"),
        })
        .collect();
    assert_eq!(
        wire,
        [
            (1, "a".to_string()),
            (2, "b".into()),
            (3, "c".into()),
            (4, "d".into())
        ]
    );
    assert_eq!(machine.queued(), 0);
    assert_eq!(machine.inflight(), 4);
}

/// 语音按住说话：按下与松开保序，松开比 UI 起来得早也不会丢。
#[test]
fn hold_to_talk_press_release_order_survives_startup() {
    let t0 = Instant::now();
    let mut machine = HostMachine::new();
    machine.submit(t0, action("start_dictation"));
    machine.submit(t0, action("stop_dictation"));
    machine.on_spawned(1, PID, token());
    let effects = machine.on_ui_message(t0, 1, hello(PID));
    let ids: Vec<_> = sent(&effects)
        .into_iter()
        .filter_map(|m| match m {
            HostMessage::Command { id, .. } => Some(id),
            _ => None,
        })
        .collect();
    assert_eq!(ids, ["start_dictation", "stop_dictation"]);
}

/// 运行中：命令立即写管道，先授权前台再写；非动作消息不授权。
#[test]
fn running_submit_sends_immediately_with_foreground_grant() {
    let t0 = Instant::now();
    let (mut machine, _, generation) = running_with("capture", t0);
    let effects = machine.submit(t0, action("settings"));
    assert_eq!(
        effects,
        vec![
            Effect::GrantForeground { pid: PID },
            Effect::Send {
                generation,
                message: HostMessage::Command {
                    seq: 2,
                    id: "settings".into(),
                    origin: Some(ActionOrigin::Hotkey)
                }
            }
        ]
    );
    let effects = machine.submit(
        t0,
        Outbound::SetConfig {
            key: "k".into(),
            value: json!(true),
        },
    );
    assert!(!has(&effects, |e| matches!(
        e,
        Effect::GrantForeground { .. }
    )));
    assert_eq!(sent(&effects).len(), 1);
}

/// 握手：令牌错、PID 错、协议版本错、第一条不是 hello、管道客户端 PID 与拉起的不符，都被拒绝：
/// 结束进程、关会话、计一次崩溃、提示，命令队列清空，进入 Exiting 等进程真正退出。
#[test]
fn bad_handshakes_are_rejected() {
    let t0 = Instant::now();
    let cases: Vec<(&str, UiMessage, Option<u32>)> = vec![
        (
            "令牌错",
            UiMessage::Hello {
                pid: PID,
                proto: PROTOCOL_VERSION,
                token: "cd".repeat(TOKEN_BYTES),
            },
            None,
        ),
        ("PID 错", hello(PID + 1), None),
        (
            "协议版本错",
            UiMessage::Hello {
                pid: PID,
                proto: PROTOCOL_VERSION + 1,
                token: token(),
            },
            None,
        ),
        ("首条不是 hello", UiMessage::Ack { seq: 1 }, None),
        ("管道客户端 PID 不符", hello(PID), Some(PID + 7)),
    ];
    for (name, message, connected) in cases {
        let mut machine = HostMachine::new();
        machine.submit(t0, action("capture"));
        machine.on_spawned(1, PID, token());
        if let Some(connected) = connected {
            machine.on_pipe_connected(1, connected);
        }
        let effects = machine.on_ui_message(t0, 1, message);
        assert!(
            has(&effects, |e| *e == Effect::KillUi { pid: PID }),
            "{name}: 应结束进程"
        );
        assert!(
            has(&effects, |e| *e == Effect::CloseSession { generation: 1 }),
            "{name}: 应关会话"
        );
        assert!(
            has(&effects, |e| *e == Effect::Notice(Notice::UiStartFailed)),
            "{name}: 应提示失败"
        );
        assert!(
            sent(&effects).is_empty(),
            "{name}: 握手失败不得投递任何命令"
        );
        assert_eq!(machine.kind(), StateKind::Exiting, "{name}");
        assert_eq!(machine.queued(), 0, "{name}");
    }
}

/// 握手 10 秒超时：强制结束、提示失败、队列清空；进程真正退出前不允许再拉新的。
#[test]
fn hello_timeout_kills_and_waits_for_exit() {
    let t0 = Instant::now();
    let mut machine = HostMachine::new();
    machine.submit(t0, action("capture"));
    machine.on_spawned(1, PID, token());
    assert_eq!(machine.next_deadline(), Some(t0 + HELLO_TIMEOUT));
    assert!(machine.on_tick(t0 + Duration::from_secs(9)).is_empty());
    let effects = machine.on_tick(t0 + HELLO_TIMEOUT);
    assert!(has(&effects, |e| *e == Effect::KillUi { pid: PID }));
    assert!(has(&effects, |e| *e == Effect::Notice(Notice::UiStartFailed)));
    assert_eq!(machine.kind(), StateKind::Exiting);
    // 进程退出前来了新命令：只入队，不拉起第二个 UI
    let effects = machine.submit(t0 + HELLO_TIMEOUT, action("capture"));
    assert!(effects.is_empty());
    assert_eq!(machine.queued(), 1);
    // 进程真正退出后才拉起新的，并带上排队的命令
    let effects = machine.on_ui_exited(t0 + HELLO_TIMEOUT, 1);
    assert!(has(&effects, |e| matches!(
        e,
        Effect::SpawnUi { generation: 2 }
    )));
    assert!(has(&effects, |e| *e == Effect::UiGone));
}

/// 拉起失败（找不到 exe 等）：提示、清队列、回到 Absent。
#[test]
fn spawn_failure_notifies_and_returns_to_absent() {
    let t0 = Instant::now();
    let mut machine = HostMachine::new();
    machine.submit(t0, action("capture"));
    let effects = machine.on_spawn_failed(t0, 1, "找不到 snow-shot-ui.exe");
    assert!(has(&effects, |e| *e == Effect::Notice(Notice::UiStartFailed)));
    assert_eq!(machine.kind(), StateKind::Absent);
    assert_eq!(machine.queued(), 0);
    // 过期代数的失败回报被忽略
    assert!(machine.on_spawn_failed(t0, 99, "x").is_empty());
}

/// 确认：累计确认，`ack{seq}` 清掉所有序号不大于它的在途消息。
#[test]
fn ack_is_cumulative() {
    let t0 = Instant::now();
    let (mut machine, _, generation) = running_with("a", t0);
    machine.submit(t0, action("b"));
    machine.submit(t0, action("c"));
    assert_eq!(machine.inflight(), 3);
    machine.on_ui_message(t0, generation, UiMessage::Ack { seq: 2 });
    assert_eq!(machine.inflight(), 1);
    machine.on_ui_message(t0, generation, UiMessage::Ack { seq: 3 });
    assert_eq!(machine.inflight(), 0);
    assert_eq!(machine.next_deadline(), None);
}

/// 确认超时：1.5 秒没确认就原样重发一次（同序号），再超时就放弃并记日志。
#[test]
fn unacked_message_is_resent_once_then_dropped() {
    let t0 = Instant::now();
    let (mut machine, _, generation) = running_with("capture", t0);
    assert_eq!(machine.next_deadline(), Some(t0 + ACK_TIMEOUT));
    assert!(machine.on_tick(t0 + Duration::from_millis(1499)).is_empty());
    let first = machine.on_tick(t0 + ACK_TIMEOUT);
    assert_eq!(
        sent(&first),
        vec![HostMessage::Command {
            seq: 1,
            id: "capture".into(),
            origin: Some(ActionOrigin::Hotkey)
        }]
    );
    assert_eq!(machine.inflight(), 1);
    let second_at = t0 + ACK_TIMEOUT * 2;
    let second = machine.on_tick(second_at);
    assert_eq!(second, vec![Effect::GaveUp { seq: 1 }]);
    assert_eq!(machine.inflight(), 0);
    assert_eq!(generation, machine.generation());
}

/// 重发后收到确认：不再放弃。
#[test]
fn ack_after_resend_cancels_giving_up() {
    let t0 = Instant::now();
    let (mut machine, _, generation) = running_with("capture", t0);
    machine.on_tick(t0 + ACK_TIMEOUT);
    machine.on_ui_message(t0 + ACK_TIMEOUT, generation, UiMessage::Ack { seq: 1 });
    assert!(machine.on_tick(t0 + ACK_TIMEOUT * 3).is_empty());
}

/// 退出仲裁（同意）：UI 处理完宿主投递的全部命令，回 exit_ok 并转入 Exiting；
/// 此后的命令只入队，等旧进程退出后才拉起新 UI 并按序投递。
#[test]
fn exit_ok_then_commands_wait_for_old_process() {
    let t0 = Instant::now();
    let (mut machine, _, generation) = running_with("capture", t0);
    machine.on_ui_message(t0, generation, UiMessage::Ack { seq: 1 });
    let effects = machine.on_ui_message(t0, generation, UiMessage::ExitRequest { last_seq: 1 });
    assert_eq!(
        sent(&effects),
        vec![HostMessage::ExitOk],
        "只回 exit_ok，不投递别的"
    );
    assert_eq!(machine.kind(), StateKind::Exiting);
    // 同意退出之后来了新热键
    for id in ["capture", "settings"] {
        assert!(machine.submit(t0, action(id)).is_empty());
    }
    assert_eq!(machine.queued(), 2);
    // 旧进程退出：拉起新 UI（代数 +1），队列带过去
    let effects = machine.on_ui_exited(t0, generation);
    assert!(has(&effects, |e| *e == Effect::UiGone));
    assert!(has(&effects, |e| *e
        == Effect::SpawnUi {
            generation: generation + 1
        }));
    assert!(
        !has(&effects, |e| matches!(e, Effect::Notice(_))),
        "正常退出不算崩溃"
    );
    machine.on_spawned(generation + 1, PID + 1, token());
    machine.on_pipe_connected(generation + 1, PID + 1);
    let effects = machine.on_ui_message(t0, generation + 1, hello(PID + 1));
    let seqs: Vec<_> = sent(&effects)
        .into_iter()
        .map(|m| m.seq().expect("命令带序号"))
        .collect();
    assert_eq!(seqs, [1, 2], "新会话的序号从 1 重新开始");
}

/// 退出竞态：UI 请求退出的同时宿主又投递了新命令（序号不等），必须拒绝；
/// UI 处理完新命令后再次请求才同意。宿主是唯一仲裁方，不存在“命令写进已决定退出的 UI”。
#[test]
fn exit_racing_with_new_command_is_denied_then_granted() {
    let t0 = Instant::now();
    let (mut machine, _, generation) = running_with("capture", t0);
    machine.on_ui_message(t0, generation, UiMessage::Ack { seq: 1 });
    // UI 发出 exit_request{last_seq: 1} 的同时，宿主已收到热键并写出了第 2 条
    let racing = machine.submit(t0, action("settings"));
    assert_eq!(sent(&racing).len(), 1);
    let effects = machine.on_ui_message(t0, generation, UiMessage::ExitRequest { last_seq: 1 });
    assert_eq!(sent(&effects), vec![HostMessage::ExitDenied]);
    assert_eq!(machine.kind(), StateKind::Running, "拒绝后仍在运行");
    // UI 处理了第 2 条，再次请求
    machine.on_ui_message(t0, generation, UiMessage::Ack { seq: 2 });
    let effects = machine.on_ui_message(t0, generation, UiMessage::ExitRequest { last_seq: 2 });
    assert_eq!(sent(&effects), vec![HostMessage::ExitOk]);
    assert_eq!(machine.kind(), StateKind::Exiting);
}

/// 超前的退出请求（UI 声称处理了没发过的序号，协议错误）也被拒绝。
#[test]
fn exit_request_ahead_of_sent_is_denied() {
    let t0 = Instant::now();
    let (mut machine, _, generation) = running_with("capture", t0);
    let effects = machine.on_ui_message(t0, generation, UiMessage::ExitRequest { last_seq: 9 });
    assert_eq!(sent(&effects), vec![HostMessage::ExitDenied]);
    assert_eq!(machine.kind(), StateKind::Running);
}

/// UI 一条命令都没处理过就请求退出（0 对 0）：同意。
#[test]
fn exit_with_nothing_sent_is_granted() {
    let t0 = Instant::now();
    let mut machine = HostMachine::new();
    machine.submit(t0, Outbound::RestorePins);
    machine.on_spawned(1, PID, token());
    // 队列在握手时才冲出；这里模拟 hello 之后队列已空的 UI（先请求退出前宿主没发任何命令）
    machine.queue.clear();
    machine.on_ui_message(t0, 1, hello(PID));
    let effects = machine.on_ui_message(t0, 1, UiMessage::ExitRequest { last_seq: 0 });
    assert_eq!(sent(&effects), vec![HostMessage::ExitOk]);
}

/// UI 被杀（运行中进程退出且没走过 exit_ok）：不重放未确认的命令，计一次崩溃；
/// 之后的新命令重新拉起，序号从 1 开始。
#[test]
fn killed_ui_is_not_replayed() {
    let t0 = Instant::now();
    let (mut machine, _, generation) = running_with("capture", t0);
    machine.submit(t0, action("settings"));
    assert_eq!(machine.inflight(), 2);
    let effects = machine.on_ui_exited(t0, generation);
    assert!(has(&effects, |e| *e == Effect::CloseSession { generation }));
    assert!(
        sent(&effects).is_empty(),
        "不重放未确认命令（重放可能二次截图）"
    );
    assert!(!has(&effects, |e| matches!(e, Effect::SpawnUi { .. })));
    assert_eq!(machine.kind(), StateKind::Absent);
    assert_eq!(machine.inflight(), 0);
    let effects = machine.submit(t0, action("capture"));
    assert_eq!(
        effects,
        vec![Effect::SpawnUi {
            generation: generation + 1
        }]
    );
}

/// UI 崩溃前报告过有贴图：自动拉起并恢复贴图（未触发熔断时）。
#[test]
fn crash_with_pins_restores_them() {
    let t0 = Instant::now();
    let (mut machine, _, generation) = running_with("capture", t0);
    machine.on_ui_message(
        t0,
        generation,
        UiMessage::Activity {
            windows: 2,
            pins: 2,
            busy_reasons: vec![],
        },
    );
    let effects = machine.on_ui_exited(t0, generation);
    assert!(has(&effects, |e| matches!(e, Effect::SpawnUi { .. })));
    machine.on_spawned(generation + 1, PID, token());
    let effects = machine.on_ui_message(t0, generation + 1, hello(PID));
    assert_eq!(sent(&effects), vec![HostMessage::RestorePins { seq: 1 }]);
}

/// 熔断：60 秒内崩溃 3 次进入退避，期间命令被丢弃并提示；5 分钟后恢复拉起。
#[test]
fn crash_loop_trips_breaker_and_recovers() {
    let t0 = Instant::now();
    let mut machine = HostMachine::new();
    let mut last_effects = Vec::new();
    for round in 0..3u64 {
        let at = t0 + Duration::from_secs(10 * round);
        let spawn = machine.submit(at, action("capture"));
        let Effect::SpawnUi { generation } = spawn[0] else {
            panic!("第 {round} 轮应拉起: {spawn:?}");
        };
        machine.on_spawned(generation, PID, token());
        machine.on_ui_message(at, generation, hello(PID));
        last_effects = machine.on_ui_exited(at, generation);
    }
    assert!(has(&last_effects, |e| *e == Effect::Notice(Notice::UiCrashLoop)));
    assert_eq!(machine.kind(), StateKind::Backoff);
    // 退避期间：命令被丢弃，不拉起，提示崩溃循环
    let mid = t0 + Duration::from_secs(120);
    let effects = machine.submit(mid, action("capture"));
    assert_eq!(effects, vec![Effect::Notice(Notice::UiCrashLoop)]);
    assert_eq!(machine.queued(), 0);
    // 退避结束（第 3 次崩溃时刻 + 5 分钟）：恢复拉起
    let until = t0 + Duration::from_secs(20) + CRASH_BACKOFF;
    assert_eq!(machine.next_deadline(), Some(until));
    let effects = machine.submit(until, action("capture"));
    assert!(has(&effects, |e| matches!(e, Effect::SpawnUi { .. })));
}

/// 崩溃间隔超过 60 秒不触发熔断。
#[test]
fn spread_out_crashes_do_not_trip() {
    let t0 = Instant::now();
    let mut machine = HostMachine::new();
    for round in 0..6u64 {
        let at = t0 + Duration::from_secs(70 * round);
        let spawn = machine.submit(at, action("capture"));
        let Effect::SpawnUi { generation } = spawn[0] else {
            panic!("第 {round} 轮应拉起");
        };
        machine.on_spawned(generation, PID, token());
        machine.on_ui_message(at, generation, hello(PID));
        machine.on_ui_exited(at, generation);
        assert_eq!(machine.kind(), StateKind::Absent, "第 {round} 轮");
    }
}

/// 握手前就退出的 UI 也计入熔断（启动失败循环同样要刹车）。
#[test]
fn startup_failures_count_toward_breaker() {
    let t0 = Instant::now();
    let mut machine = HostMachine::new();
    for round in 0..3u64 {
        let spawn = machine.submit(t0, action("capture"));
        let Effect::SpawnUi { generation } = spawn[0] else {
            panic!("第 {round} 轮应拉起: {spawn:?}");
        };
        machine.on_spawned(generation, PID, token());
        machine.on_ui_exited(t0, generation);
    }
    assert_eq!(machine.kind(), StateKind::Backoff);
}

/// 过期会话（旧代数）的消息与退出事件被忽略，不影响新会话。
#[test]
fn stale_generation_events_are_ignored() {
    let t0 = Instant::now();
    let (mut machine, _, generation) = running_with("capture", t0);
    machine.on_ui_message(t0, generation, UiMessage::ExitRequest { last_seq: 1 });
    machine.on_ui_exited(t0, generation);
    machine.submit(t0, action("capture"));
    let new_generation = machine.generation();
    assert_ne!(new_generation, generation);
    machine.on_spawned(new_generation, PID + 1, token());
    // 旧会话迟到的 hello / 退出
    assert!(machine.on_ui_message(t0, generation, hello(PID)).is_empty());
    assert!(machine.on_ui_exited(t0, generation).is_empty());
    assert_eq!(machine.kind(), StateKind::Starting);
}

/// UI 发来的信息类消息被转发给运行时：托盘提示、快照变更、热键重注册、活动状态。
#[test]
fn informational_messages_are_forwarded() {
    let t0 = Instant::now();
    let (mut machine, _, generation) = running_with("capture", t0);
    for message in [
        UiMessage::Tooltip { text: "hi".into() },
        UiMessage::SpecChanged { rev: 5 },
        UiMessage::HotkeysApply { rev: 6 },
    ] {
        let effects = machine.on_ui_message(t0, generation, message.clone());
        assert_eq!(effects, vec![Effect::FromUi(message)]);
    }
}

/// 退出阶段仍转发信息类消息（UI 退出前最后写的快照 / 提示不能丢），但不再理会确认和退出请求。
#[test]
fn exiting_forwards_informational_but_ignores_control() {
    let t0 = Instant::now();
    let (mut machine, _, generation) = running_with("capture", t0);
    machine.on_ui_message(t0, generation, UiMessage::ExitRequest { last_seq: 1 });
    assert_eq!(machine.kind(), StateKind::Exiting);
    let message = UiMessage::SpecChanged { rev: 9 };
    assert_eq!(
        machine.on_ui_message(t0, generation, message.clone()),
        vec![Effect::FromUi(message)]
    );
    assert!(
        machine
            .on_ui_message(t0, generation, UiMessage::ExitRequest { last_seq: 1 })
            .is_empty()
    );
}

/// 退出超时：同意退出后 10 秒进程还在，只强制结束一次（最后手段）。
#[test]
fn exit_wait_timeout_kills_once() {
    let t0 = Instant::now();
    let (mut machine, _, generation) = running_with("capture", t0);
    machine.on_ui_message(t0, generation, UiMessage::Ack { seq: 1 });
    machine.on_ui_message(t0, generation, UiMessage::ExitRequest { last_seq: 1 });
    assert_eq!(machine.next_deadline(), Some(t0 + EXIT_WAIT_TIMEOUT));
    assert!(machine.on_tick(t0 + Duration::from_secs(9)).is_empty());
    assert_eq!(
        machine.on_tick(t0 + EXIT_WAIT_TIMEOUT),
        vec![Effect::KillUi { pid: PID }]
    );
    assert!(
        machine.on_tick(t0 + EXIT_WAIT_TIMEOUT * 2).is_empty(),
        "只结束一次"
    );
    assert_eq!(machine.next_deadline(), None);
}

/// 宿主退出：UI 在运行 → 先写 shutdown 并进入 Exiting，UI 退出后才让宿主退出。
#[test]
fn quit_with_running_ui_is_graceful() {
    let t0 = Instant::now();
    let (mut machine, _, generation) = running_with("capture", t0);
    let effects = machine.request_quit(t0, false);
    assert_eq!(sent(&effects), vec![HostMessage::Shutdown]);
    assert!(!has(&effects, |e| matches!(e, Effect::ExitHost { .. })));
    assert!(machine.is_quitting());
    // 退出中不再接新命令
    assert!(machine.submit(t0, action("capture")).is_empty());
    let effects = machine.on_ui_exited(t0, generation);
    assert!(has(&effects, |e| *e == Effect::ExitHost { restart: false }));
}

/// 宿主退出：UI 不在 → 立即让宿主退出；重启标志原样带出。
#[test]
fn quit_without_ui_exits_immediately() {
    let mut machine = HostMachine::new();
    let effects = machine.request_quit(Instant::now(), true);
    assert_eq!(effects, vec![Effect::ExitHost { restart: true }]);
    assert!(
        machine.request_quit(Instant::now(), false).is_empty(),
        "重复请求忽略"
    );
}

/// 宿主退出：UI 正在启动 → 握手一完成就让它优雅退出，不投递排队的命令。
#[test]
fn quit_during_startup_shuts_down_after_hello() {
    let t0 = Instant::now();
    let mut machine = HostMachine::new();
    machine.submit(t0, action("capture"));
    machine.on_spawned(1, PID, token());
    let effects = machine.request_quit(t0, false);
    assert!(effects.is_empty(), "启动中先等握手");
    let effects = machine.on_ui_message(t0, 1, hello(PID));
    assert_eq!(sent(&effects), vec![HostMessage::Shutdown]);
    assert_eq!(machine.kind(), StateKind::Exiting);
    let effects = machine.on_ui_exited(t0, 1);
    assert!(has(&effects, |e| *e == Effect::ExitHost { restart: false }));
}

/// 宿主退出：退避中也能立即退出。
#[test]
fn quit_during_backoff_exits() {
    let t0 = Instant::now();
    let mut machine = HostMachine::new();
    for _ in 0..3 {
        let spawn = machine.submit(t0, action("capture"));
        let Effect::SpawnUi { generation } = spawn[0] else {
            panic!("应拉起");
        };
        machine.on_spawned(generation, PID, token());
        machine.on_ui_exited(t0, generation);
    }
    assert_eq!(machine.kind(), StateKind::Backoff);
    let effects = machine.request_quit(t0, false);
    assert_eq!(effects, vec![Effect::ExitHost { restart: false }]);
}

/// 队列过期：UI 迟迟起不来时，30 秒前的命令在握手时不会被投递。
#[test]
fn expired_queue_entries_are_not_delivered() {
    let t0 = Instant::now();
    let mut machine = HostMachine::new();
    machine.submit(t0, action("old"));
    // 模拟启动很慢（比握手超时短，所以没被杀）：手动推进到 old 过期之后再来新命令
    machine.on_spawned(1, PID, token());
    machine.queue.expire(t0 + COMMAND_EXPIRY);
    machine.submit(t0 + COMMAND_EXPIRY, action("fresh"));
    machine.on_pipe_connected(1, PID);
    let effects = machine.on_ui_message(t0 + COMMAND_EXPIRY, 1, hello(PID));
    let ids: Vec<_> = sent(&effects)
        .into_iter()
        .filter_map(|m| match m {
            HostMessage::Command { id, .. } => Some(id),
            _ => None,
        })
        .collect();
    assert_eq!(ids, ["fresh"]);
}

/// 队列满：Exiting 期间连发超过 64 条，旧的被挤掉，新的保留。
#[test]
fn queue_overflow_while_exiting_keeps_newest() {
    let t0 = Instant::now();
    let (mut machine, _, generation) = running_with("capture", t0);
    machine.on_ui_message(t0, generation, UiMessage::Ack { seq: 1 });
    machine.on_ui_message(t0, generation, UiMessage::ExitRequest { last_seq: 1 });
    for i in 0..MAX_QUEUED_COMMANDS + 5 {
        machine.submit(t0, action(&format!("c{i}")));
    }
    assert_eq!(machine.queued(), MAX_QUEUED_COMMANDS);
    machine.on_ui_exited(t0, generation);
    let new_generation = machine.generation();
    machine.on_spawned(new_generation, PID + 1, token());
    let effects = machine.on_ui_message(t0, new_generation, hello(PID + 1));
    let ids: Vec<_> = sent(&effects)
        .into_iter()
        .filter_map(|m| match m {
            HostMessage::Command { id, .. } => Some(id),
            _ => None,
        })
        .collect();
    assert_eq!(ids.len(), MAX_QUEUED_COMMANDS);
    assert_eq!(ids.first().map(String::as_str), Some("c5"));
    assert_eq!(
        ids.last().map(String::as_str),
        Some(format!("c{}", MAX_QUEUED_COMMANDS + 4).as_str())
    );
}

/// 手势 update 在排队期间只留最新一条，Begin / Finish 不受影响。
#[test]
fn gesture_updates_are_coalesced_while_starting() {
    use snow_host_protocol::message::GesturePhase;
    let t0 = Instant::now();
    let mut machine = HostMachine::new();
    let gesture = |phase, x| Outbound::Gesture {
        phase,
        id: 1,
        action: None,
        x,
        y: 0,
    };
    machine.submit(t0, gesture(GesturePhase::Begin, 0));
    for x in 1..=50 {
        machine.submit(t0, gesture(GesturePhase::Update, x));
    }
    machine.submit(t0, gesture(GesturePhase::Finish, 50));
    assert_eq!(machine.queued(), 3);
    machine.on_spawned(1, PID, token());
    let effects = machine.on_ui_message(t0, 1, hello(PID));
    let phases: Vec<_> = sent(&effects)
        .into_iter()
        .filter_map(|m| match m {
            HostMessage::Gesture { phase, x, .. } => Some((phase, x)),
            _ => None,
        })
        .collect();
    assert_eq!(
        phases,
        [
            (GesturePhase::Begin, 0),
            (GesturePhase::Update, 50),
            (GesturePhase::Finish, 50)
        ]
    );
}

/// next_deadline 取各项里最早的一个：握手超时、队列过期、确认超时。
#[test]
fn next_deadline_is_the_earliest() {
    let t0 = Instant::now();
    let (mut machine, _, generation) = running_with("capture", t0);
    assert_eq!(machine.next_deadline(), Some(t0 + ACK_TIMEOUT));
    machine.on_ui_message(t0, generation, UiMessage::Ack { seq: 1 });
    assert_eq!(
        machine.next_deadline(),
        None,
        "空闲时没有任何待办，宿主不轮询"
    );
}

/// 快照要求 UI 常驻：UI 崩溃（没跳闸）后宿主补拉一个，握手后投递“保活”占位消息；序号从 1 开始。
#[test]
fn crash_with_keep_alive_respawns_ui() {
    let t0 = Instant::now();
    let (mut machine, _, generation) = running_with("capture", t0);
    machine.set_keep_alive(true);
    let effects = machine.on_ui_exited(t0, generation);
    assert!(has(&effects, |e| matches!(e, Effect::SpawnUi { .. })));
    machine.on_spawned(generation + 1, PID, token());
    let effects = machine.on_ui_message(t0, generation + 1, hello(PID));
    let wire = sent(&effects);
    assert_eq!(wire.len(), 1);
    assert!(matches!(
        &wire[0],
        HostMessage::Command { seq: 1, id, .. } if id == ACTION_KEEP_ALIVE
    ));
}

/// 没有常驻要求：UI 崩溃后不自己拉起（托盘态要的就是没有 UI）。
#[test]
fn crash_without_keep_alive_stays_absent() {
    let t0 = Instant::now();
    let (mut machine, _, generation) = running_with("capture", t0);
    let effects = machine.on_ui_exited(t0, generation);
    assert!(!has(&effects, |e| matches!(e, Effect::SpawnUi { .. })));
    assert_eq!(machine.kind(), StateKind::Absent);
    assert!(!machine.ui_alive());
}

/// 常驻 + 崩溃前有贴图：只拉起一个 UI，恢复贴图与保活占位都排进队列（贴图在前）。
#[test]
fn crash_with_pins_and_keep_alive_spawns_once() {
    let t0 = Instant::now();
    let (mut machine, _, generation) = running_with("capture", t0);
    machine.set_keep_alive(true);
    machine.on_ui_message(
        t0,
        generation,
        UiMessage::Activity {
            windows: 1,
            pins: 1,
            busy_reasons: vec![],
        },
    );
    let effects = machine.on_ui_exited(t0, generation);
    assert_eq!(
        effects
            .iter()
            .filter(|e| matches!(e, Effect::SpawnUi { .. }))
            .count(),
        1
    );
    machine.on_spawned(generation + 1, PID, token());
    let wire = sent(&machine.on_ui_message(t0, generation + 1, hello(PID)));
    assert!(matches!(wire[0], HostMessage::RestorePins { seq: 1 }));
    assert!(matches!(&wire[1], HostMessage::Command { seq: 2, .. }));
}

/// 常驻 + 连续崩溃跳闸：退避期间不拉起；退避结束的那次 tick 补拉一个。
#[test]
fn keep_alive_respawns_after_backoff_ends() {
    let t0 = Instant::now();
    let mut machine = HostMachine::new();
    machine.set_keep_alive(true);
    let spawn = machine.submit(t0, action("capture"));
    let Effect::SpawnUi { mut generation } = spawn[0] else {
        panic!("首条命令应拉起: {spawn:?}");
    };
    for round in 0..3u64 {
        let at = t0 + Duration::from_secs(10 * round);
        machine.on_spawned(generation, PID, token());
        machine.on_ui_message(at, generation, hello(PID));
        let effects = machine.on_ui_exited(at, generation);
        if round < 2 {
            // 没跳闸：常驻要求让宿主立刻补拉
            let Some(Effect::SpawnUi { generation: next }) = effects
                .iter()
                .find(|e| matches!(e, Effect::SpawnUi { .. }))
                .cloned()
            else {
                panic!("第 {round} 轮应补拉: {effects:?}");
            };
            generation = next;
        } else {
            assert!(!has(&effects, |e| matches!(e, Effect::SpawnUi { .. })));
        }
    }
    assert_eq!(machine.kind(), StateKind::Backoff);
    assert!(
        machine
            .on_tick(t0 + Duration::from_secs(60))
            .iter()
            .all(|e| !matches!(e, Effect::SpawnUi { .. })),
        "退避期间不拉起"
    );
    let until = t0 + Duration::from_secs(20) + CRASH_BACKOFF;
    let effects = machine.on_tick(until);
    assert!(has(&effects, |e| matches!(e, Effect::SpawnUi { .. })));
    assert_eq!(machine.kind(), StateKind::Starting);
}

/// 宿主退出流程里不补拉：常驻要求不能挡住退出。
#[test]
fn keep_alive_does_not_block_quit() {
    let t0 = Instant::now();
    let (mut machine, _, generation) = running_with("capture", t0);
    machine.set_keep_alive(true);
    machine.request_quit(t0, false);
    let effects = machine.on_ui_exited(t0, generation);
    assert!(!has(&effects, |e| matches!(e, Effect::SpawnUi { .. })));
    assert!(has(&effects, |e| *e == Effect::ExitHost { restart: false }));
}

/// 手势 update 不等确认、不进重发表：连发 100 条 update 只留下 begin / finish 两条待确认，
/// 且确认超时只重发这两条。
#[test]
fn gesture_updates_are_not_tracked_for_ack() {
    use snow_host_protocol::message::GesturePhase;
    let t0 = Instant::now();
    let (mut machine, _, generation) = running_with("capture", t0);
    machine.on_ui_message(t0, generation, UiMessage::Ack { seq: 1 });
    let gesture = |phase, x| Outbound::Gesture {
        phase,
        id: 7,
        action: (phase == GesturePhase::Begin).then(|| "screenshot_copy".to_string()),
        x,
        y: 0,
    };
    let begin = machine.submit(t0, gesture(GesturePhase::Begin, 0));
    assert!(matches!(
        &sent(&begin)[0],
        HostMessage::Gesture { seq: 2, action: Some(a), .. } if a == "screenshot_copy"
    ));
    for x in 1..=100 {
        let effects = machine.submit(t0, gesture(GesturePhase::Update, x));
        assert_eq!(sent(&effects).len(), 1, "update 立即写出");
    }
    machine.submit(t0, gesture(GesturePhase::Finish, 100));
    assert_eq!(machine.inflight(), 2, "只有 begin 与 finish 等确认");
    let resent = sent(&machine.on_tick(t0 + ACK_TIMEOUT));
    assert_eq!(resent.len(), 2);
    assert!(resent.iter().all(|m| matches!(
        m,
        HostMessage::Gesture {
            phase: GesturePhase::Begin | GesturePhase::Finish,
            ..
        }
    )));
}

/// UI 发来的“取消手势”被转给运行时（让钩子别再吞鼠标输入）。
#[test]
fn cancel_gesture_is_forwarded() {
    let t0 = Instant::now();
    let (mut machine, _, generation) = running_with("capture", t0);
    let message = UiMessage::CancelGesture { id: 3 };
    assert_eq!(
        machine.on_ui_message(t0, generation, message.clone()),
        vec![Effect::FromUi(message)]
    );
}
