//! 宿主拉起 UI 进程时用的命令行开关（方案 §3、§4）。
//!
//! 宿主和 UI 进程对开关的拼写、取值格式必须一致，所以放在这个共用 crate 里：宿主用
//! [`UiLaunch::to_args`] 拼参数，UI 进程用 [`UiLaunch::new`] 校验自己收到的值。

use crate::message::is_valid_token;
use std::fmt;

/// 开关：本进程是宿主拉起的 UI 进程（需同时带会话名和令牌）。
pub const FLAG_UI: &str = "--ui";

/// 开关：会话管道的应用标识（与 `--ui` 配合）。
pub const FLAG_SESSION: &str = "--session";

/// 开关：一次性会话令牌（与 `--ui` 配合）。
pub const FLAG_TOKEN: &str = "--token";

/// 开关：旧的单进程常驻模式（回退用；阶段 4 起不再是默认，必须显式带上）。
pub const FLAG_RESIDENT: &str = "--resident";

/// 托盘宿主的 exe 文件名（与 UI 进程同目录，是常驻入口：自启、快捷方式、无参数转调都指向它）。
pub const HOST_EXE_NAME: &str = "snow-shot.exe";

/// 开关：无窗口任务（不初始化 GPUI），后面跟任务名。
pub const FLAG_HEADLESS: &str = "--headless";

/// 无窗口任务：按当前配置生成 `host/spec.json` 后退出。
pub const HEADLESS_EMIT_SPEC: &str = "emit-spec";

/// 无窗口任务：到点的自动更新检查（查完即退；有新版本把提示文案写到标准输出第一行）。
pub const HEADLESS_UPDATE_CHECK: &str = "update-check";

/// 无窗口任务：改一项配置（UI 是配置唯一写者，宿主要改配置而 UI 不在时用它），后面跟 `--key` / `--value`。
pub const HEADLESS_SET_CONFIG: &str = "set-config";

/// 开关：`set-config` 的配置键。
pub const FLAG_KEY: &str = "--key";

/// 开关：`set-config` 的新值（JSON 文本）。
pub const FLAG_VALUE: &str = "--value";

/// UI 进程以 `--ui` 启动（或无窗口任务）时的参数无效。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LaunchError {
    /// 缺少 `--session`。
    MissingSession,
    /// 缺少 `--token`。
    MissingToken,
    /// 会话名为空或含管道名不允许的字符。
    BadSession(String),
    /// 令牌不是小写十六进制或长度不对。
    BadToken,
    /// 不认识的无窗口任务名。
    UnknownJob(String),
    /// `set-config` 缺少 `--key`（或为空）。
    MissingKey,
    /// `set-config` 缺少 `--value`。
    MissingValue,
    /// `--value` 不是合法的 JSON。
    BadValue(String),
}

impl fmt::Display for LaunchError {
    /// 输出面向日志的错误描述。
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            LaunchError::MissingSession => write!(f, "--ui 缺少 --session"),
            LaunchError::MissingToken => write!(f, "--ui 缺少 --token"),
            LaunchError::BadSession(s) => write!(f, "会话名无效: {s:?}"),
            LaunchError::BadToken => write!(f, "会话令牌格式无效"),
            LaunchError::UnknownJob(job) => write!(f, "未知无窗口任务: {job}"),
            LaunchError::MissingKey => write!(f, "set-config 缺少 --key"),
            LaunchError::MissingValue => write!(f, "set-config 缺少 --value"),
            LaunchError::BadValue(e) => write!(f, "--value 不是合法的 JSON: {e}"),
        }
    }
}

impl std::error::Error for LaunchError {}

/// 会话名是否合法：非空，只含字母、数字、`.`、`_`、`-`（与管道应用标识的限制一致）。
fn is_valid_session(session: &str) -> bool {
    !session.is_empty()
        && session
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
}

/// 宿主传给 UI 进程的会话参数。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UiLaunch {
    /// 会话管道的应用标识（含一次性 launch_id）。
    pub session: String,
    /// 一次性会话令牌（小写十六进制）。
    pub token: String,
}

impl UiLaunch {
    /// 校验并构造。
    ///
    /// # 参数
    /// - `session`：命令行里 `--session` 的值；`None` 表示没带。
    /// - `token`：命令行里 `--token` 的值；`None` 表示没带。
    ///
    /// # 返回
    /// 通过校验的参数，否则 [`LaunchError`]。
    ///
    /// ```rust
    /// use snow_host_protocol::launch::UiLaunch;
    /// let token = "ab".repeat(32);
    /// let launch = UiLaunch::new(Some("cisox.session.1".into()), Some(token)).unwrap();
    /// assert_eq!(launch.session, "cisox.session.1");
    /// assert!(UiLaunch::new(None, None).is_err());
    /// ```
    pub fn new(session: Option<String>, token: Option<String>) -> Result<Self, LaunchError> {
        let session = session.ok_or(LaunchError::MissingSession)?;
        let token = token.ok_or(LaunchError::MissingToken)?;
        if !is_valid_session(&session) {
            return Err(LaunchError::BadSession(session));
        }
        if !is_valid_token(&token) {
            return Err(LaunchError::BadToken);
        }
        Ok(Self { session, token })
    }

    /// 拼成命令行参数（不含程序名）：`--ui --session <名> --token <令牌>`。
    ///
    /// ```rust
    /// use snow_host_protocol::launch::UiLaunch;
    /// let launch = UiLaunch::new(Some("s".into()), Some("cd".repeat(32))).unwrap();
    /// let args = launch.to_args();
    /// assert_eq!(args[0], "--ui");
    /// assert_eq!(args[2], "s");
    /// ```
    pub fn to_args(&self) -> Vec<String> {
        vec![
            FLAG_UI.to_string(),
            FLAG_SESSION.to_string(),
            self.session.clone(),
            FLAG_TOKEN.to_string(),
            self.token.clone(),
        ]
    }
}

/// 无窗口任务（`--headless <任务>`）：不初始化 GPUI，做完就退。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HeadlessJob {
    /// 按当前配置生成 `host/spec.json`。
    EmitSpec,
    /// 到点的自动更新检查。
    UpdateCheck,
    /// 改一项配置。
    SetConfig {
        /// 配置键。
        key: String,
        /// 新值。
        value: serde_json::Value,
    },
}

impl HeadlessJob {
    /// 由命令行里的任务名与参数构造并校验。
    ///
    /// # 参数
    /// - `job`：`--headless` 后面的任务名。
    /// - `key`：`--key` 的值（只有 `set-config` 用）。
    /// - `value`：`--value` 的值，JSON 文本（只有 `set-config` 用）。
    ///
    /// # 返回
    /// 任务；任务名不认识、`set-config` 缺参数或值不是 JSON 返回 [`LaunchError`]。
    ///
    /// ```rust
    /// use snow_host_protocol::launch::HeadlessJob;
    /// let job = HeadlessJob::parse("set-config", Some("a/b".into()), Some("true".into())).unwrap();
    /// assert_eq!(job, HeadlessJob::SetConfig { key: "a/b".into(), value: true.into() });
    /// assert_eq!(HeadlessJob::parse("update-check", None, None).unwrap(), HeadlessJob::UpdateCheck);
    /// assert!(HeadlessJob::parse("nope", None, None).is_err());
    /// ```
    pub fn parse(
        job: &str,
        key: Option<String>,
        value: Option<String>,
    ) -> Result<Self, LaunchError> {
        match job {
            HEADLESS_EMIT_SPEC => Ok(Self::EmitSpec),
            HEADLESS_UPDATE_CHECK => Ok(Self::UpdateCheck),
            HEADLESS_SET_CONFIG => {
                let key = key
                    .filter(|key| !key.is_empty())
                    .ok_or(LaunchError::MissingKey)?;
                let text = value.ok_or(LaunchError::MissingValue)?;
                let value = serde_json::from_str(&text)
                    .map_err(|e| LaunchError::BadValue(e.to_string()))?;
                Ok(Self::SetConfig { key, value })
            }
            other => Err(LaunchError::UnknownJob(other.to_string())),
        }
    }

    /// 任务名（`--headless` 后面的那个词）。
    pub fn name(&self) -> &'static str {
        match self {
            Self::EmitSpec => HEADLESS_EMIT_SPEC,
            Self::UpdateCheck => HEADLESS_UPDATE_CHECK,
            Self::SetConfig { .. } => HEADLESS_SET_CONFIG,
        }
    }

    /// 拼成命令行参数（不含程序名）：`--headless <任务> [--key <键> --value <JSON>]`。
    ///
    /// ```rust
    /// use snow_host_protocol::launch::HeadlessJob;
    /// assert_eq!(HeadlessJob::UpdateCheck.to_args(), ["--headless", "update-check"]);
    /// let set = HeadlessJob::SetConfig { key: "k".into(), value: 1.into() };
    /// assert_eq!(set.to_args(), ["--headless", "set-config", "--key", "k", "--value", "1"]);
    /// ```
    pub fn to_args(&self) -> Vec<String> {
        let mut args = vec![FLAG_HEADLESS.to_string(), self.name().to_string()];
        if let Self::SetConfig { key, value } = self {
            args.extend([
                FLAG_KEY.to_string(),
                key.clone(),
                FLAG_VALUE.to_string(),
                value.to_string(),
            ]);
        }
        args
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 无窗口任务：参数往返；缺参数、值不是 JSON、未知任务都被拒绝。
    #[test]
    fn headless_jobs_round_trip_and_validate() {
        for job in [
            HeadlessJob::EmitSpec,
            HeadlessJob::UpdateCheck,
            HeadlessJob::SetConfig {
                key: "global_shortcuts/x".into(),
                value: serde_json::json!({"a": [1, "二"], "b": null}),
            },
        ] {
            let args = job.to_args();
            assert_eq!(args[0], FLAG_HEADLESS);
            let after = |flag: &str| {
                args.iter()
                    .position(|a| a == flag)
                    .map(|i| args[i + 1].clone())
            };
            assert_eq!(
                HeadlessJob::parse(&args[1], after(FLAG_KEY), after(FLAG_VALUE)).unwrap(),
                job
            );
        }
        assert_eq!(
            HeadlessJob::parse(HEADLESS_SET_CONFIG, None, Some("1".into())),
            Err(LaunchError::MissingKey)
        );
        assert_eq!(
            HeadlessJob::parse(HEADLESS_SET_CONFIG, Some(String::new()), Some("1".into())),
            Err(LaunchError::MissingKey)
        );
        assert_eq!(
            HeadlessJob::parse(HEADLESS_SET_CONFIG, Some("k".into()), None),
            Err(LaunchError::MissingValue)
        );
        assert!(matches!(
            HeadlessJob::parse(HEADLESS_SET_CONFIG, Some("k".into()), Some("{oops".into())),
            Err(LaunchError::BadValue(_))
        ));
        assert_eq!(
            HeadlessJob::parse("nope", None, None),
            Err(LaunchError::UnknownJob("nope".into()))
        );
    }

    /// 参数往返：拼出来的参数能被逐项读回并通过校验。
    #[test]
    fn args_round_trip() {
        let launch = UiLaunch::new(
            Some("cisox.snow_shot.session.0a1b".into()),
            Some("e1".repeat(32)),
        )
        .unwrap();
        let args = launch.to_args();
        assert_eq!(args.len(), 5);
        assert_eq!(args[1], FLAG_SESSION);
        assert_eq!(args[3], FLAG_TOKEN);
        let again = UiLaunch::new(Some(args[2].clone()), Some(args[4].clone())).unwrap();
        assert_eq!(again, launch);
    }

    /// 缺参数、会话名含路径分隔符、令牌格式不对都被拒绝。
    #[test]
    fn invalid_launch_is_rejected() {
        let token = "ab".repeat(32);
        assert_eq!(
            UiLaunch::new(None, Some(token.clone())),
            Err(LaunchError::MissingSession)
        );
        assert_eq!(
            UiLaunch::new(Some("s".into()), None),
            Err(LaunchError::MissingToken)
        );
        assert!(matches!(
            UiLaunch::new(Some(r"a\b".into()), Some(token.clone())),
            Err(LaunchError::BadSession(_))
        ));
        assert!(matches!(
            UiLaunch::new(Some(String::new()), Some(token)),
            Err(LaunchError::BadSession(_))
        ));
        assert_eq!(
            UiLaunch::new(Some("s".into()), Some("xyz".into())),
            Err(LaunchError::BadToken)
        );
    }
}
