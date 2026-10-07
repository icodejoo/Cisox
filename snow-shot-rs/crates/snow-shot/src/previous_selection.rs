//! 「上一次选区」的持久化编解码（配置键 `screenshot_selection/previous_selection`）。
//!
//! 格式与旧版一致：矩形用逻辑坐标，另带圆角、阴影、比例锁字段；覆盖窗内部用物理坐标，
//! 编码时除以缩放比、解码时乘回并裁到底图范围。纯逻辑，不接触界面与磁盘。

use serde_json::{Value, json};
use snow_ui::shell::geometry::PhysicalRect;

/// 上一次选区的配置键。
pub const PREVIOUS_SELECTION_KEY: &str = "screenshot_selection/previous_selection";

/// 旧版默认的阴影颜色（`#AARRGGBB`）。
const DEFAULT_SHADOW_COLOR: &str = "#780A141E";

/// 与选区一起持久化的外观参数（覆盖窗暂未使用，原样回写以保持与旧版互通）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct SelectionStyle {
    /// 圆角半径。
    pub corner_radius: i32,
    /// 阴影宽度。
    pub shadow_width: i32,
    /// 是否锁定宽高比。
    pub lock_aspect_ratio: bool,
}

/// 物理像素换算为逻辑像素（四舍五入）。
fn to_logical(value: i32, scale: f32) -> i32 {
    (value as f32 / scale).round() as i32
}

/// 把选区编码成配置值。
///
/// # 参数
/// - `rect`：选区（底图物理坐标）。
/// - `scale`：显示缩放比（物理 / 逻辑），非正数按 1 处理。
/// - `style`：随选区保存的外观参数。
///
/// # 返回
/// 可直接写入 `screenshot_selection/previous_selection` 的 JSON。
///
/// ```ignore
/// let value = encode(PhysicalRect::new(10, 20, 300, 200), 1.5, SelectionStyle::default());
/// ```
pub fn encode(rect: PhysicalRect, scale: f32, style: SelectionStyle) -> Value {
    let scale = if scale > 0.0 { scale } else { 1.0 };
    let x = to_logical(rect.x, scale);
    let y = to_logical(rect.y, scale);
    // 宽高按右下角换算，避免取整累积误差；至少 1
    let width = (to_logical(rect.right(), scale) - x).max(1);
    let height = (to_logical(rect.bottom(), scale) - y).max(1);
    json!({
        "rectangle": {"x": x, "y": y, "width": width, "height": height},
        "corner_radius": style.corner_radius.clamp(0, 256),
        "shadow_width": style.shadow_width.clamp(0, 64),
        "shadow_color": DEFAULT_SHADOW_COLOR,
        "lock_aspect_ratio": style.lock_aspect_ratio,
        "lock_drag_aspect_ratio": false,
    })
}

/// 从配置值还原选区，并裁到底图范围。
///
/// # 参数
/// - `value`：`previous_selection` 配置值。
/// - `scale`：显示缩放比，非正数按 1 处理。
/// - `bounds`：底图范围（物理坐标）。
/// - `min_size`：最小选区边长，裁剪后小于它视为无效。
///
/// # 返回
/// 底图物理坐标选区；值为 `null` / 结构不对 / 与底图无交集 / 太小时为 `None`。
///
/// ```ignore
/// let rect = decode(&store.document().value(PREVIOUS_SELECTION_KEY), 1.0, bounds, 8);
/// ```
pub fn decode(
    value: &Value,
    scale: f32,
    bounds: PhysicalRect,
    min_size: i32,
) -> Option<PhysicalRect> {
    let scale = if scale > 0.0 { scale } else { 1.0 };
    let rectangle = value.as_object()?.get("rectangle")?.as_object()?;
    let field = |key: &str| {
        rectangle
            .get(key)?
            .as_i64()
            .and_then(|n| i32::try_from(n).ok())
    };
    let (x, y, w, h) = (field("x")?, field("y")?, field("width")?, field("height")?);
    let to_physical = |n: i32| (n as f32 * scale).round() as i32;
    let left = to_physical(x).max(bounds.x);
    let top = to_physical(y).max(bounds.y);
    let right = to_physical(x.checked_add(w)?).min(bounds.right());
    let bottom = to_physical(y.checked_add(h)?).min(bounds.bottom());
    let (width, height) = (right - left, bottom - top);
    (width >= min_size && height >= min_size).then(|| PhysicalRect::new(left, top, width, height))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 测试用矩形构造。
    fn r(x: i32, y: i32, w: i32, h: i32) -> PhysicalRect {
        PhysicalRect::new(x, y, w, h)
    }

    /// 缩放 1 时往返不变，且字段与旧版格式一致。
    #[test]
    fn round_trip_at_unit_scale() {
        let rect = r(10, 20, 300, 200);
        let value = encode(
            rect,
            1.0,
            SelectionStyle {
                corner_radius: 8,
                shadow_width: 3,
                lock_aspect_ratio: true,
            },
        );
        assert_eq!(value["rectangle"]["width"], 300);
        assert_eq!(value["corner_radius"], 8);
        assert_eq!(value["shadow_color"], "#780A141E");
        assert_eq!(value["lock_drag_aspect_ratio"], false);
        assert_eq!(decode(&value, 1.0, r(0, 0, 1920, 1080), 8), Some(rect));
    }

    /// 高缩放下按逻辑坐标存，解码乘回物理坐标。
    #[test]
    fn stored_in_logical_coordinates() {
        let value = encode(r(150, 300, 450, 300), 1.5, SelectionStyle::default());
        assert_eq!(value["rectangle"]["x"], 100);
        assert_eq!(value["rectangle"]["width"], 300);
        assert_eq!(
            decode(&value, 1.5, r(0, 0, 3000, 2000), 8),
            Some(r(150, 300, 450, 300))
        );
    }

    /// 超出底图的部分被裁掉；裁完太小或无交集为 None。
    #[test]
    fn decode_clips_and_rejects() {
        let value = encode(r(900, 500, 400, 400), 1.0, SelectionStyle::default());
        assert_eq!(
            decode(&value, 1.0, r(0, 0, 1000, 600), 8),
            Some(r(900, 500, 100, 100))
        );
        assert_eq!(decode(&value, 1.0, r(0, 0, 905, 600), 8), None);
        assert_eq!(decode(&value, 1.0, r(0, 0, 500, 500), 8), None);
    }

    /// null、缺字段、类型错误一律 None。
    #[test]
    fn decode_rejects_garbage() {
        let bounds = r(0, 0, 100, 100);
        assert_eq!(decode(&Value::Null, 1.0, bounds, 1), None);
        assert_eq!(
            decode(&json!({"rectangle": {"x": 1}}), 1.0, bounds, 1),
            None
        );
        assert_eq!(
            decode(
                &json!({"rectangle": {"x": "a", "y": 0, "width": 5, "height": 5}}),
                1.0,
                bounds,
                1
            ),
            None
        );
    }

    /// 编码出的值能通过配置层的严格校验（与旧版互通）。
    #[test]
    fn encoded_value_passes_config_validation() {
        let value = encode(r(4, 5, 120, 80), 1.0, SelectionStyle::default());
        let normalized = snow_config::selection::normalize_selection(&value);
        assert!(normalized.valid);
    }
}
