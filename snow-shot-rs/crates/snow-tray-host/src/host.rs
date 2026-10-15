//! 宿主核心：把状态机、快照、热键暂停 / 前台全屏闸门、托盘与热键的应用串在一起。
//!
//! 所有系统调用（拉起进程、写管道、托盘、热键、前台探测、修剪工作集……）都在 [`Platform`] 特征后面，
//! 核心本身是单线程、不读时钟、不碰系统，所以整条链路（热键 → 闸门 → 状态机 → 拉起 UI → 握手 → 投递、
//! 快照更新、暂停开关、退出）都能用假平台离屏确定性地测试。真实平台在 `win` 模块。
//!
//! 线程模型：托盘线程、热键线程、管道读线程、进程等待线程只把事件投进同一个通道（[`Event`]），
//! 由主线程按序处理；主线程用 `recv_timeout(下一个截止时刻)` 等待，空闲时没有任何轮询。

use crate::gesture::GestureEvent;
use crate::machine::{Effect, HostMachine, Notice};
use crate::queue::Outbound;
use crate::rapid::{RAPID_REOPEN_GAP, RapidGuard, RapidVerdict, opens_capture_overlay};
use crate::view::{
    FALLBACK_NOTICE_UI_CRASH_LOOP, FALLBACK_NOTICE_UI_START_FAILED, aggregate_results,
    compose_tooltip, describe_failures, effective_hotkeys, effective_menu, fallback_spec,
    fill_failures, gate_config_change, rapid_capture_text,
};
use snow_data_paths::PRODUCT_NAME;
use snow_host_protocol::frame::decode_line;
use snow_host_protocol::launch::{HeadlessJob, UiHint};
use snow_host_protocol::message::{
    ActionOrigin, GesturePhase, HostMessage, HotkeyResult, UiMessage,
};
use snow_host_protocol::spec::{
    HostSpec, HotkeySpec, IconRgba, MenuItemSpec, MouseBindingSpec, TrayTheme,
};
use snow_host_protocol::spec_file::SpecFileError;
use snow_host_protocol::{
    ACTION_QUIT, ACTION_RESTART, ACTION_TOGGLE_FULLSCREEN_GATE, ACTION_TOGGLE_GLOBAL_HOTKEYS,
    UPDATE_CHECK_START_DELAY,
};
use snow_shell_core::tray::{Balloon, BalloonIcon};
use std::collections::VecDeque;
use std::path::PathBuf;
use std::time::{Duration, Instant};

/// 宿主启动后多久做第一次工作集修剪（等启动期的一次性分配落定）。
pub const TRIM_AFTER_START: Duration = Duration::from_secs(5);

/// UI 进程退出后多久做一次工作集修剪。
pub const TRIM_AFTER_UI_EXIT: Duration = Duration::from_secs(2);

/// 拉起 UI 进程的结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpawnedUi {
    /// UI 进程 ID。
    pub pid: u32,
    /// 传给 UI 的一次性令牌（校验 `hello` 用）。
    pub token: String,
}

/// 要显示的托盘（创建或替换时整体交给平台）。
#[derive(Debug, Clone, PartialEq)]
pub struct TrayPlan {
    /// 托盘图标（32x32）。
    pub icon: IconRgba,
    /// 悬停提示。
    pub tooltip: String,
    /// 右键菜单（已叠上宿主状态的勾选）。
    pub menu: Vec<MenuItemSpec>,
    /// 左键单击动作 id。
    pub on_left: Option<String>,
    /// 中键单击动作 id。
    pub on_middle: Option<String>,
    /// 左键双击动作 id。
    pub on_double: Option<String>,
}

/// 平台能力：宿主核心需要系统做的所有事。
pub trait Platform {
    /// 建会话管道并拉起 UI 进程（先建管道、再 `CreateProcess`，UI 一启动就连得上）。
    ///
    /// # 参数
    /// - `generation`：会话代数；管道事件、进程退出事件都要带着它回报。
    /// - `hint`：启动提示（队首命令是语音转文字时给 UI 提前预拉语音工作进程的机会），转成 `--hint` 参数。
    fn spawn_ui(&mut self, generation: u64, hint: UiHint) -> Result<SpawnedUi, String>;

    /// 往该代会话的管道写一条消息。
    fn send(&mut self, generation: u64, message: &HostMessage) -> Result<(), String>;

    /// 关闭该代会话的管道。
    fn close_session(&mut self, generation: u64);

    /// 强制结束 UI 进程（最后手段）。
    fn kill_ui(&mut self, pid: u32);

    /// 对 UI 进程调 `AllowSetForegroundWindow`。
    fn grant_foreground(&mut self, pid: u32);

    /// 读取 `host/spec.json`。
    fn read_spec(&mut self) -> Result<HostSpec, SpecFileError>;

    /// 拉起 `--headless emit-spec` 的 UI 生成快照；结束后平台投递 [`Event::SpecEmitted`]。
    fn spawn_emit_spec(&mut self) -> Result<(), String>;

    /// 创建（或整体替换）托盘。
    fn start_tray(&mut self, plan: &TrayPlan) -> Result<(), String>;

    /// 只替换托盘右键菜单。
    fn set_tray_menu(&mut self, menu: &[MenuItemSpec]) -> Result<(), String>;

    /// 更新托盘悬停提示。
    fn set_tooltip(&mut self, text: &str);

    /// 弹一条系统通知气泡。
    fn show_balloon(&mut self, balloon: Balloon);

    /// 移除托盘图标。
    fn stop_tray(&mut self);

    /// 设置弹出菜单的深浅色。
    fn set_popup_theme(&mut self, theme: TrayTheme);

    /// 注销全部旧热键并注册 `hotkeys`；返回与 `hotkeys` 一一对应的结果。
    fn register_hotkeys(&mut self, hotkeys: &[HotkeySpec]) -> Vec<Result<(), String>>;

    /// 前台是否是全屏窗口。
    fn fullscreen_focused(&mut self) -> bool;

    /// 修剪工作集（`EmptyWorkingSet`，只降工作集不降提交）。
    fn trim_working_set(&mut self);

    /// 拉起延迟启动的新宿主（重启用）。
    fn restart_host(&mut self) -> Result<(), String>;

    /// 按快照里的鼠标手势绑定装 / 换 / 卸全局鼠标钩子：空表卸载（平时系统里没有钩子），
    /// 非空表则在需要时启动钩子线程并替换绑定。钩子事件由平台投递 [`Event::Gesture`]。
    ///
    /// # 参数
    /// - `bindings`：快照里的绑定；无效项（未知名字、没有修饰键）由平台丢弃。
    fn set_mouse_bindings(&mut self, bindings: &[MouseBindingSpec]) -> Result<(), String>;

    /// 取消进行中的鼠标手势（UI 放弃了这次拖选，钩子别再吞鼠标输入）。
    fn cancel_gesture(&mut self);

    /// 拉起无窗口任务（`--headless update-check` / `set-config`）；结束后平台投递 [`Event::HeadlessDone`]。
    ///
    /// # 参数
    /// - `job`：任务；生成快照走 [`Platform::spawn_emit_spec`]，不经过这里。
    fn spawn_headless(&mut self, job: &HeadlessJob) -> Result<(), String>;

    /// 当前 Unix 秒（更新检查到点判断用）。
    fn unix_now(&mut self) -> u64;

    /// 弹“连续误触发”确认框（系统模态提示，默认选“否”）。**不得阻塞调用线程**：实现要把模态调用放到
    /// 单独的短命线程里，用户回答后由平台投递 [`Event::RapidCaptureAnswered`]。
    ///
    /// # 参数
    /// - `title`：标题（产品名）。
    /// - `text`：正文（已本地化、已填好次数）。
    ///
    /// # 返回
    /// 弹窗线程拉起失败返回错误（宿主按“刻意”放行，不能让提示状态卡死）。
    fn confirm_rapid_capture(&mut self, title: &str, text: &str) -> Result<(), String>;
}

/// 宿主拉起的无窗口任务种类（结束时按它投递 [`Event::HeadlessDone`]）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HeadlessKind {
    /// 到点的自动更新检查。
    UpdateCheck,
    /// 改一项配置。
    SetConfig,
}

impl HeadlessKind {
    /// 任务对应的种类；生成快照不走这条路径，返回 `None`。
    ///
    /// ```rust
    /// use snow_host_protocol::launch::HeadlessJob;
    /// use snow_tray_host::host::HeadlessKind;
    /// assert_eq!(HeadlessKind::of(&HeadlessJob::UpdateCheck), Some(HeadlessKind::UpdateCheck));
    /// assert_eq!(HeadlessKind::of(&HeadlessJob::EmitSpec), None);
    /// ```
    pub fn of(job: &HeadlessJob) -> Option<Self> {
        match job {
            HeadlessJob::UpdateCheck => Some(Self::UpdateCheck),
            HeadlessJob::SetConfig { .. } => Some(Self::SetConfig),
            HeadlessJob::EmitSpec => None,
        }
    }
}

/// 汇入主线程的事件。
#[derive(Debug, Clone, PartialEq)]
pub enum Event {
    /// 托盘 / 热键 / `--cmd` 触发了一个动作。
    Action {
        /// 动作 id。
        id: String,
        /// 触发来源。
        origin: ActionOrigin,
    },
    /// 会话管道：客户端连上了。
    SessionConnected {
        /// 会话代数。
        generation: u64,
        /// 客户端进程 ID（0 表示取不到）。
        client_pid: u32,
    },
    /// 会话管道：收到一行。
    SessionLine {
        /// 会话代数。
        generation: u64,
        /// 一行文本（JSON）。
        line: String,
    },
    /// 会话管道：连接结束。
    SessionClosed {
        /// 会话代数。
        generation: u64,
    },
    /// UI 进程退出了。
    UiExited {
        /// 会话代数。
        generation: u64,
    },
    /// `--headless emit-spec` 的 UI 结束了（快照应已生成）。
    SpecEmitted,
    /// 鼠标手势钩子产生了一个事件（已过节流）。
    Gesture(GestureEvent),
    /// 连续误触发确认框有结果了。
    RapidCaptureAnswered {
        /// 用户是否选了“否，退出”。
        quit: bool,
    },
    /// 无窗口任务（更新检查 / 改配置）结束了。
    HeadlessDone {
        /// 任务种类。
        kind: HeadlessKind,
        /// 更新检查发现新版本时的提示文案（UI 侧已本地化，来自标准输出第一行）；其余为 `None`。
        notice: Option<String>,
    },
}

/// 宿主核心。
pub struct HostCore<P: Platform> {
    /// 平台。
    platform: P,
    /// UI 进程状态机。
    machine: HostMachine,
    /// 当前生效的快照。
    spec: Option<HostSpec>,
    /// 当前生效的是不是内置兜底快照。
    fallback_active: bool,
    /// 当前显示的托盘（用来判断只换菜单还是整体重建）。
    tray_shown: Option<TrayPlan>,
    /// 当前已注册的热键表（用来判断要不要重新注册）。
    registered: Vec<HotkeySpec>,
    /// 最近一次热键注册的逐配置键结果。
    last_results: Vec<HotkeyResult>,
    /// 全局热键是否被暂停。
    paused: bool,
    /// 前台全屏闸门是否打开。
    gate_on: bool,
    /// 是否已经请求过生成快照（只请求一次，避免反复拉起）。
    emit_requested: bool,
    /// 下一次工作集修剪的时刻。
    trim_at: Option<Instant>,
    /// 宿主该退出了（`Some(restart)`）。
    exit: Option<bool>,
    /// 启动时刻（更新检查登录后延后用）；`start` 之前为 `None`。
    started_at: Option<Instant>,
    /// 当前装着的鼠标手势绑定（用来判断要不要重装钩子）。
    mouse_applied: Vec<MouseBindingSpec>,
    /// 快照里的更新检查到点时间里，下一个要检查的那个（`Unix` 秒）。
    update_due: Option<u64>,
    /// 下一次更新检查的时刻；`None` 表示没有待办。
    update_at: Option<Instant>,
    /// 已经按这个到点时间检查过（或拉起过）：同一个到点时间不重复检查，等 UI 写出新快照。
    update_done_for: Option<u64>,
    /// 更新检查的无窗口任务是否在跑。
    update_running: bool,
    /// 改配置的无窗口任务是否在跑；期间新消息暂存在 `held`，免得新 UI 和它同时写配置。
    config_job_running: bool,
    /// 等改配置任务结束后再投递的消息（保序）。
    held: VecDeque<Outbound>,
    /// 连续误触发保护（截图覆盖窗“关闭 → 开启”间隔判定）。
    rapid: RapidGuard,
    /// 弹确认框期间被扣下的命令：`(动作 id, 来源)`；鼠标手势 `begin` 过期，不扣。
    rapid_pending: Option<(String, ActionOrigin)>,
    /// 被保护丢弃的鼠标手势编号（它后续的 update / finish / cancel 也要丢，否则会把 UI 拉起来）。
    dropped_gesture: Option<u64>,
}

impl<P: Platform> HostCore<P> {
    /// 新建（尚未读快照）。
    pub fn new(platform: P) -> Self {
        Self {
            platform,
            machine: HostMachine::new(),
            spec: None,
            fallback_active: false,
            tray_shown: None,
            registered: Vec::new(),
            last_results: Vec::new(),
            paused: false,
            gate_on: false,
            emit_requested: false,
            trim_at: None,
            exit: None,
            started_at: None,
            mouse_applied: Vec::new(),
            update_due: None,
            update_at: None,
            update_done_for: None,
            update_running: false,
            config_job_running: false,
            held: VecDeque::new(),
            rapid: RapidGuard::new(),
            rapid_pending: None,
            dropped_gesture: None,
        }
    }

    /// 平台的只读引用（测试读取记录用）。
    pub fn platform(&self) -> &P {
        &self.platform
    }

    /// 状态机的只读引用（测试与日志用）。
    pub fn machine(&self) -> &HostMachine {
        &self.machine
    }

    /// 启动：读快照、显示托盘、注册热键、装鼠标手势钩子（有绑定才装）；
    /// 登录时有贴图要恢复就拉起 UI 恢复（贴图窗口让 UI 常驻）；要求常驻的功能（MCP、OCR 预热）开着也拉起 UI；
    /// 安排第一次工作集修剪与更新检查。
    ///
    /// # 参数
    /// - `now`：当前时刻。
    pub fn start(&mut self, now: Instant) {
        self.started_at = Some(now);
        self.load_spec(now, true);
        self.schedule_trim(now + TRIM_AFTER_START);
        // 要求常驻（MCP / OCR 预热）的拉起在读快照时已经提交（`apply_keep_alive`）
        let restore = self
            .spec
            .as_ref()
            .is_some_and(|spec| spec.flags.pins_to_restore > 0);
        if restore {
            tracing::info!("快照里有待恢复的贴图，拉起 UI");
            self.submit(now, Outbound::RestorePins);
        }
    }

    /// 宿主是否该退出：`Some(restart)`。
    pub fn exit_requested(&self) -> Option<bool> {
        self.exit
    }

    /// 退出前的收尾：需要重启就拉起延迟启动的新宿主（托盘与热键随核心被丢弃时一并释放）。
    pub fn finish(&mut self) {
        if self.exit == Some(true)
            && let Err(e) = self.platform.restart_host()
        {
            tracing::error!(error = %e, "拉起新宿主失败，重启取消");
        }
    }

    /// 处理一个事件。
    ///
    /// # 参数
    /// - `now`：当前时刻。
    /// - `event`：事件。
    pub fn handle(&mut self, now: Instant, event: Event) {
        match event {
            Event::Action { id, origin } => self.handle_action(now, id, origin),
            Event::SessionConnected {
                generation,
                client_pid,
            } => self.machine.on_pipe_connected(generation, client_pid),
            Event::SessionLine { generation, line } => match decode_line::<UiMessage>(&line) {
                Ok(message) => {
                    let effects = self.machine.on_ui_message(now, generation, message);
                    self.run_effects(now, effects);
                }
                Err(e) => tracing::warn!(error = %e, "会话里收到无法解析的消息，已忽略"),
            },
            Event::SessionClosed { generation } => {
                tracing::debug!(generation, "会话管道已断开");
            }
            Event::UiExited { generation } => {
                // 覆盖窗随 UI 进程消失：记为关闭（过期代数的退出不算）
                if generation == self.machine.generation() {
                    self.rapid.on_closed(now);
                }
                let effects = self.machine.on_ui_exited(now, generation);
                self.run_effects(now, effects);
            }
            Event::SpecEmitted => self.load_spec(now, true),
            Event::Gesture(gesture) => self.handle_gesture(now, gesture),
            Event::RapidCaptureAnswered { quit } => self.on_rapid_answer(now, quit),
            Event::HeadlessDone { kind, notice } => self.on_headless_done(now, kind, notice),
        }
    }

    /// 处理到点的事：状态机的各种超时、工作集修剪。
    ///
    /// # 参数
    /// - `now`：当前时刻。
    pub fn tick(&mut self, now: Instant) {
        let effects = self.machine.on_tick(now);
        self.run_effects(now, effects);
        if self.trim_at.is_some_and(|at| now >= at) {
            self.trim_at = None;
            self.platform.trim_working_set();
        }
        if self.update_at.is_some_and(|at| now >= at) {
            self.update_at = None;
            self.start_update_check();
        }
    }

    /// 下一个需要调用 [`tick`](Self::tick) 的时刻；没有待办返回 `None`（主线程可以无限期等事件）。
    pub fn next_deadline(&self) -> Option<Instant> {
        [self.machine.next_deadline(), self.trim_at, self.update_at]
            .into_iter()
            .flatten()
            .min()
    }

    /// 处理一个动作：宿主自己的（退出、重启、两个开关）直接做，其余转给 UI。
    fn handle_action(&mut self, now: Instant, id: String, origin: ActionOrigin) {
        match id.as_str() {
            ACTION_QUIT => return self.quit(now, false),
            ACTION_RESTART => return self.quit(now, true),
            _ => {}
        }
        let is_toggle = id == ACTION_TOGGLE_GLOBAL_HOTKEYS || id == ACTION_TOGGLE_FULLSCREEN_GATE;
        // 前台全屏闸门：只拦热键（托盘、命令行不受影响），两个开关本身除外，否则关不回去
        if origin == ActionOrigin::Hotkey
            && self.gate_on
            && !is_toggle
            && self.platform.fullscreen_focused()
        {
            tracing::info!(%id, "前台为全屏窗口，已忽略全局热键");
            return;
        }
        match id.as_str() {
            ACTION_TOGGLE_GLOBAL_HOTKEYS => self.toggle_pause(),
            ACTION_TOGGLE_FULLSCREEN_GATE => self.toggle_gate(now),
            _ => self.dispatch_action(now, id, origin),
        }
    }

    /// 转发一个动作给 UI；会打开截图覆盖窗的动作先过连续误触发保护。
    fn dispatch_action(&mut self, now: Instant, id: String, origin: ActionOrigin) {
        if opens_capture_overlay(&id) {
            match self.rapid.on_open(now) {
                RapidVerdict::Allow => {}
                RapidVerdict::Drop => {
                    tracing::debug!(%id, ?origin, "连续误触发确认框开着，截图类命令已丢弃");
                    return;
                }
                RapidVerdict::Ask { count } => {
                    return self.ask_rapid(now, count, Some((id, origin)));
                }
            }
        }
        self.submit(
            now,
            Outbound::Action {
                id,
                origin: Some(origin),
            },
        );
    }

    /// 处理一个鼠标手势事件：`begin` 过连续误触发保护；被丢弃的手势后续事件一并丢弃。
    fn handle_gesture(&mut self, now: Instant, gesture: GestureEvent) {
        if gesture.phase == GesturePhase::Begin {
            self.dropped_gesture = None;
            match self.rapid.on_open(now) {
                RapidVerdict::Allow => {}
                RapidVerdict::Drop => {
                    tracing::debug!(id = gesture.id, "连续误触发确认框开着，鼠标手势已丢弃");
                    self.drop_gesture(gesture.id);
                    return;
                }
                RapidVerdict::Ask { count } => {
                    self.drop_gesture(gesture.id);
                    return self.ask_rapid(now, count, None);
                }
            }
        } else if self.dropped_gesture == Some(gesture.id) {
            if matches!(gesture.phase, GesturePhase::Finish | GesturePhase::Cancel) {
                self.dropped_gesture = None;
            }
            return;
        }
        self.submit(now, gesture.into_outbound());
    }

    /// 丢弃一次鼠标手势：让钩子别再吞鼠标输入，并记下编号以便丢掉它后续的事件。
    fn drop_gesture(&mut self, id: u64) {
        self.dropped_gesture = Some(id);
        self.platform.cancel_gesture();
    }

    /// 判定为连续误触发：本次命令不分发，弹确认框问是不是刻意的。
    ///
    /// # 参数
    /// - `count`：连续“太快”的次数。
    /// - `pending`：被扣下的命令（动作 id 与来源）；鼠标手势 `begin` 为 `None`。
    fn ask_rapid(&mut self, now: Instant, count: u32, pending: Option<(String, ActionOrigin)>) {
        tracing::warn!(
            count,
            window_secs = RAPID_REOPEN_GAP.as_secs(),
            origin = ?pending.as_ref().map(|(_, origin)| *origin),
            "短时间内连续启动截图，已拦下本次命令并询问是否刻意"
        );
        let localized = self
            .spec
            .as_ref()
            .map(|spec| spec.strings.notice_rapid_capture.as_str());
        let text = rapid_capture_text(localized, count);
        self.rapid_pending = pending;
        if let Err(e) = self.platform.confirm_rapid_capture(PRODUCT_NAME, &text) {
            tracing::warn!(error = %e, "确认框拉不起来，按刻意操作放行");
            self.on_rapid_answer(now, false);
        }
    }

    /// 确认框有答案了：选“否”走正常退出路径（UI 一并退出）；选“是”放行被扣下的命令并进入冷却。
    fn on_rapid_answer(&mut self, now: Instant, quit: bool) {
        let pending = self.rapid_pending.take();
        if quit {
            tracing::warn!("用户确认并非刻意连续截图，按回车选择退出，宿主即将退出");
            self.rapid.answer_quit();
            self.quit(now, false);
            return;
        }
        tracing::info!("用户确认是刻意的连续截图，放行并进入冷却");
        self.rapid.answer_intentional(now, pending.is_some());
        if let Some((id, origin)) = pending {
            self.submit(
                now,
                Outbound::Action {
                    id,
                    origin: Some(origin),
                },
            );
        }
    }

    /// 退出（或重启）：UI 在就先让它优雅退出。
    fn quit(&mut self, now: Instant, restart: bool) {
        let effects = self.machine.request_quit(now, restart);
        self.run_effects(now, effects);
    }

    /// 提交一条要转给 UI 的消息。
    ///
    /// - 改配置的无窗口任务在跑时先暂存（保序），免得新拉起的 UI 与它同时写配置；
    /// - 改配置且 UI 不在时，不拉起 UI 会话，改走 `--headless set-config`（UI 仍是配置唯一写者）；
    /// - 手势 `begin` 被状态机丢弃（熔断退避）时让钩子取消手势，别一直吞着鼠标输入。
    fn submit(&mut self, now: Instant, message: Outbound) {
        if self.config_job_running {
            self.held.push_back(message);
            return;
        }
        if let Outbound::SetConfig { key, value } = &message
            && !self.machine.ui_alive()
            && !self.machine.is_quitting()
        {
            let job = HeadlessJob::SetConfig {
                key: key.clone(),
                value: value.clone(),
            };
            match self.platform.spawn_headless(&job) {
                Ok(()) => {
                    tracing::info!(%key, "UI 不在，改用无窗口任务写配置");
                    self.config_job_running = true;
                    return;
                }
                Err(e) => {
                    tracing::warn!(error = %e, %key, "拉起写配置的无窗口任务失败，退回会话路径");
                }
            }
        }
        let is_begin = matches!(
            &message,
            Outbound::Gesture {
                phase: GesturePhase::Begin,
                ..
            }
        );
        let effects = self.machine.submit(now, message);
        if is_begin
            && effects
                .iter()
                .any(|e| matches!(e, Effect::Notice(Notice::UiCrashLoop)))
        {
            // 命令被丢弃了：别让钩子继续吞着这次拖动
            self.platform.cancel_gesture();
        }
        self.run_effects(now, effects);
    }

    /// 无窗口任务结束了。
    fn on_headless_done(&mut self, now: Instant, kind: HeadlessKind, notice: Option<String>) {
        match kind {
            HeadlessKind::UpdateCheck => {
                self.update_running = false;
                // 新快照里的到点时间已经往后推了
                self.load_spec(now, true);
                if let Some(text) = notice.filter(|text| !text.is_empty()) {
                    self.show_text(&text);
                }
            }
            HeadlessKind::SetConfig => {
                self.config_job_running = false;
                self.load_spec(now, true);
                // 暂存的消息按序投递；其中又有改配置且 UI 仍不在的，会再起一个任务并把后面的继续暂存
                let pending = std::mem::take(&mut self.held);
                for message in pending {
                    self.submit(now, message);
                }
            }
        }
    }

    /// 到点的更新检查：拉起 `--headless update-check`；同一个到点时间只拉起一次。
    fn start_update_check(&mut self) {
        if self.update_running || self.machine.is_quitting() {
            return;
        }
        self.update_done_for = self.update_due;
        match self.platform.spawn_headless(&HeadlessJob::UpdateCheck) {
            Ok(()) => {
                tracing::info!("到点的更新检查已交给无窗口任务");
                self.update_running = true;
            }
            Err(e) => {
                tracing::warn!(error = %e, "拉起更新检查的无窗口任务失败，等下次快照更新再试")
            }
        }
    }

    /// 按快照里的到点时间安排更新检查：登录后至少再等 [`UPDATE_CHECK_START_DELAY`]；
    /// 同一个到点时间已经检查过就不再安排（等 UI 写出新快照）。
    fn schedule_update_check(&mut self, now: Instant, due: Option<u64>) {
        self.update_due = due;
        let Some(due) = due else {
            self.update_at = None;
            return;
        };
        if self.update_done_for == Some(due) {
            self.update_at = None;
            return;
        }
        let unix = self.platform.unix_now();
        let wait = Duration::from_secs(due.saturating_sub(unix));
        let earliest = self
            .started_at
            .map_or(now, |started| started + UPDATE_CHECK_START_DELAY);
        self.update_at = Some((now + wait).max(earliest));
    }

    /// 按快照装 / 换 / 卸鼠标手势钩子（绑定没变就不动）。
    fn apply_mouse(&mut self, bindings: &[MouseBindingSpec]) {
        if self.mouse_applied == bindings {
            return;
        }
        // 先记下再装：装失败也不反复重试，等绑定再变
        self.mouse_applied = bindings.to_vec();
        match self.platform.set_mouse_bindings(bindings) {
            Ok(()) => tracing::info!(bindings = bindings.len(), "鼠标手势绑定已应用"),
            Err(e) => tracing::warn!(error = %e, "安装鼠标手势钩子失败，手势不可用"),
        }
    }

    /// 在托盘悬停提示里放一条文案：`产品名: 文案`。
    fn show_text(&mut self, text: &str) {
        let base = self
            .spec
            .as_ref()
            .map_or(crate::view::FALLBACK_TOOLTIP, |spec| {
                spec.tray.tooltip.as_str()
            })
            .to_string();
        tracing::info!(%text, "托盘提示");
        self.platform.set_tooltip(&compose_tooltip(&base, text));
    }

    /// 执行状态机给出的动作；拉起结果会产生新的动作，所以循环到没有为止。
    fn run_effects(&mut self, now: Instant, effects: Vec<Effect>) {
        let mut pending = std::collections::VecDeque::from(effects);
        while let Some(effect) = pending.pop_front() {
            match effect {
                Effect::SpawnUi { generation } => {
                    let hint = self.machine.spawn_hint();
                    let follow_up = match self.platform.spawn_ui(generation, hint) {
                        Ok(spawned) => {
                            self.machine
                                .on_spawned(generation, spawned.pid, spawned.token)
                        }
                        Err(reason) => self.machine.on_spawn_failed(now, generation, &reason),
                    };
                    pending.extend(follow_up);
                }
                Effect::Send {
                    generation,
                    message,
                } => {
                    if let Err(e) = self.platform.send(generation, &message) {
                        tracing::warn!(error = %e, ?message, "写会话管道失败");
                    }
                }
                Effect::CloseSession { generation } => self.platform.close_session(generation),
                Effect::GrantForeground { pid } => self.platform.grant_foreground(pid),
                Effect::KillUi { pid } => self.platform.kill_ui(pid),
                Effect::Notice(notice) => self.show_notice(notice),
                Effect::FromUi(message) => self.handle_ui_message(now, message),
                Effect::GaveUp { seq } => tracing::warn!(seq, "命令重发后仍未确认，已放弃"),
                Effect::UiGone => self.schedule_trim(now + TRIM_AFTER_UI_EXIT),
                Effect::ExitHost { restart } => self.exit = Some(restart),
            }
        }
    }

    /// 处理 UI 发来的信息类消息。
    fn handle_ui_message(&mut self, now: Instant, message: UiMessage) {
        match message {
            UiMessage::Tooltip { text } => self.platform.set_tooltip(&text),
            UiMessage::Balloon {
                title,
                text,
                icon,
                open,
            } => {
                tracing::info!(%title, %text, has_icon = icon.is_some(), ?open, "收到 UI 的通知气泡请求");
                self.platform.show_balloon(Balloon {
                    title,
                    text,
                    icon: icon.map(|icon| BalloonIcon {
                        width: icon.width,
                        height: icon.height,
                        rgba: icon.rgba,
                    }),
                    open: open.map(PathBuf::from),
                })
            }
            UiMessage::SpecChanged { .. } => self.load_spec(now, true),
            UiMessage::HotkeysApply { rev } => self.apply_hotkeys_request(now, rev),
            UiMessage::CancelGesture { id } => {
                tracing::debug!(id, "UI 放弃了这次鼠标手势，取消钩子里的手势");
                self.platform.cancel_gesture();
            }
            UiMessage::CaptureIdle => self.rapid.on_closed(now),
            UiMessage::Activity {
                windows,
                pins,
                busy_reasons,
            } => tracing::debug!(windows, pins, ?busy_reasons, "UI 活动状态"),
            // 握手、确认、退出请求由状态机消化，不会走到这里
            UiMessage::Hello { .. } | UiMessage::Ack { .. } | UiMessage::ExitRequest { .. } => {}
        }
    }

    /// 读快照并应用：缺失 / 损坏时退回内置最小菜单并去生成快照；修订号没变就跳过。
    ///
    /// `register_hotkeys` 为假时只更新托盘与开关状态，热键由调用方随后自己注册
    /// （设置页改热键的流程要拿注册结果，不能在这里先注册一遍）。
    fn load_spec(&mut self, now: Instant, register_hotkeys: bool) {
        match self.platform.read_spec() {
            Ok(spec) => {
                let unchanged = !self.fallback_active
                    && self
                        .spec
                        .as_ref()
                        .is_some_and(|current| current.rev == spec.rev);
                if unchanged {
                    tracing::debug!(rev = spec.rev, "快照修订号没变，跳过");
                    return;
                }
                self.fallback_active = false;
                self.apply_spec(now, spec, register_hotkeys);
            }
            Err(error) => {
                tracing::warn!(%error, "读取快照失败");
                if self.spec.is_none() {
                    self.fallback_active = true;
                    self.apply_spec(now, fallback_spec(), register_hotkeys);
                }
                self.request_emit_spec();
            }
        }
    }

    /// 请求生成快照（只请求一次）。
    fn request_emit_spec(&mut self) {
        if self.emit_requested {
            return;
        }
        self.emit_requested = true;
        if let Err(e) = self.platform.spawn_emit_spec() {
            tracing::error!(error = %e, "拉起快照生成任务失败，托盘将保持内置最小菜单");
        }
    }

    /// 应用一份快照：闸门状态、托盘、热键、鼠标手势钩子、UI 常驻要求、更新检查的到点时间。
    fn apply_spec(&mut self, now: Instant, spec: HostSpec, register_hotkeys: bool) {
        if !self.fallback_active {
            self.gate_on = spec.gate.fullscreen_disable;
        }
        self.platform.set_popup_theme(spec.tray.theme);
        self.apply_tray(&spec);
        if register_hotkeys && (!spec.hotkeys.is_empty() || !self.registered.is_empty()) {
            let wanted = effective_hotkeys(&spec.hotkeys, self.paused);
            if wanted != self.registered {
                self.register(&wanted);
            }
        }
        if !self.fallback_active {
            self.apply_mouse(&spec.mouse);
            self.apply_keep_alive(now, &spec);
            self.schedule_update_check(now, spec.flags.update_due_at);
        }
        self.spec = Some(spec);
    }

    /// 同步 UI 常驻要求：状态机据此在崩溃 / 退避结束后补拉；启动完成后才出现的要求
    /// （快照生成任务刚跑完、读到真快照）当场拉起一个 UI。
    fn apply_keep_alive(&mut self, now: Instant, spec: &HostSpec) {
        let resident = spec.flags.needs_resident_ui();
        let newly = resident && !self.machine.keep_alive();
        self.machine.set_keep_alive(resident);
        if newly && self.started_at.is_some() && !self.machine.ui_alive() {
            tracing::info!("快照要求 UI 常驻，拉起 UI");
            self.submit(now, Outbound::keep_alive());
        }
    }

    /// 按快照与宿主状态显示托盘：只有菜单 / 提示变了就原地换，图标或点击动作变了才整体重建。
    fn apply_tray(&mut self, spec: &HostSpec) {
        if !spec.tray.enabled {
            if self.tray_shown.take().is_some() {
                self.platform.stop_tray();
            }
            return;
        }
        let plan = TrayPlan {
            icon: spec.tray.icon_rgba.clone(),
            tooltip: self.base_tooltip(spec),
            menu: effective_menu(&spec.tray.menu, self.paused, self.gate_on),
            on_left: spec.tray.on_left.clone(),
            on_middle: spec.tray.on_middle.clone(),
            on_double: spec.tray.on_double.clone(),
        };
        match &self.tray_shown {
            Some(shown) if same_shell(shown, &plan) => {
                if shown.menu != plan.menu
                    && let Err(e) = self.platform.set_tray_menu(&plan.menu)
                {
                    tracing::warn!(error = %e, "更新托盘菜单失败");
                }
                if shown.tooltip != plan.tooltip {
                    self.platform.set_tooltip(&plan.tooltip);
                }
            }
            _ => {
                if let Err(e) = self.platform.start_tray(&plan) {
                    tracing::error!(error = %e, "创建托盘失败，将无托盘图标运行");
                    self.tray_shown = None;
                    return;
                }
            }
        }
        self.tray_shown = Some(plan);
    }

    /// 当前常态悬停提示：暂停时用“已暂停”提示。
    fn base_tooltip(&self, spec: &HostSpec) -> String {
        if self.paused {
            spec.tray.paused_tooltip.clone()
        } else {
            spec.tray.tooltip.clone()
        }
    }

    /// 注册一份热键表并记下结果。
    fn register(&mut self, wanted: &[HotkeySpec]) -> Vec<HotkeyResult> {
        let outcomes = self.platform.register_hotkeys(wanted);
        let results = aggregate_results(wanted, &outcomes);
        self.registered = wanted.to_vec();
        self.last_results = results.clone();
        let failed = results.iter().filter(|r| !r.ok).count();
        tracing::info!(registered = wanted.len(), failed, "全局热键已注册");
        results
    }

    /// UI 请求立即按最新快照重注册热键（设置页改热键），并回报每个配置键的成败。
    fn apply_hotkeys_request(&mut self, now: Instant, rev: u64) {
        self.load_spec(now, false);
        let hotkeys = self
            .spec
            .as_ref()
            .map(|spec| effective_hotkeys(&spec.hotkeys, self.paused))
            .unwrap_or_default();
        let results = self.register(&hotkeys);
        let reply = HostMessage::HotkeysResult { rev, results };
        if let Err(e) = self.platform.send(self.machine.generation(), &reply) {
            tracing::warn!(error = %e, "回报热键注册结果失败");
        }
    }

    /// 暂停 / 恢复全部全局热键：暂停时只保留开关本身，菜单勾选与悬停提示随之变化。
    fn toggle_pause(&mut self) {
        self.paused = !self.paused;
        let Some(spec) = self.spec.clone() else {
            return;
        };
        let wanted = effective_hotkeys(&spec.hotkeys, self.paused);
        let results = self.register(&wanted);
        self.apply_tray(&spec);
        let failures = describe_failures(&results);
        tracing::info!(paused = self.paused, "全局热键暂停状态已切换");
        if self.paused {
            // 暂停状态要常驻可见：悬停提示固定为“热键已暂停”
            self.platform.set_tooltip(&spec.tray.paused_tooltip);
            return;
        }
        let text = if failures.is_empty() {
            spec.strings.notice_resumed.clone()
        } else {
            fill_failures(&spec.strings.notice_resume_failed, &failures)
        };
        self.platform
            .set_tooltip(&compose_tooltip(&spec.tray.tooltip, &text));
    }

    /// 切换“前台全屏窗口时停用热键”：宿主自己改勾选，并让 UI 把新状态落盘（UI 是配置唯一写者）。
    fn toggle_gate(&mut self, now: Instant) {
        self.gate_on = !self.gate_on;
        if let Some(spec) = self.spec.clone() {
            self.apply_tray(&spec);
            let text = if self.gate_on {
                &spec.strings.notice_gate_on
            } else {
                &spec.strings.notice_gate_off
            };
            self.platform
                .set_tooltip(&compose_tooltip(&spec.tray.tooltip, text));
        }
        let (key, value) = gate_config_change(self.gate_on);
        self.submit(
            now,
            Outbound::SetConfig {
                key: key.to_string(),
                value,
            },
        );
    }

    /// 显示一条状态提示：用快照里的本地化文案，没有就用内置兜底。
    fn show_notice(&mut self, notice: Notice) {
        let (localized, fallback) = match notice {
            Notice::UiStartFailed => (
                self.spec
                    .as_ref()
                    .map(|spec| spec.strings.notice_ui_start_failed.as_str()),
                FALLBACK_NOTICE_UI_START_FAILED,
            ),
            Notice::UiCrashLoop => (
                self.spec
                    .as_ref()
                    .map(|spec| spec.strings.notice_ui_crash_loop.as_str()),
                FALLBACK_NOTICE_UI_CRASH_LOOP,
            ),
        };
        let text = localized
            .filter(|text| !text.is_empty())
            .unwrap_or(fallback);
        let base = self
            .spec
            .as_ref()
            .map_or(crate::view::FALLBACK_TOOLTIP, |spec| {
                spec.tray.tooltip.as_str()
            });
        tracing::warn!(?notice, %text, "托盘提示");
        self.platform.set_tooltip(&compose_tooltip(base, text));
    }

    /// 安排一次工作集修剪（已有更早的安排就不改）。
    fn schedule_trim(&mut self, at: Instant) {
        self.trim_at = Some(self.trim_at.map_or(at, |existing| existing.min(at)));
    }
}

/// 两份托盘计划的“外壳”是否相同（图标与点击动作一致，只有菜单 / 提示可能不同）。
fn same_shell(a: &TrayPlan, b: &TrayPlan) -> bool {
    a.icon == b.icon
        && a.on_left == b.on_left
        && a.on_middle == b.on_middle
        && a.on_double == b.on_double
}

#[cfg(test)]
mod tests;
