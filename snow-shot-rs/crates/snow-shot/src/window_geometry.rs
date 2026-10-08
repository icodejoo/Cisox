//! 窗口位置 / 大小记忆的纯逻辑：配置值的解析与序列化、落到屏幕内的修正。
//!
//! 键名与旧版一致（`interface/main_window_geometry`、`interface/translation_window_size`），
//! 字段为 `x / y / width / height / maximized`。不依赖 GPUI，可离屏测试。

use serde_json::{Value, json};
use snow_ui::shell::geometry::PhysicalRect;

/// 配置键：主窗口位置与大小。
pub const MAIN_WINDOW_GEOMETRY_KEY: &str = "interface/main_window_geometry";
/// 配置键：独立翻译窗口大小。
pub const TRANSLATION_WINDOW_SIZE_KEY: &str = "interface/translation_window_size";
/// 窗口单边允许的最大像素（与旧版一致，超出视为损坏）。
const MAX_EXTENT: i64 = 32767;
/// 主窗口最小宽度（物理像素，恢复位置时的下限）。
pub const MAIN_MIN_WIDTH: i32 = 640;
/// 主窗口最小高度（物理像素，恢复位置时的下限）。
pub const MAIN_MIN_HEIGHT: i32 = 420;
/// 翻译窗口最小逻辑宽度。
pub const TRANSLATE_MIN_WIDTH: i32 = 420;
/// 翻译窗口最小逻辑高度。
pub const TRANSLATE_MIN_HEIGHT: i32 = 360;

/// 持久化的窗口几何：普通态外框与是否最大化。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SavedGeometry {
    /// 普通（非最大化）态的外框，屏幕物理像素。
    pub rect: PhysicalRect,
    /// 上次关闭时是否最大化。
    pub maximized: bool,
}

/// 从 JSON 取整数字段：必须是有限的整数值。
fn int_field(value: &Value, key: &str) -> Option<i64> {
    let number = value.get(key)?.as_f64()?;
    (number.is_finite() && number.fract() == 0.0 && number.abs() <= i32::MAX as f64)
        .then_some(number as i64)
}

/// 宽高是否在合法范围内。
fn extent_ok(width: i64, height: i64) -> bool {
    (1..=MAX_EXTENT).contains(&width) && (1..=MAX_EXTENT).contains(&height)
}

/// 解析主窗口几何；缺字段、非整数、尺寸越界或 `maximized` 不是布尔都返回 `None`。
///
/// # 参数
/// - `value`：配置里的对象值。
pub fn parse_geometry(value: &Value) -> Option<SavedGeometry> {
    let (x, y) = (int_field(value, "x")?, int_field(value, "y")?);
    let (width, height) = (int_field(value, "width")?, int_field(value, "height")?);
    if !extent_ok(width, height) {
        return None;
    }
    let maximized = match value.get("maximized") {
        None => false,
        Some(flag) => flag.as_bool()?,
    };
    Some(SavedGeometry {
        rect: PhysicalRect::new(x as i32, y as i32, width as i32, height as i32),
        maximized,
    })
}

/// 把主窗口几何写成配置值。
///
/// # 参数
/// - `rect`：普通态外框。
/// - `maximized`：是否最大化。
pub fn geometry_to_json(rect: PhysicalRect, maximized: bool) -> Value {
    json!({
        "x": rect.x,
        "y": rect.y,
        "width": rect.width,
        "height": rect.height,
        "maximized": maximized,
    })
}

/// 解析窗口大小（`width` / `height`）；非法返回 `None`。
///
/// # 参数
/// - `value`：配置里的对象值。
pub fn parse_window_size(value: &Value) -> Option<(i32, i32)> {
    let (width, height) = (int_field(value, "width")?, int_field(value, "height")?);
    extent_ok(width, height).then_some((width as i32, height as i32))
}

/// 把窗口大小写成配置值。
///
/// # 参数
/// - `width` / `height`：逻辑像素。
pub fn size_to_json(width: i32, height: i32) -> Value {
    json!({ "width": width, "height": height })
}

/// 把大小夹到 `[min, max]`；`max` 小于 `min` 时以 `min` 为准。
///
/// # 参数
/// - `size`：`(宽, 高)`。
/// - `min` / `max`：下限与上限。
pub fn clamp_size(size: (i32, i32), min: (i32, i32), max: (i32, i32)) -> (i32, i32) {
    (
        size.0.max(min.0).min(max.0.max(min.0)),
        size.1.max(min.1).min(max.1.max(min.1)),
    )
}

/// 把保存的外框修正到当前屏幕内：尺寸先抬到下限、再压到最大屏尺寸；
/// 与任何屏幕都不相交时挪进第一块（主屏）工作区。
///
/// # 参数
/// - `saved`：保存的外框。
/// - `min`：最小尺寸 `(宽, 高)`。
/// - `screens`：各屏工作区，**主屏在前**；为空时原样返回。
pub fn fit_geometry(
    saved: PhysicalRect,
    min: (i32, i32),
    screens: &[PhysicalRect],
) -> PhysicalRect {
    let Some(first) = screens.first() else {
        return saved;
    };
    let largest = screens
        .iter()
        .fold((1, 1), |acc, s| (acc.0.max(s.width), acc.1.max(s.height)));
    let (width, height) = clamp_size((saved.width, saved.height), min, largest);
    let mut rect = PhysicalRect::new(saved.x, saved.y, width, height);
    if screens.iter().any(|s| s.intersect(&rect).is_some()) {
        return rect;
    }
    rect.width = rect.width.min(first.width);
    rect.height = rect.height.min(first.height);
    rect.x = rect
        .x
        .clamp(first.x, (first.right() - rect.width).max(first.x));
    rect.y = rect
        .y
        .clamp(first.y, (first.bottom() - rect.height).max(first.y));
    rect
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 主屏工作区。
    fn screen() -> PhysicalRect {
        PhysicalRect::new(0, 0, 1920, 1040)
    }

    /// 几何序列化后能原样解析；缺 maximized 视为 false。
    #[test]
    fn geometry_roundtrip_and_defaults() {
        let rect = PhysicalRect::new(-100, 20, 900, 640);
        let back = parse_geometry(&geometry_to_json(rect, true)).unwrap();
        assert_eq!((back.rect, back.maximized), (rect, true));
        let legacy = json!({"x": 1, "y": 2, "width": 3, "height": 4});
        assert!(!parse_geometry(&legacy).unwrap().maximized);
    }

    /// 非法值一律拒绝：空对象（默认值）、缺字段、小数、越界、maximized 非布尔。
    #[test]
    fn geometry_rejects_bad_values() {
        for bad in [
            json!({}),
            json!({"x": 1, "y": 2, "width": 3}),
            json!({"x": 1.5, "y": 2, "width": 3, "height": 4}),
            json!({"x": 1, "y": 2, "width": 0, "height": 4}),
            json!({"x": 1, "y": 2, "width": 40000, "height": 4}),
            json!({"x": 1, "y": 2, "width": 3, "height": 4, "maximized": "yes"}),
            json!("text"),
        ] {
            assert!(parse_geometry(&bad).is_none(), "{bad}");
        }
    }

    /// 大小的解析与往返；非法大小拒绝。
    #[test]
    fn size_roundtrip() {
        assert_eq!(parse_window_size(&size_to_json(640, 560)), Some((640, 560)));
        assert_eq!(parse_window_size(&json!({})), None);
        assert_eq!(parse_window_size(&json!({"width": -1, "height": 5})), None);
    }

    /// 大小夹取：低于下限抬起，高于上限压住，上限小于下限时以下限为准。
    #[test]
    fn clamp_size_bounds() {
        assert_eq!(
            clamp_size((100, 9000), (420, 360), (1920, 1080)),
            (420, 1080)
        );
        assert_eq!(clamp_size((800, 600), (420, 360), (300, 300)), (420, 360));
    }

    /// 仍在屏幕内的窗口原样保留；过小的被抬到下限。
    #[test]
    fn fit_keeps_visible_window() {
        let rect = PhysicalRect::new(100, 100, 900, 640);
        assert_eq!(fit_geometry(rect, (640, 420), &[screen()]), rect);
        let tiny = PhysicalRect::new(100, 100, 10, 10);
        let fitted = fit_geometry(tiny, (640, 420), &[screen()]);
        assert_eq!((fitted.width, fitted.height), (640, 420));
    }

    /// 副屏拔掉后（窗口完全在屏外）被挪回主屏，且不超出主屏。
    #[test]
    fn fit_moves_offscreen_window_back() {
        let gone = PhysicalRect::new(-3000, 50, 900, 640);
        let fitted = fit_geometry(gone, (640, 420), &[screen()]);
        assert_eq!(fitted.x, 0);
        assert!(screen().intersect(&fitted) == Some(fitted));
        let beyond = PhysicalRect::new(5000, 5000, 900, 640);
        let fitted = fit_geometry(beyond, (640, 420), &[screen()]);
        assert!(screen().intersect(&fitted) == Some(fitted));
    }

    /// 没有屏幕信息时原样返回；窗口跨在副屏上时保留。
    #[test]
    fn fit_without_screens_or_on_secondary() {
        let rect = PhysicalRect::new(-1500, 10, 900, 640);
        assert_eq!(fit_geometry(rect, (640, 420), &[]), rect);
        let second = PhysicalRect::new(-1920, 0, 1920, 1040);
        assert_eq!(fit_geometry(rect, (640, 420), &[screen(), second]), rect);
    }
}
