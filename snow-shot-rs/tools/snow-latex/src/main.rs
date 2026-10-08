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
use std::path::PathBuf;
use std::process::ExitCode;

/// 环境变量：onnxruntime 动态库路径。
const ENV_ORT_DYLIB: &str = "SNOW_ORT_DYLIB";
/// 失败退出码。
const EXIT_FAILED: u8 = 2;

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
    /// 编码器输出（形状与数据），`step` 用。
    context: Option<(Vec<usize>, Vec<f32>)>,
}

/// 加载一个模型。
fn load(path: &PathBuf) -> Result<Session, String> {
    Session::builder()
        .map_err(|e| e.to_string())?
        .commit_from_file(path)
        .map_err(|e| format!("cannot load {}: {e}", path.display()))
}

/// 一个会话的输入输出名列表（排错用）。
fn io_names(session: &Session) -> serde_json::Value {
    serde_json::json!({
        "in": session.inputs().iter().map(|i| i.name().to_string()).collect::<Vec<_>>(),
        "out": session.outputs().iter().map(|o| o.name().to_string()).collect::<Vec<_>>(),
    })
}

/// 取一个会话输出里第一个 `f32` 张量（形状与数据）。
fn first_output(
    outputs: &ort::session::SessionOutputs<'_>,
) -> Result<(Vec<i64>, Vec<f32>), String> {
    let (_, value) = outputs.iter().next().ok_or("model returned no output")?;
    let (shape, data) = value
        .try_extract_tensor::<f32>()
        .map_err(|e| format!("output: {e}"))?;
    Ok((shape.iter().copied().collect(), data.to_vec()))
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
        let tensor = Tensor::from_array((shape, data)).map_err(|e| e.to_string())?;
        let name = self.encoder.inputs()[0].name().to_string();
        let outputs = self
            .encoder
            .run(ort::inputs![name => tensor])
            .map_err(|e| format!("encoder: {e}"))?;
        let (shape, data) = first_output(&outputs)?;
        let dims: Vec<usize> = shape.iter().map(|d| *d as usize).collect();
        self.context = Some((dims, data));
        Ok(Response {
            ok: Some(true),
            shape: Some(shape),
            ..Response::default()
        })
    }

    /// 解码一步：整段 token 重算，只回最后一个位置的 logits。
    fn step(&mut self, tokens: Vec<i64>) -> Result<Response, String> {
        let (dims, ctx) = self.context.clone().ok_or("no encoded context")?;
        let len = tokens.len();
        if len == 0 {
            return Err("empty token sequence".into());
        }
        let ids = Tensor::from_array(([1usize, len], tokens)).map_err(|e| e.to_string())?;
        let mask =
            Tensor::from_array(([1usize, len], vec![true; len])).map_err(|e| e.to_string())?;
        let context = Tensor::from_array((dims, ctx)).map_err(|e| e.to_string())?;
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
        let (shape, data) = first_output(&outputs)?;
        let vocab = *shape.last().ok_or("decoder output has no shape")? as usize;
        if vocab == 0 || data.len() < vocab {
            return Err("decoder output is empty".into());
        }
        Ok(Response {
            logits: Some(data[data.len() - vocab..].to_vec()),
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
    let mut engine = Engine {
        resizer: load(&args.resizer)?,
        encoder: load(&args.encoder)?,
        decoder: load(&args.decoder)?,
        context: None,
    };
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
