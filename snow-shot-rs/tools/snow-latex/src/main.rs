//! 公式识别工作进程：托管 RapidLaTeXOCR 的三个 ONNX 会话（image_resizer / encoder / decoder），
//! 只做张量推理，预处理、解码循环与后处理都在主程序里（纯函数，可离屏测试）。
//!
//! 协议（一行一个 JSON，主程序发请求、本进程回应；stdin 关闭即退出，不常驻）：
//! - 命令行：`--resizer <onnx> --encoder <onnx> --decoder <onnx> --ort <onnxruntime.dll>`
//!   （`--ort` 缺省读环境变量 `SNOW_ORT_DYLIB`）。
//! - 启动完成先输出 `{"ready":true,"io":{...}}`（各模型的输入输出名，排错用）。
//! - `{"op":"resize","shape":[1,1,H,W],"data":[...]}` -> `{"data":[...]}`：宽度分类器的整段输出。
//! - `{"op":"encode","shape":[1,1,H,W],"data":[...]}` -> `{"ok":true,"shape":[...]}`：编码并把上下文留在本进程。
//! - `{"op":"step","tokens":[...]}` -> `{"logits":[...]}`：用留存的上下文解码一步，只回最后一个位置的 logits。
//! - 任何失败 -> `{"error":"..."}`，进程继续等待下一条请求。

use ort::session::Session;
use ort::value::Tensor;
use serde::{Deserialize, Serialize};
use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::{Duration, Instant};

/// 环境变量：onnxruntime 动态库路径。
const ENV_ORT_DYLIB: &str = "SNOW_ORT_DYLIB";
/// 环境变量：设为非空即向 stderr 输出分段耗时（默认静默，避免污染宿主的错误尾部）。
const ENV_TIMING: &str = "SNOW_LATEX_TIMING";
/// 耗时日志行前缀。
const TIMING_PREFIX: &str = "[latex-timing]";
/// 失败退出码。
const EXIT_FAILED: u8 = 2;
/// 带 KV cache 的解码器输入名前缀（RapidLaTeXOCR 现有导出不带）。
const PAST_KV_PREFIX: &str = "past_key_values";

/// 是否开启耗时日志。
fn timing_enabled() -> bool {
    std::env::var_os(ENV_TIMING).is_some_and(|v| !v.is_empty())
}

/// 输出一行耗时日志（仅开启时）。
fn timing_log(message: &str) {
    if timing_enabled() {
        eprintln!("{TIMING_PREFIX} {message}");
    }
}

/// 进程内存计数（对应 Win32 `PROCESS_MEMORY_COUNTERS`）。
#[cfg(windows)]
#[repr(C)]
struct ProcessMemoryCounters {
    /// 结构体大小。
    cb: u32,
    /// 缺页次数。
    page_fault_count: u32,
    /// 工作集峰值（字节）。
    peak_working_set_size: usize,
    /// 当前工作集（字节）。
    working_set_size: usize,
    /// 四项池配额，本进程不用。
    pool_quota: [usize; 4],
    /// 当前提交量（字节）。
    pagefile_usage: usize,
    /// 提交量峰值（字节）。
    peak_pagefile_usage: usize,
}

#[cfg(windows)]
unsafe extern "system" {
    /// 当前进程伪句柄。
    fn GetCurrentProcess() -> isize;
    /// 读取进程内存计数。
    fn K32GetProcessMemoryInfo(
        process: isize,
        counters: *mut ProcessMemoryCounters,
        cb: u32,
    ) -> i32;
}

/// 当前进程的（工作集峰值, 工作集）字节数；读取失败或非 Windows 返回 `None`。
#[cfg(windows)]
fn memory_bytes() -> Option<(usize, usize)> {
    let mut c = ProcessMemoryCounters {
        cb: std::mem::size_of::<ProcessMemoryCounters>() as u32,
        page_fault_count: 0,
        peak_working_set_size: 0,
        working_set_size: 0,
        pool_quota: [0; 4],
        pagefile_usage: 0,
        peak_pagefile_usage: 0,
    };
    // SAFETY: `c` 是合法的栈上结构体，`cb` 与其大小一致，伪句柄无需释放。
    let ok = unsafe { K32GetProcessMemoryInfo(GetCurrentProcess(), &mut c, c.cb) };
    (ok != 0).then_some((c.peak_working_set_size, c.working_set_size))
}

/// 非 Windows 无内存计数。
#[cfg(not(windows))]
fn memory_bytes() -> Option<(usize, usize)> {
    None
}

/// 输出一行内存日志（仅开启时）。
fn timing_log_memory(label: &str) {
    if !timing_enabled() {
        return;
    }
    if let Some((peak, now)) = memory_bytes() {
        timing_log(&format!(
            "{label} memory peak_ws={}MB ws={}MB",
            peak >> 20,
            now >> 20
        ));
    }
}

/// 解码器输入名里是否带 KV cache（`past_key_values*`）。
fn decoder_has_kv_cache(names: &[String]) -> bool {
    names.iter().any(|n| n.starts_with(PAST_KV_PREFIX))
}

/// 命令行参数。
struct Args {
    /// 宽度分类器模型。
    resizer: PathBuf,
    /// 编码器模型。
    encoder: PathBuf,
    /// 解码器模型。
    decoder: PathBuf,
    /// onnxruntime 动态库。
    ort: PathBuf,
}

/// 解析命令行；缺参数返回说明。
fn parse_args() -> Result<Args, String> {
    let (mut resizer, mut encoder, mut decoder) = (None, None, None);
    let mut ort = std::env::var_os(ENV_ORT_DYLIB)
        .filter(|v| !v.is_empty())
        .map(PathBuf::from);
    let mut it = std::env::args_os().skip(1);
    while let Some(flag) = it.next() {
        let value = it.next().map(PathBuf::from);
        match flag.to_str() {
            Some("--resizer") => resizer = value,
            Some("--encoder") => encoder = value,
            Some("--decoder") => decoder = value,
            Some("--ort") => ort = value,
            other => return Err(format!("unknown argument: {other:?}")),
        }
    }
    Ok(Args {
        resizer: resizer.ok_or("missing --resizer")?,
        encoder: encoder.ok_or("missing --encoder")?,
        decoder: decoder.ok_or("missing --decoder")?,
        ort: ort.ok_or("missing --ort (or SNOW_ORT_DYLIB)")?,
    })
}

/// 一条请求。
#[derive(Deserialize)]
struct Request {
    /// 操作名：`resize` / `encode` / `step`。
    op: String,
    /// 张量形状（`resize` / `encode`）。
    #[serde(default)]
    shape: Vec<usize>,
    /// 展平的 `f32` 数据（`resize` / `encode`）。
    #[serde(default)]
    data: Vec<f32>,
    /// 已生成的 token（`step`）。
    #[serde(default)]
    tokens: Vec<i64>,
}

/// 一条回应；只填与操作相关的字段。
#[derive(Serialize, Default)]
struct Response {
    /// 失败原因。
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
    /// 成功标记（`encode`）。
    #[serde(skip_serializing_if = "Option::is_none")]
    ok: Option<bool>,
    /// 形状（`encode`）。
    #[serde(skip_serializing_if = "Option::is_none")]
    shape: Option<Vec<i64>>,
    /// 数据（`resize`）。
    #[serde(skip_serializing_if = "Option::is_none")]
    data: Option<Vec<f32>>,
    /// 最后一个位置的 logits（`step`）。
    #[serde(skip_serializing_if = "Option::is_none")]
    logits: Option<Vec<f32>>,
}

/// 三个会话与留存的编码上下文。
struct Engine {
    /// 宽度分类器。
    resizer: Session,
    /// 编码器。
    encoder: Session,
    /// 解码器。
    decoder: Session,
    /// 编码器输出张量，`step` 每步按引用复用，不再逐步拷贝。
    context: Option<Tensor<f32>>,
    /// 本图已解码步数（仅计时日志用）。
    steps: u32,
    /// 本图解码累计耗时（仅计时日志用）。
    step_time: Duration,
}

/// 加载一个模型。
fn load(path: &Path) -> Result<Session, String> {
    Session::builder()
        .map_err(|e| e.to_string())?
        .commit_from_file(path)
        .map_err(|e| format!("cannot load {}: {e}", path.display()))
}

/// 按旧串行顺序（resizer、encoder、decoder）取并行加载的结果，先出错的优先报告。
fn ordered<T>(
    resizer: Result<T, String>,
    encoder: Result<T, String>,
    decoder: Result<T, String>,
) -> Result<(T, T, T), String> {
    Ok((resizer?, encoder?, decoder?))
}

/// 并行加载三个会话并记录各自耗时；错误优先级与旧串行一致。
fn load_all(args: &Args) -> Result<(Session, Session, Session), String> {
    let timed = |name: &'static str, path: &Path| {
        let start = Instant::now();
        let result = load(path);
        timing_log(&format!("load {name} {}ms", start.elapsed().as_millis()));
        result
    };
    let (resizer, encoder, decoder) = std::thread::scope(|s| {
        let encoder = s.spawn(|| timed("encoder", &args.encoder));
        let decoder = s.spawn(|| timed("decoder", &args.decoder));
        let resizer = timed("resizer", &args.resizer);
        let join = |h: std::thread::ScopedJoinHandle<'_, Result<Session, String>>| {
            h.join().unwrap_or_else(|p| std::panic::resume_unwind(p))
        };
        (resizer, join(encoder), join(decoder))
    });
    ordered(resizer, encoder, decoder)
}

/// 一个会话的输入输出名列表（排错用）。
fn io_names(session: &Session) -> serde_json::Value {
    serde_json::json!({
        "in": session.inputs().iter().map(|i| i.name().to_string()).collect::<Vec<_>>(),
        "out": session.outputs().iter().map(|o| o.name().to_string()).collect::<Vec<_>>(),
    })
}

/// 在借用状态下读取会话输出里第一个 `f32` 张量，把形状与数据交给 `f`，只拷贝 `f` 需要的部分。
fn with_first_output<R>(
    outputs: &ort::session::SessionOutputs<'_>,
    f: impl FnOnce(Vec<i64>, &[f32]) -> R,
) -> Result<R, String> {
    let (_, value) = outputs.iter().next().ok_or("model returned no output")?;
    let (shape, data) = value
        .try_extract_tensor::<f32>()
        .map_err(|e| format!("output: {e}"))?;
    Ok(f(shape.iter().copied().collect(), data))
}

/// 取一个会话输出里第一个 `f32` 张量（形状与整段数据拷贝）。
fn first_output(
    outputs: &ort::session::SessionOutputs<'_>,
) -> Result<(Vec<i64>, Vec<f32>), String> {
    with_first_output(outputs, |shape, data| (shape, data.to_vec()))
}

/// 开始新图编码时先使旧上下文失效，仅成功后安装新上下文。
fn replace_context<T, R>(
    context: &mut Option<T>,
    encode: impl FnOnce() -> Result<(T, R), String>,
) -> Result<R, String> {
    *context = None;
    let (value, response) = encode()?;
    *context = Some(value);
    Ok(response)
}

/// 缺少本次图像的编码结果时禁止解码。
fn encoded_context<T>(context: &Option<T>) -> Result<&T, String> {
    context.as_ref().ok_or_else(|| "no encoded context".into())
}

impl Engine {
    /// 宽度分类：返回整段输出。
    fn resize(&mut self, shape: Vec<usize>, data: Vec<f32>) -> Result<Response, String> {
        let tensor = Tensor::from_array((shape, data)).map_err(|e| e.to_string())?;
        let name = self.resizer.inputs()[0].name().to_string();
        let outputs = self
            .resizer
            .run(ort::inputs![name => tensor])
            .map_err(|e| format!("resizer: {e}"))?;
        let (_, data) = first_output(&outputs)?;
        Ok(Response {
            data: Some(data),
            ..Response::default()
        })
    }

    /// 编码并留存上下文。
    fn encode(&mut self, shape: Vec<usize>, data: Vec<f32>) -> Result<Response, String> {
        self.log_decode_summary();
        let start = Instant::now();
        let result = replace_context(&mut self.context, || {
            let tensor = Tensor::from_array((shape, data)).map_err(|e| e.to_string())?;
            let name = self
                .encoder
                .inputs()
                .first()
                .ok_or("encoder has no input")?
                .name()
                .to_string();
            let outputs = self
                .encoder
                .run(ort::inputs![name => tensor])
                .map_err(|e| format!("encoder: {e}"))?;
            let (shape, data) = first_output(&outputs)?;
            let dims: Vec<usize> = shape.iter().map(|d| *d as usize).collect();
            let context = Tensor::from_array((dims, data)).map_err(|e| e.to_string())?;
            Ok((
                context,
                Response {
                    ok: Some(true),
                    shape: Some(shape),
                    ..Response::default()
                },
            ))
        });
        timing_log(&format!("encode {}ms", start.elapsed().as_millis()));
        result
    }

    /// 输出上一张图的解码步数与总耗时并清零（仅开启计时时）。
    fn log_decode_summary(&mut self) {
        if self.steps > 0 {
            timing_log(&format!(
                "decode steps={} total={}ms",
                self.steps,
                self.step_time.as_millis()
            ));
        }
        self.steps = 0;
        self.step_time = Duration::ZERO;
    }

    /// 解码一步并累计步数与耗时。
    fn step(&mut self, tokens: Vec<i64>) -> Result<Response, String> {
        let start = Instant::now();
        let result = self.step_inner(tokens);
        self.steps += 1;
        self.step_time += start.elapsed();
        result
    }

    /// 解码一步：整段 token 重算，只回最后一个位置的 logits。
    fn step_inner(&mut self, tokens: Vec<i64>) -> Result<Response, String> {
        let context = encoded_context(&self.context)?;
        let len = tokens.len();
        if len == 0 {
            return Err("empty token sequence".into());
        }
        let ids = Tensor::from_array(([1usize, len], tokens)).map_err(|e| e.to_string())?;
        let mask =
            Tensor::from_array(([1usize, len], vec![true; len])).map_err(|e| e.to_string())?;
        let names: Vec<String> = self
            .decoder
            .inputs()
            .iter()
            .map(|i| i.name().to_string())
            .collect();
        if names.len() < 3 {
            return Err(format!("decoder has {} inputs, expected 3", names.len()));
        }
        let outputs = self
            .decoder
            .run(ort::inputs![
                names[0].as_str() => ids,
                names[1].as_str() => mask,
                names[2].as_str() => context
            ])
            .map_err(|e| format!("decoder: {e}"))?;
        let logits = with_first_output(&outputs, |shape, data| {
            let vocab = *shape.last().ok_or("decoder output has no shape")? as usize;
            if vocab == 0 || data.len() < vocab {
                return Err("decoder output is empty".to_string());
            }
            // 只拷贝最后一个位置的 logits，不复制整段输出
            Ok(data[data.len() - vocab..].to_vec())
        })??;
        Ok(Response {
            logits: Some(logits),
            ..Response::default()
        })
    }

    /// 处理一条请求。
    fn handle(&mut self, request: Request) -> Result<Response, String> {
        match request.op.as_str() {
            "resize" => self.resize(request.shape, request.data),
            "encode" => self.encode(request.shape, request.data),
            "step" => self.step(request.tokens),
            other => Err(format!("unknown op: {other}")),
        }
    }
}

/// 写一行 JSON 并刷新；管道断了返回 `false`。
fn send(out: &mut impl Write, value: &impl Serialize) -> bool {
    serde_json::to_writer(&mut *out, value).is_ok()
        && out.write_all(b"\n").is_ok()
        && out.flush().is_ok()
}

/// 主体：加载模型、报告就绪、循环处理请求。
fn run() -> Result<(), String> {
    let args = parse_args()?;
    ort::init_from(&args.ort)
        .map_err(|e| format!("cannot load {}: {e}", args.ort.display()))?
        .with_telemetry(false)
        .commit();
    let load_start = Instant::now();
    let (resizer, encoder, decoder) = load_all(&args)?;
    let mut engine = Engine {
        resizer,
        encoder,
        decoder,
        context: None,
        steps: 0,
        step_time: Duration::ZERO,
    };
    timing_log(&format!(
        "load total {}ms",
        load_start.elapsed().as_millis()
    ));
    timing_log_memory("after-load");
    let decoder_inputs: Vec<String> = engine
        .decoder
        .inputs()
        .iter()
        .map(|i| i.name().to_string())
        .collect();
    timing_log(&format!(
        "decoder kv_cache={}",
        decoder_has_kv_cache(&decoder_inputs)
    ));
    let stdout = std::io::stdout();
    let mut out = stdout.lock();
    let ready = serde_json::json!({
        "ready": true,
        "io": {
            "resizer": io_names(&engine.resizer),
            "encoder": io_names(&engine.encoder),
            "decoder": io_names(&engine.decoder),
        }
    });
    if !send(&mut out, &ready) {
        return Ok(());
    }
    for line in std::io::stdin().lock().lines() {
        let line = line.map_err(|e| format!("cannot read stdin: {e}"))?;
        if line.trim().is_empty() {
            continue;
        }
        let response = serde_json::from_str::<Request>(&line)
            .map_err(|e| format!("bad request: {e}"))
            .and_then(|request| engine.handle(request))
            .unwrap_or_else(|error| Response {
                error: Some(error),
                ..Response::default()
            });
        if !send(&mut out, &response) {
            break;
        }
    }
    engine.log_decode_summary();
    timing_log_memory("exit");
    Ok(())
}

/// 入口：失败写 stderr 并以 2 退出。
fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("{e}");
            ExitCode::from(EXIT_FAILED)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A 成功、B 失败后禁止沿用 A 解码，C 成功才恢复。
    #[test]
    fn failed_encode_invalidates_previous_image() {
        let mut context = None;
        replace_context(&mut context, || Ok(("A", ()))).unwrap();
        assert_eq!(*encoded_context(&context).unwrap(), "A");
        assert!(replace_context::<_, ()>(&mut context, || Err("B failed".into())).is_err());
        assert_eq!(encoded_context(&context).unwrap_err(), "no encoded context");
        replace_context(&mut context, || Ok(("C", ()))).unwrap();
        assert_eq!(*encoded_context(&context).unwrap(), "C");
    }

    /// 并行加载的错误优先级与旧串行一致：resizer > encoder > decoder。
    #[test]
    fn ordered_error_precedence_matches_serial() {
        let err = |m: &str| Err::<u8, String>(m.into());
        assert_eq!(ordered(err("r"), err("e"), err("d")).unwrap_err(), "r");
        assert_eq!(ordered(Ok(1), err("e"), err("d")).unwrap_err(), "e");
        assert_eq!(ordered(Ok(1), Ok(2), err("d")).unwrap_err(), "d");
        assert_eq!(ordered(Ok(1), Ok(2), Ok(3)).unwrap(), (1, 2, 3));
    }

    /// 只有带 `past_key_values*` 输入才判定为有 KV cache。
    #[test]
    fn kv_cache_detection() {
        let plain = ["x".to_string(), "mask".to_string(), "context".to_string()];
        assert!(!decoder_has_kv_cache(&plain));
        let kv = ["input_ids".to_string(), "past_key_values.0.key".to_string()];
        assert!(decoder_has_kv_cache(&kv));
    }
}
