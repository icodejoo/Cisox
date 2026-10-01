# 四档（1080p/1440p x 30/60fps）重复实测并汇总：对一个录制进程可执行文件，按环境变量组合跑 N 轮，打印每轮与汇总。
# 安全约束同 run-fps-test.ps1：每档由它读取目标显示器真实 bounds 并校验，主屏且未传 -AllowPrimary 立即中止；每档占屏约 10 秒（-AllowPrimary 时占用主屏）。
# 桌面不可复制（锁屏/屏保/UAC 等安全桌面）时，录制进程会报"拒绝访问"或录到黑屏：本脚本发现后立即中止并提示，不继续占屏。
# 用法:
#   scripts/run-matrix.ps1 -RecorderExe <exe> -Name hw [-Repeats 3] [-EnvPairs "SNOW_RECORDER_HARDWARE=1"] [-Tiers "1920x1080:30,1920x1080:60,2560x1440:30,2560x1440:60"]
#   跨屏: -AllowPrimary -SeamX 2560（两屏接缝的 x 坐标；每档窗口/录制区域以接缝为中心横跨两屏，占用主屏的一部分）
#   双窗口（需同时给 -SeamX）: 加 -Dual，每块屏一个夹具窗口各按自己的 vsync 出帧
#   软编对照: -EnvPairs "SNOW_RECORDER_HARDWARE=0"
# 输出: <OutDir>\<Name>-r<轮>-<WxH>-<fps>.txt（原始输出）与末尾汇总表（通过线: 30fps>=28.5、60fps>=56，且丢帧率<1%）。
[CmdletBinding(PositionalBinding = $false)]
param(
    [Parameter(Mandatory)][string]$RecorderExe,
    [Parameter(Mandatory)][string]$Name,
    [int]$Repeats = 3,
    [string]$Tiers = "1920x1080:30,1920x1080:60,2560x1440:30,2560x1440:60",
    [string]$EnvPairs = "",
    [int]$Seconds = 6,
    [switch]$AllowPrimary,
    [int]$SeamX = 0,
    [switch]$Dual,
    [string]$FixtureExe = "",
    [string]$FfmpegDir = "",
    [string]$OutDir = (Join-Path $env:TEMP "snow-fps-matrix")
)
$ErrorActionPreference = "Continue"
$toolRoot = (Resolve-Path (Join-Path $PSScriptRoot "..")).Path
$driver = Join-Path $PSScriptRoot "run-fps-test.ps1"
if ($AllowPrimary) { Write-Warning "已传 -AllowPrimary：将占用主屏约 10 秒/轮（共 $($Repeats * ($Tiers -split ',').Count) 轮），期间请勿操作屏幕" }
New-Item -ItemType Directory -Force -Path $OutDir | Out-Null
$set = @()
foreach ($kv in ($EnvPairs -split ";" | Where-Object { $_ })) {
    $k, $v = $kv -split "=", 2
    Set-Item -Path "Env:$k" -Value $v
    $set += $k
}
try {
    for ($r = 1; $r -le $Repeats; $r++) {
        foreach ($tier in ($Tiers -split ",")) {
            $size, $fps = $tier -split ":"
            $out = Join-Path $OutDir "$Name-r$r-$size-$fps.txt"
            $extra = @{}
            if ($FixtureExe) { $extra.FixtureExe = $FixtureExe }
            if ($FfmpegDir) { $extra.FfmpegDir = $FfmpegDir }
            if ($Dual) {
                if ($SeamX -le 0) { throw "-Dual 只能与 -SeamX（跨屏）同用" }
                $extra.Dual = $true
            }
            if ($SeamX -gt 0) {
                if (-not $AllowPrimary) { throw "跨屏会占用主屏的一部分，必须同时传 -AllowPrimary" }
                $tw, $th = $size.ToLower().Split("x") | ForEach-Object { [int]$_ }
                $extra.SpanRegion = "$($SeamX - [int]($tw / 2)),0,$tw,$th"
            }
            & $driver -Size $size -Fps ([int]$fps) -Seconds $Seconds -Tag "$Name-r$r-$size-$fps" -RecorderExe $RecorderExe -AllowPrimary:$AllowPrimary @extra 2>&1 | Out-File $out -Encoding utf8
            $text = Get-Content $out -Raw
            if ($text -match "拒绝访问|无可解码帧|DuplicateOutput 失败") {
                "!! 桌面当前不可复制（锁屏/屏保/安全桌面？）：$out ——中止，请解锁后重试。"
                return
            }
            "done $Name r$r $size@$fps"
        }
    }
}
finally {
    foreach ($k in $set) { Remove-Item "Env:$k" -ErrorAction SilentlyContinue }
}
python (Join-Path $toolRoot "analyze/summarize_runs.py") $OutDir $Name
