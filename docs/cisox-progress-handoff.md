# Cisox 进度交接（2026-10-03 暂停点；2026-10-13 最新收口见 §0.-1，上一轮 2026-10-09 见 §0.0）

> 本文记录 2026-10-03 暂停时的真实状态与续做入口。**续做先读 §0.-4（宿主与 worker 审查修复）、§0.-3（2026-10-20 UI 审查）与 §0.-2（截图翻译贴图接手）；双进程状态见 §0.-1（2026-10-13 收口）**。方案主文档：`docs/cisox-gpui-migration-plan.md`。
> **功能对齐程度以 [`research/qt-parity-audit.md`](research/qt-parity-audit.md)（2026-10-03，读代码得出）为准**；方案 P2~P7 与旧验收报告里的「已完成」没按现状重核，别直接引用。
> 最新待办清单在 §5.1；§5 及之前的清单是 09-29 旧稿，仍有效的条目已在 §5 开头标出。
> 进程模型（托盘宿主 + 按需 UI：两个 exe、启动开关、构建、日志位置、验收脚本；**2026-10-13 已切默认为双进程；阶段 0~4、6 完成，阶段 5（冷启动优化）未执行**）见 §6.2。

## 0.-4 2026-10-20 宿主与 worker 审查修复

> 来源：2026-10-20 按 exe 逐个做的代码审查（cisox-ui / cisox / recorder / stt / translator / table / latex 共七轮），主会话复核后全部修复。UI 侧见 §0.-3。

- **宿主 `cisox.exe`**：会话管道改为有界异步写队列（`mailbox.rs`），手势更新合并，写失败进入会话故障处理，`--cmd quit` 优先；无窗口配置任务限时 30 秒，超时终止回收，暂存命令队列有容量、合并和过期；托盘菜单展开时窗口消息调用 `EndMenu` 退出；UI 子进程登记前由清理守卫持有，任一步失败都终止回收；首次有效快照到达后一次性恢复登录贴图；全屏闸门放行已放行按下的配对松键；重启改为宿主自身 `--restart-after` 等旧进程退出后直接启动，不再经 cmd / ping。
- **录制 `cisox-recorder.exe`**：MF 编码保留末帧并按停止终点定时长（静止尾段不再提前结束）；后台编码 / 采集故障及时回报 Error；暂停恢复重新抓完整桌面（跨屏等全部首帧）；采集队列有界、光标更新取最新值；软编复用设备选择与两路音量；音频写入失败回报 Lost 并保留首个错误；光标缓存限容 FIFO。软编停止时无损合并音轨有临时磁盘与收尾 I/O 开销；软编音画同步原点为会话就绪时刻，精确同步待实测。
- **`cisox-stt.exe`**：在途 PCM 限 30 秒，过载立即拒收并报错；STOP 冻结采样、释放采集，收尾轮询并持续输出结果、期间响应 CANCEL / EOF；STOP 积压按 `max_seconds` 剩余额度裁剪。sherpa 单次解码仍不可中断。
- **`cisox-translator.exe`**：超长单句按 token 偏移切分并在推理前再校验；独立读取 stdin，EOF / 取消贯穿分段、解码和 ORT Run（`cancellation.rs`）；Seq2Seq 加载期校验 logits、运行期缺失返回结构化错误；相对 DLL 路径检查后固定为规范化绝对路径。
- **`cisox-table.exe` / `cisox-latex.exe`**：table 校验输入数量 / 类型 / 形状，不再直接索引；latex 新编码开始即清空旧上下文，失败后禁止解码。
- 验证（各自独立 target 目录）：宿主库 164、shell-core 25、workspace-guard 24+6；recorder 脚本测试 208（2 项基准忽略）；STT 94（1 忽略）、translator 108、table 1、latex 1；相关 clippy / fmt / `git diff --check` 通过。
- 未覆盖：真屏托盘菜单退出与宿主重启、PE 导入表验收、真实多屏暂停恢复与音画同步、真实麦克风与慢模型的中断效果。

## 0.-3 2026-10-20 UI 侧审查修复

- 贴图跨 DPI 时保持图像物理位置，统一重算操作条外框、图像偏移、原生命中区域和拖动基准；新增含负坐标屏的连续往返测试。
- 翻译请求持有引擎 Arc 与只读路由策略快照，仍复用模型池；取消沿分段、包池等待、本地 worker 加载、管道读写和分块传播；OpenAI 兼容后端的段间请求与本地 curl 等待也响应取消。非致命首错也启动空闲回收；分块预算改按 JSON 转义后长度计算，单段超限明确拒绝。
- 翻译贴图保留结构化错误：失败时共享 `pin_action_bar` 中的撤销 / 复制位置分别提供重试 / 下载（后者按错误能力启用），图标 + 双语 tooltip；重试保持原图与原目标语言。最终排版、GDI 光栅化与像素转换在后台完成，UI 只安装图像；无可绘制内容时返还原像素缓冲，回退不再整图克隆。
- OCR / 表格 / 公式沿用应用会话播种序号，拒收上一截图的迟到结果；翻译原有会话播种和 serial ↔ pin 映射已存在，保留。
- 开始听写即绑定前台窗口、进程、原生焦点及 UIA RuntimeId，每批注入前复核；切窗或同窗切控件时停止键入并沿用目标丢失提示。UIA 身份无法确定时不向未知控件注入。
- 表格属性缺少 `>` 时返回结构错误，不能吞掉后续格 / 行；新增跨度、行列和文本导出的固定 token 黄金样例。公式解码必须遇到 EOS，长度耗尽返回双语错误；总期限为加载预算 + 推理预算（当前合计 120 秒），读写均可取消，退出识别界面或关闭截图时回收 worker；stderr 只保存固定容量尾部。补固定公式图 + 脚本 token 的精确输出断言。
- STT worker 与 UI 共用稳定 `[worker:<code>] <参数>` 错误编码，麦克风 / WAV 故障按码本地化，其他诊断只写日志。未涉及本轮之外的 STT 问题。
- 已检查删除的 `result_bar.rs` 没有代码悬空引用；`calculate_toolbar_placement` 仍有录屏 / 长图的有效调用，与前节文档一致。
- 本轮改动文件（不包含会话开始前已有、且本轮未编辑的文件）：
  - `crates/snow-shot/src/`：`app_runtime.rs`、`overlay_view.rs`、`pinned_view.rs`、`pinned_manager.rs`、`pinned_shared.rs`、`frozen_frame.rs`、`ocr_client.rs`、`table_structure.rs`、`latex_ocr.rs`、`latex_service.rs`、`translate_render.rs`、`translate_service.rs`、`dictation/{engine,flow,status,typing}.rs`。
  - `crates/snow-translate/src/{lib,router,worker,openai}.rs`、`crates/snow-platform/src/focus_probe.rs`、`crates/snow-stt-protocol/src/lib.rs`、`tools/snow-stt/src/{main,source}.rs`。
  - `crates/snow-i18n/locales/{en-US,zh-CN}/{pinned_window,dictation,latex_recognition}.ftl`；本交接文档、`docs/README.md`、`docs/guides/snow-stt-worker.md`。以上 crate / tools 路径均相对 `snow-shot-rs/`；`AGENTS.md`、共享操作栏和 Qt/C++ 未被本轮编辑。
- 本轮全部 Cargo 命令固定 `CARGO_TARGET_DIR=C:\cargo-target\cisox-review`，保留已有未提交改动；不增依赖、不改 Qt/C++，不提交 / stash / checkout / reset，不启动或停止产品进程。
- 最终验证：`cargo test -p snow-shot` 按 `latex_`（34 通过 / 1 忽略）、`table_`（36 / 1）、`pinned`（89 / 1）、`translat`（231）、`dictation`（133 / 1）、`overlay`（194）、`action_bar`（20）过滤通过；析构兜底和翻译预热取消改动后分别复跑受影响的 `overlay` / `dictation` / `translat` 组通过。各过滤组有重叠，不相加作总用例数。
- `snow-translate` 初轮完整 crate 测试通过；最终 `worker::tests`（42）、`router::tests`（37）、`openai::tests`（12）通过。`snow-stt-protocol`（21 单测 + 10 文档测试）、`snow-i18n`（38 单测 / 集成测试 + 11 文档测试）、`workspace-guard`（24 单测 + 6 文档测试）通过；`snow-platform com::tests`（1）通过，`focus_probe` 过滤无匹配用例，真实 UIA 查询未验。独立 `snow-stt` 87 通过 / 1 个真实模型测试忽略；最后对更新后的 WAV 错误码断言单独复跑通过。
- `cargo clippy -p <crate> --all-targets -- -D warnings` 对 `snow-shot`、`snow-translate`、`snow-platform`、`snow-stt-protocol`、`snow-i18n`、独立 `snow-stt` 均通过；主 workspace 受影响包及独立 STT 的 `cargo fmt -- --check` 通过。i18n `check` 与 `extract crates --exclude crates/snow-i18n --strict-refs` 通过（398 引用、缺失 0），`git diff --check` 通过。未跑 workspace 全量测试。
- 验证环境：STT 首次测试因隔离目录缺少匹配 DLL 报 `0xc000007b`，随后将已有 sherpa C API 与 ORT 1.28 DLL 复制到 `cisox-review/debug/deps` 后复跑通过；没有改运行目录或运行中的 exe。编译期间的接口 / 死代码检查失败均已修正并复跑相关检查。
- 未覆盖：真实跨 DPI 桌面交互、UIA 提供方稳定性 / 延迟、实际输入注入、下载 / 剪贴板端到端、真实模型识别精度及无响应 worker 的系统级回收冒烟。离线公式样例验证流水线输出，不代表真实模型质量；本轮没有新增 Qt 模块移植，也没有新增 C++ 黄金样本。

## 0.-2 2026-10-20 截图翻译贴图接手（含 10-19 浮层调整）

- 点击截图翻译后，在 `TranslateRequested` 处理时立即复用 `PinnedManager` / `PinnedWindowView` 创建原图临时贴图并关闭全部截图覆盖窗；后台 OCR / 翻译由贴图接手，成功前不等待覆盖窗、不进入翻译详情窗口。独立输入翻译与听写翻译入口保留。
- 复用现有 OCR 排版与译文原位渲染，共用函数 `translate_render::build_translated_rgba` 不依赖覆盖窗，覆盖窗基准路径与贴图均调用它；完成时替换对应贴图的图像。图片保持原选区的桌面物理坐标与尺寸，含负原点副屏。操作按钮为保存原图、撤销、复制译文、关闭，使用组件库 Small 图标 Button + tooltip；图标经 `snow_ui::ui` 现有 antd 资源门面加载，提示沿用 en-US / zh-CN 的四个 `translation-pin-*` 键，不显示按钮文字。
- 翻译期间以现有 `scan_overlay` 扫描图像区域，不遮按钮条、不拦截鼠标；可拖动、保存原图和关闭，撤销 / 复制译文在结果出现前禁用。成功、失败、无文字或取消均停止扫描；失败提示复用翻译错误文案，空译文用新增双语状态文案，通过贴图 badge 显示，保持原图。
- 保存原图取翻译提交时的截图副本；撤销释放译文图，仅保留原截图浮层，撤销与复制译文按钮禁用。浮层关闭即结束，不进入普通贴图持久化 / 自动恢复。
- `AppState::translation_pins` 登记 serial → 贴图 ID，完成后消费路由；关闭贴图删除路由并调用 `translate_cancel.cancel(serial)`。取消槽支持多个独立请求；截图会话序号占高 32 位，避免后一次截图收到前一次的迟到结果。已关闭或已完成的贴图也拒收结果。
- 覆盖窗只移交本次原图并退出等待态，关闭覆盖窗不会取消已移交任务；旧状态机 / 详情窗保留给 `SNOW_OVERLAY_BENCH_TRANSLATE_AUTO` 等覆盖窗内等待路径。
- 翻译贴图与普通贴图共用 `render`、识别边框、悬停高亮 / 光标、窗口级拖动与 Esc 关闭；翻译图像只允许整体移动（内部操作条另有抓手可移动条），禁用缩放、透明度、标注等普通交互，移动仍跳过持久化。
- 操作条封装为独立 `pin_action_bar.rs` 组件，接收按钮定义（图标、tooltip、禁用状态、点击回调）、放置模式与逻辑尺寸，统一使用组件库 Small 图标 Button；`pinned_view.rs` 只装配业务按钮、回调与布局输入。按钮（含禁用按钮）拦截左 / 右 / 中键按下，条空白区域继续冒泡给根节点拖窗。按钮宽 / 高均为 24px、间距 4px、条高由实际按钮高度 + 上下各 4px 内边距计算（当前 32px）、抓手宽 24px、两侧内边距各 4px，条宽由按钮数和模式统一计算：四按钮外部条 116px，内部含抓手为 144px；翻译完成前撤销 / 复制继续禁用。
- `Inside` 模式条首位显示 Lucide `grip-vertical` 图标 Button，提示使用双语 `translation-pin-move-bar`；该图标经 `snow_ui` 资源门面从现有库按需嵌入。抓手仅移动操作条，不改变图像 / 窗口；`Below / Above` 不显示抓手且固定位置。`TranslationPin::bar_state` 保存相对选区左上角的逻辑偏移和抓手拖动起点，重绘复用状态；默认靠右贴底，移动图像或条空白使整窗移动时相对偏移保持。
- 纯函数 `clamp_bar_offset` 接收选区尺寸、条尺寸、当前逻辑偏移与拖动增量，按四边 4px 边距夹紧；空间不足时按轴缩减边距，条比选区大时该轴偏移退化为 0，由窗口裁剪，保持选区 / 窗口尺寸不变。抓手移动按下点到当前鼠标的总增量计算，避免边界处往返拖动累积误差；松开及按键丢失结束抓手拖动。
- `pinned_manager::translation_bounds` 为纯函数：输入选区、条逻辑尺寸、所在显示器物理 `bounds` 与 scale，输出 `Below / Above / Inside` 和窗口物理外框。优先选区正下方，下方不足则试正上方，上下都不足才默认在选区内部底部靠右悬浮（可用抓手调整相对位置）。外部布局按条高扩展窗口，窄选区必要时向左扩宽，上方布局同时上移窗口；图像通过窗口内偏移保持原屏幕坐标与尺寸。内部模式外框严格等于选区，不扩窗。显示器按选区最大交集选取，负原点与混合 DPI 统一在物理坐标中判断。
- 10-19 原调整离线验证：新增 6 项单测；翻译相关 178 项、贴图相关 68 项、i18n 测试、架构守卫，以及相关 crate clippy、fmt、i18n check / strict-refs 通过。未做真屏按钮点击、剪贴板 / 保存落盘端到端、多屏混合 DPI 观感验收。
- 10-19 共用渲染返工验证：`cargo fmt --all` / `--check`、`cargo clippy -p snow-shot --all-targets -- -D warnings`、贴图过滤测试（72 通过 / 1 个既有发布检查忽略）、i18n check / strict-refs（403 引用，缺失 0）、`workspace-guard`（24 项）通过。新增离线测试覆盖翻译图像 / 条空白拖动与不持久化、所有八向缩放及滚轮缩放 / 透明度禁用、移动后撤销几何不变，以及普通贴图共用入口回归。真屏按钮事件冒泡、边框观感、Esc、多屏混合 DPI 未验。
- 10-20 贴图接手验证：`cargo fmt --all` / `--check`、`cargo clippy -p snow-shot --all-targets -- -D warnings` 通过；`cargo test -p snow-shot pinned`（75 通过，1 个既有发布检查忽略）、`translat`（213 通过）、`overlay`（184 通过），i18n check / strict-refs（406 引用，缺失 0），`workspace-guard`（24 单测 + 6 文档测试）通过。离线覆盖提交即产生原图贴图数据、完成换图停扫、失败 / 无文字 / 取消保留原图和状态、关闭取消、迟到结果与跨会话隔离。未跑 workspace 全量；`CARGO_TARGET_DIR` 保持 `C:\cargo-target\cisox`。未做真屏扫描裁剪 / 拖动 / 按钮点击、保存 / 剪贴板端到端、多屏混合 DPI 和基准脚本实跑；本次仅调整 Rust UI 生命周期并复用既有渲染器，没有移植新模块。
- 10-20 按钮条位置 / 图标调整验证：`cargo fmt --all` / `--check`、`cargo clippy -p snow-shot --all-targets -- -D warnings` 通过；`cargo test -p snow-shot pinned`（80 通过，1 个既有发布检查忽略）、`translat`（218 通过），i18n check / strict-refs（406 引用，缺失 0）、`cargo test -p workspace-guard`（24 单测 + 6 文档测试）通过。位置单测覆盖下方优先、上方回退、内部外框等于选区、右下角、负坐标与 1.5 倍 DPI；视图离屏测试覆盖三种布局安装后图像屏幕位置不变、空白拖动、撤销不改几何 / 不持久化，以及四个图标资源存在。未做真屏按钮 tooltip / 点击冒泡、内部条空白拖动与混合 DPI 多屏观感验收，保存 / 剪贴板端到端仍未覆盖；未跑 workspace 全量，未改 `CARGO_TARGET_DIR`（`C:\cargo-target\cisox`），没有移植新模块。
- 10-20 独立操作条 / 内部抓手验证：新增 4 项离屏单测并扩展三模式整窗移动回归，覆盖四边夹紧、条大于选区 / 零尺寸退化、拖动偏移更新及边界往返、默认靠右贴底、整窗移动后相对偏移不变、仅 Inside 暴露且接受抓手、按模式计算宽度和抓手资源可加载。`cargo fmt --all` / `--check`、`cargo clippy -p snow-shot --all-targets -- -D warnings` 通过；`cargo test -p snow-shot pinned`（84 通过，1 个既有发布检查忽略）、`translat`（218 通过），i18n check / strict-refs（407 引用，缺失 0），`cargo test -p workspace-guard`（24 单测 + 6 文档测试）通过。未做真屏抓手 / 按钮事件冒泡、tooltip、窄选区裁剪观感、跨屏混合 DPI，以及保存 / 剪贴板端到端验收。未跑 workspace 全量，`CARGO_TARGET_DIR` 保持 `C:\cargo-target\cisox`；无新增依赖、无新 Qt 模块移植、未修改 Qt/C++，未提交。
- **10-20 原有翻译操作栏统一迁移**：在上述未提交改动之上继续改，保留截图翻译由贴图接手的生命周期；本轮只整理 UI 组件与离屏验证。
  - 覆盖窗 `translate_bar` 五动作（详情 / 撤销 / 保存译文图 / 保存原图 / 关闭）及 `result_panel` 三动作（确定 / 撤销 / 关闭）均改用 `pin_action_bar`。原位详情面板只有两列联动文字、没有另一排按钮；其交互不变。
  - `recognition_view`（翻译详情 / 识别结果共用窗）底部复制全文 / 关闭、格式复制与转换引导行全部复用组件。共用窗的 OCR / 表格 / 公式入口一并使用同一渲染路径。
  - `translate_page_view` 独立窗和设置内嵌页的翻译 / 复制 / 复制并关闭动作行迁移，内嵌仍不显示关闭动作；交换与清空历史的独立图标按钮也复用组件。提交忙碌动画、复制禁用条件和键盘入口保留。
  - `translate_input_view` 关闭 / 清空 / 交换 / 复制四个独立动作全部复用组件的按钮入口，删除旧 `icon_button`，交换图标保留 90° 旋转。`dictation/view`（含听写翻译）底部复制 / 关闭改用共用条，空文本时仍允许复制。
  - 原有翻译贴图四动作继续使用该组件；组件新增普通流布局、覆盖窗浮动布局、可离屏检查 / 分发的 `ActionSpec<A>` 与泛型宿主实体绑定，不依赖 `PinnedWindowView`。所有动作仅显示图标 + 本地化 tooltip，图标沿用 `snow_ui::ui` 现有资源门面，不加依赖或图标包。
  - 空白行为显式参数化：贴图条空白继续冒泡给父节点拖窗，覆盖窗条空白拦截左 / 中 / 右键，避免重新框选；按钮（含禁用态）统一隔离事件。内部抓手只移动条，偏移随宿主重绘保留，新的翻译状态重置偏移；超窄选区按宿主选区裁剪。
  - **位置策略变化**：覆盖窗原条按“选区右侧外缘 → 内部右下 → 避让主工具栏”放置；现统一为选区下方优先、不足回退上方、上下均不足才内部悬浮并显示抓手，不再采用旧的主工具栏避让位置策略。贴图窗口扩框逻辑复用同一个外部优先决策。
  - **本步骤当时未迁移的部分（截图主栏现已由下文补齐）**：翻译页 / 输入浮窗的语言与模型下拉、文本输入和标题拖动区是输入 / 选择 / 宿主布局，保留 gpui-component 控件及焦点行为；翻译历史记录按钮显示记录内容，是记录列表而非图标操作栏。原位详情两列文字没有按钮条。设置页翻译 / 视觉模型下拉后的“获取”属于模型配置表单的导航动作，保留原控件。纯 OCR 浮窗、录屏与非翻译设置操作不在本任务范围。
  - 删除无调用者的 `snow-ui-widgets/src/result_bar.rs`（旧渲染、交互、尺寸与定位算法及旧策略测试）和门面导出，删除旧按钮间距 / 图标尺寸常量。所有旧按钮文案均继续用作 tooltip，没有因本轮迁移而废弃的 `.ftl` 键；双语 catalog 的已有未提交新增内容保留。
  - 本轮改动文件：`snow-shot/src/{pin_action_bar,overlay_view,pinned_manager,pinned_view,recognition_view,translate_input_view,translate_page_view}.rs`、`snow-shot/src/dictation/view.rs`、`snow-ui-widgets/src/lib.rs`、删除 `snow-ui-widgets/src/result_bar.rs`，以及本节与 `docs/README.md` 索引。
  - 验证结果：`cargo fmt --all` / `--check`、`cargo clippy -p snow-shot --all-targets -- -D warnings` 通过；`cargo test -p snow-shot pinned`（84 通过，1 个既有 sha256 发布检查忽略）、`translat`（224 通过）、`overlay`（185 通过）、`action_bar`（11 通过）通过；i18n check / strict-refs（395 引用，缺失 0）通过，`cargo test -p workspace-guard`（24 单测 + 6 文档测试）通过。新增 7 项离屏用例覆盖生产按钮集合、双语 tooltip / 图标资源、各禁用条件、业务动作回调分发、覆盖窗三种放置与内部相对拖动；既有贴图 / 翻译 / 覆盖窗状态机回归通过。
  - 未覆盖：真屏 tooltip / 鼠标事件命中与焦点观感、抓手跨屏混合 DPI、窄选区实际裁剪、保存落盘 / 剪贴板端到端。没有新增 Qt 模块移植，故不新增 C++ 黄金样本；沿用既有业务状态机离屏回归。未跑 workspace 全量，未改 `CARGO_TARGET_DIR`（`C:\cargo-target\cisox`），无新增依赖，未改 AGENTS.md 或 Qt/C++，未提交。
- **10-20 截图主工具栏按钮行接入共用组件（本轮，在既有未提交修改上继续）**：
  - 主栏构成：形状 / 画笔 / 标记 / 滤镜 / 编辑五个工具组 → 撤销 / 重做 → 识别组（OCR / 翻译 / 表格 / 公式）→ 输出组（贴图 / 长图 / 录屏 / 保存）→ 复制 / 取消。分组弹层包含全部 15 个标注工具和 8 个识别 / 输出动作，另有 4 个独立动作，共 27 项；确认入口仍由现有 Enter / 自动确认逻辑处理，没有新增动作。
  - 主栏按钮使用 `pin_action_bar::PinActionButton`，只显示图标 + 原 `.ftl` tooltip。七组下拉菜单（形状 / 画笔 / 标记 / 滤镜 / 编辑 / 识别 / 输出）的选项保持迁移前的图标 + 文字，不属于操作栏，不走通用操作栏组件；直接恢复 HEAD 的菜单行布局、配色、选中勾选槽、禁用和点击 / 悬停行为。水印面板“保存模板”恢复原 gpui-component 带文字 Button。组件保留主栏使用的选中态、按钮宽高、横向行、分隔符、分组提示箭头、右键菜单回调与覆盖窗原生 tooltip，删除无人使用的纵向行参数和独立原生提示构造器；菜单展开时暂停主按钮提示。撤销 / 重做、录屏 / 长图的禁用条件以及快捷键 / 工具切换 / 焦点业务回调继续沿用。
  - `snow-ui-widgets::ScreenshotToolbar` 保留分组模型、悬停计时器和菜单 / 二级菜单布局，必须显式 `with_renderer` 注入 `ScreenshotActionRenderer` 才成为可渲染元素；widgets 不依赖 snow-shot。删除旧的主栏 `icon_button` / `solo_button` 实体构造与分组主按钮渲染；主栏生成按钮定义，主栏渲染、选中态和左 / 中 / 右按下隔离由通用组件负责，禁用主按钮同样隔离事件。弹层继续用原 `menu_row_content` + gpui-component Button，不通过主栏渲染器。分组悬停、右键开关、当前项记忆和翻译目标语言回调保持原路径。
  - **位置与性能边界（位置部分已由本节末统一放置修正取代）**：该轮仅迁移按钮，错误保留了截图主栏专有位置决策；现已改为组件统一的 Below / Above / Inside 与抓手。`ToolbarGroups` 实体和量字缓存跨帧复用，弹层仍仅展开时构造；静态主栏布局表由生产渲染与顺序测试共用，不为此分配临时集合，没有新增模型、图像或持久状态的逐帧重建。覆盖窗已迁移的译文条不改位置 / 生命周期。
  - **保留的非图标动作控件**：颜色色块 / 最近颜色（色样）、装饰 `ColorPicker`（任意颜色输入）；线宽、字号、箭头头型、滤镜、模板、字体、透明度、角度、间距的 `Select`（多值选择）；填充 `Checkbox`（二值开关）；水印文字 / 模板 `Input`（文本输入）；带语言名称和勾选标记的翻译目标语言二级菜单（语言选择，不是图标动作行）。七组菜单的图标 + 文字选项及水印保存模板也保留原控件。选区形状菜单属于另一个选区选择控件，保持原实现。
  - 本轮改动文件：`AGENTS.md`、`docs/{README,cisox-progress-handoff}.md`、`snow-shot/src/{overlay_view,pin_action_bar,pinned_view}.rs`、`snow-ui-widgets/src/{toolbar,toolbar_groups,lib}.rs`。贴图视图仅将原四个按钮的结构体字面量改为组件构造器，回调不变。删除失用图标尺寸 / 箭头间距常量；没有废弃文案键，双语提示继续复用现有 catalog，没有新增依赖。
  - 验证结果：`cargo fmt --all` / `--check`、`cargo clippy -p snow-shot --all-targets -- -D warnings` 通过；`cargo test -p snow-shot toolbar`（16 通过）、`overlay`（187 通过）、`pinned`（87 通过，1 项既有发布检查忽略）、`action_bar`（19 通过）；额外 `cargo test -p snow-ui-widgets toolbar`（24 通过）。i18n check / strict-refs（396 引用，缺失 0）与 `cargo test -p workspace-guard`（24 单测 + 6 文档测试）通过，`git diff --check` 通过。新增 4 项离线测试覆盖生产静态布局与全部 27 项分组 / 顺序、当前工具高亮与模式 / 历史禁用、双语 tooltip 与图标、生产输入的禁用分发和事件隔离先于回调；既有工具切换、快捷键、输出 sink、翻译条、贴图和覆盖窗回归通过。
  - 未覆盖：真屏鼠标命中 / tooltip / 焦点、分组与语言菜单悬停、混合 DPI 观感、FPS / 渲染耗时实测，以及保存 / 剪贴板端到端。本轮是现有 UI 按钮组件整理，没有新增 Qt 模块移植，沿用原业务黄金样本与覆盖窗离屏回归；没有新增 C++ 黄金样本。未跑 workspace 全量，不修改 `CARGO_TARGET_DIR`（`C:\cargo-target\cisox`），不改 Qt/C++，不提交；既有未提交内容保留。


- **10-20 截图分组弹层验收修正**：操作栏的纯图标规则只适用于按钮行，不适用于下拉菜单。七组共 23 项（15 个工具、8 个识别 / 输出动作）的 `menu_view`、语言二级菜单本体与水印“保存模板”按钮直接恢复 HEAD 实现，保留原 `.ftl` 文案、图标文字布局、勾选 / 悬停 / 禁用配色与点击 / 悬停回调。主栏的七个分组入口与撤销 / 重做 / 复制 / 取消继续走共用操作栏；相关尺寸、选中态、分隔符和分组箭头仍被主栏使用，widgets 导出与分组模型保留。删除仅弹层使用的纵向行参数和失用的 `native_tooltip()` 构造器。
  - 本次改动文件：`AGENTS.md`、`docs/{README,cisox-progress-handoff}.md`、`snow-shot/src/{overlay_view,pin_action_bar}.rs`、`snow-ui-widgets/src/toolbar.rs`；其它既有未提交改动保留。
  - 验证：`cargo fmt --all` / `--check`、`cargo clippy -p snow-shot --all-targets -- -D warnings`；`cargo test -p snow-shot overlay`（188 通过）、`toolbar`（18 通过）、`pinned`（87 通过、1 项既有忽略）、`action_bar`（20 通过）；`cargo test -p snow-ui-widgets`（46 单测、18 文档测试通过，1 文档测试既有忽略）；i18n check / strict-refs（396 引用、缺失 0）；`cargo test -p workspace-guard`（24 单测、6 文档测试）均通过，`git diff --check` 通过。
  - 新增 3 项离屏回归：菜单图标文字渲染入口与主栏纯图标渲染入口的源码契约、全部分组菜单内容可构造，以及水印保存模板带文字。源码契约检查防止重复迁移错误，不等同于真屏像素 / 鼠标验收；实际布局与交互直接复用 HEAD，既有状态机测试保持通过。未验真屏悬停 / 点击 / tooltip / 焦点、混合 DPI、保存 / 剪贴板端到端。无新增依赖或文案键、无新模块移植、无方案偏差；未跑 workspace 全量，未改 `CARGO_TARGET_DIR`（`C:\cargo-target\cisox`）、Qt/C++，未提交。

- 本节取代 §0.-1 中截图翻译完成后的按钮条与详情交互描述；没有新增依赖，没有移植新 Qt 模块。

- **10-20 外置白条 / Inside 验收入口修正（本轮，在上述未提交改动上继续）**：
  - A 采用单窗原生形状方案。已核实滚轮透明度走 `SetLayeredWindowAttributes` 整窗 alpha，不能提供局部穿透；GPUI Windows 的 Transparent 背景只改变合成属性，没有按空白区域自动透传输入的保证。仓库已有 ADR-2b 的 `SetWindowRgn` 矩形并集能力，区域外既不可见也不接收输入，故直接复用，比新增独立置顶窗更简单，也不需要同步两窗的移动 / 关闭 / 焦点。
  - `pin_action_bar::outer_bar_rect` 统一外置几何：宽度为内容宽度，默认靠选区右缘，屏幕边界向左夹紧；覆盖窗与贴图共用。翻译贴图用同一矩形设置内容宽容器和“图像 ∪ 外置条”的原生窗口区域，条左侧空白从系统窗口形状中排除，不能画白底或拦截鼠标。窗口外框仍是两块区域的包围盒，图像原始桌面坐标 / 尺寸不变，拖动天然一起移动，关闭只有一个窗口；内部模式只保留选区区域。实际 DPI 在建窗时初始化，区域随 DPI 更新，几何未变不重复提交系统调用。
  - B 未发现 Inside 缺少抓手分支、图标资源或 tooltip：抓手 Button 在业务按钮之前，按下隔离整窗拖动，仅更新相对偏移，夹紧在选区内。验收不可见的原因是默认外置布局本来不显示抓手；`SNOW_OVERLAY_SYNTH=800x500` 改变底图和基准缩放，而贴图外置判断使用真实显示器边界，因此小合成图不能保证进入 Inside。
  - 新增 `app_runtime::ENV_PIN_BAR_FORCE`（`SNOW_PIN_BAR_FORCE=inside`）：仅 `debug_assertions` 构建生效，只强制新翻译贴图进入 Inside，外框等于选区；不改变截图 / 翻译图像位置和尺寸。删除该变量恢复正常外部优先策略。提示继续复用双语 `.ftl` 的 `translation-pin-move-bar`，无新增文案或依赖。
  - 可复现入口：先 `cargo build -p snow-shot`；在没有其它 Cisox 实例时，用 PowerShell 设置 `$env:SNOW_OVERLAY_SYNTH='800x500'`、`$env:SNOW_OVERLAY_BENCH='60'`、`$env:SNOW_OVERLAY_BENCH_TRANSLATE='1'`、`$env:SNOW_PIN_BAR_FORCE='inside'`；以 `Start-Process C:\cargo-target\cisox\debug\cisox-ui.exe -ArgumentList '--resident' -WindowStyle Hidden` 启动首进程，待托盘就绪后执行 `& C:\cargo-target\cisox\debug\cisox-ui.exe --resident --cmd screenshot`，由第二进程 IPC 触发。预期选区图像出现后即可看到最前面的六点竖向抓手及 tooltip，拖动抓手只移动条，拖图像则整窗移动；普通 Below / Above 验收去掉强制变量。
  - 新增离屏覆盖：生产渲染的抓手 Button 输入仅 Inside 存在；外置宽度与左右边界夹紧；原生区域排除空白角，DPI 1 / 1.5 / 2，移动 / 撤销保持相同窗口内区域；抓手往四边越界夹紧。另以隐藏的真实 Win32 窗口，调用 `GetWindowRgn` / `PtInRegion` 验证实际系统区域。
  - 最终验证：`cargo fmt --all` / `--check`、`cargo clippy -p snow-shot --all-targets -- -D warnings` 通过；`cargo test -p snow-shot pinned`（85 通过，1 个既有发布 sha256 检查忽略）、`translat`（225 通过）、`overlay`（185 通过）、`action_bar`（13 通过）均通过；原生窗口区域测试 `cargo test -p snow-ui-shell translation_region_native`（1 项）通过；i18n check / strict-refs（396 引用，缺失 0）通过；`workspace-guard`（24 单测 + 6 文档测试）通过；`git diff --check` 通过。新增 4 项测试，既有抓手拖动与双语提示测试一并扩展。
  - 本轮改动文件：`snow-shot/src/{app_runtime,pin_action_bar,pinned_manager,pinned_view}.rs`、`snow-ui-shell/src/{ui,native/win}.rs` 与本节；其余已有未提交改动保留。未跑 workspace 全量，未修改 `CARGO_TARGET_DIR`（`C:\cargo-target\cisox`）、AGENTS.md、Qt/C++，无新增依赖，未提交。
  - 未覆盖 / 验收差异：真屏 resident / IPC 尚未运行：当前有既有 `cisox.exe` 宿主占用与 resident 共用的单实例互斥体，本轮未结束该进程。离屏构造并非像素截图或真实鼠标验收；真实 tooltip / 抓手拖动 / 区域外跨进程点击、跨屏混合 DPI 以及保存 / 剪贴板端到端仍需验收。没有新增 Qt 模块移植，沿用既有业务黄金样本 / 离屏回归。


- **10-20 真机反馈修正：自适应条高与异形边框（历史步骤；联合轮廓边框由下文矩形边框修正取代，自适应条高保留）**：
  - `pin_action_bar` 抽出实际按钮高度 `BUTTON_HEIGHT`（24 逻辑像素）和上下各 `BAR_VERTICAL_PADDING`（4px），按钮 / 抓手通过同一个构造入口保留组件库 Small 样式并显式设置实际高度。`bar_height()` 根据按钮与内边距计算 32px，替代固定的 40px。浮动条、普通流布局、覆盖窗、翻译页、输入浮窗 / 听写浮窗及识别结果共用该尺寸来源；独立混排图标按钮保持按钮自身尺寸。
  - 翻译贴图建窗、Below / Above / Inside 空间判断、条定位、原生 `SetWindowRgn` 区域 / 命中和内部拖动夹紧统一消费 `bar_height()`，物理尺寸仍按 DPI 向上取整。572×558 选区在 100% DPI 的 Below 外框由 572×598 变为 572×590，图像坐标和尺寸不变。
  - 翻译贴图改为图像矩形与外置条矩形并集的外沿描边：纯函数按两个矩形的边缘划分格子，仅输出外沿的顺时针线段，再向区域内部展开为色条拼接。交接处不画内线，Above 同样闭合；Inside 仍为图像外框，普通贴图沿用原矩形边框。极窄图像按内部可用厚度缩减描边，避免混合 DPI 下越出原生区域。
  - 新增离屏测试覆盖按钮高度 / 内边距公式及外置空间恰好够用的临界判断、真机尺寸案例、Below / Above / Inside / 向左扩条 / 单物理像素窄图的轮廓闭合、每段两侧内外关系、无内部交接线、1 / 1.5 / 2 倍 DPI 描边完全在区域内；现有区域、整窗移动、撤销、普通贴图及所有操作栏宿主回归保留。
  - 抓手规格未改：仅 Inside 创建，生产渲染在业务按钮之前插入；图标资源与双语 tooltip 沿用既有实现。离屏构造生产抓手 Button 并验证仅 Inside 存在，实际条高参与初始位置与四边越界夹紧 / 返回起点回归。`SNOW_PIN_BAR_FORCE=inside` 仍仅在 debug 构建生效。真屏复现沿用上文入口：无其它实例时以该环境变量启动 debug resident，再通过第二进程 `--resident --cmd screenshot` 触发合成截图翻译，检查最前抓手、拖动仅移动条并夹紧选区四边；去掉变量核对 Below / Above 不显示抓手。
  - 验证：`cargo fmt --all` / `--check`、`cargo clippy -p snow-shot --all-targets -- -D warnings` 通过；`cargo test -p snow-shot pinned`（87 通过，1 个既有发布检查忽略）、`translat`（227 通过）、`overlay`（185 通过）、`action_bar`（15 通过）；i18n check / strict-refs（396 引用，缺失 0）与 `cargo test -p workspace-guard`（24 单测 + 6 文档测试）通过；`git diff --check` 通过。新增 3 项测试，其余相关测试随自适应高度更新。
  - 本轮仅修改 `snow-shot/src/{pin_action_bar,pinned_manager,pinned_view}.rs` 与本节，保留其它未提交改动；无新增依赖 / 文案，不改 AGENTS.md、Qt/C++、`CARGO_TARGET_DIR`（`C:\cargo-target\cisox`），未提交。
  - 未覆盖 / 偏差：当前已有 `cisox.exe` 与 `cisox-ui.exe` 实例运行，未重启，未做真屏抓手拖动 / tooltip、边框像素观感、区域外点击及跨屏混合 DPI 验收；离屏测试不是实际鼠标验收。保存 / 剪贴板端到端仍未覆盖。无新模块移植，无需新增 C++ 黄金样本；无实现要求或迁移方案偏差，未跑 workspace 全量。

- **10-20 真机反馈修正：图像闭合矩形边框与外置条独立描边（本轮最终状态）**：
  - 撤销上一轮“图像 ∪ 条”联合轮廓描边。翻译贴图的图像始终独立绘制完整矩形四边，Below / Above 不再绕过条，Inside 仍为图像矩形；`border` / 悬停及拖动时 `border_active` 的选择逻辑不变。普通贴图沿用原渲染路径。
  - 外置条使用同一颜色独立描边，仅省略与图像相接的重合边段，保留图像完整边线以避免双线；窄图导致条向左扩出时，交接边超出图像的部分仍画出。所有色条向所属矩形内部展开，不越出 `SetWindowRgn` 区域；极窄尺寸按内部厚度缩减，图像位置、窗口形状和命中区域不变。
  - 删除格子划分式联合轮廓算法，改为共用矩形四边生成与条交接段裁减；替换原联合轮廓测试，覆盖 Below / Above / Inside、窄图扩条、单物理像素图像、图像四边闭合、条独立边线 / 无重合双线，以及 1 / 1.25 / 1.5 / 2 倍 DPI 下每段描边均在所属矩形内。保留原区域、移动、撤销及各操作栏宿主回归。
  - 验证：`cargo fmt --all` / `--check`、`cargo clippy -p snow-shot --all-targets -- -D warnings` 通过；`cargo test -p snow-shot pinned`（87 通过，1 个既有发布 sha256 检查忽略）、`translat`（227 通过）、`overlay`（185 通过）、`action_bar`（15 通过）通过；i18n check / strict-refs（396 引用，缺失 0）、`cargo test -p workspace-guard`（24 单测 + 6 文档测试）和 `git diff --check` 通过。
  - 本轮仅改 `snow-shot/src/pinned_view.rs` 与本节。共用 `pin_action_bar.rs`、覆盖窗 / 翻译页 / 输入浮窗 / 听写浮窗不改；其余已有未提交改动保留，无新增依赖 / 文案，不改 AGENTS.md、Qt/C++ 或 `CARGO_TARGET_DIR`（`C:\cargo-target\cisox`），未提交，未跑 workspace 全量。
  - 未覆盖：真屏矩形边框与条交接处像素观感、实际鼠标 / tooltip / 跨进程区域外点击、跨屏混合 DPI，以及保存 / 剪贴板端到端仍未验收；离屏测试验证几何与业务回归，不等同于真屏验收。本轮只修正边框，无新模块移植、无新增 C++ 黄金样本，与实现要求及迁移方案无偏差。

- **10-20 截图主工具栏统一位置与内部抓手（本轮，取代前文主栏位置例外）**：
  - 截图主栏删除对旧 `calculate_toolbar_placement` 的调用、`TOOLBAR_MARGIN`、向右边缘二次锚定和按最大菜单高度决定展开方向的逻辑；统一由 `pin_action_bar.rs` 的 `outer_position` / `outer_bar_rect` / `clamp_bar_offset` 与 `action_bar_layout` 决策。下方 → 上方 → 内部，Inside 使用同一 `BAR_PADDING`、`GRIP_WIDTH`、`BUTTON_GAP`，最前显示组件库抓手。旧定位函数仍服务录屏 / 长图的独立视图，不再参与截图主栏。
  - 抽出不依赖 `PinnedWindowView` 的 `ActionBarGrip`，贴图条与截图主栏共用按钮构造、按下隔离和窗口级移动 / 松开监听；`PinActionBarState` 跨帧持有内部偏移，夹紧在选区内，选区变化和开始重新框选 / 调整时重置。覆盖窗原始捕获监听在抓手会话中跳过选区状态机，避免误改选区或自动确认。共用 `positioned_actions` 只裁剪内部条，选区内其它空白继续沿用框选 / 拖选区 / 手柄事件。
  - 覆盖窗沿用每屏 monitor / bounds、起始屏选区求交与当前窗口 DPI 换算，不把负坐标屏当作主屏；实际条矩形（含抓手宽度）同时用于放大镜命中和其它面板避让。主栏不再执行自己的位置避让。分组菜单 Below 向下、Above 向上，Inside 按实际条上下剩余空间选择方向，随拖动更新。七组弹层选项仍为图标 + 文字；其它选择 / 输入控件不变。
  - `ToolbarGroups` 与量字缓存仍跨帧复用；新增几何运算和状态均为常数开销，不逐帧重建模型 / 图像 / 分组实体。更新 AGENTS.md 的 Action bars 规则，删除截图主栏位置例外。
  - 新增覆盖窗离屏测试覆盖负坐标、1 / 1.25 / 1.5 / 2 倍 DPI、上下边均无外置空间时 Inside 且提供生产抓手、外置无抓手、四边夹紧、重绘保留 / 重新框选与调整重置，以及弹层随实际位置与抓手偏移改向。复用既有通用组件和贴图夹紧回归。
  - 本轮修改：`AGENTS.md`、`docs/README.md`、本交接文档、`snow-shot/src/{overlay_view,pin_action_bar}.rs`、`snow-ui-widgets/src/toolbar.rs`。其余未提交修改保留。
  - 验证：`cargo fmt --all` / `--check`、`cargo clippy -p snow-shot --all-targets -- -D warnings` 通过；`cargo test -p snow-shot` 分别按 `overlay`（191 通过）、`toolbar`、`pinned`（87 通过，1 个既有发布 sha256 检查忽略）、`action_bar`（20 通过）过滤，全部通过。`cargo test -p snow-ui-widgets`（46 单测、18 文档测试通过，1 个既有文档测试忽略）、i18n check / strict-refs（398 引用，缺失 0）、`cargo test -p workspace-guard`（24 单测 + 6 文档测试）与 `git diff --check` 均通过。
  - 未覆盖：真实桌面鼠标 / tooltip、菜单像素观感、跨屏混合 DPI 实机与性能基准；离屏几何及状态测试不等同于真屏验收。无新 Qt 模块移植，沿用既有业务黄金样本，不新增 C++ 样本；无新增依赖 / 文案，不改 Qt/C++ 或 `CARGO_TARGET_DIR`（`C:\cargo-target\cisox`），未提交，未跑 workspace 全量。无实现要求或迁移方案偏差。

## 0.-1 2026-10-13 收口（最新，先读这里）

**范围**：2026-10-12~13 两天的全部提交（`7f9c96ad..d26aa22b`），均已合入当前分支 `jelon/simplify-cleanup`。**状态口径沿用 §0.0：离屏测试通过，新做的界面与交互没有逐项真机点过**；真屏测过的只有托盘宿主那一块（有数据的写在下面，其余没有记录就当没测）。

**本轮落地**
- **截图翻译（`7400567d`）**：
  - 工具栏翻译行悬停展开目标语言二级菜单，14 种（`main14`，新增越南语、印尼语），只对本次截图生效，不写配置。
  - 译文原位替换：按 OCR 行框锚定，逐行填底色；字号用墨迹实测高度推算，基线与对齐方式推断，处理项目符号；底色从行框边缘采样取色；标题行先从正文段落里切出来再翻译。
  - 结果按钮条：查看详情 / 撤销 / 关闭 / 保存图片 / 保存原图，选区外侧有空位就放外侧。
  - 详情面板：悬停时 OCR 框、原文、译文三方联动高亮。
- **翻译输入浮窗按草稿改版（`7400567d`）**：源语言（自动 + 14 种）/ 目标语言、清空、交换、复制、模型下拉、标题栏可拖动、失焦不关闭；停顿后自动翻译，Enter 翻译、Shift+Enter 换行，两个下拉一变立刻重翻。
- **托盘宿主 + 按需 UI 双进程，阶段 0~4、6 完成并已切为默认，阶段 5（冷启动优化）未执行**（见本节末「未验证」最后一条；方案与实测见 [design/tray-host-split.md](design/tray-host-split.md)，用法见 §6.2）：
  - 方案与调研：`82c1dd47`（tray-idle-memory 与拆分方案）、`8010d5f8`（宿主最小化调研）。
  - 阶段 1 抽 crate 与宿主协议 `5953974a`、`d15cc373`；阶段 2 宿主 + UI 双进程 `9c8a54ba`、`7f1c17ab`；阶段 3 活动守卫、保活规则、贴图恢复、headless 任务、手势接入 `65a7ac40`、`4fc2f908`；阶段 4 CI 导入表闸门 `418ba98c`、真屏验收与脚本加固 `6af6c6c0`；切默认 `fda56c27`（合并 `19027f5f`）；阶段 6 宿主精简 `8da21621`（合并 `7e1640e0`）。
  - 数据：宿主托盘态私有提交 **约 1.93MB**（阶段 4 实测 2.1MB，阶段 6 降 0.23MB；弹一次菜单后 2.2MB），原单进程空闲 56.8MB、截图后约 135MB；热键到首帧 0.83~1.02s（i5-13500 / UHD 770，双屏 2560×1440，release）。
  - 语音热键不再保活（`accdbdcf`，2026-10-13 用户改判）：绑了语音热键窗口全关照常退出，只有听写进行中才保活。代价是冷拉起时开头那一小段话会丢，配套见下面「语音」。
  - **进程名统一 cisox 开头（2026-10-13，用户拍板）**：宿主 `cisox.exe`、UI `cisox-ui.exe`，worker `cisox-recorder.exe` / `cisox-stt.exe` / `cisox-translator.exe` / `cisox-table.exe` / `cisox-latex.exe`（任务管理器里的描述：Cisox / Cisox UI / Cisox recorder 等，worker 的版本资源由 `tools/common/worker_version.rs` 调系统 `rc.exe` 生成，未新增依赖）。只改 bin 名与版本资源，crate / package 名不变（`cargo build -p snow-tray-host -p snow-shot` 照旧）；互斥体、会话管道名、数据目录、日志文件名不变；开发工具 `snow-ocr-compare` / `snow-fps-fixture` / `workspace-guard` / `snow-i18n-tool` 不改。**例外：OCR worker 仍叫 `snow-ocr-process-1.0.8-windows-x64.exe`**，它是按 `ocr-asset-manifest.json` 从上游 ModelScope 下载的预编译程序，不是本仓库编译的；2026-10-14 用户决定不自编、也不在解压时改名。本节与下文 §6.2 以后都用新名，更早的日志性段落里的旧名是当时叫法。
  - 单进程回退开关 `cisox-ui.exe --resident` 保留，至少到用户另行拍板。
  - 阶段 6 逐项测过：做了 5 项（菜单按需构建、图标像素共享、快照探测与流式读取、常驻线程改直接 `CreateThread`）；不做并有数据的 5 项（换 windows-sys、自写托盘 / 热键、HeapCompact、SegmentHeap、日志初始化），数据都在 tray-host-split 阶段 6 执行记录里。
- **性能 / 稳定性修复**：
  - 读系统偏好（深色、系统语言）改 `RegGetValueW`，不再起 `reg.exe` 子进程（`7400567d`）。截图热路径里它每次 80~90ms、最多 4 次，修后两屏像素都变的耗时中位数 481ms → 358ms，约 -123ms（各 3 次，release）。
  - vendor 两处补丁（`3666265b`，登记在 `snow-shot-rs/vendor/README.md`）：`GpuReleaseGuard` 在渲染器销毁后显式解绑并 `Trim`，截图一次后私有提交 169MB → 135MB，释放从约 10s 缩到 0.5s；着色器 / 混合状态改设备级缓存，不再每窗口新建（Intel UHD 770 驱动对新着色器首次绘制会漏内核 Event，每次关窗约 9 个句柄）。
  - 手势绑定修复（`01db0a95`）：配置规整会把单项 `activation_key` 如 `["windows"]` 变成字符串，手势解析和宿主快照只认数组，重载后绑定全丢、全局钩子没装上。现两种形式都收。
- **语音转文字**：
  - 冷启动（`056ab371`、`5a5720fd`，合并 `aedb015c`）：worker 收到 START 先开麦，写入 30s 环形缓存，模型就绪后追赶积压；UI 带宿主传的 `--hint dictation` 启动，在 `main` 最早处预拉 worker；界面区分「加载中 / 录音中 / 已就绪」三态，按住说话在录音开始后松手会等加载与积压处理完再出字。丢字窗口估约 1.0~1.4s，其中麦克风打开 0.7~0.9s 是硬件下限，不补偿。依据 [research/dictation-cold-start.md](research/dictation-cold-start.md)，计时脚本 `scripts/verify-dictation-cold-start.ps1`（`fc33cf45`）。
  - 双语模型加载提速（`7a901ff2`、`dc76377f`，合并 `d26aa22b`）：清单里的 `model_type` 写进 `model.json` 并传给 sherpa（它不填时会把 encoder 载两遍），再用 ORT 预优化 encoder 落盘缓存。x-asr 480ms 加载中位数 5.8s → 约 0.78s，multi-zh 4.1s → 0.62s，识别文本逐字一致，数据见 [research/stt-bilingual-model-load.md](research/stt-bilingual-model-load.md) §11。**snow-stt 新增 `ort` 依赖**（`=2.0.0-rc.13`，与 snow-table 一致，用户已批准）。
- **调研 / 设计文档**：[research/tray-idle-memory.md](research/tray-idle-memory.md)、[design/tray-host-split.md](design/tray-host-split.md)、[research/tray-host-minimal.md](research/tray-host-minimal.md)、[research/dictation-cold-start.md](research/dictation-cold-start.md)、[research/stt-bilingual-model-load.md](research/stt-bilingual-model-load.md)。

**未验证 / 待办**
- 语音先开麦的真人计时没做（要对着麦克风报数，没法无人值守）：`scripts/verify-dictation-cold-start.ps1`，三个条件（UI 冷拉起页缓存热、清页缓存、UI 已在）各 5~10 轮。离线 / VAD 模式下积压喂入也没在真机验过。
- release 构建下的自启项迁移冒烟没跑：注册表里指向 UI exe（`cisox-ui.exe`）的值，无参数启动转去拉宿主后是否被改写成宿主路径（离屏单测过，开发构建不改注册表）。
- admin 混合权限（宿主普通权限、UI 管理员）的热键 / `--cmd` / 焦点：本机 UAC 关闭（`EnableLUA=0`，当前用户本就是管理员），测不了；`admin` 场景只能退化通过。
- 宿主暂停热键 / 前台全屏闸门的真屏验收不稳（三次完整运行 1 次全过，失败轮是托盘菜单点击没落到预期那项，怀疑是 UI Automation 点菜单不稳，未证实）。
- Intel 句柄泄漏：着色器缓存修复后主会话 2026-10-13 真屏复测（release 单进程 `--resident`，连截 5 次）：空闲 429 → 第 1 次 834（一次性初始化）→ 之后 831~833 持平，已不再逐次增长；双进程下 UI 退出即释放。
- `stt_download` 测试整组并行跑时偶发失败（`tar` 打包 `assert!(status.success())`），`--test-threads=1` 串行全过；另有子代理观察到 System32 `tar.exe` 不支持 `-j`（bzip2），PATH 加 Git `usr/bin` 后通过。根因未定，待修。
- 常驻线程再合并（托盘 + 热键 + 鼠标钩子并成一条消息泵线程）：估约 -0.06MB，要把热键改成回调直发，改动面大，建议不做，**待用户定**。
- 首帧另外三项可选优化（估算，没做）：采集与建窗并行，预计省 80~100ms；首窗 `SetWindowPlacement` 阻塞 79ms，原因未归因；落位时多余两次改尺寸，约 50ms，要动 vendor 或 gpui-kit 选项。
- 语音缓存溢出（模型加载超过 30s）只写 worker 的 stderr，UI 没有提示，协议里也没有对应事件。
- 离线 transducer 的 `model_type` 加载前后对比没测（本机没装离线模型）。
- ORT 预优化缓存只在本机同一台机器、同一版 ORT 1.28.0 上验证过，没在第二台不同 CPU（尤其 AMD）上测；缓存在用户机器本地生成，规避了跨机器问题，但可移植性没实证。
- 截图翻译原位替换、结果按钮条、详情联动与翻译浮窗改版的真机观感：没有记录，按未真机验证对待。
- 阶段 5（冷启动优化）在 tray-host-split 里只有范围与验收线、没有独立执行记录；已落地的相关项是上面的 `RegGetValueW` 与语音预拉；该阶段「UI 启动即并行采集」「无窗口动作走 `--headless`」没有记录，「首帧中位数 ≤ 0.7s」也没单独验收（实测 0.83~1.02s）。

## 0.0 2026-10-09 收口（上一轮；其后的变化见 §0.-1）

**状态**：全部已合入 `main`（以 `git log` 为准）；合并后 `snow-shot` 整个 bin 986 个测试通过，相关 crate `clippy -D warnings`、`workspace-guard`、i18n `check` 与 `extract --strict-refs`（引用 397 处、缺失 0）通过。均为离屏验证，**新做的界面都没有真机点过**。

**本轮落地（详见审计表对应行，状态多为 🟡）**
- **聚光灯 / 水印**：装饰层 pass（`snow-canvas-raster/src/decoration.rs`，0 新依赖，预览与导出共用）+ 工具栏入口 + 设置面板（模板、字体族、取色器、DPR 换算）。旧版黄金对照没做（本机没有旧版渲染器基线）。
- **A06 配置归档**：导出 / 导入（读存储与 deflate，写存储），导出可选是否带密钥。
- **A18 主窗口**：主窗口已并入设置窗口（保留设置窗口 + 追加翻译/历史/贴图管理/关于页）。自绘标题栏不做（GPUI 无边框窗口需自补拖动区与缩放，只能真机验证）。
- **G07 翻译页**：独立窗口 + 主窗口内嵌，历史列表（50 条）、防抖自动翻译（默认关）。流式输出不做（后端协议整段返回）。
- **G04**：表格识别（官方 SLANet_plus，Ctrl+X / 工具栏）；公式识别（RapidLaTeXOCR，**用户自行下载模型、程序不内置不托管不下载**，设置 `screenshot_conversion/latex_model_dir`，Ctrl+L / 工具栏）；Markdown / HTML 仅未配置引导。
- **A12 MCP**：一、二期，已实现 12 / 101 个 tool，控制类默认拒绝，授权域开关；桥接进程 M4 未做。
- **D12 自动滤镜（P1）**：复用仓库内 `visual-region-detector`（path 依赖，无新第三方包）；**智能擦除 P2 / P3 未做**，保持占位。选型见 [research/d12-auto-filter-erase-selection.md](research/d12-auto-filter-erase-selection.md)。
- **A09 / A05 / A15**：更新下载续传与 `updates/mode`；录屏 5 个键（清晰度、动图清晰度、编码器、预设、动图循环）经协议 `QualityRequest` 接通；未消费键剩 **22 个**；覆盖窗工具栏文案与放大镜提示走 `.ftl`。
- **发布前**：STT 13 个模型包 sha256 固定、许可核对（两个 paraformer 仍 `unverified`）、CI `workers` 任务覆盖 `snow-table` / `snow-stt`。

**用户已批准的依赖 / 结构决定（2026-10-08）**：`windows` 的 `Win32_Security_Cryptography` 特性；独立 workspace `tools/snow-table`；`miniz_oxide`（仅 `snow-config` 读 deflate 归档）；`rqrr`；`snow-shot` 对仓库内 `visual-region-detector` 与 `snow-draw-engine-document` 的 path 依赖。新增独立 workspace `tools/snow-latex`（沿用 snow-table 的 ort 版本，无新第三方 crate）。

**已裁决（2026-10-09，用户）**
1. `screen_recording/capture_toolbar_in_recording`：**不做**。录制控制条继续一律排除在成片之外，该键保持未消费（schema 保留以兼容旧配置）。
2. `updates/mode = download`：**不做自动下载**。与 `check` 行为相同，只检查并提示，下载由用户在设置页点击。

**仍未做 / 已知缺口**
- 智能擦除（D12 P2 / P3：Lab 多尺度 PatchMatch 自写，零新依赖）；MCP 余下 89 个 tool 与桥接进程；G04 Markdown / HTML 调用。
- 打包：`cisox-table.exe` / `cisox-latex.exe` / `cisox-stt.exe` 入安装包（**2026-10-19 已由 GitHub Actions 打包，见 [guides/cisox-packaging.md](guides/cisox-packaging.md)**，七个 exe 全在包里；安装后真机未验证）；`snow-stt` CI 里没有 test。
- 聚光灯 / 水印黄金对照、水印模板删除与弹窗命名；录屏默认软件编码预设由 superfast 变为 veryfast（CPU 略高）。
- 真机验证清单：本轮所有新界面（设置内嵌页、翻译窗口、检查更新、归档对话框、表格 / 公式 / 自动滤镜 / 水印面板）、录屏控制条键盘焦点、G02 结果窗渲染、多屏混合 DPI 与负原点副屏；鼠标钩子全流程这台机器测不了。

**协作备忘**：并发子代理用独立 worktree + 独立 `CARGO_TARGET_DIR`；合并多分支时冲突集中在 `overlay_view.rs`、`settings_view.rs`、`toolbar.rs`、`extensions.rs`（`EXTENSION_ENTRY_COUNT`）、两份 `settings_items.ftl` 与审计表。录屏 worker 构建不能直接用 `scripts/build-snow-recorder.ps1`（它会强制覆盖 `CARGO_TARGET_DIR`），FFmpeg 实际目录是 `.tools/vcpkg/installed/static-nox265/x64-windows-static`。

## 0.1 2026-10-08 收口（旧一轮，数字以 §0.0 为准）

**状态**：2026-10-07/08 的改动已全部提交（最新 `b3011642`）。

**本轮完成（均见 [research/qt-parity-audit.md](research/qt-parity-audit.md) 对应行，状态多为 🟡：主体做了、真机验证有限）**
- 贴图 E06~E11：键位表、缩略图、点击穿透、隐藏到顶部、分组 + 托盘分组块、管理页、识别文字、贴选中文件（资源管理器选中项**真机验证过**）、恢复最近关闭。
- 录屏 F07 订正、F08 特效（轨迹 / 点击 / 高亮 / 按键回显，**带特效录制抽帧验证过**，开特效会放弃硬件编码）、F09 控制条快捷键与「录屏并复制」（文件列表入剪贴板**真机验证过**；控制条能否拿到键盘焦点**未验证**）。
- 热键与覆盖窗：C02 / C05 / C06（含按住 Space 平移选区、按住 Shift 锁比例、快速保存、重新截图、坐标模式）、C03 托盘（图标 / 左中键 / `tray/enabled` / `menu_options`，默认菜单变短）、C04 全局鼠标手势（状态机离屏测过、键盘钩子真机看过，**鼠标钩子全流程这台机器测不了**，见审计 C04）。
- 识别：G02 结果窗（未真机渲染）、G08 选中文字翻译（**真机验证过**）。
- 标注：D08 橡皮 + 选对象；D09 订正。
- 设置：A05 新消费一批键（双击 / 中键动作、选区颜色与单位、`system/*`、记住上次工具、识别后自动动作、自动识别二维码），按脚本口径只剩 27 个键没被消费（2026-10-09 再盘点，新消费 `updates/mode`、两个滤镜样式键、`text_recognition/model_hot_start`），分类见审计 A05。**注意**：识别后的默认动作现在是 `no_action`（不再自动复制，按 Enter 复制）。
- i18n A15：覆盖窗与贴图窗全部走 `.ftl`。

**原「待拍板 6 项」已按总原则裁决（2026-10-08，`b3011642`）**
1. G03 二维码：引入纯 Rust `rqrr`，覆盖窗识别键已接线；内容是 http / https 链接时按 O 用默认浏览器打开（无真机验证）。
2. G04 表格 / LaTeX / Markdown：**表格已做**（官方 SLANet_plus ONNX，`tools/snow-table` 工作进程 + 纯函数管线，Ctrl+X）；Markdown / HTML 仅做未配置引导；公式识别已做（RapidLaTeXOCR，用户自行下载模型、不内置，`tools/snow-latex`，覆盖窗 Ctrl+L）；`cisox-table.exe` 待纳入安装包。详见 research/qt-parity-audit.md G04。
3. A09 自动更新：配置键 `updates/manifest_url`（默认空）+ 地址解析已做；**设置页「更新」分组有“检查更新”入口，发现新版本后显示更新说明并可“下载”到数据目录、校验清单里的 `sha256`、提示“打开所在目录”，不自动安装**；下载支持 `.part` 断点续传，启动后按 `updates/mode`（手动 / 检查）在后台自动检查（间隔至少一天，只在托盘提示新版本）（清单格式 `{"version","url","notes","sha256"}`，细节见审计 A09）。
4. A10 代理：`network/proxy` 支持 `none` / `system`（环境变量）或直接填地址（`http` / `https` / `socks5` / `socks5h`），转成 curl `--proxy`。
5. A12 MCP：第一期（M0 骨架）与二期已实现（合计 12/101 个 tool，见 §0.0），余下分期见 [design/mcp-subsystem.md](design/mcp-subsystem.md) §8。
6. 托盘默认菜单：维持 Qt 的 12 项默认，不改。

**用户已批准的依赖 / 结构决定（2026-10-08）**
- `windows` crate 的 `Win32_Security_Cryptography` 特性（`snow-platform`，MCP 令牌随机数 `BCryptGenRandom`），详见 [design/mcp-subsystem.md](design/mcp-subsystem.md)。
- 独立 workspace `snow-shot-rs/tools/snow-table`（表格识别工作进程，自带 `[workspace]` 与 `Cargo.lock`，ort load-dynamic）。构建脚本 `scripts/build-snow-table.ps1`，CI `workers` 任务覆盖它与 `snow-stt`。

**STT 清单收尾（2026-10-08）**：13 个模型压缩包 sha256 已全部固定；许可证核对见 `stt-model-manifest.json` 的 `license_source` 与 `licenses/runtime-components.md`（x-asr 三款 Apache-2.0、SenseVoice 为 FunASR 许可，paraformer-zh-small 与 trilingual 仍 `unverified`）。`collect-third-party-licenses.ps1` 新增 `-ExtraNotices` 参数承载非 Cargo 组件声明；Rust 版安装包脚本尚不存在（`package-snow-shot.ps1` 仍是 Qt 版），`cisox-table.exe` / `cisox-stt.exe` 入包待该脚本出现后接入。

**技术上还能继续、但量大的**：见 §0.0「仍未做 / 已知缺口」（本段 2026-10-08 的清单已大部分完成，不再列）。

**构建环境补充**：worker 构建需要 `libclang.dll`（已用 PyPI `libclang` 轮子里的 DLL 放在 `.tools/llvm/bin/`，被 gitignore）；`sccache` 在 vcvars 环境下会启动失败，需 `RUSTC_WRAPPER=""`；E: 盘慢，建议把 `CARGO_TARGET_DIR` 指到 C: 盘。

## 0. 2026-10-07 更新（更早一轮，最新状态见 §0.-1）

**分支与提交**：现在在 `main` 上工作（不再是 `rust-gpui`）。本轮新增提交：`29ef4398`（智能选区 / 历史翻页 / 选区形状）、`1837df69`（托盘重做 + 构建配置，用户的工作）、`95e8aff8`（多屏覆盖窗），外加构建提速文档的 `docs` 提交。**后两个本地提交尚未 push**：连续 3 次推送被 GitHub 返回 `Internal Server Error`（远端临时故障，没用强推），稍后重试。

**本轮已完成（均有离屏测试，`snow-shot` 全量 762 个通过，`clippy -D warnings` 干净）**
- 智能选区完整对齐 Qt：控件级、滚轮切层、目标切换快捷键、UIA 后台细化、过渡动画、右键 / 松开立刻重新命中、选回上一次选区；多屏每块屏都能命中（共享一条 UIA 线程）。
- 截图历史：覆盖窗导出写入完整现场（整帧 + 选区 + 标注历史），快捷键前后翻，右键回当前截图（`history_nav.rs`）。
- 选区形状：折线 / 曲线 / 自由绘制 + 加减区域（栅格蒙版，`snow-canvas-raster/src/region.rs`）；区域外导出透明（剪贴板 `CF_DIBV5` + `PNG`、贴图预乘 alpha）。
- 多屏覆盖窗：所有显示器并行采集、每屏一个窗口、一个共享视图工作在虚拟桌面画布坐标上，选区可跨屏；方案与结果见 [design/multi-display-overlay.md](design/multi-display-overlay.md)。
- 托盘菜单重做（用户的工作，见 `1837df69`）。

**待真机验证（我这边做不了的）**：鼠标按住跨缝拖动框选（本机 `SendInput` 合成按键送不到覆盖窗，`examples/multi_overlay_probe.rs --manual` 可手动验证）、混合 DPI（副屏临时改 125% / 150%）、负原点副屏（副屏拖到主屏左边）。

**已知取舍**：选区跨屏时录屏 / 长截图禁用（跨屏采集窗未做）；历史里多屏会话写成一张画布图；跨屏提示文案目前写死中文。

**构建环境（重要）**：本机 `E:` 盘写大文件比 `C:` 盘慢约 40 倍，增量构建 14~89 秒且不稳定。已设用户级环境变量 `CARGO_TARGET_DIR=C:\cargo-target\cisox`（增量构建稳定约 8 秒），可执行文件在 `C:\cargo-target\cisox\debug\`；下文和脚本里的 `build\cargo\...` 路径在设置后要换成这个。实测数据与采纳结论见 [guides/build-speed.md](guides/build-speed.md)。

**贴图后半 E06~E11 已做（2026-10-07，见审计表各行；均为离屏测试，真机只验证了读取资源管理器选中项）**：键位表、缩略图、点击穿透、隐藏到顶部、分组、管理页、识别文字、贴选中文件、恢复最近关闭。

**录屏特效 F08 已做（2026-10-08）**：见审计表 F08 一行。构建 worker 的两个坑：① 本机没有 `libclang.dll`，已用 PyPI `libclang` 轮子里的 DLL 放到 `.tools/llvm/bin/`（被 gitignore，需要时重新取）；② 根目录 `.cargo/config.toml` 的 `rust-lld` 链不了 `/GL` 的静态 FFmpeg，`scripts/build-snow-recorder.ps1` 已强制 `link.exe`。E: 盘慢，建议把脚本里的 `CARGO_TARGET_DIR` 临时指到 C: 盘；sccache 在 vcvars 环境下会启动失败，需 `RUSTC_WRAPPER=""`。

**接下来可做（按 [research/qt-parity-audit.md](research/qt-parity-audit.md) 的“非刻意缺口”）**：录屏音频与特效（F07 / F08）、覆盖窗快捷键里剩余的热键（C02 / C06）、托盘细节（C03）、识别结果窗 / QR / 表格（G02~G04）、设置页 198 个未消费键（A05）、MCP（A12）、截图历史管理页之外的贴图管理页（B04）。

## 1. 一句话状态

Windows 主线已经能跑通「热键 / 托盘 / IPC → 截图覆盖窗（单屏）→ 8 个标注工具 → 复制 / 保存 PNG / 贴图 / OCR / 翻译 / 长截图 / 录屏」，另有 Rust 侧新增的输入翻译浮窗、语音转文字、视频编辑后端；底层 crate（配置、i18n、仓储、滤镜、录制 worker、本地翻译 worker）质量较扎实。但**距离全功能对齐还差很多**：按审计，82 项对照里 ✅ 24、🟡 27、🟥 5、⬜ 26；设置页 238 个键里只有 40 个被运行时读取；MCP / 更新 / 网络 crate 仍是骨架；智能选区、样式面板、截图历史界面、录屏音频与特效、QR / 表格识别均缺。真机验证清单见审计 §5，大多还没做。

## 2. 已提交（本地分支 `rust-gpui`，领先 `main` 92 个提交；领先 `origin/rust-gpui` 1 个：`d6396ea9`，未 push）

| 时间 | 提交（按主题归并） | 内容 |
|---|---|---|
| 09-29 | `2d267b81`、`294d20df`、`3be6791d`、`cccc6553`、`9e2f4125` | 方案文档、P0 spike、workspace 骨架、gpui vendor（28 个 crate）、守卫、三平台 CI；`snow-config`、`snow-i18n`、`snow-ui-icons`、`snow-ui-theme`、`snow-capability`、`snow-app-core` |
| 09-29 | `f9d053d2`、`a6964c2e`、`0e34663c`、`7e692e9c`、`7c877155`、`039533e4`、`bed21e5d`、`2fa816f1`、`97f4fae0` | P1 收尾（复审 bug 修复、运行时引导）、自研组件（Checkerboard / Segmented / Popconfirm）、标注文本、`snow-ui` 聚合、选区几何 / 放大镜 / 工具栏、覆盖窗、贴图、OCR / 翻译 / 拼接服务（首版） |
| 09-30 | `3fac3c8d`、`311a71fa`、`682498a0`~`096c86ee`、`16d272c2`、`d80ff719`、`5a6dfa4c` | 设置页、单实例 IPC、托盘；录制模型；**验收报告（已失实）**；「完整接线」提交（录制 worker、OCR / 翻译 worker 等）；换机接手指南 |
| 10-01 | `8c6dcc8e`、`d05a61d2`、`a4bc9a8a`、`25bcc5c3`、`df03eba8`、`16017fff`、`3fb593bd`、… | 录屏 Media Foundation 后端与默认 Auto、跨屏硬编、夹具与逐帧追踪；AGENTS.md 重写；OCR 后端抽象、系统 OCR、对比工具与样片；翻译模型调研 / 量化评测 |
| 10-02 | `227db5e5`~`57a2f308`、`55aab1f5`、`1d2ae5e1`、`72837e20`、`7d2eb8c0`、`07b5e518`、`e2a07adc`、`c04e41fb`、`cf0d6384` | 翻译路由与 Hy-MT2 可选包及设置页；i18n 设置页本地化、去掉 zh-TW；视频编辑协议与抽帧 / 降帧 / 缩放 / 裁剪；输入翻译浮窗；STT worker；MCP 载荷对齐 schema；capability 文案与 extract 门禁修复 |
| 10-03 | `68529abb`、`75aa840a`、`3027b635`、`c54f16d6`、`d6396ea9` | 语音转文字接入主程序与 Windows 系统语音后端；视频编辑 Media Foundation 系统引擎；工具栏 ID 常量提取；文档同步 |

## 3. 未提交（`git status` 实查，2026-10-03）

| 内容 | 位置 | 备注 |
|---|---|---|
| fps 夹具改动（他人 / 另一会话） | `snow-shot-rs/tools/snow-fps-fixture/` 下 7 个文件（`analyze/*.py`、`scripts/run-fps-test.ps1`、`src/dual.rs`、`src/lib.rs`、`src/main.rs`） | 「序号条只由一个窗口绘制」的双窗口夹具改动，未验证、暂不提交（见 §5.1 跨屏待验证）。**不要碰，也不要顺手提交** |
| 测试素材 | `materials/`（`ocr/`、`translate/`） | **已于 2026-10-05 删除**（原本就未入库）；OCR 样片与翻译素材不在本机，见 `docs/guides/ocr-samples.md` |
| 审计文档与本次文档更新 | `docs/research/qt-parity-audit.md`、本文、方案、索引、验收报告 | 本次任务产物，待用户确认后提交 |

原 09-29 暂停时的「未提交」清单（i18n 门禁、滤镜、历史仓储、T3、shell、raster、调研、spike 等）**已全部随上面的提交入库，不再有未提交残留**；当时「均未经独立复审」的担忧见 §3.1，逐项处理情况已标注。「提交前必做：复审员变异实验残留检查」已随历次提交与全量测试过去，本次审计另跑 `snow-app-core / snow-config / snow-history / snow-canvas-filters` 共 260 项测试全过。

## 3.1 已收集的复审发现（09-29 CONFIRMED；2026-10-03 起逐条标注处理情况）

> 2026-10-03 审计核实已修复：`snow-ui-theme` 的 alpha 改为 16 位除以 257 取整（`color.rs`）；`snow-history::pinned::sweep_orphans` 清单有错误时拒绝清扫；`snow-config::paths` 增加 `is_upstream_location_resolved`（canonicalize + 末尾点 / 空格归一）；提交 `c04e41fb`（MCP 载荷）、`cf0d6384`（capability 文案、extract 门禁）。其余条目没有逐条复核，按「未确认」对待。

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

**`snow-ui-theme`**（复审员用静态 Qt `E:\qt-static\6.11.1`（2026-10-05 已删除）编了小程序直接对照真 Qt；小程序在会话 scratchpad `qt\q.cpp`、`b.bat`，可复用）
1. **高**：`src/color.rs` 的 `alpha()` 语义与真实 Qt 6.11.1 不符。作者按"8 位读取 = `>>8`"手工推导，真 Qt：`setAlphaF(0.88)`→224（Rust 225）、0.95→242（Rust 243），其余抽样值一致。推断真实规则是"16 位值除以 257 四舍五入"（复审员未读 Qt 源码）。`tests/tokens.rs::alpha_tokens_follow_qt_semantics` 里断言 `light.color_text` 为 225、`raw16 == 57671` 的黄金依据不成立；受影响的是 colorText（alpha 0.88）。修法：8 位读取改成除以 257 取整，测试改用真 Qt 输出。
2. **中**：`tools/p1-reference-baselines/palette-gen/fast_color_lite.cpp` **不是 `ant_design_qt` 原文**，是 std::string/std::regex 改写版，与原文有 3 处行为差异（无 `input.trimmed()`、`parseHex` 重写、`parseRgb` 重写并多了非法数字返回 false 的分支）；黄金样本 20 个解析输入里没有带空白的用例，Rust 的 trim 行为没被对拍。复审员对真 Qt 编原文测边界：`#+f0000` 原文有效 Rust 无效（作者已注明）；`#ff 000` 原文有效（#ff0000）、`rgb(1,\n2,3)` 原文无效，Rust 的行为是读代码推断（未实跑）。修法：黄金生成器改为链接真 Qt 编译原文，补带空白/换行的解析样例。
- 已复现：MSVC 重编黄金生成器输出 423 行与 `tests/golden/palette_golden.txt` 完全相同（说明黄金样本确由 C++ 可执行文件产生，但等价的是改写版，不是 Qt 原文）；`palette_generate.rs`、`fast_color.rs` 的 HSV/HSL/mix/darken/lighten 与 C++ 逐行一致。
- **未做**：icons 全部复审（829/1658、`build.rs` 规范化、渲染目视、`IconColors`、缓存）、8 处变异测试、令牌层 20 个令牌逐个对照、规范符合度。

**文档与调研结论复审**（已在 §5 体现）：ADR-8 的 `QDataStream` 例外、V6 证据不足、V2 判据事后修订与"68 次"对不上、vendor 28、ADR-5 若干措辞过强、约 15 处文档自相矛盾。

**未收集**：`snow-config`、`theme+icons`、`canvas-filters` 三份复审被中断，结论未取得。

## 4. 参考版（C++）构建：续做方法

> **2026-10-05：`E:\qt-static`（静态 Qt 6.11.1、覆盖端口、日志与脚本）已整体删除，以下为历史记录；要取参考版 P99 须重新构建 Qt 与 vcpkg 依赖。** 2026-10-03 未复核：本节描述 09-29 的构建进度，本次审计没有检查 `E:\qt-static` 与 vcpkg 的当前状态，也没有取得参考版 P99；只可能让 V2 判据更宽松，不阻塞产品开发。方法本身仍有效。

目的：取得 Qt 参考版的帧间隔 P99，用来定 V2 判据的松紧（**只可能让判据更宽松**，不阻塞产品开发）。

- **已完成**：静态 Qt 6.11.1 已装到 `E:\qt-static\6.11.1`（`EXIT=0`）；vcpkg 主树已装完约 54 个包。
- **剩余**：`onnxruntime`（含 directml）和 `opencv4` 两个包。
- **onnxruntime 提速**：upstream 端口写死 `DISABLE_PARALLEL`（单线程，因 LTCG 会吃光内存）。已在仓库外做了覆盖副本 `E:\qt-static\overlay-fast\onnxruntime\`（只去掉该参数），配 `VCPKG_MAX_CONCURRENCY=6`。**本机内存 31.7GB，可用常在 8GB 左右，若出现 C1060/LNK1102/out of memory 就回退串行。**
- **续跑脚本**：`E:\qt-static\logs\loop2.ps1`（直接调用 vcpkg，overlay 与并发变量都已写好）；`watchdog.ps1` 和 `mon.ps1` 是看门狗与监控。**看门狗曾两次误杀正常进程，重启前先确认卡死阈值为 25 分钟且把 `cmake -E` 算作忙碌进程。**
- **下载**：一律走 aria2（全局 `E:\software\aria2\aria2c.exe`）；vcpkg 下载经 `X_VCPKG_ASSET_SOURCES='x-script,E:\qt-static\fetch.cmd {url} {dst}'`。**`fetch.cmd` 仍引用仓库内旧副本 `.tools\aria2\`，不要删它。**
- **装完之后**：`.\scripts\build.ps1 -Preset snow-shot-msvc-fast -Target snow_shot`，环境变量 `SNOW_QT_STATIC_DIR=E:\qt-static\6.11.1\lib\cmake\Qt6`；帧计时埋点补丁 `tools/frame-probe/frame-probe.patch`（已在工作区应用，**首次随整个工程编译**，可能要修）；启动时设 `CISOX_FRAME_PROBE=E:\workspaces\Cisox\tools\frame-probe\probe-ref.log`；先查它的单实例互斥体与数据目录是否会与已装版冲突。**测试需要真人**：4K 全屏截图，画一个箭头拖动约 15 秒后退出，再读日志算 P99。
- **完成后清理**：`C:\Python314` 里为构建 crashpad 装了 `virtualenv`，可 `pip uninstall virtualenv`。
- 静态 Qt 编译的关键提速：脚本默认并行 4，实际用了 `-Parallelism 16`。

## 5. 待办与待决（09-29 旧清单，2026-10-03 已标注）

> **读法**：本节各 P 阶段「✅ 已完成」是当时作者自述，已被 [审计](research/qt-parity-audit.md) 订正，下面逐条加了〔审计更正〕。最新待办看 §5.1 与审计 §4（缺口前十）、§5（真机清单）。
> **本节里仍有效的待办（别丢）**：
> - 复审未做项：`snow-config` 变异测试 / clippy；`snow-ui-theme` 的 icons 全量复审、令牌逐个对照、真 Qt 编原文的黄金生成器（`fast_color_lite` 改写版问题）；`snow-canvas-filters` 复审；T3 / shell / raster 复审；`snow-history` 的低优先级项（全零 UUID、`safe_file_name` 撞名、`format_version: 2.0`、嵌套深度）。
> - V2 判据依赖的参考版 P99（§4）；V6 搜狗与 gpui-kit `Input` 的真人预编辑留档；T12。
> - 命令总线 / MCP：约 70 个 tool 待建模（审计 A11、A12）。
> - 滤镜 `cargo test --workspace` 曾编译失败的原因（后续全量通过，可视为已消除，但没人确认根因）。
> - 已被 §5.1 取代或已完成：复审重派 5 份（部分已由 `c04e41fb`、`cf0d6384` 等修复）、文档订正（v1.9）、全量测试（09-29 通过）、守卫与视图层矛盾。

**先做（成本低）**
1. 收集/重派 5 份复审：config、theme+icons、i18n+capability+总线+守卫+vendor、canvas-filters、snow-history；再补 T3、shell、raster 的复审。
2. **文档订正（✅ 已完成，2026-09-29）**：方案文档已升至 v1.9，订正全部要点（ADR-8 的 QDataStream 例外、V6/V2 措辞降级、vendor 28、ADR-5 引用与依赖版本、约 15 处前后矛盾残留如 30 vs 32 个 .ts、quick-xml 状态、V5 license 状态、14.8 万行工作量对齐、"疑似引擎序列化"去疑等）。
3. **跑一次 `cargo test --workspace`（✅ 已完成并通过，2026-09-29）**：全量单元测试、集成测试、文档测试全绿（EXIT: 0）。顺带修复了 3 个高优 bug（`validate_geometry` 范围溢出、`sweep_orphans` 损坏清扫兜底、`paths.rs` junction/末尾点绕过）、`index.rs` 递归键序排序（化解 `preserve_order` 影响）、`snow-ui-shell` 两处 doctest 错误，以及恢复了误删的 `.cargo/config.toml`。

**已知问题**
- **`cargo test --workspace` 编译/测试（✅ 已解决）**：之前并发链接导致页面文件耗尽（os error 1455）及 doctest 错误已全部修复，单线程/控并发全量测试 100% 通过。
- **`snow-history`/`snow-config` 已知偏差**：见各自作者报告（越界配置只报 Err、`shadow_color` 只近似校验等）。
- **滤镜**：`cargo test --workspace` 曾在编译 `lyon_algorithms`/`strum_macros` 失败，原因未查，怀疑是并发改依赖。
- **`snow-capability` & 运行时接线（✅ 已完成，2026-09-29）**：`snow-capability` 已补入 `Capability::CrashDump` 能力项；`snow-shot/src/main.rs` 已完成存储解析、按天滚动文件日志初始化、本地崩溃转储安装与能力表加载接线，可正常编译运行。
- **守卫与视图层矛盾（✅ 已解决，2026-09-29）**：`snow-ui-widgets` 统一依赖 `snow-ui-shell` 门面（`use snow_ui_shell::ui::*`）而非直接声明 `gpui` 依赖，既满足了视图层开发需求，又严格遵守了 `workspace-guard` 隔离守卫。已落地自研缺口组件 `Checkerboard`（透明棋盘底纹）、`Segmented`（胶囊型分段选择器）与 `Popconfirm`（气泡确认框，缺口用量榜首 32 处），单元测试与 doctests 全过，通过 clippy 0 warning 检查。（后续清理：Checkerboard、Segmented 因无人使用已于 2026-10 删除）
- **P2 `snow-canvas-text`（✅ 已完成，2026-09-29）**〔审计更正：文本组件属实；P2 整体的「15 种标注工具」只接了 8 个，智能擦除未移植，见审计 D08、D12〕：完成标注文本草稿管理 `TextDraft`、样式模型 `CanvasTextStyle`、多行排版测量 `TextLayoutResult` 以及 GPUI `EntityInputHandler` 接入实体 `CanvasTextInput`，全套 10 个单元测试与 7 个文档测试全绿，通过 clippy 0 warning 与 workspace-guard 检查。
- **P3 `snow-ui` 聚合器（✅ 已完成，2026-09-29）**：完成 `snow-ui` 总入口聚合（`shell`、`theme`、`icons`、`widgets` 与 `ui` 门面），验证通过单元测试、clippy 0 warning 与 `workspace-guard` 守卫检查。
- **P3 选区几何与交互模型（✅ 已完成，2026-09-29）**：在 `snow-ui-shell::selection` 落地橡皮筋框选（Marquee）、八向手柄命中测试与外框生成、边界限制（`bounded_selection_rect`）、拖拽位移更新（`dragged_selection_rect`）、宽高比锁定处理以及选区状态机（`SelectionState`），并通过 46 个单测与 44 个文档测试。
- **P3 放大镜与操作工具栏组件（✅ 已完成，2026-09-29）**：在 `snow-ui-widgets` 落地像素级采样放大网格与取色器 `Magnifier`（支持 HEX/RGB/HSL 循环切换与防遮挡自适应翻转定位）以及截图浮动操作工具栏 `ScreenshotToolbar`（支持标注工具切换、撤销/重做堆栈状态控制、导出动作按钮组以及智能摆放定位），单测与 doctest 全绿并通过 clippy 0 warning 检查。
- **P3 全屏覆盖窗与屏幕采集/剪贴板主链路（✅ 已完成，2026-09-29）**〔审计更正：单屏 + GDI 采集；智能选区、历史界面未做，见审计 D01~D20；PDF / 多格式导出与总线直接截图已于 2026-10-03 补齐，见下「导出块」〕：在 `snow-platform` 落地 Win32 原生 GDI 屏幕抓取（`capture_display`、内存位图与局部裁剪）与剪贴板图文直写；在 `snow-shot` 落地 `ScreenshotOverlayView` 全屏交互视图，统一承载全屏帧底图、四象限遮罩暗化、八向缩放手柄、浮动放大镜、浮动工具栏、鼠标事件交互状态机及动作分发。单元测试全过，workspace-guard 与 clippy 0 warning 保持全绿。
- **P4 贴图（✅ 已完成，2026-09-29）**〔审计更正：核心交互与持久化属实；分组、点击穿透、缩略图、快捷键、管理页、文件贴图未做，见审计 E06~E11〕：
  - 在 `snow-ui-shell::pinned_geometry` 落地等比拖动调整、瞄准锚点算法（保持鼠标相对点固定）、滚轮阶梯缩放与透明度调节算法；
  - 在 `snow-shot::pinned_view` 落地 `PinnedWindowView` 贴图浮动窗口视图组件与二次标注（矩形、椭圆、箭头、直线、画笔、文本、马赛克及撤销重做堆栈、PNG 编码与剪贴板复制）；
  - 在 `snow-shot::pinned_manager` 落地 `PinnedManager`，与 `snow_history::pinned::PinnedStore` 仓储进行崩溃安全持久化同步，支持多贴图生命周期调度与分组管理；
  - 联动 `overlay_view` 钉图动作（`ToolbarAction::Pin`）无缝裁切生成贴图。全套单元测试与文档测试全绿，通过 clippy 0 warning 检查。
- **ADR-8**：`result_style.bin` 与 `recognition_results.bin` 是 Qt `QDataStream` 私有二进制，导入器需最小读取器。
- **MCP** 共 101 个 tool，命令总线只覆盖 screenshot 域 28 个语义，其余约 70 个待建模。
- **V2**：判据（稳态平均 ≥58fps 且稳态 P99 ≤ max(参考版 P99, 20ms)）是看到数据后修订的；前提是"安静环境重跑 + 参考版 P99"。长尾归因未定，需要 GPU 侧计时（PIX/ETW）才能区分上传还是调度。
- **V6**：只有 raw 模式一份真人日志；gpui-kit `Input` 的真人预编辑与搜狗行为未留档。T12（搜狗无内联预编辑）已登记。
- **P5 OCR / 翻译 / 拼接（✅ 已完成，2026-09-29）**〔审计更正：主链路属实，`OfflineDictionaryEngine` 等已不存在；QR / 表格 / 转换 / 识别窗缺，见审计 G01~G09〕：
  - 在 `snow-translate` 落地语言枚举 `Lang`、`model.json` 模型清单扫描器 `ModelScanner`、`TranslationEngine` 抽象、离线词典 `OfflineDictionaryEngine`、OpenAI 兼容端点格式化 `OpenAiCompatibleConfig` 与 `TranslationService` 缓存调度器；
  - 在 `snow-shot::ocr_service` 落地 `OcrService`，支持外部 `snow-ocr-process` worker 调度与离线启发式分析兜底，结构化输出 `OcrTextBox` 与 `OcrResult`；
  - 在 `snow-shot::stitch_service` 落地 `StitchService` 滚动长图合成器，通过行级匹配动态计算位移重叠并拼接扩展画布，输出标准 `CapturedScreen`；
  - 联动 `overlay_view` 中 OCR（`ToolbarAction::Ocr`）与翻译（`ToolbarAction::Translate`）动作，自动提取、翻译并复制到系统剪贴板。全套测试与 clippy 0 warning 全绿。
- **P6 屏幕录制（✅ 已完成，2026-09-29）**〔审计更正：录制已改独立 worker；下面写的 `ClickRipple` / `KeystrokeDisplay` 代码中不存在，音频、特效缺，见审计 F07、F08〕：
  - 在 `snow-shot::recording::model` 落地 `RecordingFormat`（MP4、GIF、WebM）、`RecordingConfig` 与 `RecordingState` 状态机；
  - 在 `snow-shot::recording::runtime` 落地 `ScreenRecordingSession`，支持 3 秒倒计时、帧采样推进、暂停/恢复、元数据与产物导出，内置水波纹特效 `ClickRipple` 与键盘屏幕回显 `KeystrokeDisplay`；
  - 在 `snow-shot::recording::area_view` 落地 `RecordingAreaView`，提供录制框选高亮、中央倒计时大数字徽章、按键回显悬浮框与集成控制工具栏（暂停/恢复、停止完成、放弃取消）；
  - 联动 `overlay_view` 选区生成录制视图 `start_recording_from_selection`。全套 23 个测试全绿，通过 clippy 0 warning 检查。
- **P7 外围收口、设置页、单实例与托盘（✅ 已完成，2026-09-30）**〔审计更正：设置页只有 31 / 238 个键生效；托盘图标占位；MCP / 更新 / 网络仍是骨架，见审计 A05、A09、A10、A12、C03〕：
  - 在 `snow-shot::settings_view` 落地基于 `snow_config::schema` 驱动的 `SettingsView`，对齐 238 个配置项至 9 大导航分类，支持动态控件渲染与重置；
  - 在 `snow-platform::single_instance` 落地 Windows 原生 `CreateMutexW` 互斥保护与本地 IPC 端口监听（`IpcCommand` 指令派发）；
  - 在 `snow-platform::tray` 落地 `TrayAndHotkeyManager`，对接系统托盘菜单与全局快捷键注册；（后续已被 snow-ui-shell 的托盘取代，该模块已于 2026-10 清理删除）
  - 在 `snow-shot::main` 完成单实例引导、从属实例委托投递与托盘上下文接入；
  - `cargo test --workspace` 全工作区全量测试 100% 通过，`workspace-guard` 零违规，clippy 0 warning 全绿。
- **依赖状态**：已批准 `fluent-bundle`、`unic-langid`、`quick-xml`、`tracing*`、`windows`、`serde`、`serde_json`、`image`。

**提速经验（下次并发前先做）**
- 每个子代理**只跑 `-p 自己的 crate`**，workspace 全量验证由主会话最后统一做一次；不要每个任务一个独立 `CARGO_TARGET_DIR`（依赖会被重复编译 6 遍）。
- 可考虑 `rust-lld` 链接器与 `debug = "line-tables-only"`（Windows 上 `link.exe` 链接大型 debug 二进制很慢），统一写进 `.cargo/config`，别在多个任务进行中途改。
- **agy（antigravity）MCP 已断线**，恢复前不要派给它；此前 agy 有编造 API 的前科，必须把真实源码整段贴进 prompt，产出必须独立复审。

## 5.1 2026-10-01 待办清单（录屏 / OCR·翻译 / 视频编辑；含 10-02、10-03 更新；上面 §5 是 09-29 旧清单）

> 2026-10-13 补：10-12~13 新做与新增待办（托盘宿主、截图翻译、语音冷启动与模型加载提速）集中写在 §0.-1，本节下面各条是当时口径，与 §0.-1 冲突时以 §0.-1 为准。已被取代的条目在行内标了「（2026-10-13）」。

**收尾**
- [x] ~~工作区约 29 个文件未提交~~ 已过时（2026-10-03）：当前仅剩 fps 夹具 7 个文件与 `materials/`（见 §3）。推送前先 `git status` 看有没有范围外的改动；本地领先远端 1 个提交（`git rev-list --count origin/rust-gpui..HEAD` 查证）。
- [x] 根目录 `AGENTS.md` 已改写为 Rust+GPUI 版（提交 `25bcc5c3`）。

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
- [x] `materials/` 未入库，且已于 2026-10-05 从本机删除。

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
- [ ] 许可证（2026-10-08 已核）：x-asr 三款 Apache-2.0、SenseVoice 为 FunASR 许可；仅 paraformer-zh-small 与 paraformer-trilingual 仍 `unverified`（官方页均未声明）。评测过但未入清单的模型没核。
- [x] sha256 固定（2026-10-08）：原 7 个空 sha256 已实际下载并填入清单。
- [ ] 翻译上下文：首版每句独立翻译，`snow-translate` 接口不支持上下文。
- [x] CI 已覆盖 `snow-stt`（fmt+clippy）与 `snow-table`（fmt+clippy+test+release 构建）；`snow-stt` 测试因缺 onnxruntime 1.28 DLL 暂不在 CI 跑；Rust 版打包脚本待做。
- [x] 第三方声明：`licenses/runtime-components.md` + 收集器 `-ExtraNotices` 参数（2026-10-08）；调用方（Rust 版打包脚本）待接入。

**文本输入翻译浮窗**
- [x] 快捷键唤起的文本输入翻译浮窗（2026-10-03，提交 07b5e518）：配置键 `global_shortcuts/translate_input`、`AppCommand::OpenTranslateInput`、模型下拉=已装包+自动、点击译文复制。
- [ ] 真机渲染验证：浮窗位置、焦点获取、Esc 关闭、Select 选择误关窗。（2026-10-13：浮窗已按草稿改版，改为失焦不关闭、标题栏拖动，见 §0.-1，本条验证项仍未做。）

**视频编辑器（`docs/research/video-editor-backends.md`）**
- 已决定：输入只处理本软件录的 MP4（解码 h264）；系统引擎与 FFmpeg 引擎并存、用户自选；输出只做 H.264；不新增第三方依赖（抽帧：FFmpeg 引擎用 FFmpeg，系统引擎用 WIC/ImageIO）。
- 已决定：编辑任务走 worker（`snow-recorder` + `snow-recorder-protocol` 扩展），两引擎联动取消，默认输出均 H.264。
- [x] MVP 设计文档已写：`docs/research/video-editor-mvp-design.md`（协议扩展、引擎 trait、四功能、测试、P0~P4 计划）。
- [x] 核心功能已实现（2026-10-03，提交 72837e20、7d2eb8c0）：协议扩展（EDIT/PROBE）、按时间戳精确 seek、抽帧（PNG/JPEG/无损 WebP）、YUV 直通；帧率降采、缩放、关键帧裁剪编辑操作，FFmpeg 引擎（libx264 软编、音频直通）。
- [x] 系统引擎（Media Foundation）已加入（提交 `3027b635`，`tools/snow-recorder/src/edit/system/*`）。[ ] 真实录屏样片基准与两引擎内存对比、合并操作（需改协议）仍缺；**主程序没有任何视频编辑入口**（审计 §3.3）。
- [ ] 需要用户提供有代表性的录屏样片。

**截图导出（迁移顺序 A 块，2026-10-03，已提交 `5bb678ec`）**
- [x] 另存为（系统 `IFileSaveDialog`）、快速保存、PNG / JPEG / BMP / WebP / PDF、文件名模板（Qt `QDateTime` 语法，默认含 `PRODUCT_NAME`）、重名 `_N`、目录回退；`Export` / `DirectCapture` 总线 handler；新接线 9 个 `screenshot/*` 配置键（见审计 §2）。
- [ ] 仍缺：JXL / AVIF 编码、WebP 有损、自绘另存为对话框（`save_as_file_dialog = snow_shot`）、保存路径快捷项、缩放比例导出、保存后写历史、复制为文件、直接截图的 `render` 输出与对应全局热键。
- [ ] 真机验证：对话框外观与置顶覆盖窗下的层级、PDF 在常见阅读器里打开、多屏 / 高 DPI 下焦点窗口直接截图。

**覆盖窗键位表（2026-10-03，WIP 提交 `0061a4bc`，随合并保留）**
- [x] 新增 `overlay_keymap.rs`：按旧版 `screenshot_shortcuts/*`（27 键）与 `drawing_shortcuts/*`（10 键）解析覆盖窗键位，并接入 `overlay_view.rs`；`snow-platform` 补 `window_rect` 前台窗口矩形等。
- [ ] 本机断电前并行做过一套 `GlobalAction` 热键动作表（含“前台全屏窗口停用热键”闸门），与远端 `QuickAction` 重复，合并时已丢弃（仍在 `0061a4bc` 的 `global_actions.rs` 里）。**“前台全屏停用热键”已移植到 `QuickAction`（2026-10-07，`fullscreen_gate.rs`）**：开关持久化在 `global_shortcuts/disable_on_focused_fullscreen_window`，打开后前台全屏窗口时热键命令被丢弃（托盘 / IPC 不受影响，两个开关热键除外），托盘勾选跟随配置；探测用 `snow_platform::window_rect::focused_fullscreen_window_exists`，纯函数与派发器已离屏测试，真机（全屏游戏 / 视频）未验证。覆盖窗键位表尚无真机验证。

**2026-10-04 界面与截图补齐（未提交，在工作区）**
- [x] 听写浮窗：右上角 ✕ 关闭按钮（原底部关闭按钮移除）；去掉撑高空白，窗口高度 270→234，复制按钮上下留白减半。
- [x] 托盘菜单文案走 i18n（新增 `tray.ftl`、en-US/zh-CN），设置里切换语言后托盘菜单即时刷新（新增 `TrayService::set_menu`）。
- [x] 设置窗口标题栏与托盘弹出菜单跟随主题深浅（`snow-ui-shell` 新增 `ShellWindow::set_dark_title`、`ui::set_popup_menu_dark`；后者用 uxtheme 未公开序号 135/136，系统不生效时需换做法；windows crate 新增 feature `Win32_Graphics_Dwm`、`Win32_System_LibraryLoader`，无新依赖）。
- [x] 截图智能选区阶段 1（窗口级）：悬停高亮顶层窗口、单击选中、位移超约 10 逻辑像素转手动框选；复用配置键 `screenshot_selection`/`smart_selection`（默认开，文案"智能选择"）；新增 `window_pick.rs`；依赖 `snow-ui-selector`（path，仅 Windows，Apache-2.0）及其传递的 `rstar`、`crossbeam-channel`、`heapless` 0.8，Cargo.lock 带入 macOS 专用包（Windows 不编译）；第三方许可证脚本未跑，需确认是否覆盖 `snow-shot-rs`。多显示器/负坐标/DPI 仅纯函数单测，无真机验证。[x] 阶段 2 控件级（2026-10-07，照搬 Qt 交互）：命中路径自深到浅，`window_sub_element` 目标默认最深层、`window` 目标固定顶层；空闲态滚轮向上向外 / 向下向内；快捷键 `switch_selection_between_window_and_window_sub_element` 切换目标并写回 `screenshot_selection/selection_target`；UIA 前台超预算时后台细化更深层（`apply_refinement`，浅端吻合才接受）；高亮框 101ms OutQuad 过渡，受 `screenshot_ui/selection_transition_animation` 控制。细化期间渲染循环按帧轮询（`refinement_pending`），无需鼠标事件即可刷新；右键回退 / 松开回到空闲态时立刻按光标重新命中；松开点必须仍在锁定窗口内才算单击。（`visual-region-detector` 在 Qt 里只服务自动滤镜工具，与智能选区无关；历史翻页见审计 D18；选区形状与加减区域见下一条。）`select_previously_selected_area` 已做（2026-10-07）：导出（复制 / 保存 / 贴图）时把选区按逻辑坐标写入 `screenshot_selection/previous_selection`（格式与旧版互通，`snow-shot/previous_selection.rs`），快捷键在空闲 / 已选中状态下选回；多显示器命中已做（2026-10-07，见下一条与 [design/multi-display-overlay.md](design/multi-display-overlay.md)）；运行中切换智能选区开关（现在只在开始截图时读取）；多显示器 / DPI 仍无真机验证。
- [x] 多屏覆盖窗（2026-10-07）：所有显示器并行采集、每屏一个覆盖窗、一个共享视图工作在虚拟桌面画布坐标上，选区可跨屏框选 / 导出，智能选区与放大镜在每块屏上都生效，上次选区按桌面坐标存取；跨屏选区时录屏 / 长截图禁用；历史按单画布图写入。真机（并排双屏 100%）验证了同时出窗、同步关闭、副屏智能选区高亮；**未验证**：鼠标按住跨缝拖动、混合 DPI、负原点副屏。
- [x] 选区形状与加减区域（2026-10-07，已定栅格蒙版方案）：折线（逐点单击、双击 / Enter 闭合、Backspace 撤销顶点）、曲线（Catmull-Rom 闭合样条）、自由绘制（按住拖动、松开完成，RDP 简化）；Ctrl+Tab / Ctrl+Shift+Tab 或屏幕上的形状栏切换，写回 `screenshot_selection/region_type`；已确认区域只能整体移动，点框外整体重置；形状栏在选区后多出「添加 / 减去区域」，加减以栅格蒙版并集 / 差集合成，矩形形状下用框选 / 点选窗口当操作数，Esc / 右键取消。区域外导出为透明：剪贴板 `CF_DIBV5` + `PNG`（真机验证）、PNG / WebP 保留 alpha、JPEG / BMP 铺白、贴图按预乘 alpha 上传。实现：`snow-canvas-raster/src/region.rs`（蒙版、合成、描边遮罩）、`snow-shot/src/region_select.rs`（草稿状态机）。未做：圆角 / 阴影（Qt 对自定义区域也不可用）、`previous_selection` 只存外接矩形（不含区域形状）、草稿阶段不实时压暗区域外、形状栏文字按钮（无图标）。
- [x] 标注样式面板最小闭环：选中工具弹出样式条（颜色预设+最近 8 色、线宽/字号/箭头头型下拉、矩形椭圆填充开关），按工具记忆并持久化到旧键 `drawing/*_style`（子字段名 `color`/`width`/`font_size`/`fill`/`arrowhead` 为自定，旧版无金样本；`recent_colors` 放在 `shape_style` 内）；荧光笔（`PenHighlight`）与序号球（`SerialNumber`）接通，工具栏新增"高亮""序号"。未做：独立填充色、选择工具（已画标注无法选中改样式）、工具栏按钮文字仍硬编码中文未迁 `.ftl`、面板首次显示会调用一次 `Theme::change(Dark)`。
- [x] 截图工具栏右侧溢出修复：改为按右边缘锚定，估算宽度 790→890；[ ] 仍是估算值，根治需测量真实宽度。
- [x] 听写加载中提示与结束收尾（未提交）：模型未就绪时聆听图标下方显示循环进度条，就绪后维持原样；结束时若还有句子在翻译，先弹浮窗等译文（12s 超时），再次触发结束 = 强制结束；toggle 再按 / hold 松开同一路径。详见 `guides/snow-stt-worker.md`「进程生命周期」6、7。未做真机验证（麦克风、热键松开链路、进度条观感）。（2026-10-13：加载提示已扩成「加载中 / 录音中 / 已就绪」三态，worker 先开麦，见 §0.-1。）
- [ ] 听写有焦点场景（`output_mode=auto/type`）的真机验证仍未做。
- [x] 全局热键补全与直接截图（未提交）：新增 `quick_actions.rs`（键表、延迟状态机、输出方案）、`direct_capture.rs`、`AppCommand::QuickAction`。已接线：`screenshot_full_screen`、`screenshot_focused_window`（新增 `foreground_window_rect()`，snow-platform 的 windows 依赖加 `Win32_Graphics_Dwm`）、`screenshot_delay`（只用于该动作，普通 F1 不延迟）、`screenshot_fixed` / `screenshot_copy`（Ctrl+F1）/ `screenshot_ocr` / `screenshot_translation`（覆盖层框选后自动确认，对应旧版 `captureAndPinSelection` 等）、`open_settings`、`toggle_global_hotkeys`、`open_screen_recording_folder`。直接截图**不再复制到剪贴板**（2026-10-15 起）：只写 `%TEMP%\Cisox\captures\` 的 PNG 副本（保留最近 20 张），由宿主弹系统通知气泡（缩略图作大图标，点击用默认看图程序打开该图；托盘菜单"打开截图临时目录"进目录）；开 `screenshot/auto_save_after_copy` 或 `copy_image_file_to_clipboard` 时再按配置落盘。气泡走 `UiMessage::Balloon` → 宿主 `Shell_NotifyIconW(NIF_INFO)`，点击拦截与图标 uID 推算依赖 `tray-icon` 0.25 的内部常量，升级该库须复核（`snow-shell-core/src/tray.rs`）。
- [ ] 占位（触发后只给本地化提示，提示走托盘悬停与日志，无弹窗）：`screen_record_copy`、`open_capture_history`、`open_pin_to_screen_management`、`translate_selected_text`、`pin_selected_files`、`restore_last_closed_windows`、`toggle_disable_on_focused_fullscreen_window`。
- [x] 截图历史（未提交）：新增 `history_store.rs`（后台写入、去重 10 秒、上限裁剪、分页、缩略图）、`history_view.rs`（历史窗口：虚拟滚动、每页 20 条、复制/贴图/定位文件/删除、清空二次确认）；写入点覆盖普通截图复制/保存/贴图与直接截图；入口为托盘"截图历史"与热键 `open_capture_history`（已从占位改为真实动作）；复用旧键 `capture_history/*` 与 `snow-history` 的 `index.json` v2 存储，未新增依赖。
- [ ] 历史遗留：仅靠 `snow-history` 往返测试验证兼容，未用旧版真实数据对照；`canvas_history.json` 写 `{}`（旧版"再编辑"不可用）；`displays[0]` 与 `result` 为同一份 PNG（每条占两份）；Popconfirm 颜色写死浅色，深色主题下气泡为浅色卡片；保留天数清理只在打开历史页时触发；无筛选/批量删除；无真机验证。
- [x] 录屏音频阶段 1（未提交）：MP4 单轨混音（系统声 WASAPI loopback + 麦克风）。协议 `START` 加可选前缀 `mic=`/`sys=`/`mvol=`/`svol=`/`mdev=`/`sdev=`（旧格式字节不变）与事件 `AUDIO_STATE`；worker 新增 `audio.rs`（混音器：10ms 槽、100ms 抖动窗口、增益、补零、暂停丢包）与 `win/aacsink.rs`；Media Foundation 路径加 AAC 流、FFmpeg 硬编路径加原生 `aac` 流（128 kbps、48k 立体声）、软件路径透传开关；无新增依赖（`snow-audio-recorder` 为 path 依赖，已在依赖树内）。主程序读旧键 `screen_recording/enable_microphone`、`enable_system_audio`，非 MP4 强制关闭；录制控制条显示降级提示（不可用/中断/本次没有声音）；`verify-snow-recorder.ps1` 加 `-Audio`。真屏自检：三条路径音视频时长差 ≤0.05s，暂停场景正确，GIF 无音轨。
- [ ] 录屏音频遗留：开关只在设置页，录制工具栏没有（延迟为 0 时来不及用）；非 MP4 时设置页只改说明文字，开关未置灰；软件路径不支持音量/设备/AUDIO_STATE；麦克风真实内容未验证（本机麦克风阵列近乎静音）；设备选择、音量滑块、双轨属阶段 2；偶发风险：带音频放音 + MF 路径下出现过 3 次停止耗时约 90 秒（捕获更新数异常高），之后 4 次未复现，未定位，建议长录制多测，必要时给 Finalize 加超时。

**迁移差距盘点（2026-10-04）**

核心项目进度与已知偏差：

- **P0**：标注样式面板（已补最小闭环，见上）、截图历史（已补，见上）、全局热键原先仅接 5 组，现已补接直接截图等 9 个动作（见上，无真机验证），仍有 7 个为占位、录屏音频（阶段 1 已补，见上）。
- **P1**：延迟截图（已接到 `screenshot_delay` 动作；普通 F1 不延迟，是否也要延迟待定）、快捷键录入控件（仍是通用文本框）、贴图管理页/分组/隐藏到顶部/从文件贴图、OCR 结果窗与二维码识别、选中文本翻译、视频编辑 UI、`snow-mcp`/`snow-update`/`snow-net` 仅占位、托盘缺更新等入口（历史入口已加）且图标为纯色占位、聚光灯/橡皮擦/水印/自动滤镜工具缺失。
- **文档与代码不一致**：`docs/cisox-migration-acceptance-report.md` 写"核心功能已 100% 交付"、迁移方案 P6/handoff §5 写 `ClickRipple`、`KeystrokeDisplay` 已落地，但 crates 内无这两个符号，录屏也无音频/点击特效/按键回显；报告写"15 种标注工具"而 `AnnotationTool` 只有 12 个变体（本轮已补荧光笔与序号球可用）；handoff 说根 `AGENTS.md` 仍是旧 Qt 版的待办已过期（现已是 Rust 版）。
- **已明确推迟/允许占位**：macOS/Linux（ADR-7）、自动更新/崩溃上报/表格公式识别/安装包/旧数据导入（迁移方案 §10）、H.265/AV1/WebM。

**已搁置（存档，可恢复）**
- H.265：暂不支持，原因与恢复起点见 `docs/research/windows-hevc-support.md`。恢复前要补：干净 Windows（未装 HEVC 扩展）实测、Intel/AMD 实测、法务确认授权。
- AV1：仅作设想，未调研落地；需驱动升级后才能验证本机 `av1_nvenc`。
- WebM 录制：见 `docs/cisox-todo-webm.md`。

## 6. 环境速查

- 编译目录：主工作区在 `build/cargo`；各 worker 是独立 workspace，产物在各自的 `target/`（`tools/snow-recorder/target` 等），都可随时删除重建。2026-10-05 已清掉 `build/`、`E:\cargo-targets`、各 worker 的 `target/`，下次编译要完整重编（录屏 worker 约 14 分钟）。
- 模型：应用从 `<数据根>/models/translate/<模型 ID>/` 读取翻译模型（数据根默认 `%LOCALAPPDATA%\Cisox`），模型都在仓库外；我们转换 / 量化的 5 个成品包托管在 GitHub Release `models`（地址见 `snow-shot-rs/README.md`「模型下载地址」），下载后解压到该目录即可；OCR 模型与运行时在 `%LOCALAPPDATA%\Cisox\assets\ocr`。早期手动放置的 `E:\models\translate\`（上游 OPUS-MT int8 旧包）已于 2026-10-05 删除。
- 推荐模型清单与统一模型目录：各功能模型统一放 `<数据根>/models/<功能>/`（translate / stt / ocr / table / latex，一个文件夹或一个文件 = 一个模型，显示名就是文件（夹）名）。**旧目录已不再读取**：`assets/{ocr,table}/models` 的模型已迁完，读取端只认统一目录（设了 `SNOW_OCR_ASSET_DIR` / `SNOW_TABLE_ASSET_DIR` 时仍只认 `<覆盖目录>/models`）。读取端不要求 `.complete.json`：手动放入的文件齐全（存在且非空）即可，应用内下载留下的标记仍按清单校验大小与哈希；下载器落盘位置是统一目录。公式配置里自设目录优先，缺文件时用 `models/latex`。**“默认模型”下拉与“获取”入口**：翻译 / OCR / 表格 / 公式 / 语音的默认模型下拉只列已下载、能用的模型（显示名 = 文件（夹）名），选项就是且仅是已下载的模型（不追加“当前值（未安装）”）；配置里存的值不在列表里（旧配置 / 模型被删）时显示占位文案、不报错，读取端回退到首选项 / 第一个可用的；一个都没有时同样显示占位，下拉后面都有“获取”按钮，打开该功能的推荐模型弹窗（`snow-shot/resources/model-catalog.json`，逻辑在 `model_catalog.rs`，新增条目只改 JSON 与 `model_catalog_notes.ftl`）。各下拉的判定与配置键：翻译 = 扫翻译模型目录里的文件夹，写 `screenshot_translation/local_model_id`（值取 model.json 的 ID）；OCR = 清单里已知档位且 det/rec/dict 齐全的文件夹（认不出结构的不列），写原有的 `text_recognition/model_type`（档位键，值不在已下载列表里时显示占位，原有按需下载 / 提示逻辑本身不变）；语音 = 沿用 `stt_download::is_installed` 的已安装集合，写 `dictation/model_id`；表格 = 含 onnx 的文件夹或单个 onnx 文件，新增键 `screenshot_conversion/table_model`（空 = 自动，优先 slanet-plus，否则第一个可用的）；公式 = 四个文件齐全的文件夹（目录本身齐全也算一个），新增键 `screenshot_conversion/latex_model`（空 = 自动，用第一个可用的）。选中项不存在或为空时读取端回退到首选项 / 第一个可用的（`snow-shot/src/model_pick.rs`）。OCR 运行时（`assets/ocr/runtimes`）与 onnxruntime 不在统一范围。设置页各功能顶部不再有“获取模型”入口行，“获取”只在“默认模型”下拉后面（顺序：下拉、获取、重置；翻译 / OCR / 表格 / 公式下拉宽 210，语音 300，默认窗口宽度下三者同时可见）；翻译页另外不再展示“翻译模型”（`screenshot_translation/model`）；翻译 / 截图转换 / 语音三页的“模型目录”栏（`screenshot_translation/local_models_dir`、`screenshot_conversion/latex_model_dir`、`dictation/model_dir`）也不再展示（`settings_state::HIDDEN_KEYS`，键、schema 与读取逻辑保留：这三个目录键仍被读取端使用：翻译读取端与听写翻译的模型扫描、公式解析的自设目录优先、语音的自定义流式模型目录会停用模型选择项；旧配置里手动指定的目录继续生效）。**自定义 AI 模型（`api_configuration/custom_models`）**：设置页“模型接口”分组整页是专用编辑界面（`settings_view.rs` 的 `render_custom_models`，纯逻辑在 `custom_models_edit.rs`）：模型列表、添加 / 编辑 / 复制 / 删除（删除需确认）、重名与地址校验，字段为名称、API URL、API 密钥（输入遮罩，可切换显示）、API 模型、“允许此模型将图像转换为 Markdown 和 HTML”（视觉）；ID 为本地生成的小写 v4 UUID（不引入新依赖）；保存走 `SettingsState::apply`（沿用 normalizer 与落盘，失败只提示固定文案、不带配置内容）。该键仍属 SECRET_KEYS，并加入 `HIDDEN_KEYS`（不再以只读行出现）。**未实现“从 API URL 拉取模型列表”**：snow-net 没有 HTTP 客户端，不引入新依赖，模型 ID 手填。翻译页“翻译模型”（`screenshot_translation/model`）与截图转换页“视觉模型”（`screenshot_conversion/vision_model`）恢复展示，改为下拉（选项 = 自定义模型名称，视觉只含 supports_vision 的模型，值为模型 ID），控件顺序：下拉（宽 210）、获取（跳到“模型接口”页并打开添加表单）、重置；没有模型时显示“尚未配置自定义模型”占位。设置页每项下面的说明行不再回退显示配置键：没有说明时留空；所有会展示的配置键都补了中英文说明（`settings_items.ftl` 末尾“补全的设置项说明”，快捷键 / 鼠标 / 样式类按分组模板生成），并有单测 `every_displayed_key_has_description_in_all_locales` 防止回归；状态栏的“已保存 / 已恢复”提示改用设置项名称而不是键名。设置页文案：公式模型 / 表格模型 / 文字识别“本地模型” / 语音“本地模型”，说明统一为“选择默认模型”。原来的 Hy-MT2 说明区、语音模型下载面板（含“未安装，下载约…”行与应用内下载按钮）、公式模型面板已移除，语音应用内下载的界面入口与事件链一并删除（`stt_download.rs` 等后端文件保留）；OCR 运行时 / onnxruntime 缺失的引导仍在覆盖窗的识别提示里，没动。翻译包大小是文档里的约数（磁盘体积，非 zip 体积）。
- 全局工具：aria2 在 `E:\software\aria2`（已加用户 PATH），下载规则已写进 `~/.claude/CLAUDE.md`。
- 机器：20 逻辑核、内存 31.7GB（长期紧张）、磁盘 SSD；本机无 winget，`choco` 需管理员。
- 网络：单连接约 0.2–0.6MB/s，多连接可叠加；curl 需 `--ssl-no-revoke`；**PowerShell 5.1 的 `>`/`>>` 会破坏二进制**。
- 不要往 upstream 数据目录（`%LOCALAPPDATA%\SnowShot\`、`%APPDATA%\SnowShot\`）写任何东西。

## 6.1 资源清理记录与复现（2026-10-05）

已删除：`E:\models\translate-eval`（70.5 GB，评测模型、fp32 导出、原版 HF 模型、成品包本地副本）、`E:\models\translate`（1.2 GB，早期手动放置的上游 OPUS-MT int8 旧包，应用并不读取这里）、`build/`（95 GB，含 `mt-quant`、`flores`、`nllb-corpus`、`stt`、`recorder-diag`、各轮录屏实验目录等调研数据）、`E:\cargo-targets`（69 GB）、`E:\qt-static`（25 GB）、三个 worker 的 `target/`、`.tools/vcpkg/downloads` 与旧的含 x265 的 `installed/static`、`materials/`。共释放约 317 GB。`E:\cargo-target\termy`（15 GB）不属于本项目，未处理。

保留：`.tools/vcpkg/installed/static-nox265`（无 x265 的 FFmpeg，录屏 worker 默认用它）、`.cache`（含 onnxruntime 动态库）、aria2 与 vcpkg 工具本身。

复现提示：
- 翻译成品包：直接从 Release `models` 下载；要重新生成，先下载原版 `facebook/nllb-200-distilled-600M`、`tencent/Hy-MT2-1.8B`、`Helsinki-NLP/opus-mt-*`，再用 `snow-shot-rs/tools/snow-translator/{eval,scripts}` 下的脚本导出、量化、打包（流程见 `docs/guides/translation-model-release.md`）；评测用的 CCMatrix 词频与 FLORES 数据需重新取得。
- 在本机重装 FFmpeg（`scripts/build-ffmpeg-recorder.ps1`）前先设 `X_VCPKG_ASSET_SOURCES='x-script,E:\workspaces\Cisox\.tools\vcpkg-fetch.cmd {url} {dst}'`：vcpkg 自带 curl 在本机连 msys2 镜像会报 SSL 错误 35，该钩子走 aria2 绕开（钩子脚本放在 `.tools/`，不入库）。下载缓存已清，会重新下载 msys2 工具包与 cmake 等。
- Qt 参考版（V2 判据所需的参考版 P99）：见 §4，需从零重新构建。

## 6.2 进程模型（托盘宿主 + 按需 UI）（2026-10-12；2026-10-13 更新）

> 方案与全部实测见 [`design/tray-host-split.md`](design/tray-host-split.md)（§8 阶段 0~6、§12 阶段 0 实测）。**默认是双进程（2026-10-13 起）**：双击 / 自启 / 无参数运行的入口都是宿主 `cisox.exe`；单进程常驻成为回退开关 `cisox-ui.exe --resident`。

**两个 exe（同目录；宿主按自己的目录找 UI，环境变量 `SNOW_UI_EXE` 可覆盖）**

| exe | 包 | 职责 |
| --- | --- | --- |
| `cisox.exe`（宿主，约 1.1MB，不链 GPUI） | `snow-tray-host` | 托盘与菜单、全局热键（含暂停开关、前台全屏闸门）、鼠标手势钩子、单实例互斥体与 `--cmd` 入口、UI 进程拉起与仲裁（状态机、命令队列、崩溃熔断）、更新检查到点拉起 headless、登录时恢复贴图 / 保活拉起。**不读 `config.json`、不带 i18n、不画图标**，只认 UI 写的 `host/spec.json` 快照。 |
| `cisox-ui.exe`（完整应用，约 26MB） | `snow-shot` | 全部窗口、配置读写（唯一写者）、历史、贴图、录屏 / 语音 / 翻译 / OCR 调度、MCP、更新。所有窗口关闭且无后台活动后向宿主请求退出，宿主同意后立即退出（不保活；只有 MCP 开启、OCR 预热、贴图、语音 / 录屏进行中等才留着，绑语音热键不算）。 |

**启动开关**

| 入口 | 开关 | 含义 |
| --- | --- | --- |
| 宿主 `cisox.exe`（**默认入口**） | 无参数 | 抢单实例互斥体，成为常驻宿主（托盘态只剩它）；抢不到就唤醒运行中的实例 |
| | `--cmd screenshot｜recording｜scroll-capture｜pin-clipboard｜settings｜show｜quit` | 有运行中的实例：经控制管道把命令交给它；**没有实例：本进程成为宿主并在起来后执行该命令**（`quit` 例外：没有可退出的对象，直接结束）。命令名两种模式不变 |
| | `--resident` | 不跑宿主，转调 `cisox-ui.exe --resident` 后退出（回退入口） |
| UI `cisox-ui.exe` | 无参数 | **转去启动同目录宿主 `cisox.exe` 后退出**（老用户注册表里指向本 exe 的开机自启走这条，随后由 UI 进程把自启值改写成宿主路径） |
| | `--resident` | **单进程常驻**（回退开关，显式才进）：自带托盘 / 热键 / IPC，与宿主共用互斥体所以互斥；开机自启仍指向宿主，不随它改变 |
| | `--ui --session <名> --token <令牌> [--hint dictation]` | 宿主拉起的 UI 会话：连会话管道，不持托盘 / 热键；用户不要手敲。`--hint dictation`（宿主队首命令是语音转文字时带）让 UI 在 `main` 最早处并行预拉语音 worker |
| | `--headless emit-spec｜update-check｜set-config --key <键> --value <JSON>` | 无窗口任务，不初始化 GPUI，做完就退（宿主调用） |
| | `--cmd …`（不带 `--resident`） | 转给宿主（等价于 `cisox.exe --cmd …`）；`--resident --cmd …` 才是单进程模式下的单实例委托 |

**构建**（在 `snow-shot-rs/` 下；产物目录见 §6 / 根 `.cargo/config.toml`，本机若设了 `CARGO_TARGET_DIR` 以它为准）

- 两个包一起构建：`cargo build -p snow-tray-host -p snow-shot`（加 `--release` 出发布版）。**`<产物目录>/<profile>/cisox.exe` 自拆分起是宿主**，完整应用是 `cisox-ui.exe`；2026-10-13 起宿主叫 `cisox.exe`、UI 叫 `cisox-ui.exe`（原 `snow-shot.exe` / `snow-shot-ui.exe`，不做旧名兼容），旧脚本 / 习惯里的旧名要改。
- 宿主导入表闸门：`scripts/check-host-imports.ps1 -Build [-WithUi]`（`-WithUi` = 两个包同一条命令构建）。CI 的 Windows `host-imports` 任务跑 `-Build -WithUi`。依赖闭包规则在 `cargo test -p workspace-guard`。
- 完整应用开发：单进程调试 `cargo run -p snow-shot -- --resident`（不带 `--resident` 会转去拉宿主，开发时通常没构建宿主）；要跑双进程，先 `cargo build -p snow-tray-host -p snow-shot`，再运行 `<产物目录>/<profile>/cisox.exe`。
- 重启：双进程由宿主处理；单进程回退模式的重启辅助会带 `--resident` 拉起新实例。

**数据与日志位置**（数据根 = 便携标记 `__data_directory` 指向的目录，否则默认 `%LOCALAPPDATA%\Cisox`；宿主与 UI 用同一套解析，代码在 `snow-data-paths`）

- 快照：`<数据根>\host\spec.json`（UI 写、宿主读；热键表、托盘菜单快照、手势绑定、几个开关、`pins_to_restore`、`update_due_at`）。
- 日志：`<数据根>\logs\`，宿主 `host.log`（INFO 及以上，256KB 滚动为 `host.log.1`），UI 是 `cisox.<日期>.log` 滚动文件；崩溃转储 `logs\crash\`。
- MCP 描述符 `<数据根>\mcp\descriptor.json` 仍由 UI 写，路径内容不变。
- 单实例标识：宿主 / `--resident` 共用互斥体 `Local\cisox.snow_shot.single_instance`；UI 另持 `Local\cisox.snow_shot.ui` 防重复；会话管道前缀 `cisox.snow_shot.session`。

**验收脚本**（都要 Windows PowerShell 5.1、桌面已解锁、本机没有别的 Cisox 实例；按约定不在锁屏时跑）

- `scripts\verify-tray-host-split.ps1 [-Scenario all|idle|ime|hotkey|tray-focus|cmd|settings-hotkey|pause-gate|crash|admin|gesture|pins-restore|stt-keepalive|mcp-keepalive|update-headless] [-Rounds 3]`：把宿主 / UI exe 复制到临时目录并写 `__data_directory` 标记，不碰真实数据目录；结果写 `build\split-accept\<时间戳>\results.json`。覆盖 §8 阶段 2、3 里需要真屏的各项（托盘态宿主私有提交 ≤ 6MB、冷拉起热键→首帧 ≤ 1.14s 等）。
- `scripts\check-host-imports.ps1`：离屏，导入表白名单。
- 完整应用（单进程）的老脚本：`scripts\verify-snow-shot-recording.ps1`、`tools\self_acceptance_verify.ps1`，指向 `cisox-ui.exe` 并**显式 `--resident`**（切默认后无参数会转去宿主；录制脚本里的 `--cmd` 委托也带 `--resident`，免得依赖宿主 exe 存在）。

**现状**：阶段 0~4、6 已完成并合入（阶段 5 冷启动优化未执行）：真屏验收通过（主会话 2026-10-13 实测：宿主托盘态私有提交约 2.1MB，热键→首帧 0.83~1.02s，命令无丢失、无双 UI；明细回填见 `design/tray-host-split.md` 阶段 2~4），用户 2026-10-13 拍板**切默认为双进程**，改动清单已按 `design/tray-host-split.md` §8 阶段 4 完成；阶段 6（宿主精简）把托盘态私有提交再压到**约 1.93MB**（弹一次菜单后约 2.2MB，逐项数据见该文阶段 6 执行记录）。阶段 5（冷启动优化）在方案里没有独立执行记录，口径见 §0.-1「未验证 / 待办」最后一条。

**切默认时的前提（已由用户拍板放行）**：① 真屏验收通过；② 托盘态宿主私有提交 ≤ 6MB；③ 冷拉起热键→首帧、连续热键截图无丢命令无双 UI；④ 功能回归；⑤ ~~`dictation_hotkey` 是否登录预拉~~（**2026-10-13 用户改判：绑语音热键不保活、不预拉，窗口全关照常立即退出**，见 `docs/design/tray-host-split.md` §10 第 4 条；代价是冷拉起时按住说话开头可能丢约 1s）。注意首帧实测上沿 1.02s 高于阶段 4 量化口径 0.9s，属已知偏差（2026-10-13 读系统偏好改 `RegGetValueW` 后热路径再快约 123ms；剩余可选优化见 §0.-1）。`--resident` 路径至少保留到用户另行拍板。

**升级与自启（切默认后）**：开机自启永远指向宿主（`system_settings::auto_start_target`，注册表值不能带参数，所以 `--resident` 回退进程也指向宿主）；未发布、不做旧名兼容：旧名（`snow-shot.exe` / `snow-shot-ui.exe`）登记的自启项由用户自行重设；若注册表里指向的是 `cisox-ui.exe`，其无参数启动会转去拉宿主，随后配置载入时自启值被改写为宿主路径。Rust 版安装包已由 CI 出（2026-10-19，`installer/cisox.nsi` + `scripts/package-cisox.ps1`，开始菜单快捷方式指向宿主，见 [guides/cisox-packaging.md](guides/cisox-packaging.md)；`scripts/package-snow-shot.ps1` 等是 Qt 版流水线）；约束不变：必须把两个 exe 放同一目录，开始菜单与自启指向宿主，`snow-update` 自动安装要先 `--cmd quit` 宿主再替换两个 exe。
