//! 语音转文字宿主：把引擎、键入、右下角浮窗、托盘提示串成一条流程，生命周期随主线程事件循环。
//!
//! 数据流：热键 / 总线命令 → [`DictationHost::command`]；工作进程读线程与定时器经收件箱唤醒
//! [`DictationHost::tick`]；焦点探测在后台线程完成后经收件箱回到 [`DictationHost::probed`]。
//! 所有 GPUI 对象只在主线程触碰。

use super::DictationCommand;
use super::badge::{BADGE_WINDOW_ALPHA, ListeningBadge, badge_rect};
use super::client::{ProcessSttLink, locate_stt_exe};
use super::config::{DictationConfig, prepare_launch};
use super::engine::{Effect, Engine, Launch};
use super::focus::{FocusProbe, SystemProbe, Verdict, classify};
use super::output::{OutputMode, OutputPlan, RouteNote, decide};
use super::status::{Failure, Status};
use super::text::Transcript;
use super::translate::{
    HostTranslator, ModelSupport, OnTranslated, TranslationOutcome, TranslationState,
    TranslationTracker,
};
use super::typing::{KeySink, SyncOutcome, SystemKeySink, Typer};
use super::view::{DictationView, WINDOW_HEIGHT, WINDOW_WIDTH, bottom_right_rect};
use crate::app_runtime::{TRAY_TOOLTIP, UiEvent, ui_prefs_from_config};
use crate::capture_flow::pick_monitor;
use crate::settings_state::SharedConfig;
use crate::sys_prefs::system_ui_language;
use crate::translate_service::{TranslateConfig, TranslateHost};
use snow_ui::shell::inbox::MainThreadInbox;
use snow_ui::shell::overlay::cursor_screen_position;
use snow_ui::shell::tray::TrayService;
use snow_ui::shell::window::{Placement, WindowSpec};
use snow_ui::ui::{Context, Entity, ShellContext, ShellWindow};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

/// 等待焦点探测结果的上限；超时按“不确定”处理，走浮窗。
const PROBE_TIMEOUT: Duration = Duration::from_millis(2500);
/// 识别结束后，给“还没键进去的尾巴”的最长补发时间；超时把文字留在浮窗。
const TYPE_FLUSH_TIMEOUT: Duration = Duration::from_secs(3);
/// 定时器周期：驱动超时检查与键入重试。
const TICK_INTERVAL: Duration = Duration::from_millis(150);

/// 语音转文字宿主。
pub struct DictationHost {
    /// 工作进程会话引擎。
    engine: Engine,
    /// 本轮文本累积。
    transcript: Transcript,
    /// 键入状态。
    typer: Typer,
    /// 键入通道。
    sink: Box<dyn KeySink>,
    /// 本轮输出方案（自动模式下，焦点判定返回前为占位）。
    plan: OutputPlan,
    /// 本轮输出方式。
    mode: OutputMode,
    /// 键入时是否同时显示浮窗。
    with_overlay: bool,
    /// 当前状态。
    status: Status,
    /// 轮次编号，用来丢弃过期的焦点探测结果。
    round: u64,
    /// 等待焦点判定的截止时间。
    probe_deadline: Option<Instant>,
    /// 结束后补发键入的截止时间。
    flush_deadline: Option<Instant>,
    /// 浮窗（若已打开）。
    overlay: Option<(ShellWindow, Entity<DictationView>)>,
    /// 聆听指示（主屏右下角的耳朵图标，仅“只键入、无浮窗”时显示）。
    badge: Option<ShellWindow>,
    /// 浮窗是否曾经成功打开过（用来识别“被用户关掉”）。
    overlay_seen_open: bool,
    /// 本轮浮窗被用户关掉后不再自动重开。
    dismissed: bool,
    /// 定时器存活标志。
    ticker: Option<Arc<AtomicBool>>,
    /// 共享配置。
    config: SharedConfig,
    /// 主线程收件箱。
    inbox: MainThreadInbox<UiEvent>,
    /// 应用数据根目录（默认模型目录所在）。
    data_root: PathBuf,
    /// 应用共享的翻译宿主（语音翻译复用其引擎与结果缓存）。
    translator: Arc<TranslateHost>,
    /// 本轮按句译文跟踪（翻译线程、句序号、轮次过滤）。
    translation: TranslationTracker,
}

impl DictationHost {
    /// 创建空闲宿主。
    ///
    /// # 参数
    /// - `config`：共享配置。
    /// - `inbox`：主线程收件箱。
    /// - `data_root`：应用数据根目录。
    /// - `translator`：应用共享的翻译宿主。
    pub fn new(
        config: SharedConfig,
        inbox: MainThreadInbox<UiEvent>,
        data_root: PathBuf,
        translator: Arc<TranslateHost>,
    ) -> Self {
        Self {
            engine: Engine::default(),
            transcript: Transcript::default(),
            typer: Typer::default(),
            sink: Box::new(SystemKeySink),
            plan: OutputPlan::pending(),
            mode: OutputMode::Auto,
            with_overlay: false,
            status: Status::Done,
            round: 0,
            probe_deadline: None,
            flush_deadline: None,
            overlay: None,
            badge: None,
            overlay_seen_open: false,
            dismissed: false,
            ticker: None,
            config,
            inbox,
            data_root,
            translator,
            translation: TranslationTracker::default(),
        }
    }

    /// 处理热键 / 总线命令。
    ///
    /// # 参数
    /// - `cx`：外壳上下文。
    /// - `tray`：托盘服务（用于状态提示）。
    /// - `command`：切换 / 开始 / 结束。
    pub fn command(
        &mut self,
        cx: &mut ShellContext,
        tray: Option<&TrayService>,
        command: DictationCommand,
    ) {
        let now = Instant::now();
        let active = self.engine.active();
        match command {
            DictationCommand::Toggle if active => self.stop(cx, tray, now),
            DictationCommand::Stop if active => self.stop(cx, tray, now),
            DictationCommand::Toggle | DictationCommand::Start if !active => {
                self.begin(cx, tray, now)
            }
            _ => {}
        }
    }

    /// 定时 / 读线程唤醒：取工作进程事件、检查各类超时、重试键入。
    ///
    /// # 参数
    /// - `cx`：外壳上下文。
    /// - `tray`：托盘服务。
    pub fn tick(&mut self, cx: &mut ShellContext, tray: Option<&TrayService>) {
        let now = Instant::now();
        let effects = self.engine.poll(now);
        self.apply(cx, tray, effects, now);

        if self.probe_deadline.is_some_and(|d| now >= d) && self.plan_pending() {
            self.decide_route(cx, tray, None);
        }
        if self.plan.typing && self.typer.typed() != self.transcript.full() {
            self.type_sync(cx, tray, !self.engine.listening());
        }
        if self.flush_deadline.is_some_and(|d| now >= d) {
            self.flush_deadline = None;
            if self.plan.typing && self.typer.typed() != self.transcript.full() {
                self.fall_back(cx, tray, RouteNote::TypingStuck);
            }
        }
        self.watch_overlay_dismissed(cx, tray, now);
        self.sync_badge(cx);
        if !self.engine.active() && self.flush_deadline.is_none() && self.probe_deadline.is_none() {
            self.stop_ticker();
        }
    }

    /// 焦点探测完成（后台线程投递回主线程）。
    ///
    /// # 参数
    /// - `cx`：外壳上下文。
    /// - `tray`：托盘服务。
    /// - `round`：探测所属轮次；过期的结果被丢弃。
    /// - `verdict`：能否键入的判定。
    pub fn probed(
        &mut self,
        cx: &mut ShellContext,
        tray: Option<&TrayService>,
        round: u64,
        verdict: Verdict,
    ) {
        if round == self.round && self.plan_pending() {
            self.decide_route(cx, tray, Some(verdict));
        }
    }

    /// 翻译线程回传了一句的译文（经收件箱回到主线程）。
    ///
    /// # 参数
    /// - `cx`：外壳上下文。
    /// - `round`：结果所属轮次；过期的结果被丢弃。
    /// - `seq`：句序号，译文按它对位（乱序到达也不会错位）。
    /// - `outcome`：译文或失败。
    pub fn translated(
        &mut self,
        cx: &mut ShellContext,
        round: u64,
        seq: usize,
        outcome: TranslationOutcome,
    ) {
        if let Some((seq, state)) = self.translation.on_result(round, seq, outcome) {
            self.with_view(cx, |v, vcx| v.set_translation(seq, state, vcx));
        }
    }

    /// 应用退出：尽力结束工作进程（不按名查杀，只动自己拉起的那个）。
    pub fn shutdown(&mut self) {
        self.translation.shutdown();
        self.stop_ticker();
        self.engine.shutdown();
    }

    /// 方案是否还在等焦点判定。
    fn plan_pending(&self) -> bool {
        matches!(self.plan.note, RouteNote::Pending)
    }

    /// 开始新一轮。
    fn begin(&mut self, cx: &mut ShellContext, tray: Option<&TrayService>, now: Instant) {
        let config = DictationConfig::from_document(self.config.borrow().document());
        self.round += 1;
        self.transcript.clear();
        self.typer = Typer::default();
        self.mode = config.output;
        self.with_overlay = config.type_with_overlay;
        self.probe_deadline = None;
        self.flush_deadline = None;
        self.dismissed = false;
        self.plan = if self.mode == OutputMode::Overlay {
            OutputPlan::overlay_only()
        } else {
            OutputPlan::pending()
        };
        self.begin_translation(&config);
        let launch = self.make_launch(&config);
        let launched = launch.is_ok();
        let effects = self.engine.start(now, launch);
        if launched {
            if self.mode != OutputMode::Overlay {
                self.spawn_probe();
                self.probe_deadline = Some(now + PROBE_TIMEOUT);
            }
            self.start_ticker();
        }
        self.apply(cx, tray, effects, now);
    }

    /// 每轮开始时算一次翻译可用性并装配本轮翻译线程；不可用时只留原因，供状态提示。
    fn begin_translation(&mut self, config: &DictationConfig) {
        let tcfg =
            TranslateConfig::from_document(self.config.borrow().document(), &system_ui_language());
        let support = if config.translate_enabled {
            ModelSupport::from_config(&tcfg, || self.translator.scan(&tcfg))
        } else {
            ModelSupport::Unavailable
        };
        let inbox = self.inbox.clone();
        let on_done: OnTranslated = Arc::new(move |round, seq, outcome| {
            inbox.push(UiEvent::DictationTranslated {
                round,
                seq,
                outcome,
            });
        });
        let translator = Arc::new(HostTranslator::new(Arc::clone(&self.translator), tcfg));
        self.translation
            .begin(self.round, config, support, translator, on_done);
    }

    /// 本轮听写状态：翻译不可用时在去向说明后带上原因。
    fn listening_status(&self) -> Status {
        let note = self.plan.note.clone();
        match self.translation.issue() {
            Some(issue) => Status::ListeningNotice(note, issue.clone()),
            None => Status::Listening(note),
        }
    }

    /// 请求结束当前一轮。
    fn stop(&mut self, cx: &mut ShellContext, tray: Option<&TrayService>, now: Instant) {
        let effects = self.engine.stop(now);
        self.apply(cx, tray, effects, now);
    }

    /// 检查并拉起工作进程。
    fn make_launch(&self, config: &DictationConfig) -> Result<Launch, Failure> {
        let (exe, request) =
            prepare_launch(config, &self.data_root, locate_stt_exe(), |p| p.is_dir())?;
        let inbox = self.inbox.clone();
        let wake: Arc<dyn Fn() + Send + Sync> = Arc::new(move || {
            inbox.push(UiEvent::DictationPoll);
        });
        let link = ProcessSttLink::spawn(&exe, wake).map_err(Failure::Spawn)?;
        Ok(Launch {
            link: Box::new(link),
            request,
        })
    }

    /// 在后台线程探测前台焦点（UIA 可能被无响应的目标程序拖住，所以不在主线程做）。
    fn spawn_probe(&self) {
        let inbox = self.inbox.clone();
        let round = self.round;
        let spawned = std::thread::Builder::new()
            .name("snow-dictation-probe".into())
            .spawn(move || {
                let reading = SystemProbe.read();
                let verdict = classify(&reading);
                tracing::info!(?verdict, ?reading, "语音转文字焦点探测");
                inbox.push(UiEvent::DictationProbed { round, verdict });
            });
        if let Err(e) = spawned {
            tracing::error!(error = %e, "无法创建焦点探测线程，本轮按不确定处理");
            self.inbox.push(UiEvent::DictationProbed {
                round: self.round,
                verdict: Verdict::NoType(super::focus::NoTypeReason::Uncertain),
            });
        }
    }

    /// 启动定时器线程：周期性唤醒 [`DictationHost::tick`]，标志位清零或收件箱关闭后退出。
    fn start_ticker(&mut self) {
        self.stop_ticker();
        let alive = Arc::new(AtomicBool::new(true));
        let flag = Arc::clone(&alive);
        let inbox = self.inbox.clone();
        let spawned = std::thread::Builder::new()
            .name("snow-dictation-tick".into())
            .spawn(move || {
                while flag.load(Ordering::Relaxed) {
                    std::thread::sleep(TICK_INTERVAL);
                    if !flag.load(Ordering::Relaxed) || !inbox.push(UiEvent::DictationPoll) {
                        break;
                    }
                }
            });
        match spawned {
            Ok(_) => self.ticker = Some(alive),
            Err(e) => {
                tracing::error!(error = %e, "无法创建语音转文字定时器，超时检查将只靠进程事件触发")
            }
        }
    }

    /// 停止定时器线程。
    fn stop_ticker(&mut self) {
        if let Some(alive) = self.ticker.take() {
            alive.store(false, Ordering::Relaxed);
        }
    }

    /// 落实引擎效果。
    fn apply(
        &mut self,
        cx: &mut ShellContext,
        tray: Option<&TrayService>,
        effects: Vec<Effect>,
        now: Instant,
    ) {
        for effect in effects {
            match effect {
                Effect::Begin => self.on_begin(cx),
                Effect::Loading => self.set_status(cx, tray, Status::Loading),
                Effect::Listening => {
                    let status = self.listening_status();
                    self.set_status(cx, tray, status);
                }
                Effect::Partial(text) => {
                    self.transcript.set_partial(&text);
                    self.with_view(cx, |v, vcx| v.push_partial(&text, vcx));
                    self.type_sync(cx, tray, false);
                }
                Effect::Final(text) => {
                    self.transcript.push_final(&text);
                    self.with_view(cx, |v, vcx| v.push_final(&text, vcx));
                    // 只有定稿句送翻译；译文不进键入输出
                    if let Some((seq, true)) = self.translation.on_final(&text) {
                        self.with_view(cx, |v, vcx| {
                            v.set_translation(seq, TranslationState::Pending, vcx)
                        });
                    }
                    self.type_sync(cx, tray, true);
                }
                Effect::Finishing => self.set_status(cx, tray, Status::Finishing),
                Effect::Done => self.on_ended(cx, tray, now, Status::Done),
                Effect::Failed(failure) => self.on_ended(cx, tray, now, Status::Failed(failure)),
            }
        }
    }

    /// 新一轮开始：已打开的浮窗清空；只浮窗模式立刻弹出。
    fn on_begin(&mut self, cx: &mut ShellContext) {
        self.with_view(cx, |v, vcx| v.begin_round(vcx));
        if self.plan.overlay {
            self.ensure_overlay(cx);
        }
    }

    /// 一轮结束（正常或失败）：未落定文字并入已落定，补发键入，失败时一定弹浮窗说明。
    fn on_ended(
        &mut self,
        cx: &mut ShellContext,
        tray: Option<&TrayService>,
        now: Instant,
        status: Status,
    ) {
        self.probe_deadline = None;
        let leftover = self.transcript.partial().to_string();
        if !leftover.is_empty() {
            self.transcript.push_final(&leftover);
            self.with_view(cx, |v, vcx| v.push_final(&leftover, vcx));
            // 收尾并入的残余文字占一个句位，但不翻译
            self.translation.register(&leftover);
        }
        if self.plan_pending() {
            // 焦点判定还没回来就结束了：按不确定处理，保证文字有去处
            self.plan = decide(self.mode, self.with_overlay, None);
        }
        if status.is_failed() {
            self.plan.overlay = true;
        }
        self.set_status(cx, tray, status);
        if self.plan.overlay {
            self.ensure_overlay(cx);
        }
        if self.plan.typing {
            self.type_sync(cx, tray, true);
            if self.plan.typing && self.typer.typed() != self.transcript.full() {
                self.flush_deadline = Some(now + TYPE_FLUSH_TIMEOUT);
            }
        }
        self.sync_badge(cx);
    }

    /// 按焦点判定定下本轮输出方案（整轮不变）。
    fn decide_route(
        &mut self,
        cx: &mut ShellContext,
        tray: Option<&TrayService>,
        verdict: Option<Verdict>,
    ) {
        self.probe_deadline = None;
        self.plan = decide(self.mode, self.with_overlay, verdict);
        if self.status.is_listening() {
            let status = self.listening_status();
            self.set_status(cx, tray, status);
        }
        if self.plan.overlay {
            self.ensure_overlay(cx);
        }
        if self.plan.typing {
            self.type_sync(cx, tray, true);
        }
        self.sync_badge(cx);
    }

    /// 把已键入内容修正到当前转写；目标变了或发送失败就退到浮窗。
    fn type_sync(&mut self, cx: &mut ShellContext, tray: Option<&TrayService>, settle: bool) {
        if !self.plan.typing {
            return;
        }
        let desired = self.transcript.full();
        match self.typer.sync(&desired, settle, self.sink.as_mut()) {
            SyncOutcome::Synced | SyncOutcome::Deferred | SyncOutcome::Skipped => {}
            SyncOutcome::TargetChanged => self.fall_back(cx, tray, RouteNote::TargetLost),
            SyncOutcome::Failed(reason) => self.fall_back(cx, tray, RouteNote::SendFailed(reason)),
        }
    }

    /// 键入出问题：停止键入，浮窗里铺上全部已识别文字，状态里写明原因。
    fn fall_back(&mut self, cx: &mut ShellContext, tray: Option<&TrayService>, note: RouteNote) {
        tracing::warn!(?note, "键入中断，文字改在浮窗保留");
        self.plan.fall_back(note);
        if self.status.is_listening() {
            let status = self.listening_status();
            self.set_status(cx, tray, status);
        }
        // 浮窗已开（键入时同时显示）就不重铺，免得冲掉用户的编辑；没开则打开并铺上全部转写
        self.ensure_overlay(cx);
        self.flush_deadline = None;
        self.sync_badge(cx);
    }

    /// 用当前转写与状态重新铺满浮窗。
    fn reseed(&mut self, cx: &mut ShellContext) {
        let finals = self.transcript.finals().to_string();
        let partial = self.transcript.partial().to_string();
        let status = self.status.clone();
        let translations = self.translation.entries().to_vec();
        self.with_view(cx, |v, vcx| {
            v.seed(&finals, &partial, &translations, vcx);
            v.set_status(status, vcx);
        });
    }

    /// 聆听指示是否该显示：识别进行中，且文字只走键入（没有浮窗）。
    fn badge_wanted(&self) -> bool {
        self.engine.active() && self.plan.typing && !self.plan.overlay
    }

    /// 按当前状态显示或关闭聆听指示。
    fn sync_badge(&mut self, cx: &mut ShellContext) {
        if self.badge_wanted() {
            self.show_badge(cx);
        } else {
            self.hide_badge(cx);
        }
    }

    /// 在主屏右下角打开聆听指示（鼠标穿透、不抢焦点，避免改变键入目标）；已打开则不重复。
    fn show_badge(&mut self, cx: &mut ShellContext) {
        if self
            .badge
            .as_ref()
            .is_some_and(|window| cx.is_window_open(window))
        {
            return;
        }
        self.badge = None;
        let Some(rect) = cx
            .monitors()
            .ok()
            .and_then(|monitors| monitors.primary().map(|m| badge_rect(m.work_area, m.scale)))
        else {
            tracing::warn!("取不到主显示器信息，无法显示聆听指示");
            return;
        };
        let spec = WindowSpec {
            title: String::new(),
            placement: Placement::Physical(rect),
            transparent: true,
            always_on_top: true,
            decorations: false,
            show_in_taskbar: false,
            focus: false,
            resizable: false,
        };
        match cx.open_window(&spec, |_window, app| ListeningBadge::create(app)) {
            Ok((window, _view)) => {
                if let Err(e) = window.set_input_transparent(true) {
                    tracing::warn!(error = %e, "聆听指示设置鼠标穿透失败");
                }
                // 窗口按不透明绘制时，用整窗 alpha 呈现半透明蒙层（须在鼠标穿透之后设置）
                if let Err(e) = window.set_window_alpha(BADGE_WINDOW_ALPHA) {
                    tracing::warn!(error = %e, "聆听指示设置整窗透明度失败");
                }
                self.badge = Some(window);
            }
            Err(e) => tracing::warn!(error = %e, "打开聆听指示失败"),
        }
    }

    /// 关闭聆听指示（未打开则忽略）。
    fn hide_badge(&mut self, cx: &mut ShellContext) {
        if let Some(window) = self.badge.take()
            && cx.is_window_open(&window)
        {
            window.close(cx.app());
        }
    }

    /// 浮窗是否打开着。
    fn overlay_open(&self, cx: &ShellContext) -> bool {
        self.overlay
            .as_ref()
            .is_some_and(|(window, _)| cx.is_window_open(window))
    }

    /// 在浮窗视图上执行操作（窗口已关闭则忽略）。
    fn with_view(
        &self,
        cx: &mut ShellContext,
        f: impl FnOnce(&mut DictationView, &mut Context<DictationView>),
    ) {
        if let Some((window, view)) = &self.overlay
            && cx.is_window_open(window)
        {
            view.update(cx.app(), f);
        }
    }

    /// 确保浮窗已打开；本轮被用户关掉后不再自动重开。新开的浮窗立即铺上当前转写与状态。
    fn ensure_overlay(&mut self, cx: &mut ShellContext) {
        if self.overlay_open(cx) {
            return;
        }
        if self.overlay.take().is_some() && self.overlay_seen_open {
            self.dismissed = true;
        }
        if self.dismissed {
            return;
        }
        let Some(rect) = self.overlay_rect(cx) else {
            tracing::error!("取不到显示器信息，无法打开语音转文字浮窗");
            return;
        };
        let spec = WindowSpec {
            title: String::new(),
            placement: Placement::Physical(rect),
            transparent: false,
            always_on_top: true,
            decorations: false,
            show_in_taskbar: false,
            focus: false,
            resizable: false,
        };
        let prefs = ui_prefs_from_config(&self.config);
        match cx.open_window(&spec, move |window, app| {
            DictationView::create(window, app, prefs)
        }) {
            Ok((window, view)) => {
                tracing::info!(
                    width = WINDOW_WIDTH,
                    height = WINDOW_HEIGHT,
                    "语音转文字浮窗已打开"
                );
                self.overlay = Some((window, view));
                self.overlay_seen_open = true;
                self.reseed(cx);
            }
            Err(e) => tracing::error!(error = %e, "打开语音转文字浮窗失败"),
        }
    }

    /// 浮窗落点：光标所在显示器工作区的右下角。
    fn overlay_rect(&self, cx: &ShellContext) -> Option<snow_ui::shell::geometry::PhysicalRect> {
        let monitors = cx.monitors().ok()?;
        let monitor = pick_monitor(&monitors, cursor_screen_position().ok())?;
        Some(bottom_right_rect(monitor.work_area, monitor.scale))
    }

    /// 浮窗被用户关掉（关闭按钮 / Esc）：记下，并结束这一轮，即退出语音识别模式。
    fn watch_overlay_dismissed(
        &mut self,
        cx: &mut ShellContext,
        tray: Option<&TrayService>,
        now: Instant,
    ) {
        if self.overlay_seen_open && self.overlay.is_some() && !self.overlay_open(cx) {
            self.overlay = None;
            self.dismissed = true;
            if self.engine.active() {
                self.stop(cx, tray, now);
            }
        }
    }

    /// 更新状态：记录、同步到浮窗，并刷新托盘悬停提示（进行中显示状态，结束后复原）。
    fn set_status(&mut self, cx: &mut ShellContext, tray: Option<&TrayService>, status: Status) {
        let locale = ui_prefs_from_config(&self.config).locale;
        let tooltip = match status {
            Status::Done | Status::Failed(_) => TRAY_TOOLTIP.to_string(),
            _ => format!("{TRAY_TOOLTIP} - {}", status.message(locale)),
        };
        if let Some(tray) = tray
            && let Err(e) = tray.set_tooltip(tooltip)
        {
            tracing::debug!(error = %e, "更新托盘提示失败");
        }
        self.status = status.clone();
        self.with_view(cx, |v, vcx| v.set_status(status, vcx));
    }
}
