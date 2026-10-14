//! 命令分发：把协议命令映射到引擎调用，与 stdin/stdout 解耦以便离线测试。

use std::path::Path;
use std::time::Instant;

use crate::chat::ChatEngine;
use crate::engine::{Engine, EngineError, TranslateOptions, unsupported_pair};
use crate::manifest::Manifest;
use crate::protocol::{Command, ErrorKind, Event};
use crate::sysmem;

/// 翻译后端抽象，真实实现是 [`Engine`]，测试里用假实现。
pub trait Backend {
    /// 模型 ID。
    fn model_id(&self) -> &str;
    /// 翻译一段文本；`src`/`tgt` 是本次请求的语言对，不支持时返回 `UnsupportedPair`。
    fn translate(
        &mut self,
        text: &str,
        src: &str,
        tgt: &str,
        opts: &TranslateOptions,
    ) -> Result<String, EngineError>;
    /// 请求结束后的内存收缩，缺省什么都不做。
    fn trim(&mut self) {}
    /// 覆盖“请求后收缩”开关，缺省什么都不做。
    fn set_trim(&mut self, _on: bool) {}
}

/// 后端加载函数：`(模型目录, 可选的语言对校验) -> 后端`；语言对只用于早失败，不进入后端。
pub type Loader<B> = fn(&Path, Option<(&str, &str)>) -> Result<B, EngineError>;

/// 真实后端：编解码族（marian、m2m100）或对话式 decoder-only 族（hunyuan_chat）。
pub enum AnyEngine {
    /// 编码器 + 合并解码器引擎。
    Seq2Seq(Box<Engine>),
    /// 对话式 decoder-only 引擎。
    Chat(Box<ChatEngine>),
}

impl Backend for AnyEngine {
    /// 转发到具体引擎。
    fn model_id(&self) -> &str {
        match self {
            Self::Seq2Seq(e) => e.model_id(),
            Self::Chat(e) => e.model_id(),
        }
    }

    /// 转发到具体引擎。
    fn translate(
        &mut self,
        text: &str,
        src: &str,
        tgt: &str,
        opts: &TranslateOptions,
    ) -> Result<String, EngineError> {
        match self {
            Self::Seq2Seq(e) => e.translate(text, src, tgt, opts),
            Self::Chat(e) => e.translate(text, src, tgt, opts),
        }
    }

    /// 只有编解码族有 arena 收缩；对话式引擎什么都不做。
    fn trim(&mut self) {
        if let Self::Seq2Seq(e) = self {
            e.trim_memory();
        }
    }

    /// 只有编解码族有 arena 收缩开关。
    fn set_trim(&mut self, on: bool) {
        if let Self::Seq2Seq(e) = self {
            e.set_trim_after_request(on);
        }
    }
}

/// 真实加载函数：按清单 `family` 选引擎，只按目录加载一次，语言对随翻译请求指定。
///
/// # 参数
/// - `dir`：模型目录。
/// - `pair`：可选的 `(src, tgt)`，给了就先校验清单支持它（早失败）；`None` 只加载模型。
///
/// # 返回
/// 加载好的 [`AnyEngine`]；给了语言对但清单不支持返回 `UnsupportedPair`。
///
/// # 示例
/// ```ignore
/// let mut worker = Worker::new(load_engine);
/// ```
pub fn load_engine(dir: &Path, pair: Option<(&str, &str)>) -> Result<AnyEngine, EngineError> {
    let manifest = Manifest::load(dir)?;
    if manifest.is_hunyuan_chat() {
        if let Some((src, tgt)) = pair
            && !manifest.supports_pair(src, tgt)
        {
            return Err(unsupported_pair(&manifest, src, tgt));
        }
        return ChatEngine::load(&manifest, dir).map(|e| AnyEngine::Chat(Box::new(e)));
    }
    Engine::load(dir, pair).map(|e| AnyEngine::Seq2Seq(Box::new(e)))
}

/// 一次命令处理的结果。
#[derive(Debug, PartialEq)]
pub struct Outcome {
    /// 需要依次发送的事件。
    pub events: Vec<Event>,
    /// 发送完后是否退出进程。
    pub exit: bool,
}

impl Outcome {
    /// 单事件、不退出。
    fn one(event: Event) -> Self {
        Self {
            events: vec![event],
            exit: false,
        }
    }
}

/// 构造错误事件。
pub fn error_event(id: Option<u64>, kind: ErrorKind, message: impl Into<String>) -> Event {
    Event::Error {
        id,
        kind,
        message: message.into(),
    }
}

/// 工作进程状态机。
pub struct Worker<B: Backend> {
    /// 已加载后端。
    backend: Option<B>,
    /// 加载函数。
    loader: Loader<B>,
    /// `load` 给的缺省语言对，`translate` 没带语言时沿用（`load` 没给则为空）。
    default_pair: Option<(String, String)>,
}

impl<B: Backend> Worker<B> {
    /// 创建空闲（未加载）的工作器。
    ///
    /// # 参数
    /// - `loader`：加载后端的函数。
    ///
    /// # 返回
    /// 新的 [`Worker`]。
    ///
    /// # 示例
    /// ```ignore
    /// let worker = Worker::new(load_engine);
    /// ```
    pub fn new(loader: Loader<B>) -> Self {
        Self {
            backend: None,
            loader,
            default_pair: None,
        }
    }

    /// 处理一条命令。
    ///
    /// # 参数
    /// - `cmd`：解析后的命令。
    ///
    /// # 返回
    /// 要发送的事件与是否退出。
    ///
    /// # 示例
    /// ```ignore
    /// let out = worker.handle(Command::Ping);
    /// ```
    pub fn handle(&mut self, cmd: Command) -> Outcome {
        match cmd {
            Command::Ping => {
                let m = sysmem::snapshot();
                Outcome::one(Event::Pong {
                    loaded: self.backend.is_some(),
                    mem_bytes: m.working_set,
                    peak_bytes: m.peak_working_set,
                })
            }
            Command::Unload => {
                self.backend = None;
                Outcome {
                    events: vec![Event::Unloaded],
                    exit: true,
                }
            }
            Command::Load {
                model_dir,
                src,
                tgt,
                trim_after_request,
            } => self.load(&model_dir, src, tgt, trim_after_request),
            Command::Translate {
                id,
                text,
                texts,
                max_len,
                num_beams,
                src,
                tgt,
            } => self.translate(
                id,
                text,
                texts,
                (src, tgt),
                TranslateOptions { max_len, num_beams },
            ),
        }
    }

    /// 加载模型（替换已加载的旧模型，先释放旧的再加载以降低峰值内存）。
    ///
    /// `src`/`tgt` 要么都给要么都不给；都给时只用于早失败校验，并记作 `translate` 缺省的语言对。
    /// 参数不合法时不动已加载的模型。
    fn load(
        &mut self,
        dir: &str,
        src: Option<String>,
        tgt: Option<String>,
        trim: Option<bool>,
    ) -> Outcome {
        let pair = match (src, tgt) {
            (Some(s), Some(t)) => Some((s, t)),
            (None, None) => None,
            _ => {
                return Outcome::one(error_event(
                    None,
                    ErrorKind::BadRequest,
                    "load needs both `src` and `tgt`, or neither",
                ));
            }
        };
        self.backend = None;
        self.default_pair = None;
        let started = Instant::now();
        let check = pair.as_ref().map(|(s, t)| (s.as_str(), t.as_str()));
        match (self.loader)(Path::new(dir), check) {
            Ok(mut backend) => {
                if let Some(on) = trim {
                    backend.set_trim(on);
                }
                let model_id = backend.model_id().to_string();
                self.backend = Some(backend);
                self.default_pair = pair;
                Outcome::one(Event::Loaded {
                    model_id,
                    load_ms: started.elapsed().as_millis() as u64,
                    mem_bytes: sysmem::snapshot().working_set,
                })
            }
            Err(e) => Outcome::one(error_event(None, e.kind, e.message)),
        }
    }

    /// 翻译 `text` + `texts`，任何一条失败则整个请求失败。
    ///
    /// `pair` 是请求带的 `(src, tgt)`，缺哪个就沿用 `load` 给的缺省语言对，仍缺则 `BadRequest`。
    fn translate(
        &mut self,
        id: u64,
        text: Option<String>,
        texts: Option<Vec<String>>,
        pair: (Option<String>, Option<String>),
        opts: TranslateOptions,
    ) -> Outcome {
        let Some(backend) = self.backend.as_mut() else {
            return Outcome::one(error_event(
                Some(id),
                ErrorKind::NotLoaded,
                "no model loaded; send `load` first",
            ));
        };
        let inputs: Vec<String> = text
            .into_iter()
            .chain(texts.into_iter().flatten())
            .collect();
        if inputs.is_empty() {
            return Outcome::one(error_event(
                Some(id),
                ErrorKind::BadRequest,
                "translate needs `text` or `texts`",
            ));
        }
        let (default_src, default_tgt) = match &self.default_pair {
            Some((s, t)) => (Some(s.as_str()), Some(t.as_str())),
            None => (None, None),
        };
        let (src, tgt) = (
            pair.0.as_deref().or(default_src),
            pair.1.as_deref().or(default_tgt),
        );
        let (Some(src), Some(tgt)) = (src, tgt) else {
            return Outcome::one(error_event(
                Some(id),
                ErrorKind::BadRequest,
                "translate needs `src` and `tgt` (not given here, and `load` set no default)",
            ));
        };
        let started = Instant::now();
        let mut out = Vec::with_capacity(inputs.len());
        for t in &inputs {
            match backend.translate(t, src, tgt, &opts) {
                Ok(s) => out.push(s),
                Err(e) => return Outcome::one(error_event(Some(id), e.kind, e.message)),
            }
        }
        // elapsed_ms 只统计翻译本身，不含随后的内存收缩
        let elapsed_ms = started.elapsed().as_millis() as u64;
        backend.trim();
        Outcome::one(Event::Result {
            id,
            texts: out,
            elapsed_ms,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 假后端：把文本反转，遇到 "boom" 报错；语言源语言为 "xx" 时模拟不支持的语言对。
    struct Fake;

    impl Backend for Fake {
        /// 固定 ID。
        fn model_id(&self) -> &str {
            "fake"
        }

        /// 反转文本。
        fn translate(
            &mut self,
            text: &str,
            src: &str,
            tgt: &str,
            opts: &TranslateOptions,
        ) -> Result<String, EngineError> {
            if src == "xx" {
                return Err(EngineError {
                    kind: ErrorKind::UnsupportedPair,
                    message: format!("no {src} -> {tgt}"),
                });
            }
            if text == "pair" {
                return Ok(format!("{src}->{tgt}"));
            }
            if text == "beams" {
                return Ok(format!("{:?}/{:?}", opts.num_beams, opts.max_len));
            }
            if text == "boom" {
                return Err(EngineError {
                    kind: ErrorKind::OutOfMemory,
                    message: "bad_alloc".into(),
                });
            }
            Ok(text.chars().rev().collect())
        }
    }

    thread_local! {
        /// 本测试线程里假加载函数的调用次数（验证“换语言对不重载”；每个测试一个线程，互不干扰）。
        static LOADS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    }

    /// 假加载函数：目录名为 "missing" 时失败，语言对源语言为 "xx" 时按不支持处理。
    fn fake_loader(dir: &Path, pair: Option<(&str, &str)>) -> Result<Fake, EngineError> {
        LOADS.with(|c| c.set(c.get() + 1));
        if dir.ends_with("missing") {
            return Err(EngineError {
                kind: ErrorKind::ModelMissing,
                message: "nope".into(),
            });
        }
        if matches!(pair, Some(("xx", _))) {
            return Err(EngineError {
                kind: ErrorKind::UnsupportedPair,
                message: "no xx".into(),
            });
        }
        Ok(Fake)
    }

    /// 构造带缺省语言对 en -> zh-CN 的 load 命令。
    fn load_cmd(dir: &str) -> Command {
        Command::Load {
            model_dir: dir.into(),
            src: Some("en".into()),
            tgt: Some("zh-CN".into()),
            trim_after_request: None,
        }
    }

    /// 构造不带语言对的 load 命令。
    fn load_cmd_no_pair(dir: &str) -> Command {
        Command::Load {
            model_dir: dir.into(),
            src: None,
            tgt: None,
            trim_after_request: None,
        }
    }

    /// 构造翻译命令（`src`/`tgt` 可缺省）。
    fn translate_cmd(id: u64, text: &str, src: Option<&str>, tgt: Option<&str>) -> Command {
        Command::Translate {
            id,
            text: Some(text.into()),
            texts: None,
            max_len: None,
            num_beams: None,
            src: src.map(Into::into),
            tgt: tgt.map(Into::into),
        }
    }

    /// 取唯一事件。
    fn only(out: Outcome) -> Event {
        assert_eq!(out.events.len(), 1);
        out.events.into_iter().next().unwrap()
    }

    /// 未加载时翻译报 NotLoaded，且不退出。
    #[test]
    fn translate_before_load_errors() {
        let mut w = Worker::new(fake_loader);
        let out = w.handle(Command::Translate {
            id: 1,
            text: Some("a".into()),
            texts: None,
            max_len: None,
            num_beams: None,
            src: None,
            tgt: None,
        });
        assert!(!out.exit);
        assert!(matches!(
            only(out),
            Event::Error {
                id: Some(1),
                kind: ErrorKind::NotLoaded,
                ..
            }
        ));
    }

    /// 加载成功返回 Loaded，失败返回带类别的错误。
    #[test]
    fn load_success_and_failure() {
        let mut w = Worker::new(fake_loader);
        assert!(
            matches!(only(w.handle(load_cmd("ok"))), Event::Loaded { ref model_id, .. } if model_id == "fake")
        );
        assert!(matches!(
            only(w.handle(load_cmd("missing"))),
            Event::Error {
                kind: ErrorKind::ModelMissing,
                ..
            }
        ));
        // 失败的加载会释放旧模型
        let out = w.handle(Command::Translate {
            id: 2,
            text: Some("a".into()),
            texts: None,
            max_len: None,
            num_beams: None,
            src: None,
            tgt: None,
        });
        assert!(matches!(
            only(out),
            Event::Error {
                kind: ErrorKind::NotLoaded,
                ..
            }
        ));
    }

    /// text 与 texts 合并，顺序为先 text 后 texts。
    #[test]
    fn translate_text_and_texts_in_order() {
        let mut w = Worker::new(fake_loader);
        w.handle(load_cmd("ok"));
        let out = w.handle(Command::Translate {
            id: 9,
            text: Some("ab".into()),
            texts: Some(vec!["cd".into(), "ef".into()]),
            max_len: None,
            num_beams: None,
            src: None,
            tgt: None,
        });
        match only(out) {
            Event::Result { id, texts, .. } => {
                assert_eq!(id, 9);
                assert_eq!(texts, vec!["ba", "dc", "fe"]);
            }
            other => panic!("unexpected {other:?}"),
        }
    }

    /// 请求里的 num_beams / max_len 原样传给后端。
    #[test]
    fn translate_forwards_options() {
        let mut w = Worker::new(fake_loader);
        w.handle(load_cmd("ok"));
        let out = w.handle(Command::Translate {
            id: 3,
            text: Some("beams".into()),
            texts: None,
            max_len: Some(64),
            num_beams: Some(4),
            src: None,
            tgt: None,
        });
        match only(out) {
            Event::Result { texts, .. } => assert_eq!(texts, vec!["Some(4)/Some(64)"]),
            other => panic!("unexpected {other:?}"),
        }
    }

    /// 没有任何文本是 BadRequest；后端出错时整条请求以后端错误类别返回。
    #[test]
    fn translate_bad_request_and_backend_error() {
        let mut w = Worker::new(fake_loader);
        w.handle(load_cmd("ok"));
        let out = w.handle(Command::Translate {
            id: 3,
            text: None,
            texts: None,
            max_len: None,
            num_beams: None,
            src: None,
            tgt: None,
        });
        assert!(matches!(
            only(out),
            Event::Error {
                kind: ErrorKind::BadRequest,
                ..
            }
        ));
        let out = w.handle(Command::Translate {
            id: 4,
            text: None,
            texts: Some(vec!["x".into(), "boom".into()]),
            max_len: None,
            num_beams: None,
            src: None,
            tgt: None,
        });
        assert!(matches!(
            only(out),
            Event::Error {
                id: Some(4),
                kind: ErrorKind::OutOfMemory,
                ..
            }
        ));
    }

    /// Ping 反映加载状态；Unload 发出 Unloaded 并要求退出。
    #[test]
    fn ping_and_unload() {
        let mut w = Worker::new(fake_loader);
        assert!(matches!(
            only(w.handle(Command::Ping)),
            Event::Pong { loaded: false, .. }
        ));
        w.handle(load_cmd("ok"));
        assert!(matches!(
            only(w.handle(Command::Ping)),
            Event::Pong { loaded: true, .. }
        ));
        let out = w.handle(Command::Unload);
        assert!(out.exit);
        assert_eq!(out.events, vec![Event::Unloaded]);
    }

    /// 不带语言对也能加载；翻译必须带语言对，缺了报 BadRequest 且模型仍可用。
    #[test]
    fn load_without_pair_then_translate_with_pair() {
        let mut w = Worker::new(fake_loader);
        assert!(matches!(
            only(w.handle(load_cmd_no_pair("ok"))),
            Event::Loaded { .. }
        ));
        let out = w.handle(translate_cmd(1, "pair", None, None));
        assert!(matches!(
            only(out),
            Event::Error {
                id: Some(1),
                kind: ErrorKind::BadRequest,
                ..
            }
        ));
        // 只给一个语言也不够
        let out = w.handle(translate_cmd(2, "pair", Some("en"), None));
        assert!(matches!(
            only(out),
            Event::Error {
                kind: ErrorKind::BadRequest,
                ..
            }
        ));
        match only(w.handle(translate_cmd(3, "pair", Some("en"), Some("ja")))) {
            Event::Result { texts, .. } => assert_eq!(texts, vec!["en->ja"]),
            other => panic!("unexpected {other:?}"),
        }
    }

    /// load 给的语言对是 translate 的缺省；请求自带的语言对优先，也可以只覆盖一边。
    #[test]
    fn translate_falls_back_to_load_pair() {
        let mut w = Worker::new(fake_loader);
        w.handle(load_cmd("ok"));
        let result = |w: &mut Worker<Fake>, cmd: Command| match only(w.handle(cmd)) {
            Event::Result { texts, .. } => texts[0].clone(),
            other => panic!("unexpected {other:?}"),
        };
        assert_eq!(
            result(&mut w, translate_cmd(1, "pair", None, None)),
            "en->zh-CN"
        );
        assert_eq!(
            result(&mut w, translate_cmd(2, "pair", Some("fr"), Some("de"))),
            "fr->de"
        );
        assert_eq!(
            result(&mut w, translate_cmd(3, "pair", None, Some("ja"))),
            "en->ja"
        );
        // 不带语言对重新加载后，旧的缺省不再残留
        w.handle(load_cmd_no_pair("ok"));
        assert!(matches!(
            only(w.handle(translate_cmd(4, "pair", None, None))),
            Event::Error {
                kind: ErrorKind::BadRequest,
                ..
            }
        ));
    }

    /// 同一个模型换语言对不会再次调用加载函数；不支持的语言对按请求报错，模型保持可用。
    #[test]
    fn language_change_does_not_reload_and_unsupported_pair_is_per_request() {
        let mut w = Worker::new(fake_loader);
        w.handle(load_cmd_no_pair("ok"));
        assert_eq!(LOADS.with(|c| c.get()), 1);
        for (i, (s, t)) in [("en", "zh-CN"), ("zh-CN", "en"), ("en", "ja")]
            .into_iter()
            .enumerate()
        {
            let out = w.handle(translate_cmd(i as u64, "pair", Some(s), Some(t)));
            assert!(matches!(only(out), Event::Result { .. }));
        }
        let out = w.handle(translate_cmd(10, "pair", Some("xx"), Some("en")));
        assert!(matches!(
            only(out),
            Event::Error {
                id: Some(10),
                kind: ErrorKind::UnsupportedPair,
                ..
            }
        ));
        assert!(matches!(
            only(w.handle(translate_cmd(11, "pair", Some("en"), Some("ja")))),
            Event::Result { .. }
        ));
        assert_eq!(
            LOADS.with(|c| c.get()),
            1,
            "换语言对与不支持的语言对都不应触发加载"
        );
    }

    /// load 带了不支持的语言对：早失败并释放旧模型；只给一边是 BadRequest 且不动旧模型。
    #[test]
    fn load_pair_validation() {
        let mut w = Worker::new(fake_loader);
        w.handle(load_cmd("ok"));
        let half = Command::Load {
            model_dir: "ok".into(),
            src: Some("en".into()),
            tgt: None,
            trim_after_request: None,
        };
        assert!(matches!(
            only(w.handle(half)),
            Event::Error {
                kind: ErrorKind::BadRequest,
                ..
            }
        ));
        // BadRequest 不动已加载的模型与缺省语言对
        match only(w.handle(translate_cmd(1, "pair", None, None))) {
            Event::Result { texts, .. } => assert_eq!(texts, vec!["en->zh-CN"]),
            other => panic!("unexpected {other:?}"),
        }
        let bad = Command::Load {
            model_dir: "ok".into(),
            src: Some("xx".into()),
            tgt: Some("en".into()),
            trim_after_request: None,
        };
        assert!(matches!(
            only(w.handle(bad)),
            Event::Error {
                kind: ErrorKind::UnsupportedPair,
                ..
            }
        ));
        assert!(matches!(
            only(w.handle(translate_cmd(2, "pair", Some("en"), Some("ja")))),
            Event::Error {
                kind: ErrorKind::NotLoaded,
                ..
            }
        ));
    }
}
