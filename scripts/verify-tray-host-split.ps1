<#
.SYNOPSIS
托盘宿主 + UI 双进程的真屏验收（docs/design/tray-host-split.md §8 阶段 2 的硬验收、§10 第 14 条）。

.DESCRIPTION
阶段 2 的离屏部分（状态机 / 队列 / 仲裁 / 熔断 / 焦点授权决策 / spec 原子写 / 导入表）由 cargo 测试覆盖，
这个脚本只做“必须有真屏才能验”的部分，逐项给出 PASS / FAIL / INFO 和数值：

  idle         托盘态：整套进程只有宿主一个（没有 UI、没有 worker）；宿主私有提交 <= 6MB（提交与工作集分开记）。
  ime          弹出托盘菜单后宿主模块里没有 *.ime / 第三方输入法 DLL；弹菜单后私有提交仍 <= 6MB。
  hotkey       热键（默认 F1）-> 覆盖窗首帧：冷拉起 N 轮取中位数，验收线 <= 1.14s（阶段 5 目标 0.9s / 0.7s）；
               UI 拿到前台与键盘焦点（Esc 能关掉覆盖窗）；关掉后 UI 进程 3 秒内消失，宿主仍在。
  tray-focus   托盘菜单项（设置）拉起 UI 后窗口在前台、有键盘焦点；窗口关闭后 UI 退出。
  cmd          --cmd 七个命令（show / settings / screenshot / recording / scroll-capture / pin-clipboard / quit）。
  settings-hotkey  设置里改热键：冲突时回滚配置并提示，成功时新热键生效（用本脚本进程占住一个热键制造冲突）。
  pause-gate   暂停热键开关、前台全屏闸门（用一个全屏置顶窗口模拟全屏应用）。
  crash        杀掉 UI 进程（只按 PID）后宿主仍在、下一次热键能重新拉起；3 次连杀触发熔断提示。
  admin        宿主以管理员运行（UAC 弹一次）时热键 / --cmd / 焦点正常；--cmd 来自非管理员 shell（混合权限）。

.PARAMETER Scenario
要跑的场景，默认 all（不含 admin，admin 要单独指定，因为会弹 UAC）。

.PARAMETER HostExe
宿主 exe，默认 build\cargo\release\snow-shot.exe。UI exe 取它同目录的 snow-shot-ui.exe（-UiExe 可覆盖）。

.PARAMETER Rounds
hotkey 场景的轮数（取中位数），默认 3。

.NOTES
运行方式：用 Windows PowerShell 5.1（powershell.exe）。托盘图标 / 菜单靠 UI Automation 找，pwsh 7（.NET Core）
加载不了 UIAutomationClient，那样托盘类场景（ime / tray-focus / pause-gate）会报 SKIP。文件带 UTF-8 BOM，
5.1 才能正确读中文。

前提（脚本会在开头逐项检查，不满足就中止，不会硬跑）：
  1. 桌面已解锁、会话在前台（锁屏时 SendInput / 前台判定都无效）。
  2. 本机没有别的 Cisox 实例在跑：宿主与 --resident 的 UI 共用互斥体 Local\cisox.snow_shot.single_instance，
     有旧的 debug snow-shot.exe 在跑就拿不到互斥体。脚本只报告、不替你结束它（不按名字杀进程）。
  3. 任务栏通知区里能直接看到 Cisox 图标（在“选择哪些图标显示在任务栏上”里把它设为显示，
     否则 tray-focus / ime / pause-gate 找不到图标；找不到时这些场景报 SKIP，不算失败）。
  4. 验收期间不要碰键盘鼠标、不要播放视频（首帧用屏幕采样点判定，桌面必须静止）；关闭屏保与锁屏；
     避开远程控制代理与 GPU 重负载程序。
  5. 数据隔离：脚本把宿主 / UI exe 复制到临时目录，写 __data_directory 标记让数据根落在该目录，
     不会读写 %LOCALAPPDATA%\SnowShot 或你正在用的 Cisox 数据目录。
  6. 机器信息会写进结果文件（CPU、显卡、驱动、刷新率）；不同 GPU / 机器的数据不能直接比较。

输出：build\split-accept\<时间戳>\results.json 与 host.log / ui 日志副本；控制台打印汇总表。
退出码：0 全部 PASS（INFO / SKIP 不算失败）；1 有 FAIL；2 前提不满足。

示例：
  scripts\verify-tray-host-split.ps1                       # 跑 idle / ime / hotkey / tray-focus / cmd / ...
  scripts\verify-tray-host-split.ps1 -Scenario hotkey -Rounds 5
  scripts\verify-tray-host-split.ps1 -Scenario admin       # 管理员宿主（会弹 UAC）
#>
[CmdletBinding()]
param(
    [ValidateSet('all', 'idle', 'ime', 'hotkey', 'tray-focus', 'cmd', 'settings-hotkey', 'pause-gate', 'crash', 'admin')]
    [string[]]$Scenario = @('all'),
    [string]$HostExe = '',
    [string]$UiExe = '',
    [int]$Rounds = 3,
    [string]$WorkDir = '',
    [switch]$KeepWork
)

$ErrorActionPreference = 'Stop'
$script:RepoRoot = [System.IO.Path]::GetFullPath((Join-Path $PSScriptRoot '..'))
if (-not $HostExe) { $HostExe = Join-Path $script:RepoRoot 'build\cargo\release\snow-shot.exe' }
if (-not $UiExe) { $UiExe = Join-Path (Split-Path -Parent $HostExe) 'snow-shot-ui.exe' }
if (-not $WorkDir) { $WorkDir = Join-Path $env:TEMP 'cisox-split-accept' }
$script:Stamp = Get-Date -Format 'yyyyMMdd-HHmmss'
$script:OutDir = Join-Path $script:RepoRoot "build\split-accept\$script:Stamp"
$script:Results = New-Object System.Collections.Generic.List[object]
$script:HostProc = $null
$script:Work = ''
$script:Data = ''

# 验收线（来自方案文档）
$MaxIdleCommitMB = 6.0
$MaxFirstFrameMs = 1140
$MaxUiExitMs = 3000
$HotkeyVk = 0x70          # 默认截图热键 F1
$VkEsc = 0x1B
$ProductPattern = 'Cisox'

# ---------------------------------------------------------------------------------------------
# Win32 辅助（SendInput / 前台判定 / 窗口枚举 / 屏幕采样）
# ---------------------------------------------------------------------------------------------
Add-Type -AssemblyName System.Windows.Forms, System.Drawing
# UI Automation 只在 Windows PowerShell 5.1（powershell.exe）下可靠；pwsh 7 加载不了时，托盘类场景报 SKIP
$script:HaveUia = $true
try { Add-Type -AssemblyName UIAutomationClient, UIAutomationTypes } catch { $script:HaveUia = $false }
if (-not ('CisoxAccept.W' -as [type])) {
    Add-Type -TypeDefinition @'
using System;
using System.Collections.Generic;
using System.Runtime.InteropServices;

namespace CisoxAccept {
    [StructLayout(LayoutKind.Sequential)] public struct RECT { public int Left, Top, Right, Bottom; }
    [StructLayout(LayoutKind.Sequential)] public struct GUITHREADINFO {
        public int cbSize; public int flags; public IntPtr hwndActive; public IntPtr hwndFocus;
        public IntPtr hwndCapture; public IntPtr hwndMenuOwner; public IntPtr hwndMoveSize;
        public IntPtr hwndCaret; public RECT rcCaret;
    }
    [StructLayout(LayoutKind.Sequential)] public struct KEYBDINPUT {
        public ushort wVk; public ushort wScan; public uint dwFlags; public uint time; public IntPtr dwExtraInfo;
    }
    [StructLayout(LayoutKind.Explicit, Size = 40)] public struct INPUT {
        [FieldOffset(0)] public uint type;
        [FieldOffset(8)] public KEYBDINPUT ki;
    }
    public delegate bool EnumProc(IntPtr h, IntPtr l);

    public static class W {
        [DllImport("user32.dll")] public static extern IntPtr GetForegroundWindow();
        [DllImport("user32.dll")] public static extern uint GetWindowThreadProcessId(IntPtr h, out uint pid);
        [DllImport("user32.dll")] public static extern bool GetGUIThreadInfo(uint tid, ref GUITHREADINFO info);
        [DllImport("user32.dll")] public static extern bool EnumWindows(EnumProc p, IntPtr l);
        [DllImport("user32.dll")] public static extern bool IsWindowVisible(IntPtr h);
        [DllImport("user32.dll")] public static extern bool GetWindowRect(IntPtr h, out RECT r);
        [DllImport("user32.dll")] public static extern IntPtr OpenInputDesktop(uint flags, bool inherit, uint access);
        [DllImport("user32.dll")] public static extern bool CloseDesktop(IntPtr h);
        [DllImport("user32.dll")] public static extern bool SetCursorPos(int x, int y);
        [DllImport("user32.dll")] public static extern void mouse_event(uint f, uint dx, uint dy, uint data, UIntPtr extra);
        [DllImport("user32.dll")] public static extern uint SendInput(uint n, INPUT[] inputs, int size);
        [DllImport("user32.dll")] public static extern IntPtr GetDC(IntPtr h);
        [DllImport("user32.dll")] public static extern int ReleaseDC(IntPtr h, IntPtr dc);
        [DllImport("gdi32.dll")] public static extern uint GetPixel(IntPtr dc, int x, int y);
        [DllImport("user32.dll")] public static extern bool RegisterHotKey(IntPtr h, int id, uint mod, uint vk);
        [DllImport("user32.dll")] public static extern bool UnregisterHotKey(IntPtr h, int id);
        [DllImport("user32.dll")] public static extern bool PostMessage(IntPtr h, uint msg, IntPtr w, IntPtr l);

        public static void Key(ushort vk, bool up) {
            INPUT[] i = new INPUT[1];
            i[0].type = 1; i[0].ki.wVk = vk; i[0].ki.dwFlags = up ? 2u : 0u;
            SendInput(1, i, Marshal.SizeOf(typeof(INPUT)));
        }

        public static void Tap(ushort vk) { Key(vk, false); Key(vk, true); }

        // 采样屏幕点的亮度（0..255），一次取一批，避免 PowerShell 逐点调用的开销
        public static int[] Sample(int[] xs, int[] ys) {
            IntPtr dc = GetDC(IntPtr.Zero);
            int[] r = new int[xs.Length];
            for (int i = 0; i < xs.Length; i++) {
                uint c = GetPixel(dc, xs[i], ys[i]);
                int R = (int)(c & 0xFF), G = (int)((c >> 8) & 0xFF), B = (int)((c >> 16) & 0xFF);
                r[i] = (R * 3 + G * 6 + B) / 10;
            }
            ReleaseDC(IntPtr.Zero, dc);
            return r;
        }

        // 指定进程的、可见的、宽度不小于 minWidth 的顶层窗口
        public static List<IntPtr> BigWindowsOf(uint pid, int minWidth) {
            List<IntPtr> found = new List<IntPtr>();
            EnumWindows(delegate (IntPtr h, IntPtr l) {
                uint p; GetWindowThreadProcessId(h, out p);
                if (p == pid && IsWindowVisible(h)) {
                    RECT r; GetWindowRect(h, out r);
                    if (r.Right - r.Left >= minWidth) found.Add(h);
                }
                return true;
            }, IntPtr.Zero);
            return found;
        }

        // 前台窗口：所属进程 ID 与它所在线程的键盘焦点窗口所属进程 ID
        public static uint[] ForegroundInfo() {
            IntPtr fg = GetForegroundWindow();
            if (fg == IntPtr.Zero) return new uint[] { 0, 0 };
            uint pid; uint tid = GetWindowThreadProcessId(fg, out pid);
            GUITHREADINFO gi = new GUITHREADINFO(); gi.cbSize = Marshal.SizeOf(typeof(GUITHREADINFO));
            uint focusPid = 0;
            if (GetGUIThreadInfo(tid, ref gi) && gi.hwndFocus != IntPtr.Zero) GetWindowThreadProcessId(gi.hwndFocus, out focusPid);
            return new uint[] { pid, focusPid };
        }

        public static void LeftClick(int x, int y) {
            SetCursorPos(x, y);
            mouse_event(0x0002, 0, 0, 0, UIntPtr.Zero); mouse_event(0x0004, 0, 0, 0, UIntPtr.Zero);
        }
        public static void RightClick(int x, int y) {
            SetCursorPos(x, y);
            mouse_event(0x0008, 0, 0, 0, UIntPtr.Zero); mouse_event(0x0010, 0, 0, 0, UIntPtr.Zero);
        }
    }
}
'@
}

# ---------------------------------------------------------------------------------------------
# 通用函数
# ---------------------------------------------------------------------------------------------
function Add-Result {
    param([string]$Scenario, [string]$Check, [ValidateSet('PASS', 'FAIL', 'INFO', 'SKIP')][string]$Status, [string]$Value = '', [string]$Note = '')
    $script:Results.Add([pscustomobject]@{ Scenario = $Scenario; Check = $Check; Status = $Status; Value = $Value; Note = $Note }) | Out-Null
    $color = switch ($Status) { 'PASS' { 'Green' } 'FAIL' { 'Red' } 'SKIP' { 'Yellow' } default { 'Gray' } }
    Write-Host ("[{0,-4}] {1}/{2}  {3}  {4}" -f $Status, $Scenario, $Check, $Value, $Note) -ForegroundColor $color
}

function Wait-Until {
    param([scriptblock]$Condition, [int]$TimeoutMs = 10000, [int]$PollMs = 50)
    $sw = [Diagnostics.Stopwatch]::StartNew()
    while ($sw.ElapsedMilliseconds -lt $TimeoutMs) {
        if (& $Condition) { return $true }
        Start-Sleep -Milliseconds $PollMs
    }
    return [bool](& $Condition)
}

function Get-Median([double[]]$Values) {
    if (-not $Values -or $Values.Count -eq 0) { return $null }
    $sorted = $Values | Sort-Object
    $n = $sorted.Count
    if ($n % 2 -eq 1) { return $sorted[($n - 1) / 2] }
    return ($sorted[$n / 2 - 1] + $sorted[$n / 2]) / 2
}

# 工作目录下的进程（按路径过滤，绝不按名字杀进程）
function Get-WorkProcesses {
    Get-Process -ErrorAction SilentlyContinue | Where-Object {
        try { $_.Path -and $_.Path.StartsWith($script:Work, [StringComparison]::OrdinalIgnoreCase) } catch { $false }
    }
}

function Get-UiProcess {
    Get-WorkProcesses | Where-Object { $_.ProcessName -eq 'snow-shot-ui' } | Select-Object -First 1
}

function Get-HostProcess {
    Get-WorkProcesses | Where-Object { $_.ProcessName -eq 'snow-shot' } | Select-Object -First 1
}

function Send-Hotkey([int]$Vk) { [CisoxAccept.W]::Tap([uint16]$Vk) }

function Stop-WorkProcessById([int]$ProcessId) {
    # 只结束路径在工作目录下的进程，且只按 PID
    $p = Get-Process -Id $ProcessId -ErrorAction SilentlyContinue
    if ($p -and $p.Path -and $p.Path.StartsWith($script:Work, [StringComparison]::OrdinalIgnoreCase)) {
        Stop-Process -Id $ProcessId -Force
    }
}

function Get-MemoryInfo([int]$ProcessId) {
    $p = Get-Process -Id $ProcessId -ErrorAction Stop
    $p.Refresh()
    [pscustomobject]@{
        CommitMB     = [math]::Round($p.PrivateMemorySize64 / 1MB, 2)   # 私有提交（任务管理器“提交大小”）
        WorkingSetMB = [math]::Round($p.WorkingSet64 / 1MB, 2)          # 工作集（EmptyWorkingSet 会压低它，不等于省内存）
        Threads      = $p.Threads.Count
    }
}

function Get-ModuleNames([int]$ProcessId) {
    try { (Get-Process -Id $ProcessId -ErrorAction Stop).Modules | ForEach-Object { $_.ModuleName } }
    catch { @() }
}

function Get-MachineInfo {
    $cpu = (Get-CimInstance Win32_Processor | Select-Object -First 1).Name
    $gpus = Get-CimInstance Win32_VideoController | ForEach-Object { "{0} (driver {1}, {2}Hz)" -f $_.Name, $_.DriverVersion, $_.CurrentRefreshRate }
    $os = (Get-CimInstance Win32_OperatingSystem)
    [pscustomobject]@{
        Computer = $env:COMPUTERNAME; Cpu = $cpu; Gpu = ($gpus -join '; ')
        Os = "$($os.Caption) $($os.Version)"
        Screens = ([Windows.Forms.Screen]::AllScreens | ForEach-Object { "{0} {1}x{2}{3}" -f $_.DeviceName, $_.Bounds.Width, $_.Bounds.Height, $(if ($_.Primary) { ' primary' } else { '' }) }) -join '; '
    }
}

# ---------------------------------------------------------------------------------------------
# 前提检查
# ---------------------------------------------------------------------------------------------
function Test-Prerequisites {
    $problems = @()
    foreach ($exe in @($HostExe, $UiExe)) {
        if (-not (Test-Path -LiteralPath $exe)) { $problems += "找不到 $exe（先 cargo build --release -p snow-tray-host -p snow-shot）" }
    }
    $desktop = [CisoxAccept.W]::OpenInputDesktop(0, $false, 0x0001)
    if ($desktop -eq [IntPtr]::Zero -or [CisoxAccept.W]::GetForegroundWindow() -eq [IntPtr]::Zero) {
        $problems += '桌面处于锁屏 / 无前台窗口状态：请先解锁，并保持会话在前台'
    }
    elseif ($desktop -ne [IntPtr]::Zero) { [void][CisoxAccept.W]::CloseDesktop($desktop) }
    $mutex = $null
    if ([System.Threading.Mutex]::TryOpenExisting('Local\cisox.snow_shot.single_instance', [ref]$mutex)) {
        $mutex.Dispose()
        $running = Get-Process -Name 'snow-shot', 'snow-shot-ui' -ErrorAction SilentlyContinue | ForEach-Object { "$($_.ProcessName)#$($_.Id)" }
        $problems += "已有 Cisox 实例持有单实例互斥体（$($running -join ', ')）：请先自行退出它，脚本不会替你结束"
    }
    if ($problems.Count -gt 0) {
        Write-Host '前提不满足，验收中止：' -ForegroundColor Red
        $problems | ForEach-Object { Write-Host "  - $_" -ForegroundColor Red }
        exit 2
    }
}

# ---------------------------------------------------------------------------------------------
# 宿主生命周期
# ---------------------------------------------------------------------------------------------
function Initialize-Work {
    if (Test-Path -LiteralPath $WorkDir) { Remove-Item -LiteralPath $WorkDir -Recurse -Force }
    New-Item -ItemType Directory -Force -Path $WorkDir | Out-Null
    $script:Work = (Resolve-Path -LiteralPath $WorkDir).Path
    $script:Data = Join-Path $script:Work 'data'
    Copy-Item -LiteralPath $HostExe -Destination (Join-Path $script:Work 'snow-shot.exe')
    Copy-Item -LiteralPath $UiExe -Destination (Join-Path $script:Work 'snow-shot-ui.exe')
    # 便携标记：让数据根落在工作目录，不碰真实数据目录
    [System.IO.File]::WriteAllText((Join-Path $script:Work '__data_directory'), $script:Data, (New-Object System.Text.UTF8Encoding($false)))
    New-Item -ItemType Directory -Force -Path $script:OutDir | Out-Null
}

function Start-TestHost {
    param([switch]$Admin, [hashtable]$Env = @{})
    $exe = Join-Path $script:Work 'snow-shot.exe'
    foreach ($k in $Env.Keys) { Set-Item -Path "Env:$k" -Value $Env[$k] }
    try {
        if ($Admin) { $script:HostProc = Start-Process -FilePath $exe -Verb RunAs -PassThru -WindowStyle Hidden }
        else { $script:HostProc = Start-Process -FilePath $exe -PassThru -WindowStyle Hidden }
    }
    finally { foreach ($k in $Env.Keys) { Remove-Item -Path "Env:$k" -ErrorAction SilentlyContinue } }
    # 冷启动没有快照：宿主先起内置最小托盘，再拉起 --headless emit-spec 生成快照
    $spec = Join-Path $script:Data 'host\spec.json'
    $ok = Wait-Until { Test-Path -LiteralPath $spec } -TimeoutMs 30000 -PollMs 100
    if (-not $ok) { throw "宿主启动后 30 秒内没有生成 host\spec.json（看 $($script:Data)\logs\host.log）" }
    Start-Sleep -Milliseconds 500
    return $script:HostProc
}

function Stop-TestHost {
    if ($script:HostProc) {
        # 优雅退出：--cmd quit 走控制管道，宿主让 UI 先走 shutdown_app 再自己退出
        & (Join-Path $script:Work 'snow-shot.exe') --cmd quit | Out-Null
        $gone = Wait-Until { -not (Get-WorkProcesses) } -TimeoutMs 15000
        if (-not $gone) {
            Get-WorkProcesses | ForEach-Object { Stop-WorkProcessById $_.Id }
        }
        $script:HostProc = $null
    }
}

function Save-Logs {
    $logs = Join-Path $script:Data 'logs'
    if (Test-Path -LiteralPath $logs) {
        Copy-Item -LiteralPath $logs -Destination (Join-Path $script:OutDir 'logs') -Recurse -Force -ErrorAction SilentlyContinue
    }
    foreach ($f in @('config.json', 'host\spec.json')) {
        $p = Join-Path $script:Data $f
        if (Test-Path -LiteralPath $p) { Copy-Item -LiteralPath $p -Destination (Join-Path $script:OutDir ($f -replace '[\\/]', '_')) -Force }
    }
}

function Read-HostLog {
    $p = Join-Path $script:Data 'logs\host.log'
    if (Test-Path -LiteralPath $p) { return [System.IO.File]::ReadAllText($p) }
    return ''
}

function Read-UiLogs {
    $dir = Join-Path $script:Data 'logs'
    if (-not (Test-Path -LiteralPath $dir)) { return '' }
    ($dir | Get-ChildItem -File | Where-Object { $_.Name -ne 'host.log' -and $_.Name -ne 'host.log.1' } | ForEach-Object { [System.IO.File]::ReadAllText($_.FullName) }) -join "`n"
}

# ---------------------------------------------------------------------------------------------
# 屏幕采样（首帧判定）、托盘 UIA
# ---------------------------------------------------------------------------------------------
function New-SamplePlan {
    $xs = New-Object System.Collections.Generic.List[int]
    $ys = New-Object System.Collections.Generic.List[int]
    $owner = New-Object System.Collections.Generic.List[int]
    $index = 0
    foreach ($s in [Windows.Forms.Screen]::AllScreens) {
        foreach ($ix in 1..8) {
            foreach ($iy in 1..5) {
                $xs.Add([int]($s.Bounds.X + $s.Bounds.Width * $ix / 9)); $ys.Add([int]($s.Bounds.Y + $s.Bounds.Height * $iy / 6)); $owner.Add($index)
            }
        }
        $index++
    }
    [pscustomobject]@{ Xs = $xs.ToArray(); Ys = $ys.ToArray(); Owner = $owner.ToArray(); Screens = $index }
}

# 每块屏上亮度变化的采样点占比都 >= Fraction 才算“所有屏首帧都出来了”
function Test-AllScreensChanged($Plan, [int[]]$Baseline, [int[]]$Now, [double]$Fraction = 0.3) {
    $changed = New-Object 'int[]' $Plan.Screens
    $total = New-Object 'int[]' $Plan.Screens
    for ($i = 0; $i -lt $Now.Length; $i++) {
        $o = $Plan.Owner[$i]; $total[$o]++
        if ([math]::Abs($Now[$i] - $Baseline[$i]) -ge 8) { $changed[$o]++ }
    }
    for ($s = 0; $s -lt $Plan.Screens; $s++) { if ($changed[$s] / [double]$total[$s] -lt $Fraction) { return $false } }
    return $true
}

function Find-TrayButton {
    if (-not $script:HaveUia) { return $null }
    $root = [Windows.Automation.AutomationElement]::RootElement
    $tray = $root.FindFirst([Windows.Automation.TreeScope]::Children,
        (New-Object Windows.Automation.PropertyCondition([Windows.Automation.AutomationElement]::ClassNameProperty, 'Shell_TrayWnd')))
    if (-not $tray) { return $null }
    $buttons = $tray.FindAll([Windows.Automation.TreeScope]::Descendants,
        (New-Object Windows.Automation.PropertyCondition([Windows.Automation.AutomationElement]::ControlTypeProperty, [Windows.Automation.ControlType]::Button)))
    foreach ($b in $buttons) { if ($b.Current.Name -match $ProductPattern) { return $b } }
    return $null
}

function Get-ElementCenter($Element) {
    $r = $Element.Current.BoundingRectangle
    [pscustomobject]@{ X = [int]($r.X + $r.Width / 2); Y = [int]($r.Y + $r.Height / 2) }
}

function Open-TrayMenu {
    $button = Find-TrayButton
    if (-not $button) { return $null }
    $c = Get-ElementCenter $button
    [CisoxAccept.W]::RightClick($c.X, $c.Y)
    $root = [Windows.Automation.AutomationElement]::RootElement
    $menu = $null
    $null = Wait-Until {
        $script:menuTmp = $root.FindFirst([Windows.Automation.TreeScope]::Children,
            (New-Object Windows.Automation.PropertyCondition([Windows.Automation.AutomationElement]::ClassNameProperty, '#32768')))
        [bool]$script:menuTmp
    } -TimeoutMs 3000
    $menu = $script:menuTmp
    return $menu
}

# 在已弹出的菜单里按文案（来自快照）点一项
function Invoke-MenuItem($Menu, [string]$Label) {
    $items = $Menu.FindAll([Windows.Automation.TreeScope]::Descendants,
        (New-Object Windows.Automation.PropertyCondition([Windows.Automation.AutomationElement]::ControlTypeProperty, [Windows.Automation.ControlType]::MenuItem)))
    foreach ($item in $items) {
        if ($item.Current.Name -like "*$Label*") {
            $pattern = $null
            if ($item.TryGetCurrentPattern([Windows.Automation.InvokePattern]::Pattern, [ref]$pattern)) { $pattern.Invoke() }
            else { $c = Get-ElementCenter $item; [CisoxAccept.W]::LeftClick($c.X, $c.Y) }
            return $true
        }
    }
    return $false
}

function Get-SpecMenuLabel([string]$ActionId) {
    $spec = Get-Content -LiteralPath (Join-Path $script:Data 'host\spec.json') -Raw -Encoding UTF8 | ConvertFrom-Json
    foreach ($item in $spec.tray.menu) { if ($item.kind -eq 'item' -and $item.action_id -eq $ActionId) { return $item.label } }
    return $null
}

function Close-ForegroundWindowOfUi {
    $ui = Get-UiProcess
    if (-not $ui) { return }
    foreach ($h in [CisoxAccept.W]::BigWindowsOf([uint32]$ui.Id, 1)) { [void][CisoxAccept.W]::PostMessage($h, 0x0010, [IntPtr]::Zero, [IntPtr]::Zero) }   # WM_CLOSE
}

# ---------------------------------------------------------------------------------------------
# 场景
# ---------------------------------------------------------------------------------------------
function Test-Idle {
    $null = Start-TestHost
    Start-Sleep -Seconds 8   # 启动后 5 秒修剪一次工作集，再留点余量
    $hostPid = (Get-HostProcess).Id
    $mem = Get-MemoryInfo $hostPid
    $procs = @(Get-WorkProcesses | ForEach-Object { "$($_.ProcessName)#$($_.Id)" })
    Add-Result 'idle' 'only-host-process' $(if ($procs.Count -eq 1 -and $procs[0] -like 'snow-shot#*') { 'PASS' } else { 'FAIL' }) ($procs -join ', ') '托盘态应只有宿主：没有 UI、没有任何 worker'
    Add-Result 'idle' 'host-private-commit' $(if ($mem.CommitMB -le $MaxIdleCommitMB) { 'PASS' } else { 'FAIL' }) "$($mem.CommitMB) MB" "验收线 <= $MaxIdleCommitMB MB（提交与工作集分开记）"
    Add-Result 'idle' 'host-working-set' 'INFO' "$($mem.WorkingSetMB) MB" "EmptyWorkingSet 后的工作集；线程 $($mem.Threads)"
    $modules = Get-ModuleNames $hostPid
    Add-Result 'idle' 'host-modules' 'INFO' "$($modules.Count) 个" ($modules -join ',')
    $bad = @($modules | Where-Object { $_ -match '\.ime$|^d3d|^dxgi|uiautomationcore|^dwrite|^windowscodecs' })
    Add-Result 'idle' 'host-no-graphics-or-ime-modules' $(if ($bad.Count -eq 0) { 'PASS' } else { 'FAIL' }) ($bad -join ',') ''
}

function Test-Ime {
    $null = Start-TestHost
    Start-Sleep -Seconds 6
    $menu = Open-TrayMenu
    if (-not $menu) { Add-Result 'ime' 'tray-menu' 'SKIP' '' '通知区里找不到 Cisox 图标：请把它设为在任务栏上显示后重试'; return }
    Start-Sleep -Milliseconds 1500
    $hostPid = (Get-HostProcess).Id
    $modules = Get-ModuleNames $hostPid
    $ime = @($modules | Where-Object { $_ -match '\.ime$|sogou|ichat|isgpet|picface|textinputframework' })
    Add-Result 'ime' 'no-ime-modules-after-menu' $(if ($ime.Count -eq 0) { 'PASS' } else { 'FAIL' }) ($ime -join ',') '弹出菜单后宿主不得被输入法 DLL 注入（ImmDisableIME(-1) + 扩展点禁用）'
    Add-Result 'ime' 'msctf-loaded' 'INFO' $(if ($modules -contains 'msctf.dll') { 'yes' } else { 'no' }) '阶段 0 观察到 msctf.dll 约 +0.2MB，可接受'
    $mem = Get-MemoryInfo $hostPid
    Add-Result 'ime' 'host-private-commit-after-menu' $(if ($mem.CommitMB -le $MaxIdleCommitMB) { 'PASS' } else { 'FAIL' }) "$($mem.CommitMB) MB" "验收线 <= $MaxIdleCommitMB MB"
    [CisoxAccept.W]::Tap([uint16]$VkEsc)
}

function Test-Hotkey {
    $null = Start-TestHost
    Start-Sleep -Seconds 6
    $plan = New-SamplePlan
    $firstFrames = New-Object System.Collections.Generic.List[double]
    $winFirsts = New-Object System.Collections.Generic.List[double]
    $exitMs = New-Object System.Collections.Generic.List[double]
    $focusFails = 0
    for ($round = 1; $round -le $Rounds; $round++) {
        if (Get-UiProcess) { throw '轮次开始时不应有 UI 进程' }
        Start-Sleep -Milliseconds 800
        $baseline = [CisoxAccept.W]::Sample($plan.Xs, $plan.Ys)
        $sw = [Diagnostics.Stopwatch]::StartNew()
        Send-Hotkey $HotkeyVk
        $firstFrame = $null; $winFirst = $null
        while ($sw.ElapsedMilliseconds -lt 8000 -and -not $firstFrame) {
            if (-not $winFirst) {
                $ui = Get-UiProcess
                if ($ui -and [CisoxAccept.W]::BigWindowsOf([uint32]$ui.Id, 800).Count -gt 0) { $winFirst = $sw.ElapsedMilliseconds }
            }
            $now = [CisoxAccept.W]::Sample($plan.Xs, $plan.Ys)
            if (Test-AllScreensChanged $plan $baseline $now) { $firstFrame = $sw.ElapsedMilliseconds }
        }
        if ($firstFrame) { $firstFrames.Add($firstFrame) }
        if ($winFirst) { $winFirsts.Add($winFirst) }
        # 焦点：前台窗口与键盘焦点都属于 UI 进程
        Start-Sleep -Milliseconds 600
        $ui = Get-UiProcess
        $fg = [CisoxAccept.W]::ForegroundInfo()
        if (-not $ui -or $fg[0] -ne $ui.Id -or $fg[1] -ne $ui.Id) { $focusFails++ }
        # Esc 能关掉覆盖窗 = 键盘焦点真的在它上面；随后 UI 3 秒内消失
        $esc = [Diagnostics.Stopwatch]::StartNew()
        [CisoxAccept.W]::Tap([uint16]$VkEsc)
        $gone = Wait-Until { -not (Get-UiProcess) } -TimeoutMs 8000
        if ($gone) { $exitMs.Add($esc.ElapsedMilliseconds) } else { Stop-WorkProcessById (Get-UiProcess).Id }
        Start-Sleep -Milliseconds 500
    }
    $medianFrame = Get-Median $firstFrames.ToArray()
    Add-Result 'hotkey' 'first-frame-cold-median' $(if ($medianFrame -and $medianFrame -le $MaxFirstFrameMs) { 'PASS' } else { 'FAIL' }) $(if ($medianFrame) { "$([math]::Round($medianFrame)) ms" } else { '未采到' }) "验收线 <= $MaxFirstFrameMs ms；各轮 $((($firstFrames | ForEach-Object { [math]::Round($_) }) -join '/'))；窗口首次可见中位 $([math]::Round((Get-Median $winFirsts.ToArray()))) ms"
    Add-Result 'hotkey' 'ui-focus-foreground-and-keyboard' $(if ($focusFails -eq 0) { 'PASS' } else { 'FAIL' }) "失败 $focusFails / $Rounds 轮" '热键路径：UI 的覆盖窗在前台且有键盘焦点'
    Add-Result 'hotkey' 'ui-exits-after-overlay-closed' $(if ($exitMs.Count -eq $Rounds -and ((Get-Median $exitMs.ToArray()) -le $MaxUiExitMs)) { 'PASS' } else { 'FAIL' }) "中位 $([math]::Round((Get-Median $exitMs.ToArray()))) ms" "验收线 <= $MaxUiExitMs ms（立即退出，不保活）；各轮 $((($exitMs | ForEach-Object { [math]::Round($_) }) -join '/'))"
    Add-Result 'hotkey' 'host-still-running' $(if (Get-HostProcess) { 'PASS' } else { 'FAIL' }) '' 'UI 退出后宿主常驻'
}

function Test-TrayFocus {
    $null = Start-TestHost
    Start-Sleep -Seconds 6
    $menu = Open-TrayMenu
    if (-not $menu) { Add-Result 'tray-focus' 'tray-menu' 'SKIP' '' '通知区里找不到 Cisox 图标'; return }
    $label = Get-SpecMenuLabel 'settings'
    if (-not $label) { Add-Result 'tray-focus' 'menu-label' 'SKIP' '' '快照里没有 settings 菜单项'; return }
    $clicked = Invoke-MenuItem $menu $label
    Add-Result 'tray-focus' 'menu-item-clicked' $(if ($clicked) { 'PASS' } else { 'FAIL' }) $label ''
    $opened = Wait-Until { $ui = Get-UiProcess; $ui -and [CisoxAccept.W]::BigWindowsOf([uint32]$ui.Id, 400).Count -gt 0 } -TimeoutMs 8000
    Add-Result 'tray-focus' 'ui-window-opened' $(if ($opened) { 'PASS' } else { 'FAIL' }) '' '托盘菜单项拉起 UI 并打开设置窗口'
    Start-Sleep -Milliseconds 800
    $ui = Get-UiProcess
    $fg = [CisoxAccept.W]::ForegroundInfo()
    $ok = $ui -and $fg[0] -eq $ui.Id -and $fg[1] -eq $ui.Id
    Add-Result 'tray-focus' 'foreground-and-keyboard-focus' $(if ($ok) { 'PASS' } else { 'FAIL' }) "前台 pid=$($fg[0]) 焦点 pid=$($fg[1]) ui=$(if ($ui) { $ui.Id })" '托盘菜单点击路径（§12.3 未测的一项）'
    Close-ForegroundWindowOfUi
    $gone = Wait-Until { -not (Get-UiProcess) } -TimeoutMs 8000
    Add-Result 'tray-focus' 'ui-exits-after-window-closed' $(if ($gone) { 'PASS' } else { 'FAIL' }) '' '所有窗口关闭 -> 请求退出 -> 立即退出'
}

function Test-Cmd {
    $null = Start-TestHost
    Start-Sleep -Seconds 4
    $exe = Join-Path $script:Work 'snow-shot.exe'
    foreach ($name in @('show', 'settings', 'screenshot', 'recording', 'scroll-capture', 'pin-clipboard')) {
        $proc = Start-Process -FilePath $exe -ArgumentList @('--cmd', $name) -Wait -PassThru -WindowStyle Hidden
        Add-Result 'cmd' "$name-exit-code" $(if ($proc.ExitCode -eq 0) { 'PASS' } else { 'FAIL' }) "exit=$($proc.ExitCode)" ''
        $appeared = Wait-Until { [bool](Get-UiProcess) } -TimeoutMs 8000
        if ($name -ne 'pin-clipboard') { Add-Result 'cmd' "$name-ui-spawned" $(if ($appeared) { 'PASS' } else { 'FAIL' }) '' '' }
        Start-Sleep -Milliseconds 1200
        $ui = Get-UiProcess
        if ($ui -and $name -ne 'pin-clipboard') {
            $fg = [CisoxAccept.W]::ForegroundInfo()
            Add-Result 'cmd' "$name-foreground" $(if ($fg[0] -eq $ui.Id) { 'PASS' } else { 'FAIL' }) "前台 pid=$($fg[0])" '--cmd 路径：从属实例先授权宿主，宿主再授权 UI'
        }
        # 覆盖窗类用 Esc 关，窗口类用 WM_CLOSE 关
        [CisoxAccept.W]::Tap([uint16]$VkEsc)
        Close-ForegroundWindowOfUi
        $gone = Wait-Until { -not (Get-UiProcess) } -TimeoutMs 8000
        Add-Result 'cmd' "$name-ui-exits" $(if ($gone) { 'PASS' } else { 'FAIL' }) '' ''
        if (-not $gone) { $ui = Get-UiProcess; if ($ui) { Stop-WorkProcessById $ui.Id } }
        Start-Sleep -Milliseconds 400
    }
    $quit = Start-Process -FilePath $exe -ArgumentList @('--cmd', 'quit') -Wait -PassThru -WindowStyle Hidden
    $allGone = Wait-Until { -not (Get-WorkProcesses) } -TimeoutMs 15000
    Add-Result 'cmd' 'quit' $(if ($quit.ExitCode -eq 0 -and $allGone) { 'PASS' } else { 'FAIL' }) "exit=$($quit.ExitCode)" '宿主与 UI 都应退出'
    $script:HostProc = $null
}

function Test-SettingsHotkey {
    $conflictKey = 0x7B    # F12：本脚本进程先占住 Ctrl+Alt+F12，制造“被其它程序占用”
    $mod = 0x0001 -bor 0x0002
    $registered = [CisoxAccept.W]::RegisterHotKey([IntPtr]::Zero, 0x7001, [uint32]$mod, [uint32]$conflictKey)
    if (-not $registered) { Add-Result 'settings-hotkey' 'setup' 'SKIP' '' '本脚本占不到 Ctrl+Alt+F12（已被别的程序占用），换一个键后重试'; return }
    try {
        # 设置窗口打开后由自动化脚本改热键：先改成冲突的（应回滚），再改成可用的
        $ops = '[{"op":"shortcut","key":"global_shortcuts/screenshot","text":"Ctrl+Alt+F12"},{"op":"shortcut","key":"global_shortcuts/screenshot","text":"Ctrl+Alt+F11"}]'
        $null = Start-TestHost -Env @{ SNOW_SETTINGS_AUTOTEST = $ops }
        Start-Sleep -Seconds 4
        & (Join-Path $script:Work 'snow-shot.exe') --cmd settings | Out-Null
        $null = Wait-Until { (Read-UiLogs) -match '热键重新注册失败|回滚配置' } -TimeoutMs 20000
        Start-Sleep -Seconds 3
        $uiLog = Read-UiLogs
        Add-Result 'settings-hotkey' 'conflict-rolled-back' $(if ($uiLog -match '全局热键重新注册失败，回滚配置') { 'PASS' } else { 'FAIL' }) '' '冲突的新热键注册失败 -> 配置回滚 -> 设置页提示'
        $hostLog = Read-HostLog
        Add-Result 'settings-hotkey' 'host-reported-failure' $(if ($hostLog -match '已被系统或其他程序占用|热键冲突') { 'PASS' } else { 'INFO' }) '' '宿主日志里应有注册失败的原因'
        Close-ForegroundWindowOfUi
        $null = Wait-Until { -not (Get-UiProcess) } -TimeoutMs 8000
        $config = Get-Content -LiteralPath (Join-Path $script:Data 'config.json') -Raw -Encoding UTF8
        Add-Result 'settings-hotkey' 'config-ends-with-new-valid-hotkey' $(if ($config -match 'Ctrl\+Alt\+F11') { 'PASS' } else { 'FAIL' }) '' '第二次（可用的）改动应被接受并落盘'
        # F1 不再生效，Ctrl+Alt+F11 生效
        Send-Hotkey $HotkeyVk
        Start-Sleep -Seconds 2
        Add-Result 'settings-hotkey' 'old-hotkey-no-longer-fires' $(if (-not (Get-UiProcess)) { 'PASS' } else { 'FAIL' }) '' ''
        [CisoxAccept.W]::Key(0x11, $false); [CisoxAccept.W]::Key(0x12, $false); [CisoxAccept.W]::Tap(0x7A); [CisoxAccept.W]::Key(0x12, $true); [CisoxAccept.W]::Key(0x11, $true)
        $fired = Wait-Until { [bool](Get-UiProcess) } -TimeoutMs 8000
        Add-Result 'settings-hotkey' 'new-hotkey-fires' $(if ($fired) { 'PASS' } else { 'FAIL' }) '' ''
        [CisoxAccept.W]::Tap([uint16]$VkEsc)
    }
    finally { [void][CisoxAccept.W]::UnregisterHotKey([IntPtr]::Zero, 0x7001) }
}

function Test-PauseGate {
    $null = Start-TestHost
    Start-Sleep -Seconds 6
    $menu = Open-TrayMenu
    if (-not $menu) { Add-Result 'pause-gate' 'tray-menu' 'SKIP' '' '通知区里找不到 Cisox 图标'; return }
    $pauseLabel = Get-SpecMenuLabel 'quick.toggle_global_hotkeys'
    $gateLabel = Get-SpecMenuLabel 'quick.toggle_disable_on_focused_fullscreen'
    if (-not $pauseLabel) { Add-Result 'pause-gate' 'menu-label' 'SKIP' '' '快照里没有暂停热键菜单项'; return }
    [void](Invoke-MenuItem $menu $pauseLabel)
    Start-Sleep -Milliseconds 800
    Send-Hotkey $HotkeyVk
    Start-Sleep -Seconds 2
    Add-Result 'pause-gate' 'paused-hotkey-ignored' $(if (-not (Get-UiProcess)) { 'PASS' } else { 'FAIL' }) '' '暂停后 F1 不应拉起 UI'
    $menu = Open-TrayMenu
    if ($menu) { [void](Invoke-MenuItem $menu $pauseLabel) }
    Start-Sleep -Milliseconds 800
    Send-Hotkey $HotkeyVk
    $fired = Wait-Until { [bool](Get-UiProcess) } -TimeoutMs 8000
    Add-Result 'pause-gate' 'resumed-hotkey-fires' $(if ($fired) { 'PASS' } else { 'FAIL' }) '' ''
    [CisoxAccept.W]::Tap([uint16]$VkEsc)
    $null = Wait-Until { -not (Get-UiProcess) } -TimeoutMs 8000
    if (-not $gateLabel) { Add-Result 'pause-gate' 'gate-menu-label' 'SKIP' '' '快照里没有前台全屏闸门菜单项'; return }
    $menu = Open-TrayMenu
    if ($menu) { [void](Invoke-MenuItem $menu $gateLabel) }
    # 闸门开关由宿主经 set_config 让 UI 落盘：UI 会被拉起、写完配置再退出
    $null = Wait-Until { (Get-Content -LiteralPath (Join-Path $script:Data 'config.json') -Raw -Encoding UTF8) -match 'disable_on_focused_fullscreen_window"\s*:\s*true' } -TimeoutMs 15000
    $persisted = (Get-Content -LiteralPath (Join-Path $script:Data 'config.json') -Raw -Encoding UTF8) -match 'disable_on_focused_fullscreen_window"\s*:\s*true'
    Add-Result 'pause-gate' 'gate-persisted-via-ui' $(if ($persisted) { 'PASS' } else { 'FAIL' }) '' '宿主切换闸门 -> set_config -> UI 写 config.json'
    $null = Wait-Until { -not (Get-UiProcess) } -TimeoutMs 8000
    # 用一个全屏置顶窗口模拟全屏应用
    $form = New-Object Windows.Forms.Form
    $form.FormBorderStyle = 'None'; $form.WindowState = 'Maximized'; $form.TopMost = $true; $form.BackColor = 'Black'
    $form.Show(); $form.Activate()
    Start-Sleep -Seconds 1
    Send-Hotkey $HotkeyVk
    Start-Sleep -Seconds 2
    Add-Result 'pause-gate' 'fullscreen-hotkey-dropped' $(if (-not (Get-UiProcess)) { 'PASS' } else { 'FAIL' }) '' '前台全屏 + 闸门打开：F1 被丢弃'
    $form.Close(); $form.Dispose()
    Start-Sleep -Seconds 1
    Send-Hotkey $HotkeyVk
    $fired = Wait-Until { [bool](Get-UiProcess) } -TimeoutMs 8000
    Add-Result 'pause-gate' 'hotkey-works-after-fullscreen-closed' $(if ($fired) { 'PASS' } else { 'FAIL' }) '' ''
    [CisoxAccept.W]::Tap([uint16]$VkEsc)
}

function Test-Crash {
    $null = Start-TestHost
    Start-Sleep -Seconds 5
    $killed = 0
    for ($i = 1; $i -le 3; $i++) {
        Send-Hotkey $HotkeyVk
        $up = Wait-Until { [bool](Get-UiProcess) } -TimeoutMs 8000
        if (-not $up) { break }
        Start-Sleep -Milliseconds 1500
        Stop-WorkProcessById (Get-UiProcess).Id
        $killed++
        $null = Wait-Until { -not (Get-UiProcess) } -TimeoutMs 5000
        Add-Result 'crash' "host-alive-after-kill-$i" $(if (Get-HostProcess) { 'PASS' } else { 'FAIL' }) '' '只按 PID 结束路径在工作目录下的 UI 进程'
        if ($i -eq 1) {
            Start-Sleep -Milliseconds 500
            Send-Hotkey $HotkeyVk
            $again = Wait-Until { [bool](Get-UiProcess) } -TimeoutMs 8000
            Add-Result 'crash' 'respawn-after-kill' $(if ($again) { 'PASS' } else { 'FAIL' }) '' '被杀后下一次热键能重新拉起（不重放未确认命令）'
            if ($again) { Start-Sleep -Milliseconds 1500; Stop-WorkProcessById (Get-UiProcess).Id; $null = Wait-Until { -not (Get-UiProcess) } -TimeoutMs 5000; $i++ }
        }
    }
    Start-Sleep -Seconds 1
    Send-Hotkey $HotkeyVk
    Start-Sleep -Seconds 2
    $hostLog = Read-HostLog
    Add-Result 'crash' 'breaker-engaged' $(if ($hostLog -match '熔断') { 'PASS' } else { 'INFO' }) '' '60 秒内异常退出 3 次进入 5 分钟退避（日志里应有“熔断”）'
}

function Test-Admin {
    $null = Start-TestHost -Admin
    Start-Sleep -Seconds 6
    $hostProc = Get-HostProcess
    Add-Result 'admin' 'elevated-host-running' $(if ($hostProc) { 'PASS' } else { 'FAIL' }) $(if ($hostProc) { "pid=$($hostProc.Id)" }) 'UAC 已确认'
    Send-Hotkey $HotkeyVk
    $up = Wait-Until { [bool](Get-UiProcess) } -TimeoutMs 10000
    Add-Result 'admin' 'hotkey-spawns-elevated-ui' $(if ($up) { 'PASS' } else { 'FAIL' }) '' '热键 -> 管理员宿主 -> 管理员 UI（会话管道、令牌握手正常）'
    Start-Sleep -Milliseconds 1000
    $ui = Get-UiProcess
    $fg = [CisoxAccept.W]::ForegroundInfo()
    Add-Result 'admin' 'ui-foreground-focus' $(if ($ui -and $fg[0] -eq $ui.Id -and $fg[1] -eq $ui.Id) { 'PASS' } else { 'FAIL' }) "前台 pid=$($fg[0]) 焦点 pid=$($fg[1])" ''
    [CisoxAccept.W]::Tap([uint16]$VkEsc)
    $null = Wait-Until { -not (Get-UiProcess) } -TimeoutMs 8000
    # 混合权限：本脚本（非管理员）发 --cmd 给管理员宿主，控制管道 DACL 只授当前用户（同 SID，UIPI 不挡）
    $proc = Start-Process -FilePath (Join-Path $script:Work 'snow-shot.exe') -ArgumentList @('--cmd', 'settings') -Wait -PassThru -WindowStyle Hidden
    Add-Result 'admin' 'non-elevated-cmd-to-elevated-host' $(if ($proc.ExitCode -eq 0) { 'PASS' } else { 'FAIL' }) "exit=$($proc.ExitCode)" '混合权限：--cmd 来自非管理员 shell'
    $shown = Wait-Until { [bool](Get-UiProcess) } -TimeoutMs 10000
    Add-Result 'admin' 'cmd-spawns-ui' $(if ($shown) { 'PASS' } else { 'FAIL' }) '' ''
    Start-Sleep -Milliseconds 1000
    $ui = Get-UiProcess
    if ($ui) {
        $fg = [CisoxAccept.W]::ForegroundInfo()
        Add-Result 'admin' 'cmd-ui-foreground' $(if ($fg[0] -eq $ui.Id) { 'PASS' } else { 'FAIL' }) "前台 pid=$($fg[0])" '非管理员 CLI 授权管理员宿主，宿主再授权 UI'
    }
    Close-ForegroundWindowOfUi
    $null = Wait-Until { -not (Get-UiProcess) } -TimeoutMs 8000
}

# ---------------------------------------------------------------------------------------------
# 主流程
# ---------------------------------------------------------------------------------------------
Test-Prerequisites
$machine = Get-MachineInfo
Write-Host ('机器: {0} | {1} | {2} | {3} | 屏幕: {4}' -f $machine.Computer, $machine.Cpu, $machine.Gpu, $machine.Os, $machine.Screens)

$all = @('idle', 'ime', 'hotkey', 'tray-focus', 'cmd', 'settings-hotkey', 'pause-gate', 'crash')
$selected = if ($Scenario -contains 'all') { $all } else { $Scenario | Where-Object { $_ -ne 'all' } }
$scenarioMap = @{
    'idle' = { Test-Idle }; 'ime' = { Test-Ime }; 'hotkey' = { Test-Hotkey }; 'tray-focus' = { Test-TrayFocus }
    'cmd' = { Test-Cmd }; 'settings-hotkey' = { Test-SettingsHotkey }; 'pause-gate' = { Test-PauseGate }
    'crash' = { Test-Crash }; 'admin' = { Test-Admin }
}

Initialize-Work
try {
    foreach ($name in $selected) {
        Write-Host "==== 场景: $name ====" -ForegroundColor Cyan
        try { & $scenarioMap[$name] }
        catch { Add-Result $name 'scenario-error' 'FAIL' '' $_.Exception.Message }
        finally {
            Save-Logs
            Stop-TestHost
            # 场景之间清理：只按路径过滤、只按 PID 结束，数据目录保留（后一个场景沿用快照）
            Get-WorkProcesses | ForEach-Object { Stop-WorkProcessById $_.Id }
        }
    }
}
finally {
    Save-Logs
    $summary = [pscustomobject]@{ Machine = $machine; HostExe = $HostExe; UiExe = $UiExe; Results = $script:Results }
    $summary | ConvertTo-Json -Depth 6 | Set-Content -LiteralPath (Join-Path $script:OutDir 'results.json') -Encoding UTF8
    if (-not $KeepWork -and $script:Work) { Remove-Item -LiteralPath $script:Work -Recurse -Force -ErrorAction SilentlyContinue }
}

Write-Host ''
$script:Results | Format-Table Scenario, Check, Status, Value -AutoSize
$failed = @($script:Results | Where-Object { $_.Status -eq 'FAIL' })
Write-Host ("汇总: PASS {0} / FAIL {1} / INFO {2} / SKIP {3}；结果与日志: {4}" -f @($script:Results | Where-Object Status -eq 'PASS').Count, $failed.Count, @($script:Results | Where-Object Status -eq 'INFO').Count, @($script:Results | Where-Object Status -eq 'SKIP').Count, $script:OutDir)
if ($failed.Count -gt 0) { exit 1 }
exit 0
