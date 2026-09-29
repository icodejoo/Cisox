use gpui::*;

mod subclass;

struct Overlay {
    text: SharedString,
    subclassed: bool,
}

impl Render for Overlay {
    fn render(&mut self, window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        if !self.subclassed {
            use raw_window_handle::HasWindowHandle;
            if let Ok(handle) = window.window_handle() {
                if let raw_window_handle::RawWindowHandle::Win32(win32) = handle.as_raw() {
                    let hwnd = windows::Win32::Foundation::HWND(win32.hwnd.get() as _);
                    subclass::enable_hit_test_subclass(hwnd);
                    self.subclassed = true;
                }
            }
        }

        div()
            .flex()
            .items_center()
            .justify_center()
            .w_full()
            .h_full()
            .bg(rgba(0x00000000)) // Transparent overall
            .child(
                div()
                    .bg(rgba(0xFF000088)) // Semi-transparent red box
                    .p_4()
                    .rounded_lg()
                    .child(self.text.clone()),
            )
    }
}

fn main() {
    gpui_platform::application().run(|cx: &mut App| {
        // 1. Get displays
        let displays = cx.displays();
        println!("Found {} displays.", displays.len());

        for (i, display) in displays.into_iter().enumerate() {
            let bounds = display.bounds();
            println!("Display {}: bounds = {:?}", i, bounds);

            // 2. Create a transparent, topmost, borderless window for each display
            let options = WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                titlebar: None,
                window_background: WindowBackgroundAppearance::Transparent,
                kind: WindowKind::PopUp, // Often implies topmost / unmanaged
                focus: false,
                display_id: Some(display.id()),
                ..Default::default()
            };

            cx.open_window(options, |_, cx| {
                cx.new(|_| Overlay {
                    text: format!("Overlay on Display {}", i).into(),
                    subclassed: false,
                })
            }).unwrap();
        }
    });
}
