# 构建录制工作进程 snow-recorder（独立 workspace，静态 CRT + 静态 FFmpeg，默认无 x265 版）。
# 用法: scripts/build-snow-recorder.ps1 [-Profile release|debug] [-Test] [-Clippy]
# 产物: snow-shot-rs/tools/snow-recorder/target/<profile>/snow-recorder.exe
param(
    [ValidateSet("release", "debug")][string]$Profile = "release",
    [switch]$Test,
    [switch]$Clippy
)
$ErrorActionPreference = "Continue"
$repoRoot = (Resolve-Path (Join-Path $PSScriptRoot "..")).Path
$manifest = Join-Path $repoRoot "snow-shot-rs/tools/snow-recorder/Cargo.toml"
. (Join-Path $PSScriptRoot "common.ps1")

# FFmpeg：默认用不含 x265 的静态版（scripts/build-ffmpeg-recorder.ps1 生成，省约 5 MB；H.265 已搁置）。
# 该目录不存在时回退到旧的完整版（installed/static，含 x265）。FFMPEG_DIR 环境变量优先。
if ([string]::IsNullOrWhiteSpace($env:FFMPEG_DIR)) {
    $noX265 = Join-Path $repoRoot ".tools/vcpkg/installed/static-nox265/x64-windows-static"
    if (Test-Path (Join-Path $noX265 "lib/avcodec.lib")) {
        $env:FFMPEG_DIR = $noX265
    }
    else {
        Write-Host "未找到无 x265 的 FFmpeg，回退旧路径（可先运行 scripts/build-ffmpeg-recorder.ps1）"
        $env:FFMPEG_DIR = Join-Path $repoRoot ".tools/vcpkg/installed/static/x64-windows-static"
    }
}
if (-not (Test-Path (Join-Path $env:FFMPEG_DIR "lib/avcodec.lib"))) {
    throw "FFMPEG_DIR 下缺少 lib/avcodec.lib: $($env:FFMPEG_DIR)"
}
$env:VCPKGRS_TRIPLET = "x64-windows-static"
$env:VCPKGRS_DYNAMIC = "0"

# libclang（bindgen 需要）：环境变量优先，其次常见安装位置
Set-SnowLibclangPath $repoRoot

# MSVC 环境（INCLUDE/LIB/link.exe）：不在开发者 shell 时自动导入
Import-SnowVsEnvironment -PrependVswhereDir

# 静态 CRT 与静态 FFmpeg 匹配；独立 target 目录避免与其它 workspace 混用
# 根目录 .cargo/config.toml 为主工作区指定了 rust-lld，但静态 FFmpeg 库用 /GL 编译，只能由 MSVC link.exe 链接
$env:CARGO_TARGET_X86_64_PC_WINDOWS_MSVC_LINKER = "link.exe"
$env:RUSTFLAGS = "-C target-feature=+crt-static"
$env:CARGO_TARGET_DIR = Join-Path $repoRoot "snow-shot-rs/tools/snow-recorder/target"

Invoke-SnowCargo $manifest $Profile $Test.IsPresent $Clippy.IsPresent
