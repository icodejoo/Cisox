//! 设置页视图：侧栏分组、搜索框、按控件类型渲染的虚拟滚动列表、状态栏。
//!
//! 渲染只读取 [`SettingsState`] 里预先构建好的行模型；列表使用定高虚拟滚动，
//! 每帧只构建屏幕内可见的几行，与配置项总数无关。

use crate::config_transfer::{TRANSFER_GROUP_ID, TransferAction, TransferUiState, transfer_panel};
use crate::dictation::status::backend_notice as dictation_backend_notice;
use crate::dictation::translate::ModelSupport;
use crate::language_names::{is_language_key, language_option_label};
use crate::net_settings::{UPDATES_GROUP_ID, UpdateAction, UpdateUiState, update_panel};
use crate::ocr_backend::{OcrBackend, OcrNotice};
use crate::settings_model::{
    Control, SLIDER_CELLS, edit_text, parse_hex_color, preview_text, slider_active_cell,
    slider_cell_value, step_int, window_text,
};
use crate::settings_state::{
    ConfigChange, EditTarget, KeyMods, RowModel, Scope, SettingsAction, SettingsState,
    SharedConfig, StatusKind, SystemPrefs, read_only_note,
};
use crate::settings_text::{Lang, Text, group_title, item_desc, item_label, option_text, t};
use crate::stt_download::{self, Progress};
use crate::stt_models::{self, mode_as_str};
use crate::stt_settings::{
    CancelFlag, DICTATION_GROUP_ID, DownloadState, PanelAction, PanelModel, SttHooks, SttInputs,
    build_panel, is_selector_key, is_translate_note_key, model_row_status,
    option_label as stt_option_label, option_value, rescans_translate_support,
    scan_translate_support, selector_note, translate_note,
};
use crate::translate_settings::{
    HYMT2_LINE_COUNT, Hymt2Button, Hymt2Click, Hymt2Row, Hymt2View, hymt2_click, hymt2_rows,
    hymt2_view, route_hint, route_mode_label, split_list_index,
};
use serde_json::{Value, json};
use snow_config::extensions::{
    KEY_DICTATION_BACKEND, KEY_DICTATION_MODEL_ID, KEY_LOCAL_MODEL_ID, KEY_LOCAL_ROUTE_MODE,
    KEY_OCR_BACKEND,
};
use snow_ui::ui::component::button::Button;
use snow_ui::ui::component::checkbox::Checkbox;
use snow_ui::ui::component::searchable_list::{SearchableListItem, SearchableVec};
use snow_ui::ui::component::select::{Select, SelectEvent, SelectState};
use snow_ui::ui::component::{
    Disableable, IndexPath, Sizable, Size as ComponentSize, Theme, ThemeMode,
};
use snow_ui::ui::*;
use std::collections::{HashMap, HashSet};
use std::ops::Range;
use std::rc::Rc;
use std::time::{Duration, Instant};

/// 行高（逻辑像素），虚拟滚动要求定高。
const ROW_HEIGHT: f32 = 60.0;
/// 侧栏宽度。
const SIDEBAR_WIDTH: f32 = 220.0;
/// 行左侧标签列宽度。
const LABEL_WIDTH: f32 = 300.0;
/// 行说明最大高度，容纳两行 11px 文字（行高约 18px），保证定高行不被撑开。
const SUB_MAX_HEIGHT: f32 = 36.0;
/// 下拉选择器触发器宽度。
const DROPDOWN_WIDTH: f32 = 200.0;
/// 语音模型下拉触发器宽度（需完整容纳最长的「名称（推荐）」标签，约 66 个英文字符）。
const MODEL_DROPDOWN_WIDTH: f32 = 540.0;
/// 下拉选择器触发器高度。
const DROPDOWN_HEIGHT: f32 = 28.0;
/// 下拉浮层最大高度（超出后浮层内滚动）。
const DROPDOWN_MENU_MAX_HEIGHT: f32 = 280.0;
/// 文本输入框宽度。
const FIELD_WIDTH: f32 = 300.0;
/// 搜索框宽度。
const SEARCH_WIDTH: f32 = 240.0;
/// 文本框可见字符数上限。
const FIELD_VISIBLE_CHARS: usize = 36;
/// 只读预览字符数上限。
const READ_ONLY_PREVIEW_CHARS: usize = 28;
/// 插入符字符。
const CARET: &str = "\u{258F}";
/// 单行最多显示的快捷键数量。
const SHORTCUT_CHIPS_MAX: usize = 4;
/// 自动化测试操作的间隔。
pub const AUTOTEST_STEP_INTERVAL: Duration = Duration::from_millis(350);

/// 一套配色。
#[derive(Clone, Copy)]
pub(crate) struct Palette {
    /// 窗口底色。
    pub(crate) bg: Rgba,
    /// 侧栏底色。
    pub(crate) sidebar: Rgba,
    /// 分隔线。
    pub(crate) border: Rgba,
    /// 正文。
    pub(crate) text: Rgba,
    /// 次要文字。
    pub(crate) dim: Rgba,
    /// 控件底色。
    pub(crate) control: Rgba,
    /// 主色。
    pub(crate) accent: Rgba,
    /// 主色上的文字。
    pub(crate) on_accent: Rgba,
    /// 错误色。
    pub(crate) danger: Rgba,
    /// 成功色。
    pub(crate) ok: Rgba,
}

/// 按深浅色与主色生成配色。
///
/// # 参数
/// - `dark`：是否深色
/// - `accent`：主色 RGBA
pub(crate) fn palette(dark: bool, accent: [u8; 4]) -> Palette {
    let accent = rgba(u32::from_be_bytes(accent));
    if dark {
        Palette {
            bg: rgba(0x1F1F1FFF),
            sidebar: rgba(0x141414FF),
            border: rgba(0x303030FF),
            text: rgba(0xE6E6E6FF),
            dim: rgba(0x8C8C8CFF),
            control: rgba(0x3A3A3AFF),
            accent,
            on_accent: rgba(0xFFFFFFFF),
            danger: rgba(0xFF4D4FFF),
            ok: rgba(0x52C41AFF),
        }
    } else {
        Palette {
            bg: rgba(0xFFFFFFFF),
            sidebar: rgba(0xF3F3F3FF),
            border: rgba(0xE0E0E0FF),
            text: rgba(0x1F1F1FFF),
            dim: rgba(0x7A7A7AFF),
            control: rgba(0xE6E6E6FF),
            accent,
            on_accent: rgba(0xFFFFFFFF),
            danger: rgba(0xD9363EFF),
            ok: rgba(0x389E0DFF),
        }
    }
}

/// 下拉选项：配置值加本地化标签。
#[derive(Clone)]
struct DropdownItem {
    /// 写入配置的值。
    value: &'static str,
    /// 界面显示的标签。
    label: SharedString,
}

impl SearchableListItem for DropdownItem {
    type Value = &'static str;

    /// 下拉与触发器显示的标签。
    fn title(&self) -> SharedString {
        self.label.clone()
    }

    /// 选项的配置值。
    fn value(&self) -> &Self::Value {
        &self.value
    }
}

/// 下拉状态实体的具体类型。
type DropdownState = SelectState<SearchableVec<DropdownItem>>;

/// 一个常驻的下拉选择器：状态实体与其标签所用的语料语言。
struct Dropdown {
    /// gpui-component 的选择器状态（必须常驻，行滚出视口后不能丢）。
    state: Entity<DropdownState>,
    /// 当前标签所用语料语言，语言切换时据此重建标签。
    locale: &'static str,
}

/// 渲染耗时探针。
#[derive(Debug, Clone, Copy, Default)]
struct RenderProbe {
    /// 根树构建次数。
    frames: u64,
    /// 根树构建总耗时。
    root_total: Duration,
    /// 根树构建最大耗时。
    root_max: Duration,
    /// 行构建次数。
    row_batches: u64,
    /// 行构建总耗时。
    rows_total: Duration,
    /// 行构建最大耗时。
    rows_max: Duration,
    /// 最近一次行构建的行数。
    last_rows_built: usize,
}

/// 自动化验收操作（经环境变量注入，走与点击相同的状态入口）。
#[derive(Debug, Clone, PartialEq)]
pub enum AutotestOp {
    /// 切换到某个分组 id。
    Group(String),
    /// 设置搜索文本。
    Search(String),
    /// 写入某项。
    Set {
        /// 配置键。
        key: String,
        /// 值。
        value: Value,
    },
    /// 重置某项。
    Reset(String),
    /// 录入一条快捷键（跳过键盘捕获，走冲突检测与写回）。
    Shortcut {
        /// 配置键。
        key: String,
        /// 替换下标。
        index: Option<usize>,
        /// 快捷键文本。
        text: String,
    },
    /// 模拟在文本框中逐字输入并回车。
    Type {
        /// 配置键。
        key: String,
        /// 输入文本。
        text: String,
    },
    /// 滚动到第 n 行。
    Scroll(usize),
    /// 输出性能探针日志。
    Perf,
    /// 输出当前状态日志。
    State,
}

/// 解析自动化操作 JSON 数组。
///
/// # 参数
/// - `text`：JSON 文本，如 `[{"op":"group","id":"screenshot"}]`
///
/// # 返回
/// 操作列表；格式不对返回错误文本。
///
/// ```ignore
/// let ops = parse_autotest_ops(r#"[{"op":"perf"}]"#).unwrap();
/// assert_eq!(ops, vec![AutotestOp::Perf]);
/// ```
pub fn parse_autotest_ops(text: &str) -> Result<Vec<AutotestOp>, String> {
    let items: Vec<Value> = serde_json::from_str(text).map_err(|e| e.to_string())?;
    items.iter().map(parse_autotest_op).collect()
}

/// 解析单个自动化操作。
fn parse_autotest_op(item: &Value) -> Result<AutotestOp, String> {
    let field = |name: &str| item.get(name).and_then(Value::as_str).map(str::to_string);
    let need = |name: &str| field(name).ok_or_else(|| format!("缺少字段 {name}: {item}"));
    let op = need("op")?;
    Ok(match op.as_str() {
        "group" => AutotestOp::Group(need("id")?),
        "search" => AutotestOp::Search(field("q").unwrap_or_default()),
        "set" => AutotestOp::Set {
            key: need("key")?,
            value: item.get("value").cloned().unwrap_or(Value::Null),
        },
        "reset" => AutotestOp::Reset(need("key")?),
        "shortcut" => AutotestOp::Shortcut {
            key: need("key")?,
            index: item
                .get("index")
                .and_then(Value::as_u64)
                .map(|n| n as usize),
            text: need("text")?,
        },
        "type" => AutotestOp::Type {
            key: need("key")?,
            text: need("text")?,
        },
        "scroll" => {
            AutotestOp::Scroll(item.get("index").and_then(Value::as_u64).unwrap_or(0) as usize)
        }
        "perf" => AutotestOp::Perf,
        "state" => AutotestOp::State,
        other => return Err(format!("未知操作: {other}")),
    })
}

/// 设置页视图。
pub struct SettingsView {
    /// 状态机。
    state: SettingsState,
    /// 变更通知出口（热键重注册等由上层响应）。
    notify: Rc<dyn Fn(ConfigChange)>,
    /// 根焦点句柄（接收键盘输入）。
    focus: FocusHandle,
    /// 列表滚动句柄。
    list_scroll: UniformListScrollHandle,
    /// 渲染耗时探针。
    probe: RenderProbe,
    /// Hy-MT2 下载入口点击后的提示（暂无发布地址，只给手动放置指引）。
    hymt2_notice: Option<String>,
    /// 各配置键的下拉选择器（按需创建后常驻）。
    dropdowns: HashMap<&'static str, Dropdown>,
    /// 已应用到组件主题的深浅色；`None` 表示尚未应用。
    themed_dark: Option<bool>,
    /// 窗口标题当前对应的界面语言代码（变化时刷新标题）。
    titled_locale: Option<&'static str>,
    /// 上一帧列表的纵向滚动偏移（逻辑像素），变化即视为滚动。
    last_scroll_y: f32,
    /// 语音模型下载入口与数据根（未接入时为 `None`，面板按钮不可用）。
    stt_hooks: Option<SttHooks>,
    /// “检查更新”的界面状态。
    update_state: UpdateUiState,
    /// “更新”分组动作（检查 / 下载 / 打开目录）的入口（未接入时为 `None`，按钮不可用）。
    update_hook: Option<Rc<dyn Fn(UpdateAction)>>,
    /// 设置导出 / 导入的界面状态。
    transfer_state: TransferUiState,
    /// 导出时是否包含 API 密钥（默认不含，只在本视图存活）。
    transfer_include_keys: bool,
    /// 请求导出 / 导入的入口（未接入时为 `None`，按钮不可用）。
    transfer_hook: Option<Rc<dyn Fn(TransferAction)>>,
    /// 语音模型下载任务的界面状态。
    stt_download: DownloadState,
    /// 进行中下载的取消标记。
    stt_cancel: Option<CancelFlag>,
    /// 已安装的语音模型 ID 缓存（避免每帧访问磁盘）。
    stt_installed: HashSet<String>,
    /// 共享 VAD 是否已安装（缓存）。
    stt_vad_installed: bool,
    /// 翻译后端能力缓存（避免每帧扫盘）；未扫描为 `None`。
    translate_support: Option<ModelSupport>,
    /// 模型下拉当前选项的签名，变化时重建选项。
    model_signature: String,
    /// 内嵌在主窗口内容区：不画左侧分组栏，也不改窗口标题。
    embedded: bool,
}

/// 选项的显示标签：OCR 后端与本地路由模式有专用本地化，语言类选项固定显示各语言自称，其余原样显示。
///
/// # 参数
/// - `key`：配置键
/// - `option`：选项配置值
/// - `locale`：语料语言
fn option_label(key: &str, option: &str, locale: &str) -> String {
    match OcrBackend::from_config_value(option).filter(|_| key == KEY_OCR_BACKEND) {
        Some(backend) => backend.label(locale),
        None if key == KEY_LOCAL_ROUTE_MODE => {
            route_mode_label(option, locale).unwrap_or_else(|| option.to_string())
        }
        None if is_language_key(key) => language_option_label(option, locale),
        None => option_text(locale, key, option).unwrap_or_else(|| option.to_string()),
    }
}

/// 把候选列表转成下拉选项（保持顺序）。
fn dropdown_items(key: &str, options: &[&'static str], locale: &str) -> Vec<DropdownItem> {
    options
        .iter()
        .map(|option| DropdownItem {
            value: option,
            label: option_label(key, option, locale).into(),
        })
        .collect()
}

/// 当前配置值在候选里对应的选项；不在候选内返回 `None`。
fn dropdown_value(options: &[&'static str], current: &str) -> Option<&'static str> {
    options.iter().copied().find(|option| *option == current)
}

/// 当前配置值在候选里的下标（用作下拉初始选中）。
fn dropdown_index(options: &[&'static str], current: &str) -> Option<usize> {
    options.iter().position(|option| *option == current)
}

/// 更新分组说明区的文本行数（只有“当前版本”一行）。
const UPDATE_PANEL_LINES: usize = 1;

/// 导出 / 导入说明区的文本行数（只有一行说明）。
const TRANSFER_PANEL_LINES: usize = 1;

/// 翻译设置所在分组的 id。
const TRANSLATION_GROUP_ID: &str = "screenshot_translation";

impl SettingsView {
    /// 创建设置页并把键盘焦点交给它。
    ///
    /// # 参数
    /// - `window`：所属窗口
    /// - `app`：应用上下文
    /// - `store`：共享配置存储
    /// - `system`：系统偏好快照
    /// - `notify`：配置变更通知出口
    pub fn create(
        window: &mut Window,
        app: &mut App,
        store: SharedConfig,
        system: SystemPrefs,
        notify: Rc<dyn Fn(ConfigChange)>,
    ) -> Entity<Self> {
        let view = app.new(|cx| Self {
            state: SettingsState::new(store, system),
            notify,
            focus: cx.focus_handle(),
            list_scroll: UniformListScrollHandle::new(),
            probe: RenderProbe::default(),
            hymt2_notice: None,
            dropdowns: HashMap::new(),
            themed_dark: None,
            titled_locale: None,
            last_scroll_y: 0.0,
            stt_hooks: None,
            update_state: UpdateUiState::Idle,
            update_hook: None,
            transfer_state: TransferUiState::Idle,
            transfer_include_keys: false,
            transfer_hook: None,
            stt_download: DownloadState::Idle,
            stt_cancel: None,
            stt_installed: HashSet::new(),
            stt_vad_installed: false,
            translate_support: None,
            model_signature: String::new(),
            embedded: false,
        });
        let handle = view.read(app).focus.clone();
        window.focus(&handle, app);
        view
    }

    /// 确保 `key` 的下拉选择器存在，并把选中项与标签同步到当前配置值与界面语言。
    ///
    /// 状态实体存放在视图里，行滚出视口不会丢失；选中事件走 [`SettingsAction::Change`] 写配置。
    fn ensure_dropdown(
        &mut self,
        key: &'static str,
        options: &'static [&'static str],
        current: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let locale = self.state.prefs().locale;
        let want = dropdown_value(options, current);
        let Some(existing) = self.dropdowns.get(key) else {
            let items = SearchableVec::new(dropdown_items(key, options, locale));
            let index = dropdown_index(options, current).map(|row| IndexPath::default().row(row));
            let state = cx.new(|cx| SelectState::new(items, index, window, cx));
            cx.subscribe_in(
                &state,
                window,
                move |this,
                      _state,
                      event: &SelectEvent<SearchableVec<DropdownItem>>,
                      window,
                      cx| {
                    if let SelectEvent::Confirm(Some(value)) = event {
                        this.act(
                            SettingsAction::Change {
                                key,
                                value: json!(value),
                            },
                            window,
                            cx,
                        );
                    }
                },
            )
            .detach();
            self.dropdowns.insert(key, Dropdown { state, locale });
            return;
        };
        let relabel = existing.locale != locale;
        let state = existing.state.clone();
        let stale = state.read(cx).selected_value().copied() != want;
        if relabel {
            let items = SearchableVec::new(dropdown_items(key, options, locale));
            state.update(cx, |select, cx| select.set_items(items, window, cx));
        }
        if relabel || stale {
            state.update(cx, |select, cx| match want {
                Some(value) => select.set_selected_value(&value, window, cx),
                None => select.set_selected_index(None, window, cx),
            });
        }
        if let Some(entry) = self.dropdowns.get_mut(key) {
            entry.locale = locale;
        }
    }

    /// 收起展开中的下拉浮层：组件把焦点锁在浮层内，改焦点无效，
    /// 所以向持有焦点的浮层派发 Esc 同款的取消动作，走组件自己的关闭逻辑。
    fn close_dropdowns(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let focused = self
            .dropdowns
            .values()
            .any(|dropdown| dropdown.state.focus_handle(cx).contains_focused(window, cx));
        if focused {
            window.dispatch_action(Box::new(Cancel), cx);
        }
    }

    /// 读取语音转文字相关的当前配置值。
    fn stt_inputs(&self) -> SttInputs {
        SttInputs::from_lookup(|key| {
            self.state
                .row_by_key(key)
                .map_or(Value::Null, |row| row.value.clone())
        })
    }

    /// 接入“更新”分组入口（检查、下载、打开所在目录）。
    ///
    /// # 参数
    /// - `hook`：点击按钮时带动作调用，由上层起后台线程执行
    pub fn set_update_hook(&mut self, hook: Rc<dyn Fn(UpdateAction)>) {
        self.update_hook = Some(hook);
    }

    /// 收到检查更新的结果（经主线程收件箱转入）。
    ///
    /// # 参数
    /// - `state`：结果状态
    /// - `cx`：视图上下文
    pub fn finish_update_check(&mut self, state: UpdateUiState, cx: &mut Context<Self>) {
        self.update_state = state;
        cx.notify();
    }

    /// 接入设置导出 / 导入入口。
    ///
    /// # 参数
    /// - `hook`：点击按钮时调用，由上层弹文件对话框并执行
    pub fn set_transfer_hook(&mut self, hook: Rc<dyn Fn(TransferAction)>) {
        self.transfer_hook = Some(hook);
    }

    /// 收到导出 / 导入的结果；导入成功时重建行模型让界面显示新值。
    ///
    /// # 参数
    /// - `state`：结果状态
    /// - `reload`：是否需要从配置重建界面（导入成功）
    /// - `cx`：视图上下文
    pub fn finish_transfer(
        &mut self,
        state: TransferUiState,
        reload: bool,
        cx: &mut Context<Self>,
    ) {
        if reload {
            self.state.reload();
        }
        self.transfer_state = state;
        cx.notify();
    }

    /// 接入语音模型下载入口与数据根目录，并刷新安装状态缓存。
    ///
    /// # 参数
    /// - `hooks`：下载入口与数据根
    pub fn set_stt_hooks(&mut self, hooks: SttHooks) {
        self.stt_hooks = Some(hooks);
        self.refresh_stt_cache();
    }

    /// 重新检查各模型与共享 VAD 的安装状态（访问磁盘，只在用户操作或下载结束时调用）。
    fn refresh_stt_cache(&mut self) {
        let Some(hooks) = &self.stt_hooks else { return };
        self.stt_installed = stt_models::manifest()
            .models
            .iter()
            .filter(|spec| stt_download::is_installed(spec, &hooks.data_root))
            .map(|spec| spec.id.clone())
            .collect();
        self.stt_vad_installed = stt_download::is_vad_installed(&hooks.data_root);
        self.refresh_translate_support();
    }

    /// 重新扫描翻译模型（访问磁盘）；翻译开关关闭或未接入数据根时清空缓存。
    fn refresh_translate_support(&mut self) {
        let (config, tcfg) = self.state.translate_snapshot();
        self.translate_support = match &self.stt_hooks {
            Some(hooks) if config.translate_enabled => {
                Some(scan_translate_support(&tcfg, &hooks.data_root))
            }
            _ => None,
        };
    }

    /// 当前语音模型面板；被联动置灰或清单无模型时为 `None`。
    fn stt_panel(&self) -> Option<PanelModel> {
        build_panel(
            &self.stt_inputs(),
            |spec| self.stt_installed.contains(&spec.id),
            self.stt_vad_installed,
            &self.stt_download,
            self.state.prefs().locale,
        )
    }

    /// 开始下载模型（含离线模式缺的共享 VAD，由安装流程一并补齐）。
    fn start_stt_download(&mut self, model_id: String) {
        let Some(hooks) = &self.stt_hooks else { return };
        let cancel = CancelFlag::default();
        self.stt_cancel = Some(cancel.clone());
        self.stt_download = DownloadState::Running {
            model_id: model_id.clone(),
            progress: None,
        };
        (hooks.request)(model_id, cancel);
    }

    /// 取消进行中的下载。
    fn cancel_stt_download(&mut self) {
        if let Some(cancel) = &self.stt_cancel {
            cancel.cancel();
        }
    }

    /// 下载线程上报进度（经主线程收件箱转入）。
    ///
    /// # 参数
    /// - `progress`：进度快照
    /// - `cx`：视图上下文
    pub fn update_stt_download(&mut self, progress: Progress, cx: &mut Context<Self>) {
        if let DownloadState::Running { progress: slot, .. } = &mut self.stt_download {
            *slot = Some(progress);
            cx.notify();
        }
    }

    /// 下载结束（成功、失败或用户取消）。
    ///
    /// # 参数
    /// - `model_id`：模型 ID
    /// - `result`：结果，失败带错误说明
    /// - `cx`：视图上下文
    pub fn finish_stt_download(
        &mut self,
        model_id: String,
        result: Result<(), String>,
        cx: &mut Context<Self>,
    ) {
        let cancelled = self.stt_cancel.take().is_some_and(|c| c.is_cancelled());
        self.stt_download = match result {
            Ok(()) => DownloadState::Done { model_id },
            Err(_) if cancelled => DownloadState::Failed {
                model_id,
                message: None,
            },
            Err(message) => DownloadState::Failed {
                model_id,
                message: Some(message),
            },
        };
        self.refresh_stt_cache();
        cx.notify();
    }

    /// 确保模型下拉存在，选项与标签随维度、模式、安装状态与界面语言同步。
    fn ensure_model_dropdown(
        &mut self,
        inputs: &SttInputs,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let locale = self.state.prefs().locale;
        let specs = inputs.options();
        if specs.is_empty() {
            return;
        }
        let items: Vec<DropdownItem> = specs
            .iter()
            .map(|spec| DropdownItem {
                value: option_value(spec),
                label: stt_option_label(spec, locale).into(),
            })
            .collect();
        let signature = format!(
            "{locale}|{}|{}|{}",
            inputs.dimension.as_str(),
            mode_as_str(inputs.mode),
            items
                .iter()
                .map(|item| format!("{}={}", item.value, item.label))
                .collect::<Vec<_>>()
                .join(";"),
        );
        let want = inputs.selected_value();
        let index = items
            .iter()
            .position(|item| Some(item.value) == want)
            .map(|row| IndexPath::default().row(row));
        let key = KEY_DICTATION_MODEL_ID;
        let Some(existing) = self.dropdowns.get(key) else {
            let state = cx.new(|cx| SelectState::new(SearchableVec::new(items), index, window, cx));
            cx.subscribe_in(
                &state,
                window,
                move |this,
                      _state,
                      event: &SelectEvent<SearchableVec<DropdownItem>>,
                      window,
                      cx| {
                    if let SelectEvent::Confirm(Some(value)) = event {
                        this.act(
                            SettingsAction::Change {
                                key,
                                value: json!(value),
                            },
                            window,
                            cx,
                        );
                    }
                },
            )
            .detach();
            self.dropdowns.insert(key, Dropdown { state, locale });
            self.model_signature = signature;
            return;
        };
        let state = existing.state.clone();
        let changed = self.model_signature != signature;
        let stale = state.read(cx).selected_value().copied() != want;
        if changed {
            state.update(cx, |select, cx| {
                select.set_items(SearchableVec::new(items), window, cx)
            });
            self.model_signature = signature;
        }
        if changed || stale {
            state.update(cx, |select, cx| match want {
                Some(value) => select.set_selected_value(&value, window, cx),
                None => select.set_selected_index(index, window, cx),
            });
        }
    }

    /// 切换为内嵌模式（主窗口里复用本视图时调用）：隐藏左侧分组栏，窗口标题归宿主管。
    ///
    /// # 参数
    /// - `embedded`：是否内嵌。
    pub fn set_embedded(&mut self, embedded: bool) {
        self.embedded = embedded;
    }

    /// 按分组 id 切到某个设置分组（不抢焦点，宿主跳转用）；id 不存在时忽略。
    ///
    /// # 参数
    /// - `group_id`：设置分组 id，如 `screenshot_ui`。
    pub fn show_group_id(&mut self, group_id: &str, cx: &mut Context<Self>) {
        let Some(index) = crate::settings_model::groups()
            .iter()
            .position(|g| g.id == group_id)
        else {
            return;
        };
        self.state.dispatch(SettingsAction::SwitchGroup(index));
        self.refresh_stt_cache();
        self.scroll_to_top();
        self.flush_changes();
        cx.notify();
    }

    /// 执行动作：抢焦点、更新状态、转发变更、重绘。
    fn act(&mut self, action: SettingsAction, window: &mut Window, cx: &mut Context<Self>) {
        cx.stop_propagation();
        window.focus(&self.focus, cx);
        let switched = matches!(
            action,
            SettingsAction::SwitchGroup(_) | SettingsAction::SetSearch(_)
        );
        let recheck = matches!(&action, SettingsAction::SwitchGroup(_));
        self.state.dispatch(action);
        if recheck {
            self.refresh_stt_cache();
        }
        if switched {
            self.scroll_to_top();
        }
        self.flush_changes();
        cx.notify();
    }

    /// 把状态机积累的变更交给上层。
    fn flush_changes(&mut self) {
        for change in self.state.take_pending() {
            if rescans_translate_support(change.key) {
                self.refresh_translate_support();
            }
            (self.notify)(change);
        }
    }

    /// 列表滚回顶部。
    fn scroll_to_top(&self) {
        if self.state.visible_len() + self.header_len() > 0 {
            self.list_scroll
                .scroll_to_item_strict(0, ScrollStrategy::Top);
        }
    }

    /// 当前界面语言。
    pub fn language(&self) -> Lang {
        self.state.prefs().lang
    }

    /// 上层热键重注册失败并已还原配置后调用：刷新界面并提示。
    ///
    /// # 参数
    /// - `key`：被回滚的配置键
    /// - `message`：错误提示
    /// - `cx`：视图上下文
    pub fn notify_reverted(&mut self, key: &'static str, message: String, cx: &mut Context<Self>) {
        self.state.notify_reverted(key, message);
        cx.notify();
    }

    /// 执行一个自动化验收操作。
    ///
    /// # 参数
    /// - `op`：操作
    /// - `window`：所属窗口
    /// - `cx`：视图上下文
    pub fn run_autotest_op(
        &mut self,
        op: &AutotestOp,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        use crate::settings_model::groups;
        let static_key = |key: &str| snow_config::schema::entry_for(key).map(|e| e.key);
        match op {
            AutotestOp::Group(id) => {
                if let Some(index) = groups().iter().position(|g| g.id == id) {
                    self.act(SettingsAction::SwitchGroup(index), window, cx);
                }
            }
            AutotestOp::Search(q) => self.act(SettingsAction::SetSearch(q.clone()), window, cx),
            AutotestOp::Set { key, value } => {
                if let Some(key) = static_key(key) {
                    self.act(
                        SettingsAction::Change {
                            key,
                            value: value.clone(),
                        },
                        window,
                        cx,
                    );
                }
            }
            AutotestOp::Reset(key) => {
                if let Some(key) = static_key(key) {
                    self.act(SettingsAction::Reset(key), window, cx);
                }
            }
            AutotestOp::Shortcut { key, index, text } => {
                if let Some(key) = static_key(key) {
                    let _ = self.state.commit_shortcut(key, *index, text);
                    self.flush_changes();
                    cx.notify();
                }
            }
            AutotestOp::Type { key, text } => {
                if let Some(key) = static_key(key) {
                    self.state.dispatch(SettingsAction::BeginEdit(key));
                    let mods = KeyMods::default();
                    for _ in 0..64 {
                        self.state.on_key("backspace", None, mods, None);
                    }
                    for ch in text.chars() {
                        let s = ch.to_string();
                        self.state.on_key(&s, Some(&s), mods, None);
                    }
                    self.state.on_key("enter", None, mods, None);
                    self.flush_changes();
                    cx.notify();
                }
            }
            AutotestOp::Scroll(index) => {
                if *index < self.state.visible_len() {
                    let target = self.header_len() + *index;
                    self.list_scroll
                        .scroll_to_item_strict(target, ScrollStrategy::Top);
                    cx.notify();
                }
            }
            AutotestOp::Perf => self.log_perf(),
            AutotestOp::State => self.log_state(),
        }
    }

    /// 输出当前状态摘要日志。
    pub fn log_state(&self) {
        let prefs = self.state.prefs();
        tracing::info!(
            scope = ?self.state.scope(),
            visible = self.state.visible_len(),
            total = self.state.total_rows(),
            dark = prefs.dark,
            lang = ?prefs.lang,
            status = self.state.status().map(|s| s.text.as_str()).unwrap_or(""),
            "settings state"
        );
    }

    /// 输出性能探针日志。
    pub fn log_perf(&self) {
        let perf = self.state.perf();
        let p = self.probe;
        let avg = |total: Duration, n: u64| (total.as_micros() as u64).checked_div(n).unwrap_or(0);
        tracing::info!(
            build_all_rows_us = perf.build_all_rows.as_micros() as u64,
            last_switch_us = perf.last_switch.as_micros() as u64,
            last_search_us = perf.last_search.as_micros() as u64,
            frames = p.frames,
            root_avg_us = avg(p.root_total, p.frames),
            root_max_us = p.root_max.as_micros() as u64,
            row_batches = p.row_batches,
            rows_avg_us = avg(p.rows_total, p.row_batches),
            rows_max_us = p.rows_max.as_micros() as u64,
            last_rows_built = p.last_rows_built,
            visible = self.state.visible_len(),
            "settings perf"
        );
    }

    /// 生成点击（左键按下）监听器。
    fn click(
        cx: &mut Context<Self>,
        action: SettingsAction,
    ) -> impl Fn(&MouseDownEvent, &mut Window, &mut App) + 'static {
        cx.listener(move |this, _event: &MouseDownEvent, window, cx| {
            this.act(action.clone(), window, cx);
        })
    }

    /// 小按钮（文字按钮）。
    fn button(label: impl Into<SharedString>, enabled: bool, p: &Palette) -> Div {
        div()
            .px_2()
            .h(px(24.0))
            .flex()
            .items_center()
            .rounded_md()
            .bg(p.control)
            .text_size(px(12.0))
            .text_color(if enabled { p.text } else { p.dim })
            .when(enabled, |d| d.cursor_pointer())
            .child(label.into())
    }

    /// 平铺候选按钮。
    fn chip(label: impl Into<SharedString>, active: bool, p: &Palette) -> Div {
        div()
            .px_3()
            .h(px(26.0))
            .flex()
            .items_center()
            .rounded_md()
            .cursor_pointer()
            .text_size(px(12.0))
            .bg(if active { p.accent } else { p.control })
            .text_color(if active { p.on_accent } else { p.text })
            .child(label.into())
    }

    /// 渲染一行右侧的控件。
    fn render_control(
        &self,
        row: &RowModel,
        p: &Palette,
        lang: Lang,
        cx: &mut Context<Self>,
    ) -> Div {
        let key = row.key;
        let row_div = div().flex().items_center().gap_2();
        if key == KEY_DICTATION_MODEL_ID {
            return self.model_control(row_div, p, lang.locale());
        }
        match row.control {
            Control::Switch => {
                let on = row.value.as_bool().unwrap_or(false);
                row_div.child(
                    div()
                        .w(px(40.0))
                        .h(px(22.0))
                        .relative()
                        .rounded_full()
                        .cursor_pointer()
                        .bg(if on { p.accent } else { p.control })
                        .child(
                            div()
                                .absolute()
                                .top(px(2.0))
                                .left(px(if on { 20.0 } else { 2.0 }))
                                .size(px(18.0))
                                .rounded_full()
                                .bg(rgba(0xFFFFFFFF)),
                        )
                        .on_mouse_down(
                            MouseButton::Left,
                            Self::click(
                                cx,
                                SettingsAction::Change {
                                    key,
                                    value: json!(!on),
                                },
                            ),
                        ),
                )
            }
            Control::Slider(range) => {
                let current = row.value.as_i64().unwrap_or(i64::from(range.min));
                let active = slider_active_cell(range, current);
                let mut cells = div().flex().items_center();
                for cell in 0..SLIDER_CELLS {
                    let value = slider_cell_value(range, cell);
                    cells = cells.child(
                        div()
                            .w(px(9.0))
                            .h(px(18.0))
                            .mr(px(1.0))
                            .cursor_pointer()
                            .bg(if cell <= active { p.accent } else { p.control })
                            .on_mouse_down(
                                MouseButton::Left,
                                Self::click(
                                    cx,
                                    SettingsAction::Change {
                                        key,
                                        value: json!(value),
                                    },
                                ),
                            ),
                    );
                }
                let step_button = |label: &'static str, direction: i64, cx: &mut Context<Self>| {
                    Self::button(label, true, p).on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, event: &MouseDownEvent, window, cx| {
                            let value = step_int(current, range, direction, event.modifiers.shift);
                            this.act(
                                SettingsAction::Change {
                                    key,
                                    value: json!(value),
                                },
                                window,
                                cx,
                            );
                        }),
                    )
                };
                row_div
                    .child(step_button("-", -1, cx))
                    .child(cells)
                    .child(step_button("+", 1, cx))
                    .child(self.text_field(row, 84.0, p, lang, cx))
            }
            Control::IntText | Control::Text | Control::ListText | Control::JsonText => {
                row_div.child(self.text_field(row, FIELD_WIDTH, p, lang, cx))
            }
            Control::Color => {
                let swatch = parse_hex_color(row.value.as_str().unwrap_or_default())
                    .map_or(p.control, |c| rgba(u32::from_be_bytes(c)));
                row_div
                    .child(
                        div()
                            .size(px(22.0))
                            .rounded_md()
                            .border_1()
                            .border_color(p.border)
                            .bg(swatch),
                    )
                    .child(self.text_field(row, FIELD_WIDTH - 30.0, p, lang, cx))
            }
            Control::Choice(_) => {
                let locked = is_selector_key(key) && self.stt_inputs().lock_reason().is_some();
                let select = self.dropdowns.get(key).map(|dropdown| {
                    div().w(px(DROPDOWN_WIDTH)).h(px(DROPDOWN_HEIGHT)).child(
                        Select::new(&dropdown.state)
                            .with_size(ComponentSize::Small)
                            .menu_max_h(px(DROPDOWN_MENU_MAX_HEIGHT))
                            .disabled(locked),
                    )
                });
                row_div.children(select)
            }
            Control::Shortcuts { max_items, .. } => {
                self.shortcut_editor(row, max_items, p, lang, cx)
            }
            Control::ReadOnly(reason) => {
                let note = read_only_note(lang, reason);
                let preview = if matches!(reason, crate::settings_model::ReadOnlyReason::Secret) {
                    String::new()
                } else {
                    format!("{}  ", preview_text(&row.value, READ_ONLY_PREVIEW_CHARS))
                };
                row_div.child(
                    div()
                        .max_w(px(FIELD_WIDTH + 60.0))
                        .overflow_hidden()
                        .whitespace_nowrap()
                        .text_size(px(12.0))
                        .text_color(p.dim)
                        .child(format!("{preview}[{}: {note}]", t(lang, Text::ReadOnly))),
                )
            }
        }
    }

    /// 语音模型行的控件：下拉（候选来自清单）；被联动置灰时禁用，无候选时给占位文案。
    fn model_control(&self, row_div: Div, p: &Palette, locale: &'static str) -> Div {
        let inputs = self.stt_inputs();
        let locked = inputs.lock_reason().is_some();
        match self.dropdowns.get(KEY_DICTATION_MODEL_ID) {
            Some(dropdown) if !inputs.options().is_empty() => row_div.child(
                div()
                    .w(px(MODEL_DROPDOWN_WIDTH))
                    .h(px(DROPDOWN_HEIGHT))
                    .child(
                        Select::new(&dropdown.state)
                            .with_size(ComponentSize::Small)
                            .menu_max_h(px(DROPDOWN_MENU_MAX_HEIGHT))
                            .disabled(locked),
                    ),
            ),
            _ => row_div.child(
                div()
                    .text_size(px(12.0))
                    .text_color(p.dim)
                    .child(crate::ocr_backend::i18n_for(locale).tr("stt-ui-no-models")),
            ),
        }
    }

    /// 单行文本框：编辑中显示带插入符的窗口化文本，否则显示预览，点击进入编辑。
    fn text_field(
        &self,
        row: &RowModel,
        width: f32,
        p: &Palette,
        lang: Lang,
        cx: &mut Context<Self>,
    ) -> Div {
        let key = row.key;
        let editing = self
            .state
            .edit()
            .filter(|e| e.target == EditTarget::Row(key));
        let (shown, active) = match editing {
            Some(edit) => {
                let (visible, cursor) = window_text(&edit.buffer, FIELD_VISIBLE_CHARS);
                let left: String = visible.chars().take(cursor).collect();
                let right: String = visible.chars().skip(cursor).collect();
                (format!("{left}{CARET}{right}"), true)
            }
            None => (
                preview_text(
                    &Value::String(edit_text(row.control, &row.value)),
                    FIELD_VISIBLE_CHARS,
                ),
                false,
            ),
        };
        let _ = lang;
        div()
            .w(px(width))
            .h(px(28.0))
            .px_2()
            .flex()
            .items_center()
            .overflow_hidden()
            .whitespace_nowrap()
            .rounded_md()
            .cursor_text()
            .border_1()
            .border_color(if active { p.accent } else { p.border })
            .bg(p.control)
            .text_size(px(12.0))
            .child(shown)
            .on_mouse_down(
                MouseButton::Left,
                Self::click(cx, SettingsAction::BeginEdit(key)),
            )
    }

    /// 快捷键编辑器：已有绑定的芯片（点击替换、× 删除）与添加按钮。
    fn shortcut_editor(
        &self,
        row: &RowModel,
        max_items: Option<usize>,
        p: &Palette,
        lang: Lang,
        cx: &mut Context<Self>,
    ) -> Div {
        let key = row.key;
        let capture = self.state.capture().filter(|c| c.key == key);
        let prompt = t(lang, Text::PressShortcut);
        let mut list = div().flex().items_center().gap_2();
        for (index, text) in row.shortcuts.iter().take(SHORTCUT_CHIPS_MAX).enumerate() {
            let capturing_this = capture.is_some_and(|c| c.index == Some(index));
            let label = if capturing_this {
                prompt.to_string()
            } else {
                text.clone()
            };
            let chip = Self::chip(label, capturing_this, p).on_mouse_down(
                MouseButton::Left,
                Self::click(
                    cx,
                    SettingsAction::BeginCapture {
                        key,
                        index: Some(index),
                    },
                ),
            );
            let remove = Self::button("x", true, p).on_mouse_down(
                MouseButton::Left,
                Self::click(cx, SettingsAction::RemoveShortcut { key, index }),
            );
            list = list.child(
                div()
                    .flex()
                    .items_center()
                    .gap_1()
                    .child(chip)
                    .child(remove),
            );
        }
        let full = max_items.is_some_and(|m| row.shortcuts.len() >= m);
        if capture.is_some_and(|c| c.index.is_none()) {
            list = list.child(Self::chip(prompt, true, p));
        } else if !full {
            list = list.child(
                Self::button(t(lang, Text::AddShortcut), true, p).on_mouse_down(
                    MouseButton::Left,
                    Self::click(cx, SettingsAction::BeginCapture { key, index: None }),
                ),
            );
        }
        list
    }

    /// 渲染第 `position` 个可见行。
    fn render_row(
        &mut self,
        position: usize,
        p: &Palette,
        lang: Lang,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let cycle = self
            .state
            .visible_row(position)
            .and_then(|row| match row.control {
                Control::Choice(options) => Some((
                    row.key,
                    options,
                    self.state.choice_value(row.key, &row.value),
                )),
                _ => None,
            });
        if let Some((key, options, current)) = cycle {
            self.ensure_dropdown(key, options, &current, window, cx);
        }
        if self
            .state
            .visible_row(position)
            .is_some_and(|row| row.key == KEY_DICTATION_MODEL_ID)
        {
            let inputs = self.stt_inputs();
            self.ensure_model_dropdown(&inputs, window, cx);
        }
        let Some(row) = self.state.visible_row(position) else {
            return div().into_any_element();
        };
        let key = row.key;
        let read_only = matches!(row.control, Control::ReadOnly(_));
        let resettable = !row.is_default && !read_only;
        let reset = {
            let button = Self::button(t(lang, Text::ResetItem), resettable, p);
            if resettable {
                button.on_mouse_down(
                    MouseButton::Left,
                    Self::click(cx, SettingsAction::Reset(key)),
                )
            } else {
                button
            }
        };
        let backend_notice = if key == KEY_OCR_BACKEND {
            OcrNotice::for_config_value(&row.value)
        } else {
            None
        };
        let route_note = route_hint(key, &row.value, self.state.prefs().locale);
        let dictation_notice = if key == KEY_DICTATION_BACKEND {
            dictation_backend_notice(&row.value, self.state.prefs().locale)
        } else {
            None
        };
        let stt_note = self.stt_row_note(key);
        let audio_note = self
            .state
            .row_by_key(crate::recording::output::KEY_FORMAT)
            .and_then(|format| crate::recording::audio::mp4_only_note(key, &format.value))
            .map(|id| crate::ocr_backend::i18n_for(lang.locale()).tr(id));
        let route_note = route_note.or(audio_note);
        let sub = match (
            &row.error,
            stt_note,
            backend_notice,
            dictation_notice,
            route_note,
        ) {
            (Some(error), ..) => div().text_color(p.danger).child(error.clone()),
            (None, Some((text, danger)), ..) => div()
                .text_color(if danger { p.danger } else { p.dim })
                .child(text),
            (None, None, Some(notice), ..) => div()
                .text_color(p.danger)
                .child(notice.message(self.state.prefs().locale)),
            (None, None, None, Some(notice), _) => div().text_color(p.danger).child(notice),
            (None, None, None, None, Some(note)) => div().text_color(p.dim).child(note),
            (None, None, None, None, None) => div()
                .text_color(p.dim)
                .child(item_desc(lang, key).unwrap_or_else(|| key.to_string())),
        };
        let label = div()
            .w(px(LABEL_WIDTH))
            .flex()
            .flex_col()
            .overflow_hidden()
            .child(
                div()
                    .text_size(px(13.0))
                    .font_weight(FontWeight::MEDIUM)
                    .whitespace_nowrap()
                    .child(item_label(lang, key)),
            )
            .child(
                div()
                    .text_size(px(11.0))
                    .max_h(px(SUB_MAX_HEIGHT))
                    .overflow_hidden()
                    .child(sub),
            );
        div()
            .h(px(ROW_HEIGHT))
            .w_full()
            .px_4()
            .flex()
            .items_center()
            .justify_between()
            .border_b_1()
            .border_color(p.border)
            .child(label)
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_3()
                    .child(self.render_control(row, p, lang, cx))
                    .child(reset),
            )
            .into_any_element()
    }

    /// 渲染侧栏。
    fn render_sidebar(&self, p: &Palette, lang: Lang, cx: &mut Context<Self>) -> impl IntoElement {
        let active = match self.state.scope() {
            Scope::Group(index) => Some(index),
            Scope::Search => None,
        };
        let mut list = div()
            .id("settings-sidebar-list")
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .flex()
            .flex_col()
            .gap_1();
        for (index, group) in crate::settings_model::groups().iter().enumerate() {
            let item_count = self.state.group_item_count(index);
            let is_active = active == Some(index);
            list = list.child(
                div()
                    .h(px(32.0))
                    .px_3()
                    .flex()
                    .flex_none()
                    .items_center()
                    .justify_between()
                    .rounded_md()
                    .cursor_pointer()
                    .text_size(px(13.0))
                    .bg(if is_active {
                        p.accent
                    } else {
                        rgba(0x00000000)
                    })
                    .text_color(if is_active { p.on_accent } else { p.text })
                    .child(group_title(lang, group.id))
                    .child(
                        div()
                            .text_size(px(11.0))
                            .text_color(if is_active { p.on_accent } else { p.dim })
                            .child(item_count.to_string()),
                    )
                    .on_mouse_down(
                        MouseButton::Left,
                        Self::click(cx, SettingsAction::SwitchGroup(index)),
                    ),
            );
        }
        div()
            .w(px(SIDEBAR_WIDTH))
            .h_full()
            .flex_none()
            .flex()
            .flex_col()
            .p_3()
            .gap_2()
            .bg(p.sidebar)
            .border_r_1()
            .border_color(p.border)
            .child(
                div()
                    .pb_2()
                    .text_size(px(16.0))
                    .font_weight(FontWeight::BOLD)
                    .child(t(lang, Text::Title)),
            )
            .child(list)
    }

    /// 渲染顶部栏：标题、计数、搜索框、重置本组。
    fn render_header(&self, p: &Palette, lang: Lang, cx: &mut Context<Self>) -> impl IntoElement {
        let searching = self
            .state
            .edit()
            .is_some_and(|e| e.target == EditTarget::Search);
        let search_text = if searching {
            let edit = self.state.edit().map(|e| &e.buffer);
            match edit {
                Some(buffer) => {
                    let (visible, cursor) = window_text(buffer, 24);
                    let left: String = visible.chars().take(cursor).collect();
                    let right: String = visible.chars().skip(cursor).collect();
                    format!("{left}{CARET}{right}")
                }
                None => String::new(),
            }
        } else if self.state.search().is_empty() {
            t(lang, Text::SearchPlaceholder).to_string()
        } else {
            self.state.search().to_string()
        };
        let dim_placeholder = !searching && self.state.search().is_empty();
        let search = div()
            .w(px(SEARCH_WIDTH))
            .h(px(28.0))
            .px_2()
            .flex()
            .items_center()
            .overflow_hidden()
            .whitespace_nowrap()
            .rounded_md()
            .cursor_text()
            .border_1()
            .border_color(if searching { p.accent } else { p.border })
            .bg(p.control)
            .text_size(px(12.0))
            .text_color(if dim_placeholder { p.dim } else { p.text })
            .child(search_text)
            .on_mouse_down(
                MouseButton::Left,
                Self::click(cx, SettingsAction::BeginSearch),
            );
        let reset_group = Self::button(t(lang, Text::ResetGroup), true, p).on_mouse_down(
            MouseButton::Left,
            Self::click(cx, SettingsAction::ResetScope),
        );
        div()
            .h(px(56.0))
            .px_4()
            .flex()
            .flex_none()
            .items_center()
            .justify_between()
            .border_b_1()
            .border_color(p.border)
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_3()
                    .child(
                        div()
                            .text_size(px(18.0))
                            .font_weight(FontWeight::BOLD)
                            .child(self.state.scope_title()),
                    )
                    .child(div().text_size(px(12.0)).text_color(p.dim).child(format!(
                        "{} {}",
                        self.state.visible_len(),
                        t(lang, Text::ItemsCount)
                    ))),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_3()
                    .child(search)
                    .child(reset_group),
            )
    }

    /// 当前分组顶部说明区占的列表行数（翻译分组的 Hy-MT2 说明、语音分组的模型面板）；其它范围为 0。
    fn header_len(&self) -> usize {
        match self.current_group_id() {
            Some(TRANSLATION_GROUP_ID) => self.hymt2_header_len(),
            Some(DICTATION_GROUP_ID) => self
                .stt_panel()
                .map_or(0, |panel| hymt2_rows(panel.lines.len()).len()),
            Some(UPDATES_GROUP_ID) => hymt2_rows(UPDATE_PANEL_LINES).len(),
            Some(TRANSFER_GROUP_ID) => hymt2_rows(TRANSFER_PANEL_LINES).len(),
            _ => 0,
        }
    }

    /// 当前所在分组 id；搜索范围为 `None`。
    fn current_group_id(&self) -> Option<&'static str> {
        let Scope::Group(index) = self.state.scope() else {
            return None;
        };
        crate::settings_model::groups()
            .get(index)
            .map(|group| group.id)
    }

    /// 翻译分组下说明区占的列表行数；其它范围为 0。
    fn hymt2_header_len(&self) -> usize {
        let Scope::Group(index) = self.state.scope() else {
            return 0;
        };
        match crate::settings_model::groups().get(index) {
            Some(group) if group.id == TRANSLATION_GROUP_ID => hymt2_rows(HYMT2_LINE_COUNT).len(),
            _ => 0,
        }
    }

    /// 渲染 Hy-MT2 说明区的第 `row_index` 个定高行（与普通行同高，随列表滚动）。
    fn render_hymt2_row(
        &self,
        row_index: usize,
        p: &Palette,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let locale = self.state.prefs().locale;
        let current = self
            .state
            .row_by_key(KEY_LOCAL_MODEL_ID)
            .and_then(|row| row.value.as_str().map(str::to_owned))
            .unwrap_or_default();
        let Hymt2View { panel, in_use } = hymt2_view(locale, &current);
        let rows = hymt2_rows(panel.lines.len());
        let frame = div()
            .h(px(ROW_HEIGHT))
            .w_full()
            .px_4()
            .py_1()
            .flex()
            .flex_col()
            .overflow_hidden()
            .text_size(px(12.0))
            .line_height(px(16.0));
        let Some(row) = rows.get(row_index) else {
            return frame.into_any_element();
        };
        match row {
            Hymt2Row::Text { title, lines } => {
                let mut block = frame;
                if *title {
                    block = block.child(
                        div()
                            .font_weight(FontWeight::BOLD)
                            .child(panel.title.clone()),
                    );
                }
                for line in &panel.lines[lines.clone()] {
                    block = block.child(
                        div()
                            .text_color(p.dim)
                            .whitespace_nowrap()
                            .child(line.clone()),
                    );
                }
                block.into_any_element()
            }
            Hymt2Row::Actions => {
                // 用 gpui-component 的 Button；已选中时禁用并显示“使用中”。
                let use_button = match hymt2_click(Hymt2Button::Use, locale) {
                    _ if in_use => Button::new("hymt2-use")
                        .small()
                        .label(panel.in_use_label)
                        .disabled(true),
                    Hymt2Click::SetConfig { key, value } => Button::new("hymt2-use")
                        .small()
                        .label(panel.use_label)
                        .on_click(cx.listener(move |this, _event: &ClickEvent, window, cx| {
                            this.act(
                                SettingsAction::Change {
                                    key,
                                    value: json!(value),
                                },
                                window,
                                cx,
                            );
                        })),
                    Hymt2Click::ShowNotice(_) => {
                        Button::new("hymt2-use").small().label(panel.use_label)
                    }
                };
                let download = Button::new("hymt2-download")
                    .small()
                    .outline()
                    .label(panel.download_label)
                    .on_click(cx.listener(move |this, _event: &ClickEvent, _window, cx| {
                        if let Hymt2Click::ShowNotice(text) =
                            hymt2_click(Hymt2Button::Download, locale)
                        {
                            this.hymt2_notice = Some(text);
                        }
                        cx.stop_propagation();
                        cx.notify();
                    }));
                let mut block = frame
                    .py_0()
                    .pt(px(2.0))
                    .gap(px(2.0))
                    .border_b_1()
                    .border_color(p.border)
                    .child(div().flex().gap_2().child(use_button).child(download));
                // 提示放在按钮下方并用中性色，出现时不挤动按钮。
                if let Some(notice) = &self.hymt2_notice {
                    block = block.child(
                        div()
                            .text_size(px(11.0))
                            .line_height(px(14.0))
                            .text_color(p.text)
                            .child(notice.clone()),
                    );
                }
                block.into_any_element()
            }
        }
    }

    /// 语音三项选择行的说明：置灰原因、无效组合提示，模型行未置灰时显示安装状态。
    ///
    /// # 返回
    /// `(文案, 是否为警示)`；与语音转文字无关的键返回 `None`。
    fn stt_row_note(&self, key: &str) -> Option<(String, bool)> {
        if is_translate_note_key(key) {
            let (config, _) = self.state.translate_snapshot();
            return translate_note(
                &config,
                self.translate_support.as_ref(),
                self.state.prefs().locale,
            );
        }
        if !is_selector_key(key) {
            return None;
        }
        let locale = self.state.prefs().locale;
        let inputs = self.stt_inputs();
        if let Some(note) = selector_note(&inputs, key, locale) {
            return Some(note);
        }
        if key != KEY_DICTATION_MODEL_ID {
            return None;
        }
        if let DownloadState::Running { model_id, progress } = &self.stt_download
            && inputs.selected().is_some_and(|spec| &spec.id == model_id)
        {
            let text = progress
                .as_ref()
                .map(|p| crate::stt_settings::progress_text(p, locale))
                .unwrap_or_default();
            return Some((text, false));
        }
        let spec = inputs.selected()?;
        Some((
            model_row_status(spec, self.stt_installed.contains(&spec.id), locale),
            false,
        ))
    }

    /// 渲染说明区的第 `row_index` 个定高行：翻译分组走 Hy-MT2 说明，语音分组走模型面板。
    fn render_header_row(
        &self,
        row_index: usize,
        p: &Palette,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        match self.current_group_id() {
            Some(DICTATION_GROUP_ID) => self.render_stt_row(row_index, p, cx),
            Some(UPDATES_GROUP_ID) => self.render_update_row(row_index, p, cx),
            Some(TRANSFER_GROUP_ID) => self.render_transfer_row(row_index, p, cx),
            _ => self.render_hymt2_row(row_index, p, cx),
        }
    }

    /// 渲染“检查更新”说明区的第 `row_index` 个定高行（当前版本行与按钮行）。
    fn render_update_row(
        &self,
        row_index: usize,
        p: &Palette,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let panel = update_panel(self.state.prefs().locale, &self.update_state);
        let frame = div()
            .h(px(ROW_HEIGHT))
            .w_full()
            .px_4()
            .py_1()
            .flex()
            .flex_col()
            .overflow_hidden()
            .text_size(px(12.0))
            .line_height(px(16.0));
        let rows = hymt2_rows(UPDATE_PANEL_LINES);
        let Some(row) = rows.get(row_index) else {
            return frame.into_any_element();
        };
        match row {
            Hymt2Row::Text { .. } => frame
                .child(div().font_weight(FontWeight::BOLD).child(panel.title))
                .child(
                    div()
                        .text_color(p.dim)
                        .whitespace_nowrap()
                        .child(panel.current_line),
                )
                .into_any_element(),
            Hymt2Row::Actions => {
                let running = matches!(
                    self.update_state,
                    UpdateUiState::Running | UpdateUiState::Downloading
                );
                let mut button = Button::new("update-check").small().label(panel.button_label);
                button = match &self.update_hook {
                    Some(hook) if !running => {
                        let hook = Rc::clone(hook);
                        button.on_click(cx.listener(move |this, _event: &ClickEvent, _window, cx| {
                            this.update_state = UpdateUiState::Running;
                            hook(UpdateAction::Check);
                            cx.notify();
                        }))
                    }
                    _ => button.disabled(true),
                };
                // 发现新版本且有下载地址时给“下载”；下载完成后给“打开所在目录”
                let extra = match (&self.update_state, &self.update_hook) {
                    (UpdateUiState::Available { info, .. }, Some(hook)) if !info.url.is_empty() => {
                        let (hook, info) = (Rc::clone(hook), info.clone());
                        Some(
                            Button::new("update-download")
                                .small()
                                .label(panel.download_label)
                                .on_click(cx.listener(move |this, _event: &ClickEvent, _window, cx| {
                                    this.update_state = UpdateUiState::Downloading;
                                    hook(UpdateAction::Download(info.clone()));
                                    cx.notify();
                                })),
                        )
                    }
                    (UpdateUiState::Downloaded { dir, .. }, Some(hook)) => {
                        let (hook, dir) = (Rc::clone(hook), dir.clone());
                        Some(
                            Button::new("update-open-folder")
                                .small()
                                .label(panel.open_folder_label)
                                .on_click(move |_event: &ClickEvent, _window, _cx| {
                                    hook(UpdateAction::OpenFolder(dir.clone()));
                                }),
                        )
                    }
                    _ => None,
                };
                let notice = panel.notice.map(|(text, danger)| {
                    div()
                        .text_size(px(11.0))
                        .text_color(if danger { p.danger } else { p.dim })
                        .whitespace_nowrap()
                        .child(text)
                });
                frame
                    .py_0()
                    .pt(px(2.0))
                    .border_b_1()
                    .border_color(p.border)
                    .child(div().flex().items_center().gap_3().child(button).children(extra).children(notice))
                    .into_any_element()
            }
        }
    }

    /// 渲染“导出 / 导入设置”说明区的第 `row_index` 个定高行（说明行与按钮行）。
    fn render_transfer_row(
        &self,
        row_index: usize,
        p: &Palette,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let panel = transfer_panel(self.state.prefs().locale, &self.transfer_state);
        let frame = div()
            .h(px(ROW_HEIGHT))
            .w_full()
            .px_4()
            .py_1()
            .flex()
            .flex_col()
            .overflow_hidden()
            .text_size(px(12.0))
            .line_height(px(16.0));
        let rows = hymt2_rows(TRANSFER_PANEL_LINES);
        let Some(row) = rows.get(row_index) else {
            return frame.into_any_element();
        };
        match row {
            Hymt2Row::Text { .. } => frame
                .child(div().font_weight(FontWeight::BOLD).child(panel.title))
                .child(
                    div()
                        .text_color(p.dim)
                        .whitespace_nowrap()
                        .child(panel.description),
                )
                .into_any_element(),
            Hymt2Row::Actions => {
                let mut buttons = Vec::new();
                for (id, label, action) in [
                    (
                        "config-export",
                        panel.export_label,
                        TransferAction::Export { include_credentials: self.transfer_include_keys },
                    ),
                    ("config-import", panel.import_label, TransferAction::Import),
                ] {
                    let mut button = Button::new(id).small().label(label);
                    button = match &self.transfer_hook {
                        Some(hook) => {
                            let hook = Rc::clone(hook);
                            button.on_click(cx.listener(
                                move |_this, _event: &ClickEvent, _window, _cx| {
                                    hook(action);
                                },
                            ))
                        }
                        None => button.disabled(true),
                    };
                    buttons.push(button);
                }
                let include_keys = Checkbox::new("config-include-keys")
                    .label(SharedString::from(panel.include_keys_label))
                    .checked(self.transfer_include_keys)
                    .on_click(cx.listener(|this, checked: &bool, _window, cx| {
                        this.transfer_include_keys = *checked;
                        cx.notify();
                    }));
                let notice = panel.notice.map(|(text, danger)| {
                    div()
                        .text_size(px(11.0))
                        .text_color(if danger { p.danger } else { p.dim })
                        .whitespace_nowrap()
                        .child(text)
                });
                frame
                    .py_0()
                    .pt(px(2.0))
                    .border_b_1()
                    .border_color(p.border)
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_3()
                            .children(buttons)
                            .child(include_keys)
                            .children(notice),
                    )
                    .into_any_element()
            }
        }
    }

    /// 渲染语音模型面板的第 `row_index` 个定高行（详情文本行与下载按钮行）。
    fn render_stt_row(&self, row_index: usize, p: &Palette, cx: &mut Context<Self>) -> AnyElement {
        let frame = div()
            .h(px(ROW_HEIGHT))
            .w_full()
            .px_4()
            .py_1()
            .flex()
            .flex_col()
            .overflow_hidden()
            .text_size(px(12.0))
            .line_height(px(16.0));
        let Some(panel) = self.stt_panel() else {
            return frame.into_any_element();
        };
        let rows = hymt2_rows(panel.lines.len());
        let Some(row) = rows.get(row_index) else {
            return frame.into_any_element();
        };
        match row {
            Hymt2Row::Text { title, lines } => {
                let mut block = frame;
                if *title {
                    block = block.child(
                        div()
                            .font_weight(FontWeight::BOLD)
                            .child(panel.title.clone()),
                    );
                }
                for line in &panel.lines[lines.clone()] {
                    block = block.child(
                        div()
                            .text_color(p.dim)
                            .whitespace_nowrap()
                            .child(line.clone()),
                    );
                }
                block.into_any_element()
            }
            Hymt2Row::Actions => {
                let action = panel.action;
                let model_id = panel.model_id.clone();
                let mut button = Button::new("stt-action")
                    .small()
                    .label(panel.action_label.clone());
                button = match action {
                    PanelAction::Download => button.on_click(cx.listener(
                        move |this, _event: &ClickEvent, _window, cx| {
                            this.start_stt_download(model_id.clone());
                            cx.notify();
                        },
                    )),
                    PanelAction::Cancel => button.outline().on_click(cx.listener(
                        |this, _event: &ClickEvent, _window, cx| {
                            this.cancel_stt_download();
                            cx.notify();
                        },
                    )),
                    PanelAction::Installed | PanelAction::Busy => button.disabled(true),
                };
                let button =
                    button.disabled(action == PanelAction::Download && self.stt_hooks.is_none());
                let notice = panel.notice.map(|(text, danger)| {
                    div()
                        .text_size(px(11.0))
                        .text_color(if danger { p.danger } else { p.dim })
                        .whitespace_nowrap()
                        .child(text)
                });
                frame
                    .py_0()
                    .pt(px(2.0))
                    .border_b_1()
                    .border_color(p.border)
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_3()
                            .child(button)
                            .children(notice),
                    )
                    .into_any_element()
            }
        }
    }

    /// 渲染状态栏。
    fn render_status(&self, p: &Palette) -> impl IntoElement {
        let (text, color) = match self.state.status() {
            Some(status) if status.kind == StatusKind::Error => (status.text.clone(), p.danger),
            Some(status) => (status.text.clone(), p.ok),
            None => (String::new(), p.dim),
        };
        div()
            .h(px(28.0))
            .px_4()
            .flex()
            .flex_none()
            .items_center()
            .border_t_1()
            .border_color(p.border)
            .text_size(px(12.0))
            .text_color(color)
            .overflow_hidden()
            .whitespace_nowrap()
            .child(text)
    }
}

impl Render for SettingsView {
    /// 渲染设置窗口：左侧分组，右侧标题栏 + 虚拟滚动列表 + 状态栏。
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let started = Instant::now();
        let scroll_y = uniform_list_offset_y(&self.list_scroll);
        if scroll_y != self.last_scroll_y {
            // 列表滚动时收起已展开的下拉：焦点回根即触发选择器的失焦关闭。
            self.last_scroll_y = scroll_y;
            if !self.dropdowns.is_empty() {
                // 渲染中改焦点不会触发失焦通知，推迟到本帧之后再做。
                cx.defer_in(window, |this, window, cx| this.close_dropdowns(window, cx));
            }
        }
        let prefs = self.state.prefs();
        if !self.embedded && self.titled_locale != Some(prefs.locale) {
            // 窗口标题跟随界面语言（首帧与语言切换后各设一次）。
            self.titled_locale = Some(prefs.locale);
            window.set_window_title(&crate::settings_text::window_title(prefs.lang));
        }
        if self.themed_dark != Some(prefs.dark) {
            // 组件库（下拉选择器）的主题跟随设置页深浅色。
            self.themed_dark = Some(prefs.dark);
            Theme::change(
                if prefs.dark {
                    ThemeMode::Dark
                } else {
                    ThemeMode::Light
                },
                None,
                cx,
            );
        }
        let p = palette(prefs.dark, prefs.accent);
        let lang = prefs.lang;
        let header = self.header_len();
        let visible = self.state.visible_len();
        let total = header + visible;

        let body = if total == 0 {
            div()
                .flex_1()
                .flex()
                .items_center()
                .justify_center()
                .text_color(p.dim)
                .child(t(lang, Text::NoResults))
                .into_any_element()
        } else {
            uniform_list(
                "settings-rows",
                total,
                cx.processor(move |this, range: Range<usize>, window, cx| {
                    let started = Instant::now();
                    let rows: Vec<AnyElement> = range
                        .clone()
                        .map(|position| match split_list_index(position, header) {
                            Err(row) => this.render_header_row(row, &p, cx),
                            Ok(row) => this.render_row(row, &p, lang, window, cx),
                        })
                        .collect();
                    let elapsed = started.elapsed();
                    this.probe.row_batches += 1;
                    this.probe.rows_total += elapsed;
                    this.probe.rows_max = this.probe.rows_max.max(elapsed);
                    this.probe.last_rows_built = rows.len();
                    rows
                }),
            )
            .track_scroll(&self.list_scroll)
            .flex_1()
            .into_any_element()
        };

        let root = div()
            .id("settings-root")
            .track_focus(&self.focus)
            .flex()
            .size_full()
            .bg(p.bg)
            .text_color(p.text)
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _event: &MouseDownEvent, window, cx| {
                    window.focus(&this.focus, cx);
                    if this.state.edit().is_some() || this.state.capture().is_some() {
                        this.state.cancel_input();
                        cx.notify();
                    }
                }),
            )
            .on_key_down(cx.listener(|this, event: &KeyDownEvent, _window, cx| {
                let m = event.keystroke.modifiers;
                let mods = KeyMods {
                    ctrl: m.control,
                    alt: m.alt,
                    shift: m.shift,
                    win: m.platform,
                };
                let key = event.keystroke.key.as_str();
                let paste = (m.control && key == "v")
                    .then(|| cx.read_from_clipboard().and_then(|item| item.text()))
                    .flatten();
                let handled = this.state.on_key(
                    key,
                    event.keystroke.key_char.as_deref(),
                    mods,
                    paste.as_deref(),
                );
                if handled {
                    this.flush_changes();
                    cx.stop_propagation();
                    cx.notify();
                }
            }))
            .when(!self.embedded, |root| {
                root.child(self.render_sidebar(&p, lang, cx))
            })
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .h_full()
                    .flex()
                    .flex_col()
                    .child(self.render_header(&p, lang, cx))
                    .child(body)
                    .child(self.render_status(&p)),
            );

        let elapsed = started.elapsed();
        self.probe.frames += 1;
        self.probe.root_total += elapsed;
        self.probe.root_max = self.probe.root_max.max(elapsed);
        root
    }
}

impl Drop for SettingsView {
    /// 窗口关闭时输出一次性能探针，便于事后核对。
    fn drop(&mut self) {
        self.log_perf();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 选项标签：路由模式本地化，其余原样；下拉与平铺共用。
    #[test]
    fn option_labels_localized() {
        assert_eq!(
            option_label("screenshot/image_format", "png", "zh-CN"),
            "PNG"
        );
        assert_eq!(
            option_label("screen_recording/output_format", "mp4", "zh-CN"),
            "mp4"
        );
        assert_eq!(option_label("tray/icon", "dark", "zh-CN"), "深色");
        assert_eq!(option_label("tray/icon", "dark", "en-US"), "Dark");
        let mode = option_label(KEY_LOCAL_ROUTE_MODE, "single", "zh-CN");
        assert_ne!(mode, "single");
        assert_eq!(option_label("other/key", "single", "zh-CN"), "single");
        for info in snow_i18n::locales() {
            assert_eq!(
                option_label("screenshot_translation/target_language", "ja", info.code),
                "日本語"
            );
            assert_eq!(
                option_label("interface/language", "zh_CN", info.code),
                "简体中文"
            );
            assert_eq!(
                option_label("interface/language", "en_US", info.code),
                "English"
            );
        }
        assert_eq!(
            option_label("screenshot_translation/source_language", "auto", "en-US"),
            "Auto detect"
        );
        assert_eq!(
            option_label("screenshot_translation/source_language", "auto", "zh-CN"),
            "自动识别"
        );
    }

    /// 下拉选项保持候选顺序，值与标签一一对应；选中值只认候选内的值。
    #[test]
    fn dropdown_items_and_selection() {
        const OPTIONS: &[&str] = &["a", "b", "c", "d", "e"];
        let items = dropdown_items("x/y", OPTIONS, "en-US");
        let values: Vec<&str> = items.iter().map(|i| i.value).collect();
        assert_eq!(values, OPTIONS);
        assert_eq!(items[2].title().as_ref(), "c");
        assert_eq!(dropdown_value(OPTIONS, "d"), Some("d"));
        assert_eq!(dropdown_value(OPTIONS, "zzz"), None);
        assert_eq!(dropdown_index(OPTIONS, "e"), Some(4));
        assert_eq!(dropdown_index(OPTIONS, ""), None);
    }

    /// 自动化操作 JSON 解析：全部操作类型与错误输入。
    #[test]
    fn autotest_ops_parse() {
        let text = r#"[
            {"op":"group","id":"screenshot"},
            {"op":"search","q":"theme"},
            {"op":"set","key":"screenshot/image_quality","value":80},
            {"op":"reset","key":"screenshot/image_quality"},
            {"op":"shortcut","key":"global_shortcuts/screenshot","index":0,"text":"F5"},
            {"op":"type","key":"screen_recording/frame_rate","text":"45"},
            {"op":"scroll","index":3},
            {"op":"perf"},
            {"op":"state"}
        ]"#;
        let ops = parse_autotest_ops(text).unwrap();
        assert_eq!(ops.len(), 9);
        assert_eq!(ops[0], AutotestOp::Group("screenshot".into()));
        assert_eq!(
            ops[2],
            AutotestOp::Set {
                key: "screenshot/image_quality".into(),
                value: json!(80)
            }
        );
        assert_eq!(
            ops[4],
            AutotestOp::Shortcut {
                key: "global_shortcuts/screenshot".into(),
                index: Some(0),
                text: "F5".into()
            }
        );
        assert_eq!(ops[6], AutotestOp::Scroll(3));
        assert!(parse_autotest_ops("not json").is_err());
        assert!(parse_autotest_ops(r#"[{"op":"boom"}]"#).is_err());
        assert!(parse_autotest_ops(r#"[{"op":"set"}]"#).is_err());
    }

    /// 深浅两套配色的底色不同，主色随配置。
    #[test]
    fn palette_follows_mode_and_accent() {
        let dark = palette(true, [1, 2, 3, 255]);
        let light = palette(false, [1, 2, 3, 255]);
        assert_ne!(dark.bg, light.bg);
        assert_eq!(dark.accent, light.accent);
        assert_eq!(dark.accent, rgba(0x010203FF));
    }
}
