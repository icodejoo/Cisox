//! 更新协议与 helper 进程驱动（T4：验收前整体禁用）。
//!
//! 所属阶段：P7。当前只落地“更新清单地址”的解析：不硬编码任何端点，未配置时明确报错。

use snow_net::{UrlError, validate_url};

/// 本 crate 的阶段标记，用于骨架连通性测试。
pub const PHASE: &str = "P7";

/// 检查更新前的配置错误。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UpdateConfigError {
    /// 没有配置更新清单地址。
    NotConfigured,
    /// 地址不合法（携带原值）。
    InvalidUrl(String),
}

/// 解析配置里的更新清单地址。
///
/// # 参数
/// - `raw`：`updates/manifest_url` 配置值。
///
/// # 返回
/// 可用于下载的地址；空串返回 [`UpdateConfigError::NotConfigured`]，协议不支持返回 `InvalidUrl`。
///
/// ```ignore
/// assert_eq!(resolve_manifest_url(""), Err(UpdateConfigError::NotConfigured));
/// ```
pub fn resolve_manifest_url(raw: &str) -> Result<String, UpdateConfigError> {
    validate_url(raw).map_err(|e| match e {
        UrlError::Empty => UpdateConfigError::NotConfigured,
        UrlError::UnsupportedScheme(url) => UpdateConfigError::InvalidUrl(url),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 阶段标记不应为空。
    #[test]
    fn phase_not_empty() {
        assert!(!PHASE.is_empty());
    }

    /// 空地址视为未配置。
    #[test]
    fn empty_is_not_configured() {
        assert_eq!(
            resolve_manifest_url(""),
            Err(UpdateConfigError::NotConfigured)
        );
        assert_eq!(
            resolve_manifest_url("   "),
            Err(UpdateConfigError::NotConfigured)
        );
    }

    /// 非法协议被拒，合法地址通过。
    #[test]
    fn validates_scheme() {
        assert_eq!(
            resolve_manifest_url("ftp://x"),
            Err(UpdateConfigError::InvalidUrl("ftp://x".into()))
        );
        assert_eq!(
            resolve_manifest_url("https://a.b/m.json").unwrap(),
            "https://a.b/m.json"
        );
    }
}
