//! 录制区域视图与悬浮控制条（Recording Area View）。
//!
//! 一个铺满所在显示器的窗口。就绪态用进入时抓的静态屏幕底图铺满整窗（和截图一样，不依赖透明），
//! 开始录制后只画选区外圈边框、倒计时数字和控制条，其余像素靠 `SetWindowRgn` 命中区域
//! 穿透给被录制的应用（区域计算见 [`compute_layout`]）。
//! 窗口自身通过 `WDA_EXCLUDEFROMCAPTURE` 排除在录制画面之外。

use crate::recording::keymap::{RecordKeyAction, RecordKeymap};
use crate::recording::model::{RecordingConfig, RecordingFormat, RecordingState};
use crate::recording::ready_options::{
    self, DragEdge, MIN_SELECTION_SIZE, OutputPreset, RangePreset, audio_available, output_size_for,
};
use crate::recording::ready_panel::{READY_LOGICAL_WIDTH, ReadyUi, render_ready_panel};
use crate::recording::runtime::ScreenRecordingSession;
use snow_i18n::Args;
use snow_recorder_protocol::AudioRequest;
use snow_ui::shell::geometry::{PhysicalPoint, PhysicalRect};
use snow_ui::ui::component::button::{Button, ButtonVariants};
use snow_ui::ui::component::{Icon, Sizable, Size as ComponentSize};
use snow_ui::ui::*;
use snow_ui::widgets::calculate_toolbar_placement;
use std::sync::Arc;
use std::time::{Duration, Instant};

/// 边框厚度（逻辑像素）。
const BORDER_LOGICAL: f32 = 1.0;
/// 控制条逻辑宽度的下限与带音频提示时的宽度（提示文字较长）。
const TOOLBAR_LOGICAL_WIDTH: f32 = 340.0;
/// 控制条左右内边距加描边之和（`px_3` 两侧 + 1px 描边两侧）。
pub(crate) const BAR_FRAME: f32 = 26.0;
/// 控制条子元素间距（`gap_2`）。
pub(crate) const BAR_GAP: f32 = 8.0;
/// 控制条里小号图标按钮的边长。
pub(crate) const BAR_BUTTON: f32 = 24.0;
/// 状态圆点直径。
const BAR_DOT: f32 = 10.0;
/// 计时文字预留宽度（`00:00:00`）。
const BAR_TIME_WIDTH: f32 = 60.0;
/// 选区尺寸文字预留宽度（`3840x2160`）。
pub(crate) const BAR_SIZE_WIDTH: f32 = 64.0;
/// 录制中计时与尺寸共用的槽宽（计时 + 分隔线 + 尺寸 + 两处间距）。
const BAR_STATUS_WIDTH: f32 = BAR_TIME_WIDTH + BAR_DIVIDER + BAR_SIZE_WIDTH + 2.0 * BAR_GAP;
/// 竖分隔线宽度。
pub(crate) const BAR_DIVIDER: f32 = 1.0;
/// 倒计时文字预留宽度。
const BAR_COUNTDOWN_TEXT: f32 = 150.0;
/// 保存中文字预留宽度。
const BAR_SAVING_TEXT: f32 = 100.0;
/// 出错时控制条逻辑宽度（要放得下错误原因）。
const TOOLBAR_ERROR_LOGICAL_WIDTH: f32 = 520.0;
/// 控制条上图标按钮的图标：暂停。
const ICON_PAUSE: &str = "icons/antd/pause.svg";
/// 控制条上图标按钮的图标：继续。
const ICON_RESUME: &str = "icons/antd/caret-right.svg";
/// 控制条上图标按钮的图标：完成并编辑。
const ICON_EDIT: &str = "icons/antd/edit.svg";
/// 控制条上图标按钮的图标：完成（保存）。
const ICON_FINISH: &str = "icons/antd/check.svg";
/// 控制条上图标按钮的图标：放弃 / 关闭。
const ICON_CLOSE: &str = "icons/antd/close.svg";
/// 控制条逻辑高度。
const TOOLBAR_LOGICAL_HEIGHT: f32 = 40.0;
/// 音频降级提示行的逻辑高度。
const NOTICE_LOGICAL_HEIGHT: f32 = 22.0;
/// 音频降级提示的文字颜色（警示黄）。
const COLOR_NOTICE: u32 = 0xFAAD14FF;
/// 多条音频提示之间的分隔。
const NOTICE_SEPARATOR: &str = " · ";
/// 就绪面板（单行长条）的逻辑高度。
pub(crate) const READY_PANEL_LOGICAL_HEIGHT: f32 = 44.0;
/// 没有底图时（退回裁剪模式），就绪面板下方为下拉菜单预留的命中区域高度（命中区域之外的像素画不出菜单）。
const READY_MENU_SPACE_LOGICAL: f32 = 220.0;
/// 边中点拖拽手柄的可点击逻辑边长（比视觉点大，便于抓取）。
const HANDLE_HIT_LOGICAL: f32 = 24.0;
/// 边中点拖拽手柄的视觉点逻辑边长。
const HANDLE_DOT_LOGICAL: f32 = 10.0;
/// 手柄视觉点底色。
const COLOR_HANDLE: u32 = 0xFFFFFFFF;
/// 控制条与选区的逻辑间距。
const TOOLBAR_LOGICAL_MARGIN: f32 = 8.0;
/// 倒计时数字框的逻辑边长。
const COUNTDOWN_LOGICAL_BOX: f32 = 160.0;
/// 倒计时每步时长。
const COUNTDOWN_STEP: Duration = Duration::from_secs(1);
/// 错误信息在窗口里停留的时长。
const ERROR_DISPLAY: Duration = Duration::from_secs(8);
/// 错误文案最多显示的字符数。
const ERROR_MAX_CHARS: usize = 34;
/// 边框颜色：待命 / 倒计时（蓝）。
const COLOR_IDLE: u32 = 0x1677FFFF;
/// 边框颜色：录制中（红）。
const COLOR_RECORDING: u32 = 0xFF4D4FFF;
/// 边框颜色：暂停（黄）。
const COLOR_PAUSED: u32 = 0xFAAD14FF;

/// 录制交互事件通知。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecordingAreaAction {
    /// 暂停或恢复录制。
    TogglePause,
    /// 停止录制并保存导出。
    StopAndSave,
    /// 取消并放弃当前录制。
    Cancel,
    /// 关闭错误提示或完成提示。
    Dismiss,
    /// 就绪态点“开始”：按倒计时设置开始录制。
    Start,
    /// 录制中点“完成并编辑”：等同停止，文件写完后改去打开视频编辑器。
    Edit,
}

/// 自动化验证计划（仅由环境变量驱动，不经过操作系统输入）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AutoPlan {
    /// 有效录制满多少秒后自动停止。
    pub stop_after_secs: u64,
    /// 可选的暂停计划：`(有效录制满多少秒时暂停, 暂停多少秒)`。
    pub pause: Option<(u64, u64)>,
}

/// 自动化计划的运行状态。
#[derive(Debug, Clone, Copy)]
struct AutoState {
    /// 计划。
    plan: AutoPlan,
    /// 暂停已执行。
    pause_done: bool,
    /// 本次暂停开始时刻。
    paused_since: Option<Instant>,
}

/// 图标按钮的色调。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum IconTone {
    /// 主操作（开始）。
    Primary,
    /// 危险操作（放弃）。
    Danger,
    /// 普通（幽灵）。
    Plain,
}

/// 窗口内各元素的物理像素布局（窗口坐标）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AreaLayout {
    /// 边框四条（上、下、左、右），已裁到窗口内。
    pub border: Vec<PhysicalRect>,
    /// 控制条。
    pub toolbar: PhysicalRect,
    /// 倒计时数字框（仅倒计时期间）。
    pub countdown_box: Option<PhysicalRect>,
    /// 四条边中点的拖拽手柄可点击范围（仅 Ready 态非空）。
    pub handles: Vec<(DragEdge, PhysicalRect)>,
    /// 整窗命中矩形（仅 Ready 且有底图时）：整窗被底图画满，命中区域不再裁剪。
    pub full: Option<PhysicalRect>,
    /// 选区内部的平移抓取区（仅 Ready 态有）。
    pub move_zone: Option<PhysicalRect>,
}

impl AreaLayout {
    /// 需要接收点击（同时也是可见）的全部矩形：边框 ∪ 控制条 ∪ 倒计时框 ∪ 手柄；就绪且有底图时是整窗。
    pub fn hit_rects(&self) -> Vec<PhysicalRect> {
        if let Some(full) = self.full {
            return vec![full];
        }
        let mut rects = self.border.clone();
        rects.push(self.toolbar);
        rects.extend(self.countdown_box);
        rects.extend(self.handles.iter().map(|(_, r)| *r));
        rects
    }
}

/// 非就绪、非出错状态下控制条的逻辑宽度：按各状态实际显示的控件累加，不留空白。
///
/// # 参数
/// - `state`：当前录制状态。
/// - `edit`：是否显示“完成并编辑”按钮。
/// - `notice`：是否带音频提示行（提示文字长，取不小于 [`TOOLBAR_LOGICAL_WIDTH`]）。
///
/// # 返回
/// 逻辑宽度。
fn bar_logical_width(state: &RecordingState, edit: bool, notice: bool) -> f32 {
    // (子元素内容宽度之和, 子元素个数)
    let (content, count) = match state {
        RecordingState::Countdown { .. } => (BAR_DOT + BAR_COUNTDOWN_TEXT + BAR_BUTTON, 3),
        RecordingState::Recording { .. } => {
            let buttons = if edit { 4.0 } else { 3.0 };
            (
                BAR_DOT + BAR_TIME_WIDTH + BAR_DIVIDER + BAR_SIZE_WIDTH + buttons * BAR_BUTTON,
                if edit { 8 } else { 7 },
            )
        }
        _ => (BAR_DOT + BAR_SAVING_TEXT, 2),
    };
    let width = BAR_FRAME + content + (count - 1) as f32 * BAR_GAP;
    if notice {
        width.max(TOOLBAR_LOGICAL_WIDTH)
    } else {
        width
    }
}

/// 计算窗口内各元素的物理布局。
///
/// # 参数
/// - `region`：录制区域（窗口坐标，物理像素）。
/// - `window`：窗口矩形（原点 0,0，物理像素）。
/// - `scale`：显示器缩放比（物理 / 逻辑）。
/// - `countdown`：是否处于倒计时（需要数字框）。
/// - `notice`：控制条是否多带一行音频降级提示（增高）。
///
/// # 返回
/// 布局；边框条画在选区**外侧**，不会进入被录制的画面。
///
/// # 示例
/// ```ignore
/// let l = compute_layout(PhysicalRect::new(100, 100, 400, 300), PhysicalRect::new(0, 0, 1920, 1080), 1.0, false, false);
/// assert_eq!(l.border.len(), 4);
/// ```
pub fn compute_layout(
    region: PhysicalRect,
    window: PhysicalRect,
    scale: f32,
    countdown: bool,
    notice: bool,
) -> AreaLayout {
    let height = TOOLBAR_LOGICAL_HEIGHT + if notice { NOTICE_LOGICAL_HEIGHT } else { 0.0 };
    compute_layout_sized(
        region,
        window,
        scale,
        countdown,
        (TOOLBAR_LOGICAL_WIDTH, height),
        BORDER_LOGICAL,
    )
}

/// 同 [`compute_layout`]，但控制条逻辑尺寸由调用方给出（就绪面板比普通控制条大）。
///
/// # 参数
/// - `toolbar_logical`：控制条（宽，高）逻辑尺寸。
/// - `border_logical`：边框逻辑粗细；其余参数同 [`compute_layout`]。
pub fn compute_layout_sized(
    region: PhysicalRect,
    window: PhysicalRect,
    scale: f32,
    countdown: bool,
    toolbar_logical: (f32, f32),
    border_logical: f32,
) -> AreaLayout {
    let t = (border_logical * scale).ceil() as i32;
    let strips = [
        PhysicalRect::new(region.x - t, region.y - t, region.width + 2 * t, t),
        PhysicalRect::new(region.x - t, region.bottom(), region.width + 2 * t, t),
        PhysicalRect::new(region.x - t, region.y, t, region.height),
        PhysicalRect::new(region.right(), region.y, t, region.height),
    ];
    let border = strips.iter().filter_map(|s| s.intersect(&window)).collect();
    let size = PhysicalPoint::new(
        (toolbar_logical.0 * scale).round() as i32,
        (toolbar_logical.1 * scale).round() as i32,
    );
    let margin = (TOOLBAR_LOGICAL_MARGIN * scale).round() as i32;
    let pos = place_toolbar(region, size, window, margin);
    let toolbar = PhysicalRect::new(pos.x, pos.y, size.x, size.y);
    let countdown_box = countdown.then(|| {
        let side = (COUNTDOWN_LOGICAL_BOX * scale).round() as i32;
        PhysicalRect::new(
            region.x + (region.width - side) / 2,
            region.y + (region.height - side) / 2,
            side,
            side,
        )
    });
    AreaLayout {
        border,
        toolbar,
        countdown_box,
        handles: Vec::new(),
        full: None,
        move_zone: None,
    }
}

/// 控制条定位：选区里放得下就放选区内部右下角（拖动时不会上下乱跳），放不下再放到选区外。
///
/// # 参数
/// - `region`：选区（窗口坐标）。
/// - `size`：控制条物理尺寸。
/// - `window`：窗口矩形。
/// - `margin`：与选区边缘的间距。
///
/// # 返回
/// 控制条左上角物理坐标。
///
/// # 示例
/// ```ignore
/// let p = place_toolbar(PhysicalRect::new(100, 100, 800, 600), PhysicalPoint::new(300, 40), window, 8);
/// assert_eq!((p.x, p.y), (592, 652));
/// ```
fn place_toolbar(
    region: PhysicalRect,
    size: PhysicalPoint,
    window: PhysicalRect,
    margin: i32,
) -> PhysicalPoint {
    if region.width >= size.x + 2 * margin && region.height >= size.y + 2 * margin {
        return PhysicalPoint::new(
            region.right() - size.x - margin,
            region.bottom() - size.y - margin,
        );
    }
    calculate_toolbar_placement(region, size, window, margin)
}

/// 计算四条边中点的拖拽手柄可点击范围（窗口坐标）。
///
/// # 参数
/// - `region`：选区（窗口坐标，物理像素）。
/// - `window`：窗口矩形（原点 0,0）。
/// - `scale`：显示器缩放比。
///
/// # 返回
/// 上、下、左、右四个手柄的矩形；以边中点为中心，贴屏幕边时整体平移回窗口内。
pub fn compute_handles(
    region: PhysicalRect,
    window: PhysicalRect,
    scale: f32,
) -> Vec<(DragEdge, PhysicalRect)> {
    let side = (HANDLE_HIT_LOGICAL * scale).round() as i32;
    let centers = [
        (DragEdge::Top, region.x + region.width / 2, region.y),
        (
            DragEdge::Bottom,
            region.x + region.width / 2,
            region.bottom(),
        ),
        (DragEdge::Left, region.x, region.y + region.height / 2),
        (
            DragEdge::Right,
            region.right(),
            region.y + region.height / 2,
        ),
    ];
    centers
        .into_iter()
        .map(|(edge, cx, cy)| {
            let x = (cx - side / 2).clamp(window.x, (window.right() - side).max(window.x));
            let y = (cy - side / 2).clamp(window.y, (window.bottom() - side).max(window.y));
            (edge, PhysicalRect::new(x, y, side, side))
        })
        .collect()
}

/// 手柄对应的缩放光标。
const fn handle_cursor(edge: DragEdge) -> CursorStyle {
    match edge {
        DragEdge::Top | DragEdge::Bottom => CursorStyle::ResizeUpDown,
        DragEdge::Left | DragEdge::Right => CursorStyle::ResizeLeftRight,
        DragEdge::Move => CursorStyle::ClosedHand,
    }
}

/// 把错误文案截到合理长度。
///
/// # 参数
/// - `reason`：原始错误原因。
pub fn shorten_reason(reason: &str) -> String {
    if reason.chars().count() <= ERROR_MAX_CHARS {
        reason.to_string()
    } else {
        let head: String = reason.chars().take(ERROR_MAX_CHARS).collect();
        format!("{head}…")
    }
}

/// 录制区域视图组件。
pub struct RecordingAreaView {
    /// 录制核心会话控制器。
    pub session: ScreenRecordingSession,
    /// 窗口所在显示器左上角（虚拟桌面物理坐标），用于把选区换算成窗口坐标。
    origin: PhysicalPoint,
    /// 窗口物理尺寸。
    window_size: (i32, i32),
    /// 显示器缩放比。
    scale: f32,
    /// 用户已取消。
    cancelled: bool,
    /// 用户已关闭错误提示。
    dismissed: bool,
    /// 上一次倒计时步进时刻。
    countdown_tick_at: Instant,
    /// 首次观察到错误的时刻。
    error_since: Option<Instant>,
    /// 自动化计划。
    auto: Option<AutoState>,
    /// 界面语言代码（提示文案用）。
    locale: &'static str,
    /// 控制条键位表。
    keymap: RecordKeymap,
    /// 键盘焦点句柄（离屏测试为空）。
    focus: Option<FocusHandle>,
    /// 完成后把录制文件复制到剪贴板。
    copy_on_finish: bool,
    /// 窗口所在显示器范围（范围预设以它为边界）。
    monitor_rect: PhysicalRect,
    /// 进入就绪态时的原始选区（“恢复”预设回到它）。
    initial_region: PhysicalRect,
    /// 范围预设的基准选区；手动拖动、移动或恢复后清空。
    range_base: Option<PhysicalRect>,
    /// 正在拖拽的边及拖拽开始时的选区（仅 Ready 态有效）。
    drag: Option<(DragEdge, PhysicalRect)>,
    /// 平移拖拽的按下点（虚拟桌面物理坐标）。
    drag_anchor: (i32, i32),
    /// 鼠标悬停的按钮文字（录制中窗口被裁成控制条，系统 tooltip 会被裁掉，改在条内第二行显示）。
    hover_hint: Option<String>,
    /// 就绪态的屏幕底图（进入就绪时抓的静态画面，铺满整窗，开始录制后丢弃）。
    backdrop: Option<Arc<RenderImage>>,
    /// 就绪态选中的输出尺寸档（选区变化后据此重算输出尺寸）。
    output_preset: OutputPreset,
    /// 就绪面板的下拉组件（离屏测试为空）。
    pub(crate) ready_ui: Option<ReadyUi>,
    /// 录制中是否提供“完成并编辑”按钮（自动化验证不提供）。
    edit_enabled: bool,
    /// 用户点了“完成并编辑”：完成后去视频编辑器，而不是定位 / 复制文件。
    edit_after_finish: bool,
}

impl RecordingAreaView {
    /// 创建录制区域视图。
    ///
    /// # 参数
    /// - `config`：录制配置（区域为虚拟桌面物理坐标）。
    /// - `monitor_bounds`：窗口所在显示器的物理范围。
    /// - `scale`：显示器缩放比。
    pub fn new(config: RecordingConfig, monitor_bounds: PhysicalRect, scale: f32) -> Self {
        let scale = if scale.is_finite() && scale > 0.0 {
            scale
        } else {
            1.0
        };
        let initial_region = config.region;
        Self {
            session: ScreenRecordingSession::new(config),
            monitor_rect: monitor_bounds,
            initial_region,
            range_base: None,
            drag: None,
            drag_anchor: (0, 0),
            hover_hint: None,
            backdrop: None,
            output_preset: OutputPreset::Original,
            ready_ui: None,
            edit_enabled: false,
            edit_after_finish: false,
            origin: PhysicalPoint::new(monitor_bounds.x, monitor_bounds.y),
            window_size: (monitor_bounds.width, monitor_bounds.height),
            scale,
            cancelled: false,
            dismissed: false,
            countdown_tick_at: Instant::now(),
            error_since: None,
            auto: None,
            locale: snow_i18n::FALLBACK_LOCALE,
            keymap: RecordKeymap::default(),
            focus: None,
            copy_on_finish: false,
        }
    }

    /// 开启录制中的“完成并编辑”按钮。
    ///
    /// # 参数
    /// - `enabled`：是否开启。
    pub fn set_edit_enabled(&mut self, enabled: bool) {
        self.edit_enabled = enabled;
    }

    /// 是否显示“完成并编辑”按钮：已开启，且正在录制（含暂停）。
    pub fn edit_button_visible(&self) -> bool {
        self.edit_enabled && matches!(self.session.state(), RecordingState::Recording { .. })
    }

    /// 取走“去编辑”意图：已点编辑且文件写完时返回录好的文件路径，只返回一次。
    ///
    /// 出错 / 空闲（含取消）时清除意图，避免残留到下一次录制；保存中则保留等待完成。
    pub fn take_edit_request(&mut self) -> Option<std::path::PathBuf> {
        if !self.edit_after_finish {
            return None;
        }
        match self.session.state() {
            RecordingState::Finished { file_path, .. } => {
                let path = file_path.clone();
                self.edit_after_finish = false;
                Some(path)
            }
            RecordingState::Error { .. } | RecordingState::Idle => {
                self.edit_after_finish = false;
                None
            }
            _ => None,
        }
    }

    /// 就绪选项面板当前是否应显示：只在 `Ready` 态；点了开始后（含暂停期间）整排隐藏。
    pub fn ready_panel_visible(&self) -> bool {
        self.session.state().is_ready()
    }

    /// 就绪态改写本次录制配置；其他状态返回 `false`（开始后不可改）。
    fn edit_ready(&mut self, edit: impl FnOnce(&mut RecordingConfig)) -> bool {
        match self.session.config_in_ready() {
            Some(config) => {
                edit(config);
                true
            }
            None => false,
        }
    }

    /// 就绪态：设置帧率（只对本次录制生效）。
    pub fn ready_set_fps(&mut self, fps: u32) -> bool {
        self.edit_ready(|c| c.fps = fps)
    }

    /// 就绪态：设置倒计时秒数（0 = 关闭）。
    pub fn ready_set_countdown(&mut self, secs: u32) -> bool {
        self.edit_ready(|c| c.countdown_secs = secs)
    }

    /// 就绪态：设置是否录入光标。
    pub fn ready_set_cursor(&mut self, show: bool) -> bool {
        self.edit_ready(|c| c.show_cursor = show)
    }

    /// 就绪态：选择输出尺寸档，并按当前选区算出具体输出尺寸。
    pub fn ready_set_output(&mut self, preset: OutputPreset) -> bool {
        let r = self.bounds();
        let ok = self.edit_ready(|c| c.output_size = output_size_for(preset, r.width, r.height));
        if ok {
            self.output_preset = preset;
        }
        ok
    }

    /// 当前选中的输出尺寸档。
    pub fn output_preset(&self) -> OutputPreset {
        self.output_preset
    }

    /// 就绪态：应用范围预设（以选区中心为基准按比例重设，夹回显示器内），并重算输出尺寸。
    /// 控制条与命中区域由 `layout()` 随选区自动更新，宿主下次同步时刷新。
    pub fn ready_apply_range(&mut self, preset: RangePreset) -> bool {
        let region = if preset == RangePreset::Restore {
            self.range_base = None;
            self.initial_region
        } else {
            // 比例预设都基于同一个基准选区，连续切换不会越缩越小；手动拖动 / 移动后基准作废
            let base = *self.range_base.get_or_insert(self.bounds());
            ready_options::apply_range_preset(preset, base, self.monitor_rect)
        };
        self.set_ready_region(region)
    }

    /// 就绪态：改写选区并按当前输出档重算输出尺寸；其他状态返回 `false`。
    fn set_ready_region(&mut self, region: PhysicalRect) -> bool {
        let output = self.output_preset;
        self.edit_ready(|c| {
            c.region = region;
            c.output_size = output_size_for(output, region.width, region.height);
        })
    }

    /// 拖拽手柄是否可见 / 可拖：只在就绪态。
    pub fn handles_visible(&self) -> bool {
        self.session.state().is_ready()
    }

    /// 开始拖拽某条边；非就绪态无效。
    ///
    /// # 参数
    /// - `edge`：被按下的手柄所在的边。
    pub fn begin_drag(&mut self, edge: DragEdge) {
        if self.handles_visible() {
            self.range_base = None;
            self.drag = Some((edge, self.bounds()));
        }
    }

    /// 拖拽中按鼠标物理坐标更新选区（对边固定、夹在显示器内、不小于最小尺寸）。
    ///
    /// # 参数
    /// - `pointer`：鼠标虚拟桌面物理坐标 `(x, y)`。
    ///
    /// # 返回
    /// 选区是否被更新。
    pub fn drag_to(&mut self, pointer: (i32, i32)) -> bool {
        let Some((edge, origin)) = self.drag else {
            return false;
        };
        let region = if edge == DragEdge::Move {
            let delta = (
                pointer.0 - self.drag_anchor.0,
                pointer.1 - self.drag_anchor.1,
            );
            ready_options::move_region(origin, delta, self.monitor_rect)
        } else {
            ready_options::drag_edge(origin, edge, pointer, self.monitor_rect, MIN_SELECTION_SIZE)
        };
        self.set_ready_region(region)
    }

    /// 设置就绪态底图（选区时冻结的那块显示器的干净画面，铺满整窗）。
    ///
    /// # 参数
    /// - `image`：覆盖本显示器的整屏图像资源。
    pub fn set_backdrop(&mut self, image: Arc<RenderImage>) {
        self.backdrop = Some(image);
    }

    /// 已离开就绪态时丢弃底图（宿主在命中区域裁剪好之后调用，避免中间闪一帧空白）。
    ///
    /// # 返回
    /// 是否真的丢弃了底图（需要重绘）。
    pub fn release_backdrop_if_started(&mut self) -> bool {
        if self.backdrop.is_some() && !self.ready_panel_visible() {
            self.backdrop = None;
            return true;
        }
        false
    }

    /// 开始整体平移选区（拖选区内部）；非就绪态无效。
    ///
    /// # 参数
    /// - `pointer`：按下点的虚拟桌面物理坐标。
    pub fn begin_move(&mut self, pointer: (i32, i32)) {
        if self.handles_visible() {
            self.range_base = None;
            self.drag_anchor = pointer;
            self.drag = Some((DragEdge::Move, self.bounds()));
        }
    }

    /// 结束拖拽。
    pub fn end_drag(&mut self) {
        self.drag = None;
    }

    /// 窗口逻辑坐标换算成虚拟桌面物理坐标。
    fn pointer_to_physical(&self, position: Point<Pixels>) -> (i32, i32) {
        (
            (f32::from(position.x) * self.scale).round() as i32 + self.origin.x,
            (f32::from(position.y) * self.scale).round() as i32 + self.origin.y,
        )
    }

    /// 就绪态：单独开关麦克风；格式不支持音频时无效。
    pub fn ready_set_mic(&mut self, on: bool) -> bool {
        self.ready_edit_audio(|a| a.microphone = on)
    }

    /// 就绪态：单独开关系统声；格式不支持音频时无效。
    pub fn ready_set_system(&mut self, on: bool) -> bool {
        self.ready_edit_audio(|a| a.system = on)
    }

    /// 改写音频请求，仅 MP4 且就绪态有效。
    fn ready_edit_audio(&mut self, edit: impl FnOnce(&mut AudioRequest)) -> bool {
        if !audio_available(self.format()) {
            return false;
        }
        self.edit_ready(|c| edit(&mut c.audio))
    }

    /// 设置控制条键位表。
    ///
    /// # 参数
    /// - `keymap`：由配置构造的键位表。
    pub fn set_keymap(&mut self, keymap: RecordKeymap) {
        self.keymap = keymap;
    }

    /// 设置键盘焦点句柄（视图实体创建时由 GPUI 提供）。
    pub fn set_focus_handle(&mut self, focus: FocusHandle) {
        self.focus = Some(focus);
    }

    /// 设置录制完成后是否把文件复制到剪贴板。
    ///
    /// # 参数
    /// - `copy`：是否复制。
    pub fn set_copy_on_finish(&mut self, copy: bool) {
        self.copy_on_finish = copy;
    }

    /// 录制完成后是否需要把文件复制到剪贴板。
    pub fn copy_on_finish(&self) -> bool {
        self.copy_on_finish
    }

    /// 处理一次按键动作；只在状态允许时生效（录制中才能导出 / 暂停，Esc 只放弃倒计时与错误提示）。
    ///
    /// # 参数
    /// - `action`：键位表解析出的动作。
    pub fn handle_key_action(&mut self, action: RecordKeyAction) {
        let recording = matches!(self.session.state(), RecordingState::Recording { .. });
        match action {
            RecordKeyAction::Export if recording => {
                self.handle_action(RecordingAreaAction::StopAndSave)
            }
            RecordKeyAction::ToggleRecording if recording => {
                self.handle_action(RecordingAreaAction::TogglePause)
            }
            RecordKeyAction::CopyToClipboard if recording => {
                self.copy_on_finish = true;
                self.handle_action(RecordingAreaAction::StopAndSave);
            }
            RecordKeyAction::EndRecording => match self.session.state() {
                RecordingState::Ready | RecordingState::Countdown { .. } => {
                    self.handle_action(RecordingAreaAction::Cancel)
                }
                RecordingState::Error { .. } | RecordingState::Finished { .. } => {
                    self.handle_action(RecordingAreaAction::Dismiss)
                }
                _ => {}
            },
            _ => {}
        }
    }

    /// 当前界面语言代码。
    pub fn locale(&self) -> &'static str {
        self.locale
    }

    /// 设置界面语言（影响音频降级提示文案）。
    ///
    /// # 参数
    /// - `locale`：语料语言代码，如 `zh-CN`。
    pub fn set_locale(&mut self, locale: &'static str) {
        self.locale = locale;
    }

    /// 音频降级提示文本：仅倒计时 / 录制 / 保存期间显示，无降级时为 `None`。
    pub fn audio_notice_text(&self) -> Option<String> {
        if !matches!(
            self.session.state(),
            RecordingState::Countdown { .. }
                | RecordingState::Recording { .. }
                | RecordingState::Saving
        ) {
            return None;
        }
        let notices = self.session.audio_notices();
        if notices.is_empty() {
            return None;
        }
        let i18n = crate::ocr_backend::i18n_for(self.locale);
        let texts: Vec<String> = notices.iter().map(|n| i18n.tr(n.message_id())).collect();
        Some(texts.join(NOTICE_SEPARATOR))
    }

    /// 启用自动化验证计划（仅测试 / 验收用）。
    pub fn set_auto_plan(&mut self, plan: AutoPlan) {
        self.auto = Some(AutoState {
            plan,
            pause_done: false,
            paused_since: None,
        });
    }

    /// 获取录制物理矩形（虚拟桌面坐标）。
    pub fn bounds(&self) -> PhysicalRect {
        self.session.config().region
    }

    /// 获取当前录制格式。
    pub fn format(&self) -> RecordingFormat {
        self.session.config().format
    }

    /// 录制区域在窗口坐标下的位置。
    fn region_in_window(&self) -> PhysicalRect {
        let r = self.bounds();
        PhysicalRect::new(r.x - self.origin.x, r.y - self.origin.y, r.width, r.height)
    }

    /// 当前状态下的窗口布局。
    pub fn layout(&self) -> AreaLayout {
        self.layout_with_notice(self.audio_notice_text().is_some())
    }

    /// 同 [`Self::layout`]，但音频提示是否显示由调用方给出（渲染时已取过提示文本，免得再算一遍）。
    fn layout_with_notice(&self, notice: bool) -> AreaLayout {
        let window = PhysicalRect::new(0, 0, self.window_size.0, self.window_size.1);
        let countdown = matches!(self.session.state(), RecordingState::Countdown { .. });
        if self.ready_panel_visible() {
            let mut layout = compute_layout_sized(
                self.region_in_window(),
                window,
                self.scale,
                false,
                (
                    READY_LOGICAL_WIDTH,
                    READY_PANEL_LOGICAL_HEIGHT
                        + if self.backdrop.is_some() {
                            0.0
                        } else {
                            READY_MENU_SPACE_LOGICAL
                        },
                ),
                BORDER_LOGICAL,
            );
            layout.handles = compute_handles(self.region_in_window(), window, self.scale);
            layout.move_zone = Some(self.region_in_window());
            layout.full = self.backdrop.is_some().then_some(window);
            return layout;
        }
        if matches!(self.session.state(), RecordingState::Error { .. }) {
            let height = TOOLBAR_LOGICAL_HEIGHT + if notice { NOTICE_LOGICAL_HEIGHT } else { 0.0 };
            return compute_layout_sized(
                self.region_in_window(),
                window,
                self.scale,
                false,
                (TOOLBAR_ERROR_LOGICAL_WIDTH, height),
                BORDER_LOGICAL,
            );
        }
        let height = TOOLBAR_LOGICAL_HEIGHT + if notice { NOTICE_LOGICAL_HEIGHT } else { 0.0 };
        let width = bar_logical_width(self.session.state(), self.edit_button_visible(), notice);
        compute_layout_sized(
            self.region_in_window(),
            window,
            self.scale,
            countdown,
            (width, height),
            BORDER_LOGICAL,
        )
    }

    /// 处理外部操作动作。
    ///
    /// # 参数
    /// - `action`：用户在控制条上触发的动作。
    pub fn handle_action(&mut self, action: RecordingAreaAction) {
        match action {
            RecordingAreaAction::TogglePause => self.session.toggle_pause(),
            RecordingAreaAction::StopAndSave => {
                if let Err(e) = self.session.finish() {
                    tracing::warn!(error = %e, "停止录制被拒绝");
                }
            }
            RecordingAreaAction::Cancel => {
                self.session.cancel();
                self.cancelled = true;
                self.edit_after_finish = false;
            }
            RecordingAreaAction::Dismiss => self.dismissed = true,
            RecordingAreaAction::Start => {
                self.drag = None;
                if self.session.start_from_ready() {
                    self.countdown_tick_at = Instant::now();
                }
            }
            RecordingAreaAction::Edit => {
                if self.edit_button_visible() {
                    match self.session.finish() {
                        Ok(()) => self.edit_after_finish = true,
                        Err(e) => tracing::warn!(error = %e, "停止录制被拒绝"),
                    }
                }
            }
        }
    }

    /// 推进时间：倒计时、录制进程事件、超时检查与自动化计划。
    ///
    /// # 参数
    /// - `now`：当前时刻。
    ///
    /// # 返回
    /// 是否有可见变化（需要重绘）。
    pub fn advance(&mut self, now: Instant) -> bool {
        let mut changed = false;
        if matches!(self.session.state(), RecordingState::Countdown { .. })
            && now.saturating_duration_since(self.countdown_tick_at) >= COUNTDOWN_STEP
        {
            self.countdown_tick_at = now;
            self.session.tick_countdown();
            changed = true;
        }
        changed |= self.session.poll(now);
        changed |= self.run_auto_plan(now);
        if let RecordingState::Error { reason } = self.session.state()
            && self.error_since.is_none()
        {
            tracing::error!(reason = ?reason, "录制出错");
            self.error_since = Some(now);
        }
        changed
    }

    /// 执行自动化计划（暂停 / 恢复 / 停止）。
    fn run_auto_plan(&mut self, now: Instant) -> bool {
        let Some(mut auto) = self.auto else {
            return false;
        };
        let RecordingState::Recording {
            elapsed_secs,
            is_paused,
            ..
        } = *self.session.state()
        else {
            return false;
        };
        let mut acted = false;
        if let Some((at_secs, pause_secs)) = auto.plan.pause {
            if !auto.pause_done && !is_paused && elapsed_secs >= at_secs {
                self.session.toggle_pause();
                auto.pause_done = true;
                auto.paused_since = Some(now);
                acted = true;
            } else if is_paused
                && let Some(since) = auto.paused_since
                && now.saturating_duration_since(since) >= Duration::from_secs(pause_secs)
            {
                self.session.toggle_pause();
                auto.paused_since = None;
                acted = true;
            }
        }
        let paused_now = matches!(
            self.session.state(),
            RecordingState::Recording {
                is_paused: true,
                ..
            }
        );
        if !acted && !paused_now && elapsed_secs >= auto.plan.stop_after_secs {
            self.handle_action(RecordingAreaAction::StopAndSave);
            acted = true;
        }
        self.auto = Some(auto);
        acted
    }

    /// 窗口是否应该关闭：已完成、被取消、错误提示已读或超时。
    ///
    /// # 参数
    /// - `now`：当前时刻。
    pub fn is_over(&self, now: Instant) -> bool {
        if self.cancelled || self.dismissed {
            return true;
        }
        match self.session.state() {
            RecordingState::Finished { .. } => true,
            RecordingState::Error { .. } => self
                .error_since
                .is_some_and(|t| now.saturating_duration_since(t) >= ERROR_DISPLAY),
            _ => false,
        }
    }

    /// 物理矩形换算为逻辑像素位置（左、上、宽、高）。
    fn logical(&self, r: PhysicalRect) -> (Pixels, Pixels, Pixels, Pixels) {
        (
            px(r.x as f32 / self.scale),
            px(r.y as f32 / self.scale),
            px(r.width as f32 / self.scale),
            px(r.height as f32 / self.scale),
        )
    }

    /// 渲染平移抓取区（选区内部，透明）：按下开始整体移动选区。
    fn render_move_area(&self, rect: PhysicalRect, cx: &mut Context<Self>) -> impl IntoElement {
        let (l, t, w, h) = self.logical(rect);
        div()
            .absolute()
            .left(l)
            .top(t)
            .w(w)
            .h(h)
            .cursor(CursorStyle::OpenHand)
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, ev: &MouseDownEvent, window, cx| {
                    if let Some(focus) = &this.focus {
                        window.focus(focus, cx);
                    }
                    this.begin_move(this.pointer_to_physical(ev.position));
                    cx.notify();
                    cx.stop_propagation();
                }),
            )
    }

    /// 渲染一个边中点拖拽手柄：外层是可点击范围，中间一个白色小点。
    fn render_handle(
        &self,
        edge: DragEdge,
        rect: PhysicalRect,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let (l, t, w, h) = self.logical(rect);
        div()
            .absolute()
            .left(l)
            .top(t)
            .w(w)
            .h(h)
            .flex()
            .items_center()
            .justify_center()
            .cursor(handle_cursor(edge))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, _: &MouseDownEvent, window, cx| {
                    if let Some(focus) = &this.focus {
                        window.focus(focus, cx);
                    }
                    this.begin_drag(edge);
                    cx.notify();
                    cx.stop_propagation();
                }),
            )
            .child(
                div()
                    .w(px(HANDLE_DOT_LOGICAL))
                    .h(px(HANDLE_DOT_LOGICAL))
                    .rounded_full()
                    .bg(rgba(COLOR_HANDLE))
                    .border_1()
                    .border_color(rgba(COLOR_IDLE)),
            )
    }

    /// 控制条图标按钮（悬停出文字提示）。
    ///
    /// # 参数
    /// - `id`：元素 id。
    /// - `icon`：图标资源路径。
    /// - `tooltip`：悬停提示文字。
    /// - `tone`：按钮色调。
    /// - `action`：点击触发的动作。
    pub(crate) fn icon_button(
        &self,
        id: &'static str,
        icon: &'static str,
        tooltip: String,
        tone: IconTone,
        action: RecordingAreaAction,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let hint = tooltip.clone();
        let entity = cx.entity();
        let button = Button::new(id)
            .with_size(ComponentSize::Small)
            .icon(Icon::default().path(icon))
            .tooltip(tooltip)
            .on_hover(move |hovered, _window, app| {
                entity.update(app, |this, cx| {
                    // 从一个按钮移到另一个时，旧按钮的“离开”可能晚于新按钮的“进入”，只清自己的提示
                    let next = if *hovered {
                        Some(hint.clone())
                    } else if this.hover_hint.as_deref() == Some(hint.as_str()) {
                        None
                    } else {
                        return;
                    };
                    this.hover_hint = next;
                    cx.notify();
                });
            });
        let button = match tone {
            IconTone::Primary => button.primary(),
            IconTone::Danger => button.danger(),
            IconTone::Plain => button.ghost(),
        };
        button.on_click(cx.listener(move |this, _ev: &ClickEvent, _window, cx| {
            this.handle_action(action);
            cx.notify();
        }))
    }

    /// 控制条按钮。
    pub(crate) fn button(
        &self,
        id: &'static str,
        label: String,
        bg: u32,
        text: u32,
        action: RecordingAreaAction,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        div()
            .id(id)
            .px_2()
            .py_1()
            .rounded_sm()
            .bg(rgba(bg))
            .text_size(px(12.0))
            .text_color(rgba(text))
            .cursor_pointer()
            .child(label)
            .on_click(cx.listener(move |this, _ev: &ClickEvent, _window, cx| {
                this.handle_action(action);
                cx.notify();
            }))
    }
}

impl Render for RecordingAreaView {
    /// 渲染边框、倒计时数字与控制条。
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let state = self.session.state().clone();
        let notice_text = self.audio_notice_text();
        let layout = self.layout_with_notice(notice_text.is_some());
        let bounds = self.bounds();
        let accent = match state {
            RecordingState::Recording {
                is_paused: false, ..
            }
            | RecordingState::Saving => COLOR_RECORDING,
            RecordingState::Recording {
                is_paused: true, ..
            } => COLOR_PAUSED,
            _ => COLOR_IDLE,
        };

        let mut root = div()
            .relative()
            .w_full()
            .h_full()
            .on_key_down(cx.listener(|this, ev: &KeyDownEvent, _window, cx| {
                let m = ev.keystroke.modifiers;
                if let Some(action) =
                    this.keymap
                        .resolve(ev.keystroke.key.as_str(), m.control, m.shift, m.alt)
                {
                    this.handle_key_action(action);
                    cx.notify();
                    cx.stop_propagation();
                }
            }))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _: &MouseDownEvent, window, cx| {
                    if let Some(focus) = &this.focus {
                        window.focus(focus, cx);
                    }
                }),
            )
            .on_mouse_move(cx.listener(|this, ev: &MouseMoveEvent, _window, cx| {
                // 没按着左键还在拖：弹层吞掉了抬起事件，直接结束拖动，免得选区跟着鼠标跑
                if this.drag.is_some() && ev.pressed_button != Some(MouseButton::Left) {
                    this.drag = None;
                    cx.notify();
                    return;
                }
                if this.drag.is_some() && this.drag_to(this.pointer_to_physical(ev.position)) {
                    cx.notify();
                }
            }))
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|this, _: &MouseUpEvent, _window, cx| {
                    if this.drag.take().is_some() {
                        cx.notify();
                    }
                }),
            );
        if let Some((edge, _)) = self.drag {
            root = root.cursor(handle_cursor(edge));
        }
        if let Some(focus) = &self.focus {
            root = root.track_focus(focus);
        }
        // 底图在就绪态铺满；开始后保留到宿主把命中区域裁剪好（见 release_backdrop_if_started）
        if let Some(image) = &self.backdrop {
            root = root.child(
                img(ImageSource::Render(Arc::clone(image)))
                    .absolute()
                    .left(px(0.0))
                    .top(px(0.0))
                    .w_full()
                    .h_full()
                    .object_fit(ObjectFit::Fill),
            );
        }
        for strip in &layout.border {
            let (l, t, w, h) = self.logical(*strip);
            root = root.child(div().absolute().left(l).top(t).w(w).h(h).bg(rgba(accent)));
        }

        if let (Some(boxr), RecordingState::Countdown { seconds_left }) =
            (layout.countdown_box, &state)
        {
            let (l, t, w, h) = self.logical(boxr);
            root = root.child(
                div()
                    .absolute()
                    .left(l)
                    .top(t)
                    .w(w)
                    .h(h)
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded_lg()
                    .bg(rgba(0x000000AA))
                    .text_size(px(72.0))
                    .font_weight(FontWeight::BOLD)
                    .text_color(rgba(0xFFFFFFFF))
                    .child(format!("{seconds_left}")),
            );
        }

        let (l, t, w, h) = self.logical(layout.toolbar);
        let i18n = crate::ocr_backend::i18n_for(self.locale);
        if self.ready_panel_visible() {
            if let Some(zone) = layout.move_zone {
                root = root.child(self.render_move_area(zone, cx));
            }
            for (edge, rect) in &layout.handles {
                root = root.child(self.render_handle(*edge, *rect, cx));
            }
            return root.child(render_ready_panel(self, i18n, (l, t, w), cx));
        }
        let mut bar = div()
            .w_full()
            .h(px(TOOLBAR_LOGICAL_HEIGHT))
            .flex()
            .items_center()
            .gap_2()
            .px_3()
            .child(
                div()
                    .w(px(10.0))
                    .h(px(10.0))
                    .rounded_full()
                    .bg(rgba(accent)),
            );
        let label_style = |text: String| {
            div()
                .text_size(px(13.0))
                .font_weight(FontWeight::MEDIUM)
                .text_color(rgba(0xFFFFFFFF))
                .child(text)
        };
        match &state {
            RecordingState::Countdown { seconds_left } => {
                bar = bar
                    .child(label_style(i18n.tr_with(
                        "recording-ui-countdown",
                        &Args::new().named("seconds", seconds_left.to_string()),
                    )))
                    .child(self.icon_button(
                        "rec-cancel",
                        ICON_CLOSE,
                        i18n.tr("recording-ui-cancel"),
                        IconTone::Danger,
                        RecordingAreaAction::Cancel,
                        cx,
                    ));
            }
            RecordingState::Recording {
                elapsed_secs,
                is_paused,
                ..
            } => {
                // 计时与尺寸占一个固定宽度的槽；悬停按钮时槽里改显示按钮名（窗口被裁小，系统 tooltip 会被裁掉）
                let status = if let Some(hint) = self.hover_hint.clone() {
                    div()
                        .flex_none()
                        .w(px(BAR_STATUS_WIDTH))
                        .child(label_style(hint))
                } else {
                    div()
                        .flex_none()
                        .w(px(BAR_STATUS_WIDTH))
                        .flex()
                        .items_center()
                        .gap_2()
                        .child(label_style(RecordingState::format_duration(*elapsed_secs)))
                        .child(div().w(px(BAR_DIVIDER)).h(px(16.0)).bg(rgba(0xFFFFFF26)))
                        .child(
                            div()
                                .text_size(px(12.0))
                                .text_color(rgba(0x8C8C8CFF))
                                .child(format!("{}x{}", bounds.width, bounds.height)),
                        )
                };
                bar = bar
                    .child(status)
                    .child(self.icon_button(
                        "rec-pause",
                        if *is_paused { ICON_RESUME } else { ICON_PAUSE },
                        i18n.tr(if *is_paused {
                            "recording-ui-resume"
                        } else {
                            "recording-ui-pause"
                        }),
                        IconTone::Plain,
                        RecordingAreaAction::TogglePause,
                        cx,
                    ))
                    .children(self.edit_button_visible().then(|| {
                        self.icon_button(
                            "rec-edit",
                            ICON_EDIT,
                            i18n.tr("recording-ready-edit"),
                            IconTone::Primary,
                            RecordingAreaAction::Edit,
                            cx,
                        )
                    }))
                    .child(self.icon_button(
                        "rec-stop",
                        ICON_FINISH,
                        i18n.tr("recording-ui-finish"),
                        IconTone::Primary,
                        RecordingAreaAction::StopAndSave,
                        cx,
                    ))
                    .child(self.icon_button(
                        "rec-cancel",
                        ICON_CLOSE,
                        i18n.tr("recording-ui-cancel"),
                        IconTone::Danger,
                        RecordingAreaAction::Cancel,
                        cx,
                    ));
            }
            RecordingState::Saving => bar = bar.child(label_style(i18n.tr("recording-ui-saving"))),
            RecordingState::Error { reason } => {
                bar = bar
                    .child(label_style(i18n.tr_with(
                        "recording-ui-failed",
                        &Args::new().named("reason", shorten_reason(&reason.message(i18n))),
                    )))
                    .child(self.button(
                        "rec-dismiss",
                        i18n.tr("recording-ui-close"),
                        0x434343FF,
                        0xFFFFFFFF,
                        RecordingAreaAction::Dismiss,
                        cx,
                    ));
            }
            RecordingState::Idle | RecordingState::Ready | RecordingState::Finished { .. } => {}
        }
        let mut outer = div()
            .absolute()
            .left(l)
            .top(t)
            .w(w)
            .h(h)
            .flex()
            .flex_col()
            .rounded_lg()
            .bg(rgba(0x1F1F1FEE))
            .border_1()
            .border_color(rgba(0xFFFFFF26))
            .child(bar);
        if let Some(text) = notice_text {
            outer = outer.child(
                div()
                    .w_full()
                    .h(px(NOTICE_LOGICAL_HEIGHT))
                    .px_3()
                    .flex()
                    .items_center()
                    .text_size(px(11.0))
                    .text_color(rgba(COLOR_NOTICE))
                    .child(text),
            );
        }
        root.child(outer)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::recording::model::RecordingConfig;
    use crate::recording::runtime::{LinkEvent, RecorderLink};
    use snow_recorder_protocol::{Command, Event};
    use std::cell::RefCell;
    use std::rc::Rc;

    /// 假通道与测试共享的数据：已发送命令、待取事件。
    type Shared = Rc<RefCell<(Vec<Command>, Vec<LinkEvent>)>>;

    /// 假通道：记录命令，事件由测试注入。
    struct Link(Shared);

    impl RecorderLink for Link {
        /// 记录命令。
        fn send(&mut self, command: &Command) -> Result<(), String> {
            self.0.borrow_mut().0.push(command.clone());
            Ok(())
        }
        /// 取走事件。
        fn poll(&mut self) -> Vec<LinkEvent> {
            std::mem::take(&mut self.0.borrow_mut().1)
        }
        /// 无操作。
        fn shutdown(&mut self) {}
    }

    /// 构造未开始的视图与假通道（区域 100,100 800x600，显示器原点 0,0 1920x1080）。
    fn idle_view(config: RecordingConfig) -> (RecordingAreaView, Shared) {
        let shared = Rc::new(RefCell::new((Vec::new(), Vec::new())));
        let config = RecordingConfig {
            region: PhysicalRect::new(100, 100, 800, 600),
            ..config
        };
        let v = RecordingAreaView::new(config, PhysicalRect::new(0, 0, 1920, 1080), 1.0);
        (v, shared)
    }

    /// 构造已开始的视图（同 [`idle_view`]，默认配置）。
    fn view(countdown: u32) -> (RecordingAreaView, Shared) {
        let (mut v, shared) = idle_view(RecordingConfig::default());
        v.session.begin(Box::new(Link(shared.clone())), countdown);
        (v, shared)
    }

    /// 键位动作按状态生效：录制中可导出 / 暂停 / 复制，Esc 只放弃倒计时，录制中的 Esc 不丢录像。
    #[test]
    fn key_actions_follow_state() {
        let (mut v, shared) = idle_view(RecordingConfig::default());
        // 空闲时导出 / Esc 都不起作用
        v.handle_key_action(RecordKeyAction::Export);
        v.handle_key_action(RecordKeyAction::EndRecording);
        assert!(shared.borrow().0.is_empty() && !v.cancelled);

        v.session.begin(Box::new(Link(shared.clone())), 3);
        v.handle_key_action(RecordKeyAction::Export);
        assert!(
            shared.borrow().0.is_empty(),
            "倒计时中不能导出，也还没发过 START"
        );
        v.handle_key_action(RecordKeyAction::EndRecording);
        assert!(v.cancelled, "倒计时中 Esc 放弃");

        let (mut v, shared) = view(0);
        v.handle_key_action(RecordKeyAction::ToggleRecording);
        assert_eq!(shared.borrow().0.last(), Some(&Command::Pause));
        v.handle_key_action(RecordKeyAction::EndRecording);
        assert!(!v.cancelled, "录制中 Esc 不丢录像");
        v.handle_key_action(RecordKeyAction::CopyToClipboard);
        assert!(v.copy_on_finish());
        assert_eq!(shared.borrow().0.last(), Some(&Command::Stop));
    }

    /// 音频降级提示：只在录制期间出现，随语言切换，并让控制条增高一行。
    #[test]
    fn audio_notice_shows_and_grows_toolbar() {
        use snow_recorder_protocol::{AudioRequest, AudioSource, AudioStatus};
        let (mut v, shared) = idle_view(RecordingConfig {
            audio: AudioRequest {
                microphone: true,
                system: true,
                ..AudioRequest::default()
            },
            ..RecordingConfig::default()
        });
        v.set_locale("zh-CN");
        v.session.begin(Box::new(Link(shared.clone())), 0);
        let plain_h = v.layout().toolbar.height;
        assert_eq!(v.audio_notice_text(), None);
        let event = |source, status| LinkEvent::Event(Event::AudioState { source, status });
        shared
            .borrow_mut()
            .1
            .push(event(AudioSource::Microphone, AudioStatus::Unavailable));
        assert!(v.advance(Instant::now()));
        assert_eq!(v.audio_notice_text().as_deref(), Some("麦克风不可用"));
        assert!(v.layout().toolbar.height > plain_h);
        assert_eq!(v.layout(), v.layout_with_notice(true));
        shared
            .borrow_mut()
            .1
            .push(event(AudioSource::System, AudioStatus::Unavailable));
        v.advance(Instant::now());
        assert_eq!(v.audio_notice_text().as_deref(), Some("本次录制没有声音"));
        v.set_locale("en-US");
        assert_eq!(
            v.audio_notice_text().as_deref(),
            Some("This recording has no sound")
        );
    }

    /// 边框画在选区外侧且不与选区相交（不会进入被录制画面）。
    #[test]
    fn border_stays_outside_region() {
        let region = PhysicalRect::new(100, 100, 400, 300);
        let l = compute_layout(
            region,
            PhysicalRect::new(0, 0, 1920, 1080),
            1.0,
            false,
            false,
        );
        assert_eq!(l.border.len(), 4);
        for strip in &l.border {
            assert!(strip.intersect(&region).is_none(), "{strip:?} 侵入选区");
        }
    }

    /// 选区贴屏幕边时，越界的边框条被裁掉；全屏选区没有边框。
    #[test]
    fn border_clipped_at_screen_edge() {
        let win = PhysicalRect::new(0, 0, 1920, 1080);
        let l = compute_layout(PhysicalRect::new(0, 0, 1920, 1080), win, 1.0, false, false);
        assert!(l.border.is_empty());
        let l = compute_layout(PhysicalRect::new(0, 50, 500, 400), win, 1.0, false, false);
        assert!(l.border.iter().all(|s| win.intersect(s) == Some(*s)));
    }

    /// 控制条落在窗口内；倒计时框仅倒计时期间存在，居中于选区。
    #[test]
    fn toolbar_and_countdown_box() {
        let win = PhysicalRect::new(0, 0, 1920, 1080);
        let region = PhysicalRect::new(200, 200, 600, 400);
        let l = compute_layout(region, win, 1.5, true, false);
        assert_eq!(win.intersect(&l.toolbar), Some(l.toolbar));
        let b = l.countdown_box.unwrap();
        assert_eq!(b.x + b.width / 2, region.x + region.width / 2);
        assert!(
            compute_layout(region, win, 1.0, false, false)
                .countdown_box
                .is_none()
        );
        assert_eq!(l.hit_rects().len(), l.border.len() + 2);
    }

    /// 选区窗口坐标 = 虚拟桌面坐标 - 显示器原点（副屏在左侧时为负原点）。
    #[test]
    fn region_uses_monitor_origin() {
        let config = RecordingConfig {
            region: PhysicalRect::new(-1800, 100, 400, 300),
            ..RecordingConfig::default()
        };
        let v = RecordingAreaView::new(config, PhysicalRect::new(-1920, 0, 1920, 1080), 1.0);
        assert_eq!(v.region_in_window(), PhysicalRect::new(120, 100, 400, 300));
    }

    /// 倒计时按秒推进，结束时发出 START。
    #[test]
    fn countdown_advances_by_time() {
        let (mut v, shared) = view(2);
        let t0 = Instant::now();
        assert!(!v.advance(t0));
        assert!(v.advance(t0 + COUNTDOWN_STEP));
        assert!(matches!(
            v.session.state(),
            RecordingState::Countdown { seconds_left: 1 }
        ));
        assert!(v.advance(t0 + 2 * COUNTDOWN_STEP));
        assert!(v.session.state().is_active());
        assert!(matches!(shared.borrow().0.first(), Some(Command::Start(_))));
    }

    /// 控制条动作：暂停 / 完成 / 取消 / 关闭提示。
    #[test]
    fn actions_drive_session() {
        let (mut v, shared) = view(0);
        v.handle_action(RecordingAreaAction::TogglePause);
        v.handle_action(RecordingAreaAction::StopAndSave);
        assert_eq!(*v.session.state(), RecordingState::Saving);
        assert_eq!(shared.borrow().0[1..], [Command::Pause, Command::Stop]);
        assert!(!v.is_over(Instant::now()));
        v.handle_action(RecordingAreaAction::Cancel);
        assert!(v.is_over(Instant::now()));
    }

    /// 录制进程崩溃后窗口会在提示停留时间后自动关闭，也可手动关闭。
    #[test]
    fn error_auto_closes_after_display_time() {
        let (mut v, shared) = view(0);
        shared.borrow_mut().1.push(LinkEvent::Exited { code: None });
        let t0 = Instant::now();
        assert!(v.advance(t0));
        assert!(matches!(v.session.state(), RecordingState::Error { .. }));
        assert!(!v.is_over(t0));
        assert!(v.is_over(t0 + ERROR_DISPLAY));
        let (mut v2, shared2) = view(0);
        shared2
            .borrow_mut()
            .1
            .push(LinkEvent::Event(Event::Error { reason: "x".into() }));
        v2.advance(t0);
        v2.handle_action(RecordingAreaAction::Dismiss);
        assert!(v2.is_over(t0));
    }

    /// 自动化计划：到点暂停、暂停够久后恢复、录满后停止。
    #[test]
    fn auto_plan_pauses_resumes_and_stops() {
        let (mut v, shared) = view(0);
        v.set_auto_plan(AutoPlan {
            stop_after_secs: 4,
            pause: Some((2, 3)),
        });
        let t0 = Instant::now();
        let recording = |ms: u64| {
            LinkEvent::Event(Event::Recording {
                elapsed_ms: ms,
                frames: 0,
            })
        };
        shared.borrow_mut().1.push(recording(2100));
        v.advance(t0);
        assert_eq!(shared.borrow().0.last(), Some(&Command::Pause));
        shared.borrow_mut().1.push(LinkEvent::Event(Event::Paused));
        v.advance(t0 + Duration::from_secs(1));
        assert_eq!(
            shared.borrow().0.last(),
            Some(&Command::Pause),
            "暂停未满不应恢复"
        );
        v.advance(t0 + Duration::from_secs(4));
        assert_eq!(shared.borrow().0.last(), Some(&Command::Resume));
        shared.borrow_mut().1.push(LinkEvent::Event(Event::Resumed));
        shared.borrow_mut().1.push(recording(4200));
        v.advance(t0 + Duration::from_secs(5));
        assert_eq!(shared.borrow().0.last(), Some(&Command::Stop));
    }

    /// 构造处于就绪态的视图与假通道。
    fn ready_view(config: RecordingConfig) -> (RecordingAreaView, Shared) {
        let (mut v, shared) = idle_view(config);
        v.session.attach_ready(Box::new(Link(shared.clone())));
        (v, shared)
    }

    /// 选区完成进入就绪态：不发 START；点开始才发，并带上就绪态改过的选项。
    #[test]
    fn ready_waits_then_start_carries_choices() {
        use snow_recorder_protocol::AudioRequest;
        let (mut v, shared) = ready_view(RecordingConfig {
            audio: AudioRequest {
                microphone: true,
                system: true,
                ..AudioRequest::default()
            },
            ..RecordingConfig::default()
        });
        assert!(v.session.state().is_ready() && v.ready_panel_visible());
        assert!(shared.borrow().0.is_empty(), "就绪态不发 START");
        assert!(v.ready_set_fps(60) && v.ready_set_countdown(0) && v.ready_set_cursor(false));
        assert!(v.ready_set_output(OutputPreset::P480));
        assert!(v.ready_set_mic(false) && v.ready_set_system(true));
        v.handle_action(RecordingAreaAction::Start);
        match &shared.borrow().0[..] {
            [Command::Start(r)] => {
                assert_eq!((r.fps, r.show_cursor), (60, false));
                assert_eq!(r.output_size, Some((640, 480)));
                assert!(!r.audio.microphone && r.audio.system);
            }
            other => panic!("期望单条 START，实际 {other:?}"),
        }
    }

    /// 开始后选项面板隐藏、选项不可再改（含暂停期间）。
    #[test]
    fn options_hidden_and_locked_after_start() {
        let (mut v, shared) = ready_view(RecordingConfig::default());
        v.handle_action(RecordingAreaAction::Start);
        assert!(!v.ready_panel_visible());
        assert!(!v.ready_set_fps(15) && !v.ready_set_cursor(true));
        assert!(!v.ready_apply_range(RangePreset::Square));
        v.handle_action(RecordingAreaAction::TogglePause);
        shared.borrow_mut().1.push(LinkEvent::Event(Event::Paused));
        v.advance(Instant::now());
        assert!(!v.ready_panel_visible(), "暂停期间也不显示");
        assert!(!v.ready_set_output(OutputPreset::P480));
        assert_eq!(v.layout().toolbar.height, 40, "录制中回到普通控制条");
    }

    /// 就绪态 Esc / 放弃：不发 START，发 CANCEL，窗口应关闭。
    #[test]
    fn ready_cancel_sends_no_start() {
        let (mut v, shared) = ready_view(RecordingConfig::default());
        v.handle_key_action(RecordKeyAction::EndRecording);
        assert!(v.is_over(Instant::now()));
        assert_eq!(*v.session.state(), RecordingState::Idle);
        assert_eq!(shared.borrow().0, [Command::Cancel]);
    }

    /// 范围预设：选区、输出尺寸与命中区域同步刷新，且夹在显示器内。
    #[test]
    fn bar_width_follows_content() {
        let rec = |edit| {
            bar_logical_width(
                &RecordingState::Recording {
                    elapsed_secs: 0,
                    is_paused: false,
                    frames_captured: 0,
                },
                edit,
                false,
            )
        };
        assert!(rec(true) > rec(false) && rec(false) < TOOLBAR_LOGICAL_WIDTH);
        assert_eq!(
            bar_logical_width(&RecordingState::Saving, false, true),
            TOOLBAR_LOGICAL_WIDTH
        );
    }

    #[test]
    fn range_preset_updates_region_layout_and_output() {
        let (mut v, _shared) = ready_view(RecordingConfig::default());
        let before = v.layout().hit_rects();
        assert!(v.ready_set_output(OutputPreset::P480));
        assert!(v.ready_apply_range(RangePreset::Portrait9x16));
        let r = v.bounds();
        assert_eq!((r.width, r.height), (336, 600));
        assert_eq!(
            v.session.config().output_size,
            None,
            "短边 336 低于 480 不放大"
        );
        assert_ne!(v.layout().hit_rects(), before);
        let screen = PhysicalRect::new(0, 0, 1920, 1080);
        assert_eq!(screen.intersect(&r), Some(r));
        assert!(v.ready_apply_range(RangePreset::FullScreen));
        assert_eq!(v.bounds(), screen);
        assert_eq!(v.session.config().output_size, Some((852, 480)));
    }

    /// 恢复：多次用预设 / 拖拽改选区后，回到最初框选的矩形，输出尺寸与命中区域同步重算。
    #[test]
    fn restore_returns_to_initial_region() {
        let (mut v, _s) = ready_view(RecordingConfig::default());
        let initial = PhysicalRect::new(100, 100, 800, 600);
        assert!(v.ready_set_output(OutputPreset::P480));
        assert!(v.ready_apply_range(RangePreset::Square));
        v.begin_drag(DragEdge::Right);
        v.drag_to((1500, 0));
        v.end_drag();
        assert!(v.ready_apply_range(RangePreset::FullScreen));
        let changed_hit = v.layout().hit_rects();
        assert!(v.ready_apply_range(RangePreset::Restore));
        assert_eq!(v.bounds(), initial);
        assert_eq!(v.session.config().output_size, Some((640, 480)));
        assert_ne!(v.layout().hit_rects(), changed_hit);
        // 重复恢复结果不变
        assert!(v.ready_apply_range(RangePreset::Restore));
        assert_eq!(v.bounds(), initial);
    }

    /// 手柄只在就绪态出现；开始后（含暂停）隐藏且不可拖。
    #[test]
    fn handles_only_in_ready_and_gone_after_start() {
        let (mut v, shared) = ready_view(RecordingConfig::default());
        assert_eq!(v.layout().handles.len(), 4);
        v.begin_drag(DragEdge::Left);
        v.handle_action(RecordingAreaAction::Start);
        assert!(v.layout().handles.is_empty());
        assert!(!v.handles_visible());
        let before = v.bounds();
        v.begin_drag(DragEdge::Left);
        assert!(!v.drag_to((0, 0)));
        assert_eq!(v.bounds(), before);
        v.handle_action(RecordingAreaAction::TogglePause);
        shared.borrow_mut().1.push(LinkEvent::Event(Event::Paused));
        v.advance(Instant::now());
        assert!(v.layout().handles.is_empty());
    }

    /// 拖内部整体平移：尺寸不变，位置跟着鼠标位移走，开始后不可平移。
    #[test]
    fn move_drag_translates_region() {
        let (mut v, _s) = ready_view(RecordingConfig::default());
        assert!(v.layout().move_zone.is_some());
        let r0 = v.bounds();
        v.begin_move((300, 300));
        assert!(v.drag_to((350, 340)));
        let r = v.bounds();
        assert_eq!((r.x, r.y), (r0.x + 50, r0.y + 40));
        assert_eq!((r.width, r.height), (r0.width, r0.height));
        v.end_drag();
        v.handle_action(RecordingAreaAction::Start);
        assert!(v.layout().move_zone.is_none());
        let before = v.bounds();
        v.begin_move((0, 0));
        assert!(!v.drag_to((90, 90)));
        assert_eq!(v.bounds(), before);
    }

    /// 录制中控制条用到的图标资源都能解析到。
    #[test]
    fn bar_icons_resolve() {
        for path in [ICON_PAUSE, ICON_RESUME, ICON_EDIT, ICON_FINISH, ICON_CLOSE] {
            assert!(snow_ui::ui::icon_asset_exists(path), "缺少图标 {path}");
        }
    }

    /// 控制条定位：选区装得下就在内部右下角，装不下才放到选区外。
    #[test]
    fn toolbar_prefers_inside_bottom_right() {
        let window = PhysicalRect::new(0, 0, 1920, 1080);
        let size = PhysicalPoint::new(300, 40);
        let p = place_toolbar(PhysicalRect::new(100, 100, 800, 600), size, window, 8);
        assert_eq!((p.x, p.y), (592, 652));
        // 选区太窄：放到选区外（下方）
        let p = place_toolbar(PhysicalRect::new(100, 100, 200, 100), size, window, 8);
        assert_eq!(p.y, 208);
        // 内部放置不随选区上下位置翻转
        let a = place_toolbar(PhysicalRect::new(100, 100, 800, 600), size, window, 8);
        let b = place_toolbar(PhysicalRect::new(100, 440, 800, 600), size, window, 8);
        assert_eq!(b.y - a.y, 340);
    }

    /// 就绪态有底图时命中区域是整窗；开始后先保留底图，宿主裁剪好命中区域再释放。
    #[test]
    fn backdrop_makes_hit_full_window_until_released() {
        let (mut v, _s) = ready_view(RecordingConfig::default());
        let window = PhysicalRect::new(0, 0, 1920, 1080);
        assert_ne!(v.layout().hit_rects(), vec![window], "没底图时仍是裁剪");
        let pixels = image::RgbaImage::from_raw(2, 2, vec![0; 16]).unwrap();
        v.set_backdrop(Arc::new(RenderImage::new(vec![image::Frame::new(pixels)])));
        assert_eq!(v.layout().hit_rects(), vec![window]);
        assert!(!v.release_backdrop_if_started(), "就绪态不释放");
        v.handle_action(RecordingAreaAction::Start);
        assert_ne!(
            v.layout().hit_rects(),
            vec![window],
            "开始后命中区域回到裁剪"
        );
        assert!(v.release_backdrop_if_started());
        assert!(!v.release_backdrop_if_started(), "只释放一次");
    }

    /// 命中区域包含每个手柄的可点击范围，手柄落在窗口内。
    #[test]
    fn hit_rects_include_handles() {
        let (v, _s) = ready_view(RecordingConfig::default());
        let l = v.layout();
        let hits = l.hit_rects();
        let window = PhysicalRect::new(0, 0, 1920, 1080);
        for (_, r) in &l.handles {
            assert!(hits.contains(r));
            assert_eq!(window.intersect(r), Some(*r));
        }
        // 上手柄以上边中点为中心
        let top = l.handles[0].1;
        assert_eq!((top.x + top.width / 2, top.y + top.height / 2), (500, 100));
    }

    /// 拖动手柄实时更新选区、输出尺寸（保持档位）与命中区域，对边固定。
    #[test]
    fn drag_updates_region_output_and_hit() {
        let (mut v, _s) = ready_view(RecordingConfig::default());
        assert!(v.ready_set_output(OutputPreset::P480));
        let hit0 = v.layout().hit_rects();
        v.begin_drag(DragEdge::Bottom);
        assert!(v.drag_to((0, 1000)));
        let r = v.bounds();
        assert_eq!((r.x, r.y, r.width, r.height), (100, 100, 800, 900));
        assert_eq!(v.session.config().output_size, Some((480, 540)));
        assert_ne!(v.layout().hit_rects(), hit0);
        // 拖过对边：钳在最小尺寸
        assert!(v.drag_to((0, 0)));
        assert_eq!(v.bounds().height, MIN_SELECTION_SIZE);
        v.end_drag();
        assert!(!v.drag_to((0, 500)), "松手后不再响应");
    }

    /// 非 MP4 格式：音频开关无效（界面置灰），范围外的选项仍可改。
    #[test]
    fn audio_controls_follow_format() {
        let (mut v, _s) = ready_view(RecordingConfig {
            format: RecordingFormat::Gif,
            ..RecordingConfig::default()
        });
        assert!(!v.ready_set_mic(true) && !v.ready_set_system(true));
        assert!(!v.session.config().audio.enabled());
        assert!(v.ready_set_fps(15));
    }

    /// 构造一条“录制完成”事件。
    fn finished_event() -> LinkEvent {
        LinkEvent::Event(Event::Finished {
            path: "out.mp4".into(),
            frames: 1,
            dropped: 0,
        })
    }

    /// 编辑按钮只在录制中（含暂停）出现；Ready / 倒计时 / 保存中 / 完成态都没有。
    #[test]
    fn edit_button_only_while_recording() {
        let (mut v, _s) = ready_view(RecordingConfig::default());
        v.set_edit_enabled(true);
        assert!(!v.edit_button_visible(), "就绪态没有编辑按钮");
        let (mut v, _s) = view(3);
        v.set_edit_enabled(true);
        assert!(!v.edit_button_visible(), "倒计时没有编辑按钮");
        let (mut v, shared) = view(0);
        v.set_edit_enabled(true);
        assert!(v.edit_button_visible());
        v.handle_action(RecordingAreaAction::TogglePause);
        shared.borrow_mut().1.push(LinkEvent::Event(Event::Paused));
        v.advance(Instant::now());
        assert!(v.edit_button_visible(), "暂停中也有编辑按钮");
        v.set_edit_enabled(false);
        assert!(!v.edit_button_visible(), "自动化验证不提供");
        v.set_edit_enabled(true);
        v.handle_action(RecordingAreaAction::Edit);
        assert!(!v.edit_button_visible(), "保存中没有编辑按钮");
        shared.borrow_mut().1.push(finished_event());
        v.advance(Instant::now());
        assert!(!v.edit_button_visible(), "完成态没有编辑按钮");
    }

    /// 点编辑 = 发 STOP 并记下“去编辑”意图；文件写完后只交出一次路径，窗口当拍关闭。
    #[test]
    fn edit_stops_and_hands_over_file() {
        let (mut v, shared) = view(0);
        v.set_edit_enabled(true);
        v.handle_action(RecordingAreaAction::Edit);
        assert_eq!(shared.borrow().0.last(), Some(&Command::Stop));
        assert_eq!(*v.session.state(), RecordingState::Saving);
        assert!(v.take_edit_request().is_none(), "保存中不交文件，意图保留");
        shared.borrow_mut().1.push(finished_event());
        let t0 = Instant::now();
        v.advance(t0);
        assert!(v.is_over(t0), "完成后当拍关窗，不再停留");
        assert_eq!(v.take_edit_request(), Some("out.mp4".into()));
        assert!(v.take_edit_request().is_none());
    }

    /// 普通停止不带意图；点编辑后出错 / 取消，意图被清除。
    #[test]
    fn edit_intent_cleared_on_error_and_cancel() {
        let (mut v, _s) = view(0);
        v.set_edit_enabled(true);
        v.handle_action(RecordingAreaAction::StopAndSave);
        assert!(!v.edit_after_finish);

        let (mut v, shared) = view(0);
        v.set_edit_enabled(true);
        v.handle_action(RecordingAreaAction::Edit);
        assert!(v.edit_after_finish);
        shared
            .borrow_mut()
            .1
            .push(LinkEvent::Event(Event::Error { reason: "x".into() }));
        v.advance(Instant::now());
        assert!(v.take_edit_request().is_none());
        assert!(!v.edit_after_finish, "出错后意图清除");

        let (mut v, _s) = view(0);
        v.set_edit_enabled(true);
        v.handle_action(RecordingAreaAction::Edit);
        v.handle_action(RecordingAreaAction::Cancel);
        assert!(!v.edit_after_finish, "取消后意图清除");
    }

    /// 完成即关闭，不再停留（含开启编辑功能时）；普通停止不去编辑。
    #[test]
    fn finished_closes_immediately() {
        let (mut v, shared) = view(0);
        v.set_edit_enabled(true);
        v.handle_action(RecordingAreaAction::StopAndSave);
        shared.borrow_mut().1.push(finished_event());
        v.advance(Instant::now());
        assert!(v.is_over(Instant::now()));
        assert!(v.take_edit_request().is_none());
    }

    /// 长错误文案被截断并带省略号。
    #[test]
    fn reason_is_shortened() {
        assert_eq!(shorten_reason("短"), "短");
        let long = "很".repeat(100);
        let s = shorten_reason(&long);
        assert_eq!(s.chars().count(), ERROR_MAX_CHARS + 1);
        assert!(s.ends_with('…'));
    }
}
