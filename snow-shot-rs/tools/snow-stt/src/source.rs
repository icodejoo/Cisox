//! 音频来源抽象：麦克风（复用 snow-audio-recorder）与测试用 wav 文件。

use std::time::Duration;

use snow_audio_recorder::{
    AudioError, AudioEvent, AudioFormat, AudioSession, AudioStreamConfig, AudioStreamHandle,
    DeviceSelector, RecvTimeoutError,
};

use crate::sherpa::SAMPLE_RATE;

/// 麦克风每个包的时长；越短越跟手，开销也越高。
const MIC_PACKET_MS: u64 = 20;
/// i16 转 f32 的归一化系数。
const I16_SCALE: f32 = 32768.0;
/// wav 来源每次交出的样本数（约 100ms，模拟采集包）。
const WAV_STEP_SAMPLES: usize = 1600;

/// 来源一次取样的结果。
#[derive(Debug, Clone, PartialEq)]
pub enum SourceEvent {
    /// 一批 16kHz 单声道样本。
    Samples(Vec<f32>),
    /// 超时内没有新数据。
    Idle,
    /// 来源正常结束（wav 读完）。
    Ended,
    /// 来源出错，原因为单行文本。
    Failed(String),
}

/// 音频来源。
pub trait AudioSource {
    /// 等待下一批样本。
    ///
    /// # 参数
    /// - `timeout`：最长等待时间。
    fn next(&mut self, timeout: Duration) -> SourceEvent;
}

/// 默认麦克风来源：采集与重采样都由 snow-audio-recorder 完成，直接得到 16k 单声道。
pub struct MicSource {
    /// 采集流句柄，丢弃时停止采集并释放设备。
    handle: AudioStreamHandle,
}

impl MicSource {
    /// 打开默认麦克风。
    ///
    /// # 返回
    /// 来源实例；设备不可用或被隐私设置拒绝时返回原因。
    pub fn open() -> Result<Self, String> {
        let mut cfg = AudioStreamConfig::default();
        cfg.system.enabled = false;
        cfg.system.required = false;
        cfg.microphone.enabled = true;
        cfg.microphone.required = true;
        cfg.microphone.device = DeviceSelector::DefaultCapture;
        cfg.microphone.output_format = AudioFormat::new(SAMPLE_RATE as u32, 1);
        cfg.microphone.packet_duration = Duration::from_millis(MIC_PACKET_MS);
        let session = AudioSession::new().map_err(|e| describe(&e))?;
        let handle = session.start_streaming(cfg).map_err(|e| describe(&e))?;
        Ok(Self { handle })
    }
}

/// 把音频错误压成单行文本。
fn describe(err: &AudioError) -> String {
    format!("麦克风不可用: {err}").replace(['\r', '\n'], " ")
}

impl AudioSource for MicSource {
    /// 取下一个采集事件；非数据事件当作空闲。
    fn next(&mut self, timeout: Duration) -> SourceEvent {
        match self.handle.recv_timeout(timeout) {
            Ok(AudioEvent::Packet(p)) => {
                SourceEvent::Samples(p.data.iter().map(|&s| f32::from(s) / I16_SCALE).collect())
            }
            Ok(AudioEvent::Error(e)) => SourceEvent::Failed(describe(&e)),
            Ok(AudioEvent::StreamEnded) => SourceEvent::Failed("麦克风采集已结束".into()),
            Ok(_) => SourceEvent::Idle,
            Err(RecvTimeoutError::Timeout) => SourceEvent::Idle,
            Err(RecvTimeoutError::Closed) => SourceEvent::Failed("麦克风采集通道已关闭".into()),
        }
    }
}

/// 测试用 wav 来源：整段样本切成小步依次交出，读完后返回 `Ended`。
pub struct WavSource {
    /// 全部样本（含尾部补的静音）。
    samples: Vec<f32>,
    /// 已交出的位置。
    pos: usize,
}

impl WavSource {
    /// 读取 16kHz 单声道 wav，并在末尾补静音（用于触发端点）。
    ///
    /// # 参数
    /// - `path`：wav 路径。
    /// - `pad_ms`：尾部补的静音毫秒数。
    ///
    /// # 返回
    /// 来源实例；读取失败或采样率不是 16k 时返回原因。
    pub fn open(path: &str, pad_ms: u32) -> Result<Self, String> {
        let wave = sherpa_onnx::Wave::read(path).ok_or_else(|| format!("无法读取 wav: {path}"))?;
        if wave.sample_rate() != SAMPLE_RATE {
            return Err(format!("wav 采样率须为 16000，实际 {}", wave.sample_rate()));
        }
        let mut samples = wave.samples().to_vec();
        samples.resize(
            samples.len() + (SAMPLE_RATE as usize * pad_ms as usize / 1000),
            0.0,
        );
        Ok(Self::from_samples(samples))
    }

    /// 由现成样本构造来源。
    ///
    /// # 参数
    /// - `samples`：16kHz 单声道样本。
    pub fn from_samples(samples: Vec<f32>) -> Self {
        Self { samples, pos: 0 }
    }
}

impl AudioSource for WavSource {
    /// 交出下一小步样本；读完后返回 `Ended`。
    fn next(&mut self, _timeout: Duration) -> SourceEvent {
        if self.pos >= self.samples.len() {
            return SourceEvent::Ended;
        }
        let end = (self.pos + WAV_STEP_SAMPLES).min(self.samples.len());
        let out = self.samples[self.pos..end].to_vec();
        self.pos = end;
        SourceEvent::Samples(out)
    }
}

/// 节拍来源：按实时节奏交出静音样本，不碰任何音频设备。
///
/// 系统语音后端自己采麦克风，主循环只需要一个按时间推进的节拍
/// （驱动后端轮询、命令响应与最长时长计数）。
pub struct ClockSource {
    /// 每个节拍的间隔。
    tick: Duration,
    /// 下一个节拍到期时刻。
    due: std::time::Instant,
}

impl ClockSource {
    /// 创建节拍来源。
    ///
    /// # 参数
    /// - `tick`：节拍间隔，同时决定每次交出的静音样本数（16kHz 折算）。
    pub fn new(tick: Duration) -> Self {
        Self {
            tick,
            due: std::time::Instant::now() + tick,
        }
    }
}

impl AudioSource for ClockSource {
    /// 到点交出一批静音样本，否则最多等 `timeout` 后返回空闲。
    fn next(&mut self, timeout: Duration) -> SourceEvent {
        let now = std::time::Instant::now();
        if now < self.due {
            std::thread::sleep(self.due.saturating_duration_since(now).min(timeout));
            if std::time::Instant::now() < self.due {
                return SourceEvent::Idle;
            }
        }
        self.due += self.tick;
        // 长时间没被调用时不连发补拍，直接对齐到当前
        let now = std::time::Instant::now();
        if self.due < now {
            self.due = now + self.tick;
        }
        let n = (SAMPLE_RATE as u128 * self.tick.as_millis() / 1000) as usize;
        SourceEvent::Samples(vec![0.0; n])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clock_source_ticks_in_real_time() {
        let mut s = ClockSource::new(Duration::from_millis(40));
        // 超时短于节拍：先空闲
        assert_eq!(s.next(Duration::from_millis(1)), SourceEvent::Idle);
        let started = std::time::Instant::now();
        let got = loop {
            match s.next(Duration::from_millis(50)) {
                SourceEvent::Idle => continue,
                other => break other,
            }
        };
        assert!(matches!(got, SourceEvent::Samples(v) if v.len() == 640));
        assert!(started.elapsed() < Duration::from_secs(2));
    }

    #[test]
    fn wav_source_steps_then_ends() {
        let mut s = WavSource::from_samples(vec![0.5; WAV_STEP_SAMPLES + 10]);
        let t = Duration::from_millis(1);
        assert!(matches!(s.next(t), SourceEvent::Samples(v) if v.len() == WAV_STEP_SAMPLES));
        assert!(matches!(s.next(t), SourceEvent::Samples(v) if v.len() == 10));
        assert_eq!(s.next(t), SourceEvent::Ended);
        assert_eq!(s.next(t), SourceEvent::Ended);
    }

    #[test]
    fn missing_wav_is_error() {
        assert!(WavSource::open("Z:/nope.wav", 0).is_err());
    }
}
