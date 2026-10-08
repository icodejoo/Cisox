//! 贴图共享上下文：持久化仓储、配置读取与 ID 分配，由管理器与各贴图视图共同持有。
//!
//! 所有调用都发生在 GPUI 主线程，因此用 `Rc<RefCell<..>>` 共享，不需要锁。

use crate::pinned_model::{
    PinClickAction, PinEntryInfo, PinGeometry, PinPolicy, build_record, parse_double_click_action,
    parse_hex_color, parse_middle_click_action, record_created_ms, record_geometry,
    record_payload_bytes, select_evictions,
};
use crate::pinned_keymap::PinKeymap;
use crate::settings_state::SharedConfig;
use image::ImageFormat;
use snow_history::pin_id::new_unique_pin_id;
use snow_history::pinned::{
    DEFAULT_GROUP_ID, MAX_GROUP_NAME_UNITS, MAX_GROUPS, PinGroup, PinImage, PinOptions, PinPayload, PinnedStore,
};
use snow_history::timeutil::now_utc_ms;
use snow_ui::shell::geometry::PhysicalRect;
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
    /// 贴图窗口键位表（`pin_to_screen_shortcuts/*`）。
    pub keymap: PinKeymap,
    /// 界面语言代码（右键菜单与状态提示的文案语言）。
    pub locale: &'static str,
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

/// 贴图窗口发给管理器的控制事件（管理器负责开关点击穿透时的退出按钮小窗）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PinControlEvent {
    /// 贴图请求文字识别（像素为合成后的完整图像，识别在后台线程执行）。
    OcrRequested {
        /// 贴图 ID。
        id: String,
        /// 图像宽。
        width: u32,
        /// 图像高。
        height: u32,
        /// 不透明 RGBA 像素。
        rgba: Vec<u8>,
    },
    /// 贴图请求移到另一个分组（由管理器改记录并关闭窗口）。
    MoveToGroup {
        /// 贴图 ID。
        id: String,
        /// 目标分组 ID。
        group: String,
    },
    /// 贴图进入点击穿透：需要在它旁边放一个可点击的退出按钮。
    ClickThroughEntered {
        /// 贴图 ID。
        id: String,
        /// 贴图当前外框（物理像素）。
        rect: PhysicalRect,
        /// 贴图是否置顶（退出按钮要在同一层级）。
        topmost: bool,
    },
    /// 贴图请求隐藏到屏幕顶部：管理器放出把手小窗，并回告把手与工作区位置。
    HideToTopRequested {
        /// 贴图 ID。
        id: String,
        /// 贴图当前外框（物理像素）。
        rect: PhysicalRect,
        /// 贴图是否置顶。
        topmost: bool,
    },
    /// 贴图退出隐藏到顶部（或关闭）：撤掉把手小窗。
    HideToTopExited {
        /// 贴图 ID。
        id: String,
    },
    /// 贴图退出点击穿透（或关闭）：撤掉退出按钮。
    ClickThroughExited {
        /// 贴图 ID。
        id: String,
    },
}

/// 最多记住的最近关闭贴图数。
const MAX_CLOSED_PINS: usize = 10;
/// 最近关闭贴图占用内存的上限（源图 + 标注会话字节数之和）。
const MAX_CLOSED_BYTES: usize = 64 * 1024 * 1024;

/// 一张最近关闭的贴图（内存里的快照，用来恢复）。
#[derive(Debug, Clone, PartialEq)]
pub struct ClosedPin {
    /// 源图 PNG。
    pub png: Vec<u8>,
    /// 标注引擎会话字节；没有标注为空。
    pub session: Vec<u8>,
    /// 关闭时的窗口几何。
    pub geometry: Option<PinGeometry>,
}

impl ClosedPin {
    /// 占用的内存字节数。
    fn bytes(&self) -> usize {
        self.png.len() + self.session.len()
    }
}

/// 控制事件出口的类型。
type ControlSink = Box<dyn Fn(PinControlEvent)>;

/// 贴图共享上下文。
pub struct PinShared {
    /// 最近关闭的贴图（新的在末尾），受条数与内存双重上限约束。
    closed: RefCell<Vec<ClosedPin>>,
    /// 控制事件出口（由主程序接到主线程收件箱）。
    control_sink: RefCell<Option<ControlSink>>,
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
            closed: RefCell::new(Vec::new()),
            control_sink: RefCell::new(None),
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
            keymap: PinKeymap::from_document(doc),
            locale: crate::app_runtime::ui_prefs_from_document(doc).locale,
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

    /// 一张贴图所属的分组：已有记录沿用其分组，新贴图归入当前激活分组。
    fn group_for(&self, id: &str) -> String {
        let store = self.store.borrow();
        store
            .record(id)
            .and_then(|r| r.get("group_id").and_then(|g| g.as_str().map(str::to_string)))
            .unwrap_or_else(|| store.active_group_id())
    }

    /// 贴图当前保存的窗口几何；记录缺失或损坏返回 `None`。
    ///
    /// # 参数
    /// - `id`：贴图 ID。
    pub fn pin_geometry(&self, id: &str) -> Option<PinGeometry> {
        crate::pinned_model::record_geometry(&self.store.borrow().record(id)?)
    }

    /// 贴图源图的 PNG 字节（管理页缩略图用）；读取失败返回 `None`。
    ///
    /// # 参数
    /// - `id`：贴图 ID。
    pub fn source_png(&self, id: &str) -> Option<Vec<u8>> {
        let payload = self.store.borrow().load_payload(id).ok()??;
        payload.image.map(|image| image.bytes)
    }

    /// 全部分组（默认分组在最前）。
    pub fn groups(&self) -> Vec<PinGroup> {
        self.store.borrow().groups()
    }

    /// 当前激活分组 ID。
    pub fn active_group_id(&self) -> String {
        self.store.borrow().active_group_id()
    }

    /// 某分组里的贴图 ID（升序）。
    ///
    /// # 参数
    /// - `group`：分组 ID。
    pub fn ids_in_group(&self, group: &str) -> Vec<String> {
        let store = self.store.borrow();
        store
            .record_ids()
            .into_iter()
            .filter(|id| {
                store
                    .record(id)
                    .and_then(|r| r.get("group_id").and_then(|g| g.as_str().map(|g| g == group)))
                    .unwrap_or(false)
            })
            .collect()
    }

    /// 读取一个布尔配置项；缺失或类型不对按 `false`。
    ///
    /// # 参数
    /// - `key`：配置键。
    pub fn config_bool(&self, key: &str) -> bool {
        self.config.borrow().document().value(key).as_bool().unwrap_or(false)
    }

    /// 贴图所属分组 ID；仓储里没有这条记录时返回 `None`。
    ///
    /// # 参数
    /// - `id`：贴图 ID。
    pub fn pin_group(&self, id: &str) -> Option<String> {
        self.store
            .borrow()
            .record(id)
            .and_then(|r| r.get("group_id").and_then(|g| g.as_str().map(str::to_string)))
    }

    /// 新建分组。
    ///
    /// # 参数
    /// - `name`：分组名；`None` 时自动取「分组 N」，N 取第一个未被占用的序号。
    ///
    /// # 返回
    /// 新分组 ID；名称为空、超过 16 个 UTF-16 单元、重名或分组数已满返回错误说明。
    ///
    /// ```ignore
    /// let id = shared.create_group(Some("工作")).unwrap();
    /// ```
    pub fn create_group(&self, name: Option<&str>) -> Result<String, String> {
        let mut store = self.store.borrow_mut();
        let groups = store.groups();
        let name = match name.map(str::trim) {
            Some(n) => n.to_string(),
            None => (1..)
                .map(|n| format!("分组 {n}"))
                .find(|candidate| !groups.iter().any(|g| &g.name == candidate))
                .unwrap_or_default(),
        };
        if name.is_empty() {
            return Err("分组名不能为空".into());
        }
        if name.encode_utf16().count() > MAX_GROUP_NAME_UNITS {
            return Err(format!("分组名不能超过 {MAX_GROUP_NAME_UNITS} 个字符"));
        }
        if groups.iter().any(|g| g.name == name) {
            return Err("已有同名分组".into());
        }
        if groups.len() >= MAX_GROUPS {
            return Err("分组数量已达上限".into());
        }
        let id = snow_history::pin_id::new_uuid_v4();
        let mut next: Vec<PinGroup> = groups.into_iter().filter(|g| !g.built_in).collect();
        next.push(PinGroup { id: id.clone(), name, built_in: false });
        let active = store.active_group_id();
        store.set_groups(&next, &active);
        store.flush().map_err(|e| e.to_string())?;
        Ok(id)
    }

    /// 删除一个分组及其中的全部贴图；默认分组不可删。
    ///
    /// # 参数
    /// - `group`：分组 ID。
    ///
    /// # 返回
    /// 被一并删除的贴图 ID（调用方应关闭对应窗口）；分组不存在或是默认分组返回错误。
    pub fn delete_group(&self, group: &str) -> Result<Vec<String>, String> {
        if group == DEFAULT_GROUP_ID {
            return Err("默认分组不能删除".into());
        }
        if !self.groups().iter().any(|g| g.id == group) {
            return Err("分组不存在".into());
        }
        let victims = self.ids_in_group(group);
        let mut store = self.store.borrow_mut();
        for id in &victims {
            store.remove(id);
        }
        let next: Vec<PinGroup> = store.groups().into_iter().filter(|g| !g.built_in && g.id != group).collect();
        let active = store.active_group_id();
        let active = if active == group { DEFAULT_GROUP_ID.to_string() } else { active };
        store.set_groups(&next, &active);
        store.flush().map_err(|e| e.to_string())?;
        Ok(victims)
    }

    /// 删除全部没有贴图的自定义分组。
    ///
    /// # 返回
    /// 被删除的分组数。
    pub fn delete_empty_groups(&self) -> Result<usize, String> {
        let empty: Vec<String> = self
            .groups()
            .into_iter()
            .filter(|g| !g.built_in && self.ids_in_group(&g.id).is_empty())
            .map(|g| g.id)
            .collect();
        for id in &empty {
            self.delete_group(id)?;
        }
        Ok(empty.len())
    }

    /// 切换激活分组并落盘。
    ///
    /// # 参数
    /// - `group`：分组 ID，必须存在。
    pub fn set_active_group(&self, group: &str) -> Result<(), String> {
        let mut store = self.store.borrow_mut();
        let groups = store.groups();
        if !groups.iter().any(|g| g.id == group) {
            return Err("分组不存在".into());
        }
        let custom: Vec<PinGroup> = groups.into_iter().filter(|g| !g.built_in).collect();
        store.set_groups(&custom, group);
        store.flush().map_err(|e| e.to_string())
    }

    /// 把一张贴图移到另一个分组（只改清单，不动图片）。
    ///
    /// # 参数
    /// - `id`：贴图 ID。
    /// - `group`：目标分组 ID。
    pub fn move_pin(&self, id: &str, group: &str) -> Result<(), String> {
        let mut store = self.store.borrow_mut();
        if !store.groups().iter().any(|g| g.id == group) {
            return Err("分组不存在".into());
        }
        let mut record = store.record(id).ok_or_else(|| "贴图不存在".to_string())?;
        record.insert("group_id".into(), serde_json::Value::String(group.into()));
        store.upsert(record, None).map_err(|e| e.to_string())?;
        store.flush().map_err(|e| e.to_string())
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
        let group = self.group_for(id);
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
        let group = store
            .record(id)
            .and_then(|r| r.get("group_id").and_then(|g| g.as_str().map(str::to_string)))
            .unwrap_or_else(|| store.active_group_id());
        let record = build_record(id, &group, geometry, created_ms, total);
        store
            .upsert(record, Some(payload))
            .map_err(|e| e.to_string())?;
        store.flush().map_err(|e| e.to_string())?;
        Ok(total)
    }

    /// 用户主动关闭一张贴图：先把它的源图、标注会话和几何记进「最近关闭」，再从仓储移除。
    ///
    /// # 参数
    /// - `id`：贴图 ID。
    ///
    /// # 返回
    /// 仓储里确有这条记录并已移除时为 `true`。
    pub fn remove_remembering(&self, id: &str) -> bool {
        let snapshot = {
            let store = self.store.borrow();
            let geometry = store.record(id).and_then(|r| crate::pinned_model::record_geometry(&r));
            store.load_payload(id).ok().flatten().and_then(|payload| {
                payload.image.map(|image| ClosedPin { png: image.bytes, session: payload.canvas_session, geometry })
            })
        };
        if let Some(pin) = snapshot {
            let mut closed = self.closed.borrow_mut();
            closed.push(pin);
            while closed.len() > MAX_CLOSED_PINS || (closed.len() > 1 && closed.iter().map(ClosedPin::bytes).sum::<usize>() > MAX_CLOSED_BYTES) {
                closed.remove(0);
            }
        }
        self.remove(id)
    }

    /// 取出最近关闭的一张贴图（后进先出）；没有则为 `None`。
    pub fn pop_closed(&self) -> Option<ClosedPin> {
        self.closed.borrow_mut().pop()
    }

    /// 当前可恢复的最近关闭贴图数。
    pub fn closed_count(&self) -> usize {
        self.closed.borrow().len()
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

    /// 设置控制事件出口（启动时由主程序调用一次）。
    ///
    /// # 参数
    /// - `sink`：收到事件时调用，应只做轻量转发。
    pub fn set_control_sink(&self, sink: Box<dyn Fn(PinControlEvent)>) {
        *self.control_sink.borrow_mut() = Some(sink);
    }

    /// 请求把一张贴图移到另一个分组（经控制事件交给管理器处理）。
    ///
    /// # 参数
    /// - `id`：贴图 ID。
    /// - `group`：目标分组 ID。
    pub fn request_move_to_group(&self, id: &str, group: &str) {
        self.emit_control(PinControlEvent::MoveToGroup { id: id.to_string(), group: group.to_string() });
    }

    /// 发出一个控制事件；没有设置出口时忽略。
    ///
    /// # 参数
    /// - `event`：控制事件。
    pub fn emit_control(&self, event: PinControlEvent) {
        if let Some(sink) = self.control_sink.borrow().as_ref() {
            sink(event);
        }
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

    /// 分组：新建 / 重名 / 超长 / 移动 / 删除 / 切换激活分组，且几何更新不会把贴图挪回激活分组。
    #[test]
    fn groups_create_move_delete_and_keep_membership() {
        let dir = temp_dir("groups");
        let (shared, _) = open_in(&dir);
        let work = shared.create_group(Some("工作")).unwrap();
        assert!(shared.create_group(Some("工作")).is_err());
        assert!(shared.create_group(Some("")).is_err());
        assert!(shared.create_group(Some("一二三四五六七八九十一二三四五六七")).is_err());
        let auto = shared.create_group(None).unwrap();
        assert_eq!(shared.groups().iter().find(|g| g.id == auto).unwrap().name, "分组 1");

        let a = store_pin(&shared, 8, 8, PhysicalRect::new(0, 0, 8, 8));
        let b = store_pin(&shared, 8, 8, PhysicalRect::new(10, 0, 8, 8));
        assert_eq!(shared.pin_group(&a).as_deref(), Some("default"));
        shared.move_pin(&a, &work).unwrap();
        shared.set_active_group(&work).unwrap();
        // 激活分组变了，但已有记录的几何更新仍留在自己的分组
        let geometry = PinGeometry::new(PhysicalRect::new(5, 5, 8, 8), 1.0, 1.0, true);
        shared.persist(&b, &geometry, 1, 10, None).unwrap();
        assert_eq!(shared.pin_group(&b).as_deref(), Some("default"));
        assert_eq!(shared.ids_in_group(&work), vec![a.clone()]);

        assert!(shared.delete_group("default").is_err());
        assert_eq!(shared.delete_empty_groups().unwrap(), 1);
        let removed = shared.delete_group(&work).unwrap();
        assert_eq!(removed, vec![a]);
        assert_eq!(shared.active_group_id(), "default");
        assert_eq!(shared.ids_in_group("default"), vec![b]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 关闭记忆：主动关闭的贴图可按后进先出恢复（含几何），超过条数上限丢最旧的；仅仓储移除（淘汰）不记忆。
    #[test]
    fn closed_pins_restore_lifo_and_respect_limit() {
        let dir = temp_dir("closed");
        let (shared, _) = open_in(&dir);
        let first = store_pin(&shared, 8, 8, PhysicalRect::new(1, 2, 8, 8));
        let second = store_pin(&shared, 8, 8, PhysicalRect::new(3, 4, 8, 8));
        assert!(shared.remove_remembering(&first));
        assert!(shared.remove_remembering(&second));
        assert!(!shared.remove_remembering(&second), "记录已不在，不应重复记忆");
        assert_eq!(shared.closed_count(), 2);
        let latest = shared.pop_closed().unwrap();
        assert_eq!(latest.geometry.unwrap().rect(), PhysicalRect::new(3, 4, 8, 8));
        assert!(!latest.png.is_empty());
        let older = shared.pop_closed().unwrap();
        assert_eq!(older.geometry.unwrap().rect(), PhysicalRect::new(1, 2, 8, 8));
        assert!(shared.pop_closed().is_none());

        let kept = store_pin(&shared, 8, 8, PhysicalRect::new(0, 0, 8, 8));
        assert!(shared.remove(&kept));
        assert_eq!(shared.closed_count(), 0, "普通移除（淘汰 / 删除）不进最近关闭");
        for _ in 0..(MAX_CLOSED_PINS + 3) {
            let id = store_pin(&shared, 8, 8, PhysicalRect::new(0, 0, 8, 8));
            shared.remove_remembering(&id);
        }
        assert_eq!(shared.closed_count(), MAX_CLOSED_PINS);
        let _ = std::fs::remove_dir_all(&dir);
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
