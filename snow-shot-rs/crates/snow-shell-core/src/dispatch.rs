//! 动作派发出口：托盘 / 热键后端把触发的动作负载交给它，由上层决定怎么处理。
//!
//! 动作负载 `A` 对本 crate 完全不透明：UI 进程用 `AppCommand`，托盘宿主用动作 id 字符串。

use std::sync::Arc;

/// 动作的触发来源。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ActionOrigin {
    /// 全局热键。
    Hotkey,
    /// 托盘菜单或托盘点击。
    Tray,
}

/// 动作派发器，可克隆；事件线程调用 `send` 同步执行下游函数。
pub struct Dispatcher<A> {
    /// 实际派发函数。
    sink: Arc<dyn Fn(ActionOrigin, A) + Send + Sync>,
}

impl<A> Clone for Dispatcher<A> {
    /// 克隆只增加共享引用，不要求 `A: Clone`。
    fn clone(&self) -> Self {
        Self {
            sink: Arc::clone(&self.sink),
        }
    }
}

impl<A> Dispatcher<A> {
    /// 由任意函数构造。
    ///
    /// # 参数
    /// - `f`：收到 `(来源, 动作)` 时调用，在事件线程上同步执行，应只做轻量转发。
    ///
    /// # 返回
    /// 派发器。
    ///
    /// ```rust
    /// use snow_shell_core::dispatch::{ActionOrigin, Dispatcher};
    /// let d = Dispatcher::<String>::from_fn(|_origin, _action| {});
    /// d.send(ActionOrigin::Hotkey, "capture".to_string());
    /// ```
    pub fn from_fn(f: impl Fn(ActionOrigin, A) + Send + Sync + 'static) -> Self {
        Self { sink: Arc::new(f) }
    }

    /// 派发一个动作。
    ///
    /// # 参数
    /// - `origin`：触发来源。
    /// - `action`：动作负载。
    pub fn send(&self, origin: ActionOrigin, action: A) {
        (self.sink)(origin, action);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    /// 派发携带来源与负载，克隆后的派发器共用同一出口（负载类型无需 Clone）。
    #[test]
    fn send_reaches_sink_and_clone_shares_it() {
        struct Opaque(u32);
        let seen: Arc<Mutex<Vec<(ActionOrigin, u32)>>> = Arc::default();
        let sink = Arc::clone(&seen);
        let d = Dispatcher::from_fn(move |origin, a: Opaque| {
            sink.lock().unwrap().push((origin, a.0));
        });
        let d2 = d.clone();
        d.send(ActionOrigin::Hotkey, Opaque(1));
        d2.send(ActionOrigin::Tray, Opaque(2));
        assert_eq!(
            *seen.lock().unwrap(),
            vec![(ActionOrigin::Hotkey, 1), (ActionOrigin::Tray, 2)]
        );
    }
}
