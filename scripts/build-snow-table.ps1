# 构建表格结构识别工作进程 snow-table（独立 workspace，ort load-dynamic，运行期才找 onnxruntime.dll）。
# 用法: scripts/build-snow-table.ps1 [-Profile release|debug] [-Test] [-Clippy]
# 产物: snow-shot-rs/tools/snow-table/target/<profile>/snow-table.exe；发布时需放在主程序同目录。
param(
    [ValidateSet("release", "debug")][string]$Profile = "release",
    [switch]$Test,
    [switch]$Clippy
)
$ErrorActionPreference = "Continue"
$repoRoot = (Resolve-Path (Join-Path $PSScriptRoot "..")).Path
$manifest = Join-Path $repoRoot "snow-shot-rs/tools/snow-table/Cargo.toml"

# MSVC 环境（INCLUDE/LIB/link.exe）：不在开发者 shell 时自动导入
if (-not $env:INCLUDE) {
    $vswhere = Join-Path ${env:ProgramFiles(x86)} "Microsoft Visual Studio/Installer/vswhere.exe"
    $vsPath = & $vswhere -latest -products * -property installationPath
    $vcvars = Join-Path $vsPath "VC/Auxiliary/Build/vcvars64.bat"
    if (-not (Test-Path $vcvars)) { throw "找不到 vcvars64.bat: $vcvars" }
    cmd /c "`"$vcvars`" >nul 2>&1 && set" | ForEach-Object {
        if ($_ -match "^([^=]+)=(.*)$") { Set-Item -Path "Env:$($Matches[1])" -Value $Matches[2] }
    }
}

# 独立 target 目录；可用 $env:CARGO_TARGET_DIR 覆盖
if ([string]::IsNullOrWhiteSpace($env:CARGO_BUILD_JOBS)) {
    $env:CARGO_BUILD_JOBS = "4"
}
if ([string]::IsNullOrWhiteSpace($env:CARGO_TARGET_DIR)) {
    $env:CARGO_TARGET_DIR = Join-Path $repoRoot "snow-shot-rs/tools/snow-table/target"
}

[string[]]$profileArgs = if ($Profile -eq "release") { @("--release") } else { @() }
if ($Clippy) {
    cargo clippy --manifest-path $manifest @profileArgs --all-targets -- -D warnings
}
elseif ($Test) {
    cargo test --manifest-path $manifest @profileArgs
}
else {
    cargo build --manifest-path $manifest @profileArgs
}
if ($LASTEXITCODE -ne 0) { throw "cargo 失败，退出码 $LASTEXITCODE" }
