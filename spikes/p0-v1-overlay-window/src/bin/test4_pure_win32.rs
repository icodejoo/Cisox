// 独立测试4：完全不用 GPUI，纯 Win32 API 建一个全屏透明置顶窗口，
// 用同样的 WS_EX_TRANSPARENT|WS_EX_TOPMOST 配方测点击穿透。
// 目的：隔离出"点不透"到底是 GPUI 的问题，还是这台机器/这个 Windows 版本的通用现象。
use windows::core::*;
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::WindowsAndMessaging::*;

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    match msg {
        WM_DESTROY => {
            PostQuitMessage(0);
            LRESULT(0)
        }
        WM_PAINT => {
            let mut ps = PAINTSTRUCT::default();
            let hdc = BeginPaint(hwnd, &mut ps);
            let brush = CreateSolidBrush(COLORREF(0x00880000)); // BGR: 深蓝
            let mut rect = RECT { left: 100, top: 100, right: 500, bottom: 400 };
            FillRect(hdc, &mut rect, brush);
            let _ = DeleteObject(brush);
            let _ = EndPaint(hwnd, &ps);
            LRESULT(0)
        }
        _ => DefWindowProcW(hwnd, msg, wparam, lparam),
    }
}

fn main() {
    unsafe {
        let hinstance = GetModuleHandleW(None).unwrap();
        let class_name = w!("PureWin32TransparentTest");

        let wc = WNDCLASSW {
            lpfnWndProc: Some(wndproc),
            hInstance: hinstance.into(),
            lpszClassName: class_name,
            hbrBackground: HBRUSH(std::ptr::null_mut()),
            ..Default::default()
        };
        RegisterClassW(&wc);

        let hinst: HINSTANCE = hinstance.into();
        let hwnd = CreateWindowExW(
            WS_EX_TOOLWINDOW | WS_EX_TOPMOST | WS_EX_LAYERED,
            class_name,
            w!("Pure Win32 Test"),
            WINDOW_STYLE(0x0),
            2560,
            0,
            2560,
            1440,
            None,
            None,
            hinst,
            None,
        )
        .unwrap();

        // 半透明整窗（用经典 LWA_ALPHA，跟 GPUI 那套 DirectComposition 不同的老机制，作对照）
        let _ = SetLayeredWindowAttributes(hwnd, COLORREF(0), 180, LWA_ALPHA);

        // 加上 WS_EX_TRANSPARENT 做整窗点击穿透
        let current = GetWindowLongPtrW(hwnd, GWL_EXSTYLE);
        let new_style = current | (WS_EX_TRANSPARENT.0 as isize);
        let result = SetWindowLongPtrW(hwnd, GWL_EXSTYLE, new_style);
        eprintln!(
            "[test4] hwnd={:?} old_exstyle={:#x} new_exstyle={:#x} set_result={:#x}",
            hwnd.0, current, new_style, result
        );

        let _ = ShowWindow(hwnd, SW_SHOW);
        let _ = UpdateWindow(hwnd);

        println!("Pure Win32 window opened. hwnd={:?}", hwnd.0);

        let mut msg = MSG::default();
        while GetMessageW(&mut msg, None, 0, 0).into() {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
}
