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

dictation-error-system-online-off = 系统语音需要开启「联机语音识别」。请打开 Windows 设置 → 隐私 → 语音（可在开始菜单运行 ms-settings:privacy-speech），打开「联机语音识别」后重试；也可以在本程序设置里改用本地模型

dictation-error-system-mic-denied = 系统语音无法使用麦克风。请打开 Windows 设置 → 隐私 → 麦克风（ms-settings:privacy-microphone），允许桌面应用访问麦克风，并确认有可用的默认录音设备。{ $detail }

dictation-error-system-language = 系统缺少所需语言的语音识别支持。请在 Windows 设置 → 时间和语言 → 语音（ms-settings:speech）里添加该语言的语音包，或在本程序设置里把语言改成系统已有的语言。{ $detail }

dictation-error-system-network = 联机语音识别连接失败，请检查网络后重试；也可以在本程序设置里改用本地模型

dictation-error-system-other = 系统语音识别出错：{ $detail }。请检查 Windows 设置 → 隐私 → 语音（ms-settings:privacy-speech）里的「联机语音识别」是否已开启

dictation-notice-system = 系统语音使用 Windows 自带识别，只能用默认麦克风，并需要开启「联机语音识别」（Windows 设置 → 隐私 → 语音，ms-settings:privacy-speech）

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

dictation-error-model-not-installed = 语音模型 { $model } 还没下载。请到设置里的“语音输入”下载后再用。

dictation-error-manual-dir-streaming = 手动指定的模型文件夹只能配合流式识别使用。请清空模型文件夹以使用内置离线模型，或改回流式。

dictation-error-model-unavailable = 这个组合下没有可用的语音模型：{ $detail }

stt-note-low-latency = 延迟最低，准确率略低于默认

stt-note-multilingual = 另支持粤语与英文，下载体积大

stt-note-high-memory = 最准，但占内存多（运行时约 670 MiB）

stt-note-itn = 自动加标点并规整数字，另支持日语、韩语、粤语

stt-ui-label-recommended = { $name }（推荐）

stt-ui-label-alternate = { $name }

stt-ui-label-legacy = { $name }（旧版）

stt-ui-state-installed = 已安装

stt-ui-state-missing = 未安装

stt-ui-no-models = 这个语言与模式下没有可选模型

stt-ui-row-installed = 已安装，占用 { $size }

stt-ui-row-missing = 未安装，需下载约 { $archive }

stt-ui-panel-title = 当前模型：{ $name }

stt-ui-line-installed = 已安装。磁盘占用 { $size }，运行时内存约 { $mem } MiB（评测口径，仅供参考）。

stt-ui-line-missing = 未安装。需下载约 { $archive }，磁盘占用 { $size }，运行时内存约 { $mem } MiB（评测口径，仅供参考）。

stt-ui-line-license = 许可证：{ $license }

stt-ui-license-unverified = 尚未核对

stt-ui-line-vad-ok = 离线模式另用共享的语音活动检测文件：已安装

stt-ui-line-vad-missing = 离线模式另需共享的语音活动检测文件（{ $size }），会随模型一起下载

stt-ui-line-unpinned = 校验值待固定：下载文件目前只校验大小

stt-ui-action-download = 下载

stt-ui-action-cancel = 取消

stt-ui-action-installed = 已安装

stt-ui-action-busy = 正在下载其他模型

stt-ui-stage-downloading = 下载中

stt-ui-stage-verifying = 校验中

stt-ui-stage-extracting = 解压中

stt-ui-progress = { $stage } { $asset } { $percent }%

stt-ui-progress-no-total = { $stage } { $asset }

stt-ui-download-failed = 下载失败：{ $detail }

stt-ui-download-cancelled = 已取消下载

stt-ui-download-done = 下载完成

stt-ui-lock-system = 系统语音引擎不使用这些模型

stt-ui-lock-manual-dir = 已指定手动模型目录，此项不可选

stt-ui-manual-dir-offline = 手动模型目录只能配合流式识别使用

dictation-translate-no-model = 翻译已开启，但没有翻译模型。请先在翻译设置里下载。

dictation-translate-unsupported = 已开启翻译，但已装的翻译模型都不支持 { $src } 到 { $tgt }。请先到翻译设置里下载支持的模型。

dictation-translate-same-language = 已开启翻译，但源语言和目标语言相同，所以不翻译。

dictation-translate-failed = （翻译失败）

dictation-lang-zh-hans = 中文

dictation-lang-en = 英文

stt-ui-translate-preview = 将翻译：{ $pairs }

stt-ui-translate-pair = { $src }译为{ $tgt }
