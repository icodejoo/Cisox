//! 本地崩溃转储（方案 §10 T3）：只落盘，不上传、不联网。
//!
//! Windows：顶层异常过滤器 + panic hook，写 `.dmp`（minidump）与 `.txt` 摘要。
//! 其他平台：仅 panic hook 写 `.txt` 摘要，不产生 minidump（[`dump_supported`] 为 `false`）。

use std::backtrace::Backtrace;
use std::fmt::Write as _;
use std::fs;
use std::io;
use std::panic::PanicHookInfo;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

#[cfg(windows)]
mod win32;

/// 崩溃报告文件名前缀。
pub const REPORT_FILE_PREFIX: &str = "crash-";
/// minidump 扩展名。
pub const DUMP_EXTENSION: &str = "dmp";
/// 人类可读摘要扩展名。
pub const SUMMARY_EXTENSION: &str = "txt";
/// 默认保留的崩溃报告份数。
pub const DEFAULT_MAX_REPORTS: usize = 10;
/// 一天的秒数。
const SECONDS_PER_DAY: u64 = 86_400;
/// 无名线程的显示名。
const UNNAMED_THREAD: &str = "<unnamed>";

/// 已安装的配置（全局唯一，供 panic hook 与异常过滤器读取）。
static CONFIG: OnceLock<CrashDumpConfig> = OnceLock::new();
/// 全局重入标志：崩溃处理期间再次崩溃时直接放弃。
static HANDLING: ReentryFlag = ReentryFlag::new();

/// 崩溃转储配置。
#[derive(Debug, Clone)]
pub struct CrashDumpConfig {
    /// 转储输出目录（应位于数据根的 `logs/crash` 下）。
    pub crash_dir: PathBuf,
    /// 产品名，写入摘要。
    pub app_name: String,
    /// 应用版本，写入文件名与摘要。
    pub app_version: String,
    /// 目录内最多保留的报告份数。
    pub max_reports: usize,
}

/// 崩溃类型。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CrashKind {
    /// Rust panic。
    Panic {
        /// panic 消息。
        message: String,
        /// 触发位置（`文件:行:列`）。
        location: Option<String>,
    },
    /// 系统异常（如访问违规）。
    Exception {
        /// 异常代码。
        code: u32,
        /// 异常地址。
        address: u64,
    },
}

/// 一次崩溃的现场信息。
#[derive(Debug, Clone)]
pub struct CrashInfo {
    /// 崩溃类型。
    pub kind: CrashKind,
    /// 崩溃线程名。
    pub thread_name: String,
    /// 调用栈文本。
    pub backtrace: String,
}

/// 写出的报告文件。
#[derive(Debug, Clone)]
pub struct CrashReportFiles {
    /// 摘要文件路径。
    pub summary: PathBuf,
    /// minidump 路径；不支持或写入失败时为 `None`。
    pub dump: Option<PathBuf>,
}

/// 安装失败原因。
#[derive(Debug)]
pub enum CrashInstallError {
    /// 已经安装过。
    AlreadyInstalled,
    /// 无法创建转储目录。
    Io(io::Error),
}

impl std::fmt::Display for CrashInstallError {
    /// 输出简短英文原因。
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::AlreadyInstalled => f.write_str("the crash handler is already installed"),
            Self::Io(e) => write!(f, "cannot create the crash directory: {e}"),
        }
    }
}

impl std::error::Error for CrashInstallError {}

/// 安装结果句柄。
#[derive(Debug, Clone)]
pub struct CrashGuard {
    /// 转储目录。
    pub crash_dir: PathBuf,
    /// 当前平台是否会产生 minidump。
    pub dump_supported: bool,
}

/// 重入标志：同一时刻只允许一个处理流程。
struct ReentryFlag(AtomicBool);

/// 持有期间标志保持占用，drop 时释放。
struct ReentryGuard<'a>(&'a ReentryFlag);

impl ReentryFlag {
    /// 创建未占用的标志。
    const fn new() -> Self {
        Self(AtomicBool::new(false))
    }

    /// 尝试占用；已被占用（重入）时返回 `None`。
    fn try_enter(&self) -> Option<ReentryGuard<'_>> {
        (!self.0.swap(true, Ordering::AcqRel)).then_some(ReentryGuard(self))
    }
}

impl Drop for ReentryGuard<'_> {
    /// 释放占用。
    fn drop(&mut self) {
        self.0.0.store(false, Ordering::Release);
    }
}

/// 当前平台是否会生成 minidump（仅 Windows）。
///
/// # 示例
/// ```
/// assert_eq!(snow_platform::crash::dump_supported(), cfg!(windows));
/// ```
pub fn dump_supported() -> bool {
    cfg!(windows)
}

/// 由公历日期字段格式化 Unix 秒为 `YYYYMMDDTHHMMSSZ`（UTC）。
///
/// # 参数
/// - `unix_secs`：Unix 时间戳（秒）
///
/// # 示例
/// ```
/// assert_eq!(snow_platform::crash::format_timestamp(0), "19700101T000000Z");
/// ```
pub fn format_timestamp(unix_secs: u64) -> String {
    let days = (unix_secs / SECONDS_PER_DAY) as i64;
    let rem = unix_secs % SECONDS_PER_DAY;
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!(
        "{year:04}{month:02}{day:02}T{:02}{:02}{:02}Z",
        rem / 3600,
        rem % 3600 / 60,
        rem % 60
    )
}

/// 报告文件主名：`crash-<UTC 时间戳>-v<版本>`（版本中的非常规字符替换为 `_`）。
pub fn report_stem(config: &CrashDumpConfig, unix_secs: u64) -> String {
    let version: String = config
        .app_version
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '.' || c == '-' {
                c
            } else {
                '_'
            }
        })
        .collect();
    format!(
        "{REPORT_FILE_PREFIX}{}-v{version}",
        format_timestamp(unix_secs)
    )
}

/// 渲染人类可读的崩溃摘要。
///
/// # 参数
/// - `config`：转储配置
/// - `info`：崩溃现场
/// - `unix_secs`：崩溃时间
/// - `dump_name`：同批 minidump 文件名，没有则为 `None`
pub fn render_summary(
    config: &CrashDumpConfig,
    info: &CrashInfo,
    unix_secs: u64,
    dump_name: Option<&str>,
) -> String {
    let mut out = String::new();
    let _ = writeln!(out, "{} crash report", config.app_name);
    let _ = writeln!(out, "version: {}", config.app_version);
    let _ = writeln!(out, "time: {}", format_timestamp(unix_secs));
    let _ = writeln!(out, "thread: {}", info.thread_name);
    match &info.kind {
        CrashKind::Panic { message, location } => {
            let _ = writeln!(out, "kind: panic");
            let _ = writeln!(out, "message: {message}");
            let _ = writeln!(
                out,
                "location: {}",
                location.as_deref().unwrap_or("unknown")
            );
        }
        CrashKind::Exception { code, address } => {
            let _ = writeln!(out, "kind: exception");
            let _ = writeln!(out, "code: 0x{code:08X}");
            let _ = writeln!(out, "address: 0x{address:X}");
        }
    }
    let _ = writeln!(out, "minidump: {}", dump_name.unwrap_or("none"));
    let _ = writeln!(out, "\nbacktrace:\n{}", info.backtrace);
    out
}

/// 只保留最新的 `max` 份报告（按文件主名中的时间戳排序），返回删除的文件数。
///
/// 仅处理 `crash-*.dmp` 与 `crash-*.txt`，不触碰其他文件。
///
/// # 参数
/// - `dir`：转储目录
/// - `max`：保留份数（至少 1）
pub fn prune_reports(dir: &Path, max: usize) -> usize {
    let Ok(entries) = fs::read_dir(dir) else {
        return 0;
    };
    let mut files: Vec<(String, PathBuf)> = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        let is_report = path
            .extension()
            .is_some_and(|e| e == DUMP_EXTENSION || e == SUMMARY_EXTENSION);
        let stem = path.file_stem().map(|s| s.to_string_lossy().into_owned());
        if let (true, Some(stem)) = (is_report, stem)
            && stem.starts_with(REPORT_FILE_PREFIX)
        {
            files.push((stem, path));
        }
    }
    let mut stems: Vec<&String> = files.iter().map(|(s, _)| s).collect();
    stems.sort();
    stems.dedup();
    let keep_from = stems.len().saturating_sub(max.max(1));
    let expired: Vec<String> = stems[..keep_from].iter().map(|s| (*s).clone()).collect();
    files
        .iter()
        .filter(|(stem, _)| expired.contains(stem))
        .filter(|(_, path)| fs::remove_file(path).is_ok())
        .count()
}

/// 写出一份崩溃报告（minidump 可选 + 摘要），并按上限清理旧报告。
///
/// # 参数
/// - `config`：转储配置
/// - `info`：崩溃现场
/// - `unix_secs`：崩溃时间
/// - `write_dump`：写 minidump 的回调，返回是否成功；`None` 表示不写
///
/// # 返回
/// 实际写出的文件；摘要写入失败时返回 IO 错误。
pub fn write_report(
    config: &CrashDumpConfig,
    info: &CrashInfo,
    unix_secs: u64,
    write_dump: Option<&dyn Fn(&Path) -> bool>,
) -> io::Result<CrashReportFiles> {
    fs::create_dir_all(&config.crash_dir)?;
    let stem = report_stem(config, unix_secs);
    let dump_path = config.crash_dir.join(format!("{stem}.{DUMP_EXTENSION}"));
    let dump = write_dump
        .filter(|write| write(&dump_path))
        .map(|_| dump_path);
    let dump_name = dump
        .as_ref()
        .and_then(|p| p.file_name())
        .map(|n| n.to_string_lossy().into_owned());
    let summary = config.crash_dir.join(format!("{stem}.{SUMMARY_EXTENSION}"));
    fs::write(
        &summary,
        render_summary(config, info, unix_secs, dump_name.as_deref()),
    )?;
    prune_reports(&config.crash_dir, config.max_reports);
    Ok(CrashReportFiles { summary, dump })
}

/// 当前 Unix 秒。
fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// 从 panic 信息提取消息文本。
fn panic_message(info: &PanicHookInfo<'_>) -> String {
    let payload = info.payload();
    payload
        .downcast_ref::<&str>()
        .map(|s| (*s).to_string())
        .or_else(|| payload.downcast_ref::<String>().cloned())
        .unwrap_or_else(|| "non-string panic payload".to_string())
}

/// panic 时的处理：写摘要（Windows 同时写 minidump），处理完释放重入标志。
fn handle_panic(info: &PanicHookInfo<'_>) {
    let Some(config) = CONFIG.get() else { return };
    let Some(_guard) = HANDLING.try_enter() else {
        return;
    };
    let crash = CrashInfo {
        kind: CrashKind::Panic {
            message: panic_message(info),
            location: info
                .location()
                .map(|l| format!("{}:{}:{}", l.file(), l.line(), l.column())),
        },
        thread_name: std::thread::current()
            .name()
            .unwrap_or(UNNAMED_THREAD)
            .to_string(),
        backtrace: Backtrace::force_capture().to_string(),
    };
    #[cfg(windows)]
    let dump_writer: Option<&dyn Fn(&Path) -> bool> =
        Some(&|path| win32::write_minidump(path, None));
    #[cfg(not(windows))]
    let dump_writer: Option<&dyn Fn(&Path) -> bool> = None;
    let _ = write_report(config, &crash, now_secs(), dump_writer);
}

/// 安装本地崩溃转储：panic hook（全平台）与顶层异常过滤器（仅 Windows）。
///
/// 只落盘，不上传、不联网。安装时会创建转储目录并按上限清理旧报告。
/// 原有的 panic hook 会在写完报告后继续被调用。
///
/// # 参数
/// - `config`：转储配置
///
/// # 返回
/// 句柄；重复安装或目录无法创建时返回错误。
///
/// # 示例
/// ```no_run
/// use snow_platform::crash::{CrashDumpConfig, DEFAULT_MAX_REPORTS, install};
///
/// let guard = install(CrashDumpConfig {
///     crash_dir: "D:/data/Cisox/logs/crash".into(),
///     app_name: "Cisox".into(),
///     app_version: "0.1.0".into(),
///     max_reports: DEFAULT_MAX_REPORTS,
/// })
/// .unwrap();
/// println!("minidump: {}", guard.dump_supported);
/// ```
pub fn install(config: CrashDumpConfig) -> Result<CrashGuard, CrashInstallError> {
    fs::create_dir_all(&config.crash_dir).map_err(CrashInstallError::Io)?;
    let guard = CrashGuard {
        crash_dir: config.crash_dir.clone(),
        dump_supported: dump_supported(),
    };
    prune_reports(&config.crash_dir, config.max_reports);
    CONFIG
        .set(config)
        .map_err(|_| CrashInstallError::AlreadyInstalled)?;
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        handle_panic(info);
        previous(info);
    }));
    #[cfg(windows)]
    win32::install_exception_filter();
    Ok(guard)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::{Command, Stdio};
    use std::sync::atomic::AtomicU32;

    /// 子进程模式环境变量名。
    const CHILD_MODE_ENV: &str = "SNOW_CRASH_TEST_MODE";
    /// 子进程转储目录环境变量名。
    const CHILD_DIR_ENV: &str = "SNOW_CRASH_TEST_DIR";
    /// 子进程 panic 模式。
    const MODE_PANIC: &str = "panic";
    /// 子进程访问违规模式。
    const MODE_VIOLATION: &str = "violation";
    /// 子进程入口测试的完整路径。
    const CHILD_TEST_PATH: &str = "crash::tests::crash_child_entry";
    /// minidump 文件头魔数。
    const DUMP_MAGIC: &[u8; 4] = b"MDMP";

    /// 创建独立的临时目录。
    fn temp_dir(tag: &str) -> PathBuf {
        static COUNTER: AtomicU32 = AtomicU32::new(0);
        let dir = std::env::temp_dir().join(format!(
            "snow-platform-crash-{}-{tag}-{}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// 测试用配置。
    fn config(dir: &Path, max: usize) -> CrashDumpConfig {
        CrashDumpConfig {
            crash_dir: dir.to_path_buf(),
            app_name: "TestApp".into(),
            app_version: "1.2.3+beta".into(),
            max_reports: max,
        }
    }

    /// 测试用崩溃现场。
    fn sample_info() -> CrashInfo {
        CrashInfo {
            kind: CrashKind::Panic {
                message: "boom".into(),
                location: Some("src/x.rs:1:2".into()),
            },
            thread_name: "worker-1".into(),
            backtrace: "frame0\nframe1".into(),
        }
    }

    /// 时间戳格式：纪元、闰日、任意时刻。
    #[test]
    fn formats_timestamps() {
        assert_eq!(format_timestamp(0), "19700101T000000Z");
        assert_eq!(format_timestamp(951_782_400), "20000229T000000Z");
        assert_eq!(format_timestamp(1_790_000_000), "20260921T141320Z");
    }

    /// 文件名含时间戳与（净化后的）版本。
    #[test]
    fn stem_has_timestamp_and_version() {
        let c = config(Path::new("x"), 3);
        assert_eq!(report_stem(&c, 0), "crash-19700101T000000Z-v1.2.3_beta");
    }

    /// 摘要内容：panic 信息、位置、线程、backtrace、转储文件名。
    #[test]
    fn summary_contains_key_fields() {
        let c = config(Path::new("x"), 3);
        let text = render_summary(&c, &sample_info(), 0, Some("a.dmp"));
        for needle in [
            "TestApp crash report",
            "version: 1.2.3+beta",
            "thread: worker-1",
            "kind: panic",
            "message: boom",
            "location: src/x.rs:1:2",
            "minidump: a.dmp",
            "frame1",
        ] {
            assert!(text.contains(needle), "缺少 {needle}");
        }
        let exception = CrashInfo {
            kind: CrashKind::Exception {
                code: 0xC000_0005,
                address: 0x1234,
            },
            ..sample_info()
        };
        let text = render_summary(&c, &exception, 0, None);
        assert!(text.contains("code: 0xC0000005") && text.contains("minidump: none"));
    }

    /// 留存上限：只留最新 N 份，成对删除，不碰无关文件。
    #[test]
    fn prune_keeps_newest_reports() {
        let dir = temp_dir("prune");
        let c = config(&dir, 3);
        for i in 0..6u64 {
            let stem = report_stem(&c, i * 1000);
            fs::write(dir.join(format!("{stem}.dmp")), b"d").unwrap();
            fs::write(dir.join(format!("{stem}.txt")), b"t").unwrap();
        }
        fs::write(dir.join("notes.txt"), b"keep").unwrap();
        assert_eq!(prune_reports(&dir, 3), 6);
        let mut names: Vec<String> = fs::read_dir(&dir)
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        assert_eq!(names.len(), 7);
        assert!(names.contains(&"notes.txt".to_string()));
        assert!(!names.iter().any(|n| n.contains("19700101T000000Z")));
        assert!(names.iter().any(|n| n.contains("19700101T01")));
        fs::remove_dir_all(dir).unwrap();
    }

    /// write_report 写出摘要与（模拟的）转储，并触发留存清理。
    #[test]
    fn write_report_creates_files_and_prunes() {
        let dir = temp_dir("write");
        let c = config(&dir, 2);
        let fake_dump = |p: &Path| fs::write(p, b"MDMPfake").is_ok();
        for t in [100, 200, 300] {
            write_report(&c, &sample_info(), t, Some(&fake_dump)).unwrap();
        }
        let last = write_report(&c, &sample_info(), 400, None).unwrap();
        assert!(last.dump.is_none() && last.summary.is_file());
        let text = fs::read_to_string(&last.summary).unwrap();
        assert!(text.contains("minidump: none"));
        let count = fs::read_dir(&dir).unwrap().count();
        assert_eq!(count, 3, "保留 2 份：一份含 dmp+txt，另一份仅 txt");
        fs::remove_dir_all(dir).unwrap();
    }

    /// 重入保护：占用期间再次进入得到 None，释放后可再次进入。
    #[test]
    fn reentry_is_rejected() {
        let flag = ReentryFlag::new();
        let first = flag.try_enter();
        assert!(first.is_some());
        assert!(flag.try_enter().is_none());
        drop(first);
        assert!(flag.try_enter().is_some());
    }

    /// 子进程入口：仅在带环境变量时安装处理器并故意崩溃，平时空跑通过。
    #[test]
    fn crash_child_entry() {
        let (Ok(mode), Ok(dir)) = (std::env::var(CHILD_MODE_ENV), std::env::var(CHILD_DIR_ENV))
        else {
            return;
        };
        install(config(Path::new(&dir), DEFAULT_MAX_REPORTS)).unwrap();
        if mode == MODE_PANIC {
            panic!("intentional test panic");
        }
        // 故意触发访问违规（仅子进程）
        unsafe { std::ptr::write_volatile(std::ptr::null_mut::<u32>(), 1) };
    }

    /// 启动子进程并按模式崩溃，返回转储目录与退出是否失败。
    fn run_crash_child(mode: &str, tag: &str) -> (PathBuf, bool) {
        let dir = temp_dir(tag);
        let status = Command::new(std::env::current_exe().unwrap())
            .args(["--exact", CHILD_TEST_PATH, "--test-threads=1"])
            .env(CHILD_MODE_ENV, mode)
            .env(CHILD_DIR_ENV, &dir)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .unwrap();
        (dir, !status.success())
    }

    /// 在目录里找指定扩展名的报告文件。
    fn find_ext(dir: &Path, ext: &str) -> Option<PathBuf> {
        fs::read_dir(dir)
            .ok()?
            .flatten()
            .map(|e| e.path())
            .find(|p| p.extension().is_some_and(|e| e == ext))
    }

    /// 真实 panic：子进程失败，产生 txt 摘要；Windows 上还产生有效 minidump。
    #[test]
    fn real_panic_writes_report() {
        let (dir, failed) = run_crash_child(MODE_PANIC, "panic");
        assert!(failed, "子进程应以失败退出");
        let summary = find_ext(&dir, SUMMARY_EXTENSION).expect("应有 txt 摘要");
        let text = fs::read_to_string(summary).unwrap();
        assert!(text.contains("intentional test panic") && text.contains("kind: panic"));
        assert!(text.contains("thread:") && text.contains("backtrace:"));
        if dump_supported() {
            let bytes = fs::read(find_ext(&dir, DUMP_EXTENSION).expect("应有 dmp")).unwrap();
            assert!(bytes.len() > 1024 && bytes.starts_with(DUMP_MAGIC));
        }
        fs::remove_dir_all(dir).unwrap();
    }

    /// 真实访问违规（仅 Windows）：过滤器写出有效 minidump 与含异常码的摘要。
    #[cfg(windows)]
    #[test]
    fn real_access_violation_writes_minidump() {
        let (dir, failed) = run_crash_child(MODE_VIOLATION, "av");
        assert!(failed, "子进程应以失败退出");
        let bytes = fs::read(find_ext(&dir, DUMP_EXTENSION).expect("应有 dmp")).unwrap();
        assert!(bytes.len() > 1024 && bytes.starts_with(DUMP_MAGIC));
        let text = fs::read_to_string(find_ext(&dir, SUMMARY_EXTENSION).unwrap()).unwrap();
        assert!(text.contains("kind: exception") && text.contains("0xC0000005"));
        fs::remove_dir_all(dir).unwrap();
    }
}
