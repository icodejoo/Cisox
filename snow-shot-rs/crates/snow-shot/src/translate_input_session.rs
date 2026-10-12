//! 输入框翻译浮窗的会话状态：语言对、翻译包选择、防抖自动翻译、交换与清空。
//!
//! 翻译阶段、复制与错误文案复用 [`crate::translate_input`]，防抖复用翻译页的 [`AutoTranslate`]，
//! 语言选项复用翻译页的 [`choices_for`]；这里只补浮窗特有的规则（语言集合、自动检测结果参与交换）。
//! 不依赖 GPUI，全部可离屏单测；窗口与控件见 [`crate::translate_input_view`]。

use crate::translate_input::{InputError, TranslateInputModel};
use crate::translate_page::{
    AUTO_LANGUAGE_ID, AutoDecision, AutoTranslate, LangChoice, choices_for, lang_code,
};
use crate::translate_service::{TOOLBAR_TARGET_LANGS, Translated};
use snow_translate::Lang;
use snow_translate::script_split::ScriptSplitter;
use snow_translate::segment::{dominant_language, same_language};

/// 浮窗里可选的具体语言（与截图翻译二级菜单同一份列表，源 / 目标共用）。
pub fn input_languages() -> &'static [Lang] {
    &TOOLBAR_TARGET_LANGS
}

/// 源语言下拉选项：第一项「自动检测」，其后是 [`input_languages`]。
///
/// # 参数
/// - `locale`：界面语言（只影响「自动检测」的文案，其余为各语言自称）。
pub fn source_choices(locale: &str) -> Vec<LangChoice> {
    choices_for(input_languages(), true, locale)
}

/// 目标语言下拉选项：[`input_languages`]，不含自动检测。
///
/// # 参数
/// - `locale`：界面语言。
pub fn target_choices(locale: &str) -> Vec<LangChoice> {
    choices_for(input_languages(), false, locale)
}

/// 输入框里按下回车时是否立即翻译：Enter 翻译，Shift+Enter 留给换行。
///
/// # 参数
/// - `shift`：是否同时按着 Shift。
///
/// # 返回
/// 为真表示应立即翻译。
///
/// # 示例
/// ```ignore
/// assert!(enter_submits(false));
/// assert!(!enter_submits(true));
/// ```
pub fn enter_submits(shift: bool) -> bool {
    !shift
}

/// 把语言收敛进浮窗的语言集合（简繁视为同一种，取集合里的写法）；不在集合内返回 `None`。
///
/// # 参数
/// - `lang`：任意语言。
fn in_list(lang: Lang) -> Option<Lang> {
    input_languages()
        .iter()
        .copied()
        .find(|candidate| same_language(*candidate, lang))
}

/// 识别一段文本的主导语言（与翻译引擎判定源语言用同一套规则），并收敛进浮窗的语言集合。
///
/// # 参数
/// - `text`：原文。
///
/// # 返回
/// 识别出且在集合内的语言；纯数字标点或集合外的语言返回 `None`。
///
/// # 示例
/// ```ignore
/// assert_eq!(detect_language("你好，世界"), Some(Lang::ZhHans));
/// assert_eq!(detect_language("123"), None);
/// ```
pub fn detect_language(text: &str) -> Option<Lang> {
    dominant_language(&ScriptSplitter, text).and_then(in_list)
}

/// 输入框翻译浮窗的会话状态。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TranslateInputSession {
    /// 源语言（可为自动检测）。
    pub source: Lang,
    /// 目标语言。
    pub target: Lang,
    /// 选中的翻译包 ID（`AUTO_CHOICE_ID` 为沿用全局路由），只影响本次窗口内的请求。
    pub pack_id: String,
    /// 翻译阶段与复制状态。
    pub flow: TranslateInputModel,
    /// 防抖自动翻译（浮窗里恒开启）。
    auto: AutoTranslate,
    /// 源语言为自动检测时，最近一次翻译成功后识别出的源语言。
    detected: Option<Lang>,
    /// 进行中请求的原文（完成后据此识别源语言）。
    in_flight: Option<String>,
}

impl TranslateInputSession {
    /// 创建会话：源语言为自动检测，目标语言取给定默认值（不在语言集合内时退回英语）。
    ///
    /// # 参数
    /// - `target`：默认目标语言（一般是截图翻译设置里的目标语言）。
    ///
    /// # 示例
    /// ```ignore
    /// let session = TranslateInputSession::new(Lang::Ja);
    /// assert_eq!((session.source, session.target), (Lang::Auto, Lang::Ja));
    /// ```
    pub fn new(target: Lang) -> Self {
        Self {
            source: Lang::Auto,
            target: in_list(target).unwrap_or(Lang::En),
            pack_id: crate::translate_input::AUTO_CHOICE_ID.to_string(),
            flow: TranslateInputModel::default(),
            auto: AutoTranslate::new(true),
            detected: None,
            in_flight: None,
        }
    }

    /// 实际参与交换的源语言：手选的语言；自动检测时取识别结果，识别不出为 `None`。
    pub fn effective_source(&self) -> Option<Lang> {
        if self.source == Lang::Auto {
            self.detected
        } else {
            Some(self.source)
        }
    }

    /// 设置翻译包（空串为沿用全局路由）；返回是否变化。
    ///
    /// # 参数
    /// - `id`：下拉取值。
    pub fn set_pack(&mut self, id: &str) -> bool {
        if self.pack_id == id {
            return false;
        }
        self.pack_id = id.to_string();
        true
    }

    /// 切换源语言 / 目标语言 / 翻译包后重新翻译：输入非空就立即发起（不走防抖），
    /// 正在翻译也以最新选择为准，旧请求的结果按序号丢弃。
    ///
    /// # 参数
    /// - `text`：当前输入。
    ///
    /// # 返回
    /// 需要发请求时返回请求序号；输入为空返回 `None`，不发请求。
    pub fn retranslate(&mut self, text: &str) -> Option<u64> {
        let serial = self.flow.restart(text)?;
        self.auto.mark_sent(text);
        self.auto.cancel();
        self.in_flight = Some(text.to_string());
        Some(serial)
    }

    /// 设置源语言（代码拼写，`auto` 为自动检测；集合外的值忽略）；返回是否变化。
    ///
    /// # 参数
    /// - `code`：下拉取值。
    pub fn set_source(&mut self, code: &str) -> bool {
        let lang = if code == AUTO_LANGUAGE_ID {
            Some(Lang::Auto)
        } else {
            Lang::from_code(code).and_then(in_list)
        };
        match lang {
            Some(lang) if lang != self.source => {
                self.source = lang;
                self.detected = None;
                true
            }
            _ => false,
        }
    }

    /// 设置目标语言（代码拼写，集合外的值与自动检测忽略）；返回是否变化。
    ///
    /// # 参数
    /// - `code`：下拉取值。
    pub fn set_target(&mut self, code: &str) -> bool {
        match Lang::from_code(code).and_then(in_list) {
            Some(lang) if lang != self.target => {
                self.target = lang;
                true
            }
            _ => false,
        }
    }

    /// 能否交换：不在翻译中，且有可用的源语言（手选，或自动检测已识别）并与目标语言不同。
    pub fn can_swap(&self) -> bool {
        !self.flow.is_busy()
            && self
                .effective_source()
                .is_some_and(|source| !same_language(source, self.target))
    }

    /// 交换源 / 目标语言，并取出当前译文供视图放回输入框重新翻译。
    ///
    /// # 返回
    /// 不能交换返回 `None`；否则返回当前译文（还没有译文时为空串，视图此时只换语言不发请求）。
    pub fn swap(&mut self) -> Option<String> {
        if !self.can_swap() {
            return None;
        }
        let from = self.effective_source()?;
        let moved = self.flow.translation().to_string();
        self.source = self.target;
        self.target = from;
        self.detected = None;
        Some(moved)
    }

    /// 清空输入与译文：回到空闲态，作废进行中的请求与已排的防抖；语言选择保留。
    pub fn clear(&mut self) {
        self.flow.reset();
        self.detected = None;
        self.in_flight = None;
        self.auto.mark_synced("");
    }

    /// 输入框内容变化：返回需要启动的防抖代数（到点交给 [`Self::debounce_due`]）。
    ///
    /// # 参数
    /// - `text`：当前输入。
    pub fn input_changed(&mut self, text: &str) -> Option<u64> {
        self.auto.input_changed(text)
    }

    /// 防抖到点：判断要不要发起翻译。
    ///
    /// # 参数
    /// - `generation`：启动防抖时拿到的代数。
    /// - `text`：此刻的输入。
    pub fn debounce_due(&mut self, generation: u64, text: &str) -> AutoDecision {
        let busy = self.flow.is_busy();
        self.auto.debounce_due(generation, text, busy)
    }

    /// 当前翻译结束后，取走排队的自动翻译；返回真表示应对 `text` 再发一轮。
    ///
    /// # 参数
    /// - `text`：此刻的输入。
    pub fn take_queued(&mut self, text: &str) -> bool {
        self.auto.take_queued(text)
    }

    /// 开始翻译（Enter、防抖到点、交换后都走这里）。
    ///
    /// # 参数
    /// - `text`：输入原文。
    ///
    /// # 返回
    /// 通过则返回请求序号；输入为空（进入失败态）或正忙时返回 `None`。
    pub fn begin(&mut self, text: &str) -> Option<u64> {
        let serial = self.flow.begin(text)?;
        self.auto.mark_sent(text);
        self.in_flight = Some(text.to_string());
        Some(serial)
    }

    /// 收到翻译结果；序号过期返回 `false`。成功且源语言为自动检测时顺带记下识别结果。
    ///
    /// # 参数
    /// - `serial`：结果对应的请求序号。
    /// - `result`：译文或失败原因。
    pub fn finish(&mut self, serial: u64, result: Result<Translated, InputError>) -> bool {
        let succeeded = result.is_ok();
        if !self.flow.finish(serial, result) {
            return false;
        }
        let text = self.in_flight.take();
        self.detected = match text {
            Some(text) if succeeded && self.source == Lang::Auto => detect_language(&text),
            _ => None,
        };
        true
    }

    /// 源语言下拉应显示的取值（自动检测为 `auto`）。
    pub fn source_code(&self) -> &'static str {
        lang_code(self.source)
    }

    /// 目标语言下拉应显示的取值。
    pub fn target_code(&self) -> &'static str {
        lang_code(self.target)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::translate_input::{AUTO_CHOICE_ID, CopyState, Phase};
    use snow_config::document::ConfigDocument;
    use snow_translate::TranslateError;

    /// 造一次成功的翻译结果。
    fn done(text: &str) -> Result<Translated, InputError> {
        Ok(Translated {
            texts: vec![text.into()],
            label: "pack".into(),
        })
    }

    /// 完成一轮翻译：`begin` 后立刻返回结果。
    fn translate(session: &mut TranslateInputSession, text: &str, out: &str) {
        let serial = session.begin(text).expect("应能开始翻译");
        assert!(session.finish(serial, done(out)));
    }

    /// 源语言列表 = 自动检测 + 14 种；目标语言 = 14 种且不含自动检测；都用各语言自称。
    #[test]
    fn language_lists_have_auto_and_fourteen() {
        let source = source_choices("zh-CN");
        let target = target_choices("zh-CN");
        assert_eq!(source.len(), 15);
        assert_eq!(target.len(), 14);
        assert_eq!(source[0].id, AUTO_LANGUAGE_ID);
        assert_eq!(source[0].label, "自动检测");
        assert_eq!(source_choices("en-US")[0].label, "Auto detect");
        assert!(target.iter().all(|c| c.id != AUTO_LANGUAGE_ID));
        assert_eq!(source[1..], target[..]);
        let codes: Vec<&str> = target.iter().map(|c| c.id.as_str()).collect();
        assert_eq!(
            codes,
            [
                "zh-CN", "en", "fr", "es", "ru", "ar", "de", "ja", "ko", "pt", "it", "tr", "vi",
                "id"
            ]
        );
        let label = |code: &str| target.iter().find(|c| c.id == code).unwrap().label.clone();
        assert_eq!(label("ja"), "日本語");
        assert_eq!(label("ko"), "한국어");
        assert_eq!(label("vi"), "Tiếng Việt");
        assert_eq!(label("zh-CN"), "简体中文");
    }

    /// 默认语言：源为自动检测，目标取截图翻译设置的目标语言；集合外的目标退回英语。
    #[test]
    fn defaults_follow_screenshot_translation_target() {
        let config = crate::translate_service::TranslateConfig::from_document(
            &ConfigDocument::from_bytes(None),
            "ja-JP",
        );
        let session = TranslateInputSession::new(config.target);
        assert_eq!((session.source, session.target), (Lang::Auto, Lang::Ja));
        assert_eq!(session.pack_id, AUTO_CHOICE_ID);
        assert_eq!(
            (session.source_code(), session.target_code()),
            ("auto", "ja")
        );
        assert_eq!(TranslateInputSession::new(Lang::Auto).target, Lang::En);
        assert_eq!(
            TranslateInputSession::new(Lang::ZhHant).target,
            Lang::ZhHans
        );
        assert_eq!(TranslateInputSession::new(Lang::Ko).target, Lang::Ko);
    }

    /// 设置语言：集合外、未知值、自动检测作目标都被忽略；相同值不算变化。
    #[test]
    fn set_language_validates() {
        let mut s = TranslateInputSession::new(Lang::En);
        assert!(s.set_source("ko"));
        assert!(!s.set_source("ko"));
        assert!(!s.set_source("klingon"));
        assert!(s.set_source("auto"));
        assert!(!s.set_target("auto"));
        assert!(!s.set_target("klingon"));
        assert!(s.set_target("vi"));
        assert_eq!(s.target, Lang::Vi);
    }

    /// 清空：回到空闲态，作废进行中的请求，语言选择保留，之后同一原文可以再次翻译。
    #[test]
    fn clear_resets_flow_and_keeps_languages() {
        let mut s = TranslateInputSession::new(Lang::Ja);
        s.set_source("fr");
        translate(&mut s, "bonjour", "こんにちは");
        assert_eq!(s.flow.translation(), "こんにちは");
        s.clear();
        assert_eq!(s.flow.phase, Phase::Idle);
        assert_eq!(s.flow.copy, CopyState::Pending);
        assert_eq!((s.source, s.target), (Lang::Fr, Lang::Ja));
        assert!(
            s.input_changed("bonjour").is_some(),
            "清空后同一原文可再翻译"
        );
        // 进行中清空：结果到达时被当成过期忽略
        let serial = s.begin("salut").unwrap();
        s.clear();
        assert!(!s.finish(serial, done("你好")));
        assert_eq!(s.flow.phase, Phase::Idle);
        assert!(!s.flow.is_busy());
    }

    /// 源为自动检测且识别出语言：用识别结果参与交换，译文交回输入框，语言对对调。
    #[test]
    fn swap_with_detected_auto_source() {
        let mut s = TranslateInputSession::new(Lang::ZhHans);
        assert!(!s.can_swap(), "还没有识别结果时置灰");
        assert_eq!(s.swap(), None);
        translate(&mut s, "Hello world, nice to meet you", "你好，世界");
        assert_eq!(s.effective_source(), Some(Lang::En));
        assert!(s.can_swap());
        assert_eq!(s.swap().as_deref(), Some("你好，世界"));
        assert_eq!((s.source, s.target), (Lang::ZhHans, Lang::En));
        assert_eq!(s.effective_source(), Some(Lang::ZhHans));
    }

    /// 自动检测识别不出（纯数字标点）或结果不在集合内：按钮置灰；翻译失败也不留识别结果。
    #[test]
    fn swap_disabled_when_auto_source_undetected() {
        let mut s = TranslateInputSession::new(Lang::En);
        translate(&mut s, "12345 !?", "12345 !?");
        assert_eq!(s.effective_source(), None);
        assert!(!s.can_swap());
        let serial = s.begin("你好").unwrap();
        assert!(s.finish(serial, Err(InputError::Translate(TranslateError::Timeout))));
        assert!(!s.can_swap());
        // 切换成手选源语言后识别结果作废，改用手选值
        translate(&mut s, "你好，世界", "Hello world");
        assert_eq!(s.effective_source(), Some(Lang::ZhHans));
        s.set_source("fr");
        assert_eq!(s.effective_source(), Some(Lang::Fr));
    }

    /// 手选源语言：直接交换；源与目标相同、正忙时不能交换；还没有译文时只换语言（返回空串）。
    #[test]
    fn swap_with_manual_source() {
        let mut s = TranslateInputSession::new(Lang::Ja);
        s.set_source("fr");
        assert!(s.can_swap());
        assert_eq!(s.swap().as_deref(), Some(""), "没有译文时只换语言");
        assert_eq!((s.source, s.target), (Lang::Ja, Lang::Fr));
        s.set_source("fr");
        assert!(!s.can_swap(), "源与目标相同");
        s.set_target("de");
        s.begin("bonjour").unwrap();
        assert!(!s.can_swap(), "翻译中不能交换");
    }

    /// 切换源 / 目标 / 模型后重翻：输入非空立即发起；空输入不触发；翻译中切换以最新选择为准，旧结果丢弃。
    #[test]
    fn selection_change_retranslates_immediately() {
        let mut s = TranslateInputSession::new(Lang::En);
        // 空输入：不发请求，也不改状态
        assert!(s.set_target("ja"));
        assert_eq!(
            s.retranslate(
                "  
"
            ),
            None
        );
        assert_eq!(s.flow.phase, Phase::Idle);
        // 有输入：切目标语言后立即重翻（不经过防抖），之后同一原文的防抖不会重复触发
        let first = s.begin("hello").unwrap();
        assert!(s.set_target("fr"));
        let second = s.retranslate("hello").expect("翻译中切换也立即重翻");
        assert!(second > first);
        let debounce = s.input_changed("hello");
        assert_eq!(debounce, None, "同一原文不再走防抖");
        // 旧请求的结果被丢弃，最新请求的结果被采纳
        assert!(!s.finish(first, done("旧译文")));
        assert!(s.flow.is_busy());
        assert!(s.finish(second, done("nouveau")));
        assert_eq!(s.flow.translation(), "nouveau");
        // 切源语言、切模型同样触发；模型值没变不算变化
        assert!(s.set_source("en"));
        assert!(s.retranslate("hello").is_some());
        assert!(s.set_pack("opus-en-fr"));
        assert!(!s.set_pack("opus-en-fr"));
        assert_eq!(s.pack_id, "opus-en-fr");
        assert!(s.retranslate("hello").is_some());
    }

    /// 切换后重翻时自动检测的识别结果按新一轮的原文重新计算。
    #[test]
    fn retranslate_refreshes_detection() {
        let mut s = TranslateInputSession::new(Lang::ZhHans);
        let serial = s.retranslate("Hello world, nice to meet you").unwrap();
        assert!(s.finish(serial, done("你好，世界")));
        assert_eq!(s.effective_source(), Some(Lang::En));
        s.set_target("ja");
        let serial = s.retranslate("Hello world, nice to meet you").unwrap();
        assert_eq!(
            s.effective_source(),
            Some(Lang::En),
            "新一轮完成前沿用旧识别"
        );
        assert!(s.finish(serial, done("こんにちは")));
        assert_eq!(s.effective_source(), Some(Lang::En));
    }

    /// 回车决策：Enter 翻译、Shift+Enter 不翻译（留给换行）。
    #[test]
    fn enter_and_shift_enter() {
        assert!(enter_submits(false));
        assert!(!enter_submits(true));
    }

    /// 防抖：输入停顿到点才翻译；新输入作废旧一轮；Enter 立即翻译后到点的防抖不重复翻译。
    #[test]
    fn debounce_and_enter_interplay() {
        let mut s = TranslateInputSession::new(Lang::En);
        assert_eq!(s.input_changed("   "), None, "空白不启动防抖");
        let first = s.input_changed("he").unwrap();
        let second = s.input_changed("hello").unwrap();
        assert_eq!(s.debounce_due(first, "hello"), AutoDecision::Skip);
        assert_eq!(s.debounce_due(second, "hello"), AutoDecision::Run);
        // 防抖等待期间按了 Enter：立即翻译，到点后同一原文不再重复触发
        let third = s.input_changed("hello!").unwrap();
        let serial = s.begin("hello!").expect("Enter 立即翻译");
        assert_eq!(s.debounce_due(third, "hello!"), AutoDecision::Skip);
        // 翻译期间又有新输入：到点时排队，结束后补一轮
        let fourth = s.input_changed("hello!!").unwrap();
        assert_eq!(s.debounce_due(fourth, "hello!!"), AutoDecision::Queued);
        assert!(s.finish(serial, done("你好")));
        assert!(s.take_queued("hello!!"));
        assert!(!s.take_queued("hello!!"), "排队只取一次");
    }

    /// 识别规则：中文识别为简中，英文识别为英语，日文识别为日语，纯数字标点与空串识别不出。
    #[test]
    fn detect_language_rules() {
        assert_eq!(detect_language("你好，世界"), Some(Lang::ZhHans));
        assert_eq!(detect_language("The quick brown fox"), Some(Lang::En));
        assert_eq!(detect_language("こんにちは"), Some(Lang::Ja));
        assert_eq!(detect_language("12345 !?"), None);
        assert_eq!(detect_language(""), None);
    }
}
