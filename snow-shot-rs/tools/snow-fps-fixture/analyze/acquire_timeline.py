#!/usr/bin/env python3
"""把夹具呈现时刻、逐次 AcquireNextFrame 调用、ReleaseFrame、录制流水线事件与被丢序号对齐到同一条时间线。

目的：查明 DXGI 合并丢帧（取帧时 AccumulatedFrames=2）的根因——那些约 4.4ms 的"阻塞调用"何时开始、返回什么、
和夹具的呈现时刻是什么先后关系。需要录制进程同时设置 SNOW_RECORDER_FRAME_TRACE 与 SNOW_RECORDER_POLL_TRACE=1
（`poll`/`release` 事件与列定义见 snow-recorder/src/frametrace.rs 模块文档）。

用法:
    python acquire_timeline.py --fixture 夹具.frames.csv --trace 录制追踪.csv [--join trace_join.json] \\
        [--window-ms 40] [--block-us 2000] [--max-timelines 60] [--collapse] [--report 报告.txt] [--json 摘要.json]

输入:
    --fixture  夹具 frames.csv（seq,submit_ns,flush_ns,unix_us）；
    --trace    录制追踪 csv（含 poll/release 事件）；
    --join     可选，trace_join.py 的 --json 输出：取其中的被丢序号 lost[].seq 与时钟偏移 clock.offset_us。
               没有时脚本自己用追踪求：偏移 = 夹具序号与最近一次"成功取帧的呈现时刻"之差的中位数（复用 trace_join.estimate_offset），
               被丢序号 = 夹具序号在 夹具时刻+偏移 附近（±MATCH_US）没有任何成功取帧的呈现时刻与之对应。

时间约定: 全部用挂钟微秒（unix_us）。某序号的"呈现时刻" t_present = 夹具 unix_us + 偏移（夹具记录的是 Present 返回时刻，
偏移 = DXGI 呈现时刻 - 夹具时刻，见 trace_join.py）。"阻塞调用" = AcquireNextFrame 耗时 >= BLOCK_US（默认 2ms）。

机理判定规则（每个被丢序号，`classify_loss`）。记 tS = 该序号呈现时刻，tN = 下一个序号的呈现时刻（缺失则 tS + 夹具间隔），
tol = TOL_US（默认 0.5ms）。成功取帧 = poll 的 code 为 0 且带呈现时刻；"覆盖调用" = tS 之后第一次呈现时刻晚于 tS + MATCH_US 的成功取帧。
按顺序取第一条命中的：
  1. no_poll_data                  追踪里没有任何 poll 事件（没开 SNOW_RECORDER_POLL_TRACE 或旧版录制进程）。
  2. delivered                     有成功取帧的呈现时刻与 tS 吻合（±MATCH_US）：其实没丢（时钟偏移/匹配误差）。
  3. not_coalesced                 找不到覆盖调用，或覆盖调用 AccumulatedFrames < 2：丢失不能用 DXGI 合并解释。
  4. blocked_through_next_present  tS 时刻有一次阻塞调用正在进行（start <= tS < end），且它直到 tN - tol 之后才返回：
                                   阻塞结束时该序号已被下一次呈现覆盖。
  5. blocked_returned_without_it   tS 时刻有阻塞调用正在进行，但它在 tN - tol 之前就返回了，返回的不是该序号
                                   （超时或更早的帧）：阻塞期间的呈现没有体现在这次返回里，之后又没来得及再取。
  6. no_call_in_window             [tS, tN) 内没有任何调用开始，tS 时刻也没有阻塞调用：呈现后到被覆盖前"没有调用"（调用间隔/线程调度）。
  7. calls_returned_no_frame       [tS, tN) 内有调用开始，但都没有取到该序号（超时返回或更早的帧）：呈现后调用没立刻看到它。
  8. other                         其余。

依赖: 仅 Python 3 标准库与同目录的 trace_join.py（复用 parse_trace / parse_fixture / estimate_offset）。
"""

import argparse
import bisect
import json
import statistics
import sys
from dataclasses import dataclass
from typing import Dict, List, Optional, Sequence, Tuple

import trace_join as tj

# ---- 与 snow-recorder/src/frametrace.rs 的约定一致的 poll 结果码 ----
POLL_OK, POLL_TIMEOUT, POLL_LOST, POLL_ERROR = 0, 1, 2, 3
POLL_NAMES = {POLL_OK: "成功", POLL_TIMEOUT: "超时", POLL_LOST: "权限丢失", POLL_ERROR: "错误"}
# poll 事件 flags 位。
FLAG_MOUSE, FLAG_PRESENT, FLAG_RECTS, FLAG_PROTECTED = 1, 2, 4, 8

BLOCK_US = 2000  # 阻塞调用的耗时下限（微秒）
MATCH_US = 4000  # 呈现时刻与夹具序号匹配的容差（微秒）
TOL_US = 500  # 判定"阻塞结束是否早于下一次呈现"的容差（微秒）
WINDOW_US = 40000  # 每个丢失序号时间线的半宽（微秒）
PHASE_BIN_US = 1000  # 相位直方图的桶宽（微秒）
PHASE_MAX_US = 20000  # 相位直方图的上限（微秒）

# ---- 机理判定 ----
M_NO_DATA = "no_poll_data"
M_DELIVERED = "delivered"
M_NOT_COALESCED = "not_coalesced"
M_BLOCKED_THROUGH = "blocked_through_next_present"
M_BLOCKED_RETURNED = "blocked_returned_without_it"
M_NO_CALL = "no_call_in_window"
M_CALLS_NO_FRAME = "calls_returned_no_frame"
M_OTHER = "other"
MECHANISM_TEXT = {
    M_NO_DATA: "追踪里没有 poll 事件",
    M_DELIVERED: "其实被取到了（匹配误差），不算丢",
    M_NOT_COALESCED: "找不到 AccumulatedFrames>=2 的覆盖调用，不能用 DXGI 合并解释",
    M_BLOCKED_THROUGH: "呈现时 poll 正阻塞，阻塞结束时已被下一次呈现覆盖",
    M_BLOCKED_RETURNED: "呈现时 poll 正阻塞，阻塞在下一次呈现之前返回但没带回该序号，之后没来得及再取",
    M_NO_CALL: "呈现后到被覆盖前没有任何取帧调用",
    M_CALLS_NO_FRAME: "呈现后有调用，但都没取到该序号（超时/更早的帧）",
    M_OTHER: "其它",
}


@dataclass
class Poll:
    """一次 AcquireNextFrame 调用。"""

    start_us: int
    dur_us: int
    code: int
    src: int = 0
    n: int = 0  # 成功时的 AccumulatedFrames
    present_us: Optional[int] = None  # 成功且带内容更新时的呈现挂钟
    flags: int = 0
    pointer_bytes: int = 0
    meta_bytes: int = 0
    since_release_us: int = 0  # 上一次 ReleaseFrame 结束到本次调用开始（0 也可能表示没有）

    @property
    def end_us(self) -> int:
        """调用返回时刻。"""
        return self.start_us + self.dur_us

    @property
    def ok(self) -> bool:
        """是否成功取到带呈现时刻的帧。"""
        return self.code == POLL_OK and self.present_us is not None


@dataclass
class Release:
    """一次 ReleaseFrame 调用。"""

    start_us: int
    dur_us: int
    src: int = 0

    @property
    def end_us(self) -> int:
        """返回时刻。"""
        return self.start_us + self.dur_us


@dataclass
class Verdict:
    """一个被丢序号的机理判定。"""

    mechanism: str
    detail: str = ""
    active_block: Optional[Poll] = None  # tS 时刻正在进行的阻塞调用
    cover: Optional[Poll] = None  # 覆盖调用


def load_polls(trace: tj.Trace) -> Tuple[List[Poll], List[Release]]:
    """从解析后的追踪里取出 poll / release 事件（按开始时刻排序）。

    参数:
        trace: tj.parse_trace 的结果。
    返回:
        (polls, releases)。poll 的 flags/指针字节/元数据字节/释放间隔依次来自扩展列 sleep_req/sleep_over/acq/lock。
    示例:
        >>> t = tj.parse_trace("mono_ns,unix_us,event,thread,src,cap_id,slot,present_unix_us,n,dur_us,code\\n1,100,poll,cap,0,0,,,0,4400,1\\n")
        >>> load_polls(t)[0][0].dur_us
        4400
    """
    polls: List[Poll] = []
    rels: List[Release] = []
    for e in trace.events:
        if e.event == "poll":
            polls.append(Poll(e.unix_us, e.dur_us, e.code, e.src, e.n, e.present_us, e.sleep_req_us, e.sleep_over_us, e.acq_us, e.lock_us))
        elif e.event == "release":
            rels.append(Release(e.unix_us, e.dur_us, e.src))
    polls.sort(key=lambda p: p.start_us)
    rels.sort(key=lambda r: r.start_us)
    return polls, rels


def percentile(sorted_vals: Sequence[float], q: float) -> float:
    """已排序序列的 q 分位（最近邻）；空序列返回 0。

    参数:
        sorted_vals: 升序数据。
        q: 0~1。
    示例:
        >>> percentile([1, 2, 3, 4, 5], 0.5)
        3
    """
    if not sorted_vals:
        return 0
    return sorted_vals[min(len(sorted_vals) - 1, int(len(sorted_vals) * q))]


def dist(values: Sequence[float]) -> dict:
    """分布摘要：个数、p50/p90/p99/最大（保持原单位）。

    参数:
        values: 任意顺序的数据。
    示例:
        >>> dist([3, 1, 2])["p50"]
        2
    """
    s = sorted(values)
    return {"count": len(s), "p50": percentile(s, 0.5), "p90": percentile(s, 0.9), "p99": percentile(s, 0.99), "max": s[-1] if s else 0}


def summarize_polls(polls: Sequence[Poll], block_us: int = BLOCK_US) -> dict:
    """poll 耗时分布（按结果类别）与阻塞调用的结果占比。

    参数:
        polls: 调用列表。
        block_us: 阻塞调用的耗时下限（微秒）。
    返回:
        {"total", "by_code": {名: 分布(毫秒)}, "blocking": {"count", "by_code": {名: 个数}, "ok_ratio", "timeout_ratio", "dur_ms": 分布}}。
    示例:
        >>> s = summarize_polls([Poll(0, 5000, 1), Poll(10, 10, 0, present_us=1)])
        >>> (s["blocking"]["count"], s["blocking"]["by_code"])
        (1, {'超时': 1})
    """
    by_code: Dict[str, List[float]] = {}
    for p in polls:
        by_code.setdefault(POLL_NAMES.get(p.code, str(p.code)), []).append(p.dur_us / 1000)
    blocking = [p for p in polls if p.dur_us >= block_us]
    b_codes: Dict[str, int] = {}
    for p in blocking:
        name = POLL_NAMES.get(p.code, str(p.code))
        b_codes[name] = b_codes.get(name, 0) + 1
    total_b = len(blocking)
    return {
        "total": len(polls),
        "by_code": {k: dist(v) for k, v in sorted(by_code.items())},
        "blocking": {
            "count": total_b,
            "by_code": b_codes,
            "ok_ratio": (sum(1 for p in blocking if p.code == POLL_OK) / total_b) if total_b else 0.0,
            "timeout_ratio": (sum(1 for p in blocking if p.code == POLL_TIMEOUT) / total_b) if total_b else 0.0,
            "dur_ms": dist([p.dur_us / 1000 for p in blocking]),
        },
    }


def phase_histogram(polls: Sequence[Poll], block_us: int = BLOCK_US, bin_us: int = PHASE_BIN_US, max_us: int = PHASE_MAX_US) -> List[Tuple[int, int]]:
    """阻塞调用起点相对"上一次成功取帧返回时刻"的相位直方图。

    参数:
        polls: 按开始时刻排序的调用。
        block_us: 阻塞调用下限。
        bin_us: 桶宽。
        max_us: 上限，更晚的计入最后一个桶（>= max_us）。
    返回:
        [(桶起点微秒, 个数)]，最后一桶是 >= max_us 的；没有上一次成功取帧的阻塞调用不计入。
    示例:
        >>> phase_histogram([Poll(0, 10, 0, present_us=1), Poll(2500, 5000, 1)])[2]
        (2000, 1)
    """
    nbins = max_us // bin_us + 1
    counts = [0] * nbins
    last_ok_end: Optional[int] = None
    for p in polls:
        if p.dur_us >= block_us and last_ok_end is not None:
            phase = max(0, p.start_us - last_ok_end)
            counts[min(nbins - 1, phase // bin_us)] += 1
        if p.ok:
            last_ok_end = p.end_us
    return [(i * bin_us, c) for i, c in enumerate(counts)]


def _blocking_index(polls: Sequence[Poll], block_us: int) -> Tuple[List[int], List[Poll]]:
    """阻塞调用按开始时刻排序后的 (开始时刻列表, 调用列表)。"""
    blocking = sorted((p for p in polls if p.dur_us >= block_us), key=lambda p: p.start_us)
    return [p.start_us for p in blocking], blocking


def _inside_block(t_us: float, starts: List[int], blocking: List[Poll]) -> Optional[Poll]:
    """t 落在哪次阻塞调用内部（start <= t < end），没有返回 None（调用在单线程里串行，不重叠）。"""
    i = bisect.bisect_right(starts, t_us) - 1
    if i >= 0 and blocking[i].start_us <= t_us < blocking[i].end_us:
        return blocking[i]
    return None


def present_overlap(presents: Sequence[int], polls: Sequence[Poll], block_us: int = BLOCK_US, lost: Optional[set] = None, interval_us: float = 16667.0) -> dict:
    """呈现时刻落在阻塞调用内部的比例，对比两种随机基线。

    基线一（时间占比）= 阻塞总时长 / 观测时长；基线二（相位打乱）= 把全部呈现整体平移 interval 的 1/10..9/10 后的平均命中率
    （保留呈现的周期性，只破坏它与阻塞调用的相位关系）。

    参数:
        presents: 全部呈现时刻（unix_us，顺序无关）。
        polls: 调用列表。
        block_us: 阻塞调用下限。
        lost: 可选，被丢呈现时刻的集合（用于单独统计丢帧呈现的命中率）。
        interval_us: 夹具出帧间隔。
    返回:
        {"presents","inside","ratio","baseline_time_ratio","baseline_shift_ratio","lost_total","lost_inside","lost_ratio"}。
    示例:
        >>> r = present_overlap([100, 20000], [Poll(50, 5000, 1)])
        >>> (r["inside"], r["ratio"])
        (1, 0.5)
    """
    starts, blocking = _blocking_index(polls, block_us)
    pts = sorted(presents)
    inside = sum(1 for t in pts if _inside_block(t, starts, blocking))
    span = (pts[-1] - pts[0]) if len(pts) > 1 else 0
    blocked_us = sum(max(0, min(b.end_us, pts[-1]) - max(b.start_us, pts[0])) for b in blocking) if span else 0
    shifted: List[float] = []
    for k in range(1, 10):
        sh = int(interval_us * k / 10)
        shifted.append(sum(1 for t in pts if _inside_block(t + sh, starts, blocking)) / len(pts) if pts else 0.0)
    lost_pts = sorted(lost) if lost else []
    lost_inside = sum(1 for t in lost_pts if _inside_block(t, starts, blocking))
    return {
        "presents": len(pts),
        "inside": inside,
        "ratio": inside / len(pts) if pts else 0.0,
        "baseline_time_ratio": blocked_us / span if span else 0.0,
        "baseline_shift_ratio": statistics.mean(shifted) if shifted else 0.0,
        "lost_total": len(lost_pts),
        "lost_inside": lost_inside,
        "lost_ratio": lost_inside / len(lost_pts) if lost_pts else 0.0,
    }


def block_vs_presents(polls: Sequence[Poll], presents: Sequence[int], block_us: int = BLOCK_US) -> dict:
    """阻塞调用与呈现的时间关系：呈现到阻塞起点、阻塞终点到下一次呈现的间隔分布（毫秒）。

    参数:
        polls: 调用列表。
        presents: 全部呈现时刻。
        block_us: 阻塞调用下限。
    返回:
        {"since_present_ms": 分布, "until_next_present_ms": 分布}；某次阻塞前/后没有呈现则不计入。
    示例:
        >>> block_vs_presents([Poll(1000, 5000, 1)], [0, 20000])["since_present_ms"]["p50"]
        1.0
    """
    pts = sorted(presents)
    since: List[float] = []
    until: List[float] = []
    for p in polls:
        if p.dur_us < block_us:
            continue
        i = bisect.bisect_right(pts, p.start_us)
        if i > 0:
            since.append((p.start_us - pts[i - 1]) / 1000)
        j = bisect.bisect_left(pts, p.end_us)
        if j < len(pts):
            until.append((pts[j] - p.end_us) / 1000)
    return {"since_present_ms": dist(since), "until_next_present_ms": dist(until)}


def estimate_offset_from_trace(fixture: Dict[int, int], polls: Sequence[Poll], interval_us: float) -> tj.Offset:
    """没有 trace_join 的 JSON 时，用成功取帧的呈现时刻自己标定夹具时间到呈现时间的偏移。

    做法：每个夹具序号取与其时刻最近的呈现时刻之差，取中位数作初值；再只保留与"夹具时刻+初值"相差在半个间隔内的配对，复用 tj.estimate_offset。

    参数:
        fixture: {序号: 夹具 unix_us}。
        polls: 调用列表。
        interval_us: 夹具出帧间隔。
    返回:
        tj.Offset；没有任何呈现时刻时 calibrated=False。
    示例:
        >>> f = {1: 0, 2: 16667}
        >>> estimate_offset_from_trace(f, [Poll(1, 1, 0, present_us=3000), Poll(2, 1, 0, present_us=19667)], 16667).offset_us
        3000
    """
    presents = sorted({p.present_us for p in polls if p.ok})
    if not presents or not fixture:
        return tj.Offset()

    def nearest(t: float) -> int:
        i = bisect.bisect_left(presents, t)
        cands = presents[max(0, i - 1) : i + 1]
        return min(cands, key=lambda c: abs(c - t))

    first = int(statistics.median(nearest(t) - t for t in fixture.values()))
    pairs = []
    for t in fixture.values():
        near = nearest(t + first)
        if abs(near - (t + first)) <= interval_us / 2:
            pairs.append((t, near))
    return tj.estimate_offset(pairs)


def infer_lost(fixture: Dict[int, int], polls: Sequence[Poll], offset_us: int, match_us: int = MATCH_US) -> List[int]:
    """没有 trace_join 的 JSON 时，由追踪推断被丢序号：夹具时刻+偏移附近没有任何成功取帧的呈现时刻与之对应。

    参数:
        fixture: {序号: 夹具 unix_us}。
        polls: 调用列表。
        offset_us: 夹具时间到呈现时间的偏移。
        match_us: 匹配容差。
    返回:
        升序的序号列表。
    示例:
        >>> infer_lost({1: 0, 2: 16667}, [Poll(1, 1, 0, present_us=0)], 0)
        [2]
    """
    presents = sorted({p.present_us for p in polls if p.ok})
    lost = []
    for seq in sorted(fixture):
        t = fixture[seq] + offset_us
        i = bisect.bisect_left(presents, t - match_us)
        if not (i < len(presents) and presents[i] <= t + match_us):
            lost.append(seq)
    return lost


def classify_loss(t_present: int, t_next: int, polls: Sequence[Poll], block_us: int = BLOCK_US, match_us: int = MATCH_US, tol_us: int = TOL_US) -> Verdict:
    """给一个被丢序号做机理判定（规则见模块文档）。

    参数:
        t_present: 该序号的呈现时刻（unix_us）。
        t_next: 下一个序号的呈现时刻（unix_us）。
        polls: 按开始时刻排序的全部调用。
        block_us: 阻塞调用耗时下限。
        match_us: 呈现时刻与序号匹配的容差。
        tol_us: 阻塞结束时刻与下一次呈现比较的容差。
    返回:
        Verdict。
    示例:
        >>> classify_loss(1000, 17667, []).mechanism
        'no_poll_data'
    """
    if not polls:
        return Verdict(M_NO_DATA)
    oks = [p for p in polls if p.ok]
    if any(abs(p.present_us - t_present) <= match_us for p in oks):
        return Verdict(M_DELIVERED)
    cover = next((p for p in oks if p.present_us > t_present + match_us and p.end_us >= t_present), None)
    if cover is None or cover.n < 2:
        return Verdict(M_NOT_COALESCED, "覆盖调用 " + ("不存在" if cover is None else f"n={cover.n}"), cover=cover)
    starts, blocking = _blocking_index(polls, block_us)
    active = _inside_block(t_present, starts, blocking)
    if active is not None:
        late = active.end_us >= t_next - tol_us
        detail = f"阻塞调用 {active.dur_us / 1000:.2f}ms，结果={POLL_NAMES.get(active.code, active.code)}，返回于下一次呈现{'之后' if late else '之前'} {abs(active.end_us - t_next) / 1000:.2f}ms"
        return Verdict(M_BLOCKED_THROUGH if late else M_BLOCKED_RETURNED, detail, active, cover)
    in_window = [p for p in polls if t_present <= p.start_us < t_next - tol_us]
    if not in_window:
        return Verdict(M_NO_CALL, f"最近一次调用开始于 {(t_present - max((p.start_us for p in polls if p.start_us < t_present), default=t_present)) / 1000:.2f}ms 前", cover=cover)
    if all(p.present_us is None or p.present_us <= t_present + match_us for p in in_window):
        return Verdict(M_CALLS_NO_FRAME, f"窗口内 {len(in_window)} 次调用均未带回该序号", cover=cover)
    return Verdict(M_OTHER, "", cover=cover)


@dataclass
class Item:
    """时间线上的一行。"""

    t_us: float
    order: int  # 同一时刻的先后：夹具 < poll < release < 流水线事件
    text: str
    short: bool = False  # 无信息量的短超时 poll（折叠时合并）


def build_timeline(seq: int, t_present: int, t_next: int, fixture: Dict[int, int], offset_us: int, polls: Sequence[Poll], rels: Sequence[Release], trace: tj.Trace, window_us: int = WINDOW_US, block_us: int = BLOCK_US, collapse: bool = False) -> List[str]:
    """生成某个丢失序号前后 ±window_us 的合并时间线（文本行，已按时间排序，偏移相对该序号的呈现时刻，毫秒）。

    参数:
        seq: 被丢序号。
        t_present: 其呈现时刻。
        t_next: 下一个序号的呈现时刻（保留以便与 annotate 对称，时间线本身不用）。
        fixture: {序号: 夹具 unix_us}。
        offset_us: 夹具时间到呈现时间的偏移。
        polls, rels, trace: 调用、释放与完整追踪。
        window_us: 半宽。
        block_us: 阻塞调用下限（打标）。
        collapse: 为真时把连续 3 个以上的短超时 poll 折叠成一行摘要。
    返回:
        文本行列表。
    """
    lo, hi = t_present - window_us, t_present + window_us

    def rel(t: float) -> float:
        return (t - t_present) / 1000

    items: List[Item] = []
    for s, ft in fixture.items():
        t = ft + offset_us
        if lo <= t <= hi:
            mark = "  <== 丢失序号" if s == seq else ""
            items.append(Item(t, 0, f"{rel(t):+9.3f}ms  夹具呈现 seq={s}{mark}"))
    for p in polls:
        if p.end_us < lo or p.start_us > hi:
            continue
        tag = " [阻塞]" if p.dur_us >= block_us else ""
        body = f"{rel(p.start_us):+9.3f}ms  poll   →{rel(p.end_us):+9.3f}ms 耗时={p.dur_us / 1000:.3f}ms {POLL_NAMES.get(p.code, p.code)}{tag}"
        if p.code == POLL_OK:
            body += f" n={p.n}"
            if p.present_us is not None:
                body += f" present={rel(p.present_us):+.3f}ms"
            body += f" flags={p.flags}"
        if p.since_release_us:
            body += f" 距上次释放={p.since_release_us / 1000:.3f}ms"
        items.append(Item(p.start_us, 1, body, short=p.code == POLL_TIMEOUT and p.dur_us < block_us))
    for r in rels:
        if lo <= r.start_us <= hi:
            items.append(Item(r.start_us, 2, f"{rel(r.start_us):+9.3f}ms  release →{rel(r.end_us):+9.3f}ms 耗时={r.dur_us / 1000:.3f}ms"))
    for e in trace.events:
        if e.event in ("acquire", "enqueue") and lo <= e.unix_us <= hi:
            extra = f" n={e.n} code={e.code}" if e.event == "acquire" else f" fresh={e.code}"
            items.append(Item(e.unix_us, 3, f"{rel(e.unix_us):+9.3f}ms  {e.event:<7} cap={e.cap_id}{extra}"))
    items.sort(key=lambda i: (i.t_us, i.order))
    if not collapse:
        return [i.text for i in items]
    out: List[str] = []
    run: List[Item] = []

    def flush() -> None:
        if len(run) >= 3:
            out.append(f"{rel(run[0].t_us):+9.3f}ms  poll   ×{len(run)} 个短超时调用，至 {rel(run[-1].t_us):+.3f}ms")
        else:
            out.extend(i.text for i in run)
        run.clear()

    for i in items:
        if i.short:
            run.append(i)
        else:
            flush()
            out.append(i.text)
    flush()
    return out


def annotate(seq: int, t_present: int, t_next: int, polls: Sequence[Poll], verdict: Verdict, window_us: int = WINDOW_US, block_us: int = BLOCK_US) -> List[str]:
    """给丢失序号的时间线加上自动标注行。

    参数:
        seq: 被丢序号。
        t_present: 呈现时刻。
        t_next: 下一次呈现时刻。
        polls: 全部调用。
        verdict: classify_loss 的判定。
        window_us: 半宽（成功取帧列表的范围）。
        block_us: 阻塞调用下限。
    返回:
        标注行。
    """
    lines = []
    a = verdict.active_block
    if a is not None:
        lines.append(f"呈现时是否有 poll 正在阻塞: 是（{(a.start_us - t_present) / 1000:+.3f}ms → {(a.end_us - t_present) / 1000:+.3f}ms，耗时 {a.dur_us / 1000:.2f}ms，结果 {POLL_NAMES.get(a.code, a.code)}）")
    else:
        lines.append("呈现时是否有 poll 正在阻塞: 否")
    nxt = next((p for p in polls if p.ok and p.end_us >= t_present), None)
    lines.append("下一次成功取帧距该序号呈现: " + (f"{(nxt.end_us - t_present) / 1000:+.3f}ms（n={nxt.n}）" if nxt else "无"))
    succ = [p for p in polls if p.ok and t_present - window_us <= p.end_us <= t_present + window_us]
    lines.append("期间的成功取帧: " + ("; ".join(f"{(p.end_us - t_present) / 1000:+.2f}ms n={p.n} present={(p.present_us - t_present) / 1000:+.2f}ms" for p in succ) if succ else "无"))
    lines.append(f"下一次呈现: {(t_next - t_present) / 1000:+.3f}ms")
    return lines


def format_summary(polls: Sequence[Poll], presents: Sequence[int], lost_presents: set, interval_us: float, block_us: int) -> Tuple[List[str], dict]:
    """汇总统计（调用耗时分布、阻塞调用结果占比、相位直方图、与呈现的关系、重叠基线）的文本与数据。

    参数:
        polls: 调用列表。
        presents: 全部呈现时刻。
        lost_presents: 被丢序号的呈现时刻集合。
        interval_us: 夹具间隔。
        block_us: 阻塞调用下限。
    返回:
        (文本行, 数据字典)。
    """
    s = summarize_polls(polls, block_us)
    hist = phase_histogram(polls, block_us)
    rel = block_vs_presents(polls, presents, block_us)
    ov = present_overlap(presents, polls, block_us, lost_presents, interval_us)
    out = [f"== (a) 汇总：poll 共 {s['total']} 次，阻塞调用阈值 {block_us / 1000:.1f}ms =="]
    for name, d in s["by_code"].items():
        out.append(f"  {name:<6} 个数={d['count']:>6}  耗时ms p50={d['p50']:.3f} p90={d['p90']:.3f} p99={d['p99']:.3f} max={d['max']:.3f}")
    b = s["blocking"]
    out.append(f"  阻塞调用: {b['count']} 次  结果分布={b['by_code']}  成功占比={b['ok_ratio'] * 100:.1f}%  超时占比={b['timeout_ratio'] * 100:.1f}%")
    d = b["dur_ms"]
    out.append(f"           耗时ms p50={d['p50']:.2f} p90={d['p90']:.2f} max={d['max']:.2f}")
    out.append("  阻塞调用起点相对上一次成功取帧返回的相位（毫秒桶）:")
    peak = max((c for _, c in hist), default=0) or 1
    for lo, c in hist:
        label = f">={lo // 1000}" if lo == hist[-1][0] else f"{lo // 1000}-{lo // 1000 + 1}"
        out.append(f"    {label:>6}ms {c:>5} {'#' * round(40 * c / peak)}")
    out.append("  阻塞调用与夹具呈现的关系（毫秒）:")
    for key, name in (("since_present_ms", "上一次呈现 → 阻塞起点"), ("until_next_present_ms", "阻塞终点 → 下一次呈现")):
        d = rel[key]
        out.append(f"    {name}: n={d['count']} p50={d['p50']:.2f} p90={d['p90']:.2f} max={d['max']:.2f}")
    out.append("== (c) 呈现时刻落在阻塞调用内部的比例 ==")
    out.append(f"  全部呈现 {ov['inside']}/{ov['presents']} = {ov['ratio'] * 100:.1f}%  基线（阻塞时间占比）={ov['baseline_time_ratio'] * 100:.1f}%  基线（相位打乱）={ov['baseline_shift_ratio'] * 100:.1f}%")
    out.append(f"  被丢序号的呈现 {ov['lost_inside']}/{ov['lost_total']} = {ov['lost_ratio'] * 100:.1f}%")
    return out, {"polls": s, "phase_hist": hist, "block_vs_presents": rel, "overlap": ov}


def analyze(fixture: Dict[int, int], trace: tj.Trace, offset_us: Optional[int] = None, lost_seqs: Optional[Sequence[int]] = None, window_us: int = WINDOW_US, block_us: int = BLOCK_US, max_timelines: int = 60, collapse: bool = False) -> Tuple[str, dict]:
    """纯函数：生成完整报告文本与可 JSON 序列化的摘要。

    参数:
        fixture: {序号: 夹具 unix_us}。
        trace: 录制追踪。
        offset_us: 夹具到呈现的偏移；None 时由追踪标定。
        lost_seqs: 被丢序号；None 时由追踪推断。
        window_us: 每个丢失序号的时间线半宽。
        block_us: 阻塞调用下限。
        max_timelines: 最多打印多少个丢失序号的时间线（判定仍对全部给出）。
        collapse: 是否折叠连续的短超时 poll。
    返回:
        (报告文本, 摘要字典)。
    """
    polls, rels = load_polls(trace)
    ordered = sorted(fixture)
    gaps = [fixture[b] - fixture[a] for a, b in zip(ordered, ordered[1:]) if b == a + 1]
    interval = float(statistics.median(gaps)) if gaps else 16667.0
    src = {"offset": "json" if offset_us is not None else "trace", "lost": "json" if lost_seqs is not None else "trace"}
    if offset_us is None:
        est = estimate_offset_from_trace(fixture, polls, interval)
        offset_us = est.offset_us
    if lost_seqs is None:
        lost_seqs = infer_lost(fixture, polls, offset_us)
    lost_seqs = sorted(s for s in lost_seqs if s in fixture)
    presents = [t + offset_us for t in fixture.values()]
    lost_presents = {fixture[s] + offset_us for s in lost_seqs}
    lines = [f"acquire_timeline: poll={len(polls)} release={len(rels)} 夹具序号={len(fixture)} 被丢={len(lost_seqs)} 偏移={offset_us}us(来源:{src['offset']}) 被丢序号来源:{src['lost']} 间隔={interval:.0f}us"]
    overflow = trace.meta.get("poll_overflow")
    if overflow not in (None, "0"):
        lines.append(f"!! poll_overflow={overflow}：逐调用缓冲写满，部分调用未记录，结论需谨慎")
    if not polls:
        lines.append("!! 追踪里没有 poll 事件：请同时设置 SNOW_RECORDER_FRAME_TRACE 与 SNOW_RECORDER_POLL_TRACE=1")
    summary_lines, data = format_summary(polls, presents, lost_presents, interval, block_us)
    lines += summary_lines
    verdicts = []
    lines.append("== (b) 被丢序号的机理判定 ==")
    detail_blocks: List[List[str]] = []
    for seq in lost_seqs:
        t_p = fixture[seq] + offset_us
        t_n = fixture[seq + 1] + offset_us if seq + 1 in fixture else int(t_p + interval)
        v = classify_loss(t_p, t_n, polls, block_us)
        verdicts.append({"seq": seq, "mechanism": v.mechanism, "detail": v.detail})
        lines.append(f"  seq={seq}: {v.mechanism}（{MECHANISM_TEXT[v.mechanism]}）{(' ' + v.detail) if v.detail else ''}")
        if len(detail_blocks) < max_timelines:
            block = [f"---- seq={seq} 呈现时刻(unix_us)={t_p} 判定={v.mechanism} ----"]
            block += ["  " + x for x in annotate(seq, t_p, t_n, polls, v, window_us, block_us)]
            block += ["  " + x for x in build_timeline(seq, t_p, t_n, fixture, offset_us, polls, rels, trace, window_us, block_us, collapse)]
            detail_blocks.append(block)
    tally: Dict[str, int] = {}
    for v in verdicts:
        tally[v["mechanism"]] = tally.get(v["mechanism"], 0) + 1
    lines.append(f"  判定分布: {tally}")
    for block in detail_blocks:
        lines.append("")
        lines += block
    if len(lost_seqs) > max_timelines:
        lines.append(f"\n（只打印前 {max_timelines} 个丢失序号的时间线，其余见判定列表）")
    data.update({"offset_us": offset_us, "offset_source": src["offset"], "lost_source": src["lost"], "lost": verdicts, "mechanisms": tally})
    return "\n".join(lines), data


def main(argv: Optional[Sequence[str]] = None) -> int:
    """命令行入口；成功返回 0，输入错误返回 2。

    参数:
        argv: 参数列表，缺省取 sys.argv。
    """
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--fixture", required=True, help="夹具 frames.csv")
    ap.add_argument("--trace", required=True, help="录制追踪 csv（含 poll/release 事件）")
    ap.add_argument("--join", help="可选：trace_join.py 的 --json 输出（取被丢序号与时钟偏移）")
    ap.add_argument("--window-ms", type=float, default=WINDOW_US / 1000, help="每个丢失序号时间线的半宽（毫秒）")
    ap.add_argument("--block-us", type=int, default=BLOCK_US, help="阻塞调用的耗时下限（微秒）")
    ap.add_argument("--max-timelines", type=int, default=60, help="最多打印多少个丢失序号的时间线")
    ap.add_argument("--collapse", action="store_true", help="把连续 3 个以上的短超时 poll 折叠成一行（缺省逐条列出）")
    ap.add_argument("--report", help="文本报告输出路径（缺省只打印）")
    ap.add_argument("--json", help="JSON 摘要输出路径")
    args = ap.parse_args(argv)
    try:
        with open(args.fixture, encoding="utf-8") as fh:
            fixture = tj.parse_fixture(fh.read())
        with open(args.trace, encoding="utf-8") as fh:
            trace = tj.parse_trace(fh.read())
        offset: Optional[int] = None
        lost: Optional[List[int]] = None
        if args.join:
            with open(args.join, encoding="utf-8") as fh:
                joined = json.load(fh)
            offset = int(joined["clock"]["offset_us"]) if joined.get("clock", {}).get("calibrated") else None
            lost = [int(r["seq"]) for r in joined.get("lost", [])]
    except (OSError, ValueError, KeyError) as e:
        print(f"错误: {e}", file=sys.stderr)
        return 2
    text, data = analyze(fixture, trace, offset, lost, int(args.window_ms * 1000), args.block_us, args.max_timelines, args.collapse)
    print(text)
    if args.report:
        with open(args.report, "w", encoding="utf-8") as fh:
            fh.write(text + "\n")
    if args.json:
        with open(args.json, "w", encoding="utf-8") as fh:
            json.dump(data, fh, ensure_ascii=False, indent=2)
    return 0


if __name__ == "__main__":
    sys.exit(main())
