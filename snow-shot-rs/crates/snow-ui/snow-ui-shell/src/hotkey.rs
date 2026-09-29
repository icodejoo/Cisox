//! 全局热键：与 gpui 无关的热键模型、字符串解析与注册服务。
//!
//! 热键触发后以 [`CommandSource::Hotkey`](snow_app_core::command::CommandSource::Hotkey)
//! 把绑定的 [`AppCommand`] 送入命令总线（见 [`crate::dispatch::Dispatcher`]）。

use crate::error::ShellError;
use snow_app_core::command::AppCommand;
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

/// 解析主键片段；不是主键时返回 `UnknownKey`。
fn parse_key(token: &str) -> Result<Key, HotkeyParseError> {
    let lower = token.to_ascii_lowercase();
    let unknown = || HotkeyParseError::UnknownKey(token.to_string());
    let single = |s: &str| {
        let mut it = s.chars();
        match (it.next(), it.next()) {
            (Some(c), None) => Some(c),
            _ => None,
        }
    };
    // 单字符：字母 / 数字
    if let Some(c) = single(&lower) {
        if c.is_ascii_lowercase() {
            let up = c.to_ascii_uppercase();
            return Ok(Key {
                code: format!("Key{up}"),
                label: up.to_string(),
            });
        }
        if c.is_ascii_digit() {
            return Ok(Key {
                code: format!("Digit{c}"),
                label: c.to_string(),
            });
        }
    }
    // 显式 code 名：KeyS / Digit1
    if let Some(rest) = lower.strip_prefix("key")
        && let Some(c) = single(rest).filter(char::is_ascii_lowercase)
    {
        let up = c.to_ascii_uppercase();
        return Ok(Key {
            code: format!("Key{up}"),
            label: up.to_string(),
        });
    }
    if let Some(rest) = lower.strip_prefix("digit")
        && let Some(c) = single(rest).filter(char::is_ascii_digit)
    {
        return Ok(Key {
            code: format!("Digit{c}"),
            label: c.to_string(),
        });
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
    /// use snow_ui_shell::hotkey::Hotkey;
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

/// 一条热键绑定：触发时派发 `command`。
#[derive(Debug, Clone, PartialEq)]
pub struct HotkeyBinding {
    /// 热键。
    pub hotkey: Hotkey,
    /// 触发的命令。
    pub command: AppCommand,
}

/// 已注册热键的句柄，用于注销。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct HotkeyHandle(pub u32);

/// 热键登记表（纯逻辑）：负责句柄分配与进程内冲突判定。
#[derive(Debug, Default)]
pub(crate) struct HotkeyTable {
    /// 句柄到绑定。
    entries: HashMap<HotkeyHandle, HotkeyBinding>,
    /// 下一个句柄编号。
    next: u32,
}

impl HotkeyTable {
    /// 登记绑定；同一热键已登记则冲突。
    pub(crate) fn insert(&mut self, binding: HotkeyBinding) -> Result<HotkeyHandle, ShellError> {
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
    pub(crate) fn remove(&mut self, handle: HotkeyHandle) -> Option<HotkeyBinding> {
        self.entries.remove(&handle)
    }

    /// 取绑定。
    pub(crate) fn get(&self, handle: HotkeyHandle) -> Option<&HotkeyBinding> {
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
    use crate::dispatch::Dispatcher;
    use crate::native::{self, LoopWaker};
    use global_hotkey::hotkey::{Code, HotKey, Modifiers as GhMods};
    use global_hotkey::{GlobalHotKeyEvent, GlobalHotKeyManager, HotKeyState};
    use snow_app_core::command::CommandSource;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::mpsc::{Receiver, Sender, channel};
    use std::thread::JoinHandle;

    /// 进程内只允许一个热键服务（global-hotkey 的事件通道是进程级单例）。
    static RUNNING: AtomicBool = AtomicBool::new(false);

    /// 发给后台线程的控制请求。
    enum Ctl {
        /// 注册热键。
        Register(HotkeyBinding, Sender<Result<HotkeyHandle, ShellError>>),
        /// 注销热键。
        Unregister(HotkeyHandle, Sender<Result<(), ShellError>>),
        /// 列出已注册热键。
        List(Sender<Vec<(HotkeyHandle, Hotkey)>>),
        /// 停止线程。
        Shutdown,
    }

    /// 后端句柄。
    pub(super) struct Inner {
        /// 控制通道。
        ctl: Sender<Ctl>,
        /// 消息循环唤醒器。
        waker: LoopWaker,
        /// 后台线程。
        join: Option<JoinHandle<()>>,
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
    struct Worker {
        /// 系统热键管理器。
        manager: GlobalHotKeyManager,
        /// 登记表。
        table: HotkeyTable,
        /// 平台热键 id 到句柄。
        by_platform_id: HashMap<u32, HotkeyHandle>,
        /// 命令出口。
        dispatcher: Dispatcher,
    }

    impl Worker {
        /// 处理注册请求。
        fn register(&mut self, binding: HotkeyBinding) -> Result<HotkeyHandle, ShellError> {
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
        fn tick(&mut self, ctl: &Receiver<Ctl>) -> bool {
            while let Ok(req) = ctl.try_recv() {
                match req {
                    Ctl::Register(b, reply) => {
                        let _ = reply.send(self.register(b));
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
                if ev.state() != HotKeyState::Pressed {
                    continue;
                }
                let binding = self
                    .by_platform_id
                    .get(&ev.id())
                    .and_then(|h| self.table.get(*h));
                if let Some(b) = binding {
                    tracing::debug!(hotkey = %b.hotkey, "全局热键触发");
                    self.dispatcher
                        .send(CommandSource::Hotkey, b.command.clone());
                }
            }
            true
        }
    }

    impl Inner {
        /// 启动后台线程并等待其就绪。
        pub(super) fn start(dispatcher: Dispatcher) -> Result<Self, ShellError> {
            if RUNNING.swap(true, Ordering::SeqCst) {
                return Err(ShellError::AlreadyRunning("HotkeyService"));
            }
            let (ctl_tx, ctl_rx) = channel::<Ctl>();
            let (ready_tx, ready_rx) = channel::<Result<LoopWaker, ShellError>>();
            let spawned = std::thread::Builder::new()
                .name("shell-hotkey".into())
                .spawn(move || {
                    let manager = match GlobalHotKeyManager::new() {
                        Ok(m) => m,
                        Err(e) => {
                            let _ = ready_tx.send(Err(ShellError::Platform(format!(
                                "创建热键管理器失败: {e}"
                            ))));
                            return;
                        }
                    };
                    let waker = native::current_thread_waker();
                    let mut worker = Worker {
                        manager,
                        table: HotkeyTable::default(),
                        by_platform_id: HashMap::new(),
                        dispatcher,
                    };
                    let _ = ready_tx.send(Ok(waker));
                    native::run_message_loop(|| worker.tick(&ctl_rx));
                });
            let join = match spawned {
                Ok(j) => j,
                Err(e) => {
                    RUNNING.store(false, Ordering::SeqCst);
                    return Err(ShellError::Platform(format!("创建热键线程失败: {e}")));
                }
            };
            match ready_rx.recv() {
                Ok(Ok(waker)) => Ok(Self {
                    ctl: ctl_tx,
                    waker,
                    join: Some(join),
                }),
                Ok(Err(e)) => {
                    let _ = join.join();
                    RUNNING.store(false, Ordering::SeqCst);
                    Err(e)
                }
                Err(_) => {
                    RUNNING.store(false, Ordering::SeqCst);
                    Err(ShellError::ServiceClosed)
                }
            }
        }

        /// 发送请求并等待应答。
        fn call<T>(&self, make: impl FnOnce(Sender<T>) -> Ctl) -> Result<T, ShellError> {
            let (tx, rx) = channel();
            self.ctl
                .send(make(tx))
                .map_err(|_| ShellError::ServiceClosed)?;
            self.waker.wake();
            rx.recv().map_err(|_| ShellError::ServiceClosed)
        }

        /// 注册。
        pub(super) fn register(&self, b: HotkeyBinding) -> Result<HotkeyHandle, ShellError> {
            self.call(|tx| Ctl::Register(b, tx))?
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

    impl Drop for Inner {
        /// 停止后台线程并释放单例标记。
        fn drop(&mut self) {
            let _ = self.ctl.send(Ctl::Shutdown);
            self.waker.wake();
            if let Some(j) = self.join.take() {
                let _ = j.join();
            }
            RUNNING.store(false, Ordering::SeqCst);
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
            use snow_capability::{CapabilityRegistry, Platform};
            let caps = CapabilityRegistry::for_platform(Platform::Windows);
            let mk = |s: &str| HotkeyBinding {
                hotkey: Hotkey::parse(s).unwrap(),
                command: AppCommand::Cancel,
            };
            {
                let svc =
                    crate::hotkey::HotkeyService::start(&caps, Dispatcher::from_fn(|_, _| {}))
                        .unwrap();
                assert!(matches!(
                    crate::hotkey::HotkeyService::start(&caps, Dispatcher::from_fn(|_, _| {})),
                    Err(ShellError::AlreadyRunning(_))
                ));
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
            assert!(
                crate::hotkey::HotkeyService::start(&caps, Dispatcher::from_fn(|_, _| {})).is_ok()
            );
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
    use crate::dispatch::Dispatcher;
    use snow_capability::{Capability, REASON_NOT_IMPLEMENTED};

    /// 降级后端句柄（无状态）。
    pub(super) struct Inner;

    /// 构造统一的“未实现”错误。
    fn unsupported() -> ShellError {
        ShellError::Unsupported {
            capability: Capability::GlobalHotkey,
            reason: REASON_NOT_IMPLEMENTED,
        }
    }

    impl Inner {
        /// 启动：不支持。
        pub(super) fn start(_d: Dispatcher) -> Result<Self, ShellError> {
            Err(unsupported())
        }

        /// 注册：不支持。
        pub(super) fn register(&self, _b: HotkeyBinding) -> Result<HotkeyHandle, ShellError> {
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

/// 全局热键服务：后台线程注册系统热键，触发后经 [`Dispatcher`](crate::dispatch::Dispatcher)
/// 派发 `AppCommand`（来源 `Hotkey`）。丢弃即停止并注销全部热键。
///
/// 进程内只能启动一个实例；可在任意线程调用注册/注销。
pub struct HotkeyService {
    /// 平台后端。
    inner: backend::Inner,
}

impl HotkeyService {
    /// 启动服务。
    ///
    /// # 参数
    /// - `caps`：能力表；`GlobalHotkey` 不可用时直接返回 `Unsupported`（降级，不 panic）。
    /// - `dispatcher`：命令出口，通常为 `Dispatcher::from_bus(bus)`。
    ///
    /// # 返回
    /// 服务句柄；重复启动返回 `AlreadyRunning`。
    ///
    /// ```no_run
    /// use snow_app_core::bus::CommandBus;
    /// use snow_app_core::command::AppCommand;
    /// use snow_capability::CapabilityRegistry;
    /// use snow_ui_shell::dispatch::Dispatcher;
    /// use snow_ui_shell::hotkey::{Hotkey, HotkeyBinding, HotkeyService};
    /// let caps = CapabilityRegistry::for_current_platform();
    /// let svc = HotkeyService::start(&caps, Dispatcher::from_bus(CommandBus::new())).unwrap();
    /// svc.register(HotkeyBinding {
    ///     hotkey: Hotkey::parse("Ctrl+Alt+S").unwrap(),
    ///     command: AppCommand::Cancel,
    /// }).unwrap();
    /// ```
    pub fn start(
        caps: &snow_capability::CapabilityRegistry,
        dispatcher: crate::dispatch::Dispatcher,
    ) -> Result<Self, ShellError> {
        crate::error::require_capability(caps, snow_capability::Capability::GlobalHotkey)?;
        Ok(Self {
            inner: backend::Inner::start(dispatcher)?,
        })
    }

    /// 注册热键。
    ///
    /// # 参数
    /// - `binding`：热键与触发命令。
    ///
    /// # 返回
    /// 句柄；同一热键重复注册或被其他程序占用返回 `HotkeyConflict`。
    pub fn register(&self, binding: HotkeyBinding) -> Result<HotkeyHandle, ShellError> {
        self.inner.register(binding)
    }

    /// 注销热键。
    ///
    /// # 参数
    /// - `handle`：注册时返回的句柄。
    pub fn unregister(&self, handle: HotkeyHandle) -> Result<(), ShellError> {
        self.inner.unregister(handle)
    }

    /// 当前已注册的热键列表。
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

    /// 错误可转换为适配层错误。
    #[test]
    fn parse_error_converts() {
        let err: ShellError = Hotkey::parse("x").unwrap_err().into();
        assert!(matches!(err, ShellError::InvalidHotkey(_)));
    }

    /// 登记表：句柄自增、同热键冲突、注销后可重新登记。
    #[test]
    fn table_conflict_and_release() {
        let mut t = HotkeyTable::default();
        let mk = |s: &str, c: AppCommand| HotkeyBinding {
            hotkey: Hotkey::parse(s).unwrap(),
            command: c,
        };
        let h1 = t.insert(mk("Ctrl+Alt+S", AppCommand::Cancel)).unwrap();
        let h2 = t.insert(mk("Ctrl+Alt+D", AppCommand::Undo)).unwrap();
        assert_ne!(h1, h2);
        // 等价写法也要判为冲突
        assert!(matches!(
            t.insert(mk("alt+ctrl+s", AppCommand::Redo)),
            Err(ShellError::HotkeyConflict(_))
        ));
        assert_eq!(t.get(h2).unwrap().command, AppCommand::Undo);
        assert_eq!(t.list().len(), 2);
        assert!(t.remove(h1).is_some());
        assert!(t.remove(h1).is_none());
        assert!(t.insert(mk("Ctrl+Alt+S", AppCommand::Redo)).is_ok());
    }

    /// 能力不可用时启动直接降级为错误。
    #[test]
    fn start_degrades_when_capability_missing() {
        use snow_capability::{CapabilityRegistry, Platform};
        let caps = CapabilityRegistry::for_platform(Platform::MacOs);
        let d = crate::dispatch::Dispatcher::from_fn(|_, _| {});
        assert!(matches!(
            HotkeyService::start(&caps, d),
            Err(ShellError::Unsupported { .. })
        ));
    }
}
