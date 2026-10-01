"""按给定的 spm 片段 id 排序文件裁剪 NLLB 词表（CCMatrix 词频版）。

保留集合 = 排序文件前 K 个 id ∪ 这些片段的 BPE 合并中间片段 ∪ 4 个特殊符号 ∪ 语言集的语言码 ∪ 目标文字的单字符片段。
排序文件里的 id 是原 sentencepiece 的片段编号（0 起，与 vocab_coverage.parse_spm 的下标一致），
HF 侧 id = spm id + 1（spm id >= 3），这一约定与 prune_nllb_vocab.build_maps 相同。
复用 prune_nllb_vocab 的 build_maps / write_pruned_spm / 单字符判定，不改动原脚本。

用法示例：
    python prune_nllb_by_ids.py --nllb E:/models/translate-eval/nllb-200-distilled-600M \
        --ranked E:/models/translate-eval/vocab-sets/un6_ccm_ranked.json --top 60000 \
        --out E:/models/translate-eval/nllb600m-pruned-un6-ccm-fp32
"""
import argparse
import json
import os
import sys

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from prune_nllb_vocab import (LANG_SCRIPTS, SPACE, build_maps, encode_word_collect, is_keep_single,  # noqa: E402
                              write_pruned_spm)
from vocab_coverage import parse_spm  # noqa: E402


def select_by_ids(spm_path, ranked_ids, top, langs):
    """由排序 id 计算保留的 spm id 集合。

    参数：spm_path 原 spm；ranked_ids 按频率降序的 spm id；top 取前多少个；langs 语言码列表。
    返回：(保留 spm id 排序列表, 统计 dict)。
    """
    pieces = parse_spm(spm_path)
    scores = {t: s for t, s, typ in pieces if typ == 1}
    by_text = {t: i for i, (t, _, typ) in enumerate(pieces) if typ == 1}
    top_ids = [i for i in ranked_ids[:top] if pieces[i][2] == 1]  # 只保留普通片段，特殊符号单独处理
    seen = set()
    for i in top_ids:  # 每个片段单独当作一个"词"做 BPE，收集其合并链上的中间片段
        encode_word_collect(pieces[i][0], scores, seen)
    scripts = set().union(*(LANG_SCRIPTS[c] for c in langs))
    single = {t for t in scores if len(t.replace(SPACE, "")) == 1 and is_keep_single(t.replace(SPACE, ""), scripts)}
    single.add(SPACE)
    top_text = {pieces[i][0] for i in top_ids}
    keep_text = (top_text | seen | single) & set(scores)
    keep = set(range(3)) | {by_text[t] for t in keep_text}
    stats = {"top": top, "top_normal": len(top_ids), "merge_intermediate_extra": len((seen & set(scores)) - top_text),
             "single_char_extra": len(single - top_text - seen), "normal_pieces_kept": len(keep) - 3}
    return sorted(keep), stats


def prune_by_ids(nllb_dir, ranked_path, top, out_dir):
    """执行裁剪并落盘（结构与 prune_nllb_vocab.prune 的输出一致，可被 PrunedNllbTokenizer 直接使用）。

    参数：nllb_dir 原 HF 目录；ranked_path 排序 json；top 取前 K；out_dir 输出目录。返回：报告 dict。
    """
    import torch
    from transformers import M2M100Config, M2M100ForConditionalGeneration, NllbTokenizer
    meta = json.load(open(ranked_path))
    langs = meta["langs"]
    os.makedirs(out_dir, exist_ok=True)
    spm_path = os.path.join(nllb_dir, "sentencepiece.bpe.model")
    keep_spm_ids, stats = select_by_ids(spm_path, meta["ranked_ids"], top, langs)
    tok = NllbTokenizer.from_pretrained(nllb_dir)
    new_to_orig, orig_to_new = build_maps(tok, keep_spm_ids, langs)
    v = len(new_to_orig)
    # 自检：前几个高频 id 的片段文本应与 HF 侧 token 一致
    pieces = parse_spm(spm_path)
    for i in meta["ranked_ids"][:50]:
        if pieces[i][2] == 1:
            assert tok.convert_ids_to_tokens(i + 1) == pieces[i][0], (i, pieces[i][0])
    model = M2M100ForConditionalGeneration.from_pretrained(nllb_dir, torch_dtype=torch.float32)
    old = model.state_dict()
    shared = old["model.shared.weight"].index_select(0, torch.tensor(new_to_orig)).clone()
    tied = ("model.shared.weight", "model.encoder.embed_tokens.weight", "model.decoder.embed_tokens.weight", "lm_head.weight")
    cfg = M2M100Config.from_pretrained(nllb_dir)
    cfg.vocab_size = v
    small = M2M100ForConditionalGeneration(cfg)
    new_sd = {k: val for k, val in old.items() if k not in tied}
    for k in tied:
        new_sd[k] = shared
    missing, unexpected = small.load_state_dict(new_sd, strict=False)
    small.tie_weights()
    small.save_pretrained(out_dir, safe_serialization=True)
    n_params = sum(p.numel() for p in {id(p): p for p in small.parameters()}.values())
    json.dump({"new_to_orig": new_to_orig, "orig_to_new": orig_to_new, "unk_new_id": 3},
              open(os.path.join(out_dir, "id_map.json"), "w"))
    table = [{"new_id": n, "orig_id": o, "piece": tok.convert_ids_to_tokens(o)} for n, o in enumerate(new_to_orig)]
    json.dump(table, open(os.path.join(out_dir, "pieces.json"), "w", encoding="utf-8"), ensure_ascii=False)
    write_pruned_spm(spm_path, set(keep_spm_ids), os.path.join(out_dir, "sentencepiece.pruned.model"))
    info = {"langs": langs, "source": meta.get("source"), "v": v, **stats}
    json.dump(info, open(os.path.join(out_dir, "prune_info.json"), "w"), indent=1)
    return {"v": v, "params": n_params, "missing": list(missing), "unexpected": list(unexpected), **stats}


def main():
    """命令行入口。"""
    ap = argparse.ArgumentParser()
    ap.add_argument("--nllb", required=True)
    ap.add_argument("--ranked", required=True)
    ap.add_argument("--top", type=int, required=True)
    ap.add_argument("--out", required=True)
    a = ap.parse_args()
    print(json.dumps(prune_by_ids(a.nllb, a.ranked, a.top, a.out), ensure_ascii=False, indent=1))


if __name__ == "__main__":
    main()
