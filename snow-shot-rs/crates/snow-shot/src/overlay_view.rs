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
use crate::frozen_frame::FrozenFrame;
use crate::history_store::HistorySource;
use crate::ocr_client::OcrError;
use crate::ocr_flow::{OcrUiState, panel_lines};
use crate::ocr_service::OcrResult;
use crate::translate_flow::{TranslateUiState, panel_lines as translate_panel_lines, stage_text};
use crate::translate_service::{TranslateFlowError, TranslateOutcome, TranslateStage};
use crate::overlay_probe::FrameProbe;
use crate::window_pick::{DRAG_THRESHOLD_LOGICAL, WindowHover, exceeds_drag_threshold};
use crate::screenshot_output;
use image::{Frame, RgbaImage};
use snow_canvas_raster::TileKey;
use snow_config::store::ConfigStore;
use snow_i18n::{Args, I18n};
use snow_canvas_text::{CanvasTextInput, CanvasTextStyle, EditKeyOutcome};
use snow_platform::text_raster::DEFAULT_FONT_FAMILY;
use snow_platform::clipboard::{copy_image_to_clipboard, copy_text_to_clipboard};
use snow_ui::shell::geometry::{PhysicalPoint, PhysicalRect};
use snow_ui::ui::component::checkbox::Checkbox;
use snow_ui::ui::component::searchable_list::{SearchableListItem, SearchableVec};
use snow_ui::ui::component::select::{Select, SelectEvent, SelectState};
use snow_ui::ui::component::{IndexPath, Sizable, Size as ComponentSize, Theme, ThemeMode};
use snow_ui::shell::selection::{
    DEFAULT_EDGE_TOLERANCE, DEFAULT_HANDLE_SIZE, DEFAULT_MINIMUM_SELECTION_SIZE,
    SelectionDragMode, SelectionState, dragged_selection_rect, handle_rects, hit_test_drag_mode,
    marquee_selection_rect, selection_size_label,
};
use snow_ui::ui::*;
use snow_ui::widgets::{
    AnnotationTool, ColorFormat, Magnifier, MagnifierGrid, ScreenshotToolbar, ToolbarAction,
    calculate_magnifier_placement, calculate_toolbar_placement,
};
use std::cell::RefCell;
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
const TOOLBAR_LOGICAL_SIZE: (i32, i32) = (890, 36);
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
/// 底部默认提示文案。
const HINT_TEXT: &str =
    "拖拽框选 · 双击/Enter 复制 · Ctrl+S 保存 · C 复制颜色 · Esc/右键 取消";
/// 选中标注工具后的底部提示文案。
const TOOL_HINT_TEXT: &str =
    "在选区内拖动绘制 · Ctrl+Z 撤销 · Ctrl+Y 重做 · Enter 复制 · Ctrl+S 保存 · Esc 取消";
/// 文字工具编辑中的提示文案。
const TEXT_HINT_TEXT: &str = "输入文字 · Enter 完成 · Shift+Enter 换行 · Esc 放弃";
/// 录屏选区模式的底部提示文案。
const RECORD_HINT_TEXT: &str = "拖拽框选录制区域 · 双击/Enter 开始录制 · Esc/右键 取消";
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
const SCROLL_HINT_TEXT: &str = "拖拽框选要滚动截取的区域 · 双击/Enter 开始长截图 · Esc/右键 取消";
/// OCR 结果面板的逻辑宽度上限。
const OCR_PANEL_MAX_WIDTH: f32 = 520.0;
/// OCR 结果面板背景色。
const OCR_PANEL_BG: u32 = 0x000000D9;
/// OCR 文本框描边色。
const OCR_BOX_COLOR: u32 = 0xFAAD14;
/// 双击判定所需的点击次数。
const DOUBLE_CLICK_COUNT: usize = 2;
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

/// 用户操作处理后的窗口去向。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OverlayOutcome {
    /// 覆盖窗继续保留。
    Stay,
    /// 关闭覆盖窗。
    Close,
    /// 在给定底图物理坐标处开始文字输入（需要窗口上下文才能创建输入框）。
    BeginText(PhysicalPoint),
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
}

impl AutoConfirm {
    /// 对应的工具栏动作。
    pub fn toolbar_action(self) -> ToolbarAction {
        match self {
            Self::Copy => ToolbarAction::Copy,
            Self::Pin => ToolbarAction::Pin,
            Self::Ocr => ToolbarAction::Ocr,
            Self::Translate => ToolbarAction::Translate,
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
            key, x, y, w, h, bgra,
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

    /// 把 RGBA 图像保存为文件，返回写入的路径。
    fn save_image(&mut self, width: u32, height: u32, rgba: &[u8]) -> Result<PathBuf, String>;

    /// 以选区（底图物理坐标）启动屏幕录制；默认不支持。
    ///
    /// # 参数
    /// - `region`：选区（覆盖窗底图坐标，原点为该显示器左上角）。
    fn start_recording(&mut self, _region: PhysicalRect) -> Result<(), String> {
        Err("录屏功能未接入".to_string())
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
        Err("贴图功能未接入".to_string())
    }

    /// 对选区图像发起文字识别（异步：结果稍后经 [`ScreenshotOverlayView::finish_ocr`] 回来）；默认不支持。
    ///
    /// # 参数
    /// - `serial`：本次请求序号，回传结果时原样带回（过期结果据此丢弃）。
    /// - `width` / `height`：图像尺寸。
    /// - `rgba`：选区（含标注合成）的 RGBA 像素。
    fn start_ocr(&mut self, _serial: u64, _width: u32, _height: u32, _rgba: Vec<u8>) -> Result<(), String> {
        Err("文字识别功能未接入".to_string())
    }

    /// 触发 OCR 组件下载（异步）；默认不支持。
    fn start_ocr_download(&mut self) -> Result<(), String> {
        Err("OCR 下载功能未接入".to_string())
    }

    /// 对选区图像发起“识别 + 翻译”（异步：结果稍后经 [`ScreenshotOverlayView::finish_translate`] 回来）；默认不支持。
    ///
    /// # 参数
    /// - `serial`：本次请求序号，回传结果时原样带回（过期结果据此丢弃）。
    /// - `width` / `height`：图像尺寸。
    /// - `rgba`：选区（含标注合成）的 RGBA 像素。
    fn start_translate(&mut self, _serial: u64, _width: u32, _height: u32, _rgba: Vec<u8>) -> Result<(), String> {
        Err("文字翻译功能未接入".to_string())
    }

    /// 触发 onnxruntime 运行时下载（异步）；默认不支持。
    fn start_translate_download(&mut self) -> Result<(), String> {
        Err("翻译运行时下载功能未接入".to_string())
    }

    /// 以选区（底图物理坐标）启动长截图；默认不支持。
    ///
    /// # 参数
    /// - `region`：选区（覆盖窗底图坐标，原点为该显示器左上角）。
    fn start_scroll_capture(&mut self, _region: PhysicalRect) -> Result<(), String> {
        Err("长截图功能未接入".to_string())
    }
}

/// 系统输出：真实剪贴板 + 保存目录。
pub struct SystemOutput {
    /// 图片保存目录。
    save_directory: PathBuf,
    /// 选区确认录屏时的回调（参数为覆盖窗底图坐标下的选区）。
    on_record: Option<Box<dyn Fn(PhysicalRect)>>,
    /// 贴图回调：选区（覆盖窗底图坐标）、图像尺寸与不透明 RGBA 像素。
    on_pin: Option<PinCallback>,
    /// 文字识别回调：`(序号, 宽, 高, RGBA)`。
    on_ocr: Option<OcrCallback>,
    /// OCR 组件下载回调。
    on_ocr_download: Option<Box<dyn Fn()>>,
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
            on_record: None,
            on_pin: None,
            on_ocr: None,
            on_ocr_download: None,
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
    pub fn with_history(mut self, callback: impl Fn(HistorySource, u32, u32, &[u8]) + 'static) -> Self {
        self.on_history = Some(Box::new(callback));
        self
    }

    /// 成功输出后通知历史回调（未设置则忽略）。
    fn note_history(&self, source: HistorySource, width: u32, height: u32, rgba: &[u8]) {
        if let Some(callback) = &self.on_history {
            callback(source, width, height, rgba);
        }
    }

    /// 设置文字识别回调：用户点“OCR”时触发（识别在后台线程执行）。
    ///
    /// # 参数
    /// - `callback`：接收请求序号、图像尺寸与 RGBA 像素。
    pub fn with_ocr(mut self, callback: impl Fn(u64, u32, u32, Vec<u8>) + 'static) -> Self {
        self.on_ocr = Some(Box::new(callback));
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
    pub fn with_pin(mut self, callback: impl Fn(PhysicalRect, u32, u32, Vec<u8>) + 'static) -> Self {
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
    /// 写入系统剪贴板（CF_DIB）。
    fn copy_image(&mut self, width: u32, height: u32, rgba: &[u8]) -> Result<(), String> {
        copy_image_to_clipboard(width, height, rgba)?;
        self.note_history(HistorySource::Copied, width, height, rgba);
        Ok(())
    }

    /// 写入系统剪贴板（Unicode 文本）。
    fn copy_text(&mut self, text: &str) -> Result<(), String> {
        copy_text_to_clipboard(text)
    }

    /// 保存为 PNG 文件。
    fn save_image(&mut self, width: u32, height: u32, rgba: &[u8]) -> Result<PathBuf, String> {
        let path = screenshot_output::save_png(&self.save_directory, width, height, rgba)?;
        self.note_history(HistorySource::Saved, width, height, rgba);
        Ok(path)
    }

    /// 触发录屏回调；未设置回调时报错。
    fn start_recording(&mut self, region: PhysicalRect) -> Result<(), String> {
        match &self.on_record {
            Some(callback) => {
                callback(region);
                Ok(())
            }
            None => Err("录屏功能未接入".to_string()),
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
            None => Err("贴图功能未接入".to_string()),
        }
    }

    /// 触发文字识别回调；未设置回调时报错。
    fn start_ocr(&mut self, serial: u64, width: u32, height: u32, rgba: Vec<u8>) -> Result<(), String> {
        match &self.on_ocr {
            Some(callback) => {
                callback(serial, width, height, rgba);
                Ok(())
            }
            None => Err("文字识别功能未接入".to_string()),
        }
    }

    /// 触发 OCR 组件下载回调；未设置回调时报错。
    fn start_ocr_download(&mut self) -> Result<(), String> {
        match &self.on_ocr_download {
            Some(callback) => {
                callback();
                Ok(())
            }
            None => Err("OCR 下载功能未接入".to_string()),
        }
    }

    /// 触发文字翻译回调；未设置回调时报错。
    fn start_translate(&mut self, serial: u64, width: u32, height: u32, rgba: Vec<u8>) -> Result<(), String> {
        match &self.on_translate {
            Some(callback) => {
                callback(serial, width, height, rgba);
                Ok(())
            }
            None => Err("文字翻译功能未接入".to_string()),
        }
    }

    /// 触发翻译运行时下载回调；未设置回调时报错。
    fn start_translate_download(&mut self) -> Result<(), String> {
        match &self.on_translate_download {
            Some(callback) => {
                callback();
                Ok(())
            }
            None => Err("翻译运行时下载功能未接入".to_string()),
        }
    }

    /// 触发长截图回调；未设置回调时报错。
    fn start_scroll_capture(&mut self, region: PhysicalRect) -> Result<(), String> {
        match &self.on_scroll {
            Some(callback) => {
                callback(region);
                Ok(())
            }
            None => Err("长截图功能未接入".to_string()),
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
    PhysicalRect::new(l(rect.x), l(rect.y), l(rect.width).max(1), l(rect.height).max(1))
}

/// 截图覆盖窗主视图组件。
pub struct ScreenshotOverlayView {
    /// 冻结底图。
    frame: FrozenFrame,
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
    /// OCR 交互状态。
    ocr: OcrUiState,
    /// 最近一次 OCR 请求序号（过期结果据此丢弃）。
    ocr_serial: u64,
    /// 文字翻译交互状态。
    translate: TranslateUiState,
    /// 最近一次翻译请求序号（过期结果据此丢弃）。
    translate_serial: u64,
    /// 窗口悬停来源（智能选区开启时才有）。
    window_hover: Option<Box<dyn WindowHover>>,
    /// 悬停窗口的底图矩形（仅 Idle 时更新并高亮）。
    hover_window: Option<PhysicalRect>,
    /// 按下时锁定的窗口矩形：位移未超阈值就松开则直接作为选区，超过则作废转手动框选。
    click_window: Option<PhysicalRect>,
}

impl ScreenshotOverlayView {
    /// 创建覆盖窗主视图（不依赖 GPUI 上下文，可离屏测试）。
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
        let screen_bounds = frame.bounds();
        let scale = if scale.is_finite() && scale > 0.0 { scale } else { 1.0 };
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
            ocr: OcrUiState::Idle,
            ocr_serial: 0,
            translate: TranslateUiState::Idle,
            translate_serial: 0,
            window_hover: None,
            hover_window: None,
            click_window: None,
        };
        let start = view.clamp_point(initial_cursor);
        view.cursor_pos = start;
        view.update_magnifier_grid(start);
        view
    }

    /// 在 GPUI 中创建视图实体并抢占键盘焦点。
    ///
    /// # 参数
    /// - `window` / `app`：建窗回调提供的上下文。
    /// - 其余参数同 [`ScreenshotOverlayView::new`]。
    pub fn create(
        window: &mut Window,
        app: &mut App,
        frame: FrozenFrame,
        initial_cursor: PhysicalPoint,
        output: Box<dyn OutputSink>,
    ) -> Entity<Self> {
        let scale = window.scale_factor();
        let view = app.new(|cx| {
            let mut view = Self::new(frame, scale, initial_cursor, output);
            view.focus_handle = Some(cx.focus_handle());
            view
        });
        if let Some(handle) = view.read(app).focus_handle.clone() {
            window.focus(&handle, app);
        }
        view
    }

    /// 固定缩放比（性能基准用：让 4K 底图铺满较小的窗口时坐标仍自洽）。
    pub fn set_scale_override(&mut self, scale: Option<f32>) {
        self.scale_override = scale.filter(|s| s.is_finite() && *s > 0.0);
        if let Some(s) = self.scale_override {
            self.scale = s;
        }
    }

    /// 接入窗口悬停来源，开启窗口级智能选区。
    ///
    /// # 参数
    /// - `source`：悬停来源；传 `None` 关闭该功能。
    ///
    /// ```ignore
    /// view.set_window_hover(Some(Box::new(picker)));
    /// ```
    pub fn set_window_hover(&mut self, source: Option<Box<dyn WindowHover>>) {
        self.window_hover = source;
        self.hover_window = None;
        self.click_window = None;
    }

    /// 当前应高亮的窗口矩形：空闲时是悬停窗口，按下未超阈值时是锁定窗口。
    fn window_highlight(&self) -> Option<PhysicalRect> {
        match self.state {
            SelectionState::Idle => self.hover_window,
            SelectionState::MarqueeDragging { .. } => self.click_window,
            _ => None,
        }
    }

    /// 向悬停来源查询指定点下的窗口并更新高亮。
    fn refresh_window_hover(&mut self, point: PhysicalPoint) {
        if let Some(source) = self.window_hover.as_mut() {
            self.hover_window = source.hover(point);
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
                None,
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
        self.clamp_point(PhysicalPoint::new(
            (logical.x.as_f32() * self.scale).round() as i32,
            (logical.y.as_f32() * self.scale).round() as i32,
        ))
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
    pub fn handle_mouse_down(&mut self, point: PhysicalPoint, click_count: usize) -> OverlayOutcome {
        let point = self.clamp_point(point);
        self.cursor_pos = point;
        match self.state {
            SelectionState::Idle => {
                // 按下点处的窗口先锁定，松开时若位移很小就直接选中该窗口
                self.refresh_window_hover(point);
                self.click_window = self.hover_window;
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
                    self.edge_tolerance(),
                    DEFAULT_MINIMUM_SELECTION_SIZE,
                );
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
                        self.copy_selection_and_close()
                    };
                }
                self.state = if mode == SelectionDragMode::None {
                    // 点击选区外，重新开始框选
                    SelectionState::MarqueeDragging {
                        start: point,
                        current: point,
                    }
                } else {
                    SelectionState::Reshaping {
                        mode,
                        origin_rect: rect,
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
        self.cursor_pos = point;
        self.update_magnifier_grid(point);
        if self.annotating {
            self.continue_annotation(point);
            return;
        }
        match self.state {
            SelectionState::MarqueeDragging { start, .. } => {
                if self.click_window.is_some() && exceeds_drag_threshold(start, point, self.drag_threshold()) {
                    // 位移超过阈值：放弃窗口选区，转为手动框选
                    self.click_window = None;
                    self.hover_window = None;
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
                self.hover_mode = hit_test_drag_mode(
                    rect,
                    point,
                    false,
                    self.edge_tolerance(),
                    DEFAULT_MINIMUM_SELECTION_SIZE,
                );
            }
            SelectionState::Idle => {
                self.hover_mode = SelectionDragMode::None;
                self.refresh_window_hover(point);
            }
        }
    }

    /// 处理鼠标左键释放。
    ///
    /// # 参数
    /// - `point`：底图物理坐标。
    pub fn handle_mouse_up(&mut self, point: PhysicalPoint) {
        let point = self.clamp_point(point);
        self.cursor_pos = point;
        if self.annotating {
            self.finish_annotation(point);
            return;
        }
        match self.state {
            SelectionState::MarqueeDragging { start, .. } => {
                let window = self.click_window.take();
                self.hover_window = None;
                if let Some(rect) = window
                    && !exceeds_drag_threshold(start, point, self.drag_threshold())
                    && rect.width >= DEFAULT_MINIMUM_SELECTION_SIZE
                    && rect.height >= DEFAULT_MINIMUM_SELECTION_SIZE
                {
                    self.state = SelectionState::Selected { rect };
                    return;
                }
                let r = marquee_selection_rect(start, point);
                self.state = if r.width >= DEFAULT_MINIMUM_SELECTION_SIZE
                    && r.height >= DEFAULT_MINIMUM_SELECTION_SIZE
                {
                    SelectionState::Selected { rect: r }
                } else {
                    SelectionState::Idle
                };
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
                    None,
                );
                self.state = SelectionState::Selected { rect };
                self.hover_mode = SelectionDragMode::None;
            }
            _ => {}
        }
    }

    /// 处理鼠标右键：有选区时先撤销选区，没有选区则关闭覆盖窗。
    pub fn handle_right_click(&mut self) -> OverlayOutcome {
        if self.state == SelectionState::Idle {
            return OverlayOutcome::Close;
        }
        self.state = SelectionState::Idle;
        self.hover_mode = SelectionDragMode::None;
        self.hover_window = None;
        self.click_window = None;
        self.reset_annotations();
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
        match (key, control) {
            // 翻译 / OCR 界面打开时，Esc 先退出该界面（回到框选状态），再按一次才关闭覆盖窗
            ("escape", _) if self.translate.is_visible() => {
                self.dismiss_translate();
                OverlayOutcome::Stay
            }
            ("escape", _) if self.ocr.is_visible() => {
                self.dismiss_ocr();
                OverlayOutcome::Stay
            }
            ("escape", _) => OverlayOutcome::Close,
            ("enter", _) if matches!(self.ocr, OcrUiState::Done { .. }) => self.copy_ocr_text_and_close(),
            ("enter", _) if matches!(self.translate, TranslateUiState::Done { .. }) => {
                self.copy_translation_and_close()
            }
            ("d", false) if matches!(self.translate, TranslateUiState::Failed { can_download: true, .. }) => {
                self.start_translate_download();
                OverlayOutcome::Stay
            }
            ("d", false) if matches!(self.ocr, OcrUiState::Failed { can_download: true, .. }) => {
                self.start_ocr_download();
                OverlayOutcome::Stay
            }
            ("enter", _) if self.record_mode => self.start_recording_and_close(),
            ("enter", _) if self.scroll_mode => self.start_scroll_capture_and_close(),
            // 录屏选区模式下没有复制 / 保存
            ("c", true) | ("s", true) if self.record_mode || self.scroll_mode => OverlayOutcome::Stay,
            ("enter", _) | ("c", true) => self.copy_selection_and_close(),
            ("s", true) => self.save_selection_and_close(),
            ("z", true) if shift => {
                self.redo_annotation();
                OverlayOutcome::Stay
            }
            ("z", true) => {
                self.undo_annotation();
                OverlayOutcome::Stay
            }
            ("y", true) => {
                self.redo_annotation();
                OverlayOutcome::Stay
            }
            ("c", false) => {
                self.copy_current_color();
                OverlayOutcome::Stay
            }
            _ => OverlayOutcome::Stay,
        }
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
        let next = if tool == self.tool { AnnotationTool::None } else { tool };
        let Some(layer) = self.annotations.as_mut() else {
            self.status_message = Some("标注功能不可用".into());
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
                self.status_message = Some(format!("切换工具失败: {e}"));
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
        self.style_config = Some(config);
        self.i18n = crate::ocr_backend::i18n_for(locale);
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
                label: self.i18n.tr_with("annot-size-px", &Args::new().arg(1, v)).into(),
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
        if let (Some(width), Some(font), Some(arrowhead)) = (made.next(), made.next(), made.next()) {
            self.style_ui = Some(StyleUi { width, font, arrowhead });
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
        let head = ArrowheadChoice::ALL.iter().position(|c| *c == style.arrowhead);
        ui.width.update(cx, |s, cx| s.set_selected_index(width, window, cx));
        ui.font.update(cx, |s, cx| s.set_selected_index(font, window, cx));
        ui.arrowhead
            .update(cx, |s, cx| s.set_selected_index(head.and_then(pick), window, cx));
    }

    /// 样式下拉选中：先提交进行中的文字输入，再改当前工具样式。
    fn on_style_select(&mut self, kind: StyleSelectKind, value: &str, window: &mut Window, cx: &mut Context<Self>) {
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
    fn swatch_row(&self, id: &'static str, colors: &[Rgba], current: Rgba, cx: &mut Context<Self>) -> Div {
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
                    .border_color(if picked { rgb(0xFFFFFF) } else { rgba(0xFFFFFF40) })
                    .bg(rgba(u32::from_be_bytes(color)))
                    .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                        this.on_style_color(color, window, cx);
                    })),
            );
        }
        row
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
        let label = move |id: &str| div().text_xs().text_color(rgba(STYLE_LABEL_COLOR)).child(i18n.tr(id));
        let select = |state: &Entity<StyleSelect>| {
            div().w(px(STYLE_SELECT_WIDTH)).h(px(STYLE_SELECT_HEIGHT)).child(
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
                panel = panel.child(label("annot-style-width")).child(select(&ui.width));
            }
            if fields.font_size {
                panel = panel.child(label("annot-style-font-size")).child(select(&ui.font));
            }
            if fields.arrowhead {
                panel = panel.child(label("annot-style-arrowhead")).child(select(&ui.arrowhead));
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
        Some(panel_placement(toolbar, TOOLBAR_LOGICAL_SIZE.1, STYLE_PANEL_SIZE, screen, STYLE_PANEL_GAP))
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
        op: impl FnOnce(&mut AnnotationLayer, crate::annotation::BaseView<'_>) -> Result<LayerUpdate, String>,
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
                self.status_message = Some(format!("标注失败: {e}"));
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
            self.probe.record_annotation(compute, started.elapsed(), tiles, bytes);
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
        match self.annotations.as_mut() {
            Some(layer) => layer.export_rgba(
                [rect.x, rect.y, rect.right(), rect.bottom()],
                self.frame.base_view(),
            ),
            None => self.frame.crop_rgba(rect),
        }
    }

    /// 复制选区到剪贴板；成功后关闭覆盖窗，失败保留窗口并提示。
    fn copy_selection_and_close(&mut self) -> OverlayOutcome {
        if !self.has_committed_selection() {
            self.status_message = Some("请先框选一个区域".into());
            return OverlayOutcome::Stay;
        }
        let Some((w, h, rgba)) = self.selection_image() else {
            self.status_message = Some("选区无效".into());
            return OverlayOutcome::Stay;
        };
        match self.output.copy_image(w, h, &rgba) {
            Ok(()) => {
                tracing::info!(width = w, height = h, "截图已复制到剪贴板");
                OverlayOutcome::Close
            }
            Err(e) => {
                tracing::error!(error = %e, "复制截图到剪贴板失败");
                self.status_message = Some(format!("复制失败: {e}"));
                OverlayOutcome::Stay
            }
        }
    }

    /// 保存选区为文件；成功后关闭覆盖窗，失败保留窗口并提示。
    fn save_selection_and_close(&mut self) -> OverlayOutcome {
        if !self.has_committed_selection() {
            self.status_message = Some("请先框选一个区域".into());
            return OverlayOutcome::Stay;
        }
        let Some((w, h, rgba)) = self.selection_image() else {
            self.status_message = Some("选区无效".into());
            return OverlayOutcome::Stay;
        };
        match self.output.save_image(w, h, &rgba) {
            Ok(path) => {
                tracing::info!(path = %path.display(), width = w, height = h, "截图已保存");
                OverlayOutcome::Close
            }
            Err(e) => {
                tracing::error!(error = %e, "保存截图失败");
                self.status_message = Some(format!("保存失败: {e}"));
                OverlayOutcome::Stay
            }
        }
    }

    /// 把选区（含标注合成结果）贴到屏幕原位；成功后关闭覆盖窗，失败保留窗口并提示。
    fn pin_selection_and_close(&mut self) -> OverlayOutcome {
        if !self.has_committed_selection() {
            self.status_message = Some("请先框选一个区域".into());
            return OverlayOutcome::Stay;
        }
        let Some(rect) = self
            .current_selection()
            .and_then(|r| self.screen_bounds.intersect(&r))
        else {
            self.status_message = Some("选区无效".into());
            return OverlayOutcome::Stay;
        };
        let Some((w, h, rgba)) = self.selection_image() else {
            self.status_message = Some("选区无效".into());
            return OverlayOutcome::Stay;
        };
        match self.output.pin_image(rect, w, h, rgba) {
            Ok(()) => {
                tracing::info!(rect = ?rect, width = w, height = h, "选区已贴图");
                OverlayOutcome::Close
            }
            Err(e) => {
                tracing::error!(error = %e, "贴图失败");
                self.status_message = Some(format!("贴图失败: {e}"));
                OverlayOutcome::Stay
            }
        }
    }

    /// 对选区（含标注合成结果）发起文字识别；识别在后台进行，结果经 [`Self::finish_ocr`] 回来。
    fn start_ocr(&mut self) -> OverlayOutcome {
        if !self.has_committed_selection() {
            self.status_message = Some("请先框选一个区域".into());
            return OverlayOutcome::Stay;
        }
        if self.ocr.is_busy() || self.translate.is_busy() {
            return OverlayOutcome::Stay;
        }
        if self.translate.is_visible() {
            self.dismiss_translate();
        }
        let Some((w, h, rgba)) = self.selection_image() else {
            self.status_message = Some("选区无效".into());
            return OverlayOutcome::Stay;
        };
        self.ocr_serial += 1;
        match self.output.start_ocr(self.ocr_serial, w, h, rgba) {
            Ok(()) => {
                tracing::info!(serial = self.ocr_serial, width = w, height = h, "已提交文字识别");
                self.set_ocr_state(OcrUiState::Running);
            }
            Err(e) => {
                tracing::error!(error = %e, "提交文字识别失败");
                self.set_ocr_state(OcrUiState::Failed {
                    message: format!("文字识别不可用: {e}"),
                    can_download: false,
                });
            }
        }
        OverlayOutcome::Stay
    }

    /// 切换 OCR 状态，同时刷新底部状态条。
    fn set_ocr_state(&mut self, state: OcrUiState) {
        self.status_message = state.status_text();
        self.ocr = state;
    }

    /// 退出 OCR 界面（回到框选状态）；识别仍在进行时，其结果会因序号过期被丢弃。
    fn dismiss_ocr(&mut self) {
        self.ocr_serial += 1;
        self.ocr = OcrUiState::Idle;
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
                tracing::info!(lines = r.boxes.len(), elapsed_ms = r.elapsed_ms, copied, "文字识别完成");
                OcrUiState::from_result(&r, copied)
            }
            Err(e) => {
                tracing::warn!(error = ?e, "文字识别失败");
                OcrUiState::from_error(&e)
            }
        };
        self.set_ocr_state(state);
    }

    /// 触发 OCR 组件下载（仅缺资产时可用）。
    fn start_ocr_download(&mut self) {
        match self.output.start_ocr_download() {
            Ok(()) => self.set_ocr_state(OcrUiState::Downloading("正在准备下载 OCR 组件…".to_string())),
            Err(e) => self.set_ocr_state(OcrUiState::Failed {
                message: format!("无法下载 OCR 组件: {e}"),
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
                self.status_message = Some("OCR 组件已就绪，请再次点击“OCR”".to_string());
            }
            Err(e) => self.set_ocr_state(OcrUiState::Failed {
                message: format!("OCR 组件下载失败: {e}"),
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
            self.status_message = Some("请先框选一个区域".into());
            return OverlayOutcome::Stay;
        }
        if self.translate.is_busy() || self.ocr.is_busy() {
            return OverlayOutcome::Stay;
        }
        if self.ocr.is_visible() {
            self.dismiss_ocr();
        }
        let Some((w, h, rgba)) = self.selection_image() else {
            self.status_message = Some("选区无效".into());
            return OverlayOutcome::Stay;
        };
        self.translate_serial += 1;
        match self.output.start_translate(self.translate_serial, w, h, rgba) {
            Ok(()) => {
                tracing::info!(serial = self.translate_serial, width = w, height = h, "已提交文字翻译");
                self.set_translate_state(TranslateUiState::Running(stage_text(TranslateStage::Recognizing).to_string()));
            }
            Err(e) => {
                tracing::error!(error = %e, "提交文字翻译失败");
                self.set_translate_state(TranslateUiState::Failed {
                    message: format!("文字翻译不可用: {e}"),
                    can_download: false,
                });
            }
        }
        OverlayOutcome::Stay
    }

    /// 切换翻译状态，同时刷新底部状态条。
    fn set_translate_state(&mut self, state: TranslateUiState) {
        self.status_message = state.status_text();
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
        if serial == self.translate_serial && matches!(self.translate, TranslateUiState::Running(_)) {
            self.set_translate_state(TranslateUiState::Running(stage_text(stage).to_string()));
        }
    }

    /// 收到翻译结果：成功则把译文复制到剪贴板并展示，失败则展示原因；过期结果被丢弃。
    ///
    /// # 参数
    /// - `serial`：结果对应的请求序号。
    /// - `result`：翻译产出或失败原因。
    pub fn finish_translate(&mut self, serial: u64, result: Result<TranslateOutcome, TranslateFlowError>) {
        if serial != self.translate_serial || !matches!(self.translate, TranslateUiState::Running(_)) {
            tracing::info!(serial, current = self.translate_serial, "丢弃过期的翻译结果");
            return;
        }
        let state = match result {
            Ok(outcome) => {
                let copied = outcome.translated.is_empty() || self.output.copy_text(&outcome.translated).is_ok();
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
                TranslateUiState::from_error(&e)
            }
        };
        self.set_translate_state(state);
    }

    /// 触发翻译运行时下载（仅缺运行时可用）。
    fn start_translate_download(&mut self) {
        match self.output.start_translate_download() {
            Ok(()) => self.set_translate_state(TranslateUiState::Downloading("正在准备下载 onnxruntime 运行时…".to_string())),
            Err(e) => self.set_translate_state(TranslateUiState::Failed {
                message: format!("无法下载运行时: {e}"),
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
                self.status_message = Some("翻译运行时已就绪，请再次点击“翻译”".to_string());
            }
            Err(e) => self.set_translate_state(TranslateUiState::Failed {
                message: format!("运行时下载失败: {e}"),
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
                self.status_message = Some(format!("复制译文失败: {e}"));
                OverlayOutcome::Stay
            }
        }
    }

    /// 翻译结果面板叠加层（无翻译界面时为空）。
    fn translate_overlay(&self, selection: PhysicalRect) -> Vec<AnyElement> {
        let lines = translate_panel_lines(&self.translate);
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
                self.status_message = Some(format!("复制文字失败: {e}"));
                OverlayOutcome::Stay
            }
        }
    }

    /// 以当前选区开始长截图；成功后关闭覆盖窗，失败保留窗口并提示。
    fn start_scroll_capture_and_close(&mut self) -> OverlayOutcome {
        if !self.has_committed_selection() {
            self.status_message = Some("请先框选要滚动截取的区域".into());
            return OverlayOutcome::Stay;
        }
        let Some(rect) = self
            .current_selection()
            .and_then(|r| self.screen_bounds.intersect(&r))
        else {
            self.status_message = Some("选区无效".into());
            return OverlayOutcome::Stay;
        };
        match self.output.start_scroll_capture(rect) {
            Ok(()) => {
                tracing::info!(rect = ?rect, "长截图选区已确认");
                OverlayOutcome::Close
            }
            Err(e) => {
                tracing::error!(error = %e, "启动长截图失败");
                self.status_message = Some(format!("启动长截图失败: {e}"));
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
        let lines = panel_lines(&self.ocr);
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
                self.status_message = Some(format!("已复制颜色 {text}"));
            }
            Err(e) => {
                tracing::error!(error = %e, "复制颜色失败");
                self.status_message = Some(format!("复制颜色失败: {e}"));
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
            Some(action) => self.apply_action(action.toolbar_action()),
            None => OverlayOutcome::Stay,
        }
    }

    /// 以当前选区开始录屏；成功后关闭覆盖窗，失败保留窗口并提示。
    fn start_recording_and_close(&mut self) -> OverlayOutcome {
        if !self.has_committed_selection() {
            self.status_message = Some("请先框选录制区域".into());
            return OverlayOutcome::Stay;
        }
        let Some(rect) = self.current_selection() else {
            self.status_message = Some("选区无效".into());
            return OverlayOutcome::Stay;
        };
        match self.output.start_recording(rect) {
            Ok(()) => {
                tracing::info!(rect = ?rect, "录屏选区已确认");
                OverlayOutcome::Close
            }
            Err(e) => {
                tracing::error!(error = %e, "启动录屏失败");
                self.status_message = Some(format!("启动录屏失败: {e}"));
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
        let (cx, cy) = (rect.x as f32 + rect.width as f32 / 2.0, rect.y as f32 + rect.height as f32 / 2.0);
        let (rx, ry) = (rect.width as f32 * 0.4, rect.height as f32 * 0.4);
        let angle = std::f32::consts::TAU * (0.25 * stroke as f32 + 0.6 * phase);
        let start = PhysicalPoint::new(
            (cx - rx * 0.6 + stroke as f32 * 24.0) as i32,
            (cy - ry * 0.6 + stroke as f32 * 24.0) as i32,
        );
        let end = PhysicalPoint::new((cx + rx * angle.cos()) as i32, (cy + ry * angle.sin()) as i32);
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
        if let Err(e) = window.drop_image(self.frame.image()) {
            tracing::warn!(error = %e, "释放底图图集失败");
        }
        self.flush_pending_drops(window);
        for (_, sprite) in self.tile_sprites.drain() {
            if let Err(e) = window.drop_image(sprite.image) {
                tracing::warn!(error = %e, "释放标注分块图集失败");
            }
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
        }
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
    fn on_tool_selected(&mut self, tool: AnnotationTool, window: &mut Window, cx: &mut Context<Self>) {
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
    fn begin_text_edit(&mut self, origin: PhysicalPoint, window: &mut Window, cx: &mut Context<Self>) {
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
        let input = cx.new(|cx| CanvasTextInput::with_text_and_style("", text_style, None, window, cx));
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

    /// 底部提示条的默认文案。
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
    /// 渲染覆盖窗视图。
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.flush_pending_annotation();
        let render_started = self.probe.render_start();
        self.flush_pending_drops(window);
        if self.scale_override.is_none() {
            self.scale = window.scale_factor();
        }
        let scale = self.scale;
        // 窗口点击候选期间不画框选遮罩，改画窗口高亮
        let sel = if self.click_window.is_some() { None } else { self.current_selection() };
        let highlight = self.window_highlight();
        let (frame_w, frame_h) = self.frame.size();
        let screen_w = frame_w as f32 / scale;
        let screen_h = frame_h as f32 / scale;
        let dragging = matches!(self.state, SelectionState::Reshaping { .. });

        let mut root = div()
            .relative()
            .size_full()
            .bg(rgb(0x000000))
            .overflow_hidden()
            .cursor(self.cursor_style(dragging))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, ev: &MouseDownEvent, window, cx| {
                    this.scale = this.scale_override.unwrap_or(window.scale_factor());
                    // 点击输入框以外的位置：先提交正在输入的文字
                    this.commit_text_edit(window, cx);
                    let p = this.physical_point(ev.position);
                    let outcome = this.handle_mouse_down(p, ev.click_count);
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
            .on_mouse_move(cx.listener(|this, ev: &MouseMoveEvent, window, cx| {
                let started = Instant::now();
                this.scale = this.scale_override.unwrap_or(window.scale_factor());
                let p = this.physical_point(ev.position);
                this.handle_mouse_move(p);
                this.probe.record_move(started.elapsed());
                cx.notify();
            }))
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|this, ev: &MouseUpEvent, window, cx| {
                    this.scale = this.scale_override.unwrap_or(window.scale_factor());
                    let p = this.physical_point(ev.position);
                    this.handle_mouse_up(p);
                    let outcome = this.auto_confirm_outcome();
                    this.finish(outcome, window, cx);
                }),
            )
            .on_key_down(cx.listener(|this, ev: &KeyDownEvent, window, cx| {
                let mods = ev.keystroke.modifiers;
                // 文字输入进行中：按键先交给输入框（字符本身走系统输入法通道）
                if this.route_key_to_text_edit(&ev.keystroke.key, mods.shift, mods.control, window, cx) {
                    cx.stop_propagation();
                    return;
                }
                let outcome = this.handle_key(&ev.keystroke.key, mods.control, mods.shift);
                this.finish(outcome, window, cx);
            }));
        if let Some(handle) = &self.focus_handle {
            root = root.track_focus(handle);
        }

        // 冻结底图：图像资源只构造一次，这里仅复用句柄
        root = root.child(
            img(ImageSource::Render(self.frame.image()))
                .absolute()
                .top(px(0.0))
                .left(px(0.0))
                .w(px(screen_w))
                .h(px(screen_h))
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
            let label_top = if wy >= LABEL_OFFSET { wy - LABEL_OFFSET } else { wy + 4.0 };
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
                    .child(selection_size_label(w)),
            );
        }

        let mut toolbar_pos: Option<PhysicalPoint> = None;
        let mut panel_pos: Option<PhysicalPoint> = None;
        if let Some(s) = sel {
            let (sx, sy) = (s.x as f32 / scale, s.y as f32 / scale);
            let (sw, sh) = (s.width as f32 / scale, s.height as f32 / scale);
            let mask_color = rgba(MASK_COLOR);

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

            // 选区框
            root = root.child(
                div()
                    .absolute()
                    .top(px(sy))
                    .left(px(sx))
                    .w(px(sw))
                    .h(px(sh))
                    .border_1()
                    .border_color(rgb(ACCENT_COLOR)),
            );

            // 八向手柄（边长保持约 8 个逻辑像素）
            let handle_size = (HANDLE_LOGICAL_SIZE * scale).round() as i32;
            for (_, hr) in handle_rects(s, handle_size) {
                root = root.child(
                    div()
                        .absolute()
                        .top(self.lp(hr.y))
                        .left(self.lp(hr.x))
                        .w(self.lp(hr.width))
                        .h(self.lp(hr.height))
                        .bg(rgba(0xFFFFFFFF))
                        .border_1()
                        .border_color(rgb(ACCENT_COLOR)),
                );
            }

            // 尺寸标签（选区左上方，贴近上沿时落到选区内侧）
            let label_top = if sy >= LABEL_OFFSET { sy - LABEL_OFFSET } else { sy + 4.0 };
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
                    .child(selection_size_label(s)),
            );

            // 浮动工具栏（选区确定后展示；标注工具未实现所以隐藏，贴图 / OCR / 翻译置灰）
            if matches!(self.state, SelectionState::Selected { .. }) {
                let screen_logical = PhysicalRect::new(0, 0, screen_w as i32, screen_h as i32);
                let pos = calculate_toolbar_placement(
                    logical_rect(s, scale),
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
                if let Some(origin) =
                    self.style_panel_origin((pos.x, pos.y), (screen_w as i32, screen_h as i32))
                {
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

        // OCR 结果：文本框描边 + 结果面板；翻译结果面板
        if let Some(s) = sel {
            for part in self.ocr_overlay(s) {
                root = root.child(part);
            }
            for part in self.translate_overlay(s) {
                root = root.child(part);
            }
        }

        // 文字输入框：点击框内不冒泡到根节点（否则会被当成“点击别处”而提交）
        if let Some(session) = &self.text_edit {
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
        if !self.cursor_over_toolbar(toolbar_pos) && !self.cursor_over_style_panel(panel_pos) {
            let screen_logical = PhysicalRect::new(0, 0, screen_w as i32, screen_h as i32);
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
            let mag = Magnifier::new("cursor-mag", self.magnifier_grid.clone(), self.cursor_pos)
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

        // 底部提示条（状态消息优先）
        let hint = self
            .status_message
            .clone()
            .unwrap_or_else(|| self.default_hint().to_string());
        root = root.child(
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

        self.probe.render_end(render_started);
        root
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
        fn pin_image(&mut self, region: PhysicalRect, w: u32, h: u32, rgba: Vec<u8>) -> Result<(), String> {
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
        fn start_translate(&mut self, serial: u64, w: u32, h: u32, rgba: Vec<u8>) -> Result<(), String> {
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
    fn view_with(w: u32, h: u32, scale: f32, fail: bool) -> (ScreenshotOverlayView, Rc<RefCell<Recorded>>) {
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
        let view = ScreenshotOverlayView::new(frame, scale, PhysicalPoint::new(0, 0), Box::new(sink));
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
        /// 点在矩形内返回矩形，否则无窗口。
        fn hover(&mut self, point: PhysicalPoint) -> Option<PhysicalRect> {
            let r = self.0;
            (point.x >= r.x && point.x < r.right() && point.y >= r.y && point.y < r.bottom()).then_some(r)
        }
    }

    /// 带假窗口来源（窗口 40,30 100x80）的视图。
    fn hover_view(scale: f32) -> ScreenshotOverlayView {
        let (mut view, _) = view_with(300, 200, scale, false);
        view.set_window_hover(Some(Box::new(FakeHover(PhysicalRect::new(40, 30, 100, 80)))));
        view
    }

    /// 空闲时悬停高亮窗口，移出窗口后高亮消失。
    #[test]
    fn idle_hover_highlights_window() {
        let mut view = hover_view(1.0);
        view.handle_mouse_move(PhysicalPoint::new(60, 50));
        assert_eq!(view.window_highlight(), Some(PhysicalRect::new(40, 30, 100, 80)));
        view.handle_mouse_move(PhysicalPoint::new(250, 150));
        assert_eq!(view.window_highlight(), None);
    }

    /// 单击（位移小于阈值）直接把窗口矩形作为选区。
    #[test]
    fn click_selects_hovered_window() {
        let mut view = hover_view(1.0);
        view.handle_mouse_move(PhysicalPoint::new(60, 50));
        drag(&mut view, (60, 50), (64, 53));
        assert_eq!(view.state, SelectionState::Selected { rect: PhysicalRect::new(40, 30, 100, 80) });
        assert_eq!(view.window_highlight(), None);
    }

    /// 位移超过阈值转为手动框选，行为与原来一致。
    #[test]
    fn drag_past_threshold_falls_back_to_marquee() {
        let mut view = hover_view(1.0);
        drag(&mut view, (60, 50), (120, 100));
        assert_eq!(view.state, SelectionState::Selected { rect: PhysicalRect::new(60, 50, 61, 51) });
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
        assert_eq!(view.window_highlight(), None);
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
        assert_eq!(view.current_selection(), Some(PhysicalRect::new(20, 20, 101, 71)));
        view.handle_mouse_up(PhysicalPoint::new(120, 90));
        assert!(matches!(view.state, SelectionState::Selected { .. }));
        assert_eq!(view.current_selection(), Some(PhysicalRect::new(20, 20, 101, 71)));
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
        assert!(view.screen_bounds.intersect(&sel) == Some(sel), "选区越界: {sel:?}");
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
            SelectionState::Reshaping { mode: SelectionDragMode::All, .. }
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
            SelectionState::Reshaping { mode: SelectionDragMode::BottomRight, .. }
        ));
        view.handle_mouse_move(PhysicalPoint::new(corner.x + 40, corner.y + 30));
        view.handle_mouse_up(PhysicalPoint::new(corner.x + 40, corner.y + 30));
        let after = view.current_selection().unwrap();
        assert_eq!((after.x, after.y), (sel.x, sel.y));
        assert_eq!((after.width, after.height), (sel.width + 40, sel.height + 30));
    }

    /// 点击选区外重新开始框选。
    #[test]
    fn click_outside_restarts_marquee() {
        let (mut view, _) = view_with(300, 200, 1.0, false);
        drag(&mut view, (20, 20), (80, 80));
        view.handle_mouse_down(PhysicalPoint::new(250, 180), 1);
        assert!(matches!(view.state, SelectionState::MarqueeDragging { .. }));
    }

    /// Esc 关闭，不写任何输出。
    #[test]
    fn escape_closes_without_output() {
        let (mut view, rec) = view_with(100, 80, 1.0, false);
        drag(&mut view, (10, 10), (60, 50));
        assert_eq!(view.handle_key("escape", false, false), OverlayOutcome::Close);
        let r = rec.borrow();
        assert!(r.images.is_empty() && r.texts.is_empty() && r.saved.is_empty());
    }

    /// Enter 把选区裁成 RGBA 写入剪贴板并关闭。
    #[test]
    fn enter_copies_cropped_rgba() {
        let (mut view, rec) = view_with(100, 80, 1.0, false);
        drag(&mut view, (10, 20), (29, 39));
        assert_eq!(view.handle_key("enter", false, false), OverlayOutcome::Close);
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
        assert_eq!(view.apply_action(ToolbarAction::Save), OverlayOutcome::Close);
        assert_eq!(view.apply_action(ToolbarAction::Copy), OverlayOutcome::Close);
        assert_eq!(view.apply_action(ToolbarAction::Cancel), OverlayOutcome::Close);
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
        view.start_annotation(PhysicalPoint::new(30, 30), view.current_selection().unwrap());
        view.finish_annotation(PhysicalPoint::new(120, 90));
        assert_eq!(view.apply_action(ToolbarAction::Pin), OverlayOutcome::Close);
        let r = rec.borrow();
        let (_, w, h, pinned) = &r.pins[0];
        let plain = view.frame.crop_rgba(view.current_selection().unwrap()).unwrap().2;
        assert_eq!(pinned.len(), (*w * *h * 4) as usize);
        assert_ne!(pinned, &plain, "贴图像素应包含标注");
    }

    /// 贴图失败时窗口保留并提示。
    #[test]
    fn pin_failure_keeps_window() {
        let (mut view, _) = view_with(100, 80, 1.0, true);
        drag(&mut view, (5, 5), (44, 34));
        assert_eq!(view.apply_action(ToolbarAction::Pin), OverlayOutcome::Stay);
        assert!(view.status_message.as_deref().unwrap().contains("贴图失败"));
    }

    /// 输出失败时窗口保留并给出错误提示。
    #[test]
    fn output_failure_keeps_window() {
        let (mut view, _) = view_with(100, 80, 1.0, true);
        drag(&mut view, (5, 5), (44, 34));
        assert_eq!(view.handle_key("enter", false, false), OverlayOutcome::Stay);
        assert!(view.status_message.as_deref().unwrap().contains("复制失败"));
        assert_eq!(view.apply_action(ToolbarAction::Save), OverlayOutcome::Stay);
        assert!(view.status_message.as_deref().unwrap().contains("保存失败"));
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
        assert_eq!(cursor_for_mode(SelectionDragMode::None, false), CursorStyle::Crosshair);
        assert_eq!(cursor_for_mode(SelectionDragMode::All, false), CursorStyle::OpenHand);
        assert_eq!(cursor_for_mode(SelectionDragMode::All, true), CursorStyle::ClosedHand);
        assert_eq!(cursor_for_mode(SelectionDragMode::Top, false), CursorStyle::ResizeUpDown);
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
        assert!(matches!(view.state, SelectionState::Selected { .. }) || view.state == SelectionState::Idle);
        assert_eq!(view.probe.summary().2.count, 50);
    }

    /// 自动动作：框选松手后只触发一次；未设置或无选区时不触发。
    #[test]
    fn auto_confirm_copies_once_after_selection() {
        let (mut view, rec) = view_with(255, 200, 1.0, false);
        view.set_auto_confirm(Some(AutoConfirm::Copy));
        assert_eq!(view.auto_confirm_outcome(), OverlayOutcome::Stay, "无选区不触发");
        drag(&mut view, (50, 50), (150, 120));
        assert_eq!(view.auto_confirm_outcome(), OverlayOutcome::Close);
        assert_eq!(rec.borrow().images.len(), 1);
        assert_eq!(view.auto_confirm_outcome(), OverlayOutcome::Stay, "只触发一次");
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
        assert_eq!(AutoConfirm::Translate.toolbar_action(), ToolbarAction::Translate);
    }

    /// 录屏选区模式：Enter 把选区交给录制流程并关闭；复制 / 保存快捷键不起作用。
    #[test]
    fn record_mode_enter_starts_recording() {
        let (mut view, rec) = view_with(255, 200, 1.0, false);
        view.set_record_mode(true);
        assert_eq!(view.handle_key("enter", false, false), OverlayOutcome::Stay, "无选区先提示");
        drag(&mut view, (50, 50), (150, 120));
        assert_eq!(view.handle_key("c", true, false), OverlayOutcome::Stay);
        assert_eq!(view.handle_key("s", true, false), OverlayOutcome::Stay);
        assert!(rec.borrow().images.is_empty() && rec.borrow().saved.is_empty());
        assert_eq!(view.handle_key("enter", false, false), OverlayOutcome::Close);
        assert_eq!(rec.borrow().records, vec![PhysicalRect::new(50, 50, 101, 71)]);
    }

    /// 工具栏“录屏”动作在截图模式下同样可用；输出失败时保留窗口。
    #[test]
    fn toolbar_record_action_and_failure() {
        let (mut view, rec) = view_with(255, 200, 1.0, false);
        drag(&mut view, (10, 10), (100, 100));
        assert_eq!(view.apply_action(ToolbarAction::Record), OverlayOutcome::Close);
        assert_eq!(rec.borrow().records.len(), 1);
        let (mut failing, _) = view_with(255, 200, 1.0, true);
        drag(&mut failing, (10, 10), (100, 100));
        assert_eq!(failing.apply_action(ToolbarAction::Record), OverlayOutcome::Stay);
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
        assert_eq!(view.handle_key("enter", false, false), OverlayOutcome::Close);
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
        assert!(tiles < total_tiles / 2.0, "平均每次更新 {tiles} 块应远小于整屏 {total_tiles}");
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
        assert_eq!(view.apply_action(ToolbarAction::Translate), OverlayOutcome::Stay);
        assert!(rec.borrow().translates.is_empty());
        drag(&mut view, (5, 5), (44, 34));
        assert_eq!(view.apply_action(ToolbarAction::Translate), OverlayOutcome::Stay);
        assert!(matches!(view.translate_state(), TranslateUiState::Running(_)));
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
        assert!(view.status_message.as_deref().is_some_and(|s| s.contains("翻译")));
        view.finish_translate(1, Ok(translate_outcome("你好")));
        assert!(matches!(view.translate_state(), TranslateUiState::Done { copied: true, .. }));
        assert_eq!(rec.borrow().texts, vec!["你好".to_string()]);
        assert!(view.status_message.as_deref().is_some_and(|s| s.contains("已复制")));
        assert_eq!(view.handle_key("enter", false, false), OverlayOutcome::Close);
        assert_eq!(rec.borrow().texts.len(), 2);

        let (mut view, _) = view_with(100, 80, 1.0, false);
        drag(&mut view, (5, 5), (44, 34));
        view.apply_action(ToolbarAction::Translate);
        view.finish_translate(1, Ok(translate_outcome("a")));
        assert_eq!(view.handle_key("escape", false, false), OverlayOutcome::Stay);
        assert_eq!(view.translate_state(), &TranslateUiState::Idle);
        assert_eq!(view.handle_key("escape", false, false), OverlayOutcome::Close);
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
            Err(TranslateFlowError::Translate(TranslateError::RuntimeMissing("未安装 onnxruntime 运行时".into()))),
        );
        let runtime = view.status_message.clone().unwrap_or_default();
        assert!(runtime.contains("运行时") && runtime.contains("按 D 下载"), "{runtime}");
        view.dismiss_translate();
        view.apply_action(ToolbarAction::Translate);
        view.finish_translate(
            view.translate_serial,
            Err(TranslateFlowError::Translate(TranslateError::NoModelFound("模型目录 D:/m 里没有可用的翻译模型".into()))),
        );
        let no_model = view.status_message.clone().unwrap_or_default();
        assert!(no_model.contains("D:/m") && !no_model.contains("按 D"), "{no_model}");
        view.dismiss_translate();
        view.apply_action(ToolbarAction::Translate);
        view.finish_translate(
            view.translate_serial,
            Err(TranslateFlowError::Ocr(OcrError::Unavailable(OcrUnavailable::NoRuntime))),
        );
        let ocr = view.status_message.clone().unwrap_or_default();
        assert!(ocr.contains("OCR"), "{ocr}");
        assert_ne!(runtime, no_model);
        assert!(rec.borrow().texts.is_empty(), "失败时不能写剪贴板");

        let (mut view, _) = view_with(100, 80, 1.0, true);
        drag(&mut view, (5, 5), (44, 34));
        view.apply_action(ToolbarAction::Translate);
        assert!(matches!(view.translate_state(), TranslateUiState::Failed { can_download: false, .. }));
    }

    /// 缺运行时时按 D 触发下载；进度与结果更新状态；成功回到待命，失败可重试。
    #[test]
    fn translate_runtime_download_flow() {
        use snow_translate::TranslateError;
        let (mut view, rec) = view_with(100, 80, 1.0, false);
        drag(&mut view, (5, 5), (44, 34));
        view.handle_key("d", false, false);
        assert_eq!(rec.borrow().translate_downloads, 0, "没有失败态时按 D 不下载");
        view.apply_action(ToolbarAction::Translate);
        view.finish_translate(
            1,
            Err(TranslateFlowError::Translate(TranslateError::RuntimeMissing("缺运行时".into()))),
        );
        assert_eq!(view.handle_key("d", false, false), OverlayOutcome::Stay);
        assert_eq!(rec.borrow().translate_downloads, 1);
        assert!(matches!(view.translate_state(), TranslateUiState::Downloading(_)));
        view.update_translate_download("正在下载 onnxruntime 运行时…");
        assert_eq!(view.status_message.as_deref(), Some("正在下载 onnxruntime 运行时…"));
        view.finish_translate_download(Err("网络断了".into()));
        assert!(matches!(view.translate_state(), TranslateUiState::Failed { can_download: true, .. }));
        view.handle_key("d", false, false);
        assert_eq!(rec.borrow().translate_downloads, 2);
        view.finish_translate_download(Ok(()));
        assert_eq!(view.translate_state(), &TranslateUiState::Idle);
        assert!(view.status_message.as_deref().is_some_and(|s| s.contains("再次点击")));
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
        assert!(matches!(view.translate_state(), TranslateUiState::Running(_)));
        assert_eq!(rec.borrow().translates.len(), 2);
    }

    /// 识别成功：文本复制到剪贴板、进入结果态；Enter 再复制并关闭；Esc 先退出 OCR 再关闭窗口。
    #[test]
    fn ocr_success_copies_text_and_enter_closes() {
        let (mut view, rec) = view_with(100, 80, 1.0, false);
        drag(&mut view, (5, 5), (44, 34));
        view.apply_action(ToolbarAction::Ocr);
        view.finish_ocr(1, Ok(ocr_result(&["hello", "世界"])));
        assert!(matches!(view.ocr_state(), OcrUiState::Done { copied: true, .. }));
        assert_eq!(rec.borrow().texts, vec!["hello\n世界".to_string()]);
        assert!(view.status_message.as_deref().is_some_and(|s| s.contains("已复制")));
        assert_eq!(view.handle_key("enter", false, false), OverlayOutcome::Close);
        assert_eq!(rec.borrow().texts.len(), 2);

        // Esc：先退出 OCR 界面，再按一次才关闭
        let (mut view, _) = view_with(100, 80, 1.0, false);
        drag(&mut view, (5, 5), (44, 34));
        view.apply_action(ToolbarAction::Ocr);
        view.finish_ocr(1, Ok(ocr_result(&["a"])));
        assert_eq!(view.handle_key("escape", false, false), OverlayOutcome::Stay);
        assert_eq!(view.ocr_state(), &OcrUiState::Idle);
        assert_eq!(view.handle_key("escape", false, false), OverlayOutcome::Close);
    }

    /// 空结果不复制文本（不覆盖用户剪贴板），提示“未识别到文字”。
    #[test]
    fn ocr_empty_result_does_not_touch_clipboard() {
        let (mut view, rec) = view_with(100, 80, 1.0, false);
        drag(&mut view, (5, 5), (44, 34));
        view.apply_action(ToolbarAction::Ocr);
        view.finish_ocr(1, Ok(ocr_result(&[])));
        assert!(rec.borrow().texts.is_empty());
        assert_eq!(view.status_message.as_deref(), Some("未识别到文字"));
    }

    /// 复制失败：仍展示结果，但状态条说明复制失败。
    #[test]
    fn ocr_copy_failure_is_reported() {
        // fail 输出会让提交本身失败，所以直接把状态推进到 Running 再喂结果
        let (mut view, _) = view_with(100, 80, 1.0, true);
        drag(&mut view, (5, 5), (44, 34));
        view.ocr = OcrUiState::Running;
        view.finish_ocr(view.ocr_serial, Ok(ocr_result(&["x"])));
        assert!(matches!(view.ocr_state(), OcrUiState::Done { copied: false, .. }));
        assert!(view.status_message.as_deref().is_some_and(|s| s.contains("失败")));
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
        assert!(no_runtime.contains("运行时") && no_runtime.contains("按 D 下载"), "{no_runtime}");
        view.dismiss_ocr();
        view.apply_action(ToolbarAction::Ocr);
        view.finish_ocr(view.ocr_serial, Err(OcrError::SessionNotReady));
        let not_ready = view.status_message.clone().unwrap_or_default();
        assert!(not_ready.contains("模型加载失败") && !not_ready.contains("按 D"), "{not_ready}");
        assert_ne!(no_runtime, not_ready);

        // 输出通道拒绝提交：直接进入失败态
        let (mut view, _) = view_with(100, 80, 1.0, true);
        drag(&mut view, (5, 5), (44, 34));
        view.apply_action(ToolbarAction::Ocr);
        assert!(matches!(view.ocr_state(), OcrUiState::Failed { can_download: false, .. }));
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
        view.finish_ocr(1, Err(OcrError::Unavailable(OcrUnavailable::NoModel { id: "m".into() })));
        assert_eq!(view.handle_key("d", false, false), OverlayOutcome::Stay);
        assert_eq!(rec.borrow().ocr_downloads, 1);
        assert!(matches!(view.ocr_state(), OcrUiState::Downloading(_)));
        view.update_ocr_download("正在下载 OCR 模型 (2/3)…");
        assert_eq!(view.status_message.as_deref(), Some("正在下载 OCR 模型 (2/3)…"));
        view.finish_ocr_download(Err("网络不可达".into()));
        assert!(matches!(view.ocr_state(), OcrUiState::Failed { can_download: true, .. }));
        view.handle_key("d", false, false);
        view.finish_ocr_download(Ok(()));
        assert_eq!(view.ocr_state(), &OcrUiState::Idle);
        assert!(view.status_message.as_deref().is_some_and(|s| s.contains("再次点击")));
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
        assert_eq!(view.apply_action(ToolbarAction::ScrollCapture), OverlayOutcome::Stay);
        assert!(rec.borrow().scrolls.is_empty());
        drag(&mut view, (5, 5), (44, 34));
        assert_eq!(view.apply_action(ToolbarAction::ScrollCapture), OverlayOutcome::Close);
        assert_eq!(rec.borrow().scrolls, vec![PhysicalRect::new(5, 5, 40, 30)]);
        // 输出通道失败：保留窗口并提示
        let (mut view, _) = view_with(100, 80, 1.0, true);
        drag(&mut view, (5, 5), (44, 34));
        assert_eq!(view.apply_action(ToolbarAction::ScrollCapture), OverlayOutcome::Stay);
        assert!(view.status_message.as_deref().is_some_and(|s| s.contains("长截图")));
    }

    /// 长截图选区模式：Enter 确认区域并交给长截图；复制 / 保存快捷键被禁用；Esc 仍可取消。
    #[test]
    fn scroll_mode_enter_starts_scroll_capture() {
        let (mut view, rec) = view_with(100, 80, 1.0, false);
        view.set_scroll_mode(true);
        assert_eq!(view.handle_key("enter", false, false), OverlayOutcome::Stay);
        assert!(view.status_message.as_deref().is_some_and(|s| s.contains("框选")));
        drag(&mut view, (5, 5), (44, 34));
        assert_eq!(view.handle_key("c", true, false), OverlayOutcome::Stay);
        assert_eq!(view.handle_key("s", true, false), OverlayOutcome::Stay);
        assert!(rec.borrow().images.is_empty() && rec.borrow().saved.is_empty());
        assert_eq!(view.handle_key("enter", false, false), OverlayOutcome::Close);
        assert_eq!(rec.borrow().scrolls, vec![PhysicalRect::new(5, 5, 40, 30)]);
        let (mut view, _) = view_with(100, 80, 1.0, false);
        view.set_scroll_mode(true);
        assert_eq!(view.handle_key("escape", false, false), OverlayOutcome::Close);
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
        assert_eq!(view.handle_key("enter", false, false), OverlayOutcome::Close);
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
        assert_eq!(view.tool_style(AnnotationTool::Arrow), crate::annotation_style::default_style(AnnotationTool::Arrow));
        // 再切回直线（选箭头会取消；再选直线）
        view.select_tool(AnnotationTool::Line);
        assert_eq!(view.tool_style(AnnotationTool::Line).color, [0x16, 0x77, 0xFF, 0xFF]);
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
        assert!(raw.contains("arrow_style") && raw.contains("#010203FF"), "配置应含箭头样式: {raw}");

        let (mut second, _) = view_with(100, 80, 1.0, false);
        second.set_style_config(Rc::new(RefCell::new(ConfigStore::open(&path))), "en-US");
        let arrow = second.tool_style(AnnotationTool::Arrow);
        assert_eq!((arrow.width, arrow.color, arrow.arrowhead), (12, [1, 2, 3, 255], ArrowheadChoice::Dot));
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
