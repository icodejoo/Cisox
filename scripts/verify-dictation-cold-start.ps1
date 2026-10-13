# 听写冷启动真人计时（docs/research/dictation-cold-start.md §7 E2，worker 层）。
# 需要真人对着麦克风说话，无法无人值守：声学回环会被回声消除滤掉。
#
# 运行环境: PowerShell 7（pwsh；脚本含中文字面量，按 UTF-8 读取），Windows，已构建 snow-stt。
# 做什么: 模拟 UI main 最早处预拉 worker 的做法——一启动 snow-stt 就立刻发 START + PING，
#         记录 RECORDING（麦克风已开、已在录音）、PONG（模型加载完、识别就绪）、第一条识别文本的时间点，
#         然后在 -SpeakSeconds 后发 STOP，等 FINAL / STOPPED。
# 怎么说: 看到 "GO" 立刻开始报数，约 1 次 / 秒："一、二、三、四、五、六……"（别提前说，计时从进程启动算）。
#         结束后看“第一个被识别的数字”：判据是 ≤ "二"；越晚说明开头丢得越多。
#
# 用法: scripts/verify-dictation-cold-start.ps1 -ModelDir <模型目录> [-Rounds 5] [-SpeakSeconds 8] [-Threads 2]
#         [-LoadDelayMs 0] [-Exe <snow-stt.exe>]
#   -ModelDir:     流式模型目录（默认双语模型在 <数据根>\models\stt\<模型ID>，数据根通常是 %LOCALAPPDATA%\Cisox）。
#   -Rounds:       重复轮数，建议 5~10；每轮间隔 4 秒让你喘口气。
#   -SpeakSeconds: 每轮从 RECORDING 起说多久再发 STOP（模拟按住说话松开）。
#   -LoadDelayMs:  worker 加载模型前人为多等这么久（压力场景：模拟 10 s 以上的慢加载，测环形缓存与追赶）。
#   -Exe:          snow-stt.exe 路径，默认 build/stt/release/snow-stt.exe（先 scripts/build-snow-stt.ps1）。
#
# 判据（调研文档 §7 E2，worker 层部分）：
#   - 进程启动到 RECORDING（开麦）的中位数 ≤ 1.3 s，p95 ≤ 1.8 s（热页缓存）；
#   - 第一个被识别的数字 ≤ "二"；
#   - 追赶耗时（PONG 到积压喂完，近似为 PONG 到“文字追上实时”）≤ 积压时长 × 0.25；
#   - 同一轮里 STOP 在 PONG 之前到达时，文字完整出现、不被丢弃（-SpeakSeconds 小于加载时间即可复现）。
# 端到端（UI + 宿主）部分手工做：运行宿主 snow-shot.exe（两个 exe 都已构建），绑好听写热键，
#   冷启动场景请先退出所有 UI 窗口；按热键后立刻报数，观察托盘提示 / 右下角图标依次是：
#   加载中（进度条）→ 录音中（光环 + 进度条）→ 已就绪（光环），按住说话时在“录音中”松开应等加载完再出字。
# 不会写任何数据目录，不保存音频与识别文本之外的东西；识别文本只打印在本窗口。
param(
    [Parameter(Mandatory = $true)][string]$ModelDir,
    [int]$Rounds = 5,
    [int]$SpeakSeconds = 8,
    [int]$Threads = 2,
    [int]$LoadDelayMs = 0,
    [string]$Exe = ""
)
$ErrorActionPreference = "Stop"
$repoRoot = (Resolve-Path (Join-Path $PSScriptRoot "..")).Path
if ([string]::IsNullOrWhiteSpace($Exe)) { $Exe = Join-Path $repoRoot "build/stt/release/snow-stt.exe" }
if (-not (Test-Path $Exe)) { throw "找不到 $Exe，先运行 scripts/build-snow-stt.ps1" }
if (-not (Test-Path $ModelDir)) { throw "找不到模型目录: $ModelDir" }

# 第一个被识别出的数字：按中文数字与阿拉伯数字都认，返回 1..10 或 $null
function Get-FirstDigit([string]$text) {
    $map = @{ "一" = 1; "二" = 2; "两" = 2; "三" = 3; "四" = 4; "五" = 5; "六" = 6; "七" = 7; "八" = 8; "九" = 9; "十" = 10 }
    foreach ($ch in $text.ToCharArray()) {
        $s = [string]$ch
        if ($map.ContainsKey($s)) { return $map[$s] }
        if ($s -match "[1-9]") { return [int]$s }
    }
    return $null
}

# 简单分位数（升序数组，q 在 0..1）
function Get-Quantile([double[]]$values, [double]$q) {
    if ($values.Count -eq 0) { return $null }
    $sorted = $values | Sort-Object
    return $sorted[[math]::Min($sorted.Count - 1, [math]::Floor(($sorted.Count - 1) * $q))]
}

$results = @()
for ($round = 1; $round -le $Rounds; $round++) {
    Write-Host ""
    Write-Host "=== 第 $round / $Rounds 轮：3 秒后开始，看到 GO 立刻报数 ===" -ForegroundColor Cyan
    Start-Sleep -Seconds 3

    $psi = New-Object System.Diagnostics.ProcessStartInfo
    $psi.FileName = $Exe
    $psi.Arguments = if ($LoadDelayMs -gt 0) { "--load-delay-ms $LoadDelayMs --stats" } else { "--stats" }
    $psi.UseShellExecute = $false
    $psi.RedirectStandardInput = $true
    $psi.RedirectStandardOutput = $true
    $psi.RedirectStandardError = $true
    $psi.StandardOutputEncoding = [System.Text.Encoding]::UTF8
    $psi.StandardErrorEncoding = [System.Text.Encoding]::UTF8
    $psi.CreateNoWindow = $true

    Write-Host "GO" -ForegroundColor Green
    $sw = [System.Diagnostics.Stopwatch]::StartNew()
    $p = [System.Diagnostics.Process]::Start($psi)
    $errTask = $p.StandardError.ReadToEndAsync()
    $stdin = New-Object System.IO.StreamWriter($p.StandardInput.BaseStream, (New-Object System.Text.UTF8Encoding($false)))
    $stdin.NewLine = "`n"
    # 预拉起的做法：不等 READY，进程一起来就发 START + PING（与 UI main 最早处一致）
    $start = "START zh-en $Threads 2400 1200 20000 0 " + $ModelDir.Replace("\", "\\")
    $stdin.WriteLine($start)
    $stdin.WriteLine("PING")
    $stdin.Flush()

    $t = @{ ready = $null; recording = $null; pong = $null; firstText = $null; firstDigit = $null; firstDigitAt = $null; stopped = $null; stopSent = $null }
    $texts = New-Object System.Collections.Generic.List[string]
    $readTask = $p.StandardOutput.ReadLineAsync()
    while (-not $p.HasExited -or $readTask.IsCompleted) {
        if ($readTask.IsCompleted) {
            $line = $readTask.Result
            if ($null -eq $line) { break }
            $ms = $sw.ElapsedMilliseconds
            $readTask = $p.StandardOutput.ReadLineAsync()
            switch -Regex ($line) {
                "^READY" { $t.ready = $ms }
                "^RECORDING" { $t.recording = $ms }
                "^PONG" { if ($null -eq $t.pong) { $t.pong = $ms } }
                "^(PARTIAL|FINAL) (.*)$" {
                    if ($null -eq $t.firstText) { $t.firstText = $ms }
                    $kind = $Matches[1]; $text = $Matches[2]
                    $texts.Add("{0,6}ms {1} {2}" -f $ms, $kind, $text)
                    $d = Get-FirstDigit $text
                    if ($null -ne $d -and $null -eq $t.firstDigit) { $t.firstDigit = $d; $t.firstDigitAt = $ms }
                }
                "^STOPPED" { $t.stopped = $ms }
                "^ERROR" { Write-Host "ERROR: $line" -ForegroundColor Red }
            }
            continue
        }
        Start-Sleep -Milliseconds 10
        # 从 RECORDING 起说够 SpeakSeconds 就松开（模拟按住说话）
        if ($null -ne $t.recording -and $null -eq $t.stopSent -and ($sw.ElapsedMilliseconds - $t.recording) -ge ($SpeakSeconds * 1000)) {
            $stdin.WriteLine("STOP"); $stdin.Flush(); $t.stopSent = $sw.ElapsedMilliseconds
            Write-Host "（已松开：STOP）" -ForegroundColor DarkGray
        }
        if ($sw.Elapsed.TotalSeconds -gt 180) { $p.Kill(); throw "超时 180s，已结束自己启动的进程 $($p.Id)" }
    }
    $p.WaitForExit()
    Write-Host $errTask.Result.Trim() -ForegroundColor DarkGray
    $texts | ForEach-Object { Write-Host $_ }
    $summary = [pscustomobject]@{
        Round         = $round
        RecordingMs   = $t.recording
        PongMs        = $t.pong
        FirstTextMs   = $t.firstText
        FirstDigit    = $t.firstDigit
        FirstDigitMs  = $t.firstDigitAt
        StopSentMs    = $t.stopSent
        StoppedMs     = $t.stopped
        Exit          = $p.ExitCode
    }
    $results += $summary
    $summary | Format-List | Out-String | Write-Host
    Start-Sleep -Seconds 1
}

Write-Host ""
Write-Host "=== 汇总 ===" -ForegroundColor Cyan
$results | Format-Table -AutoSize | Out-String | Write-Host
$rec = @($results | Where-Object { $null -ne $_.RecordingMs } | ForEach-Object { [double]$_.RecordingMs })
if ($rec.Count -gt 0) {
    Write-Host ("进程启动 -> RECORDING：中位数 {0:N0} ms，p95 {1:N0} ms（判据 ≤ 1300 / ≤ 1800，热页缓存）" -f (Get-Quantile $rec 0.5), (Get-Quantile $rec 0.95))
}
$digits = @($results | Where-Object { $null -ne $_.FirstDigit } | ForEach-Object { [int]$_.FirstDigit })
if ($digits.Count -gt 0) {
    Write-Host ("第一个被识别的数字：{0}（判据 ≤ 2）" -f (($digits | Sort-Object) -join ", "))
}
Write-Host "追赶耗时请对照每轮 PongMs 与文字追上实时的时刻（PARTIAL 间隔回到约 1 s 一条）；按住说话场景请把 -SpeakSeconds 调小到小于加载时间再跑一次。"
