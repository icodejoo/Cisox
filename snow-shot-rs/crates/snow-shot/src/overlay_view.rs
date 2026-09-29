//! 截图全屏交互覆盖窗视图（Screenshot Overlay View）。
//!
//! 承载全屏冻结底图、四向半透明暗化遮罩、动态矩形选区与八向手柄、
//! 局部取色放大镜、自适应浮动工具栏以及鼠标/键盘交互状态机驱动。

use snow_platform::capture::CapturedScreen;
use snow_platform::clipboard::{copy_image_to_clipboard, copy_text_to_clipboard};
use snow_ui::shell::geometry::{PhysicalPoint, PhysicalRect};
use snow_ui::shell::selection::{
    DEFAULT_EDGE_TOLERANCE, DEFAULT_HANDLE_SIZE, DEFAULT_MINIMUM_SELECTION_SIZE,
    SelectionDragMode, SelectionState, dragged_selection_rect, handle_rects,
    hit_test_drag_mode, marquee_selection_rect, selection_size_label,
};
use snow_ui::ui::*;
use snow_ui::widgets::{
    AnnotationTool, ColorFormat, Magnifier, MagnifierGrid, ScreenshotToolbar, ToolbarAction,
    calculate_magnifier_placement, calculate_toolbar_placement,
};

/// 截图覆盖窗主视图组件。
pub struct ScreenshotOverlayView {
    /// 捕获的原始物理屏幕底图。
    captured_screen: CapturedScreen,
    /// 屏幕物理边界尺寸。
    screen_bounds: PhysicalRect,
    /// 交互选区状态。
    state: SelectionState,
    /// 当前鼠标物理坐标。
    cursor_pos: PhysicalPoint,
    /// 放大镜采样数据。
    magnifier_grid: MagnifierGrid,
    /// 色彩格式。
    color_format: ColorFormat,
    /// 当前激活的标注工具。
    active_tool: AnnotationTool,
    /// 状态提示文本。
    status_message: Option<String>,
}

impl ScreenshotOverlayView {
    /// 创建覆盖窗主视图。
    ///
    /// # 参数
    /// - `captured_screen`: 截取的屏幕底图帧。
    ///
    /// # 返回
    /// 覆盖窗视图实例。
    ///
    /// # 示例
    /// ```no_run
    /// use snow_platform::capture::CapturedScreen;
    /// use snow_shot::overlay_view::ScreenshotOverlayView;
    /// let screen = CapturedScreen::new_solid(1920, 1080, (0, 0, 0, 255));
    /// let _view = ScreenshotOverlayView::new(screen);
    /// ```
    pub fn new(captured_screen: CapturedScreen) -> Self {
        let screen_bounds = PhysicalRect::new(
            0,
            0,
            captured_screen.width as i32,
            captured_screen.height as i32,
        );
        let magnifier_grid = MagnifierGrid::new_solid(15, (0, 0, 0, 255));

        Self {
            captured_screen,
            screen_bounds,
            state: SelectionState::Idle,
            cursor_pos: PhysicalPoint::new(0, 0),
            magnifier_grid,
            color_format: ColorFormat::Hex,
            active_tool: AnnotationTool::None,
            status_message: None,
        }
    }

    /// 获取当前生效的物理选区矩形。
    pub fn current_selection(&self) -> Option<PhysicalRect> {
        self.state.current_rect()
    }

    /// 从底图提取光标周围像素网格。
    fn update_magnifier_grid(&mut self, cursor: PhysicalPoint) {
        let dim = 15;
        let half = (dim / 2) as i32;
        let mut pixels = Vec::with_capacity(dim * dim * 4);

        for row in 0..dim {
            let py = cursor.y - half + row as i32;
            for col in 0..dim {
                let px = cursor.x - half + col as i32;
                if px >= 0
                    && px < self.captured_screen.width as i32
                    && py >= 0
                    && py < self.captured_screen.height as i32
                {
                    let idx = ((py as u32 * self.captured_screen.width + px as u32) * 4) as usize;
                    if idx + 3 < self.captured_screen.data.len() {
                        let b = self.captured_screen.data[idx];
                        let g = self.captured_screen.data[idx + 1];
                        let r = self.captured_screen.data[idx + 2];
                        let a = self.captured_screen.data[idx + 3];
                        pixels.push(r);
                        pixels.push(g);
                        pixels.push(b);
                        pixels.push(a);
                        continue;
                    }
                }
                pixels.push(0);
                pixels.push(0);
                pixels.push(0);
                pixels.push(255);
            }
        }

        self.magnifier_grid = MagnifierGrid {
            dimension: dim,
            pixels,
        };
    }

    /// 处理鼠标按下事件。
    pub fn handle_mouse_down(&mut self, point: PhysicalPoint) {
        self.cursor_pos = point;
        match self.state {
            SelectionState::Idle => {
                self.state = SelectionState::MarqueeDragging {
                    start: point,
                    current: point,
                };
            }
            SelectionState::Selected { rect } => {
                let mode = hit_test_drag_mode(
                    rect,
                    point,
                    false,
                    DEFAULT_EDGE_TOLERANCE,
                    DEFAULT_MINIMUM_SELECTION_SIZE,
                );
                if mode != SelectionDragMode::None {
                    self.state = SelectionState::Reshaping {
                        mode,
                        origin_rect: rect,
                        origin_pos: point,
                        current_pos: point,
                    };
                } else {
                    // 点击选区外，重新开始框选
                    self.state = SelectionState::MarqueeDragging {
                        start: point,
                        current: point,
                    };
                }
            }
            _ => {}
        }
    }

    /// 处理鼠标移动事件。
    pub fn handle_mouse_move(&mut self, point: PhysicalPoint) {
        self.cursor_pos = point;
        self.update_magnifier_grid(point);

        match self.state {
            SelectionState::MarqueeDragging { start, .. } => {
                self.state = SelectionState::MarqueeDragging {
                    start,
                    current: point,
                };
            }
            SelectionState::Reshaping {
                mode,
                origin_rect,
                origin_pos,
                ..
            } => {
                self.state = SelectionState::Reshaping {
                    mode,
                    origin_rect,
                    origin_pos,
                    current_pos: point,
                };
            }
            _ => {}
        }
    }

    /// 处理鼠标释放事件。
    pub fn handle_mouse_up(&mut self, point: PhysicalPoint) {
        self.cursor_pos = point;
        match self.state {
            SelectionState::MarqueeDragging { start, current } => {
                let r = marquee_selection_rect(start, current);
                if r.width >= DEFAULT_MINIMUM_SELECTION_SIZE
                    && r.height >= DEFAULT_MINIMUM_SELECTION_SIZE
                {
                    self.state = SelectionState::Selected { rect: r };
                } else {
                    self.state = SelectionState::Idle;
                }
            }
            SelectionState::Reshaping {
                mode,
                origin_rect,
                origin_pos,
                current_pos,
            } => {
                let r = dragged_selection_rect(
                    mode,
                    origin_rect,
                    origin_pos,
                    current_pos,
                    Some(self.screen_bounds),
                    DEFAULT_MINIMUM_SELECTION_SIZE,
                    None,
                );
                self.state = SelectionState::Selected { rect: r };
            }
            _ => {}
        }
    }

    /// 执行动作分发。
    pub fn trigger_action(&mut self, action: ToolbarAction, window: &mut Window) {
        match action {
            ToolbarAction::Copy => {
                let crop_opt = self.current_selection().and_then(|rect| {
                    self.captured_screen.crop(
                        rect.x,
                        rect.y,
                        rect.width as u32,
                        rect.height as u32,
                    )
                });
                if let Some(sub) = crop_opt {
                    let rgba = sub.to_rgba();
                    let _ = copy_image_to_clipboard(sub.width, sub.height, &rgba);
                }
                window.remove_window();
            }
            ToolbarAction::Cancel => {
                window.remove_window();
            }
            ToolbarAction::Save => {
                // 保存文件操作（占位提示）
                self.status_message = Some("已触发保存".into());
            }
            ToolbarAction::Pin => {
                self.status_message = Some("贴图模式已就绪".into());
            }
            ToolbarAction::Ocr => {
                self.status_message = Some("OCR 识别请求已排队".into());
            }
            ToolbarAction::Translate => {
                self.status_message = Some("翻译请求已排队".into());
            }
            ToolbarAction::Undo => {
                self.status_message = Some("撤销".into());
            }
            ToolbarAction::Redo => {
                self.status_message = Some("重做".into());
            }
        }
    }

    /// 复制当前光标下色彩值到剪贴板。
    pub fn copy_current_color(&mut self) {
        let (r, g, b, _) = self.magnifier_grid.center_pixel();
        let color_str = self.color_format.format_color(r, g, b);
        let _ = copy_text_to_clipboard(&color_str);
        self.status_message = Some(format!("已复制色彩: {}", color_str));
    }
}

impl Render for ScreenshotOverlayView {
    /// 渲染覆盖窗视图。
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        let sel = self.current_selection();
        let screen_w = self.screen_bounds.width as f32;
        let screen_h = self.screen_bounds.height as f32;

        let mut root = div()
            .relative()
            .w_full()
            .h_full()
            .bg(rgba(0x00000000))
            .overflow_hidden();

        // 选区外四周半透明暗化蒙版（4 个矩形拼接）
        if let Some(s) = sel {
            let sx = s.x as f32;
            let sy = s.y as f32;
            let sw = s.width as f32;
            let sh = s.height as f32;
            let mask_color = rgba(0x00000066);

            // 顶侧蒙版
            if sy > 0.0 {
                root = root.child(
                    div()
                        .absolute()
                        .top(px(0.0))
                        .left(px(0.0))
                        .w(px(screen_w))
                        .h(px(sy))
                        .bg(mask_color),
                );
            }
            // 底侧蒙版
            if sy + sh < screen_h {
                root = root.child(
                    div()
                        .absolute()
                        .top(px(sy + sh))
                        .left(px(0.0))
                        .w(px(screen_w))
                        .h(px(screen_h - (sy + sh)))
                        .bg(mask_color),
                );
            }
            // 左侧蒙版
            if sx > 0.0 {
                root = root.child(
                    div()
                        .absolute()
                        .top(px(sy))
                        .left(px(0.0))
                        .w(px(sx))
                        .h(px(sh))
                        .bg(mask_color),
                );
            }
            // 右侧蒙版
            if sx + sw < screen_w {
                root = root.child(
                    div()
                        .absolute()
                        .top(px(sy))
                        .left(px(sx + sw))
                        .w(px(screen_w - (sx + sw)))
                        .h(px(sh))
                        .bg(mask_color),
                );
            }

            // 选区高亮外边框
            root = root.child(
                div()
                    .absolute()
                    .top(px(sy))
                    .left(px(sx))
                    .w(px(sw))
                    .h(px(sh))
                    .border_2()
                    .border_color(rgb(0x1677FF)),
            );

            // 8 个调整手柄
            let handles = handle_rects(s, DEFAULT_HANDLE_SIZE);
            for (_, hr) in handles {
                root = root.child(
                    div()
                        .absolute()
                        .top(px(hr.y as f32))
                        .left(px(hr.x as f32))
                        .w(px(hr.width as f32))
                        .h(px(hr.height as f32))
                        .bg(rgba(0xFFFFFFFF))
                        .border_1()
                        .border_color(rgb(0x1677FF)),
                );
            }

            // 选区尺寸标签（左上方浮动提示）
            let label_text = selection_size_label(s);
            root = root.child(
                div()
                    .absolute()
                    .top(px((sy - 22.0).max(4.0)))
                    .left(px(sx.max(4.0)))
                    .px_2()
                    .py_0p5()
                    .rounded_xs()
                    .bg(rgba(0x000000CC))
                    .text_color(rgba(0xFFFFFFFF))
                    .text_xs()
                    .child(label_text),
            );

            // 浮动工具栏（选区确认后展示）
            if matches!(self.state, SelectionState::Selected { .. }) {
                let tb_size = PhysicalPoint::new(380, 36);
                let tb_pos = calculate_toolbar_placement(s, tb_size, self.screen_bounds, 8);
                let tb = ScreenshotToolbar::new("overlay-toolbar")
                    .active_tool(self.active_tool)
                    .undo_redo_state(false, false);

                root = root.child(
                    div()
                        .absolute()
                        .top(px(tb_pos.y as f32))
                        .left(px(tb_pos.x as f32))
                        .child(tb),
                );
            }
        } else {
            // 未选区时全屏蒙版
            root = root.child(
                div()
                    .absolute()
                    .top(px(0.0))
                    .left(px(0.0))
                    .w(px(screen_w))
                    .h(px(screen_h))
                    .bg(rgba(0x00000040)),
            );

            // 放大镜（未确立选区时跟随光标）
            let mag_size = PhysicalPoint::new(140, 160);
            let mag_pos = calculate_magnifier_placement(
                self.cursor_pos,
                mag_size,
                self.screen_bounds,
                16,
            );

            let mag = Magnifier::new("cursor-mag", self.magnifier_grid.clone(), self.cursor_pos)
                .color_format(self.color_format);

            root = root.child(
                div()
                    .absolute()
                    .top(px(mag_pos.y as f32))
                    .left(px(mag_pos.x as f32))
                    .child(mag),
            );
        }

        // 底部快捷键提示条
        root = root.child(
            div()
                .absolute()
                .bottom(px(12.0))
                .left(px(16.0))
                .px_3()
                .py_1()
                .rounded_md()
                .bg(rgba(0x000000B3))
                .text_color(rgba(0xFFFFFFB3))
                .text_xs()
                .child("拖拽鼠标框选 · 双击/Enter 复制 · ESC 取消 · C 复制色彩"),
        );

        root
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 验证覆盖窗视图生命周期与选区状态流转。
    #[test]
    fn overlay_view_state_flow() {
        let screen = CapturedScreen::new_solid(1920, 1080, (0, 0, 0, 255));
        let mut view = ScreenshotOverlayView::new(screen);

        assert_eq!(view.current_selection(), None);

        // 模拟鼠标按下
        view.handle_mouse_down(PhysicalPoint::new(100, 100));
        assert!(matches!(view.state, SelectionState::MarqueeDragging { .. }));

        // 模拟鼠标移动
        view.handle_mouse_move(PhysicalPoint::new(400, 300));
        assert_eq!(
            view.current_selection(),
            Some(PhysicalRect::new(100, 100, 301, 201))
        );

        // 模拟鼠标释放
        view.handle_mouse_up(PhysicalPoint::new(400, 300));
        assert!(matches!(view.state, SelectionState::Selected { .. }));
        assert_eq!(
            view.current_selection(),
            Some(PhysicalRect::new(100, 100, 301, 201))
        );
    }

    /// 验证色彩复制操作。
    #[test]
    fn overlay_color_copy() {
        let screen = CapturedScreen::new_solid(100, 100, (255, 0, 0, 255));
        let mut view = ScreenshotOverlayView::new(screen);
        view.handle_mouse_move(PhysicalPoint::new(50, 50));
        view.copy_current_color();
        assert!(view.status_message.is_some());
    }
}
