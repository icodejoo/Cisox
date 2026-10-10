//! 下载底层：系统工具定位、不弹窗子进程、curl 命令构造、certutil 哈希与可续传下载。
//!
//! 只依赖标准库；真正的下载由系统自带 `curl.exe` 完成，哈希由 `certutil` 计算。

use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

/// Windows 不弹控制台窗口的进程创建标志。
#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;
/// curl 连接超时（秒）。
const CURL_CONNECT_TIMEOUT: &str = "20";
/// curl 失败重试次数。
const CURL_RETRIES: &str = "3";
/// 轮询下载进度与取消标志的间隔。
const POLL_INTERVAL: Duration = Duration::from_millis(250);
/// `curl` 工具名（错误里的技术名词）。
pub const TOOL_CURL: &str = "curl";
/// `certutil` 工具名（错误里的技术名词）。
pub const TOOL_CERTUTIL: &str = "certutil";
/// 追加给 curl 的代理参数（空表示不走代理）。
static CURL_PROXY_ARGS: std::sync::Mutex<Vec<String>> = std::sync::Mutex::new(Vec::new());

/// 下载底层的错误；调用方按需转成自己的错误类型。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NetError {
    /// 无法启动外部工具。
    RunTool {
        /// 工具名。
        tool: &'static str,
        /// 系统给出的原因。
        detail: String,
    },
    /// 等待外部工具结束失败。
    WaitTool {
        /// 工具名。
        tool: &'static str,
        /// 系统给出的原因。
        detail: String,
    },
    /// `certutil` 输出里找不到哈希。
    HashUnreadable {
        /// 被计算的文件路径。
        path: String,
    },
    /// curl 失败，或返回 0 却没产出文件。
    DownloadFailed {
        /// 下载地址。
        url: String,
        /// curl 的 stderr。
        detail: String,
    },
    /// 被取消。
    Cancelled,
    /// 改名失败。
    Rename(String),
}

/// 系统工具路径：优先 `%SystemRoot%\System32\<name>`，否则直接用名字走 PATH。
///
/// # 参数
/// - `name`：工具文件名，如 `curl.exe`。
///
/// # 返回
/// 可执行路径。
///
/// ```ignore
/// let tar = snow_net::system_tool("tar.exe");
/// ```
pub fn system_tool(name: &str) -> PathBuf {
    std::env::var_os("SystemRoot")
        .map(|root| PathBuf::from(root).join("System32").join(name))
        .filter(|p| p.is_file())
        .unwrap_or_else(|| PathBuf::from(name))
}

/// 构造不弹窗、stdin 置空的子进程命令。
///
/// # 参数
/// - `program`：程序路径。
///
/// # 返回
/// 尚未启动的命令。
pub fn quiet_command(program: &Path) -> Command {
    let mut command = Command::new(program);
    command.stdin(Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(CREATE_NO_WINDOW);
    }
    command
}

/// 设置此后所有 curl 下载追加的代理参数。
///
/// # 参数
/// - `args`：形如 `["--proxy", 地址]`；传空表示不走代理。
pub fn set_curl_proxy_args(args: Vec<String>) {
    if let Ok(mut guard) = CURL_PROXY_ARGS.lock() {
        *guard = args;
    }
}

/// 构造 curl 下载命令（写 `part`，支持续传、重试，带 `--ssl-no-revoke`，追加当前代理参数）。
///
/// # 参数
/// - `url`：下载地址。
/// - `part`：输出文件（已存在则从其末尾续传）。
///
/// # 返回
/// 尚未启动的命令，调用方可继续追加参数。
///
/// ```ignore
/// let out = snow_net::curl_command("https://a.b/x", Path::new("x.part")).output();
/// ```
pub fn curl_command(url: &str, part: &Path) -> Command {
    let mut command = quiet_command(&system_tool("curl.exe"));
    command
        .args([
            "--fail",
            "--location",
            "--silent",
            "--show-error",
            "--ssl-no-revoke",
        ])
        .args(["--connect-timeout", CURL_CONNECT_TIMEOUT])
        .args(["--retry", CURL_RETRIES, "-C", "-"]);
    if let Ok(guard) = CURL_PROXY_ARGS.lock() {
        command.args(guard.iter());
    }
    command.arg("-o").arg(part).arg(url);
    command
}

/// 判断一次 curl 结果是否算成功：退出码为 0 且确实产出了文件。
///
/// 个别情况下 curl 报错却返回 0（如 `file://` 源不存在），所以要再确认文件存在。
///
/// # 参数
/// - `exit_ok`：curl 是否以 0 退出。
/// - `part_exists`：输出文件是否存在。
pub fn curl_succeeded(exit_ok: bool, part_exists: bool) -> bool {
    exit_ok && part_exists
}

/// 阻塞地用 curl 下载到 `part`（支持续传），并确认产出了文件。
///
/// # 参数
/// - `url`：下载地址。
/// - `part`：`.part` 文件路径。
///
/// # 返回
/// 成功 `Ok`；启动失败或下载失败返回对应错误。
pub fn download_blocking(url: &str, part: &Path) -> Result<(), NetError> {
    let output = curl_command(url, part)
        .stderr(Stdio::piped())
        .output()
        .map_err(|e| NetError::RunTool {
            tool: TOOL_CURL,
            detail: e.to_string(),
        })?;
    if curl_succeeded(output.status.success(), part.is_file()) {
        Ok(())
    } else {
        Err(NetError::DownloadFailed {
            url: url.to_string(),
            detail: String::from_utf8_lossy(&output.stderr).trim().to_string(),
        })
    }
}

/// 下载到 `part`（支持续传），期间轮询：回调已下载字节数并检查取消；取消时结束 curl 并保留 `.part`。
///
/// # 参数
/// - `url`：下载地址。
/// - `part`：`.part` 文件路径。
/// - `cancel`：取消标志。
/// - `on_bytes`：进度回调（已下载字节数）。
pub fn download_polling(
    url: &str,
    part: &Path,
    cancel: &AtomicBool,
    mut on_bytes: impl FnMut(u64),
) -> Result<(), NetError> {
    let mut child = curl_command(url, part)
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| NetError::RunTool {
            tool: TOOL_CURL,
            detail: e.to_string(),
        })?;
    loop {
        if cancel.load(Ordering::Relaxed) {
            let _ = child.kill();
            let _ = child.wait();
            return Err(NetError::Cancelled);
        }
        match child.try_wait().map_err(|e| NetError::WaitTool {
            tool: TOOL_CURL,
            detail: e.to_string(),
        })? {
            Some(status) => {
                let mut stderr = String::new();
                if let Some(mut pipe) = child.stderr.take() {
                    let _ = pipe.read_to_string(&mut stderr);
                }
                if curl_succeeded(status.success(), part.is_file()) {
                    return Ok(());
                }
                return Err(NetError::DownloadFailed {
                    url: url.to_string(),
                    detail: stderr.trim().to_string(),
                });
            }
            None => {
                on_bytes(std::fs::metadata(part).map_or(0, |m| m.len()));
                std::thread::sleep(POLL_INTERVAL);
            }
        }
    }
}

/// 续传下载到 `part`、校验、原子改名到 `dest`。
///
/// 校验失败会删除 `.part`（避免下次续传坏数据）；下载失败或取消保留 `.part`。
///
/// # 参数
/// - `url`：下载地址。
/// - `part`：`.part` 文件路径。
/// - `dest`：最终路径。
/// - `cancel`：`Some` 时轮询进度并支持取消（回调 `on_bytes`）；`None` 时阻塞下载。
/// - `on_bytes`：进度回调（仅轮询模式使用）。
/// - `verify`：对 `.part` 的校验，失败返回调用方自己的错误。
///
/// # 返回
/// 成功 `Ok`；下载 / 改名错误经 `From<NetError>` 转成 `E`。
///
/// ```ignore
/// fetch_resumable(url, &part, &dest, None, |_| {}, |p| check(p))?;
/// ```
pub fn fetch_resumable<E: From<NetError>>(
    url: &str,
    part: &Path,
    dest: &Path,
    cancel: Option<&AtomicBool>,
    on_bytes: impl FnMut(u64),
    verify: impl FnOnce(&Path) -> Result<(), E>,
) -> Result<(), E> {
    match cancel {
        Some(flag) => download_polling(url, part, flag, on_bytes)?,
        None => download_blocking(url, part)?,
    }
    if let Err(e) = verify(part) {
        let _ = std::fs::remove_file(part);
        return Err(e);
    }
    std::fs::rename(part, dest).map_err(|e| NetError::Rename(e.to_string()))?;
    Ok(())
}

/// 从 `certutil -hashfile` 的输出里取出 SHA-256（去掉空格）。
///
/// # 参数
/// - `output`：certutil 标准输出。
///
/// # 返回
/// 64 位小写十六进制；输出不符合预期返回 `None`。
///
/// ```ignore
/// let out = "SHA256 的 a 哈希:\r\nab cd ...\r\nCertUtil: -hashfile 命令成功完成。";
/// ```
pub fn parse_certutil_sha256(output: &str) -> Option<String> {
    output
        .lines()
        .map(|line| {
            line.chars()
                .filter(|c| !c.is_whitespace())
                .collect::<String>()
        })
        .find(|line| line.len() == 64 && line.chars().all(|c| c.is_ascii_hexdigit()))
        .map(|line| line.to_ascii_lowercase())
}

/// 计算文件的 SHA-256（调用系统 `certutil`）。
///
/// # 参数
/// - `path`：文件路径。
///
/// # 返回
/// 小写十六进制；`certutil` 不可用或输出异常返回错误。
pub fn sha256_file(path: &Path) -> Result<String, NetError> {
    let output = quiet_command(&system_tool("certutil.exe"))
        .arg("-hashfile")
        .arg(path)
        .arg("SHA256")
        .output()
        .map_err(|e| NetError::RunTool {
            tool: TOOL_CERTUTIL,
            detail: e.to_string(),
        })?;
    parse_certutil_sha256(&String::from_utf8_lossy(&output.stdout)).ok_or_else(|| {
        NetError::HashUnreadable {
            path: path.display().to_string(),
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// curl 兜底判断：退出 0 但没产出文件算失败。
    #[test]
    fn curl_succeeded_needs_file() {
        assert!(curl_succeeded(true, true));
        assert!(!curl_succeeded(true, false));
        assert!(!curl_succeeded(false, true));
    }

    /// 代理参数出现在 `-o` 之前，清空后消失；同一测试内串行改全局状态。
    #[test]
    fn curl_command_args_and_proxy() {
        let args_of = || {
            curl_command("https://a.b/x", Path::new("x.part"))
                .get_args()
                .map(|a| a.to_string_lossy().into_owned())
                .collect::<Vec<_>>()
        };
        let base = args_of();
        assert!(base.contains(&"--ssl-no-revoke".to_string()));
        assert_eq!(base.last().unwrap(), "https://a.b/x");
        set_curl_proxy_args(vec!["--proxy".into(), "http://127.0.0.1:7890".into()]);
        let with = args_of();
        let at = with.iter().position(|a| a == "--proxy").unwrap();
        assert_eq!(with[at + 1], "http://127.0.0.1:7890");
        assert!(at < with.iter().position(|a| a == "-o").unwrap());
        set_curl_proxy_args(Vec::new());
        assert!(!args_of().iter().any(|a| a == "--proxy"));
    }

    /// certutil 输出解析：带空格、大写、无哈希。
    #[test]
    fn parse_certutil_variants() {
        let hash = "AB".repeat(32);
        let spaced = hash
            .as_bytes()
            .chunks(2)
            .map(|c| std::str::from_utf8(c).unwrap())
            .collect::<Vec<_>>()
            .join(" ");
        let out = format!("SHA256 的 a 哈希:\r\n{spaced}\r\nCertUtil: -hashfile 命令成功完成。");
        assert_eq!(parse_certutil_sha256(&out), Some(hash.to_lowercase()));
        assert_eq!(parse_certutil_sha256(&hash), Some(hash.to_lowercase()));
        assert_eq!(parse_certutil_sha256("CertUtil: 失败"), None);
        assert_eq!(parse_certutil_sha256(""), None);
    }

    /// 校验失败时删除 `.part` 且不改名（file:// 源，不联网）。
    #[cfg(windows)]
    #[test]
    fn fetch_resumable_removes_part_on_verify_failure() {
        struct W(String);
        impl From<NetError> for W {
            fn from(e: NetError) -> Self {
                W(format!("{e:?}"))
            }
        }
        let dir = std::env::temp_dir().join(format!("snow-net-fr-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let src = dir.join("src.bin");
        std::fs::write(&src, b"hello").unwrap();
        let url = format!("file:///{}", src.display().to_string().replace('\\', "/"));
        let (part, dest) = (dir.join("d.part"), dir.join("d.bin"));
        let r = fetch_resumable::<W>(&url, &part, &dest, None, |_| {}, |_| Err(W("bad".into())));
        assert_eq!(r.err().map(|w| w.0), Some("bad".to_string()));
        assert!(!part.exists() && !dest.exists());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
