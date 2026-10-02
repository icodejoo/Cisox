"""Hy-MT2-1.8B 的 transformers CPU 评测（质量上限）：逐句贪心，支持断点续跑。

用法：python hf_eval.py --version hymt2-bf16 --dtype bfloat16 [--pairs core11] [--limit 30]
      [--rep-penalty 1.05] [--threads 6] [--max-new 512]
"""
import argparse
import json
import os
import statistics
import time

import hymt_common as C


def main():
    """入口：加载模型，按语向逐句生成并落盘。"""
    ap = argparse.ArgumentParser()
    ap.add_argument("--version", required=True)
    ap.add_argument("--dtype", choices=["bfloat16", "float32"], default="bfloat16")
    ap.add_argument("--pairs", default="core11")
    ap.add_argument("--limit", type=int, default=30)
    ap.add_argument("--rep-penalty", type=float, default=1.05)
    ap.add_argument("--threads", type=int, default=6)
    ap.add_argument("--max-new", type=int, default=512)
    a = ap.parse_args()
    import torch
    from transformers import AutoModelForCausalLM, AutoTokenizer
    torch.set_num_threads(a.threads)
    t0 = time.perf_counter()
    tok = AutoTokenizer.from_pretrained(C.MODEL_DIR)
    model = AutoModelForCausalLM.from_pretrained(C.MODEL_DIR, torch_dtype=getattr(torch, a.dtype)).eval()
    load_s = time.perf_counter() - t0
    print(f"loaded {load_s:.1f}s", flush=True)
    for src, tgt in C.parse_pairs(a.pairs):
        st = C.PairStore(a.version, src, tgt, a.limit)
        if st.done():
            st.finish()
            continue
        srcs = C.read_flores(src, a.limit)
        for i in range(len(st.rows), a.limit):
            ids = tok.apply_chat_template(C.build_messages(srcs[i], tgt), add_generation_prompt=True,
                                          return_tensors="pt", return_dict=True)
            ids.pop("token_type_ids", None)
            t = time.perf_counter()
            with torch.inference_mode():
                out = model.generate(**ids, do_sample=False, temperature=None, top_p=None, top_k=None,
                                     repetition_penalty=a.rep_penalty, max_new_tokens=a.max_new,
                                     eos_token_id=C.EOS_ID, pad_token_id=120002)
            gen = out[0, ids["input_ids"].shape[1]:]
            sec = time.perf_counter() - t
            text = tok.decode(gen, skip_special_tokens=True)
            st.add(text, int(gen.shape[0]), sec)
            print(f"{src}-{tgt} {i+1}/{a.limit} {sec:.1f}s ntok={int(gen.shape[0])}", flush=True)
        st.finish()
    print("done")


if __name__ == "__main__":
    main()
