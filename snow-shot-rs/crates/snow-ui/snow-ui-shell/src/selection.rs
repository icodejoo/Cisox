//! 选区几何计算与交互拖拽状态模型。
//!
//! 提供矩形选区规范化、橡皮筋框选（Marquee）、八向手柄命中判定、
//! 边界钳制、宽高比锁定以及交互拖拽几何计算。

use crate::geometry::{PhysicalPoint, PhysicalRect};

/// 选区拖拽模式。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum SelectionDragMode {
    /// 无拖拽或在选区外。
    #[default]
    None,
    /// 整体移动选区。
    All,
    /// 左上手柄。
    TopLeft,
    /// 顶边手柄。
    Top,
    /// 右上手柄。
    TopRight,
    /// 右边手柄。
    Right,
    /// 右下手柄。
    BottomRight,
    /// 底边手柄。
    Bottom,
    /// 左下手柄。
    BottomLeft,
    /// 左边手柄。
    Left,
    /// 橡皮筋框选创建选区。
    Marquee,
}

impl SelectionDragMode {
    /// 是否为调整手柄或边框拖拽。
    ///
    /// # 返回
    /// 若为八个方向手柄之一则返回 `true`。
    ///
    /// # 示例
    /// ```rust
    /// use snow_ui_shell::selection::SelectionDragMode;
    /// assert!(SelectionDragMode::TopLeft.is_resize());
    /// assert!(!SelectionDragMode::All.is_resize());
    /// ```
    pub const fn is_resize(&self) -> bool {
        matches!(
            self,
            Self::TopLeft
                | Self::Top
                | Self::TopRight
                | Self::Right
                | Self::BottomRight
                | Self::Bottom
                | Self::BottomLeft
                | Self::Left
        )
    }

    /// 水平方向拖拽分量（-1 表示向左，0 表示居中，1 表示向右）。
    ///
    /// # 返回
    /// 水平分量。
    ///
    /// # 示例
    /// ```rust
    /// use snow_ui_shell::selection::SelectionDragMode;
    /// assert_eq!(SelectionDragMode::Left.horizontal_direction(), -1);
    /// assert_eq!(SelectionDragMode::Top.horizontal_direction(), 0);
    /// assert_eq!(SelectionDragMode::Right.horizontal_direction(), 1);
    /// ```
    pub const fn horizontal_direction(&self) -> i32 {
        match self {
            Self::TopLeft | Self::BottomLeft | Self::Left => -1,
            Self::TopRight | Self::BottomRight | Self::Right => 1,
            _ => 0,
        }
    }

    /// 垂直方向拖拽分量（-1 表示向上，0 表示居中，1 表示向下）。
    ///
    /// # 返回
    /// 垂直分量。
    ///
    /// # 示例
    /// ```rust
    /// use snow_ui_shell::selection::SelectionDragMode;
    /// assert_eq!(SelectionDragMode::Top.vertical_direction(), -1);
    /// assert_eq!(SelectionDragMode::Left.vertical_direction(), 0);
    /// assert_eq!(SelectionDragMode::Bottom.vertical_direction(), 1);
    /// ```
    pub const fn vertical_direction(&self) -> i32 {
        match self {
            Self::TopLeft | Self::TopRight | Self::Top => -1,
            Self::BottomRight | Self::BottomLeft | Self::Bottom => 1,
            _ => 0,
        }
    }
}

/// 默认手柄尺寸（物理像素）。
pub const DEFAULT_HANDLE_SIZE: i32 = 8;
/// 默认边缘命中容差（物理像素）。
pub const DEFAULT_EDGE_TOLERANCE: i32 = 6;
/// 默认最小选区尺寸（物理像素）。
pub const DEFAULT_MINIMUM_SELECTION_SIZE: i32 = 8;

/// 计算两个端点之间的橡皮筋矩形。
///
/// 包含起点与终点所在像素。当起点与终点重合时返回空矩形。
///
/// # 参数
/// - `start`: 起始点。
/// - `end`: 终止点。
///
/// # 返回
/// 规范化后的物理矩形。
///
/// # 示例
/// ```rust
/// use snow_ui_shell::geometry::PhysicalPoint;
/// use snow_ui_shell::selection::marquee_selection_rect;
/// let r = marquee_selection_rect(PhysicalPoint::new(10, 20), PhysicalPoint::new(30, 40));
/// assert_eq!((r.width, r.height), (21, 21));
/// ```
pub fn marquee_selection_rect(start: PhysicalPoint, end: PhysicalPoint) -> PhysicalRect {
    if start == end {
        return PhysicalRect::new(start.x, start.y, 0, 0);
    }
    let left = start.x.min(end.x);
    let top = start.y.min(end.y);
    let right = start.x.max(end.x) + 1;
    let bottom = start.y.max(end.y) + 1;
    PhysicalRect::new(left, top, right - left, bottom - top)
}

/// 测试点落在选区的哪个拖拽区域。
///
/// # 参数
/// - `selection`: 当前选区。
/// - `point`: 待测试点。
/// - `border_only`: 是否仅命中边框与手柄（若为 false，选区内部返回 `SelectionDragMode::All`）。
/// - `edge_tolerance`: 边缘容差（像素）。
/// - `min_size`: 最小有效选区尺寸。
///
/// # 返回
/// 命中的拖拽模式。
///
/// # 示例
/// ```rust
/// use snow_ui_shell::geometry::{PhysicalPoint, PhysicalRect};
/// use snow_ui_shell::selection::{hit_test_drag_mode, SelectionDragMode};
/// let sel = PhysicalRect::new(100, 100, 200, 200);
/// assert_eq!(hit_test_drag_mode(sel, PhysicalPoint::new(100, 100), false, 6, 8), SelectionDragMode::TopLeft);
/// assert_eq!(hit_test_drag_mode(sel, PhysicalPoint::new(200, 200), false, 6, 8), SelectionDragMode::All);
/// ```
pub fn hit_test_drag_mode(
    selection: PhysicalRect,
    point: PhysicalPoint,
    border_only: bool,
    edge_tolerance: i32,
    min_size: i32,
) -> SelectionDragMode {
    if selection.is_empty() || selection.width < min_size || selection.height < min_size {
        return SelectionDragMode::None;
    }

    let outer = PhysicalRect::new(
        selection.x - edge_tolerance,
        selection.y - edge_tolerance,
        selection.width + edge_tolerance * 2,
        selection.height + edge_tolerance * 2,
    );
    if !outer.contains(point) {
        return SelectionDragMode::None;
    }

    if border_only {
        let inner = PhysicalRect::new(
            selection.x + edge_tolerance,
            selection.y + edge_tolerance,
            (selection.width - edge_tolerance * 2).max(0),
            (selection.height - edge_tolerance * 2).max(0),
        );
        if !inner.is_empty() && inner.contains(point) {
            return SelectionDragMode::None;
        }
    }

    let mut position = 0u8;
    if point.y <= selection.y + edge_tolerance {
        position |= 0b1000;
    }
    if point.x >= selection.right() - edge_tolerance {
        position |= 0b0100;
    }
    if point.y >= selection.bottom() - edge_tolerance {
        position |= 0b0010;
    }
    if point.x <= selection.x + edge_tolerance {
        position |= 0b0001;
    }

    match position {
        0b1001 => SelectionDragMode::TopLeft,
        0b1100 => SelectionDragMode::TopRight,
        0b0110 => SelectionDragMode::BottomRight,
        0b0011 => SelectionDragMode::BottomLeft,
        0b1000 => SelectionDragMode::Top,
        0b0100 => SelectionDragMode::Right,
        0b0010 => SelectionDragMode::Bottom,
        0b0001 => SelectionDragMode::Left,
        _ => {
            if border_only {
                SelectionDragMode::None
            } else {
                SelectionDragMode::All
            }
        }
    }
}

/// 计算选区 8 个调整手柄的矩形区域。
///
/// # 参数
/// - `selection`: 当前选区。
/// - `handle_size`: 手柄边长。
///
/// # 返回
/// 8 个手柄的拖拽模式与对应物理矩形。
///
/// # 示例
/// ```rust
/// use snow_ui_shell::geometry::PhysicalRect;
/// use snow_ui_shell::selection::handle_rects;
/// let sel = PhysicalRect::new(100, 100, 200, 200);
/// let handles = handle_rects(sel, 8);
/// assert_eq!(handles.len(), 8);
/// ```
pub fn handle_rects(selection: PhysicalRect, handle_size: i32) -> [(SelectionDragMode, PhysicalRect); 8] {
    let half = handle_size / 2;
    let cx = selection.x + selection.width / 2;
    let cy = selection.y + selection.height / 2;

    [
        (
            SelectionDragMode::TopLeft,
            PhysicalRect::new(selection.x - half, selection.y - half, handle_size, handle_size),
        ),
        (
            SelectionDragMode::Top,
            PhysicalRect::new(cx - half, selection.y - half, handle_size, handle_size),
        ),
        (
            SelectionDragMode::TopRight,
            PhysicalRect::new(selection.right() - half, selection.y - half, handle_size, handle_size),
        ),
        (
            SelectionDragMode::Right,
            PhysicalRect::new(selection.right() - half, cy - half, handle_size, handle_size),
        ),
        (
            SelectionDragMode::BottomRight,
            PhysicalRect::new(selection.right() - half, selection.bottom() - half, handle_size, handle_size),
        ),
        (
            SelectionDragMode::Bottom,
            PhysicalRect::new(cx - half, selection.bottom() - half, handle_size, handle_size),
        ),
        (
            SelectionDragMode::BottomLeft,
            PhysicalRect::new(selection.x - half, selection.bottom() - half, handle_size, handle_size),
        ),
        (
            SelectionDragMode::Left,
            PhysicalRect::new(selection.x - half, cy - half, handle_size, handle_size),
        ),
    ]
}

/// 将选区限制在指定边界之内。
///
/// # 参数
/// - `selection`: 原始选区。
/// - `bounds`: 允许的最大边界（通常为屏幕或虚拟桌面）。
/// - `preserve_size`: 是否保持尺寸优先（整体平移回边界内）。
/// - `min_size`: 最小有效尺寸。
///
/// # 返回
/// 限制后的物理矩形。
///
/// # 示例
/// ```rust
/// use snow_ui_shell::geometry::PhysicalRect;
/// use snow_ui_shell::selection::bounded_selection_rect;
/// let sel = PhysicalRect::new(-10, 0, 100, 100);
/// let bounds = PhysicalRect::new(0, 0, 1920, 1080);
/// let r = bounded_selection_rect(sel, bounds, true, 8);
/// assert_eq!(r.x, 0);
/// ```
pub fn bounded_selection_rect(
    selection: PhysicalRect,
    bounds: PhysicalRect,
    preserve_size: bool,
    min_size: i32,
) -> PhysicalRect {
    if bounds.is_empty() {
        return selection;
    }

    let mut result = selection;
    if preserve_size {
        if result.x < bounds.x {
            result.x = bounds.x;
        }
        if result.y < bounds.y {
            result.y = bounds.y;
        }
        if result.right() > bounds.right() {
            result.x = bounds.right() - result.width;
        }
        if result.bottom() > bounds.bottom() {
            result.y = bounds.bottom() - result.height;
        }
        return result;
    }

    let left = result.x.clamp(bounds.x, bounds.right());
    let top = result.y.clamp(bounds.y, bounds.bottom());
    let right = result.right().clamp(bounds.x, bounds.right());
    let bottom = result.bottom().clamp(bounds.y, bounds.bottom());

    let width = (right - left).max(min_size);
    let height = (bottom - top).max(min_size);
    PhysicalRect::new(left, top, width, height)
}

/// 根据拖拽模式与位移计算更新后的选区矩形。
///
/// # 参数
/// - `mode`: 当前拖拽模式。
/// - `origin`: 拖拽开始时的原始选区。
/// - `origin_pos`: 鼠标按下时的起始坐标。
/// - `current_pos`: 鼠标当前坐标。
/// - `bounds`: 边界限制（可选）。
/// - `min_size`: 最小尺寸。
/// - `locked_aspect_ratio`: 锁定宽高比（可选，宽 / 高）。
///
/// # 返回
/// 拖拽计算后的物理矩形。
///
/// # 示例
/// ```rust
/// use snow_ui_shell::geometry::{PhysicalPoint, PhysicalRect};
/// use snow_ui_shell::selection::{dragged_selection_rect, SelectionDragMode};
/// let origin = PhysicalRect::new(100, 100, 200, 200);
/// let start = PhysicalPoint::new(300, 300);
/// let cur = PhysicalPoint::new(350, 350);
/// let r = dragged_selection_rect(SelectionDragMode::BottomRight, origin, start, cur, None, 8, None);
/// assert_eq!((r.width, r.height), (250, 250));
/// ```
pub fn dragged_selection_rect(
    mode: SelectionDragMode,
    origin: PhysicalRect,
    origin_pos: PhysicalPoint,
    current_pos: PhysicalPoint,
    bounds: Option<PhysicalRect>,
    min_size: i32,
    locked_aspect_ratio: Option<f64>,
) -> PhysicalRect {
    let dx = current_pos.x - origin_pos.x;
    let dy = current_pos.y - origin_pos.y;

    let mut left = origin.x;
    let mut top = origin.y;
    let mut right = origin.right();
    let mut bottom = origin.bottom();

    match mode {
        SelectionDragMode::Marquee => {
            let r = marquee_selection_rect(origin_pos, current_pos);
            if let Some(b) = bounds {
                return bounded_selection_rect(r, b, false, min_size);
            }
            return r;
        }
        SelectionDragMode::All => {
            left += dx;
            top += dy;
            let moved = PhysicalRect::new(left, top, origin.width, origin.height);
            if let Some(b) = bounds {
                return bounded_selection_rect(moved, b, true, min_size);
            }
            return moved;
        }
        SelectionDragMode::TopLeft => {
            left += dx;
            top += dy;
        }
        SelectionDragMode::Top => {
            top += dy;
        }
        SelectionDragMode::TopRight => {
            right += dx;
            top += dy;
        }
        SelectionDragMode::Right => {
            right += dx;
        }
        SelectionDragMode::BottomRight => {
            right += dx;
            bottom += dy;
        }
        SelectionDragMode::Bottom => {
            bottom += dy;
        }
        SelectionDragMode::BottomLeft => {
            left += dx;
            bottom += dy;
        }
        SelectionDragMode::Left => {
            left += dx;
        }
        SelectionDragMode::None => return origin,
    }

    // 翻转处理：当手柄拖动越过对边时，自适应对调
    if right < left {
        std::mem::swap(&mut left, &mut right);
    }
    if bottom < top {
        std::mem::swap(&mut top, &mut bottom);
    }

    let mut width = (right - left).max(min_size);
    let mut height = (bottom - top).max(min_size);

    // 宽高比锁定处理
    if let Some(ratio) = locked_aspect_ratio.filter(|r| *r > 0.0) {
        let expected_height = (width as f64 / ratio).round() as i32;
        if expected_height >= min_size {
            height = expected_height;
        } else {
            height = min_size;
            width = (height as f64 * ratio).round() as i32;
        }
    }

    let rect = PhysicalRect::new(left, top, width, height);
    if let Some(b) = bounds {
        bounded_selection_rect(rect, b, false, min_size)
    } else {
        rect
    }
}

/// 格式化选区尺寸标签（如 `"800 × 600"`）。
///
/// # 参数
/// - `rect`: 选区矩形。
///
/// # 返回
/// 格式化字符串。
///
/// # 示例
/// ```rust
/// use snow_ui_shell::geometry::PhysicalRect;
/// use snow_ui_shell::selection::selection_size_label;
/// assert_eq!(selection_size_label(PhysicalRect::new(0, 0, 1920, 1080)), "1920 × 1080");
/// ```
pub fn selection_size_label(rect: PhysicalRect) -> String {
    format!("{} × {}", rect.width.max(0), rect.height.max(0))
}

/// 选区交互状态机模型。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SelectionState {
    /// 空闲，未开始选区。
    #[default]
    Idle,
    /// 橡皮筋框选过程中。
    MarqueeDragging {
        /// 按下起点。
        start: PhysicalPoint,
        /// 当前鼠标位置。
        current: PhysicalPoint,
    },
    /// 已有固定选区。
    Selected {
        /// 当前生效选区矩形。
        rect: PhysicalRect,
    },
    /// 正在移动或缩放已有选区。
    Reshaping {
        /// 调整模式。
        mode: SelectionDragMode,
        /// 原始选区。
        origin_rect: PhysicalRect,
        /// 按下起点。
        origin_pos: PhysicalPoint,
        /// 当前坐标。
        current_pos: PhysicalPoint,
    },
}

impl SelectionState {
    /// 获取当前生效的选区矩形（若有）。
    ///
    /// # 返回
    /// 物理矩形。
    ///
    /// # 示例
    /// ```rust
    /// use snow_ui_shell::geometry::PhysicalRect;
    /// use snow_ui_shell::selection::SelectionState;
    /// let state = SelectionState::Selected { rect: PhysicalRect::new(10, 10, 100, 100) };
    /// assert_eq!(state.current_rect(), Some(PhysicalRect::new(10, 10, 100, 100)));
    /// ```
    pub fn current_rect(&self) -> Option<PhysicalRect> {
        match self {
            Self::Idle => None,
            Self::MarqueeDragging { start, current } => Some(marquee_selection_rect(*start, *current)),
            Self::Selected { rect } => Some(*rect),
            Self::Reshaping {
                mode,
                origin_rect,
                origin_pos,
                current_pos,
            } => Some(dragged_selection_rect(
                *mode,
                *origin_rect,
                *origin_pos,
                *current_pos,
                None,
                DEFAULT_MINIMUM_SELECTION_SIZE,
                None,
            )),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 验证橡皮筋选区计算。
    #[test]
    fn test_marquee_rect() {
        let p1 = PhysicalPoint::new(10, 20);
        let p2 = PhysicalPoint::new(30, 40);
        let r = marquee_selection_rect(p1, p2);
        assert_eq!(r, PhysicalRect::new(10, 20, 21, 21));

        // 反向拖拽
        let r_rev = marquee_selection_rect(p2, p1);
        assert_eq!(r_rev, PhysicalRect::new(10, 20, 21, 21));

        // 单点点击返回空矩形
        let r_empty = marquee_selection_rect(p1, p1);
        assert!(r_empty.is_empty());
    }

    /// 验证手柄命中测试与八向判定。
    #[test]
    fn test_hit_test_handles() {
        let sel = PhysicalRect::new(100, 100, 200, 200);
        let tol = 6;
        let min_s = 8;

        assert_eq!(
            hit_test_drag_mode(sel, PhysicalPoint::new(100, 100), false, tol, min_s),
            SelectionDragMode::TopLeft
        );
        assert_eq!(
            hit_test_drag_mode(sel, PhysicalPoint::new(300, 100), false, tol, min_s),
            SelectionDragMode::TopRight
        );
        assert_eq!(
            hit_test_drag_mode(sel, PhysicalPoint::new(300, 300), false, tol, min_s),
            SelectionDragMode::BottomRight
        );
        assert_eq!(
            hit_test_drag_mode(sel, PhysicalPoint::new(100, 300), false, tol, min_s),
            SelectionDragMode::BottomLeft
        );
        assert_eq!(
            hit_test_drag_mode(sel, PhysicalPoint::new(200, 100), false, tol, min_s),
            SelectionDragMode::Top
        );
        assert_eq!(
            hit_test_drag_mode(sel, PhysicalPoint::new(300, 200), false, tol, min_s),
            SelectionDragMode::Right
        );
        assert_eq!(
            hit_test_drag_mode(sel, PhysicalPoint::new(200, 300), false, tol, min_s),
            SelectionDragMode::Bottom
        );
        assert_eq!(
            hit_test_drag_mode(sel, PhysicalPoint::new(100, 200), false, tol, min_s),
            SelectionDragMode::Left
        );
        assert_eq!(
            hit_test_drag_mode(sel, PhysicalPoint::new(200, 200), false, tol, min_s),
            SelectionDragMode::All
        );
        assert_eq!(
            hit_test_drag_mode(sel, PhysicalPoint::new(200, 200), true, tol, min_s),
            SelectionDragMode::None
        );
    }

    /// 验证手柄几何分布。
    #[test]
    fn test_handle_rects_count_and_coords() {
        let sel = PhysicalRect::new(100, 100, 200, 200);
        let handles = handle_rects(sel, 8);
        assert_eq!(handles.len(), 8);
        assert_eq!(handles[0].0, SelectionDragMode::TopLeft);
        assert_eq!(handles[0].1, PhysicalRect::new(96, 96, 8, 8));
        assert_eq!(handles[4].0, SelectionDragMode::BottomRight);
        assert_eq!(handles[4].1, PhysicalRect::new(296, 296, 8, 8));
    }

    /// 验证拖拽调整尺寸与位移。
    #[test]
    fn test_dragged_rect() {
        let origin = PhysicalRect::new(100, 100, 200, 200);
        let start = PhysicalPoint::new(300, 300);
        let cur = PhysicalPoint::new(350, 350);

        // 缩放右下角
        let r = dragged_selection_rect(
            SelectionDragMode::BottomRight,
            origin,
            start,
            cur,
            None,
            8,
            None,
        );
        assert_eq!(r, PhysicalRect::new(100, 100, 250, 250));

        // 整体平移
        let r_move = dragged_selection_rect(
            SelectionDragMode::All,
            origin,
            start,
            cur,
            None,
            8,
            None,
        );
        assert_eq!(r_move, PhysicalRect::new(150, 150, 200, 200));
    }

    /// 验证选区尺寸格式化。
    #[test]
    fn test_selection_size_label() {
        assert_eq!(
            selection_size_label(PhysicalRect::new(10, 20, 800, 600)),
            "800 × 600"
        );
    }
}
