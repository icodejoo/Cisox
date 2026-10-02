//! 语音转文字的状态与提示文案（文案全部来自 `dictation.ftl`）。

use super::focus::NoTypeReason;
use super::output::RouteNote;
use crate::ocr_backend::i18n_for;
use snow_config::extensions::DICTATION_BACKEND_SYSTEM;
use snow_i18n::Args;
use snow_stt_protocol::SystemError;

/// 失败原因。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Failure {
    /// 系统语音后端报告的可识别错误（类别与细节）。
    SystemSpeech(SystemError, String),
    /// 找不到语音识别工作进程。
    WorkerMissing,
    /// 工作进程启动失败。
    Spawn(String),
    /// 模型目录不存在。
    ModelDirMissing(String),
    /// 等待工作进程就绪 / 模型加载超时。
    StartTimeout,
    /// 等待结束超时，已强制结束。
    StopTimeout,
    /// 工作进程意外退出（退出码未知为 `None`）。
    Crashed(Option<i32>),
    /// 工作进程上报的错误。
    Worker(String),
    /// 向工作进程写命令失败。
    Link(String),
}

/// 一轮识别的状态。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Status {
    /// 正在加载（拉起进程、加载模型）。
    Loading,
    /// 正在听；携带输出去向说明。
    Listening(RouteNote),
    /// 已发出结束，等待收尾。
    Finishing,
    /// 已结束。
    Done,
    /// 失败。
    Failed(Failure),
}

impl Status {
    /// 是否为失败态。
    pub fn is_failed(&self) -> bool {
        matches!(self, Status::Failed(_))
    }

    /// 按界面语言生成提示文案。
    ///
    /// # 参数
    /// - `locale`：界面语言代码。
    ///
    /// ```ignore
    /// let text = Status::Loading.message("zh-CN");
    /// ```
    pub fn message(&self, locale: &str) -> String {
        let i18n = i18n_for(locale);
        match self {
            Status::Loading => i18n.tr("dictation-status-loading"),
            Status::Listening(note) => {
                let route = route_message(note, locale);
                i18n.tr_with(
                    "dictation-status-listening",
                    &Args::new().named("route", route.as_str()),
                )
                .trim()
                .to_string()
            }
            Status::Finishing => i18n.tr("dictation-status-finishing"),
            Status::Done => i18n.tr("dictation-status-done"),
            Status::Failed(failure) => failure_message(failure, locale),
        }
    }
}

/// 不能键入原因的文案。
fn reason_message(reason: NoTypeReason, locale: &str) -> String {
    let id = match reason {
        NoTypeReason::NoFocus => "dictation-reason-no-focus",
        NoTypeReason::NotEditable => "dictation-reason-not-editable",
        NoTypeReason::Elevated => "dictation-reason-elevated",
        NoTypeReason::Uncertain => "dictation-reason-uncertain",
    };
    i18n_for(locale).tr(id)
}

/// 输出去向说明的文案。
///
/// # 参数
/// - `note`：去向说明。
/// - `locale`：界面语言代码。
pub fn route_message(note: &RouteNote, locale: &str) -> String {
    let i18n = i18n_for(locale);
    match note {
        RouteNote::Pending => String::new(),
        RouteNote::Typing => i18n.tr("dictation-route-typing"),
        RouteNote::TypingWithOverlay => i18n.tr("dictation-route-typing-overlay"),
        RouteNote::OverlayOnly => i18n.tr("dictation-route-overlay"),
        RouteNote::Fallback(reason) => i18n.tr_with(
            "dictation-route-fallback",
            &Args::new().named("reason", reason_message(*reason, locale).as_str()),
        ),
        RouteNote::Blocked(reason) => i18n.tr_with(
            "dictation-route-blocked",
            &Args::new().named("reason", reason_message(*reason, locale).as_str()),
        ),
        RouteNote::TargetLost => i18n.tr("dictation-route-target-lost"),
        RouteNote::SendFailed(detail) => i18n.tr_with(
            "dictation-route-send-failed",
            &Args::new().named("detail", detail.as_str()),
        ),
        RouteNote::TypingStuck => i18n.tr("dictation-route-typing-stuck"),
    }
}

/// 失败原因的文案。
fn failure_message(failure: &Failure, locale: &str) -> String {
    let i18n = i18n_for(locale);
    match failure {
        Failure::SystemSpeech(kind, detail) => {
            let id = match kind {
                SystemError::OnlineSpeechOff => "dictation-error-system-online-off",
                SystemError::MicrophoneDenied => "dictation-error-system-mic-denied",
                SystemError::LanguageUnavailable => "dictation-error-system-language",
                SystemError::Network => "dictation-error-system-network",
                SystemError::Other => "dictation-error-system-other",
            };
            i18n.tr_with(id, &Args::new().named("detail", detail.as_str()))
        }
        Failure::WorkerMissing => i18n.tr("dictation-error-worker-missing"),
        Failure::Spawn(detail) => i18n.tr_with(
            "dictation-error-spawn",
            &Args::new().named("detail", detail.as_str()),
        ),
        Failure::ModelDirMissing(path) => i18n.tr_with(
            "dictation-error-model-dir",
            &Args::new().named("path", path.as_str()),
        ),
        Failure::StartTimeout => i18n.tr("dictation-error-start-timeout"),
        Failure::StopTimeout => i18n.tr("dictation-error-stop-timeout"),
        Failure::Crashed(code) => {
            let shown = code.map_or_else(|| "?".to_string(), |c| c.to_string());
            i18n.tr_with(
                "dictation-error-crashed",
                &Args::new().named("code", shown.as_str()),
            )
        }
        Failure::Worker(detail) => i18n.tr_with(
            "dictation-error-worker",
            &Args::new().named("detail", detail.as_str()),
        ),
        Failure::Link(detail) => i18n.tr_with(
            "dictation-error-link",
            &Args::new().named("detail", detail.as_str()),
        ),
    }
}

/// 设置页里选中语音后端时的提示：选了系统语音返回使用前提与设置指引，其余返回 `None`。
///
/// # 参数
/// - `value`：`dictation/backend` 的当前值。
/// - `locale`：界面语言代码。
///
/// ```ignore
/// assert!(backend_notice(&serde_json::json!("system"), "zh-CN").is_some());
/// ```
pub fn backend_notice(value: &serde_json::Value, locale: &str) -> Option<String> {
    (value.as_str() == Some(DICTATION_BACKEND_SYSTEM))
        .then(|| i18n_for(locale).tr("dictation-notice-system"))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 全部状态与失败原因在两种语言下都能取到非空文案，且不会回落成消息 id 本身。
    #[test]
    fn every_status_has_text_in_all_locales() {
        let failures = [
            Failure::WorkerMissing,
            Failure::Spawn("x".into()),
            Failure::ModelDirMissing("D:/m".into()),
            Failure::StartTimeout,
            Failure::StopTimeout,
            Failure::Crashed(Some(3)),
            Failure::Crashed(None),
            Failure::Worker("bad".into()),
            Failure::Link("pipe".into()),
        ];
        let system_failures = SystemError::ALL
            .into_iter()
            .map(|k| Failure::SystemSpeech(k, "d".into()));
        let reasons = [
            NoTypeReason::NoFocus,
            NoTypeReason::NotEditable,
            NoTypeReason::Elevated,
            NoTypeReason::Uncertain,
        ];
        let mut statuses = vec![Status::Loading, Status::Finishing, Status::Done];
        statuses.extend(
            failures
                .into_iter()
                .chain(system_failures)
                .map(Status::Failed),
        );
        for note in [
            RouteNote::Pending,
            RouteNote::Typing,
            RouteNote::TypingWithOverlay,
            RouteNote::OverlayOnly,
            RouteNote::TargetLost,
            RouteNote::SendFailed("e".into()),
            RouteNote::TypingStuck,
        ] {
            statuses.push(Status::Listening(note));
        }
        for reason in reasons {
            statuses.push(Status::Listening(RouteNote::Fallback(reason)));
            statuses.push(Status::Listening(RouteNote::Blocked(reason)));
        }
        for info in snow_i18n::locales() {
            for status in &statuses {
                let text = status.message(info.code);
                assert!(!text.trim().is_empty(), "{} {status:?}", info.code);
                assert!(
                    !text.starts_with("dictation-"),
                    "{} {status:?} 缺文案: {text}",
                    info.code
                );
            }
        }
    }

    /// 失败原因里的细节会带进文案；退出码未知显示问号。
    #[test]
    fn details_are_interpolated() {
        let text = Status::Failed(Failure::ModelDirMissing("D:/models".into())).message("en-US");
        assert!(text.contains("D:/models"), "{text}");
        let text = Status::Failed(Failure::Crashed(None)).message("en-US");
        assert!(text.contains('?'), "{text}");
        assert!(Status::Failed(Failure::StartTimeout).is_failed());
        assert!(!Status::Done.is_failed());
    }

    /// 系统语音各类错误的文案互不相同，且给出设置指引，细节会带进文案。
    #[test]
    fn system_failures_have_guidance() {
        for locale in ["zh-CN", "en-US"] {
            let mut seen = std::collections::HashSet::new();
            for kind in SystemError::ALL {
                let text =
                    Status::Failed(Failure::SystemSpeech(kind, "DETAIL".into())).message(locale);
                assert!(seen.insert(text.clone()), "{locale} {kind:?} 文案重复");
                assert!(!text.starts_with("dictation-"), "{text}");
            }
            let off = Status::Failed(Failure::SystemSpeech(
                SystemError::OnlineSpeechOff,
                String::new(),
            ))
            .message(locale);
            assert!(off.contains("ms-settings:privacy-speech"), "{off}");
            let other = Status::Failed(Failure::SystemSpeech(SystemError::Other, "DETAIL".into()))
                .message(locale);
            assert!(other.contains("DETAIL"), "{other}");
        }
    }

    /// 设置页提示：只有选了系统语音才有，且两种语言都有文案。
    #[test]
    fn backend_notice_only_for_system() {
        for info in snow_i18n::locales() {
            let text =
                backend_notice(&serde_json::json!("system"), info.code).expect("系统后端应有提示");
            assert!(!text.is_empty() && !text.starts_with("dictation-"));
            assert!(backend_notice(&serde_json::json!("local-model"), info.code).is_none());
        }
        assert!(backend_notice(&serde_json::json!(1), "en-US").is_none());
    }

    /// 键入去向与兜底去向有不同文案；判定中没有文案。
    #[test]
    fn route_messages_differ() {
        let typing = route_message(&RouteNote::Typing, "zh-CN");
        let overlay = route_message(&RouteNote::Fallback(NoTypeReason::NoFocus), "zh-CN");
        assert_ne!(typing, overlay);
        assert!(route_message(&RouteNote::Pending, "zh-CN").is_empty());
    }
}
