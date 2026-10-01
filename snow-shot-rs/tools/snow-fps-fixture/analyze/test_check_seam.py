"""check_seam 的离线测试（合成数据，不需要 ffmpeg 与屏幕）。运行: python -m unittest test_check_seam"""

import doctest
import unittest

import check_seam as cs

WIDTH = 1920
X0 = 1600
SEAM_COL = 960


def make_row(cols, width=WIDTH, extra=()):
    """生成一行：给定列为白，其余为黑。"""
    row = [16] * width
    for c in list(cols) + list(extra):
        if 0 <= c < width:
            row[c] = 235
    return row


EXPECTED = cs.expected_columns(WIDTH, X0)


class ExpectedTests(unittest.TestCase):
    """期望列。"""

    def test_columns(self):
        self.assertEqual(EXPECTED[0], 0)
        self.assertIn(SEAM_COL, EXPECTED)
        self.assertEqual(len(EXPECTED), 30)
        self.assertEqual(cs.expected_columns(100, 10), [54])


class CompareTests(unittest.TestCase):
    """四类合成场景及缺失/多余。"""

    def run_case(self, row):
        return cs.compare_row(row, EXPECTED, SEAM_COL)

    def test_normal_passes(self):
        rep = self.run_case(make_row(EXPECTED))
        self.assertTrue(rep.passed, rep.issues)
        self.assertEqual((rep.expected, rep.found, rep.max_err), (30, 30, 0.0))

    def test_missing_column_at_seam_fails(self):
        # 接缝处丢 1 列：接缝及之后的线整体左移 1
        cols = [c if c < SEAM_COL else c - 1 for c in EXPECTED]
        rep = self.run_case(make_row(cols))
        self.assertFalse(rep.passed)
        self.assertTrue(any("接缝附近" in m for m in rep.issues))
        self.assertEqual(rep.seam_err, 1.0)

    def test_duplicate_column_at_seam_fails(self):
        # 接缝处重复 1 列：接缝线变宽 2，之后整体右移 1
        cols = [c if c < SEAM_COL else c + 1 for c in EXPECTED]
        rep = self.run_case(make_row(cols, extra=[SEAM_COL]))
        self.assertFalse(rep.passed)
        self.assertTrue(any("加宽" in m for m in rep.issues))

    def test_global_offset_two_fails(self):
        rep = self.run_case(make_row([c + 2 for c in EXPECTED]))
        self.assertFalse(rep.passed)
        self.assertEqual(rep.max_err, 2.0)

    def test_far_offset_one_within_tol_passes(self):
        # 远离接缝的 1 列偏差在一般容差内
        cols = [c + 1 if c > 200 and abs(c - SEAM_COL) > 20 else c for c in EXPECTED]
        self.assertTrue(self.run_case(make_row(cols)).passed)

    def test_missing_and_extra_line(self):
        cols = [c for c in EXPECTED if c != 128]
        rep = self.run_case(make_row(cols, extra=[500]))
        self.assertTrue(any("缺失" in m for m in rep.issues))
        self.assertTrue(any("多余" in m for m in rep.issues))


class FrameTests(unittest.TestCase):
    """多帧汇总。"""

    def test_check_frames_pass_and_fail(self):
        h = 1080
        frame = bytes(make_row(EXPECTED) * h)
        rep, lines = cs.check_frames([frame, frame], WIDTH, h, X0, 2560, 1.0, 0.25)
        self.assertTrue(rep.passed, rep.issues)
        self.assertIn("960.0", lines[0])
        bad = bytes(make_row([c + 2 for c in EXPECTED]) * h)
        rep, _ = cs.check_frames([bad], WIDTH, h, X0, 2560, 1.0, 0.25)
        self.assertFalse(rep.passed)


class DocTests(unittest.TestCase):
    """文档示例。"""

    def test_doctests(self):
        self.assertEqual(doctest.testmod(cs).failed, 0)


if __name__ == "__main__":
    unittest.main()
