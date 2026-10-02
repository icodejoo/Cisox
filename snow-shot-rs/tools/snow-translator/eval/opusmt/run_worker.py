"""用真实 snow-translator worker 进程翻译评测句，并记录工作集内存与逐句延迟。

做法：起一个全新 worker，`load` 一个 Marian 模型包，逐句 `translate`（每句一个请求），
外部每 10ms 采样 worker 进程的工作集（同 Python 评测用的 GetProcessMemoryInfo）。
译文写到 results/<name>/<src>-<tgt>/hyp.txt，并与 Python 评测的译文（同一目录名的 python 结果）逐句比较。

用法示例：
    python run_worker.py --exe C:/tmp/snow-translator.exe --pack E:/models/translate-eval/opusmt-en-zh-int8-pack \
        --src en --tgt zh-CN --src-file E:/.../results/en-zh/src.txt --name opusmt-en-zh-int8-rust --limit 10
"""
import argparse
import ctypes
import ctypes.wintypes as wt
import json
import os
import statistics
import subprocess
import threading
import time

MIB = 1048576
DEFAULT_ORT = "E:/workspaces/Cisox/build/mt-quant/ort128/onnxruntime/capi/onnxruntime.dll"
RESULTS = "E:/workspaces/Cisox/materials/translate/results"
SAMPLE_PERIOD = 0.01  # 工作集采样周期（秒）


class Counters(ctypes.Structure):
    """Windows PROCESS_MEMORY_COUNTERS 结构。"""
    _fields_ = [("cb", wt.DWORD), ("PageFaultCount", wt.DWORD), ("PeakWorkingSetSize", ctypes.c_size_t),
                ("WorkingSetSize", ctypes.c_size_t), ("QuotaPeakPagedPoolUsage", ctypes.c_size_t),
                ("QuotaPagedPoolUsage", ctypes.c_size_t), ("QuotaPeakNonPagedPoolUsage", ctypes.c_size_t),
                ("QuotaNonPagedPoolUsage", ctypes.c_size_t), ("PagefileUsage", ctypes.c_size_t),
                ("PeakPagefileUsage", ctypes.c_size_t)]


K32 = ctypes.WinDLL("kernel32", use_last_error=True)
PSAPI = ctypes.WinDLL("psapi", use_last_error=True)
K32.OpenProcess.restype = wt.HANDLE
K32.OpenProcess.argtypes = [wt.DWORD, wt.BOOL, wt.DWORD]
PSAPI.GetProcessMemoryInfo.argtypes = [wt.HANDLE, ctypes.POINTER(Counters), wt.DWORD]
PROCESS_QUERY_INFORMATION, PROCESS_VM_READ = 0x0400, 0x0010


def mem_of(handle):
    """返回 (当前工作集, 峰值工作集, 私有提交) MiB。"""
    c = Counters()
    c.cb = ctypes.sizeof(c)
    PSAPI.GetProcessMemoryInfo(handle, ctypes.byref(c), c.cb)
    return c.WorkingSetSize / MIB, c.PeakWorkingSetSize / MIB, c.PagefileUsage / MIB


class Sampler(threading.Thread):
    """后台线程：定时读取目标进程工作集，记录最大值。"""

    def __init__(self, handle):
        super().__init__(daemon=True)
        self.handle, self.max_ws, self.stop_flag = handle, 0.0, False

    def run(self):
        """循环采样直到停止。"""
        while not self.stop_flag:
            self.max_ws = max(self.max_ws, mem_of(self.handle)[0])
            time.sleep(SAMPLE_PERIOD)


def main():
    """入口：起 worker、加载、逐句翻译、统计。"""
    ap = argparse.ArgumentParser()
    ap.add_argument("--exe", required=True)
    ap.add_argument("--pack", required=True)
    ap.add_argument("--src", required=True)
    ap.add_argument("--tgt", required=True)
    ap.add_argument("--src-file", required=True)
    ap.add_argument("--name", required=True)
    ap.add_argument("--limit", type=int, default=10)
    ap.add_argument("--beams", type=int, default=0, help="0=用清单缺省")
    ap.add_argument("--py-hyp", help="Python 评测译文文件，用于逐句比较")
    ap.add_argument("--out-json", required=True)
    a = ap.parse_args()
    with open(a.src_file, encoding="utf-8") as f:
        srcs = f.read().rstrip("\n").split("\n")[:a.limit]
    env = dict(os.environ, SNOW_ORT_DYLIB=os.environ.get("SNOW_ORT_DYLIB", DEFAULT_ORT))
    p = subprocess.Popen([a.exe], stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.DEVNULL, env=env)

    def call(cmd):
        p.stdin.write((json.dumps(cmd, ensure_ascii=False) + "\n").encode("utf-8"))
        p.stdin.flush()
        return json.loads(p.stdout.readline().decode("utf-8"))

    ready = json.loads(p.stdout.readline().decode("utf-8"))
    assert ready["evt"] == "ready", ready
    h = K32.OpenProcess(PROCESS_QUERY_INFORMATION | PROCESS_VM_READ, False, p.pid)
    idle = mem_of(h)
    t0 = time.perf_counter()
    r = call({"cmd": "load", "model_dir": a.pack.replace("\\", "/"), "src": a.src, "tgt": a.tgt})
    assert r["evt"] == "loaded", r
    load_wall = time.perf_counter() - t0
    after_load = mem_of(h)
    sampler = Sampler(h)
    sampler.start()
    hyps, lats, worker_ms = [], [], []
    for i, s in enumerate(srcs):
        cmd = {"cmd": "translate", "id": i, "texts": [s]}
        if a.beams:
            cmd["num_beams"] = a.beams
        t = time.perf_counter()
        r = call(cmd)
        lats.append(time.perf_counter() - t)
        assert r["evt"] == "result", r
        worker_ms.append(r["elapsed_ms"])
        hyps.append(r["texts"][0])
    sampler.stop_flag = True
    end = mem_of(h)
    peak_translate = max(sampler.max_ws, end[0])
    call({"cmd": "unload"})
    p.wait(timeout=10)
    d = os.path.join(RESULTS, a.name, f"{a.src}-{a.tgt}")
    os.makedirs(d, exist_ok=True)
    with open(os.path.join(d, "hyp.txt"), "w", encoding="utf-8", newline="\n") as f:
        f.write("\n".join(hyps) + "\n")
    same = None
    if a.py_hyp:
        with open(a.py_hyp, encoding="utf-8") as f:
            py = f.read().rstrip("\n").split("\n")[:a.limit]
        same = [x == y for x, y in zip(hyps, py)]
    m = {"name": a.name, "pack": a.pack, "n": len(srcs), "beams": a.beams or "manifest",
         "idle_ws_mib": round(idle[0], 1), "after_load_ws_mib": round(after_load[0], 1),
         "after_load_private_mib": round(after_load[2], 1), "peak_during_load_mib": round(after_load[1], 1),
         "peak_translate_ws_mib": round(peak_translate, 1), "end_ws_mib": round(end[0], 1),
         "end_private_mib": round(end[2], 1), "process_peak_ws_mib": round(end[1], 1),
         "load_seconds": round(load_wall, 2), "first_sentence_ms": round(lats[0] * 1000),
         "lat_mean_ms": round(statistics.mean(lats) * 1000), "lat_p50_ms": round(statistics.median(lats) * 1000),
         "lat_max_ms": round(max(lats) * 1000), "worker_elapsed_ms": worker_ms,
         "identical_to_python": sum(same) if same else None, "hyps": hyps}
    with open(a.out_json, "w", encoding="utf-8") as f:
        json.dump(m, f, ensure_ascii=False, indent=1)
    print(json.dumps({k: v for k, v in m.items() if k != "hyps"}, ensure_ascii=False))
    if same:
        for i, ok in enumerate(same):
            if not ok:
                print(f"#{i} src: {srcs[i]}\n  rust  : {hyps[i]}\n  python: {py[i]}")


if __name__ == "__main__":
    main()
