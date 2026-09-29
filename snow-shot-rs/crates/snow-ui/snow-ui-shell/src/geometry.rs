//! 几何与 DPI 换算（纯逻辑，无平台依赖）。
//!
//! # 坐标系约定（全 crate 统一）
//! - **屏幕坐标**：虚拟桌面的物理像素，原点在主显示器左上角，X 向右、Y 向下，
//!   副屏在主屏左/上方时为负数。与 Win32 `GetWindowRect` / `MONITORINFO` 一致（要求进程
//!   为 Per-Monitor-V2 DPI 感知，[`crate::ui::run`] 启动时会设置）。
//! - **窗口坐标**：物理像素，原点在窗口外框左上角。无边框覆盖窗下与客户区坐标相同。
//!   [`Region`]（点击穿透区域）使用窗口坐标。
//! - **逻辑像素**：`物理像素 / ScaleFactor`，`ScaleFactor` 取自窗口所在显示器
//!   （100% = 1.0，150% = 1.5）。GPUI 视图内部布局使用逻辑像素。
//! - 逻辑 → 物理取整：矩形按“左右边缘分别四舍五入”，相邻矩形不会出现缝隙或重叠。

/// 屏幕/窗口坐标系下的物理像素点。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct PhysicalPoint {
    /// 横坐标。
    pub x: i32,
    /// 纵坐标。
    pub y: i32,
}

impl PhysicalPoint {
    /// 构造点。
    ///
    /// ```rust
    /// use snow_ui_shell::geometry::PhysicalPoint;
    /// assert_eq!(PhysicalPoint::new(1, 2).x, 1);
    /// ```
    pub const fn new(x: i32, y: i32) -> Self {
        Self { x, y }
    }
}

/// 物理像素矩形（左上角 + 宽高），宽高非正视为空。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct PhysicalRect {
    /// 左边缘。
    pub x: i32,
    /// 上边缘。
    pub y: i32,
    /// 宽度。
    pub width: i32,
    /// 高度。
    pub height: i32,
}

impl PhysicalRect {
    /// 构造矩形。
    ///
    /// ```rust
    /// use snow_ui_shell::geometry::PhysicalRect;
    /// let r = PhysicalRect::new(10, 20, 100, 50);
    /// assert_eq!((r.right(), r.bottom()), (110, 70));
    /// ```
    pub const fn new(x: i32, y: i32, width: i32, height: i32) -> Self {
        Self {
            x,
            y,
            width,
            height,
        }
    }

    /// 右边缘（不含）。
    pub const fn right(&self) -> i32 {
        self.x + self.width
    }

    /// 下边缘（不含）。
    pub const fn bottom(&self) -> i32 {
        self.y + self.height
    }

    /// 是否为空（宽或高非正）。
    pub const fn is_empty(&self) -> bool {
        self.width <= 0 || self.height <= 0
    }

    /// 点是否落在矩形内（左/上含，右/下不含）。
    ///
    /// ```rust
    /// use snow_ui_shell::geometry::{PhysicalPoint, PhysicalRect};
    /// let r = PhysicalRect::new(0, 0, 10, 10);
    /// assert!(r.contains(PhysicalPoint::new(0, 0)));
    /// assert!(!r.contains(PhysicalPoint::new(10, 5)));
    /// ```
    pub const fn contains(&self, p: PhysicalPoint) -> bool {
        !self.is_empty()
            && p.x >= self.x
            && p.x < self.right()
            && p.y >= self.y
            && p.y < self.bottom()
    }

    /// 求交集，无交集返回 `None`。
    ///
    /// ```rust
    /// use snow_ui_shell::geometry::PhysicalRect;
    /// let a = PhysicalRect::new(0, 0, 10, 10);
    /// let b = PhysicalRect::new(5, 5, 10, 10);
    /// assert_eq!(a.intersect(&b), Some(PhysicalRect::new(5, 5, 5, 5)));
    /// ```
    pub fn intersect(&self, other: &PhysicalRect) -> Option<PhysicalRect> {
        let left = self.x.max(other.x);
        let top = self.y.max(other.y);
        let right = self.right().min(other.right());
        let bottom = self.bottom().min(other.bottom());
        (right > left && bottom > top)
            .then(|| PhysicalRect::new(left, top, right - left, bottom - top))
    }

    /// 平移。
    pub const fn translate(&self, dx: i32, dy: i32) -> PhysicalRect {
        PhysicalRect::new(self.x + dx, self.y + dy, self.width, self.height)
    }

    /// 中心点（整数除法向零取整）。
    pub const fn center(&self) -> PhysicalPoint {
        PhysicalPoint::new(self.x + self.width / 2, self.y + self.height / 2)
    }

    /// 与另一矩形的外接矩形；空矩形不参与。
    pub fn union_bounds(&self, other: &PhysicalRect) -> PhysicalRect {
        if self.is_empty() {
            return *other;
        }
        if other.is_empty() {
            return *self;
        }
        let left = self.x.min(other.x);
        let top = self.y.min(other.y);
        PhysicalRect::new(
            left,
            top,
            self.right().max(other.right()) - left,
            self.bottom().max(other.bottom()) - top,
        )
    }
}

/// 逻辑像素矩形（单位与 GPUI 布局一致）。
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct LogicalRect {
    /// 左边缘。
    pub x: f32,
    /// 上边缘。
    pub y: f32,
    /// 宽度。
    pub width: f32,
    /// 高度。
    pub height: f32,
}

/// 逻辑像素尺寸。
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct LogicalSize {
    /// 宽度。
    pub width: f32,
    /// 高度。
    pub height: f32,
}

impl LogicalSize {
    /// 构造尺寸。
    pub const fn new(width: f32, height: f32) -> Self {
        Self { width, height }
    }
}

/// Windows 基准 DPI（100% 缩放）。
pub const BASE_DPI: u32 = 96;

/// DPI 缩放比（物理像素 / 逻辑像素），保证为有限正数。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ScaleFactor(f32);

impl ScaleFactor {
    /// 100% 缩放。
    pub const ONE: ScaleFactor = ScaleFactor(1.0);

    /// 由数值构造；非有限或非正数回退为 1.0。
    ///
    /// ```rust
    /// use snow_ui_shell::geometry::ScaleFactor;
    /// assert_eq!(ScaleFactor::new(1.5).value(), 1.5);
    /// assert_eq!(ScaleFactor::new(f32::NAN).value(), 1.0);
    /// ```
    pub fn new(value: f32) -> Self {
        if value.is_finite() && value > 0.0 {
            Self(value)
        } else {
            Self::ONE
        }
    }

    /// 由显示器 DPI 构造（96 → 1.0，144 → 1.5）。
    ///
    /// ```rust
    /// use snow_ui_shell::geometry::ScaleFactor;
    /// assert_eq!(ScaleFactor::from_dpi(144).value(), 1.5);
    /// ```
    pub fn from_dpi(dpi: u32) -> Self {
        Self::new(dpi as f32 / BASE_DPI as f32)
    }

    /// 取数值。
    pub fn value(self) -> f32 {
        self.0
    }

    /// 逻辑长度 → 物理像素（四舍五入）。
    ///
    /// ```rust
    /// use snow_ui_shell::geometry::ScaleFactor;
    /// assert_eq!(ScaleFactor::new(1.25).to_physical(10.0), 13);
    /// ```
    pub fn to_physical(self, logical: f32) -> i32 {
        (logical * self.0).round() as i32
    }

    /// 物理像素 → 逻辑长度。
    ///
    /// ```rust
    /// use snow_ui_shell::geometry::ScaleFactor;
    /// assert_eq!(ScaleFactor::new(2.0).to_logical(100), 50.0);
    /// ```
    pub fn to_logical(self, physical: i32) -> f32 {
        physical as f32 / self.0
    }

    /// 逻辑矩形 → 物理矩形（左右/上下边缘分别取整，相邻矩形无缝）。
    ///
    /// ```rust
    /// use snow_ui_shell::geometry::{LogicalRect, ScaleFactor};
    /// let r = ScaleFactor::new(1.5)
    ///     .rect_to_physical(LogicalRect { x: 1.0, y: 1.0, width: 3.0, height: 3.0 });
    /// assert_eq!((r.x, r.y, r.width, r.height), (2, 2, 4, 4));
    /// ```
    pub fn rect_to_physical(self, r: LogicalRect) -> PhysicalRect {
        let left = self.to_physical(r.x);
        let top = self.to_physical(r.y);
        let right = self.to_physical(r.x + r.width);
        let bottom = self.to_physical(r.y + r.height);
        PhysicalRect::new(left, top, right - left, bottom - top)
    }

    /// 物理矩形 → 逻辑矩形。
    pub fn rect_to_logical(self, r: PhysicalRect) -> LogicalRect {
        LogicalRect {
            x: self.to_logical(r.x),
            y: self.to_logical(r.y),
            width: self.to_logical(r.width),
            height: self.to_logical(r.height),
        }
    }
}

/// 点击命中区域：若干物理像素矩形的并集，窗口坐标系。
///
/// 覆盖窗只有落在区域内的部分可见且可点击，区域外整体穿透（ADR-2b）。
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Region {
    /// 组成区域的矩形（可相互重叠，均非空）。
    rects: Vec<PhysicalRect>,
}

impl Region {
    /// 空区域（整窗穿透且不可见）。
    pub fn new() -> Self {
        Self::default()
    }

    /// 由单个矩形构造；空矩形得到空区域。
    ///
    /// ```rust
    /// use snow_ui_shell::geometry::{PhysicalRect, Region};
    /// assert!(!Region::from_rect(PhysicalRect::new(0, 0, 5, 5)).is_empty());
    /// ```
    pub fn from_rect(rect: PhysicalRect) -> Self {
        let mut region = Self::new();
        region.union_rect(rect);
        region
    }

    /// 并入一个矩形；空矩形忽略。
    pub fn union_rect(&mut self, rect: PhysicalRect) {
        if !rect.is_empty() {
            self.rects.push(rect);
        }
    }

    /// 减去一个矩形（可能把原矩形切成最多 4 块）。
    ///
    /// ```rust
    /// use snow_ui_shell::geometry::{PhysicalPoint, PhysicalRect, Region};
    /// let mut r = Region::from_rect(PhysicalRect::new(0, 0, 10, 10));
    /// r.subtract_rect(PhysicalRect::new(4, 4, 2, 2));
    /// assert!(!r.contains(PhysicalPoint::new(5, 5)));
    /// assert!(r.contains(PhysicalPoint::new(1, 1)));
    /// ```
    pub fn subtract_rect(&mut self, cut: PhysicalRect) {
        let mut out = Vec::with_capacity(self.rects.len());
        for a in &self.rects {
            let Some(i) = a.intersect(&cut) else {
                out.push(*a);
                continue;
            };
            let pieces = [
                PhysicalRect::new(a.x, a.y, a.width, i.y - a.y),
                PhysicalRect::new(a.x, i.bottom(), a.width, a.bottom() - i.bottom()),
                PhysicalRect::new(a.x, i.y, i.x - a.x, i.height),
                PhysicalRect::new(i.right(), i.y, a.right() - i.right(), i.height),
            ];
            out.extend(pieces.into_iter().filter(|p| !p.is_empty()));
        }
        self.rects = out;
    }

    /// 与一个矩形求交（把区域裁剪到该矩形内）。
    pub fn intersect_rect(&mut self, clip: PhysicalRect) {
        self.rects = self
            .rects
            .iter()
            .filter_map(|r| r.intersect(&clip))
            .collect();
    }

    /// 整体平移。
    pub fn translate(&mut self, dx: i32, dy: i32) {
        for r in &mut self.rects {
            *r = r.translate(dx, dy);
        }
    }

    /// 点是否在区域内。
    pub fn contains(&self, p: PhysicalPoint) -> bool {
        self.rects.iter().any(|r| r.contains(p))
    }

    /// 区域是否为空。
    pub fn is_empty(&self) -> bool {
        self.rects.is_empty()
    }

    /// 外接矩形；空区域返回 `None`。
    pub fn bounds(&self) -> Option<PhysicalRect> {
        let mut it = self.rects.iter();
        let first = *it.next()?;
        Some(it.fold(first, |acc, r| acc.union_bounds(r)))
    }

    /// 组成区域的矩形列表。
    pub fn rects(&self) -> &[PhysicalRect] {
        &self.rects
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 矩形基本运算：边界、交集、外接。
    #[test]
    fn rect_basics() {
        let a = PhysicalRect::new(0, 0, 10, 10);
        assert!(a.contains(PhysicalPoint::new(9, 9)));
        assert!(!a.contains(PhysicalPoint::new(10, 9)));
        assert_eq!(a.intersect(&PhysicalRect::new(10, 0, 5, 5)), None);
        assert_eq!(
            a.union_bounds(&PhysicalRect::new(20, 20, 5, 5)),
            PhysicalRect::new(0, 0, 25, 25)
        );
        assert!(PhysicalRect::new(0, 0, 0, 5).is_empty());
    }

    /// 负坐标（副屏在左上）下的包含判定。
    #[test]
    fn negative_origin_contains() {
        let r = PhysicalRect::new(-1920, -100, 1920, 1080);
        assert!(r.contains(PhysicalPoint::new(-1, 0)));
        assert!(!r.contains(PhysicalPoint::new(0, 0)));
    }

    /// DPI 与缩放换算。
    #[test]
    fn scale_conversions() {
        assert_eq!(ScaleFactor::from_dpi(96), ScaleFactor::ONE);
        assert_eq!(ScaleFactor::from_dpi(192).value(), 2.0);
        assert_eq!(ScaleFactor::new(0.0), ScaleFactor::ONE);
        assert_eq!(ScaleFactor::new(-1.0), ScaleFactor::ONE);
        let s = ScaleFactor::new(1.25);
        assert_eq!(s.to_physical(100.0), 125);
        assert_eq!(s.to_logical(125), 100.0);
    }

    /// 相邻逻辑矩形换算后不留缝、不重叠。
    #[test]
    fn adjacent_rects_have_no_gap() {
        let s = ScaleFactor::new(1.5);
        let rect = |x: f32| LogicalRect {
            x,
            y: 0.0,
            width: 3.0,
            height: 1.0,
        };
        assert_eq!(
            s.rect_to_physical(rect(0.0)).right(),
            s.rect_to_physical(rect(3.0)).x
        );
        let odd = ScaleFactor::new(1.25);
        let unit = |x: f32| LogicalRect {
            x,
            y: 0.0,
            width: 1.0,
            height: 1.0,
        };
        assert_eq!(
            odd.rect_to_physical(unit(0.0)).right(),
            odd.rect_to_physical(unit(1.0)).x
        );
    }

    /// 物理 → 逻辑 → 物理往返在整数倍缩放下无损。
    #[test]
    fn round_trip_integer_scale() {
        let s = ScaleFactor::new(2.0);
        let p = PhysicalRect::new(10, 20, 300, 200);
        assert_eq!(s.rect_to_physical(s.rect_to_logical(p)), p);
    }

    /// 区域减去中心矩形后中心不命中、四周命中。
    #[test]
    fn region_subtract_hole() {
        let mut r = Region::from_rect(PhysicalRect::new(0, 0, 10, 10));
        r.subtract_rect(PhysicalRect::new(3, 3, 4, 4));
        assert!(!r.contains(PhysicalPoint::new(5, 5)));
        assert!(!r.contains(PhysicalPoint::new(3, 3)));
        assert!(r.contains(PhysicalPoint::new(2, 5)));
        assert!(r.contains(PhysicalPoint::new(7, 5)));
        assert!(r.contains(PhysicalPoint::new(5, 2)));
        assert!(r.contains(PhysicalPoint::new(5, 7)));
        assert_eq!(r.bounds(), Some(PhysicalRect::new(0, 0, 10, 10)));
    }

    /// 减去不相交矩形无变化；减去覆盖矩形得到空区域。
    #[test]
    fn region_subtract_edges() {
        let mut r = Region::from_rect(PhysicalRect::new(0, 0, 10, 10));
        r.subtract_rect(PhysicalRect::new(20, 20, 5, 5));
        assert_eq!(r.rects().len(), 1);
        r.subtract_rect(PhysicalRect::new(-5, -5, 30, 30));
        assert!(r.is_empty());
        assert_eq!(r.bounds(), None);
    }

    /// 并集、裁剪与平移。
    #[test]
    fn region_union_clip_translate() {
        let mut r = Region::from_rect(PhysicalRect::new(0, 0, 4, 4));
        r.union_rect(PhysicalRect::new(10, 10, 4, 4));
        r.union_rect(PhysicalRect::new(0, 0, 0, 0));
        assert_eq!(r.rects().len(), 2);
        r.intersect_rect(PhysicalRect::new(0, 0, 12, 12));
        assert!(r.contains(PhysicalPoint::new(11, 11)));
        assert!(!r.contains(PhysicalPoint::new(12, 12)));
        r.translate(5, 5);
        assert!(r.contains(PhysicalPoint::new(5, 5)));
        assert!(!r.contains(PhysicalPoint::new(4, 4)));
    }
}
