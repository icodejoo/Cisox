---
title: 本机 Rust 编译提速：实测数据与建议
status: active
updated: 2026-10-07
summary: 链接器、Cranelift、拆工作区、nextest、sccache、构建目录所在磁盘各自的实测效果，以及哪些值得采纳。
---

## TL;DR

- **最大的收益是把构建目录放到 `C:` 盘**：同一份代码的增量构建，`E:` 盘上 14~89 秒且极不稳定，`C:` 盘上稳定 **约 8 秒**。原因是 `E:` 盘上写大文件慢约 40 倍（写 49 MB：`E:` 约 2 秒、首次 16 秒，`C:` 约 0.05 秒），疑似终端安全代理（`saio_*` 等）的文件过滤驱动，两个分区在同一块 SSD 上，不是硬件差异。
- **rust-lld 已在用**（`.cargo/config.toml`），无需再改。
- **sccache 在 `C:` 盘上没有可见开销**（增量构建 9.1/8.1 秒 vs 绕过后 8.1/7.9 秒）；之前观察到的“sccache 让增量构建慢 50 秒”其实是 `E:` 盘 I/O 的现象。
- **拆分超大模块无收益**：把 `overlay_view.rs`（6381 行）拆成 11 个子模块后，改一个函数体的增量构建仍是 8.2 秒，与不拆相同，所以没有采纳（已还原）。
- **nextest 在这里更慢**：762 个测试，`cargo nextest run` 约 30 秒，`cargo test` 约 15 秒（Windows 上每个测试一个进程的启动开销）。不采纳。
- **Cranelift 不可行**：只在 nightly 可用，而项目固定 stable 1.97.1，且依赖里有对内联汇编 / SIMD 敏感的代码。

## 怎么用（推荐的本机设置）

把构建目录指到 `C:` 盘，仅影响本机，不改仓库配置：

```powershell
# 当前会话
$env:CARGO_TARGET_DIR = "C:\cargo-target\cisox"
# 永久（用户级环境变量，对所有 Rust 项目生效；只想影响本项目就用上面的会话变量或 just 配方）
setx CARGO_TARGET_DIR "C:\cargo-target\cisox"
```

- 仓库的 `.cargo/config.toml` 里是 `target-dir = "build/cargo"`；环境变量优先级更高，不需要改仓库。
- 文档和脚本里写的 `build\cargo\debug\snow-shot.exe` 等路径，在设置了环境变量后要换成 `C:\cargo-target\cisox\debug\...`。
- 冷构建一次约 7 分钟（依赖大部分走 sccache 命中），之后增量构建约 8 秒；目录约几 GB，可随时删除。

## 实测数据

测量机器：13th Gen Intel i5-13500（14 核 20 线程），Kingston SKC600 512G，Windows 10；改一个函数体后 `cargo build -p snow-shot --tests`，墙钟秒数。

| 场景 | 结果（秒） | 说明 |
|---|---|---|
| `E:` 盘 / 默认（含 sccache） | 14、80、79、89 | 同一配置波动极大 |
| `E:` 盘 / 绕过 sccache | 22、23、37 | 仍不稳定 |
| `C:` 盘 / 旧布局 / 绕过 sccache | 9.2、8.1、7.9、8.3、8.3 | 稳定 |
| `C:` 盘 / 旧布局 / 默认 sccache | 9.1、8.1、8.2、8.2 | 与绕过无差别 |
| `C:` 盘 / `overlay_view.rs` 拆 11 个子模块 | 8.2、8.2、8.1、8.1、8.3 | 与旧布局无差别 |
| `cargo check -p snow-shot`（仅前端） | 约 4 | 前端占一半，其余是代码生成与链接 |
| `cargo test -p snow-shot`（762 个测试） | 约 15（测试本身 13） | |
| `cargo nextest run -p snow-shot` | 约 30 | 慢一倍 |

## 后续还可以做的

- 增量构建里 8 秒中前端约 4 秒、代码生成 + 链接约 4 秒，再往下压要么减少 `snow-shot` 的体量（抽稳定的叶子 crate，例如 stitch / export / annotation 约 5000 行），要么减轻链接（49 MB 的调试 exe）。这些都没有实测收益，需要时再单独评估。
- 若要查清 `E:` 盘为什么慢：对比 `C:` 与 `E:` 上的文件过滤驱动（`fltmc`）与终端安全代理对 `E:\workspaces` 的监控范围，让 IT 或安全软件给构建目录加例外。
