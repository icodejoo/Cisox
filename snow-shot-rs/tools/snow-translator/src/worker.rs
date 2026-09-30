//! 命令分发：把协议命令映射到引擎调用，与 stdin/stdout 解耦以便离线测试。

use std::path::Path;
use std::time::Instant;

use crate::engine::{Engine, EngineError, TranslateOptions};
use crate::protocol::{Command, ErrorKind, Event};
use crate::sysmem;

/// 翻译后端抽象，真实实现是 [`Engine`]，测试里用假实现。
pub trait Backend {
    /// 模型 ID。
    fn model_id(&self) -> &str;
    /// 翻译一段文本。
    fn translate(&mut self, text: &str, opts: &TranslateOptions) -> Result<String, EngineError>;
    /// 请求结束后的内存收缩，缺省什么都不做。
    fn trim(&mut self) {}
    /// 覆盖“请求后收缩”开关，缺省什么都不做。
    fn set_trim(&mut self, _on: bool) {}
}

impl Backend for Engine {
    /// 转发到引擎。
    fn model_id(&self) -> &str {
        Engine::model_id(self)
    }

    /// 转发到引擎。
    fn translate(&mut self, text: &str, opts: &TranslateOptions) -> Result<String, EngineError> {
        Engine::translate(self, text, opts)
    }

    /// 转发到引擎。
    fn trim(&mut self) {
        Engine::trim_memory(self);
    }

    /// 转发到引擎。
    fn set_trim(&mut self, on: bool) {
        Engine::set_trim_after_request(self, on);
    }
}

/// 后端加载函数：`(模型目录, 源语言, 目标语言) -> 后端`。
pub type Loader<B> = fn(&Path, &str, &str) -> Result<B, EngineError>;

/// 真实加载函数。
///
/// # 参数
/// - `dir`/`src`/`tgt`：模型目录与语言对。
///
/// # 返回
/// 加载好的 [`Engine`]。
///
/// # 示例
/// ```ignore
/// let mut worker = Worker::new(load_engine);
/// ```
pub fn load_engine(dir: &Path, src: &str, tgt: &str) -> Result<Engine, EngineError> {
    Engine::load(dir, src, tgt)
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
fn error_event(id: Option<u64>, kind: ErrorKind, message: impl Into<String>) -> Event {
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
            } => self.load(&model_dir, &src, &tgt, trim_after_request),
            Command::Translate {
                id,
                text,
                texts,
                max_len,
                num_beams,
            } => self.translate(id, text, texts, TranslateOptions { max_len, num_beams }),
        }
    }

    /// 加载模型（替换已加载的旧模型，先释放旧的再加载以降低峰值内存）。
    fn load(&mut self, dir: &str, src: &str, tgt: &str, trim: Option<bool>) -> Outcome {
        self.backend = None;
        let started = Instant::now();
        match (self.loader)(Path::new(dir), src, tgt) {
            Ok(mut backend) => {
                if let Some(on) = trim {
                    backend.set_trim(on);
                }
                let model_id = backend.model_id().to_string();
                self.backend = Some(backend);
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
    fn translate(
        &mut self,
        id: u64,
        text: Option<String>,
        texts: Option<Vec<String>>,
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
        let started = Instant::now();
        let mut out = Vec::with_capacity(inputs.len());
        for t in &inputs {
            match backend.translate(t, &opts) {
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

    /// 假后端：把文本反转，遇到 "boom" 报错。
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
            opts: &TranslateOptions,
        ) -> Result<String, EngineError> {
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

    /// 假加载函数：目录名为 "missing" 时失败。
    fn fake_loader(dir: &Path, _src: &str, _tgt: &str) -> Result<Fake, EngineError> {
        if dir.ends_with("missing") {
            return Err(EngineError {
                kind: ErrorKind::ModelMissing,
                message: "nope".into(),
            });
        }
        Ok(Fake)
    }

    /// 构造 load 命令。
    fn load_cmd(dir: &str) -> Command {
        Command::Load {
            model_dir: dir.into(),
            src: "en".into(),
            tgt: "zh-CN".into(),
            trim_after_request: None,
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
}
