// 验证 upstream Qt 实际使用的技术：SetWindowRgn（对应 Qt 的 setMask）。
// 把窗口的"区域"直接收窄到只剩红色方块那一块，方块以外的屏幕像素
// 根本不属于这个窗口了——理论上点击会直接穿透到底下，因为操作系统层面
// 那块区域压根不算这个窗口的一部分。
use gpui::*;

struct Overlay {
    masked: bool,
}

impl Render for Overlay {
    fn render(&mut self, window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        if !self.masked {
            use raw_window_handle::HasWindowHandle;
            if let Ok(handle) = window.window_handle() {
                if let raw_window_handle::RawWindowHandle::Win32(win32) = handle.as_raw() {
                    unsafe {
                        use windows::Win32::Foundation::HWND;
                        use windows::Win32::Graphics::Gdi::{CreateRectRgn, SetWindowRgn};
                        let hwnd = HWND(win32.hwnd.get() as _);
                        // 只留方块区域 (100,100)-(500,400) 作为窗口的有效区域
                        let rgn = CreateRectRgn(100, 100, 500, 400);
                        let result = SetWindowRgn(hwnd, rgn, true);
                        eprintln!("[test6] SetWindowRgn result={}", result);
                    }
                    self.masked = true;
                }
            }
        }

        div()
            .flex()
            .items_center()
            .justify_center()
            .w_full()
            .h_full()
            .bg(rgba(0x00000000))
            .child(
                div()
                    .bg(rgba(0xFF000088))
                    .p_4()
                    .rounded_lg()
                    .child("SETWINDOWRGN TEST"),
            )
    }
}

fn main() {
    gpui_platform::application().run(|cx: &mut App| {
        let displays = cx.displays();
        let bounds = displays[0].bounds();
        println!("Testing on single display: {:?}", bounds);

        let options = WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(bounds)),
            titlebar: None,
            window_background: WindowBackgroundAppearance::Transparent,
            kind: WindowKind::PopUp,
            focus: false,
            display_id: Some(displays[0].id()),
            ..Default::default()
        };

        cx.open_window(options, |_, cx| cx.new(|_| Overlay { masked: false })).unwrap();
        println!("Window opened.");
    });
}
