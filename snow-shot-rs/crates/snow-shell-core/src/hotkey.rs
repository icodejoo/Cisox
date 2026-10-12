//! 全局热键：与 gpui 无关的热键模型、字符串解析与注册服务。
//!
//! 绑定携带的动作负载 `A` 对本模块不透明：热键触发后以 [`ActionOrigin::Hotkey`](crate::dispatch::ActionOrigin::Hotkey)
//! 把它交给 [`Dispatcher`]（UI 进程里 `A` 是 `AppCommand`，托盘宿主里是动作 id 字符串）。

use crate::dispatch::Dispatcher;
use crate::error::ShellError;
use std::collections::HashMap;
use std::fmt;
use std::str::FromStr;

/// 修饰键集合。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct Modifiers {
    /// Ctrl。
    pub ctrl: bool,
    /// Alt。
    pub alt: bool,
    /// Shift。
    pub shift: bool,
    /// Windows 键（macOS 为 Cmd）。
    pub win: bool,
}

/// Win32 `RegisterHotKey` 的修饰键位：Alt。
pub const WIN_MOD_ALT: u32 = 0x0001;
/// Win32 `RegisterHotKey` 的修饰键位：Ctrl。
pub const WIN_MOD_CONTROL: u32 = 0x0002;
/// Win32 `RegisterHotKey` 的修饰键位：Shift。
pub const WIN_MOD_SHIFT: u32 = 0x0004;
/// Win32 `RegisterHotKey` 的修饰键位：Win。
pub const WIN_MOD_WIN: u32 = 0x0008;

impl Modifiers {
    /// 折成 Win32 `RegisterHotKey` 使用的修饰键位掩码（Alt=1、Ctrl=2、Shift=4、Win=8）。
    ///
    /// # 返回
    /// 掩码；不含 `MOD_NOREPEAT`。
    ///
    /// ```rust
    /// use snow_shell_core::hotkey::Hotkey;
    /// let hk = Hotkey::parse("Ctrl+Alt+S").unwrap();
    /// assert_eq!(hk.modifiers.win_bits(), 0x0003);
    /// ```
    pub fn win_bits(self) -> u32 {
        [
            (self.alt, WIN_MOD_ALT),
            (self.ctrl, WIN_MOD_CONTROL),
            (self.shift, WIN_MOD_SHIFT),
            (self.win, WIN_MOD_WIN),
        ]
        .into_iter()
        .filter(|(on, _)| *on)
        .fold(0, |acc, (_, bit)| acc | bit)
    }
}

/// 主键，内部保存 W3C `code` 名（如 `KeyS`、`Digit1`、`F5`、`ArrowUp`）。
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Key {
    /// W3C code 名，同时是后端识别用的规范名。
    code: String,
    /// 面向用户的显示名。
    label: String,
}

impl Key {
    /// W3C code 名。
    pub fn code(&self) -> &str {
        &self.code
    }

    /// 对应的 Win32 虚拟键码（`RegisterHotKey` 的 `vk` 参数）；`parse` 产出的键都有值。
    ///
    /// # 返回
    /// 虚拟键码，无法映射（不应出现）返回 `None`。
    ///
    /// ```rust
    /// use snow_shell_core::hotkey::Hotkey;
    /// assert_eq!(Hotkey::parse("Ctrl+S").unwrap().key.win_vk(), Some(0x53));
    /// assert_eq!(Hotkey::parse("F5").unwrap().key.win_vk(), Some(0x74));
    /// ```
    pub fn win_vk(&self) -> Option<u32> {
        let code = self.code.as_str();
        if let Some(c) = code.strip_prefix("Key").and_then(single_char) {
            return c.is_ascii_uppercase().then_some(c as u32);
        }
        if let Some(c) = code.strip_prefix("Digit").and_then(single_char) {
            return c.is_ascii_digit().then_some(c as u32);
        }
        if let Some(n) = code.strip_prefix('F').and_then(|n| n.parse::<u32>().ok()) {
            return (1..=24).contains(&n).then_some(WIN_VK_F1 + n - 1);
        }
        NAMED_KEY_VK
            .iter()
            .find(|(name, _)| *name == code)
            .map(|(_, vk)| *vk)
    }

    /// 是否可以不带修饰键单独注册（功能键、翻页/编辑键等）。
    fn allowed_bare(&self) -> bool {
        let is_fn = self
            .code
            .strip_prefix('F')
            .is_some_and(|n| n.parse::<u8>().is_ok());
        is_fn
            || matches!(
                self.code.as_str(),
                "PrintScreen"
                    | "Pause"
                    | "Insert"
                    | "Delete"
                    | "Home"
                    | "End"
                    | "PageUp"
                    | "PageDown"
            )
    }
}

/// 热键：修饰键 + 主键。
///
/// 字符串形式如 `Ctrl+Alt+S`、`Shift+F1`、`Ctrl+Shift+Digit1`，不区分大小写，
/// 输出统一为 `Ctrl+Alt+Shift+Win+Key` 的规范顺序。
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Hotkey {
    /// 修饰键。
    pub modifiers: Modifiers,
    /// 主键。
    pub key: Key,
}

/// 热键字符串解析错误。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HotkeyParseError {
    /// 输入为空。
    Empty,
    /// 出现空片段（如 `Ctrl++S`）。
    EmptyToken,
    /// 修饰键重复。
    DuplicateModifier(String),
    /// 主键不在末尾或出现多个主键。
    MisplacedKey,
    /// 缺少主键（只有修饰键）。
    MissingKey,
    /// 无法识别的键名。
    UnknownKey(String),
    /// 该主键必须至少带一个 Ctrl/Alt/Win 修饰键。
    NeedModifier,
}

impl fmt::Display for HotkeyParseError {
    /// 输出可读的错误描述。
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            HotkeyParseError::Empty => write!(f, "热键为空"),
            HotkeyParseError::EmptyToken => write!(f, "热键含空片段"),
            HotkeyParseError::DuplicateModifier(m) => write!(f, "修饰键重复: {m}"),
            HotkeyParseError::MisplacedKey => write!(f, "主键必须唯一且位于末尾"),
            HotkeyParseError::MissingKey => write!(f, "缺少主键"),
            HotkeyParseError::UnknownKey(k) => write!(f, "无法识别的键: {k}"),
            HotkeyParseError::NeedModifier => write!(f, "该键需要至少一个 Ctrl/Alt/Win 修饰键"),
        }
    }
}

impl std::error::Error for HotkeyParseError {}

impl From<HotkeyParseError> for ShellError {
    /// 解析错误转适配层错误。
    fn from(e: HotkeyParseError) -> Self {
        ShellError::InvalidHotkey(e.to_string())
    }
}

/// 命名键别名表：(小写别名, W3C code, 显示名)。
const NAMED_KEYS: &[(&[&str], &str, &str)] = &[
    (&["space"], "Space", "Space"),
    (&["enter", "return"], "Enter", "Enter"),
    (&["esc", "escape"], "Escape", "Esc"),
    (&["tab"], "Tab", "Tab"),
    (&["backspace"], "Backspace", "Backspace"),
    (&["delete", "del"], "Delete", "Delete"),
    (&["insert", "ins"], "Insert", "Insert"),
    (&["home"], "Home", "Home"),
    (&["end"], "End", "End"),
    (&["pageup", "pgup"], "PageUp", "PageUp"),
    (&["pagedown", "pgdn"], "PageDown", "PageDown"),
    (&["up", "arrowup"], "ArrowUp", "Up"),
    (&["down", "arrowdown"], "ArrowDown", "Down"),
    (&["left", "arrowleft"], "ArrowLeft", "Left"),
    (&["right", "arrowright"], "ArrowRight", "Right"),
    (
        &["printscreen", "prtsc", "prtscr"],
        "PrintScreen",
        "PrintScreen",
    ),
    (&["pause", "pausebreak"], "Pause", "Pause"),
    (&["minus", "-"], "Minus", "-"),
    (&["equal", "="], "Equal", "="),
    (&["comma", ","], "Comma", ","),
    (&["period", "."], "Period", "."),
    (&["slash", "/"], "Slash", "/"),
    (&["semicolon", ";"], "Semicolon", ";"),
    (&["quote", "'"], "Quote", "'"),
    (&["backquote", "`"], "Backquote", "`"),
    (&["bracketleft", "["], "BracketLeft", "["),
    (&["bracketright", "]"], "BracketRight", "]"),
    (&["backslash", "\\"], "Backslash", "\\"),
];

/// `F1` 的 Win32 虚拟键码，`F2..F24` 依次递增。
const WIN_VK_F1: u32 = 0x70;

/// 命名键（W3C code）到 Win32 虚拟键码；覆盖 [`NAMED_KEYS`] 的全部规范名。
const NAMED_KEY_VK: &[(&str, u32)] = &[
    ("Space", 0x20),
    ("Enter", 0x0D),
    ("Escape", 0x1B),
    ("Tab", 0x09),
    ("Backspace", 0x08),
    ("Delete", 0x2E),
    ("Insert", 0x2D),
    ("Home", 0x24),
    ("End", 0x23),
    ("PageUp", 0x21),
    ("PageDown", 0x22),
    ("ArrowUp", 0x26),
    ("ArrowDown", 0x28),
    ("ArrowLeft", 0x25),
    ("ArrowRight", 0x27),
    ("PrintScreen", 0x2C),
    ("Pause", 0x13),
    ("Minus", 0xBD),
    ("Equal", 0xBB),
    ("Comma", 0xBC),
    ("Period", 0xBE),
    ("Slash", 0xBF),
    ("Semicolon", 0xBA),
    ("Quote", 0xDE),
    ("Backquote", 0xC0),
    ("BracketLeft", 0xDB),
    ("BracketRight", 0xDD),
    ("Backslash", 0xDC),
];

/// 取只含一个字符的字符串的那个字符。
fn single_char(s: &str) -> Option<char> {
    let mut it = s.chars();
    match (it.next(), it.next()) {
        (Some(c), None) => Some(c),
        _ => None,
    }
}

/// 字母主键（`c` 为 ASCII 字母，大小写均可）。
fn letter_key(c: char) -> Key {
    let up = c.to_ascii_uppercase();
    Key {
        code: format!("Key{up}"),
        label: up.to_string(),
    }
}

/// 数字主键（`c` 为 ASCII 数字）。
fn digit_key(c: char) -> Key {
    Key {
        code: format!("Digit{c}"),
        label: c.to_string(),
    }
}

/// 解析主键片段；不是主键时返回 `UnknownKey`。
fn parse_key(token: &str) -> Result<Key, HotkeyParseError> {
    let lower = token.to_ascii_lowercase();
    let unknown = || HotkeyParseError::UnknownKey(token.to_string());
    let single = single_char;
    // 单字符：字母 / 数字
    if let Some(c) = single(&lower) {
        if c.is_ascii_lowercase() {
            return Ok(letter_key(c));
        }
        if c.is_ascii_digit() {
            return Ok(digit_key(c));
        }
    }
    // 显式 code 名：KeyS / Digit1
    if let Some(rest) = lower.strip_prefix("key")
        && let Some(c) = single(rest).filter(char::is_ascii_lowercase)
    {
        return Ok(letter_key(c));
    }
    if let Some(rest) = lower.strip_prefix("digit")
        && let Some(c) = single(rest).filter(char::is_ascii_digit)
    {
        return Ok(digit_key(c));
    }
    // 功能键 F1..F24
    if let Some(n) = lower.strip_prefix('f').and_then(|n| n.parse::<u8>().ok())
        && (1..=24).contains(&n)
    {
        return Ok(Key {
            code: format!("F{n}"),
            label: format!("F{n}"),
        });
    }
    // 命名键
    NAMED_KEYS
        .iter()
        .find(|(aliases, _, _)| aliases.contains(&lower.as_str()))
        .map(|(_, code, label)| Key {
            code: (*code).to_string(),
            label: (*label).to_string(),
        })
        .ok_or_else(unknown)
}

/// 尝试把片段解析为修饰键并并入集合；不是修饰键返回 `Ok(false)`。
fn apply_modifier(mods: &mut Modifiers, token: &str) -> Result<bool, HotkeyParseError> {
    let lower = token.to_ascii_lowercase();
    let slot = match lower.as_str() {
        "ctrl" | "control" | "cmdorctrl" | "commandorcontrol" => &mut mods.ctrl,
        "alt" | "option" => &mut mods.alt,
        "shift" => &mut mods.shift,
        "win" | "super" | "meta" | "cmd" | "command" => &mut mods.win,
        _ => return Ok(false),
    };
    if *slot {
        return Err(HotkeyParseError::DuplicateModifier(token.to_string()));
    }
    *slot = true;
    Ok(true)
}

impl FromStr for Hotkey {
    type Err = HotkeyParseError;

    /// 解析 `Ctrl+Alt+S` 形式的字符串。
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let s = s.trim();
        if s.is_empty() {
            return Err(HotkeyParseError::Empty);
        }
        let mut mods = Modifiers::default();
        let mut key: Option<Key> = None;
        for raw in s.split('+') {
            let token = raw.trim();
            if token.is_empty() {
                return Err(HotkeyParseError::EmptyToken);
            }
            if key.is_some() {
                return Err(HotkeyParseError::MisplacedKey);
            }
            if !apply_modifier(&mut mods, token)? {
                key = Some(parse_key(token)?);
            }
        }
        let key = key.ok_or(HotkeyParseError::MissingKey)?;
        if !(mods.ctrl || mods.alt || mods.win || key.allowed_bare()) {
            return Err(HotkeyParseError::NeedModifier);
        }
        Ok(Hotkey {
            modifiers: mods,
            key,
        })
    }
}

impl Hotkey {
    /// 解析热键字符串。
    ///
    /// # 参数
    /// - `s`：如 `"Ctrl+Alt+S"`，不区分大小写。
    ///
    /// # 返回
    /// 解析结果或 [`HotkeyParseError`]。
    ///
    /// ```rust
    /// use snow_shell_core::hotkey::Hotkey;
    /// let hk = Hotkey::parse("alt+ctrl+s").unwrap();
    /// assert_eq!(hk.to_string(), "Ctrl+Alt+S");
    /// ```
    pub fn parse(s: &str) -> Result<Self, HotkeyParseError> {
        s.parse()
    }
}

impl fmt::Display for Hotkey {
    /// 规范输出：`Ctrl+Alt+Shift+Win+Key`。
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let m = self.modifiers;
        for (on, name) in [
            (m.ctrl, "Ctrl"),
            (m.alt, "Alt"),
            (m.shift, "Shift"),
            (m.win, "Win"),
        ] {
            if on {
                write!(f, "{name}+")?;
            }
        }
        write!(f, "{}", self.key.label)
    }
}

/// 热键的边沿：按下或松开。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HotkeyEdge {
    /// 按下。
    Pressed,
    /// 松开（Windows 由 global-hotkey 轮询主键状态产生，最坏延迟约一个轮询周期）。
    Released,
}

/// 一条热键绑定：按下时派发 `command`；若设置了 `on_release`，松开时再派发它（按住说话用）。
///
/// `A` 是不透明的动作负载（UI 进程用 `AppCommand`，托盘宿主用动作 id 字符串）。
#[derive(Debug, Clone, PartialEq)]
pub struct HotkeyBinding<A> {
    /// 热键。
    pub hotkey: Hotkey,
    /// 按下时触发的动作。
    pub command: A,
    /// 松开时触发的动作；`None` 表示松开事件被忽略（现有热键的行为）。
    pub on_release: Option<A>,
}

impl<A> HotkeyBinding<A> {
    /// 只在按下时触发动作的普通绑定。
    ///
    /// # 参数
    /// - `hotkey`：热键。
    /// - `command`：按下时派发的动作。
    ///
    /// ```rust
    /// use snow_shell_core::hotkey::{Hotkey, HotkeyBinding};
    /// let b = HotkeyBinding::new(Hotkey::parse("F9").unwrap(), "recapture");
    /// assert!(b.on_release.is_none());
    /// ```
    pub fn new(hotkey: Hotkey, command: A) -> Self {
        Self {
            hotkey,
            command,
            on_release: None,
        }
    }

    /// 追加松开时派发的动作（按住说话：按下开始、松开结束）。
    ///
    /// # 参数
    /// - `command`：松开时派发的动作。
    ///
    /// ```rust
    /// use snow_shell_core::hotkey::{Hotkey, HotkeyBinding};
    /// let b = HotkeyBinding::new(Hotkey::parse("F9").unwrap(), "start").with_release("stop");
    /// assert_eq!(b.on_release, Some("stop"));
    /// ```
    pub fn with_release(mut self, command: A) -> Self {
        self.on_release = Some(command);
        self
    }

    /// 取指定边沿应派发的动作；松开且未配置 `on_release` 时为 `None`。
    ///
    /// # 参数
    /// - `edge`：按下或松开。
    pub fn command_for(&self, edge: HotkeyEdge) -> Option<&A> {
        match edge {
            HotkeyEdge::Pressed => Some(&self.command),
            HotkeyEdge::Released => self.on_release.as_ref(),
        }
    }
}

/// 已注册热键的句柄，用于注销。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct HotkeyHandle(pub u32);

/// 热键登记表（纯逻辑）：负责句柄分配与进程内冲突判定。
#[derive(Debug)]
pub(crate) struct HotkeyTable<A> {
    /// 句柄到绑定。
    entries: HashMap<HotkeyHandle, HotkeyBinding<A>>,
    /// 下一个句柄编号。
    next: u32,
}

impl<A> Default for HotkeyTable<A> {
    /// 空登记表（不要求 `A: Default`）。
    fn default() -> Self {
        Self {
            entries: HashMap::new(),
            next: 0,
        }
    }
}

impl<A> HotkeyTable<A> {
    /// 登记绑定；同一热键已登记则冲突。
    pub(crate) fn insert(&mut self, binding: HotkeyBinding<A>) -> Result<HotkeyHandle, ShellError> {
        if self.entries.values().any(|b| b.hotkey == binding.hotkey) {
            return Err(ShellError::HotkeyConflict(format!(
                "{} 已被本程序其他命令占用",
                binding.hotkey
            )));
        }
        self.next += 1;
        let handle = HotkeyHandle(self.next);
        self.entries.insert(handle, binding);
        Ok(handle)
    }

    /// 移除登记。
    pub(crate) fn remove(&mut self, handle: HotkeyHandle) -> Option<HotkeyBinding<A>> {
        self.entries.remove(&handle)
    }

    /// 取绑定。
    pub(crate) fn get(&self, handle: HotkeyHandle) -> Option<&HotkeyBinding<A>> {
        self.entries.get(&handle)
    }

    /// 列出全部（按句柄升序）。
    pub(crate) fn list(&self) -> Vec<(HotkeyHandle, Hotkey)> {
        let mut v: Vec<_> = self
            .entries
            .iter()
            .map(|(h, b)| (*h, b.hotkey.clone()))
            .collect();
        v.sort_by_key(|(h, _)| h.0);
        v
    }
}

#[cfg(windows)]
mod backend {
    //! Windows 后端：专用线程持有 `GlobalHotKeyManager` 并泵消息。

    use super::*;
    use crate::dispatch::ActionOrigin;
    use crate::native::{self, LoopWaker};
    use crate::service_thread::ServiceThread;
    use global_hotkey::hotkey::{Code, HotKey, Modifiers as GhMods};
    use global_hotkey::{GlobalHotKeyEvent, GlobalHotKeyManager, HotKeyState};
    use std::sync::atomic::AtomicBool;
    use std::sync::mpsc::{Receiver, Sender, channel};

    /// 进程内只允许一个热键服务（global-hotkey 的事件通道是进程级单例）。
    static RUNNING: AtomicBool = AtomicBool::new(false);

    /// 发给后台线程的控制请求。
    enum Ctl<A> {
        /// 注册热键。
        Register(
            Box<HotkeyBinding<A>>,
            Sender<Result<HotkeyHandle, ShellError>>,
        ),
        /// 注销热键。
        Unregister(HotkeyHandle, Sender<Result<(), ShellError>>),
        /// 列出已注册热键。
        List(Sender<Vec<(HotkeyHandle, Hotkey)>>),
        /// 停止线程。
        Shutdown,
    }

    /// 后端句柄。
    pub(super) struct Inner<A> {
        /// 控制通道。
        ctl: Sender<Ctl<A>>,
        /// 后台服务线程；丢弃时通知退出、join 并释放单例标记。
        thread: ServiceThread<LoopWaker>,
    }

    /// 转成 global-hotkey 的热键类型。
    pub(super) fn to_gh(hotkey: &Hotkey) -> Result<HotKey, ShellError> {
        let mut mods = GhMods::empty();
        let m = hotkey.modifiers;
        for (on, flag) in [
            (m.ctrl, GhMods::CONTROL),
            (m.alt, GhMods::ALT),
            (m.shift, GhMods::SHIFT),
            (m.win, GhMods::SUPER),
        ] {
            if on {
                mods |= flag;
            }
        }
        let code = Code::from_str(hotkey.key.code()).map_err(|_| {
            ShellError::InvalidHotkey(format!("后端不识别键 {}", hotkey.key.code()))
        })?;
        Ok(HotKey::new(Some(mods), code))
    }

    /// 映射 global-hotkey 的注册错误。
    fn map_register_err(hotkey: &Hotkey, err: global_hotkey::Error) -> ShellError {
        match err {
            global_hotkey::Error::AlreadyRegistered(_) => {
                ShellError::HotkeyConflict(format!("{hotkey} 已被系统或其他程序占用"))
            }
            other => ShellError::Platform(format!("注册热键 {hotkey} 失败: {other}")),
        }
    }

    /// 后台线程状态。
    struct Worker<A> {
        /// 系统热键管理器。
        manager: GlobalHotKeyManager,
        /// 登记表。
        table: HotkeyTable<A>,
        /// 平台热键 id 到句柄。
        by_platform_id: HashMap<u32, HotkeyHandle>,
        /// 动作出口。
        dispatcher: Dispatcher<A>,
    }

    impl<A: Clone> Worker<A> {
        /// 处理注册请求。
        fn register(&mut self, binding: HotkeyBinding<A>) -> Result<HotkeyHandle, ShellError> {
            let gh = to_gh(&binding.hotkey)?;
            let hotkey = binding.hotkey.clone();
            let handle = self.table.insert(binding)?;
            if let Err(err) = self.manager.register(gh) {
                self.table.remove(handle);
                return Err(map_register_err(&hotkey, err));
            }
            self.by_platform_id.insert(gh.id(), handle);
            Ok(handle)
        }

        /// 处理注销请求。
        fn unregister(&mut self, handle: HotkeyHandle) -> Result<(), ShellError> {
            let binding = self
                .table
                .remove(handle)
                .ok_or_else(|| ShellError::InvalidArgument(format!("未知热键句柄 {handle:?}")))?;
            let gh = to_gh(&binding.hotkey)?;
            self.by_platform_id.remove(&gh.id());
            self.manager
                .unregister(gh)
                .map_err(|e| ShellError::Platform(format!("注销热键 {} 失败: {e}", binding.hotkey)))
        }

        /// 处理一轮控制请求与热键事件；返回 `false` 表示应退出。
        fn tick(&mut self, ctl: &Receiver<Ctl<A>>) -> bool {
            while let Ok(req) = ctl.try_recv() {
                match req {
                    Ctl::Register(b, reply) => {
                        let _ = reply.send(self.register(*b));
                    }
                    Ctl::Unregister(h, reply) => {
                        let _ = reply.send(self.unregister(h));
                    }
                    Ctl::List(reply) => {
                        let _ = reply.send(self.table.list());
                    }
                    Ctl::Shutdown => return false,
                }
            }
            while let Ok(ev) = GlobalHotKeyEvent::receiver().try_recv() {
                let edge = match ev.state() {
                    HotKeyState::Pressed => HotkeyEdge::Pressed,
                    HotKeyState::Released => HotkeyEdge::Released,
                };
                let binding = self
                    .by_platform_id
                    .get(&ev.id())
                    .and_then(|h| self.table.get(*h));
                if let Some(b) = binding
                    && let Some(command) = b.command_for(edge)
                {
                    tracing::debug!(hotkey = %b.hotkey, ?edge, "全局热键触发");
                    self.dispatcher.send(ActionOrigin::Hotkey, command.clone());
                }
            }
            true
        }
    }

    impl<A: Clone + Send + 'static> Inner<A> {
        /// 启动后台线程并等待其就绪。
        pub(super) fn start(dispatcher: Dispatcher<A>) -> Result<Self, ShellError> {
            let (ctl_tx, ctl_rx) = channel::<Ctl<A>>();
            let notify_tx = ctl_tx.clone();
            let thread = ServiceThread::spawn(
                &RUNNING,
                "HotkeyService",
                "shell-hotkey",
                "创建热键线程失败",
                move || {
                    let manager = GlobalHotKeyManager::new()
                        .map_err(|e| ShellError::Platform(format!("创建热键管理器失败: {e}")))?;
                    let waker = native::current_thread_waker();
                    let worker = Worker {
                        manager,
                        table: HotkeyTable::default(),
                        by_platform_id: HashMap::new(),
                        dispatcher,
                    };
                    Ok((worker, waker))
                },
                move |mut worker: Worker<A>| native::run_message_loop(|| worker.tick(&ctl_rx)),
                move |waker: &LoopWaker| {
                    let _ = notify_tx.send(Ctl::Shutdown);
                    waker.wake();
                },
            )?;
            Ok(Self {
                ctl: ctl_tx,
                thread,
            })
        }

        /// 发送请求并等待应答。
        fn call<T>(&self, make: impl FnOnce(Sender<T>) -> Ctl<A>) -> Result<T, ShellError> {
            let (tx, rx) = channel();
            self.ctl
                .send(make(tx))
                .map_err(|_| ShellError::ServiceClosed)?;
            self.thread.ready().wake();
            rx.recv().map_err(|_| ShellError::ServiceClosed)
        }

        /// 注册。
        pub(super) fn register(&self, b: HotkeyBinding<A>) -> Result<HotkeyHandle, ShellError> {
            self.call(|tx| Ctl::Register(Box::new(b), tx))?
        }

        /// 注销。
        pub(super) fn unregister(&self, h: HotkeyHandle) -> Result<(), ShellError> {
            self.call(|tx| Ctl::Unregister(h, tx))?
        }

        /// 列表。
        pub(super) fn list(&self) -> Result<Vec<(HotkeyHandle, Hotkey)>, ShellError> {
            self.call(Ctl::List)
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        /// 所有可解析的主键都能被后端识别（覆盖字母/数字/F 键/命名键）。
        #[test]
        fn every_key_maps_to_backend_code() {
            let mut names: Vec<String> = ('A'..='Z').map(|c| c.to_string()).collect();
            names.extend((0..=9).map(|d| d.to_string()));
            names.extend((1..=24).map(|n| format!("F{n}")));
            names.extend(NAMED_KEYS.iter().map(|(a, _, _)| a[0].to_string()));
            for n in names {
                let hk: Hotkey = format!("Ctrl+{n}").parse().unwrap();
                assert!(to_gh(&hk).is_ok(), "后端不识别: {n}");
            }
        }

        /// 真实服务：注册 / 等价写法冲突 / 注销 / 重复启动被拒 / drop 后可重启。
        /// 用 Ctrl+Alt+Shift+Win+F24 这种几乎不会被占用的组合，不触发也不模拟任何输入。
        #[test]
        fn service_lifecycle_on_real_os() {
            let mk = |s: &str| HotkeyBinding::new(Hotkey::parse(s).unwrap(), "noop".to_string());
            let start = || HotkeyService::<String>::start(Dispatcher::from_fn(|_, _| {}));
            {
                let svc = start().unwrap();
                assert!(matches!(start(), Err(ShellError::AlreadyRunning(_))));
                let h = svc.register(mk("Ctrl+Alt+Shift+Win+F24")).unwrap();
                assert!(matches!(
                    svc.register(mk("win+shift+alt+ctrl+f24")),
                    Err(ShellError::HotkeyConflict(_))
                ));
                assert_eq!(svc.registered().unwrap().len(), 1);
                svc.unregister(h).unwrap();
                assert!(svc.unregister(h).is_err());
                assert!(svc.registered().unwrap().is_empty());
            }
            // drop 后单例标记释放，可再次启动
            assert!(start().is_ok());
        }

        /// 修饰键映射正确。
        #[test]
        fn modifiers_map() {
            let gh = to_gh(&Hotkey::parse("Ctrl+Alt+Shift+Win+S").unwrap()).unwrap();
            assert!(
                gh.mods
                    .contains(GhMods::CONTROL | GhMods::ALT | GhMods::SHIFT | GhMods::SUPER)
            );
            assert_eq!(gh.key, Code::KeyS);
        }
    }
}

#[cfg(not(windows))]
mod backend {
    //! 非 Windows 降级后端：所有操作返回 `Unsupported`。

    use super::*;
    use crate::error::{CAPABILITY_GLOBAL_HOTKEY, REASON_NOT_IMPLEMENTED};
    use std::marker::PhantomData;

    /// 降级后端句柄（无状态；`start` 恒失败，不会被构造）。
    pub(super) struct Inner<A>(PhantomData<A>);

    /// 构造统一的“未实现”错误。
    fn unsupported() -> ShellError {
        ShellError::Unsupported {
            capability: CAPABILITY_GLOBAL_HOTKEY,
            reason: REASON_NOT_IMPLEMENTED,
        }
    }

    impl<A> Inner<A> {
        /// 启动：不支持。
        pub(super) fn start(_d: Dispatcher<A>) -> Result<Self, ShellError> {
            Err(unsupported())
        }

        /// 注册：不支持。
        pub(super) fn register(&self, _b: HotkeyBinding<A>) -> Result<HotkeyHandle, ShellError> {
            Err(unsupported())
        }

        /// 注销：不支持。
        pub(super) fn unregister(&self, _h: HotkeyHandle) -> Result<(), ShellError> {
            Err(unsupported())
        }

        /// 列表：不支持。
        pub(super) fn list(&self) -> Result<Vec<(HotkeyHandle, Hotkey)>, ShellError> {
            Err(unsupported())
        }
    }
}

/// 全局热键服务：后台线程注册系统热键，触发后经 [`Dispatcher`] 派发绑定的动作
/// （来源 [`ActionOrigin::Hotkey`](crate::dispatch::ActionOrigin::Hotkey)）。丢弃即停止并注销全部热键。
///
/// 进程内只能启动一个实例；可在任意线程调用注册/注销。
pub struct HotkeyService<A> {
    /// 平台后端。
    inner: backend::Inner<A>,
}

impl<A: Clone + Send + 'static> HotkeyService<A> {
    /// 启动服务。
    ///
    /// # 参数
    /// - `dispatcher`：动作出口；热键触发时在服务线程上同步调用，应只做轻量转发。
    ///
    /// # 返回
    /// 服务句柄；重复启动返回 `AlreadyRunning`，非 Windows 平台返回 `Unsupported`。
    ///
    /// ```no_run
    /// use snow_shell_core::dispatch::Dispatcher;
    /// use snow_shell_core::hotkey::{Hotkey, HotkeyBinding, HotkeyService};
    /// let svc = HotkeyService::start(Dispatcher::from_fn(|_origin, action: String| {
    ///     println!("热键触发: {action}");
    /// }))
    /// .unwrap();
    /// svc.register(HotkeyBinding::new(
    ///     Hotkey::parse("Ctrl+Alt+S").unwrap(),
    ///     "capture".to_string(),
    /// ))
    /// .unwrap();
    /// ```
    pub fn start(dispatcher: Dispatcher<A>) -> Result<Self, ShellError> {
        Ok(Self {
            inner: backend::Inner::start(dispatcher)?,
        })
    }

    /// 注册热键。
    ///
    /// # 参数
    /// - `binding`：热键与触发动作。
    ///
    /// # 返回
    /// 句柄；同一热键重复注册或被其他程序占用返回 `HotkeyConflict`。
    pub fn register(&self, binding: HotkeyBinding<A>) -> Result<HotkeyHandle, ShellError> {
        self.inner.register(binding)
    }

    /// 注销热键。
    ///
    /// # 参数
    /// - `handle`：注册时返回的句柄。
    pub fn unregister(&self, handle: HotkeyHandle) -> Result<(), ShellError> {
        self.inner.unregister(handle)
    }

    /// 当前已注册的热键列表（按句柄升序）。
    pub fn registered(&self) -> Result<Vec<(HotkeyHandle, Hotkey)>, ShellError> {
        self.inner.list()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 常规写法与大小写、顺序无关，输出规范形式。
    #[test]
    fn parse_and_canonical_display() {
        for (input, want) in [
            ("Ctrl+Alt+S", "Ctrl+Alt+S"),
            ("alt + CTRL + s", "Ctrl+Alt+S"),
            ("shift+f1", "Shift+F1"),
            ("Win+Shift+Digit1", "Shift+Win+1"),
            ("Ctrl+KeyA", "Ctrl+A"),
            ("Control+Return", "Ctrl+Enter"),
            ("Ctrl+PgUp", "Ctrl+PageUp"),
            ("PrintScreen", "PrintScreen"),
            ("F12", "F12"),
            ("Ctrl+/", "Ctrl+/"),
        ] {
            assert_eq!(Hotkey::parse(input).unwrap().to_string(), want, "{input}");
        }
    }

    /// 规范输出可再次解析且相等（往返）。
    #[test]
    fn display_round_trip() {
        let hk = Hotkey::parse("Ctrl+Shift+Alt+Win+F24").unwrap();
        assert_eq!(Hotkey::parse(&hk.to_string()).unwrap(), hk);
    }

    /// 各类非法输入给出对应错误。
    #[test]
    fn parse_errors() {
        assert_eq!(Hotkey::parse("  "), Err(HotkeyParseError::Empty));
        assert_eq!(Hotkey::parse("Ctrl++S"), Err(HotkeyParseError::EmptyToken));
        assert_eq!(
            Hotkey::parse("Ctrl+Shift"),
            Err(HotkeyParseError::MissingKey)
        );
        assert_eq!(
            Hotkey::parse("Ctrl+S+A"),
            Err(HotkeyParseError::MisplacedKey)
        );
        assert_eq!(Hotkey::parse("S+Ctrl"), Err(HotkeyParseError::MisplacedKey));
        assert!(matches!(
            Hotkey::parse("Ctrl+Ctrl+S"),
            Err(HotkeyParseError::DuplicateModifier(_))
        ));
        assert!(matches!(
            Hotkey::parse("Ctrl+Banana"),
            Err(HotkeyParseError::UnknownKey(_))
        ));
        assert!(matches!(
            Hotkey::parse("Ctrl+F25"),
            Err(HotkeyParseError::UnknownKey(_))
        ));
    }

    /// 字母/数字等必须带 Ctrl/Alt/Win，Shift 不够；功能键可单独使用。
    #[test]
    fn modifier_requirements() {
        assert_eq!(Hotkey::parse("S"), Err(HotkeyParseError::NeedModifier));
        assert_eq!(
            Hotkey::parse("Shift+S"),
            Err(HotkeyParseError::NeedModifier)
        );
        assert_eq!(Hotkey::parse("Space"), Err(HotkeyParseError::NeedModifier));
        assert!(Hotkey::parse("Alt+S").is_ok());
        assert!(Hotkey::parse("F5").is_ok());
        assert!(Hotkey::parse("Insert").is_ok());
    }

    /// 字母 / 数字主键的 code 与显示名（大小写不敏感）。
    #[test]
    fn letter_and_digit_keys() {
        let k = letter_key('s');
        assert_eq!((k.code(), k.label.as_str()), ("KeyS", "S"));
        let k = digit_key('7');
        assert_eq!((k.code(), k.label.as_str()), ("Digit7", "7"));
        assert_eq!(Hotkey::parse("ctrl+digit7").unwrap().to_string(), "Ctrl+7");
    }

    /// 错误可转换为适配层错误。
    #[test]
    fn parse_error_converts() {
        let err: ShellError = Hotkey::parse("x").unwrap_err().into();
        assert!(matches!(err, ShellError::InvalidHotkey(_)));
    }

    /// 修饰键位掩码与 Win32 `MOD_*` 一致。
    #[test]
    fn modifier_bits_match_win32() {
        let bits = |s: &str| Hotkey::parse(s).unwrap().modifiers.win_bits();
        assert_eq!(bits("Alt+S"), 0x1);
        assert_eq!(bits("Ctrl+S"), 0x2);
        assert_eq!(bits("Shift+F1"), 0x4);
        assert_eq!(bits("Win+S"), 0x8);
        assert_eq!(bits("Ctrl+Alt+Shift+Win+S"), 0xF);
        assert_eq!(bits("F1"), 0);
    }

    /// 虚拟键码：字母、数字、功能键、命名键都有值，抽样与 Win32 常量一致。
    #[test]
    fn virtual_key_codes() {
        let vk = |s: &str| Hotkey::parse(s).unwrap().key.win_vk();
        assert_eq!(vk("Ctrl+A"), Some(0x41));
        assert_eq!(vk("Ctrl+Z"), Some(0x5A));
        assert_eq!(vk("Ctrl+0"), Some(0x30));
        assert_eq!(vk("Ctrl+9"), Some(0x39));
        assert_eq!(vk("F1"), Some(0x70));
        assert_eq!(vk("F24"), Some(0x87));
        assert_eq!(vk("PrintScreen"), Some(0x2C));
        assert_eq!(vk("Ctrl+Space"), Some(0x20));
        assert_eq!(vk("Ctrl+/"), Some(0xBF));
        assert_eq!(vk("Ctrl+Left"), Some(0x25));
    }

    /// 凡是 `parse` 能产出的主键，都能映射到虚拟键码；命名键表与别名表一一对应。
    #[test]
    fn every_parsable_key_has_virtual_key() {
        let mut names: Vec<String> = ('A'..='Z').map(|c| c.to_string()).collect();
        names.extend((0..=9).map(|d| d.to_string()));
        names.extend((1..=24).map(|n| format!("F{n}")));
        names.extend(
            NAMED_KEYS
                .iter()
                .flat_map(|(a, _, _)| a.iter().map(|s| s.to_string())),
        );
        for n in names {
            let hk: Hotkey = format!("Ctrl+{n}").parse().unwrap();
            assert!(hk.key.win_vk().is_some(), "无虚拟键码: {n}");
        }
        assert_eq!(NAMED_KEY_VK.len(), NAMED_KEYS.len());
        for (_, code, _) in NAMED_KEYS {
            assert!(NAMED_KEY_VK.iter().any(|(c, _)| c == code), "缺 {code}");
        }
    }

    /// 边沿到动作：普通绑定只响应按下（松开被忽略，现有热键行为不变）；带 on_release 的松开才派发。
    #[test]
    fn edge_selects_command() {
        let hotkey = Hotkey::parse("F9").unwrap();
        let plain = HotkeyBinding::new(hotkey.clone(), "start");
        assert_eq!(plain.command_for(HotkeyEdge::Pressed), Some(&"start"));
        assert_eq!(plain.command_for(HotkeyEdge::Released), None);
        let hold = plain.with_release("stop");
        assert_eq!(hold.command_for(HotkeyEdge::Pressed), Some(&"start"));
        assert_eq!(hold.command_for(HotkeyEdge::Released), Some(&"stop"));
        // 登记表照常保存松开动作
        let mut table = HotkeyTable::default();
        let handle = table.insert(hold.clone()).unwrap();
        assert_eq!(table.get(handle), Some(&hold));
    }

    /// 登记表：句柄自增、同热键冲突、注销后可重新登记。
    #[test]
    fn table_conflict_and_release() {
        let mut t = HotkeyTable::default();
        let mk = |s: &str, c: &'static str| HotkeyBinding::new(Hotkey::parse(s).unwrap(), c);
        let h1 = t.insert(mk("Ctrl+Alt+S", "cancel")).unwrap();
        let h2 = t.insert(mk("Ctrl+Alt+D", "undo")).unwrap();
        assert_ne!(h1, h2);
        // 等价写法也要判为冲突
        assert!(matches!(
            t.insert(mk("alt+ctrl+s", "redo")),
            Err(ShellError::HotkeyConflict(_))
        ));
        assert_eq!(t.get(h2).unwrap().command, "undo");
        assert_eq!(t.list().len(), 2);
        assert!(t.remove(h1).is_some());
        assert!(t.remove(h1).is_none());
        assert!(t.insert(mk("Ctrl+Alt+S", "redo")).is_ok());
    }

    /// 非 Windows 平台启动直接降级为错误。
    #[cfg(not(windows))]
    #[test]
    fn start_is_unsupported_off_windows() {
        let d = Dispatcher::<String>::from_fn(|_, _| {});
        assert!(matches!(
            HotkeyService::start(d),
            Err(ShellError::Unsupported { .. })
        ));
    }
}
