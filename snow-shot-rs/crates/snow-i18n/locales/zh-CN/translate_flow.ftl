## 覆盖窗里的截图翻译面板：进度、结果与失败说明。

translate-flow-stage-recognizing = 正在识别文字…

translate-flow-stage-translating = 正在翻译…（首次会加载模型，稍等片刻）

translate-flow-ocr-hint = { $message } 请先点击“OCR”按钮并按 D 下载 OCR 组件，再回来翻译。

translate-flow-no-text = 未识别到可翻译的文字。

translate-flow-already-target = 原文已经是{ $lang }，无需翻译（可在设置里更改目标语言）。

translate-flow-done-copied = 已翻译，译文已复制到剪贴板

translate-flow-done-copy-failed = 已翻译（复制到剪贴板失败）

translate-flow-empty = 译文为空

translate-flow-press-d = { $message } · 按 D 下载

translate-flow-downloading-hint = 下载完成后请再次点击“翻译”。

translate-flow-title = 译文 · { $label }

translate-flow-more-paragraphs = …（另有 { $count } 段）

translate-flow-footer-copied = 已复制 · Enter 复制并关闭 · Esc 返回

translate-flow-footer-copy-failed = 复制失败 · Enter 重试并关闭 · Esc 返回

translate-flow-download-hint = 按 D 下载 onnxruntime 运行时（约 14 MB，官方发布并校验哈希）

translate-flow-esc = Esc 返回

translate-flow-error-no-model = 未找到可用的翻译模型。请把模型文件夹放进翻译模型目录。详情: { $detail }

translate-flow-error-pair = 不支持的翻译语言对: { $source } 到 { $target }

translate-flow-error-invalid-request = 翻译请求不合法: { $detail }

translate-flow-error-network = 翻译端点连接失败: { $detail }

translate-flow-error-io = 模型文件读取错误: { $detail }

translate-flow-error-timeout = 翻译执行超时。

translate-flow-error-runtime-missing = 缺少 onnxruntime 运行时（{ $detail }）。

translate-flow-error-worker-unavailable = 翻译组件不可用: { $detail }

translate-flow-error-worker-died = 翻译进程异常退出: { $detail }

translate-flow-error-model-load = 翻译模型加载失败: { $detail }

translate-flow-error-inference = 翻译失败: { $detail }

translate-flow-error-oom = 翻译内存不足: { $detail }

translate-flow-error-no-custom-model = 尚未配置自定义 AI 模型，请先在“自定义模型”里添加一个 OpenAI 兼容的端点。

translate-flow-error-custom-model-not-selected = 请在设置里为“文字翻译”选择一个自定义 AI 模型。
