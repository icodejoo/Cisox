//! 屏幕录制功能模块（Recording Module）。
//!
//! 录制在独立的 `snow-recorder` 进程里完成；本模块提供配置模型、会话状态机、
//! 进程客户端、输出路径解析以及区域视图与悬浮控制条。

pub mod area_view;
pub mod audio;
pub mod client;
pub mod model;
pub mod output;
pub mod runtime;

pub use area_view::{AutoPlan, RecordingAreaAction, RecordingAreaView};
pub use model::{RecordingConfig, RecordingFormat, RecordingState};
pub use runtime::ScreenRecordingSession;
