//! 程序入口：子模式分发、存储解析、日志初始化与崩溃转储接线。
//!
//! 所属阶段：P1。完成基础运行时生命周期接线。

use std::path::{Path, PathBuf};
use snow_app_core::PRODUCT_NAME;
use snow_app_core::logging::{CRASH_DIR_NAME, LOG_DIR_NAME, LogConfig, LogGuard, init_logging};
use snow_capability::CapabilityRegistry;
use snow_config::paths::{StorageDirectorySelection, default_app_data_directory, resolve_directory};
use snow_platform::crash::{
    CrashDumpConfig, CrashGuard, DEFAULT_MAX_REPORTS, install as install_crash_handler,
};

use snow_platform::single_instance::{
    IpcCommand, SingleInstanceGuard, SingleInstanceManager, SingleInstanceStatus,
};
use snow_platform::tray::TrayAndHotkeyManager;

pub mod ocr_service;
pub mod overlay_view;
pub mod pinned_manager;
pub mod pinned_view;
pub mod recording;
pub mod settings_view;
pub mod stitch_service;

/// 运行时启动上下文。
pub struct AppBootstrap {
    /// 存储目录解析结果。
    pub storage: StorageDirectorySelection,
    /// 实际生效的数据根目录。
    pub data_root: PathBuf,
    /// 全局日志守卫（必须在进程存活期间持有）。
    pub log_guard: LogGuard,
    /// 本地崩溃转储守卫（若安装成功）。
    pub crash_guard: Option<CrashGuard>,
    /// 当前平台能力表。
    pub capabilities: CapabilityRegistry,
    /// 单实例独占守卫。
    pub single_instance: Option<SingleInstanceGuard>,
    /// 托盘与快捷键管理器。
    pub tray: TrayAndHotkeyManager,
}

/// 执行应用基础运行时引导。
///
/// 解析可执行文件目录与标准应用数据目录，选择生效存储位置并初始化日志与崩溃转储。
///
/// # 参数
/// - `exe_dir`：当前可执行文件所在目录，若为空则自动通过进程环境定位。
///
/// # 返回
/// 初始化的运行上下文，包含数据目录与生命周期守卫。
///
/// # 示例
/// ```no_run
/// use snow_shot::bootstrap;
/// let _ctx = bootstrap(None);
/// ```
pub fn bootstrap(exe_dir: Option<&Path>) -> AppBootstrap {
    let current_exe_dir = exe_dir
        .map(|p| p.to_path_buf())
        .or_else(|| {
            std::env::current_exe()
                .ok()
                .and_then(|p| p.parent().map(|d| d.to_path_buf()))
        })
        .unwrap_or_else(|| PathBuf::from("."));

    let app_data_dir =
        default_app_data_directory().unwrap_or_else(|| current_exe_dir.join("data"));

    let storage = resolve_directory(&current_exe_dir, &app_data_dir);
    let data_root = storage
        .effective_directory
        .clone()
        .unwrap_or_else(|| current_exe_dir.clone());

    let log_config = LogConfig::new(&data_root);
    let log_guard = init_logging(log_config);

    let crash_dir = data_root.join(LOG_DIR_NAME).join(CRASH_DIR_NAME);
    let crash_config = CrashDumpConfig {
        crash_dir,
        app_name: PRODUCT_NAME.to_string(),
        app_version: env!("CARGO_PKG_VERSION").to_string(),
        max_reports: DEFAULT_MAX_REPORTS,
    };
    let crash_guard = install_crash_handler(crash_config).ok();
    let capabilities = CapabilityRegistry::for_current_platform();

    let single_instance = match SingleInstanceManager::acquire("cisox.snow_shot.single_instance") {
        SingleInstanceStatus::Primary(guard) => {
            let _ = SingleInstanceManager::start_listener(&guard, 49210, |cmd| {
                tracing::info!(command = ?cmd, "received IPC command from secondary instance");
            });
            Some(guard)
        }
        SingleInstanceStatus::Secondary => {
            tracing::warn!("secondary instance detected, delegating to primary");
            let _ = SingleInstanceManager::send_command_to_primary(49210, &IpcCommand::ShowMainWindow);
            None
        }
    };

    let tray = TrayAndHotkeyManager::new();

    tracing::info!(
        app = PRODUCT_NAME,
        version = env!("CARGO_PKG_VERSION"),
        data_root = %data_root.display(),
        storage_mode = ?storage.mode,
        is_primary = single_instance.is_some(),
        "runtime bootstrap completed"
    );

    AppBootstrap {
        storage,
        data_root,
        log_guard,
        crash_guard,
        capabilities,
        single_instance,
        tray,
    }
}

/// 生成启动横幅文本。
pub fn banner() -> String {
    format!("{} {}", PRODUCT_NAME, env!("CARGO_PKG_VERSION"))
}

/// 程序入口。
fn main() {
    println!("{}", banner());
    let _app = bootstrap(None);
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 横幅应包含产品名。
    #[test]
    fn banner_contains_product_name() {
        assert!(banner().starts_with(PRODUCT_NAME));
    }

    /// 在临时目录模拟引导，能正常产出有效数据根与能力表。
    #[test]
    fn bootstrap_in_temp_dir() {
        let temp = std::env::temp_dir().join(format!("snow-shot-test-{}", std::process::id()));
        let ctx = bootstrap(Some(&temp));
        assert!(!ctx.data_root.as_os_str().is_empty());
        assert!(ctx.capabilities.query(snow_capability::Capability::CrashDump).is_usable());
        let _ = std::fs::remove_dir_all(&temp);
    }
}
