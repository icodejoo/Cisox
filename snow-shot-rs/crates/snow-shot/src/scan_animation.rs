//! OCR 期间的“扫描线 + 拖尾”动画：在被识别的图像区域上，一条主题色横线自上而下循环扫动，
//! 线上方跟一段渐隐的半透明拖尾。
//!
//! 动画层不拦截鼠标事件（不 occlude、不挂回调），不影响选区交互和结果面板；整体裁剪在目标矩形内。
//! 帧率封顶 [`MAX_FPS`]；系统开了“减少动态效果”时 GPUI 会自动退回静态。
//! 位置计算（[`scan_frame`]）是不依赖 GPUI 的纯函数，可单测。

use crate::overlay_view::ACCENT_COLOR;
use snow_ui::ui::*;
use std::time::Duration;

/// 扫描线与拖尾的颜色（与覆盖窗主题强调色一致）。
pub const SCAN_COLOR: u32 = ACCENT_COLOR;
/// 扫描线高度（逻辑像素）。
const SCAN_LINE_HEIGHT: f32 = 2.0;
/// 拖尾高度占目标矩形高度的比例。
const TRAIL_RATIO: f32 = 0.3;
/// 拖尾最小高度（逻辑像素）。
const TRAIL_MIN: f32 = 16.0;
/// 拖尾最大高度（逻辑像素）。
const TRAIL_MAX: f32 = 120.0;
/// 拖尾贴近扫描线一端的不透明度（往上渐隐到 0）。
const TRAIL_ALPHA: f32 = 0.35;
/// 扫描一轮（从顶到底）的时长。
const SCAN_PERIOD: Duration = Duration::from_millis(1600);
/// 动画最高刷新率（帧/秒），扫描提示不需要 60 帧。
const MAX_FPS: f32 = 30.0;
/// 渐变方向：自上而下（0 度朝上，顺时针增加）。
const GRADIENT_TO_BOTTOM: f32 = 180.0;

/// 某一时刻扫描线与拖尾的位置（逻辑像素，相对目标矩形左上角）。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ScanFrame {
    /// 扫描线顶边的 y。
    pub line_y: f32,
    /// 拖尾可见部分的顶边 y（不小于 0）。
    pub trail_top: f32,
    /// 拖尾可见部分的高度（不小于 0；刚从顶部开始时拖尾还没完全露出，小于 [`Self::trail_full`]）。
    pub trail_height: f32,
    /// 拖尾完整高度（渐变按它铺满，露出一部分时只裁剪、不压缩渐变）。
    pub trail_full: f32,
}

/// 拖尾完整高度：矩形高度的 [`TRAIL_RATIO`]，夹在 `[TRAIL_MIN, TRAIL_MAX]`，且不超过矩形高度。
///
/// # 参数
/// - `height`：目标矩形高度（逻辑像素）。
fn trail_full_height(height: f32) -> f32 {
    (height * TRAIL_RATIO)
        .clamp(TRAIL_MIN, TRAIL_MAX)
        .min(height.max(0.0))
}

/// 按动画进度算扫描线与拖尾位置。
///
/// # 参数
/// - `delta`：动画进度，0 在顶部、1 在底部（超出范围按边界处理）。
/// - `height`：目标矩形高度（逻辑像素）。
///
/// # 返回
/// 扫描线与拖尾的位置；拖尾顶部被矩形上沿裁掉的部分不会出现负高度。
///
/// ```ignore
/// let frame = scan_frame(0.5, 200.0);
/// assert_eq!(frame.line_y, 99.0);
/// ```
pub fn scan_frame(delta: f32, height: f32) -> ScanFrame {
    let height = height.max(0.0);
    let delta = delta.clamp(0.0, 1.0);
    let line_y = delta * (height - SCAN_LINE_HEIGHT).max(0.0);
    let trail_full = trail_full_height(height);
    let trail_top = (line_y - trail_full).max(0.0);
    ScanFrame {
        line_y,
        trail_top,
        trail_height: line_y - trail_top,
        trail_full,
    }
}

/// 在目标矩形上叠一层循环的扫描动画（绝对定位，调用方放进相对定位的父容器里）。
///
/// # 参数
/// - `id`：动画元素的稳定 ID（同一父容器里不同扫描层要不同）。
/// - `left`、`top`：矩形左上角（逻辑像素，相对父容器）。
/// - `width`、`height`：矩形大小（逻辑像素）。
///
/// # 返回
/// 扫描层元素；识别结束后不再返回它即可让动画消失。
///
/// ```ignore
/// root = root.child(scan_overlay("pin-ocr-scan", px(0.0), px(0.0), px(300.0), px(200.0)));
/// ```
pub fn scan_overlay(
    id: impl Into<ElementId>,
    left: Pixels,
    top: Pixels,
    width: Pixels,
    height: Pixels,
) -> AnyElement {
    let rect_height = f32::from(height);
    let layer = div().absolute().top(px(0.0)).left(px(0.0)).size_full();
    let layer = layer.with_animation(
        id,
        Animation::new(SCAN_PERIOD).repeat().with_max_fps(MAX_FPS),
        move |layer, delta| {
            let frame = scan_frame(delta, rect_height);
            layer
                .when(frame.trail_height > 0.0, |layer| {
                    layer.child(trail_element(&frame))
                })
                .child(
                    div()
                        .absolute()
                        .left(px(0.0))
                        .top(px(frame.line_y))
                        .w_full()
                        .h(px(SCAN_LINE_HEIGHT))
                        .bg(rgb(SCAN_COLOR)),
                )
        },
    );
    div()
        .absolute()
        .left(left)
        .top(top)
        .w(width)
        .h(height)
        .overflow_hidden()
        .child(layer)
        .into_any_element()
}

/// 拖尾元素：外层按可见部分裁剪，内层是贴底的完整渐变（上透明、下半透明）。
///
/// # 参数
/// - `frame`：当前帧位置。
fn trail_element(frame: &ScanFrame) -> Div {
    let color: Hsla = rgb(SCAN_COLOR).into();
    div()
        .absolute()
        .left(px(0.0))
        .top(px(frame.trail_top))
        .w_full()
        .h(px(frame.trail_height))
        .overflow_hidden()
        .child(
            div()
                .absolute()
                .left(px(0.0))
                .bottom(px(0.0))
                .w_full()
                .h(px(frame.trail_full))
                .bg(linear_gradient(
                    GRADIENT_TO_BOTTOM,
                    linear_color_stop(color.opacity(0.0), 0.0),
                    linear_color_stop(color.opacity(TRAIL_ALPHA), 1.0),
                )),
        )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 浮点近似相等。
    fn near(a: f32, b: f32) -> bool {
        (a - b).abs() < 1e-3
    }

    /// 起点：线在最顶，拖尾还没露出。
    #[test]
    fn frame_at_start() {
        let f = scan_frame(0.0, 200.0);
        assert!(near(f.line_y, 0.0));
        assert!(near(f.trail_height, 0.0));
        assert!(near(f.trail_top, 0.0));
    }

    /// 中点：线在可移动范围正中，拖尾完整露出（高度 = 矩形的 30%）。
    #[test]
    fn frame_at_middle() {
        let f = scan_frame(0.5, 200.0);
        assert!(near(f.line_y, 99.0));
        assert!(near(f.trail_full, 60.0));
        assert!(near(f.trail_height, 60.0));
        assert!(near(f.trail_top, 39.0));
    }

    /// 接近终点：线贴着底边（不越界），拖尾完整。
    #[test]
    fn frame_near_end() {
        let f = scan_frame(0.999, 200.0);
        assert!(f.line_y > 197.0 && f.line_y + SCAN_LINE_HEIGHT <= 200.0);
        assert!(near(f.trail_height, 60.0));
        assert!(near(f.trail_top + f.trail_height, f.line_y));
    }

    /// 拖尾高度夹取：太矮取下限，太高取上限，矩形比下限还矮时不超过矩形。
    #[test]
    fn trail_height_is_clamped() {
        assert!(near(scan_frame(0.5, 40.0).trail_full, TRAIL_MIN));
        assert!(near(scan_frame(0.5, 1000.0).trail_full, TRAIL_MAX));
        assert!(near(scan_frame(0.5, 10.0).trail_full, 10.0));
        assert!(near(scan_frame(0.5, 0.0).trail_full, 0.0));
    }

    /// 拖尾刚出发时被矩形上沿裁掉：可见高度等于线的位置，永远不出现负数。
    #[test]
    fn trail_never_negative() {
        for height in [0.0_f32, 1.0, 10.0, 40.0, 200.0, 1000.0] {
            for i in 0..=100 {
                let f = scan_frame(i as f32 / 100.0, height);
                assert!(f.trail_top >= 0.0, "top h={height} i={i}");
                assert!(f.trail_height >= 0.0, "height h={height} i={i}");
                assert!(f.trail_height <= f.trail_full + 1e-3, "h={height} i={i}");
                assert!(
                    near(f.trail_top + f.trail_height, f.line_y),
                    "h={height} i={i}"
                );
            }
        }
        let early = scan_frame(0.05, 200.0);
        assert!(early.line_y < early.trail_full);
        assert!(near(early.trail_height, early.line_y));
    }

    /// 进度超出 0..1 时按边界处理。
    #[test]
    fn delta_is_clamped() {
        assert_eq!(scan_frame(-1.0, 200.0), scan_frame(0.0, 200.0));
        assert_eq!(scan_frame(2.0, 200.0), scan_frame(1.0, 200.0));
    }
}
