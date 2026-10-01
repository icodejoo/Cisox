# 项目原则

> 本文是原则的**唯一出处**。其他文档引用时请写链接，不要改写措辞。
> **关键词：总原则 = 第一原则 = 根本原则**（三词同义，2026-10-01 用户确认），指下面的 §1 与 §2。
> 做裁决时先读这里，再逐条按优先级推导，依据必须引用原文。

## 1. 优先级链

> 高性能 > 低内存 > 高 fps > 少编译依赖 > 多用系统自带能力

出处：`recording-handover/experiment-ledger.md` 第 3 行；`research/video-editor-backends.md` 第 9 行；`research/video-editor-mvp-design.md` 第 136 行（"用户授权按项目第一原则……裁决"）；`research/system-ocr-translate-backends.md` 第 113 行。

## 2. 总原则（用户授权代为决策）

> 高性能、低内存、小体积——能复用就不新增依赖、能独立 worker 进程就不常驻、能按需加载就不预载；**优先利用操作系统已有能力**（2026-10-01 用户补充）。

出处：`cisox-gpui-migration-plan.md` §845 第 9 条（2026-09-30 授权，2026-10-01 补充最后一句）。

## 3. 执行原则

> **功能验证优先。** 基础设施类缺口按 §10 清单最简占位，功能验收后再补齐。

出处：`cisox-gpui-migration-plan.md` 第 10 行。占位必须是可运行的降级态，不能是 `todo!()`。

## 4. 红线

> **macOS / Linux 不得阻塞 Windows 的功能对齐。** 任何非 Windows 平台相关任务优先级永远低于 Windows 主线；P0-P6 全部先在 Windows 上做完，跨平台补齐是完全对齐之后单独立项。

出处：`cisox-gpui-migration-plan.md` 第 336 行。

## 5. 能力降级

> 每个功能声明所需能力，不满足时 UI 显示禁用态 + 原因文案，**绝不崩溃、绝不静默失败**。

出处：`cisox-gpui-migration-plan.md` 第 324 行（`snow-capability`）。

## 适用范围说明

`snow-shot-releases.md` 另有一条只针对**发布与更新**的优先级："data preservation and authenticity > recovery > testability > maintainability"，不属于上面的总原则，两者不要混用。
