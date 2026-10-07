//! 多屏覆盖窗 spike（M0）：每块显示器同时开一个覆盖窗，验证按住鼠标跨屏拖动时事件如何投递。
//!
//! 运行：`cargo run -p snow-ui-shell --example multi_overlay_probe`
//!
//! 过程（全自动，不需要人操作，约 6 秒；运行期间请不要碰鼠标）：
//! 1. 每块显示器开一个铺满的覆盖窗，记录窗口的屏幕矩形与 `scale_factor()`；
//! 2. 后台线程用 `SetCursorPos` / `SendInput` 在第一块屏按下左键，拖过屏缝到第二块屏再松开，
//!    然后反向再来一次；
//! 3. 每个窗口记录收到的按下 / 移动 / 松开事件（窗口逻辑坐标 + `GetCursorPos` 物理坐标）。
//!
//! 判定：
//! - 方案 A：起始窗口在光标离开它之后仍持续收到 move，且 up 也投给起始窗口；
//! - 方案 B：光标进入另一块屏后起始窗口收不到事件（需要全局光标轮询）。
//!
//! 手动模式：`... --example multi_overlay_probe -- --manual`，不模拟输入，留 25 秒给真人按住左键跨屏拖动。
//!
//! 已知限制：本机实测 `SendInput` 的合成按键没有稳定送达覆盖窗（`GetAsyncKeyState` 显示按钮一闪即灭），
//! 自动模式的按下 / 松开事件为 0，判定以源码为准（见设计文档 §6.2）；需要实测时用手动模式。
//!
//! 日志：`%TEMP%\snow-multi-overlay-probe.log`，末行为 `VERDICT A|B|INCONCLUSIVE`。

use snow_ui_shell::geometry::PhysicalPoint;
use snow_ui_shell::monitor::{MonitorInfo, MonitorTarget};
use snow_ui_shell::overlay::cursor_screen_position;
use snow_ui_shell::ui::{
    self, AppContext, Context, InteractiveElement, IntoElement, MouseButton, MouseDownEvent,
    MouseMoveEvent, MouseUpEvent, Render, Styled, Window, div, rgba,
};
use snow_ui_shell::window::WindowSpec;
use std::io::Write;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    INPUT, INPUT_0, INPUT_MOUSE, MOUSEEVENTF_LEFTDOWN, MOUSEEVENTF_LEFTUP, MOUSEINPUT, SendInput,
};
use windows::Win32::UI::Input::KeyboardAndMouse::{GetAsyncKeyState, VK_LBUTTON};
use windows::Win32::UI::WindowsAndMessaging::SetCursorPos;

/// 整个探针的最长运行时间（超时强制退出）。
const AUTO_QUIT_SECS: u64 = 12;
/// 手动模式的最长运行时间。
const MANUAL_QUIT_SECS: u64 = 25;
/// 启动后等窗口落位的时间。
const SETTLE_MS: u64 = 1500;
/// 拖动分多少步走完。
const DRAG_STEPS: i32 = 40;
/// 每步间隔（毫秒）。
const STEP_MS: u64 = 15;

/// 一次事件记录。
#[derive(Debug, Clone)]
struct Event {
    /// 窗口序号（与显示器枚举顺序一致）。
    window: usize,
    /// 事件类型：down / move / up。
    kind: &'static str,
    /// 窗口逻辑坐标。
    local: (f32, f32),
    /// 事件发生时的全局光标物理坐标。
    cursor: PhysicalPoint,
    /// 一轮拖动的编号（1 = 第一块 → 第二块，2 = 反向）。
    round: u32,
}

/// 共享状态。
#[derive(Default)]
struct Probe {
    /// 事件流水。
    events: Vec<Event>,
    /// 当前拖动轮次（0 = 还没开始）。
    round: u32,
    /// 日志文件。
    log_path: PathBuf,
    /// 各窗口的屏幕物理矩形 `(x, y, w, h)`。
    rects: Vec<(i32, i32, i32, i32)>,
    /// 不看轮次的原始按下 / 松开计数（排查模拟输入是否到达窗口）。
    raw_down: usize,
    /// 同上，松开。
    raw_up: usize,
}

impl Probe {
    /// 写日志并同步打印。
    fn log(&self, line: &str) {
        println!("{line}");
        if let Ok(mut f) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.log_path)
        {
            let _ = writeln!(f, "{line}");
        }
    }

    /// 记录一次窗口事件。
    fn push(&mut self, window: usize, kind: &'static str, local: (f32, f32)) {
        if self.round == 0 {
            return;
        }
        let cursor = cursor_screen_position().unwrap_or(PhysicalPoint::new(i32::MIN, i32::MIN));
        let event = Event {
            window,
            kind,
            local,
            cursor,
            round: self.round,
        };
        self.events.push(event);
    }
}

/// 共享句柄。
type Shared = Arc<Mutex<Probe>>;

/// 覆盖窗视图：半透明着色（每屏一种颜色），只负责把事件交给探针。
struct ProbeView {
    /// 窗口序号。
    index: usize,
    /// 共享探针。
    probe: Shared,
}

impl Render for ProbeView {
    /// 全窗着色并挂上按下 / 移动 / 松开监听。
    fn render(&mut self, window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        let (i, down, mv, up) = (
            self.index,
            self.probe.clone(),
            self.probe.clone(),
            self.probe.clone(),
        );
        let _ = window.scale_factor();
        let tint = if i % 2 == 0 { 0x1677FF33 } else { 0xFF4D4F33 };
        div()
            .size_full()
            .bg(rgba(tint))
            .on_mouse_down(MouseButton::Left, move |e: &MouseDownEvent, _, _| {
                if let Ok(mut p) = down.lock() {
                    p.raw_down += 1;
                    p.push(i, "down", (e.position.x.as_f32(), e.position.y.as_f32()));
                }
            })
            .on_mouse_move(move |e: &MouseMoveEvent, _, _| {
                if let Ok(mut p) = mv.lock() {
                    p.push(i, "move", (e.position.x.as_f32(), e.position.y.as_f32()));
                }
            })
            .on_mouse_up(MouseButton::Left, move |e: &MouseUpEvent, _, _| {
                if let Ok(mut p) = up.lock() {
                    p.raw_up += 1;
                    p.push(i, "up", (e.position.x.as_f32(), e.position.y.as_f32()));
                }
            })
    }
}

/// 发一个左键按下 / 松开。
fn click_button(down: bool) {
    let flags = if down { MOUSEEVENTF_LEFTDOWN } else { MOUSEEVENTF_LEFTUP };
    let input = INPUT {
        r#type: INPUT_MOUSE,
        Anonymous: INPUT_0 {
            mi: MOUSEINPUT {
                dx: 0,
                dy: 0,
                mouseData: 0,
                dwFlags: flags,
                time: 0,
                dwExtraInfo: 0,
            },
        },
    };
    // SAFETY: 单个栈上的 INPUT 结构，大小取自类型本身。
    let sent = unsafe { SendInput(&[input], std::mem::size_of::<INPUT>() as i32) };
    std::thread::sleep(Duration::from_millis(30));
    // SAFETY: 只读取按键状态。
    let state = unsafe { GetAsyncKeyState(i32::from(VK_LBUTTON.0)) };
    println!("SENDINPUT down={down} sent={sent} lbutton_state={state:#x}");
}

/// 把光标移到物理坐标。
fn move_cursor(x: i32, y: i32) {
    // SAFETY: 纯参数调用，失败只影响本次探测。
    unsafe {
        let _ = SetCursorPos(x, y);
    }
}

/// 从 `from` 按住左键拖到 `to` 再松开。
fn drag(from: (i32, i32), to: (i32, i32)) {
    move_cursor(from.0, from.1);
    std::thread::sleep(Duration::from_millis(120));
    click_button(true);
    std::thread::sleep(Duration::from_millis(120));
    for step in 1..=DRAG_STEPS {
        let x = from.0 + (to.0 - from.0) * step / DRAG_STEPS;
        let y = from.1 + (to.1 - from.1) * step / DRAG_STEPS;
        move_cursor(x, y);
        std::thread::sleep(Duration::from_millis(STEP_MS));
    }
    std::thread::sleep(Duration::from_millis(120));
    click_button(false);
    std::thread::sleep(Duration::from_millis(200));
}

/// 屏幕物理点是否落在矩形内。
fn inside(rect: (i32, i32, i32, i32), p: PhysicalPoint) -> bool {
    p.x >= rect.0 && p.x < rect.0 + rect.2 && p.y >= rect.1 && p.y < rect.1 + rect.3
}

/// 分析一轮拖动：起始窗口在光标离开后仍收到的 move 数、对侧窗口收到的 move 数、up 投给了谁。
fn analyze(probe: &Probe, round: u32, start: usize, other: usize) -> (usize, usize, Option<usize>) {
    let start_rect = probe.rects[start];
    let mut start_after_leave = 0;
    let mut other_moves = 0;
    let mut up_to = None;
    for e in probe.events.iter().filter(|e| e.round == round) {
        match e.kind {
            "move" if e.window == start && !inside(start_rect, e.cursor) => start_after_leave += 1,
            "move" if e.window == other => other_moves += 1,
            "up" => up_to = Some(e.window),
            _ => {}
        }
    }
    (start_after_leave, other_moves, up_to)
}

fn main() {
    let log_path = std::env::temp_dir().join("snow-multi-overlay-probe.log");
    let _ = std::fs::remove_file(&log_path);
    let probe: Shared = Arc::new(Mutex::new(Probe {
        log_path,
        ..Probe::default()
    }));
    let run_probe = probe.clone();
    ui::run(move |cx| {
        let probe = run_probe;
        let Ok(monitors) = cx.monitors() else {
            println!("枚举显示器失败");
            cx.quit();
            return;
        };
        let list: Vec<MonitorInfo> = monitors.all().to_vec();
        if list.len() < 2 {
            if let Ok(p) = probe.lock() {
                p.log("SKIP 只有一块显示器，无法验证跨屏");
            }
            cx.quit();
            return;
        }
        for (index, monitor) in list.iter().enumerate() {
            let spec = WindowSpec::overlay(MonitorTarget::Id(monitor.id));
            let view_probe = probe.clone();
            let opened = cx.open_window(&spec, move |_w, app| {
                app.new(|_| ProbeView {
                    index,
                    probe: view_probe,
                })
            });
            if let Ok(mut p) = probe.lock() {
                p.rects.push((
                    monitor.bounds.x,
                    monitor.bounds.y,
                    monitor.bounds.width,
                    monitor.bounds.height,
                ));
                p.log(&format!(
                    "WINDOW {index} monitor={} bounds={:?} scale={} opened={}",
                    monitor.name,
                    monitor.bounds,
                    monitor.scale.value(),
                    opened.is_ok()
                ));
            }
        }
        let manual = std::env::args().any(|a| a == "--manual");
        if manual {
            if let Ok(mut p) = probe.lock() {
                p.round = 1;
                p.log("MANUAL 请在 25 秒内按住左键从一块屏拖到另一块屏再松开");
            }
            cx.quit_after(Duration::from_secs(MANUAL_QUIT_SECS));
            return;
        }
        // 后台线程模拟输入；主线程继续跑 GPUI 事件循环
        let driver = probe.clone();
        let (a, b) = (list[0].bounds, list[1].bounds);
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(SETTLE_MS));
            let from_a = (a.x + a.width / 2, a.y + a.height / 2);
            let to_b = (b.x + b.width / 2, b.y + b.height / 2);
            for (round, (from, to)) in [(from_a, to_b), (to_b, from_a)].into_iter().enumerate() {
                if let Ok(mut p) = driver.lock() {
                    p.round = round as u32 + 1;
                }
                drag(from, to);
            }
            if let Ok(mut p) = driver.lock() {
                p.round = 0;
            }
        });
        cx.quit_after(Duration::from_secs(AUTO_QUIT_SECS));
    });
    let Ok(p) = probe.lock() else {
        std::process::exit(2);
    };
    if p.rects.len() < 2 {
        std::process::exit(0);
    }
    let (a_after, b_moves, a_up) = analyze(&p, 1, 0, 1);
    let (b_after, a_moves, b_up) = analyze(&p, 2, 1, 0);
    p.log(&format!(
        "ROUND1 start=0 moves_in_start_after_leave={a_after} moves_in_other={b_moves} up_to={a_up:?}"
    ));
    p.log(&format!(
        "ROUND2 start=1 moves_in_start_after_leave={b_after} moves_in_other={a_moves} up_to={b_up:?}"
    ));
    // 抽样输出：按下 / 松开全部列出，move 只列起始窗口在光标离开后的前几条（看坐标是否超界）
    let mut sampled_moves = [0usize; 3];
    for e in &p.events {
        let leaving = e.kind == "move" && !inside(p.rects[e.window], e.cursor);
        let keep = e.kind != "move" || (leaving && sampled_moves[e.round as usize] < 3);
        if keep {
            if e.kind == "move" {
                sampled_moves[e.round as usize] += 1;
            }
            p.log(&format!(
                "EVT round={} window={} {} local=({:.1},{:.1}) cursor=({},{})",
                e.round, e.window, e.kind, e.local.0, e.local.1, e.cursor.x, e.cursor.y
            ));
        }
    }
    let total = p.events.len();
    let continues = a_after > 0 && b_after > 0 && a_up == Some(0) && b_up == Some(1);
    let stops = a_after == 0 && b_after == 0;
    let verdict = if total == 0 {
        "INCONCLUSIVE（没有收到任何事件）"
    } else if continues {
        "A（起始窗口持续收到事件，up 投给起始窗口）"
    } else if stops {
        "B（光标离开后起始窗口收不到事件，需要全局光标轮询）"
    } else {
        "INCONCLUSIVE（行为不一致，见上面的轮次数据）"
    };
    p.log(&format!("EVENTS total={total} raw_down={} raw_up={}", p.raw_down, p.raw_up));
    p.log(&format!("VERDICT {verdict}"));
}
