# 托盘态内存：截图关闭后怎么回到启动水平

> 状态：调研完成，**未实现、未改任何产品代码**；方案待用户拍板。日期 2026-10-12。依据 [`principles.md`](../principles.md) 的优先级链（高性能 > 低内存 > 高 fps > 少编译依赖 > 多用系统能力，不常驻、按需加载）。
> 关联：[`cisox-progress-handoff.md`](../cisox-progress-handoff.md)、[`cisox-gpui-migration-plan.md`](../cisox-gpui-migration-plan.md)（V3 冷启动首帧数据）、[`design/multi-display-overlay.md`](../design/multi-display-overlay.md)（每屏一窗）。
> 资料来源说明：本文先交给 antigravity 做联网调研，两次都没拿到有效内容（第一次 5 分钟超时，第二次只回了"已派发子任务"），之后全部由本人检索官方文档 + 读本仓库与 `~/.cargo` 里的源码完成。官方文档能直接读到的写"已证实"，只有二手摘要或纯推理的写"未证实"。

## 1. 结论（先看这里）

1. **三项里只有"换进程"能保证清干净；进程内的补丁最多清一部分，而且有一项连能不能清都没证据。** 搜狗 IME 的 DLL/线程不会因为窗口关闭而退，微软文档里也找不到"卸载 IME"的接口；驱动里的两份整屏副本和着色器缓存，官方文档只承诺 `Trim` 会丢"内部缓冲"，没有承诺能清上传暂存，实测也没清掉。
2. **UIAutomation 这一项找到了明确的代码根因，不是输入法引起的**：GPUI 每开一个窗口都会创建 AccessKit 适配器，适配器构造函数里无条件调 `UiaLookupId` 强行初始化 UIA（见 §3.1）。这条可以用一个 vendor 小补丁去掉，零延迟代价。能省多少没有测过，要先做 §3.1 的 A/B。
3. **推荐落地顺序**：先做两个便宜的定量实验（拆开三项各占多少），再做"托盘宿主 + 按需 UI 进程，最后一个窗口关闭 N 秒后 UI 进程退出"的拆分。中间可选的"空闲销毁 D3D 设备"只能省第 2、3 项约 38MB，不碰第 1 项，价值取决于实验结果，不建议先做。
4. **没有实测数据的地方，本文只给估算并标明**：设备重建耗时、各项补丁的实际节省、业界同类工具托盘内存，公开资料都没查到可引用的数字。

## 2. 背景与实测

环境：release，i5-13500，Intel UHD 770（驱动 31.0.101.4953），双屏 2560×1440，Win10 19045。

| 状态 | 私有提交 | 工作集 | 私有工作集 |
| --- | --- | --- | --- |
| 启动后空闲（目标） | 56.8 MB | 61.5 MB | 32.7 MB |
| 截图一次并关闭后 | 134~136 MB | — | — |

多出来的约 78MB 里，约 35MB 的 GPUI D3D 状态滞留已修掉（`directx_renderer.rs` 里 `ClearState` + `Flush` + `IDXGIDevice3::Trim`），剩下约 70MB 三项：

| # | 现象 | 量级 | 备注 |
| --- | --- | --- | --- |
| 1 | 输入法 / UIAutomation 注入：搜狗拼音（`SogouPY.ime`、`ichat_bundle64.dll`、`isgpet_bundle64.dll`）、TSF/IMM、`uiautomationcore` 进进程，窗口关了也不退 | ~33MB 堆 + 十来个线程 | 文字标注、翻译输入框要中文输入，不能简单关掉 |
| 2 | 驱动里残留两份整屏 BGRA（每屏 14MB） | ~28MB | 不在进程堆里；`ClearState`+`Flush`+`Trim` 后还在 |
| 3 | 驱动着色器缓存 | ~10MB | 设备存续期间常驻 |

冷启动到截图首帧约 1.14s，常驻热路径约 0.57s；冷启动里托盘/热键初始化约 0.3s。

**本次读代码得到的几个事实**（对方案有直接影响）：

- `uiautomationcore.dll`、`imm32.dll`、`d3d11.dll`、`dxgi.dll` 都是 `snow-shot.exe` 的**静态导入**（用脚本解析了 `build/cargo/release/snow-shot.exe` 的导入表，无延迟导入）。所以"DLL 被加载"本身从进程启动就发生了，截图时多出来的 33MB 更可能是 UIA / IME 的**初始化与线程**，而不是首次映射 DLL。这与"第一个覆盖窗获焦点时才注入"的观察不矛盾，但说明"卸载 uiautomationcore"这个目标本身不成立（静态导入的 DLL 不能卸载）。
- vendor 里 GPUI 已经有按需开关 IME 的逻辑：`events.rs::update_ime_enabled`，输入处理器不接受文本时调 `ImmAssociateContextEx(hwnd, NULL, 0)` 解除关联，需要时 `IACE_DEFAULT` 恢复。所以"只在需要文字输入时关联 IME 上下文"**已经在做**，而 IME DLL/线程仍然留着，说明这一招挡不住加载（见 §3.2）。
- 着色器已经是构建期用 fxc 预编译的 DXBC（`vendor/gpui-pre-windows-0.3.7/build.rs`），不存在"换成预编译"这个选项。
- `platform.rs` 已有设备丢失恢复路径：`handle_gpu_device_lost` 重建 `DirectXDevices`，再通知窗口和 `DirectWriteTextSystem::handle_gpu_lost`。"空闲销毁设备"可以借这条路，但注意 DirectWrite 文字系统和图集也持有设备，不止渲染器一处；该路径里还有一个固定 350ms 的等待，是为系统恢复设计的，复用时要去掉。
- 用户已有"独立进程 + 命名管道"的先例（录制 worker `snow-recorder`、MCP 管道），进程拆分不需要从零发明 IPC。

## 3. 第 1 项：输入法 / UIAutomation

### 3.1 UIAutomation：为什么会被初始化

**已证实（读源码）**：`vendor/gpui-pre-windows-0.3.7/src/window.rs::a11y_init` 在每个窗口创建时无条件调用 `accesskit_windows::Adapter::new`；该函数（`accesskit_windows-0.34.0/src/adapter.rs`）内部调 `init_uia()`，实现就是一次 `UiaLookupId(...)`，注释写明目的是"强制 UIA 初始化"。也就是说，**只要开了窗口，UIA 就被拉起来，跟有没有读屏软件、有没有输入法无关。** 窗口收到 `WM_GETOBJECT` 时才会真正创建 UIA 提供者树（官方 [WM_GETOBJECT](https://learn.microsoft.com/en-us/windows/win32/winauto/wm-getobject) 说明）。

**未证实**：UIA 初始化到底占多少堆、多少线程。搜狗这类输入法也可能自己用 UIA 取光标位置，所以 UIA 与 IME 两部分各占多少，要 A/B 才知道。

| 候选 | 做法 | 优点 | 缺点 | 对首帧延迟 | 成本 / 风险 |
| --- | --- | --- | --- | --- | --- |
| A 跳过 | vendor 里把 `a11y_init` 改成空实现 | 最彻底，UIA 不再被主动初始化 | 覆盖窗对读屏软件不可见（截图选区本来就是纯视觉操作，影响小，但要在文档里写明） | 0 | 极低；改 vendor 须先读 `vendor/README.md` 并登记补丁，不属于"升级 gpui" |
| B 懒创建 | `a11y_init` 只存回调；第一次 `WM_GETOBJECT`（`UiaRootObjectId` / `OBJID_CLIENT`）到来再 `Adapter::new` | 读屏软件仍可用 | 第一次被 UIA 客户端探测（输入法、读屏）就会初始化，若搜狗会探测则白做 | 0，首次探测时多一次初始化 | 低~中；适配器要在 `WM_GETOBJECT` 之前存在，需要确认 AccessKit 对"晚创建"的容忍度（未验证） |
| C 不管 | 保持现状 | 无 | 内存留着 | 0 | 无 |

**推荐 A**（若实验显示 UIA 确实占大头），读屏支持作为"覆盖窗不支持辅助功能"写进已知限制；B 作为备选。

### 3.2 IME / TSF：能不能只在需要输入时才激活

先厘清官方文档能证实什么：

| 接口 | 文档说的 | 对本项目意味着什么 |
| --- | --- | --- |
| [`ImmDisableIME(idThread)`](https://learn.microsoft.com/en-us/windows/win32/api/immdev/nf-immdev-immdisableime) | 禁用线程（或传 -1 禁用进程内所有线程）的 IME。**必须在该线程第一个顶层窗口收到 `WM_CREATE` 之前调用**（创建窗口前，或第一个窗口的 `WM_NCCREATE` 里） | 只能"创建窗口前一次性决定"，**不能运行时再打开**。GPUI 所有窗口共用一个主线程，所以它只适用于"确定整条线程永远不需要输入法"的线程 |
| [`ImmDisableTextFrameService`](https://learn.microsoft.com/en-us/windows/win32/api/imm/nf-imm-immdisabletextframeservice) | Vista 起不可用，改用 `ImmDisableIME` | 不用考虑 |
| [`ImmAssociateContextEx`](https://learn.microsoft.com/en-us/windows/win32/api/imm/nf-imm-immassociatecontextex) | 改变窗口与输入上下文的关联；`IACE_DEFAULT` 恢复默认上下文 | 文档没写"传 NULL 即禁用"。旧版 `ImmAssociateContext` 的 [Windows CE 文档](https://learn.microsoft.com/en-us/previous-versions/windows/embedded/ms905984(v=msdn.10)) 写了 NULL 解除关联，桌面版沿用此语义（GPUI 已这么用）。**它解除的是窗口与上下文的关联，文档没承诺卸载 IME DLL** |
| [`ITfThreadMgr::Deactivate`](https://learn.microsoft.com/en-us/windows/win32/api/msctf/nf-msctf-itfthreadmgr-deactivate) | 反激活调用线程的 TSF，必须与同线程的 `Activate` 配对 | 只对自己激活了 TSF 的线程有意义；Windows 自己为窗口线程挂的 TSF 不由我们 `Activate`，我们也没法替它 `Deactivate`（推断，未证实） |

可知的结论：

- **没有任何官方接口"卸载已加载的输入法 DLL"。** 查到的所有接口要么是创建窗口前禁用，要么是解除窗口关联。
- GPUI 的 `update_ime_enabled` 已经做了"不需要时解除关联"，实测 Sogou 的 DLL 和线程依然在，所以**窗口级关联这一招没用**（已证实：现状即证据）。
- 线程级释放：把窗口放到独立线程、线程退出时 TSF/IMM 的线程级资源是否释放——官方文档没有明确描述。WPF 的 `TextServicesContext` 有"不调用 Deactivate 会泄漏 IME"的注释，这是搜索摘要里的转述，**没能直接读到源码，未证实**。况且 GPUI 的窗口必须在主 UI 线程，没法把覆盖窗挪到可退出的工作线程，这条在工程上也走不通。

候选方案：

| 候选 | 做法 | 优点 | 缺点 | 对首帧延迟 | 成本 / 风险 |
| --- | --- | --- | --- | --- | --- |
| A `ImmDisableIME(0)` 用在"永不输入"的线程 | 托盘、热键、录制监控等有窗口的服务线程创建窗口前调用 | 这些线程不再触发 IME 初始化，官方支持 | 对主 UI 线程不能用（文字标注要输入法）；**是否真的拦住了搜狗加载，未证实** | 0 | 低；先做实验确认服务线程是不是注入源之一 |
| B 主线程 `ImmDisableIME`，输入类窗口放另一进程 | 覆盖窗禁用 IME，文字标注/翻译输入框交给独立进程 | 覆盖窗进程干净 | 文字标注与画布紧耦合，拆出去代价极高 | 输入框要多一次进程启动 | 高，不建议 |
| C 进程拆分 | 见 §6 | 进程退出时 IME、UIA 线程、堆全部回收，**唯一确定有效** | 需要重构，见 §6 | 见 §6 | 高 |
| D 工作集收缩（`EmptyWorkingSet` / `SetProcessWorkingSetSizeEx(-1,-1)`） | 关窗后调用 | 把"私有工作集"压下去 | 见下 | 下一次热键要把页再缺页换回，延迟未测 | 低，但只是障眼法 |
| E `HeapCompact` | 关窗后调用 | — | 见下 | — | 不推荐 |

关于 D / E，把"私有提交"和"私有工作集"分开讲清楚：

- **私有提交（Private Bytes / commit）**：进程已提交的私有虚拟内存，计入系统提交上限；**`EmptyWorkingSet` 不会减少它**。工作集的定义是"当前驻留在物理内存里的页"（官方 [SetProcessWorkingSetSizeEx](https://learn.microsoft.com/en-us/windows/win32/api/memoryapi/nf-memoryapi-setprocessworkingsetsizeex) 说明），收缩只是把页踢出物理内存，提交量不变。
- **私有工作集**：任务管理器"内存"列看的是它。`EmptyWorkingSet` 能把它压得很低，但页是被换出，不是释放；一次热键或定时器就会再缺页换回来，等于把成本推迟到下次截图的首帧。
- **`HeapCompact`**：官方文档写"主要用于调试"，"通常系统在每次 `HeapFree` 时已经合并堆"，调用只返回最大空闲块大小（见 [HeapCompact](https://learn.microsoft.com/en-us/windows/win32/api/heapapi/nf-heapapi-heapcompact)）。对 33MB 堆没有归还作用，不推荐。`HeapSetInformation` 本次没查到任何"归还提交"的用法，同样不列入候选。

**推荐**：不要指望进程内手段清 IME。做 A 的小实验（服务线程 `ImmDisableIME(0)` 是否改变注入情况）和 §3.1 的 A，之后用 C 兜底。D 只作为"托盘态展示数字好看"的可选末位手段，且必须在文档写明它是工作集而不是提交。

### 3.3 最小验证实验（拆分第 1 项）

用 release 构建，每组跑 3 次，截图→关闭→等 10s→取数；取数用 `Get-Process` 的 `PrivateMemorySize64`（提交）、`WorkingSet64`，perfmon 的 `\Process(snow-shot)\Working Set - Private`，`tasklist /m` 或 Sysinternals ListDLLs 看模块，`Get-Process | % Threads` 数线程，VMMap 看堆。

| 组 | 改动 | 看什么 | 判据 |
| --- | --- | --- | --- |
| 0 | 基线 | 模块列表、线程数、提交 | 复现 134~136MB |
| 1 | 仅 `a11y_init` 空实现 | 提交下降量、线程数 | 降 ≥10MB 才值得保留补丁；降不动说明 UIA 不是主因 |
| 2 | 仅服务线程 `ImmDisableIME(0)` | 搜狗 DLL 是否还出现 | 模块不再出现 = 服务线程是注入源之一 |
| 3 | 1+2 | 同上 | 合计下降量 |
| 4 | 换成微软拼音输入法重复组 0 | 提交 | 与搜狗差多少 = 第三方 IME 自己的开销，不是我们能优化的 |

## 4. 第 2 项：驱动里的两份整屏 BGRA 副本

### 4.1 机制

- 官方 [`UpdateSubresource`](https://learn.microsoft.com/en-us/windows/win32/api/d3d11/nf-d3d11-id3d11devicecontext-updatesubresource) 明确写：返回时数据**已经被拷贝/快照走**；有资源争用时会做两次拷贝，先 CPU 拷到"命令缓冲可访问的临时存储"，GPU 稍后再拷进最终的不可映射内存；无争用时由驱动选更快的方式。**文档没有说这份临时存储何时释放。**
- 官方 [`IDXGIDevice3::Trim`](https://learn.microsoft.com/en-us/windows/win32/api/dxgi1_3/nf-dxgi1_3-idxgidevice3-trim)：丢弃驱动为应用分配的"内部缓冲"；应在**最后一批 D3D 操作**里、`ClearState` 之后调用；Direct3D 平时会**延迟销毁**对象，`Trim` 强制立即销毁，因此"在 `Trim` 之后再释放最后一个引用，不保证内存马上归还"。Chromium 的 Dawn 也是这么用的：先 `Flush` 再 `Trim`（[Dawn 提交](https://dawn.googlesource.com/dawn/+/427218ec02b33b6b7bf8def14c1235bcff2b243e%5E%21)）。**已有实现已按这个顺序做，副本仍在，说明它不在 `Trim` 的作用范围内，或者根本不是暂存副本。**
- **一个需要先排除的替代假设（未证实）**：UHD 770 是核显，DEFAULT 纹理本身就落在共享系统内存里。"两份 14MB、内容与截图一致"也可能就是**那两张纹理本身**还没被销毁（上面 `Trim` 文档说销毁是延迟的；或者还有别的引用，如 DWM/合成路径、我们自己没释放干净的视图）。这两种假设的处理方式完全不同，必须先分清。

### 4.2 候选方案

| 候选 | 做法 | 优点 | 缺点 / 未证实点 | 对首帧延迟 | 成本 |
| --- | --- | --- | --- | --- | --- |
| A `CreateTexture2D` 带初始数据 | 创建时传 `D3D11_SUBRESOURCE_DATA`，不再 `UpdateSubresource` | 少一次调用 | 文档没说驱动是否因此少留一份副本，**未证实** | 同现状 | 低，要读 vendor 里 `directx_atlas`/渲染器的上传点 |
| B STAGING 纹理 + `CopyResource` | 建 STAGING（CPU 写），`Map` 写入，复制到 DEFAULT，**立刻释放 STAGING** | 暂存生命周期由我们控制，释放后是否归还可观测 | 多一次 GPU 拷贝（14MB 级，估算 <1ms）；驱动归还时机**未证实** | 估算 +1~3ms/屏 | 中 |
| C DYNAMIC + `Map(WRITE_DISCARD)` | 纹理用 DYNAMIC，直接映射写入 | 官方说明 DYNAMIC 不能作 `UpdateSubresource` 目标，走 Map 是正路；无需额外 CPU 暂存 | 驱动对 DISCARD 可能"换新缓冲"，短期内多份并存；整屏图只上传一次，DYNAMIC 纹理用于只读显示不是典型场景；**未证实** | 估算持平 | 中 |
| D `DiscardResource` / `DiscardView` | 释放前调用 | 官方：告诉 GPU 内容不再需要（[DiscardResource](https://learn.microsoft.com/en-us/windows/win32/api/d3d11_1/nf-d3d11_1-id3d11devicecontext1-discardresource)） | **文档没提释放内存**，只是内容失效提示；不期望有效 | 0 | 低，可顺手试 |
| E 空闲销毁整个 D3D 设备 | 见 §5 | 驱动里挂在设备上的一切随设备走，最可能有效 | 需要重建设备 + 文字系统 | 见 §5 | 中高 |
| F 把整屏图拆成多张小纹理（瓦片）逐张上传释放 | 降低单次暂存峰值 | — | 不改变"驱动里是否留副本"，只改单块大小；**未证实有益** | 略增 | 中，不推荐 |

### 4.3 最小验证实验

目标：判断"驱动暂存副本"还是"纹理没真销毁"，再决定走哪个候选。

1. 写一个独立小程序（放 `spikes/` 或 scratchpad，不入产品代码），只做 D3D11 设备 + 两张 2560×1440 BGRA DEFAULT 纹理。
2. 变量：上传方式 ∈ {无上传，`UpdateSubresource`，初始数据，STAGING+Copy，DYNAMIC+Map}；释放后 ∈ {直接 Release，ClearState+Flush+Release，再加 Trim，再加销毁设备}。
3. 每格记录：进程私有提交、perfmon `\GPU Process Memory(pid_*)\Shared Usage` / `Dedicated Usage`（计数器集本次没去核对官方页，使用前先在本机 `Get-Counter -ListSet "GPU Process Memory"` 确认存在）。
4. 判据：
   - "无上传"组释放后无残留、"UpdateSubresource"组有残留 → 是上传暂存，选 B/C 里残留为零的那个。
   - 两组都有残留 → 是纹理没销毁或驱动本身，转向 E。
   - 所有格都清得掉但产品里清不掉 → 产品里有没释放的引用，查引用（图集、视图、`DirectWrite` 文字系统共用设备）。
5. 另测一次 NVIDIA 机器，驱动差异不要拿 Intel 的结果外推（项目原则：不同 GPU 的数据不直接比较）。

## 5. 第 3 项：着色器缓存与"销毁设备"

- 本项目着色器是**构建期预编译的 DXBC**（vendor `build.rs`），所以"预编译字节码来减少缓存"没有可做的空间。DXBC 仍然要由驱动在 `CreateVertexShader/CreatePixelShader` 时翻译成 GPU 指令；驱动侧缓存是翻译后的结果，与我们交的是不是预编译字节码关系不大（推理，**未证实**）。
- Windows 层有磁盘级 D3D 着色器缓存 `%LOCALAPPDATA%\D3DSCache`（事件提供者 [Microsoft-Windows-Direct3DShaderCache](https://winevt-kb.readthedocs.io/en/latest/sources/eventlog-providers/Provider-Microsoft-Windows-Direct3DShaderCache.html)，二手来源）。那是磁盘缓存，与进程里的 ~10MB 驻留不是同一个东西；**没有查到任何 API 能让驱动在空闲时释放它的内存缓存**（Intel 驱动行为也没有公开文档，未证实）。
- 所以第 3 项的实质选择是：**销毁设备，或接受这 10MB。**

**空闲销毁设备的候选**：

| 候选 | 做法 | 优点 | 缺点 | 延迟 | 成本 / 风险 |
| --- | --- | --- | --- | --- | --- |
| A 复用设备丢失恢复路径 | 空闲 N 秒后主动走 `handle_gpu_device_lost` 的重建流程（去掉 350ms 等待），关窗时释放旧设备 | 重建逻辑已存在、已被设计成"文字系统/窗口都跟着换设备" | 那条路径设计目标是"设备已坏"，主动触发要确认旧设备引用是否真能全部放掉（图集、`DirectWrite`、各窗口渲染器）；重建后文字图集要重新栅格化 | 设备创建 + 文字系统重建；**没有可引用的公开数字**（ANGLE 的 [EGLInitializePerf](https://chromium.googlesource.com/angle/angle.git/+/HEAD/src/tests/perf_tests/EGLInitializePerf.cpp) 里有 `createDeviceMS` 的测量项，但没找到具体毫秒值），要本机实测；GPUI 冷启动总计 750~960ms，设备重建只占其中一部分，**上界估算 <0.5s，未测** | 中高，改 vendor |
| B 不销毁，只 Trim | 现状 | 无风险 | 清不掉这 38MB | 0 | — |
| C 进程拆分 | 见 §6 | 同时解决三项 | 重构 | 见 §6 | 高 |

**推荐**：先用 §4.3 的小程序量出"销毁设备能不能真把 38MB 清掉、设备重建多少毫秒"。数据好再决定 A；如果最终要做 §6，A 就没有必要单独做（进程退出时自然全部回收）。

## 6. 总体架构备选

目标态：**托盘宿主进程只做 Win32：托盘图标、全局热键、配置、必要时的唤起；GPUI/D3D/IME/UIA 全在按需启动的 UI 进程里，最后一个窗口（覆盖窗、贴图、设置页、翻译浮窗……）关闭后等 N 秒退出。**

| 方案 | 说明 | 托盘态内存（估算） | 热键→首帧 | 成本 / 风险 |
| --- | --- | --- | --- | --- |
| 现状 | 单进程常驻，GPUI 全程在 | 截图后 134~136MB；开机空闲 56.8MB | 热路径约 0.57s | 无 |
| P0 空闲销毁设备 | §5 方案 A | 约省 38MB（上界，未证实），IME/UIA 部分不变 | 首次空闲后多 <0.5s（估算） | 中高 |
| **P1 托盘宿主 + 按需 UI 进程（含 N 秒保活）** | 宿主 Win32 常驻；热键时若 UI 进程在就复用，不在就拉起；N 秒无窗口则退出 | 宿主远低于 56.8MB（纯 Win32 托盘通常是个位数到十几 MB，**估算，没有本项目实测**）；UI 进程退出后三项全部清零 | 保活期内 ≈0.57s；冷拉起 ≈1.14s 减去宿主已做完的 ~0.3s 托盘/热键初始化 ≈ **0.84s**（估算，按用户给的两个实测数相减） | 高：app_runtime 里热键、历史、贴图、设置、翻译等混在一处，要划清边界；贴图窗口是 UI 进程的窗口，有贴图时 UI 进程不能退 |
| P2 每次截图短命子进程（N=0） | 截完就退，宿主不碰 GPU | 同 P1 | 每次都 ≈0.84s | 同 P1，但没有保活收益，体验更差 |
| P3 宿主先采集再交给 UI 进程 | 宿主在拉起 UI 进程的同时做 GDI/WGC 采集，通过共享内存/命名管道交图 | 采集期间宿主临时多出 ~29MB（两屏整图），交完即释放 | 采集 170~260ms（V3 数据）与 UI 进程冷启动并行，可能再省 ~0.2s（估算，需要实测并行度） | 高；采集失败的边界更多（跨进程图像所有权、HDR/旋转屏回退） |
| P4 预热备用 UI 进程 | 平时留一个隐藏的 UI 进程 | 起码保持 ~57MB，**违背目标** | ≈0.57s | 不建议 |

P1 的几个必须想清楚的问题：

1. UI 进程退出后，**贴图/设置页/翻译浮窗**怎么办：它们本来就在这个进程里，保活条件应是"没有任何窗口 + 空闲 N 秒"，不是"没有覆盖窗"。
2. 全局热键注册在宿主，触发后要把"启动参数 + 触发类型"带给 UI 进程；已有命名管道和 MCP 管道的先例可复用。
3. 剪贴板、历史库、配置的写入归属要定一个唯一写者，避免两个进程同时写。
4. 配置变更（设置页改热键）需要宿主重新注册热键，走 IPC。
5. 文档先行：这是架构变更，进入实施前要单独出设计文档并评审，本文只做选型调研。

**业界同类工具**（公开资料很少，不编数字）：

- PowerToys 已经在做同一件事：PR [#47487](https://github.com/microsoft/PowerToys/pull/47487) 给 Text Extractor、Color Picker、Advanced Paste、Peek 加"空闲时关闭应用"（原名"低内存模式"），默认仍保持常驻，开关说明写的是"不用时关闭以省内存，可能会打开得更慢"。信息来自搜索摘要，**没能打开 PR 原文核对细节与是否已发布，未证实**。这个设计与 P1 同构（宿主常驻、辅助进程按需退出），可作为"用户愿意用延迟换内存"的旁证。
- ShareX、Snipaste、PixPin、Flameshot、Windows 截图工具：**没有查到可引用的托盘态内存公开数据**，也没有看到它们对"空闲销毁 GPU 设备/IME"的公开说明。不据此下结论，需要的话在本机装几个实测一次。

## 7. 推荐落地顺序

### 阶段 0：定量（不改产品代码，约半天）

- 做 §3.3 的 5 组 A/B（临时分支改 vendor 与服务线程，不提交）与 §4.3 的独立小程序。
- 验收判据：产出一张表，三项里每项"可进程内清除的量"和"必须换进程才能清的量"，并给出设备重建毫秒数（取 3 次中位数，记录机器、GPU、驱动版本、刷新率）。
- 禁止：不得拿 Intel 的结果推 NVIDIA；不得用 `EmptyWorkingSet` 后的数字当"内存降了"；不在上游数据目录读写。

### 阶段 1：低风险补丁（若阶段 0 显示有收益）

- vendor 的 `a11y_init` 改空实现（或懒创建），登记到 `vendor/README.md`；服务线程 `ImmDisableIME(0)`（仅当阶段 0 组 2 证明有效）。
- 验收：`cargo test -p workspace-guard`、相关 crate 测试通过；热键→首帧延迟不变（±5%）；提交量下降达到阶段 0 的预期。
- 禁止：不升级 gpui；不在 `snow-ui-shell` 之外出现 `gpui::`；覆盖窗的读屏退化要写进已知限制，不得悄悄去掉。

### 阶段 2：空闲销毁设备（可选，仅当阶段 0 证明设备销毁能清掉 38MB 且重建延迟可接受，并且阶段 3 短期内不做）

- 验收：空闲 N 秒后私有提交回到 ≤ 阶段 0 预测值；再次热键首帧 ≤ 冷启动首帧；设备重建全程无黑屏、无文字缺失（文字图集重建后重栅格化）。
- 禁止：不得无限重试重建；设备丢失与主动销毁的路径要分开打日志。

### 阶段 3：托盘宿主 + 按需 UI 进程（P1）

- 先出设计文档（进程边界、IPC 协议、保活策略、贴图归属、热键注册、写者唯一性），评审后实施。
- 验收：关闭全部窗口并过 N 秒后，宿主进程私有提交 ≤ 启动空闲值（目标越低越好，以阶段 0 数据定数值）；冷拉起热键→首帧不超过现状冷启动首帧 1.14s；保活期内热键→首帧不超过现状热路径 0.57s 的 10%；全套录制 / MCP / 贴图回归通过。
- 禁止：不得为省延迟引入常驻预热进程（P4）；不得把 UI 进程的退出做成强杀（要走正常关窗，让历史库与配置落盘）。

## 8. 预期收益与延迟代价（估算汇总）

下表数字均为**估算**，除标注"实测"外没有本项目的实测支撑。

| 措施 | 预期省 | 增加的热键→首帧延迟 | 把握 |
| --- | --- | --- | --- |
| 跳过 / 懒创建 AccessKit 适配器 | 0~一部分第 1 项，**量未知** | 0 | 根因已证实，节省量未证实 |
| 服务线程 `ImmDisableIME(0)` | 0~一部分第 1 项，**量未知** | 0 | 是否有效未证实 |
| `EmptyWorkingSet` | 只降私有工作集，提交 0 | 下次热键缺页，量未测 | 官方语义已证实 |
| `HeapCompact` | 0 | 0 | 官方文档不推荐 |
| 换上传方式（A~D） | 0~28MB（取决于是暂存还是纹理没销毁） | 估算 0~3ms/屏 | 未证实，实验决定 |
| 空闲销毁 D3D 设备 | 上界约 38MB（第 2+3 项） | 空闲后首次估算 <0.5s | 未证实 |
| P1 托盘宿主 + 按需 UI 进程 | 上界约 71MB（三项全部）；宿主本身比 56.8MB 低多少未测 | 冷拉起约 0.84s，保活期内约 0（估算） | 机制确定；数值未测 |

## 9. 未证实清单

1. UIA 初始化与 IME 各占那 33MB 的多少。
2. `ImmDisableIME(0)` 用于服务线程是否能拦住搜狗加载。
3. 驱动里的 28MB 是上传暂存副本还是纹理本身未销毁；Intel / NVIDIA 行为差异。
4. 设备销毁能否真清掉着色器缓存与上述 28MB；设备 + 文字系统重建的毫秒数。
5. 线程退出后 TSF/IMM 线程级资源是否释放（只有二手转述）。
6. PowerToys 低内存模式的具体实现与发布状态；ShareX / Snipaste / PixPin / Flameshot / Windows 截图工具的托盘态内存。
7. AccessKit 适配器"晚创建"（懒创建）是否安全。
8. perfmon `GPU Process Memory` 计数器集在本机是否可用（使用前先 `Get-Counter -ListSet` 确认）。

## 10. 参考资料

官方文档（均已读到正文）：

- [ImmDisableIME](https://learn.microsoft.com/en-us/windows/win32/api/immdev/nf-immdev-immdisableime)
- [ImmDisableTextFrameService（已弃用）](https://learn.microsoft.com/en-us/windows/win32/api/imm/nf-imm-immdisabletextframeservice)
- [ImmAssociateContextEx](https://learn.microsoft.com/en-us/windows/win32/api/imm/nf-imm-immassociatecontextex)
- [ImmAssociateContext（Windows CE 文档，NULL 语义）](https://learn.microsoft.com/en-us/previous-versions/windows/embedded/ms905984(v=msdn.10))
- [ITfThreadMgr::Deactivate](https://learn.microsoft.com/en-us/windows/win32/api/msctf/nf-msctf-itfthreadmgr-deactivate)（仅读到搜索摘要）
- [WM_GETOBJECT](https://learn.microsoft.com/en-us/windows/win32/winauto/wm-getobject)（仅读到搜索摘要）
- [ID3D11DeviceContext::UpdateSubresource](https://learn.microsoft.com/en-us/windows/win32/api/d3d11/nf-d3d11-id3d11devicecontext-updatesubresource)
- [IDXGIDevice3::Trim](https://learn.microsoft.com/en-us/windows/win32/api/dxgi1_3/nf-dxgi1_3-idxgidevice3-trim)
- [ID3D11DeviceContext1::DiscardResource](https://learn.microsoft.com/en-us/windows/win32/api/d3d11_1/nf-d3d11_1-id3d11devicecontext1-discardresource)
- [SetProcessWorkingSetSizeEx](https://learn.microsoft.com/en-us/windows/win32/api/memoryapi/nf-memoryapi-setprocessworkingsetsizeex)
- [HeapCompact](https://learn.microsoft.com/en-us/windows/win32/api/heapapi/nf-heapapi-heapcompact)

其他来源：

- [Dawn：D3D11 `ReduceMemoryUsageImpl` 先 Flush 再 Trim](https://dawn.googlesource.com/dawn/+/427218ec02b33b6b7bf8def14c1235bcff2b243e%5E%21)（搜索摘要）
- [ANGLE EGLInitializePerf（含 createDeviceMS 测量项，无具体数值）](https://chromium.googlesource.com/angle/angle.git/+/HEAD/src/tests/perf_tests/EGLInitializePerf.cpp)
- [PowerToys PR #47487：close apps when inactive](https://github.com/microsoft/PowerToys/pull/47487)（搜索摘要）
- [Direct3DShaderCache 事件提供者（D3DSCache 位置）](https://winevt-kb.readthedocs.io/en/latest/sources/eventlog-providers/Provider-Microsoft-Windows-Direct3DShaderCache.html)（二手）

本仓库源码（本次直接读过）：

- `snow-shot-rs/vendor/gpui-pre-windows-0.3.7/src/window.rs`（`a11y_init`）、`events.rs`（`update_ime_enabled`、`handle_wm_getobject`）、`platform.rs`（`handle_gpu_device_lost`）、`directx_renderer.rs`（`ClearState` / `Trim`）、`build.rs`（fxc 预编译）
- `~/.cargo/registry/src/*/accesskit_windows-0.34.0/src/adapter.rs`（`Adapter::new` → `init_uia` → `UiaLookupId`）
- 冷启动 / 热路径数据：用户给定实测，以及 [`cisox-gpui-migration-plan.md`](../cisox-gpui-migration-plan.md) V3 行（采集 + 像素转换 170~260ms）
