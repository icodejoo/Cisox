//! 日志初始化：数据根下 `logs/` 的按天滚动文件 + 可选 stderr（方案 §10 T3）。
//!
//! 日志目录由数据根派生，并硬拒绝 upstream 的 `SnowShot` 目录（约定 10）。
//! 不记录配置内容与用户数据，调用方也不应把整份配置写进日志。

use std::fmt;
use std::fs;
use std::path::{Component, Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use tracing::Subscriber;
use tracing_appender::non_blocking::WorkerGuard;
use tracing_appender::rolling::{RollingFileAppender, Rotation};
use tracing_subscriber::layer::Layered;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;
use tracing_subscriber::{EnvFilter, Layer, Registry};

use crate::APP_ID;

/// 数据根下的日志目录名。
pub const LOG_DIR_NAME: &str = "logs";
/// 日志目录下的崩溃转储子目录名。
pub const CRASH_DIR_NAME: &str = "crash";
/// 日志文件扩展名。
const LOG_FILE_SUFFIX: &str = "log";
/// 默认保留天数（对齐 upstream：保留最近 7 天）。
pub const DEFAULT_RETENTION_DAYS: usize = 7;
/// 默认日志级别。
pub const DEFAULT_LEVEL: &str = "info";
/// upstream 数据目录的组件名（仅用于拒绝，不用于拼路径）。
const UPSTREAM_DIRECTORY_NAME: &str = "SnowShot";
/// 一天的秒数。
const SECONDS_PER_DAY: u64 = 86_400;

/// 日志配置。
#[derive(Debug, Clone)]
pub struct LogConfig {
    /// 数据根目录（由 `snow-config` 的目录解析得到）。
    pub data_root: PathBuf,
    /// 保留最近多少天的日志。
    pub retention_days: usize,
    /// 是否同时输出到 stderr。
    pub to_stderr: bool,
    /// 环境变量未设置时使用的过滤规则。
    pub default_level: String,
}

impl LogConfig {
    /// 以默认值创建配置。
    ///
    /// # 参数
    /// - `data_root`：数据根目录
    ///
    /// # 示例
    /// ```
    /// use snow_app_core::logging::LogConfig;
    ///
    /// let config = LogConfig::new(std::env::temp_dir());
    /// assert_eq!(config.retention_days, 7);
    /// ```
    pub fn new(data_root: impl Into<PathBuf>) -> Self {
        Self {
            data_root: data_root.into(),
            retention_days: DEFAULT_RETENTION_DAYS,
            to_stderr: false,
            default_level: DEFAULT_LEVEL.to_string(),
        }
    }
}

/// 日志目录不可用的原因。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LogDirError {
    /// 数据根为空。
    EmptyRoot,
    /// 数据根落在 upstream 的 `SnowShot` 目录下。
    UpstreamLocation,
}

impl fmt::Display for LogDirError {
    /// 输出简短英文原因（面向日志与开发者）。
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptyRoot => f.write_str("the data root is empty"),
            Self::UpstreamLocation => f.write_str("the data root points at the upstream data"),
        }
    }
}

impl std::error::Error for LogDirError {}

/// 覆盖日志级别的环境变量名（由 `APP_ID` 派生，如 `CISOX_LOG`）。
///
/// # 示例
/// ```
/// assert!(snow_app_core::logging::level_env_var().ends_with("_LOG"));
/// ```
pub fn level_env_var() -> String {
    format!("{}_LOG", APP_ID.to_ascii_uppercase())
}

/// 校验数据根并返回 `<数据根>/logs`。
///
/// # 参数
/// - `data_root`：数据根目录
///
/// # 返回
/// 日志目录路径；数据根为空或位于 upstream 目录下时返回错误。
///
/// # 示例
/// ```
/// use std::path::Path;
/// use snow_app_core::logging::logs_directory;
///
/// assert!(logs_directory(Path::new("data")).unwrap().ends_with("logs"));
/// assert!(logs_directory(Path::new("x/SnowShot/snow_shot")).is_err());
/// ```
pub fn logs_directory(data_root: &Path) -> Result<PathBuf, LogDirError> {
    if data_root.as_os_str().is_empty() {
        return Err(LogDirError::EmptyRoot);
    }
    let is_upstream = data_root.components().any(|component| match component {
        Component::Normal(name) => name
            .to_string_lossy()
            .eq_ignore_ascii_case(UPSTREAM_DIRECTORY_NAME),
        _ => false,
    });
    if is_upstream {
        return Err(LogDirError::UpstreamLocation);
    }
    Ok(data_root.join(LOG_DIR_NAME))
}

/// 崩溃转储目录：`<数据根>/logs/crash`。
///
/// # 参数
/// - `data_root`：数据根目录
///
/// # 返回
/// 崩溃转储目录路径；校验规则同 [`logs_directory`]。
pub fn crash_directory(data_root: &Path) -> Result<PathBuf, LogDirError> {
    Ok(logs_directory(data_root)?.join(CRASH_DIR_NAME))
}

/// 由公历日期计算自 1970-01-01 起的天数。
fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let y = if month <= 2 { year - 1 } else { year };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (month + 9) % 12;
    let doy = (153 * mp + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// 从日志文件名 `<APP_ID>.YYYY-MM-DD.log` 解析日期（自 1970 起的天数）。
fn log_file_day(file_name: &str) -> Option<i64> {
    let middle = file_name
        .strip_prefix(APP_ID)?
        .strip_prefix('.')?
        .strip_suffix(LOG_FILE_SUFFIX)?
        .strip_suffix('.')?;
    let mut parts = middle.split('-');
    let year: i64 = parts.next()?.parse().ok()?;
    let month: i64 = parts.next()?.parse().ok()?;
    let day: i64 = parts.next()?.parse().ok()?;
    if parts.next().is_some() || !(1..=12).contains(&month) || !(1..=31).contains(&day) {
        return None;
    }
    Some(days_from_civil(year, month, day))
}

/// 删除超出保留期的日志文件，只处理符合命名规则的文件。
///
/// # 参数
/// - `dir`：日志目录
/// - `today`：今天（自 1970 起的天数，UTC）
/// - `retention_days`：保留最近多少天（含今天）
///
/// # 返回
/// 实际删除的文件数。
pub fn prune_expired_logs(dir: &Path, today: i64, retention_days: usize) -> usize {
    let Ok(entries) = fs::read_dir(dir) else {
        return 0;
    };
    let oldest_kept = today - retention_days.max(1) as i64 + 1;
    let mut removed = 0;
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        if log_file_day(&name).is_some_and(|day| day < oldest_kept)
            && fs::remove_file(entry.path()).is_ok()
        {
            removed += 1;
        }
    }
    removed
}

/// 当前 UTC 日期（自 1970 起的天数），与滚动文件名所用的日期一致。
fn today_utc() -> i64 {
    let seconds = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    (seconds / SECONDS_PER_DAY) as i64
}

/// 日志句柄：drop 时刷新缓冲；持有期间日志写入线程保持存活。
#[must_use = "LogGuard 被丢弃后缓冲日志将被刷新并停止写文件"]
pub struct LogGuard {
    /// 后台写入线程的守卫；降级为无文件日志时为空。
    _worker: Option<WorkerGuard>,
    /// 实际使用的日志目录；降级时为空。
    log_dir: Option<PathBuf>,
    /// 降级原因；正常时为空。
    degraded_reason: Option<String>,
}

impl LogGuard {
    /// 实际使用的日志目录；文件日志不可用时为 `None`。
    pub fn log_dir(&self) -> Option<&Path> {
        self.log_dir.as_deref()
    }

    /// 文件日志的降级原因；一切正常时为 `None`。
    pub fn degraded_reason(&self) -> Option<&str> {
        self.degraded_reason.as_deref()
    }
}

/// 生成过滤器：环境变量优先，非法或缺失时回退默认规则，再回退 `info`。
fn build_filter(env_value: Option<&str>, default_level: &str) -> EnvFilter {
    env_value
        .and_then(|value| EnvFilter::try_new(value).ok())
        .or_else(|| EnvFilter::try_new(default_level).ok())
        .unwrap_or_else(|| EnvFilter::new(DEFAULT_LEVEL))
}

/// 组装 subscriber 与守卫（不触碰全局状态，便于测试）。
fn build_subscriber(
    config: &LogConfig,
    env_value: Option<&str>,
    today: i64,
) -> (impl Subscriber + Send + Sync, LogGuard) {
    // 过滤器作为最内层，才能对其后所有输出层全局生效
    let mut layers: Vec<Box<dyn Layer<Layered<EnvFilter, Registry>> + Send + Sync>> = Vec::new();
    let mut guard = LogGuard {
        _worker: None,
        log_dir: None,
        degraded_reason: None,
    };

    match open_file_writer(config, today) {
        Ok((writer, worker, dir)) => {
            layers.push(Box::new(
                tracing_subscriber::fmt::layer()
                    .with_ansi(false)
                    .with_writer(writer),
            ));
            guard._worker = Some(worker);
            guard.log_dir = Some(dir);
        }
        Err(reason) => guard.degraded_reason = Some(reason),
    }
    if config.to_stderr || guard.log_dir.is_none() {
        layers.push(Box::new(
            tracing_subscriber::fmt::layer().with_writer(std::io::stderr),
        ));
    }
    (
        tracing_subscriber::registry()
            .with(build_filter(env_value, &config.default_level))
            .with(layers),
        guard,
    )
}

/// 创建日志目录与滚动文件写入器，先清理过期文件。
fn open_file_writer(
    config: &LogConfig,
    today: i64,
) -> Result<
    (
        tracing_appender::non_blocking::NonBlocking,
        WorkerGuard,
        PathBuf,
    ),
    String,
> {
    let dir = logs_directory(&config.data_root).map_err(|e| e.to_string())?;
    fs::create_dir_all(&dir).map_err(|e| format!("cannot create the log directory: {e}"))?;
    prune_expired_logs(&dir, today, config.retention_days);
    let appender = RollingFileAppender::builder()
        .rotation(Rotation::DAILY)
        .filename_prefix(APP_ID)
        .filename_suffix(LOG_FILE_SUFFIX)
        .max_log_files(config.retention_days.max(1))
        .build(&dir)
        .map_err(|e| format!("cannot open the log file: {e}"))?;
    let (writer, worker) = tracing_appender::non_blocking(appender);
    Ok((writer, worker, dir))
}

/// 初始化全局日志。
///
/// 写入 `<数据根>/logs/` 的按天滚动文件；目录不可用时降级为仅 stderr，不会 panic。
/// 级别由 [`level_env_var`] 指定的环境变量覆盖，语法同 `RUST_LOG`。
/// 全局 subscriber 只能设置一次，重复调用会在返回的守卫里记录降级原因。
///
/// # 参数
/// - `config`：日志配置
///
/// # 返回
/// 日志守卫，需在进程生命周期内持有。
///
/// # 示例
/// ```no_run
/// use snow_app_core::logging::{LogConfig, init_logging};
///
/// let guard = init_logging(LogConfig::new("D:/data/Cisox"));
/// tracing::info!("started");
/// drop(guard);
/// ```
pub fn init_logging(config: LogConfig) -> LogGuard {
    let env_value = std::env::var(level_env_var()).ok();
    let (subscriber, mut guard) = build_subscriber(&config, env_value.as_deref(), today_utc());
    if subscriber.try_init().is_err() {
        guard
            .degraded_reason
            .get_or_insert_with(|| "a global subscriber is already installed".to_string());
    }
    guard
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU32, Ordering};

    /// 创建独立的临时目录。
    fn temp_dir(tag: &str) -> PathBuf {
        static COUNTER: AtomicU32 = AtomicU32::new(0);
        let dir = std::env::temp_dir().join(format!(
            "snow-app-core-log-{}-{tag}-{}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// 日志与崩溃目录位于数据根之下。
    #[test]
    fn directories_are_under_data_root() {
        let root = Path::new("data-root");
        assert_eq!(logs_directory(root).unwrap(), root.join("logs"));
        assert_eq!(
            crash_directory(root).unwrap(),
            root.join("logs").join("crash")
        );
    }

    /// 拒绝 upstream 目录（忽略大小写）与空根。
    #[test]
    fn upstream_and_empty_roots_are_refused() {
        for bad in [
            r"C:\Users\u\AppData\Local\SnowShot\snow_shot",
            r"C:\x\snowshot",
        ] {
            assert_eq!(
                logs_directory(Path::new(bad)),
                Err(LogDirError::UpstreamLocation)
            );
            assert!(crash_directory(Path::new(bad)).is_err());
        }
        assert_eq!(logs_directory(Path::new("")), Err(LogDirError::EmptyRoot));
    }

    /// 被拒绝的数据根不会在磁盘上创建任何内容，且降级为 stderr。
    #[test]
    fn refused_root_creates_nothing_and_degrades() {
        let base = temp_dir("refuse");
        let upstream = base.join("SnowShot").join("snow_shot");
        let (subscriber, guard) = build_subscriber(&LogConfig::new(&upstream), None, 0);
        drop(subscriber);
        assert!(!upstream.exists());
        assert!(guard.log_dir().is_none() && guard.degraded_reason().is_some());
        fs::remove_dir_all(base).unwrap();
    }

    /// 文件名日期解析：合法、非法、非本应用文件。
    #[test]
    fn parses_log_file_names() {
        let ok = format!("{APP_ID}.2026-09-29.log");
        assert_eq!(log_file_day(&ok), Some(days_from_civil(2026, 9, 29)));
        assert_eq!(days_from_civil(1970, 1, 1), 0);
        assert!(log_file_day(&format!("{APP_ID}.2026-13-01.log")).is_none());
        assert!(log_file_day("other.2026-09-29.log").is_none());
        assert!(log_file_day(&format!("{APP_ID}.2026-09-29.txt")).is_none());
    }

    /// 保留天数：注入"今天"，只删过期的、只删本应用日志文件。
    #[test]
    fn prune_keeps_only_recent_days() {
        let dir = temp_dir("prune");
        let today = days_from_civil(2026, 9, 29);
        for day in 20..=29 {
            fs::write(dir.join(format!("{APP_ID}.2026-09-{day:02}.log")), b"x").unwrap();
        }
        fs::write(dir.join("unrelated.txt"), b"x").unwrap();
        let removed = prune_expired_logs(&dir, today, 7);
        assert_eq!(removed, 3);
        assert!(!dir.join(format!("{APP_ID}.2026-09-22.log")).exists());
        assert!(dir.join(format!("{APP_ID}.2026-09-23.log")).exists());
        assert!(dir.join("unrelated.txt").exists());
        fs::remove_dir_all(dir).unwrap();
    }

    /// 环境变量覆盖级别；非法值回退默认。
    #[test]
    fn env_overrides_level() {
        assert_eq!(build_filter(Some("debug"), "info").to_string(), "debug");
        assert_eq!(build_filter(None, "warn").to_string(), "warn");
        assert_eq!(build_filter(Some("[[bad"), "warn").to_string(), "warn");
        assert!(level_env_var().starts_with(&APP_ID.to_ascii_uppercase()));
    }

    /// 端到端：事件写入数据根下 logs/ 的当日文件，且被过滤的级别不落盘。
    #[test]
    fn writes_events_to_daily_file() {
        let root = temp_dir("write");
        let (subscriber, guard) = build_subscriber(&LogConfig::new(&root), None, today_utc());
        tracing::subscriber::with_default(subscriber, || {
            tracing::info!("hello-visible");
            tracing::debug!("hello-filtered");
        });
        let dir = guard.log_dir().unwrap().to_path_buf();
        assert_eq!(dir, root.join("logs"));
        drop(guard);
        let file = fs::read_dir(&dir)
            .unwrap()
            .flatten()
            .find(|e| log_file_day(&e.file_name().to_string_lossy()).is_some())
            .expect("应生成当日日志文件");
        let text = fs::read_to_string(file.path()).unwrap();
        assert!(text.contains("hello-visible"));
        assert!(!text.contains("hello-filtered"));
        fs::remove_dir_all(root).unwrap();
    }
}
