//! 全局鼠标手势状态机（纯逻辑）：按住修饰键（如 Win）再按下指定鼠标键拖动，开始一次「拖选」手势。
//!
//! 输入是已翻译的鼠标 / 取消事件，输出是要不要吞掉这次输入，以及一个拖动事件（开始 / 更新 / 结束 / 取消）。
//! 规则对齐旧版 `globalmousegesture.cpp`：同一时刻只有一个手势；匹配必须修饰键完全一致且按下时没有其它鼠标键；
//! 手势期间按下的其它鼠标键与已吞掉按键的松开也一并吞掉；多个绑定同时匹配时放弃（不猜）。

use std::collections::BTreeSet;

/// 修饰键集合。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Modifiers {
    /// Win 键。
    pub win: bool,
    /// Ctrl 键。
    pub ctrl: bool,
    /// Alt 键。
    pub alt: bool,
    /// Shift 键。
    pub shift: bool,
}

impl Modifiers {
    /// 是否一个修饰键都没按。
    pub fn is_empty(&self) -> bool {
        *self == Self::default()
    }

    /// 由配置里的 `activation_key` 名字列表解析（`windows` / `ctrl` / `alt` / `shift`）；有未知名字返回 `None`。
    ///
    /// # 参数
    /// - `names`：名字列表。
    ///
    /// ```
    /// use snow_platform::global_mouse::Modifiers;
    /// let m = Modifiers::parse(&["windows", "ctrl"]).unwrap();
    /// assert!(m.win && m.ctrl && !m.alt);
    /// assert!(Modifiers::parse(&["hyper"]).is_none());
    /// ```
    pub fn parse<S: AsRef<str>>(names: &[S]) -> Option<Self> {
        let mut out = Self::default();
        for name in names {
            match name.as_ref() {
                "windows" => out.win = true,
                "ctrl" => out.ctrl = true,
                "alt" => out.alt = true,
                "shift" => out.shift = true,
                _ => return None,
            }
        }
        Some(out)
    }
}

/// 参与手势的鼠标键。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum MouseKey {
    /// 左键。
    Left,
    /// 右键。
    Right,
    /// 中键（滚轮）。
    Middle,
    /// 侧键 1（后退）。
    Back,
    /// 侧键 2（前进）。
    Forward,
}

impl MouseKey {
    /// 由配置里的 `mouse_button` 名字解析（`left_drag` 等）；未知返回 `None`。
    ///
    /// # 参数
    /// - `text`：配置值。
    ///
    /// ```
    /// use snow_platform::global_mouse::MouseKey;
    /// assert_eq!(MouseKey::parse("wheel_drag"), Some(MouseKey::Middle));
    /// ```
    pub fn parse(text: &str) -> Option<Self> {
        Some(match text {
            "left_drag" => Self::Left,
            "right_drag" => Self::Right,
            "wheel_drag" => Self::Middle,
            "side_button_1_drag" => Self::Back,
            "side_button_2_drag" => Self::Forward,
            _ => return None,
        })
    }
}

/// 一条手势绑定：修饰键组合 + 鼠标键 → 动作标识。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Binding {
    /// 动作标识（由调用方定义，如 `screenshot_copy`）。
    pub action: String,
    /// 需要按住的修饰键（不能为空）。
    pub modifiers: Modifiers,
    /// 需要按下的鼠标键。
    pub button: MouseKey,
}

/// 屏幕坐标（物理像素）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Point {
    /// 横坐标。
    pub x: i32,
    /// 纵坐标。
    pub y: i32,
}

/// 喂给状态机的输入。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Input {
    /// 鼠标键按下。`others_held` 表示按下瞬间是否还有别的鼠标键按着。
    Press {
        /// 按下的键。
        button: MouseKey,
        /// 位置。
        pos: Point,
        /// 此刻按住的修饰键。
        modifiers: Modifiers,
        /// 是否还有其它鼠标键按着。
        others_held: bool,
    },
    /// 鼠标移动。
    Move {
        /// 位置。
        pos: Point,
    },
    /// 鼠标键松开。
    Release {
        /// 松开的键。
        button: MouseKey,
        /// 位置。
        pos: Point,
    },
    /// 外部取消（失去采集、配置变化等）。
    Cancel,
}

/// 手势产生的拖动事件。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DragEvent {
    /// 手势开始。
    Begin {
        /// 手势编号（递增）。
        id: u64,
        /// 触发的动作标识。
        action: String,
        /// 按下位置。
        pos: Point,
    },
    /// 拖动中。
    Update {
        /// 手势编号。
        id: u64,
        /// 当前位置。
        pos: Point,
    },
    /// 正常结束（松开）。
    Finish {
        /// 手势编号。
        id: u64,
        /// 松开位置。
        pos: Point,
    },
    /// 被取消。
    Cancel {
        /// 手势编号。
        id: u64,
    },
}

/// 一次输入的处理结果。
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Outcome {
    /// 是否吞掉这次输入（不再交给前台应用）。
    pub consumed: bool,
    /// 产生的拖动事件。
    pub event: Option<DragEvent>,
}

/// 全局鼠标手势状态机。
#[derive(Debug, Default)]
pub struct Gesture {
    /// 进行中的手势：`(编号, 动作, 触发键)`。
    active: Option<(u64, String, MouseKey)>,
    /// 已吞掉按下、还没见到松开的键（松开时也要吞）。
    consumed: BTreeSet<MouseKey>,
    /// 上一个手势编号。
    last_id: u64,
}

impl Gesture {
    /// 创建空状态机。
    pub fn new() -> Self {
        Self::default()
    }

    /// 是否有手势进行中。
    pub fn active(&self) -> bool {
        self.active.is_some()
    }

    /// 是否需要继续收到鼠标输入（手势进行中，或还有吞掉按下的键等着松开）。
    pub fn needs_mouse_input(&self) -> bool {
        self.active.is_some() || !self.consumed.is_empty()
    }

    /// 处理一次输入。
    ///
    /// # 参数
    /// - `input`：输入。
    /// - `bindings`：当前全部绑定。
    ///
    /// # 返回
    /// 是否吞掉输入与产生的事件。
    ///
    /// ```
    /// use snow_platform::global_mouse::{Binding, Gesture, Input, Modifiers, MouseKey, Point};
    /// let bindings = vec![Binding { action: "copy".into(), modifiers: Modifiers { win: true, ..Default::default() }, button: MouseKey::Left }];
    /// let mut g = Gesture::new();
    /// let out = g.handle(Input::Press { button: MouseKey::Left, pos: Point::default(), modifiers: bindings[0].modifiers, others_held: false }, &bindings);
    /// assert!(out.consumed && out.event.is_some());
    /// ```
    pub fn handle(&mut self, input: Input, bindings: &[Binding]) -> Outcome {
        match input {
            Input::Cancel => match self.active.take() {
                Some((id, _, _)) => Outcome { consumed: true, event: Some(DragEvent::Cancel { id }) },
                None => Outcome::default(),
            },
            Input::Release { button, pos } => {
                // 吞掉过按下的键：松开也吞；触发键松开时结束手势
                if self.consumed.remove(&button) {
                    let event = match &self.active {
                        Some((id, _, key)) if *key == button => {
                            let id = *id;
                            self.active = None;
                            Some(DragEvent::Finish { id, pos })
                        }
                        _ => None,
                    };
                    return Outcome { consumed: true, event };
                }
                Outcome::default()
            }
            Input::Move { pos } => match &self.active {
                // 光标必须继续移动，所以不吞；按键序列已被吞掉，前台应用不会把它当成拖动
                Some((id, _, _)) => Outcome { consumed: false, event: Some(DragEvent::Update { id: *id, pos }) },
                None => Outcome::default(),
            },
            Input::Press { button, pos, modifiers, others_held } => {
                if self.active.is_some() || !self.consumed.is_empty() {
                    // 手势期间的其它按下：一并吞掉，松开时再吞
                    if self.active.is_some() {
                        self.consumed.insert(button);
                        return Outcome { consumed: true, event: None };
                    }
                    return Outcome::default();
                }
                if others_held {
                    return Outcome::default();
                }
                let mut matched = bindings.iter().filter(|b| b.button == button && b.modifiers == modifiers);
                let Some(first) = matched.next() else {
                    return Outcome::default();
                };
                if matched.next().is_some() {
                    return Outcome::default();
                }
                self.last_id += 1;
                let id = self.last_id;
                self.active = Some((id, first.action.clone(), button));
                self.consumed.insert(button);
                Outcome { consumed: true, event: Some(DragEvent::Begin { id, action: first.action.clone(), pos }) }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Win 修饰键。
    const WIN: Modifiers = Modifiers { win: true, ctrl: false, alt: false, shift: false };

    /// 造一条绑定。
    fn bind(action: &str, modifiers: Modifiers, button: MouseKey) -> Binding {
        Binding { action: action.into(), modifiers, button }
    }

    /// 造一次按下。
    fn press(button: MouseKey, modifiers: Modifiers) -> Input {
        Input::Press { button, pos: Point { x: 1, y: 2 }, modifiers, others_held: false }
    }

    /// 完整手势：按下吞掉并开始，移动给出更新但不吞，松开吞掉并结束。
    #[test]
    fn full_drag_lifecycle() {
        let bindings = vec![bind("copy", WIN, MouseKey::Left)];
        let mut g = Gesture::new();
        let begin = g.handle(press(MouseKey::Left, WIN), &bindings);
        assert!(begin.consumed);
        assert!(matches!(begin.event, Some(DragEvent::Begin { id: 1, ref action, .. }) if action == "copy"));
        assert!(g.active() && g.needs_mouse_input());
        let moved = g.handle(Input::Move { pos: Point { x: 9, y: 9 } }, &bindings);
        assert!(!moved.consumed);
        assert_eq!(moved.event, Some(DragEvent::Update { id: 1, pos: Point { x: 9, y: 9 } }));
        let done = g.handle(Input::Release { button: MouseKey::Left, pos: Point { x: 20, y: 30 } }, &bindings);
        assert!(done.consumed);
        assert_eq!(done.event, Some(DragEvent::Finish { id: 1, pos: Point { x: 20, y: 30 } }));
        assert!(!g.active() && !g.needs_mouse_input());
    }

    /// 修饰键必须完全一致；多按了别的键或没按都不触发。
    #[test]
    fn modifiers_must_match_exactly() {
        let bindings = vec![bind("copy", WIN, MouseKey::Left)];
        let mut g = Gesture::new();
        assert_eq!(g.handle(press(MouseKey::Left, Modifiers::default()), &bindings), Outcome::default());
        let extra = Modifiers { ctrl: true, ..WIN };
        assert_eq!(g.handle(press(MouseKey::Left, extra), &bindings), Outcome::default());
        assert_eq!(g.handle(press(MouseKey::Right, WIN), &bindings), Outcome::default());
    }

    /// 按下时已有别的鼠标键按着：不触发；重复匹配的绑定放弃。
    #[test]
    fn ambiguity_and_chords_are_ignored() {
        let bindings = vec![bind("a", WIN, MouseKey::Left), bind("b", WIN, MouseKey::Left)];
        let mut g = Gesture::new();
        assert_eq!(g.handle(press(MouseKey::Left, WIN), &bindings), Outcome::default());
        let single = vec![bind("a", WIN, MouseKey::Left)];
        let held = Input::Press { button: MouseKey::Left, pos: Point::default(), modifiers: WIN, others_held: true };
        assert_eq!(g.handle(held, &single), Outcome::default());
    }

    /// 手势期间的其它按键按下 / 松开都被吞掉；它们的松开不会结束手势。
    #[test]
    fn other_buttons_during_gesture_are_swallowed() {
        let bindings = vec![bind("copy", WIN, MouseKey::Left)];
        let mut g = Gesture::new();
        g.handle(press(MouseKey::Left, WIN), &bindings);
        let right = g.handle(press(MouseKey::Right, Modifiers::default()), &bindings);
        assert!(right.consumed && right.event.is_none());
        let right_up = g.handle(Input::Release { button: MouseKey::Right, pos: Point::default() }, &bindings);
        assert!(right_up.consumed && right_up.event.is_none());
        assert!(g.active(), "右键松开不结束手势");
    }

    /// 取消：进行中的手势产生 Cancel 事件；之后触发键的松开仍被吞掉但不再结束。
    #[test]
    fn cancel_then_release_is_swallowed() {
        let bindings = vec![bind("copy", WIN, MouseKey::Left)];
        let mut g = Gesture::new();
        g.handle(press(MouseKey::Left, WIN), &bindings);
        let cancel = g.handle(Input::Cancel, &bindings);
        assert_eq!(cancel.event, Some(DragEvent::Cancel { id: 1 }));
        assert!(!g.active());
        let up = g.handle(Input::Release { button: MouseKey::Left, pos: Point::default() }, &bindings);
        assert!(up.consumed && up.event.is_none());
        assert!(!g.needs_mouse_input());
    }

    /// 配置名解析。
    #[test]
    fn config_names_parse() {
        assert_eq!(Modifiers::parse::<&str>(&[]), Some(Modifiers::default()));
        assert_eq!(MouseKey::parse("side_button_2_drag"), Some(MouseKey::Forward));
        assert_eq!(MouseKey::parse("click"), None);
    }
}
