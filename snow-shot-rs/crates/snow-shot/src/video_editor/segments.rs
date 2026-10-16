//! 剪辑片段列表：按原视频时间升序的保留区间 `[start, end)`，以及分割 / 删除 / 恢复 / 拖边界。
//!
//! 不依赖 GPUI，全部可离屏单测。已删除的区间就是相邻保留片段之间（以及首尾）的空隙。

/// 片段（或空隙）的最小长度（毫秒）。
pub const MIN_SPAN_MS: u64 = 100;

/// 一个保留区间 `[start_ms, end_ms)`。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Segment {
    /// 起点（毫秒，含）。
    pub start_ms: u64,
    /// 终点（毫秒，不含）。
    pub end_ms: u64,
}

impl Segment {
    /// 区间长度。
    pub(crate) fn len(&self) -> u64 {
        self.end_ms.saturating_sub(self.start_ms)
    }

    /// `ms` 是否落在 `[start, end)` 内。
    pub fn contains(&self, ms: u64) -> bool {
        ms >= self.start_ms && ms < self.end_ms
    }
}

/// 被拖动的片段边。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Edge {
    /// 第 `usize` 个片段的起点。
    Start(usize),
    /// 第 `usize` 个片段的终点。
    End(usize),
}

/// 剪辑列表：视频总时长 + 升序、不重叠的保留片段（至少一段）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EditList {
    /// 原视频总时长。
    duration_ms: u64,
    /// 保留片段。
    keep: Vec<Segment>,
}

impl EditList {
    /// 整段视频全部保留。
    ///
    /// # 参数
    /// - `duration_ms`：原视频总时长。
    ///
    /// # 示例
    /// ```ignore
    /// let list = EditList::new(10_000);
    /// assert_eq!(list.kept_ms(), 10_000);
    /// ```
    pub fn new(duration_ms: u64) -> Self {
        Self {
            duration_ms,
            keep: vec![Segment {
                start_ms: 0,
                end_ms: duration_ms,
            }],
        }
    }

    /// 原视频总时长。
    pub fn duration_ms(&self) -> u64 {
        self.duration_ms
    }

    /// 保留片段（升序）。
    pub fn segments(&self) -> &[Segment] {
        &self.keep
    }

    /// 保留部分累计时长（界面显示的总时长）。
    pub fn kept_ms(&self) -> u64 {
        self.keep.iter().map(Segment::len).sum()
    }

    /// 是否满足导出条件（至少一段且每段不小于最小长度；构造与各操作已保证，这里兜底）。
    pub fn can_export(&self) -> bool {
        !self.keep.is_empty()
            && self
                .keep
                .iter()
                .all(|s| s.len() >= MIN_SPAN_MS.min(self.duration_ms))
    }

    /// 导出用的区间列表 `(start, end)`（毫秒）。
    pub fn export_ranges(&self) -> Vec<(u64, u64)> {
        self.keep.iter().map(|s| (s.start_ms, s.end_ms)).collect()
    }

    /// 包含 `ms` 的保留片段下标。
    pub fn segment_at(&self, ms: u64) -> Option<usize> {
        self.keep.iter().position(|s| s.contains(ms))
    }

    /// 包含 `ms` 的已删空隙 `(start, end)`；`ms` 在保留片段内时为 `None`。
    pub fn gap_at(&self, ms: u64) -> Option<(u64, u64)> {
        if ms >= self.duration_ms || self.segment_at(ms).is_some() {
            return None;
        }
        let start = self
            .keep
            .iter()
            .rev()
            .find(|s| s.end_ms <= ms)
            .map_or(0, |s| s.end_ms);
        let end = self
            .keep
            .iter()
            .find(|s| s.start_ms > ms)
            .map_or(self.duration_ms, |s| s.start_ms);
        Some((start, end))
    }

    /// 在 `ms` 处分割所在片段（两侧都不能短于最小长度）。
    ///
    /// # 返回
    /// 分割后左半片段的下标；位置不合法返回 `None`。
    pub fn split_at(&mut self, ms: u64) -> Option<usize> {
        let idx = self.segment_at(ms)?;
        let seg = self.keep[idx];
        if ms - seg.start_ms < MIN_SPAN_MS || seg.end_ms - ms < MIN_SPAN_MS {
            return None;
        }
        self.keep[idx].end_ms = ms;
        self.keep.insert(
            idx + 1,
            Segment {
                start_ms: ms,
                end_ms: seg.end_ms,
            },
        );
        Some(idx)
    }

    /// 删除第 `index` 个片段；只剩一段时拒绝（保证至少保留一段）。
    ///
    /// # 返回
    /// 是否删除成功。
    pub fn delete(&mut self, index: usize) -> bool {
        if self.keep.len() <= 1 || index >= self.keep.len() {
            return false;
        }
        self.keep.remove(index);
        true
    }

    /// 恢复 `ms` 所在的已删空隙（整段空隙变回保留片段），并与相接的邻居合并。
    ///
    /// # 返回
    /// 恢复后该位置所在片段的下标；`ms` 不在空隙内返回 `None`。
    pub fn restore_at(&mut self, ms: u64) -> Option<usize> {
        let (start, end) = self.gap_at(ms)?;
        let at = self.keep.partition_point(|s| s.start_ms < start);
        self.keep.insert(
            at,
            Segment {
                start_ms: start,
                end_ms: end,
            },
        );
        self.merge_touching();
        self.segment_at(start)
    }

    /// 合并首尾相接的相邻片段。
    fn merge_touching(&mut self) {
        let mut merged: Vec<Segment> = Vec::with_capacity(self.keep.len());
        for seg in self.keep.drain(..) {
            match merged.last_mut() {
                Some(last) if last.end_ms == seg.start_ms => last.end_ms = seg.end_ms,
                _ => merged.push(seg),
            }
        }
        self.keep = merged;
    }

    /// 设置入点：把 `ms` 之前的部分全部删掉（`ms` 夹在最后一段终点之前至少一个最小长度处）。
    ///
    /// # 返回
    /// 实际生效的入点。
    pub fn set_in(&mut self, ms: u64) -> u64 {
        let max = self
            .keep
            .last()
            .map_or(0, |s| s.end_ms.saturating_sub(MIN_SPAN_MS));
        let ms = ms.min(max);
        for seg in &mut self.keep {
            seg.start_ms = seg.start_ms.max(ms);
        }
        self.keep
            .retain(|s| s.end_ms > s.start_ms && s.len() >= MIN_SPAN_MS);
        ms
    }

    /// 设置出点：把 `ms` 之后的部分全部删掉（`ms` 夹在第一段起点之后至少一个最小长度处）。
    ///
    /// # 返回
    /// 实际生效的出点。
    pub fn set_out(&mut self, ms: u64) -> u64 {
        let min = self.keep.first().map_or(0, |s| s.start_ms + MIN_SPAN_MS);
        let ms = ms.clamp(min.min(self.duration_ms), self.duration_ms);
        for seg in &mut self.keep {
            seg.end_ms = seg.end_ms.min(ms);
        }
        self.keep
            .retain(|s| s.end_ms > s.start_ms && s.len() >= MIN_SPAN_MS);
        ms
    }

    /// 拖动某个片段边到 `ms`。与前 / 后一段首尾相接时两边一起动，否则只动这一条边。
    ///
    /// # 返回
    /// 实际生效的位置；边不存在返回 `None`。
    pub fn move_edge(&mut self, edge: Edge, ms: u64) -> Option<u64> {
        match edge {
            Edge::Start(i) => {
                let cur = *self.keep.get(i)?;
                let touching = i > 0 && self.keep[i - 1].end_ms == cur.start_ms;
                let lo = match (i, touching) {
                    (0, _) => 0,
                    (_, true) => self.keep[i - 1].start_ms + MIN_SPAN_MS,
                    (_, false) => self.keep[i - 1].end_ms,
                };
                let hi = cur.end_ms.saturating_sub(MIN_SPAN_MS);
                let v = ms.clamp(lo.min(hi), hi);
                self.keep[i].start_ms = v;
                if touching {
                    self.keep[i - 1].end_ms = v;
                }
                Some(v)
            }
            Edge::End(i) => {
                let cur = *self.keep.get(i)?;
                let next = self.keep.get(i + 1).copied();
                let touching = next.is_some_and(|n| n.start_ms == cur.end_ms);
                let hi = match (next, touching) {
                    (None, _) => self.duration_ms,
                    (Some(n), true) => n.end_ms.saturating_sub(MIN_SPAN_MS),
                    (Some(n), false) => n.start_ms,
                };
                let lo = cur.start_ms + MIN_SPAN_MS;
                let v = ms.clamp(lo.min(hi), hi);
                self.keep[i].end_ms = v;
                if touching {
                    self.keep[i + 1].start_ms = v;
                }
                Some(v)
            }
        }
    }

    /// 原视频时间换算成“保留部分累计时间”（落在空隙里按空隙起点算）。
    pub fn source_to_kept(&self, ms: u64) -> u64 {
        self.keep
            .iter()
            .map(|s| ms.min(s.end_ms).saturating_sub(s.start_ms))
            .sum()
    }

    /// 保留部分累计时间换算回原视频时间；超出总长时返回最后一段终点。
    ///
    /// 预留：界面目前只用“原时间 → 累计时间”，反向换算供按累计时间定位时使用。
    #[allow(dead_code)]
    pub fn kept_to_source(&self, kept_ms: u64) -> u64 {
        let mut left = kept_ms;
        for s in &self.keep {
            if left < s.len() {
                return s.start_ms + left;
            }
            left -= s.len();
        }
        self.keep.last().map_or(0, |s| s.end_ms)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 取片段列表的 `(start, end)` 便于断言。
    fn ranges(list: &EditList) -> Vec<(u64, u64)> {
        list.export_ranges()
    }

    /// 分割产生相接的两段；太靠近边缘的分割被拒绝。
    #[test]
    fn split_respects_min_span() {
        let mut l = EditList::new(10_000);
        assert_eq!(l.split_at(50), None);
        assert_eq!(l.split_at(9_950), None);
        assert_eq!(l.split_at(4_000), Some(0));
        assert_eq!(ranges(&l), vec![(0, 4_000), (4_000, 10_000)]);
        assert_eq!(l.split_at(4_050), None);
        assert_eq!(l.split_at(20_000), None);
    }

    /// 删除后留下空隙；最后一段不允许删。
    #[test]
    fn delete_keeps_one_segment() {
        let mut l = EditList::new(10_000);
        l.split_at(4_000);
        l.split_at(7_000);
        assert!(l.delete(1));
        assert_eq!(ranges(&l), vec![(0, 4_000), (7_000, 10_000)]);
        assert_eq!(l.gap_at(5_000), Some((4_000, 7_000)));
        assert_eq!(l.gap_at(1_000), None);
        assert!(l.delete(0));
        assert!(!l.delete(0));
        assert!(!l.delete(9));
        assert_eq!(l.gap_at(1_000), Some((0, 7_000)));
    }

    /// 恢复空隙会与相接邻居合并回一段。
    #[test]
    fn restore_merges_neighbours() {
        let mut l = EditList::new(10_000);
        l.split_at(4_000);
        l.split_at(7_000);
        l.delete(1);
        assert_eq!(l.restore_at(5_000), Some(0));
        assert_eq!(ranges(&l), vec![(0, 10_000)]);
        assert_eq!(ranges(&l), vec![(0, 10_000)]);
        assert_eq!(l.restore_at(5_000), None);
        // 首部空隙恢复
        l.set_in(2_000);
        assert_eq!(l.restore_at(500), Some(0));
        assert_eq!(ranges(&l), vec![(0, 10_000)]);
    }

    /// 入点 / 出点等价于删掉之前 / 之后的部分，并丢弃过短的残片。
    #[test]
    fn set_in_out_trim_ends() {
        let mut l = EditList::new(10_000);
        l.split_at(2_000);
        l.split_at(5_000);
        assert_eq!(l.set_in(3_000), 3_000);
        assert_eq!(ranges(&l), vec![(3_000, 5_000), (5_000, 10_000)]);
        assert_eq!(l.set_out(8_000), 8_000);
        assert_eq!(ranges(&l), vec![(3_000, 5_000), (5_000, 8_000)]);
        // 入点超过尾部时夹住，最后一段保留最小长度
        assert_eq!(l.set_in(99_000), 8_000 - MIN_SPAN_MS);
        assert_eq!(ranges(&l), vec![(8_000 - MIN_SPAN_MS, 8_000)]);
        // 出点小于首段起点时夹住
        assert_eq!(l.set_out(0), 8_000);
    }

    /// 拖边界：独立边只动自己，相接边一起动，且不越过邻居、不小于最小长度。
    #[test]
    fn move_edge_clamps() {
        let mut l = EditList::new(10_000);
        l.split_at(4_000);
        l.split_at(7_000);
        l.delete(1); // [0,4000) [7000,10000)
        assert_eq!(l.move_edge(Edge::End(0), 9_000), Some(7_000));
        assert_eq!(l.move_edge(Edge::End(0), 0), Some(MIN_SPAN_MS));
        assert_eq!(l.move_edge(Edge::Start(1), 0), Some(MIN_SPAN_MS));
        assert_eq!(ranges(&l), vec![(0, MIN_SPAN_MS), (MIN_SPAN_MS, 10_000)]);
        // 此时两段相接，拖相接边两边同动
        assert_eq!(l.move_edge(Edge::Start(1), 3_000), Some(3_000));
        assert_eq!(ranges(&l), vec![(0, 3_000), (3_000, 10_000)]);
        assert_eq!(
            l.move_edge(Edge::End(0), 99_000),
            Some(10_000 - MIN_SPAN_MS)
        );
        assert_eq!(l.move_edge(Edge::End(7), 1), None);
        // 首段起点、末段终点
        assert_eq!(l.move_edge(Edge::Start(0), 500), Some(500));
        assert_eq!(l.move_edge(Edge::End(1), 99_999), Some(10_000));
    }

    /// 原时间与累计保留时间互换。
    #[test]
    fn kept_time_conversion() {
        let mut l = EditList::new(10_000);
        l.split_at(4_000);
        l.split_at(7_000);
        l.delete(1); // 保留 0-4000、7000-10000，共 7000
        assert_eq!(l.kept_ms(), 7_000);
        assert_eq!(l.source_to_kept(0), 0);
        assert_eq!(l.source_to_kept(3_000), 3_000);
        assert_eq!(l.source_to_kept(5_500), 4_000);
        assert_eq!(l.source_to_kept(8_000), 5_000);
        assert_eq!(l.source_to_kept(10_000), 7_000);
        assert_eq!(l.kept_to_source(3_999), 3_999);
        assert_eq!(l.kept_to_source(4_000), 7_000);
        assert_eq!(l.kept_to_source(5_000), 8_000);
        assert_eq!(l.kept_to_source(99_999), 10_000);
        for k in [0, 1, 3_999, 4_000, 6_999] {
            assert_eq!(l.source_to_kept(l.kept_to_source(k)), k);
        }
    }

    /// 导出区间升序、不重叠。
    #[test]
    fn export_ranges_are_sorted() {
        let mut l = EditList::new(9_000);
        l.split_at(3_000);
        l.split_at(6_000);
        l.delete(1);
        let r = l.export_ranges();
        assert_eq!(r, vec![(0, 3_000), (6_000, 9_000)]);
        assert!(r.windows(2).all(|w| w[0].1 <= w[1].0));
        assert!(l.can_export());
    }
}
