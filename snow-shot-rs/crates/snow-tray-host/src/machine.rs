//! 宿主状态机：UI 进程的拉起、命令投递、退出仲裁、崩溃熔断（方案 §4）。
//!
//! 状态机是纯逻辑：输入是“发生了什么”（收到一条要转给 UI 的消息、UI 发来一条消息、进程退出了、
//! 时间到了），输出是 [`Effect`] 列表（拉起进程、写管道、结束进程、提示……），本身不碰管道、
//! 进程和时钟——当前时刻由调用方传入。这样“`exit_request` 与新命令交错”“UI 启动期间连发命令”
//! “UI 被杀”这类竞态都能确定性地测试，不需要真起进程。
//!
//! # 状态
//! `Absent → Starting → Running → Exiting → Absent`，另有 `Backoff`（熔断）。
//!
//! # 会话代数
//! 每次拉起 UI 进程递增一次 `generation`；管道事件、进程退出事件都带着它，状态机丢弃过期代数的
//! 事件，所以旧 UI 迟到的消息不会影响新 UI。

use crate::breaker::{BreakerState, CrashBreaker};
use crate::focus::{GRANT_ON_SPAWN, grant_for_forward};
use crate::queue::{CommandQueue, Outbound, PushOutcome};
use snow_host_protocol::launch::UiHint;
use snow_host_protocol::message::{GesturePhase, HelloError, HostMessage, UiMessage, check_hello};
use snow_host_protocol::seq::{ExitDecision, HostSeq};
use snow_host_protocol::{ACK_TIMEOUT, EXIT_WAIT_TIMEOUT, HELLO_TIMEOUT};
use std::collections::VecDeque;
use std::time::Instant;

/// 需要提示用户的事件（宿主没有 i18n，文案由运行时取快照里的字符串或内置兜底）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Notice {
    /// 界面进程启动失败（超时、握手不通过、拉不起来）。
    UiStartFailed,
    /// 界面进程反复异常退出，已进入退避。
    UiCrashLoop,
}

/// 状态机要求运行时去做的事。
#[derive(Debug, Clone, PartialEq)]
pub enum Effect {
    /// 先建会话管道、再 `CreateProcess` 拉起 UI；完成后调用 [`HostMachine::on_spawned`]
    /// 或 [`HostMachine::on_spawn_failed`]。
    SpawnUi {
        /// 本次会话的代数。
        generation: u64,
    },
    /// 把一条消息写进该代会话的管道。
    Send {
        /// 会话代数。
        generation: u64,
        /// 消息。
        message: HostMessage,
    },
    /// 关闭该代会话的管道（进程已退出或被判定为坏会话）。
    CloseSession {
        /// 会话代数。
        generation: u64,
    },
    /// 对 UI 进程调 `AllowSetForegroundWindow`。
    GrantForeground {
        /// UI 进程 ID。
        pid: u32,
    },
    /// 强制结束 UI 进程（最后手段：启动超时、握手不通过、退出超时）。
    KillUi {
        /// UI 进程 ID。
        pid: u32,
    },
    /// 提示用户。
    Notice(Notice),
    /// UI 发来的、需要运行时处理的消息（托盘提示、快照变更、热键重注册、活动状态）。
    FromUi(UiMessage),
    /// 一条消息重发一次后仍没等到确认，已放弃（运行时记日志）。
    GaveUp {
        /// 被放弃的命令序号。
        seq: u64,
    },
    /// UI 进程已经彻底退出（运行时安排一次工作集修剪）。
    UiGone,
    /// 宿主该退出了（`restart` 为真时先拉起延迟启动的新宿主）。
    ExitHost {
        /// 是否重启。
        restart: bool,
    },
}

/// 状态机所处的阶段（测试与日志用）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StateKind {
    /// 没有 UI 进程。
    Absent,
    /// 已拉起、等 `hello`。
    Starting,
    /// 握手完成，正常转发命令。
    Running,
    /// 正在退出（已同意退出或已要求关闭），等进程结束。
    Exiting,
    /// 熔断退避中。
    Backoff,
}

/// 启动阶段的数据。
#[derive(Debug)]
struct Starting {
    /// 会话代数。
    generation: u64,
    /// `CreateProcess` 得到的进程 ID（拉起结果回来之前为空）。
    pid: Option<u32>,
    /// 传给 UI 的一次性令牌。
    token: Option<String>,
    /// 等 `hello` 的截止时刻。
    deadline: Instant,
}

/// 退出阶段的数据。
#[derive(Debug)]
struct Exiting {
    /// UI 进程 ID。
    pid: u32,
    /// 等进程退出的截止时刻，过了就强制结束。
    deadline: Instant,
    /// 是否已经强制结束过（只结束一次）。
    kill_sent: bool,
}

/// 状态。
#[derive(Debug)]
enum State {
    /// 没有 UI 进程。
    Absent,
    /// 已拉起、等握手。
    Starting(Starting),
    /// 握手完成。
    Running {
        /// 会话代数。
        generation: u64,
        /// UI 进程 ID。
        pid: u32,
    },
    /// 正在退出。
    Exiting(Exiting),
    /// 熔断退避。
    Backoff {
        /// 退避结束时刻。
        until: Instant,
    },
}

/// 已写进管道、等待确认的一条消息。
#[derive(Debug, Clone)]
struct Inflight {
    /// 命令序号。
    seq: u64,
    /// 消息本体（重发用）。
    message: HostMessage,
    /// 最近一次写出的时刻。
    sent_at: Instant,
    /// 是否已经重发过一次。
    retried: bool,
}

/// 宿主状态机。
#[derive(Debug)]
pub struct HostMachine {
    /// 当前状态。
    state: State,
    /// 命令队列（UI 没就绪时存放）。
    queue: CommandQueue,
    /// 崩溃熔断器。
    breaker: CrashBreaker,
    /// 当前会话的序号分配与退出仲裁。
    seq: HostSeq,
    /// 等确认的消息（按序号升序）。
    inflight: VecDeque<Inflight>,
    /// 最近一次拉起的会话代数。
    generation: u64,
    /// 宿主是否正在退出（`Some(restart)`）。
    quit: Option<bool>,
    /// UI 最近一次上报的贴图数（崩溃后据此决定要不要自动恢复）。
    pins: u32,
    /// 管道服务端看到的客户端进程 ID（用来核对 `hello` 里自报的 PID）。
    connected_pid: Option<u32>,
    /// 快照要求 UI 常驻（MCP / OCR 预热）：崩溃或退避结束后宿主要补拉一个。
    keep_alive: bool,
    /// 最近一次拉起 UI 时带的启动提示（拉起那一刻队首命令的种类）。
    spawn_hint: UiHint,
}

impl Default for HostMachine {
    /// 同 [`HostMachine::new`]。
    fn default() -> Self {
        Self::new()
    }
}

impl HostMachine {
    /// 新建（没有 UI 进程）。
    pub fn new() -> Self {
        Self {
            state: State::Absent,
            queue: CommandQueue::new(),
            breaker: CrashBreaker::new(),
            seq: HostSeq::new(),
            inflight: VecDeque::new(),
            generation: 0,
            quit: None,
            pins: 0,
            connected_pid: None,
            keep_alive: false,
            spawn_hint: UiHint::None,
        }
    }

    /// 最近一次 `SpawnUi` 要带给 UI 的启动提示：拉起那一刻队首命令是语音转文字就是
    /// [`UiHint::Dictation`]，让 UI 在 `main` 最早处预拉语音工作进程；其余情况没有提示。
    pub fn spawn_hint(&self) -> UiHint {
        self.spawn_hint
    }

    /// 设置“UI 要常驻”标志（来自快照的保活原因，见 `HostFlags::needs_resident_ui`）。
    ///
    /// 置位后，UI 异常退出（没跳闸）会补拉一个，熔断退避结束时也会补拉；正常的 `exit_ok` 退出不补拉。
    ///
    /// # 参数
    /// - `keep_alive`：是否需要常驻。
    pub fn set_keep_alive(&mut self, keep_alive: bool) {
        self.keep_alive = keep_alive;
    }

    /// 当前是否要求 UI 常驻。
    pub fn keep_alive(&self) -> bool {
        self.keep_alive
    }

    /// UI 当前是否在运行或正在启动（可以直接收命令的状态，用来决定 set-config 走会话还是无窗口任务）。
    pub fn ui_alive(&self) -> bool {
        matches!(
            self.state,
            State::Starting(_) | State::Running { .. } | State::Exiting(_)
        )
    }

    /// 当前阶段。
    pub fn kind(&self) -> StateKind {
        match self.state {
            State::Absent => StateKind::Absent,
            State::Starting(_) => StateKind::Starting,
            State::Running { .. } => StateKind::Running,
            State::Exiting(_) => StateKind::Exiting,
            State::Backoff { .. } => StateKind::Backoff,
        }
    }

    /// 当前（或最近一次）会话的代数。
    pub fn generation(&self) -> u64 {
        self.generation
    }

    /// 队列里等待的条数。
    pub fn queued(&self) -> usize {
        self.queue.len()
    }

    /// 等待确认的消息条数。
    pub fn inflight(&self) -> usize {
        self.inflight.len()
    }

    /// 宿主是否已在退出流程里。
    pub fn is_quitting(&self) -> bool {
        self.quit.is_some()
    }

    /// 提交一条要转给 UI 的消息。
    ///
    /// UI 不在就入队并拉起；启动中 / 退出中只入队；运行中立即写管道；退避中丢弃并提示。
    ///
    /// # 参数
    /// - `now`：当前时刻。
    /// - `message`：消息。
    ///
    /// # 返回
    /// 运行时要执行的动作。
    ///
    /// ```rust
    /// use snow_tray_host::machine::{Effect, HostMachine};
    /// use snow_tray_host::queue::Outbound;
    /// use std::time::Instant;
    /// let mut machine = HostMachine::new();
    /// let effects = machine.submit(
    ///     Instant::now(),
    ///     Outbound::Action { id: "capture".into(), origin: None },
    /// );
    /// assert!(matches!(effects[0], Effect::SpawnUi { .. }));
    /// ```
    pub fn submit(&mut self, now: Instant, message: Outbound) -> Vec<Effect> {
        let mut out = Vec::new();
        if self.quit.is_some() {
            tracing::info!("宿主正在退出，丢弃新的命令");
            return out;
        }
        self.leave_backoff_if_due(now);
        match &self.state {
            State::Backoff { .. } => {
                tracing::warn!("UI 进程在熔断退避中，丢弃命令");
                out.push(Effect::Notice(Notice::UiCrashLoop));
            }
            State::Absent => {
                self.enqueue(message, now);
                self.start_ui(now, &mut out);
            }
            State::Starting(_) | State::Exiting(_) => {
                self.enqueue(message, now);
            }
            State::Running { .. } => self.send_now(message, now, &mut out),
        }
        out
    }

    /// 拉起 UI 的结果：进程已创建。
    ///
    /// # 参数
    /// - `generation`：`SpawnUi` 里的代数；与当前不符（过期）则忽略。
    /// - `pid`：UI 进程 ID。
    /// - `token`：传给 UI 的令牌（用来校验 `hello`）。
    pub fn on_spawned(&mut self, generation: u64, pid: u32, token: String) -> Vec<Effect> {
        let mut out = Vec::new();
        if let State::Starting(starting) = &mut self.state
            && starting.generation == generation
            && starting.pid.is_none()
        {
            starting.pid = Some(pid);
            starting.token = Some(token);
            if GRANT_ON_SPAWN {
                out.push(Effect::GrantForeground { pid });
            }
        } else {
            tracing::warn!(generation, pid, "收到过期的拉起结果，已忽略");
        }
        out
    }

    /// 拉起 UI 的结果：失败（找不到 exe、`CreateProcess` 出错、管道建不起来）。
    ///
    /// # 参数
    /// - `now`：当前时刻。
    /// - `generation`：`SpawnUi` 里的代数。
    /// - `reason`：失败原因（记日志用）。
    pub fn on_spawn_failed(&mut self, now: Instant, generation: u64, reason: &str) -> Vec<Effect> {
        let mut out = Vec::new();
        let matches_current = matches!(
            &self.state,
            State::Starting(s) if s.generation == generation
        );
        if !matches_current {
            return out;
        }
        tracing::error!(generation, %reason, "拉起 UI 进程失败");
        self.queue.clear();
        out.push(Effect::CloseSession { generation });
        self.settle_after_failed_start(now, None, &mut out);
        self.finish_if_quitting(&mut out);
        out
    }

    /// 管道服务端报告客户端连上了。
    ///
    /// # 参数
    /// - `generation`：会话代数。
    /// - `client_pid`：客户端进程 ID（0 表示取不到）。
    pub fn on_pipe_connected(&mut self, generation: u64, client_pid: u32) {
        if generation == self.generation {
            self.connected_pid = (client_pid != 0).then_some(client_pid);
        }
    }

    /// UI 发来一条消息。
    ///
    /// # 参数
    /// - `now`：当前时刻。
    /// - `generation`：会话代数；过期的会话消息被忽略。
    /// - `message`：UI 消息。
    ///
    /// # 返回
    /// 运行时要执行的动作。
    pub fn on_ui_message(
        &mut self,
        now: Instant,
        generation: u64,
        message: UiMessage,
    ) -> Vec<Effect> {
        let mut out = Vec::new();
        if generation != self.generation {
            tracing::debug!(
                generation,
                current = self.generation,
                "过期会话的消息，已忽略"
            );
            return out;
        }
        match &self.state {
            State::Starting(_) => self.handle_starting_message(now, message, &mut out),
            State::Running { .. } => self.handle_running_message(now, message, &mut out),
            State::Exiting(_) => forward_if_informational(message, &mut out),
            State::Absent | State::Backoff { .. } => {}
        }
        out
    }

    /// UI 进程退出了（进程句柄触发）。
    ///
    /// # 参数
    /// - `now`：当前时刻。
    /// - `generation`：会话代数；过期的退出事件被忽略。
    ///
    /// # 返回
    /// 运行时要执行的动作。
    pub fn on_ui_exited(&mut self, now: Instant, generation: u64) -> Vec<Effect> {
        let mut out = Vec::new();
        if generation != self.generation {
            return out;
        }
        match &self.state {
            State::Starting(_) => {
                tracing::error!(generation, "UI 进程在握手前就退出了");
                self.queue.clear();
                out.push(Effect::CloseSession { generation });
                self.settle_after_failed_start(now, None, &mut out);
            }
            State::Running { .. } => {
                tracing::error!(generation, "UI 进程异常退出");
                self.inflight.clear();
                out.push(Effect::CloseSession { generation });
                let pins = std::mem::take(&mut self.pins);
                let opened = self.record_crash(now, &mut out);
                if !opened && self.quit.is_none() {
                    self.recover_after_crash(now, pins, &mut out);
                }
            }
            State::Exiting(_) => {
                self.state = State::Absent;
                self.inflight.clear();
                self.pins = 0;
                out.push(Effect::CloseSession { generation });
                out.push(Effect::UiGone);
                if self.quit.is_none() && !self.queue.is_empty() {
                    self.queue.expire(now);
                    if !self.queue.is_empty() {
                        self.start_ui(now, &mut out);
                    }
                }
            }
            State::Absent | State::Backoff { .. } => return out,
        }
        self.finish_if_quitting(&mut out);
        out
    }

    /// 请求宿主退出（托盘“退出”、`--cmd quit`），或重启（托盘“重启”）。
    ///
    /// UI 在运行就先让它优雅退出，等它走完再让宿主退出；UI 不在就直接让宿主退出。
    ///
    /// # 参数
    /// - `now`：当前时刻。
    /// - `restart`：是否在退出后拉起延迟启动的新宿主。
    ///
    /// # 返回
    /// 运行时要执行的动作。
    pub fn request_quit(&mut self, now: Instant, restart: bool) -> Vec<Effect> {
        let mut out = Vec::new();
        if self.quit.is_some() {
            return out;
        }
        self.quit = Some(restart);
        self.queue.clear();
        match &self.state {
            State::Running { generation, pid } => {
                let (generation, pid) = (*generation, *pid);
                self.begin_exit(now, pid);
                out.push(Effect::Send {
                    generation,
                    message: HostMessage::Shutdown,
                });
            }
            // 启动中：握手完成后再发 Shutdown；退出中：等进程结束
            State::Starting(_) | State::Exiting(_) => {}
            State::Absent | State::Backoff { .. } => {}
        }
        self.finish_if_quitting(&mut out);
        out
    }

    /// 处理到点的事：握手超时、确认超时、队列过期、退出超时、退避结束。
    ///
    /// # 参数
    /// - `now`：当前时刻。
    ///
    /// # 返回
    /// 运行时要执行的动作。
    pub fn on_tick(&mut self, now: Instant) -> Vec<Effect> {
        let mut out = Vec::new();
        if self.leave_backoff_if_due(now) && self.keep_alive && self.quit.is_none() {
            tracing::info!("熔断退避结束，UI 需要常驻，补拉一个");
            self.enqueue(Outbound::keep_alive(), now);
            self.start_ui(now, &mut out);
        }
        let expired = self.queue.expire(now);
        if expired > 0 {
            tracing::warn!(expired, "队列里的命令超过 30 秒未能投递，已丢弃");
        }
        match &mut self.state {
            State::Starting(starting) if now >= starting.deadline => {
                let generation = starting.generation;
                let pid = starting.pid;
                tracing::error!(generation, "UI 进程 10 秒内没有完成握手，强制结束");
                self.queue.clear();
                if let Some(pid) = pid {
                    out.push(Effect::KillUi { pid });
                }
                out.push(Effect::CloseSession { generation });
                self.settle_after_failed_start(now, pid, &mut out);
                self.finish_if_quitting(&mut out);
            }
            State::Running { generation, .. } => {
                let generation = *generation;
                self.resend_or_give_up(now, generation, &mut out);
            }
            State::Exiting(exiting) if !exiting.kill_sent && now >= exiting.deadline => {
                tracing::warn!(pid = exiting.pid, "UI 进程超时未退出，强制结束（最后手段）");
                exiting.kill_sent = true;
                out.push(Effect::KillUi { pid: exiting.pid });
            }
            _ => {}
        }
        out
    }

    /// 下一个需要调用 [`on_tick`](Self::on_tick) 的时刻（没有待办返回 `None`）。
    pub fn next_deadline(&self) -> Option<Instant> {
        let mut deadlines: Vec<Instant> = Vec::new();
        deadlines.extend(self.queue.next_expiry());
        match &self.state {
            State::Starting(starting) => deadlines.push(starting.deadline),
            State::Running { .. } => {
                deadlines.extend(self.inflight.iter().map(|item| item.sent_at + ACK_TIMEOUT));
            }
            State::Exiting(exiting) if !exiting.kill_sent => deadlines.push(exiting.deadline),
            State::Backoff { until } => deadlines.push(*until),
            State::Exiting(_) | State::Absent => {}
        }
        deadlines.into_iter().min()
    }

    /// 入队并记录异常情况。
    fn enqueue(&mut self, message: Outbound, now: Instant) {
        match self.queue.push(message, now) {
            PushOutcome::Queued => {}
            PushOutcome::Coalesced => tracing::debug!("手势 update 已合并"),
            PushOutcome::DroppedOldest => tracing::warn!("命令队列已满，丢弃了最旧的一条"),
        }
    }

    /// 退避期满就回到 `Absent`。
    ///
    /// # 返回
    /// 本次是否刚从退避里出来。
    fn leave_backoff_if_due(&mut self, now: Instant) -> bool {
        if let State::Backoff { until } = self.state
            && now >= until
        {
            tracing::info!("熔断退避结束，恢复拉起 UI");
            self.breaker.open_until(now);
            self.state = State::Absent;
            return true;
        }
        false
    }

    /// UI 崩溃（没跳闸）后的恢复：崩溃前有贴图就恢复贴图（方案 §10 第 9 条）；快照要求常驻就补拉一个。
    fn recover_after_crash(&mut self, now: Instant, pins: u32, out: &mut Vec<Effect>) {
        let mut respawn = false;
        if pins > 0 {
            tracing::info!(pins, "崩溃前有贴图，自动拉起 UI 恢复");
            self.enqueue(Outbound::RestorePins, now);
            respawn = true;
        }
        if self.keep_alive {
            tracing::info!("崩溃前 UI 要求常驻，自动补拉");
            self.enqueue(Outbound::keep_alive(), now);
            respawn = true;
        }
        if respawn {
            self.start_ui(now, out);
        }
    }

    /// 拉起一个新的 UI 会话。
    fn start_ui(&mut self, now: Instant, out: &mut Vec<Effect>) {
        self.spawn_hint = match self.queue.front() {
            Some(Outbound::Action { id, .. }) => UiHint::for_action(id),
            _ => UiHint::None,
        };
        self.generation += 1;
        self.seq = HostSeq::new();
        self.inflight.clear();
        self.connected_pid = None;
        self.state = State::Starting(Starting {
            generation: self.generation,
            pid: None,
            token: None,
            deadline: now + HELLO_TIMEOUT,
        });
        out.push(Effect::SpawnUi {
            generation: self.generation,
        });
    }

    /// 分配序号并立即写管道（只在 `Running` 状态调用）。
    fn send_now(&mut self, message: Outbound, now: Instant, out: &mut Vec<Effect>) {
        let State::Running { generation, pid } = self.state else {
            return;
        };
        if grant_for_forward(&message) {
            out.push(Effect::GrantForeground { pid });
        }
        let seq = self.seq.next();
        let wire = to_wire(seq, &message);
        // 手势 `update` 每秒上百条，丢一条无所谓（下一条、`finish` 带着最新位置）：不等确认、不重发
        if !is_gesture_update(&message) {
            self.inflight.push_back(Inflight {
                seq,
                message: wire.clone(),
                sent_at: now,
                retried: false,
            });
        }
        out.push(Effect::Send {
            generation,
            message: wire,
        });
    }

    /// 进入 `Exiting`（调用方负责写 `ExitOk` / `Shutdown`）。
    fn begin_exit(&mut self, now: Instant, pid: u32) {
        self.inflight.clear();
        self.state = State::Exiting(Exiting {
            pid,
            deadline: now + EXIT_WAIT_TIMEOUT,
            kill_sent: false,
        });
    }

    /// 记一次崩溃，决定回到 `Absent` 还是进入 `Backoff`。
    ///
    /// # 返回
    /// 是否跳闸（进入退避）。
    fn record_crash(&mut self, now: Instant, out: &mut Vec<Effect>) -> bool {
        match self.breaker.record_crash(now) {
            BreakerState::Open { until } => {
                tracing::error!("UI 进程反复异常退出，进入熔断退避");
                self.state = State::Backoff { until };
                out.push(Effect::Notice(Notice::UiCrashLoop));
                true
            }
            BreakerState::Closed => {
                self.state = State::Absent;
                false
            }
        }
    }

    /// 启动失败后的收尾：记崩溃、提示；进程还活着就进入 `Exiting` 等它真正退出（避免新旧 UI 并存）。
    fn settle_after_failed_start(
        &mut self,
        now: Instant,
        killed: Option<u32>,
        out: &mut Vec<Effect>,
    ) {
        match self.breaker.record_crash(now) {
            BreakerState::Open { until } => {
                self.state = State::Backoff { until };
                out.push(Effect::Notice(Notice::UiCrashLoop));
            }
            BreakerState::Closed => {
                out.push(Effect::Notice(Notice::UiStartFailed));
                self.state = match killed {
                    Some(pid) => State::Exiting(Exiting {
                        pid,
                        deadline: now + EXIT_WAIT_TIMEOUT,
                        kill_sent: true,
                    }),
                    None => State::Absent,
                };
            }
        }
    }

    /// 宿主在退出流程里、且 UI 已经不在了：让宿主退出。
    fn finish_if_quitting(&mut self, out: &mut Vec<Effect>) {
        if let Some(restart) = self.quit
            && matches!(self.state, State::Absent | State::Backoff { .. })
            && !out.iter().any(|e| matches!(e, Effect::ExitHost { .. }))
        {
            out.push(Effect::ExitHost { restart });
        }
    }

    /// `Starting` 阶段收到的消息：必须是合法的 `hello`。
    fn handle_starting_message(&mut self, now: Instant, message: UiMessage, out: &mut Vec<Effect>) {
        let State::Starting(starting) = &self.state else {
            return;
        };
        let generation = starting.generation;
        let (Some(pid), Some(token)) = (starting.pid, starting.token.clone()) else {
            tracing::warn!("拉起结果还没回来就收到 UI 消息，按握手失败处理");
            self.reject_session(now, generation, None, out);
            return;
        };
        let verdict = check_hello(&message, pid, &token).and(match self.connected_pid {
            Some(connected) if connected != pid => Err(HelloError::PidMismatch),
            _ => Ok(()),
        });
        if let Err(reason) = verdict {
            tracing::error!(%reason, "UI 握手未通过，断开并结束该进程");
            self.reject_session(now, generation, Some(pid), out);
            return;
        }
        self.state = State::Running { generation, pid };
        if let Some(_restart) = self.quit {
            // 宿主在 UI 启动期间要求退出：握手一完成就让它优雅退出
            self.begin_exit(now, pid);
            out.push(Effect::Send {
                generation,
                message: HostMessage::Shutdown,
            });
            return;
        }
        let (live, expired) = self.queue.drain_live(now);
        if expired > 0 {
            tracing::warn!(expired, "启动期间排队的命令里有过期的，已丢弃");
        }
        for message in live {
            self.send_now(message, now, out);
        }
    }

    /// 握手失败：断开会话、必要时结束进程、计一次崩溃。
    fn reject_session(
        &mut self,
        now: Instant,
        generation: u64,
        pid: Option<u32>,
        out: &mut Vec<Effect>,
    ) {
        self.queue.clear();
        if let Some(pid) = pid {
            out.push(Effect::KillUi { pid });
        }
        out.push(Effect::CloseSession { generation });
        self.settle_after_failed_start(now, pid, out);
        self.finish_if_quitting(out);
    }

    /// `Running` 阶段收到的消息。
    fn handle_running_message(&mut self, now: Instant, message: UiMessage, out: &mut Vec<Effect>) {
        let State::Running { generation, pid } = self.state else {
            return;
        };
        match message {
            UiMessage::Hello { .. } => tracing::warn!("会话中又收到 hello，已忽略"),
            UiMessage::Ack { seq } => {
                while self.inflight.front().is_some_and(|item| item.seq <= seq) {
                    self.inflight.pop_front();
                }
            }
            UiMessage::ExitRequest { last_seq } => {
                let decision = self.seq.arbitrate_exit(last_seq);
                tracing::info!(last_seq, sent = self.seq.last_sent(), ?decision, "退出仲裁");
                out.push(Effect::Send {
                    generation,
                    message: decision.reply(),
                });
                if decision == ExitDecision::Ok {
                    self.begin_exit(now, pid);
                }
            }
            UiMessage::Activity { pins, .. } => {
                self.pins = pins;
                out.push(Effect::FromUi(message));
            }
            other => out.push(Effect::FromUi(other)),
        }
    }

    /// 重发超时的消息一次，再超时就放弃。
    fn resend_or_give_up(&mut self, now: Instant, generation: u64, out: &mut Vec<Effect>) {
        let mut given_up = Vec::new();
        for item in &mut self.inflight {
            if now < item.sent_at + ACK_TIMEOUT {
                continue;
            }
            if item.retried {
                given_up.push(item.seq);
            } else {
                item.retried = true;
                item.sent_at = now;
                out.push(Effect::Send {
                    generation,
                    message: item.message.clone(),
                });
            }
        }
        for seq in given_up {
            tracing::error!(seq, "命令重发后仍未确认，已放弃");
            self.inflight.retain(|item| item.seq != seq);
            out.push(Effect::GaveUp { seq });
        }
    }
}

/// 退出阶段只转发无副作用的信息类消息（提示、快照变更、热键重注册），其余忽略。
fn forward_if_informational(message: UiMessage, out: &mut Vec<Effect>) {
    if matches!(
        message,
        UiMessage::Tooltip { .. }
            | UiMessage::Balloon { .. }
            | UiMessage::SpecChanged { .. }
            | UiMessage::HotkeysApply { .. }
    ) {
        out.push(Effect::FromUi(message));
    }
}

/// 把待发消息配上序号，转成线上消息。
fn to_wire(seq: u64, message: &Outbound) -> HostMessage {
    match message {
        Outbound::Action { id, origin } => HostMessage::Command {
            seq,
            id: id.clone(),
            origin: *origin,
        },
        Outbound::RestorePins => HostMessage::RestorePins { seq },
        Outbound::SetConfig { key, value } => HostMessage::SetConfig {
            seq,
            key: key.clone(),
            value: value.clone(),
        },
        Outbound::Gesture {
            phase,
            id,
            action,
            x,
            y,
        } => HostMessage::Gesture {
            seq,
            phase: *phase,
            id: *id,
            action: action.clone(),
            x: *x,
            y: *y,
        },
    }
}

/// 是否是手势的 `update` 事件。
fn is_gesture_update(message: &Outbound) -> bool {
    matches!(
        message,
        Outbound::Gesture {
            phase: GesturePhase::Update,
            ..
        }
    )
}

#[cfg(test)]
mod tests;
