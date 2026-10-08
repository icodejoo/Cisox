//! 网络与更新设置的落地：`network/proxy` 变成 curl 代理参数，`updates/manifest_url` 变成检查更新的目标或“未配置”提示，
//! 以及“检查更新”的下载、比对与结果文案。计算部分是纯逻辑，可离屏测试。

use crate::ocr_backend::i18n_for;
use crate::ocr_download::{FetchError, curl_command, sha256_file};
use snow_config::document::ConfigDocument;
use snow_config::extensions::KEY_UPDATE_MANIFEST_URL;
use snow_i18n::Args;
use snow_update::{
    MAX_MANIFEST_BYTES, ManifestError, UpdateConfigError, UpdateStatus, check_manifest,
    resolve_manifest_url,
};
use std::path::{Path, PathBuf};

/// 代理配置键。
pub const KEY_PROXY: &str = "network/proxy";
/// 当前应用版本（来自 Cargo 包版本）。
pub const APP_VERSION: &str = env!("CARGO_PKG_VERSION");
/// 设置页里“更新”分组的 id。
pub const UPDATES_GROUP_ID: &str = "updates";
/// 拉取清单的总超时（秒）。
const MANIFEST_TIMEOUT_SECS: &str = "30";
/// 更新包在数据根目录下的子目录名。
const UPDATES_DIR: &str = "updates";
/// 从地址里取不出文件名时的兜底名。
const FALLBACK_UPDATE_FILE: &str = "cisox-update.bin";
/// 版本号不能当目录名时的兜底目录名。
const FALLBACK_VERSION_DIR: &str = "unknown";
/// 提示里更新说明保留的最大字符数。
const NOTES_PREVIEW_CHARS: usize = 80;

/// 清单里的一个可用更新（已通过解析校验）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UpdateInfo {
    /// 新版本号。
    pub version: String,
    /// 安装包下载地址（可空）。
    pub url: String,
    /// 更新说明（可空）。
    pub notes: String,
    /// 安装包 SHA-256（小写十六进制，可空表示不校验）。
    pub sha256: String,
}

/// 设置页“更新”分组里用户触发的动作。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UpdateAction {
    /// 检查更新。
    Check,
    /// 下载指定更新。
    Download(UpdateInfo),
    /// 打开已下载更新包所在目录。
    OpenFolder(PathBuf),
}

/// 下载更新包的结果（未本地化）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UpdateDownloadOutcome {
    /// 已下载：落盘路径与是否做过哈希校验。
    Done {
        /// 更新包落盘路径。
        path: PathBuf,
        /// 清单给了 sha256 且校验通过。
        verified: bool,
    },
    /// 清单没有下载地址。
    NoUrl,
    /// 下载或校验失败。
    Failed(FetchError),
}

/// “检查更新”在设置页里的界面状态。
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum UpdateUiState {
    /// 尚未检查。
    #[default]
    Idle,
    /// 检查进行中。
    Running,
    /// 发现新版本：可下载（`info.url` 非空时），`text` 是已本地化的提示（含更新说明或上次下载失败的原因）。
    Available {
        /// 可用更新。
        info: UpdateInfo,
        /// 已本地化的提示文案。
        text: String,
        /// 提示是否为失败类（用警示色）。
        failed: bool,
    },
    /// 更新包下载中。
    Downloading,
    /// 更新包已下载：所在目录与已本地化的提示。
    Downloaded {
        /// 更新包所在目录。
        dir: PathBuf,
        /// 已本地化的提示文案。
        text: String,
    },
    /// 已出结果：本地化文案与是否为失败类提示。
    Done {
        /// 已本地化的结果文案。
        text: String,
        /// 是否为失败 / 未配置类提示（用警示色）。
        failed: bool,
    },
}

/// “检查更新”的结果（未本地化的数据）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UpdateCheckOutcome {
    /// 已是最新。
    UpToDate,
    /// 有新版本。
    Available(UpdateInfo),
    /// 没有配置或配置的地址不合法（已本地化的提示）。
    Config(String),
    /// 下载失败（携带原因）。
    FetchFailed(String),
    /// 清单内容不合法。
    BadManifest,
}

/// 更新分组说明区的文案。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UpdatePanel {
    /// 标题。
    pub title: String,
    /// 当前版本行。
    pub current_line: String,
    /// 按钮文案。
    pub button_label: String,
    /// “下载”按钮文案。
    pub download_label: String,
    /// “打开所在目录”按钮文案。
    pub open_folder_label: String,
    /// 按钮下方的结果 / 进度提示与是否警示。
    pub notice: Option<(String, bool)>,
}

/// 生成更新分组说明区的文案。
///
/// # 参数
/// - `locale`：界面语言。
/// - `state`：当前界面状态。
///
/// ```ignore
/// let panel = update_panel("zh-CN", &UpdateUiState::Idle);
/// ```
pub fn update_panel(locale: &str, state: &UpdateUiState) -> UpdatePanel {
    let i18n = i18n_for(locale);
    let notice = match state {
        UpdateUiState::Idle => None,
        UpdateUiState::Running => Some((i18n.tr("update-check-running"), false)),
        UpdateUiState::Downloading => Some((i18n.tr("update-download-running"), false)),
        UpdateUiState::Available { text, failed, .. } => Some((text.clone(), *failed)),
        UpdateUiState::Downloaded { text, .. } => Some((text.clone(), false)),
        UpdateUiState::Done { text, failed } => Some((text.clone(), *failed)),
    };
    UpdatePanel {
        title: i18n.tr("update-check-title"),
        current_line: i18n.tr_with("update-check-current", &Args::new().arg(1, APP_VERSION)),
        button_label: i18n.tr("update-check-button"),
        download_label: i18n.tr("update-download-button"),
        open_folder_label: i18n.tr("update-open-folder-button"),
        notice,
    }
}

/// 把检查结果转成界面状态（本地化文案）。
///
/// # 参数
/// - `outcome`：检查结果。
/// - `locale`：界面语言。
pub fn update_outcome_state(outcome: &UpdateCheckOutcome, locale: &str) -> UpdateUiState {
    let i18n = i18n_for(locale);
    let (text, failed) = match outcome {
        UpdateCheckOutcome::UpToDate => (
            i18n.tr_with("update-check-latest", &Args::new().arg(1, APP_VERSION)),
            false,
        ),
        UpdateCheckOutcome::Available(info) => {
            return UpdateUiState::Available {
                text: available_text(info, i18n),
                info: info.clone(),
                failed: false,
            };
        }
        UpdateCheckOutcome::Config(text) => (text.clone(), true),
        UpdateCheckOutcome::FetchFailed(reason) => (
            i18n.tr_with("update-check-failed", &Args::new().arg(1, reason.clone())),
            true,
        ),
        UpdateCheckOutcome::BadManifest => (i18n.tr("update-check-bad-manifest"), true),
    };
    UpdateUiState::Done { text, failed }
}

/// 取更新说明的第一行并截断，用于单行提示。
fn notes_preview(notes: &str) -> String {
    let line = notes.lines().map(str::trim).find(|l| !l.is_empty());
    let line = line.unwrap_or_default();
    if line.chars().count() > NOTES_PREVIEW_CHARS {
        let head: String = line.chars().take(NOTES_PREVIEW_CHARS).collect();
        format!("{head}…")
    } else {
        line.to_string()
    }
}

/// “发现新版本”的提示文案：版本号，有说明时附带说明摘要。
fn available_text(info: &UpdateInfo, i18n: &snow_i18n::I18n) -> String {
    let head = i18n.tr_with(
        "update-check-available",
        &Args::new().arg(1, info.version.clone()),
    );
    let notes = notes_preview(&info.notes);
    if notes.is_empty() {
        head
    } else {
        let tail = i18n.tr_with("update-check-notes", &Args::new().arg(1, notes));
        format!("{head} {tail}")
    }
}

/// 比对清单文本与当前版本，得到检查结果。
///
/// # 参数
/// - `current`：当前版本号。
/// - `manifest_text`：下载到的清单。
pub fn evaluate_manifest(current: &str, manifest_text: &str) -> UpdateCheckOutcome {
    match check_manifest(current, manifest_text) {
        Ok(UpdateStatus::UpToDate) => UpdateCheckOutcome::UpToDate,
        Ok(UpdateStatus::Available(m)) => UpdateCheckOutcome::Available(UpdateInfo {
            version: m.version,
            url: m.url,
            notes: m.notes,
            sha256: m.sha256,
        }),
        Err(
            ManifestError::TooLarge
            | ManifestError::NotJson
            | ManifestError::MissingVersion
            | ManifestError::BadVersion(_)
            | ManifestError::BadSha256(_),
        ) => UpdateCheckOutcome::BadManifest,
    }
}

/// 用 curl 拉取清单文本（限大小、限时；写到临时文件后读回并删除）。
///
/// # 参数
/// - `url`：已校验的清单地址。
///
/// # 返回
/// 清单文本；curl 失败或读文件失败返回原因。
pub fn fetch_manifest(url: &str) -> Result<String, String> {
    let part: PathBuf = std::env::temp_dir().join(format!(
        "cisox-update-{}-{}.json",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos())
    ));
    let output = crate::ocr_download::curl_command(url, &part)
        .args(["--max-time", MANIFEST_TIMEOUT_SECS])
        .args(["--max-filesize", &MAX_MANIFEST_BYTES.to_string()])
        .stderr(std::process::Stdio::piped())
        .output()
        .map_err(|e| e.to_string());
    let result = output.and_then(|out| {
        if out.status.success() {
            std::fs::read(&part)
                .map(|bytes| String::from_utf8_lossy(&bytes).into_owned())
                .map_err(|e| e.to_string())
        } else {
            Err(String::from_utf8_lossy(&out.stderr).trim().to_string())
        }
    });
    let _ = std::fs::remove_file(&part);
    result
}

/// 完整的检查更新流程（会阻塞，须在后台线程调用）。
///
/// # 参数
/// - `url`：已校验的清单地址。
/// - `current`：当前版本号。
pub fn run_update_check(url: &str, current: &str) -> UpdateCheckOutcome {
    match fetch_manifest(url) {
        Ok(text) => evaluate_manifest(current, &text),
        Err(reason) => UpdateCheckOutcome::FetchFailed(reason),
    }
}

/// 把文本规整成可做文件 / 目录名的片段（只留字母数字与 `.-_`，其余换成 `_`）。
fn sanitize_segment(text: &str) -> String {
    let cleaned: String = text
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_') {
                c
            } else {
                '_'
            }
        })
        .collect();
    cleaned.trim_matches('.').to_string()
}

/// 更新包的目标路径：`<数据根>/updates/<版本>/<地址里的文件名>`。
///
/// # 参数
/// - `data_root`：应用数据根目录。
/// - `info`：可用更新。
///
/// ```ignore
/// let p = update_package_path(root, &info); // …/updates/1.2.0/setup.exe
/// ```
pub fn update_package_path(data_root: &Path, info: &UpdateInfo) -> PathBuf {
    let name = info
        .url
        .split(['?', '#'])
        .next()
        .and_then(|u| u.rsplit('/').next())
        .map(sanitize_segment)
        .filter(|n| !n.is_empty())
        .unwrap_or_else(|| FALLBACK_UPDATE_FILE.to_string());
    let version = Some(sanitize_segment(&info.version))
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| FALLBACK_VERSION_DIR.to_string());
    data_root.join(UPDATES_DIR).join(version).join(name)
}

/// 下载更新包到应用数据目录并校验（会阻塞，须在后台线程调用；不安装）。
///
/// 清单给了 `sha256` 就必须一致，不一致会删除已下载文件；没给则放行并在结果里标记未校验。
///
/// # 参数
/// - `info`：可用更新。
/// - `data_root`：应用数据根目录。
///
/// # 返回
/// 下载结果；落盘路径见 [`update_package_path`]。
pub fn download_update(info: &UpdateInfo, data_root: &Path) -> UpdateDownloadOutcome {
    if info.url.trim().is_empty() {
        return UpdateDownloadOutcome::NoUrl;
    }
    let dest = update_package_path(data_root, info);
    match fetch_package(info, &dest) {
        Ok(verified) => UpdateDownloadOutcome::Done {
            path: dest,
            verified,
        },
        Err(e) => UpdateDownloadOutcome::Failed(e),
    }
}

/// 下载到 `.part`、校验、改名为 `dest`；返回是否做过哈希校验。
fn fetch_package(info: &UpdateInfo, dest: &Path) -> Result<bool, FetchError> {
    let dir = dest.parent().unwrap_or(Path::new("."));
    std::fs::create_dir_all(dir).map_err(|_| FetchError::CreateDir(dir.display().to_string()))?;
    let mut part = dest.as_os_str().to_owned();
    part.push(".part");
    let part = PathBuf::from(part);
    // 旧的残留可能已是完整文件，续传会被服务器拒绝，所以每次从头下
    let _ = std::fs::remove_file(&part);
    let output = curl_command(&info.url, &part)
        .stderr(std::process::Stdio::piped())
        .output()
        .map_err(|e| FetchError::RunTool {
            tool: crate::ocr_download::TOOL_CURL,
            detail: e.to_string(),
        })?;
    // 个别情况下 curl 报错却返回 0（如 file:// 源不存在），所以还要确认确实产出了文件
    if !output.status.success() || !part.is_file() {
        let _ = std::fs::remove_file(&part);
        return Err(FetchError::DownloadFailed {
            url: info.url.clone(),
            detail: String::from_utf8_lossy(&output.stderr).trim().to_string(),
        });
    }
    let verified = !info.sha256.is_empty();
    if verified {
        let actual = sha256_file(&part).inspect_err(|_| {
            let _ = std::fs::remove_file(&part);
        })?;
        if actual != info.sha256 {
            let _ = std::fs::remove_file(&part);
            return Err(FetchError::HashMismatch {
                name: dest
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_default(),
                expected: info.sha256.clone(),
                actual,
            });
        }
    }
    let _ = std::fs::remove_file(dest);
    std::fs::rename(&part, dest).map_err(|_| FetchError::Rename(dest.display().to_string()))?;
    Ok(verified)
}

/// 把下载结果转成界面状态（本地化文案）。
///
/// # 参数
/// - `info`：被下载的更新（失败时回到“可再次下载”的状态）。
/// - `outcome`：下载结果。
/// - `locale`：界面语言。
pub fn download_outcome_state(
    info: &UpdateInfo,
    outcome: &UpdateDownloadOutcome,
    locale: &str,
) -> UpdateUiState {
    let i18n = i18n_for(locale);
    let failed = |text: String| UpdateUiState::Available {
        info: info.clone(),
        text,
        failed: true,
    };
    match outcome {
        UpdateDownloadOutcome::Done { path, verified } => {
            let id = if *verified {
                "update-downloaded"
            } else {
                "update-downloaded-unverified"
            };
            UpdateUiState::Downloaded {
                dir: path.parent().map(Path::to_path_buf).unwrap_or_default(),
                text: i18n.tr_with(id, &Args::new().arg(1, path.display())),
            }
        }
        UpdateDownloadOutcome::NoUrl => failed(i18n.tr("update-download-no-url")),
        UpdateDownloadOutcome::Failed(e) => failed(e.message(i18n)),
    }
}

/// 按配置同步 curl 下载的代理参数（`network/proxy`）。
///
/// # 参数
/// - `document`：配置文档。
pub fn apply_proxy(document: &ConfigDocument) {
    let setting = document.value(KEY_PROXY);
    let args = snow_net::curl_proxy_args(
        setting.as_str().unwrap_or_default(),
        snow_net::system_proxy_from_env().as_deref(),
    );
    tracing::info!(enabled = !args.is_empty(), "下载代理已同步");
    crate::ocr_download::set_curl_proxy_args(args);
}

/// 检查更新的目标：取配置里的清单地址，失败时给出已本地化的提示。
///
/// # 参数
/// - `document`：配置文档。
/// - `locale`：界面语言（如 `zh-CN`）。
///
/// # 返回
/// 清单地址；未配置或地址不合法时返回可直接展示的文案。
///
/// ```ignore
/// let msg = update_target(&doc, "zh-CN").unwrap_err(); // “未配置更新地址”
/// ```
pub fn update_target(document: &ConfigDocument, locale: &str) -> Result<String, String> {
    let raw = document.value(KEY_UPDATE_MANIFEST_URL);
    resolve_manifest_url(raw.as_str().unwrap_or_default()).map_err(|e| {
        let i18n = i18n_for(locale);
        match e {
            UpdateConfigError::NotConfigured => i18n.tr("update-check-not-configured"),
            UpdateConfigError::InvalidUrl(url) => {
                i18n.tr_with("update-check-invalid-url", &Args::new().arg(1, url))
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// 默认（空）地址：两种语言都提示未配置。
    #[test]
    fn unconfigured_is_localized() {
        let doc = ConfigDocument::from_bytes(None);
        assert_eq!(update_target(&doc, "zh-CN").unwrap_err(), "未配置更新地址");
        assert_eq!(
            update_target(&doc, "en-US").unwrap_err(),
            "No update address is configured."
        );
    }

    /// 配置合法地址后返回该地址；非法协议提示无效。
    #[test]
    fn configured_url_is_returned() {
        let mut doc = ConfigDocument::from_bytes(None);
        doc.set_value(KEY_UPDATE_MANIFEST_URL, json!("https://example.com/m.json"))
            .unwrap();
        assert_eq!(
            update_target(&doc, "en-US").unwrap(),
            "https://example.com/m.json"
        );
        doc.set_value(KEY_UPDATE_MANIFEST_URL, json!("ftp://x"))
            .unwrap();
        assert!(
            update_target(&doc, "en-US")
                .unwrap_err()
                .contains("ftp://x")
        );
    }

    /// 测试用的更新信息（无说明、无校验值）。
    fn info(version: &str, url: &str) -> UpdateInfo {
        UpdateInfo {
            version: version.into(),
            url: url.into(),
            notes: String::new(),
            sha256: String::new(),
        }
    }

    /// 清单比对：有新版本、持平、非法清单。
    #[test]
    fn evaluates_manifest() {
        assert_eq!(
            evaluate_manifest("0.1.0", r#"{"version":"0.2.0","url":"https://a.b"}"#),
            UpdateCheckOutcome::Available(info("0.2.0", "https://a.b"))
        );
        assert_eq!(
            evaluate_manifest("0.2.0", r#"{"version":"0.2.0"}"#),
            UpdateCheckOutcome::UpToDate
        );
        assert_eq!(
            evaluate_manifest("0.1.0", "<html>"),
            UpdateCheckOutcome::BadManifest
        );
        assert_eq!(
            evaluate_manifest("0.1.0", r#"{"version":"x"}"#),
            UpdateCheckOutcome::BadManifest
        );
    }

    /// 结果文案：两种语言都有，失败类带警示标记，新版本带版本号。
    #[test]
    fn outcome_texts_are_localized() {
        let up = update_outcome_state(&UpdateCheckOutcome::UpToDate, "zh-CN");
        assert!(
            matches!(&up, UpdateUiState::Done { text, failed: false } if text.contains(APP_VERSION))
        );
        let new = update_outcome_state(&UpdateCheckOutcome::Available(info("9.9.9", "")), "en-US");
        assert!(
            matches!(&new, UpdateUiState::Available { text, failed: false, .. } if text.contains("9.9.9"))
        );
        let mut with_notes = info("9.9.9", "https://a.b/d");
        with_notes.notes = "\nFix crash\nsecond line".into();
        let noted = update_outcome_state(&UpdateCheckOutcome::Available(with_notes), "zh-CN");
        assert!(matches!(&noted, UpdateUiState::Available { text, .. }
                if text.contains("Fix crash") && !text.contains("second line")));
        for locale in ["zh-CN", "en-US"] {
            for outcome in [
                UpdateCheckOutcome::FetchFailed("timeout".into()),
                UpdateCheckOutcome::BadManifest,
            ] {
                assert!(matches!(
                    update_outcome_state(&outcome, locale),
                    UpdateUiState::Done { failed: true, .. }
                ));
            }
        }
        assert!(matches!(
            update_outcome_state(&UpdateCheckOutcome::FetchFailed("timeout".into()), "zh-CN"),
            UpdateUiState::Done { text, .. } if text.contains("timeout")
        ));
    }

    /// 说明区：空闲无提示，检查中有提示，结果原样带出；产品名不进文案。
    #[test]
    fn panel_follows_state() {
        let idle = update_panel("zh-CN", &UpdateUiState::Idle);
        assert!(idle.notice.is_none());
        assert!(idle.current_line.contains(APP_VERSION));
        assert_eq!(idle.button_label, "检查更新");
        assert_eq!(
            update_panel("en-US", &UpdateUiState::Idle).button_label,
            "Check for updates"
        );
        assert!(
            update_panel("en-US", &UpdateUiState::Running)
                .notice
                .is_some()
        );
        let done = UpdateUiState::Done {
            text: "x".into(),
            failed: true,
        };
        assert_eq!(
            update_panel("en-US", &done).notice,
            Some(("x".into(), true))
        );
    }

    /// 端到端（Windows，用本地 file:// 清单走真实 curl）：新版本、持平、不存在的文件失败。
    #[cfg(windows)]
    #[test]
    fn run_update_check_with_local_file() {
        let dir = std::env::temp_dir().join(format!("cisox-update-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("m.json");
        std::fs::write(
            &file,
            r#"{"version":"9.0.0","url":"https://example.com/d"}"#,
        )
        .unwrap();
        let url = format!(
            "file:///{}",
            file.to_string_lossy()
                .replace(std::path::MAIN_SEPARATOR, "/")
        );
        assert_eq!(
            run_update_check(&url, "1.0.0"),
            UpdateCheckOutcome::Available(info("9.0.0", "https://example.com/d"))
        );
        assert_eq!(
            run_update_check(&url, "9.0.0"),
            UpdateCheckOutcome::UpToDate
        );
        let missing = format!(
            "file:///{}",
            dir.join("none.json")
                .to_string_lossy()
                .replace(std::path::MAIN_SEPARATOR, "/")
        );
        assert!(matches!(
            run_update_check(&missing, "1.0.0"),
            UpdateCheckOutcome::FetchFailed(_)
        ));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 清单的 sha256 与说明带进检查结果。
    #[test]
    fn evaluate_carries_sha256_and_notes() {
        let hash = "a".repeat(64);
        let text =
            format!(r#"{{"version":"2.0.0","url":"https://a.b/x","notes":"n","sha256":"{hash}"}}"#);
        let UpdateCheckOutcome::Available(got) = evaluate_manifest("1.0.0", &text) else {
            panic!("应有新版本");
        };
        assert_eq!(
            (got.notes.as_str(), got.sha256.as_str()),
            ("n", hash.as_str())
        );
        assert_eq!(
            evaluate_manifest("1.0.0", r#"{"version":"2","sha256":"zz"}"#),
            UpdateCheckOutcome::BadManifest
        );
    }

    /// 落盘路径：取地址里的文件名（去掉查询串），特殊字符被替换，缺失时用兜底名。
    #[test]
    fn package_path_is_sanitized() {
        let root = Path::new("root");
        let p = update_package_path(root, &info("1.2.0", "https://a.b/dl/setup.exe?token=1#x"));
        assert_eq!(p, root.join("updates").join("1.2.0").join("setup.exe"));
        let weird = update_package_path(root, &info("v1/../2", "https://a.b/"));
        assert_eq!(
            weird,
            root.join("updates")
                .join("v1_.._2")
                .join("cisox-update.bin")
        );
        let name = update_package_path(root, &info("1", "https://a.b/a b*c.zip"));
        assert_eq!(name.file_name().unwrap(), "a_b_c.zip");
    }

    /// 没有下载地址时直接报 `NoUrl`，且界面回到带失败提示的 Available 状态。
    #[test]
    fn download_without_url() {
        let update = info("1.0.0", "  ");
        let outcome = download_update(&update, Path::new("unused"));
        assert_eq!(outcome, UpdateDownloadOutcome::NoUrl);
        for locale in ["zh-CN", "en-US"] {
            assert!(matches!(
                download_outcome_state(&update, &outcome, locale),
                UpdateUiState::Available { failed: true, .. }
            ));
        }
    }

    /// 下载结果文案：成功带路径与所在目录，未校验有单独提示，两种语言都有。
    #[test]
    fn download_outcome_texts() {
        let update = info("1.0.0", "https://a.b/x.exe");
        let path = PathBuf::from("C:/data/updates/1.0.0/x.exe");
        let done = |verified| UpdateDownloadOutcome::Done {
            path: path.clone(),
            verified,
        };
        let UpdateUiState::Downloaded { dir, text } =
            download_outcome_state(&update, &done(true), "zh-CN")
        else {
            panic!("应为已下载");
        };
        assert_eq!(dir, PathBuf::from("C:/data/updates/1.0.0"));
        assert!(text.contains("已下载") && text.contains("x.exe"));
        let UpdateUiState::Downloaded { text: loose, .. } =
            download_outcome_state(&update, &done(false), "en-US")
        else {
            panic!("应为已下载");
        };
        assert!(loose.contains("not verified"));
        let bad = UpdateDownloadOutcome::Failed(FetchError::Cancelled);
        assert!(matches!(
            download_outcome_state(&update, &bad, "en-US"),
            UpdateUiState::Available { failed: true, .. }
        ));
    }

    /// 说明区按钮文案与下载中提示。
    #[test]
    fn panel_has_download_labels() {
        let zh = update_panel("zh-CN", &UpdateUiState::Downloading);
        assert_eq!(
            (zh.download_label.as_str(), zh.open_folder_label.as_str()),
            ("下载", "打开所在目录")
        );
        assert!(zh.notice.is_some());
        assert_eq!(
            update_panel("en-US", &UpdateUiState::Idle).download_label,
            "Download"
        );
    }

    /// 端到端（Windows，file:// 源走真实 curl 与 certutil）：校验通过落盘、哈希不符删除并报错、无校验值放行。
    #[cfg(windows)]
    #[test]
    fn download_update_verifies_hash() {
        let dir = std::env::temp_dir().join(format!("cisox-update-dl-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let src = dir.join("pkg.bin");
        std::fs::write(&src, b"hello cisox").unwrap();
        let url = format!(
            "file:///{}",
            src.to_string_lossy()
                .replace(std::path::MAIN_SEPARATOR, "/")
        );
        let hash = sha256_file(&src).unwrap();
        let root = dir.join("data");
        let mut update = info("3.0.0", &url);
        update.sha256 = hash.clone();
        let UpdateDownloadOutcome::Done { path, verified } = download_update(&update, &root) else {
            panic!("应下载成功");
        };
        assert!(verified && path.is_file());
        assert_eq!(std::fs::read(&path).unwrap(), b"hello cisox");
        assert!(path.starts_with(root.join("updates").join("3.0.0")));
        // 哈希不符：报错且不留成品与残留
        update.version = "3.0.1".into();
        update.sha256 = "0".repeat(64);
        let outcome = download_update(&update, &root);
        assert!(matches!(
            outcome,
            UpdateDownloadOutcome::Failed(FetchError::HashMismatch { .. })
        ));
        let bad_dest = update_package_path(&root, &update);
        assert!(!bad_dest.exists() && !bad_dest.with_extension("bin.part").exists());
        // 没有校验值：放行，标记未校验
        update.version = "3.0.2".into();
        update.sha256.clear();
        assert!(matches!(
            download_update(&update, &root),
            UpdateDownloadOutcome::Done {
                verified: false,
                ..
            }
        ));
        // 源文件不存在：下载失败
        update.url = format!(
            "file:///{}/none.bin",
            dir.to_string_lossy()
                .replace(std::path::MAIN_SEPARATOR, "/")
        );
        assert!(matches!(
            download_update(&update, &root),
            UpdateDownloadOutcome::Failed(FetchError::DownloadFailed { .. })
        ));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
