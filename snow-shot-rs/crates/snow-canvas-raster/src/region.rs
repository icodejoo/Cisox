//! 自定义选区的栅格蒙版：折线 / 曲线 / 自由绘制的形状，以及加、减区域的合成。
//!
//! 选区形状用与底图同尺寸的 8 位 alpha 蒙版表示（覆盖率 0~255）：画一个形状就是把它抗锯齿地
//! 光栅化后并入 / 挖出蒙版。形状的曲线部分复用引擎的 Catmull-Rom 路径命令，
//! 填充规则与旧版一致（奇偶填充）。不依赖 gpui，全部可离屏单测。
//!
//! # 用法
//! ```
//! use snow_canvas_raster::region::{RegionMask, RegionOp, RegionShape, shape_commands};
//!
//! let mut mask = RegionMask::new(100, 80);
//! let triangle = [(10.0, 10.0), (60.0, 10.0), (35.0, 60.0)];
//! mask.apply(&shape_commands(RegionShape::Polyline, &triangle), RegionOp::Add);
//! assert!(mask.contains(35, 25));
//! assert!(!mask.contains(80, 70));
//! ```

pub use snow_draw_engine_core::PathCommand;
use snow_draw_engine_core::{Point, catmull_rom_path_commands};
use tiny_skia::{FillRule, Paint, PathBuilder, Pixmap, Transform};

/// 每像素通道数（RGBA）。
const CHANNELS: usize = 4;

/// 覆盖率达到该值才算「在区域内」（半透明抗锯齿边缘按最近处理）。
const INSIDE_THRESHOLD: u8 = 128;

/// 不透明 alpha。
const OPAQUE: u32 = 255;

/// 自定义选区的形状类型（不含矩形，矩形走原有选区路径）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RegionShape {
    /// 折线：顶点之间用直线连接并自动闭合。
    Polyline,
    /// 曲线：Catmull-Rom 样条穿过所有顶点并自动闭合。
    Curve,
    /// 自由绘制：鼠标轨迹点，按曲线平滑后闭合。
    Freehand,
}

/// 把一个形状并入或挖出蒙版的方式。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RegionOp {
    /// 并入（加区域）。
    Add,
    /// 挖出（减区域）。
    Subtract,
}

/// 由顶点生成形状的路径命令。
///
/// # 参数
/// - `shape`：形状类型。
/// - `points`：顶点（像素坐标）。折线 / 曲线是用户点的顶点，自由绘制是鼠标轨迹。
///
/// # 返回
/// 路径命令；少于 1 个点时为空。折线只含直线，曲线与自由绘制在 3 个点以上时按闭合样条平滑。
///
/// ```
/// use snow_canvas_raster::region::{RegionShape, shape_commands};
/// assert!(shape_commands(RegionShape::Polyline, &[]).is_empty());
/// ```
pub fn shape_commands(shape: RegionShape, points: &[(f32, f32)]) -> Vec<PathCommand> {
    if points.is_empty() {
        return Vec::new();
    }
    match shape {
        RegionShape::Polyline => {
            let mut commands = Vec::with_capacity(points.len());
            commands.push(PathCommand::MoveTo {
                point: [f64::from(points[0].0), f64::from(points[0].1)],
            });
            for (x, y) in &points[1..] {
                commands.push(PathCommand::LineTo {
                    point: [f64::from(*x), f64::from(*y)],
                });
            }
            commands
        }
        RegionShape::Curve | RegionShape::Freehand => {
            let vertices: Vec<Point<f64>> = points
                .iter()
                .map(|(x, y)| Point::new(f64::from(*x), f64::from(*y)))
                .collect();
            catmull_rom_path_commands(&vertices, &[], vertices.len() >= 3)
        }
    }
}

/// 三次贝塞尔曲线展平时每段的采样步数。
const CUBIC_FLATTEN_STEPS: usize = 16;

/// 把路径命令展平成折线顶点（曲线按固定步数采样），供草稿描边等矢量绘制使用。
///
/// # 参数
/// - `commands`：路径命令。
///
/// # 返回
/// 折线顶点；没有命令时为空。
///
/// ```
/// use snow_canvas_raster::region::{RegionShape, flatten_commands, shape_commands};
/// let pts = [(0.0, 0.0), (10.0, 0.0), (10.0, 10.0)];
/// let flat = flatten_commands(&shape_commands(RegionShape::Polyline, &pts));
/// assert_eq!(flat.len(), 3);
/// ```
pub fn flatten_commands(commands: &[PathCommand]) -> Vec<(f32, f32)> {
    let mut out: Vec<(f32, f32)> = Vec::with_capacity(commands.len());
    for command in commands {
        match *command {
            PathCommand::MoveTo { point } | PathCommand::LineTo { point } => {
                out.push((point[0] as f32, point[1] as f32));
            }
            PathCommand::QuadTo { control, end } => {
                let start = out
                    .last()
                    .copied()
                    .unwrap_or((end[0] as f32, end[1] as f32));
                for step in 1..=CUBIC_FLATTEN_STEPS {
                    let t = step as f32 / CUBIC_FLATTEN_STEPS as f32;
                    let u = 1.0 - t;
                    out.push((
                        u * u * start.0 + 2.0 * u * t * control[0] as f32 + t * t * end[0] as f32,
                        u * u * start.1 + 2.0 * u * t * control[1] as f32 + t * t * end[1] as f32,
                    ));
                }
            }
            PathCommand::CubicTo {
                control_1,
                control_2,
                end,
            } => {
                let start = out
                    .last()
                    .copied()
                    .unwrap_or((end[0] as f32, end[1] as f32));
                for step in 1..=CUBIC_FLATTEN_STEPS {
                    let t = step as f32 / CUBIC_FLATTEN_STEPS as f32;
                    let u = 1.0 - t;
                    let (b0, b1, b2, b3) = (u * u * u, 3.0 * u * u * t, 3.0 * u * t * t, t * t * t);
                    out.push((
                        b0 * start.0
                            + b1 * control_1[0] as f32
                            + b2 * control_2[0] as f32
                            + b3 * end[0] as f32,
                        b0 * start.1
                            + b1 * control_1[1] as f32
                            + b2 * control_2[1] as f32
                            + b3 * end[1] as f32,
                    ));
                }
            }
        }
    }
    out
}

/// 点到线段距离的平方。
fn segment_distance_squared(p: (f32, f32), a: (f32, f32), b: (f32, f32)) -> f32 {
    let (dx, dy) = (b.0 - a.0, b.1 - a.1);
    let length_squared = dx * dx + dy * dy;
    let t = if length_squared <= f32::EPSILON {
        0.0
    } else {
        (((p.0 - a.0) * dx + (p.1 - a.1) * dy) / length_squared).clamp(0.0, 1.0)
    };
    let (px, py) = (p.0 - (a.0 + t * dx), p.1 - (a.1 + t * dy));
    px * px + py * py
}

/// Ramer–Douglas–Peucker 折线简化：去掉偏离不超过 `epsilon` 的中间点，保留首尾。
///
/// # 参数
/// - `points`：折线顶点。
/// - `epsilon`：容差（像素）；非正或非有限时原样返回。
///
/// # 返回
/// 简化后的顶点（保持顺序）。
///
/// ```
/// use snow_canvas_raster::region::simplify_points;
/// let line = [(0.0, 0.0), (5.0, 0.01), (10.0, 0.0)];
/// assert_eq!(simplify_points(&line, 0.5), vec![(0.0, 0.0), (10.0, 0.0)]);
/// ```
pub fn simplify_points(points: &[(f32, f32)], epsilon: f32) -> Vec<(f32, f32)> {
    if points.len() <= 2 || !epsilon.is_finite() || epsilon <= 0.0 {
        return points.to_vec();
    }
    let threshold = epsilon * epsilon;
    let mut keep = vec![false; points.len()];
    keep[0] = true;
    keep[points.len() - 1] = true;
    let mut ranges = vec![(0usize, points.len() - 1)];
    while let Some((start, end)) = ranges.pop() {
        if end <= start + 1 {
            continue;
        }
        let (mut far_index, mut far_distance) = (start, threshold);
        for index in start + 1..end {
            let distance = segment_distance_squared(points[index], points[start], points[end]);
            if distance > far_distance {
                (far_index, far_distance) = (index, distance);
            }
        }
        if far_index != start {
            keep[far_index] = true;
            ranges.push((start, far_index));
            ranges.push((far_index, end));
        }
    }
    points
        .iter()
        .zip(keep)
        .filter_map(|(point, kept)| kept.then_some(*point))
        .collect()
}

/// 与底图同尺寸的选区 alpha 蒙版（行优先，每像素 1 字节覆盖率）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RegionMask {
    /// 蒙版宽（像素）。
    width: u32,
    /// 蒙版高（像素）。
    height: u32,
    /// 覆盖率，长度 `width * height`。
    alpha: Vec<u8>,
}

impl RegionMask {
    /// 创建全空蒙版。
    ///
    /// # 参数
    /// - `width` / `height`：蒙版尺寸（像素）。
    pub fn new(width: u32, height: u32) -> Self {
        Self {
            width,
            height,
            alpha: vec![0; width as usize * height as usize],
        }
    }

    /// 由矩形创建蒙版（矩形外为空，越界部分被裁掉）。
    ///
    /// # 参数
    /// - `width` / `height`：蒙版尺寸。
    /// - `(x, y, w, h)`：矩形。
    pub fn from_rect(width: u32, height: u32, (x, y, w, h): (i32, i32, i32, i32)) -> Self {
        let mut mask = Self::new(width, height);
        let left = x.clamp(0, width as i32);
        let top = y.clamp(0, height as i32);
        let right = x.saturating_add(w).clamp(0, width as i32);
        let bottom = y.saturating_add(h).clamp(0, height as i32);
        for row in top..bottom {
            let start = row as usize * width as usize + left as usize;
            mask.alpha[start..start + (right - left).max(0) as usize].fill(u8::MAX);
        }
        mask
    }

    /// 蒙版尺寸 `(宽, 高)`。
    pub fn size(&self) -> (u32, u32) {
        (self.width, self.height)
    }

    /// 某像素的覆盖率；越界为 0。
    pub fn alpha_at(&self, x: i32, y: i32) -> u8 {
        if x < 0 || y < 0 || x >= self.width as i32 || y >= self.height as i32 {
            return 0;
        }
        self.alpha[y as usize * self.width as usize + x as usize]
    }

    /// 某像素是否在区域内（覆盖率过半）。
    pub fn contains(&self, x: i32, y: i32) -> bool {
        self.alpha_at(x, y) >= INSIDE_THRESHOLD
    }

    /// 覆盖率非零部分的外接矩形 `(x, y, 宽, 高)`；蒙版全空时为 `None`。
    pub fn bounds(&self) -> Option<(i32, i32, i32, i32)> {
        let width = self.width as usize;
        let (mut min_x, mut min_y, mut max_x, mut max_y) = (usize::MAX, usize::MAX, 0, 0);
        for (row, line) in self.alpha.chunks_exact(width.max(1)).enumerate() {
            let Some(first) = line.iter().position(|a| *a > 0) else {
                continue;
            };
            let last = line.iter().rposition(|a| *a > 0).unwrap_or(first);
            min_x = min_x.min(first);
            max_x = max_x.max(last);
            min_y = min_y.min(row);
            max_y = max_y.max(row);
        }
        (min_x != usize::MAX).then(|| {
            (
                min_x as i32,
                min_y as i32,
                (max_x - min_x + 1) as i32,
                (max_y - min_y + 1) as i32,
            )
        })
    }

    /// 蒙版是否全空。
    pub fn is_empty(&self) -> bool {
        self.alpha.iter().all(|a| *a == 0)
    }

    /// 把路径形状并入或挖出蒙版（奇偶填充、抗锯齿）。
    ///
    /// # 参数
    /// - `commands`：形状的路径命令（见 [`shape_commands`]）；少于 3 个有效点时不产生面积，不改变蒙版。
    /// - `op`：并入或挖出。
    ///
    /// # 返回
    /// 蒙版是否发生了变化。
    ///
    /// ```
    /// use snow_canvas_raster::region::{RegionMask, RegionOp, RegionShape, shape_commands};
    /// let mut mask = RegionMask::new(40, 40);
    /// let square = [(5.0, 5.0), (30.0, 5.0), (30.0, 30.0), (5.0, 30.0)];
    /// assert!(mask.apply(&shape_commands(RegionShape::Polyline, &square), RegionOp::Add));
    /// ```
    pub fn apply(&mut self, commands: &[PathCommand], op: RegionOp) -> bool {
        let Some(path) = build_path(commands) else {
            return false;
        };
        let bounds = path.bounds();
        let left = (bounds.left().floor() as i32 - 1).clamp(0, self.width as i32);
        let top = (bounds.top().floor() as i32 - 1).clamp(0, self.height as i32);
        let right = (bounds.right().ceil() as i32 + 1).clamp(0, self.width as i32);
        let bottom = (bounds.bottom().ceil() as i32 + 1).clamp(0, self.height as i32);
        let (w, h) = ((right - left) as u32, (bottom - top) as u32);
        let Some(mut pixmap) = (w > 0 && h > 0).then(|| Pixmap::new(w, h)).flatten() else {
            return false;
        };
        let mut paint = Paint::default();
        paint.set_color_rgba8(0, 0, 0, u8::MAX);
        paint.anti_alias = true;
        pixmap.fill_path(
            &path,
            &paint,
            FillRule::EvenOdd,
            Transform::from_translate(-(left as f32), -(top as f32)),
            None,
        );
        let before = self.alpha.clone();
        for row in 0..h as usize {
            for col in 0..w as usize {
                let coverage = u32::from(pixmap.data()[(row * w as usize + col) * CHANNELS + 3]);
                if coverage == 0 {
                    continue;
                }
                let index = (top as usize + row) * self.width as usize + left as usize + col;
                let current = u32::from(self.alpha[index]);
                self.alpha[index] = match op {
                    // 并集：a + b·(1 − a)
                    RegionOp::Add => (current + coverage * (OPAQUE - current) / OPAQUE) as u8,
                    // 差集：a·(1 − b)
                    RegionOp::Subtract => (current * (OPAQUE - coverage) / OPAQUE) as u8,
                };
            }
        }
        before != self.alpha
    }

    /// 平移整个区域（越出蒙版的部分被裁掉）。
    ///
    /// # 参数
    /// - `dx` / `dy`：像素位移。
    ///
    /// # 返回
    /// 平移后的新蒙版。
    pub fn translated(&self, dx: i32, dy: i32) -> Self {
        let mut out = Self::new(self.width, self.height);
        let width = self.width as i32;
        for y in 0..self.height as i32 {
            let ty = y + dy;
            if ty < 0 || ty >= self.height as i32 {
                continue;
            }
            let src_start = (-dx).max(0);
            let src_end = (width - dx).min(width);
            if src_end <= src_start {
                continue;
            }
            let src = y as usize * self.width as usize;
            let dst = ty as usize * self.width as usize;
            let len = (src_end - src_start) as usize;
            out.alpha[dst + (src_start + dx) as usize..dst + (src_start + dx) as usize + len]
                .copy_from_slice(
                    &self.alpha[src + src_start as usize..src + src_start as usize + len],
                );
        }
        out
    }

    /// 把区域覆盖率乘到一块 RGBA（非预乘）像素的 alpha 通道上，区域外变透明（导出用）。
    ///
    /// # 参数
    /// - `(x, y, w, h)`：这块像素对应的蒙版范围。
    /// - `rgba`：长度须为 `w * h * 4`，被就地修改。
    ///
    /// ```
    /// use snow_canvas_raster::region::RegionMask;
    /// let mask = RegionMask::from_rect(4, 4, (0, 0, 2, 4));
    /// let mut px = vec![255u8; 4 * 4 * 4];
    /// mask.apply_to_rgba((0, 0, 4, 4), &mut px);
    /// assert_eq!(px[3], 255);
    /// assert_eq!(px[(3) * 4 + 3], 0);
    /// ```
    pub fn apply_to_rgba(&self, (x, y, w, h): (i32, i32, i32, i32), rgba: &mut [u8]) {
        if w <= 0 || h <= 0 || rgba.len() != w as usize * h as usize * CHANNELS {
            return;
        }
        for row in 0..h {
            for col in 0..w {
                let index = (row as usize * w as usize + col as usize) * CHANNELS + 3;
                let coverage = u32::from(self.alpha_at(x + col, y + row));
                rgba[index] = (u32::from(rgba[index]) * coverage / OPAQUE) as u8;
            }
        }
    }

    /// 只生成某矩形范围内的选区遮罩图：区域外压暗，区域内透明，可选在区域边缘描一圈轮廓。
    /// 矩形外交给调用方用整块色块处理，省内存。
    ///
    /// # 参数
    /// - `(x, y, w, h)`：矩形范围，通常是区域外接矩形。
    /// - `dim_alpha`：区域外的不透明度。
    /// - `edge`：轮廓 `(B, G, R, 线宽像素)`；`None` 不描边。轮廓画在区域内侧边缘。
    ///
    /// # 返回
    /// 长度 `w * h * 4` 的预乘 BGRA；宽高非正时为空。
    ///
    /// ```
    /// use snow_canvas_raster::region::RegionMask;
    /// let mask = RegionMask::from_rect(8, 8, (2, 2, 4, 4));
    /// let px = mask.overlay_bgra_in((1, 1, 6, 6), 100, None);
    /// assert_eq!(px.len(), 6 * 6 * 4);
    /// assert_eq!(px[3], 100); // (1,1) 在区域外
    /// ```
    pub fn overlay_bgra_in(
        &self,
        (x, y, w, h): (i32, i32, i32, i32),
        dim_alpha: u8,
        edge: Option<(u8, u8, u8, i32)>,
    ) -> Vec<u8> {
        if w <= 0 || h <= 0 {
            return Vec::new();
        }
        let dim = u32::from(dim_alpha);
        let mut out = vec![0u8; w as usize * h as usize * CHANNELS];
        for row in 0..h {
            for col in 0..w {
                let (px, py) = (x + col, y + row);
                let coverage = u32::from(self.alpha_at(px, py));
                let index = (row as usize * w as usize + col as usize) * CHANNELS;
                if let Some((b, g, r, width)) = edge
                    && self.is_edge_pixel(px, py, width)
                {
                    out[index..index + CHANNELS].copy_from_slice(&[b, g, r, u8::MAX]);
                    continue;
                }
                out[index + 3] = (dim * (OPAQUE - coverage) / OPAQUE) as u8;
            }
        }
        out
    }

    /// 像素是否在区域内侧边缘：自身在区域内，且上下左右 `width` 像素内有区域外的点。
    fn is_edge_pixel(&self, x: i32, y: i32, width: i32) -> bool {
        self.contains(x, y)
            && (1..=width.max(1)).any(|k| {
                !self.contains(x - k, y)
                    || !self.contains(x + k, y)
                    || !self.contains(x, y - k)
                    || !self.contains(x, y + k)
            })
    }

    /// 生成「区域外压暗」的遮罩图：预乘 BGRA，区域内透明，区域外是半透明黑。
    ///
    /// # 参数
    /// - `dim_alpha`：区域外的不透明度（0~255）。
    ///
    /// # 返回
    /// 长度 `width * height * 4` 的像素。
    pub fn dim_overlay_bgra(&self, dim_alpha: u8) -> Vec<u8> {
        let dim = u32::from(dim_alpha);
        let mut out = vec![0u8; self.alpha.len() * CHANNELS];
        for (pixel, coverage) in out.chunks_exact_mut(CHANNELS).zip(&self.alpha) {
            // 预乘黑色：只有 alpha 通道有值
            pixel[3] = (dim * (OPAQUE - u32::from(*coverage)) / OPAQUE) as u8;
        }
        out
    }
}

/// 把引擎路径命令转成 tiny-skia 路径；点数不足以围成面积时返回 `None`。
fn build_path(commands: &[PathCommand]) -> Option<tiny_skia::Path> {
    let mut builder = PathBuilder::new();
    let mut points = 0usize;
    for command in commands {
        match *command {
            PathCommand::MoveTo { point } => {
                builder.move_to(point[0] as f32, point[1] as f32);
                points += 1;
            }
            PathCommand::LineTo { point } => {
                builder.line_to(point[0] as f32, point[1] as f32);
                points += 1;
            }
            PathCommand::QuadTo { control, end } => {
                builder.quad_to(
                    control[0] as f32,
                    control[1] as f32,
                    end[0] as f32,
                    end[1] as f32,
                );
                points += 1;
            }
            PathCommand::CubicTo {
                control_1,
                control_2,
                end,
            } => {
                builder.cubic_to(
                    control_1[0] as f32,
                    control_1[1] as f32,
                    control_2[0] as f32,
                    control_2[1] as f32,
                    end[0] as f32,
                    end[1] as f32,
                );
                points += 1;
            }
        }
    }
    if points < 3 {
        return None;
    }
    builder.close();
    builder.finish()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 正方形顶点。
    fn square(x: f32, y: f32, size: f32) -> [(f32, f32); 4] {
        [(x, y), (x + size, y), (x + size, y + size), (x, y + size)]
    }

    /// 折线正方形：内部在区域内，外部不在，外接矩形贴合。
    #[test]
    fn polyline_square_fills_inside_only() {
        let mut mask = RegionMask::new(100, 80);
        assert!(mask.apply(
            &shape_commands(RegionShape::Polyline, &square(10.0, 20.0, 30.0)),
            RegionOp::Add
        ));
        assert!(mask.contains(25, 35));
        assert!(!mask.contains(5, 35));
        assert!(!mask.contains(50, 35));
        let (x, y, w, h) = mask.bounds().unwrap();
        assert!(
            (x - 10).abs() <= 1
                && (y - 20).abs() <= 1
                && (w - 30).abs() <= 2
                && (h - 30).abs() <= 2
        );
    }

    /// 少于 3 个点的线段没有面积，不改变蒙版。
    #[test]
    fn degenerate_shapes_change_nothing() {
        let mut mask = RegionMask::new(50, 50);
        assert!(!mask.apply(
            &shape_commands(RegionShape::Polyline, &[(1.0, 1.0), (20.0, 20.0)]),
            RegionOp::Add
        ));
        assert!(!mask.apply(&[], RegionOp::Add));
        assert!(mask.is_empty());
        assert!(mask.bounds().is_none());
    }

    /// 加区域是并集；减区域在中间挖洞。
    #[test]
    fn add_unions_and_subtract_cuts_a_hole() {
        let mut mask = RegionMask::new(120, 80);
        mask.apply(
            &shape_commands(RegionShape::Polyline, &square(10.0, 10.0, 40.0)),
            RegionOp::Add,
        );
        mask.apply(
            &shape_commands(RegionShape::Polyline, &square(40.0, 10.0, 40.0)),
            RegionOp::Add,
        );
        assert!(mask.contains(20, 30) && mask.contains(70, 30) && mask.contains(45, 30));
        mask.apply(
            &shape_commands(RegionShape::Polyline, &square(30.0, 20.0, 30.0)),
            RegionOp::Subtract,
        );
        assert!(!mask.contains(45, 30), "中间被挖空");
        assert!(mask.contains(20, 30) && mask.contains(70, 30));
    }

    /// 曲线与自由绘制：闭合样条围出的面积包含中心点。
    #[test]
    fn curve_and_freehand_enclose_area() {
        let ring = [(50.0, 10.0), (90.0, 40.0), (50.0, 70.0), (10.0, 40.0)];
        for shape in [RegionShape::Curve, RegionShape::Freehand] {
            let mut mask = RegionMask::new(100, 80);
            assert!(mask.apply(&shape_commands(shape, &ring), RegionOp::Add));
            assert!(mask.contains(50, 40), "{shape:?} 中心应在区域内");
            assert!(!mask.contains(2, 2));
        }
    }

    /// 平移：区域整体移动，移出画布的部分被裁掉。
    #[test]
    fn translate_moves_and_clips() {
        let mask = RegionMask::from_rect(20, 20, (2, 2, 6, 6));
        let moved = mask.translated(5, 3);
        assert!(moved.contains(8, 6) && !moved.contains(3, 3));
        let (x, y, w, h) = moved.bounds().unwrap();
        assert_eq!((x, y, w, h), (7, 5, 6, 6));
        let clipped = mask.translated(17, 0);
        assert_eq!(clipped.bounds(), Some((19, 2, 1, 6)));
        assert!(mask.translated(40, 0).is_empty());
    }

    /// 导出：区域外 alpha 归零，区域内保持；遮罩图区域外有暗度、区域内透明。
    #[test]
    fn export_alpha_and_dim_overlay() {
        let mask = RegionMask::from_rect(4, 2, (0, 0, 2, 2));
        let mut rgba = vec![200u8; 4 * 2 * 4];
        mask.apply_to_rgba((0, 0, 4, 2), &mut rgba);
        assert_eq!(rgba[3], 200);
        assert_eq!(rgba[2 * 4 + 3], 0);
        let overlay = mask.dim_overlay_bgra(160);
        assert_eq!(overlay[3], 0);
        assert_eq!(overlay[2 * 4 + 3], 160);
        // 长度不符时不改动
        let mut wrong = vec![9u8; 3];
        mask.apply_to_rgba((0, 0, 4, 2), &mut wrong);
        assert_eq!(wrong, vec![9, 9, 9]);
    }

    /// 展平：折线原样输出顶点，曲线按步数采样且首尾落在原顶点上。
    #[test]
    fn flatten_samples_curves_through_vertices() {
        let ring = [(50.0, 10.0), (90.0, 40.0), (50.0, 70.0), (10.0, 40.0)];
        let flat = flatten_commands(&shape_commands(RegionShape::Curve, &ring));
        assert_eq!(flat.first(), Some(&(50.0, 10.0)));
        // 闭合样条 4 条边，每条 16 个采样点，外加起点
        assert_eq!(flat.len(), 1 + 4 * CUBIC_FLATTEN_STEPS);
        assert!(
            flat.iter()
                .any(|p| (p.0 - 90.0).abs() < 0.01 && (p.1 - 40.0).abs() < 0.01)
        );
        assert!(flatten_commands(&[]).is_empty());
    }

    /// 范围遮罩图：区域内透明、区域外压暗，与整图遮罩一致。
    #[test]
    fn dim_overlay_in_rect_matches_full_overlay() {
        let mask = RegionMask::from_rect(8, 8, (2, 2, 4, 4));
        let full = mask.dim_overlay_bgra(90);
        let part = mask.overlay_bgra_in((1, 1, 6, 6), 90, None);
        for row in 0..6usize {
            for col in 0..6usize {
                let a = part[(row * 6 + col) * 4 + 3];
                let b = full[((row + 1) * 8 + col + 1) * 4 + 3];
                assert_eq!(a, b);
            }
        }
        assert!(mask.overlay_bgra_in((0, 0, 0, 3), 90, None).is_empty());
    }

    /// 描边：区域内侧边缘一圈是轮廓色且不透明，内部仍透明，外部压暗。
    #[test]
    fn overlay_draws_inner_edge() {
        let mask = RegionMask::from_rect(10, 10, (2, 2, 6, 6));
        let px = mask.overlay_bgra_in((0, 0, 10, 10), 100, Some((1, 2, 3, 1)));
        let at = |x: usize, y: usize| &px[(y * 10 + x) * 4..(y * 10 + x) * 4 + 4];
        assert_eq!(at(2, 2), &[1, 2, 3, 255], "角点是轮廓");
        assert_eq!(at(2, 5), &[1, 2, 3, 255], "边上是轮廓");
        assert_eq!(at(4, 4), &[0, 0, 0, 0], "内部透明");
        assert_eq!(at(1, 1), &[0, 0, 0, 100], "外部压暗");
    }

    /// 简化：共线点被去掉，拐点保留，首尾始终保留。
    #[test]
    fn simplify_keeps_corners() {
        let pts = [(0.0, 0.0), (5.0, 0.0), (10.0, 0.0), (10.0, 10.0)];
        assert_eq!(
            simplify_points(&pts, 0.5),
            vec![(0.0, 0.0), (10.0, 0.0), (10.0, 10.0)]
        );
        assert_eq!(simplify_points(&pts, 0.0), pts.to_vec());
        assert_eq!(simplify_points(&pts[..2], 1.0), pts[..2].to_vec());
    }

    /// 矩形蒙版越界被裁，外接矩形等于裁后范围。
    #[test]
    fn from_rect_clips_to_canvas() {
        let mask = RegionMask::from_rect(10, 10, (-5, 5, 8, 20));
        assert_eq!(mask.bounds(), Some((0, 5, 3, 5)));
    }
}
