//! 数据驱动的 tool 注册表：由旧清单镜像 [`MANIFEST`] 全量登记，已实现的挂处理函数。

use crate::manifest::MANIFEST;
use crate::tools::{self, ToolContext, ToolError};
use serde_json::{Value, json};
use std::collections::HashMap;

/// tool 所属的域（沿用旧版 4 域的切法）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Domain {
    /// 截图域（旧键 `screenshot`）。
    Screenshot,
    /// 应用域（旧键 `application`）。
    Application,
    /// 文档与异步任务域（旧键 `documents_jobs`）。
    DocumentsJobs,
    /// 录制与贴图域（旧键 `recording_pinned`）。
    RecordingPinned,
}

impl Domain {
    /// 旧清单里的域键。
    pub fn legacy_key(self) -> &'static str {
        match self {
            Self::Screenshot => "screenshot",
            Self::Application => "application",
            Self::DocumentsJobs => "documents_jobs",
            Self::RecordingPinned => "recording_pinned",
        }
    }

    /// 该域计划落地的里程碑（设计文档 §5），用于“未实现”错误的提示。
    pub fn milestone(self) -> &'static str {
        match self {
            Self::Screenshot => "M1",
            Self::Application | Self::DocumentsJobs => "M2",
            Self::RecordingPinned => "M3",
        }
    }
}

/// 权限域：描述符里的 scope 列表按它授权。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Scope {
    /// 只读查询。
    ReadOnly,
    /// 截图与标注（默认授权）。
    Capture,
    /// 写设置、删数据、录制等破坏性或有副作用的控制（默认不授权）。
    Control,
}

impl Scope {
    /// 描述符与日志里使用的名字。
    pub fn name(self) -> &'static str {
        match self {
            Self::ReadOnly => "read_only",
            Self::Capture => "capture",
            Self::Control => "control",
        }
    }

    /// 默认授权的权限域（设计文档 §3：只读 + 截图）。
    pub const DEFAULT_GRANTED: &'static [Scope] = &[Scope::ReadOnly, Scope::Capture];
}

/// tool 处理函数：输入已通过 schema 校验。
pub type Handler = fn(&ToolContext<'_>, &Value) -> Result<Value, ToolError>;

/// 一个已登记的 tool。
pub struct ToolDef {
    /// tool 名（保持旧版 `snow_shot_` 前缀）。
    pub name: &'static str,
    /// 所属域。
    pub domain: Domain,
    /// 调用所需权限域。
    pub scope: Scope,
    /// 入参 schema。
    pub schema: Value,
    /// 处理函数；`None` 表示尚未实现。
    pub handler: Option<Handler>,
}

/// 按名字推导权限域：只读后缀 → 只读；截图域其余 → 截图；剩下 → 控制。
fn scope_for(domain: Domain, name: &str) -> Scope {
    const READ_SUFFIXES: &[&str] = &[
        "_status",
        "_get",
        "_list",
        "_displays",
        "_state",
        "_catalog",
    ];
    if READ_SUFFIXES.iter().any(|s| name.ends_with(s)) {
        Scope::ReadOnly
    } else if domain == Domain::Screenshot {
        Scope::Capture
    } else {
        Scope::Control
    }
}

/// tool 注册表。
pub struct Registry {
    /// 按清单顺序排列的 tool。
    tools: Vec<ToolDef>,
    /// 名字到下标。
    index: HashMap<&'static str, usize>,
}

impl Registry {
    /// 用旧清单镜像与已实现处理表构建注册表。
    pub fn build() -> Self {
        let mut implemented = tools::implemented();
        let tools: Vec<ToolDef> = MANIFEST
            .iter()
            .map(|(domain, name)| {
                let (schema, handler) = match implemented.remove(name) {
                    Some((schema, handler)) => (schema, Some(handler)),
                    None => (
                        json!({"type": "object", "additionalProperties": true}),
                        None,
                    ),
                };
                ToolDef {
                    name,
                    domain: *domain,
                    scope: scope_for(*domain, name),
                    schema,
                    handler,
                }
            })
            .collect();
        debug_assert!(implemented.is_empty(), "已实现处理表里有清单外的 tool");
        let index = tools.iter().enumerate().map(|(i, t)| (t.name, i)).collect();
        Self { tools, index }
    }

    /// 按名查找。
    pub fn get(&self, name: &str) -> Option<&ToolDef> {
        self.index.get(name).map(|i| &self.tools[*i])
    }

    /// 全部 tool（清单顺序）。
    pub fn all(&self) -> &[ToolDef] {
        &self.tools
    }

    /// 已实现的 tool 数。
    pub fn implemented_count(&self) -> usize {
        self.tools.iter().filter(|t| t.handler.is_some()).count()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    /// 旧清单文件（只读对照）。
    const LEGACY_JSON: &str = include_str!("../../../../snow_shot/mcp-capabilities.json");

    /// 取旧清单某域的 tool 名列表。
    fn legacy_tools(domain: &str) -> Vec<String> {
        let v: Value = serde_json::from_str(LEGACY_JSON).unwrap();
        v["domains"][domain]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .map(|t| t.as_str().unwrap().to_string())
            .collect()
    }

    /// 与旧版清单逐域逐项一致（名称与顺序），合计 101 个且无重复。
    #[test]
    fn manifest_matches_legacy_capabilities() {
        let registry = Registry::build();
        for domain in [
            Domain::Screenshot,
            Domain::Application,
            Domain::DocumentsJobs,
            Domain::RecordingPinned,
        ] {
            let ours: Vec<String> = registry
                .all()
                .iter()
                .filter(|t| t.domain == domain)
                .map(|t| t.name.to_string())
                .collect();
            assert_eq!(ours, legacy_tools(domain.legacy_key()), "{domain:?}");
        }
        assert_eq!(registry.all().len(), 101);
        let unique: BTreeSet<_> = registry.all().iter().map(|t| t.name).collect();
        assert_eq!(unique.len(), 101);
        assert!(
            registry
                .all()
                .iter()
                .all(|t| t.name.starts_with("snow_shot_"))
        );
    }

    /// 每个 schema 本身是 object 类型；剩余未实现数 = 总数 - 已实现数（随期数递减）。
    #[test]
    fn schemas_are_objects_and_counts_add_up() {
        let registry = Registry::build();
        assert!(registry.all().iter().all(|t| t.schema["type"] == "object"));
        let pending = registry
            .all()
            .iter()
            .filter(|t| t.handler.is_none())
            .count();
        assert_eq!(pending + registry.implemented_count(), 101);
        assert!(registry.implemented_count() >= 5);
    }

    /// 权限域推导：只读 / 截图 / 控制。
    #[test]
    fn scope_derivation() {
        let registry = Registry::build();
        let scope = |n: &str| registry.get(n).unwrap().scope;
        assert_eq!(scope("snow_shot_app_status"), Scope::ReadOnly);
        assert_eq!(scope("snow_shot_settings_get"), Scope::ReadOnly);
        assert_eq!(scope("snow_shot_screenshot_begin"), Scope::Capture);
        assert_eq!(scope("snow_shot_settings_update"), Scope::Control);
        assert_eq!(scope("snow_shot_history_clear"), Scope::Control);
        assert_eq!(scope("snow_shot_recording_start"), Scope::Control);
    }
}
