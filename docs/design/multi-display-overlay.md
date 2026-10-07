---
title: 多屏截图覆盖窗设计（每屏一个覆盖窗 + 共享虚拟桌面选区）
status: active
updated: 2026-10-07
summary: 从单屏覆盖窗走向「每块显示器一个覆盖窗、选区可跨屏」的坐标约定、改动清单、分阶段验收判据与真机验证办法。
---

## 实施结果（2026-10-07，M0~M4 已落地，M5 按计划推迟）

**与原方案不同的实际做法（按第一原则：低风险、少拷贝）**
- **没有拆 `SharedState`**：仍然只有**一个** `ScreenshotOverlayView`，但它工作在**虚拟桌面画布坐标**（原点 = 虚拟桌面外接矩形左上角，非负）。选区、蒙版、标注、导出、历史、上次选区的逻辑一行坐标假设都没改。
- **`DesktopFrames`**（`desktop_frames.rs`）替代单张 `FrozenFrame`：每屏一张图 + 画布；`crop_rgba` 跨屏拼接、`sample_rgba_grid` 跨缝取样、`base_view` 多屏时才懒合成整画布（单屏零拷贝）。
- **每块屏一个薄窗口** `OverlayWindowView`：只持共享视图引用和自己的焦点句柄，渲染交给 `render_monitor(index)`。渲染 = 把整个画布场景整体平移 −本屏原点÷本屏 scale 后由外层 `overflow_hidden` 裁剪，所以所有元素位置计算不变；工具栏 / 样式面板 / 形状栏 / OCR 与翻译面板只在锚点屏（选区右下角所在屏）画，放大镜与提示条只在光标屏画。
- **跨屏拖动**：元素级 `on_mouse_move` / `on_mouse_up` 换成窗口级 `Window::on_mouse_event`（捕获阶段，不受悬停命中限制），事件坐标用 `canvas_point(index, logical)`（本屏原点 + 逻辑×scale，可为负）。
- **窗口命中**：一条共享 UIA 线程服务整张画布，`DisplayHover` 把桌面坐标路径换成画布坐标；细化结果按最近查询点归属。
- **会话协调**：`CaptureCollector` 等所有显示器并行采集到齐再统一开窗（光标屏最后开，拿焦点）；任一窗口关闭 → 共享视图关闭钩子 → 其余窗口一起关（幂等）。
- **上次选区**按**桌面坐标**存取（`canvas_origin`），换屏幕排布后对不上就不乱选。

**验证**
- 离屏：`DesktopFrames` 8 个、`canvas_point` / 跨缝导出 / 录屏跨屏禁用 / 懒合成 / 上次选区桌面坐标 / 收集器 / `DisplayHover` 等；`snow-shot` 全量 762 个测试通过，`clippy -D warnings` 干净。
- 真机（本机 DISPLAY1 (0,0) + DISPLAY2 (2560,0)，2560×1440，100%）：两块屏同时出窗、各显示冻结画面；基准入口下两窗口同一毫秒一起关闭；**光标在副屏时智能选区高亮出现在副屏**，放大镜与坐标正确。
- 未做真人验证：用鼠标**按住跨缝拖动框选**（本机 `SendInput` 合成按键送不到覆盖窗，自动化做不了；`examples/multi_overlay_probe.rs --manual` 可手动验证 `SetCapture` 语义）；混合 DPI（1.0 + 1.5）与负原点副屏（只有离屏单测）。

**已知取舍 / 未做**
- 历史记录里多屏会话写成**一张画布图**（单显示器记录），没有按显示器拆 `displays`；Qt 多显示器记录读回时因尺寸不符被跳过。
- 选区跨屏时**录屏 / 长截图被禁用**（提示文案目前写死中文，沿用该文件现有写法）；跨屏录屏 / 长截图的采集窗（原方案 M5）未做。
- 本方案之外的 Qt 差异：显示器热插拔时会话内不重排。

## TL;DR

- 决定（用户 2026-10-07 拍板）：**每块显示器一个覆盖窗 + 共享的虚拟桌面选区模型，选区可跨屏**；先出本方案，批准后再动手。
- 坐标一律以**虚拟桌面物理像素**（可为负）为准；窗口局部坐标 = 桌面坐标 − 该屏 `bounds` 原点；逻辑像素 = 局部物理 ÷ 该屏 `scale`。
- 最大未知量是「按住鼠标跨窗口拖动时，GPUI 还会不会把事件投给起始窗口」，所以**阶段 M0 先做 spike**，结论决定输入走窗口事件还是全局光标轮询。
- 分 6 个阶段（M0~M5），每阶段有可离屏跑的验收判据；真机只有两块 2560×1440@100% 并排屏，**混合 DPI 与负坐标本机验证不了**，需要用户临时改系统设置（见 §8）。
- 依据 [principles.md](../principles.md)：内存与帧率优先，整桌面拷贝只能按需、用完即放。

## 1. 现状（调研事实）

- 覆盖窗只开在光标所在的一块屏：`request_capture` → `pick_monitor` → `spawn_capture`（只抓该屏 `bounds`）→ `open_overlay`（`app_runtime.rs`）。`AppState` 里是单个 `overlay` / `overlay_view` / `window_hover`。
- 显示器信息齐全：`MonitorInfo{id, name, bounds, work_area, scale, is_primary}`，`bounds` 为虚拟桌面物理像素且可为负；进程已是 Per-Monitor-V2；`ShellContext::open_window` 支持指定显示器、物理矩形、透明、置顶、无边框，可多次调用开多个窗口。
- 采集：`capture_display(region)` 是 GDI `BitBlt`，region 为虚拟桌面坐标，`None` 抓整个虚拟桌面；单边上限 32768；屏间空洞得到黑色；锁屏 / 屏保下会失败。
- `ScreenshotOverlayView` 内置「一个 frame、`screen_bounds` 原点恒为 0、一个 `scale`」的假设，涉及：`frame`、`screen_bounds`、`scale`、`pick` / `window_hover`、`annotations`（按帧尺寸建）、`selection_image`、`region_mask`、`history_snapshot`、`previous_selection`。
- 窗口命中 `WindowPicker` 内部已经是桌面坐标查询（`desktop_point` 加了 monitor 偏移）；Windows 下 `Point.display_id` 被忽略。
- 录屏、长截图、贴图的区域**本来就用桌面坐标**（`monitor_local_to_desktop`、`pin_origin`），但录屏 / 长截图的采集窗仍绑单屏。
- 历史 schema 已支持多显示器（`displays` 1..=32、`source_canvas_origin`、`native_display_id`、`desktop_geometry`），但写入端 `assemble_draft` 写死单显示器，读取端取 `displays.first()`。

## 2. 目标与非目标

**目标**
1. 开始截图后**所有显示器同时出覆盖窗**（各自冻结本屏画面，按本屏 DPI 渲染）。
2. 框选 / 移动 / 调整选区可以**跨屏**；选区以桌面物理坐标存放。
3. 复制、保存、贴图、OCR、翻译、历史、「选回上一次选区」对跨屏选区行为正确。
4. 智能选区在每块屏上都能命中窗口 / 控件。

**非目标（本方案不做）**
- 跨显示器适配器的硬件编码优化（见 [cross-monitor-recording-hw.md](../research/cross-monitor-recording-hw.md)）。
- 显示器热插拔时「无缝续用」：会话期间配置变化直接取消本次截图。
- macOS / Linux。

## 3. 坐标约定（全方案唯一依据）

| 名称 | 含义 | 用在哪 |
|---|---|---|
| 桌面物理坐标 | 虚拟桌面物理像素，原点 = 系统虚拟桌面原点，可为负 | 选区模型、蒙版、标注画布、导出、历史、录屏 / 长截图 / 贴图 |
| 窗口局部物理坐标 | 桌面物理 − 该屏 `bounds` 原点 | 单窗口内的命中、渲染布局、放大镜取点 |
| 窗口逻辑坐标 | 局部物理 ÷ 该屏 `scale` | GPUI 元素尺寸与位置 |

规则：
1. **模型只存桌面物理坐标**；只有渲染与事件入口做换算，换算函数集中在新模块 `desktop_space.rs`，纯函数、可单测。
2. 事件入口：窗口逻辑 → 局部物理（用**该窗口**的 `scale_factor()`，每次事件重取）→ 桌面物理。
3. 选区矩形用半开区间 `[x, right)`，与现有 `PhysicalRect` 一致；`previous_selection` 仍存逻辑坐标，基准 scale = 选区左上角所在屏的 scale，并额外记 `desktop_origin`（见 §6.5）。
4. 屏间空洞：选区允许包含空洞；导出时空洞为透明（PNG / WebP）或白色（JPEG / BMP），**不拒绝、不吸附**。

## 4. 架构

```
CaptureSession（新，AppState 里取代 overlay / overlay_view / window_hover）
├─ displays: Vec<DisplayFrame { monitor: MonitorInfo, frame: FrozenFrame }>
├─ desktop: DesktopCanvas        // 按需合成：crop_rgba(桌面矩形)、base_view()（懒建）
├─ shared: Rc<RefCell<SharedState>>   // 选区状态机、region_type / draft / mask / op、
│                                      // pick 路径、工具与标注层、OCR / 翻译状态、历史翻页、状态栏
├─ picker: WindowPicker           // 单个，桌面坐标，覆盖窗出现前已抓快照
└─ windows: Vec<Entity<OverlayWindowView>>   // 每屏一个，只做渲染与事件换算
```

**共享什么 / 每窗独立什么**

- 共享（`SharedState`，桌面物理坐标）：`state`（Idle / Marquee / Selected / Reshaping）、`region_type`、`region_draft`、`region_mask`、`region_op`、`pick`、`tool`、`annotations`、`text_edit`、`ocr` / `translate`、`history_host`、`status_message`、`cursor_pos`。
- 每窗：本屏 `frame` 的 GPUI 图像、`tile_sprites`（本屏相交的标注分块）、`pending_drops`、`focus_handle`、放大镜、光标样式、`scale`。
- 工具栏、样式面板、形状栏、OCR / 翻译面板：**只在「锚点窗口」渲染一份**。锚点 = 选区右下角（工具栏锚点）所在的屏；选区未定时形状栏在光标所在屏。
- 标注画布：**一块**，尺寸 = 虚拟桌面外接矩形（物理像素），`dpr` 取主屏 scale；预览分块按屏相交裁剪后分发给各窗。
- 蒙版：**一块** 8 位 alpha，同样是虚拟桌面外接矩形；遮罩图按外接矩形（现有做法）在各窗里按相交部分切片。

**重构方式**：把现有 `ScreenshotOverlayView` 拆成 `SharedState`（逻辑，几乎全部现有字段与方法，坐标改桌面物理）+ `OverlayWindowView`（薄视图，持 `Rc<RefCell<SharedState>>`、`display_index`）。不改变现有单屏行为：单显示器时 `displays.len() == 1` 且桌面原点 = 该屏 `bounds` 原点，等价于现状。

## 5. 采集与底图

- **每屏一次 `capture_display(Some(bounds))`**，N 个采集线程并行，全部完成再统一开窗（避免先开的窗口遮挡后抓的屏）。保持现有 `FrozenFrame` 移动语义，不整屏拷贝。
- `DesktopCanvas::crop_rgba(rect)`：只在需要导出 / OCR / 历史 / 标注基底时，按选区相交的每块屏逐行拷贝到一块缓冲，空洞补透明。
- `DesktopCanvas::base_view()`：标注引擎需要连续的 BGRA 基底（马赛克 / 模糊采样）。**首次选中标注工具时才合成**整个虚拟桌面一块缓冲（两块 2560×1440 约 29 MB），会话结束释放；单屏时直接用该屏帧，不拷贝。
- 内存预算（本机 2×2560×1440）：冻结帧 2×14.7 MB（已有量级）+ 标注基底 29 MB（仅用标注时）+ 蒙版 3.7 MB（仅自定义区域）。验收时记录峰值。
- 失败处理：任一屏采集失败 → 整次截图取消并提示（沿用现有锁屏 / 屏保的失败文案）。

## 6. 各块改动清单

### 6.1 `desktop_space.rs`（新）
`window_to_desktop`、`desktop_to_window`、`rect_intersect_monitor`、`monitor_containing(point)`、`virtual_bounds(monitors)`。纯函数 + 单测（含负原点、混合 scale 1.0 / 1.5）。

### 6.2 事件与光标
- 每窗独立收鼠标事件，换算成桌面坐标后写入共享状态。
- **跨窗拖动（M0 结论：采用方案 A，2026-10-07）**：
  - 依据（GPUI 源码 `gpui-pre-windows/src/events.rs`）：按下时 `SetCapture(hwnd)`，之后 `WM_MOUSEMOVE` / `WM_LBUTTONUP` 持续投给起始窗口；move 的坐标取 `lparam` 的**带符号**高低字，可为负、可超出窗口，没有边界裁剪。
  - 注意：元素级 `on_mouse_move` 要求命中悬停，指针离开窗口后不会触发；跨屏拖动必须用窗口级原始监听 `Window::on_mouse_event`（不受命中限制，捕获阶段即可收到）。
  - 换算：`桌面物理 = 窗口 bounds 原点 + 事件逻辑坐标 × 该窗 scale`。
  - 实测局限：本机 `SendInput` 合成的按键没有稳定送达覆盖窗，自动探针（`examples/multi_overlay_probe.rs`）拿不到按下事件；留有 `--manual` 手动模式，M2 真机验收时用真人拖动确认。
  - 备用方案 B（若真人实测不成立）：拖动期间改用 `cursor_screen_position()` 在帧回调里轮询，成本很低，接口已存在。
- 光标样式、放大镜：以光标所在屏为准；放大镜取样跨屏边缘时从 `DesktopCanvas` 取。

### 6.3 选区与渲染
- 每窗渲染 = 共享选区 ∩ 本屏矩形：暗化遮罩的四向矩形、选区框、手柄（仅端点落在本屏时画）、尺寸标签（锚点窗）。
- 智能选区高亮框与过渡动画：高亮矩形按屏相交绘制；过渡动画状态放共享层，各窗读同一个 `displayed` 矩形。
- 草稿描边（折线 / 曲线 / 自由绘制）：每窗用桌面→局部换算后画自己的那一段。
- 工具栏摆放：沿用 `calculate_toolbar_placement`，输入改成「锚点窗局部坐标下的选区 ∩ 本屏」。

### 6.4 窗口命中
- 一个 `WindowPicker`，快照在**所有**覆盖窗出现之前抓；查询点 = 桌面坐标，结果为桌面坐标，不再在 worker 里裁到单屏（`screen_rect_to_frame` 的裁剪移到渲染层）。
- `Point.display_id` 在 Windows 忽略，保持 0。

### 6.5 导出、历史、上次选区
- `selection_image` / `history_snapshot` / OCR / 翻译 / 贴图：全部改走 `DesktopCanvas::crop_rgba(桌面选区)`。
- 历史 `assemble_draft`：`canvas_bounds` = 虚拟桌面外接矩形；`displays` 每屏一项（`stable_id` = 显示器名，`source_canvas_origin` = 屏原点 − 画布原点，`native_display_id` = `MonitorId`，`backing_scale`）；`desktop_geometry` 填桌面原点。读取端翻页时校验「显示器布局指纹」（各屏 id + bounds），不一致的记录按现有「不适用」规则跳过。
- `previous_selection`：矩形仍写逻辑坐标，基准 scale = 左上角所在屏；新增 `desktop_origin` 字段（旧版没有该字段，读取端缺省按单屏处理）；解码后按当前虚拟桌面裁剪。

### 6.6 录屏 / 长截图 / 贴图
- 贴图：已是桌面坐标，只需把 `with_pin` 的 `+ pin_origin` 去掉（选区本身已是桌面坐标）。
- 录屏 / 长截图：选区跨屏时**首期禁用这两个按钮**并给出本地化提示；选区在单屏内行为不变。跨屏录屏的采集窗留到 M5。

### 6.7 `ScreenshotOverlayView` 现有字段的归宿
`frame` → `DisplayFrame`；`screen_bounds` → `DesktopCanvas::virtual_bounds()`；`scale` → 每窗；`pick` / `window_hover` → 会话级；`annotations` / `region_mask` / `region_overlay` → 会话级（桌面尺寸）；`tile_sprites` / `pending_drops` → 每窗；`history_host` → 会话级（`LiveEndpoint` 持有 `Vec<DisplayFrame>`）。

## 7. 分阶段与验收判据

每阶段都要：相关测试全过、`cargo clippy --workspace --all-targets -- -D warnings`、`workspace-guard`、i18n 检查全绿，文档同步。

| 阶段 | 内容 | 验收判据 |
|---|---|---|
| **M0 spike（已完成）** | 双屏同时开覆盖窗；按下后拖出窗口，记录事件是否继续、坐标是否可为负 / 超界；每窗 `scale_factor()`；负坐标副屏落位 | 结论：方案 A（见 §6.2）；本机双屏同时开窗成功，两窗 `bounds` 与 `MonitorInfo.bounds` 一致（DISPLAY1 (0,0) / DISPLAY2 (2560,0)，scale 1）；合成输入局限已记录 |
| **M1 多窗采集与会话（已完成 2026-10-07）** | `CaptureSession`、N 路并行采集、N 个覆盖窗、关闭即全关；选区先限定在起始屏（行为 = 现状） | 离屏：用假 `Monitors`（含负原点、1.0 / 1.5 混合）测会话开关、`capture_gate`、关闭清理；真机：双屏同时出窗、Esc 全关、截图后内存不泄漏 |

**M1 实际做法与结果**：每块屏一个现有 `ScreenshotOverlayView`（暂不拆 `SharedState`，选区限定在起始屏）；`CaptureCollector` 等所有显示器并行采集到齐再统一开窗（光标所在屏最后开、拿键盘焦点）；一条共享的 UIA 命中线程，每屏用 `DisplayHover` 换算桌面坐标（提前完成了 M4 的“单 picker”）；会话钩子：任一窗口按下 → 其余窗口 `drop_selection`，任一窗口关闭 → 其余窗口一起关闭。离屏测试：收集器顺序 / 取消、`DisplayHover` 双屏换算与细化归属、会话钩子；全量 `snow-shot` 750 个测试通过。真机（DISPLAY1 (0,0) + DISPLAY2 (2560,0)，100%）：基准入口 `SNOW_OVERLAY_BENCH` 触发后两块屏同时出窗，结束时同一毫秒一起关闭，无残留进程。
| **M2 共享选区与跨屏渲染（已完成，做法见上）** | 拆 `SharedState` / `OverlayWindowView`；桌面坐标选区；跨屏框选 / 移动 / 调整；每窗切片渲染；锚点工具栏 | 离屏：`desktop_space` 全套单测；跨屏框选的状态机测试（起点终点在不同屏、负原点）；真机：跨缝框选，缝两侧遮罩无错位 |
| **M3 导出与标注（已完成：`DesktopFrames` 提供）** | `DesktopCanvas`；跨屏导出 / OCR / 翻译 / 贴图；标注与蒙版改桌面尺寸；放大镜跨缝 | 离屏：`crop_rgba` 跨屏拼接逐像素断言（含空洞透明）；标注基底懒建只在用工具时发生；真机：跨缝复制 / 保存 / 贴图，像素与屏幕一致 |
| **M4 命中 / 历史 / 上次选区（命中与上次选区已完成；历史按单画布图写入）** | 单 picker 桌面命中；历史多显示器写入与读取（含布局指纹）；`previous_selection` 加 `desktop_origin` | 离屏：历史写入后 `displays` 数量 / 原点断言；布局变化后翻页跳过；解码裁剪；真机：副屏窗口也能智能高亮与滚轮切层 |
| **M5 录屏 / 长截图跨屏（推迟，跨屏时禁用）** | 录屏区域窗与长截图窗支持跨屏选区 | 与 [cross-monitor-recording-hw.md](../research/cross-monitor-recording-hw.md) 的跨屏验收对齐；真机录一段跨缝视频 |

建议 M0 → M1 → M2 逐阶段停下来验收；M3~M5 可在 M2 通过后合并推进。

## 8. 验证办法

**离屏（CI 可跑）**
- 假显示器布局构造器（新，放测试工具模块，替代各模块各写一份）：单屏、并排双屏、副屏在左（负原点）、混合 DPI（1.0 + 1.5）、上下堆叠、非矩形（带空洞）。
- 视图用现有 `view_with` 思路扩成「多帧」夹具，给每屏一张可辨认的纯色 / 渐变帧，导出后断言像素来源。

**真机（用户本机：`\\.\DISPLAY1` 主屏 (0,0) 2560×1440，`\\.\DISPLAY2` (2560,0) 2560×1440，均 100%，Intel UHD 770）**
- 本机原样可测：并排双屏、跨缝框选 / 导出 / 贴图 / 历史。
- 混合 DPI：Windows 设置 → 显示，把 DISPLAY2 缩放临时改为 125% 或 150%，重新登录后测；测完改回。
- 负坐标：设置 → 显示，把 DISPLAY2 拖到 DISPLAY1 左侧（副屏原点变负），测完改回。
- 每次真机验证记录：显卡 / 驱动 / 缩放 / 排布，不同机器数据不直接比较。

## 9. 约束与红线

- `gpui::` 只能出现在 `snow-ui-shell`（`workspace-guard`）；窗口相关新能力走 `ShellContext` 门面。
- 不新增第三方依赖；如 M0 发现需要低级鼠标钩子，优先用已有 `windows` 绑定，仍需先征得同意。
- 不改 `snow_draw_engine_qt/` 等旧版目录；引擎侧不够用时在 `snow-canvas-raster` / `snow-shot` 里封装。
- 用户可见文案走 `.ftl`（en-US + zh-CN），不写死中文。
- 单显示器行为必须与现状逐项一致：现有 100+ 个覆盖窗测试在每阶段结束时全部保持通过。
- 整桌面缓冲只允许懒建、会话结束即释放；每阶段记录峰值内存。

## 10. 风险与待定

1. **跨窗拖动事件**（M0）：决定 §6.2 方案 A / B；B 会增加一条全局输入通路。
2. **多个全屏置顶透明窗的焦点与 Esc**：需保证无论焦点在哪个窗，按键都进共享状态；可能要把键盘事件统一从「当前聚焦窗」转发。
3. **GDI 并行抓取**：多线程各自 `GetDC(None)` 是否互相阻塞（M1 测耗时，对比串行）。
4. **混合 DPI 下的线宽 / 手柄 / 标签**：各窗按本屏 `scale` 取整，跨缝处看起来可能不一致，M2 真机肉眼验收。
5. **非矩形布局空洞**：当前约定为允许选中、导出补透明；若体验不好再收紧（改为吸附到屏）。
6. **显示器变化（热插拔 / 分辨率 / 缩放）发生在会话期间**：直接取消本次截图，不尝试重排。
7. **HDR / 色彩管理**：沿用现有 GDI 8 位采集，不在本方案范围。

## 11. 需要用户批准的事项

1. 方案整体（尤其 §4 的 `SharedState` / `OverlayWindowView` 重构）。
2. 先做 M0 spike（只新增一个临时示例，不动主程序），再决定 §6.2 的 A / B。
3. 首期跨屏选区**禁用录屏 / 长截图**，M5 再补。
4. 真机验证需要你临时改系统显示设置（§8）。
