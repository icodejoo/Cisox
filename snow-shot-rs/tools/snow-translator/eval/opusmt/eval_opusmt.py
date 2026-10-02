"""OPUS-MT 单语向（en-zh / zh-en）量化档评测：FLORES devtest 前 N 句，记录 chrF、内存、延迟。

复用 eval_quant.py 的内存采样、会话选项与 FLORES 读取；每次调用只评一个（模型目录 x 方向）。
chrF 口径：字符 1~6 阶、去空白、beta=2（chrF，不含词级项）；同时给出 chrF++ 供参考。
内存口径同 eval_quant：增量 = 读数 - 导入 torch/ORT 之后、建会话之前的工作集。

用法示例：
    python eval_opusmt.py --dir E:/models/translate-eval/opusmt-en-zh-int8 --direction en-zh \
        --name opusmt-en-zh-int8 --threads 4 --out-json E:/.../metrics.json
"""
import argparse
import json
import os
import statistics
import sys
import time

HERE = os.path.dirname(os.path.abspath(__file__))
EVAL = os.path.dirname(HERE)
sys.path.insert(0, EVAL)

# 方向 -> (源 FLORES 码, 目标 FLORES 码, 目标语言前缀)；zh-en 不需要前缀
DIRECTIONS = {"en-zh": ("eng_Latn", "zho_Hans", ">>cmn_Hans<< "), "zh-en": ("zho_Hans", "eng_Latn", "")}
RESULTS = "E:/workspaces/Cisox/materials/translate/results"


def main():
    """入口：加载模型、翻译、打分、写译文与 metrics。"""
    ap = argparse.ArgumentParser()
    ap.add_argument("--dir", required=True, help="ONNX 目录（encoder_model.onnx + decoder_model_merged.onnx + spm/vocab）")
    ap.add_argument("--direction", choices=sorted(DIRECTIONS), required=True)
    ap.add_argument("--name", required=True)
    ap.add_argument("--threads", type=int, default=4)
    ap.add_argument("--beams", type=int, default=4)
    ap.add_argument("--limit", type=int, default=30)
    ap.add_argument("--start", type=int, default=0, help="起始句下标（复核用，如 30 取 31~60 句）")
    ap.add_argument("--prefix", help="覆盖目标语言前缀（如 '>>cmn_Hant<< '）")
    ap.add_argument("--results", default=RESULTS)
    ap.add_argument("--out-json", required=True)
    a = ap.parse_args()

    import eval_quant as eq
    from chrf import corpus_chrf
    import numpy  # noqa: F401
    import onnxruntime as ort
    import torch
    torch.set_num_threads(1)
    ws_base, _, _ = eq.mem_mib()
    src_code, tgt_code, prefix = DIRECTIONS[a.direction]
    if a.prefix is not None:
        prefix = a.prefix
    so, so_desc = eq.make_session_options("default", a.threads)
    from export_quant_onnx import patch_normalized_config
    patch_normalized_config()
    from optimum.onnxruntime import ORTModelForSeq2SeqLM
    from transformers import MarianTokenizer
    t0 = time.perf_counter()
    model = ORTModelForSeq2SeqLM.from_pretrained(a.dir, use_merged=True, use_cache=True, provider="CPUExecutionProvider",
                                                 session_options=so, use_io_binding=False)
    tok = MarianTokenizer.from_pretrained(a.dir)

    def tr(text):
        b = tok([prefix + text], return_tensors="pt", padding=True)
        with torch.inference_mode():
            out = model.generate(**b, num_beams=a.beams, max_new_tokens=256)
        return tok.batch_decode(out, skip_special_tokens=True)[0]

    t1 = time.perf_counter()
    tr("Hello." if a.direction == "en-zh" else "你好。")
    first = time.perf_counter() - t1
    ws_load, peak_load, _ = eq.mem_mib()
    sampler = eq.Sampler()
    sampler.start()
    n_end = a.start + a.limit
    srcs = eq.read_flores(src_code, n_end)[a.start:]
    refs = eq.read_flores(tgt_code, n_end)[a.start:]
    hyps, lats = [], []
    for s in srcs:
        t = time.perf_counter()
        hyps.append(tr(s).replace("\n", " "))
        lats.append(time.perf_counter() - t)
    sampler.stop()
    peak_tr = max(sampler.max_ws, eq.mem_mib()[0])
    d = os.path.join(a.results, a.name, f"{src_code}-{tgt_code}")
    os.makedirs(d, exist_ok=True)
    for fn, lines in (("src.txt", srcs), ("ref.txt", refs), ("hyp.txt", hyps)):
        with open(os.path.join(d, fn), "w", encoding="utf-8", newline="\n") as f:
            f.write("\n".join(lines) + "\n")
    q = statistics.quantiles(lats, n=100)
    m = {"name": a.name, "dir": a.dir, "direction": a.direction, "ort_version": ort.__version__, "session": so_desc,
         "beams": a.beams, "prefix": prefix, "start": a.start, "n": len(srcs),
         "chrF": round(corpus_chrf(hyps, refs, word_order=0), 2), "chrF++": round(corpus_chrf(hyps, refs), 2),
         "load_seconds": round(t1 - t0, 2), "first_call_seconds": round(first, 2),
         "ws_baseline_mib": round(ws_base), "ws_after_load_mib": round(ws_load), "peak_during_load_mib": round(peak_load),
         "delta_after_load_mib": round(ws_load - ws_base), "sampled_peak_translate_mib": round(peak_tr),
         "delta_peak_translate_mib": round(peak_tr - ws_base), "lat_mean_s": round(statistics.mean(lats), 3),
         "lat_p50_s": round(statistics.median(lats), 3), "lat_p95_s": round(q[94], 3)}
    os.makedirs(os.path.dirname(os.path.abspath(a.out_json)), exist_ok=True)
    with open(a.out_json, "w", encoding="utf-8") as f:
        json.dump(m, f, indent=1)
    print(json.dumps(m, ensure_ascii=False))


if __name__ == "__main__":
    main()
