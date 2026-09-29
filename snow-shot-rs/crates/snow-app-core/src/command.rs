//! 命令定义：`AppCommand`、请求载荷、命令来源与 MCP tool 映射（方案 §2.3）。
//!
//! UI 按钮、全局热键、托盘菜单、MCP 均为同一组命令的发射端。
//! 字段以现有 MCP schema 的真实含义为准；复杂载荷先建最小占位，待后续补全。

/// 截图呈现方式。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Presentation {
    /// 可见的编辑器会话。
    Visible,
    /// 静默会话（无界面）。
    Silent,
}

/// 发起截图会话时的采集目标。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CaptureTarget {
    /// 所有显示器。
    AllDisplays,
    /// 指定显示器（配合 `monitor_id`）。
    Monitor,
    /// 当前显示器。
    CurrentMonitor,
    /// 焦点窗口。
    FocusedWindow,
}

/// 直接采集的目标（比会话采集少两项）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DirectTarget {
    /// 当前显示器。
    CurrentMonitor,
    /// 焦点窗口。
    FocusedWindow,
}

/// 直接采集的输出去向。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DirectOutput {
    /// 渲染并返回。
    Render,
    /// 保存文件。
    Save,
    /// 复制到剪贴板。
    Copy,
}

/// 图片输出格式。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Format {
    /// PNG。
    Png,
    /// JPEG。
    Jpeg,
    /// WebP。
    Webp,
    /// AVIF。
    Avif,
    /// JPEG XL。
    Jxl,
    /// BMP。
    Bmp,
    /// PDF。
    Pdf,
}

/// 发起截图会话请求（对应 `screenshot_begin`）。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CaptureRequest {
    /// 呈现方式。
    pub presentation: Option<Presentation>,
    /// 采集目标。
    pub target: Option<CaptureTarget>,
    /// 显示器 ID（`target` 为 `Monitor` 时使用）。
    pub monitor_id: Option<String>,
    /// 是否采集光标。
    pub capture_cursor: Option<bool>,
    /// 是否启用智能选区。
    pub smart_selection: Option<bool>,
}

/// 直接采集请求（对应 `screenshot_direct_capture`）。
#[derive(Debug, Clone, PartialEq)]
pub struct DirectCaptureRequest {
    /// 采集目标。
    pub target: DirectTarget,
    /// 输出去向。
    pub output: DirectOutput,
    /// 是否采集光标。
    pub capture_cursor: Option<bool>,
    /// 缩放比例。
    pub scale: Option<f64>,
    /// 保存路径。
    pub path: Option<String>,
    /// 是否自动生成路径。
    pub automatic_path: Option<bool>,
    /// 输出格式。
    pub format: Option<Format>,
}

/// 选区合并方式。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RegionOperation {
    /// 替换。
    Replace,
    /// 加选。
    Add,
    /// 减选。
    Subtract,
}

/// 选区形状。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RegionType {
    /// 矩形。
    Rectangle,
    /// 多边形。
    Polygon,
    /// 折线。
    Polyline,
    /// 自由手绘。
    Freehand,
}

/// 设置选区请求（对应 `screenshot_set_selection`）。
#[derive(Debug, Clone, PartialEq)]
pub struct SelectionRequest {
    /// 合并方式。
    pub operation: Option<RegionOperation>,
    /// 形状。
    pub region_type: RegionType,
    /// 外接矩形 `[x, y, w, h]`。
    pub bounds: Option<[f64; 4]>,
    /// 顶点列表。
    pub points: Option<Vec<[f64; 2]>>,
}

/// 画布/识别工具种类（对应 MCP 的 CanvasTool）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ToolKind {
    /// 移动。
    Move,
    /// 选择。
    Select,
    /// 矩形。
    Rectangle,
    /// 箭头。
    Arrow,
    /// 直线。
    Line,
    /// 自由画笔。
    Freehand,
    /// 矩形高亮。
    RectangleHighlight,
    /// 画笔高亮。
    PenHighlight,
    /// 橡皮擦。
    Eraser,
    /// 矩形滤镜。
    RectangleFilter,
    /// 画笔滤镜。
    PenFilter,
    /// 文本。
    Text,
    /// 序号。
    SerialNumber,
    /// 水印。
    Watermark,
    /// 聚光灯。
    Spotlight,
    /// 自动滤镜。
    AutoFilter,
    /// 文字识别。
    Ocr,
    /// 表格识别。
    Table,
    /// 二维码识别。
    Qr,
    /// Markdown 识别。
    Markdown,
    /// HTML 识别。
    Html,
}

/// 渲染请求（对应 `screenshot_render`）。
#[derive(Debug, Clone, Default, PartialEq)]
pub struct RenderRequest {
    /// 缩放比例。
    pub scale: Option<f64>,
}

/// 保存请求（对应 `screenshot_save`）。
#[derive(Debug, Clone, Default, PartialEq)]
pub struct SaveRequest {
    /// 缩放比例。
    pub scale: Option<f64>,
    /// 保存路径。
    pub path: Option<String>,
    /// 是否自动生成路径。
    pub automatic_path: Option<bool>,
    /// 输出格式。
    pub format: Option<Format>,
    /// 编码质量。
    pub quality: Option<u32>,
}

/// 导出去向（保存 / 复制到剪贴板）。
#[derive(Debug, Clone, PartialEq)]
pub enum ExportTarget {
    /// 保存到文件。
    Save(SaveRequest),
    /// 复制到剪贴板（对应 `screenshot_copy`）。
    Copy,
}

/// 滚动截图动作。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ScrollAction {
    /// 开始。
    Start,
    /// 设置轴向。
    SetAxis,
    /// 自动滚动。
    AutoScroll,
    /// 移动。
    Move,
    /// 裁剪。
    Trim,
    /// 结束。
    Stop,
}

/// 滚动轴向。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ScrollAxis {
    /// 垂直。
    Vertical,
    /// 水平。
    Horizontal,
}

/// 滚动截图请求（对应 `screenshot_scrolling`）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScrollingRequest {
    /// 动作。
    pub action: ScrollAction,
    /// 轴向。
    pub axis: Option<ScrollAxis>,
    /// 开关。
    pub enabled: Option<bool>,
    /// 偏移 `[x, y]`。
    pub offset: Option<[i32; 2]>,
    /// 裁剪起点。
    pub start: Option<u32>,
    /// 裁剪终点。
    pub end: Option<u32>,
}

/// 识别种类。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RecognitionKind {
    /// 文本。
    Text,
    /// 表格。
    Table,
    /// 二维码。
    Qr,
    /// Markdown。
    Markdown,
    /// HTML。
    Html,
}

/// 识别请求（对应 `screenshot_recognize`）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OcrRequest {
    /// 识别种类。
    pub kind: RecognitionKind,
}

/// 翻译请求（对应 `screenshot_translate`，该 tool 无额外入参）。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TranslateRequest {}

/// 按 ID 引用的操作请求（对应 `screenshot_operation`）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OperationRequest {
    /// 操作 ID。
    pub operation_id: String,
}

/// 录制配置（`recording_options` 的子集）。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RecordingConfig {
    /// 输出路径。
    pub path: Option<String>,
    /// 输出格式。
    pub format: Option<String>,
    /// 开始延迟（秒）。
    pub start_delay_seconds: Option<u32>,
    /// 是否录麦克风。
    pub microphone: Option<bool>,
    /// 是否录系统音频。
    pub system_audio: Option<bool>,
    /// 帧率。
    pub frame_rate: Option<u32>,
}

/// 生成一个仅含 `target` 的最小占位请求结构体（字段待后续补全）。
macro_rules! placeholder_request {
    ($(#[$meta:meta])* $name:ident) => {
        $(#[$meta])*
        #[derive(Debug, Clone, Default, PartialEq, Eq)]
        pub struct $name {
            /// 样式/编辑目标名（占位，字段待后续补全）。
            pub target: Option<String>,
        }
    };
}

placeholder_request!(
    /// 选区样式请求（对应 `screenshot_set_selection_style`，占位）。
    SelectionStyleRequest
);
placeholder_request!(
    /// 工具样式请求（对应 `screenshot_set_tool_style`，占位）。
    ToolStyleRequest
);
placeholder_request!(
    /// 应用标注请求（对应 `screenshot_apply_annotations`，占位）。
    AnnotationsRequest
);
placeholder_request!(
    /// 元素编辑请求（对应 `screenshot_edit_elements`，占位）。
    EditElementsRequest
);
placeholder_request!(
    /// 模板绘制请求（对应 `screenshot_draw_template`，占位）。
    DrawTemplateRequest
);
placeholder_request!(
    /// 自动滤镜请求（对应 `screenshot_auto_filter`，占位）。
    AutoFilterRequest
);
placeholder_request!(
    /// 识别结果编辑请求（对应 `screenshot_edit_recognition`，占位）。
    EditRecognitionRequest
);
placeholder_request!(
    /// 识别结果导出请求（对应 `screenshot_export_recognition`，占位）。
    ExportRecognitionRequest
);

/// 应用命令：所有发射端共用的命令集合。
#[derive(Debug, Clone, PartialEq)]
pub enum AppCommand {
    /// 发起截图会话。
    Capture(CaptureRequest),
    /// 直接采集。
    DirectCapture(DirectCaptureRequest),
    /// 查询会话状态。
    QueryState,
    /// 查询 MCP 服务状态。
    McpStatus,
    /// 设置选区。
    SetSelection(SelectionRequest),
    /// 切换工具。
    SelectTool(ToolKind),
    /// 设置选区样式。
    SetSelectionStyle(SelectionStyleRequest),
    /// 设置工具样式。
    SetToolStyle(ToolStyleRequest),
    /// 应用标注。
    ApplyAnnotations(AnnotationsRequest),
    /// 编辑元素。
    EditElements(EditElementsRequest),
    /// 绘制模板。
    DrawTemplate(DrawTemplateRequest),
    /// 自动滤镜。
    AutoFilter(AutoFilterRequest),
    /// 撤销。
    Undo,
    /// 重做。
    Redo,
    /// 渲染。
    Render(RenderRequest),
    /// 导出（保存/复制）。
    Export(ExportTarget),
    /// 将选区钉为贴图。
    PinSelection,
    /// 完成会话。
    Finish,
    /// 取消会话。
    Cancel,
    /// 重新采集。
    Recapture,
    /// 滚动截图。
    Scrolling(ScrollingRequest),
    /// 单次滚动。
    ScrollOnce,
    /// 运行识别。
    RunOcr(OcrRequest),
    /// 翻译。
    Translate(TranslateRequest),
    /// 编辑识别结果。
    EditRecognition(EditRecognitionRequest),
    /// 导出识别结果。
    ExportRecognition(ExportRecognitionRequest),
    /// 执行指定操作。
    RunOperation(OperationRequest),
    /// 开始录制（属 recording_pinned 域，不在 28 个截图 tool 内）。
    StartRecording(RecordingConfig),
}

/// 生成 `CommandKind` 枚举与 `AppCommand::kind()`，保证两者变体同步。
macro_rules! command_kinds {
    ($($variant:ident $(($pat:tt))?),* $(,)?) => {
        /// 命令种类：`AppCommand` 的无载荷判别，用于 handler 注册。
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
        pub enum CommandKind {
            $(
                /// 对应同名 `AppCommand` 变体。
                $variant,
            )*
        }

        impl AppCommand {
            /// 取命令种类。
            ///
            /// # 返回
            /// 该命令对应的 `CommandKind`。
            ///
            /// ```rust
            /// use snow_app_core::command::{AppCommand, CommandKind};
            /// assert_eq!(AppCommand::Undo.kind(), CommandKind::Undo);
            /// ```
            pub fn kind(&self) -> CommandKind {
                match self {
                    $( AppCommand::$variant $(($pat))? => CommandKind::$variant, )*
                }
            }
        }
    };
}

command_kinds!(
    Capture(_),
    DirectCapture(_),
    QueryState,
    McpStatus,
    SetSelection(_),
    SelectTool(_),
    SetSelectionStyle(_),
    SetToolStyle(_),
    ApplyAnnotations(_),
    EditElements(_),
    DrawTemplate(_),
    AutoFilter(_),
    Undo,
    Redo,
    Render(_),
    Export(_),
    PinSelection,
    Finish,
    Cancel,
    Recapture,
    Scrolling(_),
    ScrollOnce,
    RunOcr(_),
    Translate(_),
    EditRecognition(_),
    ExportRecognition(_),
    RunOperation(_),
    StartRecording(_),
);

/// MCP 截图域 28 个 tool（去掉 `snow_shot_` 前缀）到命令种类的映射。
pub const MCP_TOOL_MAP: &[(&str, CommandKind)] = &[
    ("mcp_status", CommandKind::McpStatus),
    ("screenshot_begin", CommandKind::Capture),
    ("screenshot_state", CommandKind::QueryState),
    ("screenshot_set_selection", CommandKind::SetSelection),
    ("screenshot_set_tool", CommandKind::SelectTool),
    (
        "screenshot_apply_annotations",
        CommandKind::ApplyAnnotations,
    ),
    ("screenshot_undo", CommandKind::Undo),
    ("screenshot_redo", CommandKind::Redo),
    ("screenshot_render", CommandKind::Render),
    ("screenshot_save", CommandKind::Export),
    ("screenshot_copy", CommandKind::Export),
    ("screenshot_pin", CommandKind::PinSelection),
    ("screenshot_finish", CommandKind::Finish),
    ("screenshot_cancel", CommandKind::Cancel),
    ("screenshot_direct_capture", CommandKind::DirectCapture),
    (
        "screenshot_set_selection_style",
        CommandKind::SetSelectionStyle,
    ),
    ("screenshot_set_tool_style", CommandKind::SetToolStyle),
    ("screenshot_edit_elements", CommandKind::EditElements),
    ("screenshot_recapture", CommandKind::Recapture),
    ("screenshot_scrolling", CommandKind::Scrolling),
    ("screenshot_scroll_once", CommandKind::ScrollOnce),
    ("screenshot_recognize", CommandKind::RunOcr),
    ("screenshot_translate", CommandKind::Translate),
    ("screenshot_auto_filter", CommandKind::AutoFilter),
    ("screenshot_operation", CommandKind::RunOperation),
    ("screenshot_edit_recognition", CommandKind::EditRecognition),
    (
        "screenshot_export_recognition",
        CommandKind::ExportRecognition,
    ),
    ("screenshot_draw_template", CommandKind::DrawTemplate),
];

/// 命令来源。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CommandSource {
    /// UI 按钮/菜单。
    Ui,
    /// 全局热键。
    Hotkey,
    /// 托盘菜单。
    Tray,
    /// MCP tool。
    Mcp,
    /// 测试直驱。
    Test,
}

/// 命令上下文：来源与会话元数据（MCP Mutation 的公共入参）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandContext {
    /// 命令来源。
    pub source: CommandSource,
    /// 会话 ID。
    pub session_id: Option<String>,
    /// 期望的会话版本号。
    pub expected_revision: Option<u64>,
    /// 幂等键。
    pub idempotency_key: Option<String>,
}

impl CommandContext {
    /// 以来源创建仅含来源的上下文。
    ///
    /// # 参数
    /// - `source`：命令来源。
    ///
    /// # 返回
    /// 其余字段为空的上下文。
    ///
    /// ```rust
    /// use snow_app_core::command::{CommandContext, CommandSource};
    /// let ctx = CommandContext::new(CommandSource::Ui);
    /// assert!(ctx.session_id.is_none());
    /// ```
    pub fn new(source: CommandSource) -> Self {
        Self {
            source,
            session_id: None,
            expected_revision: None,
            idempotency_key: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    /// MCP 映射恰好 28 项且 tool 名不重复。
    #[test]
    fn mcp_tool_map_has_28_unique_names() {
        assert_eq!(MCP_TOOL_MAP.len(), 28);
        let names: HashSet<_> = MCP_TOOL_MAP.iter().map(|(n, _)| *n).collect();
        assert_eq!(names.len(), 28);
    }

    /// 命令能还原为对应种类。
    #[test]
    fn command_kind_roundtrip() {
        assert_eq!(AppCommand::Undo.kind(), CommandKind::Undo);
        assert_eq!(
            AppCommand::SelectTool(ToolKind::Arrow).kind(),
            CommandKind::SelectTool
        );
        assert_eq!(
            AppCommand::Export(ExportTarget::Copy).kind(),
            CommandKind::Export
        );
    }

    /// 上下文默认仅含来源。
    #[test]
    fn context_new_is_minimal() {
        let ctx = CommandContext::new(CommandSource::Tray);
        assert_eq!(ctx.source, CommandSource::Tray);
        assert!(ctx.expected_revision.is_none() && ctx.idempotency_key.is_none());
    }
}
