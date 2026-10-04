//! 录屏音频：系统声（WASAPI loopback）与麦克风的采集、10ms 槽混音，以及向封装后端输出连续 PCM。
//!
//! - 混音器 [`AudioMixer`] 是纯逻辑：按 10ms 槽对齐、i32 饱和相加、每路定点增益，缺包的槽补零保证时间线连续。
//! - 运行器 [`AudioRecorder`] 在独立线程里读采集事件，用 [`Timeline`] 把采集时刻换算成有效时间（暂停不计入），
//!   超出 100ms 抖动窗口的槽按序交给 [`AudioSink`]；暂停期间的包直接丢弃，恢复后重新对齐。
//! - 采集失败一律降级为"没有该路"，不会让录制失败；状态变化通过 [`StateReporter`] 汇报（对应 `AUDIO_STATE` 事件）。

use std::collections::{BTreeMap, VecDeque};
use std::sync::Arc;
use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use snow_audio_recorder::{
    AudioDeviceInfo, AudioEvent, AudioFormat, AudioPacket, AudioSession, AudioSourceKind,
    AudioStreamConfig, AudioStreamHandle, DeviceSelector, RecvTimeoutError,
};
use snow_recorder_protocol::{AUDIO_VOLUME_DEFAULT, AudioRequest, AudioSource, AudioStatus, Event};

use crate::timeline::Timeline;

/// 采样率（Hz）。
pub const SAMPLE_RATE: u32 = 48_000;
/// 输出声道数（立体声）。
pub const CHANNELS: usize = 2;
/// 槽长（毫秒）。
const SLOT_MS: u64 = 10;
/// 每槽帧数。
pub const SLOT_FRAMES: u64 = SAMPLE_RATE as u64 * SLOT_MS / 1000;
/// 抖动窗口（毫秒）：晚到不超过该时长的包仍能并入对应槽。
const JITTER_MS: u64 = 100;
/// 抖动窗口帧数。
const JITTER_FRAMES: u64 = SAMPLE_RATE as u64 * JITTER_MS / 1000;
/// 连续性容差（帧，20ms）：包的时间戳与"上一包末尾"相差不超过它时按连续放置，忽略时间戳抖动；
/// 超出则按时间戳重新对齐，所以连续放置造成的偏移不会累积超过该值。
const ALIGN_TOLERANCE_FRAMES: u64 = SAMPLE_RATE as u64 * 20 / 1000;
/// 允许领先已输出位置的最大槽数；更远的包视为时间戳异常，丢弃以保证内存有界。
const MAX_AHEAD_SLOTS: u64 = 1000;
/// 原点确定前最多缓存的事件数（约 20 秒）。
const MAX_PENDING_EVENTS: usize = 2000;
/// 运行器轮询间隔。
const POLL_INTERVAL: Duration = Duration::from_millis(SLOT_MS);
/// 采集事件队列深度（约 2.5 秒）。
const EVENT_BUFFER_DEPTH: usize = 256;
/// 采集端麦克风声道数（单声道，由混音前上混为立体声）。
const MIC_CAPTURE_CHANNELS: u16 = 1;
/// 增益定点基准（100 对应原始电平）。
const GAIN_UNIT: i32 = AUDIO_VOLUME_DEFAULT as i32;
/// 每个槽的采样数（帧数 × 声道数）。
const SLOT_SAMPLES: usize = SLOT_FRAMES as usize * CHANNELS;

/// 音频状态回报回调。
pub type StateReporter = Arc<dyn Fn(AudioSource, AudioStatus) + Send + Sync>;

/// 把状态写成协议 `AUDIO_STATE` 事件的回报器（发往主程序）。
pub fn stdout_reporter() -> StateReporter {
    Arc::new(|source, status| crate::emit(&Event::AudioState { source, status }))
}

/// 封装后端的音频输入：接收连续的 48k 立体声 i16 PCM。
pub trait AudioSink: Send {
    /// 写入从 `first_slot` 开始的若干个完整 10ms 槽（交错立体声）；槽号严格连续递增，从 0 开始。
    ///
    /// # 参数
    /// - `first_slot`：首个槽号，对应有效时间 `first_slot * 10ms`。
    /// - `pcm`：交错立体声采样，长度为 960 的整数倍。
    fn write(&mut self, first_slot: u64, pcm: &[i16]) -> Result<(), String>;

    /// 冲刷编码器并结束该音轨（须在容器收尾前调用）。
    fn finish(self: Box<Self>) -> Result<(), String>;
}

/// 音源在数组里的下标。
fn lane(source: AudioSource) -> usize {
    match source {
        AudioSource::System => 0,
        AudioSource::Microphone => 1,
    }
}

/// 时长换算成 48k 帧数（向下取整）。
pub fn frames_from_duration(duration: Duration) -> u64 {
    u64::try_from(duration.as_nanos() * u128::from(SAMPLE_RATE) / 1_000_000_000).unwrap_or(u64::MAX)
}

/// 单声道上混为交错立体声（左右相同），追加到 `out`。
///
/// # 参数
/// - `mono`：单声道采样。
/// - `out`：输出缓冲（追加）。
pub fn upmix_mono_to_stereo(mono: &[i16], out: &mut Vec<i16>) {
    out.reserve(mono.len() * CHANNELS);
    for &s in mono {
        out.push(s);
        out.push(s);
    }
}

/// 10ms 槽混音器：两路输入（系统声、麦克风）按有效时间帧位置累加，按序吐出连续的槽。
#[derive(Debug)]
pub struct AudioMixer {
    /// 各路是否启用。
    enabled: [bool; 2],
    /// 各路增益（百分比）。
    gain: [u16; 2],
    /// 各路"上一包末尾"的帧位置；`None` 表示需要按时间戳重新对齐。
    next_frame: [Option<u64>; 2],
    /// 尚未输出的槽：槽号 -> 累加值（i32，避免相加溢出）。
    slots: BTreeMap<u64, Vec<i32>>,
    /// 已输出的槽数（也是下一个要输出的槽号）。
    emitted: u64,
    /// 因迟到或时间戳异常被丢弃的帧数。
    dropped_frames: u64,
}

impl AudioMixer {
    /// 创建混音器。
    ///
    /// # 参数
    /// - `system_gain`：系统声增益（百分比），`None` 表示不启用。
    /// - `mic_gain`：麦克风增益（百分比），`None` 表示不启用。
    pub fn new(system_gain: Option<u16>, mic_gain: Option<u16>) -> Self {
        Self {
            enabled: [system_gain.is_some(), mic_gain.is_some()],
            gain: [
                system_gain.unwrap_or(AUDIO_VOLUME_DEFAULT),
                mic_gain.unwrap_or(AUDIO_VOLUME_DEFAULT),
            ],
            next_frame: [None; 2],
            slots: BTreeMap::new(),
            emitted: 0,
            dropped_frames: 0,
        }
    }

    /// 放入一包交错立体声采样。
    ///
    /// 与上一包基本连续时按连续位置放置（忽略时间戳抖动），否则按 `start_frame` 重新对齐；
    /// 已输出位置之前的部分作为迟到数据丢弃。
    ///
    /// # 参数
    /// - `source`：音源。
    /// - `start_frame`：该包起点的有效时间帧位置。
    /// - `samples`：交错立体声采样。
    pub fn push(&mut self, source: AudioSource, start_frame: u64, samples: &[i16]) {
        let idx = lane(source);
        if !self.enabled[idx] || samples.is_empty() || !samples.len().is_multiple_of(CHANNELS) {
            return;
        }
        let frames = (samples.len() / CHANNELS) as u64;
        let mut pos = match self.next_frame[idx] {
            Some(next) if next.abs_diff(start_frame) <= ALIGN_TOLERANCE_FRAMES => next,
            _ => start_frame,
        };
        self.next_frame[idx] = Some(pos + frames);
        let floor = self.emitted * SLOT_FRAMES;
        let mut data = samples;
        if pos < floor {
            let skip = (floor - pos).min(frames);
            self.dropped_frames += skip;
            data = &samples[skip as usize * CHANNELS..];
            pos += skip;
        }
        if data.is_empty() {
            return;
        }
        if pos / SLOT_FRAMES > self.emitted + MAX_AHEAD_SLOTS {
            self.dropped_frames += (data.len() / CHANNELS) as u64;
            return;
        }
        let gain = i32::from(self.gain[idx]);
        while !data.is_empty() {
            let slot = pos / SLOT_FRAMES;
            let offset = (pos % SLOT_FRAMES) as usize * CHANNELS;
            let take = (SLOT_SAMPLES - offset).min(data.len());
            let buf = self
                .slots
                .entry(slot)
                .or_insert_with(|| vec![0; SLOT_SAMPLES]);
            for (dst, &src) in buf[offset..offset + take].iter_mut().zip(&data[..take]) {
                *dst += i32::from(src) * gain / GAIN_UNIT;
            }
            data = &data[take..];
            pos += (take / CHANNELS) as u64;
        }
    }

    /// 让指定音源（`None` 表示全部）在下一包时按时间戳重新对齐（暂停恢复、设备重启后调用）。
    pub fn reset_alignment(&mut self, source: Option<AudioSource>) {
        match source {
            Some(s) => self.next_frame[lane(s)] = None,
            None => self.next_frame = [None; 2],
        }
    }

    /// 输出所有"完整落在 `end_frame` 之前"的槽（缺包的槽补零），饱和裁剪为 i16，追加到 `out`。
    ///
    /// # 参数
    /// - `end_frame`：截止帧位置（排他，按槽向下取整）。
    /// - `out`：输出缓冲（追加）。
    ///
    /// # 返回
    /// 本次输出的槽数。
    pub fn drain_until(&mut self, end_frame: u64, out: &mut Vec<i16>) -> u64 {
        let end_slot = end_frame / SLOT_FRAMES;
        let mut count = 0;
        while self.emitted < end_slot {
            match self.slots.remove(&self.emitted) {
                Some(buf) => out.extend(
                    buf.iter()
                        .map(|&v| v.clamp(i32::from(i16::MIN), i32::from(i16::MAX)) as i16),
                ),
                None => out.extend(std::iter::repeat_n(0i16, SLOT_SAMPLES)),
            }
            self.emitted += 1;
            count += 1;
        }
        count
    }

    /// 已输出的槽数。
    pub fn emitted_slots(&self) -> u64 {
        self.emitted
    }

    /// 被丢弃的帧数（迟到或时间戳异常）。
    pub fn dropped_frames(&self) -> u64 {
        self.dropped_frames
    }
}

/// 计算采集包在有效时间线上的位置。
///
/// # 参数
/// - `timeline`：有效时间线（起点即视频原点）。
/// - `start`：包起点的采集时刻。
/// - `end`：包终点的采集时刻。
///
/// # 返回
/// `(起点帧位置, 需要裁掉的头部帧数)`；整包落在暂停区间、或整包早于原点时返回 `None`。
pub fn place_packet(
    timeline: &Timeline,
    origin: Instant,
    start: Instant,
    end: Instant,
) -> Option<(u64, u64)> {
    if timeline.in_pause(start) || timeline.in_pause(end) || end <= origin {
        return None;
    }
    let skip = frames_from_duration(origin.saturating_duration_since(start));
    let anchor = start.max(origin);
    Some((frames_from_duration(timeline.active_at(anchor)), skip))
}

/// 判断设备列表里是否有可用于该选择器的活动设备。
///
/// # 参数
/// - `devices`：对应方向（渲染或采集）的设备列表。
/// - `selector`：设备选择器。
pub fn source_usable(devices: &[AudioDeviceInfo], selector: &DeviceSelector) -> bool {
    devices.iter().any(|d| {
        d.is_active
            && match selector {
                DeviceSelector::Id(id) => &d.id == id,
                _ => d.is_default,
            }
    })
}

/// 构造采集配置：两路都不是必需（缺失时降级为无该路），麦克风单声道，包长 10ms。
///
/// # 参数
/// - `system`：系统声设备选择器，`None` 不启用。
/// - `microphone`：麦克风设备选择器，`None` 不启用。
pub fn build_stream_config(
    system: Option<DeviceSelector>,
    microphone: Option<DeviceSelector>,
) -> AudioStreamConfig {
    let mut config = AudioStreamConfig::default();
    config.system.enabled = system.is_some();
    config.system.required = false;
    if let Some(device) = system {
        config.system.device = device;
    }
    config.system.output_format = AudioFormat::new(SAMPLE_RATE, CHANNELS as u16);
    config.system.packet_duration = Duration::from_millis(SLOT_MS);
    config.microphone.enabled = microphone.is_some();
    config.microphone.required = false;
    if let Some(device) = microphone {
        config.microphone.device = device;
    }
    config.microphone.output_format = AudioFormat::new(SAMPLE_RATE, MIC_CAPTURE_CHANNELS);
    config.microphone.packet_duration = Duration::from_millis(SLOT_MS);
    config.event_buffer_depth = EVENT_BUFFER_DEPTH;
    config
}

/// 已打开的采集：尚未绑定封装后端，绑定后由 [`AudioCapture::run`] 启动混音线程。
pub struct AudioCapture {
    /// 采集流。
    stream: AudioStreamHandle,
    /// 各路增益（`None` 表示该路不可用/未启用）。
    gains: [Option<u16>; 2],
    /// 状态回报。
    report: StateReporter,
}

impl AudioCapture {
    /// 按请求打开采集。无设备、设备被拒绝等都降级处理并汇报 `unavailable`，不会失败。
    ///
    /// # 参数
    /// - `request`：录音请求。
    /// - `report`：状态回报回调。
    ///
    /// # 返回
    /// 至少有一路可用时返回采集；没有请求或全部不可用返回 `None`（此时不应创建音轨）。
    pub fn open(request: &AudioRequest, report: StateReporter) -> Option<Self> {
        if !request.enabled() {
            return None;
        }
        let wanted = [
            (AudioSource::System, request.system),
            (AudioSource::Microphone, request.microphone),
        ];
        let unavailable_all = |report: &StateReporter| {
            for (source, on) in wanted {
                if on {
                    report(source, AudioStatus::Unavailable);
                }
            }
        };
        let session = match AudioSession::new() {
            Ok(s) => s,
            Err(e) => {
                eprintln!("音频不可用: {e}");
                unavailable_all(&report);
                return None;
            }
        };
        let system = request.system.then(|| {
            let selector = request
                .system_device
                .clone()
                .map_or(DeviceSelector::DefaultRender, DeviceSelector::Id);
            session
                .enumerate_render_devices()
                .is_ok_and(|d| source_usable(&d, &selector))
                .then_some(selector)
        });
        let microphone = request.microphone.then(|| {
            let selector = request
                .mic_device
                .clone()
                .map_or(DeviceSelector::DefaultCapture, DeviceSelector::Id);
            session
                .enumerate_capture_devices()
                .is_ok_and(|d| source_usable(&d, &selector))
                .then_some(selector)
        });
        let (system, microphone) = (system.flatten(), microphone.flatten());
        if request.system && system.is_none() {
            report(AudioSource::System, AudioStatus::Unavailable);
        }
        if request.microphone && microphone.is_none() {
            report(AudioSource::Microphone, AudioStatus::Unavailable);
        }
        if system.is_none() && microphone.is_none() {
            return None;
        }
        let gains = [
            system.is_some().then_some(request.system_volume),
            microphone.is_some().then_some(request.mic_volume),
        ];
        let stream = match session.start_streaming(build_stream_config(system, microphone)) {
            Ok(s) => s,
            Err(e) => {
                eprintln!("启动音频采集失败: {e}");
                unavailable_all(&report);
                return None;
            }
        };
        for (source, gain) in [
            (AudioSource::System, gains[0]),
            (AudioSource::Microphone, gains[1]),
        ] {
            if gain.is_some() {
                report(source, AudioStatus::Ok);
            }
        }
        Some(Self {
            stream,
            gains,
            report,
        })
    }

    /// 绑定封装后端并启动混音线程；时间原点由 [`AudioRecorder::set_origin`] 之后确定。
    ///
    /// # 参数
    /// - `sink`：音频输入。
    pub fn run(self, sink: Box<dyn AudioSink>) -> AudioRecorder {
        let (control, rx) = mpsc::channel();
        let runner = Runner {
            source: Box::new(self.stream),
            mixer: AudioMixer::new(self.gains[0], self.gains[1]),
            sink: Some(sink),
            report: self.report,
            control: rx,
            timeline: None,
            origin: None,
            pending: VecDeque::new(),
            scratch: Vec::new(),
            last_status: [None; 2],
            sink_error_logged: false,
        };
        let thread = std::thread::Builder::new()
            .name("snow-audio-mix".into())
            .spawn(move || runner.run())
            .ok();
        AudioRecorder { control, thread }
    }
}

/// 发给混音线程的控制命令。
enum Control {
    /// 设定时间原点（视频第一帧时刻）。
    Origin(Instant),
    /// 暂停。
    Pause(Instant),
    /// 恢复。
    Resume(Instant),
    /// 停止：冲刷到该时刻并收尾。
    Stop(Instant),
    /// 放弃：直接退出，不收尾。
    Abort,
}

/// 混音线程的收尾报告。
#[derive(Debug, Clone, Default)]
pub struct AudioReport {
    /// 写入后端的槽数（10ms 一槽）。
    pub slots: u64,
    /// 被丢弃的帧数。
    pub dropped_frames: u64,
    /// 收尾时后端报告的错误。
    pub error: Option<String>,
}

/// 运行中的音频录制句柄（暂停、恢复、停止随视频流水线转发）。
pub struct AudioRecorder {
    /// 控制通道。
    control: Sender<Control>,
    /// 混音线程。
    thread: Option<JoinHandle<AudioReport>>,
}

impl AudioRecorder {
    /// 设定时间原点（视频时间线起点）；此前采集到的事件会在原点确定后补处理。
    pub fn set_origin(&self, at: Instant) {
        let _ = self.control.send(Control::Origin(at));
    }

    /// 暂停（暂停段内的音频被丢弃）。
    pub fn pause(&self, at: Instant) {
        let _ = self.control.send(Control::Pause(at));
    }

    /// 恢复并重新对齐。
    pub fn resume(&self, at: Instant) {
        let _ = self.control.send(Control::Resume(at));
    }

    /// 停止：冲刷到 `at` 对应的有效时间并结束音轨，阻塞到线程退出。
    ///
    /// # 返回
    /// 收尾报告；线程已异常退出返回 `None`。
    pub fn stop(mut self, at: Instant) -> Option<AudioReport> {
        let _ = self.control.send(Control::Stop(at));
        self.thread.take().and_then(|t| t.join().ok())
    }
}

impl Drop for AudioRecorder {
    /// 未显式 `stop` 就被丢弃时放弃收尾，保证线程退出。
    fn drop(&mut self) {
        if let Some(thread) = self.thread.take() {
            let _ = self.control.send(Control::Abort);
            let _ = thread.join();
        }
    }
}

/// 事件来源（真实采集流或测试替身）。
trait EventSource: Send {
    /// 取下一个事件，最多等 `timeout`。
    fn next(&self, timeout: Duration) -> Result<AudioEvent, RecvTimeoutError>;
}

impl EventSource for AudioStreamHandle {
    /// 转发到采集流。
    fn next(&self, timeout: Duration) -> Result<AudioEvent, RecvTimeoutError> {
        self.recv_timeout(timeout)
    }
}

/// 混音线程的状态。
struct Runner {
    /// 事件来源。
    source: Box<dyn EventSource>,
    /// 混音器。
    mixer: AudioMixer,
    /// 后端音频输入；写入出错后置空，之后只丢弃数据。
    sink: Option<Box<dyn AudioSink>>,
    /// 状态回报。
    report: StateReporter,
    /// 控制通道。
    control: Receiver<Control>,
    /// 有效时间线（原点确定后才有）。
    timeline: Option<Timeline>,
    /// 时间原点。
    origin: Option<Instant>,
    /// 原点确定前缓存的包。
    pending: VecDeque<AudioPacket>,
    /// 输出暂存。
    scratch: Vec<i16>,
    /// 各路最近一次汇报的状态（去重）。
    last_status: [Option<AudioStatus>; 2],
    /// 是否已记录过后端写入错误。
    sink_error_logged: bool,
}

impl Runner {
    /// 线程主循环。
    fn run(mut self) -> AudioReport {
        loop {
            loop {
                match self.control.try_recv() {
                    Ok(Control::Stop(at)) => return self.finish(at),
                    Ok(Control::Abort) | Err(TryRecvError::Disconnected) => {
                        return self.report_now(None);
                    }
                    Ok(other) => self.apply(other),
                    Err(TryRecvError::Empty) => break,
                }
            }
            match self.source.next(POLL_INTERVAL) {
                Ok(event) => self.on_event(event),
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Closed) => {
                    self.set_status(None, AudioStatus::Lost);
                    std::thread::sleep(POLL_INTERVAL);
                }
            }
            self.pump();
        }
    }

    /// 应用原点、暂停、恢复命令。
    fn apply(&mut self, control: Control) {
        match control {
            Control::Origin(at) => {
                self.origin = Some(at);
                self.timeline = Some(Timeline::new(at, 1));
                while let Some(packet) = self.pending.pop_front() {
                    self.on_packet(packet);
                }
            }
            Control::Pause(at) => {
                if let Some(t) = self.timeline.as_mut() {
                    t.pause(at);
                }
                self.mixer.reset_alignment(None);
            }
            Control::Resume(at) => {
                if let Some(t) = self.timeline.as_mut() {
                    t.resume(at);
                }
                self.mixer.reset_alignment(None);
            }
            Control::Stop(_) | Control::Abort => {}
        }
    }

    /// 处理一个采集事件。
    fn on_event(&mut self, event: AudioEvent) {
        match event {
            AudioEvent::Packet(packet) => {
                if self.timeline.is_some() {
                    self.on_packet(packet);
                } else {
                    if self.pending.len() >= MAX_PENDING_EVENTS {
                        self.pending.pop_front();
                    }
                    self.pending.push_back(packet);
                }
            }
            AudioEvent::SourceRestarted { source, .. } => {
                let source = to_protocol(source);
                self.mixer.reset_alignment(Some(source));
                self.set_status(Some(source), AudioStatus::Ok);
            }
            AudioEvent::Error(e) => {
                eprintln!("音频采集出错: {e}");
                self.set_status(None, AudioStatus::Lost);
            }
            AudioEvent::StreamEnded => self.set_status(None, AudioStatus::Lost),
            AudioEvent::PacketDropped { .. }
            | AudioEvent::Paused { .. }
            | AudioEvent::Resumed { .. } => {}
        }
    }

    /// 把一个包放进混音器。
    fn on_packet(&mut self, packet: AudioPacket) {
        let (Some(timeline), Some(origin)) = (self.timeline.as_ref(), self.origin) else {
            return;
        };
        if packet.format.sample_rate != SAMPLE_RATE || packet.frames == 0 {
            return;
        }
        let end = packet.end_capture_time().unwrap_or_else(Instant::now);
        let start = end.checked_sub(packet.duration()).unwrap_or(end);
        let Some((start_frame, skip)) = place_packet(timeline, origin, start, end) else {
            return;
        };
        let source = to_protocol(packet.source);
        let skip = usize::try_from(skip).unwrap_or(usize::MAX);
        let channels = usize::from(packet.format.channels);
        let samples = packet
            .data
            .get(skip.saturating_mul(channels)..)
            .unwrap_or(&[]);
        match channels {
            1 => {
                let mut stereo = Vec::with_capacity(samples.len() * CHANNELS);
                upmix_mono_to_stereo(samples, &mut stereo);
                self.mixer.push(source, start_frame, &stereo);
            }
            2 => self.mixer.push(source, start_frame, samples),
            _ => {}
        }
    }

    /// 输出已超出抖动窗口的槽。
    fn pump(&mut self) {
        let Some(timeline) = self.timeline.as_ref() else {
            return;
        };
        let now = frames_from_duration(timeline.active_at(Instant::now()));
        self.emit_until(now.saturating_sub(JITTER_FRAMES));
    }

    /// 把截止位置之前的槽交给后端。
    fn emit_until(&mut self, end_frame: u64) {
        let first = self.mixer.emitted_slots();
        self.scratch.clear();
        let count = self.mixer.drain_until(end_frame, &mut self.scratch);
        if count == 0 {
            return;
        }
        if let Some(sink) = self.sink.as_mut()
            && let Err(e) = sink.write(first, &self.scratch)
        {
            if !self.sink_error_logged {
                eprintln!("音频写入失败，之后的音频将丢弃: {e}");
                self.sink_error_logged = true;
            }
            self.sink = None;
        }
    }

    /// 汇报状态变化；`source` 为 `None` 表示所有启用的音源。
    fn set_status(&mut self, source: Option<AudioSource>, status: AudioStatus) {
        for s in [AudioSource::System, AudioSource::Microphone] {
            let idx = lane(s);
            if !self.mixer.enabled[idx]
                || source.is_some_and(|x| x != s)
                || self.last_status[idx] == Some(status)
            {
                continue;
            }
            self.last_status[idx] = Some(status);
            (self.report)(s, status);
        }
    }

    /// 收尾：吃掉队列里剩余事件，冲刷到停止时刻，结束音轨。
    fn finish(mut self, at: Instant) -> AudioReport {
        while let Ok(event) = self.source.next(Duration::ZERO) {
            self.on_event(event);
        }
        if let Some(timeline) = self.timeline.as_ref() {
            let end = frames_from_duration(timeline.active_at(at)).next_multiple_of(SLOT_FRAMES);
            self.emit_until(end);
        }
        let error = self.sink.take().and_then(|sink| sink.finish().err());
        self.report_now(error)
    }

    /// 生成报告。
    fn report_now(&self, error: Option<String>) -> AudioReport {
        AudioReport {
            slots: self.mixer.emitted_slots(),
            dropped_frames: self.mixer.dropped_frames(),
            error,
        }
    }
}

/// 采集库音源类别转协议音源。
fn to_protocol(kind: AudioSourceKind) -> AudioSource {
    match kind {
        AudioSourceKind::System => AudioSource::System,
        AudioSourceKind::Microphone => AudioSource::Microphone,
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use super::*;

    /// 构造指定帧数、恒定值的交错立体声。
    fn stereo(frames: usize, value: i16) -> Vec<i16> {
        vec![value; frames * CHANNELS]
    }

    /// 取出所有已到期槽。
    fn drain(mixer: &mut AudioMixer, end_frame: u64) -> Vec<i16> {
        let mut out = Vec::new();
        mixer.drain_until(end_frame, &mut out);
        out
    }

    /// 单路按 10ms 槽对齐：跨槽的包被拆开，槽内位置正确。
    #[test]
    fn packets_are_aligned_to_slots() {
        let mut m = AudioMixer::new(Some(100), None);
        // 起点在槽 0 的第 240 帧，长 480 帧：前半在槽 0，后半在槽 1
        m.push(AudioSource::System, 240, &stereo(480, 100));
        let out = drain(&mut m, 2 * SLOT_FRAMES);
        assert_eq!(out.len(), 2 * SLOT_SAMPLES);
        assert_eq!(out[0], 0);
        assert_eq!(out[239 * CHANNELS], 0);
        assert_eq!(out[240 * CHANNELS], 100);
        assert_eq!(out[SLOT_SAMPLES + 239 * CHANNELS + 1], 100);
        assert_eq!(out[SLOT_SAMPLES + 240 * CHANNELS], 0);
    }

    /// 两路相加并做 i16 饱和，不会回绕。
    #[test]
    fn mixing_saturates() {
        let mut m = AudioMixer::new(Some(100), Some(100));
        m.push(AudioSource::System, 0, &stereo(480, 30_000));
        m.push(AudioSource::Microphone, 0, &stereo(480, 30_000));
        let out = drain(&mut m, SLOT_FRAMES);
        assert!(out.iter().all(|&v| v == i16::MAX));
        let mut m = AudioMixer::new(Some(100), Some(100));
        m.push(AudioSource::System, 0, &stereo(480, -30_000));
        m.push(AudioSource::Microphone, 0, &stereo(480, -30_000));
        assert!(drain(&mut m, SLOT_FRAMES).iter().all(|&v| v == i16::MIN));
    }

    /// 增益按百分比定点缩放：0 静音、50 减半、200 加倍（再饱和）。
    #[test]
    fn gain_scales_samples() {
        let mut m = AudioMixer::new(Some(50), Some(200));
        m.push(AudioSource::System, 0, &stereo(480, 1000));
        m.push(AudioSource::Microphone, 0, &stereo(480, 100));
        assert_eq!(drain(&mut m, SLOT_FRAMES)[0], 500 + 200);
        let mut m = AudioMixer::new(Some(0), None);
        m.push(AudioSource::System, 0, &stereo(480, 1000));
        assert!(drain(&mut m, SLOT_FRAMES).iter().all(|&v| v == 0));
        let mut m = AudioMixer::new(None, Some(200));
        m.push(AudioSource::Microphone, 0, &stereo(480, 20_000));
        assert!(drain(&mut m, SLOT_FRAMES).iter().all(|&v| v == i16::MAX));
    }

    /// 未启用的音源被忽略。
    #[test]
    fn disabled_source_is_ignored() {
        let mut m = AudioMixer::new(Some(100), None);
        m.push(AudioSource::Microphone, 0, &stereo(480, 1000));
        assert!(drain(&mut m, SLOT_FRAMES).iter().all(|&v| v == 0));
    }

    /// 缺包的槽补零，输出始终连续。
    #[test]
    fn missing_slots_are_zero_filled_and_contiguous() {
        let mut m = AudioMixer::new(Some(100), None);
        m.push(AudioSource::System, 0, &stereo(480, 7));
        m.push(AudioSource::System, 5 * SLOT_FRAMES, &stereo(480, 9));
        let mut out = Vec::new();
        let n = m.drain_until(6 * SLOT_FRAMES, &mut out);
        assert_eq!(n, 6);
        assert_eq!(out.len(), 6 * SLOT_SAMPLES);
        assert_eq!(out[0], 7);
        assert!(out[SLOT_SAMPLES..5 * SLOT_SAMPLES].iter().all(|&v| v == 0));
        assert_eq!(out[5 * SLOT_SAMPLES], 9);
        assert_eq!(m.emitted_slots(), 6);
        // 没有数据时也能补零推进
        assert_eq!(m.drain_until(8 * SLOT_FRAMES, &mut out), 2);
    }

    /// 时间戳小抖动（20ms 内）时按连续位置放置，不留缝也不重叠。
    #[test]
    fn small_jitter_keeps_continuity() {
        let mut m = AudioMixer::new(Some(100), None);
        m.push(AudioSource::System, 0, &stereo(480, 1));
        m.push(AudioSource::System, 500, &stereo(480, 1)); // 时间戳晚了 20 帧，仍接在 480 处
        m.push(AudioSource::System, 950, &stereo(480, 1));
        let out = drain(&mut m, 3 * SLOT_FRAMES);
        assert!(
            out[..3 * SLOT_SAMPLES].iter().all(|&v| v == 1),
            "不应有缝隙或叠加"
        );
    }

    /// 迟到数据（早于已输出位置）被裁掉并计数。
    #[test]
    fn late_data_is_dropped() {
        let mut m = AudioMixer::new(Some(100), None);
        drain(&mut m, 2 * SLOT_FRAMES);
        m.reset_alignment(None);
        m.push(AudioSource::System, 0, &stereo(960, 5));
        assert_eq!(m.dropped_frames(), 960);
        m.reset_alignment(None);
        m.push(AudioSource::System, 2 * SLOT_FRAMES - 240, &stereo(480, 5));
        assert_eq!(m.dropped_frames(), 960 + 240);
        let out = drain(&mut m, 3 * SLOT_FRAMES);
        assert_eq!(out[0], 5);
    }

    /// 异常的远期时间戳被丢弃，内存有界。
    #[test]
    fn far_future_packet_is_rejected() {
        let mut m = AudioMixer::new(Some(100), None);
        m.push(
            AudioSource::System,
            (MAX_AHEAD_SLOTS + 10) * SLOT_FRAMES,
            &stereo(480, 5),
        );
        assert!(m.slots.is_empty());
        assert_eq!(m.dropped_frames(), 480);
    }

    /// 重新对齐后，按新时间戳放置而不是接在旧位置后。
    #[test]
    fn reset_alignment_uses_new_timestamp() {
        let mut m = AudioMixer::new(Some(100), None);
        m.push(AudioSource::System, 0, &stereo(480, 1));
        // 不重置：差距在容差内会被拉回连续位置
        m.push(AudioSource::System, 1000, &stereo(480, 2));
        assert_eq!(m.slots.get(&1).map(|b| b[0]), Some(2));
        m.reset_alignment(None);
        m.push(AudioSource::System, 2000, &stereo(480, 3));
        let slot4 = m.slots.get(&4).expect("按新时间戳落在槽 4");
        assert_eq!(slot4[(2000 % SLOT_FRAMES) as usize * CHANNELS], 3);
    }

    /// 单声道上混：左右相同，长度翻倍。
    #[test]
    fn mono_is_upmixed_to_stereo() {
        let mut out = vec![9];
        upmix_mono_to_stereo(&[1, -2, 3], &mut out);
        assert_eq!(out, vec![9, 1, 1, -2, -2, 3, 3]);
    }

    /// 暂停：暂停段内的包被丢弃，恢复后位置按有效时间换算（不含暂停）。
    #[test]
    fn pause_drops_packets_and_shifts_timeline() {
        let origin = Instant::now();
        let ms = Duration::from_millis;
        let mut tl = Timeline::new(origin, 1);
        // 有效 0..1000ms 之后暂停 500ms，再恢复
        tl.pause(origin + ms(1000));
        tl.resume(origin + ms(1500));
        // 暂停段内的包
        assert_eq!(
            place_packet(&tl, origin, origin + ms(1100), origin + ms(1110)),
            None
        );
        // 恢复之后 100ms 处的包：有效时间 1100ms
        let (start, skip) =
            place_packet(&tl, origin, origin + ms(1600), origin + ms(1610)).unwrap();
        assert_eq!(skip, 0);
        assert_eq!(start, 1100 * u64::from(SAMPLE_RATE) / 1000);
        // 暂停前的包不受影响
        let (start, _) = place_packet(&tl, origin, origin + ms(500), origin + ms(510)).unwrap();
        assert_eq!(start, 500 * u64::from(SAMPLE_RATE) / 1000);
    }

    /// 早于原点的包：整包早于原点丢弃；跨原点的包裁掉头部。
    #[test]
    fn packets_before_origin_are_trimmed() {
        let origin = Instant::now() + Duration::from_millis(100);
        let tl = Timeline::new(origin, 1);
        let ms = Duration::from_millis;
        assert_eq!(
            place_packet(&tl, origin, origin - ms(30), origin - ms(20)),
            None
        );
        let (start, skip) = place_packet(&tl, origin, origin - ms(5), origin + ms(5)).unwrap();
        assert_eq!(start, 0);
        assert_eq!(skip, 5 * u64::from(SAMPLE_RATE) / 1000);
    }

    /// 暂停与恢复期间混音器输出仍连续：恢复后接在暂停前的槽之后，不产生空洞。
    #[test]
    fn output_stays_contiguous_across_pause() {
        let mut m = AudioMixer::new(Some(100), None);
        m.push(AudioSource::System, 0, &stereo(4800, 4)); // 有效 0..100ms
        m.reset_alignment(None); // 暂停/恢复
        m.push(AudioSource::System, 4800, &stereo(4800, 6)); // 恢复后继续，有效 100..200ms
        let out = drain(&mut m, 9600);
        assert_eq!(out.len(), 20 * SLOT_SAMPLES);
        assert!(out[..10 * SLOT_SAMPLES].iter().all(|&v| v == 4));
        assert!(out[10 * SLOT_SAMPLES..].iter().all(|&v| v == 6));
    }

    /// 设备可用性判断：默认设备与指定 ID。
    #[test]
    fn device_usability() {
        use snow_audio_recorder::DeviceFlow;
        let dev = |id: &str, is_default: bool, is_active: bool| AudioDeviceInfo {
            id: id.into(),
            name: id.into(),
            is_default,
            is_active,
            flow: DeviceFlow::Render,
        };
        let list = vec![
            dev("a", true, true),
            dev("b", false, true),
            dev("c", false, false),
        ];
        assert!(source_usable(&list, &DeviceSelector::DefaultRender));
        assert!(source_usable(&list, &DeviceSelector::Id("b".into())));
        assert!(!source_usable(&list, &DeviceSelector::Id("c".into())));
        assert!(!source_usable(&list, &DeviceSelector::Id("zzz".into())));
        assert!(!source_usable(
            &[dev("a", true, false)],
            &DeviceSelector::DefaultRender
        ));
        assert!(!source_usable(&[], &DeviceSelector::DefaultCapture));
    }

    /// 采集配置：两路都非必需，麦克风单声道，未启用的路保持关闭。
    #[test]
    fn stream_config_is_optional_and_formatted() {
        let c = build_stream_config(Some(DeviceSelector::DefaultRender), None);
        assert!(c.system.enabled && !c.system.required);
        assert!(!c.microphone.enabled && !c.microphone.required);
        assert_eq!(c.system.output_format, AudioFormat::new(SAMPLE_RATE, 2));
        assert_eq!(c.microphone.output_format, AudioFormat::new(SAMPLE_RATE, 1));
        assert!(c.validate().is_ok());
        let c = build_stream_config(None, Some(DeviceSelector::Id("x".into())));
        assert!(c.microphone.enabled && c.microphone.device == DeviceSelector::Id("x".into()));
        assert!(build_stream_config(None, None).validate().is_err());
    }

    /// 记录写入的假后端。
    struct Recording(Arc<Mutex<Vec<(u64, usize)>>>);

    impl AudioSink for Recording {
        fn write(&mut self, first_slot: u64, pcm: &[i16]) -> Result<(), String> {
            self.0.lock().unwrap().push((first_slot, pcm.len()));
            Ok(())
        }
        fn finish(self: Box<Self>) -> Result<(), String> {
            Ok(())
        }
    }

    /// 永远没有事件的来源。
    struct Silent;

    impl EventSource for Silent {
        fn next(&self, timeout: Duration) -> Result<AudioEvent, RecvTimeoutError> {
            std::thread::sleep(timeout);
            Err(RecvTimeoutError::Timeout)
        }
    }

    /// 运行器端到端：无任何包时，停止时仍补零到停止时刻，槽号连续、时长正确；暂停段不计时长。
    #[test]
    fn runner_pads_silence_to_stop_time_excluding_pause() {
        let (tx, rx) = mpsc::channel();
        let written = Arc::new(Mutex::new(Vec::new()));
        let runner = Runner {
            source: Box::new(Silent),
            mixer: AudioMixer::new(Some(100), None),
            sink: Some(Box::new(Recording(Arc::clone(&written)))),
            report: Arc::new(|_, _| {}),
            control: rx,
            timeline: None,
            origin: None,
            pending: VecDeque::new(),
            scratch: Vec::new(),
            last_status: [None; 2],
            sink_error_logged: false,
        };
        let origin = Instant::now();
        let ms = Duration::from_millis;
        tx.send(Control::Origin(origin)).unwrap();
        tx.send(Control::Pause(origin + ms(300))).unwrap();
        tx.send(Control::Resume(origin + ms(700))).unwrap();
        // 有效时长 = 1000 - 400 = 600ms
        tx.send(Control::Stop(origin + ms(1000))).unwrap();
        let report = runner.run();
        assert_eq!(report.slots, 60);
        let total: usize = written.lock().unwrap().iter().map(|&(_, len)| len).sum();
        assert_eq!(total, 60 * SLOT_SAMPLES);
        let mut next = 0;
        for &(first, len) in written.lock().unwrap().iter() {
            assert_eq!(first, next, "槽号必须连续");
            next += (len / SLOT_SAMPLES) as u64;
        }
    }
}
