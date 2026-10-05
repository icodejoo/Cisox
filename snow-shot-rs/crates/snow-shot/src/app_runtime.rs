//! 常驻运行时：托盘、全局热键、单实例 IPC 的事件汇入 GPUI 主线程并分发。
//!
//! 数据流：热键 / 托盘 / IPC 线程 → [`UiEvent`] → [`MainThreadInbox`] → GPUI 主线程
//! [`handle_event`]。非主线程只做 `push`，不接触任何 GPUI 对象。
//!
//! 所属阶段：A（事件循环骨架）+ B1（截图触发 → 采集光标所在显示器 → 冻结覆盖窗）。
//! 采集在后台线程完成，结果经收件箱回到主线程再建窗。

use crate::capture_flow::{CapturePayload, pick_monitor, spawn_capture};
use crate::direct_capture::{DirectHistory, DirectResult, spawn_direct_capture};
use crate::history_store::{
    HistoryRecorder, HistorySource, HistoryStore, Thumbnail, policy_from_document,
};
use crate::history_view::{HistoryAction, HistoryView};
use crate::quick_actions::{
    DelayGate, DirectKind, QUICK_ACTION_KEYS, QuickPlan, clip_to_monitor, delay_seconds,
    direct_output_plan_from, full_monitor_region, plan_for, recording_directory, stays_registered_when_paused,
};
use crate::window_pick::{WindowHover, start_window_hover};
use crate::dictation::config::DictationConfig;
use crate::dictation::translate::TranslationOutcome;
use crate::dictation::focus::Verdict;
use crate::dictation::{DictationCommand, DictationHost};
use crate::frozen_frame::FrozenFrame;
use crate::ocr_assets::{ENV_OCR_ASSET_DIR, ocr_root};
use crate::ocr_client::OcrError;
use crate::ocr_download;
use crate::stt_download::{self, Progress as SttProgress};
use crate::stt_models;
use crate::stt_settings::{CancelFlag, SttHooks};
use crate::ocr_backend::{OcrInput, select_from_document};
use crate::ocr_service::{OcrRequestConfig, OcrResult, OcrService};
use crate::ort_runtime;
use crate::sys_prefs::system_ui_language;
use crate::translate_flow::TranslateUiState;
use crate::translate_input::{InputError, translate_text};
use crate::translate_input_view::{TranslateInputView, WINDOW_HEIGHT as TRANSLATE_INPUT_HEIGHT, WINDOW_WIDTH as TRANSLATE_INPUT_WIDTH};
use crate::translate_service::{
    TranslateConfig, TranslateFlowError, TranslateHost, TranslateOutcome, TranslateStage, Translated, run_flow,
};
use crate::overlay_view::{AutoConfirm, OverlayOutcome, ScreenshotOverlayView, SystemOutput};
use crate::pinned_manager::PinnedManager;
use crate::recording_flow::{
    ENV_RECORDING_AUTOTEST, RecordingHost, monitor_for_region, parse_autotest,
};
use snow_ui::widgets::{AnnotationTool, ToolbarAction};
use crate::screenshot_output::{
    ExportSettings, configured_format, export_direct, home_directory, resolve_save_directory,
};
use crate::scroll_view::{ENV_SCROLL_AUTOTEST, ScrollHost, parse_scroll_autotest};
use crate::settings_model::portable_to_hotkey_text;
use crate::settings_state::{ConfigChange, SharedConfig, SystemPrefs, UiPrefs, restore_value};
use crate::settings_model::{LANGUAGE_KEY, THEME_COLOR_KEY, THEME_MODE_KEY};
use crate::settings_text::{Lang, window_title};
use crate::settings_view::{AUTOTEST_STEP_INTERVAL, SettingsView, parse_autotest_ops};
use serde_json::Value;
use snow_app_core::bus::{CommandBus, CommandError, CommandOutcome};
use snow_app_core::command::{
    AppCommand, CaptureRequest, CommandKind, CommandSource, DirectCaptureRequest, DirectOutput,
    DirectTarget, ExportTarget, QuickAction, RecordingConfig as RecordingRequest,
};
use snow_i18n::Args;
use snow_capability::CapabilityRegistry;
use snow_config::document::ConfigDocument;
use snow_config::paths::config_file_path;
use snow_config::store::ConfigStore;
use snow_platform::single_instance::IpcCommand;
use snow_ui::shell::dispatch::Dispatcher;
use snow_ui::shell::geometry::{LogicalSize, PhysicalPoint, PhysicalRect};
use snow_ui::shell::hotkey::{Hotkey, HotkeyBinding, HotkeyHandle, HotkeyService};
use snow_ui::shell::inbox::MainThreadInbox;
use snow_ui::shell::monitor::{MonitorInfo, MonitorTarget};
use snow_ui::shell::overlay::cursor_screen_position;
use snow_ui::shell::tray::{
    TrayAction, TrayIconImage, TrayMenuEntry, TraySpec, TrayService,
};
use snow_ui::ui::{Entity, ShellContext, ShellWindow};
use snow_ui::shell::window::{Placement, WindowSpec};
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
pub(crate) const TRAY_TOOLTIP: &str = "Cisox";
/// 托盘信号：从剪贴板贴图。
pub const TRAY_SIGNAL_PIN_CLIPBOARD: &str = "pin_clipboard";
/// 托盘信号：打开截图历史。
pub const TRAY_SIGNAL_HISTORY: &str = "history";
/// 托盘信号：打开设置。
pub const TRAY_SIGNAL_SETTINGS: &str = "settings";
/// 托盘信号：退出。
pub const TRAY_SIGNAL_QUIT: &str = "quit";
/// 托盘占位图标边长（像素）。
const TRAY_ICON_SIZE: u32 = 32;
/// 托盘占位图标颜色（RGBA）。
const TRAY_ICON_RGBA: [u8; 4] = [22, 119, 255, 255];
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
pub const TRANSLATE_INPUT_HOTKEY_CONFIG_KEY: &str = snow_config::extensions::KEY_TRANSLATE_INPUT_HOTKEY;
/// “语音转文字·切换式”全局热键的配置键。
pub const DICTATION_TOGGLE_HOTKEY_CONFIG_KEY: &str = snow_config::extensions::KEY_DICTATION_TOGGLE_HOTKEY;
/// “语音转文字·按住说话”全局热键的配置键。
pub const DICTATION_HOLD_HOTKEY_CONFIG_KEY: &str = snow_config::extensions::KEY_DICTATION_HOLD_HOTKEY;
/// 语音转文字触发模式的配置键（决定上面两个热键哪个生效）。
pub const DICTATION_TRIGGER_MODE_CONFIG_KEY: &str = snow_config::extensions::KEY_DICTATION_TRIGGER_MODE;
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
    /// 打开（或激活）设置窗口。
    OpenSettings,
    /// 请求录制：进入选区，确认后拉起独立的录制进程。
    StartRecording,
    /// 导出命令（保存 / 复制），作用于当前覆盖窗里的选区。
    Export(ExportTarget),
    /// 直接截图：不进入选区，采集后直接复制或保存。
    DirectCapture(DirectCaptureRequest),
    /// 打开（或激活）输入框翻译浮窗。
    OpenTranslateInput,
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
        /// 结果。
        result: Result<(), String>,
    },
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
        // 目前唯一的窗口是设置窗，"唤醒主窗口"即打开/激活它
        IpcCommand::OpenSettings | IpcCommand::ShowMainWindow => Some(UiEvent::OpenSettings),
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
        TRAY_SIGNAL_SETTINGS => Some(UiEvent::OpenSettings),
        TRAY_SIGNAL_QUIT => Some(UiEvent::Quit),
        _ => None,
    }
}

/// 构造托盘描述：截图 / 录屏（命令）、剪贴板贴图 / 设置 / 退出（信号）。
///
/// # 参数
/// - `locale`：界面语料语言代码（如 `zh-CN`）。
///
/// # 返回
/// 托盘描述；图标数据非法返回错误文本（占位图标恒合法）。
///
/// ```ignore
/// let spec = build_tray_spec("zh-CN").unwrap();
/// assert_eq!(spec.menu.len(), 7);
/// ```
pub fn build_tray_spec(locale: &str) -> Result<TraySpec, String> {
    let i18n = crate::ocr_backend::i18n_for(locale);
    let icon = TrayIconImage::solid(TRAY_ICON_SIZE, TRAY_ICON_SIZE, TRAY_ICON_RGBA)
        .map_err(|e| e.to_string())?;
    let item = |key: &str, action: TrayAction| TrayMenuEntry::Item {
        label: i18n.tr(key),
        enabled: true,
        action,
    };
    Ok(TraySpec {
        tooltip: TRAY_TOOLTIP.to_string(),
        icon,
        menu: vec![
            item(
                "tray-capture",
                TrayAction::Command(AppCommand::Capture(CaptureRequest::default())),
            ),
            item(
                "tray-record",
                TrayAction::Command(AppCommand::StartRecording(RecordingRequest::default())),
            ),
            item(
                "tray-pin-clipboard",
                TrayAction::Signal(TRAY_SIGNAL_PIN_CLIPBOARD.into()),
            ),
            item("tray-history", TrayAction::Signal(TRAY_SIGNAL_HISTORY.into())),
            item("tray-settings", TrayAction::Signal(TRAY_SIGNAL_SETTINGS.into())),
            TrayMenuEntry::Separator,
            item("tray-quit", TrayAction::Signal(TRAY_SIGNAL_QUIT.into())),
        ],
        on_left_click: None,
        on_double_click: Some(TrayAction::Signal(TRAY_SIGNAL_SETTINGS.into())),
    })
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
                .filter_map(|v| v.as_str().or_else(|| v.get(PORTABLE_FIELD).and_then(Value::as_str)))
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
pub fn register_capture_hotkeys(service: &HotkeyService, document: &ConfigDocument) -> HotkeyRegistration {
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
pub fn register_recording_hotkeys(service: &HotkeyService, document: &ConfigDocument) -> HotkeyRegistration {
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
pub fn register_all_hotkeys(service: &HotkeyService, document: &ConfigDocument) -> HotkeyRegistration {
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
    /// 截图历史后台写入器（启动失败时为 `None`，历史功能降级）。
    history: Option<Arc<HistoryRecorder>>,
    /// 截图历史窗口（若已打开）与其视图。
    history_window: Option<(ShellWindow, Entity<HistoryView>)>,
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
    /// 当前覆盖窗的视图（OCR 结果回写用）。
    overlay_view: Option<Entity<ScreenshotOverlayView>>,
    /// 为即将打开的覆盖窗预先启动的窗口悬停来源（采集开始时抓窗口快照，覆盖窗打开时交给视图）。
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
        let translator = Arc::new(TranslateHost::new(data_root));
        Self {
            pins,
            ocr: Arc::new(OcrService::new(data_root)),
            translator: Arc::clone(&translator),
            data_root: data_root.to_path_buf(),
            overlay_view: None,
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
            history,
            history_window: None,
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
        }
    }

    /// 已收到的截图请求数。
    pub fn capture_requests(&self) -> u64 {
        self.capture_requests
    }

    /// 释放托盘与热键（移除图标、注销热键）。
    pub fn shutdown_services(&mut self) {
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
    let overlay_open = state
        .overlay
        .as_ref()
        .is_some_and(|window| cx.is_window_open(window));
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
    // 此刻覆盖窗尚未出现：先抓窗口快照，保证与冻结帧一致且不含 Cisox 自己的覆盖窗
    state.window_hover = start_window_hover(state.config.borrow().document(), monitor.bounds);
    let inbox = state.inbox.clone();
    // 基准合成底图模式：底图由 open_overlay 用合成渐变替换，不需要真实屏幕采集
    // （锁屏 / 屏保期间 GDI 采集必失败，此模式仍可跑标注性能基准）
    if std::env::var(ENV_OVERLAY_SYNTH).ok().and_then(|v| parse_size(&v)).is_some() {
        tracing::info!(seq, "性能基准：跳过真实屏幕采集");
        inbox.push(UiEvent::CaptureReady(CapturePayload {
            monitor,
            screen: snow_platform::capture::CapturedScreen::new_solid(1, 1, (0, 0, 0, u8::MAX)),
            elapsed: Duration::ZERO,
        }));
        return;
    }
    let spawned = spawn_capture(monitor, move |result| {
        inbox.push(match result {
            Ok(payload) => UiEvent::CaptureReady(payload),
            Err(e) => UiEvent::CaptureFailed(e),
        });
    });
    if let Err(e) = spawned {
        state.capture_in_flight = false;
        state.capture_mode = CaptureMode::Screenshot;
        tracing::error!(seq, error = %e, "启动采集线程失败");
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

/// 采集完成后在光标所在显示器上打开冻结覆盖窗。
///
/// # 参数
/// - `cx`：GPUI 外壳上下文。
/// - `state`：运行时状态。
/// - `payload`：采集结果。
fn open_overlay(cx: &mut ShellContext, state: &mut AppState, payload: CapturePayload) {
    state.capture_in_flight = false;
    let mode = std::mem::replace(&mut state.capture_mode, CaptureMode::Screenshot);
    let record_mode = mode == CaptureMode::Record;
    let scroll_mode = mode == CaptureMode::Scroll;
    let auto_confirm = match mode {
        CaptureMode::Quick(action) => Some(action),
        _ => None,
    };
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
    // 性能基准可用合成底图替换真实截图（例如在非 4K 屏上测 4K 纹理）
    let synth = std::env::var(ENV_OVERLAY_SYNTH)
        .ok()
        .and_then(|v| parse_size(&v));
    let mut scale_override = None;
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
    let cursor = cursor_in_monitor(&monitor, cursor_screen_position().ok());

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

    let mut spec = WindowSpec::overlay(MonitorTarget::Id(monitor.id));
    // 需要键盘（Esc / Enter / C），所以建窗时抢占焦点；底图不透明，无需窗口透明
    spec.focus = true;
    spec.transparent = false;
    let record_inbox = state.inbox.clone();
    let record_monitor = monitor.clone();
    let pin_inbox = state.inbox.clone();
    let pin_origin = monitor.bounds;
    let ocr_inbox = state.inbox.clone();
    let ocr_download_inbox = state.inbox.clone();
    let translate_inbox = state.inbox.clone();
    let translate_download_inbox = state.inbox.clone();
    let scroll_inbox = state.inbox.clone();
    let scroll_monitor = monitor.clone();
    let history_sink = state.history.clone();
    let history_config = Rc::clone(&state.config);
    let output = Box::new(
        SystemOutput::new(save_dir)
            .with_config(state.config.clone())
            .with_history(move |source, width, height, rgba| {
                if let Some(recorder) = &history_sink {
                    let policy = policy_from_document(history_config.borrow().document());
                    recorder.submit(policy, source, width, height, rgba);
                }
            })
            .with_recording(move |rect| {
                // 覆盖窗坐标以显示器左上角为原点，换算成虚拟桌面坐标
                record_inbox.push(UiEvent::RecordingRegionChosen {
                    region: monitor_local_to_desktop(rect, record_monitor.bounds),
                    monitor: record_monitor.clone(),
                });
            })
            .with_pin(move |rect, width, height, rgba| {
                // 贴图在选区原位打开：同样把显示器内坐标换算成虚拟桌面坐标
                pin_inbox.push(UiEvent::PinCreate {
                    rect: PhysicalRect::new(
                        rect.x + pin_origin.x,
                        rect.y + pin_origin.y,
                        rect.width,
                        rect.height,
                    ),
                    width,
                    height,
                    rgba,
                });
            })
            .with_ocr(move |serial, width, height, rgba| {
                ocr_inbox.push(UiEvent::OcrRequested { serial, width, height, rgba });
            })
            .with_ocr_download(move || {
                ocr_download_inbox.push(UiEvent::OcrDownloadRequested);
            })
            .with_translate(move |serial, width, height, rgba| {
                translate_inbox.push(UiEvent::TranslateRequested { serial, width, height, rgba });
            })
            .with_translate_download(move || {
                translate_download_inbox.push(UiEvent::TranslateDownloadRequested);
            })
            .with_scroll_capture(move |rect| {
                let bounds = scroll_monitor.bounds;
                scroll_inbox.push(UiEvent::ScrollRegionChosen {
                    region: PhysicalRect::new(rect.x + bounds.x, rect.y + bounds.y, rect.width, rect.height),
                    monitor: scroll_monitor.clone(),
                });
            }),
    );
    let opened = cx.open_window(&spec, move |window, app| {
        ScreenshotOverlayView::create(window, app, frame, cursor, output)
    });
    match opened {
        Ok((window, view)) => {
            tracing::info!(
                monitor = monitor.id.0,
                rect = ?monitor.bounds,
                hwnd = ?window.native_id().map(|id| id.0),
                "screenshot overlay opened"
            );
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
            // 标注样式：读取已保存的各工具样式，之后的修改写回同一份配置
            let style_locale = ui_prefs_from_document(state.config.borrow().document()).locale;
            let style_config = state.config.clone();
            view.update(cx.app(), |v, _| v.set_style_config(style_config, style_locale));
            if let Some(hover) = state.window_hover.take() {
                view.update(cx.app(), |v, _| v.set_window_hover(Some(hover)));
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
        }
        Err(e) => tracing::error!(error = %e, "打开截图覆盖窗失败"),
    }
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
    if request.scale.is_some_and(|s| (s - 1.0).abs() > f64::EPSILON) || request.capture_cursor == Some(true) {
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
    let spawned = std::thread::Builder::new().name("snow-ocr-request".into()).spawn(move || {
        let result = engine.recognize(&OcrInput { width, height, rgba: &rgba });
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
    let spawned = std::thread::Builder::new().name("snow-translate-request".into()).spawn(move || {
        let progress_inbox = inbox.clone();
        let result = run_flow(
            || ocr.recognize(&OcrInput { width, height, rgba: &rgba }),
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
            result: Err(TranslateFlowError::Ocr(OcrError::SpawnFailed(e.to_string()))),
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
    let spawned = std::thread::Builder::new().name("snow-ort-download".into()).spawn(move || {
        tracing::info!(root = %data_root.display(), "开始下载 onnxruntime 运行时");
        let cancel = AtomicBool::new(false);
        let progress_inbox = inbox.clone();
        let result = ort_runtime::install(&data_root, &cancel, |step| {
            progress_inbox.push(UiEvent::TranslateDownloadProgress(step.to_string()));
        })
        .map(|_| ());
        inbox.push(UiEvent::TranslateDownloadFinished(result));
    });
    if let Err(e) = spawned {
        state.inbox.push(UiEvent::TranslateDownloadFinished(Err(e.to_string())));
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
                progress_inbox.push(UiEvent::OcrDownloadProgress(step.to_string()));
            },
        );
        inbox.push(UiEvent::OcrDownloadFinished(result));
    });
    if let Err(e) = spawned {
        state.inbox.push(UiEvent::OcrDownloadFinished(Err(e.to_string())));
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
            None => Err(format!("unknown speech model: {id}")),
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
        state.inbox.push(UiEvent::SttDownloadFinished { model_id, result: Err(e.to_string()) });
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
        view.update(cx.app(), |v, _| v.bench_annotation_setup(tool.unwrap_or(AnnotationTool::None)));
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
    state.recording.begin(cx, spec.region, &monitor, Some(&spec));
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
    let notify_inbox = state.inbox.clone();
    let notify: Rc<dyn Fn(ConfigChange)> = Rc::new(move |change: ConfigChange| {
        notify_inbox.push(UiEvent::ConfigChanged {
            key: change.key.to_string(),
            previous: change.previous,
        });
    });
    match cx.open_window(&spec, move |window, app| {
        SettingsView::create(window, app, config, system, notify)
    }) {
        Ok((window, view)) => {
            state.settings = Some(window);
            state.settings_view = Some(view.clone());
            apply_chrome_theme(state);
            let stt_inbox = state.inbox.clone();
            let stt_hooks = SttHooks {
                data_root: state.data_root.clone(),
                request: std::sync::Arc::new(move |model_id, cancel| {
                    stt_inbox.push(UiEvent::SttDownloadRequested { model_id, cancel });
                }),
            };
            view.update(cx.app(), |v, _| v.set_stt_hooks(stt_hooks));
            tracing::info!("settings window opened");
            if let Ok(path) = std::env::var(ENV_SETTINGS_AUTOTEST) {
                spawn_settings_autotest(cx, window, view, &path);
            }
        }
        Err(e) => tracing::error!(error = %e, "打开设置窗口失败"),
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
        LogicalSize::new(crate::history_view::WINDOW_WIDTH, crate::history_view::WINDOW_HEIGHT),
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

/// 光标所在显示器作为输入框翻译浮窗的落点；取不到光标或显示器时用主屏。
///
/// # 参数
/// - `cx`：外壳上下文。
fn translate_input_monitor(cx: &ShellContext) -> MonitorTarget {
    let Ok(monitors) = cx.monitors() else {
        return MonitorTarget::Primary;
    };
    pick_monitor(&monitors, cursor_screen_position().ok()).map_or(MonitorTarget::Primary, |m| MonitorTarget::Id(m.id))
}

/// 读取界面偏好（深浅色、语言、主色），与设置页同一套解析。
///
/// # 参数
/// - `config`：共享配置。
pub(crate) fn ui_prefs_from_config(config: &SharedConfig) -> UiPrefs {
    let store = config.borrow();
    let text = |key: &str| store.value(key).as_str().unwrap_or_default().to_string();
    UiPrefs::resolve(&text(THEME_MODE_KEY), &text(LANGUAGE_KEY), &text(THEME_COLOR_KEY), &SystemPrefs::query())
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
        let translate_config = TranslateConfig::from_document(config.document(), &system_ui_language());
        crate::translate_input::installed_packs(&state.translator.scan(&translate_config).models)
    };
    let size = LogicalSize::new(TRANSLATE_INPUT_WIDTH, TRANSLATE_INPUT_HEIGHT);
    let spec = WindowSpec {
        title: String::new(),
        placement: Placement::Centered { monitor: translate_input_monitor(cx), size },
        transparent: false,
        always_on_top: true,
        decorations: false,
        show_in_taskbar: false,
        focus: true,
        resizable: false,
    };
    let inbox = state.inbox.clone();
    match cx.open_window(&spec, move |window, app| TranslateInputView::create(window, app, packs, prefs, inbox)) {
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
    let config = TranslateConfig::from_document(state.config.borrow().document(), &system_ui_language());
    let translator = Arc::clone(&state.translator);
    let inbox = state.inbox.clone();
    let spawned = std::thread::Builder::new().name("snow-translate-input".into()).spawn(move || {
        let result = translate_text(translator.as_ref(), &config, &model_id, &text);
        inbox.push(UiEvent::TranslateInputFinished { serial, result });
    });
    if let Err(e) = spawned {
        tracing::error!(error = %e, "无法创建输入框翻译线程");
        state.inbox.push(UiEvent::TranslateInputFinished {
            serial,
            result: Err(InputError::Translate(snow_translate::TranslateError::Io(e.to_string()))),
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
        .find(|m| m.name.to_ascii_uppercase().contains(&wanted.to_ascii_uppercase()))
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
                acx.background_executor().timer(AUTOTEST_STEP_INTERVAL).await;
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
const DICTATION_HOTKEY_KEYS: [&str; 2] = [DICTATION_TOGGLE_HOTKEY_CONFIG_KEY, DICTATION_HOLD_HOTKEY_CONFIG_KEY];

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

/// 按当前界面语言重建托盘菜单文案。
///
/// # 参数
/// - `state`：运行时状态。
fn refresh_tray_menu(state: &AppState) {
    let Some(tray) = state.tray.as_ref() else {
        return;
    };
    let locale = ui_prefs_from_document(state.config.borrow().document()).locale;
    match build_tray_spec(locale).and_then(|spec| tray.set_menu(spec.menu).map_err(|e| e.to_string())) {
        Ok(()) => tracing::info!(locale, "托盘菜单已按界面语言刷新"),
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

/// 设置页写入配置后的响应：全局热键类配置变更时重新注册并在失败时回滚。
///
/// # 参数
/// - `cx`：外壳上下文。
/// - `state`：运行时状态。
/// - `key`：变更的配置键。
/// - `previous`：变更前的值。
fn on_config_changed(cx: &mut ShellContext, state: &mut AppState, key: &str, previous: Value) {
    if key == LANGUAGE_KEY {
        refresh_tray_menu(state);
        return;
    }
    if key == THEME_MODE_KEY {
        apply_chrome_theme(state);
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
    let attempt = register_all_hotkeys_gated(service, state.config.borrow().document(), state.hotkeys_paused);
    if !hotkey_attempt_failed(&attempt, config_key) {
        let listing = service
            .registered()
            .map(|list| list.iter().map(|(_, h)| h.to_string()).collect::<Vec<_>>().join(", "))
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
    let restored = register_all_hotkeys_gated(service, state.config.borrow().document(), state.hotkeys_paused);
    tracing::info!(key, registered = restored.handles.len(), "已恢复回滚后的全局热键");
    state.hotkey_handles = restored.handles;
    if let Some(view) = state.settings_view.clone() {
        let message = format!("{}: {reason}", crate::settings_text::t(
            view.read(cx.app()).language(),
            crate::settings_text::Text::HotkeyRegisterFailed,
        ));
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
    PhysicalRect::new(rect.x + bounds.x, rect.y + bounds.y, rect.width, rect.height)
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
        QuickPlan::Overlay(auto) => request_capture(cx, state, ORIGIN_HOTKEY, CaptureMode::Quick(auto)),
        QuickPlan::Delayed => begin_delayed_capture(state),
        QuickPlan::OpenSettings => open_or_focus_settings(cx, state),
        QuickPlan::OpenHistory => open_or_focus_history(cx, state),
        QuickPlan::ToggleHotkeys => toggle_global_hotkeys(state),
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
            let text = notice_text(state, "quick-notice-delay-started", Args::new().arg(1, seconds));
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
    if state.direct_in_flight || capture_gate(state.capture_in_flight, overlay_open) != CaptureGate::Proceed {
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
        DirectKind::FullScreen => {
            pick_monitor(&monitors, cursor_screen_position().ok()).and_then(|m| full_monitor_region(&m))
        }
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
        (direct_output_plan_from(document), dir, policy_from_document(document))
    };
    let history = state.history.clone().map(|recorder| DirectHistory {
        recorder,
        policy,
        source: match kind {
            DirectKind::FullScreen => HistorySource::CurrentMonitor,
            DirectKind::FocusedWindow => HistorySource::FocusedWindow,
        },
    });
    tracing::info!(?kind, monitor = monitor.id.0, ?region, ?plan, "开始直接截图");
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
            notice_text(state, "quick-notice-direct-failed", Args::new().arg(1, reason))
        }
        Ok(r) if r.has_failure() => {
            let reason = r.failure_reason().unwrap_or_default();
            tracing::error!(%reason, "直接截图输出失败");
            notice_text(state, "quick-notice-direct-failed", Args::new().arg(1, reason))
        }
        Ok(r) => {
            match &r.saved {
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
            }
        }
    };
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
    state.hotkey_handles = attempt.handles;
    tracing::info!(paused, registered = state.hotkey_handles.len(), "全局热键暂停状态已切换");
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
        notice_text(state, "quick-notice-hotkeys-resume-failed", Args::new().arg(1, reasons))
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
    let dir = recording_directory(state.config.borrow().document(), home_directory().as_deref());
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
            let text = notice_text(state, "quick-notice-folder-failed", Args::new().arg(1, reason));
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
        UiEvent::CaptureReady(payload) => open_overlay(cx, state, payload),
        UiEvent::CaptureFailed(reason) => {
            state.capture_in_flight = false;
            state.capture_mode = CaptureMode::Screenshot;
            tracing::error!(%reason, "屏幕采集失败，未打开覆盖窗");
        }
        UiEvent::Export(target) => export_from_overlay(cx, state, &target),
        UiEvent::DirectCapture(request) => direct_capture(cx, state, request),
        UiEvent::OpenSettings => open_or_focus_settings(cx, state),
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
        UiEvent::HistoryPin { width, height, rgba } => {
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
        UiEvent::Dictation(command) => state.dictation.command(cx, state.tray.as_ref(), command),
        UiEvent::DictationPoll => state.dictation.tick(cx, state.tray.as_ref()),
        UiEvent::DictationProbed { round, verdict } => {
            state
                .dictation
                .probed(cx, state.tray.as_ref(), round, verdict)
        }
        UiEvent::DictationTranslated { round, seq, outcome } => {
            state.dictation.translated(cx, round, seq, outcome)
        }
        UiEvent::TranslateInputRequested { serial, text, model_id } => {
            spawn_translate_input(state, serial, text, model_id)
        }
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
        }
        UiEvent::PinClosed { id } => state.pins.forget(&id),
        UiEvent::OcrRequested {
            serial,
            width,
            height,
            rgba,
        } => spawn_ocr(state, serial, width, height, rgba),
        UiEvent::OcrFinished { serial, result } => {
            if let Some(view) = &state.overlay_view {
                view.update(cx.app(), |v, vcx| {
                    v.finish_ocr(serial, result);
                    vcx.notify();
                });
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
            let (auto, _) = parse_translate_auto(std::env::var(ENV_OVERLAY_BENCH_TRANSLATE_AUTO).ok().as_deref());
            if let Some(view) = &state.overlay_view {
                view.update(cx.app(), |v, vcx| {
                    v.finish_translate(serial, result);
                    if auto && matches!(v.translate_state(), TranslateUiState::Failed { can_download: true, .. }) {
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
            let (_, auto_retry) = parse_translate_auto(std::env::var(ENV_OVERLAY_BENCH_TRANSLATE_AUTO).ok().as_deref());
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
        UiEvent::SttDownloadRequested { model_id, cancel } => spawn_stt_download(state, model_id, cancel),
        UiEvent::SttDownloadProgress(progress) => {
            if let Some(view) = &state.settings_view {
                view.update(cx.app(), |v, vcx| v.update_stt_download(progress, vcx));
            }
        }
        UiEvent::SttDownloadFinished { model_id, result } => {
            match (stt_download::classify(&result), &result) {
                (stt_download::Outcome::Done, _) => tracing::info!(model = %model_id, "语音模型下载完成"),
                (stt_download::Outcome::Cancelled, _) => tracing::info!(model = %model_id, "用户取消下载"),
                (stt_download::Outcome::Failed, Err(e)) => {
                    tracing::warn!(model = %model_id, error = %e, "语音模型下载失败")
                }
                (stt_download::Outcome::Failed, Ok(())) => {}
            }
            if let Some(view) = &state.settings_view {
                view.update(cx.app(), |v, vcx| v.finish_stt_download(model_id, result, vcx));
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
        UiEvent::Quit => {
            tracing::info!("quit requested, shutting down");
            state.ocr.shutdown();
            state.translator.shutdown();
            state.dictation.shutdown();
            state.pins.persist_all(cx);
            state.shutdown_services();
            cx.quit();
        }
    }
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
) -> (Option<TrayService>, Option<HotkeyService>, Vec<HotkeyHandle>) {
    let prefs = ui_prefs_from_document(document);
    let locale = prefs.locale;
    apply_popup_menu_theme(prefs.dark);
    let tray = match build_tray_spec(locale)
        .and_then(|spec| TrayService::start(caps, spec, Dispatcher::from_bus(bus.clone())).map_err(|e| e.to_string()))
    {
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
            tracing::error!(error = %e, "托盘启动失败，将无托盘图标运行");
            None
        }
    };
    let mut handles = Vec::new();
    let hotkeys = match HotkeyService::start(caps, Dispatcher::from_bus(bus.clone())) {
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
        assert_eq!(map_ipc_command(&IpcCommand::TriggerRecording), Some(UiEvent::StartRecording));
        assert_eq!(map_ipc_command(&IpcCommand::ScrollCapture), Some(UiEvent::StartScrollCapture));
        assert_eq!(map_ipc_command(&IpcCommand::PinClipboard), Some(UiEvent::PinFromClipboard));
        assert_eq!(map_ipc_command(&IpcCommand::OpenSettings), Some(UiEvent::OpenSettings));
        assert_eq!(map_ipc_command(&IpcCommand::ShowMainWindow), Some(UiEvent::OpenSettings));
        assert_eq!(map_ipc_command(&IpcCommand::Quit), Some(UiEvent::Quit));
        assert_eq!(map_ipc_command(&IpcCommand::Custom("x".into())), None);
    }

    /// 托盘信号映射：已知信号有事件，未知信号忽略。
    #[test]
    fn tray_signal_mapping() {
        assert_eq!(map_tray_signal("settings"), Some(UiEvent::OpenSettings));
        assert_eq!(map_tray_signal("history"), Some(UiEvent::OpenHistory));
        assert_eq!(map_tray_signal("quit"), Some(UiEvent::Quit));
        assert_eq!(map_tray_signal("pin_clipboard"), Some(UiEvent::PinFromClipboard));
        assert_eq!(map_tray_signal("rm -rf"), None);
    }

    /// 托盘菜单：截图为命令，设置/退出为信号，退出前有分隔线。
    #[test]
    fn tray_spec_shape() {
        let spec = build_tray_spec("zh-CN").unwrap();
        assert_eq!(spec.menu.len(), 7);
        assert!(matches!(
            &spec.menu[0],
            TrayMenuEntry::Item { action: TrayAction::Command(AppCommand::Capture(_)), .. }
        ));
        assert!(matches!(
            &spec.menu[1],
            TrayMenuEntry::Item { action: TrayAction::Command(AppCommand::StartRecording(_)), .. }
        ));
        assert!(matches!(
            &spec.menu[2],
            TrayMenuEntry::Item { action: TrayAction::Signal(s), .. } if s == TRAY_SIGNAL_PIN_CLIPBOARD
        ));
        assert!(matches!(
            &spec.menu[3],
            TrayMenuEntry::Item { action: TrayAction::Signal(s), .. } if s == TRAY_SIGNAL_HISTORY
        ));
        assert!(matches!(
            &spec.menu[4],
            TrayMenuEntry::Item { action: TrayAction::Signal(s), .. } if s == TRAY_SIGNAL_SETTINGS
        ));
        assert!(matches!(&spec.menu[5], TrayMenuEntry::Separator));
        assert!(matches!(
            &spec.menu[6],
            TrayMenuEntry::Item { action: TrayAction::Signal(s), .. } if s == TRAY_SIGNAL_QUIT
        ));
    }

    /// 总线上的截图命令会变成带来源的收件箱事件。
    #[test]
    fn bus_capture_reaches_inbox() {
        let bus = CommandBus::new();
        let inbox = MainThreadInbox::new();
        register_bus_handlers(&bus, &inbox);
        let ctx = CommandContext::new(CommandSource::Hotkey);
        bus.emit(&ctx, AppCommand::Capture(CaptureRequest::default())).unwrap();
        assert_eq!(inbox.try_recv(), Some(UiEvent::Capture { origin: ORIGIN_HOTKEY }));
        bus.emit(&CommandContext::new(CommandSource::Tray), AppCommand::Capture(CaptureRequest::default()))
            .unwrap();
        assert_eq!(inbox.try_recv(), Some(UiEvent::Capture { origin: ORIGIN_TRAY }));
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
        bus.emit(&CommandContext::new(CommandSource::Hotkey), AppCommand::OpenTranslateInput).unwrap();
        assert_eq!(inbox.try_recv(), Some(UiEvent::OpenTranslateInput));
    }

    /// 总线上的 QuickAction 命令变成对应的快捷动作事件，且携带的动作原样保留。
    #[test]
    fn bus_quick_action_reaches_inbox() {
        let bus = CommandBus::new();
        let inbox = MainThreadInbox::new();
        register_bus_handlers(&bus, &inbox);
        for action in [QuickAction::ScreenshotFullScreen, QuickAction::ToggleGlobalHotkeys] {
            bus.emit(&CommandContext::new(CommandSource::Hotkey), AppCommand::QuickAction(action))
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
        assert_eq!(hotkey_config_key(SCREENSHOT_HOTKEY_CONFIG_KEY), Some(SCREENSHOT_HOTKEY_CONFIG_KEY));
        assert_eq!(hotkey_config_key("global_shortcuts/disable_on_focused_fullscreen_window"), None);
        assert_eq!(hotkey_config_key("screenshot/delay_seconds"), None);
    }

    /// 快捷动作的默认热键都能被解析（默认未绑定的为空列表，不会注册）。
    #[test]
    fn quick_action_default_hotkeys_parse() {
        let doc = ConfigDocument::from_bytes(None);
        for (key, _) in QUICK_ACTION_KEYS {
            for text in shortcut_strings(&doc.value(key)) {
                assert!(Hotkey::parse(&portable_to_hotkey_text(&text)).is_ok(), "{key}: {text}");
            }
        }
    }

    /// 输入框翻译热键默认不绑定（因此不会注册）；绑定后能解析为合法热键。
    #[test]
    fn translate_input_hotkey_unbound_by_default() {
        let mut doc = ConfigDocument::from_bytes(None);
        assert!(shortcut_strings(&doc.value(TRANSLATE_INPUT_HOTKEY_CONFIG_KEY)).is_empty());
        doc.set_value(TRANSLATE_INPUT_HOTKEY_CONFIG_KEY, serde_json::json!(["Ctrl+Alt+T"])).unwrap();
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
        assert!(hotkey_attempt_failed(&attempt, DICTATION_TRIGGER_MODE_CONFIG_KEY));
        assert!(hotkey_attempt_failed(&attempt, DICTATION_HOLD_HOTKEY_CONFIG_KEY));
        assert!(!hotkey_attempt_failed(&attempt, DICTATION_TOGGLE_HOTKEY_CONFIG_KEY));
        assert!(describe_hotkey_failure(&attempt, DICTATION_TRIGGER_MODE_CONFIG_KEY).contains("F9"));
    }

    /// 贴图热键配置键与 schema 一致，默认值（F3）可解析为合法热键。
    #[test]
    fn pin_hotkey_key_exists_in_schema() {
        let doc = ConfigDocument::from_bytes(None);
        let list = shortcut_strings(&doc.value(PIN_CLIPBOARD_HOTKEY_CONFIG_KEY));
        assert!(!list.is_empty());
        for s in list {
            assert!(Hotkey::parse(&portable_to_hotkey_text(&s)).is_ok(), "默认热键无法解析: {s}");
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
        let dir = std::env::temp_dir().join(format!("snow-shot-cfg-counter-{}", std::process::id()));
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
        let r = monitor_local_to_desktop(PhysicalRect::new(10, 20, 300, 200), PhysicalRect::new(-1920, 100, 1920, 1080));
        assert_eq!(r, PhysicalRect::new(-1910, 120, 300, 200));
        let p = monitor_local_to_desktop(PhysicalRect::new(0, 0, 2560, 1440), PhysicalRect::new(0, 0, 2560, 1600));
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
        assert_eq!(parse_bench_tool(Some(" MOSAIC ")), Some(AnnotationTool::Mosaic));
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
