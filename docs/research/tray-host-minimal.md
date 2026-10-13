# 托盘宿主内存最小化：还能再压多少

> 状态：调研 + 本机探针实测完成，**未改任何产品代码**；方案待用户拍板。日期 2026-10-12。（2026-10-13 补：建议 1、2 已落地；阶段 6 的精简逐项实测与取舍见 [`design/tray-host-split.md`](../design/tray-host-split.md) §8 阶段 6 执行记录，其中菜单按需构建、图标像素共享、轻量线程等本文没有预见的项合计省了约 0.23MB。）依据 [`principles.md`](../principles.md) 的优先级链（高性能 > 低内存 > 高 fps > 少编译依赖 > 多用系统能力）。
> 关联：[`design/tray-host-split.md`](../design/tray-host-split.md)（宿主目标 ≤ 6MB 见其 §7，阶段 0 探针见 §12）、[`research/tray-idle-memory.md`](tray-idle-memory.md)（为什么要拆进程）。本文只回答一件事：宿主这个常驻小进程，私有提交还能不能再往下压，怎么压。
> 资料来源说明：先交给 antigravity 做联网调研。第一次 5 分钟超时，只回了一句"返回部分输出"，没有内容；第二次（缩小范围）返回了一张表，但我抽查其中 3 条链接，**全部对不上**（voidtools FAQ 链接 404；ShareX #8097 实际是 1~2GB 的泄漏报告，不是它写的"30~60MB 托盘态"；PowerToys #47518 里没有任何内存数字），所以它的内容整体弃用。下面所有事实只来自两处：我自己 WebFetch 读到正文的官方文档/仓库页面，以及本机探针实测。读到正文的写"已证实"，只有搜索摘要或没能核对的写"未证实"。

## 1. 结论（先看这里）

1. **最大的一笔是进程级的"禁用扩展点"缓解，不是任何编译选项。** 宿主启动后第一件事调 `SetProcessMitigationPolicy(ProcessExtensionPointDisablePolicy)`：本机实测启动态提交 2.80MB → 1.64MB，弹一次菜单后 3.3MB → 2.1MB，线程 9 → 4，启动态模块 45 → 29。再叠 `ImmDisableIME`（同线程，实测用的是 `(0)`，同线程时 `-1` 结果相同），弹菜单后到 **1.85MB**（纯 Win32）；`tray-icon`+`global-hotkey` 原样不动时是 1.91~2.02MB。这台机器上约 1.1MB 的收益来自深信服 SNAC 的钩子 DLL 链（环境因素，见 §3.1），干净机器上能省多少没测；但这个缓解按微软文档同时会挡掉旧式 IME、AppInit DLL、全局钩子注入，对"宿主不能被 IME 注入"本身就是对症的。
2. **编译选项全是零收益**：静态 CRT、`panic=abort`、栈大小、`no_main` 绕过 std 初始化，每一项 n=5 的提交都落在 ±0.03MB 的噪声里（§3.2）。别在这上面花时间。
3. **托盘/热键后端换成纯 Win32 自写，只再省 0.15MB 提交**（2.0 → 1.87MB，已经叠了上面两条的前提下）；`EmptyWorkingSet` 后工作集 0.77MB → 0.20MB。边际收益很小，`tray-host-split.md` 阶段 6 的前提是"没数据不做"，现在数据有了：不够格，不做。
4. **`EmptyWorkingSet` 只降工作集不降提交**（官方文档、实测都一致）：工作集 11MB → 0.4MB，代价是每次热键约 260 次软缺页，唤醒延迟 0.04ms → 0.08ms 量级、`CreateProcess` 无显著变化。没有内存压力时几乎感觉不到；有内存压力时的硬缺页延迟没测（§7）。
5. **拉起 UI 子进程的选项不值得抠**：`bInheritHandles` 真假、句柄白名单、Job Object 之间的差距 < 0.5ms，淹没在 ~10ms 的 `CreateProcess` 本身里。
6. **业界公开数据很少**：只有 Everything 有官方内存数字（随索引量走，不是托盘基线）；PowerToys 我只拿到本机一次性快照；AutoHotkey、Ditto、ShareX、Snipaste、PixPin、Windows 截图工具**没查到可信的托盘态内存数字**（§6）。

## 2. 环境与方法

- 机器：Dell，i5-13500（混合架构），32GB，Intel UHD 770，Win10 19045，release 构建，Rust 1.97.1。
- **环境因素（必须看）**：这台机器有深信服 SNAC，往每个进程注入 `printerhook64`、`printeradapter64`、`csxumd64_21408`、`umppc21408`（见 [`tray-host-split.md` §12.8](../design/tray-host-split.md)）。本文所有数字都含它。§3.1 用 DLL 加载通知把它的开销单独拆了出来。
- **本次会话的硬限制**：测量期间 Windows 会话处于锁屏状态（`OpenInputDesktop` 返回 0，`GetForegroundWindow` 返回 0，`LogonUI.exe` 在跑）。后果：①菜单没拿到前台，**搜狗没有被注入**，§3.4 的矩阵测的是"TSF 相关 DLL"而不是搜狗本身；②`SendInput` 注入不到输入桌面，所以**没法测真实按键→`WM_HOTKEY` 链路**，延迟改用"另一进程 `PostMessage` → 宿主窗口过程"代替（只覆盖唤醒+缺页部分，不含输入栈）。解锁后的复测命令见 §7。
- 探针（一次性，不入仓库）：`<scratchpad>/hostmin`（`windows` crate，可选 `tray-icon 0.25.1` + `global-hotkey 0.8.0` 变体，纯 Win32 变体，子命令 `run / send / spawnbench / launch`）和 `<scratchpad>/mini`（`windows-sys`，约 90 行的最小纯 Win32 宿主，只导入 6 个 DLL）。`scratchpad` = `C:\Users\jelon\AppData\Local\Temp\claude\E--workspaces-Cisox\db6a3f3d-a8af-4d7a-8ed1-b765b6155afa\scratchpad`。
- 数字口径：**私有提交** = `PROCESS_MEMORY_COUNTERS_EX2.PrivateUsage`（任务管理器"提交大小"）；**工作集** = `WorkingSetSize`；**私有工作集** = `PrivateWorkingSetSize`。三者分开写，不混用。每格 n=3~5，括号里是范围。探针自己在报告前先取内存、再枚举模块，避免报告本身污染；两次报告之间堆会略涨，所以只比同一阶段。
- 所有探针进程跑完即退，按 PID 清理；没改系统设置、没写注册表（`SetProcessMitigationPolicy` 只作用于探针进程自己）。

## 3. 实测数据

### 3.1 宿主那 ~2.8MB 提交是从哪来的

三个变体（`ImmDisableIME` 都没调，`--nhk 0` 不注册热键，只看进程骨架）：

| 变体 | 提交 | 工作集 | 线程 | 模块 |
| --- | --- | --- | --- | --- |
| bare：无窗口，只有消息循环 | 1.49MB | 6.5MB | 6 | 25 |
| win：+ 一个隐藏顶层窗口 | 2.75MB | 10.9MB | 9 | 45 |
| pure：+ 托盘图标 + 12 个热键 | 2.80MB | 11.1MB | 9 | 45 |

**托盘图标本身 + 12 个热键合计约 +0.05MB，几乎是零；花钱的是"第一个窗口出现"。** 用 `LdrRegisterDllNotification` 记每个 DLL 加载时的栈和当时提交，窗口创建那一段的归因：

- `user32` 首次建窗载入 `uxtheme`、`shlwapi`、`imm32`（再由 `imm32` 载入 `bcrypt`、`sechost`、`msctf`），这是**系统自带**的部分，逐项相加约 0.25MB。
- 同一时刻 SNAC 的 `printerhook64.dll` 起的链子载入 `wintrust` → `crypt32`（+`msasn1`）→ `cryptsp`/`rsaenh`/`cryptbase` → `imagehlp` → `gpapi`（单个 +624KB）→ `cryptnet` → `advapi32`、`version`、`ws2_32`、`printeradapter64` → `shell32` → `windows.storage` → `shcore`。这条链逐项相加 **约 1.2MB**（口径是加载通知时刻的提交差，不精确），另有 3 个 `printeradapter64` 线程 + 1 个 `crypt32` 线程。
- 线程里还有 4 条起点在 `ntdll` 的（Win10 并行加载器的工作线程），去不掉（§7）。干净机器上线程应是 5 条左右（推断，未测）。

提交构成（VirtualQuery 汇总，pure 变体）：MEM_PRIVATE 约 1.4MB（默认进程堆 0.9MB 已提交/1MB 预留，各线程栈合计约 0.25MB，其余 0.17MB），其余是已写过的镜像页（COW 计费）。`bare` 里默认堆就有 0.95MB，说明堆的大头不是 Rust 引入的（没做 C 程序对照，结论到此为止）。

### 3.2 编译选项（pure 变体，`--nhk 12`，空闲 1.5s，n=5，提交中位数）

| 变体 | 提交（min / 中位 / max） | 模块 | 说明 |
| --- | --- | --- | --- |
| V0 动态 CRT，`panic=abort`，fat LTO（基线） | 2.78 / 2.81 / 2.83 MB | 45 | |
| V1 `+crt-static` | 2.79 / 2.80 / 2.85 | 44 | 少 `vcruntime140`，导入表少 8 项 `api-ms-win-crt-*`；exe +100KB |
| V2 `+crt-static` + `no_main`（绕过 Rust 运行时初始化） | 2.77 / 2.79 / 2.83 | 44 | 无差别 |
| V3 `/STACK:65536,4096` | 2.80 / 2.82 / 2.84 | 45 | 无差别 |
| V4 `panic=unwind` | 2.80 / 2.83 / 2.88 | 45 | 无差别 |
| V5 动态 CRT + `no_main` | 2.78 / 2.82 / 2.86 | 45 | 无差别 |
| V6 `+crt-static` + `opt-level=s` | 2.79 / 2.81 / 2.85 | 44 | 无差别 |

机制上也说得通：官方文档写明线程栈只**提交**可执行头里指定的初始页（默认 reserve 1MB / commit 4KB，`/STACK` 只调这两个数），预留的地址空间不计提交；Rust 的 `std::thread` 默认栈 2MiB 也是预留，且"主线程的栈不由 Rust 决定"。本探针里所有线程栈合计才 ~0.25MB 已提交，没有可压的空间。

### 3.3 后端：`tray-icon`+`global-hotkey` vs 纯 Win32

| 变体 | 提交 | 工作集 | 导入 DLL | 备注 |
| --- | --- | --- | --- | --- |
| `windows` crate 纯 Win32（pure） | 2.82MB | 11.2MB | 15 项（含 `oleaut32`、动态 CRT） | `windows-core` 把 `oleaut32` 带进来了，真宿主用 `windows-sys` 就没有 |
| `tray-icon`+`global-hotkey`（crate） | 2.94MB | 11.6MB | 17 项，**多 `comctl32`、`gdi32`、`bcryptprimitives`** | exe 503KB（pure 310KB） |
| `windows-sys` 最小宿主（mini，静态 CRT） | 2.78~2.88MB 启动 | 11.1MB | **6 项**：`user32 kernel32 shell32 imm32 ntdll` + `api-ms-win-core-synch` | exe 237KB |

crate 版比纯 Win32 多 0.12MB 提交、0.37MB 工作集，一个 `comctl32`；差距很小。叠上 §3.4 的两条缓解之后，crate 动态 CRT 弹菜单后 1.91~2.02MB，crate 静态 CRT 2.02MB，mini 1.86MB——**自写只省 ~0.15MB**。

源码层面（读 `~/.cargo/registry` 里的 `tray-icon-0.25.1`、`muda-0.20.0`、`global-hotkey-0.8.0`，已核对）：

- 依赖：tray-icon → muda、crossbeam-channel、once_cell、thiserror；global-hotkey → crossbeam-channel、keyboard-types、once_cell、thiserror；Windows 侧都是 `windows-sys`。
- 线程：两个库**不常驻线程**；但 `global-hotkey` 每收到一次 `WM_HOTKEY` 会 `thread::spawn` 一个线程，每 50ms `GetAsyncKeyState` 轮询到键松开（为了发 Released 边沿），所以**长按一次热键 = 起一条 Rust 默认 2MiB 预留栈的线程**，松开即退。常驻内存不受影响，但"按住说话"类热键会持续占一条线程。
- muda 暗色菜单：`dark_menu_bar.rs` 运行时 `LoadLibraryA("uxtheme.dll")` 取序号 132（ShouldAppsUseDarkMode）、137（IsDarkModeAllowedForWindow），菜单挂 `SetWindowSubclass`（所以导入 `comctl32`）。`uxtheme` 在宿主里本来就被 `user32` 建窗时载入，所以这几个调用不新增模块。我另外在弹菜单前调了未公开的序号 135（`SetPreferredAppMode`）+ 136（`FlushMenuThemes`）：提交 3.23~3.33MB，不调 3.20~3.34MB，**无可测差别**。
- comctl32 v6：探针的 `tray-icon` 变体即使没有 v6 清单，加载的也是 WinSxS 里的 comctl32 5.82（`...common-controls_..._5.82.19041.6280...`）；加了 v6 清单后载入 6.0.19041.6456，提交 2.86~2.89MB 对 2.92~2.93MB，无显著差别（n=3）。所以"启用 comctl32 v6 会不会涨内存"：不会。pure/mini 变体根本不导入 `comctl32`。
- 隐藏窗口用 `HWND_MESSAGE`（消息专用窗口）：提交持平（2.72~2.79 对 2.75MB），少载入 1 个模块；但官方文档明确写"message-only window ... does not receive broadcast messages"，而托盘图标在资源管理器重启后重新注册靠的是 `TaskbarCreated` 广播，**所以托盘窗口不能用消息专用窗口**。
- `RegisterHotKey` 的 `hWnd` 官方允许传 NULL（`WM_HOTKEY` 投到调用线程队列），不需要窗口也能收热键；但托盘图标需要窗口，所以宿主反正有一个窗口。

### 3.4 IME / TSF / 扩展点矩阵（pure 变体 + 弹一次 20 项原生 HMENU，菜单 1.2s 后 `EndMenu`）

**注意：本表在锁屏下测，菜单没有前台，搜狗没被注入。** 它测的是 TSF 相关系统 DLL 和 SNAC，不是第三方输入法。

| 配置 | 启动提交 | 弹菜单后提交 | 弹菜单后新增模块 | 线程 |
| --- | --- | --- | --- | --- |
| 默认 | 2.80MB | 3.20~3.34MB | `coremessaging coreuicomponents ntmarta textinputframework textshaping wintypes`（+6，模块 51） | 9 |
| `ImmDisableIME(0)`，窗口与调用同线程 | 2.75 | 2.95~2.96 | 只有 `textshaping`（模块 45）；**`msctf` 全程没出现** | 9 |
| `ImmDisableIME(-1)` | 2.76 | 2.96~3.00 | 同上 | 9 |
| `ImmDisableIME(0)` 在主线程调，**窗口建在另一个线程** | 2.86 | 3.34 | 与默认相同（+6，模块 51） | 10 |
| `ImmDisableIME(-1)` 在主线程调，窗口建在**之后创建**的另一个线程 | 2.86 | 3.08 | 同上 `ImmDisableIME(0)` 的结果（模块 45） | 10 |
| `ImmDisableIME(0)` 在窗口**创建之后**才调 | 2.82（`msctf` 已载入） | 3.15 | 只有 `textshaping`（模块 46） | 9 |
| 禁用扩展点（`--extpt`） | **1.64** | 2.12 | 模块 38 | **4** |
| 禁用扩展点 + `ImmDisableIME(0)` | **1.60** | **1.85** | 模块 29 | 3~4 |

结论：

- **`ImmDisableIME(0)` 是线程级的，`-1` 才是进程级**：窗口在别的线程上时 `(0)` 完全不起作用（实测 3.34MB，与默认一样），`-1` 连"调用之后才创建的线程"也覆盖（3.08MB）。官方文档的参数栏是空的，只说"禁用线程或进程内所有线程的 IME"；`-1` 的语义是实测得出（**官方未写明**）。
- **时机**：官方写"必须在第一个顶层窗口收到 `WM_CREATE` 之前调用"。实测创建之后才调也返回 TRUE，并且仍然挡住了菜单期的 `textinputframework` 等 5 个 DLL，但 `msctf` 在启动时已经载入了，所以是"部分有效"，别依赖它。
- **msctf 能否避免**：窗口创建前调 `ImmDisableIME(0/-1)`，启动态和弹菜单后**都没有 `msctf.dll`**。这与 [`tray-host-split.md` §12.2](../design/tray-host-split.md) 里"拦不住 `msctf.dll`（约 +0.2MB）"的观察不一致；那次菜单拿到了前台，这次没有。**差异来自前台与否，未能复现，见 §7。**
- **`uxtheme`、`dwmapi` 为什么被加载**：`uxtheme` 由 `user32` 在第一个窗口创建时载入（DLL 加载通知里的调用链是 `user32 ← win32u ← user32 ← 宿主 exe`），没有应用侧的开关；`dwmapi` 在宿主探针里**根本没有加载**（任何阶段都不在模块列表里）。真 `snow-shot.exe` 导入 `dwmapi` 是因为它链了 GPUI，宿主不会有。
- 禁用扩展点缓解（`ProcessExtensionPointDisablePolicy = 6`，缓冲区 `{ Flags: 1 }`）之所以有效：官方 Defender 文档写它挡 **AppInit DLL、旧式（Legacy）IME、Windows 事件钩子**，`CreateProcess` 属性文档里还写它挡 Winsock LSP 和全局 Windows 钩子；本机 `AppInit_DLLs` 为空，所以 SNAC 走的应该是全局钩子（推断，文件名叫 `printerhook`，未做进一步取证）。副作用实测：`SetWindowsHookEx(WH_MOUSE_LL / WH_KEYBOARD_LL)` 在禁用后**仍然可以安装**（鼠标手势钩子不受影响，只测了安装成功，没测事件投递）；托盘 + 菜单 + 12 个热键正常创建。官方文档要点：Win8+ 可用；"为了最大效果应在进程初始化期间应用"；未写可撤销，本机没试撤销。
- **运行时只测了 `SetProcessMitigationPolicy`**，没有测"由拉起方用 `PROC_THREAD_ATTRIBUTE_MITIGATION_POLICY` 在创建时设置"的路径（宿主由资源管理器/HKCU Run 拉起，没有"拉起方"可用）。

**反例（别做）**：`ProcessSignaturePolicy`（只允许微软签名的二进制）无论是运行时设置还是创建时设置，在本机都让进程卡死（运行时设置后连无窗口的 bare 变体都不再响应；创建时设置的子进程悬在初始化里，私有提交 1.2MB 不动）。很可能是被 SNAC 的未签名钩子线程卡住了，原因没深究；总之此路不通。

### 3.5 工作集 API 的真实效果与对延迟的影响

测法：宿主（pure 变体，12 个热键）等一段时间后，另一进程 `PostMessage` 自定义消息到宿主窗口，宿主在窗口过程里记时间戳后立即 `CreateProcess` 一个空子进程（模拟"收到热键 → 拉起 UI"）。n=6，QPC 计时，时间戳同机同时钟。**这只覆盖"唤醒 + 缺页 + CreateProcess"，不含真实键盘输入栈。**

| 场景 | 唤醒延迟中位（范围） | 处理器内 `CreateProcess` 中位（范围） | 每次热键软缺页 |
| --- | --- | --- | --- |
| A 不修剪 | 0.04ms（0.02~0.06） | 10.2ms（9.4~12.6） | 4（首次 38） |
| B 每次热键前 `EmptyWorkingSet` + 等 1.5s | 0.08ms（0.04~0.08） | 11.8ms（10.1~20.7，其余 5 次 10.1~12.6） | 263~358 |
| C `EcoQoS` 开 + B | 0.05ms（0.05~0.21） | **14.8ms（13.9~16.3）** | 256~359 |
| D `ProcessMemoryPriority = VERY_LOW` + B | 0.10ms（0.04~0.38） | 10.5ms（8.7~12.2） | 255~358 |
| E `SetProcessWorkingSetSizeEx(200KB, 1MB, HARDWS_MAX)` 硬上限，不显式修剪 | 0.06ms（0.05~0.23） | 10.0ms（9.3~14.6） | **624~1012** |

- 提交在所有场景里都不变（2.79~2.86MB），**工作集是唯一会动的数**：B 后工作集 0.46~0.48MB、私有工作集 0.18~0.20MB（对不修剪的 11MB / 2.2MB）。E 的硬上限把工作集卡在 0.9MB 左右。
- `EmptyWorkingSet` 与 `SetProcessWorkingSetSizeEx(-1, -1)` 官方文档写明等价。工作集定义、"软缺页可以不碰后备存储解决"（页在转换状态未被再利用时）也都有官方原文。**无内存压力时被修剪的页留在转换状态，所以都是软缺页**，这就是 B 场景延迟几乎没变的原因；**有内存压力时页会被再利用，换回来要读页文件，硬缺页延迟没测**（§7）。
- EcoQoS（`PROCESS_POWER_THROTTLING_EXECUTION_SPEED`）：调用在 Win10 19045 成功，`CreateProcess` 慢 ~4ms（14.8 对 11.8），唤醒延迟噪声内。官方 QoS 文档的表格把 Eco 级别的发布版本标为 **Windows 11**，"总是选最省电的频率并调度到能效核"；这台机器是 13 代混合架构，数据与"落到能效核/低频"一致，但 Win10 上的语义文档没写。宿主几乎不干活，收益测不出来，却确定会拖慢热键后的 `CreateProcess`，**不建议**。
- `ProcessMemoryPriority`：调用成功，无压力下无可见变化；它是"内存紧张时这些页更先被回收"的提示，反而增加硬缺页风险，**不建议**。
- 硬上限 E：每次热键软缺页 ~640（EmptyWorkingSet 是 ~260），延迟无显著差别，但它是常驻的限制，进程每次醒来都在互相换页，对提交**没有任何帮助**，**不建议**（空闲期的缺页风暴没单独测准——探针自己的报告函数就会制造 ~1.7 万次缺页，污染了数据，所以这里不下结论）。

### 3.6 拉起 UI 子进程的开销

`spawnbench`：`nop` 子进程立即退出，每种方式 n=20，交错执行，两轮（括号里是第二轮）。时间 = `CreateProcessW` 调用本身 / 到子进程退出的总耗时，中位数。

| 方式 | CreateProcess | 到子进程退出 |
| --- | --- | --- |
| Rust `std::process::Command`（`bInheritHandles=TRUE`） | 10.65ms（11.69） | 31.9ms（33.5） |
| 原生 `CreateProcessW`，`bInheritHandles=FALSE` | 10.89（11.84） | 32.3（34.5） |
| 原生，`bInheritHandles=TRUE` | 10.40（11.54） | 32.2（33.5） |
| 原生，`PROC_THREAD_ATTRIBUTE_HANDLE_LIST` | 10.64（11.14） | 32.2（33.9） |
| 原生 + `CREATE_SUSPENDED` + 加入 Job（`KILL_ON_JOB_CLOSE`）+ `ResumeThread` | 10.82（11.48） | 32.6（34.2） |
| 同上，重复测一遍（`jobsusp`，代码路径相同，当噪声参照） | 10.57（11.37） | 32.3（34.8） |

六种方式之间差距 < 0.5ms，两轮之间的漂移（~1ms）比方式之间的差距还大。Job Object 多 ~0.2ms，可以放心用（`KILL_ON_JOB_CLOSE` 让宿主崩了 UI 跟着清掉，要不要这个语义是设计问题）。`spawnbench` 进程（无窗口）自己的提交在 20×6 次拉起前后 1.23 → 1.33MB（+0.1MB，推测是 `Command` 的堆分配没归还，未单独核实）。`CreateProcess` 自己要 ~10ms，其中有多少是 SNAC 钩子 `CreateProcess` 的开销，没法隔离。

## 4. 可执行建议清单（按预期收益排序）

| # | 做什么 | 预期收益 | 依据 |
| --- | --- | --- | --- |
| 1 | 宿主 `main` 的**第一行**调 `SetProcessMitigationPolicy(ProcessExtensionPointDisablePolicy = 6, { Flags: 1 })`，早于创建任何窗口（宿主是单窗口线程，`(0)` 与 `-1` 同效，仍选 `-1` 以覆盖 `shell-tray` 等之后创建的线程）；加回归测试：弹菜单后断言宿主模块里没有 `*.ime` / `textinputframework`，私有提交 ≤ 6MB | 本机实测 弹菜单后 **3.3 → 2.1MB**（`tray-icon` 原样）；其中约 1.1MB 是本机 SNAC，干净机器上的量**未测**；同时对第三方输入法/全局钩子 DLL 注入是官方文档支持的防线 | 实测 + 官方文档 |
| 2 | 同处再调 `ImmDisableIME(-1)`（**不是 0**：托盘窗口在 `shell-tray` 线程，0 管不到），必须在任何窗口创建前 | 在 1 的基础上 2.1 → **1.85MB**（实测用的是同线程 `(0)`；单独用 3.3 → 3.0MB）；挡住 `textinputframework` 等 5 个 TSF 系统 DLL | 实测 |
| 3 | 保留 §7 已有的"启动 +5s、UI 退出 +2s、菜单关闭 +2s 做一次 `EmptyWorkingSet`" | **工作集** 11MB → ~0.4~0.8MB；**不降提交**，验收里两个口径分开写；每次热键多 ~260 次软缺页，唤醒 <0.1ms，`CreateProcess` 无显著变化 | 实测（无内存压力）+ 官方文档 |
| 4 | 宿主依赖用 `windows-sys`，**不要**用 `windows` crate（后者的 `windows-core` 把 `oleaut32` 带进导入表）；导入表闸门白名单可收紧到 `kernel32 user32 shell32 imm32 ntdll + api-ms-win-*`（`gdi32`、`comctl32` 只有用 `tray-icon`/`muda` 才会出现） | 提交几乎无变化，工作集（修剪后）0.77 → 0.20MB（mini 对 crate 变体） | 实测导入表 |
| 5 | 要不要自写托盘/热键：**暂不做**。数据：叠 1+2 后纯 Win32 只比 `tray-icon`+`global-hotkey` 再省 ~0.15MB 提交 | 0.15MB（提交）、少 2 个库依赖 | 实测 |
| 6 | 拉起 UI 用原生 `CreateProcessW`，`bInheritHandles=FALSE`；要不要 Job Object 看"宿主崩了 UI 要不要跟着退"，开销可忽略 | < 0.5ms | 实测 |

建议 1、2 写进 [`tray-host-split.md`](../design/tray-host-split.md) §7"做法"和 §12.9 第 1 条（把"`ImmDisableIME(0)`"改成"`ImmDisableIME(-1)` + 扩展点缓解"，并补一句"`(0)` 是线程级，实测对另一线程的窗口无效"）；那份文档当前由别的代理在改，本文不动它，合并时请用户或后续任务做。

## 5. 不建议做的事及理由

- **改 CRT / `panic` / `/STACK` / `no_main`**：实测 ±0.03MB 噪声内（§3.2）。静态 CRT 唯一的好处是导入表更干净，按 `tray-host-split.md` §3 已经决定用静态 CRT，没问题，但不是为了省内存。
- **用 `SetProcessWorkingSetSizeEx` 硬上限、`ProcessMemoryPriority`、`EcoQoS`**：对提交零帮助，前者每次醒来多 ~640 次缺页，后两者分别有"回收更积极"和"`CreateProcess` 慢 4ms"的确定副作用。官方文档提醒调大工作集会抢走系统物理内存、要慎用；清空（`-1,-1`，即 `EmptyWorkingSet`）是文档里明确描述的做法，也是最温和的一种。
- **`ProcessSignaturePolicy`（仅微软签名）**：本机下进程直接卡死，不可用（§3.4）。
- **消息专用窗口（`HWND_MESSAGE`）当托盘窗口**：收不到 `TaskbarCreated` 广播，资源管理器重启后图标不会重新注册（官方文档）。
- **`HeapCompact` / 默认堆调优**：延续 [`tray-idle-memory.md`](tray-idle-memory.md) §3.2 的结论，官方文档定位为调试用途；本次实测 bare 进程里默认堆就有 0.95MB 已提交，不是 Rust 引入的，没有可压的空间。
- **预载 DLL、常驻预热**：违背"宿主不预载任何 DLL"（`tray-host-split.md` §6 第 3 点）；本次数据也说明系统 DLL 本身就占提交的一半。
- **把去掉 4 条 `ntdll` 加载器工作线程当目标**：没找到应用侧的开关（IFEO `MaxLoaderThreads` 是系统注册表设置，不在应用能控制的范围，也没试）；这几条线程栈提交很小，不值得动。

## 6. 业界同类工具的公开数据

| 工具 | 查到的数据 | 属性 | 来源 |
| --- | --- | --- | --- |
| Everything | 新装 Windows 11（约 25 万文件）约 **35MB** 内存；100 万文件约 **100MB** | 官方 FAQ，已证实；随索引规模走，**不是**纯托盘基线，也是"数据库型"常驻 | [voidtools FAQ](https://www.voidtools.com/faq/) |
| PowerToys | 本机一次性快照（2026-10-12，Win10 19045）：`PowerToys.exe`（Runner）提交 **69.6MB** / 工作集 37.1MB / 39 线程；`PowerToys.Awake` 12.7 / 14.1；`AlwaysOnTop` 21.9 / 6.3；`PowerOCR` 15.5 / 13.6；`FancyZones` 93.2 / 7.2；`ColorPickerUI` 237 / 48 | **本机实测**，非公开数据，只是量级参照 | 本机 `Get-Process` |
| PowerToys | 开发者在 issue 里写 PowerToys Run 提示框打开时约 100MB 提交 | 单条用户评论，是 Run 不是 Runner 空闲 | [issue #17710](https://github.com/microsoft/PowerToys/issues/17710) |
| PowerToys | "关闭不活动的应用"（低内存模式，针对 Text Extractor / Color Picker / Advanced Paste / Peek）PR，默认关；**PR 已于 2026-08-21 关闭、未合并**（无作者回复）。没有量化内存数字 | 已证实（纠正 [`tray-idle-memory.md`](tray-idle-memory.md) §6 里"未能核对是否发布"的说法） | [PR #47487](https://github.com/microsoft/PowerToys/pull/47487)、[feature issue #47518](https://github.com/microsoft/PowerToys/issues/47518) |
| ShareX | 一位用户报告 18.0.1 后台占 1~2GB，每截一张图涨 8~12MB，issue 以 not planned 关闭 | 单条泄漏报告，**不是空闲基线**，只说明截图工具不退图片会涨 | [issue #8097](https://github.com/ShareX/ShareX/issues/8097) |
| 参照：Windows 自带小服务 | 本机 `svchost` 等一批小服务提交 1.3~1.7MB | 本机实测，量级参照：本文宿主的 1.6~1.9MB 与它们在同一档 | 本机 `Get-Process` |
| AutoHotkey v2、Ditto、Snipaste、PixPin、Windows 截图工具 | **未查到可信的托盘态内存数字**。搜索摘要里有"AHK v2 空闲 2.1~3.4MB"，出处是低质量页面，**未证实**，不引用 | — | — |

## 7. 未证实项（以及解锁后怎么补测）

1. **搜狗（或任何第三方 IME）在有前台的菜单上，被"禁用扩展点 + `ImmDisableIME(-1)`"挡住没有**。本会话锁屏，没法复现注入。解锁后：`hostmin.exe run pure --extpt --noime0 --steps r:s@1500,m@1000,r:after@600,x`，把当前输入法切到搜狗，在日志里看模块有没有 `sogou*`、`*.ime`、`ichat*`；对照组去掉两个开关。**旧式 `.ime` 被扩展点缓解挡掉有官方文档支撑；搜狗另有基于 TSF 的组件（`sogoutsf.ime` 等），扩展点缓解是否挡它、`ImmDisableIME` 是否挡它，都只有这次实测才能说。**
2. 干净机器（无 SNAC）上禁用扩展点能省多少。本机的 1.1MB 绝大部分是 SNAC，别外推。
3. 前台菜单下 `msctf.dll` 是否仍被加载（§3.4：本次无前台时没加载，与旧观察不一致）。
4. 真实键盘输入路径的热键延迟，以及修剪后的页在**内存压力下**被再利用后的硬缺页延迟。
5. 启动时禁用扩展点是否可撤销、是否影响某些企业端点软件（只在本机一个环境测过）。
6. `ImmDisableIME(-1)` 的"进程内所有线程（含之后创建的）"语义：官方参数栏为空，仅靠本机实测；Win11 / 其他输入法未验证。
7. 并行加载器的 `ntdll` 工作线程能否在应用侧关闭；`ProcessMemoryPriority` 在内存紧张下的实际回收行为；EcoQoS 在 Win10 上的调度语义。
8. 鼠标低级钩子在"禁用扩展点"后的事件投递（只验证了安装成功）。

## 8. 参考资料

官方文档（均读到正文）：

- [SetProcessMitigationPolicy](https://learn.microsoft.com/en-us/windows/win32/api/processthreadsapi/nf-processthreadsapi-setprocessmitigationpolicy)（"应在进程初始化期间应用"；`ProcessExtensionPointDisablePolicy`、`ProcessSignaturePolicy`）
- [PROCESS_MITIGATION_EXTENSION_POINT_DISABLE_POLICY](https://learn.microsoft.com/en-us/windows/win32/api/winnt/ns-winnt-process_mitigation_extension_point_disable_policy)（`DisableExtensionPoints` 位，Win8+）
- [Exploit protection reference：Disable extension points](https://learn.microsoft.com/en-us/defender-endpoint/exploit-protection-reference)（AppInit DLL / Legacy IME / Windows 事件钩子）
- [UpdateProcThreadAttribute](https://learn.microsoft.com/en-us/windows/win32/api/processthreadsapi/nf-processthreadsapi-updateprocthreadattribute)（`EXTENSION_POINT_DISABLE` 含 AppInit、Winsock LSP、全局钩子、旧式 IME；`BLOCK_NON_MICROSOFT_BINARIES`；`HANDLE_LIST`）
- [ImmDisableIME](https://learn.microsoft.com/en-us/windows/win32/api/immdev/nf-immdev-immdisableime)
- [EmptyWorkingSet](https://learn.microsoft.com/en-us/windows/win32/api/psapi/nf-psapi-emptyworkingset)、[SetProcessWorkingSetSizeEx](https://learn.microsoft.com/en-us/windows/win32/api/memoryapi/nf-memoryapi-setprocessworkingsetsizeex)、[Working Set](https://learn.microsoft.com/en-us/windows/win32/memory/working-set)
- [Quality of Service（EcoQoS）](https://learn.microsoft.com/en-us/windows/win32/procthread/quality-of-service)
- [Thread Stack Size](https://learn.microsoft.com/en-us/windows/win32/procthread/thread-stack-size)、[/STACK](https://learn.microsoft.com/en-us/cpp/build/reference/stack-stack-allocations?view=msvc-170)
- [Window Features：Message-Only Windows](https://learn.microsoft.com/en-us/windows/win32/winmsg/window-features)
- [RegisterHotKey](https://learn.microsoft.com/en-us/windows/win32/api/winuser/nf-winuser-registerhotkey)
- [Rust std::thread 栈大小说明](https://doc.rust-lang.org/std/thread/index.html)、[Rust Reference：静态/动态 C 运行时](https://doc.rust-lang.org/reference/linkage.html)

其他：[voidtools FAQ](https://www.voidtools.com/faq/)、[PowerToys PR #47487](https://github.com/microsoft/PowerToys/pull/47487)、[issue #47518](https://github.com/microsoft/PowerToys/issues/47518)、[issue #17710](https://github.com/microsoft/PowerToys/issues/17710)、[ShareX issue #8097](https://github.com/ShareX/ShareX/issues/8097)。

本仓库/本机源码：`~/.cargo/registry/src/*/{tray-icon-0.25.1,muda-0.20.0,global-hotkey-0.8.0}`（`dark_menu_bar.rs`、`platform_impl/windows/mod.rs`）。

## 9. 探针位置与复现

- 位置：`<scratchpad>/hostmin/`（`src/main.rs`、`run.ps1`、各变体 `t_*` 目标目录）与 `<scratchpad>/mini/`。**不入仓库**，会话结束后随 scratchpad 清理。
- 复现举例（`HM_LOG` 指定日志，`run.ps1 -Exe <exe> -Log <log> -ArgStr "<args>"`）：
  - 基线：`run pure --steps r:s@1500,m@1000,r:after@600,t@600,r:trim@100,x`
  - 两条缓解叠加：`run pure --extpt --noime0 --steps ...`（`--noimeM1` 为进程级；`--wthread --immmain` 测窗口在另一线程）
  - `tray-icon`+`global-hotkey` 变体：`cargo build --release --features crate-variant`，参数里用 `run crate`
  - 拉起方式对比：`spawnbench --n 20`
  - DLL 加载归因：加 `--dllnotif`，步骤里加 `d`
- 清理：所有探针进程跑完即退；被 `ProcessSignaturePolicy` 卡死的进程（运行时设置的 4 个由脚本超时结束，创建时设置的 2 个我按 PID 结束）已清理；临时的热键延迟文件已删；没有系统设置或注册表被修改。
