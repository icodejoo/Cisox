//! 托盘：与 gpui 无关的托盘描述、菜单动作映射与服务。
//!
//! 菜单点击 / 托盘点击触发后：`TrayAction::Command` 携带的动作负载 `A`（UI 进程里是
//! `AppCommand`，托盘宿主里是动作 id 字符串）以 [`ActionOrigin::Tray`](crate::dispatch::ActionOrigin::Tray) 交给 [`Dispatcher`]；
//! `TrayAction::Signal`（如“退出”“打开设置”这类不属于命令集的动作）通过
//! [`TrayService::signals`] 交给应用主循环处理。

use crate::dispatch::Dispatcher;
use crate::error::ShellError;
use std::collections::HashMap;
use std::sync::Arc;

/// 托盘图标位图（RGBA8，行优先）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrayIconImage {
    /// RGBA 像素（共享所有权：克隆图标、菜单计划不复制像素）。
    rgba: Arc<[u8]>,
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
    /// use snow_shell_core::tray::TrayIconImage;
    /// assert!(TrayIconImage::new(vec![0; 16], 2, 2).is_ok());
    /// assert!(TrayIconImage::new(vec![0; 15], 2, 2).is_err());
    /// ```
    pub fn new(rgba: Vec<u8>, width: u32, height: u32) -> Result<Self, ShellError> {
        Self::from_shared(rgba.into(), width, height)
    }

    /// 由共享的 RGBA 数据构造（不复制像素）；校验规则同 [`TrayIconImage::new`]。
    ///
    /// # 参数
    /// - `rgba`：共享像素数据。
    /// - `width` / `height`：像素尺寸。
    ///
    /// # 返回
    /// 图标，或 `InvalidArgument`。
    ///
    /// ```rust
    /// use snow_shell_core::tray::TrayIconImage;
    /// let pixels: std::sync::Arc<[u8]> = vec![0; 16].into();
    /// let icon = TrayIconImage::from_shared(pixels.clone(), 2, 2).unwrap();
    /// assert!(std::ptr::eq(icon.rgba().as_ptr(), pixels.as_ptr()));
    /// ```
    pub fn from_shared(rgba: Arc<[u8]>, width: u32, height: u32) -> Result<Self, ShellError> {
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
    /// use snow_shell_core::tray::TrayIconImage;
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

    /// RGBA8 像素数据（行优先，长度恒为 `宽 * 高 * 4`）。
    pub fn rgba(&self) -> &[u8] {
        &self.rgba
    }
}

/// 托盘触发的动作；`A` 是不透明的动作负载。
#[derive(Debug, Clone, PartialEq)]
pub enum TrayAction<A> {
    /// 把动作交给 [`Dispatcher`]（来源 `Tray`）。
    Command(A),
    /// 应用自定义信号，经 [`TrayService::signals`] 取出。
    Signal(String),
}

/// 托盘菜单项。
#[derive(Debug, Clone, PartialEq)]
pub enum TrayMenuEntry<A> {
    /// 普通条目。
    Item {
        /// 显示文字（应已本地化）。
        label: String,
        /// 是否可点击。
        enabled: bool,
        /// 勾选状态，None 表示不带复选框（原生复选框菜单项不能同时显示图标）。
        checked: Option<bool>,
        /// 图标，None 表示无图标。
        icon: Option<TrayIconImage>,
        /// 点击动作。
        action: TrayAction<A>,
    },
    /// 分隔线。
    Separator,
}

/// 托盘描述。
#[derive(Debug, Clone, PartialEq)]
pub struct TraySpec<A> {
    /// 悬停提示。
    pub tooltip: String,
    /// 图标。
    pub icon: TrayIconImage,
    /// 右键菜单。
    pub menu: Vec<TrayMenuEntry<A>>,
    /// 左键单击动作；设置后左键不再弹出菜单。
    pub on_left_click: Option<TrayAction<A>>,
    /// 左键双击动作（Windows）。
    pub on_double_click: Option<TrayAction<A>>,
    /// 中键单击动作。
    pub on_middle_click: Option<TrayAction<A>>,
}

/// 菜单构建计划中的一项（已分配稳定 id）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum MenuPlan {
    /// 条目：id、文字、是否可用、勾选状态、图标。
    Item(String, String, bool, Option<bool>, Option<TrayIconImage>),
    /// 分隔线。
    Separator,
}

/// 菜单 id 到动作的映射（纯逻辑）。
#[derive(Debug)]
pub(crate) struct TrayActions<A> {
    /// id → 动作。
    map: HashMap<String, TrayAction<A>>,
}

impl<A> Default for TrayActions<A> {
    /// 空映射（不要求 `A: Default`）。
    fn default() -> Self {
        Self {
            map: HashMap::new(),
        }
    }
}

impl<A: Clone> TrayActions<A> {
    /// 为菜单项分配 id（`tray.item.N`），返回构建计划与映射表。
    pub(crate) fn plan(entries: &[TrayMenuEntry<A>]) -> (Vec<MenuPlan>, TrayActions<A>) {
        let mut actions = TrayActions::default();
        let plan = entries
            .iter()
            .enumerate()
            .map(|(i, e)| match e {
                TrayMenuEntry::Separator => MenuPlan::Separator,
                TrayMenuEntry::Item {
                    label,
                    enabled,
                    checked,
                    icon,
                    action,
                } => {
                    let id = format!("tray.item.{i}");
                    actions.map.insert(id.clone(), action.clone());
                    MenuPlan::Item(id, label.clone(), *enabled, *checked, icon.clone())
                }
            })
            .collect();
        (plan, actions)
    }

    /// 按菜单 id 取动作。
    pub(crate) fn resolve(&self, id: &str) -> Option<&TrayAction<A>> {
        self.map.get(id)
    }

    /// 按菜单里的位置（0 起，与 `plan` 的顺序一致）取动作；分隔线与越界位置返回 `None`。
    #[cfg_attr(not(windows), allow(dead_code))]
    pub(crate) fn resolve_position(
        &self,
        plan: &[MenuPlan],
        position: usize,
    ) -> Option<&TrayAction<A>> {
        match plan.get(position)? {
            MenuPlan::Item(id, ..) => self.resolve(id),
            MenuPlan::Separator => None,
        }
    }
}

#[cfg(windows)]
mod backend {
    //! Windows 后端：专用线程持有 `TrayIcon` 并泵消息。

    use super::*;
    use crate::dispatch::ActionOrigin;
    use crate::native::{self, LoopWaker};
    use crate::service_thread::ServiceThread;
    use std::sync::atomic::AtomicBool;
    use std::sync::mpsc::{Receiver, Sender, channel};
    use tray_icon::menu::{ContextMenu, Menu, MenuItem, PredefinedMenuItem};
    use tray_icon::{
        Icon, MouseButton, MouseButtonState, TrayIcon, TrayIconBuilder, TrayIconEvent,
    };

    /// 进程内只允许一个托盘服务（事件通道是进程级单例）。
    static RUNNING: AtomicBool = AtomicBool::new(false);

    /// 控制请求。
    enum Ctl<A> {
        /// 更新悬停提示。
        SetTooltip(String),
        /// 设置信号出口（设置后信号不再进入 `signals()` 通道）。
        SetSignalSink(SignalSink),
        /// 整体替换右键菜单。
        SetMenu(Vec<TrayMenuEntry<A>>),
        /// 停止线程。
        Shutdown,
    }

    /// 后端句柄。
    pub(super) struct Inner<A> {
        /// 控制通道。
        ctl: Sender<Ctl<A>>,
        /// 后台服务线程；丢弃时通知退出、join 并释放单例标记（托盘图标随线程退出移除）。
        thread: ServiceThread<LoopWaker>,
        /// 自定义信号接收端。
        signals: Receiver<String>,
    }

    /// 按构建计划创建菜单。
    fn build_menu(plan: &[MenuPlan]) -> Result<Menu, ShellError> {
        let plat =
            |what: &str, e: &dyn std::fmt::Display| ShellError::Platform(format!("{what}: {e}"));
        let menu = Menu::new();
        for p in plan {
            match p {
                MenuPlan::Separator => menu
                    .append(&PredefinedMenuItem::separator())
                    .map_err(|e| plat("追加分隔线", &e))?,
                MenuPlan::Item(id, label, enabled, checked, icon) => {
                    let native_icon = if let Some(i) = icon {
                        let (w, h) = i.size();
                        Some(
                            tray_icon::menu::Icon::from_rgba(i.rgba.to_vec(), w, h)
                                .map_err(|e| plat("图标转换", &e))?,
                        )
                    } else {
                        None
                    };
                    if let Some(c) = checked {
                        let item = tray_icon::menu::CheckMenuItem::with_id(
                            id.as_str(),
                            label,
                            *enabled,
                            *c,
                            None,
                        );
                        menu.append(&item).map_err(|e| plat("追加勾选菜单项", &e))?;
                    } else if native_icon.is_some() {
                        let item = tray_icon::menu::IconMenuItem::with_id(
                            id.as_str(),
                            label,
                            *enabled,
                            native_icon,
                            None,
                        );
                        menu.append(&item).map_err(|e| plat("追加图标菜单项", &e))?;
                    } else {
                        let item = MenuItem::with_id(id.as_str(), label, *enabled, None);
                        menu.append(&item).map_err(|e| plat("追加菜单项", &e))?;
                    }
                }
            }
        }
        Ok(menu)
    }

    /// 在线程内创建托盘图标（不带菜单：菜单每次弹出前才按计划构建，见 [`Worker::show_menu`]）。
    fn build_tray<A>(spec: &TraySpec<A>) -> Result<TrayIcon, ShellError> {
        let plat =
            |what: &str, e: &dyn std::fmt::Display| ShellError::Platform(format!("{what}: {e}"));
        let (w, h) = spec.icon.size();
        let icon = Icon::from_rgba(spec.icon.rgba.to_vec(), w, h).map_err(|e| plat("图标", &e))?;
        TrayIconBuilder::new()
            // 菜单由本模块在点击事件里按需弹出，库自带的弹出行为全部关掉
            .with_menu_on_left_click(false)
            .with_menu_on_right_click(false)
            .with_tooltip(&spec.tooltip)
            .with_icon(icon)
            .build()
            .map_err(|e| plat("创建托盘", &e))
    }

    /// 后台线程状态。
    struct Worker<A> {
        /// 托盘对象（必须留在创建线程）。
        tray: TrayIcon,
        /// 菜单构建计划：空闲时只留这份纯数据，原生菜单（含每项图标位图）在弹出时才创建、关闭后立即销毁。
        plan: Vec<MenuPlan>,
        /// 菜单动作表。
        actions: TrayActions<A>,
        /// 左键动作。
        on_left: Option<TrayAction<A>>,
        /// 双击动作。
        on_double: Option<TrayAction<A>>,
        /// 中键单击动作。
        on_middle: Option<TrayAction<A>>,
        /// 动作出口。
        dispatcher: Dispatcher<A>,
        /// 信号发送端。
        signals: Sender<String>,
        /// 自定义信号出口；有值时优先于 `signals` 通道。
        sink: Option<SignalSink>,
    }

    impl<A: Clone> Worker<A> {
        /// 执行动作。
        fn run(&self, action: &TrayAction<A>) {
            match action {
                TrayAction::Command(cmd) => self.dispatcher.send(ActionOrigin::Tray, cmd.clone()),
                TrayAction::Signal(s) => match &self.sink {
                    Some(sink) => sink(s.clone()),
                    None => {
                        let _ = self.signals.send(s.clone());
                    }
                },
            }
        }

        /// 按计划临时构建原生菜单，在光标处弹出，选中项交给动作出口；菜单（含图标位图）随即销毁。
        ///
        /// 空闲时不留原生菜单，每项图标的位图（约 8KB 提交 / 项）就不占托盘态内存。
        fn show_menu(&self) {
            if self.plan.is_empty() {
                return;
            }
            let menu = match build_menu(&self.plan) {
                Ok(menu) => menu,
                Err(e) => {
                    tracing::warn!(%e, "构建托盘菜单失败");
                    return;
                }
            };
            let picked = native::track_popup_menu_at_cursor(
                self.tray.window_handle() as isize,
                menu.hpopupmenu(),
            );
            if let Some(action) =
                picked.and_then(|pos| self.actions.resolve_position(&self.plan, pos))
            {
                self.run(action);
            }
        }

        /// 处理控制请求与托盘事件；返回 `false` 表示退出。
        fn tick(&mut self, ctl: &Receiver<Ctl<A>>) -> bool {
            while let Ok(req) = ctl.try_recv() {
                match req {
                    Ctl::SetTooltip(t) => {
                        if let Err(e) = self.tray.set_tooltip(Some(t)) {
                            tracing::warn!(%e, "更新托盘提示失败");
                        }
                    }
                    Ctl::SetSignalSink(sink) => self.sink = Some(sink),
                    Ctl::SetMenu(entries) => {
                        (self.plan, self.actions) = TrayActions::plan(&entries);
                    }
                    Ctl::Shutdown => return false,
                }
            }
            while let Ok(ev) = TrayIconEvent::receiver().try_recv() {
                match ev {
                    // 左键：有左键动作就执行，否则弹菜单
                    TrayIconEvent::Click {
                        button: MouseButton::Left,
                        button_state: MouseButtonState::Up,
                        ..
                    } => match self.on_left.as_ref() {
                        Some(action) => self.run(action),
                        None => self.show_menu(),
                    },
                    TrayIconEvent::Click {
                        button: MouseButton::Right,
                        button_state: MouseButtonState::Up,
                        ..
                    } => self.show_menu(),
                    TrayIconEvent::DoubleClick {
                        button: MouseButton::Left,
                        ..
                    } => {
                        if let Some(action) = self.on_double.as_ref() {
                            self.run(action);
                        }
                    }
                    TrayIconEvent::Click {
                        button: MouseButton::Middle,
                        button_state: MouseButtonState::Up,
                        ..
                    } => {
                        if let Some(action) = self.on_middle.as_ref() {
                            self.run(action);
                        }
                    }
                    _ => {}
                }
            }
            true
        }
    }

    impl<A: Clone + Send + 'static> Inner<A> {
        /// 启动后台线程并等待托盘创建完成。
        pub(super) fn start(
            spec: TraySpec<A>,
            dispatcher: Dispatcher<A>,
        ) -> Result<Self, ShellError> {
            let (ctl_tx, ctl_rx) = channel::<Ctl<A>>();
            let (sig_tx, sig_rx) = channel::<String>();
            let notify_tx = ctl_tx.clone();
            let thread = ServiceThread::spawn(
                &RUNNING,
                "TrayService",
                "shell-tray",
                "创建托盘线程失败",
                move || {
                    let (plan, actions) = TrayActions::plan(&spec.menu);
                    let tray = build_tray(&spec)?;
                    let waker = native::current_thread_waker();
                    let worker = Worker {
                        tray,
                        plan,
                        actions,
                        on_left: spec.on_left_click,
                        on_double: spec.on_double_click,
                        on_middle: spec.on_middle_click,
                        dispatcher,
                        signals: sig_tx,
                        sink: None,
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
                signals: sig_rx,
            })
        }

        /// 更新悬停提示。
        pub(super) fn set_tooltip(&self, text: String) -> Result<(), ShellError> {
            self.ctl
                .send(Ctl::SetTooltip(text))
                .map_err(|_| ShellError::ServiceClosed)?;
            self.thread.ready().wake();
            Ok(())
        }

        /// 整体替换右键菜单。
        pub(super) fn set_menu(&self, entries: Vec<TrayMenuEntry<A>>) -> Result<(), ShellError> {
            self.ctl
                .send(Ctl::SetMenu(entries))
                .map_err(|_| ShellError::ServiceClosed)?;
            self.thread.ready().wake();
            Ok(())
        }

        /// 设置信号出口。
        pub(super) fn set_signal_sink(&self, sink: SignalSink) -> Result<(), ShellError> {
            self.ctl
                .send(Ctl::SetSignalSink(sink))
                .map_err(|_| ShellError::ServiceClosed)?;
            self.thread.ready().wake();
            Ok(())
        }

        /// 信号接收端。
        pub(super) fn signals(&self) -> &Receiver<String> {
            &self.signals
        }
    }
}

#[cfg(not(windows))]
mod backend {
    //! 非 Windows 降级后端：不创建托盘。

    use super::*;
    use crate::error::{CAPABILITY_TRAY, REASON_NOT_IMPLEMENTED};
    use std::marker::PhantomData;
    use std::sync::mpsc::Receiver;

    /// 降级后端句柄（`start` 恒失败，故不会被构造）。
    #[allow(dead_code)]
    pub(super) struct Inner<A> {
        /// 永不产生信号的接收端。
        signals: Receiver<String>,
        /// 占位：保持与 Windows 后端同样的泛型形状。
        marker: PhantomData<A>,
    }

    /// 构造统一的“未实现”错误。
    fn unsupported() -> ShellError {
        ShellError::Unsupported {
            capability: CAPABILITY_TRAY,
            reason: REASON_NOT_IMPLEMENTED,
        }
    }

    impl<A> Inner<A> {
        /// 启动：不支持。
        pub(super) fn start(_spec: TraySpec<A>, _d: Dispatcher<A>) -> Result<Self, ShellError> {
            Err(unsupported())
        }

        /// 更新提示：不支持。
        pub(super) fn set_tooltip(&self, _text: String) -> Result<(), ShellError> {
            Err(unsupported())
        }

        /// 替换菜单：不支持。
        pub(super) fn set_menu(&self, _entries: Vec<TrayMenuEntry<A>>) -> Result<(), ShellError> {
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
pub struct TrayService<A> {
    /// 平台后端。
    inner: backend::Inner<A>,
}

impl<A: Clone + Send + 'static> TrayService<A> {
    /// 创建托盘。
    ///
    /// # 参数
    /// - `spec`：托盘描述。
    /// - `dispatcher`：动作出口；菜单 / 点击触发 `TrayAction::Command` 时在托盘线程上同步调用。
    ///
    /// # 返回
    /// 服务句柄；系统调用失败返回 `Platform`，重复创建返回 `AlreadyRunning`，
    /// 非 Windows 平台返回 `Unsupported`。
    ///
    /// ```no_run
    /// use snow_shell_core::dispatch::Dispatcher;
    /// use snow_shell_core::tray::*;
    /// let spec = TraySpec {
    ///     tooltip: "Cisox".into(),
    ///     icon: TrayIconImage::solid(32, 32, [0, 120, 255, 255]).unwrap(),
    ///     menu: vec![
    ///         TrayMenuEntry::Item { label: "截图".into(), enabled: true, checked: None, icon: None, action: TrayAction::Command("capture".to_string()) },
    ///         TrayMenuEntry::Separator,
    ///         TrayMenuEntry::Item { label: "退出".into(), enabled: true, checked: None, icon: None, action: TrayAction::Signal("quit".into()) },
    ///     ],
    ///     on_left_click: None,
    ///     on_double_click: None,
    ///     on_middle_click: None,
    /// };
    /// let tray = TrayService::start(spec, Dispatcher::from_fn(|_origin, _action: String| {})).unwrap();
    /// let _ = tray.signals().try_recv();
    /// ```
    pub fn start(spec: TraySpec<A>, dispatcher: Dispatcher<A>) -> Result<Self, ShellError> {
        Ok(Self {
            inner: backend::Inner::start(spec, dispatcher)?,
        })
    }

    /// 更新悬停提示文字。
    pub fn set_tooltip(&self, text: impl Into<String>) -> Result<(), ShellError> {
        self.inner.set_tooltip(text.into())
    }

    /// 整体替换右键菜单（例如界面语言切换后刷新文案）。
    ///
    /// # 参数
    /// - `entries`：新的菜单项，沿用 `TraySpec::menu` 的格式。
    ///
    /// # 返回
    /// 服务已关闭返回 `ServiceClosed`。
    ///
    /// ```no_run
    /// # fn demo(tray: &snow_shell_core::tray::TrayService<String>) {
    /// tray.set_menu(vec![]).unwrap();
    /// # }
    /// ```
    pub fn set_menu(&self, entries: Vec<TrayMenuEntry<A>>) -> Result<(), ShellError> {
        self.inner.set_menu(entries)
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
    /// # fn demo(tray: &snow_shell_core::tray::TrayService<String>) {
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
        assert_eq!(&*solid.rgba, &[1, 2, 3, 4, 1, 2, 3, 4]);
    }

    /// 菜单计划：条目获得唯一 id，分隔线不占映射，id 可还原动作。
    #[test]
    fn menu_plan_assigns_ids() {
        let entries = vec![
            TrayMenuEntry::Item {
                label: "取消".into(),
                enabled: true,
                checked: None,
                icon: None,
                action: TrayAction::Command("cancel".to_string()),
            },
            TrayMenuEntry::Separator,
            TrayMenuEntry::Item {
                label: "退出".into(),
                enabled: false,
                checked: Some(true),
                icon: None,
                action: TrayAction::Signal("quit".into()),
            },
        ];
        let (plan, actions) = TrayActions::plan(&entries);
        assert_eq!(plan.len(), 3);
        assert_eq!(plan[1], MenuPlan::Separator);
        let MenuPlan::Item(id0, _, en0, _, _) = &plan[0] else {
            panic!("应为条目");
        };
        let MenuPlan::Item(id2, label2, en2, checked2, _) = &plan[2] else {
            panic!("应为条目");
        };
        assert_ne!(id0, id2);
        assert!(*en0 && !*en2);
        assert_eq!(label2, "退出");
        assert_eq!(*checked2, Some(true));
        assert_eq!(
            actions.resolve(id0),
            Some(&TrayAction::Command("cancel".to_string()))
        );
        assert_eq!(
            actions.resolve(id2),
            Some(&TrayAction::Signal("quit".into()))
        );
        assert_eq!(actions.resolve("nope"), None);
    }

    /// 按菜单位置取动作：条目命中、分隔线与越界为空。
    #[test]
    fn resolve_position_follows_plan_order() {
        let entries = vec![
            TrayMenuEntry::Item {
                label: "甲".into(),
                enabled: true,
                checked: None,
                icon: None,
                action: TrayAction::Command("a".to_string()),
            },
            TrayMenuEntry::Separator,
            TrayMenuEntry::Item {
                label: "乙".into(),
                enabled: true,
                checked: Some(false),
                icon: None,
                action: TrayAction::Signal("b".into()),
            },
        ];
        let (plan, actions) = TrayActions::plan(&entries);
        assert_eq!(
            actions.resolve_position(&plan, 0),
            Some(&TrayAction::Command("a".to_string()))
        );
        assert_eq!(actions.resolve_position(&plan, 1), None);
        assert_eq!(
            actions.resolve_position(&plan, 2),
            Some(&TrayAction::Signal("b".into()))
        );
        assert_eq!(actions.resolve_position(&plan, 3), None);
    }

    /// 图标像素可按只读切片取回。
    #[test]
    fn icon_exposes_pixels() {
        let icon = TrayIconImage::solid(1, 2, [9, 8, 7, 6]).unwrap();
        assert_eq!(icon.rgba(), &[9, 8, 7, 6, 9, 8, 7, 6]);
    }

    /// 非 Windows 平台启动托盘降级为错误。
    #[cfg(not(windows))]
    #[test]
    fn start_is_unsupported_off_windows() {
        let spec = TraySpec::<String> {
            tooltip: String::new(),
            icon: TrayIconImage::solid(1, 1, [0; 4]).unwrap(),
            menu: vec![],
            on_left_click: None,
            on_double_click: None,
            on_middle_click: None,
        };
        let d = Dispatcher::from_fn(|_, _| {});
        assert!(matches!(
            TrayService::start(spec, d),
            Err(ShellError::Unsupported { .. })
        ));
    }
}
