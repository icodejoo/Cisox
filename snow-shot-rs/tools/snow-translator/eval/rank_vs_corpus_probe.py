"""对比三种选词表方式在 devtest 上的片段覆盖率（无模型，仅分词统计）。

方式 A：按 FLORES dev 语料实际用到的片段（本次初版）；
方式 B：不用任何外部语料，按词表自带的分数（BPE 合并优先级≈NLLB 训练语料整体频率排名）取前 K，
        可选限定在目标文字内；
覆盖率 = devtest 分词结果里，片段落在保留集合内的比例（按出现次数），以及整句片段全部在集合内的句子比例。
用法: python rank_vs_corpus_probe.py <sentencepiece.bpe.model> <flores_dir>
"""
import collections
import os
import sys
import unicodedata

import sentencepiece as spm

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from vocab_coverage import script_of  # noqa: E402

MAIN14 = ["zho_Hans", "eng_Latn", "fra_Latn", "spa_Latn", "rus_Cyrl", "arb_Arab", "deu_Latn", "jpn_Jpan",
          "kor_Hang", "por_Latn", "ita_Latn", "tur_Latn", "vie_Latn", "ind_Latn"]
UN6 = MAIN14[:6]
SPACE = "\u2581"


def read(flores, part, code):
    """读 FLORES 某语言某切分的全部句子。"""
    path = os.path.join(flores, part, f"{code}.{part}")
    return [l.rstrip("\n") for l in open(path, encoding="utf-8")]


def piece_scripts(piece):
    """片段所含文字集合（忽略数字、标点、词首标记）。"""
    out = set()
    for ch in piece.replace(SPACE, ""):
        if ch.isdigit() or unicodedata.category(ch).startswith(("P", "S", "Z", "M")):
            continue
        out.add(script_of(ch))
    return out


def main():
    """入口。"""
    model, flores = sys.argv[1], sys.argv[2]
    sp = spm.SentencePieceProcessor(model_file=model)
    n = sp.get_piece_size()
    scores = [sp.get_score(i) for i in range(n)]
    print(f"词表 {n} 片段；分数范围 {min(scores):.1f} ~ {max(scores):.1f}")
    for name, langs in (("联合国六语", UN6), ("14 语言", MAIN14)):
        scripts = {script_of(ch) for code in langs for ch in {
            "zho_Hans": "中", "eng_Latn": "a", "fra_Latn": "a", "spa_Latn": "a", "rus_Cyrl": "я", "arb_Arab": "ع",
            "deu_Latn": "a", "jpn_Jpan": "あ", "kor_Hang": "한", "por_Latn": "a", "ita_Latn": "a", "tur_Latn": "a",
            "vie_Latn": "a", "ind_Latn": "a"}[code]}
        dev_used, test_ids = set(), []
        for code in langs:
            for s in read(flores, "dev", code):
                dev_used.update(sp.encode(s))
            test_ids.extend(sp.encode(s) for s in read(flores, "devtest", code))
        occ = collections.Counter(i for ids in test_ids for i in ids)
        total = sum(occ.values())
        distinct_test = len(occ)

        def report(label, kept):
            cov = sum(c for i, c in occ.items() if i in kept) / total
            sent = sum(all(i in kept for i in ids) for ids in test_ids) / len(test_ids)
            miss_types = sum(1 for i in occ if i not in kept)
            print(f"  {label:44s} 保留 {len(kept):7d}  devtest 片段出现覆盖 {100 * cov:6.2f}%  整句全覆盖的句子 {100 * sent:5.1f}%  缺失的不同片段 {miss_types}")

        print(f"\n===== {name}（devtest 共 {len(test_ids)} 句，{total} 个片段出现，{distinct_test} 种不同片段）=====")
        report("A. FLORES dev 实际用到的片段（本次初版思路）", dev_used)
        eligible = [i for i in range(n) if i >= 4 and (not piece_scripts(sp.id_to_piece(i)) or piece_scripts(sp.id_to_piece(i)) <= scripts)]
        ranked = sorted(eligible, key=lambda i: -scores[i])
        print(f"  （限定在目标文字内的片段共 {len(eligible)} 个）")
        for k in (20000, 40000, 60000, 80000, 120000, 160000):
            report(f"B. 不用语料，按自带分数取前 {k}（限定目标文字）", set(range(4)) | set(ranked[:k]))
        # 取前 K 且与 dev 用到的并集
        for k in (20000, 40000):
            report(f"C. dev 用到的 ∪ 自带分数前 {k}", dev_used | set(ranked[:k]) | set(range(4)))


if __name__ == "__main__":
    main()
