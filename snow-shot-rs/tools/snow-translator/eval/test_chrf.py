"""chrf.py 单测：python -m unittest test_chrf（在 eval 目录下运行）。"""
import unittest

import chrf


class ChrfTests(unittest.TestCase):
    """覆盖相同句、不相交、手算例子与中英文处理。"""

    def test_identical_is_100(self):
        """相同句得 100。"""
        self.assertAlmostEqual(chrf.corpus_chrf(["Hello, world! 你好"], ["Hello, world! 你好"]), 100.0, places=6)

    def test_disjoint_is_zero(self):
        """完全不相交近似 0。"""
        self.assertLess(chrf.corpus_chrf(["aaa bbb"], ["xyz uvw"]), 1e-6)

    def test_hand_computed_char_only(self):
        """abc vs abd，仅字符阶：(2/3 + 1/2 + eps)/3 ≈ 38.889。"""
        got = chrf.corpus_chrf(["abc"], ["abd"], word_order=0)
        self.assertAlmostEqual(got, 100 * (2 / 3 + 1 / 2) / 3, places=4)

    def test_hand_computed_with_word_order(self):
        """chrF++ 下多一个无命中的词 1 元组阶：有效阶数 4。"""
        got = chrf.corpus_chrf(["abc"], ["abd"], word_order=2)
        self.assertAlmostEqual(got, 100 * (2 / 3 + 1 / 2) / 4, places=4)

    def test_recall_weighted(self):
        """beta=2 偏重 recall：漏译比多译扣分更重。"""
        ref = "the quick brown fox jumps over the lazy dog"
        short = chrf.corpus_chrf(["the quick brown fox"], [ref])
        long_ = chrf.corpus_chrf([ref + " and some extra words here"], [ref])
        self.assertLess(short, long_)

    def test_chinese_by_char(self):
        """中文无空格：字符阶按字计，词阶整句一个词。"""
        self.assertEqual(chrf.char_ngrams("你好 世界", 2), chrf.char_ngrams("你好世界", 2))
        self.assertEqual(chrf.split_words("今天天气很好"), ["今天天气很好"])
        part = chrf.corpus_chrf(["今天天气"], ["今天天气很好"])
        self.assertTrue(0 < part < 100)

    def test_latin_punct_split(self):
        """拉丁词尾标点被拆成独立词，大小写敏感。"""
        self.assertEqual(chrf.split_words("Hello, world!"), ["Hello", ",", "world", "!"])
        self.assertLess(chrf.corpus_chrf(["hello"], ["Hello"]), 100)

    def test_aggregates_corpus_level(self):
        """语料级是统计量累加，不等于句分平均。"""
        h, r = ["abc", "x"], ["abc", "abcdefghij"]
        agg = chrf.corpus_chrf(h, r)
        avg = (chrf.corpus_chrf(h[:1], r[:1]) + chrf.corpus_chrf(h[1:], r[1:])) / 2
        self.assertNotAlmostEqual(agg, avg, places=2)

    def test_length_mismatch_raises(self):
        """行数不等报错。"""
        with self.assertRaises(ValueError):
            chrf.corpus_chrf(["a"], [])


if __name__ == "__main__":
    unittest.main()
