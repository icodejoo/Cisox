//! OCR 识别服务：`snow-ocr-process` 的客户端封装。
//!
//! 职责：定位资产 → 按需拉起独立 worker → 准备会话 → 识别 → 空闲一段时间后自动退出 worker。
//! 没有资产、没有模型、进程崩溃时一律返回明确的 [`OcrError`]，**绝不编造识别文本**。
//! 识别是阻塞调用，必须在后台线程里执行。

use crate::ocr_assets::{
    ENV_OCR_ASSET_DIR, ENV_OCR_PROCESS_EXE, OcrAssets, ocr_root, resolve_assets,
};
use crate::ocr_client::{OcrError, OcrWorker, SessionConfig, Timeouts};
use image::{RgbaImage, imageops};
use serde_json::Value;
use snow_config::document::ConfigDocument;
use snow_ocr_protocol::{MAX_PIXELS, OcrLine};
use snow_ui::shell::geometry::PhysicalRect;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

/// 配置键：模型类型。
pub const KEY_MODEL_TYPE: &str = "text_recognition/model_type";
/// 配置键：DirectML 加速。
pub const KEY_DIRECT_ML: &str = "text_recognition/direct_ml_acceleration";
/// 配置键：检测缩放策略。
pub const KEY_RESIZE_POLICY: &str = "text_recognition/detector_resize_policy";
/// 配置键：常驻进程（开启后不做空闲退出）。
pub const KEY_RESIDENT: &str = "text_recognition/resident_process";
/// 默认空闲退出时间。
pub const DEFAULT_IDLE_TIMEOUT: Duration = Duration::from_secs(30);
/// 检测缩放策略：取较大边（与上游默认一致）。
const RESIZE_POLICY_MAX: u8 = 0;
/// 检测缩放策略：取较小边。
const RESIZE_POLICY_MIN: u8 = 1;
/// 空闲监视线程的最短轮询间隔。
const MONITOR_MIN_TICK: Duration = Duration::from_millis(10);
/// 空闲监视线程的最长轮询间隔。
const MONITOR_MAX_TICK: Duration = Duration::from_millis(500);

/// 单个检测识别出的文本块。
#[derive(Debug, Clone, PartialEq)]
pub struct OcrTextBox {
    /// 文本块外接矩形（原图像素坐标）。
    pub rect: PhysicalRect,
    /// 识别出的文字内容。
    pub text: String,
    /// 置信度分数 (0.0 ~ 1.0)；系统 OCR 不提供时为 `None`。
    pub confidence: Option<f32>,
}

/// 完整 OCR 识别结果集合。
#[derive(Debug, Clone, Default, PartialEq)]
pub struct OcrResult {
    /// 识别出的全部文本块（worker 返回的行序）。
    pub boxes: Vec<OcrTextBox>,
    /// 按行拼接的完整文本（行间 `\n`）；未识别到文字时为空串。
    pub full_text: String,
    /// 执行耗时（毫秒，含拉起与模型加载）。
    pub elapsed_ms: u64,
    /// 表格识别的三种文本；普通文字识别为 `None`。
    pub table: Option<crate::table_structure::TableTexts>,
    /// 公式识别得到的纯 LaTeX；非公式识别为 `None`。
    pub latex: Option<String>,
}

/// 一次识别请求的配置（来自设置页）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OcrRequestConfig {
    /// 模型类型键。
    pub model_kind: String,
    /// 请求 DirectML 加速。
    pub directml: bool,
    /// 检测缩放策略（0 = max，1 = min）。
    pub resize_policy: u8,
    /// 常驻进程（不做空闲退出）。
    pub resident: bool,
}

impl OcrRequestConfig {
    /// 从配置文档读取（缺失时用 schema 默认值）。
    ///
    /// # 参数
    /// - `document`：配置文档。
    ///
    /// ```ignore
    /// let cfg = OcrRequestConfig::from_document(store.document());
    /// ```
    pub fn from_document(document: &ConfigDocument) -> Self {
        let text = |key: &str, default: &str| match document.value(key) {
            Value::String(s) if !s.trim().is_empty() => s,
            _ => default.to_string(),
        };
        let flag = |key: &str| document.value(key).as_bool().unwrap_or(false);
        Self {
            model_kind: text(KEY_MODEL_TYPE, "small"),
            directml: flag(KEY_DIRECT_ML),
            resize_policy: match text(KEY_RESIZE_POLICY, "max").as_str() {
                "min" => RESIZE_POLICY_MIN,
                _ => RESIZE_POLICY_MAX,
            },
            resident: flag(KEY_RESIDENT),
        }
    }
}

/// worker 的拉起方式（便于测试注入假 worker）。
pub trait OcrLauncher: Send + Sync {
    /// 拉起并握手，返回可用连接。
    ///
    /// # 参数
    /// - `assets`：已就绪的资产。
    fn launch(&self, assets: &OcrAssets) -> Result<OcrWorker, OcrError>;
}

/// 真实进程拉起。
pub struct ProcessLauncher;

impl OcrLauncher for ProcessLauncher {
    /// 拉起 `snow-ocr-process` 子进程并握手。
    fn launch(&self, assets: &OcrAssets) -> Result<OcrWorker, OcrError> {
        OcrWorker::spawn(assets, Timeouts::default())
    }
}

/// 服务内部可变状态。
struct Inner {
    /// 当前 worker（空闲退出或出错后为空）。
    worker: Option<OcrWorker>,
    /// 最近一次使用时间。
    last_used: Instant,
    /// 空闲监视线程是否在运行。
    monitor_running: bool,
}

/// 加锁并忽略中毒（一次线程 panic 不应让 OCR 永久失效）。
fn lock(inner: &Mutex<Inner>) -> MutexGuard<'_, Inner> {
    inner.lock().unwrap_or_else(PoisonError::into_inner)
}

/// 把像素总数缩放到协议上限以内。
///
/// # 参数
/// - `width` / `height`：原始尺寸。
///
/// # 返回
/// `(新宽, 新高)`；未超限时原样返回。
///
/// ```ignore
/// assert_eq!(fit_within_limit(1920, 1080), (1920, 1080));
/// let (w, h) = fit_within_limit(7680, 4320);
/// assert!(w as usize * h as usize <= 3840 * 2160);
/// ```
pub fn fit_within_limit(width: u32, height: u32) -> (u32, u32) {
    let pixels = width as u64 * height as u64;
    if pixels <= MAX_PIXELS as u64 {
        return (width, height);
    }
    let scale = (MAX_PIXELS as f64 / pixels as f64).sqrt();
    let mut w = ((width as f64 * scale).floor() as u32).max(1);
    let mut h = ((height as f64 * scale).floor() as u32).max(1);
    // 向下取整后乘积必然不超限；极端长宽比下再保险一次
    while w as u64 * h as u64 > MAX_PIXELS as u64 {
        if w > h { w -= 1 } else { h -= 1 }
    }
    (w, h)
}

/// 把识别行转成原图坐标下的文本块（四点取外接矩形，按缩放比还原并夹到图内）。
///
/// # 参数
/// - `lines`：worker 返回的行（提交图像坐标）。
/// - `scale`：`(x 还原比, y 还原比)`，未缩放为 `(1.0, 1.0)`。
/// - `bounds`：原图尺寸。
///
/// # 返回
/// 文本块；文本为空白的行被丢弃。
pub fn lines_to_boxes(lines: &[OcrLine], scale: (f32, f32), bounds: (u32, u32)) -> Vec<OcrTextBox> {
    let (max_x, max_y) = (bounds.0 as f32, bounds.1 as f32);
    lines
        .iter()
        .filter(|line| !line.text.trim().is_empty())
        .map(|line| {
            let xs = line.quad.iter().map(|p| p[0] * scale.0);
            let ys = line.quad.iter().map(|p| p[1] * scale.1);
            let min_x = xs.clone().fold(f32::INFINITY, f32::min).clamp(0.0, max_x);
            let max_x_px = xs.fold(f32::NEG_INFINITY, f32::max).clamp(0.0, max_x);
            let min_y = ys.clone().fold(f32::INFINITY, f32::min).clamp(0.0, max_y);
            let max_y_px = ys.fold(f32::NEG_INFINITY, f32::max).clamp(0.0, max_y);
            let (x, y) = (min_x.floor() as i32, min_y.floor() as i32);
            let w = ((max_x_px.ceil() as i32) - x).max(0);
            let h = ((max_y_px.ceil() as i32) - y).max(0);
            OcrTextBox {
                rect: PhysicalRect::new(x, y, w, h),
                text: line.text.clone(),
                confidence: Some(line.score),
            }
        })
        .collect()
}

/// OCR 识别服务。
pub struct OcrService {
    /// 资产根目录。
    asset_root: PathBuf,
    /// 指定的 `snow-ocr-process` 可执行文件（开发 / 自测用）。
    exe_override: Option<PathBuf>,
    /// worker 拉起方式。
    launcher: Arc<dyn OcrLauncher>,
    /// 空闲退出时间。
    idle_timeout: Duration,
    /// 可变状态（识别期间持有锁，同一时刻只有一个推理）。
    inner: Arc<Mutex<Inner>>,
}

impl OcrService {
    /// 创建服务：资产根目录与 exe 覆盖从环境变量 / 数据根目录推导。
    ///
    /// # 参数
    /// - `data_root`：应用数据根目录。
    ///
    /// # 示例
    /// ```ignore
    /// let svc = OcrService::new(&data_root);
    /// assert!(!svc.is_worker_running());
    /// ```
    pub fn new(data_root: &Path) -> Self {
        let env_root = std::env::var(ENV_OCR_ASSET_DIR).ok();
        let exe = std::env::var_os(ENV_OCR_PROCESS_EXE)
            .filter(|v| !v.is_empty())
            .map(PathBuf::from);
        Self::with_parts(
            ocr_root(data_root, env_root.as_deref()),
            exe,
            Arc::new(ProcessLauncher),
            DEFAULT_IDLE_TIMEOUT,
        )
    }

    /// 用显式部件创建服务（测试可注入假拉起方式与短空闲时间）。
    ///
    /// # 参数
    /// - `asset_root`：资产根目录。
    /// - `exe_override`：指定的可执行文件。
    /// - `launcher`：worker 拉起方式。
    /// - `idle_timeout`：空闲退出时间。
    pub fn with_parts(
        asset_root: PathBuf,
        exe_override: Option<PathBuf>,
        launcher: Arc<dyn OcrLauncher>,
        idle_timeout: Duration,
    ) -> Self {
        Self {
            asset_root,
            exe_override,
            launcher,
            idle_timeout,
            inner: Arc::new(Mutex::new(Inner {
                worker: None,
                last_used: Instant::now(),
                monitor_running: false,
            })),
        }
    }

    /// 资产根目录。
    pub fn asset_root(&self) -> &Path {
        &self.asset_root
    }

    /// 指定的可执行文件（若有）。
    pub fn exe_override(&self) -> Option<&Path> {
        self.exe_override.as_deref()
    }

    /// 定位当前配置所需的资产。
    ///
    /// # 参数
    /// - `model_kind`：模型类型键。
    pub fn resolve(&self, model_kind: &str) -> Result<OcrAssets, OcrError> {
        resolve_assets(&self.asset_root, self.exe_override.as_deref(), model_kind)
            .map_err(OcrError::Unavailable)
    }

    /// worker 进程当前是否在运行。
    pub fn is_worker_running(&self) -> bool {
        lock(&self.inner).worker.is_some()
    }

    /// 结束 worker（应用退出时调用）。
    pub fn shutdown(&self) {
        if let Some(mut worker) = lock(&self.inner).worker.take() {
            worker.shutdown();
        }
    }

    /// 对 RGBA 图像执行文字识别（阻塞；超过 3840×2160 会先缩放，结果坐标还原到原图）。
    ///
    /// # 参数
    /// - `config`：本次识别的配置。
    /// - `width` / `height`：图像尺寸。
    /// - `rgba`：紧凑 RGBA 像素，长度须为 `宽 * 高 * 4`。
    ///
    /// # 返回
    /// 识别结果；未识别到文字时 `full_text` 为空串（不是错误）。资产缺失、进程失败等返回
    /// [`OcrError`]，不会回退到任何假文本。
    ///
    /// ```ignore
    /// let result = service.recognize_rgba(&cfg, w, h, &rgba)?;
    /// println!("{}", result.full_text);
    /// ```
    pub fn recognize_rgba(
        &self,
        config: &OcrRequestConfig,
        width: u32,
        height: u32,
        rgba: &[u8],
    ) -> Result<OcrResult, OcrError> {
        let started = Instant::now();
        let expected = (width as usize)
            .checked_mul(height as usize)
            .and_then(|p| p.checked_mul(4));
        if width == 0 || height == 0 || expected != Some(rgba.len()) {
            return Err(OcrError::InvalidImage(format!(
                "size {width}x{height} does not match the pixel length {}",
                rgba.len()
            )));
        }
        let assets = self.resolve(&config.model_kind)?;
        let (sw, sh) = fit_within_limit(width, height);
        let scaled;
        let (image, scale) = if (sw, sh) == (width, height) {
            (rgba, (1.0, 1.0))
        } else {
            let source = RgbaImage::from_raw(width, height, rgba.to_vec()).ok_or_else(|| {
                OcrError::InvalidImage("could not build the source image for scaling".to_string())
            })?;
            scaled = imageops::resize(&source, sw, sh, imageops::FilterType::Triangle).into_raw();
            (
                scaled.as_slice(),
                (width as f32 / sw as f32, height as f32 / sh as f32),
            )
        };
        let session = SessionConfig {
            directml: config.directml,
            resize_policy: config.resize_policy,
            detector: assets.detector.clone(),
            recognizer: assets.recognizer.clone(),
            dictionary: assets.dictionary.clone(),
        };
        let lines = self.run_with_worker(&assets, &session, config.resident, sw, sh, image)?;
        let boxes = lines_to_boxes(&lines, scale, (width, height));
        let full_text = boxes
            .iter()
            .map(|b| b.text.as_str())
            .collect::<Vec<_>>()
            .join("\n");
        Ok(OcrResult {
            boxes,
            full_text,
            elapsed_ms: started.elapsed().as_millis() as u64,
            table: None,
            latex: None,
        })
    }

    /// 取（或拉起）worker 并识别；复用的 worker 中途死亡时重启并重试一次。
    fn run_with_worker(
        &self,
        assets: &OcrAssets,
        session: &SessionConfig,
        resident: bool,
        width: u32,
        height: u32,
        rgba: &[u8],
    ) -> Result<Vec<OcrLine>, OcrError> {
        let mut guard = lock(&self.inner);
        let mut attempt = 0;
        loop {
            let reused = guard.worker.is_some();
            if !reused {
                guard.worker = Some(self.launcher.launch(assets)?);
            }
            let outcome = match guard.worker.as_mut() {
                Some(worker) => worker
                    .prepare(session)
                    .and_then(|()| worker.recognize(width, height, rgba)),
                None => Err(OcrError::ProcessDied(String::new())),
            };
            guard.last_used = Instant::now();
            match outcome {
                Ok(lines) => {
                    if !resident && !guard.monitor_running {
                        guard.monitor_running = true;
                        spawn_idle_monitor(Arc::clone(&self.inner), self.idle_timeout);
                    }
                    return Ok(lines);
                }
                Err(e) if e.is_fatal_for_worker() => {
                    if let Some(mut dead) = guard.worker.take() {
                        dead.shutdown();
                    }
                    if reused && attempt == 0 {
                        attempt += 1;
                        continue;
                    }
                    return Err(e);
                }
                Err(e) => return Err(e),
            }
        }
    }
}

/// 启动空闲监视线程：空闲超过阈值就结束 worker，然后自己退出（不常驻）。
fn spawn_idle_monitor(inner: Arc<Mutex<Inner>>, idle_timeout: Duration) {
    let tick = (idle_timeout / 4).clamp(MONITOR_MIN_TICK, MONITOR_MAX_TICK);
    std::thread::spawn(move || {
        loop {
            std::thread::sleep(tick);
            let mut guard = lock(&inner);
            match guard.worker.as_ref() {
                None => {
                    guard.monitor_running = false;
                    return;
                }
                Some(_) if guard.last_used.elapsed() >= idle_timeout => {
                    if let Some(mut worker) = guard.worker.take() {
                        worker.shutdown();
                    }
                    guard.monitor_running = false;
                    return;
                }
                Some(_) => {}
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ocr_assets::{COMPLETE_MARKER, find_model, manifest, model_dir, runtime_dir};
    use crate::ocr_client::fake::{FakeScript, spawn_fake};
    use snow_ocr_protocol::{CompleteResult, Kind};
    use std::collections::VecDeque;
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// 假拉起方式：按顺序取脚本，记录拉起次数。
    struct FakeLauncher {
        /// 待用脚本队列。
        scripts: Mutex<VecDeque<FakeScript>>,
        /// 已拉起次数。
        launches: AtomicUsize,
    }

    impl FakeLauncher {
        /// 用一组脚本创建。
        fn new(scripts: Vec<FakeScript>) -> Arc<Self> {
            Arc::new(Self {
                scripts: Mutex::new(scripts.into()),
                launches: AtomicUsize::new(0),
            })
        }
    }

    impl OcrLauncher for FakeLauncher {
        /// 取下一份脚本拉起假 worker 并握手。
        fn launch(&self, _assets: &OcrAssets) -> Result<OcrWorker, OcrError> {
            self.launches.fetch_add(1, Ordering::SeqCst);
            let script = self
                .scripts
                .lock()
                .map_err(|_| OcrError::SpawnFailed("锁中毒".into()))?
                .pop_front()
                .ok_or_else(|| OcrError::SpawnFailed("没有更多脚本".into()))?;
            let timeouts = Timeouts {
                ready: Duration::from_millis(500),
                prepare: Duration::from_secs(5),
                step: Duration::from_secs(5),
                recognize: Duration::from_secs(5),
            };
            let mut worker = spawn_fake(script, timeouts);
            worker.handshake(Path::new("state"))?;
            Ok(worker)
        }
    }

    /// 唯一临时目录。
    fn temp_root(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("snow-ocr-svc-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("建目录");
        dir
    }

    /// 伪造齐全的资产（运行时 + small 模型），返回根目录。
    fn fake_assets(tag: &str) -> PathBuf {
        let root = temp_root(tag);
        let m = manifest().expect("清单");
        let model = find_model(m, "small").expect("模型");
        for (dir, files) in [
            (runtime_dir(&root, &m.runtime), &m.runtime.files),
            (model_dir(&root, model), &model.files),
        ] {
            std::fs::create_dir_all(&dir).expect("建目录");
            for f in files {
                std::fs::File::create(dir.join(&f.name))
                    .and_then(|file| file.set_len(f.size))
                    .expect("建文件");
            }
            std::fs::write(dir.join(COMPLETE_MARKER), b"{}").expect("标记");
        }
        root
    }

    /// 测试用请求配置。
    fn cfg() -> OcrRequestConfig {
        OcrRequestConfig {
            model_kind: "small".into(),
            directml: false,
            resize_policy: 0,
            resident: false,
        }
    }

    /// 构造服务（假拉起方式）。
    fn service(root: PathBuf, launcher: Arc<FakeLauncher>, idle: Duration) -> OcrService {
        OcrService::with_parts(root, None, launcher, idle)
    }

    /// 没有任何资产：返回“缺运行时”，且没有拉起任何进程，更没有假文本。
    #[test]
    fn no_assets_means_unavailable_not_fake_text() {
        let root = temp_root("empty");
        let launcher = FakeLauncher::new(vec![FakeScript::ok()]);
        let svc = service(root.clone(), Arc::clone(&launcher), DEFAULT_IDLE_TIMEOUT);
        let err = svc.recognize_rgba(&cfg(), 4, 4, &[255; 64]).unwrap_err();
        assert_eq!(
            err,
            OcrError::Unavailable(crate::ocr_assets::OcrUnavailable::NoRuntime)
        );
        assert!(err.can_download());
        assert_eq!(launcher.launches.load(Ordering::SeqCst), 0);
        let _ = std::fs::remove_dir_all(&root);
    }

    /// 非法输入不 panic：零尺寸、长度不符、乘法溢出。
    #[test]
    fn invalid_input_is_an_error() {
        let root = fake_assets("invalid");
        let svc = service(
            root.clone(),
            FakeLauncher::new(vec![]),
            DEFAULT_IDLE_TIMEOUT,
        );
        assert!(matches!(
            svc.recognize_rgba(&cfg(), 0, 0, &[]),
            Err(OcrError::InvalidImage(_))
        ));
        assert!(matches!(
            svc.recognize_rgba(&cfg(), 2, 2, &[0; 3]),
            Err(OcrError::InvalidImage(_))
        ));
        assert!(matches!(
            svc.recognize_rgba(&cfg(), u32::MAX, u32::MAX, &[0; 4]),
            Err(OcrError::InvalidImage(_))
        ));
        let _ = std::fs::remove_dir_all(&root);
    }

    /// 正常识别：外接矩形、全文按行拼接；worker 收到的是 RGBA 原样与正确槽头。
    #[test]
    fn recognizes_through_fake_worker() {
        let root = fake_assets("ok");
        let script = FakeScript {
            result: CompleteResult::Success(vec![
                OcrLine {
                    text: "第一行".into(),
                    score: 0.9,
                    quad: [[10.0, 20.0], [110.0, 22.0], [108.0, 60.0], [8.0, 58.0]],
                },
                OcrLine {
                    text: "  ".into(),
                    score: 0.5,
                    quad: [[0.0; 2]; 4],
                },
                OcrLine {
                    text: "second".into(),
                    score: 0.8,
                    quad: [[10.0, 70.0], [90.0, 70.0], [90.0, 90.0], [10.0, 90.0]],
                },
            ]),
            ..FakeScript::ok()
        };
        let log = Arc::clone(&script.log);
        let svc = service(
            root.clone(),
            FakeLauncher::new(vec![script]),
            DEFAULT_IDLE_TIMEOUT,
        );
        let image: Vec<u8> = (0..200 * 100).flat_map(|_| [7u8, 8, 9, 255]).collect();
        let result = svc.recognize_rgba(&cfg(), 200, 100, &image).expect("识别");
        assert_eq!(result.boxes.len(), 2, "空白行应被丢弃");
        assert_eq!(result.boxes[0].rect, PhysicalRect::new(8, 20, 102, 40));
        assert_eq!(result.full_text, "第一行\nsecond");
        assert!(svc.is_worker_running());
        let entries = log.lock().expect("日志").clone();
        assert!(
            entries
                .iter()
                .any(|e| e == "slot header_ok=true first_pixel=[7, 8, 9, 255] size=200x100"),
            "{entries:?}"
        );
        svc.shutdown();
        assert!(!svc.is_worker_running());
        let _ = std::fs::remove_dir_all(&root);
    }

    /// 识别到 0 行：成功且全文为空（“未识别到文字”不是错误）。
    #[test]
    fn empty_result_is_success() {
        let root = fake_assets("empty-result");
        let script = FakeScript {
            result: CompleteResult::Success(vec![]),
            ..FakeScript::ok()
        };
        let svc = service(
            root.clone(),
            FakeLauncher::new(vec![script]),
            DEFAULT_IDLE_TIMEOUT,
        );
        let result = svc.recognize_rgba(&cfg(), 4, 4, &[255; 64]).expect("识别");
        assert!(result.boxes.is_empty() && result.full_text.is_empty());
        let _ = std::fs::remove_dir_all(&root);
    }

    /// 超上限图像先缩放再提交，返回的坐标还原到原图。
    #[test]
    fn oversized_images_are_downscaled_and_boxes_restored() {
        let root = fake_assets("big");
        let script = FakeScript {
            result: CompleteResult::Success(vec![OcrLine {
                text: "x".into(),
                score: 1.0,
                quad: [
                    [100.0, 100.0],
                    [200.0, 100.0],
                    [200.0, 150.0],
                    [100.0, 150.0],
                ],
            }]),
            ..FakeScript::ok()
        };
        let log = Arc::clone(&script.log);
        let svc = service(
            root.clone(),
            FakeLauncher::new(vec![script]),
            DEFAULT_IDLE_TIMEOUT,
        );
        let (w, h) = (4000u32, 2200u32);
        let image = vec![128u8; w as usize * h as usize * 4];
        let result = svc.recognize_rgba(&cfg(), w, h, &image).expect("识别");
        let (sw, sh) = fit_within_limit(w, h);
        assert!(sw as usize * sh as usize <= MAX_PIXELS && (sw, sh) != (w, h));
        let entries = log.lock().expect("日志").clone();
        assert!(
            entries
                .iter()
                .any(|e| e.contains(&format!("size={sw}x{sh}"))),
            "{entries:?}"
        );
        let fx = w as f32 / sw as f32;
        let expected_x = (100.0 * fx).floor() as i32;
        assert_eq!(result.boxes[0].rect.x, expected_x);
        assert!(result.boxes[0].rect.width > 100);
        svc.shutdown();
        let _ = std::fs::remove_dir_all(&root);
    }

    /// 空闲一段时间后自动退出 worker；再次识别会重新拉起。
    #[test]
    fn worker_exits_when_idle_and_restarts_on_demand() {
        let root = fake_assets("idle");
        let launcher = FakeLauncher::new(vec![FakeScript::ok(), FakeScript::ok()]);
        let svc = service(
            root.clone(),
            Arc::clone(&launcher),
            Duration::from_millis(150),
        );
        svc.recognize_rgba(&cfg(), 4, 4, &[255; 64]).expect("识别");
        assert!(svc.is_worker_running());
        std::thread::sleep(Duration::from_millis(700));
        assert!(!svc.is_worker_running(), "空闲后应已退出");
        svc.recognize_rgba(&cfg(), 4, 4, &[255; 64])
            .expect("再次识别");
        assert_eq!(launcher.launches.load(Ordering::SeqCst), 2);
        svc.shutdown();
        let _ = std::fs::remove_dir_all(&root);
    }

    /// 常驻模式不做空闲退出。
    #[test]
    fn resident_mode_keeps_worker() {
        let root = fake_assets("resident");
        let svc = service(
            root.clone(),
            FakeLauncher::new(vec![FakeScript::ok()]),
            Duration::from_millis(100),
        );
        let mut c = cfg();
        c.resident = true;
        svc.recognize_rgba(&c, 4, 4, &[255; 64]).expect("识别");
        std::thread::sleep(Duration::from_millis(400));
        assert!(svc.is_worker_running());
        svc.shutdown();
        let _ = std::fs::remove_dir_all(&root);
    }

    /// 复用的 worker 中途崩溃：自动重启并重试一次；新拉起的 worker 一上来就崩：直接报错。
    #[test]
    fn crash_recovery_retries_once_only_for_reused_worker() {
        let root = fake_assets("crash");
        // 第一个 worker 在第二次识别时崩溃（第一次正常）；重启后的第二个 worker 正常
        let first = FakeScript {
            die_on: Some(Kind::AttachBuffer),
            ..FakeScript::ok()
        };
        let launcher = FakeLauncher::new(vec![first, FakeScript::ok()]);
        let svc = service(root.clone(), Arc::clone(&launcher), DEFAULT_IDLE_TIMEOUT);
        // 第一次：新拉起的 worker 在 Attach 时崩溃 -> 不重试，直接报错
        let err = svc.recognize_rgba(&cfg(), 4, 4, &[255; 64]).unwrap_err();
        assert!(matches!(err, OcrError::ProcessDied(_)), "{err:?}");
        assert!(!svc.is_worker_running());
        // 出错后 worker 已被丢弃，下一次会重新拉起第二个（正常的）worker
        assert!(svc.recognize_rgba(&cfg(), 4, 4, &[255; 64]).is_ok());
        assert_eq!(launcher.launches.load(Ordering::SeqCst), 2);
        svc.shutdown();

        // 复用场景：worker 第一次正常，之后进程被外部杀掉
        let dying = FakeScript {
            die_on: None,
            ..FakeScript::ok()
        };
        let launcher = FakeLauncher::new(vec![dying, FakeScript::ok()]);
        let svc = service(root.clone(), Arc::clone(&launcher), DEFAULT_IDLE_TIMEOUT);
        svc.recognize_rgba(&cfg(), 4, 4, &[255; 64]).expect("首次");
        // 模拟进程死亡：让 worker 正常关机（管道关闭），再把这个已死的连接放回去
        let taken = lock(&svc.inner).worker.take();
        if let Some(mut w) = taken {
            w.shutdown();
            lock(&svc.inner).worker = Some(w);
        }
        svc.recognize_rgba(&cfg(), 4, 4, &[255; 64])
            .expect("重启后应成功");
        assert_eq!(launcher.launches.load(Ordering::SeqCst), 2);
        svc.shutdown();
        let _ = std::fs::remove_dir_all(&root);
    }

    /// 缩放规则：未超限原样；超限后乘积不超限且保持长宽比（误差 1 像素内）。
    #[test]
    fn fit_within_limit_rules() {
        assert_eq!(fit_within_limit(1920, 1080), (1920, 1080));
        assert_eq!(fit_within_limit(3840, 2160), (3840, 2160));
        for (w, h) in [
            (7680, 4320),
            (4000, 2200),
            (10000, 100),
            (100, 100000),
            (u32::MAX, 2),
        ] {
            let (nw, nh) = fit_within_limit(w, h);
            assert!(nw >= 1 && nh >= 1);
            assert!(
                nw as u64 * nh as u64 <= MAX_PIXELS as u64,
                "{w}x{h} -> {nw}x{nh}"
            );
        }
        let (nw, nh) = fit_within_limit(7680, 4320);
        assert_eq!((nw, nh), (3840, 2160));
    }

    /// 四点框转矩形：还原缩放、夹到图内、丢弃空白行。
    #[test]
    fn lines_to_boxes_maps_and_clamps() {
        let lines = vec![
            OcrLine {
                text: "a".into(),
                score: 0.5,
                quad: [[-5.0, -5.0], [50.0, 0.0], [50.0, 30.0], [0.0, 30.0]],
            },
            OcrLine {
                text: " ".into(),
                score: 0.5,
                quad: [[0.0; 2]; 4],
            },
            OcrLine {
                text: "b".into(),
                score: 0.5,
                quad: [[90.0, 10.0], [500.0, 10.0], [500.0, 20.0], [90.0, 20.0]],
            },
        ];
        let boxes = lines_to_boxes(&lines, (2.0, 2.0), (200, 100));
        assert_eq!(boxes.len(), 2);
        assert_eq!(boxes[0].rect, PhysicalRect::new(0, 0, 100, 60));
        assert_eq!(boxes[1].rect, PhysicalRect::new(180, 20, 20, 20));
    }

    /// 请求配置：缺省值与显式值。
    #[test]
    fn request_config_defaults() {
        let doc = ConfigDocument::from_bytes(None);
        let c = OcrRequestConfig::from_document(&doc);
        assert_eq!(c.model_kind, "small");
        assert!(!c.directml && !c.resident);
        assert_eq!(c.resize_policy, RESIZE_POLICY_MAX);
    }
}

#[cfg(test)]
mod real_worker_tests {
    //! 真实 `snow-ocr-process` 端到端探针（`--ignored`，需要本机已安装 OCR 资产）。

    use super::*;
    use crate::ocr_client::{OcrWorker, SessionConfig, Timeouts};
    use snow_config::paths::default_app_data_directory;
    use snow_platform::process_mem::process_memory;
    use snow_platform::text_raster::{DEFAULT_FONT_FAMILY, rasterize_text};

    /// 把文本渲染成白底黑字的 RGBA 图（四周留白）。
    fn render_text_image(lines: &[&str], px: f32) -> (u32, u32, Vec<u8>) {
        let bitmaps: Vec<_> = lines
            .iter()
            .map(|l| rasterize_text(l, DEFAULT_FONT_FAMILY, px, false).expect("光栅化"))
            .collect();
        let margin = 24u32;
        let width = bitmaps.iter().map(|b| b.width).max().unwrap_or(1) + 2 * margin;
        let line_gap = 12u32;
        let height = bitmaps.iter().map(|b| b.height + line_gap).sum::<u32>() + 2 * margin;
        let mut rgba = vec![255u8; width as usize * height as usize * 4];
        let mut top = margin;
        for bmp in &bitmaps {
            for y in 0..bmp.height {
                for x in 0..bmp.width {
                    let cov = u32::from(bmp.coverage[(y * bmp.width + x) as usize]);
                    let v = (255 - cov) as u8;
                    let at = ((top + y) * width + margin + x) as usize * 4;
                    rgba[at..at + 4].copy_from_slice(&[v, v, v, 255]);
                }
            }
            top += bmp.height + line_gap;
        }
        (width, height, rgba)
    }

    /// 真实识别：渲染中英文文本，经真实 worker 识别，打印耗时与 worker 内存。
    #[test]
    #[ignore = "需要本机已安装 OCR 运行时与模型"]
    fn real_worker_recognizes_rendered_text() {
        let data_root = default_app_data_directory().expect("数据根目录");
        let svc = OcrService::new(&data_root);
        let cfg = OcrRequestConfig {
            model_kind: "small".into(),
            directml: false,
            resize_policy: 0,
            resident: true,
        };
        let assets = svc.resolve(&cfg.model_kind).expect("资产应已安装");
        let (w, h, rgba) =
            render_text_image(&["Hello Snow Shot OCR 12345", "你好，截图识别测试"], 40.0);

        let cold = std::time::Instant::now();
        let first = svc
            .recognize_rgba(&cfg, w, h, &rgba)
            .expect("首次识别（含拉起与加载）");
        let cold_ms = cold.elapsed().as_millis();
        let warm = std::time::Instant::now();
        let second = svc.recognize_rgba(&cfg, w, h, &rgba).expect("二次识别");
        let warm_ms = warm.elapsed().as_millis();
        println!(
            "REAL|image={w}x{h}|cold_ms={cold_ms}|warm_ms={warm_ms}|text1={:?}|text2={:?}",
            first.full_text, second.full_text
        );
        for b in &first.boxes {
            println!("REAL|box|{:?}|{}|{:?}", b.rect, b.text, b.confidence);
        }
        assert!(
            first.full_text.contains("Snow") || first.full_text.contains("Hello"),
            "{:?}",
            first.full_text
        );
        assert!(
            first.full_text.contains("识别") || first.full_text.contains("你好"),
            "{:?}",
            first.full_text
        );
        assert_eq!(first.full_text, second.full_text);

        // 独立连接测内存：新拉起一个 worker，识别后读它的工作集
        let mut worker = OcrWorker::spawn(&assets, Timeouts::default()).expect("拉起");
        let before = worker.pid().and_then(process_memory);
        worker
            .prepare(&SessionConfig {
                directml: false,
                resize_policy: 0,
                detector: assets.detector.clone(),
                recognizer: assets.recognizer.clone(),
                dictionary: assets.dictionary.clone(),
            })
            .expect("准备会话");
        worker.recognize(w, h, &rgba).expect("识别");
        let after = worker.pid().and_then(process_memory);
        println!(
            "REAL|worker_mem|idle_ws={:?} MiB|after_ws={:?} MiB|after_peak={:?} MiB",
            before.map(|m| m.working_set / 1048576),
            after.map(|m| m.working_set / 1048576),
            after.map(|m| m.peak_working_set / 1048576)
        );
        worker.shutdown();
        svc.shutdown();
    }
}
