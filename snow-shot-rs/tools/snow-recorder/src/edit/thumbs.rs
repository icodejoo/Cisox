//! 编辑会话取帧：按时间取显示帧、缩放、输出原始 BGRA。
//!
//! 解码帧保持 YUV 直通，色彩转换与缩放由 swscale 一步完成（复用 `yuv::RgbConverter`），
//! 不经过 RGBA 中转。缩放一律"等比、不放大"：
//! - 缩略图：完整落入给定框（不裁切、不补边），实际宽高可能小于框；
//! - 预览帧：限制最大宽度。

use std::path::Path;

use ffmpeg_next::format::Pixel;

use super::EditError;
use super::source::VideoSource;
use super::yuv::RgbConverter;

/// BGRA 每像素字节数。
const BGRA_BYTES: usize = 4;
/// 不透明 alpha。
const OPAQUE: u8 = 255;

/// 目标尺寸约束。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Fit {
    /// 等比缩放到完整落入 `w`x`h` 的框（不放大、不裁切）。
    Within {
        /// 框宽。
        w: u32,
        /// 框高。
        h: u32,
    },
    /// 限制最大宽度（不放大）；0 表示保持原尺寸。
    MaxWidth(u32),
}

impl Fit {
    /// 按源尺寸算出输出尺寸（每边至少 1 像素）。
    ///
    /// # 参数
    /// - `src_w`、`src_h`：源宽高。
    ///
    /// # 示例
    /// ```ignore
    /// assert_eq!(Fit::MaxWidth(32).apply(64, 48), (32, 24));
    /// ```
    pub fn apply(self, src_w: u32, src_h: u32) -> (u32, u32) {
        match self {
            Self::Within { w, h } => fit_within(src_w, src_h, w, h),
            Self::MaxWidth(w) => fit_max_width(src_w, src_h, w),
        }
    }
}

/// 等比缩放到完整落入框内；源已在框内或框为 0 时保持原尺寸（不放大）。
///
/// # 参数
/// - `sw`、`sh`：源宽高。
/// - `bw`、`bh`：框宽高。
pub fn fit_within(sw: u32, sh: u32, bw: u32, bh: u32) -> (u32, u32) {
    if sw == 0 || sh == 0 || bw == 0 || bh == 0 || (sw <= bw && sh <= bh) {
        return (sw.max(1), sh.max(1));
    }
    let (sw64, sh64, bw64, bh64) = (u64::from(sw), u64::from(sh), u64::from(bw), u64::from(bh));
    // 比较 bw/sw 与 bh/sh：更小的那个比例决定缩放
    if bw64 * sh64 <= bh64 * sw64 {
        (bw, scale_side(sh64, bw64, sw64))
    } else {
        (scale_side(sw64, bh64, sh64), bh)
    }
}

/// 限制最大宽度的等比缩放；`max_w` 为 0 或源不宽于它时保持原尺寸。
///
/// # 参数
/// - `sw`、`sh`：源宽高。
/// - `max_w`：最大宽度。
pub fn fit_max_width(sw: u32, sh: u32, max_w: u32) -> (u32, u32) {
    if max_w == 0 || sw <= max_w || sw == 0 {
        return (sw.max(1), sh.max(1));
    }
    (
        max_w,
        scale_side(u64::from(sh), u64::from(max_w), u64::from(sw)),
    )
}

/// `side * num / den` 四舍五入，至少为 1。
fn scale_side(side: u64, num: u64, den: u64) -> u32 {
    let v = (side * num + den / 2) / den;
    u32::try_from(v.max(1)).unwrap_or(u32::MAX)
}

/// 一张渲染好的帧。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Rendered {
    /// 帧实际显示时间（毫秒）。
    pub at_ms: u64,
    /// 宽。
    pub w: u32,
    /// 高。
    pub h: u32,
    /// 紧密排列的 BGRA 像素（`w*h*4` 字节，自上而下）。
    pub pixels: Vec<u8>,
}

/// 取帧后端：给定时间与尺寸约束，返回缩放后的 BGRA 帧。
pub trait FrameRenderer {
    /// 取 `ms` 时刻正在显示的帧并缩放。
    ///
    /// # 参数
    /// - `ms`：相对视频起点的毫秒。
    /// - `fit`：尺寸约束。
    fn render(&mut self, ms: u64, fit: Fit) -> Result<Rendered, EditError>;
}

/// FFmpeg 取帧后端：`VideoSource` 精确 seek（近处复用解码位置）+ swscale 一步转缩放。
pub struct FfmpegRenderer {
    /// 视频源。
    source: VideoSource,
    /// YUV 到 BGRA 的转换/缩放器。
    conv: RgbConverter,
}

impl FfmpegRenderer {
    /// 包装一个已打开的视频源。
    ///
    /// # 参数
    /// - `source`：视频源。
    pub fn new(source: VideoSource) -> Self {
        Self {
            source,
            conv: RgbConverter::new(Pixel::BGRA),
        }
    }
}

impl FrameRenderer for FfmpegRenderer {
    /// 见 trait。
    fn render(&mut self, ms: u64, fit: Fit) -> Result<Rendered, EditError> {
        let decoded = self.source.frame_at_ms(ms)?;
        let (w, h) = fit.apply(decoded.frame.width(), decoded.frame.height());
        self.conv.set_size(Some((w, h)));
        let out = self.conv.convert(&decoded.frame)?;
        let row = w as usize * BGRA_BYTES;
        let stride = out.stride(0);
        let data = out.data(0);
        let mut pixels = Vec::with_capacity(row * h as usize);
        for y in 0..h as usize {
            let line = &data[y * stride..y * stride + row];
            let start = pixels.len();
            pixels.extend_from_slice(line);
            // 转换结果的 alpha 不可依赖，统一置为不透明
            for px in pixels[start..].chunks_exact_mut(BGRA_BYTES) {
                px[3] = OPAQUE;
            }
        }
        Ok(Rendered {
            at_ms: u64::try_from(decoded.ms.max(0)).unwrap_or(0),
            w,
            h,
            pixels,
        })
    }
}

/// 把 BGRA 像素写成文件：先写临时名再改名，读取方不会看到半截文件。
///
/// # 参数
/// - `path`：目标文件（覆盖已有文件）。
/// - `pixels`：像素字节。
pub fn write_pixels(path: &Path, pixels: &[u8]) -> Result<(), EditError> {
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, pixels).map_err(|e| EditError::new(format!("写帧文件失败: {e}")))?;
    std::fs::rename(&tmp, path).map_err(|e| EditError::new(format!("发布帧文件失败: {e}")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::edit::testclip::{self, Clip};

    /// 框内缩放：受限边决定比例，不放大，每边至少 1。
    #[test]
    fn fit_within_cases() {
        assert_eq!(fit_within(1920, 1080, 160, 90), (160, 90));
        assert_eq!(fit_within(1920, 1080, 160, 160), (160, 90));
        assert_eq!(fit_within(1080, 1920, 160, 90), (51, 90));
        assert_eq!(fit_within(64, 48, 160, 90), (64, 48));
        assert_eq!(fit_within(64, 48, 0, 0), (64, 48));
        assert_eq!(fit_within(10000, 10, 100, 100), (100, 1));
    }

    /// 最大宽度：不放大，0 表示原尺寸。
    #[test]
    fn fit_max_width_cases() {
        assert_eq!(fit_max_width(1920, 1080, 960), (960, 540));
        assert_eq!(fit_max_width(64, 48, 960), (64, 48));
        assert_eq!(fit_max_width(64, 48, 0), (64, 48));
        assert_eq!(fit_max_width(1000, 3, 10), (10, 1));
        assert_eq!(Fit::MaxWidth(32).apply(64, 48), (32, 24));
        assert_eq!(Fit::Within { w: 32, h: 32 }.apply(64, 48), (32, 24));
    }

    /// 真实取帧：输出 BGRA 大小正确、亮度对应帧序号、alpha 不透明。
    #[test]
    fn ffmpeg_renderer_outputs_bgra() {
        let dir = testclip::temp_dir("thumbs-render");
        let path = dir.join("clip.mp4");
        let clip = Clip::default();
        testclip::make_clip(&path, &clip).unwrap();
        let mut r = FfmpegRenderer::new(VideoSource::open(&path).unwrap());
        let full = r.render(400, Fit::MaxWidth(0)).unwrap();
        assert_eq!((full.w, full.h), (64, 48));
        assert_eq!(full.pixels.len(), 64 * 48 * 4);
        let want = testclip::gray_of_frame(10);
        for c in &full.pixels[..3] {
            assert!(
                (i32::from(*c) - i32::from(want)).abs() <= 3,
                "{c} vs {want}"
            );
        }
        assert_eq!(full.pixels[3], 255);
        let small = r.render(400, Fit::Within { w: 16, h: 16 }).unwrap();
        assert_eq!((small.w, small.h), (16, 12));
        assert_eq!(small.pixels.len(), 16 * 12 * 4);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
