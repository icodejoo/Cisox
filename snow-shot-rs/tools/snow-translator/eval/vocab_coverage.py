"""SentencePiece 词表覆盖率分析（仅标准库）：统计词表构成，并用 FLORES 句子算各语种的字符覆盖率。

用法示例:
    python vocab_coverage.py --spm A=path/a.model --spm B=path/b.model \
        --flores E:/workspaces/Cisox/build/flores/flores200_dataset/devtest --n 200
"""
import argparse
import os
import struct
import unicodedata


def read_varint(buf, pos):
    """读取 protobuf varint，返回 (值, 新位置)。"""
    shift = result = 0
    while True:
        b = buf[pos]
        pos += 1
        result |= (b & 0x7F) << shift
        if not b & 0x80:
            return result, pos
        shift += 7


def parse_spm(path):
    """解析 SentencePiece .model 的 pieces，返回 [(片段, 分数, 类型)]。"""
    buf = open(path, "rb").read()
    pos, pieces = 0, []
    while pos < len(buf):
        key, pos = read_varint(buf, pos)
        field, wire = key >> 3, key & 7
        if wire == 2:
            length, pos = read_varint(buf, pos)
            chunk = buf[pos:pos + length]
            pos += length
            if field == 1:
                pieces.append(parse_piece(chunk))
        elif wire == 0:
            _, pos = read_varint(buf, pos)
        elif wire == 5:
            pos += 4
        elif wire == 1:
            pos += 8
        else:
            raise ValueError(f"未知 wire type {wire}")
    return pieces


def parse_piece(chunk):
    """解析单个 SentencePiece 消息，返回 (片段, 分数, 类型)。"""
    pos, text, score, typ = 0, "", 0.0, 1
    while pos < len(chunk):
        key, pos = read_varint(chunk, pos)
        field, wire = key >> 3, key & 7
        if wire == 2:
            length, pos = read_varint(chunk, pos)
            if field == 1:
                text = chunk[pos:pos + length].decode("utf-8", "replace")
            pos += length
        elif wire == 5:
            if field == 2:
                score = struct.unpack("<f", chunk[pos:pos + 4])[0]
            pos += 4
        elif wire == 0:
            val, pos = read_varint(chunk, pos)
            if field == 3:
                typ = val
        else:
            break
    return text, score, typ


def script_of(ch):
    """粗分文字类别。"""
    try:
        name = unicodedata.name(ch)
    except ValueError:
        return "OTHER"
    for key, label in (("CJK", "CJK"), ("HIRAGANA", "JP-kana"), ("KATAKANA", "JP-kana"), ("HANGUL", "KR-hangul"),
                       ("CYRILLIC", "Cyrillic"), ("ARABIC", "Arabic"), ("LATIN", "Latin"), ("TIFINAGH", "Tifinagh"),
                       ("GREEK", "Greek"), ("DEVANAGARI", "Devanagari"), ("THAI", "Thai")):
        if key in name:
            return label
    return "OTHER"


def char_set(pieces):
    """词表里能单独表示的字符集合（去掉词首标记 ▁）。"""
    chars = set()
    for text, _, typ in pieces:
        if typ not in (1, 4):  # 1 NORMAL, 4 USER_DEFINED
            continue
        t = text.replace("\u2581", "")
        if len(t) == 1:
            chars.add(t)
    return chars


def read_lang(flores_dir, code, n):
    """读取 FLORES devtest 某语言前 n 句。"""
    path = os.path.join(flores_dir, f"{code}.devtest")
    with open(path, encoding="utf-8") as f:
        return [line.rstrip("\n") for _, line in zip(range(n), f)]


def coverage(chars, sentences):
    """返回 (字符出现次数, 被词表覆盖的次数, 未覆盖字符样例)。"""
    total = covered = 0
    missing = {}
    for s in sentences:
        for ch in s:
            if ch.isspace():
                continue
            total += 1
            if ch in chars:
                covered += 1
            else:
                missing[ch] = missing.get(ch, 0) + 1
    return total, covered, sorted(missing.items(), key=lambda kv: -kv[1])[:8]


def main():
    """入口。"""
    ap = argparse.ArgumentParser()
    ap.add_argument("--spm", action="append", required=True, help="名称=路径")
    ap.add_argument("--flores", required=True)
    ap.add_argument("--n", type=int, default=200)
    ap.add_argument("--langs", default="zho_Hans,zho_Hant,eng_Latn,fra_Latn,spa_Latn,rus_Cyrl,arb_Arab,deu_Latn,jpn_Jpan,kor_Hang,por_Latn,ita_Latn,tur_Latn,vie_Latn,ind_Latn,tha_Thai,hin_Deva,zgh_Tfng")
    args = ap.parse_args()
    vocabs = {}
    for spec in args.spm:
        name, path = spec.split("=", 1)
        pieces = parse_spm(path)
        chars = char_set(pieces)
        vocabs[name] = chars
        scripts = {}
        for ch in chars:
            scripts[script_of(ch)] = scripts.get(script_of(ch), 0) + 1
        print(f"[{name}] pieces={len(pieces)} 单字符片段={len(chars)} 按文字: {dict(sorted(scripts.items(), key=lambda kv: -kv[1]))}")
    print()
    print(f"{'语言':10s}" + "".join(f"{n:>22s}" for n in vocabs))
    for code in args.langs.split(","):
        try:
            sents = read_lang(args.flores, code, args.n)
        except FileNotFoundError:
            print(f"{code:10s} (FLORES 无此文件)")
            continue
        row = f"{code:10s}"
        misses = {}
        for name, chars in vocabs.items():
            total, covered, miss = coverage(chars, sents)
            row += f"{100 * covered / max(total, 1):>11.2f}% ({total - covered:>5d}缺)"
            misses[name] = miss
        print(row)
        for name, miss in misses.items():
            if miss and miss[0][1] > 0:
                print(f"{'':10s}  {name} 缺字样例: {' '.join(f'{c}×{k}' for c, k in miss[:6])}")


if __name__ == "__main__":
    main()
