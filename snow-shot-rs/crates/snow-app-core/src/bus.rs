//! 命令总线：同步派发、线程安全、无需 UI 即可驱动（方案 §2.3）。

use crate::command::{AppCommand, CommandContext, CommandKind, CommandSource};
use std::collections::HashMap;
use std::fmt;
use std::sync::{Arc, Mutex};

/// 命令处理成功的结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CommandOutcome {
    /// 已处理，无返回内容。
    Done,
    /// 已处理，附带文本反馈。
    Message(String),
}

/// 命令派发或处理失败的原因。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CommandError {
    /// 该种类命令没有注册 handler。
    NoHandler(CommandKind),
    /// handler 拒绝处理（如能力不可用、会话版本不符）。
    Rejected(String),
    /// handler 内部错误。
    Internal(String),
}

impl fmt::Display for CommandError {
    /// 输出面向日志的错误描述。
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            CommandError::NoHandler(kind) => write!(f, "命令未注册处理器: {kind:?}"),
            CommandError::Rejected(msg) => write!(f, "命令被拒绝: {msg}"),
            CommandError::Internal(msg) => write!(f, "命令内部错误: {msg}"),
        }
    }
}

impl std::error::Error for CommandError {}

/// 命令处理器类型。
pub type CommandHandler =
    Arc<dyn Fn(&CommandContext, &AppCommand) -> Result<CommandOutcome, CommandError> + Send + Sync>;

/// 命令总线，可克隆共享（克隆体指向同一张 handler 表）。
#[derive(Clone, Default)]
pub struct CommandBus {
    /// 命令种类到处理器的映射。
    handlers: Arc<Mutex<HashMap<CommandKind, CommandHandler>>>,
}

impl CommandBus {
    /// 创建空总线。
    ///
    /// # 返回
    /// 未注册任何 handler 的总线。
    ///
    /// ```rust
    /// use snow_app_core::bus::CommandBus;
    /// let _bus = CommandBus::new();
    /// ```
    pub fn new() -> Self {
        Self::default()
    }

    /// 注册处理器，同种类重复注册则覆盖。
    ///
    /// # 参数
    /// - `kind`：命令种类。
    /// - `handler`：处理器。
    ///
    /// # 返回
    /// 此前已有处理器被覆盖时为 `true`。
    ///
    /// ```rust
    /// use snow_app_core::bus::{CommandBus, CommandOutcome};
    /// use snow_app_core::command::CommandKind;
    /// use std::sync::Arc;
    /// let bus = CommandBus::new();
    /// let replaced = bus.register(CommandKind::Cancel, Arc::new(|_, _| Ok(CommandOutcome::Done)));
    /// assert!(!replaced);
    /// ```
    pub fn register(&self, kind: CommandKind, handler: CommandHandler) -> bool {
        self.lock().insert(kind, handler).is_some()
    }

    /// 注销处理器。
    ///
    /// # 参数
    /// - `kind`：命令种类。
    ///
    /// # 返回
    /// 确实移除了处理器时为 `true`。
    ///
    /// ```rust
    /// use snow_app_core::bus::CommandBus;
    /// use snow_app_core::command::CommandKind;
    /// assert!(!CommandBus::new().unregister(CommandKind::Cancel));
    /// ```
    pub fn unregister(&self, kind: CommandKind) -> bool {
        self.lock().remove(&kind).is_some()
    }

    /// 查询某种命令是否已有处理器。
    ///
    /// # 参数
    /// - `kind`：命令种类。
    ///
    /// # 返回
    /// 已注册为 `true`。
    ///
    /// ```rust
    /// use snow_app_core::bus::CommandBus;
    /// use snow_app_core::command::CommandKind;
    /// assert!(!CommandBus::new().has_handler(CommandKind::Undo));
    /// ```
    pub fn has_handler(&self, kind: CommandKind) -> bool {
        self.lock().contains_key(&kind)
    }

    /// 同步派发命令；调用 handler 前已释放锁，handler 内可再次 `emit`。
    ///
    /// # 参数
    /// - `ctx`：命令上下文（来源与会话元数据）。
    /// - `cmd`：命令。
    ///
    /// # 返回
    /// handler 的结果；未注册返回 `CommandError::NoHandler`。
    ///
    /// ```rust
    /// use snow_app_core::bus::{CommandBus, CommandError};
    /// use snow_app_core::command::{AppCommand, CommandContext, CommandKind, CommandSource};
    /// let bus = CommandBus::new();
    /// let ctx = CommandContext::new(CommandSource::Test);
    /// assert_eq!(
    ///     bus.emit(&ctx, AppCommand::Cancel(Default::default())),
    ///     Err(CommandError::NoHandler(CommandKind::Cancel))
    /// );
    /// ```
    pub fn emit(
        &self,
        ctx: &CommandContext,
        cmd: AppCommand,
    ) -> Result<CommandOutcome, CommandError> {
        let kind = cmd.kind();
        let handler = self.lock().get(&kind).cloned();
        match handler {
            Some(h) => h(ctx, &cmd),
            None => Err(CommandError::NoHandler(kind)),
        }
    }

    /// 以指定来源派发命令（无会话元数据的便捷方法）。
    ///
    /// # 参数
    /// - `source`：命令来源。
    /// - `cmd`：命令。
    ///
    /// # 返回
    /// 同 [`CommandBus::emit`]。
    ///
    /// ```rust
    /// use snow_app_core::bus::CommandBus;
    /// use snow_app_core::command::{AppCommand, CommandSource};
    /// assert!(CommandBus::new().emit_from(CommandSource::Mcp, AppCommand::Undo(Default::default())).is_err());
    /// ```
    pub fn emit_from(
        &self,
        source: CommandSource,
        cmd: AppCommand,
    ) -> Result<CommandOutcome, CommandError> {
        self.emit(&CommandContext::new(source), cmd)
    }

    /// 取 handler 表锁；锁中毒时恢复内部数据，避免 panic 扩散。
    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<CommandKind, CommandHandler>> {
        self.handlers.lock().unwrap_or_else(|e| e.into_inner())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::command::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// 构造各命令种类的样例命令（StartRecording 不在 MCP 映射内，亦覆盖）。
    fn sample(kind: CommandKind) -> AppCommand {
        match kind {
            CommandKind::Capture => AppCommand::Capture(CaptureRequest::default()),
            CommandKind::DirectCapture => AppCommand::DirectCapture(DirectCaptureRequest {
                target: DirectTarget::CurrentMonitor,
                output: DirectOutput::Render,
                capture_cursor: None,
                scale: None,
                path: None,
                automatic_path: None,
                format: None,
                quality: None,
                compression_level: None,
                pdf_page_size: None,
                pdf_title: None,
            }),
            CommandKind::QueryState => AppCommand::QueryState,
            CommandKind::McpStatus => AppCommand::McpStatus,
            CommandKind::SetSelection => AppCommand::SetSelection(SelectionRequest {
                operation: None,
                region_type: RegionType::Rectangle,
                bounds: Some([0.0, 0.0, 10.0, 10.0]),
                points: None,
            }),
            CommandKind::SelectTool => AppCommand::SelectTool(ToolKind::Move),
            CommandKind::SetSelectionStyle => {
                AppCommand::SetSelectionStyle(SelectionStyleRequest::default())
            }
            CommandKind::SetToolStyle => AppCommand::SetToolStyle(ToolStyleRequest::default()),
            CommandKind::ApplyAnnotations => {
                AppCommand::ApplyAnnotations(AnnotationsRequest::default())
            }
            CommandKind::EditElements => AppCommand::EditElements(EditElementsRequest::default()),
            CommandKind::DrawTemplate => AppCommand::DrawTemplate(DrawTemplateRequest::default()),
            CommandKind::AutoFilter => AppCommand::AutoFilter(AutoFilterRequest::default()),
            CommandKind::Undo => AppCommand::Undo(Default::default()),
            CommandKind::Redo => AppCommand::Redo(Default::default()),
            CommandKind::Render => AppCommand::Render(RenderRequest::default()),
            CommandKind::Export => AppCommand::Export(ExportTarget::Copy),
            CommandKind::PinSelection => AppCommand::PinSelection,
            CommandKind::Finish => AppCommand::Finish(Default::default()),
            CommandKind::Cancel => AppCommand::Cancel(Default::default()),
            CommandKind::Recapture => AppCommand::Recapture,
            CommandKind::Scrolling => AppCommand::Scrolling(ScrollingRequest {
                action: ScrollAction::Start,
                axis: None,
                enabled: None,
                offset: None,
                start: None,
                end: None,
            }),
            CommandKind::ScrollOnce => AppCommand::ScrollOnce(ScrollOnceRequest {
                direction: ScrollDirection::Down,
            }),
            CommandKind::RunOcr => AppCommand::RunOcr(OcrRequest {
                kind: RecognitionKind::Text,
            }),
            CommandKind::Translate => AppCommand::Translate(TranslateRequest::default()),
            CommandKind::EditRecognition => {
                AppCommand::EditRecognition(EditRecognitionRequest::default())
            }
            CommandKind::ExportRecognition => {
                AppCommand::ExportRecognition(ExportRecognitionRequest::default())
            }
            CommandKind::RunOperation => AppCommand::RunOperation(OperationRequest {
                operation_id: "op".to_string(),
            }),
            CommandKind::StartRecording => AppCommand::StartRecording(RecordingConfig::default()),
            CommandKind::OpenTranslateInput => AppCommand::OpenTranslateInput,
            CommandKind::ToggleDictation => AppCommand::ToggleDictation,
            CommandKind::StartDictation => AppCommand::StartDictation,
            CommandKind::StopDictation => AppCommand::StopDictation,
            CommandKind::Global => AppCommand::Global(GlobalAction::OpenSettings),
        }
    }

    /// 未注册时返回 NoHandler 而非 panic。
    #[test]
    fn emit_without_handler_errors() {
        let bus = CommandBus::new();
        let err = bus
            .emit_from(CommandSource::Test, AppCommand::Cancel(Default::default()))
            .unwrap_err();
        assert_eq!(err, CommandError::NoHandler(CommandKind::Cancel));
        assert!(!err.to_string().is_empty());
    }

    /// handler 能读到命令来源，并可注销。
    #[test]
    fn handler_sees_source_and_unregister() {
        let bus = CommandBus::new();
        bus.register(
            CommandKind::Undo,
            Arc::new(|ctx, _| Ok(CommandOutcome::Message(format!("{:?}", ctx.source)))),
        );
        let out = bus.emit_from(CommandSource::Hotkey, AppCommand::Undo(Default::default()));
        assert_eq!(out, Ok(CommandOutcome::Message("Hotkey".to_string())));
        assert!(bus.unregister(CommandKind::Undo));
        assert!(!bus.has_handler(CommandKind::Undo));
    }

    /// 重复注册覆盖旧 handler。
    #[test]
    fn register_overrides() {
        let bus = CommandBus::new();
        assert!(!bus.register(CommandKind::Redo, Arc::new(|_, _| Ok(CommandOutcome::Done))));
        assert!(bus.register(
            CommandKind::Redo,
            Arc::new(|_, _| Err(CommandError::Rejected("x".into())))
        ));
        assert_eq!(
            bus.emit_from(CommandSource::Test, AppCommand::Redo(Default::default())),
            Err(CommandError::Rejected("x".into()))
        );
    }

    /// handler 内再次 emit 不死锁。
    #[test]
    fn reentrant_emit_no_deadlock() {
        let bus = CommandBus::new();
        let inner = bus.clone();
        bus.register(CommandKind::Redo, Arc::new(|_, _| Ok(CommandOutcome::Done)));
        bus.register(
            CommandKind::Undo,
            Arc::new(move |ctx, _| inner.emit(ctx, AppCommand::Redo(Default::default()))),
        );
        assert_eq!(
            bus.emit_from(CommandSource::Test, AppCommand::Undo(Default::default())),
            Ok(CommandOutcome::Done)
        );
    }

    /// 多线程并发派发计数正确。
    #[test]
    fn concurrent_emit() {
        let bus = CommandBus::new();
        let counter = Arc::new(AtomicUsize::new(0));
        let c = counter.clone();
        bus.register(
            CommandKind::Finish,
            Arc::new(move |_, _| {
                c.fetch_add(1, Ordering::SeqCst);
                Ok(CommandOutcome::Done)
            }),
        );
        let handles: Vec<_> = (0..16)
            .map(|_| {
                let b = bus.clone();
                std::thread::spawn(move || {
                    b.emit_from(CommandSource::Test, AppCommand::Finish(Default::default()))
                })
            })
            .collect();
        for h in handles {
            assert_eq!(h.join().unwrap(), Ok(CommandOutcome::Done));
        }
        assert_eq!(counter.load(Ordering::SeqCst), 16);
    }

    /// e2e 风格：绕过 UI，用 Test 来源依次驱动 截图 -> 选工具 -> 导出 -> 完成。
    #[test]
    fn e2e_drive_without_ui() {
        let bus = CommandBus::new();
        let log = Arc::new(Mutex::new(Vec::<String>::new()));
        let record = |bus: &CommandBus, kind: CommandKind| {
            let l = log.clone();
            bus.register(
                kind,
                Arc::new(move |ctx, cmd| {
                    l.lock()
                        .unwrap()
                        .push(format!("{:?}@{:?}", cmd.kind(), ctx.source));
                    Ok(CommandOutcome::Done)
                }),
            );
        };
        for kind in [
            CommandKind::Capture,
            CommandKind::SelectTool,
            CommandKind::Export,
            CommandKind::Finish,
        ] {
            record(&bus, kind);
        }
        let cmds = [
            AppCommand::Capture(CaptureRequest::default()),
            AppCommand::SelectTool(ToolKind::Rectangle),
            AppCommand::Export(ExportTarget::Save(SaveRequest::default())),
            AppCommand::Finish(Default::default()),
        ];
        for cmd in cmds {
            bus.emit_from(CommandSource::Test, cmd).unwrap();
        }
        assert_eq!(
            *log.lock().unwrap(),
            [
                "Capture@Test",
                "SelectTool@Test",
                "Export@Test",
                "Finish@Test"
            ]
        );
    }

    /// 28 个 MCP tool 对应的命令，注册后均可经总线派发，且样例命令种类与映射一致。
    #[test]
    fn every_mcp_tool_dispatches() {
        let bus = CommandBus::new();
        for (_, kind) in MCP_TOOL_MAP {
            bus.register(*kind, Arc::new(|_, _| Ok(CommandOutcome::Done)));
        }
        for (tool, kind) in MCP_TOOL_MAP {
            let cmd = sample(*kind);
            assert_eq!(cmd.kind(), *kind, "{tool}");
            assert_eq!(
                bus.emit_from(CommandSource::Mcp, cmd),
                Ok(CommandOutcome::Done),
                "{tool}"
            );
        }
    }

    /// StartRecording 未注册时同样返回明确错误。
    #[test]
    fn start_recording_unregistered() {
        let bus = CommandBus::new();
        assert_eq!(
            bus.emit_from(CommandSource::Ui, sample(CommandKind::StartRecording)),
            Err(CommandError::NoHandler(CommandKind::StartRecording))
        );
    }

    /// 语音转文字三条命令未注册时返回明确错误，注册后可由热键来源派发。
    #[test]
    fn dictation_commands_dispatch() {
        let bus = CommandBus::new();
        for kind in [
            CommandKind::ToggleDictation,
            CommandKind::StartDictation,
            CommandKind::StopDictation,
        ] {
            assert_eq!(
                bus.emit_from(CommandSource::Hotkey, sample(kind)),
                Err(CommandError::NoHandler(kind))
            );
            bus.register(
                kind,
                std::sync::Arc::new(|ctx, _| {
                    assert_eq!(ctx.source, CommandSource::Hotkey);
                    Ok(CommandOutcome::Done)
                }),
            );
            assert_eq!(
                bus.emit_from(CommandSource::Hotkey, sample(kind)),
                Ok(CommandOutcome::Done)
            );
        }
    }

    /// OpenTranslateInput 未注册时返回明确错误，注册后可派发。
    #[test]
    fn open_translate_input_dispatch() {
        let bus = CommandBus::new();
        assert_eq!(
            bus.emit_from(
                CommandSource::Hotkey,
                sample(CommandKind::OpenTranslateInput)
            ),
            Err(CommandError::NoHandler(CommandKind::OpenTranslateInput))
        );
        bus.register(
            CommandKind::OpenTranslateInput,
            std::sync::Arc::new(|ctx, _| {
                assert_eq!(ctx.source, CommandSource::Hotkey);
                Ok(CommandOutcome::Done)
            }),
        );
        assert_eq!(
            bus.emit_from(
                CommandSource::Hotkey,
                sample(CommandKind::OpenTranslateInput)
            ),
            Ok(CommandOutcome::Done)
        );
    }

    /// 全局快捷键动作命令未注册时返回明确错误，注册后可由热键来源派发；闸门控制动作有标记。
    #[test]
    fn global_action_dispatch() {
        let bus = CommandBus::new();
        let command = AppCommand::Global(GlobalAction::ToggleGlobalHotkeys);
        assert_eq!(
            bus.emit_from(CommandSource::Hotkey, command.clone()),
            Err(CommandError::NoHandler(CommandKind::Global))
        );
        bus.register(
            CommandKind::Global,
            std::sync::Arc::new(|ctx, cmd| {
                assert_eq!(ctx.source, CommandSource::Hotkey);
                assert!(matches!(cmd, AppCommand::Global(a) if a.controls_gate()));
                Ok(CommandOutcome::Done)
            }),
        );
        assert_eq!(
            bus.emit_from(CommandSource::Hotkey, command),
            Ok(CommandOutcome::Done)
        );
        assert!(!GlobalAction::OpenSettings.controls_gate());
    }
}
