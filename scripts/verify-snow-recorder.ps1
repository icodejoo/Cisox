# 录制进程自验证驱动：按行协议驱动 snow-recorder.exe，并用 ffprobe 校验产物。
# 用法示例:
#   scripts/verify-snow-recorder.ps1 -Scenario record -Format mp4 -Seconds 5
#   scripts/verify-snow-recorder.ps1 -Scenario pause  -Format mp4 -Seconds 6
#   scripts/verify-snow-recorder.ps1 -Scenario kill
#   scripts/verify-snow-recorder.ps1 -Scenario record -Region 0,0,2560,1440 -Fps 60 -Seconds 10 -Sample
param(
    [ValidateSet("record", "pause", "kill", "eof")][string]$Scenario = "record",
    [ValidateSet("mp4", "gif", "apng", "webp")][string]$Format = "mp4",
    [int]$Seconds = 5,
    [int]$Fps = 30,
    [string]$Region = "0,0,1280,720",
    [switch]$Sample,
    [string]$OutDir = (Join-Path $env:TEMP "snow-recorder-verify"),
    [string]$Exe = ""
)
$ErrorActionPreference = "Stop"
$repoRoot = (Resolve-Path (Join-Path $PSScriptRoot "..")).Path
if (-not $Exe) { $Exe = Join-Path $repoRoot "snow-shot-rs/tools/snow-recorder/target/release/snow-recorder.exe" }
New-Item -ItemType Directory -Force -Path $OutDir | Out-Null
$out = Join-Path $OutDir ("verify-{0}-{1}.{2}" -f $Scenario, (Get-Date -Format "HHmmss"), $Format)
$x, $y, $w, $h = $Region.Split(@(",", " "), [System.StringSplitOptions]::RemoveEmptyEntries) | ForEach-Object { [int]$_ }

$psi = New-Object System.Diagnostics.ProcessStartInfo
$psi.FileName = $Exe
$psi.RedirectStandardInput = $true
$psi.RedirectStandardOutput = $true
$psi.RedirectStandardError = $true
$psi.UseShellExecute = $false
$utf8 = New-Object System.Text.UTF8Encoding($false)
$psi.StandardOutputEncoding = $utf8
$psi.StandardErrorEncoding = $utf8
$psi.CreateNoWindow = $true
$proc = New-Object System.Diagnostics.Process
$proc.StartInfo = $psi
$events = New-Object System.Collections.Concurrent.ConcurrentQueue[string]
$sw = [System.Diagnostics.Stopwatch]::StartNew()
Register-ObjectEvent -InputObject $proc -EventName OutputDataReceived -MessageData @{ q = $events; sw = $sw } -Action {
    if ($EventArgs.Data) { $Event.MessageData.q.Enqueue(("{0,8:N0}ms {1}" -f $Event.MessageData.sw.ElapsedMilliseconds, $EventArgs.Data)) }
} | Out-Null
[void]$proc.Start()
$proc.BeginOutputReadLine()
$errTask = $proc.StandardError.ReadToEndAsync()

$stdin = New-Object System.IO.StreamWriter($proc.StandardInput.BaseStream, $utf8)
function Send([string]$line) { $stdin.WriteLine($line); $stdin.Flush() }

# 每 500ms 采样 CPU/内存（-Sample）
$samples = @()
function Sample-Once {
    $p = Get-Process -Id $proc.Id -ErrorAction SilentlyContinue
    if ($p) { $script:samples += [pscustomobject]@{ cpu = $p.TotalProcessorTime.TotalSeconds; ws = $p.WorkingSet64; t = $sw.Elapsed.TotalSeconds } }
}
function Wait-Active([double]$secs) {
    $end = [DateTime]::UtcNow.AddSeconds($secs)
    while ([DateTime]::UtcNow -lt $end) { if ($Sample) { Sample-Once }; Start-Sleep -Milliseconds 500 }
}

Start-Sleep -Milliseconds 800
Send ("START $x $y $w $h $Format $Fps 1 $out")
switch ($Scenario) {
    "record" { Wait-Active $Seconds; Send "STOP" }
    "pause" {
        # 录 2 秒 → 暂停 3 秒 → 再录 (Seconds-2) 秒；期望有效时长 ≈ Seconds
        Wait-Active 2; Send "PAUSE"; Start-Sleep -Seconds 3; Send "RESUME"; Wait-Active ($Seconds - 2); Send "STOP"
    }
    "kill" { Start-Sleep -Seconds 2; $proc.Kill() }
    "eof" { Start-Sleep -Seconds 2; $stdin.Close() }
}
$exited = $proc.WaitForExit(60000)
Start-Sleep -Milliseconds 500
"--- events ---"
$events.ToArray() | ForEach-Object { $_ }
"--- exit: exited=$exited code=$($proc.ExitCode) stderr=[$($errTask.Result.Trim())] ---"
"final exists:   $(Test-Path $out)  $out"
$leftovers = @(Get-ChildItem -LiteralPath $OutDir -Force -Filter ".snow-recording-*" -ErrorAction SilentlyContinue | Where-Object { $_.Name -like ".snow-recording-$($proc.Id)*" })
"scratch leftovers for pid $($proc.Id): $($leftovers.Count)  $($leftovers.Name -join ',')"
if ($Sample -and $samples.Count -gt 2) {
    $first = $samples[1]; $last = $samples[-1]
    $cpuPct = 100 * ($last.cpu - $first.cpu) / ($last.t - $first.t) / [Environment]::ProcessorCount
    $avgWs = ($samples | Measure-Object ws -Average).Average / 1MB
    $maxWs = ($samples | Measure-Object ws -Maximum).Maximum / 1MB
    "perf: cpu(all-cores %)={0:N1}  cpu(1-core-equiv %)={1:N0}  ws avg={2:N0}MB max={3:N0}MB" -f $cpuPct, ($cpuPct * [Environment]::ProcessorCount), $avgWs, $maxWs
}
# 动画 WebP：ffprobe 不支持，直接解析 RIFF 容器（VP8X 动画标志、画布尺寸、ANMF 帧数与总时长）
function Inspect-AnimatedWebp([string]$path) {
    $b = [System.IO.File]::ReadAllBytes($path)
    $riff = [System.Text.Encoding]::ASCII.GetString($b, 0, 4)
    $webp = [System.Text.Encoding]::ASCII.GetString($b, 8, 4)
    $pos = 12; $frames = 0; $durMs = 0; $cw = 0; $ch = 0; $anim = $false
    while ($pos + 8 -le $b.Length) {
        $tag = [System.Text.Encoding]::ASCII.GetString($b, $pos, 4)
        $len = [BitConverter]::ToUInt32($b, $pos + 4)
        if ($tag -eq "VP8X") {
            $anim = ($b[$pos + 8] -band 0x02) -ne 0
            $cw = ($b[$pos + 12] + 256 * $b[$pos + 13] + 65536 * $b[$pos + 14]) + 1
            $ch = ($b[$pos + 15] + 256 * $b[$pos + 16] + 65536 * $b[$pos + 17]) + 1
        }
        if ($tag -eq "ANMF") { $frames++; $durMs += $b[$pos + 8 + 12] + 256 * $b[$pos + 8 + 13] + 65536 * $b[$pos + 8 + 14] }
        $pos += 8 + $len + ($len % 2)
    }
    "webp: riff=$riff/$webp animated=$anim canvas=${cw}x${ch} frames=$frames total_duration_ms=$durMs size=$($b.Length)"
}
if (Test-Path $out) {
    if ($Format -eq "webp") { "--- webp container ---"; Inspect-AnimatedWebp $out }
    else {
        "--- ffprobe ---"
        ffprobe -v error -count_frames -select_streams v:0 -show_entries "stream=codec_name,width,height,avg_frame_rate,nb_read_frames:format=duration,size,format_name" -of default=nw=1 $out
        "--- decode check ---"
        ffmpeg -v error -i $out -f null - 2>&1
        "decode exit=$LASTEXITCODE"
    }
}
