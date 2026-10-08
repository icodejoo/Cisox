//! 网络基础：URL 校验与下载进程（curl）的代理参数构造，均为纯函数，不引入 HTTP 客户端。
//!
//! 所属阶段：P5。真正的下载由系统自带的 `curl.exe` 完成，这里只负责算参数。

/// 本 crate 的阶段标记，用于骨架连通性测试。
pub const PHASE: &str = "P5";

/// 代理配置值：不使用代理。
pub const PROXY_NONE: &str = "none";
/// 代理配置值：使用系统代理（环境变量里的代理地址）。
pub const PROXY_SYSTEM: &str = "system";
/// 允许的下载地址协议前缀。
const ALLOWED_SCHEMES: [&str; 3] = ["https://", "http://", "file://"];

/// 地址校验失败的原因。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UrlError {
    /// 地址为空。
    Empty,
    /// 协议不在允许范围内（只认 https / http / file）。
    UnsupportedScheme(String),
}

/// 校验并规整下载地址（去首尾空白）。
///
/// # 参数
/// - `raw`：用户填写的地址。
///
/// # 返回
/// 规整后的地址；为空或协议不受支持时返回对应错误。
///
/// ```ignore
/// assert_eq!(validate_url(" https://a.b/m.json ").unwrap(), "https://a.b/m.json");
/// ```
pub fn validate_url(raw: &str) -> Result<String, UrlError> {
    let url = raw.trim();
    if url.is_empty() {
        return Err(UrlError::Empty);
    }
    let lower = url.to_ascii_lowercase();
    if ALLOWED_SCHEMES.iter().any(|s| lower.starts_with(s)) {
        Ok(url.to_string())
    } else {
        Err(UrlError::UnsupportedScheme(url.to_string()))
    }
}

/// 允许的代理地址协议（不含 `://`）。
pub const PROXY_SCHEMES: [&str; 4] = ["http", "https", "socks5", "socks5h"];
/// 协议与地址之间的分隔。
const SCHEME_SEPARATOR: &str = "://";

/// 代理配置校验失败的原因。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProxyError {
    /// 不是 `协议://地址` 形式，或协议不在 [`PROXY_SCHEMES`] 内（携带原值）。
    UnsupportedScheme(String),
    /// 缺少主机部分，或含空白 / 控制字符（携带原值）。
    InvalidAddress(String),
}

/// 校验并规整 `network/proxy` 配置值。
///
/// # 参数
/// - `raw`：用户填写的值：`none` / `system` / 空，或 `http|https|socks5|socks5h://主机[:端口]`。
///
/// # 返回
/// 规整后的值：空与 `none`（大小写不敏感）得 `none`，`system` 得 `system`，其余为去空白的地址原文；
/// 协议不受支持或地址不合法时返回对应错误。
///
/// ```ignore
/// assert_eq!(validate_proxy("  ").unwrap(), "none");
/// assert!(validate_proxy("ftp://p:1").is_err());
/// ```
pub fn validate_proxy(raw: &str) -> Result<String, ProxyError> {
    let text = raw.trim();
    if text.is_empty() || text.eq_ignore_ascii_case(PROXY_NONE) {
        return Ok(PROXY_NONE.to_string());
    }
    if text.eq_ignore_ascii_case(PROXY_SYSTEM) {
        return Ok(PROXY_SYSTEM.to_string());
    }
    let Some((scheme, rest)) = text.split_once(SCHEME_SEPARATOR) else {
        return Err(ProxyError::UnsupportedScheme(text.to_string()));
    };
    if !PROXY_SCHEMES.iter().any(|s| s.eq_ignore_ascii_case(scheme)) {
        return Err(ProxyError::UnsupportedScheme(text.to_string()));
    }
    let host_part = rest.split('/').next().unwrap_or_default();
    let host = host_part.rsplit('@').next().unwrap_or_default();
    let bad_char = text.chars().any(|c| c.is_whitespace() || c.is_control());
    if host.is_empty() || host.starts_with(':') || bad_char {
        return Err(ProxyError::InvalidAddress(text.to_string()));
    }
    Ok(text.to_string())
}

/// 计算追加给 curl 的代理参数。
///
/// # 参数
/// - `setting`：`network/proxy` 配置值：`none` / 空 = 不加；`system` = 用 `system_proxy`；
///   其他值须通过 [`validate_proxy`]，合法才按代理地址使用，非法一律不加（不把垃圾传给 curl）。
/// - `system_proxy`：系统代理地址（通常取自环境变量），`system` 模式下才用。
///
/// # 返回
/// 要追加的参数列表（`["--proxy", 地址]`）；不需要代理时为空。
///
/// ```ignore
/// assert_eq!(curl_proxy_args("system", Some("http://127.0.0.1:7890")), ["--proxy", "http://127.0.0.1:7890"]);
/// ```
pub fn curl_proxy_args(setting: &str, system_proxy: Option<&str>) -> Vec<String> {
    let address = match validate_proxy(setting) {
        Ok(v) if v == PROXY_NONE => None,
        Ok(v) if v == PROXY_SYSTEM => system_proxy
            .map(str::trim)
            .filter(|p| !p.is_empty())
            .map(str::to_string),
        Ok(custom) => Some(custom),
        Err(_) => None,
    };
    match address {
        Some(a) => vec!["--proxy".to_string(), a],
        None => Vec::new(),
    }
}

/// 读取系统代理地址：依次看 `HTTPS_PROXY`、`https_proxy`、`ALL_PROXY`、`all_proxy`、`HTTP_PROXY`、`http_proxy`。
///
/// # 返回
/// 第一个非空的值；都没有则为 `None`。
pub fn system_proxy_from_env() -> Option<String> {
    [
        "HTTPS_PROXY",
        "https_proxy",
        "ALL_PROXY",
        "all_proxy",
        "HTTP_PROXY",
        "http_proxy",
    ]
    .iter()
    .filter_map(|name| std::env::var(name).ok())
    .map(|v| v.trim().to_string())
    .find(|v| !v.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 阶段标记不应为空。
    #[test]
    fn phase_not_empty() {
        assert!(!PHASE.is_empty());
    }

    /// 空值与 none 不加代理参数。
    #[test]
    fn no_proxy_adds_nothing() {
        assert!(curl_proxy_args("", Some("http://p:1")).is_empty());
        assert!(curl_proxy_args("none", Some("http://p:1")).is_empty());
        assert!(curl_proxy_args("  ", None).is_empty());
    }

    /// system 模式用系统代理，没有则不加。
    #[test]
    fn system_mode_uses_system_proxy() {
        assert_eq!(
            curl_proxy_args("system", Some(" http://p:1 ")),
            ["--proxy", "http://p:1"]
        );
        assert!(curl_proxy_args("system", None).is_empty());
        assert!(curl_proxy_args("system", Some("  ")).is_empty());
    }

    /// 其他非空值当作代理地址。
    #[test]
    fn custom_value_is_used_directly() {
        assert_eq!(
            curl_proxy_args("socks5h://127.0.0.1:1080", None),
            ["--proxy", "socks5h://127.0.0.1:1080"]
        );
    }

    /// 代理配置校验：none / system / 空规整，四种协议通过，其余拒绝。
    #[test]
    fn proxy_validation() {
        assert_eq!(validate_proxy("").unwrap(), "none");
        assert_eq!(validate_proxy(" NONE ").unwrap(), "none");
        assert_eq!(validate_proxy("System").unwrap(), "system");
        for scheme in PROXY_SCHEMES {
            let url = format!("{scheme}://127.0.0.1:7890");
            assert_eq!(validate_proxy(&format!(" {url} ")).unwrap(), url);
        }
        assert!(validate_proxy("user:pw@h.com:8080").is_err());
        assert!(validate_proxy("http://user:pw@h.com:8080").is_ok());
        assert_eq!(
            validate_proxy("ftp://p:1"),
            Err(ProxyError::UnsupportedScheme("ftp://p:1".into()))
        );
        assert_eq!(
            validate_proxy("127.0.0.1:7890"),
            Err(ProxyError::UnsupportedScheme("127.0.0.1:7890".into()))
        );
        assert!(matches!(
            validate_proxy("http://"),
            Err(ProxyError::InvalidAddress(_))
        ));
        assert!(matches!(
            validate_proxy("http://:80"),
            Err(ProxyError::InvalidAddress(_))
        ));
        assert!(matches!(
            validate_proxy("http://a b:1"),
            Err(ProxyError::InvalidAddress(_))
        ));
    }

    /// 非法代理值不产生参数；大小写不敏感的 NONE 也不加。
    #[test]
    fn invalid_proxy_adds_nothing() {
        assert!(curl_proxy_args("ftp://p:1", None).is_empty());
        assert!(curl_proxy_args("http://", None).is_empty());
        assert!(curl_proxy_args("None", Some("http://p:1")).is_empty());
        assert_eq!(
            curl_proxy_args("HTTP://p:1", None),
            ["--proxy", "HTTP://p:1"]
        );
    }

    /// 地址校验：空、非法协议、合法协议。
    #[test]
    fn url_validation() {
        assert_eq!(validate_url("  "), Err(UrlError::Empty));
        assert_eq!(
            validate_url("ftp://a"),
            Err(UrlError::UnsupportedScheme("ftp://a".into()))
        );
        assert_eq!(validate_url(" HTTPS://a.b/x ").unwrap(), "HTTPS://a.b/x");
        assert!(validate_url("file:///C:/m.json").is_ok());
    }
}
