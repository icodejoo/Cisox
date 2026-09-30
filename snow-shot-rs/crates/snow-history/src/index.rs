//! 截图历史 index.json 的 Rust 类型定义与校验，字段逐字对照
//! `snow_shot/src/storage/capturehistoryrepository.cpp` 与 `persistedselectioncodec.cpp`。
//! 各层用 `flatten extra` 保留未知字段，保证往返不丢。

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use std::path::Path;

/// 未知字段容器，序列化时原样写回。
pub type Extra = Map<String, Value>;

/// 当前写入的索引版本（C++ 侧 kIndexVersion）。
pub const INDEX_VERSION: i64 = 2;
/// 可读取的旧版本。
pub const LEGACY_INDEX_VERSION: i64 = 1;
/// 单条记录允许的最大显示器数量。
pub const MAX_DISPLAYS: usize = 32;
/// 画布历史文件名。
pub const CANVAS_FILE: &str = "canvas_history.json";
/// 截图结果图片文件名。
pub const RESULT_FILE: &str = "capture_result.png";

// 新增的范围边界常量
/// 1 MiB 字节数
pub const MIB: i64 = 1024 * 1024;
/// 单个画布历史记录的最大字节数 (16 MiB)
pub const MAX_CANVAS_BYTES: i64 = 16 * MIB;
/// 单张图片的最大像素数量
pub const MAX_PIXELS_PER_IMAGE: i64 = 64_000_000;
/// 单条记录允许包含的总像素数量上限
pub const MAX_PIXELS_PER_RECORD: i64 = 128_000_000;
/// 单条历史记录存储文件的总字节数上限
pub const MAX_STORED_BYTES: i64 = 1_i64 << 40;
/// 32位有符号整数最小值
pub const INT_MIN: i64 = -2147483648;
/// 32位有符号整数最大值
pub const INT_MAX: i64 = 2147483647;

/// 整数矩形（x/y/width/height）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Rect {
    pub x: i64,
    pub y: i64,
    pub width: i64,
    pub height: i64,
    #[serde(flatten)]
    pub extra: Extra,
}

/// 整数点。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Point {
    pub x: i64,
    pub y: i64,
    #[serde(flatten)]
    pub extra: Extra,
}

/// 桌面几何：space 为 "points"（macOS）或 "pixels"（Windows）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DesktopGeometry {
    pub space: String,
    pub x: i64,
    pub y: i64,
    #[serde(flatten)]
    pub extra: Extra,
}

/// 自定义形状的 rectangles 分支（版本必须为1，内容为四个元素的整数数组）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GeometryRectangles {
    pub version: i64,
    pub rectangles: Vec<Value>,
    #[serde(flatten)]
    pub extra: Extra,
}

/// 自定义形状的 operand 结构，包含操作类型与绘制指令。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Operand {
    pub operation: i64,
    #[serde(rename = "type")]
    pub type_id: String,
    pub fill: i64,
    pub commands: Vec<Value>, // 包含浮点数和整数类型，使用 Value 直接保留语义
    #[serde(flatten)]
    pub extra: Extra,
}

/// 自定义形状的 operands 分支。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GeometryOperands {
    pub version: i64,
    pub operands: Vec<Operand>,
    #[serde(flatten)]
    pub extra: Extra,
}

/// 自定义截取形状。由于没有明确的 Tag 字段区分，使用 untagged 让 Serde 尝试解析。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum RegionGeometry {
    Rectangles(GeometryRectangles),
    Operands(GeometryOperands),
    Unknown(Value),
}

/// 选区。`geometry`（自定义形状）与 `regions`（矩形并集）互斥。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Selection {
    pub rectangle: Rect,
    pub corner_radius: i64,
    pub shadow_width: i64,
    /// 形如 "#FF333333"（ARGB，大写）。
    pub shadow_color: String,
    pub lock_aspect_ratio: bool,
    pub lock_drag_aspect_ratio: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub geometry: Option<RegionGeometry>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub regions: Option<Value>,
    #[serde(flatten)]
    pub extra: Extra,
}

/// 单个显示器截图元数据。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Display {
    pub image_file: String,
    pub width: i64,
    pub height: i64,
    pub encoded_bytes: i64,
    pub stable_id: String,
    pub display_name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_canvas_origin: Option<Point>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_canvas_rect: Option<Rect>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub backing_scale: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub native_display_id: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub canvas_space: Option<String>,
    #[serde(flatten)]
    pub extra: Extra,
}

/// 结果图元数据。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ResultImage {
    pub image_file: String,
    pub width: i64,
    pub height: i64,
    pub encoded_bytes: i64,
    #[serde(flatten)]
    pub extra: Extra,
}

/// 一条历史记录。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Record {
    pub id: String,
    pub created_utc: String,
    pub source: String,
    pub canvas_bounds: Rect,
    pub selection: Selection,
    pub canvas_history_file: String,
    pub canvas_byte_size: i64,
    pub total_record_size: i64,
    pub displays: Vec<Display>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<ResultImage>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content_kind: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scrolling: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub desktop_geometry: Option<DesktopGeometry>,
    #[serde(flatten)]
    pub extra: Extra,
}

/// 待删除条目。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PendingDeletion {
    pub id: String,
    pub bytes: i64,
    #[serde(flatten)]
    pub extra: Extra,
}

/// index.json 根对象。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HistoryIndex {
    pub format_version: i64,
    pub records: Vec<Record>,
    pub pending_deletions: Vec<PendingDeletion>,
    #[serde(flatten)]
    pub extra: Extra,
}

/// 解析 index.json 字节；版本非 1/2 时报错（与 C++ 一致）。
pub fn load_index(bytes: &[u8]) -> Result<HistoryIndex, String> {
    let index: HistoryIndex = serde_json::from_slice(bytes).map_err(|e| e.to_string())?;
    if index.format_version != INDEX_VERSION && index.format_version != LEGACY_INDEX_VERSION {
        return Err(format!("不支持的 format_version: {}", index.format_version));
    }
    Ok(index)
}

/// 递归按键字母升序对 JSON Object 进行排序，确保不受 serde_json preserve_order 特性影响。
fn sort_json_value(v: &mut Value) {
    match v {
        Value::Array(arr) => {
            for item in arr {
                sort_json_value(item);
            }
        }
        Value::Object(map) => {
            for item in map.values_mut() {
                sort_json_value(item);
            }
            let mut entries: Vec<(String, Value)> = std::mem::take(map).into_iter().collect();
            entries.sort_by(|a, b| a.0.cmp(&b.0));
            for (k, v) in entries {
                map.insert(k, v);
            }
        }
        _ => {}
    }
}

/// 紧凑序列化；键按字母序升序输出（与 Qt C++ QJsonDocument 紧凑格式逐字节一致）。
pub fn save_index(index: &HistoryIndex) -> Result<Vec<u8>, String> {
    let mut value = serde_json::to_value(index).map_err(|e| e.to_string())?;
    sort_json_value(&mut value);
    serde_json::to_vec(&value).map_err(|e| e.to_string())
}

/// 把数字统一成 f64 再比较，忽略 `1` 与 `1.0` 的写法差异；其余按结构逐字段比较。
pub fn json_semantic_eq(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::Number(x), Value::Number(y)) => x.as_f64() == y.as_f64(),
        (Value::Array(x), Value::Array(y)) => {
            x.len() == y.len() && x.iter().zip(y).all(|(p, q)| json_semantic_eq(p, q))
        }
        (Value::Object(x), Value::Object(y)) => {
            x.len() == y.len()
                && x.iter()
                    .all(|(k, v)| y.get(k).is_some_and(|w| json_semantic_eq(v, w)))
        }
        _ => a == b,
    }
}

/// 全零 UUID 文本（Qt 侧拒绝，视为非法）。
const NIL_UUID: &str = "00000000-0000-0000-0000-000000000000";

/// 判断是否为无花括号的小写 UUID 文本。
pub fn is_valid_uuid(id: &str) -> bool {
    let parts: Vec<&str> = id.split('-').collect();
    let lens = [8, 4, 4, 4, 12];
    id != NIL_UUID
        && parts.len() == 5
        && parts.iter().zip(lens).all(|(p, n)| {
            p.len() == n
                && p.chars()
                    .all(|c| c.is_ascii_digit() || ('a'..='f').contains(&c))
        })
}

/// 提取确切的整数，模拟 C++ 的 isDouble + floor() == coord 判定。
fn get_int(v: &Value, name: &str) -> Result<i64, String> {
    let f = v.as_f64().ok_or_else(|| format!("{} 必须是数字", name))?;
    if f.floor() != f {
        return Err(format!("{} 必须是整数", name));
    }
    Ok(f as i64)
}

/// 校验自定义形状 (geometry) 的有效性
pub fn validate_geometry(g: &RegionGeometry) -> Result<(), String> {
    match g {
        RegionGeometry::Unknown(_) => Err("未知的 geometry 格式".into()),
        RegionGeometry::Rectangles(r) => {
            if r.extra.contains_key("operands") {
                return Err("rectangles 与 operands 互斥".into());
            }
            if r.version != 1 {
                return Err("geometry.version 必须为 1".into());
            }
            if r.rectangles.len() > 65536 {
                return Err("rectangles 数量超过 65536".into());
            }
            for rect_val in &r.rectangles {
                let rect = rect_val.as_array().ok_or("rectangle 必须是数组")?;
                if rect.len() != 4 {
                    return Err("rectangle 长度必须为 4".into());
                }
                for item in rect {
                    let val = get_int(item, "rectangle 坐标")?;
                    // 先做范围检查再判断，避免 i64::MIN.abs() 溢出（高优 bug #3）
                    if !(-10_000_000..=10_000_000).contains(&val) {
                        return Err("rectangles 坐标绝对值越界 (>10000000)".into());
                    }
                }
                let w = get_int(&rect[2], "width")?;
                let h = get_int(&rect[3], "height")?;
                if w < 1 || h < 1 {
                    return Err("rectangles 宽或高 < 1".into());
                }
            }
            Ok(())
        }
        RegionGeometry::Operands(o) => {
            if o.extra.contains_key("rectangles") {
                return Err("rectangles 与 operands 互斥".into());
            }
            if o.version != 1 {
                return Err("geometry.version 必须为 1".into());
            }
            if o.operands.is_empty() || o.operands.len() > 4096 {
                return Err("operands 为空或数量超过 4096".into());
            }
            let mut total_cmds = 0;
            for (i, op) in o.operands.iter().enumerate() {
                if op.operation < 0 || op.operation > 2 {
                    return Err("operation 非法".into());
                }
                if i == 0 && op.operation != 0 {
                    return Err("首个 operand 的 operation 必须为 0 (Add)".into());
                }
                if op.fill < 0 || op.fill > 1 {
                    return Err("fill 非法".into());
                }
                if op.type_id != "rectangle"
                    && op.type_id != "polyline"
                    && op.type_id != "curve"
                    && op.type_id != "freehand"
                {
                    return Err("未知的 geometry operand type".into());
                }
                if op.commands.is_empty() {
                    return Err("commands 不能为空".into());
                }
                total_cmds += op.commands.len();
                if total_cmds > 1048576 {
                    return Err("commands 总数越界 (>1048576)".into());
                }

                let mut c = 0;
                while c < op.commands.len() {
                    let arr = op.commands[c].as_array().ok_or("command 必须为数组")?;
                    if arr.len() != 3 {
                        return Err("command 元素必须有 3 个".into());
                    }
                    let kind = get_int(&arr[0], "command kind")?;
                    let x = arr[1].as_f64().ok_or("command x 必须是数字")?;
                    let y = arr[2].as_f64().ok_or("command y 必须是数字")?;
                    if !x.is_finite()
                        || !y.is_finite()
                        || x.abs() > 10000000.0
                        || y.abs() > 10000000.0
                    {
                        return Err("command 坐标不合法或越界".into());
                    }
                    if c == 0 && kind != 0 {
                        return Err("operand 第一条指令必须为 moveTo (0)".into());
                    }

                    if kind == 0 || kind == 1 {
                        c += 1;
                    } else if kind == 2 {
                        if c + 2 >= op.commands.len() {
                            return Err("cubicTo 缺少控制点".into());
                        }
                        for j in 1..=2 {
                            let next_arr =
                                op.commands[c + j].as_array().ok_or("command 必须为数组")?;
                            if next_arr.len() != 3 || get_int(&next_arr[0], "command kind")? != 3 {
                                return Err("cubicTo 后面必须紧跟两个 kind=3 的指令".into());
                            }
                            let nx = next_arr[1].as_f64().ok_or("坐标非数字")?;
                            let ny = next_arr[2].as_f64().ok_or("坐标非数字")?;
                            if !nx.is_finite()
                                || !ny.is_finite()
                                || nx.abs() > 10000000.0
                                || ny.abs() > 10000000.0
                            {
                                return Err("command 坐标不合法或越界".into());
                            }
                        }
                        c += 3;
                    } else {
                        return Err(format!("未知的 command kind: {}", kind));
                    }
                }
            }
            Ok(())
        }
    }
}

/// 校验选区 (selection) 及可能的组合情况
pub fn validate_selection(selection: &Selection) -> Result<(), String> {
    let r = &selection.rectangle;
    if r.x < INT_MIN || r.x > INT_MAX || r.y < INT_MIN || r.y > INT_MAX {
        return Err("selection.rectangle x/y 越界".into());
    }
    if r.width < 1 || r.width > INT_MAX || r.height < 1 || r.height > INT_MAX {
        return Err("selection.rectangle w/h 越界".into());
    }
    if r.x as i128 + r.width as i128 > INT_MAX as i128
        || r.y as i128 + r.height as i128 > INT_MAX as i128
    {
        return Err("selection.rectangle border 超过 INT_MAX".into());
    }

    if selection.corner_radius < 0 || selection.corner_radius > 256 {
        return Err("corner_radius 越界".into());
    }
    if selection.shadow_width < 0 || selection.shadow_width > 64 {
        return Err("shadow_width 越界".into());
    }

    // 近似的 16 进制颜色格式检查
    if !selection.shadow_color.starts_with('#')
        || (selection.shadow_color.len() != 7 && selection.shadow_color.len() != 9)
    {
        return Err("shadow_color 不是近似的十六进制格式".into());
    }

    if let Some(geom) = &selection.geometry {
        validate_geometry(geom)?;
    } else if let Some(regions_val) = &selection.regions {
        let arr = regions_val.as_array().ok_or("regions 必须是数组")?;
        if arr.is_empty() || arr.len() > 65536 {
            return Err("regions 数量越界".into());
        }

        let mut min_left = INT_MAX;
        let mut max_right = INT_MIN;
        let mut min_top = INT_MAX;
        let mut max_bottom = INT_MIN;

        for (i, item) in arr.iter().enumerate() {
            let obj = item.as_object().ok_or("regions 元素必须是对象")?;
            let x = get_int(obj.get("x").unwrap_or(&Value::Null), "regions x")?;
            let y = get_int(obj.get("y").unwrap_or(&Value::Null), "regions y")?;
            let w = get_int(obj.get("width").unwrap_or(&Value::Null), "regions w")?;
            let h = get_int(obj.get("height").unwrap_or(&Value::Null), "regions h")?;

            if !(INT_MIN..=INT_MAX).contains(&x) || !(INT_MIN..=INT_MAX).contains(&y) {
                return Err("regions x/y 越界".into());
            }
            if !(1..=INT_MAX).contains(&w) || !(1..=INT_MAX).contains(&h) {
                return Err("regions w/h 越界".into());
            }
            if x as i128 + w as i128 > INT_MAX as i128 || y as i128 + h as i128 > INT_MAX as i128 {
                return Err("regions border 超过 INT_MAX".into());
            }

            let left = x;
            let right = x + w - 1;
            let top = y;
            let bottom = y + h - 1;

            if i > 0 {
                let u_left = min_left.min(left);
                let u_right = max_right.max(right);
                let u_top = min_top.min(top);
                let u_bottom = max_bottom.max(bottom);

                if (u_right as i128 - u_left as i128) >= INT_MAX as i128 {
                    return Err("regions 并集宽度 >= INT_MAX".into());
                }
                if (u_bottom as i128 - u_top as i128) >= INT_MAX as i128 {
                    return Err("regions 并集高度 >= INT_MAX".into());
                }

                min_left = u_left;
                max_right = u_right;
                min_top = u_top;
                max_bottom = u_bottom;
            } else {
                min_left = left;
                max_right = right;
                min_top = top;
                max_bottom = bottom;
            }
        }
    }
    Ok(())
}

/// 解析记录里的单张图片边界情况并累加像素数
fn parse_image(w: i64, h: i64, bytes: i64, pixels: &mut i64) -> Result<(), String> {
    if !(1..=MAX_PIXELS_PER_IMAGE).contains(&w) {
        return Err("图片宽度越界".into());
    }
    if !(1..=MAX_PIXELS_PER_IMAGE).contains(&h) {
        return Err("图片高度越界".into());
    }
    if !(1..=MAX_STORED_BYTES).contains(&bytes) {
        return Err("图片 encoded_bytes 越界".into());
    }

    let img_pixels = w.checked_mul(h).ok_or("图片像素数计算溢出")?;
    if img_pixels > MAX_PIXELS_PER_IMAGE {
        return Err("单张图片像素数越界".into());
    }
    if *pixels > MAX_PIXELS_PER_RECORD - img_pixels {
        return Err("记录总像素数越界".into());
    }

    *pixels += img_pixels;
    Ok(())
}

/// 内存校验单条 record 的有效范围与逻辑结构。
pub fn validate_record(r: &Record) -> Result<(), String> {
    const SOURCES: [&str; 5] = [
        "copied_to_clipboard",
        "saved_to_file",
        "pinned_to_screen",
        "current_monitor",
        "focused_window",
    ];
    if !is_valid_uuid(&r.id) {
        return Err("id 不是合法 UUID".into());
    }
    if !r.created_utc.ends_with('Z') {
        return Err("created_utc 必须以 Z 结尾".into());
    }
    if !SOURCES.contains(&r.source.as_str()) {
        return Err(format!("未知 source: {}", r.source));
    }
    if r.canvas_history_file != CANVAS_FILE {
        return Err("canvas_history_file 命名不符".into());
    }
    if r.displays.is_empty() || r.displays.len() > MAX_DISPLAYS {
        return Err("displays 数量越界".into());
    }

    if r.canvas_byte_size < 1 || r.canvas_byte_size > MAX_CANVAS_BYTES {
        return Err("canvas_byte_size 越界".into());
    }
    if r.total_record_size < 1 || r.total_record_size > MAX_STORED_BYTES {
        return Err("total_record_size 越界".into());
    }

    let b = &r.canvas_bounds;
    if b.x < INT_MIN || b.x > INT_MAX || b.y < INT_MIN || b.y > INT_MAX {
        return Err("canvas_bounds x/y 越界".into());
    }
    if b.width < 1 || b.width > INT_MAX || b.height < 1 || b.height > INT_MAX {
        return Err("canvas_bounds w/h 越界".into());
    }
    // 依据 parseRecord: x + width - 1 <= INT_MAX
    if b.x as i128 + b.width as i128 - 1 > INT_MAX as i128
        || b.y as i128 + b.height as i128 - 1 > INT_MAX as i128
    {
        return Err("canvas_bounds border 超过 INT_MAX".into());
    }

    if let Some(g) = &r.desktop_geometry {
        if g.space != "points" && g.space != "pixels" {
            return Err("desktop_geometry.space 非法".into());
        }
        if g.x < INT_MIN || g.x > INT_MAX || g.y < INT_MIN || g.y > INT_MAX {
            return Err("desktop_geometry x/y 越界".into());
        }
    }

    validate_selection(&r.selection)?;

    let mut total_bytes = r.canvas_byte_size;
    let mut pixels = 0_i64;

    if let Some(res) = &r.result {
        if res.image_file != RESULT_FILE {
            return Err("result.image_file 命名不符".into());
        }
        parse_image(res.width, res.height, res.encoded_bytes, &mut pixels)?;
        total_bytes += res.encoded_bytes;
    }

    for (i, d) in r.displays.iter().enumerate() {
        if d.image_file != format!("display_{i}.png") {
            return Err(format!("displays[{i}].image_file 命名不符"));
        }
        parse_image(d.width, d.height, d.encoded_bytes, &mut pixels)?;
        total_bytes += d.encoded_bytes;

        if let Some(orig) = &d.source_canvas_origin {
            if orig.x < INT_MIN || orig.x as i128 > INT_MAX as i128 - d.width as i128 + 1 {
                return Err("source_canvas_origin x 越界".into());
            }
            if orig.y < INT_MIN || orig.y as i128 > INT_MAX as i128 - d.height as i128 + 1 {
                return Err("source_canvas_origin y 越界".into());
            }
        }

        if let Some(rect) = &d.source_canvas_rect {
            let space = d.canvas_space.as_deref().unwrap_or("");
            if space != "points" && space != "pixels" {
                return Err("canvas_space 缺失或非法".into());
            }

            if rect.width < 1 || rect.width > INT_MAX || rect.height < 1 || rect.height > INT_MAX {
                return Err("source_canvas_rect w/h 越界".into());
            }
            if rect.x < INT_MIN || rect.x as i128 > INT_MAX as i128 - rect.width as i128 {
                return Err("source_canvas_rect x 越界".into());
            }
            if rect.y < INT_MIN || rect.y as i128 > INT_MAX as i128 - rect.height as i128 {
                return Err("source_canvas_rect y 越界".into());
            }

            let scale = if let Some(s) = d.backing_scale {
                s
            } else {
                let s_w = d.width as f64 / rect.width as f64;
                let s_h = d.height as f64 / rect.height as f64;
                s_w.max(s_h)
            };
            if !scale.is_finite() || scale <= 0.0 {
                return Err("backing_scale 必须有限且 > 0".into());
            }

            if let Some(nid) = d.native_display_id
                && (!(0..=4294967295).contains(&nid))
            {
                return Err("native_display_id 越界".into());
            }
        }
    }

    if total_bytes != r.total_record_size {
        return Err(format!(
            "total_record_size {} != 合计 {}",
            r.total_record_size, total_bytes
        ));
    }
    match r.content_kind.as_deref() {
        None => {}
        Some("image") if r.displays.len() == 1 && r.result.is_some() => {}
        Some(other) => return Err(format!("content_kind 非法或 image 记录结构不符: {other}")),
    }

    Ok(())
}

/// 校验总大小上限。
pub fn check_total_bytes(total: i64, add: i64) -> Result<(), String> {
    if total > MAX_STORED_BYTES - add {
        return Err("累计大小超过上限".into());
    }
    Ok(())
}

/// 纯内存校验整个 Index 层（总量上限、ID 唯一等），此函数会隐式检查全部子记录。
pub fn validate_index(index: &HistoryIndex) -> Result<(), String> {
    let mut total_bytes = 0_i64;
    let mut id_set = std::collections::HashSet::new();

    for r in &index.records {
        validate_record(r)?;
        if !id_set.insert(&r.id) {
            return Err(format!("记录 ID 重复: {}", r.id));
        }
        check_total_bytes(total_bytes, r.total_record_size)
            .map_err(|_| "记录 total_record_size 累计超过上限".to_string())?;
        total_bytes += r.total_record_size;
    }

    for p in &index.pending_deletions {
        if !is_valid_uuid(&p.id) {
            return Err(format!("pending_deletions ID 不合法: {}", p.id));
        }
        if !id_set.insert(&p.id) {
            return Err(format!("pending_deletions ID 冲突: {}", p.id));
        }
        if p.bytes < 0 || p.bytes > MAX_STORED_BYTES {
            return Err("pending_deletions bytes 越界".into());
        }
        check_total_bytes(total_bytes, p.bytes)
            .map_err(|_| "包含 pending_deletions 在内的存储累计超过上限".to_string())?;
        total_bytes += p.bytes;
    }

    Ok(())
}

/// 落盘校验：内存校验 + 各文件真实字节数与索引一致 + canvas 为合法 JSON。
pub fn validate_index_on_disk(index: &HistoryIndex, root: &Path) -> Result<(), String> {
    validate_index(index)?;
    for r in &index.records {
        let dir = root.join("records").join(&r.id);
        let size = |name: &str| {
            std::fs::metadata(dir.join(name))
                .map(|m| m.len() as i64)
                .map_err(|e| format!("{name}: {e}"))
        };
        if size(CANVAS_FILE)? != r.canvas_byte_size {
            return Err("canvas 文件大小不符".into());
        }
        let canvas = std::fs::read(dir.join(CANVAS_FILE)).map_err(|e| e.to_string())?;
        let v: Value = serde_json::from_slice(&canvas).map_err(|e| e.to_string())?;
        if !v.is_object() && !v.is_array() {
            return Err("canvas 必须是对象或数组".into());
        }

        if let Some(res) = &r.result
            && size(&res.image_file)? != res.encoded_bytes
        {
            return Err("result 文件大小不符".into());
        }
        for d in &r.displays {
            if size(&d.image_file)? != d.encoded_bytes {
                return Err(format!("{} 大小不符", d.image_file));
            }
        }
    }
    Ok(())
}

#[cfg(test)]
#[path = "index_tests.rs"]
mod tests;
