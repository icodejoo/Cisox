"""NLLB-200-distilled-600M 词表裁剪（评测用）：只保留目标语言集 FLORES dev 分词用到的片段。

做法：
  1. 用 SentencePiece BPE 规则（NFKC 近似规范化）对语言集的 dev 句子分词，收集最终片段与合并过程的全部中间片段；
  2. 再加 4 个特殊符号（id 0~3 不变）、语言集的语言码、目标文字的全部单字符片段；
  3. 按保留 id 取嵌入行，构建新 HF 模型与 id_map.json，并写出裁剪后的 sentencepiece 模型。

用法示例：
    python prune_nllb_vocab.py --nllb E:/models/translate-eval/nllb-200-distilled-600M \
        --flores E:/workspaces/Cisox/build/flores/flores200_dataset --preset main14 \
        --out E:/models/translate-eval/nllb600m-pruned-main14-fp32
评测用的分词包装见 PrunedNllbTokenizer。
"""
import argparse
import json
import os
import sys
import unicodedata

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from vocab_coverage import parse_spm, read_varint, script_of  # noqa: E402

SPACE = "\u2581"
UN6 = ["zho_Hans", "eng_Latn", "fra_Latn", "spa_Latn", "rus_Cyrl", "arb_Arab"]
MAIN14 = UN6 + ["deu_Latn", "jpn_Jpan", "kor_Hang", "por_Latn", "ita_Latn", "tur_Latn", "vie_Latn", "ind_Latn"]
PRESETS = {"un6": UN6, "main14": MAIN14}
# 语言码 -> vocab_coverage.script_of 的文字类别
LANG_SCRIPTS = {
    "zho_Hans": {"CJK"}, "eng_Latn": {"Latin"}, "fra_Latn": {"Latin"}, "spa_Latn": {"Latin"},
    "rus_Cyrl": {"Cyrillic"}, "arb_Arab": {"Arabic"}, "deu_Latn": {"Latin"},
    "jpn_Jpan": {"CJK", "JP-kana"}, "kor_Hang": {"KR-hangul"}, "por_Latn": {"Latin"},
    "ita_Latn": {"Latin"}, "tur_Latn": {"Latin"}, "vie_Latn": {"Latin"}, "ind_Latn": {"Latin"},
}
SPECIAL = ["<s>", "<pad>", "</s>", "<unk>"]  # HF 侧 id 0~3
LANG_CODE_START = 256001  # HF 侧第一个语言码 id（256000 之前都是 spm 片段，偏移 +1）
LANG_CODE_END = 256203    # 不含（256203 是 <mask>）


def split_model_fields(buf):
    """把 sentencepiece .model（protobuf）拆成顶层字段列表，返回 [(字段号, 含键的原始字节)]。"""
    pos, out = 0, []
    while pos < len(buf):
        start = pos
        key, pos = read_varint(buf, pos)
        field, wire = key >> 3, key & 7
        if wire == 2:
            length, pos = read_varint(buf, pos)
            pos += length
        elif wire == 0:
            _, pos = read_varint(buf, pos)
        elif wire == 5:
            pos += 4
        elif wire == 1:
            pos += 8
        else:
            raise ValueError(f"未知 wire type {wire}")
        out.append((field, buf[start:pos]))
    return out


def write_pruned_spm(src_path, keep_spm_ids, dst_path):
    """写出只含保留片段的 spm 模型（其余字段与规范化器原样保留）。

    参数：src_path 原模型；keep_spm_ids 保留的 spm id 集合（须含 0~2）；dst_path 输出路径。
    """
    fields = split_model_fields(open(src_path, "rb").read())
    out, idx = bytearray(), 0
    for field, raw in fields:
        if field == 1:
            if idx in keep_spm_ids:
                out += raw
            idx += 1
        else:
            out += raw
    open(dst_path, "wb").write(bytes(out))


def encode_word_collect(word, scores, seen):
    """对一个以 ▁ 开头的词做 BPE，并把合并过程产生的全部片段（含中间片段）记入 seen。

    参数：word 词；scores 片段->分数；seen 输出集合。返回：最终片段列表。
    """
    syms = list(word)
    seen.update(s for s in syms if s in scores)
    while len(syms) > 1:
        best, best_i = None, -1
        for i in range(len(syms) - 1):
            sc = scores.get(syms[i] + syms[i + 1])
            if sc is not None and (best is None or sc > best):
                best, best_i = sc, i
        if best_i < 0:
            break
        merged = syms[best_i] + syms[best_i + 1]
        seen.add(merged)
        syms[best_i:best_i + 2] = [merged]
    return syms


def encode_collect(text, scores, seen):
    """整句分词（NFKC 近似 + 空白切词），返回片段列表并收集中间片段。"""
    out = []
    for w in unicodedata.normalize("NFKC", text).strip().split():
        out.extend(encode_word_collect(SPACE + w, scores, seen))
    return out


def read_split(flores, code, part):
    """读 FLORES 某语言某切分（dev/devtest）的全部句子，缺失返回空表。"""
    path = os.path.join(flores, part, f"{code}.{part}")
    if not os.path.exists(path):
        return []
    return [line.rstrip("\n") for line in open(path, encoding="utf-8")]


def is_keep_single(ch, scripts):
    """判断单字符是否属于需整体保留的类别：目标文字，或标点/数字/货币/数学符号。"""
    if script_of(ch) in scripts:
        return True
    cat = unicodedata.category(ch)
    return cat[0] in ("P", "N") or cat in ("Sc", "Sm")


def select_pieces(spm_path, flores, langs, parts=("dev",), extra_files=()):
    """按规则计算保留集合。

    参数：spm_path 原 spm；flores FLORES 目录；langs 语言码列表；parts 取词切分，默认只有 dev（devtest 仅对照实验用）；
    extra_files 额外文本文件（仅对照实验，如原版译文，用于构造"零损失"对照）。
    返回：(保留的 spm id 排序列表, 统计信息 dict)。
    """
    pieces = parse_spm(spm_path)
    scores = {t: s for t, s, typ in pieces if typ == 1}
    by_text = {t: i for i, (t, _, typ) in enumerate(pieces) if typ == 1}
    seen, n_sent = set(), 0
    for code in langs:
        for part in parts:  # 默认只用 dev，不碰 devtest
            for s in read_split(flores, code, part):
                encode_collect(s, scores, seen)
                n_sent += 1
    for path in extra_files:  # 仅对照实验：把额外文本（如原版译文）用到的片段也保留
        for line in open(path, encoding="utf-8"):
            encode_collect(line.rstrip("\n"), scores, seen)
    scripts = set().union(*(LANG_SCRIPTS[c] for c in langs))
    single = {t for t in scores if len(t.replace(SPACE, "")) == 1 and is_keep_single(t.replace(SPACE, ""), scripts)}
    single.add(SPACE)
    keep_text = (seen | single) & set(scores)
    keep = set(range(3)) | {by_text[t] for t in keep_text}
    stats = {"dev_sentences": n_sent, "from_dev_and_merges": len(seen & set(scores)),
             "single_char_extra": len(single - seen), "normal_pieces_kept": len(keep) - 3}
    return sorted(keep), stats


def build_maps(tok, keep_spm_ids, langs):
    """生成新旧 id 映射。新 id = 4 个特殊符号 + 保留片段(按原顺序) + 语言码(按原顺序)。

    参数：tok 原 NllbTokenizer；keep_spm_ids spm id 列表；langs 语言码。
    返回：(new_to_orig 列表, orig_to_new 全长列表(未保留->3))。
    """
    normal = [i + 1 for i in keep_spm_ids if i >= 3]  # spm id -> HF id（+1）
    lang_ids = [tok.convert_tokens_to_ids(c) for c in sorted(langs, key=lambda c: tok.convert_tokens_to_ids(c))]
    new_to_orig = [0, 1, 2, 3] + normal + lang_ids
    orig_to_new = [3] * 256206
    for new, orig in enumerate(new_to_orig):
        orig_to_new[orig] = new
    return new_to_orig, orig_to_new


def prune(nllb_dir, flores, langs, out_dir, parts=("dev",), extra_files=()):
    """执行裁剪并落盘，返回报告 dict。

    参数：nllb_dir 原 HF 目录；flores FLORES 目录；langs 语言码；out_dir 输出目录；parts 取词切分。
    """
    import torch
    from transformers import M2M100Config, M2M100ForConditionalGeneration, NllbTokenizer
    os.makedirs(out_dir, exist_ok=True)
    spm_path = os.path.join(nllb_dir, "sentencepiece.bpe.model")
    keep_spm_ids, stats = select_pieces(spm_path, flores, langs, parts, extra_files)
    tok = NllbTokenizer.from_pretrained(nllb_dir)
    new_to_orig, orig_to_new = build_maps(tok, keep_spm_ids, langs)
    v = len(new_to_orig)

    model = M2M100ForConditionalGeneration.from_pretrained(nllb_dir, torch_dtype=torch.float32)
    old = model.state_dict()
    rows = torch.tensor(new_to_orig)
    shared = old["model.shared.weight"].index_select(0, rows).clone()
    for k in ("model.encoder.embed_tokens.weight", "model.decoder.embed_tokens.weight", "lm_head.weight"):
        assert torch.equal(old[k], old["model.shared.weight"]), f"{k} 与 shared 不一致，不能共用裁剪行"
    cfg = M2M100Config.from_pretrained(nllb_dir)
    cfg.vocab_size = v  # 特殊符号 id 未变：bos0/pad1/eos2/unk3/decoder_start2 无需改
    small = M2M100ForConditionalGeneration(cfg)
    new_sd = {k: val for k, val in old.items()
              if k not in ("model.shared.weight", "model.encoder.embed_tokens.weight",
                           "model.decoder.embed_tokens.weight", "lm_head.weight")}
    for k in ("model.shared.weight", "model.encoder.embed_tokens.weight", "model.decoder.embed_tokens.weight", "lm_head.weight"):
        new_sd[k] = shared
    missing, unexpected = small.load_state_dict(new_sd, strict=False)
    small.tie_weights()
    small.save_pretrained(out_dir, safe_serialization=True)
    n_params = sum(p.numel() for p in {id(p): p for p in small.parameters()}.values())  # 绑定权重只计一次

    json.dump({"new_to_orig": new_to_orig, "orig_to_new": orig_to_new, "unk_new_id": 3},
              open(os.path.join(out_dir, "id_map.json"), "w"))
    pieces = parse_spm(spm_path)
    table = [{"new_id": n, "orig_id": o, "piece": tok.convert_ids_to_tokens(o)} for n, o in enumerate(new_to_orig)]
    json.dump(table, open(os.path.join(out_dir, "pieces.json"), "w", encoding="utf-8"), ensure_ascii=False)
    write_pruned_spm(spm_path, set(keep_spm_ids), os.path.join(out_dir, "sentencepiece.pruned.model"))
    json.dump({"langs": langs, "parts": list(parts), "v": v, **stats}, open(os.path.join(out_dir, "prune_info.json"), "w"), indent=1)
    size = os.path.getsize(os.path.join(out_dir, "model.safetensors"))
    return {"v": v, "params": n_params, "size_bytes": size, "missing": list(missing), "unexpected": list(unexpected), **stats}


class PrunedNllbTokenizer:
    """裁剪版 NLLB 的评测用分词包装（仅评测；部署需把片段表与 id 映射带进 worker）。

    mode="remap"：用原 NllbTokenizer 分词后把 id 映射到新 id，未保留片段落到 <unk>（最坏情形）；
    mode="resegment"：用裁剪后的 spm 重新分词，未保留片段自然拆成更短的保留片段。
    用法示例：
        t = PrunedNllbTokenizer(orig_dir, pruned_dir, mode="remap")
        batch = t.encode(["你好"], "zho_Hans")
        text = t.decode(out_ids)           # out_ids 为新 id
        forced = t.lang_id("eng_Latn")     # generate(forced_bos_token_id=forced)
    """

    def __init__(self, orig_dir, pruned_dir, mode="remap"):
        """参数：orig_dir 原模型目录；pruned_dir 裁剪输出目录；mode 见类说明。"""
        import numpy as np
        from transformers import NllbTokenizer
        self.mode = mode
        self.orig = NllbTokenizer.from_pretrained(orig_dir)
        m = json.load(open(os.path.join(pruned_dir, "id_map.json")))
        self.new_to_orig = np.array(m["new_to_orig"])
        self.orig_to_new = np.array(m["orig_to_new"])
        self.sp = None
        if mode == "resegment":
            import sentencepiece as spm
            self.sp = spm.SentencePieceProcessor(model_file=os.path.join(pruned_dir, "sentencepiece.pruned.model"))

    def lang_id(self, code):
        """语言码的新 id。"""
        return int(self.orig_to_new[self.orig.convert_tokens_to_ids(code)])

    def encode(self, texts, src_lang):
        """批量编码，返回含 input_ids/attention_mask 的 torch 张量字典（新 id，pad=1）。

        参数：texts 句子列表；src_lang 源语言码。
        """
        import torch
        if self.mode == "remap":
            self.orig.src_lang = src_lang
            b = self.orig(texts, return_tensors="pt", padding=True)
            ids = torch.from_numpy(self.orig_to_new[b["input_ids"].numpy()])
            return {"input_ids": ids, "attention_mask": b["attention_mask"]}
        rows = []
        for t in texts:
            s = self.sp.encode(t)
            rows.append([self.lang_id(src_lang)] + [3 if i == 0 else i + 1 for i in s] + [2])
        width = max(map(len, rows))
        ids = torch.full((len(rows), width), 1, dtype=torch.long)
        mask = torch.zeros((len(rows), width), dtype=torch.long)
        for r, row in enumerate(rows):
            ids[r, :len(row)] = torch.tensor(row)
            mask[r, :len(row)] = 1
        return {"input_ids": ids, "attention_mask": mask}

    def decode(self, new_ids):
        """把新 id 批量解码成字符串（先映射回原 id，再用原分词器）。

        参数：new_ids 形状 [batch, seq] 的张量或列表。
        """
        import numpy as np
        arr = self.new_to_orig[np.asarray(new_ids)]
        return self.orig.batch_decode(arr.tolist(), skip_special_tokens=True)


def main():
    """命令行入口。"""
    ap = argparse.ArgumentParser()
    ap.add_argument("--nllb", required=True)
    ap.add_argument("--flores", required=True)
    ap.add_argument("--preset", choices=sorted(PRESETS))
    ap.add_argument("--langs", help="逗号分隔语言码，覆盖 preset")
    ap.add_argument("--out", required=True)
    ap.add_argument("--parts", default="dev", help="取词用的 FLORES 切分，逗号分隔；含 devtest 仅用于对照实验")
    ap.add_argument("--extra-text", nargs="*", default=[], help="额外取词文本（仅对照实验）")
    args = ap.parse_args()
    langs = args.langs.split(",") if args.langs else PRESETS[args.preset]
    rep = prune(args.nllb, args.flores, langs, args.out, tuple(args.parts.split(",")), tuple(args.extra_text))
    print(json.dumps(rep, ensure_ascii=False, indent=1))


if __name__ == "__main__":
    main()
