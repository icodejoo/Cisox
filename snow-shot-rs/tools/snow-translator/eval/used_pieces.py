"""估算"只保留目标语种所需词表"后的词表大小（仅标准库，近似）。

用 SentencePiece BPE 规则（按片段分数从高到低合并相邻符号）对 FLORES dev+devtest 分词，统计实际用到的
不同片段数。规范化用 Python 的 NFKC 近似（官方用 nmt_nfkc 预编译字符映射，存在细微差别），所以结果是估算。

用法示例:
    python used_pieces.py --spm orig.model --flores .../flores200_dataset
"""
import argparse
import os
import unicodedata

from vocab_coverage import parse_spm

UN6 = ["zho_Hans", "eng_Latn", "fra_Latn", "spa_Latn", "rus_Cyrl", "arb_Arab"]
MAIN14 = UN6 + ["deu_Latn", "jpn_Jpan", "kor_Hang", "por_Latn", "ita_Latn", "tur_Latn", "vie_Latn", "ind_Latn"]
SPACE = "▁"


def encode_word(word, scores):
    """对一个以 ▁ 开头的词做 BPE：反复合并分数最高且在词表中的相邻符号对。"""
    syms = list(word)
    while len(syms) > 1:
        best, best_i = None, -1
        for i in range(len(syms) - 1):
            cand = syms[i] + syms[i + 1]
            sc = scores.get(cand)
            if sc is not None and (best is None or sc > best):
                best, best_i = sc, i
        if best_i < 0:
            break
        syms[best_i:best_i + 2] = [syms[best_i] + syms[best_i + 1]]
    return syms


def encode(text, scores):
    """整句分词：NFKC 规范化，空白换成 ▁，CJK 等无空格文字按整段处理（词内合并）。"""
    text = unicodedata.normalize("NFKC", text).strip()
    out = []
    for w in text.split():
        out.extend(encode_word(SPACE + w, scores))
    return out


def read_lang(flores, code):
    """读 dev 与 devtest 全部句子。"""
    sents = []
    for part in ("dev", "devtest"):
        path = os.path.join(flores, part, f"{code}.{part}")
        if os.path.exists(path):
            sents += [l.rstrip("\n") for l in open(path, encoding="utf-8")]
    return sents


def main():
    """入口。"""
    ap = argparse.ArgumentParser()
    ap.add_argument("--spm", required=True)
    ap.add_argument("--flores", required=True)
    args = ap.parse_args()
    pieces = parse_spm(args.spm)
    scores = {t: s for t, s, typ in pieces if typ == 1}
    used = {}
    per_lang = {}
    for code in sorted(set(MAIN14)):
        sents = read_lang(args.flores, code)
        seen = {}
        for s in sents:
            for p in encode(s, scores):
                seen[p] = seen.get(p, 0) + 1
        per_lang[code] = seen
        print(f"{code:10s} 句子={len(sents):5d}  用到不同片段={len(seen):6d}  平均每句片段={sum(seen.values()) / max(len(sents), 1):.1f}")
    for name, langs in (("联合国六语", UN6), ("14 种主流语言", MAIN14)):
        union = set()
        for code in langs:
            union |= set(per_lang[code])
        v = len(union) + 4 + len(langs) + 1  # 特殊符号 + 语言码 + 余量
        print(f"\n{name}: 并集片段={len(union)}，估计裁剪后词表 V≈{v}（含特殊符号与语言码）")
        non_emb = 615_073_792 - 256_206 * 1024  # 近似非嵌入参数量
        params = non_emb + v * 1024
        print(f"   参数量≈{params / 1e6:.0f}M；int8 权重≈{params / 1048576:.0f}MiB；int4 权重≈{params / 2 / 1048576:.0f}MiB（仅权重，不含激活与 KV 缓存）")


if __name__ == "__main__":
    main()
