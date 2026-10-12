//! 宿主日志：一个只依赖 `tracing` 门面的最小 `Subscriber`，写 `logs/host.log`，限大小滚动。
//!
//! 宿主不引入 `tracing-subscriber`（它带 regex、解析器等一堆东西，违背“个位数 MB”的目标，
//! 且被 `workspace-guard` 禁止）。`snow-platform`、`snow-shell-core` 里的 `tracing::` 日志
//! 经这里落盘。只记 INFO 及以上；日志文件超过 [`MAX_LOG_BYTES`] 就改名成 `host.log.1`（覆盖旧的）重新开始，
//! 所以磁盘占用上限是两个文件。

use snow_platform::local_time;
use std::fmt::Write as _;
use std::fs::{File, OpenOptions};
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use tracing::field::{Field, Visit};
use tracing::span::{Attributes, Id, Record};
use tracing::{Event, Level, Metadata, Subscriber};

/// 单个日志文件的大小上限（字节）。
pub const MAX_LOG_BYTES: u64 = 256 * 1024;

/// 滚动后旧日志的扩展名后缀。
const ROLLED_SUFFIX: &str = "1";

/// 带大小滚动的日志文件。
struct RollingFile {
    /// 当前日志路径。
    path: PathBuf,
    /// 打开的文件（打开失败时为 `None`，日志静默丢弃）。
    file: Option<File>,
    /// 当前文件已写字节数。
    size: u64,
    /// 滚动阈值（字节）。
    limit: u64,
}

impl RollingFile {
    /// 打开（必要时创建目录与文件）；失败返回 `None` 版本的空壳，日志不可用但不影响宿主。
    fn open(path: &Path, limit: u64) -> Self {
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let file = OpenOptions::new().create(true).append(true).open(path).ok();
        let size = file
            .as_ref()
            .and_then(|f| f.metadata().ok())
            .map_or(0, |m| m.len());
        Self {
            path: path.to_path_buf(),
            file,
            size,
            limit,
        }
    }

    /// 写一行；写入前若会超过上限就先滚动。
    fn write_line(&mut self, line: &str) {
        let bytes = line.len() as u64 + 1;
        if self.size > 0 && self.size + bytes > self.limit {
            self.roll();
        }
        if let Some(file) = self.file.as_mut()
            && writeln!(file, "{line}").is_ok()
        {
            self.size += bytes;
        }
    }

    /// 当前文件改名为 `host.log.1`（覆盖旧的）并重新开始。
    fn roll(&mut self) {
        self.file = None;
        let mut rolled = self.path.clone().into_os_string();
        rolled.push(".");
        rolled.push(ROLLED_SUFFIX);
        let _ = std::fs::rename(&self.path, PathBuf::from(rolled));
        self.file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)
            .ok();
        self.size = 0;
    }
}

/// 把事件的字段拼成文本：`message` 在前，其余 `键=值`。
#[derive(Default)]
struct LineVisitor {
    /// `message` 字段。
    message: String,
    /// 其余字段（带前导空格）。
    fields: String,
}

impl Visit for LineVisitor {
    /// 记录一个字段。
    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        if field.name() == "message" {
            let _ = write!(self.message, "{value:?}");
        } else {
            let _ = write!(self.fields, " {}={value:?}", field.name());
        }
    }
}

/// 写 `host.log` 的 Subscriber。
pub struct HostLog {
    /// 日志文件（多线程写，加锁）。
    file: Mutex<RollingFile>,
}

impl HostLog {
    /// 在指定路径创建日志（目录不存在会创建）。
    ///
    /// # 参数
    /// - `path`：日志文件路径。
    /// - `limit`：单个文件的大小上限；超过就滚动。
    pub fn new(path: &Path, limit: u64) -> Self {
        Self {
            file: Mutex::new(RollingFile::open(path, limit)),
        }
    }

    /// 把事件格式化成一行：`日期 时间 级别 目标: 消息 字段=值`。
    fn format(event: &Event<'_>) -> String {
        let now = local_time::now();
        let mut visitor = LineVisitor::default();
        event.record(&mut visitor);
        let meta = event.metadata();
        format!(
            "{:04}-{:02}-{:02} {:02}:{:02}:{:02} {:<5} {}: {}{}",
            now.year,
            now.month,
            now.day,
            now.hour,
            now.minute,
            now.second,
            meta.level(),
            meta.target(),
            visitor.message,
            visitor.fields
        )
    }
}

impl Subscriber for HostLog {
    /// 只记 INFO 及以上。
    fn enabled(&self, metadata: &Metadata<'_>) -> bool {
        *metadata.level() <= Level::INFO
    }

    /// 宿主不使用 span；给每个 span 同一个 id 即可。
    fn new_span(&self, _span: &Attributes<'_>) -> Id {
        Id::from_u64(1)
    }

    /// 不使用 span。
    fn record(&self, _span: &Id, _values: &Record<'_>) {}

    /// 不使用 span。
    fn record_follows_from(&self, _span: &Id, _follows: &Id) {}

    /// 写一条事件。
    fn event(&self, event: &Event<'_>) {
        let line = Self::format(event);
        if let Ok(mut file) = self.file.lock() {
            file.write_line(&line);
        }
    }

    /// 不使用 span。
    fn enter(&self, _span: &Id) {}

    /// 不使用 span。
    fn exit(&self, _span: &Id) {}
}

/// 安装为全局日志；已有全局订阅者或路径不可写时返回 `false`（宿主照常运行，只是没有日志）。
///
/// # 参数
/// - `path`：日志文件路径（`snow_data_paths::host_log_path(数据根)`）。
///
/// ```no_run
/// let ok = snow_tray_host::host_log::init(std::path::Path::new("logs/host.log"));
/// println!("日志已启用: {ok}");
/// ```
pub fn init(path: &Path) -> bool {
    tracing::subscriber::set_global_default(HostLog::new(path, MAX_LOG_BYTES)).is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 创建本进程内唯一的临时目录。
    fn scratch(tag: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("snow-tray-host-log-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    /// 事件落盘：带日期、级别、目标、消息与字段；DEBUG 被过滤；目录自动创建。
    #[test]
    fn writes_info_events_with_fields_and_filters_debug() {
        let dir = scratch("format");
        let path = dir.join("logs").join("host.log");
        let log = HostLog::new(&path, MAX_LOG_BYTES);
        tracing::subscriber::with_default(log, || {
            tracing::info!(pid = 42, "UI 已拉起");
            tracing::warn!("提示 {}", "x");
            tracing::debug!("不应写入");
        });
        let text = std::fs::read_to_string(&path).unwrap();
        let lines: Vec<_> = text.lines().collect();
        assert_eq!(lines.len(), 2, "{text}");
        assert!(lines[0].contains("INFO"));
        assert!(lines[0].contains("UI 已拉起"));
        assert!(lines[0].contains("pid=42"));
        assert!(lines[1].contains("WARN"));
        assert!(lines[1].contains("提示 x"));
        // 以 `YYYY-MM-DD HH:MM:SS` 开头
        assert_eq!(&lines[0][4..5], "-");
        assert_eq!(&lines[0][13..14], ":");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 滚动：超过上限就把旧文件改名为 `.1`（覆盖更旧的），当前文件重新开始；磁盘上只有两个文件。
    #[test]
    fn rolls_at_the_size_limit() {
        let dir = scratch("roll");
        let path = dir.join("host.log");
        let log = HostLog::new(&path, 400);
        tracing::subscriber::with_default(log, || {
            for i in 0..40 {
                tracing::info!("第 {i} 条日志，凑一些长度凑一些长度");
            }
        });
        let rolled = dir.join("host.log.1");
        assert!(rolled.exists(), "应滚动出 host.log.1");
        let current = std::fs::metadata(&path).unwrap().len();
        let old = std::fs::metadata(&rolled).unwrap().len();
        assert!(
            current <= 400 && old <= 400,
            "两个文件都不超过上限: {current} {old}"
        );
        assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 2);
        // 最后一条一定在当前文件里
        assert!(std::fs::read_to_string(&path).unwrap().contains("第 39 条"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 重新打开已有文件：在原有大小上继续累计（不会因为重启而无限增长）。
    #[test]
    fn reopening_continues_from_existing_size() {
        let dir = scratch("reopen");
        let path = dir.join("host.log");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(&path, "x".repeat(390)).unwrap();
        let log = HostLog::new(&path, 400);
        tracing::subscriber::with_default(log, || {
            tracing::info!("触发滚动的一条日志");
        });
        assert!(dir.join("host.log.1").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 路径不可写时不 panic，日志静默丢弃。
    #[test]
    fn unwritable_path_is_silent() {
        let dir = scratch("unwritable");
        std::fs::create_dir_all(&dir).unwrap();
        // 目标路径是目录：打开文件失败
        let log = HostLog::new(&dir, MAX_LOG_BYTES);
        tracing::subscriber::with_default(log, || {
            tracing::info!("没有地方可写");
        });
        let _ = std::fs::remove_dir_all(&dir);
    }
}
