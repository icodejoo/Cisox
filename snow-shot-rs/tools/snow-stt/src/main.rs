//! Snow Shot 语音转文字工作进程。
//!
//! 由主程序拉起：从 stdin 读行协议命令，向 stdout 回报事件（见 `snow-stt-protocol`）。
//! 一个进程只做一次识别；STOP/CANCEL/stdin 断开都会结束进程，退出即释放全部资源。
//! 本地后端采麦克风时，START 一到就先开麦（独立线程，边开边载模型）：样本进有上限的环形缓存，
//! 开麦成功即发 `RECORDING`，模型就绪后快进识别积压再转实时（见 [`preroll`]），
//! 所以加载模型的几秒里说的话不会丢；系统语音后端自己占麦克风，流程不变。
//! 测试用参数：`--wav <文件>` 用 16kHz 单声道 wav 代替麦克风，`--wav-pad-ms <n>` 在末尾补静音，
//! `--wav-preroll` 让 wav 也走先开麦的环形缓存流程，`--load-delay-ms <n>` 在加载模型前人为等待
//! （二者合用可离线复现“加载很慢、积压追赶”），`--stats` 结束时向 stderr 打印耗时统计，
//! `--probe-system [--probe-lang <语言>]` 只探测系统语音能力后退出。

mod backend;
mod offline;
mod preroll;
mod session;
mod sherpa;
mod source;
mod system;

use std::io::{BufRead, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use snow_stt_protocol::{BackendKind, Command, Event, RecognitionMode, StartRequest, SystemError};

use backend::SttBackend;
use preroll::{PumpedSource, cap_samples};
use session::{Ctl, SessionEnd, SessionStats, run_session, wait_for_start};
use sherpa::SAMPLE_RATE;
use source::{AudioSource, ClockSource, MicSource, WavSource};

/// 进程退出码：正常结束。
const EXIT_OK: i32 = 0;
/// 进程退出码：识别失败。
const EXIT_FAILED: i32 = 1;

/// 系统语音后端的节拍间隔。
const SYSTEM_TICK: Duration = Duration::from_millis(100);

/// 模型就绪后最多等麦克风打开多久再进主循环（超时也照常进入，之后样本到了就能识别）。
const MIC_READY_WAIT: Duration = Duration::from_secs(10);

/// 是否已经发出过 ERROR：协议约定出错后进程随即退出，一个进程只报一次（采集线程与主线程都可能报）。
static ERROR_SENT: AtomicBool = AtomicBool::new(false);

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
    /// wav 也走先开麦的环形缓存流程（测试用）。
    wav_preroll: bool,
    /// 加载模型前人为等待的毫秒数（测试用，模拟加载很慢）。
    load_delay_ms: u64,
}

/// 解析命令行参数；未知参数忽略。
fn parse_args() -> Args {
    parse_args_from(std::env::args().skip(1))
}

/// 从参数序列解析（不含程序名）；未知参数忽略，数值非法按 0 / 缺省处理。
///
/// # 参数
/// - `items`：参数序列。
///
/// # 示例
/// ```ignore
/// let a = parse_args_from(["--wav", "a.wav", "--wav-preroll"].map(String::from));
/// assert!(a.wav_preroll);
/// ```
fn parse_args_from(items: impl IntoIterator<Item = String>) -> Args {
    let mut args = Args {
        probe_system: None,
        wav: None,
        wav_pad_ms: 0,
        stats: false,
        wav_preroll: false,
        load_delay_ms: 0,
    };
    let mut it = items.into_iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--wav" => args.wav = it.next(),
            "--wav-pad-ms" => args.wav_pad_ms = it.next().and_then(|v| v.parse().ok()).unwrap_or(0),
            "--wav-preroll" => args.wav_preroll = true,
            "--load-delay-ms" => {
                args.load_delay_ms = it.next().and_then(|v| v.parse().ok()).unwrap_or(0);
            }
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
/// ERROR 只发第一条（采集线程与主线程可能先后报同一个故障）。
fn emit(event: &Event) {
    if matches!(event, Event::Error(_)) && ERROR_SENT.swap(true, Ordering::SeqCst) {
        return;
    }
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

/// 发出错误事件并以失败码退出进程。
///
/// # 参数
/// - `why`：单行错误原因。
fn fail(why: String) -> ! {
    emit(&Event::Error(why));
    std::process::exit(EXIT_FAILED)
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
    let audio_s = stats.fed_samples as f64 / f64::from(SAMPLE_RATE);
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

/// 本次会话是否让 worker 在加载模型之前先开始采音（先开麦 + 环形缓存）。
///
/// 只有 worker 自己采样的本地后端才适用：麦克风，或测试用的 `--wav-preroll`；
/// 系统语音由识别器自己占麦克风，不能喂 PCM；普通 `--wav` 保持“加载完再读”的老流程。
///
/// # 参数
/// - `args`：命令行参数。
/// - `req`：START 请求。
fn wants_preroll(args: &Args, req: &StartRequest) -> bool {
    req.backend == BackendKind::Local && (args.wav.is_none() || args.wav_preroll)
}

/// 启动采集线程（先开麦）：开麦成功发 `RECORDING`，失败立刻报 `ERROR`（不必等模型加载完）。
///
/// # 参数
/// - `args`：命令行参数（决定来源是麦克风还是 wav）。
/// - `req`：START 请求（缓存上限随 `max_seconds`）。
fn start_preroll(args: &Args, req: &StartRequest) -> PumpedSource {
    let wav = args.wav.clone();
    let pad_ms = args.wav_pad_ms;
    let open = move || -> Result<Box<dyn AudioSource>, String> {
        match wav {
            Some(path) => {
                WavSource::open(&path, pad_ms).map(|s| Box::new(s) as Box<dyn AudioSource>)
            }
            None => MicSource::open().map(|s| Box::new(s) as Box<dyn AudioSource>),
        }
    };
    PumpedSource::start(
        open,
        |opened| match opened {
            Ok(()) => emit(&Event::Recording),
            Err(why) => emit(&Event::Error(why.clone())),
        },
        cap_samples(req.max_seconds),
    )
}

/// 进程入口：READY → 等 START → （本地后端先开麦）→ 加载模型 → 跑会话（先追赶积压）→ 退出。
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
    // 先开麦：采集线程与模型加载并行，加载期间的音频进环形缓存，不再丢
    let early: Option<Box<dyn AudioSource>> = wants_preroll(&args, &req)
        .then(|| Box::new(start_preroll(&args, &req)) as Box<dyn AudioSource>);
    if args.load_delay_ms > 0 {
        std::thread::sleep(Duration::from_millis(args.load_delay_ms));
    }
    let loaded: Result<Box<dyn SttBackend>, String> = match offline::check_mode_kind(&req)
        .map(|()| req.backend)
    {
        Err(why) => Err(why),
        // 离线整句识别：VAD 切句 + 后台线程解码（端点规则对其无意义）
        Ok(BackendKind::Local) if req.mode == RecognitionMode::Offline => {
            offline::OfflineSherpaBackend::load(&req).map(|b| Box::new(b) as Box<dyn SttBackend>)
        }
        Ok(BackendKind::Local) => {
            sherpa::SherpaBackend::load(&req).map(|b| Box::new(b) as Box<dyn SttBackend>)
        }
        // 系统语音：识别器自己占用默认麦克风
        Ok(BackendKind::System) if args.wav.is_some() => {
            Err(SystemError::Other.to_error_text("系统语音后端不支持 --wav，只能用麦克风"))
        }
        Ok(BackendKind::System) => {
            system::SystemBackend::load(&req).map(|b| Box::new(b) as Box<dyn SttBackend>)
        }
    };
    let mut backend = loaded.unwrap_or_else(|why| fail(why));
    let load_ms = t0.elapsed().as_millis();

    // 先开麦的来源直接沿用（积压由主循环按序快进消费）；否则加载完再开：系统语音只需节拍，
    // 普通 `--wav` 读文件
    let mut source: Box<dyn AudioSource> = match early {
        Some(source) => source,
        None => {
            let opened: Result<Box<dyn AudioSource>, String> = match (&args.wav, req.backend) {
                (_, BackendKind::System) => Ok(Box::new(ClockSource::new(SYSTEM_TICK))),
                (Some(p), _) => {
                    WavSource::open(p, args.wav_pad_ms).map(|s| Box::new(s) as Box<dyn AudioSource>)
                }
                (None, _) => MicSource::open().map(|s| Box::new(s) as Box<dyn AudioSource>),
            };
            opened.unwrap_or_else(|why| fail(why))
        }
    };

    // 先开麦的来源：等麦克风真正打开（RECORDING 已发出）再进主循环，保证 PONG（识别就绪）不早于“已在录音”；
    // 模型比麦克风先就绪时最多多等一会儿（麦克风打开本来就要 0.7~0.9 s），其它来源立即返回
    source.wait_ready(MIC_READY_WAIT);

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

#[cfg(test)]
mod tests {
    use super::*;

    /// 把字面量参数转成 `String` 序列。
    fn argv(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| s.to_string()).collect()
    }

    /// 新增的测试参数能解析：`--wav-preroll`、`--load-delay-ms`；非法数值回落 0；未知参数忽略。
    #[test]
    fn parses_preroll_test_flags() {
        let a = parse_args_from(argv(&[
            "--wav",
            "a.wav",
            "--wav-pad-ms",
            "3000",
            "--wav-preroll",
            "--load-delay-ms",
            "5000",
            "--stats",
            "--bogus",
        ]));
        assert_eq!(a.wav.as_deref(), Some("a.wav"));
        assert_eq!((a.wav_pad_ms, a.load_delay_ms), (3000, 5000));
        assert!(a.wav_preroll && a.stats && a.probe_system.is_none());

        let none = parse_args_from(argv(&[]));
        assert!(!none.wav_preroll && none.load_delay_ms == 0 && none.wav.is_none());
        let bad = parse_args_from(argv(&["--load-delay-ms", "x"]));
        assert_eq!(bad.load_delay_ms, 0);
        let probe = parse_args_from(argv(&["--probe-lang", "zh"]));
        assert_eq!(probe.probe_system.as_deref(), Some("zh"));
    }

    /// 先开麦只用于本地后端：麦克风与 `--wav-preroll` 走缓存；普通 wav 与系统语音保持老流程。
    #[test]
    fn preroll_applies_to_local_mic_and_opt_in_wav_only() {
        let local = StartRequest::default();
        let system = StartRequest {
            backend: BackendKind::System,
            ..StartRequest::default()
        };
        let mic = parse_args_from(argv(&[]));
        let wav = parse_args_from(argv(&["--wav", "a.wav"]));
        let wav_preroll = parse_args_from(argv(&["--wav", "a.wav", "--wav-preroll"]));
        assert!(wants_preroll(&mic, &local));
        assert!(!wants_preroll(&wav, &local));
        assert!(wants_preroll(&wav_preroll, &local));
        // 系统语音自己占麦克风，任何情况都不走缓存
        assert!(!wants_preroll(&mic, &system));
        assert!(!wants_preroll(&wav_preroll, &system));
    }
}
