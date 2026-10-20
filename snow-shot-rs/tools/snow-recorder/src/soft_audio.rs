//! 软件后端复用硬件路径的音频采集、混音与 AAC 输入，停止后无损合并两条轨道。

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use ffmpeg_next as ffmpeg;
use snow_recorder_protocol::{MediaFormat, StartRequest};

use crate::audio::{self, AudioCapture, AudioRecorder, AudioSink};
use crate::win::aacsink::{AacEncoder, AacSink};

/// 独立音轨封装，确保 AAC 冲刷后写入容器尾。
struct FileSink {
    /// 共用的 AAC 输入。
    sink: AacSink,
    /// 音轨封装。
    output: Arc<Mutex<ffmpeg::format::context::Output>>,
}

impl AudioSink for FileSink {
    /// 转交同一 PCM 写入实现。
    fn write(&mut self, first_slot: u64, pcm: &[i16]) -> Result<(), String> {
        self.sink.write(first_slot, pcm)
    }

    /// 结束 AAC 编码并写尾。
    fn finish(self: Box<Self>) -> Result<(), String> {
        Box::new(self.sink).finish()?;
        self.output
            .lock()
            .map_err(|e| e.to_string())?
            .write_trailer()
            .map_err(|e| e.to_string())
    }
}

/// 创建独立 AAC 音轨，生产与离屏测试共用。
fn file_sink(path: &Path) -> Result<FileSink, String> {
    let enc = AacEncoder::open(true)?;
    let mut output =
        ffmpeg::format::output_as(path, "mp4").map_err(|e| format!("create audio MP4: {e}"))?;
    let index = {
        let mut stream = output.add_stream(enc.codec()).map_err(|e| e.to_string())?;
        stream.set_parameters(enc.encoder());
        stream.set_time_base((1, audio::SAMPLE_RATE as i32));
        stream.index()
    };
    output.write_header().map_err(|e| e.to_string())?;
    let tb = output
        .stream(index)
        .ok_or("missing audio stream")?
        .time_base();
    let output = Arc::new(Mutex::new(output));
    let sink = AacSink::new(enc, Arc::clone(&output), index, tb);
    Ok(FileSink { sink, output })
}

/// 软件会话的音轨与临时路径。
pub struct SoftAudio {
    /// 同一采集和混音运行器。
    recorder: Option<AudioRecorder>,
    /// 音轨是否完整，写入降级后不合并损坏音轨。
    usable: bool,
    /// 独立音轨路径，位于 worker 中间目录。
    path: PathBuf,
}

impl SoftAudio {
    /// 按原始请求打开设备，零音量也保持该路启用并输出静音。
    pub fn open(request: &StartRequest, video: &Path) -> Result<Option<Self>, String> {
        if request.format != MediaFormat::Mp4 || !request.audio.enabled() {
            return Ok(None);
        }
        let Some(capture) = AudioCapture::open(&request.audio, audio::stdout_reporter()) else {
            return Ok(None);
        };
        let path = video.with_extension("audio.m4a");
        let sink = file_sink(&path)?;
        Ok(Some(Self {
            recorder: Some(capture.run(Box::new(sink))),
            path,
            usable: true,
        }))
    }

    /// 以视频会话就绪时刻建立音频原点，丢弃设备初始化期间的采样。
    pub fn set_origin(&self, at: Instant) {
        if let Some(recorder) = &self.recorder {
            recorder.set_origin(at);
        }
    }

    /// 暂停共同时间线。
    pub fn pause(&self, at: Instant) {
        if let Some(recorder) = &self.recorder {
            recorder.pause(at);
        }
    }

    /// 恢复共同时间线。
    pub fn resume(&self, at: Instant) {
        if let Some(recorder) = &self.recorder {
            recorder.resume(at);
        }
    }

    /// 先结束音频采集，返回可保留到最终报告的诊断。
    pub fn stop(&mut self, at: Instant) -> String {
        let report = self.recorder.take().and_then(|r| r.stop(at));
        self.usable = report.as_ref().is_some_and(|r| r.error.is_none());
        report.map_or_else(|| "audio thread ended".into(), |r| r.describe())
    }

    /// 无损合并已结束的视频和音轨，成功后才替换视频文件。
    pub fn merge(&self, video: &Path) -> Result<(), String> {
        if self.usable {
            remux(video, &self.path).map_err(|e| e.to_string())
        } else {
            Ok(())
        }
    }
}

impl Drop for SoftAudio {
    /// 取消时先停止音频线程再清理中间音轨。
    fn drop(&mut self) {
        drop(self.recorder.take());
        let _ = std::fs::remove_file(&self.path);
    }
}

/// 比较不同时间基的包，确保按解码时间交错写出且内存有界。
fn packet_time(packet: &ffmpeg::Packet, tb: ffmpeg::Rational) -> i128 {
    i128::from(packet.dts().or(packet.pts()).unwrap_or(0)) * i128::from(tb.numerator()) * 1_000_000
        / i128::from(tb.denominator())
}

/// 两路各保留一个待写包，复制编码数据而不重新编码。
fn remux(video: &Path, audio: &Path) -> Result<(), ffmpeg::Error> {
    let merged = video.with_extension("merged.mp4");
    let mut v = ffmpeg::format::input(video)?;
    let mut a = ffmpeg::format::input(audio)?;
    let mut out = ffmpeg::format::output(&merged)?;
    let mut inputs = Vec::new();
    for (input, kind) in [
        (&v, ffmpeg::media::Type::Video),
        (&a, ffmpeg::media::Type::Audio),
    ] {
        let stream = input
            .streams()
            .best(kind)
            .ok_or(ffmpeg::Error::StreamNotFound)?;
        let mut target = out.add_stream(ffmpeg::encoder::find(ffmpeg::codec::Id::None))?;
        target.set_parameters(stream.parameters());
        // SAFETY: 输出流参数由 FFmpeg 分配；重封装须清除原容器 codec_tag。
        unsafe {
            (*target.parameters().as_mut_ptr()).codec_tag = 0;
        }
        inputs.push((stream.index(), stream.time_base()));
    }
    out.write_header()?;
    let outputs = [
        out.stream(0).unwrap().time_base(),
        out.stream(1).unwrap().time_base(),
    ];
    let mut vp = v
        .packets()
        .filter(|(s, _)| s.index() == inputs[0].0)
        .peekable();
    let mut ap = a
        .packets()
        .filter(|(s, _)| s.index() == inputs[1].0)
        .peekable();
    loop {
        let track = match (vp.peek(), ap.peek()) {
            (None, None) => break,
            (Some(_), None) => 0,
            (None, Some(_)) => 1,
            (Some((_, p)), Some((_, q))) => {
                usize::from(packet_time(p, inputs[0].1) > packet_time(q, inputs[1].1))
            }
        };
        let (_, mut packet) = if track == 0 {
            vp.next().unwrap()
        } else {
            ap.next().unwrap()
        };
        packet.set_stream(track);
        packet.set_position(-1);
        packet.rescale_ts(inputs[track].1, outputs[track]);
        packet.write_interleaved(&mut out)?;
    }
    out.write_trailer()?;
    drop(vp);
    drop(ap);
    drop(v);
    drop(a);
    drop(out);
    let backup = video.with_extension("video-only.mp4");
    std::fs::rename(video, &backup).map_err(|_| ffmpeg::Error::External)?;
    if std::fs::rename(&merged, video).is_err() {
        let _ = std::fs::rename(&backup, video);
        return Err(ffmpeg::Error::External);
    }
    let _ = std::fs::remove_file(backup);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use snow_audio_recorder::DeviceSelector;
    use snow_recorder_protocol::{AudioRequest, AudioSource};
    use snow_screen_recorder::{
        ExportExecutionMode, ExportFormat, SoftwareH264Priority, StreamingEncoder,
        StreamingEncoderConfig, VideoCodec, VideoEncodeConfig,
    };

    /// 指定非默认设备与零音量走共用选择及混音，真实 AAC/MP4 合并保持双轨和视频包数。
    #[test]
    fn software_audio_uses_selected_devices_and_muted_mix() {
        let request = AudioRequest {
            system: true,
            microphone: true,
            system_device: Some("nondefault-render".into()),
            mic_device: Some("nondefault-mic".into()),
            system_volume: 0,
            mic_volume: 0,
        };
        assert_eq!(
            audio::requested_device(&request, AudioSource::System),
            DeviceSelector::Id("nondefault-render".into())
        );
        assert_eq!(
            audio::requested_device(&request, AudioSource::Microphone),
            DeviceSelector::Id("nondefault-mic".into())
        );
        let mut mixer =
            audio::AudioMixer::new(Some(request.system_volume), Some(request.mic_volume));
        mixer.push(AudioSource::System, 0, &vec![12000; 96000]);
        mixer.push(AudioSource::Microphone, 0, &vec![-10000; 96000]);
        let mut pcm = Vec::new();
        mixer.drain_until(48000, &mut pcm);
        assert_eq!(pcm.len(), 96000);
        assert!(pcm.iter().all(|&s| s == 0));
        let dir =
            std::env::temp_dir().join(format!("cisox-soft-audio-review-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let video = dir.join("video.mp4");
        let track = dir.join("audio.m4a");
        let config = StreamingEncoderConfig {
            output_path: video.clone(),
            format: ExportFormat::Mp4,
            width: 16,
            height: 16,
            fps: 10,
            codec: VideoCodec::H264,
            prefer_hardware_h264: false,
            execution_mode: ExportExecutionMode::SoftwareOnly,
            software_h264_priority: SoftwareH264Priority::X264First,
            video: VideoEncodeConfig::default(),
            encode_threads: 1,
            audio: None,
            loop_animated_images: true,
        };
        let mut encoder = StreamingEncoder::create(config).unwrap();
        for pts in 0..10 {
            encoder
                .push_rgba_frame_at_pts(pts, &[20, 30, 40, 255].repeat(16 * 16))
                .unwrap();
        }
        encoder.finish_at_pts(10).unwrap();
        let mut sink = file_sink(&track).unwrap();
        sink.write(0, &pcm).unwrap();
        Box::new(sink).finish().unwrap();
        remux(&video, &track).unwrap();
        let mut input = ffmpeg::format::input(&video).unwrap();
        assert_eq!(input.streams().count(), 2);
        let mut counts = [0; 2];
        for (stream, _) in input.packets() {
            counts[stream.index()] += 1;
        }
        assert_eq!(counts[0], 10);
        assert!(counts[1] > 0);
        assert!(input.duration() >= 1_000_000);
        drop(input);
        std::fs::remove_dir_all(dir).unwrap();
    }
}
