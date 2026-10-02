"""Hy-MT2 评测的公共部分：提示词、语言名、取句、结果落盘与断点续跑（与 NLLB 评测同口径）。

取句沿用 eval_quant.read_flores（FLORES-200 devtest 前 30 句）与 CORE11 语向，译文目录布局沿用
results/<版本>/<src>-<tgt>/{src,ref,hyp}.txt。
"""
import json
import os
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, os.path.dirname(HERE))  # eval/
from eval_quant import CORE11, read_flores  # noqa: E402

RESULTS = "E:/workspaces/Cisox/materials/translate/results"
MODEL_DIR = "E:/models/translate-eval/hy-mt2-1.8b"
PROMPT = ("Translate the following text into {target_lang}. Note that you should only output the translated "
          "result without any additional explanation:\n\n{source_text}")
LANG_NAME = {"zho_Hans": "Chinese", "eng_Latn": "English", "fra_Latn": "French", "spa_Latn": "Spanish",
             "rus_Cyrl": "Russian", "arb_Arab": "Arabic", "deu_Latn": "German", "jpn_Jpan": "Japanese",
             "kor_Hang": "Korean", "por_Latn": "Portuguese", "ita_Latn": "Italian", "tur_Latn": "Turkish",
             "vie_Latn": "Vietnamese", "ind_Latn": "Indonesian"}
EOS_ID = 120020  # config.json 的 eos_token_id


def build_messages(text, tgt):
    """返回官方英文模板的 chat messages（无 system）。参数：text 原文；tgt FLORES 目标语言码。"""
    return [{"role": "user", "content": PROMPT.format(target_lang=LANG_NAME[tgt], source_text=text)}]


def parse_pairs(spec):
    """解析 --pairs：'core11' 或逗号分隔的 'eng_Latn-zho_Hans,...'。返回 [(src, tgt)]。"""
    if spec == "core11":
        return list(CORE11)
    return [tuple(p.split("-")) for p in spec.split(",")]


class PairStore:
    """一个语向的断点续跑存储：逐句追加 jsonl，完成后写 src/ref/hyp.txt。"""

    def __init__(self, version, src, tgt, limit):
        self.dir = os.path.join(RESULTS, version, f"{src}-{tgt}")
        os.makedirs(self.dir, exist_ok=True)
        self.part = os.path.join(self.dir, "hyp.partial.jsonl")
        self.src, self.tgt, self.limit = src, tgt, limit
        self.rows = []
        if os.path.exists(self.part):
            with open(self.part, encoding="utf-8") as f:
                self.rows = [json.loads(l) for l in f if l.strip()]

    def done(self):
        """是否已完成全部句子。"""
        return len(self.rows) >= self.limit

    def add(self, hyp_raw, ntok, sec):
        """追加一句结果（原始输出、生成 token 数、耗时秒）。"""
        row = {"i": len(self.rows), "raw": hyp_raw, "ntok": ntok, "sec": round(sec, 3)}
        self.rows.append(row)
        with open(self.part, "a", encoding="utf-8", newline="\n") as f:
            f.write(json.dumps(row, ensure_ascii=False) + "\n")

    def finish(self):
        """写 src/ref/hyp.txt（hyp 去首尾空白并把换行折成空格，与 NLLB 流程一致）。"""
        srcs, refs = read_flores(self.src, self.limit), read_flores(self.tgt, self.limit)
        hyps = [" ".join(r["raw"].strip().split("\n")).strip() for r in self.rows[:self.limit]]
        for fn, lines in (("src.txt", srcs), ("ref.txt", refs), ("hyp.txt", hyps)):
            with open(os.path.join(self.dir, fn), "w", encoding="utf-8", newline="\n") as f:
                f.write("\n".join(lines) + "\n")
