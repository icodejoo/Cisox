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
