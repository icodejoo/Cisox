//! 鼠标手势在宿主侧的纯逻辑：快照里的绑定还原、钩子事件转消息、`update` 事件节流（方案 §2、§9）。
//!
//! 钩子本身（`snow_platform::global_mouse`，低级键盘 / 鼠标钩子，独立线程）只在快照里有手势绑定时才装，
//! 宿主进程声明了 PerMonitorV2（见 [`crate::harden`]），所以钩子坐标是虚拟桌面物理像素，不被 DPI 虚拟化。
//! 钩子线程里不得做任何重活，更不能碰 IME / GPUI：回调只做“转成 [`GestureEvent`]、过一遍节流、投进事件通道”。

use crate::queue::Outbound;
use snow_host_protocol::message::GesturePhase;
use snow_host_protocol::spec::MouseBindingSpec;
use snow_platform::global_mouse::{Binding, DragEvent, Modifiers, MouseKey};
use std::time::{Duration, Instant};

/// 钩子线程投给宿主主线程的一个手势事件（坐标为虚拟桌面物理像素）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GestureEvent {
    /// 阶段。
    pub phase: GesturePhase,
    /// 手势编号（同一次拖动内不变）。
    pub id: u64,
    /// 手势动作标识（只有 `begin` 带，如 `screenshot_copy`）。
    pub action: Option<String>,
    /// 横坐标（`cancel` 没有位置，为 0）。
    pub x: i32,
    /// 纵坐标（`cancel` 没有位置，为 0）。
    pub y: i32,
}

impl GestureEvent {
    /// 转成要发给 UI 的消息。
    pub fn into_outbound(self) -> Outbound {
        Outbound::Gesture {
            phase: self.phase,
            id: self.id,
            action: self.action,
            x: self.x,
            y: self.y,
        }
    }
}

/// 钩子的拖动事件转成宿主事件。
///
/// # 参数
/// - `event`：`snow_platform::global_mouse` 产生的拖动事件。
///
/// ```rust
/// use snow_platform::global_mouse::{DragEvent, Point};
/// use snow_tray_host::gesture::event_from_drag;
/// let begin = event_from_drag(&DragEvent::Begin {
///     id: 3, action: "screenshot_copy".into(), pos: Point { x: 10, y: -5 },
/// });
/// assert_eq!((begin.id, begin.x, begin.y), (3, 10, -5));
/// assert_eq!(begin.action.as_deref(), Some("screenshot_copy"));
/// ```
pub fn event_from_drag(event: &DragEvent) -> GestureEvent {
    match event {
        DragEvent::Begin { id, action, pos } => GestureEvent {
            phase: GesturePhase::Begin,
            id: *id,
            action: Some(action.clone()),
            x: pos.x,
            y: pos.y,
        },
        DragEvent::Update { id, pos } => GestureEvent {
            phase: GesturePhase::Update,
            id: *id,
            action: None,
            x: pos.x,
            y: pos.y,
        },
        DragEvent::Finish { id, pos } => GestureEvent {
            phase: GesturePhase::Finish,
            id: *id,
            action: None,
            x: pos.x,
            y: pos.y,
        },
        DragEvent::Cancel { id } => GestureEvent {
            phase: GesturePhase::Cancel,
            id: *id,
            action: None,
            x: 0,
            y: 0,
        },
    }
}

/// 把快照里的手势绑定还原成钩子要的绑定；名字不认识、没有修饰键的项被丢弃
/// （没有修饰键会劫持普通鼠标操作，与 UI 单进程模式的解析一致）。
///
/// # 参数
/// - `specs`：快照里的 `mouse` 列表。
///
/// # 返回
/// 有效的绑定（保持顺序）。
///
/// ```rust
/// use snow_host_protocol::spec::MouseBindingSpec;
/// use snow_tray_host::gesture::bindings_from_spec;
/// let specs = vec![MouseBindingSpec {
///     action: "screenshot_copy".into(),
///     modifiers: vec!["windows".into()],
///     button: "left_drag".into(),
/// }];
/// assert_eq!(bindings_from_spec(&specs).len(), 1);
/// ```
pub fn bindings_from_spec(specs: &[MouseBindingSpec]) -> Vec<Binding> {
    specs
        .iter()
        .filter_map(|spec| {
            let modifiers = Modifiers::parse(&spec.modifiers)?;
            if modifiers.is_empty() {
                return None;
            }
            Some(Binding {
                action: spec.action.clone(),
                modifiers,
                button: MouseKey::parse(&spec.button)?,
            })
        })
        .collect()
}

/// 手势 `update` 事件的节流器：同一手势内两次转发的间隔不小于 `min_interval`。
///
/// 钩子在拖动时每次鼠标移动都会产生一个 `update`（高回报率鼠标可到 1000Hz），转给 UI 用不着这么密；
/// 被丢掉的 `update` 不影响结果，因为下一条 `update` 或 `finish` 总带着最新位置。
/// `begin / finish / cancel` 永远放行。纯逻辑，时间由调用方传入。
#[derive(Debug)]
pub struct GestureThrottle {
    /// 最小转发间隔。
    min_interval: Duration,
    /// 最近一次放行的 `update`：`(手势编号, 时刻)`。
    last_update: Option<(u64, Instant)>,
}

impl GestureThrottle {
    /// 新建。
    ///
    /// # 参数
    /// - `min_interval`：`update` 的最小转发间隔（约 120Hz 取 8ms）。
    pub fn new(min_interval: Duration) -> Self {
        Self {
            min_interval,
            last_update: None,
        }
    }

    /// 这个事件要不要转发。
    ///
    /// # 参数
    /// - `now`：当前时刻。
    /// - `event`：钩子事件。
    ///
    /// # 返回
    /// 放行为 `true`；被节流的 `update` 为 `false`。
    ///
    /// ```rust
    /// use snow_host_protocol::message::GesturePhase;
    /// use snow_tray_host::gesture::{GestureEvent, GestureThrottle};
    /// use std::time::{Duration, Instant};
    /// let mut throttle = GestureThrottle::new(Duration::from_millis(8));
    /// let t0 = Instant::now();
    /// let update = GestureEvent { phase: GesturePhase::Update, id: 1, action: None, x: 0, y: 0 };
    /// assert!(throttle.admit(t0, &update));
    /// assert!(!throttle.admit(t0 + Duration::from_millis(3), &update));
    /// assert!(throttle.admit(t0 + Duration::from_millis(8), &update));
    /// ```
    pub fn admit(&mut self, now: Instant, event: &GestureEvent) -> bool {
        match event.phase {
            GesturePhase::Begin | GesturePhase::Finish | GesturePhase::Cancel => {
                self.last_update = None;
                true
            }
            GesturePhase::Update => {
                let due = match self.last_update {
                    Some((id, at)) if id == event.id => {
                        now.saturating_duration_since(at) >= self.min_interval
                    }
                    _ => true,
                };
                if due {
                    self.last_update = Some((event.id, now));
                }
                due
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use snow_platform::global_mouse::Point;

    /// 构造手势事件。
    fn event(phase: GesturePhase, id: u64, x: i32) -> GestureEvent {
        GestureEvent {
            phase,
            id,
            action: None,
            x,
            y: 0,
        }
    }

    /// 钩子事件逐种转换：begin 带动作与位置，update / finish 带位置，cancel 没有位置。
    #[test]
    fn drag_events_convert_to_gesture_events() {
        let begin = event_from_drag(&DragEvent::Begin {
            id: 1,
            action: "screenshot_ocr".into(),
            pos: Point { x: -30, y: 40 },
        });
        assert_eq!(begin.phase, GesturePhase::Begin);
        assert_eq!(begin.action.as_deref(), Some("screenshot_ocr"));
        assert_eq!((begin.x, begin.y), (-30, 40));
        let update = event_from_drag(&DragEvent::Update {
            id: 1,
            pos: Point { x: 1, y: 2 },
        });
        assert_eq!(update, event(GesturePhase::Update, 1, 1).with_y(2));
        let finish = event_from_drag(&DragEvent::Finish {
            id: 1,
            pos: Point { x: 7, y: 8 },
        });
        assert_eq!(
            (finish.phase, finish.x, finish.y),
            (GesturePhase::Finish, 7, 8)
        );
        let cancel = event_from_drag(&DragEvent::Cancel { id: 1 });
        assert_eq!(
            (cancel.phase, cancel.x, cancel.y),
            (GesturePhase::Cancel, 0, 0)
        );
        assert_eq!(cancel.action, None);
    }

    impl GestureEvent {
        /// 测试里改纵坐标的小助手。
        fn with_y(mut self, y: i32) -> Self {
            self.y = y;
            self
        }
    }

    /// 转成发给 UI 的消息：字段原样带出（编码往返由协议 crate 测）。
    #[test]
    fn gesture_event_becomes_outbound() {
        let mut begin = event(GesturePhase::Begin, 5, 9);
        begin.action = Some("screen_recording".into());
        assert_eq!(
            begin.into_outbound(),
            Outbound::Gesture {
                phase: GesturePhase::Begin,
                id: 5,
                action: Some("screen_recording".into()),
                x: 9,
                y: 0,
            }
        );
    }

    /// 快照绑定还原：有效项保留；未知修饰键、没有修饰键、未知按键的项丢弃。
    #[test]
    fn bindings_restore_from_spec_and_skip_invalid() {
        let spec = |action: &str, mods: &[&str], button: &str| MouseBindingSpec {
            action: action.into(),
            modifiers: mods.iter().map(|m| m.to_string()).collect(),
            button: button.into(),
        };
        let bindings = bindings_from_spec(&[
            spec("screenshot_copy", &["windows"], "left_drag"),
            spec("screenshot_ocr", &["ctrl", "alt"], "side_button_1_drag"),
            spec("bad_mod", &["hyper"], "left_drag"),
            spec("no_mod", &[], "left_drag"),
            spec("bad_button", &["alt"], "double"),
        ]);
        assert_eq!(bindings.len(), 2);
        assert_eq!(bindings[0].action, "screenshot_copy");
        assert!(bindings[0].modifiers.win);
        assert_eq!(bindings[0].button, MouseKey::Left);
        assert!(bindings[1].modifiers.ctrl && bindings[1].modifiers.alt);
        assert_eq!(bindings[1].button, MouseKey::Back);
        assert!(bindings_from_spec(&[]).is_empty());
    }

    /// 节流：同一手势内 update 至少隔 8ms 才放行；begin / finish / cancel 永远放行并重置。
    #[test]
    fn throttle_limits_updates_to_the_interval() {
        let interval = Duration::from_millis(8);
        let mut throttle = GestureThrottle::new(interval);
        let t0 = Instant::now();
        assert!(throttle.admit(t0, &event(GesturePhase::Begin, 1, 0)));
        assert!(throttle.admit(t0, &event(GesturePhase::Update, 1, 1)));
        // 1000Hz 的鼠标：1ms 一条，只有每 8ms 的那条放行
        let admitted = (1..=40u64)
            .filter(|ms| {
                throttle.admit(
                    t0 + Duration::from_millis(*ms),
                    &event(GesturePhase::Update, 1, *ms as i32),
                )
            })
            .count();
        assert_eq!(admitted, 5, "40ms 内约 120Hz：8/16/24/32/40ms 各一条");
        assert!(throttle.admit(
            t0 + Duration::from_millis(41),
            &event(GesturePhase::Finish, 1, 9)
        ));
    }

    /// 新手势的第一条 update 不受上一个手势的节流影响；cancel 同样重置。
    #[test]
    fn throttle_resets_between_gestures() {
        let mut throttle = GestureThrottle::new(Duration::from_millis(8));
        let t0 = Instant::now();
        assert!(throttle.admit(t0, &event(GesturePhase::Update, 1, 0)));
        assert!(!throttle.admit(t0, &event(GesturePhase::Update, 1, 1)));
        // 另一个手势编号：立刻放行
        assert!(throttle.admit(t0, &event(GesturePhase::Update, 2, 0)));
        assert!(throttle.admit(t0, &event(GesturePhase::Cancel, 2, 0)));
        assert!(
            throttle.admit(t0, &event(GesturePhase::Update, 2, 5)),
            "cancel 之后重新计时"
        );
    }

    /// 时刻倒退（后一条比前一条还早）不 panic：差值按 0 算，仍在间隔内，被节流。
    #[test]
    fn throttle_tolerates_non_monotonic_input() {
        let mut throttle = GestureThrottle::new(Duration::from_millis(8));
        let t0 = Instant::now() + Duration::from_secs(1);
        assert!(throttle.admit(t0, &event(GesturePhase::Update, 1, 0)));
        assert!(!throttle.admit(
            t0 - Duration::from_millis(500),
            &event(GesturePhase::Update, 1, 1)
        ));
    }
}
