# 帧率实测驱动：在非主屏上拉起夹具 -> 用行协议驱动 snow-recorder 录制 -> 分析成品。
# 安全约束：夹具默认只在非主屏创建窗口，单屏机器须显式传 -AllowPrimary 才占主屏；本脚本先用 --check 读取目标显示器真实 bounds，
#           主屏且未传开关即中止；录制区域必须落在该显示器内；结束后确认夹具/录制进程均已退出。
# 用法示例:
#   scripts/run-fps-test.ps1 -Size 2560x1440 -Fps 60 -Seconds 6
#   scripts/run-fps-test.ps1 -Size 1280x720  -Fps 30 -Seconds 6 -Format mp4
#   scripts/run-fps-test.ps1 -Size 1920x1080 -AllowPrimary   # 单屏机器：显式允许占用主屏
#   scripts/run-fps-test.ps1 -SpanRegion 1280,0,2560,1440 -AllowPrimary -Fps 60   # 跨屏：窗口与录制区域横跨两块屏（会占用主屏的一部分，须同时传 -AllowPrimary）
#   scripts/run-fps-test.ps1 -SpanRegion 1280,0,2560,1440 -AllowPrimary -Dual -Fps 60   # 双窗口：每块屏一个窗口各按自己的 vsync 出帧（排除单窗口跨屏的 DWM 合成抖动）
#   scripts/run-fps-test.ps1 -Size 1920x1080 -Fps 60 -Trace   # 帧级追踪：每轮建独立子目录，归档夹具/录制追踪/成品/分析/env.json/trace_join 报告
# 可用环境变量透传给录制进程: SNOW_RECORDER_CONV_THREADS / _ASYNC / _PRESET / _HARDWARE
# -Trace（默认关闭，不带时行为与之前完全一致）:
#   1) 给录制进程设 SNOW_RECORDER_FRAME_TRACE=<子目录>\<Tag>.trace.csv（录制进程帧级追踪，结束时一次性写出）；
#   2) 录制期间用 typeperf 每秒采样整机/各进程 CPU 与 GPU 引擎占用（录制结束后解析），并记录电源计划、显示器刷新率；
#   3) 写 env.json（含 interfered 标记与原因，判据见下方 $Interfere* 常量），并在分析后运行 analyze/trace_join.py 归因丢帧；
#   产物目录: <OutDir>\<Tag>-<时间戳>\（env.json、trace_join.json/txt、analysis.txt、recorder.txt 为固定文件名，其余以 <Tag> 为前缀）。
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
    [switch]$Trace,
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
$runDir = $null
if ($Trace) {
    # -Trace：每轮独立子目录，所有产物都放进去
    $runDir = $base
    New-Item -ItemType Directory -Force -Path $runDir | Out-Null
    $base = Join-Path $runDir $Tag
}
$outFile = "$base.$Format"
$logFile = "$base.frames.csv"
$readyFile = "$base.ready"
$fixOut = "$base.fixture.txt"
$traceCsv = "$base.trace.csv"
$prevTraceEnv = $env:SNOW_RECORDER_FRAME_TRACE

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


# ---- -Trace：后台干扰采样与"受干扰"判据（只在 -Trace 时使用；不带 -Trace 时这些定义不被调用）----
# 采样方式沿用 -Diag：typeperf 每秒一次，录制结束后再解析，录制期间脚本本身不做任何采样工作。
# CPU 口径：整机为占全部逻辑核的百分比；进程为占单核的百分比（多线程可超过 100）。
# 受干扰判据（任一满足即 interfered=true，原因写进 env.json 的 interference_reasons）:
$InterfereBgPeakPct = 60        # 非本测试进程（录制/夹具/dwm/typeperf/powershell/Idle）任一秒的 CPU 合计超过该值
$InterfereSuspectPeakPct = 20   # "可能干扰"名单内进程任一秒的 CPU 合计超过该值
$InterfereSystemMeanPct = 85    # 整机 CPU 均值超过该值
$InterfereSystemPeakPct = 95    # 整机 CPU 任一秒峰值超过该值
# "可能干扰"进程名单（构建/脚本/转码/杀毒/索引/更新）。
$SuspectPattern = '^(cargo|rustc|link|lld-link|cl|clang|msbuild|devenv|python|py|node|ffmpeg|MsMpEng|MpDefenderCoreService|NisSrv|SearchIndexer|TiWorker|TrustedInstaller|vctip)$'
# 本测试自身的进程（不计入后台占用）。
$SelfPattern = '^(snow-recorder|snow-fps-fixture|typeperf|powershell|pwsh|dwm|idle|_total)$'
# 解析 typeperf 的 CPU 采样（整机 + 各进程），每秒一条。
function Read-CpuSamples($path) {
    if (-not (Test-Path $path)) { return @() }
    $rows = @(Import-Csv $path)
    if ($rows.Count -gt 2) { $rows = @($rows | Select-Object -Skip 1) }   # 首行没有上一次采样的基线，值为 0
    if ($rows.Count -eq 0) { return @() }
    $cols = @($rows[0].PSObject.Properties.Name | Select-Object -Skip 1)
    $sysCol = $cols | Where-Object { $_ -match 'Processor\(_Total\)' } | Select-Object -First 1
    $procCols = @{}
    foreach ($c in $cols) {
        $m = [regex]::Match($c, '\\Process\((.+)\)\\% Processor Time$')
        if ($m.Success) { $procCols[$c] = ($m.Groups[1].Value -replace '#\d+$', '') }
    }
    $out = New-Object System.Collections.Generic.List[object]
    $i = 0
    foreach ($row in $rows) {
        $i++
        $sys = 0.0
        if ($sysCol) { [void][double]::TryParse($row.$sysCol, [ref]$sys) }
        $byName = @{}
        foreach ($c in $procCols.Keys) {
            $v = 0.0
            if (-not [double]::TryParse($row.$c, [ref]$v) -or $v -le 0) { continue }
            $n = $procCols[$c]
            $byName[$n] = [double]$byName[$n] + $v
        }
        $ranked = @($byName.GetEnumerator() | Where-Object { $_.Name -notmatch '^(idle|_total)$' } | Sort-Object Value -Descending)
        $bg = 0.0; $sus = 0.0; $susList = @()
        foreach ($e in $ranked) {
            if ($e.Name -notmatch $SelfPattern) { $bg += $e.Value }
            if ($e.Name -match $SuspectPattern) { $sus += $e.Value; $susList += [pscustomobject]@{ name = $e.Name; cpu_pct = [math]::Round($e.Value, 1) } }
        }
        $top5 = @($ranked | Select-Object -First 5 | ForEach-Object { [pscustomobject]@{ name = $_.Name; cpu_pct = [math]::Round($_.Value, 1) } })
        $out.Add([pscustomobject]@{ t = $i; system_cpu_pct = [math]::Round($sys, 1); top5 = $top5; suspects = @($susList); bg_total_pct = [math]::Round($bg, 1); suspect_total_pct = [math]::Round($sus, 1) })
    }
    return $out.ToArray()
}
# 按判据给出"受干扰"结论；没有采样数据时 interfered=$null（未知，不当作未受干扰）。
function Get-Interference($samples) {
    if ($samples.Count -eq 0) { return @{ interfered = $null; reasons = @("CPU 采样不可用（typeperf 没有产出，可能是系统语言导致计数器名不同）"); sys_mean = $null; sys_peak = $null; bg_peak = $null; suspect_peak = $null } }
    $sysMean = ($samples | Measure-Object system_cpu_pct -Average).Average
    $sysPeak = ($samples | Measure-Object system_cpu_pct -Maximum).Maximum
    $bgPeak = ($samples | Measure-Object bg_total_pct -Maximum).Maximum
    $susPeak = ($samples | Measure-Object suspect_total_pct -Maximum).Maximum
    $reasons = @()
    if ($bgPeak -ge $InterfereBgPeakPct) { $reasons += "后台进程 CPU 合计峰值 {0:N0}% >= {1}%" -f $bgPeak, $InterfereBgPeakPct }
    if ($susPeak -ge $InterfereSuspectPeakPct) { $reasons += "可能干扰进程 CPU 合计峰值 {0:N0}% >= {1}%" -f $susPeak, $InterfereSuspectPeakPct }
    if ($sysMean -ge $InterfereSystemMeanPct) { $reasons += "整机 CPU 均值 {0:N0}% >= {1}%" -f $sysMean, $InterfereSystemMeanPct }
    if ($sysPeak -ge $InterfereSystemPeakPct) { $reasons += "整机 CPU 峰值 {0:N0}% >= {1}%" -f $sysPeak, $InterfereSystemPeakPct }
    return @{ interfered = ($reasons.Count -gt 0); reasons = $reasons; sys_mean = [math]::Round($sysMean, 1); sys_peak = [math]::Round($sysPeak, 1); bg_peak = [math]::Round($bgPeak, 1); suspect_peak = [math]::Round($susPeak, 1) }
}
# 解析 typeperf 的 GPU 引擎占用：按引擎类型求全部进程之和与"录制/夹具/dwm"各自之和（每秒一个采样点，给出均值与峰值）。
function Read-GpuEngines($path) {
    if (-not (Test-Path $path)) { return @{} }
    $rows = @(Import-Csv $path)
    if ($rows.Count -eq 0) { return @{} }
    $cols = @($rows[0].PSObject.Properties.Name | Select-Object -Skip 1)
    $names = @{}
    $colInfo = @{}
    foreach ($c in $cols) {
        $m = [regex]::Match($c, 'pid_(\d+).*engtype_(\w+)')
        if (-not $m.Success) { continue }
        $procId = [int]$m.Groups[1].Value
        if (-not $names.ContainsKey($procId)) { $names[$procId] = (Get-Process -Id $procId -ErrorAction SilentlyContinue).ProcessName }
        $colInfo[$c] = @{ eng = $m.Groups[2].Value; name = $names[$procId] }
    }
    $series = @{}
    foreach ($row in $rows) {
        $acc = @{}
        foreach ($c in $colInfo.Keys) {
            $v = 0.0
            if (-not [double]::TryParse($row.$c, [ref]$v) -or $v -le 0) { continue }
            $k = "all/" + $colInfo[$c].eng
            $acc[$k] = [double]$acc[$k] + $v
            if ($colInfo[$c].name -in @("snow-recorder", "snow-fps-fixture", "dwm")) {
                $k2 = $colInfo[$c].name + "/" + $colInfo[$c].eng
                $acc[$k2] = [double]$acc[$k2] + $v
            }
        }
        foreach ($k in $acc.Keys) { if (-not $series.ContainsKey($k)) { $series[$k] = New-Object System.Collections.Generic.List[double] }; $series[$k].Add($acc[$k]) }
    }
    $result = [ordered]@{}
    foreach ($k in ($series.Keys | Sort-Object)) {
        $vals = $series[$k]
        $result[$k] = [ordered]@{ avg_pct = [math]::Round((($vals | Measure-Object -Sum).Sum / $rows.Count), 1); peak_pct = [math]::Round((($vals | Measure-Object -Maximum).Maximum), 1) }
    }
    return $result
}
# 电源计划（名称与 GUID，兼容本地化输出）。
function Get-PowerPlan {
    $text = (powercfg /getactivescheme | Out-String)
    $m = [regex]::Match($text, 'GUID:\s*([0-9a-fA-F-]{36})\s*\((.+?)\)')
    if ($m.Success) { return [ordered]@{ guid = $m.Groups[1].Value; name = $m.Groups[2].Value } }
    return [ordered]@{ guid = ""; name = $text.Trim() }
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

    if ($Trace) { $env:SNOW_RECORDER_FRAME_TRACE = $traceCsv }   # 只影响随后启动的录制进程
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
    $cpuCsv = "$base.cpu.csv"; $tpCpu = $null
    if ($Trace) { $tpCpu = Start-Process typeperf -ArgumentList "`"\Processor(_Total)\% Processor Time`"", "`"\Process(*)\% Processor Time`"", "-si", "1", "-sc", "$($Seconds + 2)", "-o", "`"$cpuCsv`"", "-y" -PassThru -WindowStyle Hidden }
    $stdin.WriteLine("START $region $Format $Fps $Cursor $outFile"); $stdin.Flush()

    $gpuCsv = "$base.gpu.csv"; $tp = $null
    if ($Diag -or $Trace) { $tp = Start-Process typeperf -ArgumentList "`"\GPU Engine(*)\Utilization Percentage`"", "-si", "1", "-sc", "$($Seconds + 1)", "-o", "`"$gpuCsv`"", "-y" -PassThru -WindowStyle Hidden }
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

    if ($Trace) {
        foreach ($p in @($tpCpu, $tp)) { if ($p) { $null = $p.WaitForExit(8000) } }
        $cpuSamples = @(Read-CpuSamples $cpuCsv)
        $verdict = Get-Interference $cpuSamples
        $recEnv = [ordered]@{}
        foreach ($e in @(Get-ChildItem Env: | Where-Object { $_.Name -like "SNOW_RECORDER_*" })) { $recEnv[$e.Name] = $e.Value }
        $envInfo = [ordered]@{
            tag = $Tag; size = $Size; fps = $Fps; seconds = $Seconds; format = $Format; load = $Load; cursor = $Cursor
            span_region = $SpanRegion; dual = [bool]$Dual; refresh_target = $Refresh
            recorder_env = $recEnv
            power_plan = Get-PowerPlan
            displays = @(Get-CimInstance Win32_VideoController | ForEach-Object { [ordered]@{ name = $_.Name; refresh_hz = $_.CurrentRefreshRate; width = $_.CurrentHorizontalResolution; height = $_.CurrentVerticalResolution } })
            logical_processors = [Environment]::ProcessorCount
            on_battery = [bool](@(Get-CimInstance Win32_Battery -ErrorAction SilentlyContinue | Where-Object { $_.BatteryStatus -eq 1 }).Count)
            interfered = $verdict.interfered; interference_reasons = @($verdict.reasons)
            thresholds = [ordered]@{ bg_peak_pct = $InterfereBgPeakPct; suspect_peak_pct = $InterfereSuspectPeakPct; system_mean_pct = $InterfereSystemMeanPct; system_peak_pct = $InterfereSystemPeakPct }
            peaks = [ordered]@{ system_mean_pct = $verdict.sys_mean; system_peak_pct = $verdict.sys_peak; bg_peak_pct = $verdict.bg_peak; suspect_peak_pct = $verdict.suspect_peak }
            gpu_engines = Read-GpuEngines $gpuCsv
            cpu_samples = $cpuSamples
        }
        $envInfo | ConvertTo-Json -Depth 6 | Set-Content -Encoding UTF8 (Join-Path $runDir "env.json")
        "--- trace env ---"
        "interfered={0} system_cpu mean/peak={1}/{2}% bg peak={3}% suspects peak={4}% reasons: {5}" -f $verdict.interfered, $verdict.sys_mean, $verdict.sys_peak, $verdict.bg_peak, $verdict.suspect_peak, (@($verdict.reasons) -join "; ")
    }

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
    if ($Trace) { ($outTask.Result + "`n" + $errTask.Result) | Set-Content -Encoding UTF8 (Join-Path $runDir "recorder.txt") }
    if ($samples.Count -gt 2) {
        $first = $samples[1]; $last = $samples[$samples.Count - 1]
        $cores = ($last.cpu - $first.cpu) / ($last.t - $first.t)
        "perf: cpu(1-core-equiv)={0:P0} of {1} logical  ws avg={2:N0}MB max={3:N0}MB  private max={4:N0}MB  threads max={5}" -f `
            $cores, [Environment]::ProcessorCount, (($samples | Measure-Object ws -Average).Average / 1MB), (($samples | Measure-Object ws -Maximum).Maximum / 1MB), (($samples | Measure-Object pm -Maximum).Maximum / 1MB), (($samples | Measure-Object th -Maximum).Maximum)
    }
}
finally {
    foreach ($p in @($rec, $fixture, $tp, $tpCpu)) { if ($p -and -not $p.HasExited) { try { $p.Kill() } catch { } } }
    if ($Trace) { if ($null -eq $prevTraceEnv) { Remove-Item Env:SNOW_RECORDER_FRAME_TRACE -ErrorAction SilentlyContinue } else { $env:SNOW_RECORDER_FRAME_TRACE = $prevTraceEnv } }
    Start-Sleep -Milliseconds 300
    $left = @(Get-Process -Name snow-fps-fixture, snow-recorder -ErrorAction SilentlyContinue)
    "leftover processes: $($left.Count)"
}
if (Test-Path $outFile) {
    "--- analysis ($outFile) ---"
    if ($Trace) {
        $analysisText = python (Join-Path $toolRoot "analyze/fps_analyze.py") $outFile --refresh $Refresh --log $logFile --ffmpeg-dir $FfmpegDir | Out-String
        $analysisText
        $analysisText | Set-Content -Encoding UTF8 (Join-Path $runDir "analysis.txt")
    } else {
        python (Join-Path $toolRoot "analyze/fps_analyze.py") $outFile --refresh $Refresh --log $logFile --ffmpeg-dir $FfmpegDir
    }
    "--- ffprobe ---"
    & (Join-Path $FfmpegDir "ffprobe.exe") -v error -select_streams v:0 -show_entries "stream=codec_name,width,height,avg_frame_rate,nb_frames:format=duration,size" -of default=nw=1 $outFile
} else { "!! 成品不存在: $outFile" }
if ($Trace) {
    "--- trace_join ---"
    if ((Test-Path $outFile) -and (Test-Path $traceCsv)) {
        python (Join-Path $toolRoot "analyze/trace_join.py") --fixture $logFile --trace $traceCsv --video $outFile --fps $Fps --refresh $Refresh --ffmpeg-dir $FfmpegDir --env (Join-Path $runDir "env.json") --report (Join-Path $runDir "trace_join.txt") --json (Join-Path $runDir "trace_join.json")
    } else { "!! 缺少追踪 csv 或成品，跳过 trace_join: $traceCsv" }
}

