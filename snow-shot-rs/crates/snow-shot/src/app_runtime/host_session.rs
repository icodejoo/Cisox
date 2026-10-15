//! 双进程模式（`--ui`）下，UI 进程与托盘宿主的接线（方案 `docs/design/tray-host-split.md` §2、§4、§5）。
//!
//! 单进程（`--resident`）里托盘、热键、前台全屏闸门都在本进程；双进程里它们在宿主，本进程只剩：
//! - 收宿主转来的动作（托盘菜单项、热键、`--cmd`），映射回现有的 `UiEvent` / `AppCommand` 处理；
//! - 把“宿主要用的东西”写成 `host/spec.json` 快照（托盘菜单、热键表、开关、文案）并通知宿主；
//! - 设置页改热键：发快照 + `hotkeys_apply`，等宿主回报注册结果，失败回滚配置；
//! - 所有窗口关闭且无活动时请求退出（`exit_request`），宿主同意后立即退出，不保活；
//! - 宿主崩溃（会话意外断开）：优雅退出并拉起新宿主。
//!
//! 与 `AppState` 字段打交道的部分放在这里（子模块可读父模块私有字段）；序号、退出请求状态、
//! 待回报登记等纯逻辑在 `crate::host_link`，有离屏测试。

use super::*;
use crate::activity::{BusyInputs, busy_reasons as compute_busy_reasons};
use crate::host_link::{HostEvent, HostLink, PendingApply, apply_failure};
use crate::host_spec::{HostSpecInputs, command_from_id, host_spec_from_document};
use crate::host_spec_sync::SpecSync;
use snow_app_core::command::CommandContext;
use snow_host_protocol::launch::HOST_EXE_NAME;
use snow_host_protocol::message::{
    ActionOrigin, BalloonIcon as WireBalloonIcon, GesturePhase, HostMessage, UiMessage,
};
use snow_host_protocol::spec::HostSpec;
use snow_host_protocol::{
    ACTION_CAPTURE, ACTION_KEEP_ALIVE, ACTION_QUIT, ACTION_RESTART, ACTION_SCROLL_CAPTURE,
    ACTION_START_RECORDING, ACTION_TOGGLE_FULLSCREEN_GATE, ACTION_TOGGLE_GLOBAL_HOTKEYS,
    CAPTURE_ACTION_IDS, HOTKEYS_APPLY_TIMEOUT,
};
use snow_platform::global_mouse::{DragEvent, Point};
use snow_platform::single_instance::session_pipe::PipeWriter;
use snow_ui::shell::tray::Balloon;
use std::cell::Cell;

/// 等宿主回报热键注册结果超时时给设置页的失败原因（占位，设置页前缀已本地化）。
const APPLY_TIMEOUT_REASON: &str = "host did not respond";

/// 宿主转来的动作该怎么处理。
#[derive(Debug, Clone, PartialEq)]
pub(super) enum ActionTarget {
    /// 直接投主线程事件。
    Event(UiEvent),
    /// 走命令总线（热键 / 托盘原来的路径）。
    Command(AppCommand),
    /// 宿主要求常驻的占位动作：收到即确认，什么都不做。
    Noop,
    /// 宿主自己处理的动作或不认识的 id，忽略。
    Ignored,
}

/// 宿主转来的鼠标手势事件还原成钩子的拖动事件（坐标为虚拟桌面物理像素）。
///
/// # 参数
/// - `phase`：阶段。
/// - `id`：手势编号。
/// - `action`：动作标识（只有 `begin` 带）。
/// - `x` / `y`：位置（`cancel` 没有位置，忽略）。
///
/// # 返回
/// 拖动事件；`begin` 没带动作标识（协议不一致）返回 `None`，调用方记日志并忽略。
///
/// ```ignore
/// let event = drag_event_from_host(GesturePhase::Finish, 3, None, 10, 20);
/// assert!(matches!(event, Some(DragEvent::Finish { id: 3, .. })));
/// ```
pub(super) fn drag_event_from_host(
    phase: GesturePhase,
    id: u64,
    action: Option<String>,
    x: i32,
    y: i32,
) -> Option<DragEvent> {
    let pos = Point { x, y };
    Some(match phase {
        GesturePhase::Begin => DragEvent::Begin {
            id,
            action: action?,
            pos,
        },
        GesturePhase::Update => DragEvent::Update { id, pos },
        GesturePhase::Finish => DragEvent::Finish { id, pos },
        GesturePhase::Cancel => DragEvent::Cancel { id },
    })
}

/// 把宿主转来的动作 id 映射成要执行的东西。
///
/// 截图、录屏、长截图直接投事件（与总线里同名命令的处理器等价，来源标签保持 `hotkey` / `tray` / `ipc`）；
/// 托盘信号走 [`map_tray_signal`]；其余命令 id 还原成 `AppCommand` 走总线。
///
/// # 参数
/// - `id`：动作 id。
/// - `origin`：触发来源；旧宿主不带来源则为 `None`。
pub(super) fn target_for_action(id: &str, origin: Option<ActionOrigin>) -> ActionTarget {
    let label = match origin {
        Some(ActionOrigin::Hotkey) => ORIGIN_HOTKEY,
        Some(ActionOrigin::Tray) => ORIGIN_TRAY,
        Some(ActionOrigin::Ipc) => ORIGIN_IPC,
        None => ORIGIN_OTHER,
    };
    match id {
        ACTION_KEEP_ALIVE => ActionTarget::Noop,
        ACTION_CAPTURE => ActionTarget::Event(UiEvent::Capture { origin: label }),
        ACTION_START_RECORDING => ActionTarget::Event(UiEvent::StartRecording),
        ACTION_SCROLL_CAPTURE => ActionTarget::Event(UiEvent::StartScrollCapture),
        // 这四个由宿主自己处理，转到 UI 说明宿主与 UI 版本不一致，忽略
        ACTION_QUIT
        | ACTION_RESTART
        | ACTION_TOGGLE_GLOBAL_HOTKEYS
        | ACTION_TOGGLE_FULLSCREEN_GATE => ActionTarget::Ignored,
        other => map_tray_signal(other)
            .map(ActionTarget::Event)
            .or_else(|| command_from_id(other).map(ActionTarget::Command))
            .unwrap_or(ActionTarget::Ignored),
    }
}

/// 配置键变化后是否需要重新生成宿主快照（托盘菜单、热键表、开关、保活标志的来源）。
///
/// # 参数
/// - `key`：配置键。
pub(super) fn affects_host_spec(key: &str) -> bool {
    key.starts_with("tray/")
        || key.starts_with("global_shortcuts/")
        || key.starts_with("global_mouse/")
        || key.starts_with("dictation/")
        || key.starts_with("updates/")
        || key == LANGUAGE_KEY
        || key == THEME_MODE_KEY
        || key == DELAY_SECONDS_CONFIG_KEY
        || key == crate::mcp_host::KEY_ENABLED
        || key == crate::ocr_service::KEY_HOT_START
}

/// 活动状态的快照（只在变化时才上报给宿主）。
#[derive(Debug, Clone, PartialEq, Eq)]
struct Activity {
    /// 窗口数。
    windows: u32,
    /// 贴图数。
    pins: u32,
    /// 阻止退出的原因。
    busy: Vec<String>,
}

/// UI 进程这一侧的宿主会话。
pub(crate) struct HostSession {
    /// 会话客户端（序号、退出请求、待回报登记）。
    link: HostLink<PipeWriter>,
    /// 命令总线（热键 / 托盘原来的路径）。
    bus: CommandBus,
    /// 快照同步器。
    sync: RefCell<SpecSync>,
    /// 是否已经排了一次“重新生成快照”的事件（多次请求合并成一次）。
    spec_dirty: Cell<bool>,
    /// 启动时的快照校验是否做过（推迟到第一次空闲，不占“热键到首帧”的路径）。
    startup_synced: Cell<bool>,
    /// 最近一次上报的活动状态。
    last_activity: RefCell<Option<Activity>>,
    /// 最近一次写进快照的“登录要恢复的贴图数”（`None` 表示还没写过，第一次空闲时校一遍）。
    last_restorable: Cell<Option<u32>>,
    /// 是否已经向宿主报告过“截图会话已结束”；宿主每转来一条命令 / 手势就清零，
    /// 会话再落到空闲时补报一次（即使这条命令什么都没开，宿主也能收到关闭信号）。
    capture_idle_sent: Cell<bool>,
}

impl HostSession {
    /// 创建会话。
    ///
    /// # 参数
    /// - `link`：已连上宿主的会话客户端。
    /// - `bus`：命令总线。
    /// - `sync`：快照同步器。
    pub(crate) fn new(link: HostLink<PipeWriter>, bus: CommandBus, sync: SpecSync) -> Self {
        Self {
            link,
            bus,
            sync: RefCell::new(sync),
            spec_dirty: Cell::new(false),
            startup_synced: Cell::new(false),
            last_activity: RefCell::new(None),
            last_restorable: Cell::new(None),
            capture_idle_sent: Cell::new(true),
        }
    }

    /// 请宿主改托盘悬停提示（单进程里是本进程直接改托盘）。
    ///
    /// # 参数
    /// - `text`：悬停提示全文。
    pub(crate) fn send_tooltip(&self, text: &str) {
        self.link.send(&UiMessage::Tooltip {
            text: text.to_string(),
        });
    }

    /// 请宿主弹系统通知气泡。
    ///
    /// # 参数
    /// - `balloon`：标题、正文、可选小图和点击后要打开的路径。
    pub(crate) fn send_balloon(&self, balloon: Balloon) {
        self.link.send(&UiMessage::Balloon {
            title: balloon.title,
            text: balloon.text,
            icon: balloon.icon.map(|icon| WireBalloonIcon {
                width: icon.width,
                height: icon.height,
                rgba: icon.rgba,
            }),
            open: balloon.open.map(|path| path.to_string_lossy().into_owned()),
        });
    }

    /// 发布快照；内容有变化才写并返回新修订号，写盘失败记日志并返回 `None`。
    fn publish(&self, spec: HostSpec) -> Option<u64> {
        match self.sync.borrow_mut().publish(spec) {
            Ok(rev) => rev,
            Err(e) => {
                tracing::warn!(error = %e, "写入宿主快照失败");
                None
            }
        }
    }
}

/// 由当前配置与运行状态算出宿主快照（纯读取，不写盘）。
fn spec_of(state: &AppState) -> Result<HostSpec, String> {
    let config = state.config.borrow();
    let document = config.document();
    let locale = locale_of(document);
    let groups = tray_group_labels(state, locale);
    host_spec_from_document(
        document,
        &HostSpecInputs {
            locale,
            hotkeys_paused: false,
            groups: &groups,
            active_group: &state.pins.shared().active_group_id(),
            pins_to_restore: state.pins.shared().restorable_count(),
            update_due_at: crate::net_settings::update_due_at(
                document,
                &state.data_root,
                crate::net_settings::unix_now(),
            ),
            rev: 0,
        },
    )
}

/// 把“下次登录要恢复的贴图数”写进快照（只改标志，不重新渲染菜单）；数没变就什么都不做。
///
/// 宿主不需要被通知：这个值只在宿主下次启动（登录、重启、崩溃后）时读。
///
/// # 参数
/// - `state`：运行时状态。
/// - `session`：宿主会话。
fn sync_restorable_pins(state: &AppState, session: &HostSession) {
    let count = state.pins.shared().restorable_count();
    if session.last_restorable.replace(Some(count)) == Some(count) {
        return;
    }
    match session
        .sync
        .borrow_mut()
        .patch_flags(|flags| flags.pins_to_restore = count)
    {
        Ok(Some(rev)) => tracing::debug!(count, rev, "登录恢复贴图数已写入快照"),
        Ok(None) => {}
        Err(e) => tracing::warn!(error = %e, "写入登录恢复贴图数失败"),
    }
}

/// 退出前（贴图已落盘之后）校准快照里的登录恢复贴图数：宿主重启 / 崩溃后重拉时据此恢复贴图。
///
/// # 参数
/// - `state`：运行时状态（没接宿主会话时什么都不做）。
pub(super) fn finalize_on_shutdown(state: &AppState) {
    if let Some(session) = state.host.as_ref() {
        // 强制重算：落盘后的记录数才是下次要恢复的数
        session.last_restorable.set(None);
        sync_restorable_pins(state, session);
    }
}

/// 请宿主取消进行中的鼠标手势（UI 放弃了这次拖选，钩子别再吞鼠标输入）。
///
/// # 参数
/// - `state`：运行时状态。
/// - `id`：被放弃的手势编号。
pub(super) fn send_cancel_gesture(state: &AppState, id: u64) {
    if let Some(session) = state.host.as_ref() {
        session.link.send(&UiMessage::CancelGesture { id });
    }
}

/// 重新生成并发布快照；有变化就通知宿主。
fn sync_now(state: &AppState, session: &HostSession) {
    match spec_of(state) {
        Ok(spec) => {
            if let Some(rev) = session.publish(spec) {
                session.link.send(&UiMessage::SpecChanged { rev });
            }
        }
        Err(e) => tracing::warn!(error = %e, "生成宿主快照失败"),
    }
}

/// 请求重新生成快照（配置、托盘分组变了）：多次请求合并成一次，延后到收件箱里排队处理。
///
/// # 参数
/// - `state`：运行时状态（需已接上宿主会话）。
pub(super) fn request_spec_sync(state: &AppState) {
    if let Some(session) = state.host.as_ref()
        && !session.spec_dirty.replace(true)
    {
        state.inbox.push(UiEvent::Host(HostEvent::SyncSpec));
    }
}

/// 现场快照：把运行时状态里“能不能退出”相关的东西摘出来（判定本身是 `activity::busy_reasons` 的纯函数）。
///
/// # 参数
/// - `cx`：外壳上下文（窗口数、录屏 / 长截图控制窗是否还开着）。
/// - `state`：运行时状态。
fn busy_inputs(cx: &ShellContext, state: &AppState) -> BusyInputs {
    let config = state.config.borrow();
    let document = config.document();
    BusyInputs {
        windows: cx.window_count(),
        capture_in_flight: state.capture_in_flight,
        direct_in_flight: state.direct_in_flight,
        delay_pending: state.delay.is_pending(),
        dictation_active: state.dictation.is_active(),
        recording: state.recording.is_busy(cx),
        scroll_capture: state.scroll.is_busy(cx),
        mcp: state.mcp.is_running() || document.bool_or(crate::mcp_host::KEY_ENABLED, false),
        ocr_hot_start: document.bool_or(crate::ocr_service::KEY_HOT_START, false),
    }
}

/// 每处理完一个事件后调用：上报活动状态的变化；收件箱已空且无窗口无活动时请求退出。
///
/// 退出判定不用定时器：只在“主线程事件处理完、收件箱为空”这个时刻看一眼。首次空闲时按文件时间
/// 校验一次快照（比 `config.json` 或 UI exe 旧才重新生成，生成一次要 ~235ms，不能每次启动都做）。
///
/// # 参数
/// - `cx`：外壳上下文。
/// - `state`：运行时状态。
pub(super) fn after_event(cx: &ShellContext, state: &AppState) {
    let Some(session) = state.host.as_ref() else {
        return;
    };
    let inputs = busy_inputs(cx, state);
    let busy = compute_busy_reasons(&inputs, &state.activity);
    let activity = Activity {
        windows: inputs.windows as u32,
        pins: state.pins.pin_count() as u32,
        busy: busy.clone(),
    };
    if session.last_activity.borrow().as_ref() != Some(&activity) {
        // 贴图增减时顺手把“登录要恢复的贴图数”写进快照（第一次也校一遍）
        let pins_changed = session
            .last_activity
            .borrow()
            .as_ref()
            .is_none_or(|last| last.pins != activity.pins);
        session.link.send(&UiMessage::Activity {
            windows: activity.windows,
            pins: activity.pins,
            busy_reasons: activity.busy.clone(),
        });
        *session.last_activity.borrow_mut() = Some(activity);
        if pins_changed {
            sync_restorable_pins(state, session);
        }
    }
    report_capture_idle(cx, state, session);
    if !busy.is_empty() || !state.inbox.is_empty() {
        return;
    }
    if !session.startup_synced.replace(true) {
        let config_path = state.config.borrow().path().to_path_buf();
        let exe = std::env::current_exe().ok();
        let sources: Vec<&std::path::Path> = std::iter::once(config_path.as_path())
            .chain(exe.as_deref())
            .collect();
        if session.sync.borrow().is_stale(&sources) {
            sync_now(state, session);
        }
    }
    if session.link.request_exit() {
        tracing::info!("所有窗口已关闭且无活动，请求退出");
    }
}

/// 截图类会话（采集中、延迟倒计时、鼠标手势、覆盖窗）是否在进行。
///
/// # 参数
/// - `cx`：外壳上下文。
/// - `state`：运行时状态。
fn capture_session_active(cx: &ShellContext, state: &AppState) -> bool {
    state.capture_in_flight
        || state.delay.is_pending()
        || state.gesture.is_some()
        || any_overlay_open(cx, state)
}

/// 截图类会话落到空闲时向宿主报告一次 `capture_idle`（宿主据此量“覆盖窗关闭 → 下次开启”的间隔，
/// 做连续误触发保护）。收件箱里还有没处理的事件时不报：刚转来的命令还没来得及开会话。
///
/// # 参数
/// - `cx`：外壳上下文。
/// - `state`：运行时状态。
/// - `session`：宿主会话。
fn report_capture_idle(cx: &ShellContext, state: &AppState, session: &HostSession) {
    if capture_session_active(cx, state) {
        session.capture_idle_sent.set(false);
        return;
    }
    if !state.inbox.is_empty() || session.capture_idle_sent.replace(true) {
        return;
    }
    session.link.send(&UiMessage::CaptureIdle);
}

/// 处理一个宿主侧事件（在 GPUI 主线程上）。
///
/// # 参数
/// - `cx`：外壳上下文。
/// - `state`：运行时状态。
/// - `event`：宿主侧事件。
pub(super) fn handle_host_event(cx: &mut ShellContext, state: &mut AppState, event: HostEvent) {
    let Some(session) = state.host.clone() else {
        return;
    };
    match event {
        HostEvent::Message(message) => handle_message(cx, state, &session, message),
        HostEvent::Lost => on_lost(cx, state, &session),
        HostEvent::HotkeysApplyTimeout { rev } => {
            if let Some(pending) = session.link.take_apply(rev) {
                tracing::warn!(rev, "宿主没有及时回报热键注册结果，按失败处理");
                rollback_hotkey_change(cx, state, &session, pending, APPLY_TIMEOUT_REASON);
            }
        }
        HostEvent::SyncSpec => {
            session.spec_dirty.set(false);
            sync_now(state, &session);
        }
        HostEvent::WindowsChanged => {}
    }
}

/// 处理宿主发来的一条消息。
fn handle_message(
    cx: &mut ShellContext,
    state: &mut AppState,
    session: &Rc<HostSession>,
    message: HostMessage,
) {
    match message {
        HostMessage::Command { seq, id, origin } => {
            if session.link.accept(seq) == snow_host_protocol::seq::SeqCheck::Duplicate {
                return;
            }
            if CAPTURE_ACTION_IDS.contains(&id.as_str()) {
                session.capture_idle_sent.set(false);
            }
            match target_for_action(&id, origin) {
                ActionTarget::Event(event) => {
                    state.inbox.push(event);
                }
                ActionTarget::Command(command) => {
                    let source = match origin {
                        Some(ActionOrigin::Hotkey) => CommandSource::Hotkey,
                        Some(ActionOrigin::Tray) => CommandSource::Tray,
                        _ => CommandSource::Ui,
                    };
                    if let Err(e) = session.bus.emit(&CommandContext::new(source), command) {
                        tracing::warn!(%id, error = %e, "宿主转来的命令执行失败");
                    }
                }
                ActionTarget::Noop => tracing::debug!("宿主要求 UI 常驻，无需动作"),
                ActionTarget::Ignored => tracing::warn!(%id, "宿主转来的动作无法识别，已忽略"),
            }
        }
        HostMessage::RestorePins { seq } => {
            if session.link.accept(seq) != snow_host_protocol::seq::SeqCheck::Duplicate {
                state.inbox.push(UiEvent::RestorePins);
            }
        }
        HostMessage::SetConfig { seq, key, value } => {
            if session.link.accept(seq) != snow_host_protocol::seq::SeqCheck::Duplicate {
                apply_set_config(state, &key, value);
            }
        }
        HostMessage::Gesture {
            seq,
            phase,
            id,
            action,
            x,
            y,
        } => {
            // update 每秒上百条，宿主不等确认：安静地登记序号，不回 ack
            let check = if phase == GesturePhase::Update {
                session.link.accept_quiet(seq)
            } else {
                session.link.accept(seq)
            };
            if check == snow_host_protocol::seq::SeqCheck::Duplicate {
                return;
            }
            if phase == GesturePhase::Begin {
                session.capture_idle_sent.set(false);
            }
            match drag_event_from_host(phase, id, action, x, y) {
                Some(event) => {
                    state.inbox.push(UiEvent::MouseGesture(event));
                }
                None => tracing::warn!(id, "宿主转来的手势 begin 没带动作标识，已忽略"),
            }
        }
        HostMessage::Shutdown | HostMessage::ExitOk => {
            session.link.begin_closing();
            shutdown_app(cx, state);
        }
        HostMessage::ExitDenied => session.link.exit_denied(),
        HostMessage::HotkeysResult { rev, results } => {
            let Some(pending) = session.link.take_apply(rev) else {
                // 回滚后的那次重新应用没有登记待回报项，结果无需处理
                return;
            };
            match apply_failure(&results, pending.config_key) {
                Some(reason) => rollback_hotkey_change(cx, state, session, pending, &reason),
                None => tracing::info!(rev, key = pending.config_key, "宿主已按新热键注册"),
            }
        }
    }
}

/// 宿主要求改配置（目前只有前台全屏闸门开关）：写入、落盘，再按普通配置变更走一遍后续处理。
fn apply_set_config(state: &mut AppState, key: &str, value: Value) {
    let previous = state.config.borrow().document().value(key);
    let result = {
        let mut store = state.config.borrow_mut();
        store
            .set_value(key, value)
            .map_err(|e| e.to_string())
            .and_then(|_| store.flush().map_err(|e| e.to_string()))
    };
    match result {
        Ok(()) => {
            state.inbox.push(UiEvent::ConfigChanged {
                key: key.to_string(),
                previous,
            });
        }
        Err(e) => tracing::warn!(key, error = %e, "宿主要求的配置修改失败"),
    }
}

/// 设置页改了热键相关配置：发快照 + `hotkeys_apply`，等宿主回报（超时按失败处理）；失败回滚配置。
///
/// 热键格式本身无效（解析失败）不必打扰宿主，直接回滚。
///
/// # 参数
/// - `cx`：外壳上下文。
/// - `state`：运行时状态。
/// - `config_key`：变更的配置键。
/// - `previous`：变更前的值。
pub(super) fn apply_hotkey_change(
    cx: &mut ShellContext,
    state: &mut AppState,
    config_key: &'static str,
    previous: Value,
) {
    let Some(session) = state.host.clone() else {
        return;
    };
    let pending = PendingApply {
        config_key,
        previous,
    };
    // 格式无效：与单进程里“注册阶段记成失败”一致，直接回滚
    let local = {
        let config = state.config.borrow();
        let plan = hotkey_plan(config.document(), false);
        let failures = plan
            .iter()
            .filter(|p| p.parsed.is_err())
            .map(|p| HotkeyFailure {
                config_key: p.config_key,
                shortcut: p.text.clone(),
                reason: p.parsed.as_ref().err().cloned().unwrap_or_default(),
            })
            .collect();
        HotkeyRegistration {
            handles: Vec::new(),
            failures,
        }
    };
    if hotkey_attempt_failed(&local, config_key) {
        let reason = describe_hotkey_failure(&local, config_key);
        tracing::warn!(key = config_key, %reason, "热键格式无效，回滚配置");
        rollback_hotkey_change(cx, state, &session, pending, &reason);
        return;
    }
    let rev = match spec_of(state) {
        Ok(spec) => session
            .publish(spec)
            .unwrap_or_else(|| session.sync.borrow().rev()),
        Err(e) => {
            tracing::warn!(error = %e, "生成宿主快照失败，热键未应用");
            return;
        }
    };
    session.link.register_apply(rev, pending);
    session.link.send(&UiMessage::HotkeysApply { rev });
    let inbox = state.inbox.clone();
    let spawned = std::thread::Builder::new()
        .name("hotkeys-apply-timer".into())
        .spawn(move || {
            std::thread::sleep(HOTKEYS_APPLY_TIMEOUT);
            inbox.push(UiEvent::Host(HostEvent::HotkeysApplyTimeout { rev }));
        });
    if let Err(e) = spawned {
        // 没有定时器就没有超时保护：不致命，宿主回报仍会到
        tracing::warn!(error = %e, "创建热键应用超时定时器失败");
    }
}

/// 回滚一次失败的热键变更：还原配置、重新发布快照并让宿主按旧热键重注册，提示设置页。
fn rollback_hotkey_change(
    cx: &mut ShellContext,
    state: &mut AppState,
    session: &Rc<HostSession>,
    pending: PendingApply,
    reason: &str,
) {
    tracing::warn!(key = pending.config_key, %reason, "全局热键重新注册失败，回滚配置");
    if let Err(e) = restore_value(&state.config, pending.config_key, pending.previous) {
        tracing::error!(key = pending.config_key, error = %e, "回滚配置失败");
    }
    if let Ok(spec) = spec_of(state)
        && let Some(rev) = session.publish(spec)
    {
        // 回滚后的重新应用没有登记待回报项：结果只用于让宿主恢复旧热键
        session.link.send(&UiMessage::HotkeysApply { rev });
    }
    if let Some(view) = state.settings_view.clone() {
        let message = format!(
            "{}: {reason}",
            crate::settings_text::t(
                view.read(cx.app()).language(),
                crate::settings_text::Text::HotkeyRegisterFailed,
            )
        );
        let key = pending.config_key;
        view.update(cx.app(), |v, cx| v.notify_reverted(key, message, cx));
    }
}

/// 宿主会话断开：正常关闭流程里是预期内的；否则视为宿主崩溃，
/// 走一遍优雅退出（贴图落盘等），最后一步拉起新宿主。
fn on_lost(cx: &mut ShellContext, state: &mut AppState, session: &Rc<HostSession>) {
    if session.link.is_closing() {
        return;
    }
    tracing::error!("托盘宿主会话意外断开，视为宿主崩溃：优雅退出并拉起新宿主");
    shutdown_app(cx, state);
    match std::env::current_exe() {
        Ok(exe) => {
            let host = exe.with_file_name(HOST_EXE_NAME);
            if let Err(e) = snow_net::quiet_command(&host).spawn() {
                tracing::error!(host = %host.display(), error = %e, "拉起新宿主失败");
            }
        }
        Err(e) => tracing::error!(error = %e, "取不到自身路径，无法拉起新宿主"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hotkey_plan::hotkey_plan;
    use snow_host_protocol::{ACTION_MAIN_WINDOW, ACTION_PIN_CLIPBOARD, ACTION_SETTINGS};

    /// 截图、录屏、长截图直接投事件；截图的来源标签按触发来源保持 hotkey / tray / ipc。
    #[test]
    fn capture_like_actions_map_to_events_with_origin_labels() {
        for (origin, label) in [
            (Some(ActionOrigin::Hotkey), ORIGIN_HOTKEY),
            (Some(ActionOrigin::Tray), ORIGIN_TRAY),
            (Some(ActionOrigin::Ipc), ORIGIN_IPC),
            (None, ORIGIN_OTHER),
        ] {
            assert_eq!(
                target_for_action(ACTION_CAPTURE, origin),
                ActionTarget::Event(UiEvent::Capture { origin: label })
            );
        }
        assert_eq!(
            target_for_action(ACTION_START_RECORDING, None),
            ActionTarget::Event(UiEvent::StartRecording)
        );
        assert_eq!(
            target_for_action(ACTION_SCROLL_CAPTURE, None),
            ActionTarget::Event(UiEvent::StartScrollCapture)
        );
    }

    /// 宿主的“保活”占位动作：收到即确认、什么都不做（既不开窗也不告警）。
    #[test]
    fn keep_alive_action_is_a_noop() {
        for origin in [None, Some(ActionOrigin::Hotkey), Some(ActionOrigin::Ipc)] {
            assert_eq!(
                target_for_action(ACTION_KEEP_ALIVE, origin),
                ActionTarget::Noop
            );
        }
    }

    /// 宿主转来的手势四个阶段还原成钩子的拖动事件；begin 没带动作标识（协议不一致）返回 `None`。
    #[test]
    fn host_gesture_maps_to_drag_events() {
        assert_eq!(
            drag_event_from_host(GesturePhase::Begin, 5, Some("screenshot_ocr".into()), -7, 9),
            Some(DragEvent::Begin {
                id: 5,
                action: "screenshot_ocr".into(),
                pos: Point { x: -7, y: 9 },
            })
        );
        assert_eq!(
            drag_event_from_host(GesturePhase::Update, 5, None, 1, 2),
            Some(DragEvent::Update {
                id: 5,
                pos: Point { x: 1, y: 2 },
            })
        );
        assert_eq!(
            drag_event_from_host(GesturePhase::Finish, 5, None, 3, 4),
            Some(DragEvent::Finish {
                id: 5,
                pos: Point { x: 3, y: 4 },
            })
        );
        assert_eq!(
            drag_event_from_host(GesturePhase::Cancel, 5, None, 0, 0),
            Some(DragEvent::Cancel { id: 5 })
        );
        assert_eq!(
            drag_event_from_host(GesturePhase::Begin, 5, None, 0, 0),
            None
        );
    }

    /// 手势动作标识与 UI 侧的截图模式一一对应：宿主快照里 `mouse` 的每个动作都能映射成模式。
    #[test]
    fn every_gesture_action_has_a_capture_mode() {
        let doc = ConfigDocument::from_bytes(None);
        let spec = host_spec_from_document(
            &doc,
            &HostSpecInputs {
                locale: "en-US",
                hotkeys_paused: false,
                groups: &[],
                active_group: "",
                pins_to_restore: 0,
                update_due_at: None,
                rev: 1,
            },
        )
        .unwrap();
        assert!(!spec.mouse.is_empty(), "默认配置带手势绑定");
        for binding in &spec.mouse {
            assert!(
                crate::mouse_gesture::mode_for_action(&binding.action).is_some(),
                "{}",
                binding.action
            );
        }
    }

    /// `--cmd` 七个命令派生的动作 id 与旧入口 `map_ipc_command` 的结果一一对应。
    #[test]
    fn ipc_derived_ids_match_legacy_ipc_mapping() {
        use snow_platform::single_instance::IpcCommand;
        let table = [
            (IpcCommand::TriggerScreenshot, ACTION_CAPTURE),
            (IpcCommand::TriggerRecording, ACTION_START_RECORDING),
            (IpcCommand::ScrollCapture, ACTION_SCROLL_CAPTURE),
            (IpcCommand::PinClipboard, ACTION_PIN_CLIPBOARD),
            (IpcCommand::OpenSettings, ACTION_SETTINGS),
            (IpcCommand::ShowMainWindow, ACTION_MAIN_WINDOW),
        ];
        for (command, id) in table {
            let legacy = map_ipc_command(&command).expect("旧入口有映射");
            let mapped = match target_for_action(id, Some(ActionOrigin::Ipc)) {
                ActionTarget::Event(event) => event,
                other => panic!("{id} 应映射成事件: {other:?}"),
            };
            assert_eq!(mapped, legacy, "{command:?}");
        }
    }

    /// 托盘信号走现有映射；命令 id 还原成 AppCommand；宿主自己处理的动作与未知 id 被忽略。
    #[test]
    fn tray_signals_commands_and_host_owned_ids() {
        assert_eq!(
            target_for_action("history", Some(ActionOrigin::Tray)),
            ActionTarget::Event(UiEvent::OpenHistory)
        );
        assert_eq!(
            target_for_action("group:abc", Some(ActionOrigin::Tray)),
            ActionTarget::Event(UiEvent::PinGroupSwitch { id: "abc".into() })
        );
        assert_eq!(
            target_for_action("quick.open_settings", Some(ActionOrigin::Hotkey)),
            ActionTarget::Command(AppCommand::QuickAction(QuickAction::OpenSettings))
        );
        assert_eq!(
            target_for_action("toggle_dictation", Some(ActionOrigin::Hotkey)),
            ActionTarget::Command(AppCommand::ToggleDictation)
        );
        for id in [
            ACTION_QUIT,
            ACTION_RESTART,
            ACTION_TOGGLE_GLOBAL_HOTKEYS,
            ACTION_TOGGLE_FULLSCREEN_GATE,
            "no_such_action",
        ] {
            assert_eq!(target_for_action(id, None), ActionTarget::Ignored, "{id}");
        }
    }

    /// 宿主可能转来的所有动作 id 都能被识别：默认配置下热键表和托盘菜单里的每个 id 都有去处。
    #[test]
    fn every_hotkey_and_menu_action_has_a_target() {
        let doc = ConfigDocument::from_bytes(None);
        let spec = host_spec_from_document(
            &doc,
            &HostSpecInputs {
                locale: "zh-CN",
                hotkeys_paused: false,
                groups: &[("g1".to_string(), "分组".to_string())],
                active_group: "g1",
                pins_to_restore: 0,
                update_due_at: None,
                rev: 1,
            },
        )
        .unwrap();
        let mut ids: Vec<String> = spec.hotkeys.iter().map(|h| h.id.clone()).collect();
        ids.extend(spec.hotkeys.iter().filter_map(|h| h.on_release_id.clone()));
        for item in &spec.tray.menu {
            if let snow_host_protocol::spec::MenuItemSpec::Item { action_id, .. } = item {
                ids.push(action_id.clone());
            }
        }
        ids.extend(spec.tray.on_left.clone());
        ids.extend(spec.tray.on_middle.clone());
        ids.extend(spec.tray.on_double.clone());
        assert!(!ids.is_empty());
        let host_owned = [
            ACTION_QUIT,
            ACTION_RESTART,
            ACTION_TOGGLE_GLOBAL_HOTKEYS,
            ACTION_TOGGLE_FULLSCREEN_GATE,
        ];
        for id in ids {
            if host_owned.contains(&id.as_str()) {
                continue;
            }
            assert_ne!(
                target_for_action(&id, Some(ActionOrigin::Tray)),
                ActionTarget::Ignored,
                "动作 {id} 没有去处"
            );
        }
    }

    /// 影响宿主快照的配置键：托盘、全局快捷键、手势、语音、更新策略（到点时间）、语言、主题、延迟秒数、MCP、OCR 预热；
    /// 无关的键（截图质量等）不触发重新生成（生成一次要渲染菜单图标，不能每次改配置都做）。
    #[test]
    fn spec_relevant_keys() {
        for key in [
            "tray/menu_options",
            "global_shortcuts/screenshot",
            "global_shortcuts/disable_on_focused_fullscreen_window",
            "global_mouse/screenshot_copy",
            "dictation/trigger_mode",
            "updates/mode",
            "updates/manifest_url",
            LANGUAGE_KEY,
            THEME_MODE_KEY,
            DELAY_SECONDS_CONFIG_KEY,
            crate::mcp_host::KEY_ENABLED,
            crate::ocr_service::KEY_HOT_START,
        ] {
            assert!(affects_host_spec(key), "{key}");
        }
        for key in ["screenshot/image_quality", "storage/schema_version"] {
            assert!(!affects_host_spec(key), "{key}");
        }
    }

    /// 默认配置下所有热键格式都有效（热键格式本地校验的前提）。
    #[test]
    fn default_hotkeys_parse() {
        let doc = ConfigDocument::from_bytes(None);
        assert!(hotkey_plan(&doc, false).iter().all(|p| p.parsed.is_ok()));
    }
}
