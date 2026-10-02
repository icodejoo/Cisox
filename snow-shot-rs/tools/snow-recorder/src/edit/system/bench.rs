//! 引擎基准（不进默认测试，需 `--ignored` 显式运行）。
//!
//! 父测试 `bench_engines` 现生成一段合成样片，然后对每个"操作 x 引擎"各起若干个子进程
//! （子进程 = 本测试可执行文件再跑 `bench_child`，并以低于正常的优先级运行），
//! 子进程里只跑一次编辑任务，量墙钟耗时和进程峰值工作集；父测试取耗时中位数与峰值最大值，打印 Markdown 表。
//! 峰值工作集含测试框架本身的基线（子进程会一并报告基线，便于扣除）。
//!
//! 运行（release 构建更有意义）：
//! `snow-recorder-<hash>.exe --ignored --exact edit::system::bench::bench_engines --nocapture`
//! 环境变量：`SNOW_BENCH_RUNS`（每组重复次数，默认 3）、`SNOW_BENCH_FRAMES`（样片帧数，默认 240）、
//! `SNOW_BENCH_OUT`（把表格另存到该文件）。结果只对合成样片有效，不可外推到真实录屏。

use std::os::windows::process::CommandExt;
use std::path::Path;
use std::process::Command;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::time::Instant;

use snow_recorder_protocol::{EditOp, EditRequest, EngineKind, ExtractMode, ImageFormat};
use windows::Win32::System::ProcessStatus::{GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS};
use windows::Win32::System::Threading::GetCurrentProcess;

use crate::edit::testclip::{self, Clip};
use crate::edit::{TaskCtl, execute};

/// 低于正常的进程优先级（`BELOW_NORMAL_PRIORITY_CLASS`）。
const BELOW_NORMAL: u32 = 0x0000_4000;
/// 子进程输出结果行的前缀。
const RESULT_PREFIX: &str = "BENCH_RESULT ";
/// 子进程用的环境变量：引擎名、操作名、输入、输出。
const ENV_ENGINE: &str = "SNOW_BENCH_ENGINE";
/// 环境变量：操作名。
const ENV_OP: &str = "SNOW_BENCH_OP";
/// 环境变量：输入样片路径。
const ENV_INPUT: &str = "SNOW_BENCH_INPUT";
/// 环境变量：输出路径。
const ENV_OUTPUT: &str = "SNOW_BENCH_OUTPUT";
/// 基准样片分辨率。
const CLIP_SIZE: (u32, u32) = (1920, 1080);
/// 基准样片帧率。
const CLIP_FPS: u32 = 30;

/// 基准里的操作名与对应的编辑操作。
const OPS: [&str; 4] = ["fps15", "scale720p", "png-250ms", "jpeg-250ms"];

/// 把操作名换成编辑操作。
///
/// # 参数
/// - `name`：`OPS` 里的名字。
fn op_of(name: &str) -> EditOp {
    match name {
        "fps15" => EditOp::ReduceFps { target_fps: 15 },
        "scale720p" => EditOp::Scale {
            width: 1280,
            height: 720,
        },
        "png-250ms" => EditOp::ExtractFrames {
            mode: ExtractMode::Interval { every_ms: 250 },
            format: ImageFormat::Png,
            quality: 90,
        },
        "jpeg-250ms" => EditOp::ExtractFrames {
            mode: ExtractMode::Interval { every_ms: 250 },
            format: ImageFormat::Jpeg,
            quality: 90,
        },
        other => panic!("未知基准操作 {other}"),
    }
}

/// 当前进程的峰值工作集（字节）。
fn peak_working_set() -> u64 {
    let mut counters = PROCESS_MEMORY_COUNTERS {
        cb: std::mem::size_of::<PROCESS_MEMORY_COUNTERS>() as u32,
        ..Default::default()
    };
    // SAFETY: counters 是局部有效结构，cb 与其大小一致。
    let ok = unsafe { GetProcessMemoryInfo(GetCurrentProcess(), &mut counters, counters.cb) };
    if ok.is_ok() {
        counters.PeakWorkingSetSize as u64
    } else {
        0
    }
}

/// 子进程：跑一次任务并打印结果行；没设环境变量（被当成普通 `--ignored` 触发）时直接跳过。
#[test]
#[ignore = "仅由 bench_engines 以子进程方式调用"]
fn bench_child() {
    let (Ok(engine), Ok(op), Ok(input), Ok(output)) = (
        std::env::var(ENV_ENGINE),
        std::env::var(ENV_OP),
        std::env::var(ENV_INPUT),
        std::env::var(ENV_OUTPUT),
    ) else {
        return;
    };
    let kind = EngineKind::parse(&engine).expect("引擎名");
    let req = EditRequest {
        engine: kind,
        op: op_of(&op),
        input: input.into(),
        output: output.into(),
    };
    let ctl = TaskCtl::new(Arc::new(AtomicBool::new(false)), Box::new(|_| {}));
    let baseline = peak_working_set();
    let started = Instant::now();
    let result = execute(&req, &ctl);
    let ms = started.elapsed().as_secs_f64() * 1000.0;
    let peak = peak_working_set();
    match result {
        Ok((report, used)) => println!(
            "{RESULT_PREFIX}ok engine={} ms={ms:.1} peak_mb={:.1} base_mb={:.1} frames={}",
            used.as_str(),
            peak as f64 / 1_048_576.0,
            baseline as f64 / 1_048_576.0,
            report.frames
        ),
        Err(e) => println!("{RESULT_PREFIX}err engine={engine} reason={e}"),
    }
}

/// 一次子进程运行的结果。
struct Sample {
    /// 耗时（毫秒）。
    ms: f64,
    /// 峰值工作集（MB）。
    peak_mb: f64,
    /// 基线（MB）。
    base_mb: f64,
    /// 输出帧（张）数。
    frames: u64,
}

/// 起一个子进程跑一次任务。
///
/// # 参数
/// - `engine`：引擎。
/// - `op`：操作名。
/// - `input`：样片。
/// - `output`：输出路径。
fn run_once(engine: EngineKind, op: &str, input: &Path, output: &Path) -> Result<Sample, String> {
    let _ = std::fs::remove_dir_all(output);
    let _ = std::fs::remove_file(output);
    let out = Command::new(std::env::current_exe().map_err(|e| e.to_string())?)
        .args([
            "--ignored",
            "--exact",
            "edit::system::bench::bench_child",
            "--nocapture",
            "--test-threads=1",
        ])
        .env(ENV_ENGINE, engine.as_str())
        .env(ENV_OP, op)
        .env(ENV_INPUT, input)
        .env(ENV_OUTPUT, output)
        .creation_flags(BELOW_NORMAL)
        .output()
        .map_err(|e| e.to_string())?;
    let text = String::from_utf8_lossy(&out.stdout);
    let line = text
        .lines()
        .find_map(|l| l.split_once(RESULT_PREFIX).map(|(_, rest)| rest))
        .ok_or_else(|| format!("子进程没有结果行: {text}"))?;
    if let Some(reason) = line.strip_prefix("err ") {
        return Err(reason.to_string());
    }
    let field = |key: &str| -> Result<String, String> {
        line.split_whitespace()
            .find_map(|kv| kv.strip_prefix(&format!("{key}=")))
            .map(str::to_string)
            .ok_or_else(|| format!("缺少字段 {key}: {line}"))
    };
    let num = |key: &str| -> Result<f64, String> {
        field(key)?
            .parse::<f64>()
            .map_err(|e| format!("{key}: {e}"))
    };
    let used = field("engine")?;
    if used != engine.as_str() {
        return Err(format!("实际引擎 {used} 与请求的 {} 不同", engine.as_str()));
    }
    Ok(Sample {
        ms: num("ms")?,
        peak_mb: num("peak_mb")?,
        base_mb: num("base_mb")?,
        frames: num("frames")? as u64,
    })
}

/// 读取整数环境变量。
fn env_num(key: &str, default: u32) -> u32 {
    std::env::var(key)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

/// 父测试：对每个操作和引擎各跑若干次，打印 Markdown 表。
#[test]
#[ignore = "基准，需显式 --ignored 运行"]
fn bench_engines() {
    let runs = env_num("SNOW_BENCH_RUNS", 3).max(1) as usize;
    let frames = env_num("SNOW_BENCH_FRAMES", 240);
    let dir = testclip::temp_dir("bench");
    let input = dir.join("clip.mp4");
    let clip = Clip {
        width: CLIP_SIZE.0,
        height: CLIP_SIZE.1,
        frames,
        fps: CLIP_FPS,
        gop: CLIP_FPS * 2,
        audio: true,
        ..Clip::default()
    };
    testclip::make_clip(&input, &clip).expect("生成基准样片");
    let size_mb = std::fs::metadata(&input).map_or(0.0, |m| m.len() as f64 / 1_048_576.0);
    let mut table = String::new();
    table.push_str(&format!(
        "样片：合成纯灰色帧（亮度逐帧递增） {}x{} @{}fps，{} 帧（{:.1}s），GOP {}，带 AAC 静音音轨，文件 {:.2} MB。每组 {} 次，取耗时中位数、峰值工作集最大值。\n\n",
        clip.width,
        clip.height,
        clip.fps,
        clip.frames,
        f64::from(clip.frames) / f64::from(clip.fps),
        clip.gop,
        size_mb,
        runs
    ));
    table.push_str("| 操作 | 引擎 | 耗时中位数 ms | 峰值工作集 MB | 基线 MB | 输出帧/张数 |\n|---|---|---|---|---|---|\n");
    for op in OPS {
        for engine in [EngineKind::System, EngineKind::Ffmpeg] {
            let output = if op.contains("png") || op.contains("jpeg") {
                dir.join(format!("out-{op}-{}", engine.as_str()))
            } else {
                dir.join(format!("out-{op}-{}.mp4", engine.as_str()))
            };
            let mut samples = Vec::new();
            let mut failure = None;
            for _ in 0..runs {
                match run_once(engine, op, &input, &output) {
                    Ok(s) => samples.push(s),
                    Err(e) => {
                        failure = Some(e);
                        break;
                    }
                }
            }
            if let Some(e) = failure {
                table.push_str(&format!(
                    "| {op} | {} | 失败: {} | | | |\n",
                    engine.as_str(),
                    e.replace('|', "/")
                ));
                continue;
            }
            samples.sort_by(|a, b| a.ms.total_cmp(&b.ms));
            let median = samples[samples.len() / 2].ms;
            let peak = samples.iter().map(|s| s.peak_mb).fold(0.0, f64::max);
            let base = samples.iter().map(|s| s.base_mb).fold(0.0, f64::max);
            table.push_str(&format!(
                "| {op} | {} | {median:.0} | {peak:.0} | {base:.0} | {} |\n",
                engine.as_str(),
                samples[0].frames
            ));
        }
    }
    println!("{table}");
    if let Ok(path) = std::env::var("SNOW_BENCH_OUT") {
        let _ = std::fs::write(path, &table);
    }
    let _ = std::fs::remove_dir_all(&dir);
}
