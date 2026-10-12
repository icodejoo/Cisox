//! `system/*` 设置的落地：进程优先级与开机自启。计划由配置算出（纯逻辑，可离屏测试），
//! 再交给 `snow-platform` 执行。开发构建（`debug_assertions`）从不改注册表，避免调试版把自己登记成开机启动。

use snow_app_core::PRODUCT_NAME;
use snow_config::document::ConfigDocument;
use snow_platform::system_integration::{PriorityLevel, set_auto_start, set_process_priority};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

/// 托盘宿主的 exe 文件名（与 UI 进程同目录）。
const HOST_EXE_NAME: &str = "snow-shot.exe";

/// 是否让开机自启指向托盘宿主（双进程 `--ui` 模式开启）。
static AUTO_START_HOST: AtomicBool = AtomicBool::new(false);

/// 双进程模式下让开机自启指向托盘宿主（它才是常驻入口）；单进程常驻仍指向自身。
pub fn prefer_host_exe() {
    AUTO_START_HOST.store(true, Ordering::Relaxed);
}

/// 开机自启要登记的可执行文件路径。
///
/// # 参数
/// - `current_exe`：本进程的 exe 路径。
/// - `prefer_host`：是否指向同目录的托盘宿主。
///
/// ```ignore
/// let target = auto_start_target(Path::new("C:/app/snow-shot-ui.exe"), true);
/// assert!(target.ends_with("snow-shot.exe"));
/// ```
pub fn auto_start_target(current_exe: &Path, prefer_host: bool) -> PathBuf {
    if prefer_host {
        current_exe.with_file_name(HOST_EXE_NAME)
    } else {
        current_exe.to_path_buf()
    }
}

/// 进程优先级配置键。
pub const KEY_PRIORITY: &str = "system/application_priority";
/// 开机自启配置键。
pub const KEY_AUTO_START: &str = "system/auto_start_at_boot";

/// 由配置算出的系统设置计划。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SystemPlan {
    /// 要应用的进程优先级；配置值非法时为 `None`（保持系统默认）。
    pub priority: Option<PriorityLevel>,
    /// 是否开机自启。
    pub auto_start: bool,
}

/// 读取配置得到系统设置计划。
///
/// # 参数
/// - `document`：配置文档。
///
/// ```ignore
/// let plan = plan_from_document(&document);
/// assert_eq!(plan.priority, Some(PriorityLevel::AboveNormal)); // 默认
/// ```
pub fn plan_from_document(document: &ConfigDocument) -> SystemPlan {
    SystemPlan {
        priority: document.value(KEY_PRIORITY).as_str().and_then(PriorityLevel::parse),
        auto_start: document.bool_or(KEY_AUTO_START, true),
    }
}

/// 应用系统设置；失败只记日志，不影响程序运行。
///
/// # 参数
/// - `document`：配置文档。
pub fn apply(document: &ConfigDocument) {
    let plan = plan_from_document(document);
    if let Some(level) = plan.priority {
        match set_process_priority(level) {
            Ok(()) => tracing::info!(?level, "进程优先级已设置"),
            Err(e) => tracing::warn!(?level, error = %e, "设置进程优先级失败"),
        }
    }
    if cfg!(debug_assertions) {
        tracing::debug!("开发构建不改开机自启");
        return;
    }
    match std::env::current_exe() {
        Ok(exe) => match set_auto_start(
            PRODUCT_NAME,
            &auto_start_target(&exe, AUTO_START_HOST.load(Ordering::Relaxed)),
            plan.auto_start,
        ) {
            Ok(()) => tracing::info!(enabled = plan.auto_start, "开机自启已同步"),
            Err(e) => tracing::warn!(enabled = plan.auto_start, error = %e, "同步开机自启失败"),
        },
        Err(e) => tracing::warn!(error = %e, "取不到自身路径，跳过开机自启同步"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// 默认配置：高于普通优先级、开机自启开启（与旧版默认一致）。
    #[test]
    fn defaults_match_legacy() {
        let plan = plan_from_document(&ConfigDocument::from_bytes(None));
        assert_eq!(plan, SystemPlan { priority: Some(PriorityLevel::AboveNormal), auto_start: true });
    }

    /// 开机自启目标：双进程模式指向同目录的宿主，单进程常驻指向自身。
    #[test]
    fn auto_start_target_follows_mode() {
        let ui = Path::new("C:/app/snow-shot-ui.exe");
        assert_eq!(auto_start_target(ui, false), ui);
        assert_eq!(
            auto_start_target(ui, true),
            Path::new("C:/app/snow-shot.exe")
        );
    }

    /// 改配置后计划跟随。
    #[test]
    fn plan_follows_settings() {
        let mut doc = ConfigDocument::from_bytes(None);
        doc.set_value(KEY_PRIORITY, json!("high")).unwrap();
        doc.set_value(KEY_AUTO_START, json!(false)).unwrap();
        assert_eq!(plan_from_document(&doc), SystemPlan { priority: Some(PriorityLevel::High), auto_start: false });
    }
}
