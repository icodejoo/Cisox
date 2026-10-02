"""OPUS-MT 单语向（英→中 / 中→英）模型包生成：tokenizer.json、ONNX、model.json、许可与署名文件、manifest。

输入：
  --hf-dir     Helsinki-NLP/opus-mt-<方向> 的 HF 目录（source.spm、target.spm、vocab.json、README.md、metadata.json、配置）
  --onnx       optimum 导出（可已量化）的目录，含 encoder_model.onnx 与 decoder_model_merged.onnx
  --direction  en-zh 或 zh-en
  --quant      量化档标签（fp32 / int8 / int4），只用于命名与 model.json
  --license    许可全文文件（须与 --hf-dir 的 README 许可一致：Apache-2.0 或 CC-BY-4.0）
  --out        输出模型包目录
输出：model.json、encoder.onnx、decoder.onnx、tokenizer.json、config.json、generation_config.json、
      LICENSE-<许可>.txt、NOTICE.txt、<包名>.manifest.json（文件名、体积、SHA256）。
用法示例：
  python make_opusmt_pack.py --hf-dir E:/models/translate-eval/_hf-opus-mt-en-zh \
      --onnx E:/models/translate-eval/opusmt-en-zh-int8 --direction en-zh --quant int8 \
      --license E:/workspaces/Cisox/licenses/Apache-2.0.txt \
      --out E:/models/translate-eval/opusmt-en-zh-int8-pack
依赖：sentencepiece（只在生成时用，不进产品）。
"""
import argparse
import base64
import hashlib
import json
import os
import re
import shutil

# 方向 -> 展示名、源/目标应用语言码、目标语言 token（应用语言码 -> 词表 token）、源文本前缀模板
DIRECTIONS = {
    "en-zh": {"name": "English to Chinese", "src": "en", "tgt": "zh-CN", "lang_tokens": {"zh-CN": ">>cmn_Hans<<"},
              "source_prefix": "{tgt_token} "},
    "zh-en": {"name": "Chinese to English", "src": "zh-CN", "tgt": "en", "lang_tokens": {}, "source_prefix": ""},
}
UPSTREAM = {"en-zh": "https://huggingface.co/Helsinki-NLP/opus-mt-en-zh",
            "zh-en": "https://huggingface.co/Helsinki-NLP/opus-mt-zh-en"}
SPDX_BY_CARD = {"apache-2.0": "Apache-2.0", "cc-by-4.0": "CC-BY-4.0"}  # HF 卡片 license 字段 -> SPDX
LOW_SCORE = -100.0  # 只在目标端 spm 出现的片段与语言标记的分数：极低，编码时不会被切词选中，解码照常还原
CHUNK = 1 << 20          # 计算 SHA256 的读块大小


def sha256_of(path):
    """流式计算文件 SHA256（小写十六进制）。"""
    h = hashlib.sha256()
    with open(path, "rb") as f:
        while chunk := f.read(CHUNK):
            h.update(chunk)
    return h.hexdigest()


def build_tokenizer(hf_dir):
    """由 source.spm、target.spm 与联合词表 vocab.json 生成 tokenizer.json（dict）。

    参数：hf_dir 为 HF 模型目录。
    做法：Unigram 词表按 vocab.json 的 id 排列（与模型嵌入行一一对应）；分数取源端 spm，源端没有的片段（目标端
    专有片段、语言标记）给极低分，使编码结果与 HF MarianTokenizer（只用源端 spm）一致，解码只查 id 不受影响；
    归一化器内嵌 spm 自带的 precompiled_charsmap；先按空白切词再加 Metaspace。
    """
    import sentencepiece as spm
    from sentencepiece import sentencepiece_model_pb2 as pb
    with open(os.path.join(hf_dir, "vocab.json"), encoding="utf-8") as f:
        vocab = json.load(f)
    size = max(vocab.values()) + 1
    assert len(set(vocab.values())) == len(vocab) == size, "vocab.json 的 id 不连续或重复"
    src = spm.SentencePieceProcessor(model_file=os.path.join(hf_dir, "source.spm"))
    inv = {i: p for p, i in vocab.items()}
    entries = []
    for i in range(size):
        p = inv[i]
        if p in ("</s>", "<unk>", "<pad>"):
            score = 0.0
        elif src.piece_to_id(p) != src.unk_id():
            score = src.get_score(src.piece_to_id(p))
        else:  # 只在目标端出现的片段与语言标记：解码要用，编码时尽量别用
            score = LOW_SCORE
        entries.append([p, score])
    proto = pb.ModelProto()
    with open(os.path.join(hf_dir, "source.spm"), "rb") as f:
        proto.ParseFromString(f.read())
    charsmap = base64.b64encode(proto.normalizer_spec.precompiled_charsmap).decode("ascii")
    added = [{"id": vocab[t], "content": t, "single_word": False, "lstrip": False, "rstrip": False,
              "normalized": False, "special": True} for t in ("</s>", "<unk>", "<pad>")]
    eos = {"Sequence": {"id": "A", "type_id": 0}}, {"SpecialToken": {"id": "</s>", "type_id": 0}}
    return {
        "version": "1.0", "truncation": None, "padding": None, "added_tokens": added,
        "normalizer": {"type": "Precompiled", "precompiled_charsmap": charsmap},
        "pre_tokenizer": {"type": "Sequence", "pretokenizers": [
            {"type": "WhitespaceSplit"},
            {"type": "Metaspace", "replacement": "▁", "prepend_scheme": "always", "split": True}]},
        "post_processor": {"type": "TemplateProcessing", "single": list(eos),
                           "pair": list(eos) + [{"Sequence": {"id": "B", "type_id": 0}},
                                                {"SpecialToken": {"id": "</s>", "type_id": 0}}],
                           "special_tokens": {"</s>": {"id": "</s>", "ids": [vocab["</s>"]], "tokens": ["</s>"]}}},
        "decoder": {"type": "Metaspace", "replacement": "▁", "prepend_scheme": "always", "split": True},
        "model": {"type": "Unigram", "unk_id": vocab["<unk>"], "vocab": entries, "byte_fallback": False},
    }


def license_of(readme):
    """从 HF README 头部元数据读 `license:` 字段，返回 SPDX 标识；未知许可直接退出，避免误署。"""
    m = re.search(r"^license:\s*(\S+)\s*$", readme, re.M)
    if not m:
        raise SystemExit("README 头部没有 license 字段，无法确定许可")
    spdx = SPDX_BY_CARD.get(m.group(1).lower())
    if not spdx:
        raise SystemExit(f"未知许可 {m.group(1)}，请先核对并补充")
    return spdx


def build_notice(direction, spdx, quant, meta, quant_note=""):
    """生成 NOTICE.txt 文本：署名、修改说明、无隶属声明、非法律意见。"""
    name = DIRECTIONS[direction]["name"]
    terms = {
        "Apache-2.0": "License: Apache License 2.0 (see LICENSE-Apache-2.0.txt). This notice keeps the "
                      "attribution and states the changes made.",
        "CC-BY-4.0": "License: Creative Commons Attribution 4.0 International, CC BY 4.0 (see LICENSE-CC-BY-4.0.txt). "
                     "CC BY 4.0 requires attribution (creator, copyright notice, license link, indication of "
                     "changes, link to the material), which this file provides.",
    }[spdx]
    quant_line = "none (fp32 weights)" if quant == "fp32" else quant
    if quant_note:
        quant_line += f" ({quant_note})"
    return f"""NOTICE

This package contains a machine translation model ({name}) derived from
Helsinki-NLP/opus-mt-{direction}.

Original work
- Model: opus-mt-{direction} (Tatoeba-Challenge model {meta.get('long_pair', '')}, trained {meta.get('train_date', '')})
- Creators: Language Technology Research Group at the University of Helsinki (Helsinki-NLP), OPUS-MT project
- Source: {UPSTREAM[direction]}
- Citation: Joerg Tiedemann and Santhosh Thottingal, "OPUS-MT - Building open translation services for the World",
  Proceedings of the 22nd Annual Conference of the European Association for Machine Translation (EAMT), Lisbon, 2020.
- {terms}

Modifications made in this package
- Exported the original PyTorch weights to ONNX (encoder and merged decoder with key/value cache).
- Quantization: {quant_line}.
- Rebuilt tokenizer.json from the original source.spm, target.spm and vocab.json so that a non-Python runtime can use it.
- Added model.json (runtime manifest) and file checksums. No retraining or fine-tuning was done.
Outputs of the modified weights can differ from those of the original model.

Disclaimers
- This package is not affiliated with, endorsed by, or sponsored by the University of Helsinki, Helsinki-NLP or the
  OPUS-MT authors.
- The model is provided "as is", without warranty of any kind. Translations may be wrong or biased.
- This notice is not legal advice.
"""


def main():
    """命令行入口。"""
    ap = argparse.ArgumentParser()
    ap.add_argument("--hf-dir", required=True)
    ap.add_argument("--onnx", required=True)
    ap.add_argument("--direction", required=True, choices=sorted(DIRECTIONS))
    ap.add_argument("--quant", required=True)
    ap.add_argument("--license", required=True, help="与 README 许可对应的许可全文文件")
    ap.add_argument("--out", required=True)
    ap.add_argument("--version", default="1")
    ap.add_argument("--quant-note", default="", help="量化细节说明，如 MatMulNBits symmetric RTN, block size 16")
    ap.add_argument("--num-beams", type=int, default=4)
    ap.add_argument("--no-repeat-ngram", type=int, default=0)
    ap.add_argument("--external-data", action="store_true", help="权重存成 .onnx_data 外部数据文件（ORT 内存映射）")
    a = ap.parse_args()
    d = DIRECTIONS[a.direction]
    os.makedirs(a.out, exist_ok=True)
    with open(os.path.join(a.hf_dir, "README.md"), encoding="utf-8") as f:
        spdx = license_of(f.read())
    with open(os.path.join(a.hf_dir, "metadata.json"), encoding="utf-8") as f:
        meta = json.load(f)
    pack_id = f"opusmt-{a.direction}-{a.quant}"

    tok = build_tokenizer(a.hf_dir)
    with open(os.path.join(a.out, "tokenizer.json"), "w", encoding="utf-8", newline="\n") as f:
        json.dump(tok, f, ensure_ascii=False)
    for n in ("config.json", "generation_config.json"):
        shutil.copy(os.path.join(a.hf_dir, n), os.path.join(a.out, n))
    files = {"encoder": "encoder.onnx", "decoder": "decoder.onnx", "tokenizer": "tokenizer.json"}
    for key, src in (("encoder", "encoder_model.onnx"), ("decoder", "decoder_model_merged.onnx")):
        dst = os.path.join(a.out, files[key])
        if a.external_data:
            import onnx
            loc = files[key] + "_data"
            files[key + "_data"] = loc
            onnx.save_model(onnx.load(os.path.join(a.onnx, src)), dst, save_as_external_data=True,
                            all_tensors_to_one_file=True, location=loc, size_threshold=1024)
        else:
            shutil.copy(os.path.join(a.onnx, src), dst)
    lic_name = f"LICENSE-{spdx}.txt"
    shutil.copy(a.license, os.path.join(a.out, lic_name))
    with open(os.path.join(a.out, "NOTICE.txt"), "w", encoding="utf-8", newline="\n") as f:
        f.write(build_notice(a.direction, spdx, a.quant, meta, a.quant_note))

    man = {
        "schema_version": 1, "id": pack_id,
        "display_name": f"OPUS-MT {d['name']} ({a.quant})",
        "family": "marian", "quantization": a.quant, "quantization_detail": a.quant_note, "files": files,
        "sha256": {k: sha256_of(os.path.join(a.out, v)) for k, v in files.items()},
        "languages": [d["src"], d["tgt"]], "pairs": [[d["src"], d["tgt"]]],
        "lang_tokens": d["lang_tokens"], "source_prefix": d["source_prefix"],
        "max_input_tokens": 512,
        "generation": {"decoder_start_token_id": 65000, "eos_token_id": 0, "pad_token_id": 65000,
                       "bad_token_ids": [65000], "max_new_tokens": 256, "num_beams": a.num_beams,
                       "length_penalty": 1.0, "no_repeat_ngram_size": a.no_repeat_ngram},
        "execution": {"intra_threads": 0, "cpu_arena": True, "mem_pattern": True, "opt_level": 3, "prepacking": True},
    }
    with open(os.path.join(a.out, "model.json"), "w", encoding="utf-8", newline="\n") as f:
        json.dump(man, f, ensure_ascii=False, indent=2)
        f.write("\n")

    # 发布用 manifest：与 OCR 资产清单同风格（schema + files[name,size,sha256]）
    inventory = []
    for n in sorted(os.listdir(a.out)):
        if n.endswith(".manifest.json"):
            continue
        p = os.path.join(a.out, n)
        inventory.append({"name": n, "size": os.path.getsize(p), "sha256": sha256_of(p)})
    manifest = {"schema": 1, "kind": "translation-model", "id": pack_id, "version": a.version, "family": "marian",
                "pairs": man["pairs"], "quantization": a.quant, "license": spdx, "upstream": UPSTREAM[a.direction],
                "total_size": sum(e["size"] for e in inventory), "files": inventory}
    with open(os.path.join(a.out, f"{pack_id}.manifest.json"), "w", encoding="utf-8", newline="\n") as f:
        json.dump(manifest, f, ensure_ascii=False, indent=2)
        f.write("\n")
    for e in inventory:
        print(f"{e['name']:32s}{e['size']:>12d}  {e['sha256']}")
    print("total", manifest["total_size"])


if __name__ == "__main__":
    main()
