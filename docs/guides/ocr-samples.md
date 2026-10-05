---
title: OCR 真实样片与同图对比用法
status: active
updated: 2026-10-05
summary: 本地真实样片（materials/ocr）的位置、格式、核对稿的来源与局限、如何跑 system 与 local-model 的同图对比、当前结果
---

> **2026-10-05 资源已清理**：本文提到的样片目录 `materials/`（含 `materials/ocr`）和 `build/ocr-*` 已删除，样片也不在本机，下文的路径与结果是当时的记录。复跑需要自行准备样片，脚本与流程仍然有效。
## TL;DR
- 样片在 `materials/ocr/`（**不入库**：含真实票据、发票、健康宝页面，已有部分打码）；我读图写的核对稿在 `materials/ocr/truth/`。
- 一条命令复跑：`snow-shot-rs/tools/snow-ocr-compare/scripts/run-materials.ps1`（需先 `cargo build --release`，产物在 `build/cargo/release/`）。
- 7 张真实样片，系统 OCR 平均 CER **57.0%**，本地 PP-OCR **17.8%**；**新用户默认值保持 `local-model`**。

## 样片与格式
`materials/ocr/*-result.jpg`（共 7 张：奥巴马证件、英文书封、繁体《心经》、外卖小票、火车票、健康宝截图、竖排商品图）都是 OCR 演示程序输出的**左右拼图**：**左半是原图加彩色检测框，右半是别的引擎把识别结果渲染在白底上的输出**。
- 评测只取**左半**（脚本用 ffmpeg 自动裁剪），右半**不是答案**（里面有错字，如 `般著波羅蜜多心經`）。
- 彩色高亮会改变文字底色，对两个引擎都有影响，这是样片本身的局限。
- 此前给过的 `input.png`/`output.txt`：`output.txt` 也只是某个引擎的输出（含 `K851971`、`Nanfe ng` 等错误），**不是标准答案**，用它算 CER 会冤枉识别正确的引擎。

## 核对稿（truth）的来源与局限
- 没有人工标注答案，`materials/ocr/truth/<名>.txt` 是**我对着左半原图逐行转写的结果**，**需要你抽查**；身份证号、订单号、条码文字等长数字串最可能有误。
- 只写了清晰可读的文字：竖排商品图里的小字（净含量、英文小字说明）没写，所以引擎多读出的小字会被算成错误；`aobama` 的身份证号按图读成 17 位 `32622196108040096`（本地模型读出同一串）。
- 以后有了人工标注，直接放进 `materials/ocr/truth/<名>.txt` 覆盖即可。

## 怎么跑
```
cd snow-shot-rs/tools/snow-ocr-compare && cargo build --release
scripts/run-materials.ps1                       # 默认 materials/ocr，输出 build/ocr-materials
scripts/run-materials.ps1 -Backends local       # 只跑本地模型
```
输出：`texts/<图名>.<后端>.txt`（每个引擎识别出的原文，逐字核对用）和 `compare.csv`。脚本支持的目录布局见脚本顶部注释（含 `input.png`+`output.txt`、`input/`+`output/`、同名配对、`*-result.jpg`+`truth/` 四种）。
- 评测口径：编辑距离 CER，**对阅读顺序敏感**，空白忽略，**全角 ASCII 折成半角、`￥` 折成 `¥`**（宽度变体不算错）。竖排、多栏版面里顺序不同会被算成大量错误，看 `texts/` 里的原文再下结论。
- 工具输出里的"合成样片"提示语对真实样片也会打印，请忽略。

## 结果（Intel UHD 770，2026-10-01，7 张真实样片）
| 样片 | system CER | local-model CER | 说明 |
|---|---|---|---|
| aobama（证件） | 56.5% | 14.5% | local 漏 `姓名/性别/出生` 淡色标签，出生日期读成 `196184` |
| en_book1（书封） | 80.2% | 21.5% | 阅读顺序与核对稿不同；`O'Doherty` 撇号 |
| fanti（繁体） | 20.8% | 0.7% | |
| fapiao（小票） | 53.9% | 4.8% | |
| huochepiao（火车票） | 60.0% | 4.4% | 参考稿 `output.txt` 本身有错，此处用核对稿 |
| jiankangbao（健康宝） | 64.2% | 5.7% | |
| shupai（竖排商品图） | 63.2% | 72.6% | local 主要文字基本都对，但顺序不同且多读出小字，**该数字高估了错误** |
| **平均 / micro** | **57.0% / 55.7%** | **17.8% / 12.7%** | |
| 热启动均耗时 | 75.6ms | 726.3ms | 样片较大；local 含 worker 通信 |
| 内存峰值 | 87.2MiB（进程内） | 503.5MiB（worker 进程） | 口径不同，不能直接相减 |

**结论**：按总原则 system 在性能与内存上占优，但识别准确度在真实图片上明显更差（每张都落后），不能用速度换正确性；**新用户默认值保持 `local-model`，system 仅作可选项**。合成样片的早期结果见 `docs/research/system-ocr-translate-backends.md`。

## 隐私
样片含真实票据/发票/健康宝页面，**不要提交进仓库**（仓库公开）。`materials/` 目前未被 `.gitignore` 忽略，注意提交时按路径添加，不要 `git add -A`。
