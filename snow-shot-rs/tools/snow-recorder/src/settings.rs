//! 录制进程的运行时设置：环境变量名、硬件编码模式、输出尺寸上限。只有纯逻辑，与平台和上游类型无关。

/// 环境变量：硬件编码模式（未设置 = `auto`：Media Foundation -> 厂商硬编 -> 软编；`0`/`off` 只用软编，
/// `1`/`gpu` 自建流水线 + FFmpeg 厂商硬编，`mf` 自建流水线 + Media Foundation，`upstream` 上游 GPU 路径）。
pub const ENV_PREFER_HARDWARE: &str = "SNOW_RECORDER_HARDWARE";
/// 环境变量：QSV async_depth（1..=8，仅自建硬件流水线）。
pub const ENV_QSV_ASYNC_DEPTH: &str = "SNOW_RECORDER_QSV_ASYNC_DEPTH";
/// 环境变量：硬编质量参数（global_quality/qp，仅自建硬件流水线）。
pub const ENV_QSV_QUALITY: &str = "SNOW_RECORDER_QSV_QUALITY";
/// 环境变量：QSV 预设（仅自建硬件流水线）。
pub const ENV_QSV_PRESET: &str = "SNOW_RECORDER_QSV_PRESET";
/// 环境变量：输出尺寸上限，形如 `1920x1080`（长边x短边，按选区方向取向）；`none` 表示不限。
pub const ENV_MAX_SIZE: &str = "SNOW_RECORDER_MAX_SIZE";
/// 环境变量：硬件编码器选择（`auto` 按适配器厂商自动选；`qsv`/`nvenc`/`amf` 强制；`off` 不用硬编）。
pub const ENV_ENCODER: &str = "SNOW_RECORDER_ENCODER";

/// DXGI VendorId：NVIDIA。
pub const VENDOR_NVIDIA: u32 = 0x10de;
/// DXGI VendorId：AMD。
pub const VENDOR_AMD: u32 = 0x1002;
/// DXGI VendorId：Intel。
pub const VENDOR_INTEL: u32 = 0x8086;

/// 硬件 H.264 编码器种类。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HwCodec {
    /// Intel Quick Sync。
    Qsv,
    /// NVIDIA NVENC。
    Nvenc,
    /// AMD AMF（独显与集显通用）。
    Amf,
}

impl HwCodec {
    /// FFmpeg 编码器名。
    pub fn ffmpeg_name(self) -> &'static str {
        match self {
            Self::Qsv => "h264_qsv",
            Self::Nvenc => "h264_nvenc",
            Self::Amf => "h264_amf",
        }
    }

    /// 编码器所属的 DXGI 厂商号。
    pub fn vendor(self) -> u32 {
        match self {
            Self::Qsv => VENDOR_INTEL,
            Self::Nvenc => VENDOR_NVIDIA,
            Self::Amf => VENDOR_AMD,
        }
    }

    /// 按适配器厂商号取对应编码器；未知厂商返回 `None`。
    ///
    /// # 参数
    /// - `vendor`：DXGI `VendorId`。
    pub fn for_vendor(vendor: u32) -> Option<Self> {
        [Self::Qsv, Self::Nvenc, Self::Amf].into_iter().find(|c| c.vendor() == vendor)
    }
}

/// 编码器选择偏好（来自 `SNOW_RECORDER_ENCODER`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EncoderPreference {
    /// 按采集适配器的厂商自动选。
    Auto,
    /// 强制指定编码器（必须与采集适配器同厂商，否则回落软编）。
    Force(HwCodec),
    /// 不用硬件编码，直接走软编。
    Software,
}

/// 适配器描述（可注入，方便不依赖真机 GPU 的单测）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdapterInfo {
    /// DXGI `VendorId`。
    pub vendor: u32,
    /// 适配器名称（仅日志用）。
    pub description: String,
}

/// 解析 `SNOW_RECORDER_ENCODER` 的取值。
///
/// # 参数
/// - `text`：环境变量值；`None` 或空表示未设置。
///
/// # 返回
/// 偏好；取值不认识时返回错误文本（调用方记日志后按 `Auto` 处理）。
///
/// # 示例
/// ```ignore
/// assert_eq!(parse_encoder_preference(Some("nvenc")), Ok(EncoderPreference::Force(HwCodec::Nvenc)));
/// assert_eq!(parse_encoder_preference(None), Ok(EncoderPreference::Auto));
/// ```
pub fn parse_encoder_preference(text: Option<&str>) -> Result<EncoderPreference, String> {
    let Some(t) = text.map(|t| t.trim().to_ascii_lowercase()).filter(|t| !t.is_empty()) else {
        return Ok(EncoderPreference::Auto);
    };
    match t.as_str() {
        "auto" => Ok(EncoderPreference::Auto),
        "qsv" | "h264_qsv" => Ok(EncoderPreference::Force(HwCodec::Qsv)),
        "nvenc" | "h264_nvenc" => Ok(EncoderPreference::Force(HwCodec::Nvenc)),
        "amf" | "h264_amf" => Ok(EncoderPreference::Force(HwCodec::Amf)),
        "off" | "none" | "soft" | "software" | "0" => Ok(EncoderPreference::Software),
        _ => Err(format!("{ENV_ENCODER}={t:?} 不认识（可选 auto/qsv/nvenc/amf/off），按 auto 处理")),
    }
}

/// 为已确定的采集适配器选编码器。
///
/// # 参数
/// - `adapter`：采集所在适配器。
/// - `preference`：选择偏好。
///
/// # 返回
/// 编码器；关闭硬编、厂商无对应编码器、强制项与适配器不同厂商时返回原因（调用方回落软编）。
///
/// # 示例
/// ```ignore
/// let gpu = AdapterInfo { vendor: VENDOR_NVIDIA, description: "RTX".into() };
/// assert_eq!(select_encoder(&gpu, EncoderPreference::Auto), Ok(HwCodec::Nvenc));
/// ```
pub fn select_encoder(adapter: &AdapterInfo, preference: EncoderPreference) -> Result<HwCodec, String> {
    match preference {
        EncoderPreference::Software => Err(format!("{ENV_ENCODER} 已关闭硬件编码")),
        EncoderPreference::Auto => HwCodec::for_vendor(adapter.vendor)
            .ok_or_else(|| format!("适配器 {} (VendorId 0x{:04x}) 没有受支持的硬件编码器", adapter.description, adapter.vendor)),
        // 编码必须与采集/合成同适配器（零拷贝）；强制项不同厂商需要跨适配器传输，目前未实现
        EncoderPreference::Force(codec) if codec.vendor() == adapter.vendor => Ok(codec),
        EncoderPreference::Force(codec) => Err(format!(
            "强制使用 {} 但采集适配器是 {} (VendorId 0x{:04x})，跨适配器编码未实现",
            codec.ffmpeg_name(),
            adapter.description,
            adapter.vendor
        )),
    }
}

/// 按偏好给候选适配器排序（多显卡时同一选区可能出现在多块适配器上）：能选出编码器的排前面，同类保持原序。
///
/// # 参数
/// - `adapters`：候选适配器。
/// - `preference`：选择偏好。
///
/// # 返回
/// 候选下标，按尝试优先级排列。
pub fn rank_adapters(adapters: &[AdapterInfo], preference: EncoderPreference) -> Vec<usize> {
    let (usable, rest): (Vec<usize>, Vec<usize>) = (0..adapters.len()).partition(|&i| select_encoder(&adapters[i], preference).is_ok());
    usable.into_iter().chain(rest).collect()
}
/// 硬件编码模式。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HardwareMode {
    /// 只用软件编码（上游直录会话）。
    Off,
    /// 自建 GPU 流水线（失败自动回落软编）。
    Gpu,
    /// 上游自带的 GPU 路径（对照用）。
    Upstream,
    /// 自建 GPU 流水线 + Media Foundation 硬件编码（失败回落软编）。
    MediaFoundation,
    /// 自动：自建流水线 + Media Foundation 硬件 MFT，不可用则改用 FFmpeg 厂商硬编（NVENC/QSV/AMF），再不行回落软编。
    Auto,
}

/// 未设置环境变量时的硬件编码模式。
///
/// 选择依据（本机 RTX 4060 实测，见 `docs/recording-handover/experiment-ledger.md` §8）：MF 与 NVENC 的 fps/丢帧/CPU 持平，
/// MF 内存低约 41%（73MB 对 124MB）且不依赖 FFmpeg 的厂商编码器、跨厂商；软编四档 0/12 过线。
pub const DEFAULT_HARDWARE_MODE: HardwareMode = HardwareMode::Auto;

/// 解析 `SNOW_RECORDER_HARDWARE` 的取值。
///
/// # 参数
/// - `text`：环境变量值；`None` 表示未设置。
///
/// # 返回
/// 硬件模式：`0/off` 关闭，`1/gpu` 自建流水线，`upstream` 上游路径，`mf` Media Foundation，`auto` 自动，其余沿用默认。
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
        Some("mf") => HardwareMode::MediaFoundation,
        Some("auto") => HardwareMode::Auto,
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
        assert_eq!(parse_hardware_mode(Some(" MF ")), HardwareMode::MediaFoundation);
        assert_eq!(parse_hardware_mode(Some("auto")), HardwareMode::Auto);
        assert_eq!(parse_hardware_mode(Some("??")), DEFAULT_HARDWARE_MODE);
        assert_eq!(parse_hardware_mode(None), DEFAULT_HARDWARE_MODE);
        // 默认必须是有硬编就用硬件的自动模式(固化的选择顺序见 backend::plan_attempts)
        assert_eq!(DEFAULT_HARDWARE_MODE, HardwareMode::Auto);
    }

    /// 构造测试用适配器。
    fn adapter(vendor: u32, name: &str) -> AdapterInfo {
        AdapterInfo { vendor, description: name.to_string() }
    }

    /// 自动选择：三大厂商（含 AMD 独显/集显同为 AMF）各得其编码器，未知厂商报错。
    #[test]
    fn auto_selects_by_vendor() {
        let auto = EncoderPreference::Auto;
        assert_eq!(select_encoder(&adapter(VENDOR_NVIDIA, "RTX 4060"), auto), Ok(HwCodec::Nvenc));
        assert_eq!(select_encoder(&adapter(VENDOR_AMD, "RX 7600"), auto), Ok(HwCodec::Amf));
        assert_eq!(select_encoder(&adapter(VENDOR_AMD, "Radeon 780M"), auto), Ok(HwCodec::Amf));
        assert_eq!(select_encoder(&adapter(VENDOR_INTEL, "UHD 770"), auto), Ok(HwCodec::Qsv));
        let error = select_encoder(&adapter(0x1414, "Microsoft Basic Render Driver"), auto).unwrap_err();
        assert!(error.contains("没有受支持") && error.contains("0x1414"));
        assert_eq!(HwCodec::Nvenc.ffmpeg_name(), "h264_nvenc");
        assert_eq!(HwCodec::for_vendor(0), None);
    }

    /// 环境变量解析：别名、大小写、空值与非法值。
    #[test]
    fn encoder_preference_parsing() {
        assert_eq!(parse_encoder_preference(None), Ok(EncoderPreference::Auto));
        assert_eq!(parse_encoder_preference(Some("  ")), Ok(EncoderPreference::Auto));
        assert_eq!(parse_encoder_preference(Some("AUTO")), Ok(EncoderPreference::Auto));
        assert_eq!(parse_encoder_preference(Some("NVENC")), Ok(EncoderPreference::Force(HwCodec::Nvenc)));
        assert_eq!(parse_encoder_preference(Some("h264_amf")), Ok(EncoderPreference::Force(HwCodec::Amf)));
        assert_eq!(parse_encoder_preference(Some("qsv")), Ok(EncoderPreference::Force(HwCodec::Qsv)));
        assert_eq!(parse_encoder_preference(Some("off")), Ok(EncoderPreference::Software));
        assert!(parse_encoder_preference(Some("x264")).is_err());
    }

    /// 覆盖：同厂商强制生效；异厂商强制与关闭硬编都给出原因（走软编回退）。
    #[test]
    fn override_and_fallback_paths() {
        let nvidia = adapter(VENDOR_NVIDIA, "RTX 4060");
        assert_eq!(select_encoder(&nvidia, EncoderPreference::Force(HwCodec::Nvenc)), Ok(HwCodec::Nvenc));
        let mismatch = select_encoder(&nvidia, EncoderPreference::Force(HwCodec::Qsv)).unwrap_err();
        assert!(mismatch.contains("h264_qsv") && mismatch.contains("RTX 4060") && mismatch.contains("跨适配器"));
        assert!(select_encoder(&nvidia, EncoderPreference::Software).unwrap_err().contains(ENV_ENCODER));
    }

    /// 多适配器排序：能出编码器的优先，强制项按厂商把匹配的提前，同类保持原序。
    #[test]
    fn adapter_ranking() {
        let list = [adapter(0x1414, "WARP"), adapter(VENDOR_INTEL, "iGPU"), adapter(VENDOR_NVIDIA, "dGPU")];
        assert_eq!(rank_adapters(&list, EncoderPreference::Auto), vec![1, 2, 0]);
        assert_eq!(rank_adapters(&list, EncoderPreference::Force(HwCodec::Nvenc)), vec![2, 0, 1]);
        assert_eq!(rank_adapters(&list, EncoderPreference::Software), vec![0, 1, 2]);
        assert!(rank_adapters(&[], EncoderPreference::Auto).is_empty());
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
