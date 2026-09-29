# 自动化点击穿透验证：不依赖人眼，用 SendInput 模拟真实点击，
# 检查点击后前台窗口是否变成了被覆盖窗盖住的目标窗口(notepad)。
Add-Type @"
using System;
using System.Runtime.InteropServices;
public static class Native {
    [DllImport("user32.dll")] public static extern IntPtr GetForegroundWindow();
    [DllImport("user32.dll")] public static extern uint GetWindowThreadProcessId(IntPtr hWnd, out uint lpdwProcessId);
    [DllImport("user32.dll")] public static extern bool SetCursorPos(int x, int y);
    [DllImport("user32.dll")] public static extern bool MoveWindow(IntPtr hWnd, int X, int Y, int nWidth, int nHeight, bool bRepaint);
    [DllImport("user32.dll")] public static extern int GetWindowTextLength(IntPtr hWnd);
    [DllImport("user32.dll")] public static extern int GetWindowText(IntPtr hWnd, System.Text.StringBuilder lpString, int nMaxCount);

    [StructLayout(LayoutKind.Sequential)]
    public struct INPUT { public uint type; public MOUSEINPUT mi; }
    [StructLayout(LayoutKind.Sequential)]
    public struct MOUSEINPUT {
        public int dx; public int dy; public uint mouseData; public uint dwFlags; public uint time; public IntPtr dwExtraInfo;
    }
    public const uint INPUT_MOUSE = 0;
    public const uint MOUSEEVENTF_LEFTDOWN = 0x0002;
    public const uint MOUSEEVENTF_LEFTUP = 0x0004;

    [DllImport("user32.dll")] public static extern uint SendInput(uint nInputs, INPUT[] pInputs, int cbSize);
    [DllImport("user32.dll", EntryPoint="GetWindowLongPtrW")] public static extern IntPtr GetWindowLongPtrW(IntPtr hWnd, int nIndex);
    public const int GWL_EXSTYLE = -20;
    public const uint WS_EX_TRANSPARENT = 0x20;
}
"@

$targetX = $env:CLICK_X
$targetY = $env:CLICK_Y
if (-not $targetX) { $targetX = 2700 }
if (-not $targetY) { $targetY = 200 }

$overlayHwnd = $env:OVERLAY_HWND
if ($overlayHwnd) {
    $exstyleNow = [Native]::GetWindowLongPtrW([IntPtr][int64]$overlayHwnd, [Native]::GWL_EXSTYLE)
    $hasTransparent = ([int64]$exstyleNow -band [Native]::WS_EX_TRANSPARENT) -ne 0
    Write-Output ("EXSTYLE_AT_CLICK_TIME={0:x} WS_EX_TRANSPARENT_bit_set={1}" -f [int64]$exstyleNow, $hasTransparent)
}

[Native]::SetCursorPos([int]$targetX, [int]$targetY) | Out-Null
Start-Sleep -Milliseconds 200

$down = New-Object Native+INPUT
$down.type = [Native]::INPUT_MOUSE
$down.mi = New-Object Native+MOUSEINPUT
$down.mi.dwFlags = [Native]::MOUSEEVENTF_LEFTDOWN

$up = New-Object Native+INPUT
$up.type = [Native]::INPUT_MOUSE
$up.mi = New-Object Native+MOUSEINPUT
$up.mi.dwFlags = [Native]::MOUSEEVENTF_LEFTUP

$inputs = [Native+INPUT[]]@($down, $up)
$size = [System.Runtime.InteropServices.Marshal]::SizeOf($down)
[Native]::SendInput(2, $inputs, $size)

Start-Sleep -Milliseconds 300

$fg = [Native]::GetForegroundWindow()
$sb = New-Object System.Text.StringBuilder 256
[Native]::GetWindowText($fg, $sb, 256) | Out-Null
$procId = 0
[Native]::GetWindowThreadProcessId($fg, [ref]$procId) | Out-Null
$proc = Get-Process -Id $procId -ErrorAction SilentlyContinue

Write-Output "CLICK_AT=($targetX,$targetY)"
Write-Output "FOREGROUND_HWND=$fg"
Write-Output "FOREGROUND_TITLE=$($sb.ToString())"
Write-Output "FOREGROUND_PROCESS=$($proc.ProcessName)"
