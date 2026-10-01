---
title: 本地 OCR 模型/方案选型调研
status: active
updated: 2026-10-01
summary: PP-OCRv6 官方资料核实；评估 RapidOCR、PaddleOCR-VL、Surya、EasyOCR、Tesseract、docTR、VLM 类方案；结论是默认 v6 small 合理，不引入 PP-OCR 以外方案，给出最小验证计划
---
# 本地 OCR 模型/方案选型调研

标记约定：【来源】= 读到公开页面（链接见文末）；【推断】= 我的判断；【未验证】= 没有证据，不能当事实。检索工具可用，但多数数字来自搜索摘要与网页转述（WebFetch 经小模型摘要），**关键数字落地前请对照原论文/模型卡复核**。

## 0. 结论先行

1. **PP-OCRv6 官方真实存在**，由 PaddleOCR 团队发布（HF 博客日期 2026-06-22，PaddleOCR 仓库说明写 2026-06-11，两处日期不一致，以官方仓库/论文为准再核）。tiny/small/medium 三档，Apache-2.0，官方提供 ONNX。项目清单里的 v6 来源是 `rapid-ocr-rs` 的 `model_registry.rs`（`multi_PP-OCRv6_*`）与 modelscope 上 `mgchao/SnowShotOCR` 的转存文件，权重是官方 v6 的 ONNX 版本【推断：文件名与官方档位一致，未逐字节比对】。
2. **默认 v6 small 是合理选择**：官方基准里 small（检测 84.1 / 识别 81.3）已**高于 v5 server**（81.6 / 78.1），参数量只有 7.7M；medium 只再多 +2.1 / +1.9，体积却是 small 的 4~5 倍（本项目清单 139MB 对 31MB）。按"性能 > 内存 > 识别率"，small 是甜点档。
3. **不建议为默认档引入 PP-OCR 以外方案**。候选里要么要新运行时（VLM、llama.cpp、PyTorch），要么中文不如 PP-OCR（Tesseract、EasyOCR、docTR），要么许可/体积不合（Surya 权重）。
4. **唯一值得保留观察的**：PaddleOCR-VL（1.x，0.5~0.9B）作为将来"高精度可选档"，但它在 Intel 核显上的速度与现成 ONNX 路径都**未验证**，且需 llama.cpp 级新运行时，现在不做。
5. **我们的真实短板不在选模型，在读序和小字**：样片 shupai 72.6% CER 主要是顺序与多读小字，不是识别错。优先做"后处理/读序"与更公平的核对稿，再谈换模型。
6. 低成本可验证项：**tiny 档**（官方 CPU 上比 v5 mobile 快约 3.9 倍）值得在 `snow-ocr-compare` 里跑一遍，看真实样片 CER 掉多少；若掉得少，可作"极速档"。

## 1. PP-OCR 家族官方数据

### 1.1 v6 / v5 对比（来源：PP-OCRv6 论文）

| 档位 | 检测 Hmean | 识别准确率 | 参数量 | CPU 速度（Xeon，s/张，OpenVINO / ORT） |
|---|---|---|---|---|
| v6 tiny | 80.6 | 73.5 | 1.5M | 0.20 / 0.22 |
| v6 small | 84.1 | 81.3 | 7.7M | 未查到 |
| v6 medium | 86.2 | 83.2 | 34.5M | 1.40 / 3.31 |
| v5 mobile | 75.2 | 73.7 | 约 5~21M（论文转述不精确） | 0.78 / 0.61 |
| v5 server | 81.6 | 78.1 | 约 21M | 7.30 / 6.36 |

- 以上为官方内部综合评测集的加权平均，**不是我们的样片**；Xeon 服务器 CPU 数字不能直接换算 Intel UHD 770 核显【推断】。
- v6 相对 v5：medium 比 v5 server 检测 +4.6、识别 +5.1；新增场景（PCB、CAD 图、数码管、点阵字符）；日文 +16.8%、古籍 +12.0%、屏幕文字 +14.4%；单模型覆盖 50 种语言（简/繁中、英、日、46 种拉丁语系），v5 论文称 4 种语言；骨干 PPLCNetV4，检测 RepLKFPN，识别 EncoderWithLightSVTR；官方提到后续会出 large 档。
- v6 tiny 识别分项（模型卡）：印刷中文 86.7、印刷英文 88.4、屏幕文字 71.2、证卡 80.5、手写英文 39.3。**small 的分项、竖排与场景文字的分项数字未查到**。
- 竖排：模型卡称通过"文本行方向分类"可选模块支持。**我们的 worker 是否启用该模块、v6 对竖排版面的真实效果：未验证**。
- v5（官方文档）：相对 v4 端到端 +13 个百分点；覆盖手写、竖排、生僻字、繁体、日文的综合基准平均 53.0%（v4）到 80.1%（v5）。
- v4：本次未单独检索官方数字，**查不到就不写**。项目清单里 v4 体积 15.6MB（small）与 204MB（medium），仅作历史档。
- 许可：模型卡与仓库均为 Apache-2.0，与 GPL-3.0 兼容（Apache-2.0 代码可进入 GPL-3.0 项目）。

### 1.2 本项目清单体积（来源：`docs/research/system-ocr-translate-backends.md` §1.1）
tiny ≈6.3MB（清单名 extra_small）、small(v6) ≈31MB、medium(v6) ≈139MB、v5 small ≈21MB、v5 medium ≈173MB、v4 small ≈15.6MB、v4 medium ≈204MB。

## 2. PP-OCR 之外的候选

| 方案 | 体积/内存 | CPU/核显可行性 | 中英繁竖排场景 | 许可（GPL-3.0 兼容） | ONNX 现成权重 | 集成成本 |
|---|---|---|---|---|---|---|
| **RapidOCR**（ONNX 封装，Apache-2.0） | 即 PP-OCR 的 ONNX，同上 | 与现状同 | 与对应 PP-OCR 档一致 | Apache-2.0，兼容 | 是（就是转换 PP-OCR 权重） | **本项目已在用其思路**（`rapid-ocr-rs`），无增量 |
| **PaddleOCR-VL 1.5/1.6**（0.5~0.9B VLM，文档解析） | 0.9B；Q4 语言模型约 300MB，推理约 1.5GB 内存（二手来源） | 已并入 llama.cpp（b8110，2026-02）；**Intel 核显速度未查到**，VLM 逐 token 解码，CPU 上预期明显慢于两阶段模型【推断】 | 声称 109 语言；文档解析 OmniDocBench v1.6 96.3；**竖排/街景招牌/繁体无具体数字** | Apache-2.0 | 有 GGUF；**ORT 现成 ONNX：未查到** | 需 llama.cpp 级新运行时与视觉塔，**新依赖，重** |
| **PP-StructureV3** | 多模型流水线 | 偏服务器 | 面向版面/表格/文档转 Markdown，不是截图取字 | Apache-2.0 | 部分 | 过重，目标不符 |
| **Surya** | 未查到精确体积 | 官方说 CPU 比 GPU 慢约 50 倍（二手）；M5 Max 上 1.9 页/秒对 PaddleOCR 6.8 页/秒（二手，第三方博客） | 90+ 语言，退化图稳；版面/阅读顺序是强项 | **代码 Apache-2.0，权重为修改版 OpenRAIL-M，有营收/融资门槛与竞争性使用限制** | 未查到 | 需 PyTorch；许可与 GPL 分发有疑点，**不建议** |
| **EasyOCR** | 未查到 | CPU 慢于 PP-OCR【二手】 | 支持简繁中；二手评测印刷体 94.8% 对 PaddleOCR v4 97.1% | Apache-2.0 | 有社区转换（asmud/EasyOCR-onnx），质量未验证 | 原生 PyTorch，不如现状 |
| **Tesseract** | chi_sim/chi_tra 数据包各数十 MB（未核实具体值） | 纯 CPU，快 | 有 `chi_sim_vert/chi_tra_vert`；场景文字弱；中英混排二手评测 71% 对 PaddleOCR 89% | Apache-2.0 | 无需 ONNX，C++ 库 | 新增 C 库与 tessdata；中文质量明显落后，**不推荐** |
| **docTR** | 未查到 | 有 OnnxTR 封装 | 官方模型以拉丁文字为主，**中文支持未查到证据** | Apache-2.0 | 有（OnnxTR） | 中文不明，不适合 |
| **MMOCR/其它开源 CJK 检测识别** | 未查到 | MMOCR 已基本停更【推断，未核实】 | 不如 PP-OCR 的持续迭代 | Apache-2.0 | 需自转 | 无收益，**未深入调研** |
| **VLM：Qwen2.5-VL / Qwen3-VL、GOT-OCR2、olmOCR(7B)、MinerU2.5(1.2B)** | 1.2B~7B+ | MinerU/olmOCR 推荐 vLLM+GPU，MinerU2.5 在 A100 上约 2.12 fps；核显不现实 | 论文称 PP-OCRv6 medium 识别 83.2 高于 Qwen3-VL-235B 74.9，检测远超 VLM（46.8）；VLM 有幻觉（论文幻觉项 PP-OCRv6 93.2 对 80.6） | Qwen/olmOCR 等多为 Apache-2.0 或 MIT（**逐个未核实**） | 无现成 | 需 transformers 或 llama.cpp，**不可行** |

要点：
- 我们要的是"截屏取字、带框、快、省内存"，不是"文档转 Markdown"；VLM 与文档解析模型的强项（表格、公式、版面）用不上，代价（体积、解码延迟、幻觉）全在。
- 官方论文自己的对比（来源 arXiv 2606.13108）也说明：在其评测集上，小型专用 OCR 在检测与识别上都高于大 VLM；**但这是百度自家评测，需保留怀疑**。

## 3. 与项目约束逐条对照

| 方案 | 能否直接进现有 ORT worker | 新运行时 | Intel 核显 | 备注 |
|---|---|---|---|---|
| PP-OCRv6 tiny/small/medium | 能，零新增（已在用） | 无 | 可行（本机实测 small 热启动 0.1~1s/张） | |
| 其它 PP-OCR 版本（v5/v4） | 能 | 无 | 可行 | v6 small 已全面压过 v5 server，v5/v4 仅作回退 |
| RapidOCR | 已是同一路线 | 无 | 同上 | |
| EasyOCR / docTR（经 ONNX） | 理论上可，但要新写检测/识别后处理 | 无，但开发量大 | 可行 | 质量无优势，不值得 |
| Tesseract | 否 | 新 C 库 | CPU 可行 | 中文质量落后 |
| PaddleOCR-VL（GGUF） | 否 | llama.cpp | **未验证，预期慢** | 仅作未来可选档 |
| Surya、MinerU、olmOCR、Qwen-VL、GOT-OCR2 | 否 | PyTorch/vLLM 等 | 不现实 | Surya 另有权重许可问题 |

## 4. 推荐

### (a) 默认 small 是否合适
合适，保持。依据：官方基准 small 已超过 v5 server；本机真实样片 CER 17.8% 对系统 OCR 的 57.0%；体积 31MB，worker 内存 200~500MiB。medium 在官方基准里只多 2 个点左右，体积 4.5 倍，Xeon 上 ORT 耗时 3.31s 对 tiny 的 0.22s（small 未查到），按性能优先不值得做默认。**tiny 值得加进"极速档"验证**，已在清单里（6.3MB）。

### (b) 是否引入 PP-OCR 之外方案
**不引入。** 没有任何候选同时满足：中文质量不低于 PP-OCR、无新重依赖、核显可用、许可无障碍。PaddleOCR-VL 是唯一值得继续观察的，条件是（一）核显实测速度可接受，（二）有稳定的 ONNX/ORT 或轻量运行时路径，（三）竖排/场景/繁体有明显收益。三条目前都没证据。

### (c) 最小验证计划
1. **tiny 对比（零成本，先做）**：清单已内置 tiny；用 `snow-ocr-compare/scripts/run-materials.ps1 -Backends local` 分别以 tiny/small/medium 各跑一遍 7 张样片，记录 CER、热启动耗时、worker 峰值内存。看 tiny 相对 small 的 CER 增量与耗时降幅。
2. **衡量口径修正**：先让核对稿经人工抽查，再把 shupai 这类竖排样片的"读序/多读小字"从 CER 里拆出来（逐行集合匹配 + 顺序单独算），避免 72.6% 误导选型。
3. **竖排验证**：确认 worker 是否启用文本行方向分类；对 shupai 与繁体《心经》竖排样片开/关该模块各跑一次。【未验证】
4. **PaddleOCR-VL（仅在需要时，非本季度）**：取官方 `PaddleOCR-VL-1.6-GGUF`（Apache-2.0），用 llama.cpp 官方二进制，在同一台 UHD 770 上对 7 张样片跑 `OCR:` 提示，记录每张耗时、内存、CER；若单张 >5~10 秒或内存 >1.5GB，直接否决。不需要转 ONNX，不改工程；只用独立目录与脚本，输出同样的 `texts/<图>.<后端>.txt`，再用现有脚本算 CER。【阈值是我的建议，不是来源】
5. 若以上第 1 项显示 tiny 与 small 差距很小，再考虑把默认下调；否则保持 small。

## 5. 开放问题
- PP-OCRv6 发布日期两处不一致（06-11 或 06-22），待在官方 release 页确认。
- small 档的 CPU 延迟、竖排与场景文字分项数字未查到；论文只给了 tiny/medium 的速度。
- 项目 modelscope 转存的 v6 ONNX 与官方 `PP-OCRv6_*_onnx` 是否逐字节一致，未验证。
- v4 官方数字、Surya/EasyOCR/Tesseract/docTR 的精确体积与内存、MMOCR 现状：本次未查到或未深入。
- 官方评测集偏重 Xeon 与自家场景，是否代表 Intel 核显 + 街景招牌，需要自家样片说话。
- 街景招牌（场景文字）目前没有样片，建议补几张再下结论。

## 6. 来源
- PP-OCRv6 论文：https://arxiv.org/html/2606.13108v1 （PDF：https://arxiv.org/pdf/2606.13108）
- PP-OCRv6 HF 博客：https://huggingface.co/blog/PaddlePaddle/pp-ocrv6
- PP-OCRv6 tiny rec 模型卡：https://huggingface.co/PaddlePaddle/PP-OCRv6_tiny_rec
- PaddleOCR 仓库与发布页：https://github.com/PaddlePaddle/PaddleOCR 、https://github.com/PaddlePaddle/PaddleOCR/releases
- PP-OCRv5 文档：https://paddlepaddle.github.io/PaddleOCR/main/en/version3.x/algorithm/PP-OCRv5/PP-OCRv5.html
- PaddleOCR-VL-1.6-GGUF：https://huggingface.co/PaddlePaddle/PaddleOCR-VL-1.6-GGUF
- llama.cpp 支持 PaddleOCR-VL 的 issue：https://github.com/ggml-org/llama.cpp/issues/16627
- PaddleOCR-VL 本地运行指南（二手）：https://insiderllm.com/guides/paddleocr-vl-local-document-ocr/
- RapidOCR：https://github.com/rapidai/rapidocr
- Surya：https://github.com/datalab-to/surya 、权重许可 https://huggingface.co/datalab-to/surya-ocr-2/blob/main/LICENSE
- Surya/docTR/PaddleOCR 速度对比（二手博客）：https://contracollective.com/blog/local-ocr-document-extraction-apple-silicon-m5-max-2026
- Tesseract tessdata：https://github.com/tesseract-ocr/tessdata
- docTR / OnnxTR：https://github.com/mindee/doctr 、https://github.com/felixdittrich92/OnnxTR
- EasyOCR ONNX（社区）：https://huggingface.co/asmud/EasyOCR-onnx
- 开源 OCR 综述（二手）：https://imagetotable.ai/blog/best-open-source-ocr-tools-2026
- MinerU2.5：https://huggingface.co/opendatalab/MinerU2.5-2509-1.2B ；olmOCR 2：https://huggingface.co/allenai/olmOCR-2-7B-1025
- 本项目：`docs/guides/ocr-samples.md`、`docs/research/system-ocr-translate-backends.md`
