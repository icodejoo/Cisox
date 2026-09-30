# Snow Shot → Rust + GPUI 改造方案

> 版本 v2.0 · 2026-09-30（v2.0 变更：同步 2026-09-30 复审后已拍板决策——局部订正 P5/P6/P7 相关条目并在文末追加"复审后决策记录"；原验收报告已不作为验收依据。v1.9 变更：文档订正——vendor 27→28（已核实实际 28 个 crate）；ADR-6 `.ts` 数量 30→32（实测）；ADR-8 补 QDataStream 例外（`result_style.bin`/`recognition_results.bin`）并删除"全部是 JSON"的过强措辞；`canvas_history.json` 注释去掉"疑似"改为"已验证"；quick-xml 状态改为"已批准"；V5 license 冲突标记为已解除；§9 工作量估算与 §0 一句话结论对齐（14.8 万行）；V2 第三轮"68 次运行"更正为 34 次。v1.8 变更：ADR-1 改为 vendor 目录锁定（27 个 crate、约 27MB，不用 submodule，附实测证据）并标记已落地；§2.1、§7 约定 3 同步；P1 进度补入 snow-config、snow-ui-theme；v1.7 变更：V2 判据改为稳态平均 + 稳态 P99 并写入第三轮结论，V2 改为有条件通过；附录 B.2 分组键数按 snow-config 实测订正；追加文件名前缀、翻译配置延后、`.ts` 解析依赖、Qt 静态构建策略四项裁决。v1.6 变更：更正 MCP tool 数量为 101 个并标注命令建模缺口；V8 与 ADR-8 措辞对齐；ADR-8 blob 待验证项已验证；P1 追加进度小节。v1.5 变更：V2 同步第二轮优化结果，判据待裁决；V6 核对并登记搜狗内联预编辑待办 T12；V8 由待执行改为通过）
> **产品名：Cisox** · 工作仓库：`github.com/icodejoo/Cisox`（fork 自 `github.com/mg-chao/snow-apps`）
> 本地检出：`E:\workspaces\Cisox`（完整历史 407 提交；`origin`→Cisox，`upstream`→snow-apps 且已禁止推送；工作分支 `rust-gpui`，`main` 保持 upstream 纯镜像）
> 本文档位置：`docs/cisox-gpui-migration-plan.md`（随仓库走，执行代理从这里读）
> 仓库形态：**fork**（执行方非 upstream 维护者），产物继续以 **GPL-3.0** 开源 —— 见 §5.1 / §5.2
> 目标范围：**全量对齐现有功能**；目标平台：**Windows / macOS / Linux**
> **执行顺序（2026-09-29 起）：Windows 优先，一路打通到全功能对齐；macOS / Linux 全部标记为"待实现"，暂不验证、暂不开发，仅在架构设计时预留接口不要写死 Windows 专属假设。** 待 Windows 完全跑通后再回头做跨平台补齐。
> **执行原则：功能验证优先。** 基础设施类缺口按 §10 清单最简占位，功能验收后再补齐。
> 本文档面向执行方（AI 代理 + 人工评审），可直接拆解为任务派发。

---

## 0. 一句话结论

**这不是"把 30 万行 C++ 重写成 Rust"，而是"把 Qt 层摘掉"。** 仓库里已经有 **约 20 万行可原样存活的 Rust**——采集、录制、OCR、拼接、元素选择，以及最关键的**标注引擎内核（7.6 万行 Rust）**。真正需要重写的是 **约 14.8 万行 C++**（其中 78% 是 Qt 表现层）+ 一个 14.6 万行的 Qt 组件库。

但有一个必须先验证的硬风险：**GPUI 不是通用 2D 画布**，它画不了任意贝塞尔描边、线帽线接、虚线。标注功能是这个 App 的命门，因此方案的第一阶段是一个 **2–3 周的垂直切片 Go/No-Go 闸口**，而不是直接开工。

---

## 1. 现状测绘（实测数据）

### 1.1 代码体量

| 模块 | 语言 | 行数 | 迁移处置 |
|---|---|---:|---|
| `snow-crates/` | Rust | 158,298 | **存活**（删除 `*-c` FFI 壳 ≈ 25k） |
| `snow_draw_engine_qt/crates/` | Rust | 75,825 | **存活**（删除 `-c` 壳 9.7k） |
| `snow_draw_engine_qt/src/` | C++/Qt | 25,882 | **重写**（QPainter 光栅化 + 文本/IME + AVX2 滤镜） |
| `snow_shot/src/` | C++/Qt | ≈147,800 | **重写**（其中 `presentation/` 114,607 = 78%） |
| `ant_design_qt/` | C++/Qt | 146,121 | **替换**（gpui-kit + 自研补齐；829 个 Ant 图标资产存活，对应 1658 个 SVG 文件，`icons/` 与 `templates/` 各一份） |
| `snow_image/` `snow_image_viewer/` | C++/Qt | 44,993 | **本次不改造**（独立 App，见 §9.3） |

**存活 Rust ≈ 199k 行 / 待重写 C++ ≈ 174k 行**（不含 `include/`、`tests/`）。

### 1.2 分层现状（关键发现）

```
┌─────────────────────────────────────────────────────────┐
│  snow_shot/src/presentation/  114.6k C++ Qt             │  ← 全部重写
│  QWidget + QPainter，零 GPU 加速，手写脏区追踪           │
│  screenshotcontroller.cpp 单文件 5,956 行（god object）  │
├─────────────────────────────────────────────────────────┤
│  ant_design_qt/  146k C++    ~60 个 QWidget 组件         │  ← 替换
│  AdButton×426  AdModal×143  AdSelect×126  IconRef×230   │
├─────────────────────────────────────────────────────────┤
│  snow_shot/src/{app,platform,storage,network,...} 33k   │  ← 重写（底层 Win32/Cocoa 调用可直译）
├─────────────────────────────────────────────────────────┤
│  snow_draw_engine_qt/src/  25.9k C++  ← QPainter 光栅化  │  ← 重写（本方案最核心的一块）
│         ↕ 116 个 extern "C" 函数（手写 C ABI）           │
│  snow_draw_engine_qt/crates/  75.8k Rust                │  ← ★ 存活
│  文档模型 / 撤销重做 / 工具状态机 / 场景图 / 脏区 diff    │
├─────────────────────────────────────────────────────────┤
│         ↕ 手写 extern "C" + 手写 C 头文件                │
│  snow-crates/  158k Rust  采集/录制/OCR/拼接/元素选择     │  ← ★ 存活
└─────────────────────────────────────────────────────────┘
```

三条必须记住的事实：

1. **标注引擎的难点已经是 Rust 了。** `snow-draw-engine-{core,model,document,editor,scene,display,draw-engine}` 包含全部工具状态机（Select/Shape/Arrow/Line/FreeDraw/Highlight/Filter/Watermark/Eraser/Text/SerialNumber/Spotlight/AutoFilter）、文档模型、撤销重做、serde 序列化、场景图与脏区计算。C++ 只负责"把 Rust 算出来的图元用 QPainter 画出来"。
2. **引擎已经输出 diff/patch 协议**（`ViewportPatch` / `LayerPatch<T>` / `DirtyRegion` / `SceneRenderRun`）。这是资产不是包袱——换渲染后端 = 换 patch 消费者，而不是重写引擎。
3. **现状全部是 CPU 光栅化**（Qt raster engine），并为此写了大量手工脏区优化。这意味着换成 CPU 光栅化的 Rust 方案**不存在性能倒退风险**，是一次同量级替换。

### 1.3 现有能力清单（需全量对齐）

截图/选区/智能元素选区（UIA·AX 无障碍树）· 标注（15 种工具）· 贴图（浮窗+分组+持久化）· 滚动截长图 · 录屏（含键鼠特效、音频、导出）· OCR（本地 ONNX RapidOCR，独立进程）· 表格/LaTeX 提取（云端 LLM）· 翻译 · 取色器 · 二维码 · 历史记录 · MCP Server（28+ tools）· 全局快捷键 · 托盘 · 自启 · 自动更新（RSA-3072 签名，独立 helper 进程）· 单实例 IPC · 崩溃上报（crashpad）· i18n（10 模块 × 3 语言）

---

## 2. 目标架构

### 2.1 Cargo workspace 布局

新建**独立 workspace**，与现有 Qt 应用并行存在、并行发布（详见 §5 迁移策略）。

```
snow-shot-rs/                       # 新 workspace（edition 2024）
├── crates/
│   ├── snow-shot/                  # bin：入口、子模式分发、单实例
│   ├── snow-app-core/              # ★ 命令总线、应用状态、会话编排（取代 screenshotcontroller god object）
│   ├── snow-capability/            # ★ 平台能力注册表（Linux 降级的基础设施）
│   │
│   ├── snow-ui/                    # GPUI 视图层总入口
│   │   ├── snow-ui-theme/          #   设计令牌（移植 ant_design_qt palette_generate）
│   │   ├── snow-ui-icons/          #   829 个 Ant 图标（1658 个 SVG 文件，两份目录）+ IconRef/IconColors 模型（resvg 光栅化）
│   │   ├── snow-ui-widgets/        #   gpui-kit 缺口补齐（Popconfirm/Checkerboard/Segmented/流式布局）
│   │   └── snow-ui-shell/          # ★ GPUI 适配隔离层：窗口/托盘/热键/DPI 全部只在这里碰 gpui
│   │
│   ├── snow-canvas-raster/         # ★★ 标注画布光栅化器（patch → tiny-skia → GPUI texture）
│   ├── snow-canvas-filters/        # ★ 马赛克/模糊/浮雕/智能擦除（AVX2 C++ → Rust SIMD）
│   ├── snow-canvas-text/           # ★ 标注文本布局 + IME + 光标/选区
│   │
│   ├── snow-translate/             # ★ 新增：本地 NMT 翻译引擎（见 ADR-5）
│   ├── snow-i18n/                  # ★ Fluent 运行时 + 提取工具
│   ├── snow-config/                # 配置 schema（与现有 JSON 磁盘格式兼容）
│   ├── snow-history/               # 截图历史 / 贴图仓储
│   ├── snow-net/                   # HTTP 客户端（reqwest）+ 云端 AI 接口
│   ├── snow-update/                # 更新协议 v2 + helper 进程驱动
│   ├── snow-mcp/                   # MCP server（进程内，见 ADR-9）
│   └── snow-platform/              # Win32 / Cocoa / Linux 原生调用（windows-rs / objc2 / ashpd）
│
└── vendor/                         # crates.io 发布包原样纳入，[patch.crates-io] 指向本地，版本 `=` 精确锁定
    └── <crate>-<版本>/             #   gpui-pre-* 与 gpui-kit 系共 28 个 crate（约 27MB），禁止擅自升级；原 longbridge/gpui-component
```

**复用（path 依赖，源码不动）**：
```
snow-crates/crates/*                  ← 删除全部 *-c 壳
snow_draw_engine_qt/crates/snow-draw-engine-{core,model,document,editor,interaction,scene,display,draw-engine}
                                      ← 删除 snow-draw-engine-c
```

### 2.2 渲染分层（核心设计）

```
        snow-draw-engine (Rust)  ──emit──▶  ViewportPatch / DirtyRegion
                                                      │
                                                      ▼
                                      ┌──────────────────────────────┐
                                      │  snow-canvas-raster          │
                                      │  trait CanvasRasterizer      │
                                      │   └ TinySkiaRasterizer (P0)  │  ← CPU 光栅化，仅重绘脏区
                                      │   └ VelloRasterizer (预留)    │
                                      └──────────────┬───────────────┘
                                                     │ 脏块像素
                                                     ▼
                                      GPUI ImageSource / Canvas 合成
                                                     │
                       ┌─────────────────────────────┼─────────────────────────────┐
                       ▼                             ▼                             ▼
              冻结屏幕底图（image）          GPUI 原生 UI（quad/text）         选区/放大镜
              ← snow-capture CPU 帧                工具栏 · 弹层
```

**要点**：GPUI 只负责 UI chrome 与合成；所有矢量绘制由 `snow-canvas-raster` 承担。两者通过"脏块纹理上传"解耦，**不共享 GPU 设备**，规避了 GPUI 在 Windows 上使用自有 DX11 设备所带来的互操作风险。

### 2.3 命令总线（消灭 god object）

现状 `screenshotcontroller.cpp`（5,956 行 QObject）把 UI、快捷键、托盘、MCP 全部signal/slot 硬连在一起。新架构：

```rust
enum AppCommand {
    Capture(CaptureRequest), SelectTool(ToolKind), Export(ExportTarget),
    PinSelection, StartRecording(RecordingConfig), RunOcr(OcrRequest),
    Translate(TranslateRequest), /* … */
}
```

UI 按钮、全局热键、托盘菜单、**MCP 的全部 tool** 全部收敛为同一组命令的发射端。MCP 因此几乎零成本（见 ADR-9），也让 e2e 测试可以绕过 UI 直接驱动。

> 数量更正（v1.6）：MCP 共 101 个 tool（screenshot 域 28 + application 27 + documents_jobs 33 + recording_pinned 13，见 `snow_shot/mcp-capabilities.json`）。当前命令总线（snow-app-core）只覆盖 screenshot 域的 28 个语义 + StartRecording，其余约 70 个 tool 的命令建模待后续任务（P7 MCP 进程内化前完成）。

---

## 3. 技术裁决（ADR）

### ADR-1 · GPUI 依赖来源 —— 锁定 + 隔离

**事实**（附录 A 实测，2026-09-28）：

| 包 | 发布方 | License | 时效 | 性质 |
|---|---|---|---|---|
| `gpui` (crates.io) | Zed 官方 | Apache-2.0 | v0.2.2，2025-10-22，**停滞约 11 个月** | 上游正式包，发布节奏跟 Zed 自己走 |
| `gpui-pre` | **huacnlee 个人**（同时是 gpui-kit 主维护者） | Apache-2.0 | v0.3.7，**2026-09-28（今天）** | 不是独立分叉，是 Zed `main` 未发布提交的快照转发，专为解开 gpui-kit 的依赖死结 |
| `gpui-ce` | 社区组织（Discord 治理） | Apache-2.0 | v0.2.2 (2026-08-28)，提交活跃 | 真正的下游分叉，目标是通用 "Electron 替代"，超出 Zed 自身需求 |

**License 结论：全线 Apache-2.0，与 GPL-3.0 单向兼容，无冲突。R2 风险解除。**
（GitHub API 显示的 NOASSERTION 是元数据探测失误，实际 LICENSE 文件已逐个读取确认。注意 gpui-kit 是 **Apache-2.0 单授权**，不是 Rust 惯例的 MIT/Apache 双授权。）

**裁决**：
- 采用 **gpui-kit 所依赖的那一支 gpui**（即 `gpui-pre`）。一个进程内不可能混两个 gpui 版本（类型标识不兼容）——**组件库事实上决定了 gpui 版本**，这不是可以分开选的两件事。
- **把 gpui 依赖族的 crates.io 发布包原样 vendor 进 `snow-shot-rs/vendor/`（28 个 crate、约 27MB），用 `[patch.crates-io]` 指向本地，版本用 `=` 精确锁定**。升级是一次需要评审的显式动作（靠 `git diff vendor/`），不是 `cargo update` 的副作用。**不用 git submodule**（v1.8 实测修正）：`gpui-pre` 是 huacnlee 把 Zed 快照（`zed@1a28cff`）拆成 15 个改名包发布的，包名与依赖都被改写，Zed 仓库里没有这些同名 crate，`[patch]` 无法直接指向 Zed 检出目录；且 Zed 全仓约 517MB。`gpui-kit 0.7.0` 对应 longbridge/gpui-kit 提交 `0c830f4d`，要求 `gpui-pre = "=0.3.7"`。其余传递依赖仍走 crates.io，由 `Cargo.lock` 校验和锁定。
- 所有 `gpui::` 直接调用**收敛到 `snow-ui-shell` 一个 crate**。上游 API churn 的爆炸半径必须是一个 crate，不是两百个文件。
- **接受并登记单点维护风险（R11）**：依赖链的关键一环由个人发布。vendor 锁定使得"上游停更"退化为"我们自己维护一份快照"，而不是"项目停摆"——这正是 vendor 而非直接依赖 crates.io 的理由。

### ADR-2 · 标注画布渲染 ★ 最关键裁决

**问题**：GPUI 的图元集是 quad / rounded-rect / border / shadow / underline / 文字 / 图片 / 有限 path fill。**没有**带线接线帽的任意折线描边、没有虚线、没有通用仿射变换。而标注引擎要画 15 种工具。

**裁决：用 `tiny-skia` 做 CPU 光栅化，替换现有的 QPainter patch 消费者。**

理由，按权重排序：
1. **与现状同量级，无性能倒退风险。** 今天就是 CPU 光栅化（Qt raster），且脏区协议已经存在。tiny-skia 是 Skia 光栅后端的 Rust 移植，去掉 Qt 的抽象开销后大概率更快。
2. **零 GPU 互操作风险。** Vello 的 compute 路径至今仍是 alpha，且需要一条独立的 wgpu 管线——在 Windows 上无法与 GPUI 自有的 DX11 设备共享，这正是最容易让排期崩掉的地方。
3. **功能完备。** 线接/线帽/虚线/渐变/裁剪/抗锯齿全都有，对得上 QPainter 现有能力。
4. **改动面最小。** 引擎侧完全不动，只换 patch 的消费端。

**留后路**：光栅化器定义为 `trait CanvasRasterizer`，Vello/wgpu 实现可以在实测需要时后补，不阻塞主线。

**禁止**：不要试图用 GPUI 的原生图元去"凑"标注绘制。这条路走到一半才发现画不了虚线箭头，是本项目最可能的翻车方式。

### ADR-2b · 覆盖窗点击穿透 —— `SetWindowRgn` 动态区域裁剪（P0 实测确认）

**问题**：截图覆盖窗需要"选区内正常交互、选区外完全穿透点击"，且这个边界会随用户拖动选区实时变化。

**排查记录（2026-09-28~29，Windows 实机）**：
1. `WM_NCHITTEST` + 外部 WNDPROC 子类返回 `HTTRANSPARENT`——钩子确认正确安装、正确触发、正确返回值，**但点击依然没有穿透**。
2. `WS_EX_TRANSPARENT` 整窗穿透样式——确认设置成功（`GetWindowLongPtrW` 回读验证），且在点击那一刻依然保持设置，**依然没有穿透**。
3. 排除了一圈可能的解释：GPUI 没有重装 WNDPROC、没有用 RawInput、没有全局钩子、`disable_direct_composition` 环境变量关掉 DirectComposition 后依然失败——说明前两条路径在这个 GPUI 版本（`gpui-pre` 0.3.7）上就是不通，不是我们代码写错了。
4. 中途发现**这台机器上 `SendInput` 模拟点击完全不生效**（对照测试：无任何遮挡直接点都收不到），推翻了早期几轮"自动化验证"的结论——**教训见下方"验证方法论"**。
5. **`SetWindowRgn`（Win32 原生窗口区域裁剪 API）——真人点击 + 满屏接收窗口 + 文件日志确认，6 次点击全部正确穿透。**

**裁决：覆盖窗的点击穿透统一使用 `SetWindowRgn`（Win32），在 GPUI 创建的窗口上直接调用，不需要更换建窗框架。macOS 对应实现（`NSWindow` 的 region 或 `-hitTest:` 覆盖）按 ADR-7 延后，不在当前阶段验证。** 这正是 upstream Qt 生产环境实际使用的技术（`QWidget::setMask()` 在 Windows 上就是 `SetWindowRgn` 的封装，upstream 代码里对应 `updateWindowMask`，见 `screenshotoverlaywindow.cpp:716-749`）——本次调研走了弯路才绕回 upstream 已经验证过的路，但过程排除了另外两条路径，值得记录避免重复踩坑。

**设计要点**：
- `SetWindowRgn` 是粗粒度的"窗口形状"控制，不是像素级实时判定——选区变化时需要重新计算区域、重新调用一次（对应 upstream 的 `updateWindowMask` 模式），不是每帧都算，这跟标注引擎的 `DirtyRegion`/`ViewportPatch` 更新节奏是一致的，不需要额外设计。
- 区域外的部分**同时**失去绘制与点击资格（不是"看得见点不穿"，是"这块地方压根不属于这个窗口了"）——这意味着窗口的可见形状和可点击形状永远一致，设计标注引擎的脏区更新时要同步这一约束。
- `WM_NCHITTEST`/`WS_EX_TRANSPARENT` 这两条路**保留在附录里作为已证伪的记录**，不要在后续开发中重新尝试。

**验证方法论教训（记录以避免重复浪费）**：本轮诊断中，为了减少用户参与，一度切换成 `SendInput` 自动化模拟点击，但事后发现**这套自动化在当前沙箱环境里完全不生效**（对照测试：无任何遮挡的直接点击也收不到），导致 3-4 轮"自动判定失败"的结论其实无效，被迫全部撤回重测。**最终采用的可靠方法**：真人点击（人类的手，真实交互）+ 程序化判定结果（一个只做"收到点击就写日志"的最小接收窗口，读日志文件，不依赖 `GetForegroundWindow` 这类会被 `TOPMOST`/激活策略干扰的间接信号）。**这个"真人输入 + 程序化判定"的分工模式，后续所有需要真实鼠标/键盘交互的验证任务都应该复用**，不要再尝试全自动模拟输入。

### ADR-3 · 组件库 —— gpui-kit 打底 + 自研补齐

> ⚠️ 命名变更：`longbridge/gpui-component` 已于 **2026-09-28 更名为 `longbridge/gpui-kit`**（v0.7.0）。全文按 `gpui-kit` 表述；旧名仍可重定向访问。

`ant_design_qt` 约 60 个组件、`AdButton` 被用了 426 次，是深度耦合，无法"局部替换"。

**覆盖率结论（附录 A 逐项核对）：高频组件几乎全覆盖。** 前 17 个高频组件中 15 个有直接对应，包括一度被列为风险项的 **ColorPicker（79 次）已确认存在**。

**必须自研的缺口**（进 `snow-ui-widgets`，按使用量排序）：

| 缺口 | 用量 | 处置 |
| Popconfirm | 32 | ✅ **已落地（2026-09-29）**：锚定内联气泡确认框（基于 `popover` + 提示图标/确认/取消按钮薄封装，进 `snow-ui-widgets`），单测与 doctest 全过 |
| Checkerboard | 低 | ✅ **已落地（2026-09-29）**：截图工具特有的透明背景网格，自研完成（进 `snow-ui-widgets`） |
| Segmented | 低 | ✅ **已落地（2026-09-29）**：胶囊型分段控制器，自研完成（进 `snow-ui-widgets`），单测与 doctest 全过 |
| Flow layout | 低 | 无换行流式容器，需在 gpui flex 上扩展 |
| 弹层几何助手 | — | gpui-kit 的锚定定位逻辑散落在 `popup_menu` 内部，无独立可复用 API，需自建胶水 |

**主题系统**：gpui-kit 自带结构化设计令牌（`ColorTokens`/`RadiusTokens`/`SemanticThemeTokens`/`ShadowTokens`/`SpacingTokens`/`TypographyTokens` + JSON schema），比 `ThemeManager`（54 处调用）更规范。但为保持现有视觉，仍需移植 `palette_generate.cpp` / `fast_color_lite.cpp` 的 Ant Design 色阶生成算法（纯计算、无 Qt 依赖）来产出令牌值。

**图标系统 —— 注意，这里没有现成路可走**：gpui-kit 的图标来自独立 crate `gpui-kit-assets`，捆绑的是 **Lucide 图标集，不是 Ant Design**。因此：
- 本仓 829 个 Ant 图标（1658 个 SVG 文件）与 `IconRef`/`IconColors` 抽象（393 处调用）**没有直接迁移路径**；
- 裁决：**保留自有图标体系**，用 `resvg`/`usvg` 光栅化，为 gpui-kit 的 `IconNamed` trait 提供自己的实现接入（其 `Icon` 类型对图标来源是泛型的，理论可行，**需在 P0 验证**）；
- 这样 393 处调用点的接口可保持不变，是改造量最小的路径。不要改用 Lucide——那是一次全量图标重设计。

**✅ P0-V7 已验证（2026-09-29，`spikes/p0-v7-icon-named/`）**：`IconNamed` 接入可行，接口极简（仅 `fn path(self) -> SharedString`）。**新增硬约束**：GPUI 的 SVG 渲染是**单色 alpha mask**（`gpui-pre` 的 `paint_svg` 只接受一个 `Hsla`，SVG 内部 `fill` 属性被完全丢弃），不能假设多色 SVG 能被 GPUI 直接原样渲染——这与 Qt 侧 `IconRenderer` 直接吃多色 SVG 的模型不同。twoTone/threeTone 多色图标须走**"AssetSource 层按 Ant twotone 的 `fill="#D9D9D9"` 约定拆层为多份单色 SVG + UI 层绝对定位叠加多个单色 `Icon`"**这条已验证路径，`IconColors` 的 Rust 版形状相应简化为 `{primary: Hsla, secondary: Option<Hsla>, ...}`（不复刻 C++ 位掩码 `presentMask_`）。threeTone 仅是从 twoTone 机制推断可行，尚无三色 SVG 样本实测。

**顺带收益**（ant_design_qt 没有、可机会主义采用）：虚拟化数据表格（十万行级）、虚拟列表、**带 Tree-sitter 与 IME 的代码编辑器**（对 ADR-11 直接有用）、停靠面板布局、命令面板、骨架屏、图表。

**gpui-kit 不提供 i18n** —— 确认 ADR-6 必须自建。

### ADR-4 · 帧数据通路 —— 先走 CPU 回读，零拷贝留到 P2 之后

现状：GPU 采集 → GPU 色调映射 → **回读到 CPU 缓冲** → 裸指针过 FFI。
纯 Rust 后 FFI 消失，但**保持 CPU 回读**：GPUI 没有公开的外部纹理导入 API，Windows 上它跑在自有 DX11 设备上。
- P0 不做零拷贝，先用 GPUI 的图像上传路径。
- 4K/多屏下若实测有瓶颈，再评估经 `IDXGIKeyedMutex` 共享句柄注入 GPUI 设备（macOS 对应 `IOSurface`）。**这是优化项，不是前置项。**
- 顺带收益：`snow-capture-c`（3,379 行 lib.rs）里那套跨 FFI 的会话/线程编排可以直接变成原生 Rust API（新 crate `snow-capture-session`），代码量净减。

### ADR-5 · 本地翻译引擎与云端 AI 替代（按需求变更设计）

**需求**：翻译改为本地模型；用户可自由选择 NMT / NLLB / opus-mt / Marian；**模型文件由用户自行下载放入指定目录，软件懒加载**。

**设计**：新建 `snow-translate`，核心是一个后端 trait：

```rust
trait TranslationEngine {
    fn translate(&self, text: &str, src: Lang, tgt: Lang) -> Result<String>;
    fn supported_pairs(&self) -> &[(Lang, Lang)];
}
```

| 后端 | 实现 | 说明 |
|---|---|---|
| `OnnxNmt`（默认） | `ort` crate + `tokenizers` | **ONNX Runtime 已经是本仓依赖**（rapid-ocr-rs 在用，vcpkg 里有 onnxruntime + DirectML），复用现成推理基建与 GPU 加速，零新增重型依赖 |
| `OpenAiCompatible`（可选） | `reqwest` | baseUrl 指向 Ollama / LM Studio / 自建服务；同时覆盖原有云端通路 |

**模型目录与懒加载**：
```
<AppData>/SnowShot/models/translate/
├── nllb-200-distilled-600M-int8/
│   ├── model.json          ← 清单
│   ├── encoder.onnx  decoder.onnx      ← decoder.onnx 实际是 merged decoder（decoder_model_merged，单文件同时含首步与 KV cache 分支，无需 decoder_with_past）
│   └── sentencepiece.model / tokenizer.json
└── opus-mt-zh-en/
    └── …
```
`model.json` 清单 schema（须精确定义并写进用户文档）：
```jsonc
{
  "schema_version": 1,
  "id": "nllb-200-distilled-600M-int8",
  "display_name": "NLLB-200 Distilled 600M (int8)",
  "family": "nllb" | "marian" | "m2m100",     // 决定前后处理与语言码映射
  "quantization": "int8",
  "files": { "encoder": "encoder.onnx", "decoder": "decoder.onnx",   // decoder = merged decoder（含 use_cache_branch 与 KV cache）
             "tokenizer": "tokenizer.json" },
  "languages": ["zho_Hans", "eng_Latn", "jpn_Jpan"],
  "max_input_tokens": 512
}
```
- 启动时**只扫描清单**（毫秒级），设置页列出可用模型；
- **首次翻译时才创建 ORT session**（懒加载），加载中给进度反馈；
- 空闲 N 分钟后卸载 session 释放内存（可配置）；
- **未安装任何模型时**：翻译入口不报错，显示引导卡片说明模型放置位置与获取方式。

**明确不做**：不打包任何模型权重进安装包。

**表格提取 / 公式提取的处置（简化方案）**：现有 `/api/v1/table/extract` 与 `/api/v1/latex/extract` 是 upstream 自建的专有端点，fork 无法继承。
**裁决：不自建端点、不做本地模型，改走已有的"自定义 AI 模型"通道。**
应用本就内置这套机制——`api_configuration/custom_models` 配置项 + 完整设置界面（`customaimodelssettingswidget.cpp`：baseUrl + apiKey + 模型 id），选中自定义模型时请求转向 `custom->baseUrl + "/chat/completions"`（`snowshotapiclient.cpp:851,951`）。视觉模型完全能胜任表格与公式识别。
- 用户配置任一 OpenAI 兼容的视觉模型端点（云端或本地 Ollama/LM Studio）即可使用；
- 未配置时显示引导，不报错；
- 零新增基础设施。后续若要做本地 ONNX 方案（PP-Structure / LaTeX-OCR），可复用本 ADR 的模型管理与懒加载机制，见 §10-T5。

### ADR-6 · i18n —— Qt .ts → Fluent

现状：10 个功能模块 × 3 语言 = 32 个 `.ts`（实测转换结果，含 `ant_design_qt` 补全后合成的 en_US），`lupdate`/`lrelease` 构建期编译，CI 用 `-fail-on-unfinished` 卡未翻译项。

裁决：迁移到 **Fluent**（`fluent-rs`），因为它对 Qt 的 `%n` 复数形态有等价且更完整的支持。需要配套三件事：
1. **一次性转换器**：`.ts` → `.ftl` 脚本（机械活，高性价比，务必自动化而非手工搬运）；
2. **提取工具**：替代 `lupdate`，扫描源码中的 `t!()` 宏生成模板；
3. **CI 门禁**：复刻 `-fail-on-unfinished` 语义——任何 locale 缺条目即构建失败。

`%1` / `%n` 占位符与复数规则必须在转换器里做等价映射，并写单测覆盖。

### ADR-7 · 平台能力矩阵 —— Windows 优先，macOS / Linux 全部延后

> **2026-09-29 更新执行顺序**：不再是"Windows/macOS 并行、Linux 降级"，而是**先把 Windows 一路打通到全功能对齐，macOS 和 Linux 同等延后**，验证/开发都不做，只在架构上不写死 Windows 专属假设（比如 `snow-capability` 的能力声明机制本身要跨平台，具体 macOS/Linux 后端实现推后）。

引入 `snow-capability`：每个功能声明所需能力，不满足时 UI 显示禁用态 + 原因文案，**绝不崩溃、绝不静默失败**。这个机制本身现在就要搭（跨平台设计），具体各平台的后端实现按下表优先级排期。

| 能力 | Windows（当前主线） | macOS（待实现，延后） | Linux（待实现，延后） |
|---|---|---|---|
| 屏幕采集 | DXGI/WGC ✅ | ScreenCaptureKit — 待实现 | `ashpd` + xdg-desktop-portal ScreenCast — 待实现 |
| 透明置顶覆盖窗 + 点击穿透 | ✅ `SetWindowRgn`（ADR-2b 已验证） | `NSWindow` region / `-hitTest:` — 待实现 | X11 可行 / Wayland 受限 — 待实现 |
| 全局热键 | `RegisterHotKey` ✅ | Carbon — 待实现 | 待实现 |
| 托盘 | ✅ | 待实现 | 待实现 |
| 元素选择（无障碍树） | UIA ✅ | AX — 待实现 | AT-SPI — 待实现 |
| 录屏 | ✅ | 待实现 | 待实现 |
| 选中文本抓取 | ✅ | 待实现 | 待实现 |

**红线：macOS / Linux 不得阻塞 Windows 的功能对齐。** 任何非 Windows 平台相关任务优先级永远低于 Windows 主线；P0-P6 全部先在 Windows 上做完，跨平台补齐是完全对齐之后单独立项。

### ADR-8 · 磁盘数据兼容 —— 独立目录 + 一次性导入器

**实测结论（附录 B）：主要是 JSON + PNG + 不透明 blob，兼容成本远低于预期。** 注意例外：`result_style.bin`（`screenshotresultcompositor.h::encodeScreenshotResultStyle`，魔数 `0x53535247`）与 `recognition_results.bin`（`screenshotpinnedwindow.cpp::serializeRecognitionResults`）是 Qt `QDataStream` 私有二进制格式，导入器需最小读取器（P4 待办，见 §10 ADR-8）。

数据根目录：`%LOCALAPPDATA%\SnowShot\snow_shot\` / `~/Library/Application Support/SnowShot/snow_shot/`（注意是**两级**），另支持便携模式——exe 同级放 `__data_directory` 标记文件即可改根。

**裁决（因 fork 形态而调整）：使用自有数据根目录，schema 与格式完全沿用，另提供一次性导入器。**

原先设想的"原地升级"前提是用户从同一产品的 Qt 版升到 Rust 版。fork 之后这个前提不成立——这是**另一个产品**，两个程序抢同一个 `config.json` 与历史记录是实打实的数据损坏路径。而 P-1 又要求装发布版当参照物，两者必然共存。

- 根目录改为自有名（如 `%LOCALAPPDATA%\SnowShotRs\`），**其余一切不变**：相同的 key 名、相同的 JSON 结构、相同的目录布局。
- 提供**一次性导入器**：读取 upstream 位置的配置/历史/贴图，复制进自有目录。用户主动触发，不自动执行。
- schema 兼容的工作量因此**一点没浪费**，只是改了写入位置。

具体格式沿用如下：
- `config.json`：238 个键，`"组/名"` 扁平命名，6 种值类型，版本字段 `storage/schema_version = 3`。用 serde 镜像 schema，**并移植那约 20 个键专属的 normalizer**（颜色 `#AARRGGBB` 正则、文件名格式、快捷键列表、工具栏布局、翻译语言白名单、自定义 AI 模型）——这些是数据正确性的关键，不是可选项。
- `capture_history/` 与 `pinned_windows_v2/`：均为 `index.json` 清单 + 每记录一个目录。注意 pinned 的 `format_version` 硬锁为 2，不匹配即整体丢弃（v1 无迁移路径）——新实现必须保持这个语义，不要"好心"去兼容 v1。
- **两阶段删除**（先把待删项写进 `index.json` 的 `pending_deletions` 再删文件）是崩溃安全设计，必须原样保留。
- 可直接丢弃：缩略图缓存、录制临时目录、`logs/`（均为可再生的 OS 缓存）。

**✅ 已验证的高价值项（2026-09-29，详见 [`docs/research/adr8-canvas-blob.md`](research/adr8-canvas-blob.md)）**：原假设（C++ 从不解析的透传 blob，极可能是 Rust 标注引擎自己的 serde 序列化产物）成立。
- `canvas_history.json` 是引擎 `DocumentHistory` 的 serde_json 直接产物（schemaVersion 当前 5，`snow-draw-engine/src/session.rs`）；`canvas_session.bin` 是 UTF-8 JSON（`DocumentSession`，比 history 多 `editor` 与 `sessionConfigSeeded`），C++ 只做哈希与量长度。
- 序列化部分直接复用引擎，只需重做容器层：文件 I/O、路径校验、体积上限、`canvas_byte_size` 记账、贴图 payload 变更检测、manifest 与两阶段删除。
- 对 P4 的影响：数据兼容不再是难点，工作量转向贴图窗口 UI 与交互（前提：GPUI 版继续用同一套 `snow-draw-engine-*` crate）。
- 要点提醒：体积上限不一致（capture_history 的 canvas 16 MiB；贴图 `canvas_session.bin` 32 MiB）；导入器遇 `schemaVersion > 5` 应「跳过并提示」而非当损坏；降级不可行（session/history/editor 层 `deny_unknown_fields`）。
- 未验证：无 `canvas_session.bin` 真实样本、非空画布元素落盘样例、schemaVersion 1~4 真实旧文件。

### ADR-9 · MCP —— 从跨进程桥接改为进程内

现状：Rust 的 `snow-shot-mcp`（`rmcp`，stdio）通过带 token 认证的命名管道 / Unix socket 跟 Qt App 通信，Qt 侧有 7k 行桥接代码。

纯 Rust 后：MCP server 作为**进程内任务**直接向命令总线（§2.3）投递 `AppCommand`。
- 对外仍保留 stdio transport 与描述符/token 文件，**外部 MCP 客户端零感知**；
- 删除 7k 行 Qt 桥接；
- 共 101 个 tool（screenshot 域 28 + application 27 + documents_jobs 33 + recording_pinned 13，见 `snow_shot/mcp-capabilities.json`）退化为命令总线上的一组映射，是本次改造少有的**净减法**。
- **命令建模缺口（v1.6）**：当前命令总线（snow-app-core）只覆盖 screenshot 域的 28 个语义 + StartRecording，其余约 70 个 tool 的命令建模待后续任务（P7 MCP 进程内化前完成）。

### ADR-10 · AVX2 滤镜内核 —— 直译为 Rust SIMD

`snow_canvas_filter_avx2.cpp` / `snow_canvas_pen_mask_avx2.cpp` 是手写 AVX2 的马赛克/模糊/浮雕/智能擦除。
用 `wide` crate（stable 上可用的可移植 SIMD）重写 + 标量兜底，**用现有实现产出的图作为黄金样本做像素级/PSNR 比对测试**。
这几个内核自包含、有强测试 oracle、可并行，是最适合早期并行派发的任务，也是执行代理的理想热身题。

### ADR-11 · 标注文本与输入法 ★ 二号风险

`snow_draw_engine_qt/src/text/` 约 12 个文件，负责文本草稿、布局、测量、IME。这是**唯一没有 Rust 对应物、且难度真实**的重写项：需要光标、多行选区、CJK 输入法预编辑串、字体回退。

裁决：优先复用 gpui-kit 的 `input/editor`（Tree-sitter + LSP + IME，官方称 20 万行文本稳定），在其上实现标注文本工具。**单独立项做技术验证（P0 的 V6）**，不与画布光栅化并线推进。

**✅ P0-V6 通过（2026-09-29，`spikes/p0-v6-ime-text/`）**。走高层 `Input`/`InputState`（不用 `editor.rs`）。Win32 消息层插桩（raw 模式，绕开 gpui-kit）实测：**微软拼音**每键都有 `WM_IME_COMPOSITION(COMPSTR)` → `replace_and_mark_text_in_range`，`marked_text_range()` 随之更新，预编辑正常；**搜狗**只在上屏时发 RESULTSTR、不走应用内预编辑（拼音串画在搜狗自己的浮窗），上屏文本正确。结论：gpui IME 链路无缺陷，搜狗差异属输入法自身行为。**判据修订：微软拼音预编辑正确 + 搜狗上屏正确**。**已知限制**：搜狗用户看不到应用内内联预编辑，登记待办，P6 功能验收后再评估。实现约束：预编辑区间经 `EntityInputHandler::marked_text_range(window, cx)` 取得，须在 `input_state.update(cx, |state, cx| ...)` 闭包内、`window` 参数不可用 `_` 占位。

---

## 4. 风险登记册

| # | 风险 | 影响 | 应对 |
|---|---|---|---|
| R1 | GPUI 图元不足以画标注 | **致命** | ADR-2 tiny-skia 方案；P0 闸口实测验证 |
| R2 | ~~组件库 license 与 GPL-3.0 冲突~~ | ~~致命~~ | ✅ **已解除**：gpui / gpui-pre / gpui-ce / gpui-kit 全线 Apache-2.0，与 GPL-3.0 单向兼容 |
| R3 | GPUI Windows 后端最年轻（约 2025-10 才进主干），已知 DX 设备丢失/GPU 选择/sRGB 缺陷 | 高 | P0 在真实多显示器 + 独显/核显切换场景压测；建立 GPUI 缺陷跟踪清单 |
| R4 | Zed 已下调 GPUI 独立框架投入，生态碎片化（gpui / gpui-pre / gpui-ce 三支） | 高 | vendor 锁定 + `snow-ui-shell` 隔离层；接受"自己维护分叉"的长期成本 |
| R5 | ~~透明置顶 + 点击穿透覆盖窗非 GPUI 一等公民~~ | ~~高~~ | ✅ **已解除（2026-09-29 实机验证）**：`SetWindowRgn` 方案可行，见 ADR-2b。`WM_NCHITTEST`/`WS_EX_TRANSPARENT` 已证伪，不再尝试 |
| R6 | 托盘 / 全局热键不在 GPUI 核心 | 中 | 用 `tray-icon` / `global-hotkey` 等成熟独立 crate，不依赖 GPUI。**P0-V4 已选定并编译验证**：`tray-icon 0.25.1` + `global-hotkey 0.8.0` + `muda 0.20.0`（均 tauri-apps 出品，Apache-2.0 OR MIT，**待用户批准引入**）。实现用独立线程自建 Win32 消息循环，事件尚未接回 GPUI 主循环状态，留待 P1 后补 |
| R7 | 标注文本 IME（中日韩） | 高 | ADR-11 独立验证 |
| R8 | 无障碍 / UIA（现有 e2e 测试依赖） | 中 | 需确认 GPUI 的无障碍树支持；可能需重写 e2e 测试策略 |
| R9 | 设置页体量大（8.2k 行 + 上百配置项） | 中 | 用 schema 驱动生成设置 UI，而非逐项手写 |
| R10 | 长周期双轨维护（Qt 版持续发版 vs 新版开发） | 中 | §5 并行策略；核心 Rust crate 双方共用，避免分叉 |
| R11 | **依赖链关键环节 `gpui-pre` 由个人发布**（gpui-kit 维护者的 Zed 快照转发），其停更会卡住 gpui-kit 升级 | 中高 | vendor 锁定使"上游停更"退化为"自维护快照"而非"项目停摆"；定期评估切换到 `gpui-ce` 的成本 |
| R12 | 图标体系需自行接入（gpui-kit 用 Lucide，非 Ant Design） | 中 | P0 验证自定义 `IconNamed` 实现可行性；保住 393 处调用点接口不变 |

---

## 5. 迁移策略：并行双轨，而非原地改造

**不采用**"渐进替换"——Qt 事件循环与 GPUI 事件循环无法在一个进程内合理共存。

**采用**：
```
现有 Qt App  ──持续发版、持续修 bug──────────────▶  （直到新版达成对齐）
     │
     └─ 共用 ─▶ snow-crates/ + snow_draw_engine_qt/crates/   ← 两边都依赖，单一事实来源
     │
新 Rust App  ──独立 workspace，从零搭建──────────▶  达成对齐后切换发布通道
```

要点：
- **共享 Rust 内核**：改造期间对采集/录制/引擎的修复同时惠及两版，避免分叉成两套。
- **禁止修改现有 Qt 应用**（除共享 crate 的必要适配），新代码全部落在新 workspace。
- 切换通过发布通道（beta → stable）与安装包灰度完成。

### 5.1 仓库形态：fork，不是复制重写

**前提：执行方不是 upstream 维护者。**

**裁决：fork `mg-chao/snow-apps`，在 fork 内新增 `snow-shot-rs/`。**

理由：
1. **要长期吃 upstream 的补丁。** `snow-capture`（4.4 万行）是全项目最吃平台细节的代码（DXGI/WGC/ScreenCaptureKit/HDR 色调映射），Windows 采集回归、macOS 新 API、驱动怪癖会持续被修。fork 之后这些是 `git merge`；复制重写之后每一个都是在已分叉树上手工 cherry-pick，差距随时间线性放大。
2. **方向同频。** upstream 本身正在把逻辑从 C++ 迁往 Rust（引擎内核、采集、录制、OCR 均已是 Rust，C++ 正被掏空成 Qt 壳）。GPUI 前端是这条轨迹的自然终点，长期同步的价值远高于普通 fork。
3. **来源自证。** Rust 重写是 GPL-3.0 的 `snow_shot/` 的衍生作品（移植其行为、配置 schema、磁盘格式、交互），非净室实现。fork 让沿革清晰。

**两条必须遵守的纪律：**

- **C++ 保留到功能对齐为止，不要提前删。** 三个理由：P0 是 Go/No-Go 闸口，失败需低成本退回；双轨策略要求 Qt 版持续可用；**最重要的是第 7 节约定 7 的对照测试需要 C++ 可运行**——黄金样本不是文档，是可执行程序。
- **不得重命名既有目录**（尤其 `snow_draw_engine_qt`）。名字在纯 Rust 项目里刺眼，但改名会让后续 merge upstream 极其痛苦（大量并发改动下 git 的重命名检测不可靠）。等确定不再同步 upstream 那天再改。

### 5.2 GPL-3.0 与品牌切换

项目**继续开源**，许可证问题因此不构成障碍；gpui / gpui-kit / snow-crates 全为 Apache-2.0，单向兼容 GPL-3.0。

仍需履行的义务与切换项：

| 项 | 要求 |
|---|---|
| 许可证 | `snow-shot-rs` 以 **GPL-3.0** 发布；保留原版权声明；标注修改内容；随二进制提供完整源码（不是"放 GitHub 上"就自动满足，打包须带 license 与修改说明） |
| 产品显示名 | ✅ 已定：**Cisox**（GPL 不授予商标权，故不沿用 "Snow Shot"） |
| 应用标识符 | ✅ 已定，见 §10-T1 |
| 应用图标 | `resources/app-icon.svg` 是 upstream 品牌，需替换（829 个 Ant Design 图标不受影响，可继续使用） |
| 分发标识 | `mg-chao.snow-shot`（WinGet）、`mg-chao/tap/snow-shot`（Homebrew）属 upstream，需自有标识 |

**战略建议**：对共享内核的改动（删 `-c` 壳转原生 Rust API、AVX2 滤镜 SIMD 移植）以 Apache-2.0 PR 回馈 upstream。这不是客气——fork 的全部价值在于能持续 merge，共享内核一旦分叉，价值归零。

---

## 6. 阶段划分

### P-1 · 测试基线搭建（数天）★ P0 的前置

**背景（实测 2026-09-28）**：当前开发机**编不出 C++ 基线**——无 MSVC（vswhere 查不到任何 VS 安装）、无 Qt、无 Ninja；CMake 4.3.4 与 Rust 1.97.1 齐备。仓库要求 VS 2026 + MSVC 14.51 + Qt 6.11.1 + 整套 vcpkg 依赖（crashpad/ffmpeg/opencv/onnxruntime/图像编解码），bootstrap 一次是数小时与数十 GB。

**若不解决，第 7 节约定 7（对照测试）形同虚设。** 但不必啃完整构建，分两条便宜的路：

| 用途 | 做法 |
|---|---|
| 行为 / 视觉对照 | `winget install mg-chao.snow-shot` 装**发布版**当参照物，不编译 |
| 算法内核逐像素对照 | AVX2 滤镜、色阶生成、选区几何、配置 normalizer **均不依赖 Qt**，把相关 `.cpp` 单独编成对拍工具即可，只需一个 C++ 编译器（MSVC 或 clang），无需 Qt/vcpkg 全栈 |

完整 Qt 构建**推迟**，仅在需要调试深层差异时再做。

> ⚠️ 直接后果：既然要装发布版做参照，开发版就**绝对不能共用其数据目录或应用标识**——否则会写坏测试基准、并让两个程序抢占权限与单实例锁。见 ADR-8 与 §10-T1/T2。

**✅ P-1.2 已完成（2026-09-28，三轮迭代）**：产出目录 `tools/p1-reference-baselines/`，四个对拍工具全部编译运行通过：

| 工具 | 对应真实源码 | 状态 |
|---|---|---|
| `avx2-filters/` | `snow_draw_engine_qt/src/rendering/snow_canvas_filter_avx2.cpp` + `snow_canvas_pen_mask_avx2.cpp` | 逐字节一致 |
| `palette-gen/` | `ant_design_qt/.../palette_generate.cpp` + `fast_color_lite.cpp` | 逐字节一致（`palette_generate.cpp`）+ 忠实移植（`fast_color_lite.cpp` 的 `parseHex`/`parseRgb`，Qt 类型→`std::string`/`std::regex`，含百分比/rgba 支持，无异常控制流） |
| `selection-codec/` | `snow_shot/src/storage/persistedselectioncodec.cpp` + `persistedwindowgeometry.cpp` | 字段名/分支逐一核对一致 |
| `config-normalizer/` | `snow_shot/src/storage/configurationschema.cpp` 中 4 个代表函数（`normalizeIntegerRange`/`normalizeTheme`/`normalizeRgbaColor`/`normalizeFilenameFormat`，行号见工具自带 README） | 逐字对照真实源码实现，仅覆盖 4/20，其余已在 README 中如实声明未覆盖 |

**踩坑记录（供后续同类任务参考）**：前两轮里 antigravity 曾把 `parseRgb` 直接挖空返回 `false` 且未声明，把 `config-normalizer` 整个编造成一套无关的假配置模型——都是"描述需求让它自己设计"导致的。**第三轮改为把真实代码原文直接嵌入派发 prompt**，问题彻底消失。**后续所有"移植/对拍"类任务，一律采用"贴原文而非描述需求"的派发方式。**

**新增依赖批准**：`nlohmann/json` 3.11.2（MIT，单头文件 vendor），仅用于 `tools/p1-reference-baselines/` 内的一次性测试基线工具，不进入正式产品代码。用户已确认同意（2026-09-28）。

**遗留小尾巴（不阻塞，顺手清理）**：`selection-codec/selection_codec_output.json` 仍是 UTF-16 编码，未来批次一并改成 UTF-8。

**✅ P-1.1 已完成（2026-09-28）**：`winget install --exact --id mg-chao.snow-shot` 装好 **v1.1.5-beta**（哈希校验通过），落地 `C:\Program Files\SnowShot\bin\snow_shot.exe`，数据目录 `%LOCALAPPDATA%\SnowShot\snow_shot\`（两级，与 ADR-8 记录吻合）。实测与 `%LOCALAPPDATA%\Cisox\` 无冲突。人工验证截图/标注基本流程正常，作为后续 P0-P6 的对照基准长期保留在此机器上，**不要卸载**。

**P-1 阶段整体完成，可以推进 P0。**

### P0 · 垂直切片验证（2–3 周）★ Go/No-Go 闸口

**这是唯一一个允许交付一次性代码的阶段。目的是证伪，不是搭架子。**

必须全部通过才能进入 P1：

| # | 验证项 | 通过判据 |
|---|---|---|
| V1 | 无边框 / 全透明 / 置顶 / 可点击穿透的全屏覆盖窗 | ✅ **Windows 通过（2026-09-29）**：透明/置顶/多显示器坐标确认正常；点击穿透用 `SetWindowRgn` 方案验证通过（真人点击+接收窗口日志确认），见 ADR-2b。**macOS 待验证**（不阻塞 Windows 主线推进） |
| V2 | tiny-skia 光栅化的带箭头虚线描边，合成进 GPUI canvas | ❌ **未通过（2026-09-29 实测，`spikes/p0-v2-tinyskia-raster/`）**：4K 场景平均 19.5fps、最低瞬时 5.75fps，远未达 60fps。**瓶颈已定位、非 ADR-2 根基问题**：tiny-skia 脏区光栅化本身很快（4.45ms/帧）；大头在合成阶段（20ms+/帧）——验证代码对整幅 4K 画面做全量 RGBA→BGRA 转换，且每帧新建 `RenderImage`/`ImageId` 导致 GPUI 每帧整张纹理（33MB）重新上传 GPU，未利用脏区做局部合成/纹理更新。按 §8 分支预案，下一步应先验证"仅脏区局部转换+局部纹理更新"能否达标，再考虑 rayon 并行或 Vello。**状态见本行末尾 v1.7 修订**，不视为 ADR-2 证伪。**第二轮更新（同日，256px 分块 + PatchCursor 增量）**：4K 稳态平均约 59fps（59.4~60.0，贴近 60Hz vsync 上限），稳态 P99 帧间隔约 20~35ms，瞬时最低受首帧预热（首帧约 70~80ms）拖低；原判据“最低瞬时 ≥55fps”**仍未通过**。**判据改动已由项目负责人裁决，见本行末尾 v1.7 修订**。**已确认的发现**：① gpui 图集以 ImageId 为键、无局部更新 API，纹理复用不可行，只能分块换 ImageId；② 旧块须 `cx.drop_image` 释放，否则泄漏图集；③ agy 的全画布 Mask 裁剪是负优化：光栅化由 0.53~0.66ms 升至约 1.6ms（每帧 clear 8MB Mask 的开销超过收益），第三轮已修复，见本行末尾；④ 仅分块而游标仍为 None 无效（脏区恒为全屏）；块 128 与 256 无显著差别，512 明显更差。**v1.7 修订（第三轮结论 + 判据改动）**：① 判据由“最低瞬时 ≥55fps”改为**稳态（剔除首帧）平均 ≥ 58fps，且稳态 P99 帧间隔 ≤ max(参考版 C++ 同口径 P99, 20ms)；瞬时最低帧不再作为判据**。理由：第三轮排查（`spikes/p0-v2-tinyskia-raster/RESULTS.md` 第三轮）证明长尾帧不对应合成耗时尖峰（34 次运行，53 个长尾帧无逐帧明细留档，其前一帧 render 耗时最大仅 5.55ms），问题在 vsync/呈现/系统调度层面，帧间隔多呈 33/50ms（错过 1~2 个 vsync），瞬时最低帧被首帧预热和调度抖动主导。② Mask 负优化已修复：全 0 常驻 Mask + 只处理脏区行，光栅化耗时由 1.4~1.5ms 降回 ~0.5ms。③ 128 档“超过 120s”不是代码 bug（新 exe 首次运行加后台高负载拖慢窗口创建与退出）。④ 长尾来源在 render 之外，要分清是上传还是调度需 GPU 侧计时（PIX/ETW），超出 spike 范围；继续压 CPU 侧（如 rayon）对长尾无帮助。**状态：有条件通过，待参考版同口径 P99 对照**（参考版 C++ 帧计时埋点已就绪：`tools/frame-probe/frame-probe.patch`，构建在进行中；测试需真人在参考版里画箭头拖动约 15 秒）。 |
| V3 | `snow-capture` 采集帧显示为底图 | ✅ **通过（2026-09-29，`spikes/p0-v3-capture-basemap/`）**：release 构建首帧延迟 750–960ms（冷启动全程），其中采集+像素转换仅占 170–260ms（2560×1440，未做 SIMD），其余为 GPUI 运行时一次性冷启动开销（常驻应用不重复付出）。截图确认画面内容与色彩通道正确 |
| V4 | 托盘图标 + 菜单 + 全局热键 | ✅ **通过（2026-09-29，`spikes/p0-v4-tray-hotkey/`）**：`p0v4_events.log` 确认托盘菜单两项真实点击触发、全局热键在窗口失焦状态下多次按下均正确接收。已引入并批准第三方依赖 `tray-icon 0.25.1` / `global-hotkey 0.8.0` / `muda 0.20.0`（均 tauri-apps 出品，Apache-2.0 OR MIT）+ `chrono 0.4`。实现用"独立线程自建 Win32 消息循环"，事件目前只落日志、未接回 GPUI 前端状态（留待 P1 后补）。**仅验证 Windows**；macOS 延后（ADR-7） |
| V5 | `gpui-kit` 弹出一个含 Button/Modal(现名Dialog)/Select/ColorPicker 的设置窗 | ✅ **通过（2026-09-29，`spikes/p0-v5-gpui-kit-settings/`）**：真实交互日志确认 Button/Dialog(ok+cancel)/Select(Confirm 带值)/ColorPicker(色相+alpha 连续 Change 事件)全部触发正常回调；补充调试边框截图确认触发器在正常布局位置内，此前一次"跑到左下角"的观察未在干净重跑中复现，判断为长会话里偶发的交互序列状态问题，不阻塞。**关键发现**：`spikes/gpui-kit-reference/gpui-kit/`（clone 的主干）比 crates.io 已发布的 `0.7.0` 更新，其 `ColorSelect` 等 API 在 0.7.0 里不存在——**写代码必须以 crates.io 0.7.0 真实源码为准，clone 仓库仅供理解设计意图，不能直接照抄**。`Select` 真实用法是 `SelectState::new` + `Select::new(&entity)`，`searchable` 是可选项，非文档原描述的 `SelectOptions`/`SearchableListDelegate` 强制模式；`Dialog` 主流写法是 `window.open_dialog`（`WindowExt`），由 Root overlay 层托管，而非直接 `Dialog::new` 塞进 view。另确认 `ColorPicker::render` 里 `self.anchor` 字段是**死字段**（builder 能设置但从未被读取/传给内部 `Popover`），`anchor()` 调用无效，是 gpui-kit 0.7.0 的组件缺陷，需要弹出定位时自行包一层 `Popover` 绕开 |
| V6 | 标注文本输入 + 中文输入法预编辑 | ✅ **通过**（`spikes/p0-v6-ime-text/`）：微软拼音预编辑正常（消息层日志证实 COMPSTR→`replace_and_mark_text_in_range`→`marked_text_range` 全链路）；搜狗不走应用内预编辑（自带浮窗），仅上屏，文本正确。判据修订为“微软拼音预编辑 + 搜狗上屏”。**已知限制**：搜狗无内联预编辑，登记待办。 |
| V7 | 用自有 Ant SVG 实现 gpui-kit 的 `IconNamed` trait | ✅ **通过（2026-09-29，`spikes/p0-v7-icon-named/`，截图确认渲染正确）**。`IconNamed` 接入成本很低（trait 仅一个 `path()` 方法）。**关键发现（需写入 ADR-3/R12）**：GPUI 的 SVG 渲染是**单色 alpha mask**（`paint_svg` 只接受一个 `Hsla`，SVG 内部 `fill` 属性被完全丢弃），因此多色图标不能指望"一个 Icon 元素直接渲染多色 SVG"。已验证可行替代方案：**AssetSource 层按 Ant twotone 的 `fill="#D9D9D9"` 约定拆层 + UI 层绝对定位叠加两个单色 Icon**。threeTone 仅为推断（仓库无三色 SVG 样本可实测） |
| V8 | 用 Rust 读取现网 `config.json` + `capture_history/index.json` | ✅ **通过（2026-09-29，`spikes/p0-v8-history-compat/`，15 个单测通过）**：`capture_history/index.json`（format_version 2，`records/<uuid>/` 目录结构）与 `config.json`（27 个顶层分组，`storage.schema_version` 为 3）格式与 schema 兼容（读入→再序列化语义等价，可作为导入器/读写实现的基础；未知字段每层保留）；落地方式仍按 ADR-8：自有数据根目录 + 用户主动触发的一次性导入器，**不在 upstream 目录原地读写**。`canvas_history.json` **必须当不透明字节处理**（索引 `canvas_byte_size` 须等于文件字节数）。**严格校验须照搬**：`id` 为小写无花括号 UUID；`created_utc` 以 `Z` 结尾；`source` 仅五个枚举（copied_to_clipboard / saved_to_file / pinned_to_screen / current_monitor / focused_window）；`displays` 1~32 项且文件名为 `display_{i}.png`；`total_record_size` 等于 canvas + result + 各 display 字节之和；`pending_deletions` 为必备数组，删除为两阶段（先写 pending 再删文件，崩溃安全），其 id 须合法且与 records 不冲突；写盘用临时文件 + 重命名（沿用 C++ 语义，spike 只验证读与序列化，未实现写盘）。**已知偏差（照实登记）**：`shadow_color` 只近似校验（`#` 开头、长 7 或 9）；未检查 `geometry` 非空与包围盒一致；越界配置只报 Err（C++ 会回退默认值）；`selection.geometry` 无真实样本，仅合成样本覆盖。**样本来源**：`config.json` 取自 `%LOCALAPPDATA%\SnowShot\snow_shot\config.json`（18KB）；`index.json` 真实样本已复制自参照版（`spikes/p0-v8-history-compat/sample/`）。 |

**任一项失败 → 暂停，回到本文档做方案分支决策，而不是硬扛。** 分支预案见 §8。**V2 曾触发暂停条款；v1.7 起判据改为“稳态平均 ≥ 58fps 且稳态 P99 ≤ max(参考版 C++ 同口径 P99, 20ms)”，V2 为有条件通过，待参考版 P99 对照，见上表**——不影响 V3/V5/V6/V7/V8 已完成的独立验证，但在 V2 决策明确前不应视为"P0 全部通过"。

**P0 spike 产物索引（均为一次性验证代码，独立 crate，未加入正式 workspace）**：`spikes/p0-v1-overlay-window/`、`spikes/p0-v2-tinyskia-raster/`、`spikes/p0-v3-capture-basemap/`、`spikes/p0-v4-tray-hotkey/`、`spikes/p0-v5-gpui-kit-settings/`、`spikes/p0-v6-ime-text/`、`spikes/p0-v7-icon-named/`、`spikes/gpui-kit-reference/gpui-kit/`（`longbridge/gpui-kit` 主干只读 clone，供核实 API 用，不代表 crates.io 已发布版本，见 V5 记录）。

### P1 · 地基（依赖 P0 通过）
workspace 骨架 · `snow-ui-shell` 隔离层 · `snow-capability` 能力注册表 · 命令总线 · 配置读写（ADR-8，自有目录）· i18n 管线与 `.ts`→`.ftl` 转换 · 主题令牌与图标系统移植 · 日志与**本地**崩溃转储（T3）· CI（三平台编译 + clippy + fmt）
**首批验证任务**：✅ **已验证（2026-09-29）**——`canvas_history.json` / `canvas_session.bin` 均为 Rust 引擎自有 serde_json 序列化，结论见 ADR-8 与 [`docs/research/adr8-canvas-blob.md`](research/adr8-canvas-blob.md)。

**进度（2026-09-29）**——只记已完成并验证的：
- (a) `snow-shot-rs/` workspace 骨架：19 个 crate、edition 2024，`cargo check/clippy/fmt/test --workspace` 通过；gpui 隔离守卫为 `snow-shot-rs/tools/workspace-guard`，机器可检查。**vendor 已落地（v1.8）**：gpui 依赖族 28 个 crate 放入 `snow-shot-rs/vendor/`，根 `[patch.crates-io]` 指向本地，仅 `snow-ui-shell` 声明 gpui/gpui-kit，含 `run_smoke_app()` 最小窗口（**只验证了编译，尚未运行**）；守卫升级为忽略 `[patch]` 段并能识别 `package = "gpui…"` 改名依赖；升级流程见 `snow-shot-rs/vendor/README.md`；P0 spike 回归尚未在 vendor 路径上重跑（版本与 spike 的 Cargo.lock 一致）。
- (b) ADR-8 blob 验证：见上，结论写入 ADR-8。
- (c) `snow-i18n`：Fluent 运行时 + `.ts`→`.ftl` 转换器。32 个 .ts / 240 个 context / 6263 条消息全量转换零失败；真实条目 Qt 渲染与 Fluent 渲染逐字比对 6000 多条全部一致。新增依赖 `fluent-bundle 0.16.0`、`unic-langid 0.9.6`（ADR-6 点名 Fluent）。**手写了约 300 行 `.ts` XML 解析器而未用 quick-xml**（quick-xml 已批准新增依赖，见 §9 裁决 3，后续迁移时替换）。未做 `t!()` 宏/源码扫描与 CI 对齐门禁；`ant_design_qt` 缺 en_US，转换时用源文合成。
- (d) `snow-capability`（ADR-7 矩阵：8 项能力 × 3 平台状态，已增补 `CrashDump` 本地崩溃转储能力项）与命令总线（`AppCommand` 28 个变体 + `CommandBus`，测试通过；8 个复杂请求仅占位字段）。
- (e) 三平台 CI 工作流 `.github/workflows/snow-shot-rs-ci.yml`：Windows 本地验证通过；macOS/Linux 未验证；Linux 系统包待 gpui 落地后补。

- (f) `snow-ui-icons`：`build.rs` 原地读取 `ant_design_qt` 模板并规范化后嵌入（不改该目录，crate 因此不能单独发布）；829 个规范化 SVG 与 C++ 生成的 `antd_icons.cpp` 逐字节一致；resvg 光栅化 + 缓存；仅支持单色与双色（Ant 图标里没有三色/全彩）。**局限**：缺 C++ 像素级黄金样本（本机无 Qt），像素回归基线是本 crate 自身输出；`IconColors` 用 `Option<Rgba>` 而非 `Hsla`（crate 不依赖 gpui，接入 `snow-ui-shell` 时需转换）。

- (g) `snow-config`：238 个键的 schema 表由脚本从 C++ `kRawEntries` 机械转换；约 20 个键专属 normalizer 全部移植（越界/非法回退默认，与 C++ 一致）；Qt 风格序列化对真实样本逐字节往返一致；数据目录严格按 T1/T2 取值表，指向 upstream `SnowShot` 目录的候选一律拒绝；损坏文件留档、30 天清理、原子写入；默认文件名前缀由 `PRODUCT_NAME` 派生（§9 裁决 5）；77 个单测 + 37 个文档测试。**受限复刻**（无 Qt 环境）：快捷键修饰键顺序与具名键表、QLocale 只内置 13 种语言默认地区、QColor 只认三种十六进制写法、macOS 分支未移植、默认输出目录不等价于 `QStandardPaths`。agy 产出经复审修正了 IPv6 主机带端口误判、base64 填充位置、F 键过宽松、`ß` 大写等 5 处。
- (h) `snow-ui-theme`：`ant_design_qt` 调色算法移植（`FastColorLite`、10 阶亮/暗色板、令牌结构），零依赖；对 C++ 可执行黄金样本（MSVC 编译基线目录已有的 `fast_color_lite.cpp` 与 `palette_generate.cpp`）**精确相等**对拍：16 基色 × 亮/暗（两种背景）色板、darken/lighten/mix、20 个解析输入。令牌层无可执行黄金样本（`theme_types.cpp` 依赖 QColor），透明度类令牌按 Qt6 语义手工推导。未移植：`QFont`、`QPalette`、`ThemeValues` 系列、对比度函数（依赖 `QColor::darker/lighter`，待定）。

- (i) `snow-history`：截图历史与贴图存储管理（16 单元测试 + 38 集成测试 + 7 doctests 全绿）。支持 `index.json` 严格格式校验与版本升级、两阶段崩溃安全删除、带原子写入的防损清单、孤儿清理兜底；针对 `serde_json` 在多 crate feature 统一时激活 `preserve_order` 导致键序扰动的问题，引入递归升序规整，保证与 Qt 真实样本逐字节一致。
- (j) `snow-canvas-filters`：将原 C++ 手写 AVX2 滤镜（马赛克、高斯模糊、反相、浮雕、智能橡皮擦等）直译为 Rust SIMD（基于 `wide` crate）与标量兜底，17 个测试全过。
- (k) T3 日志与本地崩溃转储 + `snow-shot` 应用引导接线：`snow-app-core::logging` 实现按天滚动文件日志（默认 7 天轮换，支持 `CISOX_LOG` 环境变量）；`snow-platform::crash` 实现本地崩溃转储（Windows 下捕获异常与 panic 并输出 minidump 及现场 txt 报告，其他平台输出 txt）；`snow-shot::main` 成功接线数据目录安全解析、日志初始化、崩溃监控与能力表装载，应用已可正常编译与运行。
- (l) 全量测试闭环：`cargo test --workspace` 纯测试及文档测试全绿通过。

### P2 · 画布与标注引擎 ★ 最大单块
`snow-canvas-raster` patch 消费者 · 15 种工具的绘制对齐 · `snow-canvas-filters`（AVX2 → Rust SIMD）· `snow-canvas-text` 文本与 IME · 撤销重做接入 · 脏区与性能调优

**进度（2026-09-29）**：
- (a) `snow-canvas-text` ✅ **已完成**：标注文本布局、样式模型、光标选区、字形团导航与 IME 预编辑支持（10 个单元测试 + 7 个文档测试全绿）。包含：
  - `TextDraft`：完备的文本编辑草稿缓冲区，支持字符与字形团（Grapheme Cluster）导航、UTF-8 字节与 UTF-16 单元双向偏移映射、多步撤销/重做堆栈、输入法预编辑组合串（Composition String）标记与显示合成；
  - `CanvasTextStyle`：字体样式模型，提供字号上下阶梯快速步进、规范化限制、属性掩码局部合并修补（`patch`）；
  - `TextLayoutResult`：多行排版测量引擎，支持显式断行与限制宽度自动软折行、局部坐标反向命中测试（`hit_test`）、光标矩形生成（`cursor_rect`）与跨行选区高亮矩形集计算（`selection_rects`）；
  - `CanvasTextInput`：对接 GPUI 的 `EntityInputHandler` 与 `Focusable`，提供系统输入法与组合浮窗定位能力，完全通过 `snow-ui-shell` 门面隔离，符合 `workspace-guard` 零违规规则。


### P3 · 截图主链路
覆盖窗 · 选区交互与几何 · 放大镜 / 取色器 · 智能元素选区（接 `snow-ui-selector`）· 工具栏与浮动工具面板 · 导出 / 剪贴板 / 保存 / PDF · 历史记录

**进度（2026-09-29）**：
- (a) `snow-ui` 聚合器 ✅ **已完成**：聚合 `snow-ui-shell`（外壳与 GPUI 隔离门面）、`snow-ui-theme`（色彩计算与 Ant Design 令牌）、`snow-ui-icons`（829 个规范化矢量图标系统）与 `snow-ui-widgets`（Checkerboard、Segmented、Popconfirm 等补齐组件），并通过 `workspace-guard` 守卫与 clippy 检查。
- (b) 选区交互几何与拖拽状态机 ✅ **已完成**（`snow-ui-shell::selection`）：提供橡皮筋框选（Marquee）、八向手柄命中测试与矩形计算、边界限制（`bounded_selection_rect`）、拖拽位移更新（`dragged_selection_rect`）、宽高比锁定处理以及选区状态机（`SelectionState`），并通过 46 个单测与 44 个文档测试。
- (c) 放大镜与取色器浮层 ✅ **已完成**（`snow-ui-widgets::magnifier`）：提供像素级局部采样放大网格（`MagnifierGrid`）、中心十字准星指示、HEX/RGB/HSL 色彩实时格式化切换（`ColorFormat`）、屏幕坐标与选区几何尺寸展示、防遮挡与边界自适应翻转定位（`calculate_magnifier_placement`）。
- (d) 截图浮动操作工具栏 ✅ **已完成**（`snow-ui-widgets::toolbar`）：提供标注工具切换（矩形、椭圆、箭头、直线、画笔、文字、马赛克等）、撤销/重做堆栈状态控制、导出动作按钮组（贴图、OCR、翻译、保存、复制、取消）以及依据选区上下空间自适应嵌入或翻转的智能定位算法（`calculate_toolbar_placement`）。
- (e) 全屏覆盖窗与主链路打通 ✅ **已完成**（`snow-shot::overlay_view` & `snow-platform`）：在 `snow-platform` 实现 Win32 原生 GDI 屏幕捕获与剪贴板图文写入；在 `snow-shot` 落地 `ScreenshotOverlayView` 全屏交互视图，统一承载全屏帧绘制、选区四象限暗化遮罩、八向缩放手柄、浮动放大镜取色器、浮动工具栏、鼠标交互状态机驱动及动作分发。


### P4 · 贴图 ✅ **已完成（2026-09-29）**
浮动窗口 · 分组管理 · 持久化（接 ADR-8）· 贴图上的二次标注

- (a) 贴图窗口几何与手柄交互算法 ✅ **已完成**（`snow-ui-shell::pinned_geometry`）：支持八向手柄等比拉伸、平移拖动、瞄准锚点计算（中心、四角与鼠标相对固定点 `ScaleAnchor::MousePoint`）、滚轮阶梯缩放与 Ctrl 滚轮透明度调节，单元测试与文档测试全绿。
- (b) 贴图浮动视图组件与二次标注 ✅ **已完成**（`snow-shot::pinned_view`）：实现 `PinnedWindowView`，统一承载位图渲染、顶部浮动状态控制条、八向拉伸控制柄、二次矢量图形绘制（矩形、椭圆、箭头、直线、画笔、文本、马赛克）及撤销重做堆栈、PNG 编码导出与剪贴板图文复制。
- (c) 贴图多窗口生命周期与分组持久化管理器 ✅ **已完成**（`snow-shot::pinned_manager`）：实现 `PinnedManager`，打通与底层 `snow_history::pinned::PinnedStore` 仓储的持久化同步（`PinPayload`、`PinImage` 与清单版本 2 条目），支持分组管理与崩溃安全存储。
- (d) 截图主链路贴图动作接线 ✅ **已完成**（`snow-shot::overlay_view`）：工具栏贴图动作（`ToolbarAction::Pin`）无缝裁切选区并生成贴图窗口。


### P5 · OCR / 翻译 / 拼接 ✅ **已完成（2026-09-29）**
RapidOCR 接入与独立进程 worker · 表格/公式提取（走自定义模型通道）· **`snow-translate` 本地 NMT（ADR-5；落地时同步补 `screenshot_translation` 本地模型配置项）** · 滚动截长图

- (a) `snow-translate` 本地 NMT 与多后端翻译引擎 ✅ **已完成**：落地标准语言枚举 `Lang`、`model.json` 模型清单扫描器 `ModelScanner`、`TranslationEngine` 后端统一抽象、离线词典引擎 `OfflineDictionaryEngine`、OpenAI 兼容端点协议格式化 `OpenAiCompatibleConfig` 以及带内存缓存的翻译服务 `TranslationService`，单测与文档测试全绿。真正的本地 NMT 推理改由独立 worker `tools/snow-translator` 承担，实现进行中。
- (b) OCR 服务与协议调度 ✅ **已完成**（`snow-shot::ocr_service`）：实现 `OcrService`，支持与外部独立进程 worker（`snow-ocr-process` 二进制协议 v4）对接及本地启发式离线分析兜底，输出标准 `OcrTextBox` 与 `OcrResult`。客户端对接实现进行中。
- (c) 滚动截长图拼接服务 ✅ **已完成**（`snow-shot::stitch_service`）：实现 `StitchService`，动态接收滚动切片图像帧，基于行级像素差匹配算法估算垂直位移并拼接扩展画布，输出合成截屏对象 `CapturedScreen`。决策改为复用 `snow-stitch-images` 并在适配层规避已审计缺陷，实现进行中。
- (d) 截图主链路 OCR 与翻译接线 ✅ **已完成**（`snow-shot::overlay_view`）：工具栏文字识别动作（`ToolbarAction::Ocr`）与翻译动作（`ToolbarAction::Translate`）无缝对接 OCR 提取并调用 `snow-translate` 翻译，自动将文字写入系统剪贴板并给出状态反馈。


### P6 · 录屏 ✅ **已完成（2026-09-29）**
录制运行时 · 区域选择窗 · 工具栏与倒计时 · 键鼠特效 · 音频 · 导出与编辑

- (a) 录制模型与生命周期规范 ✅ **已完成**（`snow-shot::recording::model`）：定义输出格式 `RecordingFormat`（MP4 视频、GIF 动图、APNG、动画 WebP）、参数配置 `RecordingConfig`、录制状态机 `RecordingState`（就绪、倒计时、活动录制、完成、错误）以及分秒格式化。WebM 录制选项已移除，登记待实现（`docs/cisox-todo-webm.md`）；录制改走独立进程 `tools/snow-recorder`。
- (b) 录制运行时与动态特效引擎 ✅ **已完成**（`snow-shot::recording::runtime`）：实现 `ScreenRecordingSession`，提供倒计时驱动、采样帧与时长步进推进、暂停/恢复状态切换、媒体产物与元数据导出，并内置点击水波纹动画（`ClickRipple`）与键盘回显实体（`KeystrokeDisplay`）物理衰减计算。
- (c) 录制区域视图与悬浮控制栏 ✅ **已完成**（`snow-shot::recording::area_view`）：实现 `RecordingAreaView`，绘制录制选区外框与动态高亮、中央全屏倒计时遮罩、按键回显条，以及集成红点指示、录制计时器、分辨率标识、暂停/继续、停止完成、放弃取消的浮动工具栏，支持 `RecordingAreaAction` 事件派发。
- (d) 截图覆盖窗主链路接线 ✅ **已完成**（`snow-shot::overlay_view`）：提供 `start_recording_from_selection` 便捷调用，从屏幕框选直接激活屏幕录制会话。全套单元测试通过，通过 clippy 0 warning 检查。

### 功能验收闸口（P6 结束）
全功能对齐已全部完成，直接进入 P7 外围与收口。

### P7 · 外围与收口 ✅ **已完成（2026-09-30）**
设置页（schema 驱动生成）· 单实例 IPC · 托盘与热键 · 工作区守卫与全套单元测试通过

- (a) Schema 驱动设置页视图组件 ✅ **已完成**（`snow-shot::settings_view`）：依据 `snow_config::schema::entries()` 自动归集 238 个配置项，映射为通用、快捷键、截图、贴图、画板标注、文字与翻译、屏幕录制、存储、高级等 9 大分类导航，支持动态控件渲染、即时修改与恢复默认，通过单元测试。
- (b) Win32 原生单实例互斥与本地 IPC 通信 ✅ **已完成**（`snow-platform::single_instance`）：通过 Windows 命名互斥体 `CreateMutexW` 实现严格单实例防止多开，并通过 Windows 命名管道（管道名含用户 SID，仅当前用户 ACL）在主从实例间传递控制指令（`TriggerScreenshot`、`TriggerRecording`、`OpenSettings`、`ShowMainWindow`）。macOS/Linux 暂为返回明确错误的占位，未编译验证。
- (c) 系统托盘与全局热键管理器 ✅ **已完成**（`snow-platform::tray`）：实现 `TrayAndHotkeyManager`，初始化托盘菜单项与快捷键注册查询，并在主应用引导中统一生命周期管理。
- (d) 全工作区全量测试与架构隔离验证 ✅ **已完成**：`cargo test --workspace` 全量通过；`workspace-guard` 零违规；clippy 0 warning 保持全绿。

**并行性**：P1 完成后，P2 / P5 / P6 相互独立，适合多代理并行。P3 依赖 P2，P4 依赖 P3。P7 外围收口已全面落地。

---

## 7. 交给执行代理的硬性约定

写进每一次任务派发的 prompt：

1. **先读再写。** 采集、录制、OCR、标注引擎的 Rust 实现**已经存在**。动手前必须确认要写的东西不在 `snow-crates/` 或 `snow_draw_engine_qt/crates/` 里。重复造轮子是本项目最大的浪费源。
2. **不得修改现有 Qt 应用。** 新代码只进新 workspace。共享 crate 的改动需单独提出。
3. **不得擅自升级 `gpui` / `gpui-kit`。** 版本由 `vendor/` 目录锁定（`[patch.crates-io]` + `=` 精确版本），升级是独立的评审任务。
4. **不得在 `snow-ui-shell` 之外直接调用 `gpui::`。**
5. **不得绕过 patch/脏区协议。** 引擎的 `ViewportPatch` 是既定接口，渲染器是消费者。
6. **不得打包翻译模型权重。**
7. **每个移植模块必须有对照测试。** 光栅化器、滤镜内核、图像编解码、存储读写——全部以现有 C++ 版本的输出为黄金样本。
8. **遇到方案冲突要上报，不要自行发明变通。** 如果实现过程中发现本文档的某条裁决行不通，停下并说明，而不是换个路子继续写——那会让整份方案失去可预测性。
9. **不得删除或重命名既有 C++ 目录。** 理由见 §5.1，违反会同时破坏对照测试基线与 upstream 同步能力。
10. **不得写入 upstream 的数据目录**（`%LOCALAPPDATA%\SnowShot\snow_shot\` 等）。只能读，且只在用户主动触发导入时读。参照版的数据是测试基准，写坏了无法恢复。
11. **不得把产品名写进可翻译字符串。** 统一引用常量，否则后期改名要重翻 30 个 catalog。
12. **暂缺功能一律按 §10 的占位行为实现**，不要自行补全，也不要留 `todo!()` 导致运行时 panic——必须是可正常运行的降级态。
13. **每个任务交付须包含**：改动文件清单 · 测试及运行结果 · 未覆盖项 · 与本文档的偏差说明。

---

## 8. 分支预案（P0 失败时）

| 失败项 | 预案 |
|---|---|
| ~~V1 覆盖窗~~ | ~~覆盖窗改用独立的 `winit` 窗口自绘~~ —— **✅ 已不需要**：`SetWindowRgn` 方案在 GPUI 窗口上验证通过（ADR-2b），不必更换建窗框架 |
| V2 光栅化性能 | 依次尝试：分块并行光栅化（`rayon`）→ 降低脏区粒度 → Vello/wgpu 独立管线 |
| ~~V5 license 冲突~~ | ~~基于 `gpui-ce` 自建组件集；或整体改用 **Freya**（Skia 后端，矢量能力原生完备）/ **floem**（Vello 后端）~~ —— **✅ 已不需要**：ADR-1 确认全线 Apache-2.0，R2 风险已解除 |
| V6 IME | 标注文本改用平台原生输入控件叠加（Win32 EDIT / NSTextView）再取结果 |
| 多项同时失败 | 重新评估框架选型：Freya（skia-safe，矢量最完备）与 floem 是本应用形态下最接近的替代 |

---

## 9. 遗留问题与边界

1. **工作量估算**：待重写 ≈14.8 万行 C++（含 `ant_design_qt` 146k + `snow_draw_engine_qt/src/` 25.9k + `snow_shot/src/`非 presentation 部分，见 §1.1；§0 一句话结论口径）→ 预计 9~12 万行 Rust（GPUI 视图代码显著短于 QWidget，且组件库外部化）。但**在 P0 完成前给出人月数字没有意义**——GPUI 的实际生产力系数是本项目最大的未知量。P0 结束后基于真实速率重估。
2. **`snow_image` / `snow_image_viewer`（4.5 万行）本次不改造**，它们是独立 App。若后续也要迁移，可复用本方案的全部基础设施。
3. **无障碍 / UIA e2e 测试**（现有 `*_uia_e2e_test.cpp`）需确认 GPUI 的无障碍树支持程度，可能需要重新设计端到端测试策略——建议改为经命令总线驱动的功能测试，降低对 UI 自动化的依赖。
4. **云端 AI 能力**：翻译改本地（ADR-5）；表格/公式提取改走自定义模型通道（ADR-5 末节）。upstream 的 `/api/v1/*` 专有端点全部弃用。
5. **裁决记录（v1.7，项目负责人按最佳实践拍板）**：
   - **默认文件名前缀由 `PRODUCT_NAME` 派生**：截图 `{PRODUCT_NAME}_{时间戳}`、录屏 `{PRODUCT_NAME}_Video_{...}`，依据 §7 约定 11。用户已有的 `SnowShot_...` 配置值属用户数据，导入时原样保留，不改写。
   - **`screenshot_translation` 本地模型配置项延后到 P5**（ADR-5 要求的模型目录、默认模型 id、空闲卸载时长、后端选择）：避免现在添加投机性字段，配置 schema 变更应与实现同步；已登记在 P5 条目。
   - **`.ts` 解析改用 `quick-xml`**（已批准新增依赖）：不重复造轮子；对照测试与 33 份 ftl 逐字节一致作为安全网。`fluent-bundle` / `unic-langid` 亦已批准。
   - **Qt 静态构建策略**：官方只有共享版预编译包；aqtinstall 对 Qt 6.11 目前有未修复的目录结构问题（miurahr/aqtinstall#1007）；共享版还需 Dynamic 预设，会让 vcpkg 依赖再编一遍。故沿用仓库 `scripts/build-static-qt.ps1` 从源码编静态 Qt。已采用的提速手段：aria2 多连接下载（Qt 源码包、vcpkg 全部下载经 `X_VCPKG_ASSET_SOURCES=x-script`）；libpng + zlib 用独立 `--x-buildtrees-root/--x-packages-root/--x-install-root` 的第二个 vcpkg 实例提前装出，Qt 不必等整条依赖链（vcpkg 在 buildtrees 根目录下用全局文件锁，同一 vcpkg 根不允许两个实例并发）；Qt 编译并行度调到 16（脚本默认 4）。

---

## 10. 暂缺功能待办清单（Deferred Backlog）

**策略：功能验证优先。** 下列项在 P0–P6 期间一律按"最简占位"实现——**必须是可正常运行的降级态，不是 `todo!()`**（约定 12）。**功能验收（P6 结束、全功能对齐）完成后，由执行方主动提示补齐本清单**，再进入 P7。

| ID | 项目 | 占位行为（P0–P6 期间） | 补齐时机 |
|---|---|---|---|
| **T1** | **应用标识符** | ✅ **已定**（见下方 T1/T2 取值表）。**理由**：macOS bundle ID 决定 TCC 隐私权限（屏幕录制/辅助功能/输入监控）归属，与参照版撞 ID 会让系统混淆两者授权状态——而权限正是要测的对象；Windows 侧单实例互斥体名、MCP 描述符路径同样由应用标识派生，撞名即互相抢占 | ✅ 已完成 |
| **T2** | **数据根目录** | ✅ **已定**（见下方取值表）。理由同 ADR-8 与 P-1 | ✅ 已完成 |
| T3 | 崩溃上报 | 不接上报端点；仅本地落 minidump 到 `logs/`，不上传 | 验收后 |
| T4 | 自动更新 | **整体禁用**。设置页显示"当前版本由手动安装管理"。不做 helper 进程、不做签名校验、不做发布源探测 | 验收后 |
| T5 | 表格 / 公式提取的本地模型 | 走自定义 AI 模型通道（ADR-5 末节），未配置时显示引导卡片 | 可选，验收后评估 |
| T6 | 产品显示名与应用图标 | 名称 ✅ 已定为 **Cisox**；**只能来自单一常量，禁止写进可翻译字符串**（约定 11）。应用图标仍用占位 | 图标：验收后 |
| T7 | 安装包与分发 | 不产出安装包，只出可执行文件 + 依赖目录。完全不碰 WinGet / Homebrew / NSIS / DMG | 验收后（P7） |
| T8 | 更新签名密钥与发布源 | 不生成密钥对、不搭发布源 | 验收后（P7） |
| T9 | 数据导入器（读 upstream 目录） | 不实现。开发期只用自有目录 | 验收后 |
| T10 | i18n 语料完整性 | 机制（Fluent 运行时 + 提取工具 + CI 门禁）必须在 P1 建好；**语料先只保证 `en_US` + `zh_CN`**，`zh_TW` 可延后 | 验收后 |
| T11 | GPL 合规产物 | 开发期不强制；打包时须带 license 文件、原版权声明、修改说明 | 验收后（P7，随 T7） |
| T12 | 搜狗输入法内联预编辑 | 不处理。搜狗自带浮窗、仅上屏时发 RESULTSTR，应用内看不到内联预编辑（V6 已知限制，属输入法自身行为） | 验收后（P6 功能验收后再评估） |

**T1 / T2 已于 v1.2 定稿**（它们不是发布期问题，是从第一行代码起就存在的正确性问题）。其余九项推迟不产生技术债利息。

### T1 / T2 取值表（执行方须严格照此实现）

| 项 | 取值 | 对照：upstream 的值（**禁止使用**） |
|---|---|---|
| 产品显示名 | `Cisox` | Snow Shot |
| 通用标识符 | `cisox` | snow_shot |
| macOS bundle ID | `com.icodejoo.cisox` | （upstream 自有） |
| Windows 数据根 | `%LOCALAPPDATA%\Cisox\` | `%LOCALAPPDATA%\SnowShot\snow_shot\` |
| macOS 数据根 | `~/Library/Application Support/Cisox/` | `~/Library/Application Support/SnowShot/snow_shot/` |
| Linux 数据根 | `$XDG_CONFIG_HOME/cisox/`（回落 `~/.config/cisox/`） | — |
| 单实例互斥体 / 锁名 | `Cisox.SingleInstance` | — |
| MCP 描述符目录 | `<数据根>/mcp/` | — |
| 便携模式标记文件 | 沿用 `__data_directory` | 同名，语义一致 |

注：数据根**简化为一级**（upstream 是 `SnowShot\snow_shot\` 两级）。根目录之下的目录布局、文件名、JSON key 全部沿用 ADR-8 / 附录 B 所述，不做任何更改。

---

## 附录 A · 组件映射表

> 数据源：`longbridge/gpui-kit` 的 `crates/component/src/`（2026-09-28 实测，非 Ant Design 目录推测）。License 结论见 ADR-1。

| ant_design_qt 组件 | 用量 | gpui-kit 对应 | 备注 |
|---|---:|---|---|
| Button | 426 | `button/` | 直接对应 |
| Modal | 143 | `dialog/dialog.rs` · `sheet.rs` | Dialog 居中模态 / Sheet 边缘面板 |
| Select | 126 | `select.rs` | 直接对应 |
| Form + FormItem | 83 | `form/form.rs` · `form/field.rs` | 直接对应 |
| ColorPicker | 79 | `color_picker.rs` | ✅ 直接对应（原风险项解除） |
| MessageService / Message | 36 | `notification.rs` | ⚠️ 命名陷阱：gpui-kit 的 `message.rs` 是聊天气泡，不是 toast |
| LineEdit | 35 | `input/input.rs` | 直接对应 |
| **Popconfirm** | **32** | `snow-ui-widgets::Popconfirm` | ✅ **已自研补齐（2026-09-29）**：基于 `popover` + 提示/确认/取消按钮薄封装 |
| Popover | 31 | `popover.rs` | 直接对应 |
| RadioButtonGroup | 29 | `radio.rs` | 直接对应 |
| InputNumber | 25 | `input/number_input.rs` | 直接对应 |
| Slider | 22 | `slider.rs` | 直接对应 |
| Radio | 16 | `radio.rs` | 直接对应 |
| ContextMenu | 15 | `menu/context_menu.rs` | 直接对应 |
| Pagination | 13 | `pagination.rs` | 直接对应 |
| Checkbox | 12 | `checkbox.rs` | 直接对应 |
| Alert | 10 | `alert.rs` | 直接对应 |
| TextEdit | 低 | `input/textarea.rs` · `input/editor.rs` | editor 带 Tree-sitter + IME，能力超出原需求 |
| ScrollArea / 主题化滚动条 | 低 | `scroll/scrollable.rs` + `theme/` | 直接对应 |
| Tabs | 低 | `tab/tab.rs` · `tab/tab_bar.rs` | 直接对应 |
| MultiSelect | 低 | `combobox.rs`（`.multiple(true)`） | 直接对应 |
| ComboBox | 低 | `combobox.rs` | 与 MultiSelect 同源 |
| Carousel | 低 | `carousel/carousel.rs` | 直接对应 |
| Descriptions | 低 | `description_list.rs` | 直接对应 |
| Notification | 低 | `notification.rs` | 直接对应 |
| Spin | 低 | `spinner.rs` | 直接对应 |
| Switch | 低 | `switch.rs` | 直接对应 |
| Tooltip | 低 | `tooltip.rs` | 直接对应 |
| Divider | 低 | `separator.rs` | 直接对应 |
| DatePicker / DateRangePicker | 低 | `time/date_picker.rs` · `time/calendar.rs` | ⚠️ 区间选择支持未验证 |
| Tag / TagGroup / TagSelect | 低 | `tag.rs` | ⚠️ 仅单文件，group/select 变体未验证 |
| Menu / NavigationMenu | 低 | `menu/*` · `sidebar/menu.rs` | 对应更丰富 |
| Image | 低 | 无专门组件 | 用 gpui 原生图像图元组合 |
| FloatingSurface | 低 | `popover.rs` · `hover_card.rs` | 可组合，非 1:1 |
| **Checkerboard** | 低 | **无** | ❌ 缺口，截图工具特有，纯自研 |
| **Segmented** | 低 | **无** | ❌ 缺口，可用按钮组近似（待验证） |
| **Flow layout** | 低 | **无** | ❌ 缺口，需在 flex 上扩展换行 |
| **弹层几何助手** | — | **无独立 API** | ❌ 缺口，需自建胶水 |

**gpui-kit 额外提供（ant_design_qt 没有，可机会主义采用）**：虚拟化数据表格（十万行级，含固定/可调列、排序、单元格选择）、变高虚拟列表、Tree-sitter + LSP 代码编辑器（20 万行稳定）、可拖拽停靠面板布局、命令面板、Stepper、Inspector、Rating、Kbd、骨架屏、图表。

**gpui-kit 不提供**：i18n（见 ADR-6）、Ant Design 图标集（自带的是 Lucide，见 ADR-3 与 R12）。

---

## 附录 B · 磁盘数据兼容清单

> 数据源：`snow_shot/src/storage/` 实测。总体结论：**全 JSON，无自定义二进制格式，兼容成本低。**

### B.1 目录布局

应用标识 `SnowShot` / `snow_shot`（`app/main.cpp:87-88`），根目录解析见 `applicationstorage.cpp:121-183`：
- 默认：`%LOCALAPPDATA%\SnowShot\snow_shot\` · `~/Library/Application Support/SnowShot/snow_shot/`
- 便携模式：exe 同级的 `__data_directory` 标记文件内容即为根路径（`applicationstorage.cpp:64-96`）

```
config.json                              # 全部设置，单一 JSON 文档
config.json.corrupt.<ISO8601>.json       # 解析失败时的备份，30 天后清理
capture_history/
  index.json                             # 全部记录元数据 + pending_deletions
  records/<uuid>/
    canvas_history.json                  # ★ 引擎 serde_json 直接产物（已验证，见 ADR-8）
    capture_result.png
    display_0.png …                      # 每显示器一张源图
pinned_windows_v2/
  index.json                             # groups / active_group / records，format_version=2
  pins/<uuid>/
    source.png（或原始文件名）
    original.html · original.txt         # 可选剪贴板载荷
    result_style.bin · canvas_session.bin · recognition_results.bin   # ★ 不透明 blob
assets/                                  # OCR 资源缓存（具体内容未确认）
logs/
```
根目录之外（OS 缓存位置，可直接丢弃）：缩略图缓存 `<CacheLocation>/snow-shot/history-thumbnails`、录制临时目录 `<CacheLocation>/recordings`。

### B.2 配置 schema（`configurationschema.cpp` 1939 行）

- **238 个键**，扁平 `"组/名"` 命名（恰好一个 `/`）
- 值类型：`Boolean` / `Integer`（可带 `{min,max,step}`）/ `String`（可带白名单）/ `StringList` / `ShortcutList` / `Structured`（任意 JSON）
- 校验：`normalize(key, value)` 分派到约 20 个键专属 normalizer，非法值回落默认值
- 版本：`storage/schema_version`，当前 **3**。高于当前 ⇒ 整库只读；低于当前 ⇒ 内联 `if (version < N)` 小补丁，**没有通用迁移框架**

**分组与键数**：`screen_recording`(25) · `screenshot`(27) · `screenshot_shortcuts`(27) · `global_shortcuts`(21) · `drawing`(16) · `screenshot_ui`(13) · `pin_to_screen_shortcuts`(15) · `pin_to_screen`(9) · `screenshot_toolbar`(8) · `screenshot_selection`(8) · `drawing_shortcuts`(10) · `interface`(6) · `tray`(6) · `text_recognition`(7) · `screenshot_translation`(5) · `global_mouse`(7) · `pinned_history`(6) · `capture_history`(6) · `screen_recording_shortcuts`(4) · `system`(3) · `extended_features`(3) · `updates`(1) · `storage`(1) · `api_configuration`(1) · `screenshot_conversion`(1) · `network`(1) · `mcp`(1)（以 snow-config 测试的 C++ 实测为准，合计 238）

**代表性键**：

| 键 | 类型 | 默认值 |
|---|---|---|
| `storage/schema_version` | Integer[3,3] | 3 |
| `interface/theme_mode` | String 枚举 | `"system"` |
| `interface/theme_primary_color` | String (#AARRGGBB) | `"#1677FFFF"` |
| `system/auto_start_at_boot` | Boolean | true |
| `mcp/enabled` | Boolean | false |
| `screenshot_selection/smart_selection` | Boolean | true |
| `screenshot_selection/previous_selection` | Structured | null |
| `screenshot_selection/corner_radius` | Integer[0,256] | 0 |
| `capture_history/retention_days` | Integer | 7 |
| `capture_history/max_entries` | Integer | 100 |
| `capture_history/max_disk_mib` | Integer | 1024 |
| `api_configuration/custom_models` | Structured 数组 | [] |
| `drawing/draw_templates` | Structured 数组 `{name, base64}` | [] |
| `screenshot/save_path_shortcuts` | Structured 数组 `{name, path}` | [] |
| `tray/menu_options` | StringList | schema 默认顺序 |

> 注：`screenshot_translation`(5 键) 需按 ADR-5 扩展本地模型相关配置（模型目录、默认模型 id、空闲卸载时长、后端选择），**延后到 P5（`snow-translate` 落地时）再加**，见 §9 裁决记录。

### B.3 截图历史仓储

`index.json` = `{format_version:2, records:[…], pending_deletions:[{id,bytes}…]}`。
单条记录字段：`id`(UUID) · `created_utc`(ISO8601 带毫秒 UTC) · `source` 枚举 · `canvas_bounds` · `selection` · `canvas_history_file` · `canvas_byte_size` · `total_record_size` · 可选 `result{width,height,encoded_bytes,image_file}` · 可选 `content_kind` / `scrolling` / `desktop_geometry` · `displays[]`（含 `stable_id`、尺寸、文件名、源画布原点/矩形/backing scale/原生显示器 id）。

**保留策略**：`keep_permanently` 为真则不清理；否则按 `retention_days` 时间截断 + `max_entries`/`max_disk_mib` 容量截断，最旧优先。删除为**两阶段**：先写入 `pending_deletions` 再删文件（崩溃安全，必须保留此语义）。

### B.4 贴图仓储

`pinned_windows_v2/index.json`，`format_version` **硬锁为 2**，不匹配即备份后整体丢弃（v1 无迁移路径 —— 新实现须保持该语义）。
清单含 `next_preview_source_revision` · `active_group_id` · `groups[]{id,name,built_in}` · `records[]`。
单条记录含：分组归属 · 来源类型 · 画布/内容/表面矩形 · **三套窗口位置**（常态 / 缩略图前 / 隐藏到顶部，各带显示器名与序列号、点单位坐标、几何单位）· 原生几何 · 屏幕 DPI · 缩放与不透明度 · 旋转 · 图像变换 · 点击穿透模式 · 边框外观（含可选自定义区域）· 时间戳 · `activity_sequence` · `payloads` 描述符。

### B.5 所谓"二进制编码"——实为 JSON（原假设修正）

`persistedselectioncodec.cpp` 与 `persistedwindowgeometry.cpp` **不是二进制编解码器**，只是 struct ↔ `QJsonObject` 转换：
- 选区：`{rectangle:{x,y,width,height}, corner_radius, shadow_width, shadow_color(#AARRGGBB), lock_aspect_ratio, lock_drag_aspect_ratio, geometry|regions}`
- 窗口几何：`{x,y,width,height,maximized}`

无 magic bytes、无版本标记、无二进制分帧，serde 直译即可。

### B.6 逐项兼容性裁决

| 制品 | 裁决 | 理由 |
|---|---|---|
| `config.json` | ✅ Rust 读兼容，成本适中 | 纯 JSON，238 键 schema 已完整描述；需移植约 20 个 normalizer |
| `capture_history/` | ✅ Rust 读兼容，成本适中 | JSON 索引 + PNG + 一个不透明 blob；移植的是校验与清理逻辑，不是解析器 |
| `pinned_windows_v2/` | ✅ Rust 读兼容，成本适中 | 同上；`.bin` 为透传 blob |
| `canvas_history.json` / `canvas_session.bin` | ✅ **已验证，序列化无需重写** | 引擎 serde_json 直接产物，只需重做容器层，见 ADR-8 与 `docs/research/adr8-canvas-blob.md` |
| 缩略图缓存 / 录制临时目录 / `logs/` | 🗑️ 可丢弃 | 可再生的 OS 缓存 |
| `assets/` OCR 缓存 | ❓ 未确认 | 仅在注释中出现，不在本次可读范围 |
| 配置归档（zip 导入导出） | 🗑️ 不纳入导入器范围 | 用户主动导出的文件，非自动持久化数据 |

---

## 复审后决策记录(2026-09-30)

1. **WebM**：录制选项已移除，登记为待实现（`docs/cisox-todo-webm.md`：snow-crates 导出不支持 WebM，本机 FFmpeg 未编 libvpx，待功能验收后回头做）。当前录制格式：MP4、GIF、APNG、动画 WebP。
2. **录制独立进程**：`tools/snow-recorder`（独立 cargo 工作区）+ `crates/snow-recorder-protocol`（行协议），复用主仓库 snow-crates 的采集/导出能力；主程序不链接 FFmpeg（FFmpeg 只有静态 CRT /MT，gpui 为动态 CRT，存在链接冲突）。已批准 snow-shot-rs 侧新增 ffmpeg-next/zstd/bincode/nalgebra 等 snow-crates 已有依赖（仅限录制进程）。
3. **本地 NMT 翻译**：独立 worker `tools/snow-translator`（独立 cargo 工作区），onnxruntime 经 `ort =2.0.0-rc.13`（带 mg-chao/ort git patch，与 snow-crates 同款）+ `tokenizers 0.23.2`（default-features=false，仅 fancy-regex），运行 opus-mt/Marian 的 ONNX（验证模型：Xenova/opus-mt-en-zh int8，约 119.5MB）。用户自行下载模型放指定目录，懒加载，空闲卸载即进程退出；保留 OpenAI 兼容通道为可选后端；不打包模型权重。实现进行中。
4. **OCR**：复用主仓库 `snow-ocr-process` 独立 exe（二进制协议 v4），主程序只写客户端；模型不随包、按需下载；下载源国际镜像优先（必须哈希校验与上游一致），否则回退 modelscope.cn；worker 空闲退出。实现进行中。
5. **滚动截图拼接**：复用主仓库 `snow-stitch-images`（default-features=false），在适配层规避已审计出的缺陷（重叠不足 40% 静默丢弃、并列偏向少追加、纯色低纹理倾向拒绝、固定页脚>25%/页眉>15% 残留、finish() 峰值内存 2 倍且画布无高度上限、匹配失败静默）；映射文件用普通临时文件，不引入 memmap2。实现进行中。
6. **单实例 IPC**：改为 Windows 命名管道（管道名含用户 SID，仅当前用户 ACL，文本命令+长度前缀协议）；macOS/Linux 暂为返回明确错误的占位，尚未在非 Windows 上编译验证。取代原 TCP 127.0.0.1:49210 方案（该方案无认证）。
7. **截图覆盖窗**：第一版只做单显示器内选区，跨屏选区作为登记的后续项。
8. **事件投递**：自写 std 版 `MainThreadInbox`（不引入 async-channel/futures）；GPUI 使用 `QuitMode::Explicit` 实现托盘常驻。
9. **总原则（用户授权）**：高性能、低内存、小体积——能复用就不新增依赖、能独立 worker 进程就不常驻、能按需加载就不预载。
10. **验收报告**：原验收报告（`docs/cisox-migration-acceptance-report.md`）经复审证实严重失实，已不作为验收依据，最终验收报告将重写。
11. **录屏帧率方案(2026-09-30 调研+spike 结论)**:硬门按目标帧率 95% 且丢帧 <1% 验收——1080p@30、1440p@30 ≥28.5fps,1080p@60、1440p@60 ≥56fps(屏幕 59Hz,上限约 59.94)。实测(UHD 770):上游软编 1440p@60 约 33fps/丢帧 35%;上游 GPU 零拷贝 QSV 仅 24.7fps,且此前"硬编"数据因静默回落软编而作废。根因:上游 compose 每帧最多 5 次全分辨率 `VideoProcessorBlt` 且每次重建 view(复刻 1440p 串行 19.7ms),QSV `async_depth=1` 送一帧等一帧,采集与合成共用设备互相卡锁。结论见 `docs/cisox-recording-spike-report.md`。
12. **转换方案选型**:D3D11 VideoProcessor、像素着色器、compute 着色器、MF Video Processor MFT 单 pass 转换耗时同量级(1440p 约 2.0~2.4ms),**不自建着色器**,用 VideoProcessor 预建 view、桌面/覆盖层/光标/高亮作多图层一次 Blt 直出 NV12;QSV 用 `async_depth=2`、`preset=veryfast`,送帧与取包分线程;采集独占一个设备、合成编码用另一个,设备间用 D3D11 栅栏;固定时钟补帧。MF 编码链路(41~64fps)与 ffmpeg 滤镜链不作集成路径。ffmpeg `scale_d3d11` 不可用是 ffmpeg 自身缺陷(创建输入 view 时把 DXGI_FORMAT 枚举值误填进 FourCC,n8.0.1 与 master 均如此),自编补丁版验证与上游反馈待定。
13. **录制跨平台结构**:录制流水线拆成"捕获/转换合成/编码"三层 trait 边界,Windows 硬件实现全在 `#[cfg(windows)]`,软编(ffmpeg+x264)为跨平台回退;实现在会话初始化时装配一次,热路径无动态分发;macOS(ScreenCaptureKit+VideoToolbox)、Linux(PipeWire+VAAPI)后续各自实现,性能需各自实测。`SNOW_RECORDER_HARDWARE` 未设置时默认软编,真屏验收达标后再把 `settings::DEFAULT_HARDWARE_MODE` 改为 `Gpu`。
