"""NLLB 裁剪词表模型包生成：tokenizer.json、外部数据版 ONNX、model.json。

输入：
  --pruned   裁剪产物目录（含 pieces.json、prune_info.json）
  --orig-tok 原版 NLLB 的 tokenizer.json（只取归一化器与合并表）
  --onnx     int4 量化后的 encoder_model.onnx / decoder_model_merged.onnx 所在目录
  --out      输出的模型包目录
用法示例：
  python make_nllb_pack.py --pruned E:/models/translate-eval/nllb600m-pruned-main14-ccm-fp32 \
      --orig-tok E:/models/translate-eval/nllb-200-distilled-600M/tokenizer.json \
      --onnx E:/models/translate-eval/nllb600m-pruned-main14-ccm-int4 \
      --out E:/models/translate-eval/nllb600m-main14-ccm-int4-ext
"""
import argparse
import json
import os
import shutil

# FLORES 码 -> 应用语言码（与 snow-translate 的 Lang::from_code 一致；ko/vi/id 暂无枚举，仍随清单透传）
APP_CODES = {
    "zho_Hans": "zh-CN", "eng_Latn": "en", "fra_Latn": "fr", "spa_Latn": "es", "rus_Cyrl": "ru",
    "arb_Arab": "ar", "deu_Latn": "de", "jpn_Jpan": "ja", "kor_Hang": "ko", "por_Latn": "pt",
    "ita_Latn": "it", "tur_Latn": "tr", "vie_Latn": "vi", "ind_Latn": "id",
}
SPECIAL = ["<s>", "<pad>", "</s>", "<unk>"]


def normalizer(orig_norm):
    """在原版归一化序列末尾补首尾空白裁剪（对齐 sentencepiece 的 remove_extra_whitespaces）。"""
    seq = list(orig_norm["normalizers"])
    seq.append({"type": "Strip", "strip_left": True, "strip_right": True})
    return {"type": "Sequence", "normalizers": seq}


def build_tokenizer(pieces, orig):
    """由裁剪片段表与原版 tokenizer.json 生成裁剪版 BPE tokenizer.json（dict）。

    参数：pieces 为 pieces.json 内容；orig 为原版 tokenizer.json 解析结果。
    做法：词表取保留片段，合并表只保留"左、右、合并结果三者都在词表内"的项，顺序不变。
    """
    vocab = {p["piece"]: p["new_id"] for p in pieces}
    assert len(vocab) == len(pieces), "片段文本重复"
    merges = []
    for m in orig["model"]["merges"]:
        left, right = m.split(" ")
        if left in vocab and right in vocab and (left + right) in vocab:
            merges.append(m)
    return {
        "version": "1.0",
        "truncation": None,
        "padding": None,
        # 特殊符号与语言码只放进词表（不做 added_tokens），避免用户文本里的字面 "eng_Latn"/"</s>" 被识别成控制符
        "added_tokens": [],
        "normalizer": normalizer(orig["normalizer"]),
        "pre_tokenizer": orig["pre_tokenizer"],
        "post_processor": None,
        "decoder": orig["decoder"],
        "model": {
            "type": "BPE", "dropout": None, "unk_token": "<unk>", "continuing_subword_prefix": None,
            "end_of_word_suffix": None, "fuse_unk": True, "byte_fallback": False, "ignore_merges": False,
            "vocab": vocab, "merges": merges,
        },
    }


def externalize(src, dst, location):
    """把 ONNX 权重存成同目录外部数据文件（ORT 按需内存映射）。"""
    import onnx
    model = onnx.load(src)
    onnx.save_model(model, dst, save_as_external_data=True, all_tensors_to_one_file=True,
                    location=location, size_threshold=1024)


def build_manifest(pieces, info, display):
    """生成 model.json（dict）。"""
    tokens = {p["piece"]: p["new_id"] for p in pieces}
    langs = [c for c in info["langs"] if c in tokens]
    return {
        "schema_version": 1,
        "id": "nllb600m-main14-ccm-int4",
        "display_name": display,
        "family": "m2m100",
        "quantization": "int4",
        "files": {
            "encoder": "encoder.onnx", "encoder_data": "encoder.onnx_data",
            "decoder": "decoder.onnx", "decoder_data": "decoder.onnx_data",
            "tokenizer": "tokenizer.json",
        },
        "languages": [APP_CODES[c] for c in langs],
        "lang_tokens": {APP_CODES[c]: c for c in langs},
        "max_input_tokens": 512,
        "generation": {
            "decoder_start_token_id": 2, "eos_token_id": 2, "pad_token_id": 1,
            "bad_token_ids": [], "max_new_tokens": 256, "num_beams": 2,
            "length_penalty": 2.0, "min_length_ratio": 0.7, "no_repeat_ngram_size": 0,
        },
        "execution": {"intra_threads": 0, "cpu_arena": True, "mem_pattern": True,
                      "opt_level": 3, "prepacking": True},
    }


def main():
    """命令行入口。"""
    ap = argparse.ArgumentParser()
    ap.add_argument("--pruned", required=True)
    ap.add_argument("--orig-tok", required=True)
    ap.add_argument("--onnx", required=True)
    ap.add_argument("--out", required=True)
    ap.add_argument("--display", default="NLLB-200 distilled 600M (14 languages, int4)")
    ap.add_argument("--skip-onnx", action="store_true", help="只生成 tokenizer.json 与 model.json")
    a = ap.parse_args()
    os.makedirs(a.out, exist_ok=True)
    pieces = json.load(open(os.path.join(a.pruned, "pieces.json"), encoding="utf-8"))
    info = json.load(open(os.path.join(a.pruned, "prune_info.json"), encoding="utf-8"))
    orig = json.load(open(a.orig_tok, encoding="utf-8"))
    tok = build_tokenizer(pieces, orig)
    with open(os.path.join(a.out, "tokenizer.json"), "w", encoding="utf-8", newline="\n") as f:
        json.dump(tok, f, ensure_ascii=False)
    print("tokenizer: vocab", len(tok["model"]["vocab"]), "merges", len(tok["model"]["merges"]))
    man = build_manifest(pieces, info, a.display)
    with open(os.path.join(a.out, "model.json"), "w", encoding="utf-8", newline="\n") as f:
        json.dump(man, f, ensure_ascii=False, indent=2)
        f.write("\n")
    for name in ("config.json", "generation_config.json"):
        shutil.copy(os.path.join(a.onnx, name), os.path.join(a.out, name))
    if not a.skip_onnx:
        externalize(os.path.join(a.onnx, "encoder_model.onnx"), os.path.join(a.out, "encoder.onnx"), "encoder.onnx_data")
        externalize(os.path.join(a.onnx, "decoder_model_merged.onnx"), os.path.join(a.out, "decoder.onnx"), "decoder.onnx_data")
    for n in sorted(os.listdir(a.out)):
        print(n, os.path.getsize(os.path.join(a.out, n)))


if __name__ == "__main__":
    main()
