//! 多屏冻结底图：每块显示器一张图，外加它们拼成的虚拟桌面画布。
//!
//! 画布坐标 = 桌面物理坐标 − 虚拟桌面外接矩形的左上角，所以原点恒为 (0,0)、坐标非负；
//! 每块显示器在画布里占一个矩形。接口与单屏的 [`FrozenFrame`] 同名同义（`size` / `bounds` /
//! `pixel_rgba` / `sample_rgba_grid` / `crop_rgba` / `base_view`），选区、蒙版、标注、导出等逻辑
//! 不需要关心有几块屏。整桌面连续缓冲（标注基底）只在多屏且真正需要时才合成，单屏零拷贝。

use crate::annotation::BaseView;
use crate::frozen_frame::FrozenFrame;
use snow_platform::capture::CapturedScreen;
use snow_ui::shell::geometry::{PhysicalPoint, PhysicalRect};
use snow_ui::ui::RenderImage;
use std::cell::OnceCell;
use std::sync::Arc;

/// 每像素字节数。
const BYTES_PER_PIXEL: usize = 4;

/// 画布里的一块显示器：它在画布中的矩形与它的冻结帧。
pub struct MonitorFrame {
    /// 该显示器在画布里的物理矩形（宽高等于帧尺寸）。
    pub rect: PhysicalRect,
    /// 该显示器的冻结帧。
    pub frame: FrozenFrame,
}

/// 多屏冻结底图。
pub struct DesktopFrames {
    /// 各显示器的帧（顺序与创建时一致）。
    monitors: Vec<MonitorFrame>,
    /// 画布尺寸（虚拟桌面外接矩形）。
    size: (u32, u32),
    /// 多屏时按需合成的整画布 BGRA（空洞为不透明黑）；单屏不使用。
    composite: OnceCell<Vec<u8>>,
}

impl DesktopFrames {
    /// 单显示器：画布就是这张帧。
    ///
    /// ```ignore
    /// let frames = DesktopFrames::single(frame);
    /// assert_eq!(frames.count(), 1);
    /// ```
    pub fn single(frame: FrozenFrame) -> Self {
        let (w, h) = frame.size();
        Self {
            monitors: vec![MonitorFrame {
                rect: PhysicalRect::new(0, 0, w as i32, h as i32),
                frame,
            }],
            size: (w, h),
            composite: OnceCell::new(),
        }
    }

    /// 由若干显示器帧组成画布。
    ///
    /// # 参数
    /// - `monitors`：各显示器在**画布坐标**里的矩形与帧；矩形宽高必须等于帧尺寸。
    ///
    /// # 返回
    /// 画布；列表为空、矩形与帧尺寸不符、或超出 `u32` 范围时返回错误说明。
    pub fn new(monitors: Vec<MonitorFrame>) -> Result<Self, String> {
        let first = monitors.first().ok_or("没有任何显示器帧")?;
        let mut right = first.rect.right();
        let mut bottom = first.rect.bottom();
        for monitor in &monitors {
            let (w, h) = monitor.frame.size();
            if (monitor.rect.width, monitor.rect.height) != (w as i32, h as i32) {
                return Err(format!(
                    "显示器矩形 {:?} 与帧尺寸 {w}x{h} 不符",
                    monitor.rect
                ));
            }
            if monitor.rect.x < 0 || monitor.rect.y < 0 {
                return Err(format!("显示器矩形 {:?} 的画布坐标不能为负", monitor.rect));
            }
            right = right.max(monitor.rect.right());
            bottom = bottom.max(monitor.rect.bottom());
        }
        Ok(Self {
            size: (right as u32, bottom as u32),
            monitors,
            composite: OnceCell::new(),
        })
    }

    /// 把一整张画布 BGRA 像素按各显示器矩形切成每屏一帧（历史记录恢复用）。
    ///
    /// # 参数
    /// - `width` / `height`：画布尺寸。
    /// - `bgra`：画布像素（长度须为 `宽 * 高 * 4`）。
    /// - `rects`：各显示器在画布里的矩形（顺序即窗口顺序）。
    ///
    /// # 返回
    /// 画布；缓冲长度不符或矩形越界返回错误说明。
    pub fn from_canvas_bgra(
        width: u32,
        height: u32,
        bgra: Vec<u8>,
        rects: &[PhysicalRect],
    ) -> Result<Self, String> {
        if bgra.len() != width as usize * height as usize * BYTES_PER_PIXEL {
            return Err("画布缓冲长度不符".into());
        }
        let canvas = PhysicalRect::new(0, 0, width as i32, height as i32);
        if let [only] = rects
            && *only == canvas
        {
            let frame = FrozenFrame::from_captured(CapturedScreen {
                width,
                height,
                data: bgra,
            })?;
            return Ok(Self::single(frame));
        }
        let stride = width as usize * BYTES_PER_PIXEL;
        let mut monitors = Vec::with_capacity(rects.len());
        for rect in rects {
            if canvas.intersect(rect) != Some(*rect) {
                return Err(format!("显示器矩形 {rect:?} 超出画布"));
            }
            let (w, h) = (rect.width as usize, rect.height as usize);
            let mut data = Vec::with_capacity(w * h * BYTES_PER_PIXEL);
            for row in 0..h {
                let start = (rect.y as usize + row) * stride + rect.x as usize * BYTES_PER_PIXEL;
                data.extend_from_slice(&bgra[start..start + w * BYTES_PER_PIXEL]);
            }
            let frame = FrozenFrame::from_captured(CapturedScreen {
                width: w as u32,
                height: h as u32,
                data,
            })?;
            monitors.push(MonitorFrame { rect: *rect, frame });
        }
        Self::new(monitors)
    }

    /// 画布物理尺寸 `(宽, 高)`。
    pub fn size(&self) -> (u32, u32) {
        self.size
    }

    /// 整个画布的物理矩形（原点为 0,0）。
    pub fn bounds(&self) -> PhysicalRect {
        PhysicalRect::new(0, 0, self.size.0 as i32, self.size.1 as i32)
    }

    /// 显示器数量。
    pub fn count(&self) -> usize {
        self.monitors.len()
    }

    /// 第 `index` 块显示器在画布里的矩形。
    pub fn rect(&self, index: usize) -> PhysicalRect {
        self.monitors[index].rect
    }

    /// 第 `index` 块显示器的 GPUI 图像句柄（克隆只增加引用计数）。
    pub fn image(&self, index: usize) -> Arc<RenderImage> {
        self.monitors[index].frame.image()
    }

    /// 全部显示器的图像句柄（关闭窗口时逐个释放图集用）。
    pub fn images(&self) -> Vec<Arc<RenderImage>> {
        self.monitors.iter().map(|m| m.frame.image()).collect()
    }

    /// 包含该点的显示器序号；落在空洞或画布外返回 `None`。
    pub fn monitor_at(&self, point: PhysicalPoint) -> Option<usize> {
        self.monitors.iter().position(|m| m.rect.contains(point))
    }

    /// 离该点最近的显示器序号（点在空洞或画布外时用）；至少有一块显示器时一定有结果。
    pub fn nearest_monitor(&self, point: PhysicalPoint) -> usize {
        self.monitor_at(point).unwrap_or_else(|| {
            let distance = |r: &PhysicalRect| {
                let dx = (r.x - point.x).max(0).max(point.x - (r.right() - 1)) as i64;
                let dy = (r.y - point.y).max(0).max(point.y - (r.bottom() - 1)) as i64;
                dx * dx + dy * dy
            };
            self.monitors
                .iter()
                .enumerate()
                .min_by_key(|(_, m)| distance(&m.rect))
                .map_or(0, |(i, _)| i)
        })
    }

    /// 读取画布上单个像素，返回 `(r, g, b, a)`；越界或落在空洞返回 `None`。
    pub fn pixel_rgba(&self, x: i32, y: i32) -> Option<(u8, u8, u8, u8)> {
        let index = self.monitor_at(PhysicalPoint::new(x, y))?;
        let monitor = &self.monitors[index];
        monitor
            .frame
            .pixel_rgba(x - monitor.rect.x, y - monitor.rect.y)
    }

    /// 以 `(center_x, center_y)` 为中心取 `dimension x dimension` 的 RGBA 网格（放大镜用，可跨屏缝）。
    ///
    /// 越界或空洞位置填不透明黑。
    pub fn sample_rgba_grid(&self, center_x: i32, center_y: i32, dimension: usize) -> Vec<u8> {
        if let [only] = self.monitors.as_slice() {
            return only.frame.sample_rgba_grid(center_x, center_y, dimension);
        }
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

    /// 裁切画布上的选区并转成 RGBA；跨屏时逐屏拼接，空洞与画布外为全透明。
    ///
    /// # 参数
    /// - `rect`：画布坐标系下的选区（物理像素）。
    ///
    /// # 返回
    /// `(宽, 高, RGBA 字节)`；选区超出画布的部分被裁掉，与画布完全不相交时返回 `None`。
    pub fn crop_rgba(&self, rect: PhysicalRect) -> Option<(u32, u32, Vec<u8>)> {
        if let [only] = self.monitors.as_slice() {
            return only.frame.crop_rgba(rect);
        }
        let clipped = self.bounds().intersect(&rect)?;
        if clipped.is_empty() {
            return None;
        }
        let (w, h) = (clipped.width as usize, clipped.height as usize);
        let mut out = vec![0u8; w * h * BYTES_PER_PIXEL];
        for monitor in &self.monitors {
            let Some(part) = monitor.rect.intersect(&clipped) else {
                continue;
            };
            if part.is_empty() {
                continue;
            }
            let local = PhysicalRect::new(
                part.x - monitor.rect.x,
                part.y - monitor.rect.y,
                part.width,
                part.height,
            );
            let (pw, _, pixels) = monitor.frame.crop_rgba(local)?;
            for row in 0..part.height as usize {
                let src = row * pw as usize * BYTES_PER_PIXEL;
                let dst = ((part.y - clipped.y) as usize + row) * w * BYTES_PER_PIXEL
                    + (part.x - clipped.x) as usize * BYTES_PER_PIXEL;
                let len = part.width as usize * BYTES_PER_PIXEL;
                out[dst..dst + len].copy_from_slice(&pixels[src..src + len]);
            }
        }
        Some((clipped.width as u32, clipped.height as u32, out))
    }

    /// 标注合成用的只读底图视图。单屏直接借用该屏缓冲（零拷贝）；多屏第一次调用时合成整画布，之后复用。
    pub fn base_view(&self) -> BaseView<'_> {
        if let [only] = self.monitors.as_slice() {
            return only.frame.base_view();
        }
        let bgra = self.composite.get_or_init(|| self.compose_bgra());
        BaseView {
            width: self.size.0,
            height: self.size.1,
            bgra,
        }
    }

    /// 合成整画布 BGRA（空洞为不透明黑）。
    fn compose_bgra(&self) -> Vec<u8> {
        let (width, height) = (self.size.0 as usize, self.size.1 as usize);
        let mut out = vec![0u8; width * height * BYTES_PER_PIXEL];
        for pixel in out.chunks_exact_mut(BYTES_PER_PIXEL) {
            pixel[3] = u8::MAX;
        }
        for monitor in &self.monitors {
            let src = monitor.frame.bgra_pixels();
            let mw = monitor.rect.width as usize * BYTES_PER_PIXEL;
            for row in 0..monitor.rect.height as usize {
                let dst = (monitor.rect.y as usize + row) * width * BYTES_PER_PIXEL
                    + monitor.rect.x as usize * BYTES_PER_PIXEL;
                out[dst..dst + mw].copy_from_slice(&src[row * mw..(row + 1) * mw]);
            }
        }
        out
    }

    /// 多屏合成缓冲是否已经建立（测试与内存探针用）。
    pub fn composite_built(&self) -> bool {
        self.composite.get().is_some()
    }

    /// 释放多屏合成缓冲（会话收尾时调用；之后再取 `base_view` 会重新合成）。
    pub fn drop_composite(&mut self) {
        self.composite = OnceCell::new();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 纯色帧：BGRA = (shade, shade, shade, 255)。
    fn solid(w: u32, h: u32, shade: u8) -> FrozenFrame {
        FrozenFrame::from_captured(CapturedScreen {
            width: w,
            height: h,
            data: [shade, shade, shade, 255].repeat((w * h) as usize),
        })
        .unwrap()
    }

    /// 左右并排两块 4x3 屏：左屏 10、右屏 200，中间无缝。
    fn two_side_by_side() -> DesktopFrames {
        DesktopFrames::new(vec![
            MonitorFrame {
                rect: PhysicalRect::new(0, 0, 4, 3),
                frame: solid(4, 3, 10),
            },
            MonitorFrame {
                rect: PhysicalRect::new(4, 0, 4, 3),
                frame: solid(4, 3, 200),
            },
        ])
        .unwrap()
    }

    /// 画布尺寸取外接矩形；矩形与帧尺寸不符、坐标为负、列表为空都被拒绝。
    #[test]
    fn size_is_bounding_box_and_bad_input_rejected() {
        let frames = two_side_by_side();
        assert_eq!(frames.size(), (8, 3));
        assert_eq!(frames.bounds(), PhysicalRect::new(0, 0, 8, 3));
        assert_eq!(frames.count(), 2);
        let bad = DesktopFrames::new(vec![MonitorFrame {
            rect: PhysicalRect::new(0, 0, 5, 3),
            frame: solid(4, 3, 1),
        }]);
        assert!(bad.is_err());
        let negative = DesktopFrames::new(vec![MonitorFrame {
            rect: PhysicalRect::new(-1, 0, 4, 3),
            frame: solid(4, 3, 1),
        }]);
        assert!(negative.is_err());
        assert!(DesktopFrames::new(Vec::new()).is_err());
    }

    /// 定位：屏内点命中对应显示器；空洞 / 画布外返回 None，最近显示器按距离选。
    #[test]
    fn monitor_lookup_and_nearest() {
        let gap = DesktopFrames::new(vec![
            MonitorFrame {
                rect: PhysicalRect::new(0, 0, 4, 3),
                frame: solid(4, 3, 10),
            },
            MonitorFrame {
                rect: PhysicalRect::new(10, 0, 4, 3),
                frame: solid(4, 3, 200),
            },
        ])
        .unwrap();
        assert_eq!(gap.monitor_at(PhysicalPoint::new(3, 2)), Some(0));
        assert_eq!(gap.monitor_at(PhysicalPoint::new(10, 0)), Some(1));
        assert_eq!(gap.monitor_at(PhysicalPoint::new(6, 1)), None);
        assert_eq!(gap.nearest_monitor(PhysicalPoint::new(5, 1)), 0);
        assert_eq!(gap.nearest_monitor(PhysicalPoint::new(8, 1)), 1);
        assert_eq!(gap.nearest_monitor(PhysicalPoint::new(100, 100)), 1);
    }

    /// 跨缝裁剪：左半来自左屏，右半来自右屏；超出画布被裁；完全在外为 None。
    #[test]
    fn crop_stitches_across_the_seam() {
        let frames = two_side_by_side();
        let (w, h, rgba) = frames.crop_rgba(PhysicalRect::new(2, 1, 4, 2)).unwrap();
        assert_eq!((w, h), (4, 2));
        let at = |x: usize, y: usize| rgba[(y * 4 + x) * 4];
        assert_eq!((at(0, 0), at(1, 0), at(2, 0), at(3, 0)), (10, 10, 200, 200));
        assert_eq!((at(0, 1), at(3, 1)), (10, 200));
        let (w, h, _) = frames.crop_rgba(PhysicalRect::new(6, 2, 10, 10)).unwrap();
        assert_eq!((w, h), (2, 1));
        assert!(frames.crop_rgba(PhysicalRect::new(20, 0, 3, 3)).is_none());
    }

    /// 空洞在裁剪里是全透明；单个像素读取空洞为 None。
    #[test]
    fn holes_are_transparent_in_crop() {
        let gap = DesktopFrames::new(vec![
            MonitorFrame {
                rect: PhysicalRect::new(0, 0, 2, 2),
                frame: solid(2, 2, 50),
            },
            MonitorFrame {
                rect: PhysicalRect::new(4, 0, 2, 2),
                frame: solid(2, 2, 90),
            },
        ])
        .unwrap();
        let (w, _, rgba) = gap.crop_rgba(PhysicalRect::new(0, 0, 6, 1)).unwrap();
        let alpha = |x: usize| rgba[x * 4 + 3];
        assert_eq!(w, 6);
        assert_eq!((alpha(0), alpha(2), alpha(3), alpha(4)), (255, 0, 0, 255));
        assert_eq!(gap.pixel_rgba(2, 0), None);
        assert_eq!(gap.pixel_rgba(4, 1), Some((90, 90, 90, 255)));
    }

    /// 放大镜网格跨屏缝取样；越界填黑。
    #[test]
    fn magnifier_grid_crosses_the_seam() {
        let frames = two_side_by_side();
        let grid = frames.sample_rgba_grid(4, 1, 3);
        let red = |col: usize, row: usize| grid[(row * 3 + col) * 4];
        assert_eq!((red(0, 1), red(1, 1), red(2, 1)), (10, 200, 200));
        let edge = frames.sample_rgba_grid(0, 0, 3);
        assert_eq!(&edge[0..4], &[0, 0, 0, 255], "画布外填不透明黑");
    }

    /// 标注基底：多屏第一次取才合成，内容按位置拼好；单屏不合成。
    #[test]
    fn base_view_is_lazy_and_correct() {
        let frames = two_side_by_side();
        assert!(!frames.composite_built());
        let view = frames.base_view();
        assert_eq!((view.width, view.height), (8, 3));
        assert_eq!(view.bgra[0], 10);
        assert_eq!(view.bgra[4 * 4], 200);
        assert!(frames.composite_built());
        let single = DesktopFrames::single(solid(4, 3, 7));
        assert_eq!(single.base_view().bgra[0], 7);
        assert!(!single.composite_built(), "单屏不合成");
    }

    /// 从整张画布像素切回每屏一帧，与原来的拼接结果一致；单块铺满时不拷贝成多屏。
    #[test]
    fn from_canvas_slices_per_monitor() {
        let frames = two_side_by_side();
        let canvas = frames.base_view().bgra.to_vec();
        let rects = [PhysicalRect::new(0, 0, 4, 3), PhysicalRect::new(4, 0, 4, 3)];
        let sliced = DesktopFrames::from_canvas_bgra(8, 3, canvas.clone(), &rects).unwrap();
        assert_eq!(sliced.count(), 2);
        assert_eq!(sliced.pixel_rgba(1, 1), Some((10, 10, 10, 255)));
        assert_eq!(sliced.pixel_rgba(6, 2), Some((200, 200, 200, 255)));
        assert!(DesktopFrames::from_canvas_bgra(8, 3, vec![0; 5], &rects).is_err());
        let out_of_range = [PhysicalRect::new(6, 0, 4, 3)];
        assert!(DesktopFrames::from_canvas_bgra(8, 3, canvas.clone(), &out_of_range).is_err());
        let whole = DesktopFrames::from_canvas_bgra(8, 3, canvas, &[PhysicalRect::new(0, 0, 8, 3)])
            .unwrap();
        assert_eq!(whole.count(), 1);
    }

    /// 释放合成缓冲后可重新合成。
    #[test]
    fn composite_can_be_dropped_and_rebuilt() {
        let mut frames = two_side_by_side();
        let _ = frames.base_view();
        frames.drop_composite();
        assert!(!frames.composite_built());
        assert_eq!(frames.base_view().bgra[4 * 4], 200);
    }
}
