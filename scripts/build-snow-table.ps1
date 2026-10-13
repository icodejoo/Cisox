# 构建表格结构识别工作进程 snow-table（独立 workspace，ort load-dynamic，运行期才找 onnxruntime.dll）。
# 用法: scripts/build-snow-table.ps1 [-Profile release|debug] [-Test] [-Clippy]
# 产物: snow-shot-rs/tools/snow-table/target/<profile>/cisox-table.exe；发布时需放在主程序同目录。
param(
    [ValidateSet("release", "debug")][string]$Profile = "release",
    [switch]$Test,
    [switch]$Clippy
)
$ErrorActionPreference = "Continue"
$repoRoot = (Resolve-Path (Join-Path $PSScriptRoot "..")).Path
$manifest = Join-Path $repoRoot "snow-shot-rs/tools/snow-table/Cargo.toml"
. (Join-Path $PSScriptRoot "common.ps1")

# MSVC 环境（INCLUDE/LIB/link.exe）：不在开发者 shell 时自动导入
Import-SnowVsEnvironment

# 独立 target 目录；可用 $env:CARGO_TARGET_DIR 覆盖
if ([string]::IsNullOrWhiteSpace($env:CARGO_BUILD_JOBS)) {
    $env:CARGO_BUILD_JOBS = "4"
}
if ([string]::IsNullOrWhiteSpace($env:CARGO_TARGET_DIR)) {
    $env:CARGO_TARGET_DIR = Join-Path $repoRoot "snow-shot-rs/tools/snow-table/target"
}

Invoke-SnowCargo $manifest $Profile $Test.IsPresent $Clippy.IsPresent
