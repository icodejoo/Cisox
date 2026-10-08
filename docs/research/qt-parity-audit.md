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
| A05 | 配置项被运行时实际读取 | （消费方散在各处） | 238 键里有 **40 个**在运行时代码被读（热键 3、贴图 10、录屏 7、截图 11、翻译 4、OCR 4、自定义模型 1）；另有 Rust 新增键（翻译路由、语音、OCR 后端）被读。其余 **198 个**能在设置页改、能落盘，但不改变任何行为。清单见 §2。2026-10-03 导出块新接线 9 个 `screenshot/*` 键 | 🟡 | 读码（检索）+ 推断 | **2026-10-08（晚）重新盘点，以本条为准**：口径改为「`schema_table.rs` 之外的 .rs 里没有该键的带引号字面量」（脚本：逐键 grep），早先的“78 个”口径不明，作废。当时是 50 个，其中 `global_mouse/*` 4 个由 `mouse_gesture.rs` 按前缀 `global_mouse/{action}` 读取，实际已消费，不算缺口。本轮新接线 5 个（都有离屏测试）：`screenshot_ui/color_picker_format`（取色格式，新增 `hex_without_hash` 变体，`ColorFormat::parse`）、`screenshot_ui/color_picker_display_mode`（`always_hide` 隐藏放大镜；`hide_outside_selection` 与 `always_show` 同 Qt 一样不额外处理）、`screenshot/selection_resize_mode`（`follow_mouse_position`：抓手柄时被抓的边直接落到鼠标像素，`grab_adjusted_rect`）、`pinned_history/compression_level`（贴图历史 PNG 压缩级别，`encode_png_with_level`）、`pin_to_screen/auto_resize_window`（关闭后贴图按原尺寸居中，`initial_pin_rect`）；另有 2 个（`drawing/spotlight_style`、`drawing/watermark_style`）由并行的聚光灯 / 水印工作接上，不是本轮的活。**剩余 39 个确实没有消费**（43 减去上面 4 个前缀读取的 `global_mouse/*`），按原因分类，都不是做假接线能解决的：①工具样式 / 工具栏分组未做：`drawing/{pen_filter_style, rectangle_filter_style, rectangle_highlight_style}`、`drawing/remember_last_used_tool`（`screenshot_toolbar/last_drawing_tool` 目前只在 normalize 里出现、从不写入，且矩形 / 椭圆共用一个工具栏项，没法无损还原）、`screenshot_toolbar/{arrow_line_tool, highlight_tool, last_filter_tool, last_highlight_tool, table_qr_tool}` 共 9；②主窗口 / 翻译页（A18、G07）：`extended_features/*` 3、`interface/{main_window_geometry, sidebar_collapsed, translation_window_size}` 3，另 `screenshot_conversion/vision_model`（G04 自定义模型通道）、`screenshot_translation/original_image_translation`（图上翻译）共 8；③采集后端与光标：`screenshot/{api_mode, window_element_api, capture_cursor, restore_original_screen_colors, capture_ui_in_scrolling_screenshot}` 5（Rust 只有 GDI / UIA 一条路径，没有 DXGI / WGC / MSAA 可选）；④录屏要先扩 snow-recorder 协议并在独立工作区验证：`screen_recording/{animated_image_clarity, clarity, encoder, encoding_preset, loop_animated_images, capture_toolbar_in_recording}` 6；⑤识别结果态要重做文案与状态：`screenshot/auto_execute_after_text_recognition`（现在识别后一律复制，改成可选需要“未复制”态与结束截图的关闭通路）、`text_recognition/{fill_style, model_hot_start, save_recognition_result_as_image}`、`pin_to_screen/text_selection_on_recognition_results` 共 5；⑥其它：`screenshot/confirm_before_exiting_via_shortcut`（要确认对话框）、`screenshot/shutter_sound_notification`（要系统提示音，没有对应封装）、`screenshot_ui/{area_type_hint_enabled, toolbar_size}`（工具栏缩放要改布局尺寸）、`system/launch_as_administrator`（要提权重启）、`updates/mode`（A09 只检查不下载）共 6。MCP / 更新 / 网络里骨架键本表不重复计。**以下是更早的复盘，数字已过时**：**2026-10-08 复盘（早些时候）**：按「配置键字面量在运行时代码里是否出现」重新盘点，238 键里只剩 **78 个**没有被引用（快捷键组由键位表按前缀读取，不在其中）。本轮新消费：`screenshot/double_click_action`、`middle_mouse_button_action`、`screenshot_ui/selection_border_color`、`selection_mask_color`、`selection_display_unit`、`system/application_priority`、`system/auto_start_at_boot`（开发构建不改注册表）、`tray/*` 6 键、`screen_recording` 特效 10 键、`screen_recording_shortcuts/*`、`pin_to_screen_shortcuts/*` 等。仍未消费的分类：截图 14（`capture_cursor`、`shutter_sound_notification`、`selection_resize_mode`、`auto_execute_after_text_recognition` 等）、`screenshot_ui` 8（`toolbar_size`、`color_picker_*`、引导线颜色等；`color_picker_format` 缺 `hex_without_hash` 变体）、`drawing` 9（样式模板 / 水印 / 聚光灯，依赖未做的工具）、`screenshot_toolbar` 8（工具栏布局）、`global_mouse` 7（C04 鼠标手势）、`screen_recording` 6（`clarity`、`encoder`、`encoding_preset` 等需扩协议）、`extended_features` 3 / `interface` 3（翻译页，G07）、`mcp` / `updates` / `network/proxy`（A12 / A09 / A10 骨架）、其余零散。
| A06 | 配置归档导出 / 导入 | `storage/configurationarchive.cpp` | 无 | ⬜ | 读码 |
| A07 | upstream 数据导入器（T9） | `storage/*` | 无。方案刻意延后 | ⬜ | 读码 |
| A08 | 开机自启 / 管理员启动 / 进程优先级 / 应用重启 | `platform/windows/autostartregistration.cpp`、`administratorlaunch.cpp`、`app/applicationrestart.cpp`、`presentation/settings/applicationpriority.cpp` | 无。`system/*` 三个键与托盘「重启」均未消费 | ⬜ | 读码 |
| A09 | 自动更新 | `update/updateservice.cpp`、`updateerrors.cpp`、`app/updateconfirmationdialog.cpp` | `snow-update` 只有 `PHASE` 常量和一个断言常量的测试。T4 刻意禁用 | 🟡 | 读码 **2026-10-08**：自动更新仍禁用（T4）；新增 `updates/manifest_url`（默认空，不硬编码端点），`snow-update::resolve_manifest_url` 未配置时给本地化“未配置更新地址”。**“检查更新”入口已做**：设置页「更新」分组顶部有当前版本行与按钮，后台线程用现有 curl 拉清单（限 64 KiB / 30 s），`snow-update::{parse_manifest, compare_versions, check_manifest}` 纯函数解析并比较版本（最小清单 `{"version","url","notes"}`，`version` 必填，数字段按数值比较、预发布后缀更小），结果本地化提示；纯函数与 `file://` 端到端离屏测过。**没做**：托盘 / 关于页入口（Qt 在关于页）、自动下载 / 校验 / 安装、更新说明展示。`snow-update` 为此新增对 `serde_json` 的依赖边（工作区已有，非新 crate） |
| A10 | HTTP 客户端 / 代理 / 云端 AI 接口 | `network/snowshotapiclient.cpp` | `snow-net` 只有 `PHASE` 常量。`network/proxy` 未消费。OpenAI 兼容客户端在 `snow-translate::openai`（仅翻译用） | 🟡 | 读码 **2026-10-08**：`network/proxy` 已消费——`snow-net::curl_proxy_args` 给现有 curl 下载追加 `--proxy`（none / 空不加，system 取环境变量代理）；**可填地址**：schema 改为自由文本，`snow-net::validate_proxy` 校验 `http` / `https` / `socks5` / `socks5h://主机[:端口]`（`none` / `system` / 空保留原语义，其余非法值在配置层被拒、在参数层不产生 `--proxy`），设置页按文本框输入；校验与参数构造均有单测。不引入 HTTP 客户端，云端 AI 客户端未做；代理认证只支持 URL 内联 `user:pw@`，未真机连代理验证 |
| A11 | 命令总线（取代 god object） | `presentation/services/screenshotcontroller.cpp`、`screenshotmcpcommands_p.h` | `snow-app-core::bus/command`；热键 / 托盘 / IPC 汇入。**只注册了 7 类 handler**：`Capture`、`StartRecording`、`PinSelection`（被解释成剪贴板贴图）、`OpenTranslateInput`、`Toggle/Start/StopDictation`，以及 2026-10-03 新增的 `Export`（作用于当前覆盖窗选区）、`DirectCapture`。其余 screenshot 域命令（选区、工具、撤销…）没有 handler，`emit` 会返回错误。27 项单测 | 🟡 | 读码 + 跑测 |
| A12 | MCP server（101 个 tool） | `app/mcp/*`（约 7.2k 行）、`mcp-capabilities.json`、`snow-crates` 的 `snow-shot-mcp` | `snow-mcp` 只有 `PHASE` 常量。`mcp/enabled` 未消费。`MCP_TOOL_MAP` 只是 28 个名字到命令种类的静态表，没有 server、没有 transport、没有描述符/token 文件 | 🟥 | 读码 **2026-10-08**：只出设计，见 [../design/mcp-subsystem.md](../design/mcp-subsystem.md)；不实现 |
| A13 | 平台能力注册 | `app/featureavailability.cpp` | `snow-capability`，主程序装载；托盘 / 热键服务按能力启动 | ✅ | 读码 |
| A14 | i18n 机制（Fluent、提取门禁、locale 自动发现） | `i18n/*.ts`、`services/languagemanager.cpp` | `snow-i18n`；en-US + zh-CN 完整；37 项测试；门禁命令见 AGENTS.md | ✅ | 读码 |
| A15 | UI 文案真正走 i18n | 同上 | 设置页、翻译输入浮窗、语音、Hy-MT2 面板走 `.ftl`。**覆盖窗提示、托盘菜单、贴图右键菜单、长截图视图、录制流程、`app_runtime` 里仍是写死的中文字面量**（`overlay_view` 约 86 行、`pinned_view` 55、`app_runtime` 68、`scroll_view` 28、`recording_flow` 15、`ocr_flow` 13、`translate_flow` 17，粗计，含日志行）。违反 AGENTS.md 的 i18n 规则；也导致英文界面下这些位置仍是中文 | 🟡 | 读码（检索） | **2026-10-08（晚）**：`ocr_client`、`ocr_download`、`ocr_assets`、`ort_runtime`、`ocr_flow`、`stt_download`、`translate_flow`（含 `translate_service` 的错误载荷）、`stitch_service`、`scroll_capture`、`scroll_view`、`pinned_shared`、`recording/*`、`recording_flow` 的用户可见文案与错误串已迁到 `.ftl`（新增 `fetch.ftl`、`translate_flow.ftl`、`scroll.ftl`、`pinned_errors.ftl`、`recording_ui.ftl`，en-US 与 zh-CN 齐全）；错误保持结构化（`FetchError` / `DownloadStep` / `OcrStep` / `ScrollHint` / `ScrollFailure` / `CopySkip` / `PinError` / `RecordingFailure`，`TranslateError` 新增 `NoCustomModel` / `CustomModelNotSelected`），在界面边界（覆盖窗、运行时事件、设置页回调）用 `message(&I18n)` 翻译，每个都有中英文测试。系统给出的原因、worker 报告的原文、日志里的技术信息不翻译，只作为 `{ $detail }` 带进去；原来夹在这些载荷里的中文（如 `no_model_message`、“功能未接入”）已改成英文技术描述。`ManifestIssue.reason` 来自 snow-translate 的扫描器，仍是中文，只出现在“未找到可用模型”的详情里。**早些时候的进展**：覆盖窗（`overlay_view`，底部提示与全部状态消息，新增 `overlay_messages.ftl`）与贴图窗口（`pinned_view`，右键菜单与状态提示，新增 `pinned_window.ftl`，`PinInteraction` 带界面语言）已全部走 `.ftl`，有测试（贴图右键菜单英文界面全是 ASCII 且无缺失 id）；剩余写死中文按文件计（粗计，含内部错误串）：`ocr_client` 23、`ocr_download` 17、`stitch_service` 17、`translate_flow` 17、`stt_download` 16、`scroll_view` 16、`pinned_shared` 15、`ocr_flow` 13、`recording/*` 约 27、`app_runtime` 6、其余零散。
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
| C02 | `global_shortcuts/*` 动作接线（21 个键） | `services/globalshortcutmanager.cpp` | 🟡（2026-10-08 订正）`global_shortcuts/*` 21 键里除 `translate_selected_text`（选中文字翻译，依赖选中文本读取 G 类能力）外均已接线：直接截图 / 延迟 / 贴图 / OCR / 翻译 / 复制、整屏与前台窗口、设置、历史、贴图管理、录屏目录、录屏并复制、贴选中文件、恢复最近关闭、暂停全部热键、前台全屏停用热键；另有 Rust 新增的 `translate_input`、`dictation_toggle/hold`。真机验证有限（见各动作所在行） | 🟡 | 读码 + 离屏测试 |
| C03 | 托盘图标与菜单 | `services/systemtraycontroller.cpp` | 🟡（2026-10-08）`tray_config.rs`：`tray/enabled`（关闭则不建托盘）、`tray/menu_options`（按列表显示菜单项，改动即时重建菜单；**默认只显示 Qt 默认的 12 项，之前多出的历史 / 重启 / 全屏停用等项需在设置里勾选**）、`tray/left_click_action` / `middle_click_action`（默认左键截图、中键贴图；左键不再弹菜单，右键弹菜单；`TraySpec` 新增中键动作）、`tray/icon` + `tray/custom_icon`（内置旧版应用图标 PNG，light / dark 为白 / 黑剪影，自定义路径读失败回退内置）。图标与点击动作的配置改动需重启生效（托盘服务只支持热更新菜单与提示）。未做：气泡通知、`snow-*` 图标变体与 `default` 的区别（旧版有多套图）、菜单文字外的主题联动 | 🟡 | 离屏测试 |
| C04 | 全局鼠标手势（`global_mouse` 7 键） | `services/globalmousegesture.cpp`、`globalmousemanager.cpp`、`platform/globalmousebackend.cpp` | 🟡（2026-10-08）`snow-platform::global_mouse`（手势状态机 `Gesture` 纯逻辑 + Windows 低级钩子线程）+ `snow-shot/mouse_gesture.rs`（读 `global_mouse/*` 7 键、动作 → 截图模式）：按住修饰键（默认 Win）再按住左 / 中 / 右 / 侧键拖动，开启一次截图并把拖动接成覆盖窗里的框选，松开时执行复制 / 贴图 / OCR / 翻译 / 另存 / 快速保存 / 录屏。性能策略同旧版：键盘钩子常驻只跟踪修饰键，**鼠标钩子只在修饰键恰好匹配绑定或手势进行中才安装**；有 Win 修饰时发一个无意义按键，避免松开 Win 弹开始菜单；没有任何绑定时一个钩子都不装；默认忽略注入输入。已验证：状态机 6 项离屏测试、键盘钩子修饰键跟踪与「匹配时才装鼠标钩子」（真机调试输出）。**未能验证**：这台机器上注入的鼠标事件到不了任何低级鼠标钩子（独立 Python ctypes 探针同样只收到键盘事件），所以真实 Win+拖动全流程没跑通，需要真人鼠标手动试；`snow-platform` 里有 `--ignored` 的真钩子测试可在能注入的环境跑。未做：拖动期间的框选可视化以覆盖窗打开后为准（覆盖窗打开前约 100~300ms 的移动在打开后补发）、macOS | 🟡 | 离屏测试 + 键盘钩子真机 |
| C05 | 前台全屏窗口时禁用热键 | `platform/focusedfullscreenwindow.cpp` | ✅（2026-10-07）`fullscreen_gate.rs`：开关写 `global_shortcuts/disable_on_focused_fullscreen_window`，托盘勾选，前台全屏窗口时丢弃热键命令（托盘 / IPC 与两个开关热键不受影响）；真机（全屏游戏 / 视频）未验证 | 🟡 | 离屏测试 |
| C06 | 覆盖窗内快捷键（`screenshot_shortcuts` 27 键 + `drawing_shortcuts` 10 键） | `overlay/screenshotoverlayshortcutcontroller.cpp`、`services/windowshortcutmanager.cpp` | 🟡（2026-10-08 订正）`overlay_keymap.rs` 按配置解析 `screenshot_shortcuts` 27 键 + `drawing_shortcuts` 10 键；已接线：取消 / 复制 / 另存 / 快速保存 / 贴图 / 录屏 / OCR / 翻译 / 长截图 / 撤销重做 / 复制颜色 / 移动工具 / 光标微移 / 工具键 / 智能选区目标切换 / 历史翻页 / 选回上次选区 / 重新截图（Alt+R）/ 坐标模式（Ctrl+P，写 `screenshot_ui/color_picker_coordinate_mode`）。按住类键（2026-10-08）：按住 Space（`move_entire_selection`）时框选拖动变成整体平移，松开键结束；按住 Shift（`keep_selection_width_and_height_consistent`，修饰键取自鼠标 / 键盘事件，配置若改成别的键则不再按 Shift 锁定）时新框选保持正方形、调整已有选区保持原宽高比。仍是「未实现」提示的：`table_recognition`、`qr_code_recognition`（G03 / G04）；橡皮擦 / 水印工具键未做 | 🟡 | 离屏测试 |

### D. 截图主链路

| ID | 功能 | C++ 位置 | Rust 现状 | 状态 | 依据 |
|---|---|---|---|---|---|
| D01 | 屏幕采集 | `capture/screenshotcapturecoordinator.cpp`、`screenshotcaptureworker.cpp`；`snow-crates/snow-capture` | `snow-platform::capture`，GDI 抓取；**2026-10-07 起所有显示器并行采集、每屏一个覆盖窗、共享虚拟桌面画布，选区可跨屏**（`CaptureCollector` / `DesktopFrames` / `OverlayWindowView`，方案见 [design/multi-display-overlay.md](../design/multi-display-overlay.md)）；不用 `snow-capture`（DXGI/WGC）；无光标采集、HDR、窗口排除。跨屏时录屏 / 长截图禁用；混合 DPI 与负原点副屏无真机验证 | 🟡 | 读码 |
| D02 | 冻结覆盖窗 | `overlay/screenshotoverlaywindow.cpp`、`screenshotoverlaycoordinator.cpp`、`screenshotoverlaypool.cpp` | `overlay_view.rs`（3212 行，53 项测试）；单显示器、不透明窗；无多屏跨屏选区；无窗口池（每次新建）。性能基准（4K 标注）有环境变量驱动，结果见台账 | 🟡 | 读码 |
| D03 | 选区交互（框选、八向手柄、拖动、边界限制） | `selection/screenshotselectiongeometry.cpp`、`core/screenshotgeometry.cpp` | `snow-ui-shell::selection`（46 项测试）+ overlay 状态机；右键先撤销选区再关闭 | ✅ | 读码 |
| D04 | 选区高级项（锁比例、上次选区、预设、圆角、阴影、尺寸弹窗、单位、过渡动画） | `selection/screenshotselectionresize*.cpp`、`screenshotselectionsettingsstore.cpp` | 无；`screenshot_selection/*` 8 键与 `screenshot_ui/*` 13 键全未消费 | ⬜ | 读码 |
| D05 | 智能元素 / 窗口选区 | `selector/*`；`snow-crates/snow-ui-selector`、`snow-visual-region-detector` | `snow-shot/window_pick.rs`（UIA 窗口 / 控件层级、滚轮与快捷键切换、后台细化、过渡动画，2026-10-07）；未接 `visual-region-detector`，多屏 / DPI 无真机验证 | 🟡 | 读码 + 单测 |
| D06 | 放大镜 / 取色 | `services/screenshotcolorpicker*.cpp`、`screenshotcanvascolorsampler*.cpp` | `snow-ui-widgets::magnifier` + 覆盖窗 `C` 复制光标处颜色；取色格式 / 坐标模式 / 辅助线键未消费；无独立取色窗 | 🟡 | 读码 |
| D07 | 延迟截图、固定区域、聚焦窗口、全屏直接截图 | `capture/directcapture*.cpp` | 总线 `DirectCapture` 已有 handler：当前显示器 / 焦点窗口 → 复制（可附带自动保存）或保存（指定路径 / 自动路径，格式 / 质量 / 压缩 / PDF 参数取请求，缺省读配置）；`render` 输出、缩放、光标采集未做；无对应全局热键、无延迟 / 固定区域；`screenshot/delay_seconds` 只在设置页 | 🟡 | 读码 + 跑测 |
| D08 | 标注工具集 | `tools/*`；`snow_draw_engine_qt/crates`（15 个引擎工具） | 🟡（2026-10-08 订正）工具栏 12 个：矩形、椭圆、箭头、直线、画笔、荧光笔（`PenHighlight`）、序号（`SerialNumber`）、文字、马赛克、模糊，另新增**橡皮**（`Eraser`，拖过的标注被擦除，可撤销）与**选对象**（`Select`，点选 / 移动已画标注），均有引擎层行为测试；覆盖窗绘制键 `drawing_shortcuts/select`、`eraser` 已接。聚光灯 / 水印装饰层 pass 已做（2026-10-08，`snow-canvas-raster::decoration`）：聚光灯整块压暗后 `DestinationOut` 一次性挖掉全部旋转矩形洞（并集、抗锯齿），水印按墨迹包围盒 + 间距 Pattern 平铺（奇数行错半步、绕画布中心旋转），装饰层叠在标注之上、水印在聚光灯之上；预览分块与导出共用 `render_region`（固定 256 网格，任意区域字节一致），只重画 `decoration.dirty_regions`；`AnnotationLayer::set_watermark` / `set_spotlight_style` / `apply_decoration_style` 按 `drawing/watermark_style`、`drawing/spotlight_style` 取样式（`decoration_style.rs`，范围夹取同旧版），离屏测试覆盖洞内外像素、洞内标注保留、增量与整屏逐字节一致、水印几何与覆盖量；未做：工具栏聚光灯 / 水印按钮与水印设置面板（`overlay_view` 仍提示未实现）、旧版黄金文件对照、DPR 下水印字号按画布像素而非逻辑像素。仍缺：自动滤镜（依赖未移植的智能擦除 D12）；矩形 / 画笔高亮与滤镜的细分变体 | 🟡 | 读码 + 引擎行为测试 |
| D09 | 工具样式面板（颜色、线宽、模板、最近使用） | `tools/screenshottoolpalette*.cpp`；`drawing/*` 16 键 | 🟡 最小闭环已做（见交接文档 §5.1「标注样式面板最小闭环」）：选中工具弹样式条（颜色预设 + 最近 8 色、线宽 / 字号 / 箭头头型、矩形椭圆填充开关），按工具记忆并持久化到 `drawing/*_style`；`screenshot_toolbar/*` 布局、样式模板、独立填充色仍未消费 | 🟡 | 读码 + 离屏测试 |
| D10 | 标注光栅化（patch → tiles） | `snow_draw_engine_qt/src`（QPainter） | `snow-canvas-raster`（tiny-skia，27 项测试：合成场景 + 引擎端到端 + 4K 耗时）；**无对 QPainter 输出的像素黄金比对** | 🟡 | 读码 |
| D11 | 滤镜内核（马赛克、模糊、反相、浮雕等） | `snow_canvas_filter_avx2.cpp`、`snow_canvas_pen_mask_avx2.cpp` | `snow-canvas-filters`；C++ 内核黄金样本逐字节对拍；本次跑通过 | ✅ | 读码 + 跑测 |
| D12 | 智能擦除 | `snow_canvas_smart_erase_algorithm.cpp`（约 880 行，依赖 OpenCV） | `smart_erase()` 恒返回 `false`，文件头自述未移植 | 🟥 | 读码 |
| D13 | 标注文本与 IME | `snow_draw_engine_qt/src/text/*` | `snow-canvas-text`（20 项测试）+ `CanvasTextInput` 接入覆盖窗与贴图 | ✅ | 读码；IME 手感见 §5 |
| D14 | 撤销 / 重做 | 引擎 | 经引擎 `undo/redo_with_viewport_changes`，覆盖窗与贴图均接线 | ✅ | 读码 |
| D15 | 导出：复制到剪贴板 | `services/screenshotclipboardservice.cpp`、`screenshotclipboardcontent.cpp` | `snow-platform::clipboard` 写位图；覆盖窗 Enter / Ctrl+C / 双击；总线 `Export(Copy)` 已接；`auto_save_after_copy` 已生效（复制后按自动保存规则再落盘一份，失败只记日志）；无「复制为文件」（`copy_image_file_to_clipboard` 只读入、未落到剪贴板文件格式） | 🟡 | 读码 + 跑测 |
| D16 | 导出：保存文件 | `services/screenshotsaveexportpipeline.cpp`、`screenshotimagefileservice.cpp`、`screenshotsaveasfiledialog.cpp` | 覆盖窗「保存」/ Ctrl+S = 系统另存为对话框（`snow-platform::file_dialog`，`IFileSaveDialog`，以覆盖窗为所有者，异步执行不占着界面借用；记住上次目录 / 格式并写回配置）；快速保存（复制后自动保存、贴图 / 长截图保存、直接截图）按 `image_save_directory` + `auto_save_filename_format`（默认含 `PRODUCT_NAME`）+ `image_format` 落盘，重名加 `_N`，目录不可用依次回退图片 / 文档目录；格式 PNG / JPEG / BMP / WebP / PDF，质量、压缩级别、PDF 页面读配置，总线 `Export(Save)` 的 path / automatic_path / format / quality / compression_level / pdf_page_size / pdf_title 覆盖已接。**未做**：JXL / AVIF（配置为这两种时回退 PNG 并记日志）、WebP 有损（`image` 只有无损编码器，质量 <100 仍输出无损）、自绘对话框（`save_as_file_dialog = snow_shot` 回退系统对话框）、保存路径快捷项、缩放比例导出、保存后写历史 | 🟡 | 读码 + 跑测；对话框外观待真机 |
| D17 | PDF 导出 | `services/screenshotpdfexport.cpp` | `export_pdf.rs`：单页 PDF 1.7，版式规则同旧版（96 dpi 换算、A4 纵 / 横居中等比、超大页用 `UserUnit`）；质量 100 为 Flate 无损（复用 PNG IDAT + PNG 预测器，带透明度时输出 SMask），<100 为 JPEG（DCT，白底合成）；标题缺省取文件名主干、至多 1024 字符。与旧版差异：整幅一张图（旧版分 2048 块，且无损用 qCompress）；创建时间不带时区。无 Qt 黄金文件可对照；**未用外部 PDF 阅读器打开核对** | 🟡 | 读码 + 跑测（结构 / 偏移 / 页面尺寸 / 质量） |
| D18 | 截图历史 | `services/screenshothistoryservice.cpp`、`capture/directcapturehistory.cpp` | `snow-history::capture_history`（仓储层）+ `snow-shot/history_store.rs`（写入线程、分页、缩略图）+ `history_view.rs`（历史页）+ `history_nav.rs`（覆盖窗翻页，2026-10-07）。覆盖窗导出（复制 / 保存 / 贴图）成功后写入完整现场：整帧底图、选区、标注历史 JSON（`AnnotationLayer::serialize_history`）、结果图；快捷键 `previous_screenshot_history` / `next_screenshot_history` 在覆盖窗内前后翻，右键先回到当前截图；读盘在后台线程，读取失败 / 底图尺寸不符的记录本次会话内跳过。未做：翻页读取中的“加载中”提示、跨显示器配置（底图尺寸不同）的记录、直接截图 / 长截图的整帧现场 | 🟡 | 读码 + 跑测 |
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
| E06 | 贴图快捷键（`pin_to_screen_shortcuts` 15 键） | `services/windowshortcutmanager.cpp` | 🟡 `pinned_keymap.rs` 已按 15 个配置键解析（2026-10-07）；已接线：复制、复制原图、另存、标注模式、关闭 / 销毁、方向键微移系统指针；点击穿透、缩略图、隐藏到顶部、识别结果、缩放到尺寸仍是占位（见 E07 / E10）；Ctrl+Z / Ctrl+Y 仍写死 | 🟡 | 离屏测试 |
| E07 | 点击穿透、缩略图模式、隐藏到顶部、方向键移动 | `pinned/screenshotpinnedclickthroughgeometry.cpp`、`hidetotopcontroller.cpp` | 🟡 缩略图模式（83 逻辑像素方块，鼠标锚点，无动画，右键菜单 + 键位）、点击穿透（`WS_EX_LAYERED/TRANSPARENT`，独立退出按钮小窗 `pinned_controls.rs`）已做（2026-10-07）；隐藏到顶部为简化版（30×6 把手小窗，悬停滑出、移开自动收回、点击复位；无滑入动画、多把手不避让、不持久化）；未做：穿透时的透明度 / 移动按钮、缩略图与穿透状态持久化、150ms 动画；方向键移动（指针微移）见 E06 | 🟡 | 离屏 + 原生样式测试 |
| E08 | 贴图分组与托盘分组菜单 | `services/pinnedwindowgroupmanager.cpp` | 🟡 `PinShared` 新增分组 API（新建 / 重名与长度校验 / 删除 / 删空分组 / 切换激活 / 移动贴图），已有记录的几何更新不再被挪进激活分组；托盘新增分组块（切换、新建、删除空分组，激活分组打勾）；贴图右键菜单有「移到分组」；切换分组 = 落盘并关闭当前分组窗口再恢复目标分组。未做：新建分组时输入名称（现在自动取「分组 N」）、改名、删除指定分组入口（见 E09 管理页）、托盘显示各分组窗口数 | 🟡 | 离屏测试 |
| E09 | 贴图管理页 | `components/pinnedwindowmanagementpagewidget.cpp` | 🟡 贴图管理窗口（`pinned_manage.rs` 模型 + `pinned_manage_view.rs` 视图，2026-10-07）：分组筛选条、列表（缩略图 / 分组 / 状态 / 尺寸 / 时间）、显示（必要时切换分组）、删除（确认气泡）、全部删除、按名称新建分组、删除空分组、删除当前筛选的分组；入口为托盘「贴图管理」与快捷动作 `open_pin_to_screen_management`。未做：来源 / 日期筛选、多选批量删除、分页（现为虚拟滚动，缩略图只解前 60 行）、改名；**窗口只有离屏模型测试，未真机渲染验证** | 🟡 | 离屏测试 |
| E10 | 贴图上识别文本、自动 OCR、复制原图 | `pin_to_screen/automatic_text_recognition` 等 | 🟡（2026-10-07）识别文字：`Ctrl+D` / 右键菜单切换文字框，点击文字框复制该段，菜单可复制全部文字；新建贴图按 `pin_to_screen/automatic_text_recognition` 自动识别（恢复的旧贴图不触发）；复制原图见 E06。识别沿用设置里的 OCR 后端，后台线程执行。未做：`text_selection_on_recognition_results`（框内选字，现为整段复制）、识别失败 / 组件缺失的下载引导、识别结果窗（G02）；真机未验证 | 🟡 | 离屏测试 |
| E11 | 从文件贴图、恢复最近关闭 | `services/screenshotfilepinbatch.cpp`、`platform/windows/selectedfiles.cpp`、`historypinplacement.cpp` | 🟡（2026-10-07）贴选中的文件：`snow-platform/selected_files.rs` 用 `IShellWindows` 读前台资源管理器 / 桌面的选中项（**真机验证过**：资源管理器选中 `notepad.exe` 读到正确路径），png / jpg / jpeg / bmp / gif / webp 逐张后台解码后贴出，一次最多 12 个，热键与托盘入口已接。恢复最近关闭：用户主动关闭的贴图把源图 + 标注会话 + 几何记在内存栈（最多 10 张、64MB），快捷动作 / 托盘「恢复最近关闭」后进先出恢复；淘汰和管理页删除不进栈，重启后栈清空。未做：多文件贴图的错位摆放、非图片文件（文本 / 文档）贴图、`HistoryPinPlacement`、关闭记录跨重启保留 | 🟡 | 离屏测试 + 真机（读选中项） |

### F. 录屏

| ID | 功能 | C++ 位置 | Rust 现状 | 状态 | 依据 |
|---|---|---|---|---|---|
| F01 | 录制独立进程 + 行协议 | `recording/screenrecordingcontroller.cpp`；`snow-crates/snow-screen-recorder` | `tools/snow-recorder`（独立 workspace）+ `snow-recorder-protocol`；`recording/client.rs` 拉起进程；录屏性能与硬件路径有实验台账 | ✅ | 读码；性能数据见台账 |
| F02 | 选区 → 倒计时 → 控制条（暂停 / 继续 / 停止 / 放弃） | `recording/screenrecordingareawindow.cpp`、`toolbarwindow.cpp`、`countdownoverlay.cpp` | `recording_flow` + `recording::area_view`；热键 / 托盘 / IPC / 覆盖窗工具栏四个入口 | ✅ | 读码；界面见 §5 |
| F03 | 输出格式 MP4 / GIF / APNG / WebP | `screenrecordingfolder.cpp` | 已接线；输出目录与文件名模板读配置 | ✅ | 读码 |
| F04 | WebM | 同上 | 刻意移除，见 `cisox-todo-webm.md` | ⬜ | 读码 |
| F05 | 硬件编码（Auto：MF → 厂商硬编 → 软编） | （C++ 侧走 snow-crates） | worker 内实现，默认 `Auto` | ✅ | 读码 |
| F06 | 跨屏选区录制 | `recording/screenrecordinggeometry.cpp` | worker 的跨屏拼接已实现（`win/span.rs`）；**覆盖窗只能在单显示器内选区**，所以界面上选不出跨屏区域 | 🟡 | 读码 |
| F07 | 音频（系统声 / 麦克风） | `snow-crates/snow-audio-recorder` | 🟡 阶段 1 已做（2026-10-04，见交接文档 §5.1「录屏音频阶段 1」）：MP4 单轨混音（系统声 WASAPI loopback + 麦克风），协议 `mic=`/`sys=`/`mvol=`/`svol=`/`mdev=`/`sdev=` 与 `AUDIO_STATE` 事件，控制条显示降级提示；非 MP4 强制关闭。未做：设备选择与音量滑块界面、双轨、录制工具栏上的开关、麦克风真实内容验证 | 🟡 | 真屏自检（三条路径音视频时长差 ≤0.05s） |
| F08 | 鼠标点击 / 轨迹高亮、按键回显 | `recording/recordingeffect*.cpp`；`snow-crates/snow-recording-effects` | 🟡（2026-10-08）协议新增 `EffectsRequest`（`trail=`/`trms=`/`click=`/`hl=`/`clicks=`/`keys=`/`ksize=`/`kbg=`/`kfg=` 前缀令牌，全关时 START 行字节不变），主程序 `recording/effects.rs` 读 `screen_recording/*` 的 10 个特效键；任一特效打开时 worker 不走自建硬件流水线、改走上游软件路径（它叠加点击波纹 / 轨迹 / 高亮 / 按键回显）。**真机验证**：带全部特效录 4 秒 MP4，抽帧可见红色轨迹、光标高亮圈（正片叠底）、右下角堆叠的键帽。未做：硬件流水线上的特效（开特效即放弃硬件编码，CPU 占用更高）、键帽字体与标签自定义、点击波纹未逐帧核对 | 🟡 | 离屏测试 + 真机录制抽帧 |
| F09 | 录屏快捷键、复制到剪贴板、打开录屏目录 | `recording/screenrecordingshortcutcontroller.cpp` | 🟡（2026-10-08）`open_screen_recording_folder` 早已接线；新增：控制条键位表 `recording/keymap.rs`（`screen_recording_shortcuts/*` 4 键：导出 / 暂停继续 / 复制 / 结束，只在控制条有焦点时生效，不注册全局热键以免录屏期间劫持其它程序的 Ctrl+C / Esc；录制中的 Esc 不响应，避免误丢录像）；`screen_record_copy`（录屏并复制）= 开始录屏、完成后把录制文件以文件列表写入剪贴板（`snow_platform::clipboard::copy_files_to_clipboard`，**真机验证过** PowerShell 读回 `C:\Windows
otepad.exe`）。未验证：控制条窗口能否真正拿到键盘焦点（覆盖窗不抢焦点，点击控制条是否激活待真机确认）；未做：录制工具栏上的复制按钮 | 🟡 | 离屏测试 + 剪贴板真机 |
| F10 | 录屏设置项（清晰度、编码器、预设、循环、特效颜色等） | `screen_recording/*` 25 键 | 7 个被读取（帧率、格式、光标、延迟、目录、文件名、动图帧率）；其余 18 个未消费 | 🟡 | 读码 |

### G. OCR / 识别 / 翻译

| ID | 功能 | C++ 位置 | Rust 现状 | 状态 | 依据 |
|---|---|---|---|---|---|
| G01 | OCR 引擎（local-model，独立进程） | `ocr/screenshotocrrecognitionservice.cpp`、`screenshotocrassets.cpp`；`snow-crates/snow-ocr-process` | `ocr_client`（协议 v4 客户端）、`ocr_service`、`ocr_assets`、`ocr_download`、`ort_runtime`；常驻、模型档位、DirectML、缩放策略读配置；协议 / 路径 / 失败路径有测试 | ✅ | 读码；端到端见 §5 |
| G02 | OCR 结果呈现与编辑 | `ocr/screenshotocrvisuals.cpp`、`screenshotrecognitionwindow.cpp`、`screenshotocrtexteditingsession.cpp` | 🟡（2026-10-08）覆盖窗内面板（最多 8 行、每行 56 字，框线叠加，Enter 复制全文）之外，新增识别结果窗 `recognition_view.rs`：OCR 完成后按 `E` 打开，左图叠文字块框（点框复制该段）、右侧可编辑全文、复制全文；窗口自带数据，覆盖窗关闭后仍在。未做：按版面重建 / 填充还原（`fill_style`）、保存带识别结果的图片（`save_recognition_result_as_image`）、`model_hot_start`；窗口只有离屏逻辑测试，**未真机渲染验证** | 🟡 | 离屏测试 |
| G03 | 二维码识别 | `ocr/screenshotqrcontroller.cpp`、`screenshotqrrecognitionservice.cpp` | 无；`auto_recognize_qr_code` 未消费 | 🟡 | 读码 **2026-10-08**：已做（`qr_decode.rs` 用 `rqrr` 解码，覆盖窗 `qr_code_recognition` 键接线，结果复制并用 OCR 结果面板展示；离屏单测）。**打开链接已做**：内容含 `http` / `https` 链接时面板多一行提示，按 O 经 `snow_platform::shell::open_url`（`explorer.exe <url>`，先过 `web_link` 校验，拒绝其它协议 / 空白 / 控制字符 / 超长）用默认浏览器打开并关闭覆盖窗，失败留在覆盖窗并提示；覆盖窗接线（识别 → 复制 → 链接 → O 键 → 输出通道）有离屏测试，用真实样本二维码。**没做**：鼠标点击打开（只有键盘）、`auto_recognize_qr_code` 仍未消费、没真机开浏览器验证 |
| G04 | 表格识别、LaTeX、Markdown / HTML 转换 | `ocr/screenshottable*.cpp`、`screenshotimageconversion*.cpp` | 无；连 ADR-5 要求的「未配置时引导卡片」也没有；工具栏 ID 只存在于配置常量 | ⬜ | 读码 **决策（2026-10-08）：按 ADR-5 做未配置引导卡片，模型来源待用户定；不捆模型，当前不做** |
| G05 | 截图翻译（OCR → 翻译 → 展示） | `ocr/*`、`app/translationservice.cpp` | 覆盖窗 `Translate` 动作：OCR → 本地路由翻译 → 原文 / 译文对照面板 → Enter 复制译文；缺运行时 / 模型有分级提示与下载入口。**不是把译文画回原图**（`original_image_translation` 未消费） | 🟡 | 读码 |
| G06 | 本地翻译引擎与路由 | 无（Qt 版走云端） | `snow-translate` + `tools/snow-translator`（NLLB / Hy-MT2、路由、分段、全角标点后处理）；113 项测试 | ✅ | 读码；真实模型见 §5 |
| G07 | 翻译页、独立翻译窗 | `components/translationpagewidget.cpp`、`standalonetranslationwindow.cpp` | 无翻译页；Rust 新增的输入翻译浮窗覆盖「输入文字 → 译文」这一条 | ⬜ | 读码 |
| G08 | 选中文字翻译 | `services/selectedtexttranslation*.cpp`；`snow-crates/snow-selected-text` | 🟡（2026-10-08）热键 / 托盘 `translate_selected_text`：后台线程用 `snow-selected-text`（路径依赖，Apache-2.0，Windows）读前台选中文字（无障碍优先、必要时复制回退并还原剪贴板，2 秒超时，排除自身进程），再打开输入翻译浮窗预填并自动翻译；没读到给本地化提示。**真机验证**：记事本全选后读到整句中英文。未做：浮窗定位到选区附近、`selected_text_translation` 相关设置键 | 🟡 | 真机读取 + 离屏测试 |
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

**非刻意的缺口**（方案没说不做，只是没做完或没接上；~~D05 智能选区~~、~~D18 历史界面~~、~~D01 多屏~~ 已于 2026-10-07 做完，D01 仍缺 DXGI 采集）：D08 剩余 7 个标注工具、D09 样式面板、D01 DXGI 采集、E06~E11 贴图后半、F07 / F08 录屏音频与特效、C02 的 18 个热键、C03 托盘细节、G02~G04 识别结果窗 / QR / 表格、A05 的 39 个未消费键（分类见 A05 行）、A12 MCP。

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

1. **设置页大面积不生效**（A05、B05）：2026-10-08 复盘后只剩 39 / 238 个键只落盘不改行为（见 A05 行），用户改了没反应。截图保存格式、历史、工具样式、选区外观、托盘、快捷键等全在此列。
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
