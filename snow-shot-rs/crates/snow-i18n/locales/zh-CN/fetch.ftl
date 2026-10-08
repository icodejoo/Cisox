## 下载、解压与安装的错误和步骤文案（OCR / onnxruntime / 语音模型）。

fetch-run-tool = 无法运行 { $tool }: { $detail }

fetch-wait-tool = 等待 { $tool } 失败: { $detail }

fetch-hash-unreadable = 无法解析 { $path } 的哈希。

fetch-stat-failed = 无法读取 { $name }: { $detail }

fetch-size-mismatch = { $name } 大小不符: 期望 { $expected }，实际 { $actual }。

fetch-hash-mismatch = { $name } 哈希不符: 期望 { $expected }，实际 { $actual }。

fetch-download-failed = 下载失败 ({ $url }): { $detail }

fetch-cancelled = 已取消。

fetch-create-dir-failed = 创建目录失败: { $detail }

fetch-rename-failed = 改名失败: { $detail }

fetch-extract-failed = 解压失败: { $detail }

fetch-marker-failed = 写完成标记失败: { $detail }

fetch-invalid-target = 目标路径无效。

fetch-meta-serialize-failed = 序列化模型元数据失败: { $detail }

fetch-meta-write-failed = 写 model.json 失败: { $detail }

fetch-staging-failed = 创建暂存目录失败: { $detail }

fetch-missing-files = 压缩包里缺少必需文件: { $files }

fetch-swap-failed = 换入模型目录失败: { $detail }

fetch-install-member-failed = 安装 { $name } 失败: { $detail }

fetch-manifest-corrupt = 内置的 { $what } 清单已损坏: { $detail }

fetch-step-ocr-runtime-download = 正在下载 OCR 运行时…

fetch-step-ocr-runtime-extract = 正在解压 OCR 运行时…

fetch-step-ocr-model = 正在下载 OCR 模型 ({ $index }/{ $total })…

fetch-step-ort-download = 正在下载 onnxruntime 运行时…

fetch-step-ort-extract = 正在解压 onnxruntime 运行时…

ocr-unavailable-no-runtime = 未安装 OCR 运行时，请先下载（约 17 MB）。

ocr-unavailable-no-model = 未安装 OCR 模型 { $id }，请先下载。

ocr-unavailable-unknown-model = 未知的 OCR 模型类型: { $kind }

ocr-err-spawn-failed = 无法启动 OCR 进程: { $detail }

ocr-err-ready-timeout = OCR 进程启动超时。

ocr-err-version-mismatch = OCR 运行时版本不匹配（需要协议 { $expected }，实际 { $actual }），请重新下载运行时。

ocr-err-died = OCR 进程意外退出。

ocr-err-died-detail = OCR 进程意外退出: { $detail }

ocr-err-session-not-ready = OCR 模型加载失败（模型文件损坏或缺少 onnxruntime），请重新下载模型。

ocr-err-failed = 识别失败: { $detail }

ocr-err-cancelled = 识别已取消。

ocr-err-timeout = OCR 超时（{ $step }）

ocr-err-protocol = OCR 通信错误: { $detail }

ocr-err-io = OCR 临时文件错误: { $detail }

ocr-err-invalid-image = 无法识别该图像: { $detail }

ocr-step-load-model = 加载模型

ocr-step-shutdown = 关闭

ocr-step-attach-image = 挂接图像

ocr-step-transfer-image = 传输图像

ocr-step-recognize = 识别

ocr-step-detach = 解除映射

ocr-panel-running = 正在识别文字…

ocr-panel-empty = 未识别到文字

ocr-panel-done-copied = 已识别 { $count } 行，文字已复制到剪贴板

ocr-panel-done-copy-failed = 已识别 { $count } 行（复制到剪贴板失败）

ocr-panel-press-d = { $message } · 按 D 下载

ocr-panel-downloading-hint = 下载完成后请再次点击“OCR”。

ocr-panel-esc = Esc 返回

ocr-panel-more-lines = …（另有 { $count } 行）

ocr-panel-footer-copied = 已复制 · E 打开结果窗 · Enter 复制并关闭 · Esc 返回

ocr-panel-footer-copy-failed = 复制失败 · E 打开结果窗 · Enter 重试并关闭 · Esc 返回

ocr-panel-download-hint = 按 D 下载 OCR 组件（运行时约 17 MB + 模型约 31 MB）

fetch-task-start-failed = 无法启动后台任务: { $detail }

ocr-panel-done-not-copied = 已识别 { $count } 行，按 Enter 复制文本

ocr-panel-footer-not-copied = 未复制 · E 打开结果窗 · Enter 复制并关闭 · Esc 返回
