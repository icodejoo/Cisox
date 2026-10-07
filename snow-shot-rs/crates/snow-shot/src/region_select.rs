//! 自定义选区的草稿状态机（对应 Qt `ScreenshotOverlayInputHandler` 的折线 / 曲线 / 自由绘制输入部分）。
//!
//! 交互约定（与旧版一致）：
//! - 折线 / 曲线：每次单击追加一个顶点，移动时预览到光标的边，双击闭合完成，Backspace 撤销最后一个顶点；
//! - 自由绘制：按住拖动记录轨迹，松开即完成。
//!
//! 纯逻辑，不接触界面，全部可离屏单测。

use snow_canvas_raster::region::{RegionShape, simplify_points};

/// 顶点数上限（单击类形状）。
const MAX_CLICK_VERTICES: usize = 16_384;

/// 自由绘制轨迹点数上限。
const MAX_FREEHAND_POINTS: usize = 65_532;

/// 与上一个点距离不超过该值视为重复点（像素）。
const DUPLICATE_DISTANCE: f32 = 0.01;

/// 自由绘制每累积这么多原始点就压缩一次，避免松开时一次性简化过长的轨迹。
const FREEHAND_COMPACT_WINDOW: usize = 128;

/// 自由绘制简化容差（逻辑像素，物理像素时除以缩放比）。
const FREEHAND_SIMPLIFY_EPSILON: f32 = 0.125;

/// 选区形状类型（含矩形）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum RegionType {
    /// 矩形（原有选区路径）。
    #[default]
    Rectangle,
    /// 折线。
    Polyline,
    /// 曲线。
    Curve,
    /// 自由绘制。
    Freehand,
}

impl RegionType {
    /// 循环顺序（与 Qt `ScreenshotRegionType` 枚举顺序一致）。
    const CYCLE: [Self; 4] = [Self::Rectangle, Self::Polyline, Self::Curve, Self::Freehand];

    /// 从配置值解析；未知值回落为矩形。
    ///
    /// ```
    /// use snow_shot::region_select::RegionType;
    /// assert_eq!(RegionType::from_config("curve"), RegionType::Curve);
    /// ```
    pub fn from_config(raw: &str) -> Self {
        match raw {
            "polyline" => Self::Polyline,
            "curve" => Self::Curve,
            "freehand" => Self::Freehand,
            _ => Self::Rectangle,
        }
    }

    /// 对应的配置值。
    pub const fn as_config(self) -> &'static str {
        match self {
            Self::Rectangle => "rectangle",
            Self::Polyline => "polyline",
            Self::Curve => "curve",
            Self::Freehand => "freehand",
        }
    }

    /// 循环切换到下一个 / 上一个类型。
    ///
    /// # 参数
    /// - `reverse`：为 `true` 时往回循环。
    pub fn cycled(self, reverse: bool) -> Self {
        let index = Self::CYCLE.iter().position(|t| *t == self).unwrap_or(0);
        let step = if reverse { Self::CYCLE.len() - 1 } else { 1 };
        Self::CYCLE[(index + step) % Self::CYCLE.len()]
    }

    /// 对应的光栅化形状；矩形没有。
    pub const fn shape(self) -> Option<RegionShape> {
        match self {
            Self::Rectangle => None,
            Self::Polyline => Some(RegionShape::Polyline),
            Self::Curve => Some(RegionShape::Curve),
            Self::Freehand => Some(RegionShape::Freehand),
        }
    }
}

/// 两点距离是否大于重复阈值。
fn far_apart(a: (f32, f32), b: (f32, f32)) -> bool {
    (a.0 - b.0).hypot(a.1 - b.1) > DUPLICATE_DISTANCE
}

/// 一次区域草稿：正在构造的折线 / 曲线 / 自由绘制轨迹。
#[derive(Debug, Clone, PartialEq)]
pub struct RegionDraft {
    /// 形状。
    shape: RegionShape,
    /// 已确定的顶点 / 轨迹点（物理像素）。
    points: Vec<(f32, f32)>,
    /// 自由绘制是否正按住鼠标。
    pressed: bool,
    /// 自由绘制已压缩部分的结束位置（其后是原始点）。
    raw_start: usize,
}

impl RegionDraft {
    /// 创建空草稿。
    ///
    /// # 参数
    /// - `shape`：要画的形状。
    pub fn new(shape: RegionShape) -> Self {
        Self {
            shape,
            points: Vec::new(),
            pressed: false,
            raw_start: 0,
        }
    }

    /// 形状。
    pub fn shape(&self) -> RegionShape {
        self.shape
    }

    /// 已确定的点。
    pub fn points(&self) -> &[(f32, f32)] {
        &self.points
    }

    /// 是否还没有任何点。
    pub fn is_empty(&self) -> bool {
        self.points.is_empty()
    }

    /// 自由绘制是否正按住鼠标。
    pub fn pressed(&self) -> bool {
        self.pressed
    }

    /// 按下鼠标：自由绘制从这里起笔（清掉旧轨迹）；折线 / 曲线追加一个顶点。
    ///
    /// # 参数
    /// - `point`：物理像素坐标。
    pub fn press(&mut self, point: (f32, f32)) {
        if self.shape == RegionShape::Freehand {
            self.points.clear();
            self.raw_start = 0;
            self.pressed = true;
        }
        let limit = if self.pressed {
            MAX_FREEHAND_POINTS
        } else {
            MAX_CLICK_VERTICES
        };
        if self.points.len() < limit
            && self
                .points
                .last()
                .is_none_or(|last| far_apart(*last, point))
        {
            self.points.push(point);
        }
    }

    /// 移动鼠标：自由绘制按住时记录轨迹；其余形状无动作（预览由 [`Self::preview`] 提供）。
    ///
    /// # 参数
    /// - `point`：物理像素坐标。
    pub fn drag_to(&mut self, point: (f32, f32)) {
        if !self.pressed || self.points.len() >= MAX_FREEHAND_POINTS {
            return;
        }
        if self
            .points
            .last()
            .is_some_and(|last| !far_apart(*last, point))
        {
            return;
        }
        self.points.push(point);
        // 固定窗口压缩：批量大小不影响最终几何，也避免松开时一次性简化过长的尾巴
        if self.points.len() - self.raw_start >= FREEHAND_COMPACT_WINDOW {
            let tail = simplify_points(&self.points[self.raw_start..], FREEHAND_SIMPLIFY_EPSILON);
            self.points.truncate(self.raw_start);
            self.points.extend(tail);
            self.raw_start = self.points.len().saturating_sub(1);
        }
    }

    /// 松开鼠标：自由绘制收笔并补上终点。
    ///
    /// # 参数
    /// - `point`：物理像素坐标。
    ///
    /// # 返回
    /// 是否由这次松开结束了一笔自由绘制（调用方据此尝试 [`Self::finish`]）。
    pub fn release(&mut self, point: (f32, f32)) -> bool {
        if !self.pressed {
            return false;
        }
        self.drag_to(point);
        if self
            .points
            .last()
            .is_none_or(|last| far_apart(*last, point))
        {
            self.points.push(point);
        }
        self.pressed = false;
        true
    }

    /// 双击：折线 / 曲线把最后一个顶点落在双击位置并交给调用方完成；自由绘制不处理。
    ///
    /// # 参数
    /// - `point`：双击位置。
    ///
    /// # 返回
    /// 是否应当尝试完成（折线 / 曲线为 `true`）。
    pub fn double_click(&mut self, point: (f32, f32)) -> bool {
        if self.shape == RegionShape::Freehand {
            return false;
        }
        // 双击的第一下已经追加过顶点：把它挪到双击点上，没有就补一个
        match self.points.last_mut() {
            Some(last) => *last = point,
            None => self.points.push(point),
        }
        true
    }

    /// 撤销最后一个顶点（自由绘制按住时无效）。
    ///
    /// # 返回
    /// 是否撤销了。
    pub fn remove_last(&mut self) -> bool {
        if self.pressed || self.points.is_empty() {
            return false;
        }
        self.points.pop();
        true
    }

    /// 预览用的顶点：折线 / 曲线把光标位置当作下一个顶点接上，自由绘制只给轨迹本身。
    ///
    /// # 参数
    /// - `pointer`：当前光标；`None` 表示不接。
    pub fn preview(&self, pointer: Option<(f32, f32)>) -> Vec<(f32, f32)> {
        let mut vertices = self.points.clone();
        if self.shape != RegionShape::Freehand
            && let (Some(pointer), Some(last)) = (pointer, self.points.last())
            && far_apart(*last, pointer)
        {
            vertices.push(pointer);
        }
        vertices
    }

    /// 完成草稿：返回最终顶点。点数不足 3 个、简化后不足 3 个，或所有点共线（没有面积）时失败。
    ///
    /// # 参数
    /// - `scale`：显示缩放比，用于自由绘制的简化容差；非正数按 1 处理。
    ///
    /// # 返回
    /// 最终顶点；失败为 `None`（调用方应清掉草稿）。
    ///
    /// ```ignore
    /// let vertices = draft.finish(1.5)?;
    /// ```
    pub fn finish(&self, scale: f32) -> Option<Vec<(f32, f32)>> {
        if self.points.len() < 3 {
            return None;
        }
        let mut vertices = self.points.clone();
        if self.shape == RegionShape::Freehand {
            let scale = if scale > 0.0 { scale } else { 1.0 };
            vertices = simplify_points(&vertices, FREEHAND_SIMPLIFY_EPSILON / scale);
            if vertices.len() < 3 {
                return None;
            }
        }
        has_area(&vertices).then_some(vertices)
    }
}

/// 顶点围成的多边形是否有面积（所有点共线则没有）。
fn has_area(vertices: &[(f32, f32)]) -> bool {
    let Some(first) = vertices.first() else {
        return false;
    };
    vertices.windows(2).any(|pair| {
        let cross = (pair[0].0 - first.0) * (pair[1].1 - first.1)
            - (pair[0].1 - first.1) * (pair[1].0 - first.0);
        cross.abs() > f32::EPSILON
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 类型与配置值互转；未知值回落矩形；循环 4 项并可反向。
    #[test]
    fn region_type_config_and_cycle() {
        for t in RegionType::CYCLE {
            assert_eq!(RegionType::from_config(t.as_config()), t);
        }
        assert_eq!(RegionType::from_config("???"), RegionType::Rectangle);
        assert_eq!(RegionType::Rectangle.cycled(false), RegionType::Polyline);
        assert_eq!(RegionType::Freehand.cycled(false), RegionType::Rectangle);
        assert_eq!(RegionType::Rectangle.cycled(true), RegionType::Freehand);
        assert!(RegionType::Rectangle.shape().is_none());
        assert_eq!(RegionType::Curve.shape(), Some(RegionShape::Curve));
    }

    /// 折线：逐点单击，重复点忽略，预览把光标接成下一条边，双击闭合。
    #[test]
    fn polyline_click_preview_and_double_click() {
        let mut draft = RegionDraft::new(RegionShape::Polyline);
        draft.press((10.0, 10.0));
        draft.press((10.0, 10.0));
        draft.press((60.0, 10.0));
        assert_eq!(draft.points().len(), 2);
        assert_eq!(draft.preview(Some((60.0, 50.0))).len(), 3);
        assert_eq!(draft.preview(None).len(), 2);
        // 双击：第一下已追加顶点 (35,60)，双击把它落在 (36,61)
        draft.press((35.0, 60.0));
        assert!(draft.double_click((36.0, 61.0)));
        assert_eq!(draft.points().last(), Some(&(36.0, 61.0)));
        assert_eq!(draft.finish(1.0).map(|v| v.len()), Some(3));
    }

    /// 点数不足或共线都不能完成。
    #[test]
    fn finish_rejects_too_few_or_collinear() {
        let mut draft = RegionDraft::new(RegionShape::Curve);
        draft.press((0.0, 0.0));
        draft.press((10.0, 10.0));
        assert!(draft.finish(1.0).is_none());
        draft.press((20.0, 20.0));
        assert!(draft.finish(1.0).is_none(), "共线没有面积");
        assert!(draft.remove_last());
        draft.press((20.0, 0.0));
        assert!(draft.finish(1.0).is_some());
    }

    /// Backspace：撤销最后顶点，空了再撤返回 false。
    #[test]
    fn remove_last_vertex() {
        let mut draft = RegionDraft::new(RegionShape::Polyline);
        assert!(!draft.remove_last());
        draft.press((1.0, 1.0));
        draft.press((5.0, 5.0));
        assert!(draft.remove_last());
        assert_eq!(draft.points(), &[(1.0, 1.0)]);
    }

    /// 自由绘制：按下起笔、拖动记录、松开收笔；按住期间不能撤销；双击不处理。
    #[test]
    fn freehand_stroke_press_drag_release() {
        let mut draft = RegionDraft::new(RegionShape::Freehand);
        draft.press((0.0, 0.0));
        assert!(draft.pressed());
        draft.drag_to((40.0, 0.0));
        draft.drag_to((40.0, 40.0));
        assert!(!draft.remove_last());
        assert!(!draft.double_click((1.0, 1.0)));
        assert!(draft.release((0.0, 40.0)));
        assert!(!draft.pressed());
        assert!(!draft.release((0.0, 40.0)), "未按住时松开无效");
        assert_eq!(
            draft.preview(Some((99.0, 99.0))),
            draft.points(),
            "自由绘制不接光标"
        );
        assert!(draft.finish(1.0).is_some());
        // 再次按下会清掉旧轨迹重新起笔
        draft.press((5.0, 5.0));
        assert_eq!(draft.points(), &[(5.0, 5.0)]);
    }

    /// 自由绘制：长轨迹在拖动中分窗口压缩，点数远小于原始点数且首尾保留。
    #[test]
    fn freehand_long_stroke_is_compacted() {
        let mut draft = RegionDraft::new(RegionShape::Freehand);
        draft.press((0.0, 0.0));
        for i in 1..=1000 {
            // 沿一条直线、带微小抖动
            draft.drag_to((i as f32, if i % 2 == 0 { 0.02 } else { 0.0 }));
        }
        draft.release((1000.0, 50.0));
        assert!(
            draft.points().len() < 200,
            "实际 {} 点",
            draft.points().len()
        );
        assert_eq!(draft.points().first(), Some(&(0.0, 0.0)));
        assert_eq!(draft.points().last(), Some(&(1000.0, 50.0)));
    }
}
