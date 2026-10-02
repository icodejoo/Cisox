"""Rust worker 译文与 Python ORT 参考的一致性对拍。

worker 会先按句末标点分句、逐句翻译，再做中文排版整理（全角标点、去空格）；这里的 Python 参考用同样的分句规则
逐句翻译（同一个量化 ONNX、beam=4、无 no_repeat），比较前把两边都归一化（去空白、半角标点转全角），
这样只比"字词是否一致"，不比排版。

用法示例：
    python compare_parity.py --dir E:/models/translate-eval/opusmt-en-zh-int4 --direction en-zh \
        --src-file .../src.txt --rust-json build/mt-logs/opusmt/rust-en-zh-int4.json --limit 10
"""
import argparse
import json
import os
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, os.path.dirname(HERE))
ASCII_END = ".?!"
CJK_END = "。？！…"
CLOSERS = "\"')”’」）"
HALF_TO_FULL = {",": "，", ".": "。", "?": "？", "!": "！", ":": "：", ";": "；", "(": "（", ")": "）"}
DIRECTIONS = {"en-zh": ">>cmn_Hans<< ", "zh-en": ""}


def split_sentences(text):
    """按 text.rs::split_sentences 的规则分句（只取正文，不要分隔符）。"""
    segs, cur, i, n = [], "", 0, len(text)
    while i < n:
        c = text[i]
        if c in "\r\n":
            while i < n and text[i].isspace():
                i += 1
            if cur.strip():
                segs.append(cur.strip())
            cur = ""
            continue
        cur += c
        j = i + 1
        ended = c in CJK_END
        if not ended and c in ASCII_END:
            while j < n and text[j] in CLOSERS:
                j += 1
            ended = j >= n or text[j].isspace()
        i += 1
        if ended:
            while i < n and text[i] in CLOSERS:
                cur += text[i]
                i += 1
            while i < n and text[i].isspace():
                i += 1
            if cur.strip():
                segs.append(cur.strip())
            cur = ""
    if cur.strip():
        segs.append(cur.strip())
    return segs


def norm(s):
    """归一化：去全部空白，半角标点转全角。"""
    return "".join(HALF_TO_FULL.get(ch, ch) for ch in s if not ch.isspace())


def main():
    """入口：生成 Python 参考并逐句对拍。"""
    ap = argparse.ArgumentParser()
    ap.add_argument("--dir", required=True)
    ap.add_argument("--direction", required=True, choices=sorted(DIRECTIONS))
    ap.add_argument("--src-file", required=True)
    ap.add_argument("--rust-json", required=True)
    ap.add_argument("--limit", type=int, default=10)
    ap.add_argument("--threads", type=int, default=4)
    a = ap.parse_args()
    import onnxruntime as ort
    import torch
    torch.set_num_threads(1)
    from export_quant_onnx import patch_normalized_config
    patch_normalized_config()
    from optimum.onnxruntime import ORTModelForSeq2SeqLM
    from transformers import MarianTokenizer
    so = ort.SessionOptions()
    so.intra_op_num_threads = a.threads
    model = ORTModelForSeq2SeqLM.from_pretrained(a.dir, use_merged=True, use_cache=True, provider="CPUExecutionProvider",
                                                 session_options=so, use_io_binding=False)
    tok = MarianTokenizer.from_pretrained(a.dir)
    prefix = DIRECTIONS[a.direction]
    with open(a.src_file, encoding="utf-8") as f:
        srcs = f.read().rstrip("\n").split("\n")[:a.limit]
    rust = json.load(open(a.rust_json, encoding="utf-8"))["hyps"]
    exact = 0
    for i, s in enumerate(srcs):
        segs = split_sentences(s)
        outs = []
        for seg in segs:
            b = tok([prefix + seg], return_tensors="pt", padding=True)
            with torch.inference_mode():
                out = model.generate(**b, num_beams=4, max_new_tokens=256)
            outs.append(tok.batch_decode(out, skip_special_tokens=True)[0])
        ref = "".join(outs) if a.direction == "en-zh" else " ".join(outs)
        ok = norm(ref) == norm(rust[i])
        exact += ok
        if not ok:
            print(f"#{i} DIFF\n  src   : {s}\n  rust  : {rust[i]}\n  python: {ref}")
    print(f"一致（忽略空白与半角/全角标点）: {exact}/{len(srcs)}")


if __name__ == "__main__":
    main()
