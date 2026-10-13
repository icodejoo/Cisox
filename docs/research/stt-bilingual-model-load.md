# 双语流式模型加载提速：慢在哪、怎么降到 1 秒内

> 状态：调研 + 本机离线实测完成，**未改任何产品代码**；方案待用户拍板。日期 2026-10-13。依据 [`principles.md`](../principles.md) 的优先级链（高性能 > 低内存 > 高 fps > 少编译依赖 > 多用系统能力，另含小体积、不常驻）。
> 关联：[`research/dictation-cold-start.md`](dictation-cold-start.md)（§3.1 量到 x-asr 480ms 加载 5.7 / 9.4 / 13.6 s、§9 第 5 条、§7 E5 就是本文的起点）、[`research/stt-model-selection.md`](stt-model-selection.md)（历史加载口径）、[`research/speech-to-text-backends.md`](speech-to-text-backends.md)、[`guides/snow-stt-worker.md`](../guides/snow-stt-worker.md)（worker 协议与构建）。
> 资料来源说明：产品行为来自读代码和本机实测（探针与脚本在 scratchpad，不入仓库；`rprobe` 的构建目录在已被忽略的 `build/stt-probe`）。ORT / sherpa-onnx 的行为先读源码与官方文档，再用本机实测交叉验证；只读到摘要、没实测的写"未证实"。**本机测量时有其它进程在编译和跑脚本（处理器利用率 30%~120%），绝对数字噪声很大，所以所有对比都是同一时段交替跑，看量级和排序，不看小数。**

## 1. 结论（先看这里）

1. **默认双语模型 x-asr 480ms 的加载慢，一半以上是 sherpa 把 encoder 载了两遍。** sherpa-onnx 在 `config.model_type` 为空时，会先建一个临时 ORT 会话只为读 `model_type` 元数据，用完丢掉，再建真正的会话（官方源码 `online-transducer-model.cc`，离线识别器 `offline-recognizer-impl.cc` 同样）。snow-stt 没填 `model_type`，所以每次加载付两份代价。只填 `model_type = "zipformer2"`：同一台机器同一时段，x-asr 480ms 中位数 5.4 s → 3.1 s，multi-zh 2.8~3.1 s → 1.5~1.9 s。三个已装模型的元数据都是 `zipformer2`。
2. **剩下的时间不是读盘、不是线程、不是图优化，是 encoder 图太碎。** encoder 有 12261 个节点（其中 4073 个 `Constant`、574 个 `Shape`、844 个 `Unsqueeze`……PyTorch 导出时没折叠常量），ORT 建会话是**单线程**的逐节点工作。证据：读文件 60~95 ms、解析 0.2~0.3 s，会话初始化 3.6~4.4 s；线程数 1 / 2 / 8 一样；图优化级别从 0 到 99 一样（3.0~4.2 s）；预打包开关没差别。
3. **把 encoder 预优化一次、落盘，之后每次直接载优化后的文件，加载降一个量级。** 用 ORT 自己做（`SetOptimizedModelFilePath` + 优化级别 `ORT_ENABLE_EXTENDED` + 把动态批量维 `N` 固定为 1），不需要 onnxsim：encoder 节点 12261 → 3125，叠加 `model_type`，x-asr 480ms 加载中位数 **约 0.85 s（范围 0.70~1.5 s）**，multi-zh **0.44~0.53 s**；整段 74 秒中英混合音频的识别文本逐字一致（SHA 相同），RTF 不变或略好，峰值工作集 266 → 233 MB。
4. **"历史评测 2.5 s、现在 5~13 s"的差距，本机能证实的部分：不是 ORT 版本、不是磁盘、不是线程，是单线程 CPU 时间被拉长，且波动极大。** 同一个探针连续跑，用户态 CPU 时间从 3.0 s 到 13.4 s 不等，与墙钟几乎相等（内核态占 <10%）——这是在 CPU 上真正算得慢，不是在等 I/O。机器当时有 rustc、python、find 等其它进程在跑。历史评测的机器是 Ryzen 9 7945HX / Win11，本机是 i5-13500（6P+8E 混合核）/ Win10，**两台机器本来就不同口径**。两者各占多少，没法在本机分开，见 §3.4、§8。
5. **推荐顺序**：①worker 填 `model_type`（改动最小，收益约 -40%）；②加载前先查有没有预优化过的 encoder，没有就后台生成一次并缓存（加 `ort` 依赖，需要用户批准），有就直接用（再降到约 1 s）；③不动模型精度、不换模型。完成后 x-asr 480ms 加载预计 **≈1 s（0.7~1.5 s）**，multi-zh ≈0.5 s。
6. 和 [`dictation-cold-start.md`](dictation-cold-start.md) 的关系：那边是"加载时间不丢字"（先开麦 + 环形缓存），本文是"让加载本身变短"。两条都做，加载降到 1 s 左右后，缓存和追赶的量也跟着降到最小；但 0.7~0.9 s 的开麦时间仍是下限，**两条线互不替代**。

## 2. 现状：worker 怎么加载模型

读 `tools/snow-stt/src/{main,sherpa,backend}.rs` 得到：

- 流式后端 `SherpaBackend::load`（`sherpa.rs`）：`OnlineRecognizerConfig` 只填了 `encoder/decoder/joiner/tokens`、`num_threads`（来自 START 的 `threads`，默认 2）、`provider = "cpu"`、`greedy_search`、端点规则。**没有 `model_type`**，没有任何会话选项（sherpa 的 C API 本来也不暴露优化级别、优化模型路径）。
- sherpa-onnx 1.13.8（`shared` 链接），`onnxruntime.dll` 被构建脚本替换为仓库同版 1.28.0.20260724 自编译版；sherpa 自带的是 1.28.2。
- 会话选项来自 sherpa 的 `GetSessionOptionsImpl`（`session.cc`）：`SetIntraOpNumThreads(n)`、`SetInterOpNumThreads(n)`、`SetGraphOptimizationLevel(...)`（默认全开）、`DisableMemPattern`、`DisableCpuMemArena`；不设 `optimized_model_filepath`。
- 模型文件由 `discover_model` 按文件名挑：encoder / joiner 优先 `int8`，decoder 优先非 int8。三个已装模型都是 transducer，encoder 是 `onnx.quantize` 产出的 int8（`MatMulInteger` + `DynamicQuantizeLinear`），opset 13。
- 加载在开麦之前（`main.rs` 注释："避免把加载期间的旧音频喂进去"），所以加载多久，开头就丢多久（见 dictation-cold-start）。
- 进程优先级不是 worker 自己设的，**继承拉起它的 UI 进程**（`client.rs` 的 `quiet_command` 不改优先级；UI 的优先级来自 `system/application_priority`）。历史评测用 BelowNormal，我的测量用默认优先级。

### 2.1 模型文件与图规模（本机已装，读 ONNX 元数据，不跑）

| 模型 | encoder 体积 | encoder 节点数 | 其中 `Constant` | 初始值（权重等） | decoder / joiner 节点 | `model_type` |
| --- | --- | --- | --- | --- | --- | --- |
| x-asr 480ms / 160ms（双语，默认） | 155.3 MB | **12261** | 4073 | 1686 | 17 / 8 | `zipformer2` |
| multi-zh-hans-2023-12-12（纯中文） | 70.1 MB | 10481 | 3497 | 1425 | — | `zipformer2` |

x-asr 480ms 与 160ms 的 encoder 文件体积完全相同，加载代价按同量级看（160ms 没单独测）。

## 3. 实测：时间花在哪

环境：Dell，i5-13500（6P+8E，20 线程，基频 2.5GHz），32GB，Win10 19045，Balanced 电源方案；模型在 NVMe，Defender 实时防护关闭；深信服 SNAC 注入每个进程，数字都含它。`snow-stt.exe` 是 2026-10-09 构建的 release。**每项最多 3 次**，交替跑（A B A B A B）；括号里是极端值，单列出来不进"典型"。

### 3.1 分段计时（x-asr 480ms encoder，ORT 1.28.0，同一份 DLL，热页缓存，intra=2）

探针用 ctypes 直接调 worker 同版 `onnxruntime.dll` 的 C API（OrtApi 下标按 v1.28.0 头文件算，已用 `CreateSession` 等下标核对），并打开 ORT 自带的会话 profiling，读它记的 `model_loading_uri` 与 `session_initialization`。

| 阶段 | 实测（ms，3 次） | 说明 |
| --- | --- | --- |
| 读文件（155MB，Python `read()`） | 64 / 66 / 75（另有 153、369 两次离群） | 热缓存 |
| 解析模型（ORT `model_loading_uri`） | 284 / 314 / 1166 | 含读文件 + protobuf 解析 |
| 会话初始化（`session_initialization`） | **3641 / 4393 / 7284** | 图优化 + 建内核 + 权重预打包 |
| 会话总计（`CreateSession`） | 3960 / 4678 / 8473 | 另一组 3 次：3605 / 3839 / 3220 |
| decoder.onnx 建会话 | 60 / 63 / 75 | 17 个节点 |
| joiner.int8.onnx 建会话 | 45 / 50 / 53 | 8 个节点 |
| 首次解码（建流后喂 2 s 静音、解 3 块） | 86~290，典型 ~120 | 即"首次推理预热"，**可忽略** |
| Rust worker `load_ms`（整个 `OnlineRecognizer::create`） | 4432 / 4815 / 5173（另一批 5294 / 5959 / 7351） | 约等于"一份 encoder 会话 × 2 + 其它" |

结论：**时间几乎全在会话初始化，而它是单线程的逐节点处理**。decoder / joiner / 词表 / 首次推理加起来不到 0.3 s。

### 3.2 冷缓存 vs 热缓存

用 `NtSetSystemInformation(SystemMemoryListInformation)` 清修改列表与备用列表（需要管理员权限与 `SeProfileSingleProcessPrivilege`；跟 tray-host-split §12.1 同法）。**这是整机级操作，本任务一共做了 3 次，随后系统自行回填缓存。**

| 项 | 冷 | 热 |
| --- | --- | --- |
| 读 155MB（`ReadAllBytes`） | 172 / 220 / 250 ms | 92 / 122 / 136 ms |
| rprobe 加载 x-asr 480ms（`load_ms`，清缓存后立刻跑一次，再跑一次热的） | 4499 / 6086 / 8397 | 4435 / 4905 / 5097 |

冷热差在噪声以内（最多一两秒的差来自背景负载，冷读本身只多 ~0.1 s）。**磁盘不是瓶颈**：NVMe 上整个 encoder 0.2 s 读完。另外，进程的用户态 CPU 时间 ≈ 墙钟（见 §3.4），也说明没在等 I/O。机械盘 / 杀软扫描 / 网络盘的情形没测。

### 3.3 会话选项逐项扫（只改一项，encoder，3 轮交替，会话 ms）

| 选项 | 结果 | 结论 |
| --- | --- | --- |
| 图优化级别 0 / 1 / 2 / 99（原模型） | 3635~4186 / 3756~4656 / 2959~4414 / 2956~3792 | **没有稳定差异**：优化器本身不是大头；节点数才是 |
| intra 线程 1 / 2 / 8（预优化后的 encoder） | 2110~2224 / 2024~2289 / 2024~2212 | 建会话单线程，**线程数无关** |
| `session.disable_prepacking=1` | 1835~2346（基线 2105~2289） | 预打包不是大头 |
| `DisableMemPattern` + `DisableCpuMemArena`（sherpa 已设的组合） | 2058~2356 | 无差别 |
| ORT 版本：仓库自编 1.28.0 vs sherpa 自带 1.28.2（交替各 3 次整段 worker） | A 5294 / 5959 / 7351，B 6137 / 4630 / 5187 | **无显著差异**，不是版本问题 |
| 进程亲和：仅 P 核（0xFFF） vs 仅 E 核（0xFF000），整段 worker | 6328 / 6211 / 6579 vs 8358 / 7681 / 7707 | E 核慢约 20%~30%，但没钉核时也一样慢，不是主因 |

### 3.4 为什么比历史评测慢 ~2 倍、三次差 2~2.5 倍：能证实与不能证实的

| 假设 | 检验 | 结果 |
| --- | --- | --- |
| ORT 版本（仓库自编 1.28.0 vs 评测时 sherpa 自带 1.28.2） | 交替各 3 次 | **排除** |
| 磁盘 / 冷缓存 / 杀软扫描 | 清缓存 ×3；内核态占比；Defender 实时防护状态 | **基本排除**：冷读 ~0.2 s，内核态 <10%，Defender 关闭。SNAC 的影响无法单独关掉，**未证实** |
| 线程数、图优化级别 | §3.3 | **排除** |
| 首次推理预热 | 首次解码 ~120 ms | **排除** |
| CPU 慢且抖动（背景负载、SMT 同核争用、E 核、频率） | 记进程用户态 / 内核态 CPU 时间；看同时段负载；钉核 | **主因，已部分证实**：连续 3 次同配置探针，墙钟 13.8 / 7.0 / 5.9 s，用户态 CPU 13.4 / 6.7 / 5.6 s，内核态 0.6 / 0.6 / 0.6 s——算同样的活，CPU 时间差 2.4 倍。同时段进程采样：rustc ×4、python、find、Lark 合计占 ~4~5 个核，系统处理器利用率 66%~120% |
| 机器不同（7945HX 16 核 5.4GHz Win11 vs i5-13500 Win10） | 无法在本机测 | **未证实**，但历史数据本来就不同机器，口径不可比（stt-model-selection §2.1 自己写了"不同机器的数据不能直接对比"） |
| 进程优先级 BelowNormal | 评测用 BelowNormal，我用默认；High 与默认各 3 次差别在噪声内 | 没有证据显示优先级是原因 |

所以对"差 2 倍"的最诚实的说法：**加载是纯 CPU 单线程活，机器与当时的负载决定快慢**；历史 2.5 s 是在另一台更快的机器上、负载较轻时测的，且同样含"载两遍"。本机的 5~13 s 里，"载两遍"约占一半，其余是这台机器在负载下单核较慢。无法定量拆分，也没有在空闲机器上复测（做不到：多个子代理共用这台机器）。

注意"别钉核"：钉到 `cpu0` 的那组跑出过 45 s、19 s（cpu0 要处理中断和别的线程），不能拿来当"提速手段"。

## 4. 提速手段对比

收益栏口径：同一时段交替测 x-asr 480ms 的 `OnlineRecognizer::create`（rprobe，sherpa 1.13.8 + 仓库同版 ORT，与 worker 同栈，默认优先级）。"典型"取中位数，括号是范围；基线 A 为全部 21 次运行（最慢一次 13.4 s 属背景负载尖峰）。

| 编号 | 手段 | x-asr 480ms 加载（典型，范围） | multi-zh 加载 | 体积 | 精度 | 内存 | 依赖 / 代价 | 状态 |
| --- | --- | --- | --- | --- | --- | --- | --- | --- |
| A | 现状（不填 `model_type`，encoder 载两遍） | 5.4 s（3.9~6.7） | 2.8~3.1 s | — | — | 峰值工作集 266 MB | — | 实测 |
| B | 填 `model_type="zipformer2"`，跳过临时会话 | 3.1 s（2.7~5.0） | 1.5~1.9 s | 不变 | 不变 | 基本不变 | **无新依赖**；需要拿到正确的 `model_type`（见 §5） | 实测 |
| C | B + 预先用 ORT 优化（级别 EXTENDED）并落盘，加载直接用 | 1.65 s（1.4~2.1） | — | 155.3 → 154.6 MB | 无变化（文本逐字一致） | — | 需要 ORT 做一次生成；优化后的文件与 ORT 版本绑定 | 实测 |
| D | B + onnxsim 简化（固定批量 N=1）后的 encoder | 1.0 s（0.9~1.75） | — | 153.6 MB | 无变化 | — | 要引入 onnxsim（Python 工具），仅适合发布时由我们生成 | 实测 |
| E | D + C（onnxsim 后再 ORT 优化） | **0.86 s（0.70~1.5）** | **0.44~0.53 s** | 151.9 MB | 无变化（74 s 音频 7 句 SHA 相同；multi-zh 6 句 SHA 相同） | 峰值工作集 **233 MB**（-33） | 同 D | 实测 |
| F | B + 纯 ORT：EXTENDED 优化 + `AddFreeDimensionOverrideByName("N",1)` 后落盘 | **0.8~1.05 s**（一次 3.4 s 为背景负载） | — | 152.4 MB | 无变化（SHA 相同） | 同 E 量级 | 只要 ORT（仓库里别的 worker 已用 `ort` crate），**不需要 onnxsim**；节点 12261 → 3125，与 E 的 3086 相当 | 实测 |
| G | 不优化图、只降优化级别 | 无收益（§3.3） | — | — | — | — | sherpa 的 C API 不暴露，也没用 | 实测 |
| H | 加线程 / `session.disable_prepacking` | 无收益（§3.3） | — | — | — | — | — | 实测 |
| I | 转 ORT 格式（`.ort`），含 `session.use_ort_model_bytes_directly` | 与优化后 `.onnx` 同量级（§4.3） | — | 156.2 MB | — | 理论上少一次拷贝，未测 | 要改 worker 的文件挑选（`discover_model` 只认 `.onnx`）；sherpa 不暴露 `session.*` 配置 | 实测（无收益），mmap 未测 |
| J | 外部数据 / 内存映射权重 | 未测 | — | — | — | — | 权重只占解析的 0.2~0.3 s，上限也就这点；瓶颈是节点数 | 未证实 |
| K | 量化 / 裁剪换更小的权重 | 不会明显变快（时间随节点数，不随权重字节数） | — | 变小 | 要重评 | — | 要重训或重新量化，成本高 | 推断，未测 |
| L | 预编译缓存（EP 上下文缓存） | 不适用 | — | — | — | — | CPU EP 没有跨会话内核缓存；等价于 C~F 的"优化后落盘" | 官方文档 |
| M | 拆分懒加载（decoder / joiner 延后） | 无意义 | — | — | — | — | decoder + joiner 共约 0.1 s，encoder 占 97% | 实测数据推出 |
| N | 并行加载多个子模型 | 只能并行 0.1 s 的小模型 | — | — | — | — | 同上。真正的并行是"和开麦、UI 启动重叠"，见 dictation-cold-start | 实测数据推出 |
| O | 换更小的双语模型 | 历史评测 bilingual-2023-02-20：加载 0.85~0.89 s，**混说 CER 20.66% vs x-asr 7.45%** | — | encoder int8 174 MB（官方文档），比 x-asr 还大 | **明显变差** | — | 用户已定 x-asr，不建议换 | 历史数据，未复测 |

### 4.1 官方依据

- ORT 优化级别与离线模式（`optimized_model_filepath`）：[Graph Optimizations](https://onnxruntime.ai/docs/performance/model-optimizations/graph-optimizations.html)。原文要点：离线保存能省掉每次启动的重复优化；"必须在与目标机器相同的选项和硬件上保存"；开了布局优化（NCHWc）的离线模型只能用于兼容硬件；基础与扩展优化一般可移植。本文建议保存用 EXTENDED，不用 `ORT_ENABLE_ALL`，原因就在这里。
- ORT 格式：[ORT Format Models](https://onnxruntime.ai/docs/performance/model-optimizations/ort-format-models.html)。完整构建两种格式都能载；兼容性按"当前版本能读当前及更旧的 ORT 格式"，历史上出现过破坏性变更。
- 会话配置键（`session.disable_prepacking`、`session.use_env_allocators`、`session.use_ort_model_bytes_directly`、`session.use_memory_mapped_ort_model`、`session.prepack.enable_parallel` 等）：[onnxruntime_session_options_config_keys.h](https://github.com/microsoft/onnxruntime/blob/main/include/onnxruntime/core/session/onnxruntime_session_options_config_keys.h)。`session.prepack.enable_parallel` 写明"让内核预打包在 intra-op 线程池里并行，减少权重多的模型的加载时间"；本机预打包不是大头，且 sherpa 不暴露会话配置，**没测**。
- sherpa-onnx 会话选项：[session.cc](https://github.com/k2-fsa/sherpa-onnx/blob/master/sherpa-onnx/csrc/session.cc)；在线 transducer 先建临时会话读 `model_type`：[online-transducer-model.cc](https://github.com/k2-fsa/sherpa-onnx/blob/master/sherpa-onnx/csrc/online-transducer-model.cc)；离线识别器同样：[offline-recognizer-impl.cc](https://github.com/k2-fsa/sherpa-onnx/blob/master/sherpa-onnx/csrc/offline-recognizer-impl.cc)。读的是 master 分支，不是 v1.13.8 标签；但 Python 1.13.4 与 Rust 1.13.8 的实测（§4 的 A→B）都与源码描述一致。
- 双语模型体积：[sherpa 文档：Zipformer 流式 transducer](https://k2-fsa.github.io/sherpa/onnx/pretrained_models/online-transducer/zipformer-transducer-models.html)（bilingual-zh-en-2023-02-20：encoder int8 174M）。

### 4.2 逐项原始数据

所有 `load_ms`（x-asr 480ms，ms），rprobe：

- A（现状）：5648 / 5908 / 6162；13409 / 6650 / 5511；5485 / 5420 / 5327；4579 / 4710 / 3935；3973 / 5803 / 5381；5070 / 5734 / 5251；4806 / 5771 / 4832（共 21 次，其中后 3 组夹在冷缓存实验里）
- B（`model_type`）：3105 / 2739 / 3129；4978 / 2653 / 3780；4153 / 4021 / 2725。Python sherpa 1.13.4 另测：6877 → 3377、6522 → 2302、4691 → 2294（无 / 有 `model_type`，每对紧挨着跑）
- C（B + ORT EXTENDED 优化 encoder）：1802 / 2096 / 1508；1365 / 1457 / 1837
- D（B + onnxsim encoder）：957 / 900 / 1012；998 / 1102 / 1749
- E（B + onnxsim + ORT 优化）：1335 / 853 / 779；704 / 700 / 702；701 / 1488 / 1378；855 / 884 / 900；1143 / 748 / 864
- F（B + ORT EXTENDED + N=1）：1601 / 3363 / 1388；789 / 787 / 1047
- multi-zh：A 3019 / 3095 / 2766；B 1792 / 1942 / 1459；B + onnxsim + ORT 优化 476 / 440 / 534
- 整段 worker（`snow-stt.exe --wav`，**未填 `model_type`**，所以仍载两遍；只换 encoder 文件）：原版 4432 / 4815 / 5173 / 4792 / 4593 / 3836 / 10229，换成优化版 1532 / 1668 / 1285 / 1514 / 1652 / 1695——即便不改 worker，只换 encoder 文件就从 ~4.8 s 降到 ~1.5 s（但如上所述，这样要把图固定成批量 1 并绑定 ORT 版本）。

### 4.3 其它：ORT 格式与"已优化后再载"（encoder，3 轮交替，会话 ms）

| 载入方式 | 结果 |
| --- | --- |
| 原 `.onnx`，级别 99 | 3839 / 3220 / 3725 |
| EXTENDED 优化后落盘的 `.onnx`，级别 99 | 2329 / 2176 / 2230 |
| ALL 优化后落盘的 `.onnx`，级别 99 | 2070 / 2173 / 2105（级别 0：1960 / 1830 / 1892） |
| 同一份存成 `.ort`，级别 99 | 2409 / 2565 / 1828 |
| `.ort` + 从内存缓冲区建会话 + `use_ort_model_bytes_directly` | 2470 / 2067 / 1865 |
| 优化后再固定 N=1（F）、或 onnxsim 后优化（E），级别 99 | 870~997（ORT profiling：解析 ~0.28 s + 初始化 ~0.63 s） |

只"优化后落盘"就把 3.2~3.8 s 压到 2.1~2.3 s；再把批量维固定为 1，ORT 才能把 `Shape → Gather → Unsqueeze → Concat` 这类形状计算链折掉，节点从 5787 降到 3125，会话降到 0.9~1.0 s。`.ort` 格式没有额外好处。

## 5. 推荐方案

### 5.1 第一步（零依赖，立刻做）：worker 填 `model_type`

- 在 `sherpa.rs` 的 `OnlineModelConfig` 里设 `model_type: Some("zipformer2".into())`。收益：x-asr 480ms 5.4 → 3.1 s，multi-zh 2.8~3.1 → 1.5~1.9 s；**无新依赖、无精度变化、无体积变化**。
- 难点是 `model_type` 从哪来，不能写死给所有 transducer（zipformer v1 / conformer / lstm 的取值不同，填错会直接创建失败）。选项：
  1. **模型清单 `model.json` 加 `model_type` 字段**（清单里本来就有 `kind`、`files`），START 协议或 worker 自己读目录里的 `model.json`。当前三个流式模型都填 `zipformer2`。缺省不填，走 sherpa 的旧路径（慢但正确）。推荐。
  2. worker 读 encoder 文件尾部：本机三个文件的 `model_type` 元数据都在文件最后 ~400 字节内（原文件 387 / 385 字节，ORT 重存的 311 字节），可以只读尾部 4KB 找 `model_type` 键。取巧、依赖 protobuf 字段顺序，不推荐当主路径。
  3. 在 worker 里用 `ort` 读元数据：多一次建会话，抵消收益，不要。
- 离线识别器（`offline.rs`）同样没填 `model_type`，按源码同样会载两遍；离线各 kind 要填的字符串不同，**没验证**，本机也没装离线模型，列为后续。

### 5.2 第二步（加依赖，需批准）：encoder 预优化一次，缓存起来

目标：x-asr 480ms 加载 **≈0.8~1.0 s**（含 B），multi-zh ≈0.5 s。做法（实测过的流水线，本机 F 的做法）：

1. 对某个模型目录，用 ORT 以 `ORT_ENABLE_EXTENDED`、`AddFreeDimensionOverrideByName("N", 1)`、`SetOptimizedModelFilePath(<缓存目录>/encoder...onnx)` 创建一次会话（约 3.5 s，一次性），产出优化后的 encoder；decoder / joiner / tokens 不动。
2. 加载时先找缓存：键 = 原 encoder 的 sha256（清单里已有，或文件大小 + 修改时间）+ ORT 版本串（`1.28.0.20260724...`）+ 优化级别。命中就把优化后的路径交给 sherpa；未命中或 sherpa 创建失败，**回退到原 encoder**，并后台重新生成。
3. 缓存放自家数据目录（`%LOCALAPPDATA%\Cisox\models\stt\<id>\.opt\`），不写上游目录。
4. 谁来生成：下载完成后在后台由一个 `snow-stt --optimize <模型目录>`（子命令）生成一次，或者第一次听写时顺手生成。

代价与风险：

- **依赖**：snow-stt 目前只有 sherpa-onnx；要用 `ort`（仓库里 snow-translator / snow-table / snow-latex 已在用 `ort 2.0.0-rc.13`、`api-28`、`load-dynamic`，与 sherpa 同一份 `onnxruntime.dll`），或者自己写一小段 FFI 调 C API。按规定要**先问你**。我没核对 `ort` crate 的具体方法名（`with_optimized_model_path` / `with_dimension_override` 等），只核对了 C API：`SetOptimizedModelFilePath`、`SetSessionGraphOptimizationLevel`、`AddFreeDimensionOverrideByName`（v1.28.0 头文件）。
- **绑定 ORT 版本与批量=1**：优化后的文件含 `com.microsoft` 域的融合算子（如 `DynamicQuantizeMatMul`），官方只保证"同版本、同选项"。ORT 升级必须重新生成（所以缓存键里带版本串）。固定 N=1 的模型不能一次喂多路流；worker 现在只有一路流，没问题，但将来若同一进程要并发多路，这条路径要关掉。
- **硬件可移植性**：用 EXTENDED 而非 ALL 是官方文档的建议；**没有在第二台不同 CPU 上验证**。缓存放用户机器本地、在该机器上生成，规避了跨机器问题。
- **磁盘**：每个模型多约 150MB 的缓存（可在下载完成后生成，不可再生的原文件仍保留）。
- 发布方案（把预处理好的模型作为 release 附件发布，用户免生成）：省去本机生成，但 x-asr 的许可证清单里标"unverified"（见 stt-model-selection §10 第 7 条），**重新分发修改过的模型要先核对许可证**；且仍要 ORT 版本绑定。**不推荐优先走这条**。

### 5.3 不做的

- 不降优化级别、不调线程、不关预打包：没收益（§3.3）。
- 不换模型：O 的数据说明小模型 CER 差太多。
- 不做 `.ort` 格式、不做外部数据：没收益 / 没测到收益，且要改文件挑选与 sherpa 接口。
- 不改 ORT 构建（比如自编最小化）：本次数据里 1.28.0 自编 vs 1.28.2 没差别。

### 5.4 预期

| 场景（x-asr 480ms，`load_ms`） | 现状 | 做完 5.1 | 做完 5.1 + 5.2 |
| --- | --- | --- | --- |
| 本机、有背景负载（中位数 / 范围） | 5.4 s（3.9~6.7，另有 13.4 s 尖峰） | 3.1 s（2.7~5.0） | **0.86 s（0.7~1.5）** |
| multi-zh | 2.8~3.1 s | 1.5~1.9 s | 0.44~0.53 s |
| 峰值工作集 | 266 MB | 约同 | 233 MB |

按键到"麦在录"的总链路（dictation-cold-start §3.4）：x-asr 480ms 由 6.9~15 s 降到约 2.0~2.8 s（做完 5.1 + 5.2，加载 ~1 s + 开麦 0.8 s + UI 与 worker 启动 0.4~0.6 s）。再叠加"先开麦缓存"后，丢字窗口只剩 ~1~1.4 s。

## 6. 最小验证实验（实施后做）

**V1 单测（离屏）**：`model_type` 从清单读取的解析 / 缺省回退两条路径；缓存键命中 / 未命中 / 优化失败回退到原文件（Fake 后端 + 临时目录）。判据：全部通过。

**V2 等价性**：同一批音频（建议用 stt-model-selection 的评测集，而不是 TTS），原模型与优化后 encoder 各识别一遍，逐句 FINAL 文本比对。判据：文本一致率 100%，否则逐条看差异，CER 差 ≤0.1 个百分点。本轮只做了 TTS 合成的中英混合 74 s、7 句，**不能当作精度结论**。

**V3 加载计时**：A / B / 优化后 三种配置同一时段交替各 5~10 次，指标 `load_ms` 中位数与 p95、峰值工作集；分"页缓存热""清页缓存后"两栏，**要求机器空闲**（关掉编译与其它子代理）。判据：B 比 A 中位数 ≤ 0.65×；优化后 ≤ 1.2 s（空闲机器预计更低）。

**V4 ORT 升级回退**：用一个故意不匹配的缓存（改版本串 / 损坏文件）启动，断言会回退原文件且功能不变。

**V5 跨机器**：在另一台不同 CPU（最好 AMD、无 E 核）上跑 V3，确认 EXTENDED 缓存的收益与可加载性。

**V6 离线模式**：对 SenseVoice / paraformer / x-asr 离线分别测"填 `model_type` 前后"的加载，并确认各 kind 该填什么字符串。

## 7. 需要你决定的点

1. **是否同意 snow-stt 引入 `ort`（或 FFI）以做 §5.2 的预优化**。零依赖的 §5.1 本身就能 -40%，§5.2 再 -70%。
2. **`model_type` 的来源**：清单字段（推荐）还是读文件尾部。
3. **预优化的生成时机**：下载后台生成（推荐，用户无感）还是首次听写时生成（首次仍慢 ~3.5 s）。
4. **缓存放哪、占多少盘**：每个模型约 +150MB，是否接受；是否在换模型 / 卸载模型时一起删。
5. **要不要把预处理好的模型当 release 附件发布**：需先核对 x-asr 许可证（清单里标 unverified）。不推荐，列出供你决定。
6. **固定批量为 1 是否可接受**：现在只有单路流，没问题；将来要不要并发多路识别决定这条能不能长期用。

## 8. 未证实项

1. **与历史评测差 2 倍的原因**：只证实了"CPU 单线程慢且抖动"，没法在本机拆出机器差异与背景负载各占多少；没有在空闲机器和 Ryzen 7945HX 上复测。SNAC 的影响无法单独关掉。
2. **本机测量始终有背景负载**（其它进程在编译 / 跑脚本）；绝对数字偏高，且每项最多 3 次，看量级与排序。
3. **优化后模型的跨机器 / 跨 ORT 版本兼容**：只在本机同一台机器、同一版 ORT 1.28.0 上验证过可加载；EXTENDED 的可移植性来自官方文档，没在第二台 CPU 上测。
4. **精度**：只比了 TTS 合成的 74 s 中英混合（7 句）与 multi-zh 的 6 句，全部 FINAL 逐字一致；没有用评测集跑 CER。
5. **RTF 是否变好**：长音频 3 轮 RTF 为原版 0.078~0.114、优化版 0.069~0.081，方向是不变或略好，但噪声大（F 版有一次因背景负载跑出 0.204），**不能当作"推理变快"的证据**。
6. **离线模式（SenseVoice / paraformer / x-asr 离线）**：按 sherpa 源码也载两遍，没实测，各 kind 的 `model_type` 字符串没核对；本机没有装离线模型。
7. **x-asr 160ms** 的加载没单独测（encoder 体积与 480ms 相同，按同量级处理）。
8. **未测的选项**：`session.prepack.enable_parallel`、`session.use_env_allocators`、`session.use_memory_mapped_ort_model`、外部数据 / 内存映射权重（sherpa 不暴露会话配置，只能走自己建会话的路）。
9. **`ort` crate 的 API 名**没核对，只核对了 C API。
10. **sherpa 的行为读的是 master 分支源码**，不是 v1.13.8 标签；实测与之一致。
11. **bilingual-2023-02-20 的 0.85~0.89 s** 是历史评测数据（另一台机器），没在本机复测，也没读它的图节点数。
12. 机械盘、网络盘、杀软扫描 DLL / 模型的情形没测；冷缓存只在 NVMe 上做了 3 次。

## 9. 参考资料

- 仓库：`tools/snow-stt/src/{main,sherpa,backend,offline}.rs`、`crates/snow-shot/src/dictation/{client,config}.rs`、`scripts/build-snow-stt.ps1`（ORT 版本替换）、`tools/snow-translator/Cargo.toml`（已用 `ort`）。
- 文档：[`dictation-cold-start.md`](dictation-cold-start.md) §3.1 §7 E5 §9、[`stt-model-selection.md`](stt-model-selection.md) §2.1 §4、[`../guides/snow-stt-worker.md`](../guides/snow-stt-worker.md)「构建」「DLL 加载顺序」。
- 网络：见 §4.1 的链接。

## 10. 探针与复现

位置（`scratchpad` = `C:\Users\jelon\AppData\Local\Temp\claude\E--workspaces-Cisox\db6a3f3d-a8af-4d7a-8ed1-b765b6155afa\scratchpad`），**不入仓库**：

- `sttload\ortprobe.py`：ctypes 直接调 worker 同版 `onnxruntime.dll` 的 C API，量读文件 / 建会话；`--level`、`--intra`、`--cfg k=v`、`--save`（离线优化落盘）、`--freedim N=1`、`--profile`（读 ORT 的 `model_loading_uri` / `session_initialization`）、`--frombuf`。
- `sttload\rprobe\`：独立小 crate（sherpa-onnx `=1.13.8`，与 snow-stt 同 `Cargo.lock`、同静态 CRT），`rprobe.exe <模型目录> <model_type 或 -> <encoder 路径或 -> [线程]`，量 `OnlineRecognizer::create` 与首次解码。构建脚本 `rprobe\build.ps1`（产物在 `build\stt-probe`，已被忽略）。
- `sttload\loadrun.ps1`：驱动 `snow-stt.exe --wav --stats`，打印 `load_ms`、RTF、FINAL 条数与全部 FINAL 的 SHA（可选 `-Affinity`、`-Priority`）。
- `sttload\nodes.py`（读 ONNX 节点 / 初始值 / opset）、`simp.py`（onnxsim 固定 N=1）、`shload.py`（Python sherpa 对照）、`cold.ps1`（清缓存 + 冷读 + 冷热加载）。
- 产物：`sttload\{opt2,opt99,fd2,sim,simopt2,ortfmt}\`（各种优化后的 encoder）、`m480opt2\`、`m480simopt2\`、`m480fd2\`、`mz*\`（可直接给 worker 当模型目录）、`speech.wav` / `speech_zh.wav` / `long.wav`（Windows TTS 合成）。Python 依赖（onnxsim 0.8.1、onnxruntime 1.31.0）装在 `sttload\pylib`，只给 `simp.py` 用。
- 复现 F：`python ortprobe.py <ORT dll 目录> <原 encoder> --level 2 --freedim N=1 --save <输出路径>`，再 `rprobe.exe <原模型目录> zipformer2 <输出路径>`。
- 没有动任何系统设置、注册表或用户数据目录（只读了 `%LOCALAPPDATA%\Cisox\models\stt`，所有产物都在 scratchpad 与 `build\stt-probe`）；**唯一的整机级动作是 3 轮清备用列表（§3.2）**。没有启动 `snow-shot.exe` / `snow-shot-ui.exe`，没有弹窗，没有打开麦克风（全部用 `--wav` 与离线探针）。没有修改 `tools/snow-stt` 源码。
