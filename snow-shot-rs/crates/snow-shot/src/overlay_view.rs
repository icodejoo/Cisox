//! 截图全屏交互覆盖窗视图（Screenshot Overlay View）。
//!
//! 承载冻结底图、四向半透明暗化遮罩、动态矩形选区与八向手柄、局部取色放大镜、
//! 尺寸标签、浮动工具栏，以及鼠标 / 键盘交互状态机。
//!
//! 坐标约定：选区与鼠标坐标均为“底图像素坐标”（物理像素，原点在覆盖窗左上角）；
//! 绘制时统一除以窗口缩放比换算成 GPUI 的逻辑像素。

use crate::annotation::{AnnotationLayer, LayerUpdate, TileImage};
use crate::annotation_style::{
    ArrowheadChoice, FONT_PRESETS, PALETTE, Rgba, ToolStyle, ToolStyleStore, WIDTH_PRESETS,
    config_key, nearest_index, panel_placement, style_fields,
};
use crate::desktop_frames::DesktopFrames;
use crate::frozen_frame::FrozenFrame;
use crate::history_nav::{FinishStep, HistoryNav, HistoryProvider, LoadOutcome, NavStep};
use crate::history_store::{HistorySnapshot, HistorySource, LoadedEntry};
use crate::ocr_client::OcrError;
use crate::ocr_flow::{OcrUiState, panel_lines};
use crate::ocr_service::OcrResult;
use crate::overlay_keymap::{DrawingKey, OverlayKeyAction, OverlayKeymap};
use crate::overlay_probe::FrameProbe;
use crate::previous_selection::{PREVIOUS_SELECTION_KEY, SelectionStyle, decode, encode};
use crate::region_select::{RegionDraft, RegionType};
use crate::screenshot_output::{
    self, ExportOverrides, ExportSettings, ManualSaveJob, SaveMode, SaveOutcome, home_directory,
};
use crate::settings_state::SharedConfig;
use crate::translate_flow::{TranslateUiState, panel_lines as translate_panel_lines, stage_text};
use crate::translate_service::{TranslateFlowError, TranslateOutcome, TranslateStage};
use crate::window_pick::{
    DRAG_THRESHOLD_LOGICAL, HighlightTransition, PickPath, PickTarget, SELECTION_TARGET_KEY,
    WindowHover, exceeds_drag_threshold,
};
use image::{Frame, RgbaImage};
use snow_app_core::command::SaveRequest;
use snow_canvas_raster::TileKey;
use snow_canvas_raster::region::{
    PathCommand, RegionMask, RegionOp, RegionShape, flatten_commands, shape_commands,
};
use snow_canvas_text::{CanvasTextInput, CanvasTextStyle, EditKeyOutcome};
use snow_config::store::ConfigStore;
use snow_i18n::{Args, I18n};
use snow_platform::clipboard::{copy_image_to_clipboard, copy_text_to_clipboard};
use snow_platform::text_raster::DEFAULT_FONT_FAMILY;
use snow_ui::shell::geometry::{PhysicalPoint, PhysicalRect};
use snow_ui::shell::selection::{
    DEFAULT_EDGE_TOLERANCE, DEFAULT_HANDLE_SIZE, DEFAULT_MINIMUM_SELECTION_SIZE, SelectionDragMode,
    SelectionState, dragged_selection_rect, handle_rects, hit_test_drag_mode,
    marquee_selection_rect, selection_size_label,
};
use snow_ui::ui::component::checkbox::Checkbox;
use snow_ui::ui::component::searchable_list::{SearchableListItem, SearchableVec};
use snow_ui::ui::component::select::{Select, SelectEvent, SelectState};
use snow_ui::ui::component::{IndexPath, Sizable, Size as ComponentSize, Theme, ThemeMode};
use snow_ui::ui::*;
use snow_ui::widgets::{
    AnnotationTool, ColorFormat, Magnifier, MagnifierGrid, ScreenshotToolbar, ToolbarAction,
    calculate_magnifier_placement, calculate_toolbar_placement,
};
use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;
use std::time::Instant;

/// 放大镜采样网格边长（像素）。
const MAGNIFIER_DIMENSION: usize = 15;
/// 放大镜浮层的逻辑尺寸（宽, 高），仅用于避让屏幕边缘。
const MAGNIFIER_LOGICAL_SIZE: (i32, i32) = (109, 178);
/// 放大镜距光标的逻辑偏移。
const MAGNIFIER_OFFSET: i32 = 16;
/// 工具栏的逻辑尺寸（宽, 高），仅用于定位与命中避让。
const TOOLBAR_LOGICAL_SIZE: (i32, i32) = (980, 36);
/// 样式面板的逻辑尺寸（宽, 高），仅用于定位与命中避让。
const STYLE_PANEL_SIZE: (i32, i32) = (560, 84);
/// 样式面板与工具栏的间距。
const STYLE_PANEL_GAP: i32 = 6;
/// 样式面板背景色。
const STYLE_PANEL_BG: u32 = 0x1F1F1FE6;
/// 样式面板边框色。
const STYLE_PANEL_BORDER: u32 = 0x00000080;
/// 样式面板里小标题的文字色。
const STYLE_LABEL_COLOR: u32 = 0xCCCCCCFF;
/// 色块边长（逻辑像素）。
const SWATCH_SIZE: f32 = 18.0;
/// 样式下拉的宽度。
const STYLE_SELECT_WIDTH: f32 = 96.0;
/// 样式下拉的高度。
const STYLE_SELECT_HEIGHT: f32 = 28.0;
/// 样式下拉浮层的最大高度。
const STYLE_SELECT_MENU_MAX_HEIGHT: f32 = 220.0;
/// 工具栏距选区的逻辑间距。
const TOOLBAR_MARGIN: i32 = 8;
/// 手柄的逻辑边长。
const HANDLE_LOGICAL_SIZE: f32 = DEFAULT_HANDLE_SIZE as f32;
/// 边缘命中容差的逻辑值。
const EDGE_TOLERANCE_LOGICAL: f32 = DEFAULT_EDGE_TOLERANCE as f32;
/// 选区外暗化遮罩色（有选区时）。
const MASK_COLOR: u32 = 0x00000099;
/// 未选区时的整屏薄遮罩色。
const IDLE_MASK_COLOR: u32 = 0x00000040;
/// 主题强调色。
const ACCENT_COLOR: u32 = 0x1677FF;
/// 尺寸标签背景色。
const LABEL_BG_COLOR: u32 = 0x000000CC;
/// 悬停窗口高亮的半透明填充色（强调色 + 低 alpha）。
const HOVER_FILL_COLOR: u32 = 0x1677FF22;
/// 底部提示条背景色。
const HINT_BG_COLOR: u32 = 0x000000B3;
/// 底部提示条文字色。
const HINT_TEXT_COLOR: u32 = 0xFFFFFFCC;
/// 底部默认提示文案的 id。
const HINT_TEXT: &str = "overlay-hint-idle";
/// 选中标注工具后的底部提示文案 id。
const TOOL_HINT_TEXT: &str = "overlay-hint-tool";
/// 文字工具编辑中的提示文案。
const TEXT_HINT_TEXT: &str = "overlay-hint-text";
/// 录屏选区模式的底部提示文案。
const RECORD_HINT_TEXT: &str = "overlay-hint-record";
/// 文字输入框的行高倍率。
const TEXT_LINE_HEIGHT: f32 = 1.25;
/// 尚未接入的工具栏动作（普通截图模式下已全部接入，故为空）。
const DISABLED_TOOLBAR_ACTIONS: [ToolbarAction; 0] = [];
/// 录屏选区模式下置灰的动作（只保留“录屏”与“取消”）。
const RECORD_MODE_DISABLED_ACTIONS: [ToolbarAction; 6] = [
    ToolbarAction::Pin,
    ToolbarAction::Ocr,
    ToolbarAction::Translate,
    ToolbarAction::ScrollCapture,
    ToolbarAction::Save,
    ToolbarAction::Copy,
];
/// 长截图选区模式下置灰的动作（只保留“长图”与“取消”）。
const SCROLL_MODE_DISABLED_ACTIONS: [ToolbarAction; 6] = [
    ToolbarAction::Pin,
    ToolbarAction::Ocr,
    ToolbarAction::Translate,
    ToolbarAction::Record,
    ToolbarAction::Save,
    ToolbarAction::Copy,
];
/// 长截图选区模式的底部提示文案。
const SCROLL_HINT_TEXT: &str = "overlay-hint-scroll";
/// OCR 结果面板的逻辑宽度上限。
const OCR_PANEL_MAX_WIDTH: f32 = 520.0;
/// OCR 结果面板背景色。
const OCR_PANEL_BG: u32 = 0x000000D9;
/// OCR 文本框描边色。
const OCR_BOX_COLOR: u32 = 0xFAAD14;
/// 双击判定所需的点击次数。
const DOUBLE_CLICK_COUNT: usize = 2;

/// 选区形状栏的估算高度（逻辑像素），用于把它摆在工具栏上方。
const REGION_BAR_HEIGHT: f32 = 30.0;

/// 选区形状栏估算半宽（逻辑像素），用于顶部居中。
const REGION_BAR_HALF_WIDTH: f32 = 220.0;

/// 选区形状的配置键。
const REGION_TYPE_KEY: &str = "screenshot_selection/region_type";
/// 标签相对选区上沿的逻辑偏移。
const LABEL_OFFSET: f32 = 22.0;
/// 标注基准的笔画数。
const BENCH_STROKES: u32 = 4;
/// 标注基准选区内缩量占短边的分母。
const BENCH_MARGIN_DIVISOR: i32 = 16;
/// 标注基准选区最小内缩量（像素）。
const BENCH_MIN_MARGIN: i32 = 4;
/// 标注基准的示例文字。
const BENCH_TEXT: &str = "Snow Shot 标注文字 Text 12345";

/// 双击选区内部的动作配置键。
const DOUBLE_CLICK_ACTION_KEY: &str = "screenshot/double_click_action";
/// 鼠标中键的动作配置键。
const MIDDLE_CLICK_ACTION_KEY: &str = "screenshot/middle_mouse_button_action";
/// 选区边框颜色配置键（`#RRGGBBAA`）。
const SELECTION_BORDER_COLOR_KEY: &str = "screenshot_ui/selection_border_color";
/// 选区外遮罩颜色配置键（`#RRGGBBAA`）。
const SELECTION_MASK_COLOR_KEY: &str = "screenshot_ui/selection_mask_color";
/// 选区尺寸显示单位配置键。
const SELECTION_UNIT_KEY: &str = "screenshot_ui/selection_display_unit";
/// 选区尺寸显示单位：逻辑像素。
const SELECTION_UNIT_LOGICAL: &str = "logical_pixels";

/// 双击 / 中键可以触发的选区动作。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClickAction {
    /// 复制选区并关闭。
    Copy,
    /// 另存为（弹对话框）。
    Save,
    /// 快速保存（不弹对话框）。
    QuickSave,
    /// 贴到屏幕。
    Pin,
    /// 什么也不做。
    None,
}

impl ClickAction {
    /// 由配置值解析；未知值返回 `None`。
    ///
    /// # 参数
    /// - `text`：配置里的动作名。
    ///
    /// ```ignore
    /// assert_eq!(ClickAction::parse("quick_save"), Some(ClickAction::QuickSave));
    /// ```
    pub fn parse(text: &str) -> Option<Self> {
        Some(match text {
            "copy" => Self::Copy,
            "save" => Self::Save,
            "quick_save" => Self::QuickSave,
            "pin" => Self::Pin,
            "none" => Self::None,
            _ => return Option::None,
        })
    }
}

/// 全局鼠标手势驱动覆盖窗的一步（鼠标按下的真实事件被钩子吞掉，由上层转发）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GestureStep {
    /// 在给定位置按下左键开始框选。
    Down,
    /// 拖动到给定位置。
    Move,
    /// 在给定位置松开：完成框选并执行自动动作。
    Up,
}

/// 读取 `#RRGGBBAA` 颜色配置；缺失或非法返回默认值。
///
/// # 参数
/// - `config`：配置存储。
/// - `key`：配置键。
/// - `default`：默认 `0xRRGGBBAA`。
fn color_setting(config: &ConfigStore, key: &str, default: u32) -> u32 {
    config
        .value(key)
        .as_str()
        .and_then(crate::pinned_model::parse_hex_color)
        .unwrap_or(default)
}

/// 放大镜取色格式配置键。
const COLOR_FORMAT_KEY: &str = "screenshot_ui/color_picker_format";
/// 放大镜显示模式配置键。
const COLOR_PICKER_MODE_KEY: &str = "screenshot_ui/color_picker_display_mode";
/// 放大镜显示模式：始终隐藏。
const COLOR_PICKER_ALWAYS_HIDE: &str = "always_hide";
/// 调整选区方式配置键。
const RESIZE_MODE_KEY: &str = "screenshot/selection_resize_mode";
/// 调整选区方式：被抓的边跟随鼠标位置。
const RESIZE_FOLLOW_POSITION: &str = "follow_mouse_position";

/// 让被抓住的边（或角）直接落在鼠标所在像素：左 / 上边取该像素起点，右 / 下边取其终点。
///
/// # 参数
/// - `mode`：抓住的手柄；非调整手柄原样返回。
/// - `rect`：当前选区。
/// - `point`：鼠标所在的底图像素。
fn grab_adjusted_rect(
    mode: SelectionDragMode,
    rect: PhysicalRect,
    point: PhysicalPoint,
) -> PhysicalRect {
    if !mode.is_resize() {
        return rect;
    }
    let (mut left, mut top, mut right, mut bottom) = (rect.x, rect.y, rect.right(), rect.bottom());
    match mode.horizontal_direction() {
        -1 => left = point.x,
        1 => right = point.x + 1,
        _ => {}
    }
    match mode.vertical_direction() {
        -1 => top = point.y,
        1 => bottom = point.y + 1,
        _ => {}
    }
    PhysicalRect::new(
        left.min(right),
        top.min(bottom),
        (right - left).abs().max(1),
        (bottom - top).abs().max(1),
    )
}

/// 放大镜坐标显示模式的配置键。
const COORDINATE_MODE_KEY: &str = "screenshot_ui/color_picker_coordinate_mode";
/// 坐标显示模式：桌面全局坐标。
const COORDINATE_MODE_GLOBAL: &str = "global";
/// 坐标显示模式：画布内相对坐标。
const COORDINATE_MODE_RELATIVE: &str = "relative";

/// 用户操作处理后的窗口去向。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OverlayOutcome {
    /// 覆盖窗继续保留。
    Stay,
    /// 关闭覆盖窗。
    Close,
    /// 在给定底图物理坐标处开始文字输入（需要窗口上下文才能创建输入框）。
    BeginText(PhysicalPoint),
    /// 手动保存任务已登记：需要在界面借用之外执行（对话框是模态的），完成后再决定去向。
    AwaitSave,
}

/// 正在进行的「加 / 减区域」操作。
struct RegionOpState {
    /// 并入还是挖出。
    op: RegionOp,
    /// 操作开始前选区本是普通矩形时记下它（取消时据此还原成矩形）；本来就是自定义区域为 `None`。
    plain_rect: Option<PhysicalRect>,
}

/// 翻到历史记录时暂存的「当前截图」：翻回来时原样放回（含标注撤销栈）。
struct LiveEndpoint {
    /// 当前截图的冻结底图。
    frame: DesktopFrames,
    /// 当前截图的标注层。
    annotations: Option<AnnotationLayer>,
    /// 离开时的选区状态。
    state: SelectionState,
    /// 离开时的自定义区域蒙版（折线 / 曲线 / 自由绘制选区）。
    region_mask: Option<RegionMask>,
}

/// 截图历史翻页的宿主：数据来源、状态机与暂存的当前截图。
struct HistoryHost {
    /// 数据来源（索引与异步读取）。
    provider: Box<dyn HistoryProvider>,
    /// 翻页状态机。
    nav: HistoryNav,
    /// 翻到历史记录期间暂存的当前截图。
    live: Option<LiveEndpoint>,
}

/// 读出的历史现场换成视图可用的底图、标注层与选区。
struct PreparedEntry {
    /// 历史底图。
    frame: DesktopFrames,
    /// 恢复出的标注层。
    annotations: Option<AnnotationLayer>,
    /// 历史选区。
    selection: PhysicalRect,
}

/// 登记待执行的手动保存：任务与选区像素。
struct PendingSave {
    /// 保存任务。
    job: ManualSaveJob,
    /// 图像宽。
    width: u32,
    /// 图像高。
    height: u32,
    /// RGBA 像素。
    rgba: Vec<u8>,
    /// 保存成功后写入截图历史的完整现场（历史关闭 / 未接入时为 `None`）。
    snapshot: Option<HistorySnapshot>,
}

/// 框选完成后自动执行的动作（快捷截图用：框选一松手就复制 / 贴图 / 识别 / 翻译）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AutoConfirm {
    /// 复制选区到剪贴板并关闭。
    Copy,
    /// 把选区贴到屏幕并关闭。
    Pin,
    /// 对选区做文字识别。
    Ocr,
    /// 对选区做识别 + 翻译。
    Translate,
    /// 另存为（弹对话框）。
    Save,
    /// 快速保存（不弹对话框）。
    QuickSave,
}

impl AutoConfirm {
    /// 对应的工具栏动作。
    pub fn toolbar_action(self) -> ToolbarAction {
        match self {
            Self::Copy => ToolbarAction::Copy,
            Self::Pin => ToolbarAction::Pin,
            Self::Ocr => ToolbarAction::Ocr,
            Self::Translate => ToolbarAction::Translate,
            // 快速保存没有对应的工具栏按钮，由 `auto_confirm_outcome` 单独处理
            Self::Save | Self::QuickSave => ToolbarAction::Save,
        }
    }
}

/// 屏幕上一块标注预览图（对应一个光栅分块）。
pub(crate) struct TileSprite {
    /// 块图像资源。
    pub(crate) image: Arc<RenderImage>,
    /// 块左上角 x（物理像素）。
    pub(crate) x: u32,
    /// 块左上角 y（物理像素）。
    pub(crate) y: u32,
    /// 块宽（物理像素）。
    pub(crate) w: u32,
    /// 块高（物理像素）。
    pub(crate) h: u32,
}

impl TileSprite {
    /// 把预览脏块装进 GPUI 图像资源（像素缓冲被移动，不拷贝）。
    ///
    /// # 参数
    /// - `tile`：预乘 BGRA 脏块。
    ///
    /// # 返回
    /// 图像资源；缓冲长度与尺寸不符返回 `None`。
    pub(crate) fn from_tile(tile: TileImage) -> Option<(TileKey, Self)> {
        let TileImage {
            key,
            x,
            y,
            w,
            h,
            bgra,
        } = tile;
        // GPUI 的 RenderImage 约定缓冲为（预乘）BGRA，这里把 BGRA 字节直接装进 RgbaImage 容器
        let buffer = RgbaImage::from_raw(w, h, bgra)?;
        let image = Arc::new(RenderImage::new(vec![Frame::new(buffer)]));
        Some((key, Self { image, x, y, w, h }))
    }
}

/// 样式下拉里的一个选项：取值（数字或头型标识）加本地化标签。
#[derive(Clone)]
struct StyleItem {
    /// 选项取值。
    value: String,
    /// 界面显示的标签。
    label: SharedString,
}

impl SearchableListItem for StyleItem {
    type Value = String;

    /// 下拉与触发器显示的标签。
    fn title(&self) -> SharedString {
        self.label.clone()
    }

    /// 选项取值。
    fn value(&self) -> &Self::Value {
        &self.value
    }
}

/// 样式下拉的状态实体类型。
type StyleSelect = SelectState<SearchableVec<StyleItem>>;

/// 样式面板里会触发变更的下拉种类。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StyleSelectKind {
    /// 线宽。
    Width,
    /// 字号。
    FontSize,
    /// 箭头头型。
    Arrowhead,
}

/// 样式面板用到的三个下拉实体（首次显示面板时创建）。
struct StyleUi {
    /// 线宽下拉。
    width: Entity<StyleSelect>,
    /// 字号下拉。
    font: Entity<StyleSelect>,
    /// 箭头头型下拉。
    arrowhead: Entity<StyleSelect>,
}

/// 正在进行的文字输入会话。
struct TextEditSession {
    /// 文字输入实体（负责 IME 与光标）。
    input: Entity<CanvasTextInput>,
    /// 文字外框左上角（底图物理坐标）。
    origin: PhysicalPoint,
}

/// 输出通道：剪贴板与文件系统（便于测试时替换）。
pub trait OutputSink {
    /// 把 RGBA 图像写入剪贴板。
    ///
    /// # 参数
    /// - `width` / `height`：图像尺寸。
    /// - `rgba`：RGBA 像素。
    fn copy_image(&mut self, width: u32, height: u32, rgba: &[u8]) -> Result<(), String>;

    /// 把文本写入剪贴板。
    fn copy_text(&mut self, text: &str) -> Result<(), String>;

    /// 用系统默认浏览器打开网页链接；默认不支持。
    ///
    /// # 参数
    /// - `url`：http / https 链接。
    fn open_url(&mut self, _url: &str) -> Result<(), String> {
        Err("opening links is not wired up".to_string())
    }

    /// 把 RGBA 图像快速保存为文件（按配置的目录 / 文件名 / 格式），返回写入的路径。
    fn save_image(&mut self, width: u32, height: u32, rgba: &[u8]) -> Result<PathBuf, String>;

    /// 另存为：弹出保存对话框让用户选路径与格式；默认退化为快速保存。
    ///
    /// # 返回
    /// 保存成功、用户取消，或已本地化的失败提示（直接显示给用户）。
    fn save_image_as(
        &mut self,
        width: u32,
        height: u32,
        rgba: &[u8],
    ) -> Result<SaveOutcome, String> {
        self.save_image(width, height, rgba).map(SaveOutcome::Saved)
    }

    /// 准备一次手动保存任务（对话框会进入模态循环，必须在界面借用之外执行）。
    ///
    /// # 参数
    /// - `request`：总线上的保存请求；`None` 表示用户点了“保存”。
    ///
    /// # 返回
    /// 任务；返回 `None` 时调用方退化为同步的 [`OutputSink::save_image_as`]。
    fn begin_manual_save(&mut self, _request: Option<&SaveRequest>) -> Option<ManualSaveJob> {
        None
    }

    /// 手动保存成功后记住目录与格式（只有走了对话框才会被调用）。
    fn remember_save(
        &mut self,
        _path: &std::path::Path,
        _format: crate::export_format::ExportFormat,
    ) {
    }

    /// 复制到剪贴板成功后的附加动作（配置开启时自动保存一份）；默认什么也不做。
    fn after_copy(&mut self, _width: u32, _height: u32, _rgba: &[u8]) {}

    /// 告知覆盖窗的原生句柄（`HWND` 整数值），供对话框当所有者；默认忽略。
    fn set_owner_window(&mut self, _hwnd: isize) {}

    /// 以选区（底图物理坐标）启动屏幕录制；默认不支持。
    ///
    /// # 参数
    /// - `region`：选区（覆盖窗底图坐标，原点为该显示器左上角）。
    fn start_recording(&mut self, _region: PhysicalRect) -> Result<(), String> {
        Err("recording is not wired up".to_string())
    }

    /// 把选区（含标注合成结果）贴到屏幕上；默认不支持。
    ///
    /// # 参数
    /// - `region`：选区（覆盖窗底图坐标，原点为该显示器左上角）。
    /// - `width` / `height`：图像尺寸（等于选区尺寸）。
    /// - `rgba`：不透明 RGBA 像素。
    fn pin_image(
        &mut self,
        _region: PhysicalRect,
        _width: u32,
        _height: u32,
        _rgba: Vec<u8>,
    ) -> Result<(), String> {
        Err("pinning is not wired up".to_string())
    }

    /// 对选区图像发起文字识别（异步：结果稍后经 [`ScreenshotOverlayView::finish_ocr`] 回来）；默认不支持。
    ///
    /// # 参数
    /// - `serial`：本次请求序号，回传结果时原样带回（过期结果据此丢弃）。
    /// - `width` / `height`：图像尺寸。
    /// - `rgba`：选区（含标注合成）的 RGBA 像素。
    fn start_ocr(
        &mut self,
        _serial: u64,
        _width: u32,
        _height: u32,
        _rgba: Vec<u8>,
    ) -> Result<(), String> {
        Err("text recognition is not wired up".to_string())
    }

    /// 触发 OCR 组件下载（异步）；默认不支持。
    fn start_ocr_download(&mut self) -> Result<(), String> {
        Err("OCR download is not wired up".to_string())
    }

    /// 打开文字识别结果窗（图片 + 文字块 + 可编辑全文）；默认不支持。
    ///
    /// # 参数
    /// - `data`：结果窗需要的图片、全文与文字块。
    fn open_recognition_window(
        &mut self,
        _data: crate::recognition_view::RecognitionData,
    ) -> Result<(), String> {
        Err("the recognition window is not wired up".to_string())
    }

    /// 对选区图像发起“识别 + 翻译”（异步：结果稍后经 [`ScreenshotOverlayView::finish_translate`] 回来）；默认不支持。
    ///
    /// # 参数
    /// - `serial`：本次请求序号，回传结果时原样带回（过期结果据此丢弃）。
    /// - `width` / `height`：图像尺寸。
    /// - `rgba`：选区（含标注合成）的 RGBA 像素。
    fn start_translate(
        &mut self,
        _serial: u64,
        _width: u32,
        _height: u32,
        _rgba: Vec<u8>,
    ) -> Result<(), String> {
        Err("text translation is not wired up".to_string())
    }

    /// 触发 onnxruntime 运行时下载（异步）；默认不支持。
    fn start_translate_download(&mut self) -> Result<(), String> {
        Err("translation runtime download is not wired up".to_string())
    }

    /// 以选区（底图物理坐标）启动长截图；默认不支持。
    ///
    /// # 参数
    /// - `region`：选区（覆盖窗底图坐标，原点为该显示器左上角）。
    fn start_scroll_capture(&mut self, _region: PhysicalRect) -> Result<(), String> {
        Err("scrolling capture is not wired up".to_string())
    }
}

/// 系统输出：真实剪贴板 + 保存目录。
pub struct SystemOutput {
    /// 图片保存目录（没有配置存储时的后备目录）。
    save_directory: PathBuf,
    /// 共享配置：每次导出时现读，设置页的改动立即生效；另存为后写回上次目录 / 格式。
    config: Option<SharedConfig>,
    /// 覆盖窗原生句柄，另存为对话框的所有者。
    owner: Option<isize>,
    /// 选区确认录屏时的回调（参数为覆盖窗底图坐标下的选区）。
    on_record: Option<Box<dyn Fn(PhysicalRect)>>,
    /// 贴图回调：选区（覆盖窗底图坐标）、图像尺寸与不透明 RGBA 像素。
    on_pin: Option<PinCallback>,
    /// 文字识别回调：`(序号, 宽, 高, RGBA)`。
    on_ocr: Option<OcrCallback>,
    /// OCR 组件下载回调。
    on_ocr_download: Option<Box<dyn Fn()>>,
    /// 打开识别结果窗的回调。
    on_recognition_window: Option<Box<dyn Fn(crate::recognition_view::RecognitionData)>>,
    /// 文字翻译回调：`(序号, 宽, 高, RGBA)`。
    on_translate: Option<OcrCallback>,
    /// 翻译运行时下载回调。
    on_translate_download: Option<Box<dyn Fn()>>,
    /// 长截图回调（参数为覆盖窗底图坐标下的选区）。
    on_scroll: Option<Box<dyn Fn(PhysicalRect)>>,
    /// 截图历史回调：复制 / 保存 / 贴图成功后触发。
    on_history: Option<HistoryCallback>,
}

/// 截图历史回调类型：`(来源, 宽, 高, RGBA)`。
type HistoryCallback = Box<dyn Fn(HistorySource, u32, u32, &[u8])>;

/// 文字识别回调类型：`(序号, 宽, 高, RGBA)`。
type OcrCallback = Box<dyn Fn(u64, u32, u32, Vec<u8>)>;

/// 贴图回调类型：`(选区, 宽, 高, RGBA)`。
type PinCallback = Box<dyn Fn(PhysicalRect, u32, u32, Vec<u8>)>;

impl SystemOutput {
    /// 创建系统输出。
    ///
    /// # 参数
    /// - `save_directory`：保存文件所在目录（不存在会自动创建）。
    pub fn new(save_directory: PathBuf) -> Self {
        Self {
            save_directory,
            config: None,
            owner: None,
            on_record: None,
            on_pin: None,
            on_ocr: None,
            on_ocr_download: None,
            on_recognition_window: None,
            on_translate: None,
            on_translate_download: None,
            on_scroll: None,
            on_history: None,
        }
    }

    /// 设置截图历史回调：复制 / 保存 / 贴图成功后触发（回调应只做投递，不阻塞）。
    ///
    /// # 参数
    /// - `callback`：接收来源、图像尺寸与 RGBA 像素。
    pub fn with_history(
        mut self,
        callback: impl Fn(HistorySource, u32, u32, &[u8]) + 'static,
    ) -> Self {
        self.on_history = Some(Box::new(callback));
        self
    }

    /// 成功输出后通知历史回调（未设置则忽略）。
    fn note_history(&self, source: HistorySource, width: u32, height: u32, rgba: &[u8]) {
        if let Some(callback) = &self.on_history {
            callback(source, width, height, rgba);
        }
    }

    /// 接入共享配置：导出格式、质量、文件名模板等从中现读。
    ///
    /// # 参数
    /// - `config`：共享配置存储。
    pub fn with_config(mut self, config: SharedConfig) -> Self {
        self.config = Some(config);
        self
    }

    /// 当前导出配置快照；没有配置存储时用默认值 + 后备目录。
    fn export_settings(&self) -> ExportSettings {
        match &self.config {
            Some(config) => ExportSettings::from_document(config.borrow().document()),
            None => ExportSettings::from_document(
                &snow_config::document::ConfigDocument::from_bytes(None),
            )
            .with_directory(&self.save_directory),
        }
    }

    /// 当前界面语言代码：配置优先，没有则跟随系统。
    fn locale(&self) -> String {
        let saved = self
            .config
            .as_ref()
            .and_then(|c| {
                c.borrow()
                    .value(crate::settings_model::LANGUAGE_KEY)
                    .as_str()
                    .map(str::to_string)
            })
            .filter(|v| !v.trim().is_empty());
        saved.unwrap_or_else(crate::sys_prefs::system_ui_language)
    }

    /// 设置文字识别回调：用户点“OCR”时触发（识别在后台线程执行）。
    ///
    /// # 参数
    /// - `callback`：接收请求序号、图像尺寸与 RGBA 像素。
    pub fn with_ocr(mut self, callback: impl Fn(u64, u32, u32, Vec<u8>) + 'static) -> Self {
        self.on_ocr = Some(Box::new(callback));
        self
    }

    /// 设置打开识别结果窗的回调。
    pub fn with_recognition_window(
        mut self,
        callback: impl Fn(crate::recognition_view::RecognitionData) + 'static,
    ) -> Self {
        self.on_recognition_window = Some(Box::new(callback));
        self
    }

    /// 设置 OCR 组件下载回调。
    pub fn with_ocr_download(mut self, callback: impl Fn() + 'static) -> Self {
        self.on_ocr_download = Some(Box::new(callback));
        self
    }

    /// 设置文字翻译回调：用户点“翻译”时触发（识别与翻译在后台线程执行）。
    ///
    /// # 参数
    /// - `callback`：接收请求序号、图像尺寸与 RGBA 像素。
    pub fn with_translate(mut self, callback: impl Fn(u64, u32, u32, Vec<u8>) + 'static) -> Self {
        self.on_translate = Some(Box::new(callback));
        self
    }

    /// 设置翻译运行时下载回调。
    pub fn with_translate_download(mut self, callback: impl Fn() + 'static) -> Self {
        self.on_translate_download = Some(Box::new(callback));
        self
    }

    /// 设置长截图回调：用户点“长图”时触发。
    ///
    /// # 参数
    /// - `callback`：接收选区（覆盖窗底图坐标）。
    pub fn with_scroll_capture(mut self, callback: impl Fn(PhysicalRect) + 'static) -> Self {
        self.on_scroll = Some(Box::new(callback));
        self
    }

    /// 设置贴图回调：用户在覆盖窗里点“贴图”时触发。
    ///
    /// # 参数
    /// - `callback`：接收选区（覆盖窗底图坐标）、图像尺寸与不透明 RGBA 像素。
    pub fn with_pin(
        mut self,
        callback: impl Fn(PhysicalRect, u32, u32, Vec<u8>) + 'static,
    ) -> Self {
        self.on_pin = Some(Box::new(callback));
        self
    }

    /// 设置录屏回调：用户在覆盖窗里确认“开始录制”时触发。
    ///
    /// # 参数
    /// - `callback`：接收选区（覆盖窗底图坐标）。
    pub fn with_recording(mut self, callback: impl Fn(PhysicalRect) + 'static) -> Self {
        self.on_record = Some(Box::new(callback));
        self
    }
}

impl OutputSink for SystemOutput {
    /// 写入系统剪贴板：不透明图用 CF_DIB；带透明区（自定义选区）时改放 CF_DIBV5 + PNG。
    fn copy_image(&mut self, width: u32, height: u32, rgba: &[u8]) -> Result<(), String> {
        if snow_platform::clipboard::has_transparency(rgba) {
            let png = screenshot_output::encode_png(width, height, rgba)?;
            snow_platform::clipboard::copy_image_with_png_to_clipboard(width, height, rgba, &png)?;
        } else {
            copy_image_to_clipboard(width, height, rgba)?;
        }
        self.note_history(HistorySource::Copied, width, height, rgba);
        Ok(())
    }

    /// 写入系统剪贴板（Unicode 文本）。
    fn copy_text(&mut self, text: &str) -> Result<(), String> {
        copy_text_to_clipboard(text)
    }

    /// 用系统默认浏览器打开链接。
    fn open_url(&mut self, url: &str) -> Result<(), String> {
        snow_platform::shell::open_url(url)
    }

    /// 快速保存：按配置的目录、文件名模板与格式直接落盘。
    fn save_image(&mut self, width: u32, height: u32, rgba: &[u8]) -> Result<PathBuf, String> {
        let settings = self.export_settings();
        let locale = self.locale();
        let path = screenshot_output::save_automatic(
            &settings,
            &ExportOverrides::default(),
            width,
            height,
            rgba,
            home_directory().as_deref(),
            snow_platform::local_time::now(),
        )
        .map_err(|e| e.manual_message(&locale))?;
        self.note_history(HistorySource::Saved, width, height, rgba);
        Ok(path)
    }

    /// 准备手动保存：给了路径直接写；要求自动路径则快速保存；否则弹系统对话框。
    fn begin_manual_save(&mut self, request: Option<&SaveRequest>) -> Option<ManualSaveJob> {
        let (overrides, mode) = match request {
            None => (ExportOverrides::default(), SaveMode::Dialog),
            Some(request) => {
                if request
                    .scale
                    .is_some_and(|s| (s - 1.0).abs() > f64::EPSILON)
                {
                    tracing::warn!(scale = ?request.scale, "保存请求的缩放比例暂未支持，按原尺寸导出");
                }
                let mode = match request
                    .path
                    .as_deref()
                    .map(str::trim)
                    .filter(|p| !p.is_empty())
                {
                    Some(path) => SaveMode::Path(path.to_string()),
                    None if request.automatic_path == Some(true) => SaveMode::Automatic,
                    None => SaveMode::Dialog,
                };
                (ExportOverrides::from_save_request(request), mode)
            }
        };
        Some(ManualSaveJob {
            settings: self.export_settings(),
            overrides,
            mode,
            locale: self.locale(),
            owner: self.owner,
            home: home_directory(),
            now: snow_platform::local_time::now(),
        })
    }

    /// 写回上次手动保存的目录与格式。
    fn remember_save(
        &mut self,
        path: &std::path::Path,
        format: crate::export_format::ExportFormat,
    ) {
        if let Some(config) = &self.config
            && let Err(e) =
                screenshot_output::remember_manual_save(&mut config.borrow_mut(), path, format)
        {
            tracing::warn!(error = %e, "记住上次保存位置失败");
        }
    }

    /// 配置开启“复制后自动保存”时，再按自动保存规则落盘一份；失败只记日志。
    fn after_copy(&mut self, width: u32, height: u32, rgba: &[u8]) {
        let settings = self.export_settings();
        if !settings.auto_save_after_copy {
            return;
        }
        let locale = self.locale();
        match screenshot_output::save_automatic(
            &settings,
            &ExportOverrides::default(),
            width,
            height,
            rgba,
            home_directory().as_deref(),
            snow_platform::local_time::now(),
        ) {
            Ok(path) => tracing::info!(path = %path.display(), "复制后已自动保存"),
            Err(e) => {
                tracing::warn!(error = %e, message = %e.auto_message(&locale), "复制后自动保存失败")
            }
        }
    }

    /// 记录覆盖窗句柄。
    fn set_owner_window(&mut self, hwnd: isize) {
        self.owner = Some(hwnd);
    }

    /// 触发录屏回调；未设置回调时报错。
    fn start_recording(&mut self, region: PhysicalRect) -> Result<(), String> {
        match &self.on_record {
            Some(callback) => {
                callback(region);
                Ok(())
            }
            None => Err("recording is not wired up".to_string()),
        }
    }

    /// 触发贴图回调；未设置回调时报错。
    fn pin_image(
        &mut self,
        region: PhysicalRect,
        width: u32,
        height: u32,
        rgba: Vec<u8>,
    ) -> Result<(), String> {
        match &self.on_pin {
            Some(callback) => {
                self.note_history(HistorySource::Pinned, width, height, &rgba);
                callback(region, width, height, rgba);
                Ok(())
            }
            None => Err("pinning is not wired up".to_string()),
        }
    }

    /// 触发文字识别回调；未设置回调时报错。
    fn start_ocr(
        &mut self,
        serial: u64,
        width: u32,
        height: u32,
        rgba: Vec<u8>,
    ) -> Result<(), String> {
        match &self.on_ocr {
            Some(callback) => {
                callback(serial, width, height, rgba);
                Ok(())
            }
            None => Err("text recognition is not wired up".to_string()),
        }
    }

    /// 触发打开识别结果窗回调；未设置回调时报错。
    fn open_recognition_window(
        &mut self,
        data: crate::recognition_view::RecognitionData,
    ) -> Result<(), String> {
        match &self.on_recognition_window {
            Some(callback) => {
                callback(data);
                Ok(())
            }
            None => Err("the recognition window is not wired up".to_string()),
        }
    }

    /// 触发 OCR 组件下载回调；未设置回调时报错。
    fn start_ocr_download(&mut self) -> Result<(), String> {
        match &self.on_ocr_download {
            Some(callback) => {
                callback();
                Ok(())
            }
            None => Err("OCR download is not wired up".to_string()),
        }
    }

    /// 触发文字翻译回调；未设置回调时报错。
    fn start_translate(
        &mut self,
        serial: u64,
        width: u32,
        height: u32,
        rgba: Vec<u8>,
    ) -> Result<(), String> {
        match &self.on_translate {
            Some(callback) => {
                callback(serial, width, height, rgba);
                Ok(())
            }
            None => Err("text translation is not wired up".to_string()),
        }
    }

    /// 触发翻译运行时下载回调；未设置回调时报错。
    fn start_translate_download(&mut self) -> Result<(), String> {
        match &self.on_translate_download {
            Some(callback) => {
                callback();
                Ok(())
            }
            None => Err("translation runtime download is not wired up".to_string()),
        }
    }

    /// 触发长截图回调；未设置回调时报错。
    fn start_scroll_capture(&mut self, region: PhysicalRect) -> Result<(), String> {
        match &self.on_scroll {
            Some(callback) => {
                callback(region);
                Ok(())
            }
            None => Err("scrolling capture is not wired up".to_string()),
        }
    }
}

/// 根据拖拽模式挑选鼠标指针样式。
///
/// # 参数
/// - `mode`：当前悬停或正在进行的拖拽模式。
/// - `dragging`：是否正在拖拽（整体移动时用握拳指针）。
///
/// ```ignore
/// assert_eq!(cursor_for_mode(SelectionDragMode::Left, false), CursorStyle::ResizeLeftRight);
/// ```
pub fn cursor_for_mode(mode: SelectionDragMode, dragging: bool) -> CursorStyle {
    match mode {
        SelectionDragMode::None | SelectionDragMode::Marquee => CursorStyle::Crosshair,
        SelectionDragMode::All if dragging => CursorStyle::ClosedHand,
        SelectionDragMode::All => CursorStyle::OpenHand,
        SelectionDragMode::Left | SelectionDragMode::Right => CursorStyle::ResizeLeftRight,
        SelectionDragMode::Top | SelectionDragMode::Bottom => CursorStyle::ResizeUpDown,
        SelectionDragMode::TopLeft | SelectionDragMode::BottomRight => {
            CursorStyle::ResizeUpLeftDownRight
        }
        SelectionDragMode::TopRight | SelectionDragMode::BottomLeft => {
            CursorStyle::ResizeUpRightDownLeft
        }
    }
}

/// 把物理像素矩形按缩放比换算成逻辑像素矩形（四舍五入到整数，供避让计算）。
fn logical_rect(rect: PhysicalRect, scale: f32) -> PhysicalRect {
    let l = |v: i32| (v as f32 / scale).round() as i32;
    PhysicalRect::new(
        l(rect.x),
        l(rect.y),
        l(rect.width).max(1),
        l(rect.height).max(1),
    )
}

/// 截图覆盖窗主视图组件。
pub struct ScreenshotOverlayView {
    /// 冻结底图。
    frame: DesktopFrames,
    /// 底图物理边界（原点 0,0）。
    screen_bounds: PhysicalRect,
    /// 当前窗口缩放比（物理 / 逻辑）。
    scale: f32,
    /// 固定缩放比覆盖（仅性能基准使用）。
    scale_override: Option<f32>,
    /// 交互选区状态。
    state: SelectionState,
    /// 当前鼠标物理坐标。
    cursor_pos: PhysicalPoint,
    /// 选区已确定时鼠标悬停命中的拖拽模式。
    hover_mode: SelectionDragMode,
    /// 放大镜采样数据。
    magnifier_grid: MagnifierGrid,
    /// 色彩格式。
    color_format: ColorFormat,
    /// 状态提示文本（显示在底部提示条）。
    status_message: Option<String>,
    /// 等待在借用之外执行的手动保存。
    pending_save: Option<PendingSave>,
    /// 导出成功后接收完整现场的截图历史出口（未接入则不拷贝整帧）。
    history_sink: Option<Box<dyn Fn(HistorySource, HistorySnapshot)>>,
    /// 截图历史翻页宿主（未接入则翻页键无效）。
    history_host: Option<HistoryHost>,
    /// 画布左上角在虚拟桌面里的坐标（单屏为该屏原点）；「上一次选区」按桌面坐标存取，换屏幕排布后不会错位。
    canvas_origin: PhysicalPoint,
    /// 放大镜坐标是否显示为桌面全局坐标（否则为画布内相对坐标）。
    coordinate_global: bool,
    /// 双击选区内部的动作。
    double_click_action: ClickAction,
    /// 鼠标中键的动作。
    middle_click_action: ClickAction,
    /// 选区边框颜色（`0xRRGGBBAA`）。
    border_color: u32,
    /// 选区外遮罩颜色（`0xRRGGBBAA`）。
    mask_color: u32,
    /// 选区尺寸标签是否用逻辑像素显示。
    logical_size_label: bool,
    /// 放大镜是否被配置为始终隐藏。
    magnifier_hidden: bool,
    /// 调整选区时是否让被抓的边直接跟随鼠标位置（而非跟随位移）。
    resize_follow_position: bool,
    /// 按住「移动整个选区」键：框选拖动变成平移。
    move_held: bool,
    /// 按住 Shift（且已绑定「保持宽高一致」）：框选保持正方形、调整选区保持原宽高比。
    keep_ratio: bool,
    /// 请求关闭后重新截图（与关闭回调共享，回调里读取并清零）。
    recapture: Rc<Cell<bool>>,
    /// 会话钩子：本视图关闭时通知运行时（多屏时其余显示器上的窗口跟着关闭）。
    close_hook: Option<Box<dyn Fn()>>,
    /// 当前选区形状类型（矩形 / 折线 / 曲线 / 自由绘制）。
    region_type: RegionType,
    /// 正在绘制的自定义区域草稿。
    region_draft: Option<RegionDraft>,
    /// 已确认的自定义区域蒙版；`None` 表示选区就是普通矩形。
    region_mask: Option<RegionMask>,
    /// 自定义区域在外接矩形范围内的遮罩图（区域外压暗 + 轮廓）。
    region_overlay: Option<Arc<RenderImage>>,
    /// 正在进行的加 / 减区域操作。
    region_op: Option<RegionOpState>,
    /// 输出通道（剪贴板 / 文件）。
    output: Box<dyn OutputSink>,
    /// 键盘焦点句柄（测试环境为空）。
    focus_handle: Option<FocusHandle>,
    /// 性能探针。
    probe: FrameProbe,
    /// 标注层（引擎 + 光栅化）；初始化失败时为空，此时只有选区功能。
    annotations: Option<AnnotationLayer>,
    /// 当前标注工具（与工具栏高亮同步）。
    tool: AnnotationTool,
    /// 每工具独立的样式与最近使用颜色。
    styles: ToolStyleStore,
    /// 样式持久化用的配置存储（测试与未注入时为空，此时样式只在本次截图内有效）。
    style_config: Option<Rc<RefCell<ConfigStore>>>,
    /// 样式面板文案所用的语料。
    i18n: &'static I18n,
    /// 样式面板的下拉实体。
    style_ui: Option<StyleUi>,
    /// 标注预览分块（只保留非空块）。
    tile_sprites: HashMap<TileKey, TileSprite>,
    /// 已被替换、等待在下一次渲染时从 GPU 图集释放的图像。
    pending_drops: Vec<Arc<RenderImage>>,
    /// 指针正在选区内绘制标注。
    annotating: bool,
    /// 拖动中尚未提交给标注层的最新指针位置（每帧渲染前合并成一次更新，避免高回报率鼠标压垮主线程）。
    pending_annotation_point: Option<PhysicalPoint>,
    /// 进行中的文字输入。
    text_edit: Option<TextEditSession>,
    /// 录屏选区模式（确认后交给录制流程而不是复制 / 保存）。
    record_mode: bool,
    /// 长截图选区模式（确认后交给滚动采集）。
    scroll_mode: bool,
    /// 框选完成后自动执行的动作（只触发一次）。
    auto_confirm: Option<AutoConfirm>,
    /// 覆盖窗键位表（读 `screenshot_shortcuts/*` 与 `drawing_shortcuts/*`）。
    keymap: OverlayKeymap,
    /// 界面语言代码（提示文案用）。
    locale: String,
    /// OCR 交互状态。
    ocr: OcrUiState,
    /// 二维码识别结果里第一个网页链接（有则结果面板提供“打开链接”）。
    qr_link: Option<String>,
    /// 最近一次 OCR 请求序号（过期结果据此丢弃）。
    ocr_serial: u64,
    /// 文字翻译交互状态。
    translate: TranslateUiState,
    /// 最近一次翻译请求序号（过期结果据此丢弃）。
    translate_serial: u64,
    /// 窗口悬停来源（智能选区开启时才有）。
    window_hover: Option<Box<dyn WindowHover>>,
    /// 悬停处的命中层级路径与当前选中层（仅 Idle 时更新并高亮）。
    pick: PickPath,
    /// 高亮框的过渡动画。
    highlight_transition: HighlightTransition,
    /// 按下时锁定的窗口矩形：位移未超阈值就松开则直接作为选区，超过则作废转手动框选。
    click_window: Option<PhysicalRect>,
}

impl ScreenshotOverlayView {
    /// 创建单显示器覆盖窗主视图（不依赖 GPUI 上下文，可离屏测试）。
    ///
    /// # 参数
    /// - `frame`：冻结底图。
    /// - `scale`：窗口缩放比（物理 / 逻辑），非法值按 1.0 处理。
    /// - `initial_cursor`：创建时光标在底图内的坐标，用于初始放大镜取样。
    /// - `output`：输出通道。
    ///
    /// # 返回
    /// 覆盖窗视图实例。
    ///
    /// ```ignore
    /// let frame = FrozenFrame::from_captured(CapturedScreen::new_solid(100, 80, (0, 0, 0, 255)))?;
    /// let view = ScreenshotOverlayView::new(frame, 1.0, PhysicalPoint::new(10, 10), sink);
    /// assert_eq!(view.current_selection(), None);
    /// ```
    pub fn new(
        frame: FrozenFrame,
        scale: f32,
        initial_cursor: PhysicalPoint,
        output: Box<dyn OutputSink>,
    ) -> Self {
        Self::new_multi(DesktopFrames::single(frame), scale, initial_cursor, output)
    }

    /// 创建多显示器共享的覆盖窗主视图：逻辑工作在虚拟桌面画布坐标上，每块屏由各自的窗口渲染一片。
    ///
    /// # 参数
    /// - `frame`：各显示器的冻结底图与它们在画布里的矩形。
    /// - `scale`：初始缩放比（渲染与事件时按各窗口实际缩放更新）。
    /// - `initial_cursor`：创建时光标的画布坐标。
    /// - `output`：输出通道。
    ///
    /// ```ignore
    /// let view = ScreenshotOverlayView::new_multi(frames, 1.0, cursor, sink);
    /// ```
    pub fn new_multi(
        frame: DesktopFrames,
        scale: f32,
        initial_cursor: PhysicalPoint,
        output: Box<dyn OutputSink>,
    ) -> Self {
        let screen_bounds = frame.bounds();
        let scale = if scale.is_finite() && scale > 0.0 {
            scale
        } else {
            1.0
        };
        let (frame_w, frame_h) = frame.size();
        let annotations = match AnnotationLayer::new(frame_w, frame_h, scale) {
            Ok(layer) => Some(layer),
            Err(e) => {
                tracing::error!(error = %e, "标注层初始化失败，本次截图不支持标注");
                None
            }
        };
        let mut view = Self {
            frame,
            screen_bounds,
            scale,
            scale_override: None,
            state: SelectionState::Idle,
            cursor_pos: PhysicalPoint::new(0, 0),
            hover_mode: SelectionDragMode::None,
            magnifier_grid: MagnifierGrid::new_solid(MAGNIFIER_DIMENSION, (0, 0, 0, 255)),
            color_format: ColorFormat::Hex,
            status_message: None,
            pending_save: None,
            history_sink: None,
            history_host: None,
            close_hook: None,
            canvas_origin: PhysicalPoint::new(0, 0),
            coordinate_global: true,
            double_click_action: ClickAction::Copy,
            middle_click_action: ClickAction::Pin,
            border_color: (ACCENT_COLOR << 8) | 0xFF,
            mask_color: MASK_COLOR,
            logical_size_label: false,
            magnifier_hidden: false,
            resize_follow_position: false,
            move_held: false,
            keep_ratio: false,
            recapture: Rc::new(Cell::new(false)),
            region_type: RegionType::Rectangle,
            region_draft: None,
            region_mask: None,
            region_overlay: None,
            region_op: None,
            output,
            focus_handle: None,
            probe: FrameProbe::new(),
            annotations,
            tool: AnnotationTool::None,
            styles: ToolStyleStore::new(),
            style_config: None,
            i18n: crate::ocr_backend::i18n_for(snow_i18n::FALLBACK_LOCALE),
            style_ui: None,
            tile_sprites: HashMap::new(),
            pending_drops: Vec::new(),
            annotating: false,
            pending_annotation_point: None,
            text_edit: None,
            record_mode: false,
            scroll_mode: false,
            auto_confirm: None,
            keymap: OverlayKeymap::default(),
            locale: snow_i18n::FALLBACK_LOCALE.to_string(),
            ocr: OcrUiState::Idle,
            qr_link: None,
            ocr_serial: 0,
            translate: TranslateUiState::Idle,
            translate_serial: 0,
            window_hover: None,
            pick: PickPath::new(PickTarget::WindowSubElement),
            highlight_transition: HighlightTransition::new(true),
            click_window: None,
        };
        let start = view.clamp_point(initial_cursor);
        view.cursor_pos = start;
        view.update_magnifier_grid(start);
        view
    }

    /// 在 GPUI 中创建多个窗口共用的视图实体。
    ///
    /// # 参数
    /// - `app`：应用上下文。
    /// - `frames`：各显示器的冻结底图。
    /// - `scale`：初始缩放比。
    /// - `initial_cursor`：创建时光标的画布坐标。
    /// - `output`：输出通道。
    ///
    /// ```ignore
    /// let shared = ScreenshotOverlayView::create_shared(app, frames, 1.0, cursor, output);
    /// ```
    pub fn create_shared(
        app: &mut App,
        frames: DesktopFrames,
        scale: f32,
        initial_cursor: PhysicalPoint,
        output: Box<dyn OutputSink>,
    ) -> Entity<Self> {
        app.new(|_| Self::new_multi(frames, scale, initial_cursor, output))
    }

    /// 固定缩放比（性能基准用：让 4K 底图铺满较小的窗口时坐标仍自洽）。
    pub fn set_scale_override(&mut self, scale: Option<f32>) {
        self.scale_override = scale.filter(|s| s.is_finite() && *s > 0.0);
        if let Some(s) = self.scale_override {
            self.scale = s;
        }
    }

    /// 接入悬停来源，开启智能选区（窗口 / 控件层级）。
    ///
    /// # 参数
    /// - `source`：悬停来源；传 `None` 关闭该功能。
    /// - `target`：初始目标层级（来自 `screenshot_selection/selection_target`）。
    /// - `animate`：高亮框是否使用过渡动画。
    ///
    /// ```ignore
    /// view.set_window_hover(Some(Box::new(picker)), PickTarget::WindowSubElement, true);
    /// ```
    pub fn set_window_hover(
        &mut self,
        source: Option<Box<dyn WindowHover>>,
        target: PickTarget,
        animate: bool,
    ) {
        self.window_hover = source;
        self.pick = PickPath::new(target);
        self.highlight_transition = HighlightTransition::new(animate);
        self.click_window = None;
    }

    /// 当前应高亮的窗口矩形：空闲时是悬停窗口，按下未超阈值时是锁定窗口。
    fn window_highlight(&self) -> Option<PhysicalRect> {
        match self.state {
            SelectionState::Idle => self.pick.current(),
            SelectionState::MarqueeDragging { .. } => self.click_window,
            _ => None,
        }
    }

    /// 向悬停来源查询指定点下的层级路径并更新当前选中层；顺带吸收后台细化出的更深层。
    fn refresh_window_hover(&mut self, point: PhysicalPoint) {
        let bounds = self.screen_bounds;
        if self.region_type != RegionType::Rectangle {
            self.pick.clear();
            return;
        }
        let Some(source) = self.window_hover.as_mut() else {
            return;
        };
        match source.hover(point, self.pick.target()) {
            Some(path) => {
                self.pick
                    .apply_hit_path(&path, bounds, DEFAULT_MINIMUM_SELECTION_SIZE);
            }
            None => self.pick.clear(),
        }
        self.absorb_refinement();
    }

    /// 吸收后台细化出的更深层路径（有就应用）。
    ///
    /// # 返回
    /// 路径是否被替换（需要重绘）。
    fn absorb_refinement(&mut self) -> bool {
        let bounds = self.screen_bounds;
        let Some(refined) = self.window_hover.as_mut().and_then(|s| s.refinement()) else {
            return false;
        };
        self.pick
            .apply_refinement(&refined, bounds, DEFAULT_MINIMUM_SELECTION_SIZE)
    }

    /// 渲染前轮询后台细化：空闲时吸收新路径，并在细化仍在进行时返回 `true`，让调用方继续请求下一帧。
    ///
    /// # 返回
    /// 后台细化是否仍在进行。
    fn poll_refinement(&mut self) -> bool {
        if self.state == SelectionState::Idle {
            self.absorb_refinement();
        }
        self.window_hover
            .as_ref()
            .is_some_and(|s| s.refinement_pending())
    }

    /// 滚轮切换智能选区层级：向上（`lines_y > 0`）向外，向下向内；仅空闲且开启智能选区时生效。
    ///
    /// # 参数
    /// - `lines_y`：滚轮行数，正为向上。
    /// - `point`：当前光标（底图物理坐标）。
    ///
    /// # 返回
    /// 是否消费了这次滚轮。
    ///
    /// ```ignore
    /// view.handle_scroll(1.0, PhysicalPoint::new(60, 50));
    /// ```
    pub fn handle_scroll(&mut self, lines_y: f32, point: PhysicalPoint) -> bool {
        if self.window_hover.is_none() || self.state != SelectionState::Idle || self.annotating {
            return false;
        }
        let point = self.clamp_point(point);
        self.cursor_pos = point;
        self.refresh_window_hover(point);
        if lines_y > 0.0 {
            self.pick.select_step(1);
        } else if lines_y < 0.0 {
            self.pick.select_step(-1);
        }
        true
    }

    /// 在「窗口」与「窗口内子控件」目标之间切换，写回配置并按光标位置重新取层级。
    ///
    /// # 返回
    /// 是否切换成功；未开启智能选区或不在空闲态时为 `false`。
    fn toggle_selection_target(&mut self) -> bool {
        if self.window_hover.is_none() || self.state != SelectionState::Idle {
            return false;
        }
        let target = self.pick.toggle_target();
        self.persist_selection_target(target);
        self.refresh_window_hover(self.cursor_pos);
        true
    }

    /// 把选区目标写回配置并落盘；失败只记日志。
    fn persist_selection_target(&self, target: PickTarget) {
        let Some(config) = self.config_handle() else {
            return;
        };
        let mut store = config.borrow_mut();
        if let Err(e) = store.set_value(SELECTION_TARGET_KEY, serde_json::json!(target.as_config()))
        {
            tracing::warn!(error = %e, "写入选区目标配置失败");
            return;
        }
        if let Err(e) = store.flush() {
            tracing::warn!(error = %e, "选区目标落盘失败");
        }
    }

    /// 拖拽阈值（物理像素，随缩放比放大）。
    fn drag_threshold(&self) -> i32 {
        (DRAG_THRESHOLD_LOGICAL * self.scale).round() as i32
    }

    /// 底图物理尺寸。
    pub fn frame_size(&self) -> (u32, u32) {
        self.frame.size()
    }

    /// 探针汇总文本（关闭覆盖窗时写日志）。
    pub fn probe_summary(&self) -> String {
        self.probe.describe()
    }

    /// 获取当前生效的物理选区矩形（拖拽中给出已按屏幕边界钳制后的实时矩形）。
    pub fn current_selection(&self) -> Option<PhysicalRect> {
        match self.state {
            SelectionState::Reshaping {
                mode,
                origin_rect,
                origin_pos,
                current_pos,
            } => Some(dragged_selection_rect(
                mode,
                origin_rect,
                origin_pos,
                current_pos,
                Some(self.screen_bounds),
                DEFAULT_MINIMUM_SELECTION_SIZE,
                self.locked_ratio(origin_rect),
            )),
            _ => self.state.current_rect(),
        }
    }

    /// 是否已经确定选区（可复制 / 保存）。
    fn has_committed_selection(&self) -> bool {
        matches!(
            self.state,
            SelectionState::Selected { .. } | SelectionState::Reshaping { .. }
        )
    }

    /// 把点钳制在底图范围内（拖出窗口时使用）。
    fn clamp_point(&self, p: PhysicalPoint) -> PhysicalPoint {
        PhysicalPoint::new(
            p.x.clamp(self.screen_bounds.x, self.screen_bounds.right() - 1),
            p.y.clamp(self.screen_bounds.y, self.screen_bounds.bottom() - 1),
        )
    }

    /// GPUI 逻辑坐标转底图物理坐标。
    ///
    /// # 参数
    /// - `logical`：窗口内的逻辑像素坐标。
    ///
    /// ```ignore
    /// let p = view.physical_point(point(px(100.0), px(50.0)));
    /// ```
    pub fn physical_point(&self, logical: Point<Pixels>) -> PhysicalPoint {
        self.canvas_point(0, logical)
    }

    /// 第 `index` 块显示器窗口内的逻辑坐标换成画布物理坐标（可落在本屏之外：跨屏拖动时指针已离开本窗口）。
    ///
    /// # 参数
    /// - `index`：事件所属窗口对应的显示器序号。
    /// - `logical`：窗口内的逻辑像素坐标（可为负或超出窗口）。
    ///
    /// ```ignore
    /// let p = view.canvas_point(1, point(px(30.0), px(40.0)));
    /// ```
    pub fn canvas_point(&self, index: usize, logical: Point<Pixels>) -> PhysicalPoint {
        let origin = self
            .frame
            .rect(index.min(self.frame.count().saturating_sub(1)));
        self.clamp_point(PhysicalPoint::new(
            origin.x + (logical.x.as_f32() * self.scale).round() as i32,
            origin.y + (logical.y.as_f32() * self.scale).round() as i32,
        ))
    }

    /// 选区是否跨越多块显示器（录屏 / 长截图的采集窗绑定单屏，跨屏时不允许）。
    fn selection_spans_monitors(&self) -> bool {
        let Some(selection) = self.current_selection() else {
            return false;
        };
        (0..self.frame.count())
            .filter(|&i| {
                self.frame
                    .rect(i)
                    .intersect(&selection)
                    .is_some_and(|r| !r.is_empty())
            })
            .count()
            > 1
    }

    /// 物理像素长度转 GPUI 逻辑像素。
    fn lp(&self, physical: i32) -> Pixels {
        px(physical as f32 / self.scale)
    }

    /// 边缘命中容差（物理像素，随缩放比放大，保持逻辑手感一致）。
    fn edge_tolerance(&self) -> i32 {
        (EDGE_TOLERANCE_LOGICAL * self.scale).round() as i32
    }

    /// 从底图提取光标周围像素网格（只读 15x15 个像素）。
    fn update_magnifier_grid(&mut self, cursor: PhysicalPoint) {
        self.magnifier_grid = MagnifierGrid {
            dimension: MAGNIFIER_DIMENSION,
            pixels: self
                .frame
                .sample_rgba_grid(cursor.x, cursor.y, MAGNIFIER_DIMENSION),
        };
    }

    /// 处理鼠标左键按下。
    ///
    /// # 参数
    /// - `point`：底图物理坐标。
    /// - `click_count`：连击次数（双击选区内部 = 复制）。
    ///
    /// # 返回
    /// 窗口去向。
    ///
    /// ```ignore
    /// view.handle_mouse_down(PhysicalPoint::new(100, 100), 1);
    /// ```
    pub fn handle_mouse_down(
        &mut self,
        point: PhysicalPoint,
        click_count: usize,
    ) -> OverlayOutcome {
        let point = self.clamp_point(point);
        self.cursor_pos = point;
        match self.state {
            SelectionState::Idle if self.custom_input_active() => {
                self.begin_region_input(point, click_count);
            }
            SelectionState::Idle => {
                // 按下点处的窗口先锁定，松开时若位移很小就直接选中该窗口
                self.refresh_window_hover(point);
                self.click_window = self.pick.current();
                self.state = SelectionState::MarqueeDragging {
                    start: point,
                    current: point,
                };
            }
            SelectionState::Selected { rect } => {
                let mode = self.drag_mode_for(rect, point);
                // 非矩形选区：点框外先整体重置，回到选区阶段（下一次点击才开始画）
                if mode == SelectionDragMode::None
                    && (self.region_type != RegionType::Rectangle || self.region_mask.is_some())
                {
                    self.clear_region();
                    self.reset_annotations();
                    self.state = SelectionState::Idle;
                    self.hover_mode = SelectionDragMode::None;
                    return OverlayOutcome::Stay;
                }
                // 选中标注工具后，选区内部（非手柄 / 边缘）的按下属于标注
                if mode == SelectionDragMode::All && self.tool != AnnotationTool::None {
                    if self.tool == AnnotationTool::Text {
                        return OverlayOutcome::BeginText(point);
                    }
                    self.start_annotation(point, rect);
                    return OverlayOutcome::Stay;
                }
                if mode == SelectionDragMode::All && click_count >= DOUBLE_CLICK_COUNT {
                    return if self.record_mode {
                        self.start_recording_and_close()
                    } else if self.scroll_mode {
                        self.start_scroll_capture_and_close()
                    } else {
                        self.run_click_action(self.double_click_action)
                    };
                }
                self.state = if mode == SelectionDragMode::None {
                    // 点击选区外，重新开始框选
                    SelectionState::MarqueeDragging {
                        start: point,
                        current: point,
                    }
                } else {
                    let origin_rect = if self.resize_follow_position {
                        grab_adjusted_rect(mode, rect, point)
                    } else {
                        rect
                    };
                    SelectionState::Reshaping {
                        mode,
                        origin_rect,
                        origin_pos: point,
                        current_pos: point,
                    }
                };
            }
            _ => {}
        }
        OverlayOutcome::Stay
    }

    /// 处理鼠标移动（更新放大镜取样、拖拽中的选区与悬停指针）。
    ///
    /// # 参数
    /// - `point`：底图物理坐标。
    pub fn handle_mouse_move(&mut self, point: PhysicalPoint) {
        let point = self.clamp_point(point);
        let previous = self.cursor_pos;
        self.cursor_pos = point;
        self.update_magnifier_grid(point);
        if self.annotating {
            self.continue_annotation(point);
            return;
        }
        match self.state {
            SelectionState::MarqueeDragging { start, .. } if self.move_held => {
                // 按住移动键：整个框跟着鼠标平移（起点与终点一起位移）
                let dx = point.x - previous.x;
                let dy = point.y - previous.y;
                let start = self.clamp_point(PhysicalPoint::new(start.x + dx, start.y + dy));
                self.state = SelectionState::MarqueeDragging {
                    start,
                    current: point,
                };
            }
            SelectionState::MarqueeDragging { start, .. } => {
                let point = self.constrained_end(start, point);
                if self.click_window.is_some()
                    && exceeds_drag_threshold(start, point, self.drag_threshold())
                {
                    // 位移超过阈值：放弃窗口选区，转为手动框选
                    self.click_window = None;
                    self.pick.clear();
                }
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
                self.hover_mode = mode;
                self.state = SelectionState::Reshaping {
                    mode,
                    origin_rect,
                    origin_pos,
                    current_pos: point,
                };
            }
            SelectionState::Selected { rect } => {
                self.hover_mode = self.drag_mode_for(rect, point);
            }
            SelectionState::Idle => {
                self.hover_mode = SelectionDragMode::None;
                if let Some(draft) = self.region_draft.as_mut() {
                    draft.drag_to((point.x as f32, point.y as f32));
                }
                self.refresh_window_hover(point);
            }
        }
    }

    /// 处理鼠标左键释放。
    ///
    /// # 参数
    /// - `point`：底图物理坐标。
    ///
    /// # 返回
    /// 这次释放是否确认了一个选区（框选或调整结束且尺寸有效）；标注拖动与无效选区为 `false`。
    pub fn handle_mouse_up(&mut self, point: PhysicalPoint) -> bool {
        let point = self.clamp_point(point);
        self.cursor_pos = point;
        self.move_held = false;
        if self.annotating {
            self.finish_annotation(point);
            return false;
        }
        // 自由绘制：松开即收笔，随后尝试完成
        if let Some(draft) = self.region_draft.as_mut()
            && draft.release((point.x as f32, point.y as f32))
        {
            let committed = self.commit_region_draft();
            if !committed {
                self.region_draft = None;
            }
            return committed;
        }
        match self.state {
            SelectionState::MarqueeDragging { start, .. } => {
                let window = self.click_window.take();
                self.pick.clear();
                if let Some(rect) = window
                    && !exceeds_drag_threshold(start, point, self.drag_threshold())
                    && rect.contains(point)
                    && rect.width >= DEFAULT_MINIMUM_SELECTION_SIZE
                    && rect.height >= DEFAULT_MINIMUM_SELECTION_SIZE
                {
                    if self.region_op.is_some() {
                        return self.merge_rect_operand(rect);
                    }
                    self.state = SelectionState::Selected { rect };
                    return true;
                }
                let point = self.constrained_end(start, point);
                let r = marquee_selection_rect(start, point);
                if self.region_op.is_some()
                    && r.width >= DEFAULT_MINIMUM_SELECTION_SIZE
                    && r.height >= DEFAULT_MINIMUM_SELECTION_SIZE
                {
                    self.state = SelectionState::Idle;
                    let merged = self.merge_rect_operand(r);
                    if !merged {
                        self.refresh_window_hover(point);
                    }
                    return merged;
                }
                self.state = if r.width >= DEFAULT_MINIMUM_SELECTION_SIZE
                    && r.height >= DEFAULT_MINIMUM_SELECTION_SIZE
                {
                    SelectionState::Selected { rect: r }
                } else {
                    SelectionState::Idle
                };
                if self.state == SelectionState::Idle {
                    self.refresh_window_hover(point);
                }
                return matches!(self.state, SelectionState::Selected { .. });
            }
            SelectionState::Reshaping {
                mode,
                origin_rect,
                origin_pos,
                ..
            } => {
                let rect = dragged_selection_rect(
                    mode,
                    origin_rect,
                    origin_pos,
                    point,
                    Some(self.screen_bounds),
                    DEFAULT_MINIMUM_SELECTION_SIZE,
                    self.locked_ratio(origin_rect),
                );
                // 自定义区域跟着选区一起平移
                if let Some(mask) = self.region_mask.as_mut() {
                    *mask = mask.translated(rect.x - origin_rect.x, rect.y - origin_rect.y);
                }
                self.state = SelectionState::Selected { rect };
                self.hover_mode = SelectionDragMode::None;
                return true;
            }
            _ => {}
        }
        false
    }

    /// 处理鼠标右键：有选区时先撤销选区，没有选区则关闭覆盖窗。
    pub fn handle_right_click(&mut self) -> OverlayOutcome {
        if self.state == SelectionState::Idle {
            // 正在画自定义区域：右键先取消草稿，再取消加 / 减区域，都没有才关闭
            if self.region_draft.take().is_some() || self.cancel_region_op() {
                return OverlayOutcome::Stay;
            }
            return OverlayOutcome::Close;
        }
        // 正看着历史记录：右键先回到当前截图（对齐 Qt `returnToCurrentScreenshot`）
        if self.history_return_to_live() {
            return OverlayOutcome::Stay;
        }
        self.state = SelectionState::Idle;
        self.hover_mode = SelectionDragMode::None;
        self.pick.clear();
        self.click_window = None;
        self.clear_region();
        self.reset_annotations();
        // 回到智能选区：不等下一次移动，立刻按光标位置重新命中
        self.refresh_window_hover(self.cursor_pos);
        OverlayOutcome::Stay
    }

    /// 处理键盘按键。
    ///
    /// # 参数
    /// - `key`：GPUI 按键名（小写，如 `escape` / `enter` / `c`）。
    /// - `control`：是否按住 Ctrl。
    /// - `shift`：是否按住 Shift。
    ///
    /// # 返回
    /// 窗口去向。Esc 关闭；Enter / Ctrl+C 复制选区；Ctrl+S 保存；C 复制光标处颜色；
    /// Ctrl+Z 撤销，Ctrl+Y / Ctrl+Shift+Z 重做。
    ///
    /// ```ignore
    /// assert_eq!(view.handle_key("escape", false, false), OverlayOutcome::Close);
    /// ```
    pub fn handle_key(&mut self, key: &str, control: bool, shift: bool) -> OverlayOutcome {
        self.handle_keystroke(key, control, shift, false)
    }

    /// 处理带 Alt 状态的键盘按键：先处理翻译 / OCR 界面里的固定键，再查配置键位表。
    ///
    /// # 参数
    /// - `key`：GPUI 按键名（小写）。
    /// - `control` / `shift` / `alt`：修饰键状态。
    ///
    /// # 返回
    /// 窗口去向。Enter 固定为确认（复制 / 录屏 / 长截图）；Ctrl+Shift+Z 固定为重做；其余按键位表。
    pub fn handle_keystroke(
        &mut self,
        key: &str,
        control: bool,
        shift: bool,
        alt: bool,
    ) -> OverlayOutcome {
        match (key, control) {
            ("enter", _) if self.region_draft.is_some() => {
                self.finish_region_draft();
                OverlayOutcome::Stay
            }
            ("backspace", false) if self.region_draft.is_some() => {
                if let Some(draft) = self.region_draft.as_mut() {
                    draft.remove_last();
                }
                OverlayOutcome::Stay
            }
            ("tab", true) => {
                self.cycle_region_type(shift);
                OverlayOutcome::Stay
            }
            ("e", false) if matches!(self.ocr, OcrUiState::Done { ref text, .. } if !text.is_empty()) =>
            {
                self.open_recognition_window();
                OverlayOutcome::Stay
            }
            ("o", false)
                if self.qr_link.is_some() && matches!(self.ocr, OcrUiState::Done { .. }) =>
            {
                self.open_qr_link()
            }
            ("enter", _) if matches!(self.ocr, OcrUiState::Done { .. }) => {
                self.copy_ocr_text_and_close()
            }
            ("enter", _) if matches!(self.translate, TranslateUiState::Done { .. }) => {
                self.copy_translation_and_close()
            }
            ("d", false)
                if matches!(
                    self.translate,
                    TranslateUiState::Failed {
                        can_download: true,
                        ..
                    }
                ) =>
            {
                self.start_translate_download();
                OverlayOutcome::Stay
            }
            ("d", false)
                if matches!(
                    self.ocr,
                    OcrUiState::Failed {
                        can_download: true,
                        ..
                    }
                ) =>
            {
                self.start_ocr_download();
                OverlayOutcome::Stay
            }
            ("enter", _) if self.record_mode => self.start_recording_and_close(),
            ("enter", _) if self.scroll_mode => self.start_scroll_capture_and_close(),
            ("enter", _) => self.copy_selection_and_close(),
            _ => match self.keymap.resolve(key, control, shift, alt) {
                Some(action) => self.run_key_action(action),
                // 固定后备：Ctrl+Shift+Z 重做
                None if key == "z" && control && shift => {
                    self.redo_annotation();
                    OverlayOutcome::Stay
                }
                None => OverlayOutcome::Stay,
            },
        }
    }

    /// 执行键位动作。
    ///
    /// # 参数
    /// - `action`：键位表解析出的动作。
    fn run_key_action(&mut self, action: OverlayKeyAction) -> OverlayOutcome {
        let selecting_mode = self.record_mode || self.scroll_mode;
        let tools_ready = self.current_selection().is_some();
        match action {
            // 加 / 减区域进行中：取消键先取消这次操作
            OverlayKeyAction::Cancel if self.region_op.is_some() => {
                self.cancel_region_op();
                OverlayOutcome::Stay
            }
            // 翻译 / OCR 界面打开时，取消键先退出该界面，再按一次才关闭覆盖窗
            OverlayKeyAction::Cancel if self.translate.is_visible() => {
                self.dismiss_translate();
                OverlayOutcome::Stay
            }
            OverlayKeyAction::Cancel if self.ocr.is_visible() => {
                self.dismiss_ocr();
                OverlayOutcome::Stay
            }
            OverlayKeyAction::Cancel => OverlayOutcome::Close,
            // 录屏 / 长截图选区模式下没有复制 / 保存
            OverlayKeyAction::CopyToClipboard | OverlayKeyAction::SaveAsFile if selecting_mode => {
                OverlayOutcome::Stay
            }
            OverlayKeyAction::CopyToClipboard => self.copy_selection_and_close(),
            OverlayKeyAction::SaveAsFile => self.save_selection_and_close(),
            OverlayKeyAction::PinToScreen => self.apply_action(ToolbarAction::Pin),
            OverlayKeyAction::VideoRecording => self.apply_action(ToolbarAction::Record),
            OverlayKeyAction::TextRecognition => self.apply_action(ToolbarAction::Ocr),
            OverlayKeyAction::TextTranslation => self.apply_action(ToolbarAction::Translate),
            OverlayKeyAction::ScrollingScreenshot => {
                self.apply_action(ToolbarAction::ScrollCapture)
            }
            OverlayKeyAction::Undo => {
                self.undo_annotation();
                OverlayOutcome::Stay
            }
            OverlayKeyAction::Redo => {
                self.redo_annotation();
                OverlayOutcome::Stay
            }
            OverlayKeyAction::CopyColor => {
                self.copy_current_color();
                OverlayOutcome::Stay
            }
            OverlayKeyAction::MoveTool if tools_ready && !selecting_mode => {
                if self.tool != AnnotationTool::None {
                    self.select_tool(self.tool);
                }
                OverlayOutcome::Stay
            }
            OverlayKeyAction::Tool(tool) if tools_ready && !selecting_mode => {
                self.apply_drawing_key(tool)
            }
            OverlayKeyAction::MoveTool | OverlayKeyAction::Tool(_) => OverlayOutcome::Stay,
            OverlayKeyAction::MoveCursor(dir) => {
                let (dx, dy) = dir.delta();
                self.cursor_pos = self.clamp_point(PhysicalPoint::new(
                    self.cursor_pos.x + dx,
                    self.cursor_pos.y + dy,
                ));
                OverlayOutcome::Stay
            }
            OverlayKeyAction::PreviousHistory => {
                self.history_previous();
                OverlayOutcome::Stay
            }
            OverlayKeyAction::NextHistory => {
                self.history_next();
                OverlayOutcome::Stay
            }
            OverlayKeyAction::SelectPreviousSelection => {
                self.select_previous_selection();
                OverlayOutcome::Stay
            }
            OverlayKeyAction::ToggleSelectionTarget => {
                self.toggle_selection_target();
                OverlayOutcome::Stay
            }
            OverlayKeyAction::MoveEntireSelection => {
                self.move_held = true;
                OverlayOutcome::Stay
            }
            OverlayKeyAction::QuickSave if selecting_mode => OverlayOutcome::Stay,
            OverlayKeyAction::QuickSave => self.save_selection_inner(None, true),
            OverlayKeyAction::Recapture => {
                self.recapture.set(true);
                OverlayOutcome::Close
            }
            OverlayKeyAction::ToggleCoordinateMode => {
                self.toggle_coordinate_mode();
                OverlayOutcome::Stay
            }
            OverlayKeyAction::QrCodeRecognition if selecting_mode => OverlayOutcome::Stay,
            OverlayKeyAction::QrCodeRecognition => self.recognize_qr_code(),
            OverlayKeyAction::Unimplemented(config_key) => {
                self.show_not_implemented(config_key);
                OverlayOutcome::Stay
            }
        }
    }

    /// 绘制工具快捷键：能映射到已有标注工具的切换，橡皮擦 / 水印尚未实现给出提示。
    ///
    /// # 参数
    /// - `key`：绘制键位。
    fn apply_drawing_key(&mut self, key: DrawingKey) -> OverlayOutcome {
        let tool = match key {
            DrawingKey::Select => AnnotationTool::Select,
            DrawingKey::Shape => AnnotationTool::Rectangle,
            DrawingKey::Arrow => AnnotationTool::Arrow,
            DrawingKey::Brush => AnnotationTool::Pencil,
            DrawingKey::Highlight => AnnotationTool::Highlighter,
            DrawingKey::Text => AnnotationTool::Text,
            DrawingKey::SerialNumber => AnnotationTool::Counter,
            DrawingKey::Filter => AnnotationTool::Mosaic,
            DrawingKey::Eraser => AnnotationTool::Eraser,
            DrawingKey::Watermark => {
                return self.not_implemented_outcome("drawing_shortcuts/watermark");
            }
        };
        // 再按同一个键不取消工具（与点工具栏不同），只在工具变化时切换
        if tool == self.tool {
            return OverlayOutcome::Stay;
        }
        if tool == AnnotationTool::None {
            self.select_tool(self.tool);
        } else {
            self.select_tool(tool);
        }
        OverlayOutcome::Stay
    }

    /// 在状态栏提示某个键位动作尚未实现。
    ///
    /// # 参数
    /// - `config_key`：键位的配置键（取设置页里的动作名）。
    fn show_not_implemented(&mut self, config_key: &str) {
        let name = crate::settings_text::item_label(
            crate::settings_text::Lang::new(&self.locale),
            config_key,
        );
        self.status_message = Some(crate::ocr_backend::i18n_for(&self.locale).tr_with(
            "overlay-key-not-implemented",
            &snow_i18n::Args::new().named("action", name),
        ));
    }

    /// 提示未实现并返回“留在覆盖窗”。
    fn not_implemented_outcome(&mut self, config_key: &str) -> OverlayOutcome {
        self.show_not_implemented(config_key);
        OverlayOutcome::Stay
    }

    /// 换上覆盖窗键位表。
    ///
    /// # 参数
    /// - `keymap`：由配置构造的键位表。
    pub fn set_keymap(&mut self, keymap: OverlayKeymap) {
        self.keymap = keymap;
    }

    /// 设置界面语言（提示文案用）。
    ///
    /// # 参数
    /// - `locale`：语言代码。
    pub fn set_locale(&mut self, locale: &str) {
        self.locale = locale.to_string();
    }

    /// 执行工具栏动作。
    ///
    /// # 参数
    /// - `action`：被点击的动作。
    ///
    /// # 返回
    /// 窗口去向。
    pub fn apply_action(&mut self, action: ToolbarAction) -> OverlayOutcome {
        match action {
            ToolbarAction::Copy => self.copy_selection_and_close(),
            ToolbarAction::Save => self.save_selection_and_close(),
            ToolbarAction::Record => self.start_recording_and_close(),
            ToolbarAction::Pin => self.pin_selection_and_close(),
            ToolbarAction::Ocr => self.start_ocr(),
            ToolbarAction::Translate => self.start_translate(),
            ToolbarAction::ScrollCapture => self.start_scroll_capture_and_close(),
            ToolbarAction::Cancel => OverlayOutcome::Close,
            ToolbarAction::Undo => {
                self.undo_annotation();
                OverlayOutcome::Stay
            }
            ToolbarAction::Redo => {
                self.redo_annotation();
                OverlayOutcome::Stay
            }
        }
    }

    /// 当前标注工具。
    pub fn current_tool(&self) -> AnnotationTool {
        self.tool
    }

    /// 标注预览分块数量（测试与探针用）。
    pub fn tile_sprite_count(&self) -> usize {
        self.tile_sprites.len()
    }

    /// 选中（或再次点击取消）标注工具。
    ///
    /// # 参数
    /// - `tool`：工具栏点击的工具；与当前相同则回到无工具状态。
    pub fn select_tool(&mut self, tool: AnnotationTool) {
        let next = if tool == self.tool {
            AnnotationTool::None
        } else {
            tool
        };
        let Some(layer) = self.annotations.as_mut() else {
            self.status_message = Some(self.i18n.tr("overlay-msg-annotation-unavailable"));
            return;
        };
        match layer.set_tool(next) {
            Ok(()) => {
                self.tool = next;
                self.status_message = None;
                self.apply_stored_style(next);
            }
            Err(e) => {
                tracing::error!(error = %e, tool = ?next, "切换标注工具失败");
                self.status_message = Some(self.i18n.tr_with(
                    "overlay-msg-switch-tool-failed",
                    &Args::new().arg(1, e.to_string()),
                ));
            }
        }
    }

    /// 注入样式持久化所用的配置存储与界面语言，并读取已保存的各工具样式。
    ///
    /// # 参数
    /// - `config`：应用共享的配置存储。
    /// - `locale`：界面语料语言代码（如 `zh-CN`）。
    ///
    /// ```ignore
    /// view.set_style_config(state.config.clone(), "zh-CN");
    /// ```
    pub fn set_style_config(&mut self, config: Rc<RefCell<ConfigStore>>, locale: &str) {
        self.styles = ToolStyleStore::load(|key| config.borrow().value(key));
        self.coordinate_global =
            config.borrow().value(COORDINATE_MODE_KEY).as_str() != Some(COORDINATE_MODE_RELATIVE);
        {
            let store = config.borrow();
            let action = |key: &str, default: ClickAction| {
                store
                    .value(key)
                    .as_str()
                    .and_then(ClickAction::parse)
                    .unwrap_or(default)
            };
            self.double_click_action = action(DOUBLE_CLICK_ACTION_KEY, ClickAction::Copy);
            self.middle_click_action = action(MIDDLE_CLICK_ACTION_KEY, ClickAction::Pin);
            self.border_color = color_setting(
                &store,
                SELECTION_BORDER_COLOR_KEY,
                (ACCENT_COLOR << 8) | 0xFF,
            );
            self.mask_color = color_setting(&store, SELECTION_MASK_COLOR_KEY, MASK_COLOR);
            self.logical_size_label =
                store.value(SELECTION_UNIT_KEY).as_str() == Some(SELECTION_UNIT_LOGICAL);
            self.color_format = store
                .value(COLOR_FORMAT_KEY)
                .as_str()
                .and_then(ColorFormat::parse)
                .unwrap_or_default();
            self.magnifier_hidden =
                store.value(COLOR_PICKER_MODE_KEY).as_str() == Some(COLOR_PICKER_ALWAYS_HIDE);
            self.resize_follow_position =
                store.value(RESIZE_MODE_KEY).as_str() == Some(RESIZE_FOLLOW_POSITION);
        }
        self.style_config = Some(config);
        self.i18n = crate::ocr_backend::i18n_for(locale);
    }

    /// 同步修饰键状态：按住 Shift 且配置把它绑给「保持宽高一致」时才锁比例。
    ///
    /// # 参数
    /// - `shift`：Shift 是否按下（取自鼠标 / 键盘事件的修饰键）。
    pub fn set_shift(&mut self, shift: bool) {
        self.keep_ratio = shift && self.keymap.shift_keeps_ratio();
    }

    /// 设置「移动整个选区」键是否按住。
    ///
    /// # 参数
    /// - `held`：是否按住。
    pub fn set_move_held(&mut self, held: bool) {
        self.move_held = held;
    }

    /// 键盘松开事件：松开「移动整个选区」键时结束平移模式。
    ///
    /// # 参数
    /// - `key` / `control` / `shift` / `alt`：松开的按键与修饰键。
    pub fn handle_key_release(&mut self, key: &str, control: bool, shift: bool, alt: bool) {
        if self.keymap.resolve(key, control, shift, alt)
            == Some(OverlayKeyAction::MoveEntireSelection)
        {
            self.move_held = false;
        }
    }

    /// 框选终点按「保持宽高一致」收敛：边长取两轴位移的较大者，方向保持。
    fn constrained_end(&self, start: PhysicalPoint, end: PhysicalPoint) -> PhysicalPoint {
        if !self.keep_ratio {
            return end;
        }
        let dx = end.x - start.x;
        let dy = end.y - start.y;
        let side = dx.abs().max(dy.abs());
        let x = start.x + if dx < 0 { -side } else { side };
        let y = start.y + if dy < 0 { -side } else { side };
        self.clamp_point(PhysicalPoint::new(x, y))
    }

    /// 调整选区时要锁定的宽高比：按住 Shift 时取原选区宽高比，否则不锁。
    fn locked_ratio(&self, origin: PhysicalRect) -> Option<f64> {
        (self.keep_ratio && origin.height > 0)
            .then(|| f64::from(origin.width) / f64::from(origin.height))
    }

    /// 「重新截图」请求标志：关闭回调读取后清零，为真时再发起一次截图。
    pub fn recapture_flag(&self) -> Rc<Cell<bool>> {
        Rc::clone(&self.recapture)
    }

    /// 切换放大镜坐标显示模式（全局 / 相对）并写回配置。
    fn toggle_coordinate_mode(&mut self) {
        self.coordinate_global = !self.coordinate_global;
        let value = if self.coordinate_global {
            COORDINATE_MODE_GLOBAL
        } else {
            COORDINATE_MODE_RELATIVE
        };
        if let Some(config) = self.config_handle() {
            let mut store = config.borrow_mut();
            if let Err(e) = store.set_value(COORDINATE_MODE_KEY, serde_json::json!(value)) {
                tracing::warn!(error = %e, "写入坐标显示模式失败");
            } else if let Err(e) = store.flush() {
                tracing::warn!(error = %e, "坐标显示模式落盘失败");
            }
        }
    }

    /// 工具当前样式。
    ///
    /// # 参数
    /// - `tool`：工具栏工具。
    pub fn tool_style(&self, tool: AnnotationTool) -> ToolStyle {
        self.styles.style(tool)
    }

    /// 把工具已记忆的样式下发给标注层（切换工具时调用；没有样式的工具忽略）。
    fn apply_stored_style(&mut self, tool: AnnotationTool) {
        if config_key(tool).is_none() {
            return;
        }
        let style = self.styles.style(tool);
        self.run_layer(|layer, base| layer.apply_style(tool, &style, base));
    }

    /// 修改工具样式：记忆、即时作用于标注层（含选中的同类对象）并写回配置。
    ///
    /// # 参数
    /// - `tool`：被修改的工具。
    /// - `edit`：对样式的修改；结果与原样式相同则什么也不做。
    ///
    /// ```ignore
    /// view.update_tool_style(AnnotationTool::Line, |s| s.width = 8);
    /// ```
    pub fn update_tool_style(&mut self, tool: AnnotationTool, edit: impl FnOnce(&mut ToolStyle)) {
        let before = self.styles.style(tool);
        let mut after = before;
        edit(&mut after);
        if !self.styles.set_style(tool, after) {
            return;
        }
        if after.color != before.color {
            self.styles.push_recent(after.color);
        }
        self.run_layer(|layer, base| layer.apply_style(tool, &after, base));
        self.persist_style(tool);
    }

    /// 把工具样式（及最近颜色）写回配置并落盘；失败只记日志，不打断标注。
    fn persist_style(&self, tool: AnnotationTool) {
        let Some(config) = &self.style_config else {
            return;
        };
        let mut store = config.borrow_mut();
        for (key, value) in self.styles.persist_entries(tool) {
            if let Err(e) = store.set_value(key, value) {
                tracing::warn!(key, error = %e, "写入标注样式配置失败");
            }
        }
        if let Err(e) = store.flush() {
            tracing::warn!(error = %e, "标注样式落盘失败");
        }
    }

    /// 构造样式下拉的选项（档位取值 + 本地化标签）。
    fn width_items(&self, presets: &[u32]) -> Vec<StyleItem> {
        presets
            .iter()
            .map(|v| StyleItem {
                value: v.to_string(),
                label: self
                    .i18n
                    .tr_with("annot-size-px", &Args::new().arg(1, v))
                    .into(),
            })
            .collect()
    }

    /// 首次显示样式面板时创建三个下拉并订阅选中事件。
    fn ensure_style_ui(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.style_ui.is_some() {
            return;
        }
        // 覆盖窗始终是深色界面，下拉组件跟随深色主题
        Theme::change(ThemeMode::Dark, None, cx);
        let arrow_items: Vec<StyleItem> = ArrowheadChoice::ALL
            .iter()
            .map(|c| StyleItem {
                value: c.id().to_string(),
                label: self.i18n.tr(c.text_id()).into(),
            })
            .collect();
        let sets = [
            (StyleSelectKind::Width, self.width_items(&WIDTH_PRESETS)),
            (StyleSelectKind::FontSize, self.width_items(&FONT_PRESETS)),
            (StyleSelectKind::Arrowhead, arrow_items),
        ];
        let mut made: Vec<Entity<StyleSelect>> = Vec::new();
        for (kind, items) in sets {
            let state = cx.new(|cx| SelectState::new(SearchableVec::new(items), None, window, cx));
            cx.subscribe_in(
                &state,
                window,
                move |this, _state, event: &SelectEvent<SearchableVec<StyleItem>>, window, cx| {
                    if let SelectEvent::Confirm(Some(value)) = event {
                        this.on_style_select(kind, value, window, cx);
                    }
                },
            )
            .detach();
            made.push(state);
        }
        let mut made = made.into_iter();
        if let (Some(width), Some(font), Some(arrowhead)) = (made.next(), made.next(), made.next())
        {
            self.style_ui = Some(StyleUi {
                width,
                font,
                arrowhead,
            });
        }
        self.sync_style_selects(window, cx);
    }

    /// 让三个下拉的选中项与当前工具的样式一致（切换工具后调用）。
    fn sync_style_selects(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(ui) = &self.style_ui else {
            return;
        };
        let style = self.styles.style(self.tool);
        let pick = |index: usize| Some(IndexPath::default().row(index));
        let width = pick(nearest_index(&WIDTH_PRESETS, style.width));
        let font = pick(nearest_index(&FONT_PRESETS, style.font_size));
        let head = ArrowheadChoice::ALL
            .iter()
            .position(|c| *c == style.arrowhead);
        ui.width
            .update(cx, |s, cx| s.set_selected_index(width, window, cx));
        ui.font
            .update(cx, |s, cx| s.set_selected_index(font, window, cx));
        ui.arrowhead.update(cx, |s, cx| {
            s.set_selected_index(head.and_then(pick), window, cx)
        });
    }

    /// 样式下拉选中：先提交进行中的文字输入，再改当前工具样式。
    fn on_style_select(
        &mut self,
        kind: StyleSelectKind,
        value: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.commit_text_edit(window, cx);
        let tool = self.tool;
        match kind {
            StyleSelectKind::Width => {
                if let Ok(v) = value.parse::<u32>() {
                    self.update_tool_style(tool, |s| s.width = v);
                }
            }
            StyleSelectKind::FontSize => {
                if let Ok(v) = value.parse::<u32>() {
                    self.update_tool_style(tool, |s| s.font_size = v);
                }
            }
            StyleSelectKind::Arrowhead => {
                if let Some(head) = ArrowheadChoice::from_id(value) {
                    self.update_tool_style(tool, |s| s.arrowhead = head);
                }
            }
        }
        cx.notify();
    }

    /// 色块被点击。
    fn on_style_color(&mut self, color: Rgba, window: &mut Window, cx: &mut Context<Self>) {
        self.commit_text_edit(window, cx);
        let tool = self.tool;
        self.update_tool_style(tool, |s| s.color = color);
        cx.notify();
    }

    /// 填充开关被点击。
    fn on_style_fill(&mut self, fill: bool, window: &mut Window, cx: &mut Context<Self>) {
        self.commit_text_edit(window, cx);
        let tool = self.tool;
        self.update_tool_style(tool, |s| s.fill = fill);
        cx.notify();
    }

    /// 一排可点击的色块；当前色带高亮边框。
    fn swatch_row(
        &self,
        id: &'static str,
        colors: &[Rgba],
        current: Rgba,
        cx: &mut Context<Self>,
    ) -> Div {
        let mut row = div().flex().flex_row().items_center().gap_1();
        for (index, color) in colors.iter().copied().enumerate() {
            let picked = color == current;
            row = row.child(
                div()
                    .id(SharedString::from(format!("{id}-{index}")))
                    .size(px(SWATCH_SIZE))
                    .rounded_sm()
                    .cursor_pointer()
                    .border_1()
                    .border_color(if picked {
                        rgb(0xFFFFFF)
                    } else {
                        rgba(0xFFFFFF40)
                    })
                    .bg(rgba(u32::from_be_bytes(color)))
                    .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                        this.on_style_color(color, window, cx);
                    })),
            );
        }
        row
    }

    /// 选区形状栏：四种形状单选；选区确定后再多出「添加 / 减去区域」两个按钮。
    ///
    /// # 参数
    /// - `toolbar_pos`：主工具栏位置（逻辑像素）；有则把形状栏贴在它上方。
    /// - `monitor`：本窗口所在显示器在画布里的逻辑矩形 `(x, y, 宽, 高)`；选区阶段形状栏顶部居中于它。
    ///
    /// # 返回
    /// 形状栏元素；录屏 / 长截图 / 标注 / 文字输入 / 翻译或识别界面期间不显示。
    fn render_region_bar(
        &self,
        toolbar_pos: Option<PhysicalPoint>,
        monitor: (f32, f32, f32, f32),
        cx: &mut Context<Self>,
    ) -> Option<Div> {
        if self.record_mode
            || self.scroll_mode
            || self.annotating
            || self.tool != AnnotationTool::None
            || self.text_edit.is_some()
            || self.ocr.is_visible()
            || self.translate.is_visible()
            || self
                .history_host
                .as_ref()
                .is_some_and(|h| h.nav.in_history())
        {
            return None;
        }
        let selected = matches!(self.state, SelectionState::Selected { .. });
        let i18n = self.i18n;
        let button = |label: String, active: bool| {
            div()
                .px_2()
                .py_0p5()
                .rounded_xs()
                .text_xs()
                .cursor(CursorStyle::PointingHand)
                .text_color(rgba(0xFFFFFFFF))
                .bg(if active {
                    rgb(ACCENT_COLOR)
                } else {
                    rgb(0x2B2B2B)
                })
                .child(label)
        };
        let mut row = div()
            .absolute()
            .flex()
            .flex_row()
            .gap_1()
            .p_1()
            .rounded_sm()
            .bg(rgba(0x000000CC))
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .on_mouse_down(MouseButton::Right, |_, _, cx| cx.stop_propagation());
        if selected {
            for (op, id) in [
                (
                    RegionOp::Add,
                    "screenshot-tool-palette-add-screenshot-region-4f7ae0d9",
                ),
                (
                    RegionOp::Subtract,
                    "screenshot-tool-palette-subtract-screenshot-region-c0df476a",
                ),
            ] {
                row = row.child(button(i18n.tr(id).to_string(), false).on_mouse_down(
                    MouseButton::Left,
                    cx.listener(move |this, _: &MouseDownEvent, _window, cx| {
                        this.begin_region_op(op);
                        cx.stop_propagation();
                        cx.notify();
                    }),
                ));
            }
        }
        for (region_type, id) in [
            (
                RegionType::Rectangle,
                "screenshot-tool-palette-rectangle-region-86f2b03d",
            ),
            (
                RegionType::Polyline,
                "screenshot-tool-palette-polyline-region-4f71de1a",
            ),
            (
                RegionType::Curve,
                "screenshot-tool-palette-curve-region-7a0abb9b",
            ),
            (
                RegionType::Freehand,
                "screenshot-tool-palette-freehand-region-c5a700ff",
            ),
        ] {
            row = row.child(
                button(i18n.tr(id).to_string(), self.region_type == region_type).on_mouse_down(
                    MouseButton::Left,
                    cx.listener(move |this, _: &MouseDownEvent, _window, cx| {
                        this.switch_region_type(region_type);
                        cx.stop_propagation();
                        cx.notify();
                    }),
                ),
            );
        }
        let (top, left) = match toolbar_pos {
            Some(pos) if selected => (
                (pos.y as f32 - REGION_BAR_HEIGHT - 4.0).max(4.0),
                pos.x as f32,
            ),
            _ => (
                monitor.1 + 8.0,
                monitor.0 + (monitor.2 / 2.0 - REGION_BAR_HALF_WIDTH).max(4.0),
            ),
        };
        Some(row.top(px(top)).left(px(left)))
    }

    /// 样式面板：按当前工具展示颜色 / 线宽 / 字号 / 填充 / 箭头头型，点击不穿透到选区。
    ///
    /// # 参数
    /// - `origin`：面板左上角（逻辑像素）。
    fn render_style_panel(&self, origin: (i32, i32), cx: &mut Context<Self>) -> Div {
        let tool = self.tool;
        let fields = style_fields(tool);
        let style = self.styles.style(tool);
        let i18n = self.i18n;
        let label = move |id: &str| {
            div()
                .text_xs()
                .text_color(rgba(STYLE_LABEL_COLOR))
                .child(i18n.tr(id))
        };
        let select = |state: &Entity<StyleSelect>| {
            div()
                .w(px(STYLE_SELECT_WIDTH))
                .h(px(STYLE_SELECT_HEIGHT))
                .child(
                    Select::new(state)
                        .with_size(ComponentSize::Small)
                        .menu_max_h(px(STYLE_SELECT_MENU_MAX_HEIGHT)),
                )
        };
        let mut panel = div()
            .absolute()
            .top(px(origin.1 as f32))
            .left(px(origin.0 as f32))
            .w(px(STYLE_PANEL_SIZE.0 as f32))
            .flex()
            .flex_row()
            .flex_wrap()
            .items_center()
            .gap_2()
            .px_3()
            .py_2()
            .rounded_md()
            .bg(rgba(STYLE_PANEL_BG))
            .shadow_lg()
            .border_1()
            .border_color(rgba(STYLE_PANEL_BORDER))
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .on_mouse_down(MouseButton::Right, |_, _, cx| cx.stop_propagation());
        if fields.color {
            panel = panel
                .child(label("annot-style-color"))
                .child(self.swatch_row("style-color", &PALETTE, style.color, cx));
            if !self.styles.recent().is_empty() {
                let recent = self.styles.recent().to_vec();
                panel = panel
                    .child(label("annot-style-recent"))
                    .child(self.swatch_row("style-recent", &recent, style.color, cx));
            }
        }
        if let Some(ui) = &self.style_ui {
            if fields.width {
                panel = panel
                    .child(label("annot-style-width"))
                    .child(select(&ui.width));
            }
            if fields.font_size {
                panel = panel
                    .child(label("annot-style-font-size"))
                    .child(select(&ui.font));
            }
            if fields.arrowhead {
                panel = panel
                    .child(label("annot-style-arrowhead"))
                    .child(select(&ui.arrowhead));
            }
        }
        if fields.fill {
            let entity = cx.entity();
            panel = panel.child(
                Checkbox::new("style-fill")
                    .label(SharedString::from(i18n.tr("annot-style-fill")))
                    .checked(style.fill)
                    .on_click(move |checked, window, app| {
                        let checked = *checked;
                        entity.update(app, |this, cx| this.on_style_fill(checked, window, cx));
                    }),
            );
        }
        panel
    }

    /// 样式面板的左上角；当前工具没有样式或处于录屏 / 长图模式时不显示。
    ///
    /// # 参数
    /// - `toolbar`：工具栏左上角（逻辑像素）。
    /// - `screen`：屏幕逻辑尺寸。
    fn style_panel_origin(&self, toolbar: (i32, i32), screen: (i32, i32)) -> Option<(i32, i32)> {
        if style_fields(self.tool).is_empty() || self.record_mode || self.scroll_mode {
            return None;
        }
        Some(panel_placement(
            toolbar,
            TOOLBAR_LOGICAL_SIZE.1,
            STYLE_PANEL_SIZE,
            screen,
            STYLE_PANEL_GAP,
        ))
    }

    /// 把选区限制后的点夹进选区矩形，返回浮点画布坐标。
    fn clamp_to_selection(point: PhysicalPoint, rect: PhysicalRect) -> (f64, f64) {
        let x = point.x.clamp(rect.x, (rect.right() - 1).max(rect.x));
        let y = point.y.clamp(rect.y, (rect.bottom() - 1).max(rect.y));
        (f64::from(x), f64::from(y))
    }

    /// 在标注层上执行一次操作并把增量更新装进预览分块。
    fn run_layer(
        &mut self,
        op: impl FnOnce(
            &mut AnnotationLayer,
            crate::annotation::BaseView<'_>,
        ) -> Result<LayerUpdate, String>,
    ) {
        let Some(layer) = self.annotations.as_mut() else {
            return;
        };
        let started = Instant::now();
        let result = op(layer, self.frame.base_view());
        let compute = started.elapsed();
        match result {
            Ok(update) => self.install_update(update, compute),
            Err(e) => {
                tracing::error!(error = %e, "标注层操作失败");
                self.status_message = Some(self.i18n.tr_with(
                    "overlay-msg-annotate-failed",
                    &Args::new().arg(1, e.to_string()),
                ));
            }
        }
    }

    /// 把预览增量装进分块表：替换变化块、释放空块，并记录探针。
    ///
    /// # 参数
    /// - `update`：标注层输出的增量。
    /// - `compute`：产生该增量的耗时（引擎 + 光栅化 + 合成）。
    fn install_update(&mut self, update: LayerUpdate, compute: std::time::Duration) {
        let started = Instant::now();
        let tiles = update.tiles.len();
        let mut bytes = 0;
        for tile in update.tiles {
            bytes += tile.bgra.len();
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
        if tiles > 0 || compute.as_micros() > 0 {
            self.probe
                .record_annotation(compute, started.elapsed(), tiles, bytes);
        }
    }

    /// 在选区内按下：开始一次标注拖动。
    fn start_annotation(&mut self, point: PhysicalPoint, selection: PhysicalRect) {
        let (x, y) = Self::clamp_to_selection(point, selection);
        self.run_layer(|layer, base| layer.pointer_down(x, y, base));
        self.annotating = self.annotations.as_ref().is_some_and(|l| l.is_drawing());
    }

    /// 标注拖动中：记录最新位置，实际更新推迟到下一次渲染前统一提交。
    fn continue_annotation(&mut self, point: PhysicalPoint) {
        self.pending_annotation_point = Some(point);
    }

    /// 把拖动中积压的最新指针位置提交给标注层（每帧渲染前调用一次；无积压时什么也不做）。
    pub fn flush_pending_annotation(&mut self) {
        let Some(point) = self.pending_annotation_point.take() else {
            return;
        };
        let Some(rect) = self.current_selection() else {
            return;
        };
        let (x, y) = Self::clamp_to_selection(point, rect);
        self.run_layer(|layer, base| layer.pointer_move(x, y, base));
    }

    /// 标注拖动结束：提交元素（松开位置已包含最终形状，积压的中间位置直接丢弃）。
    fn finish_annotation(&mut self, point: PhysicalPoint) {
        self.annotating = false;
        self.pending_annotation_point = None;
        let Some(rect) = self.current_selection() else {
            return;
        };
        let (x, y) = Self::clamp_to_selection(point, rect);
        self.run_layer(|layer, base| layer.pointer_up(x, y, base));
    }

    /// 撤销上一个标注。
    pub fn undo_annotation(&mut self) {
        self.run_layer(|layer, base| layer.undo(base));
    }

    /// 重做上一个被撤销的标注。
    pub fn redo_annotation(&mut self) {
        self.run_layer(|layer, base| layer.redo(base));
    }

    /// 撤销 / 重做是否可用。
    fn history_state(&self) -> (bool, bool) {
        self.annotations
            .as_ref()
            .map_or((false, false), |l| (l.can_undo(), l.can_redo()))
    }

    /// 清空全部标注（重新框选 / 取消选区时调用），并回到无工具状态。
    fn reset_annotations(&mut self) {
        self.annotating = false;
        self.pending_annotation_point = None;
        self.tool = AnnotationTool::None;
        self.text_edit = None;
        for (_, sprite) in self.tile_sprites.drain() {
            self.pending_drops.push(sprite.image);
        }
        let (w, h) = self.frame.size();
        if self.annotations.is_some() {
            self.annotations = AnnotationLayer::new(w, h, self.scale).ok();
        }
    }

    /// 取选区对应的 RGBA 图像（含标注合成）；没有选区或选区在图外返回 `None`。
    fn selection_image(&mut self) -> Option<(u32, u32, Vec<u8>)> {
        let rect = self.current_selection()?;
        let (w, h, mut rgba) = match self.annotations.as_mut() {
            Some(layer) => layer.export_rgba(
                [rect.x, rect.y, rect.right(), rect.bottom()],
                self.frame.base_view(),
            ),
            None => self.frame.crop_rgba(rect),
        }?;
        // 自定义区域：区域外变透明
        if let Some(mask) = &self.region_mask {
            mask.apply_to_rgba((rect.x, rect.y, w as i32, h as i32), &mut rgba);
        }
        Some((w, h, rgba))
    }

    /// 设置画布左上角在虚拟桌面里的坐标。
    ///
    /// # 参数
    /// - `origin`：虚拟桌面外接矩形的左上角（桌面物理坐标，可为负）。
    pub fn set_canvas_origin(&mut self, origin: PhysicalPoint) {
        self.canvas_origin = origin;
    }

    /// 接入关闭钩子：视图关闭（任一窗口关闭）时调用，运行时据此把同一会话的其余窗口一起关掉。
    ///
    /// # 参数
    /// - `on_close`：只应做投递。
    ///
    /// ```ignore
    /// view.set_close_hook(|| inbox.push(UiEvent::OverlayClosed));
    /// ```
    pub fn set_close_hook(&mut self, on_close: impl Fn() + 'static) {
        self.close_hook = Some(Box::new(on_close));
    }

    /// 当前选区形状类型。
    pub fn region_type(&self) -> RegionType {
        self.region_type
    }

    /// 设置启动时的选区形状（来自配置，不回写）。
    ///
    /// # 参数
    /// - `region_type`：形状类型。
    pub fn set_initial_region_type(&mut self, region_type: RegionType) {
        self.region_type = region_type;
    }

    /// 是否处于自定义区域输入态：选区形状不是矩形，且还没有选区。
    fn custom_input_active(&self) -> bool {
        self.region_type != RegionType::Rectangle && self.state == SelectionState::Idle
    }

    /// 清掉自定义区域的全部状态（草稿、蒙版、遮罩图）。
    fn clear_region(&mut self) {
        self.region_draft = None;
        self.region_mask = None;
        if let Some(image) = self.region_overlay.take() {
            self.pending_drops.push(image);
        }
    }

    /// 循环切换选区形状（Ctrl+Tab / Ctrl+Shift+Tab）。
    ///
    /// # 参数
    /// - `reverse`：为 `true` 时往回循环。
    ///
    /// # 返回
    /// 是否切换；标注 / 拖动 / 文字输入中为 `false`。
    ///
    /// ```ignore
    /// view.cycle_region_type(false);
    /// ```
    pub fn cycle_region_type(&mut self, reverse: bool) -> bool {
        self.switch_region_type(self.region_type.cycled(reverse))
    }

    /// 切换选区形状：丢掉当前草稿；没有进行加 / 减区域时还会丢掉当前选区，回到选区阶段。
    ///
    /// # 参数
    /// - `region_type`：目标形状。
    ///
    /// # 返回
    /// 是否发生了切换；形状没变、或标注 / 拖动 / 文字输入中为 `false`。
    ///
    /// ```ignore
    /// view.switch_region_type(RegionType::Polyline);
    /// ```
    pub fn switch_region_type(&mut self, region_type: RegionType) -> bool {
        if region_type == self.region_type
            || self.annotating
            || self.text_edit.is_some()
            || !matches!(
                self.state,
                SelectionState::Idle | SelectionState::Selected { .. }
            )
        {
            return false;
        }
        self.region_type = region_type;
        self.persist_region_type();
        self.region_draft = None;
        self.pick.clear();
        self.click_window = None;
        if self.region_op.is_none() {
            self.clear_region();
            self.reset_annotations();
            self.state = SelectionState::Idle;
            self.hover_mode = SelectionDragMode::None;
        }
        self.refresh_window_hover(self.cursor_pos);
        true
    }

    /// 开始「加区域」或「减区域」：当前选区保留为底，接下来画的形状并入 / 挖出它。
    ///
    /// # 参数
    /// - `op`：并入还是挖出。
    ///
    /// # 返回
    /// 是否开始；没有选区、正在标注 / 输入文字、录屏 / 长截图模式下为 `false`。
    ///
    /// ```ignore
    /// view.begin_region_op(RegionOp::Subtract);
    /// ```
    pub fn begin_region_op(&mut self, op: RegionOp) -> bool {
        if self.annotating || self.text_edit.is_some() || self.record_mode || self.scroll_mode {
            return false;
        }
        let SelectionState::Selected { rect } = self.state else {
            return false;
        };
        // 普通矩形选区先转成蒙版，后面统一按蒙版合成
        let plain_rect = if self.region_mask.is_none() {
            let (width, height) = self.frame.size();
            self.region_mask = Some(RegionMask::from_rect(
                width,
                height,
                (rect.x, rect.y, rect.width, rect.height),
            ));
            self.rebuild_region_overlay();
            Some(rect)
        } else {
            None
        };
        self.region_op = Some(RegionOpState { op, plain_rect });
        self.region_draft = None;
        self.state = SelectionState::Idle;
        self.hover_mode = SelectionDragMode::None;
        self.pick.clear();
        self.click_window = None;
        self.refresh_window_hover(self.cursor_pos);
        true
    }

    /// 取消进行中的加 / 减区域，选区回到操作之前的样子。
    ///
    /// # 返回
    /// 是否取消了；没有进行中的操作为 `false`。
    fn cancel_region_op(&mut self) -> bool {
        let Some(op) = self.region_op.take() else {
            return false;
        };
        self.region_draft = None;
        match op.plain_rect {
            Some(rect) => {
                self.clear_region();
                self.state = SelectionState::Selected { rect };
            }
            None => {
                if let Some((x, y, w, h)) = self.region_mask.as_ref().and_then(RegionMask::bounds) {
                    self.state = SelectionState::Selected {
                        rect: PhysicalRect::new(x, y, w, h),
                    };
                }
            }
        }
        true
    }

    /// 把一个操作数形状并入 / 挖出当前蒙版并确认为新选区。
    ///
    /// # 参数
    /// - `commands`：操作数的路径命令。
    ///
    /// # 返回
    /// 是否合成成功；结果为空或太小时丢弃操作数，操作仍保持进行。
    fn merge_region_operand(&mut self, commands: &[PathCommand]) -> bool {
        let (Some(op), Some(base)) = (
            self.region_op.as_ref().map(|o| o.op),
            self.region_mask.as_ref(),
        ) else {
            return false;
        };
        let mut merged = base.clone();
        merged.apply(commands, op);
        let Some((x, y, w, h)) = merged.bounds() else {
            return false;
        };
        if w < DEFAULT_MINIMUM_SELECTION_SIZE || h < DEFAULT_MINIMUM_SELECTION_SIZE {
            return false;
        }
        self.region_mask = Some(merged);
        self.region_op = None;
        self.rebuild_region_overlay();
        self.state = SelectionState::Selected {
            rect: PhysicalRect::new(x, y, w, h),
        };
        true
    }

    /// 用矩形当操作数（矩形形状下的加 / 减区域，来自框选或点选窗口）。
    fn merge_rect_operand(&mut self, rect: PhysicalRect) -> bool {
        let (x0, y0) = (rect.x as f32, rect.y as f32);
        let (x1, y1) = (rect.right() as f32, rect.bottom() as f32);
        let corners = [(x0, y0), (x1, y0), (x1, y1), (x0, y1)];
        self.merge_region_operand(&shape_commands(RegionShape::Polyline, &corners))
    }

    /// 把选区形状写回配置并落盘；失败只记日志。
    fn persist_region_type(&self) {
        let Some(config) = self.config_handle() else {
            return;
        };
        let mut store = config.borrow_mut();
        if let Err(e) = store.set_value(
            REGION_TYPE_KEY,
            serde_json::json!(self.region_type.as_config()),
        ) {
            tracing::warn!(error = %e, "写入选区形状配置失败");
            return;
        }
        if let Err(e) = store.flush() {
            tracing::warn!(error = %e, "选区形状落盘失败");
        }
    }

    /// 自定义区域输入时按下鼠标：开始 / 继续草稿；双击折线 / 曲线则闭合完成。
    fn begin_region_input(&mut self, point: PhysicalPoint, click_count: usize) {
        let Some(shape) = self.region_type.shape() else {
            return;
        };
        let p = (point.x as f32, point.y as f32);
        self.pick.clear();
        let draft = self
            .region_draft
            .get_or_insert_with(|| RegionDraft::new(shape));
        if click_count >= DOUBLE_CLICK_COUNT {
            if draft.double_click(p) {
                self.commit_region_draft();
            }
            return;
        }
        draft.press(p);
    }

    /// 完成草稿：栅格化成蒙版并确认为选区；点数不足 / 没有面积 / 太小则丢弃草稿。
    ///
    /// # 返回
    /// 是否成功得到选区。
    fn commit_region_draft(&mut self) -> bool {
        let Some(draft) = self.region_draft.take() else {
            return false;
        };
        let Some(vertices) = draft.finish(self.scale) else {
            return false;
        };
        if self.region_op.is_some() {
            return self.merge_region_operand(&shape_commands(draft.shape(), &vertices));
        }
        let (width, height) = self.frame.size();
        let mut mask = RegionMask::new(width, height);
        mask.apply(&shape_commands(draft.shape(), &vertices), RegionOp::Add);
        let Some((x, y, w, h)) = mask.bounds() else {
            return false;
        };
        if w < DEFAULT_MINIMUM_SELECTION_SIZE || h < DEFAULT_MINIMUM_SELECTION_SIZE {
            return false;
        }
        self.region_mask = Some(mask);
        self.rebuild_region_overlay();
        self.state = SelectionState::Selected {
            rect: PhysicalRect::new(x, y, w, h),
        };
        true
    }

    /// 完成（Enter）当前草稿；失败就清掉草稿。
    fn finish_region_draft(&mut self) {
        if !self.commit_region_draft() {
            self.region_draft = None;
        }
    }

    /// 按当前蒙版重建遮罩图（外接矩形范围内：区域外压暗、区域边缘描一圈主题色）。
    fn rebuild_region_overlay(&mut self) {
        if let Some(old) = self.region_overlay.take() {
            self.pending_drops.push(old);
        }
        let Some(mask) = &self.region_mask else {
            return;
        };
        let Some((x, y, w, h)) = mask.bounds() else {
            return;
        };
        let edge_width = self.scale.round().max(1.0) as i32;
        let accent = ACCENT_COLOR.to_be_bytes();
        let bgra = mask.overlay_bgra_in(
            (x, y, w, h),
            (self.mask_color & 0xFF) as u8,
            Some((accent[3], accent[2], accent[1], edge_width)),
        );
        if let Some(buffer) = RgbaImage::from_raw(w as u32, h as u32, bgra) {
            self.region_overlay = Some(Arc::new(RenderImage::new(vec![Frame::new(buffer)])));
        }
    }

    /// 选区内某点对应的拖动模式：自定义区域只能整体移动（框内为移动，框外为无）。
    fn drag_mode_for(&self, rect: PhysicalRect, point: PhysicalPoint) -> SelectionDragMode {
        if self.region_mask.is_some() {
            return if rect.contains(point) {
                SelectionDragMode::All
            } else {
                SelectionDragMode::None
            };
        }
        hit_test_drag_mode(
            rect,
            point,
            false,
            self.edge_tolerance(),
            DEFAULT_MINIMUM_SELECTION_SIZE,
        )
    }

    /// 接入截图历史翻页的数据来源。
    ///
    /// # 参数
    /// - `provider`：记录列表与异步读取。
    ///
    /// ```ignore
    /// view.set_history_provider(Box::new(ThreadedHistoryProvider::start(&root, policy)?));
    /// ```
    pub fn set_history_provider(&mut self, provider: Box<dyn HistoryProvider>) {
        self.history_host = Some(HistoryHost {
            provider,
            nav: HistoryNav::new(),
            live: None,
        });
    }

    /// 是否允许翻页：已接入来源、没有拖动 / 标注 / 文字输入，且在空闲或已选中状态。
    fn can_navigate_history(&self) -> bool {
        self.history_host.is_some()
            && !self.annotating
            && self.text_edit.is_none()
            && matches!(
                self.state,
                SelectionState::Idle | SelectionState::Selected { .. }
            )
    }

    /// 翻到更旧的一条截图历史。
    ///
    /// # 返回
    /// 是否发起了读取；没有更旧的记录、正在读取或当前不允许翻页时为 `false`。
    ///
    /// ```ignore
    /// view.history_previous();
    /// ```
    pub fn history_previous(&mut self) -> bool {
        self.history_step(HistoryNav::older)
    }

    /// 翻到更新的一条截图历史；翻到头就回到当前截图。
    ///
    /// # 返回
    /// 是否发生了切换或读取。
    pub fn history_next(&mut self) -> bool {
        self.history_step(HistoryNav::newer)
    }

    /// 回到当前截图（右键退出历史）。
    ///
    /// # 返回
    /// 是否确实从历史记录切回了当前截图。
    pub fn history_return_to_live(&mut self) -> bool {
        self.history_step(HistoryNav::return_to_live)
    }

    /// 刷新记录列表后执行一步导航，并落实结果。
    fn history_step(&mut self, step: impl FnOnce(&mut HistoryNav) -> NavStep) -> bool {
        if !self.can_navigate_history() {
            return false;
        }
        let Some(host) = self.history_host.as_mut() else {
            return false;
        };
        let ids = host.provider.ids();
        host.nav.set_ids(ids);
        match step(&mut host.nav) {
            NavStep::None => false,
            NavStep::Load { id } => {
                host.provider.begin_load(&id);
                true
            }
            NavStep::ShowLive => self.restore_live_endpoint(),
        }
    }

    /// 是否有历史读取在进行（渲染循环据此继续请求下一帧）。
    fn history_busy(&self) -> bool {
        self.history_host.as_ref().is_some_and(|h| h.nav.busy())
    }

    /// 轮询历史读取结果；读完且适用就应用到视图。
    ///
    /// # 返回
    /// 视图内容是否发生了变化（需要重绘）。
    pub fn poll_history(&mut self) -> bool {
        let Some((id, entry)) = self
            .history_host
            .as_mut()
            .and_then(|h| h.provider.poll_loaded())
        else {
            return false;
        };
        let prepared = entry.and_then(|e| self.prepare_entry(e));
        let outcome = if prepared.is_some() {
            LoadOutcome::Loaded
        } else {
            LoadOutcome::Failed
        };
        let Some(host) = self.history_host.as_mut() else {
            return false;
        };
        match (host.nav.finish(&id, outcome), prepared) {
            (FinishStep::Apply, Some(prepared)) => {
                self.install_history_entry(prepared);
                true
            }
            _ => false,
        }
    }

    /// 把读出的现场换成可显示的内容；底图尺寸与当前不同（换过显示器 / 分辨率）时不适用。
    fn prepare_entry(&self, entry: LoadedEntry) -> Option<PreparedEntry> {
        if (entry.frame_width, entry.frame_height) != self.frame.size() {
            return None;
        }
        // 仓储里的 RGBA 换成 GPUI 约定的 BGRA
        let mut data = entry.frame_rgba;
        for pixel in data.chunks_exact_mut(4) {
            pixel.swap(0, 2);
        }
        // 历史里是整张画布；按当前各显示器的矩形切回每屏一帧
        let rects: Vec<PhysicalRect> = (0..self.frame.count())
            .map(|i| self.frame.rect(i))
            .collect();
        let frame =
            DesktopFrames::from_canvas_bgra(entry.frame_width, entry.frame_height, data, &rects)
                .ok()?;
        let (w, h) = frame.size();
        let annotations = match &entry.canvas_history[..] {
            [] | b"{}" => AnnotationLayer::new(w, h, self.scale).ok(),
            bytes => AnnotationLayer::from_history(w, h, self.scale, bytes)
                .inspect_err(|e| tracing::warn!(error = %e, "恢复历史标注失败"))
                .ok(),
        };
        let (x, y, sw, sh) = entry.selection;
        let selection = self
            .screen_bounds
            .intersect(&PhysicalRect::new(x, y, sw, sh))?;
        Some(PreparedEntry {
            frame,
            annotations,
            selection,
        })
    }

    /// 清掉与旧底图 / 标注绑定的瞬态：标注拖动、文字输入、悬停层级与分块图。
    fn clear_view_transients(&mut self) {
        self.annotating = false;
        self.pending_annotation_point = None;
        self.tool = AnnotationTool::None;
        self.text_edit = None;
        self.pick.clear();
        self.click_window = None;
        for (_, sprite) in self.tile_sprites.drain() {
            self.pending_drops.push(sprite.image);
        }
    }

    /// 应用一条历史现场；第一次离开当前截图时把它暂存起来。
    fn install_history_entry(&mut self, prepared: PreparedEntry) {
        self.clear_view_transients();
        let old_frame = std::mem::replace(&mut self.frame, prepared.frame);
        let old_layer = std::mem::replace(&mut self.annotations, prepared.annotations);
        let old_state = self.state;
        let old_region = self.region_mask.take();
        if let Some(image) = self.region_overlay.take() {
            self.pending_drops.push(image);
        }
        self.region_draft = None;
        if let Some(host) = self.history_host.as_mut() {
            if host.live.is_none() {
                host.live = Some(LiveEndpoint {
                    frame: old_frame,
                    annotations: old_layer,
                    state: old_state,
                    region_mask: old_region,
                });
            } else {
                // 历史记录之间切换：上一条历史的图像不再需要
                self.pending_drops.extend(old_frame.images());
            }
        }
        self.state = SelectionState::Selected {
            rect: prepared.selection,
        };
        self.refresh_annotation_tiles();
    }

    /// 切回暂存的当前截图。
    ///
    /// # 返回
    /// 是否确实切回；没有暂存时为 `false`。
    fn restore_live_endpoint(&mut self) -> bool {
        let Some(live) = self.history_host.as_mut().and_then(|h| h.live.take()) else {
            return false;
        };
        self.clear_view_transients();
        let history_frame = std::mem::replace(&mut self.frame, live.frame);
        self.pending_drops.extend(history_frame.images());
        self.annotations = live.annotations;
        self.state = live.state;
        self.clear_region();
        self.region_mask = live.region_mask;
        self.rebuild_region_overlay();
        self.refresh_annotation_tiles();
        true
    }

    /// 让标注层重新输出全部预览块（恢复标注层之后调用）。
    fn refresh_annotation_tiles(&mut self) {
        self.run_layer(|layer, base| layer.refresh(base));
    }

    /// 接入截图历史出口：每次导出成功后收到整帧底图、选区、标注历史与结果图。
    ///
    /// # 参数
    /// - `sink`：接收来源与完整现场；应只做投递（写盘在别的线程）。
    ///
    /// ```ignore
    /// view.set_history_sink(|source, snapshot| recorder.submit_snapshot(policy(), source, snapshot));
    /// ```
    pub fn set_history_sink(&mut self, sink: impl Fn(HistorySource, HistorySnapshot) + 'static) {
        self.history_sink = Some(Box::new(sink));
    }

    /// 为一次导出准备历史现场；未接入历史出口时返回 `None`（不拷贝整帧）。
    ///
    /// # 参数
    /// - `width` / `height` / `rgba`：导出结果图。
    fn history_snapshot(&self, width: u32, height: u32, rgba: &[u8]) -> Option<HistorySnapshot> {
        self.history_sink.as_ref()?;
        let selection = self.current_selection()?;
        let (frame_width, frame_height, frame_rgba) = self.frame.crop_rgba(self.frame.bounds())?;
        let canvas_history = self
            .annotations
            .as_ref()
            .filter(|layer| layer.item_count() > 0 || layer.can_undo() || layer.can_redo())
            .and_then(|layer| layer.serialize_history().ok())
            .unwrap_or_default();
        Some(HistorySnapshot {
            frame_width,
            frame_height,
            frame_rgba,
            selection: (selection.x, selection.y, selection.width, selection.height),
            canvas_history,
            result_width: width,
            result_height: height,
            result_rgba: rgba.to_vec(),
        })
    }

    /// 把现场交给历史出口。
    fn record_history(&self, source: HistorySource, snapshot: Option<HistorySnapshot>) {
        if let (Some(sink), Some(snapshot)) = (&self.history_sink, snapshot) {
            sink(source, snapshot);
        }
    }

    /// 取视图持有的共享配置（`set_style_config` 注入）。
    fn config_handle(&self) -> Option<&SharedConfig> {
        self.style_config.as_ref()
    }

    /// 导出时记下当前选区，供「选择上一次选区」使用。
    fn remember_selection(&self) {
        if let Some(rect) = self.current_selection() {
            self.persist_previous_selection(rect);
        }
    }

    /// 把选区写入 `previous_selection` 并落盘；失败只记日志。
    fn persist_previous_selection(&self, rect: PhysicalRect) {
        let Some(config) = self.config_handle() else {
            return;
        };
        let mut store = config.borrow_mut();
        let doc = store.document();
        let int = |key: &str| {
            doc.value(key)
                .as_i64()
                .and_then(|n| i32::try_from(n).ok())
                .unwrap_or(0)
        };
        let style = SelectionStyle {
            corner_radius: int("screenshot_selection/corner_radius"),
            shadow_width: int("screenshot_selection/shadow_width"),
            lock_aspect_ratio: doc
                .value("screenshot_selection/lock_aspect_ratio")
                .as_bool()
                .unwrap_or(false),
        };
        let desktop = rect.translate(self.canvas_origin.x, self.canvas_origin.y);
        let value = encode(desktop, self.scale, style);
        if let Err(e) = store.set_value(PREVIOUS_SELECTION_KEY, value) {
            tracing::warn!(error = %e, "写入上一次选区失败");
            return;
        }
        if let Err(e) = store.flush() {
            tracing::warn!(error = %e, "上一次选区落盘失败");
        }
    }

    /// 选中上一次导出时保存的选区（空闲或已有选区时可用，替换当前选区）。
    ///
    /// # 返回
    /// 是否成功选中；没有保存的选区、裁剪后太小或正在标注时为 `false`。
    ///
    /// ```ignore
    /// view.select_previous_selection();
    /// ```
    pub fn select_previous_selection(&mut self) -> bool {
        if self.annotating
            || !matches!(
                self.state,
                SelectionState::Idle | SelectionState::Selected { .. }
            )
        {
            return false;
        }
        let Some(config) = self.config_handle() else {
            return false;
        };
        let value = config.borrow().document().value(PREVIOUS_SELECTION_KEY);
        let desktop_bounds = self
            .screen_bounds
            .translate(self.canvas_origin.x, self.canvas_origin.y);
        let Some(rect) = decode(
            &value,
            self.scale,
            desktop_bounds,
            DEFAULT_MINIMUM_SELECTION_SIZE,
        )
        .map(|r| r.translate(-self.canvas_origin.x, -self.canvas_origin.y)) else {
            return false;
        };
        self.pick.clear();
        self.click_window = None;
        self.clear_region();
        self.state = SelectionState::Selected { rect };
        true
    }

    /// 复制选区到剪贴板；成功后关闭覆盖窗，失败保留窗口并提示。
    fn copy_selection_and_close(&mut self) -> OverlayOutcome {
        if !self.has_committed_selection() {
            self.status_message = Some(self.i18n.tr("overlay-msg-select-area-first"));
            return OverlayOutcome::Stay;
        }
        let Some((w, h, rgba)) = self.selection_image() else {
            self.status_message = Some(self.i18n.tr("overlay-msg-selection-invalid"));
            return OverlayOutcome::Stay;
        };
        self.remember_selection();
        let snapshot = self.history_snapshot(w, h, &rgba);
        match self.output.copy_image(w, h, &rgba) {
            Ok(()) => {
                tracing::info!(width = w, height = h, "截图已复制到剪贴板");
                self.output.after_copy(w, h, &rgba);
                self.record_history(HistorySource::Copied, snapshot);
                OverlayOutcome::Close
            }
            Err(e) => {
                tracing::error!(error = %e, "复制截图到剪贴板失败");
                self.status_message = Some(self.i18n.tr_with(
                    "overlay-msg-copy-failed",
                    &Args::new().arg(1, e.to_string()),
                ));
                OverlayOutcome::Stay
            }
        }
    }

    /// 保存选区为文件（用户点“保存”）。
    fn save_selection_and_close(&mut self) -> OverlayOutcome {
        self.save_selection_with(None)
    }

    /// 导出命令入口（总线 / 热键）：复制或按请求保存当前选区。
    ///
    /// # 参数
    /// - `target`：导出去向。
    pub fn apply_export(
        &mut self,
        target: &snow_app_core::command::ExportTarget,
    ) -> OverlayOutcome {
        match target {
            snow_app_core::command::ExportTarget::Copy => self.copy_selection_and_close(),
            snow_app_core::command::ExportTarget::Save(request) => {
                self.save_selection_with(Some(request))
            }
        }
    }

    /// 保存选区：输出通道给出任务时登记为异步（对话框不能在界面借用里弹），否则同步保存。
    fn save_selection_with(&mut self, request: Option<&SaveRequest>) -> OverlayOutcome {
        self.save_selection_inner(request, false)
    }

    /// 保存选区；`quick` 为真时跳过另存为对话框，直接按配置同步保存。
    fn save_selection_inner(
        &mut self,
        request: Option<&SaveRequest>,
        quick: bool,
    ) -> OverlayOutcome {
        if !self.has_committed_selection() {
            self.status_message = Some(self.i18n.tr("overlay-msg-select-area-first"));
            return OverlayOutcome::Stay;
        }
        let Some((w, h, rgba)) = self.selection_image() else {
            self.status_message = Some(self.i18n.tr("overlay-msg-selection-invalid"));
            return OverlayOutcome::Stay;
        };
        self.remember_selection();
        let snapshot = self.history_snapshot(w, h, &rgba);
        if !quick && let Some(job) = self.output.begin_manual_save(request) {
            self.pending_save = Some(PendingSave {
                job,
                width: w,
                height: h,
                rgba,
                snapshot,
            });
            return OverlayOutcome::AwaitSave;
        }
        let result = self.output.save_image_as(w, h, &rgba).map(|outcome| {
            screenshot_output::ManualSaveDone {
                outcome,
                format: crate::export_format::ExportFormat::Png,
                remember: false,
            }
        });
        let outcome = self.complete_save(result);
        if outcome == OverlayOutcome::Close {
            self.record_history(HistorySource::Saved, snapshot);
        }
        outcome
    }

    /// 处理保存结果：成功关闭覆盖窗，取消保持原样，失败保留窗口并提示。
    ///
    /// # 参数
    /// - `result`：保存任务的结果；失败是已本地化的提示。
    pub fn complete_save(
        &mut self,
        result: Result<screenshot_output::ManualSaveDone, String>,
    ) -> OverlayOutcome {
        match result {
            Ok(done) => match done.outcome {
                SaveOutcome::Saved(path) => {
                    tracing::info!(path = %path.display(), "截图已保存");
                    if done.remember {
                        self.output.remember_save(&path, done.format);
                    }
                    OverlayOutcome::Close
                }
                SaveOutcome::Cancelled => {
                    tracing::info!("用户取消了另存为");
                    OverlayOutcome::Stay
                }
            },
            Err(message) => {
                tracing::error!(error = %message, "保存截图失败");
                // 失败提示已由保存任务按界面语言生成
                self.status_message = Some(message);
                OverlayOutcome::Stay
            }
        }
    }

    /// 把选区（含标注合成结果）贴到屏幕原位；成功后关闭覆盖窗，失败保留窗口并提示。
    fn pin_selection_and_close(&mut self) -> OverlayOutcome {
        if !self.has_committed_selection() {
            self.status_message = Some(self.i18n.tr("overlay-msg-select-area-first"));
            return OverlayOutcome::Stay;
        }
        let Some(rect) = self
            .current_selection()
            .and_then(|r| self.screen_bounds.intersect(&r))
        else {
            self.status_message = Some(self.i18n.tr("overlay-msg-selection-invalid"));
            return OverlayOutcome::Stay;
        };
        let Some((w, h, rgba)) = self.selection_image() else {
            self.status_message = Some(self.i18n.tr("overlay-msg-selection-invalid"));
            return OverlayOutcome::Stay;
        };
        self.remember_selection();
        let snapshot = self.history_snapshot(w, h, &rgba);
        match self.output.pin_image(rect, w, h, rgba) {
            Ok(()) => {
                tracing::info!(rect = ?rect, width = w, height = h, "选区已贴图");
                self.record_history(HistorySource::Pinned, snapshot);
                OverlayOutcome::Close
            }
            Err(e) => {
                tracing::error!(error = %e, "贴图失败");
                self.status_message = Some(
                    self.i18n
                        .tr_with("overlay-msg-pin-failed", &Args::new().arg(1, e.to_string())),
                );
                OverlayOutcome::Stay
            }
        }
    }

    /// 对选区（含标注合成结果）发起文字识别；识别在后台进行，结果经 [`Self::finish_ocr`] 回来。
    fn start_ocr(&mut self) -> OverlayOutcome {
        if !self.has_committed_selection() {
            self.status_message = Some(self.i18n.tr("overlay-msg-select-area-first"));
            return OverlayOutcome::Stay;
        }
        if self.ocr.is_busy() || self.translate.is_busy() {
            return OverlayOutcome::Stay;
        }
        if self.translate.is_visible() {
            self.dismiss_translate();
        }
        let Some((w, h, rgba)) = self.selection_image() else {
            self.status_message = Some(self.i18n.tr("overlay-msg-selection-invalid"));
            return OverlayOutcome::Stay;
        };
        self.ocr_serial += 1;
        match self.output.start_ocr(self.ocr_serial, w, h, rgba) {
            Ok(()) => {
                tracing::info!(
                    serial = self.ocr_serial,
                    width = w,
                    height = h,
                    "已提交文字识别"
                );
                self.set_ocr_state(OcrUiState::Running);
            }
            Err(e) => {
                tracing::error!(error = %e, "提交文字识别失败");
                self.set_ocr_state(OcrUiState::Failed {
                    message: self.i18n.tr_with(
                        "overlay-msg-ocr-unavailable",
                        &Args::new().arg(1, e.to_string()),
                    ),
                    can_download: false,
                });
            }
        }
        OverlayOutcome::Stay
    }

    /// 对选区做二维码识别：同步解码，成功则复制内容并复用文字结果面板展示，找不到码给出提示。
    fn recognize_qr_code(&mut self) -> OverlayOutcome {
        if !self.has_committed_selection() {
            self.status_message = Some(self.i18n.tr("overlay-msg-select-area-first"));
            return OverlayOutcome::Stay;
        }
        if self.ocr.is_busy() || self.translate.is_busy() {
            return OverlayOutcome::Stay;
        }
        if self.translate.is_visible() {
            self.dismiss_translate();
        }
        let Some((w, h, rgba)) = self.selection_image() else {
            self.status_message = Some(self.i18n.tr("overlay-msg-selection-invalid"));
            return OverlayOutcome::Stay;
        };
        let codes = crate::qr_decode::decode_qr_codes(w, h, &rgba);
        if codes.is_empty() {
            self.set_ocr_state(OcrUiState::Failed {
                message: self.i18n.tr("overlay-msg-qr-none"),
                can_download: false,
            });
            return OverlayOutcome::Stay;
        }
        let text = codes.join(
            "
",
        );
        let copied = self.output.copy_text(&text).is_ok();
        tracing::info!(count = codes.len(), copied, "二维码识别完成");
        let message = self.i18n.tr_with(
            "overlay-msg-qr-found",
            &Args::new().arg(1, codes.len().to_string()),
        );
        let link = codes
            .iter()
            .find_map(|code| snow_platform::shell::web_link(code));
        self.set_ocr_state(OcrUiState::Done {
            text,
            boxes: Vec::new(),
            copied,
        });
        self.qr_link = link;
        self.status_message = Some(message);
        OverlayOutcome::Stay
    }

    /// 用系统默认浏览器打开二维码里的网页链接；成功后关闭覆盖窗，失败保留窗口并提示。
    fn open_qr_link(&mut self) -> OverlayOutcome {
        let Some(url) = self.qr_link.clone() else {
            return OverlayOutcome::Stay;
        };
        match self.output.open_url(&url) {
            Ok(()) => {
                tracing::info!("已用默认浏览器打开二维码链接");
                OverlayOutcome::Close
            }
            Err(e) => {
                tracing::warn!(error = %e, "打开二维码链接失败");
                self.status_message = Some(
                    self.i18n
                        .tr_with("overlay-msg-open-link-failed", &Args::new().arg(1, e)),
                );
                OverlayOutcome::Stay
            }
        }
    }

    /// 切换 OCR 状态，同时刷新底部状态条。
    fn set_ocr_state(&mut self, state: OcrUiState) {
        self.qr_link = None;
        self.status_message = state.status_text(self.i18n);
        self.ocr = state;
    }

    /// 退出 OCR 界面（回到框选状态）；识别仍在进行时，其结果会因序号过期被丢弃。
    fn dismiss_ocr(&mut self) {
        self.ocr_serial += 1;
        self.ocr = OcrUiState::Idle;
        self.qr_link = None;
        self.status_message = None;
    }

    /// 收到识别结果：成功则把文本复制到剪贴板并展示，失败则展示原因；过期结果被丢弃。
    ///
    /// # 参数
    /// - `serial`：结果对应的请求序号。
    /// - `result`：识别结果或失败原因。
    pub fn finish_ocr(&mut self, serial: u64, result: Result<OcrResult, OcrError>) {
        if serial != self.ocr_serial || !matches!(self.ocr, OcrUiState::Running) {
            tracing::info!(serial, current = self.ocr_serial, "丢弃过期的识别结果");
            return;
        }
        let state = match result {
            Ok(r) => {
                let copied = r.full_text.is_empty() || self.output.copy_text(&r.full_text).is_ok();
                tracing::info!(
                    lines = r.boxes.len(),
                    elapsed_ms = r.elapsed_ms,
                    copied,
                    "文字识别完成"
                );
                OcrUiState::from_result(&r, copied)
            }
            Err(e) => {
                tracing::warn!(error = ?e, "文字识别失败");
                OcrUiState::from_error(&e, self.i18n)
            }
        };
        self.set_ocr_state(state);
    }

    /// 触发 OCR 组件下载（仅缺资产时可用）。
    fn start_ocr_download(&mut self) {
        match self.output.start_ocr_download() {
            Ok(()) => self.set_ocr_state(OcrUiState::Downloading(
                self.i18n.tr("overlay-msg-ocr-download-preparing"),
            )),
            Err(e) => self.set_ocr_state(OcrUiState::Failed {
                message: self.i18n.tr_with(
                    "overlay-msg-ocr-download-start-failed",
                    &Args::new().arg(1, e.to_string()),
                ),
                can_download: false,
            }),
        }
    }

    /// 更新下载进度文案。
    ///
    /// # 参数
    /// - `step`：当前步骤说明。
    pub fn update_ocr_download(&mut self, step: &str) {
        if matches!(self.ocr, OcrUiState::Downloading(_)) {
            self.set_ocr_state(OcrUiState::Downloading(step.to_string()));
        }
    }

    /// 下载结束：成功回到待命（提示再次点 OCR），失败展示原因且可重试。
    ///
    /// # 参数
    /// - `result`：下载结果。
    pub fn finish_ocr_download(&mut self, result: Result<(), String>) {
        if !matches!(self.ocr, OcrUiState::Downloading(_)) {
            return;
        }
        match result {
            Ok(()) => {
                self.ocr = OcrUiState::Idle;
                self.status_message = Some(self.i18n.tr("overlay-msg-ocr-download-ready"));
            }
            Err(e) => self.set_ocr_state(OcrUiState::Failed {
                message: self.i18n.tr_with(
                    "overlay-msg-ocr-download-failed",
                    &Args::new().arg(1, e.to_string()),
                ),
                can_download: true,
            }),
        }
    }

    /// 当前 OCR 状态（测试与探针用）。
    pub fn ocr_state(&self) -> &OcrUiState {
        &self.ocr
    }

    /// 对选区（含标注合成结果）发起“识别 + 翻译”；在后台进行，结果经 [`Self::finish_translate`] 回来。
    fn start_translate(&mut self) -> OverlayOutcome {
        if !self.has_committed_selection() {
            self.status_message = Some(self.i18n.tr("overlay-msg-select-area-first"));
            return OverlayOutcome::Stay;
        }
        if self.translate.is_busy() || self.ocr.is_busy() {
            return OverlayOutcome::Stay;
        }
        if self.ocr.is_visible() {
            self.dismiss_ocr();
        }
        let Some((w, h, rgba)) = self.selection_image() else {
            self.status_message = Some(self.i18n.tr("overlay-msg-selection-invalid"));
            return OverlayOutcome::Stay;
        };
        self.translate_serial += 1;
        match self
            .output
            .start_translate(self.translate_serial, w, h, rgba)
        {
            Ok(()) => {
                tracing::info!(
                    serial = self.translate_serial,
                    width = w,
                    height = h,
                    "已提交文字翻译"
                );
                self.set_translate_state(TranslateUiState::Running(stage_text(
                    TranslateStage::Recognizing,
                    self.i18n,
                )));
            }
            Err(e) => {
                tracing::error!(error = %e, "提交文字翻译失败");
                self.set_translate_state(TranslateUiState::Failed {
                    message: self.i18n.tr_with(
                        "overlay-msg-translate-unavailable",
                        &Args::new().arg(1, e.to_string()),
                    ),
                    can_download: false,
                });
            }
        }
        OverlayOutcome::Stay
    }

    /// 切换翻译状态，同时刷新底部状态条。
    fn set_translate_state(&mut self, state: TranslateUiState) {
        self.status_message = state.status_text(self.i18n);
        self.translate = state;
    }

    /// 退出翻译界面（回到框选状态）；翻译仍在进行时，其结果会因序号过期被丢弃。
    fn dismiss_translate(&mut self) {
        self.translate_serial += 1;
        self.translate = TranslateUiState::Idle;
        self.status_message = None;
    }

    /// 更新翻译阶段文案（识别完成后进入翻译阶段）；过期序号被忽略。
    ///
    /// # 参数
    /// - `serial`：请求序号。
    /// - `stage`：当前阶段。
    pub fn update_translate_stage(&mut self, serial: u64, stage: TranslateStage) {
        if serial == self.translate_serial && matches!(self.translate, TranslateUiState::Running(_))
        {
            self.set_translate_state(TranslateUiState::Running(stage_text(stage, self.i18n)));
        }
    }

    /// 收到翻译结果：成功则把译文复制到剪贴板并展示，失败则展示原因；过期结果被丢弃。
    ///
    /// # 参数
    /// - `serial`：结果对应的请求序号。
    /// - `result`：翻译产出或失败原因。
    pub fn finish_translate(
        &mut self,
        serial: u64,
        result: Result<TranslateOutcome, TranslateFlowError>,
    ) {
        if serial != self.translate_serial
            || !matches!(self.translate, TranslateUiState::Running(_))
        {
            tracing::info!(
                serial,
                current = self.translate_serial,
                "丢弃过期的翻译结果"
            );
            return;
        }
        let state = match result {
            Ok(outcome) => {
                let copied = outcome.translated.is_empty()
                    || self.output.copy_text(&outcome.translated).is_ok();
                tracing::info!(
                    paragraphs = outcome.pairs.len(),
                    ocr_ms = outcome.ocr_ms,
                    translate_ms = outcome.translate_ms,
                    copied,
                    "文字翻译完成"
                );
                TranslateUiState::from_outcome(&outcome, copied)
            }
            Err(e) => {
                tracing::warn!(error = ?e, "文字翻译失败");
                TranslateUiState::from_error(&e, self.i18n)
            }
        };
        self.set_translate_state(state);
    }

    /// 触发翻译运行时下载（仅缺运行时可用）。
    fn start_translate_download(&mut self) {
        match self.output.start_translate_download() {
            Ok(()) => self.set_translate_state(TranslateUiState::Downloading(
                self.i18n.tr("overlay-msg-runtime-download-preparing"),
            )),
            Err(e) => self.set_translate_state(TranslateUiState::Failed {
                message: self.i18n.tr_with(
                    "overlay-msg-runtime-download-start-failed",
                    &Args::new().arg(1, e.to_string()),
                ),
                can_download: false,
            }),
        }
    }

    /// 更新翻译运行时下载进度文案。
    ///
    /// # 参数
    /// - `step`：当前步骤说明。
    pub fn update_translate_download(&mut self, step: &str) {
        if matches!(self.translate, TranslateUiState::Downloading(_)) {
            self.set_translate_state(TranslateUiState::Downloading(step.to_string()));
        }
    }

    /// 运行时下载结束：成功回到待命（提示再次点“翻译”），失败展示原因且可重试。
    ///
    /// # 参数
    /// - `result`：下载结果。
    pub fn finish_translate_download(&mut self, result: Result<(), String>) {
        if !matches!(self.translate, TranslateUiState::Downloading(_)) {
            return;
        }
        match result {
            Ok(()) => {
                self.translate = TranslateUiState::Idle;
                self.status_message = Some(self.i18n.tr("overlay-msg-runtime-download-ready"));
            }
            Err(e) => self.set_translate_state(TranslateUiState::Failed {
                message: self.i18n.tr_with(
                    "overlay-msg-runtime-download-failed",
                    &Args::new().arg(1, e.to_string()),
                ),
                can_download: true,
            }),
        }
    }

    /// 当前翻译状态（测试与探针用）。
    pub fn translate_state(&self) -> &TranslateUiState {
        &self.translate
    }

    /// 再次复制译文并关闭覆盖窗；复制失败保留窗口并提示。
    fn copy_translation_and_close(&mut self) -> OverlayOutcome {
        let TranslateUiState::Done { translated, .. } = &self.translate else {
            return OverlayOutcome::Stay;
        };
        if translated.is_empty() {
            return OverlayOutcome::Close;
        }
        let text = translated.clone();
        match self.output.copy_text(&text) {
            Ok(()) => OverlayOutcome::Close,
            Err(e) => {
                self.status_message = Some(self.i18n.tr_with(
                    "overlay-msg-copy-translation-failed",
                    &Args::new().arg(1, e.to_string()),
                ));
                OverlayOutcome::Stay
            }
        }
    }

    /// 翻译结果面板叠加层（无翻译界面时为空）。
    fn translate_overlay(&self, selection: PhysicalRect) -> Vec<AnyElement> {
        let lines = translate_panel_lines(&self.translate, self.i18n);
        if lines.is_empty() {
            return Vec::new();
        }
        let width = (selection.width as f32 / self.scale).clamp(160.0, OCR_PANEL_MAX_WIDTH);
        let mut panel = div()
            .absolute()
            .top(self.lp(selection.y) + px(6.0))
            .left(self.lp(selection.x) + px(6.0))
            .w(px(width))
            .px_2()
            .py_1()
            .rounded_md()
            .bg(rgba(OCR_PANEL_BG))
            .text_color(rgba(0xFFFFFFFF))
            .text_xs();
        for line in lines {
            panel = panel.child(div().child(line));
        }
        vec![panel.into_any_element()]
    }

    /// 执行双击 / 中键配置的选区动作；没有确定选区时只提示（`None` 动作除外）。
    ///
    /// # 参数
    /// - `action`：要执行的动作。
    pub fn run_click_action(&mut self, action: ClickAction) -> OverlayOutcome {
        match action {
            ClickAction::None => OverlayOutcome::Stay,
            ClickAction::Copy => self.copy_selection_and_close(),
            ClickAction::Save => self.save_selection_and_close(),
            ClickAction::QuickSave => self.save_selection_inner(None, true),
            ClickAction::Pin => self.apply_action(ToolbarAction::Pin),
        }
    }

    /// 鼠标中键：执行配置的动作（录屏 / 长截图选区模式下不响应）。
    pub fn handle_middle_click(&mut self) -> OverlayOutcome {
        if self.record_mode || self.scroll_mode || !self.has_committed_selection() {
            return OverlayOutcome::Stay;
        }
        self.run_click_action(self.middle_click_action)
    }

    /// 选区尺寸标签文字：按配置用物理或逻辑像素。
    fn size_label(&self, rect: PhysicalRect) -> String {
        if self.logical_size_label {
            let l = logical_rect(rect, self.scale);
            selection_size_label(PhysicalRect::new(l.x, l.y, l.width, l.height))
        } else {
            selection_size_label(rect)
        }
    }

    /// 驱动一步全局鼠标手势：把桌面物理坐标换成画布坐标后按普通框选处理，松开时执行自动动作 / 录屏。
    ///
    /// # 参数
    /// - `step`：这一步是按下、移动还是松开。
    /// - `desktop`：鼠标位置（虚拟桌面物理像素）。
    /// - `window`：用于收尾的窗口。
    pub fn drive_gesture(
        &mut self,
        step: GestureStep,
        desktop: PhysicalPoint,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let point = self.clamp_point(PhysicalPoint::new(
            desktop.x - self.canvas_origin.x,
            desktop.y - self.canvas_origin.y,
        ));
        match step {
            GestureStep::Down => {
                let outcome = self.handle_mouse_down(point, 1);
                self.finish(outcome, window, cx);
            }
            GestureStep::Move => {
                self.handle_mouse_move(point);
                cx.notify();
            }
            GestureStep::Up => {
                self.handle_mouse_up(point);
                let outcome = if self.record_mode {
                    self.start_recording_and_close()
                } else {
                    self.auto_confirm_outcome()
                };
                self.finish(outcome, window, cx);
            }
        }
    }

    /// 打开识别结果窗：带上选区图片、全文与文字块，覆盖窗保持不变。
    fn open_recognition_window(&mut self) {
        let OcrUiState::Done { text, boxes, .. } = &self.ocr else {
            return;
        };
        let (text, boxes) = (text.clone(), boxes.clone());
        let Some((width, height, rgba)) = self.selection_image() else {
            self.status_message = Some(self.i18n.tr("overlay-msg-selection-invalid"));
            return;
        };
        let data = crate::recognition_view::RecognitionData {
            width,
            height,
            rgba,
            text,
            boxes,
        };
        if let Err(e) = self.output.open_recognition_window(data) {
            tracing::warn!(error = %e, "打开识别结果窗失败");
            self.status_message = Some(self.i18n.tr_with(
                "overlay-msg-recognition-window-failed",
                &Args::new().arg(1, e.to_string()),
            ));
        }
    }

    /// 再次复制识别文本并关闭覆盖窗；复制失败保留窗口并提示。
    fn copy_ocr_text_and_close(&mut self) -> OverlayOutcome {
        let OcrUiState::Done { text, .. } = &self.ocr else {
            return OverlayOutcome::Stay;
        };
        if text.is_empty() {
            return OverlayOutcome::Close;
        }
        let text = text.clone();
        match self.output.copy_text(&text) {
            Ok(()) => OverlayOutcome::Close,
            Err(e) => {
                self.status_message = Some(self.i18n.tr_with(
                    "overlay-msg-copy-text-failed",
                    &Args::new().arg(1, e.to_string()),
                ));
                OverlayOutcome::Stay
            }
        }
    }

    /// 以当前选区开始长截图；成功后关闭覆盖窗，失败保留窗口并提示。
    fn start_scroll_capture_and_close(&mut self) -> OverlayOutcome {
        if !self.has_committed_selection() {
            self.status_message = Some(self.i18n.tr("overlay-msg-select-scroll-first"));
            return OverlayOutcome::Stay;
        }
        let Some(rect) = self
            .current_selection()
            .and_then(|r| self.screen_bounds.intersect(&r))
        else {
            self.status_message = Some(self.i18n.tr("overlay-msg-selection-invalid"));
            return OverlayOutcome::Stay;
        };
        if self.selection_spans_monitors() {
            self.status_message = Some(self.i18n.tr("overlay-msg-scroll-single-display"));
            return OverlayOutcome::Stay;
        }
        match self.output.start_scroll_capture(rect) {
            Ok(()) => {
                tracing::info!(rect = ?rect, "长截图选区已确认");
                OverlayOutcome::Close
            }
            Err(e) => {
                tracing::error!(error = %e, "启动长截图失败");
                self.status_message = Some(self.i18n.tr_with(
                    "overlay-msg-scroll-start-failed",
                    &Args::new().arg(1, e.to_string()),
                ));
                OverlayOutcome::Stay
            }
        }
    }

    /// OCR 结果面板与文本框叠加层（无 OCR 界面或无选区时为空）。
    fn ocr_overlay(&self, selection: PhysicalRect) -> Vec<AnyElement> {
        let mut parts: Vec<AnyElement> = Vec::new();
        if let OcrUiState::Done { boxes, .. } = &self.ocr {
            for b in boxes {
                parts.push(
                    div()
                        .absolute()
                        .top(self.lp(selection.y + b.rect.y))
                        .left(self.lp(selection.x + b.rect.x))
                        .w(self.lp(b.rect.width))
                        .h(self.lp(b.rect.height))
                        .border_1()
                        .border_color(rgb(OCR_BOX_COLOR))
                        .into_any_element(),
                );
            }
        }
        let lines = panel_lines(&self.ocr, self.i18n);
        if !lines.is_empty() {
            let width = (selection.width as f32 / self.scale).clamp(160.0, OCR_PANEL_MAX_WIDTH);
            let mut panel = div()
                .absolute()
                .top(self.lp(selection.y) + px(6.0))
                .left(self.lp(selection.x) + px(6.0))
                .w(px(width))
                .px_2()
                .py_1()
                .rounded_md()
                .bg(rgba(OCR_PANEL_BG))
                .text_color(rgba(0xFFFFFFFF))
                .text_xs();
            for line in lines {
                panel = panel.child(div().child(line));
            }
            if self.qr_link.is_some() {
                panel = panel.child(div().child(self.i18n.tr("overlay-qr-open-link-hint")));
            }
            parts.push(panel.into_any_element());
        }
        parts
    }

    /// 复制光标所在像素的颜色值（按当前色彩格式）到剪贴板。
    pub fn copy_current_color(&mut self) {
        let (r, g, b, _) = self
            .frame
            .pixel_rgba(self.cursor_pos.x, self.cursor_pos.y)
            .unwrap_or((0, 0, 0, u8::MAX));
        let text = self.color_format.format_color(r, g, b);
        match self.output.copy_text(&text) {
            Ok(()) => {
                tracing::info!(color = %text, "已复制颜色值");
                self.status_message = Some(self.i18n.tr_with(
                    "overlay-msg-color-copied",
                    &Args::new().arg(1, text.clone()),
                ));
            }
            Err(e) => {
                tracing::error!(error = %e, "复制颜色失败");
                self.status_message = Some(self.i18n.tr_with(
                    "overlay-msg-color-copy-failed",
                    &Args::new().arg(1, e.to_string()),
                ));
            }
        }
    }

    /// 切换为“录屏选区”模式：Enter / 工具栏“录屏”确认区域，复制与保存被禁用。
    ///
    /// # 参数
    /// - `enabled`：是否启用。
    pub fn set_record_mode(&mut self, enabled: bool) {
        self.record_mode = enabled;
    }

    /// 是否处于录屏选区模式。
    pub fn is_record_mode(&self) -> bool {
        self.record_mode
    }

    /// 切换为“长截图选区”模式：Enter / 双击 / 工具栏“长图”确认区域，复制与保存被禁用。
    ///
    /// # 参数
    /// - `enabled`：是否启用。
    pub fn set_scroll_mode(&mut self, enabled: bool) {
        self.scroll_mode = enabled;
    }

    /// 设置框选完成后自动执行的动作。
    ///
    /// # 参数
    /// - `action`：自动动作；`None` 表示不自动执行。
    pub fn set_auto_confirm(&mut self, action: Option<AutoConfirm>) {
        self.auto_confirm = action;
    }

    /// 鼠标松开后调用：若已有选区且设置了自动动作，则执行一次（之后恢复为普通编辑）。
    ///
    /// # 返回
    /// 窗口去向；未触发时为 `Stay`。
    pub fn auto_confirm_outcome(&mut self) -> OverlayOutcome {
        if self.annotating || !self.has_committed_selection() {
            return OverlayOutcome::Stay;
        }
        match self.auto_confirm.take() {
            Some(AutoConfirm::QuickSave) => self.run_click_action(ClickAction::QuickSave),
            Some(action) => self.apply_action(action.toolbar_action()),
            None => OverlayOutcome::Stay,
        }
    }

    /// 以当前选区开始录屏；成功后关闭覆盖窗，失败保留窗口并提示。
    fn start_recording_and_close(&mut self) -> OverlayOutcome {
        if !self.has_committed_selection() {
            self.status_message = Some(self.i18n.tr("overlay-msg-select-record-first"));
            return OverlayOutcome::Stay;
        }
        let Some(rect) = self.current_selection() else {
            self.status_message = Some(self.i18n.tr("overlay-msg-selection-invalid"));
            return OverlayOutcome::Stay;
        };
        if self.selection_spans_monitors() {
            self.status_message = Some(self.i18n.tr("overlay-msg-record-single-display"));
            return OverlayOutcome::Stay;
        }
        match self.output.start_recording(rect) {
            Ok(()) => {
                tracing::info!(rect = ?rect, "录屏选区已确认");
                OverlayOutcome::Close
            }
            Err(e) => {
                tracing::error!(error = %e, "启动录屏失败");
                self.status_message = Some(self.i18n.tr_with(
                    "overlay-msg-record-start-failed",
                    &Args::new().arg(1, e.to_string()),
                ));
                OverlayOutcome::Stay
            }
        }
    }

    /// 性能基准的单步：沿李萨如轨迹移动光标，首步按下、末步松开，模拟持续框选。
    ///
    /// # 参数
    /// - `step`：从 0 开始的步序号。
    /// - `total`：总步数。
    pub fn bench_step(&mut self, step: u32, total: u32) {
        let b = self.screen_bounds;
        let t = step as f32 / total.max(1) as f32 * std::f32::consts::TAU * 3.0;
        let x = b.x + ((0.5 + 0.45 * t.sin()) * b.width as f32) as i32;
        let y = b.y + ((0.5 + 0.45 * (t * 0.7).cos()) * b.height as f32) as i32;
        let p = PhysicalPoint::new(x, y);
        let started = Instant::now();
        if step == 0 {
            self.handle_mouse_down(p, 1);
        }
        self.handle_mouse_move(p);
        if step + 1 == total {
            self.handle_mouse_up(p);
        }
        self.probe.record_move(started.elapsed());
    }

    /// 标注性能基准准备：把选区设为屏幕内缩一圈并选中工具（文字工具会落一段示例文字）。
    ///
    /// 直接驱动视图状态，不经过操作系统输入。
    ///
    /// # 参数
    /// - `tool`：要测量的标注工具。
    pub fn bench_annotation_setup(&mut self, tool: AnnotationTool) {
        let b = self.screen_bounds;
        let margin = (b.width.min(b.height) / BENCH_MARGIN_DIVISOR).max(BENCH_MIN_MARGIN);
        let rect = PhysicalRect::new(
            b.x + margin,
            b.y + margin,
            (b.width - margin * 2).max(1),
            (b.height - margin * 2).max(1),
        );
        self.state = SelectionState::Selected { rect };
        self.select_tool(tool);
        if tool == AnnotationTool::Text {
            let (x, y) = (f64::from(rect.x + margin), f64::from(rect.y + margin));
            self.run_layer(|layer, base| layer.commit_text(x, y, BENCH_TEXT, base));
        }
    }

    /// 标注性能基准的单步：分若干笔画，每笔起点固定、终点沿椭圆移动，模拟持续拖动绘制。
    ///
    /// # 参数
    /// - `step`：从 0 开始的步序号。
    /// - `total`：总步数。
    pub fn bench_annotation_step(&mut self, step: u32, total: u32) {
        let Some(rect) = self.current_selection() else {
            return;
        };
        if self.tool == AnnotationTool::Text {
            return;
        }
        let per_stroke = (total / BENCH_STROKES).max(2);
        let stroke = (step / per_stroke).min(BENCH_STROKES - 1);
        let i = step - stroke * per_stroke;
        let last = step + 1 == total || i + 1 == per_stroke;
        let phase = i as f32 / (per_stroke - 1) as f32;
        let (cx, cy) = (
            rect.x as f32 + rect.width as f32 / 2.0,
            rect.y as f32 + rect.height as f32 / 2.0,
        );
        let (rx, ry) = (rect.width as f32 * 0.4, rect.height as f32 * 0.4);
        let angle = std::f32::consts::TAU * (0.25 * stroke as f32 + 0.6 * phase);
        let start = PhysicalPoint::new(
            (cx - rx * 0.6 + stroke as f32 * 24.0) as i32,
            (cy - ry * 0.6 + stroke as f32 * 24.0) as i32,
        );
        let end = PhysicalPoint::new(
            (cx + rx * angle.cos()) as i32,
            (cy + ry * angle.sin()) as i32,
        );
        let started = Instant::now();
        if i == 0 {
            self.handle_mouse_down(start, 1);
        }
        self.handle_mouse_move(end);
        if last {
            self.handle_mouse_up(end);
        }
        self.probe.record_move(started.elapsed());
    }

    /// 关闭覆盖窗：输出探针汇总、释放 GPU 图集里的底图并移除窗口。
    ///
    /// # 参数
    /// - `window`：当前窗口。
    pub fn close(&mut self, window: &mut Window) {
        tracing::info!(stats = %self.probe.describe(), "overlay closing");
        for image in self.frame.images() {
            // 这块图只在对应显示器的窗口里上传过；别的窗口里释放会报未找到，忽略即可
            if let Err(e) = window.drop_image(image) {
                tracing::debug!(error = %e, "释放底图图集失败（可能属于别的窗口）");
            }
        }
        self.flush_pending_drops(window);
        for (_, sprite) in self.tile_sprites.drain() {
            if let Err(e) = window.drop_image(sprite.image) {
                tracing::warn!(error = %e, "释放标注分块图集失败");
            }
        }
        if let Some(hook) = &self.close_hook {
            hook();
        }
        window.remove_window();
    }

    /// 把已被替换的标注图像从 GPU 图集释放（每次渲染开头调用，避免泄漏）。
    fn flush_pending_drops(&mut self, window: &mut Window) {
        for image in self.pending_drops.drain(..) {
            if let Err(e) = window.drop_image(image) {
                tracing::warn!(error = %e, "释放旧标注分块失败");
            }
        }
    }

    /// 按结果决定是否关闭窗口并刷新。
    fn finish(&mut self, outcome: OverlayOutcome, window: &mut Window, cx: &mut Context<Self>) {
        match outcome {
            OverlayOutcome::Close => self.close(window),
            OverlayOutcome::Stay => cx.notify(),
            OverlayOutcome::BeginText(origin) => {
                self.begin_text_edit(origin, window, cx);
                cx.notify();
            }
            OverlayOutcome::AwaitSave => self.run_pending_save(window, cx),
        }
    }

    /// 在界面借用之外执行登记的保存任务（系统对话框是模态循环，不能嵌在视图更新里），完成后回到视图收尾。
    fn run_pending_save(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(pending) = self.pending_save.take() else {
            cx.notify();
            return;
        };
        let entity = cx.entity();
        let handle = window.window_handle();
        cx.spawn(async move |_this, cx| {
            let result = pending
                .job
                .run(pending.width, pending.height, &pending.rgba);
            let snapshot = pending.snapshot;
            let _ = handle.update(cx, |_, window, app| {
                entity.update(app, |view, cx| {
                    let outcome = view.complete_save(result);
                    if outcome == OverlayOutcome::Close {
                        view.record_history(HistorySource::Saved, snapshot);
                    }
                    view.finish(outcome, window, cx);
                });
            });
        })
        .detach();
    }

    /// 告知输出通道覆盖窗的原生句柄（另存为对话框以它为所有者）。
    ///
    /// # 参数
    /// - `hwnd`：`HWND` 整数值。
    pub fn set_owner_window(&mut self, hwnd: isize) {
        self.output.set_owner_window(hwnd);
    }

    /// 外部导出命令（总线 / 热键）的窗口侧入口：执行导出并按结果收尾。
    ///
    /// # 参数
    /// - `target`：导出去向。
    pub fn run_export(
        &mut self,
        target: &snow_app_core::command::ExportTarget,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let outcome = self.apply_export(target);
        self.finish(outcome, window, cx);
    }

    /// 工具栏动作入口（由按钮点击回调）。
    fn on_toolbar_action(
        &mut self,
        action: ToolbarAction,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.commit_text_edit(window, cx);
        let outcome = self.apply_action(action);
        self.finish(outcome, window, cx);
    }

    /// 工具栏工具入口（由按钮点击回调）。
    fn on_tool_selected(
        &mut self,
        tool: AnnotationTool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.commit_text_edit(window, cx);
        self.select_tool(tool);
        if !style_fields(self.tool).is_empty() {
            self.ensure_style_ui(window, cx);
            self.sync_style_selects(window, cx);
        }
        cx.notify();
    }

    /// 把焦点还给覆盖窗根节点（文字输入结束后才能继续收到快捷键）。
    fn refocus_root(&self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(handle) = &self.focus_handle {
            window.focus(handle, cx);
        }
    }

    /// 在底图物理坐标 `origin` 处开始文字输入；已有输入先提交。
    fn begin_text_edit(
        &mut self,
        origin: PhysicalPoint,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.commit_text_edit(window, cx);
        let Some(style) = self.annotations.as_ref().map(|l| l.style()) else {
            return;
        };
        let text_style = CanvasTextStyle {
            font_family: DEFAULT_FONT_FAMILY.to_string(),
            // 输入框用逻辑像素，提交后按物理像素合成，两者视觉大小一致
            font_size: style.font_px as f32 / self.scale,
            line_height: TEXT_LINE_HEIGHT,
            color: [style.color.r, style.color.g, style.color.b, style.color.a],
            ..CanvasTextStyle::default()
        };
        let input =
            cx.new(|cx| CanvasTextInput::with_text_and_style("", text_style, None, window, cx));
        self.text_edit = Some(TextEditSession { input, origin });
    }

    /// 提交文字输入：非空文本落成标注元素；没有输入会话时什么也不做。
    fn commit_text_edit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(session) = self.text_edit.take() else {
            return;
        };
        let text = session.input.read(cx).text().to_string();
        let (x, y) = (f64::from(session.origin.x), f64::from(session.origin.y));
        self.run_layer(|layer, base| layer.commit_text(x, y, &text, base));
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

    /// 文字输入进行中时把按键交给输入框；返回 `true` 表示按键已被文字输入吞掉。
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
        let outcome = input.update(cx, |input, cx| input.handle_key(key, shift, control, cx));
        match outcome {
            EditKeyOutcome::Commit => self.commit_text_edit(window, cx),
            EditKeyOutcome::Cancel => self.cancel_text_edit(window, cx),
            EditKeyOutcome::Handled | EditKeyOutcome::Ignored => {}
        }
        true
    }

    /// 当前应显示的鼠标指针样式：选中标注工具时选区内是十字（文字工具为 I 形）。
    fn cursor_style(&self, dragging: bool) -> CursorStyle {
        if self.tool != AnnotationTool::None && self.hover_mode == SelectionDragMode::All {
            return if self.tool == AnnotationTool::Text {
                CursorStyle::IBeam
            } else {
                CursorStyle::Crosshair
            };
        }
        cursor_for_mode(self.hover_mode, dragging)
    }

    /// 底部提示条默认文案的 message id。
    fn default_hint(&self) -> &'static str {
        if self.text_edit.is_some() {
            TEXT_HINT_TEXT
        } else if self.record_mode {
            RECORD_HINT_TEXT
        } else if self.scroll_mode {
            SCROLL_HINT_TEXT
        } else if self.tool != AnnotationTool::None {
            TOOL_HINT_TEXT
        } else {
            HINT_TEXT
        }
    }

    /// 鼠标是否在样式面板范围内（用于避免放大镜遮挡面板）。
    fn cursor_over_style_panel(&self, panel_pos: Option<PhysicalPoint>) -> bool {
        let Some(pos) = panel_pos else {
            return false;
        };
        let cursor = logical_rect(
            PhysicalRect::new(self.cursor_pos.x, self.cursor_pos.y, 1, 1),
            self.scale,
        );
        PhysicalRect::new(pos.x, pos.y, STYLE_PANEL_SIZE.0, STYLE_PANEL_SIZE.1)
            .contains(PhysicalPoint::new(cursor.x, cursor.y))
    }

    /// 选区与鼠标是否在工具栏范围内（用于避免放大镜遮挡工具栏）。
    fn cursor_over_toolbar(&self, toolbar_pos: Option<PhysicalPoint>) -> bool {
        let Some(pos) = toolbar_pos else {
            return false;
        };
        let cursor = logical_rect(
            PhysicalRect::new(self.cursor_pos.x, self.cursor_pos.y, 1, 1),
            self.scale,
        );
        PhysicalRect::new(pos.x, pos.y, TOOLBAR_LOGICAL_SIZE.0, TOOLBAR_LOGICAL_SIZE.1)
            .contains(PhysicalPoint::new(cursor.x, cursor.y))
    }
}

impl Render for ScreenshotOverlayView {
    /// 渲染覆盖窗视图（单窗口路径：等价于第 0 块显示器）。
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.render_monitor(0, None, window, cx)
    }
}

impl ScreenshotOverlayView {
    /// 渲染第 `index` 块显示器窗口里的内容：整个画布场景整体平移到本屏原点，外层裁剪并接收事件。
    ///
    /// 选区、遮罩、手柄、标注分块等元素的位置仍按画布坐标计算；工具栏、形状栏、OCR / 翻译面板、
    /// 文字输入框只在它们所属的那一块屏的窗口里画，放大镜与提示条只在光标所在屏画。
    ///
    /// # 参数
    /// - `index`：显示器序号。
    /// - `focus`：本窗口自己的焦点句柄；`None` 时用视图自带的。
    ///
    /// ```ignore
    /// let element = shared.update(cx, |v, vcx| v.render_monitor(1, Some(&focus), window, vcx));
    /// ```
    pub fn render_monitor(
        &mut self,
        index: usize,
        focus: Option<&FocusHandle>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Div {
        let index = index.min(self.frame.count().saturating_sub(1));
        self.flush_pending_annotation();
        let render_started = self.probe.render_start();
        self.flush_pending_drops(window);
        if self.scale_override.is_none() {
            self.scale = window.scale_factor();
        }
        let scale = self.scale;
        // 窗口点击候选期间不画框选遮罩，改画窗口高亮
        let sel = if self.click_window.is_some() {
            None
        } else {
            self.current_selection()
        };
        // 加 / 减区域期间没有当前选区，用底区域的外接矩形来显示遮罩
        let sel = sel.or_else(|| {
            self.region_op.as_ref()?;
            let (x, y, w, h) = self.region_mask.as_ref()?.bounds()?;
            Some(PhysicalRect::new(x, y, w, h))
        });
        // 本屏在画布里的几何；锚点屏 = 选区右下角所在屏，光标屏 = 光标所在屏
        let mon = self.frame.rect(index);
        let (ox, oy) = (mon.x as f32 / scale, mon.y as f32 / scale);
        let (mon_w, mon_h) = (mon.width as f32 / scale, mon.height as f32 / scale);
        let cursor_here = self.frame.nearest_monitor(self.cursor_pos) == index;
        let anchor_here = sel.is_some_and(|s| {
            self.frame
                .nearest_monitor(PhysicalPoint::new(s.right() - 1, s.bottom() - 1))
                == index
        });
        let loading_history = self.history_busy();
        if loading_history {
            self.poll_history();
        }
        let refining = self.poll_refinement() || self.history_busy();
        let highlight = self
            .highlight_transition
            .advance(self.window_highlight(), Instant::now());
        if refining || self.highlight_transition.is_running() {
            window.request_animation_frame();
        }
        let (frame_w, frame_h) = self.frame.size();
        let screen_w = frame_w as f32 / scale;
        let screen_h = frame_h as f32 / scale;
        let dragging = matches!(self.state, SelectionState::Reshaping { .. });

        // 内容层：整个画布场景，整体平移到本屏原点（外层负责裁剪与事件）
        let mut root = div()
            .absolute()
            .left(px(-ox))
            .top(px(-oy))
            .w(px(screen_w))
            .h(px(screen_h));

        // 冻结底图：图像资源只构造一次，这里仅复用句柄
        root = root.child(
            img(ImageSource::Render(self.frame.image(index)))
                .absolute()
                .top(px(oy))
                .left(px(ox))
                .w(px(mon_w))
                .h(px(mon_h))
                .object_fit(ObjectFit::Fill),
        );

        // 标注预览：每个非空分块一张图，位置固定；变化时只替换被脏区触及的块
        for sprite in self.tile_sprites.values() {
            root = root.child(
                img(ImageSource::Render(Arc::clone(&sprite.image)))
                    .absolute()
                    .top(self.lp(sprite.y as i32))
                    .left(self.lp(sprite.x as i32))
                    .w(self.lp(sprite.w as i32))
                    .h(self.lp(sprite.h as i32))
                    .object_fit(ObjectFit::Fill),
            );
        }

        // 自定义区域草稿：折线 / 曲线 / 自由绘制的轮廓与顶点
        if let Some(draft) = &self.region_draft
            && !draft.is_empty()
            && let Some(shape) = self.region_type.shape()
        {
            let cursor = (self.cursor_pos.x as f32, self.cursor_pos.y as f32);
            let vertices = draft.preview(Some(cursor));
            let outline: Vec<(f32, f32)> = flatten_commands(&shape_commands(shape, &vertices))
                .into_iter()
                .map(|(x, y)| (x / scale, y / scale))
                .collect();
            let closed = outline.len() >= 3;
            root = root.child(
                canvas(
                    |_, _, _| (),
                    move |bounds, (), window, _| {
                        let mut builder = PathBuilder::stroke(px(2.0));
                        let at = |(x, y): (f32, f32)| {
                            snow_ui::ui::point(bounds.origin.x + px(x), bounds.origin.y + px(y))
                        };
                        if let Some(first) = outline.first() {
                            builder.move_to(at(*first));
                            for p in &outline[1..] {
                                builder.line_to(at(*p));
                            }
                            if closed {
                                builder.close();
                            }
                        }
                        if let Ok(path) = builder.build() {
                            window.paint_path(path, rgb(ACCENT_COLOR));
                        }
                    },
                )
                .absolute()
                .top(px(0.0))
                .left(px(0.0))
                .w(px(screen_w))
                .h(px(screen_h)),
            );
            if shape != RegionShape::Freehand {
                for &(x, y) in draft.points() {
                    root = root.child(
                        div()
                            .absolute()
                            .top(px(y / scale - 3.0))
                            .left(px(x / scale - 3.0))
                            .w(px(6.0))
                            .h(px(6.0))
                            .rounded_full()
                            .bg(rgb(ACCENT_COLOR)),
                    );
                }
            }
        }

        // 智能选区：悬停窗口的描边与尺寸标签
        if let Some(w) = highlight {
            let (wx, wy) = (w.x as f32 / scale, w.y as f32 / scale);
            let (ww, wh) = (w.width as f32 / scale, w.height as f32 / scale);
            root = root.child(
                div()
                    .absolute()
                    .top(px(wy))
                    .left(px(wx))
                    .w(px(ww))
                    .h(px(wh))
                    .bg(rgba(HOVER_FILL_COLOR))
                    .border_2()
                    .border_color(rgb(ACCENT_COLOR)),
            );
            let label_top = if wy >= LABEL_OFFSET {
                wy - LABEL_OFFSET
            } else {
                wy + 4.0
            };
            root = root.child(
                div()
                    .absolute()
                    .top(px(label_top))
                    .left(px(wx.max(4.0)))
                    .px_2()
                    .py_0p5()
                    .rounded_xs()
                    .bg(rgba(LABEL_BG_COLOR))
                    .text_color(rgba(0xFFFFFFFF))
                    .text_xs()
                    .child(self.size_label(w)),
            );
        }

        let mut toolbar_pos: Option<PhysicalPoint> = None;
        let mut panel_pos: Option<PhysicalPoint> = None;
        if let Some(s) = sel {
            let (sx, sy) = (s.x as f32 / scale, s.y as f32 / scale);
            let (sw, sh) = (s.width as f32 / scale, s.height as f32 / scale);
            let mask_color = rgba(self.mask_color);

            // 选区外四向暗化遮罩（上 / 下 / 左 / 右 四个矩形拼接）
            let dark = |top: f32, left: f32, w: f32, h: f32| {
                div()
                    .absolute()
                    .top(px(top))
                    .left(px(left))
                    .w(px(w))
                    .h(px(h))
                    .bg(mask_color)
            };
            if sy > 0.0 {
                root = root.child(dark(0.0, 0.0, screen_w, sy));
            }
            if sy + sh < screen_h {
                root = root.child(dark(sy + sh, 0.0, screen_w, screen_h - (sy + sh)));
            }
            if sx > 0.0 {
                root = root.child(dark(sy, 0.0, sx, sh));
            }
            if sx + sw < screen_w {
                root = root.child(dark(sy, sx + sw, screen_w - (sx + sw), sh));
            }

            // 自定义区域：外接矩形内的压暗 + 轮廓图；此时没有矩形框和手柄
            let custom_region = self.region_overlay.is_some();
            if let Some(overlay) = &self.region_overlay {
                root = root.child(
                    img(ImageSource::Render(Arc::clone(overlay)))
                        .absolute()
                        .top(px(sy))
                        .left(px(sx))
                        .w(px(sw))
                        .h(px(sh))
                        .object_fit(ObjectFit::Fill),
                );
            }

            // 选区框
            if !custom_region {
                root = root.child(
                    div()
                        .absolute()
                        .top(px(sy))
                        .left(px(sx))
                        .w(px(sw))
                        .h(px(sh))
                        .border_1()
                        .border_color(rgba(self.border_color)),
                );
            }

            // 八向手柄（边长保持约 8 个逻辑像素）
            let handle_size = (HANDLE_LOGICAL_SIZE * scale).round() as i32;
            for (_, hr) in handle_rects(s, handle_size)
                .into_iter()
                .filter(|_| !custom_region)
            {
                root = root.child(
                    div()
                        .absolute()
                        .top(self.lp(hr.y))
                        .left(self.lp(hr.x))
                        .w(self.lp(hr.width))
                        .h(self.lp(hr.height))
                        .bg(rgba(0xFFFFFFFF))
                        .border_1()
                        .border_color(rgba(self.border_color)),
                );
            }

            // 尺寸标签（选区左上方，贴近上沿时落到选区内侧）
            let label_top = if sy >= LABEL_OFFSET {
                sy - LABEL_OFFSET
            } else {
                sy + 4.0
            };
            root = root.child(
                div()
                    .absolute()
                    .top(px(label_top))
                    .left(px(sx.max(4.0)))
                    .px_2()
                    .py_0p5()
                    .rounded_xs()
                    .bg(rgba(LABEL_BG_COLOR))
                    .text_color(rgba(0xFFFFFFFF))
                    .text_xs()
                    .child(self.size_label(s)),
            );

            // 浮动工具栏（选区确定后展示；标注工具未实现所以隐藏，贴图 / OCR / 翻译置灰）
            if matches!(self.state, SelectionState::Selected { .. }) && anchor_here {
                let screen_logical =
                    PhysicalRect::new(ox as i32, oy as i32, mon_w as i32, mon_h as i32);
                let on_this_monitor = mon.intersect(&s).unwrap_or(s);
                let pos = calculate_toolbar_placement(
                    logical_rect(on_this_monitor, scale),
                    PhysicalPoint::new(TOOLBAR_LOGICAL_SIZE.0, TOOLBAR_LOGICAL_SIZE.1),
                    screen_logical,
                    TOOLBAR_MARGIN,
                );
                toolbar_pos = Some(pos);
                let entity = cx.entity();
                let tool_entity = entity.clone();
                let (can_undo, can_redo) = self.history_state();
                let tb = ScreenshotToolbar::new("overlay-toolbar")
                    .active_tool(self.tool)
                    .undo_redo_state(can_undo, can_redo)
                    .show_tools(!self.record_mode && !self.scroll_mode)
                    .disabled_actions(if self.record_mode {
                        &RECORD_MODE_DISABLED_ACTIONS[..]
                    } else if self.scroll_mode {
                        &SCROLL_MODE_DISABLED_ACTIONS[..]
                    } else {
                        &DISABLED_TOOLBAR_ACTIONS[..]
                    })
                    .on_tool_change(move |tool, window, app| {
                        tool_entity.update(app, |this, cx| this.on_tool_selected(tool, window, cx));
                    })
                    .on_action(move |action, window, app| {
                        entity.update(app, |this, cx| this.on_toolbar_action(action, window, cx));
                    });
                // 按右边缘锚定：真实宽度与估算不符时也不会溢出屏幕右侧
                let right_gap = (screen_w - (pos.x + TOOLBAR_LOGICAL_SIZE.0) as f32).max(0.0);
                root = root.child(
                    div()
                        .absolute()
                        .top(px(pos.y as f32))
                        .right(px(right_gap))
                        .child(tb),
                );
                // 样式面板按本屏相对坐标摆放，再平移回画布坐标
                let (mx, my) = (ox as i32, oy as i32);
                if let Some(origin) =
                    self.style_panel_origin((pos.x - mx, pos.y - my), (mon_w as i32, mon_h as i32))
                {
                    let origin = (origin.0 + mx, origin.1 + my);
                    panel_pos = Some(PhysicalPoint::new(origin.0, origin.1));
                    root = root.child(self.render_style_panel(origin, cx));
                }
            }
        } else {
            // 未选区时整屏薄遮罩
            root = root.child(
                div()
                    .absolute()
                    .top(px(0.0))
                    .left(px(0.0))
                    .w(px(screen_w))
                    .h(px(screen_h))
                    .bg(rgba(IDLE_MASK_COLOR)),
            );
        }

        // 选区形状栏：选区阶段浮在屏幕顶部；选区确定后贴在工具栏上方（含加 / 减区域）
        if (anchor_here || (sel.is_none() && cursor_here))
            && let Some(bar) = self.render_region_bar(toolbar_pos, (ox, oy, mon_w, mon_h), cx)
        {
            root = root.child(bar);
        }

        // OCR 结果：文本框描边 + 结果面板；翻译结果面板
        if let Some(s) = sel
            && anchor_here
        {
            for part in self.ocr_overlay(s) {
                root = root.child(part);
            }
            for part in self.translate_overlay(s) {
                root = root.child(part);
            }
        }

        // 文字输入框：点击框内不冒泡到根节点（否则会被当成“点击别处”而提交）
        if let Some(session) = self
            .text_edit
            .as_ref()
            .filter(|s| self.frame.nearest_monitor(s.origin) == index)
        {
            root = root.child(
                div()
                    .absolute()
                    .top(self.lp(session.origin.y))
                    .left(self.lp(session.origin.x))
                    .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    .on_mouse_down(MouseButton::Right, |_, _, cx| cx.stop_propagation())
                    .child(session.input.clone()),
            );
        }

        // 放大镜：跟随光标，光标压在工具栏上时隐藏
        if cursor_here
            && !self.magnifier_hidden
            && !self.cursor_over_toolbar(toolbar_pos)
            && !self.cursor_over_style_panel(panel_pos)
        {
            let screen_logical =
                PhysicalRect::new(ox as i32, oy as i32, mon_w as i32, mon_h as i32);
            let cursor_logical = logical_rect(
                PhysicalRect::new(self.cursor_pos.x, self.cursor_pos.y, 1, 1),
                scale,
            );
            let pos = calculate_magnifier_placement(
                PhysicalPoint::new(cursor_logical.x, cursor_logical.y),
                PhysicalPoint::new(MAGNIFIER_LOGICAL_SIZE.0, MAGNIFIER_LOGICAL_SIZE.1),
                screen_logical,
                MAGNIFIER_OFFSET,
            );
            let shown_cursor = if self.coordinate_global {
                PhysicalPoint::new(
                    self.cursor_pos.x + self.canvas_origin.x,
                    self.cursor_pos.y + self.canvas_origin.y,
                )
            } else {
                self.cursor_pos
            };
            let mag = Magnifier::new("cursor-mag", self.magnifier_grid.clone(), shown_cursor)
                .selection_rect(sel)
                .color_format(self.color_format);
            root = root.child(
                div()
                    .absolute()
                    .top(px(pos.y as f32))
                    .left(px(pos.x as f32))
                    .child(mag),
            );
        }

        // 外层：裁剪、背景、光标、事件
        let entity = cx.entity();
        let mut outer = div()
            .relative()
            .size_full()
            .bg(rgb(0x000000))
            .overflow_hidden()
            .cursor(self.cursor_style(dragging))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, ev: &MouseDownEvent, window, cx| {
                    this.scale = this.scale_override.unwrap_or(window.scale_factor());
                    // 点击输入框以外的位置：先提交正在输入的文字
                    this.commit_text_edit(window, cx);
                    let p = this.canvas_point(index, ev.position);
                    this.set_shift(ev.modifiers.shift);
                    let outcome = this.handle_mouse_down(p, ev.click_count);
                    this.finish(outcome, window, cx);
                }),
            )
            .on_mouse_down(
                MouseButton::Middle,
                cx.listener(|this, _: &MouseDownEvent, window, cx| {
                    let outcome = this.handle_middle_click();
                    this.finish(outcome, window, cx);
                }),
            )
            .on_mouse_down(
                MouseButton::Right,
                cx.listener(|this, _: &MouseDownEvent, window, cx| {
                    let outcome = this.handle_right_click();
                    this.finish(outcome, window, cx);
                }),
            )
            .on_scroll_wheel(cx.listener(move |this, ev: &ScrollWheelEvent, window, cx| {
                this.scale = this.scale_override.unwrap_or(window.scale_factor());
                let lines_y = match ev.delta {
                    ScrollDelta::Lines(p) => p.y,
                    ScrollDelta::Pixels(p) => p.y.as_f32(),
                };
                let point = this.canvas_point(index, ev.position);
                if this.handle_scroll(lines_y, point) {
                    cx.stop_propagation();
                    cx.notify();
                }
            }))
            .on_key_down(cx.listener(|this, ev: &KeyDownEvent, window, cx| {
                let mods = ev.keystroke.modifiers;
                // 文字输入进行中：按键先交给输入框（字符本身走系统输入法通道）
                if this.route_key_to_text_edit(
                    &ev.keystroke.key,
                    mods.shift,
                    mods.control,
                    window,
                    cx,
                ) {
                    cx.stop_propagation();
                    return;
                }
                this.set_shift(mods.shift);
                let outcome =
                    this.handle_keystroke(&ev.keystroke.key, mods.control, mods.shift, mods.alt);
                this.finish(outcome, window, cx);
            }))
            .on_key_up(cx.listener(|this, ev: &KeyUpEvent, _window, _cx| {
                let mods = ev.keystroke.modifiers;
                this.set_shift(mods.shift);
                this.handle_key_release(&ev.keystroke.key, mods.control, mods.shift, mods.alt);
            }));
        if let Some(handle) = focus.or(self.focus_handle.as_ref()) {
            outer = outer.track_focus(handle);
        }
        // 移动与松开用窗口级原始监听：按下后系统把鼠标捕获给起始窗口，指针拖出本窗口（跨屏框选）
        // 后元素级监听收不到（不再命中悬停），原始监听仍能拿到带符号的窗口外坐标
        outer = outer.child(
            canvas(
                |_, _, _| (),
                move |_, (), window, _| {
                    let moves = entity.clone();
                    window.on_mouse_event(move |ev: &MouseMoveEvent, phase, window, app| {
                        if phase != DispatchPhase::Capture {
                            return;
                        }
                        moves.update(app, |this, cx| {
                            let started = Instant::now();
                            this.scale = this.scale_override.unwrap_or(window.scale_factor());
                            let p = this.canvas_point(index, ev.position);
                            this.set_shift(ev.modifiers.shift);
                            this.handle_mouse_move(p);
                            this.probe.record_move(started.elapsed());
                            cx.notify();
                        });
                    });
                    let ups = entity.clone();
                    window.on_mouse_event(move |ev: &MouseUpEvent, phase, window, app| {
                        if phase != DispatchPhase::Capture || ev.button != MouseButton::Left {
                            return;
                        }
                        ups.update(app, |this, cx| {
                            this.scale = this.scale_override.unwrap_or(window.scale_factor());
                            let p = this.canvas_point(index, ev.position);
                            this.set_shift(ev.modifiers.shift);
                            this.handle_mouse_up(p);
                            let outcome = this.auto_confirm_outcome();
                            this.finish(outcome, window, cx);
                        });
                    });
                },
            )
            .absolute()
            .size_full(),
        );
        outer = outer.child(root);

        // 底部提示条（状态消息优先）：相对窗口摆放，只在光标所在屏显示
        if cursor_here {
            let hint = self
                .status_message
                .clone()
                .unwrap_or_else(|| self.i18n.tr(self.default_hint()));
            outer = outer.child(
                div()
                    .absolute()
                    .bottom(px(12.0))
                    .left(px(16.0))
                    .px_3()
                    .py_1()
                    .rounded_md()
                    .bg(rgba(HINT_BG_COLOR))
                    .text_color(rgba(HINT_TEXT_COLOR))
                    .text_xs()
                    .child(hint),
            );
        }

        self.probe.render_end(render_started);
        outer
    }
}

/// 一块显示器上的覆盖窗根视图：只持有共享视图的引用，自己的焦点句柄，并把渲染转给共享视图。
pub struct OverlayWindowView {
    /// 所有显示器共用的覆盖窗逻辑视图。
    shared: Entity<ScreenshotOverlayView>,
    /// 本窗口对应的显示器序号。
    index: usize,
    /// 本窗口自己的焦点句柄（键盘事件进入哪个窗口取决于焦点）。
    focus: FocusHandle,
}

impl OverlayWindowView {
    /// 创建窗口根视图并抢占键盘焦点。
    ///
    /// # 参数
    /// - `shared`：共享的覆盖窗逻辑视图。
    /// - `index`：本窗口对应的显示器序号。
    /// - `window`：本窗口。
    /// - `cx`：根视图上下文。
    ///
    /// ```ignore
    /// let root = app.new(|cx| OverlayWindowView::new(shared.clone(), 1, window, cx));
    /// ```
    pub fn new(
        shared: Entity<ScreenshotOverlayView>,
        index: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let focus = cx.focus_handle();
        window.focus(&focus, cx);
        // 共享视图的状态变化要让本窗口重绘
        cx.observe(&shared, |_, _, cx| cx.notify()).detach();
        Self {
            shared,
            index,
            focus,
        }
    }

    /// 共享的覆盖窗逻辑视图。
    pub fn shared(&self) -> Entity<ScreenshotOverlayView> {
        self.shared.clone()
    }
}

impl Render for OverlayWindowView {
    /// 渲染：交给共享视图按本屏几何绘制。
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let (index, focus) = (self.index, self.focus.clone());
        self.shared.update(cx, |view, vcx| {
            view.render_monitor(index, Some(&focus), window, vcx)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use snow_platform::capture::CapturedScreen;
    use std::cell::RefCell;
    use std::rc::Rc;

    /// 记录到的输出动作。
    #[derive(Debug, Default)]
    struct Recorded {
        /// 复制的图像 `(宽, 高, RGBA)`。
        images: Vec<(u32, u32, Vec<u8>)>,
        /// 复制的文本。
        texts: Vec<String>,
        /// 打开的链接。
        urls: Vec<String>,
        /// 保存的图像 `(宽, 高)`。
        saved: Vec<(u32, u32)>,
        /// 录屏选区。
        records: Vec<PhysicalRect>,
        /// 贴图 `(选区, 宽, 高, RGBA)`。
        pins: Vec<(PhysicalRect, u32, u32, Vec<u8>)>,
        /// 文字识别请求 `(序号, 宽, 高, RGBA)`。
        ocrs: Vec<(u64, u32, u32, Vec<u8>)>,
        /// OCR 下载请求次数。
        ocr_downloads: u32,
        /// 文字翻译请求 `(序号, 宽, 高, RGBA)`。
        translates: Vec<(u64, u32, u32, Vec<u8>)>,
        /// 翻译运行时下载请求次数。
        translate_downloads: u32,
        /// 长截图选区。
        scrolls: Vec<PhysicalRect>,
    }

    /// 记录型输出通道；`fail` 为真时所有操作返回错误。
    struct RecordingSink {
        /// 共享记录。
        rec: Rc<RefCell<Recorded>>,
        /// 是否模拟失败。
        fail: bool,
    }

    impl OutputSink for RecordingSink {
        /// 记录复制的图像。
        fn copy_image(&mut self, w: u32, h: u32, rgba: &[u8]) -> Result<(), String> {
            if self.fail {
                return Err("boom".into());
            }
            self.rec.borrow_mut().images.push((w, h, rgba.to_vec()));
            Ok(())
        }
        /// 记录复制的文本。
        fn copy_text(&mut self, text: &str) -> Result<(), String> {
            if self.fail {
                return Err("boom".into());
            }
            self.rec.borrow_mut().texts.push(text.to_string());
            Ok(())
        }
        /// 记录打开的链接。
        fn open_url(&mut self, url: &str) -> Result<(), String> {
            if self.fail {
                return Err("boom".into());
            }
            self.rec.borrow_mut().urls.push(url.to_string());
            Ok(())
        }
        /// 记录保存动作。
        fn save_image(&mut self, w: u32, h: u32, _rgba: &[u8]) -> Result<PathBuf, String> {
            if self.fail {
                return Err("boom".into());
            }
            self.rec.borrow_mut().saved.push((w, h));
            Ok(PathBuf::from("mock.png"))
        }
        /// 记录录屏选区。
        fn start_recording(&mut self, region: PhysicalRect) -> Result<(), String> {
            if self.fail {
                return Err("boom".into());
            }
            self.rec.borrow_mut().records.push(region);
            Ok(())
        }
        /// 记录贴图请求。
        fn pin_image(
            &mut self,
            region: PhysicalRect,
            w: u32,
            h: u32,
            rgba: Vec<u8>,
        ) -> Result<(), String> {
            if self.fail {
                return Err("boom".into());
            }
            self.rec.borrow_mut().pins.push((region, w, h, rgba));
            Ok(())
        }
        /// 记录文字识别请求。
        fn start_ocr(&mut self, serial: u64, w: u32, h: u32, rgba: Vec<u8>) -> Result<(), String> {
            if self.fail {
                return Err("boom".into());
            }
            self.rec.borrow_mut().ocrs.push((serial, w, h, rgba));
            Ok(())
        }
        /// 记录 OCR 下载请求。
        fn start_ocr_download(&mut self) -> Result<(), String> {
            if self.fail {
                return Err("boom".into());
            }
            self.rec.borrow_mut().ocr_downloads += 1;
            Ok(())
        }
        /// 记录文字翻译请求。
        fn start_translate(
            &mut self,
            serial: u64,
            w: u32,
            h: u32,
            rgba: Vec<u8>,
        ) -> Result<(), String> {
            if self.fail {
                return Err("boom".into());
            }
            self.rec.borrow_mut().translates.push((serial, w, h, rgba));
            Ok(())
        }
        /// 记录翻译运行时下载请求。
        fn start_translate_download(&mut self) -> Result<(), String> {
            if self.fail {
                return Err("boom".into());
            }
            self.rec.borrow_mut().translate_downloads += 1;
            Ok(())
        }
        /// 记录长截图请求。
        fn start_scroll_capture(&mut self, region: PhysicalRect) -> Result<(), String> {
            if self.fail {
                return Err("boom".into());
            }
            self.rec.borrow_mut().scrolls.push(region);
            Ok(())
        }
    }

    /// 构造渐变底图视图：像素 `r = x, g = y, b = 9`。
    fn view_with(
        w: u32,
        h: u32,
        scale: f32,
        fail: bool,
    ) -> (ScreenshotOverlayView, Rc<RefCell<Recorded>>) {
        let mut data = Vec::new();
        for y in 0..h {
            for x in 0..w {
                data.extend_from_slice(&[9, y as u8, x as u8, 255]);
            }
        }
        let frame = FrozenFrame::from_captured(CapturedScreen {
            width: w,
            height: h,
            data,
        })
        .unwrap();
        let rec = Rc::new(RefCell::new(Recorded::default()));
        let sink = RecordingSink {
            rec: Rc::clone(&rec),
            fail,
        };
        let view =
            ScreenshotOverlayView::new(frame, scale, PhysicalPoint::new(0, 0), Box::new(sink));
        (view, rec)
    }

    /// 拖出一个选区并松开。
    fn drag(view: &mut ScreenshotOverlayView, from: (i32, i32), to: (i32, i32)) {
        view.handle_mouse_down(PhysicalPoint::new(from.0, from.1), 1);
        view.handle_mouse_move(PhysicalPoint::new(to.0, to.1));
        view.handle_mouse_up(PhysicalPoint::new(to.0, to.1));
    }

    /// 测试用窗口悬停来源：点落在固定矩形内就返回该矩形。
    struct FakeHover(PhysicalRect);

    impl WindowHover for FakeHover {
        /// 点在矩形内返回单层路径，否则无窗口。
        fn hover(
            &mut self,
            point: PhysicalPoint,
            _target: PickTarget,
        ) -> Option<Vec<PhysicalRect>> {
            let r = self.0;
            (point.x >= r.x && point.x < r.right() && point.y >= r.y && point.y < r.bottom())
                .then(|| vec![r])
        }
    }

    /// 测试用多层来源：点落在哪几层内就返回哪几层（按钮 → 面板 → 窗口，自深到浅）。
    /// `deeper` 非空时，第一次悬停后会"细化"出一个更深层，取走后不再待定。
    #[derive(Default)]
    struct FakeLayers {
        /// 待吐出的细化路径。
        deeper: Option<Vec<PhysicalRect>>,
    }

    impl WindowHover for FakeLayers {
        /// 按点落入的层数返回路径；窗口目标时只给最后一层。
        fn hover(&mut self, point: PhysicalPoint, target: PickTarget) -> Option<Vec<PhysicalRect>> {
            let layers = [
                PhysicalRect::new(60, 50, 20, 15),
                PhysicalRect::new(50, 40, 80, 60),
                PhysicalRect::new(40, 30, 150, 120),
            ];
            let hit: Vec<PhysicalRect> = layers.into_iter().filter(|r| r.contains(point)).collect();
            if hit.is_empty() {
                return None;
            }
            match target {
                PickTarget::Window => hit.last().map(|r| vec![*r]),
                PickTarget::WindowSubElement => Some(hit),
            }
        }

        /// 取走待吐出的细化路径。
        fn refinement(&mut self) -> Option<Vec<PhysicalRect>> {
            self.deeper.take()
        }

        /// 还有细化路径没取走就算待定。
        fn refinement_pending(&self) -> bool {
            self.deeper.is_some()
        }
    }

    /// 带三层假来源的视图。
    fn layered_view() -> ScreenshotOverlayView {
        let (mut view, _) = view_with(300, 200, 1.0, false);
        view.set_window_hover(
            Some(Box::new(FakeLayers::default())),
            PickTarget::WindowSubElement,
            false,
        );
        view
    }

    /// 子控件目标默认高亮最深层；滚轮向上依次向外，向下回到更深层。
    #[test]
    fn wheel_walks_layers_outward_and_inward() {
        let mut view = layered_view();
        let p = PhysicalPoint::new(65, 55);
        view.handle_mouse_move(p);
        assert_eq!(
            view.window_highlight(),
            Some(PhysicalRect::new(60, 50, 20, 15))
        );
        assert!(view.handle_scroll(1.0, p));
        assert_eq!(
            view.window_highlight(),
            Some(PhysicalRect::new(50, 40, 80, 60))
        );
        assert!(view.handle_scroll(1.0, p));
        assert_eq!(
            view.window_highlight(),
            Some(PhysicalRect::new(40, 30, 150, 120))
        );
        assert!(view.handle_scroll(-1.0, p));
        assert_eq!(
            view.window_highlight(),
            Some(PhysicalRect::new(50, 40, 80, 60))
        );
    }

    /// 滚轮选好的层级在同一路径内移动鼠标时保持，单击选中的就是它。
    #[test]
    fn wheel_selection_survives_move_and_is_clicked() {
        let mut view = layered_view();
        let p = PhysicalPoint::new(65, 55);
        view.handle_mouse_move(p);
        view.handle_scroll(1.0, p);
        view.handle_mouse_move(PhysicalPoint::new(66, 56));
        drag(&mut view, (66, 56), (68, 57));
        assert_eq!(
            view.state,
            SelectionState::Selected {
                rect: PhysicalRect::new(50, 40, 80, 60)
            }
        );
    }

    /// 后台细化到达后（无需鼠标事件）渲染前轮询即可吸收，高亮切到更深层；取走后不再待定。
    #[test]
    fn refinement_is_absorbed_on_poll_without_mouse_events() {
        let deeper = vec![
            PhysicalRect::new(62, 52, 16, 12),
            PhysicalRect::new(60, 50, 20, 15),
            PhysicalRect::new(50, 40, 80, 60),
            PhysicalRect::new(40, 30, 150, 120),
        ];
        let (mut view, _) = view_with(300, 200, 1.0, false);
        let source = FakeLayers {
            deeper: Some(deeper),
        };
        view.set_window_hover(Some(Box::new(source)), PickTarget::WindowSubElement, false);
        // 点落在三层内：先拿到前台结果，此时细化还没吸收
        let p = PhysicalPoint::new(65, 55);
        // 只取前台结果，不触发细化吸收
        let path = view
            .window_hover
            .as_mut()
            .unwrap()
            .hover(p, PickTarget::WindowSubElement)
            .unwrap();
        view.pick.apply_hit_path(&path, view.screen_bounds, 1);
        assert_eq!(
            view.window_highlight(),
            Some(PhysicalRect::new(60, 50, 20, 15))
        );
        assert!(!view.poll_refinement());
        assert_eq!(
            view.window_highlight(),
            Some(PhysicalRect::new(62, 52, 16, 12))
        );
    }

    /// 右键回退到智能选区时立刻按光标位置命中，不必等下一次移动。
    #[test]
    fn right_click_resumes_hover_immediately() {
        let mut view = layered_view();
        view.handle_mouse_move(PhysicalPoint::new(65, 55));
        drag(&mut view, (65, 55), (200, 150));
        assert!(matches!(view.state, SelectionState::Selected { .. }));
        // 光标回到控件上再右键：不再移动鼠标，高亮也应立刻出现
        view.handle_mouse_move(PhysicalPoint::new(65, 55));
        view.handle_right_click();
        assert_eq!(
            view.window_highlight(),
            Some(PhysicalRect::new(60, 50, 20, 15))
        );
    }

    /// 复制成功后历史出口收到完整现场：整帧底图、选区、标注历史与结果图；复制失败则不记录。
    #[test]
    fn copy_records_full_snapshot_only_on_success() {
        let (mut view, _) = view_with(120, 80, 1.0, false);
        let got: Rc<RefCell<Vec<(HistorySource, HistorySnapshot)>>> = Rc::default();
        let sink = Rc::clone(&got);
        view.set_history_sink(move |source, snapshot| sink.borrow_mut().push((source, snapshot)));
        drag(&mut view, (10, 10), (70, 50));
        assert!(matches!(view.state, SelectionState::Selected { .. }));
        view.apply_action(ToolbarAction::Copy);
        let recorded = got.borrow();
        assert_eq!(recorded.len(), 1);
        let (source, snapshot) = &recorded[0];
        assert_eq!(*source, HistorySource::Copied);
        assert_eq!((snapshot.frame_width, snapshot.frame_height), (120, 80));
        assert_eq!(snapshot.frame_rgba.len(), 120 * 80 * 4);
        assert_eq!(snapshot.selection, (10, 10, 61, 41));
        assert_eq!((snapshot.result_width, snapshot.result_height), (61, 41));
        assert!(snapshot.canvas_history.is_empty());
        drop(recorded);

        let (mut failing, _) = view_with(120, 80, 1.0, true);
        let none: Rc<RefCell<Vec<HistorySource>>> = Rc::default();
        let sink = Rc::clone(&none);
        failing.set_history_sink(move |source, _| sink.borrow_mut().push(source));
        drag(&mut failing, (10, 10), (70, 50));
        failing.apply_action(ToolbarAction::Copy);
        assert!(none.borrow().is_empty());
    }

    /// 测试用历史来源：按 ID 返回预置的现场（`None` 表示读取失败）；读取立即就绪。
    struct FakeHistory {
        /// 记录 ID（新的在前）。
        ids: Vec<String>,
        /// 预置的现场。
        entries: std::collections::HashMap<String, Option<LoadedEntry>>,
        /// 已发起、等待取走的结果。
        ready: Vec<(String, Option<LoadedEntry>)>,
    }

    impl HistoryProvider for FakeHistory {
        /// 返回预置的 ID 列表。
        fn ids(&mut self) -> Vec<String> {
            self.ids.clone()
        }

        /// 读取立即完成：把预置结果放进就绪队列。
        fn begin_load(&mut self, id: &str) {
            let entry = self.entries.get(id).cloned().flatten();
            self.ready.push((id.to_string(), entry));
        }

        /// 取走一个就绪结果。
        fn poll_loaded(&mut self) -> Option<(String, Option<LoadedEntry>)> {
            self.ready.pop()
        }
    }

    /// 纯色历史现场（底图 `w x h`，选区 `(x, y, 宽, 高)`，空标注）。
    fn history_entry(w: u32, h: u32, shade: u8, selection: (i32, i32, i32, i32)) -> LoadedEntry {
        LoadedEntry {
            frame_width: w,
            frame_height: h,
            frame_rgba: vec![shade; (w * h * 4) as usize],
            selection,
            canvas_history: b"{}".to_vec(),
        }
    }

    /// 带历史来源的 100x80 视图；`c` 是最新记录，`b` 次之，`bad` 读取失败，`big` 尺寸不符。
    fn history_view() -> ScreenshotOverlayView {
        let (mut view, _) = view_with(100, 80, 1.0, false);
        let mut entries = std::collections::HashMap::new();
        entries.insert(
            "c".to_string(),
            Some(history_entry(100, 80, 50, (10, 10, 30, 20))),
        );
        entries.insert(
            "b".to_string(),
            Some(history_entry(100, 80, 90, (20, 20, 40, 30))),
        );
        entries.insert("bad".to_string(), None);
        entries.insert(
            "big".to_string(),
            Some(history_entry(200, 160, 1, (0, 0, 10, 10))),
        );
        let ids = ["c", "b", "bad", "big"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        view.set_history_provider(Box::new(FakeHistory {
            ids,
            entries,
            ready: Vec::new(),
        }));
        view
    }

    /// 取视图底图中心像素的 R 通道（BGRA 里是第三个字节）。
    fn center_red(view: &ScreenshotOverlayView) -> u8 {
        view.frame.pixel_rgba(50, 40).unwrap().0
    }

    /// 往旧翻：底图、选区换成历史记录；再往旧翻到下一条；右键回到当前截图并原样恢复。
    #[test]
    fn history_walk_applies_entries_and_right_click_restores_live() {
        let mut view = history_view();
        let live_red = center_red(&view);
        drag(&mut view, (5, 5), (60, 40));
        let live_state = view.state;
        assert!(matches!(live_state, SelectionState::Selected { .. }));

        assert!(view.history_previous());
        assert!(view.history_busy());
        assert!(view.poll_history());
        assert_eq!(center_red(&view), 50);
        assert_eq!(
            view.state,
            SelectionState::Selected {
                rect: PhysicalRect::new(10, 10, 30, 20)
            }
        );

        assert!(view.history_previous());
        assert!(view.poll_history());
        assert_eq!(center_red(&view), 90);

        // 右键：先回到当前截图，不是撤销选区
        assert_eq!(view.handle_right_click(), OverlayOutcome::Stay);
        assert_eq!(center_red(&view), live_red);
        assert_eq!(view.state, live_state);
        assert!(!view.history_busy());
    }

    /// 往新翻到头等于回到当前截图；已在当前截图时再往新翻无动作。
    #[test]
    fn history_newer_returns_to_live() {
        let mut view = history_view();
        let live_red = center_red(&view);
        assert!(!view.history_next());
        view.history_previous();
        view.poll_history();
        assert_eq!(center_red(&view), 50);
        assert!(view.history_next());
        assert_eq!(center_red(&view), live_red);
    }

    /// 读取失败的记录被跳过（不应用、不卡住），尺寸不符的同样；之后能继续往旧翻。
    #[test]
    fn history_skips_failed_and_mismatched_entries() {
        let mut view = history_view();
        view.history_previous();
        view.poll_history();
        view.history_previous();
        view.poll_history();
        assert_eq!(center_red(&view), 90);
        // 下一条 "bad" 读取失败：画面不变，状态机不再忙
        assert!(view.history_previous());
        assert!(!view.poll_history());
        assert_eq!(center_red(&view), 90);
        assert!(!view.history_busy());
        // 再翻：来到 "big"，尺寸不符同样被丢弃
        assert!(view.history_previous());
        assert!(!view.poll_history());
        assert_eq!(center_red(&view), 90);
        // 列表里已没有可翻的记录
        assert!(!view.history_previous());
    }

    /// 标注拖动或拖拽选区过程中不允许翻页；没接历史来源时翻页键无效。
    #[test]
    fn history_navigation_is_gated() {
        let (mut plain, _) = view_with(100, 80, 1.0, false);
        assert!(!plain.history_previous());
        let mut view = history_view();
        view.handle_mouse_down(PhysicalPoint::new(5, 5), 1);
        assert!(!view.history_previous());
    }

    /// 在 `(x, y)` 单击一次（按下 + 松开）。
    fn click_at(view: &mut ScreenshotOverlayView, x: i32, y: i32, count: usize) {
        view.handle_mouse_down(PhysicalPoint::new(x, y), count);
        view.handle_mouse_up(PhysicalPoint::new(x, y));
    }

    /// 带指定选区形状的 200x150 视图。
    fn region_view(region_type: RegionType) -> (ScreenshotOverlayView, Rc<RefCell<Recorded>>) {
        let (mut view, rec) = view_with(200, 150, 1.0, false);
        view.set_initial_region_type(region_type);
        (view, rec)
    }

    /// 用折线点出一个三角形（20,20）（100,20）（60,100）并双击闭合。
    fn draw_triangle(view: &mut ScreenshotOverlayView) {
        click_at(view, 20, 20, 1);
        click_at(view, 100, 20, 1);
        click_at(view, 60, 100, 1);
        view.handle_mouse_down(PhysicalPoint::new(60, 100), 2);
        view.handle_mouse_up(PhysicalPoint::new(60, 100));
    }

    /// 折线：逐点单击、双击闭合后得到自定义区域，选区是它的外接矩形。
    #[test]
    fn polyline_click_and_double_click_commits_region() {
        let (mut view, _) = region_view(RegionType::Polyline);
        click_at(&mut view, 20, 20, 1);
        click_at(&mut view, 100, 20, 1);
        assert_eq!(view.state, SelectionState::Idle, "还在画");
        assert!(view.region_draft.is_some());
        click_at(&mut view, 60, 100, 1);
        view.handle_mouse_down(PhysicalPoint::new(60, 100), 2);
        let SelectionState::Selected { rect } = view.state else {
            panic!("应已确认选区: {:?}", view.state);
        };
        assert!(
            (rect.x - 20).abs() <= 1
                && (rect.width - 80).abs() <= 2
                && (rect.height - 80).abs() <= 2
        );
        let mask = view.region_mask.as_ref().unwrap();
        assert!(mask.contains(60, 40) && !mask.contains(25, 95));
        assert!(view.region_overlay.is_some());
        assert!(view.region_draft.is_none());
    }

    /// Enter 完成草稿、Backspace 撤销顶点；点数不足时 Enter 丢弃草稿。
    #[test]
    fn enter_finishes_and_backspace_removes_vertex() {
        let (mut view, _) = region_view(RegionType::Curve);
        click_at(&mut view, 20, 20, 1);
        click_at(&mut view, 100, 20, 1);
        click_at(&mut view, 60, 100, 1);
        click_at(&mut view, 10, 90, 1);
        view.handle_keystroke("backspace", false, false, false);
        assert_eq!(view.region_draft.as_ref().unwrap().points().len(), 3);
        view.handle_keystroke("enter", false, false, false);
        assert!(view.region_mask.is_some());

        let (mut few, _) = region_view(RegionType::Polyline);
        click_at(&mut few, 20, 20, 1);
        click_at(&mut few, 50, 50, 1);
        few.handle_keystroke("enter", false, false, false);
        assert!(few.region_draft.is_none() && few.region_mask.is_none());
        assert_eq!(few.state, SelectionState::Idle);
    }

    /// 自由绘制：按住拖一圈、松开即完成。
    #[test]
    fn freehand_drag_and_release_commits_region() {
        let (mut view, _) = region_view(RegionType::Freehand);
        view.handle_mouse_down(PhysicalPoint::new(30, 30), 1);
        view.handle_mouse_move(PhysicalPoint::new(120, 30));
        view.handle_mouse_move(PhysicalPoint::new(120, 110));
        view.handle_mouse_move(PhysicalPoint::new(30, 110));
        assert!(view.handle_mouse_up(PhysicalPoint::new(30, 70)));
        assert!(matches!(view.state, SelectionState::Selected { .. }));
        assert!(view.region_mask.as_ref().unwrap().contains(70, 70));
    }

    /// 导出：区域外（外接矩形内）alpha 为 0，区域内保持不透明。
    #[test]
    fn export_makes_outside_of_region_transparent() {
        let (mut view, rec) = region_view(RegionType::Polyline);
        draw_triangle(&mut view);
        view.apply_action(ToolbarAction::Copy);
        let recorded = rec.borrow();
        let (w, _, rgba) = &recorded.images[0];
        let alpha = |x: usize, y: usize| rgba[(y * *w as usize + x) * 4 + 3];
        let (rx, ry) = (20usize, 20usize);
        assert_eq!(alpha(60 - rx, 40 - ry), 255, "三角形内");
        assert_eq!(alpha(23 - rx, 95 - ry), 0, "外接矩形内、三角形外");
    }

    /// 右键：先取消草稿；再右键关闭。已确认的区域右键清掉，回到选区阶段。
    #[test]
    fn right_click_cancels_draft_then_clears_region() {
        let (mut view, _) = region_view(RegionType::Polyline);
        click_at(&mut view, 20, 20, 1);
        assert_eq!(view.handle_right_click(), OverlayOutcome::Stay);
        assert!(view.region_draft.is_none());
        assert_eq!(view.handle_right_click(), OverlayOutcome::Close);

        draw_triangle(&mut view);
        assert!(view.region_mask.is_some());
        assert_eq!(view.handle_right_click(), OverlayOutcome::Stay);
        assert!(view.region_mask.is_none() && view.region_overlay.is_none());
        assert_eq!(view.state, SelectionState::Idle);
    }

    /// 区域框内按住拖动整体移动，蒙版跟着平移；点框外则整体重置回选区阶段（不立即开始画）。
    #[test]
    fn region_moves_as_a_whole_and_outside_click_resets() {
        let (mut view, _) = region_view(RegionType::Polyline);
        draw_triangle(&mut view);
        view.handle_mouse_down(PhysicalPoint::new(60, 40), 1);
        view.handle_mouse_move(PhysicalPoint::new(70, 45));
        view.handle_mouse_up(PhysicalPoint::new(70, 45));
        let mask = view.region_mask.as_ref().unwrap();
        assert!(mask.contains(70, 45) && !mask.contains(25, 22));
        let SelectionState::Selected { rect } = view.state else {
            panic!("应保持选中");
        };
        assert!((rect.x - 30).abs() <= 1 && (rect.y - 25).abs() <= 1);

        view.handle_mouse_down(PhysicalPoint::new(190, 140), 1);
        assert_eq!(view.state, SelectionState::Idle);
        assert!(view.region_mask.is_none() && view.region_draft.is_none());
    }

    /// 减区域（折线操作数）：从三角形里挖掉一块，挖掉的位置不再属于选区，外接矩形不变。
    #[test]
    fn subtract_polyline_operand_cuts_hole() {
        let (mut view, _) = region_view(RegionType::Polyline);
        draw_triangle(&mut view);
        assert!(view.begin_region_op(RegionOp::Subtract));
        assert_eq!(view.state, SelectionState::Idle, "操作期间回到选区阶段");
        // 在三角形中间画一个小三角形挖掉
        click_at(&mut view, 50, 30, 1);
        click_at(&mut view, 70, 30, 1);
        click_at(&mut view, 60, 45, 1);
        view.handle_mouse_down(PhysicalPoint::new(60, 45), 2);
        assert!(view.region_op.is_none());
        let mask = view.region_mask.as_ref().unwrap();
        assert!(!mask.contains(60, 35), "被挖掉");
        assert!(mask.contains(60, 60), "没挖的部分还在");
        assert!(matches!(view.state, SelectionState::Selected { .. }));
    }

    /// 加区域（矩形操作数，框选）：并入后外接矩形扩大；普通矩形选区也能作为底。
    #[test]
    fn add_rectangle_operand_unions_with_plain_rect_selection() {
        let (mut view, _) = region_view(RegionType::Rectangle);
        drag(&mut view, (10, 10), (60, 50));
        assert!(view.begin_region_op(RegionOp::Add));
        assert!(view.region_mask.is_some(), "普通矩形先转成蒙版");
        drag(&mut view, (80, 30), (140, 90));
        let SelectionState::Selected { rect } = view.state else {
            panic!("应已确认: {:?}", view.state);
        };
        assert!(rect.x <= 10 && rect.right() >= 140 && rect.bottom() >= 90);
        let mask = view.region_mask.as_ref().unwrap();
        assert!(mask.contains(30, 30) && mask.contains(100, 60));
        assert!(!mask.contains(70, 20), "两块之间的空隙不属于选区");
    }

    /// Esc / 右键取消加减区域：本来是普通矩形就还原成矩形，本来是自定义区域就保持原区域。
    #[test]
    fn cancel_region_op_restores_previous_selection() {
        let (mut view, _) = region_view(RegionType::Rectangle);
        drag(&mut view, (10, 10), (60, 50));
        let before = view.state;
        view.begin_region_op(RegionOp::Subtract);
        view.handle_keystroke("escape", false, false, false);
        assert_eq!(view.state, before);
        assert!(view.region_mask.is_none() && view.region_op.is_none());

        let (mut custom, _) = region_view(RegionType::Polyline);
        draw_triangle(&mut custom);
        let region_state = custom.state;
        custom.begin_region_op(RegionOp::Add);
        assert_eq!(custom.handle_right_click(), OverlayOutcome::Stay);
        assert_eq!(custom.state, region_state);
        assert!(custom.region_mask.is_some() && custom.region_op.is_none());
    }

    /// 减掉整块（结果为空）时丢弃操作数，操作保持进行；没有选区或标注中不能开始。
    #[test]
    fn region_op_rejects_empty_result_and_invalid_start() {
        let (mut view, _) = region_view(RegionType::Rectangle);
        assert!(!view.begin_region_op(RegionOp::Add), "没有选区");
        drag(&mut view, (10, 10), (60, 50));
        view.begin_region_op(RegionOp::Subtract);
        drag(&mut view, (0, 0), (150, 140));
        assert!(view.region_op.is_some(), "整块被减空：操作仍在进行");
        assert!(
            view.region_mask.as_ref().is_some_and(|m| !m.is_empty()),
            "底区域没被破坏"
        );
    }

    /// 加减区域期间切换形状：保留操作与底区域，只丢草稿。
    #[test]
    fn switching_type_during_region_op_keeps_the_operation() {
        let (mut view, _) = region_view(RegionType::Polyline);
        draw_triangle(&mut view);
        view.begin_region_op(RegionOp::Add);
        click_at(&mut view, 150, 20, 1);
        assert!(view.switch_region_type(RegionType::Freehand));
        assert!(view.region_op.is_some() && view.region_mask.is_some());
        assert!(view.region_draft.is_none());
        assert!(
            !view.switch_region_type(RegionType::Freehand),
            "形状没变不算切换"
        );
    }

    /// Ctrl+Tab 循环选区形状并写回配置；切换会丢弃当前选区；非矩形形状下智能选区不高亮。
    #[test]
    fn ctrl_tab_cycles_region_type_and_disables_smart_selection() {
        let mut view = hover_view(1.0);
        view.handle_mouse_move(PhysicalPoint::new(60, 50));
        assert!(view.window_highlight().is_some());
        let dir = std::env::temp_dir().join(format!(
            "cisox-region-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| d.as_nanos())
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let config = Rc::new(RefCell::new(ConfigStore::open(dir.join("config.json"))));
        view.set_style_config(config.clone(), "zh-CN");
        view.handle_keystroke("tab", true, false, false);
        assert_eq!(view.region_type(), RegionType::Polyline);
        assert_eq!(
            config
                .borrow()
                .document()
                .value("screenshot_selection/region_type")
                .as_str(),
            Some("polyline")
        );
        view.handle_mouse_move(PhysicalPoint::new(61, 51));
        assert_eq!(view.window_highlight(), None);
        view.handle_keystroke("tab", true, true, false);
        assert_eq!(view.region_type(), RegionType::Rectangle);
        view.handle_mouse_move(PhysicalPoint::new(62, 52));
        assert!(view.window_highlight().is_some(), "回到矩形后恢复智能选区");
        // 切换时丢掉已有选区
        drag(&mut view, (10, 10), (80, 60));
        view.handle_keystroke("tab", true, false, false);
        assert_eq!(view.state, SelectionState::Idle);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 左右并排两块 100x80 的屏（左屏 B=9 / 右屏 B=77 的纯色，便于在导出里分辨来源）：多屏共享视图。
    fn two_monitor_view() -> (ScreenshotOverlayView, Rc<RefCell<Recorded>>) {
        let frame = |shade: u8| {
            FrozenFrame::from_captured(CapturedScreen {
                width: 100,
                height: 80,
                data: [shade, 0, 0, 255].repeat(100 * 80),
            })
            .unwrap()
        };
        let frames = DesktopFrames::new(vec![
            crate::desktop_frames::MonitorFrame {
                rect: PhysicalRect::new(0, 0, 100, 80),
                frame: frame(9),
            },
            crate::desktop_frames::MonitorFrame {
                rect: PhysicalRect::new(100, 0, 100, 80),
                frame: frame(77),
            },
        ])
        .unwrap();
        let rec = Rc::new(RefCell::new(Recorded::default()));
        let sink = RecordingSink {
            rec: Rc::clone(&rec),
            fail: false,
        };
        let view =
            ScreenshotOverlayView::new_multi(frames, 1.0, PhysicalPoint::new(0, 0), Box::new(sink));
        (view, rec)
    }

    /// 窗口局部逻辑坐标换成画布坐标：加上本屏在画布里的原点；指针拖出本窗口（负值）也能换算。
    #[test]
    fn canvas_point_adds_the_monitor_origin() {
        let (view, _) = two_monitor_view();
        assert_eq!(
            view.canvas_point(0, point(px(10.0), px(5.0))),
            PhysicalPoint::new(10, 5)
        );
        assert_eq!(
            view.canvas_point(1, point(px(10.0), px(5.0))),
            PhysicalPoint::new(110, 5)
        );
        assert_eq!(
            view.canvas_point(1, point(px(-30.0), px(5.0))),
            PhysicalPoint::new(70, 5)
        );
        // 越过画布边界会被夹住
        assert_eq!(view.canvas_point(1, point(px(900.0), px(5.0))).x, 199);
    }

    /// 跨屏缝框选：选区横跨两块屏，导出的图左半来自左屏、右半来自右屏。
    #[test]
    fn selection_across_the_seam_exports_both_screens() {
        let (mut view, rec) = two_monitor_view();
        drag(&mut view, (80, 10), (130, 50));
        let SelectionState::Selected { rect } = view.state else {
            panic!("应已确认选区: {:?}", view.state);
        };
        assert_eq!((rect.x, rect.right()), (80, 131));
        view.apply_action(ToolbarAction::Copy);
        let recorded = rec.borrow();
        let (w, _, rgba) = &recorded.images[0];
        let blue = |x: usize| rgba[(5 * *w as usize + x) * 4 + 2];
        assert_eq!((blue(0), blue(19), blue(20), blue(50)), (9, 9, 77, 77));
    }

    /// 选区跨屏时不能录屏 / 长截图（采集窗绑定单屏）；选区在单屏内则正常。
    #[test]
    fn record_and_scroll_refuse_cross_monitor_selection() {
        let (mut view, rec) = two_monitor_view();
        drag(&mut view, (80, 10), (130, 50));
        assert!(view.selection_spans_monitors());
        assert_eq!(
            view.apply_action(ToolbarAction::Record),
            OverlayOutcome::Stay
        );
        assert_eq!(
            view.apply_action(ToolbarAction::ScrollCapture),
            OverlayOutcome::Stay
        );
        assert!(rec.borrow().records.is_empty() && rec.borrow().scrolls.is_empty());

        let (mut single, rec) = two_monitor_view();
        drag(&mut single, (110, 10), (150, 50));
        assert!(!single.selection_spans_monitors());
        assert_eq!(
            single.apply_action(ToolbarAction::Record),
            OverlayOutcome::Close
        );
        assert_eq!(rec.borrow().records.len(), 1);
    }

    /// 标注基底在多屏时才合成整张画布；放大镜跨屏缝取样。
    #[test]
    fn multi_monitor_base_is_lazy_and_magnifier_crosses_the_seam() {
        let (mut view, _) = two_monitor_view();
        assert!(!view.frame.composite_built());
        view.handle_mouse_move(PhysicalPoint::new(100, 40));
        assert!(!view.frame.composite_built(), "只是悬停不合成");
        let grid = view.frame.sample_rgba_grid(100, 40, 3);
        let blue = |col: usize| grid[(3 + col) * 4 + 2];
        assert_eq!((blue(0), blue(1), blue(2)), (9, 77, 77));
        let _ = view.frame.base_view();
        assert!(view.frame.composite_built());
    }

    /// 上一次选区按桌面坐标存取：同样的屏幕排布下能选回；换了排布（画布原点不同）对不上时不乱选。
    #[test]
    fn previous_selection_uses_desktop_coordinates() {
        let dir = std::env::temp_dir().join(format!(
            "cisox-prevsel-origin-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| d.as_nanos())
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("config.json");
        let open = || Rc::new(RefCell::new(ConfigStore::open(&path)));

        let (mut first, _) = two_monitor_view();
        first.set_style_config(open(), "en-US");
        first.set_canvas_origin(PhysicalPoint::new(-1920, 100));
        first.state = SelectionState::Selected {
            rect: PhysicalRect::new(10, 10, 50, 40),
        };
        first.remember_selection();

        let (mut same, _) = two_monitor_view();
        same.set_style_config(open(), "en-US");
        same.set_canvas_origin(PhysicalPoint::new(-1920, 100));
        assert!(same.select_previous_selection());
        assert_eq!(
            same.state,
            SelectionState::Selected {
                rect: PhysicalRect::new(10, 10, 50, 40)
            }
        );

        let (mut moved, _) = two_monitor_view();
        moved.set_style_config(open(), "en-US");
        assert!(
            !moved.select_previous_selection(),
            "原点不同：存的桌面坐标落在画布外"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 导出时记下选区；新一次截图里用快捷键动作就能选回同一块区域。
    #[test]
    fn previous_selection_is_saved_on_export_and_restored() {
        let dir = std::env::temp_dir().join(format!(
            "cisox-prevsel-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| d.as_nanos())
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("config.json");

        let (mut first, _) = view_with(300, 200, 1.0, false);
        first.set_style_config(Rc::new(RefCell::new(ConfigStore::open(&path))), "en-US");
        first.state = SelectionState::Selected {
            rect: PhysicalRect::new(20, 30, 100, 80),
        };
        first.remember_selection();

        let (mut second, _) = view_with(300, 200, 1.0, false);
        second.set_style_config(Rc::new(RefCell::new(ConfigStore::open(&path))), "en-US");
        assert!(second.select_previous_selection());
        assert_eq!(
            second.state,
            SelectionState::Selected {
                rect: PhysicalRect::new(20, 30, 100, 80)
            }
        );

        // 没有保存值（或在标注 / 拖动中）时返回 false
        let (mut empty, _) = view_with(300, 200, 1.0, false);
        assert!(!empty.select_previous_selection());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 没有智能选区来源或非空闲态时，滚轮不被消费。
    #[test]
    fn wheel_ignored_without_source_or_when_not_idle() {
        let (mut plain, _) = view_with(300, 200, 1.0, false);
        assert!(!plain.handle_scroll(1.0, PhysicalPoint::new(5, 5)));
        let mut view = layered_view();
        view.handle_mouse_move(PhysicalPoint::new(65, 55));
        view.state = SelectionState::Selected {
            rect: PhysicalRect::new(0, 0, 50, 50),
        };
        assert!(!view.handle_scroll(1.0, PhysicalPoint::new(65, 55)));
    }

    /// 快捷键切换目标：窗口目标直接高亮顶层窗口，再切回子控件；无配置时不 panic。
    #[test]
    fn toggle_target_switches_layer() {
        let mut view = layered_view();
        let p = PhysicalPoint::new(65, 55);
        view.handle_mouse_move(p);
        assert!(view.toggle_selection_target());
        assert_eq!(
            view.window_highlight(),
            Some(PhysicalRect::new(40, 30, 150, 120))
        );
        assert!(view.toggle_selection_target());
        assert_eq!(
            view.window_highlight(),
            Some(PhysicalRect::new(60, 50, 20, 15))
        );
    }

    /// 带假窗口来源（窗口 40,30 100x80）的视图。
    fn hover_view(scale: f32) -> ScreenshotOverlayView {
        let (mut view, _) = view_with(300, 200, scale, false);
        view.set_window_hover(
            Some(Box::new(FakeHover(PhysicalRect::new(40, 30, 100, 80)))),
            PickTarget::WindowSubElement,
            false,
        );
        view
    }

    /// 空闲时悬停高亮窗口，移出窗口后高亮消失。
    #[test]
    fn idle_hover_highlights_window() {
        let mut view = hover_view(1.0);
        view.handle_mouse_move(PhysicalPoint::new(60, 50));
        assert_eq!(
            view.window_highlight(),
            Some(PhysicalRect::new(40, 30, 100, 80))
        );
        view.handle_mouse_move(PhysicalPoint::new(250, 150));
        assert_eq!(view.window_highlight(), None);
    }

    /// 单击（位移小于阈值）直接把窗口矩形作为选区。
    #[test]
    fn click_selects_hovered_window() {
        let mut view = hover_view(1.0);
        view.handle_mouse_move(PhysicalPoint::new(60, 50));
        drag(&mut view, (60, 50), (64, 53));
        assert_eq!(
            view.state,
            SelectionState::Selected {
                rect: PhysicalRect::new(40, 30, 100, 80)
            }
        );
        assert_eq!(view.window_highlight(), None);
    }

    /// 位移超过阈值转为手动框选，行为与原来一致。
    #[test]
    fn drag_past_threshold_falls_back_to_marquee() {
        let mut view = hover_view(1.0);
        drag(&mut view, (60, 50), (120, 100));
        assert_eq!(
            view.state,
            SelectionState::Selected {
                rect: PhysicalRect::new(60, 50, 61, 51)
            }
        );
    }

    /// 拖出阈值后又回到起点附近，仍按手动框选处理（不再变回窗口选区）。
    #[test]
    fn leaving_threshold_is_sticky() {
        let mut view = hover_view(1.0);
        view.handle_mouse_down(PhysicalPoint::new(60, 50), 1);
        view.handle_mouse_move(PhysicalPoint::new(90, 50));
        view.handle_mouse_move(PhysicalPoint::new(61, 50));
        view.handle_mouse_up(PhysicalPoint::new(61, 50));
        assert_eq!(view.state, SelectionState::Idle);
    }

    /// 阈值随缩放比放大：2x 下位移 15 物理像素仍算单击。
    #[test]
    fn drag_threshold_scales_with_dpi() {
        let mut view = hover_view(2.0);
        drag(&mut view, (60, 50), (75, 50));
        assert!(matches!(view.state, SelectionState::Selected { .. }));
        let mut view = hover_view(2.0);
        drag(&mut view, (60, 50), (80, 50));
        assert_eq!(view.state, SelectionState::Idle);
    }

    /// 窗口外单击没有窗口可选，沿用原来的微小框选丢弃逻辑。
    #[test]
    fn click_without_window_is_discarded() {
        let mut view = hover_view(1.0);
        drag(&mut view, (250, 150), (251, 150));
        assert_eq!(view.state, SelectionState::Idle);
    }

    /// 窗口选区后右键先撤销选区，再右键关闭（取消行为不变）。
    #[test]
    fn right_click_cancels_window_selection() {
        let mut view = hover_view(1.0);
        drag(&mut view, (60, 50), (60, 50));
        assert!(matches!(view.state, SelectionState::Selected { .. }));
        assert_eq!(view.handle_right_click(), OverlayOutcome::Stay);
        assert_eq!(view.state, SelectionState::Idle);
        // 回到智能选区：光标仍在窗口内，立刻重新高亮该窗口（对齐 Qt）
        assert_eq!(
            view.window_highlight(),
            Some(PhysicalRect::new(40, 30, 100, 80))
        );
        assert_eq!(view.handle_right_click(), OverlayOutcome::Close);
    }

    /// 框选状态流转：按下、移动、松开后得到固定选区。
    #[test]
    fn overlay_view_state_flow() {
        let (mut view, _) = view_with(200, 150, 1.0, false);
        assert_eq!(view.current_selection(), None);
        view.handle_mouse_down(PhysicalPoint::new(20, 20), 1);
        assert!(matches!(view.state, SelectionState::MarqueeDragging { .. }));
        view.handle_mouse_move(PhysicalPoint::new(120, 90));
        assert_eq!(
            view.current_selection(),
            Some(PhysicalRect::new(20, 20, 101, 71))
        );
        view.handle_mouse_up(PhysicalPoint::new(120, 90));
        assert!(matches!(view.state, SelectionState::Selected { .. }));
        assert_eq!(
            view.current_selection(),
            Some(PhysicalRect::new(20, 20, 101, 71))
        );
    }

    /// 太小的框选被丢弃回到空闲态。
    #[test]
    fn tiny_marquee_is_discarded() {
        let (mut view, _) = view_with(200, 150, 1.0, false);
        drag(&mut view, (50, 50), (53, 52));
        assert_eq!(view.state, SelectionState::Idle);
    }

    /// 拖出窗口的点被钳制在底图内，选区不会越界。
    #[test]
    fn drag_outside_is_clamped() {
        let (mut view, _) = view_with(200, 150, 1.0, false);
        drag(&mut view, (100, 100), (900, -300));
        let sel = view.current_selection().unwrap();
        assert!(
            view.screen_bounds.intersect(&sel) == Some(sel),
            "选区越界: {sel:?}"
        );
        assert_eq!(sel.right(), 200);
        assert_eq!(sel.y, 0);
    }

    /// 选区内部拖动整体移动，并被屏幕边界钳制。
    #[test]
    fn moving_selection_is_bounded() {
        let (mut view, _) = view_with(200, 150, 1.0, false);
        drag(&mut view, (20, 20), (80, 60));
        let before = view.current_selection().unwrap();
        // 在选区中心按下并向右下拖很远
        view.handle_mouse_down(PhysicalPoint::new(50, 40), 1);
        assert!(matches!(
            view.state,
            SelectionState::Reshaping {
                mode: SelectionDragMode::All,
                ..
            }
        ));
        view.handle_mouse_move(PhysicalPoint::new(199, 149));
        let live = view.current_selection().unwrap();
        assert_eq!((live.width, live.height), (before.width, before.height));
        assert!(live.right() <= 200 && live.bottom() <= 150);
        view.handle_mouse_up(PhysicalPoint::new(199, 149));
        assert_eq!(view.state, SelectionState::Selected { rect: live });
    }

    /// 拖右下角手柄缩放选区。
    #[test]
    fn resizing_by_handle() {
        let (mut view, _) = view_with(300, 200, 1.0, false);
        drag(&mut view, (50, 50), (150, 120));
        let sel = view.current_selection().unwrap();
        let corner = PhysicalPoint::new(sel.right() - 1, sel.bottom() - 1);
        view.handle_mouse_down(corner, 1);
        assert!(matches!(
            view.state,
            SelectionState::Reshaping {
                mode: SelectionDragMode::BottomRight,
                ..
            }
        ));
        view.handle_mouse_move(PhysicalPoint::new(corner.x + 40, corner.y + 30));
        view.handle_mouse_up(PhysicalPoint::new(corner.x + 40, corner.y + 30));
        let after = view.current_selection().unwrap();
        assert_eq!((after.x, after.y), (sel.x, sel.y));
        assert_eq!(
            (after.width, after.height),
            (sel.width + 40, sel.height + 30)
        );
    }

    /// 点击选区外重新开始框选。
    #[test]
    fn click_outside_restarts_marquee() {
        let (mut view, _) = view_with(300, 200, 1.0, false);
        drag(&mut view, (20, 20), (80, 80));
        view.handle_mouse_down(PhysicalPoint::new(250, 180), 1);
        assert!(matches!(view.state, SelectionState::MarqueeDragging { .. }));
    }

    /// 按住 Shift 框选得到正方形；不按则是自由矩形。
    #[test]
    fn shift_marquee_is_square() {
        let (mut view, _) = view_with(300, 200, 1.0, false);
        view.set_shift(true);
        drag(&mut view, (20, 20), (120, 70));
        let sel = view.current_selection().unwrap();
        assert_eq!(sel.width, sel.height, "Shift 锁定为正方形");
        assert!(sel.width >= 100);

        let (mut free, _) = view_with(300, 200, 1.0, false);
        free.set_shift(false);
        drag(&mut free, (20, 20), (120, 70));
        let sel = free.current_selection().unwrap();
        assert_ne!(sel.width, sel.height);
    }

    /// 配置没有把「保持宽高一致」绑在 Shift 上时，按 Shift 不锁比例。
    #[test]
    fn shift_does_not_lock_when_unbound() {
        let (mut view, _) = view_with(300, 200, 1.0, false);
        let mut doc = snow_config::document::ConfigDocument::from_bytes(None);
        doc.set_value(
            "screenshot_shortcuts/keep_selection_width_and_height_consistent",
            serde_json::json!([{"portable": "Ctrl+K"}]),
        )
        .unwrap();
        view.set_keymap(OverlayKeymap::from_document(&doc));
        view.set_shift(true);
        drag(&mut view, (20, 20), (120, 70));
        let sel = view.current_selection().unwrap();
        assert_ne!(sel.width, sel.height);
    }

    /// 框选拖动中按住移动键：整个框平移（尺寸不变），松开键后恢复框选。
    #[test]
    fn move_key_translates_the_marquee() {
        let (mut view, _) = view_with(300, 200, 1.0, false);
        view.handle_mouse_down(PhysicalPoint::new(20, 20), 1);
        view.handle_mouse_move(PhysicalPoint::new(60, 50));
        let before = view.current_selection().unwrap();
        assert_eq!(
            view.handle_keystroke("space", false, false, false),
            OverlayOutcome::Stay
        );
        view.handle_mouse_move(PhysicalPoint::new(100, 90));
        let moved = view.current_selection().unwrap();
        assert_eq!((moved.width, moved.height), (before.width, before.height));
        assert_eq!((moved.x - before.x, moved.y - before.y), (40, 40));
        view.handle_key_release("space", false, false, false);
        view.handle_mouse_move(PhysicalPoint::new(130, 110));
        let resized = view.current_selection().unwrap();
        assert!(resized.width > moved.width, "松开后继续框选");
    }

    /// Alt+R 重新截图：关闭覆盖窗、置位重截标志，不写任何输出。
    #[test]
    fn recapture_sets_flag_and_closes() {
        let (mut view, rec) = view_with(100, 80, 1.0, false);
        let flag = view.recapture_flag();
        assert!(!flag.get());
        assert_eq!(
            view.handle_keystroke("r", false, false, true),
            OverlayOutcome::Close
        );
        assert!(flag.get());
        let r = rec.borrow();
        assert!(r.images.is_empty() && r.saved.is_empty());
    }

    /// Ctrl+P 切换坐标显示模式，来回切换；没有选区时 Ctrl+Shift+S 快速保存只提示不保存。
    #[test]
    fn coordinate_mode_toggles_and_quick_save_needs_selection() {
        let (mut view, rec) = view_with(100, 80, 1.0, false);
        assert!(view.coordinate_global, "默认是全局坐标");
        assert_eq!(
            view.handle_keystroke("p", true, false, false),
            OverlayOutcome::Stay
        );
        assert!(!view.coordinate_global);
        view.handle_keystroke("p", true, false, false);
        assert!(view.coordinate_global);
        assert_eq!(
            view.handle_keystroke("s", true, true, false),
            OverlayOutcome::Stay
        );
        assert!(rec.borrow().saved.is_empty());
    }

    /// Esc 关闭，不写任何输出。
    #[test]
    fn escape_closes_without_output() {
        let (mut view, rec) = view_with(100, 80, 1.0, false);
        drag(&mut view, (10, 10), (60, 50));
        assert_eq!(
            view.handle_key("escape", false, false),
            OverlayOutcome::Close
        );
        let r = rec.borrow();
        assert!(r.images.is_empty() && r.texts.is_empty() && r.saved.is_empty());
    }

    /// Enter 把选区裁成 RGBA 写入剪贴板并关闭。
    #[test]
    fn enter_copies_cropped_rgba() {
        let (mut view, rec) = view_with(100, 80, 1.0, false);
        drag(&mut view, (10, 20), (29, 39));
        assert_eq!(
            view.handle_key("enter", false, false),
            OverlayOutcome::Close
        );
        let r = rec.borrow();
        let (w, h, rgba) = &r.images[0];
        assert_eq!((*w, *h), (20, 20));
        assert_eq!(&rgba[0..4], &[10, 20, 9, 255]);
        let last = (w * h - 1) as usize * 4;
        assert_eq!(&rgba[last..last + 4], &[29, 39, 9, 255]);
    }

    /// 没有选区时 Enter / 复制 / 保存都不关闭窗口，只给提示。
    #[test]
    fn copy_and_save_need_selection() {
        let (mut view, rec) = view_with(100, 80, 1.0, false);
        assert_eq!(view.handle_key("enter", false, false), OverlayOutcome::Stay);
        assert_eq!(view.apply_action(ToolbarAction::Copy), OverlayOutcome::Stay);
        assert_eq!(view.apply_action(ToolbarAction::Save), OverlayOutcome::Stay);
        assert!(view.status_message.is_some());
        let r = rec.borrow();
        assert!(r.images.is_empty() && r.saved.is_empty());
    }

    /// 工具栏复制 / 保存 / 取消真的走到输出通道。
    #[test]
    fn toolbar_actions_reach_sink() {
        let (mut view, rec) = view_with(100, 80, 1.0, false);
        drag(&mut view, (5, 5), (44, 34));
        assert_eq!(
            view.apply_action(ToolbarAction::Save),
            OverlayOutcome::Close
        );
        assert_eq!(
            view.apply_action(ToolbarAction::Copy),
            OverlayOutcome::Close
        );
        assert_eq!(
            view.apply_action(ToolbarAction::Cancel),
            OverlayOutcome::Close
        );
        let r = rec.borrow();
        assert_eq!(r.saved, vec![(40, 30)]);
        assert_eq!(r.images.len(), 1);
    }

    /// 未接入的动作不产生任何副作用。
    #[test]
    fn unimplemented_actions_are_inert() {
        let (mut view, rec) = view_with(100, 80, 1.0, false);
        drag(&mut view, (5, 5), (44, 34));
        for action in [ToolbarAction::Undo, ToolbarAction::Redo] {
            assert_eq!(view.apply_action(action), OverlayOutcome::Stay);
        }
        let r = rec.borrow();
        assert!(r.images.is_empty() && r.texts.is_empty() && r.saved.is_empty());
    }

    /// 工具栏“贴图”把选区原位（含尺寸与像素）交给输出通道，并关闭覆盖窗；没有选区时不触发。
    #[test]
    fn pin_action_sends_selection_in_place() {
        let (mut view, rec) = view_with(100, 80, 1.0, false);
        assert_eq!(view.apply_action(ToolbarAction::Pin), OverlayOutcome::Stay);
        assert!(rec.borrow().pins.is_empty());
        drag(&mut view, (5, 5), (44, 34));
        assert_eq!(view.apply_action(ToolbarAction::Pin), OverlayOutcome::Close);
        let r = rec.borrow();
        assert_eq!(r.pins.len(), 1);
        let (rect, w, h, rgba) = &r.pins[0];
        assert_eq!((*w, *h), (rect.width as u32, rect.height as u32));
        assert_eq!(rgba.len(), (*w * *h * 4) as usize);
        // 选区左上角像素：底图像素 r = x, g = y, b = 9
        assert_eq!(&rgba[0..3], &[rect.x as u8, rect.y as u8, 9]);
    }

    /// 贴图携带二次标注：贴图像素与复制 / 保存使用同一合成结果（含标注）。
    #[test]
    fn pin_action_includes_annotations() {
        let (mut view, rec) = view_with(160, 120, 1.0, false);
        drag(&mut view, (10, 10), (149, 109));
        view.select_tool(AnnotationTool::Rectangle);
        view.start_annotation(
            PhysicalPoint::new(30, 30),
            view.current_selection().unwrap(),
        );
        view.finish_annotation(PhysicalPoint::new(120, 90));
        assert_eq!(view.apply_action(ToolbarAction::Pin), OverlayOutcome::Close);
        let r = rec.borrow();
        let (_, w, h, pinned) = &r.pins[0];
        let plain = view
            .frame
            .crop_rgba(view.current_selection().unwrap())
            .unwrap()
            .2;
        assert_eq!(pinned.len(), (*w * *h * 4) as usize);
        assert_ne!(pinned, &plain, "贴图像素应包含标注");
    }

    /// 贴图失败时窗口保留并提示。
    #[test]
    fn pin_failure_keeps_window() {
        let (mut view, _) = view_with(100, 80, 1.0, true);
        drag(&mut view, (5, 5), (44, 34));
        assert_eq!(view.apply_action(ToolbarAction::Pin), OverlayOutcome::Stay);
        assert!(
            view.status_message
                .as_deref()
                .unwrap()
                .contains("Pin failed")
        );
    }

    /// 输出失败时窗口保留并给出错误提示。
    #[test]
    fn output_failure_keeps_window() {
        let (mut view, _) = view_with(100, 80, 1.0, true);
        drag(&mut view, (5, 5), (44, 34));
        assert_eq!(view.handle_key("enter", false, false), OverlayOutcome::Stay);
        assert!(
            view.status_message
                .as_deref()
                .unwrap()
                .contains("Copy failed")
        );
        assert_eq!(view.apply_action(ToolbarAction::Save), OverlayOutcome::Stay);
        assert!(view.status_message.as_deref().unwrap().contains("boom"));
    }

    /// 双击选区内部等同复制并关闭。
    #[test]
    fn double_click_inside_copies() {
        let (mut view, rec) = view_with(200, 150, 1.0, false);
        drag(&mut view, (20, 20), (120, 100));
        let outcome = view.handle_mouse_down(PhysicalPoint::new(70, 60), 2);
        assert_eq!(outcome, OverlayOutcome::Close);
        assert_eq!(rec.borrow().images.len(), 1);
    }

    /// 双击 / 中键的动作跟随配置；颜色与尺寸单位配置也被读取。
    #[test]
    fn click_actions_and_colors_follow_config() {
        let dir = std::env::temp_dir().join(format!("snow-click-cfg-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let mut store = ConfigStore::open(dir.join("config.json"));
        store
            .set_value(DOUBLE_CLICK_ACTION_KEY, serde_json::json!("none"))
            .unwrap();
        store
            .set_value(MIDDLE_CLICK_ACTION_KEY, serde_json::json!("quick_save"))
            .unwrap();
        store
            .set_value(SELECTION_BORDER_COLOR_KEY, serde_json::json!("#FF0000FF"))
            .unwrap();
        store
            .set_value(SELECTION_MASK_COLOR_KEY, serde_json::json!("#11223344"))
            .unwrap();
        store
            .set_value(SELECTION_UNIT_KEY, serde_json::json!("logical_pixels"))
            .unwrap();
        let (mut view, rec) = view_with(200, 150, 2.0, false);
        view.set_style_config(Rc::new(RefCell::new(store)), "en-US");
        assert_eq!(view.double_click_action, ClickAction::None);
        assert_eq!(view.middle_click_action, ClickAction::QuickSave);
        assert_eq!(
            (view.border_color, view.mask_color),
            (0xFF0000FF, 0x11223344)
        );
        assert!(view.logical_size_label);
        // 双击不再复制
        drag(&mut view, (20, 20), (120, 100));
        assert_eq!(
            view.handle_mouse_down(PhysicalPoint::new(70, 60), 2),
            OverlayOutcome::Stay
        );
        assert!(rec.borrow().images.is_empty());
        // 逻辑像素标签：缩放 2 倍时数值减半
        let physical = view.size_label(PhysicalRect::new(0, 0, 100, 60));
        assert_ne!(
            physical,
            selection_size_label(PhysicalRect::new(0, 0, 100, 60))
        );
        assert_eq!(ClickAction::parse("pin"), Some(ClickAction::Pin));
        assert_eq!(ClickAction::parse("bogus"), None);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 取色格式、放大镜显示模式与调整选区方式跟随配置。
    #[test]
    fn picker_and_resize_settings_follow_config() {
        let dir = std::env::temp_dir().join(format!("snow-picker-cfg-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let mut store = ConfigStore::open(dir.join("config.json"));
        store
            .set_value(COLOR_FORMAT_KEY, serde_json::json!("hex_without_hash"))
            .unwrap();
        store
            .set_value(COLOR_PICKER_MODE_KEY, serde_json::json!("always_hide"))
            .unwrap();
        store
            .set_value(RESIZE_MODE_KEY, serde_json::json!("follow_mouse_position"))
            .unwrap();
        let (mut view, rec) = view_with(100, 80, 1.0, false);
        view.set_style_config(Rc::new(RefCell::new(store)), "en-US");
        assert!(view.magnifier_hidden && view.resize_follow_position);
        view.handle_mouse_move(PhysicalPoint::new(0x12, 0x34));
        view.handle_key("c", false, false);
        assert_eq!(rec.borrow().texts, vec!["123409".to_string()]);
        // 抓右下角（鼠标在角内偏 2 像素）：右下边直接落到鼠标所在像素
        drag(&mut view, (10, 10), (50, 40));
        let sel = view.current_selection().unwrap();
        let grab = PhysicalPoint::new(sel.right() - 3, sel.bottom() - 3);
        view.handle_mouse_down(grab, 1);
        assert!(matches!(
            view.state,
            SelectionState::Reshaping {
                mode: SelectionDragMode::BottomRight,
                ..
            }
        ));
        let live = view.current_selection().unwrap();
        assert_eq!((live.right(), live.bottom()), (grab.x + 1, grab.y + 1));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 被抓的边落在鼠标像素上（纯函数）。
    #[test]
    fn grab_adjusted_rect_snaps_edges() {
        let rect = PhysicalRect::new(10, 10, 40, 30);
        let adjusted = grab_adjusted_rect(
            SelectionDragMode::BottomRight,
            rect,
            PhysicalPoint::new(46, 36),
        );
        assert_eq!(
            (adjusted.x, adjusted.y, adjusted.right(), adjusted.bottom()),
            (10, 10, 47, 37)
        );
        let adjusted =
            grab_adjusted_rect(SelectionDragMode::Left, rect, PhysicalPoint::new(12, 20));
        assert_eq!((adjusted.x, adjusted.right()), (12, 50));
        assert_eq!(
            grab_adjusted_rect(SelectionDragMode::All, rect, PhysicalPoint::new(0, 0)),
            rect
        );
    }

    /// C 键复制光标处像素的颜色值（不关闭窗口）。
    #[test]
    fn c_key_copies_color_under_cursor() {
        let (mut view, rec) = view_with(100, 80, 1.0, false);
        view.handle_mouse_move(PhysicalPoint::new(0x12, 0x34));
        assert_eq!(view.handle_key("c", false, false), OverlayOutcome::Stay);
        assert_eq!(rec.borrow().texts, vec!["#123409".to_string()]);
        assert_eq!(view.magnifier_grid.center_pixel(), (0x12, 0x34, 9, 255));
    }

    /// 右键：先撤销选区，再次右键才关闭。
    #[test]
    fn right_click_clears_then_closes() {
        let (mut view, _) = view_with(100, 80, 1.0, false);
        drag(&mut view, (5, 5), (44, 34));
        assert_eq!(view.handle_right_click(), OverlayOutcome::Stay);
        assert_eq!(view.state, SelectionState::Idle);
        assert_eq!(view.handle_right_click(), OverlayOutcome::Close);
    }

    /// 逻辑坐标按缩放比换算为物理坐标并钳制在底图内。
    #[test]
    fn logical_to_physical_uses_scale() {
        let (view, _) = view_with(300, 200, 1.5, false);
        let p = view.physical_point(point(px(100.0), px(50.0)));
        assert_eq!((p.x, p.y), (150, 75));
        let far = view.physical_point(point(px(9999.0), px(-5.0)));
        assert_eq!((far.x, far.y), (299, 0));
    }

    /// DPI 缩放下边缘命中容差随缩放比放大。
    #[test]
    fn edge_tolerance_scales_with_dpi() {
        let (v1, _) = view_with(100, 80, 1.0, false);
        let (v2, _) = view_with(100, 80, 2.0, false);
        assert_eq!(v1.edge_tolerance(), 6);
        assert_eq!(v2.edge_tolerance(), 12);
    }

    /// 非法缩放比回落为 1.0。
    #[test]
    fn invalid_scale_falls_back() {
        let (view, _) = view_with(10, 10, f32::NAN, false);
        assert_eq!(view.scale, 1.0);
        let (view, _) = view_with(10, 10, -2.0, false);
        assert_eq!(view.scale, 1.0);
    }

    /// 指针样式随拖拽模式变化。
    #[test]
    fn cursor_styles_follow_mode() {
        assert_eq!(
            cursor_for_mode(SelectionDragMode::None, false),
            CursorStyle::Crosshair
        );
        assert_eq!(
            cursor_for_mode(SelectionDragMode::All, false),
            CursorStyle::OpenHand
        );
        assert_eq!(
            cursor_for_mode(SelectionDragMode::All, true),
            CursorStyle::ClosedHand
        );
        assert_eq!(
            cursor_for_mode(SelectionDragMode::Top, false),
            CursorStyle::ResizeUpDown
        );
        assert_eq!(
            cursor_for_mode(SelectionDragMode::TopLeft, false),
            CursorStyle::ResizeUpLeftDownRight
        );
        assert_eq!(
            cursor_for_mode(SelectionDragMode::BottomLeft, false),
            CursorStyle::ResizeUpRightDownLeft
        );
    }

    /// 悬停在手柄上时记录命中模式。
    #[test]
    fn hover_mode_tracks_handles() {
        let (mut view, _) = view_with(300, 200, 1.0, false);
        drag(&mut view, (50, 50), (150, 120));
        view.handle_mouse_move(PhysicalPoint::new(50, 50));
        assert_eq!(view.hover_mode, SelectionDragMode::TopLeft);
        view.handle_mouse_move(PhysicalPoint::new(100, 80));
        assert_eq!(view.hover_mode, SelectionDragMode::All);
        view.handle_mouse_move(PhysicalPoint::new(280, 190));
        assert_eq!(view.hover_mode, SelectionDragMode::None);
    }

    /// 基准步进：首步按下、末步松开，得到固定选区。
    #[test]
    fn bench_steps_end_with_selection() {
        let (mut view, _) = view_with(400, 300, 1.0, false);
        for i in 0..50 {
            view.bench_step(i, 50);
        }
        assert!(
            matches!(view.state, SelectionState::Selected { .. })
                || view.state == SelectionState::Idle
        );
        assert_eq!(view.probe.summary().2.count, 50);
    }

    /// 自动动作：框选松手后只触发一次；未设置或无选区时不触发。
    #[test]
    fn auto_confirm_copies_once_after_selection() {
        let (mut view, rec) = view_with(255, 200, 1.0, false);
        view.set_auto_confirm(Some(AutoConfirm::Copy));
        assert_eq!(
            view.auto_confirm_outcome(),
            OverlayOutcome::Stay,
            "无选区不触发"
        );
        drag(&mut view, (50, 50), (150, 120));
        assert_eq!(view.auto_confirm_outcome(), OverlayOutcome::Close);
        assert_eq!(rec.borrow().images.len(), 1);
        assert_eq!(
            view.auto_confirm_outcome(),
            OverlayOutcome::Stay,
            "只触发一次"
        );
        let (mut plain, rec) = view_with(255, 200, 1.0, false);
        drag(&mut plain, (50, 50), (150, 120));
        assert_eq!(plain.auto_confirm_outcome(), OverlayOutcome::Stay);
        assert!(rec.borrow().images.is_empty());
    }

    /// 自动动作与工具栏动作一一对应。
    #[test]
    fn auto_confirm_maps_to_toolbar_actions() {
        assert_eq!(AutoConfirm::Copy.toolbar_action(), ToolbarAction::Copy);
        assert_eq!(AutoConfirm::Pin.toolbar_action(), ToolbarAction::Pin);
        assert_eq!(AutoConfirm::Ocr.toolbar_action(), ToolbarAction::Ocr);
        assert_eq!(
            AutoConfirm::Translate.toolbar_action(),
            ToolbarAction::Translate
        );
    }

    /// 录屏选区模式：Enter 把选区交给录制流程并关闭；复制 / 保存快捷键不起作用。
    #[test]
    fn record_mode_enter_starts_recording() {
        let (mut view, rec) = view_with(255, 200, 1.0, false);
        view.set_record_mode(true);
        assert_eq!(
            view.handle_key("enter", false, false),
            OverlayOutcome::Stay,
            "无选区先提示"
        );
        drag(&mut view, (50, 50), (150, 120));
        assert_eq!(view.handle_key("c", true, false), OverlayOutcome::Stay);
        assert_eq!(view.handle_key("s", true, false), OverlayOutcome::Stay);
        assert!(rec.borrow().images.is_empty() && rec.borrow().saved.is_empty());
        assert_eq!(
            view.handle_key("enter", false, false),
            OverlayOutcome::Close
        );
        assert_eq!(
            rec.borrow().records,
            vec![PhysicalRect::new(50, 50, 101, 71)]
        );
    }

    /// 工具栏“录屏”动作在截图模式下同样可用；输出失败时保留窗口。
    #[test]
    fn toolbar_record_action_and_failure() {
        let (mut view, rec) = view_with(255, 200, 1.0, false);
        drag(&mut view, (10, 10), (100, 100));
        assert_eq!(
            view.apply_action(ToolbarAction::Record),
            OverlayOutcome::Close
        );
        assert_eq!(rec.borrow().records.len(), 1);
        let (mut failing, _) = view_with(255, 200, 1.0, true);
        drag(&mut failing, (10, 10), (100, 100));
        assert_eq!(
            failing.apply_action(ToolbarAction::Record),
            OverlayOutcome::Stay
        );
    }

    /// 用当前工具在 `from`-`to` 之间拖动一次（按下 / 移动 / 松开）。
    fn annotate(view: &mut ScreenshotOverlayView, from: (i32, i32), to: (i32, i32)) {
        view.handle_mouse_down(PhysicalPoint::new(from.0, from.1), 1);
        for i in 1..=6 {
            let t = i as f32 / 6.0;
            let x = from.0 + ((to.0 - from.0) as f32 * t) as i32;
            let y = from.1 + ((to.1 - from.1) as f32 * t) as i32;
            view.handle_mouse_move(PhysicalPoint::new(x, y));
        }
        view.handle_mouse_up(PhysicalPoint::new(to.0, to.1));
    }

    /// 选中工具后在选区内拖动是标注：选区不变、产生预览分块。
    #[test]
    fn tool_drag_annotates_instead_of_moving_selection() {
        let (mut view, _) = view_with(300, 200, 1.0, false);
        drag(&mut view, (20, 20), (219, 149));
        let sel = view.current_selection().unwrap();
        view.select_tool(AnnotationTool::Rectangle);
        assert_eq!(view.current_tool(), AnnotationTool::Rectangle);
        annotate(&mut view, (60, 50), (160, 110));
        assert_eq!(view.current_selection(), Some(sel), "选区不应被拖动");
        assert!(view.tile_sprite_count() > 0);
        assert!(!view.annotating);
        assert!(view.history_state().0);
    }

    /// 无工具时选区内拖动仍是移动选区（回归）。
    #[test]
    fn no_tool_drag_still_moves_selection() {
        let (mut view, _) = view_with(300, 200, 1.0, false);
        drag(&mut view, (20, 20), (119, 99));
        let before = view.current_selection().unwrap();
        annotate(&mut view, (60, 50), (100, 80));
        let after = view.current_selection().unwrap();
        assert_eq!((after.x, after.y), (before.x + 40, before.y + 30));
        assert_eq!(view.tile_sprite_count(), 0);
    }

    /// 再次点击同一工具取消工具；点选手柄 / 边缘仍是缩放选区而不是标注。
    #[test]
    fn tool_toggle_and_handles_keep_resizing() {
        let (mut view, _) = view_with(300, 200, 1.0, false);
        drag(&mut view, (50, 50), (150, 120));
        view.select_tool(AnnotationTool::Arrow);
        view.select_tool(AnnotationTool::Arrow);
        assert_eq!(view.current_tool(), AnnotationTool::None);
        view.select_tool(AnnotationTool::Arrow);
        let sel = view.current_selection().unwrap();
        let corner = PhysicalPoint::new(sel.right() - 1, sel.bottom() - 1);
        view.handle_mouse_down(corner, 1);
        assert!(matches!(view.state, SelectionState::Reshaping { .. }));
        assert!(!view.annotating);
    }

    /// 复制带标注：导出图像里选区内的标注描边像素为红色，选区外的标注不会出现。
    #[test]
    fn copy_includes_annotation() {
        let (mut view, rec) = view_with(300, 200, 1.0, false);
        drag(&mut view, (20, 20), (219, 149));
        view.select_tool(AnnotationTool::Rectangle);
        annotate(&mut view, (60, 50), (160, 110));
        assert_eq!(
            view.handle_key("enter", false, false),
            OverlayOutcome::Close
        );
        let r = rec.borrow();
        let (w, h, rgba) = &r.images[0];
        assert_eq!((*w, *h), (200, 130));
        // 矩形左边线（底图坐标 x=60）落在裁切图 x=40；取中部 y=80 -> 60
        let o = ((60 * *w + 40) * 4) as usize;
        let p = &rgba[o..o + 4];
        assert!(p[0] > 200 && p[1] < 90 && p[2] < 90, "左边线像素 {p:?}");
        // 矩形内部保持底图渐变（r = x, g = y）
        let o = ((60 * *w + 100) * 4) as usize;
        assert_eq!(&rgba[o..o + 3], &[120, 80, 9]);
    }

    /// 标注被钳制在选区内：从选区内拖到选区外，元素不越界。
    #[test]
    fn annotation_is_clamped_to_selection() {
        let (mut view, rec) = view_with(300, 200, 1.0, false);
        drag(&mut view, (40, 40), (139, 119));
        view.select_tool(AnnotationTool::Rectangle);
        annotate(&mut view, (60, 60), (290, 190));
        view.handle_key("enter", false, false);
        let r = rec.borrow();
        let (w, h, rgba) = &r.images[0];
        assert_eq!((*w, *h), (100, 80));
        // 右下角紧贴选区边缘处应有描边（被钳制到选区右下）
        let has_red_near_corner = (70..80).any(|y| {
            (90..100).any(|x| {
                let o = ((y * *w + x) * 4) as usize;
                rgba[o] > 200 && rgba[o + 1] < 90
            })
        });
        assert!(has_red_near_corner, "被钳制的矩形应贴着选区右下角");
    }

    /// 撤销 / 重做：快捷键与工具栏动作等价，预览分块随之增减。
    #[test]
    fn undo_redo_via_keys_and_toolbar() {
        let (mut view, _) = view_with(300, 200, 1.0, false);
        drag(&mut view, (20, 20), (219, 149));
        view.select_tool(AnnotationTool::Line);
        annotate(&mut view, (40, 40), (180, 120));
        assert!(view.tile_sprite_count() > 0);
        view.handle_key("z", true, false);
        assert_eq!(view.tile_sprite_count(), 0, "撤销后预览应清空");
        assert!(view.history_state().1);
        view.handle_key("y", true, false);
        assert!(view.tile_sprite_count() > 0);
        view.apply_action(ToolbarAction::Undo);
        assert_eq!(view.tile_sprite_count(), 0);
        view.handle_key("z", true, true);
        assert!(view.tile_sprite_count() > 0, "Ctrl+Shift+Z 重做");
        assert!(!view.pending_drops.is_empty(), "被替换的分块应排队释放");
    }

    /// 文字工具：选区内按下要求打开输入框；选区外不触发。
    #[test]
    fn text_tool_requests_input_box() {
        let (mut view, _) = view_with(300, 200, 1.0, false);
        drag(&mut view, (20, 20), (219, 149));
        view.select_tool(AnnotationTool::Text);
        let p = PhysicalPoint::new(80, 60);
        assert_eq!(view.handle_mouse_down(p, 1), OverlayOutcome::BeginText(p));
        assert!(!view.annotating);
    }

    /// 右键取消选区会清空标注并回到无工具状态。
    #[test]
    fn right_click_resets_annotations() {
        let (mut view, _) = view_with(300, 200, 1.0, false);
        drag(&mut view, (20, 20), (219, 149));
        view.select_tool(AnnotationTool::Ellipse);
        annotate(&mut view, (50, 50), (150, 110));
        assert!(view.tile_sprite_count() > 0);
        assert_eq!(view.handle_right_click(), OverlayOutcome::Stay);
        assert_eq!(view.tile_sprite_count(), 0);
        assert_eq!(view.current_tool(), AnnotationTool::None);
        assert!(!view.history_state().0);
    }

    /// 探针记录标注更新：脏块数远小于整屏块数。
    #[test]
    fn probe_records_partial_annotation_updates() {
        let (mut view, _) = view_with(1024, 768, 1.0, false);
        drag(&mut view, (20, 20), (1000, 740));
        view.select_tool(AnnotationTool::Arrow);
        annotate(&mut view, (100, 100), (300, 200));
        let (compute, _, tiles, _) = view.probe.annotation_summary();
        assert!(compute.count > 0);
        let total_tiles = 4.0 * 3.0;
        assert!(
            tiles < total_tiles / 2.0,
            "平均每次更新 {tiles} 块应远小于整屏 {total_tiles}"
        );
    }

    /// 标注基准驱动每个工具都能画出内容，并且产生可撤销的历史。
    #[test]
    fn bench_annotation_drives_every_tool() {
        for tool in [
            AnnotationTool::Rectangle,
            AnnotationTool::Ellipse,
            AnnotationTool::Arrow,
            AnnotationTool::Line,
            AnnotationTool::Pencil,
            AnnotationTool::Mosaic,
            AnnotationTool::Blur,
        ] {
            let (mut view, _) = view_with(400, 300, 1.0, false);
            view.bench_annotation_setup(tool);
            for i in 0..80 {
                view.bench_annotation_step(i, 80);
            }
            assert!(!view.annotating, "{tool:?} 结束后不应仍在拖动");
            assert!(view.tile_sprite_count() > 0, "{tool:?} 应有预览块");
            assert!(view.history_state().0, "{tool:?} 应可撤销");
        }
    }

    /// 文字基准准备会落一段示例文字。
    #[test]
    fn bench_text_setup_places_text() {
        let (mut view, _) = view_with(600, 400, 1.0, false);
        view.bench_annotation_setup(AnnotationTool::Text);
        assert!(view.tile_sprite_count() > 0);
        assert!(view.history_state().0);
    }

    /// 拖动中的多次移动会合并：flush 之前预览不变，flush 一次即提交最新位置。
    #[test]
    fn annotation_moves_coalesce_until_flush() {
        let (mut view, _) = view_with(400, 300, 1.0, false);
        drag(&mut view, (20, 20), (379, 279));
        view.select_tool(AnnotationTool::Line);
        view.handle_mouse_down(PhysicalPoint::new(50, 50), 1);
        assert!(view.annotating);
        for i in 0..10 {
            view.handle_mouse_move(PhysicalPoint::new(60 + i * 20, 80 + i * 10));
        }
        assert!(view.pending_annotation_point.is_some());
        assert_eq!(view.tile_sprite_count(), 0, "flush 之前不应有预览");
        view.flush_pending_annotation();
        assert!(view.tile_sprite_count() > 0);
        assert!(view.pending_annotation_point.is_none());
        view.handle_mouse_up(PhysicalPoint::new(300, 200));
        assert!(!view.annotating);
    }

    /// 构造识别结果。
    fn ocr_result(lines: &[&str]) -> OcrResult {
        use crate::ocr_service::OcrTextBox;
        OcrResult {
            full_text: lines.join("\n"),
            boxes: lines
                .iter()
                .enumerate()
                .map(|(i, t)| OcrTextBox {
                    rect: PhysicalRect::new(2, 2 + i as i32 * 10, 30, 8),
                    text: (*t).to_string(),
                    confidence: Some(0.9),
                })
                .collect(),
            elapsed_ms: 3,
        }
    }

    /// OCR 动作：没有选区时不触发；有选区时把选区像素（RGBA）交给输出通道，进入识别中且窗口保持。
    #[test]
    fn ocr_action_submits_selection_and_stays() {
        let (mut view, rec) = view_with(100, 80, 1.0, false);
        assert_eq!(view.apply_action(ToolbarAction::Ocr), OverlayOutcome::Stay);
        assert!(rec.borrow().ocrs.is_empty());
        drag(&mut view, (5, 5), (44, 34));
        assert_eq!(view.apply_action(ToolbarAction::Ocr), OverlayOutcome::Stay);
        assert_eq!(view.ocr_state(), &OcrUiState::Running);
        {
            let r = rec.borrow();
            assert_eq!(r.ocrs.len(), 1);
            let (serial, w, h, rgba) = &r.ocrs[0];
            assert_eq!((*serial, *w, *h), (1, 40, 30));
            assert_eq!(rgba.len(), 40 * 30 * 4);
            // 底图像素 r = x, g = y, b = 9；选区左上角是 (5, 5)
            assert_eq!(&rgba[0..3], &[5, 5, 9]);
        }
        // 识别中再次点击不重复提交
        view.apply_action(ToolbarAction::Ocr);
        assert_eq!(rec.borrow().ocrs.len(), 1);
    }

    /// 造翻译产出。
    fn translate_outcome(translated: &str) -> TranslateOutcome {
        TranslateOutcome {
            source: "hello".into(),
            translated: translated.into(),
            pairs: vec![("hello".into(), translated.into())],
            label: "fake-model".into(),
            ocr_ms: 5,
            translate_ms: 9,
        }
    }

    /// 翻译动作：没有选区时不触发；有选区时把选区像素交给输出通道并进入进行中；进行中不重复提交。
    #[test]
    fn translate_action_submits_selection_and_stays() {
        let (mut view, rec) = view_with(100, 80, 1.0, false);
        assert_eq!(
            view.apply_action(ToolbarAction::Translate),
            OverlayOutcome::Stay
        );
        assert!(rec.borrow().translates.is_empty());
        drag(&mut view, (5, 5), (44, 34));
        assert_eq!(
            view.apply_action(ToolbarAction::Translate),
            OverlayOutcome::Stay
        );
        assert!(matches!(
            view.translate_state(),
            TranslateUiState::Running(_)
        ));
        {
            let r = rec.borrow();
            assert_eq!(r.translates.len(), 1);
            let (serial, w, h, rgba) = &r.translates[0];
            assert_eq!((*serial, *w, *h), (1, 40, 30));
            assert_eq!(&rgba[0..3], &[5, 5, 9]);
        }
        view.apply_action(ToolbarAction::Translate);
        view.apply_action(ToolbarAction::Ocr);
        assert_eq!(rec.borrow().translates.len(), 1);
        assert!(rec.borrow().ocrs.is_empty(), "翻译进行中不能同时发起 OCR");
    }

    /// 翻译成功：译文复制到剪贴板；Enter 再复制并关闭；Esc 先退出翻译界面再关闭窗口。
    #[test]
    fn translate_success_copies_translation_and_enter_closes() {
        let (mut view, rec) = view_with(100, 80, 1.0, false);
        drag(&mut view, (5, 5), (44, 34));
        view.apply_action(ToolbarAction::Translate);
        view.update_translate_stage(1, TranslateStage::Translating);
        assert!(
            view.status_message
                .as_deref()
                .is_some_and(|s| s.contains("Translating"))
        );
        view.finish_translate(1, Ok(translate_outcome("你好")));
        assert!(matches!(
            view.translate_state(),
            TranslateUiState::Done { copied: true, .. }
        ));
        assert_eq!(rec.borrow().texts, vec!["你好".to_string()]);
        assert!(
            view.status_message
                .as_deref()
                .is_some_and(|s| s.contains("copied"))
        );
        assert_eq!(
            view.handle_key("enter", false, false),
            OverlayOutcome::Close
        );
        assert_eq!(rec.borrow().texts.len(), 2);

        let (mut view, _) = view_with(100, 80, 1.0, false);
        drag(&mut view, (5, 5), (44, 34));
        view.apply_action(ToolbarAction::Translate);
        view.finish_translate(1, Ok(translate_outcome("a")));
        assert_eq!(
            view.handle_key("escape", false, false),
            OverlayOutcome::Stay
        );
        assert_eq!(view.translate_state(), &TranslateUiState::Idle);
        assert_eq!(
            view.handle_key("escape", false, false),
            OverlayOutcome::Close
        );
    }

    /// 过期结果（用户已退出后才回来的）被丢弃，不会复活界面或改剪贴板。
    #[test]
    fn stale_translate_result_is_dropped() {
        let (mut view, rec) = view_with(100, 80, 1.0, false);
        drag(&mut view, (5, 5), (44, 34));
        view.apply_action(ToolbarAction::Translate);
        view.handle_key("escape", false, false);
        view.finish_translate(1, Ok(translate_outcome("迟到的译文")));
        assert_eq!(view.translate_state(), &TranslateUiState::Idle);
        assert!(rec.borrow().texts.is_empty());
        view.update_translate_stage(1, TranslateStage::Translating);
        assert_eq!(view.translate_state(), &TranslateUiState::Idle);
    }

    /// 失败分支：缺运行时提示可下载；缺模型、OCR 缺资产各有文案且不写剪贴板；提交失败直接明示。
    #[test]
    fn translate_failures_show_distinct_messages() {
        use crate::ocr_assets::OcrUnavailable;
        use snow_translate::TranslateError;
        let (mut view, rec) = view_with(100, 80, 1.0, false);
        drag(&mut view, (5, 5), (44, 34));
        view.apply_action(ToolbarAction::Translate);
        view.finish_translate(
            1,
            Err(TranslateFlowError::Translate(
                TranslateError::RuntimeMissing("未安装 onnxruntime 运行时".into()),
            )),
        );
        let runtime = view.status_message.clone().unwrap_or_default();
        assert!(
            runtime.contains("onnxruntime") && runtime.contains("press D"),
            "{runtime}"
        );
        view.dismiss_translate();
        view.apply_action(ToolbarAction::Translate);
        view.finish_translate(
            view.translate_serial,
            Err(TranslateFlowError::Translate(TranslateError::NoModelFound(
                "模型目录 D:/m 里没有可用的翻译模型".into(),
            ))),
        );
        let no_model = view.status_message.clone().unwrap_or_default();
        assert!(
            no_model.contains("D:/m") && !no_model.contains("press D"),
            "{no_model}"
        );
        view.dismiss_translate();
        view.apply_action(ToolbarAction::Translate);
        view.finish_translate(
            view.translate_serial,
            Err(TranslateFlowError::Ocr(OcrError::Unavailable(
                OcrUnavailable::NoRuntime,
            ))),
        );
        let ocr = view.status_message.clone().unwrap_or_default();
        assert!(ocr.contains("OCR"), "{ocr}");
        assert_ne!(runtime, no_model);
        assert!(rec.borrow().texts.is_empty(), "失败时不能写剪贴板");

        let (mut view, _) = view_with(100, 80, 1.0, true);
        drag(&mut view, (5, 5), (44, 34));
        view.apply_action(ToolbarAction::Translate);
        assert!(matches!(
            view.translate_state(),
            TranslateUiState::Failed {
                can_download: false,
                ..
            }
        ));
    }

    /// 缺运行时时按 D 触发下载；进度与结果更新状态；成功回到待命，失败可重试。
    #[test]
    fn translate_runtime_download_flow() {
        use snow_translate::TranslateError;
        let (mut view, rec) = view_with(100, 80, 1.0, false);
        drag(&mut view, (5, 5), (44, 34));
        view.handle_key("d", false, false);
        assert_eq!(
            rec.borrow().translate_downloads,
            0,
            "没有失败态时按 D 不下载"
        );
        view.apply_action(ToolbarAction::Translate);
        view.finish_translate(
            1,
            Err(TranslateFlowError::Translate(
                TranslateError::RuntimeMissing("缺运行时".into()),
            )),
        );
        assert_eq!(view.handle_key("d", false, false), OverlayOutcome::Stay);
        assert_eq!(rec.borrow().translate_downloads, 1);
        assert!(matches!(
            view.translate_state(),
            TranslateUiState::Downloading(_)
        ));
        view.update_translate_download("正在下载 onnxruntime 运行时…");
        assert_eq!(
            view.status_message.as_deref(),
            Some("正在下载 onnxruntime 运行时…")
        );
        view.finish_translate_download(Err("网络断了".into()));
        assert!(matches!(
            view.translate_state(),
            TranslateUiState::Failed {
                can_download: true,
                ..
            }
        ));
        view.handle_key("d", false, false);
        assert_eq!(rec.borrow().translate_downloads, 2);
        view.finish_translate_download(Ok(()));
        assert_eq!(view.translate_state(), &TranslateUiState::Idle);
        assert!(
            view.status_message
                .as_deref()
                .is_some_and(|s| s.contains("again"))
        );
    }

    /// OCR 与翻译互斥：翻译界面打开时点 OCR 会先退出翻译界面；反过来同理。
    #[test]
    fn ocr_and_translate_are_exclusive() {
        let (mut view, rec) = view_with(100, 80, 1.0, false);
        drag(&mut view, (5, 5), (44, 34));
        view.apply_action(ToolbarAction::Translate);
        view.finish_translate(1, Ok(translate_outcome("你好")));
        view.apply_action(ToolbarAction::Ocr);
        assert_eq!(view.translate_state(), &TranslateUiState::Idle);
        assert_eq!(view.ocr_state(), &OcrUiState::Running);
        view.finish_ocr(view.ocr_serial, Ok(ocr_result(&["a"])));
        view.apply_action(ToolbarAction::Translate);
        assert_eq!(view.ocr_state(), &OcrUiState::Idle);
        assert!(matches!(
            view.translate_state(),
            TranslateUiState::Running(_)
        ));
        assert_eq!(rec.borrow().translates.len(), 2);
    }

    /// 识别成功：文本复制到剪贴板、进入结果态；Enter 再复制并关闭；Esc 先退出 OCR 再关闭窗口。
    #[test]
    fn ocr_success_copies_text_and_enter_closes() {
        let (mut view, rec) = view_with(100, 80, 1.0, false);
        drag(&mut view, (5, 5), (44, 34));
        view.apply_action(ToolbarAction::Ocr);
        view.finish_ocr(1, Ok(ocr_result(&["hello", "世界"])));
        assert!(matches!(
            view.ocr_state(),
            OcrUiState::Done { copied: true, .. }
        ));
        assert_eq!(rec.borrow().texts, vec!["hello\n世界".to_string()]);
        assert!(
            view.status_message
                .as_deref()
                .is_some_and(|s| s.contains("copied"))
        );
        assert_eq!(
            view.handle_key("enter", false, false),
            OverlayOutcome::Close
        );
        assert_eq!(rec.borrow().texts.len(), 2);

        // Esc：先退出 OCR 界面，再按一次才关闭
        let (mut view, _) = view_with(100, 80, 1.0, false);
        drag(&mut view, (5, 5), (44, 34));
        view.apply_action(ToolbarAction::Ocr);
        view.finish_ocr(1, Ok(ocr_result(&["a"])));
        assert_eq!(
            view.handle_key("escape", false, false),
            OverlayOutcome::Stay
        );
        assert_eq!(view.ocr_state(), &OcrUiState::Idle);
        assert_eq!(
            view.handle_key("escape", false, false),
            OverlayOutcome::Close
        );
    }

    /// 空结果不复制文本（不覆盖用户剪贴板），提示“未识别到文字”。
    #[test]
    fn ocr_empty_result_does_not_touch_clipboard() {
        let (mut view, rec) = view_with(100, 80, 1.0, false);
        drag(&mut view, (5, 5), (44, 34));
        view.apply_action(ToolbarAction::Ocr);
        view.finish_ocr(1, Ok(ocr_result(&[])));
        assert!(rec.borrow().texts.is_empty());
        assert_eq!(view.status_message.as_deref(), Some("No text found"));
    }

    /// 复制失败：仍展示结果，但状态条说明复制失败。
    #[test]
    fn ocr_copy_failure_is_reported() {
        // fail 输出会让提交本身失败，所以直接把状态推进到 Running 再喂结果
        let (mut view, _) = view_with(100, 80, 1.0, true);
        drag(&mut view, (5, 5), (44, 34));
        view.ocr = OcrUiState::Running;
        view.finish_ocr(view.ocr_serial, Ok(ocr_result(&["x"])));
        assert!(matches!(
            view.ocr_state(),
            OcrUiState::Done { copied: false, .. }
        ));
        assert!(
            view.status_message
                .as_deref()
                .is_some_and(|s| s.contains("failed"))
        );
    }

    /// 构造一个底图为二维码样本的视图，并拖出覆盖整张码的选区。
    fn view_with_qr(fail: bool) -> (ScreenshotOverlayView, Rc<RefCell<Recorded>>) {
        let (w, h, data) = crate::qr_decode::test_support::render_sample(6, 4);
        let frame = FrozenFrame::from_captured(CapturedScreen {
            width: w,
            height: h,
            data,
        })
        .unwrap();
        let rec = Rc::new(RefCell::new(Recorded::default()));
        let sink = RecordingSink {
            rec: Rc::clone(&rec),
            fail,
        };
        let mut view =
            ScreenshotOverlayView::new(frame, 1.0, PhysicalPoint::new(0, 0), Box::new(sink));
        drag(&mut view, (2, 2), (w as i32 - 2, h as i32 - 2));
        (view, rec)
    }

    /// 二维码内容是网页链接：识别后复制内容、面板出现“打开链接”提示，按 O 交给系统浏览器并关闭覆盖窗。
    #[test]
    fn qr_link_can_be_opened() {
        let sample = crate::qr_decode::test_support::SAMPLE_TEXT;
        let (mut view, rec) = view_with_qr(false);
        assert_eq!(view.recognize_qr_code(), OverlayOutcome::Stay);
        assert_eq!(rec.borrow().texts, vec![sample.to_string()]);
        assert!(
            matches!(view.ocr_state(), OcrUiState::Done { text, copied: true, .. } if text == sample)
        );
        assert_eq!(view.qr_link.as_deref(), Some(sample));
        assert_eq!(view.handle_key("o", false, false), OverlayOutcome::Close);
        assert_eq!(rec.borrow().urls, vec![sample.to_string()]);
    }

    /// 打开链接失败：覆盖窗保留并提示原因，链接仍可重试。
    #[test]
    fn qr_link_open_failure_keeps_overlay() {
        let (mut view, rec) = view_with_qr(false);
        view.recognize_qr_code();
        // 复制成功后再让输出通道失败：直接换成会失败的通道
        let fail_rec = Rc::new(RefCell::new(Recorded::default()));
        view.output = Box::new(RecordingSink {
            rec: Rc::clone(&fail_rec),
            fail: true,
        });
        assert_eq!(view.handle_key("o", false, false), OverlayOutcome::Stay);
        assert!(
            view.status_message
                .as_deref()
                .is_some_and(|s| s.contains("boom"))
        );
        assert!(view.qr_link.is_some());
        assert!(rec.borrow().urls.is_empty() && fail_rec.borrow().urls.is_empty());
    }

    /// 没有二维码链接时按 O 不会打开任何东西：普通 OCR 结果、识别不到码、退出 OCR 界面后都一样。
    #[test]
    fn open_link_key_needs_a_qr_link() {
        let (mut view, rec) = view_with_qr(false);
        // 普通文字识别结果里即使有链接也不提供打开
        view.ocr = OcrUiState::Done {
            text: "https://a.b".into(),
            boxes: Vec::new(),
            copied: true,
        };
        view.handle_key("o", false, false);
        assert!(rec.borrow().urls.is_empty());
        // 识别到码后退出 OCR 界面，链接被清掉
        view.recognize_qr_code();
        assert!(view.qr_link.is_some());
        view.dismiss_ocr();
        assert!(view.qr_link.is_none());
        view.handle_key("o", false, false);
        assert!(rec.borrow().urls.is_empty());
        // 选区里没有码：失败提示，没有链接
        let (mut blank, _) = view_with(100, 80, 1.0, false);
        drag(&mut blank, (5, 5), (44, 34));
        blank.recognize_qr_code();
        assert!(matches!(blank.ocr_state(), OcrUiState::Failed { .. }));
        assert!(blank.qr_link.is_none());
    }

    /// 失败分支：缺资产提示可下载，其它失败给出各自文案；提交失败也是明确提示而不是静默。
    #[test]
    fn ocr_failures_show_distinct_messages() {
        use crate::ocr_assets::OcrUnavailable;
        let (mut view, _) = view_with(100, 80, 1.0, false);
        drag(&mut view, (5, 5), (44, 34));
        view.apply_action(ToolbarAction::Ocr);
        view.finish_ocr(1, Err(OcrError::Unavailable(OcrUnavailable::NoRuntime)));
        let no_runtime = view.status_message.clone().unwrap_or_default();
        assert!(
            no_runtime.contains("OCR runtime") && no_runtime.contains("press D"),
            "{no_runtime}"
        );
        view.dismiss_ocr();
        view.apply_action(ToolbarAction::Ocr);
        view.finish_ocr(view.ocr_serial, Err(OcrError::SessionNotReady));
        let not_ready = view.status_message.clone().unwrap_or_default();
        assert!(
            not_ready.contains("failed to load") && !not_ready.contains("press D"),
            "{not_ready}"
        );
        assert_ne!(no_runtime, not_ready);

        // 输出通道拒绝提交：直接进入失败态
        let (mut view, _) = view_with(100, 80, 1.0, true);
        drag(&mut view, (5, 5), (44, 34));
        view.apply_action(ToolbarAction::Ocr);
        assert!(matches!(
            view.ocr_state(),
            OcrUiState::Failed {
                can_download: false,
                ..
            }
        ));
    }

    /// 按 D 触发下载；下载进度与结果更新状态；成功后回到待命，失败可再次重试。
    #[test]
    fn ocr_download_flow() {
        use crate::ocr_assets::OcrUnavailable;
        let (mut view, rec) = view_with(100, 80, 1.0, false);
        drag(&mut view, (5, 5), (44, 34));
        // 没有失败态时按 D 不触发下载
        view.handle_key("d", false, false);
        assert_eq!(rec.borrow().ocr_downloads, 0);
        view.apply_action(ToolbarAction::Ocr);
        view.finish_ocr(
            1,
            Err(OcrError::Unavailable(OcrUnavailable::NoModel {
                id: "m".into(),
            })),
        );
        assert_eq!(view.handle_key("d", false, false), OverlayOutcome::Stay);
        assert_eq!(rec.borrow().ocr_downloads, 1);
        assert!(matches!(view.ocr_state(), OcrUiState::Downloading(_)));
        view.update_ocr_download("正在下载 OCR 模型 (2/3)…");
        assert_eq!(
            view.status_message.as_deref(),
            Some("正在下载 OCR 模型 (2/3)…")
        );
        view.finish_ocr_download(Err("网络不可达".into()));
        assert!(matches!(
            view.ocr_state(),
            OcrUiState::Failed {
                can_download: true,
                ..
            }
        ));
        view.handle_key("d", false, false);
        view.finish_ocr_download(Ok(()));
        assert_eq!(view.ocr_state(), &OcrUiState::Idle);
        assert!(
            view.status_message
                .as_deref()
                .is_some_and(|s| s.contains("again"))
        );
    }

    /// 过期结果（用户已退出 OCR 或又发起了新请求）被丢弃。
    #[test]
    fn stale_ocr_results_are_dropped() {
        let (mut view, rec) = view_with(100, 80, 1.0, false);
        drag(&mut view, (5, 5), (44, 34));
        view.apply_action(ToolbarAction::Ocr);
        view.handle_key("escape", false, false);
        view.finish_ocr(1, Ok(ocr_result(&["late"])));
        assert_eq!(view.ocr_state(), &OcrUiState::Idle);
        assert!(rec.borrow().texts.is_empty());
    }

    /// 长截图动作：需要选区；成功后把（夹到屏内的）选区交给输出通道并关闭覆盖窗。
    #[test]
    fn scroll_capture_action_sends_region_and_closes() {
        let (mut view, rec) = view_with(100, 80, 1.0, false);
        assert_eq!(
            view.apply_action(ToolbarAction::ScrollCapture),
            OverlayOutcome::Stay
        );
        assert!(rec.borrow().scrolls.is_empty());
        drag(&mut view, (5, 5), (44, 34));
        assert_eq!(
            view.apply_action(ToolbarAction::ScrollCapture),
            OverlayOutcome::Close
        );
        assert_eq!(rec.borrow().scrolls, vec![PhysicalRect::new(5, 5, 40, 30)]);
        // 输出通道失败：保留窗口并提示
        let (mut view, _) = view_with(100, 80, 1.0, true);
        drag(&mut view, (5, 5), (44, 34));
        assert_eq!(
            view.apply_action(ToolbarAction::ScrollCapture),
            OverlayOutcome::Stay
        );
        assert!(
            view.status_message
                .as_deref()
                .is_some_and(|s| s.contains("scrolling capture"))
        );
    }

    /// 长截图选区模式：Enter 确认区域并交给长截图；复制 / 保存快捷键被禁用；Esc 仍可取消。
    #[test]
    fn scroll_mode_enter_starts_scroll_capture() {
        let (mut view, rec) = view_with(100, 80, 1.0, false);
        view.set_scroll_mode(true);
        assert_eq!(view.handle_key("enter", false, false), OverlayOutcome::Stay);
        assert!(
            view.status_message
                .as_deref()
                .is_some_and(|s| s.contains("Select"))
        );
        drag(&mut view, (5, 5), (44, 34));
        assert_eq!(view.handle_key("c", true, false), OverlayOutcome::Stay);
        assert_eq!(view.handle_key("s", true, false), OverlayOutcome::Stay);
        assert!(rec.borrow().images.is_empty() && rec.borrow().saved.is_empty());
        assert_eq!(
            view.handle_key("enter", false, false),
            OverlayOutcome::Close
        );
        assert_eq!(rec.borrow().scrolls, vec![PhysicalRect::new(5, 5, 40, 30)]);
        let (mut view, _) = view_with(100, 80, 1.0, false);
        view.set_scroll_mode(true);
        assert_eq!(
            view.handle_key("escape", false, false),
            OverlayOutcome::Close
        );
    }

    /// 改色 / 线宽后画的矩形按新样式出图（复制结果里左边线为蓝色）。
    #[test]
    fn style_change_recolors_next_annotation() {
        let (mut view, rec) = view_with(300, 200, 1.0, false);
        drag(&mut view, (20, 20), (219, 149));
        view.select_tool(AnnotationTool::Rectangle);
        view.update_tool_style(AnnotationTool::Rectangle, |s| {
            s.color = [0x16, 0x77, 0xFF, 0xFF];
            s.width = 6;
        });
        annotate(&mut view, (60, 50), (160, 110));
        assert_eq!(
            view.handle_key("enter", false, false),
            OverlayOutcome::Close
        );
        let r = rec.borrow();
        let (w, _, rgba) = &r.images[0];
        let o = ((60 * *w + 40) * 4) as usize;
        let p = &rgba[o..o + 4];
        assert!(p[2] > 200 && p[0] < 60, "左边线应为蓝色: {p:?}");
    }

    /// 每个工具记住自己的样式：改了直线不影响箭头，切回直线仍是改后的样式。
    #[test]
    fn style_is_remembered_per_tool() {
        let (mut view, rec) = view_with(300, 200, 1.0, false);
        drag(&mut view, (20, 20), (219, 149));
        view.select_tool(AnnotationTool::Line);
        view.update_tool_style(AnnotationTool::Line, |s| s.color = [0x16, 0x77, 0xFF, 0xFF]);
        view.select_tool(AnnotationTool::Arrow);
        assert_eq!(
            view.tool_style(AnnotationTool::Arrow),
            crate::annotation_style::default_style(AnnotationTool::Arrow)
        );
        // 再切回直线（选箭头会取消；再选直线）
        view.select_tool(AnnotationTool::Line);
        assert_eq!(
            view.tool_style(AnnotationTool::Line).color,
            [0x16, 0x77, 0xFF, 0xFF]
        );
        annotate(&mut view, (40, 80), (180, 80));
        view.handle_key("enter", false, false);
        let r = rec.borrow();
        let (w, _, rgba) = &r.images[0];
        // 直线在底图 y=80 -> 裁切图 y=60，取 x=100 -> 80
        let o = ((60 * *w + 80) * 4) as usize;
        let p = &rgba[o..o + 4];
        assert!(p[2] > 200 && p[0] < 60, "直线应为蓝色: {p:?}");
    }

    /// 样式写回配置并能被新的覆盖窗读回（键沿用旧版 `drawing/*_style`）。
    #[test]
    fn style_persists_and_reloads() {
        let dir = std::env::temp_dir().join(format!(
            "cisox-style-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| d.as_nanos())
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("config.json");
        let (mut first, _) = view_with(100, 80, 1.0, false);
        first.set_style_config(Rc::new(RefCell::new(ConfigStore::open(&path))), "en-US");
        first.update_tool_style(AnnotationTool::Arrow, |s| {
            s.width = 12;
            s.color = [1, 2, 3, 255];
            s.arrowhead = ArrowheadChoice::Dot;
        });
        let raw = std::fs::read_to_string(&path).unwrap();
        assert!(
            raw.contains("arrow_style") && raw.contains("#010203FF"),
            "配置应含箭头样式: {raw}"
        );

        let (mut second, _) = view_with(100, 80, 1.0, false);
        second.set_style_config(Rc::new(RefCell::new(ConfigStore::open(&path))), "en-US");
        let arrow = second.tool_style(AnnotationTool::Arrow);
        assert_eq!(
            (arrow.width, arrow.color, arrow.arrowhead),
            (12, [1, 2, 3, 255], ArrowheadChoice::Dot)
        );
        assert_eq!(second.styles.recent(), &[[1, 2, 3, 255]]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 荧光笔与序号球现在是可用工具：能画、能撤销。
    #[test]
    fn highlighter_and_counter_annotate_and_undo() {
        for tool in [AnnotationTool::Highlighter, AnnotationTool::Counter] {
            let (mut view, _) = view_with(300, 200, 1.0, false);
            drag(&mut view, (20, 20), (219, 149));
            view.select_tool(tool);
            assert_eq!(view.current_tool(), tool);
            if tool == AnnotationTool::Counter {
                // 序号球是单击落点
                annotate(&mut view, (80, 80), (80, 80));
            } else {
                annotate(&mut view, (60, 80), (160, 80));
            }
            assert!(view.tile_sprite_count() > 0, "{tool:?} 应产生预览分块");
            assert!(view.history_state().0, "{tool:?} 应可撤销");
            view.apply_action(ToolbarAction::Undo);
            assert_eq!(view.tile_sprite_count(), 0);
        }
    }

    /// 样式面板：滤镜工具与录屏 / 长图模式不显示，其余工具显示。
    #[test]
    fn style_panel_visibility_rules() {
        let (mut view, _) = view_with(300, 200, 1.0, false);
        drag(&mut view, (20, 20), (219, 149));
        assert_eq!(view.style_panel_origin((10, 10), (300, 200)), None);
        view.select_tool(AnnotationTool::Mosaic);
        assert_eq!(view.style_panel_origin((10, 10), (300, 200)), None);
        view.select_tool(AnnotationTool::Text);
        assert!(view.style_panel_origin((10, 10), (300, 200)).is_some());
        view.set_record_mode(true);
        assert_eq!(view.style_panel_origin((10, 10), (300, 200)), None);
    }
}
