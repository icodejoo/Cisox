//! 贴图的纯数据模型与纯函数：清单记录读写、容量 / 淘汰策略、拖动缩放矩形、配置项解析。
//!
//! 全部不依赖 GPUI，可离屏单测。窗口、持久化接线在 `pinned_view` / `pinned_manager`。

use serde_json::{Map, Value, json};
use snow_config::document::ConfigDocument;
use snow_history::pinned::SourceKind;
use snow_ui::shell::geometry::{PhysicalPoint, PhysicalRect};
use snow_ui::shell::pinned_geometry::{
    PinnedDragHandle, ScaleAnchor, anchored_scale_rect, proportional_resize_rect, step_opacity,
    step_zoom,
};

/// 贴图窗口最小边长（物理像素）。
pub const PIN_MIN_SIZE: i32 = 50;
/// 贴图窗口最大边长（物理像素）。
pub const PIN_MAX_SIZE: i32 = 16384;
/// 缩放倍率下限。
pub const ZOOM_MIN: f32 = 0.2;
/// 缩放倍率上限。
pub const ZOOM_MAX: f32 = 5.0;
/// 不透明度下限。
pub const OPACITY_MIN: f32 = 0.1;
/// 不透明度上限。
pub const OPACITY_MAX: f32 = 1.0;
/// Windows 一个滚轮刻度对应的“行数”（系统默认每刻度 3 行）。
pub const WHEEL_LINES_PER_NOTCH: f32 = 3.0;
/// 手柄显示边长（物理像素）。
pub const HANDLE_SIZE: i32 = 8;
/// 手柄边缘吸附容差（物理像素）。
pub const EDGE_MARGIN: i32 = 4;
/// 恢复窗口时，与任一显示器至少要有这么大的重叠才算“可见”。
pub const MIN_VISIBLE_OVERLAP: i32 = 32;
/// 恢复窗口落回屏幕时的层叠偏移（物理像素）。
pub const RESTORE_CASCADE_STEP: i32 = 32;
/// 一天的毫秒数。
const MS_PER_DAY: i64 = 86_400_000;
/// 一 MiB 的字节数。
const BYTES_PER_MIB: u64 = 1024 * 1024;

/// 清单记录里窗口几何对象的键。
const KEY_GEOMETRY: &str = "window_geometry";
/// 清单记录里创建时间（UTC 毫秒）的键。
const KEY_CREATED_MS: &str = "created_at_ms";
/// 清单记录里 payload 体积（字节）的键。
const KEY_PAYLOAD_BYTES: &str = "payload_bytes";

/// 贴图窗口几何与显示状态（可持久化）。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PinGeometry {
    /// 窗口左上角 x（屏幕物理像素）。
    pub x: i32,
    /// 窗口左上角 y。
    pub y: i32,
    /// 窗口宽。
    pub width: i32,
    /// 窗口高。
    pub height: i32,
    /// 缩放倍率（相对原图）。
    pub zoom: f32,
    /// 不透明度。
    pub opacity: f32,
    /// 是否置顶。
    pub topmost: bool,
}

impl PinGeometry {
    /// 由窗口外框与显示状态构造。
    ///
    /// # 参数
    /// - `rect`：窗口外框。
    /// - `zoom`：缩放倍率。
    /// - `opacity`：不透明度。
    /// - `topmost`：是否置顶。
    pub fn new(rect: PhysicalRect, zoom: f32, opacity: f32, topmost: bool) -> Self {
        Self {
            x: rect.x,
            y: rect.y,
            width: rect.width,
            height: rect.height,
            zoom,
            opacity,
            topmost,
        }
    }

    /// 窗口外框。
    pub fn rect(&self) -> PhysicalRect {
        PhysicalRect::new(self.x, self.y, self.width, self.height)
    }

    /// 序列化为清单里的几何对象。
    pub fn to_json(&self) -> Value {
        json!({
            "x": self.x,
            "y": self.y,
            "width": self.width,
            "height": self.height,
            "zoom": self.zoom,
            "opacity": self.opacity,
            "pinned_on_top": self.topmost,
        })
    }

    /// 从清单几何对象解析；缺字段 / 尺寸非法返回 `None`，缩放与透明度按范围钳制。
    ///
    /// # 参数
    /// - `value`：清单里的 `window_geometry` 值。
    ///
    /// ```
    /// use serde_json::json;
    /// use snow_shot::pinned_model::PinGeometry;
    /// let g = PinGeometry::from_json(&json!({"x": 1, "y": 2, "width": 100, "height": 50,
    ///     "zoom": 9.0, "opacity": 0.5, "pinned_on_top": true})).unwrap();
    /// assert_eq!(g.zoom, 5.0);
    /// assert!(PinGeometry::from_json(&json!({"x": 1})).is_none());
    /// ```
    pub fn from_json(value: &Value) -> Option<Self> {
        let int = |key: &str| value.get(key)?.as_i64().and_then(|n| i32::try_from(n).ok());
        let float = |key: &str, default: f32| {
            value
                .get(key)
                .and_then(Value::as_f64)
                .filter(|f| f.is_finite())
                .map_or(default, |f| f as f32)
        };
        let (width, height) = (int("width")?, int("height")?);
        if !(1..=PIN_MAX_SIZE).contains(&width) || !(1..=PIN_MAX_SIZE).contains(&height) {
            return None;
        }
        Some(Self {
            x: int("x")?,
            y: int("y")?,
            width,
            height,
            zoom: float("zoom", 1.0).clamp(ZOOM_MIN, ZOOM_MAX),
            opacity: float("opacity", 1.0).clamp(OPACITY_MIN, OPACITY_MAX),
            topmost: value
                .get("pinned_on_top")
                .and_then(Value::as_bool)
                .unwrap_or(true),
        })
    }
}

/// 构造清单记录（仓储要求含 `id` / `group_id` / `source_kind`，其余为业务字段）。
///
/// # 参数
/// - `id`：贴图 ID。
/// - `group_id`：分组 ID。
/// - `geometry`：几何与显示状态。
/// - `created_ms`：创建时间（UTC 毫秒）。
/// - `payload_bytes`：源图 PNG 体积（字节，用于容量策略）。
pub fn build_record(
    id: &str,
    group_id: &str,
    geometry: &PinGeometry,
    created_ms: i64,
    payload_bytes: u64,
) -> Map<String, Value> {
    let mut record = Map::new();
    record.insert("id".into(), json!(id));
    record.insert("group_id".into(), json!(group_id));
    record.insert("source_kind".into(), json!(SourceKind::ImageData.as_str()));
    record.insert(KEY_GEOMETRY.into(), geometry.to_json());
    record.insert(KEY_CREATED_MS.into(), json!(created_ms));
    record.insert(KEY_PAYLOAD_BYTES.into(), json!(payload_bytes));
    record
}

/// 读取记录里的几何；缺失或非法返回 `None`。
///
/// # 参数
/// - `record`：清单记录。
pub fn record_geometry(record: &Map<String, Value>) -> Option<PinGeometry> {
    PinGeometry::from_json(record.get(KEY_GEOMETRY)?)
}

/// 读取记录的创建时间（UTC 毫秒）；缺失按 0（最老，优先淘汰）。
pub fn record_created_ms(record: &Map<String, Value>) -> i64 {
    record
        .get(KEY_CREATED_MS)
        .and_then(Value::as_i64)
        .unwrap_or(0)
}

/// 读取记录的 payload 体积（字节）；缺失按 0。
pub fn record_payload_bytes(record: &Map<String, Value>) -> u64 {
    record
        .get(KEY_PAYLOAD_BYTES)
        .and_then(Value::as_u64)
        .unwrap_or(0)
}

/// 贴图历史容量策略（读自 `pinned_history/*` 配置项）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PinPolicy {
    /// 是否落盘（关闭时贴图只存在于内存，不恢复）。
    pub enabled: bool,
    /// 是否永久保留（忽略保留天数）。
    pub keep_permanently: bool,
    /// 保留天数。
    pub retention_days: u32,
    /// 最大条数。
    pub max_entries: usize,
    /// 最大磁盘占用（字节）。
    pub max_disk_bytes: u64,
}

impl PinPolicy {
    /// 从配置文档读取；取值非法时退回 schema 默认值。
    ///
    /// # 参数
    /// - `document`：配置文档。
    ///
    /// ```
    /// use snow_config::document::ConfigDocument;
    /// use snow_shot::pinned_model::PinPolicy;
    /// let policy = PinPolicy::from_document(&ConfigDocument::from_bytes(None));
    /// assert!(policy.enabled && policy.max_entries == 100 && policy.retention_days == 7);
    /// ```
    pub fn from_document(document: &ConfigDocument) -> Self {
        let uint = |key: &str, default: u64| {
            document
                .value(key)
                .as_u64()
                .filter(|n| *n > 0)
                .unwrap_or(default)
        };
        let flag = |key: &str, default: bool| document.value(key).as_bool().unwrap_or(default);
        Self {
            enabled: flag("pinned_history/enabled", true),
            keep_permanently: flag("pinned_history/keep_permanently", false),
            retention_days: uint("pinned_history/retention_days", 7).min(u64::from(u32::MAX))
                as u32,
            max_entries: uint("pinned_history/max_entries", 100).min(usize::MAX as u64) as usize,
            max_disk_bytes: uint("pinned_history/max_disk_mib", 1024).saturating_mul(BYTES_PER_MIB),
        }
    }
}

/// 淘汰判定所需的单条摘要。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PinEntryInfo {
    /// 贴图 ID。
    pub id: String,
    /// 创建时间（UTC 毫秒）。
    pub created_ms: i64,
    /// payload 体积（字节）。
    pub bytes: u64,
}

/// 按策略选出要淘汰的贴图：先按保留天数，再按条数，最后按磁盘占用，始终先淘汰最老的。
///
/// # 参数
/// - `entries`：现有条目摘要。
/// - `now_ms`：当前时间（UTC 毫秒）。
/// - `policy`：容量策略。
/// - `protect`：不可淘汰的 ID（刚创建的那张）。
///
/// # 返回
/// 要淘汰的 ID（从老到新）。
///
/// ```
/// use snow_shot::pinned_model::{PinEntryInfo, PinPolicy, select_evictions};
/// let policy = PinPolicy { enabled: true, keep_permanently: true, retention_days: 7,
///     max_entries: 1, max_disk_bytes: u64::MAX };
/// let entries = [
///     PinEntryInfo { id: "a".into(), created_ms: 1, bytes: 1 },
///     PinEntryInfo { id: "b".into(), created_ms: 2, bytes: 1 },
/// ];
/// assert_eq!(select_evictions(&entries, 10, &policy, None), vec!["a".to_string()]);
/// ```
pub fn select_evictions(
    entries: &[PinEntryInfo],
    now_ms: i64,
    policy: &PinPolicy,
    protect: Option<&str>,
) -> Vec<String> {
    let mut sorted: Vec<&PinEntryInfo> = entries.iter().collect();
    sorted.sort_by(|a, b| (a.created_ms, &a.id).cmp(&(b.created_ms, &b.id)));
    let protected = |e: &PinEntryInfo| protect == Some(e.id.as_str());
    let mut evicted: Vec<String> = Vec::new();
    let mut kept: Vec<&PinEntryInfo> = Vec::new();
    let max_age_ms = i64::from(policy.retention_days).saturating_mul(MS_PER_DAY);
    for entry in sorted {
        let expired =
            !policy.keep_permanently && now_ms.saturating_sub(entry.created_ms) > max_age_ms;
        if expired && !protected(entry) {
            evicted.push(entry.id.clone());
        } else {
            kept.push(entry);
        }
    }
    // 从最老的开始摘掉，直到满足限制（受保护的跳过）
    let mut total: u64 = kept.iter().map(|e| e.bytes).sum();
    let mut index = 0;
    while index < kept.len() && (kept.len() > policy.max_entries || total > policy.max_disk_bytes) {
        if protected(kept[index]) {
            index += 1;
            continue;
        }
        let entry = kept.remove(index);
        total = total.saturating_sub(entry.bytes);
        evicted.push(entry.id.clone());
    }
    evicted
}

/// 让恢复出的窗口落回可见区域：与任一显示器重叠不足时，移到主显示器左上并按序号层叠。
///
/// # 参数
/// - `rect`：保存的窗口外框。
/// - `monitors`：显示器物理范围（第一个视为主显示器）。
/// - `index`：本次恢复的第几张（用于层叠偏移）。
///
/// # 返回
/// 可见的外框；没有显示器信息时原样返回。
///
/// ```
/// use snow_shot::pinned_model::visible_rect;
/// use snow_ui::shell::geometry::PhysicalRect;
/// let mons = [PhysicalRect::new(0, 0, 1920, 1080)];
/// let lost = PhysicalRect::new(5000, 5000, 200, 100);
/// assert_eq!(visible_rect(lost, &mons, 0), PhysicalRect::new(0, 0, 200, 100));
/// let ok = PhysicalRect::new(100, 100, 200, 100);
/// assert_eq!(visible_rect(ok, &mons, 0), ok);
/// ```
pub fn visible_rect(rect: PhysicalRect, monitors: &[PhysicalRect], index: usize) -> PhysicalRect {
    let Some(primary) = monitors.first() else {
        return rect;
    };
    let seen = monitors.iter().any(|m| {
        m.intersect(&rect).is_some_and(|i| {
            i.width >= MIN_VISIBLE_OVERLAP.min(rect.width)
                && i.height >= MIN_VISIBLE_OVERLAP.min(rect.height)
        })
    });
    if seen {
        return rect;
    }
    let offset = RESTORE_CASCADE_STEP.saturating_mul(i32::try_from(index).unwrap_or(0));
    let width = rect.width.min(primary.width);
    let height = rect.height.min(primary.height);
    let x = (primary.x + offset).min(primary.right() - width);
    let y = (primary.y + offset).min(primary.bottom() - height);
    PhysicalRect::new(x, y, width, height)
}

/// 把滚轮的“行数”换算成步进次数（一个刻度 = 1 步）。
///
/// # 参数
/// - `lines_y`：垂直滚动行数（向上为正）。
///
/// ```
/// use snow_shot::pinned_model::wheel_steps;
/// assert_eq!(wheel_steps(3.0), 1.0);
/// assert_eq!(wheel_steps(-6.0), -2.0);
/// ```
pub fn wheel_steps(lines_y: f32) -> f32 {
    if lines_y.is_finite() {
        lines_y / WHEEL_LINES_PER_NOTCH
    } else {
        0.0
    }
}

/// 滚轮缩放 / 调透明度后的新状态。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct WheelResult {
    /// 新窗口外框。
    pub rect: PhysicalRect,
    /// 新缩放倍率。
    pub zoom: f32,
    /// 新不透明度。
    pub opacity: f32,
}

/// 应用一次滚轮：按住 Ctrl 调不透明度，否则按锚点缩放。
///
/// # 参数
/// - `rect`：当前窗口外框。
/// - `zoom` / `opacity`：当前缩放与不透明度。
/// - `image_size`：原图尺寸（缩放基准）。
/// - `steps`：步进次数（正为放大 / 更不透明）。
/// - `ctrl`：是否按住 Ctrl。
/// - `anchor`：缩放锚点。
///
/// # 返回
/// 新的外框、缩放与不透明度。
///
/// ```
/// use snow_shot::pinned_model::apply_wheel;
/// use snow_ui::shell::geometry::{PhysicalPoint, PhysicalRect};
/// use snow_ui::shell::pinned_geometry::ScaleAnchor;
/// let r = PhysicalRect::new(100, 100, 200, 100);
/// let out = apply_wheel(r, 1.0, 1.0, (200, 100), 1.0, false, ScaleAnchor::TopLeft);
/// assert_eq!((out.rect.width, out.rect.height), (220, 110));
/// ```
pub fn apply_wheel(
    rect: PhysicalRect,
    zoom: f32,
    opacity: f32,
    image_size: (i32, i32),
    steps: f32,
    ctrl: bool,
    anchor: ScaleAnchor,
) -> WheelResult {
    if ctrl {
        return WheelResult {
            rect,
            zoom,
            opacity: step_opacity(opacity, steps, OPACITY_MIN, OPACITY_MAX),
        };
    }
    let next_zoom = step_zoom(zoom, steps, ZOOM_MIN, ZOOM_MAX);
    if (next_zoom - zoom).abs() <= 1e-4 {
        return WheelResult {
            rect,
            zoom,
            opacity,
        };
    }
    let target = PhysicalPoint::new(
        ((image_size.0 as f32) * next_zoom).round() as i32,
        ((image_size.1 as f32) * next_zoom).round() as i32,
    );
    WheelResult {
        rect: anchored_scale_rect(rect, target, anchor),
        zoom: next_zoom,
        opacity,
    }
}

/// 拖动过程中的新外框：整体拖动平移，手柄拖动按原图比例等比缩放。
///
/// # 参数
/// - `handle`：正在拖动的手柄。
/// - `start_bounds`：拖动开始时的外框。
/// - `start_cursor` / `cursor`：拖动开始与当前的光标屏幕坐标。
/// - `image_size`：原图尺寸（比例基准）。
///
/// # 返回
/// 新外框；缩放手柄同时返回新的缩放倍率。
///
/// ```
/// use snow_shot::pinned_model::drag_rect;
/// use snow_ui::shell::geometry::{PhysicalPoint, PhysicalRect};
/// use snow_ui::shell::pinned_geometry::PinnedDragHandle;
/// let (rect, _) = drag_rect(PinnedDragHandle::Move, PhysicalRect::new(10, 10, 100, 50),
///     PhysicalPoint::new(0, 0), PhysicalPoint::new(5, 7), (100, 50));
/// assert_eq!(rect, PhysicalRect::new(15, 17, 100, 50));
/// ```
pub fn drag_rect(
    handle: PinnedDragHandle,
    start_bounds: PhysicalRect,
    start_cursor: PhysicalPoint,
    cursor: PhysicalPoint,
    image_size: (i32, i32),
) -> (PhysicalRect, Option<f32>) {
    let delta = PhysicalPoint::new(cursor.x - start_cursor.x, cursor.y - start_cursor.y);
    if handle == PinnedDragHandle::Move {
        let rect = PhysicalRect::new(
            start_bounds.x + delta.x,
            start_bounds.y + delta.y,
            start_bounds.width,
            start_bounds.height,
        );
        return (rect, None);
    }
    let rect = proportional_resize_rect(
        start_bounds,
        delta,
        PhysicalPoint::new(image_size.0.max(1), image_size.1.max(1)),
        handle,
        PhysicalPoint::new(PIN_MIN_SIZE, PIN_MIN_SIZE),
        PhysicalPoint::new(PIN_MAX_SIZE, PIN_MAX_SIZE),
    );
    let zoom = (rect.width as f32 / image_size.0.max(1) as f32).clamp(ZOOM_MIN, ZOOM_MAX);
    (rect, Some(zoom))
}

/// 由配置项 `pin_to_screen/mouse_wheel_zoom_mode` 得到缩放锚点。
///
/// # 参数
/// - `mode`：配置值（未知值按鼠标位置）。
/// - `cursor`：光标屏幕坐标。
///
/// ```
/// use snow_shot::pinned_model::wheel_anchor;
/// use snow_ui::shell::geometry::PhysicalPoint;
/// use snow_ui::shell::pinned_geometry::ScaleAnchor;
/// assert_eq!(wheel_anchor("center", PhysicalPoint::new(1, 2)), ScaleAnchor::Center);
/// ```
pub fn wheel_anchor(mode: &str, cursor: PhysicalPoint) -> ScaleAnchor {
    match mode {
        "top_left" => ScaleAnchor::TopLeft,
        "top_right" => ScaleAnchor::TopRight,
        "bottom_left" => ScaleAnchor::BottomLeft,
        "bottom_right" => ScaleAnchor::BottomRight,
        "center" => ScaleAnchor::Center,
        _ => ScaleAnchor::MousePoint(cursor),
    }
}

/// 双击 / 中键可触发的动作。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PinClickAction {
    /// 什么也不做。
    None,
    /// 关闭贴图。
    Close,
    /// 恢复 100% 缩放。
    ResetZoom,
}

/// 解析双击动作配置（`pin_to_screen/double_click_action`）。
///
/// 目前只实现“关闭”与“无”；`thumbnail_mode` / `hide_to_top` 尚未实现，回退为关闭
/// （返回值第二项为 `true` 表示发生了回退，调用方应记一条日志）。
///
/// # 参数
/// - `value`：配置值。
///
/// ```
/// use snow_shot::pinned_model::{PinClickAction, parse_double_click_action};
/// assert_eq!(parse_double_click_action("none"), (PinClickAction::None, false));
/// assert_eq!(parse_double_click_action("thumbnail_mode"), (PinClickAction::Close, true));
/// ```
pub fn parse_double_click_action(value: &str) -> (PinClickAction, bool) {
    match value {
        "none" => (PinClickAction::None, false),
        "close" => (PinClickAction::Close, false),
        _ => (PinClickAction::Close, true),
    }
}

/// 解析中键动作配置（`pin_to_screen/middle_mouse_button_action`）；未实现的动作按“无”处理。
///
/// # 参数
/// - `value`：配置值。
///
/// ```
/// use snow_shot::pinned_model::{PinClickAction, parse_middle_click_action};
/// assert_eq!(parse_middle_click_action("reset_zoom"), PinClickAction::ResetZoom);
/// assert_eq!(parse_middle_click_action("hide_to_top"), PinClickAction::None);
/// ```
pub fn parse_middle_click_action(value: &str) -> PinClickAction {
    match value {
        "reset_zoom" => PinClickAction::ResetZoom,
        "close" => PinClickAction::Close,
        _ => PinClickAction::None,
    }
}

/// 解析 `#RRGGBB` / `#RRGGBBAA` 颜色文本为 `0xRRGGBBAA`；格式不对返回 `None`。
///
/// # 参数
/// - `text`：颜色文本。
///
/// ```
/// use snow_shot::pinned_model::parse_hex_color;
/// assert_eq!(parse_hex_color("#DBDBDBFF"), Some(0xDBDBDBFF));
/// assert_eq!(parse_hex_color("#112233"), Some(0x112233FF));
/// assert_eq!(parse_hex_color("red"), None);
/// ```
pub fn parse_hex_color(text: &str) -> Option<u32> {
    let hex = text.trim().strip_prefix('#')?;
    if !hex.is_ascii() {
        return None;
    }
    match hex.len() {
        6 => u32::from_str_radix(hex, 16)
            .ok()
            .map(|rgb| (rgb << 8) | 0xFF),
        8 => u32::from_str_radix(hex, 16).ok(),
        _ => None,
    }
}

/// 把 RGBA 缓冲原地转成 BGRA（交换 R / B；每张图创建时只转一次，渲染阶段不再转换）。
///
/// # 参数
/// - `pixels`：4 字节一像素的缓冲。
///
/// ```
/// use snow_shot::pinned_model::swap_rb_in_place;
/// let mut px = vec![1, 2, 3, 4];
/// swap_rb_in_place(&mut px);
/// assert_eq!(px, vec![3, 2, 1, 4]);
/// ```
pub fn swap_rb_in_place(pixels: &mut [u8]) {
    for p in pixels.chunks_exact_mut(4) {
        p.swap(0, 2);
    }
}

/// 把直通（非预乘）RGBA 缓冲原地转成预乘 alpha（GPU 图像要求预乘；全不透明像素不动）。
///
/// # 参数
/// - `rgba`：4 字节一像素的缓冲。
///
/// ```
/// use snow_shot::pinned_model::premultiply_alpha_in_place;
/// let mut px = vec![200, 100, 50, 128, 1, 2, 3, 255];
/// premultiply_alpha_in_place(&mut px);
/// assert_eq!(px, vec![100, 50, 25, 128, 1, 2, 3, 255]);
/// ```
pub fn premultiply_alpha_in_place(rgba: &mut [u8]) {
    for p in rgba.chunks_exact_mut(4) {
        let a = u32::from(p[3]);
        if a == 255 {
            continue;
        }
        for c in &mut p[..3] {
            *c = ((u32::from(*c) * a + 127) / 255) as u8;
        }
    }
}

/// 剪贴板贴图的初始窗口占屏比例上限（超出则缩小显示）。
const CLIPBOARD_FIT_RATIO: f32 = 0.8;

/// 计算剪贴板贴图的初始外框：在工作区居中；图像比工作区的 80% 还大时等比缩小。
///
/// # 参数
/// - `width` / `height`：图像尺寸。
/// - `work_area`：目标显示器的工作区。
///
/// # 返回
/// `(外框, 缩放倍率)`。
///
/// ```
/// use snow_shot::pinned_model::initial_clipboard_rect;
/// use snow_ui::shell::geometry::PhysicalRect;
/// let (rect, zoom) = initial_clipboard_rect(400, 200, PhysicalRect::new(0, 0, 1000, 800));
/// assert_eq!((rect, zoom), (PhysicalRect::new(300, 300, 400, 200), 1.0));
/// ```
pub fn initial_clipboard_rect(
    width: u32,
    height: u32,
    work_area: PhysicalRect,
) -> (PhysicalRect, f32) {
    let (w, h) = (width.max(1) as f32, height.max(1) as f32);
    let fit_w = work_area.width as f32 * CLIPBOARD_FIT_RATIO / w;
    let fit_h = work_area.height as f32 * CLIPBOARD_FIT_RATIO / h;
    let zoom = fit_w.min(fit_h).clamp(f32::MIN_POSITIVE, 1.0);
    let target_w = ((w * zoom).round() as i32).max(1);
    let target_h = ((h * zoom).round() as i32).max(1);
    let rect = PhysicalRect::new(
        work_area.x + (work_area.width - target_w) / 2,
        work_area.y + (work_area.height - target_h) / 2,
        target_w,
        target_h,
    );
    (rect, zoom)
}

/// 按「自动调整窗口大小」设置算贴图初始外框：开启时等同 [`initial_clipboard_rect`]（超大图缩到工作区内），
/// 关闭时按原始尺寸居中（允许超出工作区）。
///
/// # 参数
/// - `width` / `height`：图像尺寸。
/// - `work_area`：目标显示器的工作区。
/// - `auto_resize`：`pin_to_screen/auto_resize_window` 的值。
///
/// # 返回
/// `(外框, 缩放倍率)`。
///
/// ```
/// use snow_shot::pinned_model::initial_pin_rect;
/// use snow_ui::shell::geometry::PhysicalRect;
/// let (rect, zoom) = initial_pin_rect(2000, 1000, PhysicalRect::new(0, 0, 1000, 800), false);
/// assert_eq!((rect.width, zoom), (2000, 1.0));
/// ```
pub fn initial_pin_rect(
    width: u32,
    height: u32,
    work_area: PhysicalRect,
    auto_resize: bool,
) -> (PhysicalRect, f32) {
    if auto_resize {
        return initial_clipboard_rect(width, height, work_area);
    }
    let (w, h) = (width.max(1) as i32, height.max(1) as i32);
    let rect = PhysicalRect::new(
        work_area.x + (work_area.width - w) / 2,
        work_area.y + (work_area.height - h) / 2,
        w,
        h,
    );
    (rect, 1.0)
}

/// 把带透明度的 RGBA 图像铺在白底上（结果不透明）。
///
/// 贴图管线（GPU 图像、标注合成）都假定底图不透明，剪贴板图像进入前先铺白底。
///
/// # 参数
/// - `rgba`：4 字节一像素的直通（非预乘）RGBA 缓冲，原地修改。
///
/// ```
/// use snow_shot::pinned_model::flatten_alpha_on_white;
/// let mut px = vec![0, 0, 0, 0, 10, 20, 30, 255];
/// flatten_alpha_on_white(&mut px);
/// assert_eq!(px, vec![255, 255, 255, 255, 10, 20, 30, 255]);
/// ```
pub fn flatten_alpha_on_white(rgba: &mut [u8]) {
    for p in rgba.chunks_exact_mut(4) {
        let a = u32::from(p[3]);
        if a == 255 {
            continue;
        }
        for c in &mut p[..3] {
            *c = ((u32::from(*c) * a + 255 * (255 - a) + 127) / 255) as u8;
        }
        p[3] = 255;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 构造测试用几何。
    fn geometry() -> PinGeometry {
        PinGeometry::new(PhysicalRect::new(-100, 20, 300, 200), 1.5, 0.8, false)
    }

    /// 记录写入再读出，几何、创建时间与体积原样还原。
    #[test]
    fn record_roundtrip() {
        let rec = build_record("id-1", "default", &geometry(), 1234, 999);
        assert_eq!(rec["source_kind"], "image_data");
        assert_eq!(record_geometry(&rec), Some(geometry()));
        assert_eq!(record_created_ms(&rec), 1234);
        assert_eq!(record_payload_bytes(&rec), 999);
    }

    /// 几何缺字段、尺寸为 0 或超上限时被拒绝；缩放与透明度越界会被钳制。
    #[test]
    fn geometry_validation() {
        assert!(PinGeometry::from_json(&json!({})).is_none());
        let base = json!({"x": 0, "y": 0, "width": 0, "height": 10});
        assert!(PinGeometry::from_json(&base).is_none());
        let huge = json!({"x": 0, "y": 0, "width": 99999, "height": 10});
        assert!(PinGeometry::from_json(&huge).is_none());
        let clamp = json!({"x": 0, "y": 0, "width": 10, "height": 10, "zoom": 0.0, "opacity": 7.0});
        let g = PinGeometry::from_json(&clamp).unwrap();
        assert_eq!(
            (g.zoom, g.opacity, g.topmost),
            (ZOOM_MIN, OPACITY_MAX, true)
        );
    }

    /// 缺失的元数据取安全默认值。
    #[test]
    fn record_metadata_defaults() {
        let empty = Map::new();
        assert_eq!(record_created_ms(&empty), 0);
        assert_eq!(record_payload_bytes(&empty), 0);
        assert!(record_geometry(&empty).is_none());
    }

    /// 默认配置读出 schema 默认策略。
    #[test]
    fn policy_defaults_from_config() {
        let policy = PinPolicy::from_document(&ConfigDocument::from_bytes(None));
        assert_eq!(
            policy,
            PinPolicy {
                enabled: true,
                keep_permanently: false,
                retention_days: 7,
                max_entries: 100,
                max_disk_bytes: 1024 * BYTES_PER_MIB,
            }
        );
    }

    /// 构造淘汰测试条目。
    fn entry(id: &str, created_ms: i64, bytes: u64) -> PinEntryInfo {
        PinEntryInfo {
            id: id.into(),
            created_ms,
            bytes,
        }
    }

    /// 宽松策略（永久保留、条数与体积都不限）。
    fn loose() -> PinPolicy {
        PinPolicy {
            enabled: true,
            keep_permanently: true,
            retention_days: 1,
            max_entries: usize::MAX,
            max_disk_bytes: u64::MAX,
        }
    }

    /// 超过保留天数的被淘汰；永久保留时不淘汰；受保护的不淘汰。
    #[test]
    fn eviction_by_age() {
        let day = MS_PER_DAY;
        let entries = [entry("old", 0, 1), entry("new", 10 * day, 1)];
        let mut policy = PinPolicy {
            keep_permanently: false,
            retention_days: 7,
            ..loose()
        };
        assert_eq!(
            select_evictions(&entries, 10 * day, &policy, None),
            vec!["old"]
        );
        assert!(select_evictions(&entries, 10 * day, &policy, Some("old")).is_empty());
        policy.keep_permanently = true;
        assert!(select_evictions(&entries, 10 * day, &policy, None).is_empty());
    }

    /// 超过条数上限时从最老的开始淘汰，且不动受保护的那张。
    #[test]
    fn eviction_by_count_keeps_protected() {
        let entries = [entry("a", 1, 1), entry("b", 2, 1), entry("c", 3, 1)];
        let policy = PinPolicy {
            max_entries: 2,
            ..loose()
        };
        assert_eq!(select_evictions(&entries, 10, &policy, None), vec!["a"]);
        // 最老的受保护：改淘汰次老的
        assert_eq!(
            select_evictions(&entries, 10, &policy, Some("a")),
            vec!["b"]
        );
    }

    /// 超过磁盘上限时按从老到新淘汰，直到不再超限。
    #[test]
    fn eviction_by_disk() {
        let entries = [entry("a", 1, 400), entry("b", 2, 400), entry("c", 3, 400)];
        let policy = PinPolicy {
            max_disk_bytes: 800,
            ..loose()
        };
        assert_eq!(select_evictions(&entries, 10, &policy, None), vec!["a"]);
        let tight = PinPolicy {
            max_disk_bytes: 100,
            ..loose()
        };
        // 全部超限但最新的受保护：只能淘汰另两张
        assert_eq!(
            select_evictions(&entries, 10, &tight, Some("c")),
            vec!["a", "b"]
        );
    }

    /// 没有超限时不淘汰任何条目。
    #[test]
    fn eviction_noop_when_within_limits() {
        assert!(select_evictions(&[entry("a", 1, 1)], 2, &loose(), None).is_empty());
        assert!(select_evictions(&[], 2, &loose(), None).is_empty());
    }

    /// 屏幕外的窗口被移回主显示器并层叠；可见窗口原样保留；副屏（负坐标）可见。
    #[test]
    fn visible_rect_rules() {
        let mons = [
            PhysicalRect::new(0, 0, 1920, 1080),
            PhysicalRect::new(-1280, 0, 1280, 1024),
        ];
        let seen = PhysicalRect::new(-1000, 100, 300, 200);
        assert_eq!(visible_rect(seen, &mons, 3), seen);
        let lost = PhysicalRect::new(9000, 9000, 300, 200);
        assert_eq!(
            visible_rect(lost, &mons, 0),
            PhysicalRect::new(0, 0, 300, 200)
        );
        assert_eq!(
            visible_rect(lost, &mons, 2),
            PhysicalRect::new(64, 64, 300, 200)
        );
        // 比显示器还大的窗口被收进主显示器
        let big = PhysicalRect::new(9000, 9000, 5000, 5000);
        let fit = visible_rect(big, &mons, 0);
        assert_eq!((fit.width, fit.height), (1920, 1080));
        assert_eq!(visible_rect(lost, &[], 0), lost);
    }

    /// 只露出一个边角（重叠不足 32px）的窗口视为不可见。
    #[test]
    fn visible_rect_corner_peek_is_lost() {
        let mons = [PhysicalRect::new(0, 0, 1920, 1080)];
        let peek = PhysicalRect::new(1910, 1070, 300, 200);
        assert_eq!(
            visible_rect(peek, &mons, 0),
            PhysicalRect::new(0, 0, 300, 200)
        );
    }

    /// 滚轮：一个刻度一步，非有限值按 0。
    #[test]
    fn wheel_step_conversion() {
        assert_eq!(wheel_steps(3.0), 1.0);
        assert_eq!(wheel_steps(-1.5), -0.5);
        assert_eq!(wheel_steps(f32::NAN), 0.0);
    }

    /// 滚轮缩放：按锚点缩放、上下限钳制；Ctrl+滚轮只改透明度。
    #[test]
    fn wheel_zoom_and_opacity() {
        let rect = PhysicalRect::new(100, 100, 200, 100);
        let zoomed = apply_wheel(rect, 1.0, 1.0, (200, 100), 2.0, false, ScaleAnchor::Center);
        assert!((zoomed.zoom - 1.2).abs() < 1e-4);
        assert_eq!((zoomed.rect.width, zoomed.rect.height), (240, 120));
        assert_eq!(zoomed.rect.x + zoomed.rect.width / 2, 200);
        let limit = apply_wheel(
            rect,
            ZOOM_MAX,
            1.0,
            (200, 100),
            5.0,
            false,
            ScaleAnchor::TopLeft,
        );
        assert_eq!(limit.rect, rect);
        let faded = apply_wheel(rect, 1.0, 1.0, (200, 100), -4.0, true, ScaleAnchor::TopLeft);
        assert!((faded.opacity - 0.8).abs() < 1e-4);
        assert_eq!(faded.rect, rect);
        let floor = apply_wheel(
            rect,
            1.0,
            OPACITY_MIN,
            (200, 100),
            -9.0,
            true,
            ScaleAnchor::TopLeft,
        );
        assert_eq!(floor.opacity, OPACITY_MIN);
    }

    /// 鼠标锚点缩放：光标下的内容点保持不动。
    #[test]
    fn wheel_zoom_mouse_anchor_keeps_point() {
        let rect = PhysicalRect::new(100, 100, 200, 200);
        let cursor = PhysicalPoint::new(150, 250);
        let out = apply_wheel(
            rect,
            1.0,
            1.0,
            (200, 200),
            5.0,
            false,
            ScaleAnchor::MousePoint(cursor),
        );
        let rx = (cursor.x - rect.x) as f32 / rect.width as f32;
        let nx = (cursor.x - out.rect.x) as f32 / out.rect.width as f32;
        assert!((rx - nx).abs() < 0.01);
    }

    /// 整体拖动只平移；手柄拖动等比缩放并返回新倍率；缩到下限被钳制。
    #[test]
    fn drag_rect_move_and_resize() {
        let start = PhysicalRect::new(100, 100, 200, 100);
        let (moved, zoom) = drag_rect(
            PinnedDragHandle::Move,
            start,
            PhysicalPoint::new(0, 0),
            PhysicalPoint::new(-30, 40),
            (200, 100),
        );
        assert_eq!(moved, PhysicalRect::new(70, 140, 200, 100));
        assert!(zoom.is_none());
        let (grown, zoom) = drag_rect(
            PinnedDragHandle::BottomRight,
            start,
            PhysicalPoint::new(0, 0),
            PhysicalPoint::new(100, 0),
            (200, 100),
        );
        assert_eq!(
            (grown.x, grown.y, grown.width, grown.height),
            (100, 100, 300, 150)
        );
        assert!((zoom.unwrap() - 1.5).abs() < 1e-4);
        let (tiny, _) = drag_rect(
            PinnedDragHandle::Right,
            start,
            PhysicalPoint::new(0, 0),
            PhysicalPoint::new(-5000, 0),
            (200, 100),
        );
        assert_eq!(tiny.width, PIN_MIN_SIZE);
    }

    /// 八个方向手柄缩放时，对侧边 / 角保持不动。
    #[test]
    fn drag_rect_anchors_opposite_side() {
        let start = PhysicalRect::new(100, 100, 200, 100);
        let cases = [
            (PinnedDragHandle::TopLeft, (start.right(), start.bottom())),
            (PinnedDragHandle::BottomLeft, (start.right(), start.y)),
            (PinnedDragHandle::TopRight, (start.x, start.bottom())),
            (PinnedDragHandle::BottomRight, (start.x, start.y)),
        ];
        for (handle, fixed) in cases {
            let sign = |dx: i32| PhysicalPoint::new(dx, 0);
            let delta = match handle {
                PinnedDragHandle::TopLeft | PinnedDragHandle::BottomLeft => sign(-40),
                _ => sign(40),
            };
            let (rect, _) = drag_rect(handle, start, PhysicalPoint::new(0, 0), delta, (200, 100));
            assert_eq!(rect.width, 240, "{handle:?}");
            let corner = match handle {
                PinnedDragHandle::TopLeft => (rect.right(), rect.bottom()),
                PinnedDragHandle::BottomLeft => (rect.right(), rect.y),
                PinnedDragHandle::TopRight => (rect.x, rect.bottom()),
                _ => (rect.x, rect.y),
            };
            assert_eq!(corner, fixed, "{handle:?}");
        }
    }

    /// 缩放锚点配置映射，未知值按鼠标位置。
    #[test]
    fn anchor_modes() {
        let c = PhysicalPoint::new(7, 8);
        assert_eq!(wheel_anchor("top_left", c), ScaleAnchor::TopLeft);
        assert_eq!(wheel_anchor("bottom_right", c), ScaleAnchor::BottomRight);
        assert_eq!(
            wheel_anchor("mouse_position", c),
            ScaleAnchor::MousePoint(c)
        );
        assert_eq!(wheel_anchor("???", c), ScaleAnchor::MousePoint(c));
    }

    /// 双击 / 中键动作解析，未实现的双击动作回退为关闭并标记。
    #[test]
    fn click_action_parsing() {
        assert_eq!(
            parse_double_click_action("close"),
            (PinClickAction::Close, false)
        );
        assert_eq!(
            parse_double_click_action("hide_to_top"),
            (PinClickAction::Close, true)
        );
        assert_eq!(
            parse_double_click_action("none"),
            (PinClickAction::None, false)
        );
        assert_eq!(
            parse_middle_click_action("reset_zoom"),
            PinClickAction::ResetZoom
        );
        assert_eq!(parse_middle_click_action("close"), PinClickAction::Close);
        assert_eq!(
            parse_middle_click_action("thumbnail_mode"),
            PinClickAction::None
        );
    }

    /// 颜色文本解析：6 位补不透明、8 位原样、非法格式（含多字节字符）返回 None。
    #[test]
    fn hex_colors() {
        assert_eq!(parse_hex_color("#69B1FFFF"), Some(0x69B1FFFF));
        assert_eq!(parse_hex_color(" #000000 "), Some(0x000000FF));
        assert_eq!(parse_hex_color("#12345"), None);
        assert_eq!(parse_hex_color("123456"), None);
        assert_eq!(parse_hex_color("#GGGGGG"), None);
        assert_eq!(parse_hex_color("#中中中中"), None);
    }

    /// 剪贴板贴图：小图居中原尺寸；大图按 80% 占屏等比缩小并居中。
    #[test]
    fn clipboard_initial_rect() {
        let area = PhysicalRect::new(2560, 0, 1000, 800);
        let (small, z) = initial_clipboard_rect(100, 50, area);
        assert_eq!((small, z), (PhysicalRect::new(3010, 375, 100, 50), 1.0));
        let (big, z) = initial_clipboard_rect(2000, 1000, area);
        assert!((z - 0.4).abs() < 1e-4);
        assert_eq!((big.width, big.height), (800, 400));
        assert_eq!((big.x, big.y), (2560 + 100, 200));
        // 极端尺寸不会得到 0 宽高
        let (thin, _) = initial_clipboard_rect(1, 20000, area);
        assert!(thin.width >= 1 && thin.height >= 1);
    }

    /// 关闭自动调整窗口大小：大图也按原尺寸居中；开启时仍缩进工作区。
    #[test]
    fn pin_rect_follows_auto_resize() {
        let area = PhysicalRect::new(0, 0, 1000, 800);
        let (full, z) = initial_pin_rect(2000, 1000, area, false);
        assert_eq!((full, z), (PhysicalRect::new(-500, -100, 2000, 1000), 1.0));
        let (fit, z) = initial_pin_rect(2000, 1000, area, true);
        assert!(z < 1.0 && fit.width < 2000);
        assert_eq!(
            initial_pin_rect(100, 50, area, true),
            initial_pin_rect(100, 50, area, false)
        );
    }

    /// 铺白底：全透明变白，半透明按比例混合，不透明不变。
    #[test]
    fn flatten_alpha() {
        let mut px = vec![0, 0, 0, 0, 0, 0, 0, 128, 10, 20, 30, 255];
        flatten_alpha_on_white(&mut px);
        assert_eq!(&px[0..4], &[255, 255, 255, 255]);
        assert_eq!(&px[4..8], &[127, 127, 127, 255]);
        assert_eq!(&px[8..12], &[10, 20, 30, 255]);
    }

    /// 预乘 alpha：透明像素颜色归零，半透明按比例缩，不透明不动。
    #[test]
    fn premultiply_alpha() {
        let mut px = vec![255, 255, 255, 0, 200, 100, 50, 128, 9, 9, 9, 255];
        premultiply_alpha_in_place(&mut px);
        assert_eq!(px, vec![0, 0, 0, 0, 100, 50, 25, 128, 9, 9, 9, 255]);
    }

    /// R/B 交换：整像素交换，尾部不足 4 字节的残余不动。
    #[test]
    fn swap_rb() {
        let mut px = vec![1, 2, 3, 4, 5, 6, 7, 8, 9, 9];
        swap_rb_in_place(&mut px);
        assert_eq!(px, vec![3, 2, 1, 4, 7, 6, 5, 8, 9, 9]);
    }
}
