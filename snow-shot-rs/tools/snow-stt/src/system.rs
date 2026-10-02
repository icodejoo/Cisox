//! Windows 系统语音后端（`Windows.Media.SpeechRecognition.SpeechRecognizer` 连续识别）。
//!
//! 限制：系统识别器只吃默认麦克风、不能喂 PCM，所以本后端的 `feed` 是空操作，
//! 主循环用 [`crate::source::ClockSource`] 当节拍器；识别内容完全来自系统回调。
//! 需要系统「联机语音识别」开关打开、麦克风权限放行、对应语言包已安装。
//! 假设（HypothesisGenerated）对应 PARTIAL，定稿（ResultGenerated）对应 FINAL。
//! 进程结束即释放识别器。

use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::time::{Duration, Instant};

use snow_stt_protocol::{StartRequest, SystemError};

use crate::backend::{SttBackend, SttEvent};

/// 冲刷时等待回调事件的静默窗口：这么久没有新事件就认为收尾完毕。
const FLUSH_QUIET: Duration = Duration::from_millis(300);
/// 冲刷总时长上限，防止回调源源不断时卡住。
const FLUSH_MAX: Duration = Duration::from_secs(3);
/// 微软文档里的 HRESULT：未接受联机语音识别隐私声明。
const HR_PRIVACY_NOT_ACCEPTED: i32 = 0x8004_5509_u32 as i32;
/// 微软文档里的 HRESULT：所需语言的语音识别包未安装。
const HR_LANGUAGE_NOT_INSTALLED: i32 = 0x8004_503A_u32 as i32;
/// HRESULT：访问被拒绝（麦克风隐私开关关闭时的常见返回）。
const HR_ACCESS_DENIED: i32 = 0x8007_0005_u32 as i32;

/// 按 HRESULT 归类失败原因。
///
/// # 参数
/// - `code`：错误的 HRESULT。
/// - `online_enabled`：注册表里联机语音识别开关是否已开（未归类的错误在开关关闭时按它处理）。
///
/// # 返回
/// 错误类别。
pub fn classify_hresult(code: i32, online_enabled: bool) -> SystemError {
    match code {
        HR_PRIVACY_NOT_ACCEPTED => SystemError::OnlineSpeechOff,
        HR_LANGUAGE_NOT_INSTALLED => SystemError::LanguageUnavailable,
        HR_ACCESS_DENIED => SystemError::MicrophoneDenied,
        _ if !online_enabled => SystemError::OnlineSpeechOff,
        _ => SystemError::Other,
    }
}

/// 由注册表 `OnlineSpeechPrivacy\HasAccepted` 的取值判断联机语音识别是否已开。
///
/// # 参数
/// - `has_accepted`：DWORD 值；值不存在为 `None`（按未开处理）。
pub fn online_enabled_from_registry(has_accepted: Option<u32>) -> bool {
    has_accepted == Some(1)
}

/// 取语言标记的主语言子标签（小写）。
fn primary_subtag(tag: &str) -> String {
    tag.split(['-', '_'])
        .next()
        .unwrap_or_default()
        .to_ascii_lowercase()
}

/// 把请求的语言提示解析成系统支持的语言标记。
///
/// 规则：`auto` 或空取系统语音语言；否则先找完全相同（忽略大小写）的，
/// 再按主语言子标签匹配（`zh-en`、`zh` 都匹配 `zh-Hans-CN`）。
///
/// # 参数
/// - `requested`：配置里的语言提示。
/// - `supported`：系统支持的听写语言标记。
/// - `system`：系统语音语言标记。
///
/// # 返回
/// 支持列表里的语言标记；没有匹配时为 `None`。
pub fn resolve_language(requested: &str, supported: &[String], system: &str) -> Option<String> {
    let want = requested.trim();
    let want = if want.is_empty() || want.eq_ignore_ascii_case("auto") {
        system
    } else {
        want
    };
    let exact = supported.iter().find(|s| s.eq_ignore_ascii_case(want));
    let primary = primary_subtag(want);
    exact
        .or_else(|| supported.iter().find(|s| primary_subtag(s) == primary))
        .cloned()
}

/// 事件整理器：记住尚未定稿的最后一条假设，收尾时把它升格，避免丢字。
#[derive(Debug, Default)]
pub struct Tracker {
    /// 最后一条还没被定稿覆盖的假设文本。
    pending: Option<String>,
}

impl Tracker {
    /// 记录一条回调事件并原样返回，供后续发出。
    pub fn on_event(&mut self, ev: SttEvent) -> SttEvent {
        match &ev {
            SttEvent::Partial(t) => self.pending = Some(t.clone()),
            SttEvent::Final(_) => self.pending = None,
            SttEvent::Failed(_) => {}
        }
        ev
    }

    /// 收尾：若还有没定稿的假设，把它当作一句定稿。
    pub fn flush(&mut self) -> Option<SttEvent> {
        self.pending
            .take()
            .filter(|t| !t.trim().is_empty())
            .map(SttEvent::Final)
    }
}

/// 从回调事件通道里取尽事件；`wait` 为真时每次最多等 [`FLUSH_QUIET`]，用于收尾。
fn drain(rx: &Receiver<SttEvent>, tracker: &mut Tracker, wait: bool) -> Vec<SttEvent> {
    let mut out = Vec::new();
    let started = Instant::now();
    loop {
        let next = if wait && started.elapsed() < FLUSH_MAX {
            rx.recv_timeout(FLUSH_QUIET)
        } else {
            rx.try_recv().map_err(|e| match e {
                mpsc::TryRecvError::Empty => RecvTimeoutError::Timeout,
                mpsc::TryRecvError::Disconnected => RecvTimeoutError::Disconnected,
            })
        };
        match next {
            Ok(ev) => out.push(tracker.on_event(ev)),
            Err(_) => return out,
        }
    }
}

#[cfg(windows)]
pub use win::{SystemBackend, probe_report};

#[cfg(not(windows))]
mod stub {
    //! 非 Windows 平台没有系统语音，直接报错。
    use super::*;

    /// 占位后端：加载即失败。
    pub struct SystemBackend;

    impl SystemBackend {
        /// 加载总是失败。
        pub fn load(_req: &StartRequest) -> Result<Self, String> {
            Err(SystemError::Other.to_error_text("系统语音后端仅支持 Windows"))
        }
    }

    impl SttBackend for SystemBackend {
        fn feed(&mut self, _pcm: &[f32]) {}
        fn poll(&mut self) -> Vec<SttEvent> {
            Vec::new()
        }
        fn finish(&mut self) -> Vec<SttEvent> {
            Vec::new()
        }
    }

    /// 探测报告：仅说明平台不支持。
    pub fn probe_report(_language: &str) -> String {
        "system speech: unsupported platform".to_string()
    }
}
#[cfg(not(windows))]
pub use stub::{SystemBackend, probe_report};

#[cfg(windows)]
mod win {
    //! WinRT 调用部分：创建识别器、挂回调、启动与停止。
    use std::sync::mpsc::Sender;

    use windows::Foundation::{TimeSpan, TypedEventHandler};
    use windows::Globalization::Language;
    use windows::Media::SpeechRecognition::{
        SpeechContinuousRecognitionCompletedEventArgs,
        SpeechContinuousRecognitionResultGeneratedEventArgs, SpeechContinuousRecognitionSession,
        SpeechRecognitionConfidence, SpeechRecognitionHypothesisGeneratedEventArgs,
        SpeechRecognitionResultStatus, SpeechRecognizer,
    };
    use windows::Win32::System::Com::{COINIT_MULTITHREADED, CoInitializeEx};
    use windows::Win32::System::Registry::{HKEY_CURRENT_USER, RRF_RT_REG_DWORD, RegGetValueW};
    use windows::core::{HSTRING, w};

    use super::*;

    /// 自动停止的静默超时：连续识别默认 20 秒静默就自动结束，这里放宽到 1 小时。
    const SILENCE_TIMEOUT_TICKS: i64 = 3600 * 10_000_000;

    /// 把识别结果状态归类成错误；成功或用户取消返回 `None`。
    ///
    /// # 参数
    /// - `status`：识别 / 编译约束的结果状态。
    pub fn classify_status(status: SpeechRecognitionResultStatus) -> Option<SystemError> {
        match status {
            SpeechRecognitionResultStatus::Success
            | SpeechRecognitionResultStatus::UserCanceled => None,
            SpeechRecognitionResultStatus::MicrophoneUnavailable => {
                Some(SystemError::MicrophoneDenied)
            }
            SpeechRecognitionResultStatus::TopicLanguageNotSupported
            | SpeechRecognitionResultStatus::GrammarLanguageMismatch => {
                Some(SystemError::LanguageUnavailable)
            }
            SpeechRecognitionResultStatus::NetworkFailure => Some(SystemError::Network),
            _ => Some(SystemError::Other),
        }
    }

    /// 读取注册表中联机语音识别开关（`HasAccepted`）。
    fn read_online_flag() -> bool {
        let mut value: u32 = 0;
        let mut size = std::mem::size_of::<u32>() as u32;
        // SAFETY: 输出缓冲是局部 u32，长度与 size 一致；键与值名为静态宽字符串。
        let status = unsafe {
            RegGetValueW(
                HKEY_CURRENT_USER,
                w!("Software\\Microsoft\\Speech_OneCore\\Settings\\OnlineSpeechPrivacy"),
                w!("HasAccepted"),
                RRF_RT_REG_DWORD,
                None,
                Some((&mut value as *mut u32).cast()),
                Some(&mut size),
            )
        };
        online_enabled_from_registry(status.is_ok().then_some(value))
    }

    /// 系统支持的听写语言标记与系统语音语言。
    fn languages() -> windows::core::Result<(Vec<String>, String)> {
        let supported = SpeechRecognizer::SupportedTopicLanguages()?
            .into_iter()
            .filter_map(|l| l.LanguageTag().ok().map(|t| t.to_string()))
            .collect();
        let system = SpeechRecognizer::SystemSpeechLanguage()?
            .LanguageTag()?
            .to_string();
        Ok((supported, system))
    }

    /// 创建识别器并编译默认听写约束；失败时已归类。
    fn create_compiled(
        requested: &str,
        online: bool,
    ) -> Result<(SpeechRecognizer, String), (SystemError, String)> {
        let other = |e: windows::core::Error| {
            (
                classify_hresult(e.code().0, online),
                e.message().trim().to_string(),
            )
        };
        let (supported, system) = languages().map_err(other)?;
        let tag = resolve_language(requested, &supported, &system).ok_or_else(|| {
            (
                SystemError::LanguageUnavailable,
                format!("请求 {requested}，系统支持 {supported:?}"),
            )
        })?;
        let lang = Language::CreateLanguage(&HSTRING::from(tag.as_str())).map_err(other)?;
        let recognizer = SpeechRecognizer::Create(&lang).map_err(other)?;
        let compiled = recognizer
            .CompileConstraintsAsync()
            .and_then(|op| op.join())
            .map_err(other)?;
        let status = compiled.Status().map_err(other)?;
        match classify_status(status) {
            None => Ok((recognizer, tag)),
            Some(SystemError::Other) if !online => Err((
                SystemError::OnlineSpeechOff,
                format!("编译约束状态 {}", status.0),
            )),
            Some(kind) => Err((kind, format!("编译约束状态 {}", status.0))),
        }
    }

    /// 能力探测报告（多行文本）：能否创建识别器、语言是否支持、联机开关与编译约束结果。
    ///
    /// # 参数
    /// - `language`：要探测的语言提示（`auto` 表示系统语言）。
    pub fn probe_report(language: &str) -> String {
        // SAFETY: 仅初始化当前线程的 COM 套间；重复调用返回 S_FALSE，无需配对释放。
        let _ = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) };
        let online = read_online_flag();
        let mut lines = vec![format!("online_speech_enabled: {online}")];
        match languages() {
            Ok((supported, system)) => {
                lines.push(format!("system_speech_language: {system}"));
                lines.push(format!("supported_languages: {}", supported.join(",")));
                let resolved = resolve_language(language, &supported, &system);
                lines.push(format!("resolved_language: {resolved:?}"));
            }
            Err(e) => lines.push(format!("languages_error: {e}")),
        }
        match create_compiled(language, online) {
            Ok((_, tag)) => lines.push(format!("recognizer: ok ({tag})")),
            Err((kind, detail)) => lines.push(format!("recognizer: {} {detail}", kind.tag())),
        }
        lines.join("\n")
    }

    /// 系统语音后端：持有识别器与连续识别会话。
    pub struct SystemBackend {
        /// 识别器（需保持存活，回调才会继续）。
        recognizer: SpeechRecognizer,
        /// 连续识别会话。
        session: SpeechContinuousRecognitionSession,
        /// 回调线程发来的事件。
        rx: Receiver<SttEvent>,
        /// 假设跟踪，收尾时升格。
        tracker: Tracker,
        /// 是否已停止过会话。
        stopped: bool,
    }

    impl SystemBackend {
        /// 创建识别器并开始连续识别（此刻系统开始占用默认麦克风）。
        ///
        /// # 参数
        /// - `req`：START 请求，只用到语言提示。
        ///
        /// # 返回
        /// 后端实例；失败时返回带类别标记的错误文本（见 `SystemError::to_error_text`）。
        pub fn load(req: &StartRequest) -> Result<Self, String> {
            // SAFETY: 仅初始化当前线程的 COM 套间；重复调用返回 S_FALSE，无需配对释放。
            let _ = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) };
            let online = read_online_flag();
            let fail = |(kind, detail): (SystemError, String)| kind.to_error_text(&detail);
            let (recognizer, _tag) = create_compiled(&req.language, online).map_err(fail)?;
            let session = recognizer.ContinuousRecognitionSession().map_err(|e| {
                fail((
                    classify_hresult(e.code().0, online),
                    e.message().trim().to_string(),
                ))
            })?;
            let (tx, rx) = mpsc::channel();
            Self::attach_handlers(&recognizer, &session, &tx)
                .map_err(|e| fail((SystemError::Other, e.message().trim().to_string())))?;
            let _ = session.SetAutoStopSilenceTimeout(TimeSpan {
                Duration: SILENCE_TIMEOUT_TICKS,
            });
            session.StartAsync().and_then(|op| op.join()).map_err(|e| {
                fail((
                    classify_hresult(e.code().0, online),
                    e.message().trim().to_string(),
                ))
            })?;
            Ok(Self {
                recognizer,
                session,
                rx,
                tracker: Tracker::default(),
                stopped: false,
            })
        }

        /// 挂上假设、定稿、会话结束三个回调，事件统一转进通道。
        fn attach_handlers(
            recognizer: &SpeechRecognizer,
            session: &SpeechContinuousRecognitionSession,
            tx: &Sender<SttEvent>,
        ) -> windows::core::Result<()> {
            let partial_tx = tx.clone();
            recognizer.HypothesisGenerated(&TypedEventHandler::<
                SpeechRecognizer,
                SpeechRecognitionHypothesisGeneratedEventArgs,
            >::new(move |_, args| {
                if let Some(args) = args.as_ref() {
                    let text = args.Hypothesis()?.Text()?.to_string();
                    let _ = partial_tx.send(SttEvent::Partial(text));
                }
                Ok(())
            }))?;
            let final_tx = tx.clone();
            session.ResultGenerated(&TypedEventHandler::<
                SpeechContinuousRecognitionSession,
                SpeechContinuousRecognitionResultGeneratedEventArgs,
            >::new(move |_, args| {
                if let Some(args) = args.as_ref() {
                    let result = args.Result()?;
                    let rejected = result.Confidence()? == SpeechRecognitionConfidence::Rejected;
                    let text = result.Text()?.to_string();
                    if result.Status()? == SpeechRecognitionResultStatus::Success
                        && !(rejected && text.trim().is_empty())
                    {
                        let _ = final_tx.send(SttEvent::Final(text));
                    }
                }
                Ok(())
            }))?;
            let done_tx = tx.clone();
            session.Completed(&TypedEventHandler::<
                SpeechContinuousRecognitionSession,
                SpeechContinuousRecognitionCompletedEventArgs,
            >::new(move |_, args| {
                if let Some(args) = args.as_ref()
                    && let Some(kind) = classify_status(args.Status()?)
                {
                    let _ = done_tx.send(SttEvent::Failed(
                        kind.to_error_text(&format!("会话提前结束，状态 {}", args.Status()?.0)),
                    ));
                }
                Ok(())
            }))?;
            Ok(())
        }

        /// 停止会话（只做一次）。
        fn stop(&mut self) {
            if !self.stopped {
                self.stopped = true;
                let _ = self.session.StopAsync().and_then(|op| op.join());
            }
        }
    }

    impl SttBackend for SystemBackend {
        /// 系统识别器自己采麦克风，这里不需要音频。
        fn feed(&mut self, _pcm: &[f32]) {}

        fn poll(&mut self) -> Vec<SttEvent> {
            drain(&self.rx, &mut self.tracker, false)
        }

        fn finish(&mut self) -> Vec<SttEvent> {
            self.stop();
            let mut events = drain(&self.rx, &mut self.tracker, true);
            events.extend(self.tracker.flush());
            events
        }
    }

    impl Drop for SystemBackend {
        /// 释放时停止会话并关闭识别器。
        fn drop(&mut self) {
            self.stop();
            let _ = self.recognizer.Close();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 构造字符串列表。
    fn list(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn hresult_classification() {
        assert_eq!(
            classify_hresult(HR_PRIVACY_NOT_ACCEPTED, true),
            SystemError::OnlineSpeechOff
        );
        assert_eq!(
            classify_hresult(HR_LANGUAGE_NOT_INSTALLED, true),
            SystemError::LanguageUnavailable
        );
        assert_eq!(
            classify_hresult(HR_ACCESS_DENIED, true),
            SystemError::MicrophoneDenied
        );
        assert_eq!(classify_hresult(0x1234, true), SystemError::Other);
        // 未归类的错误在联机开关关闭时优先按开关处理
        assert_eq!(
            classify_hresult(0x1234, false),
            SystemError::OnlineSpeechOff
        );
        // 明确的语言 / 权限错误不受开关影响
        assert_eq!(
            classify_hresult(HR_ACCESS_DENIED, false),
            SystemError::MicrophoneDenied
        );
    }

    #[test]
    fn registry_flag() {
        assert!(online_enabled_from_registry(Some(1)));
        assert!(!online_enabled_from_registry(Some(0)));
        assert!(!online_enabled_from_registry(None));
    }

    #[test]
    fn language_resolution() {
        let sup = list(&["en-US", "zh-Hans-CN"]);
        assert_eq!(
            resolve_language("auto", &sup, "zh-Hans-CN").as_deref(),
            Some("zh-Hans-CN")
        );
        assert_eq!(
            resolve_language("", &sup, "en-US").as_deref(),
            Some("en-US")
        );
        assert_eq!(
            resolve_language("zh-en", &sup, "en-US").as_deref(),
            Some("zh-Hans-CN")
        );
        assert_eq!(
            resolve_language("zh-CN", &sup, "en-US").as_deref(),
            Some("zh-Hans-CN")
        );
        assert_eq!(
            resolve_language("EN-us", &sup, "zh-Hans-CN").as_deref(),
            Some("en-US")
        );
        assert_eq!(resolve_language("ja", &sup, "en-US"), None);
        assert_eq!(resolve_language("auto", &sup, "fr-FR"), None);
    }

    #[test]
    fn tracker_promotes_unfinished_partial_only() {
        let mut t = Tracker::default();
        t.on_event(SttEvent::Partial("你".into()));
        t.on_event(SttEvent::Partial("你好".into()));
        assert_eq!(t.flush(), Some(SttEvent::Final("你好".into())));
        assert_eq!(t.flush(), None);

        t.on_event(SttEvent::Partial("你好".into()));
        t.on_event(SttEvent::Final("你好。".into()));
        assert_eq!(t.flush(), None);

        t.on_event(SttEvent::Partial("  ".into()));
        assert_eq!(t.flush(), None);
    }

    #[test]
    fn drain_collects_without_blocking() {
        let (tx, rx) = mpsc::channel();
        tx.send(SttEvent::Partial("a".into())).unwrap();
        tx.send(SttEvent::Final("a。".into())).unwrap();
        let mut t = Tracker::default();
        let got = drain(&rx, &mut t, false);
        assert_eq!(
            got,
            vec![SttEvent::Partial("a".into()), SttEvent::Final("a。".into())]
        );
        assert!(drain(&rx, &mut t, false).is_empty());
    }

    #[cfg(windows)]
    #[test]
    fn status_classification() {
        use super::win::classify_status;
        use windows::Media::SpeechRecognition::SpeechRecognitionResultStatus as S;
        assert_eq!(classify_status(S::Success), None);
        assert_eq!(classify_status(S::UserCanceled), None);
        assert_eq!(
            classify_status(S::MicrophoneUnavailable),
            Some(SystemError::MicrophoneDenied)
        );
        assert_eq!(
            classify_status(S::TopicLanguageNotSupported),
            Some(SystemError::LanguageUnavailable)
        );
        assert_eq!(
            classify_status(S::NetworkFailure),
            Some(SystemError::Network)
        );
        assert_eq!(
            classify_status(S::TimeoutExceeded),
            Some(SystemError::Other)
        );
    }
}
