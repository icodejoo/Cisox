//! 字符错误率（CER）：基于 Unicode 标量的编辑距离。

/// 规整文本：去掉全部空白（含全角空格与换行），只比较可见字符。
///
/// # 参数
/// - `text`：原始文本。
///
/// # 返回
/// 去空白后的字符序列。
///
/// # 示例
/// ```
/// assert_eq!(snow_ocr_compare::cer::normalize("a b\n中 文"), vec!['a', 'b', '中', '文']);
/// ```
pub fn normalize(text: &str) -> Vec<char> {
    text.chars().filter(|c| !c.is_whitespace()).collect()
}

/// 两个字符序列的 Levenshtein 编辑距离（插入、删除、替换各算 1）。
///
/// # 参数
/// - `a` / `b`：待比较序列。
///
/// # 示例
/// ```
/// use snow_ocr_compare::cer::edit_distance;
/// assert_eq!(edit_distance(&['a', 'b'], &['a', 'c']), 1);
/// ```
pub fn edit_distance(a: &[char], b: &[char]) -> usize {
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    let mut cur = vec![0; b.len() + 1];
    for (i, ca) in a.iter().enumerate() {
        cur[0] = i + 1;
        for (j, cb) in b.iter().enumerate() {
            let substitute = prev[j] + usize::from(ca != cb);
            cur[j + 1] = substitute.min(prev[j + 1] + 1).min(cur[j] + 1);
        }
        std::mem::swap(&mut prev, &mut cur);
    }
    prev[b.len()]
}

/// 一次比较的结果。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CerScore {
    /// 编辑距离。
    pub distance: usize,
    /// 期望文本的规整后字符数（CER 的分母）。
    pub expected_chars: usize,
}

impl CerScore {
    /// 字符错误率 `距离 / 期望字符数`，可能大于 1；期望为空时：识别也为空记 0，否则记 1。
    pub fn cer(&self) -> f64 {
        match (self.expected_chars, self.distance) {
            (0, 0) => 0.0,
            (0, _) => 1.0,
            (n, d) => d as f64 / n as f64,
        }
    }

    /// 字符准确率 `max(0, 1 - CER)`。
    pub fn accuracy(&self) -> f64 {
        (1.0 - self.cer()).max(0.0)
    }
}

/// 比较识别文本与期望文本（均先规整）。
///
/// # 参数
/// - `expected`：期望文本。
/// - `actual`：识别文本。
///
/// # 返回
/// 编辑距离与分母，可再换算 CER / 准确率。
///
/// # 示例
/// ```
/// use snow_ocr_compare::cer::score;
/// let s = score("你好 world", "你好 w0rld");
/// assert_eq!(s.distance, 1);
/// assert!((s.cer() - 1.0 / 7.0).abs() < 1e-9);
/// ```
pub fn score(expected: &str, actual: &str) -> CerScore {
    let (e, a) = (normalize(expected), normalize(actual));
    CerScore {
        distance: edit_distance(&e, &a),
        expected_chars: e.len(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 编辑距离的经典用例。
    #[test]
    fn edit_distance_classic_cases() {
        let d = |a: &str, b: &str| {
            edit_distance(
                &a.chars().collect::<Vec<_>>(),
                &b.chars().collect::<Vec<_>>(),
            )
        };
        assert_eq!(d("kitten", "sitting"), 3);
        assert_eq!(d("", "abc"), 3);
        assert_eq!(d("abc", ""), 3);
        assert_eq!(d("same", "same"), 0);
        assert_eq!(d("你好", "你们"), 1);
    }

    /// CER 忽略空白与换行；完全一致为 0。
    #[test]
    fn score_ignores_whitespace() {
        let s = score("Hello 世界\nline2", "Hello世 界 line2");
        assert_eq!(s.distance, 0);
        assert_eq!(s.cer(), 0.0);
        assert_eq!(s.accuracy(), 1.0);
    }

    /// 全部丢失时 CER 为 1；多识别出的字符让 CER 超过 1，准确率封底为 0。
    #[test]
    fn cer_bounds() {
        assert_eq!(score("abcd", "").cer(), 1.0);
        let over = score("ab", "abxxxx");
        assert_eq!(over.cer(), 2.0);
        assert_eq!(over.accuracy(), 0.0);
    }

    /// 期望为空的边界：识别也空为 0，否则为 1。
    #[test]
    fn empty_expected_edge_cases() {
        assert_eq!(score("  ", "").cer(), 0.0);
        assert_eq!(score("", "x").cer(), 1.0);
    }
}
