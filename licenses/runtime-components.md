# 随包二进制与按需模型的第三方声明

Cargo 依赖由 `scripts/collect-third-party-licenses.ps1` 按 `cargo tree` 自动收集；下面这些不是 Cargo crate，
收集器无法自动发现，通过它的 `-ExtraNotices` 参数把本文件并入许可证包。

## 随包二进制

| 组件 | 用途 | 许可证 | 来源 |
| --- | --- | --- | --- |
| sherpa-onnx 预编译库（`sherpa-onnx-c-api.dll` 等，`snow-stt` 构建期下载） | 语音转文字 | Apache-2.0 | https://github.com/k2-fsa/sherpa-onnx |
| onnxruntime.dll（1.28.x，`snow-stt` / `snow-table` / 翻译共用） | ONNX 推理 | MIT | https://github.com/microsoft/onnxruntime |
| espeak-ng（随 sherpa 预编译包） | 文本前端 | GPL-3.0-or-later | https://github.com/espeak-ng/espeak-ng |
| `snow-table` 依赖的 `ort`（mg-chao 补丁，rev 90018ee5） | ORT 绑定 | MIT OR Apache-2.0 | https://github.com/mg-chao/ort |
| SLANet_plus ONNX（表格结构识别模型，按需下载） | 表格识别 | Apache-2.0（以 `table-model-manifest.json` 记录为准，未另行复核） | https://www.modelscope.cn/models/RapidAI/RapidTable |

`snow-table`、`snow-stt` 的 Cargo 依赖通过 `-CargoManifest` 传入各自 `Cargo.toml` 即可由收集器处理。

## 语音转文字模型（按需下载，不随包；清单见 `snow-shot-rs/crates/snow-shot/resources/stt-model-manifest.json`）

| 模型 | 许可证 | 核对来源（2026-10-08） |
| --- | --- | --- |
| silero_vad | MIT | 清单记录 |
| 各 `apache-2.0` 的 sherpa-onnx Zipformer 模型 | Apache-2.0 | 清单记录（k2-fsa / icefall） |
| x-asr 三款（480ms / 160ms 流式、离线） | Apache-2.0（上游 X-ASR-zh-en） | https://huggingface.co/GilgameshWind/X-ASR-zh-en ；sherpa 导出仓库自身未声明 |
| SenseVoice small | FunASR Model Open Source License | https://github.com/modelscope/FunASR/blob/main/MODEL_LICENSE ；https://github.com/FunAudioLLM/SenseVoice |
| paraformer-zh-small、paraformer-trilingual | 未核实（unverified） | 导出仓库与 ModelScope 上游页均未声明许可；推测为 FunASR 许可，未确认 |

## 内置图标（随程序编译进二进制）

| 图标 | 许可证 | 来源 |
| --- | --- | --- |
| `hearing`（语音转文字聆听指示，`snow-ui-shell/assets/icons/snow/hearing.svg`，14x14 线性耳朵） | **待确认**（用户提供，疑似 Streamline 图标集，使用前需核对其许可证与署名要求） | 用户提供的 SVG；`currentColor` 改为纯黑以便按遮罩着色 |
