---
title: 翻译模型发布与授权方案
status: active
updated: 2026-10-02
summary: 本地翻译模型的最终选型、release 发布方式（A 直接发布现成文件 + B 提供自行生成脚本）、CC-BY-NC 授权声明与应用内提示要求、产物规格与待办
---
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
- [ ] Rust worker 适配：`m2m_100` 家族（NLLB）、裁剪词表的 id 映射与语言码、MatMulNBits/GatherBlockQuantized 在 ORT 1.28.0 下的真实加载验证（目前只在 Python 绑定里验证过）。
- [ ] 打包脚本：生成模型包、LICENSE/NOTICE、manifest（含 SHA256）；发布到 GitHub Release 与镜像。
- [ ] 应用内下载与授权提示界面（沿用 OCR 资产下载机制）。
- [ ] 把评测脚本整理成面向用户的 B 方案脚本。
- [ ] 补充评测：14 语言版里韩、葡、意、土、越、印尼 6 种语言尚未评测；每语向仅 30 句，需要更大样本。
