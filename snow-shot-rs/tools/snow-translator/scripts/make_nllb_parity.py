"""生成 Rust worker 译文一致性夹具（tests/fixtures/nllb_parity.json）。

做法：选 FLORES devtest 里只含一个句子的原文；分别用 (a) 裁剪后分词器（与 Rust 同一份 tokenizer.json）
与 (b) 评测所用的 remap 分词得到编码器输入 id，交给纯 ORT 束搜索（build/mt-quant/purebeam.py，
beam=2，无 no_repeat、长度惩罚 2.0、最小长度比例 0.7，即 m2m100 族推荐解码配置）译出 id，
再用同一分词器解码成文本；中日文目标再过 eval/zh_punct.py 的全角标点后处理，写入夹具。
要求环境里是 ORT 1.28.0（与 worker 同版本）：PYTHONPATH 指向 ort128 目录。
用法示例：
  PYTHONPATH=E:/workspaces/Cisox/build/mt-quant/ort128 python make_nllb_parity.py \
      --pack E:/models/translate-eval/nllb600m-main14-ccm-int4-ext \
      --pruned E:/models/translate-eval/nllb600m-pruned-main14-ccm-fp32 \
      --purebeam E:/workspaces/Cisox/build/mt-quant/purebeam.py \
      --out ../tests/fixtures/nllb_parity.json
"""
import argparse
import json
import os
import subprocess
import sys
import tempfile

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, os.path.join(HERE, "..", "eval"))
from eval_quant import FLORES, ORIG_DIR  # noqa: E402
from prune_nllb_vocab import PrunedNllbTokenizer  # noqa: E402
from zh_punct import to_fullwidth  # noqa: E402
from tokenizers import Tokenizer  # noqa: E402

# (应用源语言, 应用目标语言, FLORES 源, FLORES 目标, 句数)
PAIRS = [
    ("zh-CN", "en", "zho_Hans", "eng_Latn", 3),
    ("en", "fr", "eng_Latn", "fra_Latn", 3),
    ("en", "zh-CN", "eng_Latn", "zho_Hans", 3),
    ("ja", "en", "jpn_Jpan", "eng_Latn", 2),
    ("en", "ja", "eng_Latn", "jpn_Jpan", 3),
]
TERMINATORS = "。？！….?!"
MAX_CHARS = 140


def single_sentence(text):
    """只含一个句子：句末标点（含可能的收尾引号）只出现在末尾，长度适中。"""
    t = text.strip().rstrip("\"'”’」）)")
    if not t or len(text) > MAX_CHARS or "\n" in text:
        return False
    body = t[:-1] if t[-1] in TERMINATORS else t
    return not any(c in TERMINATORS for c in body) and ". " not in text


def pick(lang, n):
    """取 devtest 前 200 句里满足条件的前 n 句，返回 [(序号, 文本)]。"""
    with open(f"{FLORES}/devtest/{lang}.devtest", encoding="utf-8") as f:
        lines = [l.rstrip("\n") for _, l in zip(range(200), f)]
    return [(i, s) for i, s in enumerate(lines) if single_sentence(s)][:n]


def run_purebeam(a, tok_path, out_path, pairs):
    """调用 purebeam.py 并返回 {pair: [输出 id 列表]}。"""
    cmd = [sys.executable, a.purebeam, "--dir", a.pack, "--tok", tok_path, "--out", out_path,
           "--beams", "2", "--lp", "2.0", "--min-ratio", "0.7", "--pairs", ",".join(pairs), "--limit", "99",
           "--enc-file", "encoder.onnx", "--dec-file", "decoder.onnx"]
    subprocess.run(cmd, check=True, cwd=os.path.dirname(a.purebeam))
    return json.load(open(out_path))


def main():
    """命令行入口。"""
    ap = argparse.ArgumentParser()
    ap.add_argument("--pack", required=True)
    ap.add_argument("--pruned", required=True)
    ap.add_argument("--purebeam", required=True)
    ap.add_argument("--out", required=True)
    a = ap.parse_args()
    tk = Tokenizer.from_file(os.path.join(a.pack, "tokenizer.json"))
    remap = PrunedNllbTokenizer(ORIG_DIR, a.pruned, mode="remap")
    drop = {0, 1, 2, 3} | {tk.token_to_id(t) for t in
                           ("eng_Latn", "zho_Hans", "fra_Latn", "jpn_Jpan", "spa_Latn", "rus_Cyrl",
                            "arb_Arab", "deu_Latn", "kor_Hang", "por_Latn", "ita_Latn", "tur_Latn",
                            "vie_Latn", "ind_Latn")}

    def detok(ids, tgt):
        """解码并按目标语言做全角标点后处理（与 Rust worker 一致）。"""
        text = tk.decode([i for i in ids if i not in drop], skip_special_tokens=False).strip()
        return to_fullwidth(text, tgt)

    plan = []
    for app_src, app_tgt, f_src, f_tgt, n in PAIRS:
        plan.append((app_src, app_tgt, f_src, f_tgt, pick(f_src, n)))
    names = [f"{p[2]}-{p[3]}" for p in plan]
    toks = {"reseg": {}, "remap": {}}
    for app_src, app_tgt, f_src, f_tgt, items in plan:
        key = f"{f_src}-{f_tgt}"
        forced = tk.token_to_id(f_tgt)
        toks["reseg"][key] = {"forced": forced, "src_ids": [
            [tk.token_to_id(f_src)] + tk.encode(s, add_special_tokens=False).ids + [2] for _, s in items]}
        toks["remap"][key] = {"forced": forced, "src_ids": [
            remap.encode([s], f_src)["input_ids"][0].tolist() for _, s in items]}
    outs = {}
    with tempfile.TemporaryDirectory() as tmp:
        for mode in ("reseg", "remap"):
            tp, op = os.path.join(tmp, f"{mode}_tok.json"), os.path.join(tmp, f"{mode}_out.json")
            json.dump(toks[mode], open(tp, "w"))
            outs[mode] = run_purebeam(a, tp, op, names)
    doc = {"source": "FLORES-200 devtest", "decoder": "purebeam.py beam=2 lp=2.0 min_ratio=0.7 no_repeat=off + zh_punct, ORT 1.28.0",
           "pack": os.path.basename(a.pack.rstrip("/\\")), "pairs": []}
    for app_src, app_tgt, f_src, f_tgt, items in plan:
        key = f"{f_src}-{f_tgt}"
        cases = []
        for k, (idx, text) in enumerate(items):
            cases.append({
                "flores_index": idx, "text": text,
                "reseg_input_ids": toks["reseg"][key]["src_ids"][k],
                "remap_input_ids": toks["remap"][key]["src_ids"][k],
                "reseg_output_ids": outs["reseg"]["ids"][key][k],
                "remap_output_ids": outs["remap"]["ids"][key][k],
                "reseg_text": detok(outs["reseg"]["ids"][key][k], f_tgt),
                "remap_text": detok(outs["remap"]["ids"][key][k], f_tgt),
            })
        doc["pairs"].append({"src": app_src, "tgt": app_tgt, "flores_src": f_src,
                             "flores_tgt": f_tgt, "cases": cases})
    with open(a.out, "w", encoding="utf-8", newline="\n") as f:
        json.dump(doc, f, ensure_ascii=False, indent=1)
        f.write("\n")
    print("wrote", a.out)


if __name__ == "__main__":
    main()
