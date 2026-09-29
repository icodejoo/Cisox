using System;
using System.Diagnostics;
using System.Runtime.InteropServices;
using System.Collections.Generic;

public class Program {
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
    
    [DllImport("user32.dll", CharSet = CharSet.Auto)]
    public static extern int GetWindowText(IntPtr hWnd, System.Text.StringBuilder lpString, int nMaxCount);
    
    [StructLayout(LayoutKind.Sequential)]
    public struct RECT {
        public int Left;
        public int Top;
        public int Right;
        public int Bottom;
    }

    public static void Main() {
        var processes = Process.GetProcessesByName("p0-v1-overlay-window");
        if (processes.Length == 0) {
            Console.WriteLine("Process not found.");
            return;
        }
        var pids = new System.Collections.Generic.HashSet<int>();
        foreach (var p in processes) { pids.Add(p.Id); }

        EnumWindows((hWnd, lParam) => {
            uint processId;
            GetWindowThreadProcessId(hWnd, out processId);
            if (pids.Contains((int)processId)) {
                int exStyle = GetWindowLong(hWnd, -20);
                RECT rect;
                GetWindowRect(hWnd, out rect);
                
                var sb = new System.Text.StringBuilder(256);
                GetWindowText(hWnd, sb, sb.Capacity);
                
                bool isLayered = (exStyle & 0x00080000) == 0x00080000;
                bool isTransparent = (exStyle & 0x00000020) == 0x00000020;
                
                Console.WriteLine(string.Format("HWND: {0}", hWnd));
                Console.WriteLine(string.Format("Title: {0}", sb.ToString()));
                Console.WriteLine(string.Format("Layered: {0}", isLayered));
                Console.WriteLine(string.Format("Transparent (click-through style): {0}", isTransparent));
                Console.WriteLine(string.Format("EXSTYLE: 0x{0:X}", exStyle));
                Console.WriteLine(string.Format("Rect: {0}, {1}, {2}, {3}", rect.Left, rect.Top, rect.Right, rect.Bottom));
                Console.WriteLine(string.Format("Width: {0}, Height: {1}", rect.Right - rect.Left, rect.Bottom - rect.Top));
                Console.WriteLine("---");
            }
            return true;
        }, IntPtr.Zero);
    }
}
