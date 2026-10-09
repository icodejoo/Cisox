# snow-shot-rs

Cisox 的 Rust + GPUI 新 workspace，与 Qt 版并行存在。方案见 `../docs/cisox-gpui-migration-plan.md`。

## 构建与测试

```
cargo check --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all -- --check
cargo test --workspace
```

工具链由仓库根 `rust-toolchain.toml` 固定（Rust 1.97.1）。

## 目录与后续任务对应

`snow-shot` 新增模块：
- `window_pick.rs` — 截图选区智能窗口识别（悬停高亮、单击选中、手动框选；设置项"智能选择"，默认开）
- `annotation_style.rs` — 标注样式面板（颜色、线宽、字号、填充、箭头头型；按工具记忆）

| crate | 职责 | 阶段 / 填充任务 |
|---|---|---|
| `snow-shot` | bin 入口、子模式分发、单实例 | P1 单实例与入口 |
| `snow-app-core` | 产品常量、命令总线、会话编排；已 path 依赖 `snow-draw-engine-core` 验证连通 | P1 命令总线 |
| `snow-capability` | 平台能力注册表 | P1 能力注册表 |
| `snow-ui` | GPUI 视图层总入口 | P1 起 |
| `snow-ui/snow-ui-theme` | 设计令牌 | P1 主题 |
| `snow-ui/snow-ui-icons` | 图标模型与资源 | P1 图标 |
| `snow-ui/snow-ui-widgets` | gpui-kit 缺口组件 | P3 |
| `snow-ui/snow-ui-shell` | GPUI 隔离层，`gpui::` 只允许出现在这里 | P1 起 |
| `snow-canvas-raster` / `-filters` / `-text` | 画布光栅化 / 滤镜 / 文本 | P2 |
| `snow-translate` | 本地 NMT | P5 |
| `snow-i18n` | Fluent 运行时与提取 | P1 i18n |
| `snow-config` | 配置读写 | P1 配置层 |
| `snow-history` | 历史与贴图仓储 | P3 / P4 |
| `snow-net` | HTTP 与云端 AI | P5 |
| `snow-update` | 更新（T4 验收前禁用） | P7 |
| `snow-mcp` | 进程内 MCP | P7 |
| `snow-platform` | 原生平台调用 | P1 起 |
| `tools/workspace-guard` | 守卫：shell 之外出现 `gpui::` 或 gpui 依赖即测试失败 | P1 |

日志与崩溃转储、CI 属 P1 后续任务，尚未建 crate / 流水线。

## 新增依赖与功能

- `snow-shot` 依赖 `snow-crates` 中的 `snow-ui-selector`（path，仅 Windows，Apache-2.0 许可，传递 `rstar`、`crossbeam-channel`）用于窗口识别。
- `snow-ui-shell` 的 `windows` crate 新增 feature：`Win32_Graphics_Dwm`、`Win32_System_LibraryLoader`（标题栏深浅色、弹出菜单主题接口）。
- i18n 新增 `.ftl` 文件：
  - `locales/en-US/tray.ftl` / `locales/zh-CN/tray.ftl` — 托盘菜单文本（支持中英文并随语言设置即时刷新）
  - `locales/en-US/annotation_style.ftl` / `locales/zh-CN/annotation_style.ftl` — 标注样式面板文本

## 功能现状快照

当前已落地功能：
- **截图选区**：智能窗口识别（悬停高亮顶层窗口、单击选中、拖动转手动框选；设置项默认开启），控件级识别未做。
- **标注工具**：新增样式面板（颜色、线宽、字号、填充、箭头头型，按工具记忆）与荧光笔、序号球。
- **听写（实时语音转文字）**：可用（浮窗输出；键入到其它应用的真机验证未完成）。
- **托盘菜单**：支持中英文并随语言设置即时刷新。
- **深浅主题**：设置窗口标题栏与托盘菜单跟随 Windows 深浅主题。

## 尚未完成

缺口清单（简述）：7 个全局热键动作仍为占位（直接截图类已接线，无真机验证）、贴图管理页、OCR 结果窗、二维码、快捷键录入控件。MCP、更新、网络 crate 仅占位。macOS/Linux 推迟（ADR-7）。详见 `../docs/cisox-progress-handoff.md` 的「迁移差距盘点」。

## 可选下载的翻译模型与授权声明 / Optional translation models and license notice

截图翻译的本地模型**不随安装包提供**，需要时由用户在应用内按需下载（或自行用仓库里的脚本生成）。Release 中提供的翻译模型文件由 Meta FAIR 的 NLLB-200-distilled-600M 衍生而来（词表裁剪到所选语言集、4 位权重量化），**沿用其 CC-BY-NC-4.0 许可，仅限非商业使用，不用于生产部署**。这些文件不属于本仓库 GPL-3.0 许可的范围。是否使用、如何评估由此带来的授权与质量风险，请用户自行斟酌，作者不提供任何担保。

The local translation models for screenshot translation are **not bundled** with the installer; users download them on demand (or generate them with the scripts in this repository). The model files published in releases are derived from Meta FAIR's NLLB-200-distilled-600M (vocabulary pruned to the selected languages, 4-bit weight quantization) and keep its **CC-BY-NC-4.0 license: non-commercial use only, not intended for production deployment**. They are not covered by this repository's GPL-3.0 license. Users are responsible for assessing the licensing and quality risks of using them; no warranty is provided.

- 来源 / Source: https://huggingface.co/facebook/nllb-200-distilled-600M ，论文 NLLB Team et al., "No Language Left Behind: Scaling Human-Centered Machine Translation", arXiv:2207.04672
- 许可 / License: https://creativecommons.org/licenses/by-nc/4.0/
- 修改说明 / Changes: 词表裁剪、权重量化为 int4（详见 `docs/guides/translation-model-release.md`）/ vocabulary pruning and int4 weight quantization (see `docs/guides/translation-model-release.md`). 与 Meta 无隶属关系，未获其背书 / Not affiliated with or endorsed by Meta.

## 模型下载地址 / Model download URLs

模型文件都不进仓库、不随克隆分发。下表是**精确下载地址**，应用内的按需下载用的是同一批地址。下载后按各自的目录约定放置，并用 SHA-256 校验。

Model files are never stored in the repository and are not shipped with a clone. The tables below give the **exact download URLs**; the in-app on-demand download uses the same ones. Verify each file against its SHA-256.

### 1. 我们自行转换 / 量化的翻译模型（本仓库 Release `models`）/ Translation models converted by us (this repo, release `models`)

固定地址（tag 恒为 `models`，pre-release，文件原地替换、不改名）/ Fixed location (tag is always `models`, pre-release; files are replaced in place, never renamed): https://github.com/icodejoo/Cisox/releases/tag/models

| 文件 / File | 内容 / Contents | 许可 / License | 地址 / URL |
|---|---|---|---|
| `nllb600m-main14-ccm-int4.zip` | NLLB-200-distilled-600M，裁剪词表，14 语言，int4（默认 / default） | CC BY-NC 4.0（仅非商业 / non-commercial only） | https://github.com/icodejoo/Cisox/releases/download/models/nllb600m-main14-ccm-int4.zip |
| `nllb600m-un6-ccm-int4.zip` | NLLB-200-distilled-600M，裁剪词表，联合国六语，int4 | CC BY-NC 4.0（仅非商业 / non-commercial only） | https://github.com/icodejoo/Cisox/releases/download/models/nllb600m-un6-ccm-int4.zip |
| `opusmt-en-zh-int4.zip` | OPUS-MT 英→中 / en→zh，int4 | Apache-2.0 | https://github.com/icodejoo/Cisox/releases/download/models/opusmt-en-zh-int4.zip |
| `opusmt-zh-en-int4.zip` | OPUS-MT 中→英 / zh→en，int4 | CC BY 4.0 | https://github.com/icodejoo/Cisox/releases/download/models/opusmt-zh-en-int4.zip |
| `hymt2-1.8b-int4.zip` | 腾讯 Hy-MT2-1.8B，int4（可选，不参与默认选包 / optional） | Apache-2.0 | https://github.com/icodejoo/Cisox/releases/download/models/hymt2-1.8b-int4.zip |

每个 zip 内含模型、`LICENSE-*.txt`、`NOTICE.txt` 与逐文件 SHA-256 清单（`*.manifest.json`）；zip 本身的 SHA-256 写在 Release 页面的附件信息里。 / Each zip contains the model, `LICENSE-*.txt`, `NOTICE.txt` and a per-file SHA-256 manifest (`*.manifest.json`); the zip SHA-256 is shown on the release page.

### 2. OCR 模型与运行时（上游原样，不由本仓库托管）/ OCR models and runtime (unmodified upstream, not hosted here)

来源：Snow Shot 上游在 ModelScope 的 `mgchao/SnowShotOCR`（PP-OCR 系列 ONNX；PaddleOCR 为 Apache-2.0）。落盘位置 `<数据根>/models/ocr/<模型 ID>/`，运行时在 `<数据根>/assets/ocr/runtimes/<版本>/<平台>/`。 / Source: Snow Shot upstream, ModelScope `mgchao/SnowShotOCR` (PP-OCR ONNX; PaddleOCR is Apache-2.0). Files go to `<data root>/models/ocr/<model id>/` and `<data root>/assets/ocr/runtimes/<version>/<platform>/`.

默认档 / default tier: `small`

| 模型 ID / Model ID | 文件 / File | 大小 / Size | SHA-256 | 地址 / URL |
|---|---|---|---|---|
| `ppocrv6-tiny-cd609a1` | `PP-OCRv6_det_tiny.onnx` | 1.7 MiB | `f42c0fbd294d95eac1a550e131b277dac97462c8025fa4b6c3cec1b7894bd3d5` | https://www.modelscope.cn/models/mgchao/SnowShotOCR/resolve/master/PP-OCRv6/tiny/PP-OCRv6_det_tiny.onnx |
| `ppocrv6-tiny-cd609a1` | `PP-OCRv6_rec_tiny.onnx` | 4.3 MiB | `e16e242de5937ad92609223f19bc2aff3727ee40b095f996907c24749bad251b` | https://www.modelscope.cn/models/mgchao/SnowShotOCR/resolve/master/PP-OCRv6/tiny/PP-OCRv6_rec_tiny.onnx |
| `ppocrv6-tiny-cd609a1` | `ppocrv6_tiny_dict.txt` | 0.0 MiB | `c5cbe34ef40c29c4df07ed012bf96569cb69a2d2a01a07027e9f13cb832bd9cd` | https://www.modelscope.cn/models/mgchao/SnowShotOCR/resolve/master/PP-OCRv6/tiny/ppocrv6_tiny_dict.txt |
| `ppocrv6-small-463ea9f` | `PP-OCRv6_det_small.onnx` | 9.5 MiB | `090f04abcd9d9a7498bc4ebf677e4cb9bdce1fe4197ddb7e529f1ef44e1ff94f` | https://www.modelscope.cn/models/mgchao/SnowShotOCR/resolve/master/PP-OCRv6/small/PP-OCRv6_det_small.onnx |
| `ppocrv6-small-463ea9f` | `PP-OCRv6_rec_small.onnx` | 20.3 MiB | `6f327246b50388f3c176ae304bd95767ea6dc0c9ae92153ef8cbe210b3c14884` | https://www.modelscope.cn/models/mgchao/SnowShotOCR/resolve/master/PP-OCRv6/small/PP-OCRv6_rec_small.onnx |
| `ppocrv6-small-463ea9f` | `ppocrv6_dict.txt` | 0.1 MiB | `b5f2bfe2bdd9448429e3e82b51c789775d9b42f2403d082b00662eb77e401c5d` | https://www.modelscope.cn/models/mgchao/SnowShotOCR/resolve/master/PP-OCRv6/small/ppocrv6_dict.txt |
| `ppocrv6-medium-f5063c6` | `PP-OCRv6_det_medium.onnx` | 59.2 MiB | `92078b7355007ccfffcd4c8cd441a3afd4538904d06881b29a155e1e679907c2` | https://www.modelscope.cn/models/mgchao/SnowShotOCR/resolve/master/PP-OCRv6/medium/PP-OCRv6_det_medium.onnx |
| `ppocrv6-medium-f5063c6` | `PP-OCRv6_rec_medium.onnx` | 73.1 MiB | `eef444829dbbe18d7fea59a3f6eb75647518d2b3a9568d27c92e42940204894b` | https://www.modelscope.cn/models/mgchao/SnowShotOCR/resolve/master/PP-OCRv6/medium/PP-OCRv6_rec_medium.onnx |
| `ppocrv6-medium-f5063c6` | `ppocrv6_dict.txt` | 0.1 MiB | `b5f2bfe2bdd9448429e3e82b51c789775d9b42f2403d082b00662eb77e401c5d` | https://www.modelscope.cn/models/mgchao/SnowShotOCR/resolve/master/PP-OCRv6/medium/ppocrv6_dict.txt |
| `ppocrv5-small-7b2a75a` | `ch_PP-OCRv5_det_mobile.onnx` | 4.6 MiB | `4d97c44a20d30a81aad087d6a396b08f786c4635742afc391f6621f5c6ae78ae` | https://www.modelscope.cn/models/mgchao/SnowShotOCR/resolve/master/PP-OCRv5/mobile/ch_PP-OCRv5_det_mobile.onnx |
| `ppocrv5-small-7b2a75a` | `ch_PP-OCRv5_rec_mobile.onnx` | 15.9 MiB | `5825fc7ebf84ae7a412be049820b4d86d77620f204a041697b0494669b1742c5` | https://www.modelscope.cn/models/mgchao/SnowShotOCR/resolve/master/PP-OCRv5/mobile/ch_PP-OCRv5_rec_mobile.onnx |
| `ppocrv5-small-7b2a75a` | `ppocrv5_dict.txt` | 0.1 MiB | `d1979e9f794c464c0d2e0b70a7fe14dd978e9dc644c0e71f14158cdf8342af1b` | https://www.modelscope.cn/models/mgchao/SnowShotOCR/resolve/master/PP-OCRv5/mobile/ppocrv5_dict.txt |
| `ppocrv5-medium-7b2a75a` | `ch_PP-OCRv5_det_server.onnx` | 84.0 MiB | `0f8846b1d4bba223a2a2f9d9b44022fbc22cc019051a602b41a7fda9667e4cad` | https://www.modelscope.cn/models/mgchao/SnowShotOCR/resolve/master/PP-OCRv5/server/ch_PP-OCRv5_det_server.onnx |
| `ppocrv5-medium-7b2a75a` | `ch_PP-OCRv5_rec_server.onnx` | 80.7 MiB | `e09385400eaaaef34ceff54aeb7c4f0f1fe014c27fa8b9905d4709b65746562a` | https://www.modelscope.cn/models/mgchao/SnowShotOCR/resolve/master/PP-OCRv5/server/ch_PP-OCRv5_rec_server.onnx |
| `ppocrv5-medium-7b2a75a` | `ppocrv5_dict.txt` | 0.1 MiB | `d1979e9f794c464c0d2e0b70a7fe14dd978e9dc644c0e71f14158cdf8342af1b` | https://www.modelscope.cn/models/mgchao/SnowShotOCR/resolve/master/PP-OCRv5/server/ppocrv5_dict.txt |
| `ppocrv4-small-7b2a75a` | `ch_PP-OCRv4_det_mobile.onnx` | 4.5 MiB | `d2a7720d45a54257208b1e13e36a8479894cb74155a5efe29462512d42f49da9` | https://www.modelscope.cn/models/mgchao/SnowShotOCR/resolve/master/PP-OCRv4/mobile/ch_PP-OCRv4_det_mobile.onnx |
| `ppocrv4-small-7b2a75a` | `ch_PP-OCRv4_rec_mobile.onnx` | 10.4 MiB | `48fc40f24f6d2a207a2b1091d3437eb3cc3eb6b676dc3ef9c37384005483683b` | https://www.modelscope.cn/models/mgchao/SnowShotOCR/resolve/master/PP-OCRv4/mobile/ch_PP-OCRv4_rec_mobile.onnx |
| `ppocrv4-small-7b2a75a` | `ppocr_keys_v1.txt` | 0.0 MiB | `28b2362ad4ab2dc38769aa72feb535e3a9ddb3fd2a7585a05920e6393b1dc7f7` | https://www.modelscope.cn/models/mgchao/SnowShotOCR/resolve/master/PP-OCRv4/mobile/ppocr_keys_v1.txt |
| `ppocrv4-medium-7b2a75a` | `ch_PP-OCRv4_det_server.onnx` | 108.1 MiB | `cfa39a3f298f6d3fc71789834d15da36d11a6c59b489fc16ea4733728012f786` | https://www.modelscope.cn/models/mgchao/SnowShotOCR/resolve/master/PP-OCRv4/server/ch_PP-OCRv4_det_server.onnx |
| `ppocrv4-medium-7b2a75a` | `ch_PP-OCRv4_rec_server.onnx` | 86.3 MiB | `6a2676219be9907c7fc9cf61ebaa843bf2898777def567925b78886fcd90c07a` | https://www.modelscope.cn/models/mgchao/SnowShotOCR/resolve/master/PP-OCRv4/server/ch_PP-OCRv4_rec_server.onnx |
| `ppocrv4-medium-7b2a75a` | `ppocr_keys_v1.txt` | 0.0 MiB | `28b2362ad4ab2dc38769aa72feb535e3a9ddb3fd2a7585a05920e6393b1dc7f7` | https://www.modelscope.cn/models/mgchao/SnowShotOCR/resolve/master/PP-OCRv4/server/ppocr_keys_v1.txt |

运行时 / Runtime `1.0.8` (windows-x64): `snow-ocr-runtime-1.0.8-windows-x64.zip`，16.5 MiB，SHA-256 `39ea72ab8b72a7771c78d09514738399e5e7669a0d57810c5850c5ed001cb3af`，https://www.modelscope.cn/models/mgchao/SnowShotOCR/resolve/master/runtime/1.0.8/windows-x64/snow-ocr-runtime-1.0.8-windows-x64.zip

### 3. 语音转文字模型（上游原样，不由本仓库托管）/ Speech-to-text models (unmodified upstream, not hosted here)

来源：k2-fsa/sherpa-onnx 的 `asr-models` Release。解压到 `<数据根>/models/stt/<模型 ID>/`；离线模式另需 VAD 模型。许可列为 `unverified` 的模型尚未核对再分发条款，使用前请自行确认。 / Source: k2-fsa/sherpa-onnx release `asr-models`. Extract to `<data root>/models/stt/<model id>/`; offline mode also needs the VAD model. Models marked `unverified` have not had their redistribution terms checked; verify before use.

| 模型 ID / Model ID | 维度 / 模式 / 角色 | 许可 / License | 大小 / Size | SHA-256 | 地址 / URL |
|---|---|---|---|---|---|
| `sherpa-onnx-streaming-zipformer-multi-zh-hans-2023-12-12` | zh / streaming / default | apache-2.0 | 296.0 MiB | `865308b30fba262182225f7e55ebb02a092566fe25dd6a78ec9dfeb7425dc7ba` | https://github.com/k2-fsa/sherpa-onnx/releases/download/asr-models/sherpa-onnx-streaming-zipformer-multi-zh-hans-2023-12-12.tar.bz2 |
| `sherpa-onnx-streaming-zipformer-zh-int8-2025-06-30` | zh / streaming / alternate | apache-2.0 | 126.5 MiB | `5a2832047ea1f97dd0dc595b816c230c4bafad65cfc0341fa57517cadc50afd0` | https://github.com/k2-fsa/sherpa-onnx/releases/download/asr-models/sherpa-onnx-streaming-zipformer-zh-int8-2025-06-30.tar.bz2 |
| `sherpa-onnx-streaming-zipformer-en-2023-06-26` | en / streaming / default | apache-2.0 | 296.0 MiB | `（清单未固定 / not pinned）` | https://github.com/k2-fsa/sherpa-onnx/releases/download/asr-models/sherpa-onnx-streaming-zipformer-en-2023-06-26.tar.bz2 |
| `sherpa-onnx-streaming-zipformer-en-2023-06-21` | en / streaming / alternate | apache-2.0 | 483.5 MiB | `（清单未固定 / not pinned）` | https://github.com/k2-fsa/sherpa-onnx/releases/download/asr-models/sherpa-onnx-streaming-zipformer-en-2023-06-21.tar.bz2 |
| `sherpa-onnx-x-asr-480ms-streaming-zipformer-transducer-zh-en-punct-int8-2026-06-05` | bilingual / streaming / default | unverified | 127.7 MiB | `fa5f63d618e5a01526e275a358bb7772e403f84808a4769fba52cffd8160bf74` | https://github.com/k2-fsa/sherpa-onnx/releases/download/asr-models/sherpa-onnx-x-asr-480ms-streaming-zipformer-transducer-zh-en-punct-int8-2026-06-05.tar.bz2 |
| `sherpa-onnx-x-asr-160ms-streaming-zipformer-transducer-zh-en-punct-int8-2026-06-05` | bilingual / streaming / alternate | unverified | 127.7 MiB | `8a6fca056e1a342546edd78be4d50274e2c01898e7b8ae8fc336f6410319c399` | https://github.com/k2-fsa/sherpa-onnx/releases/download/asr-models/sherpa-onnx-x-asr-160ms-streaming-zipformer-transducer-zh-en-punct-int8-2026-06-05.tar.bz2 |
| `sherpa-onnx-streaming-zipformer-bilingual-zh-en-2023-02-20` | bilingual / streaming / legacy | apache-2.0 | 487.6 MiB | `（清单未固定 / not pinned）` | https://github.com/k2-fsa/sherpa-onnx/releases/download/asr-models/sherpa-onnx-streaming-zipformer-bilingual-zh-en-2023-02-20.tar.bz2 |
| `sherpa-onnx-paraformer-zh-small-2024-03-09` | zh / offline / default | unverified | 74.3 MiB | `（清单未固定 / not pinned）` | https://github.com/k2-fsa/sherpa-onnx/releases/download/asr-models/sherpa-onnx-paraformer-zh-small-2024-03-09.tar.bz2 |
| `sherpa-onnx-paraformer-trilingual-zh-cantonese-en` | zh / offline / alternate | unverified | 1010.4 MiB | `（清单未固定 / not pinned）` | https://github.com/k2-fsa/sherpa-onnx/releases/download/asr-models/sherpa-onnx-paraformer-trilingual-zh-cantonese-en.tar.bz2 |
| `sherpa-onnx-zipformer-small-en-2023-06-26` | en / offline / default | apache-2.0 | 107.0 MiB | `（清单未固定 / not pinned）` | https://github.com/k2-fsa/sherpa-onnx/releases/download/asr-models/sherpa-onnx-zipformer-small-en-2023-06-26.tar.bz2 |
| `sherpa-onnx-zipformer-en-2023-06-26` | en / offline / alternate | apache-2.0 | 293.4 MiB | `（清单未固定 / not pinned）` | https://github.com/k2-fsa/sherpa-onnx/releases/download/asr-models/sherpa-onnx-zipformer-en-2023-06-26.tar.bz2 |
| `sherpa-onnx-x-asr-zipformer-transducer-zh-en-punct-int8-2026-06-03` | bilingual / offline / default | unverified | 130.1 MiB | `5d02c36d7b44e886b7c8f0d8e051f8713acab96c264bb6ef9e718be39a6a2224` | https://github.com/k2-fsa/sherpa-onnx/releases/download/asr-models/sherpa-onnx-x-asr-zipformer-transducer-zh-en-punct-int8-2026-06-03.tar.bz2 |
| `sherpa-onnx-sense-voice-zh-en-ja-ko-yue-int8-2024-07-17` | bilingual / offline / alternate | funasr-model-license | 155.5 MiB | `7d1efa2138a65b0b488df37f8b89e3d91a60676e416f515b952358d83dfd347e` | https://github.com/k2-fsa/sherpa-onnx/releases/download/asr-models/sherpa-onnx-sense-voice-zh-en-ja-ko-yue-int8-2024-07-17.tar.bz2 |

VAD / Silero VAD (`silero_vad.onnx`，MIT，0.6 MiB)：SHA-256 `9e2449e1087496d8d4caba907f23e0bd3f78d91fa552479bb9c23ac09cbb1fd6`，https://github.com/k2-fsa/sherpa-onnx/releases/download/asr-models/silero_vad.onnx

