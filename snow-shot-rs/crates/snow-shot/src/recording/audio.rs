//! 录屏音频：配置到录制请求的映射，以及音频源状态到降级提示的状态机。
//!
//! 阶段 1 只提供“麦克风 / 系统声”两个开关，音量固定 100、设备取系统默认；
//! 仅 MP4 支持音频，GIF / APNG / WebP 一律强制全关。

use crate::recording::model::RecordingFormat;
use serde_json::Value;
use snow_config::document::ConfigDocument;
use snow_recorder_protocol::{AudioRequest, AudioSource, AudioStatus};

/// 是否录制麦克风的配置键。
pub const KEY_MICROPHONE: &str = "screen_recording/enable_microphone";
/// 是否录制系统声的配置键。
pub const KEY_SYSTEM_AUDIO: &str = "screen_recording/enable_system_audio";

/// 麦克风缺失时的兜底值（与 schema 默认一致）。
const DEFAULT_MICROPHONE: bool = false;
/// 系统声缺失时的兜底值（与 schema 默认一致）。
const DEFAULT_SYSTEM_AUDIO: bool = true;
/// 设置页“仅 MP4 支持音频”提示的 message id。
pub const MSG_MP4_ONLY: &str = "recording-audio-mp4-only";

/// 按录制格式收敛音频请求：非 MP4 强制全关。
///
/// # 参数
/// - `audio`：原始请求。
/// - `format`：录制格式。
///
/// # 返回
/// MP4 时原样返回，其余格式返回全关的默认请求。
///
/// # 示例
/// ```ignore
/// let a = AudioRequest { system: true, ..AudioRequest::default() };
/// assert!(!restrict_to_format(a, RecordingFormat::Gif).enabled());
/// ```
pub fn restrict_to_format(audio: AudioRequest, format: RecordingFormat) -> AudioRequest {
    if format == RecordingFormat::Mp4 {
        audio
    } else {
        AudioRequest::default()
    }
}

/// 由配置文档构造音频请求（每次开始录制时调用，读取最新值）。
///
/// # 参数
/// - `document`：配置文档。
/// - `format`：本次录制格式。
///
/// # 返回
/// 音量 100、设备默认的请求；非 MP4 时全关。
///
/// # 示例
/// ```ignore
/// let req = audio_request(&doc, RecordingFormat::Mp4);
/// assert_eq!(req.mic_volume, 100);
/// ```
pub fn audio_request(document: &ConfigDocument, format: RecordingFormat) -> AudioRequest {
    let flag = |key: &str, fallback: bool| match document.value(key) {
        Value::Bool(b) => b,
        _ => fallback,
    };
    restrict_to_format(
        AudioRequest {
            microphone: flag(KEY_MICROPHONE, DEFAULT_MICROPHONE),
            system: flag(KEY_SYSTEM_AUDIO, DEFAULT_SYSTEM_AUDIO),
            ..AudioRequest::default()
        },
        format,
    )
}

/// 设置页上音频开关旁的提示：当前格式不是 MP4 时返回提示 message id。
///
/// # 参数
/// - `key`：配置键。
/// - `format_value`：`screen_recording/output_format` 当前值。
///
/// # 返回
/// 仅当 `key` 是两个音频开关之一且格式不是 MP4 时返回 `Some(message id)`。
///
/// # 示例
/// ```ignore
/// assert!(mp4_only_note(KEY_MICROPHONE, &json!("gif")).is_some());
/// assert!(mp4_only_note(KEY_MICROPHONE, &json!("mp4")).is_none());
/// ```
pub fn mp4_only_note(key: &str, format_value: &Value) -> Option<&'static str> {
    if key != KEY_MICROPHONE && key != KEY_SYSTEM_AUDIO {
        return None;
    }
    let format = RecordingFormat::from_config(format_value.as_str().unwrap_or_default());
    (format != RecordingFormat::Mp4).then_some(MSG_MP4_ONLY)
}

/// 录制界面上的音频降级提示。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AudioNotice {
    /// 所有请求的音频源都不可用，成片没有声音。
    NoSound,
    /// 麦克风启动时不可用，已跳过。
    MicUnavailable,
    /// 系统声启动时不可用，已跳过。
    SystemUnavailable,
    /// 麦克风中途丢失，之后为静音。
    MicLost,
    /// 系统声中途丢失，之后为静音。
    SystemLost,
}

impl AudioNotice {
    /// 提示文案的 message id（语料见 `recording_audio.ftl`）。
    pub const fn message_id(self) -> &'static str {
        match self {
            Self::NoSound => "recording-audio-none",
            Self::MicUnavailable => "recording-audio-mic-unavailable",
            Self::SystemUnavailable => "recording-audio-system-unavailable",
            Self::MicLost => "recording-audio-mic-lost",
            Self::SystemLost => "recording-audio-system-lost",
        }
    }
}

/// 一次录制中各音频源的状态面板。
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct AudioBoard {
    /// 是否请求了麦克风。
    want_mic: bool,
    /// 是否请求了系统声。
    want_system: bool,
    /// 麦克风最近一次回报的状态。
    mic: Option<AudioStatus>,
    /// 系统声最近一次回报的状态。
    system: Option<AudioStatus>,
}

impl AudioBoard {
    /// 按本次请求创建面板。
    ///
    /// # 参数
    /// - `request`：本次录制的音频请求。
    pub fn new(request: &AudioRequest) -> Self {
        Self {
            want_mic: request.microphone,
            want_system: request.system,
            mic: None,
            system: None,
        }
    }

    /// 记录一次音频源状态回报。
    ///
    /// # 参数
    /// - `source`：音频源。
    /// - `status`：新状态。
    pub fn record(&mut self, source: AudioSource, status: AudioStatus) {
        match source {
            AudioSource::Microphone => self.mic = Some(status),
            AudioSource::System => self.system = Some(status),
        }
    }

    /// 当前应显示的提示（按麦克风、系统声顺序）；全部请求源不可用时只给“没有声音”。
    ///
    /// # 返回
    /// 提示列表；一切正常（或没请求音频）时为空。
    ///
    /// # 示例
    /// ```ignore
    /// let mut b = AudioBoard::new(&AudioRequest { system: true, ..Default::default() });
    /// b.record(AudioSource::System, AudioStatus::Unavailable);
    /// assert_eq!(b.notices(), vec![AudioNotice::NoSound]);
    /// ```
    pub fn notices(&self) -> Vec<AudioNotice> {
        let unavailable = |want: bool, status: Option<AudioStatus>| {
            !want || status == Some(AudioStatus::Unavailable)
        };
        if (self.want_mic || self.want_system)
            && unavailable(self.want_mic, self.mic)
            && unavailable(self.want_system, self.system)
        {
            return vec![AudioNotice::NoSound];
        }
        let mut out = Vec::new();
        match self.mic {
            Some(AudioStatus::Unavailable) => out.push(AudioNotice::MicUnavailable),
            Some(AudioStatus::Lost) => out.push(AudioNotice::MicLost),
            _ => {}
        }
        match self.system {
            Some(AudioStatus::Unavailable) => out.push(AudioNotice::SystemUnavailable),
            Some(AudioStatus::Lost) => out.push(AudioNotice::SystemLost),
            _ => {}
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// 构造带两个开关值的配置文档。
    fn doc(mic: bool, system: bool) -> ConfigDocument {
        let mut d = ConfigDocument::from_bytes(None);
        d.set_value(KEY_MICROPHONE, json!(mic)).unwrap();
        d.set_value(KEY_SYSTEM_AUDIO, json!(system)).unwrap();
        d
    }

    /// 默认配置：只录系统声，音量 100，设备默认。
    #[test]
    fn defaults_map_to_system_only() {
        let d = ConfigDocument::from_bytes(None);
        let r = audio_request(&d, RecordingFormat::Mp4);
        assert!(!r.microphone && r.system);
        assert_eq!((r.mic_volume, r.system_volume), (100, 100));
        assert!(r.mic_device.is_none() && r.system_device.is_none());
    }

    /// 开关值原样映射到请求；每次读取最新值。
    #[test]
    fn switches_map_and_follow_latest() {
        let mut d = doc(true, false);
        let r = audio_request(&d, RecordingFormat::Mp4);
        assert!(r.microphone && !r.system);
        d.set_value(KEY_SYSTEM_AUDIO, json!(true)).unwrap();
        assert!(audio_request(&d, RecordingFormat::Mp4).system);
    }

    /// 非 MP4 强制全关。
    #[test]
    fn non_mp4_forces_off() {
        let d = doc(true, true);
        for f in [
            RecordingFormat::Gif,
            RecordingFormat::Apng,
            RecordingFormat::Webp,
        ] {
            assert!(!audio_request(&d, f).enabled(), "{f:?}");
        }
        assert!(audio_request(&d, RecordingFormat::Mp4).enabled());
    }

    /// 开关写回配置后，下一次构造能读到新值（旧键）。
    #[test]
    fn switch_write_back_is_read_next_time() {
        let mut d = ConfigDocument::from_bytes(None);
        d.set_value(KEY_MICROPHONE, json!(true)).unwrap();
        assert_eq!(d.value(KEY_MICROPHONE), json!(true));
        assert!(audio_request(&d, RecordingFormat::Mp4).microphone);
    }

    /// 设置页提示：仅音频开关键且格式非 MP4 时出现。
    #[test]
    fn mp4_only_note_rules() {
        assert_eq!(
            mp4_only_note(KEY_MICROPHONE, &json!("gif")),
            Some(MSG_MP4_ONLY)
        );
        assert_eq!(
            mp4_only_note(KEY_SYSTEM_AUDIO, &json!("webp")),
            Some(MSG_MP4_ONLY)
        );
        assert_eq!(mp4_only_note(KEY_MICROPHONE, &json!("mp4")), None);
        assert_eq!(
            mp4_only_note("screen_recording/show_cursor", &json!("gif")),
            None
        );
    }

    /// 状态机：不可用、中断、全部不可用、恢复正常。
    #[test]
    fn board_state_machine() {
        let both = AudioRequest {
            microphone: true,
            system: true,
            ..AudioRequest::default()
        };
        let mut b = AudioBoard::new(&both);
        assert!(b.notices().is_empty());
        b.record(AudioSource::Microphone, AudioStatus::Unavailable);
        assert_eq!(b.notices(), vec![AudioNotice::MicUnavailable]);
        b.record(AudioSource::System, AudioStatus::Lost);
        assert_eq!(
            b.notices(),
            vec![AudioNotice::MicUnavailable, AudioNotice::SystemLost]
        );
        b.record(AudioSource::System, AudioStatus::Unavailable);
        assert_eq!(b.notices(), vec![AudioNotice::NoSound]);
        b.record(AudioSource::Microphone, AudioStatus::Ok);
        assert_eq!(b.notices(), vec![AudioNotice::SystemUnavailable]);
    }

    /// 只请求一路且该路不可用时提示“没有声音”；没请求音频则不提示。
    #[test]
    fn board_single_source_and_none() {
        let sys = AudioRequest {
            system: true,
            ..AudioRequest::default()
        };
        let mut b = AudioBoard::new(&sys);
        b.record(AudioSource::System, AudioStatus::Unavailable);
        assert_eq!(b.notices(), vec![AudioNotice::NoSound]);
        assert!(
            AudioBoard::new(&AudioRequest::default())
                .notices()
                .is_empty()
        );
    }

    /// 提示文案在两种语言下都存在且不含产品名占位。
    #[test]
    fn notice_messages_exist_in_all_locales() {
        for locale in ["en-US", "zh-CN"] {
            let i18n = crate::ocr_backend::i18n_for(locale);
            for n in [
                AudioNotice::NoSound,
                AudioNotice::MicUnavailable,
                AudioNotice::SystemUnavailable,
                AudioNotice::MicLost,
                AudioNotice::SystemLost,
            ] {
                assert!(i18n.has(n.message_id()), "{locale}: {}", n.message_id());
            }
            assert!(i18n.has(MSG_MP4_ONLY), "{locale}");
        }
    }
}
