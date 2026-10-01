"""量化翻译模型评测：单进程加载一个 ONNX 版本，翻译 FLORES devtest 前 30 句，记录质量/内存/延迟。

每次调用只评一个（模型 x 会话配置），由外层脚本为每个组合起新进程。
  --config default：ORT 默认会话（arena 开、内存模式开、线程自动）；
  --config tight  ：收紧（enable_cpu_mem_arena=False、enable_mem_pattern=False、intra_op_num_threads=4）。
内存口径：Windows GetProcessMemoryInfo 的 WorkingSetSize；"基线"是导入 torch/transformers/ORT 之后、
创建会话之前的工作集，"增量"=读数-基线（更接近独立 Rust worker 的模型自身占用，但不含 worker 运行时本身）。
翻译期间有采样线程每 20ms 读一次当前工作集，取最大值作为"翻译期峰值"。

用法示例：
    python eval_quant.py --dir E:/models/translate-eval/x-int8 --kind nllb --name x-int8 \
        --pruned-dir E:/models/translate-eval/nllb600m-pruned-main14-fp32 --config default --pairs all
"""
import argparse
import re
import ctypes
import ctypes.wintypes as wt
import json
import os
import statistics
import sys
import threading
import time

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, HERE)
FLORES = "E:/workspaces/Cisox/build/flores/flores200_dataset"
ORIG_DIR = "E:/models/translate-eval/nllb-200-distilled-600M"
BEAMS = 4
MAX_NEW = 256
N = 30
CORE11 = [("zho_Hans", "eng_Latn"), ("eng_Latn", "zho_Hans"), ("eng_Latn", "fra_Latn"), ("fra_Latn", "eng_Latn"),
          ("rus_Cyrl", "eng_Latn"), ("arb_Arab", "eng_Latn"), ("eng_Latn", "spa_Latn"),
          ("zho_Hans", "fra_Latn"), ("rus_Cyrl", "spa_Latn"), ("arb_Arab", "fra_Latn"), ("fra_Latn", "zho_Hans")]
EXTRA4 = [("eng_Latn", "deu_Latn"), ("deu_Latn", "eng_Latn"), ("eng_Latn", "jpn_Jpan"), ("jpn_Jpan", "eng_Latn")]
# FLORES 语言码 -> OPUS-MT mul-mul 目标语言 id（均已在 vocab.json 核实存在）
MARIAN_ID = {"eng_Latn": "eng", "zho_Hans": "cmn_Hans", "fra_Latn": "fra", "spa_Latn": "spa",
             "deu_Latn": "deu", "jpn_Jpan": "jpn"}
PAIR_SETS = {"core11": CORE11, "all": CORE11 + EXTRA4, "un6": CORE11,
             "probe": [CORE11[0], CORE11[1], CORE11[2]],
             "perf": [CORE11[0], CORE11[2]]}  # 性能遍：zho->eng 与 eng->fra


_BYTE_RUN = re.compile(r"(?:<0x[0-9A-Fa-f]{2}>)+")
_BYTE_ONE = re.compile(r"<0x([0-9A-Fa-f]{2})>")


def repair_byte_literals(text):
    """把输出里字面的 `<0xE7><0x8E><0xB0>` 字节转义串还原成 UTF-8 字符（mul-mul 的中日韩输出会带这种串）。"""
    return _BYTE_RUN.sub(lambda m: bytes.fromhex("".join(_BYTE_ONE.findall(m.group(0)))).decode("utf-8", "replace"), text)


class _Counters(ctypes.Structure):
    """Windows PROCESS_MEMORY_COUNTERS 结构。"""
    _fields_ = [("cb", wt.DWORD), ("PageFaultCount", wt.DWORD), ("PeakWorkingSetSize", ctypes.c_size_t),
                ("WorkingSetSize", ctypes.c_size_t), ("QuotaPeakPagedPoolUsage", ctypes.c_size_t),
                ("QuotaPagedPoolUsage", ctypes.c_size_t), ("QuotaPeakNonPagedPoolUsage", ctypes.c_size_t),
                ("QuotaNonPagedPoolUsage", ctypes.c_size_t), ("PagefileUsage", ctypes.c_size_t),
                ("PeakPagefileUsage", ctypes.c_size_t)]


_K32 = ctypes.WinDLL("kernel32", use_last_error=True)
_PSAPI = ctypes.WinDLL("psapi", use_last_error=True)
_K32.GetCurrentProcess.restype = wt.HANDLE
_PSAPI.GetProcessMemoryInfo.argtypes = [wt.HANDLE, ctypes.POINTER(_Counters), wt.DWORD]


def mem_mib():
    """返回本进程 (当前工作集, 峰值工作集, 提交内存) MiB。"""
    c = _Counters()
    c.cb = ctypes.sizeof(c)
    _PSAPI.GetProcessMemoryInfo(_K32.GetCurrentProcess(), ctypes.byref(c), c.cb)
    return c.WorkingSetSize / 1048576, c.PeakWorkingSetSize / 1048576, c.PagefileUsage / 1048576


class Sampler(threading.Thread):
    """后台线程：定时读取当前工作集，记录最大值。"""

    def __init__(self, period=0.02):
        super().__init__(daemon=True)
        self.period, self.max_ws, self._stop_flag = period, 0.0, False

    def run(self):
        """循环采样直到被停止。"""
        while not self._stop_flag:
            self.max_ws = max(self.max_ws, mem_mib()[0])
            time.sleep(self.period)

    def take_max(self):
        """返回自上次调用以来的最大采样值并清零（按语向分段统计峰值）。"""
        m, self.max_ws = self.max_ws, 0.0
        return m

    def stop(self):
        """停止采样。"""
        self._stop_flag = True


def read_flores(code, n=N):
    """读 FLORES devtest 某语言前 n 句。"""
    with open(f"{FLORES}/devtest/{code}.devtest", encoding="utf-8") as f:
        return [l.rstrip("\n") for _, l in zip(range(n), f)]


def make_session_options(config, threads=0, opt_level="all", entries=()):
    """按配置名构造 ORT SessionOptions；返回 (options, 描述 dict)。threads>0 仅用于 default（质量遍并行时限线程）。"""
    import onnxruntime as ort
    so = ort.SessionOptions()
    levels = {"all": ort.GraphOptimizationLevel.ORT_ENABLE_ALL, "extended": ort.GraphOptimizationLevel.ORT_ENABLE_EXTENDED,
              "basic": ort.GraphOptimizationLevel.ORT_ENABLE_BASIC, "disable": ort.GraphOptimizationLevel.ORT_DISABLE_ALL}
    so.graph_optimization_level = levels[opt_level]
    for kv in entries:  # 额外的会话配置项，形如 session.disable_prepacking=1
        k, v = kv.split("=", 1)
        so.add_session_config_entry(k, v)
    if config == "default" and threads > 0:
        so.intra_op_num_threads = threads
    if config == "tight":
        so.enable_cpu_mem_arena = False
        so.enable_mem_pattern = False
        so.intra_op_num_threads = 4
    return so, {"enable_cpu_mem_arena": so.enable_cpu_mem_arena, "enable_mem_pattern": so.enable_mem_pattern,
                "intra_op_num_threads": so.intra_op_num_threads,
                "graph_optimization_level": str(so.graph_optimization_level)}


def load_model(a, so):
    """加载 ORT seq2seq 模型与分词函数，返回 (translate(text, src, tgt)->str)。"""
    import torch
    from export_quant_onnx import patch_normalized_config
    patch_normalized_config()
    from optimum.onnxruntime import ORTModelForSeq2SeqLM
    model = ORTModelForSeq2SeqLM.from_pretrained(
        a.dir,
        use_merged=True, use_cache=True, provider="CPUExecutionProvider", session_options=so, use_io_binding=False)
    if a.kind == "marian":
        from transformers import MarianTokenizer
        tok = MarianTokenizer.from_pretrained(a.dir)

        def tr(text, src, tgt):
            b = tok([f">>{MARIAN_ID[tgt]}<< {text}"], return_tensors="pt", padding=True)
            with torch.inference_mode():
                out = model.generate(**b, num_beams=BEAMS, max_new_tokens=MAX_NEW)
            return repair_byte_literals(tok.batch_decode(out, skip_special_tokens=True)[0])
    elif a.kind == "nllb":
        from transformers import NllbTokenizer
        tok = NllbTokenizer.from_pretrained(ORIG_DIR)

        def tr(text, src, tgt):
            tok.src_lang = src
            b = tok([text], return_tensors="pt", padding=True)
            with torch.inference_mode():
                out = model.generate(**b, forced_bos_token_id=tok.convert_tokens_to_ids(tgt),
                                     num_beams=BEAMS, max_new_tokens=MAX_NEW)
            return tok.batch_decode(out, skip_special_tokens=True)[0]
    else:
        from prune_nllb_vocab import PrunedNllbTokenizer
        tok = PrunedNllbTokenizer(ORIG_DIR, a.pruned_dir, mode="remap")

        def tr(text, src, tgt):
            b = tok.encode([text], src)
            with torch.inference_mode():
                out = model.generate(**b, forced_bos_token_id=tok.lang_id(tgt), num_beams=BEAMS, max_new_tokens=MAX_NEW)
            return tok.decode(out.numpy())[0]
    return tr


def main():
    """入口：加载、翻译、写译文与 metrics。"""
    ap = argparse.ArgumentParser()
    ap.add_argument("--dir", required=True, help="量化后 ONNX 目录")
    ap.add_argument("--kind", choices=["marian", "nllb", "nllb-pruned"], required=True)
    ap.add_argument("--pruned-dir", help="裁剪版 HF 目录（取 id_map.json），kind=nllb-pruned 时必填")
    ap.add_argument("--name", required=True)
    ap.add_argument("--config", choices=["default", "tight"], required=True)
    ap.add_argument("--pairs", choices=sorted(PAIR_SETS), default="all")
    ap.add_argument("--results", default="E:/workspaces/Cisox/materials/translate/results")
    ap.add_argument("--out-json", required=True, help="metrics.json 输出路径")
    ap.add_argument("--hyp-root", help="译文输出根目录（默认 results/<name>；tight 配置建议改到临时目录）")
    ap.add_argument("--limit", type=int, default=N)
    ap.add_argument("--opt-level", choices=["all", "extended", "basic", "disable"], default="all")
    ap.add_argument("--entry", action="append", default=[], help="额外 ORT 会话配置项 key=value，可重复")
    ap.add_argument("--threads", type=int, default=0, help="default 配置下的 intra_op 线程数（0=自动）")
    a = ap.parse_args()

    t_imp = time.perf_counter()
    import numpy  # noqa: F401
    import onnxruntime as ort
    import torch
    import transformers  # noqa: F401
    torch.set_num_threads(1)  #束搜索的张量操作很小，torch 自动线程只会和 ORT 抢核
    ws_base, _, _ = mem_mib()
    so, so_desc = make_session_options(a.config, a.threads, a.opt_level, a.entry)
    so_desc["entries"] = a.entry
    t0 = time.perf_counter()
    tr = load_model(a, so)
    # 预热：一次短句（让 ORT 完成首次延迟初始化；其耗时计入 first_call，不计入逐句统计）
    t1 = time.perf_counter()
    tr("Hello.", "eng_Latn", "fra_Latn")
    first_call = time.perf_counter() - t1
    load_s = t1 - t0
    ws_load, peak_load, commit_load = mem_mib()
    sampler = Sampler()
    sampler.start()
    pairs = PAIR_SETS[a.pairs]
    hyp_root = a.hyp_root or os.path.join(a.results, a.name)
    metrics = {"name": a.name, "config": a.config, "ort_version": ort.__version__, "session": so_desc,
               "beams": BEAMS, "n": a.limit, "load_seconds": round(load_s, 2), "first_call_seconds": round(first_call, 2),
               "ws_baseline_mib": round(ws_base), "ws_after_load_mib": round(ws_load),
               "peak_during_load_mib": round(peak_load), "commit_after_load_mib": round(commit_load), "dirs": {}}
    lat_all = []
    for src, tgt in pairs:
        srcs = read_flores(src, a.limit)
        hyps, lats = [], []
        for s in srcs:
            t = time.perf_counter()
            hyps.append(tr(s, src, tgt).replace("\n", " "))
            lats.append(time.perf_counter() - t)
        lat_all += lats
        d = os.path.join(hyp_root, f"{src}-{tgt}")
        os.makedirs(d, exist_ok=True)
        refs = read_flores(tgt, a.limit)
        for fn, lines in (("src.txt", srcs), ("ref.txt", refs), ("hyp.txt", hyps)):
            with open(os.path.join(d, fn), "w", encoding="utf-8", newline="\n") as f:
                f.write("\n".join(lines) + "\n")
        metrics["dirs"][f"{src}-{tgt}"] = {"seconds": round(sum(lats), 1), "mean_s": round(statistics.mean(lats), 3),
                                           "peak_ws_mib": round(max(sampler.take_max(), mem_mib()[0])),
                                           "lats": [round(x, 3) for x in lats]}
        print(src, tgt, {k: v for k, v in metrics["dirs"][f"{src}-{tgt}"].items() if k != "lats"}, flush=True)
    sampler.stop()
    ws_end, peak_total, _ = mem_mib()
    sampler.max_ws = max(v["peak_ws_mib"] for v in metrics["dirs"].values())
    q = statistics.quantiles(lat_all, n=100)
    metrics.update({"ws_end_mib": round(ws_end), "peak_total_mib": round(peak_total),
                    "sampled_peak_translate_mib": round(sampler.max_ws),
                    "delta_after_load_mib": round(ws_load - ws_base), "delta_peak_translate_mib": round(sampler.max_ws - ws_base),
                    "lat_mean_s": round(statistics.mean(lat_all), 3), "lat_p50_s": round(statistics.median(lat_all), 3),
                    "lat_p95_s": round(q[94], 3), "sentences": len(lat_all)})
    os.makedirs(os.path.dirname(os.path.abspath(a.out_json)), exist_ok=True)
    with open(a.out_json, "w", encoding="utf-8") as f:
        json.dump(metrics, f, indent=1)
    print(json.dumps({k: v for k, v in metrics.items() if k != "dirs"}))


if __name__ == "__main__":
    main()
