"""Hy-MT2 的纯 onnxruntime + numpy + tokenizers 贪心生成（不导入 torch / transformers）。

流程与 Rust worker 一致：手工套 chat 模板 -> tokenizers 编码 -> 预填充（一次喂完提示词，past 长度 0）-> 逐 token 解码
（KV cache 作为输入输出回传）。贪心、可选 repetition_penalty（与 HF 同公式：提示词与已生成 token 的 logits 惩罚）、
遇 eos（120020）或 max_new 停止。结果经 hymt_common.PairStore 落盘，支持断点续跑。

用法：python ort_gen.py --dir E:/models/translate-eval/hymt2-1.8b-int4 --version hymt2-int4 [--pairs core11]
      [--limit 30] [--rep-penalty 1.05] [--threads 6] [--max-new 512]
"""
import argparse
import json
import os
import statistics
import time

import numpy as np

import hymt_common as C
from eval_quant import Sampler, mem_mib

CHAT_PREFIX = "<｜hy_begin▁of▁sentence｜><｜hy_User｜>"  # chat_template.jinja：无 system 时的开头
CHAT_SUFFIX = "<｜hy_Assistant｜>"  # add_generation_prompt=True 的结尾
LAYERS, KV_HEADS, HEAD_DIM = 32, 4, 128


def prompt_text(text, tgt):
    """按 chat 模板（无 system、含 generation prompt）拼出完整提示词字符串。"""
    return CHAT_PREFIX + C.build_messages(text, tgt)[0]["content"] + CHAT_SUFFIX


class Generator:
    """封装 ORT 会话与贪心生成循环。"""

    def __init__(self, model_dir, threads):
        import onnxruntime as ort
        from tokenizers import Tokenizer
        self.tok = Tokenizer.from_file(os.path.join(model_dir, "tokenizer.json"))
        so = ort.SessionOptions()
        if threads:
            so.intra_op_num_threads = min(threads, 4)  # 约束：评测线程不超过 4
        # 环境变量开关（只给内存探针用）：HYMT_NOPREPACK=1 关预打包；HYMT_NOARENA=1 关 CPU 内存池
        if os.environ.get("HYMT_NOPREPACK"):
            so.add_session_config_entry("session.disable_prepacking", "1")
        if os.environ.get("HYMT_NOARENA"):
            so.enable_cpu_mem_arena = False
            so.enable_mem_pattern = False
        self.sess = ort.InferenceSession(os.path.join(model_dir, "model.onnx"), so, providers=["CPUExecutionProvider"])
        pj = os.path.join(model_dir, "pruned.json")  # 词表裁剪版带新的 eos id
        self.eos = json.load(open(pj, encoding="utf-8"))["eos_id"] if os.path.exists(pj) else C.EOS_ID
        self.in_names = {i.name for i in self.sess.get_inputs()}
        self.out_names = [o.name for o in self.sess.get_outputs()]

    def encode(self, text, tgt):
        """返回提示词 token id 列表。"""
        return self.tok.encode(prompt_text(text, tgt), add_special_tokens=False).ids

    def generate(self, ids, max_new, rep_penalty):
        """贪心生成，返回新生成的 token id 列表（不含 eos）。"""
        past = {f"past_key_values.{l}.{kv}": np.zeros((1, KV_HEADS, 0, HEAD_DIM), np.float32)
                for l in range(LAYERS) for kv in ("key", "value")}
        cur = np.array([ids], np.int64)
        seen = set(ids)
        total = 0
        out = []
        for _ in range(max_new):
            n = cur.shape[1]
            feed = {"input_ids": cur, "attention_mask": np.ones((1, total + n), np.int64),
                    "position_ids": np.arange(total, total + n, dtype=np.int64)[None, :]}
            feed.update(past)
            res = self.sess.run(self.out_names, feed)
            logits = res[0][0, -1].astype(np.float32)
            if rep_penalty != 1.0 and seen:
                idx = np.fromiter(seen, np.int64)
                v = logits[idx]
                logits[idx] = np.where(v < 0, v * rep_penalty, v / rep_penalty)
            nxt = int(np.argmax(logits))
            if nxt == self.eos:
                break
            out.append(nxt)
            seen.add(nxt)
            total += n
            for name, arr in zip(self.out_names[1:], res[1:]):
                past[name.replace("present.", "past_key_values.")] = arr
            cur = np.array([[nxt]], np.int64)
        return out


def main():
    """入口：加载、按语向逐句生成并落盘，写 metrics.json。"""
    ap = argparse.ArgumentParser()
    ap.add_argument("--dir", required=True)
    ap.add_argument("--version", required=True)
    ap.add_argument("--pairs", default="core11")
    ap.add_argument("--limit", type=int, default=30)
    ap.add_argument("--rep-penalty", type=float, default=1.05)
    ap.add_argument("--threads", type=int, default=6)
    ap.add_argument("--max-new", type=int, default=512)
    a = ap.parse_args()
    import ctypes  # 降为 Idle 优先级，避免占满机器
    ctypes.windll.kernel32.SetPriorityClass(ctypes.windll.kernel32.GetCurrentProcess(), 0x40)
    base, _, _ = mem_mib()
    t0 = time.perf_counter()
    g = Generator(a.dir, a.threads)
    load_s = time.perf_counter() - t0
    g.generate(g.encode("Hello.", "fra_Latn"), 8, a.rep_penalty)  # 预热
    ws_load = mem_mib()[0]
    sampler = Sampler()
    sampler.start()
    lats, ntoks = [], []
    for src, tgt in C.parse_pairs(a.pairs):
        st = C.PairStore(a.version, src, tgt, a.limit)
        if not st.done():
            srcs = C.read_flores(src, a.limit)
            for i in range(len(st.rows), a.limit):
                t = time.perf_counter()
                out = g.generate(g.encode(srcs[i], tgt), a.max_new, a.rep_penalty)
                sec = time.perf_counter() - t
                st.add(g.tok.decode(out, skip_special_tokens=True), len(out), sec)
                print(f"{src}-{tgt} {i+1}/{a.limit} {sec:.1f}s ntok={len(out)}", flush=True)
        st.finish()
        lats += [r["sec"] for r in st.rows]
        ntoks += [r["ntok"] for r in st.rows]
    sampler.stop()
    ws_end = mem_mib()[0]
    m = {"version": a.version, "dir": a.dir, "threads": a.threads, "rep_penalty": a.rep_penalty, "max_new": a.max_new,
         "base_ws_mib": round(base), "load_s": round(load_s, 1), "load_delta_mib": round(ws_load - base),
         "peak_delta_mib": round(sampler.max_ws - base), "end_delta_mib": round(ws_end - base),
         "lat_mean_s": round(statistics.mean(lats), 2), "lat_p50_s": round(statistics.median(lats), 2),
         "mean_ntok": round(statistics.mean(ntoks), 1), "sentences": len(lats),
         "note": "延迟与内存是在机器有其它评测占用 CPU 时测得，只作粗略值"}
    print(json.dumps(m, ensure_ascii=False))
    os.makedirs(C.RESULTS + "/_metrics", exist_ok=True)
    with open(f"{C.RESULTS}/_metrics/{a.version}.json", "w", encoding="utf-8", newline="\n") as f:
        json.dump(m, f, indent=1, ensure_ascii=False)


if __name__ == "__main__":
    main()
