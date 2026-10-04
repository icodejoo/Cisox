//! 截图智能选区（阶段 1）：鼠标悬停时识别其下的顶层窗口矩形。
//!
//! 命中查询放在专用后台线程（COM 对象只能在创建它的线程使用），
//! UI 线程只投递坐标并读取最新结果；过期请求在邮箱里被新请求覆盖。

use snow_config::document::ConfigDocument;
use snow_ui::shell::geometry::{PhysicalPoint, PhysicalRect};
use std::sync::{Condvar, Mutex};
use std::time::Duration;

/// 拖拽阈值（逻辑像素）：按下后位移不到该值视为单击，超过则转为手动框选（对齐 Qt 的 startDragDistance）。
pub const DRAG_THRESHOLD_LOGICAL: f32 = 10.0;

/// UI 线程等待命中结果的最长时间；超时则沿用上一次结果，避免卡住鼠标移动。
pub const HOVER_WAIT: Duration = Duration::from_millis(3);

/// 「自动识别窗口范围」开关的配置键（沿用旧版 `screenshot_selection/smart_selection`，默认开）。
pub const SMART_SELECTION_KEY: &str = "screenshot_selection/smart_selection";

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

/// 悬停窗口来源：给定覆盖窗（底图）坐标，返回其下窗口在底图坐标系中的矩形。
pub trait WindowHover {
    /// 查询指定点下的窗口矩形。
    ///
    /// # 参数
    /// - `point`：底图物理坐标（原点为显示器左上角）。
    ///
    /// # 返回
    /// 窗口矩形（底图物理坐标，已裁到底图范围）；没有窗口或结果未就绪时为 `None`。
    ///
    /// ```ignore
    /// let rect = source.hover(PhysicalPoint::new(300, 200));
    /// ```
    fn hover(&mut self, point: PhysicalPoint) -> Option<PhysicalRect>;
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

/// 从命中路径里挑出作为选区的矩形：窗口级命中路径只有一项，取最后一个非空矩形。
///
/// # 参数
/// - `path`：命中路径（桌面坐标，外层到内层）。
///
/// # 返回
/// 选中的矩形；路径为空或全为空矩形时返回 `None`。
///
/// ```ignore
/// assert_eq!(pick_path_rect(&[]), None);
/// ```
pub fn pick_path_rect(path: &[PhysicalRect]) -> Option<PhysicalRect> {
    path.iter()
        .rev()
        .find(|r| r.width > 0 && r.height > 0)
        .copied()
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

/// 邮箱内部状态。
#[derive(Default)]
struct MailboxState {
    /// 最近一次提交的请求序号。
    last_seq: u64,
    /// 尚未被工作线程取走的请求（新请求覆盖旧请求）。
    pending: Option<(u64, PhysicalPoint)>,
    /// 最新已发布的结果（序号, 矩形）。
    result: Option<(u64, Option<PhysicalRect>)>,
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

    /// 提交一个查询请求，覆盖尚未被取走的旧请求。
    ///
    /// # 返回
    /// 本次请求序号（严格递增）。
    pub fn submit(&self, point: PhysicalPoint) -> u64 {
        let mut s = self.state.lock().unwrap_or_else(|e| e.into_inner());
        s.last_seq += 1;
        let seq = s.last_seq;
        s.pending = Some((seq, point));
        self.changed.notify_all();
        seq
    }

    /// 阻塞等待下一个请求；邮箱关闭后返回 `None`。
    pub fn next_request(&self) -> Option<(u64, PhysicalPoint)> {
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

    /// 发布一次查询结果；序号比已发布结果更旧时丢弃。
    pub fn publish(&self, seq: u64, rect: Option<PhysicalRect>) {
        let mut s = self.state.lock().unwrap_or_else(|e| e.into_inner());
        if s.result.is_none_or(|(old, _)| seq >= old) {
            s.result = Some((seq, rect));
        }
        self.changed.notify_all();
    }

    /// 最新已发布的结果（可能对应更早的请求）。
    pub fn latest(&self) -> Option<Option<PhysicalRect>> {
        let s = self.state.lock().unwrap_or_else(|e| e.into_inner());
        s.result.map(|(_, rect)| rect)
    }

    /// 等待序号不小于 `seq` 的结果，最多等 `timeout`。
    ///
    /// # 返回
    /// 结果已就绪返回 `Some(矩形)`；超时返回 `None`。
    pub fn wait_result(&self, seq: u64, timeout: Duration) -> Option<Option<PhysicalRect>> {
        let s = self.state.lock().unwrap_or_else(|e| e.into_inner());
        let (s, _) = self
            .changed
            .wait_timeout_while(s, timeout, |s| {
                !s.closed && s.result.is_none_or(|(got, _)| got < seq)
            })
            .unwrap_or_else(|e| e.into_inner());
        s.result
            .filter(|(got, _)| *got >= seq)
            .map(|(_, rect)| rect)
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
    use super::{HOVER_WAIT, HoverMailbox, WindowHover, pick_path_rect, screen_rect_to_frame};
    use snow_ui::shell::geometry::{PhysicalPoint, PhysicalRect};
    use snow_ui_selector::{
        AccessibilityBackend, ElementRegionService, HitTestMode, Point, QueryControl,
    };
    use std::sync::Arc;

    /// 命中线程名称。
    const PICKER_THREAD_NAME: &str = "snow-window-pick";

    /// 窗口悬停命中器：句柄由 UI 线程持有，查询在后台线程执行。
    pub struct WindowPicker {
        /// 与工作线程共享的邮箱。
        mailbox: Arc<HoverMailbox>,
        /// 最近一次查询的点与结果，用于同一点去重。
        last: Option<(PhysicalPoint, Option<PhysicalRect>)>,
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
        fn hover(&mut self, point: PhysicalPoint) -> Option<PhysicalRect> {
            if let Some((p, rect)) = self.last
                && p == point
            {
                return rect;
            }
            let seq = self.mailbox.submit(point);
            // 优先等本次结果；超时则沿用上一次已知结果，下次移动自然追平
            self.mailbox
                .wait_result(seq, HOVER_WAIT)
                .inspect(|rect| self.last = Some((point, *rect)))
                .or_else(|| self.mailbox.latest())
                .flatten()
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
        while let Some((seq, point)) = mailbox.next_request() {
            let rect = service
                .as_mut()
                .and_then(|s| query_frame_rect(s, point, monitor));
            mailbox.publish(seq, rect);
        }
    }

    /// 在桌面坐标下查询窗口并换算为底图坐标矩形。
    fn query_frame_rect(
        service: &mut ElementRegionService,
        frame_point: PhysicalPoint,
        monitor: PhysicalRect,
    ) -> Option<PhysicalRect> {
        let point = Point {
            x: frame_point.x + monitor.x,
            y: frame_point.y + monitor.y,
            display_id: 0,
        };
        let result = service
            .query(
                point,
                HitTestMode::Window,
                &QueryControl::foreground(),
                &mut |_| {},
            )
            .inspect_err(|e| tracing::debug!(error = %e, "窗口命中查询失败"))
            .ok()?;
        let path: Vec<PhysicalRect> = result
            .path?
            .iter()
            .map(|r| PhysicalRect::new(r.left(), r.top(), r.width(), r.height()))
            .collect();
        screen_rect_to_frame(pick_path_rect(&path)?, monitor)
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
                    if let Some(r) = picker.hover(PhysicalPoint::new(x, y)) {
                        assert!(r.width > 0 && r.height > 0);
                        assert!(r.x >= 0 && r.y >= 0 && r.right() <= 4000 && r.bottom() <= 3000);
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

    /// 命中路径取最后一个非空矩形。
    #[test]
    fn picks_last_non_empty_path_rect() {
        assert_eq!(pick_path_rect(&[]), None);
        assert_eq!(
            pick_path_rect(&[r(0, 0, 10, 10), r(1, 1, 5, 5)]),
            Some(r(1, 1, 5, 5))
        );
        assert_eq!(
            pick_path_rect(&[r(0, 0, 10, 10), r(1, 1, 0, 5)]),
            Some(r(0, 0, 10, 10))
        );
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

    /// 邮箱：新请求覆盖旧请求，只有最新的会被工作线程取到。
    #[test]
    fn mailbox_keeps_only_latest_request() {
        let mb = HoverMailbox::new();
        let _ = mb.submit(PhysicalPoint::new(1, 1));
        let second = mb.submit(PhysicalPoint::new(2, 2));
        assert_eq!(mb.next_request(), Some((second, PhysicalPoint::new(2, 2))));
    }

    /// 邮箱：过期结果不会覆盖较新的结果。
    #[test]
    fn mailbox_drops_stale_results() {
        let mb = HoverMailbox::new();
        mb.publish(5, Some(r(0, 0, 9, 9)));
        mb.publish(3, None);
        assert_eq!(mb.latest(), Some(Some(r(0, 0, 9, 9))));
    }

    /// 邮箱：等待结果在已发布时立即返回，未发布时超时为 None。
    #[test]
    fn mailbox_wait_result_times_out_or_returns() {
        let mb = HoverMailbox::new();
        let seq = mb.submit(PhysicalPoint::new(0, 0));
        assert_eq!(mb.wait_result(seq, Duration::from_millis(1)), None);
        mb.publish(seq, Some(r(1, 2, 3, 4)));
        assert_eq!(
            mb.wait_result(seq, Duration::from_millis(1)),
            Some(Some(r(1, 2, 3, 4)))
        );
        assert_eq!(mb.wait_result(seq + 1, Duration::from_millis(1)), None);
    }

    /// 邮箱关闭后工作线程的取请求返回 None。
    #[test]
    fn closed_mailbox_stops_worker() {
        let mb = HoverMailbox::new();
        mb.close();
        assert_eq!(mb.next_request(), None);
    }
}
