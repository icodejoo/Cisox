//! 表格识别服务：拉起 `snow-table` 工作进程做一次结构推理，再与 OCR 文字块合并成表格。
//!
//! 流程：图 → 预处理 → 工作进程（SLANet_plus，ORT）→ 结构解码 → 与现有 OCR 的文字框匹配 → HTML / Markdown / TSV。
//! 工作进程按需拉起、跑完即退出，不常驻；推理以外的逻辑都在 [`crate::table_structure`]（纯函数）。
//! 识别是阻塞调用，必须在后台线程里执行。

use crate::ocr_client::OcrError;
use crate::ocr_download::quiet_command;
use crate::ocr_service::OcrResult;
use crate::table_assets::{TableAssets, TableUnavailable};
use crate::table_structure::{
    OcrPiece, TableTexts, WorkerOutput, assemble_table, parse_worker_output, prepare_input,
};
use std::io::{Read, Write};
use std::process::Stdio;
use std::time::{Duration, Instant};

/// 工作进程的最长运行时间（含加载模型）。
pub const WORKER_TIMEOUT: Duration = Duration::from_secs(60);
/// 轮询工作进程是否退出的间隔。
const POLL_INTERVAL: Duration = Duration::from_millis(20);
/// 保留的 stderr 末尾字节数（给错误提示用）。
const STDERR_TAIL: usize = 400;

/// 表格识别失败的原因。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TableError {
    /// 组件没准备好（引导下载）。
    Unavailable(TableUnavailable),
    /// 拉起工作进程失败。
    Spawn(String),
    /// 工作进程失败退出（附 stderr 末尾）。
    Died(String),
    /// 工作进程超时。
    Timeout,
    /// 输出不是预期格式。
    Protocol(String),
    /// 输入图像不合法。
    InvalidImage,
    /// 图里没找到表格。
    NoTable,
}

impl From<TableError> for OcrError {
    /// 并入 OCR 错误体系，让现有覆盖窗的失败展示与下载引导直接可用。
    fn from(e: TableError) -> Self {
        match e {
            TableError::Unavailable(u) => Self::TableUnavailable(u),
            TableError::Spawn(d) => Self::SpawnFailed(d),
            TableError::Died(d) | TableError::Protocol(d) => Self::Table(d),
            TableError::Timeout => Self::Table("the table worker timed out".to_string()),
            TableError::InvalidImage => Self::InvalidImage("empty or malformed image".to_string()),
            TableError::NoTable => Self::NoTable,
        }
    }
}

/// 结构推理的执行方式（便于测试注入假实现）。
pub trait StructureRunner {
    /// 对预处理好的张量做一次推理。
    ///
    /// # 参数
    /// - `tensor`：`1 x 3 x 488 x 488` 展平的 CHW 张量。
    ///
    /// # 返回
    /// 模型输出与词表。
    fn run(&self, tensor: &[f32]) -> Result<WorkerOutput, TableError>;
}

/// 真实进程：每次推理拉起一个 `snow-table` 子进程。
#[derive(Debug, Clone)]
pub struct ProcessRunner {
    /// 已就绪的资产。
    assets: TableAssets,
}

impl ProcessRunner {
    /// 创建执行器。
    ///
    /// # 参数
    /// - `assets`：已就绪的资产路径。
    pub fn new(assets: TableAssets) -> Self {
        Self { assets }
    }
}

/// 在线程里读完一个管道。
fn drain<R: Read + Send + 'static>(mut pipe: R) -> std::thread::JoinHandle<Vec<u8>> {
    std::thread::spawn(move || {
        let mut buf = Vec::new();
        let _ = pipe.read_to_end(&mut buf);
        buf
    })
}

/// stderr 末尾文本。
fn tail_text(bytes: &[u8]) -> String {
    let start = bytes.len().saturating_sub(STDERR_TAIL);
    String::from_utf8_lossy(&bytes[start..]).trim().to_string()
}

impl StructureRunner for ProcessRunner {
    fn run(&self, tensor: &[f32]) -> Result<WorkerOutput, TableError> {
        let mut command = quiet_command(&self.assets.exe);
        command
            .arg("--model")
            .arg(&self.assets.model)
            .arg("--ort")
            .arg(&self.assets.ort_dll)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut child = command
            .spawn()
            .map_err(|e| TableError::Spawn(e.to_string()))?;
        let stdin = child.stdin.take();
        let bytes: Vec<u8> = tensor.iter().flat_map(|v| v.to_le_bytes()).collect();
        let writer = std::thread::spawn(move || {
            if let Some(mut pipe) = stdin {
                let _ = pipe.write_all(&bytes);
            }
        });
        let out = child.stdout.take().map(drain);
        let err = child.stderr.take().map(drain);
        let deadline = Instant::now() + WORKER_TIMEOUT;
        let status = loop {
            match child.try_wait() {
                Ok(Some(status)) => break status,
                Ok(None) if Instant::now() >= deadline => {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(TableError::Timeout);
                }
                Ok(None) => std::thread::sleep(POLL_INTERVAL),
                Err(e) => return Err(TableError::Died(e.to_string())),
            }
        };
        let _ = writer.join();
        let stdout = out.and_then(|h| h.join().ok()).unwrap_or_default();
        let stderr = err.and_then(|h| h.join().ok()).unwrap_or_default();
        if !status.success() {
            return Err(TableError::Died(tail_text(&stderr)));
        }
        parse_worker_output(&stdout).map_err(TableError::Protocol)
    }
}

/// 把 OCR 文字块换成按原图像素的框。
///
/// # 参数
/// - `ocr`：OCR 结果。
pub fn pieces_from_ocr(ocr: &OcrResult) -> Vec<OcrPiece> {
    ocr.boxes
        .iter()
        .map(|b| OcrPiece {
            rect: [
                b.rect.x as f32,
                b.rect.y as f32,
                (b.rect.x + b.rect.width) as f32,
                (b.rect.y + b.rect.height) as f32,
            ],
            text: b.text.clone(),
        })
        .collect()
}

/// 对一张图识别表格：预处理 → 推理 → 解码 → 合并文字。
///
/// # 参数
/// - `runner`：推理执行方式。
/// - `width` / `height`：图像尺寸。
/// - `rgba`：紧凑 RGBA 像素。
/// - `ocr`：同一张图上的 OCR 结果（文字块坐标为原图像素）。
///
/// # 返回
/// 表格的三种文本；图里没有表格返回 [`TableError::NoTable`]。
///
/// ```ignore
/// let texts = recognize_table(&ProcessRunner::new(assets), w, h, &rgba, &ocr)?;
/// println!("{}", texts.markdown);
/// ```
pub fn recognize_table(
    runner: &dyn StructureRunner,
    width: u32,
    height: u32,
    rgba: &[u8],
    ocr: &OcrResult,
) -> Result<TableTexts, TableError> {
    let input = prepare_input(width, height, rgba).ok_or(TableError::InvalidImage)?;
    let output = runner.run(&input.tensor)?;
    let table = assemble_table(&output, input.scale, &pieces_from_ocr(ocr))
        .map_err(TableError::Protocol)?
        .ok_or(TableError::NoTable)?;
    Ok(TableTexts::from_table(&table))
}

/// 在 OCR 结果上叠加表格识别：文本换成 TSV（可直接粘进电子表格），文字块保留，Markdown / HTML 随结果带出。
///
/// # 参数
/// - `runner`：推理执行方式。
/// - `width` / `height` / `rgba`：图像。
/// - `ocr`：OCR 结果。
///
/// # 返回
/// 可直接交给现有识别结果流程的 [`OcrResult`]。
pub fn table_result(
    runner: &dyn StructureRunner,
    width: u32,
    height: u32,
    rgba: &[u8],
    ocr: OcrResult,
) -> Result<OcrResult, OcrError> {
    let texts = recognize_table(runner, width, height, rgba, &ocr)?;
    Ok(OcrResult {
        full_text: texts.tsv.clone(),
        table: Some(texts),
        ..ocr
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ocr_service::OcrTextBox;
    use crate::table_structure::{MODEL_SIDE, WorkerTensor, build_vocab};
    use snow_ui::shell::geometry::PhysicalRect;

    /// 返回固定输出的假执行器。
    struct Canned(Result<WorkerOutput, TableError>);

    impl StructureRunner for Canned {
        fn run(&self, tensor: &[f32]) -> Result<WorkerOutput, TableError> {
            assert_eq!(tensor.len(), 3 * MODEL_SIDE * MODEL_SIDE);
            self.0.clone()
        }
    }

    /// 两个单元格一行的假模型输出（400x200 的图，scale = 1.22）。
    fn one_row_output() -> WorkerOutput {
        let character = "<tr>\n</tr>\n<td".to_string();
        let vocab = build_vocab(&character);
        let width = vocab.len();
        let steps = ["sos", "<tr>", "<td></td>", "<td></td>", "</tr>", "eos"];
        let side = MODEL_SIDE as f32;
        // 纵向占模型输入的 0.1~0.4（400x200 的图里约 y=40..160）
        let quad = |x1: f32, x2: f32| {
            [
                x1 / side,
                0.1,
                x2 / side,
                0.1,
                x2 / side,
                0.4,
                x1 / side,
                0.4,
            ]
        };
        let mut probs = Vec::new();
        let mut locs = Vec::new();
        for (i, t) in steps.iter().enumerate() {
            let at = vocab.iter().position(|v| v == t).expect("词表里有");
            let mut row = vec![0.01; width];
            row[at] = 0.9;
            probs.extend(row);
            locs.extend(match i {
                2 => quad(0.0, 244.0),
                3 => quad(244.0, 488.0),
                _ => [0.0; 8],
            });
        }
        WorkerOutput {
            character,
            outputs: vec![
                WorkerTensor {
                    name: "loc".into(),
                    shape: vec![1, 6, 8],
                    data: locs,
                },
                WorkerTensor {
                    name: "probs".into(),
                    shape: vec![1, 6, width as i64],
                    data: probs,
                },
            ],
        }
    }

    /// 构造 OCR 结果：左右两格各一个文字块（400x200 图坐标；scale=1.22，格框约 0..200 / 200..400 宽）。
    fn ocr_two_cells() -> OcrResult {
        let boxes = vec![
            OcrTextBox {
                rect: PhysicalRect::new(10, 25, 150, 40),
                text: "左".into(),
                confidence: None,
            },
            OcrTextBox {
                rect: PhysicalRect::new(210, 25, 150, 40),
                text: "右".into(),
                confidence: None,
            },
        ];
        OcrResult {
            full_text: "左\n右".into(),
            boxes,
            elapsed_ms: 5,
            table: None,
            latex: None,
        }
    }

    /// 假执行器 + 假 OCR：得到 TSV 文本，Markdown / HTML 随结果带出，文字块保留。
    #[test]
    fn table_result_uses_tsv_and_keeps_boxes() {
        let runner = Canned(Ok(one_row_output()));
        let rgba = vec![255u8; 400 * 200 * 4];
        let result = table_result(&runner, 400, 200, &rgba, ocr_two_cells()).expect("可识别");
        assert_eq!(result.full_text, "左\t右");
        assert_eq!(result.boxes.len(), 2);
        let texts = result.table.expect("带表格文本");
        assert_eq!(texts.markdown, "| 左 | 右 |\n| --- | --- |");
        assert!(texts.html.contains("<td>左</td><td>右</td>"));
    }

    /// 失败路径：坏图、执行器失败、没有表格都映射成明确的错误。
    #[test]
    fn errors_are_mapped() {
        let ok = Canned(Ok(one_row_output()));
        assert_eq!(
            recognize_table(&ok, 0, 0, &[], &ocr_two_cells()),
            Err(TableError::InvalidImage)
        );
        let down = Canned(Err(TableError::Timeout));
        let rgba = vec![0u8; 8 * 8 * 4];
        assert_eq!(
            recognize_table(&down, 8, 8, &rgba, &ocr_two_cells()),
            Err(TableError::Timeout)
        );
        let empty = WorkerOutput {
            character: "<tr>".into(),
            outputs: vec![
                WorkerTensor {
                    name: "loc".into(),
                    shape: vec![1, 2, 8],
                    data: vec![0.0; 16],
                },
                WorkerTensor {
                    name: "probs".into(),
                    shape: vec![1, 2, 4],
                    data: vec![0.9, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.9],
                },
            ],
        };
        assert_eq!(
            recognize_table(&Canned(Ok(empty)), 8, 8, &rgba, &ocr_two_cells()),
            Err(TableError::NoTable)
        );
        assert_eq!(OcrError::from(TableError::NoTable), OcrError::NoTable);
        assert!(matches!(
            OcrError::from(TableError::Unavailable(TableUnavailable::NoRuntime)),
            OcrError::TableUnavailable(TableUnavailable::NoRuntime)
        ));
    }

    /// 真实模型自检（默认忽略）：需要环境变量 `SNOW_TABLE_E2E_ASSETS=<模型>;<worker>;<ort dll>`。
    /// 画一张 3x3 网格线的图，验证真实 SLANet_plus 能解出表格结构。
    #[test]
    #[ignore = "needs the real SLANet_plus model, snow-table.exe and onnxruntime.dll"]
    fn real_model_finds_grid_cells() {
        let spec = std::env::var("SNOW_TABLE_E2E_ASSETS").expect("设置 SNOW_TABLE_E2E_ASSETS");
        let parts: Vec<&str> = spec.split(';').collect();
        let runner = ProcessRunner::new(TableAssets {
            model: parts[0].into(),
            exe: parts[1].into(),
            ort_dll: parts[2].into(),
        });
        let (w, h) = (600u32, 300u32);
        let mut rgba = vec![255u8; (w * h * 4) as usize];
        let mut dot = |x: u32, y: u32| {
            let at = ((y * w + x) * 4) as usize;
            rgba[at..at + 3].fill(0);
        };
        for y in 0..h {
            for x in 0..w {
                if x % 200 < 2 || y % 100 < 2 || x == w - 1 || y == h - 1 {
                    dot(x, y);
                }
            }
        }
        let pieces: Vec<OcrTextBox> = (0..9)
            .map(|i| OcrTextBox {
                rect: PhysicalRect::new((i % 3) * 200 + 60, (i / 3) * 100 + 35, 80, 30),
                text: format!("c{i}"),
                confidence: None,
            })
            .collect();
        let ocr = OcrResult {
            boxes: pieces,
            ..OcrResult::default()
        };
        let texts = recognize_table(&runner, w, h, &rgba, &ocr).expect("真实识别");
        println!("{}", texts.markdown);
        assert!(texts.tsv.lines().count() >= 2, "{}", texts.tsv);
    }
}
