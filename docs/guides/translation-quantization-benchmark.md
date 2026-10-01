---
title: 本地翻译模型量化实测（int8 / int4 性价比）
status: active
updated: 2026-10-01
summary: 含 int2（2 位）探针与 HQQ int4 对照：2 位在 ORT 1.28 上质量崩溃且极慢，HQQ 4 位无收益。把 mul-mul 与 NLLB-600M 词表裁剪版（6 语言 / 14 语言，多种选词表方式）导出 ONNX 并量化为 int8 与 int4，在同一批 FLORES 句子上测质量、体积、内存、延迟，给出性价比拐点；结论是 CCMatrix 词表 + int4 在质量、体积、内存上全面优于 int8，mul-mul 的中日文输出不可用
---
## 结论先行

数据来自 FLORES-200 devtest 前 30 句、beam=4、ORT 1.28.0 CPU（Intel i5-13500，Windows 10）。**30 句是小样本，差 1 分以内视为噪声**；中日文目标用字符级 chrF，其余用官方 chrF++，跨语向的绝对分数不可比，下文"质量"一律指在同一批语向上的均值差。

1. **int4（MatMulNBits，块 32、对称、RTN）比 int8（动态量化）更好：质量更高、更小，延迟差异在测量波动（见 §4，同配置重复最多差 25%）以内。** NLLB 裁剪版 int4 相对"NLLB 原版 fp32"只掉 1.3 分，int8 掉 2.9 到 3.0 分；磁盘小 150 MiB，翻译期峰值内存小 170 MiB。可能的原因（**推断**）：int8 的动态量化连激活也量化（`DynamicQuantizeLinear`，按张量一个尺度），而 int4 是仅权重量化、激活保持 fp32。旁证很薄：un6-ccm 的 8 位仅权重版（MatMulNBits 8 位）只跑了 2 个语向，eng→zho 25.5（原版 fp32 25.9，int8 动态 22.5，int4 22.8），zho→eng 51.1（fp32 52.2，int8 动态 51.2，int4 52.2），一个语向支持、一个不支持这个解释。**int4 对 int8 的结论：省约 20% 内存与 28% 磁盘，质量反而高 1.6 分，值得。**
2. **词表怎么选比用 int8 还是 int4 影响大得多。** FLORES-dev 选的初版词表在 devtest 上掉 10 到 13 分（6 语言版 -13.4，14 语言版 -10.7，int8）；用 NLLB 自己训练数据（CCMatrix）词频选的词表基本追平"拿 devtest 作弊"的 oracle（14 语言 int8：ccm 49.80 对 oracle 50.46 对原版 int8 49.96）。**现实可用的是 CCMatrix 词表。**
3. **6 语言对 14 语言：质量几乎相同，多花约 100 MiB。** 在共同的 11 个语向上 14 语言版只高 0.1 分（int4：51.43 对 51.35）；代价是词表 6.2 万到 10.1 万，磁盘 +101 MiB、翻译期峰值内存 +105 MiB（int4）、每句延迟 +0.2 s。词表每多 1000 项，磁盘与内存各多约 2.6 MiB（int4）到 2.9 MiB（int8）（嵌入在编码器、解码器、输出层各存一份）。
4. **mul-mul（OPUS-MT tc-bible-big，2.48 亿参数）输出中日文基本不可用**：目标为中文时字符级 chrF 只有 1.6 到 2.9，日文 6 到 11（同口径 NLLB 为 21 到 35）；11 个语向均值 36.2，比 NLLB 原版 fp32 低 16.5 分。它的欧洲语言之间尚可（eng→fra 61、fra→eng 59 到 61，NLLB 裁剪版 ccm 为 69 到 71 与 64 到 65），内存最小（int4 峰值增量 619 MiB），但这是模型本身的能力，不是量化造成的（torch fp32 同样输出乱码字节串）。
5. **性价比拐点**：内存-质量视角下 un6-ccm-int4 与 main14-ccm-int4 位于前沿上，二者之间的"多 8 种语言"只花 +105 MiB、质量不掉。再往上（int8、原版词表 25.6 万）只增加体积，质量更低；原版词表的延迟还翻倍。往下（mul-mul）省的内存不到 200 MiB，质量掉 15 分。延迟视角下 NLLB 裁剪版各档（1.3 到 1.7 s / 句）彼此差异在测量波动内，没有拐点可言，只有"原版词表翻倍变慢"是明确的。见 §3。
6. **推荐组合（只陈述数据，最终交主会话裁决）**：
   - **main14-ccm-int4**：质量最高（-1.27，相对原版 fp32），磁盘 484 MiB，加载后内存增量 687 MiB、翻译期峰值增量 819 MiB（默认会话），收紧会话 797 MiB，每句均值 1.66 s（60 句，默认线程，重复测量 2.07 s），支持 14 种语言。
   - **un6-ccm-int4**：只要联合国六语时更省，磁盘 383 MiB，增量 582 / 714 MiB，每句 1.47 s，质量 -1.35。
7. **"收紧"会话配置（关 CPU arena、关内存模式、4 线程）对内存几乎没用**：加载后只少 6 到 14 MiB，峰值少 20 到 30 MiB；真正的内存是权重本身加约 200 MiB 固定开销（见 §4）。收紧配置对 int8 延迟反而更好（4 线程更快，1.10 到 1.33 s），对 int4 延迟更差（1.70 到 1.97 s）。
8. **没做出来 / 受限**：NLLB 的 8 位仅权重（int8wo）在 ORT 1.28 默认配置下每句 23 s（慢 15 倍），只跑了 2 个语向的质量，未做性能遍和完整评测；加 `accuracy_level=4` 后恢复到每句约 2 s，但没有重跑质量。un6 / ccm 版本没有 fp32 参照，无法把"词表损失"与"量化损失"精确拆开（§5 给出粗估）。详见 §9。
9. **int2 探针与 HQQ int4 对照（§11）**：un6-ccm 的 2 位（RTN / HQQ，块 32 / 16）四个版本全部崩溃（相对 int4 掉 15 到 37 分，译文大面积重复），且在 ORT 1.28 CPU 上每句 40 到 65 s；2 位只比 4 位省约 50 MiB 磁盘、约 60 MiB 加载后内存。HQQ 4 位相对 RTN 4 位没有收益（5 个语向均值 -0.16），而且慢 8 到 17 倍。**int2 不值得进入候选。**

## 1. 测什么、怎么测

### 1.1 候选矩阵

| 简称 | 内容 | V（词表） | 参数量 | 备注 |
|---|---|---|---|---|
| mul-mul | `opus-mt-tc-bible-big-mul-mul`（Marian，Apache-2.0） | 69,667 | 247.8M | 句首加 `>>目标语id<<`，中文用 `cmn_Hans`（vocab 核实，另有 `cmn_Hant`、`zho`），日语 `jpn`，德语 `deu` |
| un6-ccm | NLLB-600M 裁剪，联合国六语，CCMatrix 词频前 6 万 | 61,795 | 416.0M | **现实方案** |
| main14-ccm | 同上，14 语言，词频前 10 万 | 101,245 | 456.4M | **现实方案** |
| un6 / main14（初版） | 仅由 FLORES dev 选词表 | 30,082 / 56,025 | 383.5M / 410.1M | 诊断：实际可达但有词表损失 |
| un6-oracle / main14-oracle | dev+devtest 选词表（作弊上限） | 35,883 / 66,291 | 389.5M / 420.6M | 诊断：词表足够时的上限 |
| orig | NLLB-600M 原版 | 256,206 | 615.1M | 参照 |

14 语言 = 联合国六语（zho_Hans、eng_Latn、fra_Latn、spa_Latn、rus_Cyrl、arb_Arab）加 deu、jpn、kor、por、ita、tur、vie、ind。量化版：int8 与 int4；初版 / oracle / 原版只做 int8。

### 1.2 数据与口径

- 评测集：FLORES-200 devtest 前 30 句。**核心 11 语向**（所有版本）：zho→eng、eng→zho、eng→fra、fra→eng、rus→eng、arb→eng、eng→spa，以及直译 zho→fra、rus→spa、arb→fra、fra→zho。**额外 4 语向**（只测 14 语言版、mul-mul、原版）：eng→deu、deu→eng、eng→jpn、jpn→eng。6 语言版词表里没有德语、日语，这 4 个语向记为不支持。
- 指标：中 / 日 / 韩作为目标语言时以**字符级 chrF（仅字符 1 到 6 阶）为准**，另报"词级按 CJK 逐字切分的 chrF++"；官方 chrF++（按空格切词）会把中文压低约四分之一，只作参考。其它目标用官方 chrF++。"主指标"= 该规则选出的数。实现用仓库里的 `eval/chrf.py` 与 `eval/score_results.py`（标准库，**未与 sacrebleu 对拍**）。
- 参照：已有的 `nllb600m-orig-fp32` 译文（同批 30 句、同批语向）。**fp32 不再重测**；初版 main14、main14-oracle 有自己的 fp32 译文（后者只有 7 个语向）。
- 解码：beam=4，`max_new_tokens=256`，HF `generate`（`ORTModelForSeq2SeqLM`），batch=1。注意项目 worker 默认 `no_repeat_ngram_size=3`，这里未启用，二者的译文可能略有差别。
- 裁剪版分词：`PrunedNllbTokenizer(mode="remap")`——用原版分词后把 id 映射到裁剪词表，未保留的片段落到 `<unk>`。这是**保守口径**（部署时若用裁剪后重新分词，词表损失会更小），与已有的 fp32 参照保持一致。
- mul-mul 的输出后处理：模型输出里的中日文常以字面的 `<0xE7><0x8E><0xB0>` 字节串出现，评测前用正则还原成 UTF-8 字符（`eval_quant.repair_byte_literals`）；不还原会更低。

### 1.3 机器与测量

- 机器：Intel Core i5-13500（14 核 20 线程），31.7 GB，Windows 10 Pro 19045；机器上有企业安全代理与远程桌面进程，空闲时总 CPU 约 3 到 10%。
- **质量遍**：每个版本一个进程，ORT 默认配置，`intra_op_num_threads=6`，最多 3 个进程并行，不测内存与延迟。译文确定性，线程数不影响质量。
- **性能遍**：必须在机器安静时**串行**，每个（版本 x 会话配置）单独起新进程；只翻 2 个语向（zho→eng、eng→fra）各 30 句共 60 句；运行前总 CPU 连续 5 个 1 秒采样都低于 30% 且没有 cargo / rustc / ffmpeg / 其它 python 评测（`C:\Python314\python.exe srv.py` 是别人的，忽略）；忙则每 60 秒复查。运行前先顺序读一遍模型文件，**加载耗时按热文件缓存口径**（冷读取受磁盘与杀软影响，同一模型相差可到 10 倍，无法复现）。
- 内存口径：Windows `GetProcessMemoryInfo` 的 `WorkingSetSize`（ctypes，不装包）。"基线"= 导入 torch / transformers / ORT 之后、创建会话之前的工作集（287 MiB）；"增量"= 读数减基线，更接近独立 Rust worker 里模型自身的占用，**但不含 worker 运行时自己的开销**。翻译期峰值取 20 ms 采样线程的最大值。
- 会话配置两种：**默认**（ORT 默认，线程自动）与**收紧**（`enable_cpu_mem_arena=False`、`enable_mem_pattern=False`、`intra_op_num_threads=4`；图优化级别两者都是 `ORT_ENABLE_ALL`）。这三项项目 worker 都能设置（`manifest.execution`）。
- 评测进程里 `torch.set_num_threads(1)`（束搜索的张量操作很小，不让它和 ORT 抢核）。
- 使用的 ORT：**1.28.0**，DLL 与项目 worker 下载的清单（`snow-shot/resources/ort-runtime-manifest.json`，sha256 `3d6bd02d…9855`）逐字节相同；Python 侧用 cp314 的 1.28.0 绑定（`onnxruntime_pybind11_state.pyd`）加载该 DLL。导出与量化用的是环境里的 ORT 1.30.0。

## 2. 总表

"增量"= 工作集读数 - 基线 287 MiB；括号为 默认 / 收紧 两种会话配置。延迟为 60 句（zho→eng + eng→fra）均值。"Δ原版 fp32"= 核心 11 向主指标均值与 `nllb600m-orig-fp32` 的差（共同语向上算）；"Δ自身 fp32"仅 main14 初版与 main14-oracle 有（oracle 只有 7 个语向）。

| 版本 | V | 磁盘 MiB | 核心 11 向主指标 | Δ原版 fp32 | Δ自身 fp32 | 额外 4 向均值 | 加载 s | 加载后增量 | 翻译期峰值增量 | 每句 均值 / p50 / p95 s（默认） | 每句均值（收紧） |
|---|---|---|---|---|---|---|---|---|---|---|---|
| **main14-ccm-int4** | 101,245 | 484 | **51.43** | **-1.27** | - | 51.92 | 3.3 | 687 / 673 | 819 / 797 | 1.66 / 1.54 / 2.93 | 1.97 |
| main14-ccm-int8 | 101,245 | 649 | 49.80 | -2.90 | - | 48.82 | 4.0 | 852 / 842 | 987 / 964 | 1.51 / 1.36 / 2.68 | 1.33 |
| **un6-ccm-int4** | 61,795 | 383 | **51.35** | **-1.35** | - | 不支持 | 3.2 | 582 / 572 | 714 / 685 | 1.47 / 1.39 / 2.56 | 1.70 |
| un6-ccm-int8 | 61,795 | 533 | 49.66 | -3.03 | - | 不支持 | 5.3 | 731 / 725 | 887 / 867 | 1.31 / 1.19 / 2.31 | 1.10 |
| mul-mul-int4 | 69,667 | 287 | 36.17 | -16.53 | 无 fp32 | 36.41 | 8.2（偶发） | 433 / 419 | 619 / 593 | 1.46 / 1.18 / 2.58 | 1.68 |
| mul-mul-int8 | 69,667 | 377 | 36.21 | -16.49 | 无 fp32 | 38.16 | 2.3 | 518 / 509 | 711 / 687 | 1.58 / 1.13 / 6.25 | 1.37 |
| main14-oracle-int8（诊断） | 66,291 | 546 | 50.46 | -2.24 | -1.85 | 49.84 | 4.2 | 747 / 741 | 873 / 854 | 1.70 / 1.59 / 3.12 | 1.50 |
| un6-oracle-int8（诊断） | 35,883 | 457 | 49.77 | -2.93 | 无 fp32 | 不支持 | 4.2 | 651 / 645 | 780 / 762 | 1.51 / 1.44 / 2.67 | 1.31 |
| main14-int8（初版词表，诊断） | 56,025 | 516 | 41.95 | -10.74 | -1.85 | 37.91 | 4.3 | 714 / 708 | 1018 / 988 | 1.70 / 1.50 / 2.68 | 1.64 |
| un6-int8（初版词表，诊断） | 30,082 | 440 | 39.26 | -13.44 | 无 fp32 | 不支持 | 4.3 | 634 / 628 | 942 / 918 | 1.61 / 1.41 / 2.94 | 1.46 |
| orig-int8（参照） | 256,206 | 1104 | 49.96 | -2.74 | -2.74 | 49.93 | 5.6 | 1317 / 1306 | 1446 / 1421 | 2.94 / 2.72 / 5.09 | 2.64 |

说明：

- 磁盘体积是 `encoder_model.onnx` + `decoder_model_merged.onnx`，不含分词器与映射表（几 MB）。
- mul-mul 没有 fp32 参照，**无法把量化损失与模型本身的差拆开**；它和 NLLB 在同一语向上比，差距主要来自模型本身。int4 与 int8 之间只差 0.04 分，可以认为量化对它几乎无影响。
- mul-mul-int4 的 8.2 s 加载耗时是该轮第一次加载的偶发值（复测同类版本 2 到 3 s），**以 2 到 4 s 为准**；其余版本的加载在 2.3 到 5.6 s。
- "不支持"：6 语言词表里没有德语、日语。
- 加载后增量的数据与磁盘体积高度相关，见 §4。

## 3. 性价比：前沿与拐点

### 3.1 质量对内存（翻译期峰值增量，默认会话）

| 版本 | 峰值增量 MiB | Δ原版 fp32 | 在前沿上？ |
|---|---|---|---|
| mul-mul-int4 | 619 | -16.5 | 是（内存最小，但质量不可用） |
| **un6-ccm-int4** | 714 | -1.35 | **是（拐点）** |
| mul-mul-int8 | 711 | -16.5 | 否（被 mul-mul-int4 支配） |
| un6-oracle-int8（诊断） | 780 | -2.93 | 否 |
| **main14-ccm-int4** | 819 | -1.27 | **是（质量最高）** |
| main14-oracle-int8（诊断） | 873 | -2.24 | 否 |
| un6-ccm-int8 | 887 | -3.03 | 否 |
| main14-ccm-int8 | 987 | -2.90 | 否 |
| orig-int8 | 1446 | -2.74 | 否 |

### 3.2 质量对延迟（每句均值，默认会话）

**同配置重复测量最多相差 25%，所以下面的差异多半不显著。** 默认会话：un6-ccm-int8 1.31 s、un6-ccm-int4 1.47 s、main14-ccm-int8 1.51 s、main14-ccm-int4 1.66 s，彼此在波动内；只有原版词表（orig-int8 2.94 s）明确更慢。收紧配置（4 线程）下 int8 更快（1.10 到 1.33 s），int4 更慢（1.70 到 1.97 s），int8 与 int4 的延迟差从 10% 拉大到接近 50%，超出波动，可以认为 4 线程下 int8 比 int4 快。

### 3.3 拐点在哪里

1. **int8 到 int4**：峰值内存 -173 MiB（un6）/ -168 MiB（main14），磁盘 -150 / -165 MiB，质量 +1.7 分，延迟 +10 到 12%（默认线程，在波动内；4 线程下 int4 慢约 50%）。**这一步几乎是纯赚**，不存在"再多花资源换一点质量"的区间。
2. **6 语言到 14 语言（int4）**：峰值内存 +105 MiB，磁盘 +101 MiB，延迟 +0.19 s（在波动内），质量在共同语向上 +0.08。这一步的回报是多 8 种语言（德、日、韩、葡、意、土、越、印尼；本轮只测了德、日），**代价是内存 +15%**。如果只要联合国六语，un6-ccm-int4 能省这 100 MiB。
3. **词表再加大**（14 语言词频前 10 万，再往后）：本轮没有测更大的词表；ccm 与 oracle 在 14 语言 int8 上相差 0.66 分，推断覆盖率到 99.9% 以上之后收益很小。
4. **原版 25.6 万词表**：相对 main14-ccm-int8，磁盘 +455 MiB、峰值 +459 MiB、延迟 +95%（超出波动），质量只差 0.16 分（噪声内）。**词表裁剪的收益是真实的。**

### 3.4 int4 对 int8 的细分（核心 11 向，主指标）

| 语向 | un6-ccm int8 | un6-ccm int4 | main14-ccm int8 | main14-ccm int4 | 原版 fp32 |
|---|---|---|---|---|---|
| zho→eng | 51.2 | 52.2 | 51.8 | 52.1 | 52.2 |
| eng→zho | 22.5 | 22.8 | 21.5 | 22.8 | 25.9 |
| eng→fra | 69.1 | 70.8 | 70.1 | 70.9 | 72.1 |
| fra→eng | 64.1 | 64.4 | 64.5 | 64.3 | 66.3 |
| rus→eng | 55.8 | 57.0 | 56.2 | 57.1 | 57.4 |
| arb→eng | 63.7 | 64.9 | 63.4 | 65.1 | 65.6 |
| eng→spa | 52.9 | 55.0 | 52.4 | 55.4 | 54.8 |
| zho→fra | 45.4 | 50.4 | 46.2 | 50.4 | 52.9 |
| rus→spa | 45.0 | 47.2 | 45.6 | 47.3 | 48.0 |
| arb→fra | 55.6 | 57.2 | 54.8 | 57.2 | 59.6 |
| fra→zho | 21.0 | 23.0 | 21.2 | 23.0 | 24.8 |

int4 在 un6-ccm 的 11 个语向全部高于 int8，在 main14-ccm 上 10 个高于（唯一例外 fra→eng，-0.2）；差距最大的是 zho→fra（+5.0 / +4.2）与 eng→spa（+2.1 / +3.0）。eng→zho 与 fra→zho 离原版 fp32 仍有 2 到 3 分的差（字符级 chrF），是所有裁剪 / 量化版共有的短板，见 §5。

## 4. 内存构成

- **加载后增量 ≈ 磁盘体积 + 约 195 MiB**：un6-ccm-int8 533→731、un6-ccm-int4 383→582、main14-ccm-int4 484→687、main14-ccm-int8 649→852、orig-int8 1104→1317，差值都在 194 到 213 MiB；mul-mul 是 +141 / +146 MiB。ORT 没有把权重做成共享或零拷贝，权重在内存里完整存在一份，另有固定的会话开销。
- **嵌入占大头，而且存了三份**：编码器的 Gather、解码器的 Gather、输出层 `lm_head` 的 MatMul 各有一份词表矩阵（ONNX 导出时没有去重）。词表每多 1000 项，磁盘与内存各多约 2.6 MiB（int4）到 2.9 MiB（int8）。main14-ccm-int4 的 484 MiB 里嵌入约占 260 MiB（编码器与解码器的 int8 Gather 各约 99 MiB，int4 的 lm_head 约 62 MiB，按词表 101,245 x 1024 推算）。
- 翻译期峰值比加载后再高 130 到 160 MiB（KV cache、束搜索、中间激活）；加载过程本身的瞬时峰值又比加载后高 100 到 150 MiB（读入后释放）。
- **会话选项对内存的影响**（main14-ccm-int4 / int8，默认线程，2 语向 60 句，见下表）：

| 版本 | 变体 | 加载后增量 MiB | 翻译期峰值增量 MiB | 每句均值 s | p95 s |
|---|---|---|---|---|---|
| main14-ccm-int4 | 默认（ENABLE_ALL） | 687 | 820 | 2.07 | 3.52 |
| main14-ccm-int4 | 图优化 BASIC | 688 | 823 | 2.77 | 5.12 |
| main14-ccm-int4 | 图优化 DISABLE | 688 | 823 | 2.64 | 4.62 |
| main14-ccm-int4 | 收紧 + BASIC | 674 | 794 | 2.71 | 4.71 |
| main14-ccm-int4 | 收紧 + DISABLE | 675 | 801 | 2.70 | 4.82 |
| main14-ccm-int8 | 默认（ENABLE_ALL） | 851 | 987 | 1.76 | 3.11 |
| main14-ccm-int8 | 图优化 BASIC | 855 | 988 | 2.44 | 4.46 |
| main14-ccm-int8 | 图优化 DISABLE | 855 | 999 | 2.37 | 4.26 |
| main14-ccm-int8 | 收紧 + BASIC | 845 | 967 | 2.01 | 3.72 |
| main14-ccm-int8 | 收紧 + DISABLE | 844 | 968 | 2.02 | 3.68 |

  结论：**图优化级别不影响内存**（差在 5 MiB 以内），但关优化会让延迟变差 30% 以上，保持 `ORT_ENABLE_ALL`；收紧项只少 10 到 25 MiB。另试了 `session.disable_prepacking=1`（int4）：**每句 21.8 s（慢 10 倍以上）、峰值工作集反而升到 1484 MiB（增量约 1197 MiB）**，只跑完 30 句就中止，不要用。**同一配置（默认、ENABLE_ALL）在不同时段重复测量，延迟相差 17% 到 25%**（int4 1.66 对 2.07 s，int8 1.51 对 1.76 s），而内存读数稳定在 3 MiB 以内，所以**延迟只有相差 25% 以上才算有区别，内存可以细比**。

- **可能但本轮没做的省内存办法**（均为推断，未验证）：① 把嵌入表移出 ONNX，由 worker 持有唯一一份（int8 或 int4），编码器 / 解码器改为吃 `inputs_embeds`，输出层也共用这份表，预计省 2 份嵌入，main14-ccm 约 200 MiB（int8 嵌入）；② 嵌入改用 `GatherBlockQuantized` 的 4 位（ORT 1.28 CPU 算子可用，已用小模型验证能运行），预计省一半嵌入，质量未测；③ 外部数据文件内存映射，不降低工作集读数但能降低私有内存。

## 5. 质量细节

### 5.1 CJK 目标三口径（字符级 chrF / 词级 CJK 逐字 chrF++ / 官方 chrF++，仅供对照）

| 版本 | eng→zho | fra→zho | eng→jpn |
|---|---|---|---|
| main14-ccm-int4 | 22.8 / 26.9 / 17.2 | 23.0 / 27.1 / 17.3 | 34.9 / 39.8 / 26.8 |
| main14-ccm-int8 | 21.5 / 25.5 / 17.4 | 21.2 / 25.3 / 17.2 | 28.4 / 32.5 / 22.0 |
| un6-ccm-int4 | 22.8 / 26.9 / 17.2 | 23.0 / 27.1 / 17.3 | 不支持 |
| un6-ccm-int8 | 22.5 / 26.6 / 18.2 | 21.0 / 24.9 / 17.0 | 不支持 |
| orig-int8 | 22.1 / 26.4 / 16.9 | 20.3 / 24.2 / 16.5 | 32.5 / 36.9 / 25.2 |
| orig fp32 | 25.9 / 30.1 / 19.6 | 24.8 / 29.0 / 18.8 | 无 |
| mul-mul-int4 | 2.3 / 2.5 / 2.4 | 2.4 / 2.5 / 2.9 | 6.3 / 7.8 / 4.7 |
| mul-mul-int8 | 2.9 / 2.9 / 3.4 | 1.6 / 1.9 / 1.6 | 10.8 / 12.7 / 8.7 |

官方口径的数比字符级低约 25%，三种口径的排序一致。**eng→jpn 上 main14-ccm-int4（34.9）比原版 int8（32.5）还高**，样本只有 30 句，可能是噪声。

### 5.2 14 语言版的额外语向（主指标）

| 语向 | main14-ccm-int4 | main14-ccm-int8 | main14-oracle-int8 | orig-int8 | mul-mul-int4 | mul-mul-int8 |
|---|---|---|---|---|---|---|
| eng→deu | 56.7 | 53.4 | 54.6 | 52.5 | 51.5 | 50.0 |
| deu→eng | 63.8 | 62.7 | 64.1 | 62.8 | 57.3 | 58.0 |
| eng→jpn（字符级 chrF） | 34.9 | 28.4 | 29.1 | 32.5 | 6.3 | 10.8 |
| jpn→eng | 52.3 | 50.8 | 51.6 | 51.8 | 30.6 | 33.9 |

### 5.3 词表损失与量化损失的粗估

只有 main14 初版、main14-oracle、orig 有自己的 fp32 译文：

| 版本 | 相对自身 fp32 | 说明 |
|---|---|---|
| orig-int8 | -2.74 | 纯 int8 动态量化的损失（词表原版） |
| main14-oracle-int8 | -1.85（7 个语向） | 词表几乎无损，量化损失约 1.9 |
| main14-int8（初版） | -1.85 | 同上；初版词表自身再损失约 9 分（相对原版 fp32 的 -10.74） |

由此粗估（**推断**）：int8 动态量化损失约 1.9 到 2.7 分；ccm 词表相对 oracle 约再损失 0.7 分（14 语言 int8：49.80 对 50.46）；int4 的总损失 -1.27 约等于词表 0.7 加量化 0.6。**un6 / ccm 没有 fp32 参照，这一拆分只是估算。**

### 5.4 诊断计数（全部语向合计；每个语向 30 句）

| 版本 | 句数 | 无句末标点 | 逗号等结尾（疑似截断） | 长度比<0.7 |
|---|---|---|---|---|
| main14-ccm-int4 | 450 | 31 | 25 | 38 |
| main14-ccm-int8 | 450 | 30 | 19 | 46 |
| un6-ccm-int4 | 330 | 25 | 25 | 33 |
| un6-ccm-int8 | 330 | 20 | 20 | 37 |
| orig-int8 | 450 | 27 | 16 | 48 |
| orig fp32（11 向） | 330 | 22 | 22 | 33 |
| mul-mul-int4 | 450 | 26 | 10 | 73 |
| mul-mul-int8 | 450 | 65 | 44 | 78 |

几乎所有"逗号结尾"都来自 eng→zho、fra→zho：NLLB 原版 fp32 也是 11+11 句，即原版模型就有这个习惯，不是量化或裁剪引入的。mul-mul 的长度比<0.7 高，是中日文输出短且乱。

## 6. ONNX Runtime 与算子兼容性

项目 worker 的 `ort` crate 为 `=2.0.0-rc.13`（`api-28`、`load-dynamic`），运行时 DLL 来自 `ort-runtime-manifest.json`：**onnxruntime 1.28.0（PyPI cp312 wheel 里的 CPU 版 `onnxruntime.dll`）**；本机没有其它被加载的 onnxruntime（OCR 运行时是静态链接的，不用）。用这份 DLL 实测（Python 绑定 + 同一 DLL）：

| 算子 | 用在哪 | ORT 1.28.0 CPU | 依据 |
|---|---|---|---|
| `MatMulInteger`、`DynamicQuantizeLinear`、`DequantizeLinear` | int8 动态量化、嵌入 int8 | 支持 | int8 全部版本加载并跑通 |
| `MatMulNBits`（4 位，块 32，对称，无零点） | int4 的所有 MatMul（含 lm_head） | 支持 | int4 全部版本加载并跑通 |
| `MatMulNBits`（8 位） | int8wo 试验 | 支持但**默认很慢** | 每句 23 s；设 `accuracy_level=4` 后约 2 s |
| `GatherBlockQuantized`（4 位） | 嵌入 int4（未采用） | 支持 | 小模型构造后在 1.28.0 与 1.30.0 都能运行 |
| `If`（merged decoder） | 带 KV cache 的合并解码器 | 支持 | 已是 worker 现有路径 |

- 量化产物的 IR 版本为 8 到 9，opset 17；int4 模型的节点域是 `com.microsoft`，但模型头部的 opset 导入里没有这个域，**ORT 1.28.0 仍能加载**（官方量化器的输出如此）。换更严格的运行时前需要复核。
- 没有发现不兼容的算子。仍需在 Rust worker 里真实加载一遍确认（本轮只在 Python 绑定里验证）。
- 导出与量化用的 ORT 是 1.30.0，产物在 1.28.0 上加载无问题。

## 7. Rust worker 需要的改动清单（本轮未改代码）

1. **`manifest.rs` 放行 `m2m_100` 家族**：现在只放行 `family == "marian"`。
2. **解码起始序列**：NLLB 要求解码器输入为 `[</s>(id 2), 目标语言码]`（HF 的 `decoder_start_token_id=2` 加 `forced_bos_token_id`）；worker 目前只有一个起始 token（`gen_params.start`），需要支持"强制第二个 token"，且该 token 不能被 `bad_token_ids` 屏蔽。
3. **源语言码前缀**：NLLB 编码器输入为 `[源语言码] 文本 </s>`（本仓库 HF 目录里的 `tokenizer.json` 后处理模板是 `$A </s> <unk>`，不带语言码，需要 worker 自己补）。
4. **裁剪词表的 id 映射**：评测用的是"原版分词后映射（remap）"，部署时更合适的做法是用裁剪后的 BPE 词表直接生成 `tokenizer.json`（NLLB 的 `tokenizer.json` 是 BPE 类型），并把 4 个特殊符号、语言码放在固定 id；`id_map.json` 与 `pieces.json` 已由 `prune_nllb_by_ids.py` 产出。需要实测 `tokenizers` 对该文件的行为，**本轮未验证**。
5. **`MatMulNBits` 不需要新增代码**，ORT 自带；但内存与延迟随 ORT 版本变化，升级 ORT 前要重测。
6. **语言码处理**：NLLB 的语言码是 `zho_Hans` 这类，mul-mul 是 `>>cmn_Hans<<`；`lang_tokens` 字段已能表达，需要在清单里按模型家族填。mul-mul 若要用，输出后处理要加"字面字节串还原"（见 §1.2），且中日文质量不可用。
7. **`no_repeat_ngram_size` 缺省 3**：对中文、日文、重复结构较多的文本可能有害，本轮评测没有启用，上线前要在 NLLB 上单独对比。
8. 清单里要有 `execution` 配置（`intra_threads`、`cpu_arena`、`mem_pattern`）——已有。收紧配置对内存收益很小，不建议为省内存而牺牲延迟（§4）。
9. 模型分发：每个版本是两个 onnx（编码器 + 合并解码器）加分词器与映射；整体体积 383 到 484 MiB（int4），需要下载与校验机制（项目已有）。许可：NLLB 权重是 CC-BY-NC-4.0，见 [local-translation-model-options.md](../research/local-translation-model-options.md)。

## 8. 耗时记录

| 步骤 | 实际耗时 | 备注 |
|---|---|---|
| 环境：装 onnx / onnxruntime / optimum 等 | 约 2 分钟 | 全部有 cp314 wheel（onnx 是 cp312-abi3），无需其它解释器 |
| ONNX 导出（optimum） | mul-mul 1 分 45 秒；NLLB 裁剪版 1 到 9 分 / 个；原版约 10 分 | 并行 / 与量化争抢 CPU 时变慢 |
| 量化 int8 | mul-mul 2 分 11 秒；NLLB 裁剪版 4 到 7 分（单独）；18 到 21 分（3 个并行时） | 量化器基本单线程，靠任务级并行 |
| 量化 int4（RTN，块 32） | mul-mul 约 2 分半；NLLB 裁剪版 19 到 22 分（3 个并行时） | |
| 质量遍 | 19 到 51 分 / 个（3 个并行，每个 6 线程；11 到 15 个语向 x 30 句） | 原版 int8 最慢，3045 秒 |
| 性能遍 | 每个（版本 x 配置）1.5 到 3 分钟，每版本约 3 到 6 分钟 | 含热缓存预读和安静检查 |

## 9. 风险与未验证项

- **样本小**：30 句 / 语向，核心 11 向均值的标准误约 1 分；差 1 分以内不要当结论。
- **fp32 参照不全**：un6 与全部 ccm 版本没有自己的 fp32 译文；mul-mul 没有 fp32；main14-oracle 的 fp32 只有 7 个语向。
- **未完成**：① `main14-ccm-int8wo` 量化完成但没有评测（删除了产物）；② `un6-ccm-int8wo` 只有 zho→eng（51.1）与 eng→zho（25.5）两个语向的译文（`materials	ranslate
esults
llb600m-pruned-un6-ccm-int8wo` 里只有这两个目录；默认 `accuracy_level` 下每句 23 s，评测被中止），没有性能遍；`accuracy_level=4` 的快速版只测了 3 句的速度；③ 14 语言版里除德、日以外的 6 种语言（韩、葡、意、土、越、印尼）没有评测；④ GatherBlockQuantized 4 位嵌入、嵌入去重只给了估算。
- **词表是用 FLORES 之外的数据选的**（CCMatrix，评测集不参与统计），但 CCMatrix 训练过 NLLB，对 NLLB 本身有利；换成别的领域文本，覆盖率可能不同。
- **本机内存读数的口径**：增量不含 worker 运行时自己的开销；Python 里 torch 做束搜索，与 Rust 实现的开销不同。
- **评测环境依赖**：为了装 optimum-onnx，`transformers` 从 5.18.0 降到 4.57.6，`tokenizers`、`huggingface_hub` 随之降级；已有 fp32 参照是用 5.18.0 跑的，分词行为没有发现差别，但没有逐句核对。
- **Python 3.14 与 optimum**：optimum 的 `NormalizedConfig.with_args` 在 3.14 下报错（`functools.partial` 变成方法描述符），`export_quant_onnx.py` 在导入前打了补丁；这是评测工具链的问题，不影响产物。
- **ORT 版本**：评测用 1.28.0；ORT 升级后 MatMulNBits 的速度与内存都可能变化。
- 只测了 CPU；DirectML / 核显不适用于 `MatMulNBits`（项目 OCR 文档的结论是 DirectML 对小模型更慢更占内存），本轮未测。

## 10. 复现

脚本都在 `snow-shot-rs/tools/snow-translator/eval/`（新增，不改旧脚本）；数据与日志在 `build/mt-quant/`，模型在 `E:\models\translate-eval\`，译文在 `materials\translate\results\<模型名>\<语向>\{src,ref,hyp}.txt`（均不入库）。

```powershell
# 环境（仅 mt-venv，不装全局）
build\mt-venv\Scripts\python.exe -m pip install --only-binary=:all: onnx onnxruntime optimum optimum-onnx onnxscript
# 裁剪（CCMatrix 词频前 K 个 id）
python prune_nllb_by_ids.py --nllb <原版目录> --ranked vocab-sets\un6_ccm_ranked.json --top 60000 --out <输出目录>
# 导出、量化
python export_quant_onnx.py --model <HF 目录> --out <onnx 目录>
python quantize_onnx.py --src <onnx 目录> --dst <输出目录> --mode int4        # 或 int8
# 评测（质量遍并行，性能遍串行且需安静）
python run_quant_matrix.py quality --jobs <版本名...> -P 3
python run_quant_matrix.py perf --jobs <版本名...>
python aggregate_quant.py --models <版本名...> --detail
```

CSV：`build\mt-quant\summary.csv`（每版本一行）、`pairs.csv`（逐语向）、`metrics\*.json`（含逐句延迟）。

### 环境包清单（mt-venv，Python 3.14.6，Windows）

本轮新装 / 变动：`onnx 1.23.1`、`onnxruntime 1.30.0`、`onnxscript 0.7.2`、`optimum 2.1.0`、`optimum-onnx 0.1.0`；随之装入或降级：`onnx-ir 1.0.0`、`ml_dtypes 0.6.0`、`flatbuffers 25.12.19`、`protobuf 7.36.2`、`transformers 5.18.0→4.57.6`、`tokenizers 0.23.2→0.22.2`、`huggingface_hub 1.33.0→0.36.2`、`requests 2.34.2`、`charset-normalizer 3.5.2`、`urllib3 2.8.0`。评测另用 `build\mt-quant\ort128` 目录里的 `onnxruntime 1.28.0`（来自仓库 `.cache` 里已有的 wheel，DLL 换成清单里的那一份），不在 venv 里。已有：`torch 2.14.1+cpu`、`numpy 2.5.3`、`sentencepiece 0.2.2`、`safetensors 0.8.0`。

## 11. int2 探针与 HQQ int4 对照

有边界的探针，只针对 un6-ccm（V=61,795），嵌入 Gather 仍为 int8（与现有 int4 版本一致），只改 MatMul 权重位宽 / 算法。ORT 1.28.0 上 `MatMulNBits` 的 bits 支持 2、4、8，2 位小模型能正常运行。

### 11.1 做法

- 2 位 **RTN**：ORT 自带的 RTN 路径只支持 4 / 8 位，所以 2 位 RTN 用 HQQ 量化器的同一套代码、**关掉权重优化**实现，即非对称 min-max 取整（带零点）；2 位 **HQQ**：`HQQWeightOnlyQuantConfig(bits=2)`（需要 torch，已装）。块大小 32 与 16 各一个。HQQ 不支持对称，因此 HQQ 版本都带零点。
- 对照：un6-ccm 的 **HQQ 4 位、块 32**（非对称，带零点），对比现有 RTN 4 位（对称，块 32）。
- 快速止损检查：2 个语向（zho→eng、eng→fra）各前 8 句，`max_new_tokens=96`（2 位版本解码太慢，30 句 x 256 在预算内跑不完），4 个版本并行（6 线程 x 3 到 4 个进程，所以这里的延迟与峰值内存没有参考价值）。对照 int4 取同样的前 8 句。判据：相对 int4 掉超过 10 分记"崩溃"。

### 11.2 结果

| 版本 | 磁盘 MiB | 加载后增量 MiB | zho→eng chrF++（前 8 句） | eng→fra chrF++（前 8 句） | 判定 |
|---|---|---|---|---|---|
| int4 RTN 块 32（现有） | 383 | 582（性能遍） | 49.6 | 76.0 | 基线 |
| int2 RTN 块 32 | 333 | 531 | 12.4 | 10.8 | 崩溃 |
| int2 RTN 块 16 | 432 | 619 | 12.6 | 45.4 | 崩溃 |
| int2 HQQ 块 32 | 333 | 519 | 25.4 | 37.5 | 崩溃 |
| int2 HQQ 块 16 | 432 | 620 | 34.4 | 59.5 | 崩溃（最好，仍掉 15 / 16 分） |

- **现象**：译文重复和乱码，例如 RTN 块 32 把一句输出成 "and I'm the one, and I'm the one, ..."，另一句是 "of the, of the, of the, ..." 一直重复到长度上限；HQQ 块 16 好一些，部分句子能翻出来，但仍有 "'s's's's" 这类退化和重复。HQQ 明显好于 RTN（块 32：25.4 对 12.4，37.5 对 10.8），块 16 好于块 32，但没有任何版本接近 int4。
- **2 位实际省多少**：块 32 的 2 位比 4 位磁盘 -50 MiB（-13%），加载后内存增量约 -51 到 -63 MiB（-9% 到 -11%）；块 16 因为缩放系数和零点翻倍，磁盘反而 +49 MiB、加载后 +37 MiB。省得少是因为嵌入（int8）、非权重部分和固定开销占了大头：2 位只作用在 12+12 层的 MatMul 权重（约 3.5 亿参数，4 位约 168 MiB，2 位约 84 MiB，加缩放后净省约 50 MiB）。**没有做性能遍**（全部崩溃，按约定不做），上表内存数字是并行快速检查里的加载后读数，峰值内存不可比（崩溃的译文会一直生成到上限）。
- **速度**：2 位在 ORT 1.28 CPU 上**极慢**，快速检查里每句 42 到 66 s（3 到 4 个进程并行，每个 6 线程；4 位同口径约 1.5 到 3 s）。给 MatMulNBits 加 `accuracy_level=4` 后单个版本（HQQ 块 32）仍是每句 24 到 34 s，没有救回来。ORT 对 2 位没有快速核，只有慢路径。
- **HQQ 4 位（块 32）对 RTN 4 位**：前 5 个语向（zho→eng、eng→zho、eng→fra、fra→eng、rus→eng，各 30 句）主指标均值 HQQ 53.27 对 RTN 53.43（-0.16）；逐语向 51.93 / 23.29 / 70.80 / 63.53 / 56.79 对 52.21 / 22.76 / 70.85 / 64.37 / 56.97。**更好的算法对 int4 没有收益（在噪声内，方向还略负）。** 另外它带零点，磁盘 432 MiB（+49 MiB），而且 ORT 1.28 里带零点的 MatMulNBits 走慢路径，质量遍里每句 11 到 25 s（同口径对称 RTN 约 1.5 到 3 s，慢 8 到 17 倍）。后 6 个语向没有评测（太慢，约 10 分钟一个语向，预算内停止）；`results
llb600m-pruned-un6-ccm-int4-hqq-b32` 里只有这 5 个语向，也没有性能遍。

### 11.3 结论与下一步

- **int2 不值得进入候选**：质量崩溃（最好的 HQQ 块 16 也掉 15 分以上）、速度慢 20 倍以上、只省约 50 到 60 MiB。HQQ 4 位同样不值得（无质量收益，慢，更大）。
- 若还想探 2 位，唯一合理的方向（**只是建议，没有做**）是混合精度：对敏感层（注意力投影、输出层、前几层 / 最后几层）保持 4 或 8 位，只把 FFN 压到 2 位。但 FFN 权重约 2 亿参数，即使全部 2 位也只比 4 位省约 50 MiB，而且需要自带 2 位快速核或换更新的 ORT；相比之下嵌入去重（§4，预计省约 200 MiB）和嵌入 4 位化的收益大得多，建议先做那两件事。

耗时：2 位 RTN 每个量化约 15 分（3 个并行），2 位 HQQ 块 32 约 16 分、块 16 与 4 位 HQQ 约 44 分（HQQ 需要迭代优化，用 torch，CPU 上慢）；快速检查每个版本 10 到 17 分；HQQ 4 位质量遍 5 个语向约 50 分钟。产物：`E:\models	ranslate-eval
llb600m-pruned-un6-ccm-int2-{rtn,hqq}-b{32,16}\` 与 `...-int4-hqq-b32\`；译文在 `materials	ranslateesults` 下同名目录（int2 只有 2 个语向各前 8 句，`max_new_tokens=96`）。
