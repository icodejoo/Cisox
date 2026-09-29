//! 命令派发出口：把热键/托盘事件转成 `AppCommand` 送进命令总线。

use snow_app_core::bus::CommandBus;
use snow_app_core::command::{AppCommand, CommandContext, CommandSource};
use std::sync::mpsc::{Sender, channel};
use std::sync::{Arc, Mutex};

/// 命令派发器，可克隆；事件线程调用 `send` 不会被 handler 耗时阻塞。
#[derive(Clone)]
pub struct Dispatcher {
    /// 实际派发函数。
    sink: Arc<dyn Fn(CommandSource, AppCommand) + Send + Sync>,
}

impl Dispatcher {
    /// 由任意函数构造（测试或自定义出口）。
    ///
    /// # 参数
    /// - `f`：收到 `(来源, 命令)` 时调用，在事件线程上同步执行。
    ///
    /// ```rust
    /// use snow_ui_shell::dispatch::Dispatcher;
    /// let d = Dispatcher::from_fn(|_src, _cmd| {});
    /// d.send(snow_app_core::command::CommandSource::Test, snow_app_core::command::AppCommand::Cancel);
    /// ```
    pub fn from_fn(f: impl Fn(CommandSource, AppCommand) + Send + Sync + 'static) -> Self {
        Self { sink: Arc::new(f) }
    }

    /// 接到命令总线：起一个后台线程串行 `emit`，失败只记日志。
    ///
    /// # 参数
    /// - `bus`：命令总线（克隆共享）。
    ///
    /// ```rust
    /// use snow_app_core::bus::CommandBus;
    /// use snow_ui_shell::dispatch::Dispatcher;
    /// let _d = Dispatcher::from_bus(CommandBus::new());
    /// ```
    pub fn from_bus(bus: CommandBus) -> Self {
        let (tx, rx) = channel::<(CommandSource, AppCommand)>();
        let spawned = std::thread::Builder::new()
            .name("shell-dispatch".into())
            .spawn(move || {
                while let Ok((source, cmd)) = rx.recv() {
                    let ctx = CommandContext::new(source);
                    if let Err(err) = bus.emit(&ctx, cmd) {
                        tracing::warn!(?source, %err, "命令派发失败");
                    }
                }
            });
        if let Err(err) = spawned {
            tracing::error!(%err, "无法创建命令派发线程，命令将被丢弃");
        }
        let tx: Mutex<Sender<(CommandSource, AppCommand)>> = Mutex::new(tx);
        Self::from_fn(move |source, cmd| {
            if let Ok(tx) = tx.lock() {
                let _ = tx.send((source, cmd));
            }
        })
    }

    /// 派发一条命令。
    ///
    /// # 参数
    /// - `source`：命令来源（热键/托盘等）。
    /// - `cmd`：命令。
    pub fn send(&self, source: CommandSource, cmd: AppCommand) {
        (self.sink)(source, cmd);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use snow_app_core::bus::CommandOutcome;
    use snow_app_core::command::CommandKind;
    use std::sync::mpsc;
    use std::time::Duration;

    /// 经总线派发：handler 收到正确来源与命令。
    #[test]
    fn bus_dispatch_carries_source() {
        let bus = CommandBus::new();
        let (tx, rx) = mpsc::channel();
        let tx = Mutex::new(tx);
        bus.register(
            CommandKind::Cancel,
            Arc::new(move |ctx, cmd| {
                let _ = tx.lock().unwrap().send((ctx.source, cmd.kind()));
                Ok(CommandOutcome::Done)
            }),
        );
        let d = Dispatcher::from_bus(bus);
        d.send(CommandSource::Hotkey, AppCommand::Cancel);
        let got = rx.recv_timeout(Duration::from_secs(2)).unwrap();
        assert_eq!(got, (CommandSource::Hotkey, CommandKind::Cancel));
    }

    /// 无 handler 时不 panic，后续命令仍可处理。
    #[test]
    fn missing_handler_does_not_kill_worker() {
        let bus = CommandBus::new();
        let d = Dispatcher::from_bus(bus.clone());
        d.send(CommandSource::Tray, AppCommand::Undo);
        let (tx, rx) = mpsc::channel();
        let tx = Mutex::new(tx);
        bus.register(
            CommandKind::Redo,
            Arc::new(move |_, _| {
                let _ = tx.lock().unwrap().send(());
                Ok(CommandOutcome::Done)
            }),
        );
        d.send(CommandSource::Tray, AppCommand::Redo);
        assert!(rx.recv_timeout(Duration::from_secs(2)).is_ok());
    }
}
