//! 连续误触发保护的判定（纯逻辑，时间由调用方传入）。
//!
//! 背景：截图覆盖窗会挡住用户的所有操作，一旦热键 / 鼠标手势因为各种原因（漏收按键抬起事件之类）
//! 被连环误触发，用户几乎无法自救。宿主在把截图类命令转给 UI 之前过一遍这里：
//! 记录**每次覆盖窗关闭 → 下一次截图开启**之间的间隔，**连续 [`RAPID_REOPEN_STREAK`] 次**间隔都
//! 小于 [`RAPID_REOPEN_GAP`] 就判定为异常，不分发，改为弹系统提示问是不是刻意的；任何一次间隔
//! 不小于 [`RAPID_REOPEN_GAP`] 就把连续计数清零。
//!
//! “关闭”时刻来自 UI 上报的 `capture_idle`（采集、倒计时、覆盖窗都没有了）或 UI 进程退出。
//! 覆盖窗还开着时又来的触发没有“关闭 → 开启”间隔，放行但不计数。

use snow_host_protocol::CAPTURE_ACTION_IDS;
use std::time::{Duration, Instant};

/// 判定异常的间隔上限：覆盖窗关闭到下一次截图开启不足这么久算“太快”（恰好等于不算）。
pub const RAPID_REOPEN_GAP: Duration = Duration::from_secs(5);

/// 连续多少次“太快”的间隔判定为异常。
pub const RAPID_REOPEN_STREAK: u32 = 5;

/// 用户确认是刻意操作后的冷却：这段时间内不再检测。
pub const RAPID_COOLDOWN: Duration = Duration::from_secs(10 * 60);

/// `MessageBoxW` 里“否”的返回值（`IDNO`）；只有它才让宿主退出，出错（0）等一律当作放行，避免误杀。
pub const MESSAGE_BOX_ID_NO: i32 = 7;

/// 动作 id 是否会打开全屏覆盖窗（计入连续误触发保护）。
///
/// # 参数
/// - `action_id`：宿主收到的动作 id。
///
/// ```rust
/// use snow_tray_host::rapid::opens_capture_overlay;
/// assert!(opens_capture_overlay("capture"));
/// assert!(opens_capture_overlay("quick.screenshot_copy"));
/// assert!(!opens_capture_overlay("settings"));
/// assert!(!opens_capture_overlay("quick.screenshot_full_screen"));
/// ```
pub fn opens_capture_overlay(action_id: &str) -> bool {
    CAPTURE_ACTION_IDS.contains(&action_id)
}

/// 确认框的返回值是否表示“否，退出”。
///
/// # 参数
/// - `code`：`MessageBoxW` 的返回值。
///
/// ```rust
/// use snow_tray_host::rapid::quit_chosen;
/// assert!(quit_chosen(7));
/// assert!(!quit_chosen(6));
/// assert!(!quit_chosen(0));
/// ```
pub fn quit_chosen(code: i32) -> bool {
    code == MESSAGE_BOX_ID_NO
}

/// 对一次截图类命令到达的判定。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RapidVerdict {
    /// 放行：照常分发给 UI。
    Allow,
    /// 询问：本次不分发，弹提示问用户是不是刻意的。
    Ask {
        /// 已经连续“太快”启动的次数（用于提示文案）。
        count: u32,
    },
    /// 丢弃：提示框还开着，新命令直接扔掉，不排队。
    Drop,
}

/// 连续误触发保护的状态。
#[derive(Debug, Default)]
pub struct RapidGuard {
    /// 覆盖窗会话是否还开着（最近一次放行的命令之后还没收到“关闭”）。
    open: bool,
    /// 最近一次关闭的时刻；`None` 表示还没有可用的关闭记录。
    last_close: Option<Instant>,
    /// 连续“太快”的间隔数。
    streak: u32,
    /// 提示框是否正开着（选“否”后也保持，直到宿主退出，期间新命令全部丢弃）。
    prompting: bool,
    /// 冷却截止时刻；之前不计数。
    quiet_until: Option<Instant>,
}

impl RapidGuard {
    /// 新建（无历史、无冷却）。
    pub fn new() -> Self {
        Self::default()
    }

    /// 一次截图类命令到达（分发给 UI 之前）。
    ///
    /// # 参数
    /// - `now`：当前时刻。
    ///
    /// # 返回
    /// 判定；返回 [`RapidVerdict::Ask`] 时命令没有被放行，调用方弹提示，并在用户回答后调用
    /// [`answer_intentional`](Self::answer_intentional) 或 [`answer_quit`](Self::answer_quit)。
    ///
    /// ```rust
    /// use snow_tray_host::rapid::{RapidGuard, RapidVerdict};
    /// use std::time::Instant;
    /// let mut guard = RapidGuard::new();
    /// assert_eq!(guard.on_open(Instant::now()), RapidVerdict::Allow);
    /// ```
    pub fn on_open(&mut self, now: Instant) -> RapidVerdict {
        if self.prompting {
            return RapidVerdict::Drop;
        }
        if let Some(until) = self.quiet_until {
            if now < until {
                // 冷却中：只跟踪开关状态，不计数
                self.open = true;
                return RapidVerdict::Allow;
            }
            // 冷却结束：从零重新计数，冷却期内的关闭记录不再算数
            self.quiet_until = None;
            self.streak = 0;
            self.last_close = None;
        }
        if self.open {
            return RapidVerdict::Allow;
        }
        if let Some(closed) = self.last_close {
            if now.saturating_duration_since(closed) < RAPID_REOPEN_GAP {
                self.streak += 1;
            } else {
                self.streak = 0;
            }
        }
        if self.streak >= RAPID_REOPEN_STREAK {
            let count = self.streak;
            self.streak = 0;
            self.prompting = true;
            return RapidVerdict::Ask { count };
        }
        self.open = true;
        RapidVerdict::Allow
    }

    /// 覆盖窗会话关闭了（UI 上报 `capture_idle`，或 UI 进程退出）；没有放行过的会话在开着时忽略。
    ///
    /// # 参数
    /// - `now`：当前时刻。
    pub fn on_closed(&mut self, now: Instant) {
        if self.open {
            self.open = false;
            self.last_close = Some(now);
        }
    }

    /// 用户答“是，刻意的”：进入冷却，连续计数清零。
    ///
    /// # 参数
    /// - `now`：当前时刻。
    /// - `dispatched`：被扣下的那条命令是否随后会分发（鼠标手势 `begin` 已经过期，不再分发）。
    pub fn answer_intentional(&mut self, now: Instant, dispatched: bool) {
        self.prompting = false;
        self.streak = 0;
        self.quiet_until = Some(now + RAPID_COOLDOWN);
        self.open = dispatched;
    }

    /// 用户答“否”：宿主即将退出，提示状态保持，之后到达的截图类命令一律丢弃。
    pub fn answer_quit(&mut self) {
        self.prompting = true;
    }

    /// 提示框是否开着（含选“否”后等待退出的阶段）。
    pub fn is_prompting(&self) -> bool {
        self.prompting
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// n 秒。
    fn secs(n: u64) -> Duration {
        Duration::from_secs(n)
    }

    /// 模拟一轮“开启 → 关闭”：在 `t` 开启，持续 `held` 后关闭；返回判定与关闭时刻。
    fn cycle(guard: &mut RapidGuard, t: Instant, held: Duration) -> (RapidVerdict, Instant) {
        let verdict = guard.on_open(t);
        let closed = t + held;
        guard.on_closed(closed);
        (verdict, closed)
    }

    /// 造一个已进入提示状态的保护：返回它与询问发生的时刻。
    fn asked_guard() -> (RapidGuard, Instant) {
        let mut guard = RapidGuard::new();
        let mut t = Instant::now();
        for _ in 0..=RAPID_REOPEN_STREAK {
            let (verdict, closed) = cycle(&mut guard, t, secs(1));
            if matches!(verdict, RapidVerdict::Ask { .. }) {
                return (guard, t);
            }
            t = closed + secs(1);
        }
        unreachable!("连续短间隔必然触发询问");
    }

    /// 间隔都小于 5 秒：前 5 次开启放行，第 6 次（第 5 个短间隔）才询问；次数为连续阈值。
    #[test]
    fn asks_on_fifth_consecutive_short_gap() {
        let mut guard = RapidGuard::new();
        let mut t = Instant::now();
        for i in 0..RAPID_REOPEN_STREAK {
            let (verdict, closed) = cycle(&mut guard, t, secs(1));
            assert_eq!(verdict, RapidVerdict::Allow, "第 {i} 次开启应放行");
            t = closed + secs(2);
        }
        assert_eq!(
            guard.on_open(t),
            RapidVerdict::Ask {
                count: RAPID_REOPEN_STREAK
            }
        );
        assert!(guard.is_prompting());
    }

    /// 中间有一次间隔不小于 5 秒就清零：之后要重新连续 5 个短间隔才询问。
    #[test]
    fn long_gap_resets_streak() {
        let mut guard = RapidGuard::new();
        let mut t = Instant::now();
        // 先累积 4 个短间隔（5 次开启）
        let mut closed = t;
        for _ in 0..RAPID_REOPEN_STREAK {
            let (_, c) = cycle(&mut guard, t, secs(1));
            closed = c;
            t = closed + secs(2);
        }
        // 这一次间隔 6 秒：清零，放行
        t = closed + secs(6);
        let (verdict, closed) = cycle(&mut guard, t, secs(1));
        assert_eq!(verdict, RapidVerdict::Allow);
        assert_eq!(guard.streak, 0);
        // 清零后再来 4 个短间隔仍不询问
        t = closed + secs(2);
        for _ in 0..RAPID_REOPEN_STREAK - 1 {
            let (verdict, closed) = cycle(&mut guard, t, secs(1));
            assert_eq!(verdict, RapidVerdict::Allow);
            t = closed + secs(2);
        }
        // 第 5 个短间隔才询问
        assert!(matches!(guard.on_open(t), RapidVerdict::Ask { .. }));
    }

    /// 边界：间隔恰好 5 秒不算“太快”（清零），差 1 毫秒算。
    #[test]
    fn exactly_gap_is_not_rapid() {
        let t0 = Instant::now();
        let mut guard = RapidGuard::new();
        let (_, mut closed) = cycle(&mut guard, t0, secs(1));
        // 4 个短间隔，累计 streak = 4
        for _ in 0..RAPID_REOPEN_STREAK - 1 {
            let (_, c) = cycle(&mut guard, closed + secs(1), secs(1));
            closed = c;
        }
        assert_eq!(guard.streak, RAPID_REOPEN_STREAK - 1);
        // 恰好 5 秒：清零并放行
        let (verdict, closed) = cycle(&mut guard, closed + RAPID_REOPEN_GAP, secs(1));
        assert_eq!(verdict, RapidVerdict::Allow);
        assert_eq!(guard.streak, 0);
        // 差 1 毫秒的短间隔：计 1，放行
        let (verdict, _) = cycle(
            &mut guard,
            closed + RAPID_REOPEN_GAP - Duration::from_millis(1),
            secs(1),
        );
        assert_eq!(verdict, RapidVerdict::Allow);
        assert_eq!(guard.streak, 1);
    }

    /// 提示框开着：新来的截图类命令直接丢弃，不计数；选“否”后继续丢弃。
    #[test]
    fn drops_while_prompting() {
        let (mut guard, t) = asked_guard();
        assert_eq!(guard.on_open(t + secs(1)), RapidVerdict::Drop);
        assert_eq!(guard.on_open(t + secs(2)), RapidVerdict::Drop);
        guard.answer_quit();
        assert_eq!(guard.on_open(t + secs(3)), RapidVerdict::Drop);
        assert!(guard.is_prompting());
    }

    /// 选“是”后冷却期内一律放行，即使间隔很短也不再询问；冷却里的重复开关不会累计。
    #[test]
    fn cooldown_allows_everything() {
        let (mut guard, t) = asked_guard();
        guard.answer_intentional(t + secs(1), true);
        assert!(!guard.is_prompting());
        let mut now = t + secs(2);
        for _ in 0..RAPID_REOPEN_STREAK * 3 {
            guard.on_closed(now);
            now += secs(1);
            assert_eq!(guard.on_open(now), RapidVerdict::Allow);
        }
        assert_eq!(guard.streak, 0);
    }

    /// 冷却过后重新从零计数：冷却内的关闭记录不算，要再连续 5 个短间隔才询问。
    #[test]
    fn counting_restarts_after_cooldown() {
        let (mut guard, t) = asked_guard();
        guard.answer_intentional(t, false);
        let mut now = t + RAPID_COOLDOWN;
        // 冷却刚结束的第一次开启：没有可比的关闭记录，不计数
        assert_eq!(guard.on_open(now), RapidVerdict::Allow);
        assert_eq!(guard.streak, 0);
        for round in 1..=RAPID_REOPEN_STREAK {
            guard.on_closed(now + secs(1));
            now += secs(3);
            let verdict = guard.on_open(now);
            if round < RAPID_REOPEN_STREAK {
                assert_eq!(verdict, RapidVerdict::Allow, "第 {round} 个短间隔不应询问");
            } else {
                assert!(matches!(verdict, RapidVerdict::Ask { .. }));
            }
        }
    }

    /// 覆盖窗还开着时的重复触发：放行，不计数。
    #[test]
    fn reopen_while_open_is_not_counted() {
        let mut guard = RapidGuard::new();
        let t = Instant::now();
        assert_eq!(guard.on_open(t), RapidVerdict::Allow);
        for i in 1..=RAPID_REOPEN_STREAK * 2 {
            assert_eq!(guard.on_open(t + secs(u64::from(i))), RapidVerdict::Allow);
        }
        assert_eq!(guard.streak, 0);
    }

    /// 没有放行过会话时收到的“关闭”（重复上报）被忽略，不污染最近关闭时刻。
    #[test]
    fn closed_without_open_is_ignored() {
        let mut guard = RapidGuard::new();
        let t = Instant::now();
        guard.on_closed(t);
        assert_eq!(guard.last_close, None);
        let (_, closed) = cycle(&mut guard, t, secs(1));
        guard.on_closed(closed + secs(1));
        assert_eq!(guard.last_close, Some(closed));
    }

    /// 选“是”且命令会分发：会话视为已开；手势已过期不分发：会话视为未开。
    #[test]
    fn answer_intentional_tracks_dispatch() {
        let (mut guard, t) = asked_guard();
        guard.answer_intentional(t, true);
        assert!(guard.open);
        let (mut guard, t) = asked_guard();
        guard.answer_intentional(t, false);
        assert!(!guard.open);
    }

    /// 清单里的动作会计入，整屏截图、设置等不计入。
    #[test]
    fn action_classification() {
        for id in CAPTURE_ACTION_IDS {
            assert!(opens_capture_overlay(id), "{id}");
        }
        for id in [
            "settings",
            "pin_clipboard",
            "quick.screenshot_full_screen",
            "quick.screenshot_focused_window",
            "toggle_dictation",
            "",
        ] {
            assert!(!opens_capture_overlay(id), "{id}");
        }
    }

    /// 只有 `IDNO` 才让宿主退出。
    #[test]
    fn only_no_quits() {
        assert!(quit_chosen(MESSAGE_BOX_ID_NO));
        for code in [0, 6, 1, 2] {
            assert!(!quit_chosen(code), "{code}");
        }
    }
}
