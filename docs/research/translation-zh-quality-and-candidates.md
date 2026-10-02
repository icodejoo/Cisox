---
title: 英→中翻译质量诊断与候选模型调研
status: active
updated: 2026-10-02
summary: NLLB 英→中得分低的原因、解码调优结果、可用分数线怎么看、CONE-MT/Hy-MT2/Qwen/1.25-bit 等候选的调研结论与 LLM 接入 ONNX 的路线
---
## TL;DR
- NLLB-600M 英→中字符级 chrF 低（int4 24.8、fp32 28.0，OPUS-MT 33.9），主因是**中日文目标时提前截断**（模型自身问题，fp32 同样有），其次是半角标点、字符级口径。不是量化造成的。
- 解码调优后（`beam=2`、`length_penalty=2.0`、最小长度=0.7×源 token 数、全角标点后处理）：eng→zho 22.76 → 27.76（加标点后 30.81），核心 11 向均值 51.43 → 52.98。仍略低于 OPUS-MT，截断未完全消除。
- "多少分算可用"没有统一线，中文目标的字符级 chrF 要打折，**不能套用拉丁语系的 40 分线**；最终要靠真实截图文字的人工对照来判。
- 候选：Hy-MT2-1.8B（Apache-2.0，评测进行中，只做 ONNX int4 对比 NLLB int4）。CONE-MT 无公开权重，Qwen 剔除，1.25-bit 版需定制 llama.cpp，均不用。
- LLM 接入走现有 ONNX（`ort` + 自写生成循环），不引入 llama.cpp。LLM 内存约 1GB 量级，只能做"可选下载的高质量包"，不能当默认。

## 1. 英→中为什么低（2026-10-02 诊断）
同一批 30 句（FLORES，字符级 chrF，去空白，n=1..6，β=2）：

| 系统 | chrF | 全半角折叠后 | 长度比中位 | 长度比<0.7 句数 | 以句末标点结尾 |
|---|---|---|---|---|---|
| NLLB 14 语言 int4 | 24.8 | 26.7 | 0.78 | 13/30 | 0/30 |
| NLLB 原版 fp32 | 28.0 | 30.3 | 0.76 | 13/30 | 0/30 |
| OPUS-MT en-zh（beam=4） | 33.9 | 34.0 | 0.89 | 2/30 | 27/30 |

原因，按影响大小：
1. **截断（最大）**：译文在逗号处停止，丢后半句。fp32 同样，说明是 NLLB-600M 提前预测结束符，不是量化。
2. **标点风格（约 2 分）**：NLLB 输出半角 `, . "`，参考是全角，折叠后涨 1.9~2.2 分。
3. **口径**：中文同一意思有多种合理译法，参考还有数字前后加空格、人名音译，字符级 chrF 本就偏低。官方 chrF++ 对无分词中文还会被词级 n-gram 压到约 6/8。
4. **模型本身**：OPUS-MT（专做英↔中）在这个方向确实更强，多语言通才模型的普遍代价。
- 样本仅 30 句，28 与 34 的差距含抖动，不能据此下"先天缺陷"的定论。

## 2. 解码调优结果（子代理，NLLB 14 语言 int4）
截断范围（每向 30 句）：eng→zho 12 句偏短、14 句无句末标点；fra→zho 12；eng→jpn 6；其余 12 个语向合计 8。fp32 同样。

| 配置 | eng→zho chrF（加全角后） | 偏短句数 | 核心 11 向均值（加全角后） |
|---|---|---|---|
| c0 beam=4，lp=1.0（基线） | 22.76（25.10） | 12 | 51.43（51.83） |
| c1 beam=2，lp=1.0 | 23.91 | — | 51.56（51.98） |
| c2 beam=4，最小长度 0.7 | 26.28（29.32） | 6 | 52.01（52.50） |
| **c3 beam=2，lp=2.0，最小长度 0.7** | **27.76（30.81）** | 6 | **52.47（52.98）** |

- 最小长度 0.9 以上会让 fra→zho、eng→jpn 变差且慢 2~4 倍，只取 0.7。分句翻译（eng→zho 26.3）不如最小长度，不建议。
- 全角标点后处理：规则在 `eval/zh_punct.py`（9 个单测），+2.0~2.8 分；补空格对 chrF 无影响，未做。
- 副作用：核心 11 向里 8 升 3 降（arb→eng −1.5，其余近乎持平），无重复或啰嗦；arb→eng 的降幅在噪声内，上线前复核。
- 局限：配置在同一批 30 句上选出，需用 devtest 31~60 句复核；安静串行延迟未测成；`no_repeat_ngram_size`（worker 缺省 3）未测。
- 详见 `guides/translation-quantization-benchmark.md` §13。

## 3. "多少分算可用"
- 表（拉丁语系目标，粗略）：<40 基本不可用；40–55 看大意；55–65 可用；65–75 好用；>75 很好。
- **中文目标要另看**：我曾说"低 15~25 分"，那是经验估计，**没有数据支撑**。按此折算英→中约 24~34 仅算"勉强看懂大意"，之前"能看大意"的判断偏乐观，已撤回。
- 没有同口径的在线翻译或 LLM 对照分，无法横向定位。判断标准改为三条：量化相对 fp32 损失小（满足，约 −1.3）；无截断、丢句等硬伤（已大幅缓解）；真实截图短文本读起来通顺（待人工对照）。

## 4. 候选调研
| 候选 | 结论 |
|---|---|
| **CONE-MT**（基于 NLLB-1.3B，中文增强） | 2023-03 公测，说明页称"后续开源"，**没找到权重、许可证、参数量、分数**；也未见裁剪说明。不可评测，不投入 |
| **Hy-MT2-1.8B**（腾讯，2026-05，Apache-2.0） | 33 种语言含我们的 14 种；架构 HunYuanDenseV1，词表 120,818、隐藏 2048、32 层、嵌入输入输出共用；BF16 4.08GB。**评测进行中**（见 §5） |
| HY-MT1.5-1.8B | 同量级，上一代 |
| Qwen3 小模型 / Qwen-MT | Qwen3 通用小模型 0.6B 太差、1.7B 起才像样；Qwen-MT 仅 API 无开放权重。**用户决定剔除** |
| TranslateGemma 4B / MADLAD-400 3B / Seed-X-7B / Hunyuan-MT-7B / LMT-8B | 质量高但体量 3~8B，不符合内存目标，只能做云端或高端机选项 |
| EuroLLM-1.7B | 含中文但定位是欧洲语言，优先级低于 Hy-MT2 |
| OPUS-MT en-zh / zh-en、opus-mt_tiny（25M，仅中→英）、SMALL-100、t5 en/ru/zh small | 无比 OPUS-MT 更好的小体量英→中；SMALL-100 源自 M2M100，不会优于 NLLB |
- 结论：≤300MB 且专做英→中的，没有比 OPUS-MT 更好的；高质量的都是 1.7B 以上 LLM。

## 5. Hy-MT2-1.8B 量化与接入
**官方论文量化表**（BF16 为基线，综合分，口径不同，不可与我们的 chrF 直接比）：

| 版本 | FLORES 中↔外 | FLORES 英↔外 | FLORES 外↔外 | WMT25 |
|---|---|---|---|---|
| BF16 | 83.49 | 87.02 | 79.21 | 60.33 |
| FP8 | −0.38 | −0.36 | −0.58 | −0.82 |
| Q4_K_M（1.13GB） | −1.27 | −1.15 | −2.02 | −2.87 |
| 2-bit | −2.63 | −2.28 | −2.90 | −2.37 |
| 1.25-bit | 论文表里没有数字 | | | |
- 4 位损失约 1.2，与我们 NLLB int4（−1.3）同量级。2-bit/1.25-bit 用**蒸馏式量化感知训练**，我们的后训练 RTN 做不到（NLLB int2 直接崩）。
- **词表裁剪决定：不裁**。嵌入约 2.47 亿参数，其余约 15.5 亿在 32 层里，词表裁到 6~8 万在 int4 下只省 20~50MB，收益太小。不裁语言。
- **1.25-bit GGUF**（440MB，Sherry 三值稀疏，STQ1_0 格式）：需定制 llama.cpp（PR #22836 未合并），只有 ARM NEON 有加速，x86 回落通用反量化，ONNX 无对应内核；质量无公开数字。**不用**。待验证的假设（未做）：三值权重理论上可无损重编码为 ORT 2-bit MatMulNBits，整体约 0.6~0.7GB（自行推算），仅在 int4 版质量好但体积不满意时再评估。
- **接入 ONNX 的路线（推荐）**：optimum 导出带 KV cache 的纯解码器 ONNX → 沿用对称 RTN int4（块 32）→ 在 `snow-translator` 里写"固定提示词 + 逐 token 生成"循环，复用 `ort`/`tokenizers`，不新增依赖。不推荐 onnxruntime-genai（新原生库）与 llama.cpp（并存两套引擎）。
- 风险：Hy 的 ONNX 导出可行性未验证（README 要求 transformers≥5.6，venv 为 4.57.6，模型 config 写 4.57.6，先验证能否加载）。
- 内存：1.8B int4 仍约 1GB 量级，远超 300MB，**只能做可选下载高质量包，NLLB 仍是默认兜底**。
- 评测方案：官方英文模板 `Translate the following text into {target_lang}. Note that you should only output the translated result without any additional explanation:\n\n{source_text}`，语言名用英文全名，不调提示词；确定性贪心（repetition_penalty=1.05）；与 NLLB 同样的 11 向同样句子同样 chrF 口径。**范围已砍到只做 ONNX int4 对比 NLLB int4**（砍掉 BF16 基线与 GGUF 路线）。结果将写入 `guides/translation-hymt2-eval.md`。模型已下载到 `E:\models\translate-eval\hy-mt2-1.8b\`。

## 6. 决策与待办
已决定：
- NLLB 默认包：采用推荐解码配置 + 中日文全角标点后处理（已落到 Rust）。
- Hy-MT2：不裁剪、只做 int4、ONNX 路线；Qwen、CONE-MT、1.25-bit 剔除。

待办：
- [x] Rust worker：新增最小长度参数（`beam.rs` 已有 `length_penalty`）、默认 `num_beams=2`、`length_penalty=2.0`、全角标点后处理（移植 `zh_punct.py`）。（2026-10-02 完成，见 `../guides/translation-model-release.md` §5.6；前 10 句复核偏短 旧 12 → Rust 3 / 30）
- [ ] 用 devtest 31~60 句复核推荐配置；安静条件下测延迟；评估 `no_repeat_ngram_size` 影响；arb→eng 复核。
- [ ] 真实截图文字人工对照，判断英→中是否可用。
- [x] 英→中额外提供 OPUS-MT 小包：已做出 en→zh（113 MiB）与 zh→en（123 MiB）int4 包并在真实 worker 验证（2026-10-02，见 `../guides/translation-model-release.md` §6）。结论：同口径下 OPUS-MT en→zh int4 30.35，NLLB 调优后 27.76（加全角标点 30.81），差距约 2~3 分而非此前的 9 分（§1 的 33.9/24.8 与本脚本绝对值有约 2 分系统差）；worker 翻译期峰值约 200 MiB（NLLB 447）；许可 en→zh 为 Apache-2.0、zh→en 为 CC-BY-4.0（可商用，需署名）。int8 动态量化在 zh→en 上整档崩溃，不可用。Hy-MT2 可选高质量包仍待其评测结果。
- [ ] 设置页：`screenshot_translation/local_num_beams`（1~8，低内存模式强制 1）补 i18n 标签/说明、显示低内存覆盖。（配置默认值已在 `snow-config` 改成 2；设置页 UI 与 i18n 仍待做）

## 7. 来源
- Hy-MT2 论文：arxiv.org/html/2605.22064；模型：huggingface.co/tencent/Hy-MT2-1.8B 及 `-GGUF`、`-1.25Bit-GGUF`
- Sherry：aclanthology.org/2026.acl-long.513；llama.cpp PR #22836
- CONE：github.com/CONE-MT/CONE（`readme_Chinese.md`）
- 其它：Helsinki-NLP/opus-mt-zh-en、alirezamsh/small100、google/translategemma-4b-it、google/madlad400-3b-mt、arxiv.org/html/2507.13618v2（Seed-X）、utter-project/EuroLLM-1.7B
- 这些以搜索结果与页面摘要为准，未逐条精读；落地前请对照原文。
