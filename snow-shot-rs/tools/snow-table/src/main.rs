//! 表格结构识别工作进程：加载 SLANet_plus 的 ONNX，对一张预处理好的图跑一次推理。
//!
//! 协议（一次性，跑完退出，不常驻）：
//! - 命令行：`--model <onnx 路径> --ort <onnxruntime.dll 路径>`（`--ort` 缺省读环境变量 `SNOW_ORT_DYLIB`）。
//! - stdin：`1 x 3 x 488 x 488` 的 `f32` 小端原始字节（CHW，已归一化与补边）。
//! - stdout：一行 JSON `{"character": "...", "outputs": [{"name", "shape", "data"}]}`，
//!   `character` 是模型元数据里的结构词表，输出顺序与模型一致。
//! - 失败：stderr 写原因，退出码 2。
//!
//! 结构解码、与文字框的匹配都在主程序里做（纯函数，可离屏测试），这里不含任何业务逻辑。

use ort::session::Session;
use ort::value::Tensor;
use serde::Serialize;
use std::io::{Read, Write};
use std::path::PathBuf;
use std::process::ExitCode;

/// 输入边长（SLANet_plus 固定 488）。
const SIDE: usize = 488;
/// 输入通道数。
const CHANNELS: usize = 3;
/// 环境变量：onnxruntime 动态库路径。
const ENV_ORT_DYLIB: &str = "SNOW_ORT_DYLIB";
/// 模型元数据里的词表键。
const CHARACTER_KEY: &str = "character";
/// 失败退出码。
const EXIT_FAILED: u8 = 2;

/// 一个输出张量。
#[derive(Serialize)]
struct OutputTensor {
    /// 输出名。
    name: String,
    /// 形状。
    shape: Vec<i64>,
    /// 展平后的 `f32` 数据。
    data: Vec<f32>,
}

/// 整个响应。
#[derive(Serialize)]
struct Response {
    /// 结构词表（按行分隔）。
    character: String,
    /// 全部输出张量。
    outputs: Vec<OutputTensor>,
}

/// 命令行参数。
struct Args {
    /// 模型路径。
    model: PathBuf,
    /// onnxruntime 动态库路径。
    ort: PathBuf,
}

/// 解析命令行；缺参数返回说明。
fn parse_args() -> Result<Args, String> {
    let mut model = None;
    let mut ort = std::env::var_os(ENV_ORT_DYLIB)
        .filter(|v| !v.is_empty())
        .map(PathBuf::from);
    let mut it = std::env::args_os().skip(1);
    while let Some(flag) = it.next() {
        let value = it.next().map(PathBuf::from);
        match flag.to_str() {
            Some("--model") => model = value,
            Some("--ort") => ort = value,
            other => return Err(format!("unknown argument: {other:?}")),
        }
    }
    Ok(Args {
        model: model.ok_or("missing --model")?,
        ort: ort.ok_or("missing --ort (or SNOW_ORT_DYLIB)")?,
    })
}

/// 读满 stdin 并转成 `f32` 序列；长度不对报错。
fn read_input() -> Result<Vec<f32>, String> {
    let mut bytes = Vec::new();
    std::io::stdin()
        .lock()
        .read_to_end(&mut bytes)
        .map_err(|e| format!("cannot read stdin: {e}"))?;
    let expected = CHANNELS * SIDE * SIDE * 4;
    if bytes.len() != expected {
        return Err(format!(
            "input is {} bytes, expected {expected}",
            bytes.len()
        ));
    }
    Ok(bytes
        .chunks_exact(4)
        .map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]]))
        .collect())
}

/// 加载模型、推理并组装响应。
fn run(args: &Args, input: Vec<f32>) -> Result<Response, String> {
    ort::init_from(&args.ort)
        .map_err(|e| format!("cannot load {}: {e}", args.ort.display()))?
        .with_telemetry(false)
        .commit();
    let mut session = Session::builder()
        .map_err(|e| e.to_string())?
        .commit_from_file(&args.model)
        .map_err(|e| format!("cannot load {}: {e}", args.model.display()))?;
    let character = session
        .metadata()
        .map_err(|e| e.to_string())?
        .custom(CHARACTER_KEY)
        .unwrap_or_default();
    let tensor =
        Tensor::from_array(([1usize, CHANNELS, SIDE, SIDE], input)).map_err(|e| e.to_string())?;
    let input_name = session.inputs()[0].name().to_string();
    let outputs = session
        .run(ort::inputs![input_name => tensor])
        .map_err(|e| format!("inference failed: {e}"))?;
    let mut result = Vec::new();
    for (name, value) in outputs.iter() {
        let (shape, data) = value
            .try_extract_tensor::<f32>()
            .map_err(|e| format!("output {name}: {e}"))?;
        result.push(OutputTensor {
            name: name.to_string(),
            shape: shape.iter().copied().collect(),
            data: data.to_vec(),
        });
    }
    Ok(Response {
        character,
        outputs: result,
    })
}

/// 入口：成功把 JSON 写到 stdout；失败写 stderr 并以 2 退出。
fn main() -> ExitCode {
    let result = parse_args()
        .and_then(|args| read_input().and_then(|input| run(&args, input)))
        .and_then(|response| serde_json::to_vec(&response).map_err(|e| e.to_string()));
    match result {
        Ok(json) => {
            let mut out = std::io::stdout().lock();
            if out.write_all(&json).and_then(|()| out.flush()).is_err() {
                return ExitCode::from(EXIT_FAILED);
            }
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("{e}");
            ExitCode::from(EXIT_FAILED)
        }
    }
}
