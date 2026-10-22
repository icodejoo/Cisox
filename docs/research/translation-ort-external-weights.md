# 翻译 worker：ORT 预优化缓存与预热实验（TR-23 / TR-24 / TR-26）

日期：2026-10-22。范围：`snow-shot-rs/tools/snow-translator`（`cisox-translator.exe`）。

## 环境

- 机器：Intel Core i5-13500，Windows 10 Pro 19045。
- ORT：`onnxruntime.dll` 1.28.0.20260724（`.cache/ortdll`，load-dynamic），ort crate 2.0.0-rc.13（mg-chao 补丁 rev 90018ee）。
- 构建：dev 档（`opt-level = 2`，非 release），线程数固定 4，进程 BelowNormal 优先级。
- 模型：`opusmt-en-zh-int4-pack`（Cisox 数据目录里的包，复制到临时目录实验，没有动原目录，也没碰 SnowShot 上游目录）。
- 句子：`Hello world.` / `Where is the nearest train station?` / 一句 17 词长句，束宽 4（清单默认）。
- 页缓存：**全部是热缓存**。本机没有不依赖管理员权限的清页缓存办法，冷缓存没测（见"未覆盖"）。

## TR-23：三档对比

三档交错执行（A B C A B C ...），共 4 轮；探针是 `ortcache.rs` 里的 `#[ignore]` 测试 `probe_variant`，每次一个新进程。

- A：现状（`.onnx` + `.onnx_data`，ORT 内存映射外置权重）。
- B：`session.save_external_prepacked_constant_initializers=1` 离线产出带预打包权重的优化 onnx + data，再加载。
- C：优化后存成 `.ort`（`session.save_model_format=ORT`），加载时开 `session.use_memory_mapped_ort_model=1` + `session.use_ort_model_bytes_for_initializers=1`。

| 档 | 会话建立 ms（4 次） | 中位 | 加载后工作集 MiB | 加载后私有字节 MiB | 译完 3 句后工作集 / 私有 MiB | 输出 |
|---|---|---|---|---|---|---|
| A | 1514 / 3747(离群) / 1581 / 1369 | ~1550 | 149.5 | 139.0 | 172.2 / 151.4 | 基准 |
| B（仅编码器） | 1676 / 1534 / 1364 / 1305 | ~1450 | 139.5 | 129.0 | 171.3 / 141.3 | 与 A 逐字相同 |
| C | 704 / 649 / 663 / 743 | ~680 | 180.7 | 132.1 | 201.8 / 144.5 | 与 A 逐字相同 |

输出三句：`好久不见` / `最近的火车站在哪里？` / `棕色的狐狸跳过懒狗而猫悄悄地从窗户看`，三档一致。

要点：

1. **B 实际只有编码器能用。** 解码器是合并解码器（`If` 子图共享初始化器），ORT 重存后重载报 `model.shared.weight_scale initializer name is not unique`。B 的数字是"编码器用预打包缓存、解码器沿用原文件"。收益很小：加载约 -6%（在噪声边缘），私有字节 -10 MiB（-7%），译完后工作集持平。预打包数据外置后编码器数据文件 45.5 -> 54.9 MiB。
2. **C 加载耗时约 -54%（1550 -> 680 ms），私有字节 -5%，输出一致，保留了 mmap。** 但**工作集 +31 MiB（+21%）**：`.ort` 的 flatbuffer 校验会遍历整个文件，映射页全部进工作集（文件页，可回收，但任务管理器 / `mem_bytes` 日志会按它算）。磁盘上多一份完整权重（opusmt 约 +117 MiB；NLLB 约 +500 MiB），因为 `.ort` 把权重内联，原 `.onnx_data` 还要留着做回退。
3. 产出耗时：opusmt 约 1.6 s（B 约 2.4 s，含重载校验），NLLB 未测。

### 结论与接入决定

按任务的判据，C 的加载耗时降幅 >=10%、输出一致、没丢 mmap，**形式上达标**。但我**没有把它接进 `engine.rs`**，理由：

- 工作集 +21% 违背总原则里的"低内存"，且磁盘体积翻倍违背"小体积"；换来的只是每次冷启 worker 省约 0.9 s（opusmt，热缓存、dev 档），空闲时 worker 本来就会退出，用不上常驻收益。
- 产出缓存要在 worker 里再建一遍会话（加载期峰值再加一份），或另起产出步骤，有额外复杂度；还要处理 ORT 版本 / CPU 变化后的失效。
- 冷页缓存下 C 是否仍更快、NLLB 上收益多大，都没有数据，不足以支持"收益明确"。

如果你认为加载耗时优先（总原则第一条"高性能"），可以接 C：产出与重载校验的代码已在 `ortcache.rs`（`produce`，含"重载失败则该部件退回原文件"），接入还差：缓存放 `<模型目录>/.opt/`（仿 STT）、元数据（ORT 版本 + 源文件大小 / mtime）校验、加载失败回退现状路径、产出时机（建议后台、会话释放后）。需要的话再说，我来接。

实验代码改动都不改变主流程行为：`build_session` 仍是原签名，新增的 `build_session_with` / `SessionExtra` / `Engine::load_with` 默认值等价于旧行为。

## TR-24：加载后预热

方法：A 档，加载后先译一条极短句 `Hi.`（预热）再译 `Hello world.`；与不预热交错 5 轮。

| | 首句 `Hello world.` 耗时 ms（5 次） | 预热本身 ms | 译完工作集 MiB |
|---|---|---|---|
| 不预热 | 280 / 168 / 156 / 200 / 191 | - | ~172 |
| 预热 | 168 / 197 / 150 / 173 / 213 | 257 / 281 / 260 / 423 / 346 | ~172 |

结论：**不保留。** 预热本身要 260~420 ms，首句只省约 0~30 ms（噪声内），等于把延迟从首次请求挪到了加载阶段，总量没降，还让"加载完成"晚了一截。预热后私有字节会多 ~11 MiB（arena 提前长出来），译完后与不预热持平。dev 档数据，release 可能不同，但没有支持保留的信号。

## TR-26：多段 batch 解码可行性评估（只评估，没改代码）

现状（`engine.rs`）：`translate` 把文本切成句级片段，逐段 `translate_segment` -> 每段一次 `run_encoder`（batch=1）+ 一个 decoder 循环（束搜索时 batch=束宽，`generate_beam`）。28~31 段 x 每段约 0.5 s = 14~18 s，基本就是"段数 x 单段解码步数 x 单步解码"，每个 decoder 步都是一次小矩阵 ORT 调用，CPU 利用率与调用开销占比高。

要改什么：

1. **编码器**：多段 padding 到最长长度，batch=N，传 `attention_mask`（`encoder_takes_mask` 已有；Marian 导出的编码器是否接受 mask 要逐模型确认，不接受就只能同长度分桶）。输出按段切开。改动小。
2. **解码器**：batch 维 = 段数 x 束宽。`decoder_inputs`、`fill_empty_past`、`store_present`、`reorder_kv`（按 parent 重排 KV）都要从"一条序列"改成"N 条序列 x 束"；`BeamSearch`（`beam.rs`，504 行）目前是单序列状态，要改成每段一个实例或批量版；段间完成时间不同，要么用完成掩码继续算（浪费算力），要么每步压缩 batch（KV 张量要重新 gather，拷贝开销）。
3. **KV 的 padding**：编码器侧 cross-attention KV 要带每段源长度的 mask；解码器侧各段生成长度不同，要对已结束的行喂 pad token 并屏蔽。
4. **min_length / no_repeat / forced_bos / trim_tail_repeat / 取消检查**：全部是按段逻辑，需要改成按行。
5. **输出一致性**：padding 与 batch 内数值差异可能让个别句子的束搜索结果与逐段不同（int4 MatMulNBits 在不同 M 下的累加顺序可能不同），需要新建"batch 与逐段结果一致"的对照测试；目前台账里的 NLLB 对照夹具是逐段的。

风险：

- 改动面大（`engine.rs` 解码主路径 + `beam.rs` + 测试），NLLB（m2m100）与 Marian、Hy-MT2（`chat.rs`，decoder-only，另一条路径）要分别验证；Hy-MT2 不适用这套改法。
- 内存：batch 化后 KV 与 logits 随 N x 束宽线性增长（NLLB 词表 ~10 万 x 束 2 x N 段 x 4 B = 每步 logits 约 N x 0.8 MiB），28 段一次做完峰值明显升高，需要限制每批段数（如 4~8 段）。这与"低内存"原则要权衡。
- 长度差异大的段 padding 浪费，需要按 token 长度排序分桶。

预期收益（估计，无实测）：解码步里的瓶颈是每步小 batch 的矩阵乘，int4 MatMulNBits 在 M 从 1 提到 4~8 时单步耗时增长远小于线性，常见经验是 4~8 段一批可拿到 2~3 倍总吞吐；14~18 s 有望降到 6~9 s。但这是外推，真实数字取决于 CPU 线程数与每步固定开销占比。

更便宜的备选（建议先做）：

- 段间并行：`intra_threads` 目前对单步小矩阵并行收益有限；起 2 个会话各处理一半段（内存 x2，不合"低内存"），或同会话多线程调用 `Session::run`（ORT 允许并发 run，权重共享，只多 KV/激活内存）。改动局限在 `translate` 的段循环，不碰解码内部，风险远低于真 batch。
- 先用日志拆 `translate_ms`：编码 / 解码步数 / 单步耗时，确认每步固定开销占比，再决定要不要做真 batch。

## 未覆盖项

- 冷页缓存：A/B/C 都没测冷启动（没有免管理员清页缓存的办法）。C 的 `.ort` 要整文件读入 + 校验，冷缓存下可能反而更慢，需要用户机器上用 RAMMap 之类清 standby 后补测。
- 只测了 opusmt-en-zh；NLLB-600M（外置权重 ~500 MiB）与 Hy-MT2 没测，B 的解码器 `If` 子图问题在 NLLB 上预计同样存在，未验证。
- dev 档（opt-level 2）数据，release 档没测；ws/私有字节来自 `K32GetProcessMemoryInfo`（`WorkingSetSize` / `PagefileUsage`）。
- TR-26 没有任何实测，估计值仅供排序参考。

## 改动文件

- `snow-shot-rs/tools/snow-translator/src/ortcache.rs`（新增：实验用产出 / 加载配置与探针测试）。
- `snow-shot-rs/tools/snow-translator/src/engine.rs`（`SessionExtra`、`build_session_with`、`Engine::load_with`；`load_parts` 多一个参数，默认行为不变）。
- `snow-shot-rs/tools/snow-translator/src/sysmem.rs`（`MemSnapshot` 增加 `private_bytes`）。
- `snow-shot-rs/tools/snow-translator/src/main.rs`（注册 `ortcache` 模块，`#[allow(dead_code)]`）。

复现：`cargo test --manifest-path snow-shot-rs/tools/snow-translator/Cargo.toml --no-run`，再用环境变量 `SNOW_ORT_DYLIB`、`SNOW_TR_PROBE_DIR`、`SNOW_TR_PROBE_VARIANT=A|B|C`（`SNOW_TR_PROBE_PREP=1` 先产出缓存，`SNOW_TR_PROBE_WARM=1` 开预热）运行测试 exe 的 `probe_variant --ignored --nocapture`。
