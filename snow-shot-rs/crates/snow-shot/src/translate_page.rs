//! 翻译页（G07）的纯逻辑：源 / 目标语言、交换、按页配置、状态机、本地化引导文案。
//!
//! 翻译、复制、错误文案直接复用 [`crate::translate_input`]；窗口与控件见 [`crate::translate_page_view`]。
//! 不依赖 GPUI，全部可离屏单测。

use crate::language_names::endonym;
use crate::ocr_backend::i18n_for;
use crate::translate_input::{
    AUTO_CHOICE_ID, CopyState, InputError, PackChoice, Phase, TranslateInputModel,
};
use crate::translate_service::{Backend, SUPPORTED_TARGETS, TranslateConfig, Translated};
use snow_translate::Lang;

/// 源语言下拉里“自动检测”项的取值。
pub const AUTO_LANGUAGE_ID: &str = "auto";

/// 一个语言下拉选项。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LangChoice {
    /// 取值（`Lang::code` 的拼写，自动检测为 [`AUTO_LANGUAGE_ID`]）。
    pub id: String,
    /// 展示名（各语言自称，自动检测按界面语言）。
    pub label: String,
}

/// 页面支持的具体语言（与设置页目标语言下拉同一集合）。
pub fn page_languages() -> Vec<Lang> {
    SUPPORTED_TARGETS.iter().map(|(lang, _)| *lang).collect()
}

/// 语言选项列表。
///
/// # 参数
/// - `with_auto`：是否在最前面加“自动检测”（只有源语言用）。
/// - `locale`：界面语言。
pub fn language_choices(with_auto: bool, locale: &str) -> Vec<LangChoice> {
    let auto = with_auto.then(|| LangChoice {
        id: AUTO_LANGUAGE_ID.to_string(),
        label: i18n_for(locale).tr("translate-page-lang-auto"),
    });
    let concrete = page_languages().into_iter().map(|lang| LangChoice {
        id: lang.code().to_string(),
        label: endonym(lang.code()).unwrap_or(lang.code()).to_string(),
    });
    auto.into_iter().chain(concrete).collect()
}

/// 把配置里的语言收敛到页面集合：不在集合内的源语言回落到自动检测，目标语言回落到英语。
///
/// # 参数
/// - `lang`：配置里的语言。
/// - `is_source`：是否为源语言。
pub fn normalize_language(lang: Lang, is_source: bool) -> Lang {
    if is_source && lang == Lang::Auto {
        Lang::Auto
    } else if page_languages().contains(&lang) {
        lang
    } else if is_source {
        Lang::Auto
    } else {
        Lang::En
    }
}

/// 按页面选择生成本次请求的配置：覆盖源 / 目标语言，不写回全局设置。
///
/// # 参数
/// - `base`：全局翻译配置。
/// - `source` / `target`：页面上选的语言。
pub fn page_config(base: &TranslateConfig, source: Lang, target: Lang) -> TranslateConfig {
    let mut config = base.clone();
    config.source = source;
    config.target = target;
    config
}

/// 翻译页状态：语言、翻译包选择、输入与翻译阶段。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TranslatePageModel {
    /// 源语言（可为自动检测）。
    pub source: Lang,
    /// 目标语言。
    pub target: Lang,
    /// 选中的翻译包 ID（[`AUTO_CHOICE_ID`] 为自动）。
    pub pack_id: String,
    /// 已装本地翻译包数量。
    pub installed: usize,
    /// 当前后端（决定是否需要提示安装模型）。
    pub backend: Backend,
    /// 翻译阶段与复制状态（复用输入框翻译的状态机）。
    pub flow: TranslateInputModel,
}

impl TranslatePageModel {
    /// 由全局配置与已装包创建初始状态。
    ///
    /// # 参数
    /// - `config`：全局翻译配置（取默认语言与后端）。
    /// - `packs`：已装翻译包。
    pub fn new(config: &TranslateConfig, packs: &[PackChoice]) -> Self {
        Self {
            source: normalize_language(config.source, true),
            target: normalize_language(config.target, false),
            pack_id: AUTO_CHOICE_ID.to_string(),
            installed: packs.len(),
            backend: config.backend,
            flow: TranslateInputModel::default(),
        }
    }

    /// 能否交换语言：源为自动检测或两边相同时不行。
    pub fn can_swap(&self) -> bool {
        self.source != Lang::Auto && self.source != self.target
    }

    /// 交换源 / 目标语言；返回是否真的交换了。
    pub fn swap_languages(&mut self) -> bool {
        if !self.can_swap() {
            return false;
        }
        std::mem::swap(&mut self.source, &mut self.target);
        true
    }

    /// 设置源语言（代码拼写，未知值忽略）；返回是否变化。
    pub fn set_source(&mut self, code: &str) -> bool {
        match Lang::from_code(code).map(|l| normalize_language(l, true)) {
            Some(lang) if lang != self.source => {
                self.source = lang;
                true
            }
            _ => false,
        }
    }

    /// 设置目标语言（代码拼写，未知值或自动检测忽略）；返回是否变化。
    pub fn set_target(&mut self, code: &str) -> bool {
        match Lang::from_code(code) {
            Some(lang)
                if lang != self.target
                    && lang != Lang::Auto
                    && page_languages().contains(&lang) =>
            {
                self.target = lang;
                true
            }
            _ => false,
        }
    }

    /// 本地后端却没装任何翻译包：页面要给出安装引导。
    pub fn needs_model_guide(&self) -> bool {
        self.backend == Backend::Local && self.installed == 0
    }

    /// 本次请求用的配置（语言来自页面）。
    ///
    /// # 参数
    /// - `base`：全局翻译配置。
    pub fn request_config(&self, base: &TranslateConfig) -> TranslateConfig {
        page_config(base, self.source, self.target)
    }

    /// 开始翻译，规则同输入框翻译；返回请求序号。
    ///
    /// # 参数
    /// - `text`：输入原文。
    pub fn begin(&mut self, text: &str) -> Option<u64> {
        self.flow.begin(text)
    }

    /// 收到结果，过期序号忽略；返回是否采纳。
    pub fn finish(&mut self, serial: u64, result: Result<Translated, InputError>) -> bool {
        self.flow.finish(serial, result)
    }

    /// 当前可复制的译文。
    pub fn translation(&self) -> &str {
        self.flow.translation()
    }

    /// 复制译文并记录结果。
    pub fn copy_now(&mut self, copier: impl FnOnce(&str) -> Result<(), String>) {
        self.flow.copy_now(copier);
    }

    /// 是否在翻译中。
    pub fn is_busy(&self) -> bool {
        self.flow.is_busy()
    }

    /// 是否处于失败态。
    pub fn is_failed(&self) -> bool {
        matches!(self.flow.phase, Phase::Failed(_))
    }

    /// 底部状态行：翻译态文案优先，其次空闲时的安装引导。
    ///
    /// # 参数
    /// - `locale`：界面语言。
    pub fn status_line(&self, locale: &str) -> Option<String> {
        self.flow.status_line(locale).or_else(|| {
            (self.needs_model_guide() && self.flow.phase == Phase::Idle)
                .then(|| i18n_for(locale).tr("translate-page-no-model-guide"))
        })
    }

    /// 复制状态（供视图使用）。
    pub fn copy_state(&self) -> &CopyState {
        &self.flow.copy
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use snow_config::document::ConfigDocument;
    use snow_translate::TranslateError;

    /// 默认配置（本地后端）。
    fn base() -> TranslateConfig {
        TranslateConfig::from_document(&ConfigDocument::from_bytes(None), "zh-CN")
    }

    /// 无已装包的初始页面状态。
    fn model() -> TranslatePageModel {
        TranslatePageModel::new(&base(), &[])
    }

    #[test]
    fn language_choices_start_with_auto_only_for_source() {
        let src = language_choices(true, "zh-CN");
        assert_eq!(src[0].id, AUTO_LANGUAGE_ID);
        assert_eq!(src[0].label, "自动检测");
        let dst = language_choices(false, "zh-CN");
        assert_eq!(dst.len(), src.len() - 1);
        assert!(dst.iter().all(|c| c.id != AUTO_LANGUAGE_ID));
        assert!(dst.iter().any(|c| c.id == "ja" && c.label == "日本語"));
    }

    #[test]
    fn normalize_clamps_to_page_set() {
        assert_eq!(normalize_language(Lang::Ko, true), Lang::Auto);
        assert_eq!(normalize_language(Lang::Ko, false), Lang::En);
        assert_eq!(normalize_language(Lang::Auto, false), Lang::En);
        assert_eq!(normalize_language(Lang::Ja, false), Lang::Ja);
    }

    #[test]
    fn swap_rules() {
        let mut m = model();
        m.source = Lang::Auto;
        assert!(!m.swap_languages());
        m.source = Lang::En;
        m.target = Lang::En;
        assert!(!m.swap_languages());
        m.target = Lang::Ja;
        assert!(m.swap_languages());
        assert_eq!((m.source, m.target), (Lang::Ja, Lang::En));
    }

    #[test]
    fn set_language_validates() {
        let mut m = model();
        assert!(m.set_source("fr"));
        assert!(!m.set_source("fr"));
        assert!(!m.set_source("klingon"));
        assert!(m.set_source("auto"));
        assert!(!m.set_target("auto"));
        assert!(!m.set_target("ko"));
        assert!(m.set_target("de"));
        assert_eq!(m.target, Lang::De);
    }

    #[test]
    fn page_config_overrides_languages_only() {
        let mut m = model();
        m.source = Lang::Fr;
        m.target = Lang::Ja;
        let cfg = m.request_config(&base());
        assert_eq!((cfg.source, cfg.target), (Lang::Fr, Lang::Ja));
        assert_eq!(cfg.backend, base().backend);
    }

    #[test]
    fn guide_shown_only_when_local_without_packs() {
        let mut m = model();
        m.backend = Backend::Local;
        assert!(m.needs_model_guide());
        let en = m.status_line("en-US").unwrap();
        assert!(en.is_ascii() && !en.is_empty());
        assert_ne!(en, m.status_line("zh-CN").unwrap());
        m.installed = 1;
        assert!(m.status_line("en-US").is_none());
        m.installed = 0;
        m.backend = Backend::OpenAi;
        assert!(!m.needs_model_guide());
    }

    #[test]
    fn flow_busy_stale_and_copy() {
        let mut m = model();
        assert_eq!(m.begin("  "), None);
        assert!(m.is_failed());
        let serial = m.begin("hello").unwrap();
        assert!(m.is_busy());
        assert_eq!(m.begin("again"), None);
        assert!(!m.finish(serial + 1, Err(InputError::Empty)));
        let done = Translated {
            texts: vec!["你好".into()],
            label: "m".into(),
        };
        assert!(m.finish(serial, Ok(done)));
        assert_eq!(m.translation(), "你好");
        let mut got = String::new();
        m.copy_now(|t| {
            got = t.to_string();
            Ok(())
        });
        assert_eq!(got, "你好");
        assert_eq!(m.copy_state(), &CopyState::Copied);
    }

    #[test]
    fn failure_message_for_missing_model_is_localized() {
        let mut m = model();
        let serial = m.begin("x").unwrap();
        m.finish(
            serial,
            Err(InputError::Translate(TranslateError::NoModelFound(
                String::new(),
            ))),
        );
        let zh = m.status_line("zh-CN").unwrap();
        let en = m.status_line("en-US").unwrap();
        assert_ne!(zh, en);
        assert!(en.is_ascii());
    }
}
