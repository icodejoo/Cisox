# 构建语音转文字工作进程 snow-stt（独立 workspace，sherpa-onnx shared 链接，静态 CRT）。
# 用法: scripts/build-snow-stt.ps1 [-Profile release|debug] [-Test] [-Clippy] [-Jobs 2]
# 产物: <target>/<profile>/snow-stt.exe，旁边自动放 sherpa-onnx-c-api.dll 与仓库同版 onnxruntime.dll。
# 注意:
#   - sherpa-onnx-sys 的 build.rs 构建期要联网下载预编译库（GitHub Releases）；失败请如实排查网络，别绕过。
#   - 预编译包解压路径很长，target 目录必须短（默认 <repo>/build/stt，可用 $env:CARGO_TARGET_DIR 覆盖）。
#   - onnxruntime.dll 默认取 build/mt-quant/ort128 下的 1.28.0 版，可用 $env:SNOW_ORT_DIR 指向其它目录。
param(
    [ValidateSet("release", "debug")][string]$Profile = "release",
    [switch]$Test,
    [switch]$Clippy,
    [int]$Jobs = 2
)
$ErrorActionPreference = "Stop"
$repoRoot = (Resolve-Path (Join-Path $PSScriptRoot "..")).Path
$manifest = Join-Path $repoRoot "snow-shot-rs/tools/snow-stt/Cargo.toml"

# 编译整体降到 BelowNormal，别抢前台
[System.Diagnostics.Process]::GetCurrentProcess().PriorityClass = "BelowNormal"

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

# sherpa 预编译库为静态 CRT（MT），与之匹配；独立 target 目录且路径要短
$env:RUSTFLAGS = "-C target-feature=+crt-static"
if ([string]::IsNullOrWhiteSpace($env:CARGO_TARGET_DIR)) {
    $env:CARGO_TARGET_DIR = Join-Path $repoRoot "build/stt"
}
$targetDir = $env:CARGO_TARGET_DIR
$profileDir = Join-Path $targetDir $Profile

[string[]]$profileArgs = if ($Profile -eq "release") { @("--release") } else { @() }

# 把仓库同版 onnxruntime.dll 与 sherpa c-api 放到指定目录（exe 同目录，系统按“应用目录优先”加载）
function Install-Dlls([string]$dest) {
    $ortDir = $env:SNOW_ORT_DIR
    if ([string]::IsNullOrWhiteSpace($ortDir)) {
        $ortDir = Join-Path $repoRoot "build/mt-quant/ort128/onnxruntime/capi"
    }
    $ort = Join-Path $ortDir "onnxruntime.dll"
    if (-not (Test-Path $ort)) { throw "找不到仓库 onnxruntime.dll: $ort（可设 SNOW_ORT_DIR）" }
    $ver = (Get-Item $ort).VersionInfo.FileVersion
    if ($ver -notmatch "^1\.28\.") { throw "onnxruntime.dll 版本应为 1.28.x，实际 $ver" }
    $sherpa = Get-ChildItem -Path $targetDir -Recurse -Filter "sherpa-onnx-c-api.dll" -ErrorAction SilentlyContinue |
        Where-Object { $_.FullName -match "sherpa-onnx-prebuilt" } | Select-Object -First 1
    if (-not $sherpa) { throw "在 $targetDir 下找不到 sherpa-onnx-c-api.dll（构建期下载失败？）" }
    New-Item -ItemType Directory -Force $dest | Out-Null
    Copy-Item $sherpa.FullName (Join-Path $dest "sherpa-onnx-c-api.dll") -Force
    Copy-Item $ort (Join-Path $dest "onnxruntime.dll") -Force
    $providers = Join-Path $ortDir "onnxruntime_providers_shared.dll"
    if (Test-Path $providers) { Copy-Item $providers (Join-Path $dest "onnxruntime_providers_shared.dll") -Force }
    Write-Host "已放置 DLL 到 $dest（onnxruntime $ver）"
}

# 本脚本自身：检查 LF，避免 CRLF 混入
$self = [System.IO.File]::ReadAllBytes($PSCommandPath)
if ($self -contains 13) { throw "build-snow-stt.ps1 含 CRLF，请转成 LF" }

if ($Clippy) {
    cargo clippy --manifest-path $manifest -j $Jobs @profileArgs --all-targets -- -D warnings
}
elseif ($Test) {
    # 测试 exe 在 deps/ 下，先编译再把 DLL 放进 deps/ 后运行
    cargo test --manifest-path $manifest -j $Jobs @profileArgs --no-run
    if ($LASTEXITCODE -ne 0) { throw "cargo 失败，退出码 $LASTEXITCODE" }
    Install-Dlls (Join-Path $profileDir "deps")
    cargo test --manifest-path $manifest -j $Jobs @profileArgs -- --test-threads=2
}
else {
    cargo build --manifest-path $manifest -j $Jobs @profileArgs
    if ($LASTEXITCODE -ne 0) { throw "cargo 失败，退出码 $LASTEXITCODE" }
    Install-Dlls $profileDir
}
if ($LASTEXITCODE -ne 0) { throw "cargo 失败，退出码 $LASTEXITCODE" }
