// 接收端：一个普通窗口，收到 WM_LBUTTONDOWN 就往 receiver_hits.log 追加一行。
// 用来直接验证"点击消息是否物理送达"，不依赖 GetForegroundWindow 这种
// 容易被 TOPMOST/激活策略干扰的间接信号。
use std::io::Write;
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
        WM_LBUTTONDOWN => {
            let x = (lparam.0 as i16) as i32;
            let y = ((lparam.0 >> 16) as i16) as i32;
            if let Ok(mut f) = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open("receiver_hits.log")
            {
                let _ = writeln!(f, "RECEIVER got WM_LBUTTONDOWN at client ({}, {})", x, y);
            }
            LRESULT(0)
        }
        WM_PAINT => {
            let mut ps = PAINTSTRUCT::default();
            let hdc = BeginPaint(hwnd, &mut ps);
            let brush = CreateSolidBrush(COLORREF(0x0000FF00)); // 绿色，肉眼可辨识
            let mut rect = RECT::default();
            let _ = GetClientRect(hwnd, &mut rect);
            FillRect(hdc, &mut rect, brush);
            let _ = DeleteObject(brush);
            let _ = EndPaint(hwnd, &ps);
            LRESULT(0)
        }
        _ => DefWindowProcW(hwnd, msg, wparam, lparam),
    }
}

fn main() {
    let _ = std::fs::remove_file("receiver_hits.log");
    unsafe {
        let hinstance = GetModuleHandleW(None).unwrap();
        let class_name = w!("ReceiverWindow");
        let wc = WNDCLASSW {
            lpfnWndProc: Some(wndproc),
            hInstance: hinstance.into(),
            lpszClassName: class_name,
            ..Default::default()
        };
        RegisterClassW(&wc);
        let hinst: HINSTANCE = hinstance.into();
        let hwnd = CreateWindowExW(
            WINDOW_EX_STYLE(0),
            class_name,
            w!("Receiver Window"),
            WS_OVERLAPPEDWINDOW,
            2700,
            100,
            600,
            400,
            None,
            None,
            hinst,
            None,
        )
        .unwrap();
        let _ = ShowWindow(hwnd, SW_SHOW);
        println!("Receiver window opened. hwnd={:?}", hwnd.0);

        let mut msg = MSG::default();
        while GetMessageW(&mut msg, None, 0, 0).into() {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
}
