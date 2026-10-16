//! 媒体线程：独占 MF 引擎，负责预览播放、暂停态 seek 取帧、按剪辑列表跳段。
//!
//! 与界面线程的交接只有两样：命令通道（界面 → 媒体线程）和“最新帧槽”（媒体线程 → 界面，
//! 只留最新一帧，旧帧直接丢弃，不排队）。界面线程周期性查看 [`MediaHandle::frame_version`]
//! 决定是否 [`MediaHandle::take_frame`]。丢弃 [`MediaHandle`] 会让线程收尾并释放引擎。

use crate::video_editor::engine::Engine;
pub use crate::video_editor::engine::VideoFrame;
use crate::video_editor::playback::{PlayAction, decide};
use crate::video_editor::segments::EditList;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, Sender, TryRecvError, channel};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};
use windows::Win32::System::Com::{COINIT_MULTITHREADED, CoInitializeEx, CoUninitialize};

/// 引擎状态：加载中。
const STATE_LOADING: u8 = 0;
/// 引擎状态：就绪。
const STATE_READY: u8 = 1;
/// 引擎状态：不可用（创建失败或出错）。
const STATE_FAILED: u8 = 2;
/// 播放中的轮询间隔（frame-server 模式必须持续轮询才会前进）。
const PLAY_POLL: Duration = Duration::from_millis(1);
/// 暂停时的轮询间隔（仅用于取 seek 后的帧）。
const IDLE_POLL: Duration = Duration::from_millis(8);
/// 跳段后忽略播放决策的时间，避免异步 seek 尚未生效时重复触发。
const JUMP_SETTLE: Duration = Duration::from_millis(60);

/// 界面发给媒体线程的命令。
pub enum MediaCmd {
    /// 更新剪辑列表（决定播放时跳过哪些区间）。
    SetList(EditList),
    /// 从当前位置开始播放；`looping` 表示播完回到首段。
    Play {
        /// 是否循环。
        looping: bool,
    },
    /// 暂停。
    Pause,
    /// 跳到某毫秒（连续多次只生效最后一次）。
    Seek(u64),
}

/// 媒体线程与界面共享的状态。
struct Shared {
    /// 最新帧槽。
    frame: Mutex<Option<VideoFrame>>,
    /// 槽内帧的版本号（每放入一帧加一）。
    version: AtomicU64,
    /// 引擎当前位置（毫秒）。
    pos_ms: AtomicU64,
    /// 是否在播放。
    playing: AtomicBool,
    /// 引擎状态。
    state: AtomicU8,
    /// 退出请求。
    quit: AtomicBool,
}

/// 媒体线程句柄；丢弃时让线程收尾。
pub struct MediaHandle {
    /// 命令发送端。
    tx: Sender<MediaCmd>,
    /// 共享状态。
    shared: Arc<Shared>,
    /// 线程句柄（丢弃时移交后台线程 join）。
    join: Option<JoinHandle<()>>,
}

impl MediaHandle {
    /// 启动媒体线程并异步打开视频。
    ///
    /// # 参数
    /// - `path`：视频文件路径。
    /// - `list`：初始剪辑列表。
    pub fn spawn(path: PathBuf, list: EditList) -> Self {
        let (tx, rx) = channel();
        let shared = Arc::new(Shared {
            frame: Mutex::new(None),
            version: AtomicU64::new(0),
            pos_ms: AtomicU64::new(0),
            playing: AtomicBool::new(false),
            state: AtomicU8::new(STATE_LOADING),
            quit: AtomicBool::new(false),
        });
        let thread_shared = Arc::clone(&shared);
        let join = std::thread::Builder::new()
            .name("video-editor-media".into())
            .spawn(move || run(path, list, rx, thread_shared))
            .map_err(|e| tracing::warn!(error = %e, "无法启动媒体线程"))
            .ok();
        if join.is_none() {
            shared.state.store(STATE_FAILED, Ordering::SeqCst);
        }
        Self { tx, shared, join }
    }

    /// 发送命令（线程已退出时静默忽略）。
    pub fn send(&self, cmd: MediaCmd) {
        let _ = self.tx.send(cmd);
    }

    /// 引擎是否可用于预览（加载完成且没失败）。
    pub fn is_ready(&self) -> bool {
        self.shared.state.load(Ordering::SeqCst) == STATE_READY
    }

    /// 引擎是否失败（失败后界面回落到会话取帧，禁用播放）。
    pub fn is_failed(&self) -> bool {
        self.shared.state.load(Ordering::SeqCst) == STATE_FAILED
    }

    /// 是否在播放。
    pub fn is_playing(&self) -> bool {
        self.shared.playing.load(Ordering::SeqCst)
    }

    /// 引擎当前位置（毫秒）。
    pub fn position_ms(&self) -> u64 {
        self.shared.pos_ms.load(Ordering::SeqCst)
    }

    /// 帧槽版本号；与上次不同说明有新帧。
    pub fn frame_version(&self) -> u64 {
        self.shared.version.load(Ordering::SeqCst)
    }

    /// 取走最新一帧（槽被清空）。
    pub fn take_frame(&self) -> Option<VideoFrame> {
        self.shared.frame.lock().ok().and_then(|mut g| g.take())
    }
}

impl Drop for MediaHandle {
    /// 通知线程退出，并把 join 交给后台线程，避免阻塞界面。
    fn drop(&mut self) {
        self.shared.quit.store(true, Ordering::SeqCst);
        if let Some(join) = self.join.take() {
            std::thread::spawn(move || {
                let _ = join.join();
            });
        }
    }
}

/// 线程内部状态。
struct Loop {
    /// 剪辑列表。
    list: EditList,
    /// 是否循环。
    looping: bool,
    /// 上次跳段时间。
    last_jump: Option<Instant>,
}

/// 线程主函数：初始化 COM（MTA）→ 打开引擎 → 命令 / 取帧 / 跳段循环 → 释放。
fn run(path: PathBuf, list: EditList, rx: Receiver<MediaCmd>, shared: Arc<Shared>) {
    // SAFETY: 本线程独占的 COM 初始化，退出前配对 CoUninitialize。
    let com_ok = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED).is_ok() };
    match Engine::open(&path) {
        Ok(engine) => {
            shared.state.store(STATE_READY, Ordering::SeqCst);
            main_loop(engine, list, &rx, &shared);
        }
        Err(e) => {
            tracing::warn!(error = %e, "媒体引擎打开失败，预览回落到会话取帧");
            shared.state.store(STATE_FAILED, Ordering::SeqCst);
        }
    }
    shared.playing.store(false, Ordering::SeqCst);
    if com_ok {
        // SAFETY: 与上面的 CoInitializeEx 配对；引擎已在 main_loop 返回时释放。
        unsafe { CoUninitialize() };
    }
}

/// 把一帧放进最新帧槽（覆盖旧帧）。
fn publish(shared: &Shared, frame: VideoFrame) {
    if let Ok(mut slot) = shared.frame.lock() {
        *slot = Some(frame);
        shared.version.fetch_add(1, Ordering::SeqCst);
    }
}

/// 主循环：处理命令、取帧、播放时按剪辑列表跳段。
fn main_loop(mut engine: Engine, list: EditList, rx: &Receiver<MediaCmd>, shared: &Shared) {
    let mut st = Loop {
        list,
        looping: false,
        last_jump: None,
    };
    let mut playing = false;
    while !shared.quit.load(Ordering::SeqCst) {
        let mut pending_seek: Option<u64> = None;
        loop {
            match rx.try_recv() {
                Ok(MediaCmd::Seek(ms)) => pending_seek = Some(ms),
                Ok(MediaCmd::SetList(l)) => st.list = l,
                Ok(MediaCmd::Pause) => {
                    flush_seek(&mut engine, &mut pending_seek);
                    let _ = engine.pause();
                    playing = false;
                }
                Ok(MediaCmd::Play { looping }) => {
                    flush_seek(&mut engine, &mut pending_seek);
                    st.looping = looping;
                    // 停在末尾 / 空隙里再播：先归位
                    let now = engine.current_ms();
                    let start = match decide(now, &st.list, false) {
                        PlayAction::Continue => None,
                        PlayAction::Jump(t) => Some(t),
                        PlayAction::Finish(_) => st.list.segments().first().map(|s| s.start_ms),
                    };
                    if let Some(t) = start {
                        let _ = engine.seek_ms(t);
                    }
                    if engine.play().is_ok() {
                        playing = true;
                        st.last_jump = Some(Instant::now());
                    }
                }
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => return,
            }
        }
        flush_seek(&mut engine, &mut pending_seek);

        match engine.try_frame() {
            Ok(Some(frame)) => publish(shared, frame),
            Ok(None) => {}
            Err(e) => tracing::debug!(error = %e, "取帧失败"),
        }
        let now = engine.current_ms();
        shared.pos_ms.store(now, Ordering::SeqCst);
        if playing {
            playing = drive_playback(&mut engine, &mut st, now);
        }
        shared.playing.store(playing, Ordering::SeqCst);
        if engine.failed() {
            shared.state.store(STATE_FAILED, Ordering::SeqCst);
            return;
        }
        std::thread::sleep(if playing { PLAY_POLL } else { IDLE_POLL });
    }
}

/// 应用挂起的 seek。
fn flush_seek(engine: &mut Engine, pending: &mut Option<u64>) {
    if let Some(ms) = pending.take() {
        let _ = engine.seek_ms(ms);
    }
}

/// 播放中的一次决策：跳段不暂停；播完暂停并回到准确出点（或循环）。
///
/// # 返回
/// 之后是否仍在播放。
fn drive_playback(engine: &mut Engine, st: &mut Loop, now: u64) -> bool {
    if st.last_jump.is_some_and(|t| t.elapsed() < JUMP_SETTLE) {
        return true;
    }
    let action = if engine.ended() {
        // 播到文件末尾：交给列表决定循环或结束
        decide(u64::MAX / 2, &st.list, st.looping)
    } else {
        decide(now, &st.list, st.looping)
    };
    match action {
        PlayAction::Continue => true,
        PlayAction::Jump(t) => {
            let _ = engine.seek_ms(t);
            if engine.ended() {
                let _ = engine.play();
            }
            st.last_jump = Some(Instant::now());
            true
        }
        PlayAction::Finish(t) => {
            let _ = engine.pause();
            let _ = engine.seek_ms(t);
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 找一个本机录制的 mp4（没有则返回 `None`）。
    fn sample_video() -> Option<PathBuf> {
        let dir = std::env::var_os("USERPROFILE").map(|p| PathBuf::from(p).join("Videos"))?;
        std::fs::read_dir(dir)
            .ok()?
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.extension().is_some_and(|e| e == "mp4"))
            .max_by_key(|p| std::fs::metadata(p).map(|m| m.len()).unwrap_or(0))
    }

    /// 真引擎冒烟：打开、暂停态 seek 取帧、播放后遇到空隙跳段并在末尾停下。
    ///
    /// 依赖本机视频与硬件解码，默认忽略；手动 `cargo test -p snow-shot media -- --ignored`。
    #[test]
    #[ignore]
    fn engine_smoke() {
        let Some(path) = sample_video() else {
            return;
        };
        let mut list = EditList::new(3_000);
        list.split_at(1_000);
        list.split_at(2_000);
        list.delete(1);
        let h = MediaHandle::spawn(path, list);
        let t0 = Instant::now();
        while !h.is_ready() && !h.is_failed() && t0.elapsed() < Duration::from_secs(6) {
            std::thread::sleep(Duration::from_millis(20));
        }
        assert!(h.is_ready());
        h.send(MediaCmd::Seek(500));
        let t1 = Instant::now();
        while h.take_frame().is_none() && t1.elapsed() < Duration::from_secs(2) {
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(h.frame_version() > 0);
        h.send(MediaCmd::Seek(0));
        h.send(MediaCmd::Play { looping: false });
        let t2 = Instant::now();
        let mut seen_gap = false;
        while h.is_playing() || t2.elapsed() < Duration::from_millis(300) {
            let p = h.position_ms();
            if (1_050..1_950).contains(&p) {
                seen_gap = true;
            }
            if t2.elapsed() > Duration::from_secs(8) {
                break;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(!h.is_playing(), "应在末尾停下");
        assert!(!seen_gap, "不应播放已删区间");
    }
}
