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

## 可选下载的翻译模型与授权声明 / Optional translation models and license notice

截图翻译的本地模型**不随安装包提供**，需要时由用户在应用内按需下载（或自行用仓库里的脚本生成）。Release 中提供的翻译模型文件由 Meta FAIR 的 NLLB-200-distilled-600M 衍生而来（词表裁剪到所选语言集、4 位权重量化），**沿用其 CC-BY-NC-4.0 许可，仅限非商业使用，不用于生产部署**。这些文件不属于本仓库 GPL-3.0 许可的范围。是否使用、如何评估由此带来的授权与质量风险，请用户自行斟酌，作者不提供任何担保。

The local translation models for screenshot translation are **not bundled** with the installer; users download them on demand (or generate them with the scripts in this repository). The model files published in releases are derived from Meta FAIR's NLLB-200-distilled-600M (vocabulary pruned to the selected languages, 4-bit weight quantization) and keep its **CC-BY-NC-4.0 license: non-commercial use only, not intended for production deployment**. They are not covered by this repository's GPL-3.0 license. Users are responsible for assessing the licensing and quality risks of using them; no warranty is provided.

- 来源 / Source: https://huggingface.co/facebook/nllb-200-distilled-600M ，论文 NLLB Team et al., "No Language Left Behind: Scaling Human-Centered Machine Translation", arXiv:2207.04672
- 许可 / License: https://creativecommons.org/licenses/by-nc/4.0/
- 修改说明 / Changes: 词表裁剪、权重量化为 int4（详见 `docs/guides/translation-model-release.md`）/ vocabulary pruning and int4 weight quantization (see `docs/guides/translation-model-release.md`). 与 Meta 无隶属关系，未获其背书 / Not affiliated with or endorsed by Meta.
