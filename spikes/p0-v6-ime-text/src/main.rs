//! P0-V6 IME 插桩 spike。
//! 用法：`p0-v6-ime-text [--mode raw|kit] [--smoke]`
//! - raw：自定义 EntityInputHandler，绕开 gpui-kit（默认）
//! - kit：原 gpui-kit Input，外加 30ms 轮询日志，脱离 render 时机读状态
//! - smoke：只做启动自检（装钩子、状态稳定后自动退出），不含任何输入模拟

mod raw_input;
mod trace;

use std::time::Duration;

use gpui_kit::assets::Assets;
use gpui_kit::component::{input::{Input, InputState}, *};
use gpui_kit::*;

use raw_input::{BACKSPACE_KEY, Backspace, RawInput};

/// 日志目录（编译期固定为 spike 目录下的 logs）
const LOG_DIR: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/logs");
/// kit 模式轮询间隔（毫秒）
const POLL_MS: u64 = 30;

/// kit 模式视图：gpui-kit 输入框 + 调试面板
pub struct KitView {
    /// 输入框状态实体
    input_state: Entity<InputState>,
    /// 轮询任务，持有以保持存活
    _poller: Task<()>,
}

impl KitView {
    /// 创建视图，聚焦输入框并启动状态轮询。
    fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let input_state =
            cx.new(|cx| InputState::new(window, cx).placeholder("请用搜狗输入法敲 nihao 空格"));
        input_state.update(cx, |s, cx| s.focus(window, cx));
        let st = input_state.clone();
        let _poller = cx.spawn_in(window, async move |_this, cx| {
            let mut last = String::new();
            loop {
                cx.background_executor()
                    .timer(Duration::from_millis(POLL_MS))
                    .await;
                let snap = cx.update(|window, cx| {
                    st.update(cx, |s, cx| {
                        format!(
                            "value={:?} cursor={} marked_text_range={:?}",
                            s.value(),
                            s.cursor(),
                            s.marked_text_range(window, cx)
                        )
                    })
                });
                match snap {
                    Ok(s) if s != last => {
                        trace::log(format!("KIT轮询 {s}"));
                        last = s;
                    }
                    Ok(_) => {}
                    Err(_) => break,
                }
            }
        });
        Self {
            input_state,
            _poller,
        }
    }
}

impl Render for KitView {
    /// 渲染输入框与当前状态。
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let (value, marked) = self.input_state.update(cx, |s, cx| {
            (s.value(), s.marked_text_range(window, cx))
        });
        v_flex()
            .p_5()
            .gap_4()
            .size_full()
            .child(Input::new(&self.input_state))
            .child(format!("[KIT 模式] value={value:?} marked={marked:?}"))
    }
}

/// 程序入口：解析参数、初始化日志、开窗、启动 Win32 巡检。
fn main() {
    let args: Vec<String> = std::env::args().collect();
    let kit = args.windows(2).any(|w| w[0] == "--mode" && w[1] == "kit");
    let smoke = args.iter().any(|a| a == "--smoke");
    let mode = if kit { "kit" } else { "raw" };
    let suffix = if smoke { "-smoke" } else { "" };
    trace::init_log(&format!("{LOG_DIR}/ime-trace-{mode}{suffix}.log"));
    trace::log(format!(
        "启动 mode={mode} smoke={smoke} pid={}",
        std::process::id()
    ));

    let app = gpui_kit::application().with_assets(Assets);
    app.run(move |cx| {
        gpui_kit::init(cx);
        cx.bind_keys([KeyBinding::new(BACKSPACE_KEY, Backspace, None)]);
        let opts = WindowOptions {
            window_bounds: Some(WindowBounds::centered(size(px(800.), px(400.)), cx)),
            ..Default::default()
        };
        if kit {
            gpui_kit::open_window(opts, cx, |w, cx| cx.new(|cx| KitView::new(w, cx)))
                .expect("开窗失败");
        } else {
            gpui_kit::open_window(opts, cx, |w, cx| cx.new(|cx| RawInput::new(w, cx)))
                .expect("开窗失败");
        }
        trace::spawn_watcher(cx, smoke);
    });
}
