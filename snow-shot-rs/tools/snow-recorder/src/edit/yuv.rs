//! YUV 直通：解码帧始终保持 YUV 平面，只在编码器必须要 RGB 时才一步转换。
//!
//! 不经过"解码 -> RGBA -> 再转换"的中转：这里的转换器直接从解码帧的 YUV 平面
//! 一步转成目标格式（图片编码器要的 BGR24 / BGRA），1080p 下省掉一整份 8 MB 的 RGBA 帧。
//! 色彩矩阵按帧标注选择（本软件录制的视频是 BT.709），不会误用 BT.601 造成偏色。

use ffmpeg_next as ffmpeg;
use ffmpeg_next::format::Pixel;
use ffmpeg_next::software::scaling;
use ffmpeg_next::util::color;
use ffmpeg_next::{frame, software};

use super::EditError;

/// swscale 色彩矩阵编号：ITU-R BT.709（同 `SWS_CS_ITU709`）。
const SWS_CS_BT709: i32 = 1;
/// swscale 色彩矩阵编号：ITU-R BT.601（同 `SWS_CS_ITU601`）。
const SWS_CS_BT601: i32 = 5;
/// 标注缺失时，高度不小于该值按 BT.709、否则按 BT.601。
const HD_MIN_HEIGHT: u32 = 720;
/// swscale 的 16.16 定点单位 1.0。
const FIXED_ONE: i32 = 1 << 16;

/// 选择 swscale 色彩矩阵编号。
///
/// # 参数
/// - `space`：帧上的色彩空间标注。
/// - `height`：帧高，标注缺失时据此猜测。
fn matrix_for(space: color::Space, height: u32) -> i32 {
    match space {
        color::Space::BT709 => SWS_CS_BT709,
        color::Space::BT470BG | color::Space::SMPTE170M => SWS_CS_BT601,
        _ if height >= HD_MIN_HEIGHT => SWS_CS_BT709,
        _ => SWS_CS_BT601,
    }
}

/// 转换器缓存键：输入变了就要重建。
#[derive(Clone, Copy, PartialEq, Eq)]
struct Key {
    /// 输入像素格式。
    format: Pixel,
    /// 宽。
    width: u32,
    /// 高。
    height: u32,
    /// 是否全范围。
    full_range: bool,
    /// 色彩矩阵编号。
    matrix: i32,
}

/// YUV 到 RGB 类格式的一步转换器（按需重建、复用输出缓冲）。
pub struct RgbConverter {
    /// 目标像素格式（BGR24 或 BGRA）。
    target: Pixel,
    /// 当前缓存的转换上下文及其键。
    ctx: Option<(Key, scaling::Context)>,
    /// 复用的输出帧。
    out: Option<frame::Video>,
}

impl RgbConverter {
    /// 创建转换器。
    ///
    /// # 参数
    /// - `target`：目标像素格式，应为 `Pixel::BGR24` 或 `Pixel::BGRA`。
    pub fn new(target: Pixel) -> Self {
        Self {
            target,
            ctx: None,
            out: None,
        }
    }

    /// 把解码帧一步转成目标格式，返回转换结果（下次调用前有效）。
    ///
    /// # 参数
    /// - `input`：解码帧（YUV 直通，不应事先转过 RGBA）。
    ///
    /// # 返回
    /// 目标格式帧的引用；转换器不可用时返回错误。
    ///
    /// # 示例
    /// ```ignore
    /// let mut conv = RgbConverter::new(Pixel::BGR24);
    /// let bgr = conv.convert(&decoded.frame)?;
    /// ```
    pub fn convert(&mut self, input: &frame::Video) -> Result<&frame::Video, EditError> {
        let key = Key {
            format: input.format(),
            width: input.width(),
            height: input.height(),
            full_range: input.color_range() == color::Range::JPEG
                || input.format() == Pixel::YUVJ420P,
            matrix: matrix_for(input.color_space(), input.height()),
        };
        if self.ctx.as_ref().is_none_or(|(k, _)| *k != key) {
            self.ctx = Some((key, self.build(&key)?));
            self.out = Some(frame::Video::new(self.target, key.width, key.height));
        }
        let (_, ctx) = self
            .ctx
            .as_mut()
            .ok_or_else(|| EditError::new("转换器未就绪"))?;
        let out = self
            .out
            .as_mut()
            .ok_or_else(|| EditError::new("转换器未就绪"))?;
        ctx.run(input, out)
            .map_err(|e| EditError::new(format!("像素转换失败: {e}")))?;
        Ok(out)
    }

    /// 按键创建 swscale 上下文并设置色彩矩阵与范围。
    fn build(&self, key: &Key) -> Result<scaling::Context, EditError> {
        let mut ctx = scaling::Context::get(
            key.format,
            key.width,
            key.height,
            self.target,
            key.width,
            key.height,
            software::scaling::Flags::BICUBIC
                | software::scaling::Flags::ACCURATE_RND
                | software::scaling::Flags::FULL_CHR_H_INT,
        )
        .map_err(|e| EditError::new(format!("创建像素转换器失败: {e}")))?;
        // SAFETY: ctx 在本函数内独占持有；系数表指针由 swscale 静态提供，调用期间有效。
        let code = unsafe {
            let coeffs = ffmpeg::ffi::sws_getCoefficients(key.matrix);
            ffmpeg::ffi::sws_setColorspaceDetails(
                ctx.as_mut_ptr(),
                coeffs,
                i32::from(key.full_range),
                coeffs,
                1,
                0,
                FIXED_ONE,
                FIXED_ONE,
            )
        };
        if code < 0 {
            return Err(EditError::new("设置色彩矩阵失败"));
        }
        Ok(ctx)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 构造填充固定 YUV 值的 YUV420P 帧。
    pub(crate) fn solid_yuv(width: u32, height: u32, y: u8, u: u8, v: u8) -> frame::Video {
        let mut f = frame::Video::new(Pixel::YUV420P, width, height);
        for (plane, value) in [(0, y), (1, u), (2, v)] {
            f.data_mut(plane).fill(value);
        }
        f.set_color_space(color::Space::BT709);
        f.set_color_range(color::Range::MPEG);
        f
    }

    /// 色彩矩阵：显式标注优先，缺失时按高度猜。
    #[test]
    fn matrix_selection() {
        assert_eq!(matrix_for(color::Space::BT709, 100), SWS_CS_BT709);
        assert_eq!(matrix_for(color::Space::SMPTE170M, 1080), SWS_CS_BT601);
        assert_eq!(matrix_for(color::Space::Unspecified, 1080), SWS_CS_BT709);
        assert_eq!(matrix_for(color::Space::Unspecified, 480), SWS_CS_BT601);
    }

    /// 灰色 YUV 一步转成 BGR24：BT.709 限幅 Y=126 约等于灰 128，三通道接近。
    #[test]
    fn gray_converts_directly() {
        ffmpeg::init().unwrap();
        let f = solid_yuv(16, 16, 126, 128, 128);
        let mut conv = RgbConverter::new(Pixel::BGR24);
        let out = conv.convert(&f).unwrap();
        assert_eq!(out.format(), Pixel::BGR24);
        let px = &out.data(0)[..3];
        for c in px {
            assert!((i32::from(*c) - 128).abs() <= 2, "通道值 {c} 偏离灰 128");
        }
    }

    /// BT.709 红色（限幅 Y=63,U=102,V=240）转换后以红为主，验证没有误用 BT.601。
    #[test]
    fn red_uses_bt709_matrix() {
        ffmpeg::init().unwrap();
        let f = solid_yuv(16, 16, 63, 102, 240);
        let mut conv = RgbConverter::new(Pixel::BGR24);
        let out = conv.convert(&f).unwrap();
        let (b, g, r) = (out.data(0)[0], out.data(0)[1], out.data(0)[2]);
        assert!(r >= 250 && g <= 6 && b <= 6, "BGR=({b},{g},{r})");
    }
}
