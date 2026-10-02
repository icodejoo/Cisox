# snow-translator

本地翻译工作进程：stdin/stdout 一行一条 JSON 的行协议（见 `src/protocol.rs`），空闲卸载等于进程退出。
独立 cargo workspace，`ort`（`load-dynamic`，运行时需 onnxruntime 1.28.0 的 `onnxruntime.dll`）与 `tokenizers` 不进主程序的构建。
动态库查找顺序：`SNOW_ORT_DYLIB`、`ORT_DYLIB_PATH`、可执行文件同目录。

支持三个模型族。`marian` 与 `m2m100` 是「编码器会话 + 带 KV cache 的合并解码器会话」，束搜索共用一套实现；`hunyuan_chat` 是 decoder-only（单会话）：

| family | 模型 | 语言码用法 |
|---|---|---|
| `marian` | opus-mt 系列 | 目标语言 token 放在源文本前缀（`source_prefix`） |
| `m2m100` | NLLB-200-distilled-600M（裁剪词表 + int4） | 编码器输入 `[源语言码] 词… </s>`；解码器以 `</s>`（id 2）起步，第一个生成位强制为目标语言码，随后束搜索到 `</s>` |
| `hunyuan_chat` | Hy-MT2-1.8B（int4，外部数据；用户自行下载的可选包） | 没有语言 token：目标语言名写进提示词（清单 `prompt`），单个 `model.onnx` 预填充 + 逐 token 解码共用，只做贪心 + repetition_penalty，**`num_beams` 被忽略（不报错）**，整段一次翻译（评测同口径），原文超过 `max_input_tokens` 才分句打包成多次请求 |

## 模型包（`model.json` schema_version=1）

旧清单（marian）无需任何改动。新增的字段都是可选的：

| 字段 | 说明 |
|---|---|
| `family` | `marian`、`m2m100` 或 `hunyuan_chat` |
| `files.encoder` / `decoder` / `tokenizer` | 编解码族必需，相对模型目录的路径；`hunyuan_chat` 改要 `files.model` 与 `tokenizer`（不要 encoder/decoder） |
| `files.model_data` | 仅 `hunyuan_chat`，可选：`model.onnx_data`，规则同下一行 |
| `files.encoder_data` / `decoder_data` | 可选：外部数据权重（`.onnx_data`）。文件名必须与 ONNX 图里记录的 `location` 一致、与 `.onnx` 同目录；ORT 在 `commit_from_file` 时自动内存映射，无需额外代码。列出后会校验存在，`sha256` 也可覆盖它们 |
| `languages` / `pairs` | 应用侧语言码（`zh-CN`、`en`…）；`pairs` 缺省时取 `languages` 的有向全排列 |
| `lang_tokens` | 应用语言码 → 词表里的语言 token。`m2m100` 必须覆盖每一个出现的语言，值是 FLORES 码（`zho_Hans`、`eng_Latn`…），worker 用 `tokenizer.token_to_id` 取 id |
| `generation.decoder_start_token_id` / `eos_token_id` / `pad_token_id` | 缺省回落到同目录 `generation_config.json`、`config.json`（NLLB：2 / 2 / 1） |
| `generation.bad_token_ids` | 禁止生成的 token；NLLB 设为 `[]`（HF 对 NLLB 不屏蔽任何 token） |
| `generation.num_beams` | 缺省束宽，`m2m100` 缺省 2（质量与 4 持平，内存与延迟明显更低），`marian` 缺省 1；请求（应用侧 `local_num_beams`，缺省 2）可覆盖，范围 1 到 8 |
| `generation.length_penalty` | 长度惩罚指数；`m2m100` 缺省 2.0（抑制偏短译文），`marian` 缺省 1.0 |
| `generation.no_repeat_ngram_size` | 缺省 3；NLLB 设 0（评测口径没有启用它） |
| `generation.min_length_ratio` | 仅 `m2m100`：生成步数（含强制的语言码）不足 `ceil(比例 × 输入 token 数)` 前禁止 `</s>`；`m2m100` 缺省 0.7，`marian` 缺省 0；显式写 0 即关闭，范围 0 到 1；输入 token 数含源语言码与结束符，语义同 HF `min_new_tokens` |
| `generation.max_new_tokens` | 缺省 512 |
| `prompt` | `hunyuan_chat` 必需：`prefix`（含 chat 模板的开头特殊 token）、`suffix`（generation prompt）、`template`（用户消息，必须含 `{target_lang}` 与 `{source_text}`）、`lang_names`（应用语言码 → 提示词里的语言名，必须覆盖 `languages` 与 `pairs` 里出现的目标语言）。最终提示词 = `prefix + 模板 + suffix` 后直接交给 `tokenizer.json` 分词（不加 fix_mistral_regex）；原文里与分词器特殊 token 同形的片段会先被剔除，避免被当成控制符 |
| `generation.repetition_penalty` | 仅 `hunyuan_chat`：与 HF 同公式（提示词与已生成 token 的正 logit 除以它、负 logit 乘以它），缺省 1.05（评测所用值），必须 > 0，1.0 关闭；`eos_token_id` 缺省回落 `generation_config.json`/`config.json` |
| `default_eligible` | 缺省 `true`。`false` 的包不参与应用侧的默认选包（任何路由模式），只在用户显式指定时使用；没有任何别的包支持该语言对时才兜底。Hy-MT2 包设为 `false`。worker 只透传，判定在 `snow-translate` 的 `router::pick_index` |
| `execution` | `intra_threads`、`cpu_arena`、`mem_pattern`、`opt_level`、`prepacking`、`trim_after_request`，缺省均为 ORT 默认（不要关预打包） |

NLLB 模型包目录示例（14 语言 int4，外部数据）：

```
model.json  config.json  generation_config.json  tokenizer.json
encoder.onnx  encoder.onnx_data  decoder.onnx  decoder.onnx_data
```

模型包还应带 `LICENSE-CC-BY-NC-4.0.txt` 与 `NOTICE.txt`（见 `docs/guides/translation-model-release.md`）。

Hy-MT2 包目录（`scripts/make_hymt2_pack.py` 生成）：

```
model.json  model.onnx  model.onnx_data  tokenizer.json
LICENSE-Apache-2.0.txt  NOTICE.txt  hymt2-1.8b-int4.manifest.json
```

`hunyuan_chat` 的内存与速度：权重由 ORT 对 `model.onnx_data` 内存映射，Rust 不拷贝；解码循环复用同一块 logits 缓冲，KV 张量从输出直接搬回输入，重复惩罚在 argmax 里折算、不复制 logits。预填充时模型图会输出全部位置的 logits（`n × 词表`，几十个 token 约几十 MiB 的临时输出），这是导出图的形状决定的，worker 只取最后一行。数据与分工见 `docs/guides/translation-model-release.md` §9。

## 解码后处理（NLLB）
目标为 `zho_Hans`/`zho_Hant`/`jpn_Jpan` 时，译文按片段做全角标点转换（`src/zh_punct.rs`，规则同 `eval/zh_punct.py`，对拍夹具 `tests/fixtures/zh_punct_golden.tsv`）：小数点、千分位、缩写、英文成句标点、纯英文括号保持半角，引号成对转 `“”`（日文 `「」`）。Marian 路径不变（仍用 `tidy_cjk_spacing`）。

## 分词器（NLLB 裁剪词表）

`tokenizer.json` 是 BPE 格式：词表取裁剪后保留的片段（id 与模型嵌入行一一对应），合并表取原版合并表里「左、右、合并结果都在词表内」的项，
归一化器沿用原版（含 sentencepiece 的 charsmap），末尾补首尾空白裁剪；特殊符号与语言码只放在词表里，不作为 added token（避免用户文本里的字面 `</s>` 或 `eng_Latn` 被当成控制符）。
worker 解码前自行剔除 `<s>`、`<pad>`、`</s>`、`<unk>` 与全部语言码。
不依赖原版 25.6 万词表。与评测环境「原版分词再映射 id」的差异见 `docs/guides/translation-model-release.md` 的实现说明。

## 脚本与夹具

- `scripts/make_nllb_pack.py`：由裁剪产物 + 量化后的 ONNX 生成模型包（`tokenizer.json`、外部数据版 `.onnx`、`model.json`）。
- `scripts/make_hymt2_pack.py`：由外部数据版 int4 ONNX（`eval/hymt/make_ext.py` 产物）生成 Hy-MT2 可选包（许可核对、`model.json`、NOTICE、校验和）。
- `scripts/make_nllb_fixtures.py`：生成分词金标准 `tests/fixtures/nllb_tokenizer_gold.json`（20 句 FLORES）。
- `scripts/make_nllb_parity.py`：用纯 ORT 束搜索生成译文参考 `tests/fixtures/nllb_parity.json`（需 ORT 1.28.0 的 Python 环境）。

## 测试

只跑相关测试（不要 `cargo test --workspace`）：

```
cargo test --bin snow-translator            # 单测；NLLB 分词对拍在模型包缺失时自动跳过
cargo clippy --all-targets -- -D warnings
# 真实模型：真实 worker 进程里的译文一致性 + 内存/延迟（机器安静时运行）
cargo test --release --test e2e real_nllb -- --ignored --nocapture
```

真实 Hy-MT2 对拍（只 2 句 x 2 向，BelowNormal，4 线程；参考由 `eval/hymt/ort_gen.py --version hymt2-pack-check --limit 2` 生成）：
`cargo test --release --test e2e real_hymt2 -- --ignored --nocapture`，环境变量 `SNOW_TRANSLATOR_HYMT_DIR`（缺省 `E:/models/translate-eval/hymt2-1.8b-int4-pack`）。

环境变量：`SNOW_TRANSLATOR_NLLB_DIR`（NLLB 模型包目录，缺省 `E:/models/translate-eval/nllb600m-main14-ccm-int4-ext`）、
`SNOW_ORT_DYLIB`（缺省回落到评测机的 ORT 1.28.0）、`SNOW_TRANSLATOR_TEST_MODEL_DIR`（Marian 真实模型用例）。
