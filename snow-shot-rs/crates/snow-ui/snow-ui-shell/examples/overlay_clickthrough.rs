//! 手动验证：覆盖窗 `SetWindowRgn` 点击穿透（真人点击 + 程序判定，ADR-2b 方法论）。
//!
//! 运行：`cargo run -p snow-ui-shell --example overlay_clickthrough`
//!
//! 场景：主屏居中有一个“接收窗口”（普通窗口，绿色）；其上盖着铺满主屏的透明置顶覆盖窗，
//! 覆盖窗只保留中央一块红色区域（500x340 物理像素），其余区域被 `SetWindowRgn` 裁掉。
//!
//! 操作步骤（真人）：
//! 1. 在绿色接收窗口里、红色方块**之外**的地方点几下（至少 3 次）——应穿透到绿窗；
//! 2. 在红色方块**里面**空白处点 1 下——应被覆盖窗接住；
//! 3. 点红色方块里的“完成并退出”按钮结束（45 秒无操作会自动退出）。
//!
//! 判定（程序完成，不模拟输入）：每次点击用 `GetCursorPos` 取屏幕坐标，
//! - 接收窗口收到的点击必须在区域**外**；覆盖窗收到的点击必须在区域**内**；
//! - 通过条件：接收窗 >=3 次、覆盖窗 >=1 次、无违规。
//!
//! 日志：`%TEMP%\snow-shell-overlay-verify.log`，末行为 `VERDICT PASS|FAIL`。

use snow_capability::CapabilityRegistry;
use snow_ui_shell::geometry::{LogicalSize, PhysicalRect, Region};
use snow_ui_shell::monitor::MonitorTarget;
use snow_ui_shell::overlay::cursor_screen_position;
use snow_ui_shell::ui::{
    self, AppContext, Context, InteractiveElement, IntoElement, MouseButton, ParentElement, Render,
    Styled, Window, div, px, rgb, rgba,
};
use snow_ui_shell::window::WindowSpec;
use std::io::Write;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// 接收窗口至少需要收到的穿透点击数。
const MIN_RECEIVER_HITS: usize = 3;
/// 覆盖窗至少需要收到的区域内点击数。
const MIN_OVERLAY_HITS: usize = 1;
/// 无操作自动退出秒数。
const AUTO_QUIT_SECS: u64 = 45;
/// 覆盖区域的物理尺寸（宽, 高）。
const REGION_SIZE: (i32, i32) = (500, 340);

/// 判定统计。
#[derive(Default)]
struct Stats {
    /// 屏幕坐标系下的命中区域。
    region: Region,
    /// 接收窗口在区域外收到的点击（预期）。
    receiver_outside: usize,
    /// 接收窗口在区域内收到的点击（违规：区域没挡住）。
    receiver_inside: usize,
    /// 覆盖窗在区域内收到的点击（预期）。
    overlay_inside: usize,
    /// 覆盖窗在区域外收到的点击（违规：没有穿透）。
    overlay_outside: usize,
    /// 日志文件。
    log_path: PathBuf,
}

impl Stats {
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

    /// 记录一次点击并按落点归类。
    fn record(&mut self, who: &str) {
        let Ok(p) = cursor_screen_position() else {
            self.log(&format!("{who} 点击，但读取光标位置失败"));
            return;
        };
        let inside = self.region.contains(p);
        match (who, inside) {
            ("RECEIVER", false) => self.receiver_outside += 1,
            ("RECEIVER", true) => self.receiver_inside += 1,
            (_, true) => self.overlay_inside += 1,
            (_, false) => self.overlay_outside += 1,
        }
        self.log(&format!(
            "{who}_HIT screen=({}, {}) inside_region={inside}",
            p.x, p.y
        ));
    }

    /// 输出并返回最终结论。
    fn verdict(&self) -> bool {
        let pass = self.receiver_outside >= MIN_RECEIVER_HITS
            && self.overlay_inside >= MIN_OVERLAY_HITS
            && self.receiver_inside == 0
            && self.overlay_outside == 0;
        self.log(&format!(
            "SUMMARY receiver_outside={} receiver_inside={} overlay_inside={} overlay_outside={}",
            self.receiver_outside, self.receiver_inside, self.overlay_inside, self.overlay_outside
        ));
        self.log(if pass { "VERDICT PASS" } else { "VERDICT FAIL" });
        pass
    }
}

/// 共享统计。
type Shared = Arc<Mutex<Stats>>;

/// 接收窗口视图：记录穿透过来的点击。
struct ReceiverView(Shared);

impl Render for ReceiverView {
    /// 绿底说明文字；任意左键按下记为接收。
    fn render(&mut self, _w: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        let stats = self.0.clone();
        div()
            .size_full()
            .bg(rgb(0x2e7d32))
            .text_color(rgb(0xffffff))
            .p_4()
            .on_mouse_down(MouseButton::Left, move |_, _, _| {
                if let Ok(mut s) = stats.lock() {
                    s.record("RECEIVER");
                }
            })
            .child("接收窗口：在红色方块之外点击，点击应穿透到这里。")
    }
}

/// 覆盖窗视图：透明底 + 区域内红色方块与退出按钮。
struct OverlayView {
    /// 共享统计。
    stats: Shared,
    /// 红色方块在窗口内的逻辑矩形 `(x, y, w, h)`。
    box_logical: (f32, f32, f32, f32),
}

impl Render for OverlayView {
    /// 全窗透明，中央画红色方块，方块内放退出按钮。
    fn render(&mut self, _w: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        let (x, y, w, h) = self.box_logical;
        let on_box = self.stats.clone();
        let on_quit = self.stats.clone();
        div().size_full().bg(rgba(0x00000000)).child(
            div()
                .absolute()
                .left(px(x))
                .top(px(y))
                .w(px(w))
                .h(px(h))
                .bg(rgba(0xd32f2fcc))
                .text_color(rgb(0xffffff))
                .p_4()
                .on_mouse_down(MouseButton::Left, move |_, _, _| {
                    if let Ok(mut s) = on_box.lock() {
                        s.record("OVERLAY");
                    }
                })
                .child("覆盖窗区域：在这里点击应被覆盖窗接住")
                .child(
                    div()
                        .mt_4()
                        .p_2()
                        .bg(rgb(0x212121))
                        .on_mouse_down(MouseButton::Left, move |_, _, app| {
                            if let Ok(s) = on_quit.lock() {
                                s.log("QUIT_BUTTON");
                            }
                            app.quit();
                        })
                        .child("完成并退出"),
                ),
        )
    }
}

fn main() {
    let log_path = std::env::temp_dir().join("snow-shell-overlay-verify.log");
    let _ = std::fs::remove_file(&log_path);
    let stats: Shared = Arc::new(Mutex::new(Stats {
        log_path,
        ..Stats::default()
    }));
    let caps = CapabilityRegistry::for_current_platform();
    let run_stats = stats.clone();
    ui::run(move |cx| {
        let stats = run_stats;
        let Ok(monitors) = cx.monitors() else {
            println!("枚举显示器失败");
            cx.quit();
            return;
        };
        // 1) 接收窗口（普通窗口，居中）
        let recv_spec = WindowSpec::normal("接收窗口", LogicalSize::new(1000.0, 640.0));
        let recv_stats = stats.clone();
        let Ok((recv_win, _)) =
            cx.open_window(&recv_spec, |_w, app| app.new(|_| ReceiverView(recv_stats)))
        else {
            println!("创建接收窗口失败");
            cx.quit();
            return;
        };
        let Ok(recv_rect) = recv_win.overlay(&caps).and_then(|o| o.screen_rect()) else {
            println!("读取接收窗口位置失败");
            cx.quit();
            return;
        };
        // 2) 覆盖窗：铺满主屏，区域 = 接收窗口中心的一块
        let Ok(primary) = monitors.resolve(MonitorTarget::Primary) else {
            cx.quit();
            return;
        };
        let center = recv_rect.center();
        let screen_hole = PhysicalRect::new(
            center.x - REGION_SIZE.0 / 2,
            center.y - REGION_SIZE.1 / 2,
            REGION_SIZE.0,
            REGION_SIZE.1,
        );
        let window_hole = screen_hole.translate(-primary.bounds.x, -primary.bounds.y);
        if let Ok(mut s) = stats.lock() {
            s.region = Region::from_rect(screen_hole);
            s.log(&format!(
                "SETUP monitor={:?} scale={} receiver={recv_rect:?} region_screen={screen_hole:?}",
                primary.id,
                primary.scale.value()
            ));
        }
        let scale = primary.scale;
        let logical = scale.rect_to_logical(window_hole);
        let spec = WindowSpec::overlay(MonitorTarget::Primary);
        let overlay_stats = stats.clone();
        let opened = cx.open_window(&spec, move |_w, app| {
            app.new(|_| OverlayView {
                stats: overlay_stats,
                box_logical: (logical.x, logical.y, logical.width, logical.height),
            })
        });
        let Ok((overlay_win, _)) = opened else {
            println!("创建覆盖窗失败");
            cx.quit();
            return;
        };
        match overlay_win.overlay(&caps) {
            Ok(mut ov) => {
                let r = ov.set_hit_region(&Region::from_rect(window_hole));
                if let Ok(s) = stats.lock() {
                    s.log(&format!("set_hit_region -> {r:?}"));
                }
            }
            Err(e) => println!("取覆盖窗控制器失败: {e}"),
        }
        cx.quit_after(Duration::from_secs(AUTO_QUIT_SECS));
    });
    let pass = stats.lock().map(|s| s.verdict()).unwrap_or(false);
    std::process::exit(if pass { 0 } else { 1 });
}
