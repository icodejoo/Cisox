//! `system/*` 设置的落地：进程优先级与开机自启。计划由配置算出（纯逻辑，可离屏测试），
//! 再交给 `snow-platform` 执行。开发构建（`debug_assertions`）从不改注册表，避免调试版把自己登记成开机启动。

use serde_json::Value;
use snow_app_core::PRODUCT_NAME;
use snow_config::document::ConfigDocument;
use snow_platform::system_integration::{PriorityLevel, set_auto_start, set_process_priority};

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
        auto_start: !matches!(document.value(KEY_AUTO_START), Value::Bool(false)),
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
        Ok(exe) => match set_auto_start(PRODUCT_NAME, &exe, plan.auto_start) {
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

    /// 改配置后计划跟随。
    #[test]
    fn plan_follows_settings() {
        let mut doc = ConfigDocument::from_bytes(None);
        doc.set_value(KEY_PRIORITY, json!("high")).unwrap();
        doc.set_value(KEY_AUTO_START, json!(false)).unwrap();
        assert_eq!(plan_from_document(&doc), SystemPlan { priority: Some(PriorityLevel::High), auto_start: false });
    }
}
