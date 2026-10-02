"""生成 NLLB 分词金标准夹具（tests/fixtures/nllb_tokenizer_gold.json）。

对固定的 FLORES devtest 句子产出三套期望 id（均含源语言码前缀与结尾 </s>）：
  remap     评测所用：原版 25.6 万词表分词后映射到裁剪 id，被裁掉的片段落到 <unk>（3）
  reseg     裁剪后的 sentencepiece 模型重新分词（被裁掉的片段拆成更短的保留片段）
Rust 的 tokenizer.json 路线必须与 reseg 逐 id 一致；与 remap 只允许在含 <unk> 的句子上不同。
用法示例：
  python make_nllb_fixtures.py --pruned E:/models/translate-eval/nllb600m-pruned-main14-ccm-fp32 \
      --out ../tests/fixtures/nllb_tokenizer_gold.json
"""
import argparse
import json
import os
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, os.path.join(HERE, "..", "eval"))
from eval_quant import FLORES, ORIG_DIR  # noqa: E402
from prune_nllb_vocab import PrunedNllbTokenizer  # noqa: E402

# 语言码 -> 取 devtest 的句子序号（0 起）；覆盖中、日、阿、俄、英、法，共 20 句
PICKS = {
    "zho_Hans": [0, 1, 2, 3],
    "jpn_Jpan": [0, 1, 2],
    "arb_Arab": [0, 1, 2],
    "rus_Cyrl": [0, 1, 2],
    "eng_Latn": [0, 1, 2, 3],
    "fra_Latn": [0, 1, 2],
}


def main():
    """命令行入口。"""
    ap = argparse.ArgumentParser()
    ap.add_argument("--pruned", required=True)
    ap.add_argument("--out", required=True)
    a = ap.parse_args()
    remap = PrunedNllbTokenizer(ORIG_DIR, a.pruned, mode="remap")
    reseg = PrunedNllbTokenizer(ORIG_DIR, a.pruned, mode="resegment")
    cases = []
    for lang, idxs in PICKS.items():
        with open(f"{FLORES}/devtest/{lang}.devtest", encoding="utf-8") as f:
            lines = [l.rstrip("\n") for l in f]
        for i in idxs:
            text = lines[i]
            cases.append({
                "lang": lang, "flores_index": i, "text": text,
                "remap_ids": remap.encode([text], lang)["input_ids"][0].tolist(),
                "reseg_ids": reseg.encode([text], lang)["input_ids"][0].tolist(),
            })
    doc = {"source": "FLORES-200 devtest", "pruned_vocab": os.path.basename(a.pruned.rstrip("/\\")),
           "cases": cases}
    with open(a.out, "w", encoding="utf-8", newline="\n") as f:
        json.dump(doc, f, ensure_ascii=False, indent=1)
        f.write("\n")
    print(len(cases), "cases ->", a.out)


if __name__ == "__main__":
    main()
