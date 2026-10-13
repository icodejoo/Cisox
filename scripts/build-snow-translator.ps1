# 构建翻译工具 snow-translator（独立 workspace）。
# 用法: scripts/build-snow-translator.ps1 [-Profile release|debug] [-Test] [-Clippy]
# 产物: snow-shot-rs/tools/snow-translator/target/<profile>/cisox-translator.exe
param(
    [ValidateSet("release", "debug")][string]$Profile = "release",
    [switch]$Test,
    [switch]$Clippy
)
$ErrorActionPreference = "Continue"
$repoRoot = (Resolve-Path (Join-Path $PSScriptRoot "..")).Path
$manifest = Join-Path $repoRoot "snow-shot-rs/tools/snow-translator/Cargo.toml"
. (Join-Path $PSScriptRoot "common.ps1")

# MSVC 环境（INCLUDE/LIB/link.exe）：不在开发者 shell 时自动导入
Import-SnowVsEnvironment -PrependVswhereDir

# 动态 CRT，独立 target 目录
if ([string]::IsNullOrWhiteSpace($env:CARGO_BUILD_JOBS)) {
    $env:CARGO_BUILD_JOBS = "4"
}
$env:CARGO_TARGET_DIR = Join-Path $repoRoot "snow-shot-rs/tools/snow-translator/target"

Invoke-SnowCargo $manifest $Profile $Test.IsPresent $Clippy.IsPresent
