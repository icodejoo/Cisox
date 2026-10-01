#!/usr/bin/env python3
"""汇总 run-fps-test.ps1 / run-matrix.ps1 的 -Trace 产物：按档位统计过线数、失败归因分布、受干扰轮与停顿同时性。

用法:
    python aggregate_trace.py <根目录> [--name 名称前缀] [--json 汇总.json]

根目录下每个子目录是一轮（-Trace 产物），需含 trace_join.json（trace_join.py 输出），可选 env.json（含 interfered 标记）。
档位取自 env.json 的 size/fps，缺失时取自子目录名 `<名称>-r<轮>-<宽x高>-<fps>-<时间戳>`。

输出每个档位一行：
    过线数、失败轮的归因类别分布（按被丢序号计）、受干扰轮数、剔除受干扰轮后的过线率、
    失败轮的被丢序号有多少落在停顿窗口内（以及其中多环节同时停顿的）。
"""

import argparse
import json
import re
import sys
from pathlib import Path
from typing import Dict, List, Optional, Sequence, Tuple

# 子目录名里的档位：<宽x高>-<fps>。
TIER_IN_NAME = re.compile(r"(\d+x\d+)-(\d+)(?:-|$)")
# 归因类别的显示顺序与简称（与 trace_join.CATEGORIES 一致）。
CATEGORY_SHORT = (
    ("a_dxgi_coalesced", "a"),
    ("b_slot_dropped", "b"),
    ("c_compose_encode_lost", "c"),
    ("d_unattributed", "d"),
    ("e_capture_pool_drop", "e"),
)


def tier_of(run_dir: str, summary: dict, env: Optional[dict]) -> Optional[Tuple[str, int]]:
    """确定一轮的档位 (宽x高, fps)。

    参数:
        run_dir: 子目录名。
        summary: trace_join.json 内容。
        env: env.json 内容或 None。
    返回:
        档位；确定不了返回 None。
    示例:
        >>> tier_of("hw-r1-1920x1080-60-123456", {}, None)
        ('1920x1080', 60)
    """
    size = (env or {}).get("size")
    fps = (env or {}).get("fps") or summary.get("fps")
    match = TIER_IN_NAME.search(run_dir)
    if not size and match:
        size = match.group(1)
    if not fps and match:
        fps = match.group(2)
    if not size or not fps:
        return None
    return str(size), int(round(float(fps)))


def load_runs(root: Path, name: str = "") -> List[dict]:
    """读取根目录下所有轮次。

    参数:
        root: 根目录。
        name: 只取子目录名以它开头的轮次（空表示全部）。
    返回:
        每轮一个字典：dir/tier/passed/interfered/categories/lost/lost_in_stall/lost_in_multi_stage/multi_stage_clusters。
    """
    runs: List[dict] = []
    for joined in sorted(root.glob("*/trace_join.json")):
        folder = joined.parent
        if name and not folder.name.startswith(name):
            continue
        try:
            summary = json.loads(joined.read_text(encoding="utf-8-sig"))
        except (OSError, ValueError):
            continue
        env: Optional[dict] = None
        env_path = folder / "env.json"
        if env_path.exists():
            try:
                env = json.loads(env_path.read_text(encoding="utf-8-sig"))
            except (OSError, ValueError):
                env = None
        runs.append(make_run(folder.name, summary, env))
    return runs


def make_run(run_dir: str, summary: dict, env: Optional[dict]) -> dict:
    """由一轮的 trace_join 摘要与 env 生成汇总用记录。

    参数:
        run_dir: 子目录名。
        summary: trace_join.json 内容。
        env: env.json 内容或 None。
    """
    product = summary.get("product") or {}
    stall = summary.get("lost_in_stall") or {}
    return {
        "dir": run_dir,
        "tier": tier_of(run_dir, summary, env),
        "passed": product.get("passed"),
        "interfered": None if env is None or env.get("interfered") is None else bool(env["interfered"]),
        "categories": summary.get("categories") or {},
        "lost": (summary.get("counts") or {}).get("lost", 0),
        "lost_in_stall": stall.get("any", 0),
        "lost_in_multi_stage": stall.get("multi_stage", 0),
        "multi_stage_clusters": (summary.get("stalls") or {}).get("multi_stage_clusters", 0),
    }


def _rate(passed: int, total: int) -> Optional[float]:
    """过线率；没有轮次返回 None。"""
    return passed / total if total else None


def summarize(runs: Sequence[dict]) -> Dict[str, dict]:
    """按档位汇总（另含 "ALL" 合计）。

    参数:
        runs: load_runs / make_run 的结果；passed 为 None（缺成品统计）的轮次只计入 unknown。
    返回:
        {"<宽x高>@<fps>": 汇总字典, ..., "ALL": 汇总字典}。
    示例:
        >>> r = make_run("x-r1-1920x1080-60-1", {"product": {"passed": False}, "categories": {"a_dxgi_coalesced": 2}, "counts": {"lost": 2}}, {"interfered": False})
        >>> summarize([r])["1920x1080@60"]["failed_categories"]["a_dxgi_coalesced"]
        2
    """
    groups: Dict[str, List[dict]] = {}
    for run in runs:
        tier = run["tier"]
        key = f"{tier[0]}@{tier[1]}" if tier else "unknown"
        groups.setdefault(key, []).append(run)
    result = {key: _summarize_group(group) for key, group in sorted(groups.items())}
    result["ALL"] = _summarize_group(list(runs))
    return result


def _summarize_group(group: Sequence[dict]) -> dict:
    """汇总一组轮次。"""
    judged = [r for r in group if r["passed"] is not None]
    passed = [r for r in judged if r["passed"]]
    failed = [r for r in judged if not r["passed"]]
    clean = [r for r in judged if r["interfered"] is False]
    clean_passed = [r for r in clean if r["passed"]]
    categories: Dict[str, int] = {}
    for run in failed:
        for cat, n in run["categories"].items():
            categories[cat] = categories.get(cat, 0) + n
    return {
        "runs": len(group),
        "judged": len(judged),
        "passed": len(passed),
        "pass_rate": _rate(len(passed), len(judged)),
        "interfered_runs": sum(1 for r in group if r["interfered"] is True),
        "env_unknown_runs": sum(1 for r in group if r["interfered"] is None),
        "clean_runs": len(clean),
        "clean_passed": len(clean_passed),
        "clean_pass_rate": _rate(len(clean_passed), len(clean)),
        "failed_runs": len(failed),
        "failed_interfered_runs": sum(1 for r in failed if r["interfered"] is True),
        "failed_categories": categories,
        "failed_lost_total": sum(r["lost"] for r in failed),
        "failed_lost_in_stall": sum(r["lost_in_stall"] for r in failed),
        "failed_lost_in_multi_stage": sum(r["lost_in_multi_stage"] for r in failed),
        "failed_runs_with_stall": sum(1 for r in failed if r["lost_in_stall"] > 0),
        "failed_runs_with_multi_stage": sum(1 for r in failed if r["lost_in_multi_stage"] > 0),
    }


def _pct(value: Optional[float]) -> str:
    """百分比文本；None 显示 -。"""
    return "-" if value is None else f"{value * 100:.0f}%"


def format_table(summary: Dict[str, dict]) -> str:
    """把汇总排成文本表格。

    参数:
        summary: summarize 的结果。
    """
    head = f"{'档位':<16}{'过线':>7}{'受干扰轮':>9}{'剔除干扰后':>13}{'失败轮归因(按丢失序号 a/b/c/d/e)':>38}{'丢帧落停顿窗口':>16}{'其中多环节同时停顿':>20}"
    lines = [head]
    for key, s in summary.items():
        cats = "/".join(str(s["failed_categories"].get(full, 0)) for full, _ in CATEGORY_SHORT) if s["failed_runs"] else "-"
        stall = f"{s['failed_lost_in_stall']}/{s['failed_lost_total']}" if s["failed_runs"] else "-"
        multi = f"{s['failed_lost_in_multi_stage']}/{s['failed_lost_total']}" if s["failed_runs"] else "-"
        clean = f"{s['clean_passed']}/{s['clean_runs']} ({_pct(s['clean_pass_rate'])})"
        ok = f"{s['passed']}/{s['judged']}"
        lines.append(f"{key:<16}{ok:>7}{s['interfered_runs']:>9}{clean:>13}{cats:>38}{stall:>16}{multi:>20}")
    lines.append("")
    lines.append("归因: a=DXGI 合并 b=槽选择/队列丢弃 c=合成/编码丢失 d=无法归因 e=采集池丢弃；受干扰判据见 run-fps-test.ps1 的 -Trace 说明；")
    lines.append("剔除干扰后=只统计 env.json 标记为未受干扰的轮次；停顿窗口来自 trace_join（夹具/取帧/合成/送帧/跳槽）。")
    unknown = summary.get("ALL", {}).get("env_unknown_runs", 0)
    if unknown:
        lines.append(f"注意: {unknown} 轮缺 env.json，未计入“剔除干扰后”。")
    return "\n".join(lines)


def main(argv: Optional[Sequence[str]] = None) -> int:
    """命令行入口；没有可汇总的轮次返回 1。

    参数:
        argv: 参数列表，缺省取 sys.argv。
    """
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("root", help="-Trace 产物根目录")
    ap.add_argument("--name", default="", help="只汇总子目录名以该前缀开头的轮次")
    ap.add_argument("--json", help="汇总 JSON 输出路径")
    args = ap.parse_args(argv)
    runs = load_runs(Path(args.root), args.name)
    if not runs:
        print(f"{args.root} 下没有找到含 trace_join.json 的轮次", file=sys.stderr)
        return 1
    summary = summarize(runs)
    print(format_table(summary))
    if args.json:
        Path(args.json).write_text(json.dumps({"tiers": summary, "runs": runs}, ensure_ascii=False, indent=2), encoding="utf-8")
    return 0


if __name__ == "__main__":
    sys.exit(main())
