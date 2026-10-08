//! 录制区域视图与悬浮控制条（Recording Area View）。
//!
//! 一个铺满所在显示器的透明窗口：只画选区外圈边框、倒计时数字和控制条，
//! 其余像素靠 `SetWindowRgn` 命中区域穿透给被录制的应用（区域计算见 [`compute_layout`]）。
//! 窗口自身通过 `WDA_EXCLUDEFROMCAPTURE` 排除在录制画面之外。

use crate::recording::keymap::{RecordKeyAction, RecordKeymap};
use crate::recording::model::{RecordingConfig, RecordingFormat, RecordingState};
use crate::recording::runtime::ScreenRecordingSession;
use snow_ui::shell::geometry::{PhysicalPoint, PhysicalRect};
use snow_ui::ui::*;
use snow_ui::widgets::calculate_toolbar_placement;
use std::time::{Duration, Instant};

/// 边框厚度（逻辑像素）。
const BORDER_LOGICAL: f32 = 2.0;
/// 控制条逻辑宽度。
const TOOLBAR_LOGICAL_WIDTH: f32 = 400.0;
/// 控制条逻辑高度。
const TOOLBAR_LOGICAL_HEIGHT: f32 = 40.0;
/// 音频降级提示行的逻辑高度。
const NOTICE_LOGICAL_HEIGHT: f32 = 22.0;
/// 音频降级提示的文字颜色（警示黄）。
const COLOR_NOTICE: u32 = 0xFAAD14FF;
/// 多条音频提示之间的分隔。
const NOTICE_SEPARATOR: &str = " · ";
/// 控制条与选区的逻辑间距。
const TOOLBAR_LOGICAL_MARGIN: f32 = 8.0;
/// 倒计时数字框的逻辑边长。
const COUNTDOWN_LOGICAL_BOX: f32 = 160.0;
/// 倒计时每步时长。
const COUNTDOWN_STEP: Duration = Duration::from_secs(1);
/// 错误信息在窗口里停留的时长。
const ERROR_DISPLAY: Duration = Duration::from_secs(8);
/// 错误文案最多显示的字符数。
const ERROR_MAX_CHARS: usize = 34;
/// 边框颜色：待命 / 倒计时（蓝）。
const COLOR_IDLE: u32 = 0x1677FFFF;
/// 边框颜色：录制中（红）。
const COLOR_RECORDING: u32 = 0xFF4D4FFF;
/// 边框颜色：暂停（黄）。
const COLOR_PAUSED: u32 = 0xFAAD14FF;

/// 录制交互事件通知。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecordingAreaAction {
    /// 暂停或恢复录制。
    TogglePause,
    /// 停止录制并保存导出。
    StopAndSave,
    /// 取消并放弃当前录制。
    Cancel,
    /// 关闭错误提示。
    Dismiss,
}

/// 自动化验证计划（仅由环境变量驱动，不经过操作系统输入）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AutoPlan {
    /// 有效录制满多少秒后自动停止。
    pub stop_after_secs: u64,
    /// 可选的暂停计划：`(有效录制满多少秒时暂停, 暂停多少秒)`。
    pub pause: Option<(u64, u64)>,
}

/// 自动化计划的运行状态。
#[derive(Debug, Clone, Copy)]
struct AutoState {
    /// 计划。
    plan: AutoPlan,
    /// 暂停已执行。
    pause_done: bool,
    /// 本次暂停开始时刻。
    paused_since: Option<Instant>,
}

/// 窗口内各元素的物理像素布局（窗口坐标）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AreaLayout {
    /// 边框四条（上、下、左、右），已裁到窗口内。
    pub border: Vec<PhysicalRect>,
    /// 控制条。
    pub toolbar: PhysicalRect,
    /// 倒计时数字框（仅倒计时期间）。
    pub countdown_box: Option<PhysicalRect>,
}

impl AreaLayout {
    /// 需要接收点击（同时也是可见）的全部矩形：边框 ∪ 控制条 ∪ 倒计时框。
    pub fn hit_rects(&self) -> Vec<PhysicalRect> {
        let mut rects = self.border.clone();
        rects.push(self.toolbar);
        rects.extend(self.countdown_box);
        rects
    }
}

/// 计算窗口内各元素的物理布局。
///
/// # 参数
/// - `region`：录制区域（窗口坐标，物理像素）。
/// - `window`：窗口矩形（原点 0,0，物理像素）。
/// - `scale`：显示器缩放比（物理 / 逻辑）。
/// - `countdown`：是否处于倒计时（需要数字框）。
/// - `notice`：控制条是否多带一行音频降级提示（增高）。
///
/// # 返回
/// 布局；边框条画在选区**外侧**，不会进入被录制的画面。
///
/// # 示例
/// ```ignore
/// let l = compute_layout(PhysicalRect::new(100, 100, 400, 300), PhysicalRect::new(0, 0, 1920, 1080), 1.0, false, false);
/// assert_eq!(l.border.len(), 4);
/// ```
pub fn compute_layout(
    region: PhysicalRect,
    window: PhysicalRect,
    scale: f32,
    countdown: bool,
    notice: bool,
) -> AreaLayout {
    let t = (BORDER_LOGICAL * scale).ceil() as i32;
    let strips = [
        PhysicalRect::new(region.x - t, region.y - t, region.width + 2 * t, t),
        PhysicalRect::new(region.x - t, region.bottom(), region.width + 2 * t, t),
        PhysicalRect::new(region.x - t, region.y, t, region.height),
        PhysicalRect::new(region.right(), region.y, t, region.height),
    ];
    let border = strips
        .iter()
        .filter_map(|s| s.intersect(&window))
        .collect();
    let size = PhysicalPoint::new(
        (TOOLBAR_LOGICAL_WIDTH * scale).round() as i32,
        ((TOOLBAR_LOGICAL_HEIGHT + if notice { NOTICE_LOGICAL_HEIGHT } else { 0.0 }) * scale).round() as i32,
    );
    let margin = (TOOLBAR_LOGICAL_MARGIN * scale).round() as i32;
    let pos = calculate_toolbar_placement(region, size, window, margin);
    let toolbar = PhysicalRect::new(pos.x, pos.y, size.x, size.y);
    let countdown_box = countdown.then(|| {
        let side = (COUNTDOWN_LOGICAL_BOX * scale).round() as i32;
        PhysicalRect::new(
            region.x + (region.width - side) / 2,
            region.y + (region.height - side) / 2,
            side,
            side,
        )
    });
    AreaLayout {
        border,
        toolbar,
        countdown_box,
    }
}

/// 把错误文案截到合理长度。
///
/// # 参数
/// - `reason`：原始错误原因。
pub fn shorten_reason(reason: &str) -> String {
    if reason.chars().count() <= ERROR_MAX_CHARS {
        reason.to_string()
    } else {
        let head: String = reason.chars().take(ERROR_MAX_CHARS).collect();
        format!("{head}…")
    }
}

/// 录制区域视图组件。
pub struct RecordingAreaView {
    /// 录制核心会话控制器。
    pub session: ScreenRecordingSession,
    /// 窗口所在显示器左上角（虚拟桌面物理坐标），用于把选区换算成窗口坐标。
    origin: PhysicalPoint,
    /// 窗口物理尺寸。
    window_size: (i32, i32),
    /// 显示器缩放比。
    scale: f32,
    /// 用户已取消。
    cancelled: bool,
    /// 用户已关闭错误提示。
    dismissed: bool,
    /// 上一次倒计时步进时刻。
    countdown_tick_at: Instant,
    /// 首次观察到错误的时刻。
    error_since: Option<Instant>,
    /// 自动化计划。
    auto: Option<AutoState>,
    /// 界面语言代码（提示文案用）。
    locale: &'static str,
    /// 控制条键位表。
    keymap: RecordKeymap,
    /// 键盘焦点句柄（离屏测试为空）。
    focus: Option<FocusHandle>,
    /// 完成后把录制文件复制到剪贴板。
    copy_on_finish: bool,
}

impl RecordingAreaView {
    /// 创建录制区域视图。
    ///
    /// # 参数
    /// - `config`：录制配置（区域为虚拟桌面物理坐标）。
    /// - `monitor_bounds`：窗口所在显示器的物理范围。
    /// - `scale`：显示器缩放比。
    pub fn new(config: RecordingConfig, monitor_bounds: PhysicalRect, scale: f32) -> Self {
        let scale = if scale.is_finite() && scale > 0.0 { scale } else { 1.0 };
        Self {
            session: ScreenRecordingSession::new(config),
            origin: PhysicalPoint::new(monitor_bounds.x, monitor_bounds.y),
            window_size: (monitor_bounds.width, monitor_bounds.height),
            scale,
            cancelled: false,
            dismissed: false,
            countdown_tick_at: Instant::now(),
            error_since: None,
            auto: None,
            locale: snow_i18n::FALLBACK_LOCALE,
            keymap: RecordKeymap::default(),
            focus: None,
            copy_on_finish: false,
        }
    }

    /// 设置控制条键位表。
    ///
    /// # 参数
    /// - `keymap`：由配置构造的键位表。
    pub fn set_keymap(&mut self, keymap: RecordKeymap) {
        self.keymap = keymap;
    }

    /// 设置键盘焦点句柄（视图实体创建时由 GPUI 提供）。
    pub fn set_focus_handle(&mut self, focus: FocusHandle) {
        self.focus = Some(focus);
    }

    /// 设置录制完成后是否把文件复制到剪贴板。
    ///
    /// # 参数
    /// - `copy`：是否复制。
    pub fn set_copy_on_finish(&mut self, copy: bool) {
        self.copy_on_finish = copy;
    }

    /// 录制完成后是否需要把文件复制到剪贴板。
    pub fn copy_on_finish(&self) -> bool {
        self.copy_on_finish
    }

    /// 处理一次按键动作；只在状态允许时生效（录制中才能导出 / 暂停，Esc 只放弃倒计时与错误提示）。
    ///
    /// # 参数
    /// - `action`：键位表解析出的动作。
    pub fn handle_key_action(&mut self, action: RecordKeyAction) {
        let recording = matches!(self.session.state(), RecordingState::Recording { .. });
        match action {
            RecordKeyAction::Export if recording => self.handle_action(RecordingAreaAction::StopAndSave),
            RecordKeyAction::ToggleRecording if recording => self.handle_action(RecordingAreaAction::TogglePause),
            RecordKeyAction::CopyToClipboard if recording => {
                self.copy_on_finish = true;
                self.handle_action(RecordingAreaAction::StopAndSave);
            }
            RecordKeyAction::EndRecording => match self.session.state() {
                RecordingState::Countdown { .. } => self.handle_action(RecordingAreaAction::Cancel),
                RecordingState::Error { .. } => self.handle_action(RecordingAreaAction::Dismiss),
                _ => {}
            },
            _ => {}
        }
    }

    /// 设置界面语言（影响音频降级提示文案）。
    ///
    /// # 参数
    /// - `locale`：语料语言代码，如 `zh-CN`。
    pub fn set_locale(&mut self, locale: &'static str) {
        self.locale = locale;
    }

    /// 音频降级提示文本：仅倒计时 / 录制 / 保存期间显示，无降级时为 `None`。
    pub fn audio_notice_text(&self) -> Option<String> {
        if !matches!(
            self.session.state(),
            RecordingState::Countdown { .. } | RecordingState::Recording { .. } | RecordingState::Saving
        ) {
            return None;
        }
        let notices = self.session.audio_notices();
        if notices.is_empty() {
            return None;
        }
        let i18n = crate::ocr_backend::i18n_for(self.locale);
        let texts: Vec<String> = notices.iter().map(|n| i18n.tr(n.message_id())).collect();
        Some(texts.join(NOTICE_SEPARATOR))
    }

    /// 启用自动化验证计划（仅测试 / 验收用）。
    pub fn set_auto_plan(&mut self, plan: AutoPlan) {
        self.auto = Some(AutoState {
            plan,
            pause_done: false,
            paused_since: None,
        });
    }

    /// 获取录制物理矩形（虚拟桌面坐标）。
    pub fn bounds(&self) -> PhysicalRect {
        self.session.config().region
    }

    /// 获取当前录制格式。
    pub fn format(&self) -> RecordingFormat {
        self.session.config().format
    }

    /// 录制区域在窗口坐标下的位置。
    fn region_in_window(&self) -> PhysicalRect {
        let r = self.bounds();
        PhysicalRect::new(r.x - self.origin.x, r.y - self.origin.y, r.width, r.height)
    }

    /// 当前状态下的窗口布局。
    pub fn layout(&self) -> AreaLayout {
        let window = PhysicalRect::new(0, 0, self.window_size.0, self.window_size.1);
        let countdown = matches!(self.session.state(), RecordingState::Countdown { .. });
        compute_layout(
            self.region_in_window(),
            window,
            self.scale,
            countdown,
            self.audio_notice_text().is_some(),
        )
    }

    /// 处理外部操作动作。
    ///
    /// # 参数
    /// - `action`：用户在控制条上触发的动作。
    pub fn handle_action(&mut self, action: RecordingAreaAction) {
        match action {
            RecordingAreaAction::TogglePause => self.session.toggle_pause(),
            RecordingAreaAction::StopAndSave => {
                if let Err(e) = self.session.finish() {
                    tracing::warn!(error = %e, "停止录制被拒绝");
                }
            }
            RecordingAreaAction::Cancel => {
                self.session.cancel();
                self.cancelled = true;
            }
            RecordingAreaAction::Dismiss => self.dismissed = true,
        }
    }

    /// 推进时间：倒计时、录制进程事件、超时检查与自动化计划。
    ///
    /// # 参数
    /// - `now`：当前时刻。
    ///
    /// # 返回
    /// 是否有可见变化（需要重绘）。
    pub fn advance(&mut self, now: Instant) -> bool {
        let mut changed = false;
        if matches!(self.session.state(), RecordingState::Countdown { .. })
            && now.saturating_duration_since(self.countdown_tick_at) >= COUNTDOWN_STEP
        {
            self.countdown_tick_at = now;
            self.session.tick_countdown();
            changed = true;
        }
        changed |= self.session.poll(now);
        changed |= self.run_auto_plan(now);
        if let RecordingState::Error { reason } = self.session.state()
            && self.error_since.is_none()
        {
            tracing::error!(%reason, "录制出错");
        }
        if matches!(self.session.state(), RecordingState::Error { .. }) && self.error_since.is_none() {
            self.error_since = Some(now);
        }
        changed
    }

    /// 执行自动化计划（暂停 / 恢复 / 停止）。
    fn run_auto_plan(&mut self, now: Instant) -> bool {
        let Some(mut auto) = self.auto else {
            return false;
        };
        let RecordingState::Recording {
            elapsed_secs,
            is_paused,
            ..
        } = *self.session.state()
        else {
            return false;
        };
        let mut acted = false;
        if let Some((at_secs, pause_secs)) = auto.plan.pause {
            if !auto.pause_done && !is_paused && elapsed_secs >= at_secs {
                self.session.toggle_pause();
                auto.pause_done = true;
                auto.paused_since = Some(now);
                acted = true;
            } else if is_paused
                && let Some(since) = auto.paused_since
                && now.saturating_duration_since(since) >= Duration::from_secs(pause_secs)
            {
                self.session.toggle_pause();
                auto.paused_since = None;
                acted = true;
            }
        }
        let paused_now = matches!(
            self.session.state(),
            RecordingState::Recording { is_paused: true, .. }
        );
        if !acted && !paused_now && elapsed_secs >= auto.plan.stop_after_secs {
            self.handle_action(RecordingAreaAction::StopAndSave);
            acted = true;
        }
        self.auto = Some(auto);
        acted
    }

    /// 窗口是否应该关闭：已完成、被取消、错误提示已读或超时。
    ///
    /// # 参数
    /// - `now`：当前时刻。
    pub fn is_over(&self, now: Instant) -> bool {
        if self.cancelled || self.dismissed {
            return true;
        }
        match self.session.state() {
            RecordingState::Finished { .. } => true,
            RecordingState::Error { .. } => self
                .error_since
                .is_some_and(|t| now.saturating_duration_since(t) >= ERROR_DISPLAY),
            _ => false,
        }
    }

    /// 物理矩形换算为逻辑像素位置（左、上、宽、高）。
    fn logical(&self, r: PhysicalRect) -> (Pixels, Pixels, Pixels, Pixels) {
        (
            px(r.x as f32 / self.scale),
            px(r.y as f32 / self.scale),
            px(r.width as f32 / self.scale),
            px(r.height as f32 / self.scale),
        )
    }

    /// 控制条按钮。
    fn button(
        &self,
        id: &'static str,
        label: &'static str,
        bg: u32,
        text: u32,
        action: RecordingAreaAction,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        div()
            .id(id)
            .px_2()
            .py_1()
            .rounded_sm()
            .bg(rgba(bg))
            .text_size(px(12.0))
            .text_color(rgba(text))
            .cursor_pointer()
            .child(label)
            .on_click(cx.listener(move |this, _ev: &ClickEvent, _window, cx| {
                this.handle_action(action);
                cx.notify();
            }))
    }
}

impl Render for RecordingAreaView {
    /// 渲染边框、倒计时数字与控制条。
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let state = self.session.state().clone();
        let layout = self.layout();
        let bounds = self.bounds();
        let accent = match state {
            RecordingState::Recording { is_paused: false, .. } | RecordingState::Saving => COLOR_RECORDING,
            RecordingState::Recording { is_paused: true, .. } => COLOR_PAUSED,
            _ => COLOR_IDLE,
        };

        let mut root = div()
            .relative()
            .w_full()
            .h_full()
            .on_key_down(cx.listener(|this, ev: &KeyDownEvent, _window, cx| {
                let m = ev.keystroke.modifiers;
                if let Some(action) = this.keymap.resolve(ev.keystroke.key.as_str(), m.control, m.shift, m.alt) {
                    this.handle_key_action(action);
                    cx.notify();
                    cx.stop_propagation();
                }
            }))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _: &MouseDownEvent, window, cx| {
                    if let Some(focus) = &this.focus {
                        window.focus(focus, cx);
                    }
                }),
            );
        if let Some(focus) = &self.focus {
            root = root.track_focus(focus);
        }
        for strip in &layout.border {
            let (l, t, w, h) = self.logical(*strip);
            root = root.child(div().absolute().left(l).top(t).w(w).h(h).bg(rgba(accent)));
        }

        if let (Some(boxr), RecordingState::Countdown { seconds_left }) = (layout.countdown_box, &state) {
            let (l, t, w, h) = self.logical(boxr);
            root = root.child(
                div()
                    .absolute()
                    .left(l)
                    .top(t)
                    .w(w)
                    .h(h)
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded_lg()
                    .bg(rgba(0x000000AA))
                    .text_size(px(72.0))
                    .font_weight(FontWeight::BOLD)
                    .text_color(rgba(0xFFFFFFFF))
                    .child(format!("{seconds_left}")),
            );
        }

        let (l, t, w, h) = self.logical(layout.toolbar);
        let notice_text = self.audio_notice_text();
        let mut bar = div()
            .w_full()
            .h(px(TOOLBAR_LOGICAL_HEIGHT))
            .flex()
            .items_center()
            .gap_2()
            .px_3()
            .child(div().w(px(10.0)).h(px(10.0)).rounded_full().bg(rgba(accent)));
        let label_style = |text: String| {
            div()
                .text_size(px(13.0))
                .font_weight(FontWeight::MEDIUM)
                .text_color(rgba(0xFFFFFFFF))
                .child(text)
        };
        match &state {
            RecordingState::Countdown { seconds_left } => {
                bar = bar
                    .child(label_style(format!("{seconds_left} 秒后开始")))
                    .child(self.button("rec-cancel", "放弃", 0x434343FF, 0xFF4D4FFF, RecordingAreaAction::Cancel, cx));
            }
            RecordingState::Recording {
                elapsed_secs,
                is_paused,
                ..
            } => {
                bar = bar
                    .child(label_style(RecordingState::format_duration(*elapsed_secs)))
                    .child(div().w(px(1.0)).h(px(16.0)).bg(rgba(0xFFFFFF26)))
                    .child(
                        div()
                            .text_size(px(12.0))
                            .text_color(rgba(0x8C8C8CFF))
                            .child(format!("{}x{}", bounds.width, bounds.height)),
                    )
                    .child(self.button(
                        "rec-pause",
                        if *is_paused { "继续" } else { "暂停" },
                        0x303030FF,
                        0xFFFFFFFF,
                        RecordingAreaAction::TogglePause,
                        cx,
                    ))
                    .child(self.button("rec-stop", "完成", COLOR_IDLE, 0xFFFFFFFF, RecordingAreaAction::StopAndSave, cx))
                    .child(self.button("rec-cancel", "放弃", 0x434343FF, 0xFF4D4FFF, RecordingAreaAction::Cancel, cx));
            }
            RecordingState::Saving => bar = bar.child(label_style("正在保存…".to_string())),
            RecordingState::Error { reason } => {
                bar = bar
                    .child(label_style(format!("录制失败: {}", shorten_reason(reason))))
                    .child(self.button("rec-dismiss", "关闭", 0x434343FF, 0xFFFFFFFF, RecordingAreaAction::Dismiss, cx));
            }
            RecordingState::Idle | RecordingState::Finished { .. } => {}
        }
        let mut outer = div()
            .absolute()
            .left(l)
            .top(t)
            .w(w)
            .h(h)
            .flex()
            .flex_col()
            .rounded_lg()
            .bg(rgba(0x1F1F1FEE))
            .border_1()
            .border_color(rgba(0xFFFFFF26))
            .child(bar);
        if let Some(text) = notice_text {
            outer = outer.child(
                div()
                    .w_full()
                    .h(px(NOTICE_LOGICAL_HEIGHT))
                    .px_3()
                    .flex()
                    .items_center()
                    .text_size(px(11.0))
                    .text_color(rgba(COLOR_NOTICE))
                    .child(text),
            );
        }
        root.child(outer)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::recording::model::RecordingConfig;
    use crate::recording::runtime::{LinkEvent, RecorderLink};
    use snow_recorder_protocol::{Command, Event};
    use std::cell::RefCell;
    use std::rc::Rc;

    /// 假通道与测试共享的数据：已发送命令、待取事件。
    type Shared = Rc<RefCell<(Vec<Command>, Vec<LinkEvent>)>>;

    /// 假通道：记录命令，事件由测试注入。
    struct Link(Shared);

    impl RecorderLink for Link {
        /// 记录命令。
        fn send(&mut self, command: &Command) -> Result<(), String> {
            self.0.borrow_mut().0.push(command.clone());
            Ok(())
        }
        /// 取走事件。
        fn poll(&mut self) -> Vec<LinkEvent> {
            std::mem::take(&mut self.0.borrow_mut().1)
        }
        /// 无操作。
        fn shutdown(&mut self) {}
    }

    /// 构造视图（区域 100,100 800x600，显示器原点 0,0 1920x1080）。
    fn view(countdown: u32) -> (RecordingAreaView, Shared) {
        let shared = Rc::new(RefCell::new((Vec::new(), Vec::new())));
        let config = RecordingConfig {
            region: PhysicalRect::new(100, 100, 800, 600),
            ..RecordingConfig::default()
        };
        let mut v = RecordingAreaView::new(config, PhysicalRect::new(0, 0, 1920, 1080), 1.0);
        v.session.begin(Box::new(Link(shared.clone())), countdown);
        (v, shared)
    }

    /// 键位动作按状态生效：录制中可导出 / 暂停 / 复制，Esc 只放弃倒计时，录制中的 Esc 不丢录像。
    #[test]
    fn key_actions_follow_state() {
        let shared = Rc::new(RefCell::new((Vec::new(), Vec::new())));
        let config = RecordingConfig { region: PhysicalRect::new(100, 100, 800, 600), ..RecordingConfig::default() };
        let mut v = RecordingAreaView::new(config, PhysicalRect::new(0, 0, 1920, 1080), 1.0);
        // 空闲时导出 / Esc 都不起作用
        v.handle_key_action(RecordKeyAction::Export);
        v.handle_key_action(RecordKeyAction::EndRecording);
        assert!(shared.borrow().0.is_empty() && !v.cancelled);

        v.session.begin(Box::new(Link(shared.clone())), 3);
        v.handle_key_action(RecordKeyAction::Export);
        assert!(shared.borrow().0.is_empty(), "倒计时中不能导出，也还没发过 START");
        v.handle_key_action(RecordKeyAction::EndRecording);
        assert!(v.cancelled, "倒计时中 Esc 放弃");

        let shared = Rc::new(RefCell::new((Vec::new(), Vec::new())));
        let config = RecordingConfig { region: PhysicalRect::new(100, 100, 800, 600), ..RecordingConfig::default() };
        let mut v = RecordingAreaView::new(config, PhysicalRect::new(0, 0, 1920, 1080), 1.0);
        v.session.begin(Box::new(Link(shared.clone())), 0);
        v.handle_key_action(RecordKeyAction::ToggleRecording);
        assert_eq!(shared.borrow().0.last(), Some(&Command::Pause));
        v.handle_key_action(RecordKeyAction::EndRecording);
        assert!(!v.cancelled, "录制中 Esc 不丢录像");
        v.handle_key_action(RecordKeyAction::CopyToClipboard);
        assert!(v.copy_on_finish());
        assert_eq!(shared.borrow().0.last(), Some(&Command::Stop));
    }

    /// 音频降级提示：只在录制期间出现，随语言切换，并让控制条增高一行。
    #[test]
    fn audio_notice_shows_and_grows_toolbar() {
        use snow_recorder_protocol::{AudioRequest, AudioSource, AudioStatus};
        let shared = Rc::new(RefCell::new((Vec::new(), Vec::new())));
        let config = RecordingConfig {
            region: PhysicalRect::new(100, 100, 800, 600),
            audio: AudioRequest { microphone: true, system: true, ..AudioRequest::default() },
            ..RecordingConfig::default()
        };
        let mut v = RecordingAreaView::new(config, PhysicalRect::new(0, 0, 1920, 1080), 1.0);
        v.set_locale("zh-CN");
        v.session.begin(Box::new(Link(shared.clone())), 0);
        let plain_h = v.layout().toolbar.height;
        assert_eq!(v.audio_notice_text(), None);
        let event = |source, status| LinkEvent::Event(Event::AudioState { source, status });
        shared.borrow_mut().1.push(event(AudioSource::Microphone, AudioStatus::Unavailable));
        assert!(v.advance(Instant::now()));
        assert_eq!(v.audio_notice_text().as_deref(), Some("麦克风不可用"));
        assert!(v.layout().toolbar.height > plain_h);
        shared.borrow_mut().1.push(event(AudioSource::System, AudioStatus::Unavailable));
        v.advance(Instant::now());
        assert_eq!(v.audio_notice_text().as_deref(), Some("本次录制没有声音"));
        v.set_locale("en-US");
        assert_eq!(v.audio_notice_text().as_deref(), Some("This recording has no sound"));
    }

    /// 边框画在选区外侧且不与选区相交（不会进入被录制画面）。
    #[test]
    fn border_stays_outside_region() {
        let region = PhysicalRect::new(100, 100, 400, 300);
        let l = compute_layout(region, PhysicalRect::new(0, 0, 1920, 1080), 1.0, false, false);
        assert_eq!(l.border.len(), 4);
        for strip in &l.border {
            assert!(strip.intersect(&region).is_none(), "{strip:?} 侵入选区");
        }
    }

    /// 选区贴屏幕边时，越界的边框条被裁掉；全屏选区没有边框。
    #[test]
    fn border_clipped_at_screen_edge() {
        let win = PhysicalRect::new(0, 0, 1920, 1080);
        let l = compute_layout(PhysicalRect::new(0, 0, 1920, 1080), win, 1.0, false, false);
        assert!(l.border.is_empty());
        let l = compute_layout(PhysicalRect::new(0, 50, 500, 400), win, 1.0, false, false);
        assert!(l.border.iter().all(|s| win.intersect(s) == Some(*s)));
    }

    /// 控制条落在窗口内；倒计时框仅倒计时期间存在，居中于选区。
    #[test]
    fn toolbar_and_countdown_box() {
        let win = PhysicalRect::new(0, 0, 1920, 1080);
        let region = PhysicalRect::new(200, 200, 600, 400);
        let l = compute_layout(region, win, 1.5, true, false);
        assert_eq!(win.intersect(&l.toolbar), Some(l.toolbar));
        let b = l.countdown_box.unwrap();
        assert_eq!(b.x + b.width / 2, region.x + region.width / 2);
        assert!(compute_layout(region, win, 1.0, false, false).countdown_box.is_none());
        assert_eq!(l.hit_rects().len(), l.border.len() + 2);
    }

    /// 选区窗口坐标 = 虚拟桌面坐标 - 显示器原点（副屏在左侧时为负原点）。
    #[test]
    fn region_uses_monitor_origin() {
        let config = RecordingConfig {
            region: PhysicalRect::new(-1800, 100, 400, 300),
            ..RecordingConfig::default()
        };
        let v = RecordingAreaView::new(config, PhysicalRect::new(-1920, 0, 1920, 1080), 1.0);
        assert_eq!(v.region_in_window(), PhysicalRect::new(120, 100, 400, 300));
    }

    /// 倒计时按秒推进，结束时发出 START。
    #[test]
    fn countdown_advances_by_time() {
        let (mut v, shared) = view(2);
        let t0 = Instant::now();
        assert!(!v.advance(t0));
        assert!(v.advance(t0 + COUNTDOWN_STEP));
        assert!(matches!(v.session.state(), RecordingState::Countdown { seconds_left: 1 }));
        assert!(v.advance(t0 + 2 * COUNTDOWN_STEP));
        assert!(v.session.state().is_active());
        assert!(matches!(shared.borrow().0.first(), Some(Command::Start(_))));
    }

    /// 控制条动作：暂停 / 完成 / 取消 / 关闭提示。
    #[test]
    fn actions_drive_session() {
        let (mut v, shared) = view(0);
        v.handle_action(RecordingAreaAction::TogglePause);
        v.handle_action(RecordingAreaAction::StopAndSave);
        assert_eq!(*v.session.state(), RecordingState::Saving);
        assert_eq!(shared.borrow().0[1..], [Command::Pause, Command::Stop]);
        assert!(!v.is_over(Instant::now()));
        v.handle_action(RecordingAreaAction::Cancel);
        assert!(v.is_over(Instant::now()));
    }

    /// 录制进程崩溃后窗口会在提示停留时间后自动关闭，也可手动关闭。
    #[test]
    fn error_auto_closes_after_display_time() {
        let (mut v, shared) = view(0);
        shared.borrow_mut().1.push(LinkEvent::Exited { code: None });
        let t0 = Instant::now();
        assert!(v.advance(t0));
        assert!(matches!(v.session.state(), RecordingState::Error { .. }));
        assert!(!v.is_over(t0));
        assert!(v.is_over(t0 + ERROR_DISPLAY));
        let (mut v2, shared2) = view(0);
        shared2.borrow_mut().1.push(LinkEvent::Event(Event::Error { reason: "x".into() }));
        v2.advance(t0);
        v2.handle_action(RecordingAreaAction::Dismiss);
        assert!(v2.is_over(t0));
    }

    /// 自动化计划：到点暂停、暂停够久后恢复、录满后停止。
    #[test]
    fn auto_plan_pauses_resumes_and_stops() {
        let (mut v, shared) = view(0);
        v.set_auto_plan(AutoPlan { stop_after_secs: 4, pause: Some((2, 3)) });
        let t0 = Instant::now();
        let recording = |ms: u64| LinkEvent::Event(Event::Recording { elapsed_ms: ms, frames: 0 });
        shared.borrow_mut().1.push(recording(2100));
        v.advance(t0);
        assert_eq!(shared.borrow().0.last(), Some(&Command::Pause));
        shared.borrow_mut().1.push(LinkEvent::Event(Event::Paused));
        v.advance(t0 + Duration::from_secs(1));
        assert_eq!(shared.borrow().0.last(), Some(&Command::Pause), "暂停未满不应恢复");
        v.advance(t0 + Duration::from_secs(4));
        assert_eq!(shared.borrow().0.last(), Some(&Command::Resume));
        shared.borrow_mut().1.push(LinkEvent::Event(Event::Resumed));
        shared.borrow_mut().1.push(recording(4200));
        v.advance(t0 + Duration::from_secs(5));
        assert_eq!(shared.borrow().0.last(), Some(&Command::Stop));
    }

    /// 长错误文案被截断并带省略号。
    #[test]
    fn reason_is_shortened() {
        assert_eq!(shorten_reason("短"), "短");
        let long = "很".repeat(100);
        let s = shorten_reason(&long);
        assert_eq!(s.chars().count(), ERROR_MAX_CHARS + 1);
        assert!(s.ends_with('…'));
    }
}
