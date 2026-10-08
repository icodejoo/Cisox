//! 全局快捷动作的纯逻辑：配置键与动作的映射、执行方案、延迟状态机、直接截图的区域与输出规则。
//!
//! 这里不接触 GPUI 与系统调用，全部可离屏测试；真正的执行在 `app_runtime` 与 `direct_capture`。

use crate::overlay_view::AutoConfirm;
use serde_json::Value;
use snow_app_core::command::QuickAction;
use snow_config::document::ConfigDocument;
use snow_ui::shell::geometry::{PhysicalPoint, PhysicalRect};
use snow_ui::shell::monitor::{MonitorInfo, Monitors};
use std::path::{Path, PathBuf};

/// 延迟截图秒数的配置键。
pub const DELAY_SECONDS_CONFIG_KEY: &str = "screenshot/delay_seconds";
/// 延迟截图秒数的默认值。
pub const DEFAULT_DELAY_SECONDS: u64 = 3;
/// 延迟截图秒数下限。
pub const MIN_DELAY_SECONDS: u64 = 1;
/// 延迟截图秒数上限。
pub const MAX_DELAY_SECONDS: u64 = 10;
/// “暂停全部热键”开关本身的配置键（暂停期间它仍然生效，否则无法恢复）。
pub const TOGGLE_HOTKEYS_CONFIG_KEY: &str = "global_shortcuts/toggle_global_hotkeys";
/// 自动保存开关（直接截图沿用旧版：开启则复制之外还落盘）。
pub const AUTO_SAVE_CONFIG_KEY: &str = "screenshot/auto_save_after_copy";
/// “复制图片文件到剪贴板”开关（开启时同样需要落盘）。
pub const COPY_FILE_CONFIG_KEY: &str = "screenshot/copy_image_file_to_clipboard";
/// 录屏保存目录的配置键。
pub const RECORDING_DIR_CONFIG_KEY: &str = "screen_recording/video_save_directory";
/// 默认录屏子目录名（位于用户目录下）。
const VIDEOS_DIR_NAME: &str = "Videos";

/// 快捷动作对应的全局热键配置键表（不含截图 / 录屏 / 贴图剪贴板 / 翻译浮窗 / 语音，它们另有注册路径）。
pub const QUICK_ACTION_KEYS: &[(&str, QuickAction)] = &[
    (
        "global_shortcuts/screenshot_delay",
        QuickAction::ScreenshotDelay,
    ),
    (
        "global_shortcuts/screenshot_fixed",
        QuickAction::ScreenshotFixed,
    ),
    (
        "global_shortcuts/screenshot_ocr",
        QuickAction::ScreenshotOcr,
    ),
    (
        "global_shortcuts/screenshot_translation",
        QuickAction::ScreenshotTranslation,
    ),
    (
        "global_shortcuts/screenshot_copy",
        QuickAction::ScreenshotCopy,
    ),
    (
        "global_shortcuts/screenshot_full_screen",
        QuickAction::ScreenshotFullScreen,
    ),
    (
        "global_shortcuts/screenshot_focused_window",
        QuickAction::ScreenshotFocusedWindow,
    ),
    (
        "global_shortcuts/screen_record_copy",
        QuickAction::ScreenRecordCopy,
    ),
    (
        "global_shortcuts/open_screen_recording_folder",
        QuickAction::OpenScreenRecordingFolder,
    ),
    (
        "global_shortcuts/open_capture_history",
        QuickAction::OpenCaptureHistory,
    ),
    (
        "global_shortcuts/open_pin_to_screen_management",
        QuickAction::OpenPinManagement,
    ),
    ("global_shortcuts/open_settings", QuickAction::OpenSettings),
    (
        "global_shortcuts/translate_selected_text",
        QuickAction::TranslateSelectedText,
    ),
    (
        "global_shortcuts/pin_selected_files",
        QuickAction::PinSelectedFiles,
    ),
    (
        "global_shortcuts/restore_last_closed_windows",
        QuickAction::RestoreLastClosedWindows,
    ),
    (TOGGLE_HOTKEYS_CONFIG_KEY, QuickAction::ToggleGlobalHotkeys),
    (
        "global_shortcuts/toggle_disable_on_focused_fullscreen_window",
        QuickAction::ToggleDisableOnFocusedFullscreen,
    ),
];

/// 直接截图的目标。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DirectKind {
    /// 光标所在显示器整屏。
    FullScreen,
    /// 前台窗口。
    FocusedWindow,
}

/// 快捷动作的执行方案。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QuickPlan {
    /// 不进覆盖层，直接截图并按设置输出。
    Direct(DirectKind),
    /// 进入覆盖层，框选完成后自动执行指定动作。
    Overlay(AutoConfirm),
    /// 等待设定秒数后进入普通截图。
    Delayed,
    /// 打开（或激活）设置窗口。
    OpenSettings,
    /// 打开（或激活）截图历史窗口。
    OpenHistory,
    /// 打开（或激活）贴图管理窗口。
    OpenPinManage,
    /// 把前台资源管理器 / 桌面里选中的图片文件贴到屏幕。
    PinSelectedFiles,
    /// 恢复最近关闭的一张贴图。
    RestoreClosed,
    /// 录屏，完成后把录制文件复制到剪贴板。
    RecordAndCopy,
    /// 读取前台应用选中的文字并翻译。
    TranslateSelected,
    /// 暂停 / 恢复全部全局热键。
    ToggleHotkeys,
    /// 切换「前台全屏窗口时停用热键」。
    ToggleFullscreenGate,
    /// 打开录屏保存目录。
    OpenRecordingFolder,
    /// 暂无实现：只给出本地化提示（携带提示消息 id）。
    Placeholder(&'static str),
}

/// 提示消息 id：没有读取到选中的文字。
pub const NOTICE_NO_SELECTED_TEXT: &str = "quick-notice-no-selected-text";
/// 提示消息 id：没有选中的图片文件。
pub const NOTICE_PIN_SELECTED_FILES: &str = "quick-notice-pin-selected-files";
/// 提示消息 id：没有可恢复的最近关闭贴图。
pub const NOTICE_RESTORE_CLOSED: &str = "quick-notice-restore-closed";
/// 提示消息 id：前台全屏停用热键已打开。
pub const NOTICE_FULLSCREEN_GATE_ON: &str = "quick-notice-fullscreen-gate-on";
/// 提示消息 id：前台全屏停用热键已关闭。
pub const NOTICE_FULLSCREEN_GATE_OFF: &str = "quick-notice-fullscreen-gate-off";

/// 由热键配置键查快捷动作；不在表内返回 `None`。
///
/// # 参数
/// - `key`：配置键。
///
/// ```ignore
/// assert_eq!(action_for_key("global_shortcuts/open_settings"), Some(QuickAction::OpenSettings));
/// ```
pub fn action_for_key(key: &str) -> Option<QuickAction> {
    QUICK_ACTION_KEYS
        .iter()
        .find(|(k, _)| *k == key)
        .map(|(_, a)| *a)
}

/// 快捷动作的执行方案。
///
/// # 参数
/// - `action`：快捷动作。
///
/// ```ignore
/// assert_eq!(plan_for(QuickAction::ScreenshotCopy), QuickPlan::Overlay(AutoConfirm::Copy));
/// ```
pub fn plan_for(action: QuickAction) -> QuickPlan {
    match action {
        QuickAction::ScreenshotDelay => QuickPlan::Delayed,
        QuickAction::ScreenshotFixed => QuickPlan::Overlay(AutoConfirm::Pin),
        QuickAction::ScreenshotOcr => QuickPlan::Overlay(AutoConfirm::Ocr),
        QuickAction::ScreenshotTranslation => QuickPlan::Overlay(AutoConfirm::Translate),
        QuickAction::ScreenshotCopy => QuickPlan::Overlay(AutoConfirm::Copy),
        QuickAction::ScreenshotFullScreen => QuickPlan::Direct(DirectKind::FullScreen),
        QuickAction::ScreenshotFocusedWindow => QuickPlan::Direct(DirectKind::FocusedWindow),
        QuickAction::OpenScreenRecordingFolder => QuickPlan::OpenRecordingFolder,
        QuickAction::OpenSettings => QuickPlan::OpenSettings,
        QuickAction::ToggleGlobalHotkeys => QuickPlan::ToggleHotkeys,
        QuickAction::OpenCaptureHistory => QuickPlan::OpenHistory,
        QuickAction::OpenPinManagement => QuickPlan::OpenPinManage,
        QuickAction::TranslateSelectedText => QuickPlan::TranslateSelected,
        QuickAction::PinSelectedFiles => QuickPlan::PinSelectedFiles,
        QuickAction::RestoreLastClosedWindows => QuickPlan::RestoreClosed,
        QuickAction::ScreenRecordCopy => QuickPlan::RecordAndCopy,
        QuickAction::ToggleDisableOnFocusedFullscreen => QuickPlan::ToggleFullscreenGate,
    }
}

/// 热键被暂停时，某个配置键是否仍应保持注册（只有“暂停 / 恢复”开关本身）。
///
/// # 参数
/// - `key`：热键配置键。
pub fn stays_registered_when_paused(key: &str) -> bool {
    key == TOGGLE_HOTKEYS_CONFIG_KEY
}

/// 读取延迟截图秒数，限制在 `[1, 10]`；缺失或类型不对用默认值。
///
/// # 参数
/// - `document`：配置文档。
///
/// ```ignore
/// assert_eq!(delay_seconds(&doc), 3);
/// ```
pub fn delay_seconds(document: &ConfigDocument) -> u64 {
    delay_seconds_from(&document.value(DELAY_SECONDS_CONFIG_KEY))
}

/// 把配置值换算成延迟秒数（越界夹取，非整数用默认值）。
///
/// # 参数
/// - `value`：配置值。
pub fn delay_seconds_from(value: &Value) -> u64 {
    value
        .as_u64()
        .map(|s| s.clamp(MIN_DELAY_SECONDS, MAX_DELAY_SECONDS))
        .unwrap_or(DEFAULT_DELAY_SECONDS)
}

/// 延迟截图的倒计时状态机：同一时刻只允许一个倒计时，过期的定时器回调会被丢弃。
#[derive(Debug, Default)]
pub struct DelayGate {
    /// 下一个倒计时序号。
    next_serial: u64,
    /// 进行中的倒计时序号。
    pending: Option<u64>,
}

impl DelayGate {
    /// 开始一次倒计时。
    ///
    /// # 返回
    /// 倒计时序号；已有倒计时在进行则返回 `None`（忽略重复触发）。
    ///
    /// ```ignore
    /// let serial = gate.begin().unwrap();
    /// ```
    pub fn begin(&mut self) -> Option<u64> {
        if self.pending.is_some() {
            return None;
        }
        self.next_serial += 1;
        self.pending = Some(self.next_serial);
        self.pending
    }

    /// 倒计时到点。
    ///
    /// # 参数
    /// - `serial`：到点的倒计时序号。
    ///
    /// # 返回
    /// 序号匹配（应当继续截图）返回 `true`，已被取消或过期返回 `false`。
    pub fn fire(&mut self, serial: u64) -> bool {
        if self.pending == Some(serial) {
            self.pending = None;
            true
        } else {
            false
        }
    }

    /// 取消进行中的倒计时。
    pub fn cancel(&mut self) {
        self.pending = None;
    }

    /// 是否有倒计时在进行。
    pub fn is_pending(&self) -> bool {
        self.pending.is_some()
    }
}

/// 直接截图的输出方案。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DirectOutputPlan {
    /// 是否复制到剪贴板（恒为真）。
    pub copy: bool,
    /// 是否另存为文件。
    pub save: bool,
}

/// 按设置决定直接截图的输出：恒复制；开启自动保存或“复制图片文件”时同时落盘。
///
/// # 参数
/// - `auto_save`：`screenshot/auto_save_after_copy`。
/// - `copy_file`：`screenshot/copy_image_file_to_clipboard`。
///
/// ```ignore
/// assert!(direct_output_plan(true, false).save);
/// ```
pub fn direct_output_plan(auto_save: bool, copy_file: bool) -> DirectOutputPlan {
    DirectOutputPlan {
        copy: true,
        save: auto_save || copy_file,
    }
}

/// 从配置文档读取直接截图的输出方案。
///
/// # 参数
/// - `document`：配置文档。
pub fn direct_output_plan_from(document: &ConfigDocument) -> DirectOutputPlan {
    let flag = |key: &str| document.value(key).as_bool().unwrap_or(false);
    direct_output_plan(flag(AUTO_SAVE_CONFIG_KEY), flag(COPY_FILE_CONFIG_KEY))
}

/// 录屏保存目录：优先取配置，为空则退回“用户目录/Videos”，都没有则用系统临时目录。
///
/// # 参数
/// - `document`：配置文档。
/// - `home`：用户目录。
///
/// ```ignore
/// let dir = recording_directory(&doc, Some(Path::new("C:/Users/a")));
/// ```
pub fn recording_directory(document: &ConfigDocument, home: Option<&Path>) -> PathBuf {
    if let Value::String(text) = document.value(RECORDING_DIR_CONFIG_KEY) {
        let text = text.trim();
        if !text.is_empty() {
            return PathBuf::from(text);
        }
    }
    match home {
        Some(home) => home.join(VIDEOS_DIR_NAME),
        None => std::env::temp_dir(),
    }
}

/// 一次直接截图的采集区域：所在显示器与桌面物理坐标下的 `(x, y, 宽, 高)`。
pub type DirectRegion = (MonitorInfo, (i32, i32, u32, u32));

/// 把窗口外框归到它主要所在的显示器并裁到该显示器范围内。
///
/// 先取外框中心所在显示器，中心在屏外时取重叠面积最大者；完全不重叠返回 `None`。
///
/// # 参数
/// - `rect`：窗口外框 `(x, y, 宽, 高)`（桌面物理坐标）。
/// - `monitors`：显示器快照。
///
/// ```ignore
/// let (monitor, region) = clip_to_monitor((100, 100, 800, 600), &monitors).unwrap();
/// ```
pub fn clip_to_monitor(rect: (i32, i32, i32, i32), monitors: &Monitors) -> Option<DirectRegion> {
    let window = PhysicalRect::new(rect.0, rect.1, rect.2, rect.3);
    let center = PhysicalPoint::new(rect.0 + rect.2 / 2, rect.1 + rect.3 / 2);
    let overlap_area = |m: &MonitorInfo| {
        m.bounds
            .intersect(&window)
            .map(|r| i64::from(r.width) * i64::from(r.height))
            .unwrap_or(0)
    };
    let monitor = monitors
        .at_point(center)
        .or_else(|| monitors.all().iter().max_by_key(|m| overlap_area(m)))?;
    let clipped = monitor.bounds.intersect(&window)?;
    Some((
        monitor.clone(),
        (
            clipped.x,
            clipped.y,
            clipped.width as u32,
            clipped.height as u32,
        ),
    ))
}

/// 整个显示器范围对应的采集区域；范围非法返回 `None`。
///
/// # 参数
/// - `monitor`：目标显示器。
pub fn full_monitor_region(monitor: &MonitorInfo) -> Option<DirectRegion> {
    crate::capture_flow::capture_region(monitor).map(|r| (monitor.clone(), r))
}

#[cfg(test)]
mod tests {
    use super::*;
    use snow_ui::shell::geometry::ScaleFactor;
    use snow_ui::shell::monitor::MonitorId;
    use std::collections::HashSet;

    /// 构造测试用显示器。
    fn monitor(id: u64, bounds: PhysicalRect, primary: bool) -> MonitorInfo {
        MonitorInfo {
            id: MonitorId(id),
            name: format!("M{id}"),
            bounds,
            work_area: bounds,
            scale: ScaleFactor::ONE,
            is_primary: primary,
        }
    }

    /// 双屏：主屏在原点，副屏在左侧（负坐标）。
    fn two() -> Monitors {
        Monitors::from_list(vec![
            monitor(1, PhysicalRect::new(0, 0, 2560, 1440), true),
            monitor(2, PhysicalRect::new(-1920, 200, 1920, 1080), false),
        ])
    }

    /// 键表覆盖全部快捷动作且键、动作都不重复，且每个键都在配置 schema 里。
    #[test]
    fn key_table_is_complete_and_unique() {
        let keys: HashSet<_> = QUICK_ACTION_KEYS.iter().map(|(k, _)| *k).collect();
        let actions: HashSet<_> = QUICK_ACTION_KEYS.iter().map(|(_, a)| *a).collect();
        assert_eq!(keys.len(), QUICK_ACTION_KEYS.len());
        assert_eq!(actions.len(), 17);
        let doc = ConfigDocument::from_bytes(None);
        for key in keys {
            assert!(doc.value(key).is_array(), "{key} 应是热键列表");
        }
    }

    /// 配置键 → 动作 → 方案的映射。
    #[test]
    fn plans_follow_legacy_semantics() {
        assert_eq!(
            action_for_key("global_shortcuts/screenshot_full_screen"),
            Some(QuickAction::ScreenshotFullScreen)
        );
        assert_eq!(action_for_key("global_shortcuts/screenshot"), None);
        assert_eq!(
            plan_for(QuickAction::ScreenshotFullScreen),
            QuickPlan::Direct(DirectKind::FullScreen)
        );
        assert_eq!(
            plan_for(QuickAction::ScreenshotFocusedWindow),
            QuickPlan::Direct(DirectKind::FocusedWindow)
        );
        assert_eq!(
            plan_for(QuickAction::ScreenshotCopy),
            QuickPlan::Overlay(AutoConfirm::Copy)
        );
        assert_eq!(
            plan_for(QuickAction::ScreenshotFixed),
            QuickPlan::Overlay(AutoConfirm::Pin)
        );
        assert_eq!(
            plan_for(QuickAction::ScreenshotOcr),
            QuickPlan::Overlay(AutoConfirm::Ocr)
        );
        assert_eq!(
            plan_for(QuickAction::ScreenshotTranslation),
            QuickPlan::Overlay(AutoConfirm::Translate)
        );
        assert_eq!(plan_for(QuickAction::ScreenshotDelay), QuickPlan::Delayed);
        assert_eq!(plan_for(QuickAction::OpenSettings), QuickPlan::OpenSettings);
        assert_eq!(
            plan_for(QuickAction::ToggleGlobalHotkeys),
            QuickPlan::ToggleHotkeys
        );
        assert_eq!(
            plan_for(QuickAction::OpenCaptureHistory),
            QuickPlan::OpenHistory
        );
    }

    /// 暂停期间只有开关自身保持注册。
    #[test]
    fn only_toggle_survives_pause() {
        let alive: Vec<_> = QUICK_ACTION_KEYS
            .iter()
            .filter(|(k, _)| stays_registered_when_paused(k))
            .map(|(_, a)| *a)
            .collect();
        assert_eq!(alive, vec![QuickAction::ToggleGlobalHotkeys]);
    }

    /// 延迟秒数：默认 3，越界夹取，类型不对退回默认。
    #[test]
    fn delay_seconds_clamped() {
        assert_eq!(delay_seconds(&ConfigDocument::from_bytes(None)), 3);
        assert_eq!(delay_seconds_from(&serde_json::json!(0)), 1);
        assert_eq!(delay_seconds_from(&serde_json::json!(99)), 10);
        assert_eq!(delay_seconds_from(&serde_json::json!(5)), 5);
        assert_eq!(delay_seconds_from(&serde_json::json!("x")), 3);
        assert_eq!(delay_seconds_from(&serde_json::json!(-2)), 3);
    }

    /// 倒计时状态机：重复触发被忽略，过期回调被丢弃，取消后可重新开始。
    #[test]
    fn delay_gate_state_machine() {
        let mut gate = DelayGate::default();
        let first = gate.begin().unwrap();
        assert!(gate.is_pending());
        assert_eq!(gate.begin(), None, "进行中忽略重复触发");
        assert!(!gate.fire(first + 1), "序号不符不触发");
        assert!(gate.fire(first));
        assert!(!gate.fire(first), "只触发一次");
        let second = gate.begin().unwrap();
        assert_ne!(first, second);
        gate.cancel();
        assert!(!gate.fire(second), "取消后旧定时器作废");
        assert!(gate.begin().is_some());
    }

    /// 输出方案：恒复制；自动保存或复制文件时落盘。
    #[test]
    fn output_plan_branches() {
        assert_eq!(
            direct_output_plan(false, false),
            DirectOutputPlan {
                copy: true,
                save: false
            }
        );
        assert!(direct_output_plan(true, false).save);
        assert!(direct_output_plan(false, true).save);
        assert_eq!(
            direct_output_plan_from(&ConfigDocument::from_bytes(None)),
            DirectOutputPlan {
                copy: true,
                save: false
            }
        );
    }

    /// 窗口落在副屏（负坐标）时归到副屏，跨屏窗口裁到中心所在屏。
    #[test]
    fn clip_picks_monitor_and_clips() {
        let ms = two();
        let (m, region) = clip_to_monitor((-1500, 300, 800, 600), &ms).unwrap();
        assert_eq!(m.id, MonitorId(2));
        assert_eq!(region, (-1500, 300, 800, 600));
        // 中心在主屏，左侧超出主屏左边缘的部分被裁掉
        let (m, region) = clip_to_monitor((-100, 100, 1000, 500), &ms).unwrap();
        assert_eq!(m.id, MonitorId(1));
        assert_eq!(region, (0, 100, 900, 500));
    }

    /// 中心在屏外（副屏上方空白）时取重叠最大的显示器；完全在屏外返回 None。
    #[test]
    fn clip_handles_center_outside_and_offscreen() {
        let ms = two();
        let (m, _) = clip_to_monitor((-1000, -300, 800, 600), &ms).unwrap();
        assert_eq!(m.id, MonitorId(2));
        assert!(clip_to_monitor((5000, 5000, 100, 100), &ms).is_none());
        assert!(clip_to_monitor((0, 0, 100, 100), &Monitors::default()).is_none());
    }

    /// 所有提示消息在两种语言下都能严格解析（含带参消息）。
    #[test]
    fn notice_messages_resolve_in_both_locales() {
        let plain = [
            NOTICE_NO_SELECTED_TEXT,
            NOTICE_PIN_SELECTED_FILES,
            NOTICE_RESTORE_CLOSED,
            NOTICE_FULLSCREEN_GATE_ON,
            NOTICE_FULLSCREEN_GATE_OFF,
            "quick-notice-hotkeys-paused",
            "quick-notice-hotkeys-resumed",
            "quick-notice-delay-busy",
            "quick-notice-no-focused-window",
            "quick-notice-capture-busy",
            "tray-tooltip-paused",
        ];
        let with_args = [
            "quick-notice-hotkeys-resume-failed",
            "quick-notice-delay-started",
            "quick-notice-direct-copied",
            "quick-notice-direct-saved",
            "quick-notice-direct-failed",
            "quick-notice-folder-failed",
        ];
        for locale in ["en-US", "zh-CN"] {
            let i18n = crate::ocr_backend::i18n_for(locale);
            for id in plain {
                assert!(
                    i18n.tr_checked(id, &snow_i18n::Args::new()).is_ok(),
                    "{locale} {id}"
                );
            }
            let args = snow_i18n::Args::new().arg(1, "x").arg(2, "y");
            for id in with_args {
                let text = i18n
                    .tr_checked(id, &args)
                    .unwrap_or_else(|e| panic!("{locale} {id}: {e:?}"));
                assert!(text.contains('x'), "{locale} {id} 应带上参数");
            }
        }
    }

    /// 录屏目录：配置优先，空配置退回用户目录 / Videos，无用户目录退回临时目录。
    #[test]
    fn recording_directory_fallbacks() {
        let mut doc = ConfigDocument::from_bytes(None);
        doc.set_value(RECORDING_DIR_CONFIG_KEY, Value::String(String::new()))
            .unwrap();
        let home = Path::new("C:/Users/a");
        assert_eq!(recording_directory(&doc, Some(home)), home.join("Videos"));
        assert_eq!(recording_directory(&doc, None), std::env::temp_dir());
        doc.set_value(RECORDING_DIR_CONFIG_KEY, Value::String("D:/rec".into()))
            .unwrap();
        assert_eq!(
            recording_directory(&doc, Some(home)),
            PathBuf::from("D:/rec")
        );
    }

    /// 整屏区域等于显示器范围；非法范围返回 None。
    #[test]
    fn full_region_matches_bounds() {
        let m = monitor(2, PhysicalRect::new(-1920, 200, 1920, 1080), false);
        assert_eq!(full_monitor_region(&m).unwrap().1, (-1920, 200, 1920, 1080));
        assert!(full_monitor_region(&monitor(3, PhysicalRect::new(0, 0, 0, 5), false)).is_none());
    }
}
