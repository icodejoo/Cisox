//! 网络与更新设置的落地：`network/proxy` 变成 curl 代理参数，`updates/manifest_url` 变成检查更新的目标或“未配置”提示，
//! 以及“检查更新”的下载、比对与结果文案。计算部分是纯逻辑，可离屏测试。

use crate::ocr_backend::i18n_for;
use snow_config::document::ConfigDocument;
use snow_config::extensions::KEY_UPDATE_MANIFEST_URL;
use snow_i18n::Args;
use snow_update::{
    MAX_MANIFEST_BYTES, ManifestError, UpdateConfigError, UpdateStatus, check_manifest,
    resolve_manifest_url,
};
use std::path::PathBuf;

/// 代理配置键。
pub const KEY_PROXY: &str = "network/proxy";
/// 当前应用版本（来自 Cargo 包版本）。
pub const APP_VERSION: &str = env!("CARGO_PKG_VERSION");
/// 设置页里“更新”分组的 id。
pub const UPDATES_GROUP_ID: &str = "updates";
/// 拉取清单的总超时（秒）。
const MANIFEST_TIMEOUT_SECS: &str = "30";

/// “检查更新”在设置页里的界面状态。
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum UpdateUiState {
    /// 尚未检查。
    #[default]
    Idle,
    /// 检查进行中。
    Running,
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
    /// 有新版本：`(版本号, 下载地址)`，地址可空。
    Available(String, String),
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
        UpdateUiState::Done { text, failed } => Some((text.clone(), *failed)),
    };
    UpdatePanel {
        title: i18n.tr("update-check-title"),
        current_line: i18n.tr_with("update-check-current", &Args::new().arg(1, APP_VERSION)),
        button_label: i18n.tr("update-check-button"),
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
        UpdateCheckOutcome::Available(version, url) if url.is_empty() => (
            i18n.tr_with(
                "update-check-available",
                &Args::new().arg(1, version.clone()),
            ),
            false,
        ),
        UpdateCheckOutcome::Available(version, url) => (
            i18n.tr_with(
                "update-check-available-url",
                &Args::new().arg(1, version.clone()).arg(2, url.clone()),
            ),
            false,
        ),
        UpdateCheckOutcome::Config(text) => (text.clone(), true),
        UpdateCheckOutcome::FetchFailed(reason) => (
            i18n.tr_with("update-check-failed", &Args::new().arg(1, reason.clone())),
            true,
        ),
        UpdateCheckOutcome::BadManifest => (i18n.tr("update-check-bad-manifest"), true),
    };
    UpdateUiState::Done { text, failed }
}

/// 比对清单文本与当前版本，得到检查结果。
///
/// # 参数
/// - `current`：当前版本号。
/// - `manifest_text`：下载到的清单。
pub fn evaluate_manifest(current: &str, manifest_text: &str) -> UpdateCheckOutcome {
    match check_manifest(current, manifest_text) {
        Ok(UpdateStatus::UpToDate) => UpdateCheckOutcome::UpToDate,
        Ok(UpdateStatus::Available(m)) => UpdateCheckOutcome::Available(m.version, m.url),
        Err(ManifestError::TooLarge | ManifestError::NotJson | ManifestError::MissingVersion)
        | Err(ManifestError::BadVersion(_)) => UpdateCheckOutcome::BadManifest,
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

    /// 清单比对：有新版本、持平、非法清单。
    #[test]
    fn evaluates_manifest() {
        assert_eq!(
            evaluate_manifest("0.1.0", r#"{"version":"0.2.0","url":"https://a.b"}"#),
            UpdateCheckOutcome::Available("0.2.0".into(), "https://a.b".into())
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
        let new = update_outcome_state(
            &UpdateCheckOutcome::Available("9.9.9".into(), String::new()),
            "en-US",
        );
        assert!(
            matches!(&new, UpdateUiState::Done { text, failed: false } if text.contains("9.9.9"))
        );
        let with_url = update_outcome_state(
            &UpdateCheckOutcome::Available("9.9.9".into(), "https://a.b/d".into()),
            "zh-CN",
        );
        assert!(
            matches!(&with_url, UpdateUiState::Done { text, .. } if text.contains("https://a.b/d"))
        );
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
            UpdateCheckOutcome::Available("9.0.0".into(), "https://example.com/d".into())
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
}
