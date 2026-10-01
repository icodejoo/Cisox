//! 双窗口跨屏模式：每块显示器一个窗口、一个线程、一套交换链，各自按所在屏的 vsync 出帧。
//!
//! 同步方案：第一个（最左）窗口是主节拍，递增序号并写入 [`Shared`]，只有它记录帧日志；
//! 其余窗口每个 vsync 读取最新序号重画，不记日志。序号条按整个区域宽度切段，各窗口只画自己那段。

use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU32, Ordering};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use snow_fps_fixture::content::{Load, build_scene_at};
use snow_fps_fixture::{FrameRecord, MAX_SECONDS, Options, Rect};

use crate::{gpu, win};

/// 起跑信号：等待各窗口初始化。
const GO_WAIT: u8 = 0;
/// 起跑信号：全部就绪，开始出帧。
const GO_RUN: u8 = 1;
/// 起跑信号：有窗口初始化失败，放弃。
const GO_ABORT: u8 = 2;
/// 等待信号时的轮询间隔。
const POLL: Duration = Duration::from_millis(1);
/// 看门狗余量（秒），与单窗口一致。
const WATCHDOG_EXTRA_S: f64 = 3.0;

/// 各窗口线程共享的状态。
pub struct Shared {
    /// 主节拍当前序号（0 = 尚未出第一帧）。
    pub seq: AtomicU32,
    /// 结束标志：主节拍跑完或任一线程出错时置位。
    pub stop: AtomicBool,
    /// 起跑信号（`GO_*`）。
    go: AtomicU8,
}

/// 单个窗口线程：创建窗口与渲染器，等起跑信号后按角色出帧。
fn worker(
    piece: Rect,
    region: Rect,
    options: &Options,
    shared: &Shared,
    leader: bool,
    init: &mpsc::Sender<Result<(), String>>,
) -> Result<Vec<FrameRecord>, String> {
    let setup = || -> Result<(win::FixtureWindow, gpu::Renderer), String> {
        let window = win::FixtureWindow::create(piece).map_err(|e| format!("创建窗口失败: {e}"))?;
        let mut renderer = gpu::Renderer::new(&window, piece.w, piece.h, options.load == Load::Noise)
            .map_err(|e| format!("初始化 GPU 失败: {e}"))?;
        renderer.set_bar_segment((piece.x - region.x) as u32, region.w);
        Ok((window, renderer))
    };
    let (window, mut renderer) = match setup() {
        Ok(v) => {
            let _ = init.send(Ok(()));
            v
        }
        Err(e) => {
            let _ = init.send(Err(e.clone()));
            return Err(e);
        }
    };
    while shared.go.load(Ordering::Acquire) == GO_WAIT {
        std::thread::sleep(POLL);
    }
    if shared.go.load(Ordering::Acquire) == GO_ABORT {
        return Ok(Vec::new());
    }
    let origin = (piece.x, piece.y);
    let result = if leader {
        crate::run_loop(&window, &mut renderer, options, piece.w, piece.h, origin, Some(shared))
    } else {
        let hard_limit = Duration::from_secs_f64(options.seconds.min(MAX_SECONDS) + WATCHDOG_EXTRA_S);
        let start = Instant::now();
        let mut outcome = Ok(Vec::new());
        while !shared.stop.load(Ordering::Acquire) && start.elapsed() < hard_limit {
            window.pump();
            let seq = shared.seq.load(Ordering::Acquire);
            if seq == 0 {
                std::thread::sleep(POLL);
                continue;
            }
            let scene = build_scene_at(piece.w, piece.h, options.load, seq, origin);
            if let Err(e) = renderer.draw_and_present(&scene, seq) {
                outcome = Err(e);
                break;
            }
        }
        outcome
    };
    shared.stop.store(true, Ordering::Release);
    drop(renderer);
    drop(window);
    result
}

/// 双窗口跑完整段测试并返回主节拍的帧记录。
///
/// # 参数
/// - `pieces`：按 x 升序的子矩形，每块一个窗口，第一个为主节拍（至少一个）。
/// - `region`：完整跨屏区域（序号条按它的宽度切段）。
/// - `options`：命令行选项。
///
/// # 返回
/// 主节拍的帧记录（序号单调、统计语义与单窗口一致）；任一窗口失败返回原因。
pub fn run_dual(pieces: &[Rect], region: Rect, options: &Options) -> Result<Vec<FrameRecord>, String> {
    let shared = Shared { seq: AtomicU32::new(0), stop: AtomicBool::new(false), go: AtomicU8::new(GO_WAIT) };
    let (tx, rx) = mpsc::channel();
    std::thread::scope(|scope| {
        let handles: Vec<_> = pieces
            .iter()
            .enumerate()
            .map(|(i, &piece)| {
                let (shared, tx) = (&shared, tx.clone());
                scope.spawn(move || worker(piece, region, options, shared, i == 0, &tx))
            })
            .collect();
        drop(tx);
        let init_errors: Vec<String> = rx.iter().take(pieces.len()).filter_map(Result::err).collect();
        let go = if init_errors.is_empty() { GO_RUN } else { GO_ABORT };
        shared.go.store(go, Ordering::Release);
        let mut records = Vec::new();
        let mut errors = init_errors;
        for (i, h) in handles.into_iter().enumerate() {
            match h.join() {
                Ok(Ok(r)) if i == 0 => records = r,
                Ok(Ok(_)) => {}
                Ok(Err(e)) => errors.push(e),
                Err(_) => errors.push("窗口线程崩溃".into()),
            }
        }
        match errors.into_iter().next() {
            Some(e) => Err(e),
            None => Ok(records),
        }
    })
}
