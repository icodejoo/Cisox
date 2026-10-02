# 文档索引

新人或新会话从这里开始。**续做任务先读「当前入口」**；做裁决先读「原则」。新增或改动文档时同步更新本索引。

## 原则
- [principles.md](principles.md) — **总原则 / 第一原则 / 根本原则**（三词同义）及执行原则、红线、能力降级。唯一出处。

## 当前入口
- [cisox-progress-handoff.md](cisox-progress-handoff.md) — 进度交接。开头是 2026-10-03 暂停点真实状态，**最新待办清单在 §5.1**；功能对齐程度看 [research/qt-parity-audit.md](research/qt-parity-audit.md)。

## 方案与决策
- [cisox-gpui-migration-plan.md](cisox-gpui-migration-plan.md) — Snow Shot → Rust + GPUI 改造方案（v2.0，含 ADR 与已拍板决策）。有效。
- [cisox-upstream-patches.md](cisox-upstream-patches.md) — 对 upstream 共享代码的补丁清单。有效。
- [cisox-migration-acceptance-report.md](cisox-migration-acceptance-report.md) — 迁移验收报告。**已失实，不作验收依据**（见迁移方案 §845 第 10 条），待重写。
- [snow-shot-releases.md](snow-shot-releases.md) — 发布与更新规格（有自己的优先级，见 principles.md 末尾）。

## 录屏
- [cisox-recording-handover.md](cisox-recording-handover.md) — 换机接手指南：构建、夹具、验收怎么跑。有效。
- [recording-handover/experiment-ledger.md](recording-handover/experiment-ledger.md) — 实验台账（含 §8.6 Intel UHD 770 复测与跨屏数据）。有效，最新。
- [recording-handover/e2-brief-and-data.md](recording-handover/e2-brief-and-data.md) — 阶段 E2 自建流水线简报与数据。历史，部分结论已被台账 §8 取代。
- [recording-handover/contention-research.md](recording-handover/contention-research.md) — 采集设备锁争用的替代方案调研。历史参考。
- [cisox-recording-spike-report.md](cisox-recording-spike-report.md) — BGRA→NV12 转换方案对比 spike。历史参考。
- [cisox-todo-webm.md](cisox-todo-webm.md) — WebM 录制待实现说明。待办。

## 操作手册
- [guides/ocr-samples.md](guides/ocr-samples.md) — OCR 真实样片的位置、格式、核对稿局限、同图对比怎么跑与当前结果。有效。

- [guides/ocr-model-tiers-benchmark.md](guides/ocr-model-tiers-benchmark.md) — 本地 OCR 七档模型同图实测：CER、耗时、worker 内存、DirectML 对比、复现命令。有效。

- [guides/translation-quantization-benchmark.md](guides/translation-quantization-benchmark.md) — 本地翻译模型 int8 / int4 量化实测（mul-mul、NLLB-600M 6/14 语言裁剪版）：质量、体积、内存、延迟、性价比拐点、ORT 算子兼容与 worker 改动清单。有效。
- [guides/translation-model-release.md](guides/translation-model-release.md) — 翻译模型最终选型、release 发布方式（A 现成文件 + B 自行生成）、CC-BY-NC 授权声明与应用内提示要求、产物规格与待办。有效。
- [guides/translation-hymt2-eval.md](guides/translation-hymt2-eval.md) — Hy-MT2-1.8B（Apache-2.0）int4 的 ONNX 导出与质量评测，对比 NLLB 14 语言 int4：核心 11 向 +2.4、英→中 +16.9，代价是体积 1.3 GiB、内存约 3 倍、延迟约 5 倍；含导出补丁与分词注意点。有效。
- [guides/snow-stt-worker.md](guides/snow-stt-worker.md) — 语音转文字 worker（snow-stt）：行协议、构建（短 target、联网下载、DLL 同目录）、模型放置、wav 自检脚本、espeak-ng GPL 声明注意。P1 第一步，未接主程序。

- [research/qt-parity-audit.md](research/qt-parity-audit.md) — **Qt 功能清单 vs Rust 实现对照审计（2026-10-03，读代码）**：82 项状态、配置键消费情况、缺口前十、真机验证清单。功能对齐程度以它为准。有效。
## 调研
- [research/cross-monitor-recording-hw.md](research/cross-monitor-recording-hw.md) — 跨屏/双屏选区录屏硬编方案，含裁决记录。探针已实现，跨屏 16/20（单屏对照 18/20），接缝与光标已验证；跨适配器待验证。
- [research/video-editor-backends.md](research/video-editor-backends.md) — 视频编辑器后端调研。
- [research/video-editor-mvp-design.md](research/video-editor-mvp-design.md) — 视频编辑器 MVP 设计（worker 方案）。
- [research/system-ocr-translate-backends.md](research/system-ocr-translate-backends.md) — 系统 OCR/翻译与可选后端方案。P0 开发中。
- [research/local-ocr-model-options.md](research/local-ocr-model-options.md) — 本地 OCR 模型/方案选型：PP-OCRv6 官方数据与其它候选对比，结论是默认 small 合理，附最小验证计划。
- [research/local-translation-model-options.md](research/local-translation-model-options.md) — 本地离线翻译模型选型（≤300MB）：NLLB 裁剪版核实、OPUS-MT/Bergamot/M2M100 等对比、后端搭配、推荐短名单与验证计划。
- [research/translation-zh-quality-and-candidates.md](research/translation-zh-quality-and-candidates.md) — NLLB 英→中得分低的诊断与解码调优结果、可用分数线怎么看、CONE-MT/Hy-MT2/Qwen/1.25-bit 候选结论、LLM 接入 ONNX 路线。有效，Hy-MT2 评测进行中。
- [research/windows-hevc-support.md](research/windows-hevc-support.md) — Windows H.265 支持现状。已搁置，存档。
- [research/adr5-local-nmt.md](research/adr5-local-nmt.md) — ADR-5 本地 NMT 推理后端选型。
- [research/adr8-canvas-blob.md](research/adr8-canvas-blob.md) — ADR-8 canvas 历史/会话文件调研。
- [research/speech-to-text-backends.md](research/speech-to-text-backends.md) — 实时语音转文字后端调研：sherpa-onnx 与 ort 共用 ORT、纯 ort 流式 Zipformer 实测（RTF≈0.12、~253MB）、Windows 系统语音、SendInput 键入、按住说话热键与分阶段计划。
