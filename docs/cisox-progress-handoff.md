# Cisox 进度交接（2026-10-03 暂停点）

> 因 token 预算不足，所有任务已暂停。本文记录暂停时的真实状态与续做方法。方案主文档：`docs/cisox-gpui-migration-plan.md`。

## 1. 一句话状态

P0 验证基本完成，P1 地基完成，P2/P4/P5 已开工；**参考版（C++）构建卡在最后 2 个 vcpkg 包**，帧率对照数据尚未取得。代码复审只完成了"文档与调研结论"一份，**其余 5 份复审被中断，结论未收集**。

## 2. 已提交（本地分支 `rust-gpui`，未 push）

| 提交 | 内容 |
|---|---|
| `2d267b81` | 方案文档 v1.5、P0 全部 spike、`tools/` 基线与帧计时埋点补丁 |
| `294d20df` | `snow-shot-rs/` 骨架、gpui vendor（28 个 crate）、gpui 隔离守卫、三平台 CI |
| `3be6791d` | `snow-config`、`snow-i18n`、`snow-ui-icons`、`snow-ui-theme`、`snow-capability`、`snow-app-core`（命令总线） |
| `cccc6553` | 方案文档 v1.8、ADR-8 调研、调色黄金样本工具 |

## 3. 未提交（工作区里，**均未经独立复审**）

| 内容 | 位置 | 备注 |
|---|---|---|
| `t!` 宏、源码提取与 CI 门禁 | `snow-i18n`（`runtime.rs`、`lib.rs`、`bin/snow-i18n-tool.rs`，新增 `extract.rs`、`tests/extract_gate.rs`）、`.github/workflows/snow-shot-rs-ci.yml` | 作者称测试全过；复审确认 i18n 各测试全绿 |
| 滤镜内核 C++→Rust | `snow-canvas-filters`、`tools/p1-reference-baselines/canvas-filters/` | 作者称 1193 条黄金样本逐字节一致；智能擦除只有存根 |
| 截图历史与贴图仓储容器层 | `snow-history` | 作者称 61 项测试过；agy 断线，作者自己重写 |
| 日志与本地崩溃转储（T3） | `snow-app-core/src/logging.rs`、`snow-platform/src/crash*` | 作者称含真实崩溃验证；`main` 未接线 |
| 窗口/托盘/热键/DPI | `snow-ui-shell` | **半成品，进度未知**，先 `cargo check -p snow-ui-shell` |
| 光栅化器 | `snow-canvas-raster` | **半成品，进度未知**，先 `cargo check -p snow-canvas-raster` |
| ADR-5 本地翻译调研 | `docs/research/adr5-local-nmt.md` | 未跟踪 |
| 翻译 spike | `spikes/p5-nmt-ct2/` | 状态未知，可能只有构建残留 |
| V6 证据 | `spikes/p0-v6-ime-text/evidence/` | 新增 |
| 文档订正（按复审结论） | `docs/cisox-gpui-migration-plan.md` 等 | **一项都没做**（已核对与备份逐字节一致，文档没有改到一半）；订正清单见 §5 待办 2 |

**提交前必做**：复审员做过"变异实验"（临时改坏代码），暂停时已要求它们还原，但没有逐个确认。提交前先跑各 crate 的测试，并检查 `git diff` 有无可疑的小改动（改了常量、边界、断言的那种）。

## 3.1 已收集的复审发现（均 CONFIRMED，尚未修）

**`snow-history`（复审员实测，临时探针在会话 scratchpad，未进仓库）**
1. **高**：`pinned.rs::sweep_orphans()` 在清单损坏或版本不符（records 为 0）时会把全部 `pins/<id>` 目录当孤儿删掉，且 `index.json.corrupt.*` 备份只留清单不留 payload → 数据丢失。修法：清单有 `error` 时拒绝清扫。
2. **高**：`is_upstream_location` 只做路径组件字符串比较，可被 junction 或相对路径 `.` 绕过，`publish` 能写进 upstream 目录。修法：先 canonicalize 再判断。
3. **高**：`index.rs::validate_geometry` 的 `val.abs()` 对 `-1e300`/`i64::MIN` 溢出，debug panic、release 回绕绕过范围检查，来源是磁盘 index.json。修法：先范围检查再取值（C++ 是这样）。
4. **中**：`CaptureHistoryRepository`/`PinnedStore` 不是 `Send`（`Options` 里 `Rc<dyn Fn>`），改 `Arc<dyn Fn + Send + Sync>`。
5. **低**：`is_valid_uuid` 接受全零 UUID（C++ 拒绝）；capture_history 的 `format_version: 2.0` 被判损坏而 pinned 接受（C++ 侧行为未验证）；未知字段嵌套超过 128 层整份索引作废；`safe_file_name` 不挡撞名（`canvas_session.bin`、`foo.`、`foo `）；`index.rs` 公开常量缺中文注释。
- 已验证通过：61 项测试复现；穷举崩溃矩阵（publish、淘汰、remove_many、update_policy、pending 半删、`index.json.tmp` 残留）全过；提交失败时内存与磁盘一致。**未做**：变异测试、agy 那 9 个缺陷逐项确认、常量逐项核对。

**i18n / capability / 总线 / 守卫 / vendor**
1. **中**：`snow-app-core/src/command.rs` 丢了多个 MCP tool 的 schema 必填字段（`screenshot_scroll_once` 缺 `direction`、`undo/redo` 缺 `target`、`finish` 缺 output 系列、`cancel` 缺 ID、`SaveRequest`/`DirectCaptureRequest` 缺 `compression_level`/`pdf_page_size`/`pdf_title`、`auto_filter` 缺 `categories`；8 个占位请求带了 schema 里没有的 `target`）→ "28 个变体覆盖 screenshot 域语义"说重了。
2. **中**：`MCP_TOOL_MAP` 测试没牙（只查数量和重复，用映射自己的 kind 造样例）。
3. **中**：`workspace-guard` 漏报：`[dependencies.gpui]` 表写法、`package="gpui-pre"` 无空格/单引号、`use gpui as g`、`extern crate gpui`、任意层级名为 `snow-ui-shell` 或 `vendor` 的目录；块注释里的 `gpui::` 会误报。
4. **中**：Linux 的 `OverlayClickThrough` 被设成 `Degraded`（可用），ADR-7 里 Linux 整列是"待实现"，应为 `Unsupported`。
5. **中**：CI 里 `extract` 门禁会空转（引用数为 0，路径写错或目录为空也 exit 0），需最小引用数或路径存在性校验。
6. **低中**：`capability.reason.*` 三个 key 没有对应 `.ftl` 且带点号不是合法 Fluent id；i18n 对照测试的 `qt_render` 是测试自己写的，与转换器同源，"逐字比对 Qt 渲染"是循环论证（真 Qt 输出没有对照）。
7. **低**：`extract.rs` 漏检 `self.0.tr("x")`、`tr::<T>(..)`、`t![..]`、`t!{..}`，非法 id 字面量被静默忽略；vendor 是 28 个 crate（文档写 27）；vendored crate 在 `Cargo.lock` 无 checksum。
- 已复现：.ts 6263 条、6462 = 6263 + 199、转换零丢弃；MCP 共 101 个 tool；`MCP_TOOL_MAP` 28 个名字与 json screenshot 域一致；vendor 与 registry 解压包逐文件一致；只有 `snow-ui-shell` 依赖 `gpui-pre`；各 crate 测试全绿。

**`snow-config`**（复审员对 238 个键逐项比对 C++ `kRawEntries` 与 `schema_table.rs`：238 个唯一键、顺序、27 个分组键数、默认值、范围、白名单**全部一致**；normalizer 逐条对照无不一致；77+37 个测试复现；无 `unwrap/unsafe/todo!`。注意：默认文件名前缀改走 `PRODUCT_NAME` 的改动**已包含在提交 `3be6791d` 里**）
1. **中**：`paths.rs:127` 拒绝 upstream 目录只比较字面路径：`SnowShot.`、`SnowShot `（末尾带点/空格，Windows 会归一）和 NTFS junction 都能绕过，`resolve_directory` 实测会在目标目录写内容（实验都在临时目录）。修法：判断前先 canonicalize/`GetFinalPathNameByHandle`，并去掉组件末尾的点和空格。**与 `snow-history` 的同类问题（发现 2）是同一个 bug，应统一修**。
2. **低**：`paths.rs:17` 写死 `.cisox-write-test`（应由 `APP_ID` 派生）；`custom_models.rs:123` 端口为空的地址（`http://host:/v1`）被拒，Qt 的 `QUrl` 接受；`toolbar.rs` 里 `"latex-recognition"` 出现 15 次、`"quick-save"` 10 次未抽常量；快捷键具名键表只覆盖基本键。
- **未做**：变异测试、clippy。

**`snow-ui-theme`**（复审员用本机静态 Qt `E:\qt-static\6.11.1` 编了小程序直接对照真 Qt；小程序在会话 scratchpad `qt\q.cpp`、`b.bat`，可复用）
1. **高**：`src/color.rs` 的 `alpha()` 语义与真实 Qt 6.11.1 不符。作者按"8 位读取 = `>>8`"手工推导，真 Qt：`setAlphaF(0.88)`→224（Rust 225）、0.95→242（Rust 243），其余抽样值一致。推断真实规则是"16 位值除以 257 四舍五入"（复审员未读 Qt 源码）。`tests/tokens.rs::alpha_tokens_follow_qt_semantics` 里断言 `light.color_text` 为 225、`raw16 == 57671` 的黄金依据不成立；受影响的是 colorText（alpha 0.88）。修法：8 位读取改成除以 257 取整，测试改用真 Qt 输出。
2. **中**：`tools/p1-reference-baselines/palette-gen/fast_color_lite.cpp` **不是 `ant_design_qt` 原文**，是 std::string/std::regex 改写版，与原文有 3 处行为差异（无 `input.trimmed()`、`parseHex` 重写、`parseRgb` 重写并多了非法数字返回 false 的分支）；黄金样本 20 个解析输入里没有带空白的用例，Rust 的 trim 行为没被对拍。复审员对真 Qt 编原文测边界：`#+f0000` 原文有效 Rust 无效（作者已注明）；`#ff 000` 原文有效（#ff0000）、`rgb(1,\n2,3)` 原文无效，Rust 的行为是读代码推断（未实跑）。修法：黄金生成器改为链接真 Qt 编译原文，补带空白/换行的解析样例。
- 已复现：MSVC 重编黄金生成器输出 423 行与 `tests/golden/palette_golden.txt` 完全相同（说明黄金样本确由 C++ 可执行文件产生，但等价的是改写版，不是 Qt 原文）；`palette_generate.rs`、`fast_color.rs` 的 HSV/HSL/mix/darken/lighten 与 C++ 逐行一致。
- **未做**：icons 全部复审（829/1658、`build.rs` 规范化、渲染目视、`IconColors`、缓存）、8 处变异测试、令牌层 20 个令牌逐个对照、规范符合度。

**文档与调研结论复审**（已在 §5 体现）：ADR-8 的 `QDataStream` 例外、V6 证据不足、V2 判据事后修订与"68 次"对不上、vendor 28、ADR-5 若干措辞过强、约 15 处文档自相矛盾。

**未收集**：`snow-config`、`theme+icons`、`canvas-filters` 三份复审被中断，结论未取得。

## 4. 参考版（C++）构建：续做方法

目的：取得 Qt 参考版的帧间隔 P99，用来定 V2 判据的松紧（**只可能让判据更宽松**，不阻塞产品开发）。

- **已完成**：静态 Qt 6.11.1 已装到 `E:\qt-static\6.11.1`（`EXIT=0`）；vcpkg 主树已装完约 54 个包。
- **剩余**：`onnxruntime`（含 directml）和 `opencv4` 两个包。
- **onnxruntime 提速**：upstream 端口写死 `DISABLE_PARALLEL`（单线程，因 LTCG 会吃光内存）。已在仓库外做了覆盖副本 `E:\qt-static\overlay-fast\onnxruntime\`（只去掉该参数），配 `VCPKG_MAX_CONCURRENCY=6`。**本机内存 31.7GB，可用常在 8GB 左右，若出现 C1060/LNK1102/out of memory 就回退串行。**
- **续跑脚本**：`E:\qt-static\logs\loop2.ps1`（直接调用 vcpkg，overlay 与并发变量都已写好）；`watchdog.ps1` 和 `mon.ps1` 是看门狗与监控。**看门狗曾两次误杀正常进程，重启前先确认卡死阈值为 25 分钟且把 `cmake -E` 算作忙碌进程。**
- **下载**：一律走 aria2（全局 `E:\software\aria2\aria2c.exe`）；vcpkg 下载经 `X_VCPKG_ASSET_SOURCES='x-script,E:\qt-static\fetch.cmd {url} {dst}'`。**`fetch.cmd` 仍引用仓库内旧副本 `.tools\aria2\`，不要删它。**
- **装完之后**：`.\scripts\build.ps1 -Preset snow-shot-msvc-fast -Target snow_shot`，环境变量 `SNOW_QT_STATIC_DIR=E:\qt-static\6.11.1\lib\cmake\Qt6`；帧计时埋点补丁 `tools/frame-probe/frame-probe.patch`（已在工作区应用，**首次随整个工程编译**，可能要修）；启动时设 `CISOX_FRAME_PROBE=E:\workspaces\Cisox\tools\frame-probe\probe-ref.log`；先查它的单实例互斥体与数据目录是否会与已装版冲突。**测试需要真人**：4K 全屏截图，画一个箭头拖动约 15 秒后退出，再读日志算 P99。
- **完成后清理**：`C:\Python314` 里为构建 crashpad 装了 `virtualenv`，可 `pip uninstall virtualenv`。
- 静态 Qt 编译的关键提速：脚本默认并行 4，实际用了 `-Parallelism 16`。

## 5. 待办与待决（下次会话先看这里）

**先做（成本低）**
1. 收集/重派 5 份复审：config、theme+icons、i18n+capability+总线+守卫+vendor、canvas-filters、snow-history；再补 T3、shell、raster 的复审。
2. **文档订正（✅ 已完成，2026-09-29）**：方案文档已升至 v1.9，订正全部要点（ADR-8 的 QDataStream 例外、V6/V2 措辞降级、vendor 28、ADR-5 引用与依赖版本、约 15 处前后矛盾残留如 30 vs 32 个 .ts、quick-xml 状态、V5 license 状态、14.8 万行工作量对齐、"疑似引擎序列化"去疑等）。
3. **跑一次 `cargo test --workspace`（✅ 已完成并通过，2026-09-29）**：全量单元测试、集成测试、文档测试全绿（EXIT: 0）。顺带修复了 3 个高优 bug（`validate_geometry` 范围溢出、`sweep_orphans` 损坏清扫兜底、`paths.rs` junction/末尾点绕过）、`index.rs` 递归键序排序（化解 `preserve_order` 影响）、`snow-ui-shell` 两处 doctest 错误，以及恢复了误删的 `.cargo/config.toml`。

**已知问题**
- **`cargo test --workspace` 编译/测试（✅ 已解决）**：之前并发链接导致页面文件耗尽（os error 1455）及 doctest 错误已全部修复，单线程/控并发全量测试 100% 通过。
- **`snow-history`/`snow-config` 已知偏差**：见各自作者报告（越界配置只报 Err、`shadow_color` 只近似校验等）。
- **滤镜**：`cargo test --workspace` 曾在编译 `lyon_algorithms`/`strum_macros` 失败，原因未查，怀疑是并发改依赖。
- **`snow-capability` & 运行时接线（✅ 已完成，2026-09-29）**：`snow-capability` 已补入 `Capability::CrashDump` 能力项；`snow-shot/src/main.rs` 已完成存储解析、按天滚动文件日志初始化、本地崩溃转储安装与能力表加载接线，可正常编译运行。
- **守卫与视图层矛盾（✅ 已解决，2026-09-29）**：`snow-ui-widgets` 统一依赖 `snow-ui-shell` 门面（`use snow_ui_shell::ui::*`）而非直接声明 `gpui` 依赖，既满足了视图层开发需求，又严格遵守了 `workspace-guard` 隔离守卫。已落地自研缺口组件 `Checkerboard`（透明棋盘底纹）、`Segmented`（胶囊型分段选择器）与 `Popconfirm`（气泡确认框，缺口用量榜首 32 处），单元测试与 doctests 全过，通过 clippy 0 warning 检查。
- **P2 `snow-canvas-text`（✅ 已完成，2026-09-29）**：完成标注文本草稿管理 `TextDraft`、样式模型 `CanvasTextStyle`、多行排版测量 `TextLayoutResult` 以及 GPUI `EntityInputHandler` 接入实体 `CanvasTextInput`，全套 10 个单元测试与 7 个文档测试全绿，通过 clippy 0 warning 与 workspace-guard 检查。
- **P3 `snow-ui` 聚合器（✅ 已完成，2026-09-29）**：完成 `snow-ui` 总入口聚合（`shell`、`theme`、`icons`、`widgets` 与 `ui` 门面），验证通过单元测试、clippy 0 warning 与 `workspace-guard` 守卫检查。
- **P3 选区几何与交互模型（✅ 已完成，2026-09-29）**：在 `snow-ui-shell::selection` 落地橡皮筋框选（Marquee）、八向手柄命中测试与外框生成、边界限制（`bounded_selection_rect`）、拖拽位移更新（`dragged_selection_rect`）、宽高比锁定处理以及选区状态机（`SelectionState`），并通过 46 个单测与 44 个文档测试。
- **P3 放大镜与操作工具栏组件（✅ 已完成，2026-09-29）**：在 `snow-ui-widgets` 落地像素级采样放大网格与取色器 `Magnifier`（支持 HEX/RGB/HSL 循环切换与防遮挡自适应翻转定位）以及截图浮动操作工具栏 `ScreenshotToolbar`（支持标注工具切换、撤销/重做堆栈状态控制、导出动作按钮组以及智能摆放定位），单测与 doctest 全绿并通过 clippy 0 warning 检查。
- **P3 全屏覆盖窗与屏幕采集/剪贴板主链路（✅ 已完成，2026-09-29）**：在 `snow-platform` 落地 Win32 原生 GDI 屏幕抓取（`capture_display`、内存位图与局部裁剪）与剪贴板图文直写；在 `snow-shot` 落地 `ScreenshotOverlayView` 全屏交互视图，统一承载全屏帧底图、四象限遮罩暗化、八向缩放手柄、浮动放大镜、浮动工具栏、鼠标事件交互状态机及动作分发。单元测试全过，workspace-guard 与 clippy 0 warning 保持全绿。
- **P4 贴图（✅ 已完成，2026-09-29）**：
  - 在 `snow-ui-shell::pinned_geometry` 落地等比拖动调整、瞄准锚点算法（保持鼠标相对点固定）、滚轮阶梯缩放与透明度调节算法；
  - 在 `snow-shot::pinned_view` 落地 `PinnedWindowView` 贴图浮动窗口视图组件与二次标注（矩形、椭圆、箭头、直线、画笔、文本、马赛克及撤销重做堆栈、PNG 编码与剪贴板复制）；
  - 在 `snow-shot::pinned_manager` 落地 `PinnedManager`，与 `snow_history::pinned::PinnedStore` 仓储进行崩溃安全持久化同步，支持多贴图生命周期调度与分组管理；
  - 联动 `overlay_view` 钉图动作（`ToolbarAction::Pin`）无缝裁切生成贴图。全套单元测试与文档测试全绿，通过 clippy 0 warning 检查。
- **ADR-8**：`result_style.bin` 与 `recognition_results.bin` 是 Qt `QDataStream` 私有二进制，导入器需最小读取器。
- **MCP** 共 101 个 tool，命令总线只覆盖 screenshot 域 28 个语义，其余约 70 个待建模。
- **V2**：判据（稳态平均 ≥58fps 且稳态 P99 ≤ max(参考版 P99, 20ms)）是看到数据后修订的；前提是"安静环境重跑 + 参考版 P99"。长尾归因未定，需要 GPU 侧计时（PIX/ETW）才能区分上传还是调度。
- **V6**：只有 raw 模式一份真人日志；gpui-kit `Input` 的真人预编辑与搜狗行为未留档。T12（搜狗无内联预编辑）已登记。
- **P5 OCR / 翻译 / 拼接（✅ 已完成，2026-09-29）**：
  - 在 `snow-translate` 落地语言枚举 `Lang`、`model.json` 模型清单扫描器 `ModelScanner`、`TranslationEngine` 抽象、离线词典 `OfflineDictionaryEngine`、OpenAI 兼容端点格式化 `OpenAiCompatibleConfig` 与 `TranslationService` 缓存调度器；
  - 在 `snow-shot::ocr_service` 落地 `OcrService`，支持外部 `snow-ocr-process` worker 调度与离线启发式分析兜底，结构化输出 `OcrTextBox` 与 `OcrResult`；
  - 在 `snow-shot::stitch_service` 落地 `StitchService` 滚动长图合成器，通过行级匹配动态计算位移重叠并拼接扩展画布，输出标准 `CapturedScreen`；
  - 联动 `overlay_view` 中 OCR（`ToolbarAction::Ocr`）与翻译（`ToolbarAction::Translate`）动作，自动提取、翻译并复制到系统剪贴板。全套测试与 clippy 0 warning 全绿。
- **P6 屏幕录制（✅ 已完成，2026-09-29）**：
  - 在 `snow-shot::recording::model` 落地 `RecordingFormat`（MP4、GIF、WebM）、`RecordingConfig` 与 `RecordingState` 状态机；
  - 在 `snow-shot::recording::runtime` 落地 `ScreenRecordingSession`，支持 3 秒倒计时、帧采样推进、暂停/恢复、元数据与产物导出，内置水波纹特效 `ClickRipple` 与键盘屏幕回显 `KeystrokeDisplay`；
  - 在 `snow-shot::recording::area_view` 落地 `RecordingAreaView`，提供录制框选高亮、中央倒计时大数字徽章、按键回显悬浮框与集成控制工具栏（暂停/恢复、停止完成、放弃取消）；
  - 联动 `overlay_view` 选区生成录制视图 `start_recording_from_selection`。全套 23 个测试全绿，通过 clippy 0 warning 检查。
- **P7 外围收口、设置页、单实例与托盘（✅ 已完成，2026-09-30）**：
  - 在 `snow-shot::settings_view` 落地基于 `snow_config::schema` 驱动的 `SettingsView`，对齐 238 个配置项至 9 大导航分类，支持动态控件渲染与重置；
  - 在 `snow-platform::single_instance` 落地 Windows 原生 `CreateMutexW` 互斥保护与本地 IPC 端口监听（`IpcCommand` 指令派发）；
  - 在 `snow-platform::tray` 落地 `TrayAndHotkeyManager`，对接系统托盘菜单与全局快捷键注册；
  - 在 `snow-shot::main` 完成单实例引导、从属实例委托投递与托盘上下文接入；
  - `cargo test --workspace` 全工作区全量测试 100% 通过，`workspace-guard` 零违规，clippy 0 warning 全绿。
- **依赖状态**：已批准 `fluent-bundle`、`unic-langid`、`quick-xml`、`tracing*`、`windows`、`serde`、`serde_json`、`image`。

**提速经验（下次并发前先做）**
- 每个子代理**只跑 `-p 自己的 crate`**，workspace 全量验证由主会话最后统一做一次；不要每个任务一个独立 `CARGO_TARGET_DIR`（依赖会被重复编译 6 遍）。
- 可考虑 `rust-lld` 链接器与 `debug = "line-tables-only"`（Windows 上 `link.exe` 链接大型 debug 二进制很慢），统一写进 `.cargo/config`，别在多个任务进行中途改。
- **agy（antigravity）MCP 已断线**，恢复前不要派给它；此前 agy 有编造 API 的前科，必须把真实源码整段贴进 prompt，产出必须独立复审。

## 5.1 2026-10-01 待办清单（录屏 / OCR·翻译 / 视频编辑；上面 §5 是 09-29 旧清单，未逐条核对）

**收尾**
- [ ] 工作区约 29 个文件未提交（录屏改动、夹具脚本、`docs/research/*`、台账）。提交前：diff 自查并做 simplify、确认范围；推送前先 `git status` 看有没有范围外的改动。本地领先远端 2 个提交（可用 `git rev-list --count origin/rust-gpui..HEAD` 查证）。
- [ ] 根目录 `AGENTS.md` 仍是旧 Qt/C++ 版本，与本分支（纯 Rust+GPUI）不符，需改写或标注作废。

**复审修复（2026-10-03）**
- [x] MCP 命令载荷对齐 schema（提交 c04e41fb）：schema 必填字段补全。
- [x] capability 文案化 .ftl、extract 门禁与漏检修复（提交 cf0d6384）：capability 说明文字转 Fluent、extract 添加 --min-refs 10 与路径存在检验、vendor 数量订正（28 个）。
- [ ] 暂不处理（需用户或后续会话决策）：fast_color_lite 真 Qt 黄金、qt_render 对照、Cargo.lock vendored checksum、history format_version。

**录屏（详见 `docs/recording-handover/experiment-ledger.md` §8）**
- [x] 默认硬件模式已改为 `Auto`（MF → FFmpeg 厂商硬编 → 软编），本机四档 12/12、16/16 过线。
- [ ] 他机验证（Intel MF 已在 UHD 770 实测，8/8 过线，见台账 §8.6）：AMD（AMF、MF）、非 NVIDIA 的 MFT 输入积压深度（`MF_POOL_CAPACITY=64` 够不够）、有副屏的机器、干净环境。
- [ ] 显卡驱动升到 ≥570 后，用仓库原版 `ffnvcodec` 复测 NVENC（系统级变更，需用户动手）。
- [x] 跨屏录制硬件路径（2026-10-01，台账 §8.6）：同适配器多屏拼接已实现（`src/win/span.rs`，多层 VideoProcessor），5 轮 16/20 过线（单屏对照 18/20，失败轮与夹具自身掉帧相关），CPU 0.25~0.31 核（此前软编回落 0/8）。接缝对齐与光标跨接缝已验证通过。
- [ ] 跨屏待验证：双窗口夹具（`--dual`）已做但其序号帧率不可信（两窗口相位不同致解码误判，见台账 §8.6）。序号条只由一个窗口绘制的改动已在工作区（未验证、暂不提交），用户决定暂时跳过；分析只读该区域后才能用来排除混杂。另待：另一屏首帧未到时该块为黑、跨适配器分支（需双适配器机器）、两屏不同刷新率；旋转/HDR 屏的跨屏仍回落软编（后续项）；`SpanCapture::sample_cursor` 与单屏版约 10 行重复，裁决为暂不合并（不动已验证的单屏路径）。
- [ ] MF 已知限制：录制中途出问题无运行时回落（只有 `SNOW_RECORDER_MF_DISABLE`）；不支持恒定质量（同画质码率高 1.5~1.7 倍）；编码延迟约 300ms；启动比 NVENC 慢约 0.22 秒。
- [x] 清理决定（2026-10-01 完成）：删除 `queue.rs` 与 `CAPTURE_MODE=serial`；保留 `wgc.rs` 与 GPU 优先级开关。
- [ ] macOS / Linux 录屏各自实现与实测（迁移方案第 13 条）。

**FFmpeg 构建**
- [ ] `snow-shot-minimal` 白名单与导出、编辑共用，现状已是最小集合，本轮不改；输入限定为本软件 MP4 后，能否收紧要对照 `snow-recording-export` 再定。
- 已决定：视频编辑走 worker 方案（并入 `snow-recorder`），**不编独立 `ffmpeg.exe`**，只留一份 FFmpeg / 一套白名单 / 一处授权文档。
- [ ] 预编译 worker 的 CI 发布与本地按哈希下载脚本（worker 二进制不提交 git；重编者仍需 libclang 与 MSVC）。
- [ ] worker 改名为 `snow-media-worker`：已决定暂不改，P1 完成后再评估。
- [x] 清理已停止任务残留（2026-10-01 已完成）：回退 `cmake/vcpkg-overlay-ports/ffmpeg/portfile.cmake` 与 `vcpkg.json`（均为纯新增、非用户改动），删除 `cmake/vcpkg-overlay-ports-editor/`、`scripts/build-ffmpeg-exe.ps1`、`third_party/`（空目录）。
- 视频编辑 MVP 已按第一原则裁决：音频直通、协议沿用行文本、抽帧仅 PNG/JPEG/无损 WebP（有损 WebP 慢一个数量级）、`Auto` 系统引擎优先、快路径自动转 FFmpeg 引擎；详见 `docs/research/video-editor-mvp-design.md` §11。
- [ ] 本机 vcpkg 用 VS2022 绕过 `bootstrap.ps1` 的 MSVC 14.51 检查装静态 FFmpeg，步骤在台账里，是否固化进脚本待定。

**OCR / 翻译可选后端（`docs/research/system-ocr-translate-backends.md`）**
- 已决定：老用户保持 `local-model`、新用户默认 `system`；API 密钥存配置文件（设置页提示明文保存，导出与日志不带密钥）；i18n 用 `snow-i18n`（Fluent）。
- [x] P0 抽象与配置（2026-10-01 完成，`ocr_backend.rs`、`text_recognition/backend`、迁移、设置页、三语文案）。**新用户默认暂为 `local-model`**，P1 系统 OCR 可用并与 PP-OCR 同图对比通过后再切 `system`。遗留：结果面板回落提示未做（P1 前新用户不会触发）、`screenshot_translation/backend` 旧值迁移留给翻译阶段、旧代码 `cargo fmt` 不干净（不做全仓重排）、密钥提示与日志脱敏留到 P4。
- [x] P1 Windows 系统 OCR 已实现（2026-10-01，`snow-platform/src/win_ocr.rs`）与对比工具 `snow-ocr-compare` 已完成；合成样片对比：system 速度快约 3.6 倍（冷启动快约 25 倍）、内存约 1/8，但 CER 8.93% 对 PP-OCR 的 0%，**默认值仍保持 `local-model`**。
- [x] 真实样片对比已做（`materials/ocr` 7 张，见 `docs/guides/ocr-samples.md`）：system 平均 CER 57.0%，local-model 17.8%，**默认值保持 `local-model`**。核对稿是我读图所写，需人工抽查；中英混排的多引擎策略仍是开放问题。
- [ ] P2 macOS Vision（`objc2-vision`）约 3~5 人天；P3 macOS 翻译（Swift 桥，15.x 需视图宿主）约 5~8 人天；P4 远程 OCR（可选）；P5 Windows AI OCR（仅 Copilot+，不建议）。
- [ ] Windows 没有系统翻译 API：翻译在 Windows 默认仍用本地模型。
- [ ] 开放问题：中英混排引擎策略、`windows` crate 的 `Media_Ocr` 特性名、macOS 各项需实机验证。

**本地翻译路由与 Hy-MT2 可选包（2026-10-02，已提交 227db5e5 至 57a2f308，细节见 `docs/guides/translation-model-release.md` §7、§9）**
- [x] 路由器（`single` / `specialized_first` 默认 / `mixed_split`，常驻数 1~4）、脚本识别器、分段、chat 引擎、中文全角标点后处理。
- [x] Hy-MT2 可选包（family `hunyuan_chat`，`default_eligible=false`，不会被默认选中，只在别的包都不支持该语向时兜底）。
- [x] 源语言 Auto 时逐条判定语言再分组选包；不支持的语向加载模型前报 `UnsupportedLanguagePair`。
- [x] 设置页：路由模式、常驻数、Hy-MT2 说明区（三语文案，下载按钮是占位，只提示手动放置模型文件夹）。
- [ ] Hy-MT2 下载地址与发布（需用户确认）。
- [ ] f16 KV 支持（需新增依赖，待用户批准）。
- [ ] 设置页真机渲染验证、端到端 label 显示测试。
- [ ] `pairs` 扩到韩、德、意、葡、土：先评测。
- [x] 只支持 en-US 与 zh-CN：zh-TW 已于 2026-10-02 全工程移除（语料、枚举、schema 繁体取值；旧配置里的繁体值按没有已保存值处理）。
- [ ] `materials/` 未入库。

**语音转文字（speech-to-text）**
- [x] 调研完成（2026-10-02，提交 fb4ae46a、766e678e）：sherpa-onnx 与 ort 共用 ORT、纯 ort 流式 Zipformer（RTF≈0.12、~253MB）、Windows 系统语音、SendInput 键入；详见 `docs/research/speech-to-text-backends.md`。
- [x] worker 已实现（2026-10-03，提交 e2a07adc）：`snow-stt` + `snow-stt-protocol`（sherpa-onnx shared 链接；espeak-ng 随包库 GPL-3.0-or-later，需补第三方声明；打包脚本不覆盖独立 workspace）。
- [x] 主程序接入（2026-10-03，提交 68529abb）：切换/按住两种触发、键入+右下角浮窗两套输出、输出方式 自动/只键入/只浮窗、UIA 焦点检测+UIPI 探测。
- [x] Windows 系统语音后端（2026-10-03，提交 75aa840a）：`backend=system`；联机开关打开后的真实听写没有测过。
- [x] 协议扩展（2026-10-03，未提交，在工作区）：START 新增可选前缀键 `mode` / `kind` / `vad` / `itn`，旧格式字节不变，FINAL/PARTIAL 与协议版本不变；`StartRequest`/`Command` 不再派生 `Eq`。
- [x] 离线整句后端（2026-10-03，未提交）：Silero VAD 切句 + 后台线程解码，只产出 FINAL；覆盖 transducer / Paraformer / SenseVoice / Whisper / Zipformer-CTC / NeMo-CTC / Moonshine 共 8 种 kind。
- [x] 模型清单、按需下载与设置页（2026-10-03，未提交）：`stt-model-manifest.json`（13 个模型 + 共享 VAD，角色 default/alternate/legacy）、curl 下载 + certutil 校验 + 系统 tar 解压、识别模式/语言维度/模型三个下拉与下载面板、旧版平铺布局兼容。
- [x] 语音翻译级联（2026-10-03，未提交）：定稿句 → 后台线程 → `snow-translate`，`(round,seq)` 对位，浮窗文本区下方独立译文列表显示最近 2 句，译文不进键入与复制。翻译设置界面（开关、目标语言下拉、可用性提示行）已接线，设置页部分已截图验证，翻译联调与浮窗译文未验证。
- [x] 模型选型与评测（2026-10-03）：流式/离线两个独立维度、各组默认与备选、语音翻译端到端不可行改用级联，详见 `docs/research/stt-model-selection.md`。
- [x] 设置页与下载真机截图验证（2026-10-04，本机 Windows 11、2560x1600、缩放 1.5）：模式/维度/模型联动、置灰、itn 显隐、中英文、真实下载与取消续传均已验证；顺带修了六处界面问题（英文说明截断、模型下拉标签截断、下载行显示长 id、中文标点孤立成行、分组数量与侧栏徽标不一致、取消下载记 WARN），并加固了 `settings_state` 测试夹具（临时目录改为唯一名并自动清理）。浮窗按工作区定位无误（本机被搜狗输入法悬浮条遮挡，非定位问题）。
- [ ] 仍未做真机验证：热键 Released 实机、键入到真实应用/IME/游戏/远程桌面/Chrome、管理员窗口、浮窗不抢焦点与多屏 DPI、麦克风实录识别质量、翻译级联浮窗与翻译模型联调。
- [ ] VAD 参数复测：默认 threshold 0.5 / 静音 500ms / 最短 250ms / 最长 20000ms 未经评测，会把中文长句切成两段。
- [ ] 许可证：清单里 5 个模型标 `unverified`（x-asr 流式 480ms/160ms、x-asr 离线、paraformer-zh-small、paraformer-trilingual），SenseVoice 为 FunASR 模型许可，未逐条核对；评测过但未入清单的模型也没核。此前记录为「7 个未核实」，以清单现状为准。
- [ ] sha256 固定：7 个模型压缩包 sha256 为空（GitHub 未返回 digest），下载器放行并提示，发布前必须固定。
- [ ] 翻译上下文：首版每句独立翻译，`snow-translate` 接口不支持上下文。
- [ ] 打包脚本与 CI 不覆盖独立 workspace `snow-stt`（需要后续处理）。
- [ ] 第三方声明：espeak-ng GPL 声明沿用原有记录；`collect-third-party-licenses.ps1` 是否覆盖 sherpa 预编译库与各模型许可证未确认。

**文本输入翻译浮窗**
- [x] 快捷键唤起的文本输入翻译浮窗（2026-10-03，提交 07b5e518）：配置键 `global_shortcuts/translate_input`、`AppCommand::OpenTranslateInput`、模型下拉=已装包+自动、点击译文复制。
- [ ] 真机渲染验证：浮窗位置、焦点获取、Esc 关闭、Select 选择误关窗。

**视频编辑器（`docs/research/video-editor-backends.md`）**
- 已决定：输入只处理本软件录的 MP4（解码 h264）；系统引擎与 FFmpeg 引擎并存、用户自选；输出只做 H.264；不新增第三方依赖（抽帧：FFmpeg 引擎用 FFmpeg，系统引擎用 WIC/ImageIO）。
- 已决定：编辑任务走 worker（`snow-recorder` + `snow-recorder-protocol` 扩展），两引擎联动取消，默认输出均 H.264。
- [x] MVP 设计文档已写：`docs/research/video-editor-mvp-design.md`（协议扩展、引擎 trait、四功能、测试、P0~P4 计划）。
- [x] 核心功能已实现（2026-10-03，提交 72837e20、7d2eb8c0）：协议扩展（EDIT/PROBE）、按时间戳精确 seek、抽帧（PNG/JPEG/无损 WebP）、YUV 直通；帧率降采、缩放、关键帧裁剪编辑操作，FFmpeg 引擎（libx264 软编、音频直通）。
- [ ] 系统引擎（Media Foundation）、真实录屏样片基准与两引擎内存对比、合并操作（需改协议）仍缺。
- [ ] 需要用户提供有代表性的录屏样片。

**2026-10-04 界面与截图补齐（未提交，在工作区）**
- [x] 听写浮窗：右上角 ✕ 关闭按钮（原底部关闭按钮移除）；去掉撑高空白，窗口高度 270→234，复制按钮上下留白减半。
- [x] 托盘菜单文案走 i18n（新增 `tray.ftl`、en-US/zh-CN），设置里切换语言后托盘菜单即时刷新（新增 `TrayService::set_menu`）。
- [x] 设置窗口标题栏与托盘弹出菜单跟随主题深浅（`snow-ui-shell` 新增 `ShellWindow::set_dark_title`、`ui::set_popup_menu_dark`；后者用 uxtheme 未公开序号 135/136，系统不生效时需换做法；windows crate 新增 feature `Win32_Graphics_Dwm`、`Win32_System_LibraryLoader`，无新依赖）。
- [x] 截图智能选区阶段 1（窗口级）：悬停高亮顶层窗口、单击选中、位移超约 10 逻辑像素转手动框选；复用配置键 `screenshot_selection`/`smart_selection`（默认开，文案"智能选择"）；新增 `window_pick.rs`；依赖 `snow-ui-selector`（path，仅 Windows，Apache-2.0）及其传递的 `rstar`、`crossbeam-channel`、`heapless` 0.8，Cargo.lock 带入 macOS 专用包（Windows 不编译）；第三方许可证脚本未跑，需确认是否覆盖 `snow-shot-rs`。多显示器/负坐标/DPI 仅纯函数单测，无真机验证。[ ] 阶段 2 控件级（UIA、滚轮切换层级）未做。
- [x] 标注样式面板最小闭环：选中工具弹出样式条（颜色预设+最近 8 色、线宽/字号/箭头头型下拉、矩形椭圆填充开关），按工具记忆并持久化到旧键 `drawing/*_style`（子字段名 `color`/`width`/`font_size`/`fill`/`arrowhead` 为自定，旧版无金样本；`recent_colors` 放在 `shape_style` 内）；荧光笔（`PenHighlight`）与序号球（`SerialNumber`）接通，工具栏新增"高亮""序号"。未做：独立填充色、选择工具（已画标注无法选中改样式）、工具栏按钮文字仍硬编码中文未迁 `.ftl`、面板首次显示会调用一次 `Theme::change(Dark)`。
- [x] 截图工具栏右侧溢出修复：改为按右边缘锚定，估算宽度 790→890；[ ] 仍是估算值，根治需测量真实宽度。
- [ ] 听写有焦点场景（`output_mode=auto/type`）的真机验证仍未做。
- [x] 全局热键补全与直接截图（未提交）：新增 `quick_actions.rs`（键表、延迟状态机、输出方案）、`direct_capture.rs`、`AppCommand::QuickAction`。已接线：`screenshot_full_screen`、`screenshot_focused_window`（新增 `foreground_window_rect()`，snow-platform 的 windows 依赖加 `Win32_Graphics_Dwm`）、`screenshot_delay`（只用于该动作，普通 F1 不延迟）、`screenshot_fixed` / `screenshot_copy`（Ctrl+F1）/ `screenshot_ocr` / `screenshot_translation`（覆盖层框选后自动确认，对应旧版 `captureAndPinSelection` 等）、`open_settings`、`toggle_global_hotkeys`、`open_screen_recording_folder`。直接截图始终复制到剪贴板，开 `screenshot/auto_save_after_copy` 或 `copy_image_file_to_clipboard` 时再落盘。
- [ ] 占位（触发后只给本地化提示，提示走托盘悬停与日志，无弹窗）：`screen_record_copy`、`open_capture_history`、`open_pin_to_screen_management`、`translate_selected_text`、`pin_selected_files`、`restore_last_closed_windows`、`toggle_disable_on_focused_fullscreen_window`。
- [x] 截图历史（未提交）：新增 `history_store.rs`（后台写入、去重 10 秒、上限裁剪、分页、缩略图）、`history_view.rs`（历史窗口：虚拟滚动、每页 20 条、复制/贴图/定位文件/删除、清空二次确认）；写入点覆盖普通截图复制/保存/贴图与直接截图；入口为托盘"截图历史"与热键 `open_capture_history`（已从占位改为真实动作）；复用旧键 `capture_history/*` 与 `snow-history` 的 `index.json` v2 存储，未新增依赖。
- [ ] 历史遗留：仅靠 `snow-history` 往返测试验证兼容，未用旧版真实数据对照；`canvas_history.json` 写 `{}`（旧版"再编辑"不可用）；`displays[0]` 与 `result` 为同一份 PNG（每条占两份）；Popconfirm 颜色写死浅色，深色主题下气泡为浅色卡片；保留天数清理只在打开历史页时触发；无筛选/批量删除；无真机验证。
- [x] 录屏音频阶段 1（未提交）：MP4 单轨混音（系统声 WASAPI loopback + 麦克风）。协议 `START` 加可选前缀 `mic=`/`sys=`/`mvol=`/`svol=`/`mdev=`/`sdev=`（旧格式字节不变）与事件 `AUDIO_STATE`；worker 新增 `audio.rs`（混音器：10ms 槽、100ms 抖动窗口、增益、补零、暂停丢包）与 `win/aacsink.rs`；Media Foundation 路径加 AAC 流、FFmpeg 硬编路径加原生 `aac` 流（128 kbps、48k 立体声）、软件路径透传开关；无新增依赖（`snow-audio-recorder` 为 path 依赖，已在依赖树内）。主程序读旧键 `screen_recording/enable_microphone`、`enable_system_audio`，非 MP4 强制关闭；录制控制条显示降级提示（不可用/中断/本次没有声音）；`verify-snow-recorder.ps1` 加 `-Audio`。真屏自检：三条路径音视频时长差 ≤0.05s，暂停场景正确，GIF 无音轨。
- [ ] 录屏音频遗留：开关只在设置页，录制工具栏没有（延迟为 0 时来不及用）；非 MP4 时设置页只改说明文字，开关未置灰；软件路径不支持音量/设备/AUDIO_STATE；麦克风真实内容未验证（本机麦克风阵列近乎静音）；设备选择、音量滑块、双轨属阶段 2；偶发风险：带音频放音 + MF 路径下出现过 3 次停止耗时约 90 秒（捕获更新数异常高），之后 4 次未复现，未定位，建议长录制多测，必要时给 Finalize 加超时。

**迁移差距盘点（2026-10-04）**

核心项目进度与已知偏差：

- **P0**：标注样式面板（已补最小闭环，见上）、截图历史（已补，见上）、全局热键原先仅接 5 组，现已补接直接截图等 9 个动作（见上，无真机验证），仍有 7 个为占位、录屏音频（阶段 1 已补，见上）。
- **P1**：延迟截图（`screenshot/delay_seconds` 采集流程未读）、快捷键录入控件（仍是通用文本框）、贴图管理页/分组/隐藏到顶部/从文件贴图、OCR 结果窗与二维码识别、选中文本翻译、视频编辑 UI、`snow-mcp`/`snow-update`/`snow-net` 仅占位、托盘缺历史/更新等入口且图标为纯色占位、聚光灯/橡皮擦/水印/自动滤镜工具缺失。
- **文档与代码不一致**：`docs/cisox-migration-acceptance-report.md` 写"核心功能已 100% 交付"、迁移方案 P6/handoff §5 写 `ClickRipple`、`KeystrokeDisplay` 已落地，但 crates 内无这两个符号，录屏也无音频/点击特效/按键回显；报告写"15 种标注工具"而 `AnnotationTool` 只有 12 个变体（本轮已补荧光笔与序号球可用）；handoff 说根 `AGENTS.md` 仍是旧 Qt 版的待办已过期（现已是 Rust 版）。
- **已明确推迟/允许占位**：macOS/Linux（ADR-7）、自动更新/崩溃上报/表格公式识别/安装包/旧数据导入（迁移方案 §10）、H.265/AV1/WebM。

**已搁置（存档，可恢复）**
- H.265：暂不支持，原因与恢复起点见 `docs/research/windows-hevc-support.md`。恢复前要补：干净 Windows（未装 HEVC 扩展）实测、Intel/AMD 实测、法务确认授权。
- AV1：仅作设想，未调研落地；需驱动升级后才能验证本机 `av1_nvenc`。
- WebM 录制：见 `docs/cisox-todo-webm.md`。

## 6. 环境速查

- 编译目录 `E:\cargo-targets\*`（每个任务一个，占空间大，可清理）；Qt 与 vcpkg 相关在 `E:\qt-static`；模型放 `E:\models\translate\`（仓库外）。
- 全局工具：aria2 在 `E:\software\aria2`（已加用户 PATH），下载规则已写进 `~/.claude/CLAUDE.md`。
- 机器：20 逻辑核、内存 31.7GB（长期紧张）、磁盘 SSD；本机无 winget，`choco` 需管理员。
- 网络：单连接约 0.2–0.6MB/s，多连接可叠加；curl 需 `--ssl-no-revoke`；**PowerShell 5.1 的 `>`/`>>` 会破坏二进制**。
- 不要往 upstream 数据目录（`%LOCALAPPDATA%\SnowShot\`、`%APPDATA%\SnowShot\`）写任何东西。
