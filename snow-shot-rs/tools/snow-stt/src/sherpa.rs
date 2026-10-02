//! sherpa-onnx 流式 transducer 后端（拉取式：喂音频、解码、取文本、判端点）。

use std::path::Path;

use sherpa_onnx::{
    OnlineModelConfig, OnlineRecognizer, OnlineRecognizerConfig, OnlineStream,
    OnlineTransducerModelConfig,
};
use snow_stt_protocol::StartRequest;

use crate::backend::{SttBackend, SttEvent, discover_model};

/// 识别器采样率。
pub const SAMPLE_RATE: i32 = 16_000;
/// 冲刷尾部时补的静音时长（秒），让模型把最后几帧吐完。
const TAIL_PAD_SECONDS: f32 = 0.66;
/// 毫秒转秒的除数。
const MS_PER_SECOND: f32 = 1000.0;

/// sherpa 流式识别后端，持有识别器与单路流。
pub struct SherpaBackend {
    /// 识别器（先于流之外的资源保持存活）。
    recognizer: OnlineRecognizer,
    /// 当前识别流。
    stream: OnlineStream,
}

impl SherpaBackend {
    /// 按请求加载模型并创建识别流。
    ///
    /// # 参数
    /// - `req`：START 请求（模型目录、线程数、端点规则）。
    ///
    /// # 返回
    /// 后端实例；模型缺失或加载失败时返回中文原因。
    pub fn load(req: &StartRequest) -> Result<Self, String> {
        let files = discover_model(Path::new(&req.model_dir))?;
        let cfg = OnlineRecognizerConfig {
            model_config: OnlineModelConfig {
                transducer: OnlineTransducerModelConfig {
                    encoder: Some(files.encoder),
                    decoder: Some(files.decoder),
                    joiner: Some(files.joiner),
                },
                tokens: Some(files.tokens),
                num_threads: req.threads as i32,
                provider: Some("cpu".into()),
                ..Default::default()
            },
            decoding_method: Some("greedy_search".into()),
            enable_endpoint: true,
            rule1_min_trailing_silence: req.endpoint.rule1_ms as f32 / MS_PER_SECOND,
            rule2_min_trailing_silence: req.endpoint.rule2_ms as f32 / MS_PER_SECOND,
            rule3_min_utterance_length: req.endpoint.rule3_ms as f32 / MS_PER_SECOND,
            ..Default::default()
        };
        let recognizer = OnlineRecognizer::create(&cfg)
            .ok_or_else(|| "sherpa 创建识别器失败（模型文件损坏或不兼容）".to_string())?;
        let stream = recognizer.create_stream();
        Ok(Self { recognizer, stream })
    }

    /// 解码所有已就绪的帧。
    fn decode_ready(&self) {
        while self.recognizer.is_ready(&self.stream) {
            self.recognizer.decode(&self.stream);
        }
    }

    /// 取当前文本（已去首尾空白）。
    fn current_text(&self) -> String {
        self.recognizer
            .get_result(&self.stream)
            .map(|r| r.text.trim().to_string())
            .unwrap_or_default()
    }
}

impl SttBackend for SherpaBackend {
    /// 把样本交给识别流。
    fn feed(&mut self, pcm: &[f32]) {
        self.stream.accept_waveform(SAMPLE_RATE, pcm);
    }

    /// 解码并取结果；端点触发时给出定稿并重置流。
    fn poll(&mut self) -> Vec<SttEvent> {
        self.decode_ready();
        let text = self.current_text();
        if self.recognizer.is_endpoint(&self.stream) {
            self.recognizer.reset(&self.stream);
            if text.is_empty() {
                return Vec::new();
            }
            return vec![SttEvent::Final(text)];
        }
        if text.is_empty() {
            Vec::new()
        } else {
            vec![SttEvent::Partial(text)]
        }
    }

    /// 补尾部静音、标记输入结束并取出最后一句。
    fn finish(&mut self) -> Vec<SttEvent> {
        let pad = vec![0.0f32; (SAMPLE_RATE as f32 * TAIL_PAD_SECONDS) as usize];
        self.stream.accept_waveform(SAMPLE_RATE, &pad);
        self.stream.input_finished();
        self.decode_ready();
        let text = self.current_text();
        if text.is_empty() {
            Vec::new()
        } else {
            vec![SttEvent::Final(text)]
        }
    }
}
