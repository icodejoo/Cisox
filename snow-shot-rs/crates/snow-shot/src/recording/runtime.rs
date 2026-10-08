//! 屏幕录制会话状态机（Recording Runtime）。
//!
//! 会话本身不采集、不编码：真正的录制在独立的 `snow-recorder` 进程里完成，
//! 会话通过 [`RecorderLink`] 与它按行协议通信，并把回报的事件折算成 [`RecordingState`]。
//! 录制进程崩溃 / 被杀 / 失联时，会话转入 `Error` 并清理，不会卡在“录制中”。

use crate::recording::audio::{AudioBoard, AudioNotice};
use crate::recording::model::{RecordingConfig, RecordingFailure, RecordingState};
use snow_recorder_protocol::{Command, Event, StartRequest};
use std::path::PathBuf;
use std::time::{Duration, Instant};

/// 发出 `START` 后等待首个状态回报的最长时间。
const START_TIMEOUT: Duration = Duration::from_secs(15);
/// 发出 `STOP` 后等待文件写完的最长时间。
const SAVE_TIMEOUT: Duration = Duration::from_secs(180);
/// 毫秒到秒的换算。
const MS_PER_SEC: u64 = 1000;

/// 录制进程的通信通道抽象（真实实现见 `client::ProcessRecorderLink`，测试用假实现）。
pub trait RecorderLink {
    /// 发送一条命令；管道已断开时返回错误说明。
    fn send(&mut self, command: &Command) -> Result<(), String>;

    /// 取走目前为止收到的全部事件（非阻塞）。
    fn poll(&mut self) -> Vec<LinkEvent>;

    /// 终止通信：等待进程退出（超时则强杀）并清理中间产物。可重复调用。
    fn shutdown(&mut self);
}

/// 通道上出现的事件。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LinkEvent {
    /// 录制进程回报的协议事件。
    Event(Event),
    /// 录制进程已退出（`code` 为退出码，未知为 `None`）。
    Exited {
        /// 退出码。
        code: Option<i32>,
    },
}

/// 屏幕录制活动会话控制器。
pub struct ScreenRecordingSession {
    /// 录制配置。
    config: RecordingConfig,
    /// 当前状态。
    state: RecordingState,
    /// 与录制进程的通道（空闲或已结束时为 `None`）。
    link: Option<Box<dyn RecorderLink>>,
    /// 最近一次回报的有效时长（毫秒）。
    last_elapsed_ms: u64,
    /// 等待首个回报的起点（`START` 发出后）。
    awaiting_first_event: Option<Instant>,
    /// 等待文件写完的起点（`STOP` 发出后）。
    saving_since: Option<Instant>,
    /// 各音频源状态（用于降级提示）。
    audio: AudioBoard,
}

impl ScreenRecordingSession {
    /// 创建录制会话（空闲状态）。
    ///
    /// # 参数
    /// - `config`：录制配置。
    pub fn new(config: RecordingConfig) -> Self {
        let audio = AudioBoard::new(&config.audio);
        Self {
            config,
            audio,
            state: RecordingState::Idle,
            link: None,
            last_elapsed_ms: 0,
            awaiting_first_event: None,
            saving_since: None,
        }
    }

    /// 获取当前录制配置引用。
    pub const fn config(&self) -> &RecordingConfig {
        &self.config
    }

    /// 当前应显示的音频降级提示（无降级时为空）。
    pub fn audio_notices(&self) -> Vec<AudioNotice> {
        self.audio.notices()
    }

    /// 获取当前录制状态引用。
    pub const fn state(&self) -> &RecordingState {
        &self.state
    }

    /// 接入录制进程通道并开始倒计时（`seconds == 0` 时立即开始）。
    ///
    /// # 参数
    /// - `link`：已拉起的录制进程通道。
    /// - `seconds`：倒计时秒数。
    pub fn begin(&mut self, link: Box<dyn RecorderLink>, seconds: u32) {
        self.link = Some(link);
        if seconds == 0 {
            self.start_immediately();
        } else {
            self.state = RecordingState::Countdown {
                seconds_left: seconds,
            };
        }
    }

    /// 推进倒计时一步；倒计时结束时自动开始录制并返回 `true`。
    pub fn tick_countdown(&mut self) -> bool {
        let RecordingState::Countdown { seconds_left } = &mut self.state else {
            return false;
        };
        if *seconds_left <= 1 {
            self.start_immediately();
            true
        } else {
            *seconds_left -= 1;
            false
        }
    }

    /// 立即开始录制：向录制进程发送 `START`。
    pub fn start_immediately(&mut self) {
        let request = StartRequest {
            x: self.config.region.x,
            y: self.config.region.y,
            width: u32::try_from(self.config.region.width).unwrap_or(0),
            height: u32::try_from(self.config.region.height).unwrap_or(0),
            format: self.config.format.to_media(),
            fps: self.config.fps,
            show_cursor: self.config.show_cursor,
            output: self.config.output_path.clone(),
            audio: self.config.audio.clone(),
            effects: self.config.effects.clone(),
        };
        self.state = RecordingState::Recording {
            elapsed_secs: 0,
            is_paused: false,
            frames_captured: 0,
        };
        self.last_elapsed_ms = 0;
        self.awaiting_first_event = Some(Instant::now());
        self.send_or_fail(&Command::Start(request));
    }

    /// 暂停或恢复录制。
    pub fn toggle_pause(&mut self) {
        let RecordingState::Recording { is_paused, .. } = &self.state else {
            return;
        };
        let command = if *is_paused {
            Command::Resume
        } else {
            Command::Pause
        };
        self.send_or_fail(&command);
    }

    /// 请求停止并保存：进入 `Saving`，文件写完后由 [`ScreenRecordingSession::poll`] 转为 `Finished`。
    ///
    /// # 返回
    /// 当前状态不允许停止时返回错误说明。
    pub fn finish(&mut self) -> Result<(), String> {
        if !self.state.is_active() {
            return Err("not in an active recording; cannot stop".to_string());
        }
        self.state = RecordingState::Saving;
        self.saving_since = Some(Instant::now());
        self.send_or_fail(&Command::Stop);
        Ok(())
    }

    /// 取消并放弃当前录制：通知录制进程丢弃产物并退出，回到 `Idle`。
    pub fn cancel(&mut self) {
        if let Some(mut link) = self.link.take() {
            let _ = link.send(&Command::Cancel);
            link.shutdown();
        }
        self.awaiting_first_event = None;
        self.saving_since = None;
        self.state = RecordingState::Idle;
    }

    /// 因外部原因（找不到录制进程、进程无法启动等）直接转入错误状态。
    ///
    /// # 参数
    /// - `reason`：错误原因。
    pub fn abort(&mut self, reason: RecordingFailure) {
        self.fail(reason);
    }

    /// 处理通道上新到的事件并检查超时。
    ///
    /// # 参数
    /// - `now`：当前时刻（便于确定性测试）。
    ///
    /// # 返回
    /// 状态是否发生变化。
    pub fn poll(&mut self, now: Instant) -> bool {
        let before = self.state.clone();
        let before_audio = self.audio.clone();
        let events = match self.link.as_mut() {
            Some(link) => link.poll(),
            None => Vec::new(),
        };
        for event in events {
            self.apply(event);
        }
        self.check_timeouts(now);
        self.state != before || self.audio != before_audio
    }

    /// 是否仍需要继续轮询（有通道且未到终态）。
    pub fn needs_polling(&self) -> bool {
        self.link.is_some() && !self.state.is_terminal()
    }

    /// 折算一个通道事件。
    fn apply(&mut self, event: LinkEvent) {
        match event {
            LinkEvent::Event(Event::Ready) => {}
            LinkEvent::Event(Event::Recording { elapsed_ms, frames }) => {
                self.awaiting_first_event = None;
                self.last_elapsed_ms = elapsed_ms;
                if let RecordingState::Recording { is_paused, .. } = self.state {
                    self.state = RecordingState::Recording {
                        elapsed_secs: elapsed_ms / MS_PER_SEC,
                        is_paused,
                        frames_captured: frames,
                    };
                }
            }
            LinkEvent::Event(Event::Paused) => self.set_paused(true),
            LinkEvent::Event(Event::Resumed) => self.set_paused(false),
            LinkEvent::Event(Event::Finished { path, .. }) => self.complete(path),
            LinkEvent::Event(Event::Error { reason }) => {
                self.fail(RecordingFailure::Worker(reason))
            }
            // 视频编辑 / 探测事件由编辑流程消费，录制状态机不关心
            LinkEvent::Event(
                Event::EditProgress { .. } | Event::EditFinished { .. } | Event::ProbeResult(_),
            ) => {}
            // 音频源状态只影响降级提示，不改变录制状态机
            LinkEvent::Event(Event::AudioState { source, status }) => {
                self.audio.record(source, status)
            }
            LinkEvent::Exited { code } => {
                if !self.state.is_terminal() && !matches!(self.state, RecordingState::Idle) {
                    self.fail(RecordingFailure::ProcessExited { code });
                }
            }
        }
    }

    /// 更新暂停标志。
    fn set_paused(&mut self, paused: bool) {
        self.awaiting_first_event = None;
        if let RecordingState::Recording {
            elapsed_secs,
            frames_captured,
            ..
        } = self.state
        {
            self.state = RecordingState::Recording {
                elapsed_secs,
                is_paused: paused,
                frames_captured,
            };
        }
    }

    /// 录制完成：读取文件大小，收尾通道。
    fn complete(&mut self, path: PathBuf) {
        let size = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
        self.state = RecordingState::Finished {
            file_path: path,
            duration_secs: self.last_elapsed_ms / MS_PER_SEC,
            file_size_bytes: size,
        };
        self.release_link();
    }

    /// 转入错误终态并清理通道（进程若仍在运行会被终止，中间产物随之清除）。
    fn fail(&mut self, reason: RecordingFailure) {
        self.state = RecordingState::Error { reason };
        self.release_link();
    }

    /// 收尾并丢弃通道。
    fn release_link(&mut self) {
        self.awaiting_first_event = None;
        self.saving_since = None;
        if let Some(mut link) = self.link.take() {
            link.shutdown();
        }
    }

    /// 向进程发命令；失败（管道断开）视为录制失败。
    fn send_or_fail(&mut self, command: &Command) {
        let result = match self.link.as_mut() {
            Some(link) => link.send(command),
            None => Err("the recorder process is not connected".to_string()),
        };
        if let Err(e) = result {
            self.fail(RecordingFailure::LinkFailed(e));
        }
    }

    /// 检查“开始无响应”与“保存超时”。
    fn check_timeouts(&mut self, now: Instant) {
        if let Some(since) = self.awaiting_first_event
            && now.saturating_duration_since(since) > START_TIMEOUT
        {
            self.fail(RecordingFailure::NoResponse);
        }
        if let Some(since) = self.saving_since
            && now.saturating_duration_since(since) > SAVE_TIMEOUT
        {
            self.fail(RecordingFailure::SaveTimeout);
        }
    }
}

impl Drop for ScreenRecordingSession {
    /// 会话销毁时若通道仍在（窗口被强制关闭等），取消录制并清理进程。
    fn drop(&mut self) {
        if let Some(mut link) = self.link.take() {
            let _ = link.send(&Command::Cancel);
            link.shutdown();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::recording::model::RecordingFormat;
    use std::cell::RefCell;
    use std::rc::Rc;

    /// 假通道的共享观测数据。
    #[derive(Default)]
    struct Probe {
        /// 已发送的命令。
        sent: Vec<Command>,
        /// 待取走的事件。
        inbox: Vec<LinkEvent>,
        /// `shutdown` 调用次数。
        shutdowns: u32,
        /// 是否让 `send` 失败（模拟管道断开）。
        broken: bool,
    }

    /// 测试用通道。
    struct FakeLink(Rc<RefCell<Probe>>);

    impl RecorderLink for FakeLink {
        /// 记录命令。
        fn send(&mut self, command: &Command) -> Result<(), String> {
            let mut p = self.0.borrow_mut();
            if p.broken {
                return Err("broken pipe".into());
            }
            p.sent.push(command.clone());
            Ok(())
        }

        /// 取走事件。
        fn poll(&mut self) -> Vec<LinkEvent> {
            std::mem::take(&mut self.0.borrow_mut().inbox)
        }

        /// 记录关闭。
        fn shutdown(&mut self) {
            self.0.borrow_mut().shutdowns += 1;
        }
    }

    /// 构造会话与观测句柄。
    fn session(countdown: u32) -> (ScreenRecordingSession, Rc<RefCell<Probe>>) {
        let probe = Rc::new(RefCell::new(Probe::default()));
        let mut s = ScreenRecordingSession::new(RecordingConfig {
            format: RecordingFormat::Gif,
            fps: 24,
            ..RecordingConfig::default()
        });
        s.begin(Box::new(FakeLink(probe.clone())), countdown);
        (s, probe)
    }

    /// 向假通道塞事件。
    fn push(probe: &Rc<RefCell<Probe>>, event: LinkEvent) {
        probe.borrow_mut().inbox.push(event);
    }

    /// 倒计时结束后才发送 START，且参数与配置一致。
    #[test]
    fn countdown_then_start() {
        let (mut s, probe) = session(2);
        assert_eq!(*s.state(), RecordingState::Countdown { seconds_left: 2 });
        assert!(probe.borrow().sent.is_empty());
        assert!(!s.tick_countdown());
        assert!(s.tick_countdown());
        assert!(s.state().is_active());
        match &probe.borrow().sent[..] {
            [Command::Start(r)] => {
                assert_eq!((r.width, r.height, r.fps), (1920, 1080, 24));
                assert_eq!(r.format.as_str(), "gif");
                assert!(r.show_cursor);
            }
            other => panic!("期望单条 START，实际 {other:?}"),
        }
    }

    /// START 带上配置里的音频请求；音频状态事件折算成降级提示且不影响录制状态。
    #[test]
    fn audio_request_sent_and_states_become_notices() {
        use snow_recorder_protocol::{AudioRequest, AudioSource, AudioStatus};
        let probe = Rc::new(RefCell::new(Probe::default()));
        let audio = AudioRequest {
            microphone: true,
            system: true,
            ..AudioRequest::default()
        };
        let mut s = ScreenRecordingSession::new(RecordingConfig {
            audio: audio.clone(),
            ..RecordingConfig::default()
        });
        s.begin(Box::new(FakeLink(probe.clone())), 0);
        match &probe.borrow().sent[..] {
            [Command::Start(r)] => assert_eq!(r.audio, audio),
            other => panic!("期望单条 START，实际 {other:?}"),
        }
        assert!(s.audio_notices().is_empty());
        push(
            &probe,
            LinkEvent::Event(Event::AudioState {
                source: AudioSource::Microphone,
                status: AudioStatus::Unavailable,
            }),
        );
        assert!(s.poll(Instant::now()), "提示变化应触发重绘");
        assert_eq!(s.audio_notices(), vec![AudioNotice::MicUnavailable]);
        push(
            &probe,
            LinkEvent::Event(Event::AudioState {
                source: AudioSource::System,
                status: AudioStatus::Unavailable,
            }),
        );
        s.poll(Instant::now());
        assert_eq!(s.audio_notices(), vec![AudioNotice::NoSound]);
        assert!(s.state().is_active());
    }

    /// 无倒计时立即发送 START。
    #[test]
    fn zero_countdown_starts_immediately() {
        let (s, probe) = session(0);
        assert!(s.state().is_active());
        assert_eq!(probe.borrow().sent.len(), 1);
    }

    /// 状态回报折算：时长、帧数、暂停与恢复。
    #[test]
    fn events_drive_state() {
        let (mut s, probe) = session(0);
        push(
            &probe,
            LinkEvent::Event(Event::Recording {
                elapsed_ms: 2500,
                frames: 60,
            }),
        );
        assert!(s.poll(Instant::now()));
        assert_eq!(
            *s.state(),
            RecordingState::Recording {
                elapsed_secs: 2,
                is_paused: false,
                frames_captured: 60
            }
        );
        s.toggle_pause();
        push(&probe, LinkEvent::Event(Event::Paused));
        s.poll(Instant::now());
        assert!(matches!(
            s.state(),
            RecordingState::Recording {
                is_paused: true,
                ..
            }
        ));
        s.toggle_pause();
        assert_eq!(probe.borrow().sent[1..], [Command::Pause, Command::Resume]);
        push(&probe, LinkEvent::Event(Event::Resumed));
        s.poll(Instant::now());
        assert!(matches!(
            s.state(),
            RecordingState::Recording {
                is_paused: false,
                ..
            }
        ));
    }

    /// 停止 → Saving → Finished，并读取文件大小、关闭通道。
    #[test]
    fn stop_then_finished() {
        let file = std::env::temp_dir().join(format!("snow-rec-rt-{}.bin", std::process::id()));
        std::fs::write(&file, b"12345").unwrap();
        let (mut s, probe) = session(0);
        push(
            &probe,
            LinkEvent::Event(Event::Recording {
                elapsed_ms: 4200,
                frames: 100,
            }),
        );
        s.poll(Instant::now());
        s.finish().unwrap();
        assert_eq!(*s.state(), RecordingState::Saving);
        assert_eq!(probe.borrow().sent.last(), Some(&Command::Stop));
        push(
            &probe,
            LinkEvent::Event(Event::Finished {
                path: file.clone(),
                frames: 100,
                dropped: 0,
            }),
        );
        s.poll(Instant::now());
        assert_eq!(
            *s.state(),
            RecordingState::Finished {
                file_path: file.clone(),
                duration_secs: 4,
                file_size_bytes: 5
            }
        );
        assert_eq!(probe.borrow().shutdowns, 1);
        assert!(!s.needs_polling());
        let _ = std::fs::remove_file(file);
    }

    /// 录制进程中途退出：转入 Error 并清理，不卡在录制中。
    #[test]
    fn crash_during_recording_resets() {
        let (mut s, probe) = session(0);
        push(&probe, LinkEvent::Exited { code: None });
        assert!(s.poll(Instant::now()));
        assert!(matches!(s.state(), RecordingState::Error { .. }));
        assert_eq!(probe.borrow().shutdowns, 1);
        assert!(!s.needs_polling());
    }

    /// 保存途中进程退出（未收到 Finished）同样视为失败。
    #[test]
    fn crash_while_saving_is_error() {
        let (mut s, probe) = session(0);
        s.finish().unwrap();
        push(&probe, LinkEvent::Exited { code: Some(1) });
        s.poll(Instant::now());
        assert!(matches!(s.state(), RecordingState::Error { .. }));
    }

    /// 正常完成之后进程退出不会覆盖 Finished。
    #[test]
    fn exit_after_finished_is_ignored() {
        let (mut s, probe) = session(0);
        s.finish().unwrap();
        push(
            &probe,
            LinkEvent::Event(Event::Finished {
                path: PathBuf::from("nope.mp4"),
                frames: 1,
                dropped: 0,
            }),
        );
        push(&probe, LinkEvent::Exited { code: Some(0) });
        s.poll(Instant::now());
        assert!(matches!(s.state(), RecordingState::Finished { .. }));
    }

    /// 录制进程回报 Error：转入错误状态。
    #[test]
    fn reported_error_is_terminal() {
        let (mut s, probe) = session(0);
        push(
            &probe,
            LinkEvent::Event(Event::Error {
                reason: "采集失败".into(),
            }),
        );
        s.poll(Instant::now());
        assert_eq!(
            *s.state(),
            RecordingState::Error {
                reason: RecordingFailure::Worker("采集失败".into())
            }
        );
    }

    /// 取消：发送 CANCEL、关闭通道、回到 Idle。
    #[test]
    fn cancel_sends_cancel_and_resets() {
        let (mut s, probe) = session(0);
        s.cancel();
        assert_eq!(*s.state(), RecordingState::Idle);
        assert_eq!(probe.borrow().sent.last(), Some(&Command::Cancel));
        assert_eq!(probe.borrow().shutdowns, 1);
    }

    /// 管道断开时发送失败即转错误。
    #[test]
    fn broken_pipe_fails_session() {
        let (mut s, probe) = session(0);
        probe.borrow_mut().broken = true;
        s.toggle_pause();
        assert!(matches!(s.state(), RecordingState::Error { .. }));
    }

    /// START 之后长时间无回应视为失败；保存超时同理。
    #[test]
    fn timeouts_fail_session() {
        let (mut s, _probe) = session(0);
        s.poll(Instant::now() + START_TIMEOUT + Duration::from_secs(1));
        assert!(matches!(s.state(), RecordingState::Error { .. }));

        let (mut s, probe) = session(0);
        push(
            &probe,
            LinkEvent::Event(Event::Recording {
                elapsed_ms: 100,
                frames: 3,
            }),
        );
        s.poll(Instant::now());
        s.finish().unwrap();
        s.poll(Instant::now() + SAVE_TIMEOUT + Duration::from_secs(1));
        assert!(matches!(s.state(), RecordingState::Error { .. }));
    }

    /// 非录制状态不能停止。
    #[test]
    fn finish_requires_recording() {
        let (mut s, _probe) = session(3);
        assert!(s.finish().is_err());
    }

    /// 会话被销毁时会取消并关闭仍在的通道。
    #[test]
    fn drop_cancels_running_session() {
        let (s, probe) = session(0);
        drop(s);
        assert_eq!(probe.borrow().sent.last(), Some(&Command::Cancel));
        assert_eq!(probe.borrow().shutdowns, 1);
    }
}
