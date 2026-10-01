"""用 NLLB 自己的训练数据（CCMatrix 开头切片）选词表，并画出"词表大小 V ↔ FLORES devtest 覆盖率"曲线。

步骤：1) 并行下载 data.statmt.org/cc-matrix/<语对>.bitextf.tsv.gz 的开头若干 MB（文件按 LASER 分数降序，开头质量最高），
增量解压取前 N 句；2) 用原版 sentencepiece 模型精确分词，统计各语言片段频率；
3) 按"各语言相对频率的最大值"排序取前 K，评测 devtest 的片段出现覆盖率与整句全覆盖率。评测集 devtest 绝不进入统计。
用法: python ccmatrix_vocab_curve.py --spm <sentencepiece.bpe.model> --flores <flores200_dataset> --work <工作目录>
"""
import argparse
import collections
import json
import os
import subprocess
import sys
import threading
import zlib

import sentencepiece as spm

# FLORES 语言码 → CCMatrix 语对文件名与列顺序（col2 是文件名里的前一种语言）
PAIRS = {
    "zho_Hans": ("en-zh", "zh"), "fra_Latn": ("en-fr", "fr"), "spa_Latn": ("en-es", "es"), "rus_Cyrl": ("en-ru", "ru"),
    "arb_Arab": ("ar-en", "ar"), "deu_Latn": ("de-en", "de"), "jpn_Jpan": ("en-ja", "ja"), "kor_Hang": ("en-ko", "ko"),
    "por_Latn": ("en-pt", "pt"), "ita_Latn": ("en-it", "it"), "tur_Latn": ("en-tr", "tr"), "vie_Latn": ("en-vi", "vi"),
    "ind_Latn": ("en-id", "id"),
}
UN6 = ["zho_Hans", "eng_Latn", "fra_Latn", "spa_Latn", "rus_Cyrl", "arb_Arab"]
MAIN14 = UN6 + ["deu_Latn", "jpn_Jpan", "kor_Hang", "por_Latn", "ita_Latn", "tur_Latn", "vie_Latn", "ind_Latn"]


def fetch_head(pair, max_bytes, max_lines, out_path):
    """下载并增量解压语对文件开头，保存前 max_lines 行（三列 TSV：分数、col2、col3）。已有则跳过。"""
    if os.path.exists(out_path):
        return
    url = f"http://data.statmt.org/cc-matrix/{pair}.bitextf.tsv.gz"
    dec = zlib.decompressobj(31)
    got, lines, tail = 0, [], ""
    # 本机 Python 的 HTTPS 握手会失败（需要 curl 的 --ssl-no-revoke），改用 curl 取数据流，读够开头字节后终止
    proc = subprocess.Popen(["curl", "-sL", "--ssl-no-revoke", "--max-time", "900", url], stdout=subprocess.PIPE)
    try:
        while got < max_bytes and len(lines) < max_lines:
            chunk = proc.stdout.read(1 << 20)
            if not chunk:
                break
            got += len(chunk)
            text = tail + dec.decompress(chunk).decode("utf-8", "ignore")
            parts = text.split("\n")
            tail = parts.pop()
            lines.extend(parts)
    finally:
        proc.kill()
    with open(out_path + ".tmp", "w", encoding="utf-8") as f:
        f.write("\n".join(lines[:max_lines]) + "\n")
    os.replace(out_path + ".tmp", out_path)


def load_lang(work, code, n):
    """从已下载切片取某语言一侧的句子（去重、长度过滤），最多 n 句。"""
    sents, seen = [], set()
    targets = [("eng_Latn", None)] if code == "eng_Latn" else [(code, PAIRS.get(code))]
    if code == "eng_Latn":
        files = [(f"{p}.tsv", 1 if p.startswith("en-") else 2) for p, _ in PAIRS.values()][:3]
    else:
        pair, cc = PAIRS[code]
        files = [(f"{pair}.tsv", 1 if pair.startswith(f"{cc}-") else 2)]
    for fname, col in files:
        path = os.path.join(work, fname)
        for line in open(path, encoding="utf-8"):
            cols = line.rstrip("\n").split("\t")
            if len(cols) < 3:
                continue
            s = cols[col].strip()
            if 8 <= len(s) <= 400 and s not in seen:
                seen.add(s)
                sents.append(s)
                if len(sents) >= n:
                    return sents
    return sents


def read_flores(flores, part, code):
    """读 FLORES 某切分某语言的句子。"""
    return [l.rstrip("\n") for l in open(os.path.join(flores, part, f"{code}.{part}"), encoding="utf-8")]


def main():
    """入口。"""
    ap = argparse.ArgumentParser()
    ap.add_argument("--spm", required=True)
    ap.add_argument("--flores", required=True)
    ap.add_argument("--work", required=True)
    ap.add_argument("--sentences", type=int, default=300000, help="每种语言用于统计的句子数")
    ap.add_argument("--max-mb", type=int, default=40, help="每个语对最多下载的压缩 MB")
    args = ap.parse_args()
    os.makedirs(args.work, exist_ok=True)

    threads = [threading.Thread(target=fetch_head, args=(p, args.max_mb << 20, args.sentences + 20000, os.path.join(args.work, f"{p}.tsv"))) for p, _ in PAIRS.values()]
    for t in threads:
        t.start()
    for t in threads:
        t.join()
    print("下载完成：", {p: os.path.getsize(os.path.join(args.work, f"{p}.tsv")) // 1048576 for p, _ in PAIRS.values()}, "MB（解压后文本）")

    sp = spm.SentencePieceProcessor(model_file=args.spm)
    n = sp.get_piece_size()
    freq = {}
    for code in MAIN14:
        sents = load_lang(args.work, code, args.sentences)
        c = collections.Counter()
        for i in range(0, len(sents), 20000):
            for ids in sp.encode(sents[i:i + 20000]):
                c.update(ids)
        freq[code] = c
        print(f"{code}: 统计句子 {len(sents)}，出现 {sum(c.values())} 个片段，不同片段 {len(c)}")
    dev_used = {code: set(i for s in read_flores(args.flores, "dev", code) for i in sp.encode(s)) for code in MAIN14}

    out = {}
    for name, langs in (("联合国六语", UN6), ("14 语言", MAIN14)):
        test = [ids for code in langs for ids in sp.encode(read_flores(args.flores, "devtest", code))]
        occ = collections.Counter(i for ids in test for i in ids)
        total = sum(occ.values())
        score = collections.defaultdict(float)
        for code in langs:
            tot = sum(freq[code].values())
            for i, cnt in freq[code].items():
                score[i] = max(score[i], cnt / tot)
        ranked = sorted(score, key=lambda i: -score[i])
        dev_all = set().union(*(dev_used[c] for c in langs))

        def rep(label, kept):
            cov = sum(c for i, c in occ.items() if i in kept) / total
            sent = sum(all(i in kept for i in ids) for ids in test) / len(test)
            print(f"  {label:42s} V={len(kept):7d}  devtest 片段覆盖 {100 * cov:6.2f}%  整句全覆盖 {100 * sent:5.1f}%  缺失种类 {sum(1 for i in occ if i not in kept)}")
            return {"V": len(kept), "cov": cov, "sent": sent}

        print(f"\n===== {name}：devtest {len(test)} 句，{total} 个片段出现 =====")
        res = {"dev": rep("A. FLORES dev（初版思路）", dev_all)}
        for k in (30000, 40000, 50000, 60000, 80000, 100000, 130000, 160000):
            res[f"ccm{k}"] = rep(f"D. CCMatrix 词频前 {k}", set(ranked[:k]) | set(range(4)))
        res["ccm_all"] = rep(f"D. CCMatrix 出现过的全部片段（{len(ranked)}）", set(ranked) | set(range(4)))
        res["ccm60+dev"] = rep("E. CCMatrix 前 60000 ∪ dev", set(ranked[:60000]) | dev_all | set(range(4)))
        out[name] = res
    json.dump(out, open(os.path.join(args.work, "curve.json"), "w", encoding="utf-8"), ensure_ascii=False, indent=1)


if __name__ == "__main__":
    main()
