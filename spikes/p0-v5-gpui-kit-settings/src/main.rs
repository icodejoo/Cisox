use gpui_kit::*;
use gpui_kit::component::{
    ActiveTheme, WindowExt as _,
    button::{Button, ButtonVariants as _},
    dialog::{DialogAction, DialogClose, DialogFooter, DialogTitle},
    h_flex,
    select::{Select, SelectState, SelectEvent}, v_flex,
    // 注意：crates.io 0.7.0 没有 ColorSelect（仅 git 主干有），这里用 ColorPicker
    color_picker::{ColorPicker, ColorPickerEvent, ColorPickerState},
    Colorize,
};

/// 主视图状态
struct MyView {
    select: Entity<SelectState<Vec<String>>>,
    color: Entity<ColorPickerState>,
    last_click: String,
    selected_option: String,
    selected_color: String,
}

impl MyView {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        // 1. 初始化 Select 状态
        let select = cx.new(|cx| SelectState::new(
            vec!["选项 A".to_string(), "选项 B".to_string(), "选项 C".to_string()],
            None,
            window, cx,
        ));
        
        // 订阅 Select 事件
        cx.subscribe_in(&select, window, Self::on_select_event).detach();

        // 2. 初始化 ColorPicker 状态
        let color = cx.new(|cx| ColorPickerState::new(window, cx));
        
        // 订阅 ColorPicker 事件
        cx.subscribe(&color, |this: &mut Self, _, ev, cx| match ev {
            ColorPickerEvent::Change(c) => {
                println!("Selected Color changed: {:?}", c);
                if let Some(c) = c {
                    this.selected_color = c.to_hex();
                } else {
                    this.selected_color = "无".to_string();
                }
                cx.notify();
            }
        }).detach();

        Self {
            select,
            color,
            last_click: "无".to_string(),
            selected_option: "无".to_string(),
            selected_color: "无".to_string(),
        }
    }

    /// Select 事件处理
    fn on_select_event(
        &mut self,
        _: &Entity<SelectState<Vec<String>>>,
        event: &SelectEvent<Vec<String>>,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match event {
            SelectEvent::Confirm(value) => {
                println!("Select Confirm: {:?}", value);
                // Confirm 携带 Option<Value>，None 表示清空
                self.selected_option = value.clone().unwrap_or_else(|| "无".to_string());
                cx.notify();
            }
        }
    }
}

impl Render for MyView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        v_flex()
            .size_full()
            .gap_6()
            .p_8()
            .bg(cx.theme().background)
            .text_color(cx.theme().foreground)
            // 状态显示区
            .child(
                v_flex()
                    .gap_2()
                    .child(h_flex().gap_2().child("上次点击: ").child(self.last_click.clone()))
                    .child(h_flex().gap_2().child("已选选项: ").child(self.selected_option.clone()))
                    .child(h_flex().gap_2().child("当前颜色: ").child(self.selected_color.clone()))
            )
            // 交互组件区
            .child(
                h_flex().gap_4().child(
                    Button::new("dialog-btn")
                        .primary()
                        .label("打开设置对话框")
                        .on_click(cx.listener(|this, _, window, cx| {
                            println!("Clicked: 打开设置对话框");
                            this.last_click = "打开对话框".to_string();
                            cx.notify();
                            
                            // 3. 弹出 Dialog
                            window.open_dialog(cx, move |dialog, _, cx| {
                                dialog
                                    .rounded(cx.theme().radius_lg)
                                    .overlay(true)
                                    .child(
                                        v_flex()
                                            .gap_3()
                                            .child(DialogTitle::new().child("设置提示"))
                                            .child("这是一个包含多种组件的对话框演示，支持确认和取消操作。")
                                    )
                                    .footer(
                                        DialogFooter::new()
                                            .child(DialogClose::new().child(Button::new("cancel").label("取消").outline()))
                                            .child(DialogAction::new().child(Button::new("ok").label("确定").primary())),
                                    )
                                    .on_ok(|_, window, cx| {
                                        println!("Dialog: OK clicked");
                                        window.push_notification("点击了确定", cx);
                                        true
                                    })
                                    .on_cancel(|_, window, cx| {
                                        println!("Dialog: Cancel clicked");
                                        window.push_notification("点击了取消", cx);
                                        true
                                    })
                            });
                        }))
                )
            )
            .child(
                div().border_2().border_color(gpui_kit::red()).child(
                    Select::new(&self.select)
                        .w(px(280.))
                        .placeholder("请选择...")
                        .title_prefix("选项: ")
                )
            )
            .child(
                div().border_2().border_color(gpui_kit::blue()).child(
                    ColorPicker::new(&self.color)
                )
            )
    }
}

fn main() {
    // 初始化应用并打开窗口
    gpui_kit::application().run(|cx| {
        gpui_kit::init(cx);
        gpui_kit::open_window(WindowOptions::default(), cx, |window, cx| {
            cx.new(|cx| MyView::new(window, cx))
        }).expect("Failed to open window");
        println!("[P0-V5] 设置窗已打开，等待交互…");
    });
}
