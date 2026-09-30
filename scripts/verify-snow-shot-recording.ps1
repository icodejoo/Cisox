# 主程序 <-> 录制进程端到端自验证：不模拟任何鼠标键盘，靠 `--cmd recording` + 自动化环境变量驱动。
# 用法示例:
#   scripts/verify-snow-shot-recording.ps1 -Scenario normal -Format mp4 -Seconds 5
#   scripts/verify-snow-shot-recording.ps1 -Scenario pause  -Format mp4 -Seconds 6
#   scripts/verify-snow-shot-recording.ps1 -Scenario crash
#   scripts/verify-snow-shot-recording.ps1 -Scenario norecorder
param(
    [ValidateSet("normal", "pause", "crash", "norecorder")][string]$Scenario = "normal",
    [ValidateSet("mp4", "gif", "apng", "webp")][string]$Format = "mp4",
    [int]$Seconds = 5,
    [string]$Region = "0,0,1280,720",
    [string]$Exe = "",
    [string]$OutDir = (Join-Path $env:TEMP "snow-shot-e2e-rec")
)
$ErrorActionPreference = "Continue"
$repoRoot = (Resolve-Path (Join-Path $PSScriptRoot "..")).Path
if (-not $Exe) { $Exe = Join-Path $repoRoot "build/cargo/debug/snow-shot.exe" }
$logDir = Join-Path $env:LOCALAPPDATA "Cisox/logs"
New-Item -ItemType Directory -Force -Path $OutDir | Out-Null
Get-ChildItem $OutDir -Force | Remove-Item -Recurse -Force -ErrorAction SilentlyContinue

function Latest-Log { Get-ChildItem $logDir -Filter "cisox.*.log" | Sort-Object LastWriteTime -Descending | Select-Object -First 1 }
function New-LogLines([string]$path, [long]$offset) {
    if (-not $path -or -not (Test-Path $path)) { return @() }
    $fs = [System.IO.File]::Open($path, "Open", "Read", "ReadWrite")
    try { $fs.Seek($offset, "Begin") | Out-Null; (New-Object System.IO.StreamReader($fs, [Text.Encoding]::UTF8)).ReadToEnd() -split "`r?`n" } finally { $fs.Dispose() }
}

if (Get-Process snow-shot -ErrorAction SilentlyContinue) { throw "已有 snow-shot 在运行，请先退出" }
$log = Latest-Log
$logPath = $log.FullName
$offset = $log.Length

$plan = "$Region,$Seconds,$Format"
if ($Scenario -eq "pause") { $plan += ",2:3" }
$env:SNOW_RECORDING_AUTOTEST = $plan
$env:SNOW_RECORDING_AUTOTEST_DIR = $OutDir
if ($Scenario -eq "norecorder") { $env:SNOW_RECORDER_EXE = "Z:\definitely\missing\snow-recorder.exe" }
"plan=$plan out=$OutDir"

$primary = Start-Process -FilePath $Exe -PassThru
Start-Sleep -Seconds 5
"primary alive=$(-not $primary.HasExited) pid=$($primary.Id)"
$sw = [Diagnostics.Stopwatch]::StartNew()
Start-Process -FilePath $Exe -ArgumentList "--cmd", "recording" -Wait -PassThru | ForEach-Object { "secondary exit=$($_.ExitCode)" }

$deadline = (Get-Date).AddSeconds($Seconds + 40)
$killed = $false
$lines = @()
while ((Get-Date) -lt $deadline) {
    Start-Sleep -Milliseconds 500
    $lines = New-LogLines $logPath $offset
    if ($Scenario -eq "crash" -and -not $killed -and ($lines -match "录制进程已启动")) {
        Start-Sleep -Seconds 3
        $rec = Get-Process snow-recorder -ErrorAction SilentlyContinue
        "killing recorder: $($rec.Id -join ',') at t=$([int]$sw.Elapsed.TotalSeconds)s"
        $rec | Stop-Process -Force
        $killed = $true
    }
    if ($lines -match "录屏完成|录制窗已关闭") { break }
}
"--- log (relevant) ---"
$lines | Where-Object { $_ -match "录|record|Record|窗|overlay" } | ForEach-Object { $_ }
"--- state ---"
"snow-recorder still running: $([bool](Get-Process snow-recorder -ErrorAction SilentlyContinue))"
"primary alive after run: $(-not $primary.HasExited)"
"output dir listing (incl hidden):"
Get-ChildItem $OutDir -Force -Recurse | ForEach-Object { "  $($_.FullName)  $($_.Length)" }
$mp = Get-ChildItem $OutDir -File | Where-Object { $_.Name -notlike ".*" } | Select-Object -First 1
if ($mp) {
    if ($mp.Extension -eq ".webp") { "webp: $($mp.Length) bytes (use verify-snow-recorder.ps1 container check)" }
    else {
        "--- ffprobe ---"
        ffprobe -v error -count_frames -select_streams v:0 -show_entries "stream=codec_name,width,height,avg_frame_rate,nb_read_frames:format=duration,size,format_name" -of default=nw=1 $mp.FullName
        ffmpeg -v error -i $mp.FullName -f null - 2>&1
        "decode exit=$LASTEXITCODE"
    }
}
Start-Process -FilePath $Exe -ArgumentList "--cmd", "quit" -Wait | Out-Null
Start-Sleep -Seconds 2
if (-not $primary.HasExited) { $primary | Stop-Process -Force; "primary force-killed" } else { "primary exited cleanly code=$($primary.ExitCode)" }
