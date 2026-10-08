//! OCR 资产按需下载：下载 → 大小与 SHA-256 校验 → 原子改名 → 写完成标记。
//!
//! 不新增第三方依赖：下载用 Windows 自带的 `curl.exe`（`--ssl-no-revoke`，支持断点续传与重试），
//! 哈希用 `certutil -hashfile`，运行时压缩包用自带的 `tar.exe` 解压。哈希以清单为准，
//! 因此换成任意镜像地址也不会降低校验强度。下载在调用线程里阻塞执行，调用方应放到后台线程。

use crate::ocr_assets::{
    AssetFile, COMPLETE_MARKER, Manifest, OcrUnavailable, dir_complete, find_model, manifest,
    model_dir, ocr_root, runtime_dir,
};
use snow_i18n::{Args, I18n};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};

/// Windows 不弹控制台窗口的进程创建标志。
#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;
/// 下载中的临时文件后缀。
const PART_SUFFIX: &str = ".part";
/// curl 连接超时（秒）。
const CURL_CONNECT_TIMEOUT: &str = "20";
/// curl 失败重试次数。
const CURL_RETRIES: &str = "3";
/// 追加给 curl 的代理参数（由 `network/proxy` 配置算出，空表示不走代理）。
static CURL_PROXY_ARGS: std::sync::Mutex<Vec<String>> = std::sync::Mutex::new(Vec::new());
/// OCR 清单的组件名（错误文案里的技术名词）。
pub(crate) const OCR_MANIFEST_NAME: &str = "OCR";
/// 调用 `curl` 的工具名。
pub(crate) const TOOL_CURL: &str = "curl";
/// 调用 `tar` 的工具名。
pub(crate) const TOOL_TAR: &str = "tar";
/// 调用 `certutil` 的工具名。
pub(crate) const TOOL_CERTUTIL: &str = "certutil";
/// 完成标记的内容。
const MARKER_CONTENT: &str = "{\"schema\":1}";

/// 下载 / 解压 / 安装过程中的结构化错误；界面边界再按语言翻译（见 [`FetchError::message`]）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FetchError {
    /// 无法启动外部工具（`certutil` / `curl` / `tar`）。
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
    /// 读取文件信息失败。
    Stat {
        /// 文件名。
        name: String,
        /// 系统给出的原因。
        detail: String,
    },
    /// 文件大小与清单不符。
    SizeMismatch {
        /// 文件名。
        name: String,
        /// 清单里的大小。
        expected: u64,
        /// 实际大小。
        actual: u64,
    },
    /// 文件哈希与清单不符。
    HashMismatch {
        /// 文件名。
        name: String,
        /// 清单里的哈希。
        expected: String,
        /// 实际哈希。
        actual: String,
    },
    /// 下载失败。
    DownloadFailed {
        /// 下载地址。
        url: String,
        /// curl 报告的原因。
        detail: String,
    },
    /// 用户取消。
    Cancelled,
    /// 创建目录失败。
    CreateDir(String),
    /// 改名失败。
    Rename(String),
    /// 解压失败。
    Extract(String),
    /// 写完成标记失败。
    WriteMarker(String),
    /// 目标路径没有父目录。
    InvalidTarget,
    /// 序列化模型元数据失败。
    SerializeMeta(String),
    /// 写模型元数据文件失败。
    WriteMeta(String),
    /// 创建暂存目录失败。
    CreateStaging(String),
    /// 压缩包缺少必需文件（已用逗号拼好）。
    MissingFiles(String),
    /// 把暂存目录换入正式位置失败。
    SwapDir(String),
    /// 安装压缩包里的某个成员失败。
    InstallMember {
        /// 成员名。
        name: String,
        /// 系统给出的原因。
        detail: String,
    },
    /// 内置清单损坏。
    Manifest {
        /// 清单所属组件名（技术名词，不翻译）。
        what: &'static str,
        /// 解析器给出的原因。
        detail: String,
    },
    /// OCR 资产本身不可用（如模型类型未知）。
    Ocr(OcrUnavailable),
    /// 后台任务没能启动。
    TaskStart(String),
    /// 技术信息（如未知的模型 ID），原样显示、不翻译。
    Technical(String),
}

impl FetchError {
    /// 面向用户的提示文案。
    ///
    /// # 参数
    /// - `i18n`：界面语料。
    ///
    /// ```ignore
    /// let text = FetchError::Cancelled.message(i18n);
    /// ```
    pub fn message(&self, i18n: &I18n) -> String {
        let plain = |id: &str, detail: &str| i18n.tr_with(id, &Args::new().named("detail", detail));
        let tool_args =
            |tool: &str, detail: &str| Args::new().named("tool", tool).named("detail", detail);
        match self {
            Self::RunTool { tool, detail } => {
                i18n.tr_with("fetch-run-tool", &tool_args(tool, detail))
            }
            Self::WaitTool { tool, detail } => {
                i18n.tr_with("fetch-wait-tool", &tool_args(tool, detail))
            }
            Self::HashUnreadable { path } => i18n.tr_with(
                "fetch-hash-unreadable",
                &Args::new().named("path", path.as_str()),
            ),
            Self::Stat { name, detail } => i18n.tr_with(
                "fetch-stat-failed",
                &Args::new()
                    .named("name", name.as_str())
                    .named("detail", detail.as_str()),
            ),
            Self::SizeMismatch {
                name,
                expected,
                actual,
            } => i18n.tr_with(
                "fetch-size-mismatch",
                &Args::new()
                    .named("name", name.as_str())
                    .named("expected", expected.to_string())
                    .named("actual", actual.to_string()),
            ),
            Self::HashMismatch {
                name,
                expected,
                actual,
            } => i18n.tr_with(
                "fetch-hash-mismatch",
                &Args::new()
                    .named("name", name.as_str())
                    .named("expected", expected.as_str())
                    .named("actual", actual.as_str()),
            ),
            Self::DownloadFailed { url, detail } => i18n.tr_with(
                "fetch-download-failed",
                &Args::new()
                    .named("url", url.as_str())
                    .named("detail", detail.as_str()),
            ),
            Self::Cancelled => i18n.tr("fetch-cancelled"),
            Self::CreateDir(detail) => plain("fetch-create-dir-failed", detail),
            Self::Rename(detail) => plain("fetch-rename-failed", detail),
            Self::Extract(detail) => plain("fetch-extract-failed", detail),
            Self::WriteMarker(detail) => plain("fetch-marker-failed", detail),
            Self::InvalidTarget => i18n.tr("fetch-invalid-target"),
            Self::SerializeMeta(detail) => plain("fetch-meta-serialize-failed", detail),
            Self::WriteMeta(detail) => plain("fetch-meta-write-failed", detail),
            Self::CreateStaging(detail) => plain("fetch-staging-failed", detail),
            Self::MissingFiles(files) => i18n.tr_with(
                "fetch-missing-files",
                &Args::new().named("files", files.as_str()),
            ),
            Self::SwapDir(detail) => plain("fetch-swap-failed", detail),
            Self::InstallMember { name, detail } => i18n.tr_with(
                "fetch-install-member-failed",
                &Args::new()
                    .named("name", name.as_str())
                    .named("detail", detail.as_str()),
            ),
            Self::Manifest { what, detail } => i18n.tr_with(
                "fetch-manifest-corrupt",
                &Args::new()
                    .named("what", *what)
                    .named("detail", detail.as_str()),
            ),
            Self::Ocr(unavailable) => unavailable.message(i18n),
            Self::TaskStart(detail) => plain("fetch-task-start-failed", detail),
            Self::Technical(text) => text.clone(),
        }
    }
}

/// 下载计划执行中报告给界面的步骤（界面边界再翻译）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DownloadStep {
    /// 正在下载 OCR 运行时。
    OcrRuntimeDownload,
    /// 正在解压 OCR 运行时。
    OcrRuntimeExtract,
    /// 正在下载第 `index` 个 OCR 模型文件（从 1 起，共 `total` 个）。
    OcrModel {
        /// 当前序号（从 1 起）。
        index: usize,
        /// 总数。
        total: usize,
    },
    /// 正在下载 onnxruntime 运行时。
    OrtDownload,
    /// 正在解压 onnxruntime 运行时。
    OrtExtract,
}

impl DownloadStep {
    /// 面向用户的步骤说明。
    ///
    /// # 参数
    /// - `i18n`：界面语料。
    pub fn message(&self, i18n: &I18n) -> String {
        match self {
            Self::OcrRuntimeDownload => i18n.tr("fetch-step-ocr-runtime-download"),
            Self::OcrRuntimeExtract => i18n.tr("fetch-step-ocr-runtime-extract"),
            Self::OcrModel { index, total } => i18n.tr_with(
                "fetch-step-ocr-model",
                &Args::new()
                    .named("index", index.to_string())
                    .named("total", total.to_string()),
            ),
            Self::OrtDownload => i18n.tr("fetch-step-ort-download"),
            Self::OrtExtract => i18n.tr("fetch-step-ort-extract"),
        }
    }
}

/// 一项待下载的文件。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DownloadItem {
    /// 清单里的文件信息（名称、大小、哈希、地址）。
    pub file: AssetFile,
    /// 落地目录。
    pub dest_dir: PathBuf,
}

/// 运行时压缩包的下载计划。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeDownload {
    /// 压缩包。
    pub archive: DownloadItem,
    /// 解压目标目录。
    pub extract_to: PathBuf,
    /// 解压后应有的文件。
    pub files: Vec<AssetFile>,
}

/// 一次下载的完整计划（缺哪些补哪些）。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DownloadPlan {
    /// 需要下载的运行时（已就绪则为空）。
    pub runtime: Option<RuntimeDownload>,
    /// 需要下载的模型目录与文件（已就绪则为空）。
    pub model: Option<(PathBuf, Vec<DownloadItem>)>,
}

impl DownloadPlan {
    /// 计划里是否什么都不用下。
    pub fn is_empty(&self) -> bool {
        self.runtime.is_none() && self.model.is_none()
    }
}

/// 规划要下载的内容：运行时与所选模型各自缺失才下载。
///
/// # 参数
/// - `root`：资产根目录。
/// - `model_kind`：配置里的模型类型。
/// - `need_runtime`：为 `false` 时不规划运行时（已通过环境变量指定 exe）。
///
/// # 返回
/// 下载计划；清单或模型类型无效返回错误说明。
pub fn plan_downloads(
    root: &Path,
    model_kind: &str,
    need_runtime: bool,
) -> Result<DownloadPlan, FetchError> {
    let manifest = manifest().map_err(|detail| FetchError::Manifest {
        what: OCR_MANIFEST_NAME,
        detail,
    })?;
    plan_from_manifest(manifest, root, model_kind, need_runtime)
}

/// 用给定清单规划下载（测试可注入自定义清单）。
pub fn plan_from_manifest(
    manifest: &Manifest,
    root: &Path,
    model_kind: &str,
    need_runtime: bool,
) -> Result<DownloadPlan, FetchError> {
    let model = find_model(manifest, model_kind).map_err(FetchError::Ocr)?;
    let mut plan = DownloadPlan::default();
    let runtime = &manifest.runtime;
    let rt_dir = runtime_dir(root, runtime);
    if need_runtime && !dir_complete(&rt_dir, &runtime.files) {
        plan.runtime = Some(RuntimeDownload {
            archive: DownloadItem {
                file: runtime.archive.clone(),
                dest_dir: rt_dir.clone(),
            },
            extract_to: rt_dir,
            files: runtime.files.clone(),
        });
    }
    let m_dir = model_dir(root, model);
    if !dir_complete(&m_dir, &model.files) {
        let items = model
            .files
            .iter()
            .map(|f| DownloadItem {
                file: f.clone(),
                dest_dir: m_dir.clone(),
            })
            .collect();
        plan.model = Some((m_dir, items));
    }
    Ok(plan)
}

/// 从 `certutil -hashfile` 的输出里取出 SHA-256（第二行，去掉空格）。
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

/// 系统工具路径：优先 `%SystemRoot%\System32\<name>`，否则直接用名字走 PATH。
pub(crate) fn system_tool(name: &str) -> PathBuf {
    std::env::var_os("SystemRoot")
        .map(|root| PathBuf::from(root).join("System32").join(name))
        .filter(|p| p.is_file())
        .unwrap_or_else(|| PathBuf::from(name))
}

/// 构造不弹窗的子进程命令。
pub(crate) fn quiet_command(program: &Path) -> Command {
    let mut command = Command::new(program);
    command.stdin(Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(CREATE_NO_WINDOW);
    }
    command
}

/// 计算文件的 SHA-256。
///
/// # 参数
/// - `path`：文件路径。
///
/// # 返回
/// 小写十六进制；`certutil` 不可用或输出异常返回错误说明。
pub fn sha256_file(path: &Path) -> Result<String, FetchError> {
    let output = quiet_command(&system_tool("certutil.exe"))
        .arg("-hashfile")
        .arg(path)
        .arg("SHA256")
        .output()
        .map_err(|e| FetchError::RunTool {
            tool: TOOL_CERTUTIL,
            detail: e.to_string(),
        })?;
    parse_certutil_sha256(&String::from_utf8_lossy(&output.stdout)).ok_or_else(|| {
        FetchError::HashUnreadable {
            path: path.display().to_string(),
        }
    })
}

/// 校验文件大小与哈希是否与清单一致。
///
/// # 参数
/// - `path`：文件路径。
/// - `file`：清单条目。
///
/// # 返回
/// 一致返回 `Ok`，否则说明哪里不符。
pub fn verify_file(path: &Path, file: &AssetFile) -> Result<(), FetchError> {
    let size = std::fs::metadata(path)
        .map_err(|e| FetchError::Stat {
            name: file.name.clone(),
            detail: e.to_string(),
        })?
        .len();
    if size != file.size {
        return Err(FetchError::SizeMismatch {
            name: file.name.clone(),
            expected: file.size,
            actual: size,
        });
    }
    let actual = sha256_file(path)?;
    if actual != file.sha256.to_ascii_lowercase() {
        return Err(FetchError::HashMismatch {
            name: file.name.clone(),
            expected: file.sha256.clone(),
            actual,
        });
    }
    Ok(())
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

/// 构造 curl 下载命令（写 `part`，支持续传、重试，带 `--ssl-no-revoke`）；OCR 与语音模型下载共用。
pub(crate) fn curl_command(url: &str, part: &Path) -> Command {
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

/// 用 curl 下载到 `dest`（先写 `.part`，支持续传，成功后由调用方校验并改名）。
fn curl_download(url: &str, part: &Path) -> Result<(), FetchError> {
    let status = curl_command(url, part)
        .stderr(Stdio::piped())
        .output()
        .map_err(|e| FetchError::RunTool {
            tool: TOOL_CURL,
            detail: e.to_string(),
        })?;
    if status.status.success() {
        Ok(())
    } else {
        Err(FetchError::DownloadFailed {
            url: url.to_string(),
            detail: String::from_utf8_lossy(&status.stderr).trim().to_string(),
        })
    }
}

/// 下载并校验一个文件，成功后原子改名到 `dest_dir/<name>`。
///
/// # 参数
/// - `item`：下载项。
/// - `cancel`：置位后尽快放弃（在文件之间检查）。
///
/// # 返回
/// 成功返回最终路径；下载或校验失败返回错误（残留的 `.part` 会被删除，避免下次续传坏数据）。
pub fn fetch_verified(item: &DownloadItem, cancel: &AtomicBool) -> Result<PathBuf, FetchError> {
    if cancel.load(Ordering::Relaxed) {
        return Err(FetchError::Cancelled);
    }
    std::fs::create_dir_all(&item.dest_dir).map_err(|e| FetchError::CreateDir(e.to_string()))?;
    let dest = item.dest_dir.join(&item.file.name);
    let part = item
        .dest_dir
        .join(format!("{}{PART_SUFFIX}", item.file.name));
    curl_download(&item.file.url, &part)?;
    if let Err(e) = verify_file(&part, &item.file) {
        let _ = std::fs::remove_file(&part);
        return Err(e);
    }
    std::fs::rename(&part, &dest).map_err(|e| FetchError::Rename(e.to_string()))?;
    Ok(dest)
}

/// 用系统自带的 `tar.exe` 解压 zip。
fn extract_zip(archive: &Path, dest: &Path) -> Result<(), FetchError> {
    let output = quiet_command(&system_tool("tar.exe"))
        .arg("-xf")
        .arg(archive)
        .arg("-C")
        .arg(dest)
        .output()
        .map_err(|e| FetchError::RunTool {
            tool: TOOL_TAR,
            detail: e.to_string(),
        })?;
    if output.status.success() {
        Ok(())
    } else {
        Err(FetchError::Extract(
            String::from_utf8_lossy(&output.stderr).trim().to_string(),
        ))
    }
}

/// 只解压压缩包里指定的成员（路径与包内一致），用于从大包里取少数文件。
///
/// # 参数
/// - `archive`：压缩包（zip / whl）。
/// - `dest`：解压目标目录（成员按包内相对路径落在其下）。
/// - `members`：要解压的包内路径。
pub fn extract_members(archive: &Path, dest: &Path, members: &[&str]) -> Result<(), FetchError> {
    let output = quiet_command(&system_tool("tar.exe"))
        .arg("-xf")
        .arg(archive)
        .arg("-C")
        .arg(dest)
        .args(members)
        .output()
        .map_err(|e| FetchError::RunTool {
            tool: TOOL_TAR,
            detail: e.to_string(),
        })?;
    if output.status.success() {
        Ok(())
    } else {
        Err(FetchError::Extract(
            String::from_utf8_lossy(&output.stderr).trim().to_string(),
        ))
    }
}

/// 写完成标记。
pub(crate) fn write_marker(dir: &Path) -> Result<(), FetchError> {
    std::fs::write(dir.join(COMPLETE_MARKER), MARKER_CONTENT)
        .map_err(|e| FetchError::WriteMarker(e.to_string()))
}

/// 执行下载计划：运行时（下载 → 解压 → 逐文件校验）与模型（逐文件下载校验），各自完成后写标记。
///
/// # 参数
/// - `plan`：下载计划。
/// - `cancel`：取消开关。
/// - `progress`：进度回调，参数是当前步骤（界面边界再翻译）。
///
/// # 返回
/// 全部成功返回 `Ok`；任一步失败立即返回错误说明。
pub fn execute_plan(
    plan: &DownloadPlan,
    cancel: &AtomicBool,
    mut progress: impl FnMut(DownloadStep),
) -> Result<(), FetchError> {
    if let Some(runtime) = &plan.runtime {
        progress(DownloadStep::OcrRuntimeDownload);
        let archive = fetch_verified(&runtime.archive, cancel)?;
        progress(DownloadStep::OcrRuntimeExtract);
        extract_zip(&archive, &runtime.extract_to)?;
        for file in &runtime.files {
            verify_file(&runtime.extract_to.join(&file.name), file)?;
        }
        write_marker(&runtime.extract_to)?;
        let _ = std::fs::remove_file(&archive);
    }
    if let Some((dir, items)) = &plan.model {
        for (index, item) in items.iter().enumerate() {
            progress(DownloadStep::OcrModel {
                index: index + 1,
                total: items.len(),
            });
            fetch_verified(item, cancel)?;
        }
        write_marker(dir)?;
    }
    Ok(())
}

/// 下载配置的模型（与运行时，如缺失）：规划 + 执行。
///
/// # 参数
/// - `data_root`：应用数据根目录。
/// - `env_root`：`SNOW_OCR_ASSET_DIR` 的值。
/// - `model_kind`：模型类型。
/// - `need_runtime`：是否需要下载运行时。
/// - `cancel` / `progress`：同 [`execute_plan`]。
pub fn download_missing(
    data_root: &Path,
    env_root: Option<&str>,
    model_kind: &str,
    need_runtime: bool,
    cancel: &AtomicBool,
    progress: impl FnMut(DownloadStep),
) -> Result<(), FetchError> {
    let root = ocr_root(data_root, env_root);
    let plan = plan_downloads(&root, model_kind, need_runtime)?;
    execute_plan(&plan, cancel, progress)
}

#[cfg(test)]
mod tests {
    /// 代理参数追加到 curl 命令里：设置后出现在 `-o` 之前，清空后消失。
    #[test]
    fn curl_command_appends_proxy_args() {
        let args_of = || {
            super::curl_command("https://a.b/x", std::path::Path::new("x.part"))
                .get_args()
                .map(|a| a.to_string_lossy().into_owned())
                .collect::<Vec<_>>()
        };
        super::set_curl_proxy_args(vec!["--proxy".into(), "http://127.0.0.1:7890".into()]);
        let with = args_of();
        let at = with
            .iter()
            .position(|a| a == "--proxy")
            .expect("应带 --proxy");
        assert_eq!(with[at + 1], "http://127.0.0.1:7890");
        assert!(at < with.iter().position(|a| a == "-o").unwrap());
        super::set_curl_proxy_args(Vec::new());
        assert!(!args_of().iter().any(|a| a == "--proxy"));
    }

    use super::*;
    use crate::ocr_assets::{ModelSpec, RuntimeSpec};

    /// 唯一临时目录。
    fn temp_root(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("snow-ocr-dl-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("建目录");
        dir
    }

    /// 写一个源文件并生成对应的清单条目（url 用 file:// 本地地址，离线可测）。
    fn source_file(dir: &Path, name: &str, content: &[u8]) -> AssetFile {
        let path = dir.join(name);
        std::fs::write(&path, content).expect("写源文件");
        AssetFile {
            name: name.to_string(),
            size: content.len() as u64,
            sha256: sha256_file(&path).expect("哈希"),
            url: format!("file:///{}", path.to_string_lossy().replace('\\', "/")),
        }
    }

    /// certutil 输出解析：取出 64 位十六进制、去掉空格、转小写；无哈希行返回 None。
    #[test]
    fn parses_certutil_output() {
        let hash = "AB".repeat(32);
        let spaced: Vec<String> = hash
            .as_bytes()
            .chunks(2)
            .map(|c| String::from_utf8_lossy(c).into_owned())
            .collect();
        let out = format!(
            "SHA256 的 x 哈希:\r\n{}\r\nCertUtil: -hashfile 命令成功完成。\r\n",
            spaced.join(" ")
        );
        assert_eq!(parse_certutil_sha256(&out), Some(hash.to_lowercase()));
        let compact = format!("SHA256 hash of x:\n{hash}\nCertUtil: done\n");
        assert_eq!(parse_certutil_sha256(&compact), Some(hash.to_lowercase()));
        assert_eq!(parse_certutil_sha256("CertUtil: -hashfile 失败"), None);
        assert_eq!(parse_certutil_sha256(""), None);
    }

    /// 已知内容的哈希与标准值一致（"abc" 的 SHA-256）。
    #[test]
    fn sha256_of_known_content() {
        let dir = temp_root("sha");
        let path = dir.join("abc.txt");
        std::fs::write(&path, b"abc").expect("写文件");
        assert_eq!(
            sha256_file(&path).expect("哈希"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 大小不符、哈希不符都被拒绝；一致通过。
    #[test]
    fn verify_rejects_tampering() {
        let dir = temp_root("verify");
        let file = source_file(&dir, "a.bin", b"hello world");
        let path = dir.join("a.bin");
        assert!(verify_file(&path, &file).is_ok());
        let mut bad_size = file.clone();
        bad_size.size += 1;
        assert!(matches!(
            verify_file(&path, &bad_size).unwrap_err(),
            FetchError::SizeMismatch { .. }
        ));
        let mut bad_hash = file.clone();
        bad_hash.sha256 = "0".repeat(64);
        assert!(matches!(
            verify_file(&path, &bad_hash).unwrap_err(),
            FetchError::HashMismatch { .. }
        ));
        assert!(verify_file(&dir.join("none"), &file).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 计划：什么都没有时运行时与模型都要下；`need_runtime=false` 不含运行时；已就绪则为空。
    #[test]
    fn plan_reflects_missing_pieces() {
        let root = temp_root("plan");
        let plan = plan_downloads(&root, "small", true).expect("规划");
        assert!(plan.runtime.is_some() && plan.model.is_some());
        assert_eq!(plan.model.as_ref().map(|(_, items)| items.len()), Some(3));
        let plan = plan_downloads(&root, "small", false).expect("规划");
        assert!(plan.runtime.is_none() && plan.model.is_some());
        assert!(plan_downloads(&root, "nope", true).is_err());
        let _ = std::fs::remove_dir_all(&root);
    }

    /// 端到端（file:// 源）：下载、校验、改名、写标记；再次规划为空；篡改源文件时失败且不留 `.part`。
    #[test]
    fn download_verify_and_mark_with_local_source() {
        let src = temp_root("src");
        let root = temp_root("dst");
        let det = source_file(&src, "det.onnx", b"detector-bytes");
        let rec = source_file(&src, "rec.onnx", b"recognizer-bytes");
        let dict = source_file(&src, "dict.txt", b"a\nb\n");
        let exe = source_file(&src, "worker.exe", b"MZ-fake");
        // 把 exe 打成 zip 需要外部工具；这里只验证模型链路，运行时另用清单结构验证
        let model = ModelSpec {
            kind: "tiny".into(),
            id: "tiny-1".into(),
            detector: det.name.clone(),
            recognizer: rec.name.clone(),
            dictionary: dict.name.clone(),
            files: vec![det, rec, dict],
        };
        let manifest = Manifest {
            default_model: "tiny".into(),
            runtime: RuntimeSpec {
                version: "0".into(),
                platform: "test".into(),
                archive: exe.clone(),
                files: vec![exe],
            },
            models: vec![model],
        };
        let plan = plan_from_manifest(&manifest, &root, "tiny", false).expect("规划");
        let mut steps = Vec::new();
        let cancel = AtomicBool::new(false);
        execute_plan(&plan, &cancel, |s| steps.push(s)).expect("下载");
        assert_eq!(steps.len(), 3);
        assert!(
            plan_from_manifest(&manifest, &root, "tiny", false)
                .expect("再规划")
                .is_empty()
        );
        let m_dir = model_dir(&root, &manifest.models[0]);
        assert!(m_dir.join(COMPLETE_MARKER).is_file());
        assert!(!m_dir.join("det.onnx.part").exists());

        // 篡改源文件：内容变了但清单哈希没变 -> 校验失败，且不留 .part
        let root2 = temp_root("dst2");
        std::fs::write(src.join("det.onnx"), b"detector-BYTES").expect("篡改");
        let plan = plan_from_manifest(&manifest, &root2, "tiny", false).expect("规划");
        let err = execute_plan(&plan, &cancel, |_| {}).unwrap_err();
        assert!(matches!(err, FetchError::HashMismatch { .. }), "{err:?}");
        let m_dir2 = model_dir(&root2, &manifest.models[0]);
        assert!(!m_dir2.join("det.onnx.part").exists());
        assert!(!m_dir2.join(COMPLETE_MARKER).exists());
        // 取消开关
        cancel.store(true, Ordering::Relaxed);
        let plan = plan_from_manifest(&manifest, &root2, "tiny", false).expect("规划");
        assert_eq!(
            execute_plan(&plan, &cancel, |_| {}).unwrap_err(),
            FetchError::Cancelled
        );
        for dir in [src, root, root2] {
            let _ = std::fs::remove_dir_all(&dir);
        }
    }

    /// 结构化错误与步骤在两种界面语言下都有完整文案：英文无中文、没有缺失标记，参数被代入。
    #[test]
    fn fetch_errors_and_steps_are_localized() {
        let zh = crate::ocr_backend::i18n_for("zh-CN");
        let en = crate::ocr_backend::i18n_for("en-US");
        let errors = [
            FetchError::RunTool {
                tool: TOOL_CURL,
                detail: "d".into(),
            },
            FetchError::WaitTool {
                tool: TOOL_CURL,
                detail: "d".into(),
            },
            FetchError::HashUnreadable { path: "p".into() },
            FetchError::Stat {
                name: "n".into(),
                detail: "d".into(),
            },
            FetchError::SizeMismatch {
                name: "n".into(),
                expected: 1,
                actual: 2,
            },
            FetchError::HashMismatch {
                name: "n".into(),
                expected: "a".into(),
                actual: "b".into(),
            },
            FetchError::DownloadFailed {
                url: "u".into(),
                detail: "d".into(),
            },
            FetchError::Cancelled,
            FetchError::CreateDir("d".into()),
            FetchError::Rename("d".into()),
            FetchError::Extract("d".into()),
            FetchError::WriteMarker("d".into()),
            FetchError::InvalidTarget,
            FetchError::SerializeMeta("d".into()),
            FetchError::WriteMeta("d".into()),
            FetchError::CreateStaging("d".into()),
            FetchError::MissingFiles("f".into()),
            FetchError::SwapDir("d".into()),
            FetchError::InstallMember {
                name: "n".into(),
                detail: "d".into(),
            },
            FetchError::Manifest {
                what: OCR_MANIFEST_NAME,
                detail: "d".into(),
            },
            FetchError::Ocr(OcrUnavailable::NoRuntime),
            FetchError::TaskStart("d".into()),
        ];
        for error in &errors {
            let (z, e) = (error.message(zh), error.message(en));
            assert!(
                !z.contains("[!") && !e.contains("[!"),
                "{error:?}: {z} / {e}"
            );
            assert!(e.is_ascii(), "{error:?}: {e}");
            assert_ne!(z, e, "{error:?}");
        }
        assert_eq!(FetchError::Cancelled.message(zh), "已取消。");
        assert!(
            FetchError::SizeMismatch {
                name: "a.bin".into(),
                expected: 1,
                actual: 2
            }
            .message(en)
            .contains("a.bin")
        );
        let steps = [
            DownloadStep::OcrRuntimeDownload,
            DownloadStep::OcrRuntimeExtract,
            DownloadStep::OcrModel { index: 2, total: 3 },
            DownloadStep::OrtDownload,
            DownloadStep::OrtExtract,
        ];
        for step in &steps {
            assert!(
                step.message(en).is_ascii() && !step.message(en).contains("[!"),
                "{step:?}"
            );
            assert_ne!(step.message(zh), step.message(en));
        }
        assert_eq!(
            DownloadStep::OcrModel { index: 2, total: 3 }.message(zh),
            "正在下载 OCR 模型 (2/3)…"
        );
    }

    /// 地址不可达时报下载失败而不是 panic。
    #[test]
    fn unreachable_source_is_an_error() {
        let root = temp_root("bad-url");
        let item = DownloadItem {
            file: AssetFile {
                name: "x.bin".into(),
                size: 1,
                sha256: "0".repeat(64),
                url: "file:///Z:/definitely/not/here.bin".into(),
            },
            dest_dir: root.clone(),
        };
        let err = fetch_verified(&item, &AtomicBool::new(false)).unwrap_err();
        assert!(matches!(err, FetchError::DownloadFailed { .. }), "{err:?}");
        let _ = std::fs::remove_dir_all(&root);
    }

    /// 真实网络：从 modelscope 下载清单里最小的字典文件并校验哈希（`--ignored`）。
    #[test]
    #[ignore = "需要联网"]
    fn real_network_download_of_small_file() {
        let m = manifest().expect("清单");
        let model = find_model(m, "extra_small").expect("模型");
        let file = model
            .files
            .iter()
            .find(|f| f.name.ends_with(".txt"))
            .expect("字典")
            .clone();
        let dir = temp_root("net");
        let item = DownloadItem {
            file,
            dest_dir: dir.clone(),
        };
        let started = std::time::Instant::now();
        let path = fetch_verified(&item, &AtomicBool::new(false)).expect("下载并校验");
        println!(
            "NET|{}|{} bytes|{} ms|sha256 verified",
            item.file.name,
            std::fs::metadata(&path).map_or(0, |m| m.len()),
            started.elapsed().as_millis()
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
