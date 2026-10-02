"""zh_punct 的单测。运行：python test_zh_punct.py"""
import unittest
from zh_punct import to_fullwidth as f


class T(unittest.TestCase):
    """全半角转换规则。"""

    def test_basic(self):
        self.assertEqual(f("他说,好.", "zho_Hans"), "他说，好。")

    def test_decimal_and_thousands(self):
        self.assertEqual(f("有4.5个月,共1,000人.", "zho_Hans"), "有4.5个月，共1,000人。")

    def test_abbrev_and_english_run(self):
        self.assertEqual(f("美国(U.S.)的Dr. Ehud, a教授.", "zho_Hans"), "美国(U.S.)的Dr. Ehud, a教授。")

    def test_quotes_paired(self):
        self.assertEqual(f('他说:"你好,世界."', "zho_Hans"), "他说：“你好，世界。”")
        self.assertEqual(f('他说"好"和"坏"', "jpn_Jpan"), "他说「好」和「坏」")

    def test_parens(self):
        self.assertEqual(f("苹果(水果)和(iPhone)", "zho_Hans"), "苹果（水果）和(iPhone)")

    def test_question_exclaim_semicolon(self):
        self.assertEqual(f("真的吗?是的!好;行.", "zho_Hans"), "真的吗？是的！好；行。")

    def test_japanese(self):
        self.assertEqual(f("彼は言った,行こう.", "jpn_Jpan"), "彼は言った、行こう。")

    def test_other_language_untouched(self):
        self.assertEqual(f("Hello, world.", "eng_Latn"), "Hello, world.")

    def test_space_around_fullwidth(self):
        self.assertEqual(f("你好 , 世界 .", "zho_Hans"), "你好，世界。")


if __name__ == "__main__":
    unittest.main()
