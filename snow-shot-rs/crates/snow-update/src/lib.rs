//! 更新协议与 helper 进程驱动（T4：验收前整体禁用）。
//!
//! 所属阶段：P7。当前落地两块纯逻辑：“更新清单地址”的解析（不硬编码任何端点，未配置时明确报错），
//! 以及清单 JSON 的解析与版本比较（只检查、不下载安装）。
//!
//! 最小清单格式：`{"version": "1.2.3", "url": "https://…", "notes": "更新说明"}`，
//! `version` 必填，`url` 与 `notes` 可省略。

use serde_json::Value;
use snow_net::{UrlError, validate_url};
use std::cmp::Ordering;

/// 本 crate 的阶段标记，用于骨架连通性测试。
pub const PHASE: &str = "P7";

/// 清单文本的最大字节数（超出视为非法，避免误拉到大文件）。
pub const MAX_MANIFEST_BYTES: usize = 64 * 1024;
/// 版本号里分隔数字段的字符。
const VERSION_SEPARATOR: char = '.';
/// 版本号里预发布后缀的起始字符。
const PRERELEASE_MARK: char = '-';
/// 版本号里构建元数据的起始字符（比较时忽略）。
const BUILD_MARK: char = '+';
/// UTF-8 字节序标记（有的编辑器会给 JSON 文件加上）。
const BOM: char = '\u{feff}';

/// 检查更新前的配置错误。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UpdateConfigError {
    /// 没有配置更新清单地址。
    NotConfigured,
    /// 地址不合法（携带原值）。
    InvalidUrl(String),
}

/// 更新清单。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UpdateManifest {
    /// 最新版本号（如 `1.2.3`）。
    pub version: String,
    /// 下载页或安装包地址（可空）。
    pub url: String,
    /// 更新说明（可空）。
    pub notes: String,
}

/// 清单解析失败的原因。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ManifestError {
    /// 内容过大。
    TooLarge,
    /// 不是合法 JSON 对象。
    NotJson,
    /// 缺少 `version` 字段或它不是非空字符串。
    MissingVersion,
    /// 版本号无法解析（携带原值）。
    BadVersion(String),
}

/// 当前版本与清单版本的比较结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UpdateStatus {
    /// 已是最新（含本地比清单更新的情况）。
    UpToDate,
    /// 有新版本。
    Available(UpdateManifest),
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

/// 解析更新清单 JSON。
///
/// # 参数
/// - `text`：清单文本。
///
/// # 返回
/// 清单；超长、非 JSON 对象、缺 `version` 或版本号非法时返回对应错误。
///
/// ```ignore
/// let m = parse_manifest(r#"{"version":"1.2.0","url":"https://a.b"}"#).unwrap();
/// assert_eq!(m.version, "1.2.0");
/// ```
pub fn parse_manifest(text: &str) -> Result<UpdateManifest, ManifestError> {
    if text.len() > MAX_MANIFEST_BYTES {
        return Err(ManifestError::TooLarge);
    }
    let value: Value =
        serde_json::from_str(text.trim_start_matches(BOM)).map_err(|_| ManifestError::NotJson)?;
    let object = value.as_object().ok_or(ManifestError::NotJson)?;
    let field = |name: &str| {
        object
            .get(name)
            .and_then(Value::as_str)
            .map(|s| s.trim().to_string())
            .unwrap_or_default()
    };
    let version = field("version");
    if version.is_empty() {
        return Err(ManifestError::MissingVersion);
    }
    if parse_version(&version).is_none() {
        return Err(ManifestError::BadVersion(version));
    }
    Ok(UpdateManifest {
        version,
        url: field("url"),
        notes: field("notes"),
    })
}

/// 把版本号拆成（数字段，是否带预发布后缀）；允许前缀 `v`，忽略 `+构建` 部分。
fn parse_version(raw: &str) -> Option<(Vec<u64>, bool)> {
    let raw = raw.trim();
    let raw = raw.strip_prefix(['v', 'V']).unwrap_or(raw);
    let raw = raw.split(BUILD_MARK).next().unwrap_or_default();
    let (core, pre) = match raw.split_once(PRERELEASE_MARK) {
        Some((core, pre)) => (core, !pre.is_empty()),
        None => (raw, false),
    };
    let nums = core
        .split(VERSION_SEPARATOR)
        .map(|part| part.parse::<u64>().ok())
        .collect::<Option<Vec<_>>>()?;
    (!nums.is_empty()).then_some((nums, pre))
}

/// 比较两个版本号：按数字段逐段比较（缺段补 0），数字段相同时带预发布后缀的更小。
///
/// # 参数
/// - `a` / `b`：版本号文本。
///
/// # 返回
/// `a` 相对 `b` 的顺序；任一无法解析返回 `None`。
///
/// ```ignore
/// assert_eq!(compare_versions("1.2.0", "1.10"), Some(std::cmp::Ordering::Less));
/// ```
pub fn compare_versions(a: &str, b: &str) -> Option<Ordering> {
    let (na, pa) = parse_version(a)?;
    let (nb, pb) = parse_version(b)?;
    for i in 0..na.len().max(nb.len()) {
        let order = na.get(i).unwrap_or(&0).cmp(nb.get(i).unwrap_or(&0));
        if order != Ordering::Equal {
            return Some(order);
        }
    }
    // 数字段相同：正式版大于预发布版
    Some(pb.cmp(&pa))
}

/// 用清单文本判断当前版本是否需要更新。
///
/// # 参数
/// - `current`：当前版本号。
/// - `manifest_text`：下载到的清单文本。
///
/// # 返回
/// 有新版本时为 `Available`，否则 `UpToDate`；清单非法或当前版本无法解析时返回错误。
///
/// ```ignore
/// let status = check_manifest("1.0.0", r#"{"version":"1.1.0"}"#).unwrap();
/// ```
pub fn check_manifest(current: &str, manifest_text: &str) -> Result<UpdateStatus, ManifestError> {
    let manifest = parse_manifest(manifest_text)?;
    match compare_versions(&manifest.version, current) {
        Some(Ordering::Greater) => Ok(UpdateStatus::Available(manifest)),
        Some(_) => Ok(UpdateStatus::UpToDate),
        None => Err(ManifestError::BadVersion(current.to_string())),
    }
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

    /// 版本比较：数字段按数值比较（1.10 > 1.9），缺段补 0，`v` 前缀与构建元数据被忽略。
    #[test]
    fn compares_versions_numerically() {
        use std::cmp::Ordering::*;
        assert_eq!(compare_versions("1.10.0", "1.9.9"), Some(Greater));
        assert_eq!(compare_versions("1.2", "1.2.0"), Some(Equal));
        assert_eq!(compare_versions("v2.0.0+build5", "2.0.0"), Some(Equal));
        assert_eq!(compare_versions("0.1.0", "0.1.1"), Some(Less));
        assert_eq!(compare_versions("1.0.0-beta", "1.0.0"), Some(Less));
        assert_eq!(compare_versions("1.0.0", "1.0.0-rc1"), Some(Greater));
        assert_eq!(compare_versions("abc", "1.0.0"), None);
        assert_eq!(compare_versions("1..0", "1.0.0"), None);
    }

    /// 清单解析：最小字段、可选字段缺省为空、BOM 容忍。
    #[test]
    fn parses_manifest() {
        let m =
            parse_manifest(r#"{"version":" 1.2.3 ","url":"https://a.b/d","notes":"fix"}"#).unwrap();
        assert_eq!(
            m,
            UpdateManifest {
                version: "1.2.3".into(),
                url: "https://a.b/d".into(),
                notes: "fix".into()
            }
        );
        let m = parse_manifest("\u{feff}{\"version\":\"2.0\"}").unwrap();
        assert!(m.url.is_empty() && m.notes.is_empty());
    }

    /// 清单非法：非 JSON、非对象、缺版本、版本非法、过大。
    #[test]
    fn rejects_bad_manifest() {
        assert_eq!(parse_manifest("<html>"), Err(ManifestError::NotJson));
        assert_eq!(parse_manifest("[1]"), Err(ManifestError::NotJson));
        assert_eq!(parse_manifest("{}"), Err(ManifestError::MissingVersion));
        assert_eq!(
            parse_manifest(r#"{"version":3}"#),
            Err(ManifestError::MissingVersion)
        );
        assert_eq!(
            parse_manifest(r#"{"version":"x.y"}"#),
            Err(ManifestError::BadVersion("x.y".into()))
        );
        let big = format!(
            r#"{{"version":"1","notes":"{}"}}"#,
            "a".repeat(MAX_MANIFEST_BYTES)
        );
        assert_eq!(parse_manifest(&big), Err(ManifestError::TooLarge));
    }

    /// 检查结果：有新版本、持平、本地更新、当前版本非法。
    #[test]
    fn checks_manifest_against_current() {
        let text = r#"{"version":"1.1.0","url":"https://a.b"}"#;
        assert!(
            matches!(check_manifest("1.0.0", text), Ok(UpdateStatus::Available(m)) if m.version == "1.1.0")
        );
        assert_eq!(check_manifest("1.1.0", text), Ok(UpdateStatus::UpToDate));
        assert_eq!(check_manifest("2.0.0", text), Ok(UpdateStatus::UpToDate));
        assert_eq!(
            check_manifest("oops", text),
            Err(ManifestError::BadVersion("oops".into()))
        );
    }
}
