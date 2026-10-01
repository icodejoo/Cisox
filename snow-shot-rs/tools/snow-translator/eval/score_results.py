"""对 results/<模型>/<语向>/{hyp,ref}.txt 统一打分并给出附加诊断（仅标准库，复用 chrf.py）。

口径（目标为 CJK 时官方 chrF++ 会被系统性压低，故并列三个数）：
  - chrF：仅字符 1~6 阶；
  - chrF++(官方)：词级项按空格切词，中文整句算 1 个词，只作参考；
  - chrF++(CJK词)：词级项把 CJK 逐字切开、拉丁/数字连续串保持一词。
附加诊断：无句末标点句数、逗号结尾句数、长度比<0.7 句数；中文目标另报疑似繁体句数（常用繁体字表启发式）。

用法示例：
    python score_results.py --results E:/.../results --models nllb600m-orig-fp32 opusmt-tc-bible-mul-mul-fp32
"""
import argparse
import os
import re
import sys

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from chrf import corpus_chrf  # noqa: E402

CJK_TARGETS = {"zho_Hans", "jpn_Jpan", "kor_Hang"}
CJK_RE = re.compile(r"[\u4e00-\u9fff\u3400-\u4dbf\u3040-\u30ff\uac00-\ud7af]")
TOKEN_RE = re.compile(r"[A-Za-z0-9]+(?:[.'\-][A-Za-z0-9]+)*|[\u4e00-\u9fff\u3400-\u4dbf\u3040-\u30ff\uac00-\ud7af]|\S")
END_PUNCT = set(".!?\u3002\uff01\uff1f\u2026\"\u201d\u2019)\u300d\u300f\u00bb\u061f")
COMMA_END = set(",;\uff0c\u3001\uff1b\u060c")
# 常用繁体独有字，用于估计简体目标里混入繁体的句数（启发式）
TRAD = set("們個這來說為時會對後裡國學從開關東車點間還過見長問題實現體總與書專業種經讓產線選設應當頭動機電場發樣處類歷較話請認識證難險隨樂館點齊")


def cjk_segment(text):
    """把 CJK 逐字切开、拉丁/数字连续串保持一词，用空格连接，供词级 n-gram 使用。"""
    return " ".join(TOKEN_RE.findall(text))


def read_lines(path):
    """读结果文件为行列表。"""
    with open(path, encoding="utf-8") as f:
        return f.read().rstrip("\n").split("\n")


def diagnostics(hyps, refs, tgt):
    """返回附加诊断 dict。"""
    nopunct = comma = short = trad = 0
    for h, r in zip(hyps, refs):
        s = h.strip()
        if not s or s[-1] not in END_PUNCT:
            nopunct += 1
        if s and s[-1] in COMMA_END:
            comma += 1
        if len("".join(h.split())) < 0.7 * len("".join(r.split())):
            short += 1
        if tgt == "zho_Hans" and any(c in TRAD for c in h):
            trad += 1
    return {"no_end_punct": nopunct, "comma_end": comma, "len_ratio_lt_0.7": short, "suspect_trad": trad}


def score_pair(hyps, refs, tgt):
    """返回一个语向的各口径分数与诊断。"""
    out = {"chrF": corpus_chrf(hyps, refs, word_order=0), "chrF++": corpus_chrf(hyps, refs)}
    if tgt in CJK_TARGETS:
        out["chrF++cjk"] = corpus_chrf([cjk_segment(h) for h in hyps], [cjk_segment(r) for r in refs])
    out.update(diagnostics(hyps, refs, tgt))
    return out


def main():
    """入口：打印 markdown 表。"""
    ap = argparse.ArgumentParser()
    ap.add_argument("--results", required=True)
    ap.add_argument("--models", nargs="+", required=True)
    a = ap.parse_args()
    print("| 模型 | 语向 | chrF | chrF++(官方) | chrF++(CJK词) | 无句末标点 | 逗号结尾 | 长度比<0.7 | 疑似繁体 |")
    print("|---|---|---|---|---|---|---|---|---|")
    for m in a.models:
        root = os.path.join(a.results, m)
        for d in sorted(os.listdir(root)):
            p = os.path.join(root, d)
            if not os.path.isdir(p):
                continue
            tgt = d.split("-")[1]
            s = score_pair(read_lines(os.path.join(p, "hyp.txt")), read_lines(os.path.join(p, "ref.txt")), tgt)
            cjk = f"{s['chrF++cjk']:.2f}" if "chrF++cjk" in s else "-"
            print(f"| {m} | {d} | {s['chrF']:.2f} | {s['chrF++']:.2f} | {cjk} | {s['no_end_punct']} | {s['comma_end']} | "
                  f"{s['len_ratio_lt_0.7']} | {s['suspect_trad'] if tgt == 'zho_Hans' else '-'} |")


if __name__ == "__main__":
    main()
