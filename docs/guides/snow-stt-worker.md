# snow-stt 语音转文字工作进程

> 状态：P1 第一步，只有 worker 与协议，尚未接入主程序。背景与选型见 [research/speech-to-text-backends.md](../research/speech-to-text-backends.md)（尤其 §8），原则见 [principles.md](../principles.md)。

## 组成
- `snow-shot-rs/crates/snow-stt-protocol`：主 workspace 成员，零依赖的行文本协议，主程序与 worker 共用。
- `snow-shot-rs/tools/snow-stt`：独立 workspace（自己的 `[workspace]` 与 `Cargo.lock`），二进制 `snow-stt`。依赖 sherpa-onnx 1.13.8（shared 链接，用户已批准新增）、协议 crate、`snow-crates/snow-audio-recorder`（WASAPI 默认麦克风采集与 16k 单声道重采样，不自写）。
- 识别后端是 `SttBackend` trait（`feed` / `poll` / `finish`），sherpa 一份实现；主循环 `session.rs` 只认 trait 和 `AudioSource`，单测用 Fake 后端与脚本化来源，不依赖麦克风和模型。

## 协议
每条消息一行 UTF-8，字段以空格分隔，自由文本放行尾，其中 `\`、换行、回车、制表符转义为 `\\`、`\n`、`\r`、`\t`（Windows 路径里的反斜杠也要双写，用 `Command::to_line` 生成即可）。

| 方向 | 消息 | 说明 |
|---|---|---|
| 主程序 → worker | `START <lang> <threads> <rule1_ms> <rule2_ms> <rule3_ms> <max_seconds> <model_dir>` | 加载模型并开始采集。端点规则含义同 sherpa（2400/1200/20000 为示例默认）；`max_seconds` 为 0 表示不限，超过自动当作 STOP |
| | `STOP` | 停止采集，冲刷尾部，发最后的 `FINAL` 与 `STOPPED` 后退出 |
| | `CANCEL` | 不冲刷，直接 `STOPPED` 后退出 |
| | `PING` | 回 `PONG` |
| worker → 主程序 | `READY` | 进程已就绪，可发 START |
| | `PARTIAL <文本>` | 当前句的临时文本，仅在变化时发 |
| | `FINAL <文本>` | 一句话定稿（端点触发后或 STOP 冲刷时） |
| | `ERROR <原因>` | 单行原因，随后退出（模型缺失、麦克风被拒等） |
| | `PONG` / `STOPPED` | 心跳应答 / 已停止 |

stdin 关闭（主程序退出或崩溃）视为中止：不再发事件，直接退出。进程一次只做一次识别，退出即释放模型与麦克风。首行若带 UTF-8 BOM 会被忽略。

流程：加载完模型才打开麦克风（避免把加载期间的旧音频喂进去）；音频按 320ms（5120 样本）一块喂 sherpa；每块后取结果，文本变化发 `PARTIAL`，`is_endpoint` 为真发 `FINAL` 并重置流；STOP 时补 0.66s 静音、`input_finished` 后冲刷最后一句。

## 模型放置
模型不随仓库打包。目录里需要流式 Zipformer transducer 的四类文件，文件名按前缀识别：`encoder*.onnx`、`decoder*.onnx`、`joiner*.onnx`、`tokens.txt`（encoder/joiner 优先 `int8`，decoder 优先非 int8）。已测模型：`csukuangfj/sherpa-onnx-streaming-zipformer-bilingual-zh-en-2023-02-20`（Apache-2.0）的 `encoder-epoch-99-avg-1.int8.onnx`、`decoder-epoch-99-avg-1.onnx`、`joiner-epoch-99-avg-1.int8.onnx`、`tokens.txt`。下载大文件用 `aria2c -x16 -s16 -k8M --continue=true`。

## 构建
```
scripts\build-snow-stt.ps1 [-Profile release|debug] [-Test] [-Clippy] [-Jobs 2]
```
- 产物默认在 `build/stt/<profile>/snow-stt.exe`（`$env:CARGO_TARGET_DIR` 可改，但**必须是短路径**：sherpa 预编译包解压路径很长，目录太深会静默解压失败，随后链接报 `LNK1104`）。
- sherpa-onnx-sys 的 build.rs **构建期联网**从 GitHub Releases 下载预编译库（shared 约 7.7MB）。离线环境可设 `SHERPA_ONNX_ARCHIVE_DIR`（本地 tar.bz2）或 `SHERPA_ONNX_LIB_DIR`。
- 使用静态 CRT（`+crt-static`），与 sherpa 预编译包（MT）匹配。脚本自带 `-j 2` 与 BelowNormal，并检查自身无 CRLF。
- `-Test` 会把 DLL 放进 `deps/` 再运行测试，因为 shared 链接的测试 exe 启动就要这些 DLL。

## DLL 加载顺序
`sherpa-onnx-c-api.dll` 按名字导入 `onnxruntime.dll`。构建脚本在 exe 旁放三个 DLL：`sherpa-onnx-c-api.dll`、**仓库同版 `onnxruntime.dll`**（取自 `build/mt-quant/ort128/onnxruntime/capi/`，当前 1.28.0.20260724，可用 `$env:SNOW_ORT_DIR` 指向别处；脚本要求版本为 1.28.x）、`onnxruntime_providers_shared.dll`。Windows 默认先搜应用程序目录，所以同目录的这份优先于 PATH 里别的 `onnxruntime.dll`；打包时务必保持三个 DLL 与 exe 同目录。sherpa 自带的 1.28.2 版被替换，替换后的识别结果已在 §8.3 验证一致。

## 运行与自检
手动：
```
snow-stt.exe            # 麦克风；stdin 发 START/STOP
snow-stt.exe --wav a.wav --wav-pad-ms 3000 --stats   # 测试用：wav 代替麦克风，末尾补静音，结束时向 stderr 打统计
```
`--wav` 只接受 16kHz 单声道；wav 读完等价于 STOP。脚本化验证：
```
scripts\verify-snow-stt.ps1 -ModelDir <模型目录> -Wav <wav> [-PadMs 3000] [-Threads 2] [-StopAfterMs 0]
```
走真实 stdin/stdout 协议，打印带时间戳的事件序列、退出码、峰值工作集和每块耗时统计。

## 单测
`scripts\build-snow-stt.ps1 -Test`（worker 16 个，含主循环 Fake 测试）与 `cargo test -p snow-stt-protocol`（协议往返与异常输入）。

## 许可证注意
- sherpa-onnx 与 sherpa-onnx-sys 为 Apache-2.0；传递依赖许可证已核对，无与 GPL-3.0-only 冲突者（见调研 §8.1）。
- 随包的 `sherpa-onnx-c-api.dll` 内含 **espeak-ng**（TTS 用），上游为 **GPL-3.0-or-later**；用于 GPL-3.0-only 工程可行，但**分发时必须在第三方声明里补 espeak-ng 及 sherpa 预编译库其余部分（ORT MIT、kaldi-native-fbank 等 Apache-2.0）的许可证文本**。
- `scripts/collect-third-party-licenses.ps1` 目前只由 `package-snow-shot.ps1` 以 Qt 版的清单调用，**不覆盖 snow-stt**（它按 `-CargoManifest` 逐个清单收集，且不处理 build.rs 下载的预编译库）。接入打包时需单独加入 `snow-stt/Cargo.toml` 并手写预编译库的声明，本步骤未改动脚本。
- 模型权重另有各自许可，不随仓库分发。
