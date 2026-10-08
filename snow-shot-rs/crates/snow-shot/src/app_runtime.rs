//! 常驻运行时：托盘、全局热键、单实例 IPC 的事件汇入 GPUI 主线程并分发。
//!
//! 数据流：热键 / 托盘 / IPC 线程 → [`UiEvent`] → [`MainThreadInbox`] → GPUI 主线程
//! [`handle_event`]。非主线程只做 `push`，不接触任何 GPUI 对象。
//!
//! 所属阶段：A（事件循环骨架）+ B1（截图触发 → 采集光标所在显示器 → 冻结覆盖窗）。
//! 采集在后台线程完成，结果经收件箱回到主线程再建窗。

use crate::capture_flow::{CaptureCollector, CapturePayload, pick_monitor, spawn_capture};
use crate::desktop_frames::{DesktopFrames, MonitorFrame};
use crate::dictation::config::DictationConfig;
use crate::dictation::focus::Verdict;
use crate::dictation::translate::TranslationOutcome;
use crate::dictation::{DictationCommand, DictationHost};
use crate::direct_capture::{DirectHistory, DirectResult, spawn_direct_capture};
use crate::frozen_frame::FrozenFrame;
use crate::history_nav::ThreadedHistoryProvider;
use crate::history_store::{
    HistoryRecorder, HistorySource, HistoryStore, Thumbnail, policy_from_document,
};
use crate::history_view::{HistoryAction, HistoryView};
use crate::main_window_model::{SIDEBAR_COLLAPSED_KEY, TRANSLATION_PAGE_ENABLED_KEY};
use crate::main_window_view::MainWindowView;
use crate::ocr_assets::{ENV_OCR_ASSET_DIR, ocr_root};
use crate::ocr_backend::{OcrInput, select_from_document};
use crate::ocr_client::OcrError;
use crate::ocr_download;
use crate::ocr_service::{OcrRequestConfig, OcrResult, OcrService};
use crate::ort_runtime;
use crate::overlay_view::{
    AutoConfirm, OverlayOutcome, OverlayWindowView, ScreenshotOverlayView, SystemOutput,
};
use crate::pinned_manage_view::PinManageView;
use crate::pinned_manager::PinnedManager;
use crate::pinned_shared::PinError;
use crate::quick_actions::{
    DELAY_SECONDS_CONFIG_KEY, DelayGate, DirectKind, NOTICE_FULLSCREEN_GATE_OFF,
    NOTICE_FULLSCREEN_GATE_ON, NOTICE_NO_SELECTED_TEXT, NOTICE_PIN_SELECTED_FILES,
    NOTICE_RESTORE_CLOSED, QUICK_ACTION_KEYS, QuickPlan, clip_to_monitor, delay_seconds,
    direct_output_plan_from, full_monitor_region, plan_for, recording_directory,
    stays_registered_when_paused,
};
use crate::recording_flow::{
    ENV_RECORDING_AUTOTEST, RecordingHost, monitor_for_region, parse_autotest,
};
use crate::screenshot_output::{
    ExportSettings, configured_format, export_direct, home_directory, resolve_save_directory,
};
use crate::scroll_view::{ENV_SCROLL_AUTOTEST, ScrollHost, parse_scroll_autotest};
use crate::settings_model::portable_to_hotkey_text;
use crate::settings_model::{LANGUAGE_KEY, THEME_COLOR_KEY, THEME_MODE_KEY};
use crate::settings_state::{ConfigChange, SharedConfig, SystemPrefs, UiPrefs, restore_value};
use crate::settings_text::{Lang, window_title};
use crate::settings_view::{AUTOTEST_STEP_INTERVAL, SettingsView, parse_autotest_ops};
use crate::stt_download::{self, Progress as SttProgress};
use crate::stt_models;
use crate::stt_settings::{CancelFlag, SttHooks};
use crate::sys_prefs::system_ui_language;
use crate::translate_flow::TranslateUiState;
use crate::translate_history::history_path;
use crate::translate_input::{InputError, translate_text};
use crate::translate_input_view::{
    TranslateInputView, WINDOW_HEIGHT as TRANSLATE_INPUT_HEIGHT,
    WINDOW_WIDTH as TRANSLATE_INPUT_WIDTH,
};
use crate::translate_page::{KEY_PAGE_AUTO_TRANSLATE, page_config};
use crate::translate_page_view::{
    PageOptions, TranslatePageView, WINDOW_HEIGHT as TRANSLATE_PAGE_HEIGHT,
    WINDOW_WIDTH as TRANSLATE_PAGE_WIDTH,
};
use crate::translate_service::{
    TranslateConfig, TranslateFlowError, TranslateHost, TranslateOutcome, TranslateStage,
    Translated, run_flow,
};
use crate::window_geometry::{
    MAIN_MIN_HEIGHT, MAIN_MIN_WIDTH, MAIN_WINDOW_GEOMETRY_KEY, TRANSLATE_MIN_HEIGHT,
    TRANSLATE_MIN_WIDTH, TRANSLATION_WINDOW_SIZE_KEY, clamp_size, fit_geometry, geometry_to_json,
    parse_geometry, parse_window_size, size_to_json,
};
use crate::window_pick::{
    WindowHover, selection_target, start_window_hover, transition_animation_enabled,
};
use serde_json::Value;
use snow_app_core::PRODUCT_NAME;
use snow_app_core::bus::{CommandBus, CommandError, CommandOutcome};
use snow_app_core::command::{
    AppCommand, CaptureRequest, CommandKind, CommandSource, DirectCaptureRequest, DirectOutput,
    DirectTarget, ExportTarget, QuickAction, RecordingConfig as RecordingRequest,
};
use snow_capability::CapabilityRegistry;
use snow_config::document::ConfigDocument;
use snow_config::paths::config_file_path;
use snow_config::store::ConfigStore;
use snow_i18n::Args;
use snow_platform::single_instance::IpcCommand;
use snow_ui::shell::dispatch::Dispatcher;
use snow_ui::shell::geometry::{LogicalSize, PhysicalPoint, PhysicalRect};
use snow_ui::shell::hotkey::{Hotkey, HotkeyBinding, HotkeyHandle, HotkeyService};
use snow_ui::shell::inbox::MainThreadInbox;
use snow_ui::shell::monitor::{MonitorInfo, MonitorTarget};
use snow_ui::shell::overlay::cursor_screen_position;
use snow_ui::shell::tray::{TrayAction, TrayIconImage, TrayMenuEntry, TrayService, TraySpec};
use snow_ui::shell::window::{Placement, WindowSpec};
use snow_ui::ui::AppContext;
use snow_ui::ui::{Entity, ShellContext, ShellWindow};
use snow_ui::widgets::{AnnotationTool, ToolbarAction};
use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::time::Duration;

/// 设置页自动化脚本路径环境变量（JSON 操作数组，验收用）。
pub const ENV_SETTINGS_AUTOTEST: &str = "SNOW_SETTINGS_AUTOTEST";
/// 设置窗所在显示器的设备名子串环境变量（如 `DISPLAY2`，验收用）。
pub const ENV_SETTINGS_MONITOR: &str = "SNOW_SETTINGS_MONITOR";
/// 托盘悬停提示。
pub(crate) const TRAY_TOOLTIP: &str = PRODUCT_NAME;
/// 托盘信号：从剪贴板贴图。
pub const TRAY_SIGNAL_PIN_CLIPBOARD: &str = "pin_clipboard";
/// 托盘信号：打开截图历史。
pub const TRAY_SIGNAL_HISTORY: &str = "history";
/// 托盘信号：打开主窗口。
pub const TRAY_SIGNAL_MAIN_WINDOW: &str = "main_window";
/// 托盘信号：打开设置。
pub const TRAY_SIGNAL_SETTINGS: &str = "settings";
/// 托盘信号：退出。
pub const TRAY_SIGNAL_QUIT: &str = "quit";
/// 托盘信号前缀：切换贴图分组（后接分组 ID）。
pub const TRAY_SIGNAL_GROUP_PREFIX: &str = "group:";
/// 托盘信号：打开贴图管理窗口。
pub const TRAY_SIGNAL_PIN_MANAGE: &str = "pin_manage";
/// 托盘信号：新建贴图分组。
pub const TRAY_SIGNAL_GROUP_NEW: &str = "group_new";
/// 托盘信号：删除空的贴图分组。
pub const TRAY_SIGNAL_GROUP_DELETE_EMPTY: &str = "group_delete_empty";
/// 设置窗口逻辑宽度。
const SETTINGS_WINDOW_WIDTH: f32 = 1000.0;
/// 设置窗口逻辑高度。
const SETTINGS_WINDOW_HEIGHT: f32 = 700.0;
/// 截图全局热键的配置键。
pub const SCREENSHOT_HOTKEY_CONFIG_KEY: &str = "global_shortcuts/screenshot";
/// 录屏全局热键的配置键。
pub const RECORDING_HOTKEY_CONFIG_KEY: &str = "global_shortcuts/screen_record";
/// “贴图剪贴板内容”全局热键的配置键。
pub const PIN_CLIPBOARD_HOTKEY_CONFIG_KEY: &str = "global_shortcuts/pin_clipboard_content";
/// “输入框翻译浮窗”全局热键的配置键。
pub const TRANSLATE_INPUT_HOTKEY_CONFIG_KEY: &str =
    snow_config::extensions::KEY_TRANSLATE_INPUT_HOTKEY;
/// “语音转文字·切换式”全局热键的配置键。
pub const DICTATION_TOGGLE_HOTKEY_CONFIG_KEY: &str =
    snow_config::extensions::KEY_DICTATION_TOGGLE_HOTKEY;
/// “语音转文字·按住说话”全局热键的配置键。
pub const DICTATION_HOLD_HOTKEY_CONFIG_KEY: &str =
    snow_config::extensions::KEY_DICTATION_HOLD_HOTKEY;
/// 语音转文字触发模式的配置键（决定上面两个热键哪个生效）。
pub const DICTATION_TRIGGER_MODE_CONFIG_KEY: &str =
    snow_config::extensions::KEY_DICTATION_TRIGGER_MODE;
/// “语音转文字·切换式”全局热键的配置键。
const PORTABLE_FIELD: &str = "portable";
/// 事件来源标签：全局热键。
pub const ORIGIN_HOTKEY: &str = "hotkey";
/// 事件来源标签：托盘。
pub const ORIGIN_TRAY: &str = "tray";
/// 事件来源标签：单实例 IPC。
pub const ORIGIN_IPC: &str = "ipc";
/// 事件来源标签：其它。
pub const ORIGIN_OTHER: &str = "other";
/// 事件来源标签：录屏选区流程。
pub const ORIGIN_RECORDING: &str = "recording";
/// 事件来源标签：长截图选区流程。
pub const ORIGIN_SCROLL: &str = "scroll";
/// 来源标签：全局鼠标手势。
pub const ORIGIN_GESTURE: &str = "gesture";
/// 性能基准环境变量：值为步数（正整数）时，覆盖窗打开后自动跑一遍模拟框选并自动关闭。
pub const ENV_OVERLAY_BENCH: &str = "SNOW_OVERLAY_BENCH";
/// 性能基准的合成底图尺寸环境变量，格式 `宽x高`（如 `3840x2160`）。
pub const ENV_OVERLAY_SYNTH: &str = "SNOW_OVERLAY_SYNTH";
/// 标注性能基准的工具名环境变量（rectangle / ellipse / arrow / line / pencil / mosaic / blur / text）；
/// 设置后基准改为在选区内绘制该工具的标注而不是框选。
pub const ENV_OVERLAY_BENCH_TOOL: &str = "SNOW_OVERLAY_BENCH_TOOL";
/// 设置为任意值时基准结束后不自动关闭覆盖窗（便于外部截屏取像素，用 Esc 关闭）。
pub const ENV_OVERLAY_BENCH_HOLD: &str = "SNOW_OVERLAY_BENCH_HOLD";
/// 设置为任意值时基准结束后触发“贴图”动作（验收贴图入口用，不经过系统输入）。
pub const ENV_OVERLAY_BENCH_PIN: &str = "SNOW_OVERLAY_BENCH_PIN";
/// 设置为任意值时基准结束后触发工具栏“OCR”动作（验收文字识别入口用，覆盖窗保持打开）。
pub const ENV_OVERLAY_BENCH_OCR: &str = "SNOW_OVERLAY_BENCH_OCR";
/// 设置为任意值时基准结束后触发工具栏“翻译”动作（验收文字翻译入口用，覆盖窗保持打开）。
pub const ENV_OVERLAY_BENCH_TRANSLATE: &str = "SNOW_OVERLAY_BENCH_TRANSLATE";
/// 翻译缺运行时时自动按 D 下载（值为 `download`），或下载成功后再自动重新触发“翻译”（其它非空值）；验收下载链路用，驱动视图状态，不经过系统输入。
pub const ENV_OVERLAY_BENCH_TRANSLATE_AUTO: &str = "SNOW_OVERLAY_BENCH_TRANSLATE_AUTO";
/// 设置为任意值时基准结束后触发工具栏“长图”动作（验收长截图入口用）。
pub const ENV_OVERLAY_BENCH_SCROLL: &str = "SNOW_OVERLAY_BENCH_SCROLL";
/// 性能基准的步进间隔（约 60Hz）。
const BENCH_FRAME_INTERVAL: Duration = Duration::from_millis(16);

/// 汇入 GPUI 主线程的事件。
#[derive(Debug, Clone, PartialEq)]
pub enum UiEvent {
    /// 请求截图（`origin` 为来源标签）。
    Capture {
        /// 来源标签（`ORIGIN_*`）。
        origin: &'static str,
    },
    /// 后台采集完成，携带冻结帧。
    CaptureReady(CapturePayload),
    /// 后台采集失败。
    CaptureFailed(String),
    /// 覆盖窗关闭了：同一会话里其余显示器上的覆盖窗跟着关闭。
    OverlayClosed,
    /// 打开（或激活）设置窗口。
    OpenSettings,
    /// MCP 请求：需要主线程数据或能力，处理后经请求自带的通道回复。
    Mcp(crate::mcp_host::McpRequest),
    /// 重启应用：先拉起延迟启动的新实例，再正常退出。
    Restart,
    /// 请求录制：进入选区，确认后拉起独立的录制进程。
    StartRecording,
    /// 导出命令（保存 / 复制），作用于当前覆盖窗里的选区。
    Export(ExportTarget),
    /// 直接截图：不进入选区，采集后直接复制或保存。
    DirectCapture(DirectCaptureRequest),
    /// 打开（或激活）输入框翻译浮窗。
    OpenTranslateInput,
    /// 打开（或激活）翻译页窗口（主窗口导航等入口投递此事件）。
    OpenTranslatePage,
    /// 翻译页请求翻译（在后台线程执行）。
    TranslatePageRequested {
        /// 请求序号（回传结果时带回）。
        serial: u64,
        /// 用户输入的原文。
        text: String,
        /// 下拉选中的包 ID（空串为自动）。
        model_id: String,
        /// 页面上选的源语言。
        source: snow_translate::Lang,
        /// 页面上选的目标语言。
        target: snow_translate::Lang,
        /// 是否来自主窗口内嵌的翻译页（决定结果回给哪一份视图）。
        embedded: bool,
    },
    /// 翻译页翻译完成（成功或失败）。
    TranslatePageFinished {
        /// 对应的请求序号。
        serial: u64,
        /// 译文或失败原因。
        result: Result<Translated, InputError>,
        /// 是否回给主窗口内嵌的翻译页。
        embedded: bool,
    },
    /// 独立翻译窗口尺寸停住了（该记忆大小）。
    TranslateWindowSettled {
        /// 窗口缩放比，用来把物理外框换算成逻辑大小。
        scale: f32,
    },
    /// 主窗口位置 / 大小停住了（该记忆几何）。
    MainWindowSettled {
        /// 此刻是否最大化（最大化时只更新标记，不覆盖普通态外框）。
        maximized: bool,
    },
    /// 用系统默认程序打开一个文件（如许可证摘要）。
    OpenFile(PathBuf),
    /// 语音转文字命令（来自热键或总线）。
    Dictation(DictationCommand),
    /// 语音转文字：工作进程有新事件，或定时器到点（取事件、查超时、重试键入）。
    DictationPoll,
    /// 语音转文字：后台线程完成了前台焦点探测。
    DictationProbed {
        /// 探测所属轮次（过期结果会被丢弃）。
        round: u64,
        /// 能否键入的判定。
        verdict: Verdict,
    },
    /// 语音转文字：后台翻译线程完成了一句定稿的翻译。
    DictationTranslated {
        /// 所属轮次（过期结果会被丢弃）。
        round: u64,
        /// 句序号。
        seq: usize,
        /// 翻译结果。
        outcome: TranslationOutcome,
    },
    /// 输入框翻译浮窗请求翻译（在后台线程执行）。
    TranslateInputRequested {
        /// 请求序号（回传结果时带回）。
        serial: u64,
        /// 用户输入的原文。
        text: String,
        /// 下拉选中的包 ID（空串为自动）。
        model_id: String,
    },
    /// 输入框翻译完成（成功或失败）。
    TranslateInputFinished {
        /// 对应的请求序号。
        serial: u64,
        /// 译文或失败原因。
        result: Result<Translated, InputError>,
    },
    /// 请求长截图：进入选区，确认后开始滚动采集。
    StartScrollCapture,
    /// 长截图界面 / 采集线程有进度（周期刷新或线程唤醒）。
    ScrollTick,
    /// 选区已确认（覆盖窗关闭后投递），携带虚拟桌面物理坐标下的区域与所在显示器。
    RecordingRegionChosen {
        /// 录制区域（虚拟桌面物理坐标）。
        region: PhysicalRect,
        /// 区域所在显示器。
        monitor: MonitorInfo,
    },
    /// 录制进程有新事件（读线程唤醒）。
    RecorderPoll,
    /// 录制窗的周期刷新。
    RecordingTick,
    /// 设置页写入了某个配置项（携带写入前的值，热键重注册失败时用它回滚）。
    ConfigChanged {
        /// 变更的配置键。
        key: String,
        /// 变更前的值。
        previous: Value,
    },
    /// 把覆盖窗选区（含标注合成结果）贴到屏幕原位。
    PinCreate {
        /// 贴图窗口外框（虚拟桌面物理坐标）。
        rect: PhysicalRect,
        /// 图像宽。
        width: u32,
        /// 图像高。
        height: u32,
        /// 不透明 RGBA 像素。
        rgba: Vec<u8>,
    },
    /// 把剪贴板里的图像贴到屏幕上。
    PinFromClipboard,
    /// 打开（或激活）主窗口。
    OpenMainWindow,
    /// 主窗口侧栏折叠状态变化（需要落盘）。
    MainWindowSidebarCollapsed(bool),
    /// 打开（或激活）截图历史窗口。
    OpenHistory,
    /// 截图历史有新记录写入（刷新已打开的历史窗口）。
    HistoryChanged,
    /// 历史窗口的一张缩略图就绪（`None` 表示解码失败）。
    HistoryThumb {
        /// 记录 ID。
        id: String,
        /// 缩略图。
        thumb: Option<Thumbnail>,
    },
    /// 历史窗口的异步动作（复制 / 贴图）完成。
    HistoryActionDone {
        /// 动作。
        action: HistoryAction,
        /// 失败原因；成功为 `None`。
        error: Option<String>,
    },
    /// 历史窗口请求再次贴图（像素已解码）。
    HistoryPin {
        /// 图像宽。
        width: u32,
        /// 图像高。
        height: u32,
        /// 不透明 RGBA 像素。
        rgba: Vec<u8>,
    },
    /// 启动时恢复已持久化的贴图窗口。
    RestorePins,
    /// 贴图窗口的控制事件（点击穿透退出按钮的开关）。
    PinControl(crate::pinned_shared::PinControlEvent),
    /// 打开（或激活）贴图管理窗口。
    OpenPinManage,
    /// 全局鼠标手势的拖动事件（来自钩子线程）。
    MouseGesture(snow_platform::global_mouse::DragEvent),
    /// 打开文字识别结果窗（每次新开一个，窗口自带数据）。
    OpenRecognitionWindow(Box<crate::recognition_view::RecognitionData>),
    /// 前台应用选中的文字已读取（`None` 表示没有选中或读取失败）。
    SelectedTextReady(Option<String>),
    /// 把一张已解码的图片贴到屏幕（贴选中的文件）。
    PinImage {
        /// 图像宽。
        width: u32,
        /// 图像高。
        height: u32,
        /// RGBA 像素。
        rgba: Vec<u8>,
    },
    /// 没有找到可贴的选中图片文件。
    PinFilesEmpty,
    /// 贴图文字识别完成（成功或失败）。
    PinOcrFinished {
        /// 贴图 ID。
        id: String,
        /// 识别结果或失败原因。
        result: Result<OcrResult, String>,
    },
    /// 管理窗口的一张缩略图就绪（`None` 表示解码失败）。
    PinManageThumb {
        /// 贴图 ID。
        id: String,
        /// 缩略图。
        thumb: Option<Thumbnail>,
    },
    /// 管理窗口：显示一张贴图（必要时先切换分组）。
    PinManageShow {
        /// 贴图 ID。
        id: String,
    },
    /// 管理窗口：删除一张贴图。
    PinManageDelete {
        /// 贴图 ID。
        id: String,
    },
    /// 管理窗口：删除全部贴图。
    PinManageDeleteAll,
    /// 管理窗口：按名称新建分组。
    PinGroupCreateNamed {
        /// 分组名。
        name: String,
    },
    /// 管理窗口：删除指定分组及其中的贴图。
    PinGroupDelete {
        /// 分组 ID。
        id: String,
    },
    /// 切换到某个贴图分组。
    PinGroupSwitch {
        /// 分组 ID。
        id: String,
    },
    /// 新建一个贴图分组（自动命名）。
    PinGroupNew,
    /// 删除全部空的贴图分组。
    PinGroupDeleteEmpty,
    /// 把一张贴图移到另一个分组。
    PinMoveToGroup {
        /// 贴图 ID。
        id: String,
        /// 目标分组 ID。
        group: String,
    },
    /// 鼠标移到「隐藏到顶部」的把手上。
    PinHideReveal {
        /// 贴图 ID。
        id: String,
    },
    /// 「隐藏到顶部」的把手被点击。
    PinExitHideToTop {
        /// 贴图 ID。
        id: String,
    },
    /// 点击穿透退出按钮被点击。
    PinExitClickThrough {
        /// 贴图 ID。
        id: String,
    },
    /// 某张贴图窗口已关闭（回收其句柄）。
    PinClosed {
        /// 贴图 ID。
        id: String,
    },
    /// 长截图选区已确认（覆盖窗关闭后投递），携带虚拟桌面物理坐标下的区域与所在显示器。
    ScrollRegionChosen {
        /// 滚动截取区域（虚拟桌面物理坐标）。
        region: PhysicalRect,
        /// 区域所在显示器。
        monitor: MonitorInfo,
    },
    /// 覆盖窗请求文字识别（在后台线程执行）。
    OcrRequested {
        /// 请求序号（回传结果时带回）。
        serial: u64,
        /// 图像宽。
        width: u32,
        /// 图像高。
        height: u32,
        /// RGBA 像素。
        rgba: Vec<u8>,
    },
    /// 覆盖窗请求表格识别（OCR + 结构推理，在后台线程执行）。
    TableRequested {
        /// 请求序号（回传结果时带回，走 `OcrFinished`）。
        serial: u64,
        /// 图像宽。
        width: u32,
        /// 图像高。
        height: u32,
        /// RGBA 像素。
        rgba: Vec<u8>,
    },
    /// 覆盖窗请求下载表格识别组件（进度与结果复用 OCR 下载事件）。
    TableDownloadRequested,
    /// 文字识别完成（成功或失败）。
    OcrFinished {
        /// 对应的请求序号。
        serial: u64,
        /// 识别结果或失败原因。
        result: Result<OcrResult, OcrError>,
    },
    /// 覆盖窗请求文字翻译（识别 + 翻译，在后台线程执行）。
    TranslateRequested {
        /// 请求序号（回传结果时带回）。
        serial: u64,
        /// 图像宽。
        width: u32,
        /// 图像高。
        height: u32,
        /// RGBA 像素。
        rgba: Vec<u8>,
    },
    /// 翻译流程进入新阶段。
    TranslateProgress {
        /// 对应的请求序号。
        serial: u64,
        /// 当前阶段。
        stage: TranslateStage,
    },
    /// 文字翻译完成（成功或失败）。
    TranslateFinished {
        /// 对应的请求序号。
        serial: u64,
        /// 翻译产出或失败原因。
        result: Result<TranslateOutcome, TranslateFlowError>,
    },
    /// 覆盖窗请求下载翻译用的 onnxruntime 运行时。
    TranslateDownloadRequested,
    /// 翻译运行时下载进度。
    TranslateDownloadProgress(String),
    /// 翻译运行时下载结束。
    TranslateDownloadFinished(Result<(), String>),
    /// 覆盖窗请求下载 OCR 组件。
    OcrDownloadRequested,
    /// OCR 组件下载进度。
    OcrDownloadProgress(String),
    /// OCR 组件下载结束。
    OcrDownloadFinished(Result<(), String>),
    /// 设置页请求下载语音模型（携带取消标记）。
    SttDownloadRequested {
        /// 模型 ID。
        model_id: String,
        /// 取消标记。
        cancel: CancelFlag,
    },
    /// 语音模型下载进度。
    SttDownloadProgress(SttProgress),
    /// 语音模型下载结束。
    SttDownloadFinished {
        /// 模型 ID。
        model_id: String,
        /// 结果（结构化错误，界面边界再翻译）。
        result: Result<(), ocr_download::FetchError>,
    },
    /// 设置页请求导出 / 导入设置。
    ConfigTransferRequested(crate::config_transfer::TransferAction),
    /// 设置页“更新”分组的动作（检查 / 下载 / 打开目录）。
    UpdateActionRequested(crate::net_settings::UpdateAction),
    /// 更新包下载结束：被下载的更新与结果。
    UpdateDownloadFinished(
        crate::net_settings::UpdateInfo,
        crate::net_settings::UpdateDownloadOutcome,
    ),
    /// 检查更新结束（未本地化的结果）。
    UpdateCheckFinished(crate::net_settings::UpdateCheckOutcome),
    /// 快捷动作（来自全局热键 / 总线）。
    QuickAction(QuickAction),
    /// 延迟截图的倒计时到点（携带倒计时序号，过期序号会被丢弃）。
    DelayElapsed {
        /// 倒计时序号。
        serial: u64,
    },
    /// 直接截图完成（成功的输出结果或失败原因）。
    DirectCaptureDone(Result<DirectResult, String>),
    /// 退出应用。
    Quit,
}

/// 命令来源转来源标签。
///
/// ```ignore
/// assert_eq!(origin_of(CommandSource::Hotkey), ORIGIN_HOTKEY);
/// ```
pub fn origin_of(source: CommandSource) -> &'static str {
    match source {
        CommandSource::Hotkey => ORIGIN_HOTKEY,
        CommandSource::Tray => ORIGIN_TRAY,
        _ => ORIGIN_OTHER,
    }
}

/// 把 IPC 命令映射为主线程事件；无对应动作（如自定义参数）返回 `None`。
///
/// # 参数
/// - `cmd`：从属实例发来的命令。
///
/// ```ignore
/// assert_eq!(map_ipc_command(&IpcCommand::Quit), Some(UiEvent::Quit));
/// ```
pub fn map_ipc_command(cmd: &IpcCommand) -> Option<UiEvent> {
    match cmd {
        IpcCommand::TriggerScreenshot => Some(UiEvent::Capture { origin: ORIGIN_IPC }),
        IpcCommand::TriggerRecording => Some(UiEvent::StartRecording),
        IpcCommand::ScrollCapture => Some(UiEvent::StartScrollCapture),
        IpcCommand::PinClipboard => Some(UiEvent::PinFromClipboard),
        IpcCommand::OpenSettings => Some(UiEvent::OpenSettings),
        IpcCommand::ShowMainWindow => Some(UiEvent::OpenMainWindow),
        IpcCommand::Quit => Some(UiEvent::Quit),
        IpcCommand::Custom(_) => None,
    }
}

/// 把托盘信号映射为主线程事件；未知信号返回 `None`。
///
/// ```ignore
/// assert_eq!(map_tray_signal("quit"), Some(UiEvent::Quit));
/// ```
pub fn map_tray_signal(signal: &str) -> Option<UiEvent> {
    match signal {
        TRAY_SIGNAL_PIN_CLIPBOARD => Some(UiEvent::PinFromClipboard),
        TRAY_SIGNAL_HISTORY => Some(UiEvent::OpenHistory),
        TRAY_SIGNAL_MAIN_WINDOW => Some(UiEvent::OpenMainWindow),
        TRAY_SIGNAL_SETTINGS => Some(UiEvent::OpenSettings),
        TRAY_SIGNAL_QUIT => Some(UiEvent::Quit),
        TRAY_SIGNAL_RESTART => Some(UiEvent::Restart),
        TRAY_SIGNAL_PIN_MANAGE => Some(UiEvent::OpenPinManage),
        TRAY_SIGNAL_GROUP_NEW => Some(UiEvent::PinGroupNew),
        TRAY_SIGNAL_GROUP_DELETE_EMPTY => Some(UiEvent::PinGroupDeleteEmpty),
        other => other
            .strip_prefix(TRAY_SIGNAL_GROUP_PREFIX)
            .map(|id| UiEvent::PinGroupSwitch { id: id.to_string() }),
    }
}

/// 托盘菜单图标边长（逻辑像素）。
const TRAY_MENU_ICON_SIZE: u32 = 16;
/// 托盘信号：重启应用。
pub const TRAY_SIGNAL_RESTART: &str = "restart";
/// 托盘延迟截图文案的秒数变量位置（`$arg1`）。
const TRAY_DELAY_ARG_INDEX: u8 = 1;
/// 重启时等待旧进程退出的 ping 次数（约 1 秒/次，借 ping 做无控制台延时）。
const RESTART_WAIT_PINGS: u32 = 3;

/// 渲染一个托盘菜单图标（Ant Design 描边图标）；名称不存在或渲染失败时返回 `None`，菜单项退化为无图标。
///
/// # 参数
/// - `renderer`：图标光栅化器（带缓存）。
/// - `name`：图标 kebab-case 名称，如 `camera`。
fn tray_menu_icon(renderer: &snow_ui::icons::IconRenderer, name: &str) -> Option<TrayIconImage> {
    let icon = snow_ui::icons::IconRef::new(snow_ui::icons::IconTheme::Outlined, name);
    if !icon.exists() {
        return None;
    }
    let bitmap = renderer.render(
        &icon,
        &snow_ui::icons::IconRequest::square(TRAY_MENU_ICON_SIZE, 1.0),
    )?;
    TrayIconImage::new(bitmap.to_straight_rgba(), bitmap.width, bitmap.height).ok()
}

/// 构造托盘描述：按上游分组（截图 / 贴图 / 录屏 / 其他 / 系统，组间有分隔线），每项带图标。
///
/// # 参数
/// - `locale`：界面语料语言代码（如 `zh-CN`）。
/// - `doc`：配置文档（读取延迟截图秒数）。
/// - `hotkeys_paused`：全局热键当前是否被暂停（决定“禁用全局热键”的勾选状态）。
///
/// # 返回
/// 托盘描述；图标数据非法返回错误文本（占位图标恒合法）。
///
/// ```ignore
/// let doc = ConfigDocument::from_bytes(None);
/// let spec = build_tray_spec("zh-CN", &doc, false).unwrap();
/// assert_eq!(spec.menu.len(), 25);
/// ```
pub fn build_tray_spec(
    locale: &str,
    doc: &ConfigDocument,
    hotkeys_paused: bool,
) -> Result<TraySpec, String> {
    build_tray_spec_with_groups(locale, doc, hotkeys_paused, &[], "")
}

/// 同 [`build_tray_spec`]，并在贴图组后追加「贴图分组」块（切换、新建、删除空分组）。
///
/// # 参数
/// - `groups`：`(分组 ID, 显示名)` 列表；为空则不显示分组块。
/// - `active`：当前激活的分组 ID（打勾）。
pub fn build_tray_spec_with_groups(
    locale: &str,
    doc: &ConfigDocument,
    hotkeys_paused: bool,
    groups: &[(String, String)],
    active: &str,
) -> Result<TraySpec, String> {
    let i18n = crate::ocr_backend::i18n_for(locale);
    let icon = crate::tray_config::tray_icon_image(doc);
    let enabled = crate::tray_config::menu_options(doc);
    let renderer = snow_ui::icons::IconRenderer::new();
    let entry = |label: String, icon_name: &str, checked: Option<bool>, action: TrayAction| {
        TrayMenuEntry::Item {
            label,
            enabled: true,
            checked,
            icon: tray_menu_icon(&renderer, icon_name),
            action,
        }
    };
    let item = |key: &str, icon_name: &str, action: TrayAction| {
        entry(i18n.tr(key), icon_name, None, action)
    };
    let quick = |action: QuickAction| TrayAction::Command(AppCommand::QuickAction(action));
    let delay_label = i18n.tr_with(
        "tray-capture-delay",
        &Args::new().arg(TRAY_DELAY_ARG_INDEX, delay_seconds(doc)),
    );
    let sep = || (None, TrayMenuEntry::Separator);

    // 每项带 `tray/menu_options` 里的标识，未启用的项在筛选时去掉
    let mut menu: Vec<(Option<&'static str>, TrayMenuEntry)> = vec![
        // 截图组
        (
            Some("quick.screenshot"),
            item(
                "tray-capture",
                "camera",
                TrayAction::Command(AppCommand::Capture(CaptureRequest::default())),
            ),
        ),
        (
            Some("quick.screenshot-delay"),
            entry(
                delay_label,
                "clock-circle",
                None,
                quick(QuickAction::ScreenshotDelay),
            ),
        ),
        (
            Some("quick.screenshot-fixed"),
            item(
                "tray-capture-pin",
                "pushpin",
                quick(QuickAction::ScreenshotFixed),
            ),
        ),
        (
            Some("quick.screenshot-ocr"),
            item(
                "tray-capture-ocr",
                "file-search",
                quick(QuickAction::ScreenshotOcr),
            ),
        ),
        (
            Some("quick.screenshot-translation"),
            item(
                "tray-capture-translate",
                "translation",
                quick(QuickAction::ScreenshotTranslation),
            ),
        ),
        (
            Some("quick.screenshot-copy"),
            item(
                "tray-capture-copy",
                "copy",
                quick(QuickAction::ScreenshotCopy),
            ),
        ),
        (
            Some("quick.screenshot-full-screen"),
            item(
                "tray-capture-full-screen",
                "desktop",
                quick(QuickAction::ScreenshotFullScreen),
            ),
        ),
        (
            Some("quick.screenshot-focused-window"),
            item(
                "tray-capture-focused-window",
                "scan",
                quick(QuickAction::ScreenshotFocusedWindow),
            ),
        ),
        sep(),
        // 贴图组
        (
            Some("quick.pin-clipboard-content"),
            item(
                "tray-pin-clipboard",
                "snippets",
                TrayAction::Signal(TRAY_SIGNAL_PIN_CLIPBOARD.into()),
            ),
        ),
        (
            Some("quick.pin-selected-files"),
            item(
                "tray-pin-selected-files",
                "paper-clip",
                quick(QuickAction::PinSelectedFiles),
            ),
        ),
        (
            Some("quick.restore-last-closed-windows"),
            item(
                "tray-restore-closed",
                "undo",
                quick(QuickAction::RestoreLastClosedWindows),
            ),
        ),
        sep(),
        // 录屏组
        (
            Some("quick.screen-record"),
            item(
                "tray-record",
                "video-camera",
                TrayAction::Command(AppCommand::StartRecording(RecordingRequest::default())),
            ),
        ),
        (
            Some("quick.screen-record-copy"),
            item(
                "tray-record-copy",
                "export",
                quick(QuickAction::ScreenRecordCopy),
            ),
        ),
        (
            Some("quick.open-screen-recording-folder"),
            item(
                "tray-open-recordings",
                "folder-open",
                quick(QuickAction::OpenScreenRecordingFolder),
            ),
        ),
        sep(),
        // 其他组
        (
            Some("quick.open-capture-history"),
            item(
                "tray-history",
                "history",
                TrayAction::Signal(TRAY_SIGNAL_HISTORY.into()),
            ),
        ),
        (
            Some("quick.translate-selected-text"),
            item(
                "tray-translate-selected",
                "select",
                quick(QuickAction::TranslateSelectedText),
            ),
        ),
        (
            Some("quick.toggle-global-hotkeys"),
            entry(
                i18n.tr("tray-toggle-hotkeys"),
                "stop",
                Some(hotkeys_paused),
                quick(QuickAction::ToggleGlobalHotkeys),
            ),
        ),
        (
            Some("quick.toggle-disable-on-focused-fullscreen-window"),
            entry(
                i18n.tr("tray-toggle-fullscreen"),
                "fullscreen",
                Some(crate::fullscreen_gate::configured(doc)),
                quick(QuickAction::ToggleDisableOnFocusedFullscreen),
            ),
        ),
        sep(),
        // 系统组
        (
            Some("tray.show-main-window"),
            item(
                "tray-show-main",
                "home",
                TrayAction::Signal(TRAY_SIGNAL_MAIN_WINDOW.into()),
            ),
        ),
        (
            Some("tray.restart-app"),
            item(
                "tray-restart",
                "reload",
                TrayAction::Signal(TRAY_SIGNAL_RESTART.into()),
            ),
        ),
        (
            Some("tray.exit"),
            item(
                "tray-quit",
                "poweroff",
                TrayAction::Signal(TRAY_SIGNAL_QUIT.into()),
            ),
        ),
    ];
    // 贴图分组块放在「贴图组」之后（第二条分隔线位置）；整块由 `tray.window-grouping` 控制
    if !groups.is_empty() {
        let at = menu
            .iter()
            .enumerate()
            .filter(|(_, (_, e))| matches!(e, TrayMenuEntry::Separator))
            .nth(1)
            .map_or(menu.len(), |(i, _)| i + 1);
        let mut block: Vec<(Option<&'static str>, TrayMenuEntry)> = groups
            .iter()
            .map(|(id, name)| {
                (
                    Some("tray.window-grouping"),
                    entry(
                        name.clone(),
                        "folder",
                        Some(id == active),
                        TrayAction::Signal(format!("{TRAY_SIGNAL_GROUP_PREFIX}{id}")),
                    ),
                )
            })
            .collect();
        block.push((
            Some("tray.window-grouping"),
            item(
                "tray-group-new",
                "plus",
                TrayAction::Signal(TRAY_SIGNAL_GROUP_NEW.into()),
            ),
        ));
        block.push((
            Some("tray.window-grouping"),
            item(
                "tray-group-delete-empty",
                "delete",
                TrayAction::Signal(TRAY_SIGNAL_GROUP_DELETE_EMPTY.into()),
            ),
        ));
        block.push((
            Some("tray.window-grouping"),
            item(
                "tray-pin-management",
                "appstore",
                TrayAction::Signal(TRAY_SIGNAL_PIN_MANAGE.into()),
            ),
        ));
        block.push(sep());
        menu.splice(at..at, block);
    }
    let menu = crate::tray_config::filter_menu(menu, &enabled);

    let click_signal = |action: crate::tray_config::TrayClick| {
        use crate::tray_config::TrayClick;
        match action {
            TrayClick::Screenshot => {
                TrayAction::Command(AppCommand::Capture(CaptureRequest::default()))
            }
            TrayClick::ShowMainWindow => TrayAction::Signal(TRAY_SIGNAL_MAIN_WINDOW.into()),
            TrayClick::OpenFunctionSettings => TrayAction::Signal(TRAY_SIGNAL_SETTINGS.into()),
            TrayClick::ScreenshotCopy => quick(QuickAction::ScreenshotCopy),
            TrayClick::ScreenshotFixed => quick(QuickAction::ScreenshotFixed),
        }
    };
    use crate::tray_config::{KEY_LEFT_CLICK, KEY_MIDDLE_CLICK, TrayClick, click_action};
    Ok(TraySpec {
        tooltip: TRAY_TOOLTIP.to_string(),
        icon,
        menu,
        on_left_click: Some(click_signal(click_action(
            doc,
            KEY_LEFT_CLICK,
            TrayClick::Screenshot,
        ))),
        on_double_click: None,
        on_middle_click: Some(click_signal(click_action(
            doc,
            KEY_MIDDLE_CLICK,
            TrayClick::ScreenshotFixed,
        ))),
    })
}

/// 拉起一个延迟启动的新实例（等旧进程退出、释放单实例互斥后再启动）。
///
/// # 返回
/// 成功拉起辅助进程为 `Ok`；取不到自身路径或拉起失败返回错误。
fn spawn_restart_helper() -> std::io::Result<()> {
    use std::os::windows::process::CommandExt;
    /// 不创建控制台窗口。
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    let exe = std::env::current_exe()?;
    let line = format!(
        "/C ping -n {RESTART_WAIT_PINGS} 127.0.0.1 >NUL & start \"\" \"{}\"",
        exe.display()
    );
    std::process::Command::new("cmd")
        .raw_arg(line)
        .creation_flags(CREATE_NO_WINDOW)
        .spawn()
        .map(|_| ())
}

/// 由配置文档解析界面偏好（深浅色、语言、主色）。
///
/// # 参数
/// - `document`：配置文档。
pub(crate) fn ui_prefs_from_document(document: &ConfigDocument) -> UiPrefs {
    UiPrefs::resolve(
        document.value(THEME_MODE_KEY).as_str().unwrap_or_default(),
        document.value(LANGUAGE_KEY).as_str().unwrap_or_default(),
        document.value(THEME_COLOR_KEY).as_str().unwrap_or_default(),
        &SystemPrefs::query(),
    )
}

/// 按界面深浅色设置弹出菜单（托盘右键菜单）主题。
///
/// # 参数
/// - `dark`：是否深色。
fn apply_popup_menu_theme(dark: bool) {
    if let Err(e) = snow_ui::ui::set_popup_menu_dark(Some(dark)) {
        tracing::warn!(error = %e, "设置托盘菜单主题失败");
    }
}

/// 把当前界面深浅色应用到设置 / 历史窗口标题栏与托盘菜单。
///
/// # 参数
/// - `state`：运行时状态。
fn apply_chrome_theme(state: &AppState) {
    let dark = ui_prefs_from_document(state.config.borrow().document()).dark;
    apply_popup_menu_theme(dark);
    let windows = state
        .settings
        .as_ref()
        .into_iter()
        .chain(state.main_window.as_ref().map(|(window, _)| window))
        .chain(state.history_window.as_ref().map(|(window, _)| window));
    for window in windows {
        if let Err(e) = window.set_dark_title(dark) {
            tracing::warn!(error = %e, "设置标题栏主题失败");
        }
    }
}

/// 在命令总线上注册截图 / 录制命令：handler 只把事件投递进收件箱（运行在派发线程）。
///
/// # 参数
/// - `bus`：命令总线。
/// - `inbox`：主线程收件箱。
///
/// ```ignore
/// register_bus_handlers(&bus, &inbox);
/// ```
pub fn register_bus_handlers(bus: &CommandBus, inbox: &MainThreadInbox<UiEvent>) {
    let capture_inbox = inbox.clone();
    bus.register(
        CommandKind::Capture,
        std::sync::Arc::new(move |ctx, _cmd| {
            capture_inbox.push(UiEvent::Capture {
                origin: origin_of(ctx.source),
            });
            Ok(CommandOutcome::Done)
        }),
    );
    let record_inbox = inbox.clone();
    bus.register(
        CommandKind::StartRecording,
        std::sync::Arc::new(move |_ctx, _cmd| {
            record_inbox.push(UiEvent::StartRecording);
            Ok(CommandOutcome::Done)
        }),
    );
    let export_inbox = inbox.clone();
    bus.register(
        CommandKind::Export,
        std::sync::Arc::new(move |_ctx, cmd| match cmd {
            AppCommand::Export(target) => {
                export_inbox.push(UiEvent::Export(target.clone()));
                Ok(CommandOutcome::Done)
            }
            other => Err(CommandError::Rejected(format!(
                "导出处理器收到非导出命令: {other:?}"
            ))),
        }),
    );
    let direct_inbox = inbox.clone();
    bus.register(
        CommandKind::DirectCapture,
        std::sync::Arc::new(move |_ctx, cmd| match cmd {
            AppCommand::DirectCapture(request) => {
                direct_inbox.push(UiEvent::DirectCapture(request.clone()));
                Ok(CommandOutcome::Done)
            }
            other => Err(CommandError::Rejected(format!(
                "直接截图处理器收到非直接截图命令: {other:?}"
            ))),
        }),
    );
    let translate_input_inbox = inbox.clone();
    bus.register(
        CommandKind::OpenTranslateInput,
        std::sync::Arc::new(move |_ctx, _cmd| {
            translate_input_inbox.push(UiEvent::OpenTranslateInput);
            Ok(CommandOutcome::Done)
        }),
    );
    for (kind, command) in [
        (CommandKind::ToggleDictation, DictationCommand::Toggle),
        (CommandKind::StartDictation, DictationCommand::Start),
        (CommandKind::StopDictation, DictationCommand::Stop),
    ] {
        let dictation_inbox = inbox.clone();
        bus.register(
            kind,
            std::sync::Arc::new(move |_ctx, _cmd| {
                dictation_inbox.push(UiEvent::Dictation(command));
                Ok(CommandOutcome::Done)
            }),
        );
    }
    // 全局热键 `pin_clipboard_content` 绑定的是 `PinSelection` 命令：没有进行中的截图会话，
    // 因此这里把它解释为“把剪贴板内容贴到屏幕”（避免为此新增命令变体波及 MCP 映射）
    let pin_inbox = inbox.clone();
    bus.register(
        CommandKind::PinSelection,
        std::sync::Arc::new(move |_ctx, _cmd| {
            pin_inbox.push(UiEvent::PinFromClipboard);
            Ok(CommandOutcome::Done)
        }),
    );
    // 其余全局热键动作统一走 QuickAction，由主线程按执行方案解释
    let quick_inbox = inbox.clone();
    bus.register(
        CommandKind::QuickAction,
        std::sync::Arc::new(move |_ctx, cmd| {
            if let AppCommand::QuickAction(action) = cmd {
                quick_inbox.push(UiEvent::QuickAction(*action));
            }
            Ok(CommandOutcome::Done)
        }),
    );
}

/// 从配置值提取热键字符串列表。元素可为字符串，或规范化后的 `{"portable": "F1"}` 对象；
/// 其它形态与空串被忽略。
///
/// ```ignore
/// let v = serde_json::json!(["F1", {"portable": "Ctrl+Alt+A"}]);
/// assert_eq!(shortcut_strings(&v), vec!["F1", "Ctrl+Alt+A"]);
/// ```
pub fn shortcut_strings(value: &Value) -> Vec<String> {
    value
        .as_array()
        .map(|items| {
            items
                .iter()
                .filter_map(|v| {
                    v.as_str()
                        .or_else(|| v.get(PORTABLE_FIELD).and_then(Value::as_str))
                })
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

/// 一次热键注册的失败记录。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HotkeyFailure {
    /// 失败所属的配置键。
    pub config_key: &'static str,
    /// 热键文本。
    pub shortcut: String,
    /// 失败原因。
    pub reason: String,
}

/// 一批热键注册的结果。
#[derive(Debug, Default)]
pub struct HotkeyRegistration {
    /// 注册成功的句柄（用于之后注销）。
    pub handles: Vec<HotkeyHandle>,
    /// 失败列表。
    pub failures: Vec<HotkeyFailure>,
}

impl HotkeyRegistration {
    /// 是否有属于指定配置键的失败。
    ///
    /// # 参数
    /// - `key`：配置键。
    pub fn failed_for(&self, key: &str) -> bool {
        self.failures.iter().any(|f| f.config_key == key)
    }

    /// 汇总某个配置键的失败原因（用于界面提示）。
    ///
    /// # 参数
    /// - `key`：配置键。
    pub fn describe_for(&self, key: &str) -> String {
        self.failures
            .iter()
            .filter(|f| f.config_key == key)
            .map(|f| format!("{}: {}", f.shortcut, f.reason))
            .collect::<Vec<_>>()
            .join("; ")
    }

    /// 合并另一批结果。
    fn merge(&mut self, other: HotkeyRegistration) {
        self.handles.extend(other.handles);
        self.failures.extend(other.failures);
    }
}

/// 按配置键注册一类全局热键；单个失败只记日志并收集，不影响其余。
///
/// # 参数
/// - `service`：热键服务。
/// - `document`：配置文档。
/// - `key`：热键配置键。
/// - `label`：日志里的功能名。
/// - `command`：热键按下时触发的命令。
///
/// # 返回
/// 成功句柄与失败列表。
fn register_hotkeys(
    service: &HotkeyService,
    document: &ConfigDocument,
    key: &'static str,
    label: &str,
    command: &AppCommand,
) -> HotkeyRegistration {
    register_hotkeys_with_release(service, document, key, label, command, None)
}

/// 同 `register_hotkeys`，并可指定热键松开时触发的命令（按住说话用）。
///
/// # 参数
/// - `service`：热键服务。
/// - `document`：配置文档。
/// - `key`：热键配置键。
/// - `label`：日志里的功能名。
/// - `command`：热键按下时触发的命令。
/// - `on_release`：热键松开时触发的命令，`None` 表示忽略松开。
///
/// # 返回
/// 成功句柄与失败列表。
fn register_hotkeys_with_release(
    service: &HotkeyService,
    document: &ConfigDocument,
    key: &'static str,
    label: &str,
    command: &AppCommand,
    on_release: Option<&AppCommand>,
) -> HotkeyRegistration {
    let mut result = HotkeyRegistration::default();
    for text in shortcut_strings(&document.value(key)) {
        let hotkey = match Hotkey::parse(&portable_to_hotkey_text(&text)) {
            Ok(h) => h,
            Err(e) => {
                tracing::warn!(shortcut = %text, error = %e, feature = label, "热键格式无效，已跳过");
                result.failures.push(HotkeyFailure {
                    config_key: key,
                    shortcut: text,
                    reason: e.to_string(),
                });
                continue;
            }
        };
        let binding = HotkeyBinding {
            hotkey,
            command: command.clone(),
            on_release: on_release.cloned(),
        };
        match service.register(binding) {
            Ok(handle) => {
                tracing::info!(shortcut = %text, feature = label, "全局热键已注册");
                result.handles.push(handle);
            }
            Err(e) => {
                tracing::warn!(shortcut = %text, error = %e, feature = label, "热键注册失败（可能被其它程序占用）");
                result.failures.push(HotkeyFailure {
                    config_key: key,
                    shortcut: text,
                    reason: e.to_string(),
                });
            }
        }
    }
    result
}

/// 按配置注册截图全局热键；单个失败只记日志，不影响其余。
///
/// # 参数
/// - `service`：热键服务。
/// - `document`：配置文档。
///
/// # 返回
/// 成功句柄与失败列表。
///
/// ```ignore
/// let result = register_capture_hotkeys(&service, &document);
/// ```
pub fn register_capture_hotkeys(
    service: &HotkeyService,
    document: &ConfigDocument,
) -> HotkeyRegistration {
    register_hotkeys(
        service,
        document,
        SCREENSHOT_HOTKEY_CONFIG_KEY,
        "screenshot",
        &AppCommand::Capture(CaptureRequest::default()),
    )
}

/// 按配置注册录屏全局热键（`global_shortcuts/screen_record`，默认未绑定）。
///
/// # 参数
/// - `service`：热键服务。
/// - `document`：配置文档。
///
/// # 返回
/// 成功句柄与失败列表。
///
/// ```ignore
/// let result = register_recording_hotkeys(&service, &document);
/// ```
pub fn register_recording_hotkeys(
    service: &HotkeyService,
    document: &ConfigDocument,
) -> HotkeyRegistration {
    register_hotkeys(
        service,
        document,
        RECORDING_HOTKEY_CONFIG_KEY,
        "recording",
        &AppCommand::StartRecording(RecordingRequest::default()),
    )
}

/// 按配置注册“贴图剪贴板内容”全局热键（`global_shortcuts/pin_clipboard_content`，默认未绑定）。
///
/// # 参数
/// - `service`：热键服务。
/// - `document`：配置文档。
///
/// # 返回
/// 成功句柄与失败列表。
pub fn register_pin_clipboard_hotkeys(
    service: &HotkeyService,
    document: &ConfigDocument,
) -> HotkeyRegistration {
    register_hotkeys(
        service,
        document,
        PIN_CLIPBOARD_HOTKEY_CONFIG_KEY,
        "pin_clipboard",
        &AppCommand::PinSelection,
    )
}

/// 按配置注册“输入框翻译浮窗”全局热键（`global_shortcuts/translate_input`，默认未绑定，未绑定时不注册）。
///
/// # 参数
/// - `service`：热键服务。
/// - `document`：配置文档。
///
/// # 返回
/// 成功句柄与失败列表。
pub fn register_translate_input_hotkeys(
    service: &HotkeyService,
    document: &ConfigDocument,
) -> HotkeyRegistration {
    register_hotkeys(
        service,
        document,
        TRANSLATE_INPUT_HOTKEY_CONFIG_KEY,
        "translate_input",
        &AppCommand::OpenTranslateInput,
    )
}

/// 按配置注册语音转文字的两个全局热键：切换式（按一下开始、再按一下结束）与按住说话（按下开始、松开结束）。
///
/// 触发模式（`dictation/trigger_mode`）决定哪个生效；未绑定的热键不注册。
///
/// # 参数
/// - `service`：热键服务。
/// - `document`：配置文档。
///
/// # 返回
/// 成功句柄与失败列表。
pub fn register_dictation_hotkeys(
    service: &HotkeyService,
    document: &ConfigDocument,
) -> HotkeyRegistration {
    let trigger = DictationConfig::from_document(document).trigger;
    let mut result = HotkeyRegistration::default();
    if trigger.toggle_enabled() {
        result.merge(register_hotkeys(
            service,
            document,
            DICTATION_TOGGLE_HOTKEY_CONFIG_KEY,
            "dictation_toggle",
            &AppCommand::ToggleDictation,
        ));
    }
    if trigger.hold_enabled() {
        result.merge(register_hotkeys_with_release(
            service,
            document,
            DICTATION_HOLD_HOTKEY_CONFIG_KEY,
            "dictation_hold",
            &AppCommand::StartDictation,
            Some(&AppCommand::StopDictation),
        ));
    }
    result
}

/// 按配置注册全部“快捷动作”热键（直接截图、延迟截图、打开设置、暂停热键等）；未绑定的不注册。
///
/// # 参数
/// - `service`：热键服务。
/// - `document`：配置文档。
/// - `paused`：热键是否处于暂停状态；暂停时只注册“暂停 / 恢复”开关本身。
///
/// # 返回
/// 成功句柄与失败列表。
pub fn register_quick_action_hotkeys(
    service: &HotkeyService,
    document: &ConfigDocument,
    paused: bool,
) -> HotkeyRegistration {
    let mut result = HotkeyRegistration::default();
    for (key, action) in QUICK_ACTION_KEYS {
        if paused && !stays_registered_when_paused(key) {
            continue;
        }
        result.merge(register_hotkeys(
            service,
            document,
            key,
            "quick_action",
            &AppCommand::QuickAction(*action),
        ));
    }
    result
}

/// 注册全部已接线的全局热键（截图 + 录屏 + 贴图剪贴板内容 + 输入框翻译 + 语音转文字 + 快捷动作）。
///
/// # 参数
/// - `service`：热键服务。
/// - `document`：配置文档。
pub fn register_all_hotkeys(
    service: &HotkeyService,
    document: &ConfigDocument,
) -> HotkeyRegistration {
    register_all_hotkeys_gated(service, document, false)
}

/// 同 [`register_all_hotkeys`]，并支持“暂停全部热键”：暂停时只保留暂停 / 恢复开关。
///
/// # 参数
/// - `service`：热键服务。
/// - `document`：配置文档。
/// - `paused`：是否处于暂停状态。
pub fn register_all_hotkeys_gated(
    service: &HotkeyService,
    document: &ConfigDocument,
    paused: bool,
) -> HotkeyRegistration {
    if paused {
        return register_quick_action_hotkeys(service, document, true);
    }
    let mut result = register_capture_hotkeys(service, document);
    result.merge(register_recording_hotkeys(service, document));
    result.merge(register_pin_clipboard_hotkeys(service, document));
    result.merge(register_translate_input_hotkeys(service, document));
    result.merge(register_dictation_hotkeys(service, document));
    result.merge(register_quick_action_hotkeys(service, document, false));
    result
}

/// 打开数据根下的配置存储（损坏留档、缺失用默认值），全程序共享同一份。
///
/// # 参数
/// - `data_root`：数据根目录。
///
/// ```ignore
/// let config = open_shared_config(std::path::Path::new("."));
/// ```
pub fn open_shared_config(data_root: &std::path::Path) -> SharedConfig {
    Rc::new(RefCell::new(ConfigStore::open(config_file_path(data_root))))
}

/// 采集完成后覆盖窗的用途。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CaptureMode {
    /// 普通截图（复制 / 保存 / 标注）。
    Screenshot,
    /// 录屏选区（确认后交给录制进程）。
    Record,
    /// 长截图选区（确认后交给滚动采集）。
    Scroll,
    /// 快捷截图：框选完成后自动执行指定动作（复制 / 贴图 / 识别 / 翻译）。
    Quick(AutoConfirm),
}

/// 一次全局鼠标手势会话：钩子线程比覆盖窗打开得快，期间的位置先记在这里，覆盖窗就绪后一次性补上。
#[derive(Debug, Clone)]
struct GestureSession {
    /// 手势编号（与钩子事件对应）。
    id: u64,
    /// 手势按下的位置（虚拟桌面物理像素）。
    start: snow_platform::global_mouse::Point,
    /// 最近一次位置。
    latest: snow_platform::global_mouse::Point,
    /// 鼠标键是否已经松开。
    finished: bool,
    /// 是否已经把按下 / 移动补发给覆盖窗。
    applied: bool,
}

/// 常驻运行时状态：随主线程事件循环存活。
pub struct AppState {
    /// 共享配置存储（设置页写入，截图 / 录制 / 热键从同一份读取）。
    config: SharedConfig,
    /// 设置窗口（若已打开）。
    settings: Option<ShellWindow>,
    /// 设置页视图（用于热键回滚后刷新界面）。
    settings_view: Option<Entity<SettingsView>>,
    /// 输入框翻译浮窗（若已打开）与其视图。
    translate_input: Option<(ShellWindow, Entity<TranslateInputView>)>,
    /// 翻译页窗口（若已打开）与其视图。
    translate_page: Option<(ShellWindow, Entity<TranslatePageView>)>,
    /// 截图历史后台写入器（启动失败时为 `None`，历史功能降级）。
    history: Option<Arc<HistoryRecorder>>,
    /// 主窗口（若已打开）与其视图；关闭即释放。
    main_window: Option<(ShellWindow, Entity<MainWindowView>)>,
    /// 截图历史窗口（若已打开）与其视图。
    history_window: Option<(ShellWindow, Entity<HistoryView>)>,
    /// 贴图管理窗口（打开时有值）。
    pin_manage_window: Option<(ShellWindow, Entity<PinManageView>)>,
    /// 语音转文字宿主（独立工作进程、键入与右下角浮窗的生命周期）。
    dictation: DictationHost,
    /// 收到的截图请求累计数。
    capture_requests: u64,
    /// 截图覆盖窗（若已打开）。
    overlay: Option<ShellWindow>,
    /// 后台采集是否正在进行（进行中忽略新的截图请求）。
    capture_in_flight: bool,
    /// 本次采集完成后覆盖窗的用途。
    capture_mode: CaptureMode,
    /// 录屏宿主（录制窗与录制进程的生命周期）。
    recording: RecordingHost,
    /// 长截图宿主（控制窗与采集线程的生命周期）。
    scroll: ScrollHost,
    /// 贴图窗口管理器（创建 / 恢复 / 淘汰 / 落盘）。
    pins: PinnedManager,
    /// OCR 服务（独立 worker 进程，空闲自动退出）。
    ocr: Arc<OcrService>,
    /// 文字翻译宿主（本地 NMT worker / OpenAI 兼容，按配置装配）。
    translator: Arc<TranslateHost>,
    /// 应用数据根目录（OCR 组件下载位置等）。
    data_root: PathBuf,
    /// 截图会话的共享视图（所有显示器上的覆盖窗共用；OCR 结果回写、导出命令用）。
    overlay_view: Option<Entity<ScreenshotOverlayView>>,
    /// 其余显示器上的覆盖窗（每块显示器一个窗口；光标所在屏的窗口在 `overlay`）。
    overlay_windows: Vec<ShellWindow>,
    /// 多屏采集收集器：各显示器并行采集，到齐后统一开窗。
    capture_collector: CaptureCollector,
    /// 为即将打开的覆盖窗预先启动的窗口悬停来源（采集开始时抓窗口快照，覆盖窗打开时交给共享视图）。
    window_hover: Option<Box<dyn WindowHover>>,
    /// 主线程收件箱（采集线程完成后经它回到主线程）。
    inbox: MainThreadInbox<UiEvent>,
    /// 托盘服务（丢弃即移除图标）。
    tray: Option<TrayService>,
    /// 热键服务（丢弃即注销热键）。
    hotkeys: Option<HotkeyService>,
    /// 当前已注册热键的句柄（重新注册时先注销）。
    hotkey_handles: Vec<HotkeyHandle>,
    /// 全局热键是否被用户暂停（只保留暂停 / 恢复开关）。
    hotkeys_paused: bool,
    /// 延迟截图倒计时状态机。
    delay: DelayGate,
    /// 直接截图是否正在进行（进行中忽略新的直接截图）。
    direct_in_flight: bool,
    /// 下一次开始的录制完成后要把文件复制到剪贴板（「录屏并复制」触发）。
    record_copy_pending: bool,
    /// 全局鼠标手势服务（有绑定时才启动；丢弃即卸载钩子）。
    mouse_service: Option<snow_platform::global_mouse::GlobalMouseService>,
    /// 进行中的鼠标手势会话。
    gesture: Option<GestureSession>,
    /// MCP 服务宿主（设置开关打开才启动）。
    mcp: crate::mcp_host::McpHost,
}

impl AppState {
    /// 创建状态。
    ///
    /// # 参数
    /// - `config`：共享配置存储。
    /// - `inbox`：主线程收件箱（与事件循环共享克隆）。
    /// - `caps`：平台能力表。
    /// - `tray` / `hotkeys`：已启动的服务（启动失败传 `None`，降级运行）。
    /// - `hotkey_handles`：启动时已注册的热键句柄。
    /// - `data_root`：数据根目录（贴图仓储所在）。
    pub fn new(
        config: SharedConfig,
        inbox: MainThreadInbox<UiEvent>,
        caps: CapabilityRegistry,
        tray: Option<TrayService>,
        hotkeys: Option<HotkeyService>,
        hotkey_handles: Vec<HotkeyHandle>,
        data_root: &std::path::Path,
    ) -> Self {
        let closed_inbox = inbox.clone();
        let history_inbox = inbox.clone();
        let history = match HistoryRecorder::start(data_root, move || {
            history_inbox.push(UiEvent::HistoryChanged);
        }) {
            Ok(recorder) => Some(Arc::new(recorder)),
            Err(e) => {
                tracing::warn!(error = %e, "启动截图历史写入线程失败，历史功能不可用");
                None
            }
        };
        let pins = PinnedManager::new(
            data_root,
            Rc::clone(&config),
            Box::new(move |id| {
                closed_inbox.push(UiEvent::PinClosed { id: id.to_string() });
            }),
        );
        let control_inbox = inbox.clone();
        pins.shared().set_control_sink(Box::new(move |event| {
            control_inbox.push(UiEvent::PinControl(event));
        }));
        let translator = Arc::new(TranslateHost::new(data_root));
        let mcp_inbox = inbox.clone();
        Self {
            pins,
            ocr: Arc::new(OcrService::new(data_root)),
            translator: Arc::clone(&translator),
            data_root: data_root.to_path_buf(),
            overlay_view: None,
            overlay_windows: Vec::new(),
            capture_collector: CaptureCollector::default(),
            window_hover: None,
            scroll: ScrollHost::new(caps.clone(), inbox.clone(), Rc::clone(&config)),
            recording: RecordingHost::new(caps, inbox.clone(), Rc::clone(&config)),
            dictation: DictationHost::new(
                Rc::clone(&config),
                inbox.clone(),
                data_root.to_path_buf(),
                translator,
            ),
            config,
            settings: None,
            settings_view: None,
            translate_input: None,
            translate_page: None,
            history,
            main_window: None,
            history_window: None,
            pin_manage_window: None,
            capture_requests: 0,
            overlay: None,
            capture_in_flight: false,
            capture_mode: CaptureMode::Screenshot,
            inbox,
            tray,
            hotkeys,
            hotkey_handles,
            hotkeys_paused: false,
            delay: DelayGate::default(),
            direct_in_flight: false,
            record_copy_pending: false,
            mouse_service: None,
            gesture: None,
            mcp: crate::mcp_host::McpHost::new(mcp_inbox),
        }
    }

    /// 按配置对齐 MCP 服务（启动时与设置变化时调用）。
    pub fn sync_mcp(&mut self) {
        self.mcp.sync(self.config.borrow().document(), &self.data_root);
    }

    /// 已收到的截图请求数。
    pub fn capture_requests(&self) -> u64 {
        self.capture_requests
    }

    /// 释放托盘与热键（移除图标、注销热键）。
    pub fn shutdown_services(&mut self) {
        self.mouse_service.take();
        self.mcp = crate::mcp_host::McpHost::new(self.inbox.clone());
        self.tray.take();
        self.hotkeys.take();
    }
}

/// 截图请求的计数入口：登记一次请求并记日志。
///
/// # 参数
/// - `state`：运行时状态。
/// - `origin`：来源标签。
///
/// # 返回
/// 本次请求的序号（从 1 开始）。
///
/// ```ignore
/// let seq = on_capture_requested(&mut state, ORIGIN_HOTKEY);
/// ```
pub fn on_capture_requested(state: &mut AppState, origin: &'static str) -> u64 {
    state.capture_requests += 1;
    tracing::info!(origin, seq = state.capture_requests, "capture requested");
    state.capture_requests
}

/// 新截图请求的处理结论。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CaptureGate {
    /// 可以开始采集。
    Proceed,
    /// 上一次采集还没结束，忽略。
    IgnoreInFlight,
    /// 覆盖窗仍开着，忽略。
    IgnoreOverlayOpen,
}

/// 判定新的截图请求能否开始（避免重复触发导致多个覆盖窗或叠加采集）。
///
/// # 参数
/// - `in_flight`：是否有采集正在进行。
/// - `overlay_open`：覆盖窗是否仍然打开。
///
/// ```ignore
/// assert_eq!(capture_gate(false, false), CaptureGate::Proceed);
/// ```
pub fn capture_gate(in_flight: bool, overlay_open: bool) -> CaptureGate {
    if in_flight {
        CaptureGate::IgnoreInFlight
    } else if overlay_open {
        CaptureGate::IgnoreOverlayOpen
    } else {
        CaptureGate::Proceed
    }
}

/// 解析 `宽x高` 形式的尺寸文本；任一边为 0 或格式不对返回 `None`。
///
/// # 参数
/// - `text`：如 `3840x2160`。
///
/// ```ignore
/// assert_eq!(parse_size("3840x2160"), Some((3840, 2160)));
/// ```
pub fn parse_size(text: &str) -> Option<(u32, u32)> {
    let (w, h) = text.trim().split_once(['x', 'X'])?;
    let (w, h): (u32, u32) = (w.trim().parse().ok()?, h.trim().parse().ok()?);
    (w > 0 && h > 0).then_some((w, h))
}

/// 读取性能基准步数；未设置、非数字或为 0 时返回 `None`。
///
/// # 参数
/// - `value`：环境变量原始值。
///
/// ```ignore
/// assert_eq!(parse_bench_steps(Some("300")), Some(300));
/// ```
pub fn parse_bench_steps(value: Option<&str>) -> Option<u32> {
    value?.trim().parse::<u32>().ok().filter(|n| *n > 0)
}

/// 解析翻译自动化开关（验收用）：返回 `(缺运行时时自动按 D 下载, 下载成功后自动重试翻译)`。
///
/// 空值或缺省关闭；`download` 只自动下载；其它非空值（如 `1`、`retry`）下载后还会自动重试。
///
/// # 参数
/// - `value`：环境变量原始值。
///
/// ```ignore
/// assert_eq!(parse_translate_auto(Some("download")), (true, false));
/// ```
pub fn parse_translate_auto(value: Option<&str>) -> (bool, bool) {
    match value.map(str::trim) {
        None | Some("") => (false, false),
        Some("download") => (true, false),
        Some(_) => (true, true),
    }
}

/// 解析标注基准工具名（不区分大小写）；未知名称返回 `None`。
///
/// # 参数
/// - `value`：环境变量值。
///
/// ```ignore
/// assert_eq!(parse_bench_tool(Some("Arrow")), Some(AnnotationTool::Arrow));
/// ```
pub fn parse_bench_tool(value: Option<&str>) -> Option<AnnotationTool> {
    match value?.trim().to_ascii_lowercase().as_str() {
        "rectangle" | "rect" => Some(AnnotationTool::Rectangle),
        "ellipse" => Some(AnnotationTool::Ellipse),
        "arrow" => Some(AnnotationTool::Arrow),
        "line" => Some(AnnotationTool::Line),
        "pencil" | "pen" => Some(AnnotationTool::Pencil),
        "mosaic" => Some(AnnotationTool::Mosaic),
        "blur" => Some(AnnotationTool::Blur),
        "text" => Some(AnnotationTool::Text),
        _ => None,
    }
}

/// 生成合成底图（渐变），仅用于性能基准。
///
/// # 参数
/// - `width` / `height`：尺寸。
fn synthetic_screen(width: u32, height: u32) -> snow_platform::capture::CapturedScreen {
    let mut data = Vec::with_capacity(width as usize * height as usize * 4);
    for y in 0..height {
        for x in 0..width {
            data.extend_from_slice(&[
                (y * 255 / height) as u8,
                (x * 255 / width) as u8,
                ((x + y) & 0xFF) as u8,
                u8::MAX,
            ]);
        }
    }
    snow_platform::capture::CapturedScreen {
        width,
        height,
        data,
    }
}

/// 处理截图请求：选光标所在显示器，并在后台线程采集，完成后经收件箱回到主线程。
///
/// # 参数
/// - `cx`：GPUI 外壳上下文。
/// - `state`：运行时状态。
/// - `origin`：来源标签。
/// - `mode`：采集完成后覆盖窗的用途（截图 / 录屏选区）。
fn request_capture(
    cx: &mut ShellContext,
    state: &mut AppState,
    origin: &'static str,
    mode: CaptureMode,
) {
    let seq = on_capture_requested(state, origin);
    let overlay_open = any_overlay_open(cx, state);
    match capture_gate(state.capture_in_flight, overlay_open) {
        CaptureGate::Proceed => {}
        gate => {
            tracing::info!(seq, ?gate, "截图请求被忽略");
            return;
        }
    }
    let monitors = match cx.monitors() {
        Ok(m) => m,
        Err(e) => {
            tracing::error!(seq, error = %e, "枚举显示器失败，无法截图");
            return;
        }
    };
    let cursor = cursor_screen_position().ok();
    let Some(monitor) = pick_monitor(&monitors, cursor) else {
        tracing::error!(seq, "系统没有可用显示器，无法截图");
        return;
    };
    tracing::info!(
        seq,
        monitor = monitor.id.0,
        device = %monitor.name,
        bounds = ?monitor.bounds,
        scale = monitor.scale.value(),
        cursor = ?cursor,
        "开始采集光标所在显示器"
    );
    state.capture_in_flight = true;
    state.capture_mode = mode;
    // 基准合成底图模式：底图由 open_overlay 用合成渐变替换，不需要真实屏幕采集
    // （锁屏 / 屏保期间 GDI 采集必失败，此模式仍可跑标注性能基准）
    let synth = std::env::var(ENV_OVERLAY_SYNTH)
        .ok()
        .and_then(|v| parse_size(&v))
        .is_some();
    // 所有显示器同时出覆盖窗；合成底图的性能基准只开光标所在屏
    let targets: Vec<MonitorInfo> = if synth {
        vec![monitor.clone()]
    } else {
        monitors.all().to_vec()
    };
    state.capture_collector.begin(targets.len(), monitor.id);
    // 此刻覆盖窗尚未出现：先抓窗口快照（一条线程服务所有显示器），保证与冻结帧一致且不含 Cisox 自己的覆盖窗
    let canvas = canvas_bounds(targets.iter().map(|m| m.bounds));
    state.window_hover = start_window_hover(state.config.borrow().document(), &[canvas])
        .into_iter()
        .next()
        .flatten();
    let inbox = state.inbox.clone();
    if synth {
        tracing::info!(seq, "性能基准：跳过真实屏幕采集");
        inbox.push(UiEvent::CaptureReady(CapturePayload {
            monitor,
            screen: snow_platform::capture::CapturedScreen::new_solid(1, 1, (0, 0, 0, u8::MAX)),
            elapsed: Duration::ZERO,
        }));
        return;
    }
    for target in targets {
        let inbox = inbox.clone();
        let spawned = spawn_capture(target, move |result| {
            inbox.push(match result {
                Ok(payload) => UiEvent::CaptureReady(payload),
                Err(e) => UiEvent::CaptureFailed(e),
            });
        });
        if let Err(e) = spawned {
            state.capture_collector.cancel();
            state.capture_in_flight = false;
            state.capture_mode = CaptureMode::Screenshot;
            tracing::error!(seq, error = %e, "启动采集线程失败");
            return;
        }
    }
}

/// 计算光标相对显示器左上角的坐标（覆盖窗内坐标）；光标未知时取显示器中心。
///
/// # 参数
/// - `monitor`：目标显示器。
/// - `cursor`：光标屏幕坐标。
///
/// ```ignore
/// assert_eq!(cursor_in_monitor(&m, Some(PhysicalPoint::new(-1900, 210))).x, 20);
/// ```
pub fn cursor_in_monitor(monitor: &MonitorInfo, cursor: Option<PhysicalPoint>) -> PhysicalPoint {
    match cursor {
        Some(p) => PhysicalPoint::new(p.x - monitor.bounds.x, p.y - monitor.bounds.y),
        None => PhysicalPoint::new(monitor.bounds.width / 2, monitor.bounds.height / 2),
    }
}

/// 所有显示器采集到齐后，各开一个冻结覆盖窗：所有窗口共用**一个**逻辑视图，选区工作在虚拟桌面画布坐标上，
/// 每个窗口只负责渲染自己那块屏的一片。光标所在显示器最后开（拿键盘焦点，也是主窗口）。
///
/// # 参数
/// - `cx`：GPUI 外壳上下文。
/// - `state`：运行时状态。
/// - `payloads`：按开窗顺序排好的采集结果（光标所在屏在最后）。
fn open_overlays(cx: &mut ShellContext, state: &mut AppState, payloads: Vec<CapturePayload>) {
    state.capture_in_flight = false;
    let mode = std::mem::replace(&mut state.capture_mode, CaptureMode::Screenshot);
    state.overlay_windows.clear();
    if payloads.is_empty() {
        state.gesture = None;
        return;
    }
    // 性能基准可用合成底图替换真实截图（例如在非 4K 屏上测 4K 纹理）
    let synth = std::env::var(ENV_OVERLAY_SYNTH)
        .ok()
        .and_then(|v| parse_size(&v));
    // 画布 = 所有显示器的虚拟桌面外接矩形（桌面物理坐标，可为负）
    let canvas = canvas_bounds(payloads.iter().map(|p| p.monitor.bounds));
    let mut scale_override = None;
    let mut monitors: Vec<MonitorInfo> = Vec::with_capacity(payloads.len());
    let mut frames: Vec<MonitorFrame> = Vec::with_capacity(payloads.len());
    for payload in payloads {
        let CapturePayload {
            monitor,
            mut screen,
            elapsed,
        } = payload;
        tracing::info!(
            monitor = monitor.id.0,
            width = screen.width,
            height = screen.height,
            capture_ms = elapsed.as_millis() as u64,
            "采集完成，准备打开覆盖窗"
        );
        if let Some((w, h)) = synth {
            let logical_width = monitor.bounds.width as f32 / monitor.scale.value();
            scale_override = Some(w as f32 / logical_width);
            screen = synthetic_screen(w, h);
            tracing::info!(w, h, "性能基准：使用合成底图");
        }
        let frame = match FrozenFrame::from_captured(screen) {
            Ok(f) => f,
            Err(e) => {
                tracing::error!(error = %e, "构造冻结底图失败");
                return;
            }
        };
        let (frame_w, frame_h) = frame.size();
        let rect = if synth.is_some() {
            PhysicalRect::new(0, 0, frame_w as i32, frame_h as i32)
        } else {
            PhysicalRect::new(
                monitor.bounds.x - canvas.x,
                monitor.bounds.y - canvas.y,
                frame_w as i32,
                frame_h as i32,
            )
        };
        frames.push(MonitorFrame { rect, frame });
        monitors.push(monitor);
    }
    let frames = match DesktopFrames::new(frames) {
        Ok(f) => f,
        Err(e) => {
            tracing::error!(error = %e, "拼接多屏底图失败");
            return;
        }
    };
    let Some(primary) = monitors.last().cloned() else {
        return;
    };
    let cursor = match cursor_screen_position().ok() {
        Some(p) => PhysicalPoint::new(p.x - canvas.x, p.y - canvas.y),
        None => PhysicalPoint::new(canvas.width / 2, canvas.height / 2),
    };

    let (save_dir, source, format, supported) = {
        let store = state.config.borrow();
        let document = store.document();
        let (dir, source) = resolve_save_directory(document, home_directory().as_deref());
        let (format, supported) = configured_format(document);
        (dir, source, format, supported)
    };
    tracing::info!(dir = %save_dir.display(), source, "截图保存目录");
    if !supported {
        tracing::warn!(format, "配置的图片格式暂不支持，保存时使用 PNG");
    }

    let record_inbox = state.inbox.clone();
    let record_monitors = monitors.clone();
    let pin_inbox = state.inbox.clone();
    let ocr_inbox = state.inbox.clone();
    let ocr_download_inbox = state.inbox.clone();
    let table_inbox = state.inbox.clone();
    let table_download_inbox = state.inbox.clone();
    let recognition_inbox = state.inbox.clone();
    let translate_inbox = state.inbox.clone();
    let translate_download_inbox = state.inbox.clone();
    let scroll_inbox = state.inbox.clone();
    let scroll_monitors = monitors.clone();
    let output = Box::new(
        SystemOutput::new(save_dir)
            .with_config(state.config.clone())
            .with_recording(move |rect| {
                // 覆盖窗坐标以画布左上角为原点，换算成虚拟桌面坐标；录制窗绑定选区所在的那块显示器
                let region = monitor_local_to_desktop(rect, canvas);
                record_inbox.push(UiEvent::RecordingRegionChosen {
                    region,
                    monitor: session_monitor_for(&record_monitors, region),
                });
            })
            .with_pin(move |rect, width, height, rgba| {
                // 贴图在选区原位打开：同样把画布内坐标换算成虚拟桌面坐标
                pin_inbox.push(UiEvent::PinCreate {
                    rect: monitor_local_to_desktop(rect, canvas),
                    width,
                    height,
                    rgba,
                });
            })
            .with_ocr(move |serial, width, height, rgba| {
                ocr_inbox.push(UiEvent::OcrRequested {
                    serial,
                    width,
                    height,
                    rgba,
                });
            })
            .with_ocr_download(move || {
                ocr_download_inbox.push(UiEvent::OcrDownloadRequested);
            })
            .with_table(move |serial, width, height, rgba| {
                table_inbox.push(UiEvent::TableRequested {
                    serial,
                    width,
                    height,
                    rgba,
                });
            })
            .with_table_download(move || {
                table_download_inbox.push(UiEvent::TableDownloadRequested);
            })
            .with_recognition_window(move |data| {
                recognition_inbox.push(UiEvent::OpenRecognitionWindow(Box::new(data)));
            })
            .with_translate(move |serial, width, height, rgba| {
                translate_inbox.push(UiEvent::TranslateRequested {
                    serial,
                    width,
                    height,
                    rgba,
                });
            })
            .with_translate_download(move || {
                translate_download_inbox.push(UiEvent::TranslateDownloadRequested);
            })
            .with_scroll_capture(move |rect| {
                let region = monitor_local_to_desktop(rect, canvas);
                scroll_inbox.push(UiEvent::ScrollRegionChosen {
                    region,
                    monitor: session_monitor_for(&scroll_monitors, region),
                });
            }),
    );

    // 每块显示器一个窗口；第一个窗口负责创建共享视图，其余窗口复用它
    let mut frames_slot = Some(frames);
    let mut output_slot = Some(output);
    let mut shared: Option<Entity<ScreenshotOverlayView>> = None;
    let mut windows: Vec<ShellWindow> = Vec::with_capacity(monitors.len());
    let initial_scale = scale_override.unwrap_or_else(|| primary.scale.value());
    for (index, monitor) in monitors.iter().enumerate() {
        let mut spec = WindowSpec::overlay(MonitorTarget::Id(monitor.id));
        // 需要键盘（Esc / Enter / C），所以建窗时抢占焦点；底图不透明，无需窗口透明
        spec.focus = true;
        spec.transparent = false;
        let existing = shared.clone();
        let (frames_for, output_for) = if existing.is_none() {
            (frames_slot.take(), output_slot.take())
        } else {
            (None, None)
        };
        let opened = cx.open_window(&spec, move |window, app| {
            let entity = match (existing, frames_for, output_for) {
                (Some(entity), _, _) => entity,
                (None, Some(frames), Some(output)) => {
                    ScreenshotOverlayView::create_shared(app, frames, initial_scale, cursor, output)
                }
                _ => unreachable!("首个窗口必须带着底图与输出通道"),
            };
            app.new(|vcx| OverlayWindowView::new(entity, index, window, vcx))
        });
        match opened {
            Ok((window, root)) => {
                tracing::info!(
                    monitor = monitor.id.0,
                    rect = ?monitor.bounds,
                    hwnd = ?window.native_id().map(|id| id.0),
                    "screenshot overlay opened"
                );
                if shared.is_none() {
                    shared = Some(root.read(cx.app()).shared());
                }
                windows.push(window);
            }
            Err(e) => {
                tracing::error!(error = %e, monitor = monitor.id.0, "打开截图覆盖窗失败");
                if shared.is_none() {
                    return;
                }
            }
        }
    }
    let (Some(view), Some(&window)) = (shared, windows.last()) else {
        return;
    };
    state.overlay_windows = windows[..windows.len() - 1].to_vec();
    configure_overlay(cx, state, window, view, canvas, mode, scale_override);
}

/// 虚拟桌面外接矩形：所有显示器范围的并集（桌面物理坐标，可为负）；没有显示器时为空矩形。
///
/// # 参数
/// - `bounds`：各显示器的桌面物理范围。
///
/// ```ignore
/// let canvas = canvas_bounds([PhysicalRect::new(-1920, 0, 1920, 1080), PhysicalRect::new(0, 0, 2560, 1440)]);
/// assert_eq!((canvas.x, canvas.width), (-1920, 4480));
/// ```
fn canvas_bounds(bounds: impl IntoIterator<Item = PhysicalRect>) -> PhysicalRect {
    let mut iter = bounds.into_iter();
    let Some(first) = iter.next() else {
        return PhysicalRect::new(0, 0, 0, 0);
    };
    let (mut left, mut top, mut right, mut bottom) =
        (first.x, first.y, first.right(), first.bottom());
    for rect in iter {
        left = left.min(rect.x);
        top = top.min(rect.y);
        right = right.max(rect.right());
        bottom = bottom.max(rect.bottom());
    }
    PhysicalRect::new(left, top, right - left, bottom - top)
}

/// 选区（桌面物理坐标）所在的显示器：复用录屏流程的判定，都不相交时退回第一块。
///
/// # 参数
/// - `monitors`：本次会话的各显示器（至少一块）。
/// - `region`：选区（桌面物理坐标）。
fn session_monitor_for(monitors: &[MonitorInfo], region: PhysicalRect) -> MonitorInfo {
    monitor_for_region(monitors, region)
        .or_else(|| monitors.first())
        .cloned()
        .expect("会话至少有一块显示器")
}

/// 当前是否有任何一块显示器上的覆盖窗仍然打开。
fn any_overlay_open(cx: &ShellContext, state: &AppState) -> bool {
    state.overlay.as_ref().is_some_and(|w| cx.is_window_open(w))
        || state.overlay_windows.iter().any(|w| cx.is_window_open(w))
}

/// 某个覆盖窗关闭后，把同一会话里其余仍打开的窗口也关掉（走共享视图的关闭流程，释放底图与分块图集）。
///
/// # 参数
/// - `cx`：GPUI 外壳上下文。
/// - `state`：运行时状态。
fn close_all_overlays(cx: &mut ShellContext, state: &mut AppState) {
    // 每个窗口关闭都会触发一次本函数：取走会话句柄，后到的重复事件就无事可做
    state.gesture = None;
    let Some(view) = state.overlay_view.take() else {
        state.overlay_windows.clear();
        return;
    };
    let mut windows = std::mem::take(&mut state.overlay_windows);
    if let Some(primary) = state.overlay.take() {
        windows.push(primary);
    }
    for window in windows {
        if cx.is_window_open(&window) {
            let view = view.clone();
            let _ = window
                .gpui_handle()
                .update(cx.app(), |_, w, app| view.update(app, |v, _| v.close(w)));
        }
    }
}

/// 给刚创建好的共享视图装配会话所需的一切：句柄、键位、语言、样式、历史、悬停来源、模式与基准。
///
/// # 参数
/// - `cx`：GPUI 外壳上下文。
/// - `state`：运行时状态。
/// - `window`：主窗口（光标所在显示器上的窗口，也是对话框的所有者）。
/// - `view`：共享视图。
/// - `canvas`：虚拟桌面外接矩形（桌面物理坐标）；其左上角是画布原点。
/// - `mode`：覆盖窗的用途。
/// - `scale_override`：性能基准的固定缩放比。
fn configure_overlay(
    cx: &mut ShellContext,
    state: &mut AppState,
    window: ShellWindow,
    view: Entity<ScreenshotOverlayView>,
    canvas: PhysicalRect,
    mode: CaptureMode,
    scale_override: Option<f32>,
) {
    let record_mode = mode == CaptureMode::Record;
    let scroll_mode = mode == CaptureMode::Scroll;
    let auto_confirm = match mode {
        CaptureMode::Quick(action) => Some(action),
        _ => None,
    };
    view.update(cx.app(), |v, _| {
        v.set_canvas_origin(PhysicalPoint::new(canvas.x, canvas.y))
    });
    // 另存为对话框要以覆盖窗为所有者，否则会被置顶的覆盖窗盖住
    if let Some(id) = window.native_id() {
        view.update(cx.app(), |v, _| v.set_owner_window(id.0));
    }
    let (keymap, locale) = {
        let store = state.config.borrow();
        (
            crate::overlay_keymap::OverlayKeymap::from_document(store.document()),
            interface_locale(store.document()),
        )
    };
    view.update(cx.app(), |v, _| {
        v.set_keymap(keymap);
        v.set_locale(&locale);
    });
    state.overlay = Some(window);
    state.overlay_view = Some(view.clone());
    // 多屏会话协调：任一窗口关闭时，其余显示器上的窗口跟着关闭
    {
        let close_inbox = state.inbox.clone();
        view.update(cx.app(), |v, _| {
            let recapture = v.recapture_flag();
            v.set_close_hook(move || {
                close_inbox.push(UiEvent::OverlayClosed);
                // 「重新截图」：覆盖窗收尾后再发起一次普通截图
                if recapture.take() {
                    close_inbox.push(UiEvent::Capture {
                        origin: ORIGIN_HOTKEY,
                    });
                }
            });
        });
    }
    // 标注样式：读取已保存的各工具样式，之后的修改写回同一份配置
    let style_locale = ui_prefs_from_document(state.config.borrow().document()).locale;
    let style_config = state.config.clone();
    view.update(cx.app(), |v, _| {
        v.set_style_config(style_config, style_locale)
    });
    // 选区形状：读取上次使用的形状（矩形 / 折线 / 曲线 / 自由绘制）
    {
        let region_type = crate::region_select::RegionType::from_config(
            state
                .config
                .borrow()
                .document()
                .value("screenshot_selection/region_type")
                .as_str()
                .unwrap_or_default(),
        );
        view.update(cx.app(), |v, _| v.set_initial_region_type(region_type));
    }
    // 历史翻页：覆盖窗里用快捷键在截图历史里前后翻，读盘在后台线程
    {
        let policy = policy_from_document(state.config.borrow().document());
        match ThreadedHistoryProvider::start(&state.data_root, policy) {
            Ok(provider) => {
                view.update(cx.app(), |v, _| v.set_history_provider(Box::new(provider)));
            }
            Err(e) => tracing::warn!(error = %e, "启动截图历史读取线程失败，翻页不可用"),
        }
    }
    // 导出成功后由视图把整帧、选区、标注历史与结果图交给历史写入线程
    if let Some(recorder) = state.history.clone() {
        let config = Rc::clone(&state.config);
        view.update(cx.app(), |v, _| {
            v.set_history_sink(move |source, snapshot| {
                let policy = policy_from_document(config.borrow().document());
                recorder.submit_snapshot(policy, source, snapshot);
            });
        });
    }
    if let Some(hover) = state.window_hover.take() {
        let (target, animate) = {
            let config = state.config.borrow();
            (
                selection_target(config.document()),
                transition_animation_enabled(config.document()),
            )
        };
        view.update(cx.app(), |v, _| {
            v.set_window_hover(Some(hover), target, animate)
        });
    }
    if record_mode {
        view.update(cx.app(), |v, _| v.set_record_mode(true));
    }
    if scroll_mode {
        view.update(cx.app(), |v, _| v.set_scroll_mode(true));
    }
    if auto_confirm.is_some() {
        view.update(cx.app(), |v, _| v.set_auto_confirm(auto_confirm));
    }
    if let Some(s) = scale_override {
        view.update(cx.app(), |v, _| v.set_scale_override(Some(s)));
    }
    let bench = parse_bench_steps(std::env::var(ENV_OVERLAY_BENCH).ok().as_deref());
    if let Some(steps) = bench {
        let tool = parse_bench_tool(std::env::var(ENV_OVERLAY_BENCH_TOOL).ok().as_deref());
        let hold = std::env::var_os(ENV_OVERLAY_BENCH_HOLD).is_some();
        let action = if std::env::var_os(ENV_OVERLAY_BENCH_PIN).is_some() {
            Some(ToolbarAction::Pin)
        } else if std::env::var_os(ENV_OVERLAY_BENCH_OCR).is_some() {
            Some(ToolbarAction::Ocr)
        } else if std::env::var_os(ENV_OVERLAY_BENCH_TRANSLATE).is_some() {
            Some(ToolbarAction::Translate)
        } else if std::env::var_os(ENV_OVERLAY_BENCH_SCROLL).is_some() {
            Some(ToolbarAction::ScrollCapture)
        } else {
            None
        };
        spawn_overlay_bench(cx, window, view, steps, tool, hold, action);
    }
    // 鼠标手势触发的截图：把已发生的按下 / 移动补发给刚打开的覆盖窗
    apply_pending_gesture(cx, state);
}

/// 处理总线上的导出命令：交给当前打开的覆盖窗按选区复制 / 保存；没有覆盖窗时只记日志。
///
/// # 参数
/// - `cx`：GPUI 外壳上下文。
/// - `state`：运行时状态。
/// - `target`：导出去向。
fn export_from_overlay(cx: &mut ShellContext, state: &mut AppState, target: &ExportTarget) {
    let (Some(window), Some(view)) = (state.overlay.as_ref(), state.overlay_view.clone()) else {
        tracing::warn!("没有进行中的截图会话，导出命令被忽略");
        return;
    };
    if !cx.is_window_open(window) {
        tracing::warn!("截图覆盖窗已关闭，导出命令被忽略");
        return;
    }
    let target = target.clone();
    let _ = window.gpui_handle().update(cx.app(), |_, window, app| {
        view.update(app, |v, vcx| v.run_export(&target, window, vcx));
    });
}

/// 当前界面语言代码：配置优先，没有则跟随系统。
///
/// # 参数
/// - `document`：配置文档。
pub fn interface_locale(document: &ConfigDocument) -> String {
    document
        .value(crate::settings_model::LANGUAGE_KEY)
        .as_str()
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .map(str::to_string)
        .unwrap_or_else(system_ui_language)
}

/// 处理直接截图：在后台线程采集目标区域并直接复制 / 保存，结果只记日志（没有选区界面可提示）。
///
/// # 参数
/// - `cx`：GPUI 外壳上下文。
/// - `state`：运行时状态（读配置）。
/// - `request`：直接截图请求。
fn direct_capture(cx: &mut ShellContext, state: &mut AppState, request: DirectCaptureRequest) {
    if request.output == DirectOutput::Render {
        tracing::warn!("直接截图的 render 输出需要会话返回通道，暂未支持");
        return;
    }
    if request
        .scale
        .is_some_and(|s| (s - 1.0).abs() > f64::EPSILON)
        || request.capture_cursor == Some(true)
    {
        tracing::warn!(scale = ?request.scale, cursor = ?request.capture_cursor, "直接截图的缩放与光标采集暂未支持，已忽略");
    }
    let region = match request.target {
        DirectTarget::CurrentMonitor => {
            let monitors = match cx.monitors() {
                Ok(m) => m,
                Err(e) => {
                    tracing::error!(error = %e, "枚举显示器失败，直接截图取消");
                    return;
                }
            };
            pick_monitor(&monitors, cursor_screen_position().ok())
                .and_then(|m| crate::capture_flow::capture_region(&m))
        }
        DirectTarget::FocusedWindow => snow_platform::window_rect::foreground_window_rect(),
    };
    let Some(region) = region else {
        tracing::error!(target = ?request.target, "没有可采集的区域，直接截图取消");
        return;
    };
    let (settings, locale) = {
        let store = state.config.borrow();
        (
            ExportSettings::from_document(store.document()),
            interface_locale(store.document()),
        )
    };
    let spawned = std::thread::Builder::new().name("direct-capture".into()).spawn(move || {
        let screen = match snow_platform::capture::capture_display(Some(region)) {
            Ok(screen) => screen,
            Err(e) => {
                tracing::error!(error = %e, "直接截图采集失败");
                return;
            }
        };
        let (width, height) = (screen.width, screen.height);
        // GDI 采集的 alpha 不可靠，导出前一律置为不透明
        let mut rgba = screen.to_rgba();
        rgba.chunks_exact_mut(4).for_each(|px| px[3] = u8::MAX);
        let result = export_direct(
            &settings,
            &request,
            width,
            height,
            &rgba,
            home_directory().as_deref(),
            snow_platform::local_time::now(),
            &mut |w, h, px| snow_platform::clipboard::copy_image_to_clipboard(w, h, px),
        );
        match result {
            Ok(done) => {
                if let Some(e) = &done.auto_save_error {
                    tracing::warn!(error = %e, message = %e.auto_message(&locale), "直接截图复制后自动保存失败");
                }
                tracing::info!(copied = done.copied, saved = ?done.saved, width, height, "直接截图完成");
            }
            Err(e) => tracing::error!(error = %e, message = %e.manual_message(&locale), "直接截图导出失败"),
        }
    });
    if let Err(e) = spawned {
        tracing::error!(error = %e, "启动直接截图线程失败");
    }
}

/// 在后台线程执行文字识别（阻塞调用不进主线程），结果经收件箱回到主线程。
///
/// # 参数
/// - `state`：运行时状态（取 OCR 服务、配置与收件箱）。
/// - `serial`：请求序号。
/// - `width` / `height` / `rgba`：待识别图像。
fn spawn_ocr(state: &AppState, serial: u64, width: u32, height: u32, rgba: Vec<u8>) {
    let selection = select_from_document(state.config.borrow().document(), Arc::clone(&state.ocr));
    if let Some(notice) = selection.notice {
        tracing::warn!(?notice, requested = ?selection.requested, effective = ?selection.effective, "OCR 后端回落");
    }
    let engine = selection.engine;
    let inbox = state.inbox.clone();
    let spawned = std::thread::Builder::new()
        .name("snow-ocr-request".into())
        .spawn(move || {
            let result = engine.recognize(&OcrInput {
                width,
                height,
                rgba: &rgba,
            });
            inbox.push(UiEvent::OcrFinished { serial, result });
        });
    if let Err(e) = spawned {
        tracing::error!(error = %e, "无法创建 OCR 线程");
        state.inbox.push(UiEvent::OcrFinished {
            serial,
            result: Err(OcrError::SpawnFailed(e.to_string())),
        });
    }
}

/// 在后台线程读取前台应用选中的文字（先于任何自身窗口激活），结果经收件箱回到主线程。
///
/// 读取走无障碍接口，必要时回退到「复制」并还原剪贴板；超时 2 秒。
///
/// # 参数
/// - `state`：运行时状态（取收件箱）。
fn spawn_selected_text_capture(state: &AppState) {
    let inbox = state.inbox.clone();
    let spawned = std::thread::Builder::new()
        .name("snow-selected-text".into())
        .spawn(move || {
            let text = read_selected_text();
            inbox.push(UiEvent::SelectedTextReady(text));
        });
    if let Err(e) = spawned {
        tracing::error!(error = %e, "无法创建读取选中文字的线程");
        state.inbox.push(UiEvent::SelectedTextReady(None));
    }
}

/// 同步读取前台应用选中的文字；没有选中、不支持或失败返回 `None`（阻塞，勿在界面线程调用）。
fn read_selected_text() -> Option<String> {
    use snow_selected_text::{CaptureOptions, SelectedTextService, SelectionOutcome};
    let service = match SelectedTextService::new() {
        Ok(s) => s,
        Err(e) => {
            tracing::warn!(error = ?e, "选中文字服务不可用");
            return None;
        }
    };
    // 排除自身：避免读到本程序窗口里的文字
    let own = std::env::current_exe()
        .ok()
        .and_then(|p| p.file_name().map(|n| n.to_string_lossy().into_owned()))
        .into_iter()
        .collect();
    let options = CaptureOptions {
        excluded_executables: own,
        ..CaptureOptions::default()
    };
    let request = match service.start_capture(options) {
        Ok(r) => r,
        Err(e) => {
            tracing::warn!(error = ?e, "启动选中文字读取失败");
            return None;
        }
    };
    match request.wait().as_ref() {
        Ok(SelectionOutcome::Selected(selected)) => {
            let text = selected.text.trim().to_string();
            (!text.is_empty()).then_some(text)
        }
        Ok(_) => None,
        Err(e) => {
            tracing::warn!(error = ?e, "读取选中文字失败");
            None
        }
    }
}

/// 可以贴到屏幕的图片扩展名（小写）。
const PINNABLE_IMAGE_EXTENSIONS: [&str; 6] = ["png", "jpg", "jpeg", "bmp", "gif", "webp"];
/// 一次最多贴多少个选中的文件。
const MAX_PIN_FILES: usize = 12;

/// 判断路径是不是可贴的图片文件（只看扩展名）。
///
/// # 参数
/// - `path`：文件路径。
///
/// ```ignore
/// assert!(is_pinnable_image(std::path::Path::new("a.PNG")));
/// ```
pub fn is_pinnable_image(path: &std::path::Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| PINNABLE_IMAGE_EXTENSIONS.contains(&e.to_ascii_lowercase().as_str()))
}

/// 在后台线程读取前台资源管理器 / 桌面里选中的图片并逐张解码，结果经收件箱回到主线程贴出。
///
/// # 参数
/// - `state`：运行时状态（取收件箱）。
fn spawn_pin_selected_files(state: &AppState) {
    let inbox = state.inbox.clone();
    let spawned = std::thread::Builder::new()
        .name("snow-pin-files".into())
        .spawn(move || {
            let files: Vec<_> = snow_platform::selected_files::foreground_selected_files()
                .into_iter()
                .filter(|p| is_pinnable_image(p))
                .take(MAX_PIN_FILES)
                .collect();
            if files.is_empty() {
                inbox.push(UiEvent::PinFilesEmpty);
                return;
            }
            let mut pinned = 0usize;
            for path in files {
                match image::open(&path) {
                    Ok(img) => {
                        let rgba = img.to_rgba8();
                        let (width, height) = rgba.dimensions();
                        inbox.push(UiEvent::PinImage {
                            width,
                            height,
                            rgba: rgba.into_raw(),
                        });
                        pinned += 1;
                    }
                    Err(e) => {
                        tracing::warn!(path = %path.display(), error = %e, "解码要贴的图片失败")
                    }
                }
            }
            if pinned == 0 {
                inbox.push(UiEvent::PinFilesEmpty);
            }
        });
    if let Err(e) = spawned {
        tracing::error!(error = %e, "无法创建贴选中文件的线程");
    }
}

/// 在后台线程识别一张贴图的文字，结果经收件箱回到主线程。
///
/// # 参数
/// - `state`：运行时状态（取 OCR 引擎选择与收件箱）。
/// - `id`：贴图 ID。
/// - `width` / `height` / `rgba`：贴图图像。
fn spawn_pin_ocr(state: &AppState, id: String, width: u32, height: u32, rgba: Vec<u8>) {
    let selection = select_from_document(state.config.borrow().document(), Arc::clone(&state.ocr));
    if let Some(notice) = selection.notice {
        tracing::warn!(?notice, requested = ?selection.requested, effective = ?selection.effective, "OCR 后端回落");
    }
    let engine = selection.engine;
    let inbox = state.inbox.clone();
    let done_id = id.clone();
    let spawned = std::thread::Builder::new()
        .name("snow-pin-ocr".into())
        .spawn(move || {
            let result = engine
                .recognize(&OcrInput {
                    width,
                    height,
                    rgba: &rgba,
                })
                .map_err(|e| format!("{e:?}"));
            inbox.push(UiEvent::PinOcrFinished {
                id: done_id,
                result,
            });
        });
    if let Err(e) = spawned {
        tracing::error!(error = %e, "无法创建贴图 OCR 线程");
        state.inbox.push(UiEvent::PinOcrFinished {
            id,
            result: Err(e.to_string()),
        });
    }
}

/// 在后台线程执行“识别 + 翻译”（阻塞调用不进主线程），阶段与结果经收件箱回到主线程。
///
/// # 参数
/// - `state`：运行时状态（取 OCR 服务、翻译宿主、配置与收件箱）。
/// - `serial`：请求序号。
/// - `width` / `height` / `rgba`：选区图像。
fn spawn_translate(state: &AppState, serial: u64, width: u32, height: u32, rgba: Vec<u8>) {
    let (selection, translate_config) = {
        let config = state.config.borrow();
        (
            select_from_document(config.document(), Arc::clone(&state.ocr)),
            TranslateConfig::from_document(config.document(), &system_ui_language()),
        )
    };
    if let Some(notice) = selection.notice {
        tracing::warn!(?notice, requested = ?selection.requested, effective = ?selection.effective, "OCR 后端回落");
    }
    let ocr = selection.engine;
    let translator = Arc::clone(&state.translator);
    let inbox = state.inbox.clone();
    let spawned = std::thread::Builder::new()
        .name("snow-translate-request".into())
        .spawn(move || {
            let progress_inbox = inbox.clone();
            let result = run_flow(
                || {
                    ocr.recognize(&OcrInput {
                        width,
                        height,
                        rgba: &rgba,
                    })
                },
                translator.as_ref(),
                &translate_config,
                |stage| {
                    progress_inbox.push(UiEvent::TranslateProgress { serial, stage });
                },
            );
            inbox.push(UiEvent::TranslateFinished { serial, result });
        });
    if let Err(e) = spawned {
        tracing::error!(error = %e, "无法创建翻译线程");
        state.inbox.push(UiEvent::TranslateFinished {
            serial,
            result: Err(TranslateFlowError::Ocr(OcrError::SpawnFailed(
                e.to_string(),
            ))),
        });
    }
}

/// 在后台线程下载翻译用的 onnxruntime 运行时（curl + 哈希校验），进度与结果经收件箱回到主线程。
///
/// # 参数
/// - `state`：运行时状态。
fn spawn_translate_runtime_download(state: &AppState) {
    let data_root = state.data_root.clone();
    let inbox = state.inbox.clone();
    let i18n = crate::ocr_backend::i18n_for(
        ui_prefs_from_document(state.config.borrow().document()).locale,
    );
    let spawned = std::thread::Builder::new()
        .name("snow-ort-download".into())
        .spawn(move || {
            tracing::info!(root = %data_root.display(), "开始下载 onnxruntime 运行时");
            let cancel = AtomicBool::new(false);
            let progress_inbox = inbox.clone();
            let result = ort_runtime::install(&data_root, &cancel, |step| {
                progress_inbox.push(UiEvent::TranslateDownloadProgress(step.message(i18n)));
            })
            .map(|_| ())
            .map_err(|e| e.message(i18n));
            inbox.push(UiEvent::TranslateDownloadFinished(result));
        });
    if let Err(e) = spawned {
        let message = ocr_download::FetchError::TaskStart(e.to_string()).message(i18n);
        state
            .inbox
            .push(UiEvent::TranslateDownloadFinished(Err(message)));
    }
}

/// 在后台线程做表格识别：先查组件是否齐全，再跑 OCR 取文字框，最后拉起 `snow-table` 做结构推理并合并。
///
/// 结果走 `OcrFinished`（同文字识别），缺组件时给出可下载的引导。
///
/// # 参数
/// - `state`：运行时状态。
/// - `serial`：请求序号。
/// - `width` / `height` / `rgba`：选区图像。
fn spawn_table(state: &AppState, serial: u64, width: u32, height: u32, rgba: Vec<u8>) {
    let selection = select_from_document(state.config.borrow().document(), Arc::clone(&state.ocr));
    let engine = selection.engine;
    let data_root = state.data_root.clone();
    let inbox = state.inbox.clone();
    let spawned = std::thread::Builder::new()
        .name("snow-table-request".into())
        .spawn(move || {
            let result = run_table(&data_root, &*engine, width, height, &rgba);
            inbox.push(UiEvent::OcrFinished { serial, result });
        });
    if let Err(e) = spawned {
        tracing::error!(error = %e, "无法创建表格识别线程");
        state.inbox.push(UiEvent::OcrFinished {
            serial,
            result: Err(OcrError::SpawnFailed(e.to_string())),
        });
    }
}

/// 表格识别的阻塞主体（后台线程里调用）。
///
/// # 参数
/// - `data_root`：数据根目录。
/// - `engine`：当前选中的 OCR 后端。
/// - `width` / `height` / `rgba`：选区图像。
fn run_table(
    data_root: &std::path::Path,
    engine: &dyn crate::ocr_backend::OcrEngine,
    width: u32,
    height: u32,
    rgba: &[u8],
) -> Result<OcrResult, OcrError> {
    let beside = std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(std::path::Path::to_path_buf));
    let assets = crate::table_assets::resolve_assets(
        data_root,
        std::env::var(crate::table_assets::ENV_TABLE_ASSET_DIR)
            .ok()
            .as_deref(),
        std::env::var_os(crate::table_assets::ENV_TABLE_EXE)
            .map(PathBuf::from)
            .as_deref(),
        std::env::var(snow_translate::worker::ENV_ORT_DYLIB)
            .ok()
            .as_deref(),
        beside.as_deref(),
    )
    .map_err(OcrError::TableUnavailable)?;
    let ocr = engine.recognize(&OcrInput { width, height, rgba })?;
    let runner = crate::table_service::ProcessRunner::new(assets);
    crate::table_service::table_result(&runner, width, height, rgba, ocr)
}

/// 在后台线程下载表格识别组件（模型与缺失的 onnxruntime），进度与结果复用 OCR 下载事件。
///
/// # 参数
/// - `state`：运行时状态。
fn spawn_table_download(state: &AppState) {
    let data_root = state.data_root.clone();
    let inbox = state.inbox.clone();
    let i18n = crate::ocr_backend::i18n_for(
        ui_prefs_from_document(state.config.borrow().document()).locale,
    );
    let spawned = std::thread::Builder::new()
        .name("snow-table-download".into())
        .spawn(move || {
            let env_root = std::env::var(crate::table_assets::ENV_TABLE_ASSET_DIR).ok();
            tracing::info!(root = %crate::table_assets::table_root(&data_root, env_root.as_deref()).display(), "开始下载表格识别组件");
            let cancel = AtomicBool::new(false);
            let progress_inbox = inbox.clone();
            let result = crate::table_assets::download_missing(
                &data_root,
                env_root.as_deref(),
                &cancel,
                |step| {
                    progress_inbox.push(UiEvent::OcrDownloadProgress(step.message(i18n)));
                },
            )
            .map_err(|e| e.message(i18n));
            inbox.push(UiEvent::OcrDownloadFinished(result));
        });
    if let Err(e) = spawned {
        let message = ocr_download::FetchError::TaskStart(e.to_string()).message(i18n);
        state.inbox.push(UiEvent::OcrDownloadFinished(Err(message)));
    }
}

/// 在后台线程下载缺失的 OCR 组件（curl + 哈希校验），进度与结果经收件箱回到主线程。
///
/// # 参数
/// - `state`：运行时状态。
fn spawn_ocr_download(state: &AppState) {
    let model_kind = OcrRequestConfig::from_document(state.config.borrow().document()).model_kind;
    let data_root = state.data_root.clone();
    let need_runtime = state.ocr.exe_override().is_none();
    let inbox = state.inbox.clone();
    let i18n = crate::ocr_backend::i18n_for(
        ui_prefs_from_document(state.config.borrow().document()).locale,
    );
    let spawned = std::thread::Builder::new().name("snow-ocr-download".into()).spawn(move || {
        let env_root = std::env::var(ENV_OCR_ASSET_DIR).ok();
        tracing::info!(root = %ocr_root(&data_root, env_root.as_deref()).display(), model = %model_kind, "开始下载 OCR 组件");
        let cancel = AtomicBool::new(false);
        let progress_inbox = inbox.clone();
        let result = ocr_download::download_missing(
            &data_root,
            env_root.as_deref(),
            &model_kind,
            need_runtime,
            &cancel,
            |step| {
                progress_inbox.push(UiEvent::OcrDownloadProgress(step.message(i18n)));
            },
        )
        .map_err(|e| e.message(i18n));
        inbox.push(UiEvent::OcrDownloadFinished(result));
    });
    if let Err(e) = spawned {
        let message = ocr_download::FetchError::TaskStart(e.to_string()).message(i18n);
        state.inbox.push(UiEvent::OcrDownloadFinished(Err(message)));
    }
}

/// 在后台线程安装语音模型（含离线模式缺的共享 VAD），进度与结果经收件箱回到主线程。
///
/// # 参数
/// - `state`：运行时状态。
/// - `model_id`：模型 ID。
/// - `cancel`：取消标记。
fn spawn_stt_download(state: &AppState, model_id: String, cancel: CancelFlag) {
    let data_root = state.data_root.clone();
    let inbox = state.inbox.clone();
    let id = model_id.clone();
    let spawned = std::thread::Builder::new().name("snow-stt-download".into()).spawn(move || {
        let result = match stt_models::find(&id) {
            None => Err(ocr_download::FetchError::Technical(format!("unknown speech model: {id}"))),
            Some(spec) => {
                let progress_inbox = inbox.clone();
                stt_download::install(spec, &data_root, &cancel.0, |p| {
                    progress_inbox.push(UiEvent::SttDownloadProgress(p.clone()));
                })
                .map(|report| {
                    if !report.unpinned.is_empty() {
                        tracing::warn!(assets = ?report.unpinned, "语音模型资产未固定校验值，仅校验了大小");
                    }
                })
            }
        };
        inbox.push(UiEvent::SttDownloadFinished { model_id: id, result });
    });
    if let Err(e) = spawned {
        state.inbox.push(UiEvent::SttDownloadFinished {
            model_id,
            result: Err(ocr_download::FetchError::TaskStart(e.to_string())),
        });
    }
}

/// 执行设置导出 / 导入：弹原生文件对话框（阻塞）、读写归档，再把结果交给设置页；
/// 导入成功后对有变化的键逐个走配置变更处理，让主题、语言、热键等即时生效。
///
/// # 参数
/// - `cx`：外壳上下文。
/// - `state`：运行时状态。
/// - `action`：导出或导入。
fn run_config_transfer(
    cx: &mut ShellContext,
    state: &mut AppState,
    action: crate::config_transfer::TransferAction,
) {
    use crate::config_transfer::{
        ARCHIVE_EXTENSION, ARCHIVE_PATTERN, TransferAction, default_export_name,
        export_configuration, export_state, import_configuration, import_state,
    };
    use snow_platform::file_dialog::{
        FileFilter, OpenDialogRequest, SaveDialogRequest, show_open_dialog, show_save_dialog,
    };
    let locale = ui_prefs_from_document(state.config.borrow().document()).locale;
    let i18n = crate::ocr_backend::i18n_for(locale);
    let filters = vec![FileFilter {
        label: i18n.tr("config-transfer-filter"),
        pattern: ARCHIVE_PATTERN.to_string(),
    }];
    let (ui_state, reload) = match action {
        TransferAction::Export { include_credentials } => {
            let request = SaveDialogRequest {
                title: i18n.tr("config-transfer-dialog-export"),
                file_name: default_export_name(&snow_config::archive::iso_utc_now()),
                filters,
                ..SaveDialogRequest::default()
            };
            match show_save_dialog(&request) {
                Ok(Some(choice)) => {
                    let mut path = choice.path;
                    if path.extension().is_none() {
                        path.as_mut_os_string().push(ARCHIVE_EXTENSION);
                    }
                    let result = export_configuration(&state.config.borrow(), &path, include_credentials);
                    if let Err(e) = &result {
                        tracing::warn!(error = %e, "导出设置失败");
                    }
                    (export_state(&result, locale), false)
                }
                Ok(None) => return,
                Err(e) => {
                    tracing::warn!(error = %e, "导出设置：文件对话框失败");
                    return;
                }
            }
        }
        TransferAction::Import => {
            let request = OpenDialogRequest {
                title: i18n.tr("config-transfer-dialog-import"),
                filters,
                ..OpenDialogRequest::default()
            };
            match show_open_dialog(&request) {
                Ok(Some(path)) => {
                    let result = import_configuration(&mut state.config.borrow_mut(), &path);
                    let outcome = result.map(|changed| {
                        let count = changed.len();
                        for (key, previous) in changed {
                            state.inbox.push(UiEvent::ConfigChanged { key, previous });
                        }
                        count
                    });
                    if let Err(e) = &outcome {
                        tracing::warn!(error = ?e, "导入设置失败");
                    }
                    (import_state(&outcome, locale), outcome.is_ok())
                }
                Ok(None) => return,
                Err(e) => {
                    tracing::warn!(error = %e, "导入设置：文件对话框失败");
                    return;
                }
            }
        }
    };
    for view in settings_views(state, cx.app()) {
        let ui_state = ui_state.clone();
        view.update(cx.app(), |v, vcx| v.finish_transfer(ui_state, reload, vcx));
    }
}

/// 执行设置页“更新”分组的动作：检查更新、下载更新包（后台线程）、打开已下载目录。
///
/// # 参数
/// - `state`：运行时状态。
/// - `action`：用户触发的动作。
fn run_update_action(state: &AppState, action: crate::net_settings::UpdateAction) {
    use crate::net_settings::UpdateAction;
    match action {
        UpdateAction::Check => start_update_check(state),
        UpdateAction::Download(info) => {
            let inbox = state.inbox.clone();
            let data_root = state.data_root.clone();
            let worker = (inbox.clone(), info.clone());
            let spawned = std::thread::Builder::new()
                .name("snow-update-download".into())
                .spawn(move || {
                    let outcome = crate::net_settings::download_update(&info, &data_root);
                    inbox.push(UiEvent::UpdateDownloadFinished(info, outcome));
                });
            if let Err(e) = spawned {
                let (inbox, info) = worker;
                let outcome = crate::net_settings::UpdateDownloadOutcome::Failed(
                    crate::ocr_download::FetchError::RunTool {
                        tool: "thread",
                        detail: e.to_string(),
                    },
                );
                inbox.push(UiEvent::UpdateDownloadFinished(info, outcome));
            }
        }
        UpdateAction::OpenFolder(dir) => {
            if let Err(e) = std::process::Command::new("explorer.exe").arg(&dir).spawn() {
                tracing::warn!(dir = %dir.display(), error = %e, "打开更新包目录失败");
            }
        }
    }
}

/// 开始检查更新：先解析配置里的清单地址（未配置 / 非法直接回结果），再起后台线程下载比对。
///
/// # 参数
/// - `state`：运行时状态。
fn start_update_check(state: &AppState) {
    let locale = ui_prefs_from_document(state.config.borrow().document()).locale;
    let url = match crate::net_settings::update_target(state.config.borrow().document(), locale) {
        Ok(url) => url,
        Err(text) => {
            state.inbox.push(UiEvent::UpdateCheckFinished(
                crate::net_settings::UpdateCheckOutcome::Config(text),
            ));
            return;
        }
    };
    let inbox = state.inbox.clone();
    let spawned = std::thread::Builder::new()
        .name("snow-update-check".into())
        .spawn({
            let inbox = inbox.clone();
            move || {
                let outcome =
                    crate::net_settings::run_update_check(&url, crate::net_settings::APP_VERSION);
                inbox.push(UiEvent::UpdateCheckFinished(outcome));
            }
        });
    if let Err(e) = spawned {
        inbox.push(UiEvent::UpdateCheckFinished(
            crate::net_settings::UpdateCheckOutcome::FetchFailed(e.to_string()),
        ));
    }
}

/// 启动覆盖窗性能基准：约 60Hz 驱动模拟框选（或指定工具的标注绘制），结束后关闭窗口（探针汇总写入日志）。
///
/// 这是直接驱动视图状态，不经过操作系统输入，不属于输入模拟。
///
/// # 参数
/// - `cx`：GPUI 外壳上下文。
/// - `window`：覆盖窗句柄。
/// - `view`：覆盖窗视图实体。
/// - `steps`：总步数。
/// - `tool`：给出时改为在选区内绘制该工具的标注。
/// - `hold`：为真时基准结束后不关闭窗口（用于外部截屏取像素）。
/// - `action`：给出时基准结束后触发该工具栏动作（返回关闭则关闭覆盖窗，否则保持打开等待结果）。
fn spawn_overlay_bench(
    cx: &mut ShellContext,
    window: ShellWindow,
    view: Entity<ScreenshotOverlayView>,
    steps: u32,
    tool: Option<AnnotationTool>,
    hold: bool,
    action: Option<ToolbarAction>,
) {
    tracing::info!(steps, ?tool, hold, "性能基准开始");
    // 只触发工具栏动作（OCR / 长图 / 贴图）时不需要框选轨迹：直接选中屏幕内缩一圈的区域
    let select_only = action.is_some() && tool.is_none();
    if tool.is_some() || select_only {
        view.update(cx.app(), |v, _| {
            v.bench_annotation_setup(tool.unwrap_or(AnnotationTool::None))
        });
    }
    let weak = view.downgrade();
    cx.app()
        .spawn(async move |acx| {
            for step in 0..steps {
                acx.background_executor().timer(BENCH_FRAME_INTERVAL).await;
                let stepped = weak.update(acx, |v, cx| {
                    match tool {
                        Some(_) => v.bench_annotation_step(step, steps),
                        None if select_only => {}
                        None => v.bench_step(step, steps),
                    }
                    cx.notify();
                });
                if stepped.is_err() {
                    // 覆盖窗已被关闭（例如手动取消），基准提前结束
                    return;
                }
            }
            acx.background_executor().timer(BENCH_FRAME_INTERVAL).await;
            if let Some(action) = action {
                let _ = window.gpui_handle().update(acx, |_, window, app| {
                    view.update(app, |v, cx| {
                        if v.apply_action(action) == OverlayOutcome::Close {
                            v.close(window);
                        }
                        cx.notify();
                    });
                });
                return;
            }
            if hold {
                tracing::info!("性能基准结束，覆盖窗保持打开（等待外部关闭）");
                return;
            }
            let _ = window.gpui_handle().update(acx, |_, window, app| {
                view.update(app, |v, _| v.close(window));
            });
        })
        .detach();
}

/// 处理录屏请求：普通路径进入“录屏选区”覆盖窗；设置了自动化环境变量时跳过选区直接开始。
///
/// # 参数
/// - `cx`：GPUI 外壳上下文。
/// - `state`：运行时状态。
fn request_recording(cx: &mut ShellContext, state: &mut AppState) {
    if state.recording.is_busy(cx) {
        tracing::info!("已有录制在进行，忽略录屏请求");
        return;
    }
    let Ok(text) = std::env::var(ENV_RECORDING_AUTOTEST) else {
        request_capture(cx, state, ORIGIN_RECORDING, CaptureMode::Record);
        return;
    };
    let Some(spec) = parse_autotest(&text) else {
        tracing::error!(value = %text, env = ENV_RECORDING_AUTOTEST, "自动化录屏参数格式无效");
        return;
    };
    let monitors = match cx.monitors() {
        Ok(m) => m,
        Err(e) => {
            tracing::error!(error = %e, "枚举显示器失败，无法录屏");
            return;
        }
    };
    let Some(monitor) = monitor_for_region(monitors.all(), spec.region).cloned() else {
        tracing::error!("系统没有可用显示器，无法录屏");
        return;
    };
    tracing::info!(region = ?spec.region, plan = ?spec.plan, "自动化录屏（跳过选区）");
    state
        .recording
        .begin(cx, spec.region, &monitor, Some(&spec));
}

/// 处理长截图请求：普通路径进入“长截图选区”覆盖窗；设置了自动化环境变量时跳过选区直接开始。
///
/// # 参数
/// - `cx`：GPUI 外壳上下文。
/// - `state`：运行时状态。
fn request_scroll_capture(cx: &mut ShellContext, state: &mut AppState) {
    if state.scroll.is_busy(cx) {
        tracing::info!("已有长截图在进行，忽略请求");
        return;
    }
    let Ok(text) = std::env::var(ENV_SCROLL_AUTOTEST) else {
        request_capture(cx, state, ORIGIN_SCROLL, CaptureMode::Scroll);
        return;
    };
    let Some(spec) = parse_scroll_autotest(&text) else {
        tracing::error!(value = %text, env = ENV_SCROLL_AUTOTEST, "自动化长截图参数格式无效");
        return;
    };
    let monitors = match cx.monitors() {
        Ok(m) => m,
        Err(e) => {
            tracing::error!(error = %e, "枚举显示器失败，无法长截图");
            return;
        }
    };
    let Some(monitor) = monitor_for_region(monitors.all(), spec.region).cloned() else {
        tracing::error!("系统没有可用显示器，无法长截图");
        return;
    };
    tracing::info!(region = ?spec.region, auto = spec.auto_scroll, stop_after = ?spec.stop_after_secs, "自动化长截图（跳过选区）");
    state.scroll.begin(cx, spec.region, &monitor, Some(&spec));
}

/// 打开设置窗口；已打开则激活到前台。
fn open_or_focus_settings(cx: &mut ShellContext, state: &mut AppState) {
    if let Some(window) = state.settings
        && cx.is_window_open(&window)
    {
        cx.activate_window(&window);
        tracing::info!("settings window activated");
        return;
    }
    let system = SystemPrefs::query();
    let title = {
        let store = state.config.borrow();
        let text = |key: &str| store.value(key).as_str().unwrap_or_default().to_string();
        window_title(Lang::from_config(&text(LANGUAGE_KEY), &system.language))
    };
    let mut spec = WindowSpec::normal(
        title,
        LogicalSize::new(SETTINGS_WINDOW_WIDTH, SETTINGS_WINDOW_HEIGHT),
    );
    if let Some(target) = settings_monitor_from_env(cx) {
        spec.placement = Placement::Centered {
            monitor: target,
            size: LogicalSize::new(SETTINGS_WINDOW_WIDTH, SETTINGS_WINDOW_HEIGHT),
        };
    }
    let config = Rc::clone(&state.config);
    let inbox = state.inbox.clone();
    let data_root = state.data_root.clone();
    match cx.open_window(&spec, move |window, app| {
        build_settings_view(window, app, config, system, inbox, data_root)
    }) {
        Ok((window, view)) => {
            state.settings = Some(window);
            state.settings_view = Some(view.clone());
            apply_chrome_theme(state);
            tracing::info!("settings window opened");
            if let Ok(path) = std::env::var(ENV_SETTINGS_AUTOTEST) {
                spawn_settings_autotest(cx, window, view, &path);
            }
        }
        Err(e) => tracing::error!(error = %e, "打开设置窗口失败"),
    }
}

/// 创建设置页视图并接好配置变更 / 语音模型 / 导入导出 / 更新检查回调（独立设置窗口与主窗口内嵌共用）。
///
/// # 参数
/// - `window`：视图所在窗口。
/// - `app`：应用上下文。
/// - `config`：共享配置。
/// - `system`：系统偏好快照。
/// - `inbox`：主线程收件箱（各回调经它回到主线程）。
/// - `data_root`：数据根目录（语音模型下载位置）。
fn build_settings_view(
    window: &mut snow_ui::ui::Window,
    app: &mut snow_ui::ui::App,
    config: SharedConfig,
    system: SystemPrefs,
    inbox: MainThreadInbox<UiEvent>,
    data_root: PathBuf,
) -> Entity<SettingsView> {
    let notify_inbox = inbox.clone();
    let notify: Rc<dyn Fn(ConfigChange)> = Rc::new(move |change: ConfigChange| {
        notify_inbox.push(UiEvent::ConfigChanged {
            key: change.key.to_string(),
            previous: change.previous,
        });
    });
    let view = SettingsView::create(window, app, config, system, notify);
    let stt_inbox = inbox.clone();
    let mcp_data_root = data_root.clone();
    let stt_hooks = SttHooks {
        data_root,
        request: std::sync::Arc::new(move |model_id, cancel| {
            stt_inbox.push(UiEvent::SttDownloadRequested { model_id, cancel });
        }),
    };
    let transfer_inbox = inbox.clone();
    let update_inbox = inbox;
    let mcp_descriptor = snow_mcp::descriptor::descriptor_path(&mcp_data_root);
    view.update(app, |v, _| {
        v.set_mcp_hook(Rc::new(move || {
            let text = crate::mcp_settings::client_config_json(&mcp_descriptor);
            snow_platform::clipboard::copy_text_to_clipboard(&text)
        }));
        v.set_stt_hooks(stt_hooks);
        v.set_transfer_hook(Rc::new(move |action| {
            transfer_inbox.push(UiEvent::ConfigTransferRequested(action));
        }));
        v.set_update_hook(Rc::new(move |action| {
            update_inbox.push(UiEvent::UpdateActionRequested(action));
        }));
    });
    view
}

/// 当前存活的设置页视图：独立设置窗口与主窗口内嵌的各一份，设置结果要同步给所有可见的设置页。
///
/// # 参数
/// - `state`：运行时状态。
/// - `app`：应用上下文（读取主窗口里内嵌的视图）。
fn settings_views(state: &AppState, app: &snow_ui::ui::App) -> Vec<Entity<SettingsView>> {
    let mut views: Vec<Entity<SettingsView>> = state.settings_view.iter().cloned().collect();
    if let Some((_, main)) = &state.main_window
        && let Some(embedded) = main.read(app).settings_view()
    {
        views.push(embedded);
    }
    views
}

/// 打开主窗口；已打开则激活到前台。窗口关闭即释放，不做后台常驻。
///
/// # 参数
/// - `cx`：外壳上下文。
/// - `state`：运行时状态。
fn open_or_focus_main_window(cx: &mut ShellContext, state: &mut AppState) {
    if let Some((window, _)) = &state.main_window
        && cx.is_window_open(window)
    {
        cx.activate_window(window);
        return;
    }
    let prefs = ui_prefs_from_config(&state.config);
    let mut spec = WindowSpec::normal(
        PRODUCT_NAME.to_string(),
        LogicalSize::new(
            crate::main_window_view::WINDOW_WIDTH,
            crate::main_window_view::WINDOW_HEIGHT,
        ),
    );
    // 恢复上次的位置与大小（修正到当前屏幕内）；没有记忆就保持居中
    let saved = parse_geometry(&state.config.borrow().value(MAIN_WINDOW_GEOMETRY_KEY));
    if let Some(saved) = saved
        && let Ok(monitors) = cx.monitors()
    {
        let mut areas: Vec<PhysicalRect> = monitors.all().iter().map(|m| m.work_area).collect();
        // 主屏排最前（修正落点时以第一块为准）
        if let Some(ix) = monitors.all().iter().position(|m| m.is_primary) {
            areas.swap(0, ix);
        }
        spec.placement = Placement::Physical(fit_geometry(
            saved.rect,
            (MAIN_MIN_WIDTH, MAIN_MIN_HEIGHT),
            &areas,
        ));
    }
    let maximized = saved.is_some_and(|g| g.maximized);
    let config = Rc::clone(&state.config);
    let inbox = state.inbox.clone();
    let translate_factory = translate_page_factory(state, true);
    let factory: crate::main_window_view::SettingsFactory = {
        let config = Rc::clone(&state.config);
        let inbox = state.inbox.clone();
        let data_root = state.data_root.clone();
        Rc::new(move |window, app| {
            build_settings_view(
                window,
                app,
                Rc::clone(&config),
                SystemPrefs::query(),
                inbox.clone(),
                data_root.clone(),
            )
        })
    };
    match cx.open_window(&spec, move |window, app| {
        MainWindowView::create(
            window,
            app,
            &config,
            prefs,
            inbox,
            factory,
            translate_factory,
            maximized,
        )
    }) {
        Ok((window, view)) => {
            state.main_window = Some((window, view));
            apply_chrome_theme(state);
            tracing::info!("主窗口已打开");
        }
        Err(e) => tracing::error!(error = %e, "打开主窗口失败"),
    }
}

/// 窗口外框在最小化时会被系统挪到这个坐标以下，此时的几何不能记。
const MINIMIZED_COORDINATE: i32 = -30000;

/// 把一个配置值写回并落盘；与当前值相同则不写。失败只记日志。
///
/// # 参数
/// - `state`：运行时状态。
/// - `key`：配置键。
/// - `value`：新值。
/// - `what`：日志里的说明。
fn save_config_value(state: &AppState, key: &str, value: Value, what: &str) {
    let mut store = state.config.borrow_mut();
    if store.value(key) == value {
        return;
    }
    if let Err(e) = store.set_value(key, value) {
        tracing::warn!(error = %e, "写入{what}失败");
    } else if let Err(e) = store.flush() {
        tracing::warn!(error = %e, "{what}落盘失败");
    }
}

/// 把更新检查 / 下载的新状态同步给所有显示它的界面：各份设置页与主窗口的关于页。
///
/// # 参数
/// - `state`：运行时状态。
/// - `cx`：外壳上下文。
/// - `ui_state`：新的界面状态。
fn publish_update_state(
    state: &AppState,
    cx: &mut ShellContext,
    ui_state: crate::net_settings::UpdateUiState,
) {
    for view in settings_views(state, cx.app()) {
        let ui_state = ui_state.clone();
        view.update(cx.app(), |v, vcx| v.finish_update_check(ui_state, vcx));
    }
    if let Some((_, main)) = &state.main_window {
        main.update(cx.app(), |v, vcx| v.finish_update_check(ui_state, vcx));
    }
}

/// 记忆主窗口位置与大小：最大化时只更新标记并保留普通态外框，最小化时不记。
///
/// # 参数
/// - `state`：运行时状态。
/// - `maximized`：此刻是否最大化。
fn save_main_window_geometry(state: &AppState, maximized: bool) {
    let Some((window, _)) = &state.main_window else {
        return;
    };
    let previous = parse_geometry(&state.config.borrow().value(MAIN_WINDOW_GEOMETRY_KEY));
    let rect = if maximized {
        previous.map(|g| g.rect)
    } else {
        window.rect().ok().filter(|r| r.x > MINIMIZED_COORDINATE)
    };
    if let Some(rect) = rect {
        save_config_value(
            state,
            MAIN_WINDOW_GEOMETRY_KEY,
            geometry_to_json(rect, maximized),
            "主窗口位置",
        );
    }
}

/// 记忆独立翻译窗口的逻辑大小（外框物理像素除以缩放比）。
///
/// # 参数
/// - `state`：运行时状态。
/// - `scale`：窗口缩放比。
fn save_translate_window_size(state: &AppState, scale: f32) {
    let Some((window, _)) = &state.translate_page else {
        return;
    };
    let Some(rect) = window.rect().ok().filter(|r| r.x > MINIMIZED_COORDINATE) else {
        return;
    };
    if scale <= 0.0 {
        return;
    }
    let width = (rect.width as f32 / scale).round() as i32;
    let height = (rect.height as f32 / scale).round() as i32;
    save_config_value(
        state,
        TRANSLATION_WINDOW_SIZE_KEY,
        size_to_json(width, height),
        "翻译窗口大小",
    );
}

/// 生成翻译页的创建工厂：每次调用时现读配置与已装包，历史落在数据根目录。
///
/// # 参数
/// - `state`：运行时状态。
/// - `embedded`：是否内嵌在主窗口里。
fn translate_page_factory(
    state: &AppState,
    embedded: bool,
) -> crate::main_window_view::TranslateFactory {
    let config = Rc::clone(&state.config);
    let inbox = state.inbox.clone();
    let translator = Arc::clone(&state.translator);
    let data_root = state.data_root.clone();
    Rc::new(move |window, app| {
        let (translate_config, packs, auto) = {
            let store = config.borrow();
            let translate_config =
                TranslateConfig::from_document(store.document(), &system_ui_language());
            let packs =
                crate::translate_input::installed_packs(&translator.scan(&translate_config).models);
            let auto = store
                .value(KEY_PAGE_AUTO_TRANSLATE)
                .as_bool()
                .unwrap_or(false);
            (translate_config, packs, auto)
        };
        let prefs = ui_prefs_from_config(&config);
        let options = PageOptions {
            embedded,
            auto_translate: auto,
            history_path: Some(history_path(&data_root)),
        };
        TranslatePageView::create(
            window,
            app,
            &translate_config,
            packs,
            prefs,
            inbox.clone(),
            options,
        )
    })
}

/// 把主窗口侧栏折叠状态写回配置并落盘；失败只记日志。
///
/// # 参数
/// - `state`：运行时状态。
/// - `collapsed`：新的折叠状态。
fn save_sidebar_collapsed(state: &AppState, collapsed: bool) {
    let mut store = state.config.borrow_mut();
    if let Err(e) = store.set_value(SIDEBAR_COLLAPSED_KEY, serde_json::json!(collapsed)) {
        tracing::warn!(error = %e, "写入侧栏折叠状态失败");
    } else if let Err(e) = store.flush() {
        tracing::warn!(error = %e, "侧栏折叠状态落盘失败");
    }
}

/// 打开截图历史窗口；已打开则激活到前台。
///
/// # 参数
/// - `cx`：外壳上下文。
/// - `state`：运行时状态。
fn open_or_focus_history(cx: &mut ShellContext, state: &mut AppState) {
    if let Some((window, _)) = &state.history_window
        && cx.is_window_open(window)
    {
        cx.activate_window(window);
        return;
    }
    let prefs = ui_prefs_from_config(&state.config);
    let policy = policy_from_document(state.config.borrow().document());
    let enabled = policy.enabled;
    let store = HistoryStore::new(&state.data_root, policy);
    let title = crate::ocr_backend::i18n_for(prefs.locale).tr("history-window-title");
    let spec = WindowSpec::normal(
        title,
        LogicalSize::new(
            crate::history_view::WINDOW_WIDTH,
            crate::history_view::WINDOW_HEIGHT,
        ),
    );
    let inbox = state.inbox.clone();
    match cx.open_window(&spec, move |window, app| {
        HistoryView::create(window, app, store, enabled, prefs, inbox)
    }) {
        Ok((window, view)) => {
            state.history_window = Some((window, view));
            apply_chrome_theme(state);
            tracing::info!("截图历史窗口已打开");
        }
        Err(e) => tracing::error!(error = %e, "打开截图历史窗口失败"),
    }
}

/// 打开贴图管理窗口；已打开则只激活。
///
/// # 参数
/// - `cx`：外壳上下文。
/// - `state`：运行时状态。
fn open_or_focus_pin_manage(cx: &mut ShellContext, state: &mut AppState) {
    if let Some((window, _)) = &state.pin_manage_window
        && cx.is_window_open(window)
    {
        cx.activate_window(window);
        return;
    }
    let prefs = ui_prefs_from_config(&state.config);
    let title = crate::ocr_backend::i18n_for(prefs.locale).tr("pinmgr-window-title");
    let spec = WindowSpec::normal(
        title,
        LogicalSize::new(
            crate::pinned_manage_view::WINDOW_WIDTH,
            crate::pinned_manage_view::WINDOW_HEIGHT,
        ),
    );
    let shared = Rc::clone(state.pins.shared());
    let open_ids = state.pins.open_ids();
    let inbox = state.inbox.clone();
    match cx.open_window(&spec, move |window, app| {
        PinManageView::create(window, app, shared, open_ids, prefs, inbox)
    }) {
        Ok((window, view)) => {
            state.pin_manage_window = Some((window, view));
            apply_chrome_theme(state);
            tracing::info!("贴图管理窗口已打开");
        }
        Err(e) => tracing::error!(error = %e, "打开贴图管理窗口失败"),
    }
}

/// 启动全局鼠标手势服务（至少有一条绑定时才装钩子）；失败只记日志。
///
/// # 参数
/// - `state`：运行时状态（取配置与收件箱，保存服务句柄）。
pub fn start_mouse_gesture(state: &mut AppState) {
    let bindings = crate::mouse_gesture::bindings_from_document(state.config.borrow().document());
    if bindings.is_empty() {
        tracing::info!("没有配置鼠标手势，不安装全局钩子");
        return;
    }
    let inbox = state.inbox.clone();
    match snow_platform::global_mouse::GlobalMouseService::start(Box::new(move |event| {
        inbox.push(UiEvent::MouseGesture(event));
    })) {
        Ok(service) => {
            tracing::info!(bindings = bindings.len(), "全局鼠标手势已启动");
            service.set_bindings(bindings);
            state.mouse_service = Some(service);
        }
        Err(e) => tracing::warn!(error = %e, "启动全局鼠标手势失败"),
    }
}

/// 处理鼠标手势事件：开始时发起截图；覆盖窗就绪前的位置先记下，就绪后补发；松开时完成框选。
///
/// # 参数
/// - `cx`：外壳上下文。
/// - `state`：运行时状态。
/// - `event`：拖动事件。
fn on_mouse_gesture(
    cx: &mut ShellContext,
    state: &mut AppState,
    event: snow_platform::global_mouse::DragEvent,
) {
    use snow_platform::global_mouse::DragEvent;
    match event {
        DragEvent::Begin { id, action, pos } => {
            let Some(mode) = crate::mouse_gesture::mode_for_action(&action) else {
                tracing::warn!(%action, "未知的鼠标手势动作");
                return;
            };
            if state.capture_in_flight || any_overlay_open(cx, state) || state.recording.is_busy(cx)
            {
                tracing::info!(%action, "已有截图 / 录制在进行，忽略鼠标手势");
                if let Some(service) = &state.mouse_service {
                    service.cancel();
                }
                return;
            }
            state.gesture = Some(GestureSession {
                id,
                start: pos,
                latest: pos,
                finished: false,
                applied: false,
            });
            request_capture(cx, state, ORIGIN_GESTURE, mode);
        }
        DragEvent::Update { id, pos } => {
            if let Some(session) = state.gesture.as_mut().filter(|s| s.id == id) {
                session.latest = pos;
                if session.applied {
                    drive_gesture(
                        cx,
                        state,
                        snow_platform::global_mouse::Point { x: pos.x, y: pos.y },
                        crate::overlay_view::GestureStep::Move,
                    );
                }
            }
        }
        DragEvent::Finish { id, pos } => {
            let Some(session) = state.gesture.as_mut().filter(|s| s.id == id) else {
                return;
            };
            session.latest = pos;
            session.finished = true;
            if session.applied {
                state.gesture = None;
                drive_gesture(cx, state, pos, crate::overlay_view::GestureStep::Up);
            }
        }
        DragEvent::Cancel { id } => {
            if state.gesture.as_ref().is_some_and(|s| s.id == id) {
                state.gesture = None;
            }
        }
    }
}

/// 把一步手势交给覆盖窗；没有覆盖窗时忽略。
///
/// # 参数
/// - `cx`：外壳上下文。
/// - `state`：运行时状态。
/// - `pos`：鼠标位置（虚拟桌面物理像素）。
/// - `step`：这一步是按下、移动还是松开。
fn drive_gesture(
    cx: &mut ShellContext,
    state: &AppState,
    pos: snow_platform::global_mouse::Point,
    step: crate::overlay_view::GestureStep,
) {
    let (Some(window), Some(view)) = (state.overlay.as_ref(), state.overlay_view.clone()) else {
        return;
    };
    let point = PhysicalPoint::new(pos.x, pos.y);
    let _ = window.gpui_handle().update(cx.app(), |_, window, app| {
        view.update(app, |v, vcx| v.drive_gesture(step, point, window, vcx));
    });
}

/// 覆盖窗刚打开时，把手势已经发生的按下与移动补发给它；鼠标键已松开的话顺带完成框选。
///
/// # 参数
/// - `cx`：外壳上下文。
/// - `state`：运行时状态。
fn apply_pending_gesture(cx: &mut ShellContext, state: &mut AppState) {
    let Some(session) = state.gesture.as_mut().filter(|s| !s.applied) else {
        return;
    };
    session.applied = true;
    let (start, latest, finished) = (session.start, session.latest, session.finished);
    if finished {
        state.gesture = None;
    }
    use crate::overlay_view::GestureStep;
    drive_gesture(cx, state, start, GestureStep::Down);
    drive_gesture(cx, state, latest, GestureStep::Move);
    if finished {
        drive_gesture(cx, state, latest, GestureStep::Up);
    }
}

/// 打开文字识别结果窗（每次新开一个，窗口关闭即释放）。
///
/// # 参数
/// - `cx`：外壳上下文。
/// - `state`：运行时状态（取界面偏好）。
/// - `data`：识别结果数据。
fn open_recognition_window(
    cx: &mut ShellContext,
    state: &mut AppState,
    data: crate::recognition_view::RecognitionData,
) {
    use crate::recognition_view::{RecognitionView, WINDOW_HEIGHT, WINDOW_WIDTH};
    let mut data = data;
    data.conversion =
        crate::conversion_guide::ConversionGuide::from_document(state.config.borrow().document());
    let prefs = ui_prefs_from_config(&state.config);
    let title = crate::ocr_backend::i18n_for(prefs.locale).tr("recwin-window-title");
    let spec = WindowSpec::normal(title, LogicalSize::new(WINDOW_WIDTH, WINDOW_HEIGHT));
    match cx.open_window(&spec, move |window, app| {
        RecognitionView::create(window, app, data, prefs)
    }) {
        Ok(_) => {
            apply_chrome_theme(state);
            tracing::info!("识别结果窗已打开");
        }
        Err(e) => tracing::error!(error = %e, "打开识别结果窗失败"),
    }
}

/// 贴图 / 分组 / 窗口状态变化后，刷新已打开的管理窗口。
///
/// # 参数
/// - `cx`：外壳上下文。
/// - `state`：运行时状态。
fn refresh_pin_manage(cx: &mut ShellContext, state: &AppState) {
    if let Some((_, view)) = &state.pin_manage_window {
        let open_ids = state.pins.open_ids();
        view.update(cx.app(), |v, cx| v.refresh(open_ids, cx));
    }
}

/// 把操作结果交给管理窗口显示错误（成功则什么也不做）。
///
/// # 参数
/// - `cx`：外壳上下文。
/// - `state`：运行时状态。
/// - `error`：失败原因。
fn report_pin_manage_error(cx: &mut ShellContext, state: &AppState, error: PinError) {
    if let Some((_, view)) = &state.pin_manage_window {
        let message = error.message(crate::ocr_backend::i18n_for(
            ui_prefs_from_document(state.config.borrow().document()).locale,
        ));
        view.update(cx.app(), |v, cx| v.show_error(message, cx));
    }
}

/// 新建分组默认名所用的界面语料。
///
/// # 参数
/// - `state`：运行时状态。
fn pin_group_i18n(state: &AppState) -> &'static snow_i18n::I18n {
    crate::ocr_backend::i18n_for(ui_prefs_from_document(state.config.borrow().document()).locale)
}

/// 光标所在显示器作为输入框翻译浮窗的落点；取不到光标或显示器时用主屏。
///
/// # 参数
/// - `cx`：外壳上下文。
fn translate_input_monitor(cx: &ShellContext) -> MonitorTarget {
    let Ok(monitors) = cx.monitors() else {
        return MonitorTarget::Primary;
    };
    pick_monitor(&monitors, cursor_screen_position().ok())
        .map_or(MonitorTarget::Primary, |m| MonitorTarget::Id(m.id))
}

/// 读取界面偏好（深浅色、语言、主色），与设置页同一套解析。
///
/// # 参数
/// - `config`：共享配置。
pub(crate) fn ui_prefs_from_config(config: &SharedConfig) -> UiPrefs {
    let store = config.borrow();
    let text = |key: &str| store.value(key).as_str().unwrap_or_default().to_string();
    UiPrefs::resolve(
        &text(THEME_MODE_KEY),
        &text(LANGUAGE_KEY),
        &text(THEME_COLOR_KEY),
        &SystemPrefs::query(),
    )
}

/// 打开输入框翻译浮窗；已打开则只激活，不重复创建。
///
/// # 参数
/// - `cx`：外壳上下文。
/// - `state`：运行时状态。
fn open_or_focus_translate_input(cx: &mut ShellContext, state: &mut AppState) {
    if let Some((window, _)) = &state.translate_input
        && cx.is_window_open(window)
    {
        cx.activate_window(window);
        return;
    }
    let prefs = ui_prefs_from_config(&state.config);
    let packs = {
        let config = state.config.borrow();
        let translate_config =
            TranslateConfig::from_document(config.document(), &system_ui_language());
        crate::translate_input::installed_packs(&state.translator.scan(&translate_config).models)
    };
    let size = LogicalSize::new(TRANSLATE_INPUT_WIDTH, TRANSLATE_INPUT_HEIGHT);
    let spec = WindowSpec {
        title: String::new(),
        placement: Placement::Centered {
            monitor: translate_input_monitor(cx),
            size,
        },
        transparent: false,
        always_on_top: true,
        decorations: false,
        show_in_taskbar: false,
        focus: true,
        resizable: false,
    };
    let inbox = state.inbox.clone();
    match cx.open_window(&spec, move |window, app| {
        TranslateInputView::create(window, app, packs, prefs, inbox)
    }) {
        Ok((window, view)) => {
            state.translate_input = Some((window, view));
            tracing::info!("输入框翻译窗口已打开");
        }
        Err(e) => tracing::error!(error = %e, "打开输入框翻译窗口失败"),
    }
}

/// 在后台线程翻译输入框里的文本，结果经收件箱回到主线程。
///
/// # 参数
/// - `state`：运行时状态。
/// - `serial`：请求序号。
/// - `text`：原文。
/// - `model_id`：下拉选中的包 ID（空串为自动）。
fn spawn_translate_input(state: &AppState, serial: u64, text: String, model_id: String) {
    let config =
        TranslateConfig::from_document(state.config.borrow().document(), &system_ui_language());
    let translator = Arc::clone(&state.translator);
    let inbox = state.inbox.clone();
    let spawned = std::thread::Builder::new()
        .name("snow-translate-input".into())
        .spawn(move || {
            let result = translate_text(translator.as_ref(), &config, &model_id, &text);
            inbox.push(UiEvent::TranslateInputFinished { serial, result });
        });
    if let Err(e) = spawned {
        tracing::error!(error = %e, "无法创建输入框翻译线程");
        state.inbox.push(UiEvent::TranslateInputFinished {
            serial,
            result: Err(InputError::Translate(snow_translate::TranslateError::Io(
                e.to_string(),
            ))),
        });
    }
}

/// 打开翻译页窗口；已打开则只激活，不重复创建。供主窗口导航等入口通过
/// [`UiEvent::OpenTranslatePage`] 调用。
///
/// # 参数
/// - `cx`：外壳上下文。
/// - `state`：运行时状态。
fn open_or_focus_translate_page(cx: &mut ShellContext, state: &mut AppState) {
    if let Some((window, _)) = &state.translate_page
        && cx.is_window_open(window)
    {
        cx.activate_window(window);
        return;
    }
    let prefs = ui_prefs_from_config(&state.config);
    let title = crate::ocr_backend::i18n_for(prefs.locale).tr("translate-page-title");
    // 上次记住的窗口大小（逻辑像素），没有就用默认尺寸
    let saved = parse_window_size(&state.config.borrow().value(TRANSLATION_WINDOW_SIZE_KEY));
    let (width, height) = clamp_size(
        saved.unwrap_or((TRANSLATE_PAGE_WIDTH as i32, TRANSLATE_PAGE_HEIGHT as i32)),
        (TRANSLATE_MIN_WIDTH, TRANSLATE_MIN_HEIGHT),
        (i32::MAX, i32::MAX),
    );
    let spec = WindowSpec::normal(title, LogicalSize::new(width as f32, height as f32));
    let factory = translate_page_factory(state, false);
    match cx.open_window(&spec, move |window, app| factory(window, app)) {
        Ok((window, view)) => {
            state.translate_page = Some((window, view));
            tracing::info!("翻译页窗口已打开");
        }
        Err(e) => tracing::error!(error = %e, "打开翻译页窗口失败"),
    }
}

/// 在后台线程执行翻译页的请求，结果经收件箱回到主线程。
///
/// # 参数
/// - `state`：运行时状态。
/// - `serial`：请求序号。
/// - `text`：原文。
/// - `model_id`：下拉选中的包 ID（空串为自动）。
/// - `source` / `target`：页面上选的语言。
/// - `embedded`：请求是否来自主窗口内嵌的翻译页（结果回给同一份）。
fn spawn_translate_page(
    state: &AppState,
    serial: u64,
    text: String,
    model_id: String,
    source: snow_translate::Lang,
    target: snow_translate::Lang,
    embedded: bool,
) {
    let base =
        TranslateConfig::from_document(state.config.borrow().document(), &system_ui_language());
    let config = page_config(&base, source, target);
    let translator = Arc::clone(&state.translator);
    let inbox = state.inbox.clone();
    let spawned = std::thread::Builder::new()
        .name("snow-translate-page".into())
        .spawn(move || {
            let result = translate_text(translator.as_ref(), &config, &model_id, &text);
            inbox.push(UiEvent::TranslatePageFinished {
                serial,
                result,
                embedded,
            });
        });
    if let Err(e) = spawned {
        tracing::error!(error = %e, "无法创建翻译页翻译线程");
        state.inbox.push(UiEvent::TranslatePageFinished {
            serial,
            result: Err(InputError::Translate(snow_translate::TranslateError::Io(
                e.to_string(),
            ))),
            embedded,
        });
    }
}

/// 读取环境变量 `SNOW_SETTINGS_MONITOR`（设备名子串，如 `DISPLAY2`），选择设置窗所在显示器。
///
/// # 参数
/// - `cx`：外壳上下文。
///
/// # 返回
/// 匹配到的显示器；未设置或未匹配返回 `None`（用默认主屏）。
fn settings_monitor_from_env(cx: &ShellContext) -> Option<MonitorTarget> {
    let wanted = std::env::var(ENV_SETTINGS_MONITOR).ok()?;
    let monitors = cx.monitors().ok()?;
    let found = monitors
        .all()
        .iter()
        .find(|m| {
            m.name
                .to_ascii_uppercase()
                .contains(&wanted.to_ascii_uppercase())
        })
        .map(|m| MonitorTarget::Id(m.id));
    if found.is_none() {
        tracing::warn!(wanted, "未找到指定的设置窗显示器，使用默认显示器");
    }
    found
}

/// 按 JSON 文件里的操作序列驱动设置页（验收用，走与点击相同的状态入口）。
///
/// # 参数
/// - `cx`：外壳上下文。
/// - `window`：设置窗口。
/// - `view`：设置页视图。
/// - `path`：操作 JSON 文件路径。
fn spawn_settings_autotest(
    cx: &mut ShellContext,
    window: ShellWindow,
    view: Entity<SettingsView>,
    path: &str,
) {
    let ops = match std::fs::read_to_string(path)
        .map_err(|e| e.to_string())
        .and_then(|text| parse_autotest_ops(&text))
    {
        Ok(ops) => ops,
        Err(e) => {
            tracing::error!(path, error = %e, "设置页自动化脚本无效");
            return;
        }
    };
    tracing::info!(count = ops.len(), "设置页自动化开始");
    cx.app()
        .spawn(async move |acx| {
            for op in ops {
                acx.background_executor()
                    .timer(AUTOTEST_STEP_INTERVAL)
                    .await;
                let ran = window.gpui_handle().update(acx, |_, window, app| {
                    view.update(app, |v, cx| v.run_autotest_op(&op, window, cx));
                });
                if ran.is_err() {
                    tracing::info!("设置窗已关闭，自动化提前结束");
                    return;
                }
            }
            tracing::info!("设置页自动化结束");
        })
        .detach();
}

/// 语音转文字的两个热键配置键（触发模式变更会影响它们是否注册）。
const DICTATION_HOTKEY_KEYS: [&str; 2] = [
    DICTATION_TOGGLE_HOTKEY_CONFIG_KEY,
    DICTATION_HOLD_HOTKEY_CONFIG_KEY,
];

/// 这次重新注册里，与变更的配置键相关的热键是否有失败；
/// 触发模式键本身不是热键，它的失败看两个语音热键。
///
/// # 参数
/// - `attempt`：注册结果。
/// - `config_key`：变更的配置键。
fn hotkey_attempt_failed(attempt: &HotkeyRegistration, config_key: &str) -> bool {
    if config_key == DICTATION_TRIGGER_MODE_CONFIG_KEY {
        DICTATION_HOTKEY_KEYS.iter().any(|k| attempt.failed_for(k))
    } else {
        attempt.failed_for(config_key)
    }
}

/// 汇总与变更的配置键相关的热键失败原因（触发模式键汇总两个语音热键）。
///
/// # 参数
/// - `attempt`：注册结果。
/// - `config_key`：变更的配置键。
fn describe_hotkey_failure(attempt: &HotkeyRegistration, config_key: &str) -> String {
    if config_key == DICTATION_TRIGGER_MODE_CONFIG_KEY {
        DICTATION_HOTKEY_KEYS
            .iter()
            .map(|k| attempt.describe_for(k))
            .filter(|text| !text.is_empty())
            .collect::<Vec<_>>()
            .join("; ")
    } else {
        attempt.describe_for(config_key)
    }
}

/// 托盘分组块的 `(分组 ID, 显示名)` 列表：默认分组显示本地化名称。
///
/// # 参数
/// - `state`：运行时状态。
/// - `locale`：界面语料语言代码。
fn tray_group_labels(state: &AppState, locale: &str) -> Vec<(String, String)> {
    let i18n = crate::ocr_backend::i18n_for(locale);
    state
        .pins
        .shared()
        .groups()
        .into_iter()
        .map(|g| {
            let name = if g.built_in {
                i18n.tr("tray-group-default")
            } else {
                g.name
            };
            (g.id, name)
        })
        .collect()
}

/// 按当前界面语言重建托盘菜单文案。
///
/// # 参数
/// - `state`：运行时状态。
fn refresh_tray_menu(state: &AppState) {
    let Some(tray) = state.tray.as_ref() else {
        return;
    };
    let locale = ui_prefs_from_document(state.config.borrow().document()).locale;
    let groups = tray_group_labels(state, locale);
    let built = build_tray_spec_with_groups(
        locale,
        state.config.borrow().document(),
        state.hotkeys_paused,
        &groups,
        &state.pins.shared().active_group_id(),
    );
    match built.and_then(|spec| tray.set_menu(spec.menu).map_err(|e| e.to_string())) {
        Ok(()) => tracing::info!(locale, "托盘菜单已刷新"),
        Err(e) => tracing::warn!(error = %e, "刷新托盘菜单失败"),
    }
}

/// 判断配置键是否属于会影响热键注册的键，并返回其 `'static` 形式。
///
/// # 参数
/// - `key`：配置键。
///
/// ```ignore
/// assert!(hotkey_config_key("global_shortcuts/open_settings").is_some());
/// ```
fn hotkey_config_key(key: &str) -> Option<&'static str> {
    [
        SCREENSHOT_HOTKEY_CONFIG_KEY,
        RECORDING_HOTKEY_CONFIG_KEY,
        PIN_CLIPBOARD_HOTKEY_CONFIG_KEY,
        TRANSLATE_INPUT_HOTKEY_CONFIG_KEY,
        DICTATION_TOGGLE_HOTKEY_CONFIG_KEY,
        DICTATION_HOLD_HOTKEY_CONFIG_KEY,
        DICTATION_TRIGGER_MODE_CONFIG_KEY,
    ]
    .into_iter()
    .chain(QUICK_ACTION_KEYS.iter().map(|(k, _)| *k))
    .find(|k| *k == key)
}

/// 主线程处理一个 MCP 请求（枚举显示器 / 写设置），回复后把配置变更广播给运行时。
///
/// # 参数
/// - `cx`：外壳上下文。
/// - `state`：运行时状态。
/// - `request`：MCP 请求。
fn handle_mcp_request(
    cx: &mut ShellContext,
    state: &mut AppState,
    request: &crate::mcp_host::McpRequest,
) {
    let (reply, changed) = crate::mcp_host::handle_request(
        request,
        || cx.monitors().map(|m| m.all().to_vec()),
        &state.config,
    );
    request.respond(reply);
    for (key, previous) in changed {
        state.inbox.push(UiEvent::ConfigChanged { key, previous });
    }
}

/// 设置页写入配置后的响应：全局热键类配置变更时重新注册并在失败时回滚。
///
/// # 参数
/// - `cx`：外壳上下文。
/// - `state`：运行时状态。
/// - `key`：变更的配置键。
/// - `previous`：变更前的值。
fn on_config_changed(cx: &mut ShellContext, state: &mut AppState, key: &str, previous: Value) {
    if key == crate::mcp_host::KEY_ENABLED || state.mcp.is_running() {
        state.sync_mcp();
    }
    if key == TRANSLATION_PAGE_ENABLED_KEY {
        let enabled = state.config.borrow().value(key).as_bool().unwrap_or(false);
        if let Some((_, view)) = &state.main_window {
            view.update(cx.app(), |v, cx| v.set_translation_enabled(enabled, cx));
        }
        return;
    }
    if key == KEY_PAGE_AUTO_TRANSLATE {
        let enabled = state.config.borrow().value(key).as_bool().unwrap_or(false);
        let embedded = state
            .main_window
            .as_ref()
            .and_then(|(_, main)| main.read(cx.app()).translate_view());
        let standalone = state.translate_page.as_ref().map(|(_, view)| view.clone());
        for view in embedded.into_iter().chain(standalone) {
            view.update(cx.app(), |v, vcx| v.set_auto_translate(enabled, vcx));
        }
        return;
    }
    if key == LANGUAGE_KEY || key == THEME_MODE_KEY {
        let prefs = ui_prefs_from_config(&state.config);
        if let Some((_, view)) = &state.main_window {
            view.update(cx.app(), |v, cx| v.set_prefs(prefs, cx));
        }
        if let Some((_, view)) = &state.translate_page {
            view.update(cx.app(), |v, vcx| v.set_prefs(prefs, vcx));
        }
    }
    if key == LANGUAGE_KEY
        || key == DELAY_SECONDS_CONFIG_KEY
        || key == crate::tray_config::KEY_MENU_OPTIONS
    {
        refresh_tray_menu(state);
        return;
    }
    if key == THEME_MODE_KEY {
        apply_chrome_theme(state);
        return;
    }
    if key == crate::system_settings::KEY_PRIORITY || key == crate::system_settings::KEY_AUTO_START
    {
        crate::system_settings::apply(state.config.borrow().document());
        return;
    }
    if key == crate::net_settings::KEY_PROXY {
        crate::net_settings::apply_proxy(state.config.borrow().document());
        return;
    }
    let Some(config_key) = hotkey_config_key(key) else {
        if key.starts_with("global_shortcuts/") {
            tracing::info!(key, "该全局快捷键的动作尚未接线，配置已保存但不会注册热键");
        }
        return;
    };
    let Some(service) = state.hotkeys.as_ref() else {
        tracing::warn!(key, "热键服务未运行，无法重新注册");
        return;
    };
    for handle in state.hotkey_handles.drain(..) {
        if let Err(e) = service.unregister(handle) {
            tracing::warn!(error = %e, "注销旧热键失败");
        }
    }
    let attempt = register_all_hotkeys_gated(
        service,
        state.config.borrow().document(),
        state.hotkeys_paused,
    );
    if !hotkey_attempt_failed(&attempt, config_key) {
        let listing = service
            .registered()
            .map(|list| {
                list.iter()
                    .map(|(_, h)| h.to_string())
                    .collect::<Vec<_>>()
                    .join(", ")
            })
            .unwrap_or_default();
        tracing::info!(key, registered = %listing, "全局热键已按新配置重新注册");
        state.hotkey_handles = attempt.handles;
        return;
    }
    // 新绑定注册失败（如系统返回 1409 热键已被占用）：注销本次已注册的、还原配置、恢复旧热键
    let reason = describe_hotkey_failure(&attempt, config_key);
    tracing::warn!(key, reason = %reason, "全局热键重新注册失败，回滚配置");
    for handle in attempt.handles {
        let _ = service.unregister(handle);
    }
    if let Err(e) = restore_value(&state.config, key, previous) {
        tracing::error!(key, error = %e, "回滚配置失败");
    }
    let restored = register_all_hotkeys_gated(
        service,
        state.config.borrow().document(),
        state.hotkeys_paused,
    );
    tracing::info!(
        key,
        registered = restored.handles.len(),
        "已恢复回滚后的全局热键"
    );
    state.hotkey_handles = restored.handles;
    for view in settings_views(state, cx.app()) {
        let message = format!(
            "{}: {reason}",
            crate::settings_text::t(
                view.read(cx.app()).language(),
                crate::settings_text::Text::HotkeyRegisterFailed,
            )
        );
        view.update(cx.app(), |v, cx| v.notify_reverted(config_key, message, cx));
    }
}

/// 把显示器内坐标的选区换算成虚拟桌面坐标（均为物理像素，录制进程 START 直接使用）。
///
/// # 参数
/// - `rect`：以显示器左上角为原点的选区。
/// - `bounds`：该显示器在虚拟桌面中的范围。
///
/// # 返回
/// 虚拟桌面坐标下的选区（宽高不变）。
///
/// # 示例
/// ```ignore
/// let r = monitor_local_to_desktop(PhysicalRect::new(10, 20, 300, 200), PhysicalRect::new(-1920, 0, 1920, 1080));
/// assert_eq!((r.x, r.y), (-1910, 20));
/// ```
fn monitor_local_to_desktop(rect: PhysicalRect, bounds: PhysicalRect) -> PhysicalRect {
    PhysicalRect::new(
        rect.x + bounds.x,
        rect.y + bounds.y,
        rect.width,
        rect.height,
    )
}

/// 托盘悬停提示的最大字符数（系统限制 127 个 UTF-16 单元，留出余量）。
const NOTICE_TOOLTIP_MAX_CHARS: usize = 100;

/// 取当前界面语言下的提示文案。
///
/// # 参数
/// - `state`：运行时状态（读取界面语言）。
/// - `id`：消息 id。
/// - `args`：消息参数。
fn notice_text(state: &AppState, id: &str, args: Args) -> String {
    let locale = ui_prefs_from_document(state.config.borrow().document()).locale;
    crate::ocr_backend::i18n_for(locale).tr_with(id, &args)
}

/// 显示一条轻量提示：写日志，并放到托盘悬停提示里（下次状态变化时被覆盖）。
///
/// # 参数
/// - `state`：运行时状态。
/// - `text`：已本地化的提示文案。
fn show_notice(state: &AppState, text: &str) {
    tracing::info!(notice = %text, "快捷动作提示");
    if let Some(tray) = state.tray.as_ref() {
        let tip: String = format!("{TRAY_TOOLTIP}: {text}")
            .chars()
            .take(NOTICE_TOOLTIP_MAX_CHARS)
            .collect();
        if let Err(e) = tray.set_tooltip(tip) {
            tracing::warn!(error = %e, "更新托盘提示失败");
        }
    }
}

/// 执行一个快捷动作：按 [`plan_for`] 的方案分派，未实现的动作给出本地化提示。
///
/// # 参数
/// - `cx`：外壳上下文。
/// - `state`：运行时状态。
/// - `action`：快捷动作。
fn run_quick_action(cx: &mut ShellContext, state: &mut AppState, action: QuickAction) {
    tracing::info!(?action, "快捷动作触发");
    match plan_for(action) {
        QuickPlan::Direct(kind) => request_direct_capture(cx, state, kind),
        QuickPlan::Overlay(auto) => {
            request_capture(cx, state, ORIGIN_HOTKEY, CaptureMode::Quick(auto))
        }
        QuickPlan::Delayed => begin_delayed_capture(state),
        QuickPlan::OpenSettings => open_or_focus_settings(cx, state),
        QuickPlan::OpenHistory => open_or_focus_history(cx, state),
        QuickPlan::OpenPinManage => open_or_focus_pin_manage(cx, state),
        QuickPlan::PinSelectedFiles => spawn_pin_selected_files(state),
        QuickPlan::TranslateSelected => spawn_selected_text_capture(state),
        QuickPlan::RecordAndCopy => {
            state.record_copy_pending = true;
            request_recording(cx, state);
        }
        QuickPlan::RestoreClosed => match state.pins.restore_last_closed(cx) {
            Ok(Some(id)) => {
                tracing::info!(id = %id, "已恢复最近关闭的贴图");
                refresh_pin_manage(cx, state);
            }
            Ok(None) => {
                let text = notice_text(state, NOTICE_RESTORE_CLOSED, Args::new());
                show_notice(state, &text);
            }
            Err(e) => tracing::warn!(error = %e, "恢复最近关闭的贴图失败"),
        },
        QuickPlan::ToggleHotkeys => toggle_global_hotkeys(state),
        QuickPlan::ToggleFullscreenGate => toggle_fullscreen_gate(state),
        QuickPlan::OpenRecordingFolder => open_recording_folder(state),
        QuickPlan::Placeholder(id) => {
            let text = notice_text(state, id, Args::new());
            show_notice(state, &text);
        }
    }
}

/// 开始延迟截图倒计时：到点后由 [`UiEvent::DelayElapsed`] 触发普通截图。
///
/// 倒计时期间不创建任何窗口，所以不会进入画面；秒数读取 `screenshot/delay_seconds`。
///
/// # 参数
/// - `state`：运行时状态。
fn begin_delayed_capture(state: &mut AppState) {
    let seconds = delay_seconds(state.config.borrow().document());
    let Some(serial) = state.delay.begin() else {
        let text = notice_text(state, "quick-notice-delay-busy", Args::new());
        show_notice(state, &text);
        return;
    };
    let inbox = state.inbox.clone();
    let spawned = std::thread::Builder::new()
        .name("snow-delay-capture".into())
        .spawn(move || {
            std::thread::sleep(Duration::from_secs(seconds));
            inbox.push(UiEvent::DelayElapsed { serial });
        });
    match spawned {
        Ok(_) => {
            tracing::info!(seconds, serial, "延迟截图开始倒计时");
            let text = notice_text(
                state,
                "quick-notice-delay-started",
                Args::new().arg(1, seconds),
            );
            show_notice(state, &text);
        }
        Err(e) => {
            state.delay.cancel();
            tracing::error!(error = %e, "启动延迟截图计时线程失败");
        }
    }
}

/// 直接截图：不进覆盖层，抓取整屏或前台窗口后按设置复制 / 保存。
///
/// # 参数
/// - `cx`：外壳上下文（枚举显示器）。
/// - `state`：运行时状态。
/// - `kind`：截图目标。
fn request_direct_capture(cx: &mut ShellContext, state: &mut AppState, kind: DirectKind) {
    let overlay_open = state
        .overlay
        .as_ref()
        .is_some_and(|window| cx.is_window_open(window));
    if state.direct_in_flight
        || capture_gate(state.capture_in_flight, overlay_open) != CaptureGate::Proceed
    {
        tracing::info!(?kind, "直接截图被忽略：已有截图在进行");
        let text = notice_text(state, "quick-notice-capture-busy", Args::new());
        show_notice(state, &text);
        return;
    }
    let monitors = match cx.monitors() {
        Ok(m) => m,
        Err(e) => {
            tracing::error!(error = %e, "枚举显示器失败，无法直接截图");
            return;
        }
    };
    let region = match kind {
        DirectKind::FullScreen => pick_monitor(&monitors, cursor_screen_position().ok())
            .and_then(|m| full_monitor_region(&m)),
        DirectKind::FocusedWindow => snow_platform::text_inject::foreground_window_rect()
            .and_then(|rect| clip_to_monitor(rect, &monitors)),
    };
    let Some((monitor, region)) = region else {
        let text = notice_text(state, "quick-notice-no-focused-window", Args::new());
        show_notice(state, &text);
        return;
    };
    let (plan, dir, policy) = {
        let store = state.config.borrow();
        let document = store.document();
        let (dir, _) = resolve_save_directory(document, home_directory().as_deref());
        (
            direct_output_plan_from(document),
            dir,
            policy_from_document(document),
        )
    };
    let history = state.history.clone().map(|recorder| DirectHistory {
        recorder,
        policy,
        source: match kind {
            DirectKind::FullScreen => HistorySource::CurrentMonitor,
            DirectKind::FocusedWindow => HistorySource::FocusedWindow,
        },
    });
    tracing::info!(
        ?kind,
        monitor = monitor.id.0,
        ?region,
        ?plan,
        "开始直接截图"
    );
    state.direct_in_flight = true;
    let inbox = state.inbox.clone();
    let spawned = spawn_direct_capture(region, plan, dir, history, move |result| {
        inbox.push(UiEvent::DirectCaptureDone(result));
    });
    if let Err(e) = spawned {
        state.direct_in_flight = false;
        tracing::error!(error = %e, "启动直接截图线程失败");
    }
}

/// 直接截图完成后的收尾：记录日志并给出提示（历史已在采集线程里提交写入）。
///
/// # 参数
/// - `state`：运行时状态。
/// - `result`：采集与输出结果。
fn on_direct_capture_done(state: &mut AppState, result: Result<DirectResult, String>) {
    state.direct_in_flight = false;
    let text = match result {
        Err(reason) => {
            tracing::error!(%reason, "直接截图采集失败");
            notice_text(
                state,
                "quick-notice-direct-failed",
                Args::new().arg(1, reason),
            )
        }
        Ok(r) if r.has_failure() => {
            let reason = r.failure_reason().unwrap_or_default();
            tracing::error!(%reason, "直接截图输出失败");
            notice_text(
                state,
                "quick-notice-direct-failed",
                Args::new().arg(1, reason),
            )
        }
        Ok(r) => match &r.saved {
            Some(Ok(path)) => {
                let name = path
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_default();
                notice_text(state, "quick-notice-direct-saved", Args::new().arg(1, name))
            }
            _ => notice_text(
                state,
                "quick-notice-direct-copied",
                Args::new().arg(1, r.width).arg(2, r.height),
            ),
        },
    };
    show_notice(state, &text);
}

/// 切换「前台全屏窗口时停用热键」：写回配置、同步闸门镜像并重建托盘菜单的勾选。
///
/// # 参数
/// - `state`：运行时状态。
fn toggle_fullscreen_gate(state: &mut AppState) {
    use crate::fullscreen_gate::{
        DISABLE_ON_FULLSCREEN_CONFIG_KEY, config_value, configured, set_enabled,
    };
    let enabled = !configured(state.config.borrow().document());
    {
        let mut store = state.config.borrow_mut();
        if let Err(e) = store.set_value(DISABLE_ON_FULLSCREEN_CONFIG_KEY, config_value(enabled)) {
            tracing::warn!(error = %e, "写入前台全屏停用热键开关失败");
        } else if let Err(e) = store.flush() {
            tracing::warn!(error = %e, "前台全屏停用热键开关落盘失败");
        }
    }
    // 以配置为准：写入失败时镜像与勾选都回到真实状态
    let effective = configured(state.config.borrow().document());
    set_enabled(effective);
    refresh_tray_menu(state);
    let id = if effective {
        NOTICE_FULLSCREEN_GATE_ON
    } else {
        NOTICE_FULLSCREEN_GATE_OFF
    };
    let text = notice_text(state, id, Args::new());
    show_notice(state, &text);
}

/// 暂停 / 恢复全部全局热键：暂停时只保留开关本身，并在托盘悬停提示与日志里体现。
///
/// # 参数
/// - `state`：运行时状态。
fn toggle_global_hotkeys(state: &mut AppState) {
    let Some(service) = state.hotkeys.as_ref() else {
        tracing::warn!("热键服务未运行，无法暂停 / 恢复热键");
        return;
    };
    for handle in state.hotkey_handles.drain(..) {
        if let Err(e) = service.unregister(handle) {
            tracing::warn!(error = %e, "注销旧热键失败");
        }
    }
    let paused = !state.hotkeys_paused;
    let attempt = register_all_hotkeys_gated(service, state.config.borrow().document(), paused);
    state.hotkeys_paused = paused;
    refresh_tray_menu(state);
    state.hotkey_handles = attempt.handles;
    tracing::info!(
        paused,
        registered = state.hotkey_handles.len(),
        "全局热键暂停状态已切换"
    );
    let text = if paused {
        notice_text(state, "quick-notice-hotkeys-paused", Args::new())
    } else if attempt.failures.is_empty() {
        notice_text(state, "quick-notice-hotkeys-resumed", Args::new())
    } else {
        let reasons = attempt
            .failures
            .iter()
            .map(|f| format!("{}: {}", f.shortcut, f.reason))
            .collect::<Vec<_>>()
            .join("; ");
        notice_text(
            state,
            "quick-notice-hotkeys-resume-failed",
            Args::new().arg(1, reasons),
        )
    };
    show_notice(state, &text);
    if paused && let Some(tray) = state.tray.as_ref() {
        // 暂停状态要常驻可见：把悬停提示固定为“热键已暂停”
        let tip = notice_text(state, "tray-tooltip-paused", Args::new());
        if let Err(e) = tray.set_tooltip(tip) {
            tracing::warn!(error = %e, "更新托盘提示失败");
        }
    }
}

/// 在资源管理器里打开录屏保存目录（不存在则先创建）。
///
/// # 参数
/// - `state`：运行时状态。
fn open_recording_folder(state: &AppState) {
    let dir = recording_directory(
        state.config.borrow().document(),
        home_directory().as_deref(),
    );
    let opened = std::fs::create_dir_all(&dir)
        .map_err(|e| e.to_string())
        .and_then(|()| {
            std::process::Command::new("explorer.exe")
                .arg(&dir)
                .spawn()
                .map(|_| ())
                .map_err(|e| e.to_string())
        });
    match opened {
        Ok(()) => tracing::info!(dir = %dir.display(), "已打开录屏保存目录"),
        Err(reason) => {
            tracing::warn!(dir = %dir.display(), %reason, "打开录屏保存目录失败");
            let text = notice_text(
                state,
                "quick-notice-folder-failed",
                Args::new().arg(1, reason),
            );
            show_notice(state, &text);
        }
    }
}

/// 主线程事件分发（由 GPUI 主线程调用）。
///
/// # 参数
/// - `cx`：GPUI 外壳上下文。
/// - `state`：运行时状态。
/// - `event`：待处理事件。
pub fn handle_event(cx: &mut ShellContext, state: &mut AppState, event: UiEvent) {
    tracing::debug!(?event, "dispatching ui event");
    match event {
        UiEvent::Capture { origin } => {
            request_capture(cx, state, origin, CaptureMode::Screenshot);
        }
        UiEvent::CaptureReady(payload) => {
            if let Some(payloads) = state.capture_collector.push(payload) {
                open_overlays(cx, state, payloads);
            }
        }
        UiEvent::OverlayClosed => close_all_overlays(cx, state),
        UiEvent::CaptureFailed(reason) => {
            state.capture_collector.cancel();
            state.window_hover = None;
            state.capture_in_flight = false;
            state.capture_mode = CaptureMode::Screenshot;
            tracing::error!(%reason, "屏幕采集失败，未打开覆盖窗");
        }
        UiEvent::Export(target) => export_from_overlay(cx, state, &target),
        UiEvent::DirectCapture(request) => direct_capture(cx, state, request),
        UiEvent::OpenSettings => open_or_focus_settings(cx, state),
        UiEvent::Mcp(request) => handle_mcp_request(cx, state, &request),
        UiEvent::OpenMainWindow => open_or_focus_main_window(cx, state),
        UiEvent::MainWindowSidebarCollapsed(collapsed) => save_sidebar_collapsed(state, collapsed),
        UiEvent::OpenHistory => open_or_focus_history(cx, state),
        UiEvent::HistoryChanged => {
            if let Some((_, view)) = &state.history_window {
                view.update(cx.app(), |v, cx| v.refresh(cx));
            }
        }
        UiEvent::HistoryThumb { id, thumb } => {
            if let Some((_, view)) = &state.history_window {
                view.update(cx.app(), |v, cx| v.set_thumb(&id, thumb, cx));
            }
        }
        UiEvent::HistoryActionDone { action, error } => {
            if let Some((_, view)) = &state.history_window {
                view.update(cx.app(), |v, cx| v.show_result(action, error, cx));
            }
        }
        UiEvent::HistoryPin {
            width,
            height,
            rgba,
        } => {
            let result = state.pins.create_from_image(cx, width, height, rgba);
            if let Err(e) = &result {
                tracing::warn!(error = %e, "从截图历史贴图失败");
            }
            if let Some((_, view)) = &state.history_window {
                view.update(cx.app(), |v, cx| {
                    v.show_result(HistoryAction::Pin, result.err(), cx)
                });
            }
        }
        UiEvent::OpenTranslateInput => open_or_focus_translate_input(cx, state),
        UiEvent::OpenTranslatePage => open_or_focus_translate_page(cx, state),
        UiEvent::TranslatePageRequested {
            serial,
            text,
            model_id,
            source,
            target,
            embedded,
        } => spawn_translate_page(state, serial, text, model_id, source, target, embedded),
        UiEvent::TranslatePageFinished {
            serial,
            result,
            embedded,
        } => {
            if embedded {
                let page = state
                    .main_window
                    .as_ref()
                    .and_then(|(_, main)| main.read(cx.app()).translate_view());
                if let Some(view) = page {
                    view.update(cx.app(), |v, vcx| v.finish(serial, result, vcx));
                }
            } else if let Some((window, view)) = &state.translate_page
                && cx.is_window_open(window)
            {
                view.update(cx.app(), |v, vcx| v.finish(serial, result, vcx));
            }
        }
        UiEvent::TranslateWindowSettled { scale } => save_translate_window_size(state, scale),
        UiEvent::MainWindowSettled { maximized } => save_main_window_geometry(state, maximized),
        UiEvent::OpenFile(path) => {
            if let Err(e) = std::process::Command::new("explorer.exe")
                .arg(&path)
                .spawn()
            {
                tracing::warn!(path = %path.display(), error = %e, "打开文件失败");
            }
        }
        UiEvent::Dictation(command) => state.dictation.command(cx, state.tray.as_ref(), command),
        UiEvent::DictationPoll => state.dictation.tick(cx, state.tray.as_ref()),
        UiEvent::DictationProbed { round, verdict } => {
            state
                .dictation
                .probed(cx, state.tray.as_ref(), round, verdict)
        }
        UiEvent::DictationTranslated {
            round,
            seq,
            outcome,
        } => state.dictation.translated(cx, round, seq, outcome),
        UiEvent::TranslateInputRequested {
            serial,
            text,
            model_id,
        } => spawn_translate_input(state, serial, text, model_id),
        UiEvent::TranslateInputFinished { serial, result } => {
            if let Some((window, view)) = &state.translate_input
                && cx.is_window_open(window)
            {
                view.update(cx.app(), |v, vcx| v.finish(serial, result, vcx));
            }
        }
        UiEvent::ConfigChanged { key, previous } => on_config_changed(cx, state, &key, previous),
        UiEvent::StartRecording => request_recording(cx, state),
        UiEvent::RecordingRegionChosen { region, monitor } => {
            state.recording.begin(cx, region, &monitor, None);
            if std::mem::take(&mut state.record_copy_pending) {
                state.recording.mark_copy_on_finish(cx);
            }
        }
        UiEvent::RecorderPoll | UiEvent::RecordingTick => state.recording.sync(cx),
        UiEvent::PinCreate {
            rect,
            width,
            height,
            rgba,
        } => match state.pins.create_from_rgba(cx, width, height, rgba, rect) {
            Ok(id) => tracing::info!(id = %id, rect = ?rect, "已从选区创建贴图"),
            Err(e) => tracing::error!(error = %e, "从选区创建贴图失败"),
        },
        UiEvent::PinFromClipboard => match state.pins.create_from_clipboard(cx) {
            Ok(id) => tracing::info!(id = %id, "已从剪贴板创建贴图"),
            Err(e) => tracing::warn!(error = %e, "从剪贴板创建贴图失败"),
        },
        UiEvent::RestorePins => {
            let restored = state.pins.restore_all(cx);
            tracing::info!(restored, "启动恢复贴图");
            refresh_tray_menu(state);
        }
        UiEvent::PinControl(crate::pinned_shared::PinControlEvent::OcrRequested {
            id,
            width,
            height,
            rgba,
        }) => {
            spawn_pin_ocr(state, id, width, height, rgba);
        }
        UiEvent::PinOcrFinished { id, result } => state.pins.deliver_ocr(cx, &id, result),
        UiEvent::PinClosed { id } => {
            state.pins.forget(&id);
            refresh_pin_manage(cx, state);
        }
        UiEvent::OpenPinManage => open_or_focus_pin_manage(cx, state),
        UiEvent::MouseGesture(event) => on_mouse_gesture(cx, state, event),
        UiEvent::OpenRecognitionWindow(data) => open_recognition_window(cx, state, *data),
        UiEvent::SelectedTextReady(None) => {
            let text = notice_text(state, NOTICE_NO_SELECTED_TEXT, Args::new());
            show_notice(state, &text);
        }
        UiEvent::SelectedTextReady(Some(text)) => {
            open_or_focus_translate_input(cx, state);
            if let Some((window, view)) = &state.translate_input {
                let view = view.clone();
                let _ = window.gpui_handle().update(cx.app(), |_, window, app| {
                    view.update(app, |v, vcx| v.prefill_and_submit(&text, window, vcx));
                });
            }
        }
        UiEvent::PinImage {
            width,
            height,
            rgba,
        } => {
            if let Err(e) = state.pins.create_from_image(cx, width, height, rgba) {
                tracing::warn!(error = %e, "贴选中的图片失败");
            }
            refresh_pin_manage(cx, state);
        }
        UiEvent::PinFilesEmpty => {
            let text = notice_text(state, NOTICE_PIN_SELECTED_FILES, Args::new());
            show_notice(state, &text);
        }
        UiEvent::PinManageThumb { id, thumb } => {
            if let Some((_, view)) = &state.pin_manage_window {
                view.update(cx.app(), |v, cx| v.set_thumb(&id, thumb, cx));
            }
        }
        UiEvent::PinManageShow { id } => {
            if let Err(e) = state.pins.show_pin(cx, &id) {
                report_pin_manage_error(cx, state, e);
            }
            refresh_tray_menu(state);
            refresh_pin_manage(cx, state);
        }
        UiEvent::PinManageDelete { id } => {
            state.pins.delete_pin(cx, &id);
            refresh_pin_manage(cx, state);
        }
        UiEvent::PinManageDeleteAll => {
            let deleted = state.pins.delete_all(cx);
            tracing::info!(deleted, "已删除全部贴图");
            refresh_pin_manage(cx, state);
        }
        UiEvent::PinGroupCreateNamed { name } => {
            if let Err(e) = state
                .pins
                .shared()
                .create_group(Some(&name), pin_group_i18n(state))
            {
                report_pin_manage_error(cx, state, e);
            }
            refresh_tray_menu(state);
            refresh_pin_manage(cx, state);
        }
        UiEvent::PinGroupDelete { id } => {
            if let Err(e) = state.pins.delete_group(cx, &id) {
                report_pin_manage_error(cx, state, e);
            }
            refresh_tray_menu(state);
            refresh_pin_manage(cx, state);
        }
        UiEvent::PinGroupSwitch { id } => {
            match state.pins.switch_group(cx, &id) {
                Ok(restored) => tracing::info!(group = %id, restored, "已切换贴图分组"),
                Err(e) => tracing::warn!(group = %id, error = %e, "切换贴图分组失败"),
            }
            refresh_tray_menu(state);
            refresh_pin_manage(cx, state);
        }
        UiEvent::PinGroupNew => {
            match state
                .pins
                .shared()
                .create_group(None, pin_group_i18n(state))
            {
                Ok(id) => tracing::info!(group = %id, "已新建贴图分组"),
                Err(e) => tracing::warn!(error = %e, "新建贴图分组失败"),
            }
            refresh_tray_menu(state);
            refresh_pin_manage(cx, state);
        }
        UiEvent::PinGroupDeleteEmpty => {
            match state.pins.shared().delete_empty_groups() {
                Ok(n) => tracing::info!(deleted = n, "已删除空的贴图分组"),
                Err(e) => tracing::warn!(error = %e, "删除空分组失败"),
            }
            refresh_tray_menu(state);
            refresh_pin_manage(cx, state);
        }
        UiEvent::PinMoveToGroup { id, group } => {
            if let Err(e) = state.pins.move_to_group(cx, &id, &group) {
                tracing::warn!(id = %id, group = %group, error = %e, "移动贴图到分组失败");
            }
            refresh_pin_manage(cx, state);
        }
        UiEvent::PinControl(event) => state.pins.handle_control(cx, &state.inbox, event),
        UiEvent::PinExitClickThrough { id } => state.pins.exit_click_through(cx, &id),
        UiEvent::PinHideReveal { id } => state.pins.reveal_hidden(cx, &id),
        UiEvent::PinExitHideToTop { id } => state.pins.exit_hide_to_top(cx, &id),
        UiEvent::OcrRequested {
            serial,
            width,
            height,
            rgba,
        } => spawn_ocr(state, serial, width, height, rgba),
        UiEvent::TableRequested {
            serial,
            width,
            height,
            rgba,
        } => spawn_table(state, serial, width, height, rgba),
        UiEvent::TableDownloadRequested => spawn_table_download(state),
        UiEvent::OcrFinished { serial, result } => {
            let outcome = state.overlay_view.as_ref().map(|view| {
                view.update(cx.app(), |v, vcx| {
                    let outcome = v.finish_ocr(serial, result);
                    vcx.notify();
                    outcome
                })
            });
            // 配置为“复制并结束截图”：复制成功后关闭覆盖窗
            if outcome == Some(OverlayOutcome::Close) {
                close_all_overlays(cx, state);
            }
        }
        UiEvent::TranslateRequested {
            serial,
            width,
            height,
            rgba,
        } => spawn_translate(state, serial, width, height, rgba),
        UiEvent::TranslateProgress { serial, stage } => {
            if let Some(view) = &state.overlay_view {
                view.update(cx.app(), |v, vcx| {
                    v.update_translate_stage(serial, stage);
                    vcx.notify();
                });
            }
        }
        UiEvent::TranslateFinished { serial, result } => {
            let (auto, _) = parse_translate_auto(
                std::env::var(ENV_OVERLAY_BENCH_TRANSLATE_AUTO)
                    .ok()
                    .as_deref(),
            );
            if let Some(view) = &state.overlay_view {
                view.update(cx.app(), |v, vcx| {
                    v.finish_translate(serial, result);
                    if auto
                        && matches!(
                            v.translate_state(),
                            TranslateUiState::Failed {
                                can_download: true,
                                ..
                            }
                        )
                    {
                        v.handle_key("d", false, false);
                    }
                    vcx.notify();
                });
            }
        }
        UiEvent::TranslateDownloadRequested => spawn_translate_runtime_download(state),
        UiEvent::TranslateDownloadProgress(step) => {
            if let Some(view) = &state.overlay_view {
                view.update(cx.app(), |v, vcx| {
                    v.update_translate_download(&step);
                    vcx.notify();
                });
            }
        }
        UiEvent::TranslateDownloadFinished(result) => {
            match &result {
                Ok(()) => tracing::info!("onnxruntime 运行时下载完成"),
                Err(e) => tracing::warn!(error = %e, "onnxruntime 运行时下载失败"),
            }
            let (_, auto_retry) = parse_translate_auto(
                std::env::var(ENV_OVERLAY_BENCH_TRANSLATE_AUTO)
                    .ok()
                    .as_deref(),
            );
            let retry = result.is_ok() && auto_retry;
            if let Some(view) = &state.overlay_view {
                view.update(cx.app(), |v, vcx| {
                    v.finish_translate_download(result);
                    if retry {
                        v.apply_action(ToolbarAction::Translate);
                    }
                    vcx.notify();
                });
            }
        }
        UiEvent::OcrDownloadRequested => spawn_ocr_download(state),
        UiEvent::OcrDownloadProgress(step) => {
            if let Some(view) = &state.overlay_view {
                view.update(cx.app(), |v, vcx| {
                    v.update_ocr_download(&step);
                    vcx.notify();
                });
            }
        }
        UiEvent::OcrDownloadFinished(result) => {
            match &result {
                Ok(()) => tracing::info!("OCR 组件下载完成"),
                Err(e) => tracing::warn!(error = %e, "OCR 组件下载失败"),
            }
            if let Some(view) = &state.overlay_view {
                view.update(cx.app(), |v, vcx| {
                    v.finish_ocr_download(result);
                    vcx.notify();
                });
            }
        }
        UiEvent::ConfigTransferRequested(action) => run_config_transfer(cx, state, action),
        UiEvent::UpdateActionRequested(action) => run_update_action(state, action),
        UiEvent::UpdateDownloadFinished(info, outcome) => {
            let locale = ui_prefs_from_document(state.config.borrow().document()).locale;
            let ui_state = crate::net_settings::download_outcome_state(&info, &outcome, locale);
            tracing::info!(version = %info.version, ?outcome, "更新包下载结束");
            publish_update_state(state, cx, ui_state);
        }
        UiEvent::UpdateCheckFinished(outcome) => {
            let locale = ui_prefs_from_document(state.config.borrow().document()).locale;
            let ui_state = crate::net_settings::update_outcome_state(&outcome, locale);
            tracing::info!(?outcome, "检查更新结束");
            publish_update_state(state, cx, ui_state);
        }
        UiEvent::SttDownloadRequested { model_id, cancel } => {
            spawn_stt_download(state, model_id, cancel)
        }
        UiEvent::SttDownloadProgress(progress) => {
            for view in settings_views(state, cx.app()) {
                let progress = progress.clone();
                view.update(cx.app(), |v, vcx| v.update_stt_download(progress, vcx));
            }
        }
        UiEvent::SttDownloadFinished { model_id, result } => {
            match (stt_download::classify(&result), &result) {
                (stt_download::Outcome::Done, _) => {
                    tracing::info!(model = %model_id, "语音模型下载完成")
                }
                (stt_download::Outcome::Cancelled, _) => {
                    tracing::info!(model = %model_id, "用户取消下载")
                }
                (stt_download::Outcome::Failed, Err(e)) => {
                    tracing::warn!(model = %model_id, error = ?e, "语音模型下载失败")
                }
                (stt_download::Outcome::Failed, Ok(())) => {}
            }
            let i18n = crate::ocr_backend::i18n_for(
                ui_prefs_from_document(state.config.borrow().document()).locale,
            );
            let result = result.map_err(|e| e.message(i18n));
            for view in settings_views(state, cx.app()) {
                let (model_id, result) = (model_id.clone(), result.clone());
                view.update(cx.app(), |v, vcx| {
                    v.finish_stt_download(model_id, result, vcx)
                });
            }
        }
        UiEvent::StartScrollCapture => request_scroll_capture(cx, state),
        UiEvent::ScrollRegionChosen { region, monitor } => {
            state.scroll.begin(cx, region, &monitor, None);
        }
        UiEvent::ScrollTick => state.scroll.sync(cx),
        UiEvent::QuickAction(action) => run_quick_action(cx, state, action),
        UiEvent::DelayElapsed { serial } => {
            if state.delay.fire(serial) {
                request_capture(cx, state, ORIGIN_HOTKEY, CaptureMode::Screenshot);
            } else {
                tracing::debug!(serial, "过期的延迟截图回调，已丢弃");
            }
        }
        UiEvent::DirectCaptureDone(result) => on_direct_capture_done(state, result),
        UiEvent::Quit => shutdown_app(cx, state),
        UiEvent::Restart => match spawn_restart_helper() {
            Ok(()) => shutdown_app(cx, state),
            Err(e) => tracing::warn!(error = %e, "拉起新实例失败，取消重启"),
        },
    }
}

/// 有序关闭各后台服务并退出应用。
///
/// # 参数
/// - `cx`：外壳上下文。
/// - `state`：运行时状态。
fn shutdown_app(cx: &mut ShellContext, state: &mut AppState) {
    tracing::info!("quit requested, shutting down");
    state.ocr.shutdown();
    state.translator.shutdown();
    state.dictation.shutdown();
    state.pins.persist_all(cx);
    state.shutdown_services();
    cx.quit();
}

/// 启动托盘与全局热键；任一失败只记日志并降级。
///
/// # 参数
/// - `caps`：平台能力表。
/// - `bus`：命令总线（热键 / 托盘命令的出口）。
/// - `inbox`：主线程收件箱（托盘信号的出口）。
/// - `document`：配置文档（读取热键）。
///
/// # 返回
/// `(托盘服务, 热键服务, 已注册的热键句柄)`，失败项为 `None`。
///
/// ```ignore
/// let (tray, hotkeys, handles) = start_services(&caps, &bus, &inbox, &document);
/// ```
pub fn start_services(
    caps: &CapabilityRegistry,
    bus: &CommandBus,
    inbox: &MainThreadInbox<UiEvent>,
    document: &ConfigDocument,
) -> (
    Option<TrayService>,
    Option<HotkeyService>,
    Vec<HotkeyHandle>,
) {
    let prefs = ui_prefs_from_document(document);
    let locale = prefs.locale;
    apply_popup_menu_theme(prefs.dark);
    let tray_on = crate::tray_config::tray_enabled(document);
    let tray_spec = if tray_on {
        build_tray_spec(locale, document, false)
    } else {
        Err("托盘已在设置中关闭".to_string())
    };
    let tray = match tray_spec.and_then(|spec| {
        TrayService::start(caps, spec, Dispatcher::from_bus(bus.clone())).map_err(|e| e.to_string())
    }) {
        Ok(tray) => {
            let signal_inbox = inbox.clone();
            let sink = Box::new(move |signal: String| match map_tray_signal(&signal) {
                Some(event) => {
                    signal_inbox.push(event);
                }
                None => tracing::warn!(%signal, "未知托盘信号，已忽略"),
            });
            if let Err(e) = tray.set_signal_sink(sink) {
                tracing::error!(error = %e, "托盘信号出口设置失败，设置/退出菜单将无效");
            }
            tracing::info!("tray icon started");
            Some(tray)
        }
        Err(e) => {
            if tray_on {
                tracing::error!(error = %e, "托盘启动失败，将无托盘图标运行");
            } else {
                tracing::info!("托盘已在设置中关闭，不创建托盘图标");
            }
            None
        }
    };
    crate::fullscreen_gate::set_enabled(crate::fullscreen_gate::configured(document));
    let hotkey_dispatcher = crate::fullscreen_gate::gate_dispatcher(
        Dispatcher::from_bus(bus.clone()),
        snow_platform::window_rect::focused_fullscreen_window_exists,
    );
    let mut handles = Vec::new();
    let hotkeys = match HotkeyService::start(caps, hotkey_dispatcher) {
        Ok(service) => {
            let registration = register_all_hotkeys(&service, document);
            tracing::info!(
                registered = registration.handles.len(),
                failed = registration.failures.len(),
                "global hotkey service started"
            );
            handles = registration.handles;
            Some(service)
        }
        Err(e) => {
            tracing::error!(error = %e, "全局热键服务启动失败，热键不可用");
            None
        }
    };
    (tray, hotkeys, handles)
}

#[cfg(test)]
mod tests {
    use super::*;
    use snow_app_core::command::CommandContext;

    /// 翻译自动化开关解析：空关闭、download 只下载、其它值下载后重试。
    #[test]
    fn translate_auto_switch_parsing() {
        assert_eq!(parse_translate_auto(None), (false, false));
        assert_eq!(parse_translate_auto(Some("  ")), (false, false));
        assert_eq!(parse_translate_auto(Some("download")), (true, false));
        assert_eq!(parse_translate_auto(Some("1")), (true, true));
        assert_eq!(parse_translate_auto(Some("retry")), (true, true));
    }

    /// IPC 命令映射覆盖全部变体。
    #[test]
    fn ipc_mapping() {
        assert_eq!(
            map_ipc_command(&IpcCommand::TriggerScreenshot),
            Some(UiEvent::Capture { origin: ORIGIN_IPC })
        );
        assert_eq!(
            map_ipc_command(&IpcCommand::TriggerRecording),
            Some(UiEvent::StartRecording)
        );
        assert_eq!(
            map_ipc_command(&IpcCommand::ScrollCapture),
            Some(UiEvent::StartScrollCapture)
        );
        assert_eq!(
            map_ipc_command(&IpcCommand::PinClipboard),
            Some(UiEvent::PinFromClipboard)
        );
        assert_eq!(
            map_ipc_command(&IpcCommand::OpenSettings),
            Some(UiEvent::OpenSettings)
        );
        assert_eq!(
            map_ipc_command(&IpcCommand::ShowMainWindow),
            Some(UiEvent::OpenMainWindow)
        );
        assert_eq!(map_ipc_command(&IpcCommand::Quit), Some(UiEvent::Quit));
        assert_eq!(map_ipc_command(&IpcCommand::Custom("x".into())), None);
    }

    /// 托盘信号映射：已知信号有事件，未知信号忽略。
    #[test]
    fn tray_signal_mapping() {
        assert_eq!(map_tray_signal("settings"), Some(UiEvent::OpenSettings));
        assert_eq!(
            map_tray_signal("main_window"),
            Some(UiEvent::OpenMainWindow)
        );
        assert_eq!(map_tray_signal("history"), Some(UiEvent::OpenHistory));
        assert_eq!(map_tray_signal("quit"), Some(UiEvent::Quit));
        assert_eq!(map_tray_signal("restart"), Some(UiEvent::Restart));
        assert_eq!(
            map_tray_signal("pin_clipboard"),
            Some(UiEvent::PinFromClipboard)
        );
        assert_eq!(map_tray_signal("rm -rf"), None);
    }

    /// 托盘菜单（默认 `tray/menu_options` 的 12 项）：分隔线落在各组之间，首尾条目正确，非勾选条目带图标。
    #[test]
    fn tray_spec_shape() {
        let doc = ConfigDocument::from_bytes(None);
        let spec = build_tray_spec("zh-CN", &doc, false).unwrap();
        assert_eq!(spec.menu.len(), 15);
        let seps: Vec<usize> = spec
            .menu
            .iter()
            .enumerate()
            .filter(|(_, e)| matches!(e, TrayMenuEntry::Separator))
            .map(|(i, _)| i)
            .collect();
        assert_eq!(seps, vec![5, 8, 10, 12]);
        assert!(matches!(
            &spec.menu[0],
            TrayMenuEntry::Item {
                action: TrayAction::Command(AppCommand::Capture(_)),
                ..
            }
        ));
        assert!(matches!(
            spec.menu.last().unwrap(),
            TrayMenuEntry::Item { action: TrayAction::Signal(s), .. } if s == TRAY_SIGNAL_QUIT
        ));
        for entry in &spec.menu {
            if let TrayMenuEntry::Item {
                icon,
                label,
                checked: None,
                ..
            } = entry
            {
                assert!(icon.is_some(), "缺少图标: {label}");
            }
        }
    }

    /// 取菜单里第 `i` 项的 `(label, checked)`。
    fn tray_item(spec: &TraySpec, i: usize) -> (String, Option<bool>) {
        match &spec.menu[i] {
            TrayMenuEntry::Item { label, checked, .. } => (label.clone(), *checked),
            TrayMenuEntry::Separator => panic!("应为条目"),
        }
    }

    /// 托盘“禁用全局热键”勾选状态随热键暂停状态变化。
    #[test]
    fn tray_hotkey_toggle_checked_follows_state() {
        let doc = ConfigDocument::from_bytes(None);
        let toggle_at = |spec: &TraySpec| {
            spec.menu
                .iter()
                .position(|e| {
                    matches!(
                        e,
                        TrayMenuEntry::Item {
                            action: TrayAction::Command(AppCommand::QuickAction(
                                QuickAction::ToggleGlobalHotkeys
                            )),
                            ..
                        }
                    )
                })
                .expect("默认菜单含暂停热键项")
        };
        let off = build_tray_spec("en-US", &doc, false).unwrap();
        let on = build_tray_spec("en-US", &doc, true).unwrap();
        assert_eq!(tray_item(&off, toggle_at(&off)).1, Some(false));
        assert_eq!(tray_item(&on, toggle_at(&on)).1, Some(true));
    }

    /// `tray/menu_options` 决定显示哪些项；左 / 中键动作跟随 `tray/*_click_action`，默认左键截图、中键贴图。
    #[test]
    fn tray_follows_menu_options_and_click_actions() {
        let mut doc = ConfigDocument::from_bytes(None);
        let default_spec = build_tray_spec("en-US", &doc, false).unwrap();
        assert!(matches!(
            default_spec.on_left_click,
            Some(TrayAction::Command(AppCommand::Capture(_)))
        ));
        assert!(matches!(
            default_spec.on_middle_click,
            Some(TrayAction::Command(AppCommand::QuickAction(
                QuickAction::ScreenshotFixed
            )))
        ));
        assert!(!has_signal(&default_spec, TRAY_SIGNAL_HISTORY));

        doc.set_value(
            "tray/menu_options",
            serde_json::json!(["quick.open-capture-history", "tray.exit"]),
        )
        .unwrap();
        doc.set_value(
            "tray/left_click_action",
            serde_json::json!("show_main_window"),
        )
        .unwrap();
        let spec = build_tray_spec("en-US", &doc, false).unwrap();
        assert_eq!(spec.menu.len(), 3, "历史、退出两项，中间只留一条组间分隔线");
        assert!(matches!(spec.menu[1], TrayMenuEntry::Separator));
        assert!(has_signal(&spec, TRAY_SIGNAL_HISTORY) && has_signal(&spec, TRAY_SIGNAL_QUIT));
        assert!(
            matches!(spec.on_left_click, Some(TrayAction::Signal(ref s)) if s == TRAY_SIGNAL_MAIN_WINDOW)
        );
    }

    /// 菜单里是否有触发指定信号的条目。
    fn has_signal(spec: &TraySpec, wanted: &str) -> bool {
        spec.menu.iter().any(|e| matches!(e, TrayMenuEntry::Item { action: TrayAction::Signal(s), .. } if s == wanted))
    }

    /// 延迟截图文案带上配置的秒数（默认 3，改成 7 后跟随）。
    #[test]
    fn tray_delay_label_contains_seconds() {
        let mut doc = ConfigDocument::from_bytes(None);
        assert!(
            tray_item(&build_tray_spec("en-US", &doc, false).unwrap(), 1)
                .0
                .contains('3')
        );
        doc.set_value(DELAY_SECONDS_CONFIG_KEY, serde_json::json!(7))
            .unwrap();
        assert!(
            tray_item(&build_tray_spec("zh-CN", &doc, false).unwrap(), 1)
                .0
                .contains('7')
        );
    }

    /// 两种语言下所有托盘文案都存在（无“缺失”占位）。
    #[test]
    fn tray_labels_exist_in_both_locales() {
        let doc = ConfigDocument::from_bytes(None);
        for locale in ["en-US", "zh-CN"] {
            let spec = build_tray_spec(locale, &doc, false).unwrap();
            for entry in &spec.menu {
                if let TrayMenuEntry::Item { label, .. } = entry {
                    assert!(
                        !label.is_empty() && !label.contains("[!"),
                        "{locale}: {label}"
                    );
                }
            }
        }
    }

    /// 真机：读取前台应用当前选中的文字并打印。需要先在别的应用里选中文字，默认忽略；手动用 `--ignored --nocapture` 跑。
    #[test]
    #[ignore = "需要前台应用里有选中的文字"]
    fn real_selected_text_probe() {
        eprintln!("SELECTED: {:?}", read_selected_text());
    }

    /// 只有常见图片扩展名（不分大小写）才会被当成可贴的文件。
    #[test]
    fn pinnable_image_extensions() {
        for ok in ["a.png", "b.JPG", "c.jpeg", "d.bmp", "e.gif", "f.WebP"] {
            assert!(is_pinnable_image(std::path::Path::new(ok)), "{ok}");
        }
        for bad in ["a.txt", "b", "c.png.exe", "d.svg"] {
            assert!(!is_pinnable_image(std::path::Path::new(bad)), "{bad}");
        }
    }

    /// 托盘分组块：出现在贴图组之后，激活分组带勾，新建 / 删除空分组两项走信号。
    #[test]
    fn tray_group_block_follows_pin_section() {
        let doc = ConfigDocument::from_bytes(None);
        let base = build_tray_spec("en-US", &doc, false).unwrap().menu.len();
        let groups = vec![
            ("default".to_string(), "Default group".to_string()),
            ("g1".to_string(), "Work".to_string()),
        ];
        let spec = build_tray_spec_with_groups("en-US", &doc, false, &groups, "g1").unwrap();
        // 两个分组 + 新建 + 删除空 + 贴图管理 + 一条分隔线
        assert_eq!(spec.menu.len(), base + 6);
        let find = |wanted: &str| {
            spec.menu.iter().find_map(|e| match e {
                TrayMenuEntry::Item {
                    action: TrayAction::Signal(s),
                    checked,
                    ..
                } if s == wanted => Some(*checked),
                _ => None,
            })
        };
        assert_eq!(find("group:g1"), Some(Some(true)));
        assert_eq!(find("group:default"), Some(Some(false)));
        assert!(
            find(TRAY_SIGNAL_GROUP_NEW).is_some() && find(TRAY_SIGNAL_GROUP_DELETE_EMPTY).is_some()
        );
    }

    /// 分组相关托盘信号映射成收件箱事件。
    #[test]
    fn tray_group_signals_map() {
        assert_eq!(
            map_tray_signal("group:abc"),
            Some(UiEvent::PinGroupSwitch { id: "abc".into() })
        );
        assert_eq!(
            map_tray_signal(TRAY_SIGNAL_GROUP_NEW),
            Some(UiEvent::PinGroupNew)
        );
        assert_eq!(
            map_tray_signal(TRAY_SIGNAL_GROUP_DELETE_EMPTY),
            Some(UiEvent::PinGroupDeleteEmpty)
        );
        assert_eq!(map_tray_signal("nope"), None);
    }

    /// 总线上的截图命令会变成带来源的收件箱事件。
    #[test]
    fn bus_capture_reaches_inbox() {
        let bus = CommandBus::new();
        let inbox = MainThreadInbox::new();
        register_bus_handlers(&bus, &inbox);
        let ctx = CommandContext::new(CommandSource::Hotkey);
        bus.emit(&ctx, AppCommand::Capture(CaptureRequest::default()))
            .unwrap();
        assert_eq!(
            inbox.try_recv(),
            Some(UiEvent::Capture {
                origin: ORIGIN_HOTKEY
            })
        );
        bus.emit(
            &CommandContext::new(CommandSource::Tray),
            AppCommand::Capture(CaptureRequest::default()),
        )
        .unwrap();
        assert_eq!(
            inbox.try_recv(),
            Some(UiEvent::Capture {
                origin: ORIGIN_TRAY
            })
        );
    }

    /// 总线上的导出与直接截图命令变成对应的收件箱事件。
    #[test]
    fn bus_export_and_direct_capture_reach_inbox() {
        use snow_app_core::command::SaveRequest;
        let bus = CommandBus::new();
        let inbox = MainThreadInbox::new();
        register_bus_handlers(&bus, &inbox);
        let ctx = CommandContext::new(CommandSource::Hotkey);
        let save = ExportTarget::Save(SaveRequest {
            path: Some("a.png".into()),
            ..SaveRequest::default()
        });
        bus.emit(&ctx, AppCommand::Export(save.clone())).unwrap();
        assert_eq!(inbox.try_recv(), Some(UiEvent::Export(save)));
        bus.emit(&ctx, AppCommand::Export(ExportTarget::Copy))
            .unwrap();
        assert_eq!(inbox.try_recv(), Some(UiEvent::Export(ExportTarget::Copy)));
        let direct = DirectCaptureRequest {
            target: DirectTarget::FocusedWindow,
            output: DirectOutput::Save,
            capture_cursor: None,
            scale: None,
            path: None,
            automatic_path: Some(true),
            format: None,
            quality: Some(80),
            compression_level: None,
            pdf_page_size: None,
            pdf_title: None,
        };
        bus.emit(&ctx, AppCommand::DirectCapture(direct.clone()))
            .unwrap();
        assert_eq!(inbox.try_recv(), Some(UiEvent::DirectCapture(direct)));
    }

    /// 总线上的 PinSelection 命令（热键 pin_clipboard_content）变成“从剪贴板贴图”事件。
    #[test]
    fn bus_pin_selection_reaches_inbox() {
        let bus = CommandBus::new();
        let inbox = MainThreadInbox::new();
        register_bus_handlers(&bus, &inbox);
        let ctx = CommandContext::new(CommandSource::Hotkey);
        bus.emit(&ctx, AppCommand::PinSelection).unwrap();
        assert_eq!(inbox.try_recv(), Some(UiEvent::PinFromClipboard));
    }

    /// 总线上的 OpenTranslateInput 命令（热键 translate_input）变成“打开输入框翻译”事件。
    #[test]
    fn bus_open_translate_input_reaches_inbox() {
        let bus = CommandBus::new();
        let inbox = MainThreadInbox::new();
        register_bus_handlers(&bus, &inbox);
        bus.emit(
            &CommandContext::new(CommandSource::Hotkey),
            AppCommand::OpenTranslateInput,
        )
        .unwrap();
        assert_eq!(inbox.try_recv(), Some(UiEvent::OpenTranslateInput));
    }

    /// 总线上的 QuickAction 命令变成对应的快捷动作事件，且携带的动作原样保留。
    #[test]
    fn bus_quick_action_reaches_inbox() {
        let bus = CommandBus::new();
        let inbox = MainThreadInbox::new();
        register_bus_handlers(&bus, &inbox);
        for action in [
            QuickAction::ScreenshotFullScreen,
            QuickAction::ToggleGlobalHotkeys,
        ] {
            bus.emit(
                &CommandContext::new(CommandSource::Hotkey),
                AppCommand::QuickAction(action),
            )
            .unwrap();
            assert_eq!(inbox.try_recv(), Some(UiEvent::QuickAction(action)));
        }
    }

    /// 新增的快捷动作键都被识别为热键配置键（变更后会重新注册），非热键的 global_shortcuts 键不会。
    #[test]
    fn quick_action_keys_trigger_reregistration() {
        for (key, _) in QUICK_ACTION_KEYS {
            assert_eq!(hotkey_config_key(key), Some(*key));
        }
        assert_eq!(
            hotkey_config_key(SCREENSHOT_HOTKEY_CONFIG_KEY),
            Some(SCREENSHOT_HOTKEY_CONFIG_KEY)
        );
        assert_eq!(
            hotkey_config_key("global_shortcuts/disable_on_focused_fullscreen_window"),
            None
        );
        assert_eq!(hotkey_config_key("screenshot/delay_seconds"), None);
    }

    /// 快捷动作的默认热键都能被解析（默认未绑定的为空列表，不会注册）。
    #[test]
    fn quick_action_default_hotkeys_parse() {
        let doc = ConfigDocument::from_bytes(None);
        for (key, _) in QUICK_ACTION_KEYS {
            for text in shortcut_strings(&doc.value(key)) {
                assert!(
                    Hotkey::parse(&portable_to_hotkey_text(&text)).is_ok(),
                    "{key}: {text}"
                );
            }
        }
    }

    /// 输入框翻译热键默认不绑定（因此不会注册）；绑定后能解析为合法热键。
    #[test]
    fn translate_input_hotkey_unbound_by_default() {
        let mut doc = ConfigDocument::from_bytes(None);
        assert!(shortcut_strings(&doc.value(TRANSLATE_INPUT_HOTKEY_CONFIG_KEY)).is_empty());
        doc.set_value(
            TRANSLATE_INPUT_HOTKEY_CONFIG_KEY,
            serde_json::json!(["Ctrl+Alt+T"]),
        )
        .unwrap();
        let list = shortcut_strings(&doc.value(TRANSLATE_INPUT_HOTKEY_CONFIG_KEY));
        assert_eq!(list.len(), 1);
        assert!(Hotkey::parse(&portable_to_hotkey_text(&list[0])).is_ok());
    }

    /// 总线上的三条听写命令分别变成对应的 UiEvent。
    #[test]
    fn bus_dictation_commands_reach_inbox() {
        let bus = CommandBus::new();
        let inbox = MainThreadInbox::new();
        register_bus_handlers(&bus, &inbox);
        for (command, expected) in [
            (AppCommand::ToggleDictation, DictationCommand::Toggle),
            (AppCommand::StartDictation, DictationCommand::Start),
            (AppCommand::StopDictation, DictationCommand::Stop),
        ] {
            bus.emit(&CommandContext::new(CommandSource::Hotkey), command)
                .unwrap();
            assert_eq!(inbox.try_recv(), Some(UiEvent::Dictation(expected)));
        }
    }

    /// 听写热键默认都不绑定；触发模式变更的失败判定只看两个听写热键。
    #[test]
    fn dictation_hotkeys_default_and_failure_attribution() {
        let doc = ConfigDocument::from_bytes(None);
        assert!(shortcut_strings(&doc.value(DICTATION_TOGGLE_HOTKEY_CONFIG_KEY)).is_empty());
        assert!(shortcut_strings(&doc.value(DICTATION_HOLD_HOTKEY_CONFIG_KEY)).is_empty());
        assert_eq!(
            DictationConfig::from_document(&doc).trigger,
            crate::dictation::config::TriggerMode::Both
        );

        let mut attempt = HotkeyRegistration::default();
        attempt.failures.push(HotkeyFailure {
            config_key: DICTATION_HOLD_HOTKEY_CONFIG_KEY,
            shortcut: "F9".into(),
            reason: "占用".into(),
        });
        assert!(hotkey_attempt_failed(
            &attempt,
            DICTATION_TRIGGER_MODE_CONFIG_KEY
        ));
        assert!(hotkey_attempt_failed(
            &attempt,
            DICTATION_HOLD_HOTKEY_CONFIG_KEY
        ));
        assert!(!hotkey_attempt_failed(
            &attempt,
            DICTATION_TOGGLE_HOTKEY_CONFIG_KEY
        ));
        assert!(
            describe_hotkey_failure(&attempt, DICTATION_TRIGGER_MODE_CONFIG_KEY).contains("F9")
        );
    }

    /// 贴图热键配置键与 schema 一致，默认值（F3）可解析为合法热键。
    #[test]
    fn pin_hotkey_key_exists_in_schema() {
        let doc = ConfigDocument::from_bytes(None);
        let list = shortcut_strings(&doc.value(PIN_CLIPBOARD_HOTKEY_CONFIG_KEY));
        assert!(!list.is_empty());
        for s in list {
            assert!(
                Hotkey::parse(&portable_to_hotkey_text(&s)).is_ok(),
                "默认热键无法解析: {s}"
            );
        }
    }

    /// 热键配置解析：忽略非字符串、空串与非数组。
    #[test]
    fn shortcut_parsing() {
        let v = serde_json::json!(["F1", " Ctrl+Alt+A ", "", 3, null, {"portable": "Shift+F2"}, {"portable": 5}]);
        assert_eq!(shortcut_strings(&v), vec!["F1", "Ctrl+Alt+A", "Shift+F2"]);
        assert!(shortcut_strings(&serde_json::json!("F1")).is_empty());
        assert!(shortcut_strings(&Value::Null).is_empty());
    }

    /// 默认配置里截图热键可被解析并转成合法 Hotkey。
    #[test]
    fn default_screenshot_hotkeys_parse() {
        let doc = ConfigDocument::from_bytes(None);
        let list = shortcut_strings(&doc.value(SCREENSHOT_HOTKEY_CONFIG_KEY));
        assert!(!list.is_empty());
        for s in list {
            assert!(Hotkey::parse(&s).is_ok(), "默认热键无法解析: {s}");
        }
    }

    /// 截图请求计数递增，序号从 1 开始。
    #[test]
    fn capture_counter_increments() {
        let dir =
            std::env::temp_dir().join(format!("snow-shot-cfg-counter-{}", std::process::id()));
        let mut state = AppState::new(
            open_shared_config(&dir),
            MainThreadInbox::new(),
            CapabilityRegistry::for_current_platform(),
            None,
            None,
            Vec::new(),
            &dir,
        );
        assert_eq!(on_capture_requested(&mut state, ORIGIN_IPC), 1);
        assert_eq!(on_capture_requested(&mut state, ORIGIN_HOTKEY), 2);
        assert_eq!(state.capture_requests(), 2);
    }

    /// 数据根下没有配置文件时回落到默认文档。
    #[test]
    fn missing_config_uses_defaults() {
        let dir = std::env::temp_dir().join(format!("snow-shot-cfg-none-{}", std::process::id()));
        let config = open_shared_config(&dir);
        let list = shortcut_strings(&config.borrow().value(SCREENSHOT_HOTKEY_CONFIG_KEY));
        assert!(!list.is_empty());
        assert!(!dir.exists(), "仅打开不应创建配置文件");
    }

    /// 事件收件箱先进先出，收件箱关闭后拒绝新事件。
    #[test]
    fn inbox_is_fifo_and_closable() {
        let inbox = MainThreadInbox::new();
        inbox.push(UiEvent::OpenSettings);
        inbox.push(UiEvent::Quit);
        assert_eq!(inbox.try_recv(), Some(UiEvent::OpenSettings));
        assert_eq!(inbox.try_recv(), Some(UiEvent::Quit));
        inbox.close();
        assert!(!inbox.push(UiEvent::Quit));
    }

    /// 截图请求闸门：采集中或覆盖窗未关时忽略，否则放行。
    #[test]
    fn capture_gate_rules() {
        assert_eq!(capture_gate(false, false), CaptureGate::Proceed);
        assert_eq!(capture_gate(true, false), CaptureGate::IgnoreInFlight);
        assert_eq!(capture_gate(false, true), CaptureGate::IgnoreOverlayOpen);
        assert_eq!(capture_gate(true, true), CaptureGate::IgnoreInFlight);
    }

    /// 录屏选区：显示器内坐标加上显示器原点（含负原点副屏），宽高不变，START 收到的是物理像素。
    #[test]
    fn recording_region_uses_desktop_physical_coordinates() {
        use snow_ui::shell::geometry::PhysicalRect;
        let r = monitor_local_to_desktop(
            PhysicalRect::new(10, 20, 300, 200),
            PhysicalRect::new(-1920, 100, 1920, 1080),
        );
        assert_eq!(r, PhysicalRect::new(-1910, 120, 300, 200));
        let p = monitor_local_to_desktop(
            PhysicalRect::new(0, 0, 2560, 1440),
            PhysicalRect::new(0, 0, 2560, 1600),
        );
        assert_eq!(p, PhysicalRect::new(0, 0, 2560, 1440));
    }

    /// 光标坐标换算为显示器内坐标（含负原点副屏），未知时取中心。
    #[test]
    fn cursor_relative_to_monitor() {
        use snow_ui::shell::geometry::{PhysicalRect, ScaleFactor};
        use snow_ui::shell::monitor::MonitorId;
        let m = MonitorInfo {
            id: MonitorId(2),
            name: String::new(),
            bounds: PhysicalRect::new(-1920, 200, 1920, 1080),
            work_area: PhysicalRect::new(-1920, 200, 1920, 1080),
            scale: ScaleFactor::ONE,
            is_primary: false,
        };
        assert_eq!(
            cursor_in_monitor(&m, Some(PhysicalPoint::new(-1900, 210))),
            PhysicalPoint::new(20, 10)
        );
        assert_eq!(cursor_in_monitor(&m, None), PhysicalPoint::new(960, 540));
    }

    /// 基准环境变量解析：合法值通过，非法 / 零值被拒绝。
    #[test]
    fn bench_env_parsing() {
        assert_eq!(parse_size("3840x2160"), Some((3840, 2160)));
        assert_eq!(parse_size(" 100 X 50 "), Some((100, 50)));
        assert_eq!(parse_size("0x10"), None);
        assert_eq!(parse_size("abc"), None);
        assert_eq!(parse_size("10x"), None);
        assert_eq!(parse_bench_steps(Some("300")), Some(300));
        assert_eq!(parse_bench_steps(Some("0")), None);
        assert_eq!(parse_bench_steps(Some("-1")), None);
        assert_eq!(parse_bench_steps(None), None);
    }

    /// 标注基准工具名解析：大小写不敏感，未知名称被拒绝。
    #[test]
    fn bench_tool_parsing() {
        assert_eq!(parse_bench_tool(Some("Arrow")), Some(AnnotationTool::Arrow));
        assert_eq!(
            parse_bench_tool(Some(" MOSAIC ")),
            Some(AnnotationTool::Mosaic)
        );
        assert_eq!(parse_bench_tool(Some("pen")), Some(AnnotationTool::Pencil));
        assert_eq!(parse_bench_tool(Some("text")), Some(AnnotationTool::Text));
        assert_eq!(parse_bench_tool(Some("laser")), None);
        assert_eq!(parse_bench_tool(None), None);
    }

    /// 合成底图尺寸与不透明度正确。
    #[test]
    fn synthetic_screen_is_opaque() {
        let s = synthetic_screen(32, 16);
        assert_eq!((s.width, s.height), (32, 16));
        assert_eq!(s.data.len(), 32 * 16 * 4);
        assert!(s.data.chunks_exact(4).all(|p| p[3] == 255));
    }

    /// 采集失败 / 完成事件可以经收件箱先进先出地传递。
    #[test]
    fn capture_events_flow_through_inbox() {
        let inbox = MainThreadInbox::new();
        inbox.push(UiEvent::CaptureFailed("x".into()));
        assert_eq!(inbox.try_recv(), Some(UiEvent::CaptureFailed("x".into())));
    }
}
