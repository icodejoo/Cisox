//! 设置页模型：schema 分组、键到控件的映射、文本输入、快捷键录入与冲突检测。
//!
//! 本模块不依赖 GPUI，全部逻辑可离屏单测；视图层只负责把这里的结果画出来。

use crate::settings_text::{GROUP_IDS, Lang, Text, t};
use serde_json::Value;
use snow_config::schema::{IntRange, SCHEMA_VERSION_KEY, SchemaEntry, ValueKind, entries};
use snow_config::shortcut::{
    ShortcutBinding, bindings_conflict, canonical_portable_text, shortcut_bindings_from_json,
    shortcut_bindings_to_json,
};
use std::sync::OnceLock;

/// 语言配置键。
pub const LANGUAGE_KEY: &str = "interface/language";
/// 主题模式配置键。
pub const THEME_MODE_KEY: &str = "interface/theme_mode";
/// 主题主色配置键。
pub const THEME_COLOR_KEY: &str = "interface/theme_primary_color";
/// 目标语言配置键。
pub const TARGET_LANGUAGE_KEY: &str = "screenshot_translation/target_language";

/// 界面语言下拉的候选：由已发现的内置语言（`locale.toml`）生成，取其写入配置的取值，如 `en_US`。
///
/// # 示例
/// ```ignore
/// assert!(language_options().contains(&"zh_CN"));
/// ```
pub fn language_options() -> &'static [&'static str] {
    static OPTIONS: OnceLock<Vec<&'static str>> = OnceLock::new();
    OPTIONS.get_or_init(|| snow_i18n::locales().iter().map(|l| l.config_value).collect())
}
/// 结构化 JSON 默认值序列化长度超过该值时只读展示。
pub const JSON_EDIT_MAX_LEN: usize = 120;
/// 滑条离散格数。
pub const SLIDER_CELLS: usize = 20;
/// 含密钥、不在界面展示内容的配置键。
pub const SECRET_KEYS: &[&str] = &["api_configuration/custom_models"];
/// 颜色配置键的后缀。
const COLOR_KEY_SUFFIX: &str = "_color";
/// 单独允许 Shift 作为快捷键的分组前缀。
const SHIFT_ONLY_GROUP_PREFIX: &str = "screenshot_shortcuts/";
/// 步进按钮的最小分段数（范围跨度除以该值得到大步长）。
const STEP_DIVISIONS: i64 = 50;
/// 全局快捷键分组 id。
pub const GLOBAL_SHORTCUT_GROUP: &str = "global_shortcuts";
/// 纯修饰键的按键名（不构成快捷键主键）。
const MODIFIER_KEYS: &[&str] = &[
    "control", "ctrl", "shift", "alt", "platform", "win", "meta", "super", "function", "cmd",
];

/// 只读原因。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReadOnlyReason {
    /// 内部固定值（如 schema 版本）。
    Internal,
    /// 含密钥，不展示内容。
    Secret,
    /// 结构较大，暂无行内编辑器。
    TooLarge,
}

/// 配置项对应的控件类型。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Control {
    /// 开关。
    Switch,
    /// 带范围的整数滑条（含步进按钮与文本输入）。
    Slider(IntRange),
    /// 无范围整数文本框。
    IntText,
    /// 固定候选的下拉单选（至少两项一律用下拉）。
    Choice(&'static [&'static str]),
    /// 单行文本。
    Text,
    /// 颜色（色块 + 十六进制文本）。
    Color,
    /// 快捷键列表（录入、删除、冲突检测）。
    Shortcuts {
        /// 列表上限。
        max_items: Option<usize>,
        /// 是否允许单独的 Shift。
        allow_shift_only: bool,
    },
    /// 字符串列表（逗号分隔文本）。
    ListText,
    /// 小型结构化 JSON（单行文本）。
    JsonText,
    /// 只读展示。
    ReadOnly(ReadOnlyReason),
}

/// 一个设置分组：侧栏的一项。
#[derive(Debug)]
pub struct GroupInfo {
    /// 分组 id（schema 键的组前缀）。
    pub id: &'static str,
    /// 属于该分组的 schema 条目下标（顺序同 schema）。
    pub entries: Vec<usize>,
}

/// 全部分组（顺序同 [`GROUP_IDS`]），首次调用时构建并缓存。
///
/// ```ignore
/// assert_eq!(groups().iter().map(|g| g.entries.len()).sum::<usize>(), entries().len());
/// ```
pub fn groups() -> &'static [GroupInfo] {
    static GROUPS: OnceLock<Vec<GroupInfo>> = OnceLock::new();
    GROUPS.get_or_init(|| {
        let mut list: Vec<GroupInfo> = GROUP_IDS
            .iter()
            .map(|id| GroupInfo {
                id,
                entries: Vec::new(),
            })
            .collect();
        for (index, item) in entries().iter().enumerate() {
            let prefix = item.key.split('/').next().unwrap_or_default();
            if let Some(group) = list.iter_mut().find(|g| g.id == prefix) {
                group.entries.push(index);
            }
        }
        list
    })
}

/// 取键所属分组 id。
///
/// # 参数
/// - `key`：`"组/名"` 键
pub fn group_id_of(key: &str) -> &str {
    key.split('/').next().unwrap_or_default()
}

/// 由 schema 条目决定控件类型。
///
/// # 参数
/// - `entry`：schema 条目
///
/// # 返回
/// 该条目应使用的控件。
///
/// ```ignore
/// let e = snow_config::schema::entry_for("mcp/enabled").unwrap();
/// assert_eq!(control_for(e), Control::Switch);
/// ```
pub fn control_for(entry: &SchemaEntry) -> Control {
    if entry.key == SCHEMA_VERSION_KEY {
        return Control::ReadOnly(ReadOnlyReason::Internal);
    }
    if SECRET_KEYS.contains(&entry.key) {
        return Control::ReadOnly(ReadOnlyReason::Secret);
    }
    if entry.key == LANGUAGE_KEY {
        return Control::Choice(language_options());
    }
    match entry.kind {
        ValueKind::Boolean => Control::Switch,
        ValueKind::Integer => match entry.range {
            Some(range) if range.min >= range.max => Control::ReadOnly(ReadOnlyReason::Internal),
            Some(range) => Control::Slider(range),
            None => Control::IntText,
        },
        ValueKind::String => {
            if !entry.allowed.is_empty() {
                Control::Choice(entry.allowed)
            } else if entry.key.ends_with(COLOR_KEY_SUFFIX) {
                Control::Color
            } else {
                Control::Text
            }
        }
        ValueKind::StringList => Control::ListText,
        ValueKind::ShortcutList => Control::Shortcuts {
            max_items: entry.max_items,
            allow_shift_only: entry.key.starts_with(SHIFT_ONLY_GROUP_PREFIX),
        },
        ValueKind::Structured => {
            if entry.default.to_string().len() > JSON_EDIT_MAX_LEN {
                Control::ReadOnly(ReadOnlyReason::TooLarge)
            } else {
                Control::JsonText
            }
        }
    }
}

/// 把键的组内短名转成可读标签，例如 `image_quality` 变为 `Image quality`。
///
/// # 参数
/// - `key`：`"组/名"` 键
pub fn humanize(key: &str) -> String {
    let short = key.split_once('/').map_or(key, |(_, s)| s);
    let mut text = short.replace('_', " ");
    if let Some(first) = text.get(..1) {
        let upper = first.to_uppercase();
        text.replace_range(..1, &upper);
    }
    text
}

/// 值转为编辑框的初始文本。
///
/// # 参数
/// - `control`：控件类型
/// - `value`：当前值
pub fn edit_text(control: Control, value: &Value) -> String {
    match (control, value) {
        (_, Value::String(s)) => s.clone(),
        (Control::ListText, Value::Array(items)) => items
            .iter()
            .filter_map(Value::as_str)
            .collect::<Vec<_>>()
            .join(", "),
        (_, Value::Null) => String::new(),
        _ => value.to_string(),
    }
}

/// 值转为只读展示的预览文本（超过上限以省略号截断）。
///
/// # 参数
/// - `value`：当前值
/// - `max_chars`：最多显示的字符数
pub fn preview_text(value: &Value, max_chars: usize) -> String {
    let full = match value {
        Value::String(s) => s.clone(),
        _ => value.to_string(),
    };
    if full.chars().count() <= max_chars {
        return full;
    }
    let cut: String = full.chars().take(max_chars).collect();
    format!("{cut}…")
}

/// 文本输入解析失败的原因。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InputError {
    /// 不是整数。
    NotInteger,
    /// 不是合法 JSON。
    BadJson,
    /// 该控件不接受文本输入。
    NotEditable,
}

/// 把编辑框文本解析为待校验的 JSON 值（最终合法性仍由 `set_value` 归一化判定）。
///
/// # 参数
/// - `control`：控件类型
/// - `text`：输入文本
///
/// # 返回
/// 解析出的值；无法解析返回 [`InputError`]。
///
/// ```ignore
/// assert_eq!(parse_input(Control::IntText, " 30 ").unwrap(), serde_json::json!(30));
/// ```
pub fn parse_input(control: Control, text: &str) -> Result<Value, InputError> {
    match control {
        Control::Text | Control::Color | Control::Choice(_) => {
            Ok(Value::String(text.trim().to_string()))
        }
        Control::IntText | Control::Slider(_) => text
            .trim()
            .parse::<i64>()
            .map(|n| Value::Number(n.into()))
            .map_err(|_| InputError::NotInteger),
        Control::ListText => Ok(Value::Array(
            text.split(',')
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(|s| Value::String(s.to_string()))
                .collect(),
        )),
        Control::JsonText => serde_json::from_str(text).map_err(|_| InputError::BadJson),
        _ => Err(InputError::NotEditable),
    }
}

/// 输入错误的界面文案。
///
/// # 参数
/// - `lang`：界面语言
/// - `error`：错误原因
pub fn describe_input_error(lang: Lang, error: &InputError) -> String {
    t(
        lang,
        match error {
            InputError::NotInteger => Text::ErrNotInteger,
            InputError::BadJson => Text::ErrBadJson,
            InputError::NotEditable => Text::ErrNotEditable,
        },
    )
}

/// 单行文本编辑缓冲：光标以字符下标计。
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct TextBuffer {
    /// 文本内容。
    text: String,
    /// 光标位置（字符下标）。
    cursor: usize,
}

impl TextBuffer {
    /// 以给定文本创建，光标置于末尾。
    ///
    /// # 参数
    /// - `text`：初始文本
    pub fn new(text: impl Into<String>) -> Self {
        let text = text.into();
        let cursor = text.chars().count();
        Self { text, cursor }
    }

    /// 当前文本。
    pub fn text(&self) -> &str {
        &self.text
    }

    /// 光标字符下标。
    pub fn cursor(&self) -> usize {
        self.cursor
    }

    /// 字符下标转字节下标。
    fn byte_index(&self, chars: usize) -> usize {
        self.text
            .char_indices()
            .nth(chars)
            .map_or(self.text.len(), |(i, _)| i)
    }

    /// 在光标处插入文本（换行与控制字符被过滤）。
    ///
    /// # 参数
    /// - `s`：要插入的文本
    pub fn insert_str(&mut self, s: &str) {
        let clean: String = s.chars().filter(|c| !c.is_control()).collect();
        let at = self.byte_index(self.cursor);
        self.text.insert_str(at, &clean);
        self.cursor += clean.chars().count();
    }

    /// 删除光标前一个字符。
    pub fn backspace(&mut self) {
        if self.cursor == 0 {
            return;
        }
        let start = self.byte_index(self.cursor - 1);
        let end = self.byte_index(self.cursor);
        self.text.replace_range(start..end, "");
        self.cursor -= 1;
    }

    /// 删除光标后一个字符。
    pub fn delete(&mut self) {
        if self.cursor >= self.text.chars().count() {
            return;
        }
        let start = self.byte_index(self.cursor);
        let end = self.byte_index(self.cursor + 1);
        self.text.replace_range(start..end, "");
    }

    /// 光标左移一格。
    pub fn move_left(&mut self) {
        self.cursor = self.cursor.saturating_sub(1);
    }

    /// 光标右移一格。
    pub fn move_right(&mut self) {
        self.cursor = (self.cursor + 1).min(self.text.chars().count());
    }

    /// 光标移到开头。
    pub fn home(&mut self) {
        self.cursor = 0;
    }

    /// 光标移到末尾。
    pub fn end(&mut self) {
        self.cursor = self.text.chars().count();
    }

    /// 清空文本。
    pub fn clear(&mut self) {
        self.text.clear();
        self.cursor = 0;
    }
}

/// 一次按键在文本编辑中的结果。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EditOutcome {
    /// 继续编辑。
    Continue,
    /// 提交。
    Commit,
    /// 取消。
    Cancel,
}

/// 把一次按键应用到编辑缓冲。
///
/// # 参数
/// - `buffer`：编辑缓冲
/// - `key`：GPUI 按键名（如 `enter`、`a`）
/// - `key_char`：按键产生的字符（可能为空）
/// - `ctrl`：Ctrl 是否按下
/// - `paste`：Ctrl+V 时由视图层取得的剪贴板文本
///
/// # 返回
/// 编辑结果。
pub fn apply_edit_key(
    buffer: &mut TextBuffer,
    key: &str,
    key_char: Option<&str>,
    ctrl: bool,
    paste: Option<&str>,
) -> EditOutcome {
    match key {
        "enter" => return EditOutcome::Commit,
        "escape" => return EditOutcome::Cancel,
        "backspace" => buffer.backspace(),
        "delete" => buffer.delete(),
        "left" => buffer.move_left(),
        "right" => buffer.move_right(),
        "home" => buffer.home(),
        "end" => buffer.end(),
        _ if ctrl => {
            if key == "v"
                && let Some(text) = paste
            {
                buffer.insert_str(text);
            }
        }
        _ => {
            if let Some(ch) = key_char {
                buffer.insert_str(ch);
            }
        }
    }
    EditOutcome::Continue
}

/// 取光标附近最多 `max_chars` 个字符的可见窗口，避免长文本撑破固定宽度的输入框。
///
/// # 参数
/// - `buffer`：编辑缓冲
/// - `max_chars`：窗口最大字符数
///
/// # 返回
/// `(可见文本, 光标在可见文本中的字符下标)`。
pub fn window_text(buffer: &TextBuffer, max_chars: usize) -> (String, usize) {
    let total = buffer.text.chars().count();
    if total <= max_chars {
        return (buffer.text.clone(), buffer.cursor);
    }
    let start = buffer
        .cursor
        .saturating_sub(max_chars * 3 / 4)
        .min(total - max_chars);
    let visible: String = buffer.text.chars().skip(start).take(max_chars).collect();
    (visible, buffer.cursor - start)
}

/// 整数按步长增减并夹取到范围内。
///
/// # 参数
/// - `value`：当前值
/// - `range`：取值范围
/// - `direction`：`1` 增、`-1` 减
/// - `large`：为真时使用大步长（范围跨度的约 1/50）
pub fn step_int(value: i64, range: IntRange, direction: i64, large: bool) -> i64 {
    let (min, max) = (i64::from(range.min), i64::from(range.max));
    let base = i64::from(range.step).max(1);
    let step = if large {
        base.max((max - min) / STEP_DIVISIONS)
    } else {
        base
    };
    (value + direction * step).clamp(min, max)
}

/// 滑条第 `cell` 格对应的值（按步长对齐）。
///
/// # 参数
/// - `range`：取值范围
/// - `cell`：格下标，`0..SLIDER_CELLS`
pub fn slider_cell_value(range: IntRange, cell: usize) -> i64 {
    let (min, max) = (i64::from(range.min), i64::from(range.max));
    let span = max - min;
    let cells = (SLIDER_CELLS - 1) as i64;
    let raw = min + span * (cell as i64).min(cells) / cells;
    let step = i64::from(range.step).max(1);
    let snapped = min + ((raw - min) + step / 2) / step * step;
    snapped.clamp(min, max)
}

/// 当前值落在滑条的第几格（用于高亮已填充部分）。
///
/// # 参数
/// - `range`：取值范围
/// - `value`：当前值
pub fn slider_active_cell(range: IntRange, value: i64) -> usize {
    let (min, max) = (i64::from(range.min), i64::from(range.max));
    let span = (max - min).max(1);
    let cells = (SLIDER_CELLS - 1) as i64;
    (((value.clamp(min, max) - min) * cells + span / 2) / span) as usize
}

/// 解析 `#RRGGBBAA`（也接受 `#RRGGBB`）颜色。
///
/// # 参数
/// - `text`：颜色文本
///
/// # 返回
/// `[r, g, b, a]`；格式不对返回 `None`。
pub fn parse_hex_color(text: &str) -> Option<[u8; 4]> {
    let hex = text.trim().strip_prefix('#')?;
    if !hex.is_ascii() || !(hex.len() == 6 || hex.len() == 8) {
        return None;
    }
    let byte = |i: usize| u8::from_str_radix(&hex[i..i + 2], 16).ok();
    let alpha = if hex.len() == 8 { byte(6)? } else { u8::MAX };
    Some([byte(0)?, byte(2)?, byte(4)?, alpha])
}

/// 是否为纯修饰键的按键名（单独按下不构成快捷键）。
///
/// # 参数
/// - `key`：GPUI 按键名
pub fn is_modifier_key(key: &str) -> bool {
    MODIFIER_KEYS.contains(&key.to_ascii_lowercase().as_str())
}

/// GPUI 按键名转 Qt 可移植按键名；纯修饰键或不支持的键返回 `None`。
///
/// # 参数
/// - `key`：GPUI `Keystroke.key`
fn portable_key_name(key: &str) -> Option<String> {
    let lower = key.to_ascii_lowercase();
    if MODIFIER_KEYS.contains(&lower.as_str()) {
        return None;
    }
    let named = match lower.as_str() {
        "escape" => "Esc",
        "enter" => "Return",
        "backspace" => "Backspace",
        "delete" => "Del",
        "insert" => "Ins",
        "pageup" => "PgUp",
        "pagedown" => "PgDown",
        "space" => "Space",
        "tab" => "Tab",
        "printscreen" => "Print",
        "pause" => "Pause",
        "home" => "Home",
        "end" => "End",
        "up" => "Up",
        "down" => "Down",
        "left" => "Left",
        "right" => "Right",
        _ => "",
    };
    if !named.is_empty() {
        return Some(named.to_string());
    }
    let mut chars = lower.chars();
    if let (Some(c), None) = (chars.next(), chars.next()) {
        return Some(c.to_uppercase().collect());
    }
    let digits = lower.strip_prefix('f')?;
    (!digits.is_empty() && digits.chars().all(|c| c.is_ascii_digit()))
        .then(|| format!("F{digits}"))
}

/// 由一次按键事件生成规范的可移植快捷键文本。
///
/// # 参数
/// - `key`：GPUI 按键名
/// - `ctrl` / `alt` / `shift` / `win`：修饰键状态
/// - `allow_shift_only`：是否允许无修饰的 Shift 类快捷键（保留参数，与 schema 语义一致）
///
/// # 返回
/// 规范文本（如 `Ctrl+Shift+A`）；纯修饰键或不支持的键返回 `None`。
///
/// ```ignore
/// assert_eq!(shortcut_from_keystroke("a", true, false, true, false), Some("Ctrl+Shift+A".into()));
/// ```
pub fn shortcut_from_keystroke(
    key: &str,
    ctrl: bool,
    alt: bool,
    shift: bool,
    win: bool,
) -> Option<String> {
    let name = portable_key_name(key)?;
    let mut text = String::new();
    for (on, prefix) in [(win, "Meta+"), (ctrl, "Ctrl+"), (alt, "Alt+"), (shift, "Shift+")] {
        if on {
            text.push_str(prefix);
        }
    }
    text.push_str(&name);
    let canonical = canonical_portable_text(&text, false);
    (!canonical.is_empty()).then_some(canonical)
}

/// 可移植快捷键文本转全局热键服务能解析的文本。
///
/// # 参数
/// - `text`：可移植文本，如 `Meta+Print`
pub fn portable_to_hotkey_text(text: &str) -> String {
    text.split('+')
        .map(|token| match token.trim() {
            "Print" => "PrintScreen",
            "PgDown" => "PageDown",
            "Meta" => "Win",
            other => other,
        })
        .collect::<Vec<_>>()
        .join("+")
}

/// 快捷键冲突信息。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Conflict {
    /// 冲突方所在的配置键。
    pub key: &'static str,
    /// 冲突的快捷键文本。
    pub text: String,
}

/// 读出某个快捷键配置值里的可移植文本列表。
///
/// # 参数
/// - `value`：配置值
pub fn shortcut_texts(value: &Value) -> Vec<String> {
    shortcut_bindings_from_json(value, true, None)
        .0
        .into_iter()
        .map(|b| b.portable_text)
        .collect()
}

/// 在同一分组的全部快捷键配置里查找与候选冲突的绑定。
///
/// # 参数
/// - `read`：按键读取当前配置值
/// - `key`：正在编辑的配置键
/// - `candidate`：候选快捷键文本
/// - `skip_index`：被替换的那一项下标（同键内不与自己比较）
///
/// # 返回
/// 第一处冲突；无冲突为 `None`。
pub fn find_shortcut_conflict(
    read: &dyn Fn(&str) -> Value,
    key: &str,
    candidate: &str,
    skip_index: Option<usize>,
) -> Option<Conflict> {
    let group = group_id_of(key);
    let wanted = ShortcutBinding {
        portable_text: candidate.to_string(),
        macos_key: None,
    };
    for item in entries() {
        if item.kind != ValueKind::ShortcutList || group_id_of(item.key) != group {
            continue;
        }
        let (bindings, _, _) = shortcut_bindings_from_json(&read(item.key), true, None);
        for (index, binding) in bindings.iter().enumerate() {
            if item.key == key && Some(index) == skip_index {
                continue;
            }
            if bindings_conflict(binding, &wanted) {
                return Some(Conflict {
                    key: item.key,
                    text: binding.portable_text.clone(),
                });
            }
        }
    }
    None
}

/// 生成替换或追加一条快捷键后的新列表值。
///
/// # 参数
/// - `current`：当前配置值
/// - `index`：替换的下标；`None` 表示追加
/// - `text`：新的快捷键文本
pub fn with_shortcut(current: &Value, index: Option<usize>, text: &str) -> Value {
    let (mut bindings, _, _) = shortcut_bindings_from_json(current, true, None);
    let binding = ShortcutBinding {
        portable_text: text.to_string(),
        macos_key: None,
    };
    match index {
        Some(i) if i < bindings.len() => bindings[i] = binding,
        _ => bindings.push(binding),
    }
    shortcut_bindings_to_json(&bindings)
}

/// 生成删除一条快捷键后的新列表值。
///
/// # 参数
/// - `current`：当前配置值
/// - `index`：删除的下标（越界则不变）
pub fn without_shortcut(current: &Value, index: usize) -> Value {
    let (mut bindings, _, _) = shortcut_bindings_from_json(current, true, None);
    if index < bindings.len() {
        bindings.remove(index);
    }
    shortcut_bindings_to_json(&bindings)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use snow_config::schema::entry_for;
    use std::collections::HashSet;

    /// 取键的控件类型。
    fn control_of(key: &str) -> Control {
        control_for(entry_for(key).expect("键应存在"))
    }

    /// 238 个核心键与 Cisox 扩展键恰好各属于一个具名分组，且分组表与 schema 分组一一对应。
    #[test]
    fn groups_cover_all_238_keys_exactly_once() {
        let mut seen = HashSet::new();
        for group in groups() {
            assert!(!group.entries.is_empty(), "分组 {} 为空", group.id);
            for index in &group.entries {
                assert!(seen.insert(*index), "条目 {index} 重复归组");
                assert_eq!(group_id_of(entries()[*index].key), group.id);
            }
        }
        assert_eq!(seen.len(), entries().len());
        assert_eq!(seen.len(), snow_config::schema::CORE_ENTRY_COUNT + snow_config::extensions::EXTENSION_ENTRY_COUNT);
        assert_eq!(groups().len(), 28);
        let schema_groups: HashSet<_> = entries().iter().map(|e| group_id_of(e.key)).collect();
        let table_groups: HashSet<_> = groups().iter().map(|g| g.id).collect();
        assert_eq!(schema_groups, table_groups);
    }

    /// 每个键都有控件，且控件与 schema 类型一致。
    #[test]
    fn every_entry_maps_to_a_control() {
        for item in entries() {
            let control = control_for(item);
            let ok = match item.kind {
                ValueKind::Boolean => control == Control::Switch,
                ValueKind::ShortcutList => matches!(control, Control::Shortcuts { .. }),
                ValueKind::StringList => control == Control::ListText,
                ValueKind::Integer => matches!(
                    control,
                    Control::Slider(_) | Control::IntText | Control::ReadOnly(_)
                ),
                ValueKind::String => matches!(
                    control,
                    Control::Choice(_) | Control::Text | Control::Color
                ),
                ValueKind::Structured => {
                    matches!(control, Control::JsonText | Control::ReadOnly(_))
                }
            };
            assert!(ok, "{} 的控件 {control:?} 与类型 {:?} 不符", item.key, item.kind);
        }
    }

    /// 代表性键的控件类型。
    #[test]
    fn representative_controls() {
        assert_eq!(control_of("mcp/enabled"), Control::Switch);
        assert!(matches!(control_of("screenshot/image_quality"), Control::Slider(r) if r.max == 100));
        assert!(matches!(control_of("interface/theme_mode"), Control::Choice(o) if o.len() == 3));
        assert!(matches!(control_of("screenshot/image_format"), Control::Choice(o) if o.len() > 4));
        assert!(matches!(control_of("screenshot_translation/backend"), Control::Choice(o) if o.len() == 2));
        assert_eq!(control_of("interface/language"), Control::Choice(language_options()));
        // 界面语言候选来自已发现的语言，没有“跟随系统”与繁体
        assert_eq!(language_options(), ["en_US", "zh_CN"]);
        // 目标语言直接用 schema 白名单，没有“跟随系统”与繁体
        let Control::Choice(target) = control_of(TARGET_LANGUAGE_KEY) else { panic!("目标语言应为下拉") };
        assert!(!target.contains(&"system") && !target.contains(&"zh-Hant"));
        assert!(target.contains(&"zh-Hans") && target.contains(&"ja"));
        assert_eq!(control_of("interface/theme_primary_color"), Control::Color);
        assert_eq!(control_of("screenshot/image_save_directory"), Control::Text);
        assert_eq!(control_of("screen_recording/frame_rate"), Control::IntText);
        assert_eq!(
            control_of("storage/schema_version"),
            Control::ReadOnly(ReadOnlyReason::Internal)
        );
        assert_eq!(
            control_of("api_configuration/custom_models"),
            Control::ReadOnly(ReadOnlyReason::Secret)
        );
        assert_eq!(
            control_of("screenshot_toolbar/layout"),
            Control::ReadOnly(ReadOnlyReason::TooLarge)
        );
        assert_eq!(control_of("global_mouse/screenshot_copy"), Control::JsonText);
        assert_eq!(control_of("tray/menu_options"), Control::ListText);
        assert_eq!(
            control_of("screenshot_shortcuts/move_tool"),
            Control::Shortcuts {
                max_items: entry_for("screenshot_shortcuts/move_tool").and_then(|e| e.max_items),
                allow_shift_only: true
            }
        );
        assert!(matches!(
            control_of("global_shortcuts/screenshot"),
            Control::Shortcuts { allow_shift_only: false, .. }
        ));
    }

    /// 标签可读化。
    #[test]
    fn humanize_labels() {
        assert_eq!(humanize("screenshot/image_quality"), "Image quality");
        assert_eq!(humanize("mcp/enabled"), "Enabled");
    }

    /// 文本输入解析：整数、列表、JSON 与非法输入。
    #[test]
    fn parse_input_rules() {
        assert_eq!(parse_input(Control::IntText, " 30 "), Ok(json!(30)));
        assert_eq!(parse_input(Control::IntText, "3.5"), Err(InputError::NotInteger));
        assert_eq!(parse_input(Control::IntText, ""), Err(InputError::NotInteger));
        assert_eq!(parse_input(Control::ListText, "a, b,, c"), Ok(json!(["a", "b", "c"])));
        assert_eq!(parse_input(Control::ListText, ""), Ok(json!([])));
        assert_eq!(parse_input(Control::JsonText, "{\"a\":1}"), Ok(json!({"a": 1})));
        assert_eq!(parse_input(Control::JsonText, "{"), Err(InputError::BadJson));
        assert_eq!(parse_input(Control::Switch, "x"), Err(InputError::NotEditable));
        assert_eq!(parse_input(Control::Text, "  hi "), Ok(json!("hi")));
    }

    /// 编辑缓冲：插入、删除、光标移动，含多字节字符。
    #[test]
    fn text_buffer_editing() {
        let mut b = TextBuffer::new("ab");
        assert_eq!(b.cursor(), 2);
        b.insert_str("中\n文");
        assert_eq!(b.text(), "ab中文");
        b.move_left();
        b.backspace();
        assert_eq!(b.text(), "ab文");
        b.home();
        b.delete();
        assert_eq!(b.text(), "b文");
        b.end();
        b.move_right();
        assert_eq!(b.cursor(), 2);
        b.backspace();
        b.backspace();
        b.backspace();
        assert_eq!(b.text(), "");
        b.clear();
        assert_eq!(b.cursor(), 0);
    }

    /// 按键应用：提交、取消、字符输入、粘贴。
    #[test]
    fn apply_edit_keys() {
        let mut b = TextBuffer::new("");
        assert_eq!(apply_edit_key(&mut b, "a", Some("a"), false, None), EditOutcome::Continue);
        assert_eq!(apply_edit_key(&mut b, "v", Some("v"), true, Some("xy\n")), EditOutcome::Continue);
        assert_eq!(b.text(), "axy");
        assert_eq!(apply_edit_key(&mut b, "backspace", None, false, None), EditOutcome::Continue);
        assert_eq!(b.text(), "ax");
        assert_eq!(apply_edit_key(&mut b, "enter", None, false, None), EditOutcome::Commit);
        assert_eq!(apply_edit_key(&mut b, "escape", None, false, None), EditOutcome::Cancel);
        // Ctrl 组合键不产生字符
        assert_eq!(apply_edit_key(&mut b, "c", Some("c"), true, None), EditOutcome::Continue);
        assert_eq!(b.text(), "ax");
    }

    /// 长文本窗口化：光标始终在可见窗口内。
    #[test]
    fn window_text_keeps_cursor_visible() {
        let mut b = TextBuffer::new("0123456789".repeat(10));
        let (visible, cursor) = window_text(&b, 20);
        assert_eq!(visible.chars().count(), 20);
        assert!(cursor <= 20);
        b.home();
        let (visible, cursor) = window_text(&b, 20);
        assert!(visible.starts_with("0123"));
        assert_eq!(cursor, 0);
        let short = TextBuffer::new("abc");
        assert_eq!(window_text(&short, 20), ("abc".to_string(), 3));
    }

    /// 步进与滑条格值不越界、按步长对齐。
    #[test]
    fn slider_math() {
        let range = IntRange { min: 100, max: 2000, step: 100 };
        assert_eq!(step_int(2000, range, 1, false), 2000);
        assert_eq!(step_int(100, range, -1, false), 100);
        assert_eq!(step_int(500, range, 1, false), 600);
        assert_eq!(slider_cell_value(range, 0), 100);
        assert_eq!(slider_cell_value(range, SLIDER_CELLS - 1), 2000);
        for cell in 0..SLIDER_CELLS {
            let v = slider_cell_value(range, cell);
            assert!((100..=2000).contains(&v) && (v - 100) % 100 == 0, "{v}");
        }
        assert_eq!(slider_active_cell(range, 100), 0);
        assert_eq!(slider_active_cell(range, 2000), SLIDER_CELLS - 1);
        let quality = IntRange { min: 0, max: 100, step: 1 };
        assert_eq!(step_int(50, quality, 1, true), 52);
    }

    /// 颜色解析。
    #[test]
    fn hex_colors() {
        assert_eq!(parse_hex_color("#1677FFFF"), Some([0x16, 0x77, 0xFF, 0xFF]));
        assert_eq!(parse_hex_color("#00000080"), Some([0, 0, 0, 0x80]));
        assert_eq!(parse_hex_color("#FFFFFF"), Some([255, 255, 255, 255]));
        assert_eq!(parse_hex_color("1677FF"), None);
        assert_eq!(parse_hex_color("#12345"), None);
        assert_eq!(parse_hex_color("#GGGGGGGG"), None);
        assert_eq!(parse_hex_color("#中文中文"), None);
    }

    /// 按键转快捷键文本。
    #[test]
    fn keystroke_to_shortcut() {
        assert_eq!(shortcut_from_keystroke("a", true, false, true, false), Some("Ctrl+Shift+A".into()));
        assert_eq!(shortcut_from_keystroke("f1", false, false, false, false), Some("F1".into()));
        assert_eq!(shortcut_from_keystroke("escape", true, true, false, false), Some("Ctrl+Alt+Esc".into()));
        assert_eq!(shortcut_from_keystroke("printscreen", false, false, false, true), Some("Meta+Print".into()));
        assert_eq!(shortcut_from_keystroke("pagedown", true, false, false, false), Some("Ctrl+PgDown".into()));
        assert_eq!(shortcut_from_keystroke("control", true, false, false, false), None);
        assert_eq!(shortcut_from_keystroke("shift", false, false, true, false), None);
        assert_eq!(shortcut_from_keystroke("", true, false, false, false), None);
    }

    /// 可移植文本转热键服务文本，且热键服务能解析。
    #[test]
    fn portable_to_hotkey() {
        assert_eq!(portable_to_hotkey_text("Meta+Print"), "Win+PrintScreen");
        assert_eq!(portable_to_hotkey_text("Ctrl+PgDown"), "Ctrl+PageDown");
        for text in ["Ctrl+Alt+A", "F1", "Meta+Print", "Ctrl+PgDown", "Ctrl+Esc"] {
            let converted = portable_to_hotkey_text(text);
            assert!(
                snow_ui::shell::hotkey::Hotkey::parse(&converted).is_ok(),
                "{text} -> {converted} 无法解析"
            );
        }
    }

    /// 同组冲突检测：命中同键与同组其它键，替换自身不算冲突，跨组不算。
    #[test]
    fn shortcut_conflicts() {
        let read = |key: &str| match key {
            "global_shortcuts/screenshot" => json!([{"portable": "F1"}, {"portable": "Ctrl+Alt+A"}]),
            "global_shortcuts/screen_record" => json!([{"portable": "F2"}]),
            "drawing_shortcuts/brush" => json!([{"portable": "F1"}]),
            _ => json!([]),
        };
        let hit = find_shortcut_conflict(&read, "global_shortcuts/screen_record", "F1", None);
        assert_eq!(
            hit,
            Some(Conflict { key: "global_shortcuts/screenshot", text: "F1".into() })
        );
        // 大小写与别名等价
        assert!(find_shortcut_conflict(&read, "global_shortcuts/screen_record", "ctrl+alt+a", None).is_some());
        // 替换自身那一项
        assert!(find_shortcut_conflict(&read, "global_shortcuts/screen_record", "F2", Some(0)).is_none());
        // 未替换时与自己已有项冲突
        assert!(find_shortcut_conflict(&read, "global_shortcuts/screen_record", "F2", None).is_some());
        // 绘图组的 F1 不影响全局组之外的键
        assert!(find_shortcut_conflict(&read, "drawing_shortcuts/eraser", "F3", None).is_none());
        assert!(find_shortcut_conflict(&read, "global_shortcuts/screen_record", "F9", None).is_none());
    }

    /// 快捷键列表的替换、追加与删除。
    #[test]
    fn shortcut_list_edits() {
        let list = json!([{"portable": "F1"}, {"portable": "Ctrl+A"}]);
        assert_eq!(shortcut_texts(&list), ["F1", "Ctrl+A"]);
        assert_eq!(shortcut_texts(&with_shortcut(&list, Some(0), "F5")), ["F5", "Ctrl+A"]);
        assert_eq!(shortcut_texts(&with_shortcut(&list, None, "F6")), ["F1", "Ctrl+A", "F6"]);
        assert_eq!(shortcut_texts(&without_shortcut(&list, 0)), ["Ctrl+A"]);
        assert_eq!(shortcut_texts(&without_shortcut(&list, 9)), ["F1", "Ctrl+A"]);
    }
}
