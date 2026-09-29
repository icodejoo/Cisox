// 独立测试2：在测试1(已验证透明正常)的基础上叠加子类点击穿透逻辑，隔离变量
use gpui::*;

#[path = "../subclass.rs"]
mod subclass;

struct Overlay {
    subclassed: bool,
}

impl Render for Overlay {
    fn render(&mut self, window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        if !self.subclassed {
            use raw_window_handle::HasWindowHandle;
            match window.window_handle() {
                Ok(handle) => match handle.as_raw() {
                    raw_window_handle::RawWindowHandle::Win32(win32) => {
                        eprintln!("[main] got Win32 handle: hwnd={:?}", win32.hwnd);
                        let hwnd = windows::Win32::Foundation::HWND(win32.hwnd.get() as _);
                        subclass::enable_hit_test_subclass(hwnd);
                        self.subclassed = true;
                    }
                    other => {
                        eprintln!("[main] window_handle() returned non-Win32 variant: {:?}", other);
                    }
                },
                Err(e) => {
                    eprintln!("[main] window_handle() FAILED: {:?}", e);
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
                    .child("CLICKTHROUGH TEST"),
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

        cx.open_window(options, |_, cx| cx.new(|_| Overlay { subclassed: false })).unwrap();
        println!("Window opened.");
    });
}
