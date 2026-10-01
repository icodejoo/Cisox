"""用 snow-translator worker 的 JSON 行协议逐句评测 opus-mt：chrF++、延迟、内存。

仅标准库；Windows 内存用 ctypes 读取（与 worker 自报数字互相印证）。
用法（在 eval 目录；先 prepare_models.py finalize，FLORES 解压到 build/flores）：
    python run_eval.py --dirs zh-en en-zh --beams 1 4 --runs 2
产出：CSV（build/flores/results-*.csv）与 materials/translate/results/<方向>/ 下的 hyp/ref/src/meta。
"""
from __future__ import annotations

import argparse
import csv
import ctypes
import json
import os
import statistics
import subprocess
import time
from ctypes import wintypes

import chrf

REPO = os.path.abspath(os.path.join(os.path.dirname(__file__), "../../../.."))
WORKER = os.path.join(REPO, "snow-shot-rs/tools/snow-translator/target/release/snow-translator.exe")
ORT_DLL = os.path.join(REPO, ".cache/ortdll/onnxruntime/capi/onnxruntime.dll")
FLORES = os.path.join(REPO, "build/flores/flores200_dataset/devtest")
OUT_ROOT = os.path.join(REPO, "materials/translate/results")
MODELS_ROOT = os.environ.get("SNOW_MODELS_ROOT", "E:/models/translate")
# FLORES 文件名前缀与 worker 语言码
FLORES_CODE = {"zh": "zho_Hans", "en": "eng_Latn", "fr": "fra_Latn", "es": "spa_Latn", "ru": "rus_Cyrl", "ar": "arb_Arab"}
WORKER_CODE = {"zh": "zh-CN"}
MIB = 1024 * 1024
ENGINE = "opus-mt (Helsinki-NLP) via Xenova ONNX int8, snow-translator worker (ort)"


class PMCEx(ctypes.Structure):
    """Win32 PROCESS_MEMORY_COUNTERS_EX。"""
    _fields_ = [("cb", wintypes.DWORD), ("PageFaultCount", wintypes.DWORD),
                ("PeakWorkingSetSize", ctypes.c_size_t), ("WorkingSetSize", ctypes.c_size_t),
                ("QuotaPeakPagedPoolUsage", ctypes.c_size_t), ("QuotaPagedPoolUsage", ctypes.c_size_t),
                ("QuotaPeakNonPagedPoolUsage", ctypes.c_size_t), ("QuotaNonPagedPoolUsage", ctypes.c_size_t),
                ("PagefileUsage", ctypes.c_size_t), ("PeakPagefileUsage", ctypes.c_size_t),
                ("PrivateUsage", ctypes.c_size_t)]


class MemStatus(ctypes.Structure):
    """Win32 MEMORYSTATUSEX。"""
    _fields_ = [("dwLength", wintypes.DWORD), ("dwMemoryLoad", wintypes.DWORD),
                ("ullTotalPhys", ctypes.c_ulonglong), ("ullAvailPhys", ctypes.c_ulonglong),
                ("ullTotalPageFile", ctypes.c_ulonglong), ("ullAvailPageFile", ctypes.c_ulonglong),
                ("ullTotalVirtual", ctypes.c_ulonglong), ("ullAvailVirtual", ctypes.c_ulonglong),
                ("ullAvailExtendedVirtual", ctypes.c_ulonglong)]


def proc_mem(pid: int) -> dict:
    """外部读取进程内存（字节）：当前/峰值工作集与私有字节。

    参数：pid 进程号。返回：字典；进程已退出返回空字典。
    """
    k32 = ctypes.WinDLL("kernel32", use_last_error=True)
    k32.OpenProcess.restype = wintypes.HANDLE
    h = k32.OpenProcess(0x1000 | 0x0400 | 0x0010, False, pid)  # QUERY_LIMITED | QUERY | VM_READ
    if not h:
        return {}
    c = PMCEx()
    c.cb = ctypes.sizeof(c)
    psapi = ctypes.WinDLL("psapi")
    psapi.GetProcessMemoryInfo.argtypes = [wintypes.HANDLE, ctypes.POINTER(PMCEx), wintypes.DWORD]
    ok = psapi.GetProcessMemoryInfo(h, ctypes.byref(c), c.cb)
    k32.CloseHandle(h)
    return {"ws": c.WorkingSetSize, "peak": c.PeakWorkingSetSize, "private": c.PrivateUsage} if ok else {}


def sys_avail_mib() -> float:
    """系统可用物理内存（MiB），用于判断卸载后是否回收。"""
    s = MemStatus()
    s.dwLength = ctypes.sizeof(s)
    ctypes.WinDLL("kernel32").GlobalMemoryStatusEx(ctypes.byref(s))
    return s.ullAvailPhys / MIB


def pct(values: list[float], p: float) -> float:
    """最近邻分位数。参数：values 样本；p 0~100。返回：分位值。"""
    v = sorted(values)
    return v[min(len(v) - 1, int(round(p / 100 * (len(v) - 1))))]


def read_flores(code: str, n: int) -> list[str]:
    """读取 FLORES devtest 前 n 句。参数：code 两字母语言码；n 句数。返回：句子列表。"""
    with open(f"{FLORES}/{FLORES_CODE[code]}.devtest", encoding="utf-8") as f:
        return [line.rstrip("\n") for line in f][:n]


class Worker:
    """对 snow-translator 子进程的行协议封装。"""

    def __init__(self) -> None:
        """拉起进程并读取 ready 事件。"""
        env = dict(os.environ, SNOW_ORT_DYLIB=ORT_DLL)
        t0 = time.perf_counter()
        self.p = subprocess.Popen([WORKER], stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                                  stderr=subprocess.DEVNULL, env=env)
        evt = self.recv()
        assert evt["evt"] == "ready", evt
        self.spawn_ms = (time.perf_counter() - t0) * 1000
        self.pid = self.p.pid

    def send(self, cmd: dict) -> None:
        """发送一条命令。参数：cmd 命令字典。"""
        self.p.stdin.write((json.dumps(cmd, ensure_ascii=False) + "\n").encode("utf-8"))
        self.p.stdin.flush()

    def recv(self) -> dict:
        """读取一条事件。返回：事件字典。"""
        line = self.p.stdout.readline()
        if not line:
            raise RuntimeError("worker 提前退出")
        return json.loads(line.decode("utf-8"))

    def call(self, cmd: dict) -> tuple[dict, float]:
        """发送命令并等待一条事件。返回：(事件, 往返毫秒)。"""
        t = time.perf_counter()
        self.send(cmd)
        evt = self.recv()
        return evt, (time.perf_counter() - t) * 1000


def run_once(direction: str, beams: int, sents: list[str]) -> dict:
    """跑一轮：启动 worker、加载、逐句翻译、取内存、卸载。

    参数：direction 如 zh-en；beams 束宽；sents 源句。返回：度量字典（含 hyps）。
    """
    src, tgt = direction.split("-")
    mdir = f"{MODELS_ROOT}/opus-mt-{direction}-onnx-int8"
    avail_before = sys_avail_mib()
    w = Worker()
    r = {"direction": direction, "beams": beams, "spawn_ms": w.spawn_ms, "errors": 0}
    evt, load_wall = w.call({"cmd": "load", "model_dir": mdir, "src": WORKER_CODE.get(src, src),
                             "tgt": WORKER_CODE.get(tgt, tgt)})
    if evt["evt"] != "loaded":
        w.p.kill()
        return {**r, "fatal": json.dumps(evt, ensure_ascii=False)}
    r.update(load_ms=evt["load_ms"], load_wall_ms=load_wall, ws_loaded_mib=evt["mem_bytes"] / MIB)
    ext = proc_mem(w.pid)
    r.update(ext_ws_loaded_mib=ext.get("ws", 0) / MIB, ext_private_loaded_mib=ext.get("private", 0) / MIB)
    hyps, lat = [], []
    for i, s in enumerate(sents):
        evt, ms = w.call({"cmd": "translate", "id": i, "text": s, "num_beams": beams})
        if evt["evt"] == "result":
            hyps.append(evt["texts"][0].replace("\n", " "))
        else:
            hyps.append("")
            r["errors"] += 1
            r["last_error"] = json.dumps(evt, ensure_ascii=False)
        lat.append(ms)
    pong, _ = w.call({"cmd": "ping"})
    ext = proc_mem(w.pid)
    r.update(first_translate_ms=lat[0], first_total_ms=w.spawn_ms + load_wall + lat[0],
             hot_p50_ms=statistics.median(lat[1:]), hot_p95_ms=pct(lat[1:], 95), hot_mean_ms=statistics.mean(lat[1:]),
             ws_after_mib=pong["mem_bytes"] / MIB, peak_mib=pong["peak_bytes"] / MIB,
             ext_ws_after_mib=ext.get("ws", 0) / MIB, ext_peak_mib=ext.get("peak", 0) / MIB,
             ext_private_after_mib=ext.get("private", 0) / MIB, all_lat_ms=lat, hyps=hyps)
    w.send({"cmd": "unload"})
    r["unload_evt"] = w.recv()["evt"]
    r["exit_code"] = w.p.wait(timeout=15)
    time.sleep(0.5)
    r.update(avail_before_mib=avail_before, avail_after_exit_mib=sys_avail_mib())
    return r


def main() -> None:
    """命令行入口：遍历方向与束宽，写 CSV 与结果文本。"""
    ap = argparse.ArgumentParser()
    ap.add_argument("--dirs", nargs="+", required=True)
    ap.add_argument("--beams", nargs="+", type=int, default=[1])
    ap.add_argument("--runs", type=int, default=2)
    ap.add_argument("--n", type=int, default=50)
    ap.add_argument("--tag", default=time.strftime("%Y%m%d-%H%M%S"))
    ap.add_argument("--note", default="", help="测量时的后台状态说明，写入 meta")
    ap.add_argument("--out-root", default=OUT_ROOT, help="结果文本根目录（变体实验请指到别处）")
    a = ap.parse_args()
    csv_path = os.path.join(REPO, f"build/flores/results-{a.tag}.csv")
    rows = []
    for d in a.dirs:
        src, tgt = d.split("-")
        sents, refs = read_flores(src, a.n), read_flores(tgt, a.n)
        size = sum(os.path.getsize(f"{MODELS_ROOT}/opus-mt-{d}-onnx-int8/{f}") for f in
                   ["onnx/encoder_model_int8.onnx", "onnx/decoder_model_merged_int8.onnx", "tokenizer.json"]) / MIB
        out_dir = f"{a.out_root}/{d}"
        os.makedirs(out_dir, exist_ok=True)
        for fn, lines in (("src.txt", sents), ("ref.txt", refs)):
            with open(f"{out_dir}/{fn}", "w", encoding="utf-8", newline="\n") as f:
                f.write("\n".join(lines) + "\n")
        for beams in a.beams:
            for run in range(1, a.runs + 1):
                r = run_once(d, beams, sents)
                if "fatal" in r:
                    print(d, "FATAL", r["fatal"])
                    rows.append({**r, "run": run, "note": a.note})
                    break
                r["chrf"] = chrf.corpus_chrf(r["hyps"], refs)
                r.update(run=run, model_mib=size, tag=a.tag, note=a.note)
                print(f"{d} beam={beams} run={run} chrF++={r['chrf']:.2f} load={r['load_wall_ms']:.0f}ms "
                      f"first={r['first_total_ms']:.0f}ms p50={r['hot_p50_ms']:.0f} p95={r['hot_p95_ms']:.0f} "
                      f"ws={r['ws_after_mib']:.0f} peak={r['peak_mib']:.0f} err={r['errors']}", flush=True)
                hyps = r.pop("hyps")
                lat = r.pop("all_lat_ms")
                name = "hyp.txt" if beams == 1 else f"hyp.beam{beams}.txt"
                with open(f"{out_dir}/{name}", "w", encoding="utf-8", newline="\n") as f:
                    f.write("\n".join(hyps) + "\n")
                with open(f"{out_dir}/meta.{name[:-4]}.json", "w", encoding="utf-8") as f:
                    json.dump({"engine": ENGINE, "model": f"opus-mt-{d}-onnx-int8", "beams": beams, "n": a.n,
                               "flores": "FLORES-200 devtest first N", "tag": a.tag, "note": a.note,
                               "chrf++": r["chrf"], "latencies_ms": lat}, f, ensure_ascii=False, indent=1)
                rows.append(r)
    fields = sorted({k for r in rows for k in r})
    with open(csv_path, "w", encoding="utf-8", newline="") as f:
        wr = csv.DictWriter(f, fieldnames=fields)
        wr.writeheader()
        wr.writerows(rows)
    print("CSV:", csv_path)


if __name__ == "__main__":
    main()
