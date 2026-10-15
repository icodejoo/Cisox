//! 宿主核心的离屏测试：用假平台记录所有系统调用，整条链路确定性地跑。

use super::*;
use serde_json::json;
use snow_host_protocol::spec::{GateSpec, HostFlags, HostStrings, TraySection};
use snow_host_protocol::{
    CONFIG_KEY_DISABLE_ON_FULLSCREEN, PROTOCOL_VERSION, SPEC_VERSION, TOKEN_BYTES,
};
use std::collections::HashSet;

/// 假平台记录的一次系统调用。
#[derive(Debug, Clone, PartialEq)]
enum Call {
    SpawnUi(u64),
    Send(u64, HostMessage),
    CloseSession(u64),
    Kill(u32),
    Grant(u32),
    EmitSpec,
    StartTray(TrayPlan),
    SetMenu(Vec<MenuItemSpec>),
    Tooltip(String),
    Balloon(Balloon),
    StopTray,
    Theme(TrayTheme),
    Hotkeys(Vec<String>),
    Trim,
    Restart,
    /// 鼠标手势绑定（只记动作标识）。
    Mouse(Vec<String>),
    CancelGesture,
    Headless(HeadlessJob),
    /// 连续误触发确认框：`(标题, 正文)`。
    RapidPrompt(String, String),
}

/// 假平台。
#[derive(Default)]
struct Fake {
    /// 假的 Unix 秒。
    unix: u64,
    /// 无窗口任务是否拉起失败。
    headless_fails: bool,
    /// 调用记录。
    calls: Vec<Call>,
    /// `read_spec` 的返回（`None` 表示文件不存在）。
    spec: Option<HostSpec>,
    /// `read_spec` 是否返回损坏错误。
    corrupt: bool,
    /// 前台是否全屏。
    fullscreen: bool,
    /// 注册失败的热键文本。
    busy_hotkeys: HashSet<String>,
    /// 每次拉起 UI 带的启动提示。
    spawn_hints: Vec<UiHint>,
    /// 拉起是否失败。
    spawn_fails: bool,
    /// 下一个 UI 进程 ID。
    next_pid: u32,
    /// 确认框拉起是否失败。
    prompt_fails: bool,
}

impl Platform for Fake {
    fn spawn_ui(&mut self, generation: u64, hint: UiHint) -> Result<SpawnedUi, String> {
        self.calls.push(Call::SpawnUi(generation));
        self.spawn_hints.push(hint);
        if self.spawn_fails {
            return Err("找不到 exe".into());
        }
        self.next_pid += 1;
        Ok(SpawnedUi {
            pid: 1000 + self.next_pid,
            token: token(),
        })
    }
    fn send(&mut self, generation: u64, message: &HostMessage) -> Result<(), String> {
        self.calls.push(Call::Send(generation, message.clone()));
        Ok(())
    }
    fn close_session(&mut self, generation: u64) {
        self.calls.push(Call::CloseSession(generation));
    }
    fn kill_ui(&mut self, pid: u32) {
        self.calls.push(Call::Kill(pid));
    }
    fn grant_foreground(&mut self, pid: u32) {
        self.calls.push(Call::Grant(pid));
    }
    fn read_spec(&mut self) -> Result<HostSpec, SpecFileError> {
        if self.corrupt {
            return Err(SpecFileError::Spec(
                snow_host_protocol::spec::SpecError::MissingVersion,
            ));
        }
        self.spec.clone().ok_or(SpecFileError::Missing)
    }
    fn spawn_emit_spec(&mut self) -> Result<(), String> {
        self.calls.push(Call::EmitSpec);
        Ok(())
    }
    fn start_tray(&mut self, plan: &TrayPlan) -> Result<(), String> {
        self.calls.push(Call::StartTray(plan.clone()));
        Ok(())
    }
    fn set_tray_menu(&mut self, menu: &[MenuItemSpec]) -> Result<(), String> {
        self.calls.push(Call::SetMenu(menu.to_vec()));
        Ok(())
    }
    fn set_tooltip(&mut self, text: &str) {
        self.calls.push(Call::Tooltip(text.to_string()));
    }
    fn show_balloon(&mut self, balloon: Balloon) {
        self.calls.push(Call::Balloon(balloon));
    }
    fn stop_tray(&mut self) {
        self.calls.push(Call::StopTray);
    }
    fn set_popup_theme(&mut self, theme: TrayTheme) {
        self.calls.push(Call::Theme(theme));
    }
    fn register_hotkeys(&mut self, hotkeys: &[HotkeySpec]) -> Vec<Result<(), String>> {
        self.calls.push(Call::Hotkeys(
            hotkeys.iter().map(|h| h.text.clone()).collect(),
        ));
        hotkeys
            .iter()
            .map(|h| {
                if self.busy_hotkeys.contains(&h.text) {
                    Err("已被占用".to_string())
                } else {
                    Ok(())
                }
            })
            .collect()
    }
    fn fullscreen_focused(&mut self) -> bool {
        self.fullscreen
    }
    fn trim_working_set(&mut self) {
        self.calls.push(Call::Trim);
    }
    fn restart_host(&mut self) -> Result<(), String> {
        self.calls.push(Call::Restart);
        Ok(())
    }
    fn set_mouse_bindings(&mut self, bindings: &[MouseBindingSpec]) -> Result<(), String> {
        self.calls.push(Call::Mouse(
            bindings.iter().map(|b| b.action.clone()).collect(),
        ));
        Ok(())
    }
    fn cancel_gesture(&mut self) {
        self.calls.push(Call::CancelGesture);
    }
    fn spawn_headless(&mut self, job: &HeadlessJob) -> Result<(), String> {
        if self.headless_fails {
            return Err("拉不起来".into());
        }
        self.calls.push(Call::Headless(job.clone()));
        Ok(())
    }
    fn unix_now(&mut self) -> u64 {
        self.unix
    }
    fn confirm_rapid_capture(&mut self, title: &str, text: &str) -> Result<(), String> {
        if self.prompt_fails {
            return Err("线程拉不起来".into());
        }
        self.calls
            .push(Call::RapidPrompt(title.to_string(), text.to_string()));
        Ok(())
    }
}

/// 测试令牌。
fn token() -> String {
    "ab".repeat(TOKEN_BYTES)
}

/// 构造带菜单与热键的快照。
fn spec(rev: u64) -> HostSpec {
    let icon = IconRgba {
        width: 1,
        height: 1,
        rgba: vec![1, 2, 3, 4].into(),
    };
    let item = |label: &str, checked: Option<bool>, action: &str| MenuItemSpec::Item {
        label: label.to_string(),
        enabled: true,
        checked,
        icon_rgba: None,
        action_id: action.to_string(),
    };
    let key = |config: &str, id: &str, text: &str, stays: bool| HotkeySpec {
        id: id.to_string(),
        config_key: config.to_string(),
        mods: 3,
        vk: 0x53,
        text: text.to_string(),
        on_release_id: None,
        stays_when_paused: stays,
    };
    HostSpec {
        spec_version: SPEC_VERSION,
        rev,
        hotkeys: vec![
            key("shot", "capture", "Ctrl+Alt+S", false),
            key("rec", "start_recording", "Ctrl+Alt+R", false),
            key("pause", ACTION_TOGGLE_GLOBAL_HOTKEYS, "Ctrl+Alt+P", true),
        ],
        tray: TraySection {
            enabled: true,
            tooltip: "Cisox".into(),
            paused_tooltip: "Cisox (paused)".into(),
            icon_rgba: icon,
            menu: vec![
                item("Capture", None, "capture"),
                item("Pause", Some(false), ACTION_TOGGLE_GLOBAL_HOTKEYS),
                item("Gate", Some(false), ACTION_TOGGLE_FULLSCREEN_GATE),
                item("Quit", None, ACTION_QUIT),
            ],
            on_left: Some("capture".into()),
            on_middle: None,
            on_double: None,
            theme: TrayTheme::Dark,
        },
        mouse: vec![],
        gate: GateSpec {
            fullscreen_disable: false,
        },
        strings: HostStrings {
            notice_paused: "paused".into(),
            notice_resumed: "resumed".into(),
            notice_resume_failed: "resume failed: {failures}".into(),
            notice_gate_on: "gate on".into(),
            notice_gate_off: "gate off".into(),
            notice_ui_start_failed: "ui failed".into(),
            notice_ui_crash_loop: "ui loop".into(),
            notice_rapid_capture: "rapid {count}".into(),
        },
        flags: HostFlags::default(),
    }
}

/// 建好并启动核心。
fn started(spec: Option<HostSpec>) -> (HostCore<Fake>, Instant) {
    let t0 = Instant::now();
    let platform = Fake {
        spec,
        ..Fake::default()
    };
    let mut core = HostCore::new(platform);
    core.start(t0);
    (core, t0)
}

/// 取记录的全部调用并清空。
fn take_calls(core: &mut HostCore<Fake>) -> Vec<Call> {
    std::mem::take(&mut core.platform.calls)
}

/// 构造 UI 的 hello 行。
fn hello_line(pid: u32) -> String {
    snow_host_protocol::frame::encode_line(&UiMessage::Hello {
        pid,
        proto: PROTOCOL_VERSION,
        token: token(),
    })
    .unwrap()
}

/// 编码任意 UI 消息。
fn line(message: &UiMessage) -> String {
    snow_host_protocol::frame::encode_line(message).unwrap()
}

/// 让核心把一个 UI 进程拉起并握手（返回代数与进程 ID）。
fn bring_up_ui(core: &mut HostCore<Fake>, t: Instant) -> (u64, u32) {
    core.handle(
        t,
        Event::Action {
            id: "capture".into(),
            origin: ActionOrigin::Hotkey,
        },
    );
    let generation = core.machine().generation();
    let pid = 1000 + core.platform.next_pid;
    core.handle(
        t,
        Event::SessionConnected {
            generation,
            client_pid: pid,
        },
    );
    core.handle(
        t,
        Event::SessionLine {
            generation,
            line: hello_line(pid),
        },
    );
    (generation, pid)
}

/// 启动：快照存在 → 设置主题、显示托盘（菜单勾选叠加宿主状态）、注册热键；不拉起 UI。
#[test]
fn start_applies_spec_without_spawning_ui() {
    let (mut core, t0) = started(Some(spec(3)));
    let calls = take_calls(&mut core);
    assert!(calls.contains(&Call::Theme(TrayTheme::Dark)));
    let tray = calls
        .iter()
        .find_map(|c| match c {
            Call::StartTray(plan) => Some(plan.clone()),
            _ => None,
        })
        .expect("应创建托盘");
    assert_eq!(tray.tooltip, "Cisox");
    assert_eq!(tray.on_left.as_deref(), Some("capture"));
    assert_eq!(tray.menu.len(), 4);
    assert!(calls.contains(&Call::Hotkeys(vec![
        "Ctrl+Alt+S".into(),
        "Ctrl+Alt+R".into(),
        "Ctrl+Alt+P".into()
    ])));
    assert!(
        !calls.iter().any(|c| matches!(c, Call::SpawnUi(_))),
        "托盘态不拉起 UI"
    );
    assert_eq!(core.machine().kind(), crate::machine::StateKind::Absent);
    // 启动 5 秒后修剪一次工作集；此前没有待办的 tick
    assert_eq!(core.next_deadline(), Some(t0 + TRIM_AFTER_START));
}

/// 快照里有待恢复贴图：启动即拉起 UI 并投递 restore_pins。
#[test]
fn pins_to_restore_spawns_ui_at_startup() {
    let mut s = spec(1);
    s.flags.pins_to_restore = 2;
    let (mut core, t0) = started(Some(s));
    let calls = take_calls(&mut core);
    assert!(calls.contains(&Call::SpawnUi(1)));
    let generation = core.machine().generation();
    let pid = 1000 + core.platform.next_pid;
    core.handle(
        t0,
        Event::SessionLine {
            generation,
            line: hello_line(pid),
        },
    );
    assert!(
        take_calls(&mut core)
            .contains(&Call::Send(generation, HostMessage::RestorePins { seq: 1 }))
    );
}

/// 快照缺失：先用内置最小菜单起托盘，只请求一次生成快照；生成完成后重新读取并替换。
#[test]
fn missing_spec_uses_fallback_then_reloads_after_emit() {
    let (mut core, t0) = started(None);
    let calls = take_calls(&mut core);
    let plan = calls
        .iter()
        .find_map(|c| match c {
            Call::StartTray(plan) => Some(plan.clone()),
            _ => None,
        })
        .expect("兜底托盘");
    assert_eq!(plan.menu.len(), 1);
    assert_eq!(calls.iter().filter(|c| **c == Call::EmitSpec).count(), 1);
    assert!(
        !calls.iter().any(|c| matches!(c, Call::Hotkeys(_))),
        "兜底没有热键"
    );
    // 生成完成：读到真快照，整体替换（图标不同 → 重建托盘），并注册热键
    core.platform.spec = Some(spec(1));
    core.handle(t0, Event::SpecEmitted);
    let calls = take_calls(&mut core);
    assert!(
        calls
            .iter()
            .any(|c| matches!(c, Call::StartTray(p) if p.menu.len() == 4))
    );
    assert!(
        calls
            .iter()
            .any(|c| matches!(c, Call::Hotkeys(h) if h.len() == 3))
    );
    assert!(!calls.contains(&Call::EmitSpec));
}

/// 快照损坏：同样退回内置最小菜单；生成快照仍失败时不会反复拉起任务。
#[test]
fn corrupt_spec_requests_emit_only_once() {
    let t0 = Instant::now();
    let mut core = HostCore::new(Fake {
        corrupt: true,
        ..Fake::default()
    });
    core.start(t0);
    core.handle(t0, Event::SpecEmitted);
    core.handle(t0, Event::SpecEmitted);
    let calls = take_calls(&mut core);
    assert_eq!(calls.iter().filter(|c| **c == Call::EmitSpec).count(), 1);
    assert_eq!(
        calls
            .iter()
            .filter(|c| matches!(c, Call::StartTray(_)))
            .count(),
        1,
        "托盘只建一次"
    );
}

/// 快照更新：修订号相同跳过；只有菜单 / 提示变了原地替换；图标变了整体重建；托盘被关闭则移除。
#[test]
fn spec_updates_apply_minimal_changes() {
    let (mut core, t0) = started(Some(spec(1)));
    take_calls(&mut core);
    // spec_changed 经已握手的 UI 会话发来
    let (generation, _) = bring_up_ui(&mut core, t0);
    take_calls(&mut core);
    core.handle(
        t0,
        Event::SessionLine {
            generation,
            line: line(&UiMessage::SpecChanged { rev: 1 }),
        },
    );
    assert!(take_calls(&mut core).is_empty(), "修订号没变不得重复应用");

    // 只改菜单文案与悬停提示
    let mut s2 = spec(2);
    s2.tray.tooltip = "Cisox 2".into();
    if let MenuItemSpec::Item { label, .. } = &mut s2.tray.menu[0] {
        *label = "截图".into();
    }
    core.platform.spec = Some(s2);
    core.handle(
        t0,
        Event::SessionLine {
            generation,
            line: line(&UiMessage::SpecChanged { rev: 2 }),
        },
    );
    let calls = take_calls(&mut core);
    assert!(calls.iter().any(|c| matches!(c, Call::SetMenu(_))));
    assert!(calls.contains(&Call::Tooltip("Cisox 2".into())));
    assert!(
        !calls.iter().any(|c| matches!(c, Call::StartTray(_))),
        "菜单变化不重建托盘"
    );
    assert!(
        !calls.iter().any(|c| matches!(c, Call::Hotkeys(_))),
        "热键没变不重注册"
    );

    // 图标变了：整体重建
    let mut s3 = spec(3);
    s3.tray.icon_rgba.rgba = vec![9, 9, 9, 9].into();
    core.platform.spec = Some(s3);
    core.handle(
        t0,
        Event::SessionLine {
            generation,
            line: line(&UiMessage::SpecChanged { rev: 3 }),
        },
    );
    assert!(
        take_calls(&mut core)
            .iter()
            .any(|c| matches!(c, Call::StartTray(_)))
    );

    // 托盘被设置关闭：移除图标
    let mut s4 = spec(4);
    s4.tray.enabled = false;
    core.platform.spec = Some(s4);
    core.handle(
        t0,
        Event::SessionLine {
            generation,
            line: line(&UiMessage::SpecChanged { rev: 4 }),
        },
    );
    assert!(take_calls(&mut core).contains(&Call::StopTray));
}

/// 热键改了：快照里的热键表变化时才重新注册。
#[test]
fn hotkeys_reregister_only_when_changed() {
    let (mut core, t0) = started(Some(spec(1)));
    let (generation, _) = bring_up_ui(&mut core, t0);
    take_calls(&mut core);
    let mut s2 = spec(2);
    s2.hotkeys[0].text = "Ctrl+Alt+X".into();
    core.platform.spec = Some(s2);
    core.handle(
        t0,
        Event::SessionLine {
            generation,
            line: line(&UiMessage::SpecChanged { rev: 2 }),
        },
    );
    let calls = take_calls(&mut core);
    assert!(calls.contains(&Call::Hotkeys(vec![
        "Ctrl+Alt+X".into(),
        "Ctrl+Alt+R".into(),
        "Ctrl+Alt+P".into()
    ])));
}

/// 启动提示：队首命令是语音转文字（切换 / 按住说话按下）才带 `--hint dictation`，其它命令不带。
#[test]
fn spawn_hint_follows_queue_head() {
    for (id, expected) in [
        ("toggle_dictation", UiHint::Dictation),
        ("start_dictation", UiHint::Dictation),
        ("stop_dictation", UiHint::None),
        ("capture", UiHint::None),
    ] {
        let (mut core, t0) = started(Some(spec(1)));
        take_calls(&mut core);
        core.handle(
            t0,
            Event::Action {
                id: id.into(),
                origin: ActionOrigin::Hotkey,
            },
        );
        assert_eq!(core.platform.spawn_hints, vec![expected], "{id}");
    }
}

/// 热键触发：UI 不在 → 拉起、握手后投递；动作带来源；先授权前台再写管道。
#[test]
fn hotkey_action_spawns_ui_and_delivers_after_hello() {
    let (mut core, t0) = started(Some(spec(1)));
    take_calls(&mut core);
    core.handle(
        t0,
        Event::Action {
            id: "capture".into(),
            origin: ActionOrigin::Hotkey,
        },
    );
    let calls = take_calls(&mut core);
    assert_eq!(calls, vec![Call::SpawnUi(1), Call::Grant(1001)]);
    core.handle(
        t0,
        Event::SessionConnected {
            generation: 1,
            client_pid: 1001,
        },
    );
    core.handle(
        t0,
        Event::SessionLine {
            generation: 1,
            line: hello_line(1001),
        },
    );
    let calls = take_calls(&mut core);
    assert_eq!(
        calls,
        vec![
            Call::Grant(1001),
            Call::Send(
                1,
                HostMessage::Command {
                    seq: 1,
                    id: "capture".into(),
                    origin: Some(ActionOrigin::Hotkey)
                }
            )
        ]
    );
}

/// 拉起失败：托盘提示（用快照里的本地化文案），不留脏状态。
#[test]
fn spawn_failure_shows_localized_notice() {
    let (mut core, t0) = started(Some(spec(1)));
    core.platform.spawn_fails = true;
    take_calls(&mut core);
    core.handle(
        t0,
        Event::Action {
            id: "capture".into(),
            origin: ActionOrigin::Tray,
        },
    );
    let calls = take_calls(&mut core);
    assert!(calls.contains(&Call::Tooltip("Cisox: ui failed".into())));
    assert_eq!(core.machine().kind(), crate::machine::StateKind::Absent);
}

/// 没有本地化文案（兜底快照）时用内置英文兜底。
#[test]
fn notice_falls_back_to_builtin_text() {
    let t0 = Instant::now();
    let mut core = HostCore::new(Fake {
        spawn_fails: true,
        ..Fake::default()
    });
    core.start(t0);
    take_calls(&mut core);
    core.handle(
        t0,
        Event::Action {
            id: "settings".into(),
            origin: ActionOrigin::Tray,
        },
    );
    assert!(take_calls(&mut core).contains(&Call::Tooltip(
        "Cisox: The UI process failed to start".into()
    )));
}

/// 前台全屏闸门：打开且前台全屏时丢弃热键动作（连 UI 都不拉起）；托盘来源不受影响。
#[test]
fn fullscreen_gate_drops_only_gated_hotkeys() {
    let mut s = spec(1);
    s.gate.fullscreen_disable = true;
    let (mut core, t0) = started(Some(s));
    take_calls(&mut core);
    core.platform.fullscreen = true;
    // 热键 + 全屏：丢弃（连 UI 都不拉起）
    core.handle(
        t0,
        Event::Action {
            id: "capture".into(),
            origin: ActionOrigin::Hotkey,
        },
    );
    assert!(take_calls(&mut core).is_empty());
    // 托盘来源：放行
    core.handle(
        t0,
        Event::Action {
            id: "capture".into(),
            origin: ActionOrigin::Tray,
        },
    );
    assert!(take_calls(&mut core).contains(&Call::SpawnUi(1)));
}

/// 闸门打开且全屏时，暂停 / 闸门开关热键仍然生效，否则关不回去。
#[test]
fn gate_toggles_are_exempt_from_the_gate() {
    let mut s = spec(1);
    s.gate.fullscreen_disable = true;
    let (mut core, t0) = started(Some(s));
    take_calls(&mut core);
    core.platform.fullscreen = true;
    core.handle(
        t0,
        Event::Action {
            id: ACTION_TOGGLE_GLOBAL_HOTKEYS.into(),
            origin: ActionOrigin::Hotkey,
        },
    );
    assert!(
        take_calls(&mut core)
            .iter()
            .any(|c| matches!(c, Call::Hotkeys(_)))
    );
    core.handle(
        t0,
        Event::Action {
            id: ACTION_TOGGLE_FULLSCREEN_GATE.into(),
            origin: ActionOrigin::Hotkey,
        },
    );
    let calls = take_calls(&mut core);
    assert!(calls.iter().any(|c| matches!(c, Call::SetMenu(_))));
}

/// 闸门关闭时不探测前台（热键路径不为此多一次系统调用）：用探测会翻车的假平台验证——
/// 关闭状态下即使前台全屏也放行。
#[test]
fn gate_off_lets_hotkeys_through_even_when_fullscreen() {
    let (mut core, t0) = started(Some(spec(1)));
    take_calls(&mut core);
    core.platform.fullscreen = true;
    core.handle(
        t0,
        Event::Action {
            id: "capture".into(),
            origin: ActionOrigin::Hotkey,
        },
    );
    assert!(take_calls(&mut core).contains(&Call::SpawnUi(1)));
}

/// 暂停热键：只剩开关本身注册、菜单勾选、悬停提示固定为暂停；恢复后全部重注册并提示。
#[test]
fn pause_and_resume_hotkeys() {
    let (mut core, t0) = started(Some(spec(1)));
    take_calls(&mut core);
    core.handle(
        t0,
        Event::Action {
            id: ACTION_TOGGLE_GLOBAL_HOTKEYS.into(),
            origin: ActionOrigin::Tray,
        },
    );
    let calls = take_calls(&mut core);
    assert!(calls.contains(&Call::Hotkeys(vec!["Ctrl+Alt+P".into()])));
    let menu = calls
        .iter()
        .find_map(|c| match c {
            Call::SetMenu(m) => Some(m.clone()),
            _ => None,
        })
        .expect("菜单勾选更新");
    assert!(matches!(
        &menu[1],
        MenuItemSpec::Item {
            checked: Some(true),
            ..
        }
    ));
    assert!(calls.contains(&Call::Tooltip("Cisox (paused)".into())));
    // 暂停期间别的热键动作不会来（已注销）；托盘动作照常
    core.handle(
        t0,
        Event::Action {
            id: ACTION_TOGGLE_GLOBAL_HOTKEYS.into(),
            origin: ActionOrigin::Hotkey,
        },
    );
    let calls = take_calls(&mut core);
    assert!(calls.contains(&Call::Hotkeys(vec![
        "Ctrl+Alt+S".into(),
        "Ctrl+Alt+R".into(),
        "Ctrl+Alt+P".into()
    ])));
    assert!(calls.contains(&Call::Tooltip("Cisox: resumed".into())));
}

/// 恢复热键时有注册失败：提示带上失败原因（占位符被替换）。
#[test]
fn resume_with_failure_reports_reason() {
    let (mut core, t0) = started(Some(spec(1)));
    core.handle(
        t0,
        Event::Action {
            id: ACTION_TOGGLE_GLOBAL_HOTKEYS.into(),
            origin: ActionOrigin::Tray,
        },
    );
    take_calls(&mut core);
    core.platform.busy_hotkeys.insert("Ctrl+Alt+R".into());
    core.handle(
        t0,
        Event::Action {
            id: ACTION_TOGGLE_GLOBAL_HOTKEYS.into(),
            origin: ActionOrigin::Tray,
        },
    );
    let calls = take_calls(&mut core);
    assert!(calls.contains(&Call::Tooltip(
        "Cisox: resume failed: Ctrl+Alt+R: 已被占用".into()
    )));
}

/// 切换前台全屏闸门：菜单勾选更新、提示；UI 在就经会话 set_config 让它落盘。
#[test]
fn toggling_gate_persists_through_live_ui() {
    let (mut core, t0) = started(Some(spec(1)));
    let (generation, _) = bring_up_ui(&mut core, t0);
    take_calls(&mut core);
    core.handle(
        t0,
        Event::Action {
            id: ACTION_TOGGLE_FULLSCREEN_GATE.into(),
            origin: ActionOrigin::Tray,
        },
    );
    let calls = take_calls(&mut core);
    let menu = calls
        .iter()
        .find_map(|c| match c {
            Call::SetMenu(m) => Some(m.clone()),
            _ => None,
        })
        .expect("勾选更新");
    assert!(matches!(
        &menu[2],
        MenuItemSpec::Item {
            checked: Some(true),
            ..
        }
    ));
    assert!(calls.contains(&Call::Tooltip("Cisox: gate on".into())));
    assert!(
        calls.iter().all(|c| !matches!(c, Call::Headless(_))),
        "UI 在就不起无窗口任务"
    );
    assert!(calls.contains(&Call::Send(
        generation,
        HostMessage::SetConfig {
            seq: 2,
            key: CONFIG_KEY_DISABLE_ON_FULLSCREEN.into(),
            value: json!(true)
        }
    )));
}

/// 切换前台全屏闸门而 UI 不在：不拉起 UI 会话，改走 `--headless set-config`（UI 仍是配置唯一写者）。
#[test]
fn toggling_gate_without_ui_uses_headless_set_config() {
    let (mut core, t0) = started(Some(spec(1)));
    take_calls(&mut core);
    core.handle(
        t0,
        Event::Action {
            id: ACTION_TOGGLE_FULLSCREEN_GATE.into(),
            origin: ActionOrigin::Tray,
        },
    );
    let calls = take_calls(&mut core);
    assert!(calls.contains(&Call::Tooltip("Cisox: gate on".into())));
    assert!(calls.contains(&Call::Headless(HeadlessJob::SetConfig {
        key: CONFIG_KEY_DISABLE_ON_FULLSCREEN.into(),
        value: json!(true),
    })));
    assert!(
        !calls.iter().any(|c| matches!(c, Call::SpawnUi(_))),
        "只写个配置不该拉起完整 UI"
    );
    assert_eq!(core.machine().kind(), crate::machine::StateKind::Absent);
}

/// 写配置的无窗口任务在跑时，新来的消息暂存保序（不与新 UI 同时写配置）；任务结束后按序投递，
/// 其中又有改配置且 UI 仍不在的，再起一个任务并继续暂存后面的。
#[test]
fn messages_are_held_while_headless_config_job_runs() {
    let (mut core, t0) = started(Some(spec(1)));
    take_calls(&mut core);
    let toggle = |core: &mut HostCore<Fake>| {
        core.handle(
            t0,
            Event::Action {
                id: ACTION_TOGGLE_FULLSCREEN_GATE.into(),
                origin: ActionOrigin::Tray,
            },
        );
    };
    toggle(&mut core); // 开 → 起任务 #1
    toggle(&mut core); // 关 → 任务在跑，暂存
    core.handle(
        t0,
        Event::Action {
            id: "capture".into(),
            origin: ActionOrigin::Hotkey,
        },
    );
    let calls = take_calls(&mut core);
    assert_eq!(
        calls
            .iter()
            .filter(|c| matches!(c, Call::Headless(_)))
            .count(),
        1,
        "同一时刻只有一个写配置任务"
    );
    assert!(
        !calls.iter().any(|c| matches!(c, Call::SpawnUi(_))),
        "任务在跑，UI 不能起"
    );
    // 任务 #1 结束：暂存的“关”再起任务 #2，截图动作继续暂存
    core.handle(
        t0,
        Event::HeadlessDone {
            kind: HeadlessKind::SetConfig,
            notice: None,
        },
    );
    let calls = take_calls(&mut core);
    assert!(calls.contains(&Call::Headless(HeadlessJob::SetConfig {
        key: CONFIG_KEY_DISABLE_ON_FULLSCREEN.into(),
        value: json!(false),
    })));
    assert!(!calls.iter().any(|c| matches!(c, Call::SpawnUi(_))));
    // 任务 #2 结束：截图动作终于拉起 UI
    core.handle(
        t0,
        Event::HeadlessDone {
            kind: HeadlessKind::SetConfig,
            notice: None,
        },
    );
    let calls = take_calls(&mut core);
    assert!(calls.contains(&Call::SpawnUi(1)));
}

/// 无窗口任务拉不起来：退回会话路径（拉起 UI 来写配置）。
#[test]
fn headless_spawn_failure_falls_back_to_ui_session() {
    let (mut core, t0) = started(Some(spec(1)));
    core.platform.headless_fails = true;
    take_calls(&mut core);
    core.handle(
        t0,
        Event::Action {
            id: ACTION_TOGGLE_FULLSCREEN_GATE.into(),
            origin: ActionOrigin::Tray,
        },
    );
    assert!(take_calls(&mut core).contains(&Call::SpawnUi(1)));
    core.handle(
        t0,
        Event::SessionLine {
            generation: 1,
            line: hello_line(1001),
        },
    );
    assert!(take_calls(&mut core).contains(&Call::Send(
        1,
        HostMessage::SetConfig {
            seq: 1,
            key: CONFIG_KEY_DISABLE_ON_FULLSCREEN.into(),
            value: json!(true)
        }
    )));
}

/// 带鼠标手势绑定的快照。
fn spec_with_mouse(rev: u64, actions: &[&str]) -> HostSpec {
    let mut s = spec(rev);
    s.mouse = actions
        .iter()
        .map(|action| MouseBindingSpec {
            action: action.to_string(),
            modifiers: vec!["windows".into()],
            button: "left_drag".into(),
        })
        .collect();
    s
}

/// 鼠标钩子只在快照有绑定时才装；绑定没变不重装；绑定清空就卸载（平时系统里没有钩子）。
#[test]
fn mouse_hook_follows_spec_bindings() {
    let (mut core, t0) = started(Some(spec(1)));
    assert!(
        !take_calls(&mut core)
            .iter()
            .any(|c| matches!(c, Call::Mouse(_))),
        "没有绑定不装钩子"
    );
    let (generation, _) = bring_up_ui(&mut core, t0);
    take_calls(&mut core);
    let reload = |core: &mut HostCore<Fake>, s: HostSpec| {
        let rev = s.rev;
        core.platform.spec = Some(s);
        core.handle(
            t0,
            Event::SessionLine {
                generation,
                line: line(&UiMessage::SpecChanged { rev }),
            },
        );
        take_calls(core)
    };
    let calls = reload(
        &mut core,
        spec_with_mouse(2, &["screenshot_copy", "screenshot_ocr"]),
    );
    assert!(calls.contains(&Call::Mouse(vec![
        "screenshot_copy".into(),
        "screenshot_ocr".into()
    ])));
    let calls = reload(
        &mut core,
        spec_with_mouse(3, &["screenshot_copy", "screenshot_ocr"]),
    );
    assert!(
        !calls.iter().any(|c| matches!(c, Call::Mouse(_))),
        "绑定没变不重装"
    );
    let calls = reload(&mut core, spec_with_mouse(4, &[]));
    assert!(calls.contains(&Call::Mouse(vec![])), "清空即卸载");
}

/// 手势全流程（UI 在）：begin 带动作并授权前台，update / finish 按序写出；UI 放弃时让钩子取消。
#[test]
fn gesture_flow_with_live_ui() {
    let (mut core, t0) = started(Some(spec_with_mouse(1, &["screenshot_copy"])));
    let (generation, _) = bring_up_ui(&mut core, t0);
    take_calls(&mut core);
    let gesture = |phase, action: Option<&str>, x| {
        Event::Gesture(GestureEvent {
            phase,
            id: 4,
            action: action.map(str::to_string),
            x,
            y: 7,
        })
    };
    core.handle(
        t0,
        gesture(GesturePhase::Begin, Some("screenshot_copy"), 10),
    );
    core.handle(t0, gesture(GesturePhase::Update, None, 20));
    core.handle(t0, gesture(GesturePhase::Finish, None, 30));
    let calls = take_calls(&mut core);
    assert_eq!(calls[0], Call::Grant(1001), "begin 先授权前台");
    let sent: Vec<_> = calls
        .iter()
        .filter_map(|c| match c {
            Call::Send(
                _,
                HostMessage::Gesture {
                    phase, action, x, ..
                },
            ) => Some((*phase, action.clone(), *x)),
            _ => None,
        })
        .collect();
    assert_eq!(
        sent,
        [
            (GesturePhase::Begin, Some("screenshot_copy".to_string()), 10),
            (GesturePhase::Update, None, 20),
            (GesturePhase::Finish, None, 30),
        ]
    );
    assert_eq!(
        calls.iter().filter(|c| matches!(c, Call::Grant(_))).count(),
        1,
        "update / finish 不抢焦点"
    );
    // UI 放弃了这次手势
    core.handle(
        t0,
        Event::SessionLine {
            generation,
            line: line(&UiMessage::CancelGesture { id: 4 }),
        },
    );
    assert!(take_calls(&mut core).contains(&Call::CancelGesture));
}

/// 手势触发时 UI 不在：begin 拉起 UI，期间的 update 只留最新，握手后按序冲出。
#[test]
fn gesture_while_ui_absent_spawns_ui_and_coalesces_updates() {
    let (mut core, t0) = started(Some(spec_with_mouse(1, &["screenshot_copy"])));
    take_calls(&mut core);
    let gesture = |phase, action: Option<&str>, x| {
        Event::Gesture(GestureEvent {
            phase,
            id: 1,
            action: action.map(str::to_string),
            x,
            y: 0,
        })
    };
    core.handle(t0, gesture(GesturePhase::Begin, Some("screenshot_copy"), 0));
    for x in 1..=30 {
        core.handle(t0, gesture(GesturePhase::Update, None, x));
    }
    core.handle(t0, gesture(GesturePhase::Finish, None, 30));
    assert!(take_calls(&mut core).contains(&Call::SpawnUi(1)));
    core.handle(
        t0,
        Event::SessionConnected {
            generation: 1,
            client_pid: 1001,
        },
    );
    core.handle(
        t0,
        Event::SessionLine {
            generation: 1,
            line: hello_line(1001),
        },
    );
    let phases: Vec<_> = take_calls(&mut core)
        .into_iter()
        .filter_map(|c| match c {
            Call::Send(_, HostMessage::Gesture { phase, x, .. }) => Some((phase, x)),
            _ => None,
        })
        .collect();
    assert_eq!(
        phases,
        [
            (GesturePhase::Begin, 0),
            (GesturePhase::Update, 30),
            (GesturePhase::Finish, 30)
        ]
    );
}

/// 熔断退避期间手势 begin 被丢弃：让钩子取消，别一直吞着用户的鼠标输入。
#[test]
fn dropped_gesture_begin_cancels_the_hook_gesture() {
    let (mut core, t0) = started(Some(spec_with_mouse(1, &["screenshot_copy"])));
    // 连续三次启动失败 → 熔断
    core.platform.spawn_fails = true;
    for _ in 0..3 {
        core.handle(
            t0,
            Event::Action {
                id: "capture".into(),
                origin: ActionOrigin::Hotkey,
            },
        );
    }
    assert_eq!(core.machine().kind(), crate::machine::StateKind::Backoff);
    take_calls(&mut core);
    core.handle(
        t0,
        Event::Gesture(GestureEvent {
            phase: GesturePhase::Begin,
            id: 9,
            action: Some("screenshot_copy".into()),
            x: 0,
            y: 0,
        }),
    );
    assert!(take_calls(&mut core).contains(&Call::CancelGesture));
}

/// 带保活原因的快照。
fn spec_with_flags(rev: u64, keep_alive: &[&str], pins: u32, due: Option<u64>) -> HostSpec {
    let mut s = spec(rev);
    s.flags = HostFlags {
        keep_alive: keep_alive.iter().map(|r| r.to_string()).collect(),
        pins_to_restore: pins,
        update_due_at: due,
    };
    s
}

/// 保活（§10 第 2、3 条）：MCP / OCR 预热开着，宿主登录就拉起 UI 并投递保活占位；两者都没开则托盘态不拉起。
#[test]
fn resident_features_spawn_ui_at_startup() {
    for reason in [
        snow_host_protocol::KEEP_ALIVE_MCP,
        snow_host_protocol::KEEP_ALIVE_OCR_HOT_START,
    ] {
        let (mut core, t0) = started(Some(spec_with_flags(1, &[reason], 0, None)));
        assert!(
            take_calls(&mut core).contains(&Call::SpawnUi(1)),
            "{reason}"
        );
        assert!(core.machine().keep_alive());
        core.handle(
            t0,
            Event::SessionLine {
                generation: 1,
                line: hello_line(1001),
            },
        );
        assert!(take_calls(&mut core).iter().any(|c| matches!(
            c,
            Call::Send(1, HostMessage::Command { id, .. }) if id == snow_host_protocol::ACTION_KEEP_ALIVE
        )));
    }
    let (mut core, _) = started(Some(spec_with_flags(1, &[], 0, None)));
    assert!(
        !take_calls(&mut core)
            .iter()
            .any(|c| matches!(c, Call::SpawnUi(_)))
    );
    assert!(!core.machine().keep_alive());
}

/// 语音热键不保活（§10 第 4 条，2026-10-13 改判）：旧快照里残留的 `dictation_hotkey` 原因被忽略，
/// 登录不预拉 UI、状态机不记保活；热键拉起 UI 后窗口全关（exit_request）照常同意退出，退出后不补拉。
#[test]
fn legacy_dictation_hotkey_reason_is_ignored() {
    let (mut core, t0) = started(Some(spec_with_flags(1, &["dictation_hotkey"], 0, None)));
    assert!(
        !take_calls(&mut core)
            .iter()
            .any(|c| matches!(c, Call::SpawnUi(_)))
    );
    assert!(!core.machine().keep_alive(), "语音热键不保活");
    // 热键冷拉起 UI，处理完命令后窗口全关，UI 请求退出
    let (generation, _) = bring_up_ui(&mut core, t0);
    core.handle(
        t0,
        Event::SessionLine {
            generation,
            line: line(&UiMessage::Ack { seq: 1 }),
        },
    );
    take_calls(&mut core);
    core.handle(
        t0,
        Event::SessionLine {
            generation,
            line: line(&UiMessage::ExitRequest { last_seq: 1 }),
        },
    );
    assert!(
        take_calls(&mut core)
            .iter()
            .any(|c| matches!(c, Call::Send(g, HostMessage::ExitOk) if *g == generation)),
        "绑了语音热键也同意退出"
    );
    core.handle(t0 + Duration::from_secs(1), Event::UiExited { generation });
    core.tick(t0 + Duration::from_secs(5));
    assert!(
        !take_calls(&mut core)
            .iter()
            .any(|c| matches!(c, Call::SpawnUi(_))),
        "退出后不补拉"
    );
}

/// 快照生成完成后才读到“要常驻”：当场拉起 UI（启动时只有内置兜底快照）；设置里关掉后不再要求。
#[test]
fn keep_alive_applies_when_real_spec_arrives_later() {
    let (mut core, t0) = started(None);
    take_calls(&mut core);
    core.platform.spec = Some(spec_with_flags(
        1,
        &[snow_host_protocol::KEEP_ALIVE_MCP],
        0,
        None,
    ));
    core.handle(t0, Event::SpecEmitted);
    assert!(take_calls(&mut core).contains(&Call::SpawnUi(1)));
    // UI 里关掉 MCP：快照不再要求常驻
    let (generation, _) = (1, ());
    core.handle(
        t0,
        Event::SessionLine {
            generation,
            line: hello_line(1001),
        },
    );
    core.platform.spec = Some(spec_with_flags(2, &[], 0, None));
    core.handle(
        t0,
        Event::SessionLine {
            generation,
            line: line(&UiMessage::SpecChanged { rev: 2 }),
        },
    );
    assert!(!core.machine().keep_alive());
}

/// 登录恢复贴图 + 要求常驻：只拉起一个 UI，恢复贴图与保活占位都投递。
#[test]
fn pins_and_resident_spawn_one_ui() {
    let (mut core, t0) = started(Some(spec_with_flags(
        1,
        &[snow_host_protocol::KEEP_ALIVE_MCP],
        3,
        None,
    )));
    let calls = take_calls(&mut core);
    assert_eq!(
        calls
            .iter()
            .filter(|c| matches!(c, Call::SpawnUi(_)))
            .count(),
        1
    );
    core.handle(
        t0,
        Event::SessionLine {
            generation: 1,
            line: hello_line(1001),
        },
    );
    let sent = take_calls(&mut core)
        .into_iter()
        .filter(|c| matches!(c, Call::Send(..)))
        .count();
    assert_eq!(sent, 2);
}

/// 到点的更新检查：登录后至少延后 30 秒才拉起 `--headless update-check`；同一个到点时间只拉起一次。
#[test]
fn update_check_runs_once_after_start_delay() {
    let mut core = HostCore::new(Fake {
        unix: 1_000_000,
        spec: Some(spec_with_flags(1, &[], 0, Some(999_000))),
        ..Fake::default()
    });
    let t0 = Instant::now();
    core.start(t0);
    take_calls(&mut core);
    let at = t0 + UPDATE_CHECK_START_DELAY;
    assert_eq!(
        core.next_deadline(),
        Some(t0 + TRIM_AFTER_START),
        "先是修剪"
    );
    core.tick(t0 + TRIM_AFTER_START);
    assert_eq!(core.next_deadline(), Some(at));
    core.tick(at - Duration::from_millis(1));
    assert!(
        take_calls(&mut core)
            .iter()
            .all(|c| !matches!(c, Call::Headless(_)))
    );
    core.tick(at);
    assert_eq!(
        take_calls(&mut core)
            .iter()
            .filter(|c| **c == Call::Headless(HeadlessJob::UpdateCheck))
            .count(),
        1
    );
    assert_eq!(core.next_deadline(), None, "拉起后没有待办，不轮询");
    // 检查结束、快照里到点时间没推进（比如检查被策略挡掉）：同一个到点时间不重复检查
    core.handle(
        at,
        Event::HeadlessDone {
            kind: HeadlessKind::UpdateCheck,
            notice: None,
        },
    );
    core.tick(at + Duration::from_secs(3600));
    assert!(
        take_calls(&mut core)
            .iter()
            .all(|c| !matches!(c, Call::Headless(_)))
    );
}

/// 更新检查结束后快照里的到点时间推到未来：宿主按差值安排下一次（不轮询，只设一个截止时刻）；
/// 发现新版本时提示进托盘悬停。
#[test]
fn update_check_reschedules_from_new_spec_and_shows_notice() {
    let mut core = HostCore::new(Fake {
        unix: 1_000_000,
        spec: Some(spec_with_flags(1, &[], 0, Some(0))),
        ..Fake::default()
    });
    let t0 = Instant::now();
    core.start(t0);
    take_calls(&mut core);
    let at = t0 + UPDATE_CHECK_START_DELAY;
    core.tick(at);
    assert!(take_calls(&mut core).contains(&Call::Headless(HeadlessJob::UpdateCheck)));
    // UI 侧检查完把快照改成 24 小时后到点
    core.platform.spec = Some(spec_with_flags(2, &[], 0, Some(1_000_000 + 86_400)));
    core.handle(
        at,
        Event::HeadlessDone {
            kind: HeadlessKind::UpdateCheck,
            notice: Some("Update 9.9.9 available".into()),
        },
    );
    let calls = take_calls(&mut core);
    assert!(calls.contains(&Call::Tooltip("Cisox: Update 9.9.9 available".into())));
    // 修剪在拉起检查那次 tick 里已经做完，剩下的待办只有 24 小时后的下一次检查
    assert_eq!(core.next_deadline(), Some(at + Duration::from_secs(86_400)));
}

/// 快照里没有到点时间（手动更新 / 没配置地址）：不安排更新检查。
#[test]
fn no_update_due_means_no_check() {
    let (mut core, t0) = started(Some(spec_with_flags(1, &[], 0, None)));
    core.tick(t0 + Duration::from_secs(10_000));
    assert!(
        take_calls(&mut core)
            .iter()
            .all(|c| !matches!(c, Call::Headless(_)))
    );
}

/// 宿主正在退出时不再拉起更新检查。
#[test]
fn update_check_is_skipped_while_quitting() {
    let mut core = HostCore::new(Fake {
        unix: 10,
        spec: Some(spec_with_flags(1, &[], 0, Some(0))),
        ..Fake::default()
    });
    let t0 = Instant::now();
    core.start(t0);
    take_calls(&mut core);
    core.handle(
        t0,
        Event::Action {
            id: ACTION_QUIT.into(),
            origin: ActionOrigin::Tray,
        },
    );
    core.tick(t0 + UPDATE_CHECK_START_DELAY);
    assert!(
        take_calls(&mut core)
            .iter()
            .all(|c| !matches!(c, Call::Headless(_)))
    );
}

/// 设置页改热键：重新注册并把每个配置键的结果回给 UI；冲突的键报失败与原因。
#[test]
fn hotkeys_apply_reports_per_config_key() {
    let (mut core, t0) = started(Some(spec(1)));
    let (generation, _) = bring_up_ui(&mut core, t0);
    take_calls(&mut core);
    let mut s2 = spec(2);
    s2.hotkeys[1].text = "Ctrl+Alt+Q".into();
    core.platform.spec = Some(s2);
    core.platform.busy_hotkeys.insert("Ctrl+Alt+Q".into());
    core.handle(
        t0,
        Event::SessionLine {
            generation,
            line: line(&UiMessage::HotkeysApply { rev: 2 }),
        },
    );
    let calls = take_calls(&mut core);
    let reply = calls
        .iter()
        .find_map(|c| match c {
            Call::Send(_, HostMessage::HotkeysResult { rev, results }) => {
                Some((*rev, results.clone()))
            }
            _ => None,
        })
        .expect("应回报结果");
    assert_eq!(reply.0, 2);
    assert_eq!(reply.1.len(), 3);
    let rec = reply.1.iter().find(|r| r.config_key == "rec").unwrap();
    assert!(!rec.ok);
    assert_eq!(rec.error.as_deref(), Some("Ctrl+Alt+Q: 已被占用"));
    assert!(
        reply
            .1
            .iter()
            .filter(|r| r.config_key != "rec")
            .all(|r| r.ok)
    );
    // 只注册了一次（读快照时不重复注册）
    assert_eq!(
        calls
            .iter()
            .filter(|c| matches!(c, Call::Hotkeys(_)))
            .count(),
        1
    );
}

/// 设置页回滚：失败后 UI 再发一次 apply（快照已回到旧热键），旧热键重新注册成功。
#[test]
fn rollback_apply_restores_previous_hotkeys() {
    let (mut core, t0) = started(Some(spec(1)));
    let (generation, _) = bring_up_ui(&mut core, t0);
    let mut s2 = spec(2);
    s2.hotkeys[1].text = "Ctrl+Alt+Q".into();
    core.platform.spec = Some(s2);
    core.platform.busy_hotkeys.insert("Ctrl+Alt+Q".into());
    core.handle(
        t0,
        Event::SessionLine {
            generation,
            line: line(&UiMessage::HotkeysApply { rev: 2 }),
        },
    );
    take_calls(&mut core);
    core.platform.spec = Some(spec(3));
    core.handle(
        t0,
        Event::SessionLine {
            generation,
            line: line(&UiMessage::HotkeysApply { rev: 3 }),
        },
    );
    let calls = take_calls(&mut core);
    assert!(calls.contains(&Call::Hotkeys(vec![
        "Ctrl+Alt+S".into(),
        "Ctrl+Alt+R".into(),
        "Ctrl+Alt+P".into()
    ])));
    assert!(calls.iter().any(|c| matches!(
        c,
        Call::Send(_, HostMessage::HotkeysResult { rev: 3, results }) if results.iter().all(|r| r.ok)
    )));
}

/// UI 发来的托盘提示直接显示。
#[test]
fn ui_tooltip_is_applied() {
    let (mut core, t0) = started(Some(spec(1)));
    let (generation, _) = bring_up_ui(&mut core, t0);
    take_calls(&mut core);
    core.handle(
        t0,
        Event::SessionLine {
            generation,
            line: line(&UiMessage::Tooltip {
                text: "Cisox: 已复制".into(),
            }),
        },
    );
    assert_eq!(
        take_calls(&mut core),
        vec![Call::Tooltip("Cisox: 已复制".into())]
    );
}

/// UI 发来的气泡原样交给托盘弹出。
#[test]
fn ui_balloon_is_shown() {
    let (mut core, t0) = started(Some(spec(1)));
    let (generation, _) = bring_up_ui(&mut core, t0);
    take_calls(&mut core);
    core.handle(
        t0,
        Event::SessionLine {
            generation,
            line: line(&UiMessage::Balloon {
                title: "Cisox".into(),
                text: "截图已复制".into(),
                icon: Some(snow_host_protocol::message::BalloonIcon {
                    width: 1,
                    height: 1,
                    rgba: vec![9, 8, 7, 6],
                }),
                open: Some("C:/tmp/captures".into()),
            }),
        },
    );
    assert_eq!(
        take_calls(&mut core),
        vec![Call::Balloon(Balloon {
            title: "Cisox".into(),
            text: "截图已复制".into(),
            icon: Some(snow_shell_core::tray::BalloonIcon {
                width: 1,
                height: 1,
                rgba: vec![9, 8, 7, 6],
            }),
            open: Some(std::path::PathBuf::from("C:/tmp/captures")),
        })]
    );
}

/// 无法解析的行被忽略，不影响会话。
#[test]
fn malformed_line_is_ignored() {
    let (mut core, t0) = started(Some(spec(1)));
    let (generation, _) = bring_up_ui(&mut core, t0);
    take_calls(&mut core);
    core.handle(
        t0,
        Event::SessionLine {
            generation,
            line: "{ not json".into(),
        },
    );
    core.handle(
        t0,
        Event::SessionLine {
            generation,
            line: "{\"type\":\"teleport\"}".into(),
        },
    );
    assert!(take_calls(&mut core).is_empty());
    assert_eq!(core.machine().kind(), crate::machine::StateKind::Running);
}

/// 退出：托盘“退出”→ UI 在运行就先 shutdown，UI 退出后核心标记宿主退出；
/// UI 不在则直接退出。重启标志带出。
#[test]
fn quit_flow_waits_for_ui() {
    let (mut core, t0) = started(Some(spec(1)));
    let (generation, _) = bring_up_ui(&mut core, t0);
    take_calls(&mut core);
    core.handle(
        t0,
        Event::Action {
            id: ACTION_QUIT.into(),
            origin: ActionOrigin::Tray,
        },
    );
    assert!(take_calls(&mut core).contains(&Call::Send(generation, HostMessage::Shutdown)));
    assert_eq!(core.exit_requested(), None, "UI 还没退出，宿主不能先走");
    core.handle(t0, Event::UiExited { generation });
    assert_eq!(core.exit_requested(), Some(false));
    // UI 退出后安排工作集修剪
    assert!(core.next_deadline().is_some());

    let (mut core, t0) = started(Some(spec(1)));
    take_calls(&mut core);
    core.handle(
        t0,
        Event::Action {
            id: ACTION_RESTART.into(),
            origin: ActionOrigin::Tray,
        },
    );
    assert_eq!(core.exit_requested(), Some(true));
    core.finish();
    assert_eq!(take_calls(&mut core), vec![Call::Restart]);
    // 普通退出不拉起新宿主
    let (mut core, t0) = started(Some(spec(1)));
    take_calls(&mut core);
    core.handle(
        t0,
        Event::Action {
            id: ACTION_QUIT.into(),
            origin: ActionOrigin::Tray,
        },
    );
    core.finish();
    assert!(take_calls(&mut core).is_empty());
}

/// 命令行来源的动作与热键走同一条路，来源标记为 ipc。
#[test]
fn ipc_actions_carry_ipc_origin() {
    let (mut core, t0) = started(Some(spec(1)));
    let (generation, _) = bring_up_ui(&mut core, t0);
    take_calls(&mut core);
    core.handle(
        t0,
        Event::Action {
            id: "settings".into(),
            origin: ActionOrigin::Ipc,
        },
    );
    let calls = take_calls(&mut core);
    assert!(calls.contains(&Call::Send(
        generation,
        HostMessage::Command {
            seq: 2,
            id: "settings".into(),
            origin: Some(ActionOrigin::Ipc)
        }
    )));
}

/// 工作集修剪：启动 5 秒后一次；UI 退出后 2 秒一次；到点才调用，调用后不再重复。
#[test]
fn trim_is_scheduled_after_start_and_ui_exit() {
    let (mut core, t0) = started(Some(spec(1)));
    take_calls(&mut core);
    core.tick(t0 + Duration::from_secs(4));
    assert!(take_calls(&mut core).is_empty());
    core.tick(t0 + TRIM_AFTER_START);
    assert_eq!(take_calls(&mut core), vec![Call::Trim]);
    assert_eq!(core.next_deadline(), None, "空闲时没有任何待办，宿主不轮询");
    // UI 正常退出后再修剪一次
    let (generation, _) = bring_up_ui(&mut core, t0 + Duration::from_secs(10));
    core.handle(
        t0 + Duration::from_secs(10),
        Event::SessionLine {
            generation,
            line: line(&UiMessage::Ack { seq: 1 }),
        },
    );
    core.handle(
        t0 + Duration::from_secs(10),
        Event::SessionLine {
            generation,
            line: line(&UiMessage::ExitRequest { last_seq: 1 }),
        },
    );
    core.handle(t0 + Duration::from_secs(11), Event::UiExited { generation });
    take_calls(&mut core);
    core.tick(t0 + Duration::from_secs(13));
    assert_eq!(take_calls(&mut core), vec![Call::Trim]);
}

/// 握手超时由 tick 驱动：10 秒后结束进程并提示。
#[test]
fn hello_timeout_is_driven_by_tick() {
    let (mut core, t0) = started(Some(spec(1)));
    take_calls(&mut core);
    core.handle(
        t0,
        Event::Action {
            id: "capture".into(),
            origin: ActionOrigin::Hotkey,
        },
    );
    take_calls(&mut core);
    core.tick(t0 + Duration::from_secs(10));
    let calls = take_calls(&mut core);
    assert!(calls.contains(&Call::Kill(1001)));
    assert!(calls.contains(&Call::Tooltip("Cisox: ui failed".into())));
}

// ---------- 连续误触发保护 ----------

/// 一个 UI 会话里发给 UI 的某个命令的数量（看有没有被放行）。
fn sent_commands(calls: &[Call], id: &str) -> usize {
    calls
        .iter()
        .filter(|c| matches!(c, Call::Send(_, HostMessage::Command { id: sent, .. }) if sent == id))
        .count()
}

/// 一次热键触发。
fn hotkey(id: &str) -> Event {
    Event::Action {
        id: id.into(),
        origin: ActionOrigin::Hotkey,
    }
}

/// UI 上报“截图会话已结束”。
fn capture_idle(generation: u64) -> Event {
    Event::SessionLine {
        generation,
        line: line(&UiMessage::CaptureIdle),
    }
}

/// 让保护走到“弹了确认框”：UI 已起，`bring_up_ui` 的那次 `capture` 算第 1 次开启，之后每次
/// “关闭 → 1 秒后再开”，直到第 6 次开启被拦下。返回会话代数与被拦下那次触发的时刻。
fn run_until_asked(core: &mut HostCore<Fake>, t0: Instant) -> (u64, Instant) {
    let (generation, _) = bring_up_ui(core, t0);
    let mut t = t0;
    for _ in 0..crate::rapid::RAPID_REOPEN_STREAK {
        core.handle(t, capture_idle(generation));
        t += Duration::from_secs(1);
        core.handle(t, hotkey("capture"));
    }
    (generation, t)
}

/// 连续 5 个短间隔：前 5 次放行，第 6 次不分发，改弹确认框（标题是产品名、正文带次数）。
#[test]
fn rapid_reopen_asks_instead_of_dispatching() {
    let (mut core, t0) = started(Some(spec(1)));
    take_calls(&mut core);
    run_until_asked(&mut core, t0);
    let calls = take_calls(&mut core);
    assert_eq!(sent_commands(&calls, "capture"), 5, "前 5 次放行");
    assert!(calls.contains(&Call::RapidPrompt("Cisox".into(), "rapid 5".into())));
}

/// 中间有一次间隔恰好 5 秒（不算太快）就清零：同样 6 次触发不会询问。
#[test]
fn long_gap_prevents_prompt() {
    let (mut core, t0) = started(Some(spec(1)));
    let (generation, _) = bring_up_ui(&mut core, t0);
    take_calls(&mut core);
    let mut t = t0;
    for round in 0..crate::rapid::RAPID_REOPEN_STREAK {
        core.handle(t, capture_idle(generation));
        t += if round == 2 {
            crate::rapid::RAPID_REOPEN_GAP
        } else {
            Duration::from_secs(1)
        };
        core.handle(t, hotkey("capture"));
    }
    let calls = take_calls(&mut core);
    assert_eq!(sent_commands(&calls, "capture"), 5);
    assert!(!calls.iter().any(|c| matches!(c, Call::RapidPrompt(..))));
}

/// 不开覆盖窗的动作（设置、整屏截图）不计数。
#[test]
fn non_overlay_actions_are_not_counted() {
    let (mut core, t0) = started(Some(spec(1)));
    let (generation, _) = bring_up_ui(&mut core, t0);
    take_calls(&mut core);
    let mut t = t0;
    for _ in 0..10 {
        core.handle(t, capture_idle(generation));
        t += Duration::from_secs(1);
        core.handle(t, hotkey("settings"));
        core.handle(t, hotkey("quick.screenshot_full_screen"));
    }
    let calls = take_calls(&mut core);
    assert!(!calls.iter().any(|c| matches!(c, Call::RapidPrompt(..))));
}

/// 弹窗期间新到的截图类命令直接丢弃（不排队）；选“是”后只放行被拦下的那一条，之后进入冷却，
/// 冷却过后重新计数。
#[test]
fn drop_while_prompting_then_yes_dispatches_pending_and_cools_down() {
    let (mut core, t0) = started(Some(spec(1)));
    take_calls(&mut core);
    let (generation, t) = run_until_asked(&mut core, t0);
    take_calls(&mut core);
    // 弹窗期间：丢弃，不排队、不弹第二个框
    core.handle(t, hotkey("capture"));
    core.handle(t, hotkey("start_recording"));
    assert!(take_calls(&mut core).is_empty());
    // 选“是”：被拦下的 capture 照常分发
    core.handle(t, Event::RapidCaptureAnswered { quit: false });
    let calls = take_calls(&mut core);
    assert_eq!(sent_commands(&calls, "capture"), 1);
    assert_eq!(
        sent_commands(&calls, "start_recording"),
        0,
        "弹窗期间的不补发"
    );
    // 冷却内再连续触发：一律放行，不再询问
    let mut now = t;
    for _ in 0..10 {
        core.handle(now, capture_idle(generation));
        now += Duration::from_secs(1);
        core.handle(now, hotkey("capture"));
    }
    let calls = take_calls(&mut core);
    assert_eq!(sent_commands(&calls, "capture"), 10);
    assert!(!calls.iter().any(|c| matches!(c, Call::RapidPrompt(..))));
    // 冷却过后重新计数：第一次开启不计，再连续 5 个短间隔才询问
    now += crate::rapid::RAPID_COOLDOWN;
    core.handle(now, capture_idle(generation));
    core.handle(now, hotkey("capture"));
    for _ in 0..crate::rapid::RAPID_REOPEN_STREAK {
        core.handle(now, capture_idle(generation));
        now += Duration::from_secs(1);
        core.handle(now, hotkey("capture"));
    }
    let calls = take_calls(&mut core);
    assert!(calls.iter().any(|c| matches!(c, Call::RapidPrompt(..))));
}

/// 选“否”：走与托盘“退出”相同的正常退出路径（UI 先优雅关闭），之后截图类命令仍被丢弃。
#[test]
fn answering_no_quits_through_the_normal_path() {
    let (mut core, t0) = started(Some(spec(1)));
    take_calls(&mut core);
    let (generation, t) = run_until_asked(&mut core, t0);
    take_calls(&mut core);
    core.handle(t, Event::RapidCaptureAnswered { quit: true });
    let calls = take_calls(&mut core);
    assert!(calls.contains(&Call::Send(generation, HostMessage::Shutdown)));
    assert!(core.machine().is_quitting());
    assert_eq!(sent_commands(&calls, "capture"), 0, "被拦下的命令不再分发");
    core.handle(t, hotkey("capture"));
    assert_eq!(sent_commands(&take_calls(&mut core), "capture"), 0);
}

/// 确认框拉不起来：按刻意放行被拦下的命令并进入冷却，不能让提示状态卡死。
#[test]
fn prompt_spawn_failure_releases_the_command() {
    let (mut core, t0) = started(Some(spec(1)));
    take_calls(&mut core);
    core.platform.prompt_fails = true;
    run_until_asked(&mut core, t0);
    let calls = take_calls(&mut core);
    assert_eq!(sent_commands(&calls, "capture"), 6, "第 6 次也放行了");
    assert!(!core.rapid.is_prompting());
}

/// UI 进程退出也算覆盖窗关闭：每次都是“UI 退出 → 1 秒后再触发”同样会询问。
#[test]
fn ui_exit_counts_as_close() {
    let (mut core, t0) = started(Some(spec(1)));
    take_calls(&mut core);
    let mut t = t0;
    core.handle(t, hotkey("capture"));
    for _ in 0..crate::rapid::RAPID_REOPEN_STREAK {
        let generation = core.machine().generation();
        core.handle(t, Event::UiExited { generation });
        t += Duration::from_secs(1);
        core.handle(t, hotkey("capture"));
    }
    let calls = take_calls(&mut core);
    assert!(calls.iter().any(|c| matches!(c, Call::RapidPrompt(..))));
}

/// 鼠标手势 `begin` 也计入：被拦下的手势取消钩子里的手势，它后续的 update / finish 不会发给 UI；
/// 选“是”后冷却，但过期的手势不补发。
#[test]
fn gesture_begin_is_guarded_and_its_tail_is_dropped() {
    let (mut core, t0) = started(Some(spec_with_mouse(1, &["screenshot_copy"])));
    let (generation, _) = bring_up_ui(&mut core, t0);
    take_calls(&mut core);
    let gesture = |phase: GesturePhase, id: u64| {
        Event::Gesture(GestureEvent {
            phase,
            id,
            action: (phase == GesturePhase::Begin).then(|| "screenshot_copy".to_string()),
            x: 1,
            y: 1,
        })
    };
    let mut t = t0;
    for id in 0..u64::from(crate::rapid::RAPID_REOPEN_STREAK) {
        core.handle(t, gesture(GesturePhase::Begin, id));
        core.handle(t, gesture(GesturePhase::Finish, id));
        core.handle(t, capture_idle(generation));
        t += Duration::from_secs(1);
    }
    take_calls(&mut core);
    // 第 6 次 begin 被拦下
    core.handle(t, gesture(GesturePhase::Begin, 99));
    core.handle(t, gesture(GesturePhase::Update, 99));
    core.handle(t, gesture(GesturePhase::Finish, 99));
    let calls = take_calls(&mut core);
    assert!(calls.contains(&Call::CancelGesture));
    assert!(calls.iter().any(|c| matches!(c, Call::RapidPrompt(..))));
    assert!(
        !calls
            .iter()
            .any(|c| matches!(c, Call::Send(_, HostMessage::Gesture { .. }))),
        "被拦下的手势一条都不能发给 UI"
    );
    // 选“是”：手势已过期，不补发
    core.handle(t, Event::RapidCaptureAnswered { quit: false });
    let calls = take_calls(&mut core);
    assert!(!calls.iter().any(|c| matches!(c, Call::Send(..))));
}

/// 快照里没有本地化文案（旧快照）：确认框用内置英文兜底，产品名与次数都已替换。
#[test]
fn prompt_falls_back_to_builtin_text() {
    let mut s = spec(1);
    s.strings.notice_rapid_capture = String::new();
    let (mut core, t0) = started(Some(s));
    take_calls(&mut core);
    run_until_asked(&mut core, t0);
    let text = take_calls(&mut core)
        .into_iter()
        .find_map(|c| match c {
            Call::RapidPrompt(_, text) => Some(text),
            _ => None,
        })
        .expect("应弹确认框");
    assert!(text.contains("5 times") && text.contains("Cisox"), "{text}");
}
