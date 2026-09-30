# 系统原生 OCR / 翻译 与「三选一可选后端」方案

调研日期 2026-10-01。范围：只调研与设计，不含实现。标记约定：【已核实】= 读到一手来源；【实测】= 本机 Windows 11 实测；【二手】= 非官方来源；【未验证】= 没有证据，不能当事实。macOS 各项**未在 macOS 实机验证**。

## 0. 结论摘要

| 问题 | 结论 |
|---|---|
| Windows 有系统 OCR 吗 | 有。`Windows.Media.Ocr.OcrEngine`，Win10 10240+ 全版本可用，无需 Copilot+，纯 CPU。有行/词/外接矩形，**无置信度**。实测 1707x1067 整屏约 310ms。 |
| Windows 新 AI OCR（App SDK `TextRecognizer`） | **仅 NPU（Copilot+ PC）**，GPU/CPU 官方标"不支持"。有置信度和多边形框。只能当可选加速档，不能当默认。 |
| Windows 有系统翻译 API 吗 | **没有**。Windows AI APIs 清单里无翻译，"Live Translation"标注 Not yet supported（仅字幕场景规划）。替代：Phi Silica（NPU/部分 GPU，且正被 Aion Instruct 替换，中国不可用）做 LLM 翻译，不稳定，不建议依赖。 |
| macOS 有系统 OCR 吗 | 有。Vision `VNRecognizeTextRequest`，macOS 10.15+，fast/accurate 两档，有置信度与框。Rust 用 `objc2-vision` 直调，无需 Swift 桥。 |
| macOS 有系统翻译 API 吗 | 有，Translation 框架，macOS 15+。**但**：macOS 15 只能经 SwiftUI `.translationTask` 拿 session（需要视图宿主）；macOS 26 起有 `init(installedSource:target:)`，可无界面调用，**但仅限语言包已安装**，下载授权必须走视图宿主。 |
| Linux | 无系统自带 OCR/翻译【未验证，但常识上发行版不预装；不做系统后端】。 |
| 推荐 | 三选一后端，**OCR：Windows 默认 system（WinRT），macOS 默认 system（Vision），本地模型（现有 PP-OCR）作为质量档/回落**；**翻译：只有 macOS 26+ 有可用的 system 选项，Windows 无 system，默认仍为本地模型，远程 API 为可选**。 |

## 1. 现有实现（代码事实）

### 1.1 OCR

- 独立进程 `snow-crates/crates/snow-ocr-process`（Apache-2.0），二进制协议 v4（`PROTOCOL.md`）：共享内存传 RGBA（上限 3840x2160）、Submit/Recognize/Cancel、`PrepareSession`（DirectML 开关、检测缩放策略、检测/识别/字典路径）。结果 `Complete`：每行文本 + `confidence f32` + 四点坐标。
- 推理：`rapid-ocr-rs`（本仓库 path 依赖）+ `ort 2.0.0-rc.13`（fork），特性 `dynamic-onnx-runtime`/`static-onnx-runtime`/`directml-provider`。
- 宿主侧 `snow-shot-rs/crates/snow-shot/src/ocr_service.rs`：`OcrService::recognize_rgba`，`OcrLauncher` trait（便于注入假 worker），产出 `OcrResult{boxes: Vec<OcrTextBox{rect,text,confidence}>, full_text, elapsed_ms}`。`ocr_assets.rs` 管资产下载/校验，`ocr_download.rs` 下载，`ocr_flow.rs` 管覆盖窗 UI 状态（Idle/Running/Done/Failed{can_download}/Downloading）。`ocr_client.rs`、`snow-ocr-protocol` 是客户端协议。
- 配置键（`ocr_service.rs`）：`text_recognition/model_type`(默认 small)、`text_recognition/direct_ml_acceleration`、`text_recognition/detector_resize_policy`、`text_recognition/resident_process`。没有"后端"概念。
- 资产体积（`resources/ocr-asset-manifest.json`）：运行时 zip 17.3MB（exe 19.5MB + DirectML.dll 18.5MB）；模型 det+rec：extra_small ≈6.3MB、small(PP-OCRv6) ≈31MB、medium(v6) ≈139MB、small_v5 ≈21MB、medium_v5 ≈173MB、small_v4 ≈15.6MB、medium_v4 ≈204MB。下载源 modelscope.cn。

### 1.2 翻译

- `snow-shot-rs/crates/snow-translate`（库）+ `tools/snow-translator`（独立 workspace 的 worker，GPL-3.0，`ort` + `tokenizers` + ndarray，自写 beam search）。ADR 调研见 `docs/research/adr5-local-nmt.md`（其中 CTranslate2 方案已不是现状，现状是 ort 自写解码）。
- 抽象：`trait TranslationEngine { translate(text,src,tgt); translate_batch(..) }`；实现 `WorkerEngine`（本地 NMT，按需拉起、空闲卸载、崩溃复位；`Transport`/`WorkerLauncher` trait 可 mock）与 `OpenAiEngine`（OpenAI 兼容聊天补全，经 curl 发请求，`HttpPost` trait 可 mock）。`TranslationService` 带 256 条结果缓存。
- 宿主侧 `translate_service.rs`：`Backend{Local, OpenAi}`，`TranslateConfig::from_document`，`trait Translator`，`TranslateHost`（扫描模型、装配引擎、`run_flow`）。
- 配置键（`snow-config/src/extensions.rs`）：`screenshot_translation/backend`(`local`|`openai`，默认 local)、`local_models_dir`、`local_model_id`、`local_idle_unload_seconds`、`local_num_beams`、`local_low_memory`，另有 `target_language`/`source_language`/`layout`/自定义 AI 模型列表（`custom_models.rs`，含 base_url）。
- 本地模型：用户自备 ONNX（`<数据根>/models/translate/<id>/model.json`），非内置，故"首次使用成本"是用户自己找模型。

## 2. 系统原生能力调研

### 2.1 Windows OCR：`Windows.Media.Ocr`

来源：[OcrEngine](https://learn.microsoft.com/en-us/uwp/api/windows.media.ocr.ocrengine)、[OcrWord](https://learn.microsoft.com/en-us/uwp/api/windows.media.ocr.ocrword)。

- 版本：Windows 10（10.0.10240.0）起，UniversalApiContract 1.0【已核实】。
- API：`TryCreateFromUserProfileLanguages()`、`TryCreateFromLanguage(Language)`、`AvailableRecognizerLanguages`、`IsLanguageSupported`、`MaxImageDimension`、`RecognizeAsync(SoftwareBitmap)`【已核实】。结果 `OcrResult → Lines → Words`，词有 `Text` 与 `BoundingRect`（TextAngle 为 0 时的像素矩形）；**文档未列置信度属性**【已核实，属性表无 confidence】。
- 语言：只能用"已装 OCR 语言包"的语言，引擎按用户配置语言创建；一次识别一种语言（多语言混排需分别建引擎、自行合并，**未验证**合并质量）。装语言包：系统"语言和区域"里添加语言（含 OCR 可选功能，`Language.OCR~~~xx-XX` 能力，需管理员，本机 `Get-WindowsCapability` 因未提权无法列出，改用 `AvailableRecognizerLanguages` 取得）【语言包安装细节为常识，未读到对应一手页，**未验证**】。
- Rust：`windows` crate 特性 `Media_Ocr`（及 `Graphics_Imaging`、`Storage_Streams`、`Foundation`），docs.rs 页面未明示特性名，按该 crate 命名规则推断【未验证，需 `cargo check` 证实】。`OcrEngine` 标 `Send+Sync`。
- 【实测】本机 Windows 11 Pro 10.0.26100，63.7GB 内存，PowerShell 5.1 调 WinRT，整屏截图 1707x1067（中英混排开发文档，47 行）：
  - 已装 OCR 语言：`en-US`、`zh-Hans-CN`；`MaxImageDimension = 10000`；
  - 引擎取用户配置语言 `zh-Hans-CN`；4 次识别耗时 330/312/307/308 ms（首次无明显冷启动惩罚，引擎创建不计入）；
  - 进程工作集 110~122MB、私有内存约 90MB（**含 PowerShell 宿主本身**，不能当 OCR 增量，仅作上界参考）；
  - 质量观察：中文逐字带空格分隔输出（词=字，需在拼接时按 CJK 规则去空格）；中英混排时用 zh 引擎识别英文/符号段明显劣于英文引擎（出现 `nteI/AMD`、`InteI`、`DEFAULT-HARDWARE-MODE` 里夹乱码字符等）；小字号行的框高度失真（如 4x16、10x2）。
  - 脚本在会话 scratchpad，未入库。此为单机单图测试，**没有与 PP-OCR 做同图对比**，质量结论只是定性观察。

### 2.2 Windows 新 AI OCR：App SDK `TextRecognizer`

来源：[Text Recognition (OCR)](https://learn.microsoft.com/en-us/windows/ai/apis/text-recognition)、[Windows AI APIs 总览](https://learn.microsoft.com/en-us/windows/ai/apis/)（页面更新 2026-07/08）。

- 命名空间 `Microsoft.Windows.AI.Imaging.TextRecognizer`；输出词/行/多边形边界与 `MatchConfidence`【已核实】。
- 官方原文："run exclusively on devices with an NPU"；硬件表 Text Recognition 仅 NPU(Copilot+) 可用，GPU、CPU 均"Not supported"【已核实】。需 Windows App SDK（文档称 1.7.1 起的 "All other APIs"），打包/部署形态（unpackaged、自包含）**未核实**。
- 判断：覆盖面太窄（仅 Copilot+），且引入 WinAppSDK 依赖与 WinRT 激活复杂度，违背"少编译依赖"。**不进第一期**，列为后续可选加速档。

### 2.3 Windows 翻译：没有系统原生可编程翻译 API

- Windows AI APIs 清单（Phi Silica、Text Recognition、Speech Recognition、Imaging、Video/Image SR、Image Generation）**没有翻译**；"Live Translation"被明确列在 Planned / Not yet supported【已核实，上述总览页】。
- 替代：Phi Silica（[文档](https://learn.microsoft.com/en-us/windows/ai/apis/phi-silica)）是 LLM，可提示词翻译，但：NPU 或 NVIDIA RTX 30+/AMD RX 9060+（6GB+ 显存，且需开发者模式），CPU 不支持；中国不可用；被 Aion Instruct 替换（Insider 2026-10、零售 2026-11）；是 Limited Access Feature【均已核实于总览页】。结论：不可作为稳定后端。
- 结论：Windows 上"系统翻译"选项**不提供**；翻译只在 本地模型 / 远程 API 间选。

### 2.4 macOS OCR：Vision

来源：[VNRecognizeTextRequest](https://developer.apple.com/documentation/vision/vnrecognizetextrequest)（经 developer.apple.com 的 JSON 数据接口读取）、[VNRequestTextRecognitionLevel](https://developer.apple.com/documentation/vision/vnrequesttextrecognitionlevel)、[VNRecognizedText](https://developer.apple.com/documentation/vision/vnrecognizedtext)。

- macOS 10.15+【已核实】。`recognitionLevel`：`.fast` / `.accurate`（accurate 更慢更全）；`recognitionLanguages`（优先级数组）、`automaticallyDetectsLanguage`、`usesLanguageCorrection`、`customWords`、`minimumTextHeight`、`supportedRecognitionLanguages()`；当前 revision 3【已核实】。
- 输出：`VNRecognizedTextObservation.topCandidates(n) → VNRecognizedText{string, confidence, boundingBox(for:range)}`（归一化坐标，原点在左下，需换算，此点为常识【未在本次读到】）。
- 具体支持语言列表、各版本间语言差异：**未验证**（需在目标 macOS 上调 `supportedRecognitionLanguages()`）。
- Rust：[`objc2-vision`](https://docs.rs/objc2-vision/latest/objc2_vision/struct.VNRecognizeTextRequest.html) 0.3.2（2026-08-04），Zlib/Apache-2.0/MIT 三许可，需启用 `VNRecognizeTextRequest`、`VNRequest` 特性，依赖 objc2 系（Foundation、CoreGraphics、CoreImage 等可选）【已核实】。无需 Swift 桥、无需额外运行库；系统框架随 OS 提供。仓库已有 `snow-macos` crate，可放入口。
- 无界面、无权限弹窗（对内存图像识别）【常识，未核实】。

### 2.5 macOS 翻译：Translation 框架

来源：[TranslationSession](https://developer.apple.com/documentation/translation/translationsession)、[init(installedSource:target:)](https://developer.apple.com/documentation/translation/translationsession/init(installedsource:target:))、[LanguageAvailability](https://developer.apple.com/documentation/translation/languageavailability)、第三方实测 [gancho PR #133](https://github.com/johnny4young/gancho/pull/133)【二手】。

- 可用性：`TranslationSession`/`LanguageAvailability` macOS 15.0+【已核实】。全部在设备端处理；Apple 可能收集 bundle ID、语言对等使用指标但不含内容【已核实】。
- 拿 session 的方式：macOS 15 只有 SwiftUI `.translationTask(...)` 提供 session（需要视图宿主，**本次未找到一手原文逐字确认 15.x 无其它入口**，来自 Apple 文档对 26 新 init 的说明间接成立）。
- macOS 26.0+ 新增 `init(installedSource:target:)`：语言"已安装"时可直接创建；未安装则 translate 抛错；要让用户授权下载语言必须用 `.translationTask` 提供的 session【已核实】。第三方称在无 bundle、无视图的 CLI 进程里实际翻译成功，且无法从 package service 弹下载面板【二手，需自测】。
- `prepareTranslation()`：向用户请求下载权限（走视图宿主）；`LanguageAvailability.status(from:to:)` 返回 installed/supported/unsupported 用于预检【已核实】。
- `translate(_:)`、`translate(batch:)`、`translations(from:)` 支持批量【已核实】。
- 离线：语言包已安装后离线可用（设备端处理）。具体支持语言列表（`supportedLanguages`）**未验证**。
- Rust 调用：Translation 是 Swift-only 框架（无 ObjC 头，`objc2-translation` 是否存在**未验证**），需要一个小型 Swift 静态库/dylib + C ABI 桥（Swift 异步 API → 回调或阻塞）。这是 macOS 上唯一需要引入 Swift 工具链的点；下载授权 UX 需在 GPUI 之外挂一个最小 SwiftUI 宿主窗口（**未验证**在 GPUI 进程内嵌 SwiftUI 的可行性）。
- 门槛小结：macOS 26+ 才能无界面调用（且预装语言包）；macOS 15.x 需要 SwiftUI 宿主；<15 没有。

### 2.6 Linux

无系统自带 OCR/翻译可依赖【未验证，不作为系统后端】。Linux 仅提供 本地模型 / 远程 API。

### 2.7 其它（只复述现有代码）

- 本地模型：OCR = PP-OCR ONNX（`snow-ocr-process` + `rapid-ocr-rs` + ort/DirectML）；翻译 = 用户自备 ONNX NMT（`snow-translator`）。
- 远程 API：翻译 = `OpenAiEngine`（OpenAI 兼容端点 + 自定义模型列表）。OCR 目前**没有**远程实现。

## 3. 后端对比

| 维度 | OCR：Win WinRT | OCR：Win AI(NPU) | OCR：macOS Vision | OCR：本地 PP-OCR | OCR：远程 API | 翻译：macOS Translation | 翻译：本地 NMT | 翻译：远程 API |
|---|---|---|---|---|---|---|---|---|
| 质量 | 中；CJK 逐字带空格，中英混排需多引擎【实测】 | 官方称更快更准【已核实声明，未实测】 | 官方分 fast/accurate，质量未测 | 高（专用检测+识别），量级由模型档决定，未同图实测 | 取决于服务，通常最高 | 语种有限，质量未测 | 取决于用户模型 | 高 |
| 速度 | ~310ms / 1707x1067【实测】 | 未测 | 未测 | 未测（有 DirectML 选项） | 网络 RTT + 上传 | 首次需建 session，暖路径快【二手】 | 未测 | 网络 RTT |
| 内存 | 进程内增量小，整进程上界 ~90MB 私有含宿主【实测】 | 未测 | 未测，系统框架 | ORT + 模型，small ≈31MB 模型文件，常驻进程内存未测 | 无 | 系统服务承担 | 独立进程，闲置退出 | 无 |
| 启动延迟 | 低 | 需 EnsureReady | 低 | 进程拉起 + 模型加载 | 无 | 取决于语言包 | 进程拉起 + 加载 | 无 |
| 离线 | 是 | 是 | 是 | 是 | 否 | 语言包装好后是 | 是 | 否 |
| 隐私 | 本机 | 本机 | 本机 | 本机 | **上传截图/文字** | 本机 | 本机 | **上传文字** |
| 首次使用成本 | 需装对应 OCR 语言包 | 需 Copilot+ | 无 | 下载 6~200MB + 17MB 运行时 | 填密钥 | 下载语言包（需视图授权） | 用户自备模型 | 填密钥 |
| 新增编译依赖 | `windows` crate 特性（已在依赖树的概率高，**待查**） | WinAppSDK（重） | `objc2-vision`（轻） | 已有 | HTTP 客户端 | Swift 桥 | 已有 | 已有（curl） |
| 系统门槛 | Win10+ 且装语言包 | Copilot+ NPU | macOS 10.15+ | 无 | 无 | macOS 15+（无界面需 26+） | 无 | 无 |
| 置信度 | 无 | 有 | 有 | 有 | 视服务 | 无 | 无 | 无 |

按用户优先级（高性能>低内存>高 fps>少依赖>多用系统能力）：OCR 用系统后端最贴合（几乎零依赖，不加载 ORT，不占 GPU，不抢录屏 fps）；翻译因 Windows 无系统能力，只能靠本地/远程。

## 4. 方案设计

### 4.1 trait 抽象

OCR 抽象在宿主侧（不动 worker 协议）：

```rust
/// 统一 OCR 输入：RGBA 像素 + 语言提示。
pub struct OcrInput<'a> { pub rgba: &'a [u8], pub width: u32, pub height: u32, pub langs: &'a [String] }

/// OCR 后端能力，供 UI 决定是否可选及提示。
pub enum OcrAvailability { Ready, NeedsSetup(SetupHint), Unsupported(Reason) }

pub trait OcrEngine: Send + Sync {
    fn id(&self) -> OcrBackendId;                       // system | local-model | remote-api
    fn availability(&self, langs: &[String]) -> OcrAvailability;
    fn recognize(&self, input: OcrInput, cancel: &CancelToken) -> Result<OcrResult, OcrError>;
}
```

- `OcrResult` 沿用现有 `OcrResult/OcrTextBox`；`confidence` 改为 `Option<f32>`（WinRT 无置信度）。
- 实现：`LocalModelOcr`（包装现有 `OcrService`，零改动逻辑）、`WindowsSystemOcr`（`#[cfg(windows)]`，`windows` crate，放独立线程/进程以免阻塞 UI，倾向独立小线程而非新进程）、`MacVisionOcr`（`#[cfg(target_os="macos")]`，`objc2-vision`）、`RemoteApiOcr`（可选，第二期）。
- 翻译抽象**复用现有** `TranslationEngine`，只补能力探测：给它加 `fn availability(&self, src, tgt) -> Availability`（默认 `Ready`）。新增 `MacTranslationEngine`（Swift 桥，`cfg(macos)`）。`Backend` 枚举由 `{Local, OpenAi}` 扩为 `{System, Local, Remote}`。

### 4.2 配置项（每项功能独立选后端）

| 键 | 值 | 默认 |
|---|---|---|
| `text_recognition/backend` | `system` \| `local-model` \| `remote-api` | Win/macOS：`system`；其它：`local-model` |
| `text_recognition/system_languages` | BCP-47 数组，空=跟随系统 | 空 |
| `text_recognition/remote_*` | 复用自定义 AI 模型条目引用 | 空 |
| `screenshot_translation/backend` | `system` \| `local-model` \| `remote-api` | macOS 26+ 且语言对已装：`system`；其余：`local-model` |

迁移：`screenshot_translation/backend` 旧值 `local`→`local-model`、`openai`→`remote-api`，在 normalize 里读时映射，保留未知字段（文档层本就"保留未知字段"）；`text_recognition/*` 原有键全部保留（`model_type` 等只在 `local-model` 时生效）。**已有用户的 OCR 行为变化**：若默认改 `system`，老用户会静默换引擎，建议迁移时对"已下载过 OCR 资产"的用户写入 `local-model`，仅新用户默认 `system`。

### 4.3 回落顺序与提示

- 选了 `system` 但不可用（缺语言包 / 系统过旧 / 调用失败）：**不静默回落**，先在结果面板给出可操作提示（"Windows 未安装 OCR 语言包：打开语言设置"）；并提供一键"改用本地模型"。仅当用户勾选"自动回落"时按 `system → local-model → （不自动走 remote）` 回落。
- **永不自动回落到 remote-api**：会外传数据，必须用户显式选择。
- 翻译同理：system 不可用 → 提示并可切 local-model。

### 4.4 语言包管理 UX

- Windows OCR：设置页显示"已安装 OCR 语言：en-US, zh-Hans-CN"（来自 `AvailableRecognizerLanguages`）；缺失时给"打开 Windows 语言设置"按钮，不在应用内装（需管理员，体验差）。
- macOS Vision：无语言包；设置页可显示 `supportedRecognitionLanguages`。
- macOS Translation：状态用 `LanguageAvailability.status`；`supported` 但未装 → 引导"前往系统设置 › 通用 › 语言与地区 › 翻译语言"（路径为常识，**未验证**）或拉起 SwiftUI 宿主触发 `prepareTranslation`（待 spike）。
- 本地模型：沿用现有下载流程。

### 4.5 隐私与 i18n

- 选 `remote-api` 时，设置页与首次使用弹窗用英文写明"screenshots / recognized text will be sent to <host>"，首次需确认；密钥**存放在配置文件中**（已决定，2026-10-01，沿用 `custom_models` 现状，不引入系统钥匙串）；设置页需提示"密钥以明文保存在本地配置文件"，且配置导出/日志不得带出密钥。
- 所有用户可见文案走 `snow-i18n`（Fluent `.ftl` + `t!` 宏），在 `crates/snow-i18n/locales/{en-US,zh-CN,zh-TW}` 三个目录补齐同名 id；产品名不进文案，以变量 `product` 注入。本项目是纯 Rust+GPUI，没有 Qt，`AGENTS.md` 里的 Qt `tr()`/`.ts` 流程对本分支不适用。后端 ID、配置键用字面量，不翻译。

### 4.6 测试策略

- 抽象层全部可离屏：为 `OcrEngine`/`TranslationEngine` 写假实现，覆盖：后端选择、不可用提示、回落顺序、永不自动走 remote、配置迁移（旧→新值）、`confidence: None` 的 UI 渲染。
- 系统 API 适配层：把"WinRT 调用"收敛到一个薄 `trait WinOcrApi`（创建引擎、取语言、识别），单测用 mock；真机测试标 `windows` 标签、默认不跑（与仓库现有 CTest 标签一致）。
- 后处理（CJK 去空格、行框合并、macOS 归一化坐标→像素坐标）写成纯函数 + 确定性表驱动测试。
- 只跑相关测试，不跑全量。

### 4.7 工作量与风险

| 阶段 | 内容 | 估算 | 主要风险 |
|---|---|---|---|
| P0 抽象与配置 | `OcrEngine` trait、`backend` 键、迁移、UI 选择器与提示 | 3~4 人天 | 默认值变更影响老用户（已给迁移策略） |
| P1 Windows 系统 OCR | `windows` crate 接入、后处理、语言状态、测试 | 3~5 人天 | 中英混排质量（需多语言引擎策略）；无置信度；须与 PP-OCR 同图对比后再定默认 |
| P2 macOS Vision OCR | `objc2-vision`、坐标换算、fast/accurate 选项 | 3~5 人天 | 无 macOS 实机（本次未验证）；objc2 特性裁剪 |
| P3 macOS Translation | Swift 桥 + 预检 + 引导；26+ 无界面路径 | 5~8 人天 | 15.x 需要视图宿主；Swift 工具链进 CI；26+ 行为仅二手验证 |
| P4 远程 OCR（可选） | 复用 OpenAI 兼容视觉端点 | 2~3 人天 | 隐私、费用、延迟 |
| P5 Win AI OCR（可选） | WinAppSDK `TextRecognizer` | 5+ 人天 | 仅 Copilot+，依赖重，收益窄 |

建议顺序：P0 → P1（收益最大：Windows 是主平台，且立刻省掉 ORT/DirectML 常驻）→ P2 → P3；P4、P5 视需求。

## 5. 开放问题

1. Windows 上 `system` 引擎的质量是否可接受？需用同一批截图与 PP-OCR small 做准确率/耗时/内存对比（本次只做了单图 WinRT 实测）。
2. 中英混排策略：按系统语言建多引擎并取并集，还是按脚本检测选引擎？
3. `windows` crate 是否已在依赖树中，`Media_Ocr` 特性名需 `cargo check` 确认。
4. macOS：`supportedRecognitionLanguages` 实际列表、Vision 与 PP-OCR 的对比、objc2-vision 在 GPUI 进程内的线程要求，均需 macOS 实机验证。
5. macOS Translation：15.x 下从 GPUI 进程拉起 SwiftUI 宿主授权下载是否可行；26+ `init(installedSource:target:)` 的无界面行为需自测（目前仅二手证据）。

已决定（2026-10-01）：
6. 迁移默认值：老用户（已下载过 OCR 资产）保持 `local-model`，新用户默认 `system`。
7. API 密钥存放在配置文件中。
8. i18n 以 `snow-i18n`（Fluent）为准，本项目无 Qt。

## 6. 来源

- [OcrEngine（Windows.Media.Ocr）](https://learn.microsoft.com/en-us/uwp/api/windows.media.ocr.ocrengine)
- [OcrWord](https://learn.microsoft.com/en-us/uwp/api/windows.media.ocr.ocrword)
- [Windows App SDK 文本识别 (OCR)](https://learn.microsoft.com/en-us/windows/ai/apis/text-recognition)
- [Windows AI APIs 总览与硬件支持](https://learn.microsoft.com/en-us/windows/ai/apis/)
- [Phi Silica](https://learn.microsoft.com/en-us/windows/ai/apis/phi-silica)
- [VNRecognizeTextRequest](https://developer.apple.com/documentation/vision/vnrecognizetextrequest)
- [VNRecognizedText](https://developer.apple.com/documentation/vision/vnrecognizedtext)
- [TranslationSession](https://developer.apple.com/documentation/translation/translationsession)
- [LanguageAvailability](https://developer.apple.com/documentation/translation/languageavailability)
- [objc2-vision 0.3.2](https://docs.rs/objc2-vision/latest/objc2_vision/struct.VNRecognizeTextRequest.html)
- 二手：[gancho PR #133（26 上无界面 session 实测）](https://github.com/johnny4young/gancho/pull/133)
- 仓库内：`docs/research/adr5-local-nmt.md`、`snow-crates/crates/snow-ocr-process/PROTOCOL.md`、`snow-shot-rs/crates/snow-shot/src/{ocr_service,translate_service,ocr_assets}.rs`、`snow-shot-rs/crates/snow-translate/src/lib.rs`、`snow-shot-rs/crates/snow-config/src/extensions.rs`
