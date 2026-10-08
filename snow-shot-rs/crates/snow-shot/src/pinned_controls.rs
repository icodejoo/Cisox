//! 贴图点击穿透时的「退出」按钮小窗：贴图本身不再接收鼠标，只有这个独立小窗能点。
//!
//! 位置计算是纯函数（可离屏测试），窗口本身只含一个按钮，点击后经收件箱通知主线程退出穿透。

use crate::app_runtime::UiEvent;
use snow_ui::shell::geometry::PhysicalRect;
use snow_ui::shell::inbox::MainThreadInbox;
use snow_ui::ui::component::button::Button;
use snow_ui::ui::*;

/// 退出按钮边长（逻辑像素，同旧版 `kControlHeight`）。
pub const CONTROL_SIZE: f32 = 32.0;
/// 退出按钮与贴图右上角的内缩距离（逻辑像素，同旧版）。
const CONTROL_INSET: f32 = 16.0;

/// 计算退出按钮的屏幕外框：贴在贴图右上角上方，并钳制在所在显示器可用区域内。
///
/// # 参数
/// - `pin`：贴图当前外框（物理像素）。
/// - `bounds`：贴图所在显示器的可用区域（物理像素）。
/// - `scale`：显示器缩放比。
///
/// # 返回
/// 退出按钮外框；显示器小到放不下按钮时返回 `None`。
///
/// ```ignore
/// let r = exit_button_rect(PhysicalRect::new(100, 100, 400, 300), PhysicalRect::new(0, 0, 1920, 1080), 1.0).unwrap();
/// assert_eq!((r.width, r.height), (32, 32));
/// ```
pub fn exit_button_rect(
    pin: PhysicalRect,
    bounds: PhysicalRect,
    scale: f32,
) -> Option<PhysicalRect> {
    let size = ((CONTROL_SIZE * scale).round() as i32).max(1);
    let inset = ((CONTROL_INSET * scale).round() as i32).max(1);
    if bounds.width < size || bounds.height < size {
        return None;
    }
    let left = (pin.right() - inset - size).clamp(bounds.x, bounds.right() - size);
    let top = (pin.y - inset - size).clamp(bounds.y, bounds.bottom() - size);
    Some(PhysicalRect::new(left, top, size, size))
}

/// 隐藏到顶部的把手宽（逻辑像素，同旧版）。
const HANDLE_WIDTH: f32 = 30.0;
/// 隐藏到顶部的把手高（逻辑像素，同旧版）。
pub const HANDLE_HEIGHT: f32 = 6.0;

/// 计算「隐藏到顶部」把手的屏幕外框：贴在工作区上边缘，水平居中于贴图并钳制在工作区内。
///
/// # 参数
/// - `pin`：贴图隐藏前的外框（物理像素）。
/// - `work`：所在显示器工作区（物理像素）。
/// - `scale`：显示器缩放比。
///
/// # 返回
/// 把手外框；工作区放不下时 `None`。
///
/// ```ignore
/// let h = hide_handle_rect(PhysicalRect::new(500, 300, 400, 200), PhysicalRect::new(0, 0, 1920, 1040), 1.0).unwrap();
/// assert_eq!((h.y, h.width, h.height), (0, 30, 6));
/// ```
pub fn hide_handle_rect(pin: PhysicalRect, work: PhysicalRect, scale: f32) -> Option<PhysicalRect> {
    let w = ((HANDLE_WIDTH * scale).round() as i32).max(1);
    let h = ((HANDLE_HEIGHT * scale).round() as i32).max(1);
    if work.width < w || work.height < h {
        return None;
    }
    let left = (pin.x + pin.width / 2 - w / 2).clamp(work.x, work.right() - w);
    Some(PhysicalRect::new(left, work.y, w, h))
}

/// 计算「隐藏到顶部」时鼠标移到把手上后贴图滑出的外框：紧贴把手下方，水平钳制在工作区内。
///
/// # 参数
/// - `pin`：贴图隐藏前的外框。
/// - `handle`：把手外框。
/// - `work`：工作区。
pub fn revealed_rect(pin: PhysicalRect, handle: PhysicalRect, work: PhysicalRect) -> PhysicalRect {
    let max_left = (work.right() - pin.width).max(work.x);
    let left = pin.x.clamp(work.x, max_left);
    PhysicalRect::new(left, handle.bottom(), pin.width, pin.height)
}

/// 「隐藏到顶部」把手小窗的视图：鼠标移入即请求滑出贴图，点击则恢复原位。
pub struct PinHideHandle {
    /// 所属贴图 ID。
    id: String,
    /// 主线程收件箱。
    inbox: MainThreadInbox<UiEvent>,
}

impl PinHideHandle {
    /// 创建视图。
    ///
    /// # 参数
    /// - `id`：所属贴图 ID。
    /// - `inbox`：主线程收件箱。
    pub fn new(id: String, inbox: MainThreadInbox<UiEvent>) -> Self {
        Self { id, inbox }
    }
}

impl Render for PinHideHandle {
    /// 渲染一条深色细把手。
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let hover_id = self.id.clone();
        let hover_inbox = self.inbox.clone();
        let click_id = self.id.clone();
        let click_inbox = self.inbox.clone();
        div()
            .id("pin-hide-handle")
            .size_full()
            .bg(rgba(0x3A3A3AFF))
            .on_hover(cx.listener(move |_this, hovered: &bool, _window, _cx| {
                if *hovered {
                    hover_inbox.push(UiEvent::PinHideReveal {
                        id: hover_id.clone(),
                    });
                }
            }))
            .on_click(
                cx.listener(move |_this, _event: &ClickEvent, _window, _cx| {
                    click_inbox.push(UiEvent::PinExitHideToTop {
                        id: click_id.clone(),
                    });
                }),
            )
    }
}

/// 退出按钮小窗的视图。
pub struct PinExitControl {
    /// 所属贴图 ID。
    id: String,
    /// 主线程收件箱。
    inbox: MainThreadInbox<UiEvent>,
}

impl PinExitControl {
    /// 创建视图。
    ///
    /// # 参数
    /// - `id`：所属贴图 ID。
    /// - `inbox`：主线程收件箱。
    pub fn new(id: String, inbox: MainThreadInbox<UiEvent>) -> Self {
        Self { id, inbox }
    }
}

impl Render for PinExitControl {
    /// 渲染一个关闭按钮。
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let id = self.id.clone();
        let inbox = self.inbox.clone();
        div().size_full().child(
            Button::new("pin-exit-click-through")
                .label("✕")
                .w_full()
                .h_full()
                .on_click(
                    cx.listener(move |_this, _event: &ClickEvent, _window, _cx| {
                        inbox.push(UiEvent::PinExitClickThrough { id: id.clone() });
                    }),
                ),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 把手居中于贴图并钳制；滑出外框紧贴把手下方。
    #[test]
    fn hide_handle_and_reveal_geometry() {
        let work = PhysicalRect::new(0, 0, 1920, 1040);
        let pin = PhysicalRect::new(500, 300, 400, 200);
        let h = hide_handle_rect(pin, work, 1.0).unwrap();
        assert_eq!((h.x, h.y, h.width, h.height), (685, 0, 30, 6));
        assert_eq!(
            hide_handle_rect(PhysicalRect::new(-500, 0, 100, 100), work, 1.0)
                .unwrap()
                .x,
            0
        );
        let r = revealed_rect(pin, h, work);
        assert_eq!((r.x, r.y, r.width, r.height), (500, 6, 400, 200));
        let edge = revealed_rect(PhysicalRect::new(1800, 300, 400, 200), h, work);
        assert_eq!(edge.x, 1520);
    }

    /// 默认贴在贴图右上角上方；顶部放不下时钳制进显示器。
    #[test]
    fn exit_button_sits_above_top_right_and_clamps() {
        let screen = PhysicalRect::new(0, 0, 1920, 1080);
        let r = exit_button_rect(PhysicalRect::new(100, 200, 400, 300), screen, 1.0).unwrap();
        assert_eq!(
            (r.x, r.y, r.width, r.height),
            (500 - 16 - 32, 200 - 16 - 32, 32, 32)
        );
        let top = exit_button_rect(PhysicalRect::new(100, 10, 400, 300), screen, 1.0).unwrap();
        assert_eq!(top.y, 0);
        let right = exit_button_rect(PhysicalRect::new(1800, 200, 400, 300), screen, 1.0).unwrap();
        assert_eq!(right.x, 1920 - 32);
    }

    /// 缩放比放大边长；显示器放不下时无结果。
    #[test]
    fn exit_button_scales_and_rejects_tiny_screens() {
        let screen = PhysicalRect::new(0, 0, 1920, 1080);
        let r = exit_button_rect(PhysicalRect::new(100, 200, 400, 300), screen, 1.5).unwrap();
        assert_eq!(r.width, 48);
        assert!(
            exit_button_rect(
                PhysicalRect::new(0, 0, 10, 10),
                PhysicalRect::new(0, 0, 20, 20),
                1.0
            )
            .is_none()
        );
    }
}
