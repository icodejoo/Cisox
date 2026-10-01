"""acquire_timeline.py 的离线测试：全部用合成数据，覆盖机理判定各分支、统计函数、追踪解析与端到端报告。"""

import unittest

import acquire_timeline as at
import trace_join as tj

# 60fps 的夹具出帧间隔（微秒）与时间基准。
INTERVAL = 16667
BASE = 1_700_000_000_000_000
T_S = BASE + 100_000  # 被丢序号的呈现时刻
T_N = T_S + INTERVAL  # 下一次呈现时刻


def ok(start, dur, present, n=1):
    """成功取帧的 poll。"""
    return at.Poll(start, dur, at.POLL_OK, n=n, present_us=present, flags=at.FLAG_PRESENT)


def timeout(start, dur):
    """超时的 poll。"""
    return at.Poll(start, dur, at.POLL_TIMEOUT)


def verdict(polls):
    """对 tS/tN 做判定。"""
    return at.classify_loss(T_S, T_N, sorted(polls, key=lambda p: p.start_us))


class ClassifyLossTests(unittest.TestCase):
    """机理判定：每条规则一个用例。"""

    def test_no_poll_data(self):
        self.assertEqual(verdict([]).mechanism, at.M_NO_DATA)

    def test_normal_frame_is_delivered(self):
        """正常情形：呈现后很快有成功取帧且呈现时刻吻合，不算丢。"""
        v = verdict([timeout(T_S - 3000, 10), ok(T_S + 600, 20, T_S, 1), ok(T_N + 600, 20, T_N, 1)])
        self.assertEqual(v.mechanism, at.M_DELIVERED)

    def test_blocked_through_next_present(self):
        """呈现落在阻塞内部，阻塞到下一次呈现之后才返回并带回 n=2：被覆盖。"""
        block = ok(T_S - 1000, INTERVAL + 2000, T_N, n=2)
        v = verdict([ok(T_S - 5000, 20, T_S - INTERVAL, 1), block])
        self.assertEqual(v.mechanism, at.M_BLOCKED_THROUGH)
        self.assertIs(v.active_block, block)

    def test_blocked_returned_without_it(self):
        """呈现落在阻塞内部，阻塞在下一次呈现之前以超时返回，之后的成功取帧才发现 n=2。"""
        v = verdict([timeout(T_S - 2000, 4400), ok(T_N + 800, 20, T_N, n=2)])
        self.assertEqual(v.mechanism, at.M_BLOCKED_RETURNED)

    def test_no_call_in_window(self):
        """呈现后到下一次呈现之间一次调用都没开始，也没有阻塞调用跨过呈现。"""
        v = verdict([timeout(T_S - 3000, 10), ok(T_N + 800, 20, T_N, n=2)])
        self.assertEqual(v.mechanism, at.M_NO_CALL)

    def test_calls_returned_no_frame(self):
        """窗口内有调用但都超时（没看到该序号），随后才取到 n=2。"""
        v = verdict([timeout(T_S + 500, 10), timeout(T_S + 1500, 10), timeout(T_S + 3000, 10), ok(T_N + 800, 20, T_N, n=2)])
        self.assertEqual(v.mechanism, at.M_CALLS_NO_FRAME)

    def test_not_coalesced_when_cover_has_single_frame(self):
        """覆盖调用 n=1：不能用合并解释。"""
        v = verdict([timeout(T_S - 3000, 10), ok(T_N + 800, 20, T_N, n=1)])
        self.assertEqual(v.mechanism, at.M_NOT_COALESCED)

    def test_not_coalesced_when_no_cover(self):
        self.assertEqual(verdict([timeout(T_S + 100, 10)]).mechanism, at.M_NOT_COALESCED)


class StatsTests(unittest.TestCase):
    """统计纯函数。"""

    def test_percentile_and_dist(self):
        self.assertEqual(at.percentile([], 0.5), 0)
        d = at.dist([5, 1, 3])
        self.assertEqual((d["count"], d["p50"], d["max"]), (3, 3, 5))

    def test_summarize_polls_splits_by_code_and_blocking(self):
        polls = [ok(0, 20, 1), timeout(100, 30), timeout(200, 4400), ok(300, 5200, 2, n=2), at.Poll(400, 10, at.POLL_LOST)]
        s = at.summarize_polls(polls)
        self.assertEqual(s["total"], 5)
        self.assertEqual(s["by_code"]["超时"]["count"], 2)
        self.assertEqual(s["blocking"]["count"], 2)
        self.assertEqual(s["blocking"]["by_code"], {"超时": 1, "成功": 1})
        self.assertAlmostEqual(s["blocking"]["ok_ratio"], 0.5)

    def test_phase_histogram_uses_previous_success_end(self):
        polls = [ok(0, 100, 1), timeout(2600, 4400), ok(10000, 100, 2), timeout(31000, 4400)]
        hist = dict(at.phase_histogram(polls))
        self.assertEqual(hist[2000], 1)  # 2600 - 100 = 2500us
        self.assertEqual(hist[at.PHASE_MAX_US], 1)  # 31000 - 10100 约 21ms，进最后一桶
        self.assertEqual(sum(hist.values()), 2)

    def test_present_overlap_and_baselines(self):
        # 阻塞 [1000, 6000) 内有 1 个呈现，呈现间隔 20ms
        polls = [timeout(1000, 5000)]
        r = at.present_overlap([3000, 23000, 43000, 63000], polls, lost={3000})
        self.assertEqual((r["inside"], r["lost_inside"]), (1, 1))
        self.assertAlmostEqual(r["ratio"], 0.25)
        self.assertAlmostEqual(r["baseline_time_ratio"], 3000 / 60000)  # 观测区间 [3000, 63000]，阻塞被裁成 [3000, 6000)
        self.assertGreaterEqual(r["baseline_shift_ratio"], 0)

    def test_block_vs_presents(self):
        rel = at.block_vs_presents([timeout(1000, 5000)], [0, 20000])
        self.assertAlmostEqual(rel["since_present_ms"]["p50"], 1.0)
        self.assertAlmostEqual(rel["until_next_present_ms"]["p50"], 14.0)

    def test_infer_offset_and_lost(self):
        fixture = {s: BASE + s * INTERVAL for s in range(1, 7)}
        # 呈现 = 夹具 + 3000；序号 3 没有对应的呈现（被合并）
        polls = [ok(fixture[s] + 3500, 20, fixture[s] + 3000) for s in (1, 2, 4, 5, 6)]
        offset = at.estimate_offset_from_trace(fixture, polls, INTERVAL)
        self.assertEqual(offset.offset_us, 3000)
        self.assertEqual(at.infer_lost(fixture, polls, offset.offset_us), [3])


class TraceParsingTests(unittest.TestCase):
    """追踪解析：poll/release 行经 trace_join.parse_trace 后字段映射正确。"""

    CSV = (
        "# poll_trace=1\n# poll_overflow=0\n"
        "mono_ns,unix_us,event,thread,src,cap_id,slot,present_unix_us,n,dur_us,code,sleep_req_us,sleep_over_us,acq_us,lock_us,copy_us,gap_us\n"
        "1000,100,poll,cap,0,0,,,0,4400,1,,,,,,\n"
        "2000,200,poll,cap,0,0,,150,2,30,0,3,7,9,250,,\n"
        "3000,230,release,cap,0,0,,,0,12,0,,,,,,\n"
    )

    def test_load_polls_maps_columns(self):
        polls, rels = at.load_polls(tj.parse_trace(self.CSV))
        self.assertEqual((len(polls), len(rels)), (2, 1))
        self.assertEqual((polls[0].code, polls[0].dur_us, polls[0].ok), (at.POLL_TIMEOUT, 4400, False))
        p = polls[1]
        self.assertEqual((p.n, p.present_us, p.flags, p.pointer_bytes, p.meta_bytes, p.since_release_us, p.ok), (2, 150, 3, 7, 9, 250, True))
        self.assertEqual((rels[0].start_us, rels[0].end_us), (230, 242))


class EndToEndTests(unittest.TestCase):
    """端到端：合成一次含阻塞丢帧的录制，报告含判定、标注与时间线。"""

    def build(self):
        fixture = {s: BASE + s * INTERVAL for s in range(1, 8)}
        offset = 3000
        events = []
        polls_csv = []

        def add_poll(start, dur, code, n=0, present=None):
            polls_csv.append(f"{start * 1000},{start},poll,cap,0,0,,{'' if present is None else present},{n},{dur},{code},,,,,,")

        for s in range(1, 8):
            p = fixture[s] + offset
            if s == 4:
                add_poll(p - 1000, INTERVAL + 2000, at.POLL_OK, n=2, present=fixture[5] + offset)  # 序号 4 在阻塞中被覆盖
            elif s == 5:
                continue
            else:
                add_poll(p + 500, 20, at.POLL_OK, n=1, present=p)
        events.append("mono_ns,unix_us,event,thread,src,cap_id,slot,present_unix_us,n,dur_us,code,sleep_req_us,sleep_over_us,acq_us,lock_us,copy_us,gap_us")
        return fixture, "# poll_trace=1\n# poll_overflow=0\n" + events[0] + "\n" + "\n".join(polls_csv) + "\n", offset

    def test_report_for_blocked_loss(self):
        fixture, csv_text, offset = self.build()
        text, data = at.analyze(fixture, tj.parse_trace(csv_text), offset_us=None, lost_seqs=None)
        self.assertEqual([v["seq"] for v in data["lost"]], [4])
        self.assertEqual(data["lost"][0]["mechanism"], at.M_BLOCKED_THROUGH)
        self.assertEqual(data["offset_us"], offset)
        for needle in ("呈现时是否有 poll 正在阻塞: 是", "下一次成功取帧距该序号呈现", "期间的成功取帧", "<== 丢失序号", "(a) 汇总", "(c) 呈现时刻落在阻塞调用内部"):
            self.assertIn(needle, text)

    def test_collapse_merges_short_timeouts(self):
        fixture, csv_text, offset = self.build()
        extra = "".join(f"{(BASE + i) * 1000},{BASE + 60_000 + i * 100},poll,cap,0,0,,,0,20,1,,,,,,\n" for i in range(10))
        trace = tj.parse_trace(csv_text + extra)
        full, _ = at.analyze(fixture, trace, offset_us=offset, lost_seqs=[4])
        short, _ = at.analyze(fixture, trace, offset_us=offset, lost_seqs=[4], collapse=True)
        self.assertIn("×10 个短超时调用", short)
        self.assertNotIn("×10 个短超时调用", full)

    def test_json_inputs_override_inference(self):
        fixture, csv_text, offset = self.build()
        text, data = at.analyze(fixture, tj.parse_trace(csv_text), offset_us=offset, lost_seqs=[4, 999])
        self.assertEqual((data["offset_source"], data["lost_source"]), ("json", "json"))
        self.assertEqual([v["seq"] for v in data["lost"]], [4])  # 不在夹具里的序号被忽略

    def test_missing_poll_events_warns(self):
        fixture = {1: BASE, 2: BASE + INTERVAL}
        text, data = at.analyze(fixture, tj.parse_trace("mono_ns,unix_us,event,thread,src,cap_id,slot,present_unix_us,n,dur_us,code\n"), offset_us=0, lost_seqs=[1])
        self.assertIn("SNOW_RECORDER_POLL_TRACE=1", text)
        self.assertEqual(data["lost"][0]["mechanism"], at.M_NO_DATA)


if __name__ == "__main__":
    unittest.main()
