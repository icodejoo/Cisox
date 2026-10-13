//! sherpa-onnx 流式 transducer 后端（拉取式：喂音频、解码、取文本、判端点）。

use std::path::{Path, PathBuf};

use sherpa_onnx::{
    OnlineModelConfig, OnlineRecognizer, OnlineRecognizerConfig, OnlineStream,
    OnlineTransducerModelConfig,
};
use snow_stt_protocol::StartRequest;

use crate::backend::{ModelFiles, SttBackend, SttEvent, discover_model};
use crate::optcache::{self, LoadOpts, LoadPlan, ONLINE_TRANSDUCER_TYPES};

/// 识别器采样率。
pub const SAMPLE_RATE: i32 = 16_000;
/// 冲刷尾部时补的静音时长（秒），让模型把最后几帧吐完。
const TAIL_PAD_SECONDS: f32 = 0.66;
/// 毫秒转秒的除数。
pub const MS_PER_SECOND: f32 = 1000.0;
/// 创建识别器失败时的提示。
const LOAD_FAILED: &str = "sherpa 创建识别器失败（模型文件损坏或不兼容）";

/// 一次加载的报告：实际用了什么，以及结束后是否要后台生成缓存。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoadReport {
    /// 最终采用的加载方案（回退后反映回退后的状态）。
    pub plan: LoadPlan,
    /// 加速参数导致创建失败、已回退到最原始的加载方式。
    pub fell_back: bool,
    /// 模型目录。
    model_dir: PathBuf,
}

impl LoadReport {
    /// 结束会话后是否应拉起后台生成 encoder 预优化缓存。
    pub fn wants_generation(&self) -> bool {
        self.plan.want_generation
    }

    /// 拉起后台生成缓存（空闲优先级子进程）；失败只返回原因，不影响识别。
    pub fn spawn_generation(&self) -> Result<(), String> {
        optcache::spawn_background(&self.model_dir)
    }

    /// 单行摘要（`--stats` 时打印，对比实验用）。
    pub fn summary(&self) -> String {
        format!(
            "load_plan: encoder={} model_type={} fell_back={} want_generation={}",
            if self.plan.cache_hit {
                "cache"
            } else {
                "original"
            },
            self.plan.model_type.as_deref().unwrap_or("-"),
            self.fell_back,
            self.plan.want_generation,
        )
    }
}

/// 构造流式识别器配置。
///
/// # 参数
/// - `req`：START 请求。
/// - `files`：模型文件（encoder 以 `encoder` 参数为准）。
/// - `encoder`：交给 sherpa 的 encoder 路径（原文件或预优化缓存）。
/// - `model_type`：交给 sherpa 的 `model_type`；`None` 表示让 sherpa 自己探测。
fn online_config(
    req: &StartRequest,
    files: &ModelFiles,
    encoder: &str,
    model_type: Option<&str>,
) -> OnlineRecognizerConfig {
    OnlineRecognizerConfig {
        model_config: OnlineModelConfig {
            transducer: OnlineTransducerModelConfig {
                encoder: Some(encoder.to_string()),
                decoder: Some(files.decoder.clone()),
                joiner: Some(files.joiner.clone()),
            },
            tokens: Some(files.tokens.clone()),
            num_threads: req.threads as i32,
            provider: Some("cpu".into()),
            model_type: model_type.map(str::to_string),
            ..Default::default()
        },
        decoding_method: Some("greedy_search".into()),
        enable_endpoint: true,
        rule1_min_trailing_silence: req.endpoint.rule1_ms as f32 / MS_PER_SECOND,
        rule2_min_trailing_silence: req.endpoint.rule2_ms as f32 / MS_PER_SECOND,
        rule3_min_utterance_length: req.endpoint.rule3_ms as f32 / MS_PER_SECOND,
        ..Default::default()
    }
}

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
    /// 加载提速：`model_type` 交给 sherpa（省掉它自己再载一遍 encoder 读元数据）；命中预优化缓存时用优化后的
    /// encoder。这些加速参数导致创建失败时自动回退到最原始的加载方式。
    ///
    /// # 参数
    /// - `req`：START 请求（模型目录、线程数、端点规则）。
    /// - `opts`：加载选项（生产用 `LoadOpts::default()`，对比实验可单独关闭各项）。
    ///
    /// # 返回
    /// 后端实例与加载报告（用于结束后决定是否后台生成缓存）；模型缺失或加载失败时返回中文原因。
    ///
    /// # 示例
    /// ```ignore
    /// let (backend, report) = SherpaBackend::load(&req, LoadOpts::default())?;
    /// ```
    pub fn load(req: &StartRequest, opts: LoadOpts) -> Result<(Self, LoadReport), String> {
        let dir = Path::new(&req.model_dir);
        let files = discover_model(dir)?;
        let encoder = PathBuf::from(&files.encoder);
        let fingerprint = opts
            .opt_cache
            .then(|| optcache::ort_fingerprint(&optcache::locate_ort_dll()))
            .flatten();
        let mut plan = optcache::plan_encoder(
            dir,
            &encoder,
            fingerprint.as_deref(),
            &ONLINE_TRANSDUCER_TYPES,
            opts,
        );
        let tuned = plan.cache_hit || plan.model_type.is_some();
        let cfg = online_config(
            req,
            &files,
            &plan.encoder.to_string_lossy(),
            plan.model_type.as_deref(),
        );
        let mut fell_back = false;
        let recognizer = match OnlineRecognizer::create(&cfg) {
            Some(r) => r,
            // 加速参数（缓存 / model_type）导致创建失败：缓存标记为失败不再用，回退到最原始的加载方式
            None if tuned => {
                if let (true, Some(fp)) = (plan.cache_hit, fingerprint.as_deref()) {
                    optcache::mark_failed(dir, &encoder, fp, "sherpa 载入预优化 encoder 失败");
                }
                fell_back = true;
                plan = LoadPlan {
                    encoder: encoder.clone(),
                    model_type: None,
                    cache_hit: false,
                    want_generation: false,
                };
                let plain = online_config(req, &files, &files.encoder, None);
                OnlineRecognizer::create(&plain).ok_or_else(|| LOAD_FAILED.to_string())?
            }
            None => return Err(LOAD_FAILED.to_string()),
        };
        let stream = recognizer.create_stream();
        let report = LoadReport {
            plan,
            fell_back,
            model_dir: dir.to_path_buf(),
        };
        Ok((Self { recognizer, stream }, report))
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

#[cfg(test)]
mod tests {
    use super::*;

    /// 假模型文件集合。
    fn files() -> ModelFiles {
        ModelFiles {
            encoder: "M/encoder.int8.onnx".into(),
            decoder: "M/decoder.onnx".into(),
            joiner: "M/joiner.int8.onnx".into(),
            tokens: "M/tokens.txt".into(),
        }
    }

    /// `model_type` 与 encoder 路径按参数写进配置；不给 `model_type` 时保持为空（sherpa 自己探测）。
    #[test]
    fn online_config_carries_encoder_and_model_type() {
        let req = StartRequest::default();
        let f = files();
        let tuned = online_config(&req, &f, "M/.opt/encoder.opt.onnx", Some("zipformer2"));
        let t = &tuned.model_config;
        assert_eq!(t.model_type.as_deref(), Some("zipformer2"));
        assert_eq!(
            t.transducer.encoder.as_deref(),
            Some("M/.opt/encoder.opt.onnx")
        );
        assert_eq!(t.transducer.decoder.as_deref(), Some("M/decoder.onnx"));
        assert_eq!(t.tokens.as_deref(), Some("M/tokens.txt"));
        assert_eq!(t.num_threads, req.threads as i32);
        let plain = online_config(&req, &f, &f.encoder, None);
        assert_eq!(plain.model_config.model_type, None);
        assert_eq!(
            plain.model_config.transducer.encoder.as_deref(),
            Some("M/encoder.int8.onnx")
        );
        assert!(plain.enable_endpoint);
    }

    /// 摘要里能看出用了缓存还是原文件，便于对比实验。
    #[test]
    fn report_summary_shows_plan() {
        let report = LoadReport {
            plan: LoadPlan {
                encoder: PathBuf::from("a"),
                model_type: Some("zipformer2".into()),
                cache_hit: true,
                want_generation: false,
            },
            fell_back: false,
            model_dir: PathBuf::from("M"),
        };
        let s = report.summary();
        assert!(
            s.contains("encoder=cache") && s.contains("model_type=zipformer2"),
            "{s}"
        );
        assert!(!report.wants_generation());
    }
}
