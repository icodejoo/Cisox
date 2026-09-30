"""fps_analyze 的离线样例测试（不需要 ffmpeg 与屏幕）。运行: python -m unittest test_fps_analyze"""

import doctest
import unittest

import fps_analyze as fa


def bar(seq: int, hi: int = 235, lo: int = 16) -> bytes:
    """按夹具规则生成一帧 32 字节采样（限幅范围灰度，模拟视频压缩后的值）。"""
    return bytes(hi if (seq >> (31 - i)) & 1 else lo for i in range(32))


class DecodeTests(unittest.TestCase):
    """序号解码。"""

    def test_roundtrip_typical_values(self):
        for seq in [0, 1, 2, 255, 256, 123456, 0xDEADBEEF, 0xFFFFFFFF]:
            self.assertEqual(fa.decode_seq(bar(seq)), seq)

    def test_noisy_samples_still_decode(self):
        noisy = bytes(v + (7 if i % 2 else -7) for i, v in enumerate(bar(0xA5A5A5A5)))
        self.assertEqual(fa.decode_seq(noisy), 0xA5A5A5A5)

    def test_threshold_edges(self):
        self.assertEqual(fa.decode_seq([127] * 32), 0)
        self.assertEqual(fa.decode_seq([128] * 32), 0xFFFFFFFF)

    def test_short_input(self):
        self.assertIsNone(fa.decode_seq([255] * 31))
        self.assertEqual(fa.decode_raw_frames(b""), [])

    def test_raw_stream_splits_frames_and_ignores_tail(self):
        raw = bar(7) + bar(8) + b"\x00\x01"
        self.assertEqual(fa.decode_raw_frames(raw), [7, 8])

    def test_doctests(self):
        self.assertEqual(doctest.testmod(fa).failed, 0)


class StatsTests(unittest.TestCase):
    """统计与判定。"""

    @staticmethod
    def frames(seqs, fps=60.0, start=0.0):
        return [(start + i / fps, s) for i, s in enumerate(seqs)]

    def test_perfect_60fps_passes(self):
        st = fa.compute_stats(self.frames(range(1, 601)), 10.0, 59.0)
        self.assertEqual((st.dropped_seqs, st.duplicate_frames, st.out_of_order), (0, 0, 0))
        self.assertAlmostEqual(st.fps_effective, 60.0)
        self.assertEqual(st.verdict, "PASS")

    def test_dropped_sequence_numbers_counted(self):
        seqs = [s for s in range(1, 601) if s not in (10, 11, 300)]
        st = fa.compute_stats(self.frames(seqs), 10.0, 59.0)
        self.assertEqual(st.dropped_seqs, 3)
        self.assertEqual(st.max_seq_gap, 3)
        self.assertLess(st.drop_rate, 0.01)

    def test_drop_rate_over_one_percent_fails(self):
        seqs = [s for s in range(1, 601) if s % 50 != 0]  # 丢 12 个，2%
        st = fa.compute_stats(self.frames(seqs), 10.0, 30.0)
        self.assertFalse(st.drop_ok)
        self.assertTrue(st.verdict.startswith("FAIL"))

    def test_low_fps_fails_line(self):
        st = fa.compute_stats(self.frames(range(1, 181), fps=30.0), 6.0, 59.0)
        self.assertFalse(st.fps_ok)
        self.assertEqual(st.drop_ok, True)
        self.assertIn("帧率", st.verdict)

    def test_duplicates_and_reordering(self):
        seqs = [1, 2, 2, 3, 5, 4, 6]
        st = fa.compute_stats(self.frames(seqs), 7 / 60, 59.0)
        self.assertEqual(st.duplicate_frames, 1)
        self.assertEqual(st.out_of_order, 1)
        self.assertEqual(st.dropped_seqs, 0)
        self.assertFalse(st.order_ok)

    def test_undecodable_and_outlier_frames_excluded(self):
        seqs = [1, 2, None, 4, 0xFFFFFFF0, 6]
        st = fa.compute_stats(self.frames(seqs), 0.1, 59.0)
        self.assertEqual(st.bad_frames, 2)
        self.assertEqual(st.dropped_seqs, 2)  # 缺 3 和 5

    def test_no_decodable_frames(self):
        st = fa.compute_stats(self.frames([None, None]), 1.0, 59.0)
        self.assertTrue(st.verdict.startswith("FAIL"))

    def test_duration_falls_back_to_pts_span(self):
        st = fa.compute_stats(self.frames(range(1, 61)), 0.0, 59.0)
        self.assertAlmostEqual(st.duration_s, 1.0, places=3)

    def test_fixture_log_drift_and_filtering(self):
        submit = {s: s / 60 for s in range(1, 601)}
        # 成品时间戳整体晚 0.2s 且慢 1%：漂移约 -? 累计 +0.1s
        frames = [(0.2 + (s / 60) * 1.01, s) for s in range(1, 601)]
        frames.append((11.0, 999999))  # 日志里没有的序号 -> 视为解码错误
        st = fa.compute_stats(frames, 10.0, 59.0, submit)
        self.assertEqual(st.bad_frames, 1)
        self.assertAlmostEqual(st.drift_ms, 0.01 * 599 / 60 * 1000, delta=1.0)
        self.assertGreater(st.max_abs_dev_ms, 0)
        self.assertAlmostEqual(st.published_fps, 60.0, delta=0.01)

    def test_linear_fit(self):
        self.assertEqual(fa.linear_fit([0, 1, 2], [1, 3, 5]), (2.0, 1.0))
        self.assertEqual(fa.linear_fit([1, 1], [2, 4]), (0.0, 3.0))

    def test_report_mentions_verdict(self):
        st = fa.compute_stats(self.frames(range(1, 61)), 1.0, 59.0)
        self.assertIn("判定", fa.format_report(st))


if __name__ == "__main__":
    unittest.main()
