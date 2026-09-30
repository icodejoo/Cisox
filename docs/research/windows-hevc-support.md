# Windows 上 H.265/HEVC 支持现状调研

> **决定（2026-10-01）：暂不支持 H.265，视频编辑与录制的输出只做 H.264。本文作为存档保留，以后可据此恢复。**
>
> **放弃原因：**
> 1. Windows 不内置 HEVC 软件编解码，依赖商店付费扩展；没装扩展的机器，导出的 H.265 文件在系统播放器里可能打不开。
> 2. 只能走显卡驱动的硬件 HEVC 编码器：需要“探测可用”（枚举后还要实际激活试编，注册表里有厂商残留项会误判），没有硬件的机器要置灰，UX 与测试成本高。
> 3. 随包分发 `libx265` 的专利风险落在发行方，应用调用系统硬件编码是否要另付授权没有公开明文，需法务确认。
> 4. 没有干净 Windows（未装扩展）和 Intel/AMD 硬件的实测，提示文案无法定稿。
>
> **恢复时的起点：** 见下文“建议”一节；要点是只走硬件 MFT、FFmpeg 的 `hevc_mf` 必须带 `-hw_encoding 1`、发布构建不启用 `libx265`、先补齐上面第 4 条的实测。

调研日期:2026-10-01。本机:Windows 11 Pro 24H2(build 26100.6899),RTX 4060 Laptop,驱动 566.26(32.0.15.6626),仅 NVIDIA 显卡,系统 ffmpeg 9.0.2(gyan full build)。
本文只含调研与本机实测,未改项目代码。专利/授权部分仅为公开事实整理,**非法律意见**。

## 1. 结论摘要

1. Windows 不内置 HEVC 软件解码/编码,二者都依赖 Microsoft Store 的 "HEVC Video Extensions"(付费约 US$0.99)或 OEM 免费版;本机已装该扩展 v2.4.13.0。
2. 硬件 HEVC 编码 MFT 由显卡驱动自行注册(本机 `NVIDIA HEVC Encoder MFT`,注册表里还有残留的 `AMDh265Encoder`),用 `MFT_ENUM_FLAG_HARDWARE` 枚举即可找到,从机制上不经过 Store 扩展。
3. 之前的 `ffmpeg -c:v hevc_mf` 报 `80004005`,**根因是 ffmpeg 默认选中了扩展自带的软件编码器 `HEVCVideoExtensionEncoder`,它在 ProcessInput 阶段就 E_FAIL**,与像素格式、尺寸、码率、GOP 等参数都无关。加 `-hw_encoding 1` 后选中 NVIDIA 硬件 MFT,一次成功。h264_mf 无此问题。
4. 本机 Windows 原生栈(WinRT MediaTranscoder,走 MF)可正常解码 NVENC/MF/libx265 三种来源的 H.265 MP4,耗时约 0.75 到 0.87 s(2 秒 1080p 素材转 720p),MediaTranscoder 直出 HEVC 也成功。
5. 另发现:`hevc_nvenc` 在本机因驱动 566.26 只提供 NVENC API 12.2 而失败(ffmpeg 9.0.2 要求 13.1),需升驱动,本次按要求未动。

## 2. 资料核实(来源与日期)

| # | 结论 | 来源 | 页面日期/访问 |
|---|---|---|---|
| 1 | MF 有 H.265 编码器 MFT(`Mfh265enc.dll`),输入 NV12/IYUV/YUY2/YV12,输出 HEVC Main 8bit;有认证硬件编码器时 MF 场景"通常优先使用硬件编码器" | [H.265 / HEVC Video Encoder](https://learn.microsoft.com/en-us/windows/win32/medfound/h-265---hevc-video-encoder) | 页面 2018-05-31,更新 2021-08-19;访问 2026-10-01 |
| 2 | MF 有 H.265 解码器 MFT(`hevcdecoder.dll`),Main/Main10/4:2:0,DXVA 支持 DX11/DX12,最大 4096x2304 | [H.265 / HEVC Video Decoder](https://learn.microsoft.com/en-us/windows/win32/medfound/h-265---hevc-video-decoder) | 更新 2021-01-07 |
| 3 | 官方"MF 原生支持格式"表里**没有** H.265(视频编解码只列 H.264/WMV/MPEG-4 等) | [Supported Media Formats in Media Foundation](https://learn.microsoft.com/en-us/windows/win32/medfound/supported-media-formats-in-media-foundation) | 更新 2025-04-15 |
| 4 | `MFTEnumEx` 语义:Flags 三类处理模型 SYNC/ASYNC/HARDWARE;不指定任何处理模型标志时默认只枚举 SYNCMFT;硬件 MFT 必须是异步;OEM 可通过注册表 HardwareMFT\EnableEncoders 关闭硬件编码器枚举 | [MFTEnumEx](https://learn.microsoft.com/en-us/windows/win32/api/mfapi/nf-mfapi-mftenumex)、[Hardware MFTs](https://learn.microsoft.com/en-us/windows/win32/medfound/hardware-mfts) | 2024-02-22 / 2025-03-11 |
| 5 | 商店扩展定位:让任意视频应用在 Windows 10 上播放/生成 HEVC;最低 Windows 10 16299(1709);价格 US$0.99 | [Microsoft Store 页面(重定向到 apps.microsoft.com,动态页,WebFetch 抓不到正文)](https://www.microsoft.com/en-us/p/hevc-video-extensions/9nmzlz57r3t7);价格/要求引自搜索摘要 | 访问 2026-10-01,**价格未能在页面一手确认** |
| 6 | 微软版主回复:HEVC 是授权技术,微软要付版税,为不推高每台 Windows 基础成本而做成低价附加项;官方方案是装"HEVC Video Extensions(付费)" | [Q&A: Why I have to pay for HEVC](https://learn.microsoft.com/en-us/answers/questions/5820992/why-i-have-to-pay-for-hevc) | 发帖 2026-03-13/14 |
| 7 | OEM 版 "HEVC Video Extensions from Device Manufacturer" 面向 OEM 预装(厂商已付授权),功能等同付费版;近年多地商店前台已不可直接获取,网传绕过方式不稳定 | 二手来源:[Windows Latest 2025-07-16](https://www.windowslatest.com/2025/07/16/can-you-get-hevc-codec-for-free-on-windows-11/)、[itechguides](https://www.itechguides.com/can-you-get-the-hevc-codec-for-free-on-windows-11-yes-heres-what-works/) | 未找到微软官方声明,**二手** |
| 8 | x265:GPLv2 或商业许可;许可证只管源码版权,**不涵盖专利**;商业被许可方需另行取得 HEVC 专利许可 | [x265 Introduction](https://x265.readthedocs.io/en/release_4.1/introduction.html) | 访问 2026-10-01 |
| 9 | Access Advance 管理 HEVC Advance 池(宣称 25,500+ 项必要专利);2025-07 公布价格至 2030,2025-12-31 前入池可锁定现价;2025-01 推出 Video Distribution Pool | [Access Advance 2025-07-21](https://accessadvance.com/2025/07/21/access-advance-announces-hevc-advance-and-vvc-advance-pricing-through-2030/)、[2025-01-16](https://accessadvance.com/2025/01/16/access-advance-announces-video-distribution-patent-pool-in-response-to-market-demand/) | 见各链接 |
| 10 | Via LA 的 HEVC/VVC 项目并入 Access Advance(标题为 acquisition);抓取 403,细节未读到 | [Morningstar/Accesswire](https://www.morningstar.com/news/accesswire/1117638msn/access-advance-and-via-licensing-alliance-announce-hevcvvc-program-acquisition) | **只见搜索摘要标题,正文未核实** |
| 11 | Velos Media 联合池 2022 年底解散(BlackBerry、Ericsson、Panasonic、Sharp、Sony、Qualcomm 等退出),此后各自/单独授权 | [IAM](https://www.iam-media.com/article/end-of-velos-joint-licensing-programme-leaves-two-pool-licensing-options-hevc-standard)(搜索摘要) | 正文未核实 |
| 12 | NVENC 报错 "Required 13.1 Found 12.2":ffmpeg 9 按 NVENC API 13.1 编译,需较新驱动 | 搜索摘要:[Arch 论坛](https://bbs.archlinux.org/viewtopic.php?id=314614)、[CachyOS issue #542](https://github.com/CachyOS/distribution/issues/542);本机实测一致 | 二手 |
| 13 | ffmpeg `hevc_mf` 选项:`-hw_encoding`(Force hardware encoding,默认 false)、`-rate_control`、`-scenario`、`-quality`;支持 nv12/yuv420p/d3d11 | 本机 `ffmpeg -h encoder=hevc_mf`(9.0.2);[ffmpeg-codecs 文档](https://ffmpeg.org/ffmpeg-codecs.html) 有 MediaFoundation 章节(页面被截断,细节取本机 help) | 2026-10-01 |

未取得的一手资料:微软官方对"无扩展时 MF 是否可用硬件解码"的明文;ffmpeg trac HWAccelIntro(被反爬拦截);Microsoft Store 现价页面。

## 3. 各调研问题的回答

### 3.1 解码

- 干净 Windows 10/11 不含 HEVC 软件解码,依赖商店扩展(资料 5、6)。各版本(22H2/23H2/24H2/25H2)未见官方说法变化,**未逐版本核实**。
- 本机实测:HEVC 解码 MFT 只枚举到 1 个,`HEVCVideoExtension`(同步,软件 MFT,内部按文档走 DXVA);`MFT_ENUM_FLAG_HARDWARE` 下解码器数量为 0。说明 NVIDIA 驱动不单独注册硬件 HEVC 解码 MFT,硬解是通过扩展解码器走 DXVA。
- 推论:没有扩展时,MF 的 HEVC 解码 MFT 不可用,电影和电视/Photos/Edge 等依赖 MF 的程序无法解 HEVC(Edge 另有自己的硬解路径,**未测**);VLC/mpv/ffmpeg 自带解码器不受影响。此推论**未在无扩展机器上验证**(按要求不卸载系统组件)。

### 3.2 编码

- 软件 HEVC 编码 MFT 也来自扩展(本机 `System32` 下**没有** `mfh265enc.dll`,只有 `mfh264enc.dll`;软件 HEVC 编码器位于 `WindowsApps\Microsoft.HEVCVideoExtension_2.4.13.0_x64__8wekyb3d8bbwe\x64\mfH265Enc.dll`,类名 `H265Encoder.CH265EncoderTransform`)。Win32 文档说的 `Mfh265enc.dll` 是文档口径,本机并非 inbox。
- 硬件 HEVC 编码 MFT 由驱动提供,注册表 `HKLM\SOFTWARE\Classes\MediaFoundation\Transforms` 下可见 `966F107C-...` NVIDIA HEVC Encoder MFT、`5fd65104-...` AMDh265Encoder、`80B80715-...` NVIDIA AV1 Encoder MFT。
- 枚举用法:`MFTEnumEx(MFT_CATEGORY_VIDEO_ENCODER, MFT_ENUM_FLAG_HARDWARE | SORTANDFILTER, NULL, &{MFMediaType_Video, MFVideoFormat_HEVC}, ...)`。注意:不带 HARDWARE 标志默认只返回同步 MFT(即扩展软编码器);硬件 MFT 是异步的,需要处理 `METransformNeedInput/HaveOutput` 事件。
- 硬件 MFT 是否依赖扩展:机制上不依赖(按 HARDWARE 标志独立枚举,实测成功),但**无扩展环境未验证**。

### 3.3 SinkWriter/Transcode 输出 H.265

- 本机(有扩展)实测 WinRT `MediaTranscoder` + `MediaEncodingProfile.CreateHevc` 成功,输出 hevc 1280x720。
- 无扩展的干净 Windows:SinkWriter 在 `SetInputMediaType` 时枚举不到 HEVC 编码器,预期返回 `MF_E_TOPO_CODEC_NOT_FOUND`(0xC00D5212)一类错误;有 NVIDIA/AMD/Intel 驱动提供硬件 MFT 时是否仍可编码未验证。这部分没有一手资料,**属推断**。
- 生成的 HEVC MP4 在无扩展机器上:系统自带播放器/缩略图/Photos 无法解码(推断,同 3.1),第三方播放器可播。

### 3.4 专利/授权(非法律意见)

- 微软公开说法:HEVC 是授权技术,微软需付版税,故做成付费扩展(资料 6)。
- 应用自己调用 MF 硬件 HEVC 编码是否需另付授权:**未找到微软或专利池的公开明文**。公开信息只表明 OEM/厂商为带硬件编码的设备按设备缴费,以及 x265 许可证声明不含专利。这项必须由法务确认。
- 随包分发 libx265/FFmpeg HEVC:x265 的 GPL/商业许可不含专利许可(资料 8);专利池有 Access Advance(含并入的 Via LA 项目)及 Velos 余下的单独授权(资料 9 到 11);池对软件分发者的具体费率/豁免门槛(如年度免费额度)**未核实**。结论是存在专利风险,需法务评估,技术上只做 MFT 路径可规避"我们自己分发编码器"的这一项,但不等于免责。

### 3.5 FFmpeg 侧依赖(Windows)

- `hevc_mf`:依赖 Media Foundation 上有 HEVC 编码 MFT。默认枚举(不加 `-hw_encoding`)会优先取软件 MFT,也就是依赖 Store 扩展,且在本机该软件编码器 ProcessInput 失败;加 `-hw_encoding 1` 取驱动硬件 MFT。
- `hevc_nvenc`:依赖 NVIDIA 驱动提供足够新的 NVENC API(ffmpeg 9.0.2 需要 13.1,本机驱动 566.26 只到 12.2,实测报错)。
- `hevc_qsv`:依赖 Intel 显卡与 oneVPL/MediaSDK 运行库;`hevc_amf`:依赖 AMD 驱动自带 AMF 运行时。两者本机无对应硬件,**未测**。
- `hevc_d3d12va`:本机 ffmpeg 已编入,未测。

## 4. 本机实测

### 4.1 环境

| 项 | 值 |
|---|---|
| 系统 | Windows 11 Pro 24H2,build 26100.6899 |
| 显卡 | NVIDIA GeForce RTX 4060 Laptop GPU,驱动 32.0.15.6626 |
| HEVC 包 | `Get-AppxPackage *HEVC*`:`Microsoft.HEVCVideoExtension` 2.4.13.0,Publisher Microsoft,SignatureKind Store(付费版/OEM 版无法从包信息区分) |
| `-AllUsers` 查询 | 权限不足(拒绝访问),仅当前用户结果 |
| ffmpeg | 9.0.2-full_build-www.gyan.dev |

### 4.2 MFT 枚举(临时 C# 探针 via PowerShell `Add-Type`,放在 scratchpad,未入仓库)

方法:P/Invoke `MFTEnumEx`,按 vtable 读取 `MFT_FRIENDLY_NAME_Attribute` 和 `MF_TRANSFORM_ASYNC`;类别 `MFT_CATEGORY_VIDEO_ENCODER/DECODER`,主类型 Video,子类型 HEVC / H264。

| 查询 | flags | 结果(名称 / 异步) |
|---|---|---|
| HEVC 编码器 | SYNC\|ASYNC\|HW | `AMDh265Encoder`(异步);`NVIDIA HEVC Encoder MFT`(同步属性缺失);`HEVCVideoExtensionEncoder`(同步) |
| HEVC 编码器 | 仅 HARDWARE | `AMDh265Encoder`、`NVIDIA HEVC Encoder MFT` |
| HEVC 编码器 | 仅 SYNC | `HEVCVideoExtensionEncoder` |
| H264 编码器 | 全部 | NVIDIA H.264 Encoder MFT、H264 Encoder MFT(微软软编)、AMDh264Encoder、Microsoft AVC DX12 Encoder |
| HEVC 解码器 | 全部 / 仅 SYNC | `HEVCVideoExtension`(同步);仅 HARDWARE 为 0 |
| H264 解码器 | 全部 | Microsoft H264 Video Decoder MFT(同步);仅 HARDWARE 为 0 |

注意:`AMDh265Encoder` 被列出但本机无 AMD 显卡,是残留注册,说明**"枚举到"不等于"可用"**,必须试着 `ActivateObject` 并设置类型(或试编一帧)才能判定可用。`NVIDIA HEVC Encoder MFT` 读不到 MF_TRANSFORM_ASYNC 属性(探针读的是 activate 对象的属性,实际编码时 ffmpeg 能正常使用,异步属性应在 MFT 实例上读取,此处不下结论)。

### 4.3 hevc_mf 失败根因排查(1080p 30fps 2 秒 testsrc2 输入)

| 用例 | 选用 MFT | 结果 |
|---|---|---|
| 默认(无参数) | HEVCVideoExtensionEncoder | 失败 `failed processing input: 80004005`,产物 257 字节 |
| `-pix_fmt nv12` | 同上 | 失败 |
| nv12 + `-b:v 8M` | 同上 | 失败 |
| yuv420p + `-b:v 8M` | 同上 | 失败 |
| nv12 + `-rate_control cbr` / `u_vbr` | 同上 | 失败 |
| nv12 + `-scenario display_remoting` | 同上 | 失败 |
| nv12 + `-g 60` | 同上 | 失败 |
| 1280x720 / 1918x1078 / 320x240 | 同上 | 均失败 |
| `-hw_encoding 0` | 同上 | 失败 |
| **`-hw_encoding 1`** nv12 8M | **NVIDIA HEVC Encoder MFT** | **成功,2.1 MB,0.55 s** |
| `-hw_encoding 1` + `-g 60` | NVIDIA HEVC Encoder MFT | 成功,4.35x 实时 |
| 对照 `h264_mf` 默认 / nv12 8M | H264 Encoder MFT(微软软编) | 均成功,约 0.23~0.25 s |
| 对照 `libx265` 8M | - | 成功,1.2 s |
| `hevc_nvenc` | - | 失败:Required 13.1 Found 12.2(驱动过旧) |

详细日志显示软件 MFT 的输出/输入类型协商全部通过(支持 IYUV/NV12/I422/I444 输入),失败发生在首帧 `ProcessInput`,返回通用 E_FAIL。

结论:
- **不是**像素格式、尺寸、码率、GOP、rate_control、scenario 问题(全部组合一致失败)。
- **不是**缺扩展(扩展已装,并且被枚举、被激活)。
- 是"`hevc_mf` 默认挑了扩展自带软件编码器,而这个软件编码器在本机无法工作"。根因是该软件 MFT 自身行为(E_FAIL 具体原因未定位),可能与其授权/调用方限制有关,**无一手资料,属待确认**。
- 绕过办法:ffmpeg 加 `-hw_encoding 1`;自己的 MF 代码则用 `MFT_ENUM_FLAG_HARDWARE` 枚举,不使用同步软件 HEVC MFT。

### 4.4 解码本机生成的 H.265 MP4

| 文件 | 来源 | 解码方式 | 结果 |
|---|---|---|---|
| a5.mp4(hevc_mf 硬件,1080p 60 帧) | NVIDIA HEVC MFT | ffmpeg 软解 | 成功,60 帧,0.15 s |
| 同上 | 同上 | ffmpeg `-hwaccel d3d11va` | 成功,0.31 s(含启动) |
| 同上 | 同上 | ffmpeg `-hwaccel cuda` | 成功,0.22 s |
| 同上 | 同上 | WinRT MediaTranscoder(走 MF,转 H.264 720p) | 成功,0.87 s,输出 2.2 MB |
| c2.mp4(libx265 产物) | libx265 | MediaTranscoder | 成功,0.76 s |
| b2.mp4(h264_mf) | 对照 | MediaTranscoder | 成功,0.83 s |
| b2.mp4 -> HEVC | 微软 MF 原生 HEVC 编码 | MediaTranscoder `CreateHevc` | 成功,0.73 s,输出 hevc 1280x720,1.9 MB |

说明:MediaTranscoder 的 HEVC 输出未指明用的是硬件还是扩展软件 MFT,**未区分**。上述解码都在"有扩展"环境下完成,**不能代表无扩展机器**。

## 5. 决策建议

1. **Windows 上 H.265 以"系统/硬件 MFT"为主,不随包分发 libx265。** 理由:本机事实证明硬件 MFT 路径可用且快(0.55 s 编 2 秒 1080p),不引入我们自己分发 HEVC 编码器的专利面;x265 许可证不含专利(资料 8)。
2. **用"探测可用"决定是否显示 H.265 选项,不用"枚举到"。** 流程:`MFTEnumEx(ENCODER, HARDWARE, HEVC)`,对每个候选实际 `ActivateObject` + 设置输出/输入类型(最好再试编 1 帧),全部通过才算可用。不可用时置灰并带提示。已看到残留注册(`AMDh265Encoder`)会造成误判。
3. **不要使用扩展自带的同步软件 HEVC 编码器(`HEVCVideoExtensionEncoder`)。** 本机它首帧即 E_FAIL;且依赖用户装扩展。只走硬件 MFT。
4. **兜底:** 硬件不可用时只保留 H.264;**不建议**为 H.265 加 libx265 兜底,除非产品确定需要并完成法务评估(专利池 + GPL 合规)。若将来确有需求,把 libx265 作为用户自装的可选外部 ffmpeg,而不是随包分发。
5. **UX 提示(草案):**
   - 选项置灰时的原因文案区分两类:"未检测到支持 H.265 的显卡编码器(需 NVIDIA/AMD/Intel 较新驱动)"与"H.265 播放需要系统 HEVC 扩展(Microsoft Store)"。后者用于提示"导出的 H.265 文件在没有 HEVC 扩展的 Windows 上可能无法用系统播放器打开",并给出"可改选 H.264"。
   - 首次选择 H.265 时弹一次性提示兼容性风险。
6. **FFmpeg 白名单:**
   - 启用 `hevc_mf`,但强制带 `-hw_encoding 1`(或自己的 MF 封装),禁止走默认(软件)路径。
   - `hevc_nvenc`/`hevc_qsv`/`hevc_amf` 可作为探测后的优先路径,但需按驱动版本探测(ffmpeg 9.x 对 NVENC 需 13.1,旧驱动会失败),失败自动回落 `hevc_mf -hw_encoding 1`,再回落 H.264。
   - 不启用 `libx265`(不进白名单);若现有构建里有,发布构建中关掉,降低 GPL 与专利面。
7. **解码侧(视频编辑预览/导入 H.265):** 自带 ffmpeg 解码器不依赖系统扩展,预览与导入不受影响;只是导出文件在系统播放器中的兼容性是用户侧问题。

## 6. 开放问题 / 未核实项

- 无 HEVC 扩展的干净 Windows 上,硬件 HEVC MFT 是否仍可枚举并编码、SinkWriter 的实际错误码、系统播放器的实际表现:**均未实测**(本机已装扩展,且不得卸载系统组件)。需要在一台干净虚拟机/测试机上复测,最好有 NVIDIA 与 Intel 各一台。
- 扩展软件编码器 `HEVCVideoExtensionEncoder` 在本机 ProcessInput 失败的具体原因(授权/调用方/线程/特性开关?):未定位,也没有一手资料。可再试用 MF 原生程序(非 ffmpeg)调用它,排除 ffmpeg 包装层问题。
- MediaTranscoder 的 `CreateHevc` 实际用的是硬件还是软件 MFT:未区分。
- 微软 Store 上 HEVC 扩展现价(US$0.99 来自搜索摘要)与 OEM 版当前可获得性:无一手页面确认。
- 按微软或专利池公开说法,应用调用 MF 硬件编码是否需单独付授权:未找到明文,需要法务。
- Access Advance/Via LA 合并后对软件编码器分发者的具体条款、Velos 现状细节:只读到搜索摘要。
- Intel QSV、AMD AMF 路径:本机无硬件,未测。
- Windows 10 22H2、11 22H2/23H2/25H2 的行为差异:未逐版本核实,仅测了 24H2。
- NVIDIA 驱动需升到多少才能被 ffmpeg 9.0.2 的 `hevc_nvenc` 接受:搜索摘要称 NVENC API 13.1 对应驱动约 610,未经一手确认,本次也未升级驱动。

## 7. 复现

- 临时脚本(不在仓库):`C:\Users\ADMINI~1\AppData\Local\Temp\claude\D--workspaces-Cisox\05f4a38b-f65b-48a4-bafa-f776cd0445a9\scratchpad\probe.ps1`(MFT 枚举)、`tc.ps1`/`tc2.ps1`(WinRT MediaTranscoder 解码/HEVC 编码)。
- 关键命令:`ffmpeg -f lavfi -i testsrc2=size=1920x1080:rate=30 -t 2 -pix_fmt nv12 -c:v hevc_mf -hw_encoding 1 -b:v 8M out.mp4`(成功);去掉 `-hw_encoding 1` 则 `80004005`。
