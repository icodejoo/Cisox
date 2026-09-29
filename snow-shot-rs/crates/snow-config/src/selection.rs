//! 持久化选区（`screenshot_selection/previous_selection` 与 `selection_rect_presets`）的规范化。
//!
//! 移植自 C++ `persistedselectioncodec.cpp`。C++ 依赖 `QColor`/`QRegion`/
//! `ScreenshotRegionGeometry`，这里是受限子集，与 C++ 的差异：
//! - 颜色仅接受 `#RGB`/`#RRGGBB`/`#AARRGGBB`，QColor 的命名色一律视为非法；
//! - `regions` 只校验、原样保留（不做 QRegion 的不相交矩形分解），`rectangle` 取外接矩形；
//! - `geometry` 只校验是 `version == 1` 的对象并原样保留，不重算 `rectangle`。
//!
//! 初稿由 antigravity 产出并经复审。

use crate::value::{Normalization, as_integer, int_value, json_eq, trimmed};
use serde_json::{Map, Value};

/// 圆角半径上限。
const MAXIMUM_CORNER_RADIUS: i32 = 256;
/// 阴影宽度上限。
const MAXIMUM_SHADOW_WIDTH: i32 = 64;
/// `regions` 数组最大长度。
const MAXIMUM_REGION_COUNT: usize = 65536;

/// 解析并校验矩形对象，返回 `(x, y, width, height)`。
fn parse_rectangle(value: &Value) -> Option<(i32, i32, i32, i32)> {
    let object = value.as_object()?;
    let x = as_integer(object.get("x")?)?;
    let y = as_integer(object.get("y")?)?;
    let width = as_integer(object.get("width")?)?;
    let height = as_integer(object.get("height")?)?;
    let limit = i64::from(i32::MAX);
    if width < 1 || height < 1 || i64::from(x) + i64::from(width) > limit {
        return None;
    }
    if i64::from(y) + i64::from(height) > limit {
        return None;
    }
    Some((x, y, width, height))
}

/// 在给定范围内读取整数字段。
fn ranged_integer(object: &Map<String, Value>, key: &str, min: i32, max: i32) -> Option<i32> {
    as_integer(object.get(key)?).filter(|number| (min..=max).contains(number))
}

/// 把十六进制颜色规范化为大写 `#AARRGGBB`；非法返回 `None`。
fn canonical_argb(text: &str) -> Option<String> {
    let hex = text.strip_prefix('#')?;
    if !hex.chars().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    let canonical = match hex.len() {
        3 => {
            let expanded: String = hex.chars().flat_map(|c| [c, c]).collect();
            format!("#FF{expanded}")
        }
        6 => format!("#FF{hex}"),
        8 => format!("#{hex}"),
        _ => return None,
    };
    Some(canonical.to_ascii_uppercase())
}

/// 校验 `regions` 并返回外接矩形 `(x, y, width, height)`。
fn regions_bounds(regions: &Value) -> Option<(i32, i32, i32, i32)> {
    let items = regions.as_array()?;
    if items.is_empty() || items.len() > MAXIMUM_REGION_COUNT {
        return None;
    }
    let limit = i64::from(i32::MAX);
    let (mut left, mut top, mut right, mut bottom) = (i32::MAX, i32::MAX, i32::MIN, i32::MIN);
    let mut first = true;
    for item in items {
        let (x, y, width, height) = parse_rectangle(item)?;
        let (item_right, item_bottom) = (x + width - 1, y + height - 1);
        if !first
            && (i64::from(right.max(item_right)) - i64::from(left.min(x)) >= limit
                || i64::from(bottom.max(item_bottom)) - i64::from(top.min(y)) >= limit)
        {
            return None;
        }
        first = false;
        left = left.min(x);
        top = top.min(y);
        right = right.max(item_right);
        bottom = bottom.max(item_bottom);
    }
    let width = i64::from(right) - i64::from(left) + 1;
    let height = i64::from(bottom) - i64::from(top) + 1;
    if width < 1 || height < 1 || width > limit || height > limit {
        return None;
    }
    Some((
        left,
        top,
        i32::try_from(width).ok()?,
        i32::try_from(height).ok()?,
    ))
}

/// 规范化单个持久化选区对象。
///
/// # 参数
/// - `value`：待规范化的选区 JSON
///
/// # 返回
/// `Some((规范化对象, 是否与输入不同))`；非法返回 `None`。
///
/// # 示例
/// ```
/// use serde_json::json;
/// use snow_config::selection::normalize_persisted_selection;
///
/// let value = json!({
///     "rectangle": {"x": 0, "y": 0, "width": 100, "height": 100},
///     "corner_radius": 0, "shadow_width": 0, "shadow_color": "#000000FF",
///     "lock_aspect_ratio": false, "lock_drag_aspect_ratio": false
/// });
/// assert!(normalize_persisted_selection(&value).is_some());
/// ```
pub fn normalize_persisted_selection(value: &Value) -> Option<(Value, bool)> {
    let object = value.as_object()?;
    let mut rectangle = parse_rectangle(object.get("rectangle")?)?;
    let corner_radius = ranged_integer(object, "corner_radius", 0, MAXIMUM_CORNER_RADIUS)?;
    let shadow_width = ranged_integer(object, "shadow_width", 0, MAXIMUM_SHADOW_WIDTH)?;
    let shadow_color = canonical_argb(object.get("shadow_color")?.as_str()?)?;
    let lock_aspect_ratio = object.get("lock_aspect_ratio")?.as_bool()?;
    let lock_drag_aspect_ratio = object.get("lock_drag_aspect_ratio")?.as_bool()?;

    let mut extra: Option<(&str, Value)> = None;
    if let Some(geometry) = object.get("geometry") {
        let version = as_integer(geometry.as_object()?.get("version")?)?;
        if version != 1 {
            return None;
        }
        extra = Some(("geometry", geometry.clone()));
    } else if let Some(regions) = object.get("regions") {
        rectangle = regions_bounds(regions)?;
        extra = Some(("regions", regions.clone()));
    }

    let (x, y, width, height) = rectangle;
    let mut rectangle_map = Map::new();
    for (key, number) in [("x", x), ("y", y), ("width", width), ("height", height)] {
        rectangle_map.insert(key.into(), int_value(i64::from(number)));
    }
    let mut result = Map::new();
    result.insert("rectangle".into(), Value::Object(rectangle_map));
    result.insert("corner_radius".into(), int_value(i64::from(corner_radius)));
    result.insert("shadow_width".into(), int_value(i64::from(shadow_width)));
    result.insert("shadow_color".into(), Value::String(shadow_color));
    result.insert("lock_aspect_ratio".into(), Value::Bool(lock_aspect_ratio));
    result.insert(
        "lock_drag_aspect_ratio".into(),
        Value::Bool(lock_drag_aspect_ratio),
    );
    if let Some((key, extra_value)) = extra {
        result.insert(key.into(), extra_value);
    }
    let normalized = Value::Object(result);
    let changed = !json_eq(&normalized, value);
    Some((normalized, changed))
}

/// 规范化 `screenshot_selection/previous_selection`：`null` 合法，其余按选区对象处理。
///
/// # 示例
/// ```
/// use serde_json::json;
/// use snow_config::selection::normalize_selection;
///
/// assert!(normalize_selection(&json!(null)).valid);
/// ```
pub fn normalize_selection(value: &Value) -> Normalization {
    if value.is_null() {
        return Normalization::ok(Value::Null, false);
    }
    match normalize_persisted_selection(value) {
        Some((normalized, changed)) => Normalization::ok(normalized, changed),
        None => Normalization::invalid(),
    }
}

/// 规范化 `screenshot_selection/selection_rect_presets`：逐项要求非空名称且选区合法，非法项被丢弃。
///
/// # 示例
/// ```
/// use serde_json::json;
/// use snow_config::selection::normalize_presets;
///
/// assert!(normalize_presets(&json!([])).valid);
/// ```
pub fn normalize_presets(value: &Value) -> Normalization {
    let Some(items) = value.as_array() else {
        return Normalization::invalid();
    };
    let mut result = Vec::new();
    let mut changed = false;
    for item in items {
        let Some(object) = item.as_object() else {
            changed = true;
            continue;
        };
        let name = trimmed(
            object
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or_default(),
        );
        let selection = normalize_persisted_selection(item);
        let Some((mut normalized, selection_changed)) = selection.filter(|_| !name.is_empty())
        else {
            changed = true;
            continue;
        };
        if let Value::Object(map) = &mut normalized {
            map.insert("name".into(), Value::String(name.to_string()));
        }
        changed = changed || selection_changed || !json_eq(&normalized, item);
        result.push(normalized);
    }
    Normalization::ok(Value::Array(result), changed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// 生成合法选区 JSON。
    fn sample() -> Value {
        json!({
            "rectangle": {"x": 4, "y": 5, "width": 120, "height": 80},
            "corner_radius": 8, "shadow_width": 3, "shadow_color": "#780A141E",
            "lock_aspect_ratio": true, "lock_drag_aspect_ratio": false
        })
    }

    /// 规范选区往返不变（C++ persistedSelectionCodecIsCanonicalAndStrict）。
    #[test]
    fn canonical_round_trip() {
        let (normalized, changed) = normalize_persisted_selection(&sample()).unwrap();
        assert!(!changed && normalized == sample());
    }

    /// 越界圆角、缺字段、非法颜色均非法。
    #[test]
    fn strict_validation() {
        let mut bad = sample();
        bad["corner_radius"] = json!(257);
        assert!(normalize_persisted_selection(&bad).is_none());
        let mut bad = sample();
        bad["shadow_width"] = json!(65);
        assert!(normalize_persisted_selection(&bad).is_none());
        let mut bad = sample();
        bad["shadow_color"] = json!("red");
        assert!(normalize_persisted_selection(&bad).is_none());
        let mut bad = sample();
        bad.as_object_mut()
            .unwrap()
            .remove("lock_drag_aspect_ratio");
        assert!(normalize_persisted_selection(&bad).is_none());
        let mut bad = sample();
        bad["rectangle"]["width"] = json!(0);
        assert!(normalize_persisted_selection(&bad).is_none());
    }

    /// 颜色写法规范化为大写 #AARRGGBB。
    #[test]
    fn color_forms() {
        assert_eq!(canonical_argb("#abc").as_deref(), Some("#FFAABBCC"));
        assert_eq!(canonical_argb("#0a141e").as_deref(), Some("#FF0A141E"));
        assert_eq!(canonical_argb("#780a141e").as_deref(), Some("#780A141E"));
        assert_eq!(canonical_argb("#12345"), None);
    }

    /// regions：外接矩形写回 rectangle，原样保留 regions；geometry 优先于 regions。
    #[test]
    fn regions_and_geometry() {
        let mut value = sample();
        value["regions"] = json!([
            {"x": 0, "y": 0, "width": 10, "height": 10},
            {"x": 20, "y": 30, "width": 10, "height": 10}
        ]);
        let (normalized, changed) = normalize_persisted_selection(&value).unwrap();
        assert!(changed);
        assert_eq!(
            normalized["rectangle"],
            json!({"x": 0, "y": 0, "width": 30, "height": 40})
        );
        assert_eq!(normalized["regions"], value["regions"]);
        let mut with_geometry = value.clone();
        with_geometry["geometry"] = json!({"version": 1, "rectangles": []});
        let (normalized, _) = normalize_persisted_selection(&with_geometry).unwrap();
        assert!(normalized.get("regions").is_none() && normalized.get("geometry").is_some());
        with_geometry["geometry"] = json!({"version": 2});
        assert!(normalize_persisted_selection(&with_geometry).is_none());
        value["regions"] = json!([]);
        assert!(normalize_persisted_selection(&value).is_none());
    }

    /// null 合法；预设需要非空名称，丢弃非法项。
    #[test]
    fn selection_and_presets() {
        assert_eq!(
            normalize_selection(&json!(null)),
            Normalization::ok(Value::Null, false)
        );
        assert!(!normalize_selection(&json!(5)).valid);
        let mut named = sample();
        named["name"] = json!("  Wide  ");
        let mut unnamed = sample();
        unnamed["name"] = json!("   ");
        let out = normalize_presets(&json!([named, unnamed, 7]));
        assert!(out.valid && out.changed);
        assert_eq!(out.value.as_array().unwrap().len(), 1);
        assert_eq!(out.value[0]["name"], json!("Wide"));
        assert!(!normalize_presets(&json!({})).valid);
    }
}
