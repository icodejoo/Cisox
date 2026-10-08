//! 网络与更新设置的落地：`network/proxy` 变成 curl 代理参数，`updates/manifest_url` 变成检查更新的目标或“未配置”提示。
//! 计算部分是纯逻辑，可离屏测试。

use crate::ocr_backend::i18n_for;
use snow_config::document::ConfigDocument;
use snow_config::extensions::KEY_UPDATE_MANIFEST_URL;
use snow_i18n::Args;
use snow_update::{UpdateConfigError, resolve_manifest_url};

/// 代理配置键。
pub const KEY_PROXY: &str = "network/proxy";

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
}
