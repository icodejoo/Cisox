# snow-stt 语音转文字工作进程

> 状态：P1 第二步（含系统语音后端），worker 与协议已接入主程序（热键触发、键入 / 右下角浮窗输出，见「主程序接入」「输出行为」两节；真机项尚未验证，见「已知限制与未验证」）。背景与选型见 [research/speech-to-text-backends.md](../research/speech-to-text-backends.md)（尤其 §8），原则见 [principles.md](../principles.md)。

## 组成
- `snow-shot-rs/crates/snow-stt-protocol`：主 workspace 成员，零依赖的行文本协议，主程序与 worker 共用。
- `snow-shot-rs/tools/snow-stt`：独立 workspace（自己的 `[workspace]` 与 `Cargo.lock`），二进制 `snow-stt`。依赖 sherpa-onnx 1.13.8（shared 链接，用户已批准新增）、协议 crate、`snow-crates/snow-audio-recorder`（WASAPI 默认麦克风采集与 16k 单声道重采样，不自写）。
- 识别后端是 `SttBackend` trait（`feed` / `poll` / `finish`），有两份实现：sherpa（本地模型）与 Windows 系统语音（见「系统语音后端」）；主循环 `session.rs` 只认 trait 和 `AudioSource`，单测用 Fake 后端与脚本化来源，不依赖麦克风和模型。

## 协议
每条消息一行 UTF-8，字段以空格分隔，自由文本放行尾，其中 `\`、换行、回车、制表符转义为 `\\`、`\n`、`\r`、`\t`（Windows 路径里的反斜杠也要双写，用 `Command::to_line` 生成即可）。

| 方向 | 消息 | 说明 |
|---|---|---|
| 主程序 → worker | `START [backend=local\|system] <lang> <threads> <rule1_ms> <rule2_ms> <rule3_ms> <max_seconds> <model_dir>` | 加载模型并开始采集。`backend=` 可省略，省略即本地模型（旧格式不变）；`system` 时 `model_dir` 可为空。端点规则含义同 sherpa（2400/1200/20000 为示例默认）；`max_seconds` 为 0 表示不限，超过自动当作 STOP |
| | `STOP` | 停止采集，冲刷尾部，发最后的 `FINAL` 与 `STOPPED` 后退出 |
| | `CANCEL` | 不冲刷，直接 `STOPPED` 后退出 |
| | `PING` | 回 `PONG` |
| worker → 主程序 | `READY` | 进程已就绪，可发 START |
| | `PARTIAL <文本>` | 当前句的临时文本，仅在变化时发 |
| | `FINAL <文本>` | 一句话定稿（端点触发后或 STOP 冲刷时） |
| | `ERROR <原因>` | 单行原因，随后退出（模型缺失、麦克风被拒等） |
| | `PONG` / `STOPPED` | 心跳应答 / 已停止 |

stdin 关闭（主程序退出或崩溃）视为中止：不再发事件，直接退出。进程一次只做一次识别，退出即释放模型与麦克风。首行若带 UTF-8 BOM 会被忽略。

流程：加载完模型才打开麦克风（避免把加载期间的旧音频喂进去）；音频按 320ms（5120 样本）一块喂 sherpa；每块后取结果，文本变化发 `PARTIAL`，`is_endpoint` 为真发 `FINAL` 并重置流；STOP 时补 0.66s 静音、`input_finished` 后冲刷最后一句。

## 模型放置
模型不随仓库打包。目录里需要流式 Zipformer transducer 的四类文件，文件名按前缀识别：`encoder*.onnx`、`decoder*.onnx`、`joiner*.onnx`、`tokens.txt`（encoder/joiner 优先 `int8`，decoder 优先非 int8）。已测模型：`csukuangfj/sherpa-onnx-streaming-zipformer-bilingual-zh-en-2023-02-20`（Apache-2.0）的 `encoder-epoch-99-avg-1.int8.onnx`、`decoder-epoch-99-avg-1.onnx`、`joiner-epoch-99-avg-1.int8.onnx`、`tokens.txt`。下载大文件用 `aria2c -x16 -s16 -k8M --continue=true`。

## 构建
```
scripts\build-snow-stt.ps1 [-Profile release|debug] [-Test] [-Clippy] [-Jobs 2]
```
- 产物默认在 `build/stt/<profile>/snow-stt.exe`（`$env:CARGO_TARGET_DIR` 可改，但**必须是短路径**：sherpa 预编译包解压路径很长，目录太深会静默解压失败，随后链接报 `LNK1104`）。
- sherpa-onnx-sys 的 build.rs **构建期联网**从 GitHub Releases 下载预编译库（shared 约 7.7MB）。离线环境可设 `SHERPA_ONNX_ARCHIVE_DIR`（本地 tar.bz2）或 `SHERPA_ONNX_LIB_DIR`。
- 使用静态 CRT（`+crt-static`），与 sherpa 预编译包（MT）匹配。脚本自带 `-j 2` 与 BelowNormal，并检查自身无 CRLF。
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
```
走真实 stdin/stdout 协议，打印带时间戳的事件序列、退出码、峰值工作集和每块耗时统计。

## 单测
`scripts\build-snow-stt.ps1 -Test`（worker 25 个，含主循环 Fake 测试、系统后端的纯逻辑：HRESULT / 状态分类、语言解析、假设升格、节拍来源）与 `cargo test -p snow-stt-protocol`（协议往返与异常输入）。

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
| `model_dir` | 空 | 空则用 `<数据根>/models/stt`；不存在时给出带路径的可读错误 |
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
- 打包：三个 DLL 与 `snow-stt.exe` 必须同目录，主程序旁的布局与安装包尚未做。

## 许可证注意
- sherpa-onnx 与 sherpa-onnx-sys 为 Apache-2.0；传递依赖许可证已核对，无与 GPL-3.0-only 冲突者（见调研 §8.1）。
- 随包的 `sherpa-onnx-c-api.dll` 内含 **espeak-ng**（TTS 用），上游为 **GPL-3.0-or-later**；用于 GPL-3.0-only 工程可行，但**分发时必须在第三方声明里补 espeak-ng 及 sherpa 预编译库其余部分（ORT MIT、kaldi-native-fbank 等 Apache-2.0）的许可证文本**。
- `scripts/collect-third-party-licenses.ps1` 目前只由 `package-snow-shot.ps1` 以 Qt 版的清单调用，**不覆盖 snow-stt**（它按 `-CargoManifest` 逐个清单收集，且不处理 build.rs 下载的预编译库）。接入打包时需单独加入 `snow-stt/Cargo.toml` 并手写预编译库的声明，本步骤未改动脚本。
- 模型权重另有各自许可，不随仓库分发。
