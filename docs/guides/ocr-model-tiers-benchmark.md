---
title: 本地 OCR 七档模型同图实测
status: active
updated: 2026-10-05
summary: 清单内 7 档本地 OCR 模型在 7 张真实样片上的 CER、耗时、worker 内存实测，含 DirectML 对比、examples 街景图观察、样片局限与复现命令
---

> **2026-10-05 资源已清理**：本文提到的样片目录 `materials/`（含 `materials/ocr`）和 `build/ocr-*` 已删除，样片也不在本机，下文的路径与结果是当时的记录。复跑需要自行准备样片，脚本与流程仍然有效。
## TL;DR
- 按 [principles.md](../principles.md) 的「高性能 > 低内存 > 识别率」：**medium 及以上三档（medium / medium_v5 / medium_v4）CPU 单张 11 到 90 秒、内存 1.2 到 2 GiB 以上，性能与内存两项都不可接受**；这是数据，不是推测。
- 剩下四档里，**small（默认）、small_v5、small_v4 的耗时几乎相同（中位约 0.68s）、内存 0.39 到 0.50 GiB**；`extra_small` 快约 2.7 倍、内存 0.35 GiB，但 CER 明显差（平均 22.2%）。
- 识别率在三个 small 档之间互有胜负：**small_v5 平均 CER 最低（14.5%），内存也比 small 少约 12 MiB**；small 的优势只在 en_book1（21.5% 对 30.6%）和 aobama（14.5% 对 17.4%）。样片只有 7 张且核对稿是模型转写，**差距在样片噪声范围内，不足以单凭本表断言换默认**；是否换档交主会话裁决。
- DirectML（Intel UHD 770）对 small **更慢且内存多近 1 GiB**；对 medium / medium_v5 快 4 到 5 倍，但内存升到 2.3 / 3.7 GiB。准确率与 CPU 完全一致。
- 7 档模型下载全部成功、sha256 与清单一致。

## 测试环境与口径
- 机器：Intel Core i5-13500，32 GB 内存，核显 UHD 770，Windows 10 Pro 19045。工具 `build/cargo/release/snow-ocr-compare.exe`（release），后端 `local`（snow-ocr-process worker 子进程）。
- 样片：`build/ocr-materials/stage` 的 7 张真实样片（口径与局限见 [ocr-samples.md](ocr-samples.md)）。**核对稿是模型转写，非人工标注**；CER 对阅读顺序敏感，空白忽略，全角折半角。
- 每档在同一台机器上跑两遍（r1、r2），表中用 r2；两遍 CER 完全一致，热启动均耗时差异在 10% 以内（small_v4 的 r1 642ms / r2 793ms 为个别样片抖动，故表里改用逐张中位数）。
- 内存是 **worker 进程 working set**（after / peak），不含主进程；模型加载后的常驻值，不是推理前。
- 测量时后台：启动前 `Get-Counter` 总 CPU 4.5 到 7.3%（5 次采样），无编译、无 aria2；机器上同时开着若干 claude 会话进程（空闲）。下载与基准没有同时进行。
- 「热启动均耗时」取工具输出的 `avg_ms_warm`，「首张」为 `first_ms`（含 worker 启动与模型加载）。**img 表里 en_book1 最快是因为它图最小**，耗时与图的大小强相关，各档之间对比同一张图才有意义。

## 总表（CPU，7 张）
| 模型键 | 模型 | 体积 | 平均 CER | micro CER | 去掉 shupai 平均 / micro | 首张 | 热启动均耗时 | 逐张中位 | 内存 after / peak |
|---|---|---|---|---|---|---|---|---|---|
| extra_small | v6 tiny | 6.1 MiB | 22.17% | 15.98% | 18.17% / 12.95% | 253ms | 208ms | 253ms | 341 / 354 MiB |
| **small（默认）** | v6 small | 29.8 MiB | 17.75% | 12.70% | 8.60% / 6.69% | 432ms | 662ms | 687ms | 492 / 503 MiB |
| medium | v6 medium | 132.4 MiB | 17.66% | 11.85% | 5.36% / 3.86% | 4.9s | 12.8s | 10.96s | 1217 / 1229 MiB |
| small_v5 | v5 mobile | 20.5 MiB | 14.52% | 9.90% | 10.10% / 6.78% | 499ms | 649ms | 677ms | 476 / 491 MiB |
| medium_v5 | v5 server | 164.8 MiB | 14.35% | 10.29% | 7.05% / 5.49% | 8.7s | 22.5s | 17.2s | 2024 / 2036 MiB |
| small_v4 | v4 mobile | 14.9 MiB | 15.74% | 11.07% | 11.53% / 8.06% | 439ms | 793ms | 675ms | 395 / 409 MiB |
| medium_v4 | v4 server | 194.5 MiB | 见下 | 见下 | — | 26.6s | 56s（仅 3 张成功） | 34.3s | 1618 / 1789 MiB |

**medium_v4 失败**：CPU 下 fapiao（文字最多的小票）报「OCR 超时（识别）」，随后 worker 退出，huochepiao、jiankangbao、shupai 级联失败（`OCR buffer already attached`，是超时后的连锁，不是各自独立问题）。成功的 3 张耗时 26.6s / 34.3s / 78.0s；这 3 张的 CER 平均 16.05%（同样 3 张上：extra_small 28.3%、small 12.2%、medium 6.9%、small_v5 16.0%、medium_v5 10.6%、small_v4 16.6%）。r1、r2 两遍结果一致，所以不是偶发。

体积为清单里 det + rec + 字典合计。

## 逐张 CER（%，CPU）
| 样片 | extra_small | small | medium | small_v5 | medium_v5 | small_v4 | medium_v4 |
|---|---|---|---|---|---|---|---|
| aobama | 26.1 | 14.5 | 5.8 | 17.4 | **1.5** | 23.2 | 13.0 |
| en_book1 | 50.4 | 21.5 | **14.9** | 30.6 | 28.9 | 14.1 | 19.8 |
| fanti | 8.3 | 0.7 | **0.0** | **0.0** | 1.4 | 12.5 | 15.3 |
| fapiao | 5.6 | 4.8 | **1.3** | 2.9 | 2.7 | 3.1 | 超时 |
| huochepiao | 4.4 | 4.4 | 4.4 | 5.2 | 4.4 | 4.4 | 失败 |
| jiankangbao | 14.2 | 5.7 | 5.7 | 4.5 | **3.4** | 11.9 | 失败 |
| shupai（竖排，**被高估**） | 46.2 | 72.7 | 91.5 | 41.0 | 58.1 | 41.0 | 失败 |

**shupai 须单独看**：该图是竖排商品图，CER 对阅读顺序敏感，且核对稿没写小字，模型读出小字就被算成错误。文字读得越全（medium 91.5%）反而分越差，所以该列不能用来排名，上表「去掉 shupai」列更接近真实文字识别差异。small 与上一份文档的 17.8% / 12.7% 一致，数据可复现。

## 逐张耗时（ms，CPU，r2）
| 样片 | extra_small | small | medium | small_v5 | medium_v5 | small_v4 | medium_v4 |
|---|---|---|---|---|---|---|---|
| aobama | 253 | 432 | 4946 | 499 | 8738 | 439 | 26602 |
| en_book1 | 46 | 214 | 4939 | 200 | 9430 | 168 | 34292 |
| fanti | 249 | 772 | 14221 | 689 | 24364 | 668 | 77970 |
| fapiao | 266 | 880 | 20706 | 1055 | 41093 | 1015 | 超时 |
| huochepiao | 260 | 687 | 10511 | 677 | 16366 | 675 | — |
| jiankangbao | 294 | 943 | 15579 | 842 | 26490 | 1436 | — |
| shupai | 135 | 478 | 10956 | 429 | 17163 | 793 | — |

## DirectML（`--directml`，Intel UHD 770）
DirectML 可用，三档都成功运行，**CER 与 CPU 逐张完全一致**。r1 的首次运行含着色器/算子编译（small 首张 21.5s、medium 32.9s、medium_v5 23.8s），之后稳定，下表取 r2。

| 模型键 | CPU 热启动均耗时 | DirectML（r2）均耗时 | 倍率 | CPU 内存 after | DirectML 内存 after / peak |
|---|---|---|---|---|---|
| small | 662ms | 891ms | 慢 1.35 倍 | 492 MiB | 1437 / 1493 MiB |
| medium | 12.8s | 2.80s | 快 4.6 倍 | 1217 MiB | 2280 / 2338 MiB |
| medium_v5 | 22.5s | 3.58s | 快 6.3 倍 | 2024 MiB | 3686 / 3748 MiB |

- small 上 DirectML 比 CPU 慢（小模型在 GPU 上调度开销占主导）且内存多约 0.9 GiB，**不适合作为 small 的加速手段**。
- medium 档借 DirectML 能把单张耗时降到 1 到 5 秒，但常驻内存 2.3 GiB 起，仍违背「低内存」；首次运行还要付一次 20 到 30 秒的编译。
- 只测了核显，没测独显；没有对 extra_small / small_v5 / small_v4 / medium_v4 跑 DirectML。

## examples.png 观察（街景文字拼图，无参考答案）
识别文本在 `materials/ocr/results/<模型键>/examples.txt`（不入库）。图中可见主要文字：MR.Z 串串、Taberia（车身）、万达广场、4008517588、Scent、永和大王、YONGHEKING、员工通道凭证进入、禁止停放自行车摩托车、出租车停靠点 TAXI STOP、乘坐索道由此去（含英文标语）、红包页（凤爪美味爽口零负担、OhGirl的红包、积分换成功、2.33元、已存入零钱可直接消费、莫言 22:03、查看我的红包记录）、你已进入无烟医院、禁止吸烟 NO SMOKING、华科大42分店 创于1986年。逐档主要漏识与错识（人工对照，只列显眼项）：

| 档 | 单张耗时 | 主要问题 |
|---|---|---|
| extra_small | 303ms | 4008517588 读成乱码 `040088n580`；缺万达广场、出租车停靠点、莫言、22:03、串串；`永和大`缺王；零钱句错成 `可直胺羽费`；英文标语乱码 |
| small | 647ms | 万达广场只出 `万达广`；缺出租车停靠点、积分换成功、22:03；`乘坐素道`错一字；英文标语乱码；4008517588、永和大王、莫言正确 |
| medium | 10.8s | 最全：出租车停靠点、22:03、莫言、乘坐索道全对；仍有 `万达广通`、`积分关换成功率` 错字，英文标语未识别 |
| small_v5 | 680ms | 缺万达广场、积分、莫言、22:03；`出租车停警点`、`已存入零线可宜胺羽责`、`查看教的红包记录`、`由比去` 错字；`永和大`缺王 |
| medium_v5 | 21.3s | 莫言、22:03 对；`达广场`缺首字、`积分换成功电`、`出租车停靠店`、`已存人零钱` 错字；多出噪声 `国白，不` |
| small_v4 | 663ms | `莫言`错成`其言`；缺万达广场、积分、22:03；`出租车`缺首字、`NGHEKING` 缺 YO、`零线`、`查看妆` 错字 |
| medium_v4 | 90.2s | 22:03、永和大王对；缺莫言、`万达广`缺场、`出租车停点`缺靠、`已存入零钱可直消费`缺接；多出 `白不`、`hy thywys` 噪声、2.33元重复 |

看法（仅限这一张图）：v6 系列的中文标牌识别整体最好（small 就能读出 4008517588、永和大王、莫言，v5/v4 的 small 反而在零钱句、查看我的红包记录上出现多处错字）；medium 在 small 基础上多读出的是低对比度小字（22:03、积分换成功）和出租车停靠点，换来 16 倍耗时、2.5 倍内存。街景英文标语（Taking the cableway, this way please）所有档都没读对。

## 样片局限
- 只有 7 张，每张权重大：fanti 的 0.7% 与 0.0% 差一个字，不能当作档位差别。
- 核对稿是模型转写，aobama 身份证号等长数字可能有误；对 medium / medium_v5 读出的更多字可能被误判成错。
- 样片是「原图加彩色检测框」拼图的左半，彩色高亮影响所有档。
- 测的是整图一次识别的墙钟耗时，与图大小强相关；没有测多并发、没有测连续长时间运行的内存增长。
- 没有测系统 OCR 本身（见 [ocr-samples.md](ocr-samples.md)）。

## 复现命令
模型已按清单下载到 `%LOCALAPPDATA%\Cisox\assets\ocr\models\<模型 ID>\`，每个目录写入 `.complete.json`（内容 `{"schema":1}`）；`ocr_assets.rs::dir_complete` 判据为标记文件存在且每个文件大小与清单一致。下载用 aria2（`-x16 -s16 -k8M`），modelscope.cn 单源，本机实测每文件 0.9 到 21 MiB/s（大文件 6 到 21 MiB/s，6 档新模型合计不到 2 分钟下完），sha256 全部与清单一致。

```
# CPU，7 张样片
build/cargo/release/snow-ocr-compare.exe run --dir build/ocr-materials/stage --backends local --model <模型键> --dump <输出目录> --csv <csv>
# DirectML：追加 --directml（首次运行含编译，取第二遍）
# examples.png
build/cargo/release/snow-ocr-compare.exe run --dir build/ocr-examples/stage --backends local --model <模型键> --dump <输出目录> --csv <csv>
```
`build/ocr-materials/stage` 由 `snow-shot-rs/tools/snow-ocr-compare/scripts/run-materials.ps1` 生成（见 [ocr-samples.md](ocr-samples.md)）。模型键：`extra_small`、`small`、`medium`、`small_v5`、`medium_v5`、`small_v4`、`medium_v4`。原始 csv 与日志在 `build/bench/`（生成物，不入库）。
