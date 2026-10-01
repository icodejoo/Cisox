"""aggregate_trace.py 的离线测试：用合成的 trace_join.json / env.json 目录验证按档位汇总。"""

import json
import tempfile
import unittest
from pathlib import Path

import aggregate_trace as agg


def summary(passed, cats=None, lost=0, in_stall=0, multi=0, fps=60, clusters=0, causes=None, window_causes=None):
    """构造一份 trace_join.json 内容。"""
    return {
        "lost_stall_causes": causes or {},
        "fps": fps,
        "product": {"passed": passed},
        "counts": {"lost": lost},
        "categories": cats or {},
        "lost_in_stall": {"any": in_stall, "multi_stage": multi},
        "stalls": {"multi_stage_clusters": clusters, "acquire_causes": window_causes or {}},
    }


def write_run(root: Path, name: str, body: dict, env=None):
    """写出一轮的子目录。"""
    folder = root / name
    folder.mkdir()
    (folder / "trace_join.json").write_text(json.dumps(body), encoding="utf-8")
    if env is not None:
        (folder / "env.json").write_text(json.dumps(env), encoding="utf-8-sig")


class AggregateTests(unittest.TestCase):
    """按档位汇总。"""

    def build(self, root: Path):
        write_run(root, "hw-r1-1920x1080-60-100", summary(True), {"interfered": False})
        write_run(root, "hw-r2-1920x1080-60-101", summary(False, {"a_dxgi_coalesced": 4, "b_slot_dropped": 1}, 5, 4, 2, clusters=1), {"interfered": False})
        write_run(root, "hw-r3-1920x1080-60-102", summary(False, {"c_compose_encode_lost": 3}, 3, 3, 3, clusters=2), {"interfered": True})
        write_run(root, "hw-r4-1920x1080-30-103", summary(True, fps=30), {"interfered": True})
        write_run(root, "hw-r5-1920x1080-30-104", summary(True, fps=30))  # 缺 env.json

    def test_per_tier_counts_and_clean_rate(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            self.build(root)
            result = agg.summarize(agg.load_runs(root))
        t60 = result["1920x1080@60"]
        self.assertEqual((t60["runs"], t60["passed"], t60["failed_runs"]), (3, 1, 2))
        self.assertEqual((t60["interfered_runs"], t60["clean_runs"], t60["clean_passed"]), (1, 2, 1))
        self.assertAlmostEqual(t60["clean_pass_rate"], 0.5)
        self.assertEqual(t60["failed_categories"], {"a_dxgi_coalesced": 4, "b_slot_dropped": 1, "c_compose_encode_lost": 3})
        self.assertEqual((t60["failed_lost_total"], t60["failed_lost_in_stall"], t60["failed_lost_in_multi_stage"]), (8, 7, 5))
        self.assertEqual((t60["failed_runs_with_stall"], t60["failed_runs_with_multi_stage"], t60["failed_interfered_runs"]), (2, 2, 1))
        t30 = result["1920x1080@30"]
        self.assertEqual((t30["runs"], t30["passed"], t30["env_unknown_runs"], t30["clean_runs"]), (2, 2, 1, 0))
        self.assertIsNone(t30["clean_pass_rate"])
        self.assertEqual(result["ALL"]["runs"], 5)

    def test_table_and_name_filter(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            self.build(root)
            write_run(root, "other-r1-1280x720-30-9", summary(True, fps=30), {"interfered": False})
            runs = agg.load_runs(root, "hw")
            self.assertEqual(len(runs), 5)
            table = agg.format_table(agg.summarize(runs))
        self.assertIn("1920x1080@60", table)
        self.assertIn("4/1/3/0/0", table)  # a/b/c/d/e 分布
        self.assertIn("7/8", table)
        self.assertIn("缺 env.json", table)

    def test_stall_causes_sum_over_failed_runs_only(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            write_run(root, "hw-r1-1920x1080-60-1", summary(True, window_causes={"sleep_overrun": 2}), {"interfered": False})
            write_run(root, "hw-r2-1920x1080-60-2", summary(False, {"a_dxgi_coalesced": 3}, 3, 3, causes={"sleep_overrun": 2, "acquire_blocked": 1}, window_causes={"sleep_overrun": 1, "no_slow_iter": 1}), {"interfered": False})
            write_run(root, "hw-r3-1920x1080-60-3", summary(False, {"a_dxgi_coalesced": 1}, 1, 1, causes={"acquire_blocked": 1}), {"interfered": False})
            summ = agg.summarize(agg.load_runs(root))
            table = agg.format_table(summ)
        t60 = summ["1920x1080@60"]
        self.assertEqual(t60["failed_stall_causes"], {"sleep_overrun": 2, "acquire_blocked": 2})
        self.assertEqual(t60["window_stall_causes"], {"sleep_overrun": 3, "no_slow_iter": 1})
        self.assertIn("停顿成因", table)
        self.assertIn("sleep_overrun=2", table)

    def test_controlled_load_is_counted_separately_from_interference(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            write_run(root, "hog-r1-1920x1080-60-1", summary(False, {"a_dxgi_coalesced": 1}, 1), {"interfered": False, "controlled_load": True, "hog_cores": 4})
            write_run(root, "hog-r2-1920x1080-60-2", summary(True), {"interfered": False, "controlled_load": False, "hog_cores": 0})
            write_run(root, "hog-r3-1920x1080-60-3", summary(True), {"interfered": True})
            runs = agg.load_runs(root)
            summ = agg.summarize(runs)
            table = agg.format_table(summ)
        t60 = summ["1920x1080@60"]
        self.assertEqual((t60["controlled_load_runs"], t60["interfered_runs"], t60["clean_runs"]), (1, 1, 2))
        self.assertEqual(sorted(r["hog_cores"] for r in runs), [0, 0, 4])
        self.assertIn("受控压力", table)

    def test_tier_from_env_overrides_dir_name(self):
        self.assertEqual(agg.tier_of("weird", {"fps": 60}, {"size": "2560x1440"}), ("2560x1440", 60))
        self.assertEqual(agg.tier_of("hw-r1-1920x1080-30-5", {}, None), ("1920x1080", 30))
        self.assertIsNone(agg.tier_of("nothing", {}, None))

    def test_unjudged_runs_are_not_counted_as_pass_or_fail(self):
        run = agg.make_run("x-r1-1920x1080-60-1", {"product": {}, "categories": {}}, {"interfered": False})
        result = agg.summarize([run])["1920x1080@60"]
        self.assertEqual((result["runs"], result["judged"], result["passed"]), (1, 0, 0))
        self.assertIsNone(result["pass_rate"])


if __name__ == "__main__":
    unittest.main()
