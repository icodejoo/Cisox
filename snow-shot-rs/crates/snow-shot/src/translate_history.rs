//! 翻译页历史：最近 N 条翻译记录，落盘到 Cisox 数据目录。
//!
//! 纯数据 + 简单文件读写，不依赖 GPUI，可离屏测试。旧版翻译页没有历史列表，
//! 因此没有对应的配置键，条数上限用常量。

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// 历史文件名（放在数据根目录下）。
pub const HISTORY_FILE: &str = "translate_history.json";
/// 最多保留的记录数。
pub const HISTORY_LIMIT: usize = 50;
/// 单条原文 / 译文最多保留的字符数（防止历史文件膨胀）。
const MAX_TEXT_CHARS: usize = 4000;
/// 列表预览最多显示的字符数。
const PREVIEW_CHARS: usize = 40;

/// 一条翻译记录。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HistoryEntry {
    /// 源语言代码（自动检测为 `auto`）。
    pub source_lang: String,
    /// 目标语言代码。
    pub target_lang: String,
    /// 原文。
    pub source_text: String,
    /// 译文。
    pub translation: String,
    /// 实际用的翻译包标签（可空）。
    #[serde(default)]
    pub label: String,
}

/// 截断到最多 `max` 个字符（按字符而非字节）。
fn truncate_chars(text: &str, max: usize) -> String {
    text.chars().take(max).collect()
}

impl HistoryEntry {
    /// 列表里显示的一行预览：原文压成单行并截断，超长加省略号。
    pub fn preview(&self) -> String {
        let flat: String = self
            .source_text
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ");
        if flat.chars().count() > PREVIEW_CHARS {
            format!("{}…", truncate_chars(&flat, PREVIEW_CHARS))
        } else {
            flat
        }
    }

    /// 是否是同一次翻译（原文与语言对都相同，用于去重）。
    fn same_request(&self, other: &HistoryEntry) -> bool {
        self.source_text == other.source_text
            && self.source_lang == other.source_lang
            && self.target_lang == other.target_lang
    }
}

/// 翻译历史：新的在前。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TranslateHistory {
    /// 记录，下标 0 是最近一条。
    entries: Vec<HistoryEntry>,
}

impl TranslateHistory {
    /// 全部记录（新的在前）。
    pub fn entries(&self) -> &[HistoryEntry] {
        &self.entries
    }

    /// 记录条数。
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// 是否没有记录。
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// 取第 `index` 条（0 为最近）。
    pub fn get(&self, index: usize) -> Option<&HistoryEntry> {
        self.entries.get(index)
    }

    /// 清空。
    pub fn clear(&mut self) {
        self.entries.clear();
    }

    /// 记一条翻译：空原文 / 空译文忽略；同一请求（原文 + 语言对）去重并提到最前；超过上限丢最旧的。
    ///
    /// # 参数
    /// - `entry`：新记录（文本会按上限截断）。
    ///
    /// # 返回
    /// 是否真的记录了。
    pub fn push(&mut self, mut entry: HistoryEntry) -> bool {
        if entry.source_text.trim().is_empty() || entry.translation.trim().is_empty() {
            return false;
        }
        entry.source_text = truncate_chars(&entry.source_text, MAX_TEXT_CHARS);
        entry.translation = truncate_chars(&entry.translation, MAX_TEXT_CHARS);
        self.entries.retain(|old| !old.same_request(&entry));
        self.entries.insert(0, entry);
        self.entries.truncate(HISTORY_LIMIT);
        true
    }

    /// 从 JSON 文本恢复；格式不对返回空历史，超过上限的多余部分丢弃，空记录跳过。
    ///
    /// # 参数
    /// - `text`：文件内容。
    pub fn from_json(text: &str) -> Self {
        let parsed: Vec<HistoryEntry> = serde_json::from_str(text).unwrap_or_default();
        // 文件里是新的在前；逐条倒着 push 以复用去重与截断规则
        let mut history = Self::default();
        for entry in parsed.into_iter().take(HISTORY_LIMIT).rev() {
            history.push(entry);
        }
        history
    }

    /// 序列化为 JSON 文本（新的在前）。
    pub fn to_json(&self) -> String {
        serde_json::to_string(&self.entries).unwrap_or_else(|_| "[]".to_string())
    }
}

/// 历史文件路径。
///
/// # 参数
/// - `data_root`：数据根目录。
pub fn history_path(data_root: &Path) -> PathBuf {
    data_root.join(HISTORY_FILE)
}

/// 读历史文件；不存在或损坏返回空历史。
///
/// # 参数
/// - `path`：历史文件路径。
pub fn load(path: &Path) -> TranslateHistory {
    std::fs::read_to_string(path)
        .map(|text| TranslateHistory::from_json(&text))
        .unwrap_or_default()
}

/// 写历史文件（先写临时文件再改名，避免写一半断电留下坏文件）。
///
/// # 参数
/// - `path`：历史文件路径。
/// - `history`：要保存的历史。
pub fn save(path: &Path, history: &TranslateHistory) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, history.to_json())?;
    std::fs::rename(&tmp, path)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 造一条记录。
    fn entry(src: &str, dst: &str) -> HistoryEntry {
        HistoryEntry {
            source_lang: "auto".into(),
            target_lang: "en".into(),
            source_text: src.into(),
            translation: dst.into(),
            label: "m".into(),
        }
    }

    /// 新的在前；同一请求去重并提到最前；空文本不记。
    #[test]
    fn push_orders_dedupes_and_skips_empty() {
        let mut h = TranslateHistory::default();
        assert!(h.push(entry("a", "A")));
        assert!(h.push(entry("b", "B")));
        assert!(h.push(entry("a", "A2")));
        assert_eq!(h.len(), 2);
        assert_eq!(h.get(0).unwrap().translation, "A2");
        assert!(!h.push(entry("  ", "x")));
        assert!(!h.push(entry("x", "")));
        assert_eq!(h.len(), 2);
        // 语言对不同不算重复
        let mut other = entry("a", "A3");
        other.target_lang = "ja".into();
        assert!(h.push(other));
        assert_eq!(h.len(), 3);
    }

    /// 超过上限只留最近的；超长文本被截断。
    #[test]
    fn limit_and_truncation() {
        let mut h = TranslateHistory::default();
        for i in 0..(HISTORY_LIMIT + 7) {
            h.push(entry(&format!("t{i}"), "x"));
        }
        assert_eq!(h.len(), HISTORY_LIMIT);
        assert_eq!(
            h.get(0).unwrap().source_text,
            format!("t{}", HISTORY_LIMIT + 6)
        );
        let long = "字".repeat(MAX_TEXT_CHARS + 100);
        h.push(entry(&long, &long));
        assert_eq!(
            h.get(0).unwrap().source_text.chars().count(),
            MAX_TEXT_CHARS
        );
    }

    /// 预览压成单行并截断加省略号。
    #[test]
    fn preview_is_single_line_and_short() {
        assert_eq!(entry("hello\n  world", "x").preview(), "hello world");
        let p = entry(&"a".repeat(100), "x").preview();
        assert_eq!(p.chars().count(), PREVIEW_CHARS + 1);
        assert!(p.ends_with('…'));
    }

    /// JSON 往返保持顺序；损坏内容得到空历史。
    #[test]
    fn json_roundtrip_and_corrupt() {
        let mut h = TranslateHistory::default();
        h.push(entry("a", "A"));
        h.push(entry("b", "B"));
        let back = TranslateHistory::from_json(&h.to_json());
        assert_eq!(back, h);
        assert!(TranslateHistory::from_json("not json").is_empty());
        assert!(TranslateHistory::from_json("{}").is_empty());
    }

    /// 落盘读回一致；文件不存在得到空历史；写入时自动建目录。
    #[test]
    fn save_and_load_use_given_dir() {
        let dir = std::env::temp_dir().join(format!("cisox-trans-history-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let path = history_path(&dir.join("nested"));
        assert!(load(&path).is_empty());
        let mut h = TranslateHistory::default();
        h.push(entry("你好", "hello"));
        save(&path, &h).unwrap();
        assert_eq!(load(&path), h);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
