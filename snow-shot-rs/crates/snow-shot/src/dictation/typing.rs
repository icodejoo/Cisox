//! 键入：把“已键入文本 → 期望文本”的差异变成按键序列（稳定前缀法）。
//!
//! 公共前缀保持不动，只对不同的尾部先退格再重打。差异计算、回删上限、目标窗口校验、
//! 被修饰键挡住时的延后，全是纯逻辑，通过 [`KeySink`] 与系统隔开，可离屏单测。
//! 退格按 Unicode 标量值计数：组合字符、ZWJ 表情序列在多数输入框里一次退格删得更多，这是已知偏差。

use snow_platform::text_inject::KeyStroke;

/// 未落定（PARTIAL）修正时最多回删多少个字符；超出的差异等 FINAL 再一次性修正。
pub const MAX_PARTIAL_BACKSPACES: usize = 16;

/// 一次编辑：先退格 `backspaces` 次，再键入 `text`。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EditPlan {
    /// 退格次数（字符数）。
    pub backspaces: usize,
    /// 退格之后要键入的文本。
    pub text: String,
}

impl EditPlan {
    /// 是否什么都不用做。
    pub fn is_empty(&self) -> bool {
        self.backspaces == 0 && self.text.is_empty()
    }

    /// 展开成按键序列（退格在前，字符在后）。
    pub fn strokes(&self) -> Vec<KeyStroke> {
        let mut out = vec![KeyStroke::Backspace; self.backspaces];
        out.extend(self.text.chars().map(KeyStroke::Char));
        out
    }
}

/// 计算把 `typed` 改成 `desired` 的最小尾部编辑：保持公共前缀，其余回删重打。
///
/// # 参数
/// - `typed`：已经键入的文本。
/// - `desired`：期望的文本。
///
/// # 返回
/// 编辑方案；两者相同时为空方案。
///
/// ```ignore
/// let plan = plan_edit("你好世", "你好世界");
/// assert_eq!((plan.backspaces, plan.text.as_str()), (0, "界"));
/// ```
pub fn plan_edit(typed: &str, desired: &str) -> EditPlan {
    let common = typed
        .chars()
        .zip(desired.chars())
        .take_while(|(a, b)| a == b)
        .count();
    let backspaces = typed.chars().count() - common;
    let text: String = desired.chars().skip(common).collect();
    EditPlan { backspaces, text }
}

/// 键入通道此刻的状态。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SinkState {
    /// 可以发送；携带当前前台窗口。
    Ready {
        /// 前台窗口句柄（数值）。
        window: isize,
    },
    /// 暂时不能发（修饰键被按住，或瞬时没有前台窗口），稍后重试。
    Busy,
}

/// 键入通道：真实实现走 `SendInput`，测试用 Fake。
pub trait KeySink {
    /// 当前能否发送。
    fn state(&mut self) -> SinkState;

    /// 一次性发送一批按键；失败返回原因。
    fn send(&mut self, strokes: &[KeyStroke]) -> Result<(), String>;
}

/// 系统键入通道（`SendInput`）。
#[derive(Debug, Default)]
pub struct SystemKeySink;

impl KeySink for SystemKeySink {
    /// 修饰键被按住或没有前台窗口时为 `Busy`。
    fn state(&mut self) -> SinkState {
        if snow_platform::text_inject::physical_modifiers_held() {
            return SinkState::Busy;
        }
        match snow_platform::text_inject::foreground_window() {
            Some(window) => SinkState::Ready { window },
            None => SinkState::Busy,
        }
    }

    /// 用一次 `SendInput` 发出。
    fn send(&mut self, strokes: &[KeyStroke]) -> Result<(), String> {
        snow_platform::text_inject::send_strokes(strokes)
    }
}

/// 一次同步的结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SyncOutcome {
    /// 已键入内容与期望一致（含本次刚发完）。
    Synced,
    /// 通道暂忙，稍后再同步。
    Deferred,
    /// 未落定修正需要回删过多，先不动，等落定再改。
    Skipped,
    /// 前台窗口与首次键入时不同，已停止键入。
    TargetChanged,
    /// 发送失败。
    Failed(String),
}

/// 键入状态：记录已键入的文本与目标窗口。
#[derive(Debug, Default)]
pub struct Typer {
    /// 已键入到目标里的文本。
    typed: String,
    /// 首次键入时的前台窗口；之后前台变化视为目标丢失。
    target: Option<isize>,
}

impl Typer {
    /// 已键入的文本。
    pub fn typed(&self) -> &str {
        &self.typed
    }

    /// 把已键入内容修正到 `desired`。
    ///
    /// # 参数
    /// - `desired`：期望文本（已落定 + 未落定）。
    /// - `settle`：为真表示落定/收尾，不限制回删数量；为假（未落定修正）时回删超过
    ///   [`MAX_PARTIAL_BACKSPACES`] 就跳过。
    /// - `sink`：键入通道。
    pub fn sync(&mut self, desired: &str, settle: bool, sink: &mut dyn KeySink) -> SyncOutcome {
        let plan = plan_edit(&self.typed, desired);
        if plan.is_empty() {
            return SyncOutcome::Synced;
        }
        if !settle && plan.backspaces > MAX_PARTIAL_BACKSPACES {
            return SyncOutcome::Skipped;
        }
        let window = match sink.state() {
            SinkState::Busy => return SyncOutcome::Deferred,
            SinkState::Ready { window } => window,
        };
        match self.target {
            Some(target) if target != window => return SyncOutcome::TargetChanged,
            _ => self.target = Some(window),
        }
        match sink.send(&plan.strokes()) {
            Ok(()) => {
                self.typed = desired.to_string();
                SyncOutcome::Synced
            }
            Err(reason) => SyncOutcome::Failed(reason),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 测试用通道：可控状态，记录发送内容。
    struct FakeSink {
        state: SinkState,
        sent: Vec<Vec<KeyStroke>>,
        fail: Option<String>,
    }

    impl FakeSink {
        fn ready(window: isize) -> Self {
            Self {
                state: SinkState::Ready { window },
                sent: Vec::new(),
                fail: None,
            }
        }
    }

    impl KeySink for FakeSink {
        fn state(&mut self) -> SinkState {
            self.state
        }
        fn send(&mut self, strokes: &[KeyStroke]) -> Result<(), String> {
            if let Some(reason) = &self.fail {
                return Err(reason.clone());
            }
            self.sent.push(strokes.to_vec());
            Ok(())
        }
    }

    /// 把按键序列还原成文本，便于断言（退格删掉前一个字符）。
    fn replay(base: &str, strokes: &[KeyStroke]) -> String {
        let mut out: Vec<char> = base.chars().collect();
        for s in strokes {
            match s {
                KeyStroke::Backspace => {
                    out.pop();
                }
                KeyStroke::Char(c) => out.push(*c),
            }
        }
        out.into_iter().collect()
    }

    /// 纯追加：不退格，只打新增尾部。
    #[test]
    fn append_only() {
        let plan = plan_edit("你好", "你好世界");
        assert_eq!((plan.backspaces, plan.text.as_str()), (0, "世界"));
    }

    /// 尾部被修正：保持公共前缀，只回删不同的尾部再重打。
    #[test]
    fn tail_correction_keeps_common_prefix() {
        let plan = plan_edit("今天天汽很好", "今天天气很好");
        assert_eq!(plan.backspaces, 3);
        assert_eq!(plan.text, "气很好");
        assert_eq!(replay("今天天汽很好", &plan.strokes()), "今天天气很好");
    }

    /// 缩短与清空：只退格。完全相同：空方案。
    #[test]
    fn shrink_and_equal() {
        let plan = plan_edit("abcd", "ab");
        assert_eq!((plan.backspaces, plan.text.as_str()), (2, ""));
        assert!(plan_edit("same", "same").is_empty());
        let wipe = plan_edit("abc", "");
        assert_eq!(wipe.backspaces, 3);
    }

    /// 从零开始键入；表情按标量值计一个字符。
    #[test]
    fn from_empty_and_emoji() {
        let plan = plan_edit("", "hi😀");
        assert_eq!((plan.backspaces, plan.text.as_str()), (0, "hi😀"));
        let plan = plan_edit("a😀", "a");
        assert_eq!(plan.backspaces, 1);
    }

    /// 按键序列顺序：退格在前、字符在后。
    #[test]
    fn strokes_order() {
        let strokes = EditPlan {
            backspaces: 2,
            text: "ab".into(),
        }
        .strokes();
        assert_eq!(
            strokes,
            vec![
                KeyStroke::Backspace,
                KeyStroke::Backspace,
                KeyStroke::Char('a'),
                KeyStroke::Char('b')
            ]
        );
    }

    /// 同步：追加与修正各用一次发送；已一致时不发送。
    #[test]
    fn sync_sends_one_batch_each_time() {
        let mut typer = Typer::default();
        let mut sink = FakeSink::ready(7);
        assert_eq!(
            typer.sync("今天天汽", false, &mut sink),
            SyncOutcome::Synced
        );
        assert_eq!(
            typer.sync("今天天气", false, &mut sink),
            SyncOutcome::Synced
        );
        assert_eq!(
            typer.sync("今天天气", false, &mut sink),
            SyncOutcome::Synced
        );
        assert_eq!(sink.sent.len(), 2);
        assert_eq!(typer.typed(), "今天天气");
        assert_eq!(replay("今天天汽", &sink.sent[1]), "今天天气");
    }

    /// 通道忙（修饰键被按住）时延后，状态不变；放开后一次补齐。
    #[test]
    fn busy_defers_then_catches_up() {
        let mut typer = Typer::default();
        let mut sink = FakeSink::ready(1);
        sink.state = SinkState::Busy;
        assert_eq!(typer.sync("hello", false, &mut sink), SyncOutcome::Deferred);
        assert_eq!(typer.typed(), "");
        sink.state = SinkState::Ready { window: 1 };
        assert_eq!(
            typer.sync("hello world", true, &mut sink),
            SyncOutcome::Synced
        );
        assert_eq!(typer.typed(), "hello world");
        assert_eq!(sink.sent.len(), 1);
    }

    /// 未落定修正回删过多时跳过；落定（settle）时照改。
    #[test]
    fn partial_backspace_cap() {
        let mut typer = Typer::default();
        let mut sink = FakeSink::ready(1);
        let long = "一二三四五六七八九十一二三四五六七八";
        assert_eq!(typer.sync(long, true, &mut sink), SyncOutcome::Synced);
        assert_eq!(
            typer.sync("完全不同", false, &mut sink),
            SyncOutcome::Skipped
        );
        assert_eq!(typer.typed(), long);
        assert_eq!(typer.sync("完全不同", true, &mut sink), SyncOutcome::Synced);
        assert_eq!(typer.typed(), "完全不同");
    }

    /// 前台窗口变了就停止键入，不往新窗口里乱打也不乱删。
    #[test]
    fn target_change_stops_typing() {
        let mut typer = Typer::default();
        let mut sink = FakeSink::ready(1);
        assert_eq!(typer.sync("abc", false, &mut sink), SyncOutcome::Synced);
        sink.state = SinkState::Ready { window: 2 };
        assert_eq!(
            typer.sync("abcd", false, &mut sink),
            SyncOutcome::TargetChanged
        );
        assert_eq!(typer.typed(), "abc");
        assert_eq!(sink.sent.len(), 1);
    }

    /// 发送失败：保持已键入状态并上报原因。
    #[test]
    fn send_failure_reported() {
        let mut typer = Typer::default();
        let mut sink = FakeSink::ready(1);
        sink.fail = Some("blocked".into());
        assert_eq!(
            typer.sync("x", false, &mut sink),
            SyncOutcome::Failed("blocked".into())
        );
        assert_eq!(typer.typed(), "");
    }
}
