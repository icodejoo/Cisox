//! 语音转文字会话引擎：管一个 cisox-stt 工作进程从拉起到退出的状态机。
//!
//! 与进程、时钟、文字输出都解耦：进程经 [`SttLink`] 抽象，时间由调用方传入，
//! 结果以 [`Effect`] 列表返回，所以整条生命周期（含各种超时、崩溃、取消）都能离屏单测。
//!
//! 协议里 READY 在进程一启动就发出，之后是 START。本地后端的 worker 收到 START 先开麦
//! （环形缓存，边录边存）、同时加载模型：麦克风开好发 RECORDING（此时已在录音，识别稍后），
//! 模型加载完才进入会话主循环并应答 PING。所以 START 之后立刻补一条 PING：收到 RECORDING 进入
//! “录音中”，收到 PONG（或第一条识别事件）才算“正在听”（识别已就绪）。
//! 旧 worker / 系统语音后端不发 RECORDING，从“加载中”直接到“正在听”。
//!
//! 结束语义：还没开始录音（没收到 RECORDING）就结束，没有任何音频，直接结束进程；
//! 已在录音（含模型还在加载）就发 STOP，worker 等加载完、喂完积压再冲刷出字。
//! 预拉起的 worker（UI `main` 最早处已拉起并发过 START，见 `prespawn.rs`）用 [`Launch::started`] 交接，
//! 引擎不再重复发 START。

use super::status::Failure;
use snow_stt_protocol::{Command, Event, StartRequest, SystemError};
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

    /// 预拉起的通道被会话认领时调用，用于记录“认领”时间点（默认什么都不做）。
    fn note_claimed(&mut self) {}
}

/// 一次启动所需的材料：已拉起的通道与 START 请求。
pub struct Launch {
    /// 与工作进程的通道。
    pub link: Box<dyn SttLink>,
    /// 就绪后发送的 START 请求。
    pub request: StartRequest,
    /// START（与随后的 PING）是否已经发出：预拉起的 worker 为真，引擎直接进入“加载中”，
    /// 不再等 READY、也不重复发 START；普通启动为假。
    pub started: bool,
}

/// 引擎对外的效果，由调用方落实到界面 / 键入。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Effect {
    /// 新一轮开始（清空上一轮的文字）。
    Begin,
    /// 进程已拉起，正在加载（麦克风还没开始录音）。
    Loading,
    /// 麦克风已开、已在录音，模型还在加载：此刻说的话被缓存，加载完会补识别。
    Recording,
    /// 模型已加载、识别已就绪，正在听。
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
    /// START 已发出，等待麦克风开好 / 模型加载完成。
    Loading { deadline: Instant },
    /// 已在录音（收到 RECORDING），模型还在加载；`deadline` 沿用加载期限。
    Recording { deadline: Instant },
    /// 正在听。
    Listening,
    /// 已请求结束；`cancelled` 为真表示已升级成 CANCEL；
    /// `awaiting_load` 为真表示结束请求发出时模型还没加载完，`deadline` 暂按加载期限算，
    /// 收到 PONG（加载完、开始喂积压）后改按 [`Timeouts::stop`] 重新计时。
    Stopping {
        deadline: Instant,
        cancelled: bool,
        awaiting_load: bool,
    },
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

    /// 识别是否还没就绪（READY 之前、加载中、或已在录音但模型还在加载）。
    ///
    /// 此时说的话不会立即出字；其中“已在录音”的那段由 [`Engine::recording`] 区分。
    pub fn loading(&self) -> bool {
        matches!(
            self.phase,
            Phase::WaitReady { .. } | Phase::Loading { .. } | Phase::Recording { .. }
        )
    }

    /// 是否已在录音但识别还没就绪（模型加载中）：说的话被缓存，加载完补识别。
    pub fn recording(&self) -> bool {
        matches!(self.phase, Phase::Recording { .. })
    }

    /// 是否处于收尾阶段（已发出结束，等待 STOPPED）。
    pub fn stopping(&self) -> bool {
        matches!(self.phase, Phase::Stopping { .. })
    }

    /// 强制立即结束：收尾阶段下发 CANCEL 并立刻释放进程；其余阶段忽略。
    ///
    /// # 返回
    /// 结束效果；不在收尾阶段返回空。
    pub fn force_stop(&mut self) -> Vec<Effect> {
        if !self.stopping() {
            return Vec::new();
        }
        let _ = self.send(&Command::Cancel);
        self.finish(Duration::ZERO, Effect::Done)
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
                self.phase = if launch.started {
                    Phase::Loading {
                        deadline: now + self.timeouts.load,
                    }
                } else {
                    Phase::WaitReady {
                        deadline: now + self.timeouts.ready,
                    }
                };
                vec![Effect::Begin, Effect::Loading]
            }
        }
    }

    /// 请求结束。
    ///
    /// 已经在听：发 STOP，让 worker 冲刷尾部（会再来一条 FINAL）后 STOPPED；
    /// 已在录音但模型还在加载：同样发 STOP，worker 等加载完、把积压喂完再冲刷出字
    /// （等待期间按加载期限计时，PONG 到了再按 [`Timeouts::stop`] 计）；
    /// 还没开始录音（拉起 / 加载中、没收到 RECORDING）：没有任何音频可冲刷，直接结束进程；
    /// 没有会话或已在收尾：忽略。
    ///
    /// # 参数
    /// - `now`：当前时间。
    pub fn stop(&mut self, now: Instant) -> Vec<Effect> {
        match self.phase {
            Phase::Idle | Phase::Stopping { .. } => Vec::new(),
            Phase::WaitReady { .. } | Phase::Loading { .. } => {
                self.finish(Duration::ZERO, Effect::Done)
            }
            Phase::Recording { deadline } => self.request_stop(deadline, true),
            Phase::Listening => self.request_stop(now + self.timeouts.stop, false),
        }
    }

    /// 发 STOP 并进入收尾阶段。
    ///
    /// # 参数
    /// - `deadline`：收尾期限。
    /// - `awaiting_load`：发出时模型是否还没加载完。
    fn request_stop(&mut self, deadline: Instant, awaiting_load: bool) -> Vec<Effect> {
        if let Err(e) = self.send(&Command::Stop) {
            return self.finish(Duration::ZERO, Effect::Failed(Failure::Link(e)));
        }
        self.phase = Phase::Stopping {
            deadline,
            cancelled: false,
            awaiting_load,
        };
        vec![Effect::Finishing]
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
            LinkEvent::Event(Event::Pong) => match self.phase {
                Phase::Loading { .. } | Phase::Recording { .. } => {
                    self.phase = Phase::Listening;
                    vec![Effect::Listening]
                }
                // 结束请求发出时模型还在加载：PONG 说明加载完、开始喂积压，从现在起按收尾期限计
                Phase::Stopping { .. } => {
                    self.loaded_while_stopping(now);
                    Vec::new()
                }
                _ => Vec::new(),
            },
            LinkEvent::Event(Event::Recording) => {
                if let Phase::Loading { deadline } = self.phase {
                    self.phase = Phase::Recording { deadline };
                    vec![Effect::Recording]
                } else {
                    Vec::new()
                }
            }
            LinkEvent::Event(Event::Partial(text)) => self.on_text(now, Effect::Partial(text)),
            LinkEvent::Event(Event::Final(text)) => self.on_text(now, Effect::Final(text)),
            LinkEvent::Event(Event::Error(reason)) => {
                // 系统语音的可识别错误带类别标记，翻译成本地化提示；其余原样上报
                let failure = match SystemError::from_error_text(&reason) {
                    Some((kind, detail)) => Failure::SystemSpeech(kind, detail),
                    None => Failure::Worker(reason),
                };
                self.finish(Duration::ZERO, Effect::Failed(failure))
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

    /// 收尾阶段里确认模型已加载完：把“等加载”的期限换成正常的收尾期限（已升级成 CANCEL 的不动）。
    fn loaded_while_stopping(&mut self, now: Instant) {
        if let Phase::Stopping {
            awaiting_load: true,
            cancelled: false,
            ..
        } = self.phase
        {
            self.phase = Phase::Stopping {
                deadline: now + self.timeouts.stop,
                cancelled: false,
                awaiting_load: false,
            };
        }
    }

    /// 识别文本：加载 / 录音阶段收到说明已经在听（兼容不回 PONG 的旧 worker）。
    fn on_text(&mut self, now: Instant, effect: Effect) -> Vec<Effect> {
        match self.phase {
            Phase::Loading { .. } | Phase::Recording { .. } => {
                self.phase = Phase::Listening;
                vec![Effect::Listening, effect]
            }
            Phase::Listening => vec![effect],
            Phase::Stopping { .. } => {
                // 出字了就说明模型已经加载完
                self.loaded_while_stopping(now);
                vec![effect]
            }
            Phase::Idle | Phase::WaitReady { .. } => Vec::new(),
        }
    }

    /// 检查超时：启动 / 加载超时直接结束；收尾超时先 CANCEL，再超时强制结束。
    fn check_deadline(&mut self, now: Instant) -> Vec<Effect> {
        match self.phase {
            Phase::WaitReady { deadline }
            | Phase::Loading { deadline }
            | Phase::Recording { deadline }
                if now >= deadline =>
            {
                let _ = self.send(&Command::Cancel);
                self.finish(Duration::ZERO, Effect::Failed(Failure::StartTimeout))
            }
            // 结束请求发出后模型一直没加载完：和加载超时一样当作启动失败
            Phase::Stopping {
                deadline,
                cancelled: false,
                awaiting_load: true,
            } if now >= deadline => {
                let _ = self.send(&Command::Cancel);
                self.finish(Duration::ZERO, Effect::Failed(Failure::StartTimeout))
            }
            Phase::Stopping {
                deadline,
                cancelled: false,
                awaiting_load,
            } if now >= deadline => {
                let _ = self.send(&Command::Cancel);
                self.phase = Phase::Stopping {
                    deadline: now + self.timeouts.cancel,
                    cancelled: true,
                    awaiting_load,
                };
                Vec::new()
            }
            Phase::Stopping {
                deadline,
                cancelled: true,
                ..
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

    /// 构造引擎、共享记录与起点时间，并开始一轮（`prespawned` 为真模拟预拉起、START 已发出的交接）。
    fn started_with(prespawned: bool) -> (Engine, Rc<RefCell<Shared>>, Instant) {
        let shared = Rc::new(RefCell::new(Shared::default()));
        let mut engine = Engine::new(Timeouts::default());
        let now = Instant::now();
        let request = StartRequest {
            backend: Default::default(),
            language: "auto".into(),
            threads: 2,
            endpoint: Default::default(),
            max_seconds: 0,
            model_dir: "D:/m".into(),
            ..Default::default()
        };
        let effects = engine.start(
            now,
            Ok(Launch {
                link: Box::new(FakeLink(Rc::clone(&shared))),
                request,
                started: prespawned,
            }),
        );
        assert_eq!(effects, vec![Effect::Begin, Effect::Loading]);
        (engine, shared, now)
    }

    /// 构造引擎、共享记录与起点时间，并开始一轮。
    fn started() -> (Engine, Rc<RefCell<Shared>>, Instant) {
        started_with(false)
    }

    /// 走到“已在录音、模型还在加载”（READY → RECORDING）。
    fn recording() -> (Engine, Rc<RefCell<Shared>>, Instant) {
        let (mut engine, shared, now) = started();
        feed(&shared, [ev(Event::Ready), ev(Event::Recording)]);
        assert_eq!(engine.poll(now), vec![Effect::Recording]);
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

    /// 加载阶段 `loading()` 为真，听与收尾阶段为假。
    #[test]
    fn loading_only_before_listening() {
        let (mut engine, shared, now) = started();
        assert!(engine.loading());
        feed(&shared, [ev(Event::Ready)]);
        engine.poll(now);
        assert!(engine.loading());
        feed(&shared, [ev(Event::Pong)]);
        engine.poll(now);
        assert!(!engine.loading() && engine.listening());
        engine.stop(now);
        assert!(!engine.loading());
    }

    /// 收尾阶段强制结束：发 CANCEL、立刻释放进程；听的阶段不受影响。
    #[test]
    fn force_stop_only_while_stopping() {
        let (mut engine, shared, now) = listening();
        assert!(engine.force_stop().is_empty());
        assert!(engine.active() && !engine.stopping());
        engine.stop(now);
        assert!(engine.stopping());
        assert_eq!(engine.force_stop(), vec![Effect::Done]);
        assert!(!engine.active());
        assert_eq!(shared.borrow().sent.last(), Some(&Command::Cancel));
        assert_eq!(shared.borrow().shutdowns, vec![Duration::ZERO]);
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

    /// 两段式就绪：RECORDING 进入“录音中”（识别仍未就绪），PONG 才是“正在听”；
    /// 重复或迟到的 RECORDING 不产生效果。
    #[test]
    fn recording_then_pong_is_two_stage() {
        let (mut engine, shared, now) = started();
        feed(&shared, [ev(Event::Ready)]);
        assert!(engine.poll(now).is_empty());
        assert!(engine.loading() && !engine.recording() && !engine.listening());

        feed(&shared, [ev(Event::Recording)]);
        assert_eq!(engine.poll(now), vec![Effect::Recording]);
        assert!(engine.loading() && engine.recording() && !engine.listening());

        feed(&shared, [ev(Event::Recording)]);
        assert!(engine.poll(now).is_empty(), "重复的 RECORDING 忽略");

        feed(&shared, [ev(Event::Pong)]);
        assert_eq!(engine.poll(now), vec![Effect::Listening]);
        assert!(engine.listening() && !engine.recording() && !engine.loading());

        feed(&shared, [ev(Event::Recording)]);
        assert!(
            engine.poll(now).is_empty(),
            "已在听之后迟到的 RECORDING 忽略"
        );
        assert!(engine.listening());
    }

    /// 录音中收到第一条识别文本也说明加载完了（兼容不回 PONG 的 worker）。
    #[test]
    fn first_text_while_recording_implies_listening() {
        let (mut engine, shared, now) = recording();
        feed(&shared, [ev(Event::Partial("hi".into()))]);
        assert_eq!(
            engine.poll(now),
            vec![Effect::Listening, Effect::Partial("hi".into())]
        );
        assert!(engine.listening());
    }

    /// 按住说话：已在录音（模型还在加载）时松开，不丢弃——发 STOP，加载很久也不按收尾超时处理，
    /// 加载完后积压的识别结果与收尾文字都照常交出，最后正常结束。
    #[test]
    fn stop_while_recording_waits_for_load_and_backlog() {
        let (mut engine, shared, now) = recording();
        assert_eq!(engine.stop(now), vec![Effect::Finishing]);
        assert_eq!(shared.borrow().sent.last(), Some(&Command::Stop));
        assert!(engine.stopping() && engine.active());

        // 加载慢（远超 10 s 的收尾期限）：不升级成 CANCEL
        assert!(
            engine
                .poll(now + STOP_TIMEOUT + Duration::from_secs(20))
                .is_empty()
        );
        assert_eq!(shared.borrow().sent.last(), Some(&Command::Stop));

        // 加载完（PONG）后追赶积压出字，最后 STOPPED
        let loaded_at = now + Duration::from_secs(40);
        feed(&shared, [ev(Event::Pong)]);
        assert!(engine.poll(loaded_at).is_empty());
        feed(
            &shared,
            [
                ev(Event::Partial("你".into())),
                ev(Event::Final("你好吗".into())),
                ev(Event::Stopped),
            ],
        );
        assert_eq!(
            engine.poll(loaded_at),
            vec![
                Effect::Partial("你".into()),
                Effect::Final("你好吗".into()),
                Effect::Done
            ]
        );
        assert!(!engine.active());
        assert_eq!(shared.borrow().shutdowns, vec![EXIT_GRACE]);
    }

    /// 等加载的收尾：PONG 到了才按正常收尾期限（10 s）重新计时，之后仍照旧先 CANCEL 再强制结束。
    #[test]
    fn stop_deadline_restarts_when_load_finishes() {
        let (mut engine, shared, now) = recording();
        engine.stop(now);
        let loaded_at = now + Duration::from_secs(50);
        feed(&shared, [ev(Event::Pong)]);
        engine.poll(loaded_at);
        assert!(
            engine
                .poll(loaded_at + STOP_TIMEOUT - Duration::from_millis(1))
                .is_empty()
        );
        assert_eq!(shared.borrow().sent.last(), Some(&Command::Stop));
        assert!(engine.poll(loaded_at + STOP_TIMEOUT).is_empty());
        assert_eq!(shared.borrow().sent.last(), Some(&Command::Cancel));
        assert_eq!(
            engine.poll(loaded_at + STOP_TIMEOUT + CANCEL_GRACE),
            vec![Effect::Failed(Failure::StopTimeout)]
        );
    }

    /// 收尾时一直等不到模型加载完：到加载期限就 CANCEL 并按启动超时失败，不当作收尾超时。
    #[test]
    fn stop_while_recording_times_out_as_start_timeout() {
        let (mut engine, shared, now) = recording();
        engine.stop(now);
        assert!(
            engine
                .poll(now + LOAD_TIMEOUT - Duration::from_millis(1))
                .is_empty()
        );
        assert_eq!(
            engine.poll(now + LOAD_TIMEOUT),
            vec![Effect::Failed(Failure::StartTimeout)]
        );
        assert_eq!(shared.borrow().sent.last(), Some(&Command::Cancel));
        assert_eq!(shared.borrow().shutdowns, vec![Duration::ZERO]);
        assert!(!engine.active());
    }

    /// 收尾等加载期间先出了字（没回 PONG 的 worker）：同样视为加载完，按正常收尾期限计。
    #[test]
    fn text_while_stopping_also_marks_loaded() {
        let (mut engine, shared, now) = recording();
        engine.stop(now);
        let t = now + Duration::from_secs(60);
        feed(&shared, [ev(Event::Final("好".into()))]);
        assert_eq!(engine.poll(t), vec![Effect::Final("好".into())]);
        assert!(engine.poll(t + STOP_TIMEOUT).is_empty());
        assert_eq!(shared.borrow().sent.last(), Some(&Command::Cancel));
    }

    /// 还没录音（只是加载中）就松开：没有音频，直接结束，不发 STOP。
    #[test]
    fn stop_before_recording_sends_nothing() {
        let (mut engine, shared, now) = started();
        feed(&shared, [ev(Event::Ready)]);
        engine.poll(now);
        let sent_before = shared.borrow().sent.len();
        assert_eq!(engine.stop(now), vec![Effect::Done]);
        assert_eq!(shared.borrow().sent.len(), sent_before);
        assert!(!engine.active());
    }

    /// 预拉起的 worker：直接进入“加载中”，晚到的 READY 不会让引擎重复发 START / PING；
    /// 之后 RECORDING、PONG 照常推进，松开的语义也一样。
    #[test]
    fn prespawned_worker_skips_start_handshake() {
        let (mut engine, shared, now) = started_with(true);
        assert!(engine.loading() && !engine.recording());
        feed(&shared, [ev(Event::Ready)]);
        assert!(engine.poll(now).is_empty());
        assert!(
            shared.borrow().sent.is_empty(),
            "START/PING 已由预拉起方发过"
        );

        feed(&shared, [ev(Event::Recording)]);
        assert_eq!(engine.poll(now), vec![Effect::Recording]);
        feed(&shared, [ev(Event::Pong)]);
        assert_eq!(engine.poll(now), vec![Effect::Listening]);

        // 预拉起、还没开始录音就松开：同样直接结束
        let (mut engine, shared, now) = started_with(true);
        assert_eq!(engine.stop(now), vec![Effect::Done]);
        assert_eq!(shared.borrow().shutdowns, vec![Duration::ZERO]);
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
