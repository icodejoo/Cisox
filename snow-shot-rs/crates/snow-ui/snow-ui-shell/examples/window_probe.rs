//! 自动探针（无需人工操作）：验证覆盖窗建窗、物理落位、点击穿透区域可设置，然后自动退出。
//!
//! 运行：`cargo run -p snow-ui-shell --example window_probe`
//!
//! 判定（全部由程序完成，不模拟任何键鼠输入）：
//! 1. 每块显示器各开一个覆盖窗，`GetWindowRect` 必须等于显示器物理范围；
//! 2. `set_hit_region` / `clear_hit_region` 必须返回成功；
//! 3. 再开一个带边框普通窗口，其矩形必须等于 `WindowSpec::resolve` 的结果；
//! 4. 1.5 秒后二次复核矩形（防止 GPUI 后续调整覆盖我们的落位）。
//!
//! 输出以 `PROBE PASS` / `PROBE FAIL` 结尾，退出码 0 / 1。

use snow_capability::CapabilityRegistry;
use snow_ui_shell::geometry::{LogicalSize, PhysicalRect, Region};
use snow_ui_shell::monitor::MonitorTarget;
use snow_ui_shell::ui::{
    self, AppContext, Context, IntoElement, ParentElement, Render, Styled, Window, div, rgba,
};
use snow_ui_shell::window::WindowSpec;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

/// 任一检查失败即置位。
static FAILED: AtomicBool = AtomicBool::new(false);

/// 记录一条检查结果。
fn check(ok: bool, what: impl AsRef<str>) {
    println!("[{}] {}", if ok { "ok  " } else { "FAIL" }, what.as_ref());
    if !ok {
        FAILED.store(true, Ordering::SeqCst);
    }
}

/// 探针窗口的根视图：半透明色块。
struct ProbeView(&'static str);

impl Render for ProbeView {
    /// 渲染一块半透明色块与说明文字。
    fn render(&mut self, _w: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        div().size_full().bg(rgba(0x3366ff33)).child(self.0)
    }
}

fn main() {
    let caps = CapabilityRegistry::for_current_platform();
    ui::run(move |cx| {
        let monitors = match cx.monitors() {
            Ok(m) => m,
            Err(e) => {
                check(false, format!("枚举显示器: {e}"));
                cx.quit();
                return;
            }
        };
        for m in monitors.all() {
            println!(
                "显示器 {:?} {} bounds={:?} 缩放={} 主屏={}",
                m.id,
                m.name,
                m.bounds,
                m.scale.value(),
                m.is_primary
            );
        }
        check(!monitors.all().is_empty(), "至少有一块显示器");

        // 1) 每块显示器一个覆盖窗
        let mut overlays = Vec::new();
        for m in monitors.all() {
            let spec = WindowSpec::overlay(MonitorTarget::Id(m.id));
            match cx.open_window(&spec, |_w, app| app.new(|_| ProbeView("overlay"))) {
                Ok((win, _)) => overlays.push((win, m.bounds)),
                Err(e) => check(false, format!("创建覆盖窗 {:?}: {e}", m.id)),
            }
        }
        for (win, bounds) in &overlays {
            let Ok(mut ov) = win.overlay(&caps) else {
                check(false, "取覆盖窗控制器");
                continue;
            };
            let rect = ov.screen_rect();
            check(
                rect.as_ref().ok() == Some(bounds),
                format!("覆盖窗落位 期望={bounds:?} 实际={rect:?}"),
            );
            let hole = Region::from_rect(PhysicalRect::new(100, 100, 400, 300));
            check(ov.set_hit_region(&hole).is_ok(), "set_hit_region");
            check(ov.clear_hit_region().is_ok(), "clear_hit_region");
            check(ov.set_hit_region(&Region::new()).is_ok(), "空区域也可设置");
            check(ov.clear_hit_region().is_ok(), "再次 clear_hit_region");
        }

        // 2) 普通带边框窗口
        let normal = WindowSpec::normal("probe", LogicalSize::new(480.0, 320.0));
        let expected = normal.resolve(&monitors).map(|p| p.rect);
        match cx.open_window(&normal, |_w, app| app.new(|_| ProbeView("normal"))) {
            Ok((win, _)) => {
                let actual = win.overlay(&caps).and_then(|o| o.screen_rect());
                check(
                    actual.is_ok() && actual == expected,
                    format!("普通窗口落位 期望={expected:?} 实际={actual:?}"),
                );
            }
            Err(e) => check(false, format!("创建普通窗口: {e}")),
        }

        // 3) 延时复核后自动退出
        let recheck: Vec<_> = overlays.iter().map(|(w, b)| (*w, *b)).collect();
        let caps2 = caps.clone();
        cx.app()
            .spawn(async move |acx| {
                acx.background_executor()
                    .timer(Duration::from_millis(1500))
                    .await;
                for (win, bounds) in recheck {
                    let rect = win.overlay(&caps2).and_then(|o| o.screen_rect());
                    check(
                        rect.as_ref().ok() == Some(&bounds),
                        format!("1.5s 后复核落位 {rect:?}"),
                    );
                }
                acx.update(|app| app.quit());
            })
            .detach();
        cx.quit_after(Duration::from_secs(10));
    });
    if FAILED.load(Ordering::SeqCst) {
        println!("PROBE FAIL");
        std::process::exit(1);
    }
    println!("PROBE PASS");
}
