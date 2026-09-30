# 方案 3：ffmpeg 命令行硬件帧链路实测（ddagrab 真实捕获，仅副屏）。
# 副屏按属性校验：非主屏且 bounds = 2560,0,2560x1440；找不到则整体跳过。每轮 <=10s。
param(
    [string]$Out = "C:\Users\jelon\AppData\Local\Temp\claude\E--workspaces-Cisox\e9c32797-6599-4c85-bd8b-95c077321832\scratchpad\spike\data",
    [int]$Seconds = 8
)
$ErrorActionPreference = "Continue"
$exe = Join-Path $PSScriptRoot "target\release\recording-convert-spike.exe"
New-Item -ItemType Directory -Force $Out | Out-Null
$list = (& $exe dxgi-list | Select-Object -Last 1) | ConvertFrom-Json
Add-Content -Path (Join-Path $Out "ffmpeg.jsonl") -Value ($list | ConvertTo-Json -Compress -Depth 5) -Encoding utf8
if ($null -eq $list.secondary) { Write-Host "找不到满足属性的副屏，跳过 ffmpeg 实测"; exit 0 }
$idx = $list.secondary.output
$dd = "ddagrab=output_idx=${idx}"
# 直接使用真实 ffmpeg.exe（choco 的 shim 会多一层进程，内存/退出码都不准）
$ff = 'C:\ProgramData\chocolatey\lib\ffmpeg\tools\ffmpeg\bin\ffmpeg.exe'

# 运行一个场景：名称、滤镜图、编码/输出参数
function Scenario([string]$name, [string]$graph, [string[]]$outArgs) {
    $log = Join-Path $Out ("ffmpeg_{0}.log" -f $name)
    $args1 = @("-hide_banner", "-y", "-benchmark", "-stats", "-loglevel", "info", "-filter_complex", $graph) + $outArgs + @("-t", "$Seconds", "-f", "null", "-")
    $p = Start-Process -FilePath $ff -ArgumentList $args1 -PassThru -NoNewWindow -RedirectStandardError $log -RedirectStandardOutput "$log.out"
    $null = $p.Handle
    $peak = 0
    while (-not $p.HasExited) { $p.Refresh(); $peak = [Math]::Max($peak, $p.PeakWorkingSet64); Start-Sleep -Milliseconds 200 }
    $p.WaitForExit()
    $txt = (Get-Content $log -Raw) -replace "`r", "`n"
    function LastMatch([string]$re) { $m = [regex]::Matches($txt, $re); if ($m.Count -gt 0) { return $m[$m.Count - 1].Groups[1].Value } else { return "" } }
    $frames = LastMatch "frame=\s*(\d+)"
    $bench = [regex]::Match($txt, "bench: utime=([\d.]+)s stime=([\d.]+)s rtime=([\d.]+)s")
    $speed = LastMatch "speed=\s*([\d.]+)x"
    $dup = LastMatch "dup=(\d+)"
    $drop = LastMatch "drop=(\d+)"
    $err = if ($p.ExitCode -ne 0 -or -not $frames) { ($txt -split "`n" | Where-Object { $_ -match "rror|Failed|Invalid|Nothing was written" } | Select-Object -First 3) -join " | " } else { "" }
    $obj = [ordered]@{ kind = "ffmpeg"; name = $name; ok = ($p.ExitCode -eq 0 -and [int]("0" + $frames) -gt 0); exit = $p.ExitCode; frames = [int]("0" + $frames); speed = $speed; dup = $dup; drop = $drop; peak_ws_mb = [Math]::Round($peak / 1MB); error = $err }
    if ($bench.Success) {
        $rt = [double]$bench.Groups[3].Value
        $obj.rtime_s = $rt
        $obj.fps = if ($rt -gt 0) { [Math]::Round([int]("0" + $frames) / $rt, 1) } else { 0 }
        $obj.cpu_pct = if ($rt -gt 0) { [Math]::Round(([double]$bench.Groups[1].Value + [double]$bench.Groups[2].Value) / $rt * 100, 1) } else { 0 }
    }
    $obj.graph = $graph; $obj.out = ($outArgs -join " ")
    $line = $obj | ConvertTo-Json -Compress
    Add-Content -Path (Join-Path $Out "ffmpeg.jsonl") -Value $line -Encoding utf8
    Write-Host $line.Substring(0, [Math]::Min(200, $line.Length))
    Start-Sleep -Seconds 2
}

$qsvChain = "hwmap=derive_device=qsv,format=qsv"
# 可行性：scale_d3d11（预期失败，记录错误）
Scenario "scale_d3d11_qsv" "${dd}:framerate=60,scale_d3d11=format=nv12,$qsvChain" @("-c:v", "h264_qsv")
# 主链路：ddagrab -> hwmap(qsv) -> vpp_qsv/scale_qsv -> h264_qsv
Scenario "vpp_qsv_default_60" "${dd}:framerate=60,$qsvChain,vpp_qsv=format=nv12" @("-c:v", "h264_qsv")
Scenario "vpp_qsv_veryfast_a4_60" "${dd}:framerate=60,$qsvChain,vpp_qsv=format=nv12" @("-c:v", "h264_qsv", "-preset", "veryfast", "-async_depth", "4")
Scenario "vpp_qsv_veryfast_a1_60" "${dd}:framerate=60,$qsvChain,vpp_qsv=format=nv12" @("-c:v", "h264_qsv", "-preset", "veryfast", "-async_depth", "1")
Scenario "scale_qsv_default_60" "${dd}:framerate=60,$qsvChain,scale_qsv=format=nv12" @("-c:v", "h264_qsv")
Scenario "vpp_qsv_1080p_veryfast_a4_60" "${dd}:framerate=60,$qsvChain,vpp_qsv=w=1920:h=1080:format=nv12" @("-c:v", "h264_qsv", "-preset", "veryfast", "-async_depth", "4")
# 余量：framerate=240 + dup_frames（源画面静止时复制帧，测滤镜+编码吞吐，不代表真实内容）
Scenario "vpp_qsv_veryfast_a4_240dup" "${dd}:framerate=240:dup_frames=1,$qsvChain,vpp_qsv=format=nv12" @("-c:v", "h264_qsv", "-preset", "veryfast", "-async_depth", "4", "-fps_mode", "passthrough")
Scenario "vpp_qsv_default_240dup" "${dd}:framerate=240:dup_frames=1,$qsvChain,vpp_qsv=format=nv12" @("-c:v", "h264_qsv", "-fps_mode", "passthrough")
# 仅转换（含下载到内存，下载开销计入）与仅捕获
Scenario "conv_only_vpp_qsv_download_240dup" "${dd}:framerate=240:dup_frames=1,$qsvChain,vpp_qsv=format=nv12,hwdownload,format=nv12" @("-fps_mode", "passthrough")
Scenario "capture_only_download_bgra_240dup" "${dd}:framerate=240:dup_frames=1,hwdownload,format=bgra" @("-fps_mode", "passthrough")
