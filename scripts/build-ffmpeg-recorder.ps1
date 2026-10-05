# 为录制工作进程 snow-recorder 构建不含 x265 的静态 FFmpeg（H.265 已搁置，x265 约 5 MB）。
# 用法: scripts/build-ffmpeg-recorder.ps1 [-Reset]
# 产物: .tools/vcpkg/installed/static-nox265/x64-windows-static（不动旧的 installed/static）
# 说明: 用仓库内 .tools/vcpkg，三元组 x64-windows-static，ffmpeg 端口取
#       snow-shot-rs/tools/snow-recorder/vcpkg-overlay/ffmpeg，其余 overlay 沿用 cmake/vcpkg-overlay-ports。
param(
    [switch]$Reset
)
$ErrorActionPreference = "Stop"
$repoRoot = (Resolve-Path (Join-Path $PSScriptRoot "..")).Path
. (Join-Path $PSScriptRoot "snow-build-environment.ps1")

$vcpkgRoot = Join-Path $repoRoot ".tools/vcpkg"
$vcpkgExe = Join-Path $vcpkgRoot "vcpkg.exe"
if (-not (Test-Path -LiteralPath $vcpkgExe)) {
    throw "找不到 vcpkg.exe，请先运行 scripts/bootstrap.ps1: $vcpkgExe"
}
$env:VCPKG_ROOT = $vcpkgRoot

$overlayRoot = Join-Path $repoRoot "snow-shot-rs/tools/snow-recorder/vcpkg-overlay"
$manifestRoot = Join-Path $overlayRoot "manifest"
$installRoot = Join-Path $vcpkgRoot "installed/static-nox265"
$triplet = "x64-windows-static"

if ($Reset -and (Test-Path -LiteralPath $installRoot)) {
    Remove-Item -LiteralPath $installRoot -Recurse -Force
}

# vcpkg 的 app-local 打包要用 dumpbin，先把 MSVC 工具放进 PATH
Add-SnowMsvcToolsToPath | Out-Null

# overlay：上游里除 ffmpeg 以外的端口 + 我们自己的 ffmpeg 副本
$overlayArgs = @(Get-ChildItem -LiteralPath (Join-Path $repoRoot "cmake/vcpkg-overlay-ports") -Directory |
    Where-Object { $_.Name -ne "ffmpeg" -and (Test-Path -LiteralPath (Join-Path $_.FullName "portfile.cmake") -PathType Leaf) } |
    Sort-Object Name |
    ForEach-Object { "--overlay-ports=$($_.FullName)" })
$overlayArgs += "--overlay-ports=$(Join-Path $overlayRoot 'ffmpeg')"

$vcpkgArgs = @(
    "install",
    "--x-manifest-root=$manifestRoot",
    "--x-install-root=$installRoot",
    "--triplet=$triplet"
) + $overlayArgs + @(
    "--overlay-triplets=$(Join-Path $repoRoot 'cmake/vcpkg-overlay-triplets')",
    "--clean-after-build"
)
& $vcpkgExe @vcpkgArgs
if ($LASTEXITCODE -ne 0) { throw "vcpkg install 失败，退出码 $LASTEXITCODE" }

$libDir = Join-Path $installRoot "$triplet/lib"
if (-not (Test-Path (Join-Path $libDir "avcodec.lib"))) { throw "安装后缺少 avcodec.lib: $libDir" }
if (Get-ChildItem -LiteralPath $libDir -Filter "x265*.lib" -ErrorAction SilentlyContinue) {
    throw "安装结果里仍有 x265 库: $libDir"
}
Write-Host "FFmpeg（无 x265）已就绪: $(Join-Path $installRoot $triplet)"
