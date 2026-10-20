//! 一次识别会话的主循环：收命令、取音频、攒块喂后端、把事件翻译成协议事件。
//! 只依赖 `SttBackend` / `AudioSource` 两个抽象，可用 Fake 离屏测试。

use std::sync::mpsc::{Receiver, RecvTimeoutError, TryRecvError};
use std::time::{Duration, Instant};

use snow_stt_protocol::{Command, Event};

use crate::backend::{SttBackend, SttEvent};
use crate::source::{AudioSource, SourceEvent};

/// 每块喂给后端的样本数：16kHz 下 320ms。
pub const CHUNK_SAMPLES: usize = 5120;
/// 取样等待上限，同时决定命令响应延迟。
const SOURCE_POLL: Duration = Duration::from_millis(50);
/// 采样率，用于换算最长录音时长（与后端采样率同源）。
const SAMPLE_RATE: u64 = crate::sherpa::SAMPLE_RATE as u64;

/// 命令通道上的消息。
#[derive(Debug, Clone, PartialEq)]
pub enum Ctl {
    /// 解析成功的命令。
    Cmd(Command),
    /// 无法解析的一行（原因文本）。
    BadLine(String),
    /// stdin 已关闭（主程序退出或崩溃）。
    Eof,
}

/// 会话结束方式。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SessionEnd {
    /// 正常停止（STOP、来源读完或达到最长时长），已发 STOPPED。
    Stopped,
    /// 被取消，已发 STOPPED。
    Canceled,
    /// 管道断开，没有再发任何事件。
    Aborted,
    /// 来源出错，已发 ERROR。
    Failed,
}

/// 会话统计，用于实测输出。
#[derive(Debug, Default, Clone)]
pub struct SessionStats {
    /// 每块 feed+poll 的耗时（微秒）。
    pub chunk_micros: Vec<u64>,
    /// 累计喂入的样本数。
    pub fed_samples: u64,
}

/// 事件去重器：丢弃与上次相同的 Partial 与空文本，Final 后清空记忆。
struct Dedup {
    /// 上一次发出的临时文本。
    last_partial: String,
}

impl Dedup {
    /// 把后端事件转成要发出的协议事件（可能为空）。
    fn map(&mut self, ev: SttEvent) -> Option<Event> {
        match ev {
            SttEvent::Partial(t) => {
                if t.is_empty() || t == self.last_partial {
                    return None;
                }
                self.last_partial = t.clone();
                Some(Event::Partial(t))
            }
            SttEvent::Final(t) => {
                self.last_partial.clear();
                if t.is_empty() {
                    None
                } else {
                    Some(Event::Final(t))
                }
            }
            SttEvent::Failed(why) => Some(Event::Error(why)),
        }
    }
}

/// 运行一次识别会话直到结束。
///
/// # 参数
/// - `backend`：识别后端。
/// - `source`：音频来源。
/// - `ctl`：命令通道。
/// - `emit`：事件输出回调。
/// - `max_seconds`：最长录音秒数，0 表示不限；按实际喂给后端的样本计（含加载期间录下的积压）。
///
/// # 返回
/// 结束方式与统计。
///
/// 收到 STOP 时，来源里还没取走的积压（`AudioSource::drain_pending`）会先喂完再冲刷；
/// CANCEL / 管道断开则不喂。
pub fn run_session(
    backend: &mut dyn SttBackend,
    source: &mut dyn AudioSource,
    ctl: &Receiver<Ctl>,
    emit: &mut dyn FnMut(Event),
    max_seconds: u32,
) -> (SessionEnd, SessionStats) {
    let mut dedup = Dedup {
        last_partial: String::new(),
    };
    let mut stats = SessionStats::default();
    let mut buf: Vec<f32> = Vec::with_capacity(CHUNK_SAMPLES * 2);
    let limit = u64::from(max_seconds) * SAMPLE_RATE;

    loop {
        // 先处理所有待处理的命令
        loop {
            match ctl.try_recv() {
                Ok(Ctl::Cmd(Command::Stop)) => {
                    // 来源里还有没取走的积压（模型加载期间录下的音频）：先喂完再冲刷，不能丢
                    source.stop();
                    append_limited(&mut buf, &source.drain_pending(), stats.fed_samples, limit);
                    if feed_chunks(backend, &mut buf, &mut dedup, emit, &mut stats) {
                        return (SessionEnd::Failed, stats);
                    }
                    return finish(backend, source, ctl, &mut buf, &mut dedup, emit, stats);
                }
                Ok(Ctl::Cmd(Command::Cancel)) => {
                    emit(Event::Stopped);
                    return (SessionEnd::Canceled, stats);
                }
                Ok(Ctl::Cmd(Command::Ping)) => emit(Event::Pong),
                Ok(Ctl::Cmd(Command::Start(_))) => {
                    emit(Event::Error("识别已在进行，忽略重复 START".into()))
                }
                Ok(Ctl::BadLine(why)) => emit(Event::Error(why)),
                Ok(Ctl::Eof) | Err(TryRecvError::Disconnected) => {
                    return (SessionEnd::Aborted, stats);
                }
                Err(TryRecvError::Empty) => break,
            }
        }

        match source.next(SOURCE_POLL) {
            SourceEvent::Idle => {}
            SourceEvent::Ended => {
                return finish(backend, source, ctl, &mut buf, &mut dedup, emit, stats);
            }
            SourceEvent::Failed(why) => {
                emit(Event::Error(why));
                return (SessionEnd::Failed, stats);
            }
            SourceEvent::Samples(s) => {
                append_limited(&mut buf, &s, stats.fed_samples, limit);
                if feed_chunks(backend, &mut buf, &mut dedup, emit, &mut stats) {
                    return (SessionEnd::Failed, stats);
                }
                if limit > 0 && stats.fed_samples + buf.len() as u64 >= limit {
                    return finish(backend, source, ctl, &mut buf, &mut dedup, emit, stats);
                }
            }
        }
    }
}

/// 所有输入路径按剩余额度裁剪，尾部缓冲也计入已接收额度。
fn append_limited(buf: &mut Vec<f32>, samples: &[f32], fed: u64, limit: u64) {
    let remaining = if limit == 0 {
        samples.len()
    } else {
        limit.saturating_sub(fed).saturating_sub(buf.len() as u64) as usize
    };
    buf.extend_from_slice(&samples[..samples.len().min(remaining)]);
}

/// 把缓冲里凑满整块的样本依次喂给后端，每块之后取结果并发出事件；不足一块的尾部留在缓冲里。
///
/// # 参数
/// - `backend`：识别后端。
/// - `buf`：待喂样本缓冲，喂过的部分会被移除。
/// - `dedup`：事件去重器。
/// - `emit`：事件输出回调。
/// - `stats`：会话统计，按块累加。
///
/// # 返回
/// 后端失败（已发出 ERROR）时为 `true`，调用方据此结束会话。
fn feed_chunks(
    backend: &mut dyn SttBackend,
    buf: &mut Vec<f32>,
    dedup: &mut Dedup,
    emit: &mut dyn FnMut(Event),
    stats: &mut SessionStats,
) -> bool {
    while buf.len() >= CHUNK_SAMPLES {
        let t0 = Instant::now();
        backend.feed(&buf[..CHUNK_SAMPLES]);
        let events = backend.poll();
        stats.chunk_micros.push(t0.elapsed().as_micros() as u64);
        stats.fed_samples += CHUNK_SAMPLES as u64;
        buf.drain(..CHUNK_SAMPLES);
        if emit_all(events, dedup, emit) {
            return true;
        }
    }
    false
}

/// 把后端事件去重后依次发出。
///
/// # 返回
/// 其中含后端失败（已发出 ERROR）时为 `true`，调用方据此结束会话。
fn emit_all(events: Vec<SttEvent>, dedup: &mut Dedup, emit: &mut dyn FnMut(Event)) -> bool {
    let mut failed = false;
    for e in events {
        failed |= matches!(e, SttEvent::Failed(_));
        if let Some(out) = dedup.map(e) {
            emit(out);
        }
    }
    failed
}

/// 冲刷剩余样本与后端尾部，发出最后的事件和 STOPPED。
fn finish(
    backend: &mut dyn SttBackend,
    source: &mut dyn AudioSource,
    ctl: &Receiver<Ctl>,
    buf: &mut Vec<f32>,
    dedup: &mut Dedup,
    emit: &mut dyn FnMut(Event),
    mut stats: SessionStats,
) -> (SessionEnd, SessionStats) {
    source.stop();
    let mut events = Vec::new();
    if !buf.is_empty() {
        stats.fed_samples += buf.len() as u64;
        backend.feed(buf);
        buf.clear();
        events.extend(backend.poll());
    }
    events.extend(backend.begin_finish());
    if emit_all(events, dedup, emit) {
        return (SessionEnd::Failed, stats);
    }
    loop {
        match ctl.try_recv() {
            Ok(Ctl::Cmd(Command::Cancel)) => {
                emit(Event::Stopped);
                return (SessionEnd::Canceled, stats);
            }
            Ok(Ctl::Eof) | Err(TryRecvError::Disconnected) => return (SessionEnd::Aborted, stats),
            Ok(Ctl::Cmd(Command::Ping)) => emit(Event::Pong),
            Ok(_) => continue,
            Err(TryRecvError::Empty) => {}
        }
        if !backend.finishing() {
            break;
        }
        if emit_all(backend.poll(), dedup, emit) {
            return (SessionEnd::Failed, stats);
        }
        if !backend.finishing() {
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    emit(Event::Stopped);
    (SessionEnd::Stopped, stats)
}

/// 等待 START（空闲阶段）：回应 PING，遇到 STOP/CANCEL/断开则结束。
///
/// # 参数
/// - `ctl`：命令通道。
/// - `emit`：事件输出回调。
///
/// # 返回
/// 收到 START 时返回其请求；需要直接退出时返回 `None`（STOPPED 已按需发出）。
pub fn wait_for_start(
    ctl: &Receiver<Ctl>,
    emit: &mut dyn FnMut(Event),
) -> Option<snow_stt_protocol::StartRequest> {
    loop {
        match ctl.recv_timeout(Duration::from_secs(1)) {
            Ok(Ctl::Cmd(Command::Start(r))) => return Some(r),
            Ok(Ctl::Cmd(Command::Ping)) => emit(Event::Pong),
            Ok(Ctl::Cmd(Command::Stop | Command::Cancel)) => {
                emit(Event::Stopped);
                return None;
            }
            Ok(Ctl::BadLine(why)) => emit(Event::Error(why)),
            Ok(Ctl::Eof) | Err(RecvTimeoutError::Disconnected) => return None,
            Err(RecvTimeoutError::Timeout) => {}
        }
    }
}

#[cfg(test)]
mod tests {
    /// 同步后端收尾后不再 poll，防止把刚输出的 Final 再输出为 Partial。
    #[test]
    fn synchronous_backend_is_not_polled_after_finish() {
        /// 模拟收尾后仍保留最后一句的在线后端。
        struct SyncBackend(bool);
        impl SttBackend for SyncBackend {
            /// 测试不喂入音频。
            fn feed(&mut self, _: &[f32]) {}
            /// 已收尾后再次轮询即表明流程错误。
            fn poll(&mut self) -> Vec<SttEvent> {
                assert!(!self.0);
                vec![]
            }
            /// 返回一次尾句。
            fn finish(&mut self) -> Vec<SttEvent> {
                self.0 = true;
                vec![SttEvent::Final("tail".into())]
            }
        }
        let (tx, rx) = mpsc::channel();
        tx.send(Ctl::Cmd(Command::Stop)).unwrap();
        let mut events = Vec::new();
        let (end, _) = run_session(
            &mut SyncBackend(false),
            &mut Backlog(vec![]),
            &rx,
            &mut |event| events.push(event),
            0,
        );
        assert_eq!(end, SessionEnd::Stopped);
        assert_eq!(events, vec![Event::Final("tail".into()), Event::Stopped]);
    }

    /// 单个超大批次和 STOP 积压都严格限制为一秒，尾部也不能越界。
    #[test]
    fn all_feed_paths_obey_exact_sample_limit() {
        let mut backend = FakeBackend::new(vec![], vec![]);
        let (_, stats, _) = run(&mut backend, vec![silence(100_000)], vec![], 1);
        assert_eq!(stats.fed_samples, 16_000);
        assert_eq!(backend.fed, 16_000);
        let mut backend = FakeBackend::new(vec![], vec![]);
        let (_, stats, _) = run_with(
            &mut backend,
            &mut Backlog(vec![0.0; 100_000]),
            vec![Ctl::Cmd(Command::Stop)],
            1,
        );
        assert_eq!(stats.fed_samples, 16_000);
        assert_eq!(backend.fed, 16_000);
    }

    /// STOP 后先持续交出结果，期间 CANCEL 立即结束，不等待后台解码。
    #[test]
    fn stop_flush_emits_results_and_accepts_cancel() {
        /// 始终挂起收尾的假后端。
        struct Pending;
        impl SttBackend for Pending {
            /// 测试后端不需要音频。
            fn feed(&mut self, _: &[f32]) {}
            /// 已解码的结果仍可取出。
            fn poll(&mut self) -> Vec<SttEvent> {
                vec![SttEvent::Final("decoded".into())]
            }
            /// 若调用阻塞收尾说明接线错误。
            fn finish(&mut self) -> Vec<SttEvent> {
                panic!("blocking finish")
            }
            /// 启动异步收尾。
            fn begin_finish(&mut self) -> Vec<SttEvent> {
                self.poll()
            }
            /// 模拟尚未结束的推理。
            fn finishing(&self) -> bool {
                true
            }
        }
        let (tx, rx) = mpsc::channel();
        tx.send(Ctl::Cmd(Command::Stop)).unwrap();
        let mut events = Vec::new();
        let (end, _) = run_session(
            &mut Pending,
            &mut Backlog(vec![]),
            &rx,
            &mut |e| {
                if matches!(e, Event::Final(_)) {
                    tx.send(Ctl::Cmd(Command::Cancel)).unwrap();
                }
                events.push(e);
            },
            0,
        );
        assert_eq!(end, SessionEnd::Canceled);
        assert_eq!(events, vec![Event::Final("decoded".into()), Event::Stopped]);
    }

    use super::*;
    use std::collections::VecDeque;
    use std::sync::mpsc;

    /// 可编排的假后端：按第 N 次 poll 返回预设事件，finish 返回预设尾部。
    struct FakeBackend {
        /// 每次 poll 依次弹出一组事件。
        script: VecDeque<Vec<SttEvent>>,
        /// finish 的返回值。
        tail: Vec<SttEvent>,
        /// 累计喂入样本数。
        fed: usize,
        /// feed 调用次数。
        feeds: usize,
        /// 按喂入顺序记下的全部样本（核对顺序用）。
        heard: Vec<f32>,
    }

    impl FakeBackend {
        /// 构造假后端。
        fn new(script: Vec<Vec<SttEvent>>, tail: Vec<SttEvent>) -> Self {
            Self {
                script: script.into(),
                tail,
                fed: 0,
                feeds: 0,
                heard: Vec::new(),
            }
        }
    }

    impl SttBackend for FakeBackend {
        fn feed(&mut self, pcm: &[f32]) {
            self.fed += pcm.len();
            self.feeds += 1;
            self.heard.extend_from_slice(pcm);
        }
        fn poll(&mut self) -> Vec<SttEvent> {
            self.script.pop_front().unwrap_or_default()
        }
        fn finish(&mut self) -> Vec<SttEvent> {
            std::mem::take(&mut self.tail)
        }
    }

    /// 脚本化来源：依次吐出预设事件，用完后一直 Idle。
    struct ScriptSource(VecDeque<SourceEvent>);

    impl AudioSource for ScriptSource {
        fn next(&mut self, _t: Duration) -> SourceEvent {
            self.0.pop_front().unwrap_or(SourceEvent::Idle)
        }
    }

    /// 生成 n 个样本的静音批。
    fn silence(n: usize) -> SourceEvent {
        SourceEvent::Samples(vec![0.0; n])
    }

    fn p(t: &str) -> SttEvent {
        SttEvent::Partial(t.into())
    }
    fn f(t: &str) -> SttEvent {
        SttEvent::Final(t.into())
    }

    /// 跑一次会话并收集协议事件。
    fn run(
        backend: &mut FakeBackend,
        source: Vec<SourceEvent>,
        ctl_msgs: Vec<Ctl>,
        max_seconds: u32,
    ) -> (SessionEnd, SessionStats, Vec<Event>) {
        let (tx, rx) = mpsc::channel();
        for m in ctl_msgs {
            tx.send(m).unwrap();
        }
        // 保持发送端存活，避免被当作断开
        let mut out = Vec::new();
        let (end, stats) = run_session(
            backend,
            &mut ScriptSource(source.into()),
            &rx,
            &mut |e| out.push(e),
            max_seconds,
        );
        drop(tx);
        (end, stats, out)
    }

    #[test]
    fn partial_dedup_final_reset_and_source_end_flush() {
        let mut b = FakeBackend::new(
            vec![
                vec![p("你")],
                vec![p("你"), p("你好")],
                vec![f("你好")],
                vec![p("你好")],
            ],
            vec![f("再见")],
        );
        let src = vec![
            silence(CHUNK_SAMPLES),
            silence(CHUNK_SAMPLES),
            silence(CHUNK_SAMPLES),
            silence(CHUNK_SAMPLES),
            SourceEvent::Ended,
        ];
        let (end, stats, out) = run(&mut b, src, vec![], 0);
        assert_eq!(end, SessionEnd::Stopped);
        assert_eq!(
            out,
            vec![
                Event::Partial("你".into()),
                Event::Partial("你好".into()),
                Event::Final("你好".into()),
                Event::Partial("你好".into()), // Final 之后同文本视为新句
                Event::Final("再见".into()),
                Event::Stopped,
            ]
        );
        assert_eq!(stats.chunk_micros.len(), 4);
        assert_eq!(b.fed, CHUNK_SAMPLES * 4);
    }

    #[test]
    fn samples_are_rechunked_to_320ms() {
        let mut b = FakeBackend::new(vec![], vec![]);
        let src = vec![
            silence(1600),
            silence(4000),
            silence(1600),
            SourceEvent::Ended,
        ];
        let (_, _, out) = run(&mut b, src, vec![], 0);
        // 7200 样本：一整块 5120，尾部 2080 在收尾时再喂一次
        assert_eq!(b.feeds, 2);
        assert_eq!(b.fed, 7200);
        assert_eq!(out, vec![Event::Stopped]);
    }

    #[test]
    fn stop_command_flushes_tail() {
        let mut b = FakeBackend::new(vec![], vec![f("尾巴")]);
        let (end, _, out) = run(&mut b, vec![silence(100)], vec![Ctl::Cmd(Command::Stop)], 0);
        assert_eq!(end, SessionEnd::Stopped);
        assert_eq!(out, vec![Event::Final("尾巴".into()), Event::Stopped]);
    }

    #[test]
    fn cancel_skips_flush() {
        let mut b = FakeBackend::new(vec![], vec![f("不该出现")]);
        let (end, _, out) = run(&mut b, vec![], vec![Ctl::Cmd(Command::Cancel)], 0);
        assert_eq!(end, SessionEnd::Canceled);
        assert_eq!(out, vec![Event::Stopped]);
    }

    #[test]
    fn eof_aborts_silently() {
        let mut b = FakeBackend::new(vec![], vec![f("x")]);
        let (end, _, out) = run(&mut b, vec![], vec![Ctl::Eof], 0);
        assert_eq!(end, SessionEnd::Aborted);
        assert!(out.is_empty());
    }

    #[test]
    fn disconnected_channel_aborts() {
        let (tx, rx) = mpsc::channel::<Ctl>();
        drop(tx);
        let mut b = FakeBackend::new(vec![], vec![]);
        let mut out = Vec::new();
        let (end, _) = run_session(
            &mut b,
            &mut ScriptSource(VecDeque::new()),
            &rx,
            &mut |e| out.push(e),
            0,
        );
        assert_eq!(end, SessionEnd::Aborted);
        assert!(out.is_empty());
    }

    #[test]
    fn ping_gets_pong_and_bad_line_gets_error() {
        let mut b = FakeBackend::new(vec![], vec![]);
        let (_, _, out) = run(
            &mut b,
            vec![],
            vec![
                Ctl::Cmd(Command::Ping),
                Ctl::BadLine("未知命令: X".into()),
                Ctl::Cmd(Command::Stop),
            ],
            0,
        );
        assert_eq!(
            out,
            vec![
                Event::Pong,
                Event::Error("未知命令: X".into()),
                Event::Stopped
            ]
        );
    }

    #[test]
    fn source_failure_emits_error_only() {
        let mut b = FakeBackend::new(vec![], vec![f("丢弃")]);
        let (end, _, out) = run(
            &mut b,
            vec![SourceEvent::Failed("设备丢失".into())],
            vec![],
            0,
        );
        assert_eq!(end, SessionEnd::Failed);
        assert_eq!(out, vec![Event::Error("设备丢失".into())]);
    }

    #[test]
    fn backend_failure_emits_error_and_fails() {
        let mut b = FakeBackend::new(
            vec![vec![
                p("你"),
                SttEvent::Failed("[system:mic-denied] x".into()),
            ]],
            vec![f("丢弃")],
        );
        let src = vec![silence(CHUNK_SAMPLES), silence(CHUNK_SAMPLES)];
        let (end, _, out) = run(&mut b, src, vec![], 0);
        assert_eq!(end, SessionEnd::Failed);
        assert_eq!(
            out,
            vec![
                Event::Partial("你".into()),
                Event::Error("[system:mic-denied] x".into())
            ]
        );
    }

    #[test]
    fn failure_during_flush_skips_stopped() {
        let mut b = FakeBackend::new(vec![], vec![SttEvent::Failed("boom".into())]);
        let (end, _, out) = run(&mut b, vec![], vec![Ctl::Cmd(Command::Stop)], 0);
        assert_eq!(end, SessionEnd::Failed);
        assert_eq!(out, vec![Event::Error("boom".into())]);
    }

    /// 按精确样本数停止，最后一批只喂剩余额度。
    #[test]
    fn max_seconds_triggers_stop() {
        let mut b = FakeBackend::new(vec![], vec![f("够了")]);
        // 1 秒上限 = 16000 样本，第 4 批只接收剩余的 640 个样本
        let src = (0..10).map(|_| silence(CHUNK_SAMPLES)).collect();
        let (end, stats, out) = run(&mut b, src, vec![], 1);
        assert_eq!(end, SessionEnd::Stopped);
        assert_eq!(stats.fed_samples, 16000);
        assert_eq!(b.fed, 16000);
        assert_eq!(out, vec![Event::Final("够了".into()), Event::Stopped]);
    }

    /// 带积压的假来源：`next` 永远空闲，积压只通过 `drain_pending` 交出（模拟“模型加载期间录下的音频”）。
    struct Backlog(Vec<f32>);

    impl AudioSource for Backlog {
        fn next(&mut self, _t: Duration) -> SourceEvent {
            SourceEvent::Idle
        }
        fn drain_pending(&mut self) -> Vec<f32> {
            std::mem::take(&mut self.0)
        }
    }

    /// 跑一次会话，来源是给定的 `AudioSource`。
    fn run_with(
        backend: &mut FakeBackend,
        source: &mut dyn AudioSource,
        ctl_msgs: Vec<Ctl>,
        max_seconds: u32,
    ) -> (SessionEnd, SessionStats, Vec<Event>) {
        let (tx, rx) = mpsc::channel();
        for m in ctl_msgs {
            tx.send(m).unwrap();
        }
        let mut out = Vec::new();
        let (end, stats) = run_session(backend, source, &rx, &mut |e| out.push(e), max_seconds);
        drop(tx);
        (end, stats, out)
    }

    /// 间隔很短的脚本来源：依次交出预设事件，每次之间睡 1 ms（让采集线程不空转），用完后 `Ended`。
    struct PacedScript(VecDeque<SourceEvent>);

    impl AudioSource for PacedScript {
        fn next(&mut self, _t: Duration) -> SourceEvent {
            std::thread::sleep(Duration::from_millis(1));
            self.0.pop_front().unwrap_or(SourceEvent::Ended)
        }
    }

    /// 递增序列：值就是序号，便于核对喂入顺序。
    fn ramp(n: usize) -> Vec<f32> {
        (0..n).map(|i| i as f32).collect()
    }

    /// STOP 时来源里还有积压：先把积压喂完（整块 + 尾部），再冲刷后端并发 STOPPED；喂入顺序与积压一致。
    #[test]
    fn stop_feeds_pending_backlog_before_flush() {
        let mut b = FakeBackend::new(vec![vec![p("你")], vec![f("你好")]], vec![f("尾巴")]);
        let backlog = ramp(CHUNK_SAMPLES * 2 + 100);
        let mut src = Backlog(backlog.clone());
        let (end, stats, out) = run_with(&mut b, &mut src, vec![Ctl::Cmd(Command::Stop)], 0);
        assert_eq!(end, SessionEnd::Stopped);
        assert_eq!(b.heard, backlog, "积压按原序全部喂入");
        assert_eq!(b.feeds, 3, "两整块 + 收尾时的尾部");
        assert_eq!(stats.fed_samples, backlog.len() as u64);
        assert_eq!(
            out,
            vec![
                Event::Partial("你".into()),
                Event::Final("你好".into()),
                Event::Final("尾巴".into()),
                Event::Stopped
            ]
        );
    }

    /// CANCEL 不喂积压：直接 STOPPED。
    #[test]
    fn cancel_does_not_feed_backlog() {
        let mut b = FakeBackend::new(vec![], vec![f("不该出现")]);
        let mut src = Backlog(ramp(CHUNK_SAMPLES * 3));
        let (end, _, out) = run_with(&mut b, &mut src, vec![Ctl::Cmd(Command::Cancel)], 0);
        assert_eq!(end, SessionEnd::Canceled);
        assert_eq!(b.fed, 0);
        assert_eq!(out, vec![Event::Stopped]);
    }

    /// 积压喂入时后端失败：发 ERROR 并以失败结束，不再冲刷。
    #[test]
    fn backend_failure_while_feeding_backlog_fails() {
        let mut b = FakeBackend::new(vec![vec![SttEvent::Failed("boom".into())]], vec![f("丢弃")]);
        let mut src = Backlog(ramp(CHUNK_SAMPLES * 2));
        let (end, _, out) = run_with(&mut b, &mut src, vec![Ctl::Cmd(Command::Stop)], 0);
        assert_eq!(end, SessionEnd::Failed);
        assert_eq!(out, vec![Event::Error("boom".into())]);
    }

    /// 经真实环形缓存追赶：加载期间到达的音频先于之后的实时音频，按原序整段喂给后端；
    /// 来源读完时冲刷尾部。
    #[test]
    fn catch_up_through_ring_preserves_order() {
        use crate::preroll::{PumpedSource, cap_samples};
        let total = CHUNK_SAMPLES * 3 + 777;
        let all = ramp(total);
        // 拆成不规则的几段，模拟“开麦后陆续到达的包”
        let events: VecDeque<SourceEvent> = [0, 1000, 6000, 12_000, total]
            .windows(2)
            .map(|w| SourceEvent::Samples(all[w[0]..w[1]].to_vec()))
            .collect();
        let mut src = PumpedSource::start(
            move || Ok(Box::new(PacedScript(events)) as Box<dyn AudioSource>),
            |_| {},
            cap_samples(0),
        );
        let mut b = FakeBackend::new(vec![], vec![f("完")]);
        let (end, stats, out) = run_with(&mut b, &mut src, vec![], 0);
        assert_eq!(end, SessionEnd::Stopped);
        assert_eq!(b.heard, all, "一个样本不丢、顺序不乱");
        assert_eq!(stats.fed_samples, total as u64);
        assert_eq!(out, vec![Event::Final("完".into()), Event::Stopped]);
    }

    /// `max_seconds` 按实际喂入的样本计（从开麦算起，积压也计入，溢出丢掉的不计）：
    /// 1 s 上限下缓存只保留最新的 1 s，喂满这 1 s 会话即收尾。
    #[test]
    fn max_seconds_counts_backlog_audio() {
        use crate::preroll::{PumpedSource, cap_samples};
        let events: VecDeque<SourceEvent> = [SourceEvent::Samples(vec![0.0; 3 * 16_000])].into();
        let mut src = PumpedSource::start(
            move || Ok(Box::new(PacedScript(events)) as Box<dyn AudioSource>),
            |_| {},
            cap_samples(1),
        );
        let mut b = FakeBackend::new(vec![], vec![f("够了")]);
        let (end, stats, out) = run_with(&mut b, &mut src, vec![], 1);
        assert_eq!(end, SessionEnd::Stopped);
        // 缓存上限取 min(30 s, max_seconds)，1 s 上限下只保留最新的 1 s，喂满即停
        assert!(stats.fed_samples >= 16_000 && stats.fed_samples <= 16_000 + CHUNK_SAMPLES as u64);
        assert_eq!(out, vec![Event::Final("够了".into()), Event::Stopped]);
    }

    #[test]
    fn wait_for_start_handles_idle_commands() {
        use snow_stt_protocol::{EndpointRules, StartRequest};
        let req = StartRequest {
            backend: snow_stt_protocol::BackendKind::Local,
            language: "zh-en".into(),
            threads: 1,
            endpoint: EndpointRules::default(),
            max_seconds: 0,
            model_dir: "m".into(),
            ..Default::default()
        };
        let (tx, rx) = mpsc::channel();
        tx.send(Ctl::Cmd(Command::Ping)).unwrap();
        tx.send(Ctl::Cmd(Command::Start(req.clone()))).unwrap();
        let mut out = Vec::new();
        assert_eq!(wait_for_start(&rx, &mut |e| out.push(e)), Some(req));
        assert_eq!(out, vec![Event::Pong]);

        tx.send(Ctl::Cmd(Command::Stop)).unwrap();
        let mut out = Vec::new();
        assert_eq!(wait_for_start(&rx, &mut |e| out.push(e)), None);
        assert_eq!(out, vec![Event::Stopped]);

        tx.send(Ctl::Eof).unwrap();
        let mut out = Vec::new();
        assert_eq!(wait_for_start(&rx, &mut |e| out.push(e)), None);
        assert!(out.is_empty());
    }
}
