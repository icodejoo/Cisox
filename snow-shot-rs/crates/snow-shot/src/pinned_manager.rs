//! 贴图管理器（Pinned Manager）。
//!
//! 负责多贴图窗口生命周期调度、分组管理（创建/切换分组）、与底层 `snow_history::pinned::PinnedStore`
//! 仓储的持久化同步（读取/保存/删除）。

use std::collections::BTreeMap;
use std::path::Path;
use snow_history::pinned::{PinError, PinGroup, PinOptions, PinnedStore};
use snow_ui::shell::geometry::PhysicalRect;
use crate::pinned_view::PinnedWindowView;

/// 贴图窗口集中管理器。
pub struct PinnedManager {
    /// 底层持久化仓储。
    pub store: PinnedStore,
    /// 当前内存中激活的贴图窗口实例映射表。
    pub active_pins: BTreeMap<String, PinnedWindowView>,
    /// 当前活动分组 ID。
    pub current_group_id: String,
    /// 自动递增计数器（用于生成唯一 ID）。
    next_id: u64,
}

impl PinnedManager {
    /// 构造新的贴图管理器。
    ///
    /// # 参数
    /// - `data_root`: 应用数据根目录。
    ///
    /// # 返回
    /// 管理器实例。
    ///
    /// # 示例
    /// ```rust
    /// use std::path::Path;
    /// use snow_shot::pinned_manager::PinnedManager;
    /// let temp = std::env::temp_dir().join("snow_shot_pin_mgr_test");
    /// let mgr = PinnedManager::new(&temp);
    /// assert_eq!(mgr.pin_count(), 0);
    /// ```
    pub fn new(data_root: &Path) -> Self {
        let store = PinnedStore::open(data_root, PinOptions::default());
        let current_group_id = store.active_group_id();
        Self {
            store,
            active_pins: BTreeMap::new(),
            current_group_id,
            next_id: 1,
        }
    }

    /// 从选区捕获位图创建并注册一张新的贴图。
    ///
    /// # 参数
    /// - `image_width`: 选区宽度。
    /// - `image_height`: 选区高度。
    /// - `image_rgba`: 选区 RGBA 像素数据。
    /// - `bounds`: 屏幕物理矩形。
    ///
    /// # 返回
    /// 贴图的唯一 UUID 标识符。
    pub fn create_pin(
        &mut self,
        image_width: u32,
        image_height: u32,
        image_rgba: Vec<u8>,
        bounds: PhysicalRect,
    ) -> String {
        let id = format!(
            "00000000-0000-4000-8000-{:012x}",
            self.next_id
        );
        self.next_id += 1;

        let pin_view = PinnedWindowView::new(
            id.clone(),
            self.current_group_id.clone(),
            image_width,
            image_height,
            image_rgba,
            bounds,
        );

        self.active_pins.insert(id.clone(), pin_view);
        id
    }

    /// 获取贴图引用。
    pub fn get_pin(&self, id: &str) -> Option<&PinnedWindowView> {
        self.active_pins.get(id)
    }

    /// 获取贴图可变引用。
    pub fn get_pin_mut(&mut self, id: &str) -> Option<&mut PinnedWindowView> {
        self.active_pins.get_mut(id)
    }

    /// 关闭贴图。
    pub fn close_pin(&mut self, id: &str) -> Option<PinnedWindowView> {
        self.active_pins.remove(id)
    }

    /// 将指定贴图持久化保存到仓储中。
    pub fn save_pin(&mut self, id: &str) -> Result<(), PinError> {
        if let Some(pin) = self.active_pins.get(id) {
            pin.save_to_store(&mut self.store)
        } else {
            Err(PinError::Invalid("Pin not found".to_string()))
        }
    }

    /// 保存全部活动贴图到持久化仓储。
    pub fn save_all(&mut self) -> Result<(), PinError> {
        let pins: Vec<PinnedWindowView> = self.active_pins.values().map(|p| {
            PinnedWindowView {
                id: p.id.clone(),
                group_id: p.group_id.clone(),
                image_width: p.image_width,
                image_height: p.image_height,
                image_rgba: p.image_rgba.clone(),
                bounds: p.bounds,
                zoom: p.zoom,
                opacity: p.opacity,
                is_pinned_on_top: p.is_pinned_on_top,
                is_editing: p.is_editing,
                active_tool: p.active_tool,
                active_color: p.active_color,
                stroke_width: p.stroke_width,
                annotations: p.annotations.clone(),
                undo_stack: p.undo_stack.clone(),
                redo_stack: p.redo_stack.clone(),
                drag_handle: None,
                drag_start_pos: p.drag_start_pos,
                drag_start_bounds: p.drag_start_bounds,
                drawing_start_point: None,
                current_drawing_point: None,
                is_hovered: false,
                status_message: None,
            }
        }).collect();

        for pin in pins {
            pin.save_to_store(&mut self.store)?;
        }
        Ok(())
    }

    /// 获取当前所有可用贴图分组列表。
    pub fn groups(&self) -> Vec<PinGroup> {
        self.store.groups()
    }

    /// 获取当前活动的贴图数量。
    pub fn pin_count(&self) -> usize {
        self.active_pins.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 验证贴图管理器的创建、添加、保存与关闭流程。
    #[test]
    fn test_pinned_manager_workflow() {
        let temp_dir = std::env::temp_dir().join("snow_shot_pin_mgr_test_dir");
        let mut mgr = PinnedManager::new(&temp_dir);

        let id = mgr.create_pin(
            50,
            50,
            vec![200; 50 * 50 * 4],
            PhysicalRect::new(10, 10, 50, 50),
        );

        assert_eq!(mgr.pin_count(), 1);
        assert!(mgr.get_pin(&id).is_some());

        // 保存单张贴图
        let save_res = mgr.save_pin(&id);
        assert!(save_res.is_ok(), "Save pin failed: {:?}", save_res);

        // 关闭贴图
        let closed = mgr.close_pin(&id);
        assert!(closed.is_some());
        assert_eq!(mgr.pin_count(), 0);
    }
}
