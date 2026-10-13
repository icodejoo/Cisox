//! 读取 `host/spec.json`（宿主侧）。
//!
//! 文件由 UI 进程原子写入（临时文件 + 改名覆盖，见 `snow-shot` 的 `host_spec_sync`），所以读到的
//! 要么是旧版要么是新版，不会是半份。Windows 上改名瞬间偶尔会因共享冲突读失败，这类 IO 错误
//! 短暂重试；文件不存在直接如实返回，由调用方决定退回内置最小菜单还是去生成快照。

use crate::spec::{HostSpec, SpecError};
use std::fmt;
use std::path::Path;
use std::time::Duration;

/// 读快照时遇到 IO 错误的重试次数（含第一次）。
const READ_ATTEMPTS: u32 = 4;

/// 流式读取的缓冲字节数（够小，不抬宿主的堆峰值）。
const READ_BUFFER_BYTES: usize = 4096;

/// 重试间隔。
const READ_RETRY_DELAY: Duration = Duration::from_millis(20);

/// 读取快照失败的原因。
#[derive(Debug)]
pub enum SpecFileError {
    /// 文件不存在（UI 还没生成过）。
    Missing,
    /// IO 错误（重试后仍失败）。
    Io(String),
    /// 内容不合法（损坏、版本不认识、自相矛盾）。
    Spec(SpecError),
}

impl fmt::Display for SpecFileError {
    /// 输出面向日志的错误描述。
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SpecFileError::Missing => write!(f, "快照文件不存在"),
            SpecFileError::Io(e) => write!(f, "读取快照失败: {e}"),
            SpecFileError::Spec(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for SpecFileError {}

/// 读取并校验快照文件。
///
/// # 参数
/// - `path`：快照路径（通常是 `snow_data_paths::host_spec_path(数据根)`）。
///
/// # 返回
/// 通过校验的快照；文件不存在是 [`SpecFileError::Missing`]，内容问题是 [`SpecFileError::Spec`]。
///
/// ```rust
/// use snow_host_protocol::spec_file::{read_spec, SpecFileError};
/// let missing = std::env::temp_dir().join("snow-host-protocol-doc-missing.json");
/// assert!(matches!(read_spec(&missing), Err(SpecFileError::Missing)));
/// ```
pub fn read_spec(path: &Path) -> Result<HostSpec, SpecFileError> {
    let mut last_error = String::new();
    for attempt in 0..READ_ATTEMPTS {
        match std::fs::File::open(path) {
            // 小缓冲流式读：不把整份快照读成一个大字符串（宿主启动的堆峰值）
            Ok(file) => {
                return HostSpec::from_reader(std::io::BufReader::with_capacity(
                    READ_BUFFER_BYTES,
                    file,
                ))
                .map_err(SpecFileError::Spec);
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return Err(SpecFileError::Missing);
            }
            Err(e) => {
                last_error = e.to_string();
                if attempt + 1 < READ_ATTEMPTS {
                    std::thread::sleep(READ_RETRY_DELAY);
                }
            }
        }
    }
    Err(SpecFileError::Io(last_error))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::SPEC_VERSION;
    use crate::spec::{GateSpec, HostFlags, HostStrings, IconRgba, TraySection, TrayTheme};

    /// 最小合法快照。
    fn minimal(rev: u64) -> HostSpec {
        HostSpec {
            spec_version: SPEC_VERSION,
            rev,
            hotkeys: vec![],
            tray: TraySection {
                enabled: true,
                tooltip: "t".into(),
                paused_tooltip: "p".into(),
                icon_rgba: IconRgba {
                    width: 1,
                    height: 1,
                    rgba: vec![0; 4].into(),
                },
                menu: vec![],
                on_left: None,
                on_middle: None,
                on_double: None,
                theme: TrayTheme::System,
            },
            mouse: vec![],
            gate: GateSpec::default(),
            strings: HostStrings::default(),
            flags: HostFlags::default(),
        }
    }

    /// 创建本进程内唯一的临时目录。
    fn scratch(tag: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "snow-host-protocol-spec-file-{tag}-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// 文件不存在如实报 Missing，不重试等待。
    #[test]
    fn missing_file_is_reported_immediately() {
        let dir = scratch("missing");
        let start = std::time::Instant::now();
        assert!(matches!(
            read_spec(&dir.join("spec.json")),
            Err(SpecFileError::Missing)
        ));
        assert!(start.elapsed() < Duration::from_millis(15));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 合法文件读回同一份快照；损坏内容、未知版本给出 Spec 错误。
    #[test]
    fn reads_valid_and_rejects_bad_content() {
        let dir = scratch("content");
        let path = dir.join("spec.json");
        let spec = minimal(7);
        std::fs::write(&path, spec.to_json().unwrap()).unwrap();
        assert_eq!(read_spec(&path).unwrap(), spec);

        std::fs::write(&path, "{ broken").unwrap();
        assert!(matches!(
            read_spec(&path),
            Err(SpecFileError::Spec(SpecError::Malformed(_)))
        ));
        std::fs::write(&path, r#"{"spec_version": 42}"#).unwrap();
        assert!(matches!(
            read_spec(&path),
            Err(SpecFileError::Spec(SpecError::UnsupportedVersion(42)))
        ));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 路径是目录（读取必定 IO 失败）：重试后返回 Io 而不是 panic。
    #[test]
    fn unreadable_path_returns_io_error() {
        let dir = scratch("io");
        assert!(matches!(read_spec(&dir), Err(SpecFileError::Io(_))));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
