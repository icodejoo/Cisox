# 简易视频编辑器后端方案调研（降 fps / 缩放 / 抽帧）

> **已定事项（2026-10-01）：** 输入只处理本软件录制的 MP4（解码只需 h264）；系统引擎与 FFmpeg 引擎并存、用户自选；输出只做 H.264，**H.265 暂不支持**（原因与存档见 `windows-hevc-support.md`）；不新增第三方依赖（`image` 等已在依赖树内，`turbojpeg`、`fast_image_resize` 不采用，系统引擎抽帧走 WIC/ImageIO）。下文中涉及 H.265 输出的内容仅作参考。

> **已定事项补充（2026-10-01，worker 方案）：** 视频编辑**不使用独立 `ffmpeg.exe`，也不单独编译瘦身 ffmpeg.exe**。编辑任务并入现有 worker 进程 `snow-recorder`（静态链接 ffmpeg-next 与 FFmpeg 静态库，主程序不链接 FFmpeg），通过 `snow-recorder-protocol` 扩展编辑任务（降 fps、缩放、抽帧、按关键帧裁剪/重封装）；只保留一份 FFmpeg 本体、一套白名单（`snow-shot-minimal`，本轮不改）、一处授权文档。worker 是否改名（如 `snow-media-worker`）以后再定。两个引擎（系统引擎 / FFmpeg 引擎）联动取消为同一套任务协议下的两种实现，**默认输出均为 H.264**。预编译 worker 靠 CI 发布、本地脚本按哈希下载（不把二进制提交 git）。性能对比结论（原理推断，未实测 exe 对进程内）：录屏硬件路径 exe 方案明显更差，纯变换持平，带合成的导出更差，故录屏保持进程内。落地设计见 `video-editor-mvp-design.md`。**下文 B2（ffmpeg.exe）相关内容、§7 的命令行实测与 §9 推荐中的“进程内 FFmpeg 库”表述均为存档，以 worker 方案为准。**

调研日期：2026-10-01。范围：只调研与写文档，未改代码。
目标功能：降低 fps、修改尺寸（缩放）、提取帧（导出图片）。
优先级：高性能 > 低内存 > 高 fps > 少编译依赖 > 多用系统自带能力。应用许可证 GPL-3.0。

## 1. 结论摘要

- 推荐混合方案：**以现有 FFmpeg 静态库（ffmpeg-next 9.0）为主干，Windows 上把编码交给现有 Media Foundation 硬编后端（mfenc）/ FFmpeg 的 h264_mf 等厂商硬编，抽帧图片编码用纯 Rust 的 `image`/`png`/`zune-jpeg`，缩放在 CPU 侧可选 `fast_image_resize`**。不建议走"纯 Rust 全栈"：现在没有成熟的纯 Rust H.264/H.265 编码器与 H.265/VP9 解码器。
- 仓库里已有 `snow-recording-export`（解码、缩放、帧率重映射、导出）与 `snow-recorder` 的 MF 硬编，MVP 大部分是"新增一个入口 + 新增 demux/seek/抽帧"，不是新写引擎。
- 最大缺口：现有 FFmpeg 白名单构建（`snow-shot-minimal`）在 Windows 上**只启用了 h264 解码器**，没有 hevc/vp9/av1 解码器，没有 jpeg/mjpeg 编码器、`scale`/`fps` 滤镜（走的是 swscale 直调而非 avfilter）。用户导入的任意视频（手机 H.265、网页 VP9/AV1）会打不开。需扩白名单或补系统解码。
- 专利：H.264/H.265 的专利风险不因选哪种方案而消失，只是"谁付费/谁承担"不同：系统 API 路线由微软/硬件厂商的授权覆盖（H.265 在 Windows 上还要用户装商店扩展）；自带 libx264/libx265/解码器的路线由发行方承担；Cisco OpenH264 只有其官方预编译二进制享受 MPEG LA 授权。

## 2. 现有代码盘点（可复用 / 缺口）

已读文件（均在仓库内，行为以代码为准）：

| 位置 | 已有能力 |
|---|---|
| `snow-crates/Cargo.toml:47` | 工作区 `ffmpeg-next 9.0.0`，features 仅 `codec, format, software-resampling, software-scaling`（无 `filter`/`device`） |
| `snow-crates/crates/snow-recording-export/src/editing.rs`（约 1 万行） | `EditingSession::open/export/export_async`；`choose_export_fps` 与 `build_retime_plan_from_index` 做帧率/变速重映射（丢帧 + 时间戳重映射，非插值）；软/硬解码选择（`try_open_hardware_video_decode`，d3d11va、videotoolbox）；`decoded_to_stored_frame` 用 swscale BICUBIC 转 RGBA；导出 Mp4/Avi/Gif/Apng/Webp；进度与取消（`ExportTask`） |
| `…/resize.rs` | `NearestResizePlan`：CPU 最近邻缩放，含 half 特化与线程池，适合预览不适合成品质量 |
| `…/frame_converter.rs`、`streaming.rs` | swscale 上下文 + BT.709 色彩配置；`StreamingEncoderBuilder`（GPU 输入、硬编失败回落软编、`scaled_output_dimensions`） |
| `…/gpu.rs` | D3D11 纹理 → 硬编的零拷贝输入 |
| `snow-shot-rs/tools/snow-recorder/src/win/mfenc.rs`（606 行） | MF SinkWriter + DXGI 设备管理器的 H.264 硬编，MFTEnum2 按 LUID 找硬件 MFT，拒绝静默使用微软软件 MFT，NV12 零拷贝；注释记载 NVIDIA MFT 只认平均码率 |
| `snow-recorder/src/win/vp.rs` | D3D11 VideoProcessor（GPU 缩放/色彩转换） |
| `cmake/vcpkg-overlay-ports/ffmpeg/portfile.cmake:647-661` | Windows `snow-shot-minimal`：`decoder=h264,gif,png,apng,webp,webp_anim`；`encoder=libx264,libx265,h264_mf,h264_nvenc,h264_amf,h264_qsv,mpeg4,gif,apng,libwebp_anim,aac,mp3_mf`；`demuxer=matroska,mov,gif,apng,webp,webp_anim`；`muxer=matroska,mp4,avi,gif,apng,webp`；`hwaccel=h264_d3d11va,h264_d3d11va2,h264_dxva2`；`--disable-network`，仅 `file` 协议；`--enable-gpl` |
| macOS 分支（同文件 667-681） | 额外有 hevc 解码、`h264/hevc_videotoolbox` 编码 |
| `docs/cisox-todo-webm.md` | 已记录：无 libvpx/libaom、无 webm muxer，VP8/VP9/AV1 缺失 |

可直接复用：解码器打开与硬解选择、swscale 缩放上下文、retime（降 fps）、动画图片导出、进度/取消模型、MF 硬编后端、D3D11 VideoProcessor。

缺口：
1. 通用输入：hevc/vp9/av1/mpeg4/mjpeg 等解码器，`avi`/`mpegts` 等 demuxer（白名单无）。
2. 随机访问 seek 与"任意帧"抽取：`editing.rs` 是顺序解码流（日志里有 "decode stream cannot seek to an earlier frame index"），没有对外的按时间戳 seek API。
3. 静态图片导出：白名单没有 `png`/`mjpeg`/`libwebp`(静态) 编码器（有 `png` 解码与 `apng`/`libwebp_anim` 编码），可直接用纯 Rust 的 `image` 系 crate 代替，不必扩 FFmpeg。
4. 以 CPU 内存 RGBA 为中间格式（`StoredFrame`）：编辑器级降 fps/缩放不需要走 RGBA，应保持 YUV 直通，避免内存与带宽放大（1080p RGBA 每帧约 8 MB，NV12 约 3 MB）。
5. 工作区是 snow-crates（Cargo 工作区）与 snow-shot-rs 两处，新增 crate 放哪里需决定（见 §8）。

## 3. 三种方案概述

### A. 系统原生 API

- Windows Media Foundation：
  - `IMFSourceReader` 读取并解码，可自动装载解码器；官方说明只做有限的视频处理（YUV→RGB32 转换、软件去隔行）。来源：[Source Reader](https://learn.microsoft.com/en-us/windows/win32/medfound/source-reader)。
  - seek：`SetCurrentPosition` 不保证精确，视频通常落到目标位置前最近的关键帧，之后需自行读样本并丢弃到目标时间戳。来源：[IMFSourceReader::SetCurrentPosition](https://learn.microsoft.com/en-us/windows/win32/api/mfreadwrite/nf-mfreadwrite-imfsourcereader-setcurrentposition)。
  - Video Processor MFT：色彩空间转换、缩放、去隔行、**帧率转换**、旋转、裁剪都在一个系统 MFT 内。来源：[Video Processor MFT](https://learn.microsoft.com/en-us/windows/win32/medfound/video-processor-mft)。
  - Transcode API（Windows 7 起）：整文件重编码，配置 Profile 即可，无法做抽帧与精细控制。来源：[Transcode API](https://learn.microsoft.com/en-us/windows/win32/medfound/transcode-api)。
  - H.264 解码器支持 Baseline/Main/High，最高 level 5.1。来源：[H.264 Video Decoder](https://learn.microsoft.com/en-us/windows/win32/medfound/h-264-video-decoder)。H.264 编码器支持 Baseline/Main/High。来源：[H.264 Video Encoder](https://learn.microsoft.com/en-us/windows/win32/medfound/h-264-video-encoder)。
  - H.265/HEVC 解码器页面：[H.265 / HEVC Video Decoder](https://learn.microsoft.com/en-us/windows/win32/medfound/h-265---hevc-video-decoder)。Windows 11 上 HEVC 由商店应用"HEVC Video Extensions"提供（付费或设备厂商免费版），VP9、AV1 同理依赖商店扩展（[Microsoft Q&A](https://learn.microsoft.com/en-us/answers/questions/1182851/hevc-video-extensions)，[Simon Mourier 博客](https://www.simonmourier.com/blog/Media-Foundation-cannot-initialise-H-265-HEVC-Video-Decoder-transform-cannot-con/) 记录了离线机器上无法初始化 HEVC 解码 MFT 的现象；这些是二手来源，见开放问题）。
  - 调用方式：`windows` crate 0.62.2（项目已在用，`Win32_Media_MediaFoundation` 已启用）；crates.io 下载 3.4 亿，MIT/Apache-2.0，微软官方维护。
- macOS：AVFoundation（`AVAssetReader` 取样本、`AVAssetImageGenerator` 按时间抽帧、`AVAssetWriter` 写文件）+ VideoToolbox（硬编解码，`objc2-av-foundation` 0.3.2 绑定，项目的 `snow-macos`/`videotoolbox.rs` 已有先例）。本次未逐条核对 Apple 文档，见开放问题。
- Linux：项目当前无 Linux 录屏路径，A 方案在 Linux 无系统级统一 API（VA-API/GStreamer 各自为政），本次不纳入。

### B. FFmpeg

- B1 库绑定：`ffmpeg-next 9.0.0`（WTFPL，2026-08-05 发布，crates.io 累计 747 万下载，GitHub 2004 star，最近提交 2026-09-16）。项目已经依赖，且有 vcpkg overlay 构建静态库。其他绑定：`ffmpeg-the-third 6.0.0+ffmpeg-9.0`（fork，WTFPL，161 star，1.2 万下载量级较小，2026-08-09）；`rsmpeg 0.18.0+ffmpeg.8.0`（MIT，879 star，2025-08-24 后未发布，落后于 FFmpeg 9）。
- B2 调用 ffmpeg 可执行文件：实现最快，但要随包分发 ~100 MB 级可执行文件（gyan full build）或自编精简版；进程间通信、进度解析、取消都要自己做；对 GPL 应用没有许可证障碍（独立进程，只需随附源码获取方式）。精确 API 不可测，错误靠解析 stderr。
- FFmpeg 许可证：`--enable-gpl`（libx264/libx265）使整体变 GPL，与 GPL-3.0 应用兼容；`--enable-nonfree`（如 fdk-aac）不可再分发，避免。

### C. 纯 Rust 生态

见 §5 逐个评估。结论：容器（MP4/MKV）与图片（PNG/JPEG/WebP）与缩放已经成熟；视频编解码只有 AV1（rav1e 编 / rav1d 解）和 H.264 的 C 库包装（openh264，非纯 Rust）可用，H.265/VP9 没有可用的纯 Rust 实现。

## 4. 逐功能评估

### 4.1 降低 fps

| 点 | A 系统 API | B FFmpeg | C 纯 Rust |
|---|---|---|---|
| 丢帧 + 时间戳重映射（推荐默认） | Video Processor MFT 可做帧率转换；或自己读样本、按目标间隔挑帧、重写时间戳。需重编码 | `fps` 滤镜或 `select`+`setpts`；项目的 `retime plan` 已实现等价逻辑（选帧），需重编码 | 取决于编解码器，只有容器层可做 |
| 运动插值（minterpolate、光流） | 无系统能力 | 仅 `minterpolate` 滤镜（极慢，且白名单没开 avfilter）。降 fps 不需要，升 fps 才需要 | 无成熟库 |
| 是否必须重编码 | 是（改变帧时间间隔后 GOP 结构、B 帧依赖都被破坏） | 是，**例外见 §4.4**：若目标 fps 恰为整数倍且只丢"非参考帧"（H.264 的 non-ref B/P 帧），可无损丢帧，但要逐流解析 NAL 的 `nal_ref_idc`，工程量大、覆盖面窄，不建议做 MVP | 同左 |

实测（本机，见 §7）：60→15 fps，`fps=15` 滤镜与 `select`+`setpts` 耗时几乎相同，均 1.2 s（libx264 veryfast，30 s 1080p60 合成片，约 1800 帧→450 帧）。瓶颈是编码，不是选帧。

结论：**默认丢帧 + 重编码**。质量与速度取决于编码器：本机硬编（h264_mf）0.9 s，软编 libx264 veryfast 1.2 s。

### 4.2 缩放

| 点 | A | B | C |
|---|---|---|---|
| 算法 | Video Processor MFT（GPU 上缩放，算法由驱动决定，不可选 lanczos）；`SourceReader` 自带缩放能力有限 | swscale：bilinear/bicubic/lanczos/area 等可选，项目已用 BICUBIC；`scale_cuda`/`scale_d3d11` 等需 avfilter 与硬件帧 | 单帧 CPU：`fast_image_resize 6.1.0`（MIT/Apache-2.0，462 star，2026-07-21，累计 2017 万下载，支持 SSE4.1/AVX2/NEON/WASM SIMD，卷积与 Lanczos/CatmullRom 等），输入是 RGB/YUV 缓冲，不处理编码 |
| GPU 缩放 | 有：D3D11 VideoProcessor 项目已用（`vp.rs`），零拷贝进 MF 硬编 | 需 avfilter + hwaccel，项目构建未启用 | 无 |
| 质量 | 中（驱动实现，下采样可能偏软/混叠，需实测） | 高（lanczos/bicubic，可控） | 高（lanczos/CatmullRom） |
| 速度 | 最快（GPU，但要先有硬解出的 GPU 纹理才成立） | CPU swscale，本机 1080p→720p 带重编码总共 2.1 s（对比仅 fps=1.2 s，缩放额外约 0.9 s/1800 帧≈0.5 ms/帧的量级，含在多线程内） | SIMD，速度与 swscale 同量级，缺点是要在 YUV/RGB 间转换 |

实测：`bicubic` 与 `lanczos` 耗时相同（2.1 s），但输出大小 5.1 MB vs 5.7 MB（lanczos 更锐，码率更高）。

### 4.3 提取帧（导出图片）

| 点 | A | B | C |
|---|---|---|---|
| 按时间戳精确 seek | MF：seek 到前一关键帧后读样本丢弃（需自己实现"解码到目标 PTS"） | ffmpeg 命令行 `-ss` 放在 `-i` 前：先跳关键帧再解码到目标，默认精确；库 API `av_seek_frame(BACKWARD)` + 解码丢弃到目标 PTS，项目缺这层封装 | 容器层 crate 能定位关键帧（mp4/re_mp4/shiguredo_mp4），解码仍要 openh264/ffmpeg |
| 关键帧 vs 任意帧 | 关键帧快；任意帧须解到目标 | 同左。`-skip_frame nokey` 可只解关键帧（缩略图网格类需求最快） | 同左 |
| 批量导出 | 顺序读取 + 选帧，快 | 顺序解码，按间隔选帧（`fps=5`）：实测 150 张 1080p PNG 0.76 s、JPEG(q2) 0.77 s | 图片编码并行化（rayon）容易 |
| PNG/JPEG/WebP | WIC（`windows` crate 可用，Windows 自带）或 MF 输出后再编；macOS ImageIO | 白名单需加 png/mjpeg/libwebp 编码器 | `image 0.25.10`（MIT/Apache-2.0，5886 star，2.0 亿下载）、`png 0.18.1`、`zune-jpeg 0.5.x`（解码，JPEG 编码可用 image 内置或 `turbojpeg 1.5.1`，依赖 libjpeg-turbo C 库）、`image-webp 0.2.4`（**仅无损编码**）、有损 WebP 需 `webp 0.3.1`/`libwebp-sys`（C 库包装，项目已含 libwebp 于 FFmpeg 动画 WebP） |

实测：单帧定点抽取（`-ss 20.5` 在 `-i` 前）0.16 s 含进程启动。三种静态格式的 150 张 1080p（合成内容，仅作相对参考）：PNG 46 MB、JPEG q2 17 MB、WebP(q80) 7.7 MB，WebP 编码耗时 8.7 s，远慢于 PNG/JPEG（0.76 s）。所以 WebP 抽帧要放到线程池并行。

### 4.4 无需重编码的快速路径

| 操作 | 可行性 | 说明 |
|---|---|---|
| 按关键帧裁剪（trim，不改内容） | 可行，极快 | 实测 `-c copy` 截取 10 s 仅 0.12 s。只能落在关键帧边界，起点会前移到前一关键帧（或需要编辑列表/丢弃样本的处理） |
| 仅重封装（MKV→MP4 等） | 可行 | FFmpeg 最简；纯 Rust 用 `mp4`/`shiguredo_mp4` 也可，但 H.264/H.265 码流的 Annex B/AVCC 转换要自己处理（FFmpeg 白名单已含 `h264_mp4toannexb`） |
| 降 fps | 一般不可行 | 见 §4.1，需重编码；仅"丢非参考帧"的特例可无损 |
| 缩放 | 不可行 | 改变像素，必须重编码 |
| 关键帧缩略图 | 可行 | 只解关键帧，最快路径 |

## 5. Rust 生态 crate 评估

数据来自 crates.io API 与 GitHub API（2026-10-01 查询），"活跃度"以最近提交/发布为准。"纯 Rust"指不依赖 C/C++ 代码。

### 5.1 视频编解码与 FFmpeg 绑定

| crate | 版本 / 最近发布 | 下载（累计 / 近期） | star | 许可证 | 纯 Rust | 覆盖 | 硬件加速 | 备注与坑 |
|---|---|---|---|---|---|---|---|---|
| ffmpeg-next | 9.0.0 / 2026-08-05 | 747 万 / 256 万 | 2004 | WTFPL | 否（绑定） | FFmpeg 全部 | 通过 hwaccel/厂商编码器 | 项目已用；上游 API 常随 FFmpeg 大版本破坏；无 avfilter 需开 `filter` 特性 |
| ffmpeg-the-third | 6.0.0+ffmpeg-9.0 / 2026-08-09 | 12 万 / 3 万 | 161 | WTFPL | 否 | 同上 | 同上 | ffmpeg-next 的活跃 fork，更新也勤，社区更小 |
| rsmpeg | 0.18.0+ffmpeg.8.0 / 2025-08-24 | 24 万 / 9 万 | 879 | MIT | 否 | 同上 | 同上 | 更接近 C API 的薄封装；落后 FFmpeg 9 |
| video-rs | 0.12.0 / 2026-09-09 | 41 万 / 13 万 | 427 | MIT OR Apache-2.0 | 否（基于 ffmpeg-next） | 读写/编码/解码高层 API | 无 | README 标明 WIP，可能有 bug，作者另起 `rave` 目标"不依赖 ffmpeg"，尚未成熟（[README](https://github.com/oddity-ai/video-rs)）。项目已有更强的内部封装，不值得再引入 |
| openh264 | 0.9.8 / 2026-08-08 | 126 万 / 79 万 | 129（Cisco 上游 6154） | BSD-2-Clause | 否（C++ 源码随 crate 编译，`source` 特性；或 `libloading` 加载 Cisco 预编译库） | H.264 编+解码 | 无 | 上游 README：编码与解码均只声明 **Constrained Baseline Profile 至 Level 5.2**，解码单线程；编码无 B 帧、单参考帧（[cisco/openh264](https://github.com/cisco/openh264)）。1080p 单线程基准（7950X3D，有 nasm）：编码 8.2 ms/帧，解码 2.8 ms/帧（[openh264-rs README](https://github.com/ralfbiedert/openh264-rs)）。用户的 High Profile 视频能否全部解出未实测。专利见 §6 |
| rav1e | 0.8.1 / 2025-06-16 | 4882 万 / 1593 万 | 4155 | BSD-2-Clause | 基本是（汇编优化可选，需 nasm） | AV1 编码 | 无 | 成熟但编码速度慢；无 AV1 解码 |
| rav1d | 1.1.0 / 2025-05-07 | 5.8 万 / 4.3 万 | 642 | BSD-2-Clause | 否（dav1d 的 c2rust 转写，含 unsafe，大量 Rust 版汇编回退） | AV1 解码 | 无 | 较 dav1d 略慢，memorysafety 项目维护 |
| dav1d (dav1d-rs) | 0.11.1 / 2025-11-25 | 139 万 / 38 万 | 62 | MIT（绑定）+ BSD-2（dav1d C） | 否 | AV1 解码 | 无 | 最快的 AV1 软解，需系统库或 meson 构建，Windows 上编译麻烦 |
| vpx-rs | 0.2.1 / 2025-09-20 | 4859 | — | 需核实 | 否（libvpx 绑定） | VP8/VP9 | 无 | 极不成熟，不推荐 |
| cros-codecs | 0.0.6 / 2025-06-18 | 236 万 / 98 万 | 76 | BSD-3-Clause | 是，但走 VA-API/V4L2 | H.264/H.265/VP8/VP9/AV1 解码（硬件，Linux） | 有（Linux 专属） | **仅 Linux**，对本项目无用 |
| gstreamer (gstreamer-rs) | 0.25.4 / 2026-09-21 | 1114 万 / 244 万 | — | MIT OR Apache-2.0 | 否（绑 GStreamer 运行时） | 取决于插件 | 取决于插件 | 需要分发 GStreamer 运行时，体积与依赖远大于 FFmpeg 静态库，违背"少编译依赖" |
| less-avc | 0.1.5 / 2023-08-29 | 5.5 万 / 0.4 万 | — | MIT/Apache 需核实 | 是 | 无损 H.264 编码（Intra） | 无 | 只做无损/高码率，实用价值极低；停更 |
| scap / nokhwa | — | — | — | — | — | 采集，不涉及编辑 | — | 不相关，不纳入 |

没有成熟的纯 Rust H.264/H.265 编码器，也没有 H.265/VP9 解码器。

### 5.2 容器封装 / 解封装

| crate | 版本 / 最近发布 | 下载 | star | 许可证 | 纯 Rust | 覆盖与坑 |
|---|---|---|---|---|---|---|
| mp4 (mp4-rust) | 0.14.0 / 2023-08-01 | 1375 万 | 356 | MIT | 是 | 读写 MP4，**超 3 年未发布**，仓库 2024-06 后无提交 |
| mp4parse | 0.17.0 / 2023-05-01 | 213 万 | — | MPL-2.0 | 是 | Mozilla 读取器，仅解析，不写；2023 起未发布。MPL-2.0 与 GPL-3.0 兼容（MPL 次级许可条款允许组合到 GPL） |
| shiguredo_mp4 | 2026.5.0（稳定）/ 2026-09-29（canary） | 3.8 万 | 159 | Apache-2.0 | 是 | 读写 MP4/fMP4，新且活跃，社区规模小 |
| re_mp4 | 0.5.1 / 2026-07-08 | 220 万 | 14 | MIT | 是 | Rerun 用的 MP4 解复用（mp4 crate 的 fork，面向解复用） |
| minimp4 | 0.1.2 / 2024-06-19 | 7.8 万 | — | MPL-2.0 | 否（C 包装） | 不建议 |
| matroska-demuxer | 0.8.1 / 2026-08-07 | 17 万 | 23 | Zlib OR MIT OR Apache-2.0 | 是 | 仅 MKV/WebM 解复用 |
| webm | 2.2.1 / 2026-08-25 | 58 万 / 1.5 万 | 25 | MPL-2.0 | 否（libwebm C++ 绑定） | WebM 写入 |
| h264-reader | 0.9.0 / 2026-09-14 | 209 万 | — | MIT/Apache-2.0 | 是 | 解析 H.264 NAL/SPS/PPS，做"丢非参考帧"特例时有用 |
| hevc-parser | 0.6.12 / 2026-08-29 | 8.9 万 | — | 需核实 | 是 | H.265 NAL 解析 |
| symphonia | 0.6.1 / 2026-08-13 | 1522 万 / 730 万 | 3416 | MPL-2.0 | 是 | **音频**解码为主（含 isomp4/mkv 解复用）；无视频解码。项目 dev-dependency 已用 0.5.4 |
| retina | 0.4.20 / 2026-08-14 | 18 万 | 372 | MIT/Apache-2.0 | 是 | RTSP 客户端，不相关 |

### 5.3 图片与缩放

| crate | 版本 | 下载 | star | 许可证 | 纯 Rust | 说明 |
|---|---|---|---|---|---|---|
| image | 0.25.10 / 2026-03-10 | 2.0 亿 | 5886 | MIT OR Apache-2.0 | 是（默认编解码器均纯 Rust） | PNG/JPEG/WebP/GIF 等统一接口；WebP 编码仅无损（经 image-webp） |
| png | 0.18.1 / 2026-02-14 | 2.4 亿 | — | MIT OR Apache-2.0 | 是 | 速度与压缩可配置 |
| zune-jpeg | 0.5.16-rc / 2026-09-08 | 1.2 亿 | — | MIT OR Apache-2.0 OR Zlib | 是 | JPEG 解码，快；编码不在此 crate |
| image-webp | 0.2.4 / 2025-08-27 | 8000 万 | — | MIT OR Apache-2.0 | 是 | 解码有损/无损，编码仅无损 |
| webp | 0.3.1 / 2025-08-29 | 479 万 | — | MIT OR Apache-2.0 | 否（libwebp-sys） | 有损编码；`libwebp-sys 0.14.4`（MIT，2026-04-29）活跃 |
| turbojpeg | 1.5.1 / 2026-07-25 | 298 万 | — | Unlicense OR MIT | 否（libjpeg-turbo） | 最快 JPEG 编码，需 C 库（有 cmake 构建特性） |
| fast_image_resize | 6.1.0 / 2026-07-21 | 2017 万 / 452 万 | 462 | MIT OR Apache-2.0 | 是 | SIMD 缩放，见 §4.2 |
| ravif / avif-serialize | 0.13.0 / 2026-01-19 | 4941 万 | — | BSD-3-Clause | 是（基于 rav1e） | 若要导出 AVIF 帧 |

### 5.4 系统 API 绑定

| crate | 版本 | 说明 |
|---|---|---|
| windows | 0.62.2（2025-10-06） | 微软官方，MF/WIC/D3D11 全覆盖；项目已锁定 `=0.62.2`，保持一致 |
| objc2-av-foundation | 0.3.2（2025-10-04） | AVFoundation 绑定，下载 183 万，较新 |

## 6. 许可证与专利

- 项目是 GPL-3.0。MIT/Apache-2.0/BSD/Zlib/MPL-2.0/WTFPL/Unlicense 许可的 crate 都能合并进 GPL-3.0 整体（Apache-2.0 与 GPLv3 兼容，与 GPLv2 不兼容，本项目为 v3 无碍）。FFmpeg 以 `--enable-gpl` 构建（含 libx264/libx265）整体按 GPL 分发，与项目许可证一致，但要随附对应源码/补丁（项目已有 overlay port）。避免 `--enable-nonfree`。
- 专利编解码器（H.264、H.265）：软件许可证（GPL/BSD）**不授予专利许可**。
  - 用系统 API（MF/VideoToolbox/硬件 MFT）：编解码器由 OS/厂商提供并通常已随 OS/驱动授权，应用方风险最小；但 Windows 上 H.265 解码依赖用户安装商店扩展，不可保证存在。
  - 自带 libx264/libx265（FFmpeg 路线）：发行方自行承担专利风险（MPEG LA / Access Advance / Via LA / Velos 等池）。H.265 的池子多、条款复杂，H.264 相对明确。个人/开源分发的实际诉讼风险低，但商业化前应咨询法务。
  - OpenH264：Cisco 只对**自己提供的预编译二进制**付 MPEG LA 授权费并向下游免费授权（[BINARY_LICENSE](https://www.openh264.org/BINARY_LICENSE.txt)），源码自行编译（crate 默认 `source` 特性）**不享受**该授权；想要授权需用 `libloading` 特性运行时下载 Cisco 二进制。
  - VP9/AV1：免版税编解码器（AV1 有 AOMedia 专利承诺），风险最低；但本项目的 FFmpeg 目前未编入，且 MF 需商店扩展。
- 建议：默认输出 H.264（用系统/硬件编码器），H.265 仅作输入或可选输出，并在 UI/文档注明。

## 7. 实测（本机，方法与局限）

> 注：本节用系统 `ffmpeg.exe` 做命令行实测，仅作相对参考；“自编瘦身 ffmpeg.exe 实测”已放弃，不采用，原因见文首“已定事项补充”。

环境：Windows 11 Pro 10.0.26100，32 逻辑核，NVIDIA GeForce RTX 4060 Laptop GPU，系统 ffmpeg `9.0.2-full_build-www.gyan.dev`（WinGet 安装，含 libx264、h264_mf、nvenc 等）。

方法：用 `testsrc2` 生成 1920x1080、60 fps、30 秒（1800 帧）H.264 测试片（libx264 veryfast crf20 gop120，41.7 MB），各命令单次计时（墙钟，`date +%s.%N`），未重复取均值，`-v error` 且输出到磁盘。

局限：**合成内容**（testsrc2 纯程序图形，熵偏低，解码极快），单次测量无方差，没有测内存峰值（Windows 下未做 working-set 采样），不代表真实录屏或摄影内容。数据只用于相对比较，不作性能承诺。真实结论需用项目实际录屏样片重测。

| 场景 | 命令要点 | 耗时 | 备注 |
|---|---|---|---|
| 仅解码（软件） | `-f null -` | 0.68 s | 约 2600 帧/秒（合成内容偏快） |
| 仅解码（d3d11va） | `-hwaccel d3d11va -f null -` | 2.52 s | 反而更慢：短片下硬解初始化与 GPU→CPU 回传开销大，且没传 `-hwaccel_output_format`，属测试设置的局限，不能得出"硬解慢" |
| 60→15 fps，x264 veryfast crf23 | `-vf fps=15` | 1.22 s | 输出 13.3 MB |
| 同上，`select`+`setpts` 写法 | `select=not(mod(n\,4)),setpts=N/15/TB` | 1.21 s | 与 `fps` 滤镜等价 |
| 缩放 1080p→720p，bicubic | `-vf scale=1280:720:flags=bicubic` | 2.10 s | 输出 5.1 MB |
| 缩放，lanczos | `flags=lanczos` | 2.10 s | 输出 5.7 MB |
| 15 fps + 720p，h264_mf（MF 硬编） | `-c:v h264_mf -b:v 4M` | 0.91 s | 输出 14.3 MB，码率参数在 MF 下不一定被遵守（输出 14 MB 对应约 3.8 Mbps 之外的更高值，需核实码率控制） |
| 15 fps + 720p，h264_nvenc | `-c:v h264_nvenc` | 失败 | **坑**：系统 ffmpeg 9.0.2 要求 NVENC API 13.1 / 驱动 ≥610，本机驱动只有 12.2，报 `Driver does not support the required nvenc API version`。说明自带 FFmpeg 的 nvenc 会与用户驱动版本绑定，必须有回落（项目已有硬编失败回落软编逻辑） |
| 按关键帧裁剪 10 s | `-ss 5 -to 15 -c copy` | 0.12 s | 输出 15.3 MB（关键帧对齐） |
| 抽 150 帧（fps=5）PNG | | 0.76 s | 46 MB |
| 抽 150 帧 JPEG q:v 2 | | 0.77 s | 17 MB |
| 抽 150 帧 WebP q80 | `-c:v libwebp` | 8.66 s | 7.7 MB；有损 WebP 编码明显慢 |
| 单帧抽取 `-ss 20.5 -i` | `-frames:v 1` | 0.16 s | 含进程启动 |

没有测：各 crate 的编译可行性（耗时长，且不改仓库依赖；`ffmpeg-next`、`windows` 已在工作区，`openh264` 的编译需 C++ 工具链，`fast_image_resize`/`image` 属低风险）；MF SourceReader 路线的实测（需写代码）；内存峰值。

## 8. 对比表

评分：优/良/中/差；"现有"指项目已具备。

| 维度 | A 系统 API | B1 FFmpeg 库（现有） | B2 ffmpeg.exe | C 纯 Rust |
|---|---|---|---|---|
| 性能 | 优（硬编 + GPU 缩放，零拷贝） | 良（CPU 软解/缩放；硬编回落已有） | 良（同 B1，多进程开销） | 中~差（H.264 只有 openh264 Baseline；无 H.265） |
| 内存 | 优（GPU 表面池） | 中（项目现以 RGBA 中转，可改 YUV 直通） | 优（独立进程，随退随释放，但峰值不可控） | 取决于实现，图片/缩放部分很省 |
| 编解码覆盖 | 差~中（H.264 全；H.265/VP9/AV1 靠商店扩展；容器少：MP4/部分） | 优（白名单可按需扩；当前仅 h264） | 优（gyan full 全覆盖；自编精简版同 B1） | 差（见 §5.1） |
| 硬件加速 | 优（自动选厂商 MFT / VideoToolbox） | 中（d3d11va 解码、厂商编码器；受驱动版本影响，见 nvenc 坑） | 同 B1 | 无 |
| 跨平台一致性 | 差（Win/mac 两套实现，行为不同） | 优 | 优 | 优 |
| 新增依赖/体积 | 无（系统自带，`windows` 已有） | 已有；扩解码器白名单增大静态库几 MB 级（未测） | 新增可执行文件，约 100 MB（full）或自编精简版 | 少数小 crate；openh264 需 C++ 编译 |
| 许可证与专利 | 最佳（专利由 OS 侧承担） | GPL 兼容；专利发行方承担 | 独立进程，随附源码义务 | 宽松许可；openh264 专利见 §6 |
| 开发工作量 | 大（MF COM 样板多、seek/丢弃逻辑自写；两平台） | 小~中（已有解码/缩放/retime/导出，补 seek 与抽帧） | 最小（拼命令行，但进度/取消/错误处理脆弱） | 大（缺编解码器） |
| 可测试性 | 中（依赖系统编解码器与 GPU，CI 不稳） | 优（纯 CPU 路径可确定性测试，项目有先例） | 中（进程 + 解析 stderr） | 优（纯库单测） |

## 9. 推荐方案

> 以 worker 方案为准：下文 B1 的“库调用”在 worker 进程（`snow-recorder`）内执行，而非主程序进程内；“自编瘦身 ffmpeg.exe”已放弃，不采用，原因见文首“已定事项补充”。

**混合：B1 为主干 + A 的硬编/GPU 缩放作为加速路径 + C 里的图片/缩放 crate。**

分工：
- 解封装与解码：FFmpeg（ffmpeg-next），优先 d3d11va 硬解，失败回落软解；扩白名单到 `hevc`（可选 `vp9`、`av1` 软解，视体积而定）与 `mjpeg` 等常见输入。
- 降 fps：解码后按目标时间网格选帧（复用 retime plan），YUV 直通进编码器；不做插值。
- 缩放：YUV 直通 swscale（bicubic 默认，lanczos 可选）；有 D3D11 纹理时走 VideoProcessor（`vp.rs`）GPU 缩放作为快路径。
- 编码：Windows 优先 `mfenc` 硬编/FFmpeg 厂商硬编，失败回落 libx264（`StreamingEncoder` 已有这套回落）；macOS 用 VideoToolbox（已在白名单）。
- 抽帧：FFmpeg 解出 YUV → 按需缩放 → 转 RGBA → `image`/`png`/`turbojpeg` 并行编码为 PNG/JPEG；有损 WebP 用 FFmpeg 已有的 libwebp 或 `webp` crate。不要把静态图片编码器塞进 FFmpeg 白名单。
- 无重编码快路径：仅"按关键帧裁剪/重封装"走 `-c copy` 等价逻辑（ffmpeg-next 的 packet 级拷贝）。
- 暂不采用：video-rs（重复且 WIP）、gstreamer（依赖太重）、openh264（Baseline 限制、专利不均）、rav1e/rav1d（AV1 需求出现再评估）、ffmpeg.exe（已放弃，不采用）。

为什么不选纯 A：两平台 MF/AVFoundation 各写一套，H.265/VP9/AV1 依赖商店扩展且离线机器上可能初始化失败，导入任意视频的覆盖不足；为什么不选纯 C：缺编解码器。

## 10. MVP 模块划分与阶段计划

建议新建 crate `snow-video-edit`（放 `snow-crates/crates/`，与 `snow-recording-export` 同级，FFmpeg 依赖同处；GPUI 侧界面放 `snow-shot-rs`）。

| 模块 | 职责 | 复用 |
|---|---|---|
| `probe` | 打开文件，读流信息（编码、尺寸、fps、时长、关键帧索引） | `editing.rs` 的 source index、`ffmpeg_util.rs` |
| `seek` | 按 PTS 精确定位（前一关键帧 + 丢弃解码到目标） | `try_open_hardware_video_decoder`、`decode_video_stream_worker` |
| `retime` | 目标 fps 选帧与时间戳重映射 | `build_retime_plan_from_index`、`choose_export_fps` |
| `scale` | 目标尺寸计算（保持宽高比、偶数对齐）与缩放器（swscale / VideoProcessor） | `scaled_output_dimensions`、`frame_converter.rs`、`vp.rs` |
| `transcode` | 解码→retime→scale→编码→封装流水线，进度与取消 | `StreamingEncoderBuilder`、`ExportTask`、`mfenc.rs` |
| `frames` | 抽帧（单帧/按间隔/全部/仅关键帧），图片编码与并行导出 | 新增，用 `image`/`png`/`turbojpeg` |
| `remux` | 关键帧裁剪、拷贝流 | 新增，基于 ffmpeg-next packet |

阶段：
1. P0（半周）：扩 vcpkg 白名单（hevc 解码、mjpeg、必要 demuxer），确认体积增量与构建通过；决定 `ffmpeg-next` 是否加 `filter` 特性（不需要则不加）。
2. P1：`probe` + `seek` + `frames`（单帧与批量抽帧），带确定性测试（合成片断言帧 PTS 与像素）。
3. P2：`retime` + `scale` + `transcode` 接入现有 `StreamingEncoder`（降 fps / 缩放一起做，一次重编码）。
4. P3：`remux` 快路径 + GPUI 界面（进度、取消、预览缩略图）。
5. P4：基准测试用真实录屏样片 + 内存峰值采样，必要时把 RGBA 中转改 YUV 直通。

## 11. 风险

- 输入覆盖：扩白名单后仍可能遇到 HDR、10-bit、可变帧率、带旋转元数据的手机视频，需要各自处理色彩（项目已有 `hdr.rs`）与旋转。
- 可变帧率（VFR）输入下"降 fps"要按 PTS 而非帧序号选帧，`editing.rs` 现有逻辑基于录屏均匀帧，需验证。
- 硬编依赖驱动：nvenc 版本绑定（实测失败）、MF 硬件 MFT 不接受码率控制，必须保留软编回落与明确的错误提示。
- 专利：自带 libx264/libx265 由发行方承担，H.265 输出建议默认关闭。
- 内存：现有解码路径把帧转成 RGBA（约 8 MB/帧 1080p）并有队列深度，长视频批处理需要限制队列并改 YUV 直通。
- 体积：FFmpeg 静态库加解码器后增量未测。
- 精确 seek 在长 GOP 文件上最坏情况要解码整个 GOP，UI 上拖动预览需要缓存或仅取关键帧。

## 12. 开放问题

1. 输入来源范围：只处理本应用录制的 MP4，还是任意用户视频？这决定是否要扩解码器白名单（影响体积与专利面）。
2. 是否需要输出 H.265/VP9/AV1？目前建议只输出 H.264（系统硬编）。
3. 需要音频轨处理吗（降 fps 时音频直通；缩放无影响）？`snow-audio-recorder` 已有 AAC 路径，MVP 建议音频 `copy`。
4. crate 放置：`snow-crates` 工作区还是 `snow-shot-rs`？前者与 FFmpeg 已有依赖同处，后者没有 Qt 包袱但需单独引入 ffmpeg 构建。
5. 是否允许新增依赖 `image` / `turbojpeg` / `fast_image_resize`？按个人规范需先获批；`png`/`image` 也许工作区已有间接依赖，需用 `cargo tree` 确认。
6. 未核实项：Apple AVFoundation 各 API 的官方文档细节；openh264 对 High Profile 输入的实际解码成功率；HEVC/VP9/AV1 在无商店扩展机器上的行为（仅有第三方博客佐证）；`h264_mf` 的码率遵守情况；Windows 下内存峰值。
7. 真实样片基准：需要用户提供代表性录屏（含大面积静止、滚动、视频播放）再做一轮测量。

## 13. 来源清单

- Microsoft Learn：[Source Reader](https://learn.microsoft.com/en-us/windows/win32/medfound/source-reader)、[SetCurrentPosition](https://learn.microsoft.com/en-us/windows/win32/api/mfreadwrite/nf-mfreadwrite-imfsourcereader-setcurrentposition)、[Video Processor MFT](https://learn.microsoft.com/en-us/windows/win32/medfound/video-processor-mft)、[Transcode API](https://learn.microsoft.com/en-us/windows/win32/medfound/transcode-api)、[H.264 解码器](https://learn.microsoft.com/en-us/windows/win32/medfound/h-264-video-decoder)、[H.264 编码器](https://learn.microsoft.com/en-us/windows/win32/medfound/h-264-video-encoder)、[H.265 解码器](https://learn.microsoft.com/en-us/windows/win32/medfound/h-265---hevc-video-decoder)、[MPEG-4 File Sink](https://learn.microsoft.com/en-us/windows/win32/medfound/mpeg-4-file-sink)
- 商店扩展二手来源：[Microsoft Q&A HEVC Video Extensions](https://learn.microsoft.com/en-us/answers/questions/1182851/hevc-video-extensions)、[Simon Mourier 博客](https://www.simonmourier.com/blog/Media-Foundation-cannot-initialise-H-265-HEVC-Video-Decoder-transform-cannot-con/)
- Cisco OpenH264：[README](https://github.com/cisco/openh264)、[BINARY_LICENSE](https://www.openh264.org/BINARY_LICENSE.txt)；[openh264-rs README](https://github.com/ralfbiedert/openh264-rs)
- video-rs：[README](https://github.com/oddity-ai/video-rs)
- 版本、下载、许可证：crates.io API（`https://crates.io/api/v1/crates/<name>`），star 与最近提交：GitHub API（`https://api.github.com/repos/<owner>/<repo>`），均于 2026-10-01 查询；crate 页面：[ffmpeg-next](https://crates.io/crates/ffmpeg-next)、[openh264](https://crates.io/crates/openh264)、[fast_image_resize](https://crates.io/crates/fast_image_resize)、[image](https://crates.io/crates/image)、[rav1e](https://crates.io/crates/rav1e)、[rav1d](https://crates.io/crates/rav1d)、[shiguredo_mp4](https://crates.io/crates/shiguredo_mp4)
- 项目内：`cmake/vcpkg-overlay-ports/ffmpeg/portfile.cmake`、`snow-crates/crates/snow-recording-export/src/*`、`snow-shot-rs/tools/snow-recorder/src/win/mfenc.rs`、`docs/cisox-todo-webm.md`
