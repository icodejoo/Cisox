//! Markdown / HTML 转换的“未配置”引导：转换走用户自己配置的聊天模型（ADR-5 的自定义模型通道），
//! 这里只判断当前配置状态并给出引导文案，**不发起任何模型调用**。

use serde_json::Value;
use snow_config::custom_models::custom_ai_models_from_json;
use snow_config::document::ConfigDocument;
use snow_i18n::{Args, I18n};

/// 配置键：转换用的视觉模型 ID（空表示没选）。
pub const KEY_VISION_MODEL: &str = "screenshot_conversion/vision_model";
/// 配置键：自定义 AI 模型列表。
pub const KEY_CUSTOM_MODELS: &str = "api_configuration/custom_models";

/// 转换通道的配置状态。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConversionGuide {
    /// 没有选转换模型。
    NotConfigured,
    /// 选中的模型已被删除或不存在。
    ModelMissing,
    /// 选中的模型不支持视觉输入。
    NoVision(String),
    /// 已配置（附模型显示名）；调用尚未开放。
    Ready(String),
}

impl ConversionGuide {
    /// 从配置文档判断状态。
    ///
    /// # 参数
    /// - `document`：配置文档。
    ///
    /// ```ignore
    /// let guide = ConversionGuide::from_document(store.document());
    /// ```
    pub fn from_document(document: &ConfigDocument) -> Self {
        let id = match document.value(KEY_VISION_MODEL) {
            Value::String(s) => s.trim().to_string(),
            _ => String::new(),
        };
        let (models, _) = custom_ai_models_from_json(&document.value(KEY_CUSTOM_MODELS));
        Self::from_selection(&id, &models)
    }

    /// 由选中的模型 ID 与模型列表判断状态。
    ///
    /// # 参数
    /// - `id`：选中的模型 ID（空表示没选）。
    /// - `models`：已配置的自定义模型。
    pub fn from_selection(id: &str, models: &[snow_config::custom_models::CustomAiModel]) -> Self {
        if id.is_empty() {
            return Self::NotConfigured;
        }
        match models.iter().find(|m| m.id == id) {
            None => Self::ModelMissing,
            Some(m) if !m.supports_vision => Self::NoVision(m.name.clone()),
            Some(m) => Self::Ready(m.name.clone()),
        }
    }

    /// 面向用户的引导文案。
    ///
    /// # 参数
    /// - `i18n`：界面语料。
    pub fn message(&self, i18n: &I18n) -> String {
        match self {
            Self::NotConfigured => i18n.tr("conversion-guide-not-configured"),
            Self::ModelMissing => i18n.tr("conversion-guide-model-missing"),
            Self::NoVision(name) => i18n.tr_with(
                "conversion-guide-no-vision",
                &Args::new().named("name", name.as_str()),
            ),
            Self::Ready(name) => i18n.tr_with(
                "conversion-guide-ready",
                &Args::new().named("name", name.as_str()),
            ),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use snow_config::custom_models::CustomAiModel;

    /// 构造一条模型。
    fn model(id: &str, vision: bool) -> CustomAiModel {
        CustomAiModel {
            id: id.into(),
            name: format!("name-{id}"),
            base_url: "http://localhost:1234/v1".into(),
            api_key: String::new(),
            model: "m".into(),
            supports_vision: vision,
            supports_reasoning: false,
        }
    }

    /// 四种状态都能区分。
    #[test]
    fn states() {
        let models = [model("a", true), model("b", false)];
        assert_eq!(
            ConversionGuide::from_selection("", &models),
            ConversionGuide::NotConfigured
        );
        assert_eq!(
            ConversionGuide::from_selection("zzz", &models),
            ConversionGuide::ModelMissing
        );
        assert_eq!(
            ConversionGuide::from_selection("b", &models),
            ConversionGuide::NoVision("name-b".into())
        );
        assert_eq!(
            ConversionGuide::from_selection("a", &models),
            ConversionGuide::Ready("name-a".into())
        );
    }

    /// 每种状态的文案两种语言都有，未配置时指向设置。
    #[test]
    fn messages_are_localized() {
        let zh = crate::ocr_backend::i18n_for("zh-CN");
        let en = crate::ocr_backend::i18n_for("en-US");
        for guide in [
            ConversionGuide::NotConfigured,
            ConversionGuide::ModelMissing,
            ConversionGuide::NoVision("m".into()),
            ConversionGuide::Ready("m".into()),
        ] {
            assert!(!guide.message(zh).is_empty());
            assert!(guide.message(en).is_ascii(), "{guide:?}");
        }
        assert!(ConversionGuide::NotConfigured.message(zh).contains("设置"));
    }
}
