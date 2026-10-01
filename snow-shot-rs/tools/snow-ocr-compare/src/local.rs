//! 本地模型（PP-OCR，`snow-ocr-process` 独立进程）后端：直接复用主程序的资产定位与 worker 客户端。

use crate::ocr_assets::{ENV_OCR_ASSET_DIR, ENV_OCR_PROCESS_EXE, OcrAssets, resolve_assets};
use crate::ocr_client::{OcrWorker, SessionConfig, Timeouts};
use crate::runner::Recognizer;
use crate::samples::Case;
use snow_platform::process_mem::{ProcessMemory, process_memory};
use std::path::{Path, PathBuf};

/// 应用数据目录名（与主程序 `APP_ID` 一致）。
const APP_DIR: &str = "Cisox";
/// 检测缩放策略：取较大边（主程序默认）。
const RESIZE_POLICY_MAX: u8 = 0;

/// 默认的 OCR 资产根目录：环境变量优先，否则 `%LOCALAPPDATA%\Cisox\assets\ocr`。
///
/// # 返回
/// 路径；环境变量都缺失时返回 `None`。
///
/// # 示例
/// ```ignore
/// let root = snow_ocr_compare::local::default_asset_root();
/// ```
pub fn default_asset_root() -> Option<PathBuf> {
    if let Some(dir) = std::env::var_os(ENV_OCR_ASSET_DIR).filter(|v| !v.is_empty()) {
        return Some(PathBuf::from(dir));
    }
    let base = std::env::var_os("LOCALAPPDATA")?;
    Some(PathBuf::from(base).join(APP_DIR).join("assets").join("ocr"))
}

/// 环境变量指定的 worker 可执行文件（若有）。
pub fn env_exe_override() -> Option<PathBuf> {
    std::env::var_os(ENV_OCR_PROCESS_EXE)
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
}

/// 本地模型后端：第一次识别时才拉起 worker 并加载模型。
pub struct LocalRecognizer {
    /// 已定位的资产。
    assets: OcrAssets,
    /// 会话配置。
    session: SessionConfig,
    /// 当前 worker（懒启动）。
    worker: Option<OcrWorker>,
}

impl LocalRecognizer {
    /// 定位资产并创建后端（此时不启动进程）。
    ///
    /// # 参数
    /// - `root`：资产根目录。
    /// - `exe_override`：指定的 `snow-ocr-process` 可执行文件。
    /// - `model_kind`：模型类型键，如 `small`。
    /// - `directml`：是否请求 DirectML 加速。
    ///
    /// # 返回
    /// 后端；缺运行时或模型时返回原因（中文说明，沿用主程序文案）。
    ///
    /// # 示例
    /// ```ignore
    /// let rec = LocalRecognizer::open(&root, None, "small", false)?;
    /// ```
    pub fn open(
        root: &Path,
        exe_override: Option<&Path>,
        model_kind: &str,
        directml: bool,
    ) -> Result<Self, String> {
        let assets = resolve_assets(root, exe_override, model_kind).map_err(|e| e.message())?;
        let session = SessionConfig {
            directml,
            resize_policy: RESIZE_POLICY_MAX,
            detector: assets.detector.clone(),
            recognizer: assets.recognizer.clone(),
            dictionary: assets.dictionary.clone(),
        };
        Ok(Self {
            assets,
            session,
            worker: None,
        })
    }
}

impl Recognizer for LocalRecognizer {
    /// 名字固定为 `local-model`。
    fn name(&self) -> &str {
        "local-model"
    }

    /// 首次调用拉起 worker 并准备会话；之后复用。
    fn recognize(&mut self, case: &Case) -> Result<String, String> {
        if self.worker.is_none() {
            let mut worker =
                OcrWorker::spawn(&self.assets, Timeouts::default()).map_err(|e| e.message())?;
            if let Err(e) = worker.prepare(&self.session) {
                worker.shutdown();
                return Err(e.message());
            }
            self.worker = Some(worker);
        }
        let worker = self.worker.as_mut().ok_or("worker missing")?;
        let lines = worker
            .recognize(case.width, case.height, &case.rgba)
            .map_err(|e| e.message())?;
        Ok(lines
            .iter()
            .map(|l| l.text.trim())
            .filter(|t| !t.is_empty())
            .collect::<Vec<_>>()
            .join("\n"))
    }

    /// worker 进程的内存（worker 未启动时为 `None`）。
    fn memory(&self) -> Option<ProcessMemory> {
        self.worker.as_ref()?.pid().and_then(process_memory)
    }
}

impl Drop for LocalRecognizer {
    /// 结束 worker，不留残余进程。
    fn drop(&mut self) {
        if let Some(mut worker) = self.worker.take() {
            worker.shutdown();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 资产缺失时 `open` 返回明确原因，不 panic。
    #[test]
    fn open_reports_missing_assets() {
        let root =
            std::env::temp_dir().join(format!("snow-ocr-compare-empty-{}", std::process::id()));
        let err = LocalRecognizer::open(&root, None, "small", false)
            .err()
            .expect("应不可用");
        assert!(!err.is_empty());
        let bad = LocalRecognizer::open(&root, None, "no-such-model", false)
            .err()
            .expect("未知模型");
        assert!(bad.contains("no-such-model"));
    }
}
