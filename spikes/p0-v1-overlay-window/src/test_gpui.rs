use gpui::*;

struct HelloWorld;

impl Render for HelloWorld {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .flex()
            .bg(rgb(0x2e7d32))
            .size_full()
            .justify_center()
            .items_center()
            .text_xl()
            .text_color(rgb(0xffffff))
            .child(format!("Hello, World!"))
    }
}

fn main() {
    gpui_platform::application().run(|cx: &mut App| {
        let options = WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(Bounds::centered(None, size(px(300.0), px(300.0)), cx))),
            ..Default::default()
        };
        cx.open_window(options, |cx| cx.new(|_| HelloWorld));
    });
}
