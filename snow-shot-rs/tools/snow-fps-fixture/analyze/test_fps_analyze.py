"""fps_analyze 稳态丢帧口径（排除启动首帧）的单元测试。"""
import os
import sys
import unittest

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import fps_analyze as fa  # noqa: E402


def _frames(skip=()):
    """构造 1 个陈旧启动帧（序号 123）+ 300 个稳态帧（序号 125..），skip 为稳态下标，用来制造中途缺失。"""
    return [(0.0, 123)] + [((i + 1) / 60, 125 + i) for i in range(300) if i not in skip]


class SteadyDropTest(unittest.TestCase):
    """启动边界不计入丢帧，中途丢失仍然计入。"""

    def test_startup_boundary_is_excluded(self):
        """开头缺 1 个序号：稳态 0，原始口径 1，判定通过。"""
        st = fa.compute_stats(_frames(), 5.0, 59.0)
        self.assertEqual((st.dropped_seqs, st.dropped_seqs_raw), (0, 1))
        self.assertTrue(st.startup_skipped)
        self.assertEqual(st.verdict, "PASS")

    def test_midstream_loss_still_counts(self):
        """中途丢 3 个：稳态口径照常统计并判失败。"""
        st = fa.compute_stats(_frames(skip=(100, 101, 200)), 5.0, 59.0)
        self.assertEqual(st.dropped_seqs, 3)
        self.assertEqual(st.dropped_seqs_raw, 4)
        self.assertTrue(st.verdict.startswith("FAIL"))

    def test_too_few_frames_not_skipped(self):
        """有效帧数不足时不排除首帧，口径与原来一致。"""
        st = fa.compute_stats([(i / 60, i + 1) for i in range(10)], 1.0, 59.0)
        self.assertFalse(st.startup_skipped)
        self.assertEqual(st.dropped_seqs, st.dropped_seqs_raw)

    def test_report_shows_both_numbers(self):
        """报告同时给出稳态与含首帧的原始口径，且第一处"丢帧率"是稳态值（供汇总脚本解析）。"""
        text = fa.format_report(fa.compute_stats(_frames(), 5.0, 59.0))
        self.assertIn("含首帧的原始口径 缺 1 个", text)
        self.assertLess(text.index("丢帧率 0.000%"), text.index("含首帧"))


if __name__ == "__main__":
    unittest.main()


class BarCropTest(unittest.TestCase):
    """双窗口序号条裁剪参数。"""

    def test_filter_crops_leader_segment(self):
        """不带参数整宽；带参数按比例裁主窗口那段。"""
        self.assertEqual(fa.bar_filter(), fa.BAR_FILTER)
        self.assertTrue(fa.bar_filter((0, 1280, 2560)).startswith("crop=iw*1280/2560:ih/32:iw*0/2560:"))

    def test_parse_rejects_bad_values(self):
        """合法值通过，越界或非正宽度报错。"""
        self.assertEqual(fa.parse_bar_crop("0,1280,2560"), (0, 1280, 2560))
        for bad in ("0,0,10", "5,10,10", "-1,5,10", "1,2"):
            with self.assertRaises(ValueError):
                fa.parse_bar_crop(bad)
