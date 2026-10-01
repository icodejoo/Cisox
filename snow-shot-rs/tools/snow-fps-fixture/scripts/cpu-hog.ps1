# 确定性 CPU 压力发生器（函数库，供 run-fps-test.ps1 的 -Hog 使用，也可手动点源后调用）。
# 原理：编译一个极小的 C# 控制台程序 snow-cpu-hog.exe（进程名固定、可识别），每个进程只有一个线程、普通优先级、
#       不停做整数运算，占满一个逻辑核；N 个压力 = N 个进程（不用线程池，调度行为可预期）。
#       每个进程自带最长存活时间（--seconds），即使调用方异常退出也会自行结束。
# 手动用法（注意：会真的占满 N 个逻辑核，请在机器空闲时用）:
#   . .\scripts\cpu-hog.ps1
#   Invoke-CpuHog -Count 4 -Seconds 20                          # 4 个压力进程，跑 20 秒后清理
#   Invoke-CpuHog -Count 2 -Seconds 20 -AffinityMask 0x3        # 全部压力进程限制在 0、1 号逻辑核上
# 注意：本文件没有 param 块，点源（. 路径）不会覆盖调用方的同名变量。

# 压力进程的固定名字（不含 .exe），追踪脚本按它把压力从"后台干扰"里单独标出。
$script:CpuHogName = "snow-cpu-hog"

# 压力进程源码：单线程忙循环，--seconds 为最长存活时间。
$script:CpuHogSource = @"
using System;
using System.Diagnostics;
using System.Globalization;
public static class SnowCpuHog {
    public static int Main(string[] args) {
        double seconds = 60;
        if (args.Length >= 2 && args[0] == "--seconds") {
            double.TryParse(args[1], NumberStyles.Float, CultureInfo.InvariantCulture, out seconds);
        }
        Stopwatch sw = Stopwatch.StartNew();
        ulong x = 88172645463325252UL;
        while (sw.Elapsed.TotalSeconds < seconds) {
            for (int i = 0; i < 2000000; i++) { x ^= x << 13; x ^= x >> 7; x ^= x << 17; }
        }
        return x == 0 ? 1 : 0;
    }
}
"@

# 取压力进程可执行文件路径；不存在则就地编译（约 1~2 秒，只编译一次，缓存在临时目录）。
function Get-CpuHogExe {
    param([string]$Dir = (Join-Path $env:TEMP "snow-cpu-hog"))
    $exe = Join-Path $Dir "$($script:CpuHogName).exe"
    if (-not (Test-Path $exe)) {
        New-Item -ItemType Directory -Force -Path $Dir | Out-Null
        Add-Type -TypeDefinition $script:CpuHogSource -OutputAssembly $exe -OutputType ConsoleApplication -ErrorAction Stop
    }
    return $exe
}

# 解析亲和性掩码（十六进制 0x.. 或十进制）；空串返回 0（不限制）。掩码必须与当前进程允许的核有交集。
function ConvertTo-AffinityMask {
    param([string]$Text = "")
    if (-not $Text.Trim()) { return [long]0 }
    $t = $Text.Trim()
    $mask = if ($t -match '^0[xX]([0-9a-fA-F]+)$') { [Convert]::ToInt64($Matches[1], 16) } else { [long]::Parse($t) }
    if ($mask -le 0) { throw "亲和性掩码必须为正数: $Text" }
    $allowed = [long](Get-Process -Id $PID).ProcessorAffinity
    if (($mask -band $allowed) -eq 0) { throw ("亲和性掩码 0x{0:X} 与当前可用核 0x{1:X} 没有交集" -f $mask, $allowed) }
    return $mask
}

# 启动 Count 个压力进程，返回进程对象数组。AffinityMask 非 0 时全部进程共用该掩码；MaxSeconds 是每个进程的最长存活时间。
function Start-CpuHog {
    param(
        [Parameter(Mandatory)][ValidateRange(1, 256)][int]$Count,
        [long]$AffinityMask = 0,
        [ValidateRange(1, 3600)][int]$MaxSeconds = 60
    )
    $stale = @(Get-Process -Name $script:CpuHogName -ErrorAction SilentlyContinue)
    if ($stale.Count -gt 0) { throw "已有 $($stale.Count) 个 $($script:CpuHogName) 进程在运行（上次残留或另一会话），先清理再试" }
    $exe = Get-CpuHogExe
    $started = New-Object System.Collections.Generic.List[object]
    try {
        for ($i = 0; $i -lt $Count; $i++) {
            $p = Start-Process -FilePath $exe -ArgumentList "--seconds", "$MaxSeconds" -PassThru -WindowStyle Hidden
            $started.Add($p)
            $p.PriorityClass = [System.Diagnostics.ProcessPriorityClass]::Normal
            if ($AffinityMask -ne 0) { $p.ProcessorAffinity = [IntPtr]$AffinityMask }
        }
    } catch {
        Stop-CpuHog -Processes $started.ToArray() | Out-Null
        throw
    }
    return $started.ToArray()
}

# 结束并等待给定压力进程退出，返回清理后仍然存活的 snow-cpu-hog 进程数（应为 0）。
function Stop-CpuHog {
    param([object[]]$Processes = @())
    foreach ($p in $Processes) {
        if ($p -and -not $p.HasExited) { try { $p.Kill() } catch { } }
    }
    foreach ($p in $Processes) {
        if ($p) { try { $null = $p.WaitForExit(5000) } catch { } }
    }
    return @(Get-Process -Name $script:CpuHogName -ErrorAction SilentlyContinue).Count
}

# 手动用：起 Count 个压力进程跑 Seconds 秒，try/finally 保证清理并检查无残留。
function Invoke-CpuHog {
    param(
        [Parameter(Mandatory)][ValidateRange(1, 256)][int]$Count,
        [ValidateRange(1, 3600)][int]$Seconds = 20,
        [string]$AffinityMask = ""
    )
    $mask = ConvertTo-AffinityMask $AffinityMask
    $hogs = @()
    try {
        $hogs = @(Start-CpuHog -Count $Count -AffinityMask $mask -MaxSeconds ($Seconds + 30))
        "压力已启动: $Count 个 $($script:CpuHogName)，掩码=$(if ($mask) { '0x{0:X}' -f $mask } else { '不限' })，持续 $Seconds 秒"
        Start-Sleep -Seconds $Seconds
    }
    finally {
        $left = Stop-CpuHog -Processes $hogs
        "压力已清理，残留 $($script:CpuHogName) 进程: $left"
    }
}
