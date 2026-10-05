//! FFmpeg 路径的音频输入：原生 `aac` 编码器（LC，128 kbps，48k 立体声），写入与视频共用的 MP4 封装。
//!
//! 输入是混音器给的连续 48k 立体声 i16 PCM；这里手写 i16 -> fltp 转换（不依赖 swresample 绑定），
//! 按编码器帧长（通常 1024）攒帧。封装上下文由视频编码线程与本音频线程共享，写包时短暂持锁。

use std::sync::{Arc, Mutex};

use ffmpeg_next as ffmpeg;

use crate::audio::{AudioSink, SAMPLE_RATE};

/// AAC 码率（bps）。
pub const AAC_BITRATE: usize = 128_000;
/// 编码器没有固定帧长时使用的帧长。
const DEFAULT_FRAME_SIZE: usize = 1024;
/// i16 转 f32 的缩放（-32768 对应 -1.0）。
const I16_SCALE: f32 = 1.0 / 32_768.0;

/// 已打开的 AAC 编码器（尚未接入封装）。
pub struct AacEncoder {
    /// 编码器实例。
    encoder: ffmpeg::encoder::audio::Encoder,
    /// 编码器帧长（每声道采样数）。
    frame_size: usize,
}

impl AacEncoder {
    /// 打开 AAC 编码器。
    ///
    /// # 参数
    /// - `global_header`：容器是否要求全局头（MP4 为真）。
    ///
    /// # 返回
    /// 编码器；缺少 `aac` 编码器或打开失败返回原因。
    pub fn open(global_header: bool) -> Result<Self, String> {
        ffmpeg::init().map_err(|e| e.to_string())?;
        let codec = ffmpeg::encoder::find_by_name("aac").ok_or("FFmpeg 缺少 aac 编码器")?;
        let mut audio = ffmpeg::codec::context::Context::new_with_codec(codec)
            .encoder()
            .audio()
            .map_err(|e| format!("创建 AAC 编码器上下文失败: {e}"))?;
        audio.set_rate(SAMPLE_RATE as i32);
        audio.set_channel_layout(ffmpeg::ChannelLayout::default(2));
        audio.set_format(ffmpeg::format::Sample::F32(
            ffmpeg::format::sample::Type::Planar,
        ));
        audio.set_bit_rate(AAC_BITRATE);
        audio.set_time_base((1, SAMPLE_RATE as i32));
        if global_header {
            audio.set_flags(ffmpeg::codec::Flags::GLOBAL_HEADER);
        }
        let mut options = ffmpeg::Dictionary::new();
        options.set("profile", "aac_low");
        let encoder = audio
            .open_as_with(codec, options)
            .map_err(|e| format!("打开 AAC 编码器失败: {e}"))?;
        let frame_size = match encoder.frame_size() as usize {
            0 => DEFAULT_FRAME_SIZE,
            n => n,
        };
        Ok(Self {
            encoder,
            frame_size,
        })
    }

    /// 编码器所用的编解码器（用于给封装添加音轨）。
    pub fn codec(&self) -> Option<ffmpeg::Codec> {
        self.encoder.codec()
    }

    /// 编码器本体（用于设置音轨参数）。
    pub fn encoder(&self) -> &ffmpeg::encoder::audio::Encoder {
        &self.encoder
    }
}

/// 把交错 i16 立体声拆成两路 f32 追加到 `left`、`right`。
///
/// # 参数
/// - `pcm`：交错立体声采样（长度须为偶数，多余的尾部单个采样忽略）。
/// - `left`、`right`：输出缓冲（追加）。
pub fn deinterleave_to_f32(pcm: &[i16], left: &mut Vec<f32>, right: &mut Vec<f32>) {
    for frame in pcm.chunks_exact(2) {
        left.push(f32::from(frame[0]) * I16_SCALE);
        right.push(f32::from(frame[1]) * I16_SCALE);
    }
}

/// 接入封装的 AAC 音轨：由音频线程驱动。
pub struct AacSink {
    /// 编码器。
    enc: AacEncoder,
    /// 与视频共享的封装。
    output: Arc<Mutex<ffmpeg::format::context::Output>>,
    /// 音轨序号。
    stream_index: usize,
    /// 封装层时间基。
    stream_time_base: ffmpeg::Rational,
    /// 左声道待编码采样。
    left: Vec<f32>,
    /// 右声道待编码采样。
    right: Vec<f32>,
    /// 下一帧的 pts（编码器时间基 1/48000，单位为采样数）。
    next_pts: i64,
}

impl AacSink {
    /// 组装音轨。
    ///
    /// # 参数
    /// - `enc`：已打开的编码器。
    /// - `output`：共享封装（音轨须已在写头前添加）。
    /// - `stream_index`：音轨序号。
    /// - `stream_time_base`：写头后的音轨时间基。
    pub fn new(
        enc: AacEncoder,
        output: Arc<Mutex<ffmpeg::format::context::Output>>,
        stream_index: usize,
        stream_time_base: ffmpeg::Rational,
    ) -> Self {
        Self {
            enc,
            output,
            stream_index,
            stream_time_base,
            left: Vec::new(),
            right: Vec::new(),
            next_pts: 0,
        }
    }

    /// 取满一帧的采样送编码并写出已完成的包。
    fn encode_ready(&mut self) -> Result<(), String> {
        let n = self.enc.frame_size;
        while self.left.len() >= n {
            self.encode_frame(n)?;
        }
        Ok(())
    }

    /// 取出前 `n` 个采样（不足部分补零，用于收尾）编码为一帧。
    fn encode_frame(&mut self, n: usize) -> Result<(), String> {
        let mut frame = ffmpeg::frame::Audio::new(
            ffmpeg::format::Sample::F32(ffmpeg::format::sample::Type::Planar),
            n,
            ffmpeg::ChannelLayout::default(2),
        );
        frame.set_rate(SAMPLE_RATE);
        frame.set_pts(Some(self.next_pts));
        let available = self.left.len().min(n);
        for (plane, src) in [(0usize, &self.left), (1usize, &self.right)] {
            let dst = frame.plane_mut::<f32>(plane);
            dst[..available].copy_from_slice(&src[..available]);
            dst[available..].fill(0.0);
        }
        self.left.drain(..available);
        self.right.drain(..available);
        self.next_pts += n as i64;
        self.enc
            .encoder
            .send_frame(&frame)
            .map_err(|e| format!("AAC 送帧失败: {e}"))?;
        self.drain()
    }

    /// 取走编码器里已完成的包并写入封装。
    fn drain(&mut self) -> Result<(), String> {
        loop {
            let mut packet = ffmpeg::Packet::empty();
            match self.enc.encoder.receive_packet(&mut packet) {
                Ok(()) => {
                    packet.set_stream(self.stream_index);
                    packet.rescale_ts(
                        ffmpeg::Rational(1, SAMPLE_RATE as i32),
                        self.stream_time_base,
                    );
                    let mut output = self.output.lock().map_err(|_| "封装锁已损坏".to_string())?;
                    packet
                        .write_interleaved(&mut output)
                        .map_err(|e| format!("写 AAC 包失败: {e}"))?;
                }
                Err(ffmpeg::Error::Eof) => return Ok(()),
                Err(ffmpeg::Error::Other { errno }) if errno == ffmpeg::error::EAGAIN => {
                    return Ok(());
                }
                Err(e) => return Err(format!("AAC 取包失败: {e}")),
            }
        }
    }
}

impl AudioSink for AacSink {
    /// 转换并编码一批槽。
    fn write(&mut self, _first_slot: u64, pcm: &[i16]) -> Result<(), String> {
        deinterleave_to_f32(pcm, &mut self.left, &mut self.right);
        self.encode_ready()
    }

    /// 补齐末帧（补零）、冲刷编码器。
    fn finish(mut self: Box<Self>) -> Result<(), String> {
        if !self.left.is_empty() {
            let n = self.enc.frame_size;
            self.encode_frame(n)?;
        }
        self.enc
            .encoder
            .send_eof()
            .map_err(|e| format!("AAC 送 EOF 失败: {e}"))?;
        self.drain()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 拆声道并缩放：满幅负值映射到 -1.0。
    #[test]
    fn deinterleave_scales_to_unit_range() {
        let (mut l, mut r) = (Vec::new(), Vec::new());
        deinterleave_to_f32(&[i16::MIN, 16_384, 0, i16::MAX, 5], &mut l, &mut r);
        assert_eq!(l, vec![-1.0, 0.0]);
        assert_eq!(r, vec![0.5, i16::MAX as f32 / 32_768.0]);
    }

    /// 编码器可以打开，帧长为 AAC 的 1024。
    #[test]
    fn aac_encoder_opens() {
        let enc = AacEncoder::open(true).expect("静态 FFmpeg 应带原生 aac 编码器");
        assert_eq!(enc.frame_size, 1024);
        assert!(enc.codec().is_some());
    }
}
