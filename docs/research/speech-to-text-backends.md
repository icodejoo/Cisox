# 实时语音转文字（STT）后端调研

> **后续（2026-10-03）**：本文 §3 的候选模型与 §7 的首期推荐已被实测取代：模型选型、评测方法与结论见 [stt-model-selection.md](stt-model-selection.md)（流式双语默认已定为 x-asr 480ms，另有离线整句维度），worker 与主程序接入现状见 [../guides/snow-stt-worker.md](../guides/snow-stt-worker.md)。下面的正文保持调研当时的原貌，不再修改。

调研日期 2026-10-02。范围：只调研与设计，不含仓库实现。需求（用户已定）：快捷键激活独立进程，实时语音转文字，结束即退出；触发支持「切换式」与「按住说话」；输出实时键入当前焦点输入框；引擎同时支持 Windows 系统语音 API 与本地模型，架构参考翻译/OCR 的 backend 抽象（见 [system-ocr-translate-backends.md](system-ocr-translate-backends.md)）。裁决依据 [principles.md](../principles.md)。

标记约定：【实测】= 本机跑出的数据；【已核实】= 读到一手来源（官方文档/源码/发布包）；【二手】= 非官方来源；【未验证】= 推断或没有证据，不能当事实。仅验证了 Windows，其他平台不在范围（红线）。

## 0. 结论摘要

| 问题 | 结论 |
|---|---|
| sherpa-onnx 能与现有 ort 共用一份 onnxruntime.dll 吗 | **版本上能对上**：sherpa 1.13.8 发布包自带 ORT 1.28.2【已核实】，本仓库 ort rc.13 开了 `api-28`（对应 ORT 1.28）【已核实】。sherpa 的 C API DLL 按名字导入 `onnxruntime.dll`【实测】，用仓库内的 ORT 1.28.0 DLL 替换后 sherpa 能加载并报版本【实测】。「替换后真的建 recognizer 跑识别」后来已测通，见 §8【实测】。 |
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
- 实测：新建目录只放 `sherpa-onnx-c-api.dll` + 仓库里的 ORT 1.28.0 `onnxruntime.dll`，Python ctypes 加载 c-api 成功，`SherpaOnnxGetVersionStr()` 返回 `1.13.8`，进程里的 `onnxruntime.dll` 模块路径确为替换的那份【实测】。（订正：后续已用替换 DLL 实际创建 recognizer 并识别，成功，见 §8。）
- 注意「共用」的含义：STT 是独立 worker 进程，一个进程里只会走 sherpa 或 ort 其中之一。共用的实际收益是**安装包里只放一份 onnxruntime.dll**（翻译、OCR、STT 同一份），不是进程内省内存。

### 1.3 Rust 绑定与许可证
- crate `sherpa-onnx` 1.13.8（2026-09-11），包装 C API；依赖 `serde`、`serde_json`、`sherpa-onnx-sys =1.13.8`【已核实：[docs.rs](https://docs.rs/crate/sherpa-onnx/latest)】。特性 `static`（默认，首次构建自动下载原生库）与 `shared`；可用 `SHERPA_ONNX_LIB_DIR` 指向自带库【已核实：[rust-api-examples README](https://github.com/k2-fsa/sherpa-onnx/tree/master/rust-api-examples)】。另有社区包装 `sherpa-rs`、`sherpa-transducers`、`wavekat-asr`【二手：[搜索结果](https://lib.rs/crates/sherpa-transducers)】。
- 许可证：仓库 Apache-2.0【已核实：仓库页脚】；Rust crate 自身许可证已核实为 Apache-2.0，见 §8。
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
5. **P5（按需）sherpa 后端**：仅当要支持 Paraformer/Whisper/SenseVoice 等多家族时再引入，走 shared 模式、与 ort 共用同一份 `onnxruntime.dll`；"替换 DLL 后真实识别"已在 §8 补做通过。

### 7.4 还没验证、动手前要先清的事
1. 手写 fbank 与 sherpa 官方输出的 golden 对比（本探针只证明"能出对的字"）。
2. 真实麦克风链路下的端到端延迟与 CPU 占用；`snow-audio-recorder` 的采集事件格式衔接。
3. 首次加载 49.9s 的原因（冷磁盘/杀软/ORT 图优化）；若是图优化，可考虑保存优化后模型或降低优化级别。
4. 键入兼容性矩阵（浏览器/VS Code/Office/记事本/管理员窗口/游戏/远程桌面/中文 IME 组合态）。
5. `SpeechRecognizer` 中文听写在联机开关打开后的真实质量与延迟。
6. small 版 Zipformer 的下载来源与实测。
7. 模型训练数据与权重许可细节（尤其 Paraformer 上游 FunASR 许可、Moonshine 条款）。

## 8. sherpa-onnx 实测（2026-10-02 追加）

一次性探针在系统临时目录（不在仓库），`sherpa-onnx = 1.13.8`，`cargo build --release -j 2`，进程 BelowNormal。模型同 §2.3（Zipformer 中英双语 int8），音频为 3 段样例各接 2s 静音（共 25.8s，81 块×320ms）。机器同 §2.3（i5-13500，Windows 10）。

### 8.1 许可证与依赖【已核实：crate 清单 + `cargo metadata`】
- `sherpa-onnx`、`sherpa-onnx-sys` 均为 **Apache-2.0**（Cargo.toml `license` 字段 + crate 内 LICENSE 文本）。Apache-2.0 可并入 GPL-3.0-only 工程（单向兼容），本身无冲突。
- Rust 传递依赖约 100 个包，许可证全是 MIT / Apache-2.0 / BSD / ISC / Zlib / Unicode-3.0 / 0BSD / CC0 / Unlicense / CDLA-Permissive-2.0（webpki-roots 数据）/ `ring` 的 Apache-2.0 AND ISC，**没有与 GPL-3.0-only 不兼容的**。运行时依赖只有 `serde`、`serde_json`（及其小依赖）；`ureq`、`rustls`、`ring`、`zip`、`tar`、`bzip2`、`xz2`、`zstd` 等全是 `sherpa-onnx-sys` 的 **build-dependencies**（构建期下载解压预编译库用），不进最终二进制，但拉长首次编译（shared 约 1m58s，static 约 2m31s，`-j 2`）。
- 预编译原生库（build.rs 下载，不属于 crate）：ORT（MIT）、kaldi-native-fbank / kaldi-decoder / kaldifst / openfst（Apache-2.0）、piper_phonemize、kissfft、ssentencepiece 等；另含 **espeak-ng**（TTS 用，`sherpa-onnx-c-api.dll` 内能搜到 espeak 字样【实测】）。espeak-ng 上游为 GPL-3.0-or-later【常识，本次没读到该 fork 的一手 LICENSE，未验证】，GPLv3 工程使用本身可行。结论：**可用于 GPL-3.0-only 工程**；分发这些 DLL 时需补第三方许可证声明（`collect-third-party-licenses.ps1` 是否覆盖未核实）。
- crate 的 build.rs 只会下载含 TTS 的 `shared-MT-Release-lib`（c-api 4.6MB）；§1.1 的 `no-tts` 包（2.9MB）需自己用 `SHERPA_ONNX_LIB_DIR` 指向。

### 8.2 构建与产物【实测】
| 项 | shared | static |
|---|---|---|
| 下载包（build.rs 自动联网下，GitHub Releases） | 7.7MB | 117.5MB（解压后 lib 约 1GB，onnxruntime.lib 就占 900MB） |
| 最终产物 | `onnxruntime.dll` 17.8MB + `sherpa-onnx-c-api.dll` 4.6MB + `onnxruntime_providers_shared.dll` 0.1MB = 3 个 DLL 约 22.5MB（`sherpa-onnx-cxx-api.dll` 0.26MB 用不到），exe 0.3MB | 单个 exe 19.1MB，无 DLL |
| 额外工具链 | 不需要 libclang / cmake（绑定手写，build.rs 只下载解压）；MSVC + cargo 即可 | 同 |
| 联网 | **构建期必须联网**（`ureq`，支持代理环境变量）；可用 `SHERPA_ONNX_LIB_DIR`（已解压 lib 目录）或 `SHERPA_ONNX_ARCHIVE_DIR`（本地 tar.bz2）离线构建 | 同 |
| CRT | 预编译包为 MT（静态 CRT），与仓库静态 CRT 约定一致 | 同 |

踩坑：**Windows 长路径**。build.rs 默认把包解压到 `<target>/…/out/sherpa-onnx-prebuilt/<长名字>/lib/`，target 目录路径较深时解压静默失败，随后链接报 `LNK1104 无法打开 sherpa-onnx-c-api.lib`。设 `CARGO_TARGET_DIR` 为短路径即正常（此时解压到 `<CARGO_TARGET_DIR>/sherpa-onnx-prebuilt/`）。仓库 target-dir 是 `build/cargo`，路径较短，但独立 worker 工作区放得深时要留意。

### 8.3 shared 模式替换 ORT【实测】
- 自带 `onnxruntime.dll` 文件版本 1.28.2；替换为仓库 `build/mt-quant/ort128/onnxruntime/capi/onnxruntime.dll`（1.28.0.20260724，17.8MB，同目录 `onnxruntime_providers_shared.dll` 一并替换）。
- 进程内 `GetModuleFileName(onnxruntime.dll)` 确认加载的是各自目录里的那份。
- **替换后真实创建流式 Zipformer recognizer 并识别成功**，4 段 final 文本与自带 ORT 完全一致（`昨天是 MONDAY` / `TODAY IS LIBR THE DAY AFTER TOMORROW是星期三` / `这是第一种第二种叫呃与 ALWAYS ALWAYS什么意思啊` / `这个是频繁的啊不认识记下来 FREQUENTLY频繁的`）；Silero VAD 在替换版上也能建会话并出相同切分。
- 范围限制：只覆盖本模型 + VAD 用到的算子；其他模型家族（Paraformer / Whisper / SenseVoice）、TTS 没用替换 DLL 跑过【未验证】。

### 8.4 性能（2 线程、BelowNormal、320ms 块；每块含 accept + decode + 取结果 + 端点判断）
| 配置 | 模型加载 | 工作集（加载后 / 峰值） | 每块 p50 | 每块 p95 | RTF |
|---|---|---|---|---|---|
| shared + 自带 ORT 1.28.2（正常的 2 次） | 3.0~4.0s（首次冷 11s） | 245~255 / 267MB | 46.6~48.6ms | 67.8~68.4ms | 0.152~0.156 |
| shared + 仓库 ORT 1.28.0（3 次） | 3.4~4.9s | 246~257 / 268~270MB | 43.7~47.2ms | 55.2~68.7ms | 0.137~0.152 |
| static（内置 ORT，1 次） | 3.5s | 244~254 / 267MB | 44.5ms | 61.1ms | 0.142 |
| 自研 ort 探针（§2.3，对照） | 1.4s（热） | ~245 / 253MB | 31~37ms | 40~46ms | 0.105~0.135 |

解读：
- 内存与自研基本持平（峰值多约 14MB）。**替换 ORT 与否无可见差别**，仓库那份略快或持平，差异在噪声内。
- sherpa 每块比自研高约 10ms（RTF 0.14~0.15 对 0.105~0.135）。可能因为本次计时含 JSON 取结果与端点判断、不同时段机器负载不同、图优化设置差异——**没做同时段交叉对比，不能定论**。都远低于 1.0，实时足够。
- 加载 3~5s，比自研探针的 1.4s 慢（默认配置），一次性开销；首次 11s 与 §2.3 的 49.9s 同类（冷文件）。
- **离群值**：shared 自带 ORT 两次运行出现秒级卡顿（最大块 8.8s / 1.7s，RTF 0.83 / 0.30），同一二进制其余 4 次均无。判断为本机其他进程抢占（BelowNormal 易被饿），非 sherpa 缺陷，**但实时场景下要有缓冲与超时保护**。

### 8.5 VAD 与端点【实测】
- **Silero VAD 可用**：`silero_vad.onnx` 0.64MB（GitHub asr-models 发布），建会话 160~190ms；单独进程工作集 6→27MB（含 ORT / c-api DLL 映射），**在已加载 ASR 的进程里再建 VAD 只多约 2MB**（246→248MB）；每 512 样本窗口 p50 0.24ms / p95 0.3~0.44ms，可忽略。
- 样例上切出 5 段（threshold 0.5、min_silence 0.5s）：0.68s/1.61s、3.81s/2.12s、6.53s/3.37s、12.87s/4.26s、19.14s/4.10s，与听感一致，无漏段。
- **ASR 自带端点检测可直接用于「说完一句」**：`enable_endpoint=true`，`rule1_min_trailing_silence=2.4`、`rule2=1.2`、`rule3_min_utterance_length=20`（sherpa 示例默认）；`is_endpoint()` 为真时取该句定稿，`reset(stream)` 开下一句。样例触发 4 次端点，都落在静音处，文本无丢失。第一段样例自带 >1.2s 停顿，被切成 `昨天是 MONDAY` 与 `TODAY IS…` 两句（规则 2 的正常行为，调大 `rule2` 可减少过切）。切换式「静音自动结束」不接 VAD 即可做；VAD 的价值是与模型无关、更快（0.5s 级）的静音判断，以及先滤静音再喂 ASR 省算力，属选项而非必需。
- 阈值怎么设才贴合真实说话节奏（比如要「说完 0.8s 出定稿」就把 rule2 调到约 0.8）没在麦克风下评估【未验证】。

### 8.6 流式结果接口与进程内骨架【已核实 API 源码 + 实测可跑】
- **没有回调，是拉取式**：`accept_waveform(sr, &[f32])` 推音频 → `while is_ready { decode }` → `get_result(&stream)` 取 `RecognizerResult { text, tokens, timestamps, segment, start_time, is_final }`（内部 JSON 往返，每块一次，开销已计入 8.4）。partial = 每块 `get_result` 的当前文本（本模型只追加，实测 35 次变化均为增长）；final = `is_endpoint()` 为真那一次的文本，或 `input_finished()` 后冲刷的尾部。
- 对象标了 `Send + Sync`，但 C 库按「单对象单线程」用更稳：采集线程 → channel → 推理线程 → 写管道线程。
- 适合独立进程经管道回传：每块（0.32s）最多一条纯文本消息，带宽极低，可复用 `snow-ocr-protocol` 命名管道帧格式。

```rust
// 最小骨架：worker 内循环，结果经管道发回主进程
let rec = OnlineRecognizer::create(&cfg).ok_or("create recognizer")?; // num_threads=2, enable_endpoint=true
let stream = rec.create_stream();
let mut last = String::new();
for pcm in audio_rx {                        // 16k 单声道 f32，约 320ms 一块；收到 Stop 就 break
    stream.accept_waveform(16000, &pcm);
    while rec.is_ready(&stream) { rec.decode(&stream); }
    let r = rec.get_result(&stream).unwrap();
    if r.text != last { pipe.send(SttEvent::Partial(r.text.clone())); last = r.text; }
    if rec.is_endpoint(&stream) {
        if !last.is_empty() { pipe.send(SttEvent::Final(last.clone())); }
        rec.reset(&stream); last.clear();
    }
}
stream.input_finished();                     // 冲刷尾部
while rec.is_ready(&stream) { rec.decode(&stream); }
if let Some(r) = rec.get_result(&stream) { if !r.text.is_empty() { pipe.send(SttEvent::Final(r.text)); } }
```

### 8.7 结论与取舍
- **推荐 shared**：下载与解压小（7.7MB 包 对 117.5MB / 1GB），ORT 可与翻译 / OCR 共用同一份并由仓库自管版本（已验证可替换）；代价是随 worker 多分发 2~3 个 DLL，且要保证同目录优先加载，别被 PATH 里别的 `onnxruntime.dll` 抢先。static 的好处是单 exe，但内置 ORT 无法共用、构建缓存巨大。
- 相对自研：VAD、端点、多模型家族、已验证的 fbank 全包，内存持平，每块多约 10ms；代价是多一个预编译二进制依赖（构建期联网）、第三方声明、DLL 分发。
- 订正 §7.3：P1 可直接用 sherpa（shared）替代自研 fbank / 解码，golden 对比一项因此可省。是否改动 §7.1 的首期推荐由用户定。

### 8.8 未验证 / 遗留风险
1. 真实麦克风链路（WASAPI → 重采样 → sherpa）端到端延迟与 CPU 占用；本次是文件按 320ms 块喂入。
2. 端点阈值在真实说话节奏下的过切 / 漏切；仍无标点。
3. 其他模型家族、small 版 Zipformer 未用 sherpa 跑。
4. 离群卡顿根因只是推断，未单独复现；与自研 10ms/块的差距未做同时段交叉对比。
5. 替换 ORT 仅验证了本机这份 1.28.0 开发版；ORT 升级（如仓库 venv 里的 1.30）后与 sherpa 1.13.8 是否兼容需重测。
6. espeak-ng 等随包库的确切许可证文本与第三方声明脚本覆盖情况未核实。
7. 构建期联网与长路径问题（见 §8.2）；CI / 离线环境需预置 `SHERPA_ONNX_ARCHIVE_DIR`。
