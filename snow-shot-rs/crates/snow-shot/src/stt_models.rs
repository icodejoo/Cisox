//! 语音转文字模型清单：内置 JSON 清单的解析，以及按「语言维度 + 识别模式」选默认 / 备选模型。
//!
//! 清单是 `resources/stt-model-manifest.json`（模型来自 sherpa-onnx 的 `asr-models` 发布页），
//! 默认与备选取自本机实测。下载与落盘见 [`crate::stt_download`]。

use serde::Deserialize;
use snow_config::extensions::{
    DICTATION_DIMENSION_BILINGUAL, DICTATION_DIMENSION_EN, DICTATION_DIMENSION_ZH,
    DICTATION_RECOGNITION_OFFLINE, DICTATION_RECOGNITION_STREAMING,
};
use snow_stt_protocol::{ModelKind, RecognitionMode};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

/// 内置清单。
const MANIFEST_JSON: &str = include_str!("../resources/stt-model-manifest.json");
/// 清单版本号。
const MANIFEST_SCHEMA: u32 = 1;
/// 数据根下的模型目录（相对路径各段）。
const MODELS_SUBDIR: [&str; 2] = ["models", "stt"];

/// 语言维度。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Dimension {
    /// 中文。
    Zh,
    /// 英文。
    En,
    /// 中英混合。
    Bilingual,
}

impl Dimension {
    /// 全部取值，便于遍历。
    pub const ALL: [Dimension; 3] = [Self::Zh, Self::En, Self::Bilingual];

    /// 由配置值解析；未知值取中英混合（配置默认值）。
    ///
    /// # 参数
    /// - `value`：`dictation/language_dimension` 的取值。
    ///
    /// ```ignore
    /// assert_eq!(Dimension::from_config("zh"), Dimension::Zh);
    /// ```
    pub fn from_config(value: &str) -> Self {
        match value {
            DICTATION_DIMENSION_ZH => Self::Zh,
            DICTATION_DIMENSION_EN => Self::En,
            _ => Self::Bilingual,
        }
    }

    /// 配置取值字符串。
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Zh => DICTATION_DIMENSION_ZH,
            Self::En => DICTATION_DIMENSION_EN,
            Self::Bilingual => DICTATION_DIMENSION_BILINGUAL,
        }
    }
}

/// 识别模式的配置取值解析；未知值取流式。
///
/// # 参数
/// - `value`：`dictation/mode` 的取值。
pub fn mode_from_config(value: &str) -> RecognitionMode {
    if value == DICTATION_RECOGNITION_OFFLINE {
        RecognitionMode::Offline
    } else {
        RecognitionMode::Streaming
    }
}

/// 识别模式的配置取值字符串。
pub fn mode_as_str(mode: RecognitionMode) -> &'static str {
    match mode {
        RecognitionMode::Streaming => DICTATION_RECOGNITION_STREAMING,
        RecognitionMode::Offline => DICTATION_RECOGNITION_OFFLINE,
    }
}

/// 模型在该维度 / 模式下的角色。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    /// 默认。
    Default,
    /// 备选。
    Alternate,
    /// 旧版（仍可选，不推荐）。
    Legacy,
}

/// 压缩包信息。
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct ArchiveSpec {
    /// 文件名（`<id>.tar.bz2`）。
    pub name: String,
    /// 下载地址。
    pub url: String,
    /// 字节数。
    pub size: u64,
    /// SHA-256（小写十六进制）；空串表示尚未固定。
    pub sha256: String,
}

/// 共享资产（离线模式共用的 Silero VAD）。
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct SharedAsset {
    /// 文件名。
    pub name: String,
    /// 下载地址。
    pub url: String,
    /// 字节数。
    pub size: u64,
    /// SHA-256（小写十六进制）；空串表示尚未固定。
    pub sha256: String,
    /// 许可证。
    pub license: String,
}

/// 清单里的一个模型（已解析成强类型）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SttModelSpec {
    /// 模型 ID（即 sherpa 压缩包解压后的目录名）。
    pub id: String,
    /// 语言维度。
    pub dimension: Dimension,
    /// 识别模式。
    pub mode: RecognitionMode,
    /// 模型类型（决定 worker 用哪种识别器）。
    pub kind: ModelKind,
    /// 角色。
    pub role: Role,
    /// 压缩包。
    pub archive: ArchiveSpec,
    /// 解压后必需的文件（相对模型目录）。
    pub files: Vec<String>,
    /// 许可证标识（`unverified` 表示尚未核对）。
    pub license: String,
    /// 实际选中文件的合计字节数。
    pub size_bytes: u64,
    /// 评测口径的峰值内存（MiB，仅作提示）。
    pub peak_mem_mb: u32,
    /// 说明文案的 i18n 键（可选）。
    pub notes_key: Option<String>,
}

/// 清单里的原始条目（mode / kind 先按字符串读入，再校验转换）。
#[derive(Debug, Deserialize)]
struct RawSpec {
    id: String,
    dimension: Dimension,
    mode: String,
    kind: String,
    role: Role,
    archive: ArchiveSpec,
    files: Vec<String>,
    license: String,
    size_bytes: u64,
    peak_mem_mb: u32,
    #[serde(default)]
    notes_key: Option<String>,
}

/// 清单原始文档。
#[derive(Debug, Deserialize)]
struct RawManifest {
    schema: u32,
    vad: SharedAsset,
    models: Vec<RawSpec>,
}

/// 解析后的清单。
#[derive(Debug, Clone)]
pub struct Manifest {
    /// 共享的 VAD 资产。
    pub vad: SharedAsset,
    /// 全部模型（保持清单顺序）。
    pub models: Vec<SttModelSpec>,
}

impl RawSpec {
    /// 校验并转成强类型条目。
    fn into_spec(self) -> Result<SttModelSpec, String> {
        let mode = match self.mode.as_str() {
            DICTATION_RECOGNITION_STREAMING => RecognitionMode::Streaming,
            DICTATION_RECOGNITION_OFFLINE => RecognitionMode::Offline,
            other => return Err(format!("{}: 未知模式 {other}", self.id)),
        };
        let kind = ModelKind::parse(&self.kind).map_err(|e| format!("{}: {e}", self.id))?;
        if kind.is_offline() != (mode == RecognitionMode::Offline) {
            return Err(format!("{}: 模式与模型类型不匹配", self.id));
        }
        if self.files.is_empty() {
            return Err(format!("{}: 文件清单为空", self.id));
        }
        Ok(SttModelSpec {
            id: self.id,
            dimension: self.dimension,
            mode,
            kind,
            role: self.role,
            archive: self.archive,
            files: self.files,
            license: self.license,
            size_bytes: self.size_bytes,
            peak_mem_mb: self.peak_mem_mb,
            notes_key: self.notes_key,
        })
    }
}

/// 解析清单文本（测试可注入自定义清单）。
///
/// # 参数
/// - `json`：清单 JSON。
///
/// # 返回
/// 解析后的清单；格式错误、版本不符或条目非法返回说明。
pub fn parse_manifest(json: &str) -> Result<Manifest, String> {
    let raw: RawManifest =
        serde_json::from_str(json).map_err(|e| format!("语音模型清单格式错误: {e}"))?;
    if raw.schema != MANIFEST_SCHEMA {
        return Err(format!("语音模型清单版本不支持: {}", raw.schema));
    }
    let models = raw
        .models
        .into_iter()
        .map(RawSpec::into_spec)
        .collect::<Result<Vec<_>, _>>()?;
    Ok(Manifest {
        vad: raw.vad,
        models,
    })
}

/// 内置清单（首次访问时解析；内置数据有测试守护，解析失败时记日志并按空清单处理）。
pub fn manifest() -> &'static Manifest {
    static CELL: OnceLock<Manifest> = OnceLock::new();
    CELL.get_or_init(|| {
        parse_manifest(MANIFEST_JSON).unwrap_or_else(|e| {
            tracing::error!(error = %e, "内置语音模型清单无效，语音模型列表为空");
            Manifest {
                vad: SharedAsset {
                    name: snow_stt_protocol::VAD_MODEL_FILE_NAME.to_string(),
                    url: String::new(),
                    size: 0,
                    sha256: String::new(),
                    license: String::new(),
                },
                models: Vec::new(),
            }
        })
    })
}

/// 按 ID 查模型。
///
/// # 参数
/// - `id`：模型 ID。
pub fn find(id: &str) -> Option<&'static SttModelSpec> {
    manifest().models.iter().find(|m| m.id == id)
}

/// 某维度某模式下的全部候选：默认在前，其次备选，最后旧版；同角色保持清单顺序。
///
/// # 参数
/// - `dimension`：语言维度。
/// - `mode`：识别模式。
pub fn list_for(dimension: Dimension, mode: RecognitionMode) -> Vec<&'static SttModelSpec> {
    let mut list: Vec<_> = manifest()
        .models
        .iter()
        .filter(|m| m.dimension == dimension && m.mode == mode)
        .collect();
    list.sort_by_key(|m| m.role as u8);
    list
}

/// 某维度某模式下的默认模型。
///
/// # 参数
/// - `dimension`：语言维度。
/// - `mode`：识别模式。
pub fn default_for(dimension: Dimension, mode: RecognitionMode) -> Option<&'static SttModelSpec> {
    list_for(dimension, mode)
        .into_iter()
        .find(|m| m.role == Role::Default)
}

/// 某维度某模式下的备选模型（不含默认与旧版）。
///
/// # 参数
/// - `dimension`：语言维度。
/// - `mode`：识别模式。
pub fn alternates_for(dimension: Dimension, mode: RecognitionMode) -> Vec<&'static SttModelSpec> {
    list_for(dimension, mode)
        .into_iter()
        .filter(|m| m.role == Role::Alternate)
        .collect()
}

/// 按配置选出实际使用的模型：`model_id` 为空取默认；非空且属于该维度与模式的候选则用它；
/// 不属于（如切换了维度或清单已移除该模型）时回落到默认，避免卡在失效的选择上。
///
/// # 参数
/// - `dimension`：语言维度。
/// - `mode`：识别模式。
/// - `model_id`：用户选的备选 ID，空串为默认。
///
/// # 返回
/// 选中的模型；清单里该组合没有默认模型返回错误说明。
///
/// ```ignore
/// let spec = resolve(Dimension::Zh, RecognitionMode::Offline, "")?;
/// ```
pub fn resolve(
    dimension: Dimension,
    mode: RecognitionMode,
    model_id: &str,
) -> Result<&'static SttModelSpec, String> {
    if !model_id.is_empty()
        && let Some(spec) = list_for(dimension, mode)
            .into_iter()
            .find(|m| m.id == model_id)
    {
        return Ok(spec);
    }
    default_for(dimension, mode).ok_or_else(|| {
        format!(
            "清单里没有 {} / {} 的默认模型",
            dimension.as_str(),
            mode_as_str(mode)
        )
    })
}

/// 数据根下的语音模型根目录：`<数据根>/models/stt`（各模型目录与共享 VAD 都在其下）。
///
/// # 参数
/// - `data_root`：应用数据根目录。
pub fn models_root(data_root: &Path) -> PathBuf {
    let mut dir = data_root.to_path_buf();
    dir.extend(MODELS_SUBDIR);
    dir
}

/// 某模型的目录：`<数据根>/models/stt/<id>`。
///
/// # 参数
/// - `spec`：模型。
/// - `data_root`：应用数据根目录。
pub fn model_dir(spec: &SttModelSpec, data_root: &Path) -> PathBuf {
    models_root(data_root).join(&spec.id)
}

/// 共享 VAD 文件路径：`<数据根>/models/stt/silero_vad.onnx`（各模型目录的父目录，worker 会向上查找）。
///
/// # 参数
/// - `data_root`：应用数据根目录。
pub fn vad_path(data_root: &Path) -> PathBuf {
    models_root(data_root).join(&manifest().vad.name)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 取默认模型 ID（测试辅助）。
    fn default_id(d: Dimension, m: RecognitionMode) -> String {
        default_for(d, m).expect("默认模型").id.clone()
    }

    /// 内置清单可解析，条目 ID 唯一，压缩包名与 ID 对应，文件清单不含重复。
    #[test]
    fn builtin_manifest_is_valid() {
        let m = manifest();
        assert!(!m.models.is_empty());
        let mut seen = std::collections::HashSet::new();
        for spec in &m.models {
            assert!(seen.insert(spec.id.clone()), "ID 重复: {}", spec.id);
            assert_eq!(spec.archive.name, format!("{}.tar.bz2", spec.id));
            assert!(spec.archive.url.starts_with("https://"), "{}", spec.id);
            assert!(spec.archive.size > 0 && spec.size_bytes > 0, "{}", spec.id);
            assert!(
                spec.files.iter().any(|f| f == "tokens.txt"),
                "{} 缺 tokens",
                spec.id
            );
            let unique: std::collections::HashSet<_> = spec.files.iter().collect();
            assert_eq!(unique.len(), spec.files.len(), "{} 文件重复", spec.id);
        }
        assert_eq!(m.vad.name, snow_stt_protocol::VAD_MODEL_FILE_NAME);
    }

    /// 每个（维度, 模式）组合恰有一个默认模型。
    #[test]
    fn every_combination_has_exactly_one_default() {
        for d in Dimension::ALL {
            for mode in [RecognitionMode::Streaming, RecognitionMode::Offline] {
                let count = list_for(d, mode)
                    .iter()
                    .filter(|s| s.role == Role::Default)
                    .count();
                assert_eq!(count, 1, "{} / {}", d.as_str(), mode_as_str(mode));
            }
        }
    }

    /// 默认值表与实测结论一致。
    #[test]
    fn defaults_match_evaluation() {
        use RecognitionMode::{Offline, Streaming};
        let s = "sherpa-onnx-";
        assert_eq!(
            default_id(Dimension::Zh, Streaming),
            format!("{s}streaming-zipformer-multi-zh-hans-2023-12-12")
        );
        assert_eq!(
            default_id(Dimension::En, Streaming),
            format!("{s}streaming-zipformer-en-2023-06-26")
        );
        assert_eq!(
            default_id(Dimension::Bilingual, Streaming),
            format!("{s}x-asr-480ms-streaming-zipformer-transducer-zh-en-punct-int8-2026-06-05")
        );
        assert_eq!(
            default_id(Dimension::Zh, Offline),
            format!("{s}paraformer-zh-small-2024-03-09")
        );
        assert_eq!(
            default_id(Dimension::En, Offline),
            format!("{s}zipformer-small-en-2023-06-26")
        );
        let bi = default_for(Dimension::Bilingual, Offline).expect("默认");
        assert_eq!(
            bi.id,
            format!("{s}x-asr-zipformer-transducer-zh-en-punct-int8-2026-06-03")
        );
        assert!(bi.peak_mem_mb >= 600, "高内存模型应在清单里体现");
    }

    /// 备选与排序：中英离线备选里 SenseVoice 排第一；流式中英有备选与旧版，旧版排最后。
    #[test]
    fn alternates_and_ordering() {
        let alts = alternates_for(Dimension::Bilingual, RecognitionMode::Offline);
        assert_eq!(alts[0].kind, ModelKind::OfflineSenseVoice);
        let all = list_for(Dimension::Bilingual, RecognitionMode::Streaming);
        assert_eq!(all.first().map(|s| s.role), Some(Role::Default));
        assert_eq!(all.last().map(|s| s.role), Some(Role::Legacy));
        assert_eq!(
            alternates_for(Dimension::Bilingual, RecognitionMode::Streaming).len(),
            1
        );
    }

    /// 离线模型都是离线类型，流式模型都是 online-transducer。
    #[test]
    fn kind_matches_mode() {
        for spec in &manifest().models {
            assert_eq!(
                spec.kind.is_offline(),
                spec.mode == RecognitionMode::Offline,
                "{}",
                spec.id
            );
        }
    }

    /// resolve：空 ID 取默认；备选 ID 命中；不属于该组合或未知的 ID 回落默认。
    #[test]
    fn resolve_rules() {
        use RecognitionMode::{Offline, Streaming};
        let def = resolve(Dimension::Zh, Streaming, "").expect("默认");
        assert_eq!(def.role, Role::Default);
        let alt = alternates_for(Dimension::Zh, Streaming)[0];
        assert_eq!(
            resolve(Dimension::Zh, Streaming, &alt.id).expect("备选").id,
            alt.id
        );
        // 备选 ID 换到别的维度 / 模式：回落默认
        assert_eq!(
            resolve(Dimension::En, Streaming, &alt.id)
                .expect("回落")
                .role,
            Role::Default
        );
        assert_eq!(
            resolve(Dimension::Zh, Offline, &alt.id).expect("回落").role,
            Role::Default
        );
        assert_eq!(
            resolve(Dimension::Zh, Streaming, "no-such")
                .expect("回落")
                .role,
            Role::Default
        );
        assert_eq!(find(&alt.id).map(|s| s.id.as_str()), Some(alt.id.as_str()));
        assert!(find("no-such").is_none());
    }

    /// 配置值解析：未知值取默认（中英混合、流式）。
    #[test]
    fn config_value_parsing() {
        assert_eq!(Dimension::from_config("zh"), Dimension::Zh);
        assert_eq!(Dimension::from_config("en"), Dimension::En);
        assert_eq!(Dimension::from_config("???"), Dimension::Bilingual);
        assert_eq!(mode_from_config("offline"), RecognitionMode::Offline);
        assert_eq!(mode_from_config("???"), RecognitionMode::Streaming);
        for d in Dimension::ALL {
            assert_eq!(Dimension::from_config(d.as_str()), d);
        }
    }

    /// 清单非法时报错：未知 kind、模式与类型不符、版本不符。
    #[test]
    fn invalid_manifests_are_rejected() {
        let good = MANIFEST_JSON;
        assert!(parse_manifest(good).is_ok());
        let bad_kind = good.replacen("online-transducer", "bogus", 1);
        assert!(parse_manifest(&bad_kind).is_err());
        let mismatch = good.replacen("\"mode\": \"streaming\"", "\"mode\": \"offline\"", 1);
        assert!(parse_manifest(&mismatch).unwrap_err().contains("不匹配"));
        let schema = good.replacen("\"schema\": 1", "\"schema\": 9", 1);
        assert!(parse_manifest(&schema).unwrap_err().contains("版本"));
    }

    /// 路径布局：模型目录在 models/stt/<id>，VAD 在其父目录。
    #[test]
    fn path_layout() {
        let root = Path::new("D:/data");
        let spec = default_for(Dimension::Zh, RecognitionMode::Offline).expect("默认");
        let dir = model_dir(spec, root);
        assert_eq!(dir.parent().expect("父目录"), models_root(root));
        assert_eq!(vad_path(root).parent().expect("父目录"), models_root(root));
        assert!(vad_path(root).ends_with("silero_vad.onnx"));
    }

    /// 发布前检查：统计 sha256 为空的条目并输出名单（仅提示，不失败）。
    #[test]
    fn report_unpinned_sha256() {
        let names = unpinned_names();
        println!("sha256 未固定的条目 {} 个: {names:?}", names.len());
    }

    /// 发布检查：断言所有条目 sha256 都已固定（平时忽略，发布前 `--ignored` 运行）。
    #[test]
    #[ignore = "发布前检查：sha256 必须全部固定"]
    fn release_requires_all_sha256_pinned() {
        let names = unpinned_names();
        assert!(names.is_empty(), "sha256 未固定: {names:?}");
    }

    /// 清单里 sha256 为空的条目名（含 VAD）。
    fn unpinned_names() -> Vec<String> {
        let m = manifest();
        let mut names: Vec<String> = m
            .models
            .iter()
            .filter(|s| s.archive.sha256.is_empty())
            .map(|s| s.id.clone())
            .collect();
        if m.vad.sha256.is_empty() {
            names.push(m.vad.name.clone());
        }
        names
    }
}
