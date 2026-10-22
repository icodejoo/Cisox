---
title: 模型加载优化台账（语音 / 翻译 / OCR / 表格 / 公式）
status: active
updated: 2026-10-22
summary: 各功能模型"按键到可用"做过哪些优化、哪些没做、哪些已否决、还能叠加什么；每项带编号和证据，改动后就地更新并记一笔
---

## TL;DR

- **查这个问题先看本表，不要再全盘扫 git 和文档。** 2026-10-14 用三路审计（git 全量历史按路径过 + `-S` 追符号、docs 全读、代码核对、本机日志）建立，之前漏看过"翻译预热"（只写在 `guides/translation-model-release.md` §5.7）。
- 已做得最完整的是**语音·流式**（预拉 worker + 先开麦 + `model_type` + encoder 预优化缓存，加载 5.8 s → 0.8 s）；**翻译**有懒加载、预热、内存映射、只加载一次；**OCR** 只有 UI 存活期复用和可选的常驻预热；**表格、公式**没有任何加载优化，也没有耗时数据。
- 准入线：**只要不占闲时内存**（不让 UI 或 worker 在没有窗口时保活）的手段都可以上；会常驻的一律不做，除非用户另行拍板。
- **2026-10-22 结论**：ORT 层面没有新的有效手段。预打包权重落盘无收益；`.ort`+mmap 加载快约 54% 但工作集 +21%、磁盘翻倍，不接入（TR-23）；STT-13、OCR-11、TR-24 的首次推理预热实测无收益，已撤销。翻译的外置权重 + mmap 早已是现状（TR-09）。剩下可能有效的是"热键后提前拉起"类（TR-17/18、OCR-10/18），已写但未真机验证，收益以日志为准；公式 KV cache 要重导出模型（OCR-22）。
- 下一步优先（2026-10-14 旧版，部分已被上条取代）：先查 [P-01](#问题清单)（手动放入的语音模型可能识别不到），再给三个功能补分段耗时日志（STT-14、TR-20、OCR-15），之后按"待做"里收益明确的先做。

## 维护规则

1. 改动任何一项（实施、否决、实测出新数字），**在同一次提交里**改对应行的状态 / 证据 / 收益，并在文末「变更记录」追加一行（日期、编号、改了什么、commit）。
2. 新发现的手段或问题，按功能追加新编号，编号不复用、不重排。
3. 收益一栏写清是**实测**（附出处）还是**估计**；没有证据写"未证实"。
4. 原文结论只在这里和出处文档各写一次，别处引用编号即可。

**状态图例**：✅ 已做　🟡 部分 / 已写未提交　⬜ 待做（候选）　❌ 已否决　➖ 不适用　❓ 未查清
**闲时内存**：否 = 不占；短时 = 只在功能使用期间占，UI 退出即释放；是 = 会常驻或保活。

## 语音转文字（STT）

主要出处：[research/stt-bilingual-model-load.md](../research/stt-bilingual-model-load.md)（下称 *load*）、[research/dictation-cold-start.md](../research/dictation-cold-start.md)（下称 *cold*）、[guides/snow-stt-worker.md](../guides/snow-stt-worker.md)。

| 编号 | 范围 | 手段 | 状态 | 证据 | 闲时内存 | 收益 |
|---|---|---|---|---|---|---|
| STT-01 | 链路 | UI、worker 按需拉起，听写完即退 | ✅ | `accdbdcf`（绑热键也不保活）；design/tray-host-split.md §10 | 否 | — |
| STT-02 | 链路 | 宿主给 UI 传 `--hint dictation`，UI 在 `main` 最早处预拉 worker 并立刻发 START+PING | ✅ | `5a5720fd`；`snow-host-protocol/src/launch.rs`；`dictation/prespawn.rs` | 否 | UI 冷启动与加载重叠（*cold* §13） |
| STT-03 | 链路 | worker 先开麦 + 30 s 环形缓存，模型就绪后追赶 | ✅ | `056ab371`；`snow-stt/src/main.rs`、`preroll.rs` | 否（听写中约 +2MB） | 加载期间不丢字 |
| STT-04 | 链路 | 加载中 / 录音中 / 已就绪三态提示 | ✅ | `5a5720fd`、`2631edc5` | 否 | — |
| STT-05 | 链路 | 预拉起再提前到 `bootstrap_with` 之前 | ⬜ | *cold* §13 偏差 1；日志 bootstrap→worker 启动 +40 / +431 ms（各 1 样本） | 否 | 估计 0.05~0.3 s，先做 STT-14 再定 |
| STT-06 | 链路 | 宿主直接拉 worker / 宿主先录音 | ❌ | *cold* §4.2、§4.4：破坏宿主边界与导入表闸门，只多省 0.06~0.3 s | 宿主 +0.5MB | — |
| STT-07 | 链路 | worker 带模型保活 / 麦克风常开 / 登录预拉 UI | ❌ | *cold* §4.5、§4.6；handoff:23 | 是（137~248MB） | — |
| STT-08 | 流式 | 直接告诉 sherpa `model_type`，省掉 encoder 载两遍 | ✅ | `dc76377f`、`7a901ff2`；`optcache.rs`、`sherpa.rs` | 否 | 实测 x-asr 480ms 5.8 → 2.8 s（*load* §11.2 B 档） |
| STT-09 | 流式 | ORT 预优化 encoder 落盘缓存 `<模型目录>/.opt/`（EXTENDED + N=1），加载前校验 | ✅ | `optcache.rs`；*load* §11 | 否（磁盘 +150MB/模型） | 实测 480ms 5.8 → 0.78 s，multi-zh 4.1 → 0.62 s，文本 SHA 一致 |
| STT-10 | 流式 | 会话结束后后台生成缓存（空闲优先级子进程） | ✅ | `snow-stt/src/main.rs`；`optcache::spawn_background` | 否 | 第二次起命中 |
| STT-11 | 流式 | 设置页刷新已装模型时，对无缓存的流式模型后台生成 | 🟡 已写未提交 | `crates/snow-shot/src/stt_optimize.rs`；`settings_view.rs` `refresh_stt_cache` | 否 | 首次听写即命中（前提是用户开过设置页） |
| STT-12 | 流式 | 调图优化级别 / 线程数 / prepacking / arena；`.ort` 格式；decoder、joiner 并行或懒加载；换小模型 | ❌ | *load* §3.3、§4、§5.3：无收益或损失精度 | — | — |
| STT-13 | 流式 | 等麦克风就绪空档里喂一块静音做首次推理预热 | ❌ 实测无收益（2026-10-22） | 麦克风在 START 后 0~24 ms 就开，无空档；预热本身 270~390 ms，循环就绪反而晚 0.2~1.0 s；文本 SHA 一致。若以后加载比开麦快可重测 | — | — |
| STT-14 | 链路 | 把 `load_ms`、加载计划、RECORDING / PONG 时间点写进 info 日志 | ✅（未真机验证） | 新增 `dictation/timeline.rs`（`SessionTimeline`，纯逻辑）；`dictation/client.rs` 读线程记 READY / RECORDING / PONG / 首字时间点、`send` 记 START、`drain_stderr` 把 `load:` / `load_plan:` / `stats:` 行提升为 info，`shutdown` 出一行 `语音转文字会话摘要`；`prespawn.rs` 标记预拉起与认领（`SttLink::note_claimed`）；worker `tools/snow-stt/src/main.rs` 加载后无条件写 `load:` + `load_plan:` 到 stderr（stdout 协议不变）。单测：`timeline` 5 个、worker `load_line_has_stable_prefix_and_field`；`build-snow-stt.ps1 -Test` 83 通过 | 否 | 无直接提速，为其它项定量 |
| STT-15 | 离线 | 离线 transducer 补 `model_type`（需先加加载失败回退） | 🟡 接线了未生效 | `offline.rs:450` 只读 `model.json`，清单没给离线模型填值；离线加载失败无回退 | 否 | 未测，估计省一次 encoder 载入 |
| STT-16 | 离线 | 离线 encoder 预优化缓存 | ⬜ | `--optimize` 只认流式白名单 | 否 | 未测 |
| STT-17 | 离线 | VAD 与识别器并行加载 | ✅ | `offline.rs` `load_parallel`（VAD 在新线程、识别器在当前线程，错误优先级同旧串行）；sherpa 两类对象绑定自带 `Send`；单测 6 个 | 否 | 未实测（本机无离线模型），估计 ≤0.19 s；ORT 若有进程级锁可能部分串行 |
| STT-18 | 系统 | 系统语音后端冷启动 | ❓ | `system.rs` 三步串行；本机联机开关关闭，未测 | 否 | — |
| STT-19 | 流式 | 其它 4 个流式模型、160ms 模型的缓存与 `model_type` 验证 | ❓ | 清单只核对过 3 个模型 | — | — |
| STT-20 | 发布 | 随 release 附带预生成 `.opt` | ⬜（低优先） | 每模型多下 ~150MB，绑 ORT 版本，跨 CPU 未验证 | 否 | 省首次 ~5 s 生成 |

## 翻译（TR）

主要出处：[guides/translation-model-release.md](../guides/translation-model-release.md)（下称 *release*，预热与协议 v2 在 §5.7）、[guides/translation-quantization-benchmark.md](../guides/translation-quantization-benchmark.md)（下称 *bench*）。代码：`crates/snow-translate/src/router.rs`、`tools/snow-translator/src/engine.rs`、`crates/snow-shot/src/translate_service.rs`。

| 编号 | 范围 | 手段 | 状态 | 证据 | 闲时内存 | 收益 |
|---|---|---|---|---|---|---|
| TR-01 | 进程 | 懒加载，首次用到才拉 worker | ✅ | `227db5e5`；`router.rs` | 否 | — |
| TR-02 | 进程 | 空闲 120 s 卸载 | ✅（双进程下基本作废） | `snow-config/src/extensions.rs`；tray-host-split.md：worker 随 UI 退出 | 仅 `--resident` | — |
| TR-03 | 进程 | 常驻上限 `max_resident` 默认 1，加载新包先卸最久未用的空闲包 | ✅ | `router.rs` | 否 | — |
| TR-04 | 预热 | 输入浮窗建成后预热 | ✅ | `49a9f34a`；`app_runtime.rs` `WarmHandle` | 短时 | 日志：预热 0.9~1.7 s 被藏在开窗后 |
| TR-05 | 预热 | 截图翻译点"翻译"后与 OCR 并行预热 | ✅（时机偏晚） | `49a9f34a`；`app_runtime.rs` | 短时 | OCR 1.2~3.3 s 常盖不住慢加载 |
| TR-06 | 预热 | 选中文字翻译经浮窗预热 | 🟡 | 读完选区开窗后才预热，读取期间不预热 | 短时 | — |
| TR-07 | 预热 | 没用上的预热立即回收 + 30 s TTL 兜底 | ✅ | `snow-translate/src/worker.rs` `cancel_warm` | 否 | — |
| TR-08 | 模型 | int4 量化、NLLB 词表裁剪（6/14 语言）、OPUS-MT int4 | ✅ | *bench*；*release* | 否 | — |
| TR-09 | 模型 | 权重外置 `.onnx_data` + 内存映射 | ✅ | *release*；*bench*：加载后 540 → 278 MiB | 否 | — |
| TR-10 | 模型 | NLLB `num_beams=2` | ✅ | *bench*：峰值 -127 MiB，质量持平 | 否 | — |
| TR-11 | 协议 | 协议 v2：按目录只加载一次，换语言对不重载 | ✅ | `49a9f34a`；*release* §5.7 | 否 | 省 2.7~3.2 s（干净机器约 1.7~1.8 s） |
| TR-12 | 模型 | 去掉加载时 SHA-256 校验 | ✅ | `49a9f34a`（删 `checksum.rs`） | 否 | 与 dev 构建开 opt-level 2 同批，各占多少未拆 |
| TR-13 | 模型 | 降图优化级别 / 关 prepacking / 收紧线程 / 关 arena、mem pattern | ❌ | *bench* §154、§336、§339：延迟或内存变差 | — | — |
| TR-14 | 模型 | 嵌入共享 / 4 位嵌入 / int2 / HQQ / int8 / CTranslate2 | ❌ | *bench* §288~335；translation-hymt2-eval.md §8 | — | — |
| TR-15 | 进程 | `max_resident=2` 避免来回切包重载 | ❌ | 多常驻一个模型，违反低内存 | 是 | 日志 03:07 来回切包 0.7~1.7 s |
| TR-16 | 预热 | 浮窗里首次按键时若预热已被回收则重新预热 | ✅ 未真机验证（`translate_input_session.rs` `note_edit`、`app_runtime.rs` `rewarm_translate_input`、`translate_service.rs` `WarmHandle::rewarm_on_input`；句柄改 `Vec` 免互相回收） | 日志 03:06:49 预热被 TTL 卸载，03:06:56 首次翻译重载 723 ms | 短时 | 实测一例 0.72 s，NLLB 估 1.6~2.5 s |
| TR-17 | 预热 | 翻译热键带 `--hint translate`，UI `main` 最早处预拉 worker；选中文字读取期间就预热 | 🟡 已写未提交，未真机验证 | `launch.rs` `UiHint::Translate`；`early_warm.rs`；`app_runtime.rs` `translate_pending_warm`、`release_warm_handles`；命令被丢弃时句柄留到下次开浮窗，靠 TR-16 兜底 | 短时 | 估计 ~0.5 s，未测 |
| TR-18 | 预热 | 独立翻译页开窗预热 + 输入补预热、听写翻译开始聆听时预热 | 🟡 已写未提交，未真机验证 | `translate_page.rs`、`translate_page_view.rs`、`dictation/translate.rs`、`dictation/flow.rs`；内嵌设置页打开时不预热（推测性），只在输入时补；听写翻译靠 worker 30 s TTL 回收 | 短时 | 未测 |
| TR-19 | 预热 | 截图翻译更早预热（悬停翻译按钮 / 展开语言子菜单） | ❌ 同 OCR-09 原则否决（2026-10-14） | 推测性预热：用户还没触发翻译就加载 | 短时 | — |
| TR-20 | 日志 | `load_ms` 日志带包 ID、冷/热、入口 | 🟡 缺入口 | `snow-translate/src/worker.rs`：`翻译模型已加载` 加 `model_id`、`trigger`（warm / request）、`cold_start`、`ready_ms`、`wait_ms`（原有 `load_ms`、`mem_bytes` 保留），`翻译 worker 已预热` 加 `model_id`、`warm_ms`；单测 `load_log_inputs_cold_flag`。**缺**：入口（输入浮窗 / 截图翻译 / 选中文字 / 听写）需跨层传递，未做，靠 trigger 与相邻日志（`文字翻译完成`、浮窗日志）区分；未真机验证 | 否 | 为其它项定量 |
| TR-21 | 模型 | encoder / decoder 并行建会话（分词器留调用线程） | ✅ | `engine.rs` `load_parts`（`thread::scope`，报错优先级同旧串行）；opusmt 真实模型回归通过；NLLB / Hy-MT2 未回归 | 否（加载期峰值未测，估 +50~100 MiB） | 噪声大：opusmt 三次最小值 2043 → 1276 ms（约 -37%，不可作结论） |
| TR-22 | 模型 | `trim()` 挪到发出 Result 之后 | ✅ | `worker.rs` `pending_trim` + `after_send()`，`main.rs` 发完事件后调用；下一条命令前兜底补做；协议字节不变 | 否 | 未实测，单测证明顺序 |
| TR-23 | 模型 | ORT 预优化图落盘缓存 / `.ort`+mmap | ❌ 实测不接入（2026-10-22） | `docs/research/translation-ort-external-weights.md`；opusmt-en-zh：B 预打包外置只有编码器能用（合并解码器重名报错），收益在噪声内；C `.ort`+mmap 会话建立 ~1550 → ~680 ms，但工作集 +21%、磁盘多一份全量权重（opusmt +117 MiB，NLLB 约 +500 MiB）。冷缓存、NLLB、release 档未测。实验代码 `tools/snow-translator/src/ortcache.rs`、`engine.rs` `SessionExtra` 留作探针 | 否 | 冷启省约 0.9 s，代价违背低内存小体积 |
| TR-24 | 模型 | 加载后跑一次极短句做首次推理预热 | ❌ 实测无收益（2026-10-22） | 预热本身 260~420 ms，首句只省 0~30 ms（噪声内），只是把延迟从首请求挪到加载阶段 | — | — |
| TR-25 | 在线 | OpenAI 兼容后端：多段并行（上限 `MAX_PARALLEL_REQUESTS=4`），结果保序，错误语义不变 | 🟡 已写未提交，未实测真实端点 | `openai.rs`；13 个单测（假传输）；连接复用（curl `--next`）未做，要改调度模型 | 否 | 未实测 |
| TR-26 | 解码 | 多段文本 batch 解码 | ⬜ 已评估（2026-10-22），建议先做段间并发 | 真 batch 要改编码器 padding/mask、解码器 batch 维、`beam.rs`、KV 重排，Hy-MT2 不适用；外推 4~8 段一批 14~18 s → 6~9 s，未实测 | 否 | 未实测 |
| TR-27 | 冷启动 | 冷页缓存、release 构建下的加载数 | ❓ | 本机日志全来自 dev 构建；*bench* 两次列为待办 | — | — |

## OCR / 表格 / 公式（OCR）

主要出处：[guides/ocr-model-tiers-benchmark.md](../guides/ocr-model-tiers-benchmark.md)、[research/system-ocr-translate-backends.md](../research/system-ocr-translate-backends.md)、[research/g04-structured-recognition-selection.md](../research/g04-structured-recognition-selection.md)。

> **注意**：实际运行的 OCR worker 是 ModelScope 上预编译的 `snow-ocr-process` 1.0.8（`crates/snow-shot/resources/ocr-asset-manifest.json`），改 `snow-crates` 里的 worker 源码必须重编并重新发布 runtime 才生效。2026-10-14 用户决定**不自编 OCR worker**（也不改进程名），所以下面标"需重发 runtime"的项（OCR-12、OCR-13、OCR-14）暂时做不了，除非重新讨论自编。二维码（`rqrr`）、自动滤镜 / 智能选区（`visual-region-detector`）是纯 Rust，无模型。

| 编号 | 范围 | 手段 | 状态 | 证据 | 闲时内存 | 收益 |
|---|---|---|---|---|---|---|
| OCR-01 | OCR | 按需拉起；UI 存活期复用 worker，空闲 30 s 退出 | ✅ | `ocr_service.rs`、`ocr_client.rs` | 否 | — |
| OCR-02 | OCR | `model_hot_start` 启动预热（须同时开常驻） | ✅ 默认关 | `e1d51ce6`；`ocr_service.rs` `hot_start_enabled` | **是**（UI 保活） | — |
| OCR-03 | OCR | 默认 small 档；medium 及以上否决 | ✅ | ocr-model-tiers-benchmark.md：medium+ 单张 11~90 s、1.2~2 GiB | 否 | — |
| OCR-04 | OCR | 不加载方向分类（cls） | ✅ | `snow-ocr-process/src/main.rs` | 否 | 竖排影响未验证 |
| OCR-05 | OCR | ORT 静态链接 worker | ✅ | 上游 `07ad5bb0` | 否 | — |
| OCR-06 | OCR | 避免整图拷贝、锁外 shutdown | ✅ | `1752f5f1` | 否 | — |
| OCR-07 | OCR | DirectML 加速 | ❌（可选，默认关） | benchmark：small 慢 1.35 倍、内存 +0.9 GiB、首次编译 21.5 s | — | — |
| OCR-08 | OCR | 默认改用系统 OCR | ❌ | ocr-samples.md：CER 57.0% 对 17.8% | — | 冷启动 45 ms 对 1157 ms |
| OCR-09 | OCR | 选区第一次成形（工具栏首次画出）时后台预拉 worker | ❌ 用户否决（2026-10-14） | 曾实现后撤回：**用到 OCR / 翻译等功能时再拉起，不做"还没用就先拉"的推测性预拉** | 短时 | — |
| OCR-10 | OCR | OCR 快捷键 / 手势带 `--hint ocr`，UI `main` 预拉 worker | 🟡 已写未提交，未真机验证 | `launch.rs` `UiHint::Ocr`；`early_warm.rs`；`host_spec.rs`、`mouse_gesture.rs`；仅本地模型时预热；宿主 exe 1,167,872 字节，导入表闸门通过 | 短时 | 估计 0.3~0.5 s，未测 |
| OCR-11 | OCR | 预热时跑一张空白小图做首次推理 | ❌ 实测无收益（2026-10-22） | 64×64 空白图，交替 3 轮：首次识别均值 965 ms 对 969 ms | — | — |
| OCR-12 | OCR | det / rec 并行建会话 | ⬜ | `rapid-ocr-rs/src/pipeline/rapid_ocr.rs` 串行；需重发 runtime | 否 | 未测 |
| OCR-13 | OCR | ORT 预优化图落盘缓存 | ⬜（收益存疑） | small 图节点少，上界估 0.1~0.2 s；需重发 runtime | 否 | 未测 |
| OCR-14 | OCR | 线程改用满物理核 | ⬜（收益在推理） | `26fe1790` 在上游、不在本分支；本分支物理核 / 2；需重发 runtime | 否 | 未测 |
| OCR-15 | OCR | 宿主侧分段日志（spawn / Ready / prepare / recognize），把 worker 的 `engine_ready duration_ms` 记下来 | ✅（未真机验证） | `ocr_client.rs`：`OcrWorker` 记拉起时刻，`handshake` 出 `OCR worker 已就绪`（`spawn_to_ready_ms`）、`prepare` 出 `OCR 会话已准备`（`first_prepare`、`prepare_ms`、`engine_init_ms`、`spawn_to_prepared_ms`）、首次 `recognize` 出 `OCR worker 首次识别完成`；stderr 泵改按行解析 JSON 事件（`pump_stderr`、`parse_worker_event`），`ocr.*` 事件写 info 并取 `engine_ready` 耗时。`ocr_service.rs`：`run_with_worker` 每次请求出 `OCR 识别耗时分段`（`cold_start`、`launch_ms`、`prepare_ms`、`recognize_ms`、`total_ms`），`warm_up` 出 `OCR 预热完成`（`cold_start`、`warm_ms`）。worker 未改。单测 4 个：`parses_worker_events`、`pump_stderr_parses_events_and_keeps_tail`、`engine_ready_event_updates_shared_slot`、`stage_flags_track_successes_only` | 否 | 为其它项定量 |
| OCR-16 | 系统 OCR | 复用引擎 / 线程 | ⬜（低优先） | `win_ocr.rs` 每次新线程、新引擎 | 否 | 估计几 ms~几十 ms |
| OCR-17 | 表格 | 一次性进程，跑完即退；SLANet_plus 7.7MB | ✅ | `table_service.rs`；g04 | 否 | — |
| OCR-18 | 表格 | 表格 worker 先加载模型再读 stdin；宿主在 OCR 进行时就拉起 | 🟡 已写未提交，未真机验证 | `tools/snow-table/src/main.rs`；`table_service.rs` `ProcessRunner::prestart`；OCR 失败时 `Drop` 结束提前拉起的进程 | 短时 | 估计 0.1~0.3 s，未测 |
| OCR-19 | 表格 | 加载 / 推理耗时 | 🟡 日志已补，数据待真机 | worker stderr 末行 `timing load_ms=.. infer_ms=..`，宿主 info 日志带 `spawn_to_done_ms`、`after_ocr_ms` | — | — |
| OCR-20 | 公式 | 一次性进程，每请求拉起、结束即杀 | ✅ | `latex_service.rs` | 否 | — |
| OCR-21 | 公式 | 三会话并行加载 | 🟡 已写未提交，无实测 | `snow-latex/src/main.rs` `thread::scope`，错误优先级 resizer > encoder > decoder；本机无公式模型；预优化缓存未做（量不出收益，阈值 50 ms） | 否 | 未测，加载期峰值可能略升 |
| OCR-22 | 公式 | 解码加 KV cache | ❌ 当前模型不支持 | 解码器只有 ids/mask/context 三个输入，无 `past_key_values`；需重导出（Python/PyTorch，待用户同意），方案见 `docs/research/latex-worker-optimization.md`；worker 日志会报 `kv_cache=false`（未在真实模型上确认） | 否 | 未测 |
| OCR-23 | 公式 | 加载 / 推理耗时 | 🟡 日志已补，数据待模型 | `SNOW_LATEX_TIMING=1` 才输出分段耗时与工作集峰值（默认关，避免污染宿主错误尾部）；本机没装公式模型 | — | — |

## 问题清单

审计中顺带发现、会影响功能或文档的问题。

| 编号 | 问题 | 证据 | 状态 |
|---|---|---|---|
| P-01 | 语音模型安装链路断了：`stt_download::install*` 无调用方（`81c91ef1` 删了应用内下载），新模型的 `model.json` 不会有 `model_type`；`is_installed` 要求只有应用内安装才写的 `.complete.json`，**手动放入的模型设置页列不出、听写判未安装** | `stt_download.rs`；`settings_view.rs`、`dictation/config.rs` 都走 `is_installed` | ✅ 判定改为"清单文件齐全即已安装"（2026-10-14）；`model_type` 缺失仍由缓存元数据补（STT-08/11） |
| P-02 | OCR 热启动条件不一致：预热要求常驻 + 热启动，保活只看热启动，只开热启动时 UI 被保活却不预热 | `ocr_service.rs:112`；`host_spec.rs:250` | ⬜ |
| P-03 | 翻译文档过期：SHA 校验已删仍写在校验（*release* :54、:161，*bench* :382）；越南语 / 印尼语已加入；预热与协议 v2 未进 handoff 与索引 | 见 TR-11、TR-12 | ⬜ |
| P-04 | 选中文字翻译已接线，qt-parity-audit.md C02 与 handoff:374 仍写占位 | `app_runtime.rs` | ⬜ |
| P-05 | g04 文档写表格"空闲 N 分钟退出"，实际跑完即退；local-ocr-model-options.md 说 cls 是否启用未验证，实际已关闭 | 见 OCR-04、OCR-17 | ⬜ |
| P-06 | snow-stt-worker.md 自相矛盾：一处说不再提供应用内下载，另一处仍描述下载面板 | `guides/snow-stt-worker.md` | ⬜ |
| P-07 | worker 进程优先级：Win32 只继承 Idle / BelowNormal，UI 的 AboveNormal 不会传给 worker；*load* §2 "继承 UI 优先级"的说法存疑 | 日志 worker 启动早于"进程优先级已设置" | ❓ 未实测 |

## 变更记录

| 日期 | 编号 | 变更 | commit |
|---|---|---|---|
| 2026-10-14 | 全部 | 建表：三路审计（git 全量历史 + 文档 + 代码 + 本机日志）结论入表 | 待提交 |
| 2026-10-14 | STT-11 | 设置页刷新时为无缓存的流式模型后台生成缓存（已写未提交） | 待提交 |
| 2026-10-14 | OCR-12~14 | 用户决定不自编 OCR worker，三项依赖重发 runtime 的优化暂挂 | 待提交 |
| 2026-10-14 | P-01 | `is_installed` 不再要求 `.complete.json`，手动放入的语音模型可被识别；单测 `installed_requires_files_not_marker` | 待提交 |
| 2026-10-14 | STT-14 | 听写会话时间点与摘要进 info 日志（`dictation/timeline.rs`、`client.rs`、`prespawn.rs`）；worker 加载后无条件写 `load:` / `load_plan:` 到 stderr，主程序提升为 info；同步 `guides/snow-stt-worker.md` | 待提交 |
| 2026-10-14 | TR-20 | `翻译模型已加载` 带 `model_id` / `trigger` / `cold_start` / `ready_ms` / `wait_ms`，预热日志带 `warm_ms`；入口信息未做 | 待提交 |
| 2026-10-14 | OCR-15 | OCR worker 生命周期分段日志（就绪 / 会话准备 / 首次识别 / 每请求分段），stderr 的 `ocr.*` JSON 事件转 info | 待提交 |
| 2026-10-14 | OCR-09 | 选区首次成形时预拉 OCR worker（钩子 + `UiEvent::OverlaySelectionFormed`），日志 `trigger=selection` | 未提交即撤回 |
| 2026-10-14 | OCR-09、TR-19 | 用户否决推测性预拉：用到功能时再拉起；OCR-09 代码撤回，TR-19 不做 | 待提交 |
| 2026-10-14 | TR-16 | 翻译浮窗"空白→有内容"时若包不在内存则补预热，日志 `trigger=first_input` | 待提交 |
| 2026-10-14 | TR-21 | 翻译 worker encoder / decoder 并行建会话 | 待提交 |
| 2026-10-14 | TR-22 | 翻译 worker 先发结果再 trim | 待提交 |
| 2026-10-14 | STT-17 | 离线后端 VAD 与识别器并行加载（`offline.rs` `load_parallel`），未用真实离线模型实测 | 待提交 |
| 2026-10-22 | TR-23、TR-24 | 三档对比实测：预打包外置收益在噪声内，`.ort`+mmap 加载快约 54% 但工作集 +21%、磁盘翻倍，均不接入；首次推理预热无收益 | 待提交 |
| 2026-10-22 | STT-13、OCR-11 | 实测无收益，撤销 | 待提交 |
| 2026-10-22 | TR-17、TR-18、OCR-10 | 翻译 / OCR 热键带 `--hint`，翻译页与听写翻译补预热（未真机验证） | 待提交 |
| 2026-10-22 | OCR-18、OCR-19 | 表格 worker 先加载再读 stdin，宿主 OCR 期间提前拉起，补耗时日志 | 待提交 |
| 2026-10-22 | OCR-21~23 | 公式三会话并行加载、分段耗时日志；KV cache 因模型不支持未做 | 待提交 |
| 2026-10-22 | TR-25、TR-26 | 在线翻译 4 路并行；batch 解码仅评估 | 待提交 |
