//! 截图智能选区：鼠标悬停时识别其下的窗口 / 控件层级路径（交互对齐 Qt 版）。
//!
//! 命中查询放在专用后台线程（COM 对象只能在创建它的线程使用），
//! UI 线程只投递坐标并读取最新结果；过期请求在邮箱里被新请求覆盖。
//! 命中路径自深到浅排列（最深控件在前，顶层窗口在最后），滚轮 / 快捷键在其中切换层级。

use snow_config::document::ConfigDocument;
use snow_ui::shell::geometry::{PhysicalPoint, PhysicalRect};
use std::sync::{Condvar, Mutex};
use std::time::{Duration, Instant};

/// 拖拽阈值（逻辑像素）：按下后位移不到该值视为单击，超过则转为手动框选（对齐 Qt 的 startDragDistance）。
pub const DRAG_THRESHOLD_LOGICAL: f32 = 10.0;

/// UI 线程等待命中结果的最长时间；超时则沿用上一次结果，避免卡住鼠标移动。
pub const HOVER_WAIT: Duration = Duration::from_millis(3);

/// 「自动识别窗口范围」开关的配置键（沿用旧版 `screenshot_selection/smart_selection`，默认开）。
pub const SMART_SELECTION_KEY: &str = "screenshot_selection/smart_selection";

/// 选区目标的配置键（`window` / `window_sub_element`，旧版同键）。
pub const SELECTION_TARGET_KEY: &str = "screenshot_selection/selection_target";

/// 选区目标的配置取值：整个窗口。
const TARGET_WINDOW: &str = "window";

/// 选区目标的配置取值：窗口内的子控件。
const TARGET_SUB_ELEMENT: &str = "window_sub_element";

/// 过渡动画时长（对齐 Qt `kDurationMs`）。
pub const TRANSITION_DURATION: Duration = Duration::from_millis(101);

/// 「选区过渡动画」开关的配置键。
pub const TRANSITION_ANIMATION_KEY: &str = "screenshot_ui/selection_transition_animation";

/// 智能选区的目标层级。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PickTarget {
    /// 整个顶层窗口（路径最后一项，滚轮无效）。
    Window,
    /// 窗口内的子控件（默认取最深一项，滚轮可向外 / 向内切换）。
    WindowSubElement,
}

impl PickTarget {
    /// 从配置值解析；未知值按旧版默认 `window_sub_element`。
    ///
    /// ```ignore
    /// assert_eq!(PickTarget::from_config("window"), PickTarget::Window);
    /// ```
    pub fn from_config(raw: &str) -> Self {
        if raw == TARGET_WINDOW {
            Self::Window
        } else {
            Self::WindowSubElement
        }
    }

    /// 对应的配置值。
    pub const fn as_config(self) -> &'static str {
        match self {
            Self::Window => TARGET_WINDOW,
            Self::WindowSubElement => TARGET_SUB_ELEMENT,
        }
    }

    /// 切换到另一个目标。
    pub const fn toggled(self) -> Self {
        match self {
            Self::Window => Self::WindowSubElement,
            Self::WindowSubElement => Self::Window,
        }
    }
}

/// 读取选区目标配置。
///
/// # 参数
/// - `document`：配置文档。
///
/// # 返回
/// 配置的目标；缺失或类型不对按默认 `WindowSubElement`。
pub fn selection_target(document: &ConfigDocument) -> PickTarget {
    document
        .value(SELECTION_TARGET_KEY)
        .as_str()
        .map_or(PickTarget::WindowSubElement, PickTarget::from_config)
}

/// 读取「选区过渡动画」开关，缺失按默认开启。
pub fn transition_animation_enabled(document: &ConfigDocument) -> bool {
    document
        .value(TRANSITION_ANIMATION_KEY)
        .as_bool()
        .unwrap_or(true)
}

/// 读取「自动识别窗口范围」开关。
///
/// # 参数
/// - `document`：配置文档。
///
/// # 返回
/// 开启返回 `true`；值缺失或类型不对时按默认开启。
///
/// ```ignore
/// if smart_selection_enabled(store.document()) { /* 启动命中线程 */ }
/// ```
pub fn smart_selection_enabled(document: &ConfigDocument) -> bool {
    document
        .value(SMART_SELECTION_KEY)
        .as_bool()
        .unwrap_or(true)
}

/// 按配置启动窗口悬停来源；开关关闭、非 Windows 或启动失败时返回 `None`。
///
/// # 参数
/// - `document`：配置文档。
/// - `monitor`：覆盖窗所在显示器的桌面物理范围。
///
/// # 返回
/// 悬停来源；调用应发生在覆盖窗显示之前，以便快照不含覆盖窗。
///
/// ```ignore
/// let hover = start_window_hover(store.document(), monitor.bounds);
/// ```
pub fn start_window_hover(
    document: &ConfigDocument,
    monitor: PhysicalRect,
) -> Option<Box<dyn WindowHover>> {
    if !smart_selection_enabled(document) {
        return None;
    }
    #[cfg(windows)]
    {
        match WindowPicker::spawn(monitor, Vec::new()) {
            Ok(picker) => Some(Box::new(picker)),
            Err(e) => {
                tracing::warn!(error = %e, "启动窗口命中线程失败，智能选区不可用");
                None
            }
        }
    }
    #[cfg(not(windows))]
    {
        let _ = monitor;
        None
    }
}

/// 悬停来源：给定覆盖窗（底图）坐标，返回其下窗口 / 控件的层级路径。
pub trait WindowHover {
    /// 查询指定点下的层级路径。
    ///
    /// # 参数
    /// - `point`：底图物理坐标（原点为显示器左上角）。
    /// - `target`：目标层级；`Window` 只需窗口，`WindowSubElement` 需要控件级路径。
    ///
    /// # 返回
    /// 底图坐标路径，自深到浅（最深控件在前，顶层窗口最后）；没有命中或结果未就绪时为 `None`。
    ///
    /// ```ignore
    /// let path = source.hover(PhysicalPoint::new(300, 200), PickTarget::WindowSubElement);
    /// ```
    fn hover(&mut self, point: PhysicalPoint, target: PickTarget) -> Option<Vec<PhysicalRect>>;

    /// 取走后台细化出的更完整路径（UIA 异步补出更深层级）。
    ///
    /// # 返回
    /// 最近一次悬停点的细化路径；没有新结果时为 `None`。默认实现不提供细化。
    fn refinement(&mut self) -> Option<Vec<PhysicalRect>> {
        None
    }

    /// 后台是否仍在细化（为真时调用方应继续请求重绘以便及时拿到结果）。
    fn refinement_pending(&self) -> bool {
        false
    }
}

/// 把桌面坐标的窗口矩形换算成底图坐标并裁剪到显示器范围。
///
/// # 参数
/// - `screen_rect`：窗口矩形（虚拟桌面物理坐标，可含负值）。
/// - `monitor`：覆盖窗所在显示器的桌面范围。
///
/// # 返回
/// 底图坐标矩形；与显示器无交集或为空时返回 `None`。
///
/// ```ignore
/// let r = screen_rect_to_frame(PhysicalRect::new(-1800, 10, 400, 300), monitor);
/// ```
pub fn screen_rect_to_frame(
    screen_rect: PhysicalRect,
    monitor: PhysicalRect,
) -> Option<PhysicalRect> {
    let left = screen_rect.x.max(monitor.x) - monitor.x;
    let top = screen_rect.y.max(monitor.y) - monitor.y;
    let right = screen_rect.right().min(monitor.right()) - monitor.x;
    let bottom = screen_rect.bottom().min(monitor.bottom()) - monitor.y;
    (right > left && bottom > top).then(|| PhysicalRect::new(left, top, right - left, bottom - top))
}

/// 判断按下后的位移是否达到拖拽阈值（含等于）。
///
/// # 参数
/// - `start`：按下位置。
/// - `current`：当前位置。
/// - `threshold`：阈值（物理像素）；非正数表示任何位移都算拖拽。
///
/// # 返回
/// 达到阈值返回 `true`。
///
/// ```ignore
/// assert!(exceeds_drag_threshold(PhysicalPoint::new(0, 0), PhysicalPoint::new(10, 0), 10));
/// ```
pub fn exceeds_drag_threshold(
    start: PhysicalPoint,
    current: PhysicalPoint,
    threshold: i32,
) -> bool {
    if threshold <= 0 {
        return true;
    }
    let dx = i64::from(current.x - start.x);
    let dy = i64::from(current.y - start.y);
    let limit = i64::from(threshold);
    dx * dx + dy * dy >= limit * limit
}

/// 路径里的一层矩形序列（底图物理坐标，自深到浅）。
pub type PickRects = Vec<PhysicalRect>;

/// 两个矩形求交；无交集返回 `None`。
fn intersect(a: PhysicalRect, b: PhysicalRect) -> Option<PhysicalRect> {
    let left = a.x.max(b.x);
    let top = a.y.max(b.y);
    let right = a.right().min(b.right());
    let bottom = a.bottom().min(b.bottom());
    (right > left && bottom > top).then(|| PhysicalRect::new(left, top, right - left, bottom - top))
}

/// 把命中路径裁到可选范围，丢掉小于最小尺寸的层，并合并相邻重复层。
fn bounded_path(path: &[PhysicalRect], bounds: PhysicalRect, min_size: i32) -> PickRects {
    let mut out: PickRects = Vec::with_capacity(path.len());
    for rect in path {
        let Some(b) = intersect(*rect, bounds) else {
            continue;
        };
        if b.width < min_size || b.height < min_size || out.last() == Some(&b) {
            continue;
        }
        out.push(b);
    }
    out
}

/// 智能选区的层级模型（对应 Qt `ScreenshotIntelligentSelectionModel` 的命中路径部分）。
///
/// 路径自深到浅；`Window` 目标固定选最后一层，`WindowSubElement` 目标按索引选，
/// 滚轮向上向外（索引 +1），向下向内（索引 −1）。
#[derive(Debug, Clone)]
pub struct PickPath {
    /// 当前目标层级。
    target: PickTarget,
    /// 命中路径（自深到浅）。
    rects: PickRects,
    /// 当前选中的层；路径为空时为 `None`。
    index: Option<usize>,
    /// 用户是否用滚轮显式选过层（细化路径到来时据此保持选择）。
    explicit: bool,
}

impl PickPath {
    /// 创建空模型。
    ///
    /// # 参数
    /// - `target`：初始目标层级。
    pub fn new(target: PickTarget) -> Self {
        Self {
            target,
            rects: Vec::new(),
            index: None,
            explicit: false,
        }
    }

    /// 当前目标层级。
    pub fn target(&self) -> PickTarget {
        self.target
    }

    /// 清空路径与选择（目标层级保留）。
    pub fn clear(&mut self) {
        self.rects.clear();
        self.index = None;
        self.explicit = false;
    }

    /// 当前选中的矩形。
    pub fn current(&self) -> Option<PhysicalRect> {
        self.index.and_then(|i| self.rects.get(i)).copied()
    }

    /// 当前选中层的索引。
    pub fn index(&self) -> Option<usize> {
        self.index
    }

    /// 目标层级对应的默认索引。
    fn default_index(&self) -> usize {
        match self.target {
            PickTarget::Window => self.rects.len().saturating_sub(1),
            PickTarget::WindowSubElement => 0,
        }
    }

    /// 按目标规则落定索引：`Window` 恒为最后一层，子控件目标夹在有效范围内。
    fn set_index(&mut self, index: i64) -> bool {
        if self.rects.is_empty() {
            self.clear();
            return false;
        }
        let max = self.rects.len() - 1;
        self.index = Some(match self.target {
            PickTarget::Window => max,
            PickTarget::WindowSubElement => index.clamp(0, max as i64) as usize,
        });
        true
    }

    /// 应用一次新的命中路径。
    ///
    /// # 参数
    /// - `path`：命中路径（底图物理坐标，自深到浅）。
    /// - `bounds`：可选范围（底图）。
    /// - `min_size`：最小选区边长，小于它的层被丢弃。
    ///
    /// # 返回
    /// 是否存在当前选区；路径为空返回 `false`。路径与上次完全一致时保留已有选择。
    pub fn apply_hit_path(
        &mut self,
        path: &[PhysicalRect],
        bounds: PhysicalRect,
        min_size: i32,
    ) -> bool {
        let bounded = bounded_path(path, bounds, min_size);
        if bounded.is_empty() {
            self.clear();
            return false;
        }
        if bounded == self.rects {
            return self.set_index(self.index.map_or(0, |i| i as i64));
        }
        self.rects = bounded;
        self.explicit = false;
        let index = self.default_index();
        self.set_index(index as i64)
    }

    /// 滚轮切换层级：`step > 0` 向外（更浅），`step < 0` 向内（更深）。
    ///
    /// # 返回
    /// 是否存在当前选区；`Window` 目标下层级固定，选择不变。
    pub fn select_step(&mut self, step: i64) -> bool {
        self.explicit = true;
        let base = self.index.map_or(0, |i| i as i64);
        self.set_index(base + step)
    }

    /// 切换目标层级并重置到该目标的默认层。
    ///
    /// # 返回
    /// 切换后的目标。
    pub fn toggle_target(&mut self) -> PickTarget {
        self.target = self.target.toggled();
        self.explicit = false;
        let index = self.default_index();
        self.set_index(index as i64);
        self.target
    }

    /// 应用后台细化出的更完整路径：仅当它在最浅端与现有路径完全吻合、且更深处多出新层时才接受。
    ///
    /// # 参数
    /// - `path` / `bounds` / `min_size`：同 [`Self::apply_hit_path`]。
    ///
    /// # 返回
    /// 是否替换了路径。非控件目标、路径更短或不吻合时返回 `false`。
    pub fn apply_refinement(
        &mut self,
        path: &[PhysicalRect],
        bounds: PhysicalRect,
        min_size: i32,
    ) -> bool {
        if self.target != PickTarget::WindowSubElement || self.rects.is_empty() {
            return false;
        }
        let refined = bounded_path(path, bounds, min_size);
        if refined.len() <= self.rects.len() {
            return false;
        }
        let added = refined.len() - self.rects.len();
        if refined[added..] != self.rects[..] {
            return false;
        }
        let selected = self.current();
        self.rects = refined;
        let index = if self.explicit {
            selected
                .and_then(|s| self.rects.iter().position(|r| *r == s))
                .unwrap_or(0)
        } else {
            0
        };
        self.set_index(index as i64)
    }
}

/// 把 `from` 到 `to` 按进度 `t`（0~1）线性插值成整数矩形。
fn lerp_rect(from: PhysicalRect, to: PhysicalRect, t: f32) -> PhysicalRect {
    let mix = |a: i32, b: i32| a + ((b - a) as f32 * t).round() as i32;
    PhysicalRect::new(
        mix(from.x, to.x),
        mix(from.y, to.y),
        mix(from.width, to.width),
        mix(from.height, to.height),
    )
}

/// 智能选区高亮框的过渡动画（对应 Qt `ScreenshotSmartSelectionTransition`：101ms、OutQuad）。
///
/// 第一个智能选区直接呈现；之后目标变化时从当前显示的矩形平滑过渡到新矩形。
#[derive(Debug, Clone)]
pub struct HighlightTransition {
    /// 是否启用动画；关闭时一律直接呈现。
    enabled: bool,
    /// 当前显示的矩形（动画进行中是插值结果）。
    displayed: Option<PhysicalRect>,
    /// 动画目标。
    target: Option<PhysicalRect>,
    /// 动画起点与起始时刻；`None` 表示没有在播放。
    running: Option<(PhysicalRect, Instant)>,
    /// 是否已呈现过智能选区（第一个不做动画）。
    presented: bool,
}

impl HighlightTransition {
    /// 创建过渡器。
    ///
    /// # 参数
    /// - `enabled`：是否启用动画。
    pub fn new(enabled: bool) -> Self {
        Self {
            enabled,
            displayed: None,
            target: None,
            running: None,
            presented: false,
        }
    }

    /// 喂入最新目标矩形并返回此刻应显示的矩形。
    ///
    /// # 参数
    /// - `target`：目标高亮矩形；`None` 表示没有高亮（清空并重置「首个」状态）。
    /// - `now`：当前时刻。
    ///
    /// # 返回
    /// 此刻显示的矩形（动画进行中为插值）。
    ///
    /// ```ignore
    /// let shown = transition.advance(view.window_highlight(), Instant::now());
    /// ```
    pub fn advance(&mut self, target: Option<PhysicalRect>, now: Instant) -> Option<PhysicalRect> {
        let Some(to) = target else {
            *self = Self::new(self.enabled);
            return None;
        };
        if !self.enabled || !self.presented {
            self.presented = true;
            self.running = None;
            self.displayed = Some(to);
            self.target = Some(to);
            return self.displayed;
        }
        if self.target != Some(to) {
            self.running = self.displayed.map(|d| (d, now));
            self.target = Some(to);
        }
        if let Some((from, started)) = self.running {
            let t = (now.saturating_duration_since(started).as_secs_f32()
                / TRANSITION_DURATION.as_secs_f32())
            .clamp(0.0, 1.0);
            if t >= 1.0 {
                self.running = None;
                self.displayed = Some(to);
            } else {
                let eased = 1.0 - (1.0 - t) * (1.0 - t);
                self.displayed = Some(lerp_rect(from, to, eased));
            }
        }
        self.displayed
    }

    /// 动画是否仍在播放（需要继续请求下一帧）。
    pub fn is_running(&self) -> bool {
        self.running.is_some()
    }
}

/// 邮箱内部状态。
#[derive(Default)]
struct MailboxState {
    /// 最近一次提交的请求序号。
    last_seq: u64,
    /// 尚未被工作线程取走的请求（新请求覆盖旧请求）。
    pending: Option<(u64, PhysicalPoint, PickTarget)>,
    /// 最新已发布的结果（序号, 路径）。
    result: Option<(u64, Option<PickRects>)>,
    /// 最新的细化路径（序号, 路径）；被取走后清空。
    refined: Option<(u64, PickRects)>,
    /// 工作线程是否正在做细化查询。
    refining: bool,
    /// 是否已关闭（工作线程据此退出）。
    closed: bool,
}

/// UI 线程与命中线程之间的"最新请求 / 最新结果"邮箱：只保留最新请求，丢弃过期结果。
#[derive(Default)]
pub struct HoverMailbox {
    /// 受保护的状态。
    state: Mutex<MailboxState>,
    /// 请求或结果变化的通知。
    changed: Condvar,
}

impl HoverMailbox {
    /// 创建空邮箱。
    pub fn new() -> Self {
        Self::default()
    }

    /// 提交一个查询请求，覆盖尚未被取走的旧请求，并作废旧的细化结果。
    ///
    /// # 返回
    /// 本次请求序号（严格递增）。
    pub fn submit(&self, point: PhysicalPoint, target: PickTarget) -> u64 {
        let mut s = self.state.lock().unwrap_or_else(|e| e.into_inner());
        s.last_seq += 1;
        let seq = s.last_seq;
        s.pending = Some((seq, point, target));
        s.refined = None;
        self.changed.notify_all();
        seq
    }

    /// 阻塞等待下一个请求；邮箱关闭后返回 `None`。
    pub fn next_request(&self) -> Option<(u64, PhysicalPoint, PickTarget)> {
        let mut s = self.state.lock().unwrap_or_else(|e| e.into_inner());
        loop {
            if s.closed {
                return None;
            }
            if let Some(req) = s.pending.take() {
                return Some(req);
            }
            s = self.changed.wait(s).unwrap_or_else(|e| e.into_inner());
        }
    }

    /// 是否已有更新的请求排队或邮箱已关闭（细化查询据此取消）。
    pub fn superseded(&self) -> bool {
        let s = self.state.lock().unwrap_or_else(|e| e.into_inner());
        s.closed || s.pending.is_some()
    }

    /// 发布一次查询结果；序号比已发布结果更旧时丢弃。
    pub fn publish(&self, seq: u64, path: Option<PickRects>) {
        let mut s = self.state.lock().unwrap_or_else(|e| e.into_inner());
        if s.result.as_ref().is_none_or(|(old, _)| seq >= *old) {
            s.result = Some((seq, path));
        }
        self.changed.notify_all();
    }

    /// 发布细化路径；已有更新的请求时丢弃。
    pub fn publish_refinement(&self, seq: u64, path: PickRects) {
        let mut s = self.state.lock().unwrap_or_else(|e| e.into_inner());
        if seq == s.last_seq {
            s.refined = Some((seq, path));
        }
    }

    /// 标记后台细化开始 / 结束。
    pub fn set_refining(&self, refining: bool) {
        let mut s = self.state.lock().unwrap_or_else(|e| e.into_inner());
        s.refining = refining;
    }

    /// 后台是否正在细化，或还有结果没被取走。
    pub fn refinement_pending(&self) -> bool {
        let s = self.state.lock().unwrap_or_else(|e| e.into_inner());
        s.refining || s.refined.is_some()
    }

    /// 取走细化路径。
    pub fn take_refinement(&self) -> Option<PickRects> {
        let mut s = self.state.lock().unwrap_or_else(|e| e.into_inner());
        s.refined.take().map(|(_, path)| path)
    }

    /// 最新已发布的结果（可能对应更早的请求）。
    pub fn latest(&self) -> Option<Option<PickRects>> {
        let s = self.state.lock().unwrap_or_else(|e| e.into_inner());
        s.result.as_ref().map(|(_, path)| path.clone())
    }

    /// 等待序号不小于 `seq` 的结果，最多等 `timeout`。
    ///
    /// # 返回
    /// 结果已就绪返回 `Some(路径)`；超时返回 `None`。
    pub fn wait_result(&self, seq: u64, timeout: Duration) -> Option<Option<PickRects>> {
        let s = self.state.lock().unwrap_or_else(|e| e.into_inner());
        let (s, _) = self
            .changed
            .wait_timeout_while(s, timeout, |s| {
                !s.closed && s.result.as_ref().is_none_or(|(got, _)| *got < seq)
            })
            .unwrap_or_else(|e| e.into_inner());
        s.result
            .as_ref()
            .filter(|(got, _)| *got >= seq)
            .map(|(_, path)| path.clone())
    }

    /// 关闭邮箱，唤醒工作线程退出。
    pub fn close(&self) {
        let mut s = self.state.lock().unwrap_or_else(|e| e.into_inner());
        s.closed = true;
        self.changed.notify_all();
    }
}

#[cfg(windows)]
pub use native::WindowPicker;

/// Windows 实现：专用线程持有 `ElementRegionService`，覆盖窗显示前即完成窗口快照。
#[cfg(windows)]
mod native {
    use super::{
        HOVER_WAIT, HoverMailbox, PickRects, PickTarget, WindowHover, screen_rect_to_frame,
    };
    use snow_ui::shell::geometry::{PhysicalPoint, PhysicalRect};
    use snow_ui_selector::{
        AccessibilityBackend, ElementRect, ElementRegionService, HitTestMode, Point, QueryControl,
        StopReason,
    };
    use std::sync::Arc;

    /// 命中线程名称。
    const PICKER_THREAD_NAME: &str = "snow-window-pick";

    /// 窗口悬停命中器：句柄由 UI 线程持有，查询在后台线程执行。
    pub struct WindowPicker {
        /// 与工作线程共享的邮箱。
        mailbox: Arc<HoverMailbox>,
        /// 最近一次查询的点、目标与结果，用于同一点去重。
        last: Option<(PhysicalPoint, PickTarget, Option<PickRects>)>,
    }

    impl WindowPicker {
        /// 启动命中线程并立即抓取窗口快照（应在覆盖窗显示之前调用）。
        ///
        /// # 参数
        /// - `monitor`：覆盖窗所在显示器的桌面物理范围，用于坐标换算。
        /// - `excluded`：需排除的窗口句柄（如 Cisox 自己的覆盖窗）。
        ///
        /// # 返回
        /// 命中器；线程创建失败时返回 IO 错误。
        ///
        /// ```ignore
        /// let picker = WindowPicker::spawn(monitor.bounds, Vec::new())?;
        /// ```
        pub fn spawn(monitor: PhysicalRect, excluded: Vec<usize>) -> std::io::Result<Self> {
            let mailbox = Arc::new(HoverMailbox::new());
            let worker_box = Arc::clone(&mailbox);
            std::thread::Builder::new()
                .name(PICKER_THREAD_NAME.into())
                .spawn(move || run_worker(&worker_box, monitor, &excluded))?;
            Ok(Self {
                mailbox,
                last: None,
            })
        }
    }

    impl WindowHover for WindowPicker {
        fn hover(&mut self, point: PhysicalPoint, target: PickTarget) -> Option<PickRects> {
            if let Some((p, t, path)) = &self.last
                && *p == point
                && *t == target
            {
                return path.clone();
            }
            let seq = self.mailbox.submit(point, target);
            // 优先等本次结果；超时则沿用上一次已知结果，下次移动自然追平
            match self.mailbox.wait_result(seq, HOVER_WAIT) {
                Some(path) => {
                    self.last = Some((point, target, path.clone()));
                    path
                }
                None => self.mailbox.latest().flatten(),
            }
        }

        fn refinement(&mut self) -> Option<PickRects> {
            self.mailbox.take_refinement()
        }

        fn refinement_pending(&self) -> bool {
            self.mailbox.refinement_pending()
        }
    }

    impl Drop for WindowPicker {
        /// 关闭邮箱让工作线程退出。
        fn drop(&mut self) {
            self.mailbox.close();
        }
    }

    /// 工作线程主体：建立服务（含窗口快照），循环应答请求，邮箱关闭后退出。
    fn run_worker(mailbox: &HoverMailbox, monitor: PhysicalRect, excluded: &[usize]) {
        let mut service = match ElementRegionService::with_backend_excluding_ids(
            AccessibilityBackend::Uia,
            excluded,
        ) {
            Ok(s) => Some(s),
            Err(e) => {
                tracing::warn!(error = %e, "窗口命中服务初始化失败，智能选区不可用");
                None
            }
        };
        while let Some((seq, point, target)) = mailbox.next_request() {
            let Some(service) = service.as_mut() else {
                mailbox.publish(seq, None);
                continue;
            };
            let (path, incomplete) = query_frame_path(service, point, target, monitor);
            mailbox.publish(seq, path);
            // 控件级查询未走完（UIA 树尚在展开）：在后台继续细化，有新请求就取消
            if incomplete {
                mailbox.set_refining(true);
                refine(service, mailbox, seq, point, monitor);
                mailbox.set_refining(false);
            }
        }
    }

    /// 把桌面坐标的元素矩形换算为底图坐标路径，丢弃与显示器无交集的层。
    fn to_frame_path(path: &[ElementRect], monitor: PhysicalRect) -> PickRects {
        path.iter()
            .filter_map(|r| {
                screen_rect_to_frame(
                    PhysicalRect::new(r.left(), r.top(), r.width(), r.height()),
                    monitor,
                )
            })
            .collect()
    }

    /// 构造桌面坐标查询点。
    fn desktop_point(frame_point: PhysicalPoint, monitor: PhysicalRect) -> Point {
        Point {
            x: frame_point.x + monitor.x,
            y: frame_point.y + monitor.y,
            display_id: 0,
        }
    }

    /// 前台查询：窗口目标用窗口模式（单层），控件目标用控件模式（多层）。
    ///
    /// # 返回
    /// 路径与"结果是否未走完"（控件模式下 UIA 超预算、仍有可细化空间）。
    fn query_frame_path(
        service: &mut ElementRegionService,
        frame_point: PhysicalPoint,
        target: PickTarget,
        monitor: PhysicalRect,
    ) -> (Option<PickRects>, bool) {
        let mode = match target {
            PickTarget::Window => HitTestMode::Window,
            PickTarget::WindowSubElement => HitTestMode::UiElement,
        };
        let result = match service.query(
            desktop_point(frame_point, monitor),
            mode,
            &QueryControl::foreground(),
            &mut |_| {},
        ) {
            Ok(r) => r,
            Err(e) => {
                tracing::debug!(error = %e, "窗口命中查询失败");
                return (None, false);
            }
        };
        let incomplete =
            target == PickTarget::WindowSubElement && result.reason != StopReason::Complete;
        let path = result
            .path
            .map(|p| to_frame_path(&p, monitor))
            .filter(|p| !p.is_empty());
        (path, incomplete)
    }

    /// 后台细化：以较大预算重查同一点，沿途把更完整的路径发布给 UI；有新请求时取消。
    fn refine(
        service: &mut ElementRegionService,
        mailbox: &HoverMailbox,
        seq: u64,
        frame_point: PhysicalPoint,
        monitor: PhysicalRect,
    ) {
        let cancelled = || mailbox.superseded();
        let control = QueryControl::refinement(&cancelled);
        let mut publish = |path: &[ElementRect]| {
            let frame = to_frame_path(path, monitor);
            if !frame.is_empty() {
                mailbox.publish_refinement(seq, frame);
            }
        };
        match service.query(
            desktop_point(frame_point, monitor),
            HitTestMode::UiElement,
            &control,
            &mut publish,
        ) {
            Ok(result) => {
                if let Some(path) = result.path {
                    publish(&path);
                }
            }
            Err(e) => tracing::debug!(error = %e, "窗口细化查询失败"),
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        /// 冒烟：真实枚举窗口，结果（若有）必须落在显示器范围内且不卡死。
        #[test]
        fn smoke_enumerates_windows_within_monitor() {
            let monitor = PhysicalRect::new(0, 0, 4000, 3000);
            let mut picker = WindowPicker::spawn(monitor, Vec::new()).expect("spawn");
            for (x, y) in [(10, 10), (500, 400), (1200, 800), (3000, 2000)] {
                // 首次可能因快照未就绪超时，多等几轮
                for _ in 0..50 {
                    if let Some(path) = picker.hover(PhysicalPoint::new(x, y), PickTarget::Window) {
                        for r in path {
                            assert!(r.width > 0 && r.height > 0);
                            assert!(
                                r.x >= 0 && r.y >= 0 && r.right() <= 4000 && r.bottom() <= 3000
                            );
                        }
                        break;
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 开关默认开启，显式关闭后为 false。
    #[test]
    fn smart_selection_switch_follows_config() {
        let mut doc = ConfigDocument::from_bytes(None);
        assert!(smart_selection_enabled(&doc));
        doc.set_value(SMART_SELECTION_KEY, serde_json::json!(false))
            .unwrap();
        assert!(!smart_selection_enabled(&doc));
        assert!(start_window_hover(&doc, PhysicalRect::new(0, 0, 10, 10)).is_none());
    }

    /// 选区目标默认子控件，配置为 window 时切到窗口；未知值回落默认。
    #[test]
    fn selection_target_follows_config() {
        let mut doc = ConfigDocument::from_bytes(None);
        assert_eq!(selection_target(&doc), PickTarget::WindowSubElement);
        doc.set_value(SELECTION_TARGET_KEY, serde_json::json!("window"))
            .unwrap();
        assert_eq!(selection_target(&doc), PickTarget::Window);
        assert_eq!(
            PickTarget::from_config("bogus"),
            PickTarget::WindowSubElement
        );
        assert_eq!(
            PickTarget::Window.toggled().as_config(),
            "window_sub_element"
        );
    }

    /// 测试用矩形构造。
    fn r(x: i32, y: i32, w: i32, h: i32) -> PhysicalRect {
        PhysicalRect::new(x, y, w, h)
    }

    /// 桌面坐标减去显示器偏移，负坐标显示器同样正确。
    #[test]
    fn converts_with_negative_monitor_offset() {
        let monitor = r(-1920, 0, 1920, 1080);
        assert_eq!(
            screen_rect_to_frame(r(-1800, 100, 400, 300), monitor),
            Some(r(120, 100, 400, 300))
        );
    }

    /// 跨出显示器的窗口被裁剪到显示器内。
    #[test]
    fn clips_window_to_monitor() {
        let monitor = r(0, 0, 1920, 1080);
        assert_eq!(
            screen_rect_to_frame(r(-50, -20, 300, 200), monitor),
            Some(r(0, 0, 250, 180))
        );
        assert_eq!(
            screen_rect_to_frame(r(1800, 1000, 400, 400), monitor),
            Some(r(1800, 1000, 120, 80))
        );
    }

    /// 与显示器无交集或为空的窗口返回 None（含只贴边的情况）。
    #[test]
    fn disjoint_or_empty_is_none() {
        let monitor = r(0, 0, 100, 100);
        assert_eq!(screen_rect_to_frame(r(100, 0, 50, 50), monitor), None);
        assert_eq!(screen_rect_to_frame(r(-60, 0, 60, 50), monitor), None);
        assert_eq!(screen_rect_to_frame(r(10, 10, 0, 50), monitor), None);
    }

    /// 拖拽阈值：小于不算，等于及以上算；阈值非正则一律算。
    #[test]
    fn drag_threshold_boundaries() {
        let o = PhysicalPoint::new(100, 100);
        assert!(!exceeds_drag_threshold(o, PhysicalPoint::new(109, 100), 10));
        assert!(exceeds_drag_threshold(o, PhysicalPoint::new(110, 100), 10));
        assert!(!exceeds_drag_threshold(o, PhysicalPoint::new(107, 107), 10));
        assert!(exceeds_drag_threshold(o, PhysicalPoint::new(108, 108), 10));
        assert!(exceeds_drag_threshold(o, o, 0));
    }

    /// 测试用画布范围。
    const BOUNDS: PhysicalRect = PhysicalRect::new(0, 0, 400, 300);

    /// 测试用三层路径：按钮 → 面板 → 窗口（自深到浅）。
    fn three() -> PickRects {
        vec![r(10, 10, 30, 20), r(5, 5, 100, 80), r(0, 0, 200, 150)]
    }

    /// 子控件目标默认选最深层，窗口目标固定选最浅层。
    #[test]
    fn default_index_follows_target() {
        let mut sub = PickPath::new(PickTarget::WindowSubElement);
        assert!(sub.apply_hit_path(&three(), BOUNDS, 1));
        assert_eq!(sub.current(), Some(r(10, 10, 30, 20)));
        let mut win = PickPath::new(PickTarget::Window);
        assert!(win.apply_hit_path(&three(), BOUNDS, 1));
        assert_eq!(win.current(), Some(r(0, 0, 200, 150)));
    }

    /// 滚轮向上向外、向下向内，且在两端夹住；窗口目标下滚轮不改变选择。
    #[test]
    fn wheel_steps_clamp_and_window_target_is_fixed() {
        let mut p = PickPath::new(PickTarget::WindowSubElement);
        p.apply_hit_path(&three(), BOUNDS, 1);
        p.select_step(1);
        assert_eq!(p.index(), Some(1));
        p.select_step(5);
        assert_eq!(p.index(), Some(2));
        p.select_step(-9);
        assert_eq!(p.index(), Some(0));
        let mut w = PickPath::new(PickTarget::Window);
        w.apply_hit_path(&three(), BOUNDS, 1);
        w.select_step(-1);
        assert_eq!(w.current(), Some(r(0, 0, 200, 150)));
    }

    /// 路径不变时保留滚轮选择；路径变了回到目标默认层。
    #[test]
    fn same_path_keeps_selection_changed_path_resets() {
        let mut p = PickPath::new(PickTarget::WindowSubElement);
        p.apply_hit_path(&three(), BOUNDS, 1);
        p.select_step(1);
        p.apply_hit_path(&three(), BOUNDS, 1);
        assert_eq!(p.index(), Some(1));
        p.apply_hit_path(&[r(50, 50, 20, 20), r(0, 0, 200, 150)], BOUNDS, 1);
        assert_eq!(p.index(), Some(0));
    }

    /// 切换目标重置到新目标的默认层；空路径清空选择。
    #[test]
    fn toggle_target_resets_and_empty_clears() {
        let mut p = PickPath::new(PickTarget::WindowSubElement);
        p.apply_hit_path(&three(), BOUNDS, 1);
        assert_eq!(p.toggle_target(), PickTarget::Window);
        assert_eq!(p.current(), Some(r(0, 0, 200, 150)));
        assert_eq!(p.toggle_target(), PickTarget::WindowSubElement);
        assert_eq!(p.current(), Some(r(10, 10, 30, 20)));
        assert!(!p.apply_hit_path(&[], BOUNDS, 1));
        assert_eq!(p.current(), None);
    }

    /// 裁剪 / 最小尺寸 / 相邻去重。
    #[test]
    fn path_is_bounded_filtered_and_deduped() {
        let mut p = PickPath::new(PickTarget::WindowSubElement);
        // 3x3 太小被丢；两层裁剪后相同只留一层；越界部分被裁到范围内
        let path = [
            r(0, 0, 3, 3),
            r(-50, -50, 150, 150),
            r(-10, -10, 110, 110),
            r(0, 0, 900, 900),
        ];
        p.apply_hit_path(&path, BOUNDS, 8);
        assert_eq!(p.current(), Some(r(0, 0, 100, 100)));
        p.select_step(1);
        assert_eq!(p.current(), Some(r(0, 0, 400, 300)));
    }

    /// 细化：更深层插在最深端且浅端吻合才接受；默认回到最深层，显式选择则保持原选中层。
    #[test]
    fn refinement_accepts_deeper_prefix_only() {
        let base = vec![r(5, 5, 100, 80), r(0, 0, 200, 150)];
        let deeper = vec![r(12, 12, 20, 20), r(5, 5, 100, 80), r(0, 0, 200, 150)];
        let mut p = PickPath::new(PickTarget::WindowSubElement);
        p.apply_hit_path(&base, BOUNDS, 1);
        assert!(p.apply_refinement(&deeper, BOUNDS, 1));
        assert_eq!(p.current(), Some(r(12, 12, 20, 20)));

        let mut q = PickPath::new(PickTarget::WindowSubElement);
        q.apply_hit_path(&base, BOUNDS, 1);
        q.select_step(1);
        assert!(q.apply_refinement(&deeper, BOUNDS, 1));
        assert_eq!(q.current(), Some(r(0, 0, 200, 150)));

        // 不吻合 / 不更长 / 窗口目标：拒绝
        let mut z = PickPath::new(PickTarget::WindowSubElement);
        z.apply_hit_path(&base, BOUNDS, 1);
        assert!(!z.apply_refinement(&base, BOUNDS, 1));
        assert!(!z.apply_refinement(
            &[r(1, 1, 9, 9), r(2, 2, 90, 70), r(0, 0, 200, 150)],
            BOUNDS,
            1
        ));
        let mut w = PickPath::new(PickTarget::Window);
        w.apply_hit_path(&base, BOUNDS, 1);
        assert!(!w.apply_refinement(&deeper, BOUNDS, 1));
    }

    /// 过渡动画：首个选区直接呈现；之后目标变化按 OutQuad 在 101ms 内插值到位；关闭动画则直接跳。
    #[test]
    fn transition_first_direct_then_animates_then_settles() {
        let t0 = Instant::now();
        let a = r(0, 0, 100, 100);
        let b = r(100, 0, 200, 100);
        let mut tr = HighlightTransition::new(true);
        assert_eq!(tr.advance(Some(a), t0), Some(a));
        assert!(!tr.is_running());
        // 目标变化：起点为当前显示
        assert_eq!(tr.advance(Some(b), t0), Some(a));
        assert!(tr.is_running());
        let mid = tr.advance(Some(b), t0 + TRANSITION_DURATION / 2).unwrap();
        // OutQuad(0.5)=0.75
        assert_eq!(mid.x, 75);
        assert_eq!(mid.width, 100 + 75);
        assert_eq!(tr.advance(Some(b), t0 + TRANSITION_DURATION * 2), Some(b));
        assert!(!tr.is_running());
        // 清空后再来一个视为首个，直接呈现
        assert_eq!(tr.advance(None, t0), None);
        assert_eq!(tr.advance(Some(a), t0), Some(a));

        let mut off = HighlightTransition::new(false);
        off.advance(Some(a), t0);
        assert_eq!(off.advance(Some(b), t0), Some(b));
        assert!(!off.is_running());
    }

    /// 邮箱：新请求覆盖旧请求，只有最新的会被工作线程取到。
    #[test]
    fn mailbox_keeps_only_latest_request() {
        let mb = HoverMailbox::new();
        let t = PickTarget::Window;
        let _ = mb.submit(PhysicalPoint::new(1, 1), t);
        let second = mb.submit(PhysicalPoint::new(2, 2), t);
        assert_eq!(
            mb.next_request(),
            Some((second, PhysicalPoint::new(2, 2), t))
        );
        assert!(!mb.superseded());
        mb.submit(PhysicalPoint::new(3, 3), t);
        assert!(mb.superseded());
    }

    /// 邮箱：过期结果不会覆盖较新的结果。
    #[test]
    fn mailbox_drops_stale_results() {
        let mb = HoverMailbox::new();
        mb.publish(5, Some(vec![r(0, 0, 9, 9)]));
        mb.publish(3, None);
        assert_eq!(mb.latest(), Some(Some(vec![r(0, 0, 9, 9)])));
    }

    /// 邮箱：等待结果在已发布时立即返回，未发布时超时为 None。
    #[test]
    fn mailbox_wait_result_times_out_or_returns() {
        let mb = HoverMailbox::new();
        let seq = mb.submit(PhysicalPoint::new(0, 0), PickTarget::Window);
        assert_eq!(mb.wait_result(seq, Duration::from_millis(1)), None);
        mb.publish(seq, Some(vec![r(1, 2, 3, 4)]));
        assert_eq!(
            mb.wait_result(seq, Duration::from_millis(1)),
            Some(Some(vec![r(1, 2, 3, 4)]))
        );
        assert_eq!(mb.wait_result(seq + 1, Duration::from_millis(1)), None);
    }

    /// 邮箱：细化只对最新请求有效，取走后清空，新请求到来时作废。
    #[test]
    fn mailbox_refinement_only_for_latest_request() {
        let mb = HoverMailbox::new();
        let first = mb.submit(PhysicalPoint::new(0, 0), PickTarget::WindowSubElement);
        mb.publish_refinement(first, vec![r(0, 0, 5, 5)]);
        assert_eq!(mb.take_refinement(), Some(vec![r(0, 0, 5, 5)]));
        assert_eq!(mb.take_refinement(), None);
        mb.publish_refinement(first, vec![r(0, 0, 6, 6)]);
        mb.submit(PhysicalPoint::new(1, 1), PickTarget::WindowSubElement);
        assert_eq!(mb.take_refinement(), None);
        mb.publish_refinement(first, vec![r(0, 0, 7, 7)]);
        assert_eq!(mb.take_refinement(), None);
    }

    /// 邮箱关闭后工作线程的取请求返回 None。
    #[test]
    fn closed_mailbox_stops_worker() {
        let mb = HoverMailbox::new();
        mb.close();
        assert_eq!(mb.next_request(), None);
    }
}
