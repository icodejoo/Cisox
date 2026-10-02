//! 模型清单（`model.json`）解析与校验。
//!
//! 字段沿用 snow-translate 的 `ModelManifest`（schema_version=1），并按 nmt-plan §5.4
//! 以可选字段向后兼容地扩展：`pairs`、`lang_tokens`、`source_prefix`、`generation`、`execution`。
//!
//! 架构族：`marian`（opus-mt，语言 token 放在源文本前缀）与 `m2m100`（NLLB：编码器输入以源语言码
//! 开头、解码器第一个生成 token 强制为目标语言码，语言码取自 `lang_tokens`）。
//! `hunyuan_chat`（Hy-MT2 等 decoder-only 对话翻译模型）：单个 `model` 会话（预填充与逐 token 解码共用），
//! 提示词由清单 `prompt` 描述（前缀、后缀、含 `{target_lang}` 与 `{source_text}` 的模板、语言名表），
//! 只支持贪心 + `repetition_penalty`，`num_beams` 对它无效（不报错）。
//! 外部数据权重：`files` 可额外列出 `encoder_data` / `decoder_data`（`.onnx_data`），
//! 文件名必须与 ONNX 图里记录的 `location` 一致，并与 `.onnx` 同目录。

use std::collections::HashMap;
use std::path::{Component, Path, PathBuf};

use serde::Deserialize;

use crate::checksum;

/// 当前唯一支持的清单版本。
pub const SUPPORTED_SCHEMA_VERSION: u32 = 1;
/// 清单文件名。
pub const MANIFEST_FILE: &str = "model.json";
/// `files` 中必需的键：编码器。
pub const FILE_ENCODER: &str = "encoder";
/// `files` 中必需的键：合并解码器。
pub const FILE_DECODER: &str = "decoder";
/// `files` 中必需的键：分词器。
pub const FILE_TOKENIZER: &str = "tokenizer";
/// `files` 中可选的键：编码器外部数据（`.onnx_data`，与 `.onnx` 同目录）。
pub const FILE_ENCODER_DATA: &str = "encoder_data";
/// `files` 中可选的键：解码器外部数据。
pub const FILE_DECODER_DATA: &str = "decoder_data";
/// `files` 中 `hunyuan_chat` 必需的键：单个 decoder-only 模型。
pub const FILE_MODEL: &str = "model";
/// `files` 中 `hunyuan_chat` 可选的键：模型外部数据（`.onnx_data`，与 `.onnx` 同目录，ORT 自动内存映射）。
pub const FILE_MODEL_DATA: &str = "model_data";
/// 提示词模板里目标语言名的占位符。
pub const PROMPT_TARGET_PLACEHOLDER: &str = "{target_lang}";
/// 提示词模板里原文的占位符。
pub const PROMPT_SOURCE_PLACEHOLDER: &str = "{source_text}";
/// `hunyuan_chat` 缺省的 repetition_penalty（评测所用值，同模型官方 generation_config）。
pub const HUNYUAN_DEFAULT_REPETITION_PENALTY: f32 = 1.05;
/// 架构族：Hunyuan 对话式 decoder-only（Hy-MT2）。
pub const FAMILY_HUNYUAN_CHAT: &str = "hunyuan_chat";
/// 架构族：Marian（opus-mt）。
pub const FAMILY_MARIAN: &str = "marian";
/// 架构族：M2M100（NLLB）。
pub const FAMILY_M2M100: &str = "m2m100";
/// 源文本前缀里目标语言 token 的占位符。
pub const TGT_TOKEN_PLACEHOLDER: &str = "{tgt_token}";
/// 清单缺省的最大输入 token 数。
pub const DEFAULT_MAX_INPUT_TOKENS: usize = 512;
/// 缺省 ORT 图优化级别。
pub const DEFAULT_OPT_LEVEL: u8 = 3;
/// 束搜索缺省的 no-repeat n-gram 长度。
pub const DEFAULT_NO_REPEAT_NGRAM: usize = 3;
/// 束宽上限（防止请求过大的束宽放大内存）。
pub const MAX_BEAMS: usize = 8;
/// 缺省长度惩罚指数（与 HF 的 `length_penalty=1.0` 一致）。
pub const DEFAULT_LENGTH_PENALTY: f32 = 1.0;
/// `m2m100` 族缺省束宽（评测推荐：beam=2 比 4 更快且质量相当）。
pub const M2M100_DEFAULT_NUM_BEAMS: usize = 2;
/// `m2m100` 族缺省长度惩罚指数（评测推荐 2.0，抑制偏短译文）。
pub const M2M100_DEFAULT_LENGTH_PENALTY: f32 = 2.0;
/// `m2m100` 族缺省最小输出长度比例（评测推荐 0.7）。
pub const M2M100_DEFAULT_MIN_LENGTH_RATIO: f32 = 0.7;

/// 清单加载失败的原因。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ManifestError {
    /// 目录或 `model.json` 不存在。
    Missing(String),
    /// 内容非法（解析失败、版本不符、路径越界等）。
    Invalid(String),
    /// 文件 SHA-256 与清单声明不符（或无法读取以校验）。
    Checksum(String),
}

/// 生成参数（全部可选，缺省值由 `generation_config.json` / `config.json` 补齐）。
#[derive(Debug, Clone, Default, Deserialize, PartialEq)]
pub struct GenerationOverride {
    /// 解码起始 token。
    pub decoder_start_token_id: Option<i64>,
    /// 结束 token。
    pub eos_token_id: Option<i64>,
    /// 填充 token。
    pub pad_token_id: Option<i64>,
    /// 禁止生成的 token。
    pub bad_token_ids: Option<Vec<i64>>,
    /// 最大新生成 token 数。
    pub max_new_tokens: Option<usize>,
    /// 缺省束宽，1 即贪心；请求可覆盖，范围 `1..=MAX_BEAMS`。缺省：`marian` 为 1，`m2m100` 为 2。
    pub num_beams: Option<usize>,
    /// 束搜索长度惩罚指数。缺省：`marian` 为 1.0，`m2m100` 为 2.0。
    pub length_penalty: Option<f32>,
    /// 束搜索禁止重复的 n-gram 长度，0 关闭；缺省 3（抑制短句退化循环）。
    pub no_repeat_ngram_size: Option<usize>,
    /// 最小输出长度占输入 token 数的比例（仅 `m2m100`）：生成 token 数（含强制的语言码、
    /// 不含解码起始符）不足 `ceil(比例 × 输入 token 数)` 前禁止结束符，语义同 HF `min_new_tokens`；
    /// 输入 token 数含源语言码与结束符。缺省：`m2m100` 为 0.7，`marian` 为 0；显式写 0 即关闭。
    pub min_length_ratio: Option<f32>,
    /// 重复惩罚系数（仅 `hunyuan_chat`，与 HF 同公式：提示词与已生成 token 的正 logit 除以它、负 logit 乘以它），
    /// 缺省 1.05，1.0 关闭，必须 > 0。
    pub repetition_penalty: Option<f32>,
}

/// 对话式模型的提示词描述（`hunyuan_chat` 必需）。
#[derive(Debug, Clone, Deserialize, PartialEq)]
pub struct PromptSpec {
    /// 拼在最前面的模板文本（含特殊 token，如 `<｜hy_begin▁of▁sentence｜><｜hy_User｜>`）。
    pub prefix: String,
    /// 拼在最后面的模板文本（如 `<｜hy_Assistant｜>`，即 generation prompt）。
    pub suffix: String,
    /// 用户消息模板，必须含 `{target_lang}` 与 `{source_text}`。
    pub template: String,
    /// 应用语言码（`zh-CN`、`en`…）→ 提示词里的目标语言名（`Chinese`、`English`…）。
    pub lang_names: HashMap<String, String>,
}

impl PromptSpec {
    /// 按模板拼出完整提示词字符串（不做分词）。
    ///
    /// # 参数
    /// - `text`：原文（调用方负责先去掉其中冒充特殊 token 的片段，见 `engine` 的清洗函数）。
    /// - `tgt`：目标语言码，不区分大小写。
    ///
    /// # 返回
    /// `prefix + 模板(目标语言名, 原文) + suffix`；目标语言没有名字时返回错误说明。
    ///
    /// # 示例
    /// ```ignore
    /// let p = spec.render("Hello.", "zh-CN")?;
    /// assert!(p.ends_with("<｜hy_Assistant｜>"));
    /// ```
    pub fn render(&self, text: &str, tgt: &str) -> Result<String, String> {
        let name = self
            .lang_names
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(tgt))
            .map(|(_, v)| v.as_str())
            .ok_or_else(|| format!("prompt.lang_names has no entry for `{tgt}`"))?;
        let user = self
            .template
            .replace(PROMPT_TARGET_PLACEHOLDER, name)
            .replace(PROMPT_SOURCE_PLACEHOLDER, text);
        Ok(format!("{}{user}{}", self.prefix, self.suffix))
    }
}

/// 执行参数（可选）。
#[derive(Debug, Clone, Deserialize, PartialEq)]
pub struct ExecutionOptions {
    /// 单算子内并行线程数，0 表示自动。
    #[serde(default)]
    pub intra_threads: usize,
    /// 是否启用 ORT CPU 内存 arena；缺省 `true`（内存优先场景可关）。
    #[serde(default = "default_true")]
    pub cpu_arena: bool,
    /// 是否启用 ORT 内存模式规划；缺省 `true`。
    #[serde(default = "default_true")]
    pub mem_pattern: bool,
    /// ORT 图优化级别 0..=3（0 关闭），缺省 3。
    #[serde(default = "default_opt_level")]
    pub opt_level: u8,
    /// 是否启用权重预打包；缺省 `true`。
    #[serde(default = "default_true")]
    pub prepacking: bool,
    /// 每个翻译请求结束后收缩 ORT arena，把闲置内存还给系统；缺省 `false`。
    #[serde(default)]
    pub trim_after_request: bool,
}

impl Default for ExecutionOptions {
    /// 缺省：自动线程，arena 与内存模式保持 ORT 默认（开）。
    fn default() -> Self {
        Self {
            intra_threads: 0,
            cpu_arena: true,
            mem_pattern: true,
            opt_level: DEFAULT_OPT_LEVEL,
            prepacking: true,
            trim_after_request: false,
        }
    }
}

/// serde 缺省值：图优化级别。
fn default_opt_level() -> u8 {
    DEFAULT_OPT_LEVEL
}

/// serde 缺省值：`true`。
fn default_true() -> bool {
    true
}

/// 模型清单。
#[derive(Debug, Clone, Deserialize, PartialEq)]
pub struct Manifest {
    /// 清单版本，必须为 1。
    pub schema_version: u32,
    /// 模型唯一标识。
    pub id: String,
    /// 展示名。
    #[serde(default)]
    pub display_name: String,
    /// 架构族：`marian`、`m2m100` 或 `hunyuan_chat`。
    pub family: String,
    /// 量化类型。
    #[serde(default)]
    pub quantization: String,
    /// 文件表：编解码族 `encoder`/`decoder`/`tokenizer` 必需，`encoder_data`/`decoder_data` 可选；
    /// `hunyuan_chat` 要 `model`/`tokenizer`，`model_data` 可选。值为相对模型目录的路径。
    pub files: HashMap<String, String>,
    /// 可选的文件 SHA-256（十六进制）：文件键 → 摘要，声明了就在加载前校验。
    #[serde(default)]
    pub sha256: HashMap<String, String>,
    /// 支持的语言列表（`pairs` 缺省时取其有向全排列）。
    #[serde(default)]
    pub languages: Vec<String>,
    /// 最大输入 token 数。
    #[serde(default = "default_max_input_tokens")]
    pub max_input_tokens: usize,
    /// 有向语言对，缺省由 `languages` 展开。
    #[serde(default)]
    pub pairs: Option<Vec<(String, String)>>,
    /// 语言代码 → 模型内 token（`marian`：`zh-CN` → `>>cmn_Hans<<`；`m2m100`：`zh-CN` → `zho_Hans`）。
    #[serde(default)]
    pub lang_tokens: HashMap<String, String>,
    /// 源文本前缀模板，可含 `{tgt_token}`。
    #[serde(default)]
    pub source_prefix: String,
    /// 生成参数覆盖。
    #[serde(default)]
    pub generation: GenerationOverride,
    /// 执行参数。
    #[serde(default)]
    pub execution: ExecutionOptions,
    /// 提示词描述（`hunyuan_chat` 必需，其他族忽略）。
    #[serde(default)]
    pub prompt: Option<PromptSpec>,
    /// 是否参与默认选包（应用侧路由用，worker 只透传）；缺省 `true`。
    #[serde(default = "default_true")]
    pub default_eligible: bool,
}

/// serde 缺省值：最大输入 token 数。
fn default_max_input_tokens() -> usize {
    DEFAULT_MAX_INPUT_TOKENS
}

impl Manifest {
    /// 读取并校验模型目录下的 `model.json`。
    ///
    /// # 参数
    /// - `dir`：模型目录。
    ///
    /// # 返回
    /// 通过校验的清单；目录/文件缺失返回 [`ManifestError::Missing`]，其余问题返回 [`ManifestError::Invalid`]。
    ///
    /// # 示例
    /// ```ignore
    /// let m = Manifest::load(std::path::Path::new("D:/models/opus-mt-en-zh"))?;
    /// ```
    pub fn load(dir: &Path) -> Result<Self, ManifestError> {
        if !dir.is_dir() {
            return Err(ManifestError::Missing(format!(
                "model directory not found: {}",
                dir.display()
            )));
        }
        let path = dir.join(MANIFEST_FILE);
        let raw = std::fs::read_to_string(&path)
            .map_err(|e| ManifestError::Missing(format!("cannot read {}: {e}", path.display())))?;
        Self::parse(&raw, dir)
    }

    /// 解析清单文本并校验（含文件存在性与路径安全）。
    ///
    /// # 参数
    /// - `raw`：`model.json` 文本。
    /// - `dir`：模型目录（用于检查 `files`）。
    ///
    /// # 返回
    /// 通过校验的清单，或 [`ManifestError`]。
    ///
    /// # 示例
    /// ```ignore
    /// let m = Manifest::parse(&text, dir)?;
    /// ```
    pub fn parse(raw: &str, dir: &Path) -> Result<Self, ManifestError> {
        let manifest: Manifest = serde_json::from_str(raw)
            .map_err(|e| ManifestError::Invalid(format!("model.json parse error: {e}")))?;
        manifest.validate(dir)?;
        Ok(manifest)
    }

    /// 校验版本、必需字段、`files` 路径安全与存在性。
    fn validate(&self, dir: &Path) -> Result<(), ManifestError> {
        if self.schema_version != SUPPORTED_SCHEMA_VERSION {
            return Err(ManifestError::Invalid(format!(
                "unsupported schema_version {} (expected {SUPPORTED_SCHEMA_VERSION})",
                self.schema_version
            )));
        }
        if self.id.trim().is_empty() {
            return Err(ManifestError::Invalid("id must not be empty".into()));
        }
        if ![FAMILY_MARIAN, FAMILY_M2M100, FAMILY_HUNYUAN_CHAT].contains(&self.family.as_str()) {
            return Err(ManifestError::Invalid(format!(
                "unsupported family `{}` (expected `{FAMILY_MARIAN}`, `{FAMILY_M2M100}` or `{FAMILY_HUNYUAN_CHAT}`)",
                self.family
            )));
        }
        if self.max_input_tokens < 2 {
            return Err(ManifestError::Invalid(
                "max_input_tokens must be >= 2".into(),
            ));
        }
        if let Some(beams) = self.generation.num_beams
            && !(1..=MAX_BEAMS).contains(&beams)
        {
            return Err(ManifestError::Invalid(format!(
                "num_beams={beams} out of range 1..={MAX_BEAMS}"
            )));
        }
        if let Some(lp) = self.generation.length_penalty
            && !(lp.is_finite() && lp >= 0.0)
        {
            return Err(ManifestError::Invalid(format!(
                "length_penalty={lp} must be a finite number >= 0"
            )));
        }
        if let Some(ratio) = self.generation.min_length_ratio
            && !(ratio.is_finite() && (0.0..=1.0).contains(&ratio))
        {
            return Err(ManifestError::Invalid(format!(
                "min_length_ratio={ratio} must be within 0..=1"
            )));
        }
        if let Some(rp) = self.generation.repetition_penalty
            && !(rp.is_finite() && rp > 0.0)
        {
            return Err(ManifestError::Invalid(format!(
                "repetition_penalty={rp} must be a finite number > 0"
            )));
        }
        if self.is_m2m100() {
            self.validate_lang_tokens()?;
        }
        if self.is_hunyuan_chat() {
            self.validate_prompt()?;
        }
        if self.execution.opt_level > 3 {
            return Err(ManifestError::Invalid(format!(
                "execution.opt_level={} out of range 0..=3",
                self.execution.opt_level
            )));
        }
        for (key, digest) in &self.sha256 {
            if !self.files.contains_key(key) {
                return Err(ManifestError::Invalid(format!(
                    "sha256.{key} refers to a file not listed in `files`"
                )));
            }
            if !checksum::is_valid_hex(digest) {
                return Err(ManifestError::Invalid(format!(
                    "sha256.{key} is not a 64-digit hex digest"
                )));
            }
        }
        let (required, optional_keys) = if self.is_hunyuan_chat() {
            (
                [FILE_MODEL, FILE_TOKENIZER].as_slice(),
                [FILE_MODEL_DATA].as_slice(),
            )
        } else {
            (
                [FILE_ENCODER, FILE_DECODER, FILE_TOKENIZER].as_slice(),
                [FILE_ENCODER_DATA, FILE_DECODER_DATA].as_slice(),
            )
        };
        let optional = optional_keys
            .iter()
            .copied()
            .filter(|k| self.files.contains_key(*k));
        for key in required.iter().copied().chain(optional) {
            let path = self.resolve_file(dir, key)?;
            if !path.is_file() {
                return Err(ManifestError::Missing(format!(
                    "model file `{key}` not found: {}",
                    path.display()
                )));
            }
        }
        Ok(())
    }

    /// 是否为 M2M100 族（NLLB）。
    ///
    /// # 返回
    /// `family == "m2m100"` 时为 `true`。
    ///
    /// # 示例
    /// ```ignore
    /// if manifest.is_m2m100() { /* 源语言码前缀 + 强制目标语言码 */ }
    /// ```
    pub fn is_m2m100(&self) -> bool {
        self.family == FAMILY_M2M100
    }

    /// 是否为对话式 decoder-only 族（Hy-MT2）。
    ///
    /// # 返回
    /// `family == "hunyuan_chat"` 时为 `true`。
    ///
    /// # 示例
    /// ```ignore
    /// if manifest.is_hunyuan_chat() { /* 单会话 + 提示词 + 贪心 */ }
    /// ```
    pub fn is_hunyuan_chat(&self) -> bool {
        self.family == FAMILY_HUNYUAN_CHAT
    }

    /// 对话式族要求有完整提示词描述，且每个出现在 `languages` / `pairs` 里的语言都有语言名。
    fn validate_prompt(&self) -> Result<(), ManifestError> {
        let bad = |m: String| ManifestError::Invalid(m);
        let spec = self
            .prompt
            .as_ref()
            .ok_or_else(|| bad("family `hunyuan_chat` requires a `prompt` object".into()))?;
        for placeholder in [PROMPT_TARGET_PLACEHOLDER, PROMPT_SOURCE_PLACEHOLDER] {
            if !spec.template.contains(placeholder) {
                return Err(bad(format!("prompt.template must contain {placeholder}")));
            }
        }
        let from_pairs = self.pairs.iter().flatten().map(|(_, t)| t.as_str());
        let langs = self.languages.iter().map(String::as_str).chain(from_pairs);
        for lang in langs {
            spec.render("", lang).map_err(bad)?;
        }
        Ok(())
    }

    /// 查询语言代码对应的模型内 token（不区分大小写）。
    ///
    /// # 参数
    /// - `lang`：语言代码，如 `zh-CN`。
    ///
    /// # 返回
    /// `lang_tokens` 中的 token（如 `zho_Hans`）；缺失返回错误说明。
    ///
    /// # 示例
    /// ```ignore
    /// assert_eq!(manifest.lang_token_for("zh-cn")?, "zho_Hans");
    /// ```
    pub fn lang_token_for(&self, lang: &str) -> Result<&str, String> {
        self.lang_tokens
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(lang))
            .map(|(_, v)| v.as_str())
            .ok_or_else(|| format!("lang_tokens has no entry for `{lang}`"))
    }

    /// M2M100 族要求每个出现在 `languages` / `pairs` 里的语言都有 `lang_tokens` 项。
    fn validate_lang_tokens(&self) -> Result<(), ManifestError> {
        let from_pairs = self
            .pairs
            .iter()
            .flatten()
            .flat_map(|(s, t)| [s.as_str(), t.as_str()]);
        let mut all: Vec<&str> = self.languages.iter().map(String::as_str).collect();
        all.extend(from_pairs);
        for lang in all {
            self.lang_token_for(lang).map_err(ManifestError::Invalid)?;
        }
        Ok(())
    }

    /// 按清单声明的 `sha256` 流式校验模型文件（未声明的文件跳过）。
    ///
    /// # 参数
    /// - `dir`：模型目录。
    ///
    /// # 返回
    /// 全部一致返回 `Ok(())`；任一不符返回 [`ManifestError::Checksum`]（说明文件、期望与实际摘要）。
    ///
    /// # 示例
    /// ```ignore
    /// manifest.verify_checksums(dir)?;
    /// ```
    pub fn verify_checksums(&self, dir: &Path) -> Result<(), ManifestError> {
        // 排序保证多文件不符时报错顺序稳定
        let mut keys: Vec<&String> = self.sha256.keys().collect();
        keys.sort();
        for key in keys {
            let path = self.resolve_file(dir, key)?;
            checksum::verify_file(&path, &self.sha256[key]).map_err(ManifestError::Checksum)?;
        }
        Ok(())
    }

    /// 解析 `files[key]` 为模型目录内的绝对路径，拒绝绝对路径与 `..`。
    ///
    /// # 参数
    /// - `dir`：模型目录。
    /// - `key`：文件键。
    ///
    /// # 返回
    /// 目录内的路径；键缺失或路径越界返回 [`ManifestError::Invalid`]。
    ///
    /// # 示例
    /// ```ignore
    /// let enc = manifest.resolve_file(dir, "encoder")?;
    /// ```
    pub fn resolve_file(&self, dir: &Path, key: &str) -> Result<PathBuf, ManifestError> {
        let rel = self
            .files
            .get(key)
            .ok_or_else(|| ManifestError::Invalid(format!("files.{key} is required")))?;
        let rel_path = Path::new(rel);
        let safe = !rel.is_empty()
            && rel_path
                .components()
                .all(|c| matches!(c, Component::Normal(_)));
        if !safe {
            return Err(ManifestError::Invalid(format!(
                "files.{key} must be a relative path inside the model directory: {rel}"
            )));
        }
        Ok(dir.join(rel_path))
    }

    /// 判断是否支持某个有向语言对（不区分大小写）；`pairs` 缺省时由 `languages` 展开。
    ///
    /// # 参数
    /// - `src`：源语言代码。
    /// - `tgt`：目标语言代码。
    ///
    /// # 返回
    /// 支持返回 `true`。
    ///
    /// # 示例
    /// ```ignore
    /// assert!(manifest.supports_pair("en", "zh-CN"));
    /// ```
    pub fn supports_pair(&self, src: &str, tgt: &str) -> bool {
        let eq = |a: &str, b: &str| a.eq_ignore_ascii_case(b);
        match &self.pairs {
            Some(pairs) => pairs.iter().any(|(s, t)| eq(s, src) && eq(t, tgt)),
            None => {
                !eq(src, tgt)
                    && self.languages.iter().any(|l| eq(l, src))
                    && self.languages.iter().any(|l| eq(l, tgt))
            }
        }
    }

    /// 计算某个目标语言的源文本前缀（展开 `{tgt_token}`）。
    ///
    /// # 参数
    /// - `tgt`：目标语言代码。
    ///
    /// # 返回
    /// 前缀文本（可能为空）；模板要求语言 token 但 `lang_tokens` 缺失时返回错误说明。
    ///
    /// # 示例
    /// ```ignore
    /// assert_eq!(manifest.source_prefix_for("zh-CN")?, ">>cmn_Hans<< ");
    /// ```
    pub fn source_prefix_for(&self, tgt: &str) -> Result<String, String> {
        if !self.source_prefix.contains(TGT_TOKEN_PLACEHOLDER) {
            return Ok(self.source_prefix.clone());
        }
        let token = self
            .lang_token_for(tgt)
            .map_err(|_| format!("lang_tokens has no entry for target `{tgt}`"))?;
        Ok(self.source_prefix.replace(TGT_TOKEN_PLACEHOLDER, token))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    /// 创建带三个占位文件的临时模型目录。
    fn temp_model_dir(tag: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("snow-translator-test-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        for f in ["encoder.onnx", "decoder.onnx", "tokenizer.json"] {
            fs::write(dir.join(f), b"x").unwrap();
        }
        dir
    }

    /// 生成基础清单 JSON，`extra` 追加在末尾字段。
    fn manifest_json(extra: &str) -> String {
        format!(
            r#"{{"schema_version":1,"id":"m","family":"marian","files":{{"encoder":"encoder.onnx","decoder":"decoder.onnx","tokenizer":"tokenizer.json"}},"languages":["en","zh-CN"]{extra}}}"#
        )
    }

    /// 仅含旧字段的清单可解析，`pairs` 由 languages 展开。
    #[test]
    fn legacy_manifest_parses_and_expands_pairs() {
        let dir = temp_model_dir("legacy");
        let m = Manifest::parse(&manifest_json(""), &dir).unwrap();
        assert_eq!(m.max_input_tokens, DEFAULT_MAX_INPUT_TOKENS);
        assert!(m.supports_pair("en", "zh-cn"));
        assert!(m.supports_pair("zh-CN", "en"));
        assert!(!m.supports_pair("en", "en"));
        assert!(!m.supports_pair("en", "fr"));
        let _ = fs::remove_dir_all(dir);
    }

    /// 显式 pairs 是有向的。
    #[test]
    fn explicit_pairs_are_directional() {
        let dir = temp_model_dir("pairs");
        let m = Manifest::parse(&manifest_json(r#","pairs":[["en","zh-CN"]]"#), &dir).unwrap();
        assert!(m.supports_pair("en", "zh-CN"));
        assert!(!m.supports_pair("zh-CN", "en"));
        let _ = fs::remove_dir_all(dir);
    }

    /// 版本不符、家族不支持、束宽>1 均被拒绝。
    #[test]
    fn rejects_bad_fields() {
        let dir = temp_model_dir("bad");
        let v2 = manifest_json("").replace(r#""schema_version":1"#, r#""schema_version":2"#);
        assert!(matches!(
            Manifest::parse(&v2, &dir),
            Err(ManifestError::Invalid(_))
        ));
        let nllb = manifest_json("").replace("marian", "nllb");
        assert!(matches!(
            Manifest::parse(&nllb, &dir),
            Err(ManifestError::Invalid(_))
        ));
        let beam = manifest_json(r#","generation":{"num_beams":99}"#);
        assert!(matches!(
            Manifest::parse(&beam, &dir),
            Err(ManifestError::Invalid(_))
        ));
        let zero = manifest_json(r#","generation":{"num_beams":0}"#);
        assert!(matches!(
            Manifest::parse(&zero, &dir),
            Err(ManifestError::Invalid(_))
        ));
        let lp = manifest_json(r#","generation":{"length_penalty":-1.0}"#);
        assert!(matches!(
            Manifest::parse(&lp, &dir),
            Err(ManifestError::Invalid(_))
        ));
        assert!(matches!(
            Manifest::parse("{oops", &dir),
            Err(ManifestError::Invalid(_))
        ));
        let _ = fs::remove_dir_all(dir);
    }

    /// 路径穿越与绝对路径被拒绝。
    #[test]
    fn rejects_path_traversal() {
        let dir = temp_model_dir("trav");
        let up = manifest_json("").replace("encoder.onnx", "../evil.onnx");
        assert!(matches!(
            Manifest::parse(&up, &dir),
            Err(ManifestError::Invalid(_))
        ));
        let abs = manifest_json("").replace("encoder.onnx", "C:/evil.onnx");
        assert!(matches!(
            Manifest::parse(&abs, &dir),
            Err(ManifestError::Invalid(_))
        ));
        let _ = fs::remove_dir_all(dir);
    }

    /// 文件缺失、目录缺失分别报 Missing。
    #[test]
    fn missing_file_and_dir_are_reported() {
        let dir = temp_model_dir("miss");
        fs::remove_file(dir.join("decoder.onnx")).unwrap();
        assert!(matches!(
            Manifest::parse(&manifest_json(""), &dir),
            Err(ManifestError::Missing(_))
        ));
        let _ = fs::remove_dir_all(&dir);
        assert!(matches!(
            Manifest::load(&dir),
            Err(ManifestError::Missing(_))
        ));
    }

    /// 前缀模板展开语言 token；缺 token 时报错；无占位符原样返回。
    #[test]
    fn source_prefix_expansion() {
        let dir = temp_model_dir("prefix");
        let json = manifest_json(
            r#","source_prefix":"{tgt_token} ","lang_tokens":{"zh-CN":">>cmn_Hans<<"}"#,
        );
        let m = Manifest::parse(&json, &dir).unwrap();
        assert_eq!(m.source_prefix_for("zh-cn").unwrap(), ">>cmn_Hans<< ");
        assert!(m.source_prefix_for("zh-TW").is_err());
        let plain = Manifest::parse(&manifest_json(""), &dir).unwrap();
        assert_eq!(plain.source_prefix_for("zh-CN").unwrap(), "");
        let _ = fs::remove_dir_all(dir);
    }

    /// 束宽 1..=MAX_BEAMS 与长度惩罚可解析。
    #[test]
    fn beam_settings_parse() {
        let dir = temp_model_dir("beam-ok");
        let m = Manifest::parse(
            &manifest_json(r#","generation":{"num_beams":4,"length_penalty":1.2}"#),
            &dir,
        )
        .unwrap();
        assert_eq!(m.generation.num_beams, Some(4));
        assert_eq!(m.generation.length_penalty, Some(1.2));
        let _ = fs::remove_dir_all(dir);
    }

    /// sha256 声明：格式非法/引用未知文件被拒；一致通过；不符给出 Checksum 错误。
    #[test]
    fn sha256_declarations() {
        let dir = temp_model_dir("sha");
        // 三个占位文件内容都是 "x"
        let x_digest = "2d711642b726b04401627ca9fbac32f5c8530fb1903cc4db02258717921a4881";
        let ok = manifest_json(&format!(r#","sha256":{{"encoder":"{x_digest}"}}"#));
        let m = Manifest::parse(&ok, &dir).unwrap();
        assert!(m.verify_checksums(&dir).is_ok());

        let bad = manifest_json(&format!(r#","sha256":{{"decoder":"{}"}}"#, "0".repeat(64)));
        let m = Manifest::parse(&bad, &dir).unwrap();
        assert!(matches!(
            m.verify_checksums(&dir),
            Err(ManifestError::Checksum(msg)) if msg.contains("sha256 mismatch")
        ));

        let short = manifest_json(r#","sha256":{"encoder":"abc"}"#);
        assert!(matches!(
            Manifest::parse(&short, &dir),
            Err(ManifestError::Invalid(_))
        ));
        let unknown = manifest_json(&format!(r#","sha256":{{"nope":"{x_digest}"}}"#));
        assert!(matches!(
            Manifest::parse(&unknown, &dir),
            Err(ManifestError::Invalid(_))
        ));
        // 未声明 sha256 时直接通过
        let none = Manifest::parse(&manifest_json(""), &dir).unwrap();
        assert!(none.verify_checksums(&dir).is_ok());
        let _ = fs::remove_dir_all(dir);
    }

    /// 生成 m2m100 清单 JSON：带外部数据文件与 FLORES 语言码。
    fn m2m100_json(extra: &str) -> String {
        format!(
            r#"{{"schema_version":1,"id":"n","family":"m2m100","quantization":"int4",
            "files":{{"encoder":"encoder.onnx","encoder_data":"encoder.onnx_data","decoder":"decoder.onnx","decoder_data":"decoder.onnx_data","tokenizer":"tokenizer.json"}},
            "languages":["zh-CN","en"],"lang_tokens":{{"zh-CN":"zho_Hans","en":"eng_Latn"}},
            "generation":{{"num_beams":2,"bad_token_ids":[],"min_length_ratio":0.5}}{extra}}}"#
        )
    }

    /// 写出外部数据占位文件。
    fn touch_data_files(dir: &Path) {
        for f in ["encoder.onnx_data", "decoder.onnx_data"] {
            fs::write(dir.join(f), b"x").unwrap();
        }
    }

    /// m2m100 清单：可解析，语言码查询不区分大小写，外部数据文件被识别。
    #[test]
    fn m2m100_manifest_parses() {
        let dir = temp_model_dir("m2m");
        touch_data_files(&dir);
        let m = Manifest::parse(&m2m100_json(""), &dir).unwrap();
        assert!(m.is_m2m100());
        assert_eq!(m.generation.num_beams, Some(2));
        assert_eq!(m.generation.min_length_ratio, Some(0.5));
        assert_eq!(m.generation.bad_token_ids, Some(vec![]));
        assert_eq!(m.lang_token_for("ZH-cn").unwrap(), "zho_Hans");
        assert!(m.lang_token_for("fr").is_err());
        assert!(m.supports_pair("zh-CN", "en"));
        assert!(
            m.resolve_file(&dir, FILE_ENCODER_DATA)
                .unwrap()
                .ends_with("encoder.onnx_data")
        );
        // 旧清单（marian）不带任何新字段，仍然通过
        assert!(
            !Manifest::parse(&manifest_json(""), &dir)
                .unwrap()
                .is_m2m100()
        );
        let _ = fs::remove_dir_all(dir);
    }

    /// m2m100 清单：缺外部数据文件、缺语言码、比例越界均被拒绝。
    #[test]
    fn m2m100_manifest_rejects_bad_fields() {
        let dir = temp_model_dir("m2m-bad");
        // 列出了 .onnx_data 但文件不存在
        assert!(matches!(
            Manifest::parse(&m2m100_json(""), &dir),
            Err(ManifestError::Missing(_))
        ));
        touch_data_files(&dir);
        let no_token = m2m100_json("").replace(r#","en":"eng_Latn""#, "");
        assert!(matches!(
            Manifest::parse(&no_token, &dir),
            Err(ManifestError::Invalid(m)) if m.contains("lang_tokens")
        ));
        let ratio = m2m100_json("").replace("0.5", "1.5");
        assert!(matches!(
            Manifest::parse(&ratio, &dir),
            Err(ManifestError::Invalid(_))
        ));
        // 外部数据路径穿越
        let up = m2m100_json("").replace("decoder.onnx_data", "../d.onnx_data");
        assert!(matches!(
            Manifest::parse(&up, &dir),
            Err(ManifestError::Invalid(_))
        ));
        let _ = fs::remove_dir_all(dir);
    }

    /// 生成 hunyuan_chat 清单 JSON：单文件 + 外部数据，提示词含语言名；`extra` 追加在末尾字段。
    fn chat_json(extra: &str) -> String {
        format!(
            r#"{{"schema_version":1,"id":"hy","family":"hunyuan_chat","quantization":"int4",
            "files":{{"model":"model.onnx","model_data":"model.onnx_data","tokenizer":"tokenizer.json"}},
            "languages":["zh-CN","en"],"pairs":[["en","zh-CN"],["zh-CN","en"]],
            "prompt":{{"prefix":"<U>","suffix":"<A>","template":"To {{target_lang}}:\n\n{{source_text}}",
            "lang_names":{{"zh-CN":"Chinese","en":"English"}}}},
            "generation":{{"eos_token_id":7,"repetition_penalty":1.05}},"default_eligible":false{extra}}}"#
        )
    }

    /// 写出 hunyuan_chat 需要的占位文件。
    fn touch_chat_files(dir: &Path) {
        for f in ["model.onnx", "model.onnx_data", "tokenizer.json"] {
            fs::write(dir.join(f), b"x").unwrap();
        }
    }

    /// hunyuan_chat 清单：不要求 encoder/decoder，字段可解析，提示词按模板渲染，beams 被忽略但不报错。
    #[test]
    fn hunyuan_chat_manifest_parses_and_renders() {
        let dir = std::env::temp_dir().join(format!("snow-chat-manifest-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        touch_chat_files(&dir);
        let m = Manifest::parse(&chat_json(""), &dir).unwrap();
        assert!(m.is_hunyuan_chat() && !m.is_m2m100());
        assert!(!m.default_eligible);
        assert_eq!(m.generation.repetition_penalty, Some(1.05));
        assert!(m.supports_pair("en", "ZH-cn"));
        let spec = m.prompt.as_ref().unwrap();
        assert_eq!(
            spec.render("Hi.", "zh-cn").unwrap(),
            "<U>To Chinese:\n\nHi.<A>"
        );
        assert!(spec.render("Hi.", "fr").is_err());
        // 原文里的占位符不会被二次替换
        assert_eq!(
            spec.render("{target_lang}", "en").unwrap(),
            "<U>To English:\n\n{target_lang}<A>"
        );
        let beams =
            chat_json("").replace(r#""eos_token_id":7"#, r#""eos_token_id":7,"num_beams":4"#);
        assert!(Manifest::parse(&beams, &dir).is_ok());
        // 其他族缺省参与默认选包
        assert!(
            Manifest::parse(&manifest_json(""), &temp_model_dir("elig"))
                .unwrap()
                .default_eligible
        );
        let _ = fs::remove_dir_all(dir);
    }

    /// hunyuan_chat 清单：缺 prompt、模板缺占位符、语言缺名字、惩罚非法、缺模型文件均被拒绝。
    #[test]
    fn hunyuan_chat_manifest_rejects_bad_fields() {
        let dir = std::env::temp_dir().join(format!("snow-chat-bad-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        touch_chat_files(&dir);
        let invalid =
            |json: String| matches!(Manifest::parse(&json, &dir), Err(ManifestError::Invalid(_)));
        let no_prompt = chat_json("").replace(r#""prompt""#, r#""prompt_x""#);
        assert!(invalid(no_prompt));
        assert!(invalid(chat_json("").replace("{source_text}", "x")));
        assert!(invalid(chat_json("").replace(r#","en":"English""#, "")));
        assert!(invalid(chat_json("").replace("1.05", "0")));
        fs::remove_file(dir.join("model.onnx_data")).unwrap();
        assert!(matches!(
            Manifest::parse(&chat_json(""), &dir),
            Err(ManifestError::Missing(_))
        ));
        let _ = fs::remove_dir_all(dir);
    }
}
