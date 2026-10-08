//! 语音模型按需下载：下载 `.tar.bz2` → 大小与 SHA-256 校验 → 只解压必需文件 → 写 `model.json` 与完成标记。
//!
//! 沿用 OCR 下载的做法且不新增依赖：下载用系统自带 `curl.exe`（`--ssl-no-revoke`，续传），
//! 哈希用 `certutil`，解压用系统自带 `tar.exe`（bsdtar，原生支持 bz2）。
//! 落点 `<数据根>/models/stt/<id>/`，离线模式共用的 `silero_vad.onnx` 放在其父目录。
//! 清单里 sha256 为空的资产放行但会记日志，并在 [`InstallReport::unpinned`] 里返回，让界面提示「校验值待固定」。
//! 下载在调用线程里阻塞执行，调用方应放到后台线程。

use crate::ocr_assets::COMPLETE_MARKER;
use crate::ocr_download::{
    FetchError, TOOL_CURL, TOOL_TAR, curl_command, quiet_command, sha256_file, system_tool,
    write_marker,
};
use crate::stt_models::{self, SharedAsset, SttModelSpec, mode_as_str};
use snow_stt_protocol::ModelKind;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

/// 模型元数据文件名。
pub const MODEL_META_FILE: &str = "model.json";
/// 元数据版本号。
const META_SCHEMA: u32 = 1;
/// 下载中的临时文件后缀。
const PART_SUFFIX: &str = ".part";
/// 解压暂存目录后缀。
const STAGING_SUFFIX: &str = ".extracting";
/// 下载缓存子目录（位于模型根目录下）。
const DOWNLOAD_DIR: &str = ".download";
/// 轮询下载进程与进度的间隔。
const POLL_INTERVAL: Duration = Duration::from_millis(250);
/// 下载结束的归类，用于决定日志级别。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    /// 成功。
    Done,
    /// 用户主动取消。
    Cancelled,
    /// 真正失败。
    Failed,
}

/// 把安装结果归类为成功 / 取消 / 失败。
///
/// # 参数
/// - `result`：[`install`] 映射后的结果。
///
/// # 返回
/// 错误是取消时归为 [`Outcome::Cancelled`]。
pub fn classify(result: &Result<(), FetchError>) -> Outcome {
    match result {
        Ok(()) => Outcome::Done,
        Err(FetchError::Cancelled) => Outcome::Cancelled,
        Err(_) => Outcome::Failed,
    }
}

/// 安装阶段。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stage {
    /// 正在下载。
    Downloading,
    /// 正在校验大小与哈希。
    Verifying,
    /// 正在解压。
    Extracting,
}

/// 进度快照。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Progress {
    /// 当前阶段。
    pub stage: Stage,
    /// 当前处理的资产名（压缩包或 VAD 文件名）。
    pub asset: String,
    /// 已下载字节数（非下载阶段为总大小）。
    pub done: u64,
    /// 总字节数（取清单里的大小）。
    pub total: u64,
}

/// 待下载的 VAD。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VadDownload {
    /// 清单里的 VAD 资产。
    pub asset: SharedAsset,
    /// 落地路径。
    pub dest: PathBuf,
}

/// 一次安装的计划（缺哪些补哪些）。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DownloadPlan {
    /// 需要下载的 VAD（仅离线模式且尚未安装）。
    pub vad: Option<VadDownload>,
    /// 需要下载的模型（尚未安装）。
    pub model: Option<PathBuf>,
}

impl DownloadPlan {
    /// 计划里是否什么都不用下。
    pub fn is_empty(&self) -> bool {
        self.vad.is_none() && self.model.is_none()
    }
}

/// 一次安装的结果。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct InstallReport {
    /// 本次安装的、清单里 sha256 为空（未做哈希校验）的资产名。
    pub unpinned: Vec<String>,
}

/// 模型是否已安装：完成标记在且 `files` 全部存在。
///
/// # 参数
/// - `spec`：模型。
/// - `data_root`：应用数据根目录。
pub fn is_installed(spec: &SttModelSpec, data_root: &Path) -> bool {
    let dir = stt_models::model_dir(spec, data_root);
    dir.join(COMPLETE_MARKER).is_file() && spec.files.iter().all(|f| dir.join(f).is_file())
}

/// 共享 VAD 是否已安装：文件存在且大小与清单一致。
///
/// # 参数
/// - `data_root`：应用数据根目录。
pub fn is_vad_installed(data_root: &Path) -> bool {
    vad_installed_with(&stt_models::manifest().vad, data_root)
}

/// 用给定 VAD 资产判断是否已安装。
fn vad_installed_with(asset: &SharedAsset, data_root: &Path) -> bool {
    std::fs::metadata(stt_models::models_root(data_root).join(&asset.name))
        .is_ok_and(|m| m.is_file() && m.len() == asset.size)
}

/// 规划要下载的内容：模型未安装才下；离线模式额外检查共享 VAD。
///
/// # 参数
/// - `spec`：模型。
/// - `data_root`：应用数据根目录。
///
/// # 返回
/// 下载计划；已全部就绪时为空计划。
///
/// ```ignore
/// let plan = plan_for(spec, root);
/// if plan.is_empty() { /* 已安装 */ }
/// ```
pub fn plan_for(spec: &SttModelSpec, data_root: &Path) -> DownloadPlan {
    plan_with(spec, &stt_models::manifest().vad, data_root)
}

/// 用给定 VAD 资产规划（测试可注入）。
fn plan_with(spec: &SttModelSpec, vad: &SharedAsset, data_root: &Path) -> DownloadPlan {
    let needs_vad = spec.kind.is_offline() && !vad_installed_with(vad, data_root);
    DownloadPlan {
        vad: needs_vad.then(|| VadDownload {
            asset: vad.clone(),
            dest: stt_models::models_root(data_root).join(&vad.name),
        }),
        model: (!is_installed(spec, data_root)).then(|| stt_models::model_dir(spec, data_root)),
    }
}

/// 校验文件大小与（非空时的）哈希。
///
/// # 参数
/// - `path`：文件路径。
/// - `name`：用于报错的名字。
/// - `size`：期望字节数。
/// - `sha256`：期望哈希，空串表示未固定（放行）。
///
/// # 返回
/// 通过返回「哈希是否已固定」；不符返回说明。
pub fn verify_asset(path: &Path, name: &str, size: u64, sha256: &str) -> Result<bool, FetchError> {
    let actual_size = std::fs::metadata(path)
        .map_err(|e| FetchError::Stat {
            name: name.to_string(),
            detail: e.to_string(),
        })?
        .len();
    if actual_size != size {
        return Err(FetchError::SizeMismatch {
            name: name.to_string(),
            expected: size,
            actual: actual_size,
        });
    }
    if sha256.is_empty() {
        tracing::warn!(asset = name, "清单未固定该资产的 SHA-256，已跳过哈希校验");
        return Ok(false);
    }
    let actual = sha256_file(path)?;
    if actual != sha256.to_ascii_lowercase() {
        return Err(FetchError::HashMismatch {
            name: name.to_string(),
            expected: sha256.to_string(),
            actual,
        });
    }
    Ok(true)
}

/// 下载到 `part`（支持续传），期间每个轮询周期回调已下载字节数并检查取消；取消时结束 curl 进程并保留 `.part` 以便续传。
fn download_to_part(
    url: &str,
    part: &Path,
    cancel: &AtomicBool,
    mut on_bytes: impl FnMut(u64),
) -> Result<(), FetchError> {
    let mut child = curl_command(url, part)
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| FetchError::RunTool {
            tool: TOOL_CURL,
            detail: e.to_string(),
        })?;
    loop {
        if cancel.load(Ordering::Relaxed) {
            let _ = child.kill();
            let _ = child.wait();
            return Err(FetchError::Cancelled);
        }
        match child.try_wait().map_err(|e| FetchError::WaitTool {
            tool: TOOL_CURL,
            detail: e.to_string(),
        })? {
            Some(status) => {
                let mut stderr = String::new();
                if let Some(mut pipe) = child.stderr.take() {
                    let _ = pipe.read_to_string(&mut stderr);
                }
                // 个别情况下 curl 报错却返回 0（如 file:// 源不存在），所以还要确认确实产出了文件
                if status.success() && part.is_file() {
                    return Ok(());
                }
                return Err(FetchError::DownloadFailed {
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

/// 下载并校验一个资产，成功后原子改名到 `dest`；返回哈希是否已固定。校验失败会删除 `.part`，避免续传坏数据。
fn fetch_asset(
    name: &str,
    url: &str,
    size: u64,
    sha256: &str,
    dest: &Path,
    cancel: &AtomicBool,
    on_progress: &mut impl FnMut(&Progress),
) -> Result<bool, FetchError> {
    if cancel.load(Ordering::Relaxed) {
        return Err(FetchError::Cancelled);
    }
    let parent = dest.parent().ok_or(FetchError::InvalidTarget)?;
    std::fs::create_dir_all(parent).map_err(|e| FetchError::CreateDir(e.to_string()))?;
    let part = PathBuf::from(format!("{}{PART_SUFFIX}", dest.display()));
    let mut report = |stage, done| {
        on_progress(&Progress {
            stage,
            asset: name.to_string(),
            done,
            total: size,
        })
    };
    report(Stage::Downloading, 0);
    download_to_part(url, &part, cancel, |done| report(Stage::Downloading, done))?;
    report(Stage::Verifying, size);
    let pinned = verify_asset(&part, name, size, sha256).inspect_err(|_| {
        let _ = std::fs::remove_file(&part);
    })?;
    std::fs::rename(&part, dest).map_err(|e| FetchError::Rename(e.to_string()))?;
    Ok(pinned)
}

/// 只解压压缩包里 `<id>/<file>` 成员到 `dest`（系统自带 `tar.exe`，自动识别 bz2）。
fn extract_files(
    archive: &Path,
    dest: &Path,
    id: &str,
    files: &[String],
) -> Result<(), FetchError> {
    let output = quiet_command(&system_tool("tar.exe"))
        .arg("-xf")
        .arg(archive)
        .arg("-C")
        .arg(dest)
        .args(files.iter().map(|f| format!("{id}/{f}")))
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

/// 写 `model.json`：记录模型的基本元数据，便于排查与后续升级。
fn write_model_meta(spec: &SttModelSpec, dir: &Path, pinned: bool) -> Result<(), FetchError> {
    let meta = serde_json::json!({
        "schema": META_SCHEMA,
        "id": spec.id,
        "dimension": spec.dimension.as_str(),
        "mode": mode_as_str(spec.mode),
        "kind": spec.kind.as_str(),
        "files": spec.files,
        "itn_default": spec.kind == ModelKind::OfflineSenseVoice,
        "archive_sha256": spec.archive.sha256,
        "sha256_pinned": pinned,
    });
    let text = serde_json::to_string_pretty(&meta)
        .map_err(|e| FetchError::SerializeMeta(e.to_string()))?;
    std::fs::write(dir.join(MODEL_META_FILE), text)
        .map_err(|e| FetchError::WriteMeta(e.to_string()))
}

/// 安装模型本体：下载压缩包 → 校验 → 解压到暂存目录 → 核对文件 → 换入正式目录 → 写元数据与标记。
fn install_model(
    spec: &SttModelSpec,
    data_root: &Path,
    cancel: &AtomicBool,
    on_progress: &mut impl FnMut(&Progress),
) -> Result<bool, FetchError> {
    let root = stt_models::models_root(data_root);
    let archive_path = root.join(DOWNLOAD_DIR).join(&spec.archive.name);
    let a = &spec.archive;
    let pinned = fetch_asset(
        &a.name,
        &a.url,
        a.size,
        &a.sha256,
        &archive_path,
        cancel,
        on_progress,
    )?;
    on_progress(&Progress {
        stage: Stage::Extracting,
        asset: a.name.clone(),
        done: a.size,
        total: a.size,
    });
    let staging = root.join(format!("{}{STAGING_SUFFIX}", spec.id));
    let _ = std::fs::remove_dir_all(&staging);
    std::fs::create_dir_all(&staging).map_err(|e| FetchError::CreateStaging(e.to_string()))?;
    let result = (|| {
        extract_files(&archive_path, &staging, &spec.id, &spec.files)?;
        let extracted = staging.join(&spec.id);
        if let Some(missing) = spec.files.iter().find(|f| !extracted.join(f).is_file()) {
            return Err(FetchError::MissingFiles(missing.clone()));
        }
        let dir = stt_models::model_dir(spec, data_root);
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::rename(&extracted, &dir).map_err(|e| FetchError::SwapDir(e.to_string()))?;
        write_model_meta(spec, &dir, pinned)?;
        write_marker(&dir)
    })();
    let _ = std::fs::remove_dir_all(&staging);
    result?;
    let _ = std::fs::remove_file(&archive_path);
    Ok(pinned)
}

/// 安装共享 VAD；返回哈希是否已固定。
fn install_vad(
    asset: &SharedAsset,
    data_root: &Path,
    cancel: &AtomicBool,
    on_progress: &mut impl FnMut(&Progress),
) -> Result<bool, FetchError> {
    let dest = stt_models::models_root(data_root).join(&asset.name);
    fetch_asset(
        &asset.name,
        &asset.url,
        asset.size,
        &asset.sha256,
        &dest,
        cancel,
        on_progress,
    )
}

/// 安装模型（含离线模式所需的共享 VAD）；已就绪的部分跳过。
///
/// # 参数
/// - `spec`：要安装的模型。
/// - `data_root`：应用数据根目录。
/// - `cancel`：置位后尽快放弃（下载中会结束 curl，保留 `.part` 可续传）。
/// - `on_progress`：进度回调（阶段、资产名、已下/总字节）。
///
/// # 返回
/// 成功返回安装报告（含未固定 sha256 的资产名）；失败返回错误说明。
///
/// ```ignore
/// let report = install(spec, &root, &cancel, |p| println!("{:?} {}/{}", p.stage, p.done, p.total))?;
/// ```
pub fn install(
    spec: &SttModelSpec,
    data_root: &Path,
    cancel: &AtomicBool,
    on_progress: impl FnMut(&Progress),
) -> Result<InstallReport, FetchError> {
    install_with(
        spec,
        &stt_models::manifest().vad,
        data_root,
        cancel,
        on_progress,
    )
}

/// 用给定 VAD 资产安装（测试可注入）。
fn install_with(
    spec: &SttModelSpec,
    vad: &SharedAsset,
    data_root: &Path,
    cancel: &AtomicBool,
    mut on_progress: impl FnMut(&Progress),
) -> Result<InstallReport, FetchError> {
    let plan = plan_with(spec, vad, data_root);
    let mut report = InstallReport::default();
    if plan.vad.is_some() && !install_vad(vad, data_root, cancel, &mut on_progress)? {
        report.unpinned.push(vad.name.clone());
    }
    if plan.model.is_some() && !install_model(spec, data_root, cancel, &mut on_progress)? {
        report.unpinned.push(spec.archive.name.clone());
    }
    Ok(report)
}

/// 仅安装共享 VAD（离线模式的前置）。
///
/// # 参数
/// - `data_root` / `cancel` / `on_progress`：同 [`install`]。
///
/// # 返回
/// 成功返回安装报告；已安装则为空报告。
pub fn install_vad_asset(
    data_root: &Path,
    cancel: &AtomicBool,
    mut on_progress: impl FnMut(&Progress),
) -> Result<InstallReport, FetchError> {
    let vad = &stt_models::manifest().vad;
    let mut report = InstallReport::default();
    if !vad_installed_with(vad, data_root)
        && !install_vad(vad, data_root, cancel, &mut on_progress)?
    {
        report.unpinned.push(vad.name.clone());
    }
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 取消与真正失败分开归类。
    #[test]
    fn classify_separates_cancel_from_failure() {
        assert_eq!(classify(&Ok(())), Outcome::Done);
        assert_eq!(classify(&Err(FetchError::Cancelled)), Outcome::Cancelled);
        assert_eq!(
            classify(&Err(FetchError::Technical("network".to_string()))),
            Outcome::Failed
        );
    }

    use crate::stt_models::{ArchiveSpec, Dimension, Role};
    use snow_stt_protocol::RecognitionMode;

    /// 唯一临时目录。
    fn temp_root(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("snow-stt-dl-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("建目录");
        dir
    }

    /// 本地路径转 file:// 地址（离线可测）。
    fn file_url(path: &Path) -> String {
        format!("file:///{}", path.to_string_lossy().replace('\\', "/"))
    }

    /// 造一个本地 `.tar.bz2`（内含 `<id>/` 下的若干文件，另带一个不需要的大文件）与对应的模型条目。
    fn make_spec(src: &Path, id: &str, offline: bool, pin: bool) -> SttModelSpec {
        let pack = src.join(id);
        std::fs::create_dir_all(&pack).expect("建包目录");
        std::fs::write(pack.join("model.int8.onnx"), b"model-bytes").expect("写");
        std::fs::write(pack.join("tokens.txt"), b"a 1\n").expect("写");
        std::fs::write(pack.join("fp32.onnx"), vec![7u8; 4096]).expect("写");
        let archive = src.join(format!("{id}.tar.bz2"));
        let status = quiet_command(&system_tool("tar.exe"))
            .arg("-cjf")
            .arg(&archive)
            .arg("-C")
            .arg(src)
            .arg(id)
            .status()
            .expect("tar 打包");
        assert!(status.success());
        SttModelSpec {
            id: id.to_string(),
            dimension: Dimension::Zh,
            mode: if offline {
                RecognitionMode::Offline
            } else {
                RecognitionMode::Streaming
            },
            kind: if offline {
                ModelKind::OfflineParaformer
            } else {
                ModelKind::OnlineTransducer
            },
            role: Role::Default,
            archive: ArchiveSpec {
                name: format!("{id}.tar.bz2"),
                url: file_url(&archive),
                size: std::fs::metadata(&archive).expect("元数据").len(),
                sha256: if pin {
                    sha256_file(&archive).expect("哈希")
                } else {
                    String::new()
                },
            },
            files: vec!["model.int8.onnx".into(), "tokens.txt".into()],
            license: "test".into(),
            size_bytes: 15,
            peak_mem_mb: 1,
            notes_key: None,
        }
    }

    /// 造本地 VAD 资产。
    fn make_vad(src: &Path, pin: bool) -> SharedAsset {
        let path = src.join("silero_vad.onnx");
        std::fs::write(&path, b"vad-bytes").expect("写");
        SharedAsset {
            name: "silero_vad.onnx".into(),
            url: file_url(&path),
            size: 9,
            sha256: if pin {
                sha256_file(&path).expect("哈希")
            } else {
                String::new()
            },
            license: "MIT".into(),
        }
    }

    /// 校验：大小不符、哈希不符被拒；一致返回已固定；空哈希放行并标记未固定。
    #[test]
    fn verify_asset_rules() {
        let dir = temp_root("verify");
        let path = dir.join("abc.txt");
        std::fs::write(&path, b"abc").expect("写");
        let sha = "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad";
        assert_eq!(verify_asset(&path, "abc", 3, sha), Ok(true));
        assert_eq!(verify_asset(&path, "abc", 3, &sha.to_uppercase()), Ok(true));
        assert_eq!(verify_asset(&path, "abc", 3, ""), Ok(false));
        assert!(matches!(
            verify_asset(&path, "abc", 4, sha).unwrap_err(),
            FetchError::SizeMismatch { .. }
        ));
        assert!(matches!(
            verify_asset(&path, "abc", 3, &"0".repeat(64)).unwrap_err(),
            FetchError::HashMismatch { .. }
        ));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 端到端（file:// 源）：流式模型下载解压，只落必需文件，写 model.json 与标记；再规划为空；不留暂存与缓存压缩包。
    #[test]
    fn install_streaming_model_end_to_end() {
        let src = temp_root("e2e-src");
        let root = temp_root("e2e-dst");
        let spec = make_spec(&src, "pack-stream", false, true);
        let vad = make_vad(&src, true);
        assert!(!plan_with(&spec, &vad, &root).is_empty());
        assert!(
            plan_with(&spec, &vad, &root).vad.is_none(),
            "流式不需要 VAD"
        );
        let mut stages = Vec::new();
        let report = install_with(&spec, &vad, &root, &AtomicBool::new(false), |p| {
            stages.push(p.stage)
        })
        .expect("安装");
        assert!(report.unpinned.is_empty());
        assert!(stages.contains(&Stage::Downloading));
        assert!(stages.contains(&Stage::Verifying));
        assert!(stages.contains(&Stage::Extracting));
        assert!(is_installed(&spec, &root));
        assert!(plan_with(&spec, &vad, &root).is_empty());
        let dir = stt_models::model_dir(&spec, &root);
        assert!(!dir.join("fp32.onnx").exists(), "不需要的文件不应落盘");
        let meta: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(dir.join(MODEL_META_FILE)).expect("读"))
                .expect("解析");
        assert_eq!(meta["id"], "pack-stream");
        assert_eq!(meta["kind"], "online-transducer");
        assert_eq!(meta["mode"], "streaming");
        let models = stt_models::models_root(&root);
        assert!(!models.join("pack-stream.extracting").exists());
        assert!(
            !models
                .join(DOWNLOAD_DIR)
                .join("pack-stream.tar.bz2")
                .exists()
        );
        for d in [src, root] {
            let _ = std::fs::remove_dir_all(&d);
        }
    }

    /// 离线模型：计划里带 VAD，安装后 VAD 在模型目录的父目录；已装 VAD 时计划不再含它。
    #[test]
    fn offline_model_installs_shared_vad() {
        let src = temp_root("off-src");
        let root = temp_root("off-dst");
        let spec = make_spec(&src, "pack-offline", true, true);
        let vad = make_vad(&src, true);
        let plan = plan_with(&spec, &vad, &root);
        assert!(plan.vad.is_some() && plan.model.is_some());
        install_with(&spec, &vad, &root, &AtomicBool::new(false), |_| {}).expect("安装");
        let vad_file = stt_models::models_root(&root).join("silero_vad.onnx");
        assert!(vad_file.is_file());
        assert_eq!(
            vad_file.parent(),
            stt_models::model_dir(&spec, &root).parent()
        );
        assert!(vad_installed_with(&vad, &root));
        assert!(plan_with(&spec, &vad, &root).is_empty());
        for d in [src, root] {
            let _ = std::fs::remove_dir_all(&d);
        }
    }

    /// sha256 为空：放行安装，但报告里带出未固定的资产名（模型与 VAD）。
    #[test]
    fn unpinned_assets_are_reported() {
        let src = temp_root("unpin-src");
        let root = temp_root("unpin-dst");
        let spec = make_spec(&src, "pack-unpinned", true, false);
        let vad = make_vad(&src, false);
        let report =
            install_with(&spec, &vad, &root, &AtomicBool::new(false), |_| {}).expect("安装");
        assert_eq!(
            report.unpinned,
            vec![
                "silero_vad.onnx".to_string(),
                "pack-unpinned.tar.bz2".to_string()
            ]
        );
        assert!(is_installed(&spec, &root));
        for d in [src, root] {
            let _ = std::fs::remove_dir_all(&d);
        }
    }

    /// 哈希不符：安装失败，不留正式目录、完成标记与 `.part`。
    #[test]
    fn tampered_archive_is_rejected() {
        let src = temp_root("bad-src");
        let root = temp_root("bad-dst");
        let mut spec = make_spec(&src, "pack-bad", false, true);
        spec.archive.sha256 = "0".repeat(64);
        let vad = make_vad(&src, true);
        let err = install_with(&spec, &vad, &root, &AtomicBool::new(false), |_| {}).unwrap_err();
        assert!(matches!(err, FetchError::HashMismatch { .. }), "{err:?}");
        assert!(!is_installed(&spec, &root));
        assert!(!stt_models::model_dir(&spec, &root).exists());
        let part = stt_models::models_root(&root)
            .join(DOWNLOAD_DIR)
            .join("pack-bad.tar.bz2.part");
        assert!(!part.exists());
        for d in [src, root] {
            let _ = std::fs::remove_dir_all(&d);
        }
    }

    /// 清单声明了压缩包里没有的文件：解压后核对失败，不会被当成已安装。
    #[test]
    fn missing_member_fails_install() {
        let src = temp_root("miss-src");
        let root = temp_root("miss-dst");
        let mut spec = make_spec(&src, "pack-miss", false, true);
        spec.files.push("not-in-archive.onnx".into());
        let vad = make_vad(&src, true);
        let err = install_with(&spec, &vad, &root, &AtomicBool::new(false), |_| {}).unwrap_err();
        assert!(!matches!(err, FetchError::Cancelled));
        assert!(!is_installed(&spec, &root));
        assert!(
            !stt_models::model_dir(&spec, &root)
                .join(COMPLETE_MARKER)
                .exists()
        );
        for d in [src, root] {
            let _ = std::fs::remove_dir_all(&d);
        }
    }

    /// 取消开关已置位：直接返回「已取消」，什么都不落盘。
    #[test]
    fn cancel_stops_before_download() {
        let src = temp_root("cancel-src");
        let root = temp_root("cancel-dst");
        let spec = make_spec(&src, "pack-cancel", false, true);
        let vad = make_vad(&src, true);
        let err = install_with(&spec, &vad, &root, &AtomicBool::new(true), |_| {}).unwrap_err();
        assert_eq!(err, FetchError::Cancelled);
        assert!(!is_installed(&spec, &root));
        for d in [src, root] {
            let _ = std::fs::remove_dir_all(&d);
        }
    }

    /// 地址不可达：报下载失败而不是 panic，且不留 `.part`。
    #[test]
    fn unreachable_source_is_an_error() {
        let src = temp_root("dead-src");
        let root = temp_root("dead-dst");
        let mut spec = make_spec(&src, "pack-dead", false, true);
        spec.archive.url = "file:///Z:/definitely/not/here.tar.bz2".into();
        let vad = make_vad(&src, true);
        let err = install_with(&spec, &vad, &root, &AtomicBool::new(false), |_| {}).unwrap_err();
        assert!(matches!(err, FetchError::DownloadFailed { .. }), "{err:?}");
        assert!(!is_installed(&spec, &root));
        for d in [src, root] {
            let _ = std::fs::remove_dir_all(&d);
        }
    }

    /// 已安装判定：缺任一必需文件或没有完成标记都算未安装。
    #[test]
    fn installed_requires_marker_and_files() {
        let src = temp_root("inst-src");
        let root = temp_root("inst-dst");
        let spec = make_spec(&src, "pack-inst", false, true);
        let vad = make_vad(&src, true);
        install_with(&spec, &vad, &root, &AtomicBool::new(false), |_| {}).expect("安装");
        let dir = stt_models::model_dir(&spec, &root);
        std::fs::remove_file(dir.join("tokens.txt")).expect("删");
        assert!(!is_installed(&spec, &root));
        std::fs::write(dir.join("tokens.txt"), b"a").expect("写");
        assert!(is_installed(&spec, &root));
        std::fs::remove_file(dir.join(COMPLETE_MARKER)).expect("删");
        assert!(!is_installed(&spec, &root));
        for d in [src, root] {
            let _ = std::fs::remove_dir_all(&d);
        }
    }
}
