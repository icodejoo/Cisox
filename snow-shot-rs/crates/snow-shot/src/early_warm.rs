//! 宿主 `--hint` 驱动的最早期预热：宿主拉起 UI 时已知队首命令是翻译或 OCR，
//! UI 在 `main` 最早处（不等 GPUI 初始化、不等连管道）就在后台线程里拉起对应 worker，
//! 之后 `AppState` 创建时认领同一个服务实例，首次请求不必再等进程拉起与模型加载。
//!
//! 约束：
//! - 只在用户已按下对应热键 / 手势之后才预热，不做推测性预拉；
//! - 没人认领或没用上的预热由各自服务兜底回收（OCR 空闲 30 s 退出、翻译 worker 的 30 s TTL）；
//! - 只预热本地后端，系统 OCR / 在线翻译不开线程。

use crate::ocr_backend::OcrBackend;
use crate::ocr_service::{OcrRequestConfig, OcrService};
use crate::sys_prefs::system_ui_language;
use crate::translate_input_session::initial_warm_config;
use crate::translate_service::{Backend, TranslateConfig, TranslateHost, WarmHandle};
use snow_config::document::ConfigDocument;
use snow_config::paths::config_file_path;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

/// 预拉起时留给 `AppState` 认领的翻译宿主与预热句柄。
struct EarlyTranslate {
    /// 已开始预热的翻译宿主（`AppState` 沿用它，worker 才是同一个）。
    host: Arc<TranslateHost>,
    /// 预热句柄（由输入框 / 选中文字流程认领，没用上就回收）。
    warm: WarmHandle,
}

/// 进程级槽位：预拉起的 OCR 服务。
static OCR_SLOT: Mutex<Option<Arc<OcrService>>> = Mutex::new(None);
/// 进程级槽位：预拉起的翻译宿主。
static TRANSLATE_SLOT: Mutex<Option<EarlyTranslate>> = Mutex::new(None);

/// 加锁；毒锁不影响继续使用。
fn lock<T>(slot: &Mutex<T>) -> MutexGuard<'_, T> {
    slot.lock().unwrap_or_else(PoisonError::into_inner)
}

/// 读配置文件得到文档（只读，不做损坏备份等副作用，与 UI 随后正式打开配置互不干扰）。
///
/// # 参数
/// - `data_root`：应用数据根目录。
fn read_document(data_root: &Path) -> ConfigDocument {
    let bytes = std::fs::read(config_file_path(data_root)).ok();
    ConfigDocument::from_bytes(bytes.as_deref())
}

/// 当前配置是否走本地 OCR 模型（系统 OCR 没有 worker，不预热）。
///
/// # 参数
/// - `document`：配置文档。
pub fn ocr_warm_wanted(document: &ConfigDocument) -> bool {
    OcrBackend::from_document(document) == OcrBackend::LocalModel
}

/// 预热翻译 worker 用的配置；非本地后端返回 `None`（不开线程）。
///
/// 与输入框浮窗首次真实请求一致，用户之后改语言或包不需要重新预热。
///
/// # 参数
/// - `document`：配置文档。
/// - `ui_language`：系统界面语言（决定翻译默认目标语言）。
pub fn translate_warm_config(
    document: &ConfigDocument,
    ui_language: &str,
) -> Option<TranslateConfig> {
    let base = TranslateConfig::from_document(document, ui_language);
    let warm = initial_warm_config(&base);
    (warm.backend == Backend::Local).then_some(warm)
}

/// UI 进程最早处调用（`--hint ocr`）：后台线程里拉起 OCR worker 并加载模型，不阻塞调用线程。
///
/// # 参数
/// - `data_root`：应用数据根目录。
///
/// # 示例
/// ```ignore
/// if launch.hint == UiHint::Ocr {
///     early_warm::begin_ocr(ctx.data_root.clone());
/// }
/// ```
pub fn begin_ocr(data_root: PathBuf) {
    let document = read_document(&data_root);
    if !ocr_warm_wanted(&document) {
        return;
    }
    let service = Arc::new(OcrService::new(&data_root));
    *lock(&OCR_SLOT) = Some(Arc::clone(&service));
    let config = OcrRequestConfig::from_document(&document);
    let spawned = std::thread::Builder::new()
        .name("snow-ocr-early".into())
        .spawn(move || match service.warm_up(&config) {
            Ok(()) => tracing::info!("OCR 模型已按启动提示提前预热"),
            Err(error) => tracing::info!(?error, "OCR 提前预热跳过"),
        });
    if let Err(error) = spawned {
        tracing::warn!(%error, "无法创建 OCR 提前预热线程");
    }
}

/// `AppState` 创建时取 OCR 服务：有提前预热的就沿用（识别请求会排在预热后面，不重复拉进程），否则新建。
///
/// # 参数
/// - `data_root`：应用数据根目录。
pub fn take_ocr(data_root: &Path) -> Arc<OcrService> {
    lock(&OCR_SLOT)
        .take()
        .unwrap_or_else(|| Arc::new(OcrService::new(data_root)))
}

/// UI 进程最早处调用（`--hint translate`）：开始预热本地翻译 worker，预热在后台线程里跑。
///
/// # 参数
/// - `data_root`：应用数据根目录。
///
/// # 示例
/// ```ignore
/// if launch.hint == UiHint::Translate {
///     early_warm::begin_translate(ctx.data_root.clone());
/// }
/// ```
pub fn begin_translate(data_root: &Path) {
    let document = read_document(data_root);
    let Some(config) = translate_warm_config(&document, &system_ui_language()) else {
        return;
    };
    let host = Arc::new(TranslateHost::new(data_root));
    let warm = WarmHandle::start(Arc::clone(&host), config, Arc::new(AtomicBool::new(false)));
    *lock(&TRANSLATE_SLOT) = Some(EarlyTranslate { host, warm });
}

/// `AppState` 创建时取翻译宿主与提前预热的句柄；没有提前预热时新建宿主、句柄为空。
///
/// # 参数
/// - `data_root`：应用数据根目录。
///
/// # 返回
/// `(翻译宿主, 预热句柄)`；句柄交给输入框 / 选中文字流程，没用上由它们回收。
pub fn take_translate(data_root: &Path) -> (Arc<TranslateHost>, Option<WarmHandle>) {
    match lock(&TRANSLATE_SLOT).take() {
        Some(early) => (early.host, Some(early.warm)),
        None => (Arc::new(TranslateHost::new(data_root)), None),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use snow_config::extensions::{
        BACKEND_OPENAI, KEY_OCR_BACKEND, KEY_TRANSLATION_BACKEND, OCR_BACKEND_LOCAL_MODEL,
        OCR_BACKEND_SYSTEM,
    };

    /// 本地模型后端才预热 OCR；系统 OCR 不开线程。
    #[test]
    fn ocr_warm_only_for_local_model() {
        let mut doc = ConfigDocument::from_bytes(None);
        doc.set_value(KEY_OCR_BACKEND, json!(OCR_BACKEND_SYSTEM))
            .unwrap();
        assert!(!ocr_warm_wanted(&doc));
        doc.set_value(KEY_OCR_BACKEND, json!(OCR_BACKEND_LOCAL_MODEL))
            .unwrap();
        assert!(ocr_warm_wanted(&doc));
    }

    /// 翻译预热配置：本地后端给出配置，在线后端不预热。
    #[test]
    fn translate_warm_config_follows_backend() {
        let mut doc = ConfigDocument::from_bytes(None);
        let local = translate_warm_config(&doc, "zh-CN");
        assert!(local.is_some_and(|c| c.backend == Backend::Local));
        doc.set_value(KEY_TRANSLATION_BACKEND, json!(BACKEND_OPENAI))
            .unwrap();
        assert!(translate_warm_config(&doc, "zh-CN").is_none());
    }

    /// 没有提前预热时认领得到新建的宿主且没有句柄。
    #[test]
    fn take_without_early_builds_fresh() {
        let dir = std::env::temp_dir().join("snow-early-warm-none");
        let (_, warm) = take_translate(&dir);
        assert!(warm.is_none());
        let _ = take_ocr(&dir);
    }
}
