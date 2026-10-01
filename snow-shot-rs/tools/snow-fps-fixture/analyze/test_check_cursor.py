"""check_cursor 的离线测试（合成数据，不需要 ffmpeg 与屏幕）。运行: python -m unittest test_check_cursor"""

import doctest
import unittest

import check_cursor as cc

W, H = 400, 300
SKIP = 20


def frame_with_blob(x, y, size=20, bright=255):
    """生成黑底帧，在 (x,y) 处放 size 见方亮块（模拟光标）。"""
    buf = bytearray(W * H)
    for yy in range(y, y + size):
        for xx in range(x, x + size):
            buf[yy * W + xx] = bright
    return bytes(buf)


class BoxTests(unittest.TestCase):
    """包围盒与判定。"""

    def test_bbox_and_center(self):
        on, off = frame_with_blob(100, 150), bytes(W * H)
        box = cc.diff_bbox(on, off, W, H, SKIP)
        self.assertEqual(box, (100, 150, 119, 169))
        self.assertEqual(cc.bbox_center(box), (110.0, 160.0))

    def test_identical_frames_have_no_box(self):
        f = frame_with_blob(10, 100)
        self.assertIsNone(cc.diff_bbox(f, f, W, H, SKIP))

    def test_top_bar_ignored(self):
        on = bytearray(W * H)
        on[5 * W + 5] = 255  # 序号条区域的变化
        self.assertIsNone(cc.diff_bbox(bytes(on), bytes(W * H), W, H, SKIP))

    def test_pass_near_expected(self):
        box = cc.diff_bbox(frame_with_blob(100, 150), bytes(W * H), W, H, SKIP)
        self.assertTrue(cc.evaluate(box, (110, 160), 24)[0])
        self.assertTrue(cc.evaluate(box, (130, 175), 24)[0])  # 热点偏移内

    def test_fail_far_from_expected(self):
        box = cc.diff_bbox(frame_with_blob(100, 150), bytes(W * H), W, H, SKIP)
        ok, msg = cc.evaluate(box, (200, 160), 24)
        self.assertFalse(ok)
        self.assertIn("超出容差", msg)

    def test_fail_when_no_cursor(self):
        self.assertFalse(cc.evaluate(None, (110, 160), 24)[0])

    def test_fail_when_diff_too_large(self):
        on = frame_with_blob(10, 40, size=200)
        box = cc.diff_bbox(on, bytes(W * H), W, H, SKIP)
        ok, msg = cc.evaluate(box, (110, 140), 24)
        self.assertFalse(ok)
        self.assertIn("非光标", msg)

    def test_noise_below_threshold_ignored(self):
        self.assertIsNone(cc.diff_bbox(frame_with_blob(100, 150, bright=30), bytes(W * H), W, H, SKIP))


class DocTests(unittest.TestCase):
    """文档示例。"""

    def test_doctests(self):
        self.assertEqual(doctest.testmod(cc).failed, 0)


if __name__ == "__main__":
    unittest.main()
