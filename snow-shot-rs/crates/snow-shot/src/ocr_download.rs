//! OCR 资产按需下载：下载 → 大小与 SHA-256 校验 → 原子改名 → 写完成标记。
//!
//! 不新增第三方依赖：下载用 Windows 自带的 `curl.exe`（`--ssl-no-revoke`，支持断点续传与重试），
//! 哈希用 `certutil -hashfile`，运行时压缩包用自带的 `tar.exe` 解压。哈希以清单为准，
//! 因此换成任意镜像地址也不会降低校验强度。下载在调用线程里阻塞执行，调用方应放到后台线程。

use crate::ocr_assets::{
    AssetFile, COMPLETE_MARKER, Manifest, find_model, manifest, model_dir, ocr_root, runtime_dir,
    dir_complete,
};
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
/// 完成标记的内容。
const MARKER_CONTENT: &str = "{\"schema\":1}";

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
pub fn plan_downloads(root: &Path, model_kind: &str, need_runtime: bool) -> Result<DownloadPlan, String> {
    let manifest = manifest()?;
    plan_from_manifest(manifest, root, model_kind, need_runtime)
}

/// 用给定清单规划下载（测试可注入自定义清单）。
pub fn plan_from_manifest(
    manifest: &Manifest,
    root: &Path,
    model_kind: &str,
    need_runtime: bool,
) -> Result<DownloadPlan, String> {
    let model = find_model(manifest, model_kind).map_err(|e| e.message())?;
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
        .map(|line| line.chars().filter(|c| !c.is_whitespace()).collect::<String>())
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
pub fn sha256_file(path: &Path) -> Result<String, String> {
    let output = quiet_command(&system_tool("certutil.exe"))
        .arg("-hashfile")
        .arg(path)
        .arg("SHA256")
        .output()
        .map_err(|e| format!("无法运行 certutil: {e}"))?;
    parse_certutil_sha256(&String::from_utf8_lossy(&output.stdout))
        .ok_or_else(|| format!("无法解析 {} 的哈希", path.display()))
}

/// 校验文件大小与哈希是否与清单一致。
///
/// # 参数
/// - `path`：文件路径。
/// - `file`：清单条目。
///
/// # 返回
/// 一致返回 `Ok`，否则说明哪里不符。
pub fn verify_file(path: &Path, file: &AssetFile) -> Result<(), String> {
    let size = std::fs::metadata(path).map_err(|e| format!("{}: {e}", file.name))?.len();
    if size != file.size {
        return Err(format!("{} 大小不符: 期望 {} 实际 {size}", file.name, file.size));
    }
    let actual = sha256_file(path)?;
    if actual != file.sha256.to_ascii_lowercase() {
        return Err(format!("{} 哈希不符: 期望 {} 实际 {actual}", file.name, file.sha256));
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
        .args(["--fail", "--location", "--silent", "--show-error", "--ssl-no-revoke"])
        .args(["--connect-timeout", CURL_CONNECT_TIMEOUT])
        .args(["--retry", CURL_RETRIES, "-C", "-"]);
    if let Ok(guard) = CURL_PROXY_ARGS.lock() {
        command.args(guard.iter());
    }
    command.arg("-o").arg(part).arg(url);
    command
}

/// 用 curl 下载到 `dest`（先写 `.part`，支持续传，成功后由调用方校验并改名）。
fn curl_download(url: &str, part: &Path) -> Result<(), String> {
    let status = curl_command(url, part)
        .stderr(Stdio::piped())
        .output()
        .map_err(|e| format!("无法运行 curl: {e}"))?;
    if status.status.success() {
        Ok(())
    } else {
        Err(format!(
            "下载失败 ({url}): {}",
            String::from_utf8_lossy(&status.stderr).trim()
        ))
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
pub fn fetch_verified(item: &DownloadItem, cancel: &AtomicBool) -> Result<PathBuf, String> {
    if cancel.load(Ordering::Relaxed) {
        return Err("已取消".to_string());
    }
    std::fs::create_dir_all(&item.dest_dir).map_err(|e| format!("创建目录失败: {e}"))?;
    let dest = item.dest_dir.join(&item.file.name);
    let part = item.dest_dir.join(format!("{}{PART_SUFFIX}", item.file.name));
    curl_download(&item.file.url, &part)?;
    if let Err(e) = verify_file(&part, &item.file) {
        let _ = std::fs::remove_file(&part);
        return Err(e);
    }
    std::fs::rename(&part, &dest).map_err(|e| format!("改名失败: {e}"))?;
    Ok(dest)
}

/// 用系统自带的 `tar.exe` 解压 zip。
fn extract_zip(archive: &Path, dest: &Path) -> Result<(), String> {
    let output = quiet_command(&system_tool("tar.exe"))
        .arg("-xf")
        .arg(archive)
        .arg("-C")
        .arg(dest)
        .output()
        .map_err(|e| format!("无法运行 tar: {e}"))?;
    if output.status.success() {
        Ok(())
    } else {
        Err(format!("解压失败: {}", String::from_utf8_lossy(&output.stderr).trim()))
    }
}

/// 只解压压缩包里指定的成员（路径与包内一致），用于从大包里取少数文件。
///
/// # 参数
/// - `archive`：压缩包（zip / whl）。
/// - `dest`：解压目标目录（成员按包内相对路径落在其下）。
/// - `members`：要解压的包内路径。
pub fn extract_members(archive: &Path, dest: &Path, members: &[&str]) -> Result<(), String> {
    let output = quiet_command(&system_tool("tar.exe"))
        .arg("-xf")
        .arg(archive)
        .arg("-C")
        .arg(dest)
        .args(members)
        .output()
        .map_err(|e| format!("无法运行 tar: {e}"))?;
    if output.status.success() {
        Ok(())
    } else {
        Err(format!("解压失败: {}", String::from_utf8_lossy(&output.stderr).trim()))
    }
}

/// 写完成标记。
pub(crate) fn write_marker(dir: &Path) -> Result<(), String> {
    std::fs::write(dir.join(COMPLETE_MARKER), MARKER_CONTENT).map_err(|e| format!("写完成标记失败: {e}"))
}

/// 执行下载计划：运行时（下载 → 解压 → 逐文件校验）与模型（逐文件下载校验），各自完成后写标记。
///
/// # 参数
/// - `plan`：下载计划。
/// - `cancel`：取消开关。
/// - `progress`：进度回调，参数是当前步骤的说明。
///
/// # 返回
/// 全部成功返回 `Ok`；任一步失败立即返回错误说明。
pub fn execute_plan(
    plan: &DownloadPlan,
    cancel: &AtomicBool,
    mut progress: impl FnMut(&str),
) -> Result<(), String> {
    if let Some(runtime) = &plan.runtime {
        progress("正在下载 OCR 运行时…");
        let archive = fetch_verified(&runtime.archive, cancel)?;
        progress("正在解压 OCR 运行时…");
        extract_zip(&archive, &runtime.extract_to)?;
        for file in &runtime.files {
            verify_file(&runtime.extract_to.join(&file.name), file)?;
        }
        write_marker(&runtime.extract_to)?;
        let _ = std::fs::remove_file(&archive);
    }
    if let Some((dir, items)) = &plan.model {
        for (index, item) in items.iter().enumerate() {
            progress(&format!("正在下载 OCR 模型 ({}/{})…", index + 1, items.len()));
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
    progress: impl FnMut(&str),
) -> Result<(), String> {
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
        let at = with.iter().position(|a| a == "--proxy").expect("应带 --proxy");
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
        let spaced: Vec<String> = hash.as_bytes().chunks(2).map(|c| String::from_utf8_lossy(c).into_owned()).collect();
        let out = format!("SHA256 的 x 哈希:\r\n{}\r\nCertUtil: -hashfile 命令成功完成。\r\n", spaced.join(" "));
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
        assert!(verify_file(&path, &bad_size).unwrap_err().contains("大小不符"));
        let mut bad_hash = file.clone();
        bad_hash.sha256 = "0".repeat(64);
        assert!(verify_file(&path, &bad_hash).unwrap_err().contains("哈希不符"));
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
        execute_plan(&plan, &cancel, |s| steps.push(s.to_string())).expect("下载");
        assert_eq!(steps.len(), 3);
        assert!(plan_from_manifest(&manifest, &root, "tiny", false).expect("再规划").is_empty());
        let m_dir = model_dir(&root, &manifest.models[0]);
        assert!(m_dir.join(COMPLETE_MARKER).is_file());
        assert!(!m_dir.join("det.onnx.part").exists());

        // 篡改源文件：内容变了但清单哈希没变 -> 校验失败，且不留 .part
        let root2 = temp_root("dst2");
        std::fs::write(src.join("det.onnx"), b"detector-BYTES").expect("篡改");
        let plan = plan_from_manifest(&manifest, &root2, "tiny", false).expect("规划");
        let err = execute_plan(&plan, &cancel, |_| {}).unwrap_err();
        assert!(err.contains("哈希不符"), "{err}");
        let m_dir2 = model_dir(&root2, &manifest.models[0]);
        assert!(!m_dir2.join("det.onnx.part").exists());
        assert!(!m_dir2.join(COMPLETE_MARKER).exists());
        // 取消开关
        cancel.store(true, Ordering::Relaxed);
        let plan = plan_from_manifest(&manifest, &root2, "tiny", false).expect("规划");
        assert_eq!(execute_plan(&plan, &cancel, |_| {}).unwrap_err(), "已取消");
        for dir in [src, root, root2] {
            let _ = std::fs::remove_dir_all(&dir);
        }
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
        assert!(err.contains("下载失败"), "{err}");
        let _ = std::fs::remove_dir_all(&root);
    }

    /// 真实网络：从 modelscope 下载清单里最小的字典文件并校验哈希（`--ignored`）。
    #[test]
    #[ignore = "需要联网"]
    fn real_network_download_of_small_file() {
        let m = manifest().expect("清单");
        let model = find_model(m, "extra_small").expect("模型");
        let file = model.files.iter().find(|f| f.name.ends_with(".txt")).expect("字典").clone();
        let dir = temp_root("net");
        let item = DownloadItem { file, dest_dir: dir.clone() };
        let started = std::time::Instant::now();
        let path = fetch_verified(&item, &AtomicBool::new(false)).expect("下载并校验");
        println!("NET|{}|{} bytes|{} ms|sha256 verified", item.file.name, std::fs::metadata(&path).map_or(0, |m| m.len()), started.elapsed().as_millis());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
