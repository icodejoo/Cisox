## 语音转文字（听写）：状态文案、输出去向、错误提示与右下角浮窗。

dictation-status-loading = 正在启动语音引擎并加载模型，首次可能要等一会儿

dictation-status-listening = 正在听。{ $route }

dictation-status-finishing = 正在收尾……

dictation-status-done = 已结束

dictation-route-typing = 正在键入到当前输入框

dictation-route-typing-overlay = 正在键入到当前输入框，同时显示在这里

dictation-route-overlay = 文字显示在这里

dictation-route-fallback = 当前无法键入（{ $reason }），文字显示在这里

dictation-route-blocked = 输出方式设为只键入，但当前无法键入（{ $reason }），文字改为显示在这里

dictation-route-target-lost = 目标窗口变了，已停止键入，文字保留在这里

dictation-route-send-failed = 键入失败（{ $detail }），文字保留在这里

dictation-route-typing-stuck = 文字没能及时键入（可能修饰键还按着），已保留在这里

dictation-reason-no-focus = 没有输入焦点

dictation-reason-not-editable = 焦点控件不可编辑

dictation-reason-elevated = 目标窗口权限更高

dictation-reason-uncertain = 无法确定目标是否接受键入

dictation-error-not-implemented = 系统语音引擎暂未提供，请在设置里把语音引擎改成本地模型

dictation-error-worker-missing = 找不到语音引擎程序。请把它放在主程序旁边，或用环境变量 SNOW_STT_EXE 指定路径

dictation-error-spawn = 无法启动语音引擎：{ $detail }

dictation-error-model-dir = 语音模型目录不存在：{ $path }，请在设置里指定目录

dictation-error-start-timeout = 语音引擎没能及时就绪，已停止

dictation-error-stop-timeout = 语音引擎没能及时结束，已强制停止

dictation-error-crashed = 语音引擎意外退出（退出码 { $code }）

dictation-error-worker = 语音引擎出错：{ $detail }

dictation-error-link = 无法与语音引擎通信：{ $detail }

dictation-overlay-placeholder = 识别出的文字会出现在这里，可以直接修改

dictation-overlay-copy = 复制

dictation-overlay-copied = 已复制

dictation-overlay-copy-failed = 复制失败：{ $reason }

dictation-overlay-close = 关闭
