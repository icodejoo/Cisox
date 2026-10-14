//! 覆盖窗里 OCR 结果弹窗的布局与拖动（纯数据，不依赖 GPUI，便于离屏测试）。
//!
//! 坐标全部是画布逻辑像素：弹窗尺寸估算、自动摆放（选区外优先，放不下落到选区内侧）、
//! 拖动后的位置夹取。真正的绘制与鼠标接线在 `overlay_view.rs`。

use crate::translate_render::detail_panel_origin;
use snow_ui::shell::geometry::PhysicalRect;

/// 弹窗的逻辑宽度（屏幕放不下时按屏宽收窄）。
pub const POPUP_WIDTH: f32 = 360.0;
/// 弹窗的逻辑高度上限（内容更多时内容区滚动）。
pub const POPUP_MAX_HEIGHT: f32 = 320.0;
/// 弹窗与选区、屏幕边缘的间距（逻辑像素）。
pub const POPUP_GAP: f32 = 8.0;
/// 标题栏高度（同时是可拖动区域的高度）。
pub const POPUP_TITLE_HEIGHT: f32 = 28.0;
/// 操作栏高度（含内边距）。
pub const POPUP_ACTION_HEIGHT: f32 = 40.0;
/// 操作栏按钮间距。
pub const POPUP_BUTTON_GAP: f32 = 6.0;
/// 内容区行高估值。
const LINE_HEIGHT: f32 = 18.0;
/// 内容区单个半角字符宽度估值（全角按两个计）。
const CHAR_WIDTH: f32 = 7.0;
/// 内容区左右内边距之和（含滚动条余量）。
const BODY_PADDING_X: f32 = 24.0;
/// 内容区每行上下内边距之和（含描边）。
const ROW_PADDING_Y: f32 = 6.0;
/// 内容区整体上下内边距之和。
const BODY_PADDING_Y: f32 = 8.0;

/// 文本折行时占的“半角格数”：ASCII 记 1，其余（中日韩等全角）记 2。
fn text_cells(text: &str) -> usize {
    text.chars().map(|c| if c.is_ascii() { 1 } else { 2 }).sum()
}

/// 估算内容区高度（逻辑像素）：每行按宽度估折行，再加行内边距与整体内边距。
///
/// # 参数
/// - `rows`：内容区每一行的文字（含二维码提示等附加行）。
/// - `width`：弹窗逻辑宽度。
///
/// # 返回
/// 内容区估算高度；没有行时只有整体内边距。
///
/// ```ignore
/// let h = estimate_body_height(&["第一行".to_string()], 360.0);
/// ```
pub fn estimate_body_height(rows: &[String], width: f32) -> f32 {
    let per_row = (((width - BODY_PADDING_X) / CHAR_WIDTH).floor() as usize).max(1);
    let lines: f32 = rows
        .iter()
        .map(|r| {
            let wrapped = text_cells(r).div_ceil(per_row).max(1);
            wrapped as f32 * LINE_HEIGHT + ROW_PADDING_Y
        })
        .sum();
    lines + BODY_PADDING_Y
}

/// 弹窗尺寸：宽度取 [`POPUP_WIDTH`]，高度按内容估算，都不超过本屏（四边各留 [`POPUP_GAP`]）。
///
/// # 参数
/// - `rows`：内容区每一行的文字。
/// - `screen_size`：本屏逻辑尺寸 `(宽, 高)`。
///
/// # 返回
/// 弹窗 `(宽, 高)`，至少为 1。
///
/// ```ignore
/// let (w, h) = popup_size(&rows, (1920.0, 1080.0));
/// ```
pub fn popup_size(rows: &[String], screen_size: (f32, f32)) -> (f32, f32) {
    let width = POPUP_WIDTH.min(screen_size.0 - 2.0 * POPUP_GAP).max(1.0);
    let content = POPUP_TITLE_HEIGHT + POPUP_ACTION_HEIGHT + estimate_body_height(rows, width);
    let height = content
        .min(POPUP_MAX_HEIGHT)
        .min(screen_size.1 - 2.0 * POPUP_GAP)
        .max(1.0);
    (width, height)
}

/// 把弹窗左上角限制在本屏内（整块弹窗不出屏；放不下时优先保证左上角可见）。
///
/// # 参数
/// - `want`：期望的左上角。
/// - `size`：弹窗 `(宽, 高)`。
/// - `screen`：本屏 `(x, y, 宽, 高)`。
///
/// # 返回
/// 夹取后的左上角。
///
/// ```ignore
/// let (x, y) = clamp_popup_origin((5000.0, -20.0), (360.0, 200.0), (0.0, 0.0, 1920.0, 1080.0));
/// ```
pub fn clamp_popup_origin(
    want: (f32, f32),
    size: (f32, f32),
    screen: (f32, f32, f32, f32),
) -> (f32, f32) {
    let (sx, sy, sw, sh) = screen;
    (
        want.0.min(sx + sw - size.0).max(sx),
        want.1.min(sy + sh - size.1).max(sy),
    )
}

/// 弹窗的自动位置：优先放在选区外侧（右、左、下、上，避开 `avoid`），都放不下就落到选区内侧。
///
/// # 参数
/// - `selection`：选区（逻辑像素，已裁到本屏）；没有时按整屏处理。
/// - `size`：弹窗 `(宽, 高)`。
/// - `screen`：本屏 `(x, y, 宽, 高)`。
/// - `avoid`：不能压住的矩形（工具栏等）。
///
/// # 返回
/// 弹窗左上角（逻辑像素）。
///
/// ```ignore
/// let (x, y) = auto_origin(Some(sel), (360.0, 200.0), screen, &[toolbar]);
/// ```
pub fn auto_origin(
    selection: Option<PhysicalRect>,
    size: (f32, f32),
    screen: (f32, f32, f32, f32),
    avoid: &[PhysicalRect],
) -> (f32, f32) {
    let screen_rect = PhysicalRect::new(
        screen.0 as i32,
        screen.1 as i32,
        screen.2 as i32,
        screen.3 as i32,
    );
    let (x, y) = detail_panel_origin(
        selection.unwrap_or(screen_rect),
        (size.0.ceil() as i32, size.1.ceil() as i32),
        screen_rect,
        avoid,
        POPUP_GAP as i32,
    );
    (x as f32, y as f32)
}

/// 弹窗最终位置：用户拖动过就用拖动位置（再夹回本屏），否则用自动位置。
///
/// # 参数
/// - `auto`：自动位置。
/// - `manual`：用户拖动后的位置。
/// - `size`：弹窗 `(宽, 高)`。
/// - `screen`：本屏 `(x, y, 宽, 高)`。
///
/// ```ignore
/// let origin = resolve_origin(auto, Some((100.0, 80.0)), size, screen);
/// ```
pub fn resolve_origin(
    auto: (f32, f32),
    manual: Option<(f32, f32)>,
    size: (f32, f32),
    screen: (f32, f32, f32, f32),
) -> (f32, f32) {
    manual.map_or(auto, |m| clamp_popup_origin(m, size, screen))
}

/// 一次进行中的标题栏拖动：记住抓取点相对弹窗左上角的偏移，以及拖动期间的尺寸和屏幕范围。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PopupDrag {
    /// 按下点相对弹窗左上角的偏移。
    grab: (f32, f32),
    /// 弹窗 `(宽, 高)`。
    size: (f32, f32),
    /// 弹窗所在屏的 `(x, y, 宽, 高)`，拖动不能越出它。
    screen: (f32, f32, f32, f32),
}

impl PopupDrag {
    /// 开始拖动。
    ///
    /// # 参数
    /// - `cursor`：按下点（画布逻辑坐标）。
    /// - `origin`：此刻弹窗左上角。
    /// - `size`：弹窗 `(宽, 高)`。
    /// - `screen`：弹窗所在屏 `(x, y, 宽, 高)`。
    ///
    /// ```ignore
    /// let drag = PopupDrag::begin((120.0, 90.0), (100.0, 80.0), (360.0, 200.0), screen);
    /// ```
    pub fn begin(
        cursor: (f32, f32),
        origin: (f32, f32),
        size: (f32, f32),
        screen: (f32, f32, f32, f32),
    ) -> Self {
        Self {
            grab: (cursor.0 - origin.0, cursor.1 - origin.1),
            size,
            screen,
        }
    }

    /// 光标移到 `cursor` 时弹窗该在的左上角（保持抓取偏移，并限制在本屏内）。
    ///
    /// # 参数
    /// - `cursor`：当前光标（画布逻辑坐标）。
    ///
    /// # 返回
    /// 弹窗左上角。
    ///
    /// ```ignore
    /// let origin = drag.origin_at((300.0, 200.0));
    /// ```
    pub fn origin_at(&self, cursor: (f32, f32)) -> (f32, f32) {
        clamp_popup_origin(
            (cursor.0 - self.grab.0, cursor.1 - self.grab.1),
            self.size,
            self.screen,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 一块 1920x1080 的屏。
    const SCREEN: (f32, f32, f32, f32) = (0.0, 0.0, 1920.0, 1080.0);

    /// 内容越多越高，但不超过上限与屏高；宽度不超过屏宽。
    #[test]
    fn size_is_bounded() {
        let one = popup_size(&["a".to_string()], (1920.0, 1080.0));
        assert_eq!(one.0, POPUP_WIDTH);
        assert!(one.1 < POPUP_MAX_HEIGHT);
        let many: Vec<String> = (0..100).map(|i| format!("line {i}")).collect();
        assert_eq!(popup_size(&many, (1920.0, 1080.0)).1, POPUP_MAX_HEIGHT);
        let tiny = popup_size(&many, (200.0, 100.0));
        assert!(tiny.0 <= 200.0 - 2.0 * POPUP_GAP && tiny.1 <= 100.0 - 2.0 * POPUP_GAP);
        assert!(popup_size(&[], (4.0, 4.0)).0 >= 1.0);
    }

    /// 长文本折行后更高，全角比半角占得多。
    #[test]
    fn long_rows_wrap() {
        let short = estimate_body_height(&["短".to_string()], POPUP_WIDTH);
        let long = estimate_body_height(&["长".repeat(120)], POPUP_WIDTH);
        assert!(long > short);
        let ascii = estimate_body_height(&["a".repeat(120)], POPUP_WIDTH);
        assert!(long > ascii, "全角更宽，折行更多");
        assert_eq!(estimate_body_height(&[], POPUP_WIDTH), BODY_PADDING_Y);
    }

    /// 夹取：右下越界拉回，左上越界拉回；放不下时左上优先。
    #[test]
    fn clamp_keeps_popup_on_screen() {
        let size = (360.0, 200.0);
        assert_eq!(
            clamp_popup_origin((5000.0, 5000.0), size, SCREEN),
            (1560.0, 880.0)
        );
        assert_eq!(clamp_popup_origin((-30.0, -9.0), size, SCREEN), (0.0, 0.0));
        assert_eq!(
            clamp_popup_origin((100.0, 100.0), size, SCREEN),
            (100.0, 100.0)
        );
        // 副屏：原点不在 (0,0)
        let second = (1920.0, 0.0, 1280.0, 720.0);
        assert_eq!(clamp_popup_origin((0.0, 0.0), size, second), (1920.0, 0.0));
        // 比屏幕还大：左上角优先
        assert_eq!(
            clamp_popup_origin((50.0, 50.0), (3000.0, 2000.0), SCREEN),
            (0.0, 0.0)
        );
    }

    /// 自动位置：选区右侧放得下放右侧，贴右边缘时放左侧，铺满整屏时落到选区内侧。
    #[test]
    fn auto_origin_prefers_outside() {
        let size = (360.0, 200.0);
        let sel = PhysicalRect::new(100, 100, 400, 300);
        let (x, _) = auto_origin(Some(sel), size, SCREEN, &[]);
        assert_eq!(x, (sel.right() + POPUP_GAP as i32) as f32);
        let right_edge = PhysicalRect::new(1500, 100, 420, 300);
        let (x, _) = auto_origin(Some(right_edge), size, SCREEN, &[]);
        assert!(x + size.0 <= right_edge.x as f32, "放到选区左侧");
        let full = PhysicalRect::new(0, 0, 1920, 1080);
        let (x, y) = auto_origin(Some(full), size, SCREEN, &[]);
        assert_eq!((x, y), (POPUP_GAP, POPUP_GAP), "落到选区内侧");
        let toolbar = PhysicalRect::new(sel.right() + 4, 100, 300, 40);
        let (x, y) = auto_origin(Some(sel), size, SCREEN, &[toolbar]);
        assert!(
            PhysicalRect::new(x as i32, y as i32, 360, 200)
                .intersect(&toolbar)
                .is_none(),
            "不压工具栏"
        );
    }

    /// 拖动位置优先于自动位置，并被夹回屏内。
    #[test]
    fn manual_origin_wins_and_is_clamped() {
        let size = (360.0, 200.0);
        assert_eq!(
            resolve_origin((10.0, 10.0), None, size, SCREEN),
            (10.0, 10.0)
        );
        assert_eq!(
            resolve_origin((10.0, 10.0), Some((300.0, 400.0)), size, SCREEN),
            (300.0, 400.0)
        );
        assert_eq!(
            resolve_origin((10.0, 10.0), Some((9999.0, -5.0)), size, SCREEN),
            (1560.0, 0.0)
        );
    }

    /// 拖动：弹窗跟着光标平移（保持抓取偏移），拖出屏幕被夹住，副屏同样有效。
    #[test]
    fn drag_follows_cursor_and_clamps() {
        let size = (360.0, 200.0);
        let drag = PopupDrag::begin((150.0, 110.0), (100.0, 100.0), size, SCREEN);
        assert_eq!(drag.origin_at((150.0, 110.0)), (100.0, 100.0));
        assert_eq!(drag.origin_at((250.0, 210.0)), (200.0, 200.0));
        assert_eq!(drag.origin_at((-500.0, -500.0)), (0.0, 0.0));
        assert_eq!(drag.origin_at((9000.0, 9000.0)), (1560.0, 880.0));
        let second = (1920.0, 0.0, 1280.0, 720.0);
        let drag = PopupDrag::begin((2020.0, 30.0), (2000.0, 20.0), size, second);
        assert_eq!(drag.origin_at((100.0, 30.0)), (1920.0, 20.0));
        assert_eq!(drag.origin_at((9999.0, 9999.0)), (2840.0, 520.0));
    }
}
