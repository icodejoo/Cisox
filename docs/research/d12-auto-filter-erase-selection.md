# D12 自动滤镜 / 智能擦除 选型调研

> **状态（2026-10-08）**：P1 自动滤镜已实现（`snow-shot/src/auto_filter.rs`，详见审计表 D12 行）；`snow-shot` 新增 path 依赖 `visual-region-detector` 与 `snow-draw-engine-document`（后者引擎已带入，`Cargo.lock` 仅新增 `visual-region-detector` 一条本仓库包，无新第三方包）。P2 / P3 / P4 智能擦除未做，保持占位。
>
> 调研日期 2026-10-08，调研阶段只读，未改代码。依据：`docs/principles.md` §1 优先级链（高性能 > 低内存 > 高 fps > 少编译依赖 > 多用系统自带能力）与 §2 总原则（能复用就不新增依赖）。
> 标注：[读码]=本仓库源码；[网页]=联网；[推测]=未验证。

## 0. 结论先行

1. D12 实际是**两件事**，旧版都不是调 OpenCV 的 `inpaint()`：
   - **自动滤镜**（区域检测）：旧版调 `snow_visual_region_detector`（即 `snow-crates/crates/visual-region-detector`）。该 crate **默认就是纯 Rust 后端**（`default = ["pure-rust"]`，`opencv` 是可选 feature），带 OpenCV 4.12.0 对拍夹具。Rust 侧只需接线，**零新增依赖**。
   - **智能擦除**：旧版是自写的 Lab 空间多尺度 **PatchMatch**（约 876 行 C++），OpenCV 只当“图像原语库”用。原语少且简单，**纯 Rust 自写移植，零新依赖**。
2. 推荐：两件事都不引入新第三方依赖；自动滤镜先做（接线为主），智能擦除分三期自写移植。
3. 无系统自带能力可替代：Direct2D 内置效果没有 inpaint / 内容感知填充（见 §3）。

## 1. 旧版实际用了什么

### 1.1 自动滤镜 [读码]

- `snow_shot/src/presentation/tools/screenshotautofiltercontroller.cpp`：把截图转 `Format_BGR888`，在线程池调 `snow_detect_visual_regions(...)`，返回区域按类别索引映射为 `text / text_in_box / image / avatar / icon / message_box / text_block`（7 类），作为 `SnowCanvasAutoFilterRegion{id, bounds, category}`。
- 引擎侧 `snow_draw_engine_qt/crates/snow-draw-engine-document/src/auto_filter.rs`（区域记录校验、点击命中取最小面积区域）与 `snow-draw-engine-editor/src/auto_filter_workflow.rs`（401 行，拖选命中、代数失效）。这部分属引擎，Rust 端 `snow-app-core/src/command.rs` 已有 `AutoFilterRequest`、`screenshot_auto_filter` 命令占位。
- 检测 crate：`snow-crates/crates/visual-region-detector`，约 2300 行 + `backend/pure_rust/*` 约 2300 行；原语（形态学、连通域、中值/均值模糊、Canny、轮廓）已纯 Rust 化，并与 OpenCV 4.12.0 的夹具对拍（`opencv_fixtures.json`）。许可证 Apache-2.0，依赖仅 `image`（关 default features，仅 bmp/jpeg/png）、`serde`、`serde_json`；`opencv` 为 optional，不启用则不进编译图。
- 现状：审计 D05 已写明“未接 `visual-region-detector`”；`snow-shot` 已用 path 依赖复用 `snow-crates` 的其它 crate（`snow-ui-selector`、`snow-stitch-images` 等），模式现成。

### 1.2 智能擦除 [读码]

文件：`snow_draw_engine_qt/src/rendering/snow_canvas_smart_erase_algorithm.cpp`（876 行）；文档 `snow_draw_engine_qt/tests/snow_canvas_smart_erase.md`。流程：

1. 取遮罩（旋转矩形 / 画笔路径）与原图层合成的 ROI（上限 16 Mi 像素，施主上下文 ≤512 px）。
2. **表面拟合快路径** `surfaceFill`：外圈环采样做 RGB 仿射（1,x,y）鲁棒最小二乘，97% 内点才接受（用 `solve(DECOMP_CHOLESKY)` 解 3x3）。
3. **周期快路径** `periodicFill`：已知域内验证短平移周期。
4. 否则**多尺度 PatchMatch（Lab）**：`donorDomain`（`distanceTransform` + 腐蚀）、背景引导（高斯模糊加权颜色 + 纹理方差，沿水平/垂直/对角扫描线插值）、金字塔（`pyrDown`，掩码用 `resize INTER_AREA`）、每层 5/3/2 轮传播 + 随机搜索 + 投票，层间 `resize INTER_LINEAR` 上采样，最后 Lab→RGB。
5. 失败/无施主保持红色占位；双线程池异步，结果按几何 + 源修订缓存。

用到的 OpenCV 原语（`grep` 计数）：`getStructuringElement`(RECT/ELLIPSE)、`erode`/`dilate`、`distanceTransform`(L2, mask 5, 含 `DIST_LABEL_PIXEL` 最近施主标签)、`GaussianBlur`、`pyrDown`、`resize`(AREA/LINEAR)、`cvtColor`(RGB↔Lab, float)、`threshold`、`compare`、`bitwise_and`、`countNonZero`/`findNonZero`/`boundingRect`、`norm`、`solve`、`parallel_for_`。**没有 `cv::inpaint`**，也没有直方图/边缘算子。

性能基线（旧版文档，Ryzen 9 5950X）：中型矩形 ~0.6 s，768x512 ~2.2 s，长画笔 ~0.8 s；峰值内存 30~160 MiB（单任务，已裁剪）。

## 2. 本仓库现有能力

- `snow-canvas-filters`：滤镜内核（马赛克/模糊/反相/浮雕/灰度/画笔遮罩）已按 C++ 逐字节对拍；`smart_erase.rs` 为未移植桩（`smart_erase(&mut ImageMut, mask) -> bool` 恒 `false`），接口已定。crate 零依赖。
- `snow-canvas-raster`：软件光栅化（图层、装饰层）；可承担“遮罩 → 图层合成”。
- Cargo.lock 已有：`image 0.25.10`、`rayon 1.12.0`、`windows`（0.57/0.58/0.61）；**没有** `imageproc`、`fast_image_resize`、`photon-rs`、`ndarray`、`nalgebra`、`opencv`。
- `snow-crates` 里没有 inpaint / PatchMatch 实现（`grep inpaint` 无结果）；复用点仅 `visual-region-detector`（自动滤镜）。

## 3. 候选方案逐项评估

| 候选 | 许可证 | 在 Cargo.lock | 新增依赖 | 质量 / 性能 | 结论 |
|---|---|---|---|---|---|
| `visual-region-detector`（本仓库 `snow-crates`） | Apache-2.0（兼容 GPL-3.0-only 工作区） | 否（path 依赖，传递仅 `serde`/`serde_json`，需确认锁文件是否已有，[推测] 大概率已有） | 0 个第三方新增（可能 0~2 个 serde 系，需 `cargo tree` 确认） | 与旧版同源代码、有 OpenCV 夹具；与旧版黄金样本可逐区域对拍 | **采用（自动滤镜）** |
| 自写纯 Rust PatchMatch（移植旧版） | 本项目 GPL-3.0-only | — | 0 | 与旧版同算法，可对拍；单线程搜索 + 可选 rayon 投票 | **采用（智能擦除）** |
| `inpaint` crate 0.1.7（Telea） | 未能确认（crates.io 页未取到，需核）[不确定] | 否 | `glam`、`ndarray`、`image-ndarray`、`num-traits`、`thiserror` 等约 5+ | Telea 只擅长小划痕/细线，大块纹理会糊，质量低于旧版 PatchMatch；旧版 `.md` 目标是“平面/重复/渐变背景保真” | 不推荐（备选仅限细笔画，但引入 ndarray 不值） |
| `imageproc`（MIT） | MIT | 否 | 依赖 `image`（已有）、`rayon`（已有）、默认 `rustfft` 等 | 有 `distance_transform`、`euclidean_squared_distance_transform`（[网页]）；无 inpaint；形态学/高斯需另核 | 不引入：原语自写更小，且要带标签的 L2 chamfer-5 对齐 OpenCV |
| `photon-rs` | 未核 | 否 | 多 | 面向滤镜/特效，无 inpaint | 不考虑 |
| `fast_image_resize` | MIT/Apache | 否 | 少 | 仅缩放，不覆盖 AREA/LINEAR 的 OpenCV 取整口径 | 不需要（尺寸变换自写，要与 OpenCV 取整一致） |
| `texture-synthesis`（EmbarkStudios） | 未核 | 否 | 多 | 仓库已归档（[网页]）；随机纹理合成，不是确定性补洞 | 不考虑 |
| `opencv-rust` | Apache-2.0 | 否 | 巨大（需 OpenCV 预编译/clang），违反总原则小体积、少依赖 | 与旧版完全一致 | 按任务要求不考虑 |
| Direct2D / WIC / D3D 着色器 | 系统自带 | `windows` 已有 | 0 | D2D 内置效果含高斯模糊、形态学、边缘检测、直方图，**没有 inpaint / 内容感知填充**（[官方列表](https://learn.microsoft.com/en-us/windows/win32/direct2d/built-in-effects)）；PatchMatch 逐像素随机搜索不适合用 D2D 效果表达，需自写计算着色器，且要 GPU 常驻 | 不用于擦除；可作 P3 的加速选项，非必需 |
| Media Foundation | 系统自带 | — | — | 视频编解码相关，无图像修复 | 无关 |

来源：[imageproc README](https://github.com/image-rs/imageproc)、[imageproc distance_transform](https://docs.rs/imageproc/latest/imageproc/distance_transform/index.html)、[inpaint crate](https://docs.rs/crate/inpaint/latest)（[Codeberg 仓库](https://codeberg.org/gillesvink/inpaint)本次 404，未读到源码）、[texture-synthesis](https://docs.rs/texture-synthesis)。

## 4. 自写纯 Rust 的 OpenCV 原语映射与工作量

原语放进 `snow-canvas-filters` 新模块（或 `snow-canvas-erase` 子目录），要点是与 OpenCV 4.12.0 逐字节/容差一致。可借鉴 `visual-region-detector/src/backend/pure_rust/morph.rs`（432 行，已对拍矩形/椭圆形态学）直接复用或抽公共。

| OpenCV | Rust 自写 | 难度 / 行数估计 |
|---|---|---|
| `getStructuringElement` / `erode` / `dilate` | 复用 `pure_rust/morph.rs`（`BORDER_CONSTANT 0`） | 低，复用 |
| `distanceTransform` L2 mask5 + 标签 | 5x5 chamfer 两遍扫描（a=1,b=1.4,c=2.1969），标签随距离传播 | 中，约 150 行；需对拍 OpenCV |
| `GaussianBlur`（核 2r+1, sigma r/2, float） | 可分离卷积，BORDER_REFLECT_101 | 低，约 80 行 |
| `pyrDown`（5 抽头 1-4-6-4-1/16） | 可分离 + 抽样 | 低，约 60 行 |
| `resize` AREA / LINEAR | 掩码用 AREA（缩小），层间用 LINEAR（OpenCV 半像素中心，边缘钳位） | 中，约 120 行；取整规则要对拍 |
| `cvtColor` RGB↔Lab（float） | OpenCV float 公式（sRGB 伽马 + D65，含 `LabCbrtTab`），不要换自定义公式 | 中，约 100 行 |
| `solve` Cholesky 3x3、`norm`、`threshold`/`compare`/`bitwise`/`countNonZero` | 手写 | 低，约 80 行 |
| `parallel_for_`（投票 2 条带） | `std::thread::scope` 或 `rayon`（已在锁文件） | 低 |
| 主算法 PatchMatch / 表面拟合 / 周期 / 背景引导 | 逐函数直译 | 高，约 800~1000 行 Rust |

工作量估计 [推测]：原语 + 对拍约 3~4 人日；主算法直译 + 单测 4~6 人日；接线（遮罩合成、异步协调器、缓存、占位）3~4 人日。合计约 2 人周。若只做“简单 Telea”（~200 行）约 1 人日，但质量与旧版差距大，不推荐作最终版。

## 5. 脏区协议（ViewportPatch）适配

- **自动滤镜**：产出的是区域列表 + 扫描动画，不改像素；动画/闪烁沿用旧版“只重绘扫描带与闪烁区”的脏矩形（旧版 `updateFrame` 已按脏区更新），与 `ViewportPatch` 协议天然一致。
- **智能擦除**：结果只改动遮罩包围盒（旋转矩形/笔刷路径）内像素，可以把“包围盒 + 笔刷半径”作为一个脏矩形提交；预览/占位同理。后台完成后以代数/几何校验丢弃过期任务（旧版已有此机制，直译即可）。应只在完成时提交一次 patch，不在算法中途刷新。

## 6. 推荐方案与分期

**推荐：全部自有代码，零新增第三方依赖。**

- **P1 自动滤镜接线（约 3~4 人日，先做）**：在 `snow-shot` 增加 `visual-region-detector` path 依赖（默认 feature，不开 `opencv`）；后台线程调 `detect_regions`，7 类映射与旧版 `kCategories` 对齐；接 `AutoFilterRequest`；同时可顺带解 D05 里“未接 detector”的智能选区缺口。
- **P2 智能擦除原语与快路径（约 1 人周）**：补齐 §4 原语 + 对拍；实现 `surfaceFill`、`periodicFill`、占位降级（`smart_erase` 在无解时仍可返回 `false`，UI 保持红色占位，符合 §3 能力降级）。覆盖平面、渐变、重复背景三类最常见截图场景，这些旧版也走快路径（毫秒级）。
- **P3 PatchMatch 全量（约 1 人周）**：Lab 金字塔、背景引导、投票；先串行，达标后可选 rayon 两条带投票（旧版实测 -30%~-40% 的投票阶段）。
- **P4（可选）**：D2D/着色器加速，仅当真机 benchmark 显示不够时再议，需单独评估 GPU 常驻代价。

**备选**：① 只做 P1 + P2，大块纹理孔洞继续占位（最小风险、最小体积）；② 引入 `inpaint`（Telea）补细笔画，需用户批准依赖并先核许可证，不推荐。

## 7. 需要用户批准的依赖

- 推荐方案：**无新增第三方依赖**。
- 仅需确认：`snow-shot` 新增 path 依赖 `visual-region-detector`（Apache-2.0，仓库内）。其传递的 `serde`/`serde_json` 是否已在 `Cargo.lock` 需 `cargo tree -p visual-region-detector` 确认；若出现新包再报批。按 AGENTS.md，新依赖落地后须更新第三方许可证清单。
- 若走备选 ②：`inpaint`（许可证待核）、`ndarray` 等。

## 8. 对旧版黄金样本的对照测试

遵循 AGENTS.md “每个移植模块要有对照旧版 C++ 的黄金测试”：

1. **自动滤镜**：用旧版 C++ 构建对一组固定截图（文字、聊天气泡、图片、头像、图标、纯色）跑 `snow_detect_visual_regions`，导出 `[(x,y,w,h,category)]` 为 JSON 夹具；Rust 侧同图断言区域集合相同（允许顺序无关、矩形逐值相等）。`visual-region-detector` 的 `tests/contracts.rs` 已有契约测试可参考。
2. **原语**：对每个原语（distanceTransform+标签、GaussianBlur、pyrDown、resize、cvtColor Lab）用 OpenCV 4.12.0 导出小输入/输出夹具（同 `opencv_fixtures.json` 做法），Rust 逐值或容差 ≤1 对拍。
3. **快路径**：平面/仿射渐变/周期背景，旧版文档说明周期输出**字节相同**，Rust 要求逐字节一致。
4. **PatchMatch**：算法确定性（固定候选顺序与种子），在固定夹具（旧版 `snow_canvas_smart_erase_quality_tests.cpp` 的 13 个场景）上，先导出旧版结果 PNG，再用旧版文档的质量门限比对：以 sigma=2 模糊后 Lab ΔE76 在遮罩内**均值 ≤1.5、p95 ≤4**；高频能量比 85%~115%；遮罩外像素必须完全不变；Alpha 保留。注意：若移植后逐像素不等，允许按此门限而非逐字节判通过，但需记录差异原因（浮点累加顺序、随机数发生器差异；旧版用 `std::mt19937` [推测]，需核对种子序列方可逐字节）。
5. **行为测试**：取消、占位降级、画笔跨度、负坐标、旋转矩形、DPI 缩放映射，沿用旧版 `snow_canvas_smart_erase_tests.cpp` 的用例清单改写为 Rust 离屏测试。
6. 性能回归：用旧版基准的 13 个夹具比较中位耗时与峰值内存，门限参照旧版文档（相对回归 ≤10% 或 2 ms；内存 ≤+10%）。注意记录机器/GPU/驱动，不同机器数据不可直接对比（AGENTS.md）。

## 9. 不确定处

- `inpaint` 与 `photon-rs` / `texture-synthesis` 的许可证未取到页面原文，未下结论。
- `visual-region-detector` 传递依赖是否会给 `Cargo.lock` 带来新包、对 Windows 二进制体积的具体影响，未实测。
- 旧版 PatchMatch 随机数细节（是否 `std::mt19937`、种子）未逐行核对，影响“逐字节对拍”能否达成。
- 工作量均为 [推测] 估算。
