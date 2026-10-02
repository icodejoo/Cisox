//! 对话式 decoder-only 翻译引擎（`hunyuan_chat`，Hy-MT2）。
//!
//! 单个 ONNX 会话，预填充与逐 token 解码共用同一张图：输入 `input_ids`、`attention_mask`、`position_ids`
//! 与每层 `past_key_values.{层}.{key|value}`（形状 `[1, KV 头, 序列, 头维]`，首轮序列维为 0），
//! 输出 `logits` 与 `present.*`。提示词按清单 `prompt` 拼装；只做贪心 + repetition_penalty，
//! 与 `eval/hymt/ort_gen.py` 同一逻辑；`num_beams` 对本族无效（忽略，不报错）。
//!
//! 低内存：权重由 ORT 对外部数据文件内存映射，Rust 侧不拷贝；解码循环复用同一块 logits 缓冲，
//! KV 张量直接从输出搬回输入，不重新分配；重复惩罚在 argmax 里就地折算，不复制 logits。

use std::borrow::Cow;
use std::fmt;
use std::path::Path;

use ort::session::{Session, SessionInputValue};
use ort::value::{DynValue, Tensor, ValueType};
use tokenizers::Tokenizer;

use crate::engine::{
    EngineError, TranslateOptions, build_session, classify_error, init_runtime, json_i64,
    load_tokenizer, read_json_opt,
};
use crate::manifest::{
    FILE_MODEL, FILE_TOKENIZER, HUNYUAN_DEFAULT_REPETITION_PENALTY, Manifest, PromptSpec,
};
use crate::protocol::ErrorKind;
use crate::text::{Segment, join_translated, split_sentences};

/// 缺省最大新生成 token 数。
const DEFAULT_MAX_NEW_TOKENS: usize = 512;
/// 模型输入名：词 id。
const INPUT_IDS: &str = "input_ids";
/// 模型输入名：注意力掩码。
const INPUT_MASK: &str = "attention_mask";
/// 模型输入名：位置 id。
const INPUT_POSITIONS: &str = "position_ids";
/// 模型输出名：词表 logits。
const OUTPUT_LOGITS: &str = "logits";
/// KV 输入名前缀。
const PAST_PREFIX: &str = "past_key_values.";
/// KV 输出名前缀。
const PRESENT_PREFIX: &str = "present.";
/// 分句后重新打包时，两段之间的空白分隔（拼接成一个请求）。
const SEGMENT_JOINER: &str = "";

/// 贪心解码参数。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct DecodeParams {
    /// 结束 token。
    pub eos: u32,
    /// 最多生成的 token 数。
    pub max_new: usize,
    /// 重复惩罚系数，1.0 关闭。
    pub repetition_penalty: f32,
}

/// 逐步产出“最后一个位置 logits”的模型抽象；真实实现是 ORT 会话，测试里用脚本化假实现。
pub trait LogitsSource {
    /// 喂入新 token，返回最后一个位置的 logits。
    ///
    /// # 参数
    /// - `ids`：本步新增的 token（预填充时是整段提示词，之后每步 1 个）。
    /// - `past_len`：此前已经喂入的 token 总数；为 0 表示新一轮，实现必须清空 KV。
    ///
    /// # 返回
    /// 词表大小的 logits 切片（借用自实现内部缓冲）。
    fn forward(&mut self, ids: &[u32], past_len: usize) -> Result<&[f32], EngineError>;
}

/// 带重复惩罚的 argmax，不复制 logits：已出现过的 token，正 logit 除以系数、负 logit 乘以系数
/// （与 HF `RepetitionPenaltyLogitsProcessor` 同公式）。并列取下标小者；NaN 不参与。
///
/// # 参数
/// - `logits`：词表 logits。
/// - `seen`：按 token id 下标标记“提示词或已生成过”，可比 `logits` 短（越界视为未出现）。
/// - `penalty`：惩罚系数，1.0 等价于普通 argmax。
///
/// # 返回
/// 选中的 token id；全是 NaN 或空切片返回 `None`。
///
/// # 示例
/// ```ignore
/// let seen = [false, true];
/// assert_eq!(penalized_argmax(&[1.0, 1.04], &seen, 1.05), Some(0));
/// ```
pub fn penalized_argmax(logits: &[f32], seen: &[bool], penalty: f32) -> Option<u32> {
    let mut best: Option<(usize, f32)> = None;
    for (i, &raw) in logits.iter().enumerate() {
        let v = if penalty != 1.0 && seen.get(i).copied().unwrap_or(false) {
            if raw < 0.0 {
                raw * penalty
            } else {
                raw / penalty
            }
        } else {
            raw
        };
        if v.is_nan() {
            continue;
        }
        if best.is_none_or(|(_, b)| v > b) {
            best = Some((i, v));
        }
    }
    best.map(|(i, _)| i as u32)
}

/// 贪心生成：预填充提示词后逐 token 解码，遇到 eos 或达到上限停止。
///
/// # 参数
/// - `model`：产出 logits 的模型。
/// - `prompt`：提示词 token（非空）。
/// - `params`：结束 token、上限与重复惩罚。
///
/// # 返回
/// 新生成的 token（不含 eos）。
///
/// # 示例
/// ```ignore
/// let out = greedy_decode(&mut session, &prompt_ids, &params)?;
/// ```
pub fn greedy_decode(
    model: &mut impl LogitsSource,
    prompt: &[u32],
    params: &DecodeParams,
) -> Result<Vec<u32>, EngineError> {
    let mut out = Vec::new();
    if prompt.is_empty() || params.max_new == 0 {
        return Ok(out);
    }
    let mut seen: Vec<bool> = Vec::new();
    let mut past_len = 0usize;
    let mut feed: Vec<u32> = prompt.to_vec();
    let mut pending: Vec<u32> = prompt.to_vec();
    for _ in 0..params.max_new {
        let logits = model.forward(&feed, past_len)?;
        if seen.len() < logits.len() {
            seen.resize(logits.len(), false);
            for &id in &pending {
                mark_seen(&mut seen, id);
            }
        }
        past_len += feed.len();
        let next = penalized_argmax(logits, &seen, params.repetition_penalty).ok_or_else(|| {
            EngineError::new(ErrorKind::DecodeFailed, "no selectable token in logits")
        })?;
        if next == params.eos {
            break;
        }
        out.push(next);
        mark_seen(&mut seen, next);
        feed.clear();
        feed.push(next);
        // 只有首轮需要补记提示词的 seen，之后清空即可
        pending.clear();
    }
    Ok(out)
}

/// 标记 token 已出现（越界忽略）。
fn mark_seen(seen: &mut [bool], id: u32) {
    if let Some(slot) = seen.get_mut(id as usize) {
        *slot = true;
    }
}

/// 去掉原文里冒充特殊符号的片段，防止用户文本里的 `<｜hy_Assistant｜>` 之类被分词器当成控制符。
///
/// # 参数
/// - `text`：原文。
/// - `specials`：分词器全部特殊 token 的字面文本。
///
/// # 返回
/// 清洗后的文本；删除后拼出新特殊串的情形会继续删，直到稳定。
///
/// # 示例
/// ```ignore
/// assert_eq!(strip_specials("a<s>b", &["<s>".into()]), "ab");
/// ```
pub fn strip_specials(text: &str, specials: &[String]) -> String {
    let mut cur = Cow::Borrowed(text);
    loop {
        let before = cur.len();
        for s in specials.iter().filter(|s| !s.is_empty()) {
            if cur.contains(s.as_str()) {
                cur = Cow::Owned(cur.replace(s.as_str(), ""));
            }
        }
        if cur.len() == before {
            return cur.into_owned();
        }
    }
}

/// 把分句结果按 token 上限贪心打包成若干组（每组是连续句子，保留原分隔符）。
///
/// # 参数
/// - `segments`：分句结果。
/// - `limit`：每组最多 token 数（单句超限时独占一组，不再硬切）。
/// - `count`：统计文本 token 数的函数。
///
/// # 返回
/// `(组内文本, 组后分隔符)` 列表。
///
/// # 示例
/// ```ignore
/// let groups = pack_segments(&split_sentences(text), 400, |t| t.len() / 3);
/// ```
pub fn pack_segments(
    segments: &[Segment],
    limit: usize,
    count: impl Fn(&str) -> usize,
) -> Vec<(String, String)> {
    let mut groups: Vec<(String, String)> = Vec::new();
    let mut cur = String::new();
    let mut cur_sep = String::new();
    for seg in segments {
        let merged = format!("{cur}{cur_sep}{SEGMENT_JOINER}{}", seg.text);
        if !cur.is_empty() && count(&merged) > limit {
            groups.push((std::mem::take(&mut cur), std::mem::take(&mut cur_sep)));
            cur = seg.text.clone();
        } else {
            cur = merged;
        }
        cur_sep = seg.sep.clone();
    }
    if !cur.is_empty() {
        groups.push((cur, cur_sep));
    }
    groups
}

/// ORT 会话封装：持有 KV cache 与 logits 缓冲，实现 [`LogitsSource`]。
struct ChatSession {
    /// 解码会话（单图，预填充与逐 token 共用）。
    session: Session,
    /// `past_key_values.*` 输入名（保持模型顺序）。
    past_names: Vec<String>,
    /// 与 `past_names` 一一对应的 `present.*` 输出名。
    present_names: Vec<String>,
    /// 当前 KV（与 `past_names` 同序）。
    past: Vec<DynValue>,
    /// KV 头数。
    kv_heads: usize,
    /// KV 每头维度。
    kv_head_dim: usize,
    /// 模型是否声明了 `position_ids` 输入。
    takes_positions: bool,
    /// 最后位置 logits 的复用缓冲。
    logits: Vec<f32>,
}

impl ChatSession {
    /// 把 KV 重置为序列维 0 的空张量（新一轮生成）。
    fn reset_past(&mut self) -> Result<(), EngineError> {
        let fail = |e: &dyn fmt::Display| classify_error(&e.to_string(), ErrorKind::DecodeFailed);
        self.past.clear();
        for _ in &self.past_names {
            let empty = Tensor::<f32>::from_array((
                [1usize, self.kv_heads, 0, self.kv_head_dim],
                Vec::<f32>::new(),
            ))
            .map_err(|e| fail(&e))?;
            self.past.push(empty.into_dyn());
        }
        Ok(())
    }
}

impl LogitsSource for ChatSession {
    /// 跑一步：喂新 token，回传 KV，返回最后位置 logits。
    fn forward(&mut self, ids: &[u32], past_len: usize) -> Result<&[f32], EngineError> {
        let fail = |e: &dyn fmt::Display| classify_error(&e.to_string(), ErrorKind::DecodeFailed);
        if past_len == 0 {
            self.reset_past()?;
        }
        let n = ids.len();
        let ids_i64: Vec<i64> = ids.iter().map(|&x| i64::from(x)).collect();
        let mut inputs: Vec<(Cow<'_, str>, SessionInputValue<'_>)> = vec![
            (
                INPUT_IDS.into(),
                Tensor::from_array(([1usize, n], ids_i64))
                    .map_err(|e| fail(&e))?
                    .into(),
            ),
            (
                INPUT_MASK.into(),
                Tensor::from_array(([1usize, past_len + n], vec![1i64; past_len + n]))
                    .map_err(|e| fail(&e))?
                    .into(),
            ),
        ];
        if self.takes_positions {
            let pos: Vec<i64> = (past_len..past_len + n).map(|p| p as i64).collect();
            inputs.push((
                INPUT_POSITIONS.into(),
                Tensor::from_array(([1usize, n], pos))
                    .map_err(|e| fail(&e))?
                    .into(),
            ));
        }
        for (name, value) in self.past_names.iter().zip(self.past.iter()) {
            inputs.push((name.as_str().into(), value.into()));
        }
        let mut outputs = self.session.run(inputs).map_err(|e| fail(&e))?;
        {
            let (shape, data) = outputs[OUTPUT_LOGITS]
                .try_extract_tensor::<f32>()
                .map_err(|e| fail(&e))?;
            let vocab = shape.last().copied().unwrap_or(0).max(0) as usize;
            if vocab == 0 || data.len() < vocab {
                return Err(EngineError::new(
                    ErrorKind::DecodeFailed,
                    "model returned empty logits",
                ));
            }
            self.logits.clear();
            self.logits.extend_from_slice(&data[data.len() - vocab..]);
        }
        for (present, slot) in self.present_names.iter().zip(self.past.iter_mut()) {
            if let Some(v) = outputs.remove(present.as_str()) {
                *slot = v;
            }
        }
        Ok(&self.logits)
    }
}

/// 已加载的对话式翻译引擎（绑定一个目标语言）。
pub struct ChatEngine {
    /// 带 KV 的会话。
    model: ChatSession,
    /// 分词器。
    tokenizer: Tokenizer,
    /// 提示词描述。
    prompt: PromptSpec,
    /// 目标语言码（应用侧写法）。
    tgt: String,
    /// 分词器全部特殊 token 的字面文本（用于清洗原文）。
    specials: Vec<String>,
    /// 解码参数缺省值。
    params: DecodeParams,
    /// 单次请求最大输入 token 数（只统计原文，不含提示词模板）。
    max_input_tokens: usize,
    /// 目标语言是否为 CJK。
    cjk_target: bool,
    /// 模型 ID。
    model_id: String,
}

impl ChatEngine {
    /// 加载对话式模型：校验清单与校验和，建会话（外部数据自动内存映射），探测 KV 形状，做分词自检。
    ///
    /// # 参数
    /// - `manifest`：已通过校验的清单（`family == hunyuan_chat`）。
    /// - `dir`：模型目录。
    /// - `tgt`：目标语言码。
    ///
    /// # 返回
    /// 就绪的引擎；目标语言没有提示词语言名返回 `UnsupportedPair`，其余加载问题返回 `LoadFailed`/`ManifestInvalid`。
    ///
    /// # 示例
    /// ```ignore
    /// let engine = ChatEngine::load(&manifest, dir, "zh-CN")?;
    /// ```
    pub fn load(manifest: &Manifest, dir: &Path, tgt: &str) -> Result<Self, EngineError> {
        let prompt = manifest.prompt.clone().ok_or_else(|| {
            EngineError::new(
                ErrorKind::ManifestInvalid,
                "family hunyuan_chat requires `prompt`",
            )
        })?;
        prompt
            .render("", tgt)
            .map_err(|m| EngineError::new(ErrorKind::UnsupportedPair, m))?;
        manifest.verify_checksums(dir)?;
        init_runtime()?;
        let params = resolve_decode_params(manifest, dir)?;
        let tokenizer = load_tokenizer(&manifest.resolve_file(dir, FILE_TOKENIZER)?)?;
        let session = build_session(
            &manifest.resolve_file(dir, FILE_MODEL)?,
            &manifest.execution,
        )?;

        let input_names: Vec<&str> = session.inputs().iter().map(|o| o.name()).collect();
        for required in [INPUT_IDS, INPUT_MASK] {
            if !input_names.contains(&required) {
                return Err(EngineError::new(
                    ErrorKind::LoadFailed,
                    format!("model has no `{required}` input"),
                ));
            }
        }
        let takes_positions = input_names.contains(&INPUT_POSITIONS);
        let past_names: Vec<String> = input_names
            .iter()
            .filter(|n| n.starts_with(PAST_PREFIX))
            .map(|n| n.to_string())
            .collect();
        let present_names: Vec<String> = past_names
            .iter()
            .map(|n| n.replacen(PAST_PREFIX, PRESENT_PREFIX, 1))
            .collect();
        let dims = session
            .inputs()
            .iter()
            .find(|o| o.name().starts_with(PAST_PREFIX))
            .and_then(|o| match o.dtype() {
                ValueType::Tensor { shape, .. } if shape.len() == 4 => Some((shape[1], shape[3])),
                _ => None,
            })
            .filter(|&(h, d)| h > 0 && d > 0);
        let Some((kv_heads, kv_head_dim)) = dims else {
            return Err(EngineError::new(
                ErrorKind::LoadFailed,
                "cannot infer KV cache dimensions from model inputs (a model exported with past is required)",
            ));
        };
        let specials = special_token_texts(&tokenizer);
        let engine = Self {
            model: ChatSession {
                session,
                past: Vec::with_capacity(past_names.len()),
                past_names,
                present_names,
                kv_heads: kv_heads as usize,
                kv_head_dim: kv_head_dim as usize,
                takes_positions,
                logits: Vec::new(),
            },
            tokenizer,
            prompt,
            tgt: tgt.to_string(),
            specials,
            params,
            max_input_tokens: manifest.max_input_tokens,
            cjk_target: crate::text::is_cjk_lang(tgt),
            model_id: manifest.id.clone(),
        };
        engine.encode_prompt("Hello world")?;
        Ok(engine)
    }

    /// 模型 ID。
    ///
    /// # 返回
    /// 清单中的 `id`。
    ///
    /// # 示例
    /// ```ignore
    /// println!("{}", engine.model_id());
    /// ```
    pub fn model_id(&self) -> &str {
        &self.model_id
    }

    /// 拼提示词并分词（原文先清洗特殊符号）。
    fn encode_prompt(&self, text: &str) -> Result<Vec<u32>, EngineError> {
        let clean = strip_specials(text, &self.specials);
        let prompt = self
            .prompt
            .render(&clean, &self.tgt)
            .map_err(|m| EngineError::new(ErrorKind::UnsupportedPair, m))?;
        let enc = self.tokenizer.encode(prompt, false).map_err(|e| {
            EngineError::new(ErrorKind::DecodeFailed, format!("tokenize failed: {e}"))
        })?;
        Ok(enc.get_ids().to_vec())
    }

    /// 统计原文 token 数（不含提示词模板）。
    fn count_tokens(&self, text: &str) -> usize {
        self.tokenizer
            .encode(text, false)
            .map(|e| e.get_ids().len())
            .unwrap_or(usize::MAX)
    }

    /// 翻译一段文本：整段一次提示（与评测一致）；原文超过输入上限时才分句打包成多次请求。
    ///
    /// # 参数
    /// - `text`：原文。
    /// - `opts`：`max_len` 覆盖最大新 token 数；`num_beams` 对本族无效（忽略）。
    ///
    /// # 返回
    /// 去首尾空白的译文；空白文本原样返回。
    ///
    /// # 示例
    /// ```ignore
    /// let zh = engine.translate("Hello world.", &TranslateOptions::default())?;
    /// ```
    pub fn translate(
        &mut self,
        text: &str,
        opts: &TranslateOptions,
    ) -> Result<String, EngineError> {
        if text.trim().is_empty() {
            return Ok(text.to_string());
        }
        let max_new = opts.max_len.unwrap_or(self.params.max_new).max(1);
        let whole = text.trim();
        if self.count_tokens(whole) <= self.max_input_tokens {
            return self.translate_one(whole, max_new);
        }
        let segments = split_sentences(text);
        let limit = self.max_input_tokens;
        let groups = pack_segments(&segments, limit, |t| self.count_tokens(t));
        let mut parts = Vec::with_capacity(groups.len());
        for (group, sep) in groups {
            parts.push((self.translate_one(&group, max_new)?, sep));
        }
        Ok(join_translated(&parts, self.cjk_target))
    }

    /// 翻译一个请求：编码、贪心解码、还原文本并去首尾空白。
    fn translate_one(&mut self, text: &str, max_new: usize) -> Result<String, EngineError> {
        let prompt = self.encode_prompt(text)?;
        let params = DecodeParams {
            max_new,
            ..self.params
        };
        let out = greedy_decode(&mut self.model, &prompt, &params)?;
        let decoded = self.tokenizer.decode(&out, true).map_err(|e| {
            EngineError::new(ErrorKind::DecodeFailed, format!("detokenize failed: {e}"))
        })?;
        Ok(decoded.trim().to_string())
    }
}

/// 取分词器全部特殊 token 的字面文本。
fn special_token_texts(tokenizer: &Tokenizer) -> Vec<String> {
    tokenizer
        .get_added_tokens_decoder()
        .values()
        .filter(|t| t.special)
        .map(|t| t.content.clone())
        .collect()
}

/// 汇总解码参数：清单 > generation_config.json > config.json；缺 eos 返回 `ManifestInvalid`。
///
/// # 参数
/// - `manifest`：模型清单。
/// - `dir`：模型目录。
///
/// # 返回
/// [`DecodeParams`]。
///
/// # 示例
/// ```ignore
/// let p = resolve_decode_params(&manifest, dir)?;
/// ```
pub fn resolve_decode_params(manifest: &Manifest, dir: &Path) -> Result<DecodeParams, EngineError> {
    let gen_cfg = read_json_opt(&dir.join("generation_config.json"));
    let cfg = read_json_opt(&dir.join("config.json"));
    let g = &manifest.generation;
    let eos = g
        .eos_token_id
        .or_else(|| json_i64(&gen_cfg, "eos_token_id"))
        .or_else(|| json_i64(&cfg, "eos_token_id"))
        .and_then(|v| u32::try_from(v).ok())
        .ok_or_else(|| {
            EngineError::new(
                ErrorKind::ManifestInvalid,
                "eos_token_id missing in model.json, generation_config.json and config.json",
            )
        })?;
    Ok(DecodeParams {
        eos,
        max_new: g.max_new_tokens.unwrap_or(DEFAULT_MAX_NEW_TOKENS),
        repetition_penalty: g
            .repetition_penalty
            .unwrap_or(HUNYUAN_DEFAULT_REPETITION_PENALTY),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 假模型：按脚本逐步给出“想选的 token”，并记录每步喂入的长度与 past_len。
    struct Scripted {
        /// 每步要让 argmax 选中的 token。
        script: Vec<u32>,
        /// 词表大小。
        vocab: usize,
        /// 已走步数。
        step: usize,
        /// 每步 `(ids.len(), past_len)`。
        calls: Vec<(usize, usize)>,
        /// 复用的 logits 缓冲。
        buf: Vec<f32>,
    }

    impl Scripted {
        /// 创建假模型。
        fn new(script: &[u32], vocab: usize) -> Self {
            Self {
                script: script.to_vec(),
                vocab,
                step: 0,
                calls: Vec::new(),
                buf: Vec::new(),
            }
        }
    }

    impl LogitsSource for Scripted {
        /// 目标 token 的 logit 为 10，其余 0。
        fn forward(&mut self, ids: &[u32], past_len: usize) -> Result<&[f32], EngineError> {
            self.calls.push((ids.len(), past_len));
            self.buf = vec![0.0; self.vocab];
            let want = self.script.get(self.step).copied().unwrap_or(0);
            self.buf[want as usize] = 10.0;
            self.step += 1;
            Ok(&self.buf)
        }
    }

    /// 缺省参数：eos 9，不惩罚。
    fn params(max_new: usize) -> DecodeParams {
        DecodeParams {
            eos: 9,
            max_new,
            repetition_penalty: 1.0,
        }
    }

    /// 与 HF 同公式：正值除、负值乘，只对出现过的 token；系数 1.0 等价普通 argmax。
    #[test]
    fn repetition_penalty_formula() {
        let seen = [false, true, true, false];
        // 下标 1：1.04/1.05 < 1.02，被压到第二；下标 2 为负，乘 1.05 更负
        assert_eq!(
            penalized_argmax(&[0.5, 1.04, -0.1, 1.02], &seen, 1.05),
            Some(3)
        );
        assert_eq!(
            penalized_argmax(&[0.5, 1.04, -0.1, 1.02], &seen, 1.0),
            Some(1)
        );
        assert_eq!(
            penalized_argmax(&[-1.0, -0.99, -2.0], &[true, true, false], 2.0),
            Some(1)
        );
        // 负 logit 乘以系数后比未出现的更小
        assert_eq!(
            penalized_argmax(&[-1.0, -1.4], &[true, false], 1.5),
            Some(1)
        );
        // 并列取小下标；seen 比 logits 短不越界；NaN 不参与
        assert_eq!(penalized_argmax(&[1.0, 1.0], &[], 1.05), Some(0));
        assert_eq!(penalized_argmax(&[f32::NAN, 0.1], &[], 1.0), Some(1));
        assert_eq!(penalized_argmax(&[f32::NAN], &[], 1.0), None);
        assert_eq!(penalized_argmax(&[], &[], 1.0), None);
    }

    /// 遇到 eos 立刻停，不含 eos；预填充整段提示词，之后每步 1 个 token，past_len 累加。
    #[test]
    fn stops_at_eos_and_feeds_incrementally() {
        let mut m = Scripted::new(&[3, 4, 9, 5], 16);
        let out = greedy_decode(&mut m, &[1, 2, 6], &params(50)).unwrap();
        assert_eq!(out, vec![3, 4]);
        assert_eq!(m.calls, vec![(3, 0), (1, 3), (1, 4)]);
    }

    /// 达到最大新 token 数停止；上限 0 或空提示词不调用模型。
    #[test]
    fn stops_at_max_new_and_handles_empty() {
        let mut m = Scripted::new(&[3, 4, 5, 6], 16);
        let out = greedy_decode(&mut m, &[1], &params(2)).unwrap();
        assert_eq!(out, vec![3, 4]);
        assert_eq!(m.calls.len(), 2);
        let mut none = Scripted::new(&[3], 16);
        assert!(
            greedy_decode(&mut none, &[1], &params(0))
                .unwrap()
                .is_empty()
        );
        assert!(
            greedy_decode(&mut none, &[], &params(5))
                .unwrap()
                .is_empty()
        );
        assert!(none.calls.is_empty());
    }

    /// 重复惩罚把提示词与已生成的 token 都算进“出现过”：logit 差距小时改选别的 token。
    #[test]
    fn penalty_covers_prompt_and_generated_tokens() {
        /// 两个 token 分差很小的假模型：token 2 略高，但出现在提示词里。
        struct Close(Vec<f32>);
        impl LogitsSource for Close {
            fn forward(&mut self, _ids: &[u32], _past: usize) -> Result<&[f32], EngineError> {
                self.0 = vec![0.0, 1.0, 1.02, 0.0];
                Ok(&self.0)
            }
        }
        let p = DecodeParams {
            eos: 0,
            max_new: 1,
            repetition_penalty: 1.05,
        };
        // 提示词含 2：2 被压低，选 1
        assert_eq!(
            greedy_decode(&mut Close(vec![]), &[2], &p).unwrap(),
            vec![1]
        );
        // 提示词不含 2：直接选 2
        assert_eq!(
            greedy_decode(&mut Close(vec![]), &[3], &p).unwrap(),
            vec![2]
        );
    }

    /// 原文里的特殊符号被剔除，拼出的新特殊串也会继续剔除。
    #[test]
    fn strips_special_markers() {
        let sp = vec!["<｜hy_Assistant｜>".to_string(), "<s>".to_string()];
        assert_eq!(strip_specials("a<｜hy_Assistant｜>b", &sp), "ab");
        assert_eq!(strip_specials("<<s>s>x", &sp), "x");
        assert_eq!(strip_specials("plain text", &sp), "plain text");
    }

    /// 打包：连续句子在上限内合并，超限换组，保留末句分隔符；单句超限独占一组。
    #[test]
    fn packs_segments_under_limit() {
        let segs = split_sentences("One. Two. Three.\nFour.");
        let by_len = |t: &str| t.len();
        let all = pack_segments(&segs, 1000, by_len);
        assert_eq!(all.len(), 1);
        let small = pack_segments(&segs, 9, by_len);
        assert!(small.len() > 1);
        assert!(small.iter().all(|(t, _)| !t.is_empty()));
        let tiny = pack_segments(&segs, 1, by_len);
        assert_eq!(tiny.len(), 4);
        assert_eq!(tiny[2].1, "\n");
    }

    /// 解码参数优先级：清单 > generation_config.json；缺省 repetition_penalty 1.05；缺 eos 报错。
    #[test]
    fn decode_params_resolution() {
        let dir = std::env::temp_dir().join(format!("snow-chat-params-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let manifest = |gen_json: &str| -> Manifest {
            serde_json::from_str(&format!(
                r#"{{"schema_version":1,"id":"c","family":"hunyuan_chat","files":{{}},"generation":{gen_json}}}"#
            ))
            .unwrap()
        };
        assert!(resolve_decode_params(&manifest("{}"), &dir).is_err());
        std::fs::write(dir.join("generation_config.json"), r#"{"eos_token_id":7}"#).unwrap();
        let p = resolve_decode_params(&manifest("{}"), &dir).unwrap();
        assert_eq!(p.eos, 7);
        assert_eq!(p.max_new, DEFAULT_MAX_NEW_TOKENS);
        assert_eq!(p.repetition_penalty, HUNYUAN_DEFAULT_REPETITION_PENALTY);
        let p = resolve_decode_params(
            &manifest(r#"{"eos_token_id":3,"max_new_tokens":64,"repetition_penalty":1.0}"#),
            &dir,
        )
        .unwrap();
        assert_eq!((p.eos, p.max_new, p.repetition_penalty), (3, 64, 1.0));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
