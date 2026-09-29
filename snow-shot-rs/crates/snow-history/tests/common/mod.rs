//! 集成测试共用工具：临时目录、草稿构造、目录遍历。
#![allow(dead_code)]

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use serde_json::{Value, json};
use snow_history::capture_history::{DraftDisplay, DraftImage, HistoryDraft};
use snow_history::index::{Rect, Selection};

/// 自动清理的临时目录。
pub struct TempDir(pub PathBuf);

/// 全局计数器，保证目录与 UUID 唯一。
static COUNTER: AtomicU64 = AtomicU64::new(1);

impl TempDir {
    /// 创建独立临时目录。
    pub fn new(tag: &str) -> Self {
        let dir = std::env::temp_dir().join(format!(
            "snow-history-{}-{tag}-{}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        Self(dir)
    }

    /// 目录路径。
    pub fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempDir {
    /// 递归删除目录，忽略失败。
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

/// 生成唯一的小写 UUID 文本。
pub fn new_uuid() -> String {
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    format!("{:08x}-0000-4000-8000-{:012x}", std::process::id(), n)
}

/// 构造矩形。
pub fn rect(width: i64, height: i64) -> Rect {
    serde_json::from_value(json!({"x":0,"y":0,"width":width,"height":height})).unwrap()
}

/// 构造合法选区。
pub fn selection(width: i64, height: i64) -> Selection {
    serde_json::from_value(json!({
        "rectangle": {"x":0,"y":0,"width":width,"height":height},
        "corner_radius": 4, "shadow_width": 2, "shadow_color": "#60000000",
        "lock_aspect_ratio": false, "lock_drag_aspect_ratio": false
    }))
    .unwrap()
}

/// 构造一份单显示器草稿；PNG 内容为占位字节。
pub fn draft_at(created_ms: i64) -> HistoryDraft {
    HistoryDraft {
        id: new_uuid(),
        created_utc: snow_history::timeutil::format_iso_utc_ms(created_ms),
        source: "copied_to_clipboard".into(),
        canvas_bounds: rect(32, 24),
        selection: selection(32, 24),
        canvas_history: br#"{"schemaVersion":1,"document":{},"history":{}}"#.to_vec(),
        displays: vec![DraftDisplay {
            image: DraftImage {
                width: 32,
                height: 24,
                png: vec![7; 100],
            },
            stable_id: "display-id".into(),
            display_name: "Display".into(),
            source_canvas_origin: None,
            source_canvas_rect: None,
            backing_scale: None,
            native_display_id: None,
            canvas_space: None,
        }],
        result: None,
        content_image: false,
        scrolling: None,
        desktop_geometry: None,
    }
}

/// `capture_history/records` 目录。
pub fn records_dir(root: &Path) -> PathBuf {
    root.join("capture_history").join("records")
}

/// `capture_history/index.json` 路径。
pub fn index_path(root: &Path) -> PathBuf {
    root.join("capture_history").join("index.json")
}

/// 读取并解析 `index.json`。
pub fn read_index(root: &Path) -> Value {
    serde_json::from_slice(&fs::read(index_path(root)).unwrap()).unwrap()
}

/// 覆盖写 `index.json`。
pub fn write_index(root: &Path, value: &Value) {
    fs::write(index_path(root), serde_json::to_vec(value).unwrap()).unwrap();
}

/// 列出 `records` 下的子目录名（排序）。
pub fn record_dir_names(root: &Path) -> Vec<String> {
    let mut names: Vec<String> = fs::read_dir(records_dir(root))
        .map(|it| {
            it.map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
                .collect()
        })
        .unwrap_or_default();
    names.sort();
    names
}
