use gpui::*;
use global_hotkey::{GlobalHotKeyManager, GlobalHotKeyEvent, hotkey::{HotKey, Modifiers, Code}};
use tray_icon::{TrayIconBuilder, Icon, menu::{Menu, MenuItem, MenuEvent}};
use std::fs::OpenOptions;
use std::io::Write;
use chrono::Local;
use std::thread;

struct MyView {}

impl Render for MyView {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .bg(rgb(0x202030))
            .text_color(rgb(0xFFFFFF))
            .p_8()
            .w_full()
            .h_full()
            .child("P0-V4: 托盘 + 全局热键验证窗口。请右键托盘图标点菜单，或按 Ctrl+Alt+S。")
    }
}

fn log_event(msg: &str) {
    let now = Local::now();
    let log_msg = format!("[{}] {}\n", now.format("%Y-%m-%d %H:%M:%S"), msg);
    println!("{}", msg);
    
    if let Ok(mut file) = OpenOptions::new()
        .create(true)
        .append(true)
        .open("E:\\workspaces\\Cisox\\spikes\\p0-v4-tray-hotkey\\p0v4_events.log") 
    {
        let _ = file.write_all(log_msg.as_bytes());
        let _ = file.flush();
    }
}

fn main() {
    // 方案B：开一个专用线程，在该线程内创建 tray/hotkey 并自己跑 GetMessageW/TranslateMessage/DispatchMessageW 消息循环。
    // 理由：GPUI 有自己的主消息循环，如果我们在主线程创建 tray 和 hotkey，
    // 它们的内部 HWND 可能会被 GPUI 的事件循环拦截或者不派发特定消息。
    // tray-icon 和 global-hotkey 在 Windows 上依赖标准的 Win32 消息循环。
    // 专门分配一个背景线程来跑原生的 GetMessageW 循环，可以 100% 确保
    // 托盘菜单事件和全局热键事件得到正确处理，不会和 GPUI 互相干扰。
    
    thread::spawn(move || {
        let manager = GlobalHotKeyManager::new().unwrap();
        let hotkey = HotKey::new(Some(Modifiers::CONTROL | Modifiers::ALT), Code::KeyS);
        manager.register(hotkey).unwrap();

        let menu = Menu::new();
        // 显式指定 id，方便事件线程区分是哪一项被点了
        let item1 = MenuItem::with_id("act_hello", "Action 1: 打个招呼", true, None);
        let item2 = MenuItem::with_id("act_quit", "Action 2: 退出提示", true, None);
        let _ = menu.append(&item1);
        let _ = menu.append(&item2);

        // 创建一个纯蓝的 32x32 图标
        let mut rgba = Vec::new();
        for _ in 0..(32 * 32) {
            rgba.push(0);   // R
            rgba.push(0);   // G
            rgba.push(255); // B
            rgba.push(255); // A
        }
        let icon = Icon::from_rgba(rgba, 32, 32).unwrap();

        let _tray_icon = TrayIconBuilder::new()
            .with_menu(Box::new(menu))
            .with_tooltip("Cisox Tray")
            .with_icon(icon)
            .build()
            .unwrap();

        log_event("托盘图标已创建、全局热键 Ctrl+Alt+S 已注册，进入 Win32 消息循环");


        // Windows 消息循环
        unsafe {
            use windows::Win32::UI::WindowsAndMessaging::{GetMessageW, TranslateMessage, DispatchMessageW, MSG};
            let mut msg = MSG::default();
            while GetMessageW(&mut msg, None, 0, 0).into() {
                let _ = TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
        }
    });

    // 另外开一个线程接收 crossbeam channel 里的事件并写入日志
    thread::spawn(move || {
        let hotkey_receiver = GlobalHotKeyEvent::receiver();
        let menu_receiver = MenuEvent::receiver();
        loop {
            if let Ok(event) = hotkey_receiver.try_recv() {
                log_event(&format!("HOTKEY 事件: Ctrl+Alt+S 被按下 ({:?})", event));
            }
            if let Ok(event) = menu_receiver.try_recv() {
                let name = match event.id.0.as_str() {
                    "act_hello" => "Action 1: 打个招呼",
                    "act_quit" => "Action 2: 退出提示",
                    other => other,
                };
                log_event(&format!("MENU 事件: 托盘菜单项被点击 -> {}", name));
            }
            thread::sleep(std::time::Duration::from_millis(50));
        }
    });

    gpui_platform::application().run(|cx: &mut App| {
        // 用一个小窗口，避免全屏盖住托盘区，方便真人手动验证
        let bounds = Bounds {
            origin: point(px(200.), px(200.)),
            size: size(px(780.), px(260.)),
        };
        let options = WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(bounds)), 
            ..Default::default() 
        };
        cx.open_window(options, |_, cx| cx.new(|_| MyView{})).unwrap();
    });
}
