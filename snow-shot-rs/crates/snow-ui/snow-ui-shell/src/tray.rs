//! 托盘：与 gpui 无关的托盘描述、菜单动作映射与服务。
//!
//! 菜单点击 / 托盘点击触发后：`TrayAction::Command` 以
//! [`CommandSource::Tray`](snow_app_core::command::CommandSource::Tray) 送入命令总线；
//! `TrayAction::Signal`（如“退出”“打开设置”这类不属于 `AppCommand` 的动作）通过
//! [`TrayService::signals`] 交给应用主循环处理。

use crate::error::ShellError;
use snow_app_core::command::AppCommand;
use std::collections::HashMap;

/// 托盘图标位图（RGBA8，行优先）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrayIconImage {
    /// RGBA 像素。
    rgba: Vec<u8>,
    /// 宽（像素）。
    width: u32,
    /// 高（像素）。
    height: u32,
}

impl TrayIconImage {
    /// 由 RGBA 数据构造；长度必须等于 `width * height * 4` 且尺寸非零。
    ///
    /// # 参数
    /// - `rgba`：像素数据。
    /// - `width` / `height`：像素尺寸。
    ///
    /// # 返回
    /// 图标，或 `InvalidArgument`。
    ///
    /// ```rust
    /// use snow_ui_shell::tray::TrayIconImage;
    /// assert!(TrayIconImage::new(vec![0; 16], 2, 2).is_ok());
    /// assert!(TrayIconImage::new(vec![0; 15], 2, 2).is_err());
    /// ```
    pub fn new(rgba: Vec<u8>, width: u32, height: u32) -> Result<Self, ShellError> {
        let expected = (width as usize)
            .checked_mul(height as usize)
            .and_then(|n| n.checked_mul(4));
        if width == 0 || height == 0 || expected != Some(rgba.len()) {
            return Err(ShellError::InvalidArgument(format!(
                "托盘图标数据长度 {} 与尺寸 {width}x{height} 不符",
                rgba.len()
            )));
        }
        Ok(Self {
            rgba,
            width,
            height,
        })
    }

    /// 纯色方块图标（占位图标 / 验证程序用）。
    ///
    /// ```rust
    /// use snow_ui_shell::tray::TrayIconImage;
    /// let icon = TrayIconImage::solid(16, 16, [0, 120, 255, 255]).unwrap();
    /// assert_eq!(icon.size(), (16, 16));
    /// ```
    pub fn solid(width: u32, height: u32, rgba: [u8; 4]) -> Result<Self, ShellError> {
        let data = rgba
            .iter()
            .copied()
            .cycle()
            .take(width as usize * height as usize * 4)
            .collect();
        Self::new(data, width, height)
    }

    /// 像素尺寸 `(宽, 高)`。
    pub fn size(&self) -> (u32, u32) {
        (self.width, self.height)
    }
}

/// 托盘触发的动作。
#[derive(Debug, Clone, PartialEq)]
pub enum TrayAction {
    /// 派发命令到总线（来源 `Tray`）。
    Command(AppCommand),
    /// 应用自定义信号，经 [`TrayService::signals`] 取出。
    Signal(String),
}

/// 托盘菜单项。
#[derive(Debug, Clone, PartialEq)]
pub enum TrayMenuEntry {
    /// 普通条目。
    Item {
        /// 显示文字（应已本地化）。
        label: String,
        /// 是否可点击。
        enabled: bool,
        /// 点击动作。
        action: TrayAction,
    },
    /// 分隔线。
    Separator,
}

/// 托盘描述。
#[derive(Debug, Clone, PartialEq)]
pub struct TraySpec {
    /// 悬停提示。
    pub tooltip: String,
    /// 图标。
    pub icon: TrayIconImage,
    /// 右键菜单。
    pub menu: Vec<TrayMenuEntry>,
    /// 左键单击动作；设置后左键不再弹出菜单。
    pub on_left_click: Option<TrayAction>,
    /// 左键双击动作（Windows）。
    pub on_double_click: Option<TrayAction>,
}

/// 菜单构建计划中的一项（已分配稳定 id）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum MenuPlan {
    /// 条目：id、文字、是否可用。
    Item(String, String, bool),
    /// 分隔线。
    Separator,
}

/// 菜单 id 到动作的映射（纯逻辑）。
#[derive(Debug, Default)]
pub(crate) struct TrayActions {
    /// id → 动作。
    map: HashMap<String, TrayAction>,
}

impl TrayActions {
    /// 为菜单项分配 id（`tray.item.N`），返回构建计划与映射表。
    pub(crate) fn plan(entries: &[TrayMenuEntry]) -> (Vec<MenuPlan>, TrayActions) {
        let mut actions = TrayActions::default();
        let plan = entries
            .iter()
            .enumerate()
            .map(|(i, e)| match e {
                TrayMenuEntry::Separator => MenuPlan::Separator,
                TrayMenuEntry::Item {
                    label,
                    enabled,
                    action,
                } => {
                    let id = format!("tray.item.{i}");
                    actions.map.insert(id.clone(), action.clone());
                    MenuPlan::Item(id, label.clone(), *enabled)
                }
            })
            .collect();
        (plan, actions)
    }

    /// 按菜单 id 取动作。
    pub(crate) fn resolve(&self, id: &str) -> Option<&TrayAction> {
        self.map.get(id)
    }
}

#[cfg(windows)]
mod backend {
    //! Windows 后端：专用线程持有 `TrayIcon` 并泵消息。

    use super::*;
    use crate::dispatch::Dispatcher;
    use crate::native::{self, LoopWaker};
    use snow_app_core::command::CommandSource;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::mpsc::{Receiver, Sender, channel};
    use std::thread::JoinHandle;
    use tray_icon::menu::{Menu, MenuEvent, MenuItem, PredefinedMenuItem};
    use tray_icon::{
        Icon, MouseButton, MouseButtonState, TrayIcon, TrayIconBuilder, TrayIconEvent,
    };

    /// 进程内只允许一个托盘服务（事件通道是进程级单例）。
    static RUNNING: AtomicBool = AtomicBool::new(false);

    /// 控制请求。
    enum Ctl {
        /// 更新悬停提示。
        SetTooltip(String),
        /// 设置信号出口（设置后信号不再进入 `signals()` 通道）。
        SetSignalSink(SignalSink),
        /// 停止线程。
        Shutdown,
    }

    /// 后端句柄。
    pub(super) struct Inner {
        /// 控制通道。
        ctl: Sender<Ctl>,
        /// 唤醒器。
        waker: LoopWaker,
        /// 后台线程。
        join: Option<JoinHandle<()>>,
        /// 自定义信号接收端。
        signals: Receiver<String>,
    }

    /// 在线程内创建托盘及其菜单。
    fn build_tray(spec: &TraySpec, plan: &[MenuPlan]) -> Result<(TrayIcon, Menu), ShellError> {
        let plat =
            |what: &str, e: &dyn std::fmt::Display| ShellError::Platform(format!("{what}: {e}"));
        let menu = Menu::new();
        for p in plan {
            match p {
                MenuPlan::Separator => menu
                    .append(&PredefinedMenuItem::separator())
                    .map_err(|e| plat("追加分隔线", &e))?,
                MenuPlan::Item(id, label, enabled) => menu
                    .append(&MenuItem::with_id(id.as_str(), label, *enabled, None))
                    .map_err(|e| plat("追加菜单项", &e))?,
            }
        }
        let (w, h) = spec.icon.size();
        let icon = Icon::from_rgba(spec.icon.rgba.clone(), w, h).map_err(|e| plat("图标", &e))?;
        let tray = TrayIconBuilder::new()
            .with_menu(Box::new(menu.clone()))
            .with_menu_on_left_click(spec.on_left_click.is_none())
            .with_tooltip(&spec.tooltip)
            .with_icon(icon)
            .build()
            .map_err(|e| plat("创建托盘", &e))?;
        Ok((tray, menu))
    }

    /// 后台线程状态。
    struct Worker {
        /// 托盘对象（必须留在创建线程）。
        tray: TrayIcon,
        /// 菜单，随托盘存活。
        _menu: Menu,
        /// 菜单动作表。
        actions: TrayActions,
        /// 左键动作。
        on_left: Option<TrayAction>,
        /// 双击动作。
        on_double: Option<TrayAction>,
        /// 命令出口。
        dispatcher: Dispatcher,
        /// 信号发送端。
        signals: Sender<String>,
        /// 自定义信号出口；有值时优先于 `signals` 通道。
        sink: Option<SignalSink>,
    }

    impl Worker {
        /// 执行动作。
        fn run(&self, action: &TrayAction) {
            match action {
                TrayAction::Command(cmd) => self.dispatcher.send(CommandSource::Tray, cmd.clone()),
                TrayAction::Signal(s) => match &self.sink {
                    Some(sink) => sink(s.clone()),
                    None => {
                        let _ = self.signals.send(s.clone());
                    }
                },
            }
        }

        /// 处理控制请求与托盘/菜单事件；返回 `false` 表示退出。
        fn tick(&mut self, ctl: &Receiver<Ctl>) -> bool {
            while let Ok(req) = ctl.try_recv() {
                match req {
                    Ctl::SetTooltip(t) => {
                        if let Err(e) = self.tray.set_tooltip(Some(t)) {
                            tracing::warn!(%e, "更新托盘提示失败");
                        }
                    }
                    Ctl::SetSignalSink(sink) => self.sink = Some(sink),
                    Ctl::Shutdown => return false,
                }
            }
            while let Ok(ev) = MenuEvent::receiver().try_recv() {
                if let Some(action) = self.actions.resolve(&ev.id.0) {
                    self.run(action);
                }
            }
            while let Ok(ev) = TrayIconEvent::receiver().try_recv() {
                let action = match ev {
                    TrayIconEvent::Click {
                        button: MouseButton::Left,
                        button_state: MouseButtonState::Up,
                        ..
                    } => self.on_left.as_ref(),
                    TrayIconEvent::DoubleClick {
                        button: MouseButton::Left,
                        ..
                    } => self.on_double.as_ref(),
                    _ => None,
                };
                if let Some(a) = action {
                    self.run(a);
                }
            }
            true
        }
    }

    impl Inner {
        /// 启动后台线程并等待托盘创建完成。
        pub(super) fn start(spec: TraySpec, dispatcher: Dispatcher) -> Result<Self, ShellError> {
            if RUNNING.swap(true, Ordering::SeqCst) {
                return Err(ShellError::AlreadyRunning("TrayService"));
            }
            let (ctl_tx, ctl_rx) = channel::<Ctl>();
            let (sig_tx, sig_rx) = channel::<String>();
            let (ready_tx, ready_rx) = channel::<Result<LoopWaker, ShellError>>();
            let spawned = std::thread::Builder::new()
                .name("shell-tray".into())
                .spawn(move || {
                    let (plan, actions) = TrayActions::plan(&spec.menu);
                    let (tray, menu) = match build_tray(&spec, &plan) {
                        Ok(v) => v,
                        Err(e) => {
                            let _ = ready_tx.send(Err(e));
                            return;
                        }
                    };
                    let waker = native::current_thread_waker();
                    let mut worker = Worker {
                        tray,
                        _menu: menu,
                        actions,
                        on_left: spec.on_left_click,
                        on_double: spec.on_double_click,
                        dispatcher,
                        signals: sig_tx,
                        sink: None,
                    };
                    let _ = ready_tx.send(Ok(waker));
                    native::run_message_loop(|| worker.tick(&ctl_rx));
                });
            let join = match spawned {
                Ok(j) => j,
                Err(e) => {
                    RUNNING.store(false, Ordering::SeqCst);
                    return Err(ShellError::Platform(format!("创建托盘线程失败: {e}")));
                }
            };
            match ready_rx.recv() {
                Ok(Ok(waker)) => Ok(Self {
                    ctl: ctl_tx,
                    waker,
                    join: Some(join),
                    signals: sig_rx,
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

        /// 更新悬停提示。
        pub(super) fn set_tooltip(&self, text: String) -> Result<(), ShellError> {
            self.ctl
                .send(Ctl::SetTooltip(text))
                .map_err(|_| ShellError::ServiceClosed)?;
            self.waker.wake();
            Ok(())
        }

        /// 设置信号出口。
        pub(super) fn set_signal_sink(&self, sink: SignalSink) -> Result<(), ShellError> {
            self.ctl
                .send(Ctl::SetSignalSink(sink))
                .map_err(|_| ShellError::ServiceClosed)?;
            self.waker.wake();
            Ok(())
        }

        /// 信号接收端。
        pub(super) fn signals(&self) -> &Receiver<String> {
            &self.signals
        }
    }

    impl Drop for Inner {
        /// 停止后台线程（托盘图标随线程退出移除）并释放单例标记。
        fn drop(&mut self) {
            let _ = self.ctl.send(Ctl::Shutdown);
            self.waker.wake();
            if let Some(j) = self.join.take() {
                let _ = j.join();
            }
            RUNNING.store(false, Ordering::SeqCst);
        }
    }
}

#[cfg(not(windows))]
mod backend {
    //! 非 Windows 降级后端：不创建托盘。

    use super::*;
    use crate::dispatch::Dispatcher;
    use snow_capability::{Capability, REASON_NOT_IMPLEMENTED};
    use std::sync::mpsc::Receiver;

    /// 降级后端句柄（`start` 恒失败，故不会被构造）。
    #[allow(dead_code)]
    pub(super) struct Inner {
        /// 永不产生信号的接收端。
        signals: Receiver<String>,
    }

    /// 构造统一的“未实现”错误。
    fn unsupported() -> ShellError {
        ShellError::Unsupported {
            capability: Capability::Tray,
            reason: REASON_NOT_IMPLEMENTED,
        }
    }

    impl Inner {
        /// 启动：不支持。
        pub(super) fn start(_spec: TraySpec, _d: Dispatcher) -> Result<Self, ShellError> {
            Err(unsupported())
        }

        /// 更新提示：不支持。
        pub(super) fn set_tooltip(&self, _text: String) -> Result<(), ShellError> {
            Err(unsupported())
        }

        /// 设置信号出口：不支持。
        pub(super) fn set_signal_sink(&self, _sink: SignalSink) -> Result<(), ShellError> {
            Err(unsupported())
        }

        /// 信号接收端。
        pub(super) fn signals(&self) -> &Receiver<String> {
            &self.signals
        }
    }
}

/// 托盘自定义信号出口：在托盘线程上同步调用，只应做轻量转发（如投递到主线程收件箱）。
pub type SignalSink = Box<dyn Fn(String) + Send + 'static>;

/// 托盘服务：后台线程持有系统托盘图标，菜单/点击按 [`TraySpec`] 派发动作。
/// 丢弃即移除托盘图标。进程内只能启动一个实例。
pub struct TrayService {
    /// 平台后端。
    inner: backend::Inner,
}

impl TrayService {
    /// 创建托盘。
    ///
    /// # 参数
    /// - `caps`：能力表；`Tray` 不可用时返回 `Unsupported`（降级，不 panic）。
    /// - `spec`：托盘描述。
    /// - `dispatcher`：命令出口。
    ///
    /// # 返回
    /// 服务句柄；系统调用失败返回 `Platform`，重复创建返回 `AlreadyRunning`。
    ///
    /// ```no_run
    /// use snow_app_core::bus::CommandBus;
    /// use snow_app_core::command::AppCommand;
    /// use snow_capability::CapabilityRegistry;
    /// use snow_ui_shell::dispatch::Dispatcher;
    /// use snow_ui_shell::tray::*;
    /// let spec = TraySpec {
    ///     tooltip: "Cisox".into(),
    ///     icon: TrayIconImage::solid(32, 32, [0, 120, 255, 255]).unwrap(),
    ///     menu: vec![
    ///         TrayMenuEntry::Item { label: "取消".into(), enabled: true, action: TrayAction::Command(AppCommand::Cancel) },
    ///         TrayMenuEntry::Separator,
    ///         TrayMenuEntry::Item { label: "退出".into(), enabled: true, action: TrayAction::Signal("quit".into()) },
    ///     ],
    ///     on_left_click: None,
    ///     on_double_click: None,
    /// };
    /// let caps = CapabilityRegistry::for_current_platform();
    /// let tray = TrayService::start(&caps, spec, Dispatcher::from_bus(CommandBus::new())).unwrap();
    /// let _ = tray.signals().try_recv();
    /// ```
    pub fn start(
        caps: &snow_capability::CapabilityRegistry,
        spec: TraySpec,
        dispatcher: crate::dispatch::Dispatcher,
    ) -> Result<Self, ShellError> {
        crate::error::require_capability(caps, snow_capability::Capability::Tray)?;
        Ok(Self {
            inner: backend::Inner::start(spec, dispatcher)?,
        })
    }

    /// 更新悬停提示文字。
    pub fn set_tooltip(&self, text: impl Into<String>) -> Result<(), ShellError> {
        self.inner.set_tooltip(text.into())
    }

    /// 设置自定义信号出口：此后 `TrayAction::Signal` 直接回调 `sink`，不再进入 `signals()` 通道。
    ///
    /// # 参数
    /// - `sink`：在托盘线程上被调用，只做轻量转发。
    ///
    /// # 返回
    /// 服务已关闭返回 `ServiceClosed`。
    ///
    /// ```no_run
    /// # fn demo(tray: &snow_ui_shell::tray::TrayService) {
    /// tray.set_signal_sink(Box::new(|sig| println!("signal {sig}"))).unwrap();
    /// # }
    /// ```
    pub fn set_signal_sink(&self, sink: SignalSink) -> Result<(), ShellError> {
        self.inner.set_signal_sink(sink)
    }

    /// 自定义信号（`TrayAction::Signal`）的接收端，由应用主循环轮询。
    pub fn signals(&self) -> &std::sync::mpsc::Receiver<String> {
        self.inner.signals()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 图标数据长度与尺寸校验。
    #[test]
    fn icon_validation() {
        assert!(TrayIconImage::new(vec![0; 4 * 4 * 4], 4, 4).is_ok());
        assert!(TrayIconImage::new(vec![0; 10], 4, 4).is_err());
        assert!(TrayIconImage::new(vec![], 0, 0).is_err());
        let solid = TrayIconImage::solid(2, 1, [1, 2, 3, 4]).unwrap();
        assert_eq!(solid.rgba, vec![1, 2, 3, 4, 1, 2, 3, 4]);
    }

    /// 菜单计划：条目获得唯一 id，分隔线不占映射，id 可还原动作。
    #[test]
    fn menu_plan_assigns_ids() {
        let entries = vec![
            TrayMenuEntry::Item {
                label: "取消".into(),
                enabled: true,
                action: TrayAction::Command(AppCommand::Cancel),
            },
            TrayMenuEntry::Separator,
            TrayMenuEntry::Item {
                label: "退出".into(),
                enabled: false,
                action: TrayAction::Signal("quit".into()),
            },
        ];
        let (plan, actions) = TrayActions::plan(&entries);
        assert_eq!(plan.len(), 3);
        assert_eq!(plan[1], MenuPlan::Separator);
        let MenuPlan::Item(id0, _, en0) = &plan[0] else {
            panic!("应为条目");
        };
        let MenuPlan::Item(id2, label2, en2) = &plan[2] else {
            panic!("应为条目");
        };
        assert_ne!(id0, id2);
        assert!(*en0 && !*en2);
        assert_eq!(label2, "退出");
        assert_eq!(
            actions.resolve(id0),
            Some(&TrayAction::Command(AppCommand::Cancel))
        );
        assert_eq!(
            actions.resolve(id2),
            Some(&TrayAction::Signal("quit".into()))
        );
        assert_eq!(actions.resolve("nope"), None);
    }

    /// 能力不可用时启动降级为错误。
    #[test]
    fn start_degrades_when_capability_missing() {
        use snow_capability::{CapabilityRegistry, Platform};
        let caps = CapabilityRegistry::for_platform(Platform::Linux);
        let spec = TraySpec {
            tooltip: String::new(),
            icon: TrayIconImage::solid(1, 1, [0; 4]).unwrap(),
            menu: vec![],
            on_left_click: None,
            on_double_click: None,
        };
        let d = crate::dispatch::Dispatcher::from_fn(|_, _| {});
        assert!(matches!(
            TrayService::start(&caps, spec, d),
            Err(ShellError::Unsupported { .. })
        ));
    }
}
