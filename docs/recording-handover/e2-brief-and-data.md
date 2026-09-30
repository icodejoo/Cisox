# 阶段 E2 简报：自建录制流水线（捕获 / 转换合成 / 编码）

**结论先行（如实）**
- 自建流水线（捕获 / 转换合成 / 编码三个 trait 边界，Windows 硬件实现全在 `#[cfg(windows)]`）已完成，真屏验收**已执行**（桌面解锁后，副屏、光标在副屏 (4488,0)，测前无孤儿进程）。
- **硬门槛没有稳定达成**：同一个最终二进制、同一台机器，1440p@60 在安静时段 3/3 通过（59.59fps、丢帧 0.59%），但几分钟后同样的代码 0/6（56.67fps、丢帧 4.17%）。有效帧率（≥56）基本都过，卡的是丢帧率 <1%：每次丢的是 DXGI 把两次呈现合并成一帧（采集侧漏帧），成品 12~14 个序号缺失。结果随本机后台负载（远程控制代理进程 eaio_agent/saio_xtunnel/SRFeature 等、DWM）大幅摆动，我控制不了。
- 所以结论是：**软件侧已把能做的做完（见第 2 节根因与改动），在这台 UHD 770 + 2×1440p 的机器上，1440p60 的丢帧率受 DWM/核显争用的随机性限制，稳定 <1% 不能保证；1080p 两档与 1440p30 大多数轮次达标。** 对照：最轻的第三方消费者（ffmpeg ddagrab + hwdownload）在同样条件下丢 22%，上游软编丢 13~39%。
- （历史结论，原机）因为没有"四档全过"，`DEFAULT_HARDWARE_MODE` 当时保持 Off。**2026-10-01 更新**：在 RTX 4060 机器上四档全过线，已改为 `Auto`（MF -> FFmpeg 厂商硬编 -> 软编），见 `experiment-ledger.md` §8。

## 0. 最终真屏验收数据（最终二进制，夹具 noise，录制 6 秒，`scratchpad\e2\` 与 `%TEMP%\snow-fps-matrix`）

通过线：30fps 档有效 fps ≥28.5、60fps 档 ≥56，且丢帧率 <1%。"通过次数"是**该二进制的全部轮次**。

**自建硬件流水线（HARDWARE=1）**

| 档位 | 通过 | fps 均值 / 最小 | 丢帧率 均值 / 最大 | CPU(核) | 内存 avg/峰 MB |
|---|---|---|---|---|---|
| 1080p@30 | 8/9 | 29.86 / 29.47 | 0.46% / 2.35% | 0.24 | 152 / 167 |
| 1080p@60 | 7/9 | 59.43 / 58.59 | 0.63% / 1.77% | 0.37 | 165 / 183 |
| 1440p@30 | 6/9 | 29.70 / 29.08 | 0.86% / 2.94% | 0.22 | 141 / 154 |
| 1440p@60 | 3/9 | 57.64 / 55.11 | 2.98% / 5.69% | 0.32 | 154 / 171 |

（1440p@60 的 9 轮：前 3 轮 59.59fps/0.59%（3/3 过），后 6 轮 56.67fps/4.17%（0/6）。fps 线几乎总能过，丢帧率是瓶颈。）

**上游软编对照（HARDWARE=0；孤儿进程已清，3 轮）**

| 档位 | 通过 | fps 均值 | 丢帧率 均值 / 最大 | CPU(核) | 内存 avg/峰 MB |
|---|---|---|---|---|---|
| 1080p@30 | 0/3 | 23.71 | 20.3% / 25.4% | 1.46 | 355 / 391 |
| 1080p@60 | 0/3 | 37.21 | 34.1% / 36.1% | 2.81 | 367 / 401 |
| 1440p@30 | 0/3 | 25.19 | 13.2% / 21.8% | 2.48 | 389 / 435 |
| 1440p@60 | 0/3 | 31.56 | 38.9% / 40.1% | 4.34 | 399 / 446 |

硬件 vs 软编：CPU 少 4~14 倍（0.2~0.4 核 vs 1.5~4.3 核），内存约为 40%，帧率与丢帧全面更好。

**分段耗时（1440p@60，`SNOW_RECORDER_TRACE=1`，一轮真屏，ms）**：采集取帧调用本身 563 次超过 1ms（p50 2.3 / p95 6.0 / max 15.2）；采集复制（含栅栏等待）p50 0.12 / p95 0.27 / max 0.81；合成（一次 Blt，含光标层，栅栏 Wait/Signal，Flush）p50 1.02 / p95 2.47 / max 10.0；送帧+取包（QSV）p50 0.26 / p95 0.79 / max 23.7；呈现到被合成线程看到 p50 1.0 / p95 1.9；采集到合成 p50 29.7（设计内的一个槽周期保持）；启动：等首帧 1.5 / 首帧合成 10 / 首帧送编码 1.6。结论：**我们自己的每一段都远低于帧预算；唯一的长尾在 DXGI `AcquireNextFrame` 本身（核显被 DWM/夹具/编码共用时偶发 10~15ms），正好是漏帧的来源。**

**定位实验（解释为什么软件侧到此为止）**
- 采集独占设备 + 栅栏后，取帧慢调用仍在，说明不是设备锁：慢在 DXGI/DWM 内部（核显争用）。
- 把取帧改成阻塞式 `AcquireNextFrame(timeout)` 与零超时轮询 A/B 各 5 轮：3.7~5.4% vs 3.7~4.9%，无差别。
- 质量参数 18 vs 30（QSV 负载变化）：无规律。
- 跳过 Blt / 跳过编码 / 只采集的诊断（已删除）：采集更新数 314~337 随机波动，不能归因到某一级。
- 同条件 ffmpeg ddagrab+hwdownload 参照：丢 22%/22%/24%。
- 有效改进（已保留）：释放桌面帧先于光标采样与 Flush（持有更短）；合成/编码线程不提优先级（提优先级反而更差）；合成设备 GPU 线程优先级 -7；采集线程最高优先级。这些把 1440p60 的典型丢帧率从 ~3%→~1%（安静时段 0.59%）。

**定型架构上的回归复测（真屏）**
- 暂停：暂停 2s 的成品时长 4.70s（期望≈4s+启动余量），序号乱序 0，pts 无空洞。
- EOF 取消：退出码 0，无成品，无残留目录，无残留进程。
- 硬件失败回落（故意给错 QSV 预设）：日志 `windows-hardware 不可用，回落: 打开 h264_qsv 失败: Invalid argument`，后端 `software-x264`，成品时长正常。
- WebP/APNG（走软编路径，尾段修补未回归）：WebP 45 帧总时长 4226ms（录 4s）；APNG pts 跨度 4.13s，有效 10.39fps（动图 10fps）。

## 1. 各项状态

| 项 | 状态 | 说明 |
|---|---|---|
| 三阶段 trait 边界（捕获/转换合成/编码）、初始化装配、热路径无动态分发 | 已做 | `pipeline.rs`；流水线对三个 trait 泛型单态化，控制面（暂停/恢复/停止/取消）才用 `Box<dyn RecordingBackend>` |
| Windows 硬件实现全部在 `#[cfg(windows)]` | 已做 | `win/`：`dda.rs`（DXGI 复制）、`vp.rs`+`compose.rs`（VideoProcessor 一次 Blt 直出 NV12，桌面+光标多图层）、`hwenc.rs`（QSV/NVENC/AMF + MP4 封装） |
| 软编（ffmpeg+x264）作为跨平台回退 | 已做（沿用上游会话） | `soft.rs` 适配上游 `DirectRecordingSession`；GIF/APNG/WebP 仍走它（WebP/APNG 尾段修补不变） |
| 非 Windows `cargo check` | 已做 | `scripts/check-non-windows.ps1`，对 x86_64-unknown-linux-gnu 通过（限制见下） |
| 装配/回落单测（含 VP/NV12 能力检测失败回落软编） | 已做 | `backend.rs` 4 个、`pipeline.rs` 5 个（模拟三阶段端到端）、`win/*` 真机用例 |
| 槽顺延/固定时钟、30fps 结构性丢帧 | 已做 | 改成"每槽取呈现时间不晚于切点的最新帧 + 相位跟踪把切点放在呈现时刻对侧"，仿真含频差/抖动 <1% |
| 采集侧丢帧根因定位与修复 | 已做并真屏复测，残余为 DXGI/DWM 随机漏帧 | 见第 0、2 节 |
| 四档真屏验收（≥95% 目标帧率且丢帧 <1%） | 已执行，1440p60 丢帧率未稳定达标 | 见第 0 节 |
| 硬编自动回落软编 + 日志 | 已做并实测 | 故意给错 QSV 预设，日志 `windows-hardware 不可用，回落: 打开 h264_qsv 失败: Invalid argument`，后端 `software-x264`，成品正常（`scratchpad\e2\fallback-test.txt`） |
| 暂停扣除 / EOF 取消清理 / 时长 | 已做并实测（架构定型前的变体） | 暂停 2s 的成品时长 4.47s（期望≈4s+启动余量）、pts 无空洞、序号 0 乱序；EOF 取消后无成品无残留；模拟流水线单测覆盖暂停不推进 pts |
| 软编优化（x264 去 zerolatency/帧线程、GPU 转换 NV12+x264） | **未做** | 只能靠上游会话，数据是旧基线，见第 4 节；需要时单独立项 |
| snow-crates 改动 | 无 | `docs/cisox-upstream-patches.md` 无需追加；协议 crate 未改 |

默认值（**2026-10-01 更新**：现为 `settings::DEFAULT_HARDWARE_MODE = Auto`，下面是原机当时的结论）：`SNOW_RECORDER_HARDWARE` 未设置时当时是**软编**（`Off`，因为 1440p60 没有稳定过线，未改）。`=1`/`gpu` 走自建硬件流水线（失败自动回落软编），`=upstream` 走上游 GPU 路径（对照），`=0` 强制软编。等第 6 节的矩阵跑完达标，把默认改成 `Gpu` 即可（一行常量）。

## 2. 关键发现与证据

1. **上游"硬编"从来没在我们的调用里跑过**：`SNOW_RECORDER_HARDWARE=1` 时转换线程(2)与异步编码不被 GPU 路径接受，报 `CPU conversion workers do not apply to GPU input` 后静默回落 libx264，E1/C 阶段"硬编异常"的数据其实是软编回退。把转换线程设 0 且同步后，上游 GPU 路径确实跑起来（`selected_pipeline: d3d11`、`h264_qsv`），但 1440p60 只有 24.7fps、丢帧 54%：合成 26.7ms（多 pass VideoProcessor）+ QSV 送帧 11.3ms 全在一个同步工作线程里。
2. **阻塞式/高频轮询采集都会拖垮同设备上的合成与编码**：
   - 自建 DXGI 采集先用阻塞 `AcquireNextFrame`，合成 Blt 被卡成一个帧周期（~15ms）、QSV 送帧 32~63ms —— 阻塞等待期间占着多线程保护的设备锁。
   - 改零超时 + 亚毫秒睡眠后，诊断（`SNOW_RECORDER_TRACE=1`）显示：390~430 次 `AcquireNextFrame(0)` 调用本身耗时 1~8ms（偶发 12~23ms），正好对应合成 `Flush` 与 QSV 提交持锁；DXGI 合并事件（`AccumulatedFrames=2`）每次都发生在采集线程停顿 14ms 以上之后，即真实漏帧。
   - 对策（已实现）：**采集独占设备 A，合成+编码用设备 B，桌面选区复制进共享纹理（NT 句柄），设备间用 D3D11 栅栏在 GPU 侧同步**（A 复制完 `Signal`，B 合成前 `Wait`，B 合成完 `Signal`，A 复用前 `Wait`；CPU 不阻塞、丢弃的帧不会死锁）。本机 UHD 770 上跨设备共享纹理 + 栅栏往返的真机单测通过。
3. **30fps 结构性丢帧**：夹具以 60Hz 重复呈现 30fps 内容，上游"每槽取最新帧"与"先到先得顺延"都会在相位边界处"重复一帧、丢一个序号"。改为用 DXGI `LastPresentTime`（与 vsync 对齐）当呈现时间，相位跟踪器把切点放在相位环最大空档中央（限速滑动，避免跳变），仿真（同速/二倍速/±0.2% 频差/±2ms 抖动）缺号 ≤1%。
4. **环境噪声**：测量期间曾有 4 个孤儿 `find.exe`（Git 自带，来自早先会话，一个是我中途放弃的 `find /`）各占满一个核，已清理。**此前的上游软编/硬编基线（第 4 节）是在它们运行时测的，偏悲观，需要重测。**

## 3. 早期真屏数据（架构定型前的变体，仅作历史；最终数据见第 0 节）

自建流水线 v10（单设备、自建 DXGI 轮询采集、合成单次 Blt + QSV，2 轮；当时选区内没有光标）——四档全部通过：

| 档位 | 有效 fps（两轮） | 丢帧率 | CPU(核) | 内存 avg/峰 MB | 判定 |
|---|---|---|---|---|---|
| 1080p@30 | 30.00 / 29.82 | 0.00% / 0.61% | 0.20 | 197 / 235 | 过 |
| 1080p@60 | 59.82 / 60.00 | 0.30% / 0.00% | 0.30 | 164 / 192 | 过 |
| 1440p@30 | 30.00 / 30.00 | 0.00% / 0.00% | 0.21 | 198 / 235 | 过 |
| 1440p@60 | 59.63 / 59.82 | 0.62% / 0.30% | 0.31 | 172 / 193 | 过 |

之后加入预热缓冲/线程优先级/诊断等改动的 7 轮（g12~g14，同为单设备）：1080p@30 过 1/7、1080p@60 过 4/7、1440p@30 过 4/7、1440p@60 过 0/7（fps 均值 29.6/58.6/29.5/57.8，丢帧率均值 1.2/1.7/0.9/2.7%，最坏 5.4%）。这些轮次里出现的丢帧就是第 2 节第 2 点的设备锁争用与 DXGI 合并（诊断已证实），双设备+栅栏是针对它的修复——**修复后的数据本简报没有**。

## 4. 对照基线（上游路径，均受孤儿 find 进程干扰，仅供参考，需重测）

| 档位 | 上游软编 fps / 丢帧 / CPU核 / 内存峰 | 上游 GPU 路径(conv0+sync) fps / 丢帧 / CPU核 |
|---|---|---|
| 1080p@30 | 23.55 / 19.8% / 1.71 / 391MB | 25.40 / 15.3% / 0.47 |
| 1080p@60 | 37.46 / 33.2% / 3.62 / 408MB | 33.26 / 44.0% / 0.57 |
| 1440p@30 | 21.24 / 17.6% / 3.22 / 432MB | 21.88 / 25.0% / 0.48 |
| 1440p@60 | 33.18 / 35.3% / 4.71 / 436MB | 24.72 / 54.1% / 0.57 |

（上游软编的输出上限 1080p 是这次新增的产品默认：`SNOW_RECORDER_MAX_SIZE`，默认 1920x1080 按选区方向取向，`none` 不限；自建硬件流水线同样遵守，缩放在同一次 VideoProcessor Blt 里完成。）

## 5. 最终架构的无屏基准（不依赖桌面，真实 VP + 栅栏 + QSV，合成源按固定节拍复制噪声纹理；`win/synthetic.rs`，6 秒/档）

| 档位（选区→输出, 光标） | 编码帧/期望 | 成品包数 / 封装时长 | 合成 p50/p95 ms | 送帧 p50/p95 ms | 丢弃 |
|---|---|---|---|---|---|
| 1080p→1080p @30 | 180/180 | 181 / 6.03s | 0.67 / 1.27 | 0.15 / 0.55 | 0 |
| 1080p→1080p @60 | 360/360 | 361 / 6.02s | 0.57 / 1.01 | 0.12 / 0.29 | 0 |
| 1440p→1440p @30 | 180/180 | 181 / 6.03s | 0.63 / 1.32 | 0.13 / 12.27 | 0 |
| 1440p→1440p @60（全屏纯白噪声，QSV 吃满） | 352/360 | 353 / 6.02s | 0.59 / 1.57 | 14.27 / 15.59 | 编码表面池 8 |
| 1440p→1080p @60 + 移动光标 | 360/360 | 361 / 6.02s | 0.57 / 0.99 | 0.13 / 0.28 | 0 |
| 1080p→1080p @60 + 移动光标 | 360/360 | 361 / 6.02s | 0.59 / 1.11 | 0.13 / 0.31 | 0 |

要点：合成只要 ~0.6ms（上游 26.7ms），启动探测首帧合成 7~20ms、首帧送编码 ~1ms；QSV 在"整屏 1440p 纯随机噪声"这种最坏内容下送帧 14ms（约 70fps 上限），真实内容远轻于此。另有真机单测验证：灰底 + 白色不透明光标图层一次 Blt 出 NV12，回读 Y 平面光标区 ≥225、其余保持灰底亮度（`win/vp.rs`）。

## 6. 复现验收的命令

```powershell
# 1) 构建（tool 本地 target）
scripts\build-snow-recorder.ps1        # 产物 snow-shot-rs\tools\snow-recorder\target\release\snow-recorder.exe
# 2) 新硬件流水线，四档各 3 轮并汇总（通过线：30fps>=28.5、60fps>=56，且丢帧率<1%）
cd snow-shot-rs\tools\snow-fps-fixture
scripts\run-matrix.ps1 -RecorderExe <上面的 exe> -Name hw -Repeats 3 -EnvPairs "SNOW_RECORDER_HARDWARE=1"
# 3) 软编对照（同样 3 轮）
scripts\run-matrix.ps1 -RecorderExe <exe> -Name sw -Repeats 3 -EnvPairs "SNOW_RECORDER_HARDWARE=0"
```
`run-matrix.ps1` 每档先走 `run-fps-test.ps1` 的副屏属性校验（Primary=false 且 bounds=2560,0,2560x1440，坐标不符立即中止），每档占屏约 10 秒；发现桌面不可复制（拒绝访问/黑屏）立即中止并提示。诊断细节用 `SNOW_RECORDER_TRACE=1`（采集慢取帧/合并事件）。暂停/取消回归：`scripts\run-pause-test.ps1 -RecorderExe <exe> -Mode pause|eof`。

## 7. 改动文件（均未提交 git；未改 snow-crates、主工作区与协议 crate）

`snow-shot-rs/tools/snow-recorder/`：
- 新增跨平台：`src/pipeline.rs`（trait + 流水线 + 模拟三阶段测试）、`src/backend.rs`（后端抽象/装配/回落）、`src/soft.rs`（上游软编适配）、`src/settings.rs`（环境变量/硬件模式/尺寸上限）、`src/geom.rs`（几何与光标图层）、`src/timeline.rs`（时间线/节拍/相位/队列/QPC 锚点）、`src/os.rs`（计时器精度/线程优先级）。
- 新增 Windows：`src/win/{mod,dda,vp,compose,hwenc,assemble,synthetic}.rs`。
- 修改：`src/main.rs`（只做命令循环，通过 `RecordingBackend`）、`src/plan.rs`（拆出 settings）、`Cargo.toml`（snow-d3d11 / snow-cursor 路径依赖；`ffmpeg-next`（同版本同特性）与 `windows =0.62.2` 作为 Windows 目标直接依赖——均已在依赖树内，无新增第三方 crate）、`scripts/check-non-windows.ps1`（新）。
`snow-shot-rs/tools/snow-fps-fixture/`：`src/{lib,main,win}.rs`（按属性选副屏、`--dxgi-list`、测试期间保持显示器唤醒）、`scripts/{run-fps-test,run-pause-test,run-matrix}.ps1`、`analyze/{summarize_runs,test_summarize_runs}.py`。

## 8. 验证命令与结果

- `cargo test --release`（recorder）：57 通过（含 55→57：新增 geom/timeline/pipeline/backend/win 用例）；`cargo clippy --release --all-targets -D warnings` 干净。
- 夹具：16 单测 + 8 文档测试；Python：`python -m unittest test_summarize_runs test_fps_analyze` 21 通过。
- `scripts/check-non-windows.ps1`：对 `x86_64-unknown-linux-gnu` 通过。**限制**：整包在 Windows 上无法交叉 check（上游 C 依赖 zstd-sys/ffmpeg-sys 需要 Linux 交叉编译器），所以脚本生成临时 crate 只 check 本工具全部平台无关模块（pipeline/backend/geom/timeline/os/settings/tailfix，含测试），软编后端用桩替代；它验证的是 `#[cfg(windows)]` 隔离边界（非 Windows 只装配软编路径，不引用任何 win 模块）。
- 无屏基准：`$env:SNOW_RECORDER_SYNTH_BENCH=1; $env:RUST_TEST_NOCAPTURE=1; scripts\build-snow-recorder.ps1 -Test`（过滤 `synthetic_pipeline`）。

## 9. 遗留与风险

1. **最终架构真屏四档未测**（被锁屏阻塞，见开头）；请解锁后按第 6 节跑，并把硬件模式默认值改为 `Gpu`（若达标）。
2. 上游软编/上游 GPU 基线需在干净环境重测（第 4 节受孤儿 find 干扰）。
3. 软编回退未优化（x264 去 zerolatency/帧线程、GPU 转 NV12 再 x264 都没做）；软编 1440p60 远达不到。
4. QSV 画质/体积未评估（`veryfast` + `global_quality=18`）；NVENC/AMF 分支按上游参数编写但本机没有这些硬件，未验证。
5. 自建硬件流水线的限制（不满足则自动回落软编）：选区必须落在单个显示器内、桌面须为 SDR BGRA、不支持旋转显示器、设备需支持共享纹理与栅栏、适配器需有受支持的硬件编码器。
6. 运行中硬件故障（采集会话失效重建超过 30 次、编码器出错）只能在停止时报错，不能中途回落。
7. 光标异或/反色类形状（MaskedColor）按普通 alpha 叠加，与上游 compute 着色器的掩码合成不完全一致。
8. 启动延迟：首帧探测合成 7~20ms、送编码 ~1ms，加上设备/编码器创建，START 到就绪实测在百毫秒量级（真屏未单独计时）。
