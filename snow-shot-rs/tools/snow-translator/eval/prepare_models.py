"""准备 Xenova opus-mt 的 int8 ONNX 模型：生成 aria2 输入、校验哈希、写 model.json。

仅标准库。用法（在 eval 目录）：
    python prepare_models.py aria-input     # 生成 aria2 输入文件（下载自己用 aria2c 做）
    aria2c -i <输出文件> -j4 -x16 -s16 -k8M --continue=true --max-tries=0 --retry-wait=3
    python prepare_models.py finalize       # 校验 sha256 并写各目录的 model.json
"""
from __future__ import annotations

import hashlib
import json
import os
import sys
import urllib.request

MODELS_ROOT = os.environ.get("SNOW_MODELS_ROOT", "E:/models/translate")
HF = "https://huggingface.co"
# 十个方向：(源语言代码, 目标语言代码)，对应 opus-mt-<src>-<tgt> 的 HF 命名
DIRECTIONS = ["zh-en", "en-zh", "en-fr", "fr-en", "en-es", "es-en", "en-ru", "ru-en", "en-ar", "ar-en"]
# 清单语言代码（HF 的 zh 在 worker 里叫 zh-CN）
LANG_CODE = {"zh": "zh-CN"}
# 目标端需要 `>>xxx<<` 语言 token 的方向（opus-mt 多目标模型）：方向 -> (清单语言码, token)
LANG_TOKEN = {"en-zh": ("zh-CN", ">>cmn_Hans<<"), "en-ar": ("ar", ">>ara<<")}
ENCODER = "onnx/encoder_model_int8.onnx"
DECODER = "onnx/decoder_model_merged_int8.onnx"
SMALL = ["tokenizer.json", "config.json", "generation_config.json"]
CACHE_DIR = os.environ.get("SNOW_EVAL_CACHE", "E:/workspaces/Cisox/build/flores")


def lang(code: str) -> str:
    """HF 语言码转清单语言码。参数：code；返回：清单码。"""
    return LANG_CODE.get(code, code)


def model_dir(d: str) -> str:
    """方向对应的本地模型目录。参数：d 如 zh-en；返回：目录路径。"""
    return f"{MODELS_ROOT}/opus-mt-{d}-onnx-int8"


def hf_api(repo: str) -> dict:
    """取 HF 模型元数据（含 LFS sha256）。参数：repo 仓库 id；返回：JSON。"""
    with urllib.request.urlopen(f"{HF}/api/models/{repo}?blobs=true") as r:
        return json.load(r)


def sha256_file(path: str) -> str:
    """流式计算文件 sha256。参数：path；返回：十六进制摘要。"""
    h = hashlib.sha256()
    with open(path, "rb") as f:
        for blk in iter(lambda: f.read(1 << 20), b""):
            h.update(blk)
    return h.hexdigest()


def aria_input() -> None:
    """生成 aria2 输入文件到缓存目录。"""
    lines = []
    for d in DIRECTIONS:
        for f in [ENCODER, DECODER] + SMALL:
            lines.append(f"{HF}/Xenova/opus-mt-{d}/resolve/main/{f}\n  dir={model_dir(d)}/{os.path.dirname(f)}\n"
                         f"  out={os.path.basename(f)}\n")
    out = f"{CACHE_DIR}/aria-in.txt"
    with open(out, "w", encoding="utf-8") as fh:
        fh.write("".join(lines))
    print("written", out)


def finalize() -> None:
    """逐方向校验 sha256 并写 model.json；有不一致则非零退出。"""
    bad = 0
    report = {}
    for d in DIRECTIONS:
        src, tgt = d.split("-")
        api = hf_api(f"Xenova/opus-mt-{d}")
        lfs = {s["rfilename"]: (s.get("lfs") or {}).get("sha256") for s in api["siblings"]}
        base = model_dir(d)
        digests = {}
        for key, rel in [("encoder", ENCODER), ("decoder", DECODER), ("tokenizer", "tokenizer.json")]:
            actual = sha256_file(f"{base}/{rel}")
            want = lfs.get(rel)
            ok = want is None or want == actual
            bad += 0 if ok else 1
            digests[key] = actual
            report[f"{d}/{rel}"] = {"sha256": actual, "hf_lfs_sha256": want, "match": ok}
            print(d, rel, "OK" if ok else "MISMATCH", "(no LFS meta)" if want is None else "")
        manifest = {
            "schema_version": 1,
            "id": f"opus-mt-{d}-onnx-int8",
            "display_name": f"OPUS-MT {src}->{tgt} (Xenova ONNX int8)",
            "family": "marian",
            "quantization": "int8",
            "files": {"encoder": ENCODER, "decoder": DECODER, "tokenizer": "tokenizer.json"},
            "sha256": digests,
            "languages": [lang(src), lang(tgt)],
            "pairs": [[lang(src), lang(tgt)]],
        }
        if d in LANG_TOKEN:
            code, token = LANG_TOKEN[d]
            manifest["lang_tokens"] = {code: token}
            manifest["source_prefix"] = "{tgt_token} "
        with open(f"{base}/model.json", "w", encoding="utf-8", newline="\n") as fh:
            json.dump(manifest, fh, indent=2)
            fh.write("\n")
    with open(f"{CACHE_DIR}/hashes.json", "w", encoding="utf-8") as fh:
        json.dump(report, fh, indent=2)
    sys.exit(1 if bad else 0)


if __name__ == "__main__":
    {"aria-input": aria_input, "finalize": finalize}[sys.argv[1]]()
