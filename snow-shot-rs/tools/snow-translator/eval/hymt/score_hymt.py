"""对比打分：核心 11 向主指标（CJK 目标用字符级 chrF，其余官方 chrF++）+ 诊断计数，Hy-MT2 对 NLLB 等版本并列。

复用 eval/score_results.py 的口径；额外统计：平均生成 token 数（来自 hyp.partial.jsonl）、啰嗦句数
（含 Note/Translation/解释性开头）、原样复述句数（hyp 与 src 相同）。

用法：python score_hymt.py --models hymt2-1.8b-int4 nllb600m-pruned-main14-ccm-int4 [--base nllb600m-pruned-main14-ccm-int4]
"""
import argparse
import json
import os
import re
import statistics
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, os.path.dirname(HERE))
from score_results import CJK_TARGETS, diagnostics, score_pair, read_lines  # noqa: E402
import hymt_common as C  # noqa: E402

CORE_TAGS = [f"{s}-{t}" for s, t in C.CORE11]
EXTRA_TAGS = ["eng_Latn-jpn_Jpan"]  # 四向对比里的额外语向，不计入核心 11 均值
CHATTY = re.compile(r"(?i)\bnote\b\s*[:：]|^\s*(here is|here's|translation\b|sure|the translation)|注\s*[:：]|翻译\s*[:：]|\(Note|（注")


def main_metric(s, tgt):
    """主指标：CJK 目标取字符级 chrF，其余取官方 chrF++。"""
    return s["chrF"] if tgt in CJK_TARGETS else s["chrF++"]


def load(model, tag):
    """读一个语向的 (src, hyp, ref)；不存在返回 None。"""
    d = os.path.join(C.RESULTS, model, tag)
    if not all(os.path.exists(os.path.join(d, f)) for f in ("src.txt", "hyp.txt", "ref.txt")):
        return None
    return [read_lines(os.path.join(d, f)) for f in ("src.txt", "hyp.txt", "ref.txt")]


def evaluate(model):
    """返回 {tag: 指标 dict}，附 ntok 与诊断。"""
    out = {}
    for tag in CORE_TAGS + EXTRA_TAGS:
        data = load(model, tag)
        if not data:
            continue
        src, hyp, ref = data
        tgt = tag.split("-")[1]
        s = score_pair(hyp, ref, tgt)
        s["main"] = main_metric(s, tgt)
        s["chatty"] = sum(bool(CHATTY.search(h)) for h in hyp)
        s["copy"] = sum(h.strip() == x.strip() for h, x in zip(hyp, src))
        part = os.path.join(C.RESULTS, model, tag, "hyp.partial.jsonl")
        if os.path.exists(part):
            rows = [json.loads(l) for l in open(part, encoding="utf-8") if l.strip()]
            s["ntok"] = statistics.mean(r["ntok"] for r in rows)
            s["maxed"] = sum(r["ntok"] >= 512 for r in rows)
        out[tag] = s
    return out


def main():
    """入口：打印总表、四向对比、诊断计数。"""
    ap = argparse.ArgumentParser()
    ap.add_argument("--models", nargs="+", required=True)
    ap.add_argument("--base", default="nllb600m-pruned-main14-ccm-int4")
    a = ap.parse_args()
    res = {m: evaluate(m) for m in a.models}
    print("| 语向 | " + " | ".join(a.models) + " |")
    print("|---|" + "---|" * len(a.models))
    for tag in CORE_TAGS:
        print(f"| {tag} | " + " | ".join(f"{res[m][tag]['main']:.1f}" if tag in res[m] else "-" for m in a.models) + " |")
    avg = {m: statistics.mean(res[m][t]["main"] for t in CORE_TAGS) for m in a.models if all(t in res[m] for t in CORE_TAGS)}
    print("| 核心11均值 | " + " | ".join(f"{avg[m]:.2f}" if m in avg else "-" for m in a.models) + " |")
    for tag in EXTRA_TAGS:
        print(f"| {tag}（额外） | " + " | ".join(f"{res[m][tag]['main']:.1f}" if tag in res[m] else "-" for m in a.models) + " |")
    print("\n诊断（句数合计，每向 30 句；无标点/逗号结尾/长度比<0.7/啰嗦/复述/到上限/平均token）")
    for m in a.models:
        r = res[m]
        tot = lambda k: sum(v[k] for t, v in r.items() if t in CORE_TAGS)  # noqa: E731
        nt = [v["ntok"] for v in r.values() if "ntok" in v]
        print(f"- {m}: 句数 {30*sum(t in CORE_TAGS for t in r)}, 无句末标点 {tot('no_end_punct')}, 逗号结尾 {tot('comma_end')}, "
              f"长度比<0.7 {tot('len_ratio_lt_0.7')}, 啰嗦 {tot('chatty')}, 复述 {tot('copy')}"
              + (f", 到上限 {tot('maxed')}, 每句均 token {statistics.mean(nt):.1f}" if nt else ""))
    if a.base in res:
        print(f"\n相对 {a.base} 的差（主指标）")
        for m in a.models:
            if m != a.base and m in avg and a.base in avg:
                print(f"- {m}: 11向均值 {avg[m]-avg[a.base]:+.2f}; " + ", ".join(
                    f"{t} {res[m][t]['main']-res[a.base][t]['main']:+.1f}" for t in
                    ("eng_Latn-zho_Hans", "zho_Hans-eng_Latn", "eng_Latn-fra_Latn")))


if __name__ == "__main__":
    main()
