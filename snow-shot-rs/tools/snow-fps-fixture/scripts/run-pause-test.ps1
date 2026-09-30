# 暂停扣除与 EOF 取消回归：在副屏上跑夹具，录制 START -> 暂停 -> 恢复 -> STOP，核对成品有效时长。
# 安全约束同 run-fps-test.ps1：先 --check 读取目标显示器真实 bounds，主屏且未传 -AllowPrimary 立即中止；
# 整轮占屏约 8 秒；结束后确认夹具/录制进程为 0。
# 用法: scripts/run-pause-test.ps1 -RecorderExe <exe> [-Mode pause|eof] [-Size 1920x1080] [-Fps 30] [-AllowPrimary]
[CmdletBinding(PositionalBinding = $false)]
param(
    [Parameter(Mandatory)][string]$RecorderExe,
    [ValidateSet("pause", "eof")][string]$Mode = "pause",
    [string]$Size = "1920x1080",
    [ValidateSet(30, 60)][int]$Fps = 30,
    [double]$RunSeconds = 2.0,
    [double]$PauseSeconds = 2.0,
    [switch]$AllowPrimary,
    [string]$OutDir = (Join-Path $env:TEMP "snow-fps-test"),
    [string]$FfmpegDir = "C:\ProgramData\chocolatey\bin"
)
$ErrorActionPreference = "Stop"
$toolRoot = (Resolve-Path (Join-Path $PSScriptRoot "..")).Path
$FixtureExe = Join-Path $toolRoot "target/release/snow-fps-fixture.exe"
New-Item -ItemType Directory -Force -Path $OutDir | Out-Null
$base = Join-Path $OutDir ("{0}-{1}" -f "pause", (Get-Date -Format "HHmmss"))
$outFile = "$base.mp4"
$readyFile = "$base.ready"

# 目标显示器校验：不通过则一个窗口都不创建（默认拒绝主屏；单屏机器须显式 -AllowPrimary）
$checkArgs = @("--check"); if ($AllowPrimary) { $checkArgs += "--allow-primary" }
$check = & $FixtureExe @checkArgs
if ($LASTEXITCODE -ne 0) { throw "夹具校验目标显示器失败，中止: $check" }
$mon = [regex]::Match($check, "primary=(true|false) monitor=Rect \{ x: (-?\d+), y: (-?\d+), w: (\d+), h: (\d+) \}")
if (-not $mon.Success) { throw "无法解析显示器矩形: $check" }
$isPrimary = $mon.Groups[1].Value -eq "true"
$mx, $my, $mw, $mh = 2..5 | ForEach-Object { [int]$mon.Groups[$_].Value }
# 二次防线：未传 -AllowPrimary 时，主屏（Primary=true 或坐标 (0,0)）立刻中止；期望值取自夹具实测 bounds，不写死分辨率
if (-not $AllowPrimary -and ($isPrimary -or ($mx -eq 0 -and $my -eq 0))) { throw "目标显示器是主屏 ($mx,$my,${mw}x${mh})，未传 -AllowPrimary，中止" }
if ($AllowPrimary) { Write-Warning "已传 -AllowPrimary：将占用主屏 (${mw}x${mh}) 约 10 秒/轮，期间请勿操作屏幕" }
$w, $h = $Size.ToLower().Split("x") | ForEach-Object { [int]$_ }
if ($w -le 0 -or $h -le 0 -or $w -gt $mw -or $h -gt $mh) { throw "尺寸 $Size 超出目标显示器 (${mw}x${mh})，中止" }
$region = "$mx $my $w $h"
$divisor = if ($Fps -ge 60) { 1 } else { 2 }
$fixSeconds = [Math]::Min(11.0, 2.0 * $RunSeconds + $PauseSeconds + 3.5)
$fixture = $null; $rec = $null
try {
    $fixArgs = @("--size", $Size, "--seconds", "$fixSeconds", "--divisor", "$divisor", "--load", "noise", "--ready", $readyFile)
    if ($AllowPrimary) { $fixArgs += "--allow-primary" }
    $fixture = Start-Process -FilePath $FixtureExe -ArgumentList $fixArgs -PassThru -WindowStyle Hidden
    $t0 = Get-Date
    while (-not (Test-Path $readyFile)) {
        if (((Get-Date) - $t0).TotalSeconds -gt 5) { throw "夹具 5 秒内未就绪" }
        Start-Sleep -Milliseconds 50
    }
    $psi = New-Object System.Diagnostics.ProcessStartInfo
    $psi.FileName = $RecorderExe
    $psi.RedirectStandardInput = $true; $psi.RedirectStandardOutput = $true; $psi.RedirectStandardError = $true
    $psi.UseShellExecute = $false; $psi.CreateNoWindow = $true
    $rec = New-Object System.Diagnostics.Process
    $rec.StartInfo = $psi
    [void]$rec.Start()
    $outTask = $rec.StandardOutput.ReadToEndAsync()
    $errTask = $rec.StandardError.ReadToEndAsync()
    $stdin = $rec.StandardInput
    $stdin.AutoFlush = $true
    Start-Sleep -Milliseconds 400
    $stdin.WriteLine("START $region mp4 $Fps 1 $outFile")
    Start-Sleep -Milliseconds 1200   # 启动（含编码器探测）耗时，不计入有效时长
    Start-Sleep -Milliseconds ([int]($RunSeconds * 1000))
    if ($Mode -eq "pause") {
        $stdin.WriteLine("PAUSE")
        Start-Sleep -Milliseconds ([int]($PauseSeconds * 1000))
        $stdin.WriteLine("RESUME")
        Start-Sleep -Milliseconds ([int]($RunSeconds * 1000))
        $stdin.WriteLine("STOP")
    } else {
        $stdin.Close()   # EOF：视为取消，清理半截文件
    }
    if (-not $rec.WaitForExit(30000)) { throw "录制进程 30 秒未退出" }
    "--- recorder events ---"
    ($outTask.Result.Trim() -split "`n" | Select-Object -Last 8) -join "`n"
    "--- recorder stderr (回落/GPU 相关行 + 尾部) ---"
    $errLines = $errTask.Result.Trim() -split "`n"
    ($errLines | Where-Object { $_ -match "回落|GPU 流水线|GPU 采集" }) -join "`n"
    ($errLines | Select-Object -Last 6) -join "`n"
    "exit code: $($rec.ExitCode)"
}
finally {
    foreach ($p in @($rec, $fixture)) { if ($p -and -not $p.HasExited) { try { $p.Kill() } catch { } } }
    Start-Sleep -Milliseconds 300
    $left = @(Get-Process -Name snow-fps-fixture, snow-recorder -ErrorAction SilentlyContinue)
    "leftover processes: $($left.Count)"
}
$scratch = @(Get-ChildItem -Path $OutDir -Directory -Filter ".snow-recording-*" -Force -ErrorAction SilentlyContinue)
"scratch dirs left in out dir: $($scratch.Count)"
if ($Mode -eq "pause") {
    if (-not (Test-Path $outFile)) { throw "成品不存在: $outFile" }
    $ffprobe = Join-Path $FfmpegDir "ffprobe.exe"
    "--- ffprobe ---"
    & $ffprobe -v error -select_streams v:0 -show_entries "stream=codec_name,width,height,nb_frames:format=duration,size" -of default=nw=1 $outFile
    $expected = 2.0 * $RunSeconds
    "期望有效时长约 ${expected}s（暂停 ${PauseSeconds}s 不计入）"
    "--- 分析（暂停处允许序号断档）---"
    python (Join-Path $toolRoot "analyze/fps_analyze.py") $outFile --refresh $(if ($Fps -ge 60) { 59 } else { 30 }) --ffmpeg-dir $FfmpegDir
} else {
    "成品应不存在（取消）: exists=$(Test-Path $outFile)"
}
