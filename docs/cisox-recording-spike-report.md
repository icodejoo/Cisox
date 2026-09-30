# Cisox 录屏 BGRA→NV12 转换方案对比 Spike 报告

结论先行：**推荐 D3D11 VideoProcessor，把桌面 + 覆盖层 + 光标/高亮作为同一次 `VideoProcessorBlt` 的多个图层，直接输出到 NV12 池纹理（view 预建）；QSV 用 `async_depth=2`、`preset=veryfast`。** 瓶颈主要是"5 次全分辨率 Blt 串联"与"async_depth=1 发一帧等一帧"，不是转换方案本身。4 种转换方案（VP、MF VPMFT、PS、CS）的单次转换耗时都在 2~3ms（1440p），彼此差距小于运行间波动。

数据全部为本机实测（原始 JSONL 与日志在 scratchpad 的 `spike\data\`）；推测处已标注"推测"。

## 1. 环境与方法

| 项 | 值 |
|---|---|
| GPU | Intel UHD Graphics 770（VendorId 0x8086，DeviceId 0x4680），驱动 UMD 31.0.101.4953，共享内存 16228MB，专用 128MB |
| 系统 | Windows 10 Pro 19045，两块 2560x1440@59Hz（主屏 0,0；副屏 2560,0） |
| D3D11 | Feature Level 11.1；VideoProcessor MaxInputStreams=16，支持 alpha stream |
| 构建 | release，静态 CRT + 静态 FFmpeg（与 snow-recorder 同构建方式）；`CARGO_BUILD_JOBS=4` |
| 位置 | `spikes/recording-convert-spike/`（自带 `[workspace]` 与独立 `target/`，不在主工作区；未改 snow-crates / snow-shot-rs / tools） |
| 依赖 | 仅 `windows 0.62.2`、`ffmpeg-next 9.0.0`（已获批），无其他新增三方依赖 |
| 输入 | 合成 BGRA GPU 纹理池（8 张内容各异，按帧循环，避免缓存）。`noisy`=下 1/3 随机噪声（最坏，1440p 约 832KB/帧）；`desktop`=无噪声渐变+移动色条（1440p 约 17KB/帧）。**不占屏**，无窗口/全屏程序；ffmpeg 真实捕获只在按属性校验的副屏（Primary=false 且 bounds=2560,0,2560x1440）上、每轮 8s |
| 帧数 | 每组 600 帧，丢弃 60 帧预热；端到端关键配置交错重复 3 次 |

**计时方法（重要）**：
- GPU 时间戳（`TIMESTAMP_DISJOINT`）**测不到 VideoProcessor**（它跑在视频引擎上，读数恒为 0.000ms），PS/CS 可以测到。因此所有方案统一用"串行单帧墙钟"（提交 + Fence 阻塞等 GPU 完成）作为可比耗时，表里记为 `sync`；PS/CS 额外给出时间戳值。
- 吞吐 fps：GPU 队列最多领先 3 帧（Fence 阻塞节流，不空转）。
- 进程 CPU：单核当量，`GetProcessTimes`；显存：`IDXGIAdapter3::QueryVideoMemoryInfo`（核显为共享内存口径）。

## 2. 纯转换（BGRA→NV12，单 pass）

各行为 2 次运行的平均，单位 ms（除注明）。

### 1440p

| 方案 | sync P50 | P95 | P99 | 平均 | CPU 提交 P50 | GPU 时间戳 P50 | 吞吐 fps | 进程CPU% | 峰值工作集MB | 显存MB |
|---|---|---|---|---|---|---|---|---|---|---|
| vp_base（上游写法，每帧重建 view） | 2.36 | 3.20 | 3.58 | 2.43 | 0.47 | 不可测 | 526 | 36 | 219 | 175 |
| vp_opt_rt（预建，输出仅 RT） | 2.36 | 3.58 | 4.13 | 2.50 | 0.44 | 不可测 | 520 | 30 | 219 | 175 |
| vp_opt（预建，RT+VIDEO_ENCODER） | 2.18 | 2.83 | 3.14 | 2.24 | 0.43 | 不可测 | 527 | 37 | 221 | 177 |
| ps（像素着色器两次 draw） | 2.30 | 2.87 | 3.28 | 2.32 | 0.03 | 2.07 | 449 | 30 | 173 | 141 |
| cs（compute） | 2.01 | 2.58 | 2.94 | 2.07 | 0.03 | 1.90 | 496 | 19 | 175 | 143 |
| mf_vpmft（Video Processor MFT） | 2.12 | 2.58 | 2.93 | 2.18 | 0.43 | 不可测 | 534 | 37 | 212 | 167 |
| vp_multi_tile（单 Blt：桌面+256² 覆盖层+64² 光标，输出 NV12） | 4.31 | 8.9(P99) | - | - | 1.18 | 不可测 | 318 | - | - | - |
| vp_multi（单 Blt：桌面+全屏覆盖层+64² 光标） | 5.68 | 10.69(P99) | - | - | 1.58 | 不可测 | 202 | - | - | - |

### 1080p

| 方案 | sync P50 | P95 | P99 | 吞吐 fps | 进程CPU% |
|---|---|---|---|---|---|
| vp_base | 1.41 | 1.88 | 2.18 | 975 | 52 |
| vp_opt_rt | 1.41 | 1.93 | 2.19 | 927 | 45 |
| vp_opt | 1.42 | 1.93 | 2.24 | 946 | 61 |
| ps | 1.27 | 1.66 | 1.90 | 806 | 44 |
| cs | 1.21 | 1.65 | 2.09 | 928 | 35 |
| mf_vpmft | 1.60 | 2.25 | 2.60 | 889 | 63 |
| vp_multi_tile | 3.54 | 8.71(P99) | - | 505 | - |
| vp_multi | 3.79 | 8.52(P99) | - | 335 | - |

1440→1080 缩放输出（同一 pass 内缩放）：vp_opt sync P50 约 4.7ms，ps 约 3.9ms（各 2 次运行）。

要点：
- 单 pass 转换 1440p 只要约 2.2ms，1080p 约 1.4ms，说明转换本身不是瓶颈（与离线 QSV 89~137fps 一致）。
- vp_base → vp_opt：每帧省约 0.05~0.2ms CPU，GPU 侧无可见差别。预建 view 有收益但小。
- 每多一个全屏 alpha 图层，VP 约加 2~3.5ms（vp_multi 5.68 vs 单流 2.18）；把覆盖层缩成脏区小图层（vp_multi_tile）只加约 2ms。

## 3. 上游 compose 分段计时（验证"多 pass 叠加"）

复刻 `DirectGpuCompositor::compose` 的形态：5 次全分辨率 VideoProcessorBlt（desktop / 高亮 compute 后再 blit / effects 覆盖层 / keyboard 覆盖层 / NV12）+ 1 次 compute + 2 次 256² 覆盖层上传。分段用"串行提交+Fence 等完成"的墙钟（时间戳对 VP 无效，且会把视频引擎与 3D 引擎的等待错记到下一段）。

| 段 | 1440p sync P50 | 1080p sync P50 |
|---|---|---|
| desktop_blit（BGRA→BGRA） | 3.54 | 2.05 |
| highlight_cs + blit | 4.47 | 2.41 |
| effects 上传 + blit（alpha 叠加） | 5.96 | 3.64 |
| keyboard 上传 + blit | 5.85 | 3.30 |
| nv12_blit | 2.97 | 1.85 |
| 各段之和（含每段同步开销） | 22.79 | 13.26 |
| **整体串行 P50 / P95 / P99** | **19.70 / 24.18 / 25.54** | **11.87 / 15.04 / 15.94** |
| 整体 CPU 提交 P50 | 19.18 | 11.19 |
| 流水线吞吐 | 50.4 fps | 87.4 fps |

结论：
- "多 pass 叠加"成立：5 个 pass 的总耗时约是单 pass NV12 的 8.5 倍（1440p：19.7 vs 2.3ms）。**1440p 下光是这条链就只能到约 50fps，在还没编码、没捕获之前就过不了 56fps 线。**
- 但本复刻 1080p 整体为 11.9ms，低于应用内实测的 26.7ms。差额（约 15ms）来自复刻未包含的部分（光标着色器 pass、TileSurface 差分与 CPU 绘制、捕获与 compose 共用同一设备/全局锁的争用等），本 spike 未单独量化，属于"推测"；不应理解为"26.7ms 已被完全复现"。
- compose 的 CPU 提交时间与墙钟几乎相等（19.2 vs 19.7ms）：VP 的 Blt 在驱动里会阻塞调用线程，所以这条链还会占满工作线程。

## 4. 端到端（转换 + h264_qsv，合成输入，无捕获）

`帧总耗时` = 取池帧 + 转换提交 + send_frame + 取包；fps 为整段墙钟吞吐。以下为 3 次运行平均（[最小-最大]）。QSV 参数：`look_ahead=0, bf=0, global_quality=23`；同一进程内 D3D11VA 帧池 → 派生 QSV → `av_hwframe_map`，与上游 gpu.rs 相同。

### 4.1 转换方案对比（上游参数 a1/medium vs 调优参数 a4/veryfast）

| 分辨率 / 内容 | 配置 | vp_base | vp_opt | ps | cs |
|---|---|---|---|---|---|
| 1440p noisy | a1 medium（上游参数） | 49.5 [29.6-64.2] | 54.8 [35.1-65.2] | 58.1 [47.3-70.4] | 57.5 [49.1-68.0] |
| 1440p noisy | a4 veryfast | 100.7 [89.2-116.2] | 101.5 [90.2-113.7] | 92.2 [82.1-102.8] | 90.4 [83.5-94.2] |
| 1440p desktop | a1 medium | 93.6 [76.6-108.9] | 102.3 [101.5-103.0] | 95.2 [78.1-104.9] | 95.0 [87.0-102.7] |
| 1440p desktop | a4 veryfast | 131.3 [126.1-137.5] | 125.8 [106.6-149.1] | 104.1 [77.5-125.6] | 97.7 [56.6-124.6] |
| 1080p noisy | a1 medium | 93.2 [68.6-113.4] | 104.9 [98.0-113.6] | 112.4 [104.5-118.9] | 106.6 [104.9-108.3] |
| 1080p noisy | a4 veryfast | 170.2 [169.2-171.7] | 178.2 [171.2-187.3] | 163.6 [151.0-174.5] | 167.6 [155.0-181.8] |
| 1080p desktop | a1 medium | 139.4 [126.4-152.3] | 151.8 [137.4-161.9] | 137.5 [102.8-164.3] | 144.3 [138.5-151.0] |
| 1080p desktop | a4 veryfast | 236.1 [172.1-285.9] | 238.8 [192.2-273.7] | 184.6 [143.6-213.0] | 192.1 [147.8-218.6] |

**4 种转换方案的端到端差别落在运行间波动之内**（iGPU 功耗/频率共享，同配置同方案相差可达 2 倍）。真正拉开差距的是 QSV 参数（下节）。1440p noisy 上游参数（a1 medium）最差单次仅 29.6fps，帧总耗时 P99 约 35~39ms。

### 4.2 QSV 参数扫描（vp_opt，3 次平均 fps）

| 分辨率 / 内容 | a1 medium | a2 medium | a4 medium | a1 veryfast | a2 veryfast | a4 veryfast |
|---|---|---|---|---|---|---|
| 1440p noisy | 54.8 | 84.3 | 83.8 | 79.0 | 96.0 | 101.5 |
| 1440p desktop | 102.3 | 105.5 | 119.3 | 102.0 | 137.8 | 125.8 |
| 1080p noisy | 104.9 | 136.5 | 124.0 | 113.2 | 154.7 | 178.2 |
| 1080p desktop | 151.8 | 189.1 | 169.8 | 142.7 | 224.4 | 238.8 |

- `async_depth` 1→2 收益最大（1440p noisy medium：55→84）；2→4 基本持平（a2/a4 各有胜负，均在波动内）。**取 2 即可**，少占池纹理。
- `veryfast` 相对 `medium` 约 +15~25%。画质（码率/PSNR）未评估，落地前需要看 CRF 下的体积与主观质量。
- 纹理标志消融（vp_opt，3 次平均）：`RENDER_TARGET` vs `RENDER_TARGET|VIDEO_ENCODER` 在 4 组（noisy/desktop × a1 medium/a4 veryfast）中互有胜负（如 desktop a1 medium 60.7 vs 68.5，但区间重叠 [48-72] vs [59-74]；其余三组差 ≤4%）。**VIDEO_ENCODER 标志在本机 QSV 路径上没有可测收益**，可保留（创建正常）但不作为优化抓手。

### 4.3 单 Blt 多图层（推荐形态）的端到端

| 分辨率 / 内容 | a1 medium vp_multi | a1 medium vp_multi_tile | a4 veryfast vp_multi | a4 veryfast vp_multi_tile |
|---|---|---|---|---|
| 1440p noisy | 47.8 [46.7-49.1] | 49.8 [48.8-50.5] | 81.6 [72.3-87.2] | 87.5 [82.8-90.8] |
| 1440p desktop | 59.0 (n=2) | 60.3 [54.3-70.4] | 100.8 (n=2) | 112.7 [106.4-123.6] |
| 1080p noisy | 77.1 | 81.6 | 143.6 (n=2) | 150.7 |
| 1080p desktop | 93.8 | 103.1 | 165.6 (n=2) | 182.3 |

（`vp_multi`：桌面 + 全屏覆盖层 + 64² 光标共 3 图层；`vp_multi_tile`：覆盖层仅 256² 脏区。n=2 的格子是矩阵尚有运行未落盘时的统计。）
对照：上游 5 pass 形态在 1440p 仅 VP 链就 19.7ms；单 Blt 多图层 1440p 整条（含编码）是 9~12ms/帧，从约 50fps 级提到 80~110fps 级。

## 5. 各方案结论

### 5.1 D3D11 VideoProcessor
- (a) 上游写法：功能正确，单次 2.4ms（1440p）；问题不在单次，在 compose 里串了 5 次。
- (b) 优化写法：view 预建、单 pass，省 CPU 但转换 GPU 不变。输出纹理加 `VIDEO_ENCODER` 无可测收益。
- 正确性：Y 最大误差 0.59、平均 0.25；UV（按左对齐参考）最大 0.64、平均 0.22；PSNR 约 59dB。VP 的色度取样位置是**左对齐**（MPEG-2/H.264 默认），与居中 2x2 均值的参考最大差 90~112（仅在锐利边缘，属取样位置不同，不是错误）。
- 兼容性：只需 FL11.0 + 视频驱动；本机核显可用；不需要平面 RTV/UAV。失败回退：`CheckVideoProcessorFormatConversion` 失败则走 PS/CS。
- 复杂度：最低。上游已有，~330 行（含基线/优化/多图层三个变体）。

### 5.2 Media Foundation
- Video Processor MFT（CLSID_VideoProcessorMFT）：RGB32 输入 + `IMFDXGIDeviceManager` + DXGI surface buffer 零拷贝，输出 NV12，自校验通过（误差同 VP，内部就是 D3D11 VP）。单次 2.1ms（1440p），但 **MFT 必须在每次 ProcessOutput 后继续取到 NEED_MORE_INPUT 才能收下一帧**；输出样本由 MFT 自己分配（`MFT_OUTPUT_STREAM_PROVIDES_SAMPLES`）。GPU 时间戳不可测。
- 硬件 H.264 编码 MFT（"Intel® Quick Sync Video H.264 Encoder MFT"，异步、D3D11 aware）：**手工构造的 RGB32 / ARGB32 输入类型被拒**（`0xC00D36B4` MF_E_INVALIDTYPE）；只接受它自己列出的类型（NV12 及驱动自带的 ARGB32 项，`SetInputType` 测试通过）。直接用驱动给的 ARGB32 类型喂 BGRA（驱动内部转换）可以跑通。
- MF 端到端（默认参数、CBR 40Mbps、未调 ICodecAPI，3 次平均）：1440p「驱动 ARGB32 直喂」58.2~58.4fps，「VPMFT + 硬编 MFT」54.7fps（noisy 时 41.0 [34.8-52.8]）；1080p 约 61~65fps。帧间隔中位数固定约 15.4ms（像是被编码 MFT 的固定延迟/节奏限制，未深究，推测为默认低延迟设置不足），P99 约 28~44ms。实现复杂（异步事件循环、类型协商、STREAM_CHANGE 处理，~500 行）。
- 结论：功能可行，但吞吐与帧时间抖动都不如直接走 ffmpeg h264_qsv + 自己的转换；不推荐作为主路径，可作为 ffmpeg/QSV 不可用时的回退候选。

### 5.3 ffmpeg 硬件帧链路（系统 ffmpeg 8.0.1，ddagrab 真实捕获副屏，静止桌面）

**口径说明**：本节表中的 fps 全部是「整条链路 fps」（真实捕获 + 转换 + 编码，或注明的子链），受 ddagrab 60fps 定速限制，**不是转换耗时**，不得与第 2 节的转换耗时（ms）比较。转换耗时口径（ms）仅见第 2、3 节。`scale_d3d11` 失败的根因调查见文末「附录：scale_d3d11 排查」。

| 链路 | 整条链路 fps / 结果（非转换耗时） |
|---|---|
| `ddagrab → scale_d3d11=format=nv12 → hwmap(qsv) → h264_qsv` | **不可行**（无 fps）：`scale_d3d11: Failed to create input view: HRESULT 0x887A0004`（DXGI_ERROR_UNSUPPORTED）。用 `hwupload` 的纹理做输入同样失败。根因见附录：ffmpeg 自身 bug，非标志问题、非驱动限制 |
| `ddagrab → hwmap=derive_device=qsv → vpp_qsv=format=nv12 → h264_qsv`（默认 medium, async 4） | 可行：51.7fps（rtime 含约 1s 启动/收尾，稳态约 57fps） |
| 同上，veryfast async_depth=4 | 54.4fps；veryfast async_depth=1 → 32.8fps |
| `scale_qsv=format=nv12`（默认） | 42.8fps |
| vpp_qsv 缩放到 1080p（veryfast a4） | 52.0fps |
| 仅转换（vpp_qsv→hwdownload NV12→null） | 47.5fps，CPU 121%（下载计入） |
| 仅捕获（ddagrab→hwdownload BGRA→null） | 50.1fps（下载计入） |
| `framerate=240:dup_frames=1` + h264_qsv | 打开编码器失败（`Error while opening encoder`），未能测出余量 |

说明：ddagrab 按 `framerate=60` 定速，且副屏静止，本组 fps 是"真实捕获+vpp_qsv+QSV"的实时链路结果，不是纯吞吐上限；内容静止，编码负担轻。它在稳态约 55~58fps，**接近但贴着 56fps 线**，且 async_depth=1 时明显不够。命令行链路无法给出逐帧分布，也无法并入我们的覆盖层合成。结论：仅作为"上限对照/可行性"，不作为集成路径。

### 5.4 自建着色器
- PS（Y 与 UV 两次 draw，BT.709 limited）：1440p 2.3ms（GPU 时间戳 2.07ms），UV 用半尺寸视口 + 双线性取样；有"居中/左对齐"两种取样（左对齐与 VP 一致）。正确性：Y 最大误差 0.51，UV（与对应取样位置的参考）最大 0.51、PSNR 约 59.6dB。
- CS：1440p 2.0ms（时间戳 1.90ms），每线程处理一个 2x2 块，直接写 NV12 平面 UAV；正确性同 PS。
- 兼容性：需要驱动支持 NV12 **平面** RTV（R8/R8G8）或 UAV（R8/R8G8 typed store）。本机 UHD 770 上 `FormatSupport` 与创建均通过；其它厂商未验证。NV12 池纹理需要额外带 `RENDER_TARGET`（PS）或 `UNORDERED_ACCESS`（CS）标志，经 ffmpeg 帧池（`BindFlags`）传入后，经 QSV 派生映射可正常编码（端到端已跑通）。失败回退 VP。
- 优势：可以把叠加（光标/轨迹/键盘）并进同一个着色器，彻底取消中间 BGRA；缺点：自己维护色彩与取样位置，多厂商兼容要额外验证；缩放用普通双线性，1440→1080 有锯齿风险（质量未评估）。
- 复杂度：~350 行（含 HLSL 与缓存视图）；compute 版同规模。

## 6. 推荐

**主路径：D3D11 VideoProcessor，单次 `VideoProcessorBlt`，把桌面、覆盖层（只传脏区小图层，带目标矩形）、光标/高亮贴图作为多个输入流，直接输出到 NV12 池纹理；输入/输出 view 预建或按纹理缓存；QSV `async_depth=2`、`preset=veryfast`。**

理由：
1. 性能：单 Blt 多图层 1440p 整体（转换+QSV）实测约 82~113fps（a4 veryfast，noisy 与 desktop），1440p 上游 5 pass 形态仅转换链就 19.7ms；1080p 约 143~182fps。
2. 转换方案之间无显著差别（2~3ms，差别在波动内），所以选实现最简、兼容性最广、回退最清晰的一种；VP 也是上游已有代码，改动最小。
3. 风险最低：不依赖平面 RTV/UAV 支持；色度取样位置与 H.264 默认一致。

**能否过线**：1440p@60 要求有效 fps≥56、丢帧<1%。合成输入下（无捕获）推荐形态 1440p 实测 82~113fps，且帧总耗时 P50 约 9~12ms，**过线；加上 DDA 捕获与 compose 之外的开销后估计 60~80fps（推测，未实测，需要集成后用夹具验证）**。P99 帧时间约 15~22ms（a4 veryfast），略超 16.7ms，所以送帧与取包需要异步/深度≥2 的队列，不能同步等。1080p@30 余量很大（实测 100+fps）。
对照：若不改（上游 5 pass + a1 medium），1440p 估计 ≤35fps（推测：19.7ms 链 + 约 8~14ms 送帧），过不了线。

**集成建议**（按收益排序）：
1. 把 compose 里 effects/keyboard/cursor/高亮全部改为 VP 多图层或脏区小图层，**取消 BGRA 中间纹理和 4 次全分辨率 Blt**（最大收益）。高亮圆与光标预渲染成小贴图，用目标矩形定位；覆盖层只上传并叠加脏区。
2. QSV：`async_depth` 1→2，`preset` medium→veryfast；池容量 ≥ async_depth+3。画质/体积需单独评估。
3. VP 的 view 按纹理缓存（池纹理有限），去掉每次 `check_conversion` 和 `device.check()`，仅在创建时检查。
4. 送帧线程与 compose 线程分离；compose 侧遇池耗尽时丢最旧未送帧并计数。
5. 保留 PS（左对齐取样）作为 VP 不可用时的回退，不作为首选；`VIDEO_ENCODER` 标志可加可不加。
6. 不建议：走 ffmpeg 命令行滤镜链（scale_d3d11 在 ffmpeg 8.0.1 有 FourCC bug，未打补丁不可用，见附录；vpp_qsv 链路贴线）；MF 编码 MFT 吞吐与抖动均偏差。

## 7. 局限与未完成项
- 输入是合成纹理，没有把真实 DDA 捕获接入本 spike 的流水线；ffmpeg ddagrab 对照只在静止的副屏上跑了 8s 级短轮，且 fps 含启动/收尾。
- 没做解码后 PSNR 与 QSV 画质/体积评估（只做了 NV12 与 CPU 参考的误差对比）。
- iGPU 单次运行波动大（同配置最差/最好差可达 2 倍），关键结论依赖 3 次平均；长时间热降频未测。
- `vp_multi` 系列未单独做 NV12 内容正确性对比（图层全透明，转换路径同 vp_base，推测一致）。
- 其它厂商（NVIDIA/AMD）上的 PS/CS 平面视图兼容性、VP 多图层性能未验证。
- compose 复刻未包含光标着色器 pass 与 TileSurface 差分，故未能复现应用内 26.7ms，差额来源是推测。
- 构建意外：仓库根 `.cargo/config.toml` 的 `target-dir=build/cargo` 会影响 spikes 下的 crate，已在 spike 内加 `.cargo/config.toml` 覆盖为本地 `target`；首次误用时在 `build/cargo` 下留下了少量本 crate 的产物（已被 gitignore）。
- 工程文件：`spikes/recording-convert-spike/`（`run-all.ps1`、`run-repeat.ps1`、`run-ffmpeg.ps1` 为实测驱动；`cargo test` 5 项单测通过，GPU 侧自校验用 `verify` 子命令，所有方案 Y 误差≤0.6）。

## 附录：scale_d3d11 排查（调研部分，未实测）

范围：只读源码与文档，加少量不占 GPU/捕获的探针；**未做任何 fps 或转换耗时实测**，等 E2 录制测试结束后另行复测。原始命令与日志在 scratchpad 的 `spikefmpeg-d3d11\`（`matrix.sh`、`logs\`、`fourcc-probe\`、`src\` 为下载的源码副本）。

### A.1 结论

- **根因（已由探针证实）：ffmpeg 8.0.1 的 `vf_scale_d3d11.c` 创建 VideoProcessor 输入视图时，把 `DXGI_FORMAT` 枚举值直接填进了 `D3D11_VIDEO_PROCESSOR_INPUT_VIEW_DESC.FourCC`。** FourCC 字段应为 0（按纹理自身 DXGI 格式）或 YUV 的 FOURCC 码，而不是 DXGI 枚举值（BGRA=87、NV12=103、P010=104）。本机 Intel UHD 770 驱动对这种非法值返回 `0x887A0004`。
- **不是输入格式限制**：NV12、P010、BGRA、RGB0、X2BGR10 输入全部同样失败；分辨率 640x360 / 1080p / 1440p 同样失败。
- **不是 ddagrab 纹理标志问题**：用 lavfi→hwupload 的纹理也失败；ddagrab 输出本身带 `RENDER_TARGET`（`vsrc_ddagrab.c` 841 行）。
- **是否驱动限制**：本机驱动的判定是"FourCC 非法 → 拒绝"，行为合理。其它驱动可能忽略该字段而碰巧能用（推测，未验证）。所以这是 ffmpeg 的 bug，本机只是暴露了它。
- **能否修**：一行补丁（`.FourCC = 0`）即可，理论上无需改别处。但要跑起来必须自编 ffmpeg，命令行参数无法绕过。本轮**未编译、未实测**。
- **当前 master 仍未修**：`master` 的 `vf_scale_d3d11.c` 226 行仍是 `.FourCC = s->input_format`，已核对。换 ffmpeg 版本不能解决。

### A.2 源码证据（读的是真实源码，版本 n8.0.1）

| 位置 | 内容 |
|---|---|
| [`vf_scale_d3d11.c` L211 与 L225](https://github.com/FFmpeg/FFmpeg/blob/n8.0.1/libavfilter/vf_scale_d3d11.c#L205-L235) | `s->input_format = textureDesc.Format;`（`DXGI_FORMAT`），随后 `inputViewDesc = { .FourCC = s->input_format, .ViewDimension = D3D11_VPIV_DIMENSION_TEXTURE2D, .Texture2D.ArraySlice = subIdx }`，紧接着 `CreateVideoProcessorInputView`，失败即打印 `Failed to create input view: HRESULT 0x%lX` |
| [`master` 同文件](https://github.com/FFmpeg/FFmpeg/blob/master/libavfilter/vf_scale_d3d11.c#L226) | 226 行同样写法，未修 |
| 同文件 `scale_d3d11_configure_processor` | `CONTENT_DESC` 用输入/输出宽高、`PLAYBACK_NORMAL`；只接受输出 NV12 / P010，其余报 `Invalid output format` |
| 同文件 `config_props` | 输出池 `initial_pool_size=10`，`BindFlags = RENDER_TARGET \| VIDEO_ENCODER`，无缓存：每帧都新建输入视图与输出视图 |
| [`libavfilter/vsrc_ddagrab.c` L841](https://github.com/FFmpeg/FFmpeg/blob/n8.0.1/libavfilter/vsrc_ddagrab.c#L841) | ddagrab 输出帧池追加 `D3D11_BIND_RENDER_TARGET` |
| 该滤镜的来历 | MulticoreWare 2025 年新增（[ffmpeg-devel 讨论](https://ffmpeg.org/pipermail/ffmpeg-devel/2024-December/338000.html)），较新、使用面窄（推测：所以此类 bug 未被覆盖） |

### A.3 证据一：命令行矩阵（lavfi→hwupload→scale_d3d11，固定 `-init_hw_device d3d11va`）

命令模板见 `matrix.sh`，日志 `logs\syn_*.log`，汇总 `logs\matrix_summary.txt`。**该组在 E2 并行负载期间跑，仅供参考（只看成败，不看耗时）。**

| 输入格式 | 输出 | 分辨率 | 结果 |
|---|---|---|---|
| bgra / rgb0 / x2bgr10le | nv12、p010 | 1440p | 全部 `0x887A0004` |
| nv12 | nv12、p010 | 1440p | `0x887A0004` |
| p010le | nv12、p010 | 1440p | `0x887A0004` |
| bgra | nv12 | 1080p、640x360 | `0x887A0004` |
| yuv420p | nv12、p010 | 1440p | hwupload 自己失败（d3d11 不支持该上传格式，与本问题无关） |

说明：`scale_d3d11` 没有 `w=`/`h=` 选项，实际选项名是 `width`/`height`（`matrix.sh` 最后一条用错了，未计入）。

### A.4 证据二：探针（`fourcc-probe`，逐字复刻 ffmpeg 的视图创建，只改 FourCC）

2560x1440，新建独立 D3D11 设备与 VideoProcessor 枚举器，不做任何 Blt（只创建纹理与视图，GPU 占用可忽略）。原始输出 `logsourcc_probe.txt`。

| 纹理 | FourCC=0 | FourCC=DXGI 枚举值（ffmpeg 现状） |
|---|---|---|
| BGRA，`SHADER_RESOURCE\|RENDER_TARGET` | **OK** | **`0x887A0004`** |
| NV12，`RENDER_TARGET\|VIDEO_ENCODER` | **OK** | **`0x887A0004`** |
| BGRA / NV12 / P010，仅 `SHADER_RESOURCE` | `0x80070057`（E_INVALIDARG） | `0x80070057` |

解读：现状写法在本机驱动上必然失败，改成 0 即可创建视图。另外本驱动要求 VP 输入纹理带 `RENDER_TARGET`（仅 SRV 的纹理参数就被拒），这点 ddagrab 满足，`hwupload` 默认是否满足未单独验证。

### A.5 建议的复现实验清单（E2 完成后再跑，均未执行）

前置：`Get-Process` 确认无 `snow-fps-fixture` / `snow-recorder`；ddagrab 只抓按属性校验的副屏；每轮 ≤10s；重复 3 次取平均。

1. **打补丁验证根因**：用 msys2 的 gcc 从 n8.0.1 源码自编 ffmpeg（`--enable-libvpl --enable-d3d11va`，其余精简；libvpl 包已在 `C:	ools\msys64` 的 pacman 缓存里，源码已解压到 scratchpad 的 `research\FFmpeg-n8.0.1`），把 `.FourCC = s->input_format` 改成 `.FourCC = 0`。预期：`lavfi testsrc2 → format=bgra → hwupload → scale_d3d11=format=nv12 → hwdownload` 不再报错，输出 NV12 内容正确。
2. **合成输入转换耗时**（贴近 spike 口径）：补丁版 `-f lavfi -i testsrc2=s=2560x1440:r=600 -vf "format=bgra,hwupload,scale_d3d11=format=nv12,hwdownload,format=nv12" -benchmark -f null -`，同时跑 `scale_d3d11=format=nv12,hwdownload`，与 `hwupload,hwmap=derive_device=qsv,vpp_qsv=format=nv12,hwdownload` 对比。预期：两者都带上传/下载开销，绝对值会高于 spike 的 2.2ms，只能比相对；下载要用同一条尾巴。
3. **整条链路 fps**：补丁版 `ddagrab=output_idx=<副屏>:framerate=60 → scale_d3d11=format=nv12 → hwmap=derive_device=qsv,format=qsv → h264_qsv -preset veryfast -async_depth 2/4`，静止与有运动各一组。预期：不再 0x887A0004；注意 `hwmap` 从 d3d11 到 qsv 依赖输出纹理带 `VIDEO_ENCODER`（滤镜已设置）。若此步仍失败，错误会落在 `hwmap`/QSV 侧，与本根因无关。
4. **不打补丁的旁证**：系统 ffmpeg 加 `-loglevel debug` 与 `-init_hw_device d3d11va`（已在矩阵里用过，错误不变），无需重复。

### A.6 修好后能否优于 vpp_qsv 链（推测，未实测）

- 转换本身：`scale_d3d11` 底层就是同一个 D3D11 VideoProcessor，纯 Blt 应与 spike 的 VP（1440p 约 2.2ms）同量级，和 `vpp_qsv` 相比**没有数量级差别**（推测）。
- 链路层面：它每帧新建输入/输出视图（无缓存）、输出池固定 10 张；VP 走视频引擎，后面 `hwmap` 到 QSV 是同设备共享纹理，理论上可少一次 `derive_device` 的映射成本，但两者都受 ddagrab 60fps 定速限制，**fps 上限几乎一定还是 ~57~60，差异大概率淹没在波动里**（推测）。
- 与选型的关系：**不改变选型建议**。推荐路径本来就是直接调用 VP 做"单 Blt 多图层"，ffmpeg 命令行链路无法并入覆盖层合成（第 5.3 节结论不变）。修好 scale_d3d11 只会让 ffmpeg 链路"可用"，不会让它成为集成路径。
- 是否值得上游反馈：值得，一行补丁、证据确凿，可直接向 ffmpeg-devel 提交（附探针输出）。我们不必等它合入，也不依赖它。**本轮只是判断，没有提交任何东西。**
