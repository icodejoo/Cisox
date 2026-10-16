//! 录制就绪态（Ready）可选项的纯函数：帧率 / 倒计时档位、输出尺寸档、范围预设、静音联动。
//!
//! 这里只做数值计算，不碰界面与会话；视图层据此渲染下拉并改写本次录制的配置（不写回设置页）。

use crate::recording::model::RecordingFormat;
use snow_recorder_protocol::fit_output_size;
use snow_ui::shell::geometry::PhysicalRect;

/// 帧率下拉档位。
pub const FPS_CHOICES: [u32; 4] = [15, 24, 30, 60];
/// 倒计时下拉档位（秒，0 = 关闭）。
pub const COUNTDOWN_CHOICES: [u32; 3] = [0, 3, 5];
/// 偶数对齐的除数。
const EVEN: i32 = 2;
/// iPhone 竖屏比例，取设备物理分辨率 1179x2556（iPhone 15 / 16）的宽:高，不是逻辑点 393x852。
const PHONE_RATIO: (i32, i32) = (1179, 2556);
/// 平板竖屏比例，取 iPad Pro 11 英寸物理分辨率 1668x2388 的宽:高。
const TABLET_RATIO: (i32, i32) = (1668, 2388);
/// 各输出尺寸档的目标短边。
const SHORT_1080: u32 = 1080;
/// 720p 的目标短边。
const SHORT_720: u32 = 720;
/// 480p 的目标短边。
const SHORT_480: u32 = 480;

/// 输出尺寸档。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutputPreset {
    /// 原始尺寸（不缩放）。
    Original,
    /// 短边 1080。
    P1080,
    /// 短边 720。
    P720,
    /// 短边 480。
    P480,
}

impl OutputPreset {
    /// 全部档位（下拉顺序）。
    pub const ALL: [Self; 4] = [Self::Original, Self::P1080, Self::P720, Self::P480];

    /// 该档的目标短边；原始档为 `None`。
    pub const fn target_short_side(self) -> Option<u32> {
        match self {
            Self::Original => None,
            Self::P1080 => Some(SHORT_1080),
            Self::P720 => Some(SHORT_720),
            Self::P480 => Some(SHORT_480),
        }
    }

    /// 该档对应的文案 message id。
    pub const fn message_id(self) -> &'static str {
        match self {
            Self::Original => "recording-ready-size-original",
            Self::P1080 => "recording-ready-size-1080",
            Self::P720 => "recording-ready-size-720",
            Self::P480 => "recording-ready-size-480",
        }
    }
}

/// 选区短边不超过该档目标时视为“会放大”：此时该档等同原始（不放大），界面置灰。
///
/// # 参数
/// - `preset`：输出尺寸档。
/// - `width`、`height`：当前选区物理尺寸。
pub fn is_upscale(preset: OutputPreset, width: i32, height: i32) -> bool {
    match preset.target_short_side() {
        Some(target) => u32::try_from(width.min(height).max(0)).unwrap_or(0) <= target,
        None => false,
    }
}

/// 按选区算出精确输出尺寸。
///
/// # 参数
/// - `preset`：输出尺寸档。
/// - `width`、`height`：当前选区物理尺寸。
///
/// # 返回
/// 原始档或“不放大”时返回 `None`（保持原尺寸），否则返回等比缩小的偶数尺寸。
///
/// # 示例
/// ```ignore
/// assert_eq!(output_size_for(OutputPreset::P720, 2560, 1440), Some((1280, 720)));
/// assert_eq!(output_size_for(OutputPreset::P1080, 1280, 720), None);
/// ```
pub fn output_size_for(preset: OutputPreset, width: i32, height: i32) -> Option<(u32, u32)> {
    let target = preset.target_short_side()?;
    if is_upscale(preset, width, height) {
        return None;
    }
    let (w, h) = (
        u32::try_from(width).unwrap_or(0),
        u32::try_from(height).unwrap_or(0),
    );
    Some(fit_output_size(w, h, target))
}

/// 范围预设。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RangePreset {
    /// 恢复为一开始框选完成时的矩形（由视图记录原始选区，纯函数里原样返回当前选区）。
    Restore,
    /// 当前显示器全部区域。
    FullScreen,
    /// 16:9 横屏。
    Ratio16x9,
    /// 4:3 横屏。
    Ratio4x3,
    /// 1:1 方形。
    Square,
    /// 9:16 竖屏。
    Portrait9x16,
    /// 手机竖屏（按 iPhone 物理分辨率 1179x2556 的比例）。
    PhonePortrait,
    /// 平板竖屏（按 iPad 物理分辨率 1668x2388 的比例）。
    TabletPortrait,
    /// 3:4 竖屏。
    Portrait3x4,
}

impl RangePreset {
    /// 全部预设（下拉顺序）。
    pub const ALL: [Self; 9] = [
        Self::Restore,
        Self::FullScreen,
        Self::Ratio16x9,
        Self::Ratio4x3,
        Self::Square,
        Self::Portrait3x4,
        Self::Portrait9x16,
        Self::PhonePortrait,
        Self::TabletPortrait,
    ];

    /// 宽:高比例；全屏没有固定比例。
    const fn ratio(self) -> Option<(i32, i32)> {
        match self {
            Self::Restore | Self::FullScreen => None,
            Self::Ratio16x9 => Some((16, 9)),
            Self::Ratio4x3 => Some((4, 3)),
            Self::Square => Some((1, 1)),
            Self::Portrait9x16 => Some((9, 16)),
            Self::PhonePortrait => Some(PHONE_RATIO),
            Self::TabletPortrait => Some(TABLET_RATIO),
            Self::Portrait3x4 => Some((3, 4)),
        }
    }

    /// 该预设对应的文案 message id。
    pub const fn message_id(self) -> &'static str {
        match self {
            Self::Restore => "recording-ready-range-restore",
            Self::FullScreen => "recording-ready-range-full",
            Self::Ratio16x9 => "recording-ready-range-16-9",
            Self::Ratio4x3 => "recording-ready-range-4-3",
            Self::Square => "recording-ready-range-1-1",
            Self::Portrait9x16 => "recording-ready-range-9-16",
            Self::PhonePortrait => "recording-ready-range-phone",
            Self::TabletPortrait => "recording-ready-range-tablet",
            Self::Portrait3x4 => "recording-ready-range-3-4",
        }
    }
}

/// 按预设重设选区：比例预设在 `current`（选区）内取该比例下尽量大的矩形，以选区中心为基准（宽高取偶数）；
/// 全屏预设返回显示器全部区域。结果总会夹回显示器内。
///
/// # 参数
/// - `preset`：范围预设。
/// - `current`：作为基准的选区（虚拟桌面物理坐标）。
/// - `monitor`：选区所在显示器范围。
///
/// # 返回
/// 新选区；全屏预设返回显示器全部区域，恢复预设原样返回 `current`。
///
/// # 示例
/// ```ignore
/// let m = PhysicalRect::new(0, 0, 1920, 1080);
/// let r = apply_range_preset(RangePreset::Square, PhysicalRect::new(100, 100, 400, 300), m);
/// assert_eq!((r.width, r.height), (300, 300));
/// ```
pub fn apply_range_preset(
    preset: RangePreset,
    current: PhysicalRect,
    monitor: PhysicalRect,
) -> PhysicalRect {
    if preset == RangePreset::Restore {
        return current;
    }
    let Some((rw, rh)) = preset.ratio() else {
        return monitor;
    };
    let (cw, ch) = (i64::from(current.width), i64::from(current.height));
    let (rw, rh) = (i64::from(rw), i64::from(rh));
    // 选区比目标比例更“窄”时以宽为准，否则以高为准
    let (w, h) = if cw * rh <= ch * rw {
        (cw, cw * rh / rw)
    } else {
        (ch * rw / rh, ch)
    };
    let even = |v: i64| (v - v % i64::from(EVEN)).max(i64::from(EVEN));
    let (w, h) = (even(w) as i32, even(h) as i32);
    let cx = current.x + current.width / EVEN;
    let cy = current.y + current.height / EVEN;
    let x = (cx - w / EVEN).clamp(monitor.x, (monitor.right() - w).max(monitor.x));
    let y = (cy - h / EVEN).clamp(monitor.y, (monitor.bottom() - h).max(monitor.y));
    PhysicalRect::new(x, y, w, h)
}

/// 选区拖拽的最小边长（物理像素），拖过对边时钳在这个值。
pub const MIN_SELECTION_SIZE: i32 = 64;

/// 被拖拽的选区边（只做四条边的中点手柄）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DragEdge {
    /// 上边（只改高度，下边固定）。
    Top,
    /// 下边（只改高度，上边固定）。
    Bottom,
    /// 左边（只改宽度，右边固定）。
    Left,
    /// 右边（只改宽度，左边固定）。
    Right,
    /// 整体平移（拖选区内部），尺寸不变。
    Move,
}

/// 把选区整体平移 `delta`，尺寸不变且夹在显示器内。
///
/// # 参数
/// - `origin`：拖拽开始时的选区（虚拟桌面物理坐标）。
/// - `delta`：鼠标相对按下点的位移 `(dx, dy)`。
/// - `monitor`：选区所在显示器范围；选区比显示器大时贴左上。
///
/// # 返回
/// 平移后的选区。
///
/// # 示例
/// ```ignore
/// let m = PhysicalRect::new(0, 0, 1920, 1080);
/// let r = move_region(PhysicalRect::new(100, 100, 400, 300), (50, -500), m);
/// assert_eq!((r.x, r.y), (150, 0));
/// ```
pub fn move_region(origin: PhysicalRect, delta: (i32, i32), monitor: PhysicalRect) -> PhysicalRect {
    let x = (origin.x + delta.0).clamp(monitor.x, (monitor.right() - origin.width).max(monitor.x));
    let y =
        (origin.y + delta.1).clamp(monitor.y, (monitor.bottom() - origin.height).max(monitor.y));
    PhysicalRect::new(x, y, origin.width, origin.height)
}

/// 按鼠标位置拖动选区的一条边，对边固定。
///
/// # 参数
/// - `origin`：拖拽开始时的选区（虚拟桌面物理坐标）。
/// - `edge`：被拖的边。
/// - `pointer`：当前鼠标物理坐标 `(x, y)`；上下边只用 y，左右边只用 x。
/// - `monitor`：选区所在显示器范围，结果不会超出。
/// - `min_size`：被拖维度的最小边长，拖过对边时钳在该值（不翻转）。
///
/// # 返回
/// 新选区。
///
/// # 示例
/// ```ignore
/// let m = PhysicalRect::new(0, 0, 1920, 1080);
/// let r = drag_edge(PhysicalRect::new(100, 100, 400, 300), DragEdge::Right, (700, 0), m, 64);
/// assert_eq!((r.x, r.width), (100, 600));
/// ```
pub fn drag_edge(
    origin: PhysicalRect,
    edge: DragEdge,
    pointer: (i32, i32),
    monitor: PhysicalRect,
    min_size: i32,
) -> PhysicalRect {
    // 先把可移动的边夹进显示器，且离固定边至少 min_size（显示器过小时退化为贴固定边的最大空间）
    let span = |fixed_low: bool, fixed: i32, p: i32, lo: i32, hi: i32| -> (i32, i32) {
        if fixed_low {
            let moving = p.clamp((fixed + min_size).min(hi), hi.max(fixed));
            (fixed, moving - fixed)
        } else {
            let moving = p.clamp(lo.min(fixed), (fixed - min_size).max(lo));
            (moving, fixed - moving)
        }
    };
    match edge {
        // 平移走 move_region，这里不会用到
        DragEdge::Move => origin,
        DragEdge::Top => {
            let (y, h) = span(
                false,
                origin.bottom(),
                pointer.1,
                monitor.y,
                monitor.bottom(),
            );
            PhysicalRect::new(origin.x, y, origin.width, h)
        }
        DragEdge::Bottom => {
            let (y, h) = span(true, origin.y, pointer.1, monitor.y, monitor.bottom());
            PhysicalRect::new(origin.x, y, origin.width, h)
        }
        DragEdge::Left => {
            let (x, w) = span(false, origin.right(), pointer.0, monitor.x, monitor.right());
            PhysicalRect::new(x, origin.y, w, origin.height)
        }
        DragEdge::Right => {
            let (x, w) = span(true, origin.x, pointer.0, monitor.x, monitor.right());
            PhysicalRect::new(x, origin.y, w, origin.height)
        }
    }
}

/// 当前格式是否支持音频（仅 MP4）。
pub const fn audio_available(format: RecordingFormat) -> bool {
    matches!(format, RecordingFormat::Mp4)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 1920x1080 主屏。
    const MAIN: PhysicalRect = PhysicalRect::new(0, 0, 1920, 1080);

    /// 取结果的 (宽, 高)。
    fn size(r: PhysicalRect) -> (i32, i32) {
        (r.width, r.height)
    }

    /// 800x600 的选区，中心 (960, 540)。
    fn base() -> PhysicalRect {
        PhysicalRect::new(560, 240, 800, 600)
    }

    /// 各比例在选区内取最大且偶数，且以选区中心为基准。
    #[test]
    fn ratios_fit_inside_selection_and_center() {
        let cases = [
            (RangePreset::Ratio16x9, (800, 450)),
            (RangePreset::Ratio4x3, (800, 600)),
            (RangePreset::Square, (600, 600)),
            (RangePreset::Portrait9x16, (336, 600)),
            (RangePreset::PhonePortrait, (276, 600)),
            (RangePreset::TabletPortrait, (418, 600)),
            (RangePreset::Portrait3x4, (450, 600)),
        ];
        for (preset, want) in cases {
            let r = apply_range_preset(preset, base(), MAIN);
            assert_eq!(size(r), want, "{preset:?}");
            assert_eq!(
                (r.x + r.width / 2, r.y + r.height / 2),
                (960, 540),
                "{preset:?}"
            );
            assert!(base().intersect(&r) == Some(r), "{preset:?} 必须在选区内");
        }
    }

    /// 全屏返回整个显示器。
    #[test]
    fn full_screen_is_whole_monitor() {
        assert_eq!(
            apply_range_preset(RangePreset::FullScreen, base(), MAIN),
            MAIN
        );
    }

    /// 副屏带偏移原点：结果仍在该显示器内。
    #[test]
    fn offset_monitor_stays_inside() {
        let left = PhysicalRect::new(-1920, 0, 1920, 1080);
        let cur = PhysicalRect::new(-1000, 300, 400, 300);
        let r = apply_range_preset(RangePreset::Ratio16x9, cur, left);
        assert_eq!(size(r), (400, 224));
        assert!(r.x >= left.x && r.right() <= left.right());
        assert_eq!(apply_range_preset(RangePreset::FullScreen, cur, left), left);
    }

    /// 输出档：按选区等比缩小；小选区不放大。
    #[test]
    fn output_presets() {
        assert_eq!(output_size_for(OutputPreset::Original, 2560, 1440), None);
        assert_eq!(
            output_size_for(OutputPreset::P1080, 2560, 1440),
            Some((1920, 1080))
        );
        assert_eq!(
            output_size_for(OutputPreset::P720, 2560, 1440),
            Some((1280, 720))
        );
        assert_eq!(
            output_size_for(OutputPreset::P480, 1440, 2560),
            Some((480, 852))
        );
        // 选区短边不高于目标：不放大
        assert_eq!(output_size_for(OutputPreset::P1080, 1280, 720), None);
        assert!(is_upscale(OutputPreset::P1080, 1280, 720));
        assert!(!is_upscale(OutputPreset::P720, 1280, 721));
        assert!(!is_upscale(OutputPreset::Original, 10, 10));
    }

    /// 恢复预设在纯函数里原样返回当前选区。
    #[test]
    fn restore_is_identity_in_pure_fn() {
        assert_eq!(
            apply_range_preset(RangePreset::Restore, base(), MAIN),
            base()
        );
        assert_eq!(RangePreset::ALL[0], RangePreset::Restore);
    }

    /// 拖拽测试用选区。
    fn dragged() -> PhysicalRect {
        PhysicalRect::new(400, 300, 800, 400)
    }

    /// 整体平移：尺寸不变，夹在显示器内。
    #[test]
    fn move_region_keeps_size_and_clamps() {
        let o = dragged();
        let r = move_region(o, (50, -20), MAIN);
        assert_eq!((r.x, r.y, r.width, r.height), (450, 280, 800, 400));
        let r = move_region(o, (-9999, -9999), MAIN);
        assert_eq!((r.x, r.y), (MAIN.x, MAIN.y));
        let r = move_region(o, (9999, 9999), MAIN);
        assert_eq!((r.right(), r.bottom()), (MAIN.right(), MAIN.bottom()));
    }

    /// 四条边各自拖大 / 拖小，对边固定、另一维度不变。
    #[test]
    fn drag_each_edge_grow_and_shrink() {
        let o = dragged();
        let m = MIN_SELECTION_SIZE;
        let r = drag_edge(o, DragEdge::Top, (0, 200), MAIN, m);
        assert_eq!((r.y, r.bottom(), r.x, r.width), (200, 700, 400, 800));
        let r = drag_edge(o, DragEdge::Top, (0, 400), MAIN, m);
        assert_eq!((r.y, r.bottom()), (400, 700));
        let r = drag_edge(o, DragEdge::Bottom, (0, 900), MAIN, m);
        assert_eq!((r.y, r.bottom(), r.x, r.width), (300, 900, 400, 800));
        let r = drag_edge(o, DragEdge::Bottom, (0, 500), MAIN, m);
        assert_eq!((r.y, r.bottom()), (300, 500));
        let r = drag_edge(o, DragEdge::Left, (300, 0), MAIN, m);
        assert_eq!((r.x, r.right(), r.y, r.height), (300, 1200, 300, 400));
        let r = drag_edge(o, DragEdge::Left, (600, 0), MAIN, m);
        assert_eq!((r.x, r.right()), (600, 1200));
        let r = drag_edge(o, DragEdge::Right, (1500, 0), MAIN, m);
        assert_eq!((r.x, r.right(), r.y, r.height), (400, 1500, 300, 400));
        let r = drag_edge(o, DragEdge::Right, (900, 0), MAIN, m);
        assert_eq!((r.x, r.right()), (400, 900));
    }

    /// 拖出屏幕被夹回显示器边界。
    #[test]
    fn drag_clamped_to_monitor() {
        let o = dragged();
        let m = MIN_SELECTION_SIZE;
        assert_eq!(drag_edge(o, DragEdge::Top, (0, -500), MAIN, m).y, 0);
        assert_eq!(
            drag_edge(o, DragEdge::Bottom, (0, 5000), MAIN, m).bottom(),
            1080
        );
        assert_eq!(drag_edge(o, DragEdge::Left, (-9, 0), MAIN, m).x, 0);
        assert_eq!(
            drag_edge(o, DragEdge::Right, (9999, 0), MAIN, m).right(),
            1920
        );
    }

    /// 拖过对边不翻转，钳在最小尺寸。
    #[test]
    fn drag_past_opposite_edge_keeps_min_size() {
        let o = dragged();
        let m = MIN_SELECTION_SIZE;
        let r = drag_edge(o, DragEdge::Top, (0, 1000), MAIN, m);
        assert_eq!((r.height, r.bottom()), (m, 700));
        let r = drag_edge(o, DragEdge::Bottom, (0, 0), MAIN, m);
        assert_eq!((r.y, r.height), (300, m));
        let r = drag_edge(o, DragEdge::Left, (1900, 0), MAIN, m);
        assert_eq!((r.width, r.right()), (m, 1200));
        let r = drag_edge(o, DragEdge::Right, (0, 0), MAIN, m);
        assert_eq!((r.x, r.width), (400, m));
    }

    /// 带负原点的副屏：夹在副屏范围内。
    #[test]
    fn drag_on_negative_origin_monitor() {
        let left = PhysicalRect::new(-1920, 0, 1920, 1080);
        let o = PhysicalRect::new(-1500, 200, 600, 400);
        let m = MIN_SELECTION_SIZE;
        assert_eq!(drag_edge(o, DragEdge::Left, (-3000, 0), left, m).x, -1920);
        assert_eq!(drag_edge(o, DragEdge::Right, (500, 0), left, m).right(), 0);
        let r = drag_edge(o, DragEdge::Left, (-1000, 0), left, m);
        assert_eq!((r.x, r.right()), (-1000, -900));
        let r = drag_edge(o, DragEdge::Top, (0, -50), left, m);
        assert_eq!((r.y, r.bottom()), (0, 600));
        let r = drag_edge(o, DragEdge::Bottom, (0, 2000), left, m);
        assert_eq!(r.bottom(), 1080);
    }

    /// 音频仅 MP4 可用。
    #[test]
    fn audio_only_for_mp4() {
        assert!(audio_available(RecordingFormat::Mp4));
        for f in [
            RecordingFormat::Gif,
            RecordingFormat::Apng,
            RecordingFormat::Webp,
        ] {
            assert!(!audio_available(f));
        }
    }
}
