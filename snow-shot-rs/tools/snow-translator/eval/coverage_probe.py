"""统计裁剪词表对 devtest 源文与原版译文的覆盖：未保留片段占比、含未保留片段的句数。

用法示例：
    python coverage_probe.py --orig E:/models/translate-eval/nllb-200-distilled-600M \
        --pruned E:/models/translate-eval/nllb600m-pruned-main14-fp32 --results E:/.../results --name nllb600m-orig-fp32
"""
import argparse
import json
import os
import sys

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from eval_fp32_baseline import MAIN7, EXTRA4, read_lines, read_txt  # noqa: E402


def main():
    """入口：对每个语向报告源文侧与译文侧的未保留片段占比。"""
    ap = argparse.ArgumentParser()
    ap.add_argument("--orig", required=True)
    ap.add_argument("--pruned", required=True)
    ap.add_argument("--results", required=True)
    ap.add_argument("--name", required=True, help="原版译文所在结果目录名")
    ap.add_argument("--n", type=int, default=30)
    a = ap.parse_args()
    from transformers import NllbTokenizer
    tok = NllbTokenizer.from_pretrained(a.orig)
    keep = set(json.load(open(os.path.join(a.pruned, "id_map.json")))["new_to_orig"])
    print("| 语向 | 源文片段数 | 源文未保留% | 含未保留的源句 | 译文片段数 | 译文未保留% | 含未保留的译句 |")
    print("|---|---|---|---|---|---|---|")
    tot = [0, 0, 0, 0, 0, 0]
    for pair in MAIN7 + EXTRA4:
        src = read_lines(pair[0], a.n)
        hyp = read_txt(a.results, a.name, pair, "hyp.txt")
        row = []
        for texts in (src, hyp):
            n = miss = sent = 0
            for t in texts:
                ids = tok(t, add_special_tokens=False)["input_ids"]
                m = sum(1 for i in ids if i not in keep)
                n, miss, sent = n + len(ids), miss + m, sent + (m > 0)
            row += [n, miss, sent]
        for i, v in enumerate(row):
            tot[i] += v
        print(f"| {pair[0]}->{pair[1]} | {row[0]} | {100 * row[1] / row[0]:.2f} | {row[2]}/{a.n} | {row[3]} | {100 * row[4] / row[3]:.2f} | {row[5]}/{a.n} |")
    print(f"| 合计 | {tot[0]} | {100 * tot[1] / tot[0]:.2f} | {tot[2]} | {tot[3]} | {100 * tot[4] / tot[3]:.2f} | {tot[5]} |")


if __name__ == "__main__":
    main()
