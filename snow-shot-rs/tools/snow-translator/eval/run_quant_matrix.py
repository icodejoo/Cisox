"""量化评测编排：每个（版本 x 会话配置）单独起新进程跑 eval_quant.py，运行前确认机器安静。

安静判据：连续 5 个 1 秒采样的总 CPU 占用都 < 30%，且没有 cargo/rustc/ffmpeg 以及本评测之外的 python 进程
（`C:\\Python314\\python.exe srv.py` 是别人的，忽略）。忙则每 60 秒复查，最多等 30 分钟。
结果：metrics 写到 build/mt-quant/metrics/<name>.<config>.json，已存在则跳过（可续跑）。
用法示例：
    python run_quant_matrix.py --jobs opusmt-tc-bible-mul-mul-int8 nllb600m-pruned-un6-ccm-int8 --configs default tight
"""
import argparse
import json
import os
import subprocess
import sys
import time

HERE = os.path.dirname(os.path.abspath(__file__))
PY = "E:/workspaces/Cisox/build/mt-venv/Scripts/python.exe"
MODELS = "E:/models/translate-eval"
QUANT = "E:/workspaces/Cisox/build/mt-quant"
ORT128 = f"{QUANT}/ort128"
CPU_LIMIT = 30.0
WAIT_STEP = 60
WAIT_MAX = 1800

# 作业表：名称 -> (kind, 裁剪版 HF 目录或 None, 默认配置的语向集)
JOBS = {}
for q in ("int8", "int4", "int8wo"):
    JOBS[f"opusmt-tc-bible-mul-mul-{q}"] = ("marian", None, "all")
    for v, pairs in (("un6", "un6"), ("main14", "all")):
        for kind in ("", "-oracle", "-ccm"):
            JOBS[f"nllb600m-pruned-{v}{kind}-{q}"] = ("nllb-pruned", f"{MODELS}/nllb600m-pruned-{v}{kind}-fp32", pairs)
    JOBS[f"nllb600m-orig-{q}"] = ("nllb", None, "all")
# int2 探针与 HQQ int4 对照（只针对 un6-ccm）
for t in ("int2-rtn-b32", "int2-rtn-b16", "int2-hqq-b32", "int2-hqq-b16", "int4-hqq-b32"):
    JOBS[f"nllb600m-pruned-un6-ccm-{t}"] = ("nllb-pruned", f"{MODELS}/nllb600m-pruned-un6-ccm-fp32", "un6")
# 主 14 语言版的"初版"目录名里没有 "-ccm"/"-oracle"，上面的循环已覆盖；目录与作业名保持一致
PS_CHECK = r"""
$s = (Get-Counter '\Processor(_Total)\% Processor Time' -SampleInterval 1 -MaxSamples 5).CounterSamples | ForEach-Object { [math]::Round($_.CookedValue,1) }
$bad = Get-CimInstance Win32_Process | Where-Object {
  $_.Name -match '^(cargo|rustc|ffmpeg)(\.exe)?$' -or
  ($_.Name -match '^python' -and $_.ExecutablePath -notlike 'C:\Python314\*' -and $_.CommandLine -notmatch 'srv\.py|run_quant_matrix|run_session_exp|run_pb')
} | ForEach-Object { $_.Name + ':' + $_.ProcessId }
@{cpu=$s; bad=@($bad)} | ConvertTo-Json -Compress
"""


def quiet_check():
    """返回 (是否安静, 详情 dict)。"""
    cmd = PS_CHECK
    out = subprocess.run(["powershell", "-NoProfile", "-Command", cmd], capture_output=True, text=True).stdout.strip()
    info = json.loads(out)
    cpu = info["cpu"] if isinstance(info["cpu"], list) else [info["cpu"]]
    info["ok"] = all(c < CPU_LIMIT for c in cpu) and not info["bad"]
    return info["ok"], info


def wait_quiet():
    """等待机器安静，最多 WAIT_MAX 秒；返回最后一次检查详情，超时抛异常。"""
    waited = 0
    while True:
        ok, info = quiet_check()
        if ok:
            return info
        print(f"[busy] {info}; 已等 {waited}s", flush=True)
        if waited >= WAIT_MAX:
            raise TimeoutError(f"机器 {WAIT_MAX}s 内未安静: {info}")
        time.sleep(WAIT_STEP)
        waited += WAIT_STEP


def build_cmd(name, mode, config):
    """构造 eval_quant.py 命令行与输出 json 路径。mode: quality | perf。"""
    kind, pruned, pairs = JOBS[name]
    cmd = [PY, f"{HERE}/eval_quant.py", "--dir", f"{MODELS}/{name}", "--kind", kind, "--name", name, "--config", config]
    if pruned:
        cmd += ["--pruned-dir", pruned]
    if mode == "quick":  # 快速止损：2 个语向，译文写进结果目录
        out_json = f"{QUANT}/metrics/{name}.quick.json"
        cmd += ["--pairs", "perf", "--threads", "6"]
    elif mode == "quality":
        out_json = f"{QUANT}/metrics/{name}.quality.json"
        cmd += ["--pairs", pairs, "--threads", "6"]
    else:
        out_json = f"{QUANT}/metrics/{name}.perf-{config}.json"
        cmd += ["--pairs", "perf", "--hyp-root", f"{QUANT}/perf-hyp/{name}-{config}"]
    return cmd + ["--out-json", out_json], out_json


def free_gb():
    """返回空闲物理内存 GB（PowerShell 取值）。"""
    out = subprocess.run(["powershell", "-NoProfile", "-Command",
                          "[math]::Round((Get-CimInstance Win32_OperatingSystem).FreePhysicalMemory/1MB,1)"],
                         capture_output=True, text=True).stdout.strip()
    return float(out.replace(",", "."))


def warm_cache(name):
    """顺序读一遍模型文件，让 OS 文件缓存处于热态：性能遍的"加载耗时"统一按热缓存口径（冷读取受磁盘/杀软影响，不可复现）。"""
    d = f"{MODELS}/{name}"
    for fn in ("encoder_model.onnx", "decoder_model_merged.onnx"):
        with open(f"{d}/{fn}", "rb") as f:
            while f.read(8 << 20):
                pass


def run_one(name, mode, config, force=False):
    """运行一个（版本 x 遍 x 配置）；质量遍不做安静检查，性能遍必须安静。"""
    cmd, out_json = build_cmd(name, mode, config)
    if os.path.exists(out_json) and not force:
        print("skip", out_json, flush=True)
        return
    if not os.path.exists(f"{MODELS}/{name}/DONE"):
        print("skip（量化产物未完成）", name, flush=True)
        return
    if mode == "perf":
        warm_cache(name)
    info = wait_quiet() if mode == "perf" else {"free_gb": free_gb()}
    env = dict(os.environ, PYTHONPATH=ORT128)
    os.makedirs(f"{QUANT}/logs", exist_ok=True)
    t = time.time()
    with open(f"{QUANT}/logs/{name}.{mode}-{config}.log", "w", encoding="utf-8") as lf:
        rc = subprocess.run(cmd, env=env, stdout=lf, stderr=subprocess.STDOUT).returncode
    el = time.time() - t
    print(f"{name} {mode} {config} rc={rc} {el:.0f}s", flush=True)
    with open(f"{QUANT}/timings_eval.txt", "a", encoding="utf-8") as f:
        f.write(f"{name} {mode} {config} rc={rc} {el:.0f}s\n")
    if rc == 0 and os.path.exists(out_json):
        m = json.load(open(out_json))
        m["pre_check"], m["wall_seconds"] = info, round(el)
        json.dump(m, open(out_json, "w"), indent=1)


def main():
    """命令行入口：quality 并行（-P），perf 串行。"""
    from concurrent.futures import ThreadPoolExecutor
    ap = argparse.ArgumentParser()
    ap.add_argument("mode", choices=["quality", "perf", "quick"])
    ap.add_argument("--jobs", nargs="+", required=True)
    ap.add_argument("--configs", nargs="+", default=["default", "tight"], help="仅 perf 遍使用")
    ap.add_argument("-P", type=int, default=3, help="质量遍并发数")
    ap.add_argument("--force", action="store_true")
    a = ap.parse_args()
    if a.mode in ("quality", "quick"):
        def task(name):
            while free_gb() < 6:  # 每个进程约占 1.5~3GB，空闲不足就等
                time.sleep(30)
            run_one(name, a.mode, "default", a.force)
        with ThreadPoolExecutor(a.P) as ex:
            list(ex.map(task, a.jobs))
    else:
        for name in a.jobs:
            for cfg in a.configs:
                run_one(name, "perf", cfg, a.force)


if __name__ == "__main__":
    main()
