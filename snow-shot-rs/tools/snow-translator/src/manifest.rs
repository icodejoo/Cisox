//! 模型清单（`model.json`）解析与校验。
//!
//! 字段沿用 snow-translate 的 `ModelManifest`（schema_version=1），并按 nmt-plan §5.4
//! 以可选字段向后兼容地扩展：`pairs`、`lang_tokens`、`source_prefix`、`generation`、`execution`。

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
    /// 缺省束宽，1 即贪心；请求可覆盖，范围 `1..=MAX_BEAMS`。
    pub num_beams: Option<usize>,
    /// 束搜索长度惩罚指数，缺省 1.0。
    pub length_penalty: Option<f32>,
    /// 束搜索禁止重复的 n-gram 长度，0 关闭；缺省 3（抑制短句退化循环）。
    pub no_repeat_ngram_size: Option<usize>,
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
    /// 架构族（首版仅 `marian`）。
    pub family: String,
    /// 量化类型。
    #[serde(default)]
    pub quantization: String,
    /// 文件表：`encoder`/`decoder`/`tokenizer` → 相对模型目录的路径。
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
    /// 语言代码 → 模型内 token（如 `zh-CN` → `>>cmn_Hans<<`）。
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
        if self.family != "marian" {
            return Err(ManifestError::Invalid(format!(
                "unsupported family `{}` (only `marian` is implemented)",
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
        for key in [FILE_ENCODER, FILE_DECODER, FILE_TOKENIZER] {
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
            .lang_tokens
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(tgt))
            .map(|(_, v)| v)
            .ok_or_else(|| format!("lang_tokens has no entry for target `{tgt}`"))?;
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
}
