//! 录制进程的运行时设置：环境变量名、硬件编码模式、输出尺寸上限。只有纯逻辑，与平台和上游类型无关。

/// 环境变量：硬件编码模式（`0`/`off` 只用软编，`1`/`gpu` 自建硬件流水线，`upstream` 上游 GPU 路径）。
pub const ENV_PREFER_HARDWARE: &str = "SNOW_RECORDER_HARDWARE";
/// 环境变量：QSV async_depth（1..=8，仅自建硬件流水线）。
pub const ENV_QSV_ASYNC_DEPTH: &str = "SNOW_RECORDER_QSV_ASYNC_DEPTH";
/// 环境变量：硬编质量参数（global_quality/qp，仅自建硬件流水线）。
pub const ENV_QSV_QUALITY: &str = "SNOW_RECORDER_QSV_QUALITY";
/// 环境变量：QSV 预设（仅自建硬件流水线）。
pub const ENV_QSV_PRESET: &str = "SNOW_RECORDER_QSV_PRESET";
/// 环境变量：输出尺寸上限，形如 `1920x1080`（长边x短边，按选区方向取向）；`none` 表示不限。
pub const ENV_MAX_SIZE: &str = "SNOW_RECORDER_MAX_SIZE";
/// 硬件编码模式。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HardwareMode {
    /// 只用软件编码（上游直录会话）。
    Off,
    /// 自建 GPU 流水线（失败自动回落软编）。
    Gpu,
    /// 上游自带的 GPU 路径（对照用）。
    Upstream,
}

/// 未设置环境变量时的硬件编码模式。
pub const DEFAULT_HARDWARE_MODE: HardwareMode = HardwareMode::Off;

/// 解析 `SNOW_RECORDER_HARDWARE` 的取值。
///
/// # 参数
/// - `text`：环境变量值；`None` 表示未设置。
///
/// # 返回
/// 硬件模式：`0/off` 关闭，`1/gpu` 自建流水线，`upstream` 上游路径，其余沿用默认。
///
/// # 示例
/// ```ignore
/// assert_eq!(parse_hardware_mode(Some("1")), HardwareMode::Gpu);
/// assert_eq!(parse_hardware_mode(Some("upstream")), HardwareMode::Upstream);
/// ```
pub fn parse_hardware_mode(text: Option<&str>) -> HardwareMode {
    match text.map(|t| t.trim().to_ascii_lowercase()).as_deref() {
        Some("0" | "off" | "false") => HardwareMode::Off,
        Some("1" | "gpu" | "on" | "true") => HardwareMode::Gpu,
        Some("upstream") => HardwareMode::Upstream,
        _ => DEFAULT_HARDWARE_MODE,
    }
}

/// 产品默认输出上限的长边（1080p）。
pub const DEFAULT_MAX_LONG_SIDE: u32 = 1920;
/// 产品默认输出上限的短边（1080p）。
pub const DEFAULT_MAX_SHORT_SIDE: u32 = 1080;

/// 输出尺寸上限设置：长边、短边（与选区方向无关，使用时再取向）。
pub type SizeLimit = Option<(u32, u32)>;

/// 解析输出上限文本。
///
/// # 参数
/// - `text`：`none`/`off`/`0` 表示不限；`WxH` 给出上限；空或非法返回 `None`（沿用默认）。
///
/// # 返回
/// `Some(不限或上限)`，无法解析时 `None`。
///
/// # 示例
/// ```
/// use crate::settings::parse_size_limit;
/// assert_eq!(parse_size_limit("1280x720"), Some(Some((1280, 720))));
/// assert_eq!(parse_size_limit("none"), Some(None));
/// assert_eq!(parse_size_limit("bad"), None);
/// ```
pub fn parse_size_limit(text: &str) -> Option<SizeLimit> {
    let t = text.trim().to_ascii_lowercase();
    if matches!(t.as_str(), "none" | "off" | "0") {
        return Some(None);
    }
    let (w, h) = t.split_once('x')?;
    let (w, h) = (w.trim().parse::<u32>().ok()?, h.trim().parse::<u32>().ok()?);
    (w > 0 && h > 0).then_some(Some((w.max(h), w.min(h))))
}

/// 读取当前生效的输出上限：环境变量优先，否则产品默认 1080p。
pub fn configured_size_limit() -> SizeLimit {
    std::env::var(ENV_MAX_SIZE)
        .ok()
        .and_then(|v| parse_size_limit(&v))
        .unwrap_or(Some((DEFAULT_MAX_LONG_SIDE, DEFAULT_MAX_SHORT_SIDE)))
}

/// 把上限按选区方向取向成（最大宽，最大高）。
///
/// # 参数
/// - `limit`：长边、短边上限。
/// - `width`、`height`：选区尺寸；竖屏选区长边对应高度。
///
/// # 示例
/// ```
/// use crate::settings::oriented_limit;
/// assert_eq!(oriented_limit(Some((1920, 1080)), 2560, 1440), (Some(1920), Some(1080)));
/// assert_eq!(oriented_limit(Some((1920, 1080)), 1440, 2560), (Some(1080), Some(1920)));
/// assert_eq!(oriented_limit(None, 10, 10), (None, None));
/// ```
pub fn oriented_limit(limit: SizeLimit, width: u32, height: u32) -> (Option<u32>, Option<u32>) {
    match limit {
        None => (None, None),
        Some((long, short)) if width >= height => (Some(long), Some(short)),
        Some((long, short)) => (Some(short), Some(long)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 硬件模式解析：大小写/空白不敏感，未知值沿用默认。
    #[test]
    fn hardware_mode_parsing() {
        assert_eq!(parse_hardware_mode(Some("1")), HardwareMode::Gpu);
        assert_eq!(parse_hardware_mode(Some(" GPU ")), HardwareMode::Gpu);
        assert_eq!(parse_hardware_mode(Some("0")), HardwareMode::Off);
        assert_eq!(parse_hardware_mode(Some("upstream")), HardwareMode::Upstream);
        assert_eq!(parse_hardware_mode(Some("??")), DEFAULT_HARDWARE_MODE);
        assert_eq!(parse_hardware_mode(None), DEFAULT_HARDWARE_MODE);
    }

    /// 输出上限解析、取向与默认值。
    #[test]
    fn size_limit_parsing_and_orientation() {
        assert_eq!(parse_size_limit("1280x720"), Some(Some((1280, 720))));
        assert_eq!(parse_size_limit("720x1280"), Some(Some((1280, 720))));
        assert_eq!(parse_size_limit("none"), Some(None));
        assert_eq!(parse_size_limit("bad"), None);
        assert_eq!(parse_size_limit("0x10"), None);
        assert_eq!(oriented_limit(Some((1920, 1080)), 1440, 2560), (Some(1080), Some(1920)));
        assert_eq!(oriented_limit(None, 1, 1), (None, None));
    }
}
