"""汇总量化评测：读 results/<名>/<语向>/{hyp,ref}.txt 与 build/mt-quant/metrics/*.json，输出 CSV 与 markdown 表。

打分口径（复用 score_results.score_pair，不改动原脚本）：
  - 目标为中/日/韩：主指标 = 字符级 chrF（仅字符 1~6 阶），另列 chrF++(CJK 词) 与官方 chrF++；
  - 其它目标：主指标 = 官方 chrF++。
"主指标"在同一语向内可比；跨语向求平均只是汇总参考，不代表绝对质量。
用法示例：
    python aggregate_quant.py --models opusmt-tc-bible-mul-mul-int8 nllb600m-pruned-un6-ccm-int8
"""
import argparse
import csv
import json
import os
import statistics
import sys

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from eval_quant import CORE11, EXTRA4  # noqa: E402
from score_results import CJK_TARGETS, read_lines, score_pair  # noqa: E402

RESULTS = "E:/workspaces/Cisox/materials/translate/results"
MODELS = "E:/models/translate-eval"
QUANT = "E:/workspaces/Cisox/build/mt-quant"
REF_FP32 = "nllb600m-orig-fp32"
OWN_FP32 = {"nllb600m-orig": "nllb600m-orig-fp32", "nllb600m-pruned-main14": "nllb600m-pruned-main14-fp32",
            "nllb600m-pruned-main14-oracle": "nllb600m-pruned-main14-oracle-fp32"}
ORIG_V, ORIG_PARAMS = 256206, 615073792


def pair_name(p):
    """语向目录名。"""
    return f"{p[0]}-{p[1]}"


def main_score(s, tgt):
    """主指标：CJK 目标取字符级 chrF，其余取官方 chrF++。"""
    return s["chrF"] if tgt in CJK_TARGETS else s["chrF++"]


def score_model(name):
    """返回 {语向: 打分 dict}；缺目录的语向跳过。"""
    out = {}
    for p in CORE11 + EXTRA4:
        d = os.path.join(RESULTS, name, pair_name(p))
        if not os.path.exists(os.path.join(d, "hyp.txt")):
            continue
        out[pair_name(p)] = score_pair(read_lines(os.path.join(d, "hyp.txt")), read_lines(os.path.join(d, "ref.txt")), p[1])
        out[pair_name(p)]["main"] = main_score(out[pair_name(p)], p[1])
    return out


def mean_main(scores, pairs):
    """给定语向集合上的主指标均值；若任一语向缺失返回 None。"""
    vals = [scores[pair_name(p)]["main"] for p in pairs if pair_name(p) in scores]
    return statistics.mean(vals) if len(vals) == len(pairs) else None


def delta_common(sc, other):
    """两个结果在共同语向（核心 11 向内）上的主指标均值差 sc - other；无共同语向返回 None。"""
    common = [p for p in CORE11 if pair_name(p) in sc and pair_name(p) in other]
    if not common:
        return None
    return statistics.mean(sc[pair_name(p)]["main"] - other[pair_name(p)]["main"] for p in common)


def disk_mib(name):
    """encoder + merged decoder 的磁盘体积（MiB），不含分词器/配置。"""
    d = os.path.join(MODELS, name)
    fs = [os.path.join(d, f) for f in ("encoder_model.onnx", "decoder_model_merged.onnx")]
    return sum(os.path.getsize(f) for f in fs if os.path.exists(f)) / 1048576 if all(os.path.exists(f) for f in fs) else None


def vocab_info(name):
    """返回 (V, 参数量)：NLLB 按 V 推算，mul-mul 取已知值。"""
    if "mul-mul" in name:
        return 69667, 247766051
    base = name.rsplit("-int", 1)[0]
    if base == "nllb600m-orig":
        return ORIG_V, ORIG_PARAMS
    cfg = os.path.join(MODELS, base + "-fp32", "config.json")
    v = json.load(open(cfg))["vocab_size"]
    return v, ORIG_PARAMS - (ORIG_V - v) * 1024


def load_metrics(name, cfg):
    """读某配置的 metrics，缺失返回 None。"""
    p = f"{QUANT}/metrics/{name}.{cfg}.json"
    return json.load(open(p)) if os.path.exists(p) else None


def summarize(names):
    """生成汇总行列表（每个模型一个 dict）。"""
    ref = score_model(REF_FP32)
    rows = []
    for n in names:
        sc = score_model(n)
        v, params = vocab_info(n)
        own = OWN_FP32.get(n.rsplit("-int", 1)[0])
        own_sc = score_model(own) if own else None
        md, mt = load_metrics(n, "perf-default"), load_metrics(n, "perf-tight")
        core = mean_main(sc, CORE11)
        r = {"name": n, "V": v, "params_M": round(params / 1e6, 1), "disk_MiB": round(disk_mib(n) or 0),
             "core11_main": core, "d_vs_orig_fp32": delta_common(sc, ref),
             "d_vs_own_fp32": delta_common(sc, own_sc) if own_sc else None,
             "extra4_main": mean_main(sc, EXTRA4)}
        for tag, m in (("def", md), ("tight", mt)):
            if m:
                r[f"{tag}_load_s"] = m["load_seconds"]
                r[f"{tag}_ws_load_delta"] = m["delta_after_load_mib"]
                r[f"{tag}_ws_load_abs"] = m["ws_after_load_mib"]
                r[f"{tag}_ws_peak_delta"] = m["delta_peak_translate_mib"]
                r[f"{tag}_ws_peak_abs"] = m["sampled_peak_translate_mib"]
                r[f"{tag}_lat_mean"], r[f"{tag}_lat_p50"], r[f"{tag}_lat_p95"] = m["lat_mean_s"], m["lat_p50_s"], m["lat_p95_s"]
        if md:
            r["ort"] = md["ort_version"]
        rows.append(r)
    return rows


def fmt(x, nd=1):
    """格式化数字；None 显示 -。"""
    return "-" if x is None else (f"{x:.{nd}f}" if isinstance(x, float) else str(x))


def main():
    """入口：打印 markdown 表并写 CSV。"""
    ap = argparse.ArgumentParser()
    ap.add_argument("--models", nargs="+", required=True)
    ap.add_argument("--csv", default=f"{QUANT}/summary.csv")
    ap.add_argument("--detail", action="store_true", help="另打印逐语向/CJK/诊断/性能明细表")
    a = ap.parse_args()
    rows = summarize(a.models)
    keys = sorted({k for r in rows for k in r}, key=lambda k: (k != "name", k))
    with open(a.csv, "w", encoding="utf-8", newline="") as f:
        w = csv.DictWriter(f, fieldnames=keys)
        w.writeheader()
        w.writerows(rows)
    with open(a.csv.replace("summary", "pairs"), "w", encoding="utf-8", newline="") as f:  # 逐语向明细
        w = csv.writer(f)
        w.writerow(["model", "pair", "main", "chrF", "chrF++", "chrF++cjk", "no_end_punct", "comma_end", "len_ratio_lt_0.7", "suspect_trad"])
        for n in a.models + [REF_FP32]:
            for k, v in score_model(n).items():
                w.writerow([n, k, round(v["main"], 2), round(v["chrF"], 2), round(v["chrF++"], 2),
                            round(v["chrF++cjk"], 2) if "chrF++cjk" in v else "", v["no_end_punct"], v["comma_end"],
                            v["len_ratio_lt_0.7"], v["suspect_trad"]])
    print("| 版本 | V | 参数M | 磁盘MiB | 核心11向主指标均值 | Δ原版fp32 | Δ自身fp32 | 额外4向均值 | 加载s(默认) | 加载后WS增量(默认/收紧) | 翻译期峰值WS增量(默认/收紧) | 每句延迟均值 p50 p95 (默认) | 每句延迟均值(收紧) |")
    print("|---|---|---|---|---|---|---|---|---|---|---|---|---|")
    for r in rows:
        print(f"| {r['name']} | {r['V']} | {r['params_M']} | {r['disk_MiB']} | {fmt(r['core11_main'], 2)} | {fmt(r['d_vs_orig_fp32'], 2)} | "
              f"{fmt(r['d_vs_own_fp32'], 2)} | {fmt(r['extra4_main'], 2)} | {fmt(r.get('def_load_s'))} | "
              f"{fmt(r.get('def_ws_load_delta'), 0)}/{fmt(r.get('tight_ws_load_delta'), 0)} | "
              f"{fmt(r.get('def_ws_peak_delta'), 0)}/{fmt(r.get('tight_ws_peak_delta'), 0)} | "
              f"{fmt(r.get('def_lat_mean'), 2)} {fmt(r.get('def_lat_p50'), 2)} {fmt(r.get('def_lat_p95'), 2)} | "
              f"{fmt(r.get('tight_lat_mean'), 2)} |")
    if a.detail:
        print_detail(a.models)


def short(n):
    """表格里的短名。"""
    return (n.replace("nllb600m-pruned-", "").replace("nllb600m-", "").replace("opusmt-tc-bible-", "")
            .replace("-fp32", " fp32"))


def print_detail(names):
    """打印逐语向主指标表、CJK 目标三口径表、诊断表、性能明细表（markdown）。"""
    allm = names + [REF_FP32]
    sc = {n: score_model(n) for n in allm}
    pairs = CORE11 + EXTRA4
    print("\n#### 逐语向主指标（CJK 目标=字符级 chrF，其余=官方 chrF++）\n")
    print("| 语向 | " + " | ".join(short(n) for n in allm) + " |")
    print("|---|" + "---|" * len(allm))
    for p in pairs:
        k = pair_name(p)
        print(f"| {k} | " + " | ".join(fmt(sc[n][k]["main"], 1) if k in sc[n] else "-" for n in allm) + " |")
    print("\n#### CJK 目标三口径：字符级 chrF / chrF++(CJK 逐字词) / 官方 chrF++\n")
    cj = [p for p in pairs if p[1] in CJK_TARGETS]
    print("| 版本 | " + " | ".join(pair_name(p) for p in cj) + " |")
    print("|---|" + "---|" * len(cj))
    for n in allm:
        cells = []
        for p in cj:
            v = sc[n].get(pair_name(p))
            cells.append(f"{v['chrF']:.1f} / {v['chrF++cjk']:.1f} / {v['chrF++']:.1f}" if v else "-")
        print(f"| {short(n)} | " + " | ".join(cells) + " |")
    print("\n#### 诊断（全部语向合计）：句末标点缺失 / 逗号结尾 / 长度比<0.7\n")
    print("| 版本 | 句数 | 无句末标点 | 逗号等结尾（疑似截断） | 长度比<0.7 |")
    print("|---|---|---|---|---|")
    for n in allm:
        v = sc[n].values()
        print(f"| {short(n)} | {30 * len(sc[n])} | {sum(x['no_end_punct'] for x in v)} | {sum(x['comma_end'] for x in v)} | "
              f"{sum(x['len_ratio_lt_0.7'] for x in v)} |")
    print("\n#### 性能遍明细（2 语向 x 30 句 = 60 句，beam=4）\n")
    print("| 版本 | 配置 | 加载s | 加载后WS MiB | 增量 | 翻译期峰值WS MiB | 增量 | 每句均值s | p50 | p95 |")
    print("|---|---|---|---|---|---|---|---|---|---|")
    for n in names:
        for cfg in ("perf-default", "perf-tight"):
            m = load_metrics(n, cfg)
            if m:
                print(f"| {short(n)} | {cfg[5:]} | {m['load_seconds']} | {m['ws_after_load_mib']} | {m['delta_after_load_mib']} | "
                      f"{m['sampled_peak_translate_mib']} | {m['delta_peak_translate_mib']} | {m['lat_mean_s']} | {m['lat_p50_s']} | {m['lat_p95_s']} |")


if __name__ == "__main__":
    main()
