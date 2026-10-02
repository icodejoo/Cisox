# 实时语音转文字（STT）后端调研

调研日期 2026-10-02。范围：只调研与设计，不含仓库实现。需求（用户已定）：快捷键激活独立进程，实时语音转文字，结束即退出；触发支持「切换式」与「按住说话」；输出实时键入当前焦点输入框；引擎同时支持 Windows 系统语音 API 与本地模型，架构参考翻译/OCR 的 backend 抽象（见 [system-ocr-translate-backends.md](system-ocr-translate-backends.md)）。裁决依据 [principles.md](../principles.md)。

标记约定：【实测】= 本机跑出的数据；【已核实】= 读到一手来源（官方文档/源码/发布包）；【二手】= 非官方来源；【未验证】= 推断或没有证据，不能当事实。仅验证了 Windows，其他平台不在范围（红线）。

## 0. 结论摘要

| 问题 | 结论 |
|---|---|
| sherpa-onnx 能与现有 ort 共用一份 onnxruntime.dll 吗 | **版本上能对上**：sherpa 1.13.8 发布包自带 ORT 1.28.2【已核实】，本仓库 ort rc.13 开了 `api-28`（对应 ORT 1.28）【已核实】。sherpa 的 C API DLL 按名字导入 `onnxruntime.dll`【实测】，用仓库内的 ORT 1.28.0 DLL 替换后 sherpa 能加载并报版本【实测】。但「替换后真的建 recognizer 跑识别」没测【未验证】。 |
| 不用 sherpa，纯 ort 自研可行吗 | **可行，且已跑通**：一个 ~300 行的探针（含 WAV/内存统计）用 ort rc.13 + 手写 Kaldi fbank + 贪心 transducer 解码，加载 sherpa 的流式 zipformer 中英双语 int8 模型，识别出中英混说的样例【实测】。真正的工作量在 VAD/端点、标点、多模型家族适配，不在核心推理。 |
| 本地模型实测成绩（2 线程，BelowNormal） | 常驻内存峰值 ~253MB；每 320ms 音频块计算 p50 31~37ms，RTF 0.105~0.135【实测】。 |
| Windows 系统语音 | 三条路：`Windows.Media.SpeechRecognition`（流式，有 hypothesis，但**要求开「联机语音识别」**，且只吃默认麦克风）；SAPI/`System.Speech`（真离线、有流式 hypothesis，但本机测两段样例准确率很差）；Windows AI `SpeechRecognitionModel`（真离线+流式，但需 Win11 24H2 + WinAppSDK + MSIX 打包）。 |
| 键入焦点框 | 默认 `SendInput(KEYEVENTF_UNICODE)`；剪贴板粘贴作降级（长文本/特殊目标）。管理员窗口被 UIPI 拦，**且 SendInput 失败时不报错**，必须自己探测。 |
| 按住说话 | 仓库用的 global-hotkey 0.8.0 **已经**会发 `Released` 事件（RegisterHotKey 收到按下后起线程每 50ms 轮询 `GetAsyncKeyState`）；`snow-ui-shell` 的 `hotkey.rs` 目前把非 `Pressed` 事件丢掉了，补转发即可。 |
| 推荐 | 第一期：**纯 ort + 流式 zipformer（自研推理壳）** + 系统后端占位；sherpa 作为后续可选后端（shared 模式共用同一份 ORT DLL）。详见 §7。 |

## 1. sherpa-onnx 与现有 ort 共用 onnxruntime.dll

### 1.1 版本
- sherpa-onnx 仓库 `cmake/onnxruntime-win-x64.cmake` 指向 `onnxruntime-win-x64-…-1.28.2`（从 csukuangfj/onnxruntime-libs 下载，仅 shared）【已核实：[源码](https://github.com/k2-fsa/sherpa-onnx/blob/master/cmake/onnxruntime-win-x64.cmake)】。
- 发布包 `sherpa-onnx-v1.13.8-win-x64-shared-MD-Release-no-tts-lib.tar.bz2`（6.9MB）【实测：下载解包】内含：`onnxruntime.dll` 17.1MB（文件版本 1.28.2）、`onnxruntime_providers_shared.dll`、`sherpa-onnx-c-api.dll` 2.9MB、`sherpa-onnx-cxx-api.dll` 0.13MB。来源：[Releases](https://github.com/k2-fsa/sherpa-onnx/releases)。
- 本仓库：`snow-translator` 的 `ort = "=2.0.0-rc.13"`，特性 `load-dynamic`+`api-28`，并 patch 到 mg-chao fork【已核实：`tools/snow-translator/Cargo.toml`】。docs.rs 称 rc.13 是 "safe Rust wrapper for ONNX Runtime 1.28"，默认 `api-27`，`api-28` 依赖 `api-27`【已核实：[docs.rs features](https://docs.rs/crate/ort/2.0.0-rc.13/features)】。推断：`api-28` 需要运行时 ≥1.28【未验证，没读 ort 源码确认判定逻辑】。本机仓库 `build/mt-quant/ort128/…/onnxruntime.dll`（文件版本 1.28.0.20260724，17.8MB，来自某次量化实验，不在 git 里）被 ort rc.13 成功加载【实测】。
- 结论：两边都落在 ORT 1.28 系，同一个小版本线，**不存在版本冲突**。

### 1.2 外部/动态 ORT
- sherpa 构建期支持预装 ORT：环境变量 `SHERPA_ONNXRUNTIME_INCLUDE_DIR`/`SHERPA_ONNXRUNTIME_LIB_DIR`，CMake 开关 `SHERPA_ONNX_USE_PRE_INSTALLED_ONNXRUNTIME_IF_AVAILABLE`【已核实：[onnxruntime.cmake](https://github.com/k2-fsa/sherpa-onnx/blob/master/cmake/onnxruntime.cmake)】。
- 运行期：`sherpa-onnx-c-api.dll` 的导入表含 `onnxruntime.dll`（按名字，非绝对路径）【实测：在二进制里扫到该字符串】。
- 实测：新建目录只放 `sherpa-onnx-c-api.dll` + 仓库里的 ORT 1.28.0 `onnxruntime.dll`，Python ctypes 加载 c-api 成功，`SherpaOnnxGetVersionStr()` 返回 `1.13.8`，进程里的 `onnxruntime.dll` 模块路径确为替换的那份【实测】。**未做**：用替换 DLL 实际创建 recognizer 并识别（会覆盖更多 ORT API 符号，不能由此推出完全兼容）【未验证】。
- 注意「共用」的含义：STT 是独立 worker 进程，一个进程里只会走 sherpa 或 ort 其中之一。共用的实际收益是**安装包里只放一份 onnxruntime.dll**（翻译、OCR、STT 同一份），不是进程内省内存。

### 1.3 Rust 绑定与许可证
- crate `sherpa-onnx` 1.13.8（2026-09-11），包装 C API；依赖 `serde`、`serde_json`、`sherpa-onnx-sys =1.13.8`【已核实：[docs.rs](https://docs.rs/crate/sherpa-onnx/latest)】。特性 `static`（默认，首次构建自动下载原生库）与 `shared`；可用 `SHERPA_ONNX_LIB_DIR` 指向自带库【已核实：[rust-api-examples README](https://github.com/k2-fsa/sherpa-onnx/tree/master/rust-api-examples)】。另有社区包装 `sherpa-rs`、`sherpa-transducers`、`wavekat-asr`【二手：[搜索结果](https://lib.rs/crates/sherpa-transducers)】。
- 许可证：仓库 Apache-2.0【已核实：仓库页脚】；Rust crate 自身 license 字段本次没读到（docs.rs 页未显示）【未验证】。
- 代价（对照总原则）：`static` 模式下构建时联网下载原生库、sherpa 静态库里是否另带一份 ORT 没验证【未验证】；`shared` 模式新增 2.9MB DLL。两者都比「纯 ort」多一个外部二进制依赖，违反「少编译依赖」。收益：现成的 VAD、端点检测、热词、Paraformer/Whisper/SenseVoice 等多模型家族、已验证过的 fbank。

## 2. 纯 ort 自研：工作量与风险

### 2.1 先搜成熟 Rust 库（结论：核心推理壳无需新依赖，辅助件可复用仓库内实现）
| 件 | 现成方案 | 评价 |
|---|---|---|
| ASR 推理 | sherpa-onnx（Rust 绑定）、`parakeet-rs`、`qwen3-asr` 等【二手：[crates.io 关键词页](https://crates.io/keywords/speech-recognition)】 | 都带各自的 ORT/原生库，会与仓库 ort fork 重复；未逐个评估 |
| VAD | `silero_vad_rs`、`silero-vad-rust`、`voice_activity_detector`（Silero，用 ort）【二手：[silero-vad-rs](https://docs.rs/silero-vad-rs)、[voice_activity_detector](https://lib.rs/crates/voice_activity_detector)】 | 依赖的 ort 版本很可能与仓库 `=2.0.0-rc.13` fork 冲突【未验证】；Silero ONNX 本身只有 ~2MB 量级【未验证】，自己用已有 ort 跑更干净 |
| fbank/log-mel | 没找到成熟、轻量、与 Kaldi 对齐的纯 Rust 库【未验证：搜索未命中，不排除存在】 | 自写：本探针 ~90 行（FFT+mel+窗），见 §2.3 |
| 重采样 | 仓库 `snow-crates/crates/snow-audio-recorder/src/convert.rs` 已含重采样与声道转换（`input.sample_rate`→`output.sample_rate`） | **直接复用** |
| WASAPI 采集 | 同一 crate：`platform/windows/wasapi_source.rs`、`device_enum.rs`，`DeviceSelector::DefaultCapture`、`streaming.rs` 的 `AudioStreamHandle`（`recv/pause/resume/stop`） | **直接复用**，不用 cpal（cpal 也不必引入）。是否已按麦克风场景测过、事件里的采样格式与 16k 单声道的衔接，需接入时确认【未验证】 |

### 2.2 各环节工作量与风险
| 环节 | 工作量 | 主要风险 |
|---|---|---|
| fbank | 小：~90 行 | 与训练端不一致会静默降质。本探针做对了三处才出文字：样本归一化到 [-1,1]（sherpa `normalize_samples`）、`snip_edges=false`（帧中心对齐、边缘反射）、尾部补零冲刷。需对 sherpa 输出做 golden 对比【未做】 |
| 流式 cache | 中：zipformer 有 36 个状态张量（`cached_len/avg/key/val/conv1/conv2_*`），名字规律是 `new_<输入名>`；探针按输入名/形状**泛化**地管理，没写死模型结构 | 不同模型家族状态布局不同，每家族一份适配 |
| 解码 | 小：贪心 transducer ~30 行（context=2、blank=0、每帧最多 3 个符号） | 无 beam/热词/ITN；英文 BPE 的 `▁` 要还原成空格 |
| VAD/端点 | 中 | 需另接 Silero 或用「模型自带端点（空白计数）」；按住说话模式其实不需要 VAD（松开即结束），**切换式**才需要静音超时 |
| 标点/大小写 | 本模型**不输出标点**【实测：样例输出无标点】 | 要标点得另接 CT-Transformer 类小模型【未验证】，建议第一期不做 |
| 重采样/采集 | 复用 | — |
| 总体 | 第一期推理壳估 2~3 人日（含单测与 golden），其余按模型家族累加【推断】 | 多模型家族后维护成本上升，此时 sherpa 才划算 |

### 2.3 实测方法与结果（一次性探针，已删除）
**环境**：Intel Core i5-13500，31.7GB 内存，Windows 10 专业版 19045；`cargo build --release -j 2`；进程以 **BelowNormal**（`SetPriorityClass 0x4000`）运行；无独显依赖，纯 CPU EP；ORT 为 1.28.0.20260724 的 `onnxruntime.dll`（动态加载）；ort `=2.0.0-rc.13`（crates.io 原版，特性与 translator 相同：`std ndarray api-28 load-dynamic`，没套 mg-chao fork）。探针工程在系统临时目录，未进仓库。

**模型**：[csukuangfj/sherpa-onnx-streaming-zipformer-bilingual-zh-en-2023-02-20](https://huggingface.co/csukuangfj/sherpa-onnx-streaming-zipformer-bilingual-zh-en-2023-02-20)（HF 页标 Apache-2.0）。用的文件：encoder int8 181.9MB、decoder fp32 13.9MB、joiner int8 3.2MB（共 ~199MB，用 aria2c `-x16 -s16 -k8M` 下载）。模型元数据：`T=39`、`decode_chunk_len=32`（即每块 320ms 音频、前瞻 7 帧）、编码器 2,4,3,2,4 层、维度 384。

**算子兼容**：三个 ONNX 在 ort rc.13 + ORT 1.28 CPU EP 下全部建会话成功并正常推理（int8 编码器含量化算子，无报错）【实测】。

**结果**（样例为该模型仓库自带的 3 段中英混说 wav，共 ~19.8s）：

| 项 | 2 线程 | 1 线程 |
|---|---|---|
| 会话加载（三个模型） | 1.4s（热）；**首次 49.9s**（刚下载的文件首次加载，含冷磁盘/杀软扫描等，没拆开定位原因） | 1.4s |
| 常驻内存（工作集峰值 / 提交） | ~253MB / ~245MB（整个探针进程，含三个会话） | ~252MB / ~244MB |
| 每块（320ms 音频）计算耗时 p50 / p95 | 31~37ms / 40~46ms | 45ms / 55ms |
| RTF | 0.105~0.135 | 0.16~0.20 |
| 首块离群值 | 1 线程某次最大 513ms（预热，一次性） | 同左 |

**端到端延迟推算**【推断，由上面数据+模型结构算出，未用麦克风实测】：说话到出字 ≈ 块长 320ms + 前瞻 70ms + 计算 ~35ms ≈ 0.4~0.45s；按一块一出字的节奏，文字会以 ~3 次/秒的频率更新。

**识别内容**（只是肉眼核对，没算 WER，没与 sherpa 官方输出逐字对比）：`0.wav` → `昨天是 MONDAY TODAY IS LIBRAR THE DAY AFTER TOMORROW是星期三`；`1.wav` → `这是第一种第二种叫呃与 ALWAYS ALWAYS什么意思啊`；`2.wav` → `这个是频繁的啊不认识记下来 FREQUENTLY频繁的`。中英混说基本可用；第一条的 `LIBRAR` 疑似我的 fbank 与参考实现仍有细小差异或是模型本身错误，**需要 golden 对比才能定论**【未验证】。

**没测的**：fp32 模型（按项目约定评测不测 int8/fp32 对比，且此处只测了 int8）；真实麦克风采集链路；DirectML/GPU（不建议，见 §3）。

## 3. 候选本地模型

权重不随仓库打包，按需下载（沿用 OCR/翻译的资产清单+校验模式）。

| 模型 | 流式 | 语言 | 体积（int8） | 许可证 | 备注 |
|---|---|---|---|---|---|
| sherpa 流式 Zipformer 中英双语 2023-02-20 | 是（320ms 块） | 中/英 | encoder 182MB + decoder 14MB（fp32）+ joiner 3MB ≈ 199MB | Apache-2.0【已核实：HF 页】 | **本次实测对象**。官方文档列 fp32 共 ~342MB、int8 ~190MB【已核实：[文档](https://k2-fsa.github.io/sherpa/onnx/pretrained_models/online-transducer/zipformer-transducer-models.html)】；训练数据为社区贡献的内部数据集，具体来源未披露 |
| sherpa 流式 Zipformer small 中英 2023-02-16 | 是 | 中/英 | encoder int8 ~41MB、decoder ~3.4MB【二手：搜索摘要】 | 未读到【未验证】 | 体积小一个量级，更贴合「小体积」；本次尝试下载失败（仓库路径/权限问题，HF 返回 401），**未实测** |
| 流式 Paraformer 中英 | 是 | 中/英 | encoder int8 158MB + decoder int8 68MB【二手】 | Apache-2.0【已核实：[HF 页](https://huggingface.co/csukuangfj/sherpa-onnx-streaming-paraformer-bilingual-zh-en)】 | 官方 RTF 0.21【二手】；原始模型来自 ModelScope/FunASR，上游模型许可可能另有条款【未验证】 |
| Whisper tiny/base（sherpa 导出） | **否**（整段/滑窗伪流式） | 多语 | tiny int8 ~74MB、base int8 ~140MB【二手】 | MIT（OpenAI 权重）【常识，本次未读到一手页】 | whisper.cpp 的 `--stream` 实际延迟 0.5~2s，慢机器上更差【二手：[讨论](https://github.com/ggml-org/whisper.cpp/discussions/3567)】；不适合「实时键入」，可作为「说完一次性出整段」的备选档 |
| Moonshine Streaming（Tiny 34M/Small 123M/Medium 245M 参数） | 是 | 主要英文，另有部分语种变体 | tiny 约 95MB（INT8 k-quant ORT 构建）【二手】 | 称"宽松许可"，具体条款未核实【未验证】 | 中文支持未验证，不是首选 |

倾向：首期用 Zipformer 中英双语（已实测通），并评估 small 版换小体积。GPU/DirectML 不建议：流式小块推理 CPU 已 RTF≈0.1，GPU 传输与调度开销对 320ms 小块反而不划算，且抢录屏的 GPU（总原则「高 fps」）【推断】。

## 4. Windows 系统语音 API

本机事实【实测】：Windows 10 19045，系统语音语言 `zh-Hans-CN`；`SpeechRecognizer.SupportedTopicLanguages` = `en-US, zh-Hans-CN`；已装 SAPI 桌面识别器 `MS-1033-80-DESK`（en-US）、`MS-2052-80-DESK`（zh-CN）；注册表 `HKCU\…\Speech_OneCore\Settings\OnlineSpeechPrivacy` 无 `HasAccepted` 值（即未开联机语音识别）。

| 选项 | 离线 | 语言 | 流式 | 延迟/质量 | 门槛 |
|---|---|---|---|---|---|
| `Windows.Media.SpeechRecognition.SpeechRecognizer`（连续听写 `ContinuousRecognitionSession`） | **不是纯离线**：文档要求语言包 + "Online speech recognition" 开启；自定义语法约束才在本机处理【已核实：[Enable continuous dictation](https://learn.microsoft.com/en-us/windows/apps/design/input/enable-continuous-dictation)、[搜索摘要](https://github.com/longbridge/gpui-kit/pull/3333)】 | 本机 en-US、zh-Hans-CN【实测】。文档称桌面 PC 听写"仅 en-US"【已核实】，与本机列表冲突，以实测为准但**未实测中文听写是否真能出字**（需要对着麦克风且开联机开关） | 是：`HypothesisGenerated`（实时假设）+ `ResultGenerated`（定稿）【已核实】 | `CompileConstraintsAsync` 28ms【实测】；识别延迟与质量未测 | 只吃默认麦克风，**无法喂 PCM**【已核实：gpui-kit PR 同样说明"pushed PCM only drives waveform"】。联机开关违背"离线"预期；不开则不可用 |
| SAPI / `System.Speech.Recognition.SpeechRecognitionEngine` | 是，纯本机 | 已装 en-US、zh-CN【实测】 | 是：`SpeechHypothesized`，且可 `SetInputToAudioStream` 喂 PCM（所以能复用 WASAPI 采集）【已核实 API 存在；喂流方式未测】 | 对两段样例（`1.wav`/`2.wav`，中英混说）：zh-CN 输出 `''` 与 `这是叶儿抿平安大`，en-US 输出乱码英文；耗时 370~870ms/段【实测】。样例是中英混说，对 SAPI 不公平，**但足以说明质量远不及 §2 的本地模型** | 无新依赖（.NET Framework 的 System.Speech 在 Rust 里需走 COM `ISpRecognizer`，工作量不小，未评估）；微软已把方向转向 Voice Access/Windows AI【推断】 |
| Windows AI APIs `SpeechRecognitionModel`（App SDK） | 是，纯设备端 | 文档未列语言表【未验证】 | 是：`StreamingRecognition`，定稿式（"as complete phrases are recognized"），无逐词 hypothesis | 未测 | Win11 24H2(26100)+、WinAppSDK ≥1.7.1、**MSIX 打包并声明 `systemAIModels`**；NPU 机预装，CPU 机需经 Windows Update 按需下载模型；GPU 不支持【已核实：[Speech Recognition](https://learn.microsoft.com/en-us/windows/ai/apis/speech-recognition)】。本机 Win10 不可用，打包形态也与本项目冲突 |
| Azure/云 Speech Services | 否 | — | — | — | 违背离线与隐私，不纳入 |

「Speech Services 离线语音包」：未找到面向桌面 Win32 的公开离线包/API 一手资料，**未验证**，不作为选项。

结论：Windows 系统后端在本项目里更适合当 **"零下载的兜底档"**：优先 `SpeechRecognizer`（有 hypothesis，门槛低但要联机开关，UI 要给出禁用态+原因，符合 principles §5），SAPI 只作为"确实没有联机开关又要离线"的最后手段。本地 Zipformer 才是质量档。

## 5. 键入焦点输入框

### 5.1 方案对比
| 维度 | `SendInput(KEYEVENTF_UNICODE)` | 剪贴板 + Ctrl+V |
|---|---|---|
| 原理 | 逐字符合成 Unicode 键事件（`wVk=0, wScan=UTF-16 码元`），目标收到 `WM_CHAR` 路径的字符 | 写剪贴板、合成 Ctrl+V |
| 流式回删重打 | 容易：发 `Backspace` 再发新字符 | 难：每次修正都得整段粘贴，且需要选中/回删 |
| 是否污染剪贴板 | 否 | 是，必须保存/恢复（仓库 `snow-crates/crates/snow-selected-text/src/platform/windows/clipboard.rs` 已有快照/恢复与 `SendInput` 注入 Ctrl+C 的实现，可复用其思路） |
| 速度 | 长文本（数百字）逐字符发较慢，但流式每次只补几个字，足够 | 一次性、最快 |
| 兼容性 | 绝大多数 Win32/UWP/浏览器输入框可用；部分游戏、远程桌面客户端、用 `WM_KEYDOWN`+`VK` 判断的控件不吃无 VK 的 Unicode 事件【推断，未逐一测】 | 绝大多数能粘；个别禁用粘贴的输入框不行 |
| 代理对（emoji） | 需要拆成两个 UTF-16 码元分别发 | 无问题 |

### 5.2 文档确认的边界
- `SendInput` 受 UIPI 限制：只能向**同级或更低完整性级别**的进程注入；**被 UIPI 拦时返回值和 `GetLastError` 都不会指出是 UIPI**，且返回 0 表示被其他线程阻塞【已核实：[SendInput](https://learn.microsoft.com/en-us/windows/win32/api/winuser/nf-winuser-sendinput)】。
- 它"不重置键盘状态"，已按下的键（例如用户仍按着热键里的修饰键）会干扰合成事件，建议发之前用 `GetAsyncKeyState` 检查并等修饰键松开【已核实：同页】。**按住说话**时，用户松开热键前不要键入最终文本，或只键入"不受修饰键影响"的 Unicode 字符——Unicode 事件不依赖修饰键状态，但目标程序可能把 Ctrl/Alt 仍按着的状态当成快捷键，需实测【未验证】。
- 同一次 `SendInput` 里的事件串行插入，不会与用户输入交错【已核实：同页】——所以回删+重打应放进**同一次调用**，避免视觉闪烁与被用户按键打断。

### 5.3 降级与检测
1. **管理员窗口 (UIPI)**：前台窗口的进程完整性级别高于本进程时 `SendInput` 静默失败。探测：对比前台窗口所属进程与自身的令牌完整性级别（`GetForegroundWindow`→`GetWindowThreadProcessId`→`OpenProcessToken`→`GetTokenInformation(TokenIntegrityLevel)`）；不满足则 UI 显示禁用态+原因（principles §5），可选"复制到剪贴板"作为替代。**不要**为此默认提权，违背最小权限【推断】。
2. **IME 组合态**：中文输入法下，Unicode 事件通常绕过组合、直接上屏，不会进入拼音组合串【推断，未测】。若目标正处于组合中（有未上屏的拼音），回删会删到组合串而非已上屏文字。可用 `ImmGetContext`+`ImmGetCompositionString` 或 TSF 检测组合态，有组合态时先暂缓键入【未验证】。
3. **游戏/远程桌面/虚拟机**：全屏独占游戏、部分远程桌面客户端、反作弊保护的程序会忽略合成输入。降级为"只显示实时字幕浮层 + 结束后复制到剪贴板"。无法自动识别时给用户一个"目标不接收键入"的手动提示【推断】。
4. **粘贴降级**：Unicode 失败（`SendInput` 返回数小于事件数）或用户配置为粘贴模式时，改用"保存剪贴板→写入文本→Ctrl+V→恢复剪贴板"；此时放弃逐字符流式，改为"每个定稿片段粘一次"。

### 5.4 流式结果修正（回删重打）
模型每块输出的是"当前假设"，已键入的尾部可能被后续块修正。策略（推荐）：
- **稳定前缀法**：把输出分为"已定稿"和"可变尾部"。只键入已稳定的前缀；可变尾部不上屏，只在字幕浮层显示。稳定判据：同一前缀在连续 N 块里不变，或模型端点（空白持续）触发定稿。适合 transducer（贪心解码一般只在末尾变化）。
- **回删重打法**：发现新假设与已键入文本的公共前缀长度为 L，则发 `(已键入长度 − L)` 个 Backspace + 新后缀，**放进一次 `SendInput`**。风险：用户在中途把光标移走/切窗口，会删错地方。缓解：键入前后校验前台窗口句柄未变，变了就停止并提示【推断】。
- 默认采用稳定前缀法，回删范围限制在最近 ≤N 个字符（如 8），超过的视为已提交，不再回改。
- 本模型 transducer 贪心解码天然是"只追加"：已发出的符号不会被改写（见探针输出逐块只增长）【实测：解码代码里 `ids` 只 push，无回退】，所以对本模型回删需求其实很少；回删主要服务于将来的 Paraformer/Whisper 类整句重写模型和系统后端（`SpeechRecognizer` 的 hypothesis 会被改写）。

## 6. 按住说话 / 切换：全局热键

### 6.1 仓库现状【已核实】
- `snow-ui-shell/src/hotkey.rs` 的 Windows 后端：专用线程持有 `global_hotkey::GlobalHotKeyManager`，泵消息，收到事件按 `by_platform_id` 查登记表后 `dispatcher.send(CommandSource::Hotkey, command)`。
- 关键一行：`if ev.state() != HotKeyState::Pressed { continue; }`（约 489~491 行）——**松开事件被丢弃**。
- global-hotkey 0.8.0 在 Windows 上：`RegisterHotKey` 收到 `WM_HOTKEY` 后发 `Pressed`，**随即起一个线程每 50ms 轮询 `GetAsyncKeyState(vk)`，键松开时发 `Released`**，源码注释写明是为按住说话（push-to-talk）留的，并避免空转烧核【已核实：`global-hotkey-0.8.0/src/platform_impl/windows/mod.rs`，`global_hotkey_proc`；对应 [tauri-apps/global-hotkey#176](https://github.com/tauri-apps/global-hotkey/issues/176)】。
- `RegisterHotKey` 本身不报告松开；`MOD_NOREPEAT` 抑制自动重复（Windows 7+）；F12 被调试器保留，不可注册；已被占用时注册失败【已核实：[RegisterHotKey](https://learn.microsoft.com/en-us/windows/win32/api/winuser/nf-winuser-registerhotkey)】。

### 6.2 方案
| 模式 | 流程 |
|---|---|
| 按住说话 | `Pressed` → 主进程拉起 STT worker 并开始采集；`Released` → 通知 worker "结束"，worker 冲刷尾部、键入最终文本后退出。松开延迟最坏 ~50ms（轮询周期），可接受 |
| 切换式 | 第一次 `Pressed` → 启动 worker；再次 `Pressed` → 通知结束；另加"静音超时自动结束"与"Esc 取消"（Esc 用临时注册的热键，worker 存活期间才注册，结束即注销） |

要点：
- 热键由**常驻主进程**持有（它本来就常驻），STT worker 不自己注册热键，由主进程通过 IPC 发 `Start/Stop/Cancel`（沿用 OCR/翻译 worker 的命名管道协议风格，见 `snow-ocr-protocol`）。这样 worker 保持"按需启动、结束即退出"。
- 仓库改动面小：给 `HotkeyBinding`/dispatcher 增加"按下/松开"区分（例如 `CommandSource::Hotkey` 之外加 `HotkeyEdge`），不改 global-hotkey 版本。
- 极端情况：Released 事件丢失（例如进程被挂起）→ worker 侧设最长录音时长上限（如 60s）与心跳；主进程崩溃 → worker 发现管道断开即退出（总原则「不常驻」）。
- 热键含修饰键时，`GetAsyncKeyState` 只跟踪主键 vk；用户先松修饰键、后松主键，以主键松开为准【推断，由源码行为得出，未实测】。
- 备选：低级键盘钩子（`WH_KEYBOARD_LL`）可直接拿 KeyUp，但要求持续泵消息、受杀软敏感、延迟敏感；global-hotkey 的 50ms 轮询方案已满足需求，不引入钩子。

## 7. 推荐方案与备选

### 7.1 对比表
| 维度 | A 纯 ort + Zipformer（推荐首期） | B sherpa-onnx（shared） | C Windows `SpeechRecognizer` | D SAPI |
|---|---|---|---|---|
| 质量 | 中英混说可用【实测，未算 WER】 | 同模型下同级；多模型家族可选 | 未测 | 差【实测 2 段】 |
| 延迟 | 计算 ~35ms/320ms 块，端到端推算 0.4~0.45s | 同级（同引擎） | 未测 | 370~870ms/段（非流式调用） |
| 内存 | ~253MB 峰值（2 线程） | 同级 | 系统服务承担，未测 | 未测 |
| 体积/下载 | 模型 ~199MB（或 small ~45MB 待测）；零新增 DLL | 同模型 + 2.9MB DLL | 0 | 0 |
| 新增依赖 | 无（复用 ort、snow-audio-recorder） | 新 crate + 外部二进制 | `windows` crate 特性（已在树内概率高，待查） | COM 绑定 |
| 离线 | 是 | 是 | **否**（需联机语音识别开关） | 是 |
| 门槛 | 下载模型 | 下载模型 | 语言包+隐私开关 | 语言包 |
| 总原则契合 | 最高（少依赖、复用） | 中（多一份原生依赖） | 高（系统自带）但离线性存疑 | 中 |

### 7.2 抽象
仿翻译的 `TranslationEngine`/OCR 的 backend 抽象，在 `snow-capability` 声明能力，不满足时 UI 禁用+原因：

```
trait SttEngine {
    fn capability() -> Capability;                 // 是否可用及原因
    fn start(&mut self, cfg: SttConfig) -> Result<()>;
    fn feed(&mut self, pcm16k_mono: &[f32]) -> Result<()>;   // 本地后端用
    fn poll(&mut self) -> Vec<SttEvent>;           // Partial(text) / Final(text) / Endpoint
    fn finish(&mut self) -> Result<Vec<SttEvent>>; // 冲刷尾部
}
```
系统后端 C（只吃默认麦克风）的 `feed` 为空实现，自己内部采集——抽象要允许"后端自带采集"。配置键仿 `screenshot_translation/backend`：`speech_to_text/backend`（`local`|`system`）、`…/local_model_id`、`…/trigger_mode`（`hold`|`toggle`）、`…/type_mode`（`unicode`|`paste`）。

### 7.3 分阶段计划
1. **P1 本地后端闭环（hold 模式）**：worker 进程（独立 workspace，同 `snow-translator` 做法）+ 复用 `snow-audio-recorder` 采集 → 16k 单声道 → 探针里的 fbank/解码整理成库；`SendInput` Unicode 追加键入；`hotkey.rs` 转发 Released。验收：1 线程 BelowNormal 下 RTF<0.3、峰值内存<300MB；与 sherpa 官方输出做 golden 对比（fbank 误差、文本一致率）。
2. **P2 切换式 + 端点 + 降级**：静音超时/Esc 取消、UIPI 探测与禁用态、粘贴降级、稳定前缀法与回删。
3. **P3 模型管理**：资产清单/下载/校验（沿用 OCR 方式），评估 small 版与量化；评估标点模型。
4. **P4 系统后端**：`SpeechRecognizer`（hypothesis 经稳定前缀法接入）；联机开关检测与禁用态文案；SAPI 是否做另议。
5. **P5（按需）sherpa 后端**：仅当要支持 Paraformer/Whisper/SenseVoice 等多家族时再引入，走 shared 模式、与 ort 共用同一份 `onnxruntime.dll`；先补做 §1.2 未验证的"替换 DLL 后真实识别"。

### 7.4 还没验证、动手前要先清的事
1. 手写 fbank 与 sherpa 官方输出的 golden 对比（本探针只证明"能出对的字"）。
2. 真实麦克风链路下的端到端延迟与 CPU 占用；`snow-audio-recorder` 的采集事件格式衔接。
3. 首次加载 49.9s 的原因（冷磁盘/杀软/ORT 图优化）；若是图优化，可考虑保存优化后模型或降低优化级别。
4. 键入兼容性矩阵（浏览器/VS Code/Office/记事本/管理员窗口/游戏/远程桌面/中文 IME 组合态）。
5. `SpeechRecognizer` 中文听写在联机开关打开后的真实质量与延迟。
6. small 版 Zipformer 的下载来源与实测。
7. 模型训练数据与权重许可细节（尤其 Paraformer 上游 FunASR 许可、Moonshine 条款）。
