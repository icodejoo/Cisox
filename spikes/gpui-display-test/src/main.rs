use gpui::*;

struct AppState;

impl Render for AppState {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .flex()
            .bg(rgb(0x333333))
            .size_full()
            .justify_center()
            .items_center()
            .text_xl()
            .text_color(rgb(0xffffff))
            .child("GPUI Multi-Monitor Validation")
    }
}

fn main() {
    Application::new().run(|cx: &mut App| {
        // Query all displays
        let displays = cx.displays();
        println!("Found {} display(s)", displays.len());

        for (i, display) in displays.iter().enumerate() {
            let bounds = display.bounds();
            println!("Display {}:", i);
            println!("  Bounds: {:?}", bounds);
            // GPUI abstracts DPI, we can check the window scaling after creating it if we want,
            // but `cx.displays()` gives us the logical and physical bounds or we can just print the basic info.
        }

        let options = WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(Bounds::centered(None, size(px(400.0), px(300.0)), cx))),
            ..Default::default()
        };
        
        cx.open_window(options, |cx| cx.new(|_| AppState));
        
        // We will exit immediately for the test automation, or we can just keep it running.
        // cx.quit(); 
    });
}
