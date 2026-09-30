"""summarize_runs.py 的离线测试。"""

import tempfile
import unittest
from pathlib import Path

import summarize_runs as s

# 一份典型的驱动脚本输出片段。
SAMPLE = """成品帧数            : 322（序号有效 322，无效 0）
有效帧率            : 59.82 fps（不同序号 322 / 5.383s）
被丢序号数          : 1（丢帧率 0.304%，最大序号跳变 2）
perf: cpu(1-core-equiv)=31% of 20 logical  ws avg=170MB max=190MB  private max=219MB  threads max=27
"""


class ParseTests(unittest.TestCase):
    """解析与判定。"""

    def test_parse_extracts_numbers(self):
        run = s.parse_run(SAMPLE)
        self.assertEqual(run["fps"], 59.82)
        self.assertEqual(run["drop"], 0.304)
        self.assertEqual(run["frames"], 322.0)
        self.assertEqual(run["cpu"], 0.31)
        self.assertEqual((run["ws"], run["wsmax"]), (170.0, 190.0))

    def test_truncated_recording_does_not_pass(self):
        # 序号连续但只有 12 帧（编码器死锁的残缺成品）必须判失败
        self.assertFalse(s.passed({"fps": 60.0, "drop": 0.0, "frames": 12.0}, 60))
        self.assertTrue(s.passed({"fps": 60.0, "drop": 0.0, "frames": 350.0}, 60))

    def test_parse_without_analysis_returns_none(self):
        self.assertIsNone(s.parse_run("录制进程 60 秒未退出"))

    def test_pass_lines(self):
        self.assertTrue(s.passed({"fps": 28.6, "drop": 0.9, "frames": 170.0}, 30))
        self.assertFalse(s.passed({"fps": 28.4, "drop": 0.0, "frames": 170.0}, 30))
        self.assertFalse(s.passed({"fps": 59.9, "drop": 1.0, "frames": 350.0}, 60))
        self.assertTrue(s.passed({"fps": 56.0, "drop": 0.0, "frames": 350.0}, 60))

    def test_main_summarises_a_folder(self):
        with tempfile.TemporaryDirectory() as folder:
            for r in (1, 2):
                Path(folder, f"hw-r{r}-2560x1440-60.txt").write_text(SAMPLE, encoding="utf-8")
            Path(folder, "other-r1-2560x1440-60.txt").write_text(SAMPLE, encoding="utf-8")
            self.assertEqual(s.main([folder, "hw"]), 0)
            self.assertEqual(s.main([folder]), 2)


if __name__ == "__main__":
    unittest.main()
