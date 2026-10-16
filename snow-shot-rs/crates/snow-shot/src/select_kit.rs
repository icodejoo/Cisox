//! 统一的紧凑下拉封装：各面板（录制工具栏、截图设置面板等）都走这里，尺寸、菜单高度与图标行为保持一致。

use snow_ui::ui::component::searchable_list::{SearchableListDelegate, SearchableListItem};
use snow_ui::ui::component::select::{Select, SelectState};
use snow_ui::ui::component::{Icon, Sizable, Size as ComponentSize};
use snow_ui::ui::*;

/// 紧凑下拉的可选项。
#[derive(Default)]
pub struct CompactSelect {
    /// 触发器左侧图标资源路径。
    pub icon: Option<&'static str>,
    /// 未选中时的占位文字。
    pub placeholder: Option<SharedString>,
}

/// 构造固定宽高的紧凑下拉（小号尺寸，菜单限高）。
///
/// # 参数
/// - `state`：下拉状态实体。
/// - `(width, height)`：触发器逻辑宽高。
/// - `menu_max_h`：菜单最大逻辑高度。
/// - `opts`：图标与占位文字。
///
/// # 返回
/// 可直接 `.child(..)` 的容器。
///
/// # 示例
/// ```ignore
/// let el = compact_select(&state, (100.0, 28.0), 200.0, CompactSelect::default());
/// ```
pub fn compact_select<D>(
    state: &Entity<SelectState<D>>,
    (width, height): (f32, f32),
    menu_max_h: f32,
    opts: CompactSelect,
) -> Div
where
    D: SearchableListDelegate + 'static,
    <D::Item as SearchableListItem>::Value: PartialEq + Clone,
{
    let mut select = Select::new(state)
        .with_size(ComponentSize::Small)
        .menu_max_h(px(menu_max_h));
    if let Some(icon) = opts.icon {
        select = select.icon(Icon::default().path(icon));
    }
    if let Some(text) = opts.placeholder {
        select = select.placeholder(text);
    }
    div().flex_none().w(px(width)).h(px(height)).child(select)
}
