//! 工具栏下拉二级菜单的定位（纯计算，不依赖 GPUI）。
//!
//! 二级菜单脱离主菜单的文档流绝对定位：主菜单位置只由自己决定，
//! 二级菜单出现 / 消失不会让光标下的行换位；空间不够时二级菜单自己换边、夹进窗口。

/// 二级菜单相对主菜单所在的一侧。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SubmenuSide {
    /// 主菜单右侧。
    Right,
    /// 主菜单左侧。
    Left,
}

/// 二级菜单定位的输入（逻辑像素）。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SubmenuInput {
    /// 主菜单左上角在窗口中的位置；还没量到（弹出的第一帧）时为 `None`，不做边界修正。
    pub menu_origin: Option<(f32, f32)>,
    /// 主菜单外框尺寸（宽, 高）。
    pub menu_size: (f32, f32),
    /// 二级菜单外框尺寸（宽, 高）。
    pub sub_size: (f32, f32),
    /// 窗口可视区尺寸（宽, 高）。
    pub viewport: (f32, f32),
    /// 与窗口边缘保持的距离。
    pub margin: f32,
    /// 两个菜单之间的间隔；这段间隔归二级菜单的悬停区，鼠标穿过时不会收起。
    pub gap: f32,
    /// 主菜单向上展开时为 `true`：二级菜单与主菜单底边对齐，否则与顶边对齐。
    pub bottom_aligned: bool,
}

/// 二级菜单的定位结果（相对主菜单左上角）。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SubmenuPlacement {
    /// 所在的一侧。
    pub side: SubmenuSide,
    /// 悬停容器（二级菜单 + 间隔）左边缘的水平偏移。
    pub left: f32,
    /// 悬停容器顶边的垂直偏移。
    pub top: f32,
}

/// 计算二级菜单摆在哪。
///
/// # 参数
/// - `input`: 两个菜单的尺寸、主菜单位置与窗口尺寸。
///
/// # 返回
/// 偏移相对主菜单左上角；只依赖主菜单位置与二级菜单自身尺寸，不改变主菜单布局。
///
/// # 示例
/// ```ignore
/// let p = place_submenu(&input);
/// // 容器 left/top 即二级菜单悬停容器相对主菜单左上角的偏移
/// ```
pub fn place_submenu(input: &SubmenuInput) -> SubmenuPlacement {
    let (menu_w, menu_h) = input.menu_size;
    let (sub_w, sub_h) = input.sub_size;
    let aligned_top = if input.bottom_aligned {
        menu_h - sub_h
    } else {
        0.0
    };
    let right = SubmenuPlacement {
        side: SubmenuSide::Right,
        left: menu_w,
        top: aligned_top,
    };
    let Some((x, y)) = input.menu_origin else {
        return right;
    };

    // 右侧放得下就放右侧，否则看左侧；两边都放不下取空间大的一边
    let (view_w, view_h) = input.viewport;
    let room_right = view_w - input.margin - (x + menu_w + input.gap);
    let room_left = x - input.gap - input.margin;
    let side = if room_right >= sub_w || (room_left < sub_w && room_right >= room_left) {
        SubmenuSide::Right
    } else {
        SubmenuSide::Left
    };
    let left = match side {
        SubmenuSide::Right => menu_w,
        SubmenuSide::Left => -(sub_w + input.gap),
    };

    // 垂直方向夹进窗口；比窗口还高时贴上边
    let max_top = view_h - input.margin - sub_h;
    let top = (y + aligned_top).min(max_top).max(input.margin) - y;
    SubmenuPlacement { side, left, top }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MARGIN: f32 = 8.0;
    const GAP: f32 = 2.0;

    /// 构造一份基础输入：主菜单 120x100，二级菜单 100x200，窗口 800x600。
    fn input(origin: Option<(f32, f32)>, bottom_aligned: bool) -> SubmenuInput {
        SubmenuInput {
            menu_origin: origin,
            menu_size: (120.0, 100.0),
            sub_size: (100.0, 200.0),
            viewport: (800.0, 600.0),
            margin: MARGIN,
            gap: GAP,
            bottom_aligned,
        }
    }

    /// 向下展开、右侧空间足够：放右侧，顶边对齐主菜单。
    #[test]
    fn down_with_room_goes_right_top_aligned() {
        let p = place_submenu(&input(Some((100.0, 100.0)), false));
        assert_eq!(p.side, SubmenuSide::Right);
        assert_eq!((p.left, p.top), (120.0, 0.0));
    }

    /// 向上展开：二级菜单比主菜单高，底边对齐，向上长出去而不推动主菜单。
    #[test]
    fn up_aligns_bottom_and_grows_upward() {
        let p = place_submenu(&input(Some((100.0, 300.0)), true));
        assert_eq!(p.side, SubmenuSide::Right);
        assert_eq!(p.top, 100.0 - 200.0);
    }

    /// 贴近窗口右边缘：右侧放不下，换到左侧。
    #[test]
    fn near_right_edge_flips_left() {
        let p = place_submenu(&input(Some((650.0, 100.0)), false));
        assert_eq!(p.side, SubmenuSide::Left);
        assert_eq!(p.left, -(100.0 + GAP));
    }

    /// 两侧都放不下时取空间大的一侧，不抖动。
    #[test]
    fn picks_larger_side_when_neither_fits() {
        let mut i = input(Some((60.0, 100.0)), false);
        i.viewport = (260.0, 600.0);
        i.sub_size = (150.0, 200.0);
        // 右侧余量 260-8-182=70，左侧余量 60-2-8=50
        assert_eq!(place_submenu(&i).side, SubmenuSide::Right);
    }

    /// 垂直方向溢出窗口时夹回边距内，偏移相对主菜单。
    #[test]
    fn vertical_overflow_is_clamped() {
        // 向下展开且主菜单靠近窗口底部：二级菜单整体上移到底边留 margin
        let p = place_submenu(&input(Some((100.0, 450.0)), false));
        assert_eq!(p.top, 600.0 - MARGIN - 200.0 - 450.0);
        // 向上展开且主菜单靠近窗口顶部：二级菜单下移到顶边留 margin
        let p = place_submenu(&input(Some((100.0, 50.0)), true));
        assert_eq!(p.top, MARGIN - 50.0);
    }

    /// 主菜单位置未知时（首帧）放右侧，不做边界修正。
    #[test]
    fn unknown_origin_defaults_to_right() {
        let p = place_submenu(&input(None, false));
        assert_eq!(p.side, SubmenuSide::Right);
        assert_eq!((p.left, p.top), (120.0, 0.0));
    }

    /// 二级菜单位置只取决于主菜单位置与自身尺寸，与是否展开无关：同一输入结果稳定。
    #[test]
    fn placement_is_stable_for_same_input() {
        let i = input(Some((650.0, 300.0)), true);
        assert_eq!(place_submenu(&i), place_submenu(&i));
    }
}
