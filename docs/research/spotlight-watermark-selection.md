# 聚光灯 / 水印：选型调研

> 状态：已按本方案实施（2026-10-08，`snow-canvas-raster::decoration`），UI 入口与黄金对照未做，见审计表 D08。日期 2026-10-08。依据 [`principles.md`](../principles.md) 的优先级链（高性能 > 低内存 > 高 fps > 少编译依赖 > 多用系统能力）。
> 关联：[`qt-parity-audit.md`](qt-parity-audit.md) D08、[`cisox-progress-handoff.md`](../cisox-progress-handoff.md)（"聚光灯 / 水印要先让 `snow-canvas-raster` 会画图层级配置"）。

## 1. 结论（先看这里）

**0 新增依赖，复用 `tiny-skia`（已在 `snow-canvas-raster`）+ `snow-platform::text_raster`（GDI 文字覆盖率位图），自写约 300 行。** 没有哪个开源 crate 同时给出"脏区增量 + 聚光灯挖洞 + 平铺旋转水印"，引入任何一个都比自写更重，且与第一原则冲突。需用户批准的依赖：**无**。

## 2. 要达到的行为（旧版 Qt，只读）

渲染代码：`snow_draw_engine_qt/src/rendering/snow_canvas_spotlight_renderer.cpp`（157 行）、`snow_canvas_watermark_renderer.cpp`（955 行，大头是缓存与碎片优化）、合成顺序见 `snow_canvas_compositor.cpp:56-76`。

| 项 | 聚光灯 | 水印 |
| --- | --- | --- |
| 参数 | `color`、`opacity`（默认 0.64，黑色）、`active`；挖洞列表 `DisplaySpotlightCutout{center,width,height,rotation}`（旋转矩形） | `color`、`text`（≤256 字节）、`font_size`（6~512）、`font_family`、`angle`（-90~90，默认 30）、`gap`（10~200，默认 56）、`opacity`（0~1，默认 0.16）；文档层另有 `template_value` / `template_application_time`（文本模板，引擎已处理） |
| 渲染 | 整个渲染区填 `color*opacity`，减去所有洞（Winding 并集，抗锯齿），洞内不变暗；无洞时整块快路径 | 文字排一次得"墨迹包围盒"，步长 = 包围盒宽/高 + gap，绕锚点（渲染区中心）旋转 `angle` 后无限平铺；有效 alpha = `color.a * opacity / 255`，< 0.004 或文本为空直接跳过 |
| 层级 | 在标注图元**之上**，水印在聚光灯之上；两者都是"装饰层"，不进文档图元 | 同左 |
| 与导出 | 导出（`WatermarkRenderPurpose::ImageExport`）与预览同一渲染器，导出图带聚光灯与水印 | 同左 |
| Qt 侧优化（要不要学） | 视口外洞剔除、零洞快路径 | 重复单元缓存（≤8 项 / 16 MB，单元 ≤4096 边 / 4 MB）、稀疏碎片模式 |

引擎协议：`ViewportPatch.decoration: DecorationPatch { base_revision, revision, reset, view: {watermark, spotlight}, spotlight_ops: Vec<ReplaceRangeOp<Cutout>>, dirty_regions }`（`snow_draw_engine_qt/crates/snow-draw-engine-display/src/lib.rs:847`）。

## 3. 现有能力盘点

- `snow-canvas-raster`（`tiny-skia 0.11.4`）：分块 256px、`ViewportPatch` 脏区、蒙版、`Pixmap`、`BlendMode::{SourceOver,Multiply}`、`Pattern` 已在 `draw.rs` 用于斜线纹理。**装饰层不画**：`rasterizer.rs:174` 只读 `decoration.revision`，`view`、`spotlight_ops`、`dirty_regions` 未消费；`DeferredKind` 只有 Text / Filter。
- `snow-canvas-text`：只有文字布局 / 光标 / 命中，**不出像素**，不能直接画水印。
- 文字像素来自 `snow-platform::text_raster::rasterize_text(text, family, px, bold) -> TextBitmap`（Windows GDI，覆盖率位图，0 依赖；`snow-shot/src/annotation.rs:378 draw_text` 已按 `(文本, 字号, 字体族)` 缓存复用）。符合"多用系统自带能力"。
- 图层混合：`annotation.rs` 的 `finish_layer` 已有 `source_over` 与脏块合成，滤镜 / 文字走这里；导出走同一套。

结论：缺的只是"装饰层 pass"，不是通用图层引擎。

## 4. 开源方案评估

许可证对照：工作区为 GPL-3.0-only；MIT、Apache-2.0、BSD-3-Clause、MPL-2.0 均可并入（Apache-2.0 与 GPLv3 兼容；MPL-2.0 允许以 GPL 为次级许可，但保留文件级义务）。"在 Cargo.lock"= 已查 `snow-shot-rs/Cargo.lock`。

| 候选 | 许可证 | 在 Cargo.lock | 能做什么 | 评价 |
| --- | --- | --- | --- | --- |
| [tiny-skia](https://docs.rs/tiny-skia/) 0.11.4 | BSD-3-Clause | 是，且 `snow-canvas-raster` 直接依赖 | `Pixmap` / `Mask` / `Pattern`（[`SpreadMode::Repeat` + `Transform`](https://docs.rs/tiny-skia/latest/tiny_skia/struct.Pattern.html)）/ [`BlendMode::DestinationOut` 等](https://openrr.github.io/openrr/tiny_skia/enum.BlendMode.html)，抗锯齿路径填充，[`Pixmap::draw_pixmap`](https://docs.rs/tiny-skia/latest/tiny_skia/struct.Pixmap.html) | **首选**。两个功能正好是它的原生能力，且天然落在现有脏块循环里 |
| `image` 0.25.10 | MIT / Apache-2.0 | 是（`snow-shot` 仅用其编解码） | `imageops::overlay`、旋转 | 旋转 / 平铺无硬件友好路径，要额外缓冲；不如 tiny-skia |
| [imageproc](https://docs.rs/imageproc/latest/imageproc/) | MIT | **否** | `draw_text_mut`（要字体文件 + `ab_glyph`）、[`rotate_about_center`](https://docs.rs/imageproc/latest/imageproc/geometric_transformations/index.html) | 新增依赖 + 要自带字体，丢掉系统字体 / CJK 回退；否决 |
| [ab_glyph](https://crates.io/crates/ab_glyph) | Apache-2.0 | 否（待核：可能随 gpui 传递，未查） | 字形光栅化 | 要自管字体文件与回退，GDI 已够用；否决 |
| cosmic-text 0.19 / `fontdb` / `rustybuzz` / `skrifa` / `swash` | MIT / Apache-2.0 为主 | 是，但只在 `vendor/gpui-*` 里 | 完整排版 + 光栅 | `workspace-guard` 禁止在 `snow-ui-shell` 外引 gpui 系依赖；直接依赖 = 新增显式依赖，且与 GDI 渲染的文字外观不一致；否决 |
| resvg 0.45 / 0.46（`usvg`） | 待核（上游标 MPL-2.0） | 是（图标用） | 把水印拼成 SVG 渲染 | 为一块平铺文字拉起整套 SVG + 文字管线，慢且重；否决 |
| [fast_image_resize](https://crates.io/crates/fast_image_resize) | MIT / Apache-2.0 | 否 | SIMD 缩放 | 不缩放大图，不需要；否决 |
| 其他截图工具 | - | - | [ShareX 的 Spotlight](https://getsharex.com/docs/image-editor.html)（压暗所选区域外，另见其[多区域需求 #8438](https://github.com/sharex/sharex/issues/8438)）；[Flameshot 无水印](https://github.com/flameshot-org/flameshot/issues/158)（仅功能请求） | ShareX 为 C#/GDI+，Flameshot 为 Qt；没有可复用的 Rust 代码，只做行为参照 |

没有找到现成"平铺文字水印"或"暗化挖洞"的专用 Rust crate；两者的核心算法各十几行，自写比引入新依赖划算。

## 5. 推荐方案

放在 `snow-canvas-raster` 新增 `decoration.rs`（不引新依赖、不碰 gpui），对外由 `snow-shot/src/annotation.rs` 在 `finish_layer` 之后、导出合成之前调用同一函数，保证预览与导出一致。

### 聚光灯

1. 状态：在光栅化器里维护 `Vec<Cutout>`（按 `spotlight_ops` 的 `ReplaceRangeOp` 应用，`reset` 清空）与当前 `DisplaySpotlightConfig`。
2. 每个脏块（落在 `dirty_regions` 内）：取一块复用的块大小 `Pixmap` 作草稿；整块填 `color * opacity`（预乘）；对相交的洞逐个 `fill_path` + `BlendMode::DestinationOut`（旋转矩形路径，抗锯齿，即并集语义）；再 `SourceOver` 叠到该块的标注层上。
3. 优化（学 Qt）：先用洞的旋转包围盒剔除；无洞走整块纯色快路径；`active=false` 或 `opacity<=0` 早退。
4. 不能直接用 `DestinationOut` 打在标注层上：会把洞内的标注一起擦掉，必须走草稿块。

### 水印

1. 按 `(text, family, px, bold)` 调 `rasterize_text` 取覆盖率位图（沿用 `annotation.rs` 的文字缓存），像素字号 = `font_size * dpr * zoom`，量化到 1/64 以防缩放时反复重建。
2. 构造"重复单元"`Pixmap`：尺寸 = 墨迹宽高 + gap，把覆盖率乘 `color.rgb` 与 `color.a*opacity` 写成预乘 RGBA；只缓存最近 1~2 份，上限仿 Qt（单元 ≤4096 边、≤4 MB，超限降级为不画并 `tracing::warn`，不崩）。
3. 每个脏矩形一次 `fill_rect`：`Pattern::new(cell, SpreadMode::Repeat, FilterQuality::Bilinear, 1.0, transform)`，其中 `transform = translate(锚点) · rotate(angle) · scale(zoom)`（锚点为渲染区中心，与 Qt 一致）。只触及脏矩形，天然符合 `ViewportPatch` 协议。
4. 文字模板（`template_value` / 应用时间）已在引擎文档层展开，光栅层只拿最终 `text`。

### 与 ViewportPatch 的衔接

- 装饰层 `decoration.revision` 变化时，只对 `decoration.dirty_regions` 重画；`reset` 时整屏。
- 输出仍是预乘 RGBA 脏块，上层上传逻辑不变；被装饰层覆盖的块要进 `RasterOutput.tiles`。
- 聚光灯在标注之上、水印在聚光灯之上（与 Qt 合成顺序一致）。

### 内存 / 性能预期

聚光灯草稿块 256×256×4 = 256 KB 常驻 1 份；水印单元 ≤4 MB；无新增常驻线程。每帧只动脏块，复杂度 ≈ 脏块数 × (洞数 + 1 次 Pattern 填充)。基线对照用 Qt 现成基准：`snow_draw_engine_qt/tests/snow_canvas_spotlight_benchmark.*`、`snow_canvas_watermark_benchmark.*`（1080p / 4K、DPR 1 / 1.25 / 2、1 / 16 / 128 个洞），Rust 侧用同一矩阵比 p50 / p95，不跨 GPU 直比。

## 6. 实施步骤

1. 在 `snow-canvas-raster` 加 `decoration.rs`：`SpotlightLayer`（洞列表 + 配置 + 渲染）、`WatermarkLayer`（单元缓存 + 渲染）；`TinySkiaRasterizer::apply_inner` 消费 `patch.decoration`。
2. `snow-shot/src/annotation.rs`：把水印 / 聚光灯的文字位图通过现有 `text_raster` 注入（`snow-canvas-raster` 不依赖 `snow-platform`，用回调或预先传入覆盖率位图）；导出路径调用同一合成函数。
3. 标注工具栏接入聚光灯工具与水印设置面板（用 `gpui-component` 的 `Select`、颜色、滑块；文案进 `.ftl`，`en-US` + `zh-CN`）。
4. 兼容旧配置：`drawing/watermark_style`、`drawing/spotlight_style` 的范围夹取（`screenshotcanvastoolstyles.cpp:430-434`）。
5. 单测 + 黄金对照（见 §7）；更新 handoff / 审计表 D08 后再提交。

## 7. 测试与旧版黄金样本

沿用 `snow-canvas-filters/tests/golden.rs` 的做法：固定种子输入，MSVC 编译的 C++ 渲染器输出存为黄金文件（`tools/p1-reference-baselines/`），Rust 逐像素比对。

- **聚光灯**：C++ 侧基于 `snow_canvas_spotlight_render_tests.cpp` 的构造方式，输入 1~3 个旋转矩形洞 + 颜色 / opacity，输出 RGBA。Rust 比对：洞外、洞内像素必须**逐字节一致**；洞边缘抗锯齿两家栅格器不同（QPainter vs tiny-skia），按边缘带容差（建议每通道 ≤2~3，且带外比例 ≤0.5%，数值先实测再定）。
- **水印**：文字来源不同（Qt 字形 vs GDI）无法逐字节；黄金改比**几何**：墨迹包围盒、平铺步长、旋转后单元中心坐标（亚像素容差），以及整体平均 alpha / 总覆盖量（相对误差阈值）。文字用固定字体（如 Microsoft YaHei UI）避免回退差异。
- 纯 Rust 确定性单测（离屏）：`active=false` / opacity=0 / 空文本早退为全透明；洞内标注像素保留；增量脏区重画结果与整屏重画逐字节相同（`ViewportPatch` 协议核心不变量）；`reset` 清洁；超限单元降级不 panic。
- 命令（在 `snow-shot-rs/` 内）：`cargo test -p snow-canvas-raster`、`cargo test -p snow-shot annotation`、`cargo clippy -p snow-canvas-raster --all-targets -- -D warnings`、`cargo test -p workspace-guard`、`cargo fmt --all -- --check`。

## 8. 风险与待确认

- GDI 文字与 Qt 字形外观不同，水印不可能与旧版逐像素一致，验收只对几何与总量；如需完全一致需自带字体，与第一原则冲突，不建议。
- 旋转平铺用 `FilterQuality::Bilinear` 会轻微发虚；若观感不足可对单元提高到 Bicubic（`tiny-skia` 内置），无需新依赖。
- 草稿块与 `ViewportPatch` 边界对齐、DPR 分数缩放下的接缝，需用 125% / 150% DPR 补专项测试。
- 表中标"待核"项（resvg 许可证、`ab_glyph` 是否已被 gpui 传递引入）未逐项核实，选型不依赖它们。
