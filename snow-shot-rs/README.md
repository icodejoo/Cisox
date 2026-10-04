# snow-shot-rs

Cisox 的 Rust + GPUI 新 workspace，与 Qt 版并行存在。方案见 `../docs/cisox-gpui-migration-plan.md`。

## 构建与测试

```
cargo check --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all -- --check
cargo test --workspace
```

工具链由仓库根 `rust-toolchain.toml` 固定（Rust 1.97.1）。

## 目录与后续任务对应

`snow-shot` 新增模块：
- `window_pick.rs` — 截图选区智能窗口识别（悬停高亮、单击选中、手动框选；设置项"智能选择"，默认开）
- `annotation_style.rs` — 标注样式面板（颜色、线宽、字号、填充、箭头头型；按工具记忆）

| crate | 职责 | 阶段 / 填充任务 |
|---|---|---|
| `snow-shot` | bin 入口、子模式分发、单实例 | P1 单实例与入口 |
| `snow-app-core` | 产品常量、命令总线、会话编排；已 path 依赖 `snow-draw-engine-core` 验证连通 | P1 命令总线 |
| `snow-capability` | 平台能力注册表 | P1 能力注册表 |
| `snow-ui` | GPUI 视图层总入口 | P1 起 |
| `snow-ui/snow-ui-theme` | 设计令牌 | P1 主题 |
| `snow-ui/snow-ui-icons` | 图标模型与资源 | P1 图标 |
| `snow-ui/snow-ui-widgets` | gpui-kit 缺口组件 | P3 |
| `snow-ui/snow-ui-shell` | GPUI 隔离层，`gpui::` 只允许出现在这里 | P1 起 |
| `snow-canvas-raster` / `-filters` / `-text` | 画布光栅化 / 滤镜 / 文本 | P2 |
| `snow-translate` | 本地 NMT | P5 |
| `snow-i18n` | Fluent 运行时与提取 | P1 i18n |
| `snow-config` | 配置读写 | P1 配置层 |
| `snow-history` | 历史与贴图仓储 | P3 / P4 |
| `snow-net` | HTTP 与云端 AI | P5 |
| `snow-update` | 更新（T4 验收前禁用） | P7 |
| `snow-mcp` | 进程内 MCP | P7 |
| `snow-platform` | 原生平台调用 | P1 起 |
| `tools/workspace-guard` | 守卫：shell 之外出现 `gpui::` 或 gpui 依赖即测试失败 | P1 |

日志与崩溃转储、CI 属 P1 后续任务，尚未建 crate / 流水线。

## 新增依赖与功能

- `snow-shot` 依赖 `snow-crates` 中的 `snow-ui-selector`（path，仅 Windows，Apache-2.0 许可，传递 `rstar`、`crossbeam-channel`）用于窗口识别。
- `snow-ui-shell` 的 `windows` crate 新增 feature：`Win32_Graphics_Dwm`、`Win32_System_LibraryLoader`（标题栏深浅色、弹出菜单主题接口）。
- i18n 新增 `.ftl` 文件：
  - `locales/en-US/tray.ftl` / `locales/zh-CN/tray.ftl` — 托盘菜单文本（支持中英文并随语言设置即时刷新）
  - `locales/en-US/annotation_style.ftl` / `locales/zh-CN/annotation_style.ftl` — 标注样式面板文本

## 功能现状快照

当前已落地功能：
- **截图选区**：智能窗口识别（悬停高亮顶层窗口、单击选中、拖动转手动框选；设置项默认开启），控件级识别未做。
- **标注工具**：新增样式面板（颜色、线宽、字号、填充、箭头头型，按工具记忆）与荧光笔、序号球。
- **听写（实时语音转文字）**：可用（浮窗输出；键入到其它应用的真机验证未完成）。
- **托盘菜单**：支持中英文并随语言设置即时刷新。
- **深浅主题**：设置窗口标题栏与托盘菜单跟随 Windows 深浅主题。

## 尚未完成

缺口清单（简述）：7 个全局热键动作仍为占位（直接截图类已接线，无真机验证）、贴图管理页、OCR 结果窗、二维码、快捷键录入控件。MCP、更新、网络 crate 仅占位。macOS/Linux 推迟（ADR-7）。详见 `../docs/cisox-progress-handoff.md` 的「迁移差距盘点」。

## 可选下载的翻译模型与授权声明 / Optional translation models and license notice

截图翻译的本地模型**不随安装包提供**，需要时由用户在应用内按需下载（或自行用仓库里的脚本生成）。Release 中提供的翻译模型文件由 Meta FAIR 的 NLLB-200-distilled-600M 衍生而来（词表裁剪到所选语言集、4 位权重量化），**沿用其 CC-BY-NC-4.0 许可，仅限非商业使用，不用于生产部署**。这些文件不属于本仓库 GPL-3.0 许可的范围。是否使用、如何评估由此带来的授权与质量风险，请用户自行斟酌，作者不提供任何担保。

The local translation models for screenshot translation are **not bundled** with the installer; users download them on demand (or generate them with the scripts in this repository). The model files published in releases are derived from Meta FAIR's NLLB-200-distilled-600M (vocabulary pruned to the selected languages, 4-bit weight quantization) and keep its **CC-BY-NC-4.0 license: non-commercial use only, not intended for production deployment**. They are not covered by this repository's GPL-3.0 license. Users are responsible for assessing the licensing and quality risks of using them; no warranty is provided.

- 来源 / Source: https://huggingface.co/facebook/nllb-200-distilled-600M ，论文 NLLB Team et al., "No Language Left Behind: Scaling Human-Centered Machine Translation", arXiv:2207.04672
- 许可 / License: https://creativecommons.org/licenses/by-nc/4.0/
- 修改说明 / Changes: 词表裁剪、权重量化为 int4（详见 `docs/guides/translation-model-release.md`）/ vocabulary pruning and int4 weight quantization (see `docs/guides/translation-model-release.md`). 与 Meta 无隶属关系，未获其背书 / Not affiliated with or endorsed by Meta.
