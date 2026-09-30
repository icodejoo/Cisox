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

if ([string]::IsNullOrWhiteSpace($env:FFMPEG_DIR)) { $env:FFMPEG_DIR = Join-Path $repoRoot ".tools/vcpkg/installed/static/x64-windows-static" }
if (-not (Test-Path (Join-Path $env:FFMPEG_DIR "lib/avcodec.lib"))) { throw "FFMPEG_DIR 下缺少 lib/avcodec.lib: $($env:FFMPEG_DIR)" }
$env:VCPKGRS_TRIPLET = "x64-windows-static"
$env:VCPKGRS_DYNAMIC = "0"
if ([string]::IsNullOrWhiteSpace($env:LIBCLANG_PATH)) {
    $candidates = @((Join-Path $repoRoot ".tools/llvm/bin"), "C:\Program Files\LLVM\bin", "C:\tools\msys64\mingw64\bin")
    $found = $candidates | Where-Object { Test-Path (Join-Path $_ "libclang.dll") } | Select-Object -First 1
    if (-not $found) { throw "找不到 libclang.dll，请设置 LIBCLANG_PATH" }
    $env:LIBCLANG_PATH = $found
    $env:Path = "$env:Path;$found"
}
if (-not $env:INCLUDE) {
    $vswhere = Join-Path ${env:ProgramFiles(x86)} "Microsoft Visual Studio/Installer/vswhere.exe"
    $vsPath = & $vswhere -latest -products * -property installationPath
    $vcvars = Join-Path $vsPath "VC/Auxiliary/Build/vcvars64.bat"
    if (-not (Test-Path $vcvars)) { throw "找不到 vcvars64.bat: $vcvars" }
    $env:Path = "$(Split-Path $vswhere);$env:Path"
    cmd /c "`"$vcvars`" >nul 2>&1 && set" | ForEach-Object {
        if ($_ -match "^([^=]+)=(.*)$") { Set-Item -Path "Env:$($Matches[1])" -Value $Matches[2] }
    }
}
$env:RUSTFLAGS = "-C target-feature=+crt-static"
$env:CARGO_TARGET_DIR = Join-Path $repoRoot "build/recorder-$Name"
if (-not $env:CARGO_BUILD_JOBS) { $env:CARGO_BUILD_JOBS = "4" }

[string[]]$featureArgs = if ($Features) { @("--features", $Features) } else { @() }
if ($Test) { cargo test --manifest-path $manifest --release @featureArgs }
else { cargo build --manifest-path $manifest --release @featureArgs }
if ($LASTEXITCODE -ne 0) { throw "cargo 失败，退出码 $LASTEXITCODE" }
"产物: $(Join-Path $env:CARGO_TARGET_DIR 'release/snow-recorder.exe')"
