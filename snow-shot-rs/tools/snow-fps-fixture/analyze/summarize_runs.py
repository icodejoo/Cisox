#!/usr/bin/env python3
"""汇总 run-matrix.ps1 的输出：按档位（分辨率 x 帧率）统计通过次数、帧率/丢帧率/CPU/内存。

用法:
    python summarize_runs.py <输出目录> <名称前缀>

通过线: 30fps 档有效帧率 >= 28.5，60fps 档 >= 56.0，且丢帧率 < 1%。
"""

import re
import statistics
import sys
from pathlib import Path
from typing import Dict, List, Optional

# 各帧率档的有效帧率通过线。
FPS_LINES = {30: 28.5, 60: 56.0}
# 丢帧率上限（百分比）。
DROP_LIMIT_PCT = 1.0
# 档位文件名：<名称>-r<轮>-<宽x高>-<fps>.txt
NAME_PATTERN = re.compile(r"^(?P<name>.+)-r(?P<round>\d+)-(?P<size>\d+x\d+)-(?P<fps>\d+)\.txt$")


def parse_run(text: str) -> Optional[Dict[str, float]]:
    """解析一份 run-fps-test.ps1 的输出。

    参数:
        text: 原始输出文本。
    返回:
        含 fps/drop/cpu/ws/wsmax 的字典；没有分析结果返回 None。
    示例:
        >>> parse_run("有效帧率            : 59.8 fps\\n丢帧率 0.3%\\nperf: cpu(1-core-equiv)=31% ws avg=170MB max=190MB")["fps"]
        59.8
    """

    def grab(pattern: str) -> Optional[str]:
        match = re.search(pattern, text)
        return match.group(1) if match else None

    fps = grab(r"有效帧率\s*:\s*([\d.]+)")
    if fps is None:
        return None
    return {
        "fps": float(fps),
        "drop": float(grab(r"丢帧率\s*([\d.]+)%") or 0.0),
        "cpu": float(grab(r"cpu\(1-core-equiv\)=(\d+)%") or 0.0) / 100.0,
        "ws": float(grab(r"ws avg=(\d+)MB") or 0.0),
        "wsmax": float(grab(r"max=(\d+)MB") or 0.0),
    }


def passed(run: Dict[str, float], fps_target: int) -> bool:
    """这一轮是否达标。

    参数:
        run: parse_run 的结果。
        fps_target: 档位帧率（30 或 60）。
    示例:
        >>> passed({"fps": 59.0, "drop": 0.3}, 60)
        True
    """
    return run["fps"] >= FPS_LINES.get(fps_target, fps_target * 0.95) and run["drop"] < DROP_LIMIT_PCT


def main(argv: List[str]) -> int:
    """命令行入口。

    参数:
        argv: [输出目录, 名称前缀]。
    """
    if len(argv) != 2:
        print(__doc__)
        return 2
    folder, name = Path(argv[0]), argv[1]
    tiers: Dict[tuple, List[Dict[str, float]]] = {}
    for path in sorted(folder.glob(f"{name}-r*.txt")):
        match = NAME_PATTERN.match(path.name)
        if not match or match["name"] != name:
            continue
        run = parse_run(path.read_text(encoding="utf-8", errors="replace"))
        if run is not None:
            tiers.setdefault((match["size"], int(match["fps"])), []).append(run)
    print(f"{'档位':<14}{'通过':>6}{'fps均值':>9}{'fps最小':>9}{'丢帧均值%':>10}{'丢帧最大%':>10}{'CPU核':>7}{'内存均/峰MB':>14}")
    for (size, fps), runs in sorted(tiers.items(), key=lambda kv: (kv[0][0], kv[0][1])):
        ok = sum(1 for r in runs if passed(r, fps))
        print(
            f"{size + '@' + str(fps):<14}{f'{ok}/{len(runs)}':>6}{statistics.mean(r['fps'] for r in runs):>9.2f}"
            f"{min(r['fps'] for r in runs):>9.2f}{statistics.mean(r['drop'] for r in runs):>10.2f}{max(r['drop'] for r in runs):>10.2f}"
            f"{statistics.mean(r['cpu'] for r in runs):>7.2f}{statistics.mean(r['ws'] for r in runs):>8.0f}/{max(r['wsmax'] for r in runs):<5.0f}"
        )
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
