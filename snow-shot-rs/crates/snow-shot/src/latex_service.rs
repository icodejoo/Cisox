//! 公式识别服务：拉起 `cisox-latex` 工作进程托管三个 ONNX 会话，主程序用 [`crate::latex_ocr`]
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
use std::process::{Child, Stdio};
use std::sync::mpsc::{Receiver, RecvTimeoutError, channel};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use std::time::{Duration, Instant};

/// 可比较的请求取消句柄，事件比较采用共享标记身份。
#[derive(Debug, Clone)]
pub struct Cancellation(pub Arc<AtomicBool>);

impl PartialEq for Cancellation {
    /// 只比较请求身份，不比较可变的取消状态。
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
}

/// 等待工作进程就绪（加载三个模型）的最长时间。
pub const READY_TIMEOUT: Duration = Duration::from_secs(60);
/// 单次推理请求的最长等待时间。
pub const REQUEST_TIMEOUT: Duration = Duration::from_secs(60);
/// 保留的 stderr 末尾字节数（给错误提示用）。
const STDERR_TAIL: usize = 400;

/// 持续排空诊断管道，只保留固定容量的末尾字节。
fn read_stderr_tail(reader: &mut impl std::io::Read) -> Vec<u8> {
    let mut tail = Vec::with_capacity(STDERR_TAIL);
    let mut chunk = [0u8; 1024];
    loop {
        match reader.read(&mut chunk) {
            Ok(0) => break,
            Ok(n) => {
                if n >= STDERR_TAIL {
                    tail.clear();
                    tail.extend_from_slice(&chunk[n - STDERR_TAIL..n]);
                } else {
                    let discard = (tail.len() + n).saturating_sub(STDERR_TAIL);
                    tail.drain(..discard);
                    tail.extend_from_slice(&chunk[..n]);
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(_) => break,
        }
    }
    tail
}

/// 检查共享总期限和取消状态。
fn check_budget(deadline: Instant, cancel: &AtomicBool) -> Result<(), String> {
    if cancel.load(Ordering::SeqCst) {
        Err("recognition cancelled".into())
    } else if Instant::now() >= deadline {
        Err("recognition deadline exceeded".into())
    } else {
        Ok(())
    }
}

/// 用短轮询等待管道读写，避免单次阻塞超过整次任务期限。
fn receive_with_budget<T>(
    receiver: &Receiver<T>,
    deadline: Instant,
    cancel: &AtomicBool,
) -> Result<T, String> {
    loop {
        check_budget(deadline, cancel)?;
        match receiver.recv_timeout(
            deadline
                .saturating_duration_since(Instant::now())
                .min(Duration::from_millis(20)),
        ) {
            Ok(value) => return Ok(value),
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => return Err("the worker disconnected".into()),
        }
    }
}

/// 真实后端：与 `cisox-latex` 子进程按行 JSON 对话。
pub struct ProcessBackend {
    /// 子进程。
    child: Child,
    /// 子进程的 stdin。
    stdin: std::sync::mpsc::Sender<String>,
    /// 写入确认通道，使阻塞写也受总截止时间和取消约束。
    writes: Receiver<Result<(), String>>,
    /// 整次识别（含模型加载）的总截止时间。
    deadline: Instant,
    /// 用户取消标记。
    cancel: Arc<AtomicBool>,
    /// 子进程 stdout 的行通道（由读取线程填充）。
    lines: Receiver<String>,
    /// 子进程 stderr 的收集线程。
    stderr: Option<std::thread::JoinHandle<Vec<u8>>>,
}

impl ProcessBackend {
    /// 拉起可取消的后端，所有通信共用一次识别总截止时间。
    fn start_cancellable(assets: &LatexAssets, cancel: Arc<AtomicBool>) -> Result<Self, String> {
        let deadline = Instant::now() + READY_TIMEOUT + REQUEST_TIMEOUT;
        check_budget(deadline, &cancel)?;
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
        let mut input = child.stdin.take().ok_or("worker stdin is unavailable")?;
        let stdout = child.stdout.take().ok_or("worker stdout is unavailable")?;
        let stderr = child
            .stderr
            .take()
            .map(|mut pipe| std::thread::spawn(move || read_stderr_tail(&mut pipe)));
        let (stdin, requests) = channel::<String>();
        let (ack, writes) = channel();
        std::thread::spawn(move || {
            for line in requests {
                let result = input
                    .write_all(line.as_bytes())
                    .and_then(|()| input.flush())
                    .map_err(|e| format!("cannot talk to the worker: {e}"));
                let failed = result.is_err();
                if ack.send(result).is_err() || failed {
                    break;
                }
            }
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
            writes,
            deadline,
            cancel,
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
        match receive_with_budget(
            &self.lines,
            self.deadline.min(Instant::now() + timeout),
            &self.cancel,
        ) {
            Ok(line) => Ok(line),
            Err(error) if error != "the worker disconnected" => Err(error),
            Err(_) => {
                let _ = self.child.kill();
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
        check_budget(self.deadline, &self.cancel)?;
        self.stdin.send(line).map_err(|e| e.to_string())?;
        receive_with_budget(&self.writes, self.deadline, &self.cancel)??;
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

/// 将可识别的取消与期限错误映射为本地化状态，其余保留技术诊断。
fn latex_error(detail: String) -> OcrError {
    match detail.as_str() {
        "recognition cancelled" => OcrError::LatexCancelled,
        "recognition deadline exceeded" => OcrError::LatexTimedOut,
        _ => OcrError::Latex(detail),
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
            LatexRunError::LengthExceeded => Self::LatexTruncated,
            LatexRunError::Backend(d) | LatexRunError::Tokenizer(d) => latex_error(d),
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
#[cfg(test)]
pub fn run_latex(
    assets: &LatexAssets,
    width: u32,
    height: u32,
    rgba: &[u8],
) -> Result<OcrResult, OcrError> {
    run_latex_cancellable(
        assets,
        width,
        height,
        rgba,
        Arc::new(AtomicBool::new(false)),
    )
}

/// 在取消标记控制下完成整次识别，退出时立即回收后端。
pub fn run_latex_cancellable(
    assets: &LatexAssets,
    width: u32,
    height: u32,
    rgba: &[u8],
    cancel: Arc<AtomicBool>,
) -> Result<OcrResult, OcrError> {
    let tokenizer_json = std::fs::read_to_string(&assets.models.tokenizer)
        .map_err(|e| OcrError::Latex(format!("cannot read tokenizer.json: {e}")))?;
    let mut backend = ProcessBackend::start_cancellable(assets, cancel).map_err(latex_error)?;
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
            let mut logits = vec![0.0; (next as usize + 1).max(8)];
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

    /// 截断、超时和取消均有明确的双语提示，不向用户显示内部错误码。
    #[test]
    fn terminal_errors_have_localized_messages() {
        assert_eq!(
            OcrError::from(LatexRunError::LengthExceeded),
            OcrError::LatexTruncated
        );
        assert_eq!(
            latex_error("recognition cancelled".into()),
            OcrError::LatexCancelled
        );
        assert_eq!(
            latex_error("recognition deadline exceeded".into()),
            OcrError::LatexTimedOut
        );
        for error in [
            OcrError::LatexTruncated,
            OcrError::LatexTimedOut,
            OcrError::LatexCancelled,
        ] {
            for locale in ["en-US", "zh-CN"] {
                let message = error.message(crate::ocr_backend::i18n_for(locale));
                assert!(!message.is_empty() && !message.contains("latex-err-"));
            }
        }
    }

    /// 取消和整次期限均能打断没有管道回应的等待；多次接收共用同一期限。
    #[test]
    fn communication_obeys_cancellation_and_total_deadline() {
        let (_tx, rx) = channel::<()>();
        let cancel = AtomicBool::new(true);
        assert_eq!(
            receive_with_budget(&rx, Instant::now() + REQUEST_TIMEOUT, &cancel),
            Err("recognition cancelled".into())
        );
        cancel.store(false, Ordering::SeqCst);
        assert_eq!(
            receive_with_budget(&rx, Instant::now(), &cancel),
            Err("recognition deadline exceeded".into())
        );
        let (tx, rx) = channel();
        let deadline = Instant::now() + REQUEST_TIMEOUT;
        tx.send(42).unwrap();
        assert_eq!(receive_with_budget(&rx, deadline, &cancel), Ok(42));
        assert_eq!(
            receive_with_budget(&rx, Instant::now(), &cancel),
            Err("recognition deadline exceeded".into())
        );
    }

    /// 大量 stderr 仍只保留准确尾部，短输出保持原样。
    #[test]
    fn stderr_tail_is_bounded() {
        let bytes: Vec<u8> = (0..100_000).map(|i| (i % 251) as u8).collect();
        assert_eq!(
            read_stderr_tail(&mut bytes.as_slice()),
            bytes[bytes.len() - STDERR_TAIL..]
        );
        assert_eq!(read_stderr_tail(&mut &b"short"[..]), b"short");
    }

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

    /// 固定公式图和脚本 token 的离线黄金测试，精确验证幂、花括号及等式。
    #[test]
    fn fixed_formula_has_expected_latex() {
        let tokenizer = r#"{"added_tokens":[{"id":0,"special":true},{"id":1,"special":true},{"id":2,"special":true}],"model":{"vocab":{"[PAD]":0,"[BOS]":1,"[EOS]":2,"x":3,"^":4,"{":5,"2":6,"}":7,"+":8,"1":9,"=":10,"y":11}}}"#;
        let (width, height, rgba) = formula_image();
        let mut backend = Scripted {
            script: vec![3, 4, 5, 6, 7, 8, 9, 10, 11, 2],
        };
        let result = latex_result(&mut backend, tokenizer, width, height, &rgba).unwrap();
        assert_eq!(result.full_text, "x^{2}+1=y");
        assert_eq!(result.latex.as_deref(), Some("x^{2}+1=y"));
        assert!(result.table.is_none() && result.boxes.is_empty());
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

    /// 真机自检（默认忽略）：需要环境变量 `SNOW_LATEX_E2E=<模型目录>;<cisox-latex.exe>;<onnxruntime.dll>`，
    /// 目录里放官方 RapidLaTeXOCR 的四个文件。画一个“x 的平方”样的图，验证整条链路能跑通并产出非空 LaTeX。
    #[test]
    #[ignore = "needs the official RapidLaTeXOCR files, cisox-latex.exe and onnxruntime.dll"]
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
