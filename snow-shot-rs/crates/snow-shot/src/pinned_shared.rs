//! 贴图共享上下文：持久化仓储、配置读取与 ID 分配，由管理器与各贴图视图共同持有。
//!
//! 所有调用都发生在 GPUI 主线程，因此用 `Rc<RefCell<..>>` 共享，不需要锁。

use crate::pinned_model::{
    PinClickAction, PinEntryInfo, PinGeometry, PinPolicy, build_record, parse_double_click_action,
    parse_hex_color, parse_middle_click_action, record_created_ms, record_geometry,
    record_payload_bytes, select_evictions,
};
use crate::settings_state::SharedConfig;
use image::ImageFormat;
use snow_history::pin_id::new_unique_pin_id;
use snow_history::pinned::{PinImage, PinOptions, PinPayload, PinnedStore};
use snow_history::timeutil::now_utc_ms;
use std::cell::RefCell;
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::rc::Rc;

/// 源图在贴图仓储里的固定文件名（`ImageData` 类型的约定）。
const SOURCE_IMAGE_FILE: &str = "source.png";
/// 边框默认色（配置缺失或非法时）。
const DEFAULT_BORDER: u32 = 0xDBDBDBFF;
/// 激活态边框默认色。
const DEFAULT_BORDER_ACTIVE: u32 = 0x69B1FFFF;
/// 默认滚轮缩放锚点模式。
const DEFAULT_WHEEL_MODE: &str = "mouse_position";
/// 双击动作配置键。
const KEY_DOUBLE_CLICK: &str = "pin_to_screen/double_click_action";
/// 中键动作配置键。
const KEY_MIDDLE_CLICK: &str = "pin_to_screen/middle_mouse_button_action";
/// 滚轮缩放模式配置键。
const KEY_WHEEL_MODE: &str = "pin_to_screen/mouse_wheel_zoom_mode";
/// 边框色配置键。
const KEY_BORDER: &str = "pin_to_screen/border_color";
/// 激活边框色配置键。
const KEY_BORDER_ACTIVE: &str = "pin_to_screen/border_active_color";

/// 贴图窗口的交互配置快照（创建视图时读取一次）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PinInteraction {
    /// 双击动作。
    pub double_click: PinClickAction,
    /// 双击配置的动作未实现而回退为关闭。
    pub double_click_fallback: bool,
    /// 中键动作。
    pub middle_click: PinClickAction,
    /// 滚轮缩放锚点模式（`pin_to_screen/mouse_wheel_zoom_mode` 的取值）。
    pub wheel_mode: String,
    /// 普通边框色（0xRRGGBBAA）。
    pub border: u32,
    /// 激活（悬停 / 拖动）边框色（0xRRGGBBAA）。
    pub border_active: u32,
}

/// 从仓储恢复出的一张贴图。
#[derive(Debug, Clone, PartialEq)]
pub struct RestoredPin {
    /// 贴图 ID。
    pub id: String,
    /// 保存的窗口几何；记录里缺失或损坏为 `None`。
    pub geometry: Option<PinGeometry>,
    /// 创建时间（UTC 毫秒）。
    pub created_ms: i64,
    /// 源图 PNG 体积（字节）。
    pub payload_bytes: u64,
    /// 图像宽。
    pub width: u32,
    /// 图像高。
    pub height: u32,
    /// 不透明 RGBA 像素（原图，不含二次标注）。
    pub rgba: Vec<u8>,
    /// 二次标注的引擎会话字节（元素 + 撤销历史）；没有标注为空。
    pub canvas_session: Vec<u8>,
}

/// 贴图共享上下文。
pub struct PinShared {
    /// 持久化仓储。
    store: RefCell<PinnedStore>,
    /// 共享配置。
    config: SharedConfig,
    /// 本进程已发出的 ID（含未落盘的），避免同会话内重复。
    issued: RefCell<BTreeSet<String>>,
    /// 贴图窗口关闭后的通知（管理器据此回收窗口句柄）。
    on_closed: Box<dyn Fn(&str)>,
}

impl PinShared {
    /// 打开数据根下的贴图仓储并构造共享上下文。
    ///
    /// # 参数
    /// - `data_root`：数据根目录（仓储位于其 `pinned_windows_v2/` 下）。
    /// - `config`：共享配置。
    /// - `on_closed`：某张贴图窗口关闭后的通知回调（参数为贴图 ID）。
    ///
    /// ```ignore
    /// let shared = PinShared::open(&data_root, config, Box::new(|id| println!("{id} closed")));
    /// ```
    pub fn open(data_root: &Path, config: SharedConfig, on_closed: Box<dyn Fn(&str)>) -> Rc<Self> {
        let store = PinnedStore::open(data_root, PinOptions::default());
        if !store.last_error().is_empty() {
            tracing::error!(error = %store.last_error(), "贴图仓储打开异常");
        }
        Rc::new(Self {
            store: RefCell::new(store),
            config,
            issued: RefCell::new(BTreeSet::new()),
            on_closed,
        })
    }

    /// 当前容量策略。
    pub fn policy(&self) -> PinPolicy {
        PinPolicy::from_document(self.config.borrow().document())
    }

    /// 读取交互配置快照。
    pub fn interaction(&self) -> PinInteraction {
        let store = self.config.borrow();
        let doc = store.document();
        let text = |key: &str, default: &str| {
            doc.value(key)
                .as_str()
                .map_or_else(|| default.to_string(), str::to_string)
        };
        let color = |key: &str, default: u32| {
            doc.value(key)
                .as_str()
                .and_then(parse_hex_color)
                .unwrap_or(default)
        };
        let (double_click, double_click_fallback) =
            parse_double_click_action(&text(KEY_DOUBLE_CLICK, "close"));
        PinInteraction {
            double_click,
            double_click_fallback,
            middle_click: parse_middle_click_action(&text(KEY_MIDDLE_CLICK, "none")),
            wheel_mode: text(KEY_WHEEL_MODE, DEFAULT_WHEEL_MODE),
            border: color(KEY_BORDER, DEFAULT_BORDER),
            border_active: color(KEY_BORDER_ACTIVE, DEFAULT_BORDER_ACTIVE),
        }
    }

    /// 按截图保存配置（目录 / 文件名模板 / 格式）快速保存一张图。
    ///
    /// # 参数
    /// - `width` / `height` / `rgba`：图像尺寸与像素。
    ///
    /// # 返回
    /// 写入的路径；失败为按界面语言生成的提示。
    pub fn quick_save(&self, width: u32, height: u32, rgba: &[u8]) -> Result<PathBuf, String> {
        let store = self.config.borrow();
        let locale = crate::app_runtime::interface_locale(store.document());
        crate::screenshot_output::quick_save(store.document(), width, height, rgba)
            .map_err(|e| e.manual_message(&locale))
    }

    /// 分配一个新的贴图 ID：随机 UUID，且不与仓储已有记录、本会话已发出的 ID 冲突。
    ///
    /// # 返回
    /// 新 ID；极端情况下多次碰撞返回错误。
    pub fn new_id(&self) -> Result<String, String> {
        let id = {
            let store = self.store.borrow();
            let issued = self.issued.borrow();
            new_unique_pin_id(|id| store.record(id).is_some() || issued.contains(id))?
        };
        self.issued.borrow_mut().insert(id.clone());
        Ok(id)
    }

    /// 登记一个来自仓储的既有 ID（恢复时调用，使其不会再被分配）。
    pub fn adopt_id(&self, id: &str) {
        self.issued.borrow_mut().insert(id.to_string());
    }

    /// 写入（新增或更新）一张贴图并立即落盘（崩溃安全）；持久化被配置关闭时什么也不做。
    ///
    /// # 参数
    /// - `id`：贴图 ID。
    /// - `geometry`：窗口几何与显示状态。
    /// - `created_ms`：创建时间。
    /// - `payload_bytes`：源图 PNG 体积。
    /// - `png`：源图 PNG 字节；`None` 表示只更新清单（几何变化，不重写图片）。
    ///
    /// # 返回
    /// 成功返回 `Ok(())`；仓储拒绝或落盘失败返回错误说明。
    pub fn persist(
        &self,
        id: &str,
        geometry: &PinGeometry,
        created_ms: i64,
        payload_bytes: u64,
        png: Option<Vec<u8>>,
    ) -> Result<(), String> {
        if !self.policy().enabled {
            return Ok(());
        }
        let group = self.store.borrow().active_group_id();
        let record = build_record(id, &group, geometry, created_ms, payload_bytes);
        let payload = png.map(|bytes| PinPayload {
            image: Some(PinImage {
                file_name: SOURCE_IMAGE_FILE.to_string(),
                bytes,
            }),
            ..PinPayload::default()
        });
        let mut store = self.store.borrow_mut();
        store.upsert(record, payload).map_err(|e| e.to_string())?;
        store.flush().map_err(|e| e.to_string())
    }

    /// 只更新一张已存贴图的二次标注会话（原图保持不变）并落盘。
    ///
    /// # 参数
    /// - `id`：贴图 ID（必须已存在）。
    /// - `geometry`：窗口几何与显示状态。
    /// - `created_ms`：创建时间。
    /// - `session`：引擎会话字节；空表示没有标注。
    ///
    /// # 返回
    /// 更新后的 payload 总体积（原图 + 会话，字节）；持久化被关闭时返回 0。
    pub fn persist_session(
        &self,
        id: &str,
        geometry: &PinGeometry,
        created_ms: i64,
        session: Vec<u8>,
    ) -> Result<u64, String> {
        if !self.policy().enabled {
            return Ok(0);
        }
        let mut store = self.store.borrow_mut();
        let mut payload = store
            .load_payload(id)
            .map_err(|e| e.to_string())?
            .ok_or_else(|| "贴图内容不存在".to_string())?;
        let total =
            payload.image.as_ref().map_or(0, |i| i.bytes.len() as u64) + session.len() as u64;
        payload.canvas_session = session;
        let record = build_record(id, &store.active_group_id(), geometry, created_ms, total);
        store
            .upsert(record, Some(payload))
            .map_err(|e| e.to_string())?;
        store.flush().map_err(|e| e.to_string())?;
        Ok(total)
    }

    /// 从仓储移除一张贴图并落盘；不存在返回 `false`。
    pub fn remove(&self, id: &str) -> bool {
        let mut store = self.store.borrow_mut();
        let removed = store.remove(id);
        if let Err(e) = store.flush() {
            tracing::error!(id, error = %e, "移除贴图后落盘失败");
        }
        removed
    }

    /// 仓储里的全部贴图 ID（升序）。
    pub fn record_ids(&self) -> Vec<String> {
        self.store.borrow().record_ids()
    }

    /// 仓储里各贴图的摘要（淘汰判定用）。
    pub fn stored_entries(&self) -> Vec<PinEntryInfo> {
        let store = self.store.borrow();
        store
            .record_ids()
            .into_iter()
            .filter_map(|id| {
                let record = store.record(&id)?;
                Some(PinEntryInfo {
                    created_ms: record_created_ms(&record),
                    bytes: record_payload_bytes(&record),
                    id,
                })
            })
            .collect()
    }

    /// 按容量策略淘汰贴图：从仓储移除并落盘。
    ///
    /// # 参数
    /// - `protect`：不可淘汰的 ID（刚创建的贴图）。
    ///
    /// # 返回
    /// 被淘汰的 ID（调用方应同步关闭对应窗口）。
    pub fn evict(&self, protect: Option<&str>) -> Vec<String> {
        let policy = self.policy();
        if !policy.enabled {
            return Vec::new();
        }
        let victims = select_evictions(&self.stored_entries(), now_utc_ms(), &policy, protect);
        if victims.is_empty() {
            return victims;
        }
        let mut store = self.store.borrow_mut();
        for id in &victims {
            store.remove(id);
        }
        if let Err(e) = store.flush() {
            tracing::error!(error = %e, "淘汰贴图后落盘失败");
        }
        victims
    }

    /// 清扫崩溃遗留的孤儿目录；出错只记日志。
    pub fn sweep_orphans(&self) {
        match self.store.borrow_mut().sweep_orphans() {
            Ok(0) => {}
            Ok(n) => tracing::info!(count = n, "已清扫贴图孤儿目录"),
            Err(e) => tracing::warn!(error = %e, "清扫贴图孤儿目录失败"),
        }
    }

    /// 读取并解码一张已存贴图。
    ///
    /// # 参数
    /// - `id`：贴图 ID。
    ///
    /// # 返回
    /// 解码后的贴图；记录不存在、源图缺失或解码失败返回错误说明。
    pub fn load(&self, id: &str) -> Result<RestoredPin, String> {
        let store = self.store.borrow();
        let record = store
            .record(id)
            .ok_or_else(|| "贴图记录不存在".to_string())?;
        let payload = store
            .load_payload(id)
            .map_err(|e| e.to_string())?
            .ok_or_else(|| "贴图内容不存在".to_string())?;
        let image = payload.image.ok_or_else(|| "贴图缺少源图".to_string())?;
        let decoded = image::load_from_memory_with_format(&image.bytes, ImageFormat::Png)
            .map_err(|e| format!("源图解码失败: {e}"))?
            .to_rgba8();
        let (width, height) = decoded.dimensions();
        Ok(RestoredPin {
            id: id.to_string(),
            geometry: record_geometry(&record),
            created_ms: record_created_ms(&record),
            payload_bytes: image.bytes.len() as u64 + payload.canvas_session.len() as u64,
            width,
            height,
            rgba: decoded.into_raw(),
            canvas_session: payload.canvas_session,
        })
    }

    /// 通知管理器某张贴图窗口已关闭，并释放其 ID 占用。
    pub fn notify_closed(&self, id: &str) {
        self.issued.borrow_mut().remove(id);
        (self.on_closed)(id);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::screenshot_output::encode_png;
    use snow_config::store::ConfigStore;
    use snow_ui::shell::geometry::PhysicalRect;
    use std::cell::Cell;

    /// 在唯一临时目录里打开共享上下文（配置用默认值）。
    fn open_in(dir: &Path) -> (Rc<PinShared>, Rc<Cell<u32>>) {
        let config: SharedConfig = Rc::new(RefCell::new(ConfigStore::open(dir.join("cfg.json"))));
        let closed = Rc::new(Cell::new(0));
        let counter = Rc::clone(&closed);
        let shared = PinShared::open(
            dir,
            config,
            Box::new(move |_| counter.set(counter.get() + 1)),
        );
        (shared, closed)
    }

    /// 创建唯一的临时目录。
    fn temp_dir(tag: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("snow-pin-shared-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// 生成 `w x h` 的渐变 RGBA（像素随坐标变化，便于对比）。
    fn gradient(w: u32, h: u32) -> Vec<u8> {
        let mut out = Vec::new();
        for y in 0..h {
            for x in 0..w {
                out.extend_from_slice(&[(x * 7) as u8, (y * 13) as u8, (x + y) as u8, 255]);
            }
        }
        out
    }

    /// 存一张贴图并返回 ID。
    fn store_pin(shared: &PinShared, w: u32, h: u32, rect: PhysicalRect) -> String {
        let id = shared.new_id().unwrap();
        let png = encode_png(w, h, &gradient(w, h)).unwrap();
        let geometry = PinGeometry::new(rect, 1.0, 1.0, true);
        shared
            .persist(&id, &geometry, now_utc_ms(), png.len() as u64, Some(png))
            .unwrap();
        id
    }

    /// 新分配的 ID 是合法 UUID、彼此不同，且不会与已持久化的 ID 重复（重启后不覆盖旧贴图）。
    #[test]
    fn ids_are_unique_across_restart() {
        let dir = temp_dir("ids");
        let (first, _) = open_in(&dir);
        let mut saved = Vec::new();
        for _ in 0..5 {
            saved.push(store_pin(&first, 4, 4, PhysicalRect::new(0, 0, 4, 4)));
        }
        drop(first);
        // “重启”：新的共享上下文读回同一目录，先前的 ID 全在，再分配的 ID 不与之重复
        let (second, _) = open_in(&dir);
        assert_eq!(second.record_ids().len(), 5);
        for _ in 0..50 {
            let id = second.new_id().unwrap();
            assert!(snow_history::index::is_valid_uuid(&id));
            assert!(!saved.contains(&id));
        }
        // 新增一张不影响旧的
        store_pin(&second, 4, 4, PhysicalRect::new(0, 0, 4, 4));
        assert_eq!(second.record_ids().len(), 6);
        for id in &saved {
            assert!(second.record_ids().contains(id));
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 存盘后重新打开：像素、几何、创建时间完全还原。
    #[test]
    fn restore_roundtrip() {
        let dir = temp_dir("restore");
        let (first, _) = open_in(&dir);
        let id = store_pin(&first, 9, 5, PhysicalRect::new(-300, 40, 90, 50));
        drop(first);
        let (second, _) = open_in(&dir);
        let restored = second.load(&id).unwrap();
        assert_eq!((restored.width, restored.height), (9, 5));
        assert_eq!(restored.rgba, gradient(9, 5));
        let g = restored.geometry.unwrap();
        assert_eq!(g.rect(), PhysicalRect::new(-300, 40, 90, 50));
        assert!(restored.created_ms > 0 && restored.payload_bytes > 0);
        assert!(second.load("00000000-0000-4000-8000-000000000001").is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 二次标注会话：写入后原图不变、会话可读回；更新会话覆盖旧会话；清空会话后读回为空。
    #[test]
    fn session_persist_keeps_original_image() {
        let dir = temp_dir("session");
        let (shared, _) = open_in(&dir);
        let id = store_pin(&shared, 5, 5, PhysicalRect::new(0, 0, 5, 5));
        let g = PinGeometry::new(PhysicalRect::new(0, 0, 5, 5), 1.0, 1.0, true);
        let total = shared
            .persist_session(&id, &g, 1, b"session-v1".to_vec())
            .unwrap();
        assert!(total > 10);
        drop(shared);
        let (again, _) = open_in(&dir);
        let restored = again.load(&id).unwrap();
        assert_eq!(restored.canvas_session, b"session-v1");
        assert_eq!(restored.rgba, gradient(5, 5));
        again.persist_session(&id, &g, 1, b"v2".to_vec()).unwrap();
        assert_eq!(again.load(&id).unwrap().canvas_session, b"v2");
        again.persist_session(&id, &g, 1, Vec::new()).unwrap();
        let cleared = again.load(&id).unwrap();
        assert!(cleared.canvas_session.is_empty());
        assert_eq!(cleared.rgba, gradient(5, 5));
        // 不存在的贴图不能写会话
        assert!(
            again
                .persist_session("00000000-0000-4000-8000-000000000009", &g, 1, b"x".to_vec())
                .is_err()
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 只更新几何（不带图片）不会重写 payload，且新几何被保存。
    #[test]
    fn geometry_update_keeps_image() {
        let dir = temp_dir("geom");
        let (shared, _) = open_in(&dir);
        let id = store_pin(&shared, 6, 6, PhysicalRect::new(0, 0, 6, 6));
        let moved = PinGeometry::new(PhysicalRect::new(50, 60, 12, 12), 2.0, 0.5, false);
        shared.persist(&id, &moved, 1, 1, None).unwrap();
        drop(shared);
        let (again, _) = open_in(&dir);
        let restored = again.load(&id).unwrap();
        assert_eq!(restored.rgba, gradient(6, 6));
        let g = restored.geometry.unwrap();
        assert_eq!(
            (g.rect(), g.zoom, g.opacity, g.topmost),
            (moved.rect(), 2.0, 0.5, false)
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 关闭窗口即从存储移除：记录与磁盘目录都消失，重启后不再恢复。
    #[test]
    fn remove_deletes_from_disk() {
        let dir = temp_dir("remove");
        let (shared, _) = open_in(&dir);
        let keep = store_pin(&shared, 3, 3, PhysicalRect::new(0, 0, 3, 3));
        let gone = store_pin(&shared, 3, 3, PhysicalRect::new(0, 0, 3, 3));
        let pins = dir.join("pinned_windows_v2").join("pins");
        assert!(pins.join(&gone).is_dir());
        assert!(shared.remove(&gone));
        assert!(!shared.remove(&gone));
        assert!(!pins.join(&gone).exists());
        assert!(pins.join(&keep).is_dir());
        drop(shared);
        let (again, _) = open_in(&dir);
        assert_eq!(again.record_ids(), vec![keep]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 容量策略生效：条数超过上限时淘汰最老的，刚创建的受保护，磁盘目录同步清理。
    #[test]
    fn eviction_enforces_max_entries() {
        let dir = temp_dir("evict");
        let (shared, _) = open_in(&dir);
        shared
            .config
            .borrow_mut()
            .set_value("pinned_history/max_entries", serde_json::json!(2))
            .unwrap();
        let mut ids = Vec::new();
        for i in 0..4 {
            let id = shared.new_id().unwrap();
            let png = encode_png(2, 2, &gradient(2, 2)).unwrap();
            let geometry = PinGeometry::new(PhysicalRect::new(0, 0, 2, 2), 1.0, 1.0, true);
            // 用递增的（近期）创建时间保证“老 -> 新”顺序确定，且不触发按天数的淘汰
            let created = now_utc_ms() - (4 - i64::from(i)) * 1000;
            shared
                .persist(&id, &geometry, created, png.len() as u64, Some(png))
                .unwrap();
            ids.push(id);
        }
        let victims = shared.evict(Some(&ids[3]));
        assert_eq!(victims, vec![ids[0].clone(), ids[1].clone()]);
        assert_eq!(shared.record_ids().len(), 2);
        let pins = dir.join("pinned_windows_v2").join("pins");
        assert!(!pins.join(&ids[0]).exists());
        assert!(pins.join(&ids[3]).is_dir());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 关闭持久化（`pinned_history/enabled=false`）后不落盘、不淘汰。
    #[test]
    fn disabled_policy_skips_persistence() {
        let dir = temp_dir("disabled");
        let (shared, _) = open_in(&dir);
        shared
            .config
            .borrow_mut()
            .set_value("pinned_history/enabled", serde_json::json!(false))
            .unwrap();
        let id = shared.new_id().unwrap();
        let png = encode_png(2, 2, &gradient(2, 2)).unwrap();
        let geometry = PinGeometry::new(PhysicalRect::new(0, 0, 2, 2), 1.0, 1.0, true);
        shared.persist(&id, &geometry, 1, 1, Some(png)).unwrap();
        assert!(shared.record_ids().is_empty());
        assert!(shared.evict(None).is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 窗口关闭通知会触发回调，并释放该 ID 的占用。
    #[test]
    fn close_notification_fires() {
        let dir = temp_dir("closed");
        let (shared, closed) = open_in(&dir);
        let id = shared.new_id().unwrap();
        shared.notify_closed(&id);
        assert_eq!(closed.get(), 1);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 默认配置下的交互参数：双击回退为关闭（默认值 thumbnail_mode 未实现）、中键还原缩放。
    #[test]
    fn interaction_defaults() {
        let dir = temp_dir("interaction");
        let (shared, _) = open_in(&dir);
        let i = shared.interaction();
        assert_eq!(
            (i.double_click, i.double_click_fallback),
            (PinClickAction::Close, true)
        );
        assert_eq!(i.middle_click, PinClickAction::ResetZoom);
        assert_eq!(i.wheel_mode, "mouse_position");
        assert_eq!(
            (i.border, i.border_active),
            (DEFAULT_BORDER, DEFAULT_BORDER_ACTIVE)
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
