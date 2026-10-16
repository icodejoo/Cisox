//! 预览播放的“跳段”决策：给定当前播放时间和片段列表，决定继续、跳到下一段起点还是结束。
//!
//! 纯函数，不依赖 MF / GPUI，可离屏单测。

use crate::video_editor::segments::EditList;

/// 提前量（毫秒）：播放时间离片段末尾不足它就提前处理，避免露出已删画面（含 Pause 与 seek 的延迟）。
pub const END_LEAD_MS: u64 = 25;

/// 播放决策。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlayAction {
    /// 继续播放。
    Continue,
    /// 不暂停，直接跳到该时间继续播放（下一保留片段起点，或循环回首段）。
    Jump(u64),
    /// 全部播完：暂停并停在该时间（最后一段的末尾）。
    Finish(u64),
}

/// 根据当前时间做播放决策。
///
/// # 参数
/// - `now_ms`：引擎当前播放时间（毫秒，原视频时间）。
/// - `list`：剪辑列表。
/// - `looping`：播完后是否回到首段继续。
///
/// # 返回
/// - 在保留片段内且离末尾还远：`Continue`。
/// - 临近片段末尾或落在已删空隙：有下一段则 `Jump(下一段起点)`，否则循环时 `Jump(首段起点)`，
///   不循环时 `Finish(最后一段终点 - 1ms)`。
///
/// # 示例
/// ```ignore
/// let mut l = EditList::new(10_000);
/// assert_eq!(decide(1_000, &l, false), PlayAction::Continue);
/// assert_eq!(decide(9_990, &l, false), PlayAction::Finish(9_999));
/// ```
pub fn decide(now_ms: u64, list: &EditList, looping: bool) -> PlayAction {
    let segs = list.segments();
    let (Some(first), Some(last)) = (segs.first(), segs.last()) else {
        return PlayAction::Finish(0);
    };
    let ahead = now_ms + END_LEAD_MS;
    // 当前所在片段（落在空隙里则取其后第一段）
    let cur = segs.iter().position(|s| now_ms < s.end_ms);
    let Some(i) = cur else {
        return finish_or_loop(first.start_ms, last.end_ms, looping);
    };
    let seg = segs[i];
    if now_ms < seg.start_ms {
        // 落在空隙里：马上跳到这一段起点
        return PlayAction::Jump(seg.start_ms);
    }
    if ahead < seg.end_ms {
        return PlayAction::Continue;
    }
    match segs.get(i + 1) {
        Some(next) if next.start_ms == seg.end_ms => PlayAction::Continue, // 相接，无需跳
        Some(next) => PlayAction::Jump(next.start_ms),
        None => finish_or_loop(first.start_ms, last.end_ms, looping),
    }
}

/// 播到末尾：循环则回首段起点，否则停在最后一段终点前 1ms。
fn finish_or_loop(first_start: u64, last_end: u64, looping: bool) -> PlayAction {
    if looping {
        PlayAction::Jump(first_start)
    } else {
        PlayAction::Finish(last_end.saturating_sub(1))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 构造 `[0,4000) [7000,10000)` 的列表。
    fn gapped() -> EditList {
        let mut l = EditList::new(10_000);
        l.split_at(4_000);
        l.split_at(7_000);
        l.delete(1);
        l
    }

    /// 片段中间继续播放。
    #[test]
    fn continues_inside_segment() {
        assert_eq!(decide(1_000, &gapped(), false), PlayAction::Continue);
        assert_eq!(decide(7_000, &gapped(), false), PlayAction::Continue);
    }

    /// 临近片段末尾提前跳到下一段起点。
    #[test]
    fn jumps_before_segment_end() {
        let l = gapped();
        assert_eq!(
            decide(4_000 - END_LEAD_MS, &l, false),
            PlayAction::Jump(7_000)
        );
        assert_eq!(
            decide(4_000 - END_LEAD_MS - 1, &l, false),
            PlayAction::Continue
        );
        // 落在空隙里也跳
        assert_eq!(decide(5_000, &l, false), PlayAction::Jump(7_000));
    }

    /// 最后一段播完：不循环则结束，循环则回首段。
    #[test]
    fn finish_or_loop_at_end() {
        let l = gapped();
        assert_eq!(decide(9_990, &l, false), PlayAction::Finish(9_999));
        assert_eq!(decide(9_990, &l, true), PlayAction::Jump(0));
        assert_eq!(decide(10_000, &l, false), PlayAction::Finish(9_999));
        // 末尾被删：最后一段终点之后
        let mut t = EditList::new(10_000);
        t.set_out(6_000);
        assert_eq!(decide(8_000, &t, false), PlayAction::Finish(5_999));
    }

    /// 相接的相邻片段不需要跳。
    #[test]
    fn touching_segments_play_through() {
        let mut l = EditList::new(10_000);
        l.split_at(5_000);
        assert_eq!(decide(4_990, &l, false), PlayAction::Continue);
    }

    /// 起点之前（被 set_in 删掉的头部）直接跳到首段起点。
    #[test]
    fn leading_gap_jumps_to_first() {
        let mut l = EditList::new(10_000);
        l.set_in(2_000);
        assert_eq!(decide(500, &l, false), PlayAction::Jump(2_000));
    }
}
