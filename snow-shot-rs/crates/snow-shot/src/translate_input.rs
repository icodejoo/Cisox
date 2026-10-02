//! 输入框翻译浮窗的纯逻辑：已装翻译包枚举、单次请求配置、按行翻译、状态机、复制与错误文案。
//!
//! 不依赖 GPUI，全部可离屏单测；窗口与控件见 [`crate::translate_input_view`]。

use crate::ocr_backend::i18n_for;
use crate::translate_service::{Backend, TranslateConfig, Translated, Translator};
use snow_config::extensions::ROUTE_SINGLE;
use snow_i18n::Args;
use snow_translate::router::RouteMode;
use snow_translate::{ScannedModel, TranslateError};

/// 下拉里“自动”项的取值：空串表示沿用全局路由。
pub const AUTO_CHOICE_ID: &str = "";
/// 译文行之间的分隔符。
const LINE_BREAK: &str = "\n";

/// 下拉里的一个翻译包选项。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PackChoice {
    /// 包 ID；[`AUTO_CHOICE_ID`] 表示自动。
    pub id: String,
    /// 展示名。
    pub label: String,
}

/// 由扫描结果得到已安装包列表（不含“自动”）。
///
/// # 参数
/// - `models`：模型目录扫描到的可用包。
///
/// # 返回
/// 按 ID 排序的选项；展示名为空时回落到 ID。
///
/// ```ignore
/// let packs = installed_packs(&host.scan(&cfg).models);
/// ```
pub fn installed_packs(models: &[ScannedModel]) -> Vec<PackChoice> {
    let mut packs: Vec<PackChoice> = models
        .iter()
        .map(|m| PackChoice {
            id: m.manifest.id.clone(),
            label: if m.manifest.display_name.trim().is_empty() {
                m.manifest.id.clone()
            } else {
                m.manifest.display_name.clone()
            },
        })
        .collect();
    packs.sort_by(|a, b| a.id.cmp(&b.id));
    packs
}

/// 下拉选项：第一项固定为“自动”，其后是已装包。
///
/// # 参数
/// - `packs`：已装包（不含自动）。
/// - `locale`：界面语言。
pub fn dropdown_choices(packs: Vec<PackChoice>, locale: &str) -> Vec<PackChoice> {
    let auto = PackChoice {
        id: AUTO_CHOICE_ID.to_string(),
        label: i18n_for(locale).tr("translate-input-model-auto"),
    };
    std::iter::once(auto).chain(packs).collect()
}

/// 生成本次请求用的配置：选了具体包就只用它（单包路由），选“自动”则原样沿用全局配置。
///
/// 只改本次请求的副本，不写回全局设置；OpenAI 后端不受包选择影响。
///
/// # 参数
/// - `base`：全局翻译配置。
/// - `model_id`：下拉选中的包 ID。
pub fn request_config(base: &TranslateConfig, model_id: &str) -> TranslateConfig {
    let mut config = base.clone();
    if base.backend == Backend::Local && model_id != AUTO_CHOICE_ID {
        config.model_id = model_id.to_string();
        config.route_mode = RouteMode::from_code(ROUTE_SINGLE).unwrap_or_default();
    }
    config
}

/// 输入框翻译的失败原因。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InputError {
    /// 输入为空。
    Empty,
    /// 翻译后端返回的错误。
    Translate(TranslateError),
}

/// 把失败原因转成界面语言下用户可读的说明。
///
/// # 参数
/// - `error`：失败原因。
/// - `locale`：界面语言（如 `zh-CN`）。
///
/// ```ignore
/// assert!(!error_text(&InputError::Empty, "zh-CN").is_empty());
/// ```
pub fn error_text(error: &InputError, locale: &str) -> String {
    let i18n = i18n_for(locale);
    match error {
        InputError::Empty => i18n.tr("translate-input-error-empty"),
        InputError::Translate(e) => match e {
            TranslateError::NoModelFound(_) => i18n.tr("translate-input-error-no-model"),
            TranslateError::RuntimeMissing(_) => i18n.tr("translate-input-error-runtime"),
            TranslateError::UnsupportedLanguagePair(..) => i18n.tr("translate-input-error-pair"),
            TranslateError::Timeout => i18n.tr("translate-input-error-timeout"),
            other => i18n.tr_with(
                "translate-input-error-failed",
                &Args::new().named("detail", other.to_string()),
            ),
        },
    }
}

/// 翻译整段输入：按行切分，空行原样保留，只把非空行交给后端，再按原位置拼回。
///
/// # 参数
/// - `translator`：翻译后端。
/// - `base`：全局翻译配置。
/// - `model_id`：下拉选中的包 ID（[`AUTO_CHOICE_ID`] 为自动）。
/// - `text`：用户输入。
///
/// # 返回
/// 译文（行间 `\n`）与实际用包标签；输入全空返回 [`InputError::Empty`]。
///
/// ```ignore
/// let out = translate_text(&host, &cfg, AUTO_CHOICE_ID, "Hello\n\nWorld")?;
/// ```
pub fn translate_text(
    translator: &dyn Translator,
    base: &TranslateConfig,
    model_id: &str,
    text: &str,
) -> Result<Translated, InputError> {
    let lines: Vec<&str> = text.lines().map(str::trim_end).collect();
    let filled: Vec<String> = lines
        .iter()
        .filter(|l| !l.trim().is_empty())
        .map(|l| (*l).to_string())
        .collect();
    if filled.is_empty() {
        return Err(InputError::Empty);
    }
    let config = request_config(base, model_id);
    let translated = translator
        .translate(&config, &filled)
        .map_err(InputError::Translate)?;
    if translated.texts.len() != filled.len() {
        return Err(InputError::Translate(TranslateError::Inference(format!(
            "segment count mismatch: got {}, expected {}",
            translated.texts.len(),
            filled.len()
        ))));
    }
    let mut next = translated.texts.into_iter();
    let merged: Vec<String> = lines
        .iter()
        .map(|l| {
            if l.trim().is_empty() {
                String::new()
            } else {
                next.next().unwrap_or_default()
            }
        })
        .collect();
    Ok(Translated {
        texts: vec![merged.join(LINE_BREAK)],
        label: translated.label,
    })
}

/// 复制译文的结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CopyState {
    /// 还没复制过。
    Pending,
    /// 已复制。
    Copied,
    /// 复制失败（附原因）。
    Failed(String),
}

/// 把译文复制到剪贴板；空文本不动剪贴板。
///
/// # 参数
/// - `text`：译文。
/// - `copier`：写剪贴板的函数（生产里是 `copy_text_to_clipboard`，测试里注入假的）。
///
/// # 返回
/// 空文本返回 [`CopyState::Pending`]；成功 [`CopyState::Copied`]；失败 [`CopyState::Failed`]。
///
/// ```ignore
/// let state = copy_translation("你好", snow_platform::clipboard::copy_text_to_clipboard);
/// ```
pub fn copy_translation(text: &str, copier: impl FnOnce(&str) -> Result<(), String>) -> CopyState {
    if text.is_empty() {
        return CopyState::Pending;
    }
    match copier(text) {
        Ok(()) => CopyState::Copied,
        Err(reason) => CopyState::Failed(reason),
    }
}

/// 浮窗的翻译阶段。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Phase {
    /// 空闲（还没翻译过）。
    Idle,
    /// 翻译中。
    Translating,
    /// 完成。
    Done {
        /// 译文。
        text: String,
        /// 实际用包标签。
        label: String,
    },
    /// 失败。
    Failed(InputError),
}

/// 浮窗状态：阶段、请求序号、复制状态。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TranslateInputModel {
    /// 当前阶段。
    pub phase: Phase,
    /// 复制状态（每次新译文重置）。
    pub copy: CopyState,
    /// 最近一次发出的请求序号。
    serial: u64,
}

impl Default for TranslateInputModel {
    fn default() -> Self {
        Self {
            phase: Phase::Idle,
            copy: CopyState::Pending,
            serial: 0,
        }
    }
}

impl TranslateInputModel {
    /// 是否正在翻译（此时不接受新请求）。
    pub fn is_busy(&self) -> bool {
        self.phase == Phase::Translating
    }

    /// 开始一次翻译。
    ///
    /// # 参数
    /// - `text`：用户输入。
    ///
    /// # 返回
    /// 通过则返回本次请求序号；输入为空时进入失败态返回 `None`；正忙时忽略并返回 `None`。
    pub fn begin(&mut self, text: &str) -> Option<u64> {
        if self.is_busy() {
            return None;
        }
        if text.trim().is_empty() {
            self.phase = Phase::Failed(InputError::Empty);
            self.copy = CopyState::Pending;
            return None;
        }
        self.serial += 1;
        self.phase = Phase::Translating;
        self.copy = CopyState::Pending;
        Some(self.serial)
    }

    /// 收到翻译结果；序号过期（不是最近一次请求）或当前不在翻译中则忽略。
    ///
    /// # 参数
    /// - `serial`：结果对应的请求序号。
    /// - `result`：译文或失败原因。
    ///
    /// # 返回
    /// 是否采纳了该结果。
    pub fn finish(&mut self, serial: u64, result: Result<Translated, InputError>) -> bool {
        if serial != self.serial || !self.is_busy() {
            return false;
        }
        self.copy = CopyState::Pending;
        self.phase = match result {
            Ok(done) => Phase::Done {
                text: done.texts.join(LINE_BREAK),
                label: done.label,
            },
            Err(e) => Phase::Failed(e),
        };
        true
    }

    /// 当前可复制的译文；非完成态为空。
    pub fn translation(&self) -> &str {
        match &self.phase {
            Phase::Done { text, .. } => text,
            _ => "",
        }
    }

    /// 点击译文：复制并记录结果。
    ///
    /// # 参数
    /// - `copier`：写剪贴板的函数。
    pub fn copy_now(&mut self, copier: impl FnOnce(&str) -> Result<(), String>) {
        let state = copy_translation(self.translation(), copier);
        if state != CopyState::Pending {
            self.copy = state;
        }
    }

    /// 底部状态行文案（翻译中、失败原因、复制结果、用包标签）；没有可说的返回 `None`。
    ///
    /// # 参数
    /// - `locale`：界面语言。
    pub fn status_line(&self, locale: &str) -> Option<String> {
        let i18n = i18n_for(locale);
        match (&self.phase, &self.copy) {
            (Phase::Translating, _) => Some(i18n.tr("translate-input-working")),
            (Phase::Failed(e), _) => Some(error_text(e, locale)),
            (Phase::Done { .. }, CopyState::Copied) => Some(i18n.tr("translate-input-copied")),
            (Phase::Done { .. }, CopyState::Failed(reason)) => Some(i18n.tr_with(
                "translate-input-copy-failed",
                &Args::new().named("reason", reason.as_str()),
            )),
            (Phase::Done { label, .. }, CopyState::Pending) => Some(i18n.tr_with(
                "translate-input-from-model",
                &Args::new().named("name", label.as_str()),
            )),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use snow_config::document::ConfigDocument;
    use snow_translate::Lang;
    use std::path::{Path, PathBuf};
    use std::sync::Mutex;

    /// 假翻译后端：记录收到的配置与文本，可注入失败。
    struct Fake {
        seen: Mutex<Vec<(TranslateConfig, Vec<String>)>>,
        fail: Option<TranslateError>,
    }

    impl Translator for Fake {
        fn translate(
            &self,
            config: &TranslateConfig,
            texts: &[String],
        ) -> Result<Translated, TranslateError> {
            self.seen
                .lock()
                .unwrap()
                .push((config.clone(), texts.to_vec()));
            if let Some(e) = &self.fail {
                return Err(e.clone());
            }
            Ok(Translated {
                texts: texts.iter().map(|t| format!("译:{t}")).collect(),
                label: "fake".into(),
            })
        }
    }

    /// 默认配置（本地后端）。
    fn base() -> TranslateConfig {
        TranslateConfig::from_document(&ConfigDocument::from_bytes(None), "zh-CN")
    }

    /// 唯一临时目录。
    fn temp_dir(tag: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("snow-translate-input-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("建目录");
        dir
    }

    /// 在模型根下写一份最小模型包。
    fn write_pack(models: &Path, id: &str, display: &str) {
        let dir = models.join(id);
        std::fs::create_dir_all(&dir).expect("建模型目录");
        let manifest = format!(
            r#"{{"schema_version":1,"id":"{id}","display_name":"{display}","family":"m2m100","files":{{"encoder":"e.onnx","decoder":"d.onnx","tokenizer":"t.json"}},"languages":["en","zh-CN"]}}"#
        );
        std::fs::write(dir.join("model.json"), manifest).expect("写清单");
        for f in ["e.onnx", "d.onnx", "t.json"] {
            std::fs::write(dir.join(f), b"x").expect("写文件");
        }
    }

    /// 已装包枚举：扫描真实目录，按 ID 排序，展示名为空回落到 ID，坏目录不出现。
    #[test]
    fn installed_packs_from_scanned_dir() {
        let root = temp_dir("packs");
        write_pack(&root, "zeta", "Zeta Pack");
        write_pack(&root, "alpha", "");
        std::fs::create_dir_all(root.join("broken")).expect("坏目录");
        let report = snow_translate::ModelScanner::new(&root).scan();
        let packs = installed_packs(&report.models);
        assert_eq!(
            packs,
            vec![
                PackChoice {
                    id: "alpha".into(),
                    label: "alpha".into()
                },
                PackChoice {
                    id: "zeta".into(),
                    label: "Zeta Pack".into()
                },
            ]
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    /// 没有任何包时列表为空，下拉仍只有“自动”。
    #[test]
    fn dropdown_starts_with_auto() {
        assert!(installed_packs(&[]).is_empty());
        let zh = dropdown_choices(
            vec![PackChoice {
                id: "a".into(),
                label: "A".into(),
            }],
            "zh-CN",
        );
        assert_eq!(
            (zh[0].id.as_str(), zh[0].label.as_str()),
            (AUTO_CHOICE_ID, "自动")
        );
        assert_eq!(zh[1].id, "a");
        let en = dropdown_choices(Vec::new(), "en-US");
        assert_eq!((en.len(), en[0].label.as_str()), (1, "Auto"));
    }

    /// 选具体包：本次用单包路由并指定 ID；选自动：与全局配置完全一致；原配置不被改动。
    #[test]
    fn request_config_overrides_only_the_copy() {
        let global = base();
        let auto = request_config(&global, AUTO_CHOICE_ID);
        assert_eq!(auto, global);
        let picked = request_config(&global, "opus-en-zh");
        assert_eq!(picked.model_id, "opus-en-zh");
        assert_eq!(picked.route_mode, RouteMode::Single);
        assert!(global.model_id.is_empty());
        assert_eq!(global.route_mode, RouteMode::SpecializedFirst);
        let mut openai = base();
        openai.backend = Backend::OpenAi;
        assert_eq!(request_config(&openai, "x"), openai);
    }

    /// 按行翻译：空行保留位置，只有非空行进后端，沿用全局目标/源语言。
    #[test]
    fn translate_text_keeps_blank_lines() {
        let fake = Fake {
            seen: Mutex::new(Vec::new()),
            fail: None,
        };
        let out =
            translate_text(&fake, &base(), AUTO_CHOICE_ID, "Hello \r\n\nWorld\n").expect("成功");
        assert_eq!(out.texts, vec!["译:Hello\n\n译:World".to_string()]);
        assert_eq!(out.label, "fake");
        let seen = fake.seen.lock().unwrap();
        assert_eq!(seen[0].1, vec!["Hello".to_string(), "World".to_string()]);
        assert_eq!(
            (seen[0].0.source, seen[0].0.target),
            (Lang::Auto, Lang::ZhHans)
        );
    }

    /// 全空白输入返回 Empty，且不会调用后端。
    #[test]
    fn translate_text_rejects_blank() {
        let fake = Fake {
            seen: Mutex::new(Vec::new()),
            fail: None,
        };
        assert_eq!(
            translate_text(&fake, &base(), "", " \n\t\n").unwrap_err(),
            InputError::Empty
        );
        assert!(fake.seen.lock().unwrap().is_empty());
    }

    /// 后端失败原样包成 Translate 错误；选中的包会传到后端。
    #[test]
    fn translate_text_propagates_errors_and_model() {
        let fake = Fake {
            seen: Mutex::new(Vec::new()),
            fail: Some(TranslateError::Timeout),
        };
        let err = translate_text(&fake, &base(), "pack-1", "hi").unwrap_err();
        assert_eq!(err, InputError::Translate(TranslateError::Timeout));
        assert_eq!(fake.seen.lock().unwrap()[0].0.model_id, "pack-1");
    }

    /// 错误文案：每种常见错误在两种语言下都有可读说明，兜底带上细节。
    #[test]
    fn error_texts_are_localized() {
        let cases = [
            InputError::Empty,
            InputError::Translate(TranslateError::NoModelFound("d".into())),
            InputError::Translate(TranslateError::RuntimeMissing("d".into())),
            InputError::Translate(TranslateError::UnsupportedLanguagePair(Lang::En, Lang::Ja)),
            InputError::Translate(TranslateError::Timeout),
        ];
        for locale in ["en-US", "zh-CN"] {
            for case in &cases {
                let text = error_text(case, locale);
                assert!(
                    !text.is_empty() && !text.contains("translate-input-"),
                    "{locale} {case:?}: {text}"
                );
            }
        }
        let other = error_text(
            &InputError::Translate(TranslateError::WorkerDied("boom".into())),
            "zh-CN",
        );
        assert!(
            other.starts_with("翻译失败") && other.contains("boom"),
            "{other}"
        );
        assert_ne!(
            error_text(&InputError::Empty, "en-US"),
            error_text(&InputError::Empty, "zh-CN")
        );
    }

    /// 复制逻辑：成功、失败、空文本三种。
    #[test]
    fn copy_translation_outcomes() {
        let mut copied = String::new();
        assert_eq!(
            copy_translation("你好", |t| {
                copied = t.to_string();
                Ok(())
            }),
            CopyState::Copied
        );
        assert_eq!(copied, "你好");
        assert_eq!(
            copy_translation("x", |_| Err("占用".into())),
            CopyState::Failed("占用".into())
        );
        let mut touched = false;
        assert_eq!(
            copy_translation("", |_| {
                touched = true;
                Ok(())
            }),
            CopyState::Pending
        );
        assert!(!touched, "空文本不应触碰剪贴板");
    }

    /// 状态机：空输入失败、正常流转、忙时拒绝、过期序号忽略。
    #[test]
    fn model_flow_and_stale_results() {
        let mut m = TranslateInputModel::default();
        assert_eq!(m.begin("  "), None);
        assert_eq!(m.phase, Phase::Failed(InputError::Empty));
        let first = m.begin("hello").expect("开始");
        assert!(m.is_busy());
        assert_eq!(m.begin("again"), None, "忙时忽略");
        assert!(!m.finish(
            first + 1,
            Ok(Translated {
                texts: vec!["x".into()],
                label: "l".into()
            })
        ));
        assert!(m.is_busy(), "过期结果不改变状态");
        assert!(m.finish(
            first,
            Ok(Translated {
                texts: vec!["你好".into()],
                label: "fake".into()
            })
        ));
        assert_eq!(m.translation(), "你好");
        assert!(
            !m.finish(first, Err(InputError::Empty)),
            "已完成后不再接受结果"
        );
        let second = m.begin("bye").expect("再次翻译");
        assert!(second > first);
        assert!(m.finish(second, Err(InputError::Translate(TranslateError::Timeout))));
        assert_eq!(m.translation(), "");
    }

    /// 点击复制：完成态记录成功/失败，状态行随之变化；新译文重置复制状态；非完成态点击无效。
    #[test]
    fn copy_updates_status_line() {
        let mut m = TranslateInputModel::default();
        m.copy_now(|_| panic!("未完成不应复制"));
        assert_eq!(m.copy, CopyState::Pending);
        assert_eq!(m.status_line("zh-CN"), None);
        let s = m.begin("hi").unwrap();
        assert!(m.status_line("zh-CN").unwrap().contains("正在翻译"));
        m.finish(
            s,
            Ok(Translated {
                texts: vec!["嗨".into()],
                label: "fake".into(),
            }),
        );
        assert!(m.status_line("zh-CN").unwrap().contains("fake"));
        m.copy_now(|_| Ok(()));
        assert_eq!(m.status_line("zh-CN").as_deref(), Some("已复制"));
        assert_eq!(m.status_line("en-US").as_deref(), Some("Copied"));
        m.copy_now(|_| Err("denied".into()));
        assert!(m.status_line("en-US").unwrap().contains("denied"));
        let s2 = m.begin("again").unwrap();
        m.finish(
            s2,
            Ok(Translated {
                texts: vec!["再".into()],
                label: "fake".into(),
            }),
        );
        assert_eq!(m.copy, CopyState::Pending, "新译文重置复制状态");
    }
}
