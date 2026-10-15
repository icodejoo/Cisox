//! 托盘：描述与后端在 `snow-shell-core`，这里重导出并绑定 `AppCommand` 与能力表。
//!
//! 菜单点击 / 托盘点击触发后：`TrayAction::Command` 以
//! [`CommandSource::Tray`](snow_app_core::command::CommandSource::Tray) 送入命令总线；
//! `TrayAction::Signal`（如“退出”“打开设置”这类不属于 `AppCommand` 的动作）通过
//! [`TrayService::signals`] 交给应用主循环处理。

use crate::dispatch::Dispatcher;
use crate::error::{ShellError, require_capability};
use snow_app_core::command::AppCommand;
use snow_capability::{Capability, CapabilityRegistry};
use snow_shell_core::tray::TrayService as CoreTrayService;
pub use snow_shell_core::tray::{Balloon, BalloonIcon, SignalSink, TrayIconImage};

/// 托盘触发的动作：命令（派发到总线）或应用自定义信号。
pub type TrayAction = snow_shell_core::tray::TrayAction<AppCommand>;

/// 托盘菜单项。
pub type TrayMenuEntry = snow_shell_core::tray::TrayMenuEntry<AppCommand>;

/// 托盘描述。
pub type TraySpec = snow_shell_core::tray::TraySpec<AppCommand>;

/// 托盘服务：后台线程持有系统托盘图标，菜单/点击按 [`TraySpec`] 派发动作。
/// 丢弃即移除托盘图标。进程内只能启动一个实例。
pub struct TrayService {
    /// 后端服务（动作负载为 `AppCommand`）。
    inner: CoreTrayService<AppCommand>,
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
    ///         TrayMenuEntry::Item { label: "取消".into(), enabled: true, checked: None, icon: None, action: TrayAction::Command(AppCommand::Cancel(Default::default())) },
    ///         TrayMenuEntry::Separator,
    ///         TrayMenuEntry::Item { label: "退出".into(), enabled: true, checked: None, icon: None, action: TrayAction::Signal("quit".into()) },
    ///     ],
    ///     on_left_click: None,
    ///     on_double_click: None,
    ///     on_middle_click: None,
    /// };
    /// let caps = CapabilityRegistry::for_current_platform();
    /// let tray = TrayService::start(&caps, spec, Dispatcher::from_bus(CommandBus::new())).unwrap();
    /// let _ = tray.signals().try_recv();
    /// ```
    pub fn start(
        caps: &CapabilityRegistry,
        spec: TraySpec,
        dispatcher: Dispatcher,
    ) -> Result<Self, ShellError> {
        require_capability(caps, Capability::Tray)?;
        Ok(Self {
            inner: CoreTrayService::start(spec, dispatcher.to_core())?,
        })
    }

    /// 更新悬停提示文字。
    pub fn set_tooltip(&self, text: impl Into<String>) -> Result<(), ShellError> {
        self.inner.set_tooltip(text)
    }

    /// 弹一条系统通知气泡（Win10 起显示为 Toast）。
    ///
    /// # 参数
    /// - `balloon`：标题、正文、可选小图和点击后要打开的路径。
    ///
    /// # 返回
    /// 服务已关闭返回 `ServiceClosed`。
    ///
    /// ```no_run
    /// # fn demo(tray: &snow_ui_shell::tray::TrayService) {
    /// use snow_ui_shell::tray::Balloon;
    /// tray.show_balloon(Balloon { title: "Cisox".into(), text: "截图已复制".into(), ..Default::default() }).unwrap();
    /// # }
    /// ```
    pub fn show_balloon(&self, balloon: Balloon) -> Result<(), ShellError> {
        self.inner.show_balloon(balloon)
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
    /// # fn demo(tray: &snow_ui_shell::tray::TrayService) {
    /// tray.set_menu(vec![]).unwrap();
    /// # }
    /// ```
    pub fn set_menu(&self, entries: Vec<TrayMenuEntry>) -> Result<(), ShellError> {
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
    use snow_capability::Platform;

    /// 能力不可用时启动降级为错误。
    #[test]
    fn start_degrades_when_capability_missing() {
        let caps = CapabilityRegistry::for_platform(Platform::Linux);
        let spec = TraySpec {
            tooltip: String::new(),
            icon: TrayIconImage::solid(1, 1, [0; 4]).unwrap(),
            menu: vec![],
            on_left_click: None,
            on_double_click: None,
            on_middle_click: None,
        };
        let d = Dispatcher::from_fn(|_, _| {});
        assert!(matches!(
            TrayService::start(&caps, spec, d),
            Err(ShellError::Unsupported { .. })
        ));
    }

    /// 别名保持原用法：命令动作携带 `AppCommand`，信号动作携带字符串。
    #[test]
    fn aliases_keep_variant_paths() {
        let cmd = TrayAction::Command(AppCommand::PinSelection);
        let sig = TrayAction::Signal("quit".into());
        assert!(matches!(cmd, TrayAction::Command(AppCommand::PinSelection)));
        assert!(matches!(sig, TrayAction::Signal(ref s) if s == "quit"));
        let item = TrayMenuEntry::Item {
            label: "x".into(),
            enabled: true,
            checked: None,
            icon: None,
            action: cmd,
        };
        assert!(matches!(item, TrayMenuEntry::Item { .. }));
        assert!(matches!(TrayMenuEntry::Separator, TrayMenuEntry::Separator));
    }
}
