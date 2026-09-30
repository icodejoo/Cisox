//! 冻结底图：把采集帧一次性转成 GPUI 图像资源，并提供按需的局部像素访问。
//!
//! 整屏像素只在构造时“移动”进图像资源（不拷贝），之后放大镜取样、选区裁切都直接读
//! 图像资源里的同一份缓冲，渲染阶段只复用 `Arc<RenderImage>`，不会逐帧拷贝整屏数据。

use image::{Frame, RgbaImage};
use snow_platform::capture::CapturedScreen;
use snow_ui::shell::geometry::PhysicalRect;
use snow_ui::ui::RenderImage;
use std::sync::Arc;

/// 每像素字节数（BGRA）。
const BYTES_PER_PIXEL: usize = 4;

/// 冻结的屏幕底图（BGRA，不透明）。
pub struct FrozenFrame {
    /// 图像物理宽度。
    width: u32,
    /// 图像物理高度。
    height: u32,
    /// GPUI 图像资源（持有唯一一份像素缓冲）。
    image: Arc<RenderImage>,
}

impl FrozenFrame {
    /// 由采集帧构造冻结底图；像素缓冲被移动进图像资源，不做整屏拷贝。
    ///
    /// # 参数
    /// - `screen`：采集帧（BGRA，长度须为 `宽 * 高 * 4`，尺寸不能为 0）。
    ///
    /// # 返回
    /// 冻结底图；尺寸为 0 或缓冲长度不符时返回错误说明。
    ///
    /// ```ignore
    /// let frame = FrozenFrame::from_captured(CapturedScreen::new_solid(4, 4, (1, 2, 3, 255)))?;
    /// assert_eq!(frame.size(), (4, 4));
    /// ```
    pub fn from_captured(screen: CapturedScreen) -> Result<Self, String> {
        let (width, height) = (screen.width, screen.height);
        let expected = (width as usize)
            .checked_mul(height as usize)
            .and_then(|n| n.checked_mul(BYTES_PER_PIXEL))
            .filter(|n| *n > 0)
            .ok_or_else(|| format!("采集帧尺寸非法: {width}x{height}"))?;
        if screen.data.len() != expected {
            return Err(format!(
                "采集帧缓冲长度不符: 期望 {expected}, 实际 {}",
                screen.data.len()
            ));
        }
        // GPUI 的 RenderImage 约定缓冲为 BGRA，这里把 BGRA 字节直接装进 RgbaImage 容器
        let buffer = RgbaImage::from_raw(width, height, screen.data)
            .ok_or_else(|| "构造图像缓冲失败".to_string())?;
        let image = Arc::new(RenderImage::new(vec![Frame::new(buffer)]));
        Ok(Self {
            width,
            height,
            image,
        })
    }

    /// 图像物理尺寸 `(宽, 高)`。
    pub fn size(&self) -> (u32, u32) {
        (self.width, self.height)
    }

    /// 整幅图像的物理矩形（原点为 0,0）。
    pub fn bounds(&self) -> PhysicalRect {
        PhysicalRect::new(0, 0, self.width as i32, self.height as i32)
    }

    /// GPUI 图像资源句柄（克隆只增加引用计数）。
    pub fn image(&self) -> Arc<RenderImage> {
        Arc::clone(&self.image)
    }

    /// BGRA 像素缓冲（借用图像资源内的唯一一份数据）。
    fn pixels(&self) -> &[u8] {
        self.image.as_bytes(0).unwrap_or(&[])
    }

    /// 标注合成用的只读底图视图（借用图像资源里的 BGRA 缓冲，不拷贝）。
    ///
    /// ```ignore
    /// let view = frame.base_view();
    /// assert_eq!(view.bgra.len(), (view.width * view.height * 4) as usize);
    /// ```
    pub fn base_view(&self) -> crate::annotation::BaseView<'_> {
        crate::annotation::BaseView {
            width: self.width,
            height: self.height,
            bgra: self.pixels(),
        }
    }

    /// 读取单个像素，返回 `(r, g, b, a)`；越界返回 `None`。
    ///
    /// # 参数
    /// - `x` / `y`：图像内的物理坐标。
    ///
    /// ```ignore
    /// assert_eq!(frame.pixel_rgba(0, 0), Some((1, 2, 3, 255)));
    /// assert_eq!(frame.pixel_rgba(-1, 0), None);
    /// ```
    pub fn pixel_rgba(&self, x: i32, y: i32) -> Option<(u8, u8, u8, u8)> {
        if x < 0 || y < 0 || x >= self.width as i32 || y >= self.height as i32 {
            return None;
        }
        let index = (y as usize * self.width as usize + x as usize) * BYTES_PER_PIXEL;
        let p = self.pixels().get(index..index + BYTES_PER_PIXEL)?;
        Some((p[2], p[1], p[0], p[3]))
    }

    /// 以 `(center_x, center_y)` 为中心取 `dimension x dimension` 的 RGBA 网格（放大镜用）。
    ///
    /// 只读取 `dimension²` 个像素；越界位置填不透明黑。
    ///
    /// # 参数
    /// - `center_x` / `center_y`：中心像素的物理坐标。
    /// - `dimension`：网格边长（通常为奇数）。
    ///
    /// # 返回
    /// 长度为 `dimension * dimension * 4` 的 RGBA 字节。
    ///
    /// ```ignore
    /// let grid = frame.sample_rgba_grid(10, 10, 15);
    /// assert_eq!(grid.len(), 15 * 15 * 4);
    /// ```
    pub fn sample_rgba_grid(&self, center_x: i32, center_y: i32, dimension: usize) -> Vec<u8> {
        let half = (dimension / 2) as i32;
        let mut out = Vec::with_capacity(dimension * dimension * BYTES_PER_PIXEL);
        for row in 0..dimension as i32 {
            for col in 0..dimension as i32 {
                let (r, g, b, a) = self
                    .pixel_rgba(center_x - half + col, center_y - half + row)
                    .unwrap_or((0, 0, 0, u8::MAX));
                out.extend_from_slice(&[r, g, b, a]);
            }
        }
        out
    }

    /// 裁切选区并转成 RGBA（只拷贝选区本身，不触碰整屏）。
    ///
    /// 选区超出图像的部分会被裁掉；与图像完全不相交时返回 `None`。
    ///
    /// # 参数
    /// - `rect`：图像坐标系下的选区（物理像素）。
    ///
    /// # 返回
    /// `(宽, 高, RGBA 字节)`。
    ///
    /// ```ignore
    /// let (w, h, rgba) = frame.crop_rgba(PhysicalRect::new(2, 2, 3, 3)).unwrap();
    /// assert_eq!((w, h, rgba.len()), (3, 3, 36));
    /// ```
    pub fn crop_rgba(&self, rect: PhysicalRect) -> Option<(u32, u32, Vec<u8>)> {
        let clipped = self.bounds().intersect(&rect)?;
        if clipped.is_empty() {
            return None;
        }
        let (w, h) = (clipped.width as usize, clipped.height as usize);
        let stride = self.width as usize * BYTES_PER_PIXEL;
        let src = self.pixels();
        let mut out = Vec::with_capacity(w * h * BYTES_PER_PIXEL);
        for row in 0..h {
            let start = (clipped.y as usize + row) * stride + clipped.x as usize * BYTES_PER_PIXEL;
            let line = src.get(start..start + w * BYTES_PER_PIXEL)?;
            for p in line.chunks_exact(BYTES_PER_PIXEL) {
                out.extend_from_slice(&[p[2], p[1], p[0], p[3]]);
            }
        }
        Some((clipped.width as u32, clipped.height as u32, out))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 构造一张每个像素颜色由坐标决定的测试帧：`r = x, g = y, b = 7`。
    fn gradient_frame(w: u32, h: u32) -> FrozenFrame {
        let mut data = Vec::new();
        for y in 0..h {
            for x in 0..w {
                data.extend_from_slice(&[7, y as u8, x as u8, 255]); // BGRA
            }
        }
        FrozenFrame::from_captured(CapturedScreen {
            width: w,
            height: h,
            data,
        })
        .unwrap()
    }

    /// 非法尺寸与长度不符的采集帧被拒绝。
    #[test]
    fn rejects_bad_frames() {
        let zero = CapturedScreen {
            width: 0,
            height: 5,
            data: vec![],
        };
        assert!(FrozenFrame::from_captured(zero).is_err());
        let short = CapturedScreen {
            width: 2,
            height: 2,
            data: vec![0; 15],
        };
        assert!(FrozenFrame::from_captured(short).is_err());
    }

    /// 单像素读取按 BGRA→RGBA 换算，越界返回 None。
    #[test]
    fn pixel_read_swaps_channels() {
        let f = gradient_frame(8, 6);
        assert_eq!(f.pixel_rgba(3, 2), Some((3, 2, 7, 255)));
        assert_eq!(f.pixel_rgba(-1, 0), None);
        assert_eq!(f.pixel_rgba(8, 0), None);
        assert_eq!(f.pixel_rgba(0, 6), None);
    }

    /// 放大镜网格：中心像素正确，屏幕边缘外填不透明黑。
    #[test]
    fn magnifier_grid_edges() {
        let f = gradient_frame(8, 6);
        let g = f.sample_rgba_grid(0, 0, 5);
        assert_eq!(g.len(), 5 * 5 * 4);
        // 中心（第 2 行第 2 列）就是 (0,0)
        let c = (2 * 5 + 2) * 4;
        assert_eq!(&g[c..c + 4], &[0, 0, 7, 255]);
        // 左上角在屏外
        assert_eq!(&g[0..4], &[0, 0, 0, 255]);
        // 右下相邻像素 (1,1) 有真实数据
        let d = (3 * 5 + 3) * 4;
        assert_eq!(&g[d..d + 4], &[1, 1, 7, 255]);
    }

    /// 裁切：尺寸与像素正确，越界部分被裁掉，完全在外返回 None。
    #[test]
    fn crop_clips_to_bounds() {
        let f = gradient_frame(8, 6);
        let (w, h, rgba) = f.crop_rgba(PhysicalRect::new(2, 1, 3, 2)).unwrap();
        assert_eq!((w, h), (3, 2));
        assert_eq!(&rgba[0..4], &[2, 1, 7, 255]);
        let last = (w * h - 1) as usize * 4;
        assert_eq!(&rgba[last..last + 4], &[4, 2, 7, 255]);

        let (w, h, _) = f.crop_rgba(PhysicalRect::new(6, 4, 10, 10)).unwrap();
        assert_eq!((w, h), (2, 2));
        assert!(f.crop_rgba(PhysicalRect::new(20, 20, 4, 4)).is_none());
        assert!(f.crop_rgba(PhysicalRect::new(0, 0, 0, 5)).is_none());
    }

    /// 图像资源句柄是同一份数据（克隆不复制缓冲）。
    #[test]
    fn image_handle_is_shared() {
        let f = gradient_frame(4, 4);
        let a = f.image();
        let b = f.image();
        assert!(Arc::ptr_eq(&a, &b));
        assert_eq!(a.as_bytes(0).map(<[u8]>::len), Some(4 * 4 * 4));
    }
}
