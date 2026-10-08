//! 设置页“MCP”分组顶部的说明区：复制客户端配置的纯逻辑与文案。
//!
//! 桥接进程（`snow-mcp-bridge`，设计文档 M4）尚未随应用发布，所以复制出的配置先指向描述符文件，
//! 文案里明确告知这一限制。剪贴板与文件路径由调用方提供，本模块可离屏测试。

use crate::ocr_backend::i18n_for;
use serde_json::json;
use snow_i18n::Args;
use std::path::Path;

/// 设置页里承载 MCP 说明区的分组 id。
pub const MCP_GROUP_ID: &str = "mcp";
/// 客户端配置里桥接程序的命令名（M4 才会随应用发布）。
const BRIDGE_COMMAND: &str = "snow-mcp-bridge";
/// 桥接程序接收描述符路径的命令行参数。
const BRIDGE_DESCRIPTOR_ARG: &str = "--descriptor";

/// MCP 说明区的界面状态。
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum McpUiState {
    /// 尚未操作。
    #[default]
    Idle,
    /// 已出结果：本地化文案与是否失败。
    Done {
        /// 已本地化的结果文案。
        text: String,
        /// 是否为失败提示（用警示色）。
        failed: bool,
    },
}

/// 说明区文案。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct McpPanel {
    /// 标题。
    pub title: String,
    /// 说明行。
    pub description: String,
    /// “复制客户端配置”按钮文案。
    pub copy_label: String,
    /// 结果提示与是否警示。
    pub notice: Option<(String, bool)>,
}

/// 生成说明区文案。
///
/// # 参数
/// - `locale`：界面语言
/// - `state`：当前界面状态
///
/// ```ignore
/// let panel = mcp_panel("zh-CN", &McpUiState::Idle);
/// assert!(panel.notice.is_none());
/// ```
pub fn mcp_panel(locale: &str, state: &McpUiState) -> McpPanel {
    let i18n = i18n_for(locale);
    McpPanel {
        title: i18n.tr("mcp-panel-title"),
        description: i18n.tr("mcp-panel-description"),
        copy_label: i18n.tr("mcp-panel-copy-button"),
        notice: match state {
            McpUiState::Idle => None,
            McpUiState::Done { text, failed } => Some((text.clone(), *failed)),
        },
    }
}

/// 把复制结果转成界面状态。
///
/// # 参数
/// - `locale`：界面语言
/// - `result`：写剪贴板的结果（错误为原因文本）
pub fn copy_result_state(locale: &str, result: &Result<(), String>) -> McpUiState {
    let i18n = i18n_for(locale);
    match result {
        Ok(()) => McpUiState::Done {
            text: i18n.tr("mcp-panel-copied"),
            failed: false,
        },
        Err(reason) => McpUiState::Done {
            text: i18n.tr_with("mcp-panel-copy-failed", &Args::new().arg(1, reason)),
            failed: true,
        },
    }
}

/// 生成 MCP 客户端配置 JSON（`mcpServers` 结构）。不含令牌：令牌只在描述符文件里，由桥接进程读取。
///
/// # 参数
/// - `descriptor_path`：描述符文件路径
///
/// # 返回
/// 缩进好的 JSON 文本。
///
/// ```ignore
/// let text = client_config_json(Path::new("C:/data/mcp/descriptor.json"));
/// assert!(text.contains("mcpServers"));
/// ```
pub fn client_config_json(descriptor_path: &Path) -> String {
    let name = snow_app_core::PRODUCT_NAME.to_lowercase();
    let value = json!({
        "mcpServers": {
            name: {
                "command": BRIDGE_COMMAND,
                "args": [BRIDGE_DESCRIPTOR_ARG, descriptor_path.display().to_string()],
            }
        }
    });
    serde_json::to_string_pretty(&value).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 配置含桥接命令与描述符路径，且不含令牌字样。
    #[test]
    fn client_config_points_to_descriptor() {
        let text = client_config_json(Path::new("C:/data/mcp/descriptor.json"));
        let v: serde_json::Value = serde_json::from_str(&text).unwrap();
        let server = &v["mcpServers"][snow_app_core::PRODUCT_NAME.to_lowercase()];
        assert_eq!(server["command"], BRIDGE_COMMAND);
        assert_eq!(server["args"][0], BRIDGE_DESCRIPTOR_ARG);
        assert_eq!(server["args"][1], "C:/data/mcp/descriptor.json");
        assert!(!text.to_lowercase().contains("token"));
    }

    /// 两种语言的面板文案齐全；复制结果的成功 / 失败状态正确，失败带原因。
    #[test]
    fn panel_texts_and_result_states() {
        for locale in ["en-US", "zh-CN"] {
            let panel = mcp_panel(locale, &McpUiState::Idle);
            assert!(!panel.title.is_empty() && !panel.description.is_empty());
            assert!(!panel.copy_label.is_empty());
            assert!(panel.notice.is_none());
            let ok = copy_result_state(locale, &Ok(()));
            assert!(matches!(&ok, McpUiState::Done { failed: false, .. }));
            let bad = copy_result_state(locale, &Err("denied".into()));
            let McpUiState::Done { text, failed } = &bad else {
                panic!("应为 Done");
            };
            assert!(*failed && text.contains("denied"));
            assert!(mcp_panel(locale, &bad).notice.is_some());
        }
        assert_ne!(
            mcp_panel("en-US", &McpUiState::Idle).title,
            mcp_panel("zh-CN", &McpUiState::Idle).title
        );
    }
}
