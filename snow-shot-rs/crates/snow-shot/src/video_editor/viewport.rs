//! 时间轴视口：缩放 / 平移 / 像素与毫秒换算 / 缩略图瓦片时间点 / 关键帧提示。
//!
//! 不依赖 GPUI，全部可离屏单测。内容宽度 = 轨道宽度 x 缩放倍数；`scroll_px` 是内容左端被卷出
//! 轨道左边的像素数。

use crate::video_editor::segments::{Edge, EditList};

/// 最大缩放下每毫秒的内容像素数（0.5 即 500 像素 / 秒，30fps 下约 16 像素一帧）。
const MAX_PX_PER_MS: f32 = 0.5;
/// 缩放级别量化步长：每个八度（缩放翻倍）分几级。
const LEVELS_PER_OCTAVE: f32 = 2.0;
/// 关键帧提示的容差（毫秒）：离最近关键帧不超过它视为“已在关键帧上”。
pub const KEYFRAME_TOLERANCE_MS: u64 = 20;

/// 时间轴视口。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Viewport {
    /// 视频总时长（毫秒）。
    duration_ms: u64,
    /// 轨道可见宽度（像素）。
    track_px: f32,
    /// 缩放倍数（1 = 整段视频恰好铺满轨道）。
    zoom: f32,
    /// 内容左端卷出的像素。
    scroll_px: f32,
}

impl Viewport {
    /// 创建视口：整段铺满轨道。
    ///
    /// # 参数
    /// - `duration_ms`：视频总时长。
    /// - `track_px`：轨道可见宽度（像素）。
    pub fn new(duration_ms: u64, track_px: f32) -> Self {
        Self {
            duration_ms,
            track_px: track_px.max(1.0),
            zoom: 1.0,
            scroll_px: 0.0,
        }
    }

    /// 当前缩放倍数。
    #[cfg(test)]
    pub fn zoom(&self) -> f32 {
        self.zoom
    }

    /// 内容左端卷出的像素。
    #[cfg(test)]
    pub fn scroll_px(&self) -> f32 {
        self.scroll_px
    }

    /// 轨道可见宽度。
    pub fn track_px(&self) -> f32 {
        self.track_px
    }

    /// 内容总宽度（像素）。
    pub fn content_px(&self) -> f32 {
        self.track_px * self.zoom
    }

    /// 允许的最大缩放倍数（至少为 1）。
    pub fn max_zoom(&self) -> f32 {
        (self.duration_ms as f32 * MAX_PX_PER_MS / self.track_px).max(1.0)
    }

    /// 夹取卷动量到 `[0, 内容宽 - 轨道宽]`。
    fn clamp_scroll(&mut self) {
        let max = (self.content_px() - self.track_px).max(0.0);
        self.scroll_px = self.scroll_px.clamp(0.0, max);
    }

    /// 轨道宽度变化（窗口缩放）：保持左端对应的时间不变。
    pub fn resize(&mut self, track_px: f32) {
        let left = self.x_to_ms(0.0);
        self.track_px = track_px.max(1.0);
        self.zoom = self.zoom.clamp(1.0, self.max_zoom());
        self.scroll_px = self.ms_to_content_px(left);
        self.clamp_scroll();
    }

    /// 以轨道内 `cx` 像素处为锚点缩放：`scroll' = (scroll + cx) * (z1 / z0) - cx`。
    ///
    /// # 参数
    /// - `factor`：缩放倍率（>1 放大，<1 缩小）。
    /// - `cx`：锚点相对轨道左端的像素（鼠标位置）。
    pub fn zoom_at(&mut self, factor: f32, cx: f32) {
        let z0 = self.zoom;
        let z1 = (z0 * factor).clamp(1.0, self.max_zoom());
        if (z1 - z0).abs() < f32::EPSILON {
            return;
        }
        self.scroll_px = (self.scroll_px + cx) * (z1 / z0) - cx;
        self.zoom = z1;
        self.clamp_scroll();
    }

    /// 水平平移 `dx` 像素（正值向右看更晚的时间）。
    pub fn pan(&mut self, dx: f32) {
        self.scroll_px += dx;
        self.clamp_scroll();
    }

    /// 让 `ms` 出现在轨道内（播放时跟随播放头）：不可见则平移到它位于轨道中央。
    pub fn reveal(&mut self, ms: u64) {
        let x = self.ms_to_x(ms);
        if x < 0.0 || x > self.track_px {
            self.scroll_px = self.ms_to_content_px(ms) - self.track_px / 2.0;
            self.clamp_scroll();
        }
    }

    /// 毫秒换算成内容坐标（不含卷动）。
    fn ms_to_content_px(&self, ms: u64) -> f32 {
        if self.duration_ms == 0 {
            return 0.0;
        }
        (ms as f64 / self.duration_ms as f64) as f32 * self.content_px()
    }

    /// 毫秒换算成轨道内像素（可为负或超出轨道）。
    pub fn ms_to_x(&self, ms: u64) -> f32 {
        self.ms_to_content_px(ms) - self.scroll_px
    }

    /// 轨道内像素换算成毫秒（四舍五入，夹在 `[0, 总时长]`）。
    pub fn x_to_ms(&self, x: f32) -> u64 {
        let content = self.content_px();
        if content <= 0.0 {
            return 0;
        }
        let ratio = ((x + self.scroll_px) / content).clamp(0.0, 1.0);
        ((ratio as f64) * self.duration_ms as f64).round() as u64
    }

    /// 当前可见的时间范围 `(start, end)`（毫秒）。
    pub fn visible_range(&self) -> (u64, u64) {
        (self.x_to_ms(0.0), self.x_to_ms(self.track_px))
    }

    /// 缩略图缩放级别（量化后的整数，0 = 整段一屏）。
    pub fn level(&self) -> u32 {
        (self.zoom.log2() * LEVELS_PER_OCTAVE).round().max(0.0) as u32
    }

    /// 某级别下整条时间轴被切成的瓦片数（每片宽约 `tile_px`）。
    ///
    /// # 参数
    /// - `level`：缩放级别。
    /// - `tile_px`：目标瓦片宽度（像素）。
    pub fn tile_count(&self, level: u32, tile_px: f32) -> u64 {
        let zoom_q = 2f32.powf(level as f32 / LEVELS_PER_OCTAVE);
        ((self.track_px * zoom_q / tile_px.max(1.0)).ceil() as u64).max(1)
    }

    /// 第 `index` 片瓦片覆盖的时间 `[start, end)`（毫秒）。
    pub fn tile_span(&self, level: u32, tile_px: f32, index: u64) -> (u64, u64) {
        let n = self.tile_count(level, tile_px);
        let at = |i: u64| (self.duration_ms as u128 * i as u128 / n as u128) as u64;
        (at(index), at(index + 1))
    }

    /// 可见（含左右各一片余量）的瓦片下标范围 `[first, last]`。
    ///
    /// # 返回
    /// 时长为 0 时返回 `None`。
    pub fn visible_tiles(&self, level: u32, tile_px: f32) -> Option<(u64, u64)> {
        if self.duration_ms == 0 {
            return None;
        }
        let n = self.tile_count(level, tile_px);
        let (s, e) = self.visible_range();
        let idx = |ms: u64| (ms as u128 * n as u128 / self.duration_ms as u128) as u64;
        Some((idx(s).saturating_sub(1), (idx(e) + 1).min(n - 1)))
    }

    /// 需要请求的缩略图：可见瓦片中 `missing` 判定为缺失的，按离视口中心由近及远排序。
    ///
    /// # 参数
    /// - `level`：缩放级别。
    /// - `tile_px`：目标瓦片宽度。
    /// - `missing`：`(级别, 下标)` 是否尚未缓存。
    ///
    /// # 返回
    /// `(瓦片下标, 取帧时间)` 列表；取帧时间为瓦片中点。
    pub fn thumb_requests(
        &self,
        level: u32,
        tile_px: f32,
        missing: impl Fn(u32, u64) -> bool,
    ) -> Vec<(u64, u64)> {
        let Some((first, last)) = self.visible_tiles(level, tile_px) else {
            return Vec::new();
        };
        let (s, e) = self.visible_range();
        let center = (s + e) / 2;
        let mut out: Vec<(u64, u64)> = (first..=last)
            .filter(|&i| missing(level, i))
            .map(|i| {
                let (a, b) = self.tile_span(level, tile_px, i);
                (i, (a + b) / 2)
            })
            .collect();
        out.sort_by_key(|&(_, t)| t.abs_diff(center));
        out
    }
}

/// 某时间点是否不在关键帧上（导出时会对齐到关键帧，界面据此显示提示）。
///
/// # 参数
/// - `keyframes`：升序关键帧时间（毫秒）；为空表示未知，返回 `false`。
/// - `ms`：入点 / 出点位置。
///
/// # 返回
/// 离最近关键帧超过容差时为 `true`。
///
/// # 示例
/// ```ignore
/// assert!(!off_keyframe(&[0, 2000], 2010));
/// assert!(off_keyframe(&[0, 2000], 1500));
/// ```
pub fn off_keyframe(keyframes: &[u64], ms: u64) -> bool {
    if keyframes.is_empty() {
        return false;
    }
    let at = keyframes.partition_point(|&k| k < ms);
    let after = keyframes.get(at).map(|&k| k.abs_diff(ms));
    let before = at.checked_sub(1).map(|i| ms.abs_diff(keyframes[i]));
    let nearest = match (before, after) {
        (Some(a), Some(b)) => a.min(b),
        (Some(a), None) | (None, Some(a)) => a,
        (None, None) => return false,
    };
    nearest > KEYFRAME_TOLERANCE_MS
}

/// 命中判断：取离 `x` 最近且在容差内的片段边；相接的两段共用一条边时只返回前一段的终点边。
///
/// # 参数
/// - `list`：剪辑列表。
/// - `view`：时间轴视口。
/// - `x`：相对轨道左端的像素。
/// - `tolerance`：命中容差（像素）。
///
/// # 返回
/// 命中的边；都不在容差内返回 `None`。
pub fn hit_edge(list: &EditList, view: &Viewport, x: f32, tolerance: f32) -> Option<Edge> {
    let segs = list.segments();
    let mut best: Option<(f32, Edge)> = None;
    let mut consider = |edge: Edge, ms: u64| {
        let dist = (view.ms_to_x(ms) - x).abs();
        if dist <= tolerance && best.is_none_or(|(d, _)| dist < d) {
            best = Some((dist, edge));
        }
    };
    for (i, seg) in segs.iter().enumerate() {
        let touching = i > 0 && segs[i - 1].end_ms == seg.start_ms;
        if !touching {
            consider(Edge::Start(i), seg.start_ms);
        }
        consider(Edge::End(i), seg.end_ms);
    }
    best.map(|(_, e)| e)
}

/// 是否有剪辑切点（片段边，不含视频首尾）不在关键帧上。
///
/// # 参数
/// - `list`：剪辑列表。
/// - `keyframes`：升序关键帧时间；为空表示未知，返回 `false`。
pub fn any_cut_off_keyframe(list: &EditList, keyframes: &[u64]) -> bool {
    let duration = list.duration_ms();
    list.segments()
        .iter()
        .flat_map(|s| [s.start_ms, s.end_ms])
        .filter(|&ms| ms > 0 && ms < duration)
        .any(|ms| off_keyframe(keyframes, ms))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 像素与毫秒互换，越界被夹住。
    #[test]
    fn pixel_ms_roundtrip() {
        let v = Viewport::new(2_000, 100.0);
        assert_eq!(v.x_to_ms(50.0), 1_000);
        assert_eq!(v.x_to_ms(-10.0), 0);
        assert_eq!(v.x_to_ms(500.0), 2_000);
        assert_eq!(v.ms_to_x(1_000), 50.0);
        assert_eq!(Viewport::new(0, 100.0).ms_to_x(10), 0.0);
    }

    /// 锚点缩放：锚点下的时间保持不变，卷动量被夹住。
    #[test]
    fn zoom_keeps_anchor() {
        let mut v = Viewport::new(60_000, 800.0);
        let cx = 300.0;
        let before = v.x_to_ms(cx);
        v.zoom_at(2.0, cx);
        assert!((v.zoom() - 2.0).abs() < 1e-4);
        assert!(v.x_to_ms(cx).abs_diff(before) <= 20);
        v.zoom_at(2.0, 700.0);
        let t = v.x_to_ms(700.0);
        v.zoom_at(0.5, 700.0);
        assert!(v.x_to_ms(700.0).abs_diff(t) <= 60);
        // 缩回 1 倍后卷动为 0
        v.zoom_at(0.01, 100.0);
        assert_eq!(v.zoom(), 1.0);
        assert_eq!(v.scroll_px(), 0.0);
    }

    /// 缩放公式与文档一致，且上限受每毫秒像素数约束。
    #[test]
    fn zoom_formula_and_limit() {
        let mut v = Viewport::new(100_000, 1_000.0);
        v.zoom_at(2.0, 0.0);
        v.pan(500.0);
        let (s0, z0, cx) = (v.scroll_px(), v.zoom(), 250.0);
        v.zoom_at(1.5, cx);
        let want = (s0 + cx) * (v.zoom() / z0) - cx;
        assert!((v.scroll_px() - want).abs() < 1e-2);
        // 短视频上限接近 1
        let short = Viewport::new(1_000, 1_000.0);
        assert_eq!(short.max_zoom(), 1.0);
        let mut v = Viewport::new(10_000, 1_000.0);
        v.zoom_at(1e6, 0.0);
        assert!((v.zoom() - 5.0).abs() < 1e-4);
    }

    /// 平移夹在内容范围内；`reveal` 把不可见的位置拉进轨道。
    #[test]
    fn pan_and_reveal() {
        let mut v = Viewport::new(10_000, 100.0);
        v.zoom_at(4.0, 0.0);
        v.pan(-50.0);
        assert_eq!(v.scroll_px(), 0.0);
        v.pan(1e6);
        assert_eq!(v.scroll_px(), 300.0);
        v.reveal(1_000);
        let x = v.ms_to_x(1_000);
        assert!((0.0..=100.0).contains(&x));
        let (s, e) = v.visible_range();
        assert!(s <= 1_000 && 1_000 <= e);
    }

    /// 窗口变宽时左端时间保持。
    #[test]
    fn resize_keeps_left_time() {
        let mut v = Viewport::new(30_000, 400.0);
        v.zoom_at(3.0, 0.0);
        v.pan(300.0);
        let left = v.x_to_ms(0.0);
        v.resize(600.0);
        assert!(v.x_to_ms(0.0).abs_diff(left) <= 30);
    }

    /// 缩略图请求：只含可见瓦片、跳过已缓存、由中心向外排序、时间是瓦片中点。
    #[test]
    fn thumb_requests_visible_only() {
        let mut v = Viewport::new(100_000, 400.0);
        let level0 = v.level();
        assert_eq!(level0, 0);
        let all = v.thumb_requests(0, 100.0, |_, _| true);
        assert_eq!(all.len(), 4);
        assert!(all[0].0 == 1 || all[0].0 == 2, "中心附近优先");
        assert!(
            all.iter()
                .all(|&(i, t)| t == (i * 25_000 + (i + 1) * 25_000) / 2)
        );
        // 已缓存的不再请求
        let some = v.thumb_requests(0, 100.0, |_, i| i != 2);
        assert_eq!(some.len(), 3);
        // 放大 8 倍后只请求可见窗口附近的瓦片
        v.zoom_at(8.0, 0.0);
        let lv = v.level();
        assert_eq!(lv, 6);
        let n = v.tile_count(lv, 100.0);
        assert_eq!(n, 32);
        let reqs = v.thumb_requests(lv, 100.0, |_, _| true);
        assert!(reqs.len() <= 6, "{}", reqs.len());
        let (s, e) = v.visible_range();
        assert!(
            reqs.iter()
                .all(|&(_, t)| t + 25_000 >= s && t <= e + 25_000)
        );
        // 瓦片区间首尾相接且覆盖整段
        assert_eq!(v.tile_span(lv, 100.0, 0).0, 0);
        assert_eq!(v.tile_span(lv, 100.0, n - 1).1, 100_000);
        assert_eq!(v.tile_span(lv, 100.0, 3).1, v.tile_span(lv, 100.0, 4).0);
    }

    /// 关键帧提示：离关键帧超过容差才提示，空列表不提示。
    #[test]
    fn keyframe_note() {
        let keys = [0, 2_000, 4_000];
        assert!(!off_keyframe(&keys, 0));
        assert!(!off_keyframe(&keys, 2_015));
        assert!(!off_keyframe(&keys, 1_985));
        assert!(off_keyframe(&keys, 1_500));
        assert!(off_keyframe(&keys, 9_000));
        assert!(!off_keyframe(&[], 1_500));
    }

    /// 边命中：最近且在容差内；相接边只返回前一段终点。
    #[test]
    fn edge_hit_testing() {
        let v = Viewport::new(1_000, 1_000.0);
        let mut l = EditList::new(1_000);
        l.split_at(400);
        l.split_at(700);
        l.delete(1); // [0,400) [700,1000)
        assert_eq!(hit_edge(&l, &v, 3.0, 8.0), Some(Edge::Start(0)));
        assert_eq!(hit_edge(&l, &v, 396.0, 8.0), Some(Edge::End(0)));
        assert_eq!(hit_edge(&l, &v, 703.0, 8.0), Some(Edge::Start(1)));
        assert_eq!(hit_edge(&l, &v, 550.0, 8.0), None);
        let mut t = EditList::new(1_000);
        t.split_at(500);
        assert_eq!(hit_edge(&t, &v, 500.0, 8.0), Some(Edge::End(0)));
    }

    /// 切点提示：只看非首尾的切点是否离关键帧过远。
    #[test]
    fn cut_keyframe_note() {
        let keys = [0, 2_000, 4_000];
        let mut l = EditList::new(6_000);
        assert!(!any_cut_off_keyframe(&l, &keys));
        l.set_in(2_000);
        assert!(!any_cut_off_keyframe(&l, &keys));
        l.set_out(5_000);
        assert!(any_cut_off_keyframe(&l, &keys));
        assert!(!any_cut_off_keyframe(&l, &[]));
    }
}
