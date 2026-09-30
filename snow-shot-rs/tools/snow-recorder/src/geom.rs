//! 与平台无关的几何与像素辅助：坐标缩放、图层裁剪、光标图层几何、RGBA/BGRA 互换。
//!
//! 合成阶段（Windows 上是 VideoProcessor 多图层）用这些纯函数决定光标图层怎么摆放，
//! 纯函数在任何平台都能单测。

use snow_cursor::{AttachedCursorSample, CursorShape};

/// 整数矩形（像素）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rect {
    /// 左上角 x。
    pub x: i32,
    /// 左上角 y。
    pub y: i32,
    /// 宽。
    pub width: u32,
    /// 高。
    pub height: u32,
}

impl Rect {
    /// 从原点开始、覆盖整个尺寸的矩形。
    ///
    /// # 参数
    /// - `size`：`(宽, 高)`。
    pub fn full(size: (u32, u32)) -> Self {
        Self { x: 0, y: 0, width: size.0, height: size.1 }
    }
}

/// 把源坐标按比例映射到输出坐标（四舍五入）。
///
/// # 参数
/// - `value`：源坐标。
/// - `from`：源尺寸。
/// - `to`：输出尺寸。
///
/// # 示例
/// ```ignore
/// assert_eq!(scale_coordinate(1440, 2560, 1920), 1080);
/// ```
pub fn scale_coordinate(value: i32, from: u32, to: u32) -> i32 {
    if from == 0 {
        return value;
    }
    let scaled = (i64::from(value) * i64::from(to) * 2 + i64::from(from)).div_euclid(i64::from(from) * 2);
    scaled.clamp(i64::from(i32::MIN), i64::from(i32::MAX)) as i32
}

/// 把图层目标矩形裁剪到输出范围内，并同步按比例裁剪源矩形。
///
/// # 参数
/// - `source`：图层源矩形。
/// - `dest`：图层目标矩形（可超出输出范围，含负坐标）。
/// - `bounds`：输出尺寸。
///
/// # 返回
/// 裁剪后的 `(源矩形, 目标矩形)`；完全在范围外或为空返回 `None`。
///
/// # 示例
/// ```ignore
/// let (s, d) = clip_layer(Rect { x: 0, y: 0, width: 10, height: 10 },
///                         Rect { x: -5, y: 0, width: 10, height: 10 }, (100, 100)).unwrap();
/// assert_eq!((s.x, s.width, d.x, d.width), (5, 5, 0, 5));
/// ```
pub fn clip_layer(source: Rect, dest: Rect, bounds: (u32, u32)) -> Option<(Rect, Rect)> {
    if dest.width == 0 || dest.height == 0 || source.width == 0 || source.height == 0 {
        return None;
    }
    let (dl, dt) = (i64::from(dest.x), i64::from(dest.y));
    let (dr, db) = (dl + i64::from(dest.width), dt + i64::from(dest.height));
    let (cl, ct) = (dl.max(0), dt.max(0));
    let (cr, cb) = (dr.min(i64::from(bounds.0)), db.min(i64::from(bounds.1)));
    if cl >= cr || ct >= cb {
        return None;
    }
    let map = |clipped_from: i64, edge: i64, dest_len: i64, src_origin: i64, src_len: i64| -> i64 {
        src_origin + (clipped_from - edge) * src_len / dest_len
    };
    let (dw, dh) = (i64::from(dest.width), i64::from(dest.height));
    let (sw, sh) = (i64::from(source.width), i64::from(source.height));
    let sx0 = map(cl, dl, dw, i64::from(source.x), sw);
    let sx1 = map(cr, dl, dw, i64::from(source.x), sw).max(sx0 + 1);
    let sy0 = map(ct, dt, dh, i64::from(source.y), sh);
    let sy1 = map(cb, dt, dh, i64::from(source.y), sh).max(sy0 + 1);
    Some((
        Rect { x: sx0 as i32, y: sy0 as i32, width: (sx1 - sx0) as u32, height: (sy1 - sy0) as u32 },
        Rect { x: cl as i32, y: ct as i32, width: (cr - cl) as u32, height: (cb - ct) as u32 },
    ))
}

/// 把 RGBA 像素换成 BGRA（交换 R/B 通道，保持直通 alpha）。
///
/// # 参数
/// - `rgba`：RGBA 像素（长度为 4 的倍数，多余字节被忽略）。
///
/// # 示例
/// ```ignore
/// assert_eq!(rgba_to_bgra(&[1, 2, 3, 4]), vec![3, 2, 1, 4]);
/// ```
pub fn rgba_to_bgra(rgba: &[u8]) -> Vec<u8> {
    rgba.chunks_exact(4).flat_map(|p| [p[2], p[1], p[0], p[3]]).collect()
}

/// 光标图层的源/目标矩形（已缩放并裁剪到输出范围）。
///
/// # 参数
/// - `sample`：光标采样（坐标为选区内源像素）。
/// - `shape`：光标形状。
/// - `src_size`：选区尺寸。
/// - `out_size`：输出尺寸。
///
/// # 返回
/// `(源矩形, 目标矩形)`；不可见或完全在画面外返回 `None`。
pub fn cursor_geometry(
    sample: &AttachedCursorSample,
    shape: &CursorShape,
    src_size: (u32, u32),
    out_size: (u32, u32),
) -> Option<(Rect, Rect)> {
    if !sample.visible || shape.width == 0 || shape.height == 0 {
        return None;
    }
    let left = sample.x.saturating_sub(i32::try_from(shape.hotspot_x).unwrap_or(0));
    let top = sample.y.saturating_sub(i32::try_from(shape.hotspot_y).unwrap_or(0));
    let right = left.saturating_add(i32::try_from(shape.width).unwrap_or(i32::MAX));
    let bottom = top.saturating_add(i32::try_from(shape.height).unwrap_or(i32::MAX));
    let (l, r) = (scale_coordinate(left, src_size.0, out_size.0), scale_coordinate(right, src_size.0, out_size.0));
    let (t, b) = (scale_coordinate(top, src_size.1, out_size.1), scale_coordinate(bottom, src_size.1, out_size.1));
    let dest = Rect { x: l, y: t, width: (r - l).max(1) as u32, height: (b - t).max(1) as u32 };
    clip_layer(Rect { x: 0, y: 0, width: shape.width, height: shape.height }, dest, out_size)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 构造矩形。
    fn r(x: i32, y: i32, w: u32, h: u32) -> Rect {
        Rect { x, y, width: w, height: h }
    }

    /// 缩放坐标四舍五入，且同尺寸不变。
    #[test]
    fn scale_coordinate_rounds() {
        assert_eq!(scale_coordinate(1440, 2560, 1920), 1080);
        assert_eq!(scale_coordinate(5, 10, 10), 5);
        assert_eq!(scale_coordinate(-3, 10, 10), -3);
        assert_eq!(scale_coordinate(1, 3, 2), 1);
        assert_eq!(scale_coordinate(7, 0, 100), 7);
    }

    /// 左上角越界：目标被裁到 0，源矩形同步裁掉对应部分。
    #[test]
    fn clip_layer_trims_negative_origin() {
        let (s, d) = clip_layer(r(0, 0, 10, 10), r(-5, -2, 10, 10), (100, 100)).unwrap();
        assert_eq!((s.x, s.y, s.width, s.height), (5, 2, 5, 8));
        assert_eq!((d.x, d.y, d.width, d.height), (0, 0, 5, 8));
    }

    /// 右下角越界：目标被裁到边界。
    #[test]
    fn clip_layer_trims_far_edge() {
        let (s, d) = clip_layer(r(0, 0, 10, 10), r(96, 97, 10, 10), (100, 100)).unwrap();
        assert_eq!((s.width, s.height), (4, 3));
        assert_eq!((d.width, d.height), (4, 3));
    }

    /// 完全在范围外或为空返回 None；完全在内则原样。
    #[test]
    fn clip_layer_outside_and_inside() {
        assert!(clip_layer(r(0, 0, 10, 10), r(100, 0, 10, 10), (100, 100)).is_none());
        assert!(clip_layer(r(0, 0, 10, 10), r(-10, 0, 10, 10), (100, 100)).is_none());
        assert!(clip_layer(r(0, 0, 0, 10), r(0, 0, 10, 10), (100, 100)).is_none());
        let (s, d) = clip_layer(r(0, 0, 10, 10), r(20, 30, 10, 10), (100, 100)).unwrap();
        assert_eq!((s, d), (r(0, 0, 10, 10), r(20, 30, 10, 10)));
    }

    /// RGBA 到 BGRA：交换 R/B，alpha 不变，尾部残余忽略。
    #[test]
    fn rgba_to_bgra_swaps_channels() {
        assert_eq!(rgba_to_bgra(&[1, 2, 3, 4, 5, 6, 7, 8]), vec![3, 2, 1, 4, 7, 6, 5, 8]);
        assert_eq!(rgba_to_bgra(&[1, 2, 3, 4, 9]), vec![3, 2, 1, 4]);
        assert!(rgba_to_bgra(&[]).is_empty());
    }

    /// 光标几何：热点偏移、缩放到输出、不可见返回 None。
    #[test]
    fn cursor_geometry_applies_hotspot_and_scale() {
        use snow_cursor::{CursorCompositionMode, CursorShapeState};
        let shape = CursorShape::from_rgba(2, 3, 32, 32, CursorCompositionMode::AlphaBlend, vec![0u8; 32 * 32 * 4]);
        let mut sample = AttachedCursorSample { x: 100, y: 100, visible: true, shape: CursorShapeState::Embedded(shape.clone()) };
        let (s, d) = cursor_geometry(&sample, &shape, (200, 200), (200, 200)).unwrap();
        assert_eq!((s, d), (r(0, 0, 32, 32), r(98, 97, 32, 32)));
        let (_, half) = cursor_geometry(&sample, &shape, (200, 200), (100, 100)).unwrap();
        assert_eq!((half.x, half.y, half.width, half.height), (49, 49, 16, 16));
        sample.visible = false;
        assert!(cursor_geometry(&sample, &shape, (200, 200), (200, 200)).is_none());
    }
}
