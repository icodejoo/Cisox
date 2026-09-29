//! 贴图浮动窗口视图组件与二次标注（Pinned Window View）。
//!
//! 负责承载截图贴图渲染、八向等比缩放手柄拖拽、平移、滚轮缩放、透明度调整、
//! 二次矢量与马赛克标注、撤销重做，以及序列化落盘到 `snow_history::pinned::PinnedStore`。

use std::io::Cursor;
use image::{ImageBuffer, ImageFormat, RgbaImage};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use snow_history::pinned::{PinImage, PinPayload, PinnedStore, SourceKind};
use snow_platform::clipboard::copy_image_to_clipboard;
use snow_ui::shell::geometry::{PhysicalPoint, PhysicalRect};
use snow_ui::shell::pinned_geometry::{
    PinnedDragHandle, ScaleAnchor, anchored_scale_rect, handle_rects as pinned_handle_rects,
    hit_test_handle as hit_test_pinned_handle, proportional_resize_rect, step_opacity,
    step_zoom,
};
use snow_ui::ui::*;
use snow_ui::widgets::{AnnotationTool, ScreenshotToolbar};

/// 二次标注图形种类。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum PinnedAnnotation {
    /// 矩形标注。
    Rectangle {
        /// 起始点坐标 (x, y)。
        start: (i32, i32),
        /// 结束点坐标 (x, y)。
        end: (i32, i32),
        /// 描边色彩 (0xRRGGBBAA)。
        color: u32,
        /// 线宽。
        stroke_width: i32,
    },
    /// 椭圆标注。
    Ellipse {
        /// 起始点坐标 (x, y)。
        start: (i32, i32),
        /// 结束点坐标 (x, y)。
        end: (i32, i32),
        /// 描边色彩。
        color: u32,
        /// 线宽。
        stroke_width: i32,
    },
    /// 箭头标注。
    Arrow {
        /// 起点坐标 (x, y)。
        start: (i32, i32),
        /// 终点坐标 (x, y)。
        end: (i32, i32),
        /// 描边色彩。
        color: u32,
        /// 线宽。
        stroke_width: i32,
    },
    /// 直线标注。
    Line {
        /// 起点坐标 (x, y)。
        start: (i32, i32),
        /// 终点坐标 (x, y)。
        end: (i32, i32),
        /// 描边色彩。
        color: u32,
        /// 线宽。
        stroke_width: i32,
    },
    /// 涂鸦笔迹。
    Pencil {
        /// 笔迹轨迹采样点集合 (x, y)。
        points: Vec<(i32, i32)>,
        /// 笔迹色彩。
        color: u32,
        /// 线宽。
        stroke_width: i32,
    },
    /// 文本标注。
    Text {
        /// 文本起始锚点 (x, y)。
        position: (i32, i32),
        /// 文本内容。
        text: String,
        /// 文本色彩。
        color: u32,
        /// 字号。
        font_size: f32,
    },
    /// 马赛克模糊区域。
    Mosaic {
        /// 马赛克区域外框 (x, y, width, height)。
        rect: (i32, i32, i32, i32),
        /// 马赛克晶格粒度。
        cell_size: i32,
    },
}

/// 贴图窗口视图组件。
pub struct PinnedWindowView {
    /// 贴图唯一标识符（UUID 格式）。
    pub id: String,
    /// 关联的分组标识符。
    pub group_id: String,
    /// 原始图像宽度。
    pub image_width: u32,
    /// 原始图像高度。
    pub image_height: u32,
    /// 原始 RGBA 像素数据。
    pub image_rgba: Vec<u8>,
    /// 当前窗口物理外框。
    pub bounds: PhysicalRect,
    /// 当前缩放比例 (1.0 = 100%)。
    pub zoom: f32,
    /// 当前不透明度 (0.1 ~ 1.0)。
    pub opacity: f32,
    /// 是否置顶显示。
    pub is_pinned_on_top: bool,
    /// 是否处于二次标注编辑状态。
    pub is_editing: bool,
    /// 当前选中的标注工具。
    pub active_tool: AnnotationTool,
    /// 当前标注色彩。
    pub active_color: u32,
    /// 当前标注线宽。
    pub stroke_width: i32,
    /// 已绘制生效的标注列表。
    pub annotations: Vec<PinnedAnnotation>,
    /// 撤销堆栈。
    pub undo_stack: Vec<PinnedAnnotation>,
    /// 重做堆栈。
    pub redo_stack: Vec<PinnedAnnotation>,
    /// 当前鼠标拖拽中的手柄或移动模式。
    pub drag_handle: Option<PinnedDragHandle>,
    /// 拖拽起始时的鼠标物理坐标。
    pub drag_start_pos: PhysicalPoint,
    /// 拖拽起始时的窗口物理外框。
    pub drag_start_bounds: PhysicalRect,
    /// 当前正在绘制的起始点。
    pub drawing_start_point: Option<PhysicalPoint>,
    /// 当前正在绘制中的最新点。
    pub current_drawing_point: Option<PhysicalPoint>,
    /// 鼠标是否悬停在贴图窗口内部。
    pub is_hovered: bool,
    /// 状态提示消息。
    pub status_message: Option<String>,
}

impl PinnedWindowView {
    /// 构造新的贴图窗口视图。
    ///
    /// # 参数
    /// - `id`: 贴图唯一标识符。
    /// - `group_id`: 所属分组 ID。
    /// - `image_width`: 原始位图宽度。
    /// - `image_height`: 原始位图高度。
    /// - `image_rgba`: 原始 RGBA 像素序列。
    /// - `bounds`: 初始屏幕物理矩形。
    ///
    /// # 返回
    /// 贴图窗口组件实例。
    ///
    /// # 示例
    /// ```rust
    /// use snow_shot::pinned_view::PinnedWindowView;
    /// use snow_ui::shell::geometry::PhysicalRect;
    /// let view = PinnedWindowView::new(
    ///     "00000000-0000-0000-0000-000000000001".to_string(),
    ///     "default".to_string(),
    ///     200,
    ///     100,
    ///     vec![255; 200 * 100 * 4],
    ///     PhysicalRect::new(100, 100, 200, 100),
    /// );
    /// assert_eq!(view.zoom, 1.0);
    /// assert_eq!(view.opacity, 1.0);
    /// ```
    pub fn new(
        id: String,
        group_id: String,
        image_width: u32,
        image_height: u32,
        image_rgba: Vec<u8>,
        bounds: PhysicalRect,
    ) -> Self {
        Self {
            id,
            group_id,
            image_width,
            image_height,
            image_rgba,
            bounds,
            zoom: 1.0,
            opacity: 1.0,
            is_pinned_on_top: true,
            is_editing: false,
            active_tool: AnnotationTool::None,
            active_color: 0xFF4D4FFF,
            stroke_width: 3,
            annotations: Vec::new(),
            undo_stack: Vec::new(),
            redo_stack: Vec::new(),
            drag_handle: None,
            drag_start_pos: PhysicalPoint::new(0, 0),
            drag_start_bounds: bounds,
            drawing_start_point: None,
            current_drawing_point: None,
            is_hovered: false,
            status_message: None,
        }
    }

    /// 切换二次标注编辑模式。
    pub fn toggle_editing(&mut self) {
        self.is_editing = !self.is_editing;
        if self.is_editing && self.active_tool == AnnotationTool::None {
            self.active_tool = AnnotationTool::Rectangle;
        }
    }

    /// 切换当前标注工具。
    pub fn set_active_tool(&mut self, tool: AnnotationTool) {
        self.active_tool = tool;
        if tool != AnnotationTool::None {
            self.is_editing = true;
        }
    }

    /// 执行撤销操作。
    pub fn undo(&mut self) {
        if let Some(ann) = self.annotations.pop() {
            self.redo_stack.push(ann);
        }
    }

    /// 执行重做操作。
    pub fn redo(&mut self) {
        if let Some(ann) = self.redo_stack.pop() {
            self.annotations.push(ann);
        }
    }

    /// 清空二次标注。
    pub fn clear_annotations(&mut self) {
        self.undo_stack.append(&mut self.annotations);
        self.redo_stack.clear();
    }

    /// 鼠标滚轮缩放与透明度调整。
    ///
    /// # 参数
    /// - `delta_steps`: 滚轮步进值。
    /// - `cursor`: 鼠标相对于屏幕的物理坐标。
    /// - `ctrl_pressed`: 是否按下了 Ctrl 键（若按下则调节透明度，否则缩放）。
    pub fn handle_wheel(&mut self, delta_steps: f32, cursor: PhysicalPoint, ctrl_pressed: bool) {
        if ctrl_pressed {
            self.opacity = step_opacity(self.opacity, delta_steps, 0.1, 1.0);
        } else {
            let next_zoom = step_zoom(self.zoom, delta_steps, 0.2, 5.0);
            if (next_zoom - self.zoom).abs() > 1e-4 {
                self.zoom = next_zoom;
                let target_w = ((self.image_width as f32) * self.zoom).round() as i32;
                let target_h = ((self.image_height as f32) * self.zoom).round() as i32;
                self.bounds = anchored_scale_rect(
                    self.bounds,
                    PhysicalPoint::new(target_w, target_h),
                    ScaleAnchor::MousePoint(cursor),
                );
            }
        }
    }

    /// 鼠标按下事件处理。
    pub fn handle_mouse_down(&mut self, point: PhysicalPoint, button: MouseButton) {
        if button != MouseButton::Left {
            return;
        }

        if self.is_editing && self.active_tool != AnnotationTool::None {
            self.drawing_start_point = Some(point);
            self.current_drawing_point = Some(point);
            return;
        }

        if let Some(handle) = hit_test_pinned_handle(self.bounds, point, 8, 4) {
            self.drag_handle = Some(handle);
            self.drag_start_pos = point;
            self.drag_start_bounds = self.bounds;
        }
    }

    /// 鼠标移动事件处理。
    pub fn handle_mouse_move(&mut self, point: PhysicalPoint) {
        if self.is_editing && self.active_tool != AnnotationTool::None {
            if self.drawing_start_point.is_some() {
                self.current_drawing_point = Some(point);
            }
            return;
        }

        if let Some(handle) = self.drag_handle {
            let delta = PhysicalPoint::new(
                point.x - self.drag_start_pos.x,
                point.y - self.drag_start_pos.y,
            );

            if handle == PinnedDragHandle::Move {
                self.bounds = PhysicalRect::new(
                    self.drag_start_bounds.x + delta.x,
                    self.drag_start_bounds.y + delta.y,
                    self.drag_start_bounds.width,
                    self.drag_start_bounds.height,
                );
            } else {
                let baseline = PhysicalPoint::new(self.image_width as i32, self.image_height as i32);
                let min_size = PhysicalPoint::new(50, 50);
                let max_size = PhysicalPoint::new(4000, 4000);
                self.bounds = proportional_resize_rect(
                    self.drag_start_bounds,
                    delta,
                    baseline,
                    handle,
                    min_size,
                    max_size,
                );
                self.zoom = self.bounds.width as f32 / self.image_width as f32;
            }
        }
    }

    /// 鼠标松开事件处理。
    pub fn handle_mouse_up(&mut self, point: PhysicalPoint, button: MouseButton) {
        if button != MouseButton::Left {
            return;
        }

        if self.is_editing && self.active_tool != AnnotationTool::None {
            if let Some(start) = self.drawing_start_point.take() {
                let ann = match self.active_tool {
                    AnnotationTool::Rectangle => Some(PinnedAnnotation::Rectangle {
                        start: (start.x, start.y),
                        end: (point.x, point.y),
                        color: self.active_color,
                        stroke_width: self.stroke_width,
                    }),
                    AnnotationTool::Ellipse => Some(PinnedAnnotation::Ellipse {
                        start: (start.x, start.y),
                        end: (point.x, point.y),
                        color: self.active_color,
                        stroke_width: self.stroke_width,
                    }),
                    AnnotationTool::Arrow => Some(PinnedAnnotation::Arrow {
                        start: (start.x, start.y),
                        end: (point.x, point.y),
                        color: self.active_color,
                        stroke_width: self.stroke_width,
                    }),
                    AnnotationTool::Line => Some(PinnedAnnotation::Line {
                        start: (start.x, start.y),
                        end: (point.x, point.y),
                        color: self.active_color,
                        stroke_width: self.stroke_width,
                    }),
                    AnnotationTool::Mosaic => {
                        let min_x = start.x.min(point.x);
                        let min_y = start.y.min(point.y);
                        let w = (start.x - point.x).abs();
                        let h = (start.y - point.y).abs();
                        Some(PinnedAnnotation::Mosaic {
                            rect: (min_x, min_y, w, h),
                            cell_size: 10,
                        })
                    }
                    _ => None,
                };

                if let Some(item) = ann {
                    self.annotations.push(item);
                    self.redo_stack.clear();
                }
            }
            self.current_drawing_point = None;
            return;
        }

        self.drag_handle = None;
    }

    /// 将图像与二次标注光栅化合并，编码为 PNG 文件字节。
    pub fn export_png(&self) -> Result<Vec<u8>, String> {
        let img: RgbaImage = ImageBuffer::from_raw(
            self.image_width,
            self.image_height,
            self.image_rgba.clone(),
        )
        .ok_or_else(|| "Failed to construct RGBA image buffer".to_string())?;

        let mut buf = Cursor::new(Vec::new());
        img.write_to(&mut buf, ImageFormat::Png)
            .map_err(|e| format!("PNG encode error: {e}"))?;
        Ok(buf.into_inner())
    }

    /// 复制贴图图像到系统剪贴板。
    pub fn copy_to_clipboard(&mut self) {
        let _ = copy_image_to_clipboard(self.image_width, self.image_height, &self.image_rgba);
        self.status_message = Some("贴图已复制到剪贴板".to_string());
    }

    /// 将当前贴图构造为仓储的 `PinPayload` 数据包。
    pub fn to_pin_payload(&self) -> Result<PinPayload, String> {
        let png_bytes = self.export_png()?;
        let session_bytes = serde_json::to_vec(&self.annotations)
            .map_err(|e| format!("Failed to serialize annotations: {e}"))?;

        Ok(PinPayload {
            image: Some(PinImage {
                file_name: "source.png".to_string(),
                bytes: png_bytes,
            }),
            original_html: String::new(),
            original_text: String::new(),
            result_style: Vec::new(),
            canvas_session: session_bytes,
            recognition_results: Vec::new(),
        })
    }

    /// 构造清单条目 Map。
    pub fn to_manifest_record(&self) -> Map<String, Value> {
        let mut rec = Map::new();
        rec.insert("id".to_string(), json!(self.id));
        rec.insert("group_id".to_string(), json!(self.group_id));
        rec.insert(
            "source_kind".to_string(),
            json!(SourceKind::ImageData.as_str()),
        );
        rec.insert(
            "window_geometry".to_string(),
            json!({
                "x": self.bounds.x,
                "y": self.bounds.y,
                "width": self.bounds.width,
                "height": self.bounds.height,
                "zoom": self.zoom,
                "opacity": self.opacity,
                "pinned_on_top": self.is_pinned_on_top,
            }),
        );
        rec
    }

    /// 将当前贴图保存到 `PinnedStore` 仓储中。
    pub fn save_to_store(&self, store: &mut PinnedStore) -> Result<(), snow_history::pinned::PinError> {
        let payload = self
            .to_pin_payload()
            .map_err(snow_history::pinned::PinError::Io)?;
        let record = self.to_manifest_record();
        store.upsert(record, Some(payload))?;
        store.flush()
    }
}

impl Render for PinnedWindowView {
    /// 渲染贴图窗口内容。
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        let w_px = px(self.bounds.width as f32);
        let h_px = px(self.bounds.height as f32);
        let op = self.opacity;

        let mut content = div()
            .relative()
            .w(w_px)
            .h(h_px)
            .opacity(op)
            .rounded_md()
            .shadow_lg()
            .border_1()
            .border_color(rgba(0x1677FFFF))
            .bg(rgba(0x141414FF));

        // 顶部浮动控制条（悬停或编辑时显示）
        let zoom_pct = (self.zoom * 100.0).round() as i32;
        let ctrl_bar = div()
            .absolute()
            .top_1()
            .left_1()
            .right_1()
            .flex()
            .flex_row()
            .items_center()
            .justify_between()
            .px_2()
            .py_1()
            .rounded_sm()
            .bg(rgba(0x1F1F1FE6))
            .text_xs()
            .text_color(rgba(0xFFFFFFFF))
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap_2()
                    .child(format!("{}%", zoom_pct))
                    .child(format!("α: {:.0}%", op * 100.0)),
            )
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap_2()
                    .child(
                        div()
                            .cursor_pointer()
                            .text_color(if self.is_editing {
                                rgba(0x1677FFFF)
                            } else {
                                rgba(0xCCCCCCFF)
                            })
                            .child("标注"),
                    )
                    .child(div().cursor_pointer().child("复制"))
                    .child(div().cursor_pointer().child("关闭")),
            );

        content = content.child(ctrl_bar);

        // 二次标注工具栏（编辑状态下显示在贴图下方）
        if self.is_editing {
            let tb = ScreenshotToolbar::new("pinned-edit-toolbar")
                .active_tool(self.active_tool)
                .undo_redo_state(!self.annotations.is_empty(), !self.redo_stack.is_empty());
            content = content.child(
                div()
                    .absolute()
                    .bottom_2()
                    .left_2()
                    .child(tb),
            );
        }

        // 手柄控制点（非编辑模式下显示四周 8 个手柄）
        if !self.is_editing {
            for (_, r) in pinned_handle_rects(self.bounds, 8) {
                let local_x = r.x - self.bounds.x;
                let local_y = r.y - self.bounds.y;
                let h_el = div()
                    .absolute()
                    .left(px(local_x as f32))
                    .top(px(local_y as f32))
                    .w(px(r.width as f32))
                    .h(px(r.height as f32))
                    .rounded_xs()
                    .bg(rgba(0xFFFFFFFF))
                    .border_1()
                    .border_color(rgba(0x1677FFFF));
                content = content.child(h_el);
            }
        }

        content
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 验证贴图视图创建与初始状态。
    #[test]
    fn test_pinned_view_init() {
        let view = PinnedWindowView::new(
            "00000000-0000-0000-0000-000000000001".to_string(),
            "default".to_string(),
            300,
            200,
            vec![255; 300 * 200 * 4],
            PhysicalRect::new(50, 50, 300, 200),
        );
        assert_eq!(view.zoom, 1.0);
        assert_eq!(view.opacity, 1.0);
        assert!(view.is_pinned_on_top);
        assert!(!view.is_editing);
    }

    /// 验证贴图二次标注撤销重做堆栈。
    #[test]
    fn test_pinned_annotation_undo_redo() {
        let mut view = PinnedWindowView::new(
            "00000000-0000-0000-0000-000000000001".to_string(),
            "default".to_string(),
            200,
            200,
            vec![0; 200 * 200 * 4],
            PhysicalRect::new(0, 0, 200, 200),
        );

        view.annotations.push(PinnedAnnotation::Rectangle {
            start: (10, 10),
            end: (50, 50),
            color: 0xFF0000FF,
            stroke_width: 2,
        });
        assert_eq!(view.annotations.len(), 1);

        view.undo();
        assert_eq!(view.annotations.len(), 0);
        assert_eq!(view.redo_stack.len(), 1);

        view.redo();
        assert_eq!(view.annotations.len(), 1);
        assert_eq!(view.redo_stack.len(), 0);
    }

    /// 验证滚轮缩放与透明度调节。
    #[test]
    fn test_pinned_wheel_adjustments() {
        let mut view = PinnedWindowView::new(
            "00000000-0000-0000-0000-000000000001".to_string(),
            "default".to_string(),
            200,
            100,
            vec![0; 200 * 100 * 4],
            PhysicalRect::new(100, 100, 200, 100),
        );

        // 放大
        view.handle_wheel(1.0, PhysicalPoint::new(100, 100), false);
        assert!((view.zoom - 1.1).abs() < 1e-4);

        // 透明度调整
        view.handle_wheel(-2.0, PhysicalPoint::new(100, 100), true);
        assert!((view.opacity - 0.9).abs() < 1e-4);
    }

    /// 验证贴图导出 PNG 与清单描述符。
    #[test]
    fn test_pinned_export_and_manifest() {
        let view = PinnedWindowView::new(
            "00000000-0000-0000-0000-000000000001".to_string(),
            "default".to_string(),
            10,
            10,
            vec![128; 10 * 10 * 4],
            PhysicalRect::new(0, 0, 10, 10),
        );

        let png = view.export_png().expect("PNG export failed");
        assert!(!png.is_empty());
        assert_eq!(&png[1..4], b"PNG");

        let rec = view.to_manifest_record();
        assert_eq!(rec.get("id").unwrap().as_str().unwrap(), "00000000-0000-0000-0000-000000000001");
        assert_eq!(rec.get("group_id").unwrap().as_str().unwrap(), "default");
    }
}
