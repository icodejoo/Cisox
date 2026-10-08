//! onnxruntime 运行时的定位与按需下载（仅本地翻译 worker 使用）。
//!
//! OCR 运行时是上游发布的静态链接版本（自带 ORT，只需要 DirectML.dll），不使用这里的动态库；
//! 翻译 worker 用 `ort` 的 load-dynamic 方式，需要一份 `onnxruntime.dll`。这份运行时放在用户数据目录
//! `<数据根>/runtime/onnxruntime/<版本>/`，按需下载（PyPI 上 Microsoft 官方发布的 wheel，取其中的 CPU 版 DLL），
//! 下载后逐个文件核对大小与 SHA-256，清单见 `resources/ort-runtime-manifest.json`。
//! 下载复用 OCR 资产下载的 curl / 哈希校验 / 解压工具，不新增任何依赖。

use crate::ocr_assets::{AssetFile, COMPLETE_MARKER};
use crate::ocr_download::{
    DownloadItem, DownloadStep, FetchError, extract_members, fetch_verified, verify_file,
    write_marker,
};
use serde::Deserialize;
use snow_translate::worker::ENV_ORT_DYLIB;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::sync::atomic::AtomicBool;

/// 数据根下的运行时总目录名。
const RUNTIME_DIR: &str = "runtime";
/// onnxruntime 子目录名。
const ORT_DIR: &str = "onnxruntime";
/// 主动态库文件名。
pub const ORT_DLL_NAME: &str = "onnxruntime.dll";
/// 下载压缩包的临时子目录。
const DOWNLOAD_SUBDIR: &str = ".download";
/// 解压成员的临时子目录。
const EXTRACT_SUBDIR: &str = ".extract";
/// 内置清单。
const MANIFEST_JSON: &str = include_str!("../resources/ort-runtime-manifest.json");

/// 压缩包内需要取出的一个文件。
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct OrtMember {
    /// 包内相对路径。
    pub path_in_archive: String,
    /// 落地文件名。
    pub name: String,
    /// 字节数。
    pub size: u64,
    /// SHA-256（小写十六进制）。
    pub sha256: String,
}

/// onnxruntime 运行时清单。
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct OrtManifest {
    /// 版本号。
    pub version: String,
    /// 平台标记。
    pub platform: String,
    /// 来源说明。
    #[serde(default)]
    pub source: String,
    /// 下载的压缩包。
    pub archive: AssetFile,
    /// 需要取出的文件。
    pub members: Vec<OrtMember>,
}

/// 解析内置清单（只解析一次）。
///
/// # 返回
/// 清单引用；内置 JSON 损坏时返回解析器给出的原因（技术信息，不翻译）。
///
/// ```ignore
/// assert_eq!(manifest().unwrap().version, "1.28.0");
/// ```
pub fn manifest() -> Result<&'static OrtManifest, String> {
    static CELL: OnceLock<Result<OrtManifest, String>> = OnceLock::new();
    CELL.get_or_init(|| serde_json::from_str(MANIFEST_JSON).map_err(|e| e.to_string()))
        .as_ref()
        .map_err(Clone::clone)
}

/// onnxruntime 清单的组件名（错误文案里的技术名词）。
const ORT_MANIFEST_NAME: &str = "onnxruntime";

/// 运行时目录：`<数据根>/runtime/onnxruntime/<版本>`。
///
/// # 参数
/// - `data_root`：应用数据根目录。
/// - `manifest`：清单（取版本号）。
pub fn runtime_dir(data_root: &Path, manifest: &OrtManifest) -> PathBuf {
    data_root
        .join(RUNTIME_DIR)
        .join(ORT_DIR)
        .join(&manifest.version)
}

/// 目录里的运行时是否齐全：有完成标记且每个文件大小与清单一致。
///
/// # 参数
/// - `dir`：运行时目录。
/// - `members`：清单里的文件。
pub fn runtime_complete(dir: &Path, members: &[OrtMember]) -> bool {
    dir.join(COMPLETE_MARKER).is_file()
        && members.iter().all(|m| {
            std::fs::metadata(dir.join(&m.name)).is_ok_and(|f| f.is_file() && f.len() == m.size)
        })
}

/// onnxruntime 不可用的原因。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OrtUnavailable {
    /// 没有下载过运行时。
    NotInstalled,
    /// 环境变量指定的文件不存在。
    EnvMissing(PathBuf),
    /// 内置清单损坏。
    Manifest(String),
}

impl OrtUnavailable {
    /// 技术说明（英文，给日志与上层错误的附带细节用；面向用户的文案由调用方按场景选择）。
    pub fn detail(&self) -> String {
        match self {
            Self::NotInstalled => "the onnxruntime runtime is not installed".to_string(),
            Self::EnvMissing(path) => format!(
                "{ENV_ORT_DYLIB} points to a file that does not exist: {}",
                path.display()
            ),
            Self::Manifest(detail) => detail.clone(),
        }
    }

    /// 是否可以通过下载解决（环境变量指定了错误路径时下载没有用）。
    pub fn can_download(&self) -> bool {
        matches!(self, Self::NotInstalled)
    }
}

/// 定位 onnxruntime 动态库：环境变量 `SNOW_ORT_DYLIB` 优先，否则用数据目录里下载好的。
///
/// # 参数
/// - `data_root`：应用数据根目录。
/// - `env_value`：`SNOW_ORT_DYLIB` 的值（空白视为未设置）。
///
/// # 返回
/// `onnxruntime.dll` 路径；不可用时返回原因。
///
/// ```ignore
/// let dll = resolve_ort_dylib(&data_root, std::env::var(ENV_ORT_DYLIB).ok().as_deref())?;
/// ```
pub fn resolve_ort_dylib(
    data_root: &Path,
    env_value: Option<&str>,
) -> Result<PathBuf, OrtUnavailable> {
    if let Some(value) = env_value.map(str::trim).filter(|v| !v.is_empty()) {
        let path = PathBuf::from(value);
        return if path.is_file() {
            Ok(path)
        } else {
            Err(OrtUnavailable::EnvMissing(path))
        };
    }
    let manifest = manifest().map_err(OrtUnavailable::Manifest)?;
    let dir = runtime_dir(data_root, manifest);
    if runtime_complete(&dir, &manifest.members) {
        Ok(dir.join(ORT_DLL_NAME))
    } else {
        Err(OrtUnavailable::NotInstalled)
    }
}

/// 下载并安装运行时（用给定清单）：下载压缩包 → 校验 → 只解出 DLL → 逐个校验 → 落地 → 写标记。
///
/// # 参数
/// - `manifest`：清单。
/// - `data_root`：应用数据根目录。
/// - `cancel`：取消开关（文件之间检查）。
/// - `progress`：进度回调，参数是当前步骤说明。
///
/// # 返回
/// `onnxruntime.dll` 的最终路径；任一步失败返回错误说明（临时文件会被清理）。
pub fn install_with(
    manifest: &OrtManifest,
    data_root: &Path,
    cancel: &AtomicBool,
    mut progress: impl FnMut(DownloadStep),
) -> Result<PathBuf, FetchError> {
    let dir = runtime_dir(data_root, manifest);
    if runtime_complete(&dir, &manifest.members) {
        return Ok(dir.join(ORT_DLL_NAME));
    }
    let download_dir = dir.join(DOWNLOAD_SUBDIR);
    let extract_dir = dir.join(EXTRACT_SUBDIR);
    let result = (|| {
        progress(DownloadStep::OrtDownload);
        let archive = fetch_verified(
            &DownloadItem {
                file: manifest.archive.clone(),
                dest_dir: download_dir.clone(),
            },
            cancel,
        )?;
        progress(DownloadStep::OrtExtract);
        let _ = std::fs::remove_dir_all(&extract_dir);
        std::fs::create_dir_all(&extract_dir).map_err(|e| FetchError::CreateDir(e.to_string()))?;
        let members: Vec<&str> = manifest
            .members
            .iter()
            .map(|m| m.path_in_archive.as_str())
            .collect();
        extract_members(&archive, &extract_dir, &members)?;
        for member in &manifest.members {
            let extracted = extract_dir.join(&member.path_in_archive);
            let expected = AssetFile {
                name: member.name.clone(),
                size: member.size,
                sha256: member.sha256.clone(),
                url: String::new(),
            };
            verify_file(&extracted, &expected)?;
            let target = dir.join(&member.name);
            let _ = std::fs::remove_file(&target);
            std::fs::rename(&extracted, &target).map_err(|e| FetchError::InstallMember {
                name: member.name.clone(),
                detail: e.to_string(),
            })?;
        }
        write_marker(&dir)?;
        Ok(dir.join(ORT_DLL_NAME))
    })();
    let _ = std::fs::remove_dir_all(&download_dir);
    let _ = std::fs::remove_dir_all(&extract_dir);
    result
}

/// 下载并安装内置清单指定的运行时。
///
/// # 参数
/// - `data_root`：应用数据根目录。
/// - `cancel` / `progress`：同 [`install_with`]。
///
/// # 返回
/// `onnxruntime.dll` 的最终路径。
pub fn install(
    data_root: &Path,
    cancel: &AtomicBool,
    progress: impl FnMut(DownloadStep),
) -> Result<PathBuf, FetchError> {
    let manifest = manifest().map_err(|detail| FetchError::Manifest {
        what: ORT_MANIFEST_NAME,
        detail,
    })?;
    install_with(manifest, data_root, cancel, progress)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ocr_download::sha256_file;
    use std::process::Command;

    /// 唯一临时目录。
    fn temp_root(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("snow-ort-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("建目录");
        dir
    }

    /// 内置清单可解析：版本、哈希长度、URL 协议、成员路径都合理。
    #[test]
    fn embedded_manifest_is_sane() {
        let m = manifest().expect("清单");
        assert_eq!(m.version, "1.28.0");
        assert_eq!(m.archive.sha256.len(), 64);
        assert!(m.archive.url.starts_with("https://"));
        assert!(m.archive.name.ends_with(".whl"));
        assert!(m.members.iter().any(|f| f.name == ORT_DLL_NAME));
        for member in &m.members {
            assert_eq!(member.sha256.len(), 64, "{}", member.name);
            assert!(member.path_in_archive.ends_with(&member.name));
            assert!(member.size > 0);
        }
    }

    /// 环境变量优先；指向不存在的文件报 EnvMissing 且不可下载；空白视为未设置。
    #[test]
    fn env_override_rules() {
        let root = temp_root("env");
        let dll = root.join("custom.dll");
        std::fs::write(&dll, b"x").expect("写 dll");
        assert_eq!(
            resolve_ort_dylib(&root, Some(dll.to_str().unwrap())).unwrap(),
            dll
        );
        let missing = root.join("missing.dll");
        let err = resolve_ort_dylib(&root, Some(missing.to_str().unwrap())).unwrap_err();
        assert_eq!(err, OrtUnavailable::EnvMissing(missing));
        assert!(!err.can_download());
        assert!(err.detail().contains(ENV_ORT_DYLIB));
        assert_eq!(
            resolve_ort_dylib(&root, Some("  ")),
            Err(OrtUnavailable::NotInstalled)
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    /// 没装：NotInstalled（可下载）；装齐（有标记且大小对）：返回数据目录里的 DLL；缺标记或大小不符都算没装。
    #[test]
    fn data_dir_resolution() {
        let root = temp_root("resolve");
        let m = manifest().expect("清单");
        let err = resolve_ort_dylib(&root, None).unwrap_err();
        assert_eq!(err, OrtUnavailable::NotInstalled);
        assert!(err.can_download() && err.detail().contains("not installed"));
        let dir = runtime_dir(&root, m);
        std::fs::create_dir_all(&dir).expect("建目录");
        for member in &m.members {
            std::fs::File::create(dir.join(&member.name))
                .expect("建文件")
                .set_len(member.size)
                .expect("定长");
        }
        assert!(resolve_ort_dylib(&root, None).is_err(), "缺标记");
        std::fs::write(dir.join(COMPLETE_MARKER), b"{}").expect("标记");
        assert_eq!(
            resolve_ort_dylib(&root, None).unwrap(),
            dir.join(ORT_DLL_NAME)
        );
        std::fs::write(dir.join(ORT_DLL_NAME), b"short").expect("改小");
        assert!(resolve_ort_dylib(&root, None).is_err(), "大小不符");
        let _ = std::fs::remove_dir_all(&root);
    }

    /// 用本机 tar 造一个假 wheel（zip），走完整的 下载(file://) → 校验 → 解压 → 落地 流程；哈希不符则拒绝安装。
    #[test]
    fn install_flow_with_local_archive() {
        let system_root = std::env::var_os("SystemRoot")
            .map(PathBuf::from)
            .unwrap_or_default();
        let tar = system_root.join("System32").join("tar.exe");
        if !tar.is_file() {
            return;
        }
        let root = temp_root("install");
        let src = root.join("src");
        let capi = src.join("onnxruntime").join("capi");
        std::fs::create_dir_all(&capi).expect("建包内目录");
        std::fs::write(capi.join("onnxruntime.dll"), b"fake-dll-bytes").expect("写");
        std::fs::write(
            capi.join("onnxruntime_providers_shared.dll"),
            b"fake-shared",
        )
        .expect("写");
        let wheel = root.join("fake.whl");
        let status = Command::new(&tar)
            .args(["-a", "-c", "-f"])
            .arg(&wheel)
            .arg("-C")
            .arg(&src)
            .arg("onnxruntime")
            .status()
            .expect("tar 打包");
        assert!(status.success());
        let url = format!("file:///{}", wheel.to_string_lossy().replace('\\', "/"));
        let member = |name: &str, bytes: &[u8]| {
            let path = capi.join(name);
            OrtMember {
                path_in_archive: format!("onnxruntime/capi/{name}"),
                name: name.to_string(),
                size: bytes.len() as u64,
                sha256: sha256_file(&path).expect("哈希"),
            }
        };
        let mut manifest = OrtManifest {
            version: "9.9.9".into(),
            platform: "test".into(),
            source: String::new(),
            archive: AssetFile {
                name: "fake.whl".into(),
                size: std::fs::metadata(&wheel).expect("元数据").len(),
                sha256: sha256_file(&wheel).expect("哈希"),
                url,
            },
            members: vec![
                member("onnxruntime.dll", b"fake-dll-bytes"),
                member("onnxruntime_providers_shared.dll", b"fake-shared"),
            ],
        };
        let data_root = root.join("data");
        let cancel = AtomicBool::new(false);
        let mut steps = Vec::new();
        let dll = install_with(&manifest, &data_root, &cancel, |s| steps.push(s)).expect("安装");
        assert_eq!(std::fs::read(&dll).unwrap(), b"fake-dll-bytes");
        assert_eq!(steps.len(), 2);
        let dir = runtime_dir(&data_root, &manifest);
        assert!(runtime_complete(&dir, &manifest.members));
        assert!(
            !dir.join(DOWNLOAD_SUBDIR).exists() && !dir.join(EXTRACT_SUBDIR).exists(),
            "临时目录应清理"
        );
        // 已安装：直接返回，不再走下载
        let mut again = Vec::new();
        install_with(&manifest, &data_root, &cancel, |s| again.push(s)).expect("幂等");
        assert!(again.is_empty());
        // 成员哈希被篡改：拒绝安装且不写标记
        let bad_root = root.join("bad");
        manifest.members[0].sha256 = "0".repeat(64);
        let err = install_with(&manifest, &bad_root, &cancel, |_| {}).unwrap_err();
        assert!(matches!(err, FetchError::HashMismatch { .. }), "{err:?}");
        assert!(!runtime_complete(
            &runtime_dir(&bad_root, &manifest),
            &manifest.members
        ));
        let _ = std::fs::remove_dir_all(&root);
    }

    /// 真实网络安装（默认忽略；`cargo test -p snow-shot real_network_install -- --ignored --nocapture`）：
    /// 从 PyPI 下载官方 wheel、校验哈希、解出 DLL 到临时数据根，并确认幂等。不开任何窗口。
    #[test]
    #[ignore = "需要联网下载约 14 MB"]
    fn real_network_install() {
        let root = temp_root("net");
        let manifest = manifest().expect("清单");
        let cancel = AtomicBool::new(false);
        let started = std::time::Instant::now();
        let mut steps = Vec::new();
        let dll = install(&root, &cancel, |s| steps.push(s)).expect("真实安装");
        eprintln!(
            "NET install ok in {:?}: {} steps={steps:?}",
            started.elapsed(),
            dll.display()
        );
        for member in &manifest.members {
            let path = dll.parent().expect("目录").join(&member.name);
            assert_eq!(
                sha256_file(&path).expect("哈希"),
                member.sha256,
                "{}",
                member.name
            );
        }
        assert_eq!(resolve_ort_dylib(&root, None).expect("已安装"), dll);
        assert!(install(&root, &cancel, |_| {}).is_ok());
        eprintln!("NET dll={}", dll.display());
        std::fs::write(
            std::env::temp_dir().join("snow-ort-last-install.txt"),
            dll.to_string_lossy().as_bytes(),
        )
        .ok();
    }

    /// 已取消：不会发起下载。
    #[test]
    fn cancelled_install_does_nothing() {
        let root = temp_root("cancel");
        let m = manifest().expect("清单");
        let cancel = AtomicBool::new(true);
        let err = install_with(m, &root, &cancel, |_| {}).unwrap_err();
        assert_eq!(err, FetchError::Cancelled);
        let _ = std::fs::remove_dir_all(&root);
    }
}
