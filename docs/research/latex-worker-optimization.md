# 公式识别 worker 优化调研（OCR-21 / 22 / 23）

状态：代码侧已落地，实测数据待补（2026-10-22 本机没有公式模型）。

## 1. 本次改动（`snow-shot-rs/tools/snow-latex/src/main.rs`）

- OCR-23 耗时日志：设环境变量 `SNOW_LATEX_TIMING=1` 才向 stderr 输出，前缀 `[latex-timing]`，stdout 协议不变。
  默认静默，因为宿主只在子进程提前退出时把 stderr 尾部拼进错误提示，常开会污染报错。
  内容：三个会话各自加载耗时、加载总耗时、加载后与退出时的工作集峰值/当前值、每张图 encode 耗时、
  解码步数与总耗时（下一张图 encode 前或退出时输出）、解码器是否带 KV cache。
- OCR-21 并行加载：三会话改为 `thread::scope` 并行（resizer 在当前线程，encoder/decoder 各一线程），
  错误优先级与旧串行一致（resizer > encoder > decoder），有单测。
- 预优化缓存：未实现。没有模型，无法量化收益；`Session::builder()` 仍是裸构建。
  量法：开 `SNOW_LATEX_TIMING=1`，连跑 3 次记录 `load encoder/decoder`，再对比给会话设
  `optimized_model_path` 后第二次起的加载耗时；收益小于 50 ms 就不值得多占磁盘。

## 2. KV cache（OCR-22）结论：当前模型不支持

worker 与宿主约定的解码器只有 3 个输入（token ids、mask、encoder context），
输出只有 logits，没有 `past_key_values*` 输入输出。`step` 里也硬检查了 3 个输入。
所以每步必须整段 token 重算，复杂度 O(n^2)，在 worker 侧无法单独加 cache。

worker 已加 `decoder_has_kv_cache`（只读输入名）并在计时日志里报告，拿到模型后第一次跑就能确认。

重导出方案（不改现有模型文件，产出新文件后再接）：

1. 从 RapidLaTeXOCR 的 PyTorch 权重重新导出 decoder，参考 translator 用的 merged decoder
   （`input_ids` + `encoder_hidden_states` + `use_cache_branch` + `past_key_values.N.{decoder,encoder}.{key,value}`）。
2. 导出时把自注意力与交叉注意力的 K/V 作为 present 输出；首步 `use_cache_branch=false`，之后每步只喂最新 1 个 token。
3. worker 的 `step` 协议改为带 cache 的增量：`{"op":"step","tokens":[最新一个]}`，cache 留在 worker，encode 时清空（和 context 同生命周期）。
4. 验收：同一批样例，新旧实现输出文本逐字一致（贪心解码，对比测试），并比较解码总耗时。
5. 需要先征得用户同意：重导出依赖 Python/PyTorch 环境，且新模型要走下载/分发渠道。

## 3. 未覆盖

- 前后实测数据：缺公式模型（`%LOCALAPPDATA%\Cisox\models` 下只有 ocr / stt / translate）。
  拿到 `image_resizer.onnx`、`encoder.onnx`、`decoder.onnx` 后，用 `SNOW_LATEX_TIMING=1` 对串行（git 旧版本）与并行各跑 3 次补表。
- 并行加载对峰值内存的影响：三个会话同时初始化，峰值可能略高于串行，需用内存日志确认。
