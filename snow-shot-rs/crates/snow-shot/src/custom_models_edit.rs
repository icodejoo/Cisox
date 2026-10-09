//! 自定义 AI 模型编辑的纯逻辑：表单校验、增删改复制、ID 生成与下拉选项过滤。
//!
//! 数据结构与规范化复用 `snow_config::custom_models`；这里不依赖 GPUI，可离屏单测。
//! 密钥只存在于模型记录里，本模块的日志与错误文案都不带密钥。

use crate::model_catalog::ModelOption;
use serde_json::Value;
use snow_config::custom_models::{
    CustomAiModel, CustomAiModelUrlError, custom_ai_model_url_error, custom_ai_models_from_json,
    custom_ai_models_to_json, normalize_custom_ai_model,
};
use std::collections::hash_map::RandomState;
use std::hash::{BuildHasher, Hasher};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

/// 自定义 AI 模型列表所在的配置键（与转换引导共用同一个常量）。
pub use crate::conversion_guide::KEY_CUSTOM_MODELS;
/// UUID 第 7 字节的版本位（v4）。
const UUID_VERSION_4: u8 = 0x40;
/// UUID 第 9 字节的变体位。
const UUID_VARIANT: u8 = 0x80;

/// 编辑表单（界面输入的原始文本，保存前才校验与规范化）。
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ModelForm {
    /// 正在编辑的模型 ID；新建为 `None`。
    pub id: Option<String>,
    /// 显示名称。
    pub name: String,
    /// API URL（基础地址）。
    pub base_url: String,
    /// API 密钥。
    pub api_key: String,
    /// API 模型（服务端模型 ID）。
    pub model: String,
    /// 是否允许此模型把图像转换为 Markdown / HTML。
    pub supports_vision: bool,
    /// 是否支持推理（界面不提供开关，编辑时原样带回）。
    pub supports_reasoning: bool,
}

impl ModelForm {
    /// 由已有模型生成编辑表单。
    pub fn from_model(model: &CustomAiModel) -> Self {
        Self {
            id: Some(model.id.clone()),
            name: model.name.clone(),
            base_url: model.base_url.clone(),
            api_key: model.api_key.clone(),
            model: model.model.clone(),
            supports_vision: model.supports_vision,
            supports_reasoning: model.supports_reasoning,
        }
    }
}

/// 表单校验失败的原因。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FormError {
    /// 名称为空。
    NameEmpty,
    /// 与其它模型重名（忽略大小写）。
    NameDuplicate,
    /// 不是合法的 http / https 基础地址。
    UrlInvalid,
    /// 填成了完整端点（含 `/chat/completions`）。
    UrlFullEndpoint,
    /// 模型 ID 为空。
    ModelEmpty,
    /// 密钥含换行。
    KeyHasNewline,
}

impl FormError {
    /// 对应的提示文案 message id（复用旧版设置控件的文案）。
    pub fn message_id(self) -> &'static str {
        match self {
            Self::NameEmpty => "custom-ai-models-settings-widget-enter-a-model-name-eb245b70",
            Self::NameDuplicate => {
                "custom-ai-models-settings-widget-a-model-with-this-name-already-e-27b4597e"
            }
            Self::UrlInvalid => {
                "custom-ai-models-settings-widget-enter-an-http-or-https-base-url-d65f4849"
            }
            Self::UrlFullEndpoint => {
                "custom-ai-models-settings-widget-enter-the-base-url-without-chat-480cdd09"
            }
            Self::ModelEmpty => "custom-ai-models-settings-widget-enter-the-api-model-id-eb0be802",
            Self::KeyHasNewline => {
                "custom-ai-models-settings-widget-the-api-key-must-not-contain-lin-f5360197"
            }
        }
    }
}

/// 生成一个新的模型 ID（小写 v4 UUID）。
///
/// 没有引入随机数依赖：用每进程随机种子的哈希器混合时间戳与自增计数，只用于区分记录，不用于安全目的。
///
/// # 示例
/// ```ignore
/// assert_eq!(new_model_id().len(), 36);
/// ```
pub fn new_model_id() -> String {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos() as u64);
    let mut bytes = [0u8; 16];
    for chunk in bytes.chunks_mut(8) {
        let mut hasher = RandomState::new().build_hasher();
        hasher.write_u64(nanos);
        hasher.write_u64(COUNTER.fetch_add(1, Ordering::Relaxed));
        chunk.copy_from_slice(&hasher.finish().to_le_bytes());
    }
    bytes[6] = (bytes[6] & 0x0f) | UUID_VERSION_4;
    bytes[8] = (bytes[8] & 0x3f) | UUID_VARIANT;
    let hex: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
    format!(
        "{}-{}-{}-{}-{}",
        &hex[0..8],
        &hex[8..12],
        &hex[12..16],
        &hex[16..20],
        &hex[20..32]
    )
}

/// 校验表单并生成规范化后的模型（新建时分配新 ID）。
///
/// # 参数
/// - `models`：当前全部模型（查重名用）。
/// - `form`：表单。
///
/// # 返回
/// 通过时返回规范化的模型；否则返回第一个失败原因（顺序：名称、重名、地址、模型 ID、密钥）。
pub fn build_model(models: &[CustomAiModel], form: &ModelForm) -> Result<CustomAiModel, FormError> {
    let model = normalize_custom_ai_model(CustomAiModel {
        id: form.id.clone().unwrap_or_else(new_model_id),
        name: form.name.clone(),
        base_url: form.base_url.clone(),
        api_key: form.api_key.clone(),
        model: form.model.clone(),
        supports_vision: form.supports_vision,
        supports_reasoning: form.supports_reasoning,
    });
    if model.name.is_empty() {
        return Err(FormError::NameEmpty);
    }
    let folded = model.name.to_lowercase();
    if models
        .iter()
        .any(|m| m.id != model.id && m.name.to_lowercase() == folded)
    {
        return Err(FormError::NameDuplicate);
    }
    match custom_ai_model_url_error(&model.base_url) {
        CustomAiModelUrlError::None => {}
        CustomAiModelUrlError::InvalidBaseUrl => return Err(FormError::UrlInvalid),
        CustomAiModelUrlError::FullEndpoint => return Err(FormError::UrlFullEndpoint),
    }
    if model.model.is_empty() {
        return Err(FormError::ModelEmpty);
    }
    if model.api_key.contains(['\r', '\n']) {
        return Err(FormError::KeyHasNewline);
    }
    Ok(model)
}

/// 新增或按 ID 替换一条模型。
///
/// # 返回
/// 替换已有项返回 `true`，新增返回 `false`。
pub fn upsert(models: &mut Vec<CustomAiModel>, model: CustomAiModel) -> bool {
    match models.iter_mut().find(|m| m.id == model.id) {
        Some(slot) => {
            *slot = model;
            true
        }
        None => {
            models.push(model);
            false
        }
    }
}

/// 按 ID 删除；返回是否删到了。
pub fn remove(models: &mut Vec<CustomAiModel>, id: &str) -> bool {
    let before = models.len();
    models.retain(|m| m.id != id);
    models.len() != before
}

/// 复制一条模型：新 ID，名称依次尝试 `copy_name(1)`、`copy_name(2)`……直到不重名。
///
/// # 参数
/// - `models`：当前全部模型。
/// - `id`：被复制的模型 ID。
/// - `copy_name`：由序号（1 表示“（副本）”，≥2 表示“（副本 n）”）生成名称的函数。
///
/// # 返回
/// 复制出的模型（尚未加入列表）；源不存在返回 `None`。
pub fn duplicate(
    models: &[CustomAiModel],
    id: &str,
    copy_name: impl Fn(usize) -> String,
) -> Option<CustomAiModel> {
    let source = models.iter().find(|m| m.id == id)?;
    let taken = |name: &str| {
        let folded = name.to_lowercase();
        models.iter().any(|m| m.name.to_lowercase() == folded)
    };
    let name = (1..)
        .map(&copy_name)
        .find(|candidate| !taken(candidate))
        .unwrap_or_default();
    Some(CustomAiModel {
        id: new_model_id(),
        name,
        ..source.clone()
    })
}

/// 从配置值读出模型列表（非法项已被规范化丢弃）。
pub fn read_models(value: &Value) -> Vec<CustomAiModel> {
    custom_ai_models_from_json(value).0
}

/// 模型列表转配置值。
pub fn to_value(models: &[CustomAiModel]) -> Value {
    custom_ai_models_to_json(models)
}

/// 下拉选项：值是模型 ID，显示名是模型名称；`vision_only` 为真时只含支持视觉的模型。
///
/// # 示例
/// ```ignore
/// let options = pick_options(&models, true);
/// ```
pub fn pick_options(models: &[CustomAiModel], vision_only: bool) -> Vec<ModelOption> {
    models
        .iter()
        .filter(|m| !vision_only || m.supports_vision)
        .map(|m| ModelOption {
            value: m.id.clone(),
            label: m.name.clone(),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// 造一个模型。
    fn model(name: &str, vision: bool) -> CustomAiModel {
        CustomAiModel {
            id: new_model_id(),
            name: name.into(),
            base_url: "http://localhost:1234/v1".into(),
            api_key: "secret".into(),
            model: "m".into(),
            supports_vision: vision,
            supports_reasoning: false,
        }
    }

    /// 合法表单。
    fn form(name: &str) -> ModelForm {
        ModelForm {
            name: name.into(),
            base_url: "http://localhost:1234/v1/".into(),
            model: "gpt".into(),
            ..ModelForm::default()
        }
    }

    /// 新 ID 是小写 v4 UUID，且彼此不同；能通过配置层的规范化。
    #[test]
    fn new_ids_are_canonical_v4_and_unique() {
        let ids: Vec<String> = (0..64).map(|_| new_model_id()).collect();
        for id in &ids {
            assert_eq!(id.len(), 36);
            assert_eq!(id.as_bytes()[14], b'4', "{id}");
            assert!(
                matches!(id.as_bytes()[19], b'8' | b'9' | b'a' | b'b'),
                "{id}"
            );
            assert_eq!(id, &id.to_lowercase());
        }
        let unique: std::collections::HashSet<_> = ids.iter().collect();
        assert_eq!(unique.len(), ids.len());
        let value = to_value(&[model("a", false)]);
        assert_eq!(read_models(&value).len(), 1, "新 ID 应被配置层接受");
    }

    /// 校验：名称、重名（忽略大小写，编辑自己不算）、地址、完整端点、模型 ID、密钥换行。
    #[test]
    fn build_model_validates_in_order() {
        let existing = vec![model("Alpha", false)];
        assert_eq!(
            build_model(&existing, &form("  ")),
            Err(FormError::NameEmpty)
        );
        assert_eq!(
            build_model(&existing, &form("alpha")),
            Err(FormError::NameDuplicate)
        );
        let mut bad_url = form("n");
        bad_url.base_url = "ftp://x".into();
        assert_eq!(build_model(&existing, &bad_url), Err(FormError::UrlInvalid));
        bad_url.base_url = "https://a.com/v1/chat/completions".into();
        assert_eq!(
            build_model(&existing, &bad_url),
            Err(FormError::UrlFullEndpoint)
        );
        let mut no_model = form("n");
        no_model.model = " ".into();
        assert_eq!(
            build_model(&existing, &no_model),
            Err(FormError::ModelEmpty)
        );
        let mut key = form("n");
        key.api_key = "a\nb".into();
        assert_eq!(build_model(&existing, &key), Err(FormError::KeyHasNewline));
        let ok = build_model(&existing, &form(" New ")).expect("通过");
        assert_eq!(ok.name, "New");
        assert_eq!(ok.base_url, "http://localhost:1234/v1", "尾部斜杠被去掉");
        // 编辑自己：同名不算重名
        let mut edit = ModelForm::from_model(&existing[0]);
        edit.model = "other".into();
        assert!(build_model(&existing, &edit).is_ok());
        assert!(
            FormError::NameEmpty
                .message_id()
                .starts_with("custom-ai-models-settings-widget-")
        );
    }

    /// 增删改：upsert 新增 / 替换，remove 按 ID 删。
    #[test]
    fn upsert_and_remove() {
        let mut models = Vec::new();
        let a = model("a", false);
        assert!(!upsert(&mut models, a.clone()));
        let mut changed = a.clone();
        changed.name = "renamed".into();
        assert!(upsert(&mut models, changed));
        assert_eq!(models.len(), 1);
        assert_eq!(models[0].name, "renamed");
        assert!(remove(&mut models, &a.id));
        assert!(!remove(&mut models, &a.id));
        assert!(models.is_empty());
    }

    /// 复制：新 ID、名称依次避开重名，其余字段照抄。
    #[test]
    fn duplicate_picks_unique_copy_name() {
        let a = model("Alpha", true);
        let mut models = vec![a.clone()];
        let name_of = |n: usize| {
            if n == 1 {
                "Alpha (Copy)".to_string()
            } else {
                format!("Alpha (Copy {n})")
            }
        };
        let first = duplicate(&models, &a.id, name_of).expect("复制");
        assert_eq!(first.name, "Alpha (Copy)");
        assert_ne!(first.id, a.id);
        assert!(first.supports_vision && first.api_key == a.api_key);
        models.push(first);
        let second = duplicate(&models, &a.id, name_of).expect("再复制");
        assert_eq!(second.name, "Alpha (Copy 2)");
        assert!(duplicate(&models, "missing", name_of).is_none());
    }

    /// 下拉选项：值是 ID、显示名是名称；视觉下拉只含支持视觉的模型；空列表为空。
    #[test]
    fn pick_options_filter_vision() {
        let models = vec![model("text", false), model("vision", true)];
        let all = pick_options(&models, false);
        assert_eq!(all.len(), 2);
        assert_eq!(all[0].label, "text");
        assert_eq!(all[0].value, models[0].id);
        let vision = pick_options(&models, true);
        assert_eq!(vision.len(), 1);
        assert_eq!(vision[0].label, "vision");
        assert!(pick_options(&[], true).is_empty());
    }

    /// 配置往返：写出的值能被读回；非法项读取时被丢弃（旧配置兼容）。
    #[test]
    fn config_roundtrip_and_salvage() {
        let models = vec![model("a", false), model("b", true)];
        let value = to_value(&models);
        assert_eq!(read_models(&value), models);
        let mut broken = value.as_array().cloned().unwrap_or_default();
        broken.push(json!({"name": "no-id"}));
        assert_eq!(read_models(&Value::Array(broken)).len(), 2);
        assert!(read_models(&json!("oops")).is_empty());
    }
}
