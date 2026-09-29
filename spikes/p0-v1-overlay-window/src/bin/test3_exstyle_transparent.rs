// 独立测试3：改用 WS_EX_TRANSPARENT 扩展样式做整窗穿透，验证是否是 DirectComposition
// 窗口跟 WM_NCHITTEST/HTTRANSPARENT 机制架构不兼容。这次全窗穿透（不分区域），
// 只用来判断这条路本身是否可行，不追求最终交互形态。
use gpui::*;

struct Overlay;

impl Render for Overlay {
    fn render(&mut self, window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        use raw_window_handle::HasWindowHandle;
        static DONE: std::sync::Once = std::sync::Once::new();
        if let Ok(handle) = window.window_handle() {
            if let raw_window_handle::RawWindowHandle::Win32(win32) = handle.as_raw() {
                DONE.call_once(|| unsafe {
                    use windows::Win32::Foundation::HWND;
                    use windows::Win32::UI::WindowsAndMessaging::{
                        GetWindowLongPtrW, SetWindowLongPtrW, GWL_EXSTYLE, WS_EX_TRANSPARENT,
                    };
                    let hwnd = HWND(win32.hwnd.get() as _);
                    let current = GetWindowLongPtrW(hwnd, GWL_EXSTYLE);
                    let new_style = current | (WS_EX_TRANSPARENT.0 as isize);
                    let result = SetWindowLongPtrW(hwnd, GWL_EXSTYLE, new_style);
                    eprintln!(
                        "[test3] hwnd={:?} old_exstyle={:#x} new_exstyle={:#x} set_result={:#x}",
                        hwnd.0, current, new_style, result
                    );
                });
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
                    .child("EXSTYLE WS_EX_TRANSPARENT TEST (whole window click-through)"),
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

        cx.open_window(options, |_, cx| cx.new(|_| Overlay)).unwrap();
        println!("Window opened. Try clicking ANYWHERE - whole window should be click-through now.");
    });
}
