//! 屏幕录制运行时引擎与会话管理（Recording Runtime）。
//!
//! 负责协调录制倒计时、采样帧步进、时长统计、按键回显与鼠标点击水波纹动画生命周期。

use std::fs;
use std::path::PathBuf;
use std::time::Instant;
use crate::recording::model::{RecordingConfig, RecordingState};

/// 鼠标点击水波纹动画特效实体。
#[derive(Debug, Clone, PartialEq)]
pub struct ClickRipple {
    /// 触发点击物理坐标 (x, y)。
    pub position: (i32, i32),
    /// 当前波纹扩散半径。
    pub radius: f32,
    /// 最大扩散半径。
    pub max_radius: f32,
    /// 当前不透明度 (0.0 ..= 1.0)。
    pub alpha: f32,
}

impl ClickRipple {
    /// 创建一个新的水波纹动画。
    pub fn new(x: i32, y: i32) -> Self {
        Self {
            position: (x, y),
            radius: 4.0,
            max_radius: 28.0,
            alpha: 0.8,
        }
    }

    /// 步进动画状态。若已完全消散则返回 false。
    pub fn step(&mut self, dt_secs: f32) -> bool {
        let speed = 48.0; // 像素/秒
        self.radius += speed * dt_secs;
        self.alpha -= 1.6 * dt_secs;
        self.alpha > 0.0 && self.radius <= self.max_radius
    }
}

/// 键盘回显按键实体。
#[derive(Debug, Clone, PartialEq)]
pub struct KeystrokeDisplay {
    /// 按键组合文本（例如 "Ctrl+Shift+S"）。
    pub text: String,
    /// 剩余存活秒数。
    pub ttl_secs: f32,
    /// 显示透明度。
    pub alpha: f32,
}

impl KeystrokeDisplay {
    /// 创建新的按键回显。
    pub fn new(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            ttl_secs: 2.0,
            alpha: 1.0,
        }
    }

    /// 步进衰减按键显示。
    pub fn step(&mut self, dt_secs: f32) -> bool {
        self.ttl_secs -= dt_secs;
        if self.ttl_secs < 0.5 {
            self.alpha = (self.ttl_secs / 0.5).clamp(0.0, 1.0);
        }
        self.ttl_secs > 0.0
    }
}

/// 屏幕录制活动会话控制器。
#[derive(Debug)]
pub struct ScreenRecordingSession {
    /// 录制配置。
    config: RecordingConfig,
    /// 当前状态。
    state: RecordingState,
    /// 活跃的点击水波纹列表。
    ripples: Vec<ClickRipple>,
    /// 活跃的键盘回显列表。
    keystrokes: Vec<KeystrokeDisplay>,
    /// 开始录制的时间戳。
    started_at: Option<Instant>,
}

impl ScreenRecordingSession {
    /// 创建录制会话。
    pub fn new(config: RecordingConfig) -> Self {
        Self {
            config,
            state: RecordingState::Idle,
            ripples: Vec::new(),
            keystrokes: Vec::new(),
            started_at: None,
        }
    }

    /// 获取当前录制配置引用。
    pub const fn config(&self) -> &RecordingConfig {
        &self.config
    }

    /// 获取当前录制状态引用。
    pub const fn state(&self) -> &RecordingState {
        &self.state
    }

    /// 获取水波纹列表。
    pub fn ripples(&self) -> &[ClickRipple] {
        &self.ripples
    }

    /// 获取按键回显列表。
    pub fn keystrokes(&self) -> &[KeystrokeDisplay] {
        &self.keystrokes
    }

    /// 启动录制流程（默认从 3 秒倒计时开始）。
    pub fn start_with_countdown(&mut self, seconds: u32) {
        if seconds == 0 {
            self.start_immediately();
        } else {
            self.state = RecordingState::Countdown {
                seconds_left: seconds,
            };
        }
    }

    /// 推进倒计时一步（若倒计时结束则自动进入 Recording 录制中状态）。
    pub fn tick_countdown(&mut self) -> bool {
        if let RecordingState::Countdown { seconds_left } = &mut self.state {
            if *seconds_left <= 1 {
                self.start_immediately();
                true
            } else {
                *seconds_left -= 1;
                false
            }
        } else {
            false
        }
    }

    /// 立即开始录制。
    pub fn start_immediately(&mut self) {
        self.state = RecordingState::Recording {
            elapsed_secs: 0,
            is_paused: false,
            frames_captured: 0,
        };
        self.started_at = Some(Instant::now());
    }

    /// 暂停或恢复录制。
    pub fn toggle_pause(&mut self) {
        if let RecordingState::Recording { is_paused, .. } = &mut self.state {
            *is_paused = !*is_paused;
        }
    }

    /// 推进 1 秒录制时间。
    pub fn tick_second(&mut self) {
        if let RecordingState::Recording {
            elapsed_secs,
            is_paused: false,
            frames_captured,
        } = &mut self.state
        {
            *elapsed_secs += 1;
            *frames_captured += self.config.fps as u64;
        }
    }

    /// 推进帧采样并更新动画特效。
    pub fn step_frame(&mut self, dt_secs: f32) {
        // 更新水波纹动画
        self.ripples.retain_mut(|r| r.step(dt_secs));

        // 更新按键回显
        self.keystrokes.retain_mut(|k| k.step(dt_secs));
    }

    /// 触发鼠标点击事件特效。
    pub fn record_mouse_click(&mut self, x: i32, y: i32) {
        if self.config.show_mouse_clicks && self.state.is_active() {
            self.ripples.push(ClickRipple::new(x, y));
        }
    }

    /// 触发按键事件回显。
    pub fn record_keystroke(&mut self, text: impl Into<String>) {
        if self.config.show_keystrokes && self.state.is_active() {
            self.keystrokes.push(KeystrokeDisplay::new(text));
        }
    }

    /// 完成录制并输出产物。
    pub fn finish(&mut self) -> Result<PathBuf, String> {
        match &self.state {
            RecordingState::Recording {
                elapsed_secs,
                frames_captured,
                ..
            } => {
                let duration = *elapsed_secs;
                let frames = *frames_captured;
                let target_path = self.config.output_path.clone();

                if let Some(parent) = target_path.parent() {
                    let _ = fs::create_dir_all(parent);
                }

                // 写入元数据标头模拟视频容器文件生成
                let simulated_size = (frames * 4096).max(1024);
                let metadata = format!(
                    "SnowShot Recording Media File\nFormat: {:?}\nResolution: {}x{}\nDuration: {}s\nFrames: {}\n",
                    self.config.format,
                    self.config.region.width,
                    self.config.region.height,
                    duration,
                    frames
                );
                fs::write(&target_path, metadata.as_bytes())
                    .map_err(|e| format!("写入视频输出文件失败: {e}"))?;

                self.state = RecordingState::Finished {
                    file_path: target_path.clone(),
                    duration_secs: duration,
                    file_size_bytes: simulated_size,
                };

                Ok(target_path)
            }
            _ => Err("当前状态未处于活动录制中，无法停止".to_string()),
        }
    }

    /// 取消并放弃当前录制。
    pub fn cancel(&mut self) {
        self.state = RecordingState::Idle;
        self.ripples.clear();
        self.keystrokes.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 验证水波纹步进与衰减消失。
    #[test]
    fn test_ripple_lifecycle() {
        let mut ripple = ClickRipple::new(100, 200);
        assert_eq!(ripple.position, (100, 200));
        assert!(ripple.alpha > 0.0);

        // 步进若干次
        for _ in 0..10 {
            ripple.step(0.1);
        }
        assert!(ripple.alpha <= 0.0 || ripple.radius >= ripple.max_radius);
    }

    /// 验证按键回显生命周期。
    #[test]
    fn test_keystroke_lifecycle() {
        let mut ks = KeystrokeDisplay::new("Ctrl+C");
        assert_eq!(ks.text, "Ctrl+C");
        assert!(ks.step(1.0));
        assert!(ks.step(0.6));
        // ttl 消耗完后衰减为不可见
        assert!(!ks.step(1.0));
    }

    /// 验证录制倒计时与正常流程。
    #[test]
    fn test_recording_session_flow() {
        let mut config = RecordingConfig::default();
        let temp_dir = std::env::temp_dir();
        let out_file = temp_dir.join("snow_shot_test_rec.mp4");
        config.output_path = out_file.clone();
        config.show_mouse_clicks = true;
        config.show_keystrokes = true;

        let mut session = ScreenRecordingSession::new(config);
        assert_eq!(*session.state(), RecordingState::Idle);

        // 启动倒计时 3 秒
        session.start_with_countdown(3);
        assert_eq!(
            *session.state(),
            RecordingState::Countdown { seconds_left: 3 }
        );

        assert!(!session.tick_countdown()); // 剩余 2
        assert!(!session.tick_countdown()); // 剩余 1
        assert!(session.tick_countdown()); // 结束并自动转入 Recording

        assert!(session.state().is_active());

        // 触发按键与点击
        session.record_mouse_click(50, 50);
        session.record_keystroke("Ctrl+Alt+A");
        assert_eq!(session.ripples().len(), 1);
        assert_eq!(session.keystrokes().len(), 1);

        // 录制 2 秒
        session.tick_second();
        session.tick_second();

        // 暂停与恢复
        session.toggle_pause();
        if let RecordingState::Recording { is_paused, .. } = session.state() {
            assert!(*is_paused);
        }
        session.toggle_pause();

        // 完成录制
        let res = session.finish();
        assert!(res.is_ok());
        assert!(out_file.exists());
        let _ = fs::remove_file(out_file);
    }
}
