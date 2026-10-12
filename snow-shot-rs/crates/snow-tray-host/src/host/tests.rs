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
    StopTray,
    Theme(TrayTheme),
    Hotkeys(Vec<String>),
    Trim,
    Restart,
}

/// 假平台。
#[derive(Default)]
struct Fake {
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
    /// 拉起是否失败。
    spawn_fails: bool,
    /// 下一个 UI 进程 ID。
    next_pid: u32,
}

impl Platform for Fake {
    fn spawn_ui(&mut self, generation: u64) -> Result<SpawnedUi, String> {
        self.calls.push(Call::SpawnUi(generation));
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
        rgba: vec![1, 2, 3, 4],
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
    s3.tray.icon_rgba.rgba = vec![9, 9, 9, 9];
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

/// 切换前台全屏闸门：菜单勾选更新、提示、并让 UI 落盘（set_config）；UI 不在就拉起一个来写。
#[test]
fn toggling_gate_persists_through_ui() {
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
    assert!(calls.contains(&Call::SpawnUi(1)), "UI 不在则拉起它来落盘");
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
