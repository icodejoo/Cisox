//! Windows 硬件流水线的装配：会话初始化时做一次能力检测并组装好三个阶段的具体类型。
//!
//! 任何一步失败（没有受支持的硬件编码器、视频处理器不支持 BGRA→NV12/多图层、设备不支持共享纹理与栅栏、
//! 选区跨显示器、HDR 桌面……）都返回原因，由上层回落软件编码。

use std::path::PathBuf;
use std::sync::Arc;

use crate::pipeline::{self, PipelineConfig, Running};
use crate::settings::{AdapterInfo, ENV_ENCODER, EncoderPreference, HwCodec, select_encoder};
use crate::timeline::QpcAnchor;
use crate::win::compose::GpuComposer;
use crate::win::dda::DdaCapture;
use crate::win::mfenc::{ENV_MF_QUALITY, MF_POOL_CAPACITY, MfEncoder, parse_rate_control};
use crate::win::hwenc::{SURFACE_ALIGN, DEFAULT_POOL_CAPACITY, HwConfig, HwContext, HwEncoder, QSV_ASYNC_DEPTH, QSV_PRESET};

/// Media Foundation 纹理对齐（与编码尺寸严格一致，只需偶数）。
const MF_SURFACE_ALIGN: u32 = 2;
/// 录制开始前预热的编码表面数。
const PREWARM_SURFACES: usize = 8;
/// 质量参数默认值（与上游 quality=80 对应的 CRF 18 一致）。
pub const DEFAULT_QUALITY: u32 = 18;

/// 硬件流水线的装配参数。
#[derive(Debug, Clone)]
pub struct HardwareSpec {
    /// 选区（虚拟桌面坐标）：x、y、宽、高。
    pub region: (i32, i32, u32, u32),
    /// 输出尺寸（宽、高，须为偶数）。
    pub out_size: (u32, u32),
    /// 输出帧率。
    pub fps: u32,
    /// 是否叠加光标。
    pub show_cursor: bool,
    /// 输出文件（mp4）。
    pub output: PathBuf,
    /// 质量参数。
    pub quality: u32,
    /// 编码器 async_depth（QSV）。
    pub async_depth: u32,
    /// 编码器预设（QSV）。
    pub preset: String,
    /// 硬件编码器选择偏好（默认按适配器厂商自动选）。
    pub encoder: EncoderPreference,
    /// 是否改用 Media Foundation 硬件编码（可选原型，默认关）。
    pub media_foundation: bool,
}

impl HardwareSpec {
    /// 用默认的编码参数构造。
    ///
    /// # 参数
    /// - `region`：选区 `(x, y, 宽, 高)`。
    /// - `out_size`：输出尺寸。
    /// - `fps`：帧率。
    /// - `show_cursor`：是否叠加光标。
    /// - `output`：输出路径。
    pub fn new(region: (i32, i32, u32, u32), out_size: (u32, u32), fps: u32, show_cursor: bool, output: PathBuf) -> Self {
        Self {
            region,
            out_size,
            fps,
            show_cursor,
            output,
            quality: DEFAULT_QUALITY,
            async_depth: QSV_ASYNC_DEPTH,
            preset: QSV_PRESET.to_string(),
            encoder: EncoderPreference::Auto,
            media_foundation: false,
        }
    }

    /// 校验：尺寸非零且输出为偶数、帧率非零。
    pub fn validate(&self) -> Result<(), String> {
        let (_, _, w, h) = self.region;
        if w == 0 || h == 0 || self.out_size.0 == 0 || self.out_size.1 == 0 {
            return Err("录制尺寸必须大于 0".into());
        }
        if !self.out_size.0.is_multiple_of(2) || !self.out_size.1.is_multiple_of(2) {
            return Err("输出宽高必须为偶数".into());
        }
        if self.fps == 0 {
            return Err("帧率必须大于 0".into());
        }
        Ok(())
    }
}

/// 装配并启动 Windows 硬件流水线（DXGI 复制 + VideoProcessor + 厂商硬件编码）。
///
/// # 参数
/// - `spec`：装配参数。
///
/// # 返回
/// 运行中的流水线；能力检测或首帧探测失败返回原因（输出文件可能已创建，由调用方清理）。
pub fn start_hardware(spec: &HardwareSpec) -> Result<Running, String> {
    spec.validate()?;
    let (_, _, w, h) = spec.region;
    // 关闭硬编时不必打开采集设备，直接让上层回落软编
    if spec.encoder == EncoderPreference::Software {
        return Err(format!("{ENV_ENCODER} 已关闭硬件编码"));
    }
    let (mut capture, device) = DdaCapture::open(spec.region, QpcAnchor::capture_now(), spec.encoder)?;
    let identity = device.identity();
    let adapter = AdapterInfo { vendor: identity.vendor, description: identity.description.clone() };
    // Media Foundation 不依赖 FFmpeg 编码器：厂商不认识也能用（帧池借用非 QSV 的 D3D11 路径）
    let codec = match select_encoder(&adapter, spec.encoder) {
        Ok(codec) => codec,
        Err(_) if spec.media_foundation => HwCodec::Nvenc,
        Err(e) => return Err(e),
    };
    let backend = if spec.media_foundation { "Media Foundation 硬件 MFT" } else { codec.ffmpeg_name() };
    eprintln!("硬件编码：适配器 {} (VendorId 0x{:04x}) -> {backend}", adapter.description, adapter.vendor);
    // Media Foundation 要求纹理尺寸与编码尺寸严格一致（对齐取 2），其余厂商编码器沿用 16 对齐
    let align = if spec.media_foundation { MF_SURFACE_ALIGN } else { SURFACE_ALIGN };
    let hw = Arc::new(HwContext::with_align(device.clone(), spec.out_size, if spec.media_foundation { MF_POOL_CAPACITY } else { DEFAULT_POOL_CAPACITY }, codec, align, !spec.media_foundation)?);
    hw.prewarm(PREWARM_SURFACES)?;
    if crate::win::dda::wgc_mode() {
        capture.enable_wgc()?;
        eprintln!("实验：用 Windows Graphics Capture 取帧");
    }
    let composer = GpuComposer::new(device, Arc::clone(&hw), (w, h), spec.out_size, spec.fps)?;
    if spec.media_foundation {
        let encoder = MfEncoder::open(hw, &spec.output, spec.out_size, spec.fps, parse_rate_control(std::env::var(ENV_MF_QUALITY).ok().as_deref()))?;
        return pipeline::start(
            PipelineConfig { fps: spec.fps, show_cursor: spec.show_cursor },
            "dxgi+videoprocessor+media-foundation",
            capture,
            composer,
            encoder,
        );
    }
    let codec = hw.codec_name();
    let encoder = HwEncoder::open(
        hw,
        &HwConfig {
            path: spec.output.clone(),
            width: spec.out_size.0,
            height: spec.out_size.1,
            fps: spec.fps,
            quality: spec.quality,
            async_depth: spec.async_depth,
            preset: spec.preset.clone(),
        },
    )?;
    pipeline::start(
        PipelineConfig { fps: spec.fps, show_cursor: spec.show_cursor },
        &format!("dxgi+videoprocessor+{codec}"),
        capture,
        composer,
        encoder,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 配置校验：零尺寸、奇数尺寸、零帧率都拒绝。
    #[test]
    fn spec_validation() {
        let ok = HardwareSpec::new((0, 0, 1920, 1080), (1920, 1080), 30, true, PathBuf::from("a.mp4"));
        assert!(ok.validate().is_ok());
        let mut bad = ok.clone();
        bad.region.2 = 0;
        assert!(bad.validate().is_err());
        let mut odd = ok.clone();
        odd.out_size = (1919, 1080);
        assert!(odd.validate().is_err());
        let mut zero = ok;
        zero.fps = 0;
        assert!(zero.validate().is_err());
    }

    /// 选区在任何显示器之外时装配失败并给出原因（不会 panic，也不创建输出文件）。
    #[test]
    fn region_outside_every_display_fails_cleanly() {
        let path = std::env::temp_dir().join("snow-recorder-assemble-test.mp4");
        let _ = std::fs::remove_file(&path);
        let spec = HardwareSpec::new((100_000, 100_000, 64, 64), (64, 64), 30, false, path.clone());
        let error = start_hardware(&spec).err().expect("应当失败");
        assert!(!error.is_empty());
        assert!(!path.exists());
    }
}
