//! 副屏专用帧率夹具入口。
//!
//! 在扩展副屏上创建无边框置顶窗口，由 D3D11 flip-model 交换链按 vsync 逐帧出画，
//! 每帧把单调递增序号编码进画面顶部色块条，并记录提交时间戳供对账。
//! 硬性约束：目标显示器必须是非主屏且范围等于 2560,0,2560x1440（按属性，不按设备名），窗口区域必须落在其内，否则不创建窗口直接退出。

mod gpu;
mod win;

use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use snow_fps_fixture::content::{Load, build_scene};
use snow_fps_fixture::{
    FrameRecord, MAX_SECONDS, Mode, MonitorInfo, Options, frames_to_csv, parse_args, pick_target, resolve_region,
    summarize,
};

/// 退出码：参数或显示器校验失败。
const EXIT_REJECTED: i32 = 2;
/// 退出码：运行时失败。
const EXIT_FAILED: i32 = 1;
/// 看门狗余量：无论如何超过 `seconds + 该值` 就退出（防挂起）。
const WATCHDOG_EXTRA_S: f64 = 3.0;
/// 每秒预留的帧记录容量（高刷新率余量）。
const RECORDS_PER_SECOND: usize = 130;

/// 打印显示器清单。
fn print_monitors(monitors: &[MonitorInfo]) {
    for m in monitors {
        println!("{} rect={:?} primary={}", m.device, m.rect, m.primary);
    }
}

/// 运行主循环并返回帧记录。
///
/// # 参数
/// - `window`：夹具窗口（用于泵消息）。
/// - `renderer`：渲染器。
/// - `options`：命令行选项。
/// - `width`、`height`：画面尺寸。
fn run_loop(
    window: &win::FixtureWindow,
    renderer: &mut gpu::Renderer,
    options: &Options,
    width: u32,
    height: u32,
) -> Result<Vec<FrameRecord>, String> {
    let start = Instant::now();
    let seconds = options.seconds.min(MAX_SECONDS);
    let deadline = Duration::from_secs_f64(seconds);
    let hard_limit = Duration::from_secs_f64(seconds + WATCHDOG_EXTRA_S);
    let mut records: Vec<FrameRecord> = Vec::with_capacity(seconds as usize * RECORDS_PER_SECOND + 16);
    let mut seq: u32 = 0;
    let mut pending_vsyncs: u32 = 0;
    while start.elapsed() < deadline && start.elapsed() < hard_limit {
        window.pump();
        // 半速模式：每 divisor 次 Present 才换一帧新序号，其余重复上一帧（序号不变）
        if pending_vsyncs == 0 {
            seq += 1;
            pending_vsyncs = options.divisor;
        }
        pending_vsyncs -= 1;
        let scene = build_scene(width, height, options.load, seq);
        renderer.draw_and_present(&scene, seq)?;
        if pending_vsyncs + 1 == options.divisor {
            // 只在序号首次提交时记录一次，时间戳取 Present 返回时刻
            let now_ns = start.elapsed().as_nanos() as u64;
            let unix_us = SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_micros() as u64);
            records.push(FrameRecord { seq, submit_ns: now_ns, flush_ns: now_ns, unix_us });
            if seq == 1
                && let Some(path) = &options.ready
            {
                let _ = std::fs::write(path, b"ready");
            }
        }
    }
    Ok(records)
}

/// 程序入口。
fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let options = match parse_args(&args) {
        Ok(o) => o,
        Err(e) => {
            eprintln!("参数错误: {e}");
            std::process::exit(EXIT_REJECTED);
        }
    };
    win::set_dpi_aware();
    if options.mode == Mode::DxgiList {
        match win::dxgi_output_lines() {
            Ok(lines) => lines.iter().for_each(|l| println!("{l}")),
            Err(e) => {
                eprintln!("{e}");
                std::process::exit(EXIT_FAILED);
            }
        }
        return;
    }
    let monitors = win::enumerate_monitors();
    if options.mode == Mode::List {
        print_monitors(&monitors);
        return;
    }
    // 创建窗口之前的强制校验：非主屏且范围符合预期、区域落在其内
    let target = match pick_target(&monitors) {
        Ok(t) => t,
        Err(e) => {
            print_monitors(&monitors);
            eprintln!("{e}");
            std::process::exit(EXIT_REJECTED);
        }
    };
    let rect = match resolve_region(target, &options) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("{e}");
            std::process::exit(EXIT_REJECTED);
        }
    };
    println!("target={} monitor={:?} window={:?}", target.device, target.rect, rect);
    if options.mode == Mode::Check {
        return;
    }
    win::keep_display_awake();
    let window = match win::FixtureWindow::create(rect) {
        Ok(w) => w,
        Err(e) => {
            eprintln!("创建窗口失败: {e}");
            std::process::exit(EXIT_FAILED);
        }
    };
    let mut renderer = match gpu::Renderer::new(&window, rect.w, rect.h, options.load == Load::Noise) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("初始化 GPU 失败: {e}");
            drop(window);
            std::process::exit(EXIT_FAILED);
        }
    };
    let result = run_loop(&window, &mut renderer, &options, rect.w, rect.h);
    drop(renderer);
    drop(window);
    let records = match result {
        Ok(r) => r,
        Err(e) => {
            eprintln!("出帧失败: {e}");
            std::process::exit(EXIT_FAILED);
        }
    };
    if let Some(path) = &options.log
        && let Err(e) = std::fs::write(path, frames_to_csv(&records))
    {
        eprintln!("写帧日志失败: {e}");
    }
    match summarize(&records) {
        Some(s) => println!(
            "frames={} span_s={:.3} fps={:.3} max_gap_ms={:.2} late_frames={}",
            s.frames, s.span_s, s.fps, s.max_gap_ms, s.late_frames
        ),
        None => println!("frames={}", records.len()),
    }
}
