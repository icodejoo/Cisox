//! 设置页状态机：行模型缓存、分组与搜索、编辑与快捷键录入、写回与重置。
//!
//! 不依赖 GPUI。所有写入都经 `ConfigStore::set_value` 归一化校验后原子落盘；
//! 校验失败或落盘失败时内存与磁盘保持原值。视图层只调用这里的方法并绘制结果。

use crate::settings_model::{
    Control, EditOutcome, LANGUAGE_KEY, ReadOnlyReason,
    THEME_COLOR_KEY, THEME_MODE_KEY, TextBuffer, apply_edit_key, control_for, describe_input_error,
    edit_text, find_shortcut_conflict, group_id_of, groups, humanize, is_modifier_key,
    parse_hex_color, parse_input, portable_to_hotkey_text, shortcut_from_keystroke,
    shortcut_texts, with_shortcut, without_shortcut, GLOBAL_SHORTCUT_GROUP,
};
use crate::settings_text::{Lang, Text, group_title, t};
use serde_json::Value;
use snow_config::schema::{self, entries};
use snow_config::store::ConfigStore;
use snow_config::value::json_eq;
use snow_ui::shell::hotkey::Hotkey;
use std::cell::RefCell;
use std::rc::Rc;
use std::time::{Duration, Instant};

/// 主线程共享的配置存储：设置页、截图、录制等模块从同一份取值。
pub type SharedConfig = Rc<RefCell<ConfigStore>>;

/// 默认主题主色（配置缺失或非法时使用），格式 RGBA。
const DEFAULT_ACCENT: [u8; 4] = [0x16, 0x77, 0xFF, 0xFF];
/// 主题模式：深色。
const THEME_DARK: &str = "dark";
/// 主题模式：浅色。
const THEME_LIGHT: &str = "light";
/// 主题模式与语言的“跟随系统”取值。
const FOLLOW_SYSTEM: &str = "system";

/// 一次成功写入的变更记录，供上层（热键重注册等）响应。
#[derive(Debug, Clone, PartialEq)]
pub struct ConfigChange {
    /// 变更的配置键。
    pub key: &'static str,
    /// 变更前的值（回滚用）。
    pub previous: Value,
}

/// 一行配置项的展示模型（构建一次，值变化时局部刷新）。
#[derive(Debug, Clone)]
pub struct RowModel {
    /// 配置键。
    pub key: &'static str,
    /// 可读标签。
    pub label: String,
    /// 控件类型。
    pub control: Control,
    /// 当前值。
    pub value: Value,
    /// 当前值是否等于默认值。
    pub is_default: bool,
    /// 快捷键行的文本列表。
    pub shortcuts: Vec<String>,
    /// 行内错误提示。
    pub error: Option<String>,
    /// 搜索用的小写文本（键、标签、分组标题）。
    haystack: String,
}

/// 当前列表范围。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scope {
    /// 某个分组（分组下标）。
    Group(usize),
    /// 搜索结果。
    Search,
}

/// 文本输入的目标。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EditTarget {
    /// 搜索框。
    Search,
    /// 某个配置项。
    Row(&'static str),
}

/// 正在进行的文本编辑。
#[derive(Debug, Clone)]
pub struct EditState {
    /// 编辑目标。
    pub target: EditTarget,
    /// 目标行的控件类型（搜索框为 `Text`）。
    pub control: Control,
    /// 编辑缓冲。
    pub buffer: TextBuffer,
}

/// 正在进行的快捷键录入。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CaptureState {
    /// 目标配置键。
    pub key: &'static str,
    /// 替换的下标；`None` 表示追加。
    pub index: Option<usize>,
}

/// 状态栏消息类别。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StatusKind {
    /// 普通信息。
    Info,
    /// 错误。
    Error,
}

/// 状态栏消息。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Status {
    /// 文本。
    pub text: String,
    /// 类别。
    pub kind: StatusKind,
}

/// 系统偏好快照（深色模式与界面语言）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SystemPrefs {
    /// 系统是否使用深色应用主题。
    pub dark: bool,
    /// 系统界面语言标记，如 `zh-CN`。
    pub language: String,
}

impl SystemPrefs {
    /// 读取当前系统偏好（会调用一次 `reg query`，不要放进每帧路径）。
    pub fn query() -> Self {
        Self {
            dark: crate::sys_prefs::system_prefers_dark(),
            language: crate::sys_prefs::system_ui_language(),
        }
    }
}

/// 界面偏好：由配置与系统偏好解析得到。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UiPrefs {
    /// 是否深色。
    pub dark: bool,
    /// 界面语言。
    pub lang: Lang,
    /// 主色 RGBA。
    pub accent: [u8; 4],
}

impl UiPrefs {
    /// 由配置值与系统偏好解析。
    ///
    /// # 参数
    /// - `theme_mode`：`system` / `light` / `dark`
    /// - `language`：`system` 或区域名
    /// - `accent`：主色文本
    /// - `system`：系统偏好
    pub fn resolve(theme_mode: &str, language: &str, accent: &str, system: &SystemPrefs) -> Self {
        let dark = match theme_mode {
            THEME_DARK => true,
            THEME_LIGHT => false,
            _ => system.dark,
        };
        Self {
            dark,
            lang: Lang::from_config(language, &system.language),
            accent: parse_hex_color(accent).unwrap_or(DEFAULT_ACCENT),
        }
    }
}

/// 性能探针：记录关键路径的最近一次耗时。
#[derive(Debug, Clone, Copy, Default)]
pub struct PerfStats {
    /// 一次性构建全部行模型的耗时。
    pub build_all_rows: Duration,
    /// 最近一次切换分组重建可见列表的耗时。
    pub last_switch: Duration,
    /// 最近一次搜索过滤的耗时。
    pub last_search: Duration,
}

/// 设置页用户动作。
#[derive(Debug, Clone, PartialEq)]
pub enum SettingsAction {
    /// 切换分组。
    SwitchGroup(usize),
    /// 设置搜索文本。
    SetSearch(String),
    /// 修改某项（已是最终值，如开关、滑条、候选）。
    Change {
        /// 配置键。
        key: &'static str,
        /// 新值。
        value: Value,
    },
    /// 重置某项为默认。
    Reset(&'static str),
    /// 重置当前范围内全部项。
    ResetScope,
    /// 开始文本编辑。
    BeginEdit(&'static str),
    /// 开始编辑搜索框。
    BeginSearch,
    /// 开始录入快捷键。
    BeginCapture {
        /// 配置键。
        key: &'static str,
        /// 替换下标，`None` 为追加。
        index: Option<usize>,
    },
    /// 删除一条快捷键。
    RemoveShortcut {
        /// 配置键。
        key: &'static str,
        /// 下标。
        index: usize,
    },
    /// 取消当前的编辑或录入。
    CancelInput,
}

/// 修饰键状态。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct KeyMods {
    /// Ctrl。
    pub ctrl: bool,
    /// Alt。
    pub alt: bool,
    /// Shift。
    pub shift: bool,
    /// Win。
    pub win: bool,
}

/// 设置页状态。
pub struct SettingsState {
    /// 共享配置存储。
    store: SharedConfig,
    /// 全部配置项（238 项核心 + Cisox 扩展）的行模型（下标即 schema 条目下标）。
    all_rows: Vec<RowModel>,
    /// 当前可见行（schema 条目下标）。
    visible: Vec<usize>,
    /// 当前范围。
    scope: Scope,
    /// 最近选中的分组（搜索清空后回到它）。
    group: usize,
    /// 搜索框文本。
    search: String,
    /// 进行中的文本编辑。
    edit: Option<EditState>,
    /// 进行中的快捷键录入。
    capture: Option<CaptureState>,
    /// 状态栏消息。
    status: Option<Status>,
    /// 系统偏好快照。
    system: SystemPrefs,
    /// 界面偏好。
    prefs: UiPrefs,
    /// 待上层处理的变更。
    pending: Vec<ConfigChange>,
    /// 性能探针。
    perf: PerfStats,
}

/// 把上一次写入前的值写回并落盘（热键重注册失败时回滚用）。
///
/// # 参数
/// - `store`：共享配置
/// - `key`：配置键
/// - `previous`：要还原的值
///
/// # 返回
/// 还原并落盘成功为 `Ok`；归一化拒绝或落盘失败为错误文本。
pub fn restore_value(store: &SharedConfig, key: &str, previous: Value) -> Result<(), String> {
    let mut store = store.borrow_mut();
    store.set_value(key, previous).map_err(|e| e.to_string())?;
    store.flush().map_err(|e| e.to_string())
}

impl SettingsState {
    /// 创建状态并一次性构建全部行模型。
    ///
    /// # 参数
    /// - `store`：共享配置
    /// - `system`：系统偏好快照
    ///
    /// ```ignore
    /// let state = SettingsState::new(store, SystemPrefs { dark: true, language: "zh-CN".into() });
    /// assert_eq!(state.total_rows(), snow_config::schema::entries().len());
    /// ```
    pub fn new(store: SharedConfig, system: SystemPrefs) -> Self {
        let started = Instant::now();
        let all_rows = {
            let guard = store.borrow();
            (0..entries().len())
                .map(|index| build_row(index, &guard))
                .collect::<Vec<_>>()
        };
        let prefs = resolve_prefs(&store.borrow(), &system);
        let mut state = Self {
            store,
            all_rows,
            visible: Vec::new(),
            scope: Scope::Group(0),
            group: 0,
            search: String::new(),
            edit: None,
            capture: None,
            status: None,
            system,
            prefs,
            pending: Vec::new(),
            perf: PerfStats::default(),
        };
        state.perf.build_all_rows = started.elapsed();
        state.rebuild_group_rows();
        state
    }

    /// 全部行数（等于 schema 条目数）。
    pub fn total_rows(&self) -> usize {
        self.all_rows.len()
    }

    /// 当前可见行数。
    pub fn visible_len(&self) -> usize {
        self.visible.len()
    }

    /// 取第 `position` 个可见行。
    pub fn visible_row(&self, position: usize) -> Option<&RowModel> {
        self.visible.get(position).map(|i| &self.all_rows[*i])
    }

    /// 按键取行模型。
    pub fn row_by_key(&self, key: &str) -> Option<&RowModel> {
        self.all_rows.iter().find(|r| r.key == key)
    }

    /// 当前范围。
    pub fn scope(&self) -> Scope {
        self.scope
    }

    /// 最近选中的分组下标。
    pub fn group(&self) -> usize {
        self.group
    }

    /// 搜索框文本。
    pub fn search(&self) -> &str {
        &self.search
    }

    /// 进行中的文本编辑。
    pub fn edit(&self) -> Option<&EditState> {
        self.edit.as_ref()
    }

    /// 进行中的快捷键录入。
    pub fn capture(&self) -> Option<CaptureState> {
        self.capture
    }

    /// 状态栏消息。
    pub fn status(&self) -> Option<&Status> {
        self.status.as_ref()
    }

    /// 界面偏好。
    pub fn prefs(&self) -> UiPrefs {
        self.prefs
    }

    /// 性能探针。
    pub fn perf(&self) -> PerfStats {
        self.perf
    }

    /// 取走待上层处理的变更。
    pub fn take_pending(&mut self) -> Vec<ConfigChange> {
        std::mem::take(&mut self.pending)
    }

    /// 当前范围的标题。
    pub fn scope_title(&self) -> String {
        match self.scope {
            Scope::Group(index) => group_title(self.prefs.lang, groups()[index].id).to_string(),
            Scope::Search => t(self.prefs.lang, Text::SearchResults).to_string(),
        }
    }

    /// 执行一个用户动作。
    ///
    /// # 参数
    /// - `action`：动作
    pub fn dispatch(&mut self, action: SettingsAction) {
        match action {
            SettingsAction::SwitchGroup(index) => self.switch_group(index),
            SettingsAction::SetSearch(text) => self.set_search(&text),
            SettingsAction::Change { key, value } => {
                self.edit = None;
                self.capture = None;
                let _ = self.apply(key, value);
            }
            SettingsAction::Reset(key) => {
                self.edit = None;
                self.capture = None;
                self.reset(key);
            }
            SettingsAction::ResetScope => self.reset_scope(),
            SettingsAction::BeginEdit(key) => self.begin_edit(key),
            SettingsAction::BeginSearch => self.begin_search(),
            SettingsAction::BeginCapture { key, index } => self.begin_capture(key, index),
            SettingsAction::RemoveShortcut { key, index } => self.remove_shortcut(key, index),
            SettingsAction::CancelInput => self.cancel_input(),
        }
    }

    /// 切换分组并清空搜索。
    fn switch_group(&mut self, index: usize) {
        if index >= groups().len() {
            return;
        }
        self.cancel_input();
        self.search.clear();
        self.group = index;
        self.scope = Scope::Group(index);
        let started = Instant::now();
        self.rebuild_group_rows();
        self.perf.last_switch = started.elapsed();
    }

    /// 重建当前分组的可见列表。
    fn rebuild_group_rows(&mut self) {
        self.visible = groups()[self.group].entries.clone();
        self.scope = Scope::Group(self.group);
    }

    /// 设置搜索文本：新查询以旧查询为前缀时，只在上次结果里增量过滤。
    ///
    /// # 参数
    /// - `text`：搜索框全文
    pub fn set_search(&mut self, text: &str) {
        let started = Instant::now();
        let previous = self.search.trim().to_lowercase();
        let query = text.trim().to_lowercase();
        self.search = text.to_string();
        if query.is_empty() {
            self.rebuild_group_rows();
        } else {
            let incremental =
                self.scope == Scope::Search && !previous.is_empty() && query.starts_with(&previous);
            let base: Vec<usize> = if incremental {
                std::mem::take(&mut self.visible)
            } else {
                (0..self.all_rows.len()).collect()
            };
            self.visible = base
                .into_iter()
                .filter(|i| self.all_rows[*i].haystack.contains(&query))
                .collect();
            self.scope = Scope::Search;
        }
        self.perf.last_search = started.elapsed();
    }

    /// 写入一个值：归一化校验 -> 原子落盘；任何一步失败都保持原值。
    ///
    /// # 参数
    /// - `key`：配置键
    /// - `value`：新值
    ///
    /// # 返回
    /// `Ok(true)` 已写入且有变化；`Ok(false)` 值未变；`Err` 为回显给用户的错误文本。
    pub fn apply(&mut self, key: &'static str, value: Value) -> Result<bool, String> {
        let lang = self.prefs.lang;
        let store = Rc::clone(&self.store);
        let before = store.borrow().value(key);
        let outcome: Result<bool, String> = {
            let mut guard = store.borrow_mut();
            match guard.set_value(key, value) {
                Err(error) => Err(format!("{}: {error}", t(lang, Text::InvalidValue))),
                Ok(()) => {
                    if json_eq(&before, &guard.value(key)) {
                        Ok(false)
                    } else if let Err(io_error) = guard.flush() {
                        // 落盘失败：把内存值还原，避免界面与磁盘不一致
                        if let Err(revert) = guard.set_value(key, before.clone()) {
                            tracing::error!(key, error = %revert, "落盘失败后还原内存值也失败");
                        }
                        tracing::error!(key, error = %io_error, "配置落盘失败");
                        Err(format!("{}: {io_error}", t(lang, Text::SaveFailed)))
                    } else {
                        Ok(true)
                    }
                }
            }
        };
        match &outcome {
            Ok(changed) => {
                self.refresh_row(key);
                self.set_row_error(key, None);
                if *changed {
                    self.after_write(key, before);
                }
            }
            Err(message) => {
                self.refresh_row(key);
                self.set_row_error(key, Some(message.clone()));
                self.set_status(StatusKind::Error, message.clone());
            }
        }
        outcome
    }

    /// 写入成功后的收尾：偏好刷新、待处理变更、状态栏提示。
    fn after_write(&mut self, key: &'static str, previous: Value) {
        if matches!(key, THEME_MODE_KEY | LANGUAGE_KEY | THEME_COLOR_KEY) {
            if self.system_follow_needed(key) {
                self.system = SystemPrefs::query();
            }
            self.prefs = resolve_prefs(&self.store.borrow(), &self.system);
        }
        let lang = self.prefs.lang;
        let mut text = format!("{}: {key}", t(lang, Text::Saved));
        if key == THEME_MODE_KEY || key == THEME_COLOR_KEY {
            text = format!("{text} ({})", t(lang, Text::ThemeLiveNote));
        } else if key == LANGUAGE_KEY {
            text = format!("{text} ({})", t(lang, Text::LanguageLiveNote));
        }
        self.set_status(StatusKind::Info, text);
        self.pending.push(ConfigChange { key, previous });
    }

    /// 写入 `key` 后是否需要重新读取系统偏好（值切到了“跟随系统”）。
    fn system_follow_needed(&self, key: &str) -> bool {
        let value = self.store.borrow().value(key);
        value.as_str() == Some(FOLLOW_SYSTEM)
    }

    /// 重置单项为默认值。
    ///
    /// # 参数
    /// - `key`：配置键
    pub fn reset(&mut self, key: &'static str) {
        if self.row_by_key(key).is_some_and(|r| matches!(r.control, Control::ReadOnly(_))) {
            return;
        }
        let default = schema::default_value(key);
        if let Ok(true) = self.apply(key, default) {
            let lang = self.prefs.lang;
            self.set_status(StatusKind::Info, format!("{}: {key}", t(lang, Text::Restored)));
        }
    }

    /// 重置当前范围内所有可编辑且不是默认值的项。
    pub fn reset_scope(&mut self) {
        self.cancel_input();
        let keys: Vec<&'static str> = self
            .visible
            .iter()
            .map(|i| &self.all_rows[*i])
            .filter(|r| !matches!(r.control, Control::ReadOnly(_)) && !r.is_default)
            .map(|r| r.key)
            .collect();
        let mut done = 0;
        for key in keys {
            if let Ok(true) = self.apply(key, schema::default_value(key)) {
                done += 1;
            }
        }
        let lang = self.prefs.lang;
        self.set_status(StatusKind::Info, format!("{} ({done})", t(lang, Text::Restored)));
    }

    /// 开始编辑搜索框。
    fn begin_search(&mut self) {
        self.capture = None;
        self.edit = Some(EditState {
            target: EditTarget::Search,
            control: Control::Text,
            buffer: TextBuffer::new(self.search.clone()),
        });
    }

    /// 开始文本编辑；不支持文本输入的控件忽略。
    ///
    /// # 参数
    /// - `key`：配置键
    pub fn begin_edit(&mut self, key: &'static str) {
        let Some(row) = self.row_by_key(key) else {
            return;
        };
        let editable = matches!(
            row.control,
            Control::Text
                | Control::Color
                | Control::IntText
                | Control::Slider(_)
                | Control::ListText
                | Control::JsonText
        );
        if !editable {
            return;
        }
        let state = EditState {
            target: EditTarget::Row(key),
            control: row.control,
            buffer: TextBuffer::new(edit_text(row.control, &row.value)),
        };
        self.capture = None;
        self.set_row_error(key, None);
        self.edit = Some(state);
    }

    /// 取消编辑与录入。
    pub fn cancel_input(&mut self) {
        if let Some(EditState { target: EditTarget::Row(key), .. }) = self.edit {
            self.set_row_error(key, None);
        }
        if let Some(capture) = self.capture {
            self.set_row_error(capture.key, None);
        }
        self.edit = None;
        self.capture = None;
    }

    /// 处理一次按键：录入或编辑进行中时消费按键。
    ///
    /// # 参数
    /// - `key`：GPUI 按键名
    /// - `key_char`：产生的字符
    /// - `mods`：修饰键
    /// - `paste`：Ctrl+V 时的剪贴板文本
    ///
    /// # 返回
    /// 按键是否已被消费（需要重绘）。
    pub fn on_key(
        &mut self,
        key: &str,
        key_char: Option<&str>,
        mods: KeyMods,
        paste: Option<&str>,
    ) -> bool {
        if let Some(capture) = self.capture {
            self.on_capture_key(capture, key, mods);
            return true;
        }
        let Some(edit) = self.edit.as_mut() else {
            return false;
        };
        let outcome = apply_edit_key(&mut edit.buffer, key, key_char, mods.ctrl, paste);
        let target = edit.target;
        let text = edit.buffer.text().to_string();
        match (target, outcome) {
            (EditTarget::Search, EditOutcome::Continue) => self.set_search(&text),
            (EditTarget::Search, EditOutcome::Commit) => self.edit = None,
            (EditTarget::Search, EditOutcome::Cancel) => {
                self.set_search("");
                self.edit = None;
            }
            (EditTarget::Row(row_key), EditOutcome::Continue) => self.set_row_error(row_key, None),
            (EditTarget::Row(_), EditOutcome::Commit) => self.commit_edit(),
            (EditTarget::Row(_), EditOutcome::Cancel) => self.cancel_input(),
        }
        true
    }

    /// 提交文本编辑：解析失败或校验失败时留在编辑态并显示错误。
    fn commit_edit(&mut self) {
        let Some(edit) = self.edit.clone() else {
            return;
        };
        let EditTarget::Row(key) = edit.target else {
            return;
        };
        let lang = self.prefs.lang;
        match parse_input(edit.control, edit.buffer.text()) {
            Err(error) => {
                let message = describe_input_error(lang, &error).to_string();
                self.set_row_error(key, Some(message.clone()));
                self.set_status(StatusKind::Error, message);
            }
            Ok(value) => {
                if self.apply(key, value).is_ok() {
                    self.edit = None;
                }
            }
        }
    }

    /// 开始录入快捷键；列表已满时拒绝追加。
    ///
    /// # 参数
    /// - `key`：配置键
    /// - `index`：替换下标，`None` 为追加
    pub fn begin_capture(&mut self, key: &'static str, index: Option<usize>) {
        let Some(row) = self.row_by_key(key) else {
            return;
        };
        let Control::Shortcuts { max_items, .. } = row.control else {
            return;
        };
        if index.is_none() && max_items.is_some_and(|max| row.shortcuts.len() >= max) {
            let message = list_full_text(self.prefs.lang);
            self.set_row_error(key, Some(message.clone()));
            self.set_status(StatusKind::Error, message);
            return;
        }
        self.edit = None;
        self.set_row_error(key, None);
        self.capture = Some(CaptureState { key, index });
    }

    /// 处理录入态下的一次按键。
    fn on_capture_key(&mut self, capture: CaptureState, key: &str, mods: KeyMods) {
        if key == "escape" {
            self.cancel_input();
            return;
        }
        if is_modifier_key(key) {
            return;
        }
        let Some(text) =
            shortcut_from_keystroke(key, mods.ctrl, mods.alt, mods.shift, mods.win)
        else {
            let message = unsupported_key_text(self.prefs.lang);
            self.set_row_error(capture.key, Some(message.clone()));
            self.set_status(StatusKind::Error, message);
            return;
        };
        if self.commit_shortcut(capture.key, capture.index, &text).is_ok() {
            self.capture = None;
        }
    }

    /// 提交一条快捷键：全局分组先验证热键可解析，再做同组冲突检测，最后写回。
    ///
    /// # 参数
    /// - `key`：配置键
    /// - `index`：替换下标，`None` 为追加
    /// - `text`：规范的可移植快捷键文本
    ///
    /// # 返回
    /// 已写入（或值未变）为 `Ok`；冲突、格式无效或写入失败为错误文本。
    pub fn commit_shortcut(
        &mut self,
        key: &'static str,
        index: Option<usize>,
        text: &str,
    ) -> Result<(), String> {
        let lang = self.prefs.lang;
        let fail = |this: &mut Self, message: String| {
            this.set_row_error(key, Some(message.clone()));
            this.set_status(StatusKind::Error, message.clone());
            Err(message)
        };
        if group_id_of(key) == GLOBAL_SHORTCUT_GROUP
            && let Err(error) = Hotkey::parse(&portable_to_hotkey_text(text))
        {
            return fail(self, format!("{}: {error}", t(lang, Text::InvalidValue)));
        }
        let store = Rc::clone(&self.store);
        let read = |k: &str| store.borrow().value(k);
        if let Some(conflict) = find_shortcut_conflict(&read, key, text, index) {
            let message = format!(
                "{}: {} ({})",
                t(lang, Text::ShortcutConflict),
                conflict.text,
                conflict.key
            );
            return fail(self, message);
        }
        let current = store.borrow().value(key);
        let count = shortcut_texts(&current).len();
        let max = self.row_by_key(key).and_then(|r| match r.control {
            Control::Shortcuts { max_items, .. } => max_items,
            _ => None,
        });
        if index.is_none() && max.is_some_and(|m| count >= m) {
            return fail(self, list_full_text(lang));
        }
        self.apply(key, with_shortcut(&current, index, text)).map(|_| ())
    }

    /// 删除一条快捷键。
    ///
    /// # 参数
    /// - `key`：配置键
    /// - `index`：下标
    pub fn remove_shortcut(&mut self, key: &'static str, index: usize) {
        self.cancel_input();
        let current = self.store.borrow().value(key);
        let _ = self.apply(key, without_shortcut(&current, index));
    }

    /// 上层（热键重注册）失败并已还原配置后，刷新界面并提示。
    ///
    /// # 参数
    /// - `key`：被回滚的配置键
    /// - `message`：错误提示
    pub fn notify_reverted(&mut self, key: &'static str, message: String) {
        self.refresh_row(key);
        self.set_row_error(key, Some(message.clone()));
        self.set_status(StatusKind::Error, message);
    }

    /// 从配置存储重新读取某一行的值。
    fn refresh_row(&mut self, key: &str) {
        let Some(position) = self.all_rows.iter().position(|r| r.key == key) else {
            return;
        };
        let row = build_row(position, &self.store.borrow());
        let error = self.all_rows[position].error.take();
        self.all_rows[position] = RowModel { error, ..row };
    }

    /// 设置或清除行内错误。
    fn set_row_error(&mut self, key: &str, error: Option<String>) {
        if let Some(row) = self.all_rows.iter_mut().find(|r| r.key == key) {
            row.error = error;
        }
    }

    /// 设置状态栏消息。
    fn set_status(&mut self, kind: StatusKind, text: String) {
        self.status = Some(Status { text, kind });
    }
}

/// 由配置与系统偏好解析界面偏好。
fn resolve_prefs(store: &ConfigStore, system: &SystemPrefs) -> UiPrefs {
    let text = |key: &str| store.value(key).as_str().unwrap_or_default().to_string();
    UiPrefs::resolve(&text(THEME_MODE_KEY), &text(LANGUAGE_KEY), &text(THEME_COLOR_KEY), system)
}

/// 构建一行的展示模型。
fn build_row(entry_index: usize, store: &ConfigStore) -> RowModel {
    let entry = &entries()[entry_index];
    let control = control_for(entry);
    let value = store.value(entry.key);
    let is_default = json_eq(&value, &entry.default);
    let label = humanize(entry.key);
    let shortcuts = if matches!(control, Control::Shortcuts { .. }) {
        shortcut_texts(&value)
    } else {
        Vec::new()
    };
    let group = group_id_of(entry.key);
    let haystack = format!(
        "{} {} {} {}",
        entry.key,
        label,
        group_title(Lang::ZhCn, group),
        group_title(Lang::EnUs, group)
    )
    .to_lowercase();
    RowModel {
        key: entry.key,
        label,
        control,
        value,
        is_default,
        shortcuts,
        error: None,
        haystack,
    }
}

/// “列表已满”提示。
fn list_full_text(lang: Lang) -> String {
    match lang {
        Lang::ZhCn => "快捷键数量已达上限".to_string(),
        Lang::EnUs => "Shortcut limit reached".to_string(),
    }
}

/// “不支持该按键”提示。
fn unsupported_key_text(lang: Lang) -> String {
    match lang {
        Lang::ZhCn => "不支持该按键".to_string(),
        Lang::EnUs => "Unsupported key".to_string(),
    }
}

/// 只读原因的界面文案。
///
/// # 参数
/// - `lang`：界面语言
/// - `reason`：只读原因
pub fn read_only_note(lang: Lang, reason: ReadOnlyReason) -> &'static str {
    match (lang, reason) {
        (Lang::ZhCn, ReadOnlyReason::Internal) => "内部固定值",
        (Lang::EnUs, ReadOnlyReason::Internal) => "internal value",
        (Lang::ZhCn, ReadOnlyReason::Secret) => "含密钥，不在此显示",
        (Lang::EnUs, ReadOnlyReason::Secret) => "contains secrets, hidden",
        (Lang::ZhCn, ReadOnlyReason::TooLarge) => "结构较大，暂无行内编辑器",
        (Lang::EnUs, ReadOnlyReason::TooLarge) => "large structure, no inline editor yet",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use snow_config::document::ConfigDocument;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU32, Ordering};

    /// 生成互不冲突的临时目录序号。
    static COUNTER: AtomicU32 = AtomicU32::new(0);

    /// 固定的系统偏好（浅色、英文），避免测试依赖真实系统。
    fn fake_system() -> SystemPrefs {
        SystemPrefs {
            dark: false,
            language: "en-US".to_string(),
        }
    }

    /// 在临时目录里建一个状态，返回状态、共享存储与配置文件路径。
    fn fixture() -> (SettingsState, SharedConfig, PathBuf) {
        let dir = std::env::temp_dir().join(format!(
            "snow-settings-state-{}-{}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::SeqCst)
        ));
        let path = dir.join("config.json");
        let store: SharedConfig = Rc::new(RefCell::new(ConfigStore::open(&path)));
        let state = SettingsState::new(Rc::clone(&store), fake_system());
        (state, store, path)
    }

    /// 读取磁盘上的配置值。
    fn disk_value(path: &PathBuf, key: &str) -> Option<Value> {
        let bytes = std::fs::read(path).ok()?;
        Some(ConfigDocument::from_bytes(Some(&bytes)).value(key))
    }

    /// 全部配置项（238 项核心 + Cisox 扩展）都有行模型，分组切换后可见数等于分组条目数，合计不丢。
    #[test]
    fn all_238_rows_reachable_through_groups() {
        let (mut state, _, _) = fixture();
        let total = entries().len();
        assert_eq!(state.total_rows(), total);
        let mut seen = 0;
        for (index, group) in groups().iter().enumerate() {
            state.dispatch(SettingsAction::SwitchGroup(index));
            assert_eq!(state.visible_len(), group.entries.len());
            seen += state.visible_len();
        }
        assert_eq!(seen, total);
        assert!(state.visible_row(0).is_some());
    }

    /// 搜索增量过滤：扩展查询只缩小结果，清空后回到分组。
    #[test]
    fn search_is_incremental_and_clears() {
        let (mut state, _, _) = fixture();
        state.set_search("qual");
        let first = state.visible_len();
        assert!(first >= 1);
        assert_eq!(state.scope(), Scope::Search);
        state.set_search("quality");
        assert!(state.visible_len() <= first);
        assert!((0..state.visible_len()).any(|i| state.visible_row(i).unwrap().key == "screenshot/image_quality"));
        // 非前缀的新查询会重新全量过滤
        state.set_search("shutter_nothing_zzz");
        assert_eq!(state.visible_len(), 0);
        state.set_search("");
        assert_eq!(state.scope(), Scope::Group(0));
        assert_eq!(state.visible_len(), groups()[0].entries.len());
        // 中文分组标题也能搜到
        state.set_search("托盘");
        assert!(state.visible_len() >= 1);
    }

    /// 合法写入落盘：磁盘内容与内存序列化一致，且能读回。
    #[test]
    fn apply_persists_and_round_trips() {
        let (mut state, store, path) = fixture();
        assert_eq!(state.apply("screenshot/image_quality", json!(80)), Ok(true));
        assert_eq!(disk_value(&path, "screenshot/image_quality"), Some(json!(80)));
        assert_eq!(std::fs::read(&path).unwrap(), store.borrow().document().to_bytes());
        assert!(!store.borrow().is_dirty());
        // 值未变：不再产生变更
        assert_eq!(state.apply("screenshot/image_quality", json!(80)), Ok(false));
        let changes = state.take_pending();
        assert_eq!(changes.len(), 1);
        assert_eq!(changes[0].previous, json!(100));
        assert!(state.take_pending().is_empty());
        assert!(!state.row_by_key("screenshot/image_quality").unwrap().is_default);
    }

    /// 校验失败：不落盘、内存不变、行内报错。
    #[test]
    fn invalid_value_does_not_write() {
        let (mut state, store, path) = fixture();
        let result = state.apply("screenshot/image_quality", json!(150));
        assert!(result.is_err());
        assert!(!path.exists(), "校验失败不应产生配置文件");
        assert_eq!(store.borrow().value("screenshot/image_quality"), json!(100));
        assert!(state.row_by_key("screenshot/image_quality").unwrap().error.is_some());
        assert_eq!(state.status().unwrap().kind, StatusKind::Error);
        assert!(state.apply("no/such_key", json!(1)).is_err());
        assert!(state.take_pending().is_empty());
    }

    /// 落盘失败：内存值还原并提示。
    #[test]
    fn flush_failure_reverts_memory() {
        let dir = std::env::temp_dir().join(format!(
            "snow-settings-state-blocked-{}-{}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::SeqCst)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let blocker = dir.join("blocker");
        std::fs::write(&blocker, b"x").unwrap();
        let path = blocker.join("config.json");
        let store: SharedConfig = Rc::new(RefCell::new(ConfigStore::open(&path)));
        let mut state = SettingsState::new(Rc::clone(&store), fake_system());
        let result = state.apply("screenshot/image_quality", json!(70));
        assert!(result.is_err());
        assert_eq!(store.borrow().value("screenshot/image_quality"), json!(100));
        assert_eq!(state.row_by_key("screenshot/image_quality").unwrap().value, json!(100));
        assert!(state.take_pending().is_empty());
    }

    /// 重置单项与重置范围。
    #[test]
    fn reset_item_and_scope() {
        let (mut state, _, path) = fixture();
        let shot = groups().iter().position(|g| g.id == "screenshot").unwrap();
        state.dispatch(SettingsAction::SwitchGroup(shot));
        state.apply("screenshot/image_quality", json!(60)).unwrap();
        state.apply("screenshot/delay_seconds", json!(5)).unwrap();
        state.dispatch(SettingsAction::Reset("screenshot/image_quality"));
        assert_eq!(disk_value(&path, "screenshot/image_quality"), Some(json!(100)));
        assert!(state.row_by_key("screenshot/image_quality").unwrap().is_default);
        state.dispatch(SettingsAction::ResetScope);
        assert_eq!(disk_value(&path, "screenshot/delay_seconds"), Some(json!(schema::default_value("screenshot/delay_seconds"))));
        // 只读项不可重置
        state.reset("storage/schema_version");
        assert_eq!(state.row_by_key("storage/schema_version").unwrap().value, json!(3));
    }

    /// 文本编辑：非法整数留在编辑态，合法后落盘。
    #[test]
    fn text_edit_flow() {
        let (mut state, _, path) = fixture();
        let key = "screen_recording/frame_rate";
        state.dispatch(SettingsAction::BeginEdit(key));
        assert!(state.edit().is_some());
        let mods = KeyMods::default();
        for _ in 0..8 {
            state.on_key("backspace", None, mods, None);
        }
        state.on_key("x", Some("x"), mods, None);
        state.on_key("enter", None, mods, None);
        assert!(state.edit().is_some(), "非法输入应留在编辑态");
        assert!(state.row_by_key(key).unwrap().error.is_some());
        state.on_key("backspace", None, mods, None);
        for ch in ["6", "0"] {
            state.on_key(ch, Some(ch), mods, None);
        }
        state.on_key("enter", None, mods, None);
        assert!(state.edit().is_none());
        assert_eq!(disk_value(&path, key), Some(json!(60)));
        // Esc 取消不写盘
        state.dispatch(SettingsAction::BeginEdit(key));
        state.on_key("9", Some("9"), mods, None);
        state.on_key("escape", None, mods, None);
        assert!(state.edit().is_none());
        assert_eq!(disk_value(&path, key), Some(json!(60)));
    }

    /// 搜索框输入实时过滤，Esc 清空。
    #[test]
    fn search_box_typing() {
        let (mut state, _, _) = fixture();
        state.dispatch(SettingsAction::BeginSearch);
        let mods = KeyMods::default();
        for ch in ["t", "h", "e", "m", "e"] {
            state.on_key(ch, Some(ch), mods, None);
        }
        assert_eq!(state.search(), "theme");
        assert!(state.visible_len() >= 2);
        state.on_key("escape", None, mods, None);
        assert_eq!(state.search(), "");
        assert!(state.edit().is_none());
    }

    /// 快捷键录入：成功写回、冲突拒绝且不落盘、非法全局热键拒绝。
    #[test]
    fn shortcut_capture_and_conflict() {
        let (mut state, _, path) = fixture();
        let key = "global_shortcuts/screen_record";
        state.dispatch(SettingsAction::BeginCapture { key, index: None });
        assert!(state.capture().is_some());
        let mods = KeyMods { ctrl: true, alt: true, ..KeyMods::default() };
        // 纯修饰键被忽略，仍在录入
        state.on_key("control", None, mods, None);
        assert!(state.capture().is_some());
        state.on_key("r", Some("r"), mods, None);
        assert!(state.capture().is_none());
        assert_eq!(
            disk_value(&path, key),
            Some(json!([{"portable": "Ctrl+Alt+R"}]))
        );
        // 与截图热键(F1)冲突
        let before = std::fs::read(&path).unwrap();
        let result = state.commit_shortcut(key, None, "F1");
        assert!(result.is_err());
        assert_eq!(std::fs::read(&path).unwrap(), before, "冲突不应改动磁盘");
        // 无修饰的字母不是合法全局热键
        assert!(state.commit_shortcut(key, None, "A").is_err());
        assert_eq!(std::fs::read(&path).unwrap(), before);
        // 替换自身、删除
        assert!(state.commit_shortcut(key, Some(0), "Ctrl+Alt+R").is_ok());
        state.dispatch(SettingsAction::RemoveShortcut { key, index: 0 });
        assert_eq!(disk_value(&path, key), Some(json!([])));
        // Esc 取消录入
        state.dispatch(SettingsAction::BeginCapture { key, index: None });
        state.on_key("escape", None, KeyMods::default(), None);
        assert!(state.capture().is_none());
    }

    /// 快捷键上限：追加超限被拒绝。
    #[test]
    fn shortcut_limit_enforced() {
        let (mut state, _, _) = fixture();
        let key = "global_shortcuts/screenshot";
        assert!(state.commit_shortcut(key, None, "Ctrl+Alt+Q").is_ok());
        let full = state.row_by_key(key).unwrap().shortcuts.len();
        let max = match state.row_by_key(key).unwrap().control {
            Control::Shortcuts { max_items, .. } => max_items.unwrap(),
            _ => unreachable!(),
        };
        assert_eq!(full, max);
        assert!(state.commit_shortcut(key, None, "Ctrl+Alt+W").is_err());
        state.dispatch(SettingsAction::BeginCapture { key, index: None });
        assert!(state.capture().is_none());
    }

    /// 主题与语言：写入后立即改变界面偏好。
    #[test]
    fn prefs_follow_config() {
        let (mut state, _, _) = fixture();
        assert!(!state.prefs().dark);
        assert_eq!(state.prefs().lang, Lang::EnUs);
        state.apply("interface/theme_mode", json!("dark")).unwrap();
        assert!(state.prefs().dark);
        state.apply("interface/language", json!("zh_CN")).unwrap();
        assert_eq!(state.prefs().lang, Lang::ZhCn);
        assert!(crate::settings_model::LANGUAGE_OPTIONS.contains(&"zh_CN"));
        state.apply("interface/theme_primary_color", json!("#FF0000FF")).unwrap();
        assert_eq!(state.prefs().accent, [255, 0, 0, 255]);
        assert!(state.status().unwrap().text.contains("Saved") || state.status().unwrap().text.contains("已保存"));
    }

    /// 解析函数不依赖系统：模式与语言解析。
    #[test]
    fn ui_prefs_resolution() {
        let dark_system = SystemPrefs { dark: true, language: "zh-CN".into() };
        let p = UiPrefs::resolve("system", "system", "#112233FF", &dark_system);
        assert!(p.dark && p.lang == Lang::ZhCn && p.accent == [0x11, 0x22, 0x33, 0xFF]);
        let p = UiPrefs::resolve("light", "en_US", "bad", &dark_system);
        assert!(!p.dark && p.lang == Lang::EnUs && p.accent == DEFAULT_ACCENT);
    }

    /// 回滚辅助：还原并落盘。
    #[test]
    fn restore_value_reverts_disk() {
        let (mut state, store, path) = fixture();
        state.apply("screenshot/image_quality", json!(50)).unwrap();
        restore_value(&store, "screenshot/image_quality", json!(100)).unwrap();
        assert_eq!(disk_value(&path, "screenshot/image_quality"), Some(json!(100)));
        assert!(restore_value(&store, "screenshot/image_quality", json!(999)).is_err());
        state.notify_reverted("screenshot/image_quality", "x".into());
        assert_eq!(state.row_by_key("screenshot/image_quality").unwrap().value, json!(100));
        assert_eq!(state.status().unwrap().kind, StatusKind::Error);
    }

    /// 只读项没有可用的文本编辑入口；预览文案存在。
    #[test]
    fn read_only_rows_not_editable() {
        let (mut state, _, _) = fixture();
        state.dispatch(SettingsAction::BeginEdit("storage/schema_version"));
        assert!(state.edit().is_none());
        state.dispatch(SettingsAction::BeginEdit("api_configuration/custom_models"));
        assert!(state.edit().is_none());
        for reason in [ReadOnlyReason::Internal, ReadOnlyReason::Secret, ReadOnlyReason::TooLarge] {
            assert!(!read_only_note(Lang::ZhCn, reason).is_empty());
            assert!(!read_only_note(Lang::EnUs, reason).is_empty());
        }
    }

    /// 语言候选包含系统与三种区域。
    #[test]
    fn schema_version_key_constant_matches() {
        assert_eq!(schema::SCHEMA_VERSION_KEY, "storage/schema_version");
    }
}
