//! 贴图管理页的列表模型：把仓储里的贴图整理成行（分组名、是否正在显示、尺寸、时间、体积），
//! 并支持按分组筛选。不依赖 GPUI，可离屏单测；窗口视图见 [`crate::pinned_manage_view`]。

use crate::pinned_shared::PinShared;
use snow_history::pinned::DEFAULT_GROUP_ID;
use std::collections::BTreeSet;

/// 管理页里的一行。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PinRow {
    /// 贴图 ID。
    pub id: String,
    /// 所属分组 ID。
    pub group_id: String,
    /// 所属分组显示名（默认分组为 `None`，由视图换成本地化文案）。
    pub group_name: Option<String>,
    /// 当前是否有窗口正在显示（`false` 表示只保留在存储里，如其它分组的贴图）。
    pub open: bool,
    /// 窗口宽（物理像素，来自保存的几何；缺失为 0）。
    pub width: i32,
    /// 窗口高（物理像素）。
    pub height: i32,
    /// 创建时间（UTC 毫秒）。
    pub created_ms: i64,
    /// 源图体积（字节）。
    pub payload_bytes: u64,
}

/// 分组筛选。
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum GroupFilter {
    /// 全部分组。
    #[default]
    All,
    /// 指定分组。
    Group(String),
}

/// 管理页的行数据快照。
///
/// # 参数
/// - `shared`：贴图共享上下文（读仓储）。
/// - `open_ids`：当前有窗口的贴图 ID。
/// - `filter`：分组筛选。
///
/// # 返回
/// 按创建时间从新到旧排序的行。
///
/// ```ignore
/// let rows = build_rows(&shared, &open_ids, &GroupFilter::All);
/// ```
pub fn build_rows(
    shared: &PinShared,
    open_ids: &BTreeSet<String>,
    filter: &GroupFilter,
) -> Vec<PinRow> {
    let groups = shared.groups();
    let mut rows: Vec<PinRow> = shared
        .stored_entries()
        .into_iter()
        .filter_map(|entry| {
            let group_id = shared.pin_group(&entry.id)?;
            if let GroupFilter::Group(wanted) = filter
                && wanted != &group_id
            {
                return None;
            }
            let group_name = groups
                .iter()
                .find(|g| g.id == group_id)
                .filter(|g| !g.built_in)
                .map(|g| g.name.clone());
            let geometry = shared.pin_geometry(&entry.id);
            Some(PinRow {
                open: open_ids.contains(&entry.id),
                width: geometry.map_or(0, |g| g.width),
                height: geometry.map_or(0, |g| g.height),
                id: entry.id,
                group_id,
                group_name,
                created_ms: entry.created_ms,
                payload_bytes: entry.bytes,
            })
        })
        .collect();
    rows.sort_by(|a, b| {
        b.created_ms
            .cmp(&a.created_ms)
            .then_with(|| a.id.cmp(&b.id))
    });
    rows
}

/// 筛选项是否仍然有效（分组被删后回到「全部」）。
///
/// # 参数
/// - `filter`：当前筛选。
/// - `shared`：贴图共享上下文。
pub fn valid_filter(filter: GroupFilter, shared: &PinShared) -> GroupFilter {
    match filter {
        GroupFilter::Group(id) if !shared.groups().iter().any(|g| g.id == id) => GroupFilter::All,
        other => other,
    }
}

/// 默认分组的 ID（供视图判断「删除此分组」是否可用）。
pub const fn default_group_id() -> &'static str {
    DEFAULT_GROUP_ID
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pinned_model::PinGeometry;
    use crate::screenshot_output::encode_png;
    use crate::settings_state::SharedConfig;
    use snow_config::store::ConfigStore;
    use snow_ui::shell::geometry::PhysicalRect;
    use std::cell::RefCell;
    use std::path::PathBuf;
    use std::rc::Rc;

    /// 在唯一临时目录里打开共享上下文。
    fn open(tag: &str) -> (Rc<PinShared>, PathBuf) {
        let dir =
            std::env::temp_dir().join(format!("snow-pin-manage-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let config: SharedConfig = Rc::new(RefCell::new(ConfigStore::open(dir.join("cfg.json"))));
        (PinShared::open(&dir, config, Box::new(|_| {})), dir)
    }

    /// 存一张贴图（创建时间可指定）。
    fn store(shared: &PinShared, created_ms: i64) -> String {
        let id = shared.new_id().unwrap();
        let png = encode_png(4, 4, &[200u8; 64]).unwrap();
        let geometry = PinGeometry::new(PhysicalRect::new(0, 0, 4, 4), 1.0, 1.0, true);
        shared
            .persist(&id, &geometry, created_ms, png.len() as u64, Some(png))
            .unwrap();
        id
    }

    /// 行按创建时间倒序，带分组名与是否打开；筛选只留指定分组。
    #[test]
    fn rows_sorted_filtered_and_flagged() {
        let (shared, dir) = open("rows");
        let a = store(&shared, 100);
        let b = store(&shared, 200);
        let work = shared
            .create_group(Some("工作"), crate::ocr_backend::i18n_for("zh-CN"))
            .unwrap();
        shared.move_pin(&a, &work).unwrap();
        let open_ids: BTreeSet<String> = [b.clone()].into();

        let all = build_rows(&shared, &open_ids, &GroupFilter::All);
        assert_eq!(
            all.iter().map(|r| r.id.as_str()).collect::<Vec<_>>(),
            [b.as_str(), a.as_str()]
        );
        assert!(all[0].open && all[0].group_name.is_none());
        assert!(!all[1].open && all[1].group_name.as_deref() == Some("工作"));

        let only = build_rows(&shared, &open_ids, &GroupFilter::Group(work.clone()));
        assert_eq!(only.len(), 1);
        assert_eq!(only[0].id, a);
        assert_eq!(
            valid_filter(GroupFilter::Group("gone".into()), &shared),
            GroupFilter::All
        );
        assert_eq!(
            valid_filter(GroupFilter::Group(work.clone()), &shared),
            GroupFilter::Group(work)
        );
        assert_eq!(default_group_id(), "default");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
