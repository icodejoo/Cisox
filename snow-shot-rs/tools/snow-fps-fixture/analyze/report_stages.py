#!/usr/bin/env python3
"""把 diag 构建的“录制报告”里各阶段耗时样本汇总成 count/p50/p95/max（毫秒）。

用法: python report_stages.py 驱动脚本输出.txt
"""

import re
import statistics
import sys
from typing import Dict, List

# 单个时长样本，如 19.0627ms / 83.2µs / 0ns / 1.2s。
DURATION = re.compile(r"^([0-9.]+)(ns|µs|us|ms|s)$")
# 各单位换算成毫秒的系数。
UNIT_MS = {"ns": 1e-6, "µs": 1e-3, "us": 1e-3, "ms": 1.0, "s": 1000.0}
# 形如 "阶段名": [样本, ...] 的片段。
STAGE = re.compile(r'"([A-Za-z0-9_.]+)": \[([^\]]*)\]')


def parse_duration(text: str):
    """解析单个时长文本为毫秒；无法解析返回 None。

    示例:
        >>> parse_duration("250µs")
        0.25
    """
    m = DURATION.match(text.strip())
    return float(m.group(1)) * UNIT_MS[m.group(2)] if m else None


def parse_stages(report: str) -> Dict[str, List[float]]:
    """从报告文本抽出 {阶段名: [毫秒样本]}；同名阶段合并。

    参数:
        report: “录制报告”整行文本。
    """
    out: Dict[str, List[float]] = {}
    for name, body in STAGE.findall(report):
        vals = [v for v in (parse_duration(p) for p in body.split(",")) if v is not None]
        if vals:
            out.setdefault(name, []).extend(vals)
    return out


def summarize(stages: Dict[str, List[float]]) -> List[str]:
    """把各阶段排成 count/p50/p95/max/总和 表格行。"""
    lines = [f"{'stage':<40}{'n':>6}{'p50ms':>9}{'p95ms':>9}{'maxms':>9}{'sum_ms':>10}"]
    for name in sorted(stages):
        v = sorted(stages[name])
        p = lambda q: v[min(len(v) - 1, round((len(v) - 1) * q))]
        lines.append(f"{name:<40}{len(v):>6}{statistics.median(v):>9.3f}{p(0.95):>9.3f}{v[-1]:>9.3f}{sum(v):>10.1f}")
    return lines


if __name__ == "__main__":
    text = open(sys.argv[1], encoding="utf-8", errors="replace").read()
    line = next((l for l in text.splitlines() if l.startswith("录制报告")), "")
    print("\n".join(summarize(parse_stages(line))))
