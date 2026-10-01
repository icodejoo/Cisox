# 跨屏接缝对齐/光标验证驱动：拉起 grid 夹具(1920x1080，以接缝为中心) -> 行协议驱动 snow-recorder 录约 4 秒(光标关) -> check_seam.py；带 -Cursor 再录一段光标开 -> check_cursor.py。
# 安全约束：跨屏会占用主屏一部分，必须显式传 -AllowPrimary，否则中止；先用夹具 --check 校验区域，结束后确认夹具/录制进程均已退出。
# 用法示例:
#   scripts/run-seam-test.ps1 -AllowPrimary                 # 仅接缝对齐
#   scripts/run-seam-test.ps1 -AllowPrimary -Dual           # 双窗口：每块屏一个夹具窗口
#   scripts/run-seam-test.ps1 -AllowPrimary -Cursor         # 另加光标跨接缝检查（会移动鼠标到接缝处）
#   scripts/run-seam-test.ps1 -AllowPrimary -SeamX 2560 -RecorderExe D:\x\snow-recorder.exe
# 退出码: 0=全部通过，1=有检查不通过，其它=运行出错。
param(
    [string]$RecorderExe = "",
    [string]$FixtureExe = "",
    [switch]$AllowPrimary,
    [switch]$Dual,
    [int]$SeamX = 2560,
    [switch]$Cursor,
    [int]$CursorY = 500,
    [ValidateRange(2, 8)][int]$Seconds = 4,
    [string]$OutDir = (Join-Path $env:TEMP "snow-seam-test"),
    [string]$FfmpegDir = "C:\ProgramData\chocolatey\bin"
)
$ErrorActionPreference = "Stop"
# 选区固定 1920x1080（输出与源尺寸一致才能做像素级比对），以接缝为中心。
$W = 1920; $H = 1080
$X0 = $SeamX - $W / 2; $Y0 = 0
$Fps = 60
$toolRoot = (Resolve-Path (Join-Path $PSScriptRoot "..")).Path
$repoRoot = (Resolve-Path (Join-Path $toolRoot "../../..")).Path
if (-not $RecorderExe) { $RecorderExe = Join-Path $repoRoot "snow-shot-rs/tools/snow-recorder/target/release/snow-recorder.exe" }
if (-not $FixtureExe) { $FixtureExe = Join-Path $toolRoot "target/release/snow-fps-fixture.exe" }
if (-not $AllowPrimary) { throw "跨屏模式会占用主屏的一部分，必须传 -AllowPrimary，中止" }
if (-not (Test-Path $RecorderExe)) { throw "找不到录制进程: $RecorderExe" }
if (-not (Test-Path $FixtureExe)) { throw "找不到夹具: $FixtureExe" }
New-Item -ItemType Directory -Force -Path $OutDir | Out-Null
$stamp = Get-Date -Format "HHmmss"
$spanRegion = "$X0,$Y0,$W,$H"

# 夹具校验：区域不被显示器并集完整覆盖则一个窗口都不创建
[string[]]$dualArgs = if ($Dual) { "--dual" } else { @() }
$check = & $FixtureExe --check --span --allow-primary --region $spanRegion @dualArgs
if ($LASTEXITCODE -ne 0) { throw "夹具校验区域 $spanRegion 失败，中止: $check" }
Write-Warning "已传 -AllowPrimary：将占用主屏一部分 ($spanRegion) 约 $($Seconds + 3) 秒/段，期间请勿操作屏幕与鼠标"
Write-Output "校验通过: 区域=$spanRegion 接缝输出列=$($SeamX - $X0) fps=$Fps"

if ($Cursor) {
    # SetCursorPos 须在 DPI 感知下用物理像素坐标
    Add-Type -TypeDefinition @"
using System.Runtime.InteropServices;
public static class CursorNative {
  [DllImport("user32.dll")] public static extern bool SetProcessDPIAware();
  [DllImport("user32.dll")] public static extern bool SetCursorPos(int x, int y);
}
"@
    [void][CursorNative]::SetProcessDPIAware()
}

# 录制一段：拉起夹具 -> 就绪后启动录制进程 -> 录 $Seconds 秒 -> 停止；返回成品路径
function Invoke-Segment([string]$Name, [int]$CursorOn) {
    $base = Join-Path $OutDir ("{0}-{1}" -f $Name, $stamp)
    $outFile = "$base.mp4"; $readyFile = "$base.ready"; $fixOut = "$base.fixture.txt"
    $fixture = $null; $rec = $null
    try {
        $fixArgs = @("--span", "--allow-primary", "--region", $spanRegion, "--load", "grid", "--seconds", "$($Seconds + 2.5)", "--ready", $readyFile) + $dualArgs
        $fixture = Start-Process -FilePath $FixtureExe -ArgumentList $fixArgs -PassThru -WindowStyle Hidden -RedirectStandardOutput $fixOut
        $t0 = Get-Date
        while (-not (Test-Path $readyFile)) {
            if (((Get-Date) - $t0).TotalSeconds -gt 5) { throw "夹具 5 秒内未就绪" }
            Start-Sleep -Milliseconds 50
        }
        if ($Cursor) { [void][CursorNative]::SetCursorPos($SeamX, $CursorY) }
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
        $stdin.WriteLine("START $X0 $Y0 $W $H mp4 $Fps $CursorOn $outFile"); $stdin.Flush()
        $sw = [System.Diagnostics.Stopwatch]::StartNew()
        while ($sw.Elapsed.TotalSeconds -lt $Seconds) {
            if ($Cursor) { [void][CursorNative]::SetCursorPos($SeamX, $CursorY) }  # 防止被其它输入挪走
            Start-Sleep -Milliseconds 200
        }
        $stdin.WriteLine("STOP"); $stdin.Flush()
        if (-not $rec.WaitForExit(60000)) { throw "录制进程 60 秒未退出" }
        $null = $fixture.WaitForExit(15000)
        $ev = ($outTask.Result.Trim() -split "`r?`n" | Where-Object { $_ -match "error|fail|fallback|ERR" }) -join "; "
        $err = $errTask.Result.Trim()
        if ($ev) { Write-Host "[$Name] recorder: $ev" }
        if ($err) { Write-Host "[$Name] recorder stderr: $err" }
    }
    finally {
        foreach ($p in @($rec, $fixture)) { if ($p -and -not $p.HasExited) { try { $p.Kill() } catch { } } }
    }
    if (-not (Test-Path $outFile)) { throw "成品不存在: $outFile" }
    return $outFile
}

$failed = $false
try {
    $seamFile = Invoke-Segment "seam" 0
    "--- check_seam ($seamFile) ---"
    python (Join-Path $toolRoot "analyze/check_seam.py") $seamFile --x0 $X0 --seam-x $SeamX --ffmpeg-dir $FfmpegDir
    if ($LASTEXITCODE -ne 0) { $failed = $true }
    if ($Cursor) {
        # 光标关 = 上面的接缝段（光标开关均在同一静态画面上录制）
        $onFile = Invoke-Segment "cursor-on" 1
        "--- check_cursor (on=$onFile off=$seamFile) ---"
        python (Join-Path $toolRoot "analyze/check_cursor.py") $onFile $seamFile --x0 $X0 --y0 $Y0 --cursor-x $SeamX --cursor-y $CursorY --ffmpeg-dir $FfmpegDir
        if ($LASTEXITCODE -ne 0) { $failed = $true }
    }
}
finally {
    Start-Sleep -Milliseconds 300
    $left = @(Get-Process -Name snow-fps-fixture, snow-recorder -ErrorAction SilentlyContinue)
    "leftover processes: $($left.Count)"
    if ($left.Count -gt 0) { $left | ForEach-Object { try { $_.Kill() } catch { } }; $failed = $true }
}
if ($failed) { "RESULT: FAIL"; exit 1 }
"RESULT: PASS"
