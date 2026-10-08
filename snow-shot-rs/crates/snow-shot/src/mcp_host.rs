//! MCP 服务宿主：由设置项 `mcp/enabled` 控制按需启停，关闭时不留线程与描述符。
//!
//! MCP 服务线程不碰 GPUI 与配置存储：需要主线程数据 / 能力的 tool 经 [`MainThreadInbox`] 投
//! [`UiEvent::Mcp`]（请求-响应，带超时），由主线程处理后经通道回复；纯“开窗口”类动作直接投已有的 [`UiEvent`]。

use crate::app_runtime::UiEvent;
use crate::settings_state::SharedConfig;
use serde_json::{Value, json};
use snow_config::document::ConfigDocument;
use snow_config::extensions::{KEY_MCP_ALLOW_CAPTURE, KEY_MCP_ALLOW_CONTROL};
use snow_config::normalize::normalize;
use snow_config::schema::{default_value, entry_for};
use snow_mcp::registry::Scope;
use snow_mcp::tools::{AppBackend, ToolError};
use snow_translate::Lang;
use snow_ui::shell::inbox::MainThreadInbox;
use snow_ui::shell::monitor::MonitorInfo;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Sender};
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// 开关对应的配置键。
pub const KEY_ENABLED: &str = "mcp/enabled";
/// 更新模式配置键（只读展示）。
const KEY_UPDATE_MODE: &str = "updates/mode";
/// 等主线程回复的最长时间。
pub const MAIN_THREAD_TIMEOUT: Duration = Duration::from_secs(5);
/// 存储概况最多遍历的文件系统条目数（防止超大目录拖住服务线程）。
const STORAGE_SCAN_LIMIT: usize = 200_000;

/// 主线程对 MCP 请求的回复。
pub type McpReply = Result<Value, ToolError>;

/// 需要主线程处理的请求种类。
#[derive(Debug, Clone, PartialEq)]
pub enum McpRequestKind {
    /// 枚举显示器。
    Displays,
    /// 批量写入设置（键值已过策略检查）。
    SettingsUpdate(Vec<(String, Value)>),
    /// 把若干设置恢复默认。
    SettingsReset(Vec<String>),
}

/// 一次请求-响应：请求种类加一次性回复通道。
#[derive(Debug, Clone)]
pub struct McpRequest {
    /// 请求种类。
    pub kind: McpRequestKind,
    /// 回复通道（用后取走，重复回复被忽略）。
    reply: Arc<Mutex<Option<Sender<McpReply>>>>,
}

impl PartialEq for McpRequest {
    /// 同一次请求（同种类且共享同一回复通道）才相等。
    fn eq(&self, other: &Self) -> bool {
        self.kind == other.kind && Arc::ptr_eq(&self.reply, &other.reply)
    }
}

impl McpRequest {
    /// 由种类与回复通道构造。
    pub fn new(kind: McpRequestKind, reply: Sender<McpReply>) -> Self {
        Self {
            kind,
            reply: Arc::new(Mutex::new(Some(reply))),
        }
    }

    /// 回复请求；对端已放弃等待（超时）时静默丢弃。
    pub fn respond(&self, reply: McpReply) {
        let sender = self.reply.lock().unwrap_or_else(|e| e.into_inner()).take();
        if let Some(sender) = sender {
            let _ = sender.send(reply);
        }
    }
}

/// 投请求给主线程并等回复。
///
/// # 参数
/// - `inbox`：主线程收件箱。
/// - `kind`：请求种类。
/// - `timeout`：最长等待时间。
///
/// # 返回
/// 主线程的回复；收件箱已关闭返回 `app_closing`，超时返回 `main_thread_timeout`。
pub fn round_trip(
    inbox: &MainThreadInbox<UiEvent>,
    kind: McpRequestKind,
    timeout: Duration,
) -> McpReply {
    let (tx, rx) = mpsc::channel();
    if !inbox.push(UiEvent::Mcp(McpRequest::new(kind, tx))) {
        return Err(ToolError {
            code: "app_closing",
            message: "application is shutting down".into(),
        });
    }
    rx.recv_timeout(timeout).unwrap_or_else(|_| {
        Err(ToolError {
            code: "main_thread_timeout",
            message: "the application did not answer in time".into(),
        })
    })
}

/// 把显示器列表转成 JSON（物理像素坐标，缩放为浮点倍数）。
///
/// # 参数
/// - `monitors`：显示器快照。
pub fn displays_json(monitors: &[MonitorInfo]) -> Value {
    let rect = |r: &snow_ui::shell::geometry::PhysicalRect| json!({"x": r.x, "y": r.y, "width": r.width, "height": r.height});
    let list: Vec<Value> = monitors
        .iter()
        .map(|m| {
            json!({
                "id": m.id.0,
                "name": m.name,
                "primary": m.is_primary,
                "scale": m.scale.value(),
                "bounds": rect(&m.bounds),
                "work_area": rect(&m.work_area),
            })
        })
        .collect();
    json!({"displays": list})
}

/// 在主线程批量写入设置：整体校验通过才写，写盘失败整体回滚。
///
/// # 参数
/// - `store`：共享配置。
/// - `changes`：`(键, 新值)` 列表。
///
/// # 返回
/// 实际有变化的 `(键, 旧值)`；未知键返回 `unknown_setting`，值不合法返回 `invalid_value`，
/// 只读 / 写盘失败返回 `settings_write_failed`（错误信息不含值，避免泄露内容）。
pub fn apply_settings(
    store: &SharedConfig,
    changes: Vec<(String, Value)>,
) -> Result<Vec<(String, Value)>, ToolError> {
    for (key, value) in &changes {
        if entry_for(key).is_none() {
            return Err(ToolError {
                code: "unknown_setting",
                message: format!("no such setting: {key}"),
            });
        }
        if !normalize(key, value).valid {
            return Err(ToolError {
                code: "invalid_value",
                message: format!("invalid value for setting: {key}"),
            });
        }
    }
    let mut store = store.borrow_mut();
    let mut done: Vec<(String, Value)> = Vec::new();
    let failure = |what: String| ToolError {
        code: "settings_write_failed",
        message: what,
    };
    let mut error: Option<ToolError> = None;
    for (key, value) in changes {
        let before = store.value(&key);
        if let Err(e) = store.set_value(&key, value) {
            error = Some(failure(format!("{key}: {e}")));
            break;
        }
        if store.value(&key) != before {
            done.push((key, before));
        }
    }
    if error.is_none()
        && let Err(e) = store.flush()
    {
        error = Some(failure(format!("flush: {e}")));
    }
    if let Some(error) = error {
        for (key, before) in done.into_iter().rev() {
            if let Err(e) = store.set_value(&key, before) {
                tracing::warn!(key, error = %e, "MCP 写设置失败后回滚也失败");
            }
        }
        return Err(error);
    }
    Ok(done)
}

/// 把写入结果整理成 tool 返回值，并给出需要广播的 `(键, 旧值)`。
fn changed_reply(done: &[(String, Value)]) -> Value {
    json!({"changed": done.iter().map(|(k, _)| k.as_str()).collect::<Vec<_>>()})
}

/// 主线程处理一个 MCP 请求。
///
/// # 参数
/// - `request`：请求。
/// - `monitors`：枚举显示器的回调（主线程上由外壳提供）。
/// - `store`：共享配置。
///
/// # 返回
/// 回复内容和需要广播给运行时的配置变更（`ConfigChanged` 用）。
pub fn handle_request<E: std::fmt::Display>(
    request: &McpRequest,
    monitors: impl FnOnce() -> Result<Vec<MonitorInfo>, E>,
    store: &SharedConfig,
) -> (McpReply, Vec<(String, Value)>) {
    match &request.kind {
        McpRequestKind::Displays => {
            let reply = monitors()
                .map(|m| displays_json(&m))
                .map_err(|e| ToolError {
                    code: "displays_unavailable",
                    message: e.to_string(),
                });
            (reply, Vec::new())
        }
        McpRequestKind::SettingsUpdate(changes) => {
            settings_outcome(apply_settings(store, changes.clone()))
        }
        McpRequestKind::SettingsReset(keys) => {
            let changes = keys
                .iter()
                .map(|k| (k.clone(), default_value(k)))
                .collect::<Vec<_>>();
            // 未知键要先报错：默认值函数对未知键给 Null，会被后续校验当成不合法值
            if let Some(bad) = keys.iter().find(|k| entry_for(k).is_none()) {
                let err = ToolError {
                    code: "unknown_setting",
                    message: format!("no such setting: {bad}"),
                };
                return (Err(err), Vec::new());
            }
            settings_outcome(apply_settings(store, changes))
        }
    }
}

/// 写入结果转回复与广播列表。
fn settings_outcome(
    result: Result<Vec<(String, Value)>, ToolError>,
) -> (McpReply, Vec<(String, Value)>) {
    match result {
        Ok(done) => (Ok(changed_reply(&done)), done),
        Err(e) => (Err(e), Vec::new()),
    }
}

/// 动作名对应的运行时事件；不在白名单里返回 `None`。
///
/// # 参数
/// - `action`：`snow_shot_app_action` 的动作名。
pub fn action_event(action: &str) -> Option<UiEvent> {
    Some(match action {
        "open_settings" => UiEvent::OpenSettings,
        "open_main_window" => UiEvent::OpenMainWindow,
        "open_history" => UiEvent::OpenHistory,
        "open_pin_manage" => UiEvent::OpenPinManage,
        "open_translate_input" => UiEvent::OpenTranslateInput,
        "pin_clipboard" => UiEvent::PinFromClipboard,
        _ => return None,
    })
}

/// 由配置得出授权的权限域：只读恒开，截图默认开，控制默认关。
///
/// # 参数
/// - `document`：配置文档。
pub fn scopes_from_document(document: &ConfigDocument) -> Vec<Scope> {
    let mut scopes = vec![Scope::ReadOnly];
    if document
        .value(KEY_MCP_ALLOW_CAPTURE)
        .as_bool()
        .unwrap_or(true)
    {
        scopes.push(Scope::Capture);
    }
    if document
        .value(KEY_MCP_ALLOW_CONTROL)
        .as_bool()
        .unwrap_or(false)
    {
        scopes.push(Scope::Control);
    }
    scopes
}

/// 数据目录下各一级子项的占用（字节），遍历条目数有上限。
///
/// # 参数
/// - `root`：数据根目录。
/// - `limit`：最多遍历的条目数。
///
/// # 返回
/// `{"root_exists", "entries": [{"name","bytes"}], "total_bytes", "truncated"}`。
pub fn storage_summary(root: &Path, limit: usize) -> Value {
    let mut budget = limit;
    let mut entries = Vec::new();
    let mut total = 0u64;
    if let Ok(read) = std::fs::read_dir(root) {
        let mut children: Vec<_> = read.filter_map(Result::ok).collect();
        children.sort_by_key(|c| c.file_name());
        for child in children {
            let bytes = path_size(&child.path(), &mut budget);
            total += bytes;
            entries.push(json!({"name": child.file_name().to_string_lossy(), "bytes": bytes}));
        }
    }
    json!({
        "root_exists": root.exists(),
        "entries": entries,
        "total_bytes": total,
        "truncated": budget == 0,
    })
}

/// 递归累计路径大小；每访问一个条目消耗一点预算，预算用尽即停。
fn path_size(path: &Path, budget: &mut usize) -> u64 {
    if *budget == 0 {
        return 0;
    }
    *budget -= 1;
    let Ok(meta) = std::fs::symlink_metadata(path) else {
        return 0;
    };
    if meta.is_dir() {
        let Ok(read) = std::fs::read_dir(path) else {
            return 0;
        };
        read.filter_map(Result::ok)
            .map(|c| path_size(&c.path(), budget))
            .sum()
    } else {
        meta.len()
    }
}

/// 翻译语言目录。
fn catalog_json() -> Value {
    let all = [
        Lang::Auto,
        Lang::ZhHans,
        Lang::ZhHant,
        Lang::En,
        Lang::Ja,
        Lang::Ko,
        Lang::Fr,
        Lang::De,
        Lang::Es,
        Lang::Ru,
        Lang::It,
        Lang::Pt,
        Lang::Tr,
        Lang::Ar,
    ];
    let languages: Vec<Value> = all
        .iter()
        .map(|l| json!({"code": l.code(), "name": l.display_name()}))
        .collect();
    json!({"languages": languages})
}

/// 给 MCP 服务线程用的数据来源：设置以快照形式共享，主线程能力走收件箱请求-响应。
pub struct ShellBackend {
    /// 设置快照（配置变化时由主线程刷新）。
    settings: Arc<Mutex<BTreeMap<String, Value>>>,
    /// 主线程收件箱。
    inbox: MainThreadInbox<UiEvent>,
    /// 应用数据根目录（存储概况用）。
    data_root: PathBuf,
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

    /// 显示器列表：主线程枚举。
    fn displays(&self) -> McpReply {
        round_trip(&self.inbox, McpRequestKind::Displays, MAIN_THREAD_TIMEOUT)
    }

    /// 批量写设置：主线程校验并落盘。
    fn settings_update(&self, changes: Vec<(String, Value)>) -> McpReply {
        round_trip(
            &self.inbox,
            McpRequestKind::SettingsUpdate(changes),
            MAIN_THREAD_TIMEOUT,
        )
    }

    /// 批量恢复默认：主线程执行。
    fn settings_reset(&self, keys: Vec<String>) -> McpReply {
        round_trip(
            &self.inbox,
            McpRequestKind::SettingsReset(keys),
            MAIN_THREAD_TIMEOUT,
        )
    }

    /// 白名单动作：直接投对应的运行时事件，不等待执行结果。
    fn app_action(&self, action: &str) -> McpReply {
        let Some(event) = action_event(action) else {
            return Err(ToolError {
                code: "unknown_action",
                message: format!("unknown action: {action}"),
            });
        };
        if !self.inbox.push(event) {
            return Err(ToolError {
                code: "app_closing",
                message: "application is shutting down".into(),
            });
        }
        Ok(json!({"accepted": action}))
    }

    /// 数据目录占用概况。
    fn storage_status(&self) -> Value {
        storage_summary(&self.data_root, STORAGE_SCAN_LIMIT)
    }

    /// 翻译语言目录。
    fn translation_catalog(&self) -> Value {
        catalog_json()
    }
}

/// MCP 服务宿主。
pub struct McpHost {
    /// 与后端共享的设置快照。
    settings: Arc<Mutex<BTreeMap<String, Value>>>,
    /// 主线程收件箱（交给后端做请求-响应）。
    inbox: MainThreadInbox<UiEvent>,
    /// 运行中的服务（未启用为 `None`）。
    #[cfg(windows)]
    service: Option<snow_mcp::service::McpService>,
    /// 当前服务启动时授权的权限域（变化时重启服务以换新令牌与描述符）。
    #[cfg(windows)]
    granted: Vec<Scope>,
}

impl McpHost {
    /// 创建（不启动）。
    ///
    /// # 参数
    /// - `inbox`：主线程收件箱。
    pub fn new(inbox: MainThreadInbox<UiEvent>) -> Self {
        Self {
            settings: Arc::new(Mutex::new(BTreeMap::new())),
            inbox,
            #[cfg(windows)]
            service: None,
            #[cfg(windows)]
            granted: Vec::new(),
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

    /// 按配置对齐状态：开关打开则启动（已启动则只刷新快照），关闭则停止；授权域变化时重启。
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
            let granted = scopes_from_document(document);
            if self.service.is_some() {
                if granted == self.granted {
                    return;
                }
                // 授权域变了：先关旧服务（旧令牌作废），再按新授权重新启动
                self.service = None;
            }
            let backend = Arc::new(ShellBackend {
                settings: Arc::clone(&self.settings),
                inbox: self.inbox.clone(),
                data_root: data_root.to_path_buf(),
            });
            let config = snow_mcp::service::ServiceConfig {
                data_root: data_root.to_path_buf(),
                server: snow_mcp::tools::ServerInfo {
                    name: snow_app_core::PRODUCT_NAME.to_string(),
                    version: env!("CARGO_PKG_VERSION").to_string(),
                },
                granted: granted.clone(),
                pipe_name: None,
            };
            match snow_mcp::service::McpService::start(config, backend) {
                Ok(service) => {
                    self.service = Some(service);
                    self.granted = granted;
                }
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

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use snow_config::store::ConfigStore;
    use snow_ui::shell::geometry::{PhysicalRect, ScaleFactor};
    use snow_ui::shell::monitor::MonitorId;
    use std::cell::RefCell;
    use std::rc::Rc;

    /// 构造后端（设置快照可注入）。
    fn backend(settings: BTreeMap<String, Value>, inbox: MainThreadInbox<UiEvent>) -> ShellBackend {
        ShellBackend {
            settings: Arc::new(Mutex::new(settings)),
            inbox,
            data_root: std::env::temp_dir(),
        }
    }

    /// 临时配置存储（路径唯一）。
    fn temp_store(tag: &str) -> (SharedConfig, PathBuf) {
        let dir = std::env::temp_dir().join(format!("cisox-mcp-host-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("config.json");
        (Rc::new(RefCell::new(ConfigStore::open(&path))), dir)
    }

    /// 后端：设置快照可读，更新模式来自快照，状态含版本。
    #[test]
    fn backend_reads_snapshot() {
        let settings = BTreeMap::from([(KEY_UPDATE_MODE.to_string(), json!("download"))]);
        let backend = backend(settings, MainThreadInbox::new());
        assert_eq!(backend.updates_status()["mode"], "download");
        assert!(backend.app_status()["version"].is_string());
        assert_eq!(backend.settings_snapshot().len(), 1);
    }

    /// 默认关闭：不启动、不留快照。
    #[test]
    fn disabled_by_default_does_not_start() {
        let mut host = McpHost::new(MainThreadInbox::new());
        let doc = ConfigDocument::from_bytes(None);
        let root = std::env::temp_dir().join(format!("cisox-mcp-host-{}", std::process::id()));
        host.sync(&doc, &root);
        assert!(!host.is_running());
        assert!(!root.exists());
    }

    /// 授权域：默认只读 + 截图；打开控制后含控制；关掉截图后只剩只读。
    #[test]
    fn scopes_follow_config() {
        let mut doc = ConfigDocument::from_bytes(None);
        assert_eq!(
            scopes_from_document(&doc),
            vec![Scope::ReadOnly, Scope::Capture]
        );
        doc.set_value(KEY_MCP_ALLOW_CONTROL, json!(true)).unwrap();
        assert_eq!(
            scopes_from_document(&doc),
            vec![Scope::ReadOnly, Scope::Capture, Scope::Control]
        );
        doc.set_value(KEY_MCP_ALLOW_CAPTURE, json!(false)).unwrap();
        assert_eq!(
            scopes_from_document(&doc),
            vec![Scope::ReadOnly, Scope::Control]
        );
    }

    /// 请求-响应：另一线程扮演主线程回复；无人回复则超时；收件箱关闭立即失败。
    #[test]
    fn round_trip_replies_times_out_and_detects_close() {
        let inbox: MainThreadInbox<UiEvent> = MainThreadInbox::new();
        let responder = inbox.clone();
        let handle = std::thread::spawn(move || {
            for _ in 0..200 {
                if let Some(UiEvent::Mcp(req)) = responder.try_recv() {
                    req.respond(Ok(json!({"pong": true})));
                    return;
                }
                std::thread::sleep(Duration::from_millis(10));
            }
        });
        let reply = round_trip(&inbox, McpRequestKind::Displays, Duration::from_secs(3)).unwrap();
        assert_eq!(reply["pong"], true);
        handle.join().unwrap();

        let lonely: MainThreadInbox<UiEvent> = MainThreadInbox::new();
        let err =
            round_trip(&lonely, McpRequestKind::Displays, Duration::from_millis(50)).unwrap_err();
        assert_eq!(err.code, "main_thread_timeout");

        lonely.close();
        let err =
            round_trip(&lonely, McpRequestKind::Displays, Duration::from_millis(50)).unwrap_err();
        assert_eq!(err.code, "app_closing");
    }

    /// 动作白名单与事件映射一一对应；未知动作不产生事件，且后端把动作原样投进收件箱。
    #[test]
    fn actions_map_to_events() {
        for action in snow_mcp::tools::APP_ACTIONS {
            assert!(action_event(action).is_some(), "{action}");
        }
        assert!(action_event("quit").is_none());
        let inbox: MainThreadInbox<UiEvent> = MainThreadInbox::new();
        let b = backend(BTreeMap::new(), inbox.clone());
        assert_eq!(
            b.app_action("open_settings").unwrap()["accepted"],
            "open_settings"
        );
        assert_eq!(inbox.try_recv(), Some(UiEvent::OpenSettings));
        assert_eq!(b.app_action("restart").unwrap_err().code, "unknown_action");
        assert_eq!(inbox.try_recv(), None);
    }

    /// 显示器 JSON：字段齐全，缩放取浮点倍数。
    #[test]
    fn displays_json_shape() {
        let monitor = MonitorInfo {
            id: MonitorId(7),
            name: r"\\.\DISPLAY1".into(),
            bounds: PhysicalRect {
                x: 0,
                y: 0,
                width: 1920,
                height: 1080,
            },
            work_area: PhysicalRect {
                x: 0,
                y: 0,
                width: 1920,
                height: 1040,
            },
            scale: ScaleFactor::new(1.5),
            is_primary: true,
        };
        let v = displays_json(&[monitor]);
        let d = &v["displays"][0];
        assert_eq!(d["id"], 7);
        assert_eq!(d["primary"], true);
        assert_eq!(d["scale"], 1.5);
        assert_eq!(d["bounds"]["width"], 1920);
        assert_eq!(d["work_area"]["height"], 1040);
    }

    /// 写设置：合法值落盘并报告旧值；不合法 / 未知键整体不生效；重置回默认。
    #[test]
    fn apply_settings_is_atomic() {
        let (store, dir) = temp_store("apply");
        let key = "screen_recording/frame_rate";
        let before = store.borrow().value(key);
        let done = apply_settings(&store, vec![(key.into(), json!(60))]).unwrap();
        assert_eq!(done.len(), 1);
        assert_eq!(done[0].1, before);
        assert_eq!(store.borrow().value(key), json!(60));

        // 第二个键不合法：第一个键也不能被写入
        let err = apply_settings(
            &store,
            vec![
                (key.into(), json!(30)),
                ("screen_recording/frame_rate_nope".into(), json!(1)),
            ],
        )
        .unwrap_err();
        assert_eq!(err.code, "unknown_setting");
        assert_eq!(store.borrow().value(key), json!(60));
        let err = apply_settings(&store, vec![(key.into(), json!("fast"))]).unwrap_err();
        assert_eq!(err.code, "invalid_value");
        assert!(!err.message.contains("fast"), "错误信息不回显值");

        // 无变化不报告
        assert!(
            apply_settings(&store, vec![(key.into(), json!(60))])
                .unwrap()
                .is_empty()
        );

        // 经请求处理器重置回默认，并给出广播项
        let (tx, _rx) = mpsc::channel();
        let req = McpRequest::new(McpRequestKind::SettingsReset(vec![key.into()]), tx);
        let (reply, broadcast) = handle_request(&req, || Ok::<_, String>(Vec::new()), &store);
        assert_eq!(reply.unwrap()["changed"][0], key);
        assert_eq!(broadcast.len(), 1);
        assert_eq!(store.borrow().value(key), default_value(key));
        let req = McpRequest::new(
            McpRequestKind::SettingsReset(vec!["x/y".into()]),
            mpsc::channel().0,
        );
        let (reply, broadcast) = handle_request(&req, || Ok::<_, String>(Vec::new()), &store);
        assert_eq!(reply.unwrap_err().code, "unknown_setting");
        assert!(broadcast.is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 显示器枚举失败变成结构化错误。
    #[test]
    fn displays_failure_is_structured() {
        let (store, dir) = temp_store("displays");
        let req = McpRequest::new(McpRequestKind::Displays, mpsc::channel().0);
        let (reply, _) = handle_request(&req, || Err::<Vec<MonitorInfo>, _>("boom"), &store);
        assert_eq!(reply.unwrap_err().code, "displays_unavailable");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 存储概况：统计各一级子项，预算用尽标记截断。
    #[test]
    fn storage_summary_counts_and_truncates() {
        let dir = std::env::temp_dir().join(format!("cisox-mcp-storage-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("a")).unwrap();
        std::fs::write(dir.join("a").join("x.bin"), [0u8; 10]).unwrap();
        std::fs::write(dir.join("b.bin"), [0u8; 5]).unwrap();
        let v = storage_summary(&dir, 1000);
        assert_eq!(v["total_bytes"], 15);
        assert_eq!(v["entries"].as_array().unwrap().len(), 2);
        assert_eq!(v["truncated"], false);
        assert_eq!(storage_summary(&dir, 1)["truncated"], true);
        assert_eq!(
            storage_summary(&dir.join("missing"), 10)["root_exists"],
            false
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 翻译目录含 14 种语言且带代码。
    #[test]
    fn catalog_lists_languages() {
        let v = catalog_json();
        assert_eq!(v["languages"].as_array().unwrap().len(), 14);
        assert_eq!(v["languages"][1]["code"], "zh-CN");
    }
}
