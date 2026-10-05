# snow-stt 语音转文字工作进程

> 状态：worker 与协议已接入主程序，含系统语音后端、离线整句模式、模型清单与按需下载、设置页、语音翻译级联（见「主程序接入」「离线模式」「模型管理与设置」「语音翻译级联」各节）。真机麦克风实测与 UI 截图验证尚未做，见「已知限制与未验证」。背景见 [research/speech-to-text-backends.md](../research/speech-to-text-backends.md)（尤其 §8），模型选型与评测见 [research/stt-model-selection.md](../research/stt-model-selection.md)，原则见 [principles.md](../principles.md)。

## 组成
- `snow-shot-rs/crates/snow-stt-protocol`：主 workspace 成员，零依赖的行文本协议，主程序与 worker 共用。
- `snow-shot-rs/tools/snow-stt`：独立 workspace（自己的 `[workspace]` 与 `Cargo.lock`），二进制 `snow-stt`。依赖 sherpa-onnx 1.13.8（shared 链接，用户已批准新增）、协议 crate、`snow-crates/snow-audio-recorder`（WASAPI 默认麦克风采集与 16k 单声道重采样，不自写）。
- 识别后端是 `SttBackend` trait（`feed` / `poll` / `finish`），有两份实现：sherpa（本地模型）与 Windows 系统语音（见「系统语音后端」）；主循环 `session.rs` 只认 trait 和 `AudioSource`，单测用 Fake 后端与脚本化来源，不依赖麦克风和模型。

## 协议
每条消息一行 UTF-8，字段以空格分隔，自由文本放行尾，其中 `\`、换行、回车、制表符转义为 `\\`、`\n`、`\r`、`\t`（Windows 路径里的反斜杠也要双写，用 `Command::to_line` 生成即可）。

| 方向 | 消息 | 说明 |
|---|---|---|
| 主程序 → worker | `START [backend=local\|system] [mode=...] [kind=...] [vad=...] [itn=1] <lang> <threads> <rule1_ms> <rule2_ms> <rule3_ms> <max_seconds> <model_dir>` | 加载模型并开始采集。方括号里的前缀键都可省略，全省略即旧格式（字节不变）；`system` 时 `model_dir` 可为空。端点规则含义同 sherpa（2400/1200/20000 为示例默认）；`max_seconds` 为 0 表示不限，超过自动当作 STOP |
| | `mode=` | 可选：`streaming`（缺省）或 `offline`。缺省不写 |
| | `kind=` | 可选，模型类型，8 个 kebab-case 取值：`online-transducer`（缺省）、`offline-transducer`、`offline-paraformer`、`offline-sense-voice`、`offline-whisper`、`offline-zipformer-ctc`、`offline-nemo-ctc`、`offline-moonshine`。缺省不写 |
| | `vad=` | 可选：`<threshold 0..1>:<min_silence_ms>:<min_speech_ms>:<max_speech_ms>`，缺省由 worker 使用内置默认。VAD 模型不走协议，约定在 `model_dir` 或其父目录里找 `silero_vad.onnx` |
| | `itn=` | 可选，只接受 `1` 或 `0`，仅对 SenseVoice 有意义，缺省 `0`，只在为 1 时写出 |
| | `STOP` | 停止采集，冲刷尾部，发最后的 `FINAL` 与 `STOPPED` 后退出 |
| | `CANCEL` | 不冲刷，直接 `STOPPED` 后退出 |
| | `PING` | 回 `PONG` |
| worker → 主程序 | `READY` | 进程已就绪，可发 START |
| | `PARTIAL <文本>` | 当前句的临时文本，仅在变化时发 |
| | `FINAL <文本>` | 一句话定稿（端点触发后或 STOP 冲刷时） |
| | `ERROR <原因>` | 单行原因，随后退出（模型缺失、麦克风被拒等） |
| | `PONG` / `STOPPED` | 心跳应答 / 已停止 |

前缀键（含 `backend=`）顺序任意、不可重复（重复、未知键、非法值都报解析错误）；序列化固定顺序 `backend mode kind vad itn`，缺省值不写，所以旧格式的字节不变。FINAL / PARTIAL 事件与协议版本都没有变。因为 `vad` 带 f32，`StartRequest` 与 `Command` 不再派生 `Eq`（只保留 `PartialEq`）。

stdin 关闭（主程序退出或崩溃）视为中止：不再发事件，直接退出。进程一次只做一次识别，退出即释放模型与麦克风。首行若带 UTF-8 BOM 会被忽略。

流程：加载完模型才打开麦克风（避免把加载期间的旧音频喂进去）；音频按 320ms（5120 样本）一块喂 sherpa；每块后取结果，文本变化发 `PARTIAL`，`is_endpoint` 为真发 `FINAL` 并重置流；STOP 时补 0.66s 静音、`input_finished` 后冲刷最后一句。

## 模型放置
模型不随仓库打包。目录里需要的文件按模型类型（`kind`）识别，文件名按前缀 / 约定匹配：

| kind | 需要的文件 |
|---|---|
| `online-transducer`、`offline-transducer` | `encoder*.onnx`、`decoder*.onnx`、`joiner*.onnx`、`tokens.txt`（encoder/joiner 优先 `int8`，decoder 优先非 int8） |
| `offline-paraformer`、`offline-sense-voice`、`offline-zipformer-ctc`、`offline-nemo-ctc` | `model.int8.onnx`（优先）或 `model.onnx`，加 `tokens.txt` |
| `offline-whisper` | `*encoder*.onnx`、`*decoder*.onnx`（均优先 int8），加 `tokens.txt` 或 `*-tokens.txt` |
| `offline-moonshine` | `preprocess*.onnx`、`encode*.onnx`（优先 int8）、`uncached_decode*.onnx`、`cached_decode*.onnx`，加 `tokens.txt` |

主程序管理的目录结构：`<数据根>/models/stt/<模型ID>/`，里面是上表的文件，另有 `model.json`（元数据：id、维度、模式、kind、文件清单、压缩包 sha256 与是否已固定）和 `.complete.json`（完成标记，下载解压全部成功后最后写入）。离线模式共用的 `silero_vad.onnx` 放在 `<数据根>/models/stt/` 根下，各模型目录的父目录正好能被 worker 找到。旧版把模型文件直接平铺在 `<数据根>/models/stt/` 下（有 `tokens.txt`）的布局仍兼容，见「模型管理与设置」。

早期已测模型：`csukuangfj/sherpa-onnx-streaming-zipformer-bilingual-zh-en-2023-02-20`（Apache-2.0）。现在的默认与备选以清单为准，见 [research/stt-model-selection.md](../research/stt-model-selection.md)。下载大文件用 `aria2c -x16 -s16 -k8M --continue=true`。

## 离线模式
`START mode=offline kind=offline-* ...` 时 worker 走离线整句后端（`tools/snow-stt/src/offline.rs`）：Silero VAD 切句，每个语音段交给离线识别器整句解码，**只产生 `FINAL`，不产生 `PARTIAL`**。

- `rule1/2/3_ms` 端点规则对离线无效（切句完全由 VAD 决定），START 里照常带着，worker 忽略。
- VAD 默认值：threshold 0.5、min_silence 500ms、min_speech 250ms、max_speech 20000ms（取自 sherpa 示例常见值，**未经评测**，会把较长的中文句切成两段，需要在真实音频上复测）。可用 `vad=` 覆盖。
- 每段前补 400ms 前导音频（不越过上一段的结束位置）：实测 Silero 在语音起点之后才触发，不补会吃掉段首 1~2 个词。
- 识别在后台线程里做：`feed` 只做 VAD 并投递语音段，`poll` 只收已完成结果，所以单段解码耗时不会拖慢 STOP / PING 的响应。`STOP` 时先冲刷 VAD，再**等排队的段全部处理完**才发最后的 `FINAL` 与 `STOPPED`。
- 找 VAD 模型：先 `<model_dir>/silero_vad.onnx`，再 `<model_dir 的父目录>/silero_vad.onnx`，都没有就报 `ERROR`。
- 识别文本会先去掉 `<|...|>` 形式的标签（如 SenseVoice 的 `<|zh|>`）与首尾空白。
- 离线不支持 `backend=system`。kind 与 mode 不一致时 worker 启动即报 `ERROR`，共三条：
  1. 系统后端搭配离线模式或离线 kind：「系统语音后端不支持离线模式或离线模型类型」。
  2. 本地后端 + `mode=offline` 但 kind 是 `online-transducer`：「离线模式需要离线模型类型（kind=offline-*）」。
  3. 本地后端 + 流式模式但 kind 是离线类型：「模型类型 … 只能用于离线模式（mode=offline）」。

### 语言字段
`<lang>` 是识别语言提示：
- `offline-sense-voice`：只认 `zh` / `en` / `ja` / `ko` / `yue`，其余（含 `auto`、`zh-en` 这类组合）回落 `auto`。
- `offline-whisper`：单语言码生效（如 `zh`、`en`）；`auto` 或带 `-`/`_` 的组合值按自动判断。任务固定为 `transcribe`（只转写，不翻译）。
- 其它 kind 不使用语言字段。

## 构建
```
scripts\build-snow-stt.ps1 [-Profile release|debug] [-Test] [-Clippy] [-Jobs 2]
```
- 产物默认在 `build/stt/<profile>/snow-stt.exe`（`$env:CARGO_TARGET_DIR` 可改，但**必须是短路径**：sherpa 预编译包解压路径很长，目录太深会静默解压失败，随后链接报 `LNK1104`）。
- sherpa-onnx-sys 的 build.rs **构建期联网**从 GitHub Releases 下载预编译库（shared 约 7.7MB）。离线环境可设 `SHERPA_ONNX_ARCHIVE_DIR`（本地 tar.bz2）或 `SHERPA_ONNX_LIB_DIR`。
- 使用静态 CRT（`+crt-static`），与 sherpa 预编译包（MT）匹配。脚本自带 `-j 2` 与 BelowNormal，并检查自身无 CRLF。
- 现象记录：`build-snow-stt.ps1` 在这台开发机上的工作副本是 CRLF，脚本自带的 LF 检查会拒绝它（报「含 CRLF，请转成 LF」）；本机评测时用过 LF 副本绕开。已下载好预编译包时可设 `SHERPA_ONNX_LIB_DIR` 复用，不必重新联网下载。这里只记录现象，没有改仓库脚本。
- `-Test` 会把 DLL 放进 `deps/` 再运行测试，因为 shared 链接的测试 exe 启动就要这些 DLL。

## DLL 加载顺序
`sherpa-onnx-c-api.dll` 按名字导入 `onnxruntime.dll`。构建脚本在 exe 旁放三个 DLL：`sherpa-onnx-c-api.dll`、**仓库同版 `onnxruntime.dll`**（取自 `build/mt-quant/ort128/onnxruntime/capi/`，当前 1.28.0.20260724，可用 `$env:SNOW_ORT_DIR` 指向别处；脚本要求版本为 1.28.x）、`onnxruntime_providers_shared.dll`。Windows 默认先搜应用程序目录，所以同目录的这份优先于 PATH 里别的 `onnxruntime.dll`；打包时务必保持三个 DLL 与 exe 同目录。sherpa 自带的 1.28.2 版被替换，替换后的识别结果已在 §8.3 验证一致。

## 运行与自检
手动：
```
snow-stt.exe            # 麦克风；stdin 发 START/STOP
snow-stt.exe --wav a.wav --wav-pad-ms 3000 --stats   # 测试用：wav 代替麦克风，末尾补静音，结束时向 stderr 打统计
```
`--wav` 只接受 16kHz 单声道；wav 读完等价于 STOP。脚本化验证：
```
scripts\verify-snow-stt.ps1 -ModelDir <模型目录> -Wav <wav> [-PadMs 3000] [-Threads 2] [-StopAfterMs 0]
    [-Mode streaming|offline] [-Kind <模型类型>] [-VadModel <silero_vad.onnx>] [-Itn] [-Language zh-en]
```
走真实 stdin/stdout 协议，打印带时间戳的事件序列、退出码、峰值工作集和每块耗时统计。新增参数：
- `-Mode offline`：离线整句识别，须配 `-Kind offline-*`；缺省 `streaming`，此时 START 行与旧版逐字节相同。
- `-Kind`：模型类型，取值同协议 `kind=`。
- `-Itn`：开启逆文本规整（SenseVoice）。
- `-VadModel`：**只做存在性与位置检查**。worker 只按约定找 `<模型目录>/silero_vad.onnx` 或其上级目录，协议不传路径；文件名不对或位置不对脚本会直接报错。
- `-Language`：语言提示，缺省 `zh-en`。

示例（SenseVoice 离线，VAD 放在模型目录的上级）：
```
scripts\verify-snow-stt.ps1 -Mode offline -Kind offline-sense-voice -Itn `
  -ModelDir <数据根>\models\stt\sherpa-onnx-sense-voice-zh-en-ja-ko-yue-int8-2024-07-17 `
  -VadModel <数据根>\models\stt\silero_vad.onnx -Wav a.wav
```

## 单测
`scripts\build-snow-stt.ps1 -Test`（worker 44 个，含主循环 Fake 测试、系统后端的纯逻辑：HRESULT / 状态分类、语言解析、假设升格、节拍来源）与 `cargo test -p snow-stt-protocol`（协议往返与异常输入）。

## 主程序接入
入口代码在 `snow-shot-rs/crates/snow-shot/src/dictation/`，分层如下（纯逻辑层都有离屏单测）：

| 文件 | 职责 |
|---|---|
| `engine.rs` | 工作进程生命周期状态机（纯逻辑，进程经 `SttLink` 抽象，时间由调用方传入） |
| `client.rs` | 定位并拉起 `snow-stt`，读线程转发事件，结束进程只动自己持有的子进程句柄 |
| `config.rs` | 读配置、启动前检查（后端、exe、模型目录） |
| `text.rs` / `overlay_model.rs` | PARTIAL / FINAL 累积与句间空格；浮窗文本模型 |
| `typing.rs` / `focus.rs` / `output.rs` | 键入差异与按键序列、可输入焦点判定、输出去向决策 |
| `flow.rs` / `view.rs` | 宿主（串起以上各层、托盘提示）与右下角浮窗视图 |

平台调用在 `snow-platform`：`text_inject.rs`（`SendInput`）与 `focus_probe.rs`（UI Automation 读数 + 令牌完整性级别）。

### 触发
- 命令：`ToggleDictation` / `StartDictation` / `StopDictation`（`snow-app-core`，不属于 MCP 截图域，不在 `MCP_TOOL_MAP`）。
- 两个全局热键，默认都不绑定：`global_shortcuts/dictation_toggle`（按一下开始、再按一下结束，绑 `ToggleDictation`）与 `global_shortcuts/dictation_hold`（按住说话，按下发 `StartDictation`、松开发 `StopDictation`）。
- `dictation/trigger_mode`：`both`（默认，两个都注册）/ `toggle` / `hold`，决定哪个热键被注册；改动后热键即时重新注册，注册失败会回滚配置并在设置页提示。
- 松开事件：`snow-ui-shell` 的 `HotkeyBinding` 新增可选的 `on_release` 命令（`HotkeyBinding::new(..).with_release(..)`）。global-hotkey 0.8.0 在 Windows 上用 `MOD_NOREPEAT` 注册，`WM_HOTKEY` 之后每 50ms 轮询主键状态并发 `Released`，所以松开延迟最坏约 50ms；以主键松开为准，先松修饰键不触发。没有 `on_release` 的既有热键行为不变（松开仍被忽略）。
- 重复触发：已有会话时再按 Start 被忽略；切换式在收尾阶段再按也被忽略。

### 配置键（`dictation/` 分组）
| 键 | 默认 | 说明 |
|---|---|---|
| `backend` | `local-model` | `local-model` 用本地模型；`system` 用 Windows 系统语音，同样拉起 `snow-stt`（START 带 `backend=system`，不检查模型目录）。选中 `system` 时设置页显示使用前提与设置指引 |
| `trigger_mode` | `both` | 见上 |
| `model_dir` | 空 | 手动模型目录。空 = 用清单里选定的模型（`<数据根>/models/stt/<id>/`）；非空 = 手动目录，按旧行为当流式 `online-transducer` 用，优先于清单，且**不能配合离线模式**；目录不存在时给出带路径的可读错误 |
| `mode` | `streaming` | 识别模式：`streaming` / `offline` |
| `language_dimension` | `bilingual` | 语言维度：`zh` / `en` / `bilingual`，与 `mode` 一起决定清单里的候选 |
| `model_id` | 空 | 空 = 该维度与模式的默认模型；非空 = 用户选的备选模型 ID。切换 `mode` 或 `language_dimension` 时设置页会清空它；ID 已失效（不属于当前候选）时回落默认 |
| `sensevoice_itn` | 开 | SenseVoice 是否启用逆文本规整，只对该模型生效 |
| `translate_enabled` | 关 | 定稿句是否同时翻译，见「语音翻译级联」 |
| `translate_target` | `auto` | 译文目标：`auto`（中文译英文，其它译简体中文）/ `zh-Hans` / `en` |
| `language` | `auto` | 传给 worker 的语言提示（单词） |
| `threads` | 2 | 推理线程，1~8 |
| `max_seconds` | 120 | 单次最长秒数，0 不限；由 worker 到点当作 STOP |
| `output_mode` | `auto` | `auto` / `type` / `overlay`，见「输出行为」 |
| `type_with_overlay` | 关 | 键入时是否同时显示浮窗 |

旧配置缺这些键时补默认值（含两个热键的空绑定）。

### 进程生命周期
1. 定位 exe：环境变量 `SNOW_STT_EXE`（指向不存在的文件不再回退）→ 主程序同目录 → 沿目录向上的开发布局（`build/stt/{release,debug}/`、`snow-shot-rs/tools/snow-stt/target/release/`）。找不到时提示可读错误（放在主程序旁边或设环境变量）。
2. 拉起后等 `READY`（上限 120s），随即发 `START`，**并紧跟一条 `PING`**：worker 只在会话主循环里应答 `PONG`，而主循环要等模型加载完、麦克风打开之后才开始，所以 `PONG`（或第一条 `PARTIAL`/`FINAL`，兼容旧 worker）就表示「已经在听」。从 `START` 到 `PONG` 同样给 120s（冷加载大模型可能很久），期间状态显示「加载中」。协议本身没有专门的「已就绪」事件，这是不改协议的做法。
3. 结束：在听时发 `STOP`，等 `FINAL` + `STOPPED`（上限 10s）；超时改发 `CANCEL` 再等 3s；仍不退出则强制结束，并报错。还没开始听就要结束（按住说话点了一下），没有可冲刷的音频，直接结束进程。
4. 异常：`ERROR`、进程意外退出（带退出码）、写命令失败都会结束会话并给出可读提示；`STOPPED` 之后给进程 2s 自行退出。
5. 主程序退出：发 `CANCEL`、关闭 stdin（worker 视为中止），短宽限后强制结束。结束进程只用自己 `spawn` 得到的子进程句柄，不按进程名查杀。
6. 状态提示：浮窗状态行 + 托盘悬停提示（进行中显示状态，结束后复原）。

## 输出行为
输出有两条去向，设置项 `dictation/output_mode`：
- `auto`（默认）：开始识别时（拉起 worker 的同时）在后台线程探测一次前台焦点；能键入就键入，否则弹右下角浮窗。**判定一次，整轮不变**，识别过程中焦点变化不改变这一轮的去向。判定结果显示在状态里（「正在键入到当前输入框」或「当前无法键入（没有输入焦点），文字显示在这里」等）。探测超过 2.5s 没有结果按「不确定」处理，走浮窗。
- `type`：只键入。若判定不能键入，**不静默丢字**：浮窗弹出，写明原因，文字留在浮窗里。
- `overlay`：只弹浮窗，不探测焦点。
- `type_with_overlay` 开启时，键入的同时也显示浮窗；键入模式下浮窗默认不弹。

### 可输入焦点判定（`focus.rs`，纯函数 `classify`）
读数由 `snow-platform::focus_probe` 采集：`GetForegroundWindow`、UI Automation `GetFocusedElement`（控件类型、是否启用、是否持有/可获键盘焦点、是否密码框、ValuePattern 只读状态、有无 TextPattern）、`GetGUIThreadInfo` 的系统插入符、令牌完整性级别。判定顺序，凡是「否」或「不确定」一律走浮窗：
1. 无前台窗口 → 否。
2. 目标进程完整性高于本进程（UIPI 会静默吞掉注入的按键，`SendInput` 不报错，所以必须主动探测）→ 否；读不出完整性（打不开进程等）→ 不确定。
3. UIA 出错 → 不确定（即使有插入符也不冒险）。
4. 没有焦点元素：有系统插入符才算能键入，否则否。
5. 有焦点元素：禁用 / 密码框 / ValuePattern 只读 → 否；控件类型是 Edit 或 Document 且可获键盘焦点，并且 ValuePattern 可写，或（无 ValuePattern 的）Edit 带 TextPattern → 能；Document 只有 TextPattern 时要同时有系统插入符才算（否则可能是只读网页，判不确定）；其它类型（Pane / Custom 等自绘控件）只有「持有键盘焦点且有系统插入符」才算能。

与最初设想的偏差：「ValuePattern 非只读或带 TextPattern」被收紧了一点——明确只读永远不可编辑，Document 仅凭 TextPattern 不放行，避免把只读网页页面当成输入框。

### 键入（`typing.rs`）
- `SendInput(KEYEVENTF_UNICODE)` 逐字符发送（补充平面字符拆成两个 UTF-16 码元），退格用 `VK_BACK`；回删与重打放进**同一次** `SendInput` 调用，不会被用户按键插进中间。
- **稳定前缀法**：期望文本 = 已落定（各句 FINAL 拼接）+ 当前未落定（PARTIAL）。与已键入内容比较，公共前缀保持，只对不同的尾部回删重打。PARTIAL 修正最多回删 16 个字符，超出的差异等 FINAL 落定时再一次性修正；FINAL 和收尾不限。
- 识别文本先清洗：换行、制表符变空格，其它控制字符丢弃（否则会按下回车提交表单）。句子之间：英文字母数字相接补一个空格，中日文相邻不补。
- 发送前检查修饰键（Ctrl / Alt / Shift / Win）是否仍被物理按住：按住时延后，等放开后下一次同步补齐（按住说话常用带修饰键的热键，所以最终文字通常在松开后才打出来）。结束后补发最多等 3s，仍打不进去则把文字留在浮窗。
- 目标窗口校验：首次键入时记下前台窗口，之后前台窗口变了就停止键入并把全部已识别文字铺到浮窗；`SendInput` 注入数不足也同样兜底到浮窗。
- 退格按 Unicode 标量值计数；组合字符、ZWJ 表情序列在多数输入框里一次退格删得更多，属已知偏差。

### 右下角浮窗（`view.rs`、`overlay_model.rs`）
- 位置：光标所在显示器工作区的右下角，留 16 逻辑像素边距，不盖任务栏；按该显示器缩放比换算物理像素。以 `focus: false`（不激活）方式弹出，用户点击文本区才拿到键盘焦点。
- 文本区（gpui-component `Textarea`）可编辑，里面只放「已落定 + 用户编辑过」的内容；**未落定的 PARTIAL 单独显示在文本区外**（灰色斜体带下划线），所以后续识别永远不会覆盖用户的编辑——落定的 FINAL 追加到文本区**当前内容**末尾。
- 「复制」按钮把文本区当前内容 + 尚未落定的部分写入剪贴板，并提示「已复制」；失败显示原因。
- 结束（`STOPPED`）后窗口保留，直到用户按 Esc / 点「关闭」/ 下一轮开始（下一轮清空）。用户关掉窗口后本轮不再自动重开；浮窗是唯一输出时关窗等于结束这一轮。
- 状态行显示：加载中、正在听（含去向说明）、收尾中、已结束、错误原因。

## 模型管理与设置
### 模型清单
`snow-shot-rs/crates/snow-shot/resources/stt-model-manifest.json`，编译进主程序（`stt_models.rs` 解析，`schema` 当前为 1）。结构：`vad`（共享 Silero VAD：文件名、下载地址、字节数、sha256、许可证 MIT）加 `models` 数组；每个模型含 `id`（即压缩包解压后的目录名）、`dimension`（`zh`/`en`/`bilingual`）、`mode`、`kind`、`role`、`archive`（文件名、地址、字节数、sha256）、`files`（解压后必需的文件）、`license`、`size_bytes`、`peak_mem_mb`（评测口径，仅作提示）、可选 `notes_key`（说明文案的 i18n 键）。解析时会校验 `mode` 与 `kind` 是否匹配（离线 kind 必须配离线模式）。

目前 13 个模型加 1 个共享 VAD：流式 7 个（zh 2、en 2、bilingual 3）、离线 6 个（zh 2、en 2、bilingual 2）。`role`：

| 角色 | 含义 |
|---|---|
| `default` | 该维度与模式的默认（性价比） |
| `alternate` | 备选（更准，或有特点） |
| `legacy` | 旧版，仍可选，不推荐（目前只有流式 bilingual-2023-02-20，保持旧用户可用） |

各组默认与备选及其依据见 [research/stt-model-selection.md](../research/stt-model-selection.md)。

### 下载与安装（`stt_download.rs`）
不新增依赖，沿用 OCR 下载的做法：下载用系统自带 `curl.exe`（`--ssl-no-revoke`，支持续传），哈希用 `certutil`，解压用系统自带 `tar.exe`（bsdtar，原生支持 bz2）。流程：
1. 下载压缩包到 `<数据根>/models/stt/.download/<名>.part`，边下边回报进度，可取消（取消保留 `.part` 以便续传）。
2. 校验大小与 sha256，通过后原子改名；校验失败删除 `.part`，避免续传坏数据。
3. 只解压压缩包里 `<id>/<必需文件>` 到暂存目录（`<id>.extracting`），核对文件齐全。
4. 换入 `<数据根>/models/stt/<id>/`，写 `model.json` 与 `.complete.json`，清理暂存与压缩包。
5. 离线模型额外检查共享 `silero_vad.onnx`（文件在且大小与清单一致才算已安装），缺则一并下载。

**sha256 未固定的放行机制**：清单里 sha256 为空串的资产只校验大小、放行，并记日志；安装结果里的 `unpinned` 列出这些资产，界面据此提示「校验值待固定」。目前 7 个模型压缩包 sha256 为空（GitHub 没有返回 digest），发布前必须固定。模型是否已安装 = `.complete.json` 在且 `files` 全部存在。

### 启动前解析（`dictation/config.rs::prepare_launch`）
把配置快照解析成 `START` 请求，优先级自上而下：
1. 后端为系统语音：不解析模型，START 带 `backend=system`。
2. 后端为本地模型：
   1. `model_dir` 非空（手动目录）：离线模式 → `Failure::ManualDirNeedsStreaming`（目录推断不出模型类型，不猜）；目录不存在 → `Failure::ModelDirMissing(路径)`；否则按旧行为（流式、`online-transducer`、不开 itn）。
   2. 旧版平铺布局：流式、`model_id` 为空、`<数据根>/models/stt` 存在且其下直接有 `tokens.txt` → 沿用该目录，旧用户不用重新下载。
   3. 否则按（语言维度，模式，`model_id`）在清单里取模型：清单里没有 → `Failure::ModelUnavailable(说明)`；未安装 → `Failure::ModelNotInstalled(模型ID)`；离线模型还要求共享 VAD 已装，否则 `Failure::ModelNotInstalled("silero_vad.onnx")`。`itn` 只在选中 SenseVoice 且 `sensevoice_itn` 开时为 1。
3. 找不到 `snow-stt` 可执行文件另有 `Failure::WorkerMissing`（在上面各步之前判断）。

### 设置页（`stt_settings.rs` 纯逻辑，`settings_view.rs` 视图）
语音转文字分组里，识别模式、语言维度、模型三个下拉与一个下载面板：
- 候选由 `list_for(维度, 模式)` 给出（默认在前，其次备选，最后旧版）。下拉标签只含名称与推荐 / 旧版标记（体积与安装状态看下方模型面板，标签更短才不会被截断）；名称取自 `stt_models.ftl`（语料缺失回退模型 ID）。
- **联动清空**：改动模式或语言维度会清空 `model_id`（回到默认）。
- **置灰**：后端为系统语音，或填了手动模型目录时，三个下拉置灰（系统语音优先于手动目录）；手动目录加离线模式是无效组合，会给提示。
- **下载面板**：显示选中模型详情、主按钮（下载 / 取消 / 已就绪 / 别的模型正在下载时禁用）与结果提示（失败、取消、成功）；系统语音、手动目录或清单无模型时不显示。下载在后台线程里跑，进度含阶段、模型名称（VAD 显示文件名）与百分比；用户取消只记 INFO，真正失败才记 WARN。内存偏大的模型（如 x-asr 离线）带警示说明。
- **itn 开关**：只在选中 SenseVoice 且三个下拉没被置灰时显示。
- **翻译开关与目标语言**：配置项 `translate_enabled` 与 `translate_target` 的名称、说明已进语料；`stt_settings.rs` 里已有目标语言行下方的提示逻辑（`translate_note`：可用时预览语言方向，不可用时给原因）与「写入哪些键后需重新扫描翻译模型」的判定（`rescans_translate_support`）。视图接线已完成：`settings_view.rs` / `settings_state.rs` 会引用这两个函数，写入相关键后重新扫描翻译模型。说明位最多容纳两行（中文约 27 字一行、英文约 50 字符一行），文案要控制在两行内；渲染层不做中文避头尾，标点恰好落在行首时会单独成行，所以改文案而不是改渲染。分组标题与侧栏徽标的条目数统一以可见条目为准（隐藏的 itn 开关不计）。

## 语音翻译级联
结论与取舍见 [research/stt-model-selection.md](../research/stt-model-selection.md) §6：端到端语音翻译不可行，所以是「STT 定稿 → 现有 `snow-translate`」。实现在 `dictation/translate.rs`。

- **只翻定稿句**：未落定的 `PARTIAL` 不翻译。翻译跑在后台线程（`snow-dictation-translate`），不阻塞主线程 `tick`；生产实现包住应用共享的 `TranslateHost`（自带结果缓存），沿用用户的翻译设置，只覆盖语言对。
- **对位与旧轮丢弃**：每个清洗后非空的定稿句依次编号，结果按 `(round, seq)` 回主线程；轮次不符或序号越界的结果丢弃，乱序到达也按序号对位。
- **语言对规则**：中文维度源语言固定简体中文，英文维度固定英文，中英混合维度逐句按汉字占比判断源语言。目标：`auto` 为中文译英文、其它译简体中文；`zh-Hans`、`en` 固定。源与目标相同的句子不翻译、只显示原文。
- **可用性**（`TranslationAvailability`）：`Ready` / `Disabled`（开关关闭，不扫描模型目录）/ `NoModel`（没有任何翻译模型）/ `UnsupportedPair`（已装模型不支持所需方向，携带第一个缺失的方向）/ `SameLanguage`。需要的方向里只要有一个被支持就算可用，本轮不被支持的方向的句子只显示原文。不可用时在状态行里提示原因。
- **译文不进键入与复制**：键入只处理原文；浮窗的「复制」只复制文本区内容与未落定部分，不含译文。
- **浮窗显示**：译文是文本区下方的**独立只读列表**，显示最近 2 句（`TRANSLATION_MAX_SENTENCES`），待出显示 `…`、失败显示淡色提示；**不是逐句紧贴在原文旁**，所以原文和译文的对应要靠顺序理解。
- **首版不带上下文**：每句独立翻译。原因：`snow-translate` 的接口目前不支持传上下文，加上下文要先改接口与评测，留作后续（见调研 §10）。

## 系统语音后端
`START backend=system ...` 时 worker 用 `Windows.Media.SpeechRecognition.SpeechRecognizer` 的连续识别（`ContinuousRecognitionSession`），代码在 `tools/snow-stt/src/system.rs`。
- 事件映射：`HypothesisGenerated` -> `PARTIAL`；`ResultGenerated`（状态成功、非空）-> `FINAL`。`STOP` 时 `StopAsync` 冲刷，静默 300ms（上限 3s）取尽回调，仍未被定稿的最后一条假设会升格成 `FINAL`，免得丢字。`CANCEL` / stdin 断开不冲刷；进程退出前停止会话并 `Close` 识别器。
- 音频：系统识别器**只吃默认麦克风、不能喂 PCM**，所以 `feed` 是空操作；主循环用 `ClockSource`（每 100ms 一拍的静音节拍）驱动轮询、命令响应和 `max_seconds` 计时。因此 **`--wav` 与系统后端互斥**（给了会报错）。
- 静默自动停止：连续识别默认 20s 静默会自行结束，已放宽到 1 小时；若会话仍提前结束且状态不是成功 / 用户取消，会以对应类别报 `ERROR`。
- 语言：`auto` 取系统语音语言，其余按完全匹配、再按主语言子标签匹配（`zh-en`、`zh-CN` 都落到 `zh-Hans-CN`）；没有匹配报「语言不可用」。本机支持 `en-US`、`zh-Hans-CN`。
- 错误分类：worker 的 `ERROR` 文本以 `[system:<类别>]` 打头（`online-off` / `mic-denied` / `language` / `network` / `other`，定义在 `snow-stt-protocol::SystemError`），主程序据此取 `.ftl` 里的本地化提示，提示内带「Windows 设置 -> 隐私 -> 语音 / 麦克风」与 `ms-settings:` 指引文字（只是文字，不会替用户打开设置）。分类依据：HRESULT `0x80045509`（隐私声明未接受）、`0x8004503A`（语言包未装）、`0x80070005`（拒绝访问），其余未归类错误在注册表 `OnlineSpeechPrivacy\HasAccepted` 不为 1 时按「联机识别未开」处理；这几个 HRESULT 取自微软文档与社区资料，除第一个外没有在本机逐个触发过。
- 依赖：只给 `snow-stt` 新增 `windows` crate（0.62，`Cargo.lock` 里本来就有，未新增 crate），feature：`Foundation`、`Globalization`、`Media_SpeechRecognition`、`Win32_Foundation`、`Win32_System_Com`、`Win32_System_Registry`。
- 能力探测：`snow-stt.exe --probe-system [--probe-lang <语言>]` 打印联机开关（注册表）、系统语音语言、支持语言、语言解析结果，并实际创建识别器、编译默认听写约束（不开麦克风），然后退出。
- 前提：系统「联机语音识别」打开（它不是纯离线方案）、麦克风隐私放行、对应语言包已装。限制：只能默认麦克风，START 里的线程 / 端点规则等字段被忽略，识别质量由系统决定。

本机实测（Win10 19045，联机开关关闭）：`--probe-system` 报 `online_speech_enabled: false`、语言 `zh-Hans-CN`、支持 `en-US,zh-Hans-CN`、识别器可创建；`--probe-lang ja` 报语言不可用；真实 `START backend=system` 返回 `ERROR [system:online-off] ...`（`StartAsync` 阶段失败，进程退出码 1）。**打开联机开关后的真实听写没有测过。**

## 离线模型参考（来源：评测，非麦克风）
下面是离线模型在评测集（朗读语料，不是麦克风）上的表现，来自 Python 评测，详见选型文档。**`snow-stt` 对各离线模型的 wav 逐个跑通记录本文没有留存，未验证**；离线 VAD 切句在真实音频上的行为也没测。

| 模型（kind） | 评测里观察到的行为 |
|---|---|
| paraformer-zh-small（`offline-paraformer`） | 中文 CER 5.08%，186MiB（Python 口径），最省内存；英文没有评 |
| zipformer-small-en（`offline-transducer`） | 英文 WER 2.11%，体积 27.6MB |
| SenseVoice 2024（`offline-sense-voice`） | 开 ITN 时中文数字被转阿拉伯数字，评测计为错（合并 5.70% 对关 ITN 的 4.08%）；输出带 `<|zh|>` 等标签，worker 会去掉 |
| x-asr 离线（`offline-transducer`） | 合并 3.81% 最准；输出自带标点与大小写；峰值 667MiB（Python 口径）偏大 |
| moonshine（`offline-moonshine`） | base 英文 3.11%（678MiB）、tiny 4.78%（488MiB），内存偏大；只有英文 |
| whisper-base（`offline-whisper`） | 中文 CER 36.77%，大量幻觉与繁体输出，不适合中文 |

## 已知限制与未验证
**以下都没有在真机上验证过，只有离屏单测覆盖了纯逻辑；不要当作已验证的能力。**
- **热键松开事件实机**：`Released` 转发有单测（边沿选择命令），但真实的 `WM_HOTKEY` → 轮询 → 松开链路没有在真机跑过；全局热键无法在无人值守环境里模拟。
- **键入到真实应用**：`SendInput` 注入、回删重打、修饰键延后在记事本 / 浏览器 / Office / 聊天软件里的表现都没测。
- **IME 组合态**：中文输入法正处于拼音组合时，回删可能删到组合串而不是已上屏的文字；Unicode 事件是否绕过组合也没测。没有做组合态检测。
- **游戏、全屏独占程序、远程桌面客户端、虚拟机、带反作弊的程序**：通常忽略合成输入，UIA 也多半查不到焦点——此时判定会走浮窗，但无法保证每种情形都被判成「不能键入」；判成「能键入」而实际键不进去时，没有办法发现（`SendInput` 不报错），文字会丢，需用户改用「只浮窗」。
- **Chrome / Electron**：无障碍树可能首次没有唤醒，`GetFocusedElement` 判成无焦点或拿不到可编辑元素，此时走浮窗（宁可多弹窗）。网页 contenteditable / `<textarea>` 在无障碍被唤醒后通常能判成可编辑，未实测。
- **UIPI / 权限探测**：对管理员窗口、UWP / `ApplicationFrameHost` 的完整性比较没有实机验证；打不开目标进程一律按「不确定」走浮窗。
- **UIA 阻塞**：目标程序无响应时 `GetFocusedElement` 可能卡住。探测在后台线程执行、2.5s 无结果就按不确定处理，但该线程会一直挂到调用返回（不影响主线程）。
- **浮窗**：位置（含多显示器、副屏负坐标、混合 DPI）、「不抢焦点」（依赖 gpui `focus: false` 与 PopUp 窗口样式，未确认不会触发窗口激活）、文本区更新时光标位置（每次落定都会重置选区，用户正在中间编辑时光标会跳到开头）、Esc 只在浮窗拿到焦点后才有效——都是真机项。
- **「已就绪」判定依赖 PONG**：见「进程生命周期」第 2 点；用旧版没有 PING 应答的 worker 时，会一直显示「加载中」直到第一条识别文本出现。
- **系统语音后端**：实机听写质量、延迟、中英文表现**未验证**（本机联机语音识别开关是关的，没有对着麦克风说过话）；只验证了错误路径与能力探测，见「系统语音后端」一节。
- **离线模式**：只在 wav 与 Python 评测上验证过，没有对着麦克风跑过；VAD 默认值（500ms 静音等）未经评测，会把长句切开，需用真实录音复测；离线模型没有做 `snow-stt` 对 Python 的逐句一致性对照。
- **真机验证范围**：已于 2026-10-03/04 在本机（Windows 11，2560x1600，缩放 1.5）截图验证设置页（模式 / 维度 / 模型联动、置灰、itn 显隐、中英文）与真实下载（含取消续传、校验值待固定、离线 VAD 合并下载）。仍未验证：麦克风实时听写（声学回环被麦克风阵列回声消除滤掉）、键入记事本、翻译级联浮窗与翻译模型联调（本机没有翻译模型）。
- **本轮真机截图发现并已修复的界面问题**：英文说明被两行高度截断（精简文案）；模型下拉标签被截断（去掉体积与状态）；下载中显示原始长 id（改显示模型名称）；中文说明里标点孤立成行（调整文案）；分组标题 13 项而侧栏徽标 14（统一口径）；用户取消下载被记成 WARN（改 INFO）。浮窗位置查过：按工作区右下角定位，本机上浮窗底边在任务栏上方 24 像素，盖住的是搜狗输入法悬浮条（独立置顶窗口），不是定位错误。
- **下载**：sha256 为空的 7 个模型只校验大小；各模型许可证有 5 个在清单里标 `unverified`，发布前要核实。
- 打包：三个 DLL 与 `snow-stt.exe` 必须同目录，主程序旁的布局与安装包尚未做；打包脚本与 CI 不覆盖独立 workspace `snow-stt`。

## 许可证注意
- sherpa-onnx 与 sherpa-onnx-sys 为 Apache-2.0；传递依赖许可证已核对，无与 GPL-3.0-only 冲突者（见调研 §8.1）。
- 随包的 `sherpa-onnx-c-api.dll` 内含 **espeak-ng**（TTS 用），上游为 **GPL-3.0-or-later**；用于 GPL-3.0-only 工程可行，但**分发时必须在第三方声明里补 espeak-ng 及 sherpa 预编译库其余部分（ORT MIT、kaldi-native-fbank 等 Apache-2.0）的许可证文本**。
- `scripts/collect-third-party-licenses.ps1` 目前只由 `package-snow-shot.ps1` 以 Qt 版的清单调用，**不覆盖 snow-stt**（它按 `-CargoManifest` 逐个清单收集，且不处理 build.rs 下载的预编译库）。接入打包时需单独加入 `snow-stt/Cargo.toml` 并手写预编译库的声明，本步骤未改动脚本。
- 模型权重另有各自许可，不随仓库分发。
