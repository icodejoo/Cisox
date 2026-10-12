//! 全局热键：热键模型、解析与后端在 `snow-shell-core`，这里重导出并绑定 `AppCommand` 与能力表。
//!
//! 热键触发后以 [`CommandSource::Hotkey`](snow_app_core::command::CommandSource::Hotkey)
//! 把绑定的 [`AppCommand`] 送入命令总线（见 [`crate::dispatch::Dispatcher`]）。

use crate::dispatch::Dispatcher;
use crate::error::{ShellError, require_capability};
use snow_app_core::command::AppCommand;
use snow_capability::{Capability, CapabilityRegistry};
use snow_shell_core::hotkey::HotkeyService as CoreHotkeyService;
pub use snow_shell_core::hotkey::{
    Hotkey, HotkeyEdge, HotkeyHandle, HotkeyParseError, Key, Modifiers,
};

/// 一条热键绑定：按下时派发 `command`；若设置了 `on_release`，松开时再派发它（按住说话用）。
pub type HotkeyBinding = snow_shell_core::hotkey::HotkeyBinding<AppCommand>;

/// 全局热键服务：后台线程注册系统热键，触发后经 [`Dispatcher`](crate::dispatch::Dispatcher)
/// 派发 `AppCommand`（来源 `Hotkey`）。丢弃即停止并注销全部热键。
///
/// 进程内只能启动一个实例；可在任意线程调用注册/注销。
pub struct HotkeyService {
    /// 后端服务（动作负载为 `AppCommand`）。
    inner: CoreHotkeyService<AppCommand>,
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
    /// svc.register(HotkeyBinding::new(
    ///     Hotkey::parse("Ctrl+Alt+S").unwrap(),
    ///     AppCommand::Cancel(Default::default()),
    /// )).unwrap();
    /// ```
    pub fn start(caps: &CapabilityRegistry, dispatcher: Dispatcher) -> Result<Self, ShellError> {
        require_capability(caps, Capability::GlobalHotkey)?;
        Ok(Self {
            inner: CoreHotkeyService::start(dispatcher.to_core())?,
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
        self.inner.registered()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use snow_capability::Platform;

    /// 边沿到命令：普通绑定只响应按下（松开被忽略，现有热键行为不变）；带 on_release 的松开才派发。
    #[test]
    fn edge_selects_command() {
        let hotkey = Hotkey::parse("F9").unwrap();
        let plain = HotkeyBinding::new(hotkey, AppCommand::StartDictation);
        assert_eq!(
            plain.command_for(HotkeyEdge::Pressed),
            Some(&AppCommand::StartDictation)
        );
        assert_eq!(plain.command_for(HotkeyEdge::Released), None);
        let hold = plain.with_release(AppCommand::StopDictation);
        assert_eq!(
            hold.command_for(HotkeyEdge::Released),
            Some(&AppCommand::StopDictation)
        );
    }

    /// 能力不可用时启动直接降级为错误。
    #[test]
    fn start_degrades_when_capability_missing() {
        let caps = CapabilityRegistry::for_platform(Platform::MacOs);
        let d = Dispatcher::from_fn(|_, _| {});
        assert!(matches!(
            HotkeyService::start(&caps, d),
            Err(ShellError::Unsupported { .. })
        ));
    }

    /// Windows：经适配层真实启动、注册、重复启动被拒、注销（用几乎不会被占用的组合，不模拟输入）。
    #[cfg(windows)]
    #[test]
    fn service_lifecycle_through_adapter() {
        let caps = CapabilityRegistry::for_platform(Platform::Windows);
        let svc = HotkeyService::start(&caps, Dispatcher::from_fn(|_, _| {})).unwrap();
        assert!(matches!(
            HotkeyService::start(&caps, Dispatcher::from_fn(|_, _| {})),
            Err(ShellError::AlreadyRunning(_))
        ));
        let binding = HotkeyBinding::new(
            Hotkey::parse("Ctrl+Alt+Shift+Win+F23").unwrap(),
            AppCommand::Cancel(Default::default()),
        );
        let handle = svc.register(binding).unwrap();
        assert_eq!(svc.registered().unwrap().len(), 1);
        svc.unregister(handle).unwrap();
        assert!(svc.registered().unwrap().is_empty());
    }
}
