# MCP 子系统设计（A12）

> 状态：**第一期（M0 骨架）已实现（2026-10-08）**，M1 起待做。实现细节与偏差见文末 §8。原为草案阶段的目标：给后续落地定边界，免得做到一半返工。
> 依据：总原则见 [../principles.md](../principles.md)（高性能 > 低内存 > 高 fps > 少编译依赖 > 多用系统能力）；审计行见 [../research/qt-parity-audit.md](../research/qt-parity-audit.md) A11 / A12。

## 1. 现状与范围

- Rust 侧 `snow-mcp` 只有 `PHASE` 常量；`mcp/enabled` 配置键无人消费；`snow-app-core` 的 `MCP_TOOL_MAP` 只是 28 个 tool 名到 `CommandKind` 的静态表。命令总线只通了 5 个命令（A11）。
- 旧版（Qt）实现约 7.2k 行，位于 `snow_shot/src/app/mcp/*`，能力清单在 `snow_shot/mcp-capabilities.json`。**旧目录不改**，只当黄金样本。
- 本文不写代码，只定：传输、鉴权、四个域、分期、对照入口。

## 2. 传输：本地命名管道

- 服务端监听 Windows 命名管道 `\.\pipe\cisox-mcp-<用户SID>`，ACL 只放当前用户；不开 TCP 端口，不依赖 HTTP 栈（少编译依赖、少常驻）。
- 对外给 MCP 客户端的是一个很薄的 stdio 桥接进程（`snow-mcp-bridge`，独立小 exe）：客户端起它，它把 stdio 的 JSON-RPC 转发到命名管道。这与旧版 "stdio with private same-user IPC" 的形状一致，也让主程序不必兼任 stdio 子进程。
- 协议版本沿用旧清单的 `2026-07-28`；握手时服务端回报版本，不一致就拒绝并给出可读原因。
- 服务默认**不启动**。`mcp/enabled` 为真才监听；关闭时立刻断开所有连接并删除描述符文件，不留后台线程（不常驻）。
- 主程序进程内只做“收请求 → 投命令总线 → 回结果”，重活（截图渲染、录制）仍走既有 worker，MCP 层不持有大块图像内存；图像类结果用临时文件路径或 `resource` 引用，不内联 base64 超过阈值。

## 3. 鉴权：令牌

- 启用时生成 32 字节随机令牌，写入 `<数据根>/mcp/descriptor.json`（管道名、协议版本、令牌、pid），文件权限仅当前用户可读。桥接进程读这个文件。
- 每个连接的首条消息必须带令牌，常量时间比较；错误即断开，不回显细节。连续失败做指数退避。
- 令牌每次启用重新生成，关闭即作废；设置页提供“重置令牌”。令牌不进日志、不进配置文件、不进崩溃报告。
- 破坏性能力（写设置、删文档、开始录制）分域授权：描述符里带 scope 列表，沿用旧清单的 `scope` 概念；默认只开只读与截图域，其余需用户在设置里勾选。

## 4. 四个域

旧清单 `mcp-capabilities.json` 共 4 域、101 个 tool（28 + 27 + 33 + 13）。新版沿用 4 域的切法，名称按用户语言归并为：

| 新域 | 旧域键 | tool 数 | 内容 | 落点 |
|---|---|---|---|---|
| 截图 | `screenshot` | 28 | 开始截图、选区、工具、标注、撤销重做、渲染、导出、滚动、取消等 | 命令总线，复用 `MCP_TOOL_MAP` |
| 应用 | `application` | 27 | 状态、设置读写与动作、托盘、快捷键、更新（受 `updates/manifest_url` 约束） | 配置层 + 设置动作表 |
| 文档 | `documents_jobs` | 33 | 文档状态、异步 job 的创建 / 查询 / 取消、历史 | 历史库（`snow-history`）+ job 注册表 |
| 媒体 | `recording_pinned` | 13 | 录制状态与控制、贴图管理 | 录制 worker 协议 + `pinned_manager` |

约定：所有 tool 名保持旧版 `snow_shot_` 前缀不变，便于现有 agent 配置无缝迁移；长任务一律异步 job（创建返回 job id，`job_get` / `job_cancel` 管理），不在管道上阻塞。

## 5. 分期建议

1. **M0 骨架**：管道服务 + 描述符 + 令牌 + 握手 + `mcp_status`、`app_status`；离屏测试覆盖鉴权失败、协议不一致、关闭即断开。
2. **M1 截图域**：把 `MCP_TOOL_MAP` 的 28 项逐个接到命令总线（先补总线上缺的命令，A11）；每个 tool 一条与旧版输出对比的黄金样本测试。
3. **M2 应用域 + 文档域**：设置读写走已有 schema 校验；job 注册表单独成模块，可被其他异步功能复用。
4. **M3 媒体域**：录制与贴图；依赖录制验收结论，放最后。
5. **M4 桥接进程与发布**：`snow-mcp-bridge` 打包、设置页“复制客户端配置”按钮。

每期的验收口径：与旧版同名 tool 的输入输出 schema 对齐，差异必须在文档里登记。

## 6. 与旧版 101 个 tool 的对照入口

- 能力清单（权威）：`snow_shot/mcp-capabilities.json` 的 `domains.*.tools`。
- 旧实现：`snow_shot/src/app/mcp/`（`screenshotmcpserver.cpp` 分发；`mcpapplicationservice.cpp`、`mcpdocumentservice.cpp`、`mcpmediaservice.cpp`、`mcpjobregistry.cpp` 各域服务）。
- 新版现有骨架：`snow-shot-rs/crates/snow-app-core/src/command.rs` 的 `MCP_TOOL_MAP`（仅截图域 28 项）。
- 建议在 M0 落一个对照测试：读 `mcp-capabilities.json`，断言 Rust 侧已登记的 tool 名是它的子集，并统计剩余数量，随期数递减到 0。

## 7. 待用户拍板

- 桥接进程是否接受多一个小 exe（替代方案：主程序自身以 `--mcp-stdio` 参数兼任桥接，省一个文件，但会让主程序入口变复杂）。
- 默认授权域（本文建议只读 + 截图）。
- 是否要保留旧版“远程 / HTTP”形态：本文认为不需要（只有本机同用户）。

## 8. 第一期（M0）实现状态与偏差（2026-10-08）

已落地：`snow-mcp` 的 JSON-RPC 2.0 骨架（`initialize` / `ping` / `tools/list` / `tools/call`）、令牌鉴权、数据驱动注册表（旧清单 101 个 tool 全部登记，与 `mcp-capabilities.json` 逐域逐项对照测试）、`snow-platform` 的 `line_pipe`（按行收发、当前用户 ACL、首实例防抢占、复用单实例的重叠 IO）与 `random`（系统 RNG）、`snow-shot` 的 `mcp_host`（`mcp/enabled` 打开才启动，关闭即断开并删描述符）。

已实现 5 个 tool（共 101）：`snow_shot_mcp_status`、`snow_shot_app_status`、`snow_shot_settings_get`、`snow_shot_permissions_get`、`snow_shot_updates_status`；其余 96 个调用返回 `isError: true` 的结构化结果（`error.code = "not_implemented"`，带 `tool` / `domain` / `milestone`）。

与本文前文的差异：

- **鉴权握手**：连接的第一行必须是 `cisox/auth`（`params.token`），失败统一回 `-32001 unauthorized` 并断开，失败按 50ms 起指数退避（上限 5s）。握手本身由未来的桥接进程发送，MCP 客户端不可见。
- **授权域**：固定 `read_only` + `capture`（默认值），暂无设置项可勾选其余域；控制类 tool（写设置、删数据、录制等）被拒（`-32003`）。scope 由 tool 名后缀推导，表在 `registry.rs`。
- **传输**：同一时刻只服务一个连接（单管道实例，空闲 5 分钟断开）；桥接进程 `snow-mcp-bridge` 属 M4，尚未做。
- **描述符**：写在 `<数据根>/mcp/descriptor.json`，依赖数据根目录自身的用户私有 ACL，未单独收紧文件 ACL。
- **入参 schema**：旧清单没有逐 tool 的 schema，只有名称；未实现的 tool 用开放的 `{"type":"object"}`，已实现的写了严格 schema，校验器是自带的极简子集（`schema.rs`）。`settings_get` 的参数是 `section`（键前缀）/ `key`，不同于旧版的页面 / 分组 id，敏感键（含 `api_key` / `token` / `secret` 等）输出 `<redacted>`。
- **依赖**：未新增 crate；`snow-platform` 的 `windows` 依赖新增 `Win32_Security_Cryptography` 特性（`BCryptGenRandom`）。

剩余未实现 tool 数随期数递减：M1 截图域 27（`mcp_status` 已实现）、M2 应用域 23 + 文档域 33、M3 媒体域 13；`registry.rs` 的测试固定“已实现 + 未实现 = 101”。
