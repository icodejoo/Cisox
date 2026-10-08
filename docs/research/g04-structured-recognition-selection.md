---
title: G04 表格 / LaTeX / Markdown 识别转换选型
status: active
updated: 2026-10-08
summary: 表格用 SLANet+ 经 RapidTable 思路接 ORT 独立 worker；公式先走 PP-FormulaNet_plus-S（备选 RapidLaTeXOCR）；Markdown/HTML 沿用自定义 OpenAI 兼容模型通道，不做本地模型
---
# G04 表格 / LaTeX / Markdown 识别转换选型

标记：【来源】读到页面；【推断】我的判断；【未验证】无证据。多数数字来自 WebFetch 小模型摘要，**落地前请对照模型卡复核**。

## 0. 结论先行

1. **旧版并没有本地模型**：`latex_extract` / `table_extract` 走 Snow Shot 云端 API（`snow_shot/src/network/snowshotapiclient.cpp`，注释里写明与服务端 RapidLaTeXOCR 的 max_width/max_height 对齐）；Markdown / HTML 转换是把图片发给用户配置的聊天模型（同文件，提示词在 L58~86，设置页 `customaimodelssettingswidget.cpp` 里有"允许该模型转 Markdown/HTML"开关）。云端 `/api/v1/*` 已按计划弃用。
2. **Markdown / HTML：不做本地模型**，直接复用 ADR-5 的 `OpenAiCompatible` 通道（`snow-net`，reqwest 已有）+ 旧版提示词。本地方案（PP-StructureV3 全家桶、MinerU、docling）都要版面+多模型流水线或 PyTorch，违反"少依赖/不常驻"，且体积以 GB 计。
3. **表格：SLANet_plus（ONNX 约 6.8MB）+ 现有 PP-OCR 的文字框**，复用现有 ORT worker 思路，新增依赖为零。
4. **公式：PP-FormulaNet_plus-S（官方 Apache-2.0，约 248MB，中文 BLEU 53.3）优先；RapidLaTeXOCR（MIT，ONNX，仅英文印刷体强）作备选**。ONNX 现成权重的出处是本调研最大的不确定项（见 §3）。
5. **分期：先表格（体积小、零新依赖、已有 OCR 框），再公式，Markdown/HTML 与表格第一期并行（只是接线）。**

## 1. 已有基础（避免重复）

- ONNX Runtime 已在 `snow-translator`（`ort =2.0.0-rc.13`，load-dynamic）与 `snow-stt` 使用，OCR worker 用 PP-OCRv6（见 [local-ocr-model-options.md](local-ocr-model-options.md)）。新模型一律走"独立进程 worker + 按需启动 + 空闲退出"，同翻译 worker 模式。
- ADR-5（迁移方案 §253~）：模型不打包、用户可下载放目录、懒加载、未配置显示引导卡片；T5 登记"表格/公式本地模型，可选"。
- 现有 `snow-ocr-protocol` 已有文本框协议，表格识别可直接消费其 `OcrTextBox`，不另建 OCR。

## 2. 候选逐项比较

### 2.1 表格结构识别

| 候选 | 许可 | 体积 | 延迟 | 运行时 / 接入 | 中文 | 评价 |
|---|---|---|---|---|---|---|
| **SLANet_plus**（PaddleX，RapidTable 提供 ONNX） | 代码 Apache-2.0；权重许可【未验证，PaddleOCR 系惯例 Apache-2.0】 | 约 6.8MB | 约 0.15s/张（RapidTable 文档，CPU，机型未注明） | ORT 直接跑；输出 HTML 结构标记 + 单元格框，再与 OCR 文字框按位置匹配填字 | `ppstructure_zh` 7.4MB 另有；plus 对无线/复杂表更稳 | **推荐**：精度 63.69%（官方表格）、体积最小 |
| SLANeXt（有线/无线分模型） | 同上 | 未查到 | 未查到 | 同上；官方称有线版 69.65% | 同 | 精度更高，**体积/ONNX 现成性未查到**，二期候选 |
| SLANet（旧） | 同上 | 7.3~7.8MB | 同 | 同 | 有 | 59.52%，被 plus 取代 |
| Unitable（RapidTable 内） | 未查到 | 500MB | CPU 6s | PyTorch | 未查到 | 否决：体积、PyTorch |
| PP-StructureV3 / PaddleOCR-VL | Apache-2.0 | 多模型 / 0.9B | 偏服务器 | 新运行时 | 好 | 否决，理由同 OCR 调研 |
| MinerU / docling | MinerU 许可条款近年有变动【未验证，勿当作可商用/可 GPL 分发】；docling MIT【未验证】 | GB 级 | GPU 取向 | PyTorch | — | 否决 |

### 2.2 公式识别（LaTeX）

| 候选 | 许可 | 体积 | 延迟 | 运行时 / 接入 | 中文 | 评价 |
|---|---|---|---|---|---|---|
| **PP-FormulaNet_plus-S** | Apache-2.0（模型卡） | 248MB | GPU 约 180~190ms；CPU 未查到 | 自回归解码（编码器+解码器，需实现 KV cache 解码循环 + LaTeX 词表）；**官方只确认 paddle/transformers 引擎，ONNX 需自转或另找** | 英文 BLEU 88.71，**中文 53.32** | 推荐起点 |
| PP-FormulaNet_plus-M / L | Apache-2.0 | 592 / 698MB | GPU 约 1.0 / 1.5s | 同 | **中文 89.76 / 90.64** | 中文公式场景再上；体积过大，只作"可选包" |
| PP-FormulaNet-S（旧） | Apache-2.0 | 224MB | 182ms | 同 | 中文 45.71 | 被 plus-S 取代 |
| **RapidLaTeXOCR**（pix2tex 的 ONNX 版） | MIT | 约 99MB（对应官方表里 LaTeX_OCR_rec 99MB，【推断】） | 示例图约 0.48s | ORT / OpenVINO，**仓库已转好 ONNX**；推理代码是 Python，需自行改写 Rust | **官方表中文 BLEU 39.96、英文 74.55**，弱 | **备选**：接入最省事、旧版服务端就用它（行为对齐旧版） |
| UniMERNet（tiny/small/base） | Apache-2.0 | 441MB / 773MB / 1.3GB | 官方 PP 表里 1.3s（GPU，base 级 1530MB） | PyTorch，ONNX 未提及 | 弱 | 否决：体积大、无 ONNX |
| LaTeX-OCR 原版（pix2tex） | MIT | 同上 | — | PyTorch | — | 由 RapidLaTeXOCR 覆盖，不单独选 |

### 2.3 Markdown / HTML

| 候选 | 结论 |
|---|---|
| 自定义聊天 / 视觉模型（OpenAI 兼容端点，如 Ollama、LM Studio） | **选用**。旧版就是这么做的，零新增依赖、无权重分发、用户自选模型与隐私边界；未配置时显示引导卡片 |
| 本地 PaddleOCR-VL（GGUF）等 | 需 llama.cpp 级新运行时，OCR 调研已判"未验证、不做" |
| 纯规则：OCR 框 → 段落 → Markdown | 可作无模型兜底（表格第一期做完后，表格 HTML 到 Markdown 表格是纯字符串转换），不属于识别模型 |

## 3. 推荐方案与备选

**推荐**：
- 表格：`SLANet_plus.onnx` + 现有 PP-OCR 框，worker 内完成"结构预测 → 单元格框 → 与 OCR 文本框 IoU 匹配 → HTML / Markdown / TSV"。新增 worker 或并入 OCR worker 的一个命令（建议后者：同进程已有 ORT 与 OCR 模型，省一次启动，但 worker 内存会升高约几十 MB，【推断】）。
- 公式：`PP-FormulaNet_plus-S` ONNX，独立 worker（`snow-formula`，沿用 `snow-translator` 骨架）。
- Markdown / HTML：OpenAI 兼容通道 + 引导卡片。

**备选**：公式改用 RapidLaTeXOCR 的 ONNX（MIT、约 99MB、仓库已转好，工程风险最低，但中文公式弱）。若 PP-FormulaNet ONNX 转换/验证不顺，直接降级到它，至少与旧版行为一致。

**风险与待核实**：
1. PP-FormulaNet 的 ONNX：官方文档只列 paddle / transformers 引擎，现成 ONNX 下载地址**未查到**；方案是自转（paddle2onnx 或 transformers→optimum），属一次性离线工作，转换产物托管在我们自己的 release 上；也可先查 RapidAI 的 ModelScope 空间是否已有。**【未验证】**
2. 各权重的单独许可文件需逐个核对（模型卡写 Apache-2.0 的有 PP-FormulaNet_plus-S；SLANet_plus 权重许可页未读到）。
3. 速度只有 GPU / 不明机型数字，Intel UHD 770 需实测，沿用 `tools/snow-ocr-compare` 的做法加表格/公式样片。
4. 精度（SLANet_plus 63.69%）是官方内部集，截图里的网页表格、无线表实际表现需样片验证。

## 4. 分期

| 期 | 内容 | 依赖 | 验收 |
|---|---|---|---|
| P1 | 引导卡片（G04 现状缺口，不依赖模型）；Markdown/HTML 经自定义模型通道；设置页"模型目录"入口 | 无新增 | 未配置时卡片出现、配置后可转换；单测覆盖配置缺失态 |
| P2 | 表格：SLANet_plus + OCR 框，输出 HTML / Markdown / TSV，剪贴板与识别结果窗 | 无新增 crate（沿用 `ort`） | 对照旧版云端输出做样片；`snow-ocr-compare` 加表格样片集 |
| P3 | 公式：PP-FormulaNet_plus-S（或备选 RapidLaTeXOCR）；LaTeX 输出 | 可能新增 tokenizer 词表加载（LaTeX 词表 JSON 解析，已有 `serde_json`；不需新 crate，【推断】） | 英文印刷公式样片 BLEU / 编辑距离；中文公式只标注已知局限 |
| P4（可选） | PP-FormulaNet_plus-M/L 中文增强包、SLANeXt | 视体积 | 用户选装 |

## 5. 需要用户批准

- **模型下载**：下载器 + sha256 的实现范围（见 §6）；是否由我们托管转换后的 ONNX（涉及发布地址，handoff 里"发布地址"仍是待办）。
- **P1/P2 不新增 crate 依赖**（`ort`、`reqwest`、`serde_json` 已批准在用）。P3 若发现需要 `tokenizers` 之类才需另行申请；目前判断不需要，因为 LaTeX 词表是简单的 id↔token 表。
- **模型许可**：确认分发 / 引导下载这些权重可接受（均需 Apache-2.0 / MIT，核对后写入第三方声明；不涉及 NC，与翻译模型 CC-BY-NC 的特殊处理不同）。
- 若最终要在 worker 里并入 OCR 进程，需确认接受 worker 内存上升。

## 6. 模型分发

- **不捆绑权重**（总原则 §2、ADR-5）。
- 目录：`<数据根>/models/table/<id>/`、`models/formula/<id>/`，各带 `model.json`（schema 沿用翻译 manifest：`schema_version`、`id`、`family`、`files`、`sha256`、`license`、`source_url`）。
- 下载器：设置页"下载"按钮，**反复验证 sha256**，失败删除半成品；大文件（PP-FormulaNet_plus-S 约 248MB）用多连接下载；断点续传；用户也可手动放入目录。**镜像**：ModelScope（国内快）与 GitHub release 互为备份。
- 加载：首次使用才起 worker，空闲 N 分钟退出（与翻译 worker 同策略），不预载。
- 未安装 → 引导卡片（能力降级，见 [principles.md](../principles.md) §5）。

## 7. 来源

- 旧版实现：`snow_shot/src/network/snowshotapiclient.cpp`、`snow_shot/src/presentation/components/customaimodelssettingswidget.cpp`
- 本仓：[local-ocr-model-options.md](local-ocr-model-options.md)、[迁移方案 ADR-5](../cisox-gpui-migration-plan.md)、[qt-parity-audit.md](qt-parity-audit.md) G04 行
- RapidTable：https://github.com/RapidAI/RapidTable ；模型：https://www.modelscope.cn/models/RapidAI/RapidTable/files
- PaddleOCR 表格结构识别模块：https://www.paddleocr.ai/latest/en/version3.x/module_usage/table_structure_recognition.html
- PaddleOCR 公式识别模块（模型对照表）：https://www.paddleocr.ai/latest/en/version3.x/module_usage/formula_recognition.html
- PP-FormulaNet_plus-S 模型卡：https://huggingface.co/PaddlePaddle/PP-FormulaNet_plus-S ；论文：https://arxiv.org/abs/2503.18382
- RapidLaTeXOCR：https://github.com/RapidAI/RapidLaTeXOCR ；转换仓库：https://github.com/SWHL/ConvertLaTeXOCRToONNX
- UniMERNet：https://github.com/opendatalab/UniMERNet
- PaddleOCR 3.0 技术报告（PP-StructureV3 对比 MinerU）：https://arxiv.org/abs/2507.05595
- MinerU 与 docling 许可：本次**未查到官方页面**，仅凭印象，选型前须读其 LICENSE。
