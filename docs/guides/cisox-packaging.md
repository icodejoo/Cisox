# Cisox 打包与 CI 构建

Rust 版的安装包由 GitHub Actions 出：一个 NSIS 安装包（当前用户安装，不要管理员权限）加一个便携 zip。旧的 `scripts/package-snow-shot.ps1` 等是 Qt 版流水线，和这里无关。

## 三个工作流

| 工作流 | 干什么 | 什么时候跑 |
|---|---|---|
| `cisox-package.yml` | 编宿主、UI、五个 worker，打包、静默安装冒烟、上传产物 | 手动触发；推 `vX.Y.Z_cisox` 标签（同时建草稿 Release）；`jelon/**` 分支上改了打包相关文件时自动跑 |
| `cisox-ffmpeg.yml` | 单独编静态 FFmpeg（无 x265），发布到 Release `deps-ffmpeg` | 手动触发；改了 vcpkg overlay / 三元组 / `build-ffmpeg-recorder.ps1` 时自动跑 |
| `snow-shot-rs-ci.yml` | fmt / clippy / 测试 / 导入表闸门 | 原有，不动 |

打包产物放在 `artifacts/cisox-dist/`：`Cisox-<版本>-x64-setup.exe` 和 `Cisox-<版本>-x64-portable.zip`。所有 exe / dll 平铺在同一目录（宿主按自身目录找 UI 与 worker）：`cisox.exe`、`cisox-ui.exe`、`cisox-recorder.exe`、`cisox-stt.exe`、`cisox-translator.exe`、`cisox-table.exe`、`cisox-latex.exe`，外加 `onnxruntime.dll`、`onnxruntime_providers_shared.dll`、`sherpa-onnx-c-api.dll`、`LICENSE.md`、`licenses/`。

版本号的取值顺序：手动输入、标签、`snow-shot-rs/Cargo.toml` 的 workspace 版本，必须是 `X.Y.Z`。

## FFmpeg：下载，不再自己编

FFmpeg 基本不会再变，所以不在打包流水线里编。`cisox-ffmpeg.yml` 编好后打成 `ffmpeg-static-nox265-x64-windows-static.zip`（约 42MB，里面是 `include/`、`lib/`、`share/`），连同 `.sha256` 一起放到 Release `deps-ffmpeg`。

本机或 CI 取用都走同一个脚本：

```powershell
scripts/fetch-ffmpeg.ps1            # 已有 lib/avcodec.lib 就跳过
scripts/fetch-ffmpeg.ps1 -Force     # 强制重新下载覆盖
```

它会下载、校验 sha256、解压到 `.tools/vcpkg/installed/static-nox265/x64-windows-static`，这正是 `scripts/build-snow-recorder.ps1` 默认读的位置。私有仓库先 `gh auth login`；没有 `gh` 时退回直接 HTTP 下载。

什么时候要重跑 `cisox-ffmpeg.yml`：改了 FFmpeg 的 vcpkg overlay、三元组或构建脚本之后。打包流水线不会检测这个包是否过期，需要人记得。

体积的两个口径别混：zip 里是链接前的静态库（`avcodec.lib` 单个约 45MB），不进安装包；链接进 `cisox-recorder.exe` 后，FFmpeg 相关代码只占约 7.3MB（avcodec 2.3、x264 1.8、swscale 1.0、avformat 0.7、avutil 0.6、webp 0.3、vpl 0.2、swresample 0.1、zlib 0.1，用链接器 `/MAP` 估算，各行有零点几 MB 误差）。

## onnxruntime

CI 里随包的 `onnxruntime.dll` 取自 sherpa 预编译包（构建 `cisox-stt` 时联网下载），流水线会检查版本不低于 1.28（table / translator / latex 的 `ort` 用了 `api-28`）。

本机开发用的是 1.28.0.20260724 那份，已作为官方 wheel 里的 DLL 放到 Release `models`：`onnxruntime-1.28.0-win-x64.zip`（含 `onnxruntime.dll` 和 `onnxruntime_providers_shared.dll`，sha256 `e9190adb66055e9ef56424d7f1a2c4409238e5b40ae6a7f40658335b6b82a76a`）。CI 暂时还没改成用它，要改的话在 `cisox-package.yml` 里把 stt 步骤的 DLL 来源换成这个包，并保留版本检查。

## 不在包里的东西

- OCR worker 不进包：它按 `ocr-asset-manifest.json` 在运行时下载。
- 翻译、语音等模型不进包：按需从 Release `models` 或上游下载。
- 第三方声明只带 `LICENSE.md` 和 `licenses/`；`collect-third-party-licenses.ps1` 是 Qt 版的，没接。
- 没做代码签名。

## 已验证与未验证

已验证（2026-10-19，GitHub Actions）：全部 worker、宿主和 UI 能编过；打包出安装包与便携 zip；静默安装到临时目录后 10 个关键文件齐全；`fetch-ffmpeg.ps1` 的下载、校验、解压、跳过都正常。

未验证：安装后 `cisox.exe` 真机能否启动；录制、语音、翻译、表格、公式在安装目录里能否工作（共用同一份 onnxruntime）；覆盖安装和卸载流程（`--cmd quit` 退出宿主那一步）；NSIS 脚本只在 CI 上编过，本机没有 NSIS。

另外 `snow-shot-rs-ci.yml` 目前有三处红灯，与打包无关：Windows 上 `snow-platform` 的 `pipe_dacl_only_grants_current_user` 在 runner 账户下失败；Linux、macOS 在 `cargo check --workspace` 就挂了（缺系统库，这两个平台本来就推迟）。
