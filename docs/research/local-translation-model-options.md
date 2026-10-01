---
title: 本地离线翻译模型选型调研（≤300MB 工作集）
status: active
updated: 2026-10-01
summary: 在 ≤300MB 常驻内存、Intel 核显/CPU、GPL-3.0、模型不随包分发的前提下，评估 NLLB 裁剪版、OPUS-MT、Bergamot student、M2M100、mBART-50、MADLAD、HY-MT、TranslateGemma 等；结论是没有"单个模型覆盖十几种语言且 ≤300MB"的已证实方案，推荐 OPUS-MT 按语向下载 + Bergamot 对照 + 自裁 M2M100，给出最小验证计划
---
# 本地离线翻译模型选型调研（≤300MB 工作集）

标记约定：【来源】= 读到公开页面（链接见 §9）；【推断】= 我的计算或判断；【未验证】= 没有证据，不能当事实。检索工具可用，但多数数字来自搜索摘要与 WebFetch 的小模型转述，**关键数字落地前请对照模型卡/论文原文复核**。本文在 [adr5-local-nmt.md](adr5-local-nmt.md) 基础上扩展，后端部分不重复其已查清的内容。

## 0. 结论先行

1. **没有查到任何"单个模型、覆盖十几种语言、加载后 ≤300MB"的已证实方案。** 最接近的 NLLB-200-distilled-600M 在 CT2 int8 下磁盘就约 594MiB【来源·搜索摘要】；即使把 25 万词表裁到 3 万，按参数量估算仍约 380MB（§1.3，【推断】），超预算。
2. **"NLLB 有裁剪版"的核实结果**：
   - 官方没有出"词表裁剪版"。官方稠密尺寸为 distilled-600M / distilled-1.3B / 1.3B / 3.3B（另有 54.5B MoE）。
   - 社区确实有 **truncated 版**：`slone/nllb-pruned-6L-512d-finetuned`，6 层、512 维，175M 参数，其中 131M 是 token embedding；作者自述"really bad at translation"，只是微调的起点，**不可直接用**【来源】。
   - **词表裁剪是有论文支持的做法**：一篇 2026 年预印本把 M2M100/mBART-50/NLLB-200 的词表从 12.8 万~25.6 万裁到约 1~2.6 万，嵌入层省约 60%，但只测了 **英→阿拉伯** 一个方向，且裁后做了领域微调【来源】。**没有查到**现成的、按语种子集裁好的 NLLB 成品，也没有"按语种子集裁剪的质量损失数据"，也没有查到公开发布的裁剪工具/代码。naver 的 `nllb-pruning` 是裁 **MoE 专家**（针对 54.5B 模型），对 distilled-600M 无意义【来源·搜索摘要】。
   - 所以"用户记得有裁剪版"能对上的是 **truncated 小模型** 和 **词表裁剪论文**，不是一个可下载的 300MB 成品。
3. **许可是 NLLB 的硬伤**：权重 CC-BY-NC-4.0，模型卡写"not released for production deployment"【来源】。我们不分发、只在设置页中立提示，在 GPL-3.0 开源项目里法律风险主要在用户侧，但**我们的下载清单若写成"推荐下载"，等于从项目侧引导非商用权重**，需要在文案里写明限制并由你裁决（§2）。**OPUS-MT（CC-BY-4.0）、Bergamot 模型（MPL-2.0）、M2M100（MIT）没有这个问题**。
4. **推荐短名单**（§6）：① OPUS-MT 按语向小模型（首选，许可最干净、体积最小、两条后端都可行）；② Bergamot/Firefox student（质量-体积比最高，但要引入第三套运行时，建议先当质量/速度对照）；③ 自裁词表的 M2M100-418M（唯一有机会"一个模型覆盖多语且接近预算"的 MIT 候选，需要自己做裁剪）；④ NLLB-600M（int8，仅作质量上限基线，预算存疑）。HY-MT、TranslateGemma、MADLAD、LFM2、Qwen3 小档落选，理由见 §7。
5. **推理后端：ADR-5 的前提已经过时。** 现有 `tools/snow-translator`（ort）并不是"零实现"：仓库里已有 `beam.rs`（束搜索）和 `engine.rs`（encoder 一次 + merged decoder 带 KV cache，支持 `num_beams`），**但 `manifest.rs` 目前只放行 `family=="marian"`**（NLLB/M2M100 会被拒绝）。因此：OPUS-MT 走 ort 零新增；NLLB/M2M100 走 ort 需要扩 manifest 与 BOS/语言码强制逻辑，或改走 CT2。**建议裁决**：先用 ort 把 OPUS-MT 实测完（成本最低），M2M100/NLLB 的对照用 Python 参考实现即可，CT2 是否引入等 §8 的数据出来再定。
6. 原计划"十几种语言"在 300MB 内不现实时的退路，两个方案对比见 §6.5：联合国六语（en 与 zh/fr/es/ru/ar 共 10 个方向）用 OPUS-MT，总下载约 0.8GB（【推断】），同一时刻只加载一个。

## 1. NLLB-200 与裁剪

### 1.1 官方尺寸与许可

| 项 | 事实 | 标记 |
|---|---|---|
| distilled-600M | 约 615M 参数（"615M student"）；权重 CC-BY-NC-4.0；"not released for production deployment"；模型卡限定 512 token、非文档翻译 | 【来源】（参数量来自论文转述摘要，**未对照模型卡原文**） |
| distilled-1.3B | 1.3B 级，CC-BY-NC-4.0 | 【来源】HF 页面存在；许可按同系列，**未逐页核实** |
| 1.3B / 3.3B / 54.5B MoE | 存在；CT2 有 OpenNMT 官方 int8 转换仓库（1.3B、3.3B） | 【来源·搜索结果列表】 |
| 质量（FLORES-200 全部 200 语平均） | NLLB-600M：chrF++ 45.73；NLLB-1.3B：47.77 | 【来源·搜索摘要】，是跨 200 语平均，**不是 zh/ar/ru 等具体语向**。论文给的是 distilled 比同尺寸 baseline 高 0.3~0.5 chrF++ |
| 中英及联合国六语分语向分数 | **没有查到可引用的逐语向数字**（NLLB 论文与模型卡的逐语向表本次没拿到） | 查不到 |

### 1.2 实际体积与内存

| 形态 | 体积 | 标记 |
|---|---|---|
| 原始 fp32 checkpoint | 2.31GiB | 【来源·搜索摘要】 |
| CT2 int8（600M） | 594MiB，含 tokenizer 文件 621MiB | 【来源·搜索摘要，所指页面未逐一核实】 |
| ONNX（Xenova 导出）int8 / quantized | encoder 415~419MB + decoder-merged 约 476MB（quantized）；其它 int8 变体 decoder 达 1.52GB；整仓 34.2GB（含大量变体，用户极易下错） | 【来源·HF 文件列表】 |
| 加载后常驻内存 | **没有查到公开数字** | 查不到 |

### 1.3 词表裁剪的预算推算（【推断】，需实测）

- 嵌入规模：`slone` 卡写 512 维模型 token embedding 为 131M，反推词表约 25.6 万；1024 维时嵌入约 262M，**占 600M 模型的 40% 以上**（【推断】）。
- 词表裁到 3 万（联合国六语加少量语种的粗估）：嵌入约 31M，全模型约 384M 参数，int8 约 384MB，**仍超 300MB，还没算激活与 KV cache**。
- 想进 300MB 只有再砍层数/宽度（slone 的 6L-512d 路线），则要重新微调，质量不可预期，且仍是 NC 许可。
- 裁剪能保住质量吗：论文只给了 M2M100 英→阿拉伯微调后 BLEU 42.04 / chrF++ 58.81 / COMET 0.8730（MultiUN 域），对比 OPUS-MT-en-ar 的 44.59 / 58.77 / 0.7911；**这是在联合国语料上微调后的结果，不能外推到通用文本和其它语向**【来源】。同一论文里 NLLB-200 微调后只有 chrF++ 55.79，低于 M2M100 与 mBART-50（57.47）【来源】；意味着"NLLB 一定比 M2M100 好"在这个小设置里不成立，但也只是单语向单领域证据。

### 1.4 "只在设置页推荐用户自行下载"的合规评估

- 【来源】权重 CC-BY-NC-4.0：非商用；模型卡写明未用于生产部署。
- 【推断】我们不分发权重，GPL-3.0 是代码许可，二者不直接冲突；风险点：(a) 项目若有商业化/付费版本，推荐使用 NC 权重会带来问题；(b) 清单里写默认下载地址、一键下载，实质是项目在促成使用；(c) 与 `docs/principles.md` 无冲突，但属于"法律判断"，**我不是律师，需要你定**。
- 【推断】稳妥做法：NLLB 只进"高级/自备模型"路径（用户自己放目录），默认下载清单不放；其余候选无此问题。

## 2. 其它候选逐个评估

下表"≤300MB"一栏按磁盘体积与参数量推断工作集，**均为【推断】**，实测前不下结论。

| 候选 | 参数量 / 量化后体积 | 语种覆盖 | 质量证据 | 许可（对 GPL-3.0） | 现成权重与格式 | 判断 |
|---|---|---|---|---|---|---|
| **OPUS-MT**（每语向一个，Marian） | zh-en 编码器与解码器各约 74M（Qualcomm AI Hub 页面，页面把两者分开列，总参数量以实测为准）；fp32 约 280MB 量级；int8 约 80MB 量级【推断】 | 每语向一个模型：zh-en、en-zh 已核实存在；en↔es/fr/ru/ar 的模型 id **未逐个核实** | zh→en Tatoeba：BLEU 36.1，chrF 0.548【来源】；en→zh 页面给的 chrF2 0.268 与 BLEU 31.4 不自洽（可能是不同 tokenization），**不可当质量证据**。FLORES 数字：tc-big-ar-en flores101-devtest chrF 0.66987【来源·搜索摘要】；tc-big-en-ko 0.364【来源】 | zh-en 为 CC-BY-4.0【来源】。gaudi 的 CT2 转换仓库标 Apache-2.0，写明"与原仓库一致"，与原卡不一致，**以原卡 CC-BY-4.0 为准**【来源】 | ONNX：Xenova/opus-mt-*（ADR-5 已核实 zh-en）；CT2：`gaudi/opus-mt-*-ctranslate2`、`michaelfeil/ct2fast-opus-mt-*`、`manancode/*-android`（第三方转换，可用性与更新保证**未验证**）；原始 PyTorch：Helsinki-NLP | 许可最干净，体积最小，首选 |
| **OPUS-MT 多语种变体** | tc-bible-big 系列（如 poz-en 为 0.2B）；transformer-big | 多对一/多对多（如 `mul-mul`、`zhx-en`） | 卡片自报 Tatoeba chrF 0.70473（sla-en）；**没有 zh/ar/ru 的 FLORES 汇总** | 同 OPUS-MT，具体各卡**未逐一核实** | HF 有；CT2/ONNX 现成件**未验证** | 体积在 200M 参数级，≤300MB 勉强；质量无证据，列为次要对照 |
| **Bergamot/Firefox student** | 一个 student 为 tiny/base-memory/base；int8 约 17~44MB 每语向；样例 student 15.7M 参数、量化后 17MB；对应 teacher 798MB、192.75M 参数，BLEU 52.5 对 50.7（仅该样例，英德）【来源】 | Firefox 现支持列表含阿拉伯语、简体与繁体中文、日语、韩语、法语、西班牙语等；en-zh、en-ja 有"LLM 合成语料微调"版（2025-08 加入）【来源·搜索摘要】；全部是**与英语的双向 pivot**，无非英语直连 | Mozilla 发布门槛：COMET 在 Google Translate ±5% 以内【来源】。**没有 FLORES chrF++ 数字** | 模型文件 MPL-2.0；bergamot-translator MPL-2.0【来源】。MPL-2.0 与 GPL-3.0 可组合（MPL §3.3 次级许可，常识，**需你/法务确认**） | 格式是 Marian intgemm8 `.bin` + SentencePiece `vocab.spm` + lex shortlist，**CT2 与 ort 都不能直接加载**；运行时要 bergamot-translator（C++，Windows 构建未验证）或 slimt（只文档化 en-de tiny，Linux 向，许可**未查到**）。分发：Remote Settings 附件，GCS 桶；仓库 2025-12-15 已归档，稳定下载 URL **未查到** | 质量/体积/速度最优的一类，但接入成本最高，见 §6 |
| **M2M100-418M** | 418M（名称）；词表约 12.8 万；int8 约 420MB 量级，裁到 3 万词表约 310MB 量级【推断】 | 101 语，9900 方向【来源】 | 卡上无分数；上述论文里微调后 en-ar 最好【来源】 | **MIT**【来源】 | CT2 官方转换支持（`ct2-transformers-converter --model facebook/m2m100_418M`）；ONNX 现成件**未核实** | 唯一在许可上干净、又"一个模型多语"的候选；要自己裁词表 |
| **mBART-50 many-to-many** | 0.6B；约 610M 级 | 50 语 | 卡上无分数 | 卡页面**未写**许可 | CT2 官方列为支持 | 体积大于 M2M100，质量无证据，落选 |
| **MADLAD-400** | 官方只有 3B / 7.2B / 10.7B，**没有小档**；有人做 GGUF 2-bit 到 <1GB 但质量损失大 | 400+ | 广度为主，高资源语言可用 | Apache-2.0 | HF 有 | 体积不符，落选 |
| **Tencent HY-MT1.5-1.8B** | 1.8B；GGUF 2-bit 574MB、1.25-bit 440MB、fp16 3.3GB【来源】 | 33 语 + 5 种方言，含阿拉伯、俄、西、法、中、英；1056 个方向【来源】 | 仅厂商自报"FLORES-200 中外互译超过更大开源模型和商用 API"，**无逐项数字** | **Tencent HY Community License**：territory 排除欧盟、英国、韩国；禁止用输出改进其它模型；MAU>1 亿要另申请【来源】。第三方博客称 "HY-MT2 1.8B" 于 2026-05-21 以 Apache-2.0 发布，**未对官方卡核实** | GGUF 标准 llama.cpp 可跑，需 llama.cpp 级新运行时 | 地域限制与 GPL "不得追加限制"精神冲突，【推断】；体积超 300MB；落选，列为观察 |
| **Google TranslateGemma** | 4B 卡页写 5B 参数【来源】；Q4 级约 2.5~3GB【推断】 | 55 语评测 | 4B：WMT24++ MetricX 5.32【来源】 | **Gemma Terms of Use**（专有条款） | GGUF 多个社区版 | 远超预算，落选 |
| **LFM2-350M-ENJP-MT** | 350M | **仅英↔日**【来源】 | 对标 GPT-4o 为厂商自报 | **未查到** | 有 GGUF | 语种不符，落选 |
| **Qwen3 小档** | 0.6B / 1.7B | 多语 | FLORES-200 devtest：0.6B chrF++ 36.36、1.7B 42.41；NLLB-600M 为 45.73（来自不同论文摘要，语言集合与条件**未对齐**，只能当量级参考）【来源·搜索摘要】 | 未核实 | GGUF 普遍 | 同体量下通用 LLM 弱于专用 NMT，落选 |

## 3. 为什么没有"一个小模型管十几种"

- 多语 NMT 的体积大头在**词表嵌入**（NLLB 约 40% 以上），裁词表能省，但省下的量不够把 600M 级压进 300MB（§1.3）。
- 小而强的方案（Bergamot、OPUS-MT）靠**每语向独立**换体积；代价是**非英语语向要经英语中转**（质量与延迟都会损失，【推断】），且总下载量随语向数线性增长。
- LLM 路线（HY-MT、TranslateGemma）覆盖语种与文风好，但体积在 440MB 到 3GB，需要 llama.cpp，与总原则"能复用就不新增依赖"相悖。

## 4. 推理后端可行性

### 4.1 现状（以代码为准）

【来源·仓库】`snow-shot-rs/tools/snow-translator`：依赖 `ort =2.0.0-rc.13`（load-dynamic）、`tokenizers 0.23.2`、`ndarray`；`beam.rs` 实现束搜索，`engine.rs` 实现 encoder 一次 + merged decoder 的 KV cache 解码，请求可带 `num_beams`；`manifest.rs` 第 220 行起**只允许 `family == "marian"`**，其它族返回"only `marian` is implemented"。`snow-translate` 的 `ModelManifest` 已有 `family / languages / pairs / lang_tokens / source_prefix / sha256 / max_input_tokens` 字段，足够描述 NLLB/M2M100 的语言码映射，**但还没有 `runtime` 字段**（ADR-5 建议的 `ct2 | onnx` 尚未落地）。

这意味着 ADR-5 §1.2 里"ort 解码循环实现负担：高"对 **Marian 已经付清**；对 NLLB/M2M100 还差：目标语言码强制作为 decoder 起始 token、`forced_bos`、源端语言码前缀（`lang_tokens` 与 `source_prefix` 字段疑似已为此预留，**逻辑是否齐全未验证**）。

### 4.2 候选 × 后端对照

| 候选 | CTranslate2（ct2rs） | ort（现有 worker） | tokenizer | 最省事搭配 |
|---|---|---|---|---|
| OPUS-MT | 官方支持 Marian，有第三方转换件；带 beam | **已支持**（family=marian），Xenova ONNX 现成，文件多、易选错（int8 encoder 52.7MB，ADR-5 已记） | SentencePiece 或 `tokenizer.json`，Xenova 带 tokenizer.json（含 null 词表项，已有绕过代码） | **ort（零新增）** |
| OPUS-MT 多语种变体 | 同上 | 同上，但需要目标语言前缀 token（`lang_tokens`） | 同上 | ort |
| Bergamot student | 不能直接加载；需 fp32 npz 转换，**fp32 是否公开未查到** | 不可 | SentencePiece（vocab.spm） | 只能 bergamot-translator/slimt，需新 C++ 运行时 |
| M2M100-418M | 官方支持，`target_prefix` 机制 | 需要 optimum 导出 + 扩 manifest 与语言 token 逻辑；ONNX 现成件**未核实** | HF tokenizer（SentencePiece + 语言码表） | CT2 更省事；ort 可行但要补代码 |
| NLLB-600M | 官方支持，有成品 int8 | Xenova ONNX 有，但 decoder int8 变体 1.5GB，quantized 约 476MB，整体偏大 | `tokenizer.json` 可用 | CT2 |
| mBART-50 | 官方列为支持 | 未核实 | SentencePiece | 落选，不展开 |
| HY-MT / TranslateGemma | 不适用（decoder-only LLM） | 不适用 | — | llama.cpp，不展开 |

词表裁剪支持：【未验证】CT2 与 ort 是否有"词表映射/重映射"的现成接口。通用做法是**在 PyTorch 里先裁嵌入矩阵、同步重映射 tokenizer，再导出**（论文方法即此），所以**裁剪属于离线脚本，与后端无关**。CT2 转换器是否直接接受裁后的 checkpoint，需要实测。

### 4.3 构建依赖与体积（沿用 ADR-5，不重复）

CT2：cmake、MKL/OpenMP 的 Windows 默认特性、体积未验证。ort：已在项目，无新增。**本轮没有新增这些点的证据。**

## 5. Intel 核显/CPU 速度证据

- 【来源】CT2 官方基准 658.8 tokens/s（ADR-5 已指出对比方不清、自家基准）。
- 【来源】Mozilla student 在单核 CPU 上翻译 17.9 秒 vs teacher 631 秒（同一测试集，测试集规模未记，仅该样例英德）。
- 【未验证】以上都不是 Intel UHD 核显，核显对这类小 Transformer 是否加速未查到证据；ORT 的 DirectML 已进入 sustained engineering（ADR-5 已记）。**本机实测是唯一可靠来源。**

## 6. 推荐短名单

排序按"满足 ≤300MB 且质量尽量好"，同时考虑许可与接入成本。

### 6.1 OPUS-MT 按语向下载（首选）

- 理由：CC-BY-4.0；每语向约 80MB 量级（int8，【推断】）；Marian 路径在 ort worker 里**已实现**；有 Xenova ONNX 现成件，还有多个第三方 CT2 件。
- 下载清单（以 zh-en 为样例，其它语向以对应 HF 仓库为准，**哈希未查到，需下载后自己算并写入清单**）：
  - `https://huggingface.co/Xenova/opus-mt-zh-en/tree/main/onnx` 下的 encoder/decoder_model_merged 的 int8 或 quantized 文件（ADR-5 记 encoder int8 52.7MB；decoder 体积本轮未查）；
  - 同仓库 `tokenizer.json`、`config.json`；
  - 清单 `model.json` 由我们生成，字段沿用 `snow-translate::ModelManifest`。
- 许可提示要点：CC-BY-4.0，需署名；页面显示作者 University of Helsinki / Helsinki-NLP；不写"推荐某模型质量最佳"。
- 已知短板：非英语方向需经英语中转（zh↔fr 要两跳）；en→zh 质量数据不自洽，需实测。

### 6.2 Bergamot/Firefox student（质量-体积对照，建议先实测、后定是否接入）

- 理由：每语向 17~44MB，是同量级里**唯一有厂商发布门槛（COMET 距 Google ±5%）**的；中文有专门微调版。
- 成本：需要第三套运行时；稳定下载源不明（仓库已归档，模型在 GCS 与 Remote Settings）。
- 建议：只在验证里借 Firefox 自带模型/translateLocally 做**质量参照**，不先接入工程；若质量显著领先再单独立项。
- 许可提示要点：MPL-2.0。

### 6.3 M2M100-418M 自裁词表（MIT；"一个模型多语"的唯一希望）

- 理由：MIT；101 语；裁到 3 万词表后约 310MB 量级（【推断】），贴近预算；CT2 官方支持。
- 成本：我们要自己写裁剪脚本并微调验证（**没有查到现成工具/成品**）；质量无逐语向证据；联合国语料上论文结果好，但那是微调后。
- 下载清单：本项目需要先**自己产出并托管裁后权重**才能给用户下载——这本身与"推荐用户下载公开来源"相悖，**需你裁决是否接受自己托管**。否则只能让用户按脚本在本机生成。

### 6.4 NLLB-200-distilled-600M int8（质量上限基线，预算存疑）

- 理由：质量参照（FLORES 平均 chrF++ 45.73）；有 CT2 成品；**但 594MiB 磁盘，裁词表后仍约 380MB，且 CC-BY-NC-4.0**。
- 定位：仅作评测基线，**默认下载清单不放**。

### 6.5 若 300MB 内放不下十几种语言：联合国六语 vs 按语向小模型

| 方案 | 总下载量（【推断】） | 同时加载 | 切换延迟 | 备注 |
|---|---|---|---|---|
| 联合国六语，OPUS-MT 每向一个 | en↔{zh,fr,es,ru,ar} 共 10 个方向，每个约 80MB，约 0.8GB | 1 个，约 150~250MB 工作集（粗估） | 需重新建 session，**未测**，估计亚秒到数秒 | 非英语对（如 zh→fr）要走两跳 |
| 联合国六语，单个裁词表 M2M100 | 约 310MB | 1 个，工作集可能 >300MB | 无切换 | 需自行裁剪，质量未知 |
| 十几种语言，OPUS-MT 每向一个 | 约 20+ 个方向 ≥1.6GB | 1 个 | 同上 | 用户只下自己需要的方向即可 |

### 6.6 落选清单

HY-MT（地域许可 + 体积）、TranslateGemma（体积 + Gemma 条款）、MADLAD（无小档）、LFM2 ENJP（只有日英）、Qwen3 小档（同体量弱于 NMT）、mBART-50（体积 + 无证据）。

## 7. 开放问题（需要你裁决）

1. **NLLB 能否出现在默认下载清单**：我建议不放，只作"自备模型"。
2. **ADR-5 后端裁决**：既然 ort worker 已有 Marian 的束搜索与 KV cache，是否取消"CT2 优先"，改为"OPUS-MT 走 ort，M2M100/NLLB 视验证再定"？
3. **是否接受自己托管裁剪后的 M2M100 权重**，还是只提供脚本。
4. **是否接受第三套运行时（Bergamot）**，还是仅作质量参照。
5. **联合国六语优先还是十几种语言优先**：决定下载清单规模。
6. 清单还缺 `runtime` 字段（ADR-5 已提）；本轮不动代码。

## 8. 最小验证计划

### 8.1 测试集

- FLORES-200 devtest，**每语向取前 50 句**（devtest 共 1,012 句）。许可 CC-BY-SA-4.0，不得把整套数据提交进仓库；【来源】HF 数据集 `facebook/flores` 说明该库已不再更新，新版为 `openlanguagedata/flores_plus`。取用方式：在评测用临时目录下载，**不入库**。
- 语向：zh↔en、en↔fr、en↔es、en↔ru、en↔ar 各取 50 句；另测 zh→fr 一个非英语方向以量化中转损失。

### 8.2 指标

- 质量：chrF++（主），可用 sacreBLEU 参考实现；若我们自己在 Rust 里实现 chrF++ 须与 sacreBLEU 对拍一份；中文需按字符计。
- 资源：加载后 Working Set（沿用 `sysmem.rs` 思路或外部采样）、冷启动到首译、热调用单句延迟（中位数，各 10 次）、吞吐（句/秒）、卸载后内存归还。
- 条件：Intel UHD 核显机器，CPU 推理，beam=4 与 beam=1 各一轮。

### 8.3 脚本放置

- 建议 **`snow-shot-rs/tools/` 下独立小工具**（Rust，沿用 `snow-ocr-compare` 的目录风格）负责驱动 worker 和测内存；质量打分与 Python 参考实现放 `build/` 下临时 venv，**不进工程依赖**。
- 需要你批准的第三方包（仅评测用）：`sacrebleu`（打分）、`transformers` + `torch`（参考实现与词表裁剪）、`ctranslate2`（对照后端）、`optimum`（ONNX 导出，若 Xenova 无对应件）、`sentencepiece`、`datasets` 或手动下载 FLORES。

### 8.4 验收判据（提案，可改）

| 项 | 判据 |
|---|---|
| 工作集 | 加载后常驻 ≤300MB |
| 首译 | 含加载 ≤3s（沿用 ADR-5 S4） |
| 热调用 | 单句 ≤500ms |
| 质量 | 相对 NLLB-600M 基线的 chrF++ 下降 ≤3 分算可接受（阈值为提案，需你定）；相对 Bergamot 同语向不低于其 −2 分 |
| 非英语语向 | 记录中转的 chrF++ 损失，不设阈值 |
| 卸载 | worker 退出后内存回收 |

### 8.5 步骤

1. 用 Xenova 的 opus-mt-zh-en 在现有 worker 跑通，确认 int8 的体积与 Working Set（零新增）。
2. 补 en↔fr/es/ru/ar 的 OPUS-MT 件，核实存在与体积，跑 50 句 chrF++。
3. 用 Python 参考实现跑 NLLB-600M 与 M2M100-418M 的同批 50 句，作质量基线。
4. 若有余力：试一次词表裁剪并测质量损失与体积，回答"裁完能否 ≤300MB"。
5. 借 Firefox/translateLocally 取 Bergamot 的同批语向质量作参照。

## 9. 来源

- NLLB 裁剪：https://huggingface.co/slone/nllb-pruned-6L-512d-finetuned ；https://arxiv.org/html/2608.03480 ；https://arxiv.org/pdf/2212.09811 ；https://github.com/naver/nllb-pruning
- NLLB 卡与体积：https://huggingface.co/facebook/nllb-200-distilled-600M ；https://huggingface.co/mijuanlo/nllb-200-distilled-600M-ct2-int8 ；https://huggingface.co/OpenNMT/nllb-200-distilled-1.3B-ct2-int8 ；https://huggingface.co/Xenova/nllb-200-distilled-600M/tree/main/onnx ；https://arxiv.org/pdf/2207.04672
- OPUS-MT：https://huggingface.co/Helsinki-NLP/opus-mt-zh-en ；https://huggingface.co/Helsinki-NLP/opus-mt-en-zh ；https://huggingface.co/gaudi/opus-mt-en-zh-ctranslate2 ；https://aihub.qualcomm.com/models/opus_mt_zh_en ；https://huggingface.co/Helsinki-NLP/opus-mt-tc-bible-big-poz-en
- Bergamot/Mozilla：https://hacks.mozilla.org/2022/06/training-efficient-neural-network-models-for-firefox-translations/ ；https://github.com/mozilla/firefox-translations-models ；https://github.com/mozilla/translations ；https://mozilla.github.io/translations/firefox-models/ ；https://github.com/browsermt/bergamot-translator ；https://github.com/dominostars/slimt ；https://blog.mozilla.org/en/firefox/cjk-translation-on-android/
- M2M100 / mBART / MADLAD：https://huggingface.co/facebook/m2m100_418M ；https://huggingface.co/facebook/mbart-large-50-many-to-many-mmt ；https://huggingface.co/google/madlad400-3b-mt
- HY-MT：https://github.com/Tencent-Hunyuan/Hy-MT ；https://raw.githubusercontent.com/Tencent-Hunyuan/Hy-MT/main/License.txt ；https://huggingface.co/tencent/Hy-MT1.5-1.8B-2bit-GGUF ；https://www.orcarouter.ai/blog/tencent-hy-mt2-1-8b-vs-madlad-400-1-3b（第三方，未核官方）
- TranslateGemma / LFM2 / Qwen3：https://huggingface.co/google/translategemma-4b-it ；https://huggingface.co/LiquidAI/LFM2-350M-ENJP-MT ；https://arxiv.org/pdf/2502.02481
- 后端与数据集：https://opennmt.net/CTranslate2/guides/transformers.html ；https://huggingface.co/datasets/facebook/flores
- 仓库内：`snow-shot-rs/crates/snow-translate/src/lib.rs`（`ModelManifest`）、`snow-shot-rs/tools/snow-translator/Cargo.toml`、`src/manifest.rs`、`src/engine.rs`、`src/beam.rs`；`docs/research/adr5-local-nmt.md`

## 10. 没能证实的点

- NLLB 逐语向 FLORES 分数；NLLB 加载后的真实内存；任何按语种子集裁好的 NLLB 成品及其质量损失；公开的词表裁剪工具。
- OPUS-MT 其它语向的模型 id 与体积、int8 实测内存；en→zh 的可信质量数字。
- Bergamot 模型的稳定公开下载 URL、fp32 是否公开、bergamot-translator 在 Windows 的构建、模型在核显上的速度、FLORES chrF++。
- slimt 的许可；HY-MT 现行许可（Tencent HY Community 与"Apache-2.0 的 HY-MT2"并存的说法，只核实了前者）。
- CT2/ort 对词表映射的原生支持；M2M100 的现成 ONNX 件。
- 任何 Intel 核显上的实测速度。

## 补充：HuggingFace 上的 NLLB 裁剪模型核实（2026-10-01，主会话直接读 HF 页面与 config.json）
上一轮只找到 `slone` 一个，**漏了其余几个**；以下按 HF 接口与模型页面核实，【来源】均为 HF 页面。
| 模型 | 裁了什么（【来源】） | 规模 | 对我们的意义 |
|---|---|---|---|
| `ayymen/nllb-200-distilled-600M-pruned`（2025-09，safetensors，F32） | **仅词表被裁**：`config.json` 里 `vocab_size=15273`（原版约 25.6 万），`d_model=1024`、编码/解码各 12 层、ffn 4096 未变；参数 **368,358,400** | 约 368M（F32 约 1.47GB） | **模型卡为空**：没写裁剪方法、保留了哪些语言、许可证、评测，作者未填；下游全是柏柏尔语（Tamazight）微调，推断是按该语种语料裁的词表。词表仅 1.5 万，**能否覆盖中文常用字、俄语、阿拉伯语未验证**，不能直接用于联合国六语 |
| `edyrkaj/nllb-executorch-pruned`（2025-12） | **只保留 6 种语言**：eng_Latn、deu_Latn、als_Latn、ell_Grek、ita_Latn、tur_Latn；ExecuTorch `.pte`、FP32、面向 react-native-executorch；许可 CC-BY-NC-4.0 | 体积、内存、速度、质量均未公开 | **不含中、法、俄、西、阿**；移动端专用格式，无法用于我们的 worker |
| `slone/nllb-pruned-6L-512d`、`-65Kv`、`-finetuned`（2023） | **层数与维度裁剪**：6 层、512 维，另有 65K 词表版 | 约 175M | 作者自称质量很差，只能当微调起点 |
| `vocabtrimmer/*` | 通用的词表裁剪工具及成品（mT5、XLM-R、mBART 等） | — | **没有 NLLB/M2M 的成品**，但方法可复用 |
**数学约束（【推断】，由上面的 config 推出）**：NLLB-600M 词表裁到约 1.5 万后仍有 **约 3.5 亿非嵌入参数**（12+12 层、d_model 1024、ffn 4096）。int8 约 1 字节/参数，**仅权重就约 350MB，超过 300MB 内存预算**，还没算激活与 KV 缓存；int4 才可能进 300MB，但质量损失没有数据。所以"只裁词表"对 NLLB-600M **不足以满足 300MB**；要进 300MB 只能裁层数/维度（如 slone 的 175M，但质量很差、需微调）。
**结论**：HF 上有裁剪版，但**没有一个是现成可用于我们语种集的**（ayymen 的语言覆盖未知且无许可/评测，edyrkaj 缺我们要的语种且非商用，slone 质量差）；自己裁词表可行，但即使成功也难进 300MB。因此第一阶段仍以 OPUS-MT 按语向下载为主线；M2M100-418M（词表裁后约 310MB 的估算见上文）是唯一"一个模型多语种"的备选，需自裁自验。
**未验证**：ayymen 模型的词表实际保留了哪些语言（需下载 `sentencepiece.bpe.model` 做覆盖测试）；其质量；int4 的 CT2/ONNX 路径。

### 实测分析：ayymen 词表覆盖与"裁到我们语种集需要多大词表"（2026-10-01，本机，标准库脚本）
脚本：`snow-shot-rs/tools/snow-translator/eval/vocab_coverage.py`（解析 SentencePiece protobuf，统计单字符片段并按 FLORES devtest 前 200 句算字符覆盖率）、`eval/used_pieces.py`（BPE 近似分词，统计目标语种实际用到的不同片段）。
**ayymen 词表**：15,069 个片段，**单字符片段仅 179 个**（拉丁 83、提非纳文 32、希腊 11、汉字 7、韩文 5、阿拉伯 1，其余为符号）；原版 25.6 万片段含 7,941 个单字符片段（汉字 3,719、韩文 1,290、拉丁 403、阿拉伯 178、西里尔 133 等）。字符覆盖（FLORES 前 200 句）：拉丁语种英 99.96%、法 99.13%、西 99.18%、意 99.86%、德 98.43%、葡 98.60%、印尼 99.95%（但缺带重音字母：法 `ê ô û`、西 `á ñ ú`、德 `ä ü ß`、葡 `ã`、土 `ı ş ğ`、越 `đ ư ộ ệ`；土耳其语 91.4%、越南语 77.2%）；**中文 10.3%、俄语 5.2%、阿拉伯语 6.1%、日语 5.3%、韩语 15.3%、泰语 4.0%、印地语 3.8%**。结论：该模型按柏柏尔语（拉丁+提非纳文）裁词表，**不能用于联合国六语**。（原版对中文显示 94% 是因全角标点会被分词器规范化为半角，属正常，不影响两者对比。）
**我们语种集需要的词表大小（估算，FLORES dev+devtest 共 2009 句/语种，NFKC 近似规范化，是下界）**：联合国六语并集 31,468 个片段 → 裁后词表约 **3.1 万**；14 种主流语言（六语加德、日、韩、葡、意、土、越、印尼）并集 61,588 → 约 **6.2 万**。对应参数量约 **385M / 416M**（非嵌入约 3.52 亿 + V×1024）；int8 仅权重约 **367 / 397 MiB**，int4 约 **184 / 198 MiB**（均不含激活与 KV 缓存）。**结论：词表裁剪后 int8 仍超 300MB；要进 300MB 必须 int4 或裁层数/维度。**
**裁剪后要不要再训练（技术判断，部分【推断】）**：① **词表裁剪**：删掉用不到的嵌入行并重映射 token ID，保留下来的 token 计算不变，通常**不需要重训**（vocabtrimmer 一类工作也是直接裁），但要正确处理特殊符号与语言码、分词器 ID 重映射，并用目标语种的 chrF++ 对比裁前裁后确认无下降；② **层数/维度裁剪**：**必须经过蒸馏或微调**，否则质量崩溃（NLLB-600M 本身就是从大模型蒸馏来的），需要 GPU 与平行语料，本机不现实；③ **量化**：int8 通常免训练；int4 通常要带校准的后训练量化，损失需实测；CT2 是否支持 int4 【未核实】。

### NLLB 家族与是否有更新的小模型（2026-10-01，主会话读官方仓库与 HF 页面）
- **官方发布的 5 个检查点**（【来源】fairseq nllb 分支 README `examples/nllb/modeling`，均 CC-BY-NC 4.0）：NLLB-200 MoE-128 54.5B（transformer_24_24_big）；Dense 3.3B（24_24_big）；Dense 1.3B（24_24）；**Distilled** Dense 1.3B（24_24）；**Distilled** Dense **600M**（12_12）。**官方没有 350M**。因此"NLLB-600M"即 `NLLB-200-Distilled-600M`，是 NLLB-200 系列里最小的官方版本。
- **社区的"350M"**：【来源】`dhtocks/nllb-200-distilled-350M_en-ko` 的 config 与模型卡：由 600M **把编码/解码层各从 12 砍到 3**（参数 350,537,728），`vocab_size=256206`、`d_model=1024`、ffn 4096 不变，只保留英↔韩，FLORES chrF++ 24.6，CPU 推理 1.43 秒，许可 CC-BY-NC。**是层裁剪加微调，不是词表裁剪**（此前口头猜测为词表裁剪是错的）。`lyfeyvutha` 的 350M 系列（en-km）同类。
- **Meta 的后继者**：【来源】Slator、arXiv:2603.16309、Meta AI 发布页——**2026-03-17 发布 Omnilingual MT（OMT）**，扩到 1,600+ 语言；变体 **OMT-LLaMA**（基于 LLaMA 3，1B/3B/8B）与 **OMT-NLLB**（3B 参数编码器-解码器，基于 OmniSONAR）；称 1B~8B 专门模型可匹敌 70B 通用模型。**最小也是 1B，比 NLLB-600M 还大**，int4 约 600MB 量级【推断】，**不适合 300MB 预算**。权重仓库、许可证【未核实】：按 `omnilingual` 搜 HF 只出现语音识别（ASR）模型，没有翻译权重。
- 其它相关文献（未精读）：ACL 2023《Memory-efficient NLLB-200: Language-specific Expert Pruning》（针对 54.5B MoE，可裁掉 80% 专家而质量损失可忽略，**不适用于稠密 600M**）；arXiv:2605.28042《Extracting Small Translation Specialists from LLMs by Aggressively Pruning Experts》；arXiv:2608.03480 英阿词表裁剪案例。
- **结论**：没有比 NLLB-200-Distilled-600M 更小的更新 NLLB；OMT 最小 1B，不满足预算；社区 350M 是层裁剪加微调的单语向模型，不是通用小模型。

### 多语种需求下 OPUS-MT 不止"一语向一模型"（2026-10-01，主会话读 HF 页面）
**背景**：用户要求"覆盖主流语言（最多十几种，必要时联合国六语）"，而 `opus-mt-xx-yy` 是单语向模型，需要按语向下载、非英语对经英语中转。核实到 Helsinki-NLP 还有**多语种模型**：
| 模型 | 规模 | 覆盖 | 许可 | 备注 |
|---|---|---|---|---|
| `Helsinki-NLP/opus-mt-tc-bible-big-mul-mul`（2024-10） | 247,766,051 参数（F32；transformer-big，Marian） | **469 种语言互译**，含 cmn/zho、eng、fra、spa、rus、ara、jpn、kor、deu、por；句首须加 `>>目标语言id<<` | **Apache-2.0** | 模型卡只给整体 Tatoeba 分数（multi-multi BLEU 28.1、chrF 0.5176，73531 句），**没有任何具体语对分数**；卡上写明"许多语言支持不好，大多数语言训练数据极少，许多语对根本不能用"；训练数据为 Tatoeba Challenge、HPLT v1、Bible 语料 |
| `opus-mt-tc-bible-big-deu_eng_fra_por_spa-mul` / `…-mul-deu_eng_fra_por_spa` / 各语族 `…-xxx-en`（含 `zhx-en` 汉语族→英） | 同为 transformer-big 量级（【推断】） | 德英法葡西 ↔ 多语 / 语族 | 同系列 | 未读卡 |
| `opus-mt-mul-en`、`opus-mt-en-mul`（2022） | 体积未读到 | 多语→英 / 英→多语 | Apache-2.0 | 老版本，质量【未验证】 |
**估算（【推断】，需实测）**：`mul-mul` 247.8M 参数，int8 权重约 236MiB、int4 约 118MiB（含词表嵌入；词表大小未读，裁词表可再省）；**有可能在 300MB 预算内**，且是 **Marian 架构**，现有 worker 已放行 `family=marian`，不用新增运行时。**未知数**：① 没有现成 ONNX（搜到的 ONNX 只有其它语族组合的个人转换件）→ 需要用 torch+optimum 自己导出；② 中↔英、英↔法/西/俄/阿的真实 FLORES 质量；③ 中文目标语言 id 写法（`cmn_Hans`？）；④ 下载来源：自己转换的 ONNX 需要托管。
**替代路线对比**：A 单语向模型+英语中转（质量好但要下载多个、非英语对要两次翻译，联合国六语 10 个方向约 0.8GB）；B `mul-mul` 单模型多语（Apache-2.0、一个文件、质量待测）；C NLLB-600M 自裁词表+int4（质量最可能最好，但 CC-BY-NC、工具链最重、int4 损失未知）；D 自裁 M2M100-418M（MIT，质量低于 NLLB）。**可行组合**：B 作为"多语默认"，A 的单语向模型作为"高质量可选下载"，两者同属 Marian 家族、同一 worker。
