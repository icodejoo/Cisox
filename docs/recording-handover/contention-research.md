# 录屏采集被设备锁卡住：替代/叠加方案调研

范围：只读调研，未跑任何 GPU 程序。证据标记：**[源码]** 逐行读了代码；**[文档]** 读了微软文档原文；**[推测]** 我的推理，未证实；**[未读]** 没读到。
源码副本在 `scratchpad\research\cont\`（Sunshine、OBS 的 raw 文件）与 `scratchpad\research\FFmpeg-n8.0.1\`。

## 0. 先说结论

1. **E2 的"采集独占设备 + 合成/编码另一个设备 + 共享纹理"不是我们的独创怪招，而是 Sunshine 的现成做法。** Sunshine 捕获设备与编码设备分离，捕获端 `CopyResource` 进带 `SHARED_NTHANDLE|SHARED_KEYEDMUTEX` 的纹理，编码端 `OpenSharedResource1` 后 `AcquireSync` 取用，两个设备都 `SetGPUThreadPriority(7)` [源码：display_vram.cpp:866-890、1963-1992、499-541；display_base.cpp:707]。所以"每帧一次整帧拷贝"是业界同款代价，不是我们多花的冤枉钱。
2. Sunshine 源码里直接写了我们遇到的同一个病因：D3D11 设备带一把不公平锁，`AcquireNextFrame` 执行期间一直占着，别的线程被饿死 [源码：display_base.cpp:309-321 的注释]。这是对根因判断的独立佐证。
3. 单设备内"修锁争用"的路（SINGLETHREADED、deferred context、减 Flush）基本走不通或收益小，原因见 §1。
4. 真正有增量价值、又能叠加在 E2 上的有三项：
   - **(a) 方案 Z：让 VideoProcessor 直接读 DDA 返回的纹理，在采集设备上做合成，输出 NV12 到共享纹理，省掉 14.7MB 整帧拷贝。** 收益最大，复杂度也最高，且有跨设备 NV12 共享的兼容性未知。
   - **(b) GPU 优先级**：对采集设备 `SetGPUThreadPriority`、必要时进程级 `D3DKMTSetProcessSchedulingPriorityClass`。便宜，可以直接试。
   - **(c) 用量化诊断确认 E2 之后还剩什么**：E2 已让 CPU 锁争用消失，剩下的是 GPU 引擎排队。先测再决定要不要做 (a)。
5. 换 WGC 不建议作主路径（见 §3），只适合作为 DDA 在某些机器失败时的回落，或做对照实验。

## 1. 单设备内避免争用（针对"不想加第二个设备"的路线）

### 1.1 ID3D11Multithread / SINGLETHREADED / Enter-Leave
- 原理 [文档：learn.microsoft.com/windows/win32/api/d3d11_4/nn-d3d11_4-id3d11multithread]：D3D11 immediate context 默认一次只能一个线程用；开 `SetMultithreadProtected(TRUE)` 后每次调用都加设备临界区，"会增加每个 immediate context 调用的开销"。
- `D3D11_CREATE_DEVICE_SINGLETHREADED` [文档：ne-d3d11-d3d11_create_device_flag]：只允许单线程调用，多线程使用"行为未定义"。我们有采集、合成、编码至少三个线程摸同一个设备，且 QSV 要求设备 MultithreadProtected（Sunshine 明确为 QSV 强制开启 [源码：display_vram.cpp:1159-1169]）。**结论：不可行。**
- `Enter/Leave` 只能把多个调用合并成一段临界区，让别人更久拿不到锁，对"采集被卡"方向相反。**不建议。**
- 收益：无。复杂度：低。可叠加 E2：否。

### 1.2 deferred context + 命令列表
- 原理：在合成线程用 deferred context 记录，命令列表到编码线程一次 `ExecuteCommandList`，把录制期间的锁占用挪出来。
- 问题：
  - `VideoProcessorBlt` 是通过 `ID3D11VideoContext` 发的，该接口从 immediate context 查询；deferred context 上是否支持视频处理调用我没有文档证据 [未读]，我倾向认为不支持 [推测]。
  - 即便能用，`ExecuteCommandList` 那一刻仍在 immediate context 上持设备锁，驱动做的"真活"（Intel 的 VP 命令构造、MFX 送帧）发生在提交阶段，锁占用不会消失 [推测]。
  - QSV/MFX 送帧走驱动自己的路径，不经你的 deferred context [推测]。
  - 驱动是否原生支持命令列表可用 `CheckFeatureSupport(D3D11_FEATURE_THREADING)` 的 `DriverCommandLists` 查，Intel 上结果我没数据 [未读]。
- 收益：小且不确定。复杂度：中。**不建议。**

### 1.3 减少/消除 Flush
- 现状：`compose.rs:153` 合成后 `Flush`；`dda.rs:409` 采集拷贝后 `Flush`。E2 的双设备之后，合成设备的 Flush 只占 B 的锁，已经不影响采集。采集设备 A 上只剩拷贝+Signal+Flush，很轻。
- 若回退到单设备：去掉 Flush 只是把提交延后，由运行时批处理决定；`Signal` 要让对端 `Wait` 及时解除必须 Flush，所以单设备下也不能全去 [推测]。
- 收益：在 E2 架构下几乎为零；在单设备架构下有限。复杂度：低。可叠加：是（但没必要）。

### 1.4 锁持有时间错开（时序编排）
- 原理：合成线程在固定时钟下每 1/60s 一次，知道自己的 Blt 大约在 tick+X ms 时占锁；采集线程把 `AcquireNextFrame` 调用放进空闲窗口。
- 事实基础：DDA 会把积压的呈现合并（`AccumulatedFrames`），晚几毫秒取不会丢最新内容，只有"晚到跨过下一次呈现"才真丢中间帧 [文档：AcquireNextFrame 备注，learn.microsoft.com/…/nf-dxgi1_2-idxgioutputduplication-acquirenextframe；E2 实测：合并出现在停顿超过 14ms 之后]。
- 问题：60Hz 桌面的呈现相位是系统决定的，我们无法对齐；QSV 送帧的持锁点也不固定。编排只能降低概率。
- 收益：中低，不保证。复杂度：中（要维护相位估计）。可叠加 E2：可以，但 E2 已经把主要矛盾解决了，优先级低。

## 2. 同设备换队列/优先级

### 2.1 IDXGIDevice::SetGPUThreadPriority
- 原理 [文档：…/nf-dxgi-idxgidevice-setgputhreadpriority]：相对模式 -7..7，0 为默认；文档警告"用得不当会拖慢渲染"。Sunshine 对捕获设备与编码设备都设 7，失败时提示"请以管理员运行以获得最佳性能" [源码：display_vram.cpp:887-890、display_base.cpp:707-710]。
- 进程级：Sunshine 还调 `D3DKMTSetProcessSchedulingPriorityClass`，默认想设 REALTIME，遇到 NVIDIA HAGS 会降为 HIGH，说明 REALTIME 有驱动风险 [源码：display_base.cpp:678-695，注释提到 NVIDIA 驱动 bug]。
- 对我们的意义：它影响的是 **GPU 调度**而非 CPU 设备锁。E2 之后的残余问题如果是"采集拷贝在 GPU 上排在 VP/QSV 后面"，这个开关可能有用；如果只是 CPU 锁，它没用。
- 额外开销：无。核显适用：Intel 支持程度未知，需要实测 `SetGPUThreadPriority` 返回值与效果 [推测]。复杂度：极低。风险：非管理员可能失败（仅日志），过高优先级可能饿死桌面合成（DWM），建议先只给采集设备 +3 到 +7 试。
- 可叠加：是。**建议最先试的低成本项。**

### 2.2 D3D12 / D3D11On12
- DDA 的 `DuplicateOutput` 面向 D3D11 设备；D3D12 方案需要把 DDA 纹理导入 D3D12 共享再用独立队列拷贝/转换，后端 QSV/MF 仍是 D3D11 体系，导入导出链更长 [推测；未读到 D3D12 直接支持 DDA 的文档证据]。
- D3D11On12 相关公开讨论以兼容性问题为主（见 [microsoft/D3D11On12 issue 45](https://github.com/microsoft/D3D11On12/issues/45)，仅看到标题，未细读 [未读]）。
- 收益：理论上独立队列可以让采集拷贝不被 VP 堵住，但我们要的"解耦"用两个 D3D11 设备已经做到，D3D12 是大改动。**不建议。**

## 3. 换捕获 API：Windows Graphics Capture (WGC)

- 原理：`Direct3D11CaptureFramePool::CreateFreeThreaded`，`FrameArrived` 在帧池内部工作线程触发，不依赖 DispatcherQueue [文档：learn.microsoft.com/uwp/api/windows.graphics.capture.direct3d11captureframepool.createfreethreaded]。
- Sunshine 的 WGC 实现 [源码：display_wgc.cpp:142、160-162、184-215、318]：
  - `CreateFreeThreaded(..., 2, item.Size())`，帧池深度 2。
  - 在回调线程 `TryGetNextFrame`，放进互斥保护的 `produced_frame`（新帧会替换旧帧），条件变量唤醒捕获线程。
  - 之后仍然在捕获设备上 `CopyResource`（到暂存纹理或共享纹理）。所以 WGC 不能省掉拷贝，也没有绕过"设备与 VideoProcessor 共享"的问题。
  - 用 `IsPropertyPresent` 判断 `MinUpdateInterval`，代码注释说没有它"在这个版本的 Windows 上屏幕捕获可能被限制为 60fps"。`IsBorderRequired` 同理。本机是 Win10 19045，这两个属性大概率不存在 [推测；未核对 API 合约版本]。
- OBS 的 WGC：`libobs-winrt/winrt-capture.cpp` 用的是 `Create`（非 FreeThreaded，需要 DispatcherQueue）并在回调里 `CopyResource`，也可在需要时 `frame_pool.Recreate` [源码：winrt-capture.cpp:131-183、310-333]。
- 与 DDA 对比：
  - 锁：WGC 帧池内部使用你传入的 D3D 设备，取帧 + `CopyResource` 仍要访问设备；是否也在内部持设备锁我没有证据 [未读]。如果 WGC 内部用设备锁，问题形式不变。
  - 丢帧特性：帧池深度有限，回调线程慢会导致系统丢帧；DDA 会合并。二者都"丢中间帧保最新"。
  - 光标：WGC 可开关系统光标合成，DDA 要自己叠加。
  - 黄色边框（Win11 可关，Win10 19045 不可关）会污染录屏画面 [推测；需核对]。
- 结论：WGC 不能解决本问题，只是换了取帧接口；不建议作为此问题的解。可作为 DDA 失败（权限、多显卡）时的备用来源。
- DDA 纹理直接给 VP，不拷贝：见 §4 方案 Z。

## 4. 共享纹理与栅栏的更低开销做法

### 4.1 keyed mutex 对比 fence
- Sunshine 用 keyed mutex：编码端 `AcquireSync(0, INFINITE)`、画完 `ReleaseSync`，会**阻塞 CPU 线程**直到捕获端放开 [源码：display_vram.cpp:499、541、1546]；E2 的 `ID3D11Fence` 是 GPU 侧等待（`context4.Wait/Signal`），CPU 不阻塞，更好。
- keyed mutex 的优势是兼容面（Win8+，不需要 ID3D11Device5/Win10 1703+）。本机 Win10 19045 两者都可用。
- 结论：E2 的 fence 路线优于 Sunshine 的 keyed mutex；保留。若将来在别的机器上 `OpenSharedFence` 失败，可回落 keyed mutex。**无需改动。**

### 4.2 只拷脏区（GetFrameDirtyRects / GetFrameMoveRects）
- 原理：DDA 提供脏矩形与移动矩形 [文档：DDA API 概述 learn.microsoft.com/windows/win32/direct3ddxgi/desktop-dup-api 与 API 列表；未逐条读 [未读]]。
- 问题：
  - 我们的 10 槽环里每个槽是独立纹理，只拷脏区要求槽里已有上一帧内容，等于要先拷整帧或做链式更新，复杂。
  - 合成设备 B 每帧都要读完整图，省的只有 A 的拷贝。
  - 最坏内容（全屏视频、白噪声）脏区等于全屏，验收按最坏档算，没有收益。
- 收益：对静态桌面大；对验收最坏档无。复杂度：高。可叠加 E2：是，但**不建议**，性价比低。

### 4.3 静止时复用上一帧
- 已在做：仅光标变化时沿用 `latest`（dda.rs:383-388）；超时不拷贝。**无需改动。**

### 4.4 让 VP 直接读共享纹理
- E2 已经是这样（`compose.rs` 读 `texture_b`）。真正能省掉的是"采集端那一次整帧拷贝"，即方案 Z。

### 4.5 方案 Z：采集设备直接做 VP，不做整帧拷贝（唯一能同时降拷贝与锁风险的新结构）
- 结构：采集设备 A 上，`AcquireNextFrame` 拿到纹理 → 同一线程顺序调 `VideoProcessorBlt`（桌面+光标，BGRA→NV12，缩放）→ 输出到 **共享的 NV12 编码表面**（ffmpeg d3d11va 池设 `SHARED_NTHANDLE`，在 A 上用 `OpenSharedResource1` 打开）→ `Signal` fence → 此后才 `ReleaseFrame`（用 fence/Query 保证 GPU 读完桌面纹理）。编码设备 B 只 `Wait` 后送 QSV。
- 为什么合理：A 上只有一个线程操作，**没有线程间设备锁争用**；VP 从 DDA 纹理读是同设备同队列，OBS 与微软示例也都是"取到就在同设备上用"（OBS 是 `CopyResource` 再释放 [源码：d3d11-duplicator.cpp:252、296]，不是直接读，我这里是推测性扩展）。
- 预期收益：省 14.7MB 拷贝（约 0.5~1ms GPU，带宽减半，核显共享内存带宽敏感 [推测]）；拷贝消失后采集设备只在 VP 期间持有桌面帧。
- 风险：
  1. 持有桌面帧时间变成 VP 的 GPU 时间（约 2.0~2.4ms，来自 spike 报告），期间新呈现会累积合并（不会丢最新，但增加合并次数），必须实测 `AccumulatedFrames` 分布。
  2. A 的 CPU 线程要等 GPU 完成才能 ReleaseFrame，采集循环节奏变成"取帧→VP→等 GPU"，可能拉长采集间隔，需测。
  3. **跨设备 NV12 纹理共享**在 Intel 核显上是否稳定、能否用作 VP 输出 + QSV 输入，我没有证据 [未读]；必须先做兼容性探针。
  4. 合成线程失去独立性：合成时钟与采集线程绑定，固定时钟槽的逻辑要重新设计。
- 复杂度：高。可叠加 E2：**替换** E2 的拷贝+B 合成部分，保留 fence 与双设备。
- 建议：先量化 E2 的拷贝开销；若拷贝 + 栅栏占比显著（例如 >10% 的帧预算）再做。

## 5. 其他项目的做法（对比）

| 项目 | 设备关系 | 同步 | 取帧 | 已知争用处理 | 证据 |
|---|---|---|---|---|---|
| Sunshine (display_vram) | 捕获设备与编码设备分离，各设 GPU 线程优先级 7，QSV 时编码设备开 MultithreadProtected | keyed mutex（CPU AcquireSync） | DDA `next_frame(timeout)`，超时重复上一帧；另有 WGC 后端 | 注释明确指出 AcquireNextFrame 占不公平设备锁会饿死编码线程，超时后睡 10ms 让出锁 | [源码] display_vram.cpp、display_base.cpp |
| OBS d3d11-duplicator | 与图形线程共用一个 D3D11 设备（libobs 只有一个渲染设备） | 无（同线程） | `AcquireNextFrame(0)`，超时视为成功复用上一帧，`CopyResource` 后 `ReleaseFrame` | 取帧与渲染在同一图形线程，天然串行，不存在线程间争用；滞后只计 lagged_frames | [源码] d3d11-duplicator.cpp:252、270-296 |
| ffmpeg ddagrab | 滤镜自己创建一个 D3D11VA 设备 | 无（单线程） | `AcquireNextFrame` + `CopySubresourceRegion` + `ReleaseFrame`；注释承认"AcquireNextFrame sometimes has bursts of delay" | 无 | [源码] vsrc_ddagrab.c:695、1148-1183 |
| Cap / windows-capture / scap | WGC 系，Cap 用 MF 编码，共用一个设备 | — | WGC | — | Cap 的 MF 编码器用 `IMFDXGIDeviceManager` 绑定同一个 D3D 设备 [源码：cap_video.rs:239-289]；windows-capture、scap 细节 **[未读]** |

观察：
- OBS 与 ddagrab 之所以没有我们的问题，是**采集、转换、提交在同一个线程串行**，而我们是多线程流水线共享设备，锁争用是流水线化的直接后果。这也解释了方案 Z 的思路：把和 DDA 相关的 GPU 工作放回同一个线程。
- 我没找到这几个项目里关于"双设备导致丢帧"的 issue 讨论；WebSearch 只返回了泛泛的 DDA 慢/帧合并问答（如 [微软 Q&A：DDA skip frame](https://learn.microsoft.com/en-us/answers/questions/459716/desktop-duplication-api-skip-frame-issue-and-slown)），未形成有用证据。

## 6. 方案汇总

| # | 方案 | 对采集被卡的影响 | 额外开销 | 核显适用 | 复杂度/风险 | 可叠加 E2 |
|---|---|---|---|---|---|---|
| 1 | E2 现行：双设备+fence+整帧拷贝 | 已消除 CPU 锁争用（Sunshine 同款） | 14.7MB 拷贝/帧 | 本机已验证，兼容性未知 | 已完成 | — |
| 2 | SetGPUThreadPriority（采集设备 +3..+7） | 只影响 GPU 排队，不影响 CPU 锁 | 无 | 需实测返回值 | 极低 / 非管理员可能失败 | 是 |
| 3 | 方案 Z：采集设备内 VP，省整帧拷贝 | 无线程间锁；持桌面帧时间变长 | 省拷贝，增持帧时间 | NV12 跨设备共享未知 | 高 / 兼容性 | 替换 E2 的拷贝部分 |
| 4 | 时序编排 | 降低概率，不保证 | 无 | 通用 | 中 / 收益不稳 | 是，低优先 |
| 5 | SINGLETHREADED | 不可行（多线程+QSV） | — | — | — | 否 |
| 6 | deferred context | 小且不确定 | — | 驱动命令列表支持未知 | 中 | 否 |
| 7 | 只拷脏区 | 最坏内容无收益 | — | — | 高 | 低价值 |
| 8 | keyed mutex 替换 fence | 更差（CPU 阻塞） | — | 兼容面更广 | 低 | 仅作回落 |
| 9 | WGC 替换 DDA | 不解决设备共享问题 | 同样要拷贝 | Win10 19045 功能受限 | 中 | 仅作回落/对照 |
| 10 | D3D12/D3D11On12 | 理论可解耦，成本极高 | 导入导出 | 未知 | 很高 | 不建议 |

## 7. 推荐顺序

1. **先测量，不写新功能**：确认 E2 之后 `acquire_gap_ms`/`slow_acquire_ms` 是否已回到 <1ms，残余卡顿是否转移到 GPU（见验证实验 V1、V2）。
2. 试 **SetGPUThreadPriority**（方案 2），半小时内可出结论。
3. 只有在 V1/V3 显示拷贝+栅栏确实吃掉 >10% 帧预算、或核显带宽成为瓶颈时，再立项 **方案 Z**，先做跨设备 NV12 共享的探针程序（V4），探针不过就放弃。
4. 方案 4、7、10 暂不做。WGC 留作备用来源。

## 8. 建议验证实验（以后可跑，本次未运行）

- **V1 复测 E2 的采集卡顿分布**：按 E2 简报第 6 节的验收命令，带 `SNOW_RECORDER_TRACE=1` 跑 1440p60 最坏内容，看 `acquire_gap_ms`、`slow_acquire_ms`、`copy_ms` 的 P50/P95/P99，以及 `coalesced` 计数。判据：`slow_acquire_ms` 不再出现 >1ms，合并事件 <1%。
- **V2 看 GPU 引擎占用**：录制期间用 Windows 任务管理器"性能→GPU→引擎"或 `typeperf "\GPU Engine(*)\Utilization Percentage"` 观察 3D / Video / VideoProcessing 引擎的使用率；需要更细时用 GPUView / WPA 的 GPU 调度视图看采集拷贝在队列里等了多久。判据：拷贝包排队延迟 >2ms 说明是 GPU 排队问题，此时优先级与方案 Z 才有意义。
- **V3 量化拷贝+栅栏成本**：在 `dda.rs` 已有 `copy_ms` 诊断基础上，临时对比关闭拷贝（合成设备直接读一张静态纹理）与开启拷贝的帧率差；也可用 D3D11 `ID3D11Query`（`D3D11_QUERY_TIMESTAMP`）给拷贝前后打时间戳，得到 GPU 侧真实耗时。判据：GPU 拷贝时间 <1ms、占帧预算 <6% 则不必做方案 Z。
- **V4 跨设备 NV12 共享探针（方案 Z 前置）**：写一个独立小程序：设备 B 创建 `SHARED_NTHANDLE` 的 NV12 纹理，设备 A `OpenSharedResource1` 后创建 VP 输出视图，做一次 BGRA→NV12 Blt，fence 通知 B，B 用 `CopyResource` 读回校验亮度值。判据：输出正确且连续 10 分钟无设备移除错误。
- **V5 GPU 优先级**：在采集设备创建后调用 `IDXGIDevice::SetGPUThreadPriority(3)`/`(7)`，记录返回值与是否需要管理员；重复 V1，对比 `slow_acquire_ms` 与合成耗时是否变化。
- **V6 WGC 对照（可选）**：用 ffmpeg 的 `-f lavfi -i gfxcapture`（若本机 ffmpeg 构建含该滤镜）或一个最小 WGC 程序录 10 秒，对比帧间隔分布。判据仅用于评估备用来源，不作为主路径依据。
- 所有 GPU 实验都要避开其他验收正在跑的时段。

## 9. 证据边界

读了源码/文档的结论：
- Sunshine 双设备 + keyed mutex + GPU 优先级 + 对 AcquireNextFrame 不公平锁的注释；Sunshine WGC 用法；OBS 的 DDA 取帧与 `CopyResource` 后 `ReleaseFrame`；OBS WGC 用 `Create` 非 FreeThreaded；ffmpeg ddagrab 的取帧/拷贝流程与"bursts of delay"注释。
- 微软文档：`AcquireNextFrame`、`ID3D11Multithread`、`D3D11_CREATE_DEVICE_SINGLETHREADED`、`CreateFreeThreaded`、`SetGPUThreadPriority`。

纯推测（需实验确认）：
- Intel UHD 770 上 VP/QSV 占用的引擎与抢占粒度；GPU 排队是否真的是 E2 之后的残余原因。
- deferred context 是否支持视频处理调用；WGC 内部是否持设备锁；Win10 19045 上 WGC 的 `MinUpdateInterval/IsBorderRequired` 可用性；方案 Z 的跨设备 NV12 共享稳定性。
- 对 windows-capture、scap 的细节、D3D11On12 相关 issue、DDA 脏矩形 API 细节：**没读**。
