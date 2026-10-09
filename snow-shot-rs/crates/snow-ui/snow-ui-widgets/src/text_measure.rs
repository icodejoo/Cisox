//! 下拉菜单宽度的自适应：用 GPUI 文本系统量出每个文案的实际宽度，取最长一项并缓存。
//!
//! 纯逻辑（取最大值、缓存键、宽度公式）可离屏测试；量字本身经 shell 门面完成。

use crate::toolbar_groups::{BAR_BORDER, CHECK_ICON_SIZE, MENU_PADDING};
use snow_ui_shell::ui::{Window, measure_text_width, window_font_family};
use std::cell::RefCell;
use std::collections::HashMap;
use std::hash::{Hash, Hasher};

/// 菜单与按钮里文字的字号（逻辑像素）。
pub const LABEL_FONT_PX: f32 = 14.0;
/// 每个文案宽度的余量，吸收窗口默认字体与组件主题字体之间的细微差别。
pub const TEXT_SLACK: f32 = 4.0;
/// 行左右内边距（对应 `px_2`）。
pub const ROW_PADDING_X: f32 = 8.0;
/// 行内图标边长。
pub const ROW_ICON: f32 = 16.0;
/// 行内元素间距（对应 `gap_2`）。
pub const ROW_GAP: f32 = 8.0;
/// 按钮上小箭头边长。
pub const ARROW_ICON_PX: f32 = 12.0;
/// 按钮外框内边距。
pub const FRAME_PADDING: f32 = 2.0;
/// 缓存条目上限，超过就整体清空，避免语言反复切换时无限增长。
const CACHE_LIMIT: usize = 64;

/// 取一组宽度里的最大值；空集为 0。
///
/// # 示例
/// ```rust
/// assert_eq!(snow_ui_widgets::max_width([3.0, 9.5, 7.0]), 9.5);
/// assert_eq!(snow_ui_widgets::max_width([]), 0.0);
/// ```
pub fn max_width(widths: impl IntoIterator<Item = f32>) -> f32 {
    widths.into_iter().fold(0.0, f32::max)
}

/// 宽度缓存键：文案集合、字号与字体族任一变化都会得到不同的键（即缓存失效）。
pub fn cache_key(labels: &[String], font_px: f32, family: &str) -> u64 {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    labels.hash(&mut hasher);
    font_px.to_bits().hash(&mut hasher);
    family.hash(&mut hasher);
    hasher.finish()
}

/// 带容量上限的宽度缓存（纯逻辑）。
#[derive(Debug, Default)]
pub struct WidthCache {
    /// 键到宽度的映射。
    entries: HashMap<u64, f32>,
}

impl WidthCache {
    /// 取缓存值，没有就用 `measure` 量一次并记下。
    pub fn get_or_measure(&mut self, key: u64, measure: impl FnOnce() -> f32) -> f32 {
        if let Some(width) = self.entries.get(&key) {
            return *width;
        }
        if self.entries.len() >= CACHE_LIMIT {
            self.entries.clear();
        }
        let width = measure();
        self.entries.insert(key, width);
        width
    }

    /// 当前条目数（测试用）。
    #[cfg(test)]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// 是否为空（测试用）。
    #[cfg(test)]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

thread_local! {
    /// 界面线程内共用的宽度缓存。
    static CACHE: RefCell<WidthCache> = RefCell::new(WidthCache::default());
}

/// 量出一组文案里最长一项的宽度（含余量），按「文案 + 字号 + 字体族」缓存。
///
/// # 参数
/// - `window`：当前窗口。
/// - `labels`：全部候选文案（当前语言）。
/// - `font_px`：字号。
pub fn max_label_width(window: &Window, labels: &[String], font_px: f32) -> f32 {
    let key = cache_key(labels, font_px, &window_font_family(window));
    CACHE.with(|cache| {
        cache.borrow_mut().get_or_measure(key, || {
            max_width(
                labels
                    .iter()
                    .map(|l| measure_text_width(window, l, font_px) + TEXT_SLACK),
            )
        })
    })
}

/// 下拉菜单（整体含边框与内边距）的外宽：图标 + 文字 + 勾位 + 行内边距。
///
/// # 参数
/// - `max_label`：最长文案宽度。
pub fn menu_outer_width(max_label: f32) -> f32 {
    2.0 * (MENU_PADDING + BAR_BORDER)
        + 2.0 * ROW_PADDING_X
        + ROW_ICON
        + ROW_GAP
        + max_label
        + ROW_GAP
        + CHECK_ICON_SIZE
}

/// 菜单按钮（含外框）的外宽：取「按钮自身所需」与「菜单所需」的较大者，保证菜单不比按钮宽。
///
/// # 参数
/// - `max_label`：最长文案宽度。
pub fn menu_button_outer_width(max_label: f32) -> f32 {
    let button = 2.0 * (FRAME_PADDING + BAR_BORDER)
        + 2.0 * ROW_PADDING_X
        + ROW_ICON
        + ROW_GAP
        + max_label
        + ROW_GAP
        + ARROW_ICON_PX;
    button.max(menu_outer_width(max_label))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 取最大值，空集为 0。
    #[test]
    fn max_of_widths() {
        assert_eq!(max_width([1.0, 4.0, 2.5]), 4.0);
        assert_eq!(max_width(std::iter::empty()), 0.0);
    }

    /// 缓存命中不再量；语言（文案）、字号、字体族变化都使缓存失效。
    #[test]
    fn cache_hits_and_invalidates() {
        let zh = vec!["矩形区域".to_string(), "折线区域".to_string()];
        let en = vec!["Rectangle region".to_string()];
        let mut cache = WidthCache::default();
        let mut calls = 0;
        let key = cache_key(&zh, 14.0, "A");
        for _ in 0..3 {
            let w = cache.get_or_measure(key, || {
                calls += 1;
                50.0
            });
            assert_eq!(w, 50.0);
        }
        assert_eq!(calls, 1);
        assert_ne!(key, cache_key(&en, 14.0, "A"));
        assert_ne!(key, cache_key(&zh, 15.0, "A"));
        assert_ne!(key, cache_key(&zh, 14.0, "B"));
        assert_eq!(key, cache_key(&zh, 14.0, "A"));
    }

    /// 缓存超过上限会整体清空。
    #[test]
    fn cache_is_bounded() {
        let mut cache = WidthCache::default();
        for i in 0..(CACHE_LIMIT as u64 + 5) {
            cache.get_or_measure(i, || 1.0);
        }
        assert!(cache.len() <= CACHE_LIMIT);
        assert!(!cache.is_empty());
    }

    /// 宽度随文案线性增长，且菜单不比按钮宽。
    #[test]
    fn widths_follow_label_and_menu_not_wider() {
        let (a, b) = (
            menu_button_outer_width(60.0),
            menu_button_outer_width(100.0),
        );
        assert_eq!(b - a, 40.0);
        assert!(menu_outer_width(100.0) <= menu_button_outer_width(100.0));
    }
}
