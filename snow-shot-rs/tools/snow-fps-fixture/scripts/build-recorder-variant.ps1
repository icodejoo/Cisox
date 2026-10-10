# 以独立 target 目录构建 snow-recorder 的变体（例如带 diag 特性的诊断版），不覆盖默认发布产物。
# 环境准备与 scripts/build-snow-recorder.ps1 一致（静态 FFmpeg、libclang、VS 环境、+crt-static）。
# 用法: scripts/build-recorder-variant.ps1 -Name diag -Features diag
# 产物: <repo>/build/recorder-<Name>/release/snow-recorder.exe
param(
    [Parameter(Mandatory)][string]$Name,
    [string]$Features = "",
    [switch]$Test
)
$ErrorActionPreference = "Continue"
$toolRoot = (Resolve-Path (Join-Path $PSScriptRoot "..")).Path
$repoRoot = (Resolve-Path (Join-Path $toolRoot "../../..")).Path
$manifest = Join-Path $repoRoot "snow-shot-rs/tools/snow-recorder/Cargo.toml"
. (Join-Path $repoRoot "scripts/common.ps1")

if ([string]::IsNullOrWhiteSpace($env:FFMPEG_DIR)) { $env:FFMPEG_DIR = Join-Path $repoRoot ".tools/vcpkg/installed/static/x64-windows-static" }
if (-not (Test-Path (Join-Path $env:FFMPEG_DIR "lib/avcodec.lib"))) { throw "FFMPEG_DIR 下缺少 lib/avcodec.lib: $($env:FFMPEG_DIR)" }
$env:VCPKGRS_TRIPLET = "x64-windows-static"
$env:VCPKGRS_DYNAMIC = "0"
Set-SnowLibclangPath $repoRoot
Import-SnowVsEnvironment -PrependVswhereDir
$env:RUSTFLAGS = "-C target-feature=+crt-static"
$env:CARGO_TARGET_DIR = Join-Path $repoRoot "build/recorder-$Name"
if (-not $env:CARGO_BUILD_JOBS) { $env:CARGO_BUILD_JOBS = "4" }

[string[]]$featureArgs = if ($Features) { @("--features", $Features) } else { @() }
if ($Test) { cargo test --manifest-path $manifest --release @featureArgs }
else { cargo build --manifest-path $manifest --release @featureArgs }
if ($LASTEXITCODE -ne 0) { throw "cargo 失败，退出码 $LASTEXITCODE" }
"产物: $(Join-Path $env:CARGO_TARGET_DIR 'release/snow-recorder.exe')"
