//! 录制时间线：有效时长（扣除暂停）、输出节拍与待输出队列。
//!
//! 输出按固定 `1/fps` 槽号打时间戳；每个槽从有界先进先出队列取最旧的一帧，
//! 多出的帧顺延到下一槽，只有队列溢出才丢最旧，这样 fps 与源速率接近时不会因抖动结构性丢帧。

use std::collections::VecDeque;
use std::time::{Duration, Instant};

/// 每秒的纳秒数。
pub const NANOS_PER_SEC: u64 = 1_000_000_000;

/// 暂停区间：`(暂停时刻, 恢复时刻)`；恢复为 `None` 表示仍在暂停。
type PauseSpan = (Instant, Option<Instant>);

/// 有效时间线：起点、帧率与暂停区间。
#[derive(Debug, Clone)]
pub struct Timeline {
    /// 录制起点。
    start: Instant,
    /// 输出帧率。
    fps: u32,
    /// 已发生的暂停区间（按时间升序）。
    pauses: Vec<PauseSpan>,
}

impl Timeline {
    /// 创建时间线。
    ///
    /// # 参数
    /// - `start`：录制起点。
    /// - `fps`：输出帧率（至少按 1 处理）。
    ///
    /// # 示例
    /// ```ignore
    /// let t = Timeline::new(Instant::now(), 60);
    /// ```
    pub fn new(start: Instant, fps: u32) -> Self {
        Self { start, fps: fps.max(1), pauses: Vec::new() }
    }

    /// 标记暂停开始（已在暂停则忽略）。
    ///
    /// # 参数
    /// - `at`：暂停时刻。
    pub fn pause(&mut self, at: Instant) {
        if !self.is_paused() {
            self.pauses.push((at, None));
        }
    }

    /// 标记恢复（未在暂停则忽略）。
    ///
    /// # 参数
    /// - `at`：恢复时刻。
    pub fn resume(&mut self, at: Instant) {
        if let Some(last) = self.pauses.last_mut().filter(|p| p.1.is_none()) {
            last.1 = Some(at.max(last.0));
        }
    }

    /// 当前是否处于暂停。
    pub fn is_paused(&self) -> bool {
        self.pauses.last().is_some_and(|p| p.1.is_none())
    }

    /// 某时刻是否落在暂停区间内。
    ///
    /// # 参数
    /// - `t`：查询时刻。
    pub fn in_pause(&self, t: Instant) -> bool {
        self.pauses.iter().any(|&(from, to)| t >= from && to.is_none_or(|to| t < to))
    }

    /// 某时刻的有效录制时长（不含暂停；早于起点返回 0）。
    ///
    /// # 参数
    /// - `t`：查询时刻。
    pub fn active_at(&self, t: Instant) -> Duration {
        let mut total = t.saturating_duration_since(self.start);
        for &(from, to) in &self.pauses {
            if t <= from {
                break;
            }
            let end = to.map_or(t, |to| to.min(t));
            total = total.saturating_sub(end.saturating_duration_since(from.max(self.start)));
        }
        total
    }

    /// 结束时刻对应的排他终点槽号（向上取整）。
    ///
    /// # 参数
    /// - `t`：停止时刻。
    pub fn endpoint(&self, t: Instant) -> u64 {
        let nanos = self.active_at(t).as_nanos() * u128::from(self.fps);
        nanos.div_ceil(u128::from(NANOS_PER_SEC)).min(u128::from(u64::MAX)) as u64
    }
}

/// 有效时长对应的槽号（向下取整）。
///
/// # 示例
/// ```ignore
/// assert_eq!(slot_of(Duration::from_millis(100), 30), 3);
/// ```
pub fn slot_of(active: Duration, fps: u32) -> u64 {
    (active.as_nanos() * u128::from(fps.max(1)) / u128::from(NANOS_PER_SEC)).min(u128::from(u64::MAX)) as u64
}

/// 待输出队列容量上限（按有效时间排序的帧；溢出丢最旧）。
pub const MAX_PENDING_FRAMES: usize = 8;

/// 按呈现时间排序的待输出帧队列。
///
/// 每个输出槽取"呈现时间不晚于槽时刻的最新一帧"，更旧的帧随之丢弃；
/// 呈现时间来自 DXGI 的 `LastPresentTime`（与 vsync 对齐，抖动极小），
/// 所以源与输出速率相同或成整数倍时，取帧结果稳定，不会因采集轮询抖动而跳号。
#[derive(Debug, Clone)]
pub struct TimedQueue<T> {
    /// 待输出帧：`(呈现时的有效时长, 帧)`，按时间升序。
    pending: VecDeque<(Duration, T)>,
    /// 因溢出或被更新的帧取代而丢弃的帧数。
    pub dropped: u64,
}

impl<T> Default for TimedQueue<T> {
    /// 空队列。
    fn default() -> Self {
        Self { pending: VecDeque::new(), dropped: 0 }
    }
}

impl<T> TimedQueue<T> {
    /// 放入一帧。
    ///
    /// # 参数
    /// - `at`：该帧呈现时的有效时长（扣除暂停）。
    /// - `item`：帧。
    ///
    /// # 示例
    /// ```ignore
    /// let mut q = TimedQueue::default();
    /// q.push(Duration::from_millis(10), "a");
    /// assert_eq!(q.take_for_slot(Duration::from_millis(20)), Some("a"));
    /// ```
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn push(&mut self, at: Duration, item: T) {
        self.push_with(at, item, |_| {});
    }

    /// 放入一帧，溢出被挤掉的最旧帧交给 `on_evict`（帧追踪用；行为与 [`TimedQueue::push`] 完全一致）。
    ///
    /// # 参数
    /// - `at`：该帧呈现时的有效时长（扣除暂停）。
    /// - `item`：帧。
    /// - `on_evict`：每挤掉一帧调用一次。
    ///
    /// # 示例
    /// ```ignore
    /// q.push_with(Duration::from_millis(10), "a", |old| println!("evicted {old}"));
    /// ```
    pub fn push_with(&mut self, at: Duration, item: T, mut on_evict: impl FnMut(&T)) {
        let index = self.pending.iter().rposition(|(t, _)| *t <= at).map_or(0, |i| i + 1);
        self.pending.insert(index, (at, item));
        while self.pending.len() > MAX_PENDING_FRAMES {
            if let Some((_, old)) = self.pending.pop_front() {
                on_evict(&old);
            }
            self.dropped += 1;
        }
    }

    /// 取呈现时间不晚于 `slot_time` 的最新一帧，并丢弃更旧的帧；没有则返回 `None`。
    ///
    /// # 参数
    /// - `slot_time`：输出槽对应的有效时长。
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn take_for_slot(&mut self, slot_time: Duration) -> Option<T> {
        self.take_for_slot_with(slot_time, |_| {})
    }

    /// 取呈现时间不晚于 `slot_time` 的最新一帧，被取代丢弃的更旧帧交给 `on_discard`
    /// （帧追踪用；行为与 [`TimedQueue::take_for_slot`] 完全一致）。
    ///
    /// # 参数
    /// - `slot_time`：输出槽对应的有效时长。
    /// - `on_discard`：每丢弃一帧调用一次。
    ///
    /// # 示例
    /// ```ignore
    /// let chosen = q.take_for_slot_with(Duration::from_millis(20), |old| println!("dropped {old}"));
    /// ```
    pub fn take_for_slot_with(&mut self, slot_time: Duration, mut on_discard: impl FnMut(&T)) -> Option<T> {
        let count = self.pending.iter().take_while(|(t, _)| *t <= slot_time).count();
        if count == 0 {
            return None;
        }
        self.dropped += count as u64 - 1;
        let mut drained = self.pending.drain(..count);
        let chosen = drained.next_back().map(|(_, item)| item);
        for (_, item) in drained {
            on_discard(&item);
        }
        chosen
    }

    /// 清空（暂停/恢复时丢弃过期帧）。
    pub fn clear(&mut self) {
        self.pending.clear();
    }
}

/// QPC 计数与 `Instant` 的对应关系，用于把 DXGI 的呈现时间换算成 `Instant`。
#[derive(Debug, Clone, Copy)]
pub struct QpcAnchor {
    /// 锚点时刻。
    instant: Instant,
    /// 锚点时刻的 QPC 计数。
    ticks: i64,
    /// QPC 频率（每秒计数）。
    frequency: i64,
}

impl QpcAnchor {
    /// 用给定的（时刻，计数，频率）构造。
    pub fn new(instant: Instant, ticks: i64, frequency: i64) -> Self {
        Self { instant, ticks, frequency: frequency.max(1) }
    }

    /// 读取当前的 QPC 与 `Instant` 建立锚点；失败返回 `None`。
    #[cfg(windows)]
    pub fn capture_now() -> Option<Self> {
        use windows::Win32::System::Performance::{QueryPerformanceCounter, QueryPerformanceFrequency};
        let (mut ticks, mut frequency) = (0i64, 0i64);
        // SAFETY: 两个输出都是局部变量。
        unsafe {
            QueryPerformanceFrequency(&mut frequency).ok()?;
            let instant = Instant::now();
            QueryPerformanceCounter(&mut ticks).ok()?;
            (frequency > 0).then(|| Self::new(instant, ticks, frequency))
        }
    }

    /// 把 QPC 计数换算成 `Instant`（早于锚点的时刻也能表示）。
    ///
    /// # 参数
    /// - `ticks`：QPC 计数。
    ///
    /// # 示例
    /// ```ignore
    /// let a = QpcAnchor::new(Instant::now(), 1000, 1000);
    /// assert!(a.to_instant(2000) > a.to_instant(1000));
    /// ```
    pub fn to_instant(self, ticks: i64) -> Instant {
        let delta = i128::from(ticks) - i128::from(self.ticks);
        let nanos = delta.unsigned_abs() * u128::from(NANOS_PER_SEC) / self.frequency as u128;
        let d = Duration::from_nanos(nanos.min(u128::from(u64::MAX)) as u64);
        if delta >= 0 { self.instant + d } else { self.instant.checked_sub(d).unwrap_or(self.instant) }
    }
}

/// 相位跟踪窗口（最近的呈现时刻数）。
const PHASE_WINDOW: usize = 48;
/// 至少积累这么多样本才开始调整切点。
const PHASE_MIN_SAMPLES: usize = 4;
/// 预热样本数：样本不足时允许切点直接跳到目标，之后只允许缓慢滑动。
pub const PHASE_WARMUP: usize = 8;
/// 预热后每次观察切点最多移动槽周期的这个比例（跟随源与输出的微小频差，避免跳变造成跳号/重复）。
const PHASE_SLEW: f64 = 0.004;
/// 空档不小于最大空档的这个比例，都算候选切点（多簇相位时选离当前切点最近的一个）。
const PHASE_CANDIDATE_RATIO: f64 = 0.7;

/// 槽切点相位跟踪器。
///
/// 输出槽按"呈现时间不晚于切点的最新帧"取帧。若切点恰好落在源的呈现时刻上，
/// 微小抖动会让同一帧时而归入本槽、时而归入下一槽，造成跳号/重复。
/// 这里统计最近呈现时刻在槽周期内的相位，把切点放在相位环上最大空档的中央，
/// 使切点离任何呈现时刻都尽量远（源与输出同速、二倍速等情形都适用）。
#[derive(Debug, Clone)]
pub struct PhaseTracker {
    /// 槽周期。
    period: Duration,
    /// 最近呈现时刻的相位（槽周期的小数部分，0..1）。
    phases: VecDeque<f64>,
    /// 当前切点偏移（槽周期的小数部分）。
    offset: f64,
    /// 是否已用首个样本初始化过。
    primed: bool,
}

impl PhaseTracker {
    /// 创建跟踪器。
    ///
    /// # 参数
    /// - `period`：槽周期（`1/fps`）。
    pub fn new(period: Duration) -> Self {
        Self { period, phases: VecDeque::new(), offset: 0.0, primed: false }
    }

    /// 预热是否完成（已有足够样本，切点可信）。
    pub fn ready(&self) -> bool {
        self.phases.len() >= PHASE_WARMUP
    }

    /// 当前切点偏移（有效时长）。
    pub fn offset(&self) -> Duration {
        self.period.mul_f64(self.offset)
    }

    /// 记录一次呈现时刻。
    ///
    /// # 参数
    /// - `present`：该帧呈现时的有效时长。
    ///
    /// # 示例
    /// ```ignore
    /// let mut t = PhaseTracker::new(Duration::from_millis(33));
    /// t.observe(Duration::from_millis(5));
    /// assert!(t.offset() > Duration::ZERO);
    /// ```
    pub fn observe(&mut self, present: Duration) {
        let phase = (present.as_secs_f64() / self.period.as_secs_f64()).fract();
        if !self.primed {
            self.primed = true;
            self.offset = (phase + 0.5).fract();
        }
        self.phases.push_back(phase);
        if self.phases.len() > PHASE_WINDOW {
            self.phases.pop_front();
        }
        if self.phases.len() >= PHASE_MIN_SAMPLES {
            let target = nearest_candidate(&self.phases, self.offset);
            let delta = wrap_signed(target - self.offset);
            if self.phases.len() < PHASE_WARMUP {
                self.offset = target;
            } else {
                self.offset = (self.offset + delta.clamp(-PHASE_SLEW, PHASE_SLEW)).rem_euclid(1.0);
            }
        }
    }
}

/// 把环上的差值折到 `[-0.5, 0.5]`。
fn wrap_signed(d: f64) -> f64 {
    let d = d.rem_euclid(1.0);
    if d > 0.5 { d - 1.0 } else { d }
}

/// 候选切点里离 `current` 最近的一个：候选是相位环上不小于最大空档 70% 的各空档的中央。
fn nearest_candidate(phases: &VecDeque<f64>, current: f64) -> f64 {
    let mut sorted: Vec<f64> = phases.iter().copied().collect();
    sorted.sort_by(|a, b| a.total_cmp(b));
    let gaps: Vec<(f64, f64)> = sorted
        .iter()
        .enumerate()
        .map(|(i, &p)| {
            let next = if i + 1 < sorted.len() { sorted[i + 1] } else { sorted[0] + 1.0 };
            ((p + (next - p) / 2.0).rem_euclid(1.0), next - p)
        })
        .collect();
    let widest = gaps.iter().map(|g| g.1).fold(0.0, f64::max);
    gaps.iter()
        .filter(|g| g.1 >= widest * PHASE_CANDIDATE_RATIO)
        .map(|g| g.0)
        .min_by(|a, b| wrap_signed(a - current).abs().total_cmp(&wrap_signed(b - current).abs()))
        .unwrap_or(current)
}

/// 输出节拍：每个槽至多触发一次；落后时跳过过期槽，不补发。
#[derive(Debug, Clone, Default)]
pub struct TickClock {
    /// 下一个待触发的槽号。
    next: u64,
    /// 因落后被跳过的槽数。
    pub missed: u64,
}

impl TickClock {
    /// 从指定槽号开始。
    ///
    /// # 参数
    /// - `first`：第一个待触发的槽号。
    pub fn starting_at(first: u64) -> Self {
        Self { next: first, missed: 0 }
    }

    /// 下一个待触发的槽号。
    pub fn next_slot(&self) -> u64 {
        self.next
    }

    /// 逐槽触发（不跳槽）：下一个槽号不晚于 `current` 时返回它，用于预热结束后把积压的槽依次补齐。
    ///
    /// # 参数
    /// - `current`：当前时刻所在槽号。
    ///
    /// # 示例
    /// ```ignore
    /// let mut c = TickClock::starting_at(1);
    /// assert_eq!(c.fire_next(3), Some(1));
    /// assert_eq!(c.fire_next(3), Some(2));
    /// assert_eq!(c.fire_next(3), Some(3));
    /// assert_eq!(c.fire_next(3), None);
    /// ```
    pub fn fire_next(&mut self, current: u64) -> Option<u64> {
        if current < self.next {
            return None;
        }
        let slot = self.next;
        self.next += 1;
        Some(slot)
    }

    /// 当前槽号到达时触发：返回应使用的槽号（当前槽）；未到返回 `None`。
    ///
    /// # 参数
    /// - `current`：当前时刻所在槽号。
    ///
    /// # 示例
    /// ```ignore
    /// let mut c = TickClock::starting_at(1);
    /// assert_eq!(c.fire(0), None);
    /// assert_eq!(c.fire(3), Some(3)); // 跳过槽 1、2
    /// assert_eq!(c.missed, 2);
    /// ```
    pub fn fire(&mut self, current: u64) -> Option<u64> {
        if current < self.next {
            return None;
        }
        self.missed += current - self.next;
        self.next = current + 1;
        Some(current)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 构造以 `base` 为起点、偏移毫秒的时刻。
    fn at(base: Instant, ms: u64) -> Instant {
        base + Duration::from_millis(ms)
    }

    /// 暂停区间被扣除，暂停中的时刻有效时长冻结。
    #[test]
    fn pause_is_subtracted_from_active_time() {
        let base = Instant::now();
        let mut t = Timeline::new(base, 30);
        t.pause(at(base, 1000));
        assert!(t.is_paused());
        assert_eq!(t.active_at(at(base, 1500)), Duration::from_millis(1000));
        t.resume(at(base, 2000));
        assert!(!t.is_paused());
        assert_eq!(t.active_at(at(base, 2500)), Duration::from_millis(1500));
        assert!(t.in_pause(at(base, 1500)));
        assert!(!t.in_pause(at(base, 2500)));
    }

    /// 多次暂停累计扣除；重复暂停/恢复被忽略。
    #[test]
    fn multiple_pauses_accumulate_and_duplicates_are_ignored() {
        let base = Instant::now();
        let mut t = Timeline::new(base, 60);
        t.resume(at(base, 10));
        t.pause(at(base, 100));
        t.pause(at(base, 150));
        t.resume(at(base, 200));
        t.pause(at(base, 300));
        t.resume(at(base, 500));
        assert_eq!(t.active_at(at(base, 600)), Duration::from_millis(300));
    }

    /// 槽号向下取整、终点向上取整。
    #[test]
    fn slot_and_endpoint_rounding() {
        let base = Instant::now();
        let t = Timeline::new(base, 30);
        assert_eq!(slot_of(t.active_at(at(base, 100)), 30), 3);
        assert_eq!(slot_of(t.active_at(at(base, 66)), 30), 1);
        assert_eq!(t.endpoint(at(base, 100)), 3);
        assert_eq!(t.endpoint(at(base, 101)), 4);
        assert_eq!(t.endpoint(base), 0);
    }

    /// 队列：取不晚于槽时刻的最新帧并丢弃更旧的；更晚的帧保留；溢出丢最旧。
    #[test]
    fn timed_queue_takes_newest_not_after_slot() {
        let ms = Duration::from_millis;
        let mut q = TimedQueue::default();
        q.push(ms(10), 'a');
        q.push(ms(20), 'b');
        q.push(ms(40), 'c');
        assert_eq!(q.take_for_slot(ms(5)), None);
        assert_eq!(q.take_for_slot(ms(30)), Some('b'));
        assert_eq!(q.dropped, 1);
        assert_eq!(q.take_for_slot(ms(30)), None);
        assert_eq!(q.take_for_slot(ms(50)), Some('c'));
        q.push(ms(60), 'x');
        q.clear();
        assert_eq!(q.take_for_slot(ms(100)), None);
        let mut q = TimedQueue::default();
        for i in 0..(MAX_PENDING_FRAMES as u64 + 3) {
            q.push(ms(100 + i), i);
        }
        assert_eq!(q.take_for_slot(ms(0)), None);
        assert_eq!(q.dropped, 3);
    }

    /// 带回调的变体：丢弃与溢出的帧逐个交给回调，结果与计数同无回调版本一致。
    #[test]
    fn timed_queue_callbacks_report_discarded_and_evicted() {
        let ms = Duration::from_millis;
        let mut q = TimedQueue::default();
        let mut evicted = Vec::new();
        for i in 0..(MAX_PENDING_FRAMES as u64 + 2) {
            q.push_with(ms(10 + i), i, |old| evicted.push(*old));
        }
        assert_eq!(evicted, vec![0, 1]);
        assert_eq!(q.dropped, 2);
        let mut discarded = Vec::new();
        let chosen = q.take_for_slot_with(ms(15), |old| discarded.push(*old));
        assert_eq!(chosen, Some(5));
        assert_eq!(discarded, vec![2, 3, 4]);
        assert_eq!(q.dropped, 5);
        assert_eq!(q.take_for_slot_with(ms(15), |_| panic!("无帧可丢")), None);
    }

    /// 乱序到达的帧按时间排序。
    #[test]
    fn timed_queue_sorts_out_of_order_arrivals() {
        let ms = Duration::from_millis;
        let mut q = TimedQueue::default();
        q.push(ms(30), 3);
        q.push(ms(10), 1);
        q.push(ms(20), 2);
        assert_eq!(q.take_for_slot(ms(25)), Some(2));
        assert_eq!(q.take_for_slot(ms(35)), Some(3));
    }

    /// 锚点换算：前后差值按频率折算，早于锚点也可表示。
    #[test]
    fn qpc_anchor_converts_both_directions() {
        let base = Instant::now() + Duration::from_secs(10);
        let a = QpcAnchor::new(base, 5_000, 10_000_000);
        assert_eq!(a.to_instant(5_000 + 10_000_000), base + Duration::from_secs(1));
        assert_eq!(a.to_instant(5_000 - 5_000_000), base - Duration::from_millis(500));
        assert_eq!(a.to_instant(5_000), base);
        #[cfg(windows)]
        assert!(QpcAnchor::capture_now().is_some());
    }

    /// 节拍：未到不触发，落后时跳过过期槽并计数，同一槽不重复触发。
    #[test]
    fn tick_clock_skips_obsolete_slots() {
        let mut c = TickClock::starting_at(1);
        assert_eq!(c.fire(0), None);
        assert_eq!(c.fire(1), Some(1));
        assert_eq!(c.fire(1), None);
        assert_eq!(c.fire(4), Some(4));
        assert_eq!(c.missed, 2);
        assert_eq!(c.next_slot(), 5);
        let mut c = TickClock::starting_at(1);
        assert_eq!((c.fire_next(3), c.fire_next(3), c.fire_next(3), c.fire_next(3)), (Some(1), Some(2), Some(3), None));
        assert_eq!(c.missed, 0);
    }

    /// 预热：样本不足时未就绪，达到预热样本数后就绪。
    #[test]
    fn phase_tracker_reports_readiness() {
        let mut t = PhaseTracker::new(Duration::from_millis(100));
        for i in 0..(PHASE_WARMUP as u64 - 1) {
            t.observe(Duration::from_millis(i * 100 + 5));
            assert!(!t.ready());
        }
        t.observe(Duration::from_millis(1005));
        assert!(t.ready());
    }

    /// 仿真：`src_hz` 的源（每个序号连发 `twin` 帧，呈现时间带 `jitter_ns` 抖动）按 `out_hz` 输出，
    /// 槽在 `槽时刻 + hold` 后触发；返回输出的序号列表。
    fn simulate(src_hz: u64, out_hz: u64, twin: u64, seconds: u64, phase_ns: u64, jitter_ns: i64, drift_ppm: i64) -> Vec<u64> {
        let hold_ns = NANOS_PER_SEC / (out_hz * 2) + 2_000_000;
        let mut state = 99u64;
        let mut queue = TimedQueue::default();
        let mut clock = TickClock::starting_at(0);
        let mut phase = PhaseTracker::new(Duration::from_nanos(NANOS_PER_SEC / out_hz));
        let mut out = Vec::new();
        let end_ns = seconds * NANOS_PER_SEC;
        let total = src_hz * seconds;
        let mut next_frame = 0u64;
        let mut t = 0u64;
        let present = |i: u64, state: &mut u64| -> u64 {
            *state = state.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            let j = if jitter_ns == 0 { 0 } else { ((*state >> 33) as i64 % (2 * jitter_ns + 1)) - jitter_ns };
            (i as i128 * i128::from(NANOS_PER_SEC) * (1_000_000 + i128::from(drift_ppm)) / (src_hz as i128 * 1_000_000) + i128::from(j) + i128::from(phase_ns)).max(0) as u64
        };
        let mut upcoming = present(0, &mut state);
        while t < end_ns {
            // 帧在呈现后最多再过一个轮询周期（8ms）才被合成线程看到
            while next_frame < total && upcoming + 8_000_000 <= t {
                queue.push(Duration::from_nanos(upcoming), next_frame / twin);
                phase.observe(Duration::from_nanos(upcoming));
                next_frame += 1;
                upcoming = present(next_frame, &mut state);
            }
            let offset = phase.offset().as_nanos() as u64;
            let slot = t.saturating_sub(hold_ns + offset) * out_hz / NANOS_PER_SEC;
            if let Some(slot) = clock.fire(slot)
                && let Some(seq) = queue.take_for_slot(Duration::from_nanos(slot * NANOS_PER_SEC / out_hz + offset))
            {
                out.push(seq);
            }
            t += 500_000;
        }
        out
    }

    /// 相位跟踪：单簇相位的切点在对侧；双簇（二倍速）的切点在两簇正中；窗口滚动后跟随漂移。
    #[test]
    fn phase_tracker_places_cutoff_far_from_presents() {
        let period = Duration::from_millis(100);
        let mut t = PhaseTracker::new(period);
        for i in 0..20u64 {
            t.observe(Duration::from_millis(i * 100 + 10));
        }
        let off = t.offset().as_secs_f64() / period.as_secs_f64();
        assert!((off - 0.6).abs() < 0.05, "off={off}");
        // 预热后滑动有限速：目标突变时切点不会一步跳到位
        let before = t.offset();
        for i in 20..24u64 {
            t.observe(Duration::from_millis(i * 100 + 60));
        }
        let moved = (t.offset().as_secs_f64() - before.as_secs_f64()).abs() / period.as_secs_f64();
        assert!(moved <= PHASE_SLEW * 4.0 + 1e-9, "moved={moved}");
        let mut two = PhaseTracker::new(period);
        for i in 0..40u64 {
            two.observe(Duration::from_millis(i * 50 + 10));
        }
        let off = two.offset().as_secs_f64() / period.as_secs_f64();
        let dist = [0.1f64, 0.6].iter().map(|p| (off - p).abs().min(1.0 - (off - p).abs())).fold(1.0, f64::min);
        assert!(dist > 0.2, "off={off}");
    }

    /// 输出序号区间内缺失的序号数。
    fn missing(seqs: &[u64]) -> u64 {
        let (Some(&first), Some(&last)) = (seqs.first(), seqs.last()) else { return 0 };
        let set: std::collections::BTreeSet<u64> = seqs.iter().copied().collect();
        (first..=last).filter(|s| !set.contains(s)).count() as u64
    }

    /// 源与输出同速：任意相位、小抖动下不丢序号。
    #[test]
    fn equal_rate_loses_no_sequence() {
        for phase in [0, 3_000_000, 8_000_000, 15_000_000, 16_600_000] {
            let out = simulate(60, 60, 1, 10, phase, 100_000, 0);
            assert!(missing(&out) <= 1, "phase={phase} missing={}", missing(&out));
        }
    }

    /// 30fps 内容以 60Hz 重复呈现（每序号两帧）按 30fps 输出：不丢序号。
    #[test]
    fn twin_present_at_half_rate_loses_no_sequence() {
        for phase in [0, 5_000_000, 12_000_000, 16_000_000, 25_000_000, 33_000_000] {
            let out = simulate(60, 30, 2, 10, phase, 100_000, 0);
            assert!(missing(&out) <= 1, "phase={phase} missing={} out={:?}", missing(&out), &out[..out.len().min(40)]);
            assert!(out.len() >= 290, "len={}", out.len());
        }
    }

    /// 源与输出有微小频差（±0.2%，相位缓慢扫过切点）且呈现时间抖动 ±2ms：30 秒内跳号不超过 1%。
    #[test]
    fn slow_drift_with_jitter_stays_under_one_percent() {
        for (src, out, twin, ppm) in [(60, 60, 1, -800), (60, 60, 1, 1700), (60, 30, 2, -1700), (60, 30, 2, 900)] {
            for phase in [0, 7_000_000, 14_000_000] {
                let seqs = simulate(src, out, twin, 30, phase, 2_000_000, ppm);
                let span = seqs.last().unwrap() - seqs.first().unwrap() + 1;
                assert!(missing(&seqs) * 100 <= span, "src={src} out={out} ppm={ppm} phase={phase} missing={} span={span}", missing(&seqs));
            }
        }
    }

    /// 60Hz 独立内容按 30fps 输出：约 30 帧/秒。
    #[test]
    fn double_rate_source_is_thinned() {
        let out = simulate(60, 30, 1, 10, 4_000_000, 100_000, 0);
        assert!((290..=305).contains(&(out.len() as u64)), "len={}", out.len());
    }
}
