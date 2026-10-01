#!/usr/bin/env python3
"""ETW 桌面合成与呈现解析：把 tracerpt 转出的 CSV 变成"夹具每个 Present 有没有被 DWM 合成并上屏"的真值表。

输入（均为 etw-capture.ps1 的产物）:
  --csv    tracerpt <etl> -of CSV 的输出（Dwm-Core / DxgKrnl / DXGI 三个 provider）
  --clock  <etl>.clock.json，含会话启动/停止时各取的 (QPC, 挂钟 unix 微秒) 锚点
  --pid    夹具进程号（过滤 DXGI Present 与 DxgKrnl 呈现令牌）
  --fixture 夹具 frames.csv（seq,submit_ns,flush_ns,unix_us），可选；给了就把每个 Present 对上 seq
输出: 文本报告（--report）、JSON 摘要（--json）、逐 Present 的显示表 CSV（--table）。

用到的事件（事件号取自本机 manifest；tracerpt 对它们只给"位置化载荷"，没有字段名，下标是实测确认的）:
  DxgKrnl 17   VSyncDPC          载荷 [适配器, VidPnTargetId, ?, ?, vsync 计数, vsync 的 QPC, ...]
  DxgKrnl 215  PresentHistoryDetailed Start（pid=应用）载荷 [?, TokenData 指针, 模型, ?, 令牌号, ...]
  DxgKrnl 173  PresentHistory Stop（DWM 取走令牌）载荷 [?, TokenData 指针, 模型, ?, 令牌号]
  DxgKrnl 259/386 MMIOFlipMultiPlaneOverlay（DWM 的硬件翻页）
  DxgKrnl 318  DWMVsyncCountWait 载荷 [?, DWM 等待的 vsync 计数, ...]
  Dwm-Core 10/12 PROCESS_FRAME Start/Stop（DWM 每个 vblank 的合成帧）；15/16 SCHEDULE_PRESENT Start/Stop（DWM 真正提交呈现）
  DXGI 42/43   Present Start/Stop  42 载荷 [交换链, 标志, 同步间隔, ...]，DWM 自己的试探呈现标志为 DXGI_PRESENT_TEST

时钟：Clock-Time = A + QPC（100ns 单位、A 恒定）。A 由 VSyncDPC 载荷内的 QPC 反推；再用锚点把 QPC 线性映射到挂钟 unix 微秒。

配对链（应用 Present -> 上屏 vsync）及可信度见 pair_presents 的注释；单测见 test_etw_join.py。
用法示例:
  python etw_join.py --csv etw.csv --clock x.etl.clock.json --pid 1234 --fixture run.frames.csv --report etw_report.txt --json etw_join.json --table etw_presents.csv
"""

import argparse
import bisect
import csv
import io
import json
import statistics
import sys
from dataclasses import dataclass, field
from typing import Dict, List, Optional, Sequence, Tuple

PROV_DWM = "Microsoft-Windows-Dwm-Core"
PROV_DXG = "Microsoft-Windows-DxgKrnl"
PROV_DXGI = "Microsoft-Windows-DXGI"
FT_PER_SEC = 10_000_000  # FILETIME 单位：100ns
ANOMALY_FACTOR = 1.5  # 间隔超过 1.5 个周期算异常
TEST_PRESENT_TAG = "DXGI_PRESENT_TEST"  # DWM 自己的试探 Present 的标志串
PICKUP_SLACK_US = 50  # 令牌事件相对 Present 调用/返回允许的时间松弛
MATCH_TOL_US = 2000  # frames.csv 与 Present 返回时刻的最大配对误差

# 事件号 -> 名称（取自 wevtutil gp 的系统 manifest，仅列报告里用到的）。
EVENT_NAMES = {
    (PROV_DWM, 10): "SCHEDULE_PROCESS_FRAME", (PROV_DWM, 12): "SCHEDULE_PROCESS_FRAME",
    (PROV_DWM, 15): "SCHEDULE_PRESENT", (PROV_DWM, 16): "SCHEDULE_PRESENT",
    (PROV_DWM, 1): "SCHEDULE_FRAMEINFO", (PROV_DWM, 63): "PROCESSPRESENTHISTORY",
    (PROV_DWM, 65): "PROCESSPRESENTHISTORY", (PROV_DWM, 79): "SCHEDULE_VBLANK_LOOP",
    (PROV_DWM, 80): "SCHEDULE_VBLANK_LOOP", (PROV_DWM, 141): "SCHEDULE_DXGI_PRESENT_SUCCEEDED",
    (PROV_DWM, 204): "SCHEDULE_PRESENT_STATS_DELTAS", (PROV_DWM, 303): "SCHEDULE_FRAME_VSYNCDEADLINES",
    (PROV_DXG, 17): "VSyncDPC", (PROV_DXG, 181): "VSyncInterrupt", (PROV_DXG, 273): "VSyncDPCMultiPlane",
    (PROV_DXG, 318): "DWMVsyncCountWait", (PROV_DXG, 319): "DWMVsyncSignal",
    (PROV_DXG, 215): "PresentHistoryDetailed", (PROV_DXG, 171): "PresentHistory", (PROV_DXG, 172): "PresentHistory",
    (PROV_DXG, 173): "PresentHistory", (PROV_DXG, 184): "Present", (PROV_DXG, 252): "FlipMultiPlaneOverlay",
    (PROV_DXG, 259): "MMIOFlipMultiPlaneOverlay", (PROV_DXG, 386): "MMIOFlipMultiPlaneOverlay3",
    (PROV_DXGI, 42): "Present", (PROV_DXGI, 43): "Present",
}


@dataclass
class Ev:
    """一条 ETW 事件：provider、事件号、Start/Stop/Info、进程/线程号、FILETIME(100ns)、位置化载荷。"""
    prov: str
    eid: int
    kind: str
    pid: int
    tid: int
    ft: int
    payload: List[str]


def _int(s: str) -> Optional[int]:
    """宽松解析整数（支持 0x 前缀），失败返回 None。"""
    try:
        return int(s.strip(), 0)
    except (ValueError, AttributeError):
        return None


def parse_tracerpt_csv(text: str) -> Tuple[List[Ev], Dict[str, str]]:
    """解析 tracerpt 的 CSV，返回 (事件列表, 头信息 {lost_events 等})；EventTrace 头事件不进事件列表。

    Example:
        >>> evs, _ = parse_tracerpt_csv(sample_csv())  # doctest: +SKIP
    """
    reader = csv.reader(io.StringIO(text), skipinitialspace=True)
    header = next(reader, None)
    if not header:
        return [], {}
    cols = [c.strip() for c in header]
    i_clock, i_pid, i_tid, i_ud = cols.index("Clock-Time"), cols.index("PID"), cols.index("TID"), cols.index("User Data")
    i_name, i_type, i_id = cols.index("Event Name"), cols.index("Type"), cols.index("Event ID")
    events: List[Ev] = []
    info: Dict[str, str] = {}
    for row in reader:
        if len(row) <= i_clock:
            continue
        name = row[i_name].strip()
        if name == "EventTrace":
            if row[i_type].strip() == "Header" and len(row) > i_ud + 11:
                info["header_events_lost"] = row[i_ud + 11].strip()  # 头事件载荷第 12 项为 EventsLost
            continue
        eid, pid, tid, ft = _int(row[i_id]), _int(row[i_pid]), _int(row[i_tid]), _int(row[i_clock])
        if eid is None or ft is None:
            continue
        events.append(Ev(name, eid, row[i_type].strip(), pid or 0, tid or 0, ft, [c.strip() for c in row[i_ud:]]))
    return events, info


def percentile(sorted_vals: Sequence[float], p: float) -> float:
    """已排序序列的 p 分位（0~100，最近邻）。"""
    if not sorted_vals:
        return 0.0
    return sorted_vals[min(len(sorted_vals) - 1, int(round(p / 100 * (len(sorted_vals) - 1))))]


# ---------------------------------------------------------------- 时钟 ----

@dataclass
class Clock:
    """FILETIME -> 挂钟 unix 微秒的映射：先减 A 得 QPC，再用锚点线性换算。"""
    a_ft: int            # Clock-Time = a_ft + QPC(以 100ns 计)
    freq: int            # QPC 频率
    q0: int              # 参考锚点 QPC
    u0: int              # 参考锚点 unix 微秒
    eps: float = 0.0     # 挂钟相对 QPC 的速率偏差（无量纲，由首末锚点拟合）
    a_spread_us: float = 0.0   # 估计 A 时 VSync 事件时间戳相对其内嵌 QPC 的散布（A 的误差上界）
    anchor_bracket_us: float = 0.0  # 锚点读取括号宽度的最大值（单锚点误差上界）
    a_from_trace: bool = True   # A 是否由 VSyncDPC 反推（False 表示退化为直接把 FILETIME 当挂钟）

    def to_unix_us(self, ft: int) -> float:
        """把事件的 FILETIME 换算成挂钟 unix 微秒。"""
        if not self.a_from_trace:
            return (ft - 116444736000000000) / 10.0
        qpc = (ft - self.a_ft) * self.freq / FT_PER_SEC
        return self.u0 + (qpc - self.q0) * 1e6 / self.freq * (1.0 + self.eps)


def estimate_a_ft(events: Sequence[Ev], freq: int) -> Tuple[Optional[int], float]:
    """由 VSyncDPC(17) 载荷里的 QPC（第 6 项）反推 A；事件时间戳不会早于其内嵌 QPC，故取 5 分位。返回 (A, 散布微秒)。"""
    diffs: List[int] = []
    for e in events:
        if e.prov == PROV_DXG and e.eid == 17 and len(e.payload) > 5:
            q = _int(e.payload[5])
            if q:
                diffs.append(e.ft - round(q * FT_PER_SEC / freq))
    if len(diffs) < 3:
        return None, 0.0
    diffs.sort()
    spread = (percentile(diffs, 95) - percentile(diffs, 5)) / 10.0
    return int(percentile(diffs, 5)), spread


def make_clock(events: Sequence[Ev], clock_json: Optional[dict]) -> Clock:
    """用 clock.json 的锚点与 VSync 反推的 A 构造时钟映射；缺任一项则退化并在 a_from_trace/eps 上体现。"""
    freq = int((clock_json or {}).get("freq") or FT_PER_SEC)
    a_ft, spread = estimate_a_ft(events, freq)
    anchors = (clock_json or {}).get("anchors") or []
    if a_ft is None or not anchors:
        return Clock(0, freq, 0, 0, 0.0, 0.0, 0.0, a_from_trace=False)
    first, last = anchors[0], anchors[-1]
    eps = 0.0
    dq_us = (last["qpc"] - first["qpc"]) * 1e6 / freq
    if len(anchors) > 1 and dq_us > 1e6:
        eps = (last["unix_us"] - first["unix_us"]) / dq_us - 1.0
    bracket = max(a.get("bracket_ticks", 0) * 1e6 / freq for a in anchors)
    return Clock(a_ft, freq, first["qpc"], first["unix_us"], eps, spread, bracket)


# ------------------------------------------------------------ 事件统计 ----

def count_events(events: Sequence[Ev]) -> List[dict]:
    """按 (provider, 事件号, 类型) 统计数量与每秒速率，按数量降序。"""
    if not events:
        return []
    span_s = max(1e-9, (max(e.ft for e in events) - min(e.ft for e in events)) / FT_PER_SEC)
    cnt: Dict[Tuple[str, int, str], int] = {}
    for e in events:
        k = (e.prov, e.eid, e.kind)
        cnt[k] = cnt.get(k, 0) + 1
    rows = [{"provider": k[0].replace("Microsoft-Windows-", ""), "id": k[1], "kind": k[2],
             "name": EVENT_NAMES.get((k[0], k[1]), ""), "count": n, "per_sec": round(n / span_s, 2)} for k, n in cnt.items()]
    return sorted(rows, key=lambda r: -r["count"])


def interval_stats(times_us: Sequence[float], period_us: Optional[float] = None, factor: float = ANOMALY_FACTOR) -> dict:
    """相邻时刻间隔的分布与异常（间隔 > factor*周期）。period_us 缺省取间隔中位数。时刻须为升序。"""
    if len(times_us) < 2:
        return {"n": len(times_us), "intervals": 0}
    gaps = [b - a for a, b in zip(times_us, times_us[1:])]
    s = sorted(gaps)
    period = period_us or statistics.median(gaps)
    limit = period * factor
    anomalies = [{"start_us": times_us[i], "gap_us": round(g, 1), "periods": round(g / period, 2)} for i, g in enumerate(gaps) if g > limit]
    return {"n": len(times_us), "intervals": len(gaps), "period_us": round(period, 1), "mean_us": round(statistics.fmean(gaps), 1),
            "p50_us": round(percentile(s, 50), 1), "p95_us": round(percentile(s, 95), 1), "p99_us": round(percentile(s, 99), 1),
            "max_us": round(s[-1], 1), "anomaly_limit_us": round(limit, 1), "anomalies": anomalies}


# ------------------------------------------------------------- VSync ----

def vsync_targets(events: Sequence[Ev]) -> Dict[str, List[Tuple[int, int]]]:
    """VSyncDPC(17) 按 VidPnTargetId 分组，返回 {target: [(ft, vsync 计数)]}，各组按时间升序。"""
    out: Dict[str, List[Tuple[int, int]]] = {}
    for e in events:
        if e.prov == PROV_DXG and e.eid == 17 and len(e.payload) > 4:
            out.setdefault(e.payload[1], []).append((e.ft, _int(e.payload[4]) or 0))
    for v in out.values():
        v.sort()
    return out


def pick_dwm_target(events: Sequence[Ev], targets: Dict[str, List[Tuple[int, int]]]) -> Optional[str]:
    """DWM 等待的 vsync 计数（DxgKrnl 318 载荷第 2 项）落在哪个 target 的计数里就选哪个；对不上则选 VSync 最多的。"""
    waits = {_int(e.payload[1]) for e in events if e.prov == PROV_DXG and e.eid == 318 and len(e.payload) > 1}
    best, best_n = None, 0
    for t, v in targets.items():
        n = len(waits & {c for _, c in v})
        if n > best_n:
            best, best_n = t, n
    if best:
        return best
    return max(targets, key=lambda t: len(targets[t])) if targets else None


# --------------------------------------------------------------- 配对 ----

@dataclass
class Present:
    """夹具的一次 Present 及其下游链路（时刻均为 FILETIME，换算前）。"""
    idx: int
    t_call: int
    t_ret: int
    tid: int
    swapchain: str = ""
    flags: str = ""
    token: Optional[int] = None
    tokdata: str = ""
    model: str = ""
    pickup: Optional[int] = None       # DWM 取走令牌（173 Stop）
    dwm_frame: Optional[int] = None    # 取走后第一个 DWM 合成帧（PROCESS_FRAME Start）
    dwm_present: Optional[int] = None  # 该帧内 DWM 的 Present（15 Start）
    flip: Optional[int] = None         # 随后的硬件翻页事件
    vsync: Optional[int] = None        # 翻页后第一个 vsync
    vsync_count: Optional[int] = None
    status: str = ""
    superseded_by: Optional[int] = None
    seq: Optional[int] = None


def extract_presents(events: Sequence[Ev], pid: int) -> List[Present]:
    """取夹具进程 pid 的 DXGI Present（42 Start 与同线程下一个 43 Stop 配对），并挂上同线程调用期内的 215 令牌。
    DXGI_PRESENT_TEST 的试探 Present 不算。按返回时刻升序。"""
    mine = [e for e in events if e.pid == pid and ((e.prov == PROV_DXGI and e.eid in (42, 43)) or (e.prov == PROV_DXG and e.eid == 215 and e.kind == "Start"))]
    mine.sort(key=lambda e: e.ft)
    open_by_tid: Dict[int, Present] = {}
    out: List[Present] = []
    for e in mine:
        if e.prov == PROV_DXGI and e.eid == 42:
            if len(e.payload) > 1 and TEST_PRESENT_TAG in e.payload[1]:
                continue
            open_by_tid[e.tid] = Present(0, e.ft, e.ft, e.tid, e.payload[0] if e.payload else "", e.payload[1] if len(e.payload) > 1 else "")
        elif e.prov == PROV_DXGI and e.eid == 43:
            p = open_by_tid.pop(e.tid, None)
            if p:
                p.t_ret = e.ft
                out.append(p)
        elif e.prov == PROV_DXG and e.eid == 215:
            p = open_by_tid.get(e.tid)
            if p and p.token is None and len(e.payload) > 4:
                p.tokdata, p.model, p.token = e.payload[1], e.payload[2], _int(e.payload[4])
    out.sort(key=lambda p: p.t_ret)
    for i, p in enumerate(out):
        p.idx = i
    return out


def pair_presents(presents: List[Present], events: Sequence[Ev], target: Optional[str]) -> List[Present]:
    """沿链路给每个 Present 填上下游时刻与状态。可信度（标注 [实测]=本机真实 flip-model 应用验证过，[推断]=按语义推断未经夹具实测）:
      令牌 215(应用) -> 173 Stop(DWM 取走)  按 (TokenData, 令牌号, 模型) 精确匹配          [实测，终端应用]
      取走时刻 -> 下一个 DWM PROCESS_FRAME Start 即为合成它的帧                              [推断：DWM 在帧内处理令牌]
      该帧内第一个 DWM SCHEDULE_PRESENT(15) Start 为其提交呈现；无则 dwm_no_present            [推断]
      Present 之后第一个 MMIOFlipMPO(259，退而 386) 为硬件翻页，其后第一个 vsync 为上屏时刻     [推断，可能有 ±1 vsync 歧义]
      同一 DWM 帧内同一交换链的多个令牌只有最后一个会显示，其余 superseded                   [推断，flip-model 语义]
    status: displayed / superseded / no_pickup / dwm_no_present / no_vsync。"""
    pickups: Dict[Tuple[str, Optional[int], str], List[int]] = {}
    frames: List[int] = []
    dpresents: List[int] = []
    flips: List[int] = []
    for e in events:
        if e.prov == PROV_DXG and e.eid == 173 and e.kind == "Stop" and len(e.payload) > 4:
            pickups.setdefault((e.payload[1], _int(e.payload[4]), e.payload[2]), []).append(e.ft)
        elif e.prov == PROV_DWM and e.eid == 10 and e.kind == "Start":
            frames.append(e.ft)
        elif e.prov == PROV_DWM and e.eid == 15 and e.kind == "Start":
            dpresents.append(e.ft)
        elif e.prov == PROV_DXG and e.eid == 259:
            flips.append(e.ft)
    if not flips:
        flips = sorted(e.ft for e in events if e.prov == PROV_DXG and e.eid == 386)
    for lst in list(pickups.values()) + [frames, dpresents, flips]:
        lst.sort()
    vs = (vsync_targets(events).get(target or "") or []) if target else []
    vs_t = [t for t, _ in vs]

    def first_ge(arr: List[int], t: int) -> Optional[int]:
        i = bisect.bisect_left(arr, t)
        return i if i < len(arr) else None

    for p in presents:
        lst = pickups.get((p.tokdata, p.token, p.model))
        i = first_ge(lst, p.t_call) if lst and p.token is not None else None
        if i is None:
            p.status = "no_pickup"
            continue
        p.pickup = lst[i]
        fi = first_ge(frames, p.pickup)
        if fi is None:
            p.status = "no_vsync"
            continue
        p.dwm_frame = frames[fi]
        next_frame = frames[fi + 1] if fi + 1 < len(frames) else None
        di = first_ge(dpresents, p.dwm_frame)
        if di is None or (next_frame is not None and dpresents[di] >= next_frame):
            p.status = "dwm_no_present"
            continue
        p.dwm_present = dpresents[di]
        fl = first_ge(flips, p.dwm_present)
        ref = flips[fl] if fl is not None else p.dwm_present
        p.flip = flips[fl] if fl is not None else None
        vi = bisect.bisect_right(vs_t, ref)
        if vi >= len(vs):
            p.status = "no_vsync"
            continue
        p.vsync, p.vsync_count = vs[vi]
        p.status = "displayed"
    # 同一 DWM 帧内同一交换链的多个令牌：只有取走时刻最晚的那个显示
    groups: Dict[Tuple[int, str], List[Present]] = {}
    for p in presents:
        if p.dwm_frame is not None and p.pickup is not None:
            groups.setdefault((p.dwm_frame, p.swapchain), []).append(p)
    for g in groups.values():
        if len(g) > 1:
            g.sort(key=lambda p: p.pickup)
            for p in g[:-1]:
                p.status, p.superseded_by = "superseded", g[-1].idx
    return presents


def match_fixture(presents: Sequence[Present], fixture: Sequence[Tuple[int, float]], clock: Clock, tol_us: float = MATCH_TOL_US) -> dict:
    """把 frames.csv 的 (seq, unix_us) 与 Present 返回时刻按"最近且单调"配对（两边都是升序，各用一次）。
    返回 {matched, unmatched_seq, unmatched_presents, offset_us_median}；配对成功的 Present 会写上 seq。"""
    rets = [clock.to_unix_us(p.t_ret) for p in presents]
    fx = sorted(fixture, key=lambda x: x[1])
    j, matched, unmatched_seq, offs = 0, 0, [], []
    for seq, u in fx:
        while j < len(rets) and rets[j] < u - tol_us:
            j += 1
        if j < len(rets) and abs(rets[j] - u) <= tol_us:
            presents[j].seq = seq
            offs.append(rets[j] - u)
            matched += 1
            j += 1
        else:
            unmatched_seq.append(seq)
    return {"matched": matched, "unmatched_seq": unmatched_seq,
            "unmatched_presents": sum(1 for p in presents if p.seq is None),
            "offset_us_median": round(statistics.median(offs), 1) if offs else None}


def parse_fixture(text: str) -> List[Tuple[int, float]]:
    """解析夹具 frames.csv（表头 seq,submit_ns,flush_ns,unix_us），返回 [(seq, unix_us)]。"""
    return [(int(r["seq"]), float(r["unix_us"])) for r in csv.DictReader(io.StringIO(text)) if r.get("seq")]


# ---------------------------------------------------------------- 输出 ----

TABLE_HEADER = ["idx", "seq", "status", "token", "model", "present_call_unix_us", "present_ret_unix_us", "pickup_unix_us", "dwm_frame_unix_us",
                "dwm_present_unix_us", "flip_unix_us", "vsync_unix_us", "vsync_count", "present_to_vsync_us", "superseded_by"]


def table_rows(presents: Sequence[Present], clock: Clock) -> List[List[str]]:
    """逐 Present 的显示表行（时刻为挂钟 unix 微秒，缺失为空）。"""
    def u(ft: Optional[int]) -> str:
        return "" if ft is None else f"{clock.to_unix_us(ft):.1f}"
    rows = []
    for p in presents:
        lat = "" if p.vsync is None else f"{clock.to_unix_us(p.vsync) - clock.to_unix_us(p.t_ret):.1f}"
        rows.append([p.idx, "" if p.seq is None else p.seq, p.status, "" if p.token is None else p.token, p.model, u(p.t_call), u(p.t_ret), u(p.pickup),
                     u(p.dwm_frame), u(p.dwm_present), u(p.flip), u(p.vsync), "" if p.vsync_count is None else p.vsync_count, lat,
                     "" if p.superseded_by is None else p.superseded_by])
    return rows


def build_summary(events: Sequence[Ev], clock: Clock, pid: Optional[int], fixture: Optional[Sequence[Tuple[int, float]]],
                  info: Optional[dict] = None, target_arg: str = "") -> Tuple[dict, List[Present]]:
    """汇总全部分析：事件计数、vsync/DWM 间隔、Present 配对与 seq 对应。返回 (JSON 摘要, Present 列表)。"""
    u = clock.to_unix_us
    targets = vsync_targets(events)
    target = target_arg or pick_dwm_target(events, targets)
    sm: dict = {"clock": {"from_trace": clock.a_from_trace, "a_spread_us": round(clock.a_spread_us, 2), "anchor_bracket_us": round(clock.anchor_bracket_us, 2),
                          "rate_dev_ppm": round(clock.eps * 1e6, 3)},
                "events_total": len(events), "header_events_lost": (info or {}).get("header_events_lost"), "counts": count_events(events), "vsync": {},
                "dwm_target": target}
    for t, v in targets.items():
        sm["vsync"][t] = interval_stats([u(ft) for ft, _ in v])
    frames = sorted(e.ft for e in events if e.prov == PROV_DWM and e.eid == 10 and e.kind == "Start")
    dpres = sorted(e.ft for e in events if e.prov == PROV_DWM and e.eid == 15 and e.kind == "Start")
    vperiod = sm["vsync"].get(target or "", {}).get("period_us")
    sm["dwm_frames"] = interval_stats([u(t) for t in frames], vperiod)
    sm["dwm_presents"] = interval_stats([u(t) for t in dpres], vperiod)
    presents: List[Present] = []
    if pid is not None:
        presents = pair_presents(extract_presents(events, pid), events, target)
        st: Dict[str, int] = {}
        for p in presents:
            st[p.status] = st.get(p.status, 0) + 1
        shown = [u(p.vsync) for p in presents if p.status == "displayed" and p.vsync is not None]
        sm["fixture"] = {"pid": pid, "presents": len(presents), "status": st, "display": interval_stats(sorted(set(shown)), vperiod),
                         "present_ret": interval_stats([u(p.t_ret) for p in presents], vperiod)}
        if fixture:
            sm["fixture"]["match"] = match_fixture(presents, fixture, clock)
    return sm, presents


def render_report(sm: dict) -> str:
    """把摘要渲染成文本报告。"""
    L = ["=== ETW 桌面合成与呈现 ==="]
    c = sm["clock"]
    L.append(f"事件总数 {sm['events_total']}  tracerpt 头记录丢失事件 {sm['header_events_lost']}  时钟: " +
             (f"A 由 VSync 反推(散布 {c['a_spread_us']}us)、锚点括号 {c['anchor_bracket_us']}us、挂钟/QPC 速率偏差 {c['rate_dev_ppm']}ppm" if c["from_trace"] else "退化：直接用 FILETIME 当挂钟(无 VSync 或无锚点，精度未知)"))
    L.append("--- 事件计数（前 30）---")
    for r in sm["counts"][:30]:
        L.append(f"{r['provider']:<10} id={r['id']:<4} {r['kind']:<5} {r['count']:>6}  {r['per_sec']:>8.2f}/s  {r['name']}")

    def block(title: str, s: dict) -> None:
        if s.get("intervals", 0) == 0:
            L.append(f"{title}: 样本不足 (n={s.get('n', 0)})")
            return
        L.append(f"{title}: n={s['n']} 周期={s['period_us']}us mean={s['mean_us']} p50={s['p50_us']} p95={s['p95_us']} p99={s['p99_us']} max={s['max_us']}  异常(>{s['anomaly_limit_us']}us)={len(s['anomalies'])}")
        for a in s["anomalies"][:20]:
            L.append(f"    间隔 {a['gap_us']}us ({a['periods']} 周期) 起于 unix_us={a['start_us']:.0f}")
    L.append("--- 间隔分布 ---")
    for t, s in sm["vsync"].items():
        block(f"VSync target={t}{' (DWM 等待的)' if t == sm['dwm_target'] else ''}", s)
    block("DWM 合成帧(PROCESS_FRAME)", sm["dwm_frames"])
    block("DWM 呈现(SCHEDULE_PRESENT)", sm["dwm_presents"])
    f = sm.get("fixture")
    if f:
        L.append(f"--- 夹具 pid={f['pid']} ---")
        L.append(f"Present 次数 {f['presents']}  状态分布 {f['status']}")
        block("夹具 Present 返回间隔", f["present_ret"])
        block("夹具帧的上屏 vsync 间隔", f["display"])
        if "match" in f:
            m = f["match"]
            L.append(f"frames.csv 配对: 成功 {m['matched']}  未配到 seq {len(m['unmatched_seq'])}  未配到 frames.csv 的 Present {m['unmatched_presents']}  时刻偏差中位 {m['offset_us_median']}us")
    return "\n".join(L) + "\n"


def main(argv: Optional[Sequence[str]] = None) -> int:
    """命令行入口。"""
    ap = argparse.ArgumentParser(description="解析 tracerpt CSV，给出 DWM 合成/呈现与夹具 Present 的配对真值")
    ap.add_argument("--csv", required=True)
    ap.add_argument("--clock", help="<etl>.clock.json")
    ap.add_argument("--pid", type=int, help="夹具进程号")
    ap.add_argument("--fixture", help="夹具 frames.csv")
    ap.add_argument("--target", default="", help="指定 VidPnTargetId（如 0x1041）；缺省取 DWM 等待的那个")
    ap.add_argument("--summary", help="tracerpt 的 -summary 文本，用于读取丢失事件数")
    ap.add_argument("--report")
    ap.add_argument("--json")
    ap.add_argument("--table")
    a = ap.parse_args(argv)
    with open(a.csv, encoding="utf-8", errors="replace", newline="") as f:
        events, info = parse_tracerpt_csv(f.read())
    clock_json = json.load(open(a.clock, encoding="utf-8-sig")) if a.clock else None
    if a.summary:
        import re
        m = re.search(r"Total Events\s+Lost\s+(\d+)", open(a.summary, encoding="utf-8", errors="replace").read())
        if m:
            info["header_events_lost"] = m.group(1)
    fixture = parse_fixture(open(a.fixture, encoding="utf-8").read()) if a.fixture else None
    clock = make_clock(events, clock_json)
    sm, presents = build_summary(events, clock, a.pid, fixture, info, a.target)
    text = render_report(sm)
    print(text)
    if a.report:
        open(a.report, "w", encoding="utf-8").write(text)
    if a.json:
        json.dump(sm, open(a.json, "w", encoding="utf-8"), ensure_ascii=False, indent=1)
    if a.table:
        with open(a.table, "w", encoding="utf-8", newline="") as f:
            w = csv.writer(f)
            w.writerow(TABLE_HEADER)
            w.writerows(table_rows(presents, clock))
    return 0


if __name__ == "__main__":
    sys.exit(main())
