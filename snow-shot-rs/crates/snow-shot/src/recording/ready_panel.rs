//! 录制就绪面板（Ready Panel）：选区完成后、点“开始”之前显示的可选项面板。
//!
//! 单行工具栏：下拉用 gpui-component 的 `Select`（图标加文字），开关与按钮用纯图标 `Button`；所有改动只落在本次录制的配置里，
//! 不写回设置页。点了开始后整个面板不再渲染（见 `RecordingAreaView::ready_panel_visible`）。

use crate::recording::area_view::{
    BAR_BUTTON, BAR_DIVIDER, BAR_FRAME, BAR_GAP, BAR_SIZE_WIDTH, IconTone,
    READY_PANEL_LOGICAL_HEIGHT, RecordingAreaAction, RecordingAreaView,
};
use crate::recording::audio::MSG_MP4_ONLY;
use crate::recording::ready_options::{
    COUNTDOWN_CHOICES, FPS_CHOICES, OutputPreset, RangePreset, audio_available,
};
use crate::select_kit::{CompactSelect, compact_select};
use snow_i18n::{Args, I18n};
use snow_ui::ui::component::button::{Button, ButtonVariants};
use snow_ui::ui::component::searchable_list::{SearchableListItem, SearchableVec};
use snow_ui::ui::component::select::{SelectEvent, SelectState};
use snow_ui::ui::component::{
    Disableable, Icon, IndexPath, Selectable, Sizable, Size as ComponentSize, Theme, ThemeMode,
};
use snow_ui::ui::*;

/// 帧率下拉的逻辑宽度。
const SELECT_WIDTH_FPS: f32 = 100.0;
/// 输出尺寸下拉的逻辑宽度。
const SELECT_WIDTH_SIZE: f32 = 100.0;
/// 倒计时下拉的逻辑宽度。
const SELECT_WIDTH_COUNTDOWN: f32 = 100.0;
/// 范围预设下拉的逻辑宽度。
const SELECT_WIDTH_RANGE: f32 = 132.0;
/// 就绪工具栏逻辑宽度：由各控件宽度累加得出（按内容自适应，不留空白）。
/// 子元素共 13 个：尺寸文字、4 个下拉、3 个开关、开始 / 放弃，另有 3 条分隔线。
pub(crate) const READY_LOGICAL_WIDTH: f32 = BAR_FRAME
    + BAR_SIZE_WIDTH
    + SELECT_WIDTH_FPS
    + SELECT_WIDTH_SIZE
    + SELECT_WIDTH_COUNTDOWN
    + SELECT_WIDTH_RANGE
    + 5.0 * BAR_BUTTON
    + 3.0 * BAR_DIVIDER
    + 12.0 * BAR_GAP;
/// 开始按钮图标。
const ICON_START: &str = "icons/antd/play-circle.svg";
/// 放弃按钮图标。
const ICON_CANCEL: &str = "icons/antd/close.svg";
/// 帧率下拉图标。
const ICON_FPS: &str = "icons/antd/dashboard.svg";
/// 输出尺寸下拉图标。
const ICON_SIZE: &str = "icons/antd/expand-alt.svg";
/// 倒计时下拉图标。
const ICON_COUNTDOWN: &str = "icons/antd/clock-circle.svg";
/// 范围预设下拉图标。
const ICON_RANGE: &str = "icons/antd/scan.svg";
/// 麦克风开启图标。
const ICON_MIC: &str = "icons/antd/audio.svg";
/// 麦克风关闭图标。
const ICON_MIC_OFF: &str = "icons/antd/audio-muted.svg";
/// 系统声开启图标。
const ICON_SYSTEM: &str = "icons/antd/sound.svg";
/// 系统声关闭图标。
const ICON_SYSTEM_OFF: &str = "icons/antd/desktop.svg";
/// 显示光标开关图标。
const ICON_CURSOR: &str = "icons/antd/aim.svg";
/// 下拉触发器的逻辑高度。
const SELECT_HEIGHT: f32 = 28.0;
/// 下拉菜单最大逻辑高度（需小于面板下方预留的命中区域）。
const MENU_MAX_HEIGHT: f32 = 200.0;
/// 面板底色。
const COLOR_PANEL: u32 = 0x1F1F1FEE;
/// 面板描边色。
const COLOR_PANEL_BORDER: u32 = 0xFFFFFF26;
/// 次要文字颜色。
const COLOR_MUTED: u32 = 0x8C8C8CFF;
/// 主要文字颜色。
const COLOR_TEXT: u32 = 0xFFFFFFFF;

/// 下拉里的一个选项：取值文本加本地化标签。
#[derive(Clone)]
pub struct ReadyItem {
    /// 选项取值（数字或下标的文本）。
    value: String,
    /// 界面显示的标签。
    label: SharedString,
}

impl SearchableListItem for ReadyItem {
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

/// 就绪面板下拉的状态实体类型。
type ReadySelect = SelectState<SearchableVec<ReadyItem>>;

/// 就绪面板里的下拉种类。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ReadySelectKind {
    /// 帧率。
    Fps,
    /// 输出尺寸档。
    Size,
    /// 倒计时。
    Countdown,
    /// 范围预设。
    Range,
}

/// 就绪面板的四个下拉。
pub struct ReadyUi {
    /// 帧率下拉。
    fps: Entity<ReadySelect>,
    /// 输出尺寸下拉。
    size: Entity<ReadySelect>,
    /// 倒计时下拉。
    countdown: Entity<ReadySelect>,
    /// 范围预设下拉。
    range: Entity<ReadySelect>,
}

/// 构造选项。
fn item(value: impl ToString, label: String) -> ReadyItem {
    ReadyItem {
        value: value.to_string(),
        label: label.into(),
    }
}

/// 某个下拉的选项列表。
fn items_for(kind: ReadySelectKind, i18n: &I18n) -> Vec<ReadyItem> {
    match kind {
        ReadySelectKind::Fps => FPS_CHOICES
            .iter()
            .map(|f| {
                item(
                    f,
                    i18n.tr_with(
                        "recording-ready-fps-option",
                        &Args::new().named("fps", f.to_string()),
                    ),
                )
            })
            .collect(),
        ReadySelectKind::Size => OutputPreset::ALL
            .iter()
            .enumerate()
            .map(|(i, p)| item(i, i18n.tr(p.message_id())))
            .collect(),
        ReadySelectKind::Countdown => COUNTDOWN_CHOICES
            .iter()
            .map(|s| {
                let label = if *s == 0 {
                    i18n.tr("recording-ready-countdown-off")
                } else {
                    i18n.tr_with(
                        "recording-ready-countdown-secs",
                        &Args::new().named("secs", s.to_string()),
                    )
                };
                item(s, label)
            })
            .collect(),
        ReadySelectKind::Range => RangePreset::ALL
            .iter()
            .enumerate()
            .map(|(i, p)| item(i, i18n.tr(p.message_id())))
            .collect(),
    }
}

impl RecordingAreaView {
    /// 创建就绪面板的四个下拉并订阅选中事件（视图实体创建时由开窗回调调用）。
    ///
    /// # 参数
    /// - `window`：所在窗口。
    /// - `cx`：视图上下文。
    pub fn init_ready_ui(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        // 录制窗始终是深色界面，下拉组件跟随深色主题
        Theme::change(ThemeMode::Dark, None, cx);
        let i18n = crate::ocr_backend::i18n_for(self.locale());
        let config = self.session.config();
        let fps_idx = FPS_CHOICES.iter().position(|f| *f == config.fps);
        let countdown_idx = COUNTDOWN_CHOICES
            .iter()
            .position(|s| *s == config.countdown_secs);
        let mut mk = |kind: ReadySelectKind, selected: Option<usize>| {
            let items = items_for(kind, i18n);
            let state = cx.new(|cx| {
                SelectState::new(
                    SearchableVec::new(items),
                    selected.map(|i| IndexPath::default().row(i)),
                    window,
                    cx,
                )
            });
            cx.subscribe_in(
                &state,
                window,
                move |this, _state, event: &SelectEvent<SearchableVec<ReadyItem>>, _window, cx| {
                    if let SelectEvent::Confirm(Some(value)) = event {
                        this.on_ready_select(kind, value);
                        cx.notify();
                    }
                },
            )
            .detach();
            state
        };
        self.ready_ui = Some(ReadyUi {
            fps: mk(ReadySelectKind::Fps, fps_idx),
            size: mk(ReadySelectKind::Size, Some(0)),
            countdown: mk(ReadySelectKind::Countdown, countdown_idx),
            range: mk(ReadySelectKind::Range, None),
        });
    }

    /// 处理就绪面板某个下拉的选中值，改写本次录制配置。
    fn on_ready_select(&mut self, kind: ReadySelectKind, value: &str) {
        let Ok(n) = value.parse::<usize>() else {
            return;
        };
        match kind {
            ReadySelectKind::Fps => {
                self.ready_set_fps(n as u32);
            }
            ReadySelectKind::Countdown => {
                self.ready_set_countdown(n as u32);
            }
            ReadySelectKind::Size => {
                if let Some(p) = OutputPreset::ALL.get(n) {
                    self.ready_set_output(*p);
                }
            }
            ReadySelectKind::Range => {
                if let Some(p) = RangePreset::ALL.get(n) {
                    self.ready_apply_range(*p);
                }
            }
        }
    }
}

/// 渲染就绪工具栏（单行长条）：最左是当前选区尺寸，其后帧率 / 输出尺寸 / 倒计时 / 范围下拉（图标加文字）、
/// 麦克风 / 系统声 / 光标开关（纯图标，开关时图标切换，悬停出文字），最右是开始 / 放弃。
///
/// # 参数
/// - `view`：录制视图（读当前配置）。
/// - `i18n`：界面语料。
/// - `(left, top, width)`：工具栏的逻辑位置与宽度。
/// - `cx`：视图上下文。
pub fn render_ready_panel(
    view: &RecordingAreaView,
    i18n: &I18n,
    (left, top, width): (Pixels, Pixels, Pixels),
    cx: &mut Context<RecordingAreaView>,
) -> Div {
    let config = view.session.config();
    let (region, show_cursor) = (config.region, config.show_cursor);
    let audio = config.audio.clone();
    let audio_ok = audio_available(config.format);
    let select = |state: &Entity<ReadySelect>, icon: &'static str, width: f32, hint: &str| {
        compact_select(
            state,
            (width, SELECT_HEIGHT),
            MENU_MAX_HEIGHT,
            CompactSelect {
                icon: Some(icon),
                placeholder: Some(SharedString::from(hint.to_string())),
            },
        )
    };
    let entity = cx.entity();
    let toggle = |id: &'static str,
                  icon: &'static str,
                  text_id: &str,
                  on: bool,
                  enabled: bool,
                  set: fn(&mut RecordingAreaView, bool) -> bool| {
        let entity = entity.clone();
        // 格式不支持音频时，悬停提示直接说明原因
        let tip = if enabled || icon == ICON_CURSOR {
            i18n.tr(text_id)
        } else {
            i18n.tr(MSG_MP4_ONLY)
        };
        Button::new(id)
            .ghost()
            .with_size(ComponentSize::Small)
            .icon(Icon::default().path(icon))
            .selected(on)
            .disabled(!enabled)
            .tooltip(tip)
            .on_click(move |_, _window, app| {
                entity.update(app, |this, cx| {
                    set(this, !on);
                    cx.notify();
                });
            })
    };
    let divider = || {
        div()
            .flex_none()
            .w(px(1.0))
            .h(px(18.0))
            .bg(rgba(COLOR_PANEL_BORDER))
    };
    let mut bar = div()
        .absolute()
        .left(left)
        .top(top)
        .w(width.min(px(READY_LOGICAL_WIDTH)))
        .h(px(READY_PANEL_LOGICAL_HEIGHT))
        .px_3()
        .flex()
        .items_center()
        .gap_2()
        .rounded_lg()
        .bg(rgba(COLOR_PANEL))
        .border_1()
        .border_color(rgba(COLOR_PANEL_BORDER))
        .text_color(rgba(COLOR_TEXT))
        .child(
            div()
                .flex_none()
                .text_size(px(12.0))
                .text_color(rgba(COLOR_MUTED))
                .child(format!("{}x{}", region.width, region.height)),
        )
        .child(divider());
    if let Some(ui) = &view.ready_ui {
        bar = bar
            .child(select(
                &ui.fps,
                ICON_FPS,
                SELECT_WIDTH_FPS,
                &i18n.tr("recording-ready-fps"),
            ))
            .child(select(
                &ui.size,
                ICON_SIZE,
                SELECT_WIDTH_SIZE,
                &i18n.tr("recording-ready-size"),
            ))
            .child(select(
                &ui.countdown,
                ICON_COUNTDOWN,
                SELECT_WIDTH_COUNTDOWN,
                &i18n.tr("recording-ready-countdown"),
            ))
            .child(select(
                &ui.range,
                ICON_RANGE,
                SELECT_WIDTH_RANGE,
                &i18n.tr("recording-ready-range"),
            ))
            .child(divider());
    }
    bar.child(toggle(
        "rec-mic",
        if audio.microphone {
            ICON_MIC
        } else {
            ICON_MIC_OFF
        },
        "recording-ready-mic",
        audio.microphone,
        audio_ok,
        RecordingAreaView::ready_set_mic,
    ))
    .child(toggle(
        "rec-system",
        if audio.system {
            ICON_SYSTEM
        } else {
            ICON_SYSTEM_OFF
        },
        "recording-ready-system",
        audio.system,
        audio_ok,
        RecordingAreaView::ready_set_system,
    ))
    .child(toggle(
        "rec-cursor",
        ICON_CURSOR,
        "recording-ready-cursor",
        show_cursor,
        true,
        RecordingAreaView::ready_set_cursor,
    ))
    .child(divider())
    .child(view.icon_button(
        "rec-start",
        ICON_START,
        i18n.tr("recording-ready-start"),
        IconTone::Primary,
        RecordingAreaAction::Start,
        cx,
    ))
    .child(view.icon_button(
        "rec-cancel",
        ICON_CANCEL,
        i18n.tr("recording-ui-cancel"),
        IconTone::Danger,
        RecordingAreaAction::Cancel,
        cx,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use snow_ui::ui::icon_asset_exists;

    /// 工具栏用到的图标资源都能解析到。
    #[test]
    fn toolbar_icons_resolve() {
        for path in [
            ICON_START,
            ICON_CANCEL,
            ICON_FPS,
            ICON_SIZE,
            ICON_COUNTDOWN,
            ICON_RANGE,
            ICON_MIC,
            ICON_MIC_OFF,
            ICON_SYSTEM,
            ICON_SYSTEM_OFF,
            ICON_CURSOR,
        ] {
            assert!(icon_asset_exists(path), "缺少图标 {path}");
        }
    }
}
