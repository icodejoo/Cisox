// 独立测试1：只验证透明度，不涉及点击穿透/多屏，排除变量干扰
use gpui::*;

struct Overlay;

impl Render for Overlay {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .flex()
            .items_center()
            .justify_center()
            .w_full()
            .h_full()
            .bg(rgba(0x00000000)) // 外层完全透明
            .child(
                div()
                    .bg(rgba(0xFF000088)) // 半透明红色小方块
                    .p_4()
                    .rounded_lg()
                    .child("TRANSPARENCY TEST"),
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
        println!("Window opened. If transparent, you should see desktop through black areas, only red box opaque-ish.");
    });
}
