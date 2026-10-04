# 端到端自检 snow-stt：用 wav 代替麦克风，走真实 stdin/stdout 协议，记录事件序列、耗时与内存峰值。
# 用法: scripts/verify-snow-stt.ps1 -ModelDir <模型目录> -Wav <16k 单声道 wav> [-PadMs 3000] [-Threads 2] [-StopAfterMs 0] [-Exe <snow-stt.exe>]
#         [-Mode streaming|offline] [-Kind <模型类型>] [-VadModel <silero_vad.onnx>] [-Itn]
#   -Mode offline: 离线整句识别（VAD 切句），须配 -Kind offline-*；默认 streaming，START 行与旧版逐字节相同。
#   -Kind: online-transducer|offline-transducer|offline-paraformer|offline-sense-voice|offline-whisper|offline-zipformer-ctc|offline-nemo-ctc|offline-moonshine。
#   -VadModel: 仅做存在性与位置检查（worker 只按约定找 <模型目录>/silero_vad.onnx 或其上级目录，协议不传路径）。
#   -Itn: 开启逆文本规整（SenseVoice）。
#   -StopAfterMs > 0: 收到 READY 后发送 START，延迟这么久再发 STOP（验证主动停止路径）；0 表示等 wav 读完自然结束。
param(
    [Parameter(Mandatory = $true)][string]$ModelDir,
    [Parameter(Mandatory = $true)][string]$Wav,
    [int]$PadMs = 3000,
    [int]$Threads = 2,
    [int]$StopAfterMs = 0,
    [string]$Exe = "",
    [ValidateSet("streaming", "offline")][string]$Mode = "streaming",
    [string]$Kind = "",
    [string]$VadModel = "",
    [switch]$Itn,
    [string]$Language = "zh-en"
)
$ErrorActionPreference = "Stop"
$repoRoot = (Resolve-Path (Join-Path $PSScriptRoot "..")).Path
if ([string]::IsNullOrWhiteSpace($Exe)) { $Exe = Join-Path $repoRoot "build/stt/release/snow-stt.exe" }
if (-not (Test-Path $Exe)) { throw "找不到 $Exe，先运行 scripts/build-snow-stt.ps1" }

if ($VadModel) {
    if (-not (Test-Path $VadModel)) { throw "找不到 VAD 模型: $VadModel" }
    $vadFull = (Resolve-Path $VadModel).Path
    $modelFull = (Resolve-Path $ModelDir).Path
    $okDirs = @($modelFull, (Split-Path $modelFull -Parent))
    if ((Split-Path $vadFull -Leaf) -ne "silero_vad.onnx" -or ($okDirs -notcontains (Split-Path $vadFull -Parent))) {
        throw "worker 只按约定查找 silero_vad.onnx（模型目录或其上级目录），请把 VAD 模型放到: $($okDirs -join ' 或 ')"
    }
}

$psi = New-Object System.Diagnostics.ProcessStartInfo
$psi.FileName = $Exe
$psi.Arguments = "--wav `"$Wav`" --wav-pad-ms $PadMs --stats"
$psi.UseShellExecute = $false
$psi.RedirectStandardInput = $true
$psi.RedirectStandardOutput = $true
$psi.RedirectStandardError = $true
$psi.StandardOutputEncoding = [System.Text.Encoding]::UTF8
$psi.StandardErrorEncoding = [System.Text.Encoding]::UTF8
$psi.CreateNoWindow = $true

$sw = [System.Diagnostics.Stopwatch]::StartNew()
$p = [System.Diagnostics.Process]::Start($psi)
$p.PriorityClass = "BelowNormal"
$errTask = $p.StandardError.ReadToEndAsync()
# 输入按无 BOM 的 UTF-8 写（5.1 的 ProcessStartInfo 没有 StandardInputEncoding）
$stdin = New-Object System.IO.StreamWriter($p.StandardInput.BaseStream, (New-Object System.Text.UTF8Encoding($false)))
$stdin.NewLine = "`n"
Write-Host "pid=$($p.Id)"

# 异步逐行读 stdout，在主循环里轮询，给每行打时间戳
$lines = New-Object System.Collections.Generic.List[string]
$script:readTask = $p.StandardOutput.ReadLineAsync()
function Pump-Output {
    while ($script:readTask.IsCompleted) {
        $l = $script:readTask.Result
        if ($null -eq $l) { return }
        $lines.Add(("{0,7}ms  {1}" -f $sw.ElapsedMilliseconds, $l))
        $script:readTask = $p.StandardOutput.ReadLineAsync()
    }
}

while (-not ($lines | Where-Object { $_ -match "READY" }) -and -not $p.HasExited) {
    Start-Sleep -Milliseconds 20
    Pump-Output
}
$stdin.WriteLine("PING")
# 协议里模型目录按文本转义，反斜杠要写成两个
$prefix = ""
if ($Mode -eq "offline") { $prefix += "mode=offline " }
if ($Kind) { $prefix += "kind=$Kind " }
if ($Itn) { $prefix += "itn=1 " }
$start = "START $prefix$Language $Threads 2400 1200 20000 0 " + $ModelDir.Replace("\", "\\")
$stdin.WriteLine($start)
$stdin.Flush()
$sent = [System.Diagnostics.Stopwatch]::StartNew()
$stopped = $false
$peak = 0
while (-not $p.HasExited) {
    Start-Sleep -Milliseconds 100
    Pump-Output
    try { $p.Refresh(); if ($p.PeakWorkingSet64 -gt $peak) { $peak = $p.PeakWorkingSet64 } } catch {}
    if ($StopAfterMs -gt 0 -and -not $stopped -and $sent.ElapsedMilliseconds -ge $StopAfterMs) {
        $stdin.WriteLine("STOP"); $stdin.Flush(); $stopped = $true
    }
    if ($sw.Elapsed.TotalSeconds -gt 180) { $p.Kill(); throw "超时 180s，已结束自己启动的进程 $($p.Id)" }
}
$p.WaitForExit()
$total = $sw.ElapsedMilliseconds
Start-Sleep -Milliseconds 100
Pump-Output
$lines | ForEach-Object { Write-Host $_ }
Write-Host ("exit={0} total_ms={1} peak_ws_mb={2:N1}" -f $p.ExitCode, $total, ($peak / 1MB))
Write-Host ($errTask.Result.Trim())
