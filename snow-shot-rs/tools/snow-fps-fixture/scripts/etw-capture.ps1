# ETW 抓取函数库（点源使用，供 run-fps-test.ps1 的 -Etw 调用，也可手动用）：抓取"桌面合成与呈现"底层事件，给录屏丢帧排查提供真值。
# 只用系统自带的 logman（开会话）与 tracerpt（转 CSV）；会话用 -ets 实时模式（免注册），用户态 provider 不需要内核 flag。
# 需要管理员权限。会话会带来额外开销（约 6~7 千事件/秒），属于测量探针效应，对比实验时两边要一致。
#
# 选用的 provider 与 keyword（均已用 logman query providers + wevtutil gp 在本机核实，事件号取自系统 manifest）:
#   Microsoft-Windows-Dwm-Core {9E9BBA3C-2E38-40CB-99F4-9E8281425164}  keyword 0x3  level 5
#     0x1 DwmCore（SCHEDULE_PROCESS_FRAME=10/12 每次 vblank 合成帧、SCHEDULE_PRESENT=15/16 DWM 呈现、141 呈现成功、
#         303 VSYNCDEADLINES、63~65 处理 PresentHistory）；0x2 DetailedFrameInformation（1=FRAMEINFO，含帧序号）。
#         63~65 为 Verbose(5)，所以 level 取 5。
#   Microsoft-Windows-DxgKrnl {802EC45A-1E99-4B83-9920-87C98277BA9D}  keyword 0x4000000000000000  level 5
#     该位即 "Microsoft-Windows-DxgKrnl/Performance" 通道位，我们要的事件 keyword 都是 0x4000000000000001：
#     17 VSyncDPC（每个 VidPnTarget 每次 vsync，载荷内含 vsync 的 QPC）、181 VSyncInterrupt、273 VSyncDPCMultiPlane、
#     318/319 DWMVsyncCountWait/Signal、215 PresentHistoryDetailed（应用 Present 的令牌）、173 PresentHistory Stop（DWM 取走令牌）、
#     184 Present、252/259/386 MMIOFlip/FlipMultiPlaneOverlay。不开 0x1(Base)：它带来大量 DDI 调用噪声（105/106 等）且没有额外价值。
#   Microsoft-Windows-DXGI {CA11C036-0102-4A2D-A6AD-F03CFED5D3C9}  keyword 0x2 (Events)  level 5
#     42/43 Present Start/Stop，带进程号，是夹具每次 Present 的调用/返回时刻（DWM 自己的 DXGI_PRESENT_TEST 也在里面，按 pid 过滤）。
#   未选 Microsoft-Windows-Win32k：本机 3 秒就产生 9 万事件（占总量 70%），对本目标没有用。
#
# 时钟：会话用 -ct perf（QPC，10MHz，最高精度）。tracerpt 输出的 Clock-Time 恒等于 A + QPC（A 为常数，单位 100ns），
#   A 可从 DxgKrnl VSyncDPC 载荷内嵌的 QPC 反推（etw_join.py 自动做，精度约 1 微秒）。
#   本脚本在会话启动后与停止前各取一组 (QPC, 挂钟 unix 微秒) 锚点（同一次调用内读 QPC 与 GetSystemTimePreciseAsFileTime，
#   取 8 次里括号最窄的一次，括号宽度即该锚点的读取误差上界，通常 < 5 微秒），写入 <etl>.clock.json；
#   夹具 frames.csv 的 unix_us 来自 Rust SystemTime::now()，同样是 GetSystemTimePreciseAsFileTime，所以与锚点同源。
# 手动用法:
#   . .\scripts\etw-capture.ps1
#   Start-EtwCapture -Name snowetw -Out C:\temp\x.etl      # 开会话
#   ...                                                    # 被测动作
#   $r = Stop-EtwCapture                                   # 停会话，返回 etl/clock.json 路径与丢失缓冲数（事件丢失数见 Convert-EtwToCsv 的返回值）
#   Convert-EtwToCsv -Etl $r.etl -Csv C:\temp\x.csv        # tracerpt 转 CSV
#   python analyze/etw_join.py --csv C:\temp\x.csv --clock $r.clock --pid <夹具pid> --fixture <frames.csv>
#   Stop-EtwCapture -Name snowetw -Force                   # 按会话名清理残留会话（脚本异常退出后用）
# 注意：本文件没有 param 块，点源（. 路径）不会覆盖调用方的同名变量。

# 当前会话状态（Start 写入，Stop 读取）。
$script:EtwState = $null
# 锚点读取器的 C# 源码。
$script:EtwClockSource = @"
using System;
using System.Runtime.InteropServices;
public static class SnowEtwClock {
    [DllImport("kernel32.dll")] static extern bool QueryPerformanceCounter(out long c);
    [DllImport("kernel32.dll")] static extern bool QueryPerformanceFrequency(out long f);
    [DllImport("kernel32.dll")] static extern void GetSystemTimePreciseAsFileTime(out long t);
    // 取 8 次采样中 QPC 括号最窄的一次，返回 {qpc 中点, unix 微秒, 括号宽度(ticks), 频率}。
    public static long[] Sample() {
        long freq; QueryPerformanceFrequency(out freq);
        long bestW = long.MaxValue, bestQ = 0, bestU = 0;
        for (int i = 0; i < 8; i++) {
            long q1, q2, ft;
            QueryPerformanceCounter(out q1);
            GetSystemTimePreciseAsFileTime(out ft);
            QueryPerformanceCounter(out q2);
            long w = q2 - q1;
            if (w < bestW) { bestW = w; bestQ = q1 + w / 2; bestU = (ft - 116444736000000000L) / 10; }
        }
        return new long[] { bestQ, bestU, bestW, freq };
    }
}
"@

# 取一组时钟锚点（QPC 与挂钟 unix 微秒同源采样）。
function Get-EtwClockAnchor {
    if (-not ('SnowEtwClock' -as [type])) { Add-Type -TypeDefinition $script:EtwClockSource -ErrorAction Stop }
    $s = [SnowEtwClock]::Sample()
    return [ordered]@{ qpc = $s[0]; unix_us = $s[1]; bracket_ticks = $s[2]; freq = $s[3] }
}

# 解析 logman query <会话> -ets 的计数（Buffers Lost / Buffers Written；logman 不报事件丢失数，事件丢失数以 tracerpt 摘要为准）。
function Get-EtwSessionCounters {
    param([Parameter(Mandatory)][string]$Name)
    $ErrorActionPreference = "Continue"
    $text = (logman query $Name -ets | Out-String)
    $r = [ordered]@{ buffers_lost = $null; buffers_written = $null }
    foreach ($pair in @(@("buffers_lost", "Buffers Lost"), @("buffers_written", "Buffers Written"))) {
        $m = [regex]::Match($text, [regex]::Escape($pair[1]) + '\s*:\s*(\d+)')
        if ($m.Success) { $r[$pair[0]] = [int64]$m.Groups[1].Value }
    }
    return $r
}

# 判断给定名字的 ETW 实时会话是否存在。
function Test-EtwSession {
    param([Parameter(Mandatory)][string]$Name)
    $ErrorActionPreference = "Continue"
    $null = logman query $Name -ets | Out-String
    return ($LASTEXITCODE -eq 0)
}

# 启动 ETW 实时会话并写入起始锚点。Name 为会话名；Out 为 .etl 路径；缓冲：每个 1MB，最少 64 个最多 256 个（够 10 秒量级）。
function Start-EtwCapture {
    param(
        [Parameter(Mandatory)][string]$Name,
        [Parameter(Mandatory)][string]$Out,
        [ValidateRange(64, 16384)][int]$BufferKB = 1024,
        [ValidateRange(4, 1024)][int]$MinBuffers = 64,
        [ValidateRange(4, 1024)][int]$MaxBuffers = 256
    )
    $ErrorActionPreference = "Continue"
    if ($null -ne $script:EtwState) { throw "已有进行中的 ETW 会话 '$($script:EtwState.name)'，先 Stop-EtwCapture" }
    if (Test-EtwSession $Name) { throw "同名 ETW 会话 '$Name' 已存在（上次残留？），先运行 Stop-EtwCapture -Name $Name -Force" }
    $outFull = [System.IO.Path]::GetFullPath($Out)
    $dir = Split-Path -Parent $outFull
    New-Item -ItemType Directory -Force -Path $dir | Out-Null
    # logman 只允许一个 -p，多个 provider 用 -pf 文件：每行 "{GUID} keyword level"
    $pf = "$outFull.providers.txt"
    @(
        "{9E9BBA3C-2E38-40CB-99F4-9E8281425164} 0x3 5",
        "{802EC45A-1E99-4B83-9920-87C98277BA9D} 0x4000000000000000 5",
        "{CA11C036-0102-4A2D-A6AD-F03CFED5D3C9} 0x2 5"
    ) | Set-Content -Encoding ASCII $pf
    $o = logman create trace $Name -ets -o $outFull -ct perf -nb $MinBuffers $MaxBuffers -bs $BufferKB -pf $pf | Out-String
    if ($LASTEXITCODE -ne 0) { throw "logman 创建 ETW 会话失败（需要管理员权限？）: $($o.Trim())" }
    $script:EtwState = [pscustomobject]@{ name = $Name; etl = $outFull; providers = $pf; anchors = @((Get-EtwClockAnchor)); buffer_kb = $BufferKB; min_buffers = $MinBuffers; max_buffers = $MaxBuffers }
    return $script:EtwState.etl
}

# 停止 ETW 会话。无参数：停当前会话，写 <etl>.clock.json，返回 @{ etl; clock; buffers_lost; buffers_written; ... }。
# -Name -Force：按会话名强制停止（吞掉所有错误，用于 finally 与残留清理），返回 $true/$false 表示该会话是否仍存在（应为 False）。
function Stop-EtwCapture {
    param([string]$Name = "", [switch]$Force)
    $ErrorActionPreference = "Continue"
    $st = $script:EtwState
    if (-not $Name -and $st) { $Name = $st.name }
    if (-not $Name) { throw "没有进行中的 ETW 会话；要清理残留请传 -Name <会话名> -Force" }
    $result = $null
    if ($st -and $st.name -eq $Name) {
        # 先取停止锚点与丢失计数，再停会话（停止后 logman 不再能查计数）
        $stopAnchor = Get-EtwClockAnchor
        $cnt = Get-EtwSessionCounters -Name $Name
        $null = logman stop $Name -ets | Out-String
        $stopCode = $LASTEXITCODE
        $clock = "$($st.etl).clock.json"
        $anchors = @($st.anchors) + @($stopAnchor)
        [ordered]@{ name = $Name; etl = $st.etl; freq = $stopAnchor.freq; anchors = $anchors; buffers_lost_at_stop = $cnt.buffers_lost; buffers_written_at_stop = $cnt.buffers_written } |
            ConvertTo-Json -Depth 4 | Set-Content -Encoding UTF8 $clock
        Remove-Item -ErrorAction SilentlyContinue $st.providers
        $script:EtwState = $null
        $result = [ordered]@{ etl = $st.etl; clock = $clock; stopped = ($stopCode -eq 0); buffers_lost = $cnt.buffers_lost; buffers_written = $cnt.buffers_written }
        if ($cnt.buffers_lost -gt 0) { Write-Warning "ETW 丢缓冲: buffers_lost=$($cnt.buffers_lost)，请加大 -BufferKB/-MaxBuffers 后重测" }
    } elseif (-not $Force) {
        throw "当前没有会话 '$Name' 的状态；强制清理请加 -Force"
    }
    if ($Force) {
        if (Test-EtwSession $Name) { $null = logman stop $Name -ets | Out-String }
        if ($st -and $st.name -eq $Name) { $script:EtwState = $null }
        $left = Test-EtwSession $Name
        if ($left) { Write-Warning "ETW 会话 '$Name' 仍存在，请手动 logman stop $Name -ets" }
        return $left   # -Force 只返回"会话是否仍存在"（应为 False）；有状态时 clock.json 仍会照常写出
    }
    return $result
}

# 用 tracerpt 把 .etl 转成 CSV 并返回其摘要里的事件数与丢失数；Csv 为输出 CSV 路径，Summary 默认放在 CSV 旁边。
function Convert-EtwToCsv {
    param(
        [Parameter(Mandatory)][string]$Etl,
        [Parameter(Mandatory)][string]$Csv,
        [string]$Summary = ""
    )
    $ErrorActionPreference = "Continue"
    if (-not $Summary) { $Summary = [System.IO.Path]::ChangeExtension($Csv, ".summary.txt") }
    $null = tracerpt $Etl -of CSV -o $Csv -summary $Summary -y | Out-String
    if ($LASTEXITCODE -ne 0 -or -not (Test-Path $Csv)) { throw "tracerpt 转换失败: $Etl" }
    $text = Get-Content $Summary -Raw
    $proc = [regex]::Match($text, 'Total Events\s+Processed\s+(\d+)')
    $lost = [regex]::Match($text, 'Total Events\s+Lost\s+(\d+)')
    return [ordered]@{ csv = $Csv; summary = $Summary; events_processed = $(if ($proc.Success) { [int64]$proc.Groups[1].Value } else { $null }); events_lost = $(if ($lost.Success) { [int64]$lost.Groups[1].Value } else { $null }) }
}
