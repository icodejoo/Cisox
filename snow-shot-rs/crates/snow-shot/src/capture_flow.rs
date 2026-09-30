//! 截图触发流程里与 GPUI 无关的部分：选择目标显示器、后台线程采集、结果载荷。

use snow_platform::capture::{CapturedScreen, capture_display};
use snow_ui::shell::geometry::PhysicalPoint;
use snow_ui::shell::monitor::{MonitorInfo, Monitors};
use std::time::{Duration, Instant};

/// 采集线程名称。
const CAPTURE_THREAD_NAME: &str = "snow-capture";

/// 一次采集的结果：目标显示器、冻结帧与采集耗时。
#[derive(Clone)]
pub struct CapturePayload {
    /// 被采集的显示器。
    pub monitor: MonitorInfo,
    /// 采集到的整屏 BGRA 帧（尺寸等于显示器物理范围）。
    pub screen: CapturedScreen,
    /// 采集耗时。
    pub elapsed: Duration,
}

impl std::fmt::Debug for CapturePayload {
    /// 只打印尺寸，避免日志里出现整屏像素。
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CapturePayload")
            .field("monitor", &self.monitor.id)
            .field("size", &(self.screen.width, self.screen.height))
            .field("elapsed", &self.elapsed)
            .finish()
    }
}

impl PartialEq for CapturePayload {
    /// 仅按显示器与帧尺寸比较（测试用，避免比较整屏像素）。
    fn eq(&self, other: &Self) -> bool {
        self.monitor == other.monitor
            && self.screen.width == other.screen.width
            && self.screen.height == other.screen.height
    }
}

/// 选出光标所在的显示器；光标未知或不在任何显示器内时退回主显示器。
///
/// # 参数
/// - `monitors`：显示器快照（物理坐标，可含负值）。
/// - `cursor`：光标屏幕坐标（物理像素）。
///
/// # 返回
/// 显示器信息副本；没有任何显示器时返回 `None`。
///
/// ```ignore
/// let monitor = pick_monitor(&monitors, Some(PhysicalPoint::new(-100, 50)));
/// ```
pub fn pick_monitor(monitors: &Monitors, cursor: Option<PhysicalPoint>) -> Option<MonitorInfo> {
    cursor
        .and_then(|p| monitors.at_point(p))
        .or_else(|| monitors.primary())
        .cloned()
}

/// 显示器整屏范围对应的采集区域 `(x, y, 宽, 高)`；宽高非正时返回 `None`。
///
/// # 参数
/// - `monitor`：目标显示器。
///
/// ```ignore
/// assert_eq!(capture_region(&monitor), Some((-1920, 0, 1920, 1080)));
/// ```
pub fn capture_region(monitor: &MonitorInfo) -> Option<(i32, i32, u32, u32)> {
    let b = monitor.bounds;
    (b.width > 0 && b.height > 0).then_some((b.x, b.y, b.width as u32, b.height as u32))
}

/// 在后台线程采集指定显示器，完成后调用 `on_done`（运行在采集线程上，只应做投递）。
///
/// # 参数
/// - `monitor`：目标显示器。
/// - `on_done`：结果回调。
///
/// # 返回
/// 线程创建失败时返回 IO 错误（此时 `on_done` 不会被调用）；采集区域非法则同步回调错误。
///
/// ```ignore
/// spawn_capture(monitor, |result| println!("{:?}", result.map(|p| p.elapsed)))?;
/// ```
pub fn spawn_capture(
    monitor: MonitorInfo,
    on_done: impl FnOnce(Result<CapturePayload, String>) + Send + 'static,
) -> std::io::Result<()> {
    let Some(region) = capture_region(&monitor) else {
        on_done(Err(format!("显示器范围非法: {:?}", monitor.bounds)));
        return Ok(());
    };
    std::thread::Builder::new()
        .name(CAPTURE_THREAD_NAME.into())
        .spawn(move || {
            let started = Instant::now();
            let result = capture_display(Some(region)).map(|screen| CapturePayload {
                monitor,
                screen,
                elapsed: started.elapsed(),
            });
            on_done(result);
        })
        .map(|_| ())
}

#[cfg(test)]
mod tests {
    use super::*;
    use snow_ui::shell::geometry::{PhysicalRect, ScaleFactor};
    use snow_ui::shell::monitor::MonitorId;

    /// 构造测试用显示器。
    fn monitor(id: u64, bounds: PhysicalRect, primary: bool) -> MonitorInfo {
        MonitorInfo {
            id: MonitorId(id),
            name: format!("M{id}"),
            bounds,
            work_area: bounds,
            scale: ScaleFactor::ONE,
            is_primary: primary,
        }
    }

    /// 双屏：主屏在原点，副屏在左侧（负坐标）。
    fn two() -> Monitors {
        Monitors::from_list(vec![
            monitor(1, PhysicalRect::new(0, 0, 2560, 1440), true),
            monitor(2, PhysicalRect::new(-1920, 200, 1920, 1080), false),
        ])
    }

    /// 光标在哪块屏就选哪块，含负坐标副屏。
    #[test]
    fn picks_monitor_under_cursor() {
        let ms = two();
        assert_eq!(pick_monitor(&ms, Some(PhysicalPoint::new(10, 10))).unwrap().id, MonitorId(1));
        assert_eq!(
            pick_monitor(&ms, Some(PhysicalPoint::new(-500, 300))).unwrap().id,
            MonitorId(2)
        );
    }

    /// 光标未知或落在屏外（例如副屏上方的空白）时退回主屏。
    #[test]
    fn falls_back_to_primary() {
        let ms = two();
        assert_eq!(pick_monitor(&ms, None).unwrap().id, MonitorId(1));
        assert_eq!(
            pick_monitor(&ms, Some(PhysicalPoint::new(-500, 50))).unwrap().id,
            MonitorId(1)
        );
        assert!(pick_monitor(&Monitors::default(), None).is_none());
    }

    /// 采集区域等于显示器物理范围（含负原点），空范围返回 None。
    #[test]
    fn region_matches_bounds() {
        let m = monitor(2, PhysicalRect::new(-1920, 200, 1920, 1080), false);
        assert_eq!(capture_region(&m), Some((-1920, 200, 1920, 1080)));
        let empty = monitor(3, PhysicalRect::new(0, 0, 0, 10), false);
        assert_eq!(capture_region(&empty), None);
    }

    /// 非法范围同步回调错误，不创建线程。
    #[test]
    fn invalid_region_reports_error_synchronously() {
        let empty = monitor(3, PhysicalRect::new(0, 0, 0, 10), false);
        let (tx, rx) = std::sync::mpsc::channel();
        spawn_capture(empty, move |r| tx.send(r.is_err()).unwrap()).unwrap();
        assert_eq!(rx.try_recv(), Ok(true));
    }

    /// 载荷调试输出不包含像素数据。
    #[test]
    fn payload_debug_is_compact() {
        let p = CapturePayload {
            monitor: monitor(1, PhysicalRect::new(0, 0, 4, 4), true),
            screen: CapturedScreen::new_solid(4, 4, (1, 2, 3, 255)),
            elapsed: Duration::from_millis(5),
        };
        let text = format!("{p:?}");
        assert!(text.contains("size") && text.len() < 200, "{text}");
    }

    /// 真实采集：主屏一小块区域能返回正确尺寸（无桌面会话时跳过）。
    #[cfg(windows)]
    #[test]
    fn real_capture_small_region() {
        let m = monitor(1, PhysicalRect::new(0, 0, 16, 16), true);
        let (tx, rx) = std::sync::mpsc::channel();
        spawn_capture(m, move |r| tx.send(r).unwrap()).unwrap();
        if let Ok(payload) = rx.recv_timeout(Duration::from_secs(10)).unwrap() {
            assert_eq!((payload.screen.width, payload.screen.height), (16, 16));
        }
    }
}
