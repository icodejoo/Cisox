# 简易视频编辑器 MVP 设计（worker 方案）

日期：2026-10-01。状态：设计稿，未改代码。调研背景见 `video-editor-backends.md`，H.265 放弃原因见 `windows-hevc-support.md`。

## 1. 目标与非目标

目标：
- 四个功能：降低 fps、缩放、抽帧导出图片、按关键帧无重编码裁剪/重封装。
- 系统引擎与 FFmpeg 引擎并存，用户自选；不可用时置灰或回落。
- 有结构化进度、可取消、错误可读；编辑任务不拖慢、不拖垮主程序。

非目标（本轮）：
- 不做 H.265 / VP9 / AV1 输出；不处理任意来源视频（只处理本软件录制的 MP4，H.264）。
- 不做插值升帧、时间线剪辑、转场、字幕、音频处理（音频轨 MVP 直通，见 §10 开放问题）。
- 不改 FFmpeg 白名单（`snow-shot-minimal`）、不新增第三方依赖。
- 不做 Linux。

## 2. 已定约束

- 不使用独立 `ffmpeg.exe`，不单独编瘦身 ffmpeg.exe。编辑任务并入现有 worker 进程 `snow-recorder`（`snow-shot-rs/tools/snow-recorder`，独立 workspace，静态链接 ffmpeg-next 与 FFmpeg 静态库，主程序不链接 FFmpeg），协议扩展在 `snow-shot-rs/crates/snow-recorder-protocol`。
- 只保留一份 FFmpeg 本体、一套白名单、一处授权文档。worker 是否改名（如 `snow-media-worker`）以后再定。
- 两个引擎：系统引擎（Windows 复用 `win/mfenc.rs` 硬编与 `win/vp.rs` 的 D3D11 VideoProcessor 缩放；macOS 用 AVFoundation/VideoToolbox；抽帧图片编码走 WIC/ImageIO）；FFmpeg 引擎（复用 `snow-recording-export` 的解码/retime/缩放/导出）。
- 输出只做 H.264；输入只认本软件录的 MP4。
- 不新增第三方依赖：`image`/`png`/`zune-jpeg` 已在依赖树；`turbojpeg`、`fast_image_resize` 不采用。
- 录屏保持进程内硬件路径不变（原理推断：exe/跨进程会引入显存回读、管道、重新上传，明显更差；未实测）。
- worker 方案的代价（已接受）：预编译 worker 随代码频繁变化，靠 CI 编译发布、本地脚本按哈希下载，不把 worker 二进制提交 git；没有命令行可手工复现，用日志与测试弥补；重编 worker 的人仍需 libclang（ffmpeg-sys-next 的 bindgen）与 MSVC。ffmpeg-next 只是 Rust 封装层，不是另一份 FFmpeg。

## 3. 总体架构

```
主程序 (GPUI, 不链接 FFmpeg)
  编辑页 UI ──> EditController (提交/进度/取消/结果)
        │  stdin/stdout 行协议 (snow-recorder-protocol)
        ▼
worker 进程 snow-recorder (链接 ffmpeg-next + FFmpeg 静态库)
  main.rs 命令循环 ──> 任务分发
        ├─ 录制: RecordingBackend (现有, 不动)
        └─ 编辑: EditEngine trait (新增)
              ├─ FfmpegEngine  : 解码 -> retime -> swscale -> 编码/封装
              └─ SystemEngine  : Win = mfenc + vp.rs + WIC ; mac = AVFoundation/VT + ImageIO
  共用: probe / seek / 进度上报 / 取消标志 / 中间产物目录(scratch)
```

一个 worker 进程同一时刻只跑一个任务（录制或编辑），主程序按需拉起多个进程做并行，与现有一进程一录制的模型一致。

## 4. 进程与协议

### 4.1 现有结构（已读源码）

- `snow-recorder-protocol/src/lib.rs`：零依赖行协议，一行一条 UTF-8 文本，字段空格分隔，自由文本放行尾。现有类型：`Command`（`Start(StartRequest)`、`Pause`、`Resume`、`Stop`、`Cancel`）、`Event`（`Ready`、`Recording{elapsed_ms,frames}`、`Paused`、`Resumed`、`Finished{path,frames,dropped}`、`Error{reason}`）、`StartRequest`、`MediaFormat`（Mp4/Gif/Apng/Webp）、`ParseError`；辅助 `scratch_dir`/`scratch_file`（`.snow-recording-<pid>` 中间目录）、`single_line`。
- `snow-recorder/src/main.rs`：`main()` 读 stdin 命令循环，`handle(cmd, active)` 分发，`emit(&Event)` 写 stdout，`finish`/`cancel`/`fail`/`exit_idle` 收尾；stdin 关闭即视为主程序退出并清理。
- `snow-recorder/src/backend.rs`：`RecordingBackend` trait（`name/pause/resume/stop/cancel`）、`BackendReport`、`Attempt`、`start_first_available`、`plan_attempts`（按硬件模式排出回落序列）。编辑的“能力探测 + 回落”可沿用 `Attempt` 思路。
- `snow-recorder/src/win/`：`mfenc.rs`（`MfEncoder::open`、`enumerate_hardware_h264`）、`vp.rs`（`VideoBlitter::new/blit`、`VpLayer`）、`assemble.rs`、`hwenc.rs`、`dda.rs`。

### 4.2 扩展（草案，字段以实现为准）

在同一协议 crate 内新增任务类型，沿用行格式与“自由文本放行尾”的规则：

| 方向 | 新增消息 | 含义 |
|---|---|---|
| 主 → worker | `Command::Edit(EditRequest)` | 提交一个编辑任务（进程空闲时才接受，与 `Start` 同理） |
| 主 → worker | `Command::Cancel`（复用） | 取消当前任务，worker 清理中间产物后退出 |
| worker → 主 | `Event::EditProgress{done,total,stage}` | 进度；`total` 未知时为 0 |
| worker → 主 | `Event::EditFinished{path,frames,engine}` | 完成；抽帧时 `path` 为输出目录，`frames` 为导出张数 |
| worker → 主 | `Event::Error{reason}`（复用） | 出错，进程随后退出 |
| 主 → worker | `Command::Probe{input}` / `Event::ProbeResult{...}` | 读取时长、分辨率、fps、关键帧数；能力探测另见 `Command::Capabilities` / `Event::Capabilities{engines...}` |

`EditRequest` 为枚举 `EditOp`（`ReduceFps{target_fps}`、`Scale{width,height,algo}`、`ExtractFrames{mode,format,quality,out_dir}`、`TrimKeyframe{start_ms,end_ms}`）加公共字段（`input`、`output`、`engine: EngineKind`）。`ReduceFps` 与 `Scale` 可合并为一次重编码（降 fps + 缩放一起做）。`EngineKind` 取 `Ffmpeg | System | Auto`。

约束：进度事件限频（例如不超过 10 次/秒）；取消后中间产物走 `scratch_dir` 规则清理，完成后原子改名到最终路径，与录制一致。协议必须保留往返单元测试（沿用 `command_round_trip` 风格）。

## 5. 引擎 trait 设计

```text
trait EditEngine: Send {
    fn kind(&self) -> EngineKind;
    fn capabilities(&self, input: &ProbeInfo) -> EngineCaps;   // 能做哪些 op、有无硬编
    fn run(&mut self, req: &EditRequest, ctl: &TaskCtl) -> Result<EditReport, EditError>;
}
```

- `TaskCtl` 提供 `report(done,total,stage)` 与 `is_cancelled()`，引擎在帧循环里轮询取消。
- `EngineCaps` 按操作给出“可用 / 不可用及原因”：如系统引擎在无硬件 H.264 编码 MFT 时，`ReduceFps`/`Scale` 不可用（沿用 `mfenc` “拒绝静默使用微软软件 MFT”的策略）。
- 选择与回落：`Auto` 先试系统引擎，能力不足或运行期初始化失败（尚未写出任何帧）时回落 FFmpeg 引擎；用户显式选了某引擎且不可用时，UI 置灰该选项并给出原因文案，不静默切换。运行中途失败不做跨引擎接续，直接报错。
- macOS 侧 `SystemEngine` 用 AVFoundation/VideoToolbox 实现，接口相同，P4 再做。

## 6. 四个功能

1. 降 fps：按目标时间网格丢帧（按 PTS 选帧，不用帧序号），不做插值，随后重编码为 H.264。FFmpeg 引擎复用 `snow-recording-export` 的 `choose_export_fps` 与 `build_retime_plan_from_index`；系统引擎自己读样本并挑帧，重写时间戳后送 `MfEncoder`。本软件录屏为基本均匀帧，VFR 仍按 PTS 处理。
2. 缩放：目标尺寸计算保持宽高比并偶数对齐（复用 `scaled_output_dimensions`）。FFmpeg 引擎用 swscale（bicubic 默认，lanczos 可选）；系统引擎用 D3D11 `VideoBlitter`（算法由驱动决定，不可选）。可与降 fps 合并为一次重编码。
3. 抽帧：按时间戳精确 seek（定位到前一关键帧，解码并丢弃到目标 PTS），导出 PNG/JPEG/WebP。模式：单帧、按间隔、仅关键帧。图片编码：FFmpeg 引擎侧用已在依赖树的 `image`/`png`/`zune-jpeg` 等（WebP 有损若需 libwebp，沿用 FFmpeg 已含的 libwebp 或评估后再定，不新增依赖）；系统引擎走 WIC（macOS 走 ImageIO）。批量导出的图片编码放线程池并行（WebP 最慢）。
4. 无重编码快路径：按关键帧裁剪与重封装，基于 ffmpeg-next packet 级拷贝，起点落在前一关键帧并在 UI 明示。只由 FFmpeg 引擎提供；系统引擎对此项标记“不适用”，由调度层自动转给 FFmpeg 引擎（该操作不产生画质差异，不违背“用户自选”）。

## 7. 与现有代码的复用点和缺口

复用（来自 `video-editor-backends.md` §2）：`snow-crates/crates/snow-recording-export/src/editing.rs`（解码器打开与硬解选择、retime、导出、`ExportTask` 进度取消）、`resize.rs`、`frame_converter.rs`、`streaming.rs`（`StreamingEncoderBuilder` 硬编失败回落软编）；`snow-recorder/src/win/mfenc.rs`、`win/vp.rs`；`snow-recorder-protocol` 的 scratch 机制。

缺口：
- 按时间戳精确 seek 的对外 API：`editing.rs` 是顺序解码流，没有对外按 PTS 定位的封装，需新增 `seek` 模块。
- 抽帧模块：单帧/间隔/仅关键帧与图片并行编码，全新。
- YUV 直通：现在解码后转 RGBA 中转（1080p 约 8 MB/帧，NV12 约 3 MB），编辑路径要去掉这层中转。
- worker 内的 crate 依赖：`snow-recorder` 目前 ffmpeg-next features 仅 `codec, format`，编辑路径是否需要 `software-scaling`，或直接依赖 `snow-recording-export`，实现时核对（是否引入循环或过重依赖待评估）。
- 系统引擎的 MF SourceReader 读取端（解码）尚无实现；`mfenc.rs` 只有编码端。

## 8. i18n

用户可见文案（引擎名、置灰原因、进度阶段、错误提示）走 `snow-i18n`（Fluent），在 `snow-shot-rs/crates/snow-i18n/locales/{en-US,zh-CN}` 两目录各加一份 `.ftl`（可新增 `video-edit.ftl`，现有有 `recording.ftl`、`export.ftl` 等），两处键保持一致（`tests/parity.rs` 做对齐检查）。worker 只上报错误码/英文诊断，面向用户的文案由主程序按键翻译；简体用 zh-CN、繁体用 zh-TW。

## 9. 测试策略

- 协议：新增消息的往返与非法输入拒绝单元测试，同 `snow-recorder-protocol` 现有风格。
- 引擎：可离屏、确定性。用合成样片（testsrc 类或程序生成、固定 GOP 与帧数）断言：降 fps 后输出帧数与 PTS 网格；缩放后尺寸（偶数对齐）；抽帧的目标 PTS 与若干像素；关键帧裁剪的起点落在关键帧。
- 系统引擎依赖硬件与系统编解码器，标 `#[ignore]`/运行时探测跳过，不进默认 CI；FFmpeg 引擎测试为默认。
- 取消与错误：任务中途取消后无残留中间目录；损坏输入返回可读错误而非 panic。
- 真实录屏样片基准与内存实测另列（需用户提供样片）。

## 10. 分阶段计划（工作量为粗估，非承诺）

| 阶段 | 内容 | 粗估 |
|---|---|---|
| P0 | 协议扩展（Edit/Probe/Progress/Finished）+ `EditEngine` trait + 能力探测骨架 + worker 内任务分发 | 约 3~4 人天 |
| P1 | FFmpeg 引擎三功能 + 快路径：`probe`、`seek`、降 fps/缩放合并重编码、抽帧、关键帧裁剪；合成样片测试 | 约 8~12 人天 |
| P2 | 系统引擎（Windows）：MF 读取端、`mfenc`/`vp.rs` 复用、WIC 抽帧 | 约 8~12 人天 |
| P3 | UI：引擎选择与置灰、进度/取消、缩略图预览、i18n 三目录 | 约 5~8 人天 |
| P4 | macOS 系统引擎（AVFoundation/VideoToolbox/ImageIO），需实机 | 约 8~12 人天 |
| 横向 | 预编译 worker 的 CI 发布与本地按哈希下载脚本 | 约 3~5 人天 |

## 11. 风险与开放问题

风险：
- 预编译 worker 产物变动频繁，哈希下载与 CI 发布链路是新增基础设施；本地无命令行复现，排错靠日志。
- 跨进程没有 exe 对进程内的实测对比，性能结论是原理推断，需 P1 后用真实样片验证。
- 系统引擎：MF 硬编码率控制限制、驱动差异（见 `experiment-ledger.md`）；长 GOP 精确 seek 最坏解整个 GOP。
- 关键帧裁剪起点前移，需 UI 明示。

已决定（2026-10-01，用户授权按项目第一原则“高性能 > 低内存 > 高 fps > 少编译依赖 > 多用系统能力”裁决）：
1. **worker 暂不改名**，沿用 `snow-recorder`。改名只有成本没有性能收益；P1 完成后再评估。
2. **音频轨 MVP 直通拷贝**（`-c:a copy` 语义），不解码不重编码，MVP 不做 AAC 重编码。理由：性能与内存最优。
3. **协议沿用行文本**，不引入新的序列化依赖（少依赖）。
4. **抽帧 MVP 只提供 PNG、JPEG、无损 WebP**，不提供有损 WebP。理由：本机实测抽 150 帧有损 WebP 约 8.7s，而 PNG/JPEG 约 0.76s，慢一个数量级，违背性能优先；有需要再作后续可选项。
5. **`Auto` 默认顺序：系统引擎优先，FFmpeg 回落**。理由：系统引擎内存更低、零额外依赖，符合“多用系统能力”；不可用或不支持的功能回落 FFmpeg。
6. **快路径允许自动转给 FFmpeg 引擎**：按关键帧裁剪（不重编码）是最快的路径，系统引擎做不了，用户选系统引擎时自动转 FFmpeg 引擎并在界面/日志注明，不让用户为此去切引擎。

仍待外部输入：
1. 需要代表性录屏样片做基准与内存实测（在拿到前用合成样片）。

## 12. 系统引擎实现与首轮基准（2026-10-03）

状态：Windows 系统引擎（`EngineKind::System`）已实现，代码在 `snow-shot-rs/tools/snow-recorder/src/edit/system/`。

### 12.1 做了什么

- 探测：`IMFSourceReader` 以压缩直通方式扫描全部视频样本，统计帧数、关键帧、时长、帧率；带 B 帧的 MP4 首帧显示时间不是 0，起点已扣除，与 FFmpeg 引擎对齐。
- 精确 seek：`SetCurrentPosition` 到目标前的关键帧，再解码到目标时间戳，返回"目标时刻正在显示"的那一帧；近距离向前解码不重复 seek。解码输出 NV12，只对最终选中的帧做 NV12 到 BGR24 转换（中间被丢弃的帧不付转换代价）。
- 抽帧：PNG / JPEG 走 WIC（与 FFmpeg 引擎共用同一个 WIC 写入器），并行编码、中间目录发布、取消清理规则与 FFmpeg 引擎一致。
- 降 fps / 缩放：SourceReader 解码 NV12，按 PTS 网格挑帧（复用 `FpsGrid`），缩放与裁掉解码缓冲的对齐行由独立的视频处理器 MFT 完成，样本交给带硬件 H.264 MFT 的 SinkWriter 封装 MP4；音频压缩样本原样直通。码率沿用录制侧的"每像素每帧比特数"平均码率估算。
- 能力：无硬件 H.264 编码 MFT 时降 fps / 缩放标不可用（不使用微软软件 MFT）；无损 WebP、关键帧裁剪恒标不可用。`EngineCaps` 新增 `extract_webp_lossless` 字段，`EditEngine` 新增 `probe`，`EditError` 新增"可回落"标志。
- 选择策略：`Auto` 先试系统引擎，能力不支持或启动阶段失败（打不开输入、无硬件编码器等，尚未产出结果）时回落 FFmpeg；取消和运行中途失败不回落；显式选 `System` 遇到不支持的操作返回明确错误（如"system引擎不支持该操作: 系统引擎不支持无重编码裁剪"），不静默切换。`edit::probe`（`PROBE` 命令）改为系统引擎优先、失败回落 FFmpeg。两引擎共用同一取消标志；默认输出均 H.264。

### 12.2 踩到的坑（实测）

- SourceReader / 视频处理器会"好心"改时间戳：给缩放用的视频处理器声明的帧率必须是源的真实帧率（不是目标帧率，也不能用 MP4 原生类型里的帧率——带音轨的文件上它是样本数除以轨道时长，偏了），否则会重复帧或丢帧。
- MF 的 MP4 解复用对末样本时长可能给 0，直接累加会把总时长算短一帧、帧率算偏；用上一帧时长顶替。
- H.264 解码器的缓冲高度对齐到 16（1080 对 1088），类型里却写 1080；下游按类型高度解释会让色差平面错位或编码出 1088 高的流。现在预读首样本，按缓冲长度推出真实行数，交给视频处理器按源区域裁剪。
- 音频流必须在读任何视频样本之前选好，否则读取器已越过的音频样本不会补发（输出变成无音轨）。
- 降 fps 的网格中点（30 到 15fps 的奇数帧）受 MF 时间戳截断到整 hns 影响会判到前一个槽位，已补 1 hns 抵消，选帧结果与 FFmpeg 引擎一致。
- 带 B 帧时 `SetCurrentPosition` 的绝对位置必须小于媒体源报告的演示时长（不含 B 帧后移），否则被拒。
- 离线转码保持 SinkWriter 默认节流：关掉节流时输入队列无限积压，1080p 样片峰值工作集从约 300 MB 涨到约 650 MB。

### 12.3 与设计的偏差

- 缩放没有用 `win/vp.rs` 的 D3D11 `VideoBlitter`，用的是 Media Foundation 视频处理器 MFT（系统内存路径，算法由系统决定）。原因：读取端本来就在系统内存，上传显存再回读不划算，且 `VideoBlitter` 绑定了录制的帧池。`mfenc.rs` 里只复用了辅助函数（`pack_pair`、`target_bitrate`、`configured_bpp`、`enumerate_hardware_h264`、`verify_hardware_transform`），没有复用 `MfEncoder`（它绑定 D3D11 表面）。
- 无损 WebP 不探测 WIC 是否装了 WebP 编码器，直接标不可用：商店扩展的 WebP 编码器可选安装，且无法确认能指定无损。需要时由 `Auto` 回落 FFmpeg。
- 系统引擎重编码用平均码率，FFmpeg 引擎用 CRF 20，两者画质与体积不可直接比较。
- NV12 到 BGR24 为自写整数转换（按类型里的矩阵与范围选系数，色差取最近邻），与 swscale 的结果有小幅差异（测试容忍平均差不超过 5）。

### 12.4 首轮基准（合成样片，不可外推）

- 机器：Intel Core i5-13500（20 线程），GPU 为 Intel UHD Graphics 770（硬件 H.264 编码 MFT 即其核显），Windows 10 19045。
- 样片：测试工具现生成的合成样片，纯灰色帧（亮度逐帧递增，没有任何真实画面内容和运动），1920x1080 @30fps，240 帧（8 s），GOP 60，带 AAC 静音音轨，0.73 MB。尚无真实录屏样片；这类平坦内容对软件编码（x264）几乎没有压力，对硬编也不代表真实码率，下表的耗时只反映流水线开销。
- 方法：`bench_engines`（`#[ignore]`，不进默认测试）对每个"操作 x 引擎"各起 5 个子进程（子进程为测试可执行文件自身，优先级 BelowNormal，一次只跑一个任务），量墙钟耗时与进程峰值工作集，取耗时中位数、峰值最大值；release 构建。基线（测试框架本身的峰值工作集）约 11 MB，未扣除。
- 运行：`scripts/build-snow-recorder.ps1 -Profile release -Test` 编出测试可执行文件后，`snow_recorder-<hash>.exe --ignored --exact edit::system::bench::bench_engines --nocapture`；环境变量 `SNOW_BENCH_RUNS` / `SNOW_BENCH_FRAMES` / `SNOW_BENCH_OUT`。

| 操作 | 引擎 | 耗时中位数 ms | 峰值工作集 MB | 输出帧/张数 |
|---|---|---|---|---|
| 降到 15fps | system | 1799 | 306 | 121 |
| 降到 15fps | ffmpeg | 773 | 683 | 121 |
| 缩放到 1280x720 | system | 2508 | 275 | 240 |
| 缩放到 1280x720 | ffmpeg | 1925 | 295 | 240 |
| PNG 每 250ms 一张 | system | 920 | 230 | 32 |
| PNG 每 250ms 一张 | ffmpeg | 622 | 91 | 32 |
| JPEG 每 250ms 一张 | system | 902 | 216 | 32 |
| JPEG 每 250ms 一张 | ffmpeg | 398 | 77 | 32 |

读法与结论（仅限这台机器、这段合成样片）：
- 耗时：系统引擎在四项上都慢于 FFmpeg 引擎（降 fps 约 2.3 倍，缩放约 1.3 倍，抽帧约 1.5 到 2.3 倍）。抽帧慢的原因推测是 MF 软件解码比 FFmpeg 多线程解码慢，加上系统引擎要先整段扫描压缩样本取帧表（未做剖析）。该样片对 x264 几乎没有压力，所以降 fps / 缩放上的差距可能被夸大，真实画面下需要重测。
- 内存：只有降 fps 一项系统引擎明显更低（306 对 683 MB，原因未剖析，推测与 x264 的多线程与前瞻缓冲有关）；缩放持平；抽帧系统引擎更高（216 到 230 对 77 到 91 MB，系统引擎每个待编码帧是 6 MB 的 BGR24 缓冲且队列里有若干个）。
- 因此"系统引擎内存更低"的设计假设（§11 第 5 条）目前只在降 fps 上得到支持，"系统引擎更快"则不成立；是否维持 `Auto` 系统优先，等拿到真实录屏样片的复测再定。
- 未做剖析与优化：本轮没有分析时间花在解码、颜色转换、缩放还是编码上，也没有把解码与编码放到不同线程，结论可能随优化改变。
