//! `snow-ocr-process` 的最小资产定位与 v4 协议客户端（只服务本对比工具，逻辑对应主程序 `ocr_assets` / `ocr_client`）。
//!
//! 一次识别的时序：`AttachBuffer -> Submit -> ImageConsumed -> Recognize -> Complete -> DetachBuffer`，
//! 像素经普通临时文件（32 字节槽头 + 紧凑 RGBA）交给 worker。错误一律是英文技术说明字符串。

use serde::Deserialize;
use snow_ocr_protocol::{
    CompleteResult, Frame, FrameReader, Kind, MAX_PIXELS, OcrLine, SLOT_HEADER_LEN, VERSION,
    attach_buffer_payload, decode_complete, decode_image_consumed, decode_ready,
    decode_session_ready, encode_frame, hello_payload, prepare_session_payload, submit_payload,
    write_slot_header,
};
use std::io::{BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Stdio};
use std::sync::mpsc::{Receiver, RecvTimeoutError, channel};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// 环境变量：直接指定 `snow-ocr-process` 可执行文件。
pub const ENV_OCR_PROCESS_EXE: &str = "SNOW_OCR_PROCESS_EXE";
/// 环境变量：覆盖 OCR 资产根目录。
pub const ENV_OCR_ASSET_DIR: &str = "SNOW_OCR_ASSET_DIR";
/// 校验完成标记文件名。
const COMPLETE_MARKER: &str = ".complete.json";
/// 内置清单（与主程序同一份数据文件，只读取不复制）。
const MANIFEST_JSON: &str =
    include_str!("../../../crates/snow-shot/resources/ocr-asset-manifest.json");
/// 每像素字节数（RGBA）。
const BYTES_PER_PIXEL: usize = 4;
/// stderr 只保留末尾这么多字节。
const STDERR_KEEP_BYTES: usize = 2048;
/// 关机应答的等待时间。
const SHUTDOWN_WAIT: Duration = Duration::from_secs(2);
/// 临时映射文件名前缀。
const TEMP_FILE_PREFIX: &str = "snow-ocr-compare";
/// 等待 `Ready`。
const READY_TIMEOUT: Duration = Duration::from_secs(5);
/// 等待会话准备（含模型加载）。
const PREPARE_TIMEOUT: Duration = Duration::from_secs(90);
/// 等待 Attach / Consumed / Detach 这类快速应答。
const STEP_TIMEOUT: Duration = Duration::from_secs(10);
/// 等待识别完成。
const RECOGNIZE_TIMEOUT: Duration = Duration::from_secs(120);

/// 清单里的单个文件。
#[derive(Deserialize)]
struct AssetFile {
    /// 文件名。
    name: String,
    /// 字节数。
    size: u64,
}

/// 清单里的运行时。
#[derive(Deserialize)]
struct RuntimeSpec {
    /// 运行时版本。
    version: String,
    /// 平台标记。
    platform: String,
    /// 解压后应有的文件。
    files: Vec<AssetFile>,
}

/// 清单里的一套模型。
#[derive(Deserialize)]
struct ModelSpec {
    /// 模型类型键（`small` 等）。
    #[serde(rename = "type")]
    kind: String,
    /// 模型目录 ID。
    id: String,
    /// 检测模型文件名。
    detector: String,
    /// 识别模型文件名。
    recognizer: String,
    /// 字典文件名。
    dictionary: String,
    /// 全部文件。
    files: Vec<AssetFile>,
}

/// OCR 资产清单。
#[derive(Deserialize)]
struct Manifest {
    /// 运行时。
    runtime: RuntimeSpec,
    /// 全部模型。
    models: Vec<ModelSpec>,
}

/// 已就绪的 OCR 资产路径。
#[derive(Debug, Clone)]
pub struct OcrAssets {
    /// worker 可执行文件。
    pub exe: PathBuf,
    /// 检测模型。
    pub detector: PathBuf,
    /// 识别模型。
    pub recognizer: PathBuf,
    /// 字典。
    pub dictionary: PathBuf,
    /// 能力缓存目录。
    pub state_dir: PathBuf,
}

/// 会话配置（模型路径 + 加速选项）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionConfig {
    /// 请求 DirectML 加速。
    pub directml: bool,
    /// 检测缩放策略（0 = max，1 = min）。
    pub resize_policy: u8,
    /// 检测模型。
    pub detector: PathBuf,
    /// 识别模型。
    pub recognizer: PathBuf,
    /// 字典。
    pub dictionary: PathBuf,
}

/// 目录里是否有完成标记，且每个 `(文件名, 字节数)` 都存在并大小一致。
fn dir_complete(dir: &Path, files: &[AssetFile]) -> bool {
    dir.join(COMPLETE_MARKER).is_file()
        && files.iter().all(|f| {
            std::fs::metadata(dir.join(&f.name)).is_ok_and(|m| m.is_file() && m.len() == f.size)
        })
}

/// 模型目录是否可用：有完成标记时严格校验大小，否则（手动放入）每个文件存在且非空即可。
fn model_ready(dir: &Path, files: &[AssetFile]) -> bool {
    if dir.join(COMPLETE_MARKER).is_file() {
        return dir_complete(dir, files);
    }
    files
        .iter()
        .all(|f| std::fs::metadata(dir.join(&f.name)).is_ok_and(|m| m.is_file() && m.len() > 0))
}

/// 定位 OCR 资产。
///
/// # 参数
/// - `root`：资产根目录（运行时与状态目录在它下面）。
/// - `model_dirs`：模型读取目录（其下是各模型 ID 目录），先找到的优先。
/// - `exe_override`：指定的 worker 可执行文件（存在时优先，且不要求运行时目录）。
/// - `model_kind`：模型类型键，如 `small`。
///
/// # 返回
/// 全部就绪时返回路径集合；否则返回缺什么的英文说明。
///
/// # 示例
/// ```ignore
/// let assets = resolve_assets(&root, &[models], None, "small")?;
/// ```
pub fn resolve_assets(
    root: &Path,
    model_dirs: &[PathBuf],
    exe_override: Option<&Path>,
    model_kind: &str,
) -> Result<OcrAssets, String> {
    let manifest: Manifest = serde_json::from_str(MANIFEST_JSON).map_err(|e| e.to_string())?;
    let model = manifest
        .models
        .iter()
        .find(|m| m.kind == model_kind)
        .ok_or_else(|| format!("unknown model kind: {model_kind}"))?;
    let runtime = &manifest.runtime;
    let exe = match exe_override.filter(|p| p.is_file()) {
        Some(path) => path.to_path_buf(),
        None => {
            let dir = root
                .join("runtimes")
                .join(&runtime.version)
                .join(&runtime.platform);
            let name = runtime
                .files
                .iter()
                .map(|f| f.name.as_str())
                .find(|name| name.ends_with(".exe"))
                .ok_or("the manifest has no runtime executable")?;
            if !dir_complete(&dir, &runtime.files) {
                return Err(format!("OCR runtime not installed under {}", dir.display()));
            }
            dir.join(name)
        }
    };
    let dir = model_dirs
        .iter()
        .map(|d| d.join(&model.id))
        .find(|d| model_ready(d, &model.files))
        .ok_or_else(|| format!("OCR model {} not found", model.id))?;
    Ok(OcrAssets {
        exe,
        detector: dir.join(&model.detector),
        recognizer: dir.join(&model.recognizer),
        dictionary: dir.join(&model.dictionary),
        state_dir: root.join("state").join(&runtime.version),
    })
}

/// 读线程送来的消息。
enum Msg {
    /// 一帧应答。
    Frame(Frame),
    /// 流关闭（进程退出或协议错误）。
    Closed,
}

/// 临时映射文件：离开作用域时尽力删除。
struct TempFile(PathBuf);

impl Drop for TempFile {
    /// 删除临时文件（失败忽略）。
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

/// 一个已握手的 OCR worker 连接。
pub struct OcrWorker {
    /// 发往 worker 的命令流。
    writer: std::process::ChildStdin,
    /// 读线程送来的应答。
    rx: Receiver<Msg>,
    /// 子进程句柄。
    child: Option<Child>,
    /// worker 的 stderr 末尾内容。
    stderr_tail: Arc<Mutex<String>>,
    /// 下一个操作号。
    next_id: u64,
    /// 当前已准备好的会话配置。
    prepared: Option<SessionConfig>,
}

impl OcrWorker {
    /// 拉起 worker 并完成握手。
    ///
    /// # 参数
    /// - `assets`：已就绪的资产。
    ///
    /// # 返回
    /// 已握手的连接；拉起失败、握手超时或协议版本不符时返回错误。
    pub fn spawn(assets: &OcrAssets) -> Result<Self, String> {
        let _ = std::fs::create_dir_all(&assets.state_dir);
        let mut command = std::process::Command::new(&assets.exe);
        command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        if let Some(dir) = assets.exe.parent() {
            command.current_dir(dir);
        }
        let mut child = command.spawn().map_err(|e| format!("spawn failed: {e}"))?;
        let (Some(stdin), Some(stdout), Some(stderr)) =
            (child.stdin.take(), child.stdout.take(), child.stderr.take())
        else {
            let _ = child.kill();
            return Err("could not take over the child process pipes".to_string());
        };
        let stderr_tail = Arc::new(Mutex::new(String::new()));
        spawn_stderr_pump(stderr, Arc::clone(&stderr_tail));
        let (tx, rx) = channel();
        std::thread::spawn(move || {
            let mut frames = FrameReader::new(BufReader::new(stdout));
            while let Ok(frame) = frames.read_frame() {
                if tx.send(Msg::Frame(frame)).is_err() {
                    return;
                }
            }
            let _ = tx.send(Msg::Closed);
        });
        let mut worker = Self {
            writer: stdin,
            rx,
            child: Some(child),
            stderr_tail,
            next_id: 1,
            prepared: None,
        };
        worker.handshake(&assets.state_dir)?;
        Ok(worker)
    }

    /// 发送 `Hello` 并等待 `Ready`，校验协议版本。
    fn handshake(&mut self, state_dir: &Path) -> Result<(), String> {
        self.send(Kind::Hello, 0, &hello_payload(&state_dir.to_string_lossy()))?;
        let frame = self.wait(Kind::Ready, 0, READY_TIMEOUT, "waiting for Ready")?;
        let ready = decode_ready(&frame.payload).map_err(|e| e.to_string())?;
        if !ready.success {
            return Err("the worker reported an initialization failure".to_string());
        }
        if ready.protocol != u32::from(VERSION) {
            return Err(format!(
                "protocol version mismatch: expected {VERSION}, got {}",
                ready.protocol
            ));
        }
        Ok(())
    }

    /// 准备（或复用）识别会话；配置未变时直接返回。
    pub fn prepare(&mut self, config: &SessionConfig) -> Result<(), String> {
        if self.prepared.as_ref() == Some(config) {
            return Ok(());
        }
        let id = self.take_id();
        let payload = prepare_session_payload(
            config.directml,
            config.resize_policy,
            &config.detector.to_string_lossy(),
            &config.recognizer.to_string_lossy(),
            &config.dictionary.to_string_lossy(),
        );
        self.send(Kind::PrepareSession, id, &payload)?;
        let frame = self.wait(Kind::SessionReady, id, PREPARE_TIMEOUT, "loading the model")?;
        if !decode_session_ready(&frame.payload).map_err(|e| e.to_string())? {
            self.prepared = None;
            return Err("the worker could not prepare the OCR session".to_string());
        }
        self.prepared = Some(config.clone());
        Ok(())
    }

    /// 识别一张紧凑 RGBA 图像（像素总数不得超过 3840x2160），返回识别行。
    pub fn recognize(
        &mut self,
        width: u32,
        height: u32,
        rgba: &[u8],
    ) -> Result<Vec<OcrLine>, String> {
        let pixels = (width as usize).checked_mul(height as usize);
        let expected = pixels.and_then(|p| p.checked_mul(BYTES_PER_PIXEL));
        if width == 0
            || height == 0
            || pixels.is_none_or(|p| p > MAX_PIXELS)
            || expected != Some(rgba.len())
        {
            return Err(format!(
                "size {width}x{height} does not match the pixel length {} or exceeds the {MAX_PIXELS} pixel limit",
                rgba.len()
            ));
        }
        let file = write_mapping_file(width, height, rgba)?;
        let generation = self.take_id();
        let result = self.run_recognition(&file, generation, width, height);
        // 无论成败都尽力解除映射；进程已死则跳过
        if self
            .child
            .as_mut()
            .is_some_and(|c| c.try_wait().ok().flatten().is_none())
        {
            let _ = self.detach(generation);
        }
        result
    }

    /// 关闭 worker：发 `Shutdown`，短暂等待应答，然后确保进程结束。
    pub fn shutdown(&mut self) {
        let _ = self.send(Kind::Shutdown, 0, &[]);
        let _ = self.wait(Kind::ShutdownAck, 0, SHUTDOWN_WAIT, "shutting down");
        if let Some(mut child) = self.child.take() {
            let deadline = Instant::now() + SHUTDOWN_WAIT;
            while Instant::now() < deadline && child.try_wait().ok().flatten().is_none() {
                std::thread::sleep(Duration::from_millis(20));
            }
            let _ = child.kill();
            let _ = child.wait();
        }
    }

    /// 子进程 PID。
    pub fn pid(&self) -> Option<u32> {
        self.child.as_ref().map(Child::id)
    }

    /// 执行 Attach -> Submit -> Recognize -> Complete。
    fn run_recognition(
        &mut self,
        file: &TempFile,
        generation: u64,
        width: u32,
        height: u32,
    ) -> Result<Vec<OcrLine>, String> {
        let total = std::fs::metadata(&file.0).map_err(|e| e.to_string())?.len();
        self.send(
            Kind::AttachBuffer,
            generation,
            &attach_buffer_payload(&file.0.to_string_lossy(), total),
        )?;
        self.wait(
            Kind::BufferAttached,
            generation,
            STEP_TIMEOUT,
            "attaching the image",
        )?;
        let token = self.take_id();
        let stride = width * BYTES_PER_PIXEL as u32;
        self.send(
            Kind::Submit,
            token,
            &submit_payload(generation, width, height, stride, 1),
        )?;
        let frame = self.wait(
            Kind::ImageConsumed,
            token,
            STEP_TIMEOUT,
            "transferring the image",
        )?;
        if decode_image_consumed(&frame.payload).map_err(|e| e.to_string())? != (generation, 1) {
            return Err("the ImageConsumed generation or sequence number does not match".into());
        }
        self.send(Kind::Recognize, token, &[])?;
        let frame = self.wait(Kind::Complete, token, RECOGNIZE_TIMEOUT, "recognizing")?;
        match decode_complete(&frame.payload).map_err(|e| e.to_string())? {
            CompleteResult::Success(lines) => Ok(lines),
            CompleteResult::Failed(message) => Err(format!("recognition failed: {message}")),
            CompleteResult::Cancelled(message) => Err(format!("recognition cancelled: {message}")),
        }
    }

    /// 解除映射并等待确认。
    fn detach(&mut self, generation: u64) -> Result<(), String> {
        self.send(Kind::DetachBuffer, generation, &[])?;
        self.wait(
            Kind::BufferDetached,
            generation,
            STEP_TIMEOUT,
            "detaching the image",
        )?;
        Ok(())
    }

    /// 取下一个操作号。
    fn take_id(&mut self) -> u64 {
        let id = self.next_id;
        self.next_id += 1;
        id
    }

    /// 发送一帧命令。
    fn send(&mut self, kind: Kind, id: u64, payload: &[u8]) -> Result<(), String> {
        let bytes = encode_frame(kind, id, payload).map_err(|e| e.to_string())?;
        self.writer
            .write_all(&bytes)
            .and_then(|()| self.writer.flush())
            .map_err(|_| self.died())
    }

    /// 等待指定类型与操作号的应答；其它帧视为协议错误，`step` 只用于超时说明。
    fn wait(
        &mut self,
        kind: Kind,
        id: u64,
        timeout: Duration,
        step: &str,
    ) -> Result<Frame, String> {
        match self.rx.recv_timeout(timeout) {
            Ok(Msg::Frame(frame)) if frame.kind == kind && frame.id == id => Ok(frame),
            Ok(Msg::Frame(frame)) => Err(format!(
                "expected {kind:?}#{id}, got {:?}#{}",
                frame.kind, frame.id
            )),
            Ok(Msg::Closed) | Err(RecvTimeoutError::Disconnected) => Err(self.died()),
            Err(RecvTimeoutError::Timeout) => Err(format!("timed out {step}")),
        }
    }

    /// 构造“进程已死”说明，附 stderr 末尾内容。
    fn died(&self) -> String {
        let tail = self
            .stderr_tail
            .lock()
            .map(|t| t.trim().to_string())
            .unwrap_or_default();
        format!("the OCR worker exited: {tail}")
    }
}

impl Drop for OcrWorker {
    /// 丢弃连接时确保子进程被回收。
    fn drop(&mut self) {
        if let Some(mut child) = self.child.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

/// 启动 stderr 读线程，只保留末尾若干字节。
fn spawn_stderr_pump(stderr: impl Read + Send + 'static, tail: Arc<Mutex<String>>) {
    std::thread::spawn(move || {
        let mut reader = BufReader::new(stderr);
        let mut buffer = [0u8; 512];
        while let Ok(n) = reader.read(&mut buffer) {
            if n == 0 {
                break;
            }
            if let Ok(mut text) = tail.lock() {
                text.push_str(&String::from_utf8_lossy(&buffer[..n]));
                if text.len() > STDERR_KEEP_BYTES {
                    let mut cut = text.len() - STDERR_KEEP_BYTES;
                    while !text.is_char_boundary(cut) {
                        cut += 1;
                    }
                    text.drain(..cut);
                }
            }
        }
    });
}

/// 写映射文件：32 字节槽头 + 紧凑 RGBA；返回自动清理的临时文件。
fn write_mapping_file(width: u32, height: u32, rgba: &[u8]) -> Result<TempFile, String> {
    let mut slot = [0u8; SLOT_HEADER_LEN];
    write_slot_header(&mut slot, 1, width, height).map_err(|e| e.to_string())?;
    let path = std::env::temp_dir().join(format!(
        "{TEMP_FILE_PREFIX}-{}-{width}x{height}.bin",
        std::process::id()
    ));
    let file = TempFile(path);
    let mut out = std::fs::File::create(&file.0).map_err(|e| e.to_string())?;
    out.write_all(&slot)
        .and_then(|()| out.write_all(rgba))
        .and_then(|()| out.flush())
        .map_err(|e| e.to_string())?;
    Ok(file)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 缺资产时给出明确原因；未知模型类型点名该类型。
    #[test]
    fn resolve_reports_missing_and_unknown() {
        let root =
            std::env::temp_dir().join(format!("snow-ocr-worker-empty-{}", std::process::id()));
        let missing = resolve_assets(&root, &[root.join("models")], None, "small").unwrap_err();
        assert!(missing.contains("runtime"));
        let unknown = resolve_assets(&root, &[], None, "no-such-model").unwrap_err();
        assert!(unknown.contains("no-such-model"));
    }

    /// 指定 exe 存在、模型文件齐全（无标记、非空）时能定位。
    #[test]
    fn resolve_finds_manual_model() {
        let root = std::env::temp_dir().join(format!("snow-ocr-worker-ok-{}", std::process::id()));
        let manifest: Manifest = serde_json::from_str(MANIFEST_JSON).expect("清单应可解析");
        let model = manifest
            .models
            .iter()
            .find(|m| m.kind == "small")
            .expect("small");
        let dir = root.join("models").join(&model.id);
        std::fs::create_dir_all(&dir).expect("建目录");
        for f in &model.files {
            std::fs::write(dir.join(&f.name), b"x").expect("写文件");
        }
        let exe = root.join("fake.exe");
        std::fs::write(&exe, b"x").expect("写 exe");
        let assets =
            resolve_assets(&root, &[root.join("models")], Some(&exe), "small").expect("应就绪");
        assert_eq!(assets.exe, exe);
        assert!(assets.detector.ends_with(&model.detector));
        let _ = std::fs::remove_dir_all(&root);
    }
}
