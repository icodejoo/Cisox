---
title: Hy-MT2-1.8B 译文质量评测（int4，对比 NLLB-600M 14 语言 int4）
status: active
updated: 2026-10-02
summary: 腾讯 Hy-MT2-1.8B（Apache-2.0，不裁剪词表）经 optimum 补丁导出 ONNX、MatMulNBits 对称 RTN int4 后，在 FLORES devtest 前 30 句、核心 11 向上主指标 53.84，比 NLLB main14-ccm-int4 高 2.4 分，英→中高 16.9 分；代价是体积 1.3 GiB、内存约 3 倍、延迟约 5 倍
---
## 结论先行

数据：FLORES-200 devtest 前 30 句，核心 11 向，口径与 NLLB 评测完全一致（`eval/chrf.py`，CJK 目标用字符级 chrF，其余官方 chrF++；**30 句是小样本，差 1 分以内视为噪声**）。贪心解码，repetition_penalty 1.05，官方英文提示词，不调提示词。只评了 int4（我们自己的导出与量化流程）；**没有 BF16/fp32 基线、没有 GGUF**（用户决定不做），所以"int4 相对原版的损失"无法实测。

1. **质量：核心 11 向均值 53.84，NLLB 14 语言 int4 为 51.43，高 2.41 分。** 提升集中在中日文目标与中文源：eng→zho 39.7 对 22.8（+16.9），fra→zho 36.0 对 23.0（+13.0），eng→jpn 43.8 对 34.9（+8.9），zho→eng +3.5，zho→fra +3.0。**欧洲语言目标反而更差**：eng→fra 65.4 对 70.9（-5.5），arb→eng -4.2，arb→fra -2.8，其余持平或略高。
2. **NLLB 英→中的短板（截断、逗号结尾）在 Hy-MT2 上没有了**：330 句里无句末标点 0（NLLB int4 为 25）、长度比 <0.7 的 2 句（NLLB 32）、没有一句跑到 512 上限、没有"Note:"之类解释性废话、没有原样复述。
3. **代价**（机器当时有其它评测占 CPU，延迟与内存只作粗略值）：磁盘 1306 MiB（NLLB main14 int4 为 484）；纯 ORT 加载后增量 1433 MiB、翻译期峰值 1641 MiB（NLLB 同口径未做内存映射 540 / 775，做了内存映射加 beam=2 为 279 / 461）；每句均值约 10 到 12 s（6 线程，平均生成 42 个 token；NLLB 约 1.5 到 2.2 s），约慢 5 倍。内存映射对它的收益没有测。
4. **许可证：Apache-2.0**（模型目录 `LICENSE.txt`），不像 NLLB 是 CC-BY-NC-4.0，可商用。对发布方案是实质优势。
5. **能否接入 ONNX：可以，但要补丁**（见 §3）。optimum 2.1 没有 `hunyuan_v1_dense` 的导出配置，复用 Qwen3 的配置注册一份即可导出，量化器对该图无需特殊处理。
6. **是否值得作为"可选高质量包"**：值得，定位是"中日互译与英→中的高质量可选下载（质量高、可商用）"，不是默认包：它在欧洲语言上不如 NLLB，体积和内存是 2 到 3 倍，延迟 5 倍。**建议默认包仍用 NLLB main14-ccm-int4，Hy-MT2 int4 作可选包**；是否上，取决于能否接受 1.3 GiB 下载和约 1.6 GiB 峰值（未做内存映射优化的数）。
7. **与论文 Q4_K_M 损失（FLORES 英↔外 -1.15）是否吻合：无法判断。** 本轮没有 BF16 基线；论文数字是 GGUF Q4_K_M、不同打分口径、不同样本量，不能与本文分数直接比较。若要回答，需补跑 BF16 的 eng→zho 与 zho→eng 两向（transformers CPU 约 12 s / 句，60 句约 12 分钟）。

## 1. 对比表

主指标（CJK 目标为字符级 chrF，其余官方 chrF++），每向 30 句：

| 语向 | Hy-MT2 int4 | NLLB main14-ccm int4 | 差 | mul-mul int4（参考） |
|---|---|---|---|---|
| zho→eng | 55.6 | 52.1 | +3.5 | 16.2 |
| eng→zho | 39.7 | 22.8 | +16.9 | 2.3 |
| eng→fra | 65.4 | 70.9 | -5.5 | 61.0 |
| fra→eng | 65.5 | 64.3 | +1.2 | 59.3 |
| rus→eng | 56.9 | 57.1 | -0.2 | 50.9 |
| arb→eng | 60.9 | 65.1 | -4.2 | 50.1 |
| eng→spa | 55.4 | 55.4 | 0.0 | 49.2 |
| zho→fra | 53.4 | 50.4 | +3.0 | 16.7 |
| rus→spa | 49.1 | 47.3 | +1.8 | 43.7 |
| arb→fra | 54.4 | 57.2 | -2.8 | 46.0 |
| fra→zho | 36.0 | 23.0 | +13.0 | 2.4 |
| **核心 11 向均值** | **53.84** | **51.43** | **+2.41** | 36.17 |
| eng→jpn（额外，不计入均值） | 43.8 | 34.9 | +8.9 | 6.3 |

- 四向单独：eng→zho 39.7 对 22.8；zho→eng 55.6 对 52.1；eng→jpn 43.8 对 34.9；eng→fra 65.4 对 70.9。
- 中日目标另一口径（CJK 逐字词级 chrF++）：eng→zho 45.1 对 26.9，fra→zho 41.5 对 27.1，eng→jpn 48.7 对 39.8，排序一致。
- OPUS-MT 只有 mul-mul 的数据（最后一列），中日文不可用。NLLB 原版 fp32 的 11 向均值为 52.70（= 51.43 + 1.27，取自 `translation-quantization-benchmark.md`），Hy-MT2 int4 也高 1.1 分。
- 对照：eng→zho 关闭 repetition_penalty（1.0）得 39.69，开启（1.05）39.67，**没有差别**（平均生成 token 29.1，开启时该向与之同量级）。

### 诊断（330 句 = 11 向 x 30）

| 版本 | 无句末标点 | 逗号结尾 | 长度比<0.7 | 啰嗦 / 解释 | 原样复述 | 到 512 上限 | 每句均 token |
|---|---|---|---|---|---|---|---|
| Hy-MT2 int4 | 0 | 0 | 2 | 0 | 0 | 0 | 42.4 |
| NLLB main14-ccm int4 | 25 | 25 | 32 | 0 | 0 | - | - |
| mul-mul int4 | 19 | 9 | 58 | 0 | 0 | - | - |

啰嗦判据：输出含 `Note:`、`Translation:`、`Here is`、"注："、"翻译："等（`score_hymt.py` 的 CHATTY 正则）。

## 2. 体积、内存、速度

| 项 | Hy-MT2 int4 | NLLB main14-ccm int4（来自前一份文档） |
|---|---|---|
| 磁盘 | model.onnx 1306 MiB（另 tokenizer.json 9 MiB） | 484 MiB |
| 加载后工作集增量 | 1433 MiB（纯 ORT，无内存映射，Python 基线 29 MiB） | 540（基线）/ 279（内存映射） |
| 翻译期峰值增量 | 1641 MiB | 775 / 461（beam=2） |
| 每句均值 / p50 | 约 10 到 12 s / 9 s（6 线程，核心 11 向） | 约 1.5 到 2.2 s |
| 词表 | 120,818，不裁剪 | 101,245 |

- 内存口径同前（`GetProcessMemoryInfo` 工作集，20 ms 采样）。**测量时机器上有别的评测在占 CPU，单句延迟抖动很大（2 到 26 s），只作量级参考**；未做安静机器上的性能遍。
- **tie_word_embeddings 的处理**：optimum 导出时 `lm_head` 已是独立的转置副本（`[2048, 词表]`），所以最终模型里嵌入（int8 Gather，逐张量）与 `lm_head`（int4 MatMulNBits，块 32）是两份各自量化的权重，**不共享**（布局不兼容，原因同 NLLB 文档 §12.3）。fp32 导出 8.15 GB 比权重 7.2 GB 多出的约 1 GB 就是这份重复的 `lm_head`。
- 嵌入保持 int8 的做法与 NLLB 一致；没有评估嵌入 fp32 / int4 的差别（用户要求不再加变体）。

## 3. 导出与量化的做法与卡点

脚本在 `snow-shot-rs/tools/snow-translator/eval/hymt/`（新增）：

1. **导出**（`export_hymt.py`）：optimum 2.1 的任务表没有 `hunyuan_v1_dense`。办法：沿用 `export_quant_onnx.patch_normalized_config`（Python 3.14 补丁），再用 `register_tasks_manager_onnx` 把 `hunyuan_v1_dense` 注册成 `Qwen3OnnxConfig` 的子类（同为 GQA、QK-Norm、显式 `head_dim=128`），任务 `text-generation-with-past`，opset 17（optimum 提示推荐 18，实测可用）。产物是**单个** `model.onnx`（67 个输入：input_ids、attention_mask、position_ids、32 层 x {key,value} 的 past；65 个输出：logits 与 present），预填充与逐 token 解码共用同一张图（past 长度 0 即预填充）；fp32 约 8.15 GB（外部数据）。导出没有失败；动态 RoPE（alpha 缩放）trace 时按常量处理，因最大位置 262144 远大于评测长度，无影响。
2. **量化**（`quantize_hymt.py`）：先把嵌入 Gather 量化为 int8（`quantize_dynamic`，模型 >2GB 所以中间文件用外部数据），再 `MatMulNBitsQuantizer`（bits=4、块 32、对称、RTN，覆盖全部 MatMul 含 lm_head）。没有 merge 步骤（单图）。CPU 上约 5 分钟，峰值内存约 15 GiB；产物 1306 MiB，未超 2GB protobuf 限制，可单文件。
3. **纯 ORT 生成**（`ort_gen.py`，只依赖 onnxruntime、numpy、tokenizers，不导入 torch）：手工套 chat 模板（`<｜hy_begin▁of▁sentence｜><｜hy_User｜>…<｜hy_Assistant｜>`）、预填充、KV cache 回传、贪心、与 HF 同公式的 repetition_penalty、遇 eos 120020 停止。
4. **分词注意点**：transformers 4.57.6 加载该分词器时报"incorrect regex pattern（Mistral）"警告。若照提示加 `fix_mistral_regex=True`，会把 `tokenizer.json` 里 `\p{N}{1,3}` 的数字分组改成逐位切分，330 句里 82 句的提示词 token 序列与官方不同。**这是误报，不要加该参数**：默认 HF 路径与直接用 `tokenizers` 读 `tokenizer.json`（Rust worker 的做法）在 330 句上逐 id 完全一致。
5. **transformers 4.57.6 能加载与生成**：README 写需要 5.6，实测 4.57.6 的 `hunyuan_v1_dense` 可加载，`generate` 需丢掉 `token_type_ids`；没有升级 venv。

## 4. 一致性自检（不计入对比表）

eng→zho 前 2 句：transformers BF16 与 fp32 输出逐字一致；ONNX int4 + 纯 ORT 的输出语义一致、措辞略有差别（第 1 句"它们没有糖尿病，而之前它们是患有糖尿病的"对 BF16 的"它们原本患有糖尿病，但现在已经不再患糖尿病了"）。符合 int4 量化预期，说明导出图与生成循环没写错，但**只有 2 句，不能当作量化损失的估计**。

## 5. 接入 ONNX 的可行性与待办

- 可行：用到的算子都是 NLLB 已验证过的（MatMulNBits、Gather int8 等）。但本轮用的是 venv 的 ORT 1.30.0，**不是项目锁定的 1.28.0，也没有在 Rust worker 里加载过**，需要补测。
- worker 需要的新能力（均未实现）：① decoder-only 家族；② 单图预填充 + 逐 token 的 KV cache 循环（`position_ids`、GQA 4 个 KV 头）；③ chat 模板拼接与 eos 120020；④ repetition_penalty；⑤ 解码上限 512。
- 内存：1433 / 1641 MiB 是**未优化**数。外部数据加内存映射对 NLLB 省了 262 MiB（嵌入几乎不驻留），Hy-MT2 的嵌入约 236 MiB，预期同理有收益，**未测**。
- 速度：每 token 要过 32 层 1.8B 参数，6 线程约 4 到 5 token/s（机器负载下）。短句尚可，长段落要等，适合流式输出。

## 6. 未完成与风险

- 没有 BF16/fp32 基线，给不出 int4 相对原版的损失，也无法与论文 -1.15 对照。
- 样本小：每向 30 句，11 向均值标准误约 1 分。eng→zho、fra→zho 的 +13 到 +17 远大于噪声；欧洲语言的 -2.8 到 -5.5 大概率是真的，但 eng→fra 单向只有 30 句，需更多句确认。
- 延迟、内存是在被其它评测占用 CPU 的机器上测的，只作量级。
- 只测了核心 11 向与 eng→jpn，其余语言（德、韩、葡、意、土、越、印尼等）没测。
- 评测用 ORT 1.30.0，没有在 1.28.0 上跑。

## 7. 复现

```powershell
# 全部在 build\mt-venv，不装全局包；目录 snow-shot-rs\tools\snow-translator\eval\hymt
python export_hymt.py   --model E:\models\translate-eval\hy-mt2-1.8b --out E:\models\translate-eval\hymt2-1.8b-onnx-fp32
python quantize_hymt.py --src E:\models\translate-eval\hymt2-1.8b-onnx-fp32 --dst E:\models\translate-eval\hymt2-1.8b-int4
python ort_gen.py --dir E:\models\translate-eval\hymt2-1.8b-int4 --version hymt2-1.8b-int4 [--pairs eng_Latn-jpn_Jpan] [--rep-penalty 1.0]
python score_hymt.py --models hymt2-1.8b-int4 nllb600m-pruned-main14-ccm-int4
```

译文在 `materials\translate\results\hymt2-1.8b-int4\<语向>\{src,ref,hyp}.txt`（不入库），逐句 token 数与耗时在同目录 `hyp.partial.jsonl`，内存与延迟在 `results\_metrics\*.json`。`hf_eval.py` 是 transformers CPU 生成脚本，只用于 2 句自检，没有跑全量。

## 8. 体积与内存优化探针（2026-10-02，只在现有 int4 ONNX 上做图手术，没有重导 fp32）

口径：`mem_probe.py`，eng→zho 前 5 句、max_new 80、4 线程、Idle 优先级，加载后工作集 / 翻译峰值（增量，Python 基线 29 MiB）。机器有其它负载，延迟只作量级，内存读数有 ±100 MiB 级抖动。质量用 FLORES 前 30 句，只测 eng→zho（基线 39.7）与 zho→eng（基线 55.6），均未扩到 eng→jpn。

| 方向 | 体积 | 加载后 / 峰值 MiB | eng→zho / zho→eng | 结论 |
|---|---|---|---|---|
| 基线（单文件 int4） | 1306 MiB | 1408 / 1496 | 39.7 / 55.6 | - |
| 1 外部数据 + 内存映射（`make_ext.py`） | 1306 MiB | 953 / 1261（私有内存 1514 到 1064） | 5 句译文与基线逐字一致 | **推荐，无损** |
| 1' 关 ORT 预打包（`session.disable_prepacking`） | 同上 | 99 / 2150 | - | 放弃：每句约 100 s（慢 10 倍），峰值反而更高 |
| 2 词表裁剪 120818 到 32288（dev 语料覆盖）| 1023 MiB | 847 / 1102 | 5 句里"糖尿病"出错（出现韩文字符） | 放弃，质量坏 |
| 2' 词表裁剪到 53449（再并入旧 id < 40000） | 1090 MiB | 1082 / 1131 | 35.0 / 未测 | 放弃：eng→zho -4.7；人名地名（诺贝尔、瑞典）等中文词缺行 |
| 3 嵌入与 lm_head 共享 int4（完整词表，`prune_hymt.py --share --head-k 120818`） | 1068 MiB（-238） | 1073 / 1379 | 38.9 / 55.6 | 质量在噪声内，但内存没有比 ext 更低，不推荐叠加 |
| 2'+3（裁剪 53449 + 共享） | 986 MiB | 923 / 1177 | 34.9 / 未测 | 同 2'，质量掉 |

要点：
- **内存收益主要来自内存映射**：峰值 -235 MiB，加载后 -455 MiB。内存映射后 ORT 对 MatMulNBits 仍会预打包成私有副本，所以峰值仍约 1.26 GiB，约等于全部权重；嵌入只碰用到的行，体积裁剪与共享对"驻留内存"几乎无帮助（上表 2/3 的内存差别在噪声内）。
- **词表裁剪的真正风险在中文**：Hy-MT2 的 12 万词表里 4.5 万是汉字词，FLORES dev 只覆盖其中 7.8 千。靠 BPE 序的"头部"补不够，漏掉的词模型不会改用字节拼写，直接换成别的 token。要做得安全需要大规模中文语料统计或保留全部汉字类 token（保留约 11 万行中的 7 成，体积收益只剩 100 MiB 级），本轮不推荐。
- 嵌入共享的图内实现可行：从 lm_head 的 `[N,64,16]` uint8 块 Gather 行，Div/Sub 拆高低半字节，减 8 乘缩放，输出替换原 DequantizeLinear 的输出（见 `prune_hymt.py`）。质量无损，磁盘 -238 MiB，但本轮没测出内存收益。
- 4 更激进量化（嵌入 int4 即方向 3 已覆盖；MLP 更大块 / 更低比特）：需要从 HF 权重按层重量化，时间不够，没做。
- 5 KV cache：GQA 4 个 KV 头 x 128 x 32 层，fp32 每 token 32 x 2 x 4 x 128 x 4 B = 128 KiB，512 token 上限约 64 MiB，提示词加译文通常 < 150 token 约 19 MiB，占比小；fp16 可再减半但 ORT 图需改，收益 < 10 MiB，不做。
- Rust worker 接入需要的改动（只说明）：外部数据版 `model.onnx` + `model.onnx_data` 需放在同目录，`commit_from_file` 自动映射；其余同第 5 节。若将来用裁剪版，换 `tokenizer.json` 即可（新分词器直接输出新 id），eos id 读 `pruned.json`。
