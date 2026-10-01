"""trace_join.py 的离线测试：全部用合成数据，覆盖 a/b/c/d/e 类归因、重复帧、乱序、时钟偏移与停顿簇。"""

import json
import unittest
from typing import List, Optional, Tuple

import trace_join as tj

# 60fps 的夹具出帧间隔（微秒）与时间基准。
INTERVAL = 16667
BASE = 1_700_000_000_000_000
FPS = 60


def ev(event, t_us, cap=0, slot=None, code=0, n=0, dur=0, present=None, src=0, thread="cap") -> tj.Ev:
    """构造一条追踪事件（单调时钟直接取 t_us*1000）。"""
    return tj.Ev(t_us * 1000, t_us, event, thread, src, cap, slot, present, n, dur, code)


class Scenario:
    """合成一次录制的三份数据：夹具时刻、追踪事件与成品帧。"""

    def __init__(self, count: int = 14, offset: int = 3000, stall_after: int = 0, stall_us: int = 0):
        self.offset = offset
        # stall_after 之后的序号整体推迟 stall_us（模拟夹具停顿）
        self.fixture = {s: BASE + s * INTERVAL + (stall_us if s > stall_after else 0) for s in range(1, count + 1)}
        self.events: List[tj.Ev] = []
        self.product: List[Tuple[int, Optional[int]]] = []
        self.next_cap = 1

    def present(self, seq: int) -> int:
        """该序号的 DXGI 呈现时刻（夹具 Present 返回时刻 + 常数偏移）。"""
        return self.fixture[seq] + self.offset

    def acquire(self, seq: int, accum: int = 1, outcome: int = tj.ACQ_FRESH, enqueue: bool = True) -> int:
        """记录取帧（含入队/入合成队列），返回捕获序号（采集池丢弃为 0）。"""
        t = self.present(seq) + 500
        if outcome == tj.ACQ_POOL_DROP:
            self.events.append(ev("acquire", t, 0, None, outcome, accum, 0, self.present(seq)))
            return 0
        cap = self.next_cap
        self.next_cap += 1
        self.events.append(ev("acquire", t, cap, None, outcome, accum, 0, self.present(seq)))
        if enqueue:
            self.events.append(ev("enqueue", t + 50, cap, None, 1))
            self.events.append(ev("absorb", t + 100, cap, None, 1, thread="cmp"))
        return cap

    def emit(self, slot: int, cap: int, kind: int = tj.SLOT_NEW, compose: bool = True, sent: bool = True) -> None:
        """记录槽选择及其下游（合成/送编码）。"""
        t = self.fixture[min(self.fixture)] + slot * INTERVAL + 9000
        self.events.append(ev("slot", t, cap, slot, kind, thread="cmp"))
        if compose:
            self.events.append(ev("compose", t + 300, cap, slot, 0, dur=300, thread="cmp"))
        if sent:
            self.events.append(ev("send", t + 350, 0, slot, thread="cmp"))
            self.events.append(ev("submit", t + 400, 0, slot, dur=50, thread="enc"))

    def deliver(self, seq: int, slot: Optional[int] = None, accum: int = 1) -> int:
        """幸存序号的完整路径：取帧 -> 选中 -> 合成 -> 送编码 -> 出现在成品里。"""
        slot = seq if slot is None else slot
        cap = self.acquire(seq, accum)
        self.emit(slot, cap)
        self.product.append((slot, seq))
        return cap

    def trace(self) -> tj.Trace:
        """按时间排好序的追踪。"""
        return tj.Trace({"fps": str(FPS), "overflow": "0", "backend": "mock"}, sorted(self.events, key=lambda e: e.mono_ns))

    def run(self, params: Optional[tj.Params] = None) -> dict:
        """联接分析。"""
        frames = [(slot / FPS, seq) for slot, seq in self.product]
        return tj.analyze(self.fixture, self.trace(), frames, FPS, params)


def by_seq(summary: dict) -> dict:
    """{序号: 类别}。"""
    return {r["seq"]: r["category"] for r in summary["lost"]}


def build_mixed(offset: int = 3000, stall_after: int = 0, stall_us: int = 0) -> Scenario:
    """a/b/c/d/e 各一例的混合场景：3=合并，5=槽取代，8=编码丢失，10=无法归因，12=采集池丢弃。"""
    s = Scenario(14, offset, stall_after, stall_us)
    s.deliver(1)
    s.deliver(2)
    # 3：DXGI 合并，下一次取帧 AccumulatedFrames=2
    s.deliver(4, accum=2)
    # 5：取到并入队，6 到来后槽 6 取更新的帧，5 被取代丢弃；槽 5 为空
    cap5 = s.acquire(5)
    s.events.append(ev("slot", s.fixture[1] + 5 * INTERVAL + 9000, 0, 5, tj.SLOT_EMPTY, thread="cmp"))
    cap6 = s.acquire(6)
    s.emit(6, cap6)
    s.events.append(ev("discard", s.fixture[1] + 6 * INTERVAL + 9000, cap5, 6, tj.DISCARD_SUPERSEDED, thread="cmp"))
    s.product.append((6, 6))
    s.deliver(7)
    # 8：已选中、合成并送编码，但成品里没有该槽
    cap8 = s.acquire(8)
    s.emit(8, cap8)
    s.deliver(9)
    # 10：没有任何取帧，下一次取帧 AccumulatedFrames=1
    s.deliver(11)
    # 12：采集池耗尽
    s.acquire(12, outcome=tj.ACQ_POOL_DROP)
    s.deliver(13)
    s.deliver(14)
    return s


class AttributionTests(unittest.TestCase):
    """丢帧归因。"""

    def test_each_category_is_attributed(self):
        summary = build_mixed().run()
        self.assertEqual(by_seq(summary), {3: tj.CAT_A, 5: tj.CAT_B, 8: tj.CAT_C, 10: tj.CAT_D, 12: tj.CAT_E})
        reasons = {r["seq"]: r["reason"] for r in summary["lost"]}
        self.assertEqual(reasons, {3: "coalesced_by_dxgi", 5: "superseded", 8: "encoder_lost", 10: "no_acquire_covering", 12: "pool_exhausted"})
        self.assertEqual(summary["counts"]["lost"], 5)
        self.assertEqual(summary["categories"][tj.CAT_A], 1)
        details = {r["seq"]: r["detail"] for r in summary["lost"]}
        self.assertIn("AccumulatedFrames=2", details[3])
        self.assertIn("槽 6", details[5])
        json.dumps(summary)  # 必须可序列化
        text = tj.format_report(summary)
        for category in tj.CATEGORIES:
            self.assertIn(category, text)

    def test_clock_offset_is_calibrated_away(self):
        for offset in (0, 3000, 9000, -7000):
            summary = build_mixed(offset).run()
            self.assertEqual(by_seq(summary), {3: tj.CAT_A, 5: tj.CAT_B, 8: tj.CAT_C, 10: tj.CAT_D, 12: tj.CAT_E}, f"offset={offset}")
            clock = summary["clock"]
            self.assertTrue(clock["calibrated"])
            self.assertAlmostEqual(clock["offset_us"], offset, delta=2)
            self.assertLess(clock["spread_p95_us"], 5)

    def test_compose_missing_and_surface_exhausted(self):
        s = Scenario(8)
        s.deliver(1)
        s.deliver(2)
        cap3 = s.acquire(3)
        s.emit(3, cap3, compose=False, sent=False)  # 选中但没有合成完成记录
        cap4 = s.acquire(4)
        s.emit(4, cap4, kind=tj.SLOT_SURFACE_EXHAUSTED, compose=False, sent=False)
        s.deliver(5)
        s.deliver(6)
        summary = s.run()
        self.assertEqual({r["seq"]: r["reason"] for r in summary["lost"]}, {3: "compose_missing", 4: "surface_pool_exhausted"})
        self.assertTrue(all(r["category"] == tj.CAT_C for r in summary["lost"]))

    def test_queue_overflow_and_left_in_queue(self):
        s = Scenario(8)
        s.deliver(1)
        cap2 = s.acquire(2)
        s.events.append(ev("discard", s.present(2) + 800, cap2, None, tj.DISCARD_OVERFLOW, thread="cmp"))
        s.acquire(3)  # 入队后既没被选中也没被丢弃
        s.deliver(4)
        s.deliver(5)
        summary = s.run()
        self.assertEqual({r["seq"]: (r["category"], r["reason"]) for r in summary["lost"]}, {2: (tj.CAT_B, "queue_overflow"), 3: (tj.CAT_B, "left_in_queue")})

    def test_slot_seq_conflict_is_unattributed(self):
        # 按时间匹配到的取帧所在槽，成品里却显示另一个序号：不能硬归因
        s = Scenario(6)
        s.deliver(1)
        cap2 = s.acquire(2)
        s.emit(2, cap2)
        s.product.append((2, 4))  # 槽 2 实际显示序号 4
        s.deliver(3, slot=3)
        s.deliver(5)
        s.deliver(6)
        summary = s.run()
        conflict = [r for r in summary["lost"] if r["reason"] == "slot_seq_conflict"]
        self.assertEqual([r["category"] for r in conflict], [tj.CAT_D])

    def test_outside_trace_window_is_unattributed(self):
        s = Scenario(8)
        for q in (1, 2, 3, 7):
            s.deliver(q)
        # 追踪缓冲只留下了后半段（模拟溢出/较晚开启）：序号 4~6 的时刻在追踪覆盖范围之前
        s.events = [e for e in s.events if e.unix_us >= s.fixture[7]]
        summary = s.run()
        self.assertEqual({r["seq"]: r["reason"] for r in summary["lost"]}, {4: "outside_trace_window", 5: "outside_trace_window", 6: "outside_trace_window"})
        self.assertTrue(all(r["category"] == tj.CAT_D for r in summary["lost"]))

    def test_no_loss_gives_empty_report(self):
        s = Scenario(10)
        for q in range(1, 11):
            s.deliver(q)
        summary = s.run()
        self.assertEqual((summary["counts"]["lost"], summary["lost"]), (0, []))
        self.assertIn("被丢序号 0", tj.format_report(summary))


class DuplicateAndOrderTests(unittest.TestCase):
    """重复帧与乱序成因。"""

    def test_duplicate_causes(self):
        s = Scenario(8)
        for q in range(1, 6):
            s.deliver(q)
        # 槽 6：光标补帧，复用序号 5 的桌面帧
        cap5 = s.next_cap - 1
        s.emit(6, cap5, kind=tj.SLOT_CURSOR_REUSE)
        s.product.append((6, 5))
        # 槽 7：同一个捕获序号再次被选中
        s.emit(7, cap5)
        s.product.append((7, 5))
        # 槽 8：序号 6 被两次呈现取到两个不同的捕获序号，都送出
        s.deliver(6, slot=8)
        capx = s.acquire(6)
        s.emit(9, capx)
        s.product.append((9, 6))
        summary = s.run()
        causes = {d["slot"]: d["cause"] for d in summary["duplicates"]["items"]}
        self.assertEqual(causes, {6: tj.DUP_CURSOR_REUSE, 7: tj.DUP_SAME_CAP, 9: tj.DUP_REPEAT_PRESENT})
        self.assertEqual(summary["counts"]["duplicates"], 3)

    def test_out_of_order_causes(self):
        def run(use: Tuple[int, int]) -> dict:
            """槽 2、槽 3 分别用第 use[0]、use[1] 个取到的帧，成品里序号 5 -> 4。"""
            s = Scenario(6)
            s.deliver(1)
            caps = [s.acquire(4), s.acquire(5)]  # 取帧次序：序号 4 的帧在前
            s.emit(2, caps[use[0]])
            s.emit(3, caps[use[1]])
            s.product.extend([(2, 5), (3, 4)])
            return s.run()

        # 取帧次序与槽次序一致，序号却倒退：来源（夹具/DXGI）乱序
        self.assertEqual(run((0, 1))["out_of_order"]["items"][0]["cause"], tj.OOO_SOURCE_ORDER)
        # 后一个槽用了更早取到的帧：时间槽按呈现时间排序造成的重排
        self.assertEqual(run((1, 0))["out_of_order"]["items"][0]["cause"], tj.OOO_PRESENT_REORDER)

    def test_unknown_without_trace_records(self):
        s = Scenario(4)
        s.product = [(1, 1), (2, 3), (3, 2), (4, 2)]
        summary = s.run()
        self.assertEqual(summary["out_of_order"]["items"][0]["cause"], tj.OOO_UNKNOWN)
        self.assertEqual(summary["duplicates"]["items"][0]["cause"], tj.DUP_UNKNOWN)


class StallTests(unittest.TestCase):
    """停顿窗口与多环节同时停顿判定。"""

    def regular_fixture(self, count=40, stall_after=20, stall_us=0):
        fx, t = {}, BASE
        for q in range(1, count + 1):
            fx[q] = t
            t += INTERVAL + (stall_us if q == stall_after else 0)
        return fx

    def acquires(self, fixture, skip_between=None):
        out = []
        for q, t in fixture.items():
            out.append(ev("acquire", t + 3500, q, None, tj.ACQ_FRESH, 1, 0, t + 3000))
        if skip_between is not None:
            out = [e for e in out if e.cap_id != skip_between]
        return out

    def test_find_gaps(self):
        self.assertEqual(tj.find_gaps([0, 10, 20, 60, 70, 80], 2.0), [(20, 60, 40)])
        self.assertEqual(tj.find_gaps([0, 10], 2.0), [])

    def test_fixture_stall_with_slow_compose_is_multi_stage(self):
        fx = self.regular_fixture(stall_us=60000)
        stall_t = fx[20] + INTERVAL + 30000
        events = self.acquires(fx) + [ev("compose", t, q, q, 0, 0, 300, thread="cmp") for q, t in fx.items()]
        events.append(ev("compose", stall_t, 99, 99, 0, 0, 25000, thread="cmp"))
        trace = tj.Trace({}, sorted(events, key=lambda e: e.mono_ns))
        windows = tj.find_stall_windows(fx, trace, FPS, tj.Params())
        self.assertEqual({w.series for w in windows}, {tj.SERIES_FIXTURE, tj.SERIES_ACQUIRE, tj.SERIES_COMPOSE})
        clusters = tj.cluster_windows(windows, tj.Params().margin_us)
        self.assertEqual(len(clusters), 1)
        self.assertTrue(clusters[0]["multi_stage"])
        # 取帧间隔与夹具停顿重叠 -> 源头驱动，不算独立环节
        self.assertEqual(clusters[0]["independent"], [tj.SERIES_COMPOSE, tj.SERIES_FIXTURE])

    def test_fixture_stall_alone_is_not_multi_stage(self):
        fx = self.regular_fixture(stall_us=60000)
        trace = tj.Trace({}, sorted(self.acquires(fx), key=lambda e: e.mono_ns))
        clusters = tj.cluster_windows(tj.find_stall_windows(fx, trace, FPS, tj.Params()), tj.Params().margin_us)
        self.assertEqual(len(clusters), 1)
        self.assertFalse(clusters[0]["multi_stage"])
        self.assertEqual(clusters[0]["independent"], [tj.SERIES_FIXTURE])

    def test_recorder_side_acquire_gap_with_slow_compose_is_multi_stage(self):
        fx = self.regular_fixture()
        events = [e for e in self.acquires(fx) if e.cap_id not in (20, 21, 22)]  # 夹具正常出帧，但录制侧取帧停了约 50ms
        stall_t = fx[21]
        events += [ev("compose", t, q, q, 0, 0, 300, thread="cmp") for q, t in fx.items()]
        events.append(ev("compose", stall_t + 5000, 98, 98, 0, 0, 12000, thread="cmp"))
        trace = tj.Trace({}, sorted(events, key=lambda e: e.mono_ns))
        clusters = tj.cluster_windows(tj.find_stall_windows(fx, trace, FPS, tj.Params()), tj.Params().margin_us)
        self.assertEqual(len(clusters), 1)
        self.assertTrue(clusters[0]["multi_stage"])
        self.assertEqual(clusters[0]["independent"], [tj.SERIES_ACQUIRE, tj.SERIES_COMPOSE])

    def test_lost_seq_inside_stall_cluster_is_flagged(self):
        # 序号 8 之后夹具停了 40ms，同一时刻合成也变慢；被丢的 8 落在该多环节停顿内，其余丢帧不在
        s = build_mixed(stall_after=8, stall_us=40000)
        s.events.append(ev("compose", s.fixture[8] + 4000, 777, 777, 0, 0, 20000, thread="cmp"))
        summary = s.run()
        flagged = {r["seq"]: r["in_multi_stage_stall"] for r in summary["lost"]}
        self.assertTrue(flagged[8])
        self.assertFalse(flagged[3])
        self.assertFalse(flagged[5])
        self.assertGreaterEqual(summary["lost_in_stall"]["multi_stage"], 1)
        self.assertGreaterEqual(summary["stalls"]["multi_stage_clusters"], 1)


def slow_ev(t_us, req=0, over=0, acq=0, lock=0, copy=0, gap=0, total=5000) -> tj.Ev:
    """构造一条 slow_iter 事件（采集线程）。"""
    return tj.Ev(t_us * 1000, t_us, "slow_iter", "cap", 0, 0, None, None, 0, total, 0, req, over, acq, lock, copy, gap)


class SlowIterCauseTests(unittest.TestCase):
    """取帧停顿窗口的成因归因（slow_iter）。"""

    def test_classify_picks_largest_segment(self):
        self.assertEqual(tj.classify_slow_iter(slow_ev(0, req=400, over=8000, acq=100, gap=8400)), (tj.CAUSE_SLEEP, 8000))
        self.assertEqual(tj.classify_slow_iter(slow_ev(0, req=400, over=100, acq=9000, gap=500)), (tj.CAUSE_ACQUIRE, 9000))
        self.assertEqual(tj.classify_slow_iter(slow_ev(0, req=400, over=100, lock=6000, copy=6500, gap=500)), (tj.CAUSE_LOCK, 6000))
        # 复制耗时含等锁，扣除等锁后才是复制本身
        self.assertEqual(tj.classify_slow_iter(slow_ev(0, req=400, over=100, lock=500, copy=7000, gap=500)), (tj.CAUSE_COPY, 6500))
        # 上一圈不是以睡眠结束：间隔算调用方里的时间，只有此时才归 caller_gap
        self.assertEqual(tj.classify_slow_iter(slow_ev(0, req=0, gap=12000)), (tj.CAUSE_CALLER, 12000))
        self.assertEqual(tj.classify_slow_iter(slow_ev(0, req=400, over=0, gap=0)), (tj.CAUSE_OTHER, 0))

    def test_attribute_window_cases(self):
        slow = [slow_ev(1000, req=400, over=300, gap=700), slow_ev(5000, req=400, over=30000, gap=30400), slow_ev(99000, acq=50000)]
        self.assertEqual(tj.attribute_window(0, 10, slow, False), (tj.CAUSE_NODATA, []))
        self.assertEqual(tj.attribute_window(2000, 3000, slow, True), (tj.CAUSE_NONE, []))
        cause, items = tj.attribute_window(0, 10000, slow, True)
        self.assertEqual((cause, len(items)), (tj.CAUSE_SLEEP, 2))  # 取最大单项分段所在的那条
        self.assertEqual(items[1]["sleep_over_us"], 30000)
        self.assertEqual(tj.attribute_window(90000, 100000, slow, True)[0], tj.CAUSE_ACQUIRE)

    def stalled(self, slow_events, with_meta=True):
        """夹具正常出帧、录制侧取帧停了约 50ms 的场景，返回 (夹具, 追踪)。"""
        st = StallTests()
        fx = st.regular_fixture()
        events = [e for e in st.acquires(fx) if e.cap_id not in (20, 21, 22)] + slow_events(fx)
        meta = {"slow_iter_us": "4000"} if with_meta else {}
        return fx, tj.Trace(meta, sorted(events, key=lambda e: e.mono_ns))

    def test_window_and_lost_seq_get_cause(self):
        fx, trace = self.stalled(lambda fx: [slow_ev(fx[21], req=400, over=45000, gap=45400, total=45500)])
        windows = tj.find_stall_windows(fx, trace, FPS, tj.Params())
        acq = [w for w in windows if w.series == tj.SERIES_ACQUIRE]
        self.assertEqual((len(acq), acq[0].cause, acq[0].source_driven), (1, tj.CAUSE_SLEEP, False))
        self.assertEqual(len(acq[0].slow_iters), 1)
        clusters = tj.cluster_windows(windows, tj.Params().margin_us)
        self.assertEqual(clusters[0]["windows"][0]["cause"], tj.CAUSE_SLEEP)
        rec = tj.LostRecord(21, fx[21], tj.CAT_A, "coalesced_by_dxgi", "", in_stall=[0])
        self.assertEqual(tj.lost_stall_cause(rec, clusters, 3000, tj.Params().margin_us), tj.CAUSE_SLEEP)
        self.assertEqual(tj.lost_stall_cause(tj.LostRecord(5, fx[5], tj.CAT_A, "x", ""), clusters, 0, 8000), "")

    def test_no_slow_iter_and_no_meta(self):
        fx, trace = self.stalled(lambda fx: [])
        acq = [w for w in tj.find_stall_windows(fx, trace, FPS, tj.Params()) if w.series == tj.SERIES_ACQUIRE]
        self.assertEqual(acq[0].cause, tj.CAUSE_NONE)
        fx, old = self.stalled(lambda fx: [], with_meta=False)
        acq = [w for w in tj.find_stall_windows(fx, old, FPS, tj.Params()) if w.series == tj.SERIES_ACQUIRE]
        self.assertEqual(acq[0].cause, tj.CAUSE_NODATA)

    def test_source_driven_window_is_not_counted_in_cause_tally(self):
        s = build_mixed(stall_after=8, stall_us=40000)
        summary = s.run()
        self.assertEqual(sum(summary["stalls"]["acquire_causes"].values()), sum(1 for c in summary["stalls"]["clusters"] for w in c["windows"] if w["series"] == tj.SERIES_ACQUIRE and not w["source_driven"]))
        self.assertIsInstance(summary["lost_stall_causes"], dict)
        self.assertIn("stall_cause", summary["lost"][0])
        tj.format_report(summary)

    def test_report_lists_slow_iter_detail_and_cause_distribution(self):
        fx, trace = self.stalled(lambda fx: [slow_ev(fx[21], req=400, over=45000, gap=45400, total=45500)])
        trace.meta.update({"capture_sched": "mmcss", "capture_sched_detail": "mmcss(Capture) 已生效"})
        frames = [(q / FPS, q) for q in fx if q not in (20, 21, 22)]
        summary = tj.analyze(fx, trace, frames, FPS)
        text = tj.format_report(summary)
        self.assertEqual(summary["stalls"]["acquire_causes"], {tj.CAUSE_SLEEP: 1})
        self.assertEqual((summary["stalls"]["capture_sched"], summary["stalls"]["slow_iters"]), ("mmcss", 1))
        self.assertIn("慢迭代 t", text)
        self.assertIn("取帧间隔窗口成因分布（不含源头驱动）: sleep_overrun=1", text)
        self.assertIn("采集线程调度: mmcss(Capture) 已生效", text)


class ParsingTests(unittest.TestCase):
    """CSV 解析、标定与槽号对齐。"""

    def test_parse_slow_iter_columns_and_old_format(self):
        new = (
            "# slow_iter_us=4000\n"
            "mono_ns,unix_us,event,thread,src,cap_id,slot,present_unix_us,n,dur_us,code,sleep_req_us,sleep_over_us,acq_us,lock_us,copy_us,gap_us\n"
            "10,1010,slow_iter,cap,0,0,,,0,9000,0,400,8000,100,5,70,8400\n"
            "20,1020,acquire,cap,0,1,,1000,1,0,0,,,,,,\n"
        )
        t = tj.parse_trace(new)
        self.assertEqual(t.meta["slow_iter_us"], "4000")
        slow, acq = t.events
        self.assertEqual((slow.event, slow.dur_us, slow.sleep_req_us, slow.sleep_over_us, slow.acq_us, slow.lock_us, slow.copy_us, slow.gap_us), ("slow_iter", 9000, 400, 8000, 100, 5, 70, 8400))
        self.assertEqual((acq.sleep_over_us, acq.gap_us), (0, 0))  # 其余事件这几列留空，取 0
        old = tj.parse_trace("mono_ns,unix_us,event,thread,src,cap_id,slot,present_unix_us,n,dur_us,code\n1,2,acquire,cap,0,1,,,1,0,0\n")
        self.assertEqual((len(old.events), old.events[0].gap_us), (1, 0))

    TRACE = (
        "# snow-recorder frame trace v1\n# fps=60\n# backend=mock\n# overflow=2\n"
        "mono_ns,unix_us,event,thread,src,cap_id,slot,present_unix_us,n,dur_us,code\n"
        "2000000,1002000,slot,cmp,0,5,7,,0,0,1\n"
        "1000000,1001000,acquire,cap,1,5,,1000500,2,0,0\n"
        "bad,line\n"
    )

    def test_parse_trace(self):
        t = tj.parse_trace(self.TRACE)
        self.assertEqual((t.meta["fps"], t.meta["backend"], t.meta["overflow"]), ("60", "mock", "2"))
        self.assertEqual([e.event for e in t.events], ["acquire", "slot"])  # 按单调时钟排序，坏行被跳过
        self.assertEqual((t.events[0].present_us, t.events[0].n, t.events[0].src), (1000500, 2, 1))
        self.assertEqual((t.events[1].slot, t.events[1].present_us), (7, None))

    def test_parse_fixture(self):
        self.assertEqual(tj.parse_fixture("seq,submit_ns,flush_ns,unix_us\n1,10,10,1000\nx,1,1,1\n2,20,20,2000\n"), {1: 1000, 2: 2000})

    def test_estimate_offset(self):
        off = tj.estimate_offset([(0, 3000), (16667, 19667), (33334, 36334), (50001, 60001)])
        self.assertEqual(off.offset_us, 3000)  # 中位数不受离群点影响
        self.assertTrue(off.calibrated)
        self.assertFalse(tj.estimate_offset([]).calibrated)

    def test_align_slots_handles_container_start_offset(self):
        prod = [(s + 5, s) for s in range(10)]  # 容器 pts 起点偏了 5 个槽
        self.assertEqual(tj.align_slots(prod, set(range(10))), -5)
        self.assertEqual(tj.align_slots([(s, s) for s in range(10)], set(range(10))), 0)

    def test_product_slots_round_pts(self):
        self.assertEqual(tj.product_slots([(0.0, 1), (0.0334, 2), (0.0999, None)], 30), [(0, 1), (1, 2), (3, None)])


if __name__ == "__main__":
    unittest.main()
