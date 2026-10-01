#!/usr/bin/env python3
"""联接夹具 frames.csv、录制进程帧追踪 csv 与成品 mp4，给每个被丢的序号做链路归因。

用法:
    python trace_join.py --fixture 夹具.frames.csv --trace 录制追踪.csv --video 成品.mp4 \\
        [--fps 60] [--refresh 59] [--ffmpeg-dir C:\\ProgramData\\chocolatey\\bin] \\
        [--report 报告.txt] [--json 摘要.json] [--env env.json]

数据链路（序号从夹具到成品经过的每一段）:
    夹具提交(frames.csv) -> DXGI 取帧(acquire，带 LastPresentTime 与 AccumulatedFrames) -> 采集池/入队(enqueue)
    -> 待输出队列(absorb) -> 时间槽选择(slot / discard / skip) -> 合成(compose) -> 送编码(send/submit) -> 成品

归因类别（每个"夹具提交了但成品里没有"的序号落入其一，详见 CATEGORY_TEXT）:
    a_dxgi_coalesced      DXGI 从未单独交付：取帧被合并，且覆盖它的那次取帧 AccumulatedFrames>1
    b_slot_dropped        取到了，但在待输出队列里被时间槽选择取代/溢出丢弃，或一直没被选中
    c_compose_encode_lost 被选中，但合成/编码阶段丢失（表面池耗尽、合成缺失、送编码后成品里没有该槽）
    d_unattributed        无法归因（时间对不上、追踪不完整等，原因写在 reason 里）
    e_capture_pool_drop   取到了，但采集共享槽耗尽，在采集阶段被丢弃

时钟对齐: 追踪里的挂钟 = 录制进程启动追踪时的 (单调时钟, 挂钟) 背靠背读数 + 单调增量（误差微秒级，见 frametrace.rs）；
夹具的 unix_us 是 Present 返回时刻，与"DXGI 呈现时刻"相差一个近似常数。本脚本用成品里幸存的序号
（序号 -> 所在槽 -> 捕获序号 -> 呈现时刻）自标定该常数（中位数），并给出残差分布。

依赖: Python 3 标准库；ffmpeg/ffprobe 经 fps_analyze 的管道调用（目录默认 C:\\ProgramData\\chocolatey\\bin，可用 --ffmpeg-dir 覆盖）。
"""

import argparse
import csv
import io
import json
import statistics
import sys
from dataclasses import dataclass, field
from typing import Dict, List, Optional, Sequence, Set, Tuple

# ---- 与 snow-recorder/src/frametrace.rs 的约定一致的事件码 ----
ACQ_FRESH, ACQ_CURSOR_ONLY, ACQ_POOL_DROP, ACQ_UNWANTED = 0, 1, 2, 3
DISCARD_SUPERSEDED, DISCARD_OVERFLOW = 1, 2
SLOT_EMPTY, SLOT_NEW, SLOT_CURSOR_REUSE, SLOT_SURFACE_EXHAUSTED, SLOT_PROBE = 0, 1, 2, 3, 4

# ---- 归因类别 ----
CAT_A = "a_dxgi_coalesced"
CAT_B = "b_slot_dropped"
CAT_C = "c_compose_encode_lost"
CAT_D = "d_unattributed"
CAT_E = "e_capture_pool_drop"
CATEGORIES = (CAT_A, CAT_B, CAT_C, CAT_D, CAT_E)
# 类别说明（写进报告）。
CATEGORY_TEXT = {
    CAT_A: "夹具提交后 DXGI 从未单独交付：取帧被合并，覆盖它的那次取帧 AccumulatedFrames>1",
    CAT_B: "取到了，但被待输出队列的时间槽选择取代/溢出丢弃，或入队后始终没被选中",
    CAT_C: "被时间槽选中，但合成/编码阶段丢失（表面池耗尽、合成缺失、送编码后成品里没有该槽）",
    CAT_D: "无法归因（时间对不上或追踪不完整，原因见 reason）",
    CAT_E: "取到了，但采集共享槽耗尽，在采集阶段被丢弃",
}
# 重复帧成因。
DUP_CURSOR_REUSE = "cursor_reuse"
DUP_SAME_CAP = "same_cap_reselected"
DUP_REPEAT_PRESENT = "repeat_present"
DUP_UNKNOWN = "unknown_no_trace"
# 乱序成因。
OOO_PRESENT_REORDER = "present_time_reordered"
OOO_SOURCE_ORDER = "source_order"
OOO_UNKNOWN = "unknown_no_trace"
# 停顿序列名。
SERIES_FIXTURE = "fixture_gap"
SERIES_ACQUIRE = "acquire_gap"
SERIES_COMPOSE = "compose_slow"
SERIES_SUBMIT = "submit_slow"
SERIES_SKIP = "slot_skip"


@dataclass
class Params:
    """分析参数。"""

    gate_ratio: float = 0.75  # 候选取帧与夹具序号的最大时间差 = 该比值 * 夹具出帧间隔
    stall_ratio: float = 1.5  # 间隔超过 中位数 * 该比值 视为停顿
    compose_floor_us: int = 4000  # 合成/送帧耗时停顿的下限（微秒），同时要求 > 3 倍中位数
    margin_us: int = 8000  # 停顿窗口合并与"丢帧落在停顿内"判定的时间余量（微秒）


@dataclass
class Ev:
    """追踪 csv 的一行。"""

    mono_ns: int
    unix_us: int
    event: str
    thread: str
    src: int
    cap_id: int
    slot: Optional[int]
    present_us: Optional[int]
    n: int
    dur_us: int
    code: int


@dataclass
class Trace:
    """解析后的追踪：元数据与按时间排序的事件。"""

    meta: Dict[str, str] = field(default_factory=dict)
    events: List[Ev] = field(default_factory=list)


def _opt_int(cell: str) -> Optional[int]:
    """空字符串返回 None，否则转 int。"""
    cell = cell.strip()
    return int(cell) if cell else None


def parse_trace(text: str) -> Trace:
    """解析录制进程写出的帧追踪 csv（`#` 开头的是元数据行）。

    参数:
        text: 文件全文。
    返回:
        Trace；无法解析的行被跳过。
    示例:
        >>> t = parse_trace("# fps=60\\nmono_ns,unix_us,event,thread,src,cap_id,slot,present_unix_us,n,dur_us,code\\n1000,5,slot,cmp,0,2,3,,0,0,1\\n")
        >>> (t.meta["fps"], t.events[0].slot)
        ('60', 3)
    """
    trace = Trace()
    body: List[str] = []
    for line in text.splitlines():
        if line.startswith("#"):
            key, _, value = line[1:].strip().partition("=")
            if value or "=" in line:
                trace.meta[key.strip()] = value.strip()
        elif line.strip():
            body.append(line)
    for row in csv.DictReader(io.StringIO("\n".join(body))):
        try:
            trace.events.append(
                Ev(
                    mono_ns=int(row["mono_ns"]),
                    unix_us=int(row["unix_us"]),
                    event=row["event"],
                    thread=row["thread"],
                    src=int(row["src"]),
                    cap_id=int(row["cap_id"]),
                    slot=_opt_int(row["slot"]),
                    present_us=_opt_int(row["present_unix_us"]),
                    n=int(row["n"]),
                    dur_us=int(row["dur_us"]),
                    code=int(row["code"]),
                )
            )
        except (KeyError, ValueError):
            continue
    trace.events.sort(key=lambda e: e.mono_ns)
    return trace


def parse_fixture(text: str) -> Dict[int, int]:
    """解析夹具 frames.csv（表头 seq,submit_ns,flush_ns,unix_us），返回 {序号: unix_us}。

    参数:
        text: 文件全文。
    返回:
        字典；无法解析的行被跳过。
    示例:
        >>> parse_fixture("seq,submit_ns,flush_ns,unix_us\\n1,10,10,1000\\n")
        {1: 1000}
    """
    out: Dict[int, int] = {}
    for row in csv.DictReader(io.StringIO(text)):
        try:
            out[int(row["seq"])] = int(row["unix_us"])
        except (KeyError, ValueError):
            continue
    return out


# ---------------------------------------------------------------- 链路重建


@dataclass
class CapInfo:
    """一个捕获序号在各阶段的足迹。"""

    cap_id: int
    ord: int = -1  # 在桌面取帧序列里的次序（时间顺序），-1 表示未见取帧事件
    src: int = 0
    acquire_us: Optional[int] = None
    present_us: Optional[int] = None
    accum: int = 1
    outcome: int = ACQ_FRESH
    enqueue_us: Optional[int] = None
    absorb_us: Optional[int] = None
    discards: List[Tuple[int, Optional[int]]] = field(default_factory=list)  # (原因码, 触发槽)
    selected: List[Tuple[int, int]] = field(default_factory=list)  # (槽号, 槽类型码)


@dataclass
class SlotInfo:
    """一个时间槽的选择与下游足迹。"""

    slot: int
    cap_id: int
    kind: int
    select_us: int = 0
    compose_end_us: Optional[int] = None
    compose_dur_us: Optional[int] = None
    send_us: Optional[int] = None
    submit_us: Optional[int] = None
    submit_dur_us: Optional[int] = None
    consumed_us: Optional[int] = None


@dataclass
class Acq:
    """一次带桌面内容的取帧（含被采集池丢弃的）。"""

    ord: int
    cap_id: int  # 被采集池丢弃的为 0
    src: int
    acquire_us: int
    present_us: int
    accum: int
    outcome: int


@dataclass
class Chain:
    """重建后的链路。"""

    caps: Dict[int, CapInfo] = field(default_factory=dict)
    acquires: List[Acq] = field(default_factory=list)
    slots: Dict[int, SlotInfo] = field(default_factory=dict)
    skips: List[Ev] = field(default_factory=list)
    idles: int = 0
    first_us: int = 0
    last_us: int = 0


def build_chain(trace: Trace) -> Chain:
    """把事件流按捕获序号与槽号重建成链路。

    参数:
        trace: parse_trace 的结果。
    返回:
        Chain。
    """
    chain = Chain()
    if trace.events:
        chain.first_us, chain.last_us = trace.events[0].unix_us, trace.events[-1].unix_us
    for e in trace.events:
        if e.event == "acquire":
            if e.code in (ACQ_FRESH, ACQ_POOL_DROP) and e.present_us is not None:
                acq = Acq(len(chain.acquires), e.cap_id, e.src, e.unix_us, e.present_us, e.n, e.code)
                chain.acquires.append(acq)
                if e.code == ACQ_FRESH:
                    chain.caps[e.cap_id] = CapInfo(e.cap_id, acq.ord, e.src, e.unix_us, e.present_us, e.n, e.code)
        elif e.event == "idle":
            chain.idles += 1
        elif e.event == "enqueue":
            cap = chain.caps.setdefault(e.cap_id, CapInfo(e.cap_id))
            if e.code == 1 and cap.enqueue_us is None:
                cap.enqueue_us = e.unix_us
        elif e.event == "absorb":
            cap = chain.caps.setdefault(e.cap_id, CapInfo(e.cap_id))
            if cap.absorb_us is None:
                cap.absorb_us = e.unix_us
        elif e.event == "discard":
            chain.caps.setdefault(e.cap_id, CapInfo(e.cap_id)).discards.append((e.code, e.slot))
        elif e.event == "slot" and e.slot is not None:
            chain.slots[e.slot] = SlotInfo(e.slot, e.cap_id, e.code, e.unix_us)
            if e.cap_id:
                chain.caps.setdefault(e.cap_id, CapInfo(e.cap_id)).selected.append((e.slot, e.code))
        elif e.event == "skip":
            chain.skips.append(e)
        elif e.slot is not None and e.slot in chain.slots:
            info = chain.slots[e.slot]
            if e.event == "compose":
                info.compose_end_us, info.compose_dur_us = e.unix_us, e.dur_us
            elif e.event == "send":
                info.send_us = e.unix_us
            elif e.event == "submit":
                info.submit_us, info.submit_dur_us = e.unix_us, e.dur_us
            elif e.event == "consumed":
                info.consumed_us = e.unix_us
    return chain


# ---------------------------------------------------------------- 成品映射与时钟标定


def product_slots(frames: Sequence[Tuple[float, Optional[int]]], fps: float) -> List[Tuple[int, Optional[int]]]:
    """成品每帧 (pts 秒, 序号) 换成 (槽号, 序号)；槽号 = round(pts * fps)。

    参数:
        frames: 成品每帧的 (pts 秒, 序号或 None)。
        fps: 输出帧率。
    示例:
        >>> product_slots([(0.0, 1), (1 / 30, 2)], 30)
        [(0, 1), (1, 2)]
    """
    return [(int(round(pts * fps)), seq) for pts, seq in frames]


def align_slots(prod: Sequence[Tuple[int, Optional[int]]], submit_slots: Set[int]) -> int:
    """成品槽号与追踪里送编码槽号的整体平移量（容器起点偏移时用）；对得上则为 0。

    参数:
        prod: product_slots 的结果。
        submit_slots: 追踪里有 submit/slot 记录的槽号。
    返回:
        要加到成品槽号上的平移量。
    """
    if not prod or not submit_slots:
        return 0
    first = min(s for s, _ in prod)
    candidates = [0, min(submit_slots) - first]
    return max(candidates, key=lambda d: sum(1 for s, _ in prod if s + d in submit_slots))


@dataclass
class Offset:
    """夹具时间到呈现时间的偏移标定。"""

    offset_us: int = 0  # 呈现时刻 - 夹具 unix_us 的中位数
    spread_p95_us: int = 0  # 去掉中位数后的 95 分位绝对偏差
    pairs: int = 0
    calibrated: bool = False


def estimate_offset(pairs: Sequence[Tuple[int, int]]) -> Offset:
    """由 (夹具 unix_us, 呈现 unix_us) 配对估计常数偏移与离散程度。

    参数:
        pairs: 幸存序号的配对。
    返回:
        Offset；没有配对时 calibrated=False、偏移为 0。
    示例:
        >>> estimate_offset([(0, 3000), (16667, 19667), (33334, 36334)]).offset_us
        3000
    """
    if not pairs:
        return Offset()
    diffs = sorted(p - f for f, p in pairs)
    med = int(statistics.median(diffs))
    dev = sorted(abs(d - med) for d in diffs)
    return Offset(med, dev[min(len(dev) - 1, int(len(dev) * 0.95))], len(diffs), True)


# ---------------------------------------------------------------- 丢帧归因


@dataclass
class LostRecord:
    """一个被丢序号的归因结果。"""

    seq: int
    fixture_us: int
    category: str
    reason: str
    detail: str
    cap_ids: List[int] = field(default_factory=list)
    slot: Optional[int] = None
    residual_us: Optional[int] = None
    in_stall: List[int] = field(default_factory=list)  # 命中的停顿簇下标
    in_multi_stage_stall: bool = False


def _progress(cap: CapInfo) -> int:
    """捕获序号走得多远（用于在多个候选里挑最能解释丢失的）。"""
    if cap.selected:
        return 4
    if cap.discards:
        return 3
    if cap.enqueue_us is not None:
        return 2
    return 1


def _fate(cap: CapInfo, chain: Chain, prod_by_slot: Dict[int, Optional[int]], seq: int) -> Tuple[str, str, str, Optional[int]]:
    """判断一个已匹配上的捕获序号为什么没能变成成品里的该序号，返回 (类别, 原因, 说明, 槽号)。"""
    if cap.outcome == ACQ_POOL_DROP:
        return CAT_E, "pool_exhausted", "取帧时共享纹理池没有空槽", None
    if cap.enqueue_us is None:
        return CAT_D, "never_enqueued", f"cap {cap.cap_id} 有取帧记录但没有入队记录（追踪被截断或采集线程提前结束）", None
    for slot, kind in cap.selected:
        info = chain.slots.get(slot)
        if kind == SLOT_SURFACE_EXHAUSTED:
            return CAT_C, "surface_pool_exhausted", f"槽 {slot} 选中它，但编码表面池耗尽", slot
        if info is None or info.compose_end_us is None:
            return CAT_C, "compose_missing", f"槽 {slot} 选中它，但没有合成完成记录", slot
        if slot not in prod_by_slot:
            sent = "已送编码" if info.submit_us is not None else "未见送编码"
            return CAT_C, "encoder_lost", f"槽 {slot} 已合成（{sent}），成品里没有该槽", slot
        shown = prod_by_slot[slot]
        return CAT_D, "slot_seq_conflict", f"槽 {slot} 在成品里显示序号 {shown}，与按时间匹配到的序号 {seq} 不符（匹配有误或夹具画面与时刻不一致）", slot
    for reason, slot in cap.discards:
        if reason == DISCARD_OVERFLOW:
            return CAT_B, "queue_overflow", "待输出队列溢出，被挤掉的最旧帧", slot
        return CAT_B, "superseded", f"槽 {slot} 取了更新的帧，它被取代丢弃", slot
    return CAT_B, "left_in_queue", "入队后既没被选中也没被丢弃（停止时仍在队列，或被暂停清空）", None


def attribute_lost(
    fixture: Dict[int, int],
    chain: Chain,
    prod: Sequence[Tuple[int, Optional[int]]],
    interval_us: float,
    offset: Offset,
    params: Params,
) -> List[LostRecord]:
    """对每个"夹具提交了、成品区间内却没有"的序号做归因。

    参数:
        fixture: {序号: 夹具 unix_us}。
        chain: build_chain 的结果。
        prod: 成品 (槽号, 序号)（已对齐到追踪槽号）。
        interval_us: 夹具相邻序号的典型间隔（微秒）。
        offset: 夹具时间 -> 呈现时间的标定。
        params: 分析参数。
    返回:
        按序号排序的 LostRecord 列表。
    """
    prod_by_slot = {s: q for s, q in prod}
    shown = {q for _, q in prod if q is not None and q in fixture}
    if not shown:
        return []
    lo, hi = min(shown), max(shown)
    lost = sorted(s for s in fixture if lo <= s <= hi and s not in shown)
    # 幸存序号 -> 它所在槽对应的捕获序号
    survivor_caps: Dict[int, List[int]] = {}
    for slot, seq in prod:
        info = chain.slots.get(slot)
        if seq is not None and info is not None and info.cap_id:
            survivor_caps.setdefault(seq, []).append(info.cap_id)
    survivors = sorted(survivor_caps)
    gate = params.gate_ratio * interval_us

    def ord_of(cap_id: int) -> Optional[int]:
        cap = chain.caps.get(cap_id)
        return cap.ord if cap is not None and cap.ord >= 0 else None

    # 每个丢失序号的候选取帧：夹下界/上界幸存序号的取帧次序，再按时间就近分配给"序号主人"
    cands: Dict[int, List[Acq]] = {s: [] for s in lost}
    groups: Dict[Tuple[Optional[int], Optional[int]], List[int]] = {}
    for s in lost:
        a = max((x for x in survivors if x < s), default=None)
        b = min((x for x in survivors if x > s), default=None)
        groups.setdefault((a, b), []).append(s)
    for (a, b), members in groups.items():
        ord_a = max((o for o in (ord_of(c) for c in survivor_caps.get(a, [])) if o is not None), default=-1) if a is not None else -1
        ord_b = min((o for o in (ord_of(c) for c in survivor_caps.get(b, [])) if o is not None), default=len(chain.acquires)) if b is not None else len(chain.acquires)
        owners = [(s, fixture[s] + offset.offset_us) for s in ([a] if a is not None and a in fixture else []) + members + ([b] if b is not None and b in fixture else [])]
        for acq in chain.acquires:
            if not ord_a < acq.ord < ord_b:
                continue
            owner, dist = min(((s, abs(acq.present_us - p)) for s, p in owners), key=lambda x: x[1])
            if dist <= gate and owner in cands:
                cands[owner].append(acq)

    out: List[LostRecord] = []
    for s in lost:
        p = fixture[s] + offset.offset_us
        matched = cands[s]
        if matched:
            def rank(acq: Acq) -> Tuple[int, int]:
                cap = chain.caps.get(acq.cap_id)
                return (_progress(cap) if cap is not None else 1, -acq.ord)

            best = max(matched, key=rank)
            cap = chain.caps.get(best.cap_id) or CapInfo(best.cap_id, outcome=best.outcome)
            if best.outcome == ACQ_POOL_DROP:
                cap = CapInfo(0, best.ord, best.src, best.acquire_us, best.present_us, best.accum, ACQ_POOL_DROP)
            category, reason, detail, slot = _fate(cap, chain, prod_by_slot, s)
            ids = [a.cap_id for a in matched]
            out.append(LostRecord(s, fixture[s], category, reason, detail, ids, slot, abs(best.present_us - p)))
            continue
        # 没有任何取帧对得上：看紧随其后的那次取帧是否把它合并了
        nxt = next((a for a in chain.acquires if a.present_us > p + gate), None)
        if p < chain.first_us - gate or p > chain.last_us + gate:
            out.append(LostRecord(s, fixture[s], CAT_D, "outside_trace_window", "该序号的时刻在追踪覆盖范围之外（追踪缓冲溢出或录制已开始/结束）"))
        elif nxt is not None and nxt.accum >= 2 and nxt.present_us - p <= nxt.accum * interval_us + gate:
            out.append(
                LostRecord(
                    s,
                    fixture[s],
                    CAT_A,
                    "coalesced_by_dxgi",
                    f"此后第一次取帧（cap {nxt.cap_id}，呈现 {nxt.present_us}us）AccumulatedFrames={nxt.accum}，覆盖了它",
                    [nxt.cap_id],
                    None,
                    nxt.present_us - p,
                )
            )
        else:
            why = "之后没有取帧" if nxt is None else f"此后第一次取帧 AccumulatedFrames={nxt.accum}，不足以覆盖"
            out.append(LostRecord(s, fixture[s], CAT_D, "no_acquire_covering", f"没有任何取帧对得上，且{why}（DXGI 没有交付、夹具未真正上屏或时钟标定不准）"))
    return out


# ---------------------------------------------------------------- 重复帧与乱序


def classify_duplicates(prod: Sequence[Tuple[int, Optional[int]]], chain: Chain) -> List[dict]:
    """给成品里重复出现的序号归因。

    参数:
        prod: 成品 (槽号, 序号)，按显示顺序。
        chain: 链路。
    返回:
        [{slot, seq, cause}]。
    """
    first_cap: Dict[int, int] = {}
    out: List[dict] = []
    for slot, seq in prod:
        if seq is None:
            continue
        info = chain.slots.get(slot)
        cap = info.cap_id if info is not None else 0
        if seq in first_cap:
            if info is None:
                cause = DUP_UNKNOWN
            elif info.kind == SLOT_CURSOR_REUSE:
                cause = DUP_CURSOR_REUSE
            elif cap and cap == first_cap[seq]:
                cause = DUP_SAME_CAP
            elif cap and first_cap[seq]:
                cause = DUP_REPEAT_PRESENT
            else:
                cause = DUP_UNKNOWN
            out.append({"slot": slot, "seq": seq, "cause": cause})
        else:
            first_cap[seq] = cap
    return out


def classify_out_of_order(prod: Sequence[Tuple[int, Optional[int]]], chain: Chain) -> List[dict]:
    """给成品里"序号倒退"的位置归因（与 fps_analyze 的乱序口径一致：相邻帧序号变小）。

    参数:
        prod: 成品 (槽号, 序号)，按显示顺序。
        chain: 链路。
    返回:
        [{slot, seq, prev_seq, cause}]。
    """
    out: List[dict] = []
    decoded = [(s, q) for s, q in prod if q is not None]
    for (ps, pq), (s, q) in zip(decoded, decoded[1:]):
        if q >= pq:
            continue
        a, b = chain.slots.get(ps), chain.slots.get(s)
        oa = chain.caps[a.cap_id].ord if a is not None and a.cap_id in chain.caps else -1
        ob = chain.caps[b.cap_id].ord if b is not None and b.cap_id in chain.caps else -1
        if oa < 0 or ob < 0:
            cause = OOO_UNKNOWN
        elif ob < oa:
            cause = OOO_PRESENT_REORDER
        else:
            cause = OOO_SOURCE_ORDER
        out.append({"slot": s, "seq": q, "prev_seq": pq, "cause": cause})
    return out


# ---------------------------------------------------------------- 停顿窗口


@dataclass
class Window:
    """一个停顿窗口。"""

    series: str
    start_us: int
    end_us: int
    detail: str = ""
    source_driven: bool = False  # 仅取帧间隔：与夹具停顿重叠，说明是源头没出帧而非录制进程卡住


def find_gaps(times: Sequence[int], ratio: float) -> List[Tuple[int, int, int]]:
    """在升序时刻序列里找间隔异常大的段。

    参数:
        times: 升序时刻（微秒）。
        ratio: 超过中位间隔的倍数才算异常。
    返回:
        [(起点, 终点, 间隔)]。
    示例:
        >>> find_gaps([0, 10, 20, 60, 70], 2.0)
        [(20, 60, 40)]
    """
    gaps = [b - a for a, b in zip(times, times[1:])]
    if len(gaps) < 3:
        return []
    limit = statistics.median(gaps) * ratio
    return [(a, b, b - a) for a, b in zip(times, times[1:]) if b - a > limit]


def _slow(durs: List[Tuple[int, int]], floor_us: int) -> List[Tuple[int, int]]:
    """耗时序列 (结束时刻, 耗时) 里超过 max(下限, 3 倍中位数) 的项。"""
    if not durs:
        return []
    limit = max(floor_us, 3 * statistics.median(d for _, d in durs))
    return [(t, d) for t, d in durs if d > limit]


def find_stall_windows(fixture: Dict[int, int], trace: Trace, fps: float, params: Params) -> List[Window]:
    """收集夹具提交、DXGI 取帧、合成、送编码、跳槽五类停顿窗口。

    参数:
        fixture: {序号: 夹具 unix_us}。
        trace: 追踪。
        fps: 输出帧率（跳槽窗口宽度用）。
        params: 分析参数。
    返回:
        按起点排序的窗口列表。
    """
    windows: List[Window] = []
    fx_times = sorted(fixture.values())
    for a, b, g in find_gaps(fx_times, params.stall_ratio):
        windows.append(Window(SERIES_FIXTURE, a, b, f"夹具提交间隔 {g / 1000:.1f}ms"))
    fresh = sorted(e.unix_us for e in trace.events if e.event == "acquire" and e.code == ACQ_FRESH)
    fixture_windows = [w for w in windows if w.series == SERIES_FIXTURE]
    for a, b, g in find_gaps(fresh, params.stall_ratio):
        driven = any(w.start_us - params.margin_us <= b and a - params.margin_us <= w.end_us for w in fixture_windows)
        windows.append(Window(SERIES_ACQUIRE, a, b, f"取帧间隔 {g / 1000:.1f}ms", driven))
    compose = [(e.unix_us, e.dur_us) for e in trace.events if e.event == "compose"]
    for t, d in _slow(compose, params.compose_floor_us):
        windows.append(Window(SERIES_COMPOSE, t - d, t, f"合成耗时 {d / 1000:.1f}ms"))
    submit = [(e.unix_us, e.dur_us) for e in trace.events if e.event == "submit"]
    for t, d in _slow(submit, params.compose_floor_us):
        windows.append(Window(SERIES_SUBMIT, t - d, t, f"送帧耗时 {d / 1000:.1f}ms"))
    period = int(1e6 / fps) if fps > 0 else 16667
    for e in trace.events:
        if e.event == "skip":
            windows.append(Window(SERIES_SKIP, e.unix_us - e.n * period, e.unix_us, f"合成线程落后，跳过 {e.n} 个槽（从槽 {e.slot}）"))
    windows.sort(key=lambda w: (w.start_us, w.end_us))
    return windows


def cluster_windows(windows: Sequence[Window], margin_us: int) -> List[dict]:
    """把时间上重叠（含余量）的停顿窗口并成簇，并判断是否多个独立环节同时停顿。

    独立环节 = 夹具提交、取帧（仅"非源头驱动"的）、合成、送帧、跳槽；
    取帧间隔若与夹具停顿重叠，只是夹具没出帧的后果，不算独立证据。
    >=2 个独立环节同时停顿 -> multi_stage（暗示系统级抖动）。

    参数:
        windows: 按起点排序的窗口。
        margin_us: 合并余量。
    返回:
        [{start_us, end_us, series, independent, multi_stage, windows}]。
    """
    clusters: List[dict] = []
    for w in windows:
        if clusters and w.start_us <= clusters[-1]["end_us"] + margin_us:
            c = clusters[-1]
            c["end_us"] = max(c["end_us"], w.end_us)
            c["members"].append(w)
        else:
            clusters.append({"start_us": w.start_us, "end_us": w.end_us, "members": [w]})
    for c in clusters:
        members: List[Window] = c.pop("members")
        c["series"] = sorted({w.series for w in members})
        c["independent"] = sorted({w.series for w in members if not (w.series == SERIES_ACQUIRE and w.source_driven)})
        c["multi_stage"] = len(c["independent"]) >= 2
        c["windows"] = [{"series": w.series, "start_us": w.start_us, "end_us": w.end_us, "detail": w.detail, "source_driven": w.source_driven} for w in members]
    return clusters


# ---------------------------------------------------------------- 汇总


def analyze(
    fixture: Dict[int, int],
    trace: Trace,
    product: Sequence[Tuple[float, Optional[int]]],
    fps: float,
    params: Optional[Params] = None,
    product_stats: Optional[dict] = None,
) -> dict:
    """纯函数：联接三份数据，输出可 JSON 序列化的摘要。

    参数:
        fixture: {序号: 夹具 unix_us}。
        trace: 录制追踪。
        product: 成品每帧 (pts 秒, 序号或 None)。
        fps: 输出帧率。
        params: 分析参数，缺省取默认。
        product_stats: 可选，成品统计（fps_analyze）原样放进摘要。
    返回:
        摘要字典（字段见模块文档与 format_report）。
    """
    params = params or Params()
    chain = build_chain(trace)
    prod_raw = product_slots(product, fps)
    shift = align_slots(prod_raw, set(chain.slots))
    prod = [(s + shift, q if q in fixture else None) for s, q in prod_raw]
    # 夹具相邻序号的典型间隔
    ordered = sorted(fixture)
    gaps = [fixture[b] - fixture[a] for a, b in zip(ordered, ordered[1:]) if b == a + 1]
    interval_us = float(statistics.median(gaps)) if gaps else (1e6 / fps if fps > 0 else 16667.0)
    # 用幸存序号标定夹具时间 -> 呈现时间的偏移
    pairs: List[Tuple[int, int]] = []
    seen: Set[int] = set()
    for slot, seq in prod:
        info = chain.slots.get(slot)
        cap = chain.caps.get(info.cap_id) if info is not None else None
        if seq is not None and seq not in seen and cap is not None and cap.present_us is not None and info is not None and info.kind in (SLOT_NEW, SLOT_PROBE):
            seen.add(seq)
            pairs.append((fixture[seq], cap.present_us))
    offset = estimate_offset(pairs)
    lost = attribute_lost(fixture, chain, prod, interval_us, offset, params)
    dups = classify_duplicates(prod, chain)
    ooo = classify_out_of_order(prod, chain)
    windows = find_stall_windows(fixture, trace, fps, params)
    clusters = cluster_windows(windows, params.margin_us)
    for rec in lost:
        for i, c in enumerate(clusters):
            lo, hi = c["start_us"] - params.margin_us, c["end_us"] + params.margin_us
            if lo <= rec.fixture_us <= hi or lo <= rec.fixture_us + offset.offset_us <= hi:
                rec.in_stall.append(i)
                rec.in_multi_stage_stall |= c["multi_stage"]
    counts = {c: 0 for c in CATEGORIES}
    reasons: Dict[str, int] = {}
    for rec in lost:
        counts[rec.category] += 1
        reasons[f"{rec.category}/{rec.reason}"] = reasons.get(f"{rec.category}/{rec.reason}", 0) + 1
    acqs = chain.acquires
    return {
        "version": 1,
        "fps": fps,
        "product": product_stats or {},
        "counts": {
            "fixture_frames": len(fixture),
            "product_frames": len(prod),
            "lost": len(lost),
            "duplicates": len(dups),
            "out_of_order": len(ooo),
        },
        "clock": {
            "offset_us": offset.offset_us,
            "spread_p95_us": offset.spread_p95_us,
            "pairs": offset.pairs,
            "calibrated": offset.calibrated,
            "slot_shift": shift,
            "fixture_interval_us": round(interval_us, 1),
        },
        "trace": {
            "records": len(trace.events),
            "overflow": int(trace.meta.get("overflow", "0") or 0),
            "backend": trace.meta.get("backend", ""),
            "acquires_with_content": len(acqs),
            "coalesced_total": sum(max(a.accum - 1, 0) for a in acqs),
            "pool_drops": sum(1 for a in acqs if a.outcome == ACQ_POOL_DROP),
            "idle_timeouts": chain.idles,
            "skipped_slot_events": len(chain.skips),
            "discards": {
                "superseded": sum(1 for c in chain.caps.values() for r, _ in c.discards if r == DISCARD_SUPERSEDED),
                "overflow": sum(1 for c in chain.caps.values() for r, _ in c.discards if r == DISCARD_OVERFLOW),
            },
        },
        "categories": counts,
        "reasons": reasons,
        "lost": [
            {
                "seq": r.seq,
                "fixture_us": r.fixture_us,
                "category": r.category,
                "reason": r.reason,
                "detail": r.detail,
                "cap_ids": r.cap_ids,
                "slot": r.slot,
                "residual_us": r.residual_us,
                "in_stall": r.in_stall,
                "in_multi_stage_stall": r.in_multi_stage_stall,
            }
            for r in lost
        ],
        "duplicates": {"count": len(dups), "causes": _tally(d["cause"] for d in dups), "items": dups},
        "out_of_order": {"count": len(ooo), "causes": _tally(d["cause"] for d in ooo), "items": ooo},
        "stalls": {
            "thresholds": {"stall_ratio": params.stall_ratio, "compose_floor_us": params.compose_floor_us, "margin_us": params.margin_us},
            "window_count": len(windows),
            "clusters": clusters,
            "multi_stage_clusters": sum(1 for c in clusters if c["multi_stage"]),
        },
        "lost_in_stall": {
            "any": sum(1 for r in lost if r.in_stall),
            "multi_stage": sum(1 for r in lost if r.in_multi_stage_stall),
        },
    }


def _tally(items) -> Dict[str, int]:
    """统计各取值出现的次数。"""
    out: Dict[str, int] = {}
    for item in items:
        out[item] = out.get(item, 0) + 1
    return out


def format_report(summary: dict) -> str:
    """把摘要排成可读的中文文本报告。

    参数:
        summary: analyze 的结果。
    """
    c, clk, tr = summary["counts"], summary["clock"], summary["trace"]
    lines = [
        "=== 帧链路追踪联接报告 ===",
        f"夹具序号 {c['fixture_frames']}，成品帧 {c['product_frames']}，被丢序号 {c['lost']}，重复帧 {c['duplicates']}，乱序 {c['out_of_order']}",
        f"追踪记录 {tr['records']} 条（缓冲溢出 {tr['overflow']}），后端 {tr['backend'] or '未知'}；"
        f"带桌面内容的取帧 {tr['acquires_with_content']} 次，DXGI 合并 {tr['coalesced_total']}，采集池丢弃 {tr['pool_drops']}，"
        f"取帧空闲超时 {tr['idle_timeouts']}，槽选择取代丢弃 {tr['discards']['superseded']}，队列溢出 {tr['discards']['overflow']}，跳槽事件 {tr['skipped_slot_events']}",
    ]
    if clk["calibrated"]:
        lines.append(
            f"时钟标定: 呈现时刻 - 夹具 Present 返回时刻 = {clk['offset_us'] / 1000:+.2f}ms（{clk['pairs']} 个幸存序号配对，"
            f"去偏移后 p95 偏差 {clk['spread_p95_us'] / 1000:.2f}ms；夹具间隔 {clk['fixture_interval_us'] / 1000:.2f}ms）"
        )
        if clk["spread_p95_us"] > 0.5 * clk["fixture_interval_us"]:
            lines.append("  ! 偏差接近半个出帧间隔：按时间的匹配可能串位，类别 b/c/d 的结论请谨慎")
    else:
        lines.append("时钟标定: 没有可用的幸存序号配对，按偏移 0 处理（结论可信度低）")
    if clk["slot_shift"]:
        lines.append(f"成品槽号与追踪槽号整体平移 {clk['slot_shift']}")
    lines.append("")
    lines.append("--- 被丢序号归因类别 ---")
    for cat in CATEGORIES:
        lines.append(f"{cat:<24}{summary['categories'][cat]:>4}  {CATEGORY_TEXT[cat]}")
    if summary["reasons"]:
        lines.append("细分原因: " + "; ".join(f"{k}={v}" for k, v in sorted(summary["reasons"].items())))
    if summary["lost"]:
        lines.append("")
        lines.append(f"{'序号':>6} {'夹具时刻(ms)':>12} {'类别':<24}{'原因':<22}说明")
        t0 = min(r["fixture_us"] for r in summary["lost"])
        for r in summary["lost"]:
            flag = " [停顿窗口内" + ("，多环节同时停顿" if r["in_multi_stage_stall"] else "") + "]" if r["in_stall"] else ""
            lines.append(f"{r['seq']:>6} {(r['fixture_us'] - t0) / 1000:>12.1f} {r['category']:<24}{r['reason']:<22}{r['detail']}{flag}")
    lines.append("")
    lines.append(f"--- 重复帧 {summary['duplicates']['count']}（成因: {summary['duplicates']['causes'] or '无'}）---")
    lines.append(f"--- 乱序 {summary['out_of_order']['count']}（成因: {summary['out_of_order']['causes'] or '无'}）---")
    st = summary["stalls"]
    lines.append("")
    lines.append(f"--- 停顿窗口 {st['window_count']} 个，合并成 {len(st['clusters'])} 簇，其中多环节同时停顿 {st['multi_stage_clusters']} 簇 ---")
    base = summary["lost"][0]["fixture_us"] if summary["lost"] else (st["clusters"][0]["start_us"] if st["clusters"] else 0)
    for i, cl in enumerate(st["clusters"][:30]):
        tag = "多环节同时停顿(疑似系统级抖动)" if cl["multi_stage"] else "单环节"
        lines.append(f"  簇{i}: t{(cl['start_us'] - base) / 1000:+.1f}..{(cl['end_us'] - base) / 1000:+.1f}ms 环节={','.join(cl['series'])} 独立={','.join(cl['independent'])} -> {tag}")
        for w in cl["windows"][:6]:
            lines.append(f"       {w['series']:<13} {w['detail']}" + ("（源头驱动）" if w["source_driven"] else ""))
    lines.append(f"被丢序号落在停顿窗口内: {summary['lost_in_stall']['any']}/{c['lost']}，其中多环节同时停顿: {summary['lost_in_stall']['multi_stage']}")
    return "\n".join(lines)


# ---------------------------------------------------------------- 命令行


def main(argv: Optional[Sequence[str]] = None) -> int:
    """命令行入口；成功输出报告返回 0，输入/工具错误返回 2。

    参数:
        argv: 参数列表，缺省取 sys.argv。
    """
    import fps_analyze as fa  # 复用成品解码：find_tool / probe_frames / decode_video / compute_stats
    import summarize_runs as sr  # 复用达标判据

    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--fixture", required=True, help="夹具 frames.csv")
    ap.add_argument("--trace", required=True, help="录制进程帧追踪 csv（SNOW_RECORDER_FRAME_TRACE）")
    ap.add_argument("--video", required=True, help="成品 mp4")
    ap.add_argument("--fps", type=float, default=0, help="输出帧率（缺省取追踪元数据）")
    ap.add_argument("--refresh", type=float, default=0, help="判定用刷新率 Hz（缺省 fps-1）")
    ap.add_argument("--ffmpeg-dir", default=r"C:\ProgramData\chocolatey\bin", help="ffmpeg/ffprobe 所在目录")
    ap.add_argument("--report", help="文本报告输出路径（缺省只打印）")
    ap.add_argument("--json", help="JSON 摘要输出路径")
    ap.add_argument("--env", help="可选：run-fps-test.ps1 -Trace 产出的 env.json，其 tier/interfered 并入摘要")
    ap.add_argument("--stall-ratio", type=float, default=Params.stall_ratio)
    ap.add_argument("--margin-ms", type=float, default=Params.margin_us / 1000)
    args = ap.parse_args(argv)
    try:
        with open(args.fixture, encoding="utf-8") as fh:
            fixture = parse_fixture(fh.read())
        with open(args.trace, encoding="utf-8") as fh:
            trace = parse_trace(fh.read())
        fps = args.fps or float(trace.meta.get("fps", "0") or 0)
        if fps <= 0:
            print("错误: 需要 --fps 或追踪元数据里的 fps", file=sys.stderr)
            return 2
        ffmpeg, ffprobe = fa.find_tool("ffmpeg", args.ffmpeg_dir), fa.find_tool("ffprobe", args.ffmpeg_dir)
        pts, duration = fa.probe_frames(ffprobe, args.video)
        seqs = fa.decode_video(ffmpeg, args.video)
    except (OSError, ValueError) as e:
        print(f"错误: {e}", file=sys.stderr)
        return 2
    except Exception as e:  # subprocess.CalledProcessError 等
        print(f"错误: {e}", file=sys.stderr)
        return 2
    n = min(len(pts), len(seqs))
    refresh = args.refresh or max(fps - 1.0, 1.0)
    origin_us = min(fixture.values(), default=0)
    stats = fa.compute_stats(list(zip(pts[:n], seqs[:n])), duration, refresh, {s: (t - origin_us) / 1e6 for s, t in fixture.items()})
    run = {"fps": stats.fps_effective, "drop": stats.drop_rate * 100.0, "frames": float(stats.frames)}
    product_stats = {
        "frames": stats.frames,
        "fps_effective": round(stats.fps_effective, 3),
        "drop_rate": round(stats.drop_rate, 6),
        "dropped_seqs": stats.dropped_seqs,
        "duplicate_frames": stats.duplicate_frames,
        "out_of_order": stats.out_of_order,
        "verdict": stats.verdict,
        "passed": bool(sr.passed(run, int(round(fps)))),
    }
    summary = analyze(fixture, trace, list(zip(pts[:n], seqs[:n])), fps, Params(stall_ratio=args.stall_ratio, margin_us=int(args.margin_ms * 1000)), product_stats)
    if args.env:
        try:
            with open(args.env, encoding="utf-8-sig") as fh:
                env = json.load(fh)
            summary["env"] = {k: env.get(k) for k in ("tag", "size", "fps", "interfered", "interference_reasons")}
        except (OSError, ValueError) as e:
            summary["env"] = {"error": str(e)}
    text = format_report(summary)
    print(text)
    if args.report:
        with open(args.report, "w", encoding="utf-8") as fh:
            fh.write(text + "\n")
    if args.json:
        with open(args.json, "w", encoding="utf-8") as fh:
            json.dump(summary, fh, ensure_ascii=False, indent=2)
    return 0


if __name__ == "__main__":
    sys.exit(main())
