//! 长截图的界面与宿主：铺满显示器的透明窗口，只画选区外圈边框与一条控制条，
//! 其余像素靠命中区域穿透给被截取的应用（用户在那里滚动）；窗口自身排除在捕获之外。
//!
//! 采集与拼接在独立线程里跑（见 [`crate::scroll_capture`]），这里只读共享进度并把按钮转成控制开关。

use crate::app_runtime::UiEvent;
use crate::scroll_capture::{
    AUTO_SCROLL_DELTA, AutoScroller, CaptureOptions, CaptureTiming, CopyStatus, ScreenSource, ScrollControl,
    ScrollPhase, ScrollProgress, ScrollSink, SharedProgress, run_capture,
};
use crate::screenshot_output::{ExportOverrides, ExportSettings, home_directory, save_automatic};
use crate::settings_state::SharedConfig;
use snow_capability::CapabilityRegistry;
use snow_platform::clipboard::copy_image_to_clipboard;
use snow_platform::scroll_input::post_wheel;
use snow_ui::shell::geometry::{PhysicalPoint, PhysicalRect, Region};
use snow_ui::shell::inbox::MainThreadInbox;
use snow_ui::shell::monitor::{MonitorInfo, MonitorTarget};
use snow_ui::shell::window::WindowSpec;
use snow_ui::ui::*;
use snow_ui::widgets::calculate_toolbar_placement;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

/// 自动化验证环境变量：`x,y,宽,高[,auto][,秒数]`（虚拟桌面物理坐标），设置后长截图跳过选区直接开始。
pub const ENV_SCROLL_AUTOTEST: &str = "SNOW_SCROLL_AUTOTEST";
/// 自动化验证时覆盖输出目录的环境变量。
pub const ENV_SCROLL_AUTOTEST_DIR: &str = "SNOW_SCROLL_AUTOTEST_DIR";
/// 调试：设置后长截图窗不从捕获中排除（对照“排除是否生效”）。
pub const ENV_SCROLL_KEEP_VISIBLE: &str = "SNOW_SCROLL_KEEP_VISIBLE";
/// 界面刷新间隔。
const TICK_INTERVAL: Duration = Duration::from_millis(250);
/// 边框厚度（逻辑像素）。
const BORDER_LOGICAL: f32 = 2.0;
/// 控制条逻辑宽度。
const BAR_LOGICAL_WIDTH: f32 = 600.0;
/// 控制条逻辑高度。
const BAR_LOGICAL_HEIGHT: f32 = 64.0;
/// 控制条与选区的逻辑间距。
const BAR_LOGICAL_MARGIN: f32 = 8.0;
/// 完成后结果停留时长。
const DONE_DISPLAY: Duration = Duration::from_secs(4);
/// 失败信息停留时长。
const ERROR_DISPLAY: Duration = Duration::from_secs(8);
/// 边框：采集中（蓝）。
const COLOR_CAPTURING: u32 = 0x1677FFFF;
/// 边框：完成（绿）。
const COLOR_DONE: u32 = 0x52C41AFF;
/// 边框：失败（红）。
const COLOR_FAILED: u32 = 0xFF4D4FFF;
/// 自动化参数的最少字段数（区域 4 个）。
const AUTOTEST_MIN_FIELDS: usize = 4;

/// 自动化验证参数。
#[derive(Debug, Clone, PartialEq)]
pub struct ScrollAutotest {
    /// 区域（虚拟桌面物理坐标）。
    pub region: PhysicalRect,
    /// 是否开启自动滚动。
    pub auto_scroll: bool,
    /// 运行多少秒后自动完成（`None` 表示等人点完成）。
    pub stop_after_secs: Option<u64>,
}

/// 解析自动化参数；字段缺失、非数字或尺寸非法返回 `None`。
///
/// # 参数
/// - `text`：环境变量原始值，如 `2600,100,800,600,auto,20`。
///
/// ```ignore
/// let t = parse_scroll_autotest("10,20,800,600,auto,15").unwrap();
/// assert!(t.auto_scroll && t.stop_after_secs == Some(15));
/// ```
pub fn parse_scroll_autotest(text: &str) -> Option<ScrollAutotest> {
    let fields: Vec<&str> = text.split(',').map(str::trim).collect();
    if fields.len() < AUTOTEST_MIN_FIELDS {
        return None;
    }
    let x: i32 = fields[0].parse().ok()?;
    let y: i32 = fields[1].parse().ok()?;
    let w: i32 = fields[2].parse().ok().filter(|v| *v > 0)?;
    let h: i32 = fields[3].parse().ok().filter(|v| *v > 0)?;
    let mut auto_scroll = false;
    let mut stop_after_secs = None;
    for extra in &fields[AUTOTEST_MIN_FIELDS..] {
        if extra.eq_ignore_ascii_case("auto") {
            auto_scroll = true;
        } else if !extra.is_empty() {
            stop_after_secs = Some(extra.parse::<u64>().ok().filter(|v| *v > 0)?);
        }
    }
    Some(ScrollAutotest {
        region: PhysicalRect::new(x, y, w, h),
        auto_scroll,
        stop_after_secs,
    })
}

/// 窗口内的物理布局：边框四条 + 控制条。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScrollLayout {
    /// 边框四条（上、下、左、右），已裁到窗口内，画在选区外侧。
    pub border: Vec<PhysicalRect>,
    /// 控制条。
    pub bar: PhysicalRect,
}

impl ScrollLayout {
    /// 需要可见并接收点击的全部矩形。
    pub fn hit_rects(&self) -> Vec<PhysicalRect> {
        let mut rects = self.border.clone();
        rects.push(self.bar);
        rects
    }
}

/// 计算窗口内布局。
///
/// # 参数
/// - `region`：截取区域（窗口坐标，物理像素）。
/// - `window`：窗口矩形（原点 0,0）。
/// - `scale`：显示器缩放比。
///
/// # 返回
/// 布局；边框与控制条都不与选区相交（不会进入被截取的画面）。
pub fn compute_scroll_layout(region: PhysicalRect, window: PhysicalRect, scale: f32) -> ScrollLayout {
    let t = (BORDER_LOGICAL * scale).ceil() as i32;
    let strips = [
        PhysicalRect::new(region.x - t, region.y - t, region.width + 2 * t, t),
        PhysicalRect::new(region.x - t, region.bottom(), region.width + 2 * t, t),
        PhysicalRect::new(region.x - t, region.y, t, region.height),
        PhysicalRect::new(region.right(), region.y, t, region.height),
    ];
    let border = strips.iter().filter_map(|s| s.intersect(&window)).collect();
    let size = PhysicalPoint::new(
        (BAR_LOGICAL_WIDTH * scale).round() as i32,
        (BAR_LOGICAL_HEIGHT * scale).round() as i32,
    );
    let margin = (BAR_LOGICAL_MARGIN * scale).round() as i32 + t;
    let pos = calculate_toolbar_placement(region, size, window, margin);
    ScrollLayout {
        border,
        bar: PhysicalRect::new(pos.x, pos.y, size.x, size.y),
    }
}

/// 进度文案：第一行状态，第二行提示。
///
/// # 参数
/// - `progress`：进度快照。
/// - `auto_scroll`：自动滚动是否开启。
pub fn status_lines(progress: &ScrollProgress, auto_scroll: bool) -> (String, String) {
    match &progress.phase {
        ScrollPhase::Capturing => {
            let mode = if auto_scroll { "自动滚动" } else { "请滚动内容" };
            let first = if progress.frames == 0 {
                format!("长截图 · {mode}")
            } else {
                format!("长截图 · {} 帧 · {}×{} · {mode}", progress.frames, progress.width, progress.height)
            };
            let second = progress
                .hint
                .clone()
                .unwrap_or_else(|| "每次滚动不超过半屏；滚完点“完成”".to_string());
            (first, second)
        }
        ScrollPhase::Saving => ("正在保存…".to_string(), String::new()),
        ScrollPhase::Done(done) => {
            let copied = match &done.copied {
                CopyStatus::Copied => "已复制到剪贴板".to_string(),
                CopyStatus::Skipped(why) => format!("未复制（{why}）"),
                CopyStatus::Failed(e) => format!("复制失败（{e}）"),
            };
            (
                format!("完成 · {}×{} · {copied}", done.width, done.height),
                format!("已保存 {} 个文件", done.files.len()),
            )
        }
        ScrollPhase::Failed(reason) => ("长截图失败".to_string(), reason.clone()),
        ScrollPhase::Cancelled => ("已取消".to_string(), String::new()),
    }
}

/// 长截图窗口的视图。
pub struct ScrollAreaView {
    /// 截取区域（窗口坐标，物理像素）。
    region: PhysicalRect,
    /// 窗口（显示器）矩形，原点 0,0。
    window: PhysicalRect,
    /// 显示器缩放比。
    scale: f32,
    /// 共享进度。
    progress: SharedProgress,
    /// 控制开关。
    control: Arc<ScrollControl>,
    /// 用户点了“关闭”。
    dismissed: bool,
    /// 进入终态（完成 / 失败）的时刻。
    ended_at: Option<Instant>,
}

impl ScrollAreaView {
    /// 创建视图。
    ///
    /// # 参数
    /// - `region`：截取区域（窗口坐标）。
    /// - `monitor_bounds`：显示器边界（虚拟桌面坐标，用其宽高作为窗口大小）。
    /// - `scale`：显示器缩放比。
    /// - `progress` / `control`：与采集线程共享的进度与开关。
    pub fn new(
        region: PhysicalRect,
        monitor_bounds: PhysicalRect,
        scale: f32,
        progress: SharedProgress,
        control: Arc<ScrollControl>,
    ) -> Self {
        Self {
            region,
            window: PhysicalRect::new(0, 0, monitor_bounds.width, monitor_bounds.height),
            scale: if scale.is_finite() && scale > 0.0 { scale } else { 1.0 },
            progress,
            control,
            dismissed: false,
            ended_at: None,
        }
    }

    /// 当前布局。
    pub fn layout(&self) -> ScrollLayout {
        compute_scroll_layout(self.region, self.window, self.scale)
    }

    /// 读取进度快照。
    fn snapshot(&self) -> ScrollProgress {
        self.progress.lock().unwrap_or_else(PoisonError::into_inner).clone()
    }

    /// 推进状态：记录进入终态的时刻。
    ///
    /// # 参数
    /// - `now`：当前时刻。
    pub fn advance(&mut self, now: Instant) {
        let terminal = matches!(
            self.snapshot().phase,
            ScrollPhase::Done(_) | ScrollPhase::Failed(_) | ScrollPhase::Cancelled
        );
        if terminal && self.ended_at.is_none() {
            self.ended_at = Some(now);
        }
    }

    /// 窗口是否该关闭：取消、点了关闭，或终态停留够久。
    ///
    /// # 参数
    /// - `now`：当前时刻。
    pub fn is_over(&self, now: Instant) -> bool {
        if self.dismissed {
            return true;
        }
        let phase = self.snapshot().phase;
        match (phase, self.ended_at) {
            (ScrollPhase::Cancelled, _) => true,
            (ScrollPhase::Done(_), Some(t)) => now.saturating_duration_since(t) >= DONE_DISPLAY,
            (ScrollPhase::Failed(_), Some(t)) => now.saturating_duration_since(t) >= ERROR_DISPLAY,
            _ => false,
        }
    }

    /// 完成后的产出（供宿主在窗口关闭时定位文件）。
    pub fn done_file(&self) -> Option<PathBuf> {
        match self.snapshot().phase {
            ScrollPhase::Done(done) => done.files.first().cloned(),
            _ => None,
        }
    }

    /// 物理矩形换算为逻辑像素位置。
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
        cx: &mut Context<Self>,
        on_click: impl Fn(&mut Self) + 'static,
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
                on_click(this);
                cx.notify();
            }))
    }
}

impl Render for ScrollAreaView {
    /// 渲染边框与控制条。
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let progress = self.snapshot();
        let layout = self.layout();
        let accent = match progress.phase {
            ScrollPhase::Done(_) => COLOR_DONE,
            ScrollPhase::Failed(_) => COLOR_FAILED,
            _ => COLOR_CAPTURING,
        };
        let mut root = div().relative().w_full().h_full();
        for strip in &layout.border {
            let (l, t, w, h) = self.logical(*strip);
            root = root.child(div().absolute().left(l).top(t).w(w).h(h).bg(rgba(accent)));
        }
        let auto = self.control.auto_scroll();
        let (first, second) = status_lines(&progress, auto);
        let (l, t, w, h) = self.logical(layout.bar);
        let mut buttons = div().flex().items_center().gap_2();
        match progress.phase {
            ScrollPhase::Capturing => {
                let control = Arc::clone(&self.control);
                let toggle = Arc::clone(&self.control);
                buttons = buttons
                    .child(self.button(
                        "scroll-auto",
                        if auto { "自动滚动：开" } else { "自动滚动：关" },
                        if auto { 0x1677FFFF } else { 0x303030FF },
                        0xFFFFFFFF,
                        cx,
                        move |_| toggle.set_auto_scroll(!toggle.auto_scroll()),
                    ))
                    .child(self.button("scroll-finish", "完成", COLOR_CAPTURING, 0xFFFFFFFF, cx, move |_| {
                        control.request_finish()
                    }));
                let cancel = Arc::clone(&self.control);
                buttons = buttons.child(self.button("scroll-cancel", "取消", 0x434343FF, 0xFF4D4FFF, cx, move |_| {
                    cancel.request_cancel()
                }));
            }
            ScrollPhase::Saving => {}
            _ => {
                buttons = buttons.child(self.button("scroll-close", "关闭", 0x434343FF, 0xFFFFFFFF, cx, |this| {
                    this.dismissed = true
                }));
            }
        }
        root.child(
            div()
                .absolute()
                .left(l)
                .top(t)
                .w(w)
                .h(h)
                .flex()
                .items_center()
                .justify_between()
                .gap_2()
                .px_3()
                .rounded_lg()
                .bg(rgba(0x1F1F1FEE))
                .border_1()
                .border_color(rgba(0xFFFFFF26))
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .child(
                            div()
                                .text_size(px(13.0))
                                .font_weight(FontWeight::MEDIUM)
                                .text_color(rgba(0xFFFFFFFF))
                                .child(first),
                        )
                        .child(div().text_size(px(11.0)).text_color(rgba(0xBFBFBFFF)).child(second)),
                )
                .child(buttons),
        )
    }
}

/// 输出通道：真实剪贴板 + 按截图保存配置落盘的文件。
struct SystemScrollSink {
    /// 导出配置快照（目录 / 文件名模板 / 格式 / 质量）。
    settings: ExportSettings,
    /// 界面语言代码（失败提示用）。
    locale: String,
}

impl ScrollSink for SystemScrollSink {
    /// 复制到系统剪贴板。
    fn copy_image(&mut self, width: u32, height: u32, rgba: &[u8]) -> Result<(), String> {
        copy_image_to_clipboard(width, height, rgba)
    }

    /// 按配置保存（长截图的图像不透明，直接交给自动保存）。
    fn save_png(&mut self, width: u32, height: u32, rgba: &[u8]) -> Result<PathBuf, String> {
        save_automatic(
            &self.settings,
            &ExportOverrides::default(),
            width,
            height,
            rgba,
            home_directory().as_deref(),
            snow_platform::local_time::now(),
        )
        .map_err(|e| e.manual_message(&self.locale))
    }
}

/// 正在进行的一次长截图。
struct ActiveScroll {
    /// 窗口。
    window: ShellWindow,
    /// 视图。
    view: Entity<ScrollAreaView>,
    /// 已应用到窗口的命中区域。
    applied_hit: Vec<PhysicalRect>,
    /// 结束后是否在资源管理器里定位文件。
    reveal: bool,
}

/// 长截图宿主：持有窗口与采集线程句柄。
pub struct ScrollHost {
    /// 平台能力表。
    caps: CapabilityRegistry,
    /// 主线程收件箱。
    inbox: MainThreadInbox<UiEvent>,
    /// 共享配置存储（保存目录）。
    config: SharedConfig,
    /// 当前会话。
    active: Option<ActiveScroll>,
}

impl ScrollHost {
    /// 创建宿主。
    ///
    /// # 参数
    /// - `caps`：能力表。
    /// - `inbox`：主线程收件箱。
    /// - `config`：共享配置存储。
    pub fn new(caps: CapabilityRegistry, inbox: MainThreadInbox<UiEvent>, config: SharedConfig) -> Self {
        Self {
            caps,
            inbox,
            config,
            active: None,
        }
    }

    /// 是否有长截图窗仍在运行。
    pub fn is_busy(&self, cx: &ShellContext) -> bool {
        self.active.as_ref().is_some_and(|a| cx.is_window_open(&a.window))
    }

    /// 开始一次长截图：启动采集线程并打开控制窗。
    ///
    /// # 参数
    /// - `cx`：外壳上下文。
    /// - `region`：截取区域（虚拟桌面物理坐标）。
    /// - `monitor`：区域所在显示器。
    /// - `autotest`：自动化参数（验收用）。
    pub fn begin(&mut self, cx: &mut ShellContext, region: PhysicalRect, monitor: &MonitorInfo, autotest: Option<&ScrollAutotest>) {
        if self.is_busy(cx) {
            tracing::info!("已有长截图在进行，忽略新的请求");
            return;
        }
        let (mut settings, locale) = {
            let store = self.config.borrow();
            (
                ExportSettings::from_document(store.document()),
                crate::app_runtime::interface_locale(store.document()),
            )
        };
        if let Some(dir) = autotest
            .and_then(|_| std::env::var_os(ENV_SCROLL_AUTOTEST_DIR))
            .filter(|d| !d.is_empty())
        {
            settings = settings.with_directory(&PathBuf::from(dir));
        }
        let progress: SharedProgress = Arc::new(Mutex::new(ScrollProgress::new()));
        let control = Arc::new(ScrollControl::default());
        control.set_auto_scroll(autotest.is_some_and(|a| a.auto_scroll));
        self.spawn_worker(region, Arc::clone(&progress), Arc::clone(&control), settings, locale, autotest.and_then(|a| a.stop_after_secs));

        let bounds = monitor.bounds;
        let window_region = PhysicalRect::new(region.x - bounds.x, region.y - bounds.y, region.width, region.height);
        let view = ScrollAreaView::new(window_region, bounds, monitor.scale.value(), progress, control);
        let mut spec = WindowSpec::overlay(MonitorTarget::Id(monitor.id));
        spec.focus = false;
        match cx.open_window(&spec, move |_window, app| app.new(|_| view)) {
            Ok((window, view)) => {
                self.configure_window(&window);
                self.active = Some(ActiveScroll {
                    window,
                    view: view.clone(),
                    applied_hit: Vec::new(),
                    reveal: autotest.is_none(),
                });
                self.spawn_ticker(cx, view);
                self.sync(cx);
            }
            Err(e) => tracing::error!(error = %e, "打开长截图窗失败"),
        }
    }

    /// 启动采集线程（读屏 + 拼接 + 输出）。
    fn spawn_worker(
        &self,
        region: PhysicalRect,
        progress: SharedProgress,
        control: Arc<ScrollControl>,
        settings: ExportSettings,
        locale: String,
        stop_after: Option<u64>,
    ) {
        let inbox = self.inbox.clone();
        let center = (region.x + region.width / 2, region.y + region.height / 2);
        let spawned = std::thread::Builder::new().name("snow-scroll-capture".into()).spawn(move || {
            if let Some(secs) = stop_after {
                let stopper = Arc::clone(&control);
                std::thread::spawn(move || {
                    std::thread::sleep(Duration::from_secs(secs));
                    stopper.request_finish();
                });
            }
            let scroller: AutoScroller = Box::new(move || post_wheel(center, AUTO_SCROLL_DELTA));
            let mut sink = SystemScrollSink { settings, locale };
            let wake_inbox = inbox.clone();
            run_capture(
                ScreenSource::new((region.x, region.y, region.width.max(0) as u32, region.height.max(0) as u32)),
                &control,
                &progress,
                &move || {
                    wake_inbox.push(UiEvent::ScrollTick);
                },
                Some(scroller),
                &mut sink,
                CaptureOptions {
                    copy_to_clipboard: true,
                    timing: CaptureTiming::default(),
                },
            );
            tracing::info!("长截图采集线程结束");
            inbox.push(UiEvent::ScrollTick);
        });
        if let Err(e) = spawned {
            tracing::error!(error = %e, "无法创建长截图采集线程");
        }
    }

    /// 窗口自身不进成图；命中区域稍后由 [`ScrollHost::sync`] 设置。
    fn configure_window(&self, window: &ShellWindow) {
        if std::env::var_os(ENV_SCROLL_KEEP_VISIBLE).is_some() {
            tracing::warn!("调试：长截图窗保持可被捕获（不排除）");
            return;
        }
        match window.overlay(&self.caps) {
            Ok(overlay) => {
                if let Err(e) = overlay.set_capture_excluded(true) {
                    tracing::warn!(error = %e, "长截图窗未能排除在捕获之外，控制条可能进入成图");
                }
            }
            Err(e) => tracing::warn!(error = %e, "无法取得长截图窗原生句柄"),
        }
    }

    /// 启动定时器：周期性向收件箱投递 `ScrollTick`，窗口消失后自动退出。
    fn spawn_ticker(&self, cx: &mut ShellContext, view: Entity<ScrollAreaView>) {
        let inbox = self.inbox.clone();
        let weak = view.downgrade();
        cx.app()
            .spawn(async move |acx| {
                loop {
                    acx.background_executor().timer(TICK_INTERVAL).await;
                    if weak.update(acx, |_, _| ()).is_err() || !inbox.push(UiEvent::ScrollTick) {
                        return;
                    }
                }
            })
            .detach();
    }

    /// 推进窗口状态、同步命中区域，并处理结束收尾。
    ///
    /// # 参数
    /// - `cx`：外壳上下文。
    pub fn sync(&mut self, cx: &mut ShellContext) {
        let Some(active) = self.active.as_mut() else {
            return;
        };
        let now = Instant::now();
        let (rects, over) = active.view.update(cx.app(), |v, vcx| {
            v.advance(now);
            vcx.notify();
            (v.layout().hit_rects(), v.is_over(now))
        });
        if rects != active.applied_hit {
            let mut region = Region::new();
            for rect in &rects {
                region.union_rect(*rect);
            }
            match active.window.overlay(&self.caps) {
                Ok(mut overlay) => {
                    if let Err(e) = overlay.set_hit_region(&region) {
                        tracing::warn!(error = %e, "设置长截图窗命中区域失败");
                    }
                    active.applied_hit = rects;
                }
                Err(e) => tracing::warn!(error = %e, "无法取得长截图窗句柄"),
            }
        }
        if !over {
            return;
        }
        let window = active.window;
        let reveal = active.reveal;
        let file = active.view.read(cx.app()).done_file();
        self.active = None;
        window.close(cx.app());
        match file {
            Some(path) => {
                tracing::info!(path = %path.display(), "长截图完成");
                if reveal && let Err(e) = snow_platform::shell::reveal_in_explorer(&path) {
                    tracing::warn!(error = %e, "无法在资源管理器中定位长截图文件");
                }
            }
            None => tracing::info!("长截图窗已关闭（取消或失败）"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scroll_capture::ScrollDone;

    /// 自动化参数解析：最小形态、含 auto、含秒数；非法输入被拒绝。
    #[test]
    fn autotest_parsing() {
        let t = parse_scroll_autotest("10,20,800,600").unwrap();
        assert_eq!(t.region, PhysicalRect::new(10, 20, 800, 600));
        assert!(!t.auto_scroll && t.stop_after_secs.is_none());
        let t = parse_scroll_autotest("0,0,100,100,auto,15").unwrap();
        assert!(t.auto_scroll);
        assert_eq!(t.stop_after_secs, Some(15));
        let t = parse_scroll_autotest("0,0,100,100,8").unwrap();
        assert_eq!(t.stop_after_secs, Some(8));
        for bad in ["", "1,2,3", "a,b,c,d", "0,0,0,10", "0,0,10,10,0", "0,0,10,10,xx"] {
            assert_eq!(parse_scroll_autotest(bad), None, "应拒绝 {bad:?}");
        }
    }

    /// 边框与控制条都不侵入选区；选区贴屏幕边时边框被裁；控制条落在窗口内。
    #[test]
    fn layout_stays_outside_region() {
        let win = PhysicalRect::new(0, 0, 1920, 1080);
        let region = PhysicalRect::new(300, 200, 600, 400);
        let l = compute_scroll_layout(region, win, 1.0);
        assert_eq!(l.border.len(), 4);
        for r in l.hit_rects() {
            assert!(r.intersect(&region).is_none(), "{r:?} 侵入选区");
        }
        assert_eq!(win.intersect(&l.bar), Some(l.bar));
        let full = compute_scroll_layout(win, win, 1.0);
        assert!(full.border.is_empty());
        let l = compute_scroll_layout(PhysicalRect::new(0, 100, 500, 400), win, 1.5);
        assert!(l.border.iter().all(|s| win.intersect(s) == Some(*s)));
    }

    /// 文案：采集中 / 保存中 / 完成 / 失败各不相同，提示优先于默认引导。
    #[test]
    fn status_text_per_phase() {
        let mut p = ScrollProgress::new();
        let (first, second) = status_lines(&p, false);
        assert!(first.contains("请滚动内容") && second.contains("完成"));
        p.frames = 3;
        p.width = 100;
        p.height = 400;
        p.hint = Some("滚动太快".into());
        let (first, second) = status_lines(&p, true);
        assert!(first.contains("3 帧") && first.contains("100×400") && first.contains("自动滚动"));
        assert_eq!(second, "滚动太快");
        p.phase = ScrollPhase::Saving;
        assert_eq!(status_lines(&p, false).0, "正在保存…");
        p.phase = ScrollPhase::Done(ScrollDone {
            width: 100,
            height: 400,
            files: vec![PathBuf::from("a.png"), PathBuf::from("b.png")],
            copied: CopyStatus::Skipped("太大".into()),
        });
        let (first, second) = status_lines(&p, false);
        assert!(first.contains("未复制（太大）") && second.contains("2 个文件"));
        p.phase = ScrollPhase::Failed("拒绝访问".into());
        assert_eq!(status_lines(&p, false).1, "拒绝访问");
    }

    /// 视图状态机：取消立即结束；完成后停留一段时间才结束；点关闭立即结束。
    #[test]
    fn view_lifecycle() {
        let progress: SharedProgress = Arc::new(Mutex::new(ScrollProgress::new()));
        let control = Arc::new(ScrollControl::default());
        let mut view = ScrollAreaView::new(
            PhysicalRect::new(100, 100, 300, 200),
            PhysicalRect::new(0, 0, 1920, 1080),
            1.0,
            Arc::clone(&progress),
            control,
        );
        let t0 = Instant::now();
        view.advance(t0);
        assert!(!view.is_over(t0));
        progress.lock().unwrap().phase = ScrollPhase::Done(ScrollDone {
            width: 1,
            height: 1,
            files: vec![PathBuf::from("x.png")],
            copied: CopyStatus::Copied,
        });
        view.advance(t0);
        assert!(!view.is_over(t0));
        assert!(view.is_over(t0 + DONE_DISPLAY));
        assert_eq!(view.done_file(), Some(PathBuf::from("x.png")));
        progress.lock().unwrap().phase = ScrollPhase::Cancelled;
        assert!(view.is_over(t0));
        progress.lock().unwrap().phase = ScrollPhase::Capturing;
        view.dismissed = true;
        assert!(view.is_over(t0));
    }
}
