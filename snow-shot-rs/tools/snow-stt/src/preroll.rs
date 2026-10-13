//! 先开麦的环形缓存：START 一到就开始采音，模型还在加载时样本先进有上限的环形缓存，
//! 模型就绪后会话主循环按序快进消费积压，追平后自然转入实时。
//!
//! 采集放在独立线程（打开麦克风本身要 0.7~0.9 s，不能挡住模型加载，也不能被模型加载挡住），
//! 主线程这边的 [`PumpedSource`] 只是个 [`AudioSource`]：先交出积压（每次一小批，
//! 方便主循环在两批之间响应 STOP / CANCEL），再交出实时样本。

use std::collections::VecDeque;
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::thread::JoinHandle;
use std::time::Duration;

use crate::sherpa::SAMPLE_RATE;
use crate::source::{AudioSource, SourceEvent};

/// 环形缓存的默认上限（秒）。16kHz 的 f32 样本约 64KB/s，30 s 约 1.9MB，对听写中本来就有
/// 140~250MB 的 worker 可以忽略；覆盖常见的模型加载耗时（本机实测 3~14 s）并留足余量。
pub const PREROLL_MAX_SECONDS: u32 = 30;
/// 追赶时每次最多交出的样本数（1 s）：每批之后主循环会先处理命令再取下一批。
pub const CATCHUP_BATCH_SAMPLES: usize = SAMPLE_RATE as usize;
/// 采集线程每次向来源取样的等待上限，同时决定收到停止信号的延迟。
const PUMP_POLL: Duration = Duration::from_millis(50);

/// 按本次会话的最长时长换算环形缓存上限（样本数）。
///
/// 缓存只在“加载期间 + 追赶期间”起作用，与 `max_seconds` 是两回事：`max_seconds` 按**实际喂给后端**
/// 的样本计（从开麦算起，缓存溢出丢掉的不计入）。但缓存超过 `max_seconds` 没有意义（喂满
/// 就会停），所以上限取 `min(30 s, max_seconds)`；`max_seconds` 为 0（不限）时用默认的 30 s。
///
/// # 参数
/// - `max_seconds`：START 里的最长录音秒数，0 表示不限。
///
/// # 返回
/// 缓存上限（样本数），至少 1 s。
///
/// # 示例
/// ```ignore
/// assert_eq!(cap_samples(0), 30 * 16_000);
/// assert_eq!(cap_samples(10), 10 * 16_000);
/// assert_eq!(cap_samples(120), 30 * 16_000);
/// ```
pub fn cap_samples(max_seconds: u32) -> usize {
    let seconds = match max_seconds {
        0 => PREROLL_MAX_SECONDS,
        n => n.clamp(1, PREROLL_MAX_SECONDS),
    };
    seconds as usize * SAMPLE_RATE as usize
}

/// 有上限的样本环形缓存：写满后丢弃最旧的，保留最新的一段。
pub struct SampleRing {
    /// 样本，队首最旧。
    buf: VecDeque<f32>,
    /// 容量上限（样本数）。
    cap: usize,
    /// 累计因溢出丢弃的样本数。
    dropped: u64,
}

impl SampleRing {
    /// 创建缓存。
    ///
    /// # 参数
    /// - `cap`：容量上限（样本数），至少为 1。
    pub fn new(cap: usize) -> Self {
        Self {
            buf: VecDeque::new(),
            cap: cap.max(1),
            dropped: 0,
        }
    }

    /// 当前缓存的样本数。
    pub fn len(&self) -> usize {
        self.buf.len()
    }

    /// 缓存是否为空。
    pub fn is_empty(&self) -> bool {
        self.buf.is_empty()
    }

    /// 累计因溢出丢弃的样本数。
    pub fn dropped(&self) -> u64 {
        self.dropped
    }

    /// 追加样本；超出上限时丢弃最旧的，始终保留最新的 `cap` 个。
    ///
    /// # 参数
    /// - `samples`：新到的样本。
    ///
    /// # 返回
    /// 本次因溢出丢弃的样本数。
    pub fn push(&mut self, samples: &[f32]) -> usize {
        let incoming = if samples.len() > self.cap {
            &samples[samples.len() - self.cap..]
        } else {
            samples
        };
        let overflow = (self.buf.len() + incoming.len()).saturating_sub(self.cap);
        // 整段新样本放不下时，连同没放进去的那一截一起算丢弃
        let lost = overflow + (samples.len() - incoming.len());
        self.buf.drain(..overflow);
        self.buf.extend(incoming.iter().copied());
        self.dropped += lost as u64;
        lost
    }

    /// 从队首取走至多 `max` 个样本（最旧的先出）。
    ///
    /// # 参数
    /// - `max`：最多取多少个。
    pub fn take(&mut self, max: usize) -> Vec<f32> {
        let n = max.min(self.buf.len());
        self.buf.drain(..n).collect()
    }
}

/// 采集线程结束的原因。
#[derive(Debug, Clone, PartialEq, Eq)]
enum End {
    /// 来源正常结束（wav 读完）。
    Ended,
    /// 来源出错（含打开失败），携带单行原因。
    Failed(String),
}

/// 采集线程与主线程共享的状态。
struct Shared {
    /// 环形缓存。
    ring: SampleRing,
    /// 打开来源的尝试已经结束（成功或失败，`on_open` 回调已执行完）。
    opened: bool,
    /// 采集线程已结束的原因；`None` 表示还在采。
    end: Option<End>,
    /// 主线程要求采集线程停止。
    stop: bool,
}

/// 共享状态加唤醒条件。
type Hub = Arc<(Mutex<Shared>, Condvar)>;

/// 加锁；某个线程 panic 留下的毒锁不影响继续用（数据只是样本与标志）。
fn lock(hub: &Hub) -> MutexGuard<'_, Shared> {
    hub.0.lock().unwrap_or_else(|e| e.into_inner())
}

/// 经环形缓存转交样本的来源：打开与采集都在独立线程，主线程按需取积压与实时样本。
pub struct PumpedSource {
    /// 与采集线程共享的状态。
    hub: Hub,
    /// 采集线程句柄，丢弃本来源时等它退出（保证麦克风先于进程退出释放）。
    worker: Option<JoinHandle<()>>,
}

impl PumpedSource {
    /// 启动采集线程：线程里先 `open` 来源，成功后持续把样本写进环形缓存。
    ///
    /// # 参数
    /// - `open`：在采集线程里执行的打开来源的函数（来源不必是 `Send`）。
    /// - `on_open`：打开结果回调，在采集线程里调用一次（成功时就是“已在录音”的时刻）。
    /// - `cap_samples`：环形缓存上限（样本数），见 [`cap_samples`]。
    ///
    /// # 返回
    /// 来源。线程启动失败时当作来源已失败，下次取样会报错。
    ///
    /// # 示例
    /// ```ignore
    /// let src = PumpedSource::start(
    ///     || MicSource::open().map(|s| Box::new(s) as Box<dyn AudioSource>),
    ///     |r| println!("opened: {r:?}"),
    ///     cap_samples(120),
    /// );
    /// ```
    pub fn start(
        open: impl FnOnce() -> Result<Box<dyn AudioSource>, String> + Send + 'static,
        on_open: impl FnOnce(&Result<(), String>) + Send + 'static,
        cap_samples: usize,
    ) -> Self {
        let hub: Hub = Arc::new((
            Mutex::new(Shared {
                ring: SampleRing::new(cap_samples),
                opened: false,
                end: None,
                stop: false,
            }),
            Condvar::new(),
        ));
        let thread_hub = Arc::clone(&hub);
        let worker = std::thread::Builder::new()
            .name("snow-stt-preroll".into())
            .spawn(move || pump(&thread_hub, open, on_open))
            .map_err(|e| {
                let mut g = lock(&hub);
                g.opened = true;
                g.end = Some(End::Failed(format!("无法创建采集线程: {e}")));
            })
            .ok();
        Self { hub, worker }
    }

    /// 缓存里等着被取走的样本数（测试用）。
    #[cfg(test)]
    fn pending_len(&self) -> usize {
        lock(&self.hub).ring.len()
    }

    /// 累计因溢出丢弃的样本数（测试用）。
    #[cfg(test)]
    fn dropped(&self) -> u64 {
        lock(&self.hub).ring.dropped()
    }
}

/// 采集线程主体：打开来源，循环取样写缓存，直到来源结束、出错或被要求停止。
fn pump(
    hub: &Hub,
    open: impl FnOnce() -> Result<Box<dyn AudioSource>, String>,
    on_open: impl FnOnce(&Result<(), String>),
) {
    let mut inner = match open() {
        Ok(source) => {
            on_open(&Ok(()));
            mark_opened(hub);
            source
        }
        Err(why) => {
            on_open(&Err(why.clone()));
            lock(hub).opened = true;
            finish(hub, End::Failed(why));
            return;
        }
    };
    let mut warned = false;
    loop {
        if lock(hub).stop {
            return;
        }
        match inner.next(PUMP_POLL) {
            SourceEvent::Idle => {}
            SourceEvent::Samples(samples) => {
                let lost = {
                    let mut g = lock(hub);
                    let lost = g.ring.push(&samples);
                    hub.1.notify_all();
                    lost
                };
                if lost > 0 && !warned {
                    warned = true;
                    // 只有 stderr 可用：主程序把它转进 debug 日志
                    eprintln!("preroll: 环形缓存已满，丢弃最旧的音频（至少 {lost} 个样本）");
                }
            }
            SourceEvent::Ended => {
                finish(hub, End::Ended);
                return;
            }
            SourceEvent::Failed(why) => {
                finish(hub, End::Failed(why));
                return;
            }
        }
    }
}

/// 标记“打开来源的尝试已结束”并唤醒 [`AudioSource::wait_ready`] 的等待者。
fn mark_opened(hub: &Hub) {
    lock(hub).opened = true;
    hub.1.notify_all();
}

/// 记下采集线程的结束原因并唤醒等待者。
fn finish(hub: &Hub, end: End) {
    lock(hub).end = Some(end);
    hub.1.notify_all();
}

impl AudioSource for PumpedSource {
    /// 先交出积压（每次至多 [`CATCHUP_BATCH_SAMPLES`] 个，最旧的先出），积压取尽后交出实时样本；
    /// 缓存空了再等最多 `timeout`。采集线程已结束且缓存取尽后，返回 `Ended` / `Failed`。
    fn next(&mut self, timeout: Duration) -> SourceEvent {
        let mut g = lock(&self.hub);
        if g.ring.is_empty() && g.end.is_none() {
            g = self
                .hub
                .1
                .wait_timeout_while(g, timeout, |s| s.ring.is_empty() && s.end.is_none())
                .unwrap_or_else(|e| e.into_inner())
                .0;
        }
        if !g.ring.is_empty() {
            return SourceEvent::Samples(g.ring.take(CATCHUP_BATCH_SAMPLES));
        }
        match &g.end {
            Some(End::Ended) => SourceEvent::Ended,
            Some(End::Failed(why)) => SourceEvent::Failed(why.clone()),
            None => SourceEvent::Idle,
        }
    }

    /// 等采集线程完成打开来源的尝试（成功即已发出 `RECORDING`，失败即已报错），最多等 `timeout`。
    fn wait_ready(&mut self, timeout: Duration) {
        let g = lock(&self.hub);
        let _ = self
            .hub
            .1
            .wait_timeout_while(g, timeout, |s| !s.opened)
            .unwrap_or_else(|e| e.into_inner());
    }

    /// 取走缓存里剩下的全部样本（STOP 时用，保证积压也进识别）。
    fn drain_pending(&mut self) -> Vec<f32> {
        let mut g = lock(&self.hub);
        let n = g.ring.len();
        g.ring.take(n)
    }
}

impl Drop for PumpedSource {
    /// 通知采集线程停止并等它退出（来源随线程释放，麦克风就此关闭）；有溢出丢弃时在 stderr 汇总一行。
    fn drop(&mut self) {
        let dropped = {
            let mut g = lock(&self.hub);
            g.stop = true;
            g.ring.dropped()
        };
        if dropped > 0 {
            eprintln!(
                "preroll: 本次会话共因缓存溢出丢弃约 {:.1} 秒音频",
                dropped as f64 / f64::from(SAMPLE_RATE)
            );
        }
        self.hub.1.notify_all();
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;
    use std::time::Instant;

    /// 一秒的样本数（测试里换算用）。
    const SEC: usize = SAMPLE_RATE as usize;

    /// 递增序列：第 `start` 个起的 `n` 个样本，值就是序号，便于核对顺序。
    fn ramp(start: usize, n: usize) -> Vec<f32> {
        (start..start + n).map(|i| i as f32).collect()
    }

    /// 由通道驱动的来源：测试线程往通道里塞事件，空闲时按超时睡眠（模拟真实来源的阻塞等待），
    /// 发送端全部丢弃后视为来源结束。
    struct ChannelSource(mpsc::Receiver<SourceEvent>);

    impl AudioSource for ChannelSource {
        fn next(&mut self, timeout: Duration) -> SourceEvent {
            match self.0.recv_timeout(timeout) {
                Ok(event) => event,
                Err(mpsc::RecvTimeoutError::Timeout) => SourceEvent::Idle,
                Err(mpsc::RecvTimeoutError::Disconnected) => SourceEvent::Ended,
            }
        }
    }

    /// 启动一个由通道驱动的 `PumpedSource`，返回事件发送端。
    fn start_channel(cap: usize) -> (PumpedSource, mpsc::Sender<SourceEvent>) {
        let (tx, rx) = mpsc::channel();
        let source = PumpedSource::start(
            move || Ok(Box::new(ChannelSource(rx)) as Box<dyn AudioSource>),
            |_| {},
            cap,
        );
        (source, tx)
    }

    /// 等缓存里至少有 `n` 个样本（采集线程是异步的，测试要等它写完）。
    fn wait_pending(source: &PumpedSource, n: usize) {
        let deadline = Instant::now() + Duration::from_secs(5);
        while source.pending_len() < n {
            assert!(
                Instant::now() < deadline,
                "采集线程没在 5 s 内写入 {n} 个样本"
            );
            std::thread::sleep(Duration::from_millis(2));
        }
    }

    /// 等缓存累计丢弃至少 `n` 个样本（溢出的判定同样发生在采集线程里）。
    fn wait_dropped(source: &PumpedSource, n: u64) {
        let deadline = Instant::now() + Duration::from_secs(5);
        while source.dropped() < n {
            assert!(
                Instant::now() < deadline,
                "采集线程没在 5 s 内丢弃 {n} 个样本"
            );
            std::thread::sleep(Duration::from_millis(2));
        }
    }

    /// 反复取样直到来源给出 `Ended` / `Failed`，返回收到的全部样本与终态。
    fn drain_all(source: &mut PumpedSource) -> (Vec<f32>, SourceEvent) {
        let mut all = Vec::new();
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            assert!(Instant::now() < deadline, "来源没在 5 s 内结束");
            match source.next(Duration::from_millis(20)) {
                SourceEvent::Samples(s) => all.extend(s),
                SourceEvent::Idle => {}
                end => return (all, end),
            }
        }
    }

    /// 缓存上限：默认 30 s，`max_seconds` 更小就取它，不限（0）用默认，下限 1 s。
    #[test]
    fn cap_follows_max_seconds() {
        assert_eq!(cap_samples(0), 30 * SEC);
        assert_eq!(cap_samples(10), 10 * SEC);
        assert_eq!(cap_samples(30), 30 * SEC);
        assert_eq!(cap_samples(120), 30 * SEC);
        assert_eq!(cap_samples(1), SEC);
    }

    /// 未满时按序保存；取走时最旧的先出；没有溢出就没有丢弃。
    #[test]
    fn ring_keeps_order_below_cap() {
        let mut ring = SampleRing::new(10);
        assert!(ring.is_empty());
        assert_eq!(ring.push(&ramp(0, 4)), 0);
        assert_eq!(ring.push(&ramp(4, 4)), 0);
        assert_eq!(ring.len(), 8);
        assert_eq!(ring.take(3), ramp(0, 3));
        assert_eq!(ring.take(100), ramp(3, 5));
        assert!(ring.is_empty());
        assert_eq!(ring.dropped(), 0);
    }

    /// 溢出丢最旧：容量 10 连续写入 0..14，只剩最新的 4..14，并累计丢了 4 个。
    #[test]
    fn ring_drops_oldest_on_overflow() {
        let mut ring = SampleRing::new(10);
        assert_eq!(ring.push(&ramp(0, 8)), 0);
        assert_eq!(ring.push(&ramp(8, 6)), 4);
        assert_eq!(ring.len(), 10);
        assert_eq!(ring.dropped(), 4);
        assert_eq!(ring.take(100), ramp(4, 10));
    }

    /// 单次写入就超过上限：只留最后 cap 个，其余（含缓存里原有的）都算丢弃。
    #[test]
    fn ring_oversized_push_keeps_newest_cap() {
        let mut ring = SampleRing::new(5);
        ring.push(&ramp(0, 3));
        assert_eq!(ring.push(&ramp(3, 12)), 3 + 7);
        assert_eq!(ring.take(100), ramp(10, 5));
        assert_eq!(ring.dropped(), 10);
        // 刚好写满不算溢出
        let mut ring = SampleRing::new(5);
        assert_eq!(ring.push(&ramp(0, 5)), 0);
        assert_eq!(ring.dropped(), 0);
    }

    /// 积压先行：先写入的样本先交出，每批不超过 1 s，拼起来与原序列完全一致；随后才是新到的实时样本。
    #[test]
    fn backlog_is_delivered_in_order_in_batches_before_live() {
        let (mut source, tx) = start_channel(10 * SEC);
        // “加载期间”到达的 2.5 s 积压
        tx.send(SourceEvent::Samples(ramp(0, SEC + SEC / 2)))
            .unwrap();
        tx.send(SourceEvent::Samples(ramp(SEC + SEC / 2, SEC)))
            .unwrap();
        wait_pending(&source, 2 * SEC + SEC / 2);

        let mut batches = Vec::new();
        while let SourceEvent::Samples(s) = source.next(Duration::from_millis(5)) {
            batches.push(s);
        }
        let sizes: Vec<usize> = batches.iter().map(Vec::len).collect();
        assert_eq!(sizes, [SEC, SEC, SEC / 2]);
        assert_eq!(batches.concat(), ramp(0, 2 * SEC + SEC / 2));

        // 积压取尽后，新到的实时样本接在后面
        tx.send(SourceEvent::Samples(ramp(2 * SEC + SEC / 2, 100)))
            .unwrap();
        wait_pending(&source, 100);
        assert_eq!(
            source.next(Duration::from_millis(5)),
            SourceEvent::Samples(ramp(2 * SEC + SEC / 2, 100))
        );
        assert_eq!(source.next(Duration::from_millis(1)), SourceEvent::Idle);
    }

    /// 采集线程结束后，积压先交完，才给出 `Ended`；之后一直是 `Ended`。
    #[test]
    fn end_is_reported_after_backlog() {
        let (mut source, tx) = start_channel(10 * SEC);
        tx.send(SourceEvent::Samples(ramp(0, 3000))).unwrap();
        tx.send(SourceEvent::Ended).unwrap();
        let (all, end) = drain_all(&mut source);
        assert_eq!(all, ramp(0, 3000));
        assert_eq!(end, SourceEvent::Ended);
        assert_eq!(source.next(Duration::from_millis(1)), SourceEvent::Ended);
    }

    /// 来源出错：积压先交完，再报同一条错误原因。
    #[test]
    fn failure_is_reported_after_backlog() {
        let (mut source, tx) = start_channel(10 * SEC);
        tx.send(SourceEvent::Samples(ramp(0, 10))).unwrap();
        tx.send(SourceEvent::Failed("设备丢失".into())).unwrap();
        let (all, end) = drain_all(&mut source);
        assert_eq!(all, ramp(0, 10));
        assert_eq!(end, SourceEvent::Failed("设备丢失".into()));
    }

    /// 缓存溢出只丢最旧的：上限 2 s，写入 3 s 后取到的是最新的 2 s，并记下丢了 1 s。
    #[test]
    fn overflow_keeps_newest_window() {
        let (mut source, tx) = start_channel(2 * SEC);
        tx.send(SourceEvent::Samples(ramp(0, SEC))).unwrap();
        tx.send(SourceEvent::Samples(ramp(SEC, SEC))).unwrap();
        tx.send(SourceEvent::Samples(ramp(2 * SEC, SEC))).unwrap();
        // 先等溢出发生（主线程此刻还没取过样），再结束来源
        wait_dropped(&source, SEC as u64);
        tx.send(SourceEvent::Ended).unwrap();
        let (all, end) = drain_all(&mut source);
        assert_eq!(end, SourceEvent::Ended);
        assert_eq!(all, ramp(SEC, 2 * SEC));
        assert_eq!(source.dropped(), SEC as u64);
    }

    /// `wait_ready` 等到打开尝试结束（RECORDING 回调已执行）才返回：模型比麦克风先就绪时，
    /// 先 RECORDING 后 PONG 的顺序有保证；打开失败同样会返回，不会白等到超时。
    #[test]
    fn wait_ready_returns_after_open_callback() {
        let order = Arc::new(Mutex::new(Vec::<&'static str>::new()));
        let in_cb = Arc::clone(&order);
        let mut source = PumpedSource::start(
            || {
                std::thread::sleep(Duration::from_millis(60));
                Ok(Box::new(ChannelSource(mpsc::channel().1)) as Box<dyn AudioSource>)
            },
            move |_| in_cb.lock().unwrap().push("recording"),
            SEC,
        );
        source.wait_ready(Duration::from_secs(5));
        order.lock().unwrap().push("ready");
        assert_eq!(*order.lock().unwrap(), ["recording", "ready"]);

        let started = Instant::now();
        let mut failing = PumpedSource::start(
            || {
                std::thread::sleep(Duration::from_millis(30));
                Err("被拒绝".to_string())
            },
            |_| {},
            SEC,
        );
        failing.wait_ready(Duration::from_secs(5));
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "打开失败也应及时返回"
        );
        // 默认实现（无缓存来源）立即返回
        let mut plain = crate::source::WavSource::from_samples(Vec::new());
        let t = Instant::now();
        plain.wait_ready(Duration::from_secs(5));
        assert!(t.elapsed() < Duration::from_millis(500));
    }

    /// 打开失败：回调收到原因，之后取样报同一条错误；打开成功：回调收到 Ok。
    #[test]
    fn open_result_is_reported_once() {
        let (tx, rx) = mpsc::channel();
        let tx_ok = tx.clone();
        let mut ok = PumpedSource::start(
            || Ok(Box::new(ChannelSource(mpsc::channel().1)) as Box<dyn AudioSource>),
            move |r| tx_ok.send(r.clone()).unwrap(),
            SEC,
        );
        assert_eq!(rx.recv_timeout(Duration::from_secs(5)).unwrap(), Ok(()));
        // 假来源的发送端已丢弃，视为来源结束
        let (all, end) = drain_all(&mut ok);
        assert!(all.is_empty());
        assert_eq!(end, SourceEvent::Ended);

        let mut bad = PumpedSource::start(
            || Err("麦克风不可用: 被拒绝".to_string()),
            move |r| tx.send(r.clone()).unwrap(),
            SEC,
        );
        assert_eq!(
            rx.recv_timeout(Duration::from_secs(5)).unwrap(),
            Err("麦克风不可用: 被拒绝".to_string())
        );
        let (all, end) = drain_all(&mut bad);
        assert!(all.is_empty());
        assert_eq!(end, SourceEvent::Failed("麦克风不可用: 被拒绝".into()));
    }

    /// `drain_pending` 一次取走缓存里剩下的全部（不受单批 1 s 限制）。
    #[test]
    fn drain_pending_takes_everything() {
        let (mut source, tx) = start_channel(10 * SEC);
        tx.send(SourceEvent::Samples(ramp(0, 3 * SEC))).unwrap();
        wait_pending(&source, 3 * SEC);
        assert_eq!(source.drain_pending(), ramp(0, 3 * SEC));
        assert!(source.drain_pending().is_empty());
    }

    /// 丢弃来源会让采集线程退出（来源随线程释放）；线程里的来源被丢弃证明麦克风会关。
    #[test]
    fn dropping_source_stops_and_releases_inner() {
        struct Flag(Arc<std::sync::atomic::AtomicBool>);
        impl AudioSource for Flag {
            fn next(&mut self, timeout: Duration) -> SourceEvent {
                std::thread::sleep(timeout.min(Duration::from_millis(5)));
                SourceEvent::Idle
            }
        }
        impl Drop for Flag {
            fn drop(&mut self) {
                self.0.store(true, std::sync::atomic::Ordering::SeqCst);
            }
        }
        let released = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let flag = Arc::clone(&released);
        let source = PumpedSource::start(
            move || Ok(Box::new(Flag(flag)) as Box<dyn AudioSource>),
            |_| {},
            SEC,
        );
        std::thread::sleep(Duration::from_millis(20));
        drop(source);
        assert!(released.load(std::sync::atomic::Ordering::SeqCst));
    }
}
