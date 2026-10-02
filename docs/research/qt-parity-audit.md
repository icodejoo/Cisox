# Qt 功能清单 vs 当前 Rust 实现：对照审计

> 审计日期 2026-10-03 · 分支 `rust-gpui` · 基线提交 `d6396ea9`
> 目的：给出一份「读代码得出」的真实对照，替代迁移方案 P2~P7 与旧验收报告里没按现状重核的「已完成」。
> 范围：Windows。macOS / Linux 按 ADR-7 延后，单列在 §6，不计缺口。
> 更新 2026-10-03（导出块）：D07、D15、D16、D17、H02、A05、A11 及 §2 / §3 统计按导出补齐后的代码重核；未重跑其余行。

## 0. 怎么读这份表

**状态**

| 标记 | 含义 |
|---|---|
| ✅ | 代码层已接线（入口被 main / app_runtime 实际触达），有真实行为，有测试 |
| 🟡 | 部分：缺的部分在备注里写明 |
| 🟥 | 空壳 / 占位 / 仅常量骨架 |
| ⬜ | 缺失，没有对应代码 |
| ❔ | 仅凭代码判断不了，需要真机，步骤见 §5 |

**依据列**：「读码」= 直接读了源码确认；「推断」= 由检索或命名推出，没有逐行跟踪；「跑测」= 本次实际跑了测试。

**方法与局限**
- C++ 侧以方案 §1.3 为骨架，对照 `snow_shot/src/` 目录、`configurationschema` 的 27 个分组键、托盘项、工具栏 ID、全局快捷键键名补全。C++ 位置只写到文件名，不写行号。
- Rust 侧逐项定位入口。「接线」的判据：从 `main.rs → run_primary → app_runtime::handle_event`（或热键 / 托盘 / IPC 出口）能走到。
- 配置消费统计：把 238 个 schema 键的字面量在 `crates/`、`tools/` 里检索（排除 `snow-config` 与 `settings_*.rs`）。键名若是运行时拼出来的会漏计，已知只有 `HistoryPolicy::from_document` 一处拼接，且主程序没有调用它。
- 本次只跑了 `snow-app-core`、`snow-config`、`snow-history`、`snow-canvas-filters` 四个 crate 的测试（`-j 2`、`--test-threads=2`、BelowNormal）：**260 项通过，0 失败**。其余 crate 的测试数是 `#[test]` 计数（读码），没有重跑。
- 没启动任何图形程序，没碰上游数据目录。

**"黄金样本"现状**（对照 Qt/C++ 输出的测试，只有这几处真有）：
`snow-canvas-filters/tests/golden.rs`（C++ 内核输出逐字节）、`snow-ui-theme/tests/golden_palette.rs`（调色，注意黄金生成器是改写版 `fast_color_lite`，不是 ant_design_qt 原文）、`snow-config/tests/fixtures`（真实 config.json 往返）、`snow-history/tests/fixtures`（真实 Qt 仓储样本）、`snow-i18n/tests/parity.rs`（Qt 渲染对照由测试自己实现，与转换器同源，属循环论证）。**光栅化、选区几何、标注行为、贴图交互、录制、OCR、拼接都没有对照 Qt 的黄金测试。**

## 1. 对照表

表头：`功能 | C++ 位置 | Rust 现状（位置 · 接线 · 测试） | 状态 | 依据`。平台列省略，默认 Windows 适用；非 Windows 项见 §6。

### A. 应用骨架与运行时

| ID | 功能 | C++ 位置 | Rust 现状 | 状态 | 依据 |
|---|---|---|---|---|---|
| A01 | 进程入口、CLI 命令、单实例 + IPC | `app/main.cpp`、`app/singleinstancecoordinator.cpp` | `snow-shot/src/main.rs`（`parse_cli` → `bootstrap` → 主/从实例）；`snow-platform::single_instance` + `win_pipe`（命名管道，含用户 SID）；IPC 命令经 `map_ipc_command` 进收件箱；`--cmd` 支持 7 个命令；有测试 | ✅ | 读码 |
| A02 | 日志 + 本地崩溃转储 | `diagnostics/crashcollector.cpp`、`diagnostics.cpp` | `snow-app-core::logging`、`snow-platform::crash`，`bootstrap()` 接线；有测试。不上报，符合 T3 | ✅ | 读码 |
| A03 | 数据目录解析、便携模式、拒绝 upstream 目录 | `storage/applicationstorage.cpp`、`storagedirectoryutils_p.h` | `snow-config::paths`（含末尾点/空格归一与 canonicalize 解析，已修复 handoff 里记的绕过问题） | ✅ | 读码 |
| A04 | 配置 schema（238 键）、normalizer、读写 | `storage/configurationschema.cpp`、`configurationstore.cpp`、`settingsadapters.cpp` | `snow-config`（schema_table / normalize / store / document）；真实样本往返；本次跑 96 项通过 | ✅ | 读码 + 跑测 |
| A05 | 配置项被运行时实际读取 | （消费方散在各处） | 238 键里有 **40 个**在运行时代码被读（热键 3、贴图 10、录屏 7、截图 11、翻译 4、OCR 4、自定义模型 1）；另有 Rust 新增键（翻译路由、语音、OCR 后端）被读。其余 **198 个**能在设置页改、能落盘，但不改变任何行为。清单见 §2。2026-10-03 导出块新接线 9 个 `screenshot/*` 键 | 🟡 | 读码（检索）+ 推断 |
| A06 | 配置归档导出 / 导入 | `storage/configurationarchive.cpp` | 无 | ⬜ | 读码 |
| A07 | upstream 数据导入器（T9） | `storage/*` | 无。方案刻意延后 | ⬜ | 读码 |
| A08 | 开机自启 / 管理员启动 / 进程优先级 / 应用重启 | `platform/windows/autostartregistration.cpp`、`administratorlaunch.cpp`、`app/applicationrestart.cpp`、`presentation/settings/applicationpriority.cpp` | 无。`system/*` 三个键与托盘「重启」均未消费 | ⬜ | 读码 |
| A09 | 自动更新 | `update/updateservice.cpp`、`updateerrors.cpp`、`app/updateconfirmationdialog.cpp` | `snow-update` 只有 `PHASE` 常量和一个断言常量的测试。T4 刻意禁用 | 🟥 | 读码 |
| A10 | HTTP 客户端 / 代理 / 云端 AI 接口 | `network/snowshotapiclient.cpp` | `snow-net` 只有 `PHASE` 常量。`network/proxy` 未消费。OpenAI 兼容客户端在 `snow-translate::openai`（仅翻译用） | 🟥 | 读码 |
| A11 | 命令总线（取代 god object） | `presentation/services/screenshotcontroller.cpp`、`screenshotmcpcommands_p.h` | `snow-app-core::bus/command`；热键 / 托盘 / IPC 汇入。**只注册了 7 类 handler**：`Capture`、`StartRecording`、`PinSelection`（被解释成剪贴板贴图）、`OpenTranslateInput`、`Toggle/Start/StopDictation`，以及 2026-10-03 新增的 `Export`（作用于当前覆盖窗选区）、`DirectCapture`。其余 screenshot 域命令（选区、工具、撤销…）没有 handler，`emit` 会返回错误。27 项单测 | 🟡 | 读码 + 跑测 |
| A12 | MCP server（101 个 tool） | `app/mcp/*`（约 7.2k 行）、`mcp-capabilities.json`、`snow-crates` 的 `snow-shot-mcp` | `snow-mcp` 只有 `PHASE` 常量。`mcp/enabled` 未消费。`MCP_TOOL_MAP` 只是 28 个名字到命令种类的静态表，没有 server、没有 transport、没有描述符/token 文件 | 🟥 | 读码 |
| A13 | 平台能力注册 | `app/featureavailability.cpp` | `snow-capability`，主程序装载；托盘 / 热键服务按能力启动 | ✅ | 读码 |
| A14 | i18n 机制（Fluent、提取门禁、locale 自动发现） | `i18n/*.ts`、`services/languagemanager.cpp` | `snow-i18n`；en-US + zh-CN 完整；37 项测试；门禁命令见 AGENTS.md | ✅ | 读码 |
| A15 | UI 文案真正走 i18n | 同上 | 设置页、翻译输入浮窗、语音、Hy-MT2 面板走 `.ftl`。**覆盖窗提示、托盘菜单、贴图右键菜单、长截图视图、录制流程、`app_runtime` 里仍是写死的中文字面量**（`overlay_view` 约 86 行、`pinned_view` 55、`app_runtime` 68、`scroll_view` 28、`recording_flow` 15、`ocr_flow` 13、`translate_flow` 17，粗计，含日志行）。违反 AGENTS.md 的 i18n 规则；也导致英文界面下这些位置仍是中文 | 🟡 | 读码（检索） |
| A16 | 主题 / Ant 色板 / 令牌 | `ant_design_qt` 的 `palette_generate`、`presentation/styles/*` | `snow-ui-theme`（色板对拍 423 行；alpha 的 /257 问题已修）。但覆盖窗、贴图等视图直接写死色值（如 `ACCENT_COLOR = 0x1677FF`），没有消费令牌；设置页按 `UiPrefs` 取主题 | 🟡 | 读码 |
| A17 | 图标体系（829 个 Ant 图标） | `ant_design_qt/icons` | `snow-ui-icons`（build.rs 嵌入、resvg 光栅化）；无 C++ 像素黄金样本 | ✅ | 读码 |
| A18 | 主窗口、侧栏、标题栏、关于页 | `presentation/mainwindow.cpp`、`components/sidebarwidget.cpp`、`titlebarwidget.cpp`、`aboutpagewidget.cpp` | 没有主窗口。「唤醒主窗口」被映射成打开设置窗 | ⬜ | 读码 |

### B. 设置页

| ID | 功能 | C++ 位置 | Rust 现状 | 状态 | 依据 |
|---|---|---|---|---|---|
| B01 | schema 驱动设置页（分类、搜索、重置、控件） | `presentation/settings/*`、`components/settingspagewidget.cpp` | `settings_view/state/model/text`；238 键全部可达（`all_238_rows_reachable_through_groups` 测试）；按值类型通用渲染；下拉用 gpui-component `Select`。不是逐页定制，C++ 的专用页都没有 | 🟡 | 读码 |
| B02 | 快捷键录入、冲突检测、热键注册失败回滚 | `services/shortcutrecorder.cpp`、`components/shortcutkeyrow.cpp` | `settings_model` + `app_runtime::on_config_changed`（注册失败回滚配置并提示）；有测试 | ✅ | 读码 |
| B03 | 自定义 AI 模型设置 | `components/customaimodelssettingswidget.cpp` | `api_configuration/custom_models` 列为密钥键，**只读不展示**，没有编辑界面 | 🟥 | 读码 |
| B04 | 存储状态页、截图历史页、贴图管理页 | `components/storagestatussettingswidget.cpp`、`screenshothistorypagewidget.cpp`、`pinnedwindowmanagementpagewidget.cpp` | 无 | ⬜ | 读码 |
| B05 | 设置变更即时生效 | `settings/settingsruntimesession.cpp` | `on_config_changed` 只响应 6 个热键键；主题 / 语言走 `UiPrefs` 重算；其余键改了没有运行时副作用 | 🟡 | 读码 |
| B06 | 路径选择、颜色选择等专用控件 | `components/pathinput.cpp`、`settingscustomwidget.cpp` | 颜色键有十六进制文本输入；路径是文本框；无选择器 | 🟡 | 读码 |

### C. 全局入口：热键、托盘、鼠标

| ID | 功能 | C++ 位置 | Rust 现状 | 状态 | 依据 |
|---|---|---|---|---|---|
| C01 | 全局热键基础设施 | `platform/windows/globalshortcutbackend.cpp`、`services/globalshortcutmanager.cpp` | `snow-ui-shell::hotkey`（`RegisterHotKey`，支持松开事件）；启动注册、改键重注册；有测试 | ✅ | 读码 |
| C02 | `global_shortcuts/*` 动作接线（21 个键） | `services/globalshortcutmanager.cpp` | 已接线 3 个原有键：`screenshot`、`screen_record`、`pin_clipboard_content`；另有 Rust 新增的 `translate_input`、`dictation_toggle/hold`。**18 个未接线**：`screenshot_copy/delay/fixed/focused_window/full_screen/ocr/translation`、`screen_record_copy`、`open_settings/capture_history/pin_to_screen_management/screen_recording_folder`、`pin_selected_files`、`restore_last_closed_windows`、`translate_selected_text`、`toggle_global_hotkeys`、`toggle/disable_on_focused_fullscreen_window`。代码里明说「动作尚未接线，配置已保存但不会注册热键」 | 🟡 | 读码 |
| C03 | 托盘图标与菜单 | `services/systemtraycontroller.cpp` | 菜单：截图 / 录屏 / 剪贴板贴图 / 设置 / 退出，双击开设置。**图标是 32×32 纯蓝色占位**；菜单文字写死中文；无 `menu_options`、左 / 中键动作、分组菜单、重启、禁用快捷键、气泡通知、自定义图标。`tray/*` 6 键全未消费 | 🟡 | 读码 |
| C04 | 全局鼠标手势（`global_mouse` 7 键） | `services/globalmousegesture.cpp`、`globalmousemanager.cpp`、`platform/globalmousebackend.cpp` | 无 | ⬜ | 读码 |
| C05 | 前台全屏窗口时禁用热键 | `platform/focusedfullscreenwindow.cpp` | 无 | ⬜ | 读码 |
| C06 | 覆盖窗内快捷键（`screenshot_shortcuts` 27 键 + `drawing_shortcuts` 10 键） | `overlay/screenshotoverlayshortcutcontroller.cpp`、`services/windowshortcutmanager.cpp` | `overlay_view::handle_key` 写死 Esc / Enter / Ctrl+C / Ctrl+S / Ctrl+Z / Ctrl+Y / C / D；不读配置；无方向键微调、工具快捷键、历史切换、锁比例 | 🟡 | 读码 |

### D. 截图主链路

| ID | 功能 | C++ 位置 | Rust 现状 | 状态 | 依据 |
|---|---|---|---|---|---|
| D01 | 屏幕采集 | `capture/screenshotcapturecoordinator.cpp`、`screenshotcaptureworker.cpp`；`snow-crates/snow-capture` | `snow-platform::capture`，GDI 抓取，**只抓光标所在的一块显示器**；不用 `snow-capture`（DXGI/WGC）；无光标采集、HDR、窗口排除。后台线程采集后回主线程开覆盖窗 | 🟡 | 读码 |
| D02 | 冻结覆盖窗 | `overlay/screenshotoverlaywindow.cpp`、`screenshotoverlaycoordinator.cpp`、`screenshotoverlaypool.cpp` | `overlay_view.rs`（3212 行，53 项测试）；单显示器、不透明窗；无多屏跨屏选区；无窗口池（每次新建）。性能基准（4K 标注）有环境变量驱动，结果见台账 | 🟡 | 读码 |
| D03 | 选区交互（框选、八向手柄、拖动、边界限制） | `selection/screenshotselectiongeometry.cpp`、`core/screenshotgeometry.cpp` | `snow-ui-shell::selection`（46 项测试）+ overlay 状态机；右键先撤销选区再关闭 | ✅ | 读码 |
| D04 | 选区高级项（锁比例、上次选区、预设、圆角、阴影、尺寸弹窗、单位、过渡动画） | `selection/screenshotselectionresize*.cpp`、`screenshotselectionsettingsstore.cpp` | 无；`screenshot_selection/*` 8 键与 `screenshot_ui/*` 13 键全未消费 | ⬜ | 读码 |
| D05 | 智能元素 / 窗口选区 | `selector/*`；`snow-crates/snow-ui-selector`、`snow-visual-region-detector` | 无。`snow-capability` 只有 `ElementPicker` 声明，没有代码；`screenshot_selection/smart_selection` 未消费 | ⬜ | 读码 |
| D06 | 放大镜 / 取色 | `services/screenshotcolorpicker*.cpp`、`screenshotcanvascolorsampler*.cpp` | `snow-ui-widgets::magnifier` + 覆盖窗 `C` 复制光标处颜色；取色格式 / 坐标模式 / 辅助线键未消费；无独立取色窗 | 🟡 | 读码 |
| D07 | 延迟截图、固定区域、聚焦窗口、全屏直接截图 | `capture/directcapture*.cpp` | 总线 `DirectCapture` 已有 handler：当前显示器 / 焦点窗口 → 复制（可附带自动保存）或保存（指定路径 / 自动路径，格式 / 质量 / 压缩 / PDF 参数取请求，缺省读配置）；`render` 输出、缩放、光标采集未做；无对应全局热键、无延迟 / 固定区域；`screenshot/delay_seconds` 只在设置页 | 🟡 | 读码 + 跑测 |
| D08 | 标注工具集 | `tools/*`；`snow_draw_engine_qt/crates`（15 个引擎工具） | 工具栏 8 个：矩形、椭圆、箭头、直线、画笔、文字、马赛克、模糊（经 `snow-draw-engine`）。`Highlighter`、`Counter` 在枚举里但引擎映射为 `None`，选了没有效果、也不在工具栏。缺：荧光笔（矩形 / 画笔）、聚光灯、序号、水印、橡皮、自动滤镜、选择 / 移动 | 🟡 | 读码 |
| D09 | 工具样式面板（颜色、线宽、模板、最近使用） | `tools/screenshottoolpalette*.cpp`；`drawing/*` 16 键 | 无；样式写死（默认色、3 逻辑像素线宽）；`drawing/*`、`screenshot_toolbar/*` 未消费 | ⬜ | 读码 |
| D10 | 标注光栅化（patch → tiles） | `snow_draw_engine_qt/src`（QPainter） | `snow-canvas-raster`（tiny-skia，27 项测试：合成场景 + 引擎端到端 + 4K 耗时）；**无对 QPainter 输出的像素黄金比对** | 🟡 | 读码 |
| D11 | 滤镜内核（马赛克、模糊、反相、浮雕等） | `snow_canvas_filter_avx2.cpp`、`snow_canvas_pen_mask_avx2.cpp` | `snow-canvas-filters`；C++ 内核黄金样本逐字节对拍；本次跑通过 | ✅ | 读码 + 跑测 |
| D12 | 智能擦除 | `snow_canvas_smart_erase_algorithm.cpp`（约 880 行，依赖 OpenCV） | `smart_erase()` 恒返回 `false`，文件头自述未移植 | 🟥 | 读码 |
| D13 | 标注文本与 IME | `snow_draw_engine_qt/src/text/*` | `snow-canvas-text`（20 项测试）+ `CanvasTextInput` 接入覆盖窗与贴图 | ✅ | 读码；IME 手感见 §5 |
| D14 | 撤销 / 重做 | 引擎 | 经引擎 `undo/redo_with_viewport_changes`，覆盖窗与贴图均接线 | ✅ | 读码 |
| D15 | 导出：复制到剪贴板 | `services/screenshotclipboardservice.cpp`、`screenshotclipboardcontent.cpp` | `snow-platform::clipboard` 写位图；覆盖窗 Enter / Ctrl+C / 双击；总线 `Export(Copy)` 已接；`auto_save_after_copy` 已生效（复制后按自动保存规则再落盘一份，失败只记日志）；无「复制为文件」（`copy_image_file_to_clipboard` 只读入、未落到剪贴板文件格式） | 🟡 | 读码 + 跑测 |
| D16 | 导出：保存文件 | `services/screenshotsaveexportpipeline.cpp`、`screenshotimagefileservice.cpp`、`screenshotsaveasfiledialog.cpp` | 覆盖窗「保存」/ Ctrl+S = 系统另存为对话框（`snow-platform::file_dialog`，`IFileSaveDialog`，以覆盖窗为所有者，异步执行不占着界面借用；记住上次目录 / 格式并写回配置）；快速保存（复制后自动保存、贴图 / 长截图保存、直接截图）按 `image_save_directory` + `auto_save_filename_format`（默认含 `PRODUCT_NAME`）+ `image_format` 落盘，重名加 `_N`，目录不可用依次回退图片 / 文档目录；格式 PNG / JPEG / BMP / WebP / PDF，质量、压缩级别、PDF 页面读配置，总线 `Export(Save)` 的 path / automatic_path / format / quality / compression_level / pdf_page_size / pdf_title 覆盖已接。**未做**：JXL / AVIF（配置为这两种时回退 PNG 并记日志）、WebP 有损（`image` 只有无损编码器，质量 <100 仍输出无损）、自绘对话框（`save_as_file_dialog = snow_shot` 回退系统对话框）、保存路径快捷项、缩放比例导出、保存后写历史 | 🟡 | 读码 + 跑测；对话框外观待真机 |
| D17 | PDF 导出 | `services/screenshotpdfexport.cpp` | `export_pdf.rs`：单页 PDF 1.7，版式规则同旧版（96 dpi 换算、A4 纵 / 横居中等比、超大页用 `UserUnit`）；质量 100 为 Flate 无损（复用 PNG IDAT + PNG 预测器，带透明度时输出 SMask），<100 为 JPEG（DCT，白底合成）；标题缺省取文件名主干、至多 1024 字符。与旧版差异：整幅一张图（旧版分 2048 块，且无损用 qCompress）；创建时间不带时区。无 Qt 黄金文件可对照；**未用外部 PDF 阅读器打开核对** | 🟡 | 读码 + 跑测（结构 / 偏移 / 页面尺寸 / 质量） |
| D18 | 截图历史 | `services/screenshothistoryservice.cpp`、`capture/directcapturehistory.cpp` | `snow-history::capture_history`（仓储层 960 行，23 项集成测试，真实 Qt 样本夹具）。**主程序零引用**，无写入点、无界面；`capture_history/*` 6 键未消费 | 🟡 | 读码 |
| D19 | 长截图（滚动截取 + 拼接） | `capture/screenshotscrolling*.cpp`；`snow-crates/snow-stitch-images` | `scroll_capture` + `scroll_view` + `stitch_service`（复用 `snow-stitch-images`，适配层规避已审计缺陷）；自动滚动走 `WM_MOUSEWHEEL`；覆盖窗选区 → 采集线程 → 复制 + 分块保存；合成帧序列离线测试 | ✅ | 读码；真机效果见 §5 |
| D20 | 快门声、消息提示 | `capture/camerashuttersound.cpp`、`services/screenshotmessageservice.cpp` | 无 | ⬜ | 读码 |

### E. 贴图

| ID | 功能 | C++ 位置 | Rust 现状 | 状态 | 依据 |
|---|---|---|---|---|---|
| E01 | 选区贴图（原位） | `selection/screenshotselectionpin.cpp` | `UiEvent::PinCreate` → `PinnedManager::create_from_rgba`；有测试 | ✅ | 读码 |
| E02 | 剪贴板贴图（热键 / 托盘 / IPC） | `services/screenshotclipboardcontent.cpp` | `create_from_clipboard`，三个入口都接线 | ✅ | 读码 |
| E03 | 贴图窗口交互（拖动、八向缩放、滚轮缩放 / 透明度、置顶、右键菜单、复制、另存） | `pinned/screenshotpinnedwindow.cpp`、`screenshotpinnedinteraction.cpp` | `pinned_view`（1875 行）+ `pinned_model`（1045 行）+ `snow-ui-shell::pinned_geometry`；双击 / 中键 / 滚轮模式读配置；有测试 | ✅ | 读码；手感见 §5 |
| E04 | 贴图二次标注 | `pinned/screenshotpinnededitcontroller.cpp` | 复用 `AnnotationLayer`，右键菜单进入；工具集同 D08 | 🟡 | 读码 |
| E05 | 贴图持久化与启动恢复 | `storage/pinnedwindowrepository.cpp` | `snow-history::pinned` + `PinShared`；启动时 `RestorePins`；退出时 `persist_all`；容量 / 保留策略读 `pinned_history/*` | ✅ | 读码 |
| E06 | 贴图快捷键（`pin_to_screen_shortcuts` 15 键） | `services/windowshortcutmanager.cpp` | 无；贴图窗里只有写死的 Esc / Ctrl+C / Ctrl+S / Ctrl+Z / Ctrl+Y | ⬜ | 读码 |
| E07 | 点击穿透、缩略图模式、隐藏到顶部、方向键移动 | `pinned/screenshotpinnedclickthroughgeometry.cpp`、`hidetotopcontroller.cpp` | 无 | ⬜ | 读码 |
| E08 | 贴图分组与托盘分组菜单 | `services/pinnedwindowgroupmanager.cpp` | 仓储里有 `active_group_id`，写入记录；无切换、新建、删除界面 | ⬜ | 读码 |
| E09 | 贴图管理页 | `components/pinnedwindowmanagementpagewidget.cpp` | 无 | ⬜ | 读码 |
| E10 | 贴图上识别文本、自动 OCR、复制原图 | `pin_to_screen/automatic_text_recognition` 等 | 无 | ⬜ | 读码 |
| E11 | 从文件贴图、恢复最近关闭 | `services/screenshotfilepinbatch.cpp`、`platform/windows/selectedfiles.cpp`、`historypinplacement.cpp` | 无 | ⬜ | 读码 |

### F. 录屏

| ID | 功能 | C++ 位置 | Rust 现状 | 状态 | 依据 |
|---|---|---|---|---|---|
| F01 | 录制独立进程 + 行协议 | `recording/screenrecordingcontroller.cpp`；`snow-crates/snow-screen-recorder` | `tools/snow-recorder`（独立 workspace）+ `snow-recorder-protocol`；`recording/client.rs` 拉起进程；录屏性能与硬件路径有实验台账 | ✅ | 读码；性能数据见台账 |
| F02 | 选区 → 倒计时 → 控制条（暂停 / 继续 / 停止 / 放弃） | `recording/screenrecordingareawindow.cpp`、`toolbarwindow.cpp`、`countdownoverlay.cpp` | `recording_flow` + `recording::area_view`；热键 / 托盘 / IPC / 覆盖窗工具栏四个入口 | ✅ | 读码；界面见 §5 |
| F03 | 输出格式 MP4 / GIF / APNG / WebP | `screenrecordingfolder.cpp` | 已接线；输出目录与文件名模板读配置 | ✅ | 读码 |
| F04 | WebM | 同上 | 刻意移除，见 `cisox-todo-webm.md` | ⬜ | 读码 |
| F05 | 硬件编码（Auto：MF → 厂商硬编 → 软编） | （C++ 侧走 snow-crates） | worker 内实现，默认 `Auto` | ✅ | 读码 |
| F06 | 跨屏选区录制 | `recording/screenrecordinggeometry.cpp` | worker 的跨屏拼接已实现（`win/span.rs`）；**覆盖窗只能在单显示器内选区**，所以界面上选不出跨屏区域 | 🟡 | 读码 |
| F07 | 音频（系统声 / 麦克风） | `snow-crates/snow-audio-recorder` | 无；`plan.rs` 里 `enable_system_audio` 固定 `false`；协议无音频字段 | ⬜ | 读码 |
| F08 | 鼠标点击 / 轨迹高亮、按键回显 | `recording/recordingeffect*.cpp`；`snow-crates/snow-recording-effects` | 无。**方案 P6 描述的 `ClickRipple` / `KeystrokeDisplay` 在代码里不存在**（全仓检索无结果）；录制进程不依赖 `snow-recording-effects` | ⬜ | 读码 |
| F09 | 录屏快捷键、复制到剪贴板、打开录屏目录 | `recording/screenrecordingshortcutcontroller.cpp` | 无；`screen_recording_shortcuts/*` 4 键、`screen_record_copy`、`open_screen_recording_folder` 未接线 | ⬜ | 读码 |
| F10 | 录屏设置项（清晰度、编码器、预设、循环、特效颜色等） | `screen_recording/*` 25 键 | 7 个被读取（帧率、格式、光标、延迟、目录、文件名、动图帧率）；其余 18 个未消费 | 🟡 | 读码 |

### G. OCR / 识别 / 翻译

| ID | 功能 | C++ 位置 | Rust 现状 | 状态 | 依据 |
|---|---|---|---|---|---|
| G01 | OCR 引擎（local-model，独立进程） | `ocr/screenshotocrrecognitionservice.cpp`、`screenshotocrassets.cpp`；`snow-crates/snow-ocr-process` | `ocr_client`（协议 v4 客户端）、`ocr_service`、`ocr_assets`、`ocr_download`、`ort_runtime`；常驻、模型档位、DirectML、缩放策略读配置；协议 / 路径 / 失败路径有测试 | ✅ | 读码；端到端见 §5 |
| G02 | OCR 结果呈现与编辑 | `ocr/screenshotocrvisuals.cpp`、`screenshotrecognitionwindow.cpp`、`screenshotocrtexteditingsession.cpp` | 覆盖窗内面板：最多 8 行、每行 56 字，框线叠加，Enter 复制全文；无识别窗口、无选字编辑、无版面重建；`text_recognition/*` 7 键中 `fill_style`、`save_recognition_result_as_image`、`model_hot_start` 未消费 | 🟡 | 读码 |
| G03 | 二维码识别 | `ocr/screenshotqrcontroller.cpp`、`screenshotqrrecognitionservice.cpp` | 无；`auto_recognize_qr_code` 未消费 | ⬜ | 读码 |
| G04 | 表格识别、LaTeX、Markdown / HTML 转换 | `ocr/screenshottable*.cpp`、`screenshotimageconversion*.cpp` | 无；连 ADR-5 要求的「未配置时引导卡片」也没有；工具栏 ID 只存在于配置常量 | ⬜ | 读码 |
| G05 | 截图翻译（OCR → 翻译 → 展示） | `ocr/*`、`app/translationservice.cpp` | 覆盖窗 `Translate` 动作：OCR → 本地路由翻译 → 原文 / 译文对照面板 → Enter 复制译文；缺运行时 / 模型有分级提示与下载入口。**不是把译文画回原图**（`original_image_translation` 未消费） | 🟡 | 读码 |
| G06 | 本地翻译引擎与路由 | 无（Qt 版走云端） | `snow-translate` + `tools/snow-translator`（NLLB / Hy-MT2、路由、分段、全角标点后处理）；113 项测试 | ✅ | 读码；真实模型见 §5 |
| G07 | 翻译页、独立翻译窗 | `components/translationpagewidget.cpp`、`standalonetranslationwindow.cpp` | 无翻译页；Rust 新增的输入翻译浮窗覆盖「输入文字 → 译文」这一条 | ⬜ | 读码 |
| G08 | 选中文字翻译 | `services/selectedtexttranslation*.cpp`；`snow-crates/snow-selected-text` | 无（`translate_selected_text` 热键未接线，也没用 `snow-selected-text`） | ⬜ | 读码 |
| G09 | 自定义 / OpenAI 兼容模型通道 | `network/snowshotapiclient.cpp` | `snow-translate::openai` 能格式化请求，但配置入口是只读键（B03），表格 / 公式提取没有消费者 | 🟡 | 读码 |

### H. 其它

| ID | 功能 | C++ 位置 | Rust 现状 | 状态 | 依据 |
|---|---|---|---|---|---|
| H01 | 权限引导 | `services/apppermissionservice.cpp`、`permissionguidecontroller.cpp` | Windows 无此需求（macOS 专有，见 §6） | — | — |
| H02 | 视频格式 / 图像编解码桥 | `image/snowimagecodecbackend.cpp` | 用 `image` crate 开 png / jpeg / bmp / webp（仅无损编码）；无 JXL / AVIF | 🟡 | 读码 |
| H03 | 缩略图缓存 | `components/thumbnailcache.cpp` | 无（无历史页，暂不需要） | ⬜ | 读码 |

## 2. 配置键消费情况（A05 展开）

被运行时读取的 40 个键：`api_configuration/custom_models`；`global_shortcuts/{pin_clipboard_content, screen_record, screenshot}`；`pin_to_screen/{border_active_color, border_color, double_click_action, middle_mouse_button_action, mouse_wheel_zoom_mode}`；`pinned_history/{enabled, keep_permanently, max_disk_mib, max_entries, retention_days}`；`screen_recording/{animated_image_frame_rate, frame_rate, output_format, show_cursor, start_delay_seconds, video_filename_format, video_save_directory}`；`screenshot/{image_format, image_save_directory, image_quality, compression_level, pdf_page_size, manual_save_filename_format, auto_save_filename_format, last_manual_save_directory, last_manual_save_format, auto_save_after_copy, save_as_file_dialog}`；`screenshot_translation/{layout_processing, model, source_language, target_language}`；`text_recognition/{detector_resize_policy, direct_ml_acceleration, model_type, resident_process}`。

另外通过 `settings_*` 或 `UiPrefs` 间接生效的有 `interface/{language, theme_mode, theme_primary_color}`（设置页自己用）。

**完全没被读取的分组（键数）**：`screenshot_shortcuts`（27 个，实际全未读）、`pin_to_screen_shortcuts`（15 个中 15）、`drawing`（16 个中 16）、`drawing_shortcuts`（10）、`screenshot_ui`（13）、`capture_history`（6）、`global_mouse`（7）、`tray`（6 个全未读，`tray/icon` 只在设置页出现）、`screenshot_toolbar`（8）、`screenshot_selection`（8）、`screen_recording_shortcuts`（4）、`system`（3）、`updates`、`network`、`mcp`、`screenshot_conversion`、`extended_features`（3）。`screenshot` 分组 27 个键里读了 11 个（`copy_image_file_to_clipboard` 已读入导出配置但未落到剪贴板文件格式，不计）。

结论：设置页「238 项全部对齐」成立的是**数据层**，不是**行为层**。

## 3. 汇总

### 3.1 各类别状态计数

（由脚本按表格行统计，`—` 行（H01，Windows 不适用）不计。）

| 类别 | ✅ | 🟡 | 🟥 | ⬜ | ❔ | 合计 |
|---|---:|---:|---:|---:|---:|---:|
| A 应用骨架 | 7 | 4 | 3 | 4 | 0 | 18 |
| B 设置页 | 1 | 3 | 1 | 1 | 0 | 6 |
| C 全局入口 | 1 | 3 | 0 | 2 | 0 | 6 |
| D 截图主链路 | 5 | 10 | 1 | 4 | 0 | 20 |
| E 贴图 | 4 | 1 | 0 | 6 | 0 | 11 |
| F 录屏 | 4 | 2 | 0 | 4 | 0 | 10 |
| G OCR / 翻译 | 2 | 3 | 0 | 4 | 0 | 9 |
| H 其它 | 0 | 1 | 0 | 1 | 0 | 2 |
| **合计** | **24** | **27** | **5** | **26** | **0** | **82** |

说明：没有单独标 ❔ 的行。凡代码层判为 ✅ 但手感 / 真机效果无法由代码证明的，在备注写了「见 §5」，并列入 §5 清单；这些行不降级，也不算「已验收」。

### 3.2 与上游的差异

**刻意取舍（方案已记录）**
- WebM 录制移除（F04），待实现。
- 翻译由云端改本地模型（NLLB / Hy-MT2），OpenAI 兼容通道只作可选后端。
- 表格 / 公式提取：方案要走自定义模型通道，目前连通道入口都没有（G04、B03）。
- 自动更新整体禁用（A09，T4）；不做安装包（T7）。
- 不导入 upstream 数据（A07，T9）。
- macOS / Linux 延后（§6）。
- 单屏覆盖窗：跨屏选区登记为后续项（D02、F06）。
- 录制走独立进程、不链接 FFmpeg 到主程序；MCP 计划进程内化。

**非刻意的缺口**（方案没说不做，只是没做完或没接上）：D05 智能选区、D08 剩余 7 个标注工具、D09 样式面板、D18 历史界面、D01 多屏 / DXGI 采集、E06~E11 贴图后半、F07 / F08 录屏音频与特效、C02 的 18 个热键、C03 托盘细节、G02~G04 识别结果窗 / QR / 表格、A05 的 198 个未消费键、A12 MCP。

### 3.3 Rust 侧新增、Qt 没有的功能

| 功能 | 位置 | 接线 / 测试 | 状态 |
|---|---|---|---|
| 输入框翻译浮窗（热键唤起、模型下拉、点击复制） | `translate_input*.rs` | 热键 + 总线接线；有测试；渲染需真机 | ✅（真机待验） |
| 语音转文字（按住 / 切换，键入或浮窗输出） | `tools/snow-stt`、`snow-stt-protocol`、`snow-shot/src/dictation/*` | 热键接线；有测试；真机清单见交接文档 §5.1 | ✅（真机待验） |
| 视频编辑后端（抽帧、降帧率、缩放、关键帧裁剪；FFmpeg 与 Media Foundation 两引擎） | `tools/snow-recorder/src/edit/*`、协议 `EDIT/PROBE` | worker 内有测试；**主程序没有任何入口**（`snow-shot` 无 `EditRequest` 引用） | 🟡 |
| Windows 系统 OCR 后端 + OCR 后端选择 | `snow-platform::win_ocr`、`ocr_backend.rs`、`snow-ocr-compare` | 已接线；有对比工具；默认仍 local-model | ✅ |
| 翻译路由、Hy-MT2 可选包、设置页说明区 | `snow-translate::router`、`translate_settings.rs` | 已接线；下载按钮是占位（只提示手动放置） | 🟡 |
| 录屏硬编 Auto、MF 引擎、跨屏拼接 | `tools/snow-recorder` | 台账有实测 | ✅ |
| 命令行 `--cmd` 控制主实例 | `main.rs` | 有测试 | ✅ |
| 开发 / 验收工具：fps 夹具、OCR 对比、各类自动化环境变量（`SNOW_OVERLAY_BENCH*`、`SNOW_RECORDING_AUTOTEST` 等） | `tools/*`、`app_runtime.rs` | 仅开发用 | — |

## 4. 最重要的 10 个缺口（按用户价值）

1. **设置页大面积不生效**（A05、B05）：198 / 238 个键只落盘不改行为，用户改了没反应。截图保存格式、历史、工具样式、选区外观、托盘、快捷键等全在此列。
2. **标注工具不全且不能调样式**（D08、D09）：缺荧光笔、聚光灯、序号、水印、橡皮、自动滤镜，剩下 8 个工具颜色线宽写死；这是截图工具的核心卖点。
3. ~~导出只有 PNG，且无另存为 / 快速保存 / PDF~~（D16、D17）：2026-10-03 已补另存为 / 快速保存 / PNG·JPEG·BMP·WebP·PDF，文件名前缀改读配置模板（默认含 `PRODUCT_NAME`）；剩 JXL / AVIF、WebP 有损、自绘对话框，对话框外观与 PDF 阅读器兼容性待真机。
4. **智能元素 / 窗口选区缺失**（D05）：上游高频使用的一键选窗口没有；`snow-crates` 里 `snow-ui-selector` 现成可接。
5. **只支持单显示器截图与选区**（D01、D02、F06）：多屏用户无法跨屏截图 / 录屏，GDI 采集不处理 HDR 与光标。
6. **全局热键只接了 3 个原有动作**（C02、C04、C06）：复制 / OCR / 翻译 / 延迟 / 全屏 / 聚焦窗口截图、鼠标手势、覆盖窗内键位配置全无。
7. **录屏无音频、无鼠标 / 键盘特效**（F07、F08）：而且方案和旧验收报告写成「已完成」，实际代码不存在。
8. **OCR 之外的识别能力缺失**（G03、G04）与 OCR 结果只有 8 行面板（G02）：QR、表格、公式、Markdown / HTML 转换、识别结果窗都没有。
9. **MCP 与命令总线只通了 5 个命令**（A11、A12）：对外集成能力为零；进程内 MCP 是方案的「净减法」承诺，尚未开工。
10. **截图历史与贴图管理无界面**（D18、E08、E09、B04）：仓储层做得扎实（有真实 Qt 样本夹具），但主程序不写历史、没有任何查看入口；贴图分组 / 管理 / 点击穿透 / 快捷键也缺。

另需关注但未进前十：A15 大量写死中文（英文界面下混杂）、C03 托盘占位图标、A08 开机自启、A06 配置归档、A09 自动更新（刻意禁用）。

## 5. 需要真机验证的清单

先关屏保 / 锁屏，避开远程控制；录屏项必须用有副屏的机器。每步写给人照着点。

1. **覆盖窗与选区**：按截图热键 → 拖出选区 → 拖八个手柄 → 右键一次（应撤销选区）再右键（应关闭）→ 双击选区（应复制并关闭）。双显示器下在副屏再做一遍，确认覆盖窗落在光标所在屏、坐标无偏移。（D02、D03）
2. **标注手感与 IME**：选区后依次点工具栏 8 个工具各画一次；文字工具里用微软拼音输入「你好」，确认预编辑下划线与候选窗位置；用搜狗输入一次，确认上屏正确（内联预编辑缺失为已知限制）。Ctrl+Z / Ctrl+Y 各按几次。（D08、D13、D14）
3. **复制 / 保存**：Enter 复制后到画图里粘贴，检查与选区一致、含标注；Ctrl+S 保存，到配置目录看文件名、格式。（D15、D16）
4. **贴图**：点工具栏「贴图」→ 在贴图上滚轮缩放、Ctrl+滚轮调透明度、拖八向手柄、右键菜单逐项点；退出程序再启动，确认贴图原位恢复；托盘「从剪贴板贴图」。（E01~E05）
5. **长截图**：打开一个长网页，选中滚动区域 → 手动滚动一遍 → 完成；再试勾选自动滚动，换浏览器 / 记事本各一次（自动滚动在部分应用无效）。检查拼接缝、页眉页脚、剪贴板与保存文件。（D19）
6. **录屏**：托盘「录屏」→ 选区 → 看倒计时与控制条 → 暂停 / 继续 → 停止；MP4、GIF 各录一次，用播放器检查帧率、光标。副屏上再录一次。（F01~F03、F05）
7. **OCR**：先在无组件状态点 OCR，确认出现下载提示，按 D 下载；再用中文截图和英文截图各识别一次，对照面板文字与复制结果。切换 system / local-model 后端各试一次。（G01、G02）
8. **截图翻译**：缺模型时点翻译，确认提示清晰、不出现假译文；按说明放入模型后重试，检查对照面板与复制。（G05、G06）
9. **输入翻译浮窗**：绑定 `global_shortcuts/translate_input` → 按热键 → 浮窗位置、能否直接打字、Esc 关闭、下拉选模型是否误关窗、点击译文复制。（交接文档 §5.1）
10. **语音转文字**：按交接文档 §5.1 的未验清单逐项做（键入到真实应用 / IME / 管理员窗口，浮窗位置与不抢焦点，多屏 DPI，识别质量）。
11. **设置页**：逐个分组翻一遍，改一个会生效的键（如贴图边框色）和一个不会生效的键（如 `tray/icon`），确认差异符合 §2；改热键为已被占用的组合，确认提示并回滚。
12. **托盘与单实例**：托盘右键各项、双击；命令行 `snow-shot.exe --cmd screenshot` 在主实例运行时能唤起截图；`--cmd quit` 能退出。
13. **崩溃转储**（可选）：人为触发一次崩溃，到 `<数据根>\logs\crash` 找 minidump 与文本报告。

## 6. macOS / Linux（延后，不计缺口）

按 ADR-7，下列项不在当前缺口统计里：macOS 权限引导（`apppermissionservice.cpp`、`permissionguidecontroller.cpp`）、macOS 更新（`update/macosupdateservice.cpp`）、macOS 原生贴图窗口 / 采集（`platform/macos/*`）、Linux 的 portal 采集与 Wayland 覆盖窗、非 Windows 的单实例 / 托盘 / 热键后端（当前返回明确错误或占位）、macOS Vision OCR、macOS 系统翻译。`snow-capability` 的能力声明机制本身跨平台，已就位。

## 7. 与现有文档的出入（订正依据）

| 位置 | 原说法 | 实际 |
|---|---|---|
| 方案 P2 | 「P2 `snow-canvas-text` 完成」 | 文本组件完成；但 P2 整体里「15 种工具对齐」只做了 8 个，智能擦除未移植 |
| 方案 P3 | 「覆盖窗与主链路打通」 | 单屏 + GDI；智能选区、历史未做；导出（另存为 / 快速保存 / PNG·JPEG·BMP·WebP·PDF）与总线直接截图已于 2026-10-03 补齐，JXL / AVIF 仍缺 |
| 方案 P4 | 「贴图已完成」 | 核心交互与持久化为真；分组、穿透、缩略图、快捷键、管理页、文件贴图未做 |
| 方案 P5 | 「OCR / 翻译 / 拼接已完成」，写 `OfflineDictionaryEngine` | 该引擎已不存在；OCR / 翻译 / 拼接主链路为真；QR / 表格 / 转换 / 识别窗缺 |
| 方案 P6 | 「录屏已完成」，写 `ClickRipple` / `KeystrokeDisplay` 内置 | 这两个类型不在代码里；音频、特效缺；WebM 已移除 |
| 方案 P7 | 「设置页 238 项对齐、托盘与热键已完成」 | 设置页只有 40 项生效；托盘图标占位且菜单精简；热键只接 3 + 新增；MCP / 更新 / 网络 crate 仍是骨架 |
| 旧验收报告 | 「100% 交付验收」 | 见本表；已失实 |
| 交接文档 §5.1 | 「视频编辑系统引擎（Media Foundation）仍缺」 | 提交 `3027b635` 已加入；但主程序仍无入口 |
