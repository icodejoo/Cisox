"""etw_join.py 的离线测试：全部用合成的 tracerpt 事件，覆盖解析、时钟映射、正常配对、被跳过的 Present、frames.csv 对 seq、间隔异常。"""

import unittest
from typing import List, Optional

import etw_join as ej

UNIX_EPOCH_FT = 116444736000000000
VSYNC_US = 16667
TARGET = "0x2141"
APP_PID = 4242
DWM_PID = 1040


def ft_of(unix_us: float) -> int:
    """挂钟 unix 微秒 -> FILETIME（与 from_trace=False 的时钟互逆）。"""
    return int(UNIX_EPOCH_FT + unix_us * 10)


def ev(prov, eid, kind, t_us, payload, pid=DWM_PID, tid=1) -> ej.Ev:
    """构造一条事件，时刻用挂钟 unix 微秒。"""
    return ej.Ev(prov, eid, kind, pid, tid, ft_of(t_us), [str(x) for x in payload])


def vsync(t_us, count) -> ej.Ev:
    """VSyncDPC 事件：载荷 [适配器, target, ?, ?, 计数, QPC]。"""
    return ev(ej.PROV_DXG, 17, "Info", t_us, ["0xA", TARGET, 1, 0, count, 0], pid=0)


class Scene:
    """合成场景：每个 vsync 一个 DWM 帧；按需添加应用 Present 链路与 DWM 呈现。"""

    def __init__(self, n_vsync=60, t0=1_000_000.0):
        self.t0 = t0
        self.events: List[ej.Ev] = []
        for i in range(n_vsync):
            t = t0 + i * VSYNC_US
            self.events.append(vsync(t, 1000 + i))
            self.events.append(ev(ej.PROV_DWM, 10, "Start", t + 200, []))  # DWM 帧起点晚于 vsync 200us
            self.events.append(ev(ej.PROV_DWM, 12, "Stop", t + 1500, []))

    def app_present(self, t_call, token, swapchain="0xSC1", picked=True, test=False, tid=7) -> None:
        """应用一次 Present：42/215/43 事件，可选 DWM 在 1ms 后取走令牌(173)。"""
        flags = "DXGI_PRESENT_TEST" if test else "0"
        self.events.append(ev(ej.PROV_DXGI, 42, "Start", t_call, [swapchain, flags, 1, 1, 0], pid=APP_PID, tid=tid))
        if not test:
            self.events.append(ev(ej.PROV_DXG, 215, "Start", t_call + 5, ["0xT", "0xTD", "D3DKMT_PM_REDIRECTED_FLIP", 0, token], pid=APP_PID, tid=tid))
        self.events.append(ev(ej.PROV_DXGI, 43, "Stop", t_call + 20, [0], pid=APP_PID, tid=tid))
        if picked and not test:
            self.events.append(ev(ej.PROV_DXG, 173, "Stop", t_call + 1000, ["0xA", "0xTD", "D3DKMT_PM_REDIRECTED_FLIP", 840, token], pid=DWM_PID, tid=9))

    def dwm_present_at_frame(self, vsync_index: int) -> None:
        """在第 vsync_index 个帧里加 DWM 呈现(15)与其后 0.8ms 的硬件翻页(259)。"""
        t = self.t0 + vsync_index * VSYNC_US + 300
        self.events.append(ev(ej.PROV_DWM, 15, "Start", t, []))
        self.events.append(ev(ej.PROV_DWM, 16, "Stop", t + 500, []))
        self.events.append(ev(ej.PROV_DXG, 259, "Info", t + 800, [0, 0], pid=0))

    def sorted(self) -> List[ej.Ev]:
        return sorted(self.events, key=lambda e: e.ft)


def identity_clock() -> ej.Clock:
    """FILETIME 直接当挂钟的时钟（测配对用）。"""
    return ej.Clock(0, 10_000_000, 0, 0, a_from_trace=False)


class ParseTests(unittest.TestCase):
    HEADER = "Event Name, Type, Event ID, Version, Channel, Level, Opcode, Task, Keyword, PID, TID, Processor Number, Instance ID, Parent Instance ID, Activity ID, Related Activity ID, Clock-Time, Kernel(ms), User(ms), User Data"

    def row(self, name, kind, eid, pid, tid, clock, ud):
        return f"{name}, {kind}, {eid}, 0, 0, 0, 0, 0, 0x1, {pid}, {tid}, 2, , , {{0}}, , {clock}, 30, 15, {ud}"

    def test_parse_skips_header_event_and_strips_payload(self):
        # EventTrace 头事件不进列表但记录丢失数；载荷里的枚举串去掉尾随空格
        text = "\n".join([
            self.HEADER,
            "EventTrace, Header, 0, 2, 0, 0, 0, 0, 0x0, 0x8080, 0x46DC, 0, , , {0}, , 100, 30, 15, 1048576, 1, 2, 20, 999, 156250, 0, 0x0, 25, 1, 8, 7, 2496",
            self.row("Microsoft-Windows-DXGI", "Start ", 42, "0x00000410", "0x974", 134353010808889287, "0x1F1F, DXGI_PRESENT_TEST , 1"),
        ])
        evs, info = ej.parse_tracerpt_csv(text)
        self.assertEqual(info["header_events_lost"], "7")
        self.assertEqual(len(evs), 1)
        e = evs[0]
        self.assertEqual((e.eid, e.kind, e.pid, e.tid, e.ft), (42, "Start", 0x410, 0x974, 134353010808889287))
        self.assertEqual(e.payload, ["0x1F1F", "DXGI_PRESENT_TEST", "1"])


class ClockTests(unittest.TestCase):
    def test_a_from_vsync_and_anchor_mapping(self):
        # 构造 A=1e17，VSync 事件时间戳比内嵌 QPC 晚 0~20 个 100ns；锚点 (qpc=5e9 ticks -> unix 2e15 us)
        a = 100_000_000_000_000_000
        evs = []
        for i in range(20):
            q = 5_000_000_000 + i * 166_700
            evs.append(ej.Ev(ej.PROV_DXG, 17, "Info", 0, 0, a + q + i, ["0xA", TARGET, 1, 0, i, str(q)]))
        a_est, spread = ej.estimate_a_ft(evs, 10_000_000)
        self.assertEqual(a_est, a + 1)  # 5 分位：延迟最小的那批
        self.assertLess(spread, 5)
        clk = ej.make_clock(evs, {"freq": 10_000_000, "anchors": [{"qpc": 5_000_000_000, "unix_us": 2_000_000_000_000_000, "bracket_ticks": 2},
                                                                   {"qpc": 5_050_000_000, "unix_us": 2_000_000_005_000_000, "bracket_ticks": 3}]})
        # 锚点处 QPC=5e9 -> unix 2e15；再过 100000 ticks(10ms) -> +10000us
        self.assertAlmostEqual(clk.to_unix_us(a + 5_000_000_000), 2_000_000_000_000_000, delta=1)
        self.assertAlmostEqual(clk.to_unix_us(a + 5_000_100_000), 2_000_000_000_010_000, delta=1)
        self.assertAlmostEqual(clk.eps, 0.0, places=9)
        self.assertAlmostEqual(clk.anchor_bracket_us, 0.3, places=3)

    def test_rate_deviation_fitted_from_two_anchors(self):
        # 5 秒 QPC 内挂钟多走 50us -> 10ppm
        a = 100_000_000_000_000_000
        evs = [ej.Ev(ej.PROV_DXG, 17, "Info", 0, 0, a + 5_000_000_000 + i * 166_700, ["0xA", TARGET, 1, 0, i, str(5_000_000_000 + i * 166_700)]) for i in range(5)]
        clk = ej.make_clock(evs, {"freq": 10_000_000, "anchors": [{"qpc": 5_000_000_000, "unix_us": 1_000_000_000, "bracket_ticks": 0},
                                                                   {"qpc": 5_050_000_000, "unix_us": 1_000_000_000 + 5_000_050, "bracket_ticks": 0}]})
        self.assertAlmostEqual(clk.eps * 1e6, 10.0, places=3)

    def test_fallback_without_anchors(self):
        clk = ej.make_clock([], None)
        self.assertFalse(clk.a_from_trace)
        self.assertAlmostEqual(clk.to_unix_us(UNIX_EPOCH_FT + 10_000_000), 1_000_000.0, delta=0.01)


class PairingTests(unittest.TestCase):
    def run_pair(self, scene: Scene):
        evs = scene.sorted()
        pres = ej.extract_presents(evs, APP_PID)
        ej.pair_presents(pres, evs, TARGET)
        return pres

    def test_normal_pair_to_vsync(self):
        # Present 发生在第 5 个 vsync 之后 1ms；DWM 在下一个帧(第 6 个)合成呈现，之后第一个 vsync(第 7 个)显示
        sc = Scene()
        t_call = sc.t0 + 5 * VSYNC_US + 1000
        sc.app_present(t_call, token=100)
        sc.dwm_present_at_frame(6)
        pres = self.run_pair(sc)
        self.assertEqual(len(pres), 1)
        p = pres[0]
        self.assertEqual(p.status, "displayed")
        self.assertEqual(p.token, 100)
        self.assertEqual(p.vsync_count, 1007)
        self.assertEqual(p.vsync, ft_of(sc.t0 + 7 * VSYNC_US))

    def test_skipped_present_superseded_and_no_pickup(self):
        # 同一帧内同一交换链两次 Present：第一个被第二个顶掉；再有一个令牌从未被 DWM 取走；DXGI_PRESENT_TEST 不计
        sc = Scene()
        sc.app_present(sc.t0 + 5 * VSYNC_US + 1000, token=100)
        sc.app_present(sc.t0 + 5 * VSYNC_US + 4000, token=101)
        sc.dwm_present_at_frame(6)
        sc.app_present(sc.t0 + 20 * VSYNC_US + 1000, token=102, picked=False)
        sc.app_present(sc.t0 + 30 * VSYNC_US, token=0, test=True)
        pres = self.run_pair(sc)
        self.assertEqual([p.token for p in pres], [100, 101, 102])
        self.assertEqual([p.status for p in pres], ["superseded", "displayed", "no_pickup"])
        self.assertEqual(pres[0].superseded_by, pres[1].idx)

    def test_dwm_frame_without_present(self):
        # DWM 帧里没有 SCHEDULE_PRESENT：说明该帧没提交呈现
        sc = Scene()
        sc.app_present(sc.t0 + 5 * VSYNC_US + 1000, token=100)
        pres = self.run_pair(sc)
        self.assertEqual(pres[0].status, "dwm_no_present")

    def test_pick_dwm_target_by_wait_counter(self):
        evs = [vsync(1000, 500), vsync(2000, 501), ev(ej.PROV_DXG, 318, "Info", 1500, [0, 501, 0, 1], pid=DWM_PID)]
        other = ej.Ev(ej.PROV_DXG, 17, "Info", 0, 0, ft_of(1000), ["0xA", "0x1041", 1, 0, 9000, 0])
        tg = ej.vsync_targets(evs + [other])
        self.assertEqual(ej.pick_dwm_target(evs + [other], tg), TARGET)


class FixtureMatchTests(unittest.TestCase):
    def test_match_with_offset_and_gap(self):
        # 6 次 Present 返回；夹具 frames.csv 有 7 个 seq，其中 seq 4 没有对应 Present（事件缺失）；两边有固定 150us 偏差
        rets = [1_000_000 + i * VSYNC_US for i in range(6)]
        pres = [ej.Present(i, ft_of(r - 30), ft_of(r), 7) for i, r in enumerate(rets)]
        fx = [(1, rets[0] - 150), (2, rets[1] - 150), (3, rets[2] - 150), (4, rets[2] + 8000), (5, rets[3] - 150), (6, rets[4] - 150), (7, rets[5] - 150)]
        m = ej.match_fixture(pres, fx, identity_clock())
        self.assertEqual(m["matched"], 6)
        self.assertEqual(m["unmatched_seq"], [4])
        self.assertEqual(m["unmatched_presents"], 0)
        self.assertEqual([p.seq for p in pres], [1, 2, 3, 5, 6, 7])
        self.assertAlmostEqual(m["offset_us_median"], 150.0, delta=1)

    def test_parse_fixture(self):
        self.assertEqual(ej.parse_fixture("seq,submit_ns,flush_ns,unix_us\n1,10,10,1000\n2,20,20,2000\n"), [(1, 1000.0), (2, 2000.0)])


class IntervalTests(unittest.TestCase):
    def test_anomaly_over_one_and_half_periods(self):
        ts = [i * 16667.0 for i in range(10)] + [9 * 16667.0 + 50_000]  # 末尾一个 3 周期的间隔
        s = ej.interval_stats(ts)
        self.assertEqual(len(s["anomalies"]), 1)
        self.assertAlmostEqual(s["anomalies"][0]["periods"], 3.0, places=1)
        self.assertAlmostEqual(s["period_us"], 16667.0, places=0)

    def test_count_events_rate(self):
        evs = [ev(ej.PROV_DXG, 17, "Info", 1_000_000 + i * 500_000, []) for i in range(5)]  # 2 秒内 5 个
        rows = ej.count_events(evs)
        self.assertEqual(rows[0]["count"], 5)
        self.assertAlmostEqual(rows[0]["per_sec"], 2.5, places=2)


if __name__ == "__main__":
    unittest.main()
