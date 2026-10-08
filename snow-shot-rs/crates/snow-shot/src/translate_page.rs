//! 翻译页（G07）的纯逻辑：源 / 目标语言、交换、按页配置、状态机、本地化引导文案。
//!
//! 翻译、复制、错误文案直接复用 [`crate::translate_input`]；窗口与控件见 [`crate::translate_page_view`]。
//! 不依赖 GPUI，全部可离屏单测。

use crate::language_names::endonym;
use crate::ocr_backend::i18n_for;
use crate::translate_history::{HistoryEntry, TranslateHistory};
use crate::translate_input::{
    AUTO_CHOICE_ID, CopyState, InputError, PackChoice, Phase, TranslateInputModel,
};
use crate::translate_service::{Backend, SUPPORTED_TARGETS, TranslateConfig, Translated};
use snow_translate::Lang;

/// 配置键：输入变化后是否自动翻译（默认关）。
pub use snow_config::extensions::KEY_PAGE_AUTO_TRANSLATE;
/// 自动翻译的防抖时长（毫秒，与旧版翻译页一致）。
pub const AUTO_TRANSLATE_DEBOUNCE_MS: u64 = 1500;

/// 防抖到点时对“要不要翻译”的裁决。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AutoDecision {
    /// 立即发起翻译。
    Run,
    /// 正忙：已排队，等当前翻译结束后再判断。
    Queued,
    /// 不用翻译（过期、已关闭、空文本或与上次相同）。
    Skip,
}

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
    /// 翻译历史（新的在前）。
    pub history: TranslateHistory,
    /// 历史自上次落盘后是否有变化。
    history_dirty: bool,
    /// 是否开启输入变化后自动翻译。
    auto: bool,
    /// 防抖代数：每次输入变化加一，到点时代数不符说明又有新输入。
    debounce_gen: u64,
    /// 最近一次发出翻译的原文（防止对同一原文重复翻译）。
    last_text: String,
    /// 防抖到点时正忙而排队的自动翻译。
    queued: bool,
    /// 进行中请求的原文与语言对，完成后据此记历史。
    in_flight: Option<InFlight>,
}

/// 进行中的请求快照。
#[derive(Debug, Clone, PartialEq, Eq)]
struct InFlight {
    /// 原文。
    text: String,
    /// 源语言。
    source: Lang,
    /// 目标语言。
    target: Lang,
}

/// 语言的代码拼写（自动检测为 [`AUTO_LANGUAGE_ID`]）。
fn lang_code(lang: Lang) -> String {
    if lang == Lang::Auto {
        AUTO_LANGUAGE_ID.to_string()
    } else {
        lang.code().to_string()
    }
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
            history: TranslateHistory::default(),
            history_dirty: false,
            auto: false,
            debounce_gen: 0,
            last_text: String::new(),
            queued: false,
            in_flight: None,
        }
    }

    /// 换上从磁盘读到的历史（不算“有变化”）。
    ///
    /// # 参数
    /// - `history`：读到的历史。
    pub fn set_history(&mut self, history: TranslateHistory) {
        self.history = history;
        self.history_dirty = false;
    }

    /// 取走“历史有变化”标记；返回真时调用方应落盘。
    pub fn take_history_dirty(&mut self) -> bool {
        std::mem::take(&mut self.history_dirty)
    }

    /// 清空历史并标记需要落盘。
    pub fn clear_history(&mut self) {
        if !self.history.is_empty() {
            self.history.clear();
            self.history_dirty = true;
        }
    }

    /// 按历史记录回填：恢复语言对并直接显示当时的译文；翻译进行中忽略。
    ///
    /// # 参数
    /// - `index`：历史下标（0 为最近）。
    ///
    /// # 返回
    /// 回填的原文；下标越界或正忙时返回 `None`，由视图把原文放回输入框。
    pub fn recall(&mut self, index: usize) -> Option<String> {
        if self.is_busy() {
            return None;
        }
        let entry = self.history.get(index)?.clone();
        self.source = Lang::from_code(&entry.source_lang)
            .map(|l| normalize_language(l, true))
            .unwrap_or(self.source);
        self.target = Lang::from_code(&entry.target_lang)
            .map(|l| normalize_language(l, false))
            .unwrap_or(self.target);
        self.flow.phase = Phase::Done {
            text: entry.translation,
            label: entry.label,
        };
        self.flow.copy = CopyState::Pending;
        // 回填的原文已有译文，不要再被自动翻译重复触发
        self.last_text = entry.source_text.clone();
        self.queued = false;
        self.debounce_gen += 1;
        Some(entry.source_text)
    }

    /// 是否开启自动翻译。
    pub fn auto_enabled(&self) -> bool {
        self.auto
    }

    /// 开关自动翻译；关闭时作废已排的防抖与排队。
    ///
    /// # 参数
    /// - `enabled`：新的开关值（来自配置）。
    pub fn set_auto(&mut self, enabled: bool) {
        self.auto = enabled;
        if !enabled {
            self.debounce_gen += 1;
            self.queued = false;
        }
    }

    /// 输入框内容变化：决定是否启动一轮防抖。
    ///
    /// # 参数
    /// - `text`：当前输入。
    ///
    /// # 返回
    /// 需要防抖时返回本轮代数（到点时交给 [`Self::debounce_due`]）；不需要返回 `None`。
    pub fn input_changed(&mut self, text: &str) -> Option<u64> {
        self.debounce_gen += 1;
        self.queued = false;
        (self.auto && !text.trim().is_empty() && text != self.last_text)
            .then_some(self.debounce_gen)
    }

    /// 防抖到点：判断要不要发起翻译。
    ///
    /// # 参数
    /// - `generation`：启动防抖时拿到的代数。
    /// - `text`：此刻的输入。
    pub fn debounce_due(&mut self, generation: u64, text: &str) -> AutoDecision {
        if generation != self.debounce_gen
            || !self.auto
            || text.trim().is_empty()
            || text == self.last_text
        {
            return AutoDecision::Skip;
        }
        if self.is_busy() {
            self.queued = true;
            return AutoDecision::Queued;
        }
        AutoDecision::Run
    }

    /// 当前翻译结束后，取走排队的自动翻译；返回真表示应对 `text` 再发一轮。
    ///
    /// # 参数
    /// - `text`：此刻的输入。
    pub fn take_queued(&mut self, text: &str) -> bool {
        let wanted = self.queued && self.auto && !text.trim().is_empty() && text != self.last_text;
        self.queued = false;
        wanted
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
        let serial = self.flow.begin(text)?;
        self.last_text = text.to_string();
        self.in_flight = Some(InFlight {
            text: text.to_string(),
            source: self.source,
            target: self.target,
        });
        Some(serial)
    }

    /// 收到结果，过期序号忽略；返回是否采纳。成功的译文会记入历史。
    pub fn finish(&mut self, serial: u64, result: Result<Translated, InputError>) -> bool {
        let label = result.as_ref().map(|r| r.label.clone()).unwrap_or_default();
        if !self.flow.finish(serial, result) {
            return false;
        }
        if let (Some(request), Phase::Done { text, .. }) = (self.in_flight.take(), &self.flow.phase)
            && self.history.push(HistoryEntry {
                source_lang: lang_code(request.source),
                target_lang: lang_code(request.target),
                source_text: request.text,
                translation: text.clone(),
                label,
            })
        {
            self.history_dirty = true;
        }
        true
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

    /// 造一次成功的翻译结果。
    fn done(text: &str) -> Result<Translated, InputError> {
        Ok(Translated {
            texts: vec![text.into()],
            label: "pack".into(),
        })
    }

    /// 翻译成功后记入历史并置脏标记；失败不记；取走标记后恢复干净。
    #[test]
    fn success_is_recorded_in_history() {
        let mut m = model();
        m.set_source("fr");
        m.set_target("ja");
        let serial = m.begin("bonjour").unwrap();
        assert!(m.finish(serial, done("こんにちは")));
        assert_eq!(m.history.len(), 1);
        let e = m.history.get(0).unwrap();
        assert_eq!(
            (e.source_lang.as_str(), e.target_lang.as_str()),
            ("fr", "ja")
        );
        assert_eq!(
            (e.source_text.as_str(), e.label.as_str()),
            ("bonjour", "pack")
        );
        assert!(m.take_history_dirty());
        assert!(!m.take_history_dirty());
        let serial = m.begin("x").unwrap();
        m.finish(serial, Err(InputError::Empty));
        assert_eq!(m.history.len(), 1);
        assert!(!m.take_history_dirty());
    }

    /// 自动检测的源语言以 `auto` 记录；过期结果不进历史。
    #[test]
    fn auto_source_recorded_and_stale_ignored() {
        let mut m = model();
        let serial = m.begin("hi").unwrap();
        assert!(!m.finish(serial + 5, done("你好")));
        assert!(m.history.is_empty());
        m.finish(serial, done("你好"));
        assert_eq!(m.history.get(0).unwrap().source_lang, AUTO_LANGUAGE_ID);
    }

    /// 点历史回填：恢复语言对与译文、不会被自动翻译重复触发；正忙或越界时忽略。
    #[test]
    fn recall_restores_state() {
        let mut m = model();
        m.set_auto(true);
        m.set_source("fr");
        m.set_target("ja");
        let serial = m.begin("bonjour").unwrap();
        m.finish(serial, done("こんにちは"));
        m.set_source("auto");
        m.set_target("en");
        m.flow.phase = Phase::Idle;
        assert_eq!(m.recall(0).as_deref(), Some("bonjour"));
        assert_eq!((m.source, m.target), (Lang::Fr, Lang::Ja));
        assert_eq!(m.translation(), "こんにちは");
        assert_eq!(m.input_changed("bonjour"), None, "回填的原文不再自动翻译");
        assert_eq!(m.recall(9), None);
        m.begin("busy now").unwrap();
        assert_eq!(m.recall(0), None);
    }

    /// 清空历史只在有内容时置脏。
    #[test]
    fn clear_history_marks_dirty_once() {
        let mut m = model();
        m.clear_history();
        assert!(!m.take_history_dirty());
        let serial = m.begin("a").unwrap();
        m.finish(serial, done("A"));
        m.take_history_dirty();
        m.clear_history();
        assert!(m.history.is_empty() && m.take_history_dirty());
    }

    /// 自动翻译默认关：输入变化不启动防抖；开启后空文本 / 与上次相同的文本也不启动。
    #[test]
    fn auto_translate_gating() {
        let mut m = model();
        assert!(!m.auto_enabled());
        assert_eq!(m.input_changed("hello"), None);
        m.set_auto(true);
        assert!(m.input_changed("hello").is_some());
        assert_eq!(m.input_changed("   "), None);
        let serial = m.begin("hello").unwrap();
        m.finish(serial, done("你好"));
        assert_eq!(m.input_changed("hello"), None);
        assert!(m.input_changed("hello!").is_some());
    }

    /// 防抖：新输入作废旧代数；到点且空闲才 Run；关闭开关后 Skip。
    #[test]
    fn debounce_supersedes_and_runs_once() {
        let mut m = model();
        m.set_auto(true);
        let first = m.input_changed("he").unwrap();
        let second = m.input_changed("hel").unwrap();
        assert_ne!(first, second);
        assert_eq!(m.debounce_due(first, "hel"), AutoDecision::Skip);
        assert_eq!(m.debounce_due(second, "hel"), AutoDecision::Run);
        let third = m.input_changed("hello").unwrap();
        m.set_auto(false);
        assert_eq!(m.debounce_due(third, "hello"), AutoDecision::Skip);
    }

    /// 正忙时到点的自动翻译排队，当前翻译结束后补一轮（文本没变化则不补）。
    #[test]
    fn busy_debounce_is_queued_then_replayed() {
        let mut m = model();
        m.set_auto(true);
        let serial = m.begin("first").unwrap();
        let generation = m.input_changed("second").unwrap();
        assert_eq!(m.debounce_due(generation, "second"), AutoDecision::Queued);
        m.finish(serial, done("一"));
        assert!(m.take_queued("second"));
        assert!(!m.take_queued("second"), "排队只取一次");
        // 排队后文本又变回已翻译的原文：不用补
        let g = m.input_changed("third").unwrap();
        let serial = m.begin("third").unwrap();
        assert_eq!(m.debounce_due(g, "third"), AutoDecision::Skip);
        m.finish(serial, done("三"));
        assert!(!m.take_queued("third"));
    }
}
