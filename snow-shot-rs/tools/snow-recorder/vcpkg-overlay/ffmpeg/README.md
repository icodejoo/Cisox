# ffmpeg overlay（不含 x265）

本目录是 `cmake/vcpkg-overlay-ports/ffmpeg/` 的副本，给录制工作进程 `snow-recorder` 单独用。
上游目录不动，这里只改了两处：

1. `portfile.cmake`：Windows 的 `snow-shot-minimal` 编码器白名单去掉 `libx265`（macOS 分支未动）。
2. `vcpkg.json`：`snow-shot-minimal` 特性不再依赖 `x265`（`snow-macos-media` 未动）。

两处必须一起改：白名单留着 `libx265` 又 `--disable-libx265`，FFmpeg configure 会直接失败。

## 为什么

x265 静态库约 5 MB，而 H.265 在项目里已搁置、暂不支持，链进去纯属占体积。

## 怎么用

运行 `scripts/build-ffmpeg-recorder.ps1`，产物装到 `.tools/vcpkg/installed/static-nox265/x64-windows-static`；
`scripts/build-snow-recorder.ps1` 默认优先用这个目录。

## 上游更新 overlay 后如何对齐

1. 用上游最新的 `cmake/vcpkg-overlay-ports/ffmpeg/` 整体覆盖本目录（保留 `README.md`）。
2. 重新做上面两处改动：删 Windows 白名单里的 `libx265`，删 `snow-shot-minimal` 依赖里的 `x265`。
3. 重跑 `scripts/build-ffmpeg-recorder.ps1`，确认 `lib` 下没有 `x265*.lib`。

`../manifest/` 是只含 ffmpeg 的精简 vcpkg 清单，baseline 与上游 `vcpkg-configuration.json` 一致，上游换 baseline 时同步更新。
