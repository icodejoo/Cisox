# 下载预编译的静态 FFmpeg（无 x265）并解压到录制 worker 约定的位置，替代本机跑 vcpkg 编译。
# 用法: scripts/fetch-ffmpeg.ps1 [-Force] [-Repo icodejoo/Cisox] [-Tag deps-ffmpeg] [-Dest <目录>]
# 产物: .tools/vcpkg/installed/static-nox265/x64-windows-static（build-snow-recorder.ps1 默认读这里）
# 来源: Release deps-ffmpeg，由 .github/workflows/cisox-ffmpeg.yml 构建；本机和 CI 打包共用本脚本。
# 目录里已有 avcodec.lib 时直接跳过，加 -Force 重新下载覆盖。
param(
    [string]$Repo = "icodejoo/Cisox",
    [string]$Tag = "deps-ffmpeg",
    [string]$Dest = "",
    [switch]$Force
)

$ErrorActionPreference = "Stop"
$repoRoot = (Resolve-Path (Join-Path $PSScriptRoot "..")).Path
$asset = "ffmpeg-static-nox265-x64-windows-static.zip"
$dest = if ($Dest) { $Dest } else { Join-Path $repoRoot ".tools/vcpkg/installed/static-nox265/x64-windows-static" }
$marker = Join-Path $dest "lib/avcodec.lib"

if ((Test-Path -LiteralPath $marker) -and -not $Force) {
    Write-Host "FFmpeg 已就绪，跳过下载: $dest（要重下请加 -Force）"
    return
}

$work = Join-Path ([System.IO.Path]::GetTempPath()) "cisox-ffmpeg-$([guid]::NewGuid().ToString('N'))"
New-Item -ItemType Directory -Force $work | Out-Null
try {
    $zip = Join-Path $work $asset
    $sha = "$zip.sha256"
    if (Get-Command gh -ErrorAction SilentlyContinue) {
        gh release download $Tag -R $Repo --pattern $asset --pattern "$asset.sha256" -D $work
        if ($LASTEXITCODE -ne 0) { throw "gh 下载失败（私有仓库请先 gh auth login）；也可浏览器下载后放到 $work 再重跑" }
    }
    else {
        $base = "https://github.com/$Repo/releases/download/$Tag"
        Invoke-WebRequest "$base/$asset" -OutFile $zip
        Invoke-WebRequest "$base/$asset.sha256" -OutFile $sha
    }

    $want = ((Get-Content -LiteralPath $sha -Raw).Trim() -split '\s+')[0].ToLower()
    $got = (Get-FileHash -LiteralPath $zip -Algorithm SHA256).Hash.ToLower()
    if ($want -ne $got) { throw "FFmpeg 校验失败: 期望 $want, 实际 $got" }

    if (Test-Path -LiteralPath $dest) { Remove-Item -LiteralPath $dest -Recurse -Force }
    New-Item -ItemType Directory -Force $dest | Out-Null
    Expand-Archive -LiteralPath $zip -DestinationPath $dest -Force
    if (-not (Test-Path -LiteralPath $marker)) { throw "解压后缺少 lib/avcodec.lib: $dest" }
    Write-Host "FFmpeg 已解压到: $dest"
}
finally {
    Remove-Item -LiteralPath $work -Recurse -Force -ErrorAction SilentlyContinue
}
