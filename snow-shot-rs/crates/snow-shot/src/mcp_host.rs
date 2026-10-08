//! MCP 服务宿主：由设置项 `mcp/enabled` 控制按需启停，关闭时不留线程与描述符。

use serde_json::{Value, json};
use snow_config::document::ConfigDocument;
use snow_mcp::tools::AppBackend;
use std::collections::BTreeMap;
use std::path::Path;
use std::sync::{Arc, Mutex};

/// 开关对应的配置键。
pub const KEY_ENABLED: &str = "mcp/enabled";
/// 更新模式配置键（只读展示）。
const KEY_UPDATE_MODE: &str = "updates/mode";

/// 给 MCP 服务线程用的只读数据来源：设置以快照形式共享，避免跨线程碰配置存储。
pub struct ShellBackend {
    /// 设置快照（配置变化时由主线程刷新）。
    settings: Arc<Mutex<BTreeMap<String, Value>>>,
}

impl AppBackend for ShellBackend {
    /// 版本与平台。
    fn app_status(&self) -> Value {
        json!({
            "version": env!("CARGO_PKG_VERSION"),
            "platform": std::env::consts::OS,
        })
    }

    /// 当前设置快照。
    fn settings_snapshot(&self) -> BTreeMap<String, Value> {
        self.settings
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    /// Windows 下截图与录制不需要额外系统授权。
    fn permissions(&self) -> Value {
        json!({"platform": std::env::consts::OS, "required": []})
    }

    /// 只报告配置的更新模式，不发网络请求。
    fn updates_status(&self) -> Value {
        let mode = self.settings_snapshot().remove(KEY_UPDATE_MODE);
        json!({"mode": mode})
    }
}

/// MCP 服务宿主。
pub struct McpHost {
    /// 与后端共享的设置快照。
    settings: Arc<Mutex<BTreeMap<String, Value>>>,
    /// 运行中的服务（未启用为 `None`）。
    #[cfg(windows)]
    service: Option<snow_mcp::service::McpService>,
}

impl McpHost {
    /// 创建（不启动）。
    pub fn new() -> Self {
        Self {
            settings: Arc::new(Mutex::new(BTreeMap::new())),
            #[cfg(windows)]
            service: None,
        }
    }

    /// 服务是否在运行。
    pub fn is_running(&self) -> bool {
        #[cfg(windows)]
        {
            self.service.is_some()
        }
        #[cfg(not(windows))]
        {
            false
        }
    }

    /// 按配置对齐状态：开关打开则启动（已启动则只刷新快照），关闭则停止。
    ///
    /// # 参数
    /// - `document`：当前配置文档。
    /// - `data_root`：应用数据根目录（描述符所在）。
    pub fn sync(&mut self, document: &ConfigDocument, data_root: &Path) {
        let enabled = document.value(KEY_ENABLED).as_bool().unwrap_or(false);
        if enabled {
            *self.settings.lock().unwrap_or_else(|e| e.into_inner()) = document.values().clone();
        }
        #[cfg(windows)]
        {
            if !enabled {
                if self.service.take().is_some() {
                    self.settings
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .clear();
                }
                return;
            }
            if self.service.is_some() {
                return;
            }
            let backend = Arc::new(ShellBackend {
                settings: Arc::clone(&self.settings),
            });
            let config = snow_mcp::service::ServiceConfig {
                data_root: data_root.to_path_buf(),
                server: snow_mcp::tools::ServerInfo {
                    name: snow_app_core::PRODUCT_NAME.to_string(),
                    version: env!("CARGO_PKG_VERSION").to_string(),
                },
                granted: snow_mcp::registry::Scope::DEFAULT_GRANTED.to_vec(),
                pipe_name: None,
            };
            match snow_mcp::service::McpService::start(config, backend) {
                Ok(service) => self.service = Some(service),
                Err(e) => tracing::warn!(error = %e, "启动 MCP 服务失败"),
            }
        }
        #[cfg(not(windows))]
        {
            let _ = data_root;
            if enabled {
                tracing::warn!("当前平台暂不支持 MCP 服务");
            }
        }
    }
}

impl Default for McpHost {
    /// 等同 [`McpHost::new`]。
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 后端：设置快照可读，更新模式来自快照，状态含版本。
    #[test]
    fn backend_reads_snapshot() {
        let settings = Arc::new(Mutex::new(BTreeMap::from([(
            KEY_UPDATE_MODE.to_string(),
            json!("download"),
        )])));
        let backend = ShellBackend { settings };
        assert_eq!(backend.updates_status()["mode"], "download");
        assert!(backend.app_status()["version"].is_string());
        assert_eq!(backend.settings_snapshot().len(), 1);
    }

    /// 默认关闭：不启动、不留快照。
    #[test]
    fn disabled_by_default_does_not_start() {
        let mut host = McpHost::new();
        let doc = ConfigDocument::from_bytes(None);
        let root = std::env::temp_dir().join(format!("cisox-mcp-host-{}", std::process::id()));
        host.sync(&doc, &root);
        assert!(!host.is_running());
        assert!(!root.exists());
    }
}
