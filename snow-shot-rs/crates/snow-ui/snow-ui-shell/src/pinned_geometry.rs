//! 贴图窗口几何运算与手柄交互算法（Pinned Window Geometry）。
//!
//! 提供贴图缩放瞄准点（Anchor）计算、滚轮缩放、透明度阶梯调节、等比拉伸、
//! 八向手柄外框生成与命中测试。纯几何逻辑，无 GUI 依赖。

use crate::geometry::{PhysicalPoint, PhysicalRect};

/// 贴图调整手柄与拖动模式。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PinnedDragHandle {
    /// 左上手柄。
    TopLeft,
    /// 上边缘手柄。
    Top,
    /// 右上手柄。
    TopRight,
    /// 右边缘手柄。
    Right,
    /// 右下手柄。
    BottomRight,
    /// 下边缘手柄。
    Bottom,
    /// 左下手柄。
    BottomLeft,
    /// 左边缘手柄。
    Left,
    /// 整体拖拽平移。
    Move,
}

impl PinnedDragHandle {
    /// 判断当前手柄是否为尺寸缩放手柄。
    ///
    /// # 返回
    /// 若为八向手柄之一返回 `true`，若为整体平移返回 `false`。
    ///
    /// # 示例
    /// ```rust
    /// use snow_ui_shell::pinned_geometry::PinnedDragHandle;
    /// assert!(PinnedDragHandle::TopLeft.is_resize());
    /// assert!(!PinnedDragHandle::Move.is_resize());
    /// ```
    pub const fn is_resize(&self) -> bool {
        !matches!(self, Self::Move)
    }
}

/// 缩放锚点类型。
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ScaleAnchor {
    /// 中心锚点：缩放时矩形几何中心位置保持不变。
    Center,
    /// 左上角锚点：左上角位置保持不变。
    TopLeft,
    /// 右上角锚点：右上角位置保持不变。
    TopRight,
    /// 左下角锚点：左下角位置保持不变。
    BottomLeft,
    /// 右下角锚点：右下角位置保持不变。
    BottomRight,
    /// 鼠标物理坐标锚点：保持鼠标指针所在位置在贴图内容中的相对坐标固定。
    MousePoint(PhysicalPoint),
}

/// 计算基准尺寸在指定缩放比例下的物理尺寸。
///
/// # 参数
/// - `baseline`: 原始基准尺寸（像素）。
/// - `zoom`: 缩放比例（1.0 表示 100%）。
///
/// # 返回
/// 缩放后的物理尺寸（宽、高至少为 1 像素）。
///
/// # 示例
/// ```rust
/// use snow_ui_shell::geometry::PhysicalPoint;
/// use snow_ui_shell::pinned_geometry::scaled_size;
/// let orig = PhysicalPoint::new(400, 300);
/// let sz = scaled_size(orig, 1.5);
/// assert_eq!(sz, PhysicalPoint::new(600, 450));
/// ```
pub fn scaled_size(baseline: PhysicalPoint, zoom: f32) -> PhysicalPoint {
    let w = ((baseline.x as f32) * zoom).round() as i32;
    let h = ((baseline.y as f32) * zoom).round() as i32;
    PhysicalPoint::new(w.max(1), h.max(1))
}

/// 计算基于特定锚点缩放到目标尺寸后的矩形。
///
/// # 参数
/// - `reference`: 缩放前的矩形。
/// - `target_size`: 目标尺寸。
/// - `anchor`: 缩放瞄准锚点。
///
/// # 返回
/// 调整后的矩形。
///
/// # 示例
/// ```rust
/// use snow_ui_shell::geometry::{PhysicalPoint, PhysicalRect};
/// use snow_ui_shell::pinned_geometry::{ScaleAnchor, anchored_scale_rect};
/// let rect = PhysicalRect::new(100, 100, 200, 200);
/// let res = anchored_scale_rect(rect, PhysicalPoint::new(300, 300), ScaleAnchor::Center);
/// assert_eq!(res, PhysicalRect::new(50, 50, 300, 300));
/// ```
pub fn anchored_scale_rect(
    reference: PhysicalRect,
    target_size: PhysicalPoint,
    anchor: ScaleAnchor,
) -> PhysicalRect {
    let target_w = target_size.x.max(1);
    let target_h = target_size.y.max(1);

    match anchor {
        ScaleAnchor::TopLeft => PhysicalRect::new(reference.x, reference.y, target_w, target_h),
        ScaleAnchor::TopRight => {
            let new_x = reference.right() - target_w;
            PhysicalRect::new(new_x, reference.y, target_w, target_h)
        }
        ScaleAnchor::BottomLeft => {
            let new_y = reference.bottom() - target_h;
            PhysicalRect::new(reference.x, new_y, target_w, target_h)
        }
        ScaleAnchor::BottomRight => {
            let new_x = reference.right() - target_w;
            let new_y = reference.bottom() - target_h;
            PhysicalRect::new(new_x, new_y, target_w, target_h)
        }
        ScaleAnchor::Center => {
            let cx = reference.x + reference.width / 2;
            let cy = reference.y + reference.height / 2;
            let new_x = cx - target_w / 2;
            let new_y = cy - target_h / 2;
            PhysicalRect::new(new_x, new_y, target_w, target_h)
        }
        ScaleAnchor::MousePoint(mouse) => {
            if reference.width <= 0 || reference.height <= 0 {
                return PhysicalRect::new(mouse.x, mouse.y, target_w, target_h);
            }
            let ratio_x = (mouse.x - reference.x) as f32 / reference.width as f32;
            let ratio_y = (mouse.y - reference.y) as f32 / reference.height as f32;

            let new_x = mouse.x - (target_w as f32 * ratio_x).round() as i32;
            let new_y = mouse.y - (target_h as f32 * ratio_y).round() as i32;
            PhysicalRect::new(new_x, new_y, target_w, target_h)
        }
    }
}

/// 阶梯调整缩放倍率。
///
/// # 参数
/// - `current`: 当前缩放倍率。
/// - `delta_steps`: 步进次数（正数放大，负数缩小）。
/// - `min_zoom`: 允许的最小缩放倍率。
/// - `max_zoom`: 允许的最大缩放倍率。
///
/// # 返回
/// 调整并截断后的缩放倍率。
///
/// # 示例
/// ```rust
/// use snow_ui_shell::pinned_geometry::step_zoom;
/// let z = step_zoom(1.0, 1.0, 0.1, 5.0);
/// assert!((z - 1.1).abs() < 1e-4);
/// ```
pub fn step_zoom(current: f32, delta_steps: f32, min_zoom: f32, max_zoom: f32) -> f32 {
    let step = 0.1;
    let next = current + delta_steps * step;
    next.clamp(min_zoom, max_zoom)
}

/// 阶梯调整窗口不透明度。
///
/// # 参数
/// - `current`: 当前不透明度 (0.0 ~ 1.0)。
/// - `delta_steps`: 步进次数（正数更不透明，负数更透明）。
/// - `min_opacity`: 最小不透明度。
/// - `max_opacity`: 最大不透明度。
///
/// # 返回
/// 调整并截断后的不透明度。
///
/// # 示例
/// ```rust
/// use snow_ui_shell::pinned_geometry::step_opacity;
/// let op = step_opacity(1.0, -2.0, 0.2, 1.0);
/// assert!((op - 0.9).abs() < 1e-4);
/// ```
pub fn step_opacity(
    current: f32,
    delta_steps: f32,
    min_opacity: f32,
    max_opacity: f32,
) -> f32 {
    let step = 0.05;
    let next = current + delta_steps * step;
    next.clamp(min_opacity, max_opacity)
}

/// 等比拖动缩放计算。
///
/// 根据拖动的手柄方向，依据鼠标位移 `delta` 保持原始基准比例进行缩放调整。
///
/// # 参数
/// - `reference`: 原始参照矩形。
/// - `delta`: 鼠标相对拖动起始点的物理位移。
/// - `baseline`: 原始无缩放尺寸。
/// - `handle`: 拖动的手柄。
/// - `min_size`: 允许的最小尺寸。
/// - `max_size`: 允许的最大尺寸。
///
/// # 返回
/// 调整后的矩形。
///
/// # 示例
/// ```rust
/// use snow_ui_shell::geometry::{PhysicalPoint, PhysicalRect};
/// use snow_ui_shell::pinned_geometry::{PinnedDragHandle, proportional_resize_rect};
/// let rect = PhysicalRect::new(100, 100, 200, 100);
/// let delta = PhysicalPoint::new(40, 20);
/// let res = proportional_resize_rect(
///     rect,
///     delta,
///     PhysicalPoint::new(200, 100),
///     PinnedDragHandle::BottomRight,
///     PhysicalPoint::new(50, 25),
///     PhysicalPoint::new(800, 400),
/// );
/// assert_eq!(res, PhysicalRect::new(100, 100, 240, 120));
/// ```
pub fn proportional_resize_rect(
    reference: PhysicalRect,
    delta: PhysicalPoint,
    baseline: PhysicalPoint,
    handle: PinnedDragHandle,
    min_size: PhysicalPoint,
    max_size: PhysicalPoint,
) -> PhysicalRect {
    if !handle.is_resize() {
        return reference;
    }

    let aspect = if baseline.y != 0 {
        baseline.x as f32 / baseline.y as f32
    } else {
        1.0
    };

    let (anchor, primary_delta) = match handle {
        PinnedDragHandle::BottomRight => (ScaleAnchor::TopLeft, delta.x.max((delta.y as f32 * aspect) as i32)),
        PinnedDragHandle::Right => (ScaleAnchor::TopLeft, delta.x),
        PinnedDragHandle::Bottom => (ScaleAnchor::TopLeft, (delta.y as f32 * aspect) as i32),
        PinnedDragHandle::BottomLeft => (ScaleAnchor::TopRight, (-delta.x).max((delta.y as f32 * aspect) as i32)),
        PinnedDragHandle::Left => (ScaleAnchor::TopRight, -delta.x),
        PinnedDragHandle::TopRight => (ScaleAnchor::BottomLeft, delta.x.max((-delta.y as f32 * aspect) as i32)),
        PinnedDragHandle::Top => (ScaleAnchor::BottomLeft, (-delta.y as f32 * aspect) as i32),
        PinnedDragHandle::TopLeft => (ScaleAnchor::BottomRight, (-delta.x).max((-delta.y as f32 * aspect) as i32)),
        PinnedDragHandle::Move => return reference,
    };

    let target_w = (reference.width + primary_delta)
        .clamp(min_size.x, max_size.x);
    let target_h = ((target_w as f32 / aspect).round() as i32)
        .clamp(min_size.y, max_size.y);

    anchored_scale_rect(reference, PhysicalPoint::new(target_w, target_h), anchor)
}

/// 计算贴图四周 8 个手柄的物理区域。
///
/// # 参数
/// - `bounds`: 贴图外框矩形。
/// - `handle_size`: 手柄边长（像素）。
///
/// # 返回
/// 包含 8 个手柄种类与其物理外框的数组。
///
/// # 示例
/// ```rust
/// use snow_ui_shell::geometry::PhysicalRect;
/// use snow_ui_shell::pinned_geometry::handle_rects;
/// let bounds = PhysicalRect::new(100, 100, 200, 150);
/// let handles = handle_rects(bounds, 8);
/// assert_eq!(handles.len(), 8);
/// ```
pub fn handle_rects(bounds: PhysicalRect, handle_size: i32) -> [(PinnedDragHandle, PhysicalRect); 8] {
    let hs = handle_size.max(2);
    let half = hs / 2;

    let left = bounds.x - half;
    let mid_x = bounds.x + bounds.width / 2 - half;
    let right = bounds.right() - half;

    let top = bounds.y - half;
    let mid_y = bounds.y + bounds.height / 2 - half;
    let bottom = bounds.bottom() - half;

    [
        (PinnedDragHandle::TopLeft, PhysicalRect::new(left, top, hs, hs)),
        (PinnedDragHandle::Top, PhysicalRect::new(mid_x, top, hs, hs)),
        (PinnedDragHandle::TopRight, PhysicalRect::new(right, top, hs, hs)),
        (PinnedDragHandle::Right, PhysicalRect::new(right, mid_y, hs, hs)),
        (PinnedDragHandle::BottomRight, PhysicalRect::new(right, bottom, hs, hs)),
        (PinnedDragHandle::Bottom, PhysicalRect::new(mid_x, bottom, hs, hs)),
        (PinnedDragHandle::BottomLeft, PhysicalRect::new(left, bottom, hs, hs)),
        (PinnedDragHandle::Left, PhysicalRect::new(left, mid_y, hs, hs)),
    ]
}

/// 命中测试手柄。
///
/// # 参数
/// - `bounds`: 贴图物理矩形。
/// - `point`: 测试点物理坐标。
/// - `handle_size`: 手柄显示尺寸。
/// - `edge_margin`: 边缘吸附容差。
///
/// # 返回
/// 命中的手柄类型；若在内容区内部则返回 `Some(PinnedDragHandle::Move)`，若在外部返回 `None`。
///
/// # 示例
/// ```rust
/// use snow_ui_shell::geometry::{PhysicalPoint, PhysicalRect};
/// use snow_ui_shell::pinned_geometry::{PinnedDragHandle, hit_test_handle};
/// let bounds = PhysicalRect::new(100, 100, 200, 150);
/// let hit = hit_test_handle(bounds, PhysicalPoint::new(100, 100), 8, 4);
/// assert_eq!(hit, Some(PinnedDragHandle::TopLeft));
/// let hit_inside = hit_test_handle(bounds, PhysicalPoint::new(150, 150), 8, 4);
/// assert_eq!(hit_inside, Some(PinnedDragHandle::Move));
/// ```
pub fn hit_test_handle(
    bounds: PhysicalRect,
    point: PhysicalPoint,
    handle_size: i32,
    edge_margin: i32,
) -> Option<PinnedDragHandle> {
    let tolerance = (handle_size / 2).max(edge_margin);
    for (handle, rect) in handle_rects(bounds, handle_size) {
        let hit_box = PhysicalRect::new(
            rect.x - tolerance / 2,
            rect.y - tolerance / 2,
            rect.width + tolerance,
            rect.height + tolerance,
        );
        if hit_box.contains(point) {
            return Some(handle);
        }
    }

    if bounds.contains(point) {
        Some(PinnedDragHandle::Move)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 验证等比缩放计算。
    #[test]
    fn test_scaled_size() {
        let base = PhysicalPoint::new(200, 100);
        assert_eq!(scaled_size(base, 1.0), PhysicalPoint::new(200, 100));
        assert_eq!(scaled_size(base, 2.0), PhysicalPoint::new(400, 200));
        assert_eq!(scaled_size(base, 0.5), PhysicalPoint::new(100, 50));
    }

    /// 验证中心锚点缩放保持几何中心。
    #[test]
    fn test_anchored_center() {
        let rect = PhysicalRect::new(100, 100, 200, 200);
        let scaled = anchored_scale_rect(rect, PhysicalPoint::new(300, 300), ScaleAnchor::Center);
        assert_eq!(scaled, PhysicalRect::new(50, 50, 300, 300));
    }

    /// 验证鼠标点锚点缩放。
    #[test]
    fn test_anchored_mouse() {
        let rect = PhysicalRect::new(100, 100, 200, 200);
        let mouse = PhysicalPoint::new(200, 200); // 正中心
        let scaled = anchored_scale_rect(rect, PhysicalPoint::new(400, 400), ScaleAnchor::MousePoint(mouse));
        assert_eq!(scaled, PhysicalRect::new(0, 0, 400, 400));
    }

    /// 验证手柄命中测试与拖拽。
    #[test]
    fn test_handle_hit() {
        let bounds = PhysicalRect::new(100, 100, 200, 150);
        assert_eq!(
            hit_test_handle(bounds, PhysicalPoint::new(100, 100), 8, 4),
            Some(PinnedDragHandle::TopLeft)
        );
        assert_eq!(
            hit_test_handle(bounds, PhysicalPoint::new(150, 150), 8, 4),
            Some(PinnedDragHandle::Move)
        );
        assert_eq!(
            hit_test_handle(bounds, PhysicalPoint::new(50, 50), 8, 4),
            None
        );
    }

    /// 验证等比调整拉伸。
    #[test]
    fn test_proportional_resize() {
        let rect = PhysicalRect::new(100, 100, 200, 100);
        let res = proportional_resize_rect(
            rect,
            PhysicalPoint::new(20, 10),
            PhysicalPoint::new(200, 100),
            PinnedDragHandle::BottomRight,
            PhysicalPoint::new(50, 25),
            PhysicalPoint::new(500, 250),
        );
        assert_eq!(res, PhysicalRect::new(100, 100, 220, 110));
    }
}
