# 待实现：WebM 录制格式

状态：**待实现**（阶段 C 已从配置、界面与 `recording/model.rs` 中移除 WebM 选项）。

## 原因

- snow-crates 的导出能力不支持 WebM：`ExportFormat` 只有 `Mp4 / Avi / Gif / Apng / Webp`，`VideoCodec` 只有 `H264 / H265`。
- 本机（仓库 vcpkg）的 FFmpeg 没有编入 libvpx / libaom，也没有 `webm` muxer（只有 `matroska`）。
  编码器清单见 `cmake/vcpkg-overlay-ports/ffmpeg/portfile.cmake`。

## 当前行为

- 支持的录制格式：MP4、GIF、APNG、动画 WebP。
- 配置项 `screen_recording/output_format` 的允许值不含 `webm`。
- 磁盘上已有 `"output_format": "webm"` 的旧配置：读取时被 schema 规范化回默认值 `mp4`，不会报错或崩溃
  （见 `snow-config` 的规范化逻辑，以及 `recording::model` 里的回归测试 `disk_config_with_webm_is_recovered`）。
- 录制进程协议（`snow-recorder-protocol`）的格式解析对 `webm` 同样归一化为 MP4。

## 待办

功能验收之后回头做，需要先做的事：

1. 给 vcpkg 的 ffmpeg 端口加上 `libvpx`（VP8/VP9）与 `webm` muxer，重新构建静态库。
2. 在 snow-crates 的 `snow-recording-export` 里新增 `ExportFormat::Webm`、对应的编码器选择与音频（Opus）路径。
3. 恢复配置枚举、界面选项与 `RecordingFormat::WebM`，并保留“旧值归一化”测试。
