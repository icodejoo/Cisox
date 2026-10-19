# 把各 release 产物收拢成 Cisox 发布目录，并打出便携 zip 与 NSIS 安装包。
# 用法: scripts/package-cisox.ps1 -Version 0.1.0 [-OutDir artifacts/cisox-dist] [-SkipInstaller]
# 各产物目录默认取本地构建位置，CI 通过参数显式指定。所有 exe / dll 平铺在同一目录（宿主按自身目录找 UI 与 worker）。
param(
    [Parameter(Mandatory = $true)][string]$Version,
    [string]$OutDir = "artifacts/cisox-dist",
    # 宿主 + UI（主工作区 release 目录）
    [string]$MainReleaseDir = "",
    [string]$RecorderReleaseDir = "snow-shot-rs/tools/snow-recorder/target/release",
    # stt 目录需含 sherpa-onnx-c-api.dll / onnxruntime.dll 等（由构建脚本或 CI 步骤放好）
    [string]$SttReleaseDir = "build/stt/release",
    [string]$TranslatorReleaseDir = "snow-shot-rs/tools/snow-translator/target/release",
    [string]$TableReleaseDir = "snow-shot-rs/tools/snow-table/target/release",
    [string]$LatexReleaseDir = "snow-shot-rs/tools/snow-latex/target/release",
    [switch]$SkipInstaller
)

$ErrorActionPreference = "Stop"
$repoRoot = (Resolve-Path (Join-Path $PSScriptRoot "..")).Path

function Resolve-Dir([string]$Path) {
    if ([string]::IsNullOrWhiteSpace($Path)) { throw "缺少产物目录参数" }
    $full = if ([System.IO.Path]::IsPathRooted($Path)) { $Path } else { Join-Path $repoRoot $Path }
    if (-not (Test-Path -LiteralPath $full -PathType Container)) { throw "产物目录不存在: $full" }
    return (Resolve-Path -LiteralPath $full).Path
}

if ([string]::IsNullOrWhiteSpace($MainReleaseDir)) {
    $base = if ($env:CARGO_TARGET_DIR) { $env:CARGO_TARGET_DIR } else { Join-Path $repoRoot "build/cargo" }
    $MainReleaseDir = Join-Path $base "release"
}

# 每项: 目录 + 必须存在的 exe + 要一起带上的 dll（可选）
$parts = @(
    @{ Dir = $MainReleaseDir; Files = @("cisox.exe", "cisox-ui.exe") },
    @{ Dir = $RecorderReleaseDir; Files = @("cisox-recorder.exe") },
    @{ Dir = $SttReleaseDir; Files = @("cisox-stt.exe", "sherpa-onnx-c-api.dll", "onnxruntime.dll") },
    @{ Dir = $TranslatorReleaseDir; Files = @("cisox-translator.exe") },
    @{ Dir = $TableReleaseDir; Files = @("cisox-table.exe") },
    @{ Dir = $LatexReleaseDir; Files = @("cisox-latex.exe") }
)

$outRoot = if ([System.IO.Path]::IsPathRooted($OutDir)) { $OutDir } else { Join-Path $repoRoot $OutDir }
$stage = Join-Path $outRoot "stage"
if (Test-Path -LiteralPath $outRoot) { Remove-Item -LiteralPath $outRoot -Recurse -Force }
New-Item -ItemType Directory -Force $stage | Out-Null

foreach ($part in $parts) {
    $dir = Resolve-Dir $part.Dir
    foreach ($name in $part.Files) {
        $src = Join-Path $dir $name
        if (-not (Test-Path -LiteralPath $src -PathType Leaf)) { throw "缺少产物: $src" }
        Copy-Item -LiteralPath $src -Destination (Join-Path $stage $name) -Force
    }
}
# onnxruntime 的 providers 小库（有则带上，缺了不影响 CPU 推理）
$providers = Join-Path (Resolve-Dir $SttReleaseDir) "onnxruntime_providers_shared.dll"
if (Test-Path -LiteralPath $providers) { Copy-Item -LiteralPath $providers -Destination $stage -Force }

# 许可证与第三方声明
Copy-Item -LiteralPath (Join-Path $repoRoot "LICENSE.md") -Destination $stage -Force
Copy-Item -LiteralPath (Join-Path $repoRoot "licenses") -Destination (Join-Path $stage "licenses") -Recurse -Force

$zip = Join-Path $outRoot "Cisox-$Version-x64-portable.zip"
Compress-Archive -Path (Join-Path $stage "*") -DestinationPath $zip -Force
Write-Host "便携包: $zip"

if (-not $SkipInstaller) {
    $makensis = (Get-Command makensis -ErrorAction SilentlyContinue).Source
    if (-not $makensis) {
        $candidate = "${env:ProgramFiles(x86)}\NSIS\makensis.exe"
        if (Test-Path -LiteralPath $candidate) { $makensis = $candidate }
    }
    if (-not $makensis) { throw "找不到 makensis，请先安装 NSIS（CI 里用 choco install nsis）" }
    $setup = Join-Path $outRoot "Cisox-$Version-x64-setup.exe"
    & $makensis /V2 "/DVERSION=$Version" "/DSTAGE=$stage" "/DOUTFILE=$setup" (Join-Path $repoRoot "installer/cisox.nsi")
    if ($LASTEXITCODE -ne 0) { throw "makensis 失败，退出码 $LASTEXITCODE" }
    Write-Host "安装包: $setup"
}
