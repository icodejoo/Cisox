"""Hy-MT2 int4 包的词表裁剪（+ 可选的嵌入/lm_head 共享），直接对现有 int4 ONNX 做图手术，不重新导出 fp32。

做法：
1) 用 FLORES dev（不含评测用的 devtest）14 种语言的全部句子和提示词模板分词，统计用到的 token；
   加上全部特殊（added）token、单字符基础 token（含 256 个字节级字符），并按 BPE merges 补全祖先，得到保留集合；
2) 重建 tokenizer.json：词表按旧 id 顺序重新编号，merges 只留两端与结果都保留的项（新分词器直接输出新 id，
   Rust worker 不需要任何 id 映射层，只要换 tokenizer.json 并改 eos id）；
3) 图手术：嵌入 int8 表与 lm_head 的 int4 权重/缩放都只保留这些行，MatMulNBits 的 N 同步改；
4) --share：删掉独立的 int8 嵌入，改为在图内从 lm_head 的 int4 行（Gather 行 + 解包半字节 + 乘缩放）反量化出嵌入，
   即嵌入与 lm_head 共用同一份 int4 权重。

用法：python prune_hymt.py --src E:/models/translate-eval/hymt2-1.8b-int4-ext --dst <输出目录> [--share] [--extra-text file]
"""
import argparse
import json
import os
import shutil

import numpy as np

import hymt_common as C

FLORES_DEV = "E:/workspaces/Cisox/build/flores/flores200_dataset/dev"
OLD_EOS = C.EOS_ID


def collect_used_ids(tok_path, extra_files):
    """分词 FLORES dev 的 14 种语言与提示词模板，返回用到的旧 id 集合。"""
    from tokenizers import Tokenizer
    tok = Tokenizer.from_file(tok_path)
    used = set()
    lines = []
    for code in C.LANG_NAME:
        with open(f"{FLORES_DEV}/{code}.dev", encoding="utf-8") as f:
            lines += [l.rstrip("\n") for l in f]
    for p in extra_files:
        with open(p, encoding="utf-8") as f:
            lines += [l.rstrip("\n") for l in f]
    for code in C.LANG_NAME:
        lines.append(C.PROMPT.format(target_lang=C.LANG_NAME[code], source_text=""))
    for e in tok.encode_batch(lines, add_special_tokens=False):
        used.update(e.ids)
    return used


def build_keep(tj, used, head_k=0):
    """在旧 tokenizer.json 上求保留集合：used + added + 单字符 token，并补全 BPE 祖先。返回排序后的旧 id 列表。"""
    vocab = tj["model"]["vocab"]
    id2tok = {i: t for t, i in vocab.items()}
    keep = set(used) | {a["id"] for a in tj["added_tokens"]}
    keep |= set(range(head_k))  # BPE 合并序靠前的 token 近似高频，整段保留（补 dev 语料覆盖不到的常用字词）
    keep |= {i for t, i in vocab.items() if len(t) == 1}
    first_merge = {}
    for a, b in tj["model"]["merges"]:
        first_merge.setdefault(a + b, (a, b))
    stack = [id2tok[i] for i in keep if i in id2tok]
    seen = set(stack)
    while stack:
        t = stack.pop()
        if t in first_merge:
            for p in first_merge[t]:
                if p not in seen:
                    seen.add(p)
                    stack.append(p)
    keep |= {vocab[t] for t in seen}
    return sorted(keep)


def build_tokenizer(tj, keep):
    """生成裁剪后的 tokenizer.json 字典（新 id = 保留列表中的名次）。返回 (字典, 旧 id->新 id)。"""
    vocab = tj["model"]["vocab"]
    id2tok = {i: t for t, i in vocab.items()}
    new_of = {old: n for n, old in enumerate(keep)}
    nv = {id2tok[o]: n for o, n in new_of.items() if o in id2tok}
    out = json.loads(json.dumps(tj))
    out["model"]["vocab"] = nv
    out["model"]["merges"] = [[a, b] for a, b in tj["model"]["merges"] if a in nv and b in nv and (a + b) in nv]
    for a in out["added_tokens"]:
        a["id"] = new_of[a["id"]]
    return out, new_of


def prune_graph(src, dst, keep, share):
    """对 int4 ONNX 做图手术并存成外部数据；返回新词表大小。"""
    import onnx
    from onnx import TensorProto as T
    from onnx import helper, numpy_helper
    m = onnx.load(os.path.join(src, "model.onnx"))
    g = m.graph
    inits = {i.name: i for i in g.initializer}
    lm = next(n for n in g.node if n.name == "/lm_head/MatMul_Q4")
    nb_name, sc_name = lm.input[1], lm.input[2]
    old_n = next(a.i for a in lm.attribute if a.name == "N")
    rows = np.array(keep, np.int64)
    nn = len(rows)

    def cut(name):
        """按保留行切一个 initializer（第 0 维是词表）。"""
        arr = numpy_helper.to_array(inits[name])
        assert arr.shape[0] == old_n, (name, arr.shape)
        inits[name].CopyFrom(numpy_helper.from_array(np.ascontiguousarray(arr[rows]), name))

    cut(nb_name)
    cut(sc_name)
    for a in lm.attribute:
        if a.name == "N":
            a.i = nn
    for o in g.output:
        for d in o.type.tensor_type.shape.dim:
            if d.dim_value == old_n:
                d.dim_value = nn
    emb_w = "model.embed_tokens.weight_quantized"
    if not share:
        cut(emb_w)
    else:
        gather = next(n for n in g.node if n.name == "/model/embed_tokens/Gather")
        deq = next(n for n in g.node if n.name == "/model/embed_tokens/Gather_output_0_DequantizeLinear")
        ids_in, out_name = gather.input[1], deq.output[0]
        p = "/shared_embed/"

        def const(name, arr):
            """加一个常量 initializer，返回名字。"""
            g.initializer.append(numpy_helper.from_array(np.asarray(arr), p + name))
            return p + name

        c16, c8 = const("c16", np.int32(16)), const("c8", np.float32(8.0))
        ax_last = const("ax", np.array([-1], np.int64))
        shp1 = const("shp1", np.array([0, 0, -1, 32], np.int64))
        shp2 = const("shp2", np.array([0, 0, 2048], np.int64))
        nodes = [
            helper.make_node("Gather", [nb_name, ids_in], [p + "gb"], name=p + "gb", axis=0),
            helper.make_node("Gather", [sc_name, ids_in], [p + "gs"], name=p + "gs", axis=0),
            helper.make_node("Cast", [p + "gb"], [p + "gi"], name=p + "gi", to=T.INT32),
            helper.make_node("Div", [p + "gi", c16], [p + "hi"], name=p + "hi"),
            helper.make_node("Mul", [p + "hi", c16], [p + "hi16"], name=p + "hi16"),
            helper.make_node("Sub", [p + "gi", p + "hi16"], [p + "lo"], name=p + "lo"),
            helper.make_node("Unsqueeze", [p + "lo", ax_last], [p + "lou"], name=p + "lou"),
            helper.make_node("Unsqueeze", [p + "hi", ax_last], [p + "hiu"], name=p + "hiu"),
            helper.make_node("Concat", [p + "lou", p + "hiu"], [p + "cat"], name=p + "cat", axis=-1),
            helper.make_node("Cast", [p + "cat"], [p + "cf"], name=p + "cf", to=T.FLOAT),
            helper.make_node("Sub", [p + "cf", c8], [p + "cs"], name=p + "cs"),
            helper.make_node("Reshape", [p + "cs", shp1], [p + "rs"], name=p + "rs"),
            helper.make_node("Unsqueeze", [p + "gs", ax_last], [p + "gsu"], name=p + "gsu"),
            helper.make_node("Mul", [p + "rs", p + "gsu"], [p + "ml"], name=p + "ml"),
            helper.make_node("Reshape", [p + "ml", shp2], [out_name], name=p + "out"),
        ]
        old = list(g.node)
        pos = old.index(gather)
        rest = [n for n in old if n is not gather and n is not deq]
        del g.node[:]
        g.node.extend(rest[:pos] + nodes + rest[pos:])
        for nm in (emb_w, "model.embed_tokens.weight_scale", "model.embed_tokens.weight_zero_point"):
            g.initializer.remove(inits[nm])
    os.makedirs(dst, exist_ok=True)
    onnx.save_model(m, os.path.join(dst, "model.onnx"), save_as_external_data=True, all_tensors_to_one_file=True,
                    location="model.onnx_data", size_threshold=1024)
    return nn


def main():
    """入口：统计 -> 重建分词器 -> 图手术 -> 写旁文件。"""
    ap = argparse.ArgumentParser()
    ap.add_argument("--src", required=True)
    ap.add_argument("--dst", required=True)
    ap.add_argument("--share", action="store_true", help="嵌入与 lm_head 共用 int4 权重")
    ap.add_argument("--extra-text", nargs="*", default=[], help="额外语料（每行一句），并入保留集合")
    ap.add_argument("--head-k", type=int, default=0, help="额外整段保留旧 id < K 的 token")
    ap.add_argument("--stats-only", action="store_true")
    a = ap.parse_args()
    tp = os.path.join(a.src, "tokenizer.json")
    tj = json.load(open(tp, encoding="utf-8"))
    used = collect_used_ids(tp, a.extra_text)
    keep = build_keep(tj, used, a.head_k)
    print(f"用到 token {len(used)}，保留 {len(keep)} / {len(tj['model']['vocab']) + len(tj['added_tokens'])}")
    if a.stats_only:
        return
    new_tj, new_of = build_tokenizer(tj, keep)
    n = prune_graph(a.src, a.dst, keep, a.share)
    for fn in os.listdir(a.src):
        if os.path.splitext(fn)[1] not in (".onnx", ".onnx_data") and fn != "tokenizer.json":
            shutil.copy2(os.path.join(a.src, fn), os.path.join(a.dst, fn))
    with open(os.path.join(a.dst, "tokenizer.json"), "w", encoding="utf-8", newline="\n") as f:
        json.dump(new_tj, f, ensure_ascii=False)
    with open(os.path.join(a.dst, "pruned.json"), "w", encoding="utf-8", newline="\n") as f:
        json.dump({"vocab_size": n, "eos_id": new_of[OLD_EOS], "shared_embed": a.share, "kept_old_ids": keep}, f)
    for fn in sorted(os.listdir(a.dst)):
        print(fn, round(os.path.getsize(os.path.join(a.dst, fn)) / 1048576, 1), "MiB")


if __name__ == "__main__":
    main()
