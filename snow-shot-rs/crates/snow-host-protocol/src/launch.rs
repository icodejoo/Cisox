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

/// 开关：旧的单进程常驻模式（回退用；阶段 2 之前它就是默认行为）。
pub const FLAG_RESIDENT: &str = "--resident";

/// 开关：无窗口任务（不初始化 GPUI），后面跟任务名。
pub const FLAG_HEADLESS: &str = "--headless";

/// 无窗口任务：按当前配置生成 `host/spec.json` 后退出。
pub const HEADLESS_EMIT_SPEC: &str = "emit-spec";

/// UI 进程以 `--ui` 启动时的参数无效。
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
}

impl fmt::Display for LaunchError {
    /// 输出面向日志的错误描述。
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            LaunchError::MissingSession => write!(f, "--ui 缺少 --session"),
            LaunchError::MissingToken => write!(f, "--ui 缺少 --token"),
            LaunchError::BadSession(s) => write!(f, "会话名无效: {s:?}"),
            LaunchError::BadToken => write!(f, "会话令牌格式无效"),
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

#[cfg(test)]
mod tests {
    use super::*;

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
