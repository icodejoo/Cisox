//! 缩略图缓存：按 `(缩放级别, 瓦片下标)` 分层存放，总字节数超限时 LRU 淘汰。
//!
//! 泛型于值类型，便于离屏单测（界面里的值是 GPUI 图像资源）。

use std::collections::HashMap;

/// 缓存键：`(缩放级别, 瓦片下标)`。
pub type TileKey = (u32, u64);

/// 一条缓存记录。
struct Entry<T> {
    /// 缓存的值。
    value: T,
    /// 占用字节数。
    bytes: usize,
    /// 最近一次访问的序号（越大越新）。
    stamp: u64,
    /// 该缩略图对应的时间（毫秒），用于找邻近替代图。
    at_ms: u64,
}

/// 按总字节数限额的 LRU 缩略图缓存。
pub struct ThumbCache<T> {
    /// 记录表。
    entries: HashMap<TileKey, Entry<T>>,
    /// 总字节数。
    total: usize,
    /// 字节数上限。
    limit: usize,
    /// 访问计数器。
    clock: u64,
}

impl<T: Clone> ThumbCache<T> {
    /// 创建缓存。
    ///
    /// # 参数
    /// - `limit_bytes`：总字节数上限（超出时淘汰最久未用的）。
    pub fn new(limit_bytes: usize) -> Self {
        Self {
            entries: HashMap::new(),
            total: 0,
            limit: limit_bytes,
            clock: 0,
        }
    }

    /// 当前缓存张数。
    #[cfg(test)]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// 是否为空。
    #[cfg(test)]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// 当前总字节数。
    #[cfg(test)]
    pub fn bytes(&self) -> usize {
        self.total
    }

    /// 是否已缓存（不更新访问序）。
    pub fn contains(&self, key: TileKey) -> bool {
        self.entries.contains_key(&key)
    }

    /// 取缓存值并标记为最近使用。
    pub fn get(&mut self, key: TileKey) -> Option<T> {
        self.clock += 1;
        let stamp = self.clock;
        self.entries.get_mut(&key).map(|e| {
            e.stamp = stamp;
            e.value.clone()
        })
    }

    /// 取时间上最接近 `ms` 的任意级别缩略图（精确级别没取到时的替代图），并标记为最近使用。
    pub fn nearest(&mut self, ms: u64) -> Option<T> {
        let key = self
            .entries
            .iter()
            .min_by_key(|(_, e)| e.at_ms.abs_diff(ms))
            .map(|(k, _)| *k)?;
        self.get(key)
    }

    /// 放入一张缩略图；已存在则替换。放入后若超限，淘汰最久未用的（刚放入的除外）。
    ///
    /// # 参数
    /// - `key`：缓存键。
    /// - `value`：缩略图。
    /// - `bytes`：占用字节数。
    /// - `at_ms`：缩略图对应的时间（毫秒）。
    ///
    /// # 返回
    /// 被淘汰的值（调用方可据此释放 GPU 资源）。
    pub fn insert(&mut self, key: TileKey, value: T, bytes: usize, at_ms: u64) -> Vec<T> {
        self.clock += 1;
        let mut evicted = Vec::new();
        if let Some(old) = self.entries.remove(&key) {
            self.total -= old.bytes;
            evicted.push(old.value);
        }
        self.total += bytes;
        self.entries.insert(
            key,
            Entry {
                value,
                bytes,
                stamp: self.clock,
                at_ms,
            },
        );
        while self.total > self.limit && self.entries.len() > 1 {
            let Some(oldest) = self
                .entries
                .iter()
                .filter(|(k, _)| **k != key)
                .min_by_key(|(_, e)| e.stamp)
                .map(|(k, _)| *k)
            else {
                break;
            };
            if let Some(e) = self.entries.remove(&oldest) {
                self.total -= e.bytes;
                evicted.push(e.value);
            }
        }
        evicted
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 超出字节上限时淘汰最久未用的，刚放入的保留。
    #[test]
    fn evicts_least_recently_used() {
        let mut c: ThumbCache<u32> = ThumbCache::new(300);
        c.insert((0, 0), 10, 100, 0);
        c.insert((0, 1), 11, 100, 1000);
        c.insert((0, 2), 12, 100, 2000);
        assert_eq!(c.get((0, 0)), Some(10)); // 0 变为最新
        let gone = c.insert((0, 3), 13, 100, 3000);
        assert_eq!(gone, vec![11]);
        assert!(!c.contains((0, 1)));
        assert!(c.contains((0, 0)) && c.contains((0, 2)) && c.contains((0, 3)));
        assert_eq!(c.bytes(), 300);
    }

    /// 单张超过上限也保留它自己；替换同键会回收旧值并修正字节数。
    #[test]
    fn oversize_and_replace() {
        let mut c: ThumbCache<u32> = ThumbCache::new(100);
        c.insert((1, 1), 1, 500, 0);
        assert_eq!(c.len(), 1);
        let old = c.insert((1, 1), 2, 50, 0);
        assert_eq!(old, vec![1]);
        assert_eq!(c.bytes(), 50);
        assert_eq!(c.get((1, 1)), Some(2));
        assert!(!c.is_empty());
    }

    /// 邻近替代图：跨级别取时间最近的一张。
    #[test]
    fn nearest_across_levels() {
        let mut c: ThumbCache<u32> = ThumbCache::new(1_000);
        assert_eq!(c.nearest(5), None);
        c.insert((0, 0), 1, 10, 1_000);
        c.insert((4, 7), 2, 10, 5_000);
        assert_eq!(c.nearest(4_000), Some(2));
        assert_eq!(c.nearest(1_200), Some(1));
    }
}
