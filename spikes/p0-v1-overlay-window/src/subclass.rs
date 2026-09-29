use std::sync::{Mutex, OnceLock};
use std::collections::HashMap;
use windows::Win32::UI::WindowsAndMessaging::{
    SetWindowLongPtrW, CallWindowProcW, GWLP_WNDPROC, WM_NCHITTEST, HTTRANSPARENT
};
use windows::Win32::Foundation::{LRESULT, WPARAM, LPARAM, HWND, POINT};
use windows::Win32::Graphics::Gdi::ScreenToClient;

fn wndprocs() -> &'static Mutex<HashMap<isize, isize>> {
    static MAP: OnceLock<Mutex<HashMap<isize, isize>>> = OnceLock::new();
    MAP.get_or_init(|| Mutex::new(HashMap::new()))
}

unsafe extern "system" fn subclass_proc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    let original = {
        let map = wndprocs().lock().unwrap();
        map.get(&(hwnd.0 as isize)).copied().unwrap_or(0)
    };

    if msg == WM_NCHITTEST {
        let x = (lparam.0 as i16) as i32;
        let y = ((lparam.0 >> 16) as i16) as i32;

        let mut point = POINT { x, y };
        let _ = ScreenToClient(hwnd, &mut point);

        eprintln!("[subclass] WM_NCHITTEST at client ({}, {})", point.x, point.y);

        // If it's inside the control box, handle normally
        if point.x >= 100 && point.x <= 500 && point.y >= 100 && point.y <= 400 {
            eprintln!("[subclass]   -> inside box, normal hit-test");
            // normal behavior
        } else {
            eprintln!("[subclass]   -> outside box, returning HTTRANSPARENT");
            // transparent to clicks
            return LRESULT(HTTRANSPARENT as isize);
        }
    }

    if original != 0 {
        CallWindowProcW(std::mem::transmute(original as *const ()), hwnd, msg, wparam, lparam)
    } else {
        windows::Win32::UI::WindowsAndMessaging::DefWindowProcW(hwnd, msg, wparam, lparam)
    }
}

pub fn enable_hit_test_subclass(hwnd: HWND) {
    unsafe {
        let original = SetWindowLongPtrW(hwnd, GWLP_WNDPROC, subclass_proc as *const () as isize);
        eprintln!(
            "[subclass] enable_hit_test_subclass called on hwnd={:?}, previous wndproc={:#x}",
            hwnd.0, original
        );
        if original != 0 && original != subclass_proc as *const () as isize {
            wndprocs().lock().unwrap().insert(hwnd.0 as isize, original);
        }
    }
}
