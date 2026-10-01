"""三模型 fp32 质量参考：NLLB 原版 / NLLB 裁剪版 / OPUS-MT tc-bible-big-mul-mul（直译与英语中转）。

每个模型单独起进程运行（便于取各自的进程峰值内存），译文写到 results/<模型名>/<源>-<目标>/{hyp,ref}.txt。
子命令：
    run      跑一个模型的若干语向并写 metrics.json；
    compare  比较两个结果目录的译文是否逐字一致，并输出 chrF++。
用法示例：
    python eval_fp32_baseline.py run --kind nllb --model-dir E:/models/translate-eval/nllb-200-distilled-600M \
        --name nllb600m-orig-fp32 --results E:/workspaces/Cisox/materials/translate/results
    python eval_fp32_baseline.py compare --results ... --a nllb600m-orig-fp32 --b nllb600m-pruned-main14-fp32
"""
import argparse
import ctypes
import ctypes.wintypes as wt
import json
import os
import sys
import time

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from chrf import corpus_chrf  # noqa: E402

FLORES = "E:/workspaces/Cisox/build/flores/flores200_dataset"
MAIN7 = [("zho_Hans", "eng_Latn"), ("eng_Latn", "zho_Hans"), ("eng_Latn", "fra_Latn"), ("fra_Latn", "eng_Latn"),
         ("rus_Cyrl", "eng_Latn"), ("arb_Arab", "eng_Latn"), ("eng_Latn", "spa_Latn")]
EXTRA4 = [("zho_Hans", "fra_Latn"), ("rus_Cyrl", "spa_Latn"), ("arb_Arab", "fra_Latn"), ("fra_Latn", "zho_Hans")]
# FLORES 语言码 -> OPUS-MT 目标语言 id（已在 vocab.json 核实 >>cmn_Hans<< 等存在）
MARIAN_ID = {"eng_Latn": "eng", "zho_Hans": "cmn_Hans", "fra_Latn": "fra", "spa_Latn": "spa"}
BEAMS = 4
MAX_NEW = 256


class _Counters(ctypes.Structure):
    """Windows PROCESS_MEMORY_COUNTERS 结构。"""
    _fields_ = [("cb", wt.DWORD), ("PageFaultCount", wt.DWORD), ("PeakWorkingSetSize", ctypes.c_size_t),
                ("WorkingSetSize", ctypes.c_size_t), ("QuotaPeakPagedPoolUsage", ctypes.c_size_t),
                ("QuotaPagedPoolUsage", ctypes.c_size_t), ("QuotaPeakNonPagedPoolUsage", ctypes.c_size_t),
                ("QuotaNonPagedPoolUsage", ctypes.c_size_t), ("PagefileUsage", ctypes.c_size_t),
                ("PeakPagefileUsage", ctypes.c_size_t)]


def mem_mib():
    """返回本进程 (当前工作集, 峰值工作集) MiB，取自 Windows GetProcessMemoryInfo。"""
    c = _Counters()
    c.cb = ctypes.sizeof(c)
    k32 = ctypes.WinDLL("kernel32", use_last_error=True)
    k32.GetCurrentProcess.restype = wt.HANDLE
    psapi = ctypes.WinDLL("psapi", use_last_error=True)
    psapi.GetProcessMemoryInfo.argtypes = [wt.HANDLE, ctypes.POINTER(_Counters), wt.DWORD]
    psapi.GetProcessMemoryInfo(k32.GetCurrentProcess(), ctypes.byref(c), c.cb)
    return c.WorkingSetSize / 1048576, c.PeakWorkingSetSize / 1048576


def read_lines(code, n):
    """读 FLORES devtest 某语言前 n 句。"""
    with open(f"{FLORES}/devtest/{code}.devtest", encoding="utf-8") as f:
        return [l.rstrip("\n") for _, l in zip(range(n), f)]


def load_nllb(kind, model_dir, orig_dir, pruned_mode):
    """加载 NLLB 系模型，返回 (translate(texts, src, tgt)->list[str], model)。"""
    import torch
    from transformers import M2M100ForConditionalGeneration, NllbTokenizer
    model = M2M100ForConditionalGeneration.from_pretrained(model_dir, torch_dtype=torch.float32).eval()
    if kind == "nllb":
        tok = NllbTokenizer.from_pretrained(model_dir)

        def tr(texts, src, tgt):
            tok.src_lang = src
            b = tok(texts, return_tensors="pt", padding=True)
            out = model.generate(**b, forced_bos_token_id=tok.convert_tokens_to_ids(tgt),
                                 num_beams=BEAMS, max_new_tokens=MAX_NEW)
            return tok.batch_decode(out, skip_special_tokens=True)
    else:
        from prune_nllb_vocab import PrunedNllbTokenizer
        tok = PrunedNllbTokenizer(orig_dir, model_dir, mode=pruned_mode)

        def tr(texts, src, tgt):
            b = tok.encode(texts, src)
            out = model.generate(**b, forced_bos_token_id=tok.lang_id(tgt), num_beams=BEAMS, max_new_tokens=MAX_NEW)
            return tok.decode(out.numpy())
    return tr, model


def load_opus(model_dir):
    """加载 OPUS-MT mul-mul（MarianMT, fp32），返回 (translate, model)。"""
    import torch
    from transformers import MarianMTModel, MarianTokenizer
    model = MarianMTModel.from_pretrained(model_dir, torch_dtype=torch.float32).eval()
    tok = MarianTokenizer.from_pretrained(model_dir)

    def tr(texts, src, tgt):
        b = tok([f">>{MARIAN_ID[tgt]}<< {t}" for t in texts], return_tensors="pt", padding=True)
        out = model.generate(**b, num_beams=BEAMS, max_new_tokens=MAX_NEW)
        return tok.batch_decode(out, skip_special_tokens=True)
    return tr, model


def translate_dir(tr, src, tgt, srcs, batch):
    """按 batch 翻译整个语向，返回译文列表。"""
    import torch
    hyps = []
    with torch.inference_mode():
        for i in range(0, len(srcs), batch):
            hyps += tr(srcs[i:i + batch], src, tgt)
    return hyps


def write_dir(root, name, pair, hyps, refs, extra=None):
    """写某语向的 hyp.txt/ref.txt（不覆盖已有目录内容）。"""
    d = os.path.join(root, name, f"{pair[0]}-{pair[1]}")
    os.makedirs(d, exist_ok=True)
    for fn, lines in (("hyp.txt", hyps), ("ref.txt", refs), *((extra or {}).items())):
        with open(os.path.join(d, fn), "w", encoding="utf-8", newline="\n") as f:
            f.write("\n".join(l.replace("\n", " ") for l in lines) + "\n")


def cmd_run(a):
    """run 子命令：加载模型、跑语向、写译文与 metrics.json。"""
    import torch
    out_dir = os.path.join(a.results, a.name)
    if os.path.exists(out_dir) and os.listdir(out_dir):
        sys.exit(f"{out_dir} 已存在且非空，拒绝覆盖")
    pairs = {"main7": MAIN7, "extra4": EXTRA4, "all": MAIN7 + EXTRA4}[a.dirs]
    t0 = time.perf_counter()
    if a.kind == "opus":
        tr, model = load_opus(a.model_dir)
    else:
        tr, model = load_nllb(a.kind, a.model_dir, a.orig_dir, a.pruned_mode)
    load_s = time.perf_counter() - t0
    ws_after_load, peak_after_load = mem_mib()
    params = sum(p.numel() for p in {id(p): p for p in model.parameters()}.values())
    metrics = {"name": a.name, "threads": torch.get_num_threads(), "batch": a.batch, "n": a.n, "beams": BEAMS,
               "load_seconds": round(load_s, 2), "params": params, "ws_after_load_mib": round(ws_after_load),
               "peak_after_load_mib": round(peak_after_load), "dirs": {}}
    pivot = None
    if a.pivot:  # 英语中转：先 src->eng，再 eng->tgt
        pivot = tr
    for pair in pairs:
        srcs, refs = read_lines(pair[0], a.n), read_lines(pair[1], a.n)
        t1 = time.perf_counter()
        if pivot:
            mid = translate_dir(tr, pair[0], "eng_Latn", srcs, a.batch)
            hyps = translate_dir(tr, "eng_Latn", pair[1], mid, a.batch)
            extra = {"pivot-eng.txt": mid}
        else:
            hyps, extra = translate_dir(tr, pair[0], pair[1], srcs, a.batch), None
        sec = time.perf_counter() - t1
        write_dir(a.results, a.name, pair, hyps, refs, extra)
        metrics["dirs"][f"{pair[0]}-{pair[1]}"] = {"chrf": round(corpus_chrf(hyps, refs), 2),
                                                    "seconds": round(sec, 1), "sec_per_sentence": round(sec / a.n, 3)}
        print(pair, metrics["dirs"][f"{pair[0]}-{pair[1]}"], flush=True)
    ws, peak = mem_mib()
    metrics["ws_end_mib"], metrics["peak_end_mib"] = round(ws), round(peak)
    with open(os.path.join(out_dir, "metrics.json"), "w", encoding="utf-8") as f:
        json.dump(metrics, f, indent=1)
    print(json.dumps({k: v for k, v in metrics.items() if k != "dirs"}))


def read_txt(root, name, pair, fn):
    """读结果文件为行列表。"""
    p = os.path.join(root, name, f"{pair[0]}-{pair[1]}", fn)
    with open(p, encoding="utf-8") as f:
        return f.read().rstrip("\n").split("\n")


def cmd_compare(a):
    """compare 子命令：统计 A/B 译文逐字一致率，列出差异，并给出 chrF++。"""
    total = same = 0
    for pair in MAIN7 + EXTRA4:
        try:
            ha, hb = read_txt(a.results, a.a, pair, "hyp.txt"), read_txt(a.results, a.b, pair, "hyp.txt")
            ref = read_txt(a.results, a.a, pair, "ref.txt")
        except FileNotFoundError:
            continue
        diff = [i for i, (x, y) in enumerate(zip(ha, hb)) if x != y]
        total += len(ha)
        same += len(ha) - len(diff)
        print(f"{pair[0]}->{pair[1]}: 一致 {len(ha) - len(diff)}/{len(ha)}  chrF++ A={corpus_chrf(ha, ref):.2f} B={corpus_chrf(hb, ref):.2f}")
        for i in diff[: a.show]:
            print(f"   #{i}\n     A: {ha[i]}\n     B: {hb[i]}")
    print(f"总计一致 {same}/{total} = {100 * same / max(total, 1):.1f}%")


def main():
    """命令行入口。"""
    ap = argparse.ArgumentParser()
    sub = ap.add_subparsers(dest="cmd", required=True)
    r = sub.add_parser("run")
    r.add_argument("--kind", choices=["nllb", "nllb-pruned", "opus"], required=True)
    r.add_argument("--model-dir", required=True)
    r.add_argument("--orig-dir", default="E:/models/translate-eval/nllb-200-distilled-600M")
    r.add_argument("--pruned-mode", choices=["remap", "resegment"], default="remap")
    r.add_argument("--name", required=True)
    r.add_argument("--results", required=True)
    r.add_argument("--dirs", choices=["main7", "extra4", "all"], default="all")
    r.add_argument("--pivot", action="store_true", help="经英语中转（仅 opus）")
    r.add_argument("--n", type=int, default=30)
    r.add_argument("--batch", type=int, default=1)
    c = sub.add_parser("compare")
    c.add_argument("--results", required=True)
    c.add_argument("--a", required=True)
    c.add_argument("--b", required=True)
    c.add_argument("--show", type=int, default=50)
    a = ap.parse_args()
    {"run": cmd_run, "compare": cmd_compare}[a.cmd](a)


if __name__ == "__main__":
    main()
