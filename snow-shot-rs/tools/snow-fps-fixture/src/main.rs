//! 副屏专用帧率夹具入口。
//!
//! 在扩展副屏上创建无边框置顶窗口，由 D3D11 flip-model 交换链按 vsync 逐帧出画，
//! 每帧把单调递增序号编码进画面顶部色块条，并记录提交时间戳供对账。
//! 硬性约束：目标显示器默认必须是唯一的非主屏（按属性，不按设备名），传 `--allow-primary` 才改占主屏；窗口区域必须落在其内，否则不创建窗口直接退出。

mod dual;
mod gpu;
mod win;

use std::sync::atomic::Ordering;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use snow_fps_fixture::content::{Load, build_scene_at};
use snow_fps_fixture::{
    FrameRecord, MAX_SECONDS, Mode, MonitorInfo, Options, frames_to_csv, parse_args, pick_target, resolve_region,
    resolve_span_region, split_region_by_monitors, summarize,
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
/// - `origin`：窗口左上角桌面坐标（网格负载用）。
/// - `shared`：双窗口模式的共享状态（主节拍据此发布序号、响应结束标志）；单窗口传 `None`。
fn run_loop(
    window: &win::FixtureWindow,
    renderer: &mut gpu::Renderer,
    options: &Options,
    width: u32,
    height: u32,
    origin: (i32, i32),
    shared: Option<&dual::Shared>,
) -> Result<Vec<FrameRecord>, String> {
    let start = Instant::now();
    let seconds = options.seconds.min(MAX_SECONDS);
    let deadline = Duration::from_secs_f64(seconds);
    let hard_limit = Duration::from_secs_f64(seconds + WATCHDOG_EXTRA_S);
    let mut records: Vec<FrameRecord> = Vec::with_capacity(seconds as usize * RECORDS_PER_SECOND + 16);
    let mut seq: u32 = 0;
    let mut pending_vsyncs: u32 = 0;
    while start.elapsed() < deadline && start.elapsed() < hard_limit {
        if shared.is_some_and(|s| s.stop.load(Ordering::Acquire)) {
            break;
        }
        window.pump();
        // 半速模式：每 divisor 次 Present 才换一帧新序号，其余重复上一帧（序号不变）
        if pending_vsyncs == 0 {
            seq += 1;
            pending_vsyncs = options.divisor;
        }
        pending_vsyncs -= 1;
        if let Some(s) = shared {
            s.seq.store(seq, Ordering::Release);
        }
        let scene = build_scene_at(width, height, options.load, seq, origin);
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
    // 创建窗口之前的强制校验：默认仅非主屏（显式开关才允许主屏）、区域落在其内；跨屏模式要求区域被显示器并集完整覆盖
    let rect = if options.span {
        match resolve_span_region(&monitors, &options) {
            Ok(r) => {
                println!("target=span primary=true monitor={r:?} window={r:?}");
                r
            }
            Err(e) => {
                print_monitors(&monitors);
                eprintln!("{e}");
                std::process::exit(EXIT_REJECTED);
            }
        }
    } else {
        let target = match pick_target(&monitors, options.allow_primary) {
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
        println!("target={} primary={} monitor={:?} window={:?}", target.device, target.primary, target.rect, rect);
        rect
    };
    // 双窗口：按显示器切分区域；只落在一块屏上时退化为单窗口
    let pieces = if options.dual {
        match split_region_by_monitors(&monitors, rect) {
            Ok(p) => {
                println!("dual pieces={p:?}");
                p
            }
            Err(e) => {
                eprintln!("{e}");
                std::process::exit(EXIT_REJECTED);
            }
        }
    } else {
        Vec::new()
    };
    if options.mode == Mode::Check {
        return;
    }
    win::keep_display_awake();
    if pieces.len() > 1 {
        let result = dual::run_dual(&pieces, rect, &options);
        finish(&options, result);
        return;
    }
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
    let result = run_loop(&window, &mut renderer, &options, rect.w, rect.h, (rect.x, rect.y), None);
    drop(renderer);
    drop(window);
    finish(&options, result);
}

/// 收尾：写帧日志并打印统计；出帧失败则按运行时错误退出。
///
/// # 参数
/// - `options`：命令行选项（取日志路径）。
/// - `result`：主节拍的帧记录或失败原因。
fn finish(options: &Options, result: Result<Vec<FrameRecord>, String>) {
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
