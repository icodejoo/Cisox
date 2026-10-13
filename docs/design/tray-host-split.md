---
title: 托盘宿主 + 按需 UI 进程（拆分方案）
status: approved
updated: 2026-10-12
summary: 把常驻托盘的最小 Win32 宿主和承载全部窗口的 GPUI UI 进程拆开：进程边界、IPC、退出规则、冷启动预算、分阶段验收与回退。
---

> 状态：**已审定（2026-10-12）**，§10 开放问题全部按推荐拍板，可按 §8 分阶段实施。
> 依据：[../research/tray-idle-memory.md](../research/tray-idle-memory.md)（托盘态多出的约 70MB 里，IME/UIA 注入进程内清不掉，只有换进程能清干净）；原则见 [../principles.md](../principles.md)；约定见 [../cisox-gpui-migration-plan.md](../cisox-gpui-migration-plan.md) §7。
> 已拍板、本文不再讨论：①目标只管托盘态内存；②架构是"托盘宿主（不链 GPUI、不碰 D3D/IME/UIA）+ 按需 UI 进程"；③UI 进程在所有窗口关闭后**立即退出**，不保活；④先出方案、审过再做；⑤无障碍（AccessKit/UIA）曾关闭，阶段 0 实测对内存零收益（§12.9 第 7 条），2026-10-12 已回退，保留屏幕阅读器支持。

## TL;DR

- **边界一句话**：宿主只管"能让用户唤起 Cisox 的东西"——托盘图标与菜单、全局热键（含前台全屏闸门、暂停开关）、鼠标手势钩子、单实例与命令行入口、开机自启入口、UI 进程的拉起与仲裁。其余一律在 UI 进程：窗口、配置读写、i18n、历史、贴图、录屏/语音/翻译/OCR 的调度、MCP、更新检查。
- **宿主不读 config.json、不带 i18n、不画图标**。UI 进程每次配置变化都把"宿主要用的东西"（热键表、托盘菜单快照含已渲染图标与文案、手势绑定、几个开关）写成一份 `host/spec.json` 并通知宿主。宿主只认这份快照。
- **两个 exe**：宿主（沿用 `snow-shot.exe` 这个名字，保持命令行与自启兼容）和 `snow-shot-ui.exe`。单 exe 做不到：`d3d11 / dxgi / uiautomationcore / imm32` 是现 exe 的静态导入（见研究文档 §2），进程一起来就会被加载。
- **本机实测基线（本次做的一次性探针，不入仓库）**：只有 `tray-icon + global-hotkey`、12 个热键、20 项菜单的空闲进程，私有提交约 2.9MB，工作集 12MB（绝大部分是共享 DLL 页），`EmptyWorkingSet` 后工作集 1.2MB。现状空闲 56.8MB、截图后 134~136MB。真宿主估计 3~4.5MB 提交，细节见 §7。
- **代价要说在前面**：UI 进程"立即退出"意味着**每次截图都是冷拉起**，现在的 0.57s 热路径不复存在（除非别的窗口恰好开着）。估算冷拉起热键→首帧 0.65~0.9s，比现在慢 0.1~0.3s，换托盘态内存从 134MB 降到个位数。靠"UI 启动时并行采集"（§6）把冷拉起压到 1.14s 以下，不靠常驻预热。
- **分 6 个阶段**，每阶段可单独提交、单独回退；阶段 2 之前默认行为不变，阶段 4 才切默认。单进程模式保留为回退开关。

## 1. 现状里和本方案直接相关的事实

读代码得到，均已核对：

- 托盘、热键、单实例、IPC、鼠标手势、MCP、贴图、设置、翻译、录屏、语音转文字全在一个进程里，入口 `snow-shot/src/main.rs::run_primary` → `ui::run_resident`（GPUI `QuitMode::Explicit`）。
- 托盘和热键后端（`tray.rs`、`hotkey.rs`、`service_thread.rs`、`native.rs` 的消息循环）**与 gpui 无关，但住在 `snow-ui-shell` 里**，而这个 crate 依赖 gpui；它们还依赖 `snow-app-core`（`AppCommand`），后者拉进 `snow-draw-engine-core`、`tracing-subscriber`。宿主不能直接依赖它们，得抽出来。
- 托盘菜单（`app_runtime.rs::build_tray_spec_with_groups`）要用 i18n 取文案、`IconRenderer` 渲染 16px 图标、读 `tray/menu_options`、读贴图分组（UI 里的状态）、读"热键已暂停 / 前台全屏闸门"勾选。全是 UI 进程才有的东西。
- 热键注册（`register_all_hotkeys_gated`）依赖 snow-config 的约 20 个键 + 语音转文字触发模式；设置页改热键要**同步**知道注册成败，失败要回滚配置（`on_config_changed`）。
- 前台全屏闸门（`fullscreen_gate.rs`）只是热键派发前的一层判定，探测函数 `snow_platform::window_rect::focused_fullscreen_window_exists` 在 snow-platform，无 gpui。
- 鼠标手势钩子（`snow_platform::global_mouse`）在 snow-platform，无 gpui；手势事件流（Begin/Update/Finish）现在直接推给覆盖窗，覆盖窗没就绪时先缓存（`GestureSession`），这个缓存逻辑天然兼容"UI 晚几百毫秒才起来"。
- 单实例是互斥体 `Local\cisox.snow_shot.single_instance` + 当前用户 ACL 的命名管道，线协议 `CISOX1 <命令>`，一次一帧，7 个命令（`snow-platform/src/single_instance.rs`）。另有按行收发的 `line_pipe`（MCP 用）。
- 剪贴板用 `SetClipboardData` + `GlobalAlloc`（立即渲染，没有 OLE 延迟渲染），UI 进程退出后内容留在剪贴板里。（读代码，真机待验。）
- 截图采集用 GDI（`snow_platform::capture`），无 D3D，宿主链接它也不会引入显卡栈。
- 开机自启写 HKCU Run，指向 `current_exe()`（`system_settings.rs`），开发构建不改注册表。
- OCR worker 空闲 30s 退出、翻译 worker 默认空闲 120s 退出，都由 UI 进程拉起并在 `shutdown_app` 里关。
- 配置只有 UI 进程读写（`ConfigStore`，内存全量 + 整文件 flush）。**本方案保持"UI 进程是配置唯一写者"**。

## 2. 进程边界

判定口径：这个能力在"没有任何窗口"时是否必须有人在场。必须 → 宿主；否则 → UI。

| 能力 | 归属 | 必须常驻？ | 理由 / 做法 |
| --- | --- | --- | --- |
| 托盘图标、右键菜单、左/中键动作 | 宿主 | 必须 | 菜单内容是 UI 预渲染的快照（文案、16px 图标、勾选）。宿主不带 i18n、不带 icons。点击只发"动作 id" |
| 全局热键（含按下/松开边沿） | 宿主 | 必须 | `RegisterHotKey`。热键表 = 快照里的 `hotkeys[]`（已解析成 mods+vk，宿主不需要解析器）。动作只是命令 id，转给 UI |
| 热键暂停开关、前台全屏闸门 | 宿主 | 必须 | 都是"热键派发前"的判定，且要在 UI 不在时也生效。状态存宿主内存，菜单勾选宿主自己改。闸门开关要落配置，走 §5 的 `set_config` |
| 鼠标手势钩子 | 宿主 | 有绑定才常驻 | 没绑定不装钩子（现状行为）。宿主进程要声明 PerMonitorV2 DPI，否则钩子坐标被虚拟化。事件合并后（≤120Hz）发给 UI |
| 单实例、`--cmd` 命令行入口 | 宿主 | 必须 | 沿用互斥体和 v1 管道，CLI 命令不变，宿主收到后路由给 UI |
| 开机自启（HKCU Run） | 宿主是启动对象；写注册表的动作在 UI | 入口必须 | 设置页改开关时 UI 写注册表，值指向宿主 exe。宿主自己不碰注册表 |
| UI 进程的拉起、仲裁、崩溃熔断 | 宿主 | 必须 | §4 |
| 通知提示（现在是改托盘 tooltip） | 宿主显示，UI 发起 | 按需 | UI → 宿主 `tooltip` 消息，宿主不关心内容 |
| 登录时恢复贴图 | 宿主触发，UI 执行 | 登录时一次 | 快照带 `pins_to_restore`，大于 0 就拉起 UI（`--restore-pins`）。有贴图时 UI 本来就不退 |
| 更新检查 | UI（无窗口任务） | 按需 | 快照带 `update_due_at`，宿主登录后到点就拉起 `--headless update-check`，查完 UI 自己退，有新版本经宿主 tooltip 提示 |
| MCP 监听 | UI | **v1：`mcp/enabled` 为真时 UI 保活** | 用户显式开了才有，默认关。宿主中转（宿主收连接、拉起 UI、转发）是后续可选项，见 §10 问题 2 |
| OCR 预热（`model_hot_start`） | UI | 同上：开启则 UI 保活 | 默认关。语义是"用户明确要常驻换速度"，保持这个语义 |
| 设置、历史、贴图管理、翻译页/浮窗、录屏控制条、语音浮窗、覆盖窗、贴图窗 | UI | 按需 | 都是窗口 |
| 配置读写、i18n、贴图/历史仓储、worker 调度（OCR/翻译/录屏/STT/表格/公式） | UI | 按需 | 不变 |
| 崩溃转储、日志 | 宿主与 UI 各自一份 | 必须 | 宿主日志独立文件（`logs/host.log`，限 256KB 滚动），不引入 `tracing-subscriber` |

**宿主要不要 i18n 和 snow-config？不要。** 宿主若链接 snow-config，要带上 snow-app-core（→ draw-engine-core、tracing-subscriber/regex）；若再带 snow-i18n，fluent 两套语料（en-US 484K + zh-CN 465K 的 ftl）解析后堆占用估计几 MB（估算，未测）；菜单图标还要 snow-ui-icons 光栅化。这些加起来会吃掉宿主"个位数 MB"的目标，所以用快照。快照的代价是：**语言/主题/贴图分组变化只有在 UI 进程跑过一次之后才会反映到托盘**（改语言必然要开设置窗，UI 本来就在；系统语言变了而 UI 一直没起，托盘文案会旧，可接受）。

**快照 `host/spec.json`**（版本号 `spec_version`、单调 `rev`；UI 原子写，宿主按 `rev` 去重）：

```
spec_version, rev
hotkeys[]   { id, config_key, mods, vk, text, on_release_id?, stays_when_paused }
tray        { enabled, tooltip, paused_tooltip, icon_rgba(base64, 32x32),
              menu[] { kind: item|sep, label, enabled, checked?, icon_rgba?, action_id },
              on_left, on_middle, on_double, theme: dark|light|system }
mouse       [ { action, modifiers, button } ]
gate        { fullscreen_disable: bool }
strings     { notice_paused, notice_resumed, notice_resume_failed, notice_gate_on, notice_gate_off }
flags       { keep_alive: ["mcp","ocr_hot_start"], pins_to_restore: n, update_due_at: unix }
```

`theme: system` 由宿主在弹菜单前读注册表 `AppsUseLightTheme`，其余值 UI 已定好。快照缺失、损坏、版本不认识时，宿主先用内置最小菜单（只有"退出"）起托盘，再拉起 `--headless emit-spec` 的 UI 生成快照。内置最小菜单是宿主里**唯一**写死的文案，走常量，不进 i18n。

## 3. 进程形态与 Cargo 布局

**选两个 exe，不选"同一 exe 两种模式"。** 同 exe 的话 GPUI 栈、静态导入都跟着进宿主；用 `/DELAYLOAD` 可以压，但脆、也没法在 CI 里简单验证。两个 exe 的好处是"宿主没链接 X"能在二进制层面验。

| 项 | 做法 |
| --- | --- |
| 宿主 exe | `snow-shot.exe`（沿用名字：命令行 `--cmd`、HKCU Run、快捷方式、文档里都是它）。release：`opt-level=3`、lto、`panic=abort`、strip，静态 CRT。预估 0.5~1MB（探针 0.31MB） |
| UI exe | `snow-shot-ui.exe`，即现在的 snow-shot bin 改名。模式：`--ui`（宿主拉起，带会话管道名和令牌）、`--headless <job>`（无窗口任务，不初始化 GPUI）、`--resident`（旧单进程行为，回退用） |
| 直接双击 UI exe | 不带 `--ui` 也不带 `--resident`：转去启动宿主然后退出 |
| 安装 | 两个 exe 放同一目录（worker 本来就按 `exe_dir()` 找）。Rust 版打包脚本尚不存在，等它出现时纳入；现在 `snow-update` 只下载不安装，不受影响。将来自动安装要先 `--cmd quit` 宿主再替换 |
| 自启 | `set_auto_start` 的 exe 参数改为宿主路径（UI 取同目录 `snow-shot.exe`） |
| 单实例 | 宿主持有现有互斥体。UI 另持 `Local\cisox.snow_shot.ui` 作保险：拉起时若已存在，新 UI 直接退出并回报。`--resident` 的 UI 持宿主那个互斥体，所以两种模式互斥，不会双注册热键 |
| MCP 描述符 | 路径、内容不变（`<数据根>/mcp/descriptor.json`），由 UI 写。UI 保活时描述符有效，不保活时 MCP 本来就是关的 |
| 日志目录 | 同一个 `logs/`，宿主写 `host.log`，UI 沿用现有滚动文件 |
| 数据根 | 宿主要用和 UI 一样的数据根解析（便携标记 `__data_directory`、AppData 目录、上游目录保护）。现在这段在 `snow-config::paths`，依赖 `snow-app-core` 的常量，抽成零依赖的 `snow-data-paths` |

**Cargo 布局**（`snow-shot-rs/crates/`）：

```
snow-data-paths/      新  从 snow-config::paths 抽出：数据根解析 + APP_ID / PRODUCT_NAME 常量。零依赖。snow-config、snow-app-core 改为重导出
snow-host-protocol/   新  spec 结构、会话消息、帧编解码、协议版本与常量。只依赖 serde、serde_json
snow-shell-core/      新  从 snow-ui-shell 抽出的无 gpui 部分：hotkey（解析+服务）、tray、service_thread、error、
                          native 里的消息循环/唤醒器/弹出菜单主题。动作负载用不透明 id，不依赖 snow-app-core / snow-capability
snow-tray-host/       新  bin 名 `snow-shot`。依赖 snow-host-protocol、snow-shell-core、snow-data-paths、snow-platform
ui/snow-ui-shell      改  依赖 snow-shell-core 并重导出，原路径 `snow_ui_shell::{hotkey,tray,...}` 不变；AppCommand 适配留在这里
snow-shot             改  bin 改名 snow-shot-ui；新增 spec 生成（纯函数）、会话客户端、活动跟踪
```

`snow-platform` 本身不链 gpui，宿主直接用它的 `single_instance`、`global_mouse`、`system_integration`、`window_rect`、`crash`。它的 `windows` 特性很宽（含 UIA、WinRT OCR），但只有被引用的函数才会进导入表，所以**用二进制层面的闸门兜底**（下面第 3 条）。

**workspace-guard 怎么保证宿主不带 GPUI**，三层：

1. 现有规则不动（`gpui::` 只许出现在 `snow-ui-shell`）。
2. 新增依赖闭包规则：`workspace-guard` 里加测试，解析 `Cargo.lock`，算 `snow-tray-host` 与 `snow-host-protocol` 的传递依赖，**禁止**出现：`gpui*`、`snow-ui*`、`snow-app-core`、`snow-config`、`snow-i18n`、`snow-canvas-*`、`snow-history`、`snow-translate`、`tokio`/`async-std`/`smol`/`futures-executor`、`ort`、`image`、`tiny-skia`、`resvg`、`fluent*`、`tracing-subscriber`。纯读文件，不起 cargo 进程。
3. 导入表闸门：CI 的 Windows 任务里加一步 `scripts/check-host-imports.ps1`（`dumpbin /dependents`，白名单：kernel32、user32、shell32、gdi32、advapi32、comctl32、ntdll、api-ms-win-*；黑名单：`d3d*`、`dxgi`、`dwrite`、`d2d1`、`uiautomationcore`、`windowscodecs`、`mfplat` 等）。**白名单以阶段 0 探针的实际导入表定稿**，不凭印象写死。

## 4. IPC

**两条管道，各管各的**：

| 管道 | 用途 | 协议 |
| --- | --- | --- |
| 控制管道（现有，`CISOX1`） | 命令行 `--cmd`、第二次启动唤醒。宿主是服务端 | 不改。可后续给 `--cmd` 加新命令名，不在本方案范围 |
| 会话管道（新） | 宿主 ↔ 当前这个 UI 进程的长连接，双向 | JSON 行。复用 `line_pipe`（当前用户 DACL、重叠 IO）。名字带一次性 `launch_id`；UI 启动参数 `--ui --session <名字> --token <32 字节随机十六进制>` |

宿主**先创建管道实例再 `CreateProcess`**，UI 启动即可连，不存在"连不上"的窗口。UI 连上后发 `hello{pid, proto, token}`，令牌和 PID 都对得上宿主才接受。

**命令集（宿主→UI）**：不发明新命令名，直接用现有的两套 id：托盘信号（`pin_clipboard`、`history`、`main_window`、`settings`、`pin_manage`、`group:<id>`、`group_new`、`group_delete_empty`）和 `AppCommand`/`QuickAction` 名（截图、录屏、长截图、输入框翻译、语音 `start|stop|toggle`、各快捷动作）。UI 侧复用 `map_tray_signal` / `register_bus_handlers`，所以 UI 内部处理逻辑一行不改。另有：`restore_pins`、`gesture{begin|update|finish|cancel, id, x, y}`、`set_config{key, value}`、`shutdown`（优雅退出）。

**UI→宿主**：`hello`、`ack{seq}`、`tooltip{text}`、`spec_changed{rev}`（UI 已写好快照）、`hotkeys_apply{rev}` 的应答、`exit_request{last_seq}`、`activity{windows, pins, busy_reasons}`（给崩溃恢复和日志用）。

**宿主状态机**：`Absent → Starting → Running → Exiting → Absent`，另有 `Backoff`（熔断）。

| 状态 | 收到命令 | 退出条件 |
| --- | --- | --- |
| Absent | 入队，创建管道，`CreateProcess`，`AllowSetForegroundWindow(ui_pid)` → Starting | — |
| Starting | 入队，保序 | 收到 `hello` → Running，按序 flush；10s 没 hello → 杀掉、记日志、tooltip 提示失败、计一次崩溃 |
| Running | 带 `seq` 写入管道，等 `ack`（1.5s，超时重发一次再丢弃并记日志） | 收到 `exit_request` 或进程句柄触发 |
| Exiting | 入队 | 旧进程句柄触发后才拉起新 UI，避免两个 UI 同时写数据目录 |

**"UI 正在退出时来了新热键"的竞态——用序号握手仲裁，不用定时器**：UI 判定"无窗口且无活动"后，发 `exit_request{last_seq}`（它处理过的最后一条命令序号）。宿主比较自己已投递的最大序号：相等 → 回 `exit_ok`，立刻转 Exiting，此后的命令全部入队；不等 → 回 `exit_denied`，UI 继续处理那几条命令（它们几乎必然会开窗，自然不退）。宿主是唯一仲裁方，不存在"命令写进了一个已决定退出的 UI"。

**其余边界**：

- **命令队列**：上限 64 条、30s 过期；鼠标手势的 `update` 只保留最新一条。语音"按住说话"的按下/松开保序，所以松开比 UI 起来得早也不会丢。
- **UI 崩溃**（进程句柄触发且没走过 `exit_ok`）：不重放未确认命令（重放可能二次截图）。熔断：60s 内崩 3 次进入 `Backoff` 5 分钟，期间 tooltip 提示"界面进程反复异常退出"，托盘菜单的"退出""打开日志目录"仍可用。非熔断崩溃时，若崩溃前 `activity.pins>0`，自动 `--restore-pins` 拉起。
- **宿主崩溃**：UI 管道断开即视为宿主死亡。UI 走一遍优雅退出（`shutdown_app`，贴图落盘），最后一步拉起新宿主，再退出。新宿主起来后若 `pins_to_restore>0`，会按 §2 的规则把 UI 拉回来。宿主崩溃转储走现有 `snow_platform::crash`。
- **超时**：`exit_ok` 之后宿主等旧进程退出 10s，仍不退才 `TerminateProcess` 并写 warn 日志（研究文档说"退出不得做成强杀"，这里只是挂死时的最后手段，正常路径一定是优雅退出）。
- **设置页改热键**：UI 写快照 → `hotkeys_apply{rev}` → 宿主重注册 → 回每个 `config_key` 的成败 → UI 失败则回滚配置并提示（现有 `on_config_changed` 逻辑，只是"注册"这一步变成一次往返，超时 2s 按失败处理）。
- **CLI 并发**：多个 `--cmd` 同时到，宿主串行处理。

## 5. 状态归属与"所有窗口关闭"的定义

**UI 进程退出条件** = 没有任何窗口 **且** 没有活动守卫（ActivityGuard），在主线程事件处理完、收件箱为空时判定，不加定时保活。

"窗口"包含：覆盖窗（每屏一个）、贴图窗、设置/历史/贴图管理/翻译页/翻译浮窗/识别结果窗、录屏控制条、长截图控制窗、语音浮窗。**所以有贴图时 UI 不退**，这是预期行为。

"活动守卫"覆盖没有窗口、但退出会丢事的情况（实现为 RAII 计数，持有即不退）：

| 活动 | 说明 |
| --- | --- |
| 采集进行中（`capture_in_flight`）、直接截图进行中 | 窗口还没出来，不能退 |
| 延迟截图倒计时（`DelayGate`） | 倒计时期间没有窗口 |
| 覆盖窗关闭后投递的后续事件 | `RecordingRegionChosen`、`ScrollRegionChosen`、`PinCreate`、导出等；因为"收件箱为空"才判定，天然覆盖 |
| 录屏 worker 存活、语音转文字进行中、长截图采集线程 | 对应控制窗在，但守卫兜底 |
| 模型/运行时/更新下载、配置归档导入导出、更新检查 | 设置窗关了下载不能被杀 |
| OCR/翻译 job 的结果还有消费者 | 消费者是窗口时窗口守着；无窗口消费者（如贴图 OCR）持守卫 |
| 历史写入器未 flush | 退出前必须落盘 |
| `keep_alive` 原因：`mcp/enabled`、`text_recognition/model_hot_start` | 用户显式要求常驻 |

**各长生命周期功能的影响**：

| 功能 | 影响 |
| --- | --- |
| 截图历史 | 仓储在磁盘，进程内只有写入线程；退出前 flush 完才发 `exit_request` |
| 钉图 | 有窗口就保活；登录时恢复靠宿主按 `pins_to_restore` 拉起；UI 崩溃后恢复见 §4 |
| 录屏进行中 | 控制条是窗口，保活；停止后窗口关闭才退。录屏热键（停止）宿主照常转发 |
| 语音转文字进行中 | 浮窗和 worker 都在，保活。**热键冷启动延迟问题见 §10 问题 4** |
| 翻译浮窗 | 窗口，保活 |
| worker 进程 | 随 UI 退出（走现有 `shutdown`）。OCR 30s / 翻译 120s 的空闲驻留不再有意义，见 §10 问题 5 |
| 全局热键在 UI 进程内的部分 | 没有。热键全部在宿主；覆盖窗内的快捷键是窗口自己的按键处理，不受影响 |
| 前台全屏停用热键、暂停热键 | 在宿主，状态存宿主内存；两个开关都要持久化，落配置走 `set_config`：UI 在则直接发，UI 不在则宿主 `--headless set-config key=value` 拉起一个无窗口 UI 写完就退（UI 仍是配置唯一写者） |
| 剪贴板 | 立即渲染，退出后保留（读代码，**阶段 0 真机验证**：复制后立即让 UI 退出，另一个程序能粘贴） |

宿主自己的持久状态只有 `host/state.json`（上次更新检查触发时间、崩溃计数），不碰 config.json。

## 6. 启动性能

**预算**（估算，阶段 0 用实测替换）。已知实测：现单进程常驻热路径 0.57s；冷启动 1.14s，其中托盘/热键初始化约 0.3s；采集+像素转换 170~260ms（V3，2560×1440）。

| 步骤 | 估算 | 说明 |
| --- | --- | --- |
| 宿主收到热键 → `CreateProcess` 返回 | 10~30ms | 宿主工作集被 trim 过，首次缺页略慢 |
| UI exe 加载到 `main` | 100~250ms，**待测** | 约 25MB exe + 静态导入的 d3d11/dxgi/uiautomationcore；冷盘、杀软首扫会更久 |
| GPUI 初始化 + 配置加载 + 建 D3D 设备 | 250~350ms | 1.14 − 0.3 − 0.57 的剩余部分，粗拆 |
| 采集（并行，见下） | 170~260ms | 与上一行重叠 |
| 开覆盖窗到首帧 | 约 0.2~0.3s | 热路径里这部分占大头 |
| 会话握手 + 命令投递 | 5~20ms | 管道往返 |

**目标**：冷拉起热键→首帧**验收线 ≤ 0.9s，优化目标 ≤ 0.7s**；估算区间 0.65~0.9s。参照：1.14s − 0.3s（托盘热键初始化搬进宿主）= 0.84s，再减采集并行省下的约 0.2s ≈ 0.65s。UI 恰好因别的窗口存活时，热路径多一跳 IPC，要求 ≤ 0.57s × 1.1。

**优化点，按"值不值"排序**：

1. **UI 启动即并行采集（推荐，阶段 5）**：UI `main` 的第一件事就起采集线程（GDI，不需要 GPUI），GPUI 初始化同时进行，采集结果放着，事件循环起来后推进收件箱。这一步不跨进程、不碰显卡，能拿到绝大部分重叠收益（估算省 ~0.2s）。
2. **宿主侧采集 + 共享内存交给 UI（评估：不作为首选，数据支持再做）**。可行性：宿主能用 GDI 采，不引入 D3D；交接用 `CreateFileMapping` 匿名节 + `DuplicateHandle` 到 UI 进程。相对 1 多出的收益只有"UI 进程创建、加载到 `main`"这一段（100~250ms，待测）。代价：宿主要 PerMonitorV2 DPI 并枚举显示器和缩放；两屏整图约 29MB（2560×1440 BGRA 每屏 14.7MB）在宿主里瞬时出现，必须用节对象保证交完立即还给系统，不能留在堆里；UI 侧要从映射视图拷成 `Vec`（14MB 约 3~5ms）或改 `FrozenFrame` 持借用；HDR、旋转屏、跨适配器的回退面翻倍；采集失败的归属变复杂。**判定规则**：阶段 0 测出"CreateProcess → UI 开始采集"> 150ms 才立项。
3. **预读文件 / 预载 DLL**：宿主**不预载任何 DLL**（会涨宿主私有内存，违背目标）。UI exe 的预读靠系统 Prefetch/SysMain，不自己做；阶段 0 测"开机后第一次拉起"比"页缓存热"慢多少，超过 300ms 才考虑"登录后 30s、低 I/O 优先级顺序读一遍 UI exe"（占页缓存，不占宿主工作集，可回收），默认关。
4. **UI 快退**：`exit_ok` 后做完 `shutdown_app`（贴图落盘、worker 关闭、历史 flush）就 `process::exit`，不等 GPUI 逐个析构。目标退出耗时 ≤ 150ms（待测），因为宿主要等旧进程退出才能拉新的。
5. **无窗口动作走 `--headless`（阶段 5）**：直接截图到剪贴板/文件、全屏截图这类不开窗口的快捷动作，不必初始化 GPUI，估算比开 UI 快 ~0.3s。先看 `direct_capture` 对 `ShellContext` 的依赖再定。

## 7. 宿主内存目标与做法

**本机实测**（2026-10-12，release，`lto=fat`、`panic=abort`、strip；i5-13500、Win10 19045；一次性探针放在 scratchpad，不入仓库）：

| 变体 | 提交 | 工作集 | 私有工作集 | 线程 | 模块 |
| --- | --- | --- | --- | --- | --- |
| 空窗口消息循环 | 1.44MB | 6.4MB | 1.04MB | 4 | 23 |
| + 托盘（20 项菜单、32px 图标） | 2.94MB | 12.4MB | 2.29MB | 9 | 46 |
| + 12 个全局热键 | 2.93MB | 12.4MB | 2.29MB | 9 | 46 |
| 同上 + `EmptyWorkingSet` | 2.95MB | **1.24MB** | **0.54MB** | 9 | 47 |

注意：①这台机器上有深信服 SNAC 往每个进程注入两个 DLL（`printerhook64`、`PrinterAdapter64`），算在上面的模块数和内存里，是环境因素；②`EmptyWorkingSet` 只降工作集，**提交不变**，数字好看但不是省了内存，文档与验收里两个口径分开写；③tray-icon 的 `msctf.dll`（TSF）在空闲时被加载，`ImmDisableIME(0)` 后**不再加载**；但弹出托盘菜单后，几次试验里 `msctf.dll` 有时还是出现，样本小且没把搜狗设为当前输入法，**不能下结论**，列入阶段 0。三次试验里搜狗的 DLL 都没进宿主。

**真宿主估计**（估算）：提交 3.2~4.5MB（探针 2.9MB + 会话管道线程与缓冲 + 快照与菜单图标 + serde_json + 鼠标钩子线程）；`EmptyWorkingSet` 后工作集 1.5~3MB，私有工作集 <1.5MB。对比现状 56.8MB（空闲）/ 134~136MB（截图后）。**托盘态验收目标：宿主私有提交 ≤ 6MB，且整套进程只有宿主一个**（`tasklist` 里没有 UI、没有任何 worker）。

做法：

- 无 GPUI、无 D3D、无 IME 上下文；**宿主线程创建窗口前调 `ImmDisableIME(0)`**（宿主没有文字输入，没有副作用）。
- 无异步运行时、无 `tracing-subscriber`、无 fluent。依赖只有 §3 那几项。
- 空闲 trim：启动完成 +5s、每次 UI 退出 +2s、每次托盘菜单关闭 +2s 调一次 `EmptyWorkingSet`。宿主要做的事极少，重新缺页的代价可以忽略。
- 菜单图标快照用 16px RGBA（≤1KB/个），菜单替换时旧图标句柄立即释放。
- 线程：先复用现有后端（托盘线程、热键线程、管道线程、钩子线程约 4 条）。合成单线程消息循环（`MsgWaitForMultipleObjects` 同时等消息、管道、进程句柄）是阶段 6 的可选项，量到提交超标再做。
- 后端先复用 `tray-icon` / `global-hotkey`（已批准、行为已验证、探针数据就是它们）。自写 Win32 精简版是阶段 6 的可选项，不在主线。

## 8. 迁移计划

每阶段一个或几个独立提交，可单独回退。**所有阶段的通用禁止项**：不改 Qt/C++ 旧目录；不升级 `gpui`；不新增第三方依赖（需要先问）；不往上游数据目录写；不用强杀当正常退出路径；不留 `todo!()`；每个新函数/模块有单测，注释按仓库约定；只跑覆盖改动的测试，不跑 `cargo test --workspace`。

### 阶段 0：量化与验证（不改产品代码）

- **范围**：在 `spikes/`（或 scratchpad）做探针，不进产品 crate。①拆解 UI 冷启动：`CreateProcess→main`、`main→GPUI 就绪`、采集、开窗到首帧；②宿主探针加上真管道 + 12 热键 + 鼠标钩子，量内存与线程；③搜狗设为当前输入法，右键托盘菜单若干次，看宿主模块（`ImmDisableIME` 有无作用）；④焦点矩阵：热键 / 托盘左键 / 托盘菜单 / CLI 四种触发，宿主拉起的 UI 覆盖窗能否拿到键盘焦点（`AllowSetForegroundWindow`）；⑤剪贴板：复制后 UI 立即退出，另一进程可粘贴；⑥UI 退出耗时；⑦开机后首次拉起 vs 页缓存热；⑧确定宿主导入表白名单。
- **验收**：一张表，列出 §6 每一步实测毫秒（3 次取中位数，记录机器、GPU、驱动、刷新率）、宿主内存（提交与私有工作集分开）、焦点矩阵结果、搜狗结果；决定阶段 5 里"宿主侧采集"做不做。
- **禁止**：不拿 Intel 的数当 NVIDIA 的；不拿 `EmptyWorkingSet` 后工作集当"内存降了"；不在上游数据目录读写。

### 阶段 1：抽取与协议（行为零变化）

- **范围**：新增 `snow-data-paths`、`snow-host-protocol`、`snow-shell-core`；`snow-ui-shell` 改为重导出（对外路径不变）；`snow-shot` 里把热键注册和托盘菜单拆成"先产出计划/快照（纯函数）、再注册/显示"（`hotkey_plan`、`host_spec_from_document`）；`workspace-guard` 加依赖闭包规则（此时对象还只是 `snow-host-protocol`）。
- **验收**：`cargo test -p workspace-guard -p snow-host-protocol -p snow-shell-core -p snow-data-paths -p snow-ui-shell`；`snow-shot` 里热键、托盘、`fullscreen_gate` 相关测试通过；快照对默认配置序列化再反序列化一致，热键计划与旧 `register_all_hotkeys_gated` 的注册集合逐项一致（对照测试）；fmt、clippy 通过；i18n 两条门禁通过。运行时行为与改前一致。
- **禁止**：不新增 bin；不改 IPC；`snow-shell-core` 不得依赖 `snow-app-core`、`snow-capability`、gpui。
- **状态：已完成（commit 5953974a，2026-10-12）**。验收：`cargo test -p workspace-guard -p snow-host-protocol -p snow-shell-core -p snow-data-paths -p snow-ui-shell -p snow-config -p snow-app-core` 全绿；`snow-shot` 里 `hotkey_plan` / `host_spec` / `app_runtime` / `fullscreen_gate` / `tray_config` / `quick_actions` / `mouse_gesture` 相关 79 项通过；i18n 两条门禁通过；clippy（workspace、all-targets、`-D warnings`）通过。运行时行为未改，无新增第三方依赖。
- **阶段 1 实施偏差与澄清**：
  1. `snow-shell-core` 的动作负载用**泛型 `A`** 而不是“不透明 id 字符串”：UI 进程仍是 `AppCommand`，宿主将来用 `String`。`snow-ui-shell` 里 `HotkeyService` / `TrayService` 是套了能力表校验与 `AppCommand` 适配的薄包装，`HotkeyBinding` / `TrayAction` / `TrayMenuEntry` / `TraySpec` 是类型别名，调用方代码零改动。
  2. `ShellError::Unsupported.capability` 由 `Capability` 改为 `&'static str`（能力标识），因为 core 不能依赖 `snow-capability`；`ui-shell` 里有测试守住 core 复刻的标识与原因 key 与能力表一致。非 Windows 降级桩的弹出菜单主题错误改报 `Tray`（原为 `OverlayClickThrough`），仅影响日志文案。
  3. 消息循环、唤醒器、弹出菜单主题（`native`）也移入 core；`service_thread`、`error`、`hotkey`、`tray` 如方案所列。单实例 / 命名管道 / 鼠标钩子本来就在 `snow-platform`（无 gpui），无需抽取。
  4. `snow-host-protocol` 对 §4 的取舍：`hotkeys_apply{rev}` 按 §4 设置页流程解释为 **UI→宿主** 请求，应答为宿主→UI 的 `hotkeys_result{rev, results[]}`；新增 `check_hello`、令牌格式、`FrameReader`（字节流切行、超长行丢弃）、`HostSeq` / `UiSeq` / `ExitDecision`（序号与退出仲裁）、§4 的超时与熔断常量。不含命令队列与状态机（阶段 2）。图标像素的 base64 自带最小实现，保持 crate 只依赖 serde / serde_json。
  5. `snow-shell-core::hotkey` 新增 `Hotkey::win_vk()` / `Modifiers::win_bits()`（Win32 虚拟键码与 `MOD_*` 位），供快照里的 `mods` / `vk`。
  6. `workspace-guard` 的依赖闭包规则：纯读 `Cargo.lock`，必需根 `snow-host-protocol`、`snow-shell-core`、`snow-data-paths`，可选根 `snow-tray-host`（阶段 2 建出后自动生效）。黑名单在方案 §3 的清单上多加了 `snow-capability`。**`Cargo.lock` 不区分平台**：`tray-icon` / `muda` 在 Linux 上会拉进 `gtk` → `glib` → `futures-executor`，所以遍历时对 `gtk`、`libappindicator` 两个只在非 Windows 编译的入口不继续深入（`NON_WINDOWS_ONLY`，附测试）；新增条目前须确认它确实只被非 Windows 目标依赖。导入表闸门（第三层）仍属阶段 2。
  7. `snow-shot` 新增 `hotkey_plan.rs`（`hotkey_plan` 及其三个子计划，纯函数）与 `host_spec.rs`（`host_spec_from_document` + `HostSpecInputs`、动作 id 映射 `command_id` / `command_from_id`）。注册路径 `register_all_hotkeys_gated` 改为“出计划 → `register_plan_with` 注册”，对照测试用独立手写的旧注册顺序作金标准逐项比对（默认配置、多处绑定、暂停、听写三种触发模式）。`host_spec` 阶段 1 不接入运行时，模块上用 `cfg_attr(not(test), allow(dead_code))` 压住未使用告警，阶段 2 接入后去掉。
  8. 快照里 `keep_alive` 目前只含 `mcp`、`ocr_hot_start`；§10 第 4 条“绑定了语音热键则保活”留到阶段 3。`fullscreen_gate` 仍在 `snow-shot`，宿主侧闸门随阶段 2 实现。
  9. 工作区基线：本阶段在 worktree 分支上由旧提交快进到 7400567d 后开始。
- **留给阶段 2**：`snow-tray-host` bin、`snow-shot-ui` 改名与 `--ui` / `--resident`、会话管道（复用 `line_pipe`）与宿主状态机、命令队列（64 条 / 30s / 手势 update 只留最新）、UI 侧会话客户端与活动跟踪、宿主线程创建窗口前 `ImmDisableIME(0)`（§12.9 第 1 条）及其回归测试、`--cmd` 从属实例先对宿主 PID 调 `AllowSetForegroundWindow`（§12.9 第 4 条）、导入表闸门脚本、宿主侧前台全屏闸门与暂停开关、`host/spec.json` 的原子写入与 `rev` 维护、去掉 `host_spec` 的 `allow(dead_code)`。

### 阶段 2：宿主 + UI 双进程跑通（默认仍是单进程）

- **范围**：新增 `snow-tray-host`（bin `snow-shot`）；`snow-shot` bin 改名 `snow-shot-ui` 并实现 `--ui`、`--resident`；会话管道、状态机、序号握手、命令队列、崩溃熔断；UI 侧会话客户端与活动跟踪；切换开关见 §9。先打通：托盘、截图热键、`--cmd`、设置窗、退出。
- **验收**：功能回归（分模式 `split`）：截图热键 / 托盘左中右键 / `--cmd` 七个命令 / 设置里改热键（含冲突回滚）/ 暂停热键 / 前台全屏闸门 / 退出；竞态注入测试（`exit_request` 与新命令交错、UI 启动期间连发命令、UI 被杀）全部确定性通过；宿主 `cargo tree` 与导入表闸门通过；托盘态宿主私有提交 ≤ 6MB；冷拉起热键→首帧 ≤ 1.14s（先达现状冷启动线）。
- **禁止**：不切默认；不在宿主里读 config.json；宿主不得出现定时轮询 UI 状态的逻辑。
- **状态：离屏部分完成（2026-10-12），真屏硬验收待执行**。真屏验收（§10 第 14 条、本阶段验收里需要真屏的各项）已写成可复跑的脚本 `scripts/verify-tray-host-split.ps1`，由主会话在桌面解锁后执行，结果回填本节。
- **真屏验收（2026-10-13）**：环境：i5-13500 / UHD 770（驱动 31.0.101.4953）/ 双屏 2560×1440 / Win10 19045 / 带端点安全软件；release（LTO）构建含未提交的 vendor `GpuReleaseGuard` 补丁；`verify-tray-host-split.ps1`（Windows PowerShell 5.1）。逐场景结果见 `build\split-accept\<时间戳>\results.json`（本节以 `20261013-091941`、`092533`、`093710` 三次完整 `all` 为准，Rounds=5）。**宿主私有提交 2.1MB（2.08~2.13）**，弹菜单后 2.3MB（2.32~2.39），验收线 6MB；弹菜单（含前台、搜狗在位）后宿主无 `*.ime`；**热键→首帧（两屏像素都变）中位 1016 / 831 / 863ms**，验收线 1.14s（首轮冷启动偏慢，各轮约 0.8~2.4s）；UI 拿到前台与键盘焦点 5/5 轮；覆盖窗关闭后 UI 中位 0.29~0.39s 退出；托盘菜单项（显示主界面）拉起 UI 拿前台与键盘焦点通过；`--cmd` 七个命令（含 quit）三次全过；改热键冲突回滚 / 新热键生效三次全过；杀 UI 后宿主存活、重新拉起、3 次连杀熔断三次全过；`admin` 场景 1 次通过但退化（本机 `EnableLUA=0`、当前用户本就是管理员，混合权限的 `--cmd` 没法在这台机器上验）。**暂停 / 前台全屏闸门不稳定**：三次完整运行里 1 次全过、2 次闸门部分失败，另 3 次单场景复跑 2 过 1 败；宿主日志里没有错误，失败轮的现象是托盘菜单点击没有落到预期的那一项（原因判断为 UI Automation 点菜单不稳，非产品问题，但未证实）。验收脚本这次修了若干自身问题（采样与进程枚举过慢、通知区图标查找、托盘菜单项文案、日志文件共享读、数据目录隔离等），首帧数字是修完之后的。
- **交付物**：
  - `snow-tray-host`（新 crate，bin 产物 `snow-shot.exe` = 宿主）：`queue`（命令队列：64 条 / 30s / 手势 update 只留最新）、`breaker`（崩溃熔断：60s 内 3 次、退避 5 分钟）、`machine`（UI 进程状态机 `Absent → Starting → Running → Exiting → Absent` + `Backoff`：10s 握手超时、ack 1.5s 重发一次再放弃、`exit_request` 序号仲裁、退出 10s 超时才强制结束、按会话代数丢弃过期事件）、`focus`（前台授权决策）、`host`（宿主核心：状态机 + 快照 + 暂停开关 + 前台全屏闸门 + 托盘 / 热键的应用，系统调用全在 `Platform` 特征后面，可用假平台离屏测试）、`view` / `convert` / `ipc_map` / `cli`、`harden`（进程加固）、`host_log`（写 `logs/host.log` 的最小 `tracing` 订阅者，INFO 及以上，256KB 滚动为 `host.log.1`，不引入 `tracing-subscriber`）、`win` / `app`（真实平台与入口）。主线程没有 Win32 消息循环：托盘 / 热键 / 管道读 / 进程等待线程只往一个通道投事件，主线程 `recv_timeout(下一个截止时刻)`，空闲时无限期阻塞（无轮询）。
  - UI 进程：`snow-shot` 包的 bin 改名 `snow-shot-ui`；`--ui --session <名> --token <令牌>`（宿主拉起，连会话管道、不持托盘 / 热键、所有窗口关闭且无活动后请求退出、宿主同意后立即退出）、`--resident`（单进程常驻，**无参数默认也是它**）、`--headless emit-spec`（不初始化 GPUI，按当前配置生成快照）。UI 侧新增 `host_link`（会话客户端、序号去重、退出请求单飞、设置页改热键的待回报登记）、`host_spec_sync`（`host/spec.json` 原子写与 `rev` 维护）、`app_runtime::host_session`（接线：宿主动作 → `UiEvent` / `AppCommand`、快照同步、热键应用与回滚、活动上报与退出判定、宿主崩溃后拉起新宿主）。
  - `snow-platform`：`single_instance::session_pipe`（双工会话管道，两端都能随时主动写；复用单实例的当前用户 ACL、重叠 IO）；崩溃转储的 `MiniDumpWriteDump` 改运行时加载 `dbghelp.dll`、前台全屏探测的 `DwmGetWindowAttribute` 改运行时加载 `dwmapi.dll`（宿主导入表不能有它们；UI 进程也因此少两个启动期导入）。
  - `snow-host-protocol`：`launch`（启动开关与会话参数校验）、`spec_file`（读快照，IO 错误短暂重试）、`HostMessage::Command.origin`（可选）、`HostStrings` 两个提示文案字段（带默认值）、动作 id 与配置键常量；`snow-data-paths`：单实例 / UI / 会话管道标识、`host/spec.json` 与 `host.log` 路径；`snow-ui-shell`：`window_count`、`on_window_closed`、`MainThreadInbox::is_empty`。
  - 守卫：`workspace-guard` 的依赖闭包规则把 `snow-tray-host` 升为必需根；新增 `imports` 模块与 `check-host-imports` 命令（解析 PE 静态 + 延迟导入表，不依赖 dumpbin / MSVC 环境），脚本 `scripts/check-host-imports.ps1 [-Build]`。
  - 脚本：`scripts/verify-tray-host-split.ps1`（真屏验收，见下）；`scripts/verify-snow-shot-recording.ps1`、`tools/self_acceptance_verify.ps1` 的 exe 名跟随改为 `snow-shot-ui.exe`（它们要的是完整应用）。
- **产物名与过渡**：宿主 `snow-shot.exe`、UI `snow-shot-ui.exe`，同目录（宿主按自己的目录找 UI，`SNOW_UI_EXE` 可覆盖，验收脚本与测试用）。**默认仍是单进程**：`snow-shot-ui.exe` 不带参数 = 现状的单进程常驻；双进程要显式启动宿主 `snow-shot.exe`（宿主按需拉起 `snow-shot-ui.exe --ui ...`）。`snow-shot.exe --resident` 转调 `snow-shot-ui.exe --resident` 后退出（给习惯用旧入口的人一个回退）。宿主与 `--resident` 的 UI 共用单实例互斥体，不会同时跑；`--cmd` 两种模式都走同一条控制管道，命令名不变。开机自启：`--resident` 指向 UI exe（现状），`--ui` 模式指向同目录的宿主（阶段 4 切默认时统一）。构建要同时带两个包：`cargo build -p snow-tray-host -p snow-shot`；**注意**：`build\cargo\<profile>\snow-shot.exe` 从这一阶段起是宿主，完整应用是 `snow-shot-ui.exe`。
- **离屏验收结果**（2026-10-12，release / debug 见各项）：
  - 竞态注入测试全部确定性通过（`snow-tray-host` 的 `machine` 测试，时间、进程、管道均由测试注入，不起真进程）：`exit_request` 与新命令交错（拒绝后处理完再请求才同意）、同意退出后来的命令入队并在旧进程退出后按新序号投递、UI 启动期间连发命令保序冲出、语音按下 / 松开保序、握手失败五种（令牌 / PID / 协议版本 / 非 hello / 管道客户端 PID 不符）、握手 10s 超时、ack 超时重发一次再放弃、UI 被杀不重放且不双计、崩溃前有贴图自动恢复、熔断（60s 内 3 次进入退避、退避内丢命令并提示、5 分钟后恢复；窗口外的崩溃不计数）、退出各阶段（运行中 / 启动中 / 退避中 / 退出超时只强制结束一次）、队列 64 条挤旧留新 / 30s 过期 / 手势 update 合并。宿主核心用假平台覆盖：快照缺失 / 损坏退回内置菜单且只请求一次生成、快照增量应用（菜单 / 提示原地换，图标变了才重建托盘）、前台全屏闸门（只拦热键，开关热键豁免）、暂停 / 恢复（含恢复失败原因）、闸门切换经 `set_config` 落盘、设置页改热键的逐配置键回报与回滚、退出 / 重启流程、工作集修剪调度。
  - `cargo clippy --workspace --all-targets -- -D warnings` 通过；`cargo test` 覆盖 crate 通过：`snow-tray-host`（92 项单测 + 4 项 exe 集成测试，含真起假 UI 进程 / 真管道的平台测试与“新进程里加固确实生效”的回归测试）、`snow-host-protocol`（35 + 19 doc）、`snow-data-paths`（14 + 8 doc）、`snow-shell-core`（23）、`workspace-guard`（24 + 6 doc，含宿主依赖闭包对真实 `Cargo.lock` 零违规）、`snow-platform`（`session_pipe` / `line_pipe` / `single_instance` / `crash` / `window_rect` 等 75 项）、`snow-ui-shell`、`snow-app-core`、`snow-config`、`snow-i18n`；`snow-shot` 1193 项通过，另有 10 项 `stt_download` 失败——本机 `tar.exe` 调不起 bzip2（`tar.exe: Can't launch external program: bzip2`），与本次改动无关。i18n 两条门禁（`check`、`extract --strict-refs`）通过。
  - **宿主 release 产物**（`cargo build --release -p snow-tray-host`，lto fat、panic=abort）：`snow-shot.exe` **1,033,216 字节**（约 1.0MB，方案估 0.5~1MB）。静态导入 18 项：`user32 kernel32 gdi32 comctl32 advapi32 imm32 bcryptprimitives ntdll shell32 oleaut32 api-ms-win-core-synch-l1-2-0 vcruntime140 api-ms-win-crt-{runtime,stdio,math,locale,heap,string}-l1-1-0`，延迟导入 0 项；导入表闸门（`scripts/check-host-imports.ps1 -Build`）通过。没有 `d3d*` / `dxgi` / `dwmapi` / `dwrite` / `uiautomationcore` / `windowscodecs` / `bcrypt` / `dbghelp`。宿主私有提交的实测在真屏验收里（这台机器上现在跑着用户的 debug 实例、屏幕锁着，按约定没有跑真实进程实验）。
  - **UI release 产物**（`cargo build --release -p snow-shot`）：`snow-shot-ui.exe` 26,023,936 字节，release 构建通过；崩溃转储与前台探测改运行时加载后，它的导入表里已无 `dbghelp`（`dwmapi` 仍在，来自 GPUI）。
- **真屏验收脚本**：`scripts\verify-tray-host-split.ps1 [-Scenario all|idle|ime|hotkey|tray-focus|cmd|settings-hotkey|pause-gate|crash|admin] [-Rounds 3]`，用 Windows PowerShell 5.1 运行；前提（脚本开头逐项检查，不满足就中止）：桌面已解锁、本机没有别的 Cisox 实例（含旧的 debug `snow-shot.exe`，脚本只报告不替你结束）、通知区能直接看到 Cisox 图标（托盘类场景靠 UI Automation 找图标，找不到报 SKIP）、验收期间不碰键鼠不播视频。数据隔离：脚本把宿主 / UI exe 复制到临时目录并写 `__data_directory` 标记，数据根落在该目录，不碰真实数据目录。场景与本阶段验收项的对应：`idle`（托盘态进程只有宿主、私有提交 ≤ 6MB，提交与工作集分开记）、`ime`（弹菜单后宿主无 `*.ime` 模块、提交仍 ≤ 6MB）、`hotkey`（冷拉起热键→首帧中位数 ≤ 1.14s、UI 拿到前台与键盘焦点、关覆盖窗后 UI 3s 内消失）、`tray-focus`（托盘菜单项拉起 UI 后拿到前台与键盘焦点，§12.3 未测的一项）、`cmd`（`--cmd` 七个命令）、`settings-hotkey`（改热键冲突回滚 / 成功生效）、`pause-gate`（暂停开关、前台全屏闸门）、`crash`（杀 UI 后宿主仍在、重新拉起、3 次连杀熔断）、`admin`（宿主以管理员运行：热键 / 会话管道 / 焦点正常；非管理员 shell 的 `--cmd` 发给管理员宿主）。结果写 `build\split-accept\<时间戳>\results.json` 并附机器信息（CPU、显卡、驱动、刷新率）与日志副本。
- **阶段 2 实施偏差与澄清**：
  1. **宿主闭包里仍有 `windows` crate**：`snow-platform`（单实例、崩溃转储）与 `snow-shell-core`（消息循环）本来就用它，宿主按文档复用它们，所以导入表里多一个 `oleaut32.dll`（`windows::core::Error` 的 `GetErrorInfo`），已加入白名单并写明理由；整体改写成 `windows-sys` 才能去掉，研究文档 §3.3 实测只差约 0.1MB 提交，留给阶段 6（有数据才做）。宿主自己的 Win32 调用（进程加固、拉起 / 等待 / 结束 UI、前台授权、`EmptyWorkingSet`、`ProcessPrng`）只用 `windows-sys`（锁里已有的 0.61.2，无新增第三方包）。阶段 0 白名单里的 `dwmapi`（前台全屏探测）与 `bcrypt`（随机数）已分别改成运行时加载、改用 `ProcessPrng`（`bcryptprimitives.dll`，Rust 标准库本来就导入），所以不在白名单里；`dbghelp` 同样改成崩溃时才加载。
  2. **动态 CRT**：宿主导入 `vcruntime140` 与 `api-ms-win-crt-*`，没有用静态 CRT——`+crt-static` 只能通过全局 `rustflags` 设置，会连带 UI exe；研究文档 §3.2 实测静态 / 动态 CRT 的提交无差别。白名单已含 `vcruntime140`。
  3. **宿主不装鼠标手势钩子**（§2 表里写了，但手势流在阶段 3 的范围）：双进程模式下鼠标手势暂不可用（`--ui` 不启动手势、宿主不装钩子）；协议里 `Gesture` 消息与队列合并规则已就绪并有测试。
  4. **托盘菜单关闭后的工作集修剪没做**：`tray-icon` 不暴露“菜单关闭”事件。现做启动 +5s、每次 UI 退出 +2s 各一次 `EmptyWorkingSet`。
  5. **宿主切换前台全屏闸门时 UI 不在**：不是 `--headless set-config`，而是用普通会话拉起一个 UI 写完配置后空闲退出（`--headless set-config` 是阶段 3）。`pins_to_restore` / `update_due_at` 快照里暂写 0 / `None`（登录恢复贴图、更新检查 headless 是阶段 3）；宿主已支持“快照里 `pins_to_restore > 0` 就拉起 UI 恢复贴图”和“崩溃前上报过贴图就自动恢复”。`--ui` 启动时不推 `RestorePins` / `AutoUpdateCheck`（这两件事归宿主按快照触发 / 阶段 3），只保留 `OcrWarmUp`（内部检查开关）。
  6. **活动守卫只做了最小集**：窗口、采集 / 直接截图 / 延迟截图进行中、语音转文字进行中、用户显式要求常驻（MCP 开关 / OCR 预热）；下载、历史 flush、worker 结果消费者等是阶段 3。§10 第 4 条“绑定了语音热键就保活”留阶段 3。
  7. **快照校验推迟到 UI 第一次空闲且快照可能过期**：UI 每次启动都重新生成快照要 ~235ms（i18n + 图标光栅化），不能放在热键到首帧的路径上，也不值得每次都做；改为首次空闲时按文件时间判断（快照比 `config.json` 或 UI exe 旧就重新生成），配置变化时照常即时重写。代价：系统语言 / 主题在 UI 没跑过的情况下变化，托盘文案要等下一次配置变化或升级才刷新（§2 已接受）。
  8. **协议小扩展（向前兼容）**：`HostMessage::Command` 带可选 `origin`（hotkey / tray / ipc，只用于 UI 侧日志与来源标签，旧宿主不带也行）；`HostStrings` 新增 `notice_ui_start_failed` / `notice_ui_crash_loop`（带默认值，空串时宿主用内置英文兜底），对应两条 i18n 文案；新增动作 id `restart` / `scroll_capture` 等常量。**宿主自己处理的动作**：退出、重启（先让 UI 优雅退出，再拉起延迟启动的新宿主）、暂停热键、前台全屏闸门。
  9. **焦点授权**：`--cmd` 从属实例对宿主 PID 的 `AllowSetForegroundWindow` 在现有单实例客户端（`send_frame`）里本来就有，阶段 2 无需新增；宿主在拉起 UI 时、以及把动作转给已在运行的 UI 时对 UI PID 授权（决策在 `focus` 模块，只有动作消息授权，恢复贴图 / 改配置 / 手势不抢焦点）。
  10. **UI 的“宿主崩溃”处理**：会话意外断开（不是 `shutdown` / `exit_ok` 之后）→ `shutdown_app`（贴图落盘等）→ 拉起同目录的 `snow-shot.exe` → 退出。
  11. **`--cmd` 在没有运行中实例时**：和旧入口一致，本进程成为主实例（宿主常驻），命令本身被忽略（旧行为，未改）。
- **留给阶段 3 的事项**：活动守卫全量接入（下载、历史 flush、worker 结果消费者、RAII 与“守卫归零”断言测试）与语音热键保活；贴图登录恢复（`pins_to_restore`）、更新检查 headless（`update_due_at`）、`--headless set-config`；鼠标手势流（宿主钩子 + PerMonitorV2 + 事件合并，UI 侧覆盖窗接手势事件）；通知 tooltip 全覆盖（语音状态等目前读写 `state.tray`，双进程下没有托盘对象，需要经 `tooltip` 消息）；MCP / OCR 预热保活下的描述符与宿主共存验证；托盘菜单关闭后的修剪（要么自写菜单循环要么放弃）；真屏验收结果（本阶段脚本）回填；全套功能回归清单。阶段 4 另需：CI 加导入表闸门、切默认、handoff 加进程模型一节、自启统一指向宿主。可选的后续优化（有数据才做）：宿主闭包去掉 `windows` crate 与 `oleaut32`（阶段 6）、UI 快退。

### 阶段 3：长生命周期功能与退出规则

- **范围**：§5 的活动守卫全量接入；贴图（登录恢复、崩溃恢复）、录屏、语音转文字（含按住说话保序）、翻译浮窗与选中文字翻译、鼠标手势流、通知 tooltip、更新检查 headless、`set_config` 的 headless 路径、`keep_alive`（MCP、OCR 预热）。
- **验收**：§5 表格每一行有一条测试或一条真机记录；全套功能回归清单（下）逐项过；MCP 开启时宿主 + UI 稳定共存、描述符有效；录屏进行中关掉所有其它窗口 UI 不退、停止后退；有贴图时重启宿主后贴图恢复。
- **禁止**：不得让守卫在异常路径泄漏（用 RAII，加"守卫归零"断言测试）；不得为了省事把守卫做成固定延时。
- **功能回归清单**：截图（单屏/多屏/混合 DPI）→复制/保存/贴图/OCR/翻译/长截图/录屏；各快捷动作（直接截图、延迟截图、选中贴图文件、恢复最近关闭）；输入框翻译与选中文字翻译；语音转文字（切换式、按住式）；历史、贴图管理；设置页全部热键项；表格/公式识别；配置归档导入导出；更新检查与提示；重启；退出后贴图落盘。
- **状态：离屏部分完成（2026-10-12），真屏硬验收待执行**（用户解锁桌面后由主会话跑 `scripts\verify-tray-host-split.ps1` 的新场景，结果回填本节）。默认仍是单进程（阶段 4 才切默认）。
- **真屏验收（2026-10-13）**：环境同阶段 2。`gesture`：默认三条手势绑定下宿主装钩子后私有提交 2.09~2.18MB（≤6MB），PerMonitorV2 生效；快速拖完 / UI 起来时拖动仍在进行两种都拉起 UI 并把框选复制到剪贴板、UI 随后退出，3 次完整运行里 2 次全过、1 次失败（那次运行发生在改脚本隔离数据目录之前，被上一个场景改过的配置污染，判断为脚本问题）；注意复制出的图尺寸与拖动框不一致（如 340×185、160×990，拖动框约 896×504），像是智能选区吸附到了光标下的元素，**未确认是预期行为还是手势 update 丢失，待查**。`pins-restore` 2/3 通过（失败那次同为数据目录污染，`pins_to_restore=2`）：贴图保活、杀 UI 后自动恢复、优雅退出后快照记数、宿主重启恢复贴图均通过。`stt-keepalive` 3/3：未绑定时 UI 随窗口关闭立即退出，绑定后保活且登录不预拉；保活 UI 私有提交 46.4~46.5MB。`mcp-keepalive` 3/3：登录即拉起 UI、描述符有效、MCP 管道可连并与宿主共存、杀 UI 后补拉、退出后描述符清理，宿主提交 2.15~2.17MB。`update-headless` 3/3：登录后 30s 拉起 `--headless update-check`（脚本改为按宿主日志时间戳判定，原先的进程轮询会漏掉瞬时进程）、不开 UI 会话、查完即退、到点时间推后约 24h、发现新版本进托盘悬停。
- **交付物**：
  - **活动守卫**（`snow-shot::activity`）：`ActivityTracker`（可跨线程共享的按原因计数器）+ RAII 的 `ActivityGuard`（持有即忙，丢弃含 panic 展开即放行，放行时唤醒主线程重新判断退出，否则“最后一个后台活动在后台线程结束”之后没有事件再触发判断）；`busy_reasons(BusyInputs, tracker)` 把状态型原因（窗口、采集 / 直接截图 / 延迟截图、语音转文字、录屏 / 长截图控制窗、MCP / OCR 预热 / 语音热键保活）与守卫型原因合成发给宿主的 `activity.busy_reasons`。`AppState::spawn_guarded` 在**线程创建之前**登记守卫（不存在“线程没跑起来就判空闲”的空窗）、线程创建失败时随闭包一并放行；接入点：OCR / 表格 / 公式识别、识别 + 翻译、输入框翻译、翻译页翻译、贴图 OCR、选中文件贴图、读取选中文字、模型 / 运行时 / 更新包下载（`spawn_download` 与更新下载）、手动 / 到点更新检查、OCR 预热线程；历史写入器（`HistoryRecorder::start_tracked`）每个排队任务持一个 `history_flush` 守卫，写完才放行。录屏 / 长截图 / 语音转文字 / 采集 / 延迟截图是状态型：控制窗在（或 `is_active`）就算忙，不另持守卫（控制窗被外部关掉而 `active` 不清时守卫会泄漏，状态型没有这个问题）。退出时若仍有守卫记 warn（泄漏诊断）。
  - **保活**：`host_spec::keep_alive_reasons` 新增 `dictation_hotkey`（语音触发模式放行的热键里有格式有效的绑定）；`HostFlags::needs_resident_ui()`：`mcp`、`ocr_hot_start` 要宿主在**登录 / 崩溃后 / 退避结束后**主动拉起 UI（这两项没有 UI 在场就没意义），`dictation_hotkey` 只是“拉起之后别退”、登录不预拉（托盘态内存优先）。宿主用占位动作 `keep_alive` 拉起（UI 收到即确认，什么都不做）。UI 侧 `busy_inputs` 同口径；配置变化时清缓存。
  - **登录恢复贴图 / 崩溃恢复**：快照 `pins_to_restore` = 激活分组里的贴图记录数（`PinShared::restorable_count`，与 `restore_all` 同口径）；UI 在贴图数变化时用 `SpecSync::patch_flags` 只改标志写进快照（不重新渲染菜单），`shutdown_app` 贴图落盘后再校准一次（宿主崩溃 / 重启重拉时据此恢复）；宿主登录读到 `pins_to_restore > 0` 就拉起 UI 恢复；崩溃（没跳闸）时有贴图自动恢复、要求常驻则补拉（`HostMachine::set_keep_alive`），熔断退避结束的那次 tick 也补拉；阈值仍是 60s / 3 次 / 5 分钟（§10 第 9 条，原常量与测试未改）。
  - **更新检查 headless**：快照 `update_due_at`（`net_settings::update_due_at`：手动 / 无地址为 `None`，没查过或时钟回拨为 `0`，否则上次检查 + 24h）；宿主在 `max(到点, 启动 + 30s)` 设一个截止时刻（不轮询）到点拉起 `snow-shot-ui.exe --headless update-check`，同一个到点时间只拉起一次；任务先写检查时间再查，查完用 `patch_flags` 把到点时间推后（不重新渲染菜单、不碰贴图仓储——它可能与运行中的 UI 并存），发现新版本把已本地化提示写到**标准输出第一行**，宿主读出后放进托盘悬停。
  - **`--headless set-config --key <键> --value <JSON>`**：`snow-shot::headless`（与 `emit-spec`、`update-check` 同处，`snow_host_protocol::launch::HeadlessJob` 统一解析 / 拼参数）。宿主改配置（前台全屏闸门）时 UI 不在 → 起这个任务写完即退，再重新生成完整快照（含贴图分组、恢复数、到点时间）；任务在跑期间宿主把新消息暂存（保序），避免新拉起的 UI 与它同时写配置；任务拉不起来退回阶段 2 的会话路径。取代阶段 2 “拉起普通 UI 会话写配置”的做法。
  - **鼠标手势**：宿主按快照 `mouse` 绑定装 `snow_platform::global_mouse`（键盘钩子常驻、鼠标钩子仅在修饰键匹配 / 手势进行中，沿用旧行为），空绑定即卸载；宿主进程 PerMonitorV2（阶段 2 的加固已有）所以坐标是虚拟桌面物理像素；钩子回调只做转换 + 8ms 节流（`GestureThrottle`，约 120Hz，`finish` 带最终位置）+ 投通道，不碰 IME / GPUI；`Gesture` 消息新增 `action`（只 `begin` 带），`update` 不等确认、不进重发表（UI 用 `accept_quiet` 登记序号不回 ack），`begin` 授权前台；UI 放弃手势（已有截图 / 录制在进行）发新消息 `cancel_gesture` 让宿主取消钩子里的手势；熔断退避里 `begin` 被丢弃时宿主同样取消。UI 侧把消息还原成 `DragEvent` 交给原有的 `on_mouse_gesture`（覆盖窗未就绪时的缓存补发逻辑原样复用）。
  - **tooltip**：`TooltipSink`（单进程改本进程托盘、双进程发 `tooltip` 消息、都没有则忽略）取代语音状态等对 `state.tray` 的直接读写；`show_notice` 走它。
  - **MCP / OCR 预热与宿主共存**：宿主登录就拉起 UI（`needs_resident_ui`），MCP 描述符路径内容不变（`<数据根>/mcp/descriptor.json`，由 UI 写），管道名（MCP `cisox-mcp`、宿主控制管道、会话管道前缀）互不相同；离屏测试 `mcp_service_coexists_with_host_pipes` 同时起 MCP 服务与宿主会话管道，验证描述符在 `mcp/` 下、关闭后清理。UI 崩溃后补拉的新 UI 重写描述符（新令牌 / 新一代）。
  - 脚本：`scripts/verify-tray-host-split.ps1` 新增 `gesture`、`pins-restore`、`stt-keepalive`、`mcp-keepalive`、`update-headless` 五个场景，`pause-gate` 的闸门落盘检查改为验证走 `--headless set-config`、没有拉起带窗口的 UI 会话。前提见脚本头部（`gesture` 用环境变量 `SNOW_HOST_ACCEPT_INJECTED_INPUT=1` 让钩子接受注入输入，仅验收用；`update-headless` 用 `file://` 本地清单、要等约 35 秒；各场景改配置都经产品自己的 `--headless set-config`，场景之间还原语音热键 / 预热 / 更新策略 / 贴图数据）。
- **§5 表格逐行对应**：采集 / 直接截图 / 延迟截图 / 录屏 / 长截图 / 语音转文字 / `keep_alive`（含语音热键）→ `activity::tests::every_state_row_of_the_table_blocks_exit`；下载 / 更新检查 / 历史写入 / OCR·翻译 job / 贴图 job / 选中文字 / OCR 预热 → `every_guard_row_of_the_table_blocks_exit_until_released` + `history_store::tracked_recorder_holds_a_guard_until_the_write_finishes`；异常路径不泄漏（panic、提前返回、线程创建失败、锁中毒、并发）与“归零断言”（`assert_idle`）→ `activity::tests` 其余 7 项。
- **阶段 3 实施偏差与澄清**：
  1. **守卫型 / 状态型两类**：录屏、长截图、语音转文字、采集、延迟截图按状态判定而不是 RAII（它们的“存活”本来就由窗口 / 引擎状态表示，见上）；文档 §5 的“守卫兜底”在这些行等价于这条状态判定。
  2. **语音热键保活不在登录时预拉 UI**（§10 第 4 条只说“绑定了就保活”）：预拉会让任何绑了语音热键的用户托盘态都带一个 UI，违背“托盘态只管内存”的目标；现在是“第一次按下冷拉起一次，之后不退”。要改成登录预拉，只需把 `dictation_hotkey` 加进 `needs_resident_ui`。
  3. **宿主改配置在 UI 不在时用无窗口任务**，且任务期间暂存新消息：这是对“配置唯一写者”的保序保护，没有在文档里要求过，但没有它会出现 UI 与无窗口任务同时写 `config.json`。
  4. **`update-check` 回报提示用标准输出**（宿主没有 i18n，文案要 UI 侧本地化；管道 / 临时文件都比读一行 stdout 重）。上限 4KB。
  5. **默认配置自带三条手势绑定**，所以宿主在默认配置下登录就装键盘钩子线程（一个线程 + 小栈）；宿主私有提交是否仍 ≤ 6MB 待真屏（`gesture` / `idle` 场景记数）。
  6. **更新检查登录后延后 30 秒**（常量 `UPDATE_CHECK_START_DELAY`），避开登录时磁盘与网络高峰；文档原文是“登录后到点就拉起”。
  7. **托盘菜单关闭后修剪**仍没做（`tray-icon` 不暴露事件，阶段 2 已说明），建议直接放弃，保持启动 +5s / UI 退出 +2s 两次。
  8. **没改的**：`--ui` 启动时依旧不推 `RestorePins` / `AutoUpdateCheck`（归宿主）；单进程 `--resident` 路径行为不变（手势、托盘、热键仍在本进程）。
- **离屏验收结果**（2026-10-12）：`cargo clippy --workspace --all-targets -- -D warnings` 通过；`cargo test` 通过：`workspace-guard`（24 + 6 doc，宿主依赖闭包零违规）、`snow-host-protocol`（38 + 23 doc）、`snow-tray-host`（124 单测 + 4 项 exe 集成测试，含真起假 UI 的无窗口任务 / 提示读取 / 钩子不装测试）、`snow-shot`（1231 项，12 项 ignored）；i18n 两条门禁通过（无新增文案）；`scripts/check-host-imports.ps1 -Build` 通过。
- **宿主 release 产物**（`cargo build --release -p snow-tray-host`）：`snow-shot.exe` **1,102,336 字节**（阶段 2 为 1,033,216，+69,120 字节，约 +67KB：手势钩子接线、无窗口任务、更新到点调度）；静态导入仍是同样的 18 项、延迟导入 0 项（鼠标 / 键盘钩子用的 `SetWindowsHookExW` 等都在 `user32`，没有新增 DLL）；宿主闭包没有新增 crate。宿主私有提交的实测在真屏验收里（`idle` / `gesture` / `stt-keepalive` / `mcp-keepalive` 场景都记数，默认配置带手势绑定所以要重测 ≤ 6MB）。
- **留给阶段 4 的事项**：真屏验收结果回填（含手势 / 贴图恢复 / 保活 / MCP / 更新检查五个新场景和 `idle` 重测）；默认切 `split` 时统一自启指向宿主、CI 加导入表闸门；`dictation_hotkey` 是否登录预拉的拍板；手势在“宿主刚拉起 UI、覆盖窗还没就绪”这段的体感（队列合并保证不丢，延迟取决于冷启动）；全套功能回归清单逐项过（本阶段只覆盖了协议 / 状态机 / 守卫的离屏部分）；handoff 加进程模型一节。

### 阶段 4：切默认与收口

- **范围**（切默认部分待真屏验收，见下状态）：默认 `split`；自启指向宿主；文档（handoff、迁移方案、审计表 C03、README 索引、AGENTS.md 的 crate 清单）同步；CI 加导入表闸门；打包脚本出现后纳入两个 exe。
- **验收（量化）**：关闭全部窗口 10s 后，进程只剩宿主，私有提交 ≤ 6MB（对比现状截图后 134~136MB）；一次截图→关闭，UI 与所有 worker 在 3s 内全部消失；冷拉起热键→首帧 ≤ 0.9s；连续 50 次热键截图无丢命令、无两个 UI 并存；全套回归通过；`cargo test` 覆盖 crate 的相关测试、`workspace-guard`、i18n 门禁、fmt、clippy 通过。
- **禁止**：不删 `--resident` 路径（至少保留到用户另行拍板）；不在文档里把估算写成实测。
- **状态：离屏部分完成（CI 闸门、文档），切默认待真屏验收（2026-10-12）**。默认仍是单进程；下面的验收（量化）与“切默认”都要等主会话在桌面解锁后跑完 `scripts\verify-tray-host-split.ps1` 全场景并回填阶段 2、3 再决定。
- **离屏部分交付物**：
  - **CI 导入表闸门**：`.github/workflows/snow-shot-rs-ci.yml` 新增仅 Windows 的 `host-imports` 任务（独立任务，三平台矩阵的 `ci` 任务不受影响），跑 `scripts/check-host-imports.ps1 -Build -WithUi`：宿主与 UI 两个包用同一条 `cargo build --release -p snow-tray-host -p snow-shot` 构建（与真实打包一致，也能发现两包同构建时的特性统一问题），再用 `workspace-guard` 的 `check-host-imports` 解析宿主 PE 导入表（静态 + 延迟）对白名单。脚本新增 `-WithUi`，产物目录改为尊重 `CARGO_TARGET_DIR`（本机设了它，旧写法固定读 `build\cargo` 会找不到 exe）；脚本加 UTF-8 BOM（PowerShell 5.1 不带 BOM 会把 UTF-8 按 ANSI 读）。CI 路径过滤加入 `scripts/check-host-imports.ps1`。依赖闭包规则继续走 `ci` 任务里的 `cargo test --workspace`（含 `workspace-guard`）。
  - **旧 bin 名查漏**：CI 里没有引用旧 bin 名 `snow-shot`（UI 现为 `snow-shot-ui`）的步骤；其它 workflow（release / winget / homebrew）是 Qt 版发布流水线，与 Rust 版无关；`scripts/` 下 Rust 版相关脚本里，`verify-snow-shot-recording.ps1`、`tools/self_acceptance_verify.ps1` 阶段 2 已改指 `snow-shot-ui.exe`，其余无引用。Rust 版打包脚本尚不存在（见下清单第 7 项）。
  - **文档**：`docs/cisox-progress-handoff.md` 新增 §6.2「进程模型」（两个 exe 职责、启动开关、构建、数据与日志位置、验收脚本、现状与切默认前提）；`snow-shot-rs/README.md` 新增“两个 exe 与运行方式”；`AGENTS.md` 构建说明补 CI 闸门、`-WithUi`、进程模型指针与“默认仍单进程”；`docs/README.md` 索引同步。
- **离屏验收结果**（2026-10-12）：`cargo clippy --workspace --all-targets -- -D warnings` 通过；`cargo test -p workspace-guard` 通过（24 + 6 doc）；`scripts/check-host-imports.ps1 -Build` 通过（宿主 release `snow-shot.exe` 1,102,336 字节，静态导入 18 项、延迟 0 项，与阶段 3 记录一致）；脚本用 Windows PowerShell 5.1 解析 0 错误，UTF-8 BOM、LF；CI YAML 用 PyYAML `safe_load` 校验可解析且三个任务齐全。**未实测**：`-WithUi`（UI 的 fat LTO release 构建很慢，只在 CI 上跑）与 CI 任务本身（本机没有 runner）。本阶段没有改任何 Rust 代码。
- **切默认改动位置清单**（本节只列、**不改**，真屏验收通过后一次完成；行号以 2026-10-12 的 `0cb332f4` 为准）：
  1. **无参数默认入口**（`snow-shot/src/main.rs`）：`parse_cli` 末尾 `else { RunMode::Resident }`（约 L251）改成新模式“转去启动同目录宿主 `snow-shot.exe` 后退出”（§3“直接双击 UI exe”那一行；带 `--cmd` 时一并转给宿主）；`--resident` 要变成显式开关（现在是默认的同义词）。同处要改：`RunMode` 文档注释与 `RunMode::Resident` 的“默认”字样（约 L169）、模块头注释（L4 “也是阶段 2 的默认”）、`parse_cli` 文档与 `main` 里的用法提示（`eprintln!` 那行）、单测 `cli_defaults_to_resident`（约 L597）与 `cli_modes_are_exclusive` 里相关断言。
  2. **自启注册指向**（`snow-shot/src/system_settings.rs`）：`AUTO_START_HOST` 初值 `false` 改 `true`（或去掉开关，永远指向同目录 `snow-shot.exe`），`prefer_host_exe()` 及其唯一调用点 `main.rs::run_ui`（约 L473）随之删除或保留为空操作；单测 `auto_start_target_follows_mode` 同步。注册表值只存带引号的路径、不能带参数（`snow-platform::system_integration::run_command`），所以显式 `--resident` 的回退进程也只能指向宿主。**老用户迁移**：旧版 HKCU Run 值指向 `snow-shot-ui.exe`，改默认后它无参数启动会转去拉宿主（第 1 项），能用；之后 `system_settings::apply`（`main.rs` 约 L419 / L481、`app_runtime.rs` 约 L4529 在配置载入与变化时调用）会把值改写成宿主路径。开发构建（`debug_assertions`）仍不改注册表。
  3. **重启辅助**（`snow-shot/src/app_runtime.rs::spawn_restart_helper`，约 L1062）：单进程回退模式下用 `current_exe()` 无参数拉起新实例，切默认后会变成“拉宿主”，要改成带 `--resident`（回退模式重启回到回退模式）；双进程模式的重启由宿主处理（动作 `restart`），不走这里。
  4. **宿主侧**（`snow-tray-host`）：`--resident` 转调 UI（`app.rs::run_resident_fallback`）与 `cli.rs` 的 `USAGE` 基本不用改；无运行实例时 `--cmd` 成为主实例、命令被忽略的旧行为（阶段 2 偏差 11）是否保留，在此一并拍板。宿主互斥体 `Local\cisox.snow_shot.single_instance`、UI 的 `Local\cisox.snow_shot.ui`、会话管道名都**不用改**，只做回归（两种模式互斥、不双注册热键）。
  5. **快捷方式 / 开始菜单 / 安装脚本**：Rust 版没有安装脚本与快捷方式，无可改之处；将来出现时入口一律指向宿主 `snow-shot.exe`，不是 `snow-shot-ui.exe`。
  6. **验收与自检脚本**：`scripts/verify-snow-shot-recording.ps1`（约 L42 的 `Start-Process -FilePath $Exe` 无参数启动；后面两处带 `--cmd` 的是委托，不用改）与 `tools/self_acceptance_verify.ps1`（约 L20 `& $exePath`）要的是单进程完整应用，切默认后改为显式 `--resident`；`scripts/verify-tray-host-split.ps1` 本来就起宿主，不受影响。
  7. **打包 / 更新**：Rust 版打包脚本出现时必须把 `snow-shot.exe` 与 `snow-shot-ui.exe` 放同一目录，开始菜单与自启指向宿主；`snow-update` 将来自动安装要先 `--cmd quit` 宿主再替换两个 exe（现在只下载不安装，不受影响）。
  8. **文档同步**：本文 §3 表“直接双击 UI exe / 自启 / 单实例”几行与阶段 2 “产物名与过渡”里的“默认仍是单进程”、`AGENTS.md` 第 6 行与构建说明、`snow-shot-rs/README.md`“两个 exe 与运行方式”、`docs/cisox-progress-handoff.md` §6.2「现状 / 切默认的前提」、`docs/README.md` 索引这条、迁移方案 §2.1 的 workspace 布局与 ADR-9（MCP 进程内）加注“UI 保活时成立”、审计表 C03 / C04；开发说明里 `cargo run -p snow-shot` 要加 `-- --resident`。
  9. **不要动的**：`--resident` 路径与 `snow-shot-ui.exe --cmd` 的单实例委托（至少保留到用户另行拍板）；`HostCli::Resident`；worker 按 `exe_dir()` 找的逻辑。

### 阶段 5：冷启动优化

- **范围**：UI 启动即并行采集；UI 快退；无窗口动作走 `--headless`；若阶段 0 数据支持，再评估宿主侧采集。
- **验收**：冷拉起热键→首帧中位数 ≤ 0.7s（3 次取中位数，附机器信息，交替跑基线与优化版）；采集失败回退路径有测试；无窗口动作的耗时对比有记录。
- **禁止**：不引入常驻预热进程；宿主侧采集若做，宿主不得链接 D3D，整屏内存交完即还。

### 阶段 6（可选）：宿主精简

- **范围**：单线程消息循环；自写 Win32 托盘/热键替代 `tray-icon`/`global-hotkey`。
- **前提**：阶段 4 实测宿主私有提交超过 6MB，或剖析显示这两个库贡献显著（>1MB）。
- **验收**：提交下降量达到剖析预期；托盘行为（任务栏重建后补图标、DPI、深色菜单）与旧后端逐项一致。
- **禁止**：没数据不做。

### 对现有测试 / CI / 文档的影响

- **测试**：`snow-shot` 里与托盘、热键、`fullscreen_gate`、单实例映射相关的测试会随抽取调整位置；新增 spec 往返、热键计划对照、握手竞态、状态机、活动守卫测试。`cargo test --workspace` 仍由 CI 跑，本地不跑。
- **CI**：`workspace-guard` 依赖闭包规则（走现有 `cargo test --workspace`）；Windows 任务新增导入表闸门一步；新 crate 自动纳入 fmt/clippy/check。
- **文档**：迁移方案 §2.1 的 workspace 布局、ADR-9（MCP 进程内）加注"UI 保活时成立"；handoff 加进程模型一节；审计表 C03/C04；`docs/README.md` 索引；AGENTS.md 的 crate 清单与 `tools/` 说明；本文随实施更新状态。

## 9. 风险与回退

| 风险 | 说明 | 对策 |
| --- | --- | --- |
| **热键丢失** | 宿主崩溃或被杀期间热键失效；UI 与宿主都注册会冲突 | 宿主极小、独立崩溃转储；热键只在宿主注册（`--resident` 与宿主互斥）；宿主崩溃由 UI 拉起新宿主；启动时注册失败写日志并 tooltip 提示 |
| **焦点抢占** | 后台宿主拉起的 UI，窗口可能拿不到前台和键盘焦点，覆盖窗 Esc 失效 | 宿主 `AllowSetForegroundWindow(ui_pid)`；UI 开窗走现有 `force_foreground`（AttachThreadInput 路径）；阶段 0 的焦点矩阵是硬验收，不过不进阶段 2 |
| **UI 启动失败/超时** | 磁盘冷、杀软首扫、exe 被占用 | 10s 超时、tooltip 提示、日志；熔断（60s 崩 3 次退避 5 分钟）；不无限重试 |
| **杀软误报** | 两个 exe、常驻进程拉起子进程、键盘/鼠标低级钩子（有手势绑定时）、RegisterHotKey，行为上像木马宿主 | 发布签名；宿主不写注册表（自启由 UI 在设置里写）；钩子只在有绑定时装；首发前在常见杀软上过一遍（待做）。本机已有深信服 SNAC 注入，说明终端管控软件会改变测量，测量要记录 |
| **多显示器 / 混合 DPI** | 宿主若不是 PerMonitorV2，鼠标钩子坐标被虚拟化，手势错位；宿主侧采集（若做）同理 | 宿主清单声明 PerMonitorV2；手势坐标统一用虚拟桌面物理像素（现有约定）；真机验证混合 DPI 与负原点副屏（现状这两项也没真机验证过） |
| **鼠标手势全流程没法在这台机器测** | 现状就是这样（审计 C04） | 手势事件流的合并、保序、取消做成纯逻辑测试；真机清单里单列 |
| **配置双写** | 宿主和 UI 都写配置会互相覆盖 | UI 是唯一写者；宿主要改配置只能 `set_config`（UI 在）或 `--headless set-config` |
| **快照过期** | UI 没跑时系统语言/主题变了，托盘文案旧 | 可接受；UI 每次启动校验并重写快照 |
| **OCR/翻译变冷** | UI 退出带走 worker，连续截图要重新载入模型 | 见 §10 问题 5；`model_hot_start` 开启时保活 |
| **UI 退出竞态** | 命令写进已决定退出的 UI | §4 序号握手，宿主唯一仲裁，有确定性竞态测试 |

**回退：保留单进程模式可行，代价可控。** `snow-shot-ui.exe --resident` 跑旧路径（`run_primary` + `start_services`），它依赖的托盘/热键后端仍在 `snow-ui-shell`（重导出）里，不删。代价：①两条路径并存，托盘/热键的接线（旧：直接注册；新：出快照）共用阶段 1 抽出的纯函数，分叉点只有"注册还是序列化"；②`--resident` 与宿主共用同一个互斥体，所以不会同时跑；③切换方式见 §10 问题 8。回退到老行为只需用户改用 `--resident` 启动或删除自启里的宿主入口，不需要回滚代码。

## 10. 已拍板的决定（2026-10-12，全部按推荐）

1. **exe 命名**：宿主沿用 `snow-shot.exe`，UI 进程为 `snow-shot-ui.exe`。
2. **MCP 开启时**：UI 保活（v1 不做宿主中转；MCP 默认关，托盘态不受影响）。
3. **`model_hot_start` 开启时**：UI 保活。
4. **语音转文字**：绑定了语音热键就让 UI 保活；未绑定则随窗口关闭立即退出。
5. **worker 随 UI 立即退出**：接受，OCR/翻译空闲驻留作废，连续截图每次重新载入模型。
6. **托盘菜单**：走 UI 写入的快照（`host/spec.json`），宿主不链接 snow-config 与 i18n。
7. **宿主后端**：先复用 `tray-icon` / `global-hotkey`；阶段 6 实测超标时再自写精简版。
8. **单进程回退**：命令行 `--resident`，保留到拆分稳定后再删。
9. **崩溃恢复**：自动恢复贴图；熔断阈值 60s 内 3 次、退避 5 分钟。
10. **更新检查与登录恢复贴图**：允许宿主拉起 UI，任务完成后退出（登录时有贴图则按贴图规则保活）。
11. **宿主侧采集**：阶段 0 出数据后再决定是否在阶段 5 评估。
12. **宿主进程优先级**：Normal。
13. **冷缓存首帧**（阶段 0 实测清页缓存后约 1.6s）：可接受，不做登录预读等预热。
14. **阶段 2 硬验收追加**：托盘点击（左键/菜单项）拉起 UI 后能拿到前台与键盘焦点；UI 以管理员运行（及宿主非管理员、UI 管理员的混合情形）下热键、IPC、焦点正常。
15. **独立排查项（与拆分无关，常驻模式即受益）**：每次截图后内核句柄 +~9 不回落的泄漏；热路径里覆盖窗可见到内容出现约 400ms 的空档。

## 11. 未证实清单（阶段 0 验证）

1. UI exe 从 `CreateProcess` 到 `main` 的耗时，冷盘与页缓存热各多少。 **（已验证，见 §12.1）**
2. 宿主弹出托盘菜单后，`msctf.dll` 与第三方输入法（搜狗）是否被加载；`ImmDisableIME(0)` 是否拦得住。 **（已验证，见 §12.2）**
3. 宿主拉起的 UI 覆盖窗的前台焦点：热键、托盘、CLI 三种触发是否都能拿到键盘焦点。 **（已验证，见 §12.3；托盘点击一项未测）**
4. 剪贴板内容在 UI 退出后是否可粘贴（含图片 DIBV5 + PNG、文件列表）。 **（已验证，见 §12.4；DIBV5+PNG+文本+文件列表均已测）**
5. UI 快退耗时；`exit_ok` 到进程消失的分布。 **（部分验证，见 §12.5：空闲无窗口 `--cmd quit` 约 90~100ms；带窗口、带 worker 时未测）**
6. 宿主真实内存（含会话管道、鼠标钩子线程）与导入表白名单。 **（已验证，见 §12.2 与 §12.6：探针级；真宿主含会话管道/钩子线程仍待阶段 2 实测）**
7. 无窗口动作（直接截图等）脱离 GPUI 初始化能省多少。 **（未实测，见 §12.7 的静态估算）**
8. 终端管控/杀软（深信服 SNAC 已确认存在）对宿主拉起 UI 的耗时影响。 **（无法隔离验证，见 §12.8）**

## 12. 阶段 0 结果（2026-10-12）

不改产品代码。测量程序在 scratchpad（`p0probe`：tray-icon 0.25.1 + global-hotkey 0.8.0 + windows 0.62.2，与方案 §7 一致；`p0/` 下的 `cold2.ps1`、`mem5.ps1` 复用现有 harness），不入仓库。

**环境**：Dell，i5-13500，32GB，Intel UHD 770（驱动 31.0.101.4953），双屏 2560×1440 @59Hz，Win10 19045；release 为 2026-10-12 17:06 构建的 `snow-shot.exe`（已不导入 `uiautomationcore`）。深信服 SNAC 往每个进程注入 `printerhook64`、`printeradapter64`、`csxumd64_21408`、`umppc21408`（见 §12.8），所有数字都含它。焦点与剪贴板实验的驱动进程是管理员权限；被测的宿主/子进程由 explorer 通过 .lnk 启动（意图是非管理员，**未用令牌核对**）。

### 12.1 UI 冷启动分段（release，对应 §11 第 1 项）

测法：用 `--cmd screenshot` 在管道就绪时投递命令（等价于"UI 带着待处理命令启动"），时间都从**进程创建时间（FILETIME）**算起，日志是 UTC 微秒。"首帧"取两块屏幕像素都变化（PixAll）。页缓存热 n=6，冷（每次先清备用列表/修改列表，`NtSetSystemInformation`）n=6；n 偏小，看量级，不看小数。

| 阶段（距进程创建，ms，中位数） | 页缓存热（范围） | 清页缓存后（范围） | 拆分后 UI 进程还需要吗 |
| --- | --- | --- | --- |
| 日志 `runtime bootstrap completed`（≈ 到 `main` + 引导） | **56**（47~121） | **145**（102~236） | 需要 |
| GPU 设备就绪（`Created device with Direct3D`） | 132（110~241） | 252（167~580） | 需要 |
| 进程优先级已设置（GPUI 初始化、配置加载、字体之后） | 249（213~447） | 429（323~815） | 需要 |
| 托盘图标已启动 | 534（421~905） | 762（576~1173） | **不需要** |
| 热键服务已启动 | 540 | 772 | **不需要** |
| 事件循环开始 | 541（425~916） | 777（589~1189） | 需要 |
| 采集完成 | 644 | 896 | 需要 |
| 覆盖窗"已打开"日志 | 874 | 1219 | 需要 |
| 覆盖窗首次可见（枚举到 ≥2000×1200 的可见窗） | 682 | 953 | 需要 |
| **首帧（两屏像素都变）** | **1218**（1030~2012） | **1567**（1264~2215） | — |

分段（热，中位数）：优先级→事件循环 286ms（含托盘与热键）、事件循环→采集完成 106ms、采集完成→"已打开"日志 232ms。对照 `--wat` 参数（进程到 `main` 就退出）的存活时间：热 51ms（n=10），冷 86~115ms（n=5），与上表 `bootstrap` 吻合。

**对照热路径**（常驻进程，F1 → 首帧，n=8）：窗口首次可见 133ms，首帧 PixFirst 535ms / PixAll 550ms，采集 74ms，"已打开"日志 195ms。热路径 0.55s，与方案写的 0.57s 一致。**窗口可见到内容画出来之间有约 400ms 空档**（133 → 535），这是热路径里最大的一块，与拆分无关（见 §12.9 第 6 条）。

**托盘 + 热键这一段的真实成本**：探针里 `TrayIconBuilder::build`（20 项菜单、32px 图标）44~51ms，12 个热键注册 2~3ms，共约 50ms。产品里"优先级→托盘启动"约 285ms，所以其中约 **235ms 是 `build_tray_spec`（i18n + 16px 图标光栅化）**。拆分后它只在"快照过期要重写"时才需要，但 i18n 与图标渲染器覆盖窗也要用，成本可能只是被挪后而不是消失；**不插桩分不开**（阶段 0 不改产品代码），留给阶段 1 的 spec 纯函数化顺手量。

**拆分后"宿主收到热键 → UI 首帧"的估算**（热页缓存，中位数；估算，非实测）：

| 情形 | 计算 | 结果 |
| --- | --- | --- |
| 现状冷启动（含托盘与热键） | 实测 | 1.22s |
| 只扣 `TrayService::start` + 热键（下界，≈ 50ms） | 1218 − 50 | 1.17s |
| 扣掉优先级→事件循环全部 286ms（上界：假设 `build_tray_spec` 完全不再需要） | 1218 − 286 | 0.93s |
| 上界情形再叠"UI 启动即并行采集"（可重叠 ≈ 采集 106ms） | 0.93 − 0.10 | 0.83s |
| 加宿主侧：热键 → `CreateProcess` 返回 12~20ms + 握手 5~20ms | + 约 30ms | **约 0.86s（乐观）~ 1.2s（悲观）** |
| 清页缓存（冷）版本 | 同口径，起点 1567 | 约 1.2~1.6s |

结论：**方案 §6 的 0.65~0.9s 只有在"托盘菜单构建成本不再出现 + 并行采集"两个条件同时成立时才勉强摸到 0.9 的验收线**，0.7 的优化目标目前没有数据支持。§6 预算表两处与实测不符：①"UI exe 加载到 `main` 100~250ms"实际热 ≈ 50ms、冷 ≈ 100~150ms，偏悲观；②"GPUI 初始化 + 配置 + D3D 250~350ms"实际 bootstrap→优先级已设置 ≈ 190ms（热），偏高；托盘 + 热键 ≈ 285ms，与方案写的 0.3s 一致。验收线建议见 §12.9。

### 12.2 宿主探针：模块与内存（对应 §11 第 2、6 项）

探针只有 `tray-icon + global-hotkey`（20 项菜单、12 个热键），启动后空闲 1.5s，再弹托盘菜单 5 次（程序内弹，1.5s 后注入 Esc 关闭，弹前走与产品 `force_foreground` 相同的前台序列），默认变体 n=3、弹菜单前 `ImmDisableIME(0)` 变体 n=3。当前键盘布局 `0x08040804`（简体中文，搜狗拼音是其输入法），布局列表 `0804` + `0409`。

| 变体 | 阶段 | 工作集 | 私有提交 | 模块数 | 搜狗 / TSF / UIA |
| --- | --- | --- | --- | --- | --- |
| 默认 | 启动 | 11.3~11.6MB | 2.77MB | 42 | `msctf`、`imm32` 已有 |
| 默认 | 空闲 1.5s | 12.5MB | 2.95MB | 47 | 同上 |
| 默认 | **第 1 次弹菜单后** | 56.7~59.1MB | **36.0~36.6MB** | **72** | **搜狗全进来**：`sogoupy.ime`、`sogoutsf.ime`、`ichat_bundle64`、`isgpet_bundle64`、`picface64`、`systembeautify_bundle64`、`textinputframework`，另有 `d2d1`、`dwrite`、`oleacc`、`ole32` 等 |
| 默认 | 第 5 次弹菜单后 | 56.9~59.2MB | 36.1~36.8MB | 72 | 不再新增 |
| 默认 | `EmptyWorkingSet` 后 | 0.90~0.93MB | **36.1~36.8MB（不变）** | 72 | — |
| `ImmDisableIME(0)` | 启动 | 11.1MB | 2.74MB | 40~41 | `imm32`；`msctf` 没有 |
| `ImmDisableIME(0)` | 空闲 1.5s | 12.3MB | 2.91MB | 46 | 同上 |
| `ImmDisableIME(0)` | **第 1 次弹菜单后** | 13.6MB | **3.05~3.16MB** | **48** | 只多了 `msctf.dll`；**没有任何输入法 DLL** |
| `ImmDisableIME(0)` | 第 5 次弹菜单后 | 13.6MB | 3.05~3.16MB | 48 | 不再新增 |
| `ImmDisableIME(0)` | `EmptyWorkingSet` 后 | 0.74~0.77MB | 3.05~3.16MB（不变） | 48 | — |

- **`uiautomationcore.dll` 在两个变体的任何阶段都没出现。**
- 其余耗时：`TrayIconBuilder::build` 44~51ms；12 个热键 1.5~3.3ms。
- 提交与工作集分开看：`EmptyWorkingSet` 只把工作集压到 ~0.8~0.9MB，提交不变，本节"内存降了"都指**私有提交**。
- 弹菜单之前（47 个模块）没有输入法 DLL，第一次弹菜单才加载，所以注入时机就是"菜单窗口拿到前台"。

**这是本轮最重要的发现**：宿主只要弹一次托盘菜单且没禁 IME，就从 3MB 变成 36MB，直接击穿 §7 的"≤ 6MB"目标。§7 注 ③ 里"`msctf.dll` 有时出现、样本小不能下结论"现在定论：**`ImmDisableIME(0)` 能拦住输入法 DLL（搜狗一个都不进来），拦不住 `msctf.dll` 本身（约 +0.2MB，可接受）**。限制与未验证：①`ImmDisableIME(0)` 是**线程级**的，探针里托盘窗口和菜单都在调用它的同一线程上；产品里托盘在 `shell-tray` 线程，必须在**该线程创建窗口前**调用（或用 `-1` 对全进程线程生效，`-1` 本轮没测）；②只测了弹菜单，没测托盘气泡/提示；③只测了搜狗，没测微软拼音与其它输入法。

### 12.3 焦点（对应 §11 第 3 项）

方法：自制"受害者"窗口占前台；宿主（探针）经热键/管道消息触发，`CreateProcess` 拉起子进程；子进程建一个整屏置顶弹出窗，按变体决定怎么抢前台（`plain` = 只调 `SetForegroundWindow`，`attach` = 产品 `force_foreground` 的 `AttachThreadInput` 序列，`none` = 只显示窗口），延迟 0 或 700ms 后判定：①是否前台窗口；②自注入 F24 键（没人使用，不会打到别处）窗口是否收到 `WM_KEYDOWN`。

**方法学说明**：这台机器 `ForegroundLockTimeout` 是默认 200000ms，但用户长时间不在，真实输入早过期，前台锁形同虚设（`plain` 无任何授权也成功），测不出区别。所以用 `SystemParametersInfo(SPI_SETFOREGROUNDLOCKTIMEOUT, 2000000000)` **仅本会话**调大锁超时，测完恢复，已复查为 200000，未写注册表。调大后，对照组（定时器触发，没有输入、没有授权，`plain`）成功率降到 2/5，说明测试能区分有/无权限。每格 n=5（CLI 链 n=4），数字为成功次数；个别试验受害者没拿到前台（污染），仍计入分母，所以偏低的格子有噪声。

| 触发 | 宿主授权 | 子进程抢前台方式 | 子进程延迟 | 成为前台 | 收到键盘 |
| --- | --- | --- | --- | --- | --- |
| 热键 | 无 | plain | 0 | 5/5 | 5/5 |
| 热键 | 无 | plain | 700ms | 5/5 | 5/5 |
| 热键 | 无 | attach（产品序列） | 700ms | 5/5 | 5/5 |
| 热键 | `AllowSetForegroundWindow(子 PID)` | plain | 0 | 5/5 | 5/5 |
| 热键 | `AllowSetForegroundWindow(子 PID)` | plain | 700ms | 5/5 | 5/5 |
| 热键 | `ASFW_ANY` | plain | 700ms | 4/5 | 4/5 |
| 热键 | 无 | none（只显示） | 700ms | 4/5 | 4/5 |
| 热键 | `ASFW(子 PID)` | none（只显示） | 700ms | 3/5 | 3/5 |
| 定时器（对照，无输入） | 无 | plain | 700ms | **2/5** | 2/5 |
| 定时器（对照） | `ASFW(子 PID)` | plain | 700ms | 1/5 | 1/5 |
| 定时器（对照） | `ASFW_ANY` | plain | 700ms | 2/5 | 2/5 |
| 定时器（对照） | 无 | attach（产品序列） | 700ms | **5/5** | 4/5 |
| CLI（由前台进程拉起 CLI）→ 宿主 | CLI 先 `ASFW(宿主)`，宿主再 `ASFW(子 PID)` | plain | 700ms | **4/4** | 4/4 |
| CLI | CLI 不授权，宿主 `ASFW(子 PID)` | plain | 700ms | 1/4 | 1/4 |
| CLI | 宿主不授权（CLI 授权 / 不授权） | attach | 700ms | 4/4 / 4/4 | 4/4 / 4/4 |

结论：
1. **热键路径不需要 ASFW**：宿主处理 `WM_HOTKEY` 时 `CreateProcess` 出来的子进程自带前台权（无输入的对照组 `plain` 只有 2/5，热键时 5/5）。延迟 0 与 700ms 结果一致。
2. **产品现有的 `force_foreground`（`AttachThreadInput`）所有情形都拿到前台**（含无任何输入的对照组 5/5），UI 进程里保留它就是最后一道保险。
3. **CLI 路径要链式授权**：`--cmd` 的从属进程先调 `AllowSetForegroundWindow(宿主 PID)`（它由用户的终端/快捷方式拉起，自身持有权限；测试里用"前台窗口进程拉起 CLI"模拟，返回 true），宿主再对 UI 的 PID 授权，`plain` 4/4；CLI 不授权时只有 1/4。UI 一律走 `attach` 序列则这一步可选。
4. **未测：托盘左键触发**（探针已拿到托盘图标矩形，但真点击要占屏，按用户要求省略）；托盘点击与热键同属"宿主刚收到物理输入"，预期同热键，仍需阶段 2 真机复核。
5. 管理员驱动 / 非管理员被测进程的组合没有用令牌核对；UI 进程以管理员运行（`launch_as_administrator`）时的 UIPI 行为未测。

### 12.4 剪贴板（对应 §11 第 4 项）

进程 A 按产品（`snow-platform/src/clipboard.rs`）同样的调用写入——`OpenClipboard(NULL)`、`EmptyClipboard`、`GlobalAlloc(GMEM_MOVEABLE)` + `SetClipboardData`，格式 `CF_DIBV5` + 注册格式 `PNG` + `CF_DIB` + `CF_UNICODETEXT` + `CF_HDROP` 同时放，然后**立即退出**；进程 B 枚举并读取每个格式的字节数，`Get-Clipboard` 读文本。每种 n=1。

| 写入方式（进程 A 退出后） | B 读到的格式 | 结论 |
| --- | --- | --- |
| 立即渲染（产品现用） | `CF_DIBV5` 262268B、`PNG` 67B（与写入字节数一致）、`CF_DIB` 262184B、`CF_UNICODETEXT` 52B、`CF_HDROP` 106B，另有系统合成的位图等；`Get-Clipboard` 读到文本 | **全部可粘贴** |
| 延迟渲染，A 退出前销毁所有者窗口（处理 `WM_RENDERALLFORMATS`） | 同上，五种格式齐全 | 可粘贴，但要求正确处理该消息 |
| 延迟渲染，A 直接退出 | 只剩 `CF_LOCALE`（4B）与一个读取失败的格式，文本为空 | **内容丢失** |

结论：产品现在的立即渲染方式，UI 进程退出后剪贴板内容完整保留（图片 DIBV5 + PNG、文本、文件列表均验证）。**约束**：以后任何路径（含将来的延迟渲染优化）都不得改成延迟渲染，除非在 UI 快退前先渲染全部格式；建议在 `clipboard.rs` 旁加注释/测试固化。文件列表指向的文件在 UI 退出后仍要在磁盘上（自然成立）。

### 12.5 UI 退出耗时（对应 §11 第 5 项，部分）

release 空闲（无窗口、无 worker）发 `--cmd quit`，从发命令到主进程消失 88~101ms（n=3；命令进程自身约 60ms 返回），低于 §6 优化点 4 的 150ms 目标。**带窗口（覆盖窗、贴图）、带 OCR/翻译 worker、历史写入未 flush 时没测**；`exit_ok` → 进程消失的分布依赖阶段 2 的会话握手，现在测不了。

### 12.6 宿主导入表白名单（对应 §11 第 6 项，导入表部分）

对仅含 `tray-icon + global-hotkey` 的探针 exe 解析 PE 静态导入（无延迟导入）：`kernel32`、`ntdll`、`user32`、`gdi32`、`shell32`、`comctl32`、`imm32`、`psapi`、`bcryptprimitives`、`vcruntime140` 与 `api-ms-win-crt-*`、`api-ms-win-core-synch`（探针用动态 CRT；真宿主按 §3 用静态 CRT，CRT 相关项会消失）。

对照现 `snow-shot.exe`（37 项）多出：`d3d11`、`dxgi`、`dcomp`、`dwrite`、`dwmapi`、`gdiplus`、`icuuc`、`oleacc`、`propsys`、`combase`、`ole32`、`oleaut32`、`advapi32`、`dbghelp`、`winmm`、`userenv`、`bcrypt`、winrt 相关。（测量时 `uiautomationcore` 不在导入里，是关闭无障碍的结果；该补丁已于 2026-10-12 回退，现 exe 会重新导入它。）

建议 `scripts/check-host-imports.ps1` 白名单：`kernel32 user32 gdi32 shell32 comctl32 imm32 psapi ntdll bcryptprimitives advapi32 api-ms-win-*`（`advapi32` 给宿主将来读主题注册表/管道 ACL 留余量，探针本身没导入）；黑名单沿用 §3 并补 `dcomp`、`gdiplus`、`oleacc`、`icuuc`、`dwmapi`。真宿主（鼠标钩子、命名管道）可能多出的导入，阶段 2 实际构建后用同一脚本复核。**阶段 2 已复核定稿**：实际导入表见 §8 阶段 2 的离屏验收结果；与上面的建议白名单相比，`dwmapi` / `bcrypt` / `dbghelp` 经运行时加载或改用 `ProcessPrng` 去掉，多出 `oleaut32`（`windows` crate 的错误类型）与 `vcruntime140`（动态 CRT），白名单代码里写明了理由。

### 12.7 无窗口动作脱离 GPUI（对应 §11 第 7 项，未实测）

读代码：`spawn_direct_capture`（`direct_capture.rs`）在线程里做 GDI 采集 + 编码，不依赖 GPUI，但目前由事件循环里的 `UiEvent::DirectCapture` 触发。拆分后 `--headless` 省掉的是 GPUI/D3D/字体初始化：按 §12.1，bootstrap→事件循环开始热 ≈ 485ms，其中托盘 + 热键 ≈ 285ms 拆分后本来就没有，剩下约 200ms 是 headless 可再省的量级。**这是估算**，且要先拆开 `request_direct_capture` 对 `ShellContext` 的依赖才能真测，留给阶段 5。

### 12.8 终端管控 / 杀软（对应 §11 第 8 项，无法隔离）

SNAC 的四个 DLL 在探针和产品进程里都在，没法关掉做 A/B，不能分离它对拉起耗时的贡献。本节所有耗时都含 SNAC；真首次拉起（开机后、有杀软首扫）会比"热/清缓存"两栏更慢，阶段 2 之后要在有/无管控的机器各测一次。

### 12.9 对方案的影响与待决事项

1. **（硬约束，新增）宿主必须在创建托盘窗口的线程上先调 `ImmDisableIME(0)`（或 `-1`），并加回归测试**：弹一次菜单后断言宿主模块里没有 `*.ime`/`textinputframework`，私有提交 ≤ 6MB。不做的话宿主一弹菜单就是 36MB（本机搜狗）。§7"做法"里已有这一条，但它现在是正确性前提，不是优化。
2. **§6 启动预算要改**：热缓存下拆分后首帧估算 0.86~1.2s，最乐观情形（托盘构建不再出现 + 并行采集）≈ 0.83~0.86s；"≤ 0.9s 验收线"只在乐观情形下成立。建议阶段 2 先验收 **≤ 1.2s（不劣于现状冷启动 1.22s）**，0.9s 作阶段 5 目标；冷页缓存另列一栏（约 1.6s）。§6 里 UI exe 加载到 `main` 的估算改为"热 ~50ms，冷 ~100~150ms"。
3. **宿主侧采集：不立项。** §6 的判定规则是"CreateProcess → UI 开始采集 > 150ms 才立项"。UI 在 `main` 之后就能起采集线程，起点热 56ms / 冷 145ms（p95 236ms）；热缓存远低于 150ms，冷缓存与 150ms 持平。UI 启动即并行采集已拿到绝大部分收益，宿主侧采集的额外收益 ≤ 56ms（热），代价不划算；日后"开机后首次拉起"的真实数据显示 > 150ms 再复议。（这是对 §10 第 11 条的回答，没有改 §10。）
4. **焦点（§9"焦点抢占"）**：热键路径免授权、产品 `force_foreground` 全情形可用，风险明显降低；§4 的 `AllowSetForegroundWindow(ui_pid)` 仍建议保留（成本 0.02ms）。**需补：`--cmd` 的从属实例先对宿主 PID 调 `AllowSetForegroundWindow`**（§3/§4 目前没写），否则 CLI 触发只能靠 UI 侧 `attach` 序列兜底。
5. **剪贴板**：§5 里"剪贴板：立即渲染，退出后保留"可视为已实测；新增约束：不得改延迟渲染（见 §12.4）。
6. **新增（与拆分无关，数据暴露）**：热路径里窗口首次可见（133ms）到内容出现（535ms）有约 400ms 空档，是首帧里最大的一块，拆分不会改善；阶段 5 应单列"覆盖窗首帧上屏"剖析（日志里 `frame_interval` p95 ≈ 980ms 是同一现象的另一面）。
7. **关闭无障碍对截图后内存没有收益（已补测，2026-10-12 17:52，用户解锁后）**：release（17:06 构建，不导入 `uiautomationcore`）单次完整流程，覆盖窗确认出现（日志有 `capture requested` / `screenshot overlay opened`，打开时私有提交 529MB）。

   | 时刻 | 私有提交 | 私有工作集 | 工作集 | 线程 | 句柄 | GDI | USER |
   | --- | --- | --- | --- | --- | --- | --- | --- |
   | 空闲 15s | 56.7MB | 32.9MB | 61.4MB | 20 | 431 | 102 | 29 |
   | 覆盖窗打开中 | 529.2MB | 171.9MB | 224.9MB | 35 | 904 | 194 | 88 |
   | 关闭后 +15s | **135.1MB** | 102.6MB | 155.9MB | 29 | 766 | 174 | 67 |
   | 再截 2 次，关闭后 +3s（第 2 次） | 135.7MB | 103.2MB | 156.4MB | 29 | 775 | 174 | 67 |
   | 第 3 次 | 136.3MB | 103.7MB | 156.9MB | 29 | 784 | 174 | 67 |

   - 对比关闭无障碍前（截图后私有提交 134~136MB）：**一样，没省**。空闲态也一样（61.5 / 56.8 / 32.7 对 61.4 / 56.7 / 32.9）。UIA 初始化不是截图后多出来那约 78MB 的来源，"研究文档 §3.1 方案 A"的收益为零。
   - 句柄：第 1 次截图后比空闲多 335（431 → 766），之后每次截图 +9（766 → 775 → 784），与此前 "+~10/次" 一致，仍在缓慢增长，关无障碍没改变这点（n=3，趋势而非定量）。GDI/USER 句柄不增长，增长的是内核对象句柄。
   - 截图后新增线程里能看到 `ime_bundle`、`isgpet_ime_bundle`（搜狗），线程 20 → 29；与 §12.2 一致，剩余的 ~78MB 里输入法占大头这一判断继续成立，换进程是唯一能清干净的办法。
8. **需要用户决定**：①冷缓存下约 1.6s 的首帧是否可接受（不接受就要"登录后低优先级预读 UI exe"一类预热，与"不常驻预热"原则取舍）；②托盘点击焦点、"UI 以管理员运行"两项是否纳入阶段 2 的硬验收。

### 12.10 独立排查：句柄泄漏与首帧空档（对应 §10 第 15 条，2026-10-12）

两项都与拆分无关，常驻单进程即受益。测量程序在 scratchpad（句柄枚举 `handles.ps1`、cdb 句柄追踪脚本、独立 D3D11 探针 `dxprobe`），不入仓库；诊断开关 `CISOX_TIMING_DIAG`（默认关，`timing_diag.rs`）随代码保留。环境同 §12 开头（i5-13500 / UHD 770 / 双屏 2560×1440 / SNAC）。

#### A. 句柄泄漏

**是什么**：用 `NtQuerySystemInformation(SystemExtendedHandleInformation)` + `DuplicateHandle` + `NtQueryObject` 按类型统计（没用 handle.exe，本机没有也没下载）。release（17:06 构建）空闲 427，第 1 次截图后 762（+335，一次性），之后每次 768、777、786、795，连截 14 次每次 +6~11（均值约 9），**没有平台期**。第 1 次到第 5 次之间类型差异只有 `Event` 156 → 192，全部是无名事件（访问掩码 0x1F0003）；GDI/USER/线程/ALPC 都不涨。

**谁建的**：cdb 挂到进程上开 `!htrace`，未关闭句柄的创建栈全部是 `igd10um64xe`（Intel 显卡用户态驱动）里的 `CreateEventA`，没有一个是本进程 Rust 代码。再在 `CreateEventA` 上下条件断点，抓到的主线程栈落在 `DirectXRenderer::draw` → Present 以及着色器创建（着色器那批事件会被关，不漏）。最后用独立探针（共享设备、每轮建全屏窗口 + 合成交换链 + 着色器 / MSAA 纹理 / 缓冲 / 绘制 + 销毁，30 余组组合）二分：

| 收尾动作（每轮一次，轮前有过绘制） | 句柄逐轮增长 |
| --- | --- |
| 不收尾 | 0 |
| `IDXGIDevice3::Trim` / `Flush` 单独 | 0 |
| **`ClearState`（单独也一样）** | **+1/轮** |
| `OMSetRenderTargets(None)` + VS/PS 资源槽置空 + `Flush` + `Trim` | 0 |

没画过东西就 `ClearState` 不漏。探针里第 1 轮的 +150 左右也复现了（驱动 / DirectComposition 的一次性初始化），所以"第 1 次 +335"属正常，不用管。

**根因**：`vendor/gpui-pre-windows` 里本分支加的 `GpuReleaseGuard`（见 `vendor/README.md` 补丁登记）在每个渲染器销毁时调 `ClearState`，Intel UHD 770 驱动（31.0.101.4953）每次 `ClearState` 会在 UMD 里漏一个永不关闭的内核 Event。覆盖窗两块屏各销毁一次，加上绑定点更多，产品里每次关窗约 9 个。**这是驱动缺陷被我们的补丁触发，不是 Rust 侧 Drop / 所有权问题。**

**修复**：`GpuReleaseGuard::drop` 改为显式解绑（新函数 `unbind_window_resources`：`OMSetRenderTargets(None)`、VS / PS 资源槽 0..2 置空、`Flush`），再 `Trim`；不再调 `ClearState`，因此也不用再补回光栅化状态。旧窗口的渲染目标 / 图集纹理仍然被放掉（这是补丁原本的目的）。单测 `release_guard_tests`（WARP 软件设备，离屏）：守卫析构后渲染目标与两个阶段的资源槽全空、光栅化状态保留、无目标时是空操作。vendor 不是 workspace 成员，测试要把 crate 拷到 workspace 外追加空 `[workspace]` 后 `cargo test --offline --lib --features test-support release_guard`（2 项通过）。补丁登记已更新。WARP 不复现驱动 bug，所以"不漏句柄"本身靠上面的探针与真机复测证明。

**修后复测（2026-10-13，release 含 `GpuReleaseGuard` 补丁，连截 3 次）**：空闲 426，第 1 次后 766，第 2、3 次后 773、777（逐次 +7、+4）；同一脚本同一机器的修前基线逐次 +6~+9。**句柄仍在逐次增长，修复没有把泄漏消除**：第 1 到第 3 次之间只有 `Event`（全部无名、访问掩码 0x1F0003）163 → 178，对比修前同口径 156 → 174。3 次样本太少，不能断言斜率变了；原因判断（ClearState 触发驱动漏句柄）至少不完整，还有别的触发点，待再查。

#### B. 热路径首帧 400ms 空档

**方法**：加 `CISOX_TIMING_DIAG` 打点（采集线程完成、开窗各步、`configure_overlay` 各段、首次 render），并临时在 vendor 渲染器 / 窗口创建处打了点（测完已撤掉，不留在 vendor 里）。用非 LTO 的 release 构建、同一套 F1 harness 测，日志是 UTC 微秒，时间从 `capture requested` 起（比 F1 晚约 2ms）。

**先排除 GPU**：渲染器创建 2~4ms，两次 `ResizeBuffers` 各 1~2ms，**首次 draw 的 render 0.1~0.5ms、Present 0.4~1.4ms**，Present 到像素变化不过几毫秒。空档不在 GPU / DWM，是**主线程被同步阻塞**。

**一次典型时间线**（ms，节选；harness 的 PixAll 为 526~589）：

| 时间点 | 距 F1 处理 | 说明 |
| --- | --- | --- |
| 两块屏采集完成 | 75 / 103 | GDI BitBlt，两线程并行，开窗要等最慢那块 |
| 第 1 个窗口 `open_window` | 103 → 205（≈100） | 其中 `SetWindowPlacement` 阻塞 79ms（第 2 个窗口同一步只要 19ms） |
| 第 2 个窗口 | 210 → 250 | |
| **`configure_overlay`** | **250 → 409（160）**；另一轮 266 → 608（**342**） | 见下 |
| 落位（`finalize_native_placement`） | 418 → 472 | 去边框 / `SetWindowPos` 触发 2576×1448 → 2560×1440 两次改尺寸 |
| 首次 Present | 486 / 495 | |

**主因**：`configure_overlay` 里同步起 `reg.exe` 子进程读系统偏好，每次约 80~90ms（SNAC 注入，更慢），一次截图最多 4 个：
1. `interface_locale()` → `system_ui_language()`（配置语言为空即"跟随系统"时），不缓存，90ms；
2. `locale_of()` → `SystemPrefs::cached()`，5 秒 TTL 过期就查深色 + 语言两次，约 165ms（用户隔几秒才按一次 F1，几乎总是过期）；
3. `TranslateConfig::from_document(.., &system_ui_language())`，实参即使用不上也先求值，86ms。

harness 每轮间隔 1.8~3s，TTL 命中率比真实使用高，所以测得 160ms 偏乐观，真实使用接近 340ms。**这就是"窗口可见 → 内容出现"空档里最大的一块。**

**修复**：`sys_prefs.rs` 不再起 `reg.exe`，改用 `snow_platform::system_integration::{read_user_dword, read_user_string}`（新增，`RegGetValueW`，沿用 snow-platform 已有的 `Win32_System_Registry` 特性，不新增依赖，符合"优先用系统已有能力"）。这样 4 次读取降到微秒级，TTL 缓存与实参提前求值不再有代价。单测：REG_SZ 往返、类型不符返回 None、缺项返回 None、系统语言非空、200 轮读取 < 1s。

**修后复测（2026-10-13，release，同口径两屏像素都变，各 3 次）**：修前（17:06 构建）PixAll 481ms（455~772）、WinFirst 122ms、Open1 165ms；修后 PixAll **358ms（323~392）**、WinFirst 137ms、Open1 181ms。中位数快约 123ms（约 26%），主要来自去掉 `reg.exe` 子进程，窗口出现时间持平。

**没修的，供用户决定**（都不是小改动，收益是估算）：
1. **采集与建窗串行**（采集 100~110ms 之后才建窗）：可让窗口先建好（透明或隐藏）、采集完成后再上底图。预计省 80~100ms，要改覆盖窗的"先建后填"状态机，牵涉共享视图与多屏收集，建议单独评审。
2. **首个窗口 `SetWindowPlacement` 阻塞 79ms**：怀疑与全屏 `WS_EX_NOREDIRECTIONBITMAP` 窗口首次显示（DWM）或 accesskit / UIA 适配器有关，未证实，要 ETW / 窗口过程内打点才能归因；若是 accesskit，用户已要求不再关无障碍，只能想别的办法（例如预创建隐藏窗口，与"不常驻"原则冲突）。
3. **落位引起两次多余改尺寸**（窗口先按逻辑坐标建出 2576×1448，再去边框收成 2560×1440，约 50ms）：可在建窗时直接给无边框样式与精确物理矩形，需改 `gpui-kit::open_window` 的选项或 vendor，属 vendor 改动，需另行评审。

#### 验收与未完成项

- `cargo clippy -p snow-platform -p snow-shot --all-targets -- -D warnings` 通过；`cargo test -p snow-platform system_integration`（6 项）、`cargo test -p snow-shot --bin snow-shot -- sys_prefs:: timing_diag:: settings_state`（33 项）、`cargo test -p workspace-guard` 通过；vendor 的 `release_guard_tests` 在 workspace 外的拷贝里通过。
- 真屏复测已完成（2026-10-13），结果见 A、B 两节「修后复测」：首帧改善，句柄泄漏未消除。
- **搜狗注入复测**（`docs/research/tray-host-minimal.md` §7 第 1 项，探针 `hostmin run pure`，有前台，各 3 次）：默认配置弹菜单后搜狗被注入（`SogouPY.ime`、`SogouTSF.ime`、`msctf.dll`、`ichat_bundle64`，模块 45 → 71，私有提交 2.8MB → 37MB）；禁用扩展点 + `ImmDisableIME(-1)` 后 3/3 都没有任何搜狗 / `.ime` / `msctf` 模块，提交 1.57MB → 1.82MB。真宿主（`snow-shot.exe`）在 `ime` 场景里弹菜单后同样无 `.ime` 模块，提交 2.3MB。
