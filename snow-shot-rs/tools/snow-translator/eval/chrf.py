"""chrF / chrF++ 的纯标准库实现（语料级，聚合 n-gram 统计）。

参数与 sacrebleu 默认一致：字符 n-gram 到 6、词 n-gram 到 2（chrF++）、beta=2。
**未与 sacrebleu 对拍**（本机无法联网安装），差异见 eval/README 与评测文档。

用法示例：
    python chrf.py hyp.txt ref.txt          # 输出 chrF++
    from chrf import corpus_chrf
    corpus_chrf(["a b"], ["a b"])            # -> 100.0
"""
from __future__ import annotations

import string
import sys
from collections import Counter

CHAR_ORDER = 6   # 字符 n-gram 最大阶
WORD_ORDER = 2   # 词 n-gram 最大阶（0 即普通 chrF）
BETA = 2.0       # recall 权重
EPS = 1e-16      # 与 sacrebleu 相同的平滑常数
PUNCT = set(string.punctuation)  # 仅 ASCII 标点，与 sacrebleu 一致


def char_ngrams(text: str, n: int) -> Counter:
    """去掉全部空白后统计字符 n-gram（中文天然按字符）。

    参数：text 句子；n 阶数。返回：n-gram 计数。
    """
    s = "".join(text.split())
    return Counter(s[i:i + n] for i in range(len(s) - n + 1))


def split_words(text: str) -> list[str]:
    """按空白切词，并把词首或词尾的单个 ASCII 标点拆成独立词。

    参数：text 句子。返回：词列表。中文无空格时整句视为一个词。
    """
    out: list[str] = []
    for w in text.split():
        if len(w) == 1:
            out.append(w)
        elif w[-1] in PUNCT:
            out += [w[:-1], w[-1]]
        elif w[0] in PUNCT:
            out += [w[0], w[1:]]
        else:
            out.append(w)
    return out


def word_ngrams(text: str, n: int) -> Counter:
    """统计词 n-gram。

    参数：text 句子；n 阶数。返回：n-gram 计数。
    """
    ws = split_words(text)
    return Counter(tuple(ws[i:i + n]) for i in range(len(ws) - n + 1))


def sentence_stats(hyp: str, ref: str, char_order: int = CHAR_ORDER,
                   word_order: int = WORD_ORDER) -> list[tuple[int, int, int]]:
    """单句各阶 (假设数, 参考数, 命中数)，先字符阶后词阶。

    参数：hyp 译文；ref 参考；两个阶数上限。返回：统计列表。
    """
    stats = []
    for n in range(1, char_order + 1):
        h, r = char_ngrams(hyp, n), char_ngrams(ref, n)
        stats.append((sum(h.values()), sum(r.values()), sum((h & r).values())))
    for n in range(1, word_order + 1):
        h, r = word_ngrams(hyp, n), word_ngrams(ref, n)
        stats.append((sum(h.values()), sum(r.values()), sum((h & r).values())))
    return stats


def f_score(stats: list[tuple[int, int, int]], beta: float = BETA) -> float:
    """由聚合统计算 chrF 分：每阶先算 F，再按有效阶数取平均，乘 100。

    参数：stats 各阶统计；beta recall 权重。返回：0~100 的分数。
    """
    factor = beta ** 2
    total, effective = 0.0, 0
    for n_hyp, n_ref, n_match in stats:
        prec = n_match / n_hyp if n_hyp > 0 else EPS
        rec = n_match / n_ref if n_ref > 0 else EPS
        denom = factor * prec + rec
        total += (1 + factor) * prec * rec / denom if denom > 0 else EPS
        if n_hyp > 0 and n_ref > 0:
            effective += 1
    return 100.0 * total / effective if effective else 0.0


def corpus_chrf(hyps: list[str], refs: list[str], char_order: int = CHAR_ORDER,
                word_order: int = WORD_ORDER) -> float:
    """语料级 chrF++：逐句累加各阶统计后统一计算。

    参数：hyps 译文列表；refs 参考列表（等长）；阶数上限。返回：0~100 的分数。
    """
    if len(hyps) != len(refs):
        raise ValueError("hyp 与 ref 行数不一致")
    agg = [(0, 0, 0)] * (char_order + word_order)
    for h, r in zip(hyps, refs):
        agg = [tuple(a + b for a, b in zip(x, y))
               for x, y in zip(agg, sentence_stats(h, r, char_order, word_order))]
    return f_score(agg)


def main() -> None:
    """命令行入口：对两个逐行对齐的文本文件打分。"""
    with open(sys.argv[1], encoding="utf-8") as f:
        hyps = f.read().split("\n")
    with open(sys.argv[2], encoding="utf-8") as f:
        refs = f.read().split("\n")
    n = min(len(hyps), len(refs))
    print(f"chrF++ = {corpus_chrf(hyps[:n], refs[:n]):.2f}")


if __name__ == "__main__":
    main()
