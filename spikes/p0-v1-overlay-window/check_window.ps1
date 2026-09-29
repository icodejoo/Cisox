Add-Type @"
using System;
using System.Runtime.InteropServices;
public class Win32Helper {
    [DllImport("user32.dll")]
    public static extern bool EnumWindows(EnumWindowsProc enumProc, IntPtr lParam);
    public delegate bool EnumWindowsProc(IntPtr hWnd, IntPtr lParam);
    [DllImport("user32.dll", SetLastError = true)]
    public static extern uint GetWindowThreadProcessId(IntPtr hWnd, out uint lpdwProcessId);
    [DllImport("user32.dll")]
    public static extern int GetWindowLong(IntPtr hWnd, int nIndex);
    [DllImport("user32.dll")]
    [return: MarshalAs(UnmanagedType.Bool)]
    public static extern bool GetWindowRect(IntPtr hWnd, out RECT lpRect);
    [StructLayout(LayoutKind.Sequential)]
    public struct RECT {
        public int Left;
        public int Top;
        public int Right;
        public int Bottom;
    }
}
"@

$process = Get-Process p0-v1-overlay-window -ErrorAction SilentlyContinue
if ($process) {
    $pidToFind = $process.Id
    [Win32Helper]::EnumWindows({
        param([IntPtr]$hWnd, [IntPtr]$lParam)
        $processId = 0
        [Win32Helper]::GetWindowThreadProcessId($hWnd, [ref]$processId)
        if ($processId -eq $pidToFind) {
            $gwl_exstyle = -20
            $exStyle = [Win32Helper]::GetWindowLong($hWnd, $gwl_exstyle)
            $WS_EX_LAYERED = 0x00080000
            $WS_EX_TRANSPARENT = 0x00000020
            $isLayered = ($exStyle -band $WS_EX_LAYERED) -eq $WS_EX_LAYERED
            $isTransparent = ($exStyle -band $WS_EX_TRANSPARENT) -eq $WS_EX_TRANSPARENT
            
            $rect = New-Object Win32Helper+RECT
            [Win32Helper]::GetWindowRect($hWnd, [ref]$rect) | Out-Null
            
            Write-Host "HWND: $hWnd"
            Write-Host "Layered: $isLayered"
            Write-Host "Transparent (click-through style): $isTransparent"
            Write-Host "EXSTYLE: 0x$($exStyle.ToString('X'))"
            Write-Host "Rect: $($rect.Left), $($rect.Top), $($rect.Right), $($rect.Bottom)"
            Write-Host "Width: $($rect.Right - $rect.Left), Height: $($rect.Bottom - $rect.Top)"
            Write-Host "---"
        }
        return $true
    }, [IntPtr]::Zero)
} else {
    Write-Host "Process not found."
}
