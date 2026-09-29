# ADR-8 待验证项：canvas_history.json / canvas_session.bin 调研

## 结论先行

1. 两个文件**都是 Rust 引擎 `serde_json` 的直接产物**，不是独立格式。`canvas_history.json` = `DocumentHistory`，`canvas_session.bin` = `DocumentSession`；`.bin` 后缀是误导，内容是 UTF-8 JSON。
2. C++ 侧把它们当字节透传，只有一处例外：capture_history 的 `validCanvas()` 会用 `QJsonDocument::fromJson` 确认是 JSON 对象/数组。不理解任何字段。
3. Rust 新实现**直接复用引擎的序列化/反序列化**，不重写；要重做的只有容器层（见第 3 节清单）。
4. 前提：GPUI 版继续链接同一套 `snow-draw-engine-*` crate。如果 P4 换标注引擎，这个结论作废。
5. 版本迁移是单向的：新引擎能读 schemaVersion 1..=5，读不了更高版本。
6. 局限：`canvas_session.bin` **无真实样本佐证**（本机 upstream 目录只有 `cache/`，仓库 spikes 里也没有），下面关于它的结论全部来自源码。

## 1. canvas_history.json

| 结论 | 证据 | 置信度 |
|---|---|---|
| 写：Rust 引擎 `serialize_document_history()`，经 C++ `SnowCanvasRuntime::serializeDocumentHistory` 取字节，塞进 `draft.canvasHistory` | `snow_draw_engine_qt/crates/snow-draw-engine/src/session.rs:142-149`；`snow_draw_engine_qt/src/core/snow_canvas_runtime.cpp:119-123`；`snow_shot/src/app/main.cpp:144`；`snow_shot/src/presentation/capture/directcapturehistory.cpp:19` | 高 |
| 读：`capturehistoryrepository.cpp` `load()` 原样读出字节；回放走 `applicationcontroller.cpp:1076` 的 `documentHistory`，最终由引擎 `from_serialized_document_history_with_config` 解析 | `snow_shot/src/storage/capturehistoryrepository.cpp:620-649`；`snow_shot/src/app/applicationcontroller.cpp:1076`；`session.rs:156-177` | 高 |
| 根类型 `DocumentHistory { schemaVersion, document, history }`，`camelCase` + `deny_unknown_fields`（多余字段会拒读） | `session.rs:24-30` | 高 |
| `schemaVersion` 由引擎常量 `DOCUMENT_HISTORY_SCHEMA_VERSION = 5` 定义，与 `config.json` 的 `storage/schema_version` 无关 | `session.rs:10-11,144` | 高 |
| 真实样本顶层字段与结构体逐一对上：`schemaVersion`(5) / `document` / `history` | 样本 `spikes/p0-v8-history-compat/sample/records/b5ac6a4d-.../canvas_history.json`（408 字节）；spike 测试 `spikes/p0-v8-history-compat/src/tests.rs:44-51` | 高 |
| `document` 子字段 `slots, paint_order, revision, next_index, watermark, spotlight, auto_filter_regions` 与 `Document` 结构体一致（注意 document 内部是 snake_case，外层是 camelCase） | `snow-draw-engine-document/src/document.rs:1057-1067` | 高 |
| `history` 子字段 `undoStack / redoStack` 与 `HistoryStore` 一致（`last_*` 三个字段是 `serde(skip)`） | `snow-draw-engine/src/history.rs:29-40` | 高 |
| 样本是空画布（`slots: []`，两个栈都空），**没覆盖任何元素类型**；`HistoryEntry`/`Transaction`/各 `ElementData` 变体只能从源码看，无样本对照 | 同上样本 | 高（指样本局限） |

## 2. canvas_session.bin

| 结论 | 证据 | 置信度 |
|---|---|---|
| 位置：贴图仓储 `pinned_windows_v2/pins/<id>/canvas_session.bin`，manifest 里 payload key 为 `canvas_session`，空则不写文件 | `snow_shot/src/storage/pinnedwindowrepository.cpp:391-393,674-686`；测试路径 `snow_shot/tests/pinned_window_repository_tests.cpp:241` | 高 |
| 写：贴图窗口调 `m_runtime.serializeDocumentSession()` 存入 `record.canvasSession`，仓储原样落盘 | `snow_shot/src/presentation/pinned/screenshotpinnedwindow.cpp:1467`；`snow_draw_engine_qt/.../snow_canvas_runtime.cpp:101-105` | 高 |
| 读：仓储 `readBlob()` 读字节，贴图窗口 `restoreDocumentSession(...)` 交给引擎 | `pinnedwindowrepository.cpp:709-723,828-839`；`screenshotpinnedwindow.cpp:1542,1875` | 高 |
| 格式：UTF-8 JSON，根类型 `DocumentSession { schemaVersion, document, history, editor, sessionConfigSeeded }`，同样 `camelCase` + `deny_unknown_fields`。比 history 多 `editor`（各工具样式、配置）与 `sessionConfigSeeded` | `session.rs:14-22`；`snow-draw-engine-editor/src/session.rs:29-40`（`PersistedEditorSession`）；测试断言顶层键为 `["document","history","schemaVersion"]` 的是另一个 payload，见 `session.rs:422` | 中（键集合以 struct 为准，无样本核对） |
| 不是 bincode，也不是自定义二进制：序列化直接 `serde_json::to_writer` | `session.rs:78-88` | 高 |
| C++ 侧对它完全不透明：仓储只算哈希、量长度，不解析 | `pinnedwindowrepository.cpp:243-258,1641-1647` | 高 |
| 无样本佐证 | 本机 `C:\Users\<用户名>\AppData\Local\SnowShot\snow_shot` 仅有 `cache/`；`find spikes -name canvas_session*` 无结果 | 高（指"确无样本"） |

## 3. C++ 容器层清单（供 `snow-history` / 贴图仓储照搬）

### capture_history（`canvas_history.json`）

| 项 | 行为 | 证据 |
|---|---|---|
| 体积上限 | 16 MiB，写入前 `validCanvas` 检查；读取时最多读 `上限+1` 字节再判 | `capturehistoryrepository.cpp:31,78-84,633` |
| 内容校验 | 非空 + 能被 `QJsonDocument::fromJson` 解析为 object 或 array（唯一一处"看内容"） | 同上 `:82-83` |
| 大小记账 | index 里存 `canvas_byte_size`，范围 1..=16 MiB；读盘后要求实际长度 == `canvas_byte_size` | `:149-150,228,636` |
| 文件名固定 | `canvas_history_file` 必须等于 `canvas_history.json` | `:41,227` |
| 记录总大小 | `totalBytes` 从 canvas 字节起累加，受 quota 与 `kMaximumStoredBytes`(2^40) 约束 | `:416-417,429,37` |
| 原子写 | `QSaveFile` open + write + commit | `:362-365` |
| 路径安全 | 读盘前 `containedPath`：拒绝符号链接、规范化后必须在 root 之下（Windows 不区分大小写） | `:368-383,629` |
| 索引 | `format_version` 当前 2，读时也接受 1；`pending_deletions` 两阶段删除 | `:35,356-358,849-852` |
| 哈希 | 无（capture_history 不对 canvas 做哈希） | 全文件无 hash 调用 |
| 读失败 | 调 `readFailed(record)`，返回空 | `:630,639` |

### pinned（`canvas_session.bin`）

| 项 | 行为 | 证据 |
|---|---|---|
| 体积上限 | 32 MiB（`kMaximumPayloadBytes`），比 capture_history 宽一倍；写前查（新建/更新/提交多处），读时 `file.size()` 超限即失败 | `pinnedwindowrepository.cpp:36,680,718,1644,1709,1749,1885` |
| 哈希 | `qHashBits` 对整块字节取哈希，仅用于内存里判"payload 是否变化"以决定要不要重写文件，**不落盘** | `:243-258,1782-1789` |
| 空值语义 | 空 = 不写文件、manifest 里删 `canvas_session` 键；缺文件读作空且不算错 | `:391,1806-1809,714-717` |
| 原子写 | `QSaveFile`，失败 `cancelWriting()` | `:605-612` |
| 完整性 | 读时校验 `readAll().size() == file.size()`；未见对 blob 做内容校验 | `:721-722` |
| 清理 | `pruneObsoletePayloadFiles` 删 manifest 不再引用的文件 | `:690-697` |
| 索引版本 | `format_version` 硬锁为 2，不等则整体丢弃并 `preserveInvalidIndex` | `:31,1159-1161` |

### 引擎自带的边界（新实现不用再写，但要知道存在）

| 项 | 行为 | 证据 |
|---|---|---|
| 序列化上限 | `MAX_DOCUMENT_SESSION_BYTES = 16 MiB`，`BoundedSessionWriter` 边写边限，超限报 `InvalidState`（不是先序列化完再量） | `session.rs:12,50-88` |
| 反序列化上限 | 空或 >16 MiB 直接 `InvalidArgument` | `session.rs:120-122,160-162` |
| 历史结构校验 | `validate_session`：栈总长 ≤100000，label ≤16384 字节，单条操作数 ≤1000000，并回放 undo/redo 校验一致性 | `snow-draw-engine/src/history.rs:43-72` |

注意两个上限不对齐：引擎序列化 ≤16 MiB，贴图仓储放行 ≤32 MiB。真实 session 不会超 16 MiB，但 Rust 版若改动其中一个，要让三处同步。

## 4. 复用结论与 P4 影响

| 问题 | 结论 | 依据 |
|---|---|---|
| 能否直接复用引擎序列化 | 能。GPUI 版调 `serialize_document_session/history` 与 `from_serialized_*_with_config`，字节直接落盘 | 第 1、2 节 |
| 需要重写 | 仅容器层：文件读写与原子替换、路径包含校验、体积上限、`canvas_byte_size` 记账、贴图 payload 变更检测、manifest 与两阶段删除、index 版本判定 | 第 3 节 |
| 不需要重写 | 元素模型、Transaction、撤销/重做栈、编辑器样式的序列化与迁移 | `session.rs` 全部 |
| 对 P4 的影响 | 贴图里最怕的"标注数据格式兼容"这一块**基本消失**，剩下的是文件 I/O 级别的容器工作，量小且规则清晰。P4 的主要工作量转移到贴图窗口的 UI 与交互，不在数据层 | 定性判断 |
| 前提风险 | 若 GPUI 版绑定引擎的方式变了（例如不再走 FFI 而直接依赖 crate），序列化函数是 `pub` 的，可直接调；若换掉引擎，需要重新评估 | `session.rs:100,116,142,156` 均为 `pub fn` |

## 5. 版本迁移能力

| 结论 | 证据 | 置信度 |
|---|---|---|
| 旧读新：schemaVersion 1..=5 都放行，其他值（含 0、>5）返回 `Unsupported`。session 与 history 各有独立的常量，当前都是 5 | `session.rs:125,165,10-11` | 高 |
| 旧文档靠 serde 缺省值补字段，没有集中式迁移函数。已见的迁移点：serial number 的 `serial_number_type` 缺失时补 `OutlinedCircle`；watermark 的 `template_value` / `template_application_time` 缺失补默认；`auto_filter_regions` 缺失补 `None`；`content_width/height` 缺失补 0；`rectangle_filter_stroke_width` 缺失补默认 | 测试 `session.rs:552-568,612-640`；`document.rs:58-64,329-331,1065`；`editor/session.rs:40` | 高 |
| 版本 1..=4 的兼容性只由上述测试用例守着（把 v5 载荷改写 schemaVersion 并删字段），**没有真实旧版本文件样本**，所以"哪些老文件确实能读"是推断 | 同上测试 | 中 |
| 新写旧读（回滚/降级）不可行：`deny_unknown_fields` 加 schemaVersion 上限，旧引擎会拒绝新字段或新版本号 | `session.rs:15,25,125`；`annotations.rs:14`、`history.rs:13,30`、`editor/session.rs:30` 同样带该属性 | 高 |
| `Document` 本身**没有** `deny_unknown_fields`，所以 document 层的多余字段会被静默丢弃，外层与 history、editor 层则会拒读。两层策略不一致 | `document.rs:1057-1058`（无属性）对比 `session.rs:15` | 高 |
| 不同 schemaVersion 混用风险：自有数据根目录（ADR-8）下，导入器复制来的是 upstream 写的 v5 文件。只要 Rust 版引擎版本不低于 upstream，能读；若 upstream 之后升到 v6，而本项目 fork 的引擎没跟上，导入会得到 `Unsupported` | 推断，取决于 upstream 后续演进 | 中（未验证） |

### 风险点

1. **导入器与引擎版本对齐**：导入 upstream 数据时，遇到 `schemaVersion > 5` 要走"跳过并提示"，不要当损坏处理。
2. **降级不可用**：用户在 GPUI 版存过的贴图，不能再交给 Qt 版打开。数据根已隔离（ADR-8），这一点影响不大，但导出功能里不要承诺可回退。
3. **history 载荷可能很大**：`validate_session` 会回放整个撤销栈，历史记录多、栈长时恢复耗时，属于引擎侧成本，不是容器层的事，P1 测一下即可。
4. **两个 blob 都是 JSON，未压缩**：体积上限靠 16 MiB 兜底，`canvas_history.json` 若含大量元素（如大量自由绘制点）可能触顶，届时 `serialize_*` 返回 `InvalidState`，调用方要有降级路径。C++ 侧的处理方式我没有深挖，**未验证**。

## 未验证清单

- `canvas_session.bin` 的真实样本（本机不存在）。
- 非空画布下 `canvas_history.json` 各元素类型的落盘样子。
- schemaVersion 1..=4 的真实历史文件（只看到合成测试）。
- 序列化超限时 C++ 调用方的降级行为。
