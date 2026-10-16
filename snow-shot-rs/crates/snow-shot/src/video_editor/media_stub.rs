//! 非 Windows 平台的媒体线程占位：没有系统媒体引擎，始终报告“不可用”，界面回落到会话取帧。
//!
//! 接口与 `media.rs` 保持一致，保证全平台可编译（目前只有 Windows 验证过）。

use crate::video_editor::segments::EditList;
use std::path::PathBuf;

/// 一帧 BGRA 画面。
pub struct VideoFrame {
    /// 宽（像素）。
    pub width: u32,
    /// 高（像素）。
    pub height: u32,
    /// BGRA 字节。
    pub data: Vec<u8>,
}

/// 界面发给媒体线程的命令（占位实现里全部忽略）。
pub enum MediaCmd {
    /// 更新剪辑列表。
    SetList(EditList),
    /// 开始播放。
    Play {
        /// 是否循环。
        looping: bool,
    },
    /// 暂停。
    Pause,
    /// 跳到某毫秒。
    Seek(u64),
}

/// 媒体线程句柄占位。
pub struct MediaHandle;

impl MediaHandle {
    /// 占位：不启动任何线程。
    pub fn spawn(_path: PathBuf, _list: EditList) -> Self {
        Self
    }

    /// 占位：忽略命令。
    pub fn send(&self, _cmd: MediaCmd) {}

    /// 占位：永远未就绪。
    pub fn is_ready(&self) -> bool {
        false
    }

    /// 占位：永远失败。
    pub fn is_failed(&self) -> bool {
        true
    }

    /// 占位：从不播放。
    pub fn is_playing(&self) -> bool {
        false
    }

    /// 占位：位置恒为 0。
    pub fn position_ms(&self) -> u64 {
        0
    }

    /// 占位：版本恒为 0。
    pub fn frame_version(&self) -> u64 {
        0
    }

    /// 占位：没有帧。
    pub fn take_frame(&self) -> Option<VideoFrame> {
        None
    }
}
