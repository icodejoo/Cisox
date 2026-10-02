//! 语音转文字会话引擎：管一个 snow-stt 工作进程从拉起到退出的状态机。
//!
//! 与进程、时钟、文字输出都解耦：进程经 [`SttLink`] 抽象，时间由调用方传入，
//! 结果以 [`Effect`] 列表返回，所以整条生命周期（含各种超时、崩溃、取消）都能离屏单测。
//!
//! 协议里 READY 在进程一启动就发出，之后才有 START → 加载模型 → 打开麦克风；
//! 加载完成没有专门的事件，所以 START 之后立刻补一条 PING：worker 只在会话主循环里应答 PONG，
//! 收到 PONG（或第一条识别事件）就说明模型已加载、麦克风已开，可以进入“正在听”。

use super::status::Failure;
use snow_stt_protocol::{Command, Event, StartRequest};
use std::time::{Duration, Instant};

/// 等待进程就绪（READY）的上限；冷启动要装载 DLL，给足时间。
pub const READY_TIMEOUT: Duration = Duration::from_secs(120);
/// START 之后等待模型加载完成（PONG）的上限；冷加载大模型可能很久。
pub const LOAD_TIMEOUT: Duration = Duration::from_secs(120);
/// 发出 STOP 后等待 STOPPED 的上限。
pub const STOP_TIMEOUT: Duration = Duration::from_secs(10);
/// 改发 CANCEL 后再等待多久，仍未退出就强制结束进程。
pub const CANCEL_GRACE: Duration = Duration::from_secs(3);
/// 收到 STOPPED 后等待进程自行退出的宽限期。
pub const EXIT_GRACE: Duration = Duration::from_secs(2);
/// 应用退出时等待进程响应 CANCEL 的宽限期。
pub const SHUTDOWN_GRACE: Duration = Duration::from_millis(800);

/// 超时配置（测试可缩短）。
#[derive(Debug, Clone, Copy)]
pub struct Timeouts {
    /// 等待 READY。
    pub ready: Duration,
    /// 等待加载完成。
    pub load: Duration,
    /// 等待 STOPPED。
    pub stop: Duration,
    /// CANCEL 之后的宽限。
    pub cancel: Duration,
}

impl Default for Timeouts {
    /// 取模块级常量。
    fn default() -> Self {
        Self {
            ready: READY_TIMEOUT,
            load: LOAD_TIMEOUT,
            stop: STOP_TIMEOUT,
            cancel: CANCEL_GRACE,
        }
    }
}

/// 通道上收到的事件。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LinkEvent {
    /// 工作进程发来的协议事件。
    Event(Event),
    /// 工作进程已退出（读线程见到 EOF）；退出码取不到为 `None`。
    Exited {
        /// 退出码。
        code: Option<i32>,
    },
}

/// 与工作进程之间的通道。真实实现见 `client.rs`，测试用 Fake。
pub trait SttLink {
    /// 向工作进程发一条命令。
    fn send(&mut self, command: &Command) -> Result<(), String>;

    /// 取走已收到的事件。
    fn poll(&mut self) -> Vec<LinkEvent>;

    /// 结束进程：关闭 stdin，最多等 `grace` 让它自行退出，仍在则强制结束（只动自己拉起的那个进程）。
    fn shutdown(&mut self, grace: Duration);
}

/// 一次启动所需的材料：已拉起的通道与 START 请求。
pub struct Launch {
    /// 与工作进程的通道。
    pub link: Box<dyn SttLink>,
    /// 就绪后发送的 START 请求。
    pub request: StartRequest,
}

/// 引擎对外的效果，由调用方落实到界面 / 键入。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Effect {
    /// 新一轮开始（清空上一轮的文字）。
    Begin,
    /// 进程已拉起，正在加载。
    Loading,
    /// 模型已加载、麦克风已开，正在听。
    Listening,
    /// 当前句的未落定文本。
    Partial(String),
    /// 一句话落定。
    Final(String),
    /// 已请求结束，等待收尾。
    Finishing,
    /// 正常结束（进程已释放）。
    Done,
    /// 失败结束（进程已释放）。
    Failed(Failure),
}

/// 会话阶段。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Phase {
    /// 没有会话。
    Idle,
    /// 等待 READY。
    WaitReady { deadline: Instant },
    /// START 已发出，等待加载完成。
    Loading { deadline: Instant },
    /// 正在听。
    Listening,
    /// 已请求结束；`cancelled` 为真表示已升级成 CANCEL。
    Stopping { deadline: Instant, cancelled: bool },
}

/// 语音转文字会话引擎。
pub struct Engine {
    /// 当前阶段。
    phase: Phase,
    /// 当前会话的通道与请求。
    session: Option<(Box<dyn SttLink>, StartRequest)>,
    /// 超时配置。
    timeouts: Timeouts,
}

impl Default for Engine {
    /// 空闲引擎，使用默认超时。
    fn default() -> Self {
        Self::new(Timeouts::default())
    }
}

impl Engine {
    /// 创建空闲引擎。
    ///
    /// # 参数
    /// - `timeouts`：超时配置。
    pub fn new(timeouts: Timeouts) -> Self {
        Self {
            phase: Phase::Idle,
            session: None,
            timeouts,
        }
    }

    /// 是否有进行中的会话（含收尾阶段）。
    pub fn active(&self) -> bool {
        self.phase != Phase::Idle
    }

    /// 是否已经在听（READY 之后加载完成）。
    pub fn listening(&self) -> bool {
        self.phase == Phase::Listening
    }

    /// 开始一轮。已有会话时忽略（按住说话的重复按下、切换式的连点）。
    ///
    /// # 参数
    /// - `now`：当前时间。
    /// - `launch`：已拉起的进程材料，或拉起失败的原因。
    pub fn start(&mut self, now: Instant, launch: Result<Launch, Failure>) -> Vec<Effect> {
        if self.active() {
            return Vec::new();
        }
        match launch {
            Err(failure) => vec![Effect::Begin, Effect::Failed(failure)],
            Ok(launch) => {
                self.session = Some((launch.link, launch.request));
                self.phase = Phase::WaitReady {
                    deadline: now + self.timeouts.ready,
                };
                vec![Effect::Begin, Effect::Loading]
            }
        }
    }

    /// 请求结束。
    ///
    /// 已经在听：发 STOP，让 worker 冲刷尾部（会再来一条 FINAL）后 STOPPED；
    /// 还在加载：没有任何音频可冲刷，直接结束进程；没有会话或已在收尾：忽略。
    ///
    /// # 参数
    /// - `now`：当前时间。
    pub fn stop(&mut self, now: Instant) -> Vec<Effect> {
        match self.phase {
            Phase::Idle | Phase::Stopping { .. } => Vec::new(),
            Phase::WaitReady { .. } | Phase::Loading { .. } => {
                self.finish(Duration::ZERO, Effect::Done)
            }
            Phase::Listening => {
                if let Err(e) = self.send(&Command::Stop) {
                    return self.finish(Duration::ZERO, Effect::Failed(Failure::Link(e)));
                }
                self.phase = Phase::Stopping {
                    deadline: now + self.timeouts.stop,
                    cancelled: false,
                };
                vec![Effect::Finishing]
            }
        }
    }

    /// 取走通道事件并检查各阶段的超时。
    ///
    /// # 参数
    /// - `now`：当前时间。
    pub fn poll(&mut self, now: Instant) -> Vec<Effect> {
        let mut out = Vec::new();
        let events = self
            .session
            .as_mut()
            .map(|(link, _)| link.poll())
            .unwrap_or_default();
        for event in events {
            if !self.active() {
                break;
            }
            out.extend(self.on_link_event(now, event));
        }
        if self.active() {
            out.extend(self.check_deadline(now));
        }
        out
    }

    /// 应用退出：尽力让工作进程取消并结束，绝不长时间阻塞。
    pub fn shutdown(&mut self) {
        if self.session.is_some() {
            let _ = self.send(&Command::Cancel);
            self.finish(SHUTDOWN_GRACE, Effect::Done);
        }
    }

    /// 向工作进程发命令。
    fn send(&mut self, command: &Command) -> Result<(), String> {
        match self.session.as_mut() {
            Some((link, _)) => link.send(command),
            None => Err("没有进行中的会话".to_string()),
        }
    }

    /// 结束会话：释放进程并返回结束效果。
    fn finish(&mut self, grace: Duration, effect: Effect) -> Vec<Effect> {
        if let Some((mut link, _)) = self.session.take() {
            link.shutdown(grace);
        }
        self.phase = Phase::Idle;
        vec![effect]
    }

    /// 处理一条通道事件。
    fn on_link_event(&mut self, now: Instant, event: LinkEvent) -> Vec<Effect> {
        match event {
            LinkEvent::Exited { code } => {
                if matches!(self.phase, Phase::Stopping { .. }) {
                    self.finish(Duration::ZERO, Effect::Done)
                } else {
                    self.finish(Duration::ZERO, Effect::Failed(Failure::Crashed(code)))
                }
            }
            LinkEvent::Event(Event::Ready) => self.on_ready(now),
            LinkEvent::Event(Event::Pong) => {
                if matches!(self.phase, Phase::Loading { .. }) {
                    self.phase = Phase::Listening;
                    vec![Effect::Listening]
                } else {
                    Vec::new()
                }
            }
            LinkEvent::Event(Event::Partial(text)) => self.on_text(Effect::Partial(text)),
            LinkEvent::Event(Event::Final(text)) => self.on_text(Effect::Final(text)),
            LinkEvent::Event(Event::Error(reason)) => {
                self.finish(Duration::ZERO, Effect::Failed(Failure::Worker(reason)))
            }
            LinkEvent::Event(Event::Stopped) => self.finish(EXIT_GRACE, Effect::Done),
        }
    }

    /// READY：发 START 与 PING，进入加载阶段。
    fn on_ready(&mut self, now: Instant) -> Vec<Effect> {
        if !matches!(self.phase, Phase::WaitReady { .. }) {
            return Vec::new();
        }
        let Some(request) = self.session.as_ref().map(|(_, r)| r.clone()) else {
            return Vec::new();
        };
        for command in [Command::Start(request), Command::Ping] {
            if let Err(e) = self.send(&command) {
                return self.finish(Duration::ZERO, Effect::Failed(Failure::Link(e)));
            }
        }
        self.phase = Phase::Loading {
            deadline: now + self.timeouts.load,
        };
        Vec::new()
    }

    /// 识别文本：加载阶段收到说明已经在听（兼容不回 PONG 的旧 worker）。
    fn on_text(&mut self, effect: Effect) -> Vec<Effect> {
        match self.phase {
            Phase::Loading { .. } => {
                self.phase = Phase::Listening;
                vec![Effect::Listening, effect]
            }
            Phase::Listening | Phase::Stopping { .. } => vec![effect],
            Phase::Idle | Phase::WaitReady { .. } => Vec::new(),
        }
    }

    /// 检查超时：启动 / 加载超时直接结束；收尾超时先 CANCEL，再超时强制结束。
    fn check_deadline(&mut self, now: Instant) -> Vec<Effect> {
        match self.phase {
            Phase::WaitReady { deadline } | Phase::Loading { deadline } if now >= deadline => {
                let _ = self.send(&Command::Cancel);
                self.finish(Duration::ZERO, Effect::Failed(Failure::StartTimeout))
            }
            Phase::Stopping {
                deadline,
                cancelled: false,
            } if now >= deadline => {
                let _ = self.send(&Command::Cancel);
                self.phase = Phase::Stopping {
                    deadline: now + self.timeouts.cancel,
                    cancelled: true,
                };
                Vec::new()
            }
            Phase::Stopping {
                deadline,
                cancelled: true,
            } if now >= deadline => {
                self.finish(Duration::ZERO, Effect::Failed(Failure::StopTimeout))
            }
            _ => Vec::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::collections::VecDeque;
    use std::rc::Rc;

    /// Fake 通道的共享记录。
    #[derive(Default)]
    struct Shared {
        inbox: VecDeque<LinkEvent>,
        sent: Vec<Command>,
        shutdowns: Vec<Duration>,
        fail_send: bool,
    }

    /// Fake 通道。
    struct FakeLink(Rc<RefCell<Shared>>);

    impl SttLink for FakeLink {
        fn send(&mut self, command: &Command) -> Result<(), String> {
            let mut s = self.0.borrow_mut();
            if s.fail_send {
                return Err("broken pipe".into());
            }
            s.sent.push(command.clone());
            Ok(())
        }
        fn poll(&mut self) -> Vec<LinkEvent> {
            self.0.borrow_mut().inbox.drain(..).collect()
        }
        fn shutdown(&mut self, grace: Duration) {
            self.0.borrow_mut().shutdowns.push(grace);
        }
    }

    /// 构造引擎、共享记录与起点时间，并开始一轮。
    fn started() -> (Engine, Rc<RefCell<Shared>>, Instant) {
        let shared = Rc::new(RefCell::new(Shared::default()));
        let mut engine = Engine::new(Timeouts::default());
        let now = Instant::now();
        let request = StartRequest {
            language: "auto".into(),
            threads: 2,
            endpoint: Default::default(),
            max_seconds: 0,
            model_dir: "D:/m".into(),
        };
        let effects = engine.start(
            now,
            Ok(Launch {
                link: Box::new(FakeLink(Rc::clone(&shared))),
                request,
            }),
        );
        assert_eq!(effects, vec![Effect::Begin, Effect::Loading]);
        (engine, shared, now)
    }

    /// 往 Fake 通道塞事件。
    fn feed(shared: &Rc<RefCell<Shared>>, events: impl IntoIterator<Item = LinkEvent>) {
        shared.borrow_mut().inbox.extend(events);
    }

    /// 协议事件的简写。
    fn ev(e: Event) -> LinkEvent {
        LinkEvent::Event(e)
    }

    /// 走到“正在听”。
    fn listening() -> (Engine, Rc<RefCell<Shared>>, Instant) {
        let (mut engine, shared, now) = started();
        feed(&shared, [ev(Event::Ready), ev(Event::Pong)]);
        assert_eq!(engine.poll(now), vec![Effect::Listening]);
        (engine, shared, now)
    }

    /// 完整流程：READY 后发 START + PING，PONG 进入听，文本转发，STOP 后 FINAL + STOPPED 正常收尾并释放进程。
    #[test]
    fn full_happy_path() {
        let (mut engine, shared, now) = started();
        feed(&shared, [ev(Event::Ready)]);
        assert!(engine.poll(now).is_empty());
        assert!(matches!(
            shared.borrow().sent.as_slice(),
            [Command::Start(_), Command::Ping]
        ));
        assert!(!engine.listening());

        feed(
            &shared,
            [
                ev(Event::Pong),
                ev(Event::Partial("你".into())),
                ev(Event::Final("你好".into())),
            ],
        );
        assert_eq!(
            engine.poll(now),
            vec![
                Effect::Listening,
                Effect::Partial("你".into()),
                Effect::Final("你好".into())
            ]
        );
        assert!(engine.listening());

        assert_eq!(engine.stop(now), vec![Effect::Finishing]);
        assert_eq!(shared.borrow().sent.last(), Some(&Command::Stop));
        feed(
            &shared,
            [ev(Event::Final("世界".into())), ev(Event::Stopped)],
        );
        assert_eq!(
            engine.poll(now),
            vec![Effect::Final("世界".into()), Effect::Done]
        );
        assert!(!engine.active());
        assert_eq!(shared.borrow().shutdowns, vec![EXIT_GRACE]);
    }

    /// 旧 worker 不回 PONG：第一条识别文本也能把加载态推进到正在听。
    #[test]
    fn first_text_implies_listening() {
        let (mut engine, shared, now) = started();
        feed(&shared, [ev(Event::Ready), ev(Event::Partial("hi".into()))]);
        assert_eq!(
            engine.poll(now),
            vec![Effect::Listening, Effect::Partial("hi".into())]
        );
    }

    /// 等不到 READY：超过上限就失败并结束进程（先尽力发 CANCEL）。
    #[test]
    fn ready_timeout_fails() {
        let (mut engine, shared, now) = started();
        assert!(
            engine
                .poll(now + READY_TIMEOUT - Duration::from_secs(1))
                .is_empty()
        );
        assert_eq!(
            engine.poll(now + READY_TIMEOUT),
            vec![Effect::Failed(Failure::StartTimeout)]
        );
        assert!(!engine.active());
        assert_eq!(shared.borrow().shutdowns, vec![Duration::ZERO]);
    }

    /// READY 之后冷加载超时（一直不出 PONG）：同样失败，计时从 READY 到达时重新开始。
    #[test]
    fn load_timeout_fails_and_clock_restarts_at_ready() {
        let (mut engine, shared, now) = started();
        let ready_at = now + Duration::from_secs(100);
        feed(&shared, [ev(Event::Ready)]);
        assert!(engine.poll(ready_at).is_empty());
        // 已超过最初的 READY 上限，但加载计时从 READY 起算，还不该失败
        assert!(
            engine
                .poll(now + READY_TIMEOUT + Duration::from_secs(1))
                .is_empty()
        );
        assert_eq!(
            engine.poll(ready_at + LOAD_TIMEOUT),
            vec![Effect::Failed(Failure::StartTimeout)]
        );
    }

    /// 收尾超时：先升级为 CANCEL，再超时强制结束并报 StopTimeout。
    #[test]
    fn stop_timeout_escalates_to_cancel_then_kills() {
        let (mut engine, shared, now) = listening();
        engine.stop(now);
        assert!(
            engine
                .poll(now + STOP_TIMEOUT - Duration::from_millis(1))
                .is_empty()
        );
        let t1 = now + STOP_TIMEOUT;
        assert!(engine.poll(t1).is_empty());
        assert_eq!(shared.borrow().sent.last(), Some(&Command::Cancel));
        assert!(engine.active());
        assert_eq!(
            engine.poll(t1 + CANCEL_GRACE),
            vec![Effect::Failed(Failure::StopTimeout)]
        );
        assert!(!engine.active());
        assert_eq!(shared.borrow().shutdowns, vec![Duration::ZERO]);
    }

    /// 升级为 CANCEL 后 worker 及时回 STOPPED：正常结束，不算失败。
    #[test]
    fn cancel_after_stop_timeout_can_still_finish_cleanly() {
        let (mut engine, shared, now) = listening();
        engine.stop(now);
        engine.poll(now + STOP_TIMEOUT);
        feed(&shared, [ev(Event::Stopped)]);
        assert_eq!(
            engine.poll(now + STOP_TIMEOUT + Duration::from_millis(100)),
            vec![Effect::Done]
        );
    }

    /// 还没开始听就结束（按住说话点了一下）：直接释放进程，没有失败。
    #[test]
    fn stop_before_listening_aborts() {
        let (mut engine, shared, now) = started();
        assert_eq!(engine.stop(now), vec![Effect::Done]);
        assert!(!engine.active());
        assert_eq!(shared.borrow().shutdowns, vec![Duration::ZERO]);

        let (mut engine, shared, now) = started();
        feed(&shared, [ev(Event::Ready)]);
        engine.poll(now);
        assert_eq!(engine.stop(now), vec![Effect::Done]);
    }

    /// worker 上报 ERROR：失败并释放进程，原因原样带出。
    #[test]
    fn worker_error_fails() {
        let (mut engine, shared, now) = started();
        feed(
            &shared,
            [ev(Event::Ready), ev(Event::Error("模型缺失".into()))],
        );
        assert_eq!(
            engine.poll(now),
            vec![Effect::Failed(Failure::Worker("模型缺失".into()))]
        );
        assert!(!engine.active());
    }

    /// 听的过程中进程意外退出：失败并带退出码；收尾阶段退出算正常结束。
    #[test]
    fn crash_vs_exit_while_stopping() {
        let (mut engine, shared, now) = listening();
        feed(&shared, [LinkEvent::Exited { code: Some(-1) }]);
        assert_eq!(
            engine.poll(now),
            vec![Effect::Failed(Failure::Crashed(Some(-1)))]
        );

        let (mut engine, shared, now) = listening();
        engine.stop(now);
        feed(&shared, [LinkEvent::Exited { code: Some(0) }]);
        assert_eq!(engine.poll(now), vec![Effect::Done]);
    }

    /// 已有会话时再次 start 被忽略；启动失败只给 Begin + Failed 且不占用会话。
    #[test]
    fn start_is_exclusive() {
        let (mut engine, _shared, now) = started();
        let again = engine.start(now, Err(Failure::WorkerMissing));
        assert!(again.is_empty());
        assert!(engine.active());

        let mut idle = Engine::default();
        assert_eq!(
            idle.start(now, Err(Failure::WorkerMissing)),
            vec![Effect::Begin, Effect::Failed(Failure::WorkerMissing)]
        );
        assert!(!idle.active());
        assert!(idle.stop(now).is_empty());
    }

    /// 写命令失败（管道断了）：失败并释放。
    #[test]
    fn send_failure_ends_session() {
        let (mut engine, shared, now) = started();
        shared.borrow_mut().fail_send = true;
        feed(&shared, [ev(Event::Ready)]);
        assert_eq!(
            engine.poll(now),
            vec![Effect::Failed(Failure::Link("broken pipe".into()))]
        );
        assert!(!engine.active());
    }

    /// 应用退出：发 CANCEL、带短宽限释放进程；空闲时什么都不做。
    #[test]
    fn shutdown_cancels_and_releases() {
        let (mut engine, shared, _now) = listening();
        engine.shutdown();
        assert!(!engine.active());
        assert_eq!(shared.borrow().sent.last(), Some(&Command::Cancel));
        assert_eq!(shared.borrow().shutdowns, vec![SHUTDOWN_GRACE]);
        engine.shutdown();
        assert_eq!(shared.borrow().shutdowns.len(), 1);
    }

    /// 会话结束后遗留的事件被丢弃，不会误触发新效果。
    #[test]
    fn events_after_end_are_dropped() {
        let (mut engine, shared, now) = listening();
        feed(
            &shared,
            [ev(Event::Stopped), ev(Event::Partial("late".into()))],
        );
        assert_eq!(engine.poll(now), vec![Effect::Done]);
    }
}
