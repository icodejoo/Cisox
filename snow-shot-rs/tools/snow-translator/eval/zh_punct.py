"""中日文输出的全半角标点后处理（评测用，之后移植到 Rust）。

NLLB 对中日文目标输出半角标点（, . : ; ! ? " ( )），而参考译文用全角。本模块按上下文把它们转成全角，
并保持数字里的小数点 / 千分位、英文缩写、英文成句里的标点、括号里的纯英文不动。

用法示例：
    from zh_punct import to_fullwidth
    to_fullwidth('他说:"你好,世界."', 'zho_Hans')   # -> '他说：“你好，世界。”'
"""
import re

CJK_TARGETS = {"zho_Hans", "zho_Hant", "jpn_Jpan"}
_CJK = re.compile(r"[぀-ヿ㐀-䶿一-鿿豈-﫿＀-￯　-〿]")
_MAP = {
    "zho": {",": "，", ".": "。", ":": "：", ";": "；", "!": "！", "?": "？"},
    "jpn": {",": "、", ".": "。", "!": "！", "?": "？"},
}


def _is_alnum(ch):
    """是否 ASCII 字母或数字。"""
    return ch.isascii() and ch.isalnum()


def ch_is_abbrev(text, i):
    """句点是否属于英文缩写（前面是 ASCII 字母，且前两位是句点，或后面紧跟 ) ] '）。"""
    if text[i] != "." or i == 0 or not (text[i - 1].isascii() and text[i - 1].isalpha()):
        return False
    nxt = text[i + 1] if i + 1 < len(text) else ""
    return (i >= 2 and text[i - 2] == ".") or nxt in ")]'"


def _keep_halfwidth(text, i):
    """第 i 个字符（半角标点）是否应保持半角：夹在 ASCII 字母数字之间（小数点、千分位、缩写）或处于英文成句中。

    参数：text 全文；i 标点下标。返回：True 表示保留。
    """
    prev = text[i - 1] if i > 0 else ""
    nxt = text[i + 1] if i + 1 < len(text) else ""
    if _is_alnum(prev) and _is_alnum(nxt):
        return True
    # 缩写 U.S.) / U.S.：句点前是单个字母且再前是句点或紧跟右括号
    if ch_is_abbrev(text, i):
        return True
    # 英文成句："Hello, world" —— 前一个是 ASCII 字母数字，后面是空格再接 ASCII 字母
    if _is_alnum(prev) and nxt == " " and i + 2 < len(text) and text[i + 2].isascii() and text[i + 2].isalpha():
        return True
    return False


def _convert_parens(text):
    """括号内含中日文字符的一对 ( ) 转全角，纯英文 / 数字的保持。"""
    out, i = list(text), 0
    while i < len(text):
        if text[i] == "(":
            j = text.find(")", i + 1)
            if j > 0 and _CJK.search(text[i + 1:j]) and "(" not in text[i + 1:j]:
                out[i], out[j] = "（", "）"
                i = j
        i += 1
    return "".join(out)


def to_fullwidth(text, tgt):
    """把中日文目标译文里的半角标点按上下文转成全角。

    参数：text 译文；tgt 目标语言码（zho_Hans / zho_Hant / jpn_Jpan，其它语言原样返回）。
    返回：转换后的字符串。
    示例：to_fullwidth('4.5个月,好.', 'zho_Hans') -> '4.5个月，好。'
    """
    if tgt not in CJK_TARGETS:
        return text
    table = _MAP["jpn" if tgt == "jpn_Jpan" else "zho"]
    quotes = ("「", "」") if tgt == "jpn_Jpan" else ("“", "”")
    out, qn = [], 0
    for i, ch in enumerate(text):
        if ch in table and not _keep_halfwidth(text, i):
            out.append(table[ch])
        elif ch == '"':
            out.append(quotes[qn % 2])
            qn += 1
        else:
            out.append(ch)
    res = _convert_parens("".join(out))
    # 全角标点两侧与中日文相邻的空格去掉
    res = re.sub(r"\s+([，。：；！？、”」）])", r"\1", res)
    res = re.sub(r"([，。：；！？、“「（])\s+", r"\1", res)
    return res
