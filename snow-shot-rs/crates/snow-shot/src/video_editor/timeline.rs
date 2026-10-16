//! 时间轴：缩略图条、已删区间、片段边把手、关键帧刻度、播放头的自绘，以及鼠标 / 滚轮交互。
//!
//! 这是编辑窗里唯一手写的 UI。坐标约定：时间轴容器左端距窗口 `PAD`，交互里的 x 都是
//! 相对轨道左端的像素。

use crate::video_editor::viewport::{Viewport, hit_edge};
use crate::video_editor::{Drag, PAD, THUMB_H, VideoEditorView};
use snow_ui::ui::*;
use std::sync::Arc;

/// 播放头擦洗条高度。
const SCRUB_H: f32 = 20.0;
/// 擦洗条与缩略图条的间隔。
const GAP: f32 = 4.0;
/// 关键帧刻度行高度。
const KEY_ROW_H: f32 = 6.0;
/// 缩略图条与刻度行的间隔。
const KEY_GAP: f32 = 2.0;
/// 时间轴总高度。
const TIMELINE_H: f32 = SCRUB_H + GAP + THUMB_H + KEY_GAP + KEY_ROW_H;
/// 片段边把手宽度。
const EDGE_W: f32 = 6.0;
/// 片段边命中容差（像素）。
const EDGE_HIT: f32 = 8.0;
/// 播放头线宽。
const PLAYHEAD_W: f32 = 2.0;
/// 相邻关键帧刻度的最小像素间距（更密就跳过，控制元素数量）。
const KEY_MIN_GAP: f32 = 3.0;
/// 滚轮一格对应的缩放倍率。
const ZOOM_PER_LINE: f32 = 1.2;
/// 像素型滚轮增量换算成“行”的除数。
const PIXELS_PER_LINE: f32 = 50.0;
/// 平移时一行对应的像素。
const PAN_PX_PER_LINE: f32 = 60.0;
/// 轨道底色。
const COLOR_TRACK: u32 = 0x2E2E2EFF;
/// 缩略图占位色。
const COLOR_TILE: u32 = 0x3A3A3AFF;
/// 已删区间遮罩色。
const COLOR_GAP: u32 = 0x101010D8;
/// 选中片段描边色。
const COLOR_SELECTED: u32 = 0x1668DCFF;
/// 片段边把手色。
const COLOR_EDGE: u32 = 0xE8E8E8FF;
/// 相接片段分割线色。
const COLOR_SPLIT: u32 = 0xFFFFFFB0;
/// 关键帧刻度色。
const COLOR_KEY: u32 = 0x6F8FBFFF;
/// 播放头色。
const COLOR_PLAYHEAD: u32 = 0xFFB020FF;

/// 一块要绘制的缩略图瓦片。
struct Tile {
    /// 左端 x。
    x: f32,
    /// 宽度。
    w: f32,
    /// 图像（缺失时画占位色）。
    image: Option<Arc<RenderImage>>,
}

impl VideoEditorView {
    /// 拖动中：把鼠标横坐标应用到当前被拖动的对象。
    ///
    /// # 参数
    /// - `x`：相对轨道左端的像素。
    /// - `cx`：视图上下文。
    pub(crate) fn drag_to(&mut self, x: f32, cx: &mut Context<Self>) {
        let (Some(drag), Some(view)) = (self.drag, self.viewport) else {
            return;
        };
        let ms = view.x_to_ms(x);
        match drag {
            Drag::Playhead => self.seek_to(ms),
            Drag::Edge(edge) => {
                let Some(actual) = self.list.as_mut().and_then(|l| l.move_edge(edge, ms)) else {
                    return;
                };
                self.push_list();
                self.seek_to(actual);
            }
        }
        self.ensure_ticking(cx);
        cx.notify();
    }

    /// 窗口级鼠标移动：拖动中跟随；按键已不在按下态（窗口外松开漏掉 up）则结束拖动。
    ///
    /// # 参数
    /// - `root_x`：窗口内横坐标。
    /// - `pressed`：此刻是否按着左键。
    /// - `cx`：视图上下文。
    pub(crate) fn on_window_move(&mut self, root_x: f32, pressed: bool, cx: &mut Context<Self>) {
        self.drag = drag_after_move(self.drag, pressed);
        self.drag_to(root_x - PAD, cx);
    }

    /// 窗口级左键松开：结束拖动（播放头保持在松开处，播放状态保持暂停）。
    pub(crate) fn on_window_up(&mut self, cx: &mut Context<Self>) {
        if self.drag.take().is_some() {
            cx.notify();
        }
    }

    /// 擦洗条按下：先暂停（否则播放回写会盖掉拖动位置），再开始拖动播放头并跳到点击处。
    fn press_scrub(&mut self, x: f32, cx: &mut Context<Self>) {
        self.pause();
        self.drag = Some(Drag::Playhead);
        self.drag_to(x, cx);
    }

    /// 缩略图条按下：命中片段边则开始拖边；点保留片段则选中并跳过去；点灰色区间则恢复。
    fn press_strip(&mut self, x: f32, cx: &mut Context<Self>) {
        let (Some(list), Some(view)) = (self.list.as_ref(), self.viewport) else {
            return;
        };
        if let Some(edge) = hit_edge(list, &view, x, EDGE_HIT) {
            self.drag = Some(Drag::Edge(edge));
            return;
        }
        let ms = view.x_to_ms(x);
        if let Some(idx) = list.segment_at(ms) {
            self.selected = Some(idx);
            self.seek_to(ms);
        } else {
            self.restore_at(ms, cx);
        }
        self.ensure_ticking(cx);
        cx.notify();
    }

    /// 滚轮：默认以鼠标位置为锚点缩放；按住 Shift（或横向滚动）平移。
    fn on_wheel(&mut self, ev: &ScrollWheelEvent, cx: &mut Context<Self>) {
        let Some(view) = self.viewport.as_mut() else {
            return;
        };
        let (dx, dy) = match ev.delta {
            ScrollDelta::Lines(p) => (p.x, p.y),
            ScrollDelta::Pixels(p) => (
                p.x.as_f32() / PIXELS_PER_LINE,
                p.y.as_f32() / PIXELS_PER_LINE,
            ),
        };
        if ev.modifiers.shift || dx.abs() > dy.abs() {
            let lines = if dx.abs() > dy.abs() { dx } else { dy };
            view.pan(-lines * PAN_PX_PER_LINE);
        } else {
            let local = f32::from(ev.position.x) - PAD;
            view.zoom_at(ZOOM_PER_LINE.powf(dy), local);
        }
        self.schedule_thumbs(cx);
        cx.notify();
    }

    /// 收集当前可见的缩略图瓦片（缺失的用邻近时间的替代图）。
    fn visible_tiles(&mut self, view: &Viewport) -> Vec<Tile> {
        let level = view.level();
        let Some((first, last)) = view.visible_tiles(level, self.tile_w) else {
            return Vec::new();
        };
        (first..=last)
            .map(|i| {
                let (a, b) = view.tile_span(level, self.tile_w, i);
                let x = view.ms_to_x(a);
                let w = (view.ms_to_x(b) - x).max(1.0);
                let image = self
                    .thumbs
                    .get((level, i))
                    .or_else(|| self.thumbs.nearest((a + b) / 2));
                Tile { x, w, image }
            })
            .collect()
    }

    /// 渲染时间轴。
    pub(crate) fn render_timeline(&mut self, cx: &mut Context<Self>) -> Div {
        let (Some(view), Some(list)) = (self.viewport, self.list.clone()) else {
            return div();
        };
        let track = view.track_px();
        let strip_top = SCRUB_H + GAP;
        let tiles = self.visible_tiles(&view);

        // 缩略图条：底色 + 瓦片
        let mut strip = div()
            .absolute()
            .top(px(strip_top))
            .left(px(0.0))
            .w(px(track))
            .h(px(THUMB_H))
            .overflow_hidden()
            .bg(rgba(COLOR_TRACK))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, ev: &MouseDownEvent, _w, cx| {
                    this.press_strip(f32::from(ev.position.x) - PAD, cx);
                }),
            );
        for tile in tiles {
            let cell = div()
                .absolute()
                .top(px(0.0))
                .left(px(tile.x))
                .w(px(tile.w))
                .h(px(THUMB_H))
                .overflow_hidden()
                .bg(rgba(COLOR_TILE));
            strip = strip.child(match tile.image {
                Some(image) => cell.child(
                    img(ImageSource::Render(image))
                        .size_full()
                        .object_fit(ObjectFit::Cover),
                ),
                None => cell,
            });
        }

        // 已删区间遮罩（含首尾）
        let mut prev_end = 0u64;
        let mut gaps: Vec<(u64, u64)> = Vec::new();
        for seg in list.segments() {
            if seg.start_ms > prev_end {
                gaps.push((prev_end, seg.start_ms));
            }
            prev_end = seg.end_ms;
        }
        if prev_end < list.duration_ms() {
            gaps.push((prev_end, list.duration_ms()));
        }
        for (a, b) in gaps {
            let x0 = view.ms_to_x(a).max(0.0);
            let x1 = view.ms_to_x(b).min(track);
            if x1 > x0 {
                strip = strip.child(
                    div()
                        .absolute()
                        .top(px(0.0))
                        .left(px(x0))
                        .w(px(x1 - x0))
                        .h(px(THUMB_H))
                        .bg(rgba(COLOR_GAP)),
                );
            }
        }

        // 选中描边、片段边把手、相接分割线
        let segs = list.segments();
        if let Some(seg) = self.selected.and_then(|i| segs.get(i)) {
            let x0 = view.ms_to_x(seg.start_ms);
            let x1 = view.ms_to_x(seg.end_ms);
            strip = strip.child(
                div()
                    .absolute()
                    .top(px(0.0))
                    .left(px(x0))
                    .w(px((x1 - x0).max(1.0)))
                    .h(px(THUMB_H))
                    .border_2()
                    .border_color(rgba(COLOR_SELECTED)),
            );
        }
        for (i, seg) in segs.iter().enumerate() {
            let touching = i > 0 && segs[i - 1].end_ms == seg.start_ms;
            if touching {
                let x = view.ms_to_x(seg.start_ms);
                strip = strip.child(edge_bar(x, 1.0, COLOR_SPLIT));
            } else {
                strip = strip.child(edge_bar(
                    view.ms_to_x(seg.start_ms) + EDGE_W / 2.0,
                    EDGE_W,
                    COLOR_EDGE,
                ));
            }
            if segs.get(i + 1).is_none_or(|n| n.start_ms != seg.end_ms) {
                strip = strip.child(edge_bar(
                    view.ms_to_x(seg.end_ms) - EDGE_W / 2.0,
                    EDGE_W,
                    COLOR_EDGE,
                ));
            }
        }

        // 擦洗条
        let scrub = div()
            .absolute()
            .top(px(0.0))
            .left(px(0.0))
            .w(px(track))
            .h(px(SCRUB_H))
            .rounded_sm()
            .bg(rgba(COLOR_TRACK))
            .cursor_pointer()
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, ev: &MouseDownEvent, _w, cx| {
                    this.press_scrub(f32::from(ev.position.x) - PAD, cx);
                }),
            );

        // 关键帧刻度
        let mut keys = div()
            .absolute()
            .top(px(strip_top + THUMB_H + KEY_GAP))
            .left(px(0.0))
            .w(px(track))
            .h(px(KEY_ROW_H))
            .overflow_hidden();
        let (vis_start, vis_end) = view.visible_range();
        let from = self.keyframes.partition_point(|&k| k < vis_start);
        let mut last_x = f32::MIN;
        for &k in self.keyframes[from..].iter().take_while(|&&k| k <= vis_end) {
            let x = view.ms_to_x(k);
            if x - last_x >= KEY_MIN_GAP {
                last_x = x;
                keys = keys.child(
                    div()
                        .absolute()
                        .top(px(0.0))
                        .left(px(x))
                        .w(px(1.0))
                        .h(px(KEY_ROW_H))
                        .bg(rgba(COLOR_KEY)),
                );
            }
        }

        // 播放头
        let play_x = view.ms_to_x(self.playhead_ms);
        let mut root = div()
            .relative()
            .flex_none()
            .w_full()
            .h(px(TIMELINE_H))
            .overflow_hidden()
            .on_scroll_wheel(cx.listener(|this, ev: &ScrollWheelEvent, _w, cx| {
                this.on_wheel(ev, cx);
                cx.stop_propagation();
            }))
            .child(scrub)
            .child(strip)
            .child(keys);
        if (-PLAYHEAD_W..=track + PLAYHEAD_W).contains(&play_x) {
            root = root.child(
                div()
                    .absolute()
                    .top(px(0.0))
                    .left(px(play_x - PLAYHEAD_W / 2.0))
                    .w(px(PLAYHEAD_W))
                    .h(px(TIMELINE_H))
                    .bg(rgba(COLOR_PLAYHEAD)),
            );
        }
        root
    }
}

/// 移动事件后的拖动状态：左键已松开则结束拖动，否则保持。
fn drag_after_move(drag: Option<Drag>, pressed: bool) -> Option<Drag> {
    if pressed { drag } else { None }
}

/// 竖直条（把手或分割线），`center_x` 为条中心的 x。
fn edge_bar(center_x: f32, width: f32, color: u32) -> Div {
    div()
        .absolute()
        .top(px(0.0))
        .left(px(center_x - width / 2.0))
        .w(px(width))
        .h(px(THUMB_H))
        .bg(rgba(color))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 拖播放头：down -> move 持续更新，up 后不再更新，越界夹在 [0, 总时长]。
    #[test]
    fn playhead_drag_follows_moves_and_stops_on_up() {
        let view = Viewport::new(10_000, 1000.0);
        let mut drag = Some(Drag::Playhead);
        let mut playhead = view.x_to_ms(100.0);
        assert_eq!(playhead, 1000);
        for (x, expect) in [(500.0, 5000), (-80.0, 0), (5000.0, 10_000)] {
            drag = drag_after_move(drag, true);
            if drag == Some(Drag::Playhead) {
                playhead = view.x_to_ms(x);
            }
            assert_eq!(playhead, expect);
        }
        // 松开（up 或移动时已无按键）后不再更新
        drag = drag_after_move(drag, false);
        assert_eq!(drag, None);
        let before = playhead;
        if drag == Some(Drag::Playhead) {
            playhead = view.x_to_ms(300.0);
        }
        assert_eq!(playhead, before);
    }

    /// 缩放平移后 x 换算仍夹在范围内。
    #[test]
    fn drag_clamps_when_zoomed() {
        let mut view = Viewport::new(10_000, 1000.0);
        view.zoom_at(4.0, 500.0);
        assert!(view.x_to_ms(-1e6) == 0);
        assert!(view.x_to_ms(1e6) == 10_000);
    }
}
