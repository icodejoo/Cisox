//! 贴图窗口视图：无边框置顶窗口里渲染图像，支持拖动、八向缩放、滚轮缩放 / 透明度、
//! 右键菜单、二次标注（复用覆盖窗的标注引擎）与持久化。
//!
//! 设计要点：
//! - 图像只在创建时转成一份 GPU 图像资源（`FrozenFrame`），渲染阶段只复用句柄，不逐帧拷贝 / 转换；
//! - 标注沿用覆盖窗的脏块策略（`AnnotationLayer` + 分块图像资源），窗口空闲时不产生任何重绘；
//! - 窗口移动 / 缩放用光标的屏幕坐标计算（不受窗口自身移动影响），并合并成每轮事件循环一次
//!   `SetWindowPos`（必须在 GPUI 借用之外调用，见 [`PinnedWindowView::schedule_rect_apply`]）；
//! - 落盘经 `PinShared`（`PinnedStore` 崩溃安全提交），几何变化防抖后写清单，标注变化防抖后重写源图。

use crate::annotation::{AnnotationLayer, LayerUpdate};
use crate::frozen_frame::FrozenFrame;
use crate::overlay_view::TileSprite;
use crate::pinned_model::{
    EDGE_MARGIN, HANDLE_SIZE, PinClickAction, PinGeometry, apply_wheel, drag_rect,
    premultiply_alpha_in_place, swap_rb_in_place, wheel_anchor, wheel_steps,
};
use crate::pinned_shared::{PinInteraction, PinShared};
use crate::screenshot_output::encode_png;
use serde::Deserialize;
use snow_canvas_raster::TileKey;
use snow_canvas_text::{CanvasTextInput, CanvasTextStyle, EditKeyOutcome};
use snow_platform::capture::CapturedScreen;
use snow_platform::clipboard::copy_image_to_clipboard;
use snow_platform::menu::{MenuEntry, MenuItem, show_popup_menu};
use snow_platform::text_raster::DEFAULT_FONT_FAMILY;
use snow_ui::shell::geometry::{PhysicalPoint, PhysicalRect};
use snow_ui::shell::overlay::cursor_screen_position;
use snow_ui::shell::pinned_geometry::{PinnedDragHandle, handle_rects, hit_test_handle};
use snow_ui::ui::*;
use snow_ui::widgets::AnnotationTool;
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

/// 几何 / 标注变化后延迟多久写盘（防抖，避免拖动或连续滚轮时频繁落盘）。
const PERSIST_DEBOUNCE: Duration = Duration::from_millis(400);
/// 状态提示的显示时长。
const STATUS_DURATION: Duration = Duration::from_secs(3);
/// 像素式滚动（触控板）折算成“行”的像素数。
const PIXELS_PER_LINE: f32 = 20.0;
/// 文字输入框行高倍率。
const TEXT_LINE_HEIGHT: f32 = 1.25;
/// 提示条背景色。
const BADGE_BG: u32 = 0x000000B3;
/// 提示条文字色。
const BADGE_TEXT: u32 = 0xFFFFFFE6;
/// 手柄填充色。
const HANDLE_FILL: u32 = 0xFFFFFFFF;
/// 二次标注提示。
const EDITING_HINT: &str = "二次标注 · 右键选工具 · Ctrl+Z/Y 撤销重做 · Esc 结束";

/// 菜单项：复制。
const MENU_COPY: u32 = 1;
/// 菜单项：保存为文件。
const MENU_SAVE: u32 = 2;
/// 菜单项：切换置顶。
const MENU_TOPMOST: u32 = 3;
/// 菜单项：开始 / 结束二次标注。
const MENU_ANNOTATE: u32 = 4;
/// 菜单项：还原 100% 缩放。
const MENU_RESET_ZOOM: u32 = 5;
/// 菜单项：关闭。
const MENU_CLOSE: u32 = 6;
/// 菜单项：撤销。
const MENU_UNDO: u32 = 7;
/// 菜单项：重做。
const MENU_REDO: u32 = 8;
/// 工具菜单项 ID 起点（`TOOL_BASE + 工具序号`）。
const MENU_TOOL_BASE: u32 = 100;

/// 菜单里可选的标注工具（顺序即菜单顺序）。
const MENU_TOOLS: [(AnnotationTool, &str); 8] = [
    (AnnotationTool::Rectangle, "矩形"),
    (AnnotationTool::Ellipse, "椭圆"),
    (AnnotationTool::Arrow, "箭头"),
    (AnnotationTool::Line, "直线"),
    (AnnotationTool::Pencil, "画笔"),
    (AnnotationTool::Text, "文字"),
    (AnnotationTool::Mosaic, "马赛克"),
    (AnnotationTool::Blur, "模糊"),
];

/// 创建贴图视图所需的参数。
pub struct PinInit {
    /// 贴图 ID。
    pub id: String,
    /// 底图（BGRA，不透明）。
    pub frame: FrozenFrame,
    /// 初始窗口几何与显示状态。
    pub geometry: PinGeometry,
    /// 创建时间（UTC 毫秒）。
    pub created_ms: i64,
    /// 已落盘 payload 的体积（字节）。
    pub payload_bytes: u64,
    /// 二次标注的引擎会话字节（从仓储恢复时携带）；没有标注为空。
    pub session: Vec<u8>,
    /// 恢复标注会话时使用的设备像素比。
    pub dpr: f32,
}

/// 由不透明 RGBA 像素构造底图（一次性 R/B 交换后移动进图像资源）。
///
/// # 参数
/// - `width` / `height`：图像尺寸。
/// - `rgba`：RGBA 像素（长度须为 `宽 * 高 * 4`）；带透明区（自定义选区的贴图）时按预乘 alpha 上传。
///
/// # 返回
/// 底图；尺寸为 0 或长度不符返回错误说明。
///
/// ```ignore
/// let frame = frame_from_rgba(2, 1, vec![255, 0, 0, 255, 0, 255, 0, 255])?;
/// assert_eq!(frame.size(), (2, 1));
/// ```
pub fn frame_from_rgba(width: u32, height: u32, mut rgba: Vec<u8>) -> Result<FrozenFrame, String> {
    premultiply_alpha_in_place(&mut rgba);
    swap_rb_in_place(&mut rgba);
    FrozenFrame::from_captured(CapturedScreen {
        width,
        height,
        data: rgba,
    })
}

/// 拖动状态：正在移动或缩放窗口。
#[derive(Debug, Clone, Copy)]
struct DragState {
    /// 拖动的手柄（`Move` 表示整体平移）。
    handle: PinnedDragHandle,
    /// 拖动开始时的光标屏幕坐标。
    start_cursor: PhysicalPoint,
    /// 拖动开始时的窗口外框。
    start_bounds: PhysicalRect,
}

/// 进行中的文字输入。
struct TextEditSession {
    /// 输入实体（负责 IME 与光标）。
    input: Entity<CanvasTextInput>,
    /// 文字外框左上角（图像像素坐标）。
    origin: (f64, f64),
}

/// 自动化验收脚本里的一步操作（走与鼠标 / 菜单相同的内部入口，不是系统输入模拟）。
#[derive(Debug, Clone, Deserialize, PartialEq)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum PinOp {
    /// 滚轮：`steps` 为步进次数，`ctrl` 为真时调透明度。
    Wheel {
        /// 步进次数（正为放大 / 更不透明）。
        steps: f32,
        /// 是否按住 Ctrl。
        #[serde(default)]
        ctrl: bool,
    },
    /// 手柄拖动：`handle` 为手柄名（如 `move` / `bottom_right`），`dx` / `dy` 为光标位移。
    Drag {
        /// 手柄名。
        handle: String,
        /// 水平位移（物理像素）。
        dx: i32,
        /// 垂直位移（物理像素）。
        dy: i32,
    },
    /// 进入或退出二次标注。
    Edit {
        /// 是否进入。
        on: bool,
    },
    /// 用指定工具在图像坐标里拖出一个图形。
    Annotate {
        /// 工具名（rectangle / ellipse / arrow / line / pencil / mosaic / blur）。
        tool: String,
        /// 起点 x（图像像素）。
        x0: f64,
        /// 起点 y。
        y0: f64,
        /// 终点 x。
        x1: f64,
        /// 终点 y。
        y1: f64,
    },
    /// 撤销一步标注。
    Undo,
    /// 重做一步标注。
    Redo,
    /// 设置是否置顶。
    Topmost {
        /// 目标状态。
        on: bool,
    },
    /// 弹出右键菜单（阻塞到菜单关闭）。
    Menu,
    /// 立即写盘。
    Persist,
    /// 关闭贴图（同时从存储移除）。
    Close,
}

/// 解析手柄名（自动化脚本用）；未知名称返回 `None`。
///
/// ```ignore
/// assert_eq!(parse_handle("bottom_right"), Some(PinnedDragHandle::BottomRight));
/// ```
pub fn parse_handle(name: &str) -> Option<PinnedDragHandle> {
    Some(match name {
        "move" => PinnedDragHandle::Move,
        "top_left" => PinnedDragHandle::TopLeft,
        "top" => PinnedDragHandle::Top,
        "top_right" => PinnedDragHandle::TopRight,
        "right" => PinnedDragHandle::Right,
        "bottom_right" => PinnedDragHandle::BottomRight,
        "bottom" => PinnedDragHandle::Bottom,
        "bottom_left" => PinnedDragHandle::BottomLeft,
        "left" => PinnedDragHandle::Left,
        _ => return None,
    })
}

/// 解析标注工具名（自动化脚本用）；未知名称返回 `None`。
///
/// ```ignore
/// assert_eq!(parse_tool("arrow"), Some(AnnotationTool::Arrow));
/// ```
pub fn parse_tool(name: &str) -> Option<AnnotationTool> {
    MENU_TOOLS
        .iter()
        .zip([
            "rectangle",
            "ellipse",
            "arrow",
            "line",
            "pencil",
            "text",
            "mosaic",
            "blur",
        ])
        .find(|(_, key)| *key == name)
        .map(|((tool, _), _)| *tool)
}

/// 贴图窗口视图。
pub struct PinnedWindowView {
    /// 贴图 ID。
    id: String,
    /// 共享上下文（仓储 / 配置）。
    shared: Rc<PinShared>,
    /// 交互配置快照。
    interaction: PinInteraction,
    /// 底图（未叠加二次标注的原图）。
    frame: FrozenFrame,
    /// 当前窗口外框（屏幕物理像素）。
    bounds: PhysicalRect,
    /// 当前缩放倍率。
    zoom: f32,
    /// 当前不透明度。
    opacity: f32,
    /// 是否置顶。
    topmost: bool,
    /// 创建时间（UTC 毫秒）。
    created_ms: i64,
    /// 已落盘源图的体积（字节）。
    payload_bytes: u64,
    /// 原生窗口句柄（建窗后设置）。
    window: Option<ShellWindow>,
    /// 进行中的拖动。
    drag: Option<DragState>,
    /// 鼠标是否悬停在窗口上。
    hover: bool,
    /// 悬停命中的手柄。
    hover_handle: Option<PinnedDragHandle>,
    /// 等待应用到原生窗口的外框。
    pending_rect: Option<PhysicalRect>,
    /// 已经安排了一次外框应用任务。
    rect_apply_scheduled: bool,
    /// 已经安排了一次延迟写盘任务（同一时刻最多一个，避免拖动时堆积大量定时任务）。
    persist_armed: bool,
    /// 延迟写盘任务到期时是否仍有待写入的变化。
    persist_wanted: bool,
    /// 源图（含标注合成）有改动、尚未落盘。
    payload_dirty: bool,
    /// 是否处于二次标注模式。
    editing: bool,
    /// 当前标注工具。
    tool: AnnotationTool,
    /// 标注层（首次进入标注模式时创建）。
    layer: Option<AnnotationLayer>,
    /// 标注预览分块。
    tile_sprites: HashMap<TileKey, TileSprite>,
    /// 已被替换、等待从 GPU 图集释放的图像。
    pending_drops: Vec<Arc<RenderImage>>,
    /// 指针正在绘制标注。
    annotating: bool,
    /// 拖动中尚未提交给标注层的最新位置（图像像素）。
    pending_annotation_point: Option<(f64, f64)>,
    /// 进行中的文字输入。
    text_edit: Option<TextEditSession>,
    /// 键盘焦点句柄（离屏测试为空）。
    focus_handle: Option<FocusHandle>,
    /// 状态提示。
    status: Option<String>,
    /// 状态提示代号（过期任务据此判断是否仍有效）。
    status_gen: u64,
    /// 是否已关闭。
    closed: bool,
    /// 渲染次数（验证“空闲无重绘”用）。
    render_count: u64,
    /// 最近一次渲染时的窗口缩放比。
    scale: f32,
}

impl PinnedWindowView {
    /// 创建视图状态（不依赖 GPUI 上下文，可离屏测试）。
    ///
    /// # 参数
    /// - `shared`：共享上下文。
    /// - `init`：创建参数。
    ///
    /// ```ignore
    /// let view = PinnedWindowView::new(shared, init);
    /// assert_eq!(view.id(), "…");
    /// ```
    pub fn new(shared: Rc<PinShared>, init: PinInit) -> Self {
        let interaction = shared.interaction();
        let g = init.geometry;
        let mut view = Self {
            id: init.id,
            shared,
            interaction,
            frame: init.frame,
            bounds: g.rect(),
            zoom: g.zoom,
            opacity: g.opacity,
            topmost: g.topmost,
            created_ms: init.created_ms,
            payload_bytes: init.payload_bytes,
            window: None,
            drag: None,
            hover: false,
            hover_handle: None,
            pending_rect: None,
            rect_apply_scheduled: false,
            persist_armed: false,
            persist_wanted: false,
            payload_dirty: false,
            editing: false,
            tool: AnnotationTool::None,
            layer: None,
            tile_sprites: HashMap::new(),
            pending_drops: Vec::new(),
            annotating: false,
            pending_annotation_point: None,
            text_edit: None,
            focus_handle: None,
            status: None,
            status_gen: 0,
            closed: false,
            render_count: 0,
            scale: 1.0,
        };
        if !init.session.is_empty() {
            view.restore_session(&init.session, init.dpr);
        }
        view
    }

    /// 从持久化的引擎会话恢复二次标注（含撤销历史）；失败只记日志，底图保留。
    fn restore_session(&mut self, session: &[u8], dpr: f32) {
        let (w, h) = self.frame.size();
        let restored = AnnotationLayer::from_session(w, h, dpr, session).and_then(|mut layer| {
            let update = layer.refresh(self.frame.base_view())?;
            Ok((layer, update))
        });
        match restored {
            Ok((layer, update)) => {
                self.install_update(update);
                tracing::info!(id = %self.id, items = layer.item_count(), "已恢复二次标注");
                self.layer = Some(layer);
            }
            Err(e) => {
                tracing::error!(id = %self.id, error = %e, "二次标注会话无法恢复，标注内容丢失（底图保留）")
            }
        }
    }

    /// 在 GPUI 中创建视图实体。
    ///
    /// # 参数
    /// - `_window`：建窗回调提供的窗口（保留以便后续需要窗口上下文时扩展）。
    /// - `app`：应用上下文。
    /// - `shared` / `init`：同 [`PinnedWindowView::new`]。
    pub fn create(
        _window: &mut Window,
        app: &mut App,
        shared: Rc<PinShared>,
        init: PinInit,
    ) -> Entity<Self> {
        app.new(|cx| {
            let mut view = Self::new(shared, init);
            view.focus_handle = Some(cx.focus_handle());
            view
        })
    }

    /// 记录原生窗口句柄（建窗后由管理器调用）。
    pub fn set_window(&mut self, window: ShellWindow) {
        self.window = Some(window);
    }

    /// 贴图 ID。
    pub fn id(&self) -> &str {
        &self.id
    }

    /// 当前窗口外框。
    pub fn bounds(&self) -> PhysicalRect {
        self.bounds
    }

    /// 当前缩放倍率。
    pub fn zoom(&self) -> f32 {
        self.zoom
    }

    /// 当前不透明度。
    pub fn opacity(&self) -> f32 {
        self.opacity
    }

    /// 是否置顶。
    pub fn is_topmost(&self) -> bool {
        self.topmost
    }

    /// 是否处于二次标注模式。
    pub fn is_editing(&self) -> bool {
        self.editing
    }

    /// 是否已关闭。
    pub fn is_closed(&self) -> bool {
        self.closed
    }

    /// 渲染次数（空闲时应保持不变）。
    pub fn render_count(&self) -> u64 {
        self.render_count
    }

    /// 标注预览分块数量。
    pub fn tile_sprite_count(&self) -> usize {
        self.tile_sprites.len()
    }

    /// 原图尺寸（像素）。
    pub fn image_size(&self) -> (i32, i32) {
        let (w, h) = self.frame.size();
        (w as i32, h as i32)
    }

    /// 当前窗口几何与显示状态。
    pub fn geometry(&self) -> PinGeometry {
        PinGeometry::new(self.bounds, self.zoom, self.opacity, self.topmost)
    }

    /// 1 个图像像素对应的窗口逻辑像素数（缩放 / DPI 换算）。
    fn logical_per_image_px(&self) -> f32 {
        let (w, _) = self.image_size();
        (self.bounds.width as f32 / w.max(1) as f32) / self.scale.max(f32::MIN_POSITIVE)
    }

    /// 窗口内逻辑坐标转图像像素坐标（钳制在图像内）。
    ///
    /// # 参数
    /// - `pos`：窗口内的逻辑坐标（`x`, `y`，单位逻辑像素）。
    pub fn image_point(&self, pos: (f32, f32)) -> (f64, f64) {
        let k = self.logical_per_image_px().max(f32::MIN_POSITIVE);
        let (w, h) = self.image_size();
        let x = (pos.0 / k).clamp(0.0, (w - 1).max(0) as f32);
        let y = (pos.1 / k).clamp(0.0, (h - 1).max(0) as f32);
        (f64::from(x), f64::from(y))
    }

    /// 设置状态提示（显示一段时间后自动消失）。
    fn set_status(&mut self, text: impl Into<String>, cx: &mut Context<Self>) {
        self.status = Some(text.into());
        self.status_gen += 1;
        let gen_now = self.status_gen;
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(STATUS_DURATION).await;
            let _ = this.update(cx, |v, cx| {
                if v.status_gen == gen_now && v.status.take().is_some() {
                    cx.notify();
                }
            });
        })
        .detach();
        cx.notify();
    }

    // ---------------------------------------------------------------- 几何与窗口

    /// 记录新外框并安排应用到原生窗口。
    fn set_bounds(&mut self, rect: PhysicalRect, cx: &mut Context<Self>) {
        self.bounds = rect;
        self.pending_rect = Some(rect);
        self.schedule_rect_apply(cx);
    }

    /// 安排一次原生窗口外框应用：合并同一轮事件里的多次修改，只调用一次 `SetWindowPos`。
    ///
    /// `SetWindowPos` 会同步触发窗口尺寸消息并回调 GPUI，必须在 App 未被借用时调用，
    /// 因此放进异步任务里，且在 `update` 之外执行。
    fn schedule_rect_apply(&mut self, cx: &mut Context<Self>) {
        if self.rect_apply_scheduled {
            return;
        }
        self.rect_apply_scheduled = true;
        cx.spawn(async move |this, cx| {
            let job = this
                .update(cx, |v, _| {
                    v.rect_apply_scheduled = false;
                    if v.closed {
                        return None;
                    }
                    v.window.zip(v.pending_rect.take())
                })
                .ok()
                .flatten();
            if let Some((window, rect)) = job
                && let Err(e) = window.set_rect(rect)
            {
                tracing::warn!(error = %e, ?rect, "贴图窗口移动 / 缩放失败");
            }
        })
        .detach();
    }

    /// 把窗口提到所属层级最上面（不抢焦点），在 App 借用之外执行。
    fn request_raise(&self, cx: &mut Context<Self>) {
        let (window, topmost) = (self.window, self.topmost);
        cx.spawn(async move |_this, _cx| {
            if let Some(window) = window
                && let Err(e) = window.raise(topmost)
            {
                tracing::warn!(error = %e, "贴图窗口置于顶层失败");
            }
        })
        .detach();
    }

    /// 几何 / 显示状态发生变化：重绘并防抖写盘。
    fn geometry_changed(&mut self, cx: &mut Context<Self>) {
        self.schedule_persist(cx);
        cx.notify();
    }

    /// 应用一次滚轮：Ctrl 调透明度，否则按配置的锚点缩放。
    ///
    /// # 参数
    /// - `steps`：步进次数（正为放大 / 更不透明）。
    /// - `ctrl`：是否按住 Ctrl。
    /// - `cursor`：光标屏幕坐标（鼠标锚点用）。
    pub fn wheel(&mut self, steps: f32, ctrl: bool, cursor: PhysicalPoint, cx: &mut Context<Self>) {
        let anchor = wheel_anchor(&self.interaction.wheel_mode, cursor);
        let out = apply_wheel(
            self.bounds,
            self.zoom,
            self.opacity,
            self.image_size(),
            steps,
            ctrl,
            anchor,
        );
        if out.rect == self.bounds && out.opacity == self.opacity {
            return;
        }
        self.zoom = out.zoom;
        self.opacity = out.opacity;
        if out.rect != self.bounds {
            self.set_bounds(out.rect, cx);
        }
        self.geometry_changed(cx);
    }

    /// 开始拖动（移动或缩放）。
    ///
    /// # 参数
    /// - `handle`：拖动的手柄。
    /// - `cursor`：光标屏幕坐标。
    pub fn begin_drag(&mut self, handle: PinnedDragHandle, cursor: PhysicalPoint) {
        self.drag = Some(DragState {
            handle,
            start_cursor: cursor,
            start_bounds: self.bounds,
        });
    }

    /// 拖动中：按光标位置更新外框与缩放。
    ///
    /// # 参数
    /// - `cursor`：光标屏幕坐标。
    pub fn drag_to(&mut self, cursor: PhysicalPoint, cx: &mut Context<Self>) {
        let Some(drag) = self.drag else {
            return;
        };
        let (rect, zoom) = drag_rect(
            drag.handle,
            drag.start_bounds,
            drag.start_cursor,
            cursor,
            self.image_size(),
        );
        if let Some(z) = zoom {
            self.zoom = z;
        }
        if rect != self.bounds {
            // 拖动中窗口尺寸变化会触发 GPUI 自身重绘，这里不额外 notify，只登记延迟写盘
            self.set_bounds(rect, cx);
            self.schedule_persist(cx);
        }
    }

    /// 结束拖动并写盘。
    pub fn end_drag(&mut self, cx: &mut Context<Self>) {
        if self.drag.take().is_some() {
            self.geometry_changed(cx);
        }
    }

    /// 切换置顶状态（同时调整原生窗口层级）。
    pub fn set_topmost(&mut self, topmost: bool, cx: &mut Context<Self>) {
        if self.topmost == topmost {
            return;
        }
        self.topmost = topmost;
        self.request_raise(cx);
        self.geometry_changed(cx);
    }

    /// 还原 100% 缩放，保持窗口中心不动。
    pub fn reset_zoom(&mut self, cx: &mut Context<Self>) {
        let (w, h) = self.image_size();
        let cx_pos = self.bounds.x + self.bounds.width / 2;
        let cy_pos = self.bounds.y + self.bounds.height / 2;
        let rect = PhysicalRect::new(cx_pos - w / 2, cy_pos - h / 2, w, h);
        self.zoom = 1.0;
        self.set_bounds(rect, cx);
        self.geometry_changed(cx);
    }

    // ---------------------------------------------------------------- 落盘

    /// 延迟写盘：登记“有待写入的变化”，并保证同一时刻只有一个定时任务；到期后统一写一次。
    fn schedule_persist(&mut self, cx: &mut Context<Self>) {
        self.persist_wanted = true;
        if self.persist_armed {
            return;
        }
        self.persist_armed = true;
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(PERSIST_DEBOUNCE).await;
            let _ = this.update(cx, |v, _| {
                v.persist_armed = false;
                if std::mem::take(&mut v.persist_wanted) {
                    v.persist_now();
                }
            });
        })
        .detach();
    }

    /// 立即写盘：几何总是写清单；二次标注有改动时写入引擎会话（原图不动，重启后仍可继续编辑）。
    pub fn persist_now(&mut self) {
        if self.closed {
            return;
        }
        self.persist_wanted = false;
        let geometry = self.geometry();
        let result = if self.payload_dirty {
            self.persist_annotations(&geometry)
        } else {
            self.shared.persist(
                &self.id,
                &geometry,
                self.created_ms,
                self.payload_bytes,
                None,
            )
        };
        match result {
            Ok(()) => {
                self.payload_dirty = false;
                tracing::debug!(id = %self.id, rect = ?self.bounds, "贴图已落盘");
            }
            Err(e) => tracing::error!(id = %self.id, error = %e, "贴图落盘失败"),
        }
    }

    /// 写入二次标注：优先存引擎会话；会话无法序列化（例如超过引擎上限）时退而把标注烘焙进源图。
    fn persist_annotations(&mut self, geometry: &PinGeometry) -> Result<(), String> {
        let session = self.layer.as_ref().map(AnnotationLayer::serialize_session);
        match session {
            Some(Ok(bytes)) => {
                self.payload_bytes =
                    self.shared
                        .persist_session(&self.id, geometry, self.created_ms, bytes)?;
                Ok(())
            }
            Some(Err(e)) => {
                tracing::warn!(id = %self.id, error = %e, "标注会话无法序列化，改为把标注烘焙进源图");
                let png = self.composite_png()?;
                let bytes = png.len() as u64;
                self.shared
                    .persist(&self.id, geometry, self.created_ms, bytes, Some(png))?;
                self.payload_bytes = bytes;
                Ok(())
            }
            None => self.shared.persist(
                &self.id,
                geometry,
                self.created_ms,
                self.payload_bytes,
                None,
            ),
        }
    }

    // ---------------------------------------------------------------- 图像导出

    /// 二次标注合成后的完整图像（不含标注时就是原图）。
    ///
    /// # 返回
    /// `(宽, 高, 不透明 RGBA)`。
    pub fn composite_rgba(&mut self) -> Option<(u32, u32, Vec<u8>)> {
        let (w, h) = self.frame.size();
        match self.layer.as_mut() {
            Some(layer) if layer.item_count() > 0 => {
                layer.export_rgba([0, 0, w as i32, h as i32], self.frame.base_view())
            }
            _ => self.frame.crop_rgba(self.frame.bounds()),
        }
    }

    /// 合成结果编码为 PNG。
    fn composite_png(&mut self) -> Result<Vec<u8>, String> {
        let (w, h, rgba) = self
            .composite_rgba()
            .ok_or_else(|| "无法导出贴图图像".to_string())?;
        encode_png(w, h, &rgba)
    }

    /// 复制（含标注的）图像到剪贴板。
    pub fn copy_to_clipboard(&mut self, cx: &mut Context<Self>) {
        let Some((w, h, rgba)) = self.composite_rgba() else {
            self.set_status("复制失败：无法导出图像", cx);
            return;
        };
        match copy_image_to_clipboard(w, h, &rgba) {
            Ok(()) => {
                tracing::info!(id = %self.id, width = w, height = h, "贴图已复制到剪贴板");
                self.set_status("已复制到剪贴板", cx);
            }
            Err(e) => {
                tracing::error!(id = %self.id, error = %e, "复制贴图失败");
                self.set_status(format!("复制失败：{e}"), cx);
            }
        }
    }

    /// 保存（含标注的）图像为文件（格式、目录、文件名取自截图保存配置）。
    pub fn save_to_file(&mut self, cx: &mut Context<Self>) {
        let Some((w, h, rgba)) = self.composite_rgba() else {
            self.set_status("保存失败：无法导出图像", cx);
            return;
        };
        match self.shared.quick_save(w, h, &rgba) {
            Ok(path) => {
                tracing::info!(id = %self.id, path = %path.display(), "贴图已保存");
                self.set_status(format!("已保存：{}", path.display()), cx);
            }
            Err(e) => {
                tracing::error!(id = %self.id, error = %e, "保存贴图失败");
                self.set_status(format!("保存失败：{e}"), cx);
            }
        }
    }

    // ---------------------------------------------------------------- 二次标注

    /// 进入二次标注模式（首次进入时创建标注层）。
    ///
    /// # 参数
    /// - `dpr`：设备像素比（决定默认线宽与字号）。
    pub fn begin_editing(&mut self, dpr: f32, cx: &mut Context<Self>) {
        if self.layer.is_none() {
            let (w, h) = self.frame.size();
            match AnnotationLayer::new(w, h, dpr) {
                Ok(layer) => self.layer = Some(layer),
                Err(e) => {
                    tracing::error!(id = %self.id, error = %e, "标注层初始化失败");
                    self.set_status("标注功能不可用", cx);
                    return;
                }
            }
        }
        self.editing = true;
        if self.tool == AnnotationTool::None {
            self.select_tool(AnnotationTool::Rectangle, cx);
        }
        cx.notify();
    }

    /// 退出二次标注模式（已画的标注保留）。
    pub fn end_editing(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.commit_text_edit(window, cx);
        self.editing = false;
        self.annotating = false;
        self.pending_annotation_point = None;
        cx.notify();
    }

    /// 选择标注工具（再次选择同一工具不取消，避免菜单点击误操作）。
    pub fn select_tool(&mut self, tool: AnnotationTool, cx: &mut Context<Self>) {
        let Some(layer) = self.layer.as_mut() else {
            return;
        };
        match layer.set_tool(tool) {
            Ok(()) => {
                self.tool = tool;
                cx.notify();
            }
            Err(e) => {
                tracing::error!(id = %self.id, error = %e, ?tool, "切换标注工具失败");
                self.set_status(format!("切换工具失败：{e}"), cx);
            }
        }
    }

    /// 在标注层上执行一次操作并安装预览增量；有变化时标记源图待落盘。
    fn run_layer(
        &mut self,
        cx: &mut Context<Self>,
        op: impl FnOnce(
            &mut AnnotationLayer,
            crate::annotation::BaseView<'_>,
        ) -> Result<LayerUpdate, String>,
    ) {
        let Some(layer) = self.layer.as_mut() else {
            return;
        };
        match op(layer, self.frame.base_view()) {
            Ok(update) => {
                let changed = !update.is_empty();
                self.install_update(update);
                if changed {
                    self.payload_dirty = true;
                    self.schedule_persist(cx);
                    cx.notify();
                }
            }
            Err(e) => {
                tracing::error!(id = %self.id, error = %e, "标注层操作失败");
                self.set_status(format!("标注失败：{e}"), cx);
            }
        }
    }

    /// 把预览增量装进分块表：替换变化块、释放空块。
    fn install_update(&mut self, update: LayerUpdate) {
        for tile in update.tiles {
            match TileSprite::from_tile(tile) {
                Some((key, sprite)) => {
                    if let Some(old) = self.tile_sprites.insert(key, sprite) {
                        self.pending_drops.push(old.image);
                    }
                }
                None => tracing::warn!("标注分块像素缓冲与尺寸不符，已丢弃"),
            }
        }
        for key in update.released {
            if let Some(old) = self.tile_sprites.remove(&key) {
                self.pending_drops.push(old.image);
            }
        }
    }

    /// 把已被替换的标注图像从 GPU 图集释放（每次渲染开头调用）。
    fn flush_pending_drops(&mut self, window: &mut Window) {
        for image in self.pending_drops.drain(..) {
            if let Err(e) = window.drop_image(image) {
                tracing::warn!(error = %e, "释放旧标注分块失败");
            }
        }
    }

    /// 标注指针按下（图像像素坐标）。
    fn start_annotation(&mut self, point: (f64, f64), cx: &mut Context<Self>) {
        self.run_layer(cx, |layer, base| layer.pointer_down(point.0, point.1, base));
        self.annotating = self.layer.as_ref().is_some_and(|l| l.is_drawing());
    }

    /// 标注拖动中：只记录最新位置，下一次渲染前统一提交。
    fn continue_annotation(&mut self, point: (f64, f64), cx: &mut Context<Self>) {
        self.pending_annotation_point = Some(point);
        cx.notify();
    }

    /// 把积压的最新指针位置提交给标注层（每帧渲染前一次）。
    fn flush_pending_annotation(&mut self, cx: &mut Context<Self>) {
        if let Some(p) = self.pending_annotation_point.take() {
            self.run_layer(cx, |layer, base| layer.pointer_move(p.0, p.1, base));
        }
    }

    /// 标注指针松开：提交元素。
    fn finish_annotation(&mut self, point: (f64, f64), cx: &mut Context<Self>) {
        self.annotating = false;
        self.pending_annotation_point = None;
        self.run_layer(cx, |layer, base| layer.pointer_up(point.0, point.1, base));
    }

    /// 用指定工具在图像坐标里拖出一个图形（自动化验收与测试入口）。
    ///
    /// # 参数
    /// - `tool`：标注工具（不含文字）。
    /// - `from` / `to`：起止点（图像像素）。
    ///
    /// # 返回
    /// 标注层不可用或工具切换失败返回错误说明。
    pub fn annotate_drag(
        &mut self,
        tool: AnnotationTool,
        from: (f64, f64),
        to: (f64, f64),
        cx: &mut Context<Self>,
    ) -> Result<(), String> {
        if self.layer.is_none() {
            self.begin_editing(self.scale.max(1.0), cx);
        }
        let layer = self.layer.as_mut().ok_or("标注层不可用")?;
        layer.set_tool(tool)?;
        self.tool = tool;
        self.editing = true;
        self.start_annotation(from, cx);
        self.finish_annotation(to, cx);
        Ok(())
    }

    /// 撤销一步标注。
    pub fn undo_annotation(&mut self, cx: &mut Context<Self>) {
        self.run_layer(cx, |layer, base| layer.undo(base));
    }

    /// 重做一步标注。
    pub fn redo_annotation(&mut self, cx: &mut Context<Self>) {
        self.run_layer(cx, |layer, base| layer.redo(base));
    }

    /// 标注层的撤销 / 重做是否可用。
    fn history_state(&self) -> (bool, bool) {
        self.layer
            .as_ref()
            .map_or((false, false), |l| (l.can_undo(), l.can_redo()))
    }

    /// 在图像坐标处开始文字输入；已有输入先提交。
    fn begin_text_edit(&mut self, origin: (f64, f64), window: &mut Window, cx: &mut Context<Self>) {
        self.commit_text_edit(window, cx);
        let Some(style) = self.layer.as_ref().map(|l| l.style()) else {
            return;
        };
        let k = self.logical_per_image_px();
        let text_style = CanvasTextStyle {
            font_family: DEFAULT_FONT_FAMILY.to_string(),
            // 输入框用窗口逻辑像素，提交后按图像像素合成，两者视觉大小一致
            font_size: style.font_px as f32 * k,
            line_height: TEXT_LINE_HEIGHT,
            color: [style.color.r, style.color.g, style.color.b, style.color.a],
            ..CanvasTextStyle::default()
        };
        let input =
            cx.new(|cx| CanvasTextInput::with_text_and_style("", text_style, None, window, cx));
        self.text_edit = Some(TextEditSession { input, origin });
        cx.notify();
    }

    /// 提交文字输入；没有输入会话时什么也不做。
    fn commit_text_edit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(session) = self.text_edit.take() else {
            return;
        };
        let text = session.input.read(cx).text().to_string();
        let (x, y) = session.origin;
        self.run_layer(cx, |layer, base| layer.commit_text(x, y, &text, base));
        self.refocus_root(window, cx);
        cx.notify();
    }

    /// 放弃文字输入。
    fn cancel_text_edit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.text_edit.take().is_some() {
            self.refocus_root(window, cx);
            cx.notify();
        }
    }

    /// 把焦点还给窗口根节点。
    fn refocus_root(&self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(handle) = &self.focus_handle {
            window.focus(handle, cx);
        }
    }

    /// 文字输入进行中时把按键交给输入框；返回 `true` 表示按键已被吞掉。
    fn route_key_to_text_edit(
        &mut self,
        key: &str,
        shift: bool,
        control: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let Some(session) = &self.text_edit else {
            return false;
        };
        let input = session.input.clone();
        match input.update(cx, |input, cx| input.handle_key(key, shift, control, cx)) {
            EditKeyOutcome::Commit => self.commit_text_edit(window, cx),
            EditKeyOutcome::Cancel => self.cancel_text_edit(window, cx),
            EditKeyOutcome::Handled | EditKeyOutcome::Ignored => {}
        }
        true
    }

    // ---------------------------------------------------------------- 输入事件

    /// 光标屏幕坐标；系统取不到时用窗口内位置推算。
    fn cursor_or(&self, fallback: (f32, f32)) -> PhysicalPoint {
        cursor_screen_position().unwrap_or_else(|_| {
            PhysicalPoint::new(
                self.bounds.x + (fallback.0 * self.scale) as i32,
                self.bounds.y + (fallback.1 * self.scale) as i32,
            )
        })
    }

    /// 左键按下。
    fn on_left_down(
        &mut self,
        pos: (f32, f32),
        click_count: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(handle) = &self.focus_handle {
            window.focus(handle, cx);
        }
        self.request_raise(cx);
        let tool_active = self.editing && self.tool != AnnotationTool::None;
        // 点击输入框以外：先提交正在输入的文字
        self.commit_text_edit(window, cx);
        if tool_active {
            let point = self.image_point(pos);
            if self.tool == AnnotationTool::Text {
                self.begin_text_edit(point, window, cx);
            } else {
                self.start_annotation(point, cx);
            }
            return;
        }
        if click_count >= 2 {
            self.run_click_action(self.interaction.double_click, window, cx);
            return;
        }
        let cursor = self.cursor_or(pos);
        let handle = hit_test_handle(self.bounds, cursor, HANDLE_SIZE, EDGE_MARGIN)
            .unwrap_or(PinnedDragHandle::Move);
        self.begin_drag(handle, cursor);
        cx.notify();
    }

    /// 执行双击 / 中键动作。
    fn run_click_action(
        &mut self,
        action: PinClickAction,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match action {
            PinClickAction::None => {}
            PinClickAction::Close => self.close(window, true),
            PinClickAction::ResetZoom => self.reset_zoom(cx),
        }
    }

    /// 窗口内任意位置的鼠标移动（含按住拖出窗口之外的移动）。
    fn on_raw_move(
        &mut self,
        pos: (f32, f32),
        pressed: Option<MouseButton>,
        cx: &mut Context<Self>,
    ) {
        if self.drag.is_some() {
            // 松开事件丢失时（例如在窗口外松开）用按键状态兜底结束拖动
            if pressed != Some(MouseButton::Left) {
                self.end_drag(cx);
                return;
            }
            let cursor = self.cursor_or(pos);
            self.drag_to(cursor, cx);
            return;
        }
        if self.annotating {
            let point = self.image_point(pos);
            self.continue_annotation(point, cx);
            return;
        }
        let handle = if self.editing {
            None
        } else {
            let cursor = self.cursor_or(pos);
            hit_test_handle(self.bounds, cursor, HANDLE_SIZE, EDGE_MARGIN)
        };
        if handle != self.hover_handle {
            self.hover_handle = handle;
            cx.notify();
        }
    }

    /// 左键松开。
    fn on_raw_up(&mut self, pos: (f32, f32), cx: &mut Context<Self>) {
        if self.drag.is_some() {
            self.end_drag(cx);
        }
        if self.annotating {
            let point = self.image_point(pos);
            self.finish_annotation(point, cx);
        }
    }

    /// 右键按下：弹出原生菜单（在异步任务里，避免在 GPUI 借用期间进入模态消息循环）。
    fn on_right_down(&mut self, cx: &mut Context<Self>) {
        let Some(window) = self.window else {
            return;
        };
        let Some(hwnd) = window.native_id().map(|id| id.0) else {
            return;
        };
        let entries = self.menu_entries();
        let cursor = self.cursor_or((0.0, 0.0));
        let entity = cx.entity();
        cx.spawn(async move |_this, cx| {
            let choice = match show_popup_menu(hwnd, cursor.x, cursor.y, &entries) {
                Ok(choice) => choice,
                Err(e) => {
                    tracing::error!(error = %e, "贴图右键菜单弹出失败");
                    None
                }
            };
            let Some(id) = choice else {
                return;
            };
            let _ = window.gpui_handle().update(cx, |_, window, app| {
                entity.update(app, |v, cx| v.apply_menu_choice(id, window, cx));
            });
        })
        .detach();
    }

    /// 按当前状态生成右键菜单。
    pub fn menu_entries(&self) -> Vec<MenuEntry> {
        let item = |id: u32, label: &str| MenuEntry::Item(MenuItem::new(id, label));
        let (can_undo, can_redo) = self.history_state();
        let mut entries = Vec::new();
        if self.editing {
            for (i, (tool, label)) in MENU_TOOLS.iter().enumerate() {
                entries.push(MenuEntry::Item(
                    MenuItem::new(MENU_TOOL_BASE + i as u32, format!("标注工具：{label}"))
                        .checked(self.tool == *tool),
                ));
            }
            entries.push(MenuEntry::Separator);
            entries.push(MenuEntry::Item(
                MenuItem::new(MENU_UNDO, "撤销").enabled(can_undo),
            ));
            entries.push(MenuEntry::Item(
                MenuItem::new(MENU_REDO, "重做").enabled(can_redo),
            ));
            entries.push(MenuEntry::Separator);
            entries.push(item(MENU_ANNOTATE, "结束二次标注"));
        } else {
            entries.push(item(MENU_ANNOTATE, "开始二次标注"));
        }
        entries.push(MenuEntry::Separator);
        entries.push(item(MENU_COPY, "复制"));
        entries.push(item(MENU_SAVE, "保存为文件"));
        entries.push(MenuEntry::Item(
            MenuItem::new(MENU_TOPMOST, "窗口置顶").checked(self.topmost),
        ));
        entries.push(item(MENU_RESET_ZOOM, "还原 100% 缩放"));
        entries.push(MenuEntry::Separator);
        entries.push(item(MENU_CLOSE, "关闭"));
        entries
    }

    /// 应用右键菜单的选择。
    ///
    /// # 参数
    /// - `id`：被选中的菜单项 ID。
    pub fn apply_menu_choice(&mut self, id: u32, window: &mut Window, cx: &mut Context<Self>) {
        match id {
            MENU_COPY => self.copy_to_clipboard(cx),
            MENU_SAVE => self.save_to_file(cx),
            MENU_TOPMOST => {
                let next = !self.topmost;
                self.set_topmost(next, cx);
            }
            MENU_ANNOTATE => {
                if self.editing {
                    self.end_editing(window, cx);
                } else {
                    self.begin_editing(window.scale_factor(), cx);
                }
            }
            MENU_RESET_ZOOM => self.reset_zoom(cx),
            MENU_UNDO => self.undo_annotation(cx),
            MENU_REDO => self.redo_annotation(cx),
            MENU_CLOSE => self.close(window, true),
            other => {
                let index = other.checked_sub(MENU_TOOL_BASE).map(|i| i as usize);
                match index.and_then(|i| MENU_TOOLS.get(i)) {
                    Some((tool, _)) => self.select_tool(*tool, cx),
                    None => tracing::warn!(id = other, "未知的贴图菜单项"),
                }
            }
        }
    }

    /// 按键处理。
    fn on_key(&mut self, ev: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let mods = ev.keystroke.modifiers;
        let key = ev.keystroke.key.as_str();
        if self.route_key_to_text_edit(key, mods.shift, mods.control, window, cx) {
            cx.stop_propagation();
            return;
        }
        match (key, mods.control) {
            ("escape", _) => {
                if self.editing {
                    self.end_editing(window, cx);
                } else {
                    self.close(window, true);
                }
            }
            ("c", true) => self.copy_to_clipboard(cx),
            ("s", true) => self.save_to_file(cx),
            ("z", true) if mods.shift => self.redo_annotation(cx),
            ("z", true) => self.undo_annotation(cx),
            ("y", true) => self.redo_annotation(cx),
            _ => {}
        }
    }

    // ---------------------------------------------------------------- 关闭与自动化

    /// 关闭贴图窗口：释放图集里的图像资源、（可选）从存储移除、销毁窗口并通知管理器。
    ///
    /// # 参数
    /// - `window`：当前窗口。
    /// - `remove_from_store`：为真时同时从持久化仓储移除（用户主动关闭）。
    pub fn close(&mut self, window: &mut Window, remove_from_store: bool) {
        if self.closed {
            return;
        }
        self.closed = true;
        tracing::info!(id = %self.id, renders = self.render_count, remove_from_store, "贴图窗口关闭");
        if remove_from_store {
            self.shared.remove(&self.id);
        }
        if let Err(e) = window.drop_image(self.frame.image()) {
            tracing::warn!(error = %e, "释放贴图底图图集失败");
        }
        self.flush_pending_drops(window);
        for (_, sprite) in self.tile_sprites.drain() {
            if let Err(e) = window.drop_image(sprite.image) {
                tracing::warn!(error = %e, "释放标注分块图集失败");
            }
        }
        self.text_edit = None;
        window.remove_window();
        self.shared.notify_closed(&self.id);
    }

    /// 执行自动化脚本的一步（验收用）。
    ///
    /// # 参数
    /// - `op`：操作。
    pub fn run_autotest_op(&mut self, op: &PinOp, window: &mut Window, cx: &mut Context<Self>) {
        tracing::info!(id = %self.id, ?op, "贴图自动化操作");
        match op {
            PinOp::Wheel { steps, ctrl } => {
                let center = PhysicalPoint::new(
                    self.bounds.x + self.bounds.width / 2,
                    self.bounds.y + self.bounds.height / 2,
                );
                self.wheel(*steps, *ctrl, center, cx);
            }
            PinOp::Drag { handle, dx, dy } => match parse_handle(handle) {
                Some(h) => {
                    let start = PhysicalPoint::new(0, 0);
                    self.begin_drag(h, start);
                    self.drag_to(PhysicalPoint::new(*dx, *dy), cx);
                    self.end_drag(cx);
                }
                None => tracing::warn!(handle, "未知的手柄名"),
            },
            PinOp::Edit { on } => {
                if *on {
                    self.begin_editing(window.scale_factor(), cx);
                } else {
                    self.end_editing(window, cx);
                }
            }
            PinOp::Annotate {
                tool,
                x0,
                y0,
                x1,
                y1,
            } => match parse_tool(tool) {
                Some(t) => {
                    if let Err(e) = self.annotate_drag(t, (*x0, *y0), (*x1, *y1), cx) {
                        tracing::error!(error = %e, "自动化标注失败");
                    }
                }
                None => tracing::warn!(tool, "未知的标注工具名"),
            },
            PinOp::Undo => self.undo_annotation(cx),
            PinOp::Redo => self.redo_annotation(cx),
            PinOp::Topmost { on } => self.set_topmost(*on, cx),
            PinOp::Menu => self.on_right_down(cx),
            PinOp::Persist => self.persist_now(),
            PinOp::Close => self.close(window, true),
        }
    }

    // ---------------------------------------------------------------- 渲染辅助

    /// 当前应显示的鼠标指针样式。
    fn cursor_style(&self) -> CursorStyle {
        if self.editing && self.tool != AnnotationTool::None {
            return if self.tool == AnnotationTool::Text {
                CursorStyle::IBeam
            } else {
                CursorStyle::Crosshair
            };
        }
        match self.drag.map(|d| d.handle).or(self.hover_handle) {
            Some(PinnedDragHandle::TopLeft | PinnedDragHandle::BottomRight) => {
                CursorStyle::ResizeUpLeftDownRight
            }
            Some(PinnedDragHandle::TopRight | PinnedDragHandle::BottomLeft) => {
                CursorStyle::ResizeUpRightDownLeft
            }
            Some(PinnedDragHandle::Top | PinnedDragHandle::Bottom) => CursorStyle::ResizeUpDown,
            Some(PinnedDragHandle::Left | PinnedDragHandle::Right) => CursorStyle::ResizeLeftRight,
            Some(PinnedDragHandle::Move) | None if self.drag.is_some() => CursorStyle::ClosedHand,
            _ => CursorStyle::OpenHand,
        }
    }

    /// 提示条文字：状态提示优先，其次是标注模式提示。
    fn badge_text(&self) -> Option<String> {
        if let Some(status) = &self.status {
            return Some(status.clone());
        }
        self.editing.then(|| EDITING_HINT.to_string())
    }
}

impl Render for PinnedWindowView {
    /// 渲染贴图窗口。
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.render_count += 1;
        self.scale = window.scale_factor();
        self.flush_pending_annotation(cx);
        self.flush_pending_drops(window);
        let k = self.logical_per_image_px();
        let active = self.hover || self.drag.is_some();
        let border = if active {
            self.interaction.border_active
        } else {
            self.interaction.border
        };
        let entity = cx.entity();

        let mut root = div()
            .id("pin-root")
            .relative()
            .size_full()
            .overflow_hidden()
            .opacity(self.opacity)
            .cursor(self.cursor_style())
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, ev: &MouseDownEvent, window, cx| {
                    let pos = (ev.position.x.as_f32(), ev.position.y.as_f32());
                    this.on_left_down(pos, ev.click_count, window, cx);
                }),
            )
            .on_mouse_down(
                MouseButton::Right,
                cx.listener(|this, _: &MouseDownEvent, _window, cx| this.on_right_down(cx)),
            )
            .on_mouse_down(
                MouseButton::Middle,
                cx.listener(|this, _: &MouseDownEvent, window, cx| {
                    let action = this.interaction.middle_click;
                    this.run_click_action(action, window, cx);
                }),
            )
            .on_scroll_wheel(cx.listener(|this, ev: &ScrollWheelEvent, _window, cx| {
                let lines_y = match ev.delta {
                    ScrollDelta::Lines(p) => p.y,
                    ScrollDelta::Pixels(p) => p.y.as_f32() / PIXELS_PER_LINE,
                };
                let cursor = this.cursor_or((ev.position.x.as_f32(), ev.position.y.as_f32()));
                this.wheel(wheel_steps(lines_y), ev.modifiers.control, cursor, cx);
            }))
            .on_hover(cx.listener(|this, hovered: &bool, _window, cx| {
                this.hover = *hovered;
                if !*hovered {
                    this.hover_handle = None;
                }
                cx.notify();
            }))
            .on_key_down(cx.listener(|this, ev: &KeyDownEvent, window, cx| {
                this.on_key(ev, window, cx);
            }));
        if let Some(handle) = &self.focus_handle {
            root = root.track_focus(handle);
        }

        // 窗口级鼠标监听：拖动时光标可能移出窗口，div 自身的移动 / 松开事件收不到
        root = root.child(
            canvas(
                |_, _, _| (),
                move |_, _, window, _| {
                    let moves = entity.clone();
                    window.on_mouse_event(move |ev: &MouseMoveEvent, phase, _window, cx| {
                        if phase != DispatchPhase::Bubble {
                            return;
                        }
                        let pos = (ev.position.x.as_f32(), ev.position.y.as_f32());
                        moves.update(cx, |v, cx| v.on_raw_move(pos, ev.pressed_button, cx));
                    });
                    let ups = entity.clone();
                    window.on_mouse_event(move |ev: &MouseUpEvent, phase, _window, cx| {
                        if phase != DispatchPhase::Bubble || ev.button != MouseButton::Left {
                            return;
                        }
                        let pos = (ev.position.x.as_f32(), ev.position.y.as_f32());
                        ups.update(cx, |v, cx| v.on_raw_up(pos, cx));
                    });
                },
            )
            .absolute()
            .top(px(0.0))
            .left(px(0.0))
            .w(px(1.0))
            .h(px(1.0)),
        );

        // 图像：资源只在创建时构造一次，这里只复用句柄，由 GPU 缩放到窗口大小
        root = root.child(
            img(ImageSource::Render(self.frame.image()))
                .absolute()
                .top(px(0.0))
                .left(px(0.0))
                .size_full()
                .object_fit(ObjectFit::Fill),
        );

        // 二次标注预览分块（图像像素 -> 窗口逻辑像素）
        for sprite in self.tile_sprites.values() {
            root = root.child(
                img(ImageSource::Render(Arc::clone(&sprite.image)))
                    .absolute()
                    .top(px(sprite.y as f32 * k))
                    .left(px(sprite.x as f32 * k))
                    .w(px(sprite.w as f32 * k))
                    .h(px(sprite.h as f32 * k))
                    .object_fit(ObjectFit::Fill),
            );
        }

        // 边框
        root = root.child(
            div()
                .absolute()
                .top(px(0.0))
                .left(px(0.0))
                .size_full()
                .border_1()
                .border_color(rgba(border)),
        );

        // 悬停时显示八向手柄（不在标注模式）；手柄一半在窗口外会被裁掉，只显示窗口内的部分
        if active && !self.editing {
            for (_, r) in handle_rects(self.bounds, HANDLE_SIZE) {
                root = root.child(
                    div()
                        .absolute()
                        .top(px((r.y - self.bounds.y) as f32 / self.scale))
                        .left(px((r.x - self.bounds.x) as f32 / self.scale))
                        .w(px(r.width as f32 / self.scale))
                        .h(px(r.height as f32 / self.scale))
                        .bg(rgba(HANDLE_FILL))
                        .border_1()
                        .border_color(rgba(self.interaction.border_active)),
                );
            }
        }

        if let Some(text) = self.badge_text() {
            root = root.child(
                div()
                    .absolute()
                    .top(px(4.0))
                    .left(px(4.0))
                    .px_2()
                    .py_0p5()
                    .rounded_xs()
                    .bg(rgba(BADGE_BG))
                    .text_color(rgba(BADGE_TEXT))
                    .text_xs()
                    .child(text),
            );
        }

        // 文字输入框：点击框内不冒泡到根节点
        if let Some(session) = &self.text_edit {
            root = root.child(
                div()
                    .absolute()
                    .top(px(session.origin.1 as f32 * k))
                    .left(px(session.origin.0 as f32 * k))
                    .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    .on_mouse_down(MouseButton::Right, |_, _, cx| cx.stop_propagation())
                    .child(session.input.clone()),
            );
        }
        root
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pinned_shared::PinShared;
    use crate::settings_state::SharedConfig;
    use snow_config::store::ConfigStore;
    use std::cell::RefCell;
    use std::path::{Path, PathBuf};

    /// 创建唯一的临时目录。
    fn temp_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("snow-pin-view-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// 在目录里打开共享上下文。
    fn shared_in(dir: &Path) -> Rc<PinShared> {
        let config: SharedConfig = Rc::new(RefCell::new(ConfigStore::open(dir.join("cfg.json"))));
        PinShared::open(dir, config, Box::new(|_| {}))
    }

    /// 生成纯色不透明 RGBA。
    fn solid(w: u32, h: u32, rgb: [u8; 3]) -> Vec<u8> {
        let mut out = Vec::with_capacity((w * h * 4) as usize);
        for _ in 0..w * h {
            out.extend_from_slice(&[rgb[0], rgb[1], rgb[2], 255]);
        }
        out
    }

    /// 用纯色图创建离屏视图，并像管理器创建贴图时那样先把原图落盘。
    fn view_in(shared: &Rc<PinShared>, w: u32, h: u32) -> PinnedWindowView {
        let frame = frame_from_rgba(w, h, solid(w, h, [10, 20, 30])).unwrap();
        let geometry = PinGeometry::new(
            PhysicalRect::new(100, 100, w as i32, h as i32),
            1.0,
            1.0,
            true,
        );
        let id = shared.new_id().unwrap();
        let png = encode_png(w, h, &solid(w, h, [10, 20, 30])).unwrap();
        shared
            .persist(&id, &geometry, 1, png.len() as u64, Some(png))
            .unwrap();
        PinnedWindowView::new(
            Rc::clone(shared),
            PinInit {
                id,
                frame,
                geometry,
                created_ms: 1,
                payload_bytes: 0,
                session: Vec::new(),
                dpr: 1.0,
            },
        )
    }

    /// 底图转换后 RGBA 导出与输入一致（R/B 只在内部交换一次）。
    #[test]
    fn frame_roundtrip_preserves_pixels() {
        let dir = temp_dir("roundtrip");
        let shared = shared_in(&dir);
        let mut view = view_in(&shared, 8, 6);
        let (w, h, rgba) = view.composite_rgba().unwrap();
        assert_eq!((w, h), (8, 6));
        assert_eq!(rgba, solid(8, 6, [10, 20, 30]));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 非法尺寸 / 长度不符的图像被拒绝而不是 panic。
    #[test]
    fn frame_from_rgba_rejects_bad_input() {
        assert!(frame_from_rgba(0, 4, vec![]).is_err());
        assert!(frame_from_rgba(2, 2, vec![0; 3]).is_err());
    }

    /// 窗口坐标到图像坐标：按缩放与 DPI 换算，并钳制在图像内。
    #[test]
    fn image_point_scaling() {
        let dir = temp_dir("point");
        let shared = shared_in(&dir);
        let mut view = view_in(&shared, 100, 50);
        // 窗口放大到 200x100（缩放 2.0），DPI 缩放 1.0：窗口内 (100,50) 对应图像 (50,25)
        view.bounds = PhysicalRect::new(0, 0, 200, 100);
        view.scale = 1.0;
        assert_eq!(view.image_point((100.0, 50.0)), (50.0, 25.0));
        // DPI 1.5：逻辑 (100,50) = 物理 (150,75) -> 图像 (75, 37.5)
        view.scale = 1.5;
        assert_eq!(view.image_point((100.0, 50.0)), (75.0, 37.5));
        // 越界钳制
        assert_eq!(view.image_point((-10.0, 9999.0)), (0.0, 49.0));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 二次标注导出：含标注像素（红色描边出现），原图本身不被改写；不同工具都能画出内容。
    #[test]
    fn export_includes_annotation_pixels() {
        let dir = temp_dir("export");
        let shared = shared_in(&dir);
        let mut view = view_in(&shared, 160, 120);
        let (_, _, before) = view.composite_rgba().unwrap();
        assert!(view.layer.is_none());
        view.layer = Some(AnnotationLayer::new(160, 120, 1.0).unwrap());
        let layer = view.layer.as_mut().unwrap();
        layer.set_tool(AnnotationTool::Rectangle).unwrap();
        let base = view.frame.base_view();
        layer.pointer_down(20.0, 20.0, base).unwrap();
        layer.pointer_move(120.0, 90.0, base).unwrap();
        layer.pointer_up(120.0, 90.0, base).unwrap();
        let (w, h, after) = view.composite_rgba().unwrap();
        assert_eq!((w, h), (160, 120));
        let changed = before
            .chunks_exact(4)
            .zip(after.chunks_exact(4))
            .filter(|(a, b)| a != b)
            .count();
        assert!(changed > 100, "标注应改变足够多的像素，实际 {changed}");
        // 矩形边框上的点（(20,60) 在左边线上）应为红色系，而矩形内部（(70,60)）保持原色
        let px = |data: &[u8], x: usize, y: usize| {
            let i = (y * 160 + x) * 4;
            [data[i], data[i + 1], data[i + 2]]
        };
        let edge = px(&after, 20, 60);
        assert!(
            edge[0] > 200 && edge[1] < 100 && edge[2] < 100,
            "边线像素 {edge:?}"
        );
        assert_eq!(px(&after, 70, 60), [10, 20, 30]);
        // 原图（底图缓冲）没有被改写
        let (_, _, base_only) = view.frame.crop_rgba(view.frame.bounds()).unwrap();
        assert_eq!(base_only, before);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 在视图上用矩形 + 直线画两个标注（直接驱动标注层，等价于两次拖动）。
    fn draw_two_annotations(view: &mut PinnedWindowView) {
        let (w, h) = view.frame.size();
        view.layer = Some(AnnotationLayer::new(w, h, 1.0).unwrap());
        for (tool, from, to) in [
            (AnnotationTool::Rectangle, (10.0, 10.0), (60.0, 40.0)),
            (AnnotationTool::Line, (5.0, 5.0), (90.0, 60.0)),
        ] {
            let layer = view.layer.as_mut().unwrap();
            layer.set_tool(tool).unwrap();
            let base = view.frame.base_view();
            layer.pointer_down(from.0, from.1, base).unwrap();
            layer.pointer_up(to.0, to.1, base).unwrap();
        }
        view.payload_dirty = true;
    }

    /// 二次标注可重编辑：落盘的源图仍是原图，标注存在引擎会话里；重启恢复后合成结果逐像素一致，
    /// 且撤销历史还在（撤销后回到原图），恢复出的标注层可以继续绘制。
    #[test]
    fn annotations_persist_as_session_and_stay_editable() {
        let dir = temp_dir("persist");
        let shared = shared_in(&dir);
        let mut view = view_in(&shared, 96, 64);
        draw_two_annotations(&mut view);
        let (_, _, composite) = view.composite_rgba().unwrap();
        assert_ne!(composite, solid(96, 64, [10, 20, 30]));
        view.persist_now();
        assert!(!view.payload_dirty);
        let stored = shared.load(view.id()).unwrap();
        assert_eq!(
            stored.rgba,
            solid(96, 64, [10, 20, 30]),
            "落盘的源图应仍是原图（未烘焙标注）"
        );
        assert!(!stored.canvas_session.is_empty());
        assert_eq!(
            stored.geometry.unwrap().rect(),
            PhysicalRect::new(100, 100, 96, 64)
        );

        // “重启”：用落盘内容重建视图
        let frame = frame_from_rgba(stored.width, stored.height, stored.rgba).unwrap();
        let mut restored = PinnedWindowView::new(
            Rc::clone(&shared),
            PinInit {
                id: stored.id,
                frame,
                geometry: stored.geometry.unwrap(),
                created_ms: stored.created_ms,
                payload_bytes: stored.payload_bytes,
                session: stored.canvas_session,
                dpr: 1.0,
            },
        );
        assert!(restored.tile_sprite_count() > 0, "恢复后应立即有标注预览块");
        let (_, _, again) = restored.composite_rgba().unwrap();
        assert_eq!(again, composite, "恢复后的合成结果应与重启前逐像素一致");
        // 历史还在：撤销两步回到原图
        for _ in 0..2 {
            let layer = restored.layer.as_mut().unwrap();
            assert!(layer.can_undo());
            layer.undo(restored.frame.base_view()).unwrap();
        }
        let (_, _, undone) = restored.composite_rgba().unwrap();
        assert_eq!(undone, solid(96, 64, [10, 20, 30]));
        // 恢复出的层可以继续画
        let layer = restored.layer.as_mut().unwrap();
        layer.set_tool(AnnotationTool::Ellipse).unwrap();
        let base = restored.frame.base_view();
        layer.pointer_down(20.0, 20.0, base).unwrap();
        layer.pointer_up(70.0, 50.0, base).unwrap();
        let (_, _, edited) = restored.composite_rgba().unwrap();
        assert_ne!(edited, undone);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 损坏的标注会话不会让贴图打不开：底图保留、没有标注层。
    #[test]
    fn corrupt_session_falls_back_to_base_image() {
        let dir = temp_dir("corrupt");
        let shared = shared_in(&dir);
        let frame = frame_from_rgba(32, 32, solid(32, 32, [1, 2, 3])).unwrap();
        let geometry = PinGeometry::new(PhysicalRect::new(0, 0, 32, 32), 1.0, 1.0, true);
        let mut view = PinnedWindowView::new(
            Rc::clone(&shared),
            PinInit {
                id: shared.new_id().unwrap(),
                frame,
                geometry,
                created_ms: 1,
                payload_bytes: 0,
                session: b"{not a session".to_vec(),
                dpr: 1.0,
            },
        );
        assert!(view.layer.is_none());
        assert_eq!(view.composite_rgba().unwrap().2, solid(32, 32, [1, 2, 3]));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 关闭后不再落盘：已从存储移除的贴图不会被写回，未移除的也不会被覆盖。
    #[test]
    fn closed_view_does_not_persist() {
        let dir = temp_dir("closed");
        let shared = shared_in(&dir);
        let mut view = view_in(&shared, 16, 16);
        view.closed = true;
        view.bounds = PhysicalRect::new(9, 9, 16, 16);
        view.persist_now();
        let stored = shared.load(view.id()).unwrap();
        assert_eq!(
            stored.geometry.unwrap().rect(),
            PhysicalRect::new(100, 100, 16, 16)
        );
        assert!(shared.remove(view.id()));
        view.persist_now();
        assert!(shared.record_ids().is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 菜单：非标注模式含“开始二次标注”，标注模式含工具列表并勾选当前工具；ID 合法唯一。
    #[test]
    fn menu_entries_by_mode() {
        let dir = temp_dir("menu");
        let shared = shared_in(&dir);
        let mut view = view_in(&shared, 16, 16);
        let labels = |entries: &[MenuEntry]| -> Vec<String> {
            entries
                .iter()
                .filter_map(|e| match e {
                    MenuEntry::Item(i) => Some(i.label.clone()),
                    MenuEntry::Separator => None,
                })
                .collect()
        };
        let normal = view.menu_entries();
        assert!(snow_platform::menu::validate_entries(&normal).is_ok());
        assert!(labels(&normal).contains(&"开始二次标注".to_string()));
        assert!(labels(&normal).contains(&"关闭".to_string()));
        view.editing = true;
        view.tool = AnnotationTool::Arrow;
        let editing = view.menu_entries();
        assert!(snow_platform::menu::validate_entries(&editing).is_ok());
        let arrow = editing
            .iter()
            .find_map(|e| match e {
                MenuEntry::Item(i) if i.label == "标注工具：箭头" => Some(i.clone()),
                _ => None,
            })
            .unwrap();
        assert!(arrow.checked);
        assert!(labels(&editing).contains(&"结束二次标注".to_string()));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 自动化脚本里的手柄名 / 工具名解析。
    #[test]
    fn autotest_name_parsing() {
        assert_eq!(
            parse_handle("bottom_right"),
            Some(PinnedDragHandle::BottomRight)
        );
        assert_eq!(parse_handle("nope"), None);
        assert_eq!(parse_tool("arrow"), Some(AnnotationTool::Arrow));
        assert_eq!(parse_tool("blur"), Some(AnnotationTool::Blur));
        assert_eq!(parse_tool("laser"), None);
        let ops: Vec<PinOp> = serde_json::from_str(
            r#"[{"op":"wheel","steps":2},{"op":"drag","handle":"move","dx":5,"dy":6},{"op":"close"}]"#,
        )
        .unwrap();
        assert_eq!(ops.len(), 3);
        assert_eq!(
            ops[0],
            PinOp::Wheel {
                steps: 2.0,
                ctrl: false
            }
        );
    }
}
