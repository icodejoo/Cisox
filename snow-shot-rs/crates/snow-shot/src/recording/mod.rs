//! 屏幕录制功能模块（Recording Module）。
//!
//! 提供配置模型、生命周期管理、会话运行时、区域选择与悬浮工具栏渲染。

pub mod area_view;
pub mod model;
pub mod runtime;

pub use area_view::{RecordingAreaAction, RecordingAreaView};
pub use model::{RecordingConfig, RecordingFormat, RecordingState};
pub use runtime::{ClickRipple, KeystrokeDisplay, ScreenRecordingSession};
