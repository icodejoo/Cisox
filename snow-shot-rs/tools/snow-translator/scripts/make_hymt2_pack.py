"""Hy-MT2-1.8B 可选翻译包生成：由外部数据版 int4 ONNX 产出可发布的目录。

输入：
  --src      量化并转成外部数据的目录（含 model.onnx、model.onnx_data、tokenizer.json），
             例如 E:/models/translate-eval/hymt2-1.8b-int4-ext
  --hf-dir   原版 tencent/Hy-MT2-1.8B 的 HF 目录（取 LICENSE.txt 与 README.md 核对许可）
  --out      输出包目录
输出：model.json、model.onnx、model.onnx_data、tokenizer.json、LICENSE-Apache-2.0.txt、NOTICE.txt、
      <包名>.manifest.json（文件名、体积、SHA256，与其他翻译包同风格）。
用法示例：
  python make_hymt2_pack.py --src E:/models/translate-eval/hymt2-1.8b-int4-ext \
      --hf-dir E:/models/translate-eval/hy-mt2-1.8b --out E:/models/translate-eval/hymt2-1.8b-int4-pack
提示词模板与 eval/hymt/hymt_common.py、ort_gen.py 保持一致（worker 不写死模板，全部读 model.json 的 prompt）。
只用标准库，不进产品。
"""
import argparse
import hashlib
import json
import os
import re
import shutil

PACK_ID = "hymt2-1.8b-int4"
UPSTREAM = "https://huggingface.co/tencent/Hy-MT2-1.8B"
PAPER = "https://arxiv.org/abs/2605.22064"
CHUNK = 1 << 20  # 计算 SHA256 的读块大小
EOS_ID = 120020  # config.json 的 eos_token_id
MAX_NEW_TOKENS = 512
REPETITION_PENALTY = 1.05  # 评测所用值，同官方 generation_config
CHAT_PREFIX = "<｜hy_begin▁of▁sentence｜><｜hy_User｜>"  # chat_template.jinja：无 system 时的开头
CHAT_SUFFIX = "<｜hy_Assistant｜>"  # add_generation_prompt=True 的结尾
PROMPT_TEMPLATE = ("Translate the following text into {target_lang}. Note that you should only output the "
                   "translated result without any additional explanation:\n\n{source_text}")
# 应用语言码 -> 提示词里的语言名（模型卡 Supported Languages 的英文名）
LANG_NAMES = {"zh-CN": "Chinese", "zh-TW": "Traditional Chinese", "en": "English", "ja": "Japanese",
              "ko": "Korean", "fr": "French", "de": "German", "es": "Spanish", "ru": "Russian",
              "it": "Italian", "pt": "Portuguese", "tr": "Turkish", "ar": "Arabic"}
# 声明的有向语言对：中、英、日全排列（中日互译未单独评测，见文档）+ FLORES 前 30 句评测过的其余语向
CORE_LANGS = ["zh-CN", "en", "ja"]
EVALUATED_EXTRA = [("en", "fr"), ("fr", "en"), ("ru", "en"), ("ar", "en"), ("en", "es"), ("zh-CN", "fr"),
                   ("ru", "es"), ("ar", "fr"), ("fr", "zh-CN")]
# 评测机（Windows，4 线程）实测的量级，只作界面提示
HINTS = {"disk_mib": 1306, "load_working_set_mib": 953, "peak_working_set_mib": 1261,
         "seconds_per_sentence": "10+", "note": "外部数据内存映射；每句约 10 秒以上，适合短文本与流式"}


def sha256_of(path):
    """流式计算文件 SHA256（小写十六进制）。"""
    h = hashlib.sha256()
    with open(path, "rb") as f:
        while chunk := f.read(CHUNK):
            h.update(chunk)
    return h.hexdigest()


def check_license(hf_dir):
    """核对 HF 模型卡的 license 字段与 LICENSE.txt 的声明都是 Apache-2.0，返回 LICENSE.txt 路径；不符直接退出。"""
    with open(os.path.join(hf_dir, "README.md"), encoding="utf-8") as f:
        m = re.search(r"^license:\s*(\S+)\s*$", f.read(), re.M)
    if not m or m.group(1).lower() != "apache-2.0":
        raise SystemExit(f"模型卡 license 字段不是 apache-2.0（{m.group(1) if m else '缺失'}），请先核对")
    path = os.path.join(hf_dir, "LICENSE.txt")
    with open(path, encoding="utf-8") as f:
        head = f.read(2048)
    if "Apache License, Version 2.0" not in head:
        raise SystemExit("LICENSE.txt 头部没有 Apache License, Version 2.0 声明，请先核对")
    return path


def build_notice():
    """生成 NOTICE.txt：来源、许可、修改说明、无隶属声明、非法律意见。"""
    return f"""NOTICE

This package contains a machine translation model derived from Tencent Hy-MT2-1.8B.

Original work
- Model: Hy-MT2-1.8B (Tencent Hunyuan translation model family)
- Copyright (C) 2026 Tencent. All rights reserved.
- Source: {UPSTREAM}
- Report: {PAPER}
- License: Apache License, Version 2.0 (see LICENSE-Apache-2.0.txt, the upstream LICENSE.txt). The license
  text, this attribution and the statement of changes below are kept as required by the license.

Modifications made in this package
- Converted the original weights to ONNX (single graph with key/value cache inputs and outputs).
- Quantized: MatMulNBits 4-bit (symmetric, block size 32, round-to-nearest) for all matrix multiplications
  including lm_head; token embeddings stored as int8 (Gather).
- Stored the weights as an external data file (model.onnx_data) so that runtimes can memory-map it.
- Added model.json (runtime manifest, prompt template, file checksums). No retraining or fine-tuning was done.
Outputs of the modified weights can differ from those of the original model.

Disclaimers
- This package is not affiliated with, endorsed by, or sponsored by Tencent.
- The model is provided "as is", without warranty of any kind. Translations may be wrong or biased.
- This notice is not legal advice.
"""


def build_manifest(files, out):
    """生成 model.json（dict）：族、语言对、提示词、生成参数、校验和与提示信息。"""
    pairs = [[s, t] for s in CORE_LANGS for t in CORE_LANGS if s != t] + [list(p) for p in EVALUATED_EXTRA]
    langs = sorted({x for p in pairs for x in p}, key=lambda c: (c not in CORE_LANGS, c))
    return {
        "schema_version": 1, "id": PACK_ID, "display_name": "Hy-MT2 1.8B (int4, optional high-quality pack)",
        "family": "hunyuan_chat", "quantization": "int4",
        "quantization_detail": "MatMulNBits symmetric RTN block 32, embedding int8",
        "license": "Apache-2.0", "default_eligible": False, "files": files,
        "sha256": {k: sha256_of(os.path.join(out, v)) for k, v in files.items()},
        "languages": langs, "pairs": pairs, "max_input_tokens": 512,
        "prompt": {"prefix": CHAT_PREFIX, "suffix": CHAT_SUFFIX, "template": PROMPT_TEMPLATE,
                   "lang_names": {c: LANG_NAMES[c] for c in langs}},
        "generation": {"eos_token_id": EOS_ID, "max_new_tokens": MAX_NEW_TOKENS,
                       "repetition_penalty": REPETITION_PENALTY},
        "execution": {"intra_threads": 4, "cpu_arena": True, "mem_pattern": True, "opt_level": 3,
                      "prepacking": True},
        "hints": HINTS,
    }


def write_json(path, obj):
    """以 UTF-8、LF、缩进 2 写 JSON 并补末尾换行。"""
    with open(path, "w", encoding="utf-8", newline="\n") as f:
        json.dump(obj, f, ensure_ascii=False, indent=2)
        f.write("\n")


def main():
    """命令行入口。"""
    ap = argparse.ArgumentParser()
    ap.add_argument("--src", required=True)
    ap.add_argument("--hf-dir", required=True)
    ap.add_argument("--out", required=True)
    ap.add_argument("--version", default="1")
    a = ap.parse_args()
    lic = check_license(a.hf_dir)
    os.makedirs(a.out, exist_ok=True)
    files = {"model": "model.onnx", "model_data": "model.onnx_data", "tokenizer": "tokenizer.json"}
    for name in files.values():
        shutil.copyfile(os.path.join(a.src, name), os.path.join(a.out, name))
    shutil.copyfile(lic, os.path.join(a.out, "LICENSE-Apache-2.0.txt"))
    with open(os.path.join(a.out, "NOTICE.txt"), "w", encoding="utf-8", newline="\n") as f:
        f.write(build_notice())
    man = build_manifest(files, a.out)
    write_json(os.path.join(a.out, "model.json"), man)

    # 发布用 manifest：与其他翻译包同风格（schema + files[name,size,sha256]）
    inventory = []
    for n in sorted(os.listdir(a.out)):
        if n.endswith(".manifest.json"):
            continue
        p = os.path.join(a.out, n)
        inventory.append({"name": n, "size": os.path.getsize(p), "sha256": sha256_of(p)})
    write_json(os.path.join(a.out, f"{PACK_ID}.manifest.json"),
               {"schema": 1, "kind": "translation-model", "id": PACK_ID, "version": a.version,
                "family": "hunyuan_chat", "pairs": man["pairs"], "quantization": "int4", "license": "Apache-2.0",
                "upstream": UPSTREAM, "total_size": sum(e["size"] for e in inventory), "files": inventory})
    for e in inventory:
        print(f"{e['name']:32s}{e['size']:>12d}  {e['sha256']}")
    print("total", sum(e["size"] for e in inventory))


if __name__ == "__main__":
    main()
