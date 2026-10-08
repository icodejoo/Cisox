//! 公式识别服务：拉起 `snow-latex` 工作进程托管三个 ONNX 会话，主程序用 [`crate::latex_ocr`]
//! 的纯函数驱动整条流水线（预处理、宽度迭代、自回归解码、后处理）。
//!
//! 工作进程只在一次识别期间存活，识别完立即退出，不常驻。识别是阻塞调用，必须在后台线程里执行。

use crate::latex_assets::LatexAssets;
use crate::latex_ocr::{LatexBackend, LatexRunError, LatexTensor, Tokenizer, recognize};
use crate::ocr_client::OcrError;
use crate::ocr_download::quiet_command;
use crate::ocr_service::OcrResult;
use serde_json::{Value, json};
use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, Stdio};
use std::sync::mpsc::{Receiver, RecvTimeoutError, channel};
use std::time::{Duration, Instant};

/// 等待工作进程就绪（加载三个模型）的最长时间。
pub const READY_TIMEOUT: Duration = Duration::from_secs(60);
/// 单次推理请求的最长等待时间。
pub const REQUEST_TIMEOUT: Duration = Duration::from_secs(60);
/// 保留的 stderr 末尾字节数（给错误提示用）。
const STDERR_TAIL: usize = 400;

/// 真实后端：与 `snow-latex` 子进程按行 JSON 对话。
pub struct ProcessBackend {
    /// 子进程。
    child: Child,
    /// 子进程的 stdin。
    stdin: ChildStdin,
    /// 子进程 stdout 的行通道（由读取线程填充）。
    lines: Receiver<String>,
    /// 子进程 stderr 的收集线程。
    stderr: Option<std::thread::JoinHandle<Vec<u8>>>,
}

impl ProcessBackend {
    /// 拉起工作进程并等它报告就绪。
    ///
    /// # 参数
    /// - `assets`：已就绪的资产路径。
    ///
    /// # 返回
    /// 可用的后端；拉起失败或加载模型失败返回说明（附 stderr 末尾）。
    pub fn start(assets: &LatexAssets) -> Result<Self, String> {
        let mut command = quiet_command(&assets.exe);
        command
            .arg("--resizer")
            .arg(&assets.models.resizer)
            .arg("--encoder")
            .arg(&assets.models.encoder)
            .arg("--decoder")
            .arg(&assets.models.decoder)
            .arg("--ort")
            .arg(&assets.ort_dll)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut child = command.spawn().map_err(|e| e.to_string())?;
        let stdin = child.stdin.take().ok_or("worker stdin is unavailable")?;
        let stdout = child.stdout.take().ok_or("worker stdout is unavailable")?;
        let stderr = child.stderr.take().map(|mut pipe| {
            std::thread::spawn(move || {
                let mut buf = Vec::new();
                let _ = std::io::Read::read_to_end(&mut pipe, &mut buf);
                buf
            })
        });
        let (tx, lines) = channel();
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                if tx.send(line).is_err() {
                    break;
                }
            }
        });
        let mut backend = Self {
            child,
            stdin,
            lines,
            stderr,
        };
        let ready = backend.read_line(READY_TIMEOUT)?;
        match serde_json::from_str::<Value>(&ready) {
            Ok(v) if v.get("ready").and_then(Value::as_bool) == Some(true) => Ok(backend),
            _ => Err(format!("unexpected worker greeting: {ready}")),
        }
    }

    /// 等一行输出；进程提前退出时附上 stderr 末尾。
    fn read_line(&mut self, timeout: Duration) -> Result<String, String> {
        match self.lines.recv_timeout(timeout) {
            Ok(line) => Ok(line),
            Err(RecvTimeoutError::Timeout) => Err("the worker timed out".to_string()),
            Err(RecvTimeoutError::Disconnected) => {
                let _ = self.child.wait();
                let tail = self
                    .stderr
                    .take()
                    .and_then(|h| h.join().ok())
                    .map(|bytes| {
                        let start = bytes.len().saturating_sub(STDERR_TAIL);
                        String::from_utf8_lossy(&bytes[start..]).trim().to_string()
                    })
                    .unwrap_or_default();
                Err(if tail.is_empty() {
                    "the worker exited unexpectedly".to_string()
                } else {
                    tail
                })
            }
        }
    }

    /// 发一条请求并等回应；回应里带 `error` 字段按失败处理。
    fn call(&mut self, request: &Value) -> Result<Value, String> {
        let mut line = request.to_string();
        line.push('\n');
        self.stdin
            .write_all(line.as_bytes())
            .and_then(|()| self.stdin.flush())
            .map_err(|e| format!("cannot talk to the worker: {e}"))?;
        let started = Instant::now();
        let response = self.read_line(REQUEST_TIMEOUT)?;
        let value: Value = serde_json::from_str(&response)
            .map_err(|e| format!("bad worker response ({e}) after {:?}", started.elapsed()))?;
        match value.get("error").and_then(Value::as_str) {
            Some(error) => Err(error.to_string()),
            None => Ok(value),
        }
    }

    /// 取回应里的浮点数组字段。
    fn floats(value: &Value, field: &str) -> Result<Vec<f32>, String> {
        value
            .get(field)
            .and_then(Value::as_array)
            .map(|list| {
                list.iter()
                    .filter_map(Value::as_f64)
                    .map(|v| v as f32)
                    .collect()
            })
            .ok_or_else(|| format!("the worker response has no {field}"))
    }
}

impl Drop for ProcessBackend {
    /// 识别结束（或出错）后立刻收掉工作进程。
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl LatexBackend for ProcessBackend {
    fn resize_logits(&mut self, input: &LatexTensor) -> Result<Vec<f32>, String> {
        let response =
            self.call(&json!({"op": "resize", "shape": input.shape, "data": input.data}))?;
        Self::floats(&response, "data")
    }

    fn encode(&mut self, input: &LatexTensor) -> Result<(), String> {
        self.call(&json!({"op": "encode", "shape": input.shape, "data": input.data}))
            .map(|_| ())
    }

    fn step(&mut self, tokens: &[i64]) -> Result<Vec<f32>, String> {
        let response = self.call(&json!({"op": "step", "tokens": tokens}))?;
        Self::floats(&response, "logits")
    }
}

/// 把流水线失败映射成 OCR 错误体系（覆盖窗的失败展示直接可用）。
impl From<LatexRunError> for OcrError {
    fn from(e: LatexRunError) -> Self {
        match e {
            LatexRunError::InvalidImage => {
                Self::InvalidImage("empty or malformed image".to_string())
            }
            LatexRunError::Blank => Self::NoLatex,
            LatexRunError::Backend(d) | LatexRunError::Tokenizer(d) => Self::Latex(d),
        }
    }
}

/// 用给定后端对一张图做公式识别，结果包装成可交给现有识别结果流程的 [`OcrResult`]。
///
/// # 参数
/// - `backend`：推理后端。
/// - `tokenizer_json`：`tokenizer.json` 的内容。
/// - `width` / `height` / `rgba`：图像。
///
/// # 返回
/// 文本为 LaTeX 的识别结果；模型没有产出内容返回 [`OcrError::EmptyLatex`]。
///
/// ```ignore
/// let result = latex_result(&mut backend, &tokenizer_json, w, h, &rgba)?;
/// println!("{}", result.full_text);
/// ```
pub fn latex_result(
    backend: &mut dyn LatexBackend,
    tokenizer_json: &str,
    width: u32,
    height: u32,
    rgba: &[u8],
) -> Result<OcrResult, OcrError> {
    let started = Instant::now();
    let tokenizer = Tokenizer::from_json(tokenizer_json)?;
    let latex = recognize(backend, &tokenizer, width, height, rgba)?;
    if latex.is_empty() {
        return Err(OcrError::EmptyLatex);
    }
    Ok(OcrResult {
        full_text: latex.clone(),
        latex: Some(latex),
        elapsed_ms: started.elapsed().as_millis() as u64,
        ..OcrResult::default()
    })
}

/// 阻塞完成一次公式识别：读词表、拉起工作进程、跑完流水线、收掉进程。
///
/// # 参数
/// - `assets`：已就绪的资产路径。
/// - `width` / `height` / `rgba`：选区图像。
pub fn run_latex(
    assets: &LatexAssets,
    width: u32,
    height: u32,
    rgba: &[u8],
) -> Result<OcrResult, OcrError> {
    let tokenizer_json = std::fs::read_to_string(&assets.models.tokenizer)
        .map_err(|e| OcrError::Latex(format!("cannot read tokenizer.json: {e}")))?;
    let mut backend = ProcessBackend::start(assets).map_err(OcrError::Latex)?;
    latex_result(&mut backend, &tokenizer_json, width, height, rgba)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::latex_ocr::EOS_TOKEN;

    /// 脚本化假后端：宽度分类器认可任何宽度，解码按脚本出 token。
    struct Scripted {
        /// 依次生成的 token。
        script: Vec<i64>,
    }

    impl LatexBackend for Scripted {
        fn resize_logits(&mut self, input: &LatexTensor) -> Result<Vec<f32>, String> {
            let mut logits = vec![0.0; 21];
            logits[input.shape[3] / 32 - 1] = 1.0;
            Ok(logits)
        }

        fn encode(&mut self, _input: &LatexTensor) -> Result<(), String> {
            Ok(())
        }

        fn step(&mut self, tokens: &[i64]) -> Result<Vec<f32>, String> {
            let next = self
                .script
                .get(tokens.len() - 1)
                .copied()
                .unwrap_or(EOS_TOKEN);
            let mut logits = vec![0.0; 8];
            logits[next as usize] = 3.0;
            Ok(logits)
        }
    }

    /// 带一条竖笔画的 RGBA 图。
    fn stroke_image() -> (u32, u32, Vec<u8>) {
        let (w, h) = (80u32, 40u32);
        let mut rgba = vec![255u8; (w * h * 4) as usize];
        for y in 5..35usize {
            for x in 10..20usize {
                rgba[(y * w as usize + x) * 4..(y * w as usize + x) * 4 + 3].fill(0);
            }
        }
        (w, h, rgba)
    }

    /// 最小词表。
    const TOKENIZER: &str = r#"{"added_tokens":[{"id":0,"special":true},{"id":1,"special":true},{"id":2,"special":true}],
        "model":{"vocab":{"[PAD]":0,"[BOS]":1,"[EOS]":2,"x":3,"Ġ":4,"y":5}}}"#;

    /// 成功：文本与 LaTeX 字段一致，文字块为空。
    #[test]
    fn result_carries_latex() {
        let (w, h, rgba) = stroke_image();
        let mut backend = Scripted {
            script: vec![3, 4, 5, 2],
        };
        let result = latex_result(&mut backend, TOKENIZER, w, h, &rgba).expect("成功");
        assert_eq!(result.full_text, "x y");
        assert_eq!(result.latex.as_deref(), Some("x y"));
        assert!(result.boxes.is_empty() && result.table.is_none());
    }

    /// 失败路径：纯色图、坏词表、没产出内容、坏尺寸，都映射成明确的错误。
    #[test]
    fn errors_are_mapped() {
        let (w, h, rgba) = stroke_image();
        let mut backend = Scripted { script: vec![3, 2] };
        let white = vec![255u8; (w * h * 4) as usize];
        assert_eq!(
            latex_result(&mut backend, TOKENIZER, w, h, &white).unwrap_err(),
            OcrError::NoLatex
        );
        assert!(matches!(
            latex_result(&mut backend, "{}", w, h, &rgba).unwrap_err(),
            OcrError::Latex(_)
        ));
        let mut silent = Scripted { script: vec![2] };
        assert_eq!(
            latex_result(&mut silent, TOKENIZER, w, h, &rgba).unwrap_err(),
            OcrError::EmptyLatex
        );
        assert!(matches!(
            latex_result(&mut backend, TOKENIZER, 0, 0, &[]).unwrap_err(),
            OcrError::InvalidImage(_)
        ));
        assert!(
            OcrError::NoLatex
                .message(crate::ocr_backend::i18n_for("en-US"))
                .is_ascii()
        );
        assert!(!OcrError::EmptyLatex.can_download());
    }

    /// 真机自检（默认忽略）：需要环境变量 `SNOW_LATEX_E2E=<模型目录>;<snow-latex.exe>;<onnxruntime.dll>`，
    /// 目录里放官方 RapidLaTeXOCR 的四个文件。画一个“x 的平方”样的图，验证整条链路能跑通并产出非空 LaTeX。
    #[test]
    #[ignore = "needs the official RapidLaTeXOCR files, snow-latex.exe and onnxruntime.dll"]
    fn real_model_end_to_end() {
        let spec = std::env::var("SNOW_LATEX_E2E").expect("设置 SNOW_LATEX_E2E");
        let parts: Vec<&str> = spec.split(';').collect();
        let models = crate::latex_assets::check_model_dir(std::path::Path::new(parts[0]))
            .expect("模型文件齐全");
        let assets = LatexAssets {
            exe: parts[1].into(),
            models,
            ort_dll: parts[2].into(),
        };
        let (w, h, rgba) = formula_image();
        let started = Instant::now();
        let result = run_latex(&assets, w, h, &rgba).expect("真实识别");
        println!("latex = {:?} ({:?})", result.full_text, started.elapsed());
        assert!(!result.full_text.is_empty());
    }

    /// 用 5x7 点阵字体画 `x^2+1=y`（放大 6 倍的黑字白底），给真机自检用。
    fn formula_image() -> (u32, u32, Vec<u8>) {
        const GLYPHS: [(char, [&str; 7]); 7] = [
            (
                'x',
                [
                    "     ", "     ", "#   #", " # # ", "  #  ", " # # ", "#   #",
                ],
            ),
            (
                '2',
                [
                    " ### ", "#   #", "    #", "   # ", "  #  ", " #   ", "#####",
                ],
            ),
            (
                '+',
                [
                    "     ", "  #  ", "  #  ", "#####", "  #  ", "  #  ", "     ",
                ],
            ),
            (
                '1',
                [
                    "  #  ", " ##  ", "  #  ", "  #  ", "  #  ", "  #  ", " ### ",
                ],
            ),
            (
                '=',
                [
                    "     ", "     ", "#####", "     ", "#####", "     ", "     ",
                ],
            ),
            (
                'y',
                [
                    "     ", "     ", "#   #", "#   #", " ####", "    #", " ### ",
                ],
            ),
            (
                '^',
                [
                    "  #  ", " # # ", "#   #", "     ", "     ", "     ", "     ",
                ],
            ),
        ];
        const SCALE: usize = 6;
        let text = "x^2+1=y";
        let (w, h) = (text.len() * 6 * SCALE + 40, 7 * SCALE + 40);
        let mut rgba = vec![255u8; w * h * 4];
        for (i, ch) in text.chars().enumerate() {
            let glyph = GLYPHS.iter().find(|(c, _)| *c == ch).expect("字模").1;
            for (gy, row) in glyph.iter().enumerate() {
                for (gx, cell) in row.chars().enumerate() {
                    if cell != '#' {
                        continue;
                    }
                    for dy in 0..SCALE {
                        for dx in 0..SCALE {
                            let x = 20 + (i * 6 + gx) * SCALE + dx;
                            let y = 20 + gy * SCALE + dy;
                            rgba[(y * w + x) * 4..(y * w + x) * 4 + 3].fill(0);
                        }
                    }
                }
            }
        }
        (w as u32, h as u32, rgba)
    }
}
