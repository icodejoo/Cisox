//! Snow Shot 语音转文字工作进程。
//!
//! 由主程序拉起：从 stdin 读行协议命令，向 stdout 回报事件（见 `snow-stt-protocol`）。
//! 一个进程只做一次识别；STOP/CANCEL/stdin 断开都会结束进程，退出即释放全部资源。
//! 测试用参数：`--wav <文件>` 用 16kHz 单声道 wav 代替麦克风，`--wav-pad-ms <n>` 在末尾补静音，
//! `--stats` 结束时向 stderr 打印耗时统计，`--probe-system [--probe-lang <语言>]` 只探测系统语音能力后退出。

mod backend;
mod session;
mod sherpa;
mod source;
mod system;

use std::io::{BufRead, Write};
use std::sync::mpsc;
use std::time::Instant;

use snow_stt_protocol::{BackendKind, Command, Event, SystemError};

use backend::SttBackend;
use session::{Ctl, SessionEnd, SessionStats, run_session, wait_for_start};
use source::{AudioSource, ClockSource, MicSource, WavSource};

/// 进程退出码：正常结束。
const EXIT_OK: i32 = 0;
/// 进程退出码：识别失败。
const EXIT_FAILED: i32 = 1;

/// 系统语音后端的节拍间隔。
const SYSTEM_TICK: std::time::Duration = std::time::Duration::from_millis(100);

/// 命令行参数（均为测试用途）。
struct Args {
    /// 只做系统语音能力探测并退出（附带语言提示，默认 auto）。
    probe_system: Option<String>,
    /// 用 wav 代替麦克风。
    wav: Option<String>,
    /// wav 末尾补的静音毫秒数。
    wav_pad_ms: u32,
    /// 结束时打印统计。
    stats: bool,
}

/// 解析命令行参数；未知参数忽略。
fn parse_args() -> Args {
    let mut args = Args {
        probe_system: None,
        wav: None,
        wav_pad_ms: 0,
        stats: false,
    };
    let mut it = std::env::args().skip(1);
    while let Some(a) = it.next() {
        match a.as_str() {
            "--wav" => args.wav = it.next(),
            "--wav-pad-ms" => args.wav_pad_ms = it.next().and_then(|v| v.parse().ok()).unwrap_or(0),
            "--stats" => args.stats = true,
            "--probe-system" => args.probe_system = Some("auto".into()),
            "--probe-lang" => {
                args.probe_system = Some(it.next().unwrap_or_else(|| "auto".into()));
            }
            _ => {}
        }
    }
    args
}

/// 向 stdout 写一条事件并刷新；管道断开时静默（随后 stdin EOF 会触发退出）。
fn emit(event: &Event) {
    let mut out = std::io::stdout().lock();
    let _ = writeln!(out, "{}", event.to_line());
    let _ = out.flush();
}

/// 启动 stdin 读取线程：逐行解析成 `Ctl`，EOF 时发 `Ctl::Eof`。
fn spawn_stdin_reader() -> mpsc::Receiver<Ctl> {
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let stdin = std::io::stdin();
        for line in stdin.lock().lines() {
            let Ok(line) = line else { break };
            // 某些宿主的管道会在首行前带 UTF-8 BOM，先去掉
            let line = line.trim_start_matches('\u{feff}').to_string();
            if line.trim().is_empty() {
                continue;
            }
            let msg = match Command::parse(&line) {
                Ok(c) => Ctl::Cmd(c),
                Err(e) => Ctl::BadLine(e.to_string()),
            };
            if tx.send(msg).is_err() {
                return;
            }
        }
        let _ = tx.send(Ctl::Eof);
    });
    rx
}

/// 打印统计到 stderr：加载耗时、块数、p50/p95/最大耗时与 RTF。
fn print_stats(load_ms: u128, stats: &SessionStats) {
    let mut v = stats.chunk_micros.clone();
    v.sort_unstable();
    let pick = |q: f64| {
        v.get(((v.len() as f64 - 1.0) * q) as usize)
            .copied()
            .unwrap_or(0) as f64
            / 1000.0
    };
    let audio_s = stats.fed_samples as f64 / 16_000.0;
    let total_s: f64 = v.iter().sum::<u64>() as f64 / 1e6;
    eprintln!(
        "stats: load_ms={load_ms} chunks={} audio_s={audio_s:.2} p50_ms={:.1} p95_ms={:.1} max_ms={:.1} rtf={:.3}",
        v.len(),
        pick(0.5),
        pick(0.95),
        pick(1.0),
        if audio_s > 0.0 {
            total_s / audio_s
        } else {
            0.0
        },
    );
}

/// 进程入口：READY → 等 START → 加载模型 → 打开来源 → 跑会话 → 退出。
fn main() {
    let args = parse_args();
    if let Some(lang) = &args.probe_system {
        println!("{}", system::probe_report(lang));
        return;
    }
    let ctl = spawn_stdin_reader();
    emit(&Event::Ready);

    let Some(req) = wait_for_start(&ctl, &mut |e| emit(&e)) else {
        std::process::exit(EXIT_OK);
    };

    let t0 = Instant::now();
    let loaded: Result<Box<dyn SttBackend>, String> = match req.backend {
        BackendKind::Local => {
            sherpa::SherpaBackend::load(&req).map(|b| Box::new(b) as Box<dyn SttBackend>)
        }
        // 系统语音：识别器自己占用默认麦克风
        BackendKind::System if args.wav.is_some() => {
            Err(SystemError::Other.to_error_text("系统语音后端不支持 --wav，只能用麦克风"))
        }
        BackendKind::System => {
            system::SystemBackend::load(&req).map(|b| Box::new(b) as Box<dyn SttBackend>)
        }
    };
    let mut backend = match loaded {
        Ok(b) => b,
        Err(why) => {
            emit(&Event::Error(why));
            std::process::exit(EXIT_FAILED);
        }
    };
    let load_ms = t0.elapsed().as_millis();

    // 本地模型加载完才开麦克风，避免把加载期间的旧音频喂进去；系统语音只需节拍
    let opened: Result<Box<dyn AudioSource>, String> = match (&args.wav, req.backend) {
        (_, BackendKind::System) => Ok(Box::new(ClockSource::new(SYSTEM_TICK))),
        (Some(p), _) => {
            WavSource::open(p, args.wav_pad_ms).map(|s| Box::new(s) as Box<dyn AudioSource>)
        }
        (None, _) => MicSource::open().map(|s| Box::new(s) as Box<dyn AudioSource>),
    };
    let mut source = match opened {
        Ok(s) => s,
        Err(why) => {
            emit(&Event::Error(why));
            std::process::exit(EXIT_FAILED);
        }
    };

    let (end, stats) = run_session(
        backend.as_mut(),
        source.as_mut(),
        &ctl,
        &mut |e| emit(&e),
        req.max_seconds,
    );
    if args.stats {
        print_stats(load_ms, &stats);
    }
    // 先释放采集与模型，再退出
    drop(source);
    drop(backend);
    std::process::exit(match end {
        SessionEnd::Failed => EXIT_FAILED,
        _ => EXIT_OK,
    });
}
