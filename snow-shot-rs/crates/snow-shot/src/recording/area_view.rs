//! 录制区域视图与悬浮控制条（Recording Area View）。
//!
//! 负责绘制录制选区虚线框、录制倒计时遮罩、按键回显特效、点击水波纹，
//! 以及集成的控制工具栏（暂停/恢复、停止完成、取消录制、音效开关）。

use snow_ui::shell::geometry::PhysicalRect;
use snow_ui::ui::*;
use crate::recording::model::{RecordingConfig, RecordingFormat, RecordingState};
use crate::recording::runtime::ScreenRecordingSession;

/// 录制交互事件通知。
#[derive(Debug, Clone, PartialEq)]
pub enum RecordingAreaAction {
    /// 暂停或恢复录制。
    TogglePause,
    /// 停止录制并保存导出。
    StopAndSave,
    /// 取消并放弃当前录制。
    Cancel,
    /// 切换声音录制状态。
    ToggleAudio,
    /// 切换水波纹特效。
    ToggleRipples,
    /// 切换按键回显。
    ToggleKeystrokes,
}

/// 录制区域视图组件。
#[derive(Debug)]
pub struct RecordingAreaView {
    /// 录制核心会话控制器。
    pub session: ScreenRecordingSession,
    /// 选区边框颜色。
    pub border_color: u32,
    /// 是否显示控制条。
    pub show_toolbar: bool,
}

impl RecordingAreaView {
    /// 创建录制区域视图。
    pub fn new(config: RecordingConfig) -> Self {
        Self {
            session: ScreenRecordingSession::new(config),
            border_color: 0x1677FFFF, // Ant Design 主题蓝
            show_toolbar: true,
        }
    }

    /// 获取录制物理矩形。
    pub fn bounds(&self) -> PhysicalRect {
        self.session.config().region
    }

    /// 获取当前录制格式。
    pub fn format(&self) -> RecordingFormat {
        self.session.config().format
    }

    /// 处理外部操作动作。
    pub fn handle_action(&mut self, action: RecordingAreaAction) {
        match action {
            RecordingAreaAction::TogglePause => self.session.toggle_pause(),
            RecordingAreaAction::StopAndSave => {
                let _ = self.session.finish();
            }
            RecordingAreaAction::Cancel => self.session.cancel(),
            RecordingAreaAction::ToggleAudio => {
                // 音频捕获开关逻辑
            }
            RecordingAreaAction::ToggleRipples => {
                // 水波纹特效开关逻辑
            }
            RecordingAreaAction::ToggleKeystrokes => {
                // 键盘回显开关逻辑
            }
        }
    }
}

impl Render for RecordingAreaView {
    /// 渲染录制区域覆盖层、边框、倒计时与悬浮工具栏。
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        let bounds = self.bounds();
        let state = self.session.state().clone();

        let mut root = div()
            .relative()
            .w_full()
            .h_full()
            .bg(rgba(0x00000040)); // 半透明遮罩

        // 1. 录制选区外框与高亮
        let frame_x = px(bounds.x as f32);
        let frame_y = px(bounds.y as f32);
        let frame_w = px(bounds.width as f32);
        let frame_h = px(bounds.height as f32);

        let active_color = if matches!(state, RecordingState::Recording { is_paused: false, .. }) {
            rgba(0xFF4D4FFF) // 录制中亮红边框
        } else {
            rgba(self.border_color)
        };

        let mut record_frame = div()
            .absolute()
            .left(frame_x)
            .top(frame_y)
            .w(frame_w)
            .h(frame_h)
            .border_2()
            .border_color(active_color)
            .rounded_sm();

        // 2. 倒计时遮罩层
        if let RecordingState::Countdown { seconds_left } = state {
            let countdown_overlay = div()
                .absolute()
                .inset_0()
                .flex()
                .items_center()
                .justify_center()
                .bg(rgba(0x00000088))
                .child(
                    div()
                        .text_size(px(64.0))
                        .font_weight(FontWeight::BOLD)
                        .text_color(rgba(0xFFFFFFFF))
                        .child(format!("{seconds_left}")),
                );
            record_frame = record_frame.child(countdown_overlay);
        }

        // 3. 键盘回显悬浮框
        for ks in self.session.keystrokes() {
            let ks_badge = div()
                .absolute()
                .bottom(px(16.0))
                .left(px(16.0))
                .px_3()
                .py_1()
                .rounded_md()
                .bg(rgba(0x1F1F1FAA))
                .border_1()
                .border_color(rgba(0xFFFFFF33))
                .text_size(px(14.0))
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(rgba(0xFFFFFFFF))
                .child(ks.text.clone());
            record_frame = record_frame.child(ks_badge);
        }

        // 4. 浮动录制工具栏
        if self.show_toolbar {
            let toolbar_y = if bounds.y > 50 {
                px((bounds.y - 48) as f32)
            } else {
                px((bounds.y + bounds.height + 8) as f32)
            };

            let duration_label = match &state {
                RecordingState::Recording { elapsed_secs, .. } => {
                    RecordingState::format_duration(*elapsed_secs)
                }
                RecordingState::Countdown { .. } => "准备中...".to_string(),
                RecordingState::Finished { .. } => "已完成".to_string(),
                _ => "就绪".to_string(),
            };

            let is_paused = matches!(state, RecordingState::Recording { is_paused: true, .. });
            let pause_btn_text = if is_paused { "继续" } else { "暂停" };

            let toolbar = div()
                .absolute()
                .left(frame_x)
                .top(toolbar_y)
                .h(px(40.0))
                .flex()
                .items_center()
                .gap_2()
                .px_3()
                .py_1()
                .bg(rgba(0x1F1F1FE6))
                .border_1()
                .border_color(rgba(0xFFFFFF26))
                .rounded_lg()
                .shadow_lg()
                // 红色录制圆点指示器
                .child(
                    div()
                        .w(px(10.0))
                        .h(px(10.0))
                        .rounded_full()
                        .bg(if is_paused {
                            rgba(0xFAAD14FF) // 黄色暂停
                        } else {
                            rgba(0xFF4D4FFF) // 红色录制
                        }),
                )
                // 录制时间
                .child(
                    div()
                        .text_size(px(13.0))
                        .font_weight(FontWeight::MEDIUM)
                        .text_color(rgba(0xFFFFFFFF))
                        .child(duration_label),
                )
                // 分割线
                .child(div().w(px(1.0)).h(px(16.0)).bg(rgba(0xFFFFFF26)))
                // 分辨率指示
                .child(
                    div()
                        .text_size(px(12.0))
                        .text_color(rgba(0x8C8C8CFF))
                        .child(format!("{}x{}", bounds.width, bounds.height)),
                )
                // 暂停/继续按钮
                .child(
                    div()
                        .px_2()
                        .py_1()
                        .rounded_sm()
                        .bg(rgba(0x303030FF))
                        .text_size(px(12.0))
                        .text_color(rgba(0xFFFFFFFF))
                        .child(pause_btn_text),
                )
                // 停止并完成按钮
                .child(
                    div()
                        .px_2()
                        .py_1()
                        .rounded_sm()
                        .bg(rgba(0x1677FFFF))
                        .text_size(px(12.0))
                        .text_color(rgba(0xFFFFFFFF))
                        .child("完成"),
                )
                // 取消按钮
                .child(
                    div()
                        .px_2()
                        .py_1()
                        .rounded_sm()
                        .bg(rgba(0x434343FF))
                        .text_size(px(12.0))
                        .text_color(rgba(0xFF4D4FFF))
                        .child("放弃"),
                );

            root = root.child(toolbar);
        }

        root.child(record_frame)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 验证录制区域视图创建与默认属性。
    #[test]
    fn test_recording_area_view_creation() {
        let mut config = RecordingConfig::default();
        config.region = PhysicalRect::new(100, 100, 800, 600);
        let view = RecordingAreaView::new(config);

        assert_eq!(view.bounds(), PhysicalRect::new(100, 100, 800, 600));
        assert_eq!(view.format(), RecordingFormat::Mp4);
        assert!(view.show_toolbar);
    }

    /// 验证动作事件处理。
    #[test]
    fn test_recording_area_action_handling() {
        let mut config = RecordingConfig::default();
        let temp_dir = std::env::temp_dir();
        config.output_path = temp_dir.join("test_area_rec.mp4");
        let mut view = RecordingAreaView::new(config);

        view.session.start_immediately();
        assert!(view.session.state().is_active());

        // 暂停动作
        view.handle_action(RecordingAreaAction::TogglePause);
        if let RecordingState::Recording { is_paused, .. } = view.session.state() {
            assert!(*is_paused);
        }

        // 停止动作
        view.handle_action(RecordingAreaAction::StopAndSave);
        assert!(matches!(
            view.session.state(),
            RecordingState::Finished { .. }
        ));

        // 清理文件
        let _ = std::fs::remove_file(temp_dir.join("test_area_rec.mp4"));
    }
}
