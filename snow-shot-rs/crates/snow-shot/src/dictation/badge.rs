//! 语音转文字的“聆听指示”：只键入、不弹浮窗时，在主屏幕右下角显示一个整窗半透明黑色蒙层的耳朵图标。
//!
//! 窗口对输入透明且不抢焦点（鼠标穿透、不会改变键入目标），仅作提示。动画是一圈向外扩散的光环加
//! 耳朵图标的轻微呼吸；系统开了“减少动态效果”时 GPUI 会自动退回静态，帧率也限制在较低值以省资源。

use snow_ui::shell::geometry::{PhysicalRect, ScaleFactor};
use snow_ui::ui::component::progress::Progress;
use snow_ui::ui::component::{Icon, Sizable, Size as ComponentSize};
use snow_ui::ui::*;
use std::time::Duration;

/// 指示窗口的逻辑边长（含光环扩散的范围）。
pub const BADGE_SIZE: f32 = 76.0;
/// 指示窗口距主屏工作区右、下边缘的逻辑边距。
pub const BADGE_MARGIN: f32 = 20.0;
/// 光环起始大小（略大于耳朵图标）。
const RING_START: f32 = 40.0;
/// 耳朵图标的边长。
const EAR_SIZE: f32 = 28.0;
/// 光环从起始大小扩散到窗口大小的一轮时长。
const RING_PERIOD: Duration = Duration::from_millis(1800);
/// 耳朵呼吸一轮的时长。
const BREATH_PERIOD: Duration = Duration::from_millis(2400);
/// 动画最高刷新率（帧/秒），指示本身不需要 60 帧。
const MAX_FPS: f32 = 30.0;
/// 整窗蒙层底色（纯黑；半透明由窗口整体 alpha 提供）。
const MASK_BG: u32 = 0x0000_00FF;
/// 窗口整体不透明度（0~255，约 55%）。
pub const BADGE_WINDOW_ALPHA: u8 = 140;
/// 光环与耳朵图标颜色（白色）。
const FOREGROUND: u32 = 0x00FF_FFFF;
/// 加载进度条宽度（逻辑像素），位于耳朵图标下方。
const LOADING_BAR_WIDTH: f32 = 36.0;
/// 加载进度条距窗口底边的逻辑距离。
const LOADING_BAR_BOTTOM: f32 = 10.0;
/// 耳朵图标资源路径（Material Icons 的 hearing，自绘图标表里）。
const EAR_ICON: &str = "icons/snow/hearing.svg";

/// 聆听指示视图（动画由 GPUI 驱动）；模型未就绪时在图标下方显示循环进度条，并暂停光环。
pub struct ListeningBadge {
    /// 引擎是否还在启动 / 加载模型（此时说话不会被识别）。
    loading: bool,
}

impl ListeningBadge {
    /// 创建视图。
    ///
    /// # 参数
    /// - `app`：应用上下文。
    pub fn create(app: &mut App) -> Entity<Self> {
        app.new(|_cx| Self { loading: true })
    }

    /// 切换“加载中”提示；状态没变时不重绘。
    ///
    /// # 参数
    /// - `loading`：引擎是否尚未就绪。
    pub fn set_loading(&mut self, loading: bool, cx: &mut Context<Self>) {
        if self.loading != loading {
            self.loading = loading;
            cx.notify();
        }
    }
}

impl Render for ListeningBadge {
    /// 画光环 + 整窗半透明蒙层 + 耳朵图标；加载中改画进度条、不画光环。
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        let ring = div()
            .absolute()
            .rounded_full()
            .border_2()
            .border_color(rgba((FOREGROUND << 8) | 0xFF))
            .with_animation(
                "badge-ring",
                Animation::new(RING_PERIOD).repeat().with_max_fps(MAX_FPS),
                |ring, delta| {
                    let (size, alpha) = ring_frame(delta);
                    let offset = (BADGE_SIZE - size) / 2.0;
                    ring.size(px(size))
                        .top(px(offset))
                        .left(px(offset))
                        .opacity(alpha)
                },
            );
        let ear = Icon::empty()
            .path(EAR_ICON)
            .size(px(EAR_SIZE))
            .text_color(rgb(FOREGROUND))
            .with_animation(
                "badge-ear",
                Animation::new(BREATH_PERIOD).repeat().with_max_fps(MAX_FPS),
                |ear, delta| ear.opacity(breath_alpha(delta)),
            );
        // 整个窗口铺半透明黑色，耳朵居中，光环从耳朵外向窗口边缘扩散
        div()
            .size_full()
            .relative()
            .bg(rgba(MASK_BG))
            .flex()
            .items_center()
            .justify_center()
            .when(!self.loading, |this| this.child(ring))
            .child(ear)
            .when(self.loading, |this| {
                // 未就绪：图标下方一条循环进度条，提示模型还在加载
                this.child(
                    div()
                        .absolute()
                        .bottom(px(LOADING_BAR_BOTTOM))
                        .left(px((BADGE_SIZE - LOADING_BAR_WIDTH) / 2.0))
                        .w(px(LOADING_BAR_WIDTH))
                        .child(
                            Progress::new("badge-loading")
                                .loading(true)
                                .color(rgb(FOREGROUND))
                                .with_size(ComponentSize::XSmall),
                        ),
                )
            })
    }
}

/// 光环某一帧的（边长，不透明度）：由起始大小扩到窗口大小，同时由淡入到完全消失。
///
/// # 参数
/// - `delta`：本轮进度，`0.0..=1.0`。
///
/// # 返回
/// 光环边长（逻辑像素）与不透明度。
///
/// ```ignore
/// let (size, alpha) = ring_frame(0.0);
/// assert_eq!(size, 52.0);
/// ```
pub fn ring_frame(delta: f32) -> (f32, f32) {
    let t = delta.clamp(0.0, 1.0);
    let eased = 1.0 - (1.0 - t) * (1.0 - t);
    (
        RING_START + (BADGE_SIZE - RING_START) * eased,
        0.55 * (1.0 - t),
    )
}

/// 耳朵图标某一帧的不透明度：在 0.65 与 1.0 之间平滑往返。
///
/// # 参数
/// - `delta`：本轮进度，`0.0..=1.0`。
pub fn breath_alpha(delta: f32) -> f32 {
    let wave = (delta.clamp(0.0, 1.0) * std::f32::consts::TAU).sin() * 0.5 + 0.5;
    0.65 + 0.35 * wave
}

/// 计算指示窗口在主屏工作区右下角的物理矩形（留边距，不盖任务栏）。
///
/// # 参数
/// - `work_area`：主屏工作区（去掉任务栏，物理像素）。
/// - `scale`：该显示器的缩放比。
///
/// # 返回
/// 窗口物理矩形；工作区过小时收缩到工作区内。
///
/// ```ignore
/// let rect = badge_rect(PhysicalRect::new(0, 0, 1920, 1040), ScaleFactor::ONE);
/// assert_eq!((rect.x + rect.width, rect.y + rect.height), (1920 - 20, 1040 - 20));
/// ```
pub fn badge_rect(work_area: PhysicalRect, scale: ScaleFactor) -> PhysicalRect {
    let margin = scale.to_physical(BADGE_MARGIN);
    let side = scale
        .to_physical(BADGE_SIZE)
        .min(work_area.width)
        .min(work_area.height);
    let x = (work_area.x + work_area.width - margin - side).max(work_area.x);
    let y = (work_area.y + work_area.height - margin - side).max(work_area.y);
    PhysicalRect::new(x, y, side, side)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 光环起点是起始大小且可见，终点是窗口大小且完全透明。
    #[test]
    fn ring_runs_from_disc_to_window() {
        let (s0, a0) = ring_frame(0.0);
        let (s1, a1) = ring_frame(1.0);
        assert_eq!(s0, RING_START);
        assert!(a0 > 0.0);
        assert_eq!(s1, BADGE_SIZE);
        assert_eq!(a1, 0.0);
        let (mid, _) = ring_frame(0.5);
        assert!(mid > s0 && mid < s1);
    }

    /// 呼吸不透明度始终落在 0.65..=1.0，越界进度被夹住。
    #[test]
    fn breath_stays_in_range() {
        for i in 0..=100 {
            let a = breath_alpha(i as f32 / 100.0);
            assert!((0.65..=1.0).contains(&a), "{a}");
        }
        assert!((0.65..=1.0).contains(&breath_alpha(7.0)));
    }

    /// 落在工作区右下角并留边距；缩放时边距与边长按比例放大。
    #[test]
    fn placed_bottom_right_with_margin() {
        let rect = badge_rect(PhysicalRect::new(0, 0, 1920, 1040), ScaleFactor::ONE);
        assert_eq!(rect.width, BADGE_SIZE as i32);
        assert_eq!(rect.x + rect.width, 1920 - BADGE_MARGIN as i32);
        assert_eq!(rect.y + rect.height, 1040 - BADGE_MARGIN as i32);
    }

    /// 工作区小于窗口时收缩进工作区，不越界。
    #[test]
    fn tiny_work_area_clamps() {
        let rect = badge_rect(PhysicalRect::new(100, 50, 40, 30), ScaleFactor::ONE);
        assert!(rect.width <= 30 && rect.x >= 100 && rect.y >= 50);
    }
}
