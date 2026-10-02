"""Hy-MT2 内存/速度小探针：低优先级、限线程，翻译 eng→zho 前 N 句，报加载后工作集、峰值、私有内存与 chrF。

用法：python mem_probe.py --dir <模型目录> [--n 5] [--threads 4] [--tgt zho_Hans --src eng_Latn] [--max-new 120]
"""
import argparse
import ctypes
import json
import statistics
import time

import hymt_common as C
from eval_quant import Sampler, mem_mib
from ort_gen import Generator


def set_idle():
    """把当前进程降为 Idle 优先级，避免占满机器。"""
    ctypes.windll.kernel32.SetPriorityClass(ctypes.windll.kernel32.GetCurrentProcess(), 0x40)


def main():
    """入口：加载、翻译前 N 句，打印 JSON 指标。"""
    ap = argparse.ArgumentParser()
    ap.add_argument("--dir", required=True)
    ap.add_argument("--n", type=int, default=5)
    ap.add_argument("--threads", type=int, default=4)
    ap.add_argument("--src", default="eng_Latn")
    ap.add_argument("--tgt", default="zho_Hans")
    ap.add_argument("--max-new", type=int, default=120)
    a = ap.parse_args()
    set_idle()
    base, _, base_priv = mem_mib()
    t0 = time.perf_counter()
    g = Generator(a.dir, a.threads)
    load_s = time.perf_counter() - t0
    ws_load, _, priv_load = mem_mib()
    sampler = Sampler()
    sampler.start()
    srcs = C.read_flores(a.src, a.n)
    lats, outs = [], []
    for s in srcs:
        t = time.perf_counter()
        out = g.generate(g.encode(s, a.tgt), a.max_new, 1.05)
        lats.append(time.perf_counter() - t)
        outs.append(g.tok.decode(out, skip_special_tokens=True))
    sampler.stop()
    ws_end, _, priv_end = mem_mib()
    print(json.dumps({"dir": a.dir, "load_s": round(load_s, 1), "load_delta_mib": round(ws_load - base),
                      "peak_delta_mib": round(sampler.max_ws - base), "end_delta_mib": round(ws_end - base),
                      "priv_load_mib": round(priv_load - base_priv), "priv_end_mib": round(priv_end - base_priv),
                      "lat_mean_s": round(statistics.mean(lats), 2), "outs": outs}, ensure_ascii=False))


if __name__ == "__main__":
    main()
