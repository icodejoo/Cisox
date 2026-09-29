# ADR-5 调研：本地 NMT 推理后端选型

调研日期 2026-09-29。范围：`snow-translate`（P5）的推理后端、模型格式与获取流程。不含实现。
证据栏中"未验证"表示没有读到一手来源，不能当事实用。

## 0. 结论先行

| 结论 | 依据 | 置信度 |
|---|---|---|
| 推荐 **B：CTranslate2（`ct2rs`）**，CPU 推理，放独立 worker 进程（与 OCR 同构）。这**偏离 ADR-5 现文的"OnnxNmt 默认"**，需要你拍板 | 见 §1、§3 | 中 |
| 备选 **A：`ort` 复用现有 onnxruntime**，自己写 encoder-decoder + KV cache + beam search | 见 §1 | 中 |
| 不选 C（candle/burn）：Marian 有模型结构无 beam search，NLLB/M2M100 未找到实现 | 见 §1 | 中 |
| 最大风险：NLLB 官方权重是 **CC-BY-NC-4.0**，"不用于生产部署"；opus-mt 是 CC-BY-4.0。我们不分发权重，法律风险在用户侧，但文档和 UI 引导必须写明 | HF 模型卡 | 高 |
| 次大风险：CT2 是 C++/cmake 构建，Windows 上 `cargo build` 要 cmake + MSVC，且 ct2rs 在 Windows 默认启用 MKL/OpenMP，二进制体积和 CRT 链接方式需要实测 | ct2rs README | 中 |

## 1. 候选推理后端对比

### 1.1 项目现状（已有的东西）

| 事实 | 依据 |
|---|---|
| 工作区用 `ort = 2.0.0-rc.13`（fork：`mg-chao/ort` rev `90018ee5`，为保留原生诊断日志打的补丁） | `snow-crates/Cargo.toml` 第 35-38 行、`crates/rapid-ocr-rs/Cargo.toml` |
| `rapid-ocr-rs` 有 feature：`dynamic-onnx-runtime`(`ort/load-dynamic`) / `static-onnx-runtime`(`ort-sys/disable-linking`) / `directml-provider` / `cuda-provider` / `cann-provider` | 同上 |
| vcpkg：`onnxruntime`（Windows 带 `directml`）、`opencv4`（含 `dnn`）；overlay port 在 `cmake/vcpkg-overlay-ports/onnxruntime` | `vcpkg.json` |
| OCR 跑在独立进程 `snow-ocr-process` | `snow-crates/crates/snow-ocr-process` |

### 1.2 对比表

| | A. `ort`（复用） | B. CTranslate2 / `ct2rs` | C. candle / burn | D. llama.cpp 系（`llama-cpp-2`） |
|---|---|---|---|---|
| 许可证（对 GPL-3.0） | `ort`：MIT OR Apache-2.0；ORT 本体 MIT。兼容 | ct2rs：MIT；CTranslate2：MIT，第三方件 BSD-2/BSD-3/Apache-2.0。兼容。若启用 `mkl`，Intel MKL 的再分发条款**未验证** | candle、burn 均为 MIT/Apache 系（此项凭常识，未逐个核对，**未验证**） | `llama-cpp-2`：MIT OR Apache-2.0；llama.cpp 本体 MIT（常识，未核对）。兼容 |
| 最近发布 | `ort` 2.0.0-rc.13，2026-07-28；仍无稳定 2.x（max_stable 为 null） | ct2rs 0.10.1，2026-08-31（内置 CT2 v4.8.2，同日发布） | 未验证 | `llama-cpp-2` 0.1.157，2026-09-22 |
| 支持 Marian / NLLB | 通过 ONNX 导出：optimum 支持 encoder-decoder 拆成 encoder/decoder 两个 onnx，`*-with-past` 带 KV cache，并默认合并 decoder 与 decoder-with-past。**具体 marian、m2m_100 是否在支持列表未逐项核实** | CT2 官方文档明确列出 MarianMT、M2M-100、NLLB、MADLAD-400；ct2rs README 说测过 Marian-MT、NLLB | candle 有 `marian.rs`：有 KV cache，**无 beam search**。NLLB/M2M100：搜索未找到实现 | 不是 NMT 架构。走的是"翻译专用 LLM"路线：TranslateGemma、MADLAD-400、Hunyuan-MT 均有 GGUF 版。opus-mt/NLLB 不适用 |
| 解码循环实现负担 | **高**：自己写 encoder 一次 + decoder 循环、KV cache 张量管理、beam search、语言码强制 BOS（NLLB）。社区有坑：ORT issue #17677 里有人用 argmax 手写循环得到破碎译文（原因未定论，issue 里疑似解码方式） | **低**：`Translator` 内置 beam search、批处理、量化 | 高（要补 beam search，NLLB 要整个移植） | 低，但换了模型类别 |
| 模型准备 | 用户须自己 `optimum-cli export onnx`（要 Python+torch），或下载现成 ONNX（如 Xenova/opus-mt-zh-en 的 `onnx/` 目录，含 fp32/fp16/int8/q4 等约 40 个文件，encoder fp32 210MB、int8 52.7MB，用户易选错） | 用户须 `ct2-transformers-converter`（要 `transformers[torch]`），或下载别人已转好的 CT2 模型（**可用性未验证**） | 直接读 safetensors | 直接下 GGUF |
| Windows 构建 | 已有：vcpkg onnxruntime 或 download-binaries，无新增 | 需 **cmake**（ct2rs build-dep `cmake 0.1`、`cxx-build`），README 提示 Windows 可能要 `RUSTFLAGS=-C target-feature=+crt-static`；v0.9.22 修过 MSVC 下 OpenMP 链接。能否 `cargo build` 一键：**未验证** | 纯 `cargo build` | 需 cmake + C++ 工具链（bindgen 可能要 libclang，**未验证**） |
| 二进制体积增量 | 0（已有） | **未验证**。CT2 自带多后端 CPU 分发；默认特性 `ruy`+`cuda-small-binary`，Windows 平台默认还有 `mkl/dnnl/cuda/cudnn/cuda-dynamic-loading`（README 原文），需要手动裁剪 | 未验证 | 未验证 |
| CPU / GPU | ORT：CPU；DirectML 已接入，但 **DirectML 已进入 sustained engineering**，微软推荐 WinML（onnxruntime.ai 文档、microsoft/DirectML 仓库页） | CPU（x86-64、AArch64）+ CUDA；**无 DirectML**；macOS 用 Accelerate/Ruy | CPU/CUDA/Metal，成熟度未验证 | CUDA/Metal/Vulkan 等 |
| 与现有 ORT 冲突 | 无（同一个） | **不用 ORT**，无双 ORT 问题。但 MKL/OpenMP 运行时与 ORT 同进程共存的风险**未验证**——放独立进程可规避 | 无 | 无 ORT。ggml 与 ORT 同进程符号冲突**未验证** |
| macOS / Linux | 已有覆盖 | ct2rs 有 `accelerate` 特性，README 说明 Apple Silicon 用 Accelerate+Ruy | 好 | 好 |

关于"CT2 性能和内存优于 ORT"：CT2 仓库 README 的基准是 CPU 658.8 tokens/s、内存 849MB，对比方 209-275 tokens/s、2000MB 以上。**我读到的摘要没有说明对比方是不是 ORT**，且是 CT2 自己发布的基准，所以这条只能当"CT2 自称快"，不能当"比 ORT 快"的证据。P5 spike 必须自己测。

### 1.3 D 的定位

llama.cpp 系是另一类东西（LLM 翻译，模型大、首 token 慢、可能跑偏，但语种和文风好）。它跟 ADR-5 的"opus-mt/NLLB/Marian"需求不重叠，而且 ADR-5 已保留 OpenAI 兼容通道，用户想用 LLM 翻译可以让 Ollama/llama-server 跑起来走该通道。**不作为内置后端。**

## 2. 模型格式与获取流程

| 项 | 建议 | 依据 / 置信度 |
|---|---|---|
| 目录 | `<AppData>/<产品名>/models/translate/<model-id>/`，一个目录一个模型，沿用 ADR-5 已定的布局 | ADR-5，高 |
| 清单 | 需要 sidecar `model.json`（ADR-5 已设计）。HF 上的模型自带的是 `config.json`，各家格式不同，不能替代。用户放好权重后手写或用我们提供的模板 | 推理，中 |
| `model.json` 需改动 | 增加 `runtime: "ct2" \| "onnx"`（决定谁加载）；`files` 里 CT2 是 `model.bin`+`config.json`+词表，ONNX 是 encoder/decoder。`family` 保留 | 设计，中 |
| 完整性校验 | 硬校验只做：清单声明的文件是否都存在、大小非零；可选字段 `sha256`（用户想校验可填）。**不强制哈希**：权重由用户自己转换或下载，我们没有权威哈希源 | 设计，中 |
| CT2 目录内容 | `model.bin`、`config.json`、词表（`shared_vocabulary.*` 或类似）、tokenizer（README 示例用 `--copy_files tokenizer.json`；Marian 模型带 `source.spm`）。**确切文件名未验证**，spike 时以实际转换产物为准 | 中低 |
| tokenizer | HF `tokenizers`（Apache-2.0，1.0.0-rc.2，2026-09-21）读 `tokenizer.json`；SentencePiece 需要 `sentencepiece` crate（MIT/Apache-2.0，0.14.0，2026-07-26，可 static 捆绑 C++ 或链接系统库）。opus-mt 用 SentencePiece（模型卡：32k），仓库里有 source/target 两个 spm（文件名未核实）。**opus-mt 能否只靠 `tokenizer.json` 处理，未验证**；Xenova 的 ONNX 仓库通常自带 tokenizer.json（未核实） | 中 |
| 懒加载 | 启动只扫清单；首次翻译才创建 Translator/Session，显示加载进度；空闲 N 分钟后销毁 | ADR-5 已定 |
| 内存控制 | CT2 有量化（int8 等，转换时 `--quantization`），是用户转换时的选择；我们在设置页提示"推荐 int8"。同一时刻只保留一个已加载模型 | 中 |
| 放独立进程的好处 | 崩溃隔离、空闲卸载=退出进程（内存立即归还）、规避 MKL/OpenMP 与 ORT 同进程的风险。代价：IPC 与进程启动延迟，需 spike 量 | 设计，中 |
| 语言对 / 多模型 | 模型清单里声明 `languages`（NLLB 用 `zho_Hans` 这类 FLORES 码，Marian 是单向 `zh-en` 需要靠模型 id 表达语向）。应用内部用 BCP-47，`family` 决定映射表。默认模型 id 存配置；同一语向有多个模型时按"用户选择的模型"优先，不做自动路由 | 设计，中 |
| 配置项（P5 才加） | 挂在 `screenshot_translation` 下：模型目录、默认模型 id、空闲卸载分钟数、后端选择（`local` / `openai_compat`）。现在不加字段 | 方案 §9 裁决，高 |
| 获取流程给用户的说明 | 三条路：①直接下现成 CT2/ONNX 模型（可用性未验证）；②装 Python，用 `ct2-transformers-converter` 或 `optimum-cli export onnx` 自己转；③不装模型，走 OpenAI 兼容通道。引导卡片写这三条 | 中 |
| 许可证提示 | opus-mt-zh-en：CC-BY-4.0（模型卡）。NLLB-200-distilled-600M：CC-BY-NC-4.0，卡上写明"not released for production deployment"，另限制 512 token、非文档翻译。UI 引导里只中立说明，不推荐特定模型 | HF 模型卡，高 |

## 3. 推荐方案、备选、风险

### 3.1 推荐：B（CTranslate2 / ct2rs）

理由：
1. 用户"自选 Marian/NLLB/M2M100"这个需求，CT2 是唯一一个官方文档直接把这三类都列为支持并内置 beam search 的。A 要我们自己写解码器，成熟度和正确性都是自担风险（ORT issue #17677 就是手写循环出问题的例子）。
2. 我们不需要在 GUI 进程里跑它：放 worker 进程，构建复杂度隔离在一个 crate。
3. 代价明确：不支持 DirectML，只有 CPU。opus-mt 一类小模型 CPU 足够，这一点需要 spike 量化。

偏离 ADR-5 的点：ADR-5 写"OnnxNmt 默认，零新增重型依赖"。B 新增 C++ 依赖，与其相悖，所以列入待批准。

### 3.2 备选：A（`ort` + 自写解码）

适用条件：如果 spike 证明 B 的 Windows 构建/体积不可接受，或你更看重"零新增依赖 + DirectML"。工作量在解码循环。可参考 `linguonnx`（Python，Apache-2.0，自称在原始 ONNX 图上实现了 Marian/M2M100/NLLB 的 encoder-decoder 循环、beam search、KV cache；仅作实现参考，成熟度低，0 star，**未验证质量**）。

### 3.3 风险表

| 风险 | 影响 | 缓解 |
|---|---|---|
| ct2rs Windows 构建（cmake、CRT 静态、OpenMP、MKL 默认开） | CI 变复杂 | spike 第一项就测；用 `default-features=false` 只开 `ruy`+tokenizers 试 |
| 二进制体积膨胀 | 安装包变大 | 实测，放独立 exe 使主程序不受影响 |
| ORT 与 CT2 同机不同进程，无冲突；同进程未验证 | — | 坚持独立进程 |
| 用户拿不到 CT2 格式模型（要装 Python+torch 转换） | 使用门槛高 | 文档写清；OpenAI 兼容通道兜底 |
| 模型许可证（NLLB 非商用） | 用户侧合规 | UI 与文档提示，不内置 |
| ct2rs 单人维护（jkawamoto） | 长期 | 发布频繁（2026-04 至 08 至少 6 个版本），但依赖单人；最坏退回 A |

### 3.4 P5 首个 spike

目标：用一个 opus-mt 小模型（如 opus-mt-zh-en，CC-BY-4.0）跑通一句话。本机 Windows 10，先测 B，同条件补测 A 的最简版（greedy 即可）作对照。

| # | 判据（可执行） | 通过条件 |
|---|---|---|
| S1 | 空目录下 `cargo build -p snow-translate-spike --release`（仅装 MSVC + cmake）成功 | 退出码 0；记录耗时 |
| S2 | 输出 exe 体积（不含模型） | 记录；**≤ 60MB 为可接受**（阈值为提案，可改） |
| S3 | 输入 `"今天天气很好。"` 及 20 句固定测试集（放 `snow-shot-rs/crates/snow-translate/tests/fixtures/`），beam=4 | 输出非空、无重复循环、无乱码；与 Python `transformers` 同模型 beam=4 输出**逐句相同或 ≥ 90% 句子相同**（量化会造成差异，差异句人工确认可读） |
| S4 | 冷启动（进程启动到模型就绪）与首 token 延迟，各测 10 次取中位数 | 记录；**首次翻译端到端（含加载）≤ 3s，热调用单句 ≤ 500ms** 为期望值（提案） |
| S5 | 加载后常驻内存（Working Set） | 记录；int8 的 opus-mt 期望 ≤ 500MB（提案） |
| S6 | 卸载：worker 进程退出后内存归还 | 进程消失即通过 |
| S7 | 若走 A 对照：同句子集、同机器测 S3-S5 | 用于最终定夺，不设阈值 |
| S8 | 换 NLLB-600M（int8）再跑 S3-S5，仅性能，不做产品化 | 记录 |

## 4. 需要用户批准的新依赖清单

| 名称 | 版本 | 许可证 | 用途 | 备注 |
|---|---|---|---|---|
| `ct2rs` | 0.10.1（2026-08-31） | MIT | CTranslate2 Rust 绑定（内置 CT2 v4.8.2，MIT） | 构建需 cmake；默认特性需裁剪 |
| CMake（构建工具，非 crate） | 未指定 | BSD-3（常识，未核实） | 编译 CT2 | 开发机与 CI 需安装 |
| `tokenizers` | 1.0.0-rc.2（2026-09-21）。ct2rs 内部要求 0.22，版本需对齐 | Apache-2.0 | 读 `tokenizer.json` | ADR-5 原本就列了 |
| `sentencepiece` | 0.14.0（2026-07-26）。ct2rs 内部要求 0.13，版本需对齐 | MIT OR Apache-2.0 | opus-mt 等 SentencePiece 模型的分词 | 会引入 C++（可 static 或 system） |
| `reqwest` | 待定 | MIT OR Apache-2.0 | OpenAI 兼容通道 | ADR-5 已列，需确认工作区是否已有 |
| `ort`（备选 A 才需要） | 沿用工作区 `=2.0.0-rc.13` | MIT OR Apache-2.0 | 备选后端 | 已在项目里，无新增 |

未纳入：`candle`、`burn`、`llama-cpp-2`（不选）。

## 5. 未能验证的点

- ct2rs 在 Windows 上能否只靠 `cargo build` 完成、构建耗时、最终 exe/DLL 体积。
- ct2rs 裁剪到 `ruy` 后 Windows 的实际推理速度，与 MKL 的差距。
- Intel MKL 若启用，再分发条款与 GPL-3.0 的关系。
- CT2 基准对比方是否包含 ORT。
- optimum 对 `marian`、`m2m_100` 的 ONNX 导出是否在支持列表内（只读到通用文档）。
- opus-mt 的 tokenizer 能否只用 `tokenizer.json`；CT2 转换产物的确切文件名。
- 网上是否已有现成的 CT2 版 opus-mt/NLLB 模型可供用户直接下载。
- candle/burn 的许可证与最近发布日期，candle 是否已有 NLLB/M2M100（只搜到没有结果，不等于不存在）。
- CT2/ggml 与 ORT 同进程共存的风险（已用独立进程规避，未实测）。

## 6. 来源

- crates.io API：ct2rs、ort、tokenizers、sentencepiece、llama-cpp-2（2026-09-29 读取）
- https://github.com/jkawamoto/ctranslate2-rs（README、Cargo.toml、releases）
- https://github.com/OpenNMT/CTranslate2（README、release v4.8.2）
- https://opennmt.net/CTranslate2/guides/transformers.html
- https://huggingface.co/docs/optimum-onnx/onnx/usage_guides/export_a_model
- https://raw.githubusercontent.com/huggingface/candle/main/candle-transformers/src/models/marian.rs
- https://huggingface.co/facebook/nllb-200-distilled-600M ；https://huggingface.co/Helsinki-NLP/opus-mt-zh-en ；https://huggingface.co/Xenova/opus-mt-zh-en/tree/main/onnx
- https://github.com/microsoft/onnxruntime/issues/17677 ；https://github.com/TigreGotico/linguonnx
- https://github.com/microsoft/DirectML ；https://onnxruntime.ai/docs/execution-providers/DirectML-ExecutionProvider.html
- 仓库内：`docs/cisox-gpui-migration-plan.md` ADR-5/§7/§9/§10，`snow-crates/Cargo.toml`，`snow-crates/crates/rapid-ocr-rs/Cargo.toml`，`vcpkg.json`
