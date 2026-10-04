//! 离线整句识别后端：Silero VAD 切句，每个语音段交给离线识别器整句解码，只产出 `Final`。
//!
//! 阻塞问题的结论：离线单段解码可能耗时数百毫秒，会话主循环是单线程的
//! （收命令、取音频、`feed`+`poll`），在 `poll` 里同步解码会让 STOP/PING 响应延迟、
//! 采集通道积压。所以解码放进后台工作线程：`feed` 只做 VAD（毫秒级）并把切好的段投递出去，
//! `poll` 只收已完成的结果，`finish` 冲刷 VAD 后等待工作线程处理完并回收。
//! `START` 里的端点规则 `req.endpoint` 对离线无意义（切句完全由 VAD 参数决定），这里忽略。

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::thread::JoinHandle;

use sherpa_onnx::{
    OfflineModelConfig, OfflineMoonshineModelConfig, OfflineNemoEncDecCtcModelConfig,
    OfflineParaformerModelConfig, OfflineRecognizer, OfflineRecognizerConfig,
    OfflineSenseVoiceModelConfig, OfflineTransducerModelConfig, OfflineWhisperModelConfig,
    OfflineZipformerCtcModelConfig, SileroVadModelConfig, VadModelConfig, VoiceActivityDetector,
};
use snow_stt_protocol::{
    BackendKind, ModelKind, RecognitionMode, StartRequest, VAD_MODEL_FILE_NAME, VadParams,
};

use crate::backend::{KindFiles, SttBackend, SttEvent, discover};
use crate::sherpa::SAMPLE_RATE;

/// VAD 默认阈值（sherpa 示例常见值，未经评测，需在真实音频上复测）。
pub const DEFAULT_VAD_THRESHOLD: f32 = 0.5;
/// VAD 默认切句静音毫秒数（sherpa 示例常见值，未经评测，需在真实音频上复测）。
pub const DEFAULT_VAD_MIN_SILENCE_MS: u32 = 500;
/// VAD 默认最短语音毫秒数（sherpa 示例常见值，未经评测，需在真实音频上复测）。
pub const DEFAULT_VAD_MIN_SPEECH_MS: u32 = 250;
/// VAD 默认单段最长毫秒数（sherpa 示例常见值，未经评测，需在真实音频上复测）。
pub const DEFAULT_VAD_MAX_SPEECH_MS: u32 = 20_000;
/// Silero VAD 窗口样本数（16kHz 下固定 512）。
const VAD_WINDOW_SIZE: i32 = 512;
/// VAD 推理线程数。
const VAD_THREADS: i32 = 1;
/// VAD 内部缓冲秒数，需大于单段最长时长。
const VAD_BUFFER_SECONDS: f32 = 60.0;
/// 毫秒转秒的除数。
const MS_PER_SECOND: f32 = 1000.0;
/// 每段前补的前导音频毫秒数：Silero 在语音起点之后才触发，不补会吃掉句首 1-2 个词。
const PRE_ROLL_MS: usize = 400;
/// 输入历史保留的秒数，需大于单段最长时长加前导。
const HISTORY_SECONDS: usize = 60;
/// 推理设备。
const PROVIDER_CPU: &str = "cpu";
/// 解码方式。
const DECODING_GREEDY: &str = "greedy_search";
/// SenseVoice 自动识别语言。
const LANG_AUTO: &str = "auto";
/// SenseVoice 支持的单语言码。
const SENSE_VOICE_LANGS: [&str; 5] = ["zh", "en", "ja", "ko", "yue"];
/// Whisper 任务：只做转写，不翻译。
const WHISPER_TASK: &str = "transcribe";
/// Whisper 尾部补零：-1 表示用 sherpa 内置默认（Rust 侧 Default 为 0，与 C 库默认不同）。
const WHISPER_TAIL_PADDINGS: i32 = -1;

/// VAD 切出的一个语音段。
#[derive(Debug, Clone, PartialEq)]
pub struct VadSegment {
    /// 段首在整路输入中的样本序号（从 0 起）。
    pub start: usize,
    /// 段内样本。
    pub samples: Vec<f32>,
}

/// 一个语音段的切句器（VAD）。
pub trait SegmentVad {
    /// 推入一块 16kHz 单声道样本。
    ///
    /// # 参数
    /// - `pcm`：样本。
    fn accept(&mut self, pcm: &[f32]);

    /// 把尚未结束的语音强制切出（输入结束时调用）。
    fn flush(&mut self);

    /// 取出并移除最早的一个语音段；没有时返回 `None`。
    fn pop_segment(&mut self) -> Option<VadSegment>;
}

/// 对整段语音做识别的识别器，在后台线程里使用。
pub trait SegmentRecognizer: Send {
    /// 识别一个语音段。
    ///
    /// # 参数
    /// - `samples`：16kHz 单声道样本。
    ///
    /// # 返回
    /// 识别文本（未规范化，可为空）。
    fn recognize(&mut self, samples: &[f32]) -> String;
}

/// 去掉 `<|...|>` 形式的标签（如 SenseVoice 的 `<|zh|>`）并去首尾空白。
///
/// # 参数
/// - `text`：识别器原始文本。
///
/// # 返回
/// 规范化后的文本。
///
/// # 示例
/// ```ignore
/// assert_eq!(clean_text("<|zh|><|NEUTRAL|> 你好 "), "你好");
/// ```
pub fn clean_text(text: &str) -> String {
    const OPEN: &str = "<|";
    const CLOSE: &str = "|>";
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(start) = rest.find(OPEN) {
        let after = &rest[start + OPEN.len()..];
        match after.find(CLOSE) {
            Some(end) => {
                out.push_str(&rest[..start]);
                rest = &after[end + CLOSE.len()..];
            }
            // 没有配对的结束符：按普通文本保留
            None => break,
        }
    }
    out.push_str(rest);
    out.trim().to_string()
}

/// 校验 START 里 backend/mode/kind 的组合是否自洽。
///
/// # 参数
/// - `req`：START 请求。
///
/// # 返回
/// 自洽时 `Ok`；否则返回中文原因。
pub fn check_mode_kind(req: &StartRequest) -> Result<(), String> {
    let offline_mode = req.mode == RecognitionMode::Offline;
    let offline_kind = req.kind.is_offline();
    match req.backend {
        BackendKind::System if offline_mode || offline_kind => {
            Err("系统语音后端不支持离线模式或离线模型类型".to_string())
        }
        BackendKind::System => Ok(()),
        BackendKind::Local if offline_mode && !offline_kind => Err(format!(
            "离线模式需要离线模型类型（kind=offline-*），当前为 {}",
            req.kind.as_str()
        )),
        BackendKind::Local if !offline_mode && offline_kind => Err(format!(
            "模型类型 {} 只能用于离线模式（mode=offline）",
            req.kind.as_str()
        )),
        BackendKind::Local => Ok(()),
    }
}

/// 按约定找 Silero VAD 模型：先模型目录，再其父目录。
///
/// # 参数
/// - `model_dir`：模型目录。
///
/// # 返回
/// 模型文件路径；都不存在时返回中文原因。
pub fn find_vad_model(model_dir: &Path) -> Result<PathBuf, String> {
    let near = model_dir.join(VAD_MODEL_FILE_NAME);
    if near.is_file() {
        return Ok(near);
    }
    if let Some(parent) = model_dir.parent() {
        let far = parent.join(VAD_MODEL_FILE_NAME);
        if far.is_file() {
            return Ok(far);
        }
    }
    Err(format!(
        "找不到 VAD 模型 {VAD_MODEL_FILE_NAME}（已查找 {} 及其上级目录）",
        model_dir.display()
    ))
}

/// 内置默认 VAD 参数。
pub fn default_vad_params() -> VadParams {
    VadParams {
        threshold: DEFAULT_VAD_THRESHOLD,
        min_silence_ms: DEFAULT_VAD_MIN_SILENCE_MS,
        min_speech_ms: DEFAULT_VAD_MIN_SPEECH_MS,
        max_speech_ms: DEFAULT_VAD_MAX_SPEECH_MS,
    }
}

/// 构造 Silero VAD 配置。
///
/// # 参数
/// - `model`：VAD 模型路径。
/// - `p`：VAD 参数。
pub fn build_vad_config(model: &Path, p: VadParams) -> VadModelConfig {
    let secs = |ms: u32| ms as f32 / MS_PER_SECOND;
    VadModelConfig {
        silero_vad: SileroVadModelConfig {
            model: Some(model.to_string_lossy().into_owned()),
            threshold: p.threshold,
            min_silence_duration: secs(p.min_silence_ms),
            min_speech_duration: secs(p.min_speech_ms),
            window_size: VAD_WINDOW_SIZE,
            max_speech_duration: secs(p.max_speech_ms),
        },
        sample_rate: SAMPLE_RATE,
        num_threads: VAD_THREADS,
        provider: Some(PROVIDER_CPU.into()),
        ..Default::default()
    }
}

/// 把 `zh-en` 这类组合语言提示和 `auto` 视为“未指定”，单语言码原样返回。
fn single_language(lang: &str) -> Option<String> {
    let l = lang.trim().to_ascii_lowercase();
    if l.is_empty() || l == LANG_AUTO || l.contains(['-', '_']) {
        None
    } else {
        Some(l)
    }
}

/// 按模型类型构造离线识别器配置。
///
/// # 参数
/// - `kind`：模型类型（须为离线类型）。
/// - `files`：已挑好的模型文件，须与 `kind` 对应。
/// - `req`：START 请求（线程数、语言、ITN）。
///
/// # 返回
/// 识别器配置；类型与文件不匹配时返回中文原因。
pub fn build_offline_config(
    kind: ModelKind,
    files: &KindFiles,
    req: &StartRequest,
) -> Result<OfflineRecognizerConfig, String> {
    let mut m = OfflineModelConfig {
        num_threads: req.threads as i32,
        provider: Some(PROVIDER_CPU.into()),
        ..Default::default()
    };
    match (kind, files) {
        (ModelKind::OfflineTransducer, KindFiles::Transducer(f)) => {
            m.transducer = OfflineTransducerModelConfig {
                encoder: Some(f.encoder.clone()),
                decoder: Some(f.decoder.clone()),
                joiner: Some(f.joiner.clone()),
            };
            m.tokens = Some(f.tokens.clone());
        }
        (ModelKind::OfflineParaformer, KindFiles::Single { model, tokens }) => {
            m.paraformer = OfflineParaformerModelConfig {
                model: Some(model.clone()),
            };
            m.tokens = Some(tokens.clone());
        }
        (ModelKind::OfflineSenseVoice, KindFiles::Single { model, tokens }) => {
            let language = single_language(&req.language)
                .filter(|l| SENSE_VOICE_LANGS.contains(&l.as_str()))
                .unwrap_or_else(|| LANG_AUTO.to_string());
            m.sense_voice = OfflineSenseVoiceModelConfig {
                model: Some(model.clone()),
                language: Some(language),
                use_itn: req.itn,
            };
            m.tokens = Some(tokens.clone());
        }
        (ModelKind::OfflineZipformerCtc, KindFiles::Single { model, tokens }) => {
            m.zipformer_ctc = OfflineZipformerCtcModelConfig {
                model: Some(model.clone()),
            };
            m.tokens = Some(tokens.clone());
        }
        (ModelKind::OfflineNemoCtc, KindFiles::Single { model, tokens }) => {
            m.nemo_ctc = OfflineNemoEncDecCtcModelConfig {
                model: Some(model.clone()),
            };
            m.tokens = Some(tokens.clone());
        }
        (
            ModelKind::OfflineWhisper,
            KindFiles::Whisper {
                encoder,
                decoder,
                tokens,
            },
        ) => {
            // 空语言表示由模型自动判断
            m.whisper = OfflineWhisperModelConfig {
                encoder: Some(encoder.clone()),
                decoder: Some(decoder.clone()),
                language: Some(single_language(&req.language).unwrap_or_default()),
                task: Some(WHISPER_TASK.into()),
                tail_paddings: WHISPER_TAIL_PADDINGS,
                ..Default::default()
            };
            m.tokens = Some(tokens.clone());
        }
        (
            ModelKind::OfflineMoonshine,
            KindFiles::Moonshine {
                preprocessor,
                encoder,
                uncached_decoder,
                cached_decoder,
                tokens,
            },
        ) => {
            m.moonshine = OfflineMoonshineModelConfig {
                preprocessor: Some(preprocessor.clone()),
                encoder: Some(encoder.clone()),
                uncached_decoder: Some(uncached_decoder.clone()),
                cached_decoder: Some(cached_decoder.clone()),
                ..Default::default()
            };
            m.tokens = Some(tokens.clone());
        }
        _ => {
            return Err(format!("模型类型 {} 与所选文件不匹配", kind.as_str()));
        }
    }
    Ok(OfflineRecognizerConfig {
        model_config: m,
        decoding_method: Some(DECODING_GREEDY.into()),
        ..Default::default()
    })
}

/// 最近输入的历史，用于给语音段补前导音频。
#[derive(Default)]
struct History {
    /// `buf[0]` 对应的样本序号。
    base: usize,
    /// 最近的输入样本。
    buf: Vec<f32>,
}

impl History {
    /// 追加输入；超过保留上限的两倍时丢弃最旧部分。
    fn push(&mut self, pcm: &[f32]) {
        let cap = HISTORY_SECONDS * SAMPLE_RATE as usize;
        self.buf.extend_from_slice(pcm);
        if self.buf.len() > cap * 2 {
            let drop = self.buf.len() - cap;
            self.buf.drain(..drop);
            self.base += drop;
        }
    }

    /// 给语音段补前导音频，返回补好后的样本。
    ///
    /// `floor` 是上一段的结束序号，前导不会越过它，避免相邻段重复同一段音频。
    fn with_pre_roll(&self, seg: VadSegment, floor: usize) -> Vec<f32> {
        let pre = PRE_ROLL_MS * SAMPLE_RATE as usize / 1000;
        let from = seg.start.saturating_sub(pre).max(floor).max(self.base);
        let to = seg.start.saturating_sub(self.base);
        if from >= seg.start || to > self.buf.len() {
            return seg.samples;
        }
        let mut out = self.buf[from - self.base..to].to_vec();
        out.extend_from_slice(&seg.samples);
        out
    }
}

/// sherpa Silero VAD 的切句器实现。
struct SherpaVad {
    /// 底层检测器。
    vad: VoiceActivityDetector,
}

impl SegmentVad for SherpaVad {
    /// 推入样本。
    fn accept(&mut self, pcm: &[f32]) {
        self.vad.accept_waveform(pcm);
    }

    /// 冲刷尾部语音。
    fn flush(&mut self) {
        self.vad.flush();
    }

    /// 复制队首语音段后弹出。
    fn pop_segment(&mut self) -> Option<VadSegment> {
        let seg = {
            let front = self.vad.front()?;
            VadSegment {
                start: front.start().max(0) as usize,
                samples: front.samples().to_vec(),
            }
        };
        self.vad.pop();
        Some(seg)
    }
}

/// sherpa 离线识别器的整段识别实现。
struct SherpaRecognizer {
    /// 底层识别器。
    recognizer: OfflineRecognizer,
}

impl SegmentRecognizer for SherpaRecognizer {
    /// 为该段新建流、解码并取文本。
    fn recognize(&mut self, samples: &[f32]) -> String {
        let stream = self.recognizer.create_stream();
        stream.accept_waveform(SAMPLE_RATE, samples);
        self.recognizer.decode(&stream);
        stream.get_result().map(|r| r.text).unwrap_or_default()
    }
}

/// 离线识别后端：VAD 在调用线程，识别在后台工作线程。
pub struct OfflineSherpaBackend {
    /// 切句器。
    vad: Box<dyn SegmentVad>,
    /// 向工作线程投递语音段；结束后置空以通知其退出。
    seg_tx: Option<Sender<Vec<f32>>>,
    /// 工作线程回传的规范化文本。
    text_rx: Receiver<String>,
    /// 工作线程句柄。
    worker: Option<JoinHandle<()>>,
    /// 最近输入历史，用于补前导音频。
    history: History,
    /// 上一段（含前导后）的结束样本序号。
    prev_end: usize,
    /// 取消标记：置位后工作线程丢弃尚未处理的段。
    cancel: Arc<AtomicBool>,
}

impl OfflineSherpaBackend {
    /// 按请求加载 VAD 与离线识别器。
    ///
    /// # 参数
    /// - `req`：START 请求（须已通过 [`check_mode_kind`]）。
    ///
    /// # 返回
    /// 后端实例；模型缺失或加载失败时返回中文原因。
    pub fn load(req: &StartRequest) -> Result<Self, String> {
        let dir = Path::new(&req.model_dir);
        let files = discover(dir, req.kind)?;
        let vad_model = find_vad_model(dir)?;
        let vad_cfg = build_vad_config(&vad_model, req.vad.unwrap_or_else(default_vad_params));
        let vad = VoiceActivityDetector::create(&vad_cfg, VAD_BUFFER_SECONDS)
            .ok_or_else(|| "sherpa 创建 VAD 失败（silero_vad.onnx 损坏或不兼容）".to_string())?;
        let cfg = build_offline_config(req.kind, &files, req)?;
        let recognizer = OfflineRecognizer::create(&cfg)
            .ok_or_else(|| "sherpa 创建离线识别器失败（模型文件损坏或不兼容）".to_string())?;
        Self::new(
            Box::new(SherpaVad { vad }),
            Box::new(SherpaRecognizer { recognizer }),
        )
    }

    /// 由切句器与识别器组装后端并启动工作线程。
    ///
    /// # 参数
    /// - `vad`：切句器。
    /// - `recognizer`：识别器（移入工作线程）。
    ///
    /// # 返回
    /// 后端实例；线程创建失败时返回中文原因。
    pub fn new(
        vad: Box<dyn SegmentVad>,
        mut recognizer: Box<dyn SegmentRecognizer>,
    ) -> Result<Self, String> {
        let (seg_tx, seg_rx) = channel::<Vec<f32>>();
        let (text_tx, text_rx) = channel::<String>();
        let cancel = Arc::new(AtomicBool::new(false));
        let flag = Arc::clone(&cancel);
        let worker = std::thread::Builder::new()
            .name("stt-offline-worker".into())
            .spawn(move || {
                // 发送端关闭后 recv 返回错误，线程自然退出
                while let Ok(seg) = seg_rx.recv() {
                    if flag.load(Ordering::Relaxed) {
                        continue;
                    }
                    let text = clean_text(&recognizer.recognize(&seg));
                    if text_tx.send(text).is_err() {
                        return;
                    }
                }
            })
            .map_err(|e| format!("无法创建离线识别线程: {e}"))?;
        Ok(Self {
            vad,
            seg_tx: Some(seg_tx),
            text_rx,
            worker: Some(worker),
            history: History::default(),
            prev_end: 0,
            cancel,
        })
    }

    /// 取出 VAD 已切好的全部语音段并投递给工作线程。
    fn dispatch_segments(&mut self) {
        while let Some(seg) = self.vad.pop_segment() {
            let end = seg.start + seg.samples.len();
            let samples = self.history.with_pre_roll(seg, self.prev_end);
            self.prev_end = end;
            if let Some(tx) = &self.seg_tx {
                let _ = tx.send(samples);
            }
        }
    }

    /// 把已完成的非空文本转成 `Final` 事件。
    fn collect_ready(&self) -> Vec<SttEvent> {
        self.text_rx
            .try_iter()
            .filter(|t| !t.is_empty())
            .map(SttEvent::Final)
            .collect()
    }
}

impl SttBackend for OfflineSherpaBackend {
    /// 喂给 VAD 并投递新切出的段（不在此处解码，不会长时间阻塞）。
    fn feed(&mut self, pcm: &[f32]) {
        self.history.push(pcm);
        self.vad.accept(pcm);
        self.dispatch_segments();
    }

    /// 收取工作线程已完成的结果；离线模式不产生 `Partial`。
    fn poll(&mut self) -> Vec<SttEvent> {
        self.collect_ready()
    }

    /// 冲刷 VAD 尾部，等待工作线程处理完所有段并回收，返回剩余结果。
    fn finish(&mut self) -> Vec<SttEvent> {
        self.vad.flush();
        self.dispatch_segments();
        // 关闭发送端让工作线程处理完队列后退出
        self.seg_tx = None;
        let mut events = Vec::new();
        if let Some(handle) = self.worker.take()
            && handle.join().is_err()
        {
            events.push(SttEvent::Failed("离线识别线程异常退出".into()));
        }
        // 线程已结束，其发送端已释放，try_iter 能取尽剩余结果
        events.splice(0..0, self.collect_ready());
        events
    }
}

impl Drop for OfflineSherpaBackend {
    /// 取消未处理的段并回收工作线程，避免泄漏。
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::Relaxed);
        self.seg_tx = None;
        if let Some(handle) = self.worker.take() {
            let _ = handle.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::ModelFiles;
    use std::collections::VecDeque;
    use std::time::Duration;

    /// 假切句器：全零块视为静音并结束当前段，非零块累积进当前段，flush 冲出未结束段。
    #[derive(Default)]
    struct FakeVad {
        /// 正在累积的段。
        cur: Vec<f32>,
        /// 当前段起点序号。
        cur_start: usize,
        /// 已切好的段。
        ready: VecDeque<VadSegment>,
        /// 已收到的样本总数。
        total: usize,
    }

    impl FakeVad {
        /// 结束当前段。
        fn close(&mut self) {
            if !self.cur.is_empty() {
                let samples = std::mem::take(&mut self.cur);
                self.ready.push_back(VadSegment {
                    start: self.cur_start,
                    samples,
                });
            }
        }
    }

    impl SegmentVad for FakeVad {
        fn accept(&mut self, pcm: &[f32]) {
            if pcm.iter().all(|&s| s == 0.0) {
                self.close();
            } else {
                if self.cur.is_empty() {
                    self.cur_start = self.total;
                }
                self.cur.extend_from_slice(pcm);
            }
            self.total += pcm.len();
        }
        fn flush(&mut self) {
            self.close();
        }
        fn pop_segment(&mut self) -> Option<VadSegment> {
            self.ready.pop_front()
        }
    }

    /// 假识别器：含负样本则返回空文本，否则按非零样本数给带标签文本；可模拟慢解码。
    struct FakeRecognizer {
        /// 每段额外睡眠。
        delay: Duration,
    }

    impl SegmentRecognizer for FakeRecognizer {
        fn recognize(&mut self, samples: &[f32]) -> String {
            std::thread::sleep(self.delay);
            // 前导静音为 0，只数非零样本；含负样本视为无内容
            if samples.iter().any(|&v| v < 0.0) {
                String::new()
            } else {
                let n = samples.iter().filter(|&&v| v != 0.0).count();
                format!(" <|zh|>seg{n} ")
            }
        }
    }

    /// 构造假后端。
    fn backend(delay_ms: u64) -> OfflineSherpaBackend {
        OfflineSherpaBackend::new(
            Box::new(FakeVad::default()),
            Box::new(FakeRecognizer {
                delay: Duration::from_millis(delay_ms),
            }),
        )
        .unwrap()
    }

    /// 取 Final 文本列表。
    fn finals(events: &[SttEvent]) -> Vec<String> {
        events
            .iter()
            .filter_map(|e| match e {
                SttEvent::Final(t) => Some(t.clone()),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn clean_text_strips_tags_and_spaces() {
        assert_eq!(
            clean_text("<|zh|><|NEUTRAL|><|Speech|> 你好 世界 "),
            "你好 世界"
        );
        assert_eq!(clean_text("a<|x|>b<|y|>c"), "abc");
        assert_eq!(clean_text("  plain  "), "plain");
        assert_eq!(clean_text("<|zh|>"), "");
        // 未闭合的标签原样保留
        assert_eq!(clean_text("a <| b"), "a <| b");
        assert_eq!(clean_text(""), "");
    }

    #[test]
    fn multiple_segments_emit_in_order_without_partial() {
        let mut b = backend(0);
        b.feed(&[0.5; 100]);
        b.feed(&[0.0; 10]);
        b.feed(&[0.5; 200]);
        b.feed(&[0.0; 10]);
        let mut all = Vec::new();
        // 工作线程异步完成，轮询等待
        for _ in 0..200 {
            all.extend(b.poll());
            if all.len() == 2 {
                break;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        all.extend(b.finish());
        assert_eq!(finals(&all), vec!["seg100", "seg200"]);
        assert!(all.iter().all(|e| matches!(e, SttEvent::Final(_))));
    }

    #[test]
    fn empty_text_is_not_emitted() {
        let mut b = backend(0);
        b.feed(&[-0.5; 50]);
        b.feed(&[0.0; 10]);
        b.feed(&[0.5; 60]);
        let events = b.finish();
        assert_eq!(finals(&events), vec!["seg60"]);
    }

    #[test]
    fn finish_flushes_pending_speech_and_waits_for_worker() {
        // 慢解码：finish 必须等到全部结果
        let mut b = backend(60);
        b.feed(&[0.5; 30]);
        b.feed(&[0.0; 10]);
        b.feed(&[0.5; 40]);
        let events = b.finish();
        assert_eq!(finals(&events), vec!["seg30", "seg40"]);
        assert!(b.worker.is_none());
        assert!(b.poll().is_empty());
    }

    #[test]
    fn feed_does_not_block_on_slow_decode() {
        let mut b = backend(300);
        b.feed(&[0.5; 30]);
        let t = std::time::Instant::now();
        b.feed(&[0.0; 10]);
        assert!(t.elapsed() < Duration::from_millis(150));
        drop(b);
    }

    #[test]
    fn drop_cancels_pending_work_and_joins() {
        let mut b = backend(200);
        for _ in 0..5 {
            b.feed(&[0.5; 20]);
            b.feed(&[0.0; 10]);
        }
        let t = std::time::Instant::now();
        drop(b);
        // 5 段 * 200ms = 1s；取消后最多只等当前这一段
        assert!(
            t.elapsed() < Duration::from_millis(700),
            "{:?}",
            t.elapsed()
        );
    }

    #[test]
    fn pre_roll_prepends_history_without_overlapping_previous_segment() {
        let pre = PRE_ROLL_MS * SAMPLE_RATE as usize / 1000;
        let mut h = History::default();
        let input: Vec<f32> = (0..pre * 3).map(|i| i as f32).collect();
        h.push(&input);
        // 起点足够靠后：补满前导
        let start = pre * 2;
        let seg = VadSegment {
            start,
            samples: vec![-1.0; 10],
        };
        let out = h.with_pre_roll(seg.clone(), 0);
        assert_eq!(out.len(), pre + 10);
        assert_eq!(out[0], (start - pre) as f32);
        // 受上一段结束位置限制
        let out = h.with_pre_roll(seg.clone(), start - 5);
        assert_eq!(out.len(), 5 + 10);
        // 起点靠前：只补到输入开头
        let early = VadSegment {
            start: 7,
            samples: vec![-1.0; 3],
        };
        assert_eq!(h.with_pre_roll(early, 0).len(), 7 + 3);
        // 无前导可补
        let out = h.with_pre_roll(seg, start);
        assert_eq!(out.len(), 10);
    }

    #[test]
    fn history_drops_old_samples_but_keeps_indices() {
        let cap = HISTORY_SECONDS * SAMPLE_RATE as usize;
        let mut h = History::default();
        h.push(&vec![0.0; cap * 2 + 1]);
        assert_eq!(h.base + h.buf.len(), cap * 2 + 1);
        assert_eq!(h.buf.len(), cap);
    }

    #[test]
    fn backend_adds_pre_roll_to_dispatched_segment() {
        /// 记录收到样本长度的识别器。
        struct Len;
        impl SegmentRecognizer for Len {
            fn recognize(&mut self, s: &[f32]) -> String {
                format!("n{}", s.len())
            }
        }
        let pre = PRE_ROLL_MS * SAMPLE_RATE as usize / 1000;
        let mut b = OfflineSherpaBackend::new(Box::new(FakeVad::default()), Box::new(Len)).unwrap();
        // 前面 2*pre 的“静音”（非全零但被假 VAD 当作段的前导：用全零块表示）
        b.feed(&vec![0.0; pre * 2]);
        b.feed(&[0.5; 100]);
        b.feed(&[0.0; 10]);
        let texts = finals(&b.finish());
        assert_eq!(texts, vec![format!("n{}", pre + 100)]);
    }

    /// 构造一个 START 请求。
    fn req(kind: ModelKind, mode: RecognitionMode, backend: BackendKind) -> StartRequest {
        StartRequest {
            kind,
            mode,
            backend,
            ..Default::default()
        }
    }

    #[test]
    fn mode_kind_validation() {
        use BackendKind::{Local, System};
        use ModelKind::*;
        use RecognitionMode::{Offline, Streaming};
        assert!(check_mode_kind(&req(OnlineTransducer, Streaming, Local)).is_ok());
        assert!(check_mode_kind(&req(OfflineParaformer, Offline, Local)).is_ok());
        assert!(check_mode_kind(&req(OnlineTransducer, Offline, Local)).is_err());
        assert!(check_mode_kind(&req(OfflineSenseVoice, Streaming, Local)).is_err());
        assert!(check_mode_kind(&req(OnlineTransducer, Streaming, System)).is_ok());
        let e = check_mode_kind(&req(OnlineTransducer, Offline, System)).unwrap_err();
        assert!(e.contains("系统语音"), "{e}");
        assert!(check_mode_kind(&req(OfflineWhisper, Streaming, System)).is_err());
    }

    #[test]
    fn vad_defaults_and_config_mapping() {
        let d = default_vad_params();
        assert_eq!(
            (d.min_silence_ms, d.min_speech_ms, d.max_speech_ms),
            (500, 250, 20_000)
        );
        let c = build_vad_config(Path::new("M/silero_vad.onnx"), d);
        assert_eq!(c.silero_vad.threshold, 0.5);
        assert_eq!(c.silero_vad.min_silence_duration, 0.5);
        assert_eq!(c.silero_vad.min_speech_duration, 0.25);
        assert_eq!(c.silero_vad.max_speech_duration, 20.0);
        assert_eq!(c.silero_vad.window_size, 512);
        assert_eq!(c.sample_rate, 16_000);
        assert_eq!(c.num_threads, 1);
        assert!(c.silero_vad.model.unwrap().ends_with("silero_vad.onnx"));
    }

    #[test]
    fn vad_model_found_in_dir_then_parent() {
        let root = std::env::temp_dir().join(format!("snow-stt-vad-{}", std::process::id()));
        let model = root.join("model");
        std::fs::create_dir_all(&model).unwrap();
        let e = find_vad_model(&model).unwrap_err();
        assert!(e.contains(VAD_MODEL_FILE_NAME), "{e}");
        std::fs::write(root.join(VAD_MODEL_FILE_NAME), b"x").unwrap();
        assert_eq!(
            find_vad_model(&model).unwrap(),
            root.join(VAD_MODEL_FILE_NAME)
        );
        std::fs::write(model.join(VAD_MODEL_FILE_NAME), b"x").unwrap();
        assert_eq!(
            find_vad_model(&model).unwrap(),
            model.join(VAD_MODEL_FILE_NAME)
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    /// 单文件模型的假文件集合。
    fn single() -> KindFiles {
        KindFiles::Single {
            model: "m.onnx".into(),
            tokens: "t.txt".into(),
        }
    }

    #[test]
    fn config_fields_per_kind() {
        let mut r = StartRequest {
            threads: 3,
            language: "zh".into(),
            itn: true,
            ..Default::default()
        };
        let c = build_offline_config(ModelKind::OfflineParaformer, &single(), &r).unwrap();
        let m = &c.model_config;
        assert_eq!(m.paraformer.model.as_deref(), Some("m.onnx"));
        assert_eq!(m.tokens.as_deref(), Some("t.txt"));
        assert_eq!(m.num_threads, 3);
        assert_eq!(c.decoding_method.as_deref(), Some("greedy_search"));

        let c = build_offline_config(ModelKind::OfflineSenseVoice, &single(), &r).unwrap();
        let sv = &c.model_config.sense_voice;
        assert_eq!(sv.model.as_deref(), Some("m.onnx"));
        assert_eq!(sv.language.as_deref(), Some("zh"));
        assert!(sv.use_itn);
        // 组合语言提示与不支持的语言都回落到 auto
        for lang in ["zh-en", "auto", "fr"] {
            r.language = lang.into();
            let c = build_offline_config(ModelKind::OfflineSenseVoice, &single(), &r).unwrap();
            assert_eq!(c.model_config.sense_voice.language.as_deref(), Some("auto"));
        }

        let c = build_offline_config(ModelKind::OfflineZipformerCtc, &single(), &r).unwrap();
        assert_eq!(
            c.model_config.zipformer_ctc.model.as_deref(),
            Some("m.onnx")
        );
        let c = build_offline_config(ModelKind::OfflineNemoCtc, &single(), &r).unwrap();
        assert_eq!(c.model_config.nemo_ctc.model.as_deref(), Some("m.onnx"));

        let t = KindFiles::Transducer(ModelFiles {
            encoder: "e".into(),
            decoder: "d".into(),
            joiner: "j".into(),
            tokens: "t".into(),
        });
        let c = build_offline_config(ModelKind::OfflineTransducer, &t, &r).unwrap();
        let tr = &c.model_config.transducer;
        assert_eq!(
            (
                tr.encoder.as_deref(),
                tr.decoder.as_deref(),
                tr.joiner.as_deref()
            ),
            (Some("e"), Some("d"), Some("j"))
        );
        assert_eq!(c.model_config.tokens.as_deref(), Some("t"));

        let w = KindFiles::Whisper {
            encoder: "e".into(),
            decoder: "d".into(),
            tokens: "t".into(),
        };
        r.language = "en".into();
        let c = build_offline_config(ModelKind::OfflineWhisper, &w, &r).unwrap();
        let wh = &c.model_config.whisper;
        assert_eq!(wh.language.as_deref(), Some("en"));
        assert_eq!(wh.task.as_deref(), Some("transcribe"));
        r.language = "auto".into();
        let c = build_offline_config(ModelKind::OfflineWhisper, &w, &r).unwrap();
        assert_eq!(c.model_config.whisper.language.as_deref(), Some(""));

        let ms = KindFiles::Moonshine {
            preprocessor: "p".into(),
            encoder: "e".into(),
            uncached_decoder: "u".into(),
            cached_decoder: "c".into(),
            tokens: "t".into(),
        };
        let c = build_offline_config(ModelKind::OfflineMoonshine, &ms, &r).unwrap();
        let mo = &c.model_config.moonshine;
        assert_eq!(mo.preprocessor.as_deref(), Some("p"));
        assert_eq!(mo.uncached_decoder.as_deref(), Some("u"));
        assert_eq!(mo.cached_decoder.as_deref(), Some("c"));
        assert_eq!(c.model_config.tokens.as_deref(), Some("t"));
    }

    #[test]
    fn config_rejects_mismatched_files() {
        let r = StartRequest::default();
        assert!(build_offline_config(ModelKind::OfflineWhisper, &single(), &r).is_err());
        assert!(build_offline_config(ModelKind::OnlineTransducer, &single(), &r).is_err());
    }

    /// 真实模型冒烟：读环境变量 `SNOW_STT_TEST_MODEL_DIR`、`SNOW_STT_TEST_KIND`、`SNOW_STT_TEST_WAV`。
    #[test]
    #[ignore = "需要本机模型与 silero_vad.onnx"]
    fn real_model_smoke() {
        let dir = std::env::var("SNOW_STT_TEST_MODEL_DIR").expect("缺 SNOW_STT_TEST_MODEL_DIR");
        let kind = std::env::var("SNOW_STT_TEST_KIND").expect("缺 SNOW_STT_TEST_KIND");
        let wav = std::env::var("SNOW_STT_TEST_WAV").expect("缺 SNOW_STT_TEST_WAV");
        let req = StartRequest {
            mode: RecognitionMode::Offline,
            kind: ModelKind::parse(&kind).expect("kind 无效"),
            model_dir: dir,
            threads: 2,
            ..Default::default()
        };
        let mut b = OfflineSherpaBackend::load(&req).unwrap();
        let wave = sherpa_onnx::Wave::read(&wav).expect("读 wav 失败");
        b.feed(wave.samples());
        b.feed(&vec![0.0; SAMPLE_RATE as usize]);
        let texts = finals(&b.finish());
        println!("{texts:?}");
        assert!(!texts.is_empty());
    }
}
