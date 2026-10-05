---
title: 翻译模型发布与授权方案
status: active
updated: 2026-10-05
summary: 本地翻译模型的最终选型（含 Hy-MT2 可选包 §9）、release 发布方式（A 直接发布现成文件 + B 提供自行生成脚本）、CC-BY-NC 授权声明与应用内提示要求、产物规格与待办
---

> **2026-10-05 资源已清理**：本文提到的 `E:\models\translate-eval\`（评测模型、fp32 导出、中间产物）和 `build/` 下的 `mt-quant`、`mt-venv`、`nllb-corpus`、`flores`、`hymt-eval` 等本地目录都已删除，下文的路径与数据是当时的记录。我们转换 / 量化的 5 个成品包已发布在 GitHub Release `models`（精确地址见 `snow-shot-rs/README.md`「模型下载地址」）；要复现实验，需按文中脚本重新下载原版模型再导出、量化。
## 结论先行（用户已拍板，2026-10-02）
- **模型**：NLLB-200-distilled-600M + 用 NLLB 自己的训练数据（CCMatrix 开头切片）选词表裁剪 + 对称 RTN int4（MatMulNBits，块 32）。实测依据见 `translation-quantization-benchmark.md`（相对原版 fp32 仅掉约 1.3 分，int8/int2/HQQ/mul-mul 均不如它）。
- **两个语言集都做**：联合国六语版（un6：中、英、法、西、俄、阿拉伯，V≈6.2 万，磁盘约 383MiB）与 14 语言版（main14：再加德、日、韩、葡、意、土、越、印尼，V≈10.1 万，约 484MiB）。内存优化验证后数字可能变化。
- **发布方式 A + B 都做**：A）release 附件里提供现成的两个模型文件，应用内按需下载；B）公开转换脚本，用户可自行从 HuggingFace 下载原版 NLLB 生成（可自定义语言集）。
- **授权**：NLLB 权重为 CC-BY-NC-4.0，我们的衍生文件沿用该许可，**仅限非商业使用、不用于生产部署**（官方模型卡的说明）；**用户已接受此风险**，要求在 README 与下载界面明确声明，**由用户自行斟酌风险**。本说明不是法律意见。

## 1. 发布产物规格（待内存优化验证后定稿）
| 产物 | 内容 | 预期体积 | 备注 |
|---|---|---|---|
| `nllb600m-un6-ccm-int4` | encoder/decoder ONNX（MatMulNBits int4）、裁剪词表 `sentencepiece.pruned.model`、`id_map.json`（新旧 id 映射）、语言码表、`model.json` | 约 383MiB | 联合国六语 |
| `nllb600m-main14-ccm-int4` | 同上 | 约 484MiB | 14 语言 |
| `LICENSE-CC-BY-NC-4.0.txt`、`NOTICE.txt` | 许可文本、署名、修改说明、非商业与不用于生产的声明、与 Meta 无隶属 | 小 | **每个模型包内必须带** |
| `manifest.json`（与 OCR 资产清单同风格） | 文件名、体积、SHA256、下载地址（GitHub Release 与镜像）、语言集、模型版本 | 小 | 应用据此下载与校验 |
GitHub Release 单个附件上限 2GiB，当前体积远低于上限；中国大陆可另放 ModelScope 镜像（OCR 模型已采用同样做法）。

## 2. 授权声明与应用内提示（必须做到）
1. **README**：已在 `snow-shot-rs/README.md` 加入中英双语声明（release 产物仅限非商业使用、不用于生产部署，风险由用户自行斟酌；来源、许可链接、修改说明、与 Meta 无隶属）。Release 说明文本也要复制同样声明。
2. **每个模型包**内带 `LICENSE-CC-BY-NC-4.0.txt` 与 `NOTICE.txt`（署名：Meta FAIR / NLLB Team，arXiv:2207.04672；说明做了词表裁剪与权重量化；标注 CC-BY-NC-4.0 与链接）。
3. **应用内**：下载前显示授权说明并让用户确认（"仅限非商业使用，不用于生产部署，风险自负"），设置页的模型管理处保留该说明；**不要**把模型随安装包捆绑，**不要**把模型文件说成属于 GPL 范围。
4. **转换脚本（B）**：脚本本身按仓库许可发布；脚本输出的模型文件仍受 CC-BY-NC 约束，脚本的说明里要同样声明。

## 3. B 方案：自行生成（给进阶用户，也用于自定义语言集）
流程：下载原版 `facebook/nllb-200-distilled-600M` → 下载 CCMatrix 开头切片统计词频（`ccmatrix_vocab_curve.py`）→ 按语言集裁剪词表（`prune_nllb_by_ids.py`）→ optimum 导出 ONNX（`export_quant_onnx.py`）→ int4 量化（`quantize_onnx.py`）。当前脚本是评测用的，路径写死、依赖 Python 3.14 的临时环境（torch、optimum、onnxruntime 等约 2GB），**要给用户用需要整理成一个一键脚本**（待办）：参数化路径与语言集、固定依赖版本、写清系统要求与耗时。

## 4. 待办与前置条件
- [ ] 内存优化验证（进行中）→ 定稿产物规格与预期内存。
- [x] Rust worker 适配：`m2m100` 家族（NLLB）、裁剪词表分词与语言码、外部数据内存映射、MatMulNBits 在 ORT 1.28.0 下的真实加载与译文一致性验证（2026-10-02 完成，见第 5 节；GatherBlockQuantized 4 位嵌入因不采用未验证）。
- [ ] Rust worker 复测：机器安静时重测延迟（本轮实测时机器 CPU 被其它任务占满，延迟读数不可作绝对值）与冷文件缓存下的首句延迟；大样本译文一致性（本轮 11 句）。
- [x] 默认解码配置落到 Rust（2026-10-02 完成，见 §5.6）：`num_beams=2`、`length_penalty=2.0`、最小长度=0.7×源 token 数、中日文全角标点后处理（依据与数据见 `../research/translation-zh-quality-and-candidates.md` §2）。
- [ ] 打包脚本：生成模型包、LICENSE/NOTICE、manifest（含 SHA256）；发布到 GitHub Release 与镜像。
- [ ] 应用内下载与授权提示界面（沿用 OCR 资产下载机制）。
- [ ] 把评测脚本整理成面向用户的 B 方案脚本。
- [ ] 补充评测：14 语言版里韩、葡、意、土、越、印尼 6 种语言尚未评测；每语向仅 30 句，需要更大样本。
- [x] OPUS-MT 英↔中小包：来源与许可核对、量化档选择、真实 worker 验证、`scripts/make_opusmt_pack.py`（2026-10-02 完成，见 §6）。
- [ ] OPUS-MT 小包（Hy-MT2 的待办见 §9.3）：应用内下载入口与"英→中提示可选此包"的文案/i18n；发布到 Release 与镜像（对外发布需用户确认）；机器安静时重测延迟。

## 5. Rust worker 实现说明（2026-10-02）
代码在 `snow-shot-rs/tools/snow-translator`（`src/engine.rs`、`src/manifest.rs`），用法与字段速查见该目录 `README.md`。Marian 路径与旧清单不变。

### 5.1 模型包格式（family `m2m100`）
```
model.json  config.json  generation_config.json  tokenizer.json
encoder.onnx  encoder.onnx_data  decoder.onnx  decoder.onnx_data
```
- `family` 取名 `m2m100`（与 HF 的 `m2m_100`、snow-translate 里已有的写法一致）；`marian` 不变。
- `files`：`encoder`/`decoder`/`tokenizer` 必需；`encoder_data`/`decoder_data` 可选，列出后会校验存在，`sha256` 同样可覆盖。`.onnx_data` 的文件名必须与 ONNX 图里记录的 `location` 一致（`scripts/make_nllb_pack.py` 写成 `encoder.onnx_data` / `decoder.onnx_data`），与 `.onnx` 同目录；`commit_from_file` 自动处理，Rust 里无需额外代码。
- `languages` / `pairs` 用应用语言码（`zh-CN`、`en`…）；`lang_tokens` 把它们映射到词表里的语言 token（FLORES 码，如 `zho_Hans`），`m2m100` 要求覆盖每个出现的语言。worker 用 `tokenizer.token_to_id` 取 id（14 语言版：`eng_Latn`=101233、`zho_Hans`=101244…）。特殊符号沿用 HF 约定且已核对：`<s>`=0、`<pad>`=1、`</s>`=2、`<unk>`=3，`decoder_start_token_id`=2。
- `generation`：`min_length_ratio`；`m2m100` 缺省 `num_beams=2`、`length_penalty=2.0`、`min_length_ratio=0.7`（清单可覆盖，Marian 缺省不变，见 §5.6）；NLLB 包另写 `no_repeat_ngram_size=0`（评测口径没有启用）、`bad_token_ids=[]`（HF 对 NLLB 不屏蔽 pad）、`max_new_tokens=256`。
- 推理流程：编码器输入 `[源语言码] + 词 + </s>`；解码器以 `</s>` 起步，第一个生成位强制为目标语言码（对数概率 0，不受 `bad_token_ids` 影响），之后束搜索到 `</s>`；解码前剔除 `<s>`、`<pad>`、`</s>`、`<unk>` 与语言码。optimum 导出的 merged decoder 与 Marian 的张量名一致（`input_ids`、`encoder_attention_mask`、`encoder_hidden_states`、`use_cache_branch`、`past_key_values.N.{decoder|encoder}.{key|value}`，形状 `[批, 16, 序列, 64]`），复用原有 KV cache 与束搜索代码。
- 与 Marian 的行为差异：NLLB 不做 `tidy_cjk_spacing`（该整理是为 Marian 中文输出写的）。NLLB 的中文/日文输出里逗号、冒号是半角（`他补充说:"我们…`），由 §5.6 的全角标点后处理转换。

### 5.2 会话选项
外部数据 + 内存映射 + `num_beams=2`，其余全部用 ORT 默认值：`opt_level=3`、预打包开（不要关）、arena 与内存模式开、`intra_threads=0`（自动）。这些取值写在 `ExecutionOptions` 缺省里，清单可覆盖。`.onnx_data` 使用期间的文件占用（本机在 Windows 10 上实测，worker 已加载模型）：删除被拒绝（拒绝访问）；改名与以写方式打开却都能成功，**原地覆盖映射中的文件会损坏进程里的权重页**，所以更新模型前必须先让 worker 退出（空闲卸载即退出进程）。

### 5.3 分词器路线
选「裁剪后词表生成 `tokenizer.json`」，Rust 用 `tokenizers` 直接读取，不依赖原版 25.6 万词表，也不需要 id 映射层。`scripts/make_nllb_pack.py` 由 `pieces.json` 与原版 `tokenizer.json` 生成：BPE 词表取保留片段（新 id），合并表只保留「左、右、合并结果都在词表里」的项，归一化器沿用原版（含 sentencepiece 的 charsmap）并在末尾补首尾空白裁剪（否则句尾空格会多出一个 `▁`），特殊符号与语言码只放词表、不作 added token（避免用户文本里的字面 `</s>`、`eng_Latn` 被当作控制符）。体积 4.5 MiB。

对拍结果：
- 夹具 `tests/fixtures/nllb_tokenizer_gold.json`（20 句 FLORES devtest，中日阿俄英法）：Rust 的 id 序列与「裁剪后 sentencepiece 重新分词」逐 id 一致 20/20；与评测用的 remap 分词一致 19/20，唯一不同的一句（俄语）remap 序列含 `<unk>`。
- 更大样本（Python 侧，14 语言 x 前 200 句 = 2800 句）：与裁剪后 sentencepiece 一致 2800/2800；与 remap 一致 2765/2800（98.75%），35 句不同**全部**是 remap 序列含 `<unk>` 的句子（remap 把被裁掉的片段落到 `<unk>`，重新分词则拆成更短的保留片段；无 `<unk>` 的句子 0 句不同）。
- 附带发现：中文弯引号 `“ ”`、书名号 `《 》`、法语 `’ « »`、长破折号等不在 NLLB 词表里，原版分词也给 `<unk>`（2800 句里 298 句含 `<unk>`，法语与中文最多，各约 75 句），不是裁剪造成的；输入端把这些符号规范化成 ASCII 可能提升质量，未做。

### 5.4 译文一致性（真实 worker 进程）
`#[ignore]` 集成测试 `tests/e2e.rs::real_nllb_parity_and_memory`：每个语言对起一个全新 worker 进程，加载 14 语言 int4 外部数据版，逐句翻译（zho→eng 3 句、eng→fra 3 句、eng→zho 3 句、jpn→eng 2 句，共 11 句，夹具 `tests/fixtures/nllb_parity.json`），与纯 ORT 束搜索（`purebeam.py`，beam=2、长度惩罚 1.0、无 no_repeat，ORT 1.28.0）的参考比较。结果：**11/11 逐字一致**，无论参考用 Rust 同一份分词（reseg）还是评测的 remap 分词（这 11 句里的 `<unk>` 句也得到相同译文）；同一进程内第二遍译文与第一遍逐字相同。
未覆盖的差异来源：①Rust 的 `BeamSearch` 早停用「最优存活束按 长度+1 归一化」与 purebeam 的「按步数」略有不同，样本小，未触发；②Rust worker 会先按句末标点分句再逐句翻译（长文本不会塞进一次解码），夹具只选单句文本，所以与 FLORES 整行翻译等价；多句文本的结果与整段翻译不同属预期；③浮点求和顺序在不同线程数下可能让极少数句子分叉，本轮同线程数配置下未见。

### 5.5 真实内存与延迟（Rust worker，release，ORT 1.28.0）
口径：外部进程每 10 ms 读 `K32GetProcessMemoryInfo`（与 Python 评测同一 API）；「私有」是 `PrivateUsage`（与 Python 的 pagefile 口径一致）；每个语言对一个新进程，11 句各翻译两遍，机器：Windows 10，20 逻辑核，32 GiB。**测量时机器 CPU 被其它评测任务占满（约 100%，等待 30 分钟未安静），所以延迟读数偏高且不可作绝对值，内存读数不受影响。**

| 指标（MiB） | Rust worker（4 个新进程的范围） | Python 纯 ORT（同一模型包、同一 11 句、每对一个新进程，增量 + 约 60 基线） |
|---|---|---|
| 未加载时（exe + 管道） | 3 | 基线 60（numpy + ORT 导入） |
| 加载后工作集 | **314 到 318**（私有 304 到 308） | 增量 278 到 279，约 339（私有增量 276 到 277） |
| 加载期峰值工作集 | 448 到 451 | 增量 412 到 413，约 473 |
| 翻译期峰值工作集（采样） | **421 到 447** | 增量 401 到 423，约 461 到 483 |
| 结束时工作集 / 私有 | 418 到 438 / 363 到 399 | 增量 388 到 403 / 336 到 367 |
| 加载耗时 | 4.4 到 7.2 s | 4.0 到 4.8 s |
| 每句延迟（机器占满时，beam=2） | 首句 4.6 到 7.7 s，均值 5.0 到 8.2 s，热均值 4.4 到 8.6 s | 均值 5.1 到 9.4 s |

- 对照 §12：外部数据 + beam=2 的 Python 数是加载后 279、峰值 461、结束私有 380（增量）。Rust worker 的绝对值（加载后约 315、翻译期峰值约 447）与之吻合：Rust 没有 Python 解释器与 numpy 的约 60 MiB，但多出 worker 自身与 ORT 动态库，两者的增量大致持平（加载后 Rust 高约 30 MiB 是因为未加载基线只有 3 MiB，Python 的 60 MiB 基线里已含 ORT 动态库）。
- 内存映射在 Rust 里生效：加载后工作集 315 MiB，远低于权重全驻留的 540 MiB（§12 基线）。翻译期峰值没有高出加载期峰值（约 449），进程峰值出现在加载期，与 §12.2 对预打包瞬时峰值的解释一致。
- 冷启动：首句延迟与同一进程里后续句没有可分辨的差别（机器占满，噪声远大于差异）；**文件缓存是热的**，没有测磁盘冷读，也没有测系统内存紧张后映射页被回收的再访问，仍是待办。
- 差异说明：Python 数是「增量 = 读数 - 启动基线」，Rust 数是绝对读数（空闲 3 MiB），口径不同，比较时用绝对值（Python 增量 + 60）或增量（Rust 读数 - 3）。

### 5.5.1 已知限制
- `zh-TW`（`zho_Hant`）不在 14 语言词表里，该包不支持繁体；snow-translate 的 `Lang` 没有越南语、印尼语，这两种语言在应用侧会被忽略（清单里仍然保留）。
- 生成长度：`max_new_tokens=256`，输入 token 上限沿用清单 `max_input_tokens`（缺省 512）。
- 需要同步的下游：打包脚本要把 `.onnx_data` 一并分发与校验（`files` 里列出 + `sha256`）；授权文件仍需进包。

### 5.6 解码配置与后处理（2026-10-02）
- **参数**（`m2m100` 缺省，`model.json` 的 `generation` 可覆盖；`marian` 与旧清单缺省不变：beam=1、lp=1.0、无最小长度）：`num_beams=2`、`length_penalty=2.0`、`min_length_ratio=0.7`。最小长度 = `ceil(0.7 × 编码器输入 token 数)`，输入含源语言码与 `</s>`；解码步序号（从 0 起，第 0 步是强制的目标语言码，**计入已生成长度**，解码起始符不计）小于它时屏蔽 `</s>`，与 HF `min_new_tokens` 语义、`purebeam.py` 的 `step<minlen` 一致。长度惩罚 = 得分 / (已生成长度+1)^lp。
- **束搜索对拍发现并修正的差异**：存活束在早停比较里原先也按「长度+1」归一化，`purebeam.py` 用 `max(ns)/(step+1)**lp`（不加 1）。lp=1 时几乎看不出，lp=2 时 Rust 收束更晚，会选出更长、不同的译文（对拍 14 句里 en→ja 一句不同）。已改成与 purebeam 一致（`beam.rs` 早停处），改后对拍 14/14 逐字一致。其余逐点核对一致：完成假设归一化、只有排名前 width 的 `</s>` 才算完成、达到 `max_new` 时存活束参与评比。
- **数据流**：应用设置 `screenshot_translation/local_num_beams`（`snow-config`，**缺省原为 4，现改为 2**，1~8，低内存模式在 `translate_service` 里强制 1）→ `WorkerConfig.num_beams` → 每条翻译命令的 `num_beams: Some(n)` → worker 的 `resolve_beams(请求, 清单缺省)`，**请求优先于清单**。所以应用侧缺省 4 会盖掉清单的 2，已把应用缺省改成 2（只改缺省值与测试，设置页 UI 与 i18n 另做）。已保存过旧缺省 4 的用户配置不会被迁移。长度惩罚与最小长度没有应用侧开关，只走清单缺省。
- **全角标点后处理**（`src/zh_punct.rs`，移植自 `eval/zh_punct.py`）：只对 NLLB 且目标为 `zho_Hans`/`zho_Hant`/`jpn_Jpan` 生效，Marian 不动。`, . : ; ! ?` 按上下文转全角（日文逗号为 `、`，无 `:` `;`），`"` 成对转 `“”`（日文 `「」`，引号计数在句子片段间延续），含中日文的 `( )` 转全角；小数点、千分位、`U.S.` 类缩写、英文成句里的标点、纯英文括号保持不动；全角标点前后的空白去掉。沿用 Python 的一个行为：文本末尾紧跟 ASCII 字母的句点按缩写保留。
- **清单与已有包**：`scripts/make_nllb_pack.py` 现在写 `length_penalty=2.0`、`min_length_ratio=0.7`。评测包 `E:\models\translate-eval\nllb600m-main14-ccm-int4-ext\model.json` 已同步（原文件备份为 `model.json.bak`），否则它显式写的 `length_penalty=1.0` 会盖掉新缺省。夹具 `tests/fixtures/nllb_parity.json` 用 `scripts/make_nllb_parity.py` 按新参数重新生成（purebeam `--lp 2.0 --min-ratio 0.7`，中日文参考过 `zh_punct.py`，新增 en→ja 3 句，共 14 句）。
- **对拍结果**：①全角标点：夹具 `tests/fixtures/zh_punct_golden.tsv`（96 条：单测用例与边界 + 评测目录里 eng→zho、fra→zho、eng→jpn 的真实译文，约 23KB），Rust 与 Python 逐字一致。②真实 worker（`real_nllb_parity_and_memory`）：14 句与 purebeam（beam=2、lp=2.0、min-ratio 0.7）逐字一致 14/14。③`real_nllb_decode_config_check`：评测 src.txt 前 10 句 × eng→zho / fra→zho / eng→jpn，与 purebeam 整行译文逐字一致 24/30，偏短句数（译文字数/参考字数<0.7）：旧配置（beam=2、lp=1.0，purebeam）12 → 新配置 purebeam 4 → Rust 新缺省 3；全角标点全部生效。
- **已知差异（那 6 句）**：评测里 purebeam 是整行一次解码且用 remap 分词，Rust 先按句末标点分句再逐句翻译、用重新分词的 id；所以含多句或 `Dr.` 的行不同。分句器会把 `Dr. Ehud Ur` 在 `Dr.` 后切开（空白前的句点就当句末，没有缩写表），导致 eng→zho 第 2 句译成「医生：…」，是已有行为，**未改**，建议后续给分句器加常见缩写表。另有模型自己输出的游离引号会让引号对错位（如日文第 5 句末尾「…十分だ「」），属模型输出，后处理不修。
- 延迟：测试时机器另有 Hy-MT2 评测占用 CPU，**不作延迟结论**。

## 6. OPUS-MT 英↔中小包（2026-10-02）
定位：NLLB 之外**额外可选下载**的专用小包（英→中、中→英各一个包），英→中质量更稳，体积和内存都只有 NLLB 的一半以下。不替换 NLLB 默认包。

### 6.1 来源与授权（HF 页面与仓库 README/metadata 原文核对，2026-10-02）
| 包 | 上游 | 许可（模型卡 `license:` 字段原文） | 署名要求 |
|---|---|---|---|
| 英→中 | `Helsinki-NLP/opus-mt-en-zh`（Tatoeba-Challenge eng-zho，2020-07-17 训练，HF 最后修改 2023-08-16） | **Apache-2.0**（卡片元数据与 README 头部都是 `license: apache-2.0`；README 正文没有写许可章节） | 随附许可全文与 NOTICE，保留署名、标明修改（Apache-2.0 第 4 条） |
| 中→英 | `Helsinki-NLP/opus-mt-zh-en`（同系列，同日训练） | **CC-BY-4.0**（头部与正文 `License: CC-BY-4.0` 一致） | 署名创作者、给出许可链接、**标明是否做了修改**、给出原材料链接（CC BY 4.0 第 3 条） |
- 两者都允许商用，与 NLLB 的 CC-BY-NC 不同；**没有"非商业"限制**，所以这两个包的下载界面不需要 NLLB 那段"仅限非商业"声明，但仍要显示署名与修改说明。
- 两个方向许可不一致（Apache-2.0 与 CC-BY-4.0）是 HF 上的实际状态，早先调研文档写的"预期 CC-BY-4.0"对英→中不成立。NOTICE 与许可文件按各自方向的卡片许可生成（`make_opusmt_pack.py` 读 README 的 `license:` 字段，未知许可会直接报错退出，避免误署），署名内容两边同样完整（创作者、来源链接、引用、修改说明、无隶属、非法律意见）。
- 许可全文来源：Apache-2.0 用仓库 `licenses/Apache-2.0.txt`；CC-BY-4.0 取自 creativecommons.org 的 `legalcode.txt`（18,657 字节）。HF 上的 CT2/ONNX 转换仓库（gaudi、Xenova 等）是第三方转换，卡片许可可能与原卡不同，**本包不依赖它们**：权重来自 Helsinki-NLP 原仓库的 `pytorch_model.bin`，ONNX 与 tokenizer 都自己生成。
- 本节不是法律意见。

### 6.2 量化档对比（数据）
口径：FLORES-200 devtest，第 1~30 句（A）与第 31~60 句（B），beam=4（OPUS-MT 官方默认），**chrF 为字符 1~6 阶、去空白、β=2、无词级项**（`eval/chrf.py` 的 `word_order=0`，与 NLLB 同函数同口径；英→中即字符级，中→英即常规 chrF）。Python ORT 1.30.0、4 线程，译文为模型原始输出（未做后处理）。内存是整个 Python 进程（含约 220MiB 的 torch/optimum 基线），只看相对关系。机器当时被 Hy-MT2 等评测占用，**延迟不可比较**。

| 方向 | 档 | chrF A | chrF B | 磁盘（encoder+decoder） | Python 进程工作集（加载后/翻译峰值） | 备注 |
|---|---|---|---|---|---|---|
| en→zh | fp32 | 30.37 | 33.61 | 552 MiB | 956 / 986 | 基线 |
| en→zh | int8 动态（`quantize_dynamic`） | 30.11 | 未测 | 140 MiB | 548 / 578 | 能用，但见下 |
| en→zh | int8 仅权重（MatMulNBits 8 位） | 30.44 | 未测 | 148 MiB | 555 / 706 | **极慢**（约 9 s/句，同负载下其它档约 1 s） |
| en→zh | **int4 对称 RTN 块 32** | **30.35** | **33.55** | **111 MiB** | **521 / 547** | 相对 fp32 −0.02 / −0.06 |
| en→zh | int4 块 16 | 28.58 | 34.70 | 120 MiB | 528 / — | 两段方向相反，是噪声 |
| zh→en | fp32 | 52.62 | 56.05 | 552 MiB | 956 / 992 | 基线 |
| zh→en | **int8 动态** | **6.86** | 未测 | 140 MiB | 546 / 635 | **整档崩溃**：输出重复词（"the the the…"） |
| zh→en | int8 动态、嵌入不量化 | 7.10 | 未测 | 331 MiB | 739 / 836 | 仍崩，说明问题在 MatMul 激活量化，不在嵌入 |
| zh→en | int8 仅权重 | 52.25 | 未测 | 148 MiB | 554 / 719 | 质量好但极慢（约 10 s/句） |
| zh→en | int4 块 32 | 51.43 | 54.53 | 111 MiB | 520 / 562 | 相对 fp32 −1.19 / −1.52 |
| zh→en | **int4 块 16** | **51.78** | **55.54** | 120 MiB | 529 / — | 相对 fp32 −0.84 / −0.51 |
- **取舍**：int8 动态量化对这个模型**不可靠**（zh→en 整档崩溃，en→zh 碰巧没事），不选。int8 仅权重质量好，但 ORT 的 8 位 MatMulNBits 在 CPU 上慢约一个数量级，不选。剩下 fp32 与 int4：
  - en→zh：int4 块 32 相对 fp32 损失 ≤0.06 分，体积 1/5，**选 int4 块 32**，没有取舍可谈。
  - zh→en：int4 块 32 损失 1.19 / 1.52 分（>1 分）。fp32 体积 552 MiB（5 倍）、worker 内存约 3.5 倍（见 6.4），按总原则"低内存"优先，不选 fp32；改用**块 16**，损失降到 0.84 / 0.51 分，体积只多 9 MiB，**选 int4 块 16**。块 16 在 zh→en 两段数据上都比块 32 好，但每段仅 30 句，改善幅度与噪声同量级，结论是"成本几乎为零、方向一致"，不是"已证明更好"；若要避免对拍两个方向用不同块大小，退路是两边都用块 32 并接受 zh→en 约 1.3 分损失。
  - 想要最高质量的用户：fp32 包可用同一脚本生成（`--quant fp32`，不加 `--external-data` 亦可），本次未做成发布件。
- 与 NLLB 同口径对比（同一批 30 句，字符级 chrF，Python 评测原始输出）：en→zh OPUS-MT int4 **30.35**，NLLB 14 语言 int4 默认解码 22.76、调优解码 27.76（加全角标点后处理 30.81）、fp32 原版 25.88。OPUS-MT 的原始输出（半角标点）比 NLLB 调优后高约 2.6 分；经 worker 的全角标点整理后差距缩小（前 10 句粗测：OPUS-MT 原始 26.1、经 worker 整理 29.1，NLLB 调优 27.3）。**所以结论比此前"OPUS-MT 33.9 对 NLLB 24.8"温和得多**：研究文档里那组数字（33.9/28.0/24.8）与本脚本的绝对值有约 2 分的系统差（同一批 NLLB 句子在本脚本里是 22.76/25.88），相对顺序一致，但 OPUS-MT 英→中相对"调优后的 NLLB"优势只有约 2~3 分，不是 9 分；Hy-MT2-1.8B int4 为 39.67（体量 1GB 级）。zh→en 方向 NLLB 同口径对比未做。
- 繁体：`>>cmn_Hant<<` 前缀可输出繁体（前 5 句试过，用字正确，但 3/5 句在逗号处截断，第 5 句出现字间空格乱码）。**不进包的语言表**，只记录。

### 6.3 包格式与生成脚本
- 每方向一个包，目录 `opusmt-<方向>-int4-pack\`（外部数据，`.onnx_data` 与 `.onnx` 同目录，ORT 内存映射）：`model.json`、`encoder.onnx(+_data)`、`decoder.onnx(+_data)`、`tokenizer.json`、`config.json`、`generation_config.json`、`LICENSE-<Apache-2.0|CC-BY-4.0>.txt`、`NOTICE.txt`、`opusmt-<方向>-int4.manifest.json`。
- `model.json`（schema_version=1，family `marian`，worker 无需任何改动）：`languages`/`pairs` 只含该方向（en→zh：`["en","zh-CN"]`，`lang_tokens` `zh-CN` → `>>cmn_Hans<<`，`source_prefix` `{tgt_token} `；zh→en：无前缀）；`generation`：`decoder_start_token_id=65000`、`eos=0`、`pad=65000`、`bad_token_ids=[65000]`、`num_beams=4`（官方默认；**应用设置里 `local_num_beams` 缺省 2 会盖掉清单，见 §5.6，OPUS-MT 包应单独评估 beam=2**）、`length_penalty=1.0`、`no_repeat_ngram_size=0`（与评测一致，不用 worker 的缺省 3）、`max_new_tokens=256`；`sha256` 覆盖 encoder/decoder/tokenizer 及两个 `_data`，worker 加载前校验。
- 发布 manifest `opusmt-<方向>-int4.manifest.json`：与 OCR runtime manifest 同风格（`schema=1`、`files[{name,size,sha256}]`），另含 `kind`、`id`、`version`、`pairs`、`quantization`、`license`、`upstream`、`total_size`。下载地址由发布流程填（未发布，没有 url 字段）。
- 包大小：en→zh 113.3 MiB，zh→en（块 16）122.5 MiB；NLLB 14 语言约 484 MiB。
- 生成：`snow-shot-rs/tools/snow-translator/scripts/make_opusmt_pack.py --hf-dir <HF 目录> --onnx <量化后 ONNX 目录> --direction en-zh|zh-en --quant int4 --quant-note "…" --license <许可全文> --external-data --out <包目录>`。前置：`eval/export_quant_onnx.py` 导出 fp32，`eval/quantize_onnx.py --mode int4 [--block 16]` 量化，`eval/opusmt/eval_opusmt.py` 评测。本机产物在 `E:\models\translate-eval\opusmt-en-zh-int4-pack\` 与 `opusmt-zh-en-int4-pack\`。
- tokenizer：自己由 `source.spm`、`target.spm`、`vocab.json` 生成（Unigram，id 与联合词表一致；源端没有的片段给 −100 分，只用于解码；归一化器内嵌 spm 的 `precompiled_charsmap`；不依赖 Xenova 的 tokenizer.json，它的卡片没有写许可）。对拍 HF `MarianTokenizer`（FLORES 前 300 句）：**解码 300/300 一致，zh→en 编码 300/300 一致，en→zh 编码 291/300 一致**，9 句差异是个别罕见词（如 ZMapp）的切分不同，不影响可用性。

### 6.4 真实 worker 验证（Rust，release，ORT 1.28.0，beam=4）
取评测 `src.txt` 前 10 句，每方向一个新进程，每句一个 `translate` 请求；worker 与 Python 一样先分句再逐句翻译，所以参考也按同样规则分句后用 Python ORT 翻译，比较时忽略空白和半角/全角标点（worker 会做中文排版整理）。脚本：`eval/opusmt/run_worker.py`、`eval/opusmt/compare_parity.py`。
- **译文一致：en→zh 10/10、zh→en 10/10**（int4 块 32 与块 16 两个 zh→en 包都验证了；外部数据版与内联版也一致）。含 `Dr.` 的句子被 worker 的分句器在 `Dr.` 后切开（已有行为，见 §5.6），译文因此与"整行翻译"不同，但与"同样分句的参考"一致。
- 内存（MiB，机器**当时 CPU 55~100% 被占用**，内存读数不受影响，延迟只作粗略参考）：

| 包 | 加载后工作集 | 加载期峰值 | 翻译期峰值 | 每句均值 / p50 | 加载耗时 |
|---|---|---|---|---|---|
| en→zh int4 块 32，外部数据（发布形态） | **149** | 180 | **198** | 0.77~1.67 s / 0.65~1.4 s | 1.6~4.4 s |
| zh→en int4 块 16，外部数据（发布形态） | **149** | 180 | **209** | 1.25 s / 1.18 s | 3.6 s |
| en→zh int4 内联（对照） | 225 | 257 | 265 | 1.5~1.8 s / 1.2~1.6 s | 3.6~8.5 s |
| en→zh fp32 内联（对照） | 662 | 916 | 694 | 0.96 s / 0.87 s | 6.4 s |
| 对照：NLLB 14 语言 int4 外部数据 | 315 | 449 | 447 | 约 5 s（机器占满） | 4.4~7.2 s |
- 外部数据（内存映射）比内联少约 75 MiB，加载也更快，发布形态用它。OPUS-MT int4 的翻译期峰值约 200 MiB，**在 300MB 目标内**，NLLB 的 447 MiB 不在。延迟列在机器忙时波动大（同一包重复跑 0.77~1.67 s），各档之间的延迟差异淹没在负载噪声里，**不能据此排序**；安静机器上的延迟待复测。
- 最大限制：en→zh 常在逗号处提前结束（前 10 句里 #7 "…一集后，"），这是 OPUS-MT en→zh 模型自身行为（Python 同样截断），不是 worker 或量化造成的；worker 没有 `min_length_ratio`（仅 `m2m100` 支持）。

### 6.5 与 NLLB 包的关系与应用内选择建议
- 英→中是 NLLB 最弱的方向：建议在设置页"本地翻译模型"里，当源/目标为英→中（或检测到 NLLB 已装而用户频繁翻 en→zh）时，提示"可另外下载 OPUS-MT 英→中小包（约 113 MiB，质量更稳、内存更小，许可为 Apache-2.0 可商用）"。中→英同理（约 123 MiB，CC-BY-4.0）。
- 选择逻辑建议：已安装对应方向的 OPUS-MT 包时，该方向优先用它；其它语向仍走 NLLB。两个包都只含一个方向，别的语向请求会得到 `unsupported_pair`，应用侧据此回退 NLLB。同一时刻只会加载一个模型（空闲卸载即进程退出）。
- 下载界面：OPUS-MT 包不需要"仅限非商业"声明，但要展示 NOTICE 的署名与修改说明（CC-BY-4.0 包尤其必须）。

## 7. 翻译路由与混合拆分（2026-10-02，已提交 227db5e5、a55f0c89、d4e067e4、57a2f308）

**设置项**（`snow-config/src/extensions.rs`）：`screenshot_translation/local_route_mode`（`single` / `specialized_first` 默认 / `mixed_split`）、`local_max_resident_models`（1~4，默认 1）。

**模式**
- `single`：用户手选哪个包就用哪个。
- `specialized_first`：未指定包时，`pairs` 非空且覆盖语向的窄包（如 OPUS-MT 英→中）优先于通用多语包（NLLB）；用户显式指定仍尊重。
- `mixed_split`：按脚本把文本切成片段，英文片段走专用包，其余走 NLLB，按引擎分组后按原序拼回。

**实现位置**（`snow-translate/src/`）：`router.rs`（`RoutedEngine`，每包一个懒加载 worker，常驻上限内只驱逐空闲包，卸载在池锁外执行，`shutdown` 有界超时）、`script_split.rs`（`ScriptSplitter` 默认识别器）、`segment.rs`（`Segment`/`SegmentSplitter` 接口、`NoSplit`、`merge_short_segments`）、`lib.rs`（`is_specialized`、`pick_model_routed`）。worker 侧另有 `snow-translator/src/zh_punct.rs`（中日文全角标点后处理）与 `chat.rs`（chat 引擎）。宿主侧见 `snow-shot/src/translate_service.rs` 的 `build_local`。

**识别器规则**：汉字→中文，假名→日语（汉字邻近假名整块判日语），谚文→韩语，西里尔→俄语，阿拉伯文→阿拉伯语；其余脚本为未知，按用户源语言兜底。拉丁字母默认英语，仅当含足够多非英语特征字母（德 ä ö ü ß、西 ñ ¿ ¡、葡 ã õ、法 è ê à ç œ 等、土 ı ğ ş；至少 2 个且占字母 1/16 以上、有唯一领先语言）才判非英语；é á í ó ú 不计分。不用 lingua（+50 MB、常驻 18 MiB 不可释放、短句夹杂判错）。

**已完成（a55f0c89 起）**
- 源语言 Auto 时逐条用脚本识别器判定语言，按语言分组选包，不再被专用包钉成英文；识别不出的条目按用户源语言兜底。没有任何包支持的语向，在加载模型前就报 `UnsupportedLanguagePair`（`translate_service.rs`）。
- 日语汉字夹短拉丁（至多 6 个字符）时桥接到假名，整块判日语（d4e067e4）。
- 译文标签显示实际参与翻译的包：路由器记录 `used_ids`，`mixed_split` 的预选标签跳过 `default_eligible=false` 的可选包（只有它覆盖时才退回）（57a2f308）。
- 设置页已有路由模式、常驻数与 Hy-MT2 说明区（606ea799，见 §9.3）。

**已知限制**
- 纯汉字文本仍判中文，日语只有汉字没有假名时会误判（识别器的已知局限）。
- 西里尔、阿拉伯文的 hint 机制已就位，但 `Lang` 枚举目前只有俄语、阿拉伯语，没有乌克兰语、波斯语等，所以暂无实际影响；加语言时在 `script_split.rs` 补充。
- 拉丁字母里没有特征字母的法/德/西等句子仍当英语。
- `used_ids` 是路由器上的全局记录，并发翻译可能互相覆盖，只影响标签显示，不影响译文。
- 端到端的 label 显示没测过，目前只有离屏单测与路由器假引擎测试。
- 一个包卸载过程中仍占常驻名额，慢卸载期间常驻数会短暂超限；`shutdown` 超时只是调用方不再等。
- 路由器测试用假引擎，真实 `WorkerEngine` 路径未测。

## 8. 评测与清理约定（2026-10-02）

- 评测只测 int4，不再测 int8/fp32；评测进程低优先级、限线程、限时回报。
- 清理过的中间目录：OPUS-MT 的 int4/int4b16/int8/int8wo 中间包、`hymt2-1.8b-onnx-fp32`（8 GB，可由 `eval/hymt/export_hymt.py` 重新生成）。
- 保留：`E:\models\translate-eval\` 下 `opusmt-{en-zh,zh-en}-int4-pack`、`hymt2-1.8b-int4`、`hymt2-1.8b-int4-ext`、`nllb600m-main14-ccm-int4-ext`。
- Hy-MT2 减体积结论见 `translation-hymt2-eval.md` §8：只有外部数据加 mmap 有效（加载后约 953 MiB、峰值约 1261 MiB），词表裁剪与嵌入共享都不值得。
- 同口径提醒：研究文档里 OPUS-MT 与 NLLB 英→中差 9 分，子代理用另一套脚本复测只差 2~3 分，NLLB 英→中在不同脚本下分别为 22.8 / 27.76，横向比较需同脚本同句。

## 9. Hy-MT2-1.8B 可选包（2026-10-02，用户自行下载，不是默认）

定位：和 NLLB（默认）、OPUS-MT 英↔中（可选）并列的第三个包，**中日互译与英→中质量更好、许可 Apache-2.0 可商用**；欧洲语言目标（英→法 -5.5、阿→英 -4.2 等）不如 NLLB，体积与内存是 NLLB 的 2 到 3 倍、延迟约 5 倍。数据与结论见 `translation-hymt2-eval.md`（FLORES 前 30 句：英→中 chrF 39.7 对 NLLB 22.8，中→英 55.6 对 52.1；中日互译与日→英**没有单独评测**，英→日 43.8）。

### 9.1 规格

| 项 | 值 |
|---|---|
| 包目录 | `hymt2-1.8b-int4-pack\`：`model.json`、`model.onnx`、`model.onnx_data`、`tokenizer.json`、`LICENSE-Apache-2.0.txt`、`NOTICE.txt`、`hymt2-1.8b-int4.manifest.json` |
| 体积 | 约 1.3 GiB（`model.onnx_data` 1.27 GiB + tokenizer 9 MiB），下载总量 1,379,169,302 字节 |
| 量化 | MatMulNBits int4 对称 RTN 块 32（含 lm_head），嵌入 int8 Gather，权重未改动 |
| 内存（Rust worker，release，外部数据内存映射） | 加载后工作集约 950 MiB，翻译期峰值约 1.2 到 1.3 GiB（实测 1241 / 1216 MiB，私有内存加载后约 940 MiB） |
| 延迟 | 每句约 9 到 17 秒（4 线程，BelowNormal，机器有别的负载，平均 33 个生成 token），适合短文本与逐句；长段落要等 |
| 许可 | Apache-2.0：HF 模型卡 `license: apache-2.0`，模型目录 `LICENSE.txt` 头部 "Copyright (C) 2026 Tencent … licensed under the Apache License, Version 2.0"；包内 `LICENSE-Apache-2.0.txt` 即该文件原文，NOTICE 写明来源、版权、修改（转 ONNX、int4 量化、外部数据、加清单）、无隶属、非法律意见。`make_hymt2_pack.py` 会核对卡片 license 字段，不是 apache-2.0 直接退出 |
| 声明的语向（`pairs`） | 中、英、日六向全排列 + 评测过的 英↔法、俄→英、阿→英、英→西、中→法、法→中、俄→西、阿→法，共 15 向。模型卡还列了韩、德、意、葡、土等，没评测不声明（`prompt.lang_names` 已含全部 13 个应用语言名，扩 `pairs` 即可） |

### 9.2 worker 与路由实现

- **worker**：新增 `family=hunyuan_chat`（`src/chat.rs`）。清单 `files.model`（+`model_data`）、`prompt`（前缀、后缀、模板、语言名表）、`generation.repetition_penalty`，不写死在 Rust；加载单个 `model.onnx`（外部数据由 ORT 内存映射），探测 `past_key_values.*` 形状；贪心 + 与 HF 同公式的 repetition_penalty（缺省 1.05，提示词与已生成 token 都计入），eos 120020 / 最大 512 新 token 停止，输出去首尾空白。`num_beams` 请求字段对它无效但不报错。整段一次翻译（评测同口径），原文超过 `max_input_tokens` 才分句打包。详见 `snow-translator/README.md`。
- **路由不抢默认**：`ModelManifest`（`snow-translate`）新增 `default_eligible`（缺省 `true`，worker 清单同名透传）。`router::pick_index` 在没有指定包时只从 `default_eligible` 的包里选（`single` 取第一个，`specialized_first` / `mixed_split` 取最窄专用包），**Hy-MT2 包设为 `false`**，因此即使它声明了 `pairs`、id 排在前面，也不会被默认选中；用户在设置里手动指定模型 ID 时照常使用；只有别的包都不支持该语向时才作为兜底（不报"不支持"）。其余包的行为不变（旧清单没有该字段即 `true`）。
- **真实对拍**（`cargo test --release --test e2e real_hymt2 -- --ignored --nocapture`）：评测前 2 句，英→中与中→英各起一个 worker（BelowNormal，4 线程），共 4 句译文与 `ort_gen.py`（ORT 1.30，同一个包）输出**逐字一致 4/4**。

### 9.3 进度与待办

**已完成**
- [x] 设置页 Hy-MT2 说明区（606ea799、57a2f308）：说明文字与「使用该模型」按钮；面板逻辑抽成独立模块并有离屏测试；文案 `translate_settings.ftl` 已有 en-US、zh-CN、zh-TW 三份。
- [x] `mixed_split` 标签不列 `default_eligible=false` 的包（57a2f308）。
- [x] `chat.rs` 加载时校验输出名与 KV 类型：缺 KV 报错，非 float32 的 KV 报错（60d5824b）。

**未完成 / 待决**
- [ ] 下载地址与发布到 Release、镜像（对外动作，需用户确认）。`hymt2-1.8b-int4.manifest.json` 没有 url 字段，下载地址由发布流程填。设置页的「下载」按钮目前只是占位，点击仅提示手动把模型文件夹放进翻译模型目录，没有真正下载。
- [ ] f16 KV：需要新增依赖（ORT 的 f16 张量支持），尚未获用户同意，目前遇到 f16 KV 直接报错。
- [ ] 设置页没做真机渲染验证（只有离屏测试）；端到端的 label 显示也没测。
- [ ] `pairs` 扩到韩、德、意、葡、土：先评测，没评测不声明。
- [x] zh-TW 已于 2026-10-02 移除，界面只支持 en-US 与 zh-CN。
- [ ] 中↔日、日→英及其余语言没评测；机器安静时重测延迟；`make_hymt2_pack.py` 的 `HINTS` 数据来自评测机，换机器需更新。
- [ ] `materials/` 目录仍未入库（git 里是未跟踪状态），是否入库待用户定。
