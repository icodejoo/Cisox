# 帧率实测驱动：在非主屏上拉起夹具 -> 用行协议驱动 snow-recorder 录制 -> 分析成品。
# 安全约束：夹具默认只在非主屏创建窗口，单屏机器须显式传 -AllowPrimary 才占主屏；本脚本先用 --check 读取目标显示器真实 bounds，
#           主屏且未传开关即中止；录制区域必须落在该显示器内；结束后确认夹具/录制进程均已退出。
# 用法示例:
#   scripts/run-fps-test.ps1 -Size 2560x1440 -Fps 60 -Seconds 6
#   scripts/run-fps-test.ps1 -Size 1280x720  -Fps 30 -Seconds 6 -Format mp4
#   scripts/run-fps-test.ps1 -Size 1920x1080 -AllowPrimary   # 单屏机器：显式允许占用主屏
#   scripts/run-fps-test.ps1 -SpanRegion 1280,0,2560,1440 -AllowPrimary -Fps 60   # 跨屏：窗口与录制区域横跨两块屏（会占用主屏的一部分，须同时传 -AllowPrimary）
#   scripts/run-fps-test.ps1 -SpanRegion 1280,0,2560,1440 -AllowPrimary -Dual -Fps 60   # 双窗口：每块屏一个窗口各按自己的 vsync 出帧（排除单窗口跨屏的 DWM 合成抖动）
# 可用环境变量透传给录制进程: SNOW_RECORDER_CONV_THREADS / _ASYNC / _PRESET / _HARDWARE
param(
    [string]$Size = "2560x1440",
    [ValidateSet(30, 60)][int]$Fps = 60,
    [ValidateRange(2, 8)][int]$Seconds = 6,
    [ValidateSet("mp4", "gif", "apng", "webp")][string]$Format = "mp4",
    [ValidateSet("light", "stripes", "noise")][string]$Load = "noise",
    [double]$Refresh = 0,
    [string]$Tag = "run",
    [string]$OutDir = (Join-Path $env:TEMP "snow-fps-test"),
    [string]$RecorderExe = "",
    [string]$FixtureExe = "",
    [string]$FfmpegDir = "C:\ProgramData\chocolatey\bin",
    [switch]$Diag,
    [switch]$AllowPrimary,
    [string]$SpanRegion = "",
    [switch]$Dual,
    [ValidateSet(0, 1)][int]$Cursor = 1
)
$ErrorActionPreference = "Stop"
$toolRoot = (Resolve-Path (Join-Path $PSScriptRoot "..")).Path
$repoRoot = (Resolve-Path (Join-Path $toolRoot "../../..")).Path
if (-not $RecorderExe) { $RecorderExe = Join-Path $repoRoot "snow-shot-rs/tools/snow-recorder/target/release/snow-recorder.exe" }
if (-not $FixtureExe) { $FixtureExe = Join-Path $toolRoot "target/release/snow-fps-fixture.exe" }
New-Item -ItemType Directory -Force -Path $OutDir | Out-Null
$stamp = Get-Date -Format "HHmmss"
$base = Join-Path $OutDir ("{0}-{1}" -f $Tag, $stamp)
$outFile = "$base.$Format"
$logFile = "$base.frames.csv"
$readyFile = "$base.ready"
$fixOut = "$base.fixture.txt"

# 目标显示器校验：不通过则一个窗口都不创建（默认拒绝主屏；单屏机器须显式 -AllowPrimary）
$checkArgs = @("--check"); if ($AllowPrimary) { $checkArgs += "--allow-primary" }
if ($Dual -and -not $SpanRegion) { throw "-Dual 只能与 -SpanRegion 同用，中止" }
if ($SpanRegion) {
    if (-not $AllowPrimary) { throw "跨屏模式会占用主屏的一部分，必须同时传 -AllowPrimary，中止" }
    $checkArgs += @("--span", "--region", $SpanRegion)
    if ($Dual) { $checkArgs += "--dual" }
}
$check = & $FixtureExe @checkArgs
if ($LASTEXITCODE -ne 0) { throw "夹具校验目标显示器失败，中止: $check" }
$mon = [regex]::Match($check, "primary=(true|false) monitor=Rect \{ x: (-?\d+), y: (-?\d+), w: (\d+), h: (\d+) \}")
if (-not $mon.Success) { throw "无法解析显示器矩形: $check" }
$isPrimary = $mon.Groups[1].Value -eq "true"
$mx, $my, $mw, $mh = 2..5 | ForEach-Object { [int]$mon.Groups[$_].Value }
# 二次防线：未传 -AllowPrimary 时，主屏（Primary=true 或坐标 (0,0)）立刻中止；期望值取自夹具实测 bounds，不写死分辨率
if (-not $AllowPrimary -and ($isPrimary -or ($mx -eq 0 -and $my -eq 0))) { throw "目标显示器是主屏 ($mx,$my,${mw}x${mh})，未传 -AllowPrimary，中止" }
if ($AllowPrimary) { Write-Warning "已传 -AllowPrimary：将占用主屏 (${mw}x${mh}) 约 10 秒/轮，期间请勿操作屏幕" }
if ($SpanRegion) { $Size = "${mw}x${mh}" }  # 跨屏：夹具回显的 monitor 即窗口区域本身
$w, $h = $Size.ToLower().Split("x") | ForEach-Object { [int]$_ }
if ($w -le 0 -or $h -le 0 -or $w -gt $mw -or $h -gt $mh) { throw "尺寸 $Size 超出目标显示器 (${mw}x${mh})，中止" }
# 夹具窗口贴目标显示器左上角，录制区域与之重合
$region = "$mx $my $w $h"
Write-Output "显示器校验通过(主屏=$isPrimary): monitor=($mx,$my,${mw}x${mh}) 录制/夹具区域=($mx,$my,${w}x${h}) fps=$Fps 格式=$Format 负载=$Load"

# 诊断（-Diag）：按线程名统计 CPU（GetThreadDescription）与 GPU 引擎占用
if ($Diag) {
    Add-Type -TypeDefinition @"
using System; using System.Runtime.InteropServices;
public static class ThreadNames {
  [DllImport("kernel32.dll")] static extern IntPtr OpenThread(uint a, bool i, uint t);
  [DllImport("kernel32.dll")] static extern int GetThreadDescription(IntPtr h, out IntPtr d);
  [DllImport("kernel32.dll")] static extern bool CloseHandle(IntPtr h);
  [DllImport("kernel32.dll")] static extern IntPtr LocalFree(IntPtr h);
  public static string Get(uint tid) {
    IntPtr h = OpenThread(0x800, false, tid); if (h == IntPtr.Zero) return "";
    try { IntPtr d; if (GetThreadDescription(h, out d) != 0 || d == IntPtr.Zero) return ""; string s = Marshal.PtrToStringUni(d); LocalFree(d); return s; }
    finally { CloseHandle(h); }
  }
}
"@
}
# 取进程内每个线程的累计 CPU 秒
function Get-ThreadCpu($proc) {
    $proc.Refresh()
    $m = @{}
    foreach ($t in $proc.Threads) { try { $m[$t.Id] = $t.TotalProcessorTime.TotalSeconds } catch { } }
    $m
}
# 解析 typeperf 输出的 GPU 引擎占用：按 进程名+引擎类型 求各采样点之和的均值（只看录制/夹具/dwm）
function Read-GpuCsv($path) {
    $rows = @(Import-Csv $path)
    if ($rows.Count -eq 0) { return }
    $cols = $rows[0].PSObject.Properties.Name | Select-Object -Skip 1
    $acc = @{}
    foreach ($row in $rows) {
        foreach ($c in $cols) {
            $v = 0.0
            if (-not [double]::TryParse($row.$c, [ref]$v) -or $v -le 0) { continue }
            $m = [regex]::Match($c, "pid_(\d+).*engtype_(\w+)")
            if (-not $m.Success) { continue }
            $name = (Get-Process -Id ([int]$m.Groups[1].Value) -ErrorAction SilentlyContinue).ProcessName
            if ($name -notin @("snow-recorder", "snow-fps-fixture", "dwm")) { continue }
            $k = "$name/$($m.Groups[2].Value)"
            $acc[$k] = [double]$acc[$k] + $v
        }
    }
    $acc.GetEnumerator() | Sort-Object Name | ForEach-Object { "{0,-34} avg {1,6:N1}% (over {2} samples)" -f $_.Name, ($_.Value / $rows.Count), $rows.Count }
}

$divisor = if ($Fps -ge 60) { 1 } else { 2 }
if ($Refresh -le 0) { $Refresh = if ($Fps -ge 60) { 59.0 } else { 30.0 } }
$fixSeconds = $Seconds + 2.5
$fixture = $null; $rec = $null
try {
    $fixArgs = if ($SpanRegion) { @("--span", "--region", $SpanRegion) } else { @("--size", $Size) }
    if ($Dual) { $fixArgs += "--dual" }
    $fixArgs += @("--seconds", "$fixSeconds", "--divisor", "$divisor", "--load", $Load, "--log", $logFile, "--ready", $readyFile)
    if ($AllowPrimary) { $fixArgs += "--allow-primary" }
    $fixture = Start-Process -FilePath $FixtureExe -ArgumentList $fixArgs -PassThru -WindowStyle Hidden -RedirectStandardOutput $fixOut
    $t0 = Get-Date
    while (-not (Test-Path $readyFile)) {
        if (((Get-Date) - $t0).TotalSeconds -gt 5) { throw "夹具 5 秒内未就绪" }
        Start-Sleep -Milliseconds 50
    }
    Start-Sleep -Milliseconds 700

    $psi = New-Object System.Diagnostics.ProcessStartInfo
    $psi.FileName = $RecorderExe
    $psi.RedirectStandardInput = $true; $psi.RedirectStandardOutput = $true; $psi.RedirectStandardError = $true
    $psi.UseShellExecute = $false; $psi.CreateNoWindow = $true
    $utf8 = New-Object System.Text.UTF8Encoding($false)
    $psi.StandardOutputEncoding = $utf8; $psi.StandardErrorEncoding = $utf8
    $rec = New-Object System.Diagnostics.Process
    $rec.StartInfo = $psi
    [void]$rec.Start()
    $outTask = $rec.StandardOutput.ReadToEndAsync()
    $errTask = $rec.StandardError.ReadToEndAsync()
    $stdin = New-Object System.IO.StreamWriter($rec.StandardInput.BaseStream, $utf8)
    Start-Sleep -Milliseconds 500
    $stdin.WriteLine("START $region $Format $Fps $Cursor $outFile"); $stdin.Flush()

    $gpuCsv = "$base.gpu.csv"; $tp = $null
    if ($Diag) { $tp = Start-Process typeperf -ArgumentList "`"\GPU Engine(*)\Utilization Percentage`"", "-si", "1", "-sc", "$($Seconds + 1)", "-o", "`"$gpuCsv`"", "-y" -PassThru -WindowStyle Hidden }
    $samples = New-Object System.Collections.Generic.List[object]
    if ($Diag) { $thr0 = Get-ThreadCpu $rec; $thrT0 = [DateTime]::UtcNow }
    $sw = [System.Diagnostics.Stopwatch]::StartNew()
    while ($sw.Elapsed.TotalSeconds -lt $Seconds) {
        try {
            $rec.Refresh()
            $samples.Add([pscustomobject]@{ t = $sw.Elapsed.TotalSeconds; cpu = $rec.TotalProcessorTime.TotalSeconds; ws = $rec.WorkingSet64; pm = $rec.PrivateMemorySize64; th = $rec.Threads.Count })
        } catch { }
        Start-Sleep -Milliseconds 250
    }
    if ($Diag) { $thr1 = Get-ThreadCpu $rec; $thrSecs = ([DateTime]::UtcNow - $thrT0).TotalSeconds }
    $stdin.WriteLine("STOP"); $stdin.Flush()
    if (-not $rec.WaitForExit(60000)) { throw "录制进程 60 秒未退出" }
    $null = $fixture.WaitForExit(15000)

    if ($Diag) {
        "--- recorder threads: CPU delta (1-core-equiv) over {0:N1}s, top 14 ---" -f $thrSecs
        $thr1.GetEnumerator() | ForEach-Object { [pscustomobject]@{ tid = $_.Key; cpu = ($_.Value - $(if ($thr0.ContainsKey($_.Key)) { $thr0[$_.Key] } else { 0 })) / $thrSecs } } |
            Sort-Object cpu -Descending | Select-Object -First 14 | ForEach-Object { "{0,6} {1,6:P0}  {2}" -f $_.tid, $_.cpu, [ThreadNames]::Get([uint32]$_.tid) }
    }
    if ($Diag -and $tp) { $null = $tp.WaitForExit(8000); "--- gpu engine utilisation (recorder / fixture / dwm) ---"; Read-GpuCsv $gpuCsv }
    "--- recorder events ---"
    $outTask.Result.Trim()
    "--- recorder stderr ---"
    $errTask.Result.Trim()
    "--- fixture ---"
    (Get-Content $fixOut -ErrorAction SilentlyContinue) -join "`n"
    if ($samples.Count -gt 2) {
        $first = $samples[1]; $last = $samples[$samples.Count - 1]
        $cores = ($last.cpu - $first.cpu) / ($last.t - $first.t)
        "perf: cpu(1-core-equiv)={0:P0} of {1} logical  ws avg={2:N0}MB max={3:N0}MB  private max={4:N0}MB  threads max={5}" -f `
            $cores, [Environment]::ProcessorCount, (($samples | Measure-Object ws -Average).Average / 1MB), (($samples | Measure-Object ws -Maximum).Maximum / 1MB), (($samples | Measure-Object pm -Maximum).Maximum / 1MB), (($samples | Measure-Object th -Maximum).Maximum)
    }
}
finally {
    foreach ($p in @($rec, $fixture, $tp)) { if ($p -and -not $p.HasExited) { try { $p.Kill() } catch { } } }
    Start-Sleep -Milliseconds 300
    $left = @(Get-Process -Name snow-fps-fixture, snow-recorder -ErrorAction SilentlyContinue)
    "leftover processes: $($left.Count)"
}
if (Test-Path $outFile) {
    "--- analysis ($outFile) ---"
    python (Join-Path $toolRoot "analyze/fps_analyze.py") $outFile --refresh $Refresh --log $logFile --ffmpeg-dir $FfmpegDir
    "--- ffprobe ---"
    & (Join-Path $FfmpegDir "ffprobe.exe") -v error -select_streams v:0 -show_entries "stream=codec_name,width,height,avg_frame_rate,nb_frames:format=duration,size" -of default=nw=1 $outFile
} else { "!! 成品不存在: $outFile" }

