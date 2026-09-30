# 录屏 fps 验收:换机接手指南

更新:2026-09-30。给在**另一台电脑**上继续 1440p@60 真屏验收的人(或代理)用。先读本文,再按需读下面两份附件。

## 1. 现状一句话

自建硬件录制流水线(DXGI 采集 + D3D11 VideoProcessor 一次 Blt 直出 NV12 + QSV,采集与合成分设备加栅栏)已完成,单测全过。**真屏 1440p@60 在原机不稳定**(3/9 通过,丢帧率均值 2.98%,硬门 <1%);1080p@30/60、1440p@30 大多数轮次达标。原机有远程控制代理和 DWM 争用核显,**不确定是环境噪声还是代码问题**,所以要换机复测。`DEFAULT_HARDWARE_MODE` 仍是 Off(软编),达标后才改。

## 2. 先读哪些文件(按顺序)

1. `docs/recording-handover/e2-brief-and-data.md` ← **主文件**:最终验收数据、分段耗时、根因、改动文件、复现命令、遗留风险(第 0、2、6、9 节最重要)。
2. `docs/cisox-recording-spike-report.md` ← 四种转换方案(VP/MF/ffmpeg/着色器)的对比和结论;末尾有"可优化点"与 scale_d3d11 排查附录。
3. `docs/recording-handover/contention-research.md` ← 采集/合成设备争用的调研、备选方案和验证实验 V1~V6(§8)。
4. `docs/cisox-gpui-migration-plan.md` 末尾"复审后决策记录"第 11~13 条。

## 3. 目标机器要求

- Windows 10/11 x64,**至少一块非主屏**(夹具只在非主屏建窗,主屏绝不占用)。有 Intel 核显(QSV)最贴近原测试;NVENC/AMF 分支按上游参数写,原机没有这些硬件,**从未验证**,换机若是 N 卡/A 卡请当作"首次验证"。
- 屏幕最好是 2560x1440,刷新率 59/60Hz。其它分辨率需改脚本(见 §5)。
- 工具链:VS 2026 x64 开发者环境(MSVC)、Rust(见 `rust-toolchain.toml`)、libclang(bindgen 用,`LIBCLANG_PATH`)、Python 3、系统 ffmpeg(分析成品用,脚本默认 `C:\ProgramData\chocolatey\bin`,用 `-FfmpegDir` 覆盖)。
- **静态 FFmpeg**:构建脚本默认用 `.tools/vcpkg/installed/static/x64-windows-static`(`.tools/` 不入库)。新机需先:`scripts\bootstrap.ps1 -VcpkgVariants Static`(会装 vcpkg 与静态 ffmpeg,耗时长;下载大文件按用户规范用 aria2)。或设 `FFMPEG_DIR` 指向已有静态 ffmpeg(须含 `lib/avcodec.lib`)。
- 测试前**关闭屏保/自动锁屏**(桌面被锁时 DXGI 复制全部"拒绝访问")。
- 测试前**尽量本地操作,不要开远程控制/远程桌面代理**(原机的主要嫌疑干扰源)。并记录后台进程 CPU 占用。
- 没有其他 GPU 重负载程序(浏览器视频、OBS 等)。

## 4. 操作步骤

```powershell
git clone https://github.com/icodejoo/Cisox.git   # 分支 rust-gpui
cd Cisox
git checkout rust-gpui

# 1) 构建录制进程(产物 snow-shot-rs\tools\snow-recorder\target\release\snow-recorder.exe)
scripts\build-snow-recorder.ps1
# 可选:单测(57 个应全过)
scripts\build-snow-recorder.ps1 -Test

# 2) 构建夹具(独立 workspace)
cd snow-shot-rs\tools\snow-fps-fixture
cargo build --release
target\release\snow-fps-fixture.exe --check              # 必须能识别到非主屏,否则不要继续
target\release\snow-fps-fixture.exe --dxgi-list            # 看副屏对应的 DXGI output_idx

# 3) 自建硬件流水线,四档各 3 轮(通过线:30fps档有效fps>=28.5,60fps档>=56,且丢帧率<1%)
scripts\run-matrix.ps1 -RecorderExe <recorder.exe> -Name hw -Repeats 3 -EnvPairs "SNOW_RECORDER_HARDWARE=1"
# 4) 软编对照
scripts\run-matrix.ps1 -RecorderExe <recorder.exe> -Name sw -Repeats 3 -EnvPairs "SNOW_RECORDER_HARDWARE=0"
```

- 每轮占用副屏约 10 秒;桌面不可复制会自动中止。结果落在 `%TEMP%\snow-fps-matrix`、`%TEMP%\snow-fps-test`。
- 诊断:`SNOW_RECORDER_TRACE=1`(采集慢取帧、DXGI 合并事件、分段耗时)。
- 暂停/取消回归:`scripts\run-pause-test.ps1 -RecorderExe <exe> -Mode pause|eof`。
- 无屏基准(不依赖桌面):`$env:SNOW_RECORDER_SYNTH_BENCH=1; $env:RUST_TEST_NOCAPTURE=1; scripts\build-snow-recorder.ps1 -Test`。
- 可调环境变量:`SNOW_RECORDER_HARDWARE`(0 软编/1 自建硬件/upstream 上游 GPU 路径)、`SNOW_RECORDER_QSV_ASYNC_DEPTH`、`SNOW_RECORDER_QSV_PRESET`、`SNOW_RECORDER_QSV_QUALITY`、`SNOW_RECORDER_MAX_SIZE`(默认 1920x1080,`none` 不限)。

## 5. 换机必须改的地方(重要)

`snow-fps-fixture/scripts/run-fps-test.ps1`(约第 42~43 行)和 `run-pause-test.ps1`(约第 28 行)**硬编码**了"副屏必须在 (2560,0) 且 2560x1440"作为二次防线。新机的副屏坐标/分辨率不同就会直接中止。修改方式:把期望值改成新机副屏的真实 bounds(用 `--check` 输出的 `monitor=Rect {...}` 核对),**保留"坐标是 (0,0) 就中止"的主屏防线**,不要删校验。主屏(Primary=true)不得被占用。

## 6. 通过标准与记录

- 四档都要统计:有效 fps 均值/最小、丢帧率均值/最大、CPU 核数、内存峰值,以及通过轮数。
- **1440p@60 要 ≥56fps 且丢帧 <1%,且多轮稳定**(不能只看安静时段),才把 `settings::DEFAULT_HARDWARE_MODE` 改成 `Gpu`(一行常量,并跑单测)。
- 结果填回 `docs/recording-handover/e2-brief-and-data.md` 第 0 节的表格,或另写 `docs/cisox-recording-retest-<机器>.md`。注明机器型号、显卡、驱动版本、刷新率、是否远程操作、后台高负载进程。

## 7. 如果换机后仍不过(建议排查顺序)

1. 先用 `SNOW_RECORDER_TRACE=1` 看长尾在哪一段(原机:采集取帧 p95 6ms、最大 15ms,其余各段远低于帧预算)。
2. 按 `contention-research.md` §8 做 V1~V3(复测取帧分布、看 GPU 引擎排队、量化拷贝与栅栏耗时)。
3. 试 `SetGPUThreadPriority` 开关对照(推荐顺序第一项,低成本)。
4. 下一轮对照实验:**串行单设备**(OBS/ddagrab 式,DDA 纹理直接作 VP 输入,编码走异步队列)和**方案 Z**(只把 NV12 交给编码设备,须先做跨设备 NV12 共享探针)。详见 spike 报告末尾"可优化点补充"。
5. 做对照时用**交错配对**:基线→变体→基线→变体轮流跑,同时间窗内比较,避免环境漂移被误判为改进。

## 8. 代码里需要知道的

- 入口与结构:`snow-shot-rs/tools/snow-recorder/src/{pipeline.rs,backend.rs,settings.rs}` 与 `src/win/{dda,vp,compose,hwenc,assemble,synthetic}.rs`;软编回退 `soft.rs`。
- `src/queue.rs` 是 E2 被中止时留下的**未接线草稿**(满时丢最旧帧的编码队列,为"串行单设备"变体准备),`main.rs` 未引用,不参与编译;接手时可直接用或删除。
- `dda.rs` 与 `settings.rs`/`backend.rs` 有 E2 中止前的小改动(新增 `SNOW_RECORDER_QSV_QUALITY`、dda 调整),单测 57 个通过,但这部分**没有真屏复测**。
- 没改 `snow-crates`,协议 crate 未改,没有新增第三方依赖。
- 原机采集有一个"早期单设备变体"曾 1440p@60 两轮全过(59.6/59.8fps,丢帧 0.62%/0.30%),见简报第 3 节;说明机器状态对结果影响很大。

## 9. 不要做的事

- 不要占用主屏;测试只在非主屏;每轮 ≤10 秒。
- 不要用 SendInput/SendKeys 模拟输入(此环境已证明无效)。
- 不要跳过校验、不要 `--no-verify`/force push。
