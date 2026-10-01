# 对 materials/ocr 下的真实样片跑 system 与 local-model 的同图对比，并导出每个引擎识别出的文本。
# 支持三种布局（可混用）：
#   1) materials/ocr/input.png + output.txt            -> 用例名 input
#   2) materials/ocr/input/<名>.png + output/<名>.txt  -> 用例名 <名>
#   3) materials/ocr/<名>.png + <名>.txt               -> 用例名 <名>
#   4) materials/ocr/<名>[.扩展名]-result.jpg + truth/<名>.txt
#        -> OCR 演示程序的"左原图+右识别结果"拼图：只取左半原图，答案取 truth/<名>.txt（右半是别的引擎的输出，不当答案）
# 用法示例:
#   scripts/run-materials.ps1
#   scripts/run-materials.ps1 -Materials D:\samples -Out D:\out
# 注意：output.txt 如果只是某个 OCR 引擎的输出而非人工标注，CER 会被参照物拖偏，见 docs/guides/ocr-samples.md。
param(
    [string]$Materials = "",
    [string]$Out = "",
    [string]$Exe = "",
    [string]$Backends = "system,local",
    [string]$FfmpegDir = "C:/ProgramData/chocolatey/bin"
)
$ErrorActionPreference = "Stop"
$toolRoot = (Resolve-Path (Join-Path $PSScriptRoot "..")).Path
$repoRoot = (Resolve-Path (Join-Path $toolRoot "../../..")).Path
if (-not $Materials) { $Materials = Join-Path $repoRoot "materials/ocr" }
if (-not $Out) { $Out = Join-Path $repoRoot "build/ocr-materials" }
if (-not $Exe) { $Exe = Join-Path $repoRoot "build/cargo/release/snow-ocr-compare.exe" }
if (-not (Test-Path $Materials)) { throw "找不到样片目录: $Materials" }
if (-not (Test-Path $Exe)) { throw "找不到对比工具，请先在 $toolRoot 下执行 cargo build --release: $Exe" }

$stage = Join-Path $Out "stage"
if (Test-Path $stage) { Remove-Item $stage -Recurse -Force }
New-Item -ItemType Directory -Force -Path $stage | Out-Null
$count = 0
function Add-Case([string]$name, [string]$png, [string]$txt) {
    Copy-Item $png (Join-Path $stage "$name.png") -Force
    Copy-Item $txt (Join-Path $stage "$name.txt") -Force
    $script:count++
}
# 布局 1：input.png + output.txt
$p1 = Join-Path $Materials "input.png"; $t1 = Join-Path $Materials "output.txt"
if ((Test-Path $p1) -and (Test-Path $t1)) { Add-Case "input" $p1 $t1 }
# 布局 2：input/ 与 output/ 子目录按同名配对
$inDir = Join-Path $Materials "input"; $outDir = Join-Path $Materials "output"
if ((Test-Path $inDir) -and (Test-Path $outDir)) {
    foreach ($png in Get-ChildItem $inDir -Filter *.png) {
        $txt = Join-Path $outDir ($png.BaseName + ".txt")
        if (Test-Path $txt) { Add-Case $png.BaseName $png.FullName $txt } else { Write-Warning "缺少答案文件: $txt" }
    }
}
# 布局 3：同目录同名配对
foreach ($png in Get-ChildItem $Materials -Filter *.png -File) {
    if ($png.BaseName -eq "input") { continue }
    $txt = Join-Path $Materials ($png.BaseName + ".txt")
    if (Test-Path $txt) { Add-Case $png.BaseName $png.FullName $txt }
}
# 布局 4：*-result.jpg 拼图（左半原图）+ truth/<名>.txt
$truthDir = Join-Path $Materials "truth"
if (Test-Path $truthDir) {
    $ffmpeg = Join-Path $FfmpegDir "ffmpeg.exe"
    foreach ($img in Get-ChildItem $Materials -Filter *-result.jpg -File) {
        $name = [IO.Path]::GetFileNameWithoutExtension(($img.Name -replace '-result\.jpg$', ''))
        $txt = Join-Path $truthDir "$name.txt"
        if (-not (Test-Path $txt)) { Write-Warning "缺少核对稿: $txt"; continue }
        if (-not (Test-Path $ffmpeg)) { throw "裁剪拼图需要 ffmpeg: $ffmpeg（可用 -FfmpegDir 指定）" }
        $png = Join-Path $stage "$name.png"
        & $ffmpeg -v error -y -i $img.FullName -vf "crop=floor(iw/2):ih:0:0" $png
        if ($LASTEXITCODE -ne 0) { throw "ffmpeg 裁剪失败: $($img.Name)" }
        Copy-Item $txt (Join-Path $stage "$name.txt") -Force
        $count++
    }
}
if ($count -eq 0) { throw "没有找到任何 图片+答案 配对: $Materials" }
Write-Host "已配对 $count 个样片，工作目录: $stage"
New-Item -ItemType Directory -Force -Path $Out | Out-Null
& $Exe run --dir $stage --csv (Join-Path $Out "compare.csv") --dump (Join-Path $Out "texts") --backends $Backends
Write-Host "识别文本: $(Join-Path $Out 'texts')  CSV: $(Join-Path $Out 'compare.csv')"
