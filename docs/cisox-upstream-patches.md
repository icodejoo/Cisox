# Cisox 对 upstream 共享代码的补丁清单

本文件登记 Cisox 分支对 upstream（`mg-chao/snow-apps`）**原有代码**做过的修改。
以后从 `main`（upstream 镜像）合并时，请先对照本清单处理冲突，并考虑是否向 upstream 回馈。

## 1. snow-stitch-images：位移精确复核

- **文件**：`snow-crates/crates/snow-stitch-images/src/stitcher.rs`（只追加，未删改原有逻辑）
- **日期**：2026-09-30（阶段 D3）
- **问题**：位移估计按 60 分位误差比较候选，弱对比度内容（例如只有红色通道有纹理）下会偶发 ±1 行偏差，
  且置信度依然通过，导致拼缝错位。
- **修法**：新增 `refine_offset_exactly`，在估计位移 ±2 内按"逐像素完全相等"的采样命中数复核；
  只有别的候选命中数**严格多于**估计值才改用，平局保持原估计（周期性内容不会被误改）。
- **调用点**：`StitchAccumulator` 接受帧前，紧跟位移估计之后。
- **测试**：模块 `exact_refinement_tests`；库自带 79 个测试全部通过（2 个 ignored 为原有）。
- **影响面**：`snow-stitch-images-c`（经 `snow_rust_ffi`）的 C++ 调用方同样受益；对其他内容行为不变。
- **合并注意**：若 upstream 也改动 `stitcher.rs`，需人工合并；建议合并后重跑 `cargo test -p snow-stitch-images`。
- **未解决（未在库内实现）**：固定页脚超过帧高 30% 时每个拼缝残留；周期性纹理位移存在歧义。

## 待评估的上游反馈(非本仓库补丁)

- **ffmpeg `vf_scale_d3d11.c`**:创建 VideoProcessor 输入 view 时把 `DXGI_FORMAT` 枚举值填进 `FourCC`(应为 0 或 YUV FOURCC),导致 `0x887A0004`;n8.0.1 与 master 均存在,一行可修。证据见 `docs/cisox-recording-spike-report.md` 附录。是否向 ffmpeg 上游反馈由用户决定,尚未提交。
- 录制重构(阶段 E2)未修改 `snow-crates`;若后续确需修改,须先在此登记。
