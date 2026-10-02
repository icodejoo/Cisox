//! 束搜索的纯逻辑：候选选择、假设管理、长度惩罚、KV 行重排。
//!
//! 与 ORT 无关，可用合成 logits 离线测试；[`crate::engine`] 负责把它接到 merged decoder 上。

use std::cmp::Ordering;

/// 一个候选：`(token id, 对数概率)`。
pub type Candidate = (u32, f32);

/// 一条假设（已生成的 token 与累计对数概率）。
#[derive(Debug, Clone, PartialEq)]
struct Hyp {
    /// 已生成 token（不含 eos）。
    tokens: Vec<u32>,
    /// 累计对数概率。
    score: f32,
}

/// 一步推进的结果。
#[derive(Debug, Clone, PartialEq)]
pub enum Advance {
    /// 继续：`parents[i]` 是新束 `i` 继承的旧束下标，`tokens[i]` 是它这一步的输入 token。
    Continue {
        /// 每条新束的父束下标。
        parents: Vec<usize>,
        /// 每条新束的下一步输入 token。
        tokens: Vec<u32>,
    },
    /// 搜索结束，用 [`BeamSearch::best`] 取结果。
    Done,
}

/// 束搜索状态机。
///
/// # 示例
/// ```ignore
/// let mut bs = BeamSearch::new(4, 0, 1.0, 64);
/// // 每步：对每条存活束求 top-2*width 候选，交给 advance
/// let adv = bs.advance(&cands);
/// ```
pub struct BeamSearch {
    /// 束宽。
    width: usize,
    /// 结束 token。
    eos: u32,
    /// 长度惩罚指数。
    length_penalty: f32,
    /// 最大生成步数。
    max_new: usize,
    /// 禁止重复 n-gram 的 n，0 表示不限制。
    no_repeat_ngram: usize,
    /// 存活束。
    alive: Vec<Hyp>,
    /// 已完成假设：`(归一化得分, 假设)`。
    finished: Vec<(f32, Hyp)>,
    /// 已推进步数。
    steps: usize,
    /// 是否已结束。
    done: bool,
}

/// 按分数降序比较（NaN 视为最小）。
fn cmp_desc(a: f32, b: f32) -> Ordering {
    b.partial_cmp(&a).unwrap_or_else(|| {
        if a.is_nan() && !b.is_nan() {
            Ordering::Greater
        } else if b.is_nan() && !a.is_nan() {
            Ordering::Less
        } else {
            Ordering::Equal
        }
    })
}

impl BeamSearch {
    /// 创建搜索，初始只有一条空的存活束。
    ///
    /// # 参数
    /// - `width`：束宽（至少 1）。
    /// - `eos`：结束 token id。
    /// - `length_penalty`：长度惩罚指数，得分 = 累计对数概率 / 长度^指数。
    /// - `max_new`：最大生成步数。
    /// - `no_repeat_ngram`：禁止同一假设内重复出现的 n-gram 长度，0 表示不限制。
    pub fn new(
        width: usize,
        eos: u32,
        length_penalty: f32,
        max_new: usize,
        no_repeat_ngram: usize,
    ) -> Self {
        Self {
            width: width.max(1),
            eos,
            length_penalty,
            max_new,
            no_repeat_ngram,
            alive: vec![Hyp {
                tokens: Vec::new(),
                score: 0.0,
            }],
            finished: Vec::new(),
            steps: 0,
            done: false,
        }
    }

    /// 当前存活束数量（调用 [`Self::advance`] 时 `cands` 必须与之等长）。
    pub fn alive_len(&self) -> usize {
        self.alive.len()
    }

    /// 长度归一化得分；长度含结束位，避免除零。
    fn normalized(&self, score: f32, generated: usize) -> f32 {
        score / ((generated + 1) as f32).powf(self.length_penalty)
    }

    /// 推进一步。
    ///
    /// # 参数
    /// - `cands`：每条存活束的候选（按对数概率降序，建议 `2*width` 个）。
    ///
    /// # 返回
    /// [`Advance::Continue`] 给出新束的来源与输入；[`Advance::Done`] 表示结束。
    /// 已结束后再调用恒返回 `Done`；`cands` 与存活束数量不符视为已无可推进，也返回 `Done`。
    pub fn advance(&mut self, cands: &[Vec<Candidate>]) -> Advance {
        if self.done || cands.len() != self.alive.len() {
            self.done = true;
            return Advance::Done;
        }
        // 汇总 (总分, 父束, token)，取全局前 2*width
        let mut all: Vec<(f32, usize, u32)> = Vec::new();
        for (b, list) in cands.iter().enumerate() {
            for &(tok, lp) in list {
                if tok != self.eos
                    && repeats_ngram(&self.alive[b].tokens, tok, self.no_repeat_ngram)
                {
                    continue;
                }
                all.push((self.alive[b].score + lp, b, tok));
            }
        }
        all.sort_by(|x, y| cmp_desc(x.0, y.0));
        all.truncate(2 * self.width);

        let mut next: Vec<(f32, usize, u32)> = Vec::with_capacity(self.width);
        for (rank, &(score, parent, tok)) in all.iter().enumerate() {
            if tok == self.eos {
                // 只有排在前 width 的 eos 才算有效完成（与 HF 一致）
                if rank < self.width {
                    let hyp = Hyp {
                        tokens: self.alive[parent].tokens.clone(),
                        score,
                    };
                    let norm = self.normalized(score, hyp.tokens.len());
                    self.finished.push((norm, hyp));
                }
            } else if next.len() < self.width {
                next.push((score, parent, tok));
            }
            if next.len() >= self.width && rank + 1 >= self.width {
                break;
            }
        }
        self.finished.sort_by(|a, b| cmp_desc(a.0, b.0));
        self.finished.truncate(self.width);
        self.steps += 1;

        if next.is_empty() {
            self.done = true;
            return Advance::Done;
        }
        let new_alive: Vec<Hyp> = next
            .iter()
            .map(|&(score, parent, tok)| {
                let mut tokens = self.alive[parent].tokens.clone();
                tokens.push(tok);
                Hyp { tokens, score }
            })
            .collect();

        if self.steps >= self.max_new {
            // 到达长度上限：存活束也参与评比
            for h in &new_alive {
                let norm = self.normalized(h.score, h.tokens.len());
                self.finished.push((norm, h.clone()));
            }
            self.finished.sort_by(|a, b| cmp_desc(a.0, b.0));
            self.finished.truncate(self.width);
            self.done = true;
            return Advance::Done;
        }
        // 早停：已有 width 条完成，且最差完成假设不劣于最优存活束的当前归一化得分。
        // 存活束按「已生成长度」归一化（不含结束位的 +1），与 purebeam.py 的 `max(ns)/(step+1)**lp` 一致；
        // 完成假设仍按「长度+1」归一化（结束位计入长度）。
        if self.finished.len() >= self.width {
            let worst_finished = self.finished.last().map_or(f32::MIN, |f| f.0);
            let best_alive = new_alive
                .iter()
                .map(|h| h.score / (h.tokens.len() as f32).powf(self.length_penalty))
                .fold(f32::MIN, f32::max);
            if worst_finished >= best_alive {
                self.done = true;
                return Advance::Done;
            }
        }
        let parents = next.iter().map(|n| n.1).collect();
        let tokens = next.iter().map(|n| n.2).collect();
        self.alive = new_alive;
        Advance::Continue { parents, tokens }
    }

    /// 取当前最优结果（无完成假设时退回最优存活束）。
    ///
    /// # 返回
    /// 不含 eos 的 token 序列。
    pub fn best(&self) -> Vec<u32> {
        if let Some((_, h)) = self.finished.first() {
            return h.tokens.clone();
        }
        self.alive
            .iter()
            .max_by(|a, b| {
                cmp_desc(
                    self.normalized(a.score, a.tokens.len()),
                    self.normalized(b.score, b.tokens.len()),
                )
                .reverse()
            })
            .map(|h| h.tokens.clone())
            .unwrap_or_default()
    }
}

/// 判断在 `tokens` 后追加 `tok` 是否会产生此前已出现过的 n-gram。
///
/// # 参数
/// - `tokens`：已生成序列。
/// - `tok`：拟追加的 token。
/// - `n`：n-gram 长度，0 表示不限制（恒为 `false`）。
///
/// # 返回
/// 会重复返回 `true`。
///
/// # 示例
/// ```ignore
/// assert!(repeats_ngram(&[1, 2, 3, 1, 2], 3, 3));
/// ```
pub fn repeats_ngram(tokens: &[u32], tok: u32, n: usize) -> bool {
    if n == 0 || tokens.len() + 1 < n {
        return false;
    }
    let prefix = &tokens[tokens.len() + 1 - n..];
    // 已有的每个长度为 n 的窗口 [prefix..., tok] 是否与 (窗口前 n-1 项, 窗口第 n 项) 一致
    tokens
        .windows(n)
        .any(|w| w[..n - 1] == *prefix && w[n - 1] == tok)
}

/// 对一行 logits 做屏蔽后的 log-softmax，返回对数概率最高的 `k` 项（降序）。
///
/// # 参数
/// - `logits`：一行词表 logits。
/// - `banned`：禁止的 token（不参与归一化，也不会被选中）。
/// - `k`：返回个数。
///
/// # 返回
/// `(token id, 对数概率)` 降序列表；非有限值忽略，可能少于 `k`。
///
/// # 示例
/// ```ignore
/// let top = top_k_log_softmax(&logits, &[65000], 8);
/// ```
pub fn top_k_log_softmax(logits: &[f32], banned: &[i64], k: usize) -> Vec<Candidate> {
    let is_banned = |i: usize| banned.contains(&(i as i64));
    let max = logits
        .iter()
        .enumerate()
        .filter(|&(i, v)| v.is_finite() && !is_banned(i))
        .fold(f32::NEG_INFINITY, |m, (_, &v)| m.max(v));
    if !max.is_finite() || k == 0 {
        return Vec::new();
    }
    let sum: f32 = logits
        .iter()
        .enumerate()
        .filter(|&(i, v)| v.is_finite() && !is_banned(i))
        .map(|(_, &v)| (v - max).exp())
        .sum();
    let log_z = max + sum.ln();

    let mut top: Vec<Candidate> = Vec::with_capacity(k);
    let mut floor = f32::NEG_INFINITY;
    for (i, &v) in logits.iter().enumerate() {
        if !v.is_finite() || is_banned(i) || (top.len() == k && v <= floor) {
            continue;
        }
        if top.len() == k {
            // 替换当前最小项
            if let Some(pos) = top
                .iter()
                .enumerate()
                .min_by(|a, b| a.1.1.total_cmp(&b.1.1))
                .map(|(p, _)| p)
            {
                top[pos] = (i as u32, v);
            }
        } else {
            top.push((i as u32, v));
        }
        if top.len() == k {
            floor = top.iter().map(|c| c.1).fold(f32::INFINITY, f32::min);
        }
    }
    top.sort_by(|a, b| b.1.total_cmp(&a.1));
    top.into_iter().map(|(i, v)| (i, v - log_z)).collect()
}

/// 按父束下标重排行（每行 `row_len` 个元素），用于 KV cache 的 batch 维 gather。
///
/// # 参数
/// - `data`：`rows * row_len` 的扁平数据。
/// - `row_len`：每行元素数（非 0）。
/// - `parents`：新行 `i` 取旧行 `parents[i]`。
///
/// # 返回
/// 重排后的新数据；`row_len` 为 0 或下标越界时返回 `None`。
///
/// # 示例
/// ```ignore
/// let out = gather_rows(&[1.0, 2.0, 3.0, 4.0], 2, &[1, 1]).unwrap();
/// assert_eq!(out, vec![3.0, 4.0, 3.0, 4.0]);
/// ```
pub fn gather_rows(data: &[f32], row_len: usize, parents: &[usize]) -> Option<Vec<f32>> {
    if row_len == 0 || !data.len().is_multiple_of(row_len) {
        return None;
    }
    let rows = data.len() / row_len;
    let mut out = Vec::with_capacity(parents.len() * row_len);
    for &p in parents {
        if p >= rows {
            return None;
        }
        out.extend_from_slice(&data[p * row_len..(p + 1) * row_len]);
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// top-k 结果降序、对数概率归一（全取时 exp 之和为 1），屏蔽项不出现。
    #[test]
    fn top_k_is_sorted_normalized_and_masks() {
        let logits = [1.0, 3.0, 2.0, 100.0];
        let all = top_k_log_softmax(&logits, &[3], 3);
        assert_eq!(all.iter().map(|c| c.0).collect::<Vec<_>>(), vec![1, 2, 0]);
        let p: f32 = all.iter().map(|c| c.1.exp()).sum();
        assert!((p - 1.0).abs() < 1e-5, "{p}");
        let two = top_k_log_softmax(&logits, &[3], 2);
        assert_eq!(two.len(), 2);
        assert_eq!(two[0].0, 1);
    }

    /// 边界：k=0、全被屏蔽、含 NaN/inf。
    #[test]
    fn top_k_edge_cases() {
        assert!(top_k_log_softmax(&[1.0, 2.0], &[], 0).is_empty());
        assert!(top_k_log_softmax(&[1.0, 2.0], &[0, 1], 2).is_empty());
        let r = top_k_log_softmax(&[f32::NAN, f32::NEG_INFINITY, 0.5], &[], 3);
        assert_eq!(r.len(), 1);
        assert_eq!(r[0].0, 2);
        assert!(r[0].1.abs() < 1e-6);
    }

    /// 行重排：复制、交换、越界与 row_len=0。
    #[test]
    fn gather_rows_cases() {
        let d = [1.0, 2.0, 3.0, 4.0];
        assert_eq!(gather_rows(&d, 2, &[1, 1]), Some(vec![3.0, 4.0, 3.0, 4.0]));
        assert_eq!(gather_rows(&d, 2, &[1, 0]), Some(vec![3.0, 4.0, 1.0, 2.0]));
        assert_eq!(gather_rows(&d, 2, &[2]), None);
        assert_eq!(gather_rows(&d, 0, &[0]), None);
        assert_eq!(gather_rows(&d, 3, &[0]), None);
    }

    /// 构造候选（token, 概率）。
    fn c(t: u32, p: f32) -> Candidate {
        (t, p.ln())
    }

    /// 束搜索能找到贪心找不到的更优整体序列：
    /// 贪心第一步选 1(0.6)，但 1 之后只能 eos(0.5)/2(0.5)... 而 3(0.4) 之后 eos 概率 0.95。
    #[test]
    fn beam_beats_greedy() {
        const EOS: u32 = 0;
        let mut bs = BeamSearch::new(2, EOS, 0.0, 8, 0);
        // 第一步：1:0.6, 3:0.4
        let adv = bs.advance(&[vec![c(1, 0.6), c(3, 0.4)]]);
        let Advance::Continue { parents, tokens } = adv else {
            panic!("should continue")
        };
        assert_eq!((parents, tokens), (vec![0, 0], vec![1, 3]));
        // 第二步：束 0（前缀 1）：eos 0.3 / 2 0.3 ；束 1（前缀 3）：eos 0.95
        let adv = bs.advance(&[vec![c(EOS, 0.3), c(2, 0.3)], vec![c(EOS, 0.95), c(2, 0.05)]]);
        // 完成假设：[3] = 0.4*0.95=0.38，[1] = 0.6*0.3=0.18；存活 [1,2] = 0.18
        assert_eq!(adv, Advance::Done);
        assert_eq!(bs.best(), vec![3]);
    }

    /// 达到长度上限时存活束参与评比并结束。
    #[test]
    fn stops_at_max_new() {
        let mut bs = BeamSearch::new(2, 0, 1.0, 1, 0);
        let adv = bs.advance(&[vec![c(5, 0.5), c(6, 0.3), c(7, 0.2)]]);
        assert_eq!(adv, Advance::Done);
        assert_eq!(bs.best(), vec![5]);
        // 结束后再推进恒为 Done
        assert_eq!(bs.advance(&[vec![c(1, 1.0)]]), Advance::Done);
    }

    /// 候选数量与存活束不符、或全是 eos 时安全结束，且 best 不 panic。
    #[test]
    fn degenerate_inputs_end_safely() {
        let mut bs = BeamSearch::new(2, 0, 1.0, 8, 0);
        assert_eq!(bs.advance(&[]), Advance::Done);
        assert!(bs.best().is_empty());

        let mut bs = BeamSearch::new(1, 0, 1.0, 8, 0);
        assert_eq!(bs.advance(&[vec![c(0, 1.0)]]), Advance::Done);
        assert!(bs.best().is_empty());
    }

    /// 束宽 1 等价贪心：每步取最高概率非 eos 项。
    #[test]
    fn width_one_is_greedy() {
        let mut bs = BeamSearch::new(1, 0, 1.0, 8, 0);
        let adv = bs.advance(&[vec![c(4, 0.7), c(9, 0.3)]]);
        assert_eq!(
            adv,
            Advance::Continue {
                parents: vec![0],
                tokens: vec![4]
            }
        );
        assert_eq!(bs.alive_len(), 1);
    }

    /// n-gram 重复检测：命中/未命中/关闭/序列过短。
    #[test]
    fn ngram_repeat_detection() {
        assert!(repeats_ngram(&[1, 2, 3, 1, 2], 3, 3));
        assert!(!repeats_ngram(&[1, 2, 3, 1, 2], 4, 3));
        assert!(repeats_ngram(&[7, 7], 7, 2));
        assert!(!repeats_ngram(&[1, 2, 3, 1, 2], 3, 0));
        assert!(!repeats_ngram(&[1], 1, 3));
        assert!(repeats_ngram(&[5], 5, 1));
    }

    /// 开启 no-repeat 后，会造成重复 n-gram 的候选被剔除。
    #[test]
    fn no_repeat_filters_candidates() {
        let mut bs = BeamSearch::new(1, 0, 1.0, 8, 2);
        // 先生成 5，再生成 6
        for tok in [5u32, 6] {
            let adv = bs.advance(&[vec![c(tok, 0.9)]]);
            assert!(matches!(adv, Advance::Continue { .. }));
        }
        // 此时 tokens=[5,6]；候选 5 会构成二元组 (6,5) ——没出现过，允许；候选 6 构成 (6,6) 也新。
        let adv = bs.advance(&[vec![c(5, 0.9)]]);
        assert!(matches!(adv, Advance::Continue { .. }));
        // tokens=[5,6,5]；候选 6 会重复 (5,6)，被剔除后无候选 -> 结束
        assert_eq!(bs.advance(&[vec![c(6, 0.9)]]), Advance::Done);
    }

    /// 长度惩罚归一化：得分 / (长度+1)^lp，lp 越大越偏向长句（HF 约定）。
    #[test]
    fn length_penalty_normalization() {
        let bs = BeamSearch::new(2, 2, 2.0, 8, 0);
        // 已生成 3 个 token（含结束位共 4）：-8 / 4^2 = -0.5
        assert!((bs.normalized(-8.0, 3) + 0.5).abs() < 1e-6);
        let flat = BeamSearch::new(2, 2, 1.0, 8, 0);
        assert!((flat.normalized(-8.0, 3) + 2.0).abs() < 1e-6);
        // lp=1 偏向短假设，lp=2 让较长但总分更低的假设反超
        assert!(flat.normalized(-5.0, 7) < flat.normalized(-1.0, 1));
        assert!(bs.normalized(-5.0, 7) > bs.normalized(-1.0, 1));
    }

    /// 早停比较与 purebeam.py 一致：存活束按已生成长度（无结束位 +1）归一化，lp>1 时更早收束。
    #[test]
    fn early_stop_matches_purebeam_normalization() {
        let mut bs = BeamSearch::new(1, 2, 2.0, 16, 0);
        let _ = bs.advance(&forced_like(5));
        // 结束符 -2.0 完成（归一化 -2/2^2 = -0.5）；存活束 -2.5，按 2^2 归一化为 -0.625，已不优于完成假设
        let adv = bs.advance(&[vec![(2, -2.0), (4, -2.5)]]);
        assert_eq!(adv, Advance::Done);
        assert_eq!(bs.best(), vec![5]);
    }

    /// 测试辅助：强制位候选，只有一条存活束。
    fn forced_like(token: u32) -> Vec<Vec<Candidate>> {
        vec![vec![(token, 0.0)]]
    }
}
