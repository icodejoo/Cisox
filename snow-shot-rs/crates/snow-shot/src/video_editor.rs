//! 基础视频剪辑窗：带缩略图条的时间轴、分割 / 删除 / 恢复、静音预览播放、导出。
//!
//! 入口是 [`open_video_editor`]，录制完成后的“编辑”按钮把刚录好的视频路径经它传入。
//! 缩略图 / 关键帧来自常驻的 `cisox-recorder` 编辑会话（见 [`session`]），预览播放用系统
//! Media Foundation 媒体引擎（见 [`media`]，独立媒体线程），导出另起一次性进程（见 [`worker`]）。
//! 不做多轨、转场、文字、波形、吸附、完整撤销栈。
//!
//! 操作：滚轮以鼠标为锚点缩放时间轴，Shift+滚轮平移；空格播放 / 暂停，← → 逐帧，
//! S 分割，Delete 删除选中片段，I / O 设入点 / 出点；点灰色（已删）区间恢复；
//! Ctrl+S 把当前预览帧另存为 PNG（弹另存为对话框，后台编码写盘）。

#[cfg(windows)]
mod engine;
mod logic;
#[cfg(windows)]
mod media;
#[cfg(not(windows))]
#[path = "video_editor/media_stub.rs"]
mod media;
mod playback;
mod segments;
mod session;
mod thumb_cache;
mod timeline;
mod viewport;
mod worker;

use crate::video_editor::logic::{
    FRAME_EXT, encode_frame_png, format_time, frame_file_name, frame_step_ms, trimmed_output_path,
};
use crate::video_editor::logic::{fit_contain, initial_window_size, unique_frame_path};
use crate::video_editor::media::{MediaCmd, MediaHandle};
use crate::video_editor::segments::{Edge, EditList};
use crate::video_editor::session::{Session, SessionEvent};
use crate::video_editor::thumb_cache::{ThumbCache, TileKey};
use crate::video_editor::viewport::{Viewport, any_cut_off_keyframe};
use crate::video_editor::worker::{ExportEvent, ExportWorker};
use image::{Frame, RgbaImage};
use snow_i18n::Args;
use snow_recorder_protocol::ProbeInfo;
use snow_ui::shell::geometry::LogicalSize;
use snow_ui::shell::window::WindowSpec;
use snow_ui::ui::component::button::{Button, ButtonVariants};
use snow_ui::ui::component::{Disableable, Sizable, Size as ComponentSize};
use snow_ui::ui::*;
use std::cell::Cell;
use std::collections::{HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;
use std::sync::mpsc::{Receiver, TryRecvError};
use std::time::{Duration, Instant};

/// 窗口逻辑宽度。
const WINDOW_WIDTH: f32 = 920.0;
/// 窗口逻辑高度。
const WINDOW_HEIGHT: f32 = 680.0;
/// 窗口内边距，同时是时间轴左右留白（拖动坐标换算依赖它）。
pub(crate) const PAD: f32 = 20.0;
/// 背景色。
const COLOR_BG: u32 = 0x1F1F1FFF;
/// 主文字色。
const COLOR_TEXT: u32 = 0xFFFFFFFF;
/// 次要文字色。
const COLOR_DIM: u32 = 0x8C8C8CFF;
/// 错误文字色。
const COLOR_ERROR: u32 = 0xFF6B6BFF;
/// 提示文字色。
const COLOR_NOTE: u32 = 0xE6B450FF;
/// 预览区底色。
const COLOR_PREVIEW_BG: u32 = 0x111111FF;
/// 快速轮询间隔（播放、拖动、等待 seek 帧时）。
const TICK_FAST: Duration = Duration::from_millis(16);
/// 空闲轮询间隔（只等进程事件）。
const TICK_IDLE: Duration = Duration::from_millis(80);
/// seek 之后保持快速轮询的时长（等引擎出帧）。
const FAST_AFTER_SEEK: Duration = Duration::from_millis(500);
/// 播放请求后，引擎状态尚未反映前的宽限（防止界面误判已停）。
const PLAY_GRACE: Duration = Duration::from_millis(300);
/// 缩略图条高度（逻辑像素）。
pub(crate) const THUMB_H: f32 = 48.0;
/// 缩略图按几倍分辨率取（高分屏清晰度）。
const THUMB_SCALE: u32 = 2;
/// 缩略图瓦片最小宽度。
const THUMB_MIN_W: f32 = 32.0;
/// 缩略图瓦片最大宽度。
const THUMB_MAX_W: f32 = 160.0;
/// 缩略图缓存总字节上限。
const THUMB_CACHE_BYTES: usize = 24 * 1024 * 1024;
/// 缩放 / 平移后等多久再请求缩略图（防抖）。
const THUMB_DEBOUNCE: Duration = Duration::from_millis(100);
/// 同时记住的缩略图批次数（旧批次的迟到事件仍能对上号）。
const MAX_BATCHES: usize = 4;
/// 播放完后是否回到首段循环（决定：播完停住）。
const PLAY_LOOP: bool = false;
/// 预览回落取帧的最大宽度。
const FALLBACK_FRAME_W: u32 = 1280;
/// 回落取帧的最小间隔。
const FALLBACK_FRAME_GAP: Duration = Duration::from_millis(40);
/// 另存帧对话框的文件过滤模式。
const FRAME_FILTER_PATTERN: &str = "*.png";
/// 百分比满值。
const PERCENT: u64 = 100;

/// 取路径里用于显示的文件名；没有文件名时回落为完整路径文本。
///
/// # 参数
/// - `path`：视频文件路径。
///
/// # 示例
/// ```ignore
/// assert_eq!(editor_file_label(Path::new("C:/a/b.mp4")), "b.mp4");
/// ```
pub fn editor_file_label(path: &Path) -> String {
    path.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.to_string_lossy().into_owned())
}

/// 打开视频剪辑窗。
///
/// # 参数
/// - `cx`：外壳上下文。
/// - `path`：要编辑的视频文件路径（必须仍然存在）。
/// - `locale`：界面语言代码，如 `zh-CN`。
///
/// # 示例
/// ```ignore
/// open_video_editor(cx, Path::new("C:/Videos/rec.mp4"), "zh-CN");
/// ```
pub fn open_video_editor(cx: &mut ShellContext, path: &Path, locale: &'static str) {
    let i18n = crate::ocr_backend::i18n_for(locale);
    let spec = WindowSpec::normal(
        i18n.tr("recording-video-editor-title"),
        LogicalSize::new(WINDOW_WIDTH, WINDOW_HEIGHT),
    );
    let path: PathBuf = path.to_path_buf();
    if let Err(e) = cx.open_window(&spec, move |_window, app| {
        app.new(|cx| {
            let mut view = VideoEditorView::new(path, locale, cx);
            view.start(cx);
            view
        })
    }) {
        tracing::error!(error = %e, "打开视频编辑窗失败");
    }
}

/// 窗口所在显示器的工作区逻辑尺寸；取不到显示器信息时返回 `None`。
fn current_work_area_logical(window: &Window) -> Option<(f32, f32)> {
    use snow_ui::shell::geometry::PhysicalPoint;
    use snow_ui::shell::monitor::Monitors;
    let monitors = Monitors::enumerate().ok()?;
    let scale = window.scale_factor();
    let b = window.bounds();
    let center = PhysicalPoint::new(
        ((f32::from(b.origin.x) + f32::from(b.size.width) / 2.0) * scale) as i32,
        ((f32::from(b.origin.y) + f32::from(b.size.height) / 2.0) * scale) as i32,
    );
    let mon = monitors.at_point(center).or_else(|| monitors.primary())?;
    let s = mon.scale.value();
    Some((
        mon.work_area.width as f32 / s,
        mon.work_area.height as f32 / s,
    ))
}

/// 正在拖动的对象。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Drag {
    /// 播放头。
    Playhead,
    /// 某个片段边。
    Edge(Edge),
}

/// 进行中的导出任务。
struct ExportJob {
    /// 导出进程。
    worker: ExportWorker,
    /// 最近一次进度 `(已完成, 总数)`。
    progress: Option<(u64, u64)>,
}

/// 导出结果。
enum Outcome {
    /// 成功，附输出文件。
    Done(PathBuf),
    /// 失败，附原因。
    Failed(String),
    /// 保存当前帧失败，附原因。
    FrameFailed(String),
}

/// 视频剪辑窗视图。
pub struct VideoEditorView {
    /// 要编辑的视频路径。
    path: PathBuf,
    /// 界面语言代码。
    locale: &'static str,
    /// 视频信息（探测完成后才有）。
    info: Option<ProbeInfo>,
    /// 探测 / 会话失败原因。
    probe_error: Option<String>,
    /// 剪辑列表（探测完成后才有）。
    pub(crate) list: Option<EditList>,
    /// 时间轴视口（探测完成后才有）。
    pub(crate) viewport: Option<Viewport>,
    /// 关键帧时间表。
    pub(crate) keyframes: Vec<u64>,
    /// 常驻编辑会话。
    session: Option<Session>,
    /// 媒体线程（探测完成后才有）。
    media: Option<MediaHandle>,
    /// 播放头位置（原视频毫秒）。
    pub(crate) playhead_ms: u64,
    /// 选中的片段下标。
    pub(crate) selected: Option<usize>,
    /// 正在拖动的对象。
    pub(crate) drag: Option<Drag>,
    /// 预览区实际尺寸（由绘制回调写入，逻辑像素）。
    preview_box: Rc<Cell<(f32, f32)>>,
    /// 首次渲染时的窗口内容尺寸，用来判断用户是否已手动调整。
    initial_size: Option<(f32, f32)>,
    /// 探测后待办的一次性窗口自适应（视频宽高）。
    pending_fit: Option<(u32, u32)>,
    /// 当前预览帧。
    preview: Option<Arc<RenderImage>>,
    /// 待释放 GPU 资源的旧图（下次渲染时释放）。
    retired: Vec<Arc<RenderImage>>,
    /// 已取走的媒体帧版本。
    frame_ver: u64,
    /// 界面认为是否在播放。
    playing: bool,
    /// 最近一次请求播放的时刻。
    play_requested: Option<Instant>,
    /// 快速轮询截止时刻。
    fast_until: Instant,
    /// 回落取帧：想看的时间。
    fallback_wanted: Option<u64>,
    /// 回落取帧：序号。
    fallback_seq: u64,
    /// 回落取帧：上次发出时刻。
    fallback_sent: Option<Instant>,
    /// 缩略图缓存。
    pub(crate) thumbs: ThumbCache<Arc<RenderImage>>,
    /// 最近的缩略图批次：`(批次号, 各序号对应的瓦片)`。
    batches: VecDeque<(u64, Vec<TileKey>)>,
    /// 已发出请求、尚未到达的瓦片。
    inflight: HashSet<TileKey>,
    /// 下一个批次号。
    next_batch: u64,
    /// 缩略图瓦片宽度（逻辑像素）。
    pub(crate) tile_w: f32,
    /// 到点后请求缩略图（防抖）。
    thumb_due: Option<Instant>,
    /// 轨道可见宽度（渲染时更新）。
    track_px: f32,
    /// 是否已把焦点交给根元素。
    focused: bool,
    /// 键盘焦点。
    focus: FocusHandle,
    /// 进行中的导出。
    export: Option<ExportJob>,
    /// 导出结果。
    outcome: Option<Outcome>,
    /// 进行中的“保存当前帧”后台任务（收到结果或断开即结束）。
    frame_save: Option<Receiver<Outcome>>,
    /// 轮询任务是否在跑。
    ticking: bool,
}

impl VideoEditorView {
    /// 创建视图（尚未拉起会话）。
    ///
    /// # 参数
    /// - `path`：视频路径。
    /// - `locale`：界面语言代码。
    /// - `cx`：视图上下文（用于创建焦点句柄）。
    fn new(path: PathBuf, locale: &'static str, cx: &mut Context<Self>) -> Self {
        Self {
            path,
            locale,
            info: None,
            probe_error: None,
            list: None,
            viewport: None,
            keyframes: Vec::new(),
            session: None,
            media: None,
            playhead_ms: 0,
            selected: None,
            drag: None,
            preview: None,
            preview_box: Rc::new(Cell::new((0.0, 0.0))),
            initial_size: None,
            pending_fit: None,
            retired: Vec::new(),
            frame_ver: 0,
            playing: false,
            play_requested: None,
            fast_until: Instant::now(),
            fallback_wanted: None,
            fallback_seq: 0,
            fallback_sent: None,
            thumbs: ThumbCache::new(THUMB_CACHE_BYTES),
            batches: VecDeque::new(),
            inflight: HashSet::new(),
            next_batch: 1,
            tile_w: THUMB_MIN_W * 2.0,
            thumb_due: None,
            track_px: WINDOW_WIDTH - PAD * 2.0,
            focused: false,
            focus: cx.focus_handle(),
            export: None,
            outcome: None,
            frame_save: None,
            ticking: false,
        }
    }

    /// 拉起编辑会话并开始轮询。
    fn start(&mut self, cx: &mut Context<Self>) {
        match Session::open(&self.path) {
            Ok(session) => {
                self.session = Some(session);
                self.ensure_ticking(cx);
            }
            Err(e) => self.probe_error = Some(e),
        }
    }

    /// 保证轮询任务在跑。
    pub(crate) fn ensure_ticking(&mut self, cx: &mut Context<Self>) {
        if self.ticking {
            return;
        }
        self.ticking = true;
        cx.spawn(async move |this, cx| {
            loop {
                let step = this.update(cx, |v, cx| v.tick(cx));
                match step {
                    Ok((true, fast)) => {
                        let wait = if fast { TICK_FAST } else { TICK_IDLE };
                        cx.background_executor().timer(wait).await;
                    }
                    _ => break,
                }
            }
        })
        .detach();
    }

    /// 一次轮询：返回 `(是否继续轮询, 是否用快速间隔)`。
    fn tick(&mut self, cx: &mut Context<Self>) -> (bool, bool) {
        self.poll_session();
        self.poll_media();
        self.poll_export();
        self.poll_frame_save();
        self.flush_thumbs();
        self.flush_fallback();
        cx.notify();
        let fast = self.playing || self.drag.is_some() || Instant::now() < self.fast_until;
        let busy = fast
            || self.thumb_due.is_some()
            || !self.inflight.is_empty()
            || self.export.is_some()
            || self.frame_save.is_some()
            || (self.info.is_none() && self.probe_error.is_none())
            || self.fallback_wanted.is_some();
        if !busy {
            self.ticking = false;
        }
        (busy, fast)
    }

    /// 处理会话事件：探测 / 关键帧 / 缩略图 / 回落预览帧。
    fn poll_session(&mut self) {
        let Some(session) = self.session.as_mut() else {
            return;
        };
        for event in session.poll() {
            match event {
                SessionEvent::Probed(info) => self.on_probed(info),
                SessionEvent::Keyframes(ms) => self.keyframes = ms,
                SessionEvent::Thumb {
                    id,
                    index,
                    at_ms,
                    w,
                    h,
                    bgra,
                } => self.on_thumb(id, index, at_ms, w, h, bgra),
                SessionEvent::Frame {
                    seq, w, h, bgra, ..
                } => {
                    // 只采用最新请求的回落帧
                    if seq == self.fallback_seq {
                        self.set_preview(w, h, bgra);
                    }
                }
                SessionEvent::Failed(reason) => {
                    if self.info.is_none() {
                        self.probe_error = Some(reason);
                    }
                    self.inflight.clear();
                }
            }
        }
    }

    /// 探测完成：建立剪辑列表、视口，拉起媒体线程并请求首批缩略图。
    fn on_probed(&mut self, info: ProbeInfo) {
        let list = EditList::new(info.duration_ms);
        self.viewport = Some(Viewport::new(info.duration_ms, self.track_px));
        self.media = Some(MediaHandle::spawn(self.path.clone(), list.clone()));
        self.list = Some(list);
        let aspect = info.width.max(1) as f32 / info.height.max(1) as f32;
        self.tile_w = (THUMB_H * aspect).round().clamp(THUMB_MIN_W, THUMB_MAX_W);
        self.pending_fit = Some((info.width, info.height));
        self.info = Some(info);
        self.thumb_due = Some(Instant::now());
        self.seek_to(0);
    }

    /// 收到一张缩略图：转成图像放进缓存（不属于记录批次的丢弃）。
    fn on_thumb(&mut self, id: u64, index: u64, at_ms: u64, w: u32, h: u32, bgra: Vec<u8>) {
        let Some(key) = self
            .batches
            .iter()
            .find(|(b, _)| *b == id)
            .and_then(|(_, keys)| keys.get(index as usize).copied())
        else {
            return;
        };
        self.inflight.remove(&key);
        let bytes = bgra.len();
        let Some(image) = bgra_image(w, h, bgra) else {
            return;
        };
        let evicted = self.thumbs.insert(key, image, bytes, at_ms);
        self.retired.extend(evicted);
    }

    /// 处理媒体线程：取最新帧、同步播放位置与状态。
    fn poll_media(&mut self) {
        let Some((version, playing, pos)) = self
            .media
            .as_ref()
            .map(|m| (m.frame_version(), m.is_playing(), m.position_ms()))
        else {
            return;
        };
        if version != self.frame_ver {
            self.frame_ver = version;
            if let Some(frame) = self.media.as_ref().and_then(|m| m.take_frame()) {
                self.set_preview(frame.width, frame.height, frame.data);
            }
        }
        if playing {
            let duration = self.info.map_or(0, |i| i.duration_ms);
            self.playhead_ms = pos.min(duration);
            if let Some(view) = self.viewport.as_mut() {
                view.reveal(self.playhead_ms);
            }
            self.thumb_due
                .get_or_insert(Instant::now() + THUMB_DEBOUNCE);
        } else if self.playing && self.play_requested.is_none_or(|t| t.elapsed() > PLAY_GRACE) {
            // 播完（或引擎停止）：停在引擎给出的最终位置
            self.playing = false;
            self.playhead_ms = pos;
            self.fast_until = Instant::now() + FAST_AFTER_SEEK;
        }
    }

    /// 把 BGRA 像素设为当前预览帧，旧图排队释放。
    fn set_preview(&mut self, w: u32, h: u32, bgra: Vec<u8>) {
        if let Some(old) = bgra_image(w, h, bgra).and_then(|image| self.preview.replace(image)) {
            self.retired.push(old);
        }
    }

    /// 处理导出进程事件。
    fn poll_export(&mut self) {
        let Some(job) = self.export.as_mut() else {
            return;
        };
        let mut outcome = None;
        for event in job.worker.poll() {
            match event {
                ExportEvent::Progress { done, total } => job.progress = Some((done, total)),
                ExportEvent::Finished { path } => outcome = Some(Outcome::Done(path)),
                ExportEvent::Failed(reason) => outcome = Some(Outcome::Failed(reason)),
            }
        }
        if outcome.is_some() {
            self.outcome = outcome;
            self.export = None;
        }
    }

    /// 处理“保存当前帧”后台任务：收到结果就展示；通道断开表示用户取消了对话框。
    fn poll_frame_save(&mut self) {
        let Some(rx) = self.frame_save.as_ref() else {
            return;
        };
        match rx.try_recv() {
            Ok(outcome) => {
                self.outcome = Some(outcome);
                self.frame_save = None;
            }
            Err(TryRecvError::Disconnected) => self.frame_save = None,
            Err(TryRecvError::Empty) => {}
        }
    }

    /// 防抖到点后，为当前视口请求缺失的缩略图（新批次会打断旧批次）。
    fn flush_thumbs(&mut self) {
        if self.thumb_due.is_none_or(|t| Instant::now() < t) {
            return;
        }
        self.thumb_due = None;
        let Some(view) = self.viewport else {
            return;
        };
        let level = view.level();
        let tile_w = self.tile_w;
        let reqs = view.thumb_requests(level, tile_w, |l, i| {
            !self.thumbs.contains((l, i)) && !self.inflight.contains(&(l, i))
        });
        if reqs.is_empty() {
            return;
        }
        let Some(session) = self.session.as_mut() else {
            return;
        };
        let id = self.next_batch;
        self.next_batch += 1;
        let keys: Vec<TileKey> = reqs.iter().map(|&(i, _)| (level, i)).collect();
        let times: Vec<u64> = reqs.iter().map(|&(_, t)| t).collect();
        // 新批次打断旧批次：旧批次没取完的不再算在途
        self.inflight = keys.iter().copied().collect();
        self.batches.push_back((id, keys));
        while self.batches.len() > MAX_BATCHES {
            self.batches.pop_front();
        }
        let scale = THUMB_SCALE as f32;
        session.thumbs(id, (tile_w * scale) as u32, (THUMB_H * scale) as u32, times);
    }

    /// 引擎不可用时的预览回落：节流地向会话要某时刻的帧。
    fn flush_fallback(&mut self) {
        let Some(at_ms) = self.fallback_wanted else {
            return;
        };
        if self.media.as_ref().is_some_and(|m| !m.is_failed()) {
            // 引擎可用就不需要回落
            self.fallback_wanted = None;
            return;
        }
        if self
            .fallback_sent
            .is_some_and(|t| t.elapsed() < FALLBACK_FRAME_GAP)
        {
            return;
        }
        let Some(session) = self.session.as_mut() else {
            return;
        };
        self.fallback_seq += 1;
        self.fallback_sent = Some(Instant::now());
        self.fallback_wanted = None;
        // 取帧会打断缩略图批次，之后重新请求
        self.inflight.clear();
        self.thumb_due = Some(Instant::now() + THUMB_DEBOUNCE);
        session.frame(self.fallback_seq, at_ms, FALLBACK_FRAME_W);
    }

    /// 缩放 / 平移后登记缩略图请求（防抖）。
    pub(crate) fn schedule_thumbs(&mut self, cx: &mut Context<Self>) {
        self.thumb_due = Some(Instant::now() + THUMB_DEBOUNCE);
        self.ensure_ticking(cx);
    }

    /// 把播放头移到 `ms` 并刷新预览（引擎 seek；引擎不可用时走会话取帧）。
    pub(crate) fn seek_to(&mut self, ms: u64) {
        let duration = self.info.map_or(0, |i| i.duration_ms);
        self.playhead_ms = ms.min(duration);
        self.fast_until = Instant::now() + FAST_AFTER_SEEK;
        match self.media.as_ref() {
            Some(m) if !m.is_failed() => m.send(MediaCmd::Seek(self.playhead_ms)),
            _ => self.fallback_wanted = Some(self.playhead_ms),
        }
    }

    /// 同步剪辑列表到媒体线程（改了片段后调用）。
    pub(crate) fn push_list(&self) {
        if let (Some(media), Some(list)) = (self.media.as_ref(), self.list.as_ref()) {
            media.send(MediaCmd::SetList(list.clone()));
        }
    }

    /// 播放 / 暂停切换。
    fn toggle_play(&mut self, cx: &mut Context<Self>) {
        let Some(media) = self.media.as_ref().filter(|m| m.is_ready()) else {
            return;
        };
        if self.playing {
            media.send(MediaCmd::Pause);
            self.playing = false;
        } else {
            media.send(MediaCmd::Play { looping: PLAY_LOOP });
            self.playing = true;
            self.play_requested = Some(Instant::now());
        }
        self.ensure_ticking(cx);
        cx.notify();
    }

    /// 暂停（若在播放）。
    fn pause(&mut self) {
        if self.playing {
            if let Some(media) = self.media.as_ref() {
                media.send(MediaCmd::Pause);
            }
            self.playing = false;
        }
    }

    /// 逐帧步进：`dir` 为 -1 或 1。
    fn step_frame(&mut self, dir: i64, cx: &mut Context<Self>) {
        let Some(info) = self.info else {
            return;
        };
        self.pause();
        let step = frame_step_ms(info.fps_milli) as i64;
        let target = (self.playhead_ms as i64 + dir * step).clamp(0, info.duration_ms as i64);
        self.seek_to(target as u64);
        self.ensure_ticking(cx);
        cx.notify();
    }

    /// 在播放头处分割，并选中右半片段。
    fn split(&mut self, cx: &mut Context<Self>) {
        let at = self.playhead_ms;
        if let Some(idx) = self.list.as_mut().and_then(|l| l.split_at(at)) {
            self.selected = Some(idx + 1);
            self.push_list();
            cx.notify();
        }
    }

    /// 删除选中片段（没有选中时删播放头所在片段）。
    fn delete_selected(&mut self, cx: &mut Context<Self>) {
        let at = self.playhead_ms;
        let Some(list) = self.list.as_mut() else {
            return;
        };
        let Some(idx) = self.selected.or_else(|| list.segment_at(at)) else {
            return;
        };
        if list.delete(idx) {
            self.selected = None;
            self.push_list();
            cx.notify();
        }
    }

    /// 恢复播放头所在的已删空隙。
    fn restore_at_playhead(&mut self, cx: &mut Context<Self>) {
        let at = self.playhead_ms;
        self.restore_at(at, cx);
    }

    /// 恢复 `ms` 所在的已删空隙，并选中恢复出的片段。
    pub(crate) fn restore_at(&mut self, ms: u64, cx: &mut Context<Self>) {
        if let Some(idx) = self.list.as_mut().and_then(|l| l.restore_at(ms)) {
            self.selected = Some(idx);
            self.push_list();
            cx.notify();
        }
    }

    /// 把播放头位置设为入点（删掉之前的部分）。
    fn set_in(&mut self, cx: &mut Context<Self>) {
        let at = self.playhead_ms;
        if let Some(list) = self.list.as_mut() {
            let ms = list.set_in(at);
            self.selected = None;
            self.push_list();
            self.seek_to(ms);
            cx.notify();
        }
    }

    /// 把播放头位置设为出点（删掉之后的部分）。
    fn set_out(&mut self, cx: &mut Context<Self>) {
        let at = self.playhead_ms;
        if let Some(list) = self.list.as_mut() {
            list.set_out(at);
            self.selected = None;
            self.push_list();
            cx.notify();
        }
    }

    /// 快捷键分发。
    ///
    /// # 返回
    /// 是否已处理。
    fn handle_key(&mut self, key: &str, cx: &mut Context<Self>) -> bool {
        match key {
            "space" => self.toggle_play(cx),
            "left" => self.step_frame(-1, cx),
            "right" => self.step_frame(1, cx),
            "s" => self.split(cx),
            "delete" | "backspace" => self.delete_selected(cx),
            "i" => self.set_in(cx),
            "o" => self.set_out(cx),
            _ => return false,
        }
        true
    }

    /// 当前是否有可保存的预览帧且没有保存任务在跑。
    fn can_save_frame(&self) -> bool {
        self.preview.is_some() && self.frame_save.is_none()
    }

    /// 把当前预览帧另存为 PNG：拷出 BGRA 缓冲后，在后台线程弹另存为对话框、编码并写盘，不阻塞界面。
    ///
    /// 默认目录是视频所在目录，默认文件名 `<视频名>_<mm-ss-mmm>.png`；取消对话框不留提示。
    fn save_frame(&mut self, cx: &mut Context<Self>) {
        if !self.can_save_frame() {
            return;
        }
        let Some(image) = self.preview.as_ref() else {
            return;
        };
        let size = image.size(0);
        let (w, h) = (size.width.0 as u32, size.height.0 as u32);
        let Some(bgra) = image.as_bytes(0).map(<[u8]>::to_vec) else {
            return;
        };
        let i18n = crate::ocr_backend::i18n_for(self.locale);
        let dir = self
            .path
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_default();
        // 默认文件名避开已有文件，用户仍可在对话框里改
        let default_name = unique_frame_path(
            &dir,
            &frame_file_name(&self.path, self.playhead_ms),
            Path::exists,
        )
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
        let request = snow_platform::file_dialog::SaveDialogRequest {
            title: i18n.tr("video-editor-save-frame-title"),
            initial_dir: Some(dir),
            file_name: default_name,
            filters: vec![snow_platform::file_dialog::FileFilter {
                label: i18n.tr("video-editor-save-frame-filter"),
                pattern: FRAME_FILTER_PATTERN.to_string(),
            }],
            ..Default::default()
        };
        let (tx, rx) = std::sync::mpsc::channel();
        self.frame_save = Some(rx);
        self.outcome = None;
        std::thread::spawn(move || {
            if let Some(outcome) = write_frame(&request, w, h, &bgra) {
                let _ = tx.send(outcome);
            }
        });
        self.ensure_ticking(cx);
        cx.notify();
    }

    /// 发起导出：输出到原文件同目录，不覆盖任何已有文件。
    fn start_export(&mut self, cx: &mut Context<Self>) {
        let Some(ranges) = self
            .list
            .as_ref()
            .filter(|l| l.can_export())
            .map(EditList::export_ranges)
        else {
            return;
        };
        if self.export.is_some() {
            return;
        }
        self.pause();
        let output = trimmed_output_path(&self.path, Path::exists);
        self.outcome = None;
        match ExportWorker::start(&self.path, &ranges, &output) {
            Ok(worker) => {
                self.export = Some(ExportJob {
                    worker,
                    progress: None,
                });
                self.ensure_ticking(cx);
            }
            Err(e) => self.outcome = Some(Outcome::Failed(e)),
        }
        cx.notify();
    }
}

/// 后台线程里的保存流程：弹另存为对话框，确认后编码 PNG 并写盘。
///
/// # 参数
/// - `request`：另存为对话框请求。
/// - `w`、`h`、`bgra`：预览帧尺寸与紧密排列的 BGRA 像素。
///
/// # 返回
/// 保存结果；用户取消对话框返回 `None`。
fn write_frame(
    request: &snow_platform::file_dialog::SaveDialogRequest,
    w: u32,
    h: u32,
    bgra: &[u8],
) -> Option<Outcome> {
    let choice = match snow_platform::file_dialog::show_save_dialog(request) {
        Ok(Some(choice)) => choice,
        Ok(None) => return None,
        Err(e) => return Some(Outcome::FrameFailed(e)),
    };
    let mut path = choice.path;
    if path.extension().is_none() {
        path.set_extension(FRAME_EXT);
    }
    let saved = encode_frame_png(w, h, bgra)
        .and_then(|png| std::fs::write(&path, png).map_err(|e| e.to_string()));
    Some(match saved {
        Ok(()) => Outcome::Done(path),
        Err(e) => {
            tracing::warn!(error = %e, "保存当前帧失败");
            Outcome::FrameFailed(e)
        }
    })
}

/// BGRA 像素转 GPUI 图像资源（GPUI 要求 BGRA，字节原样使用）。
///
/// # 参数
/// - `w`、`h`：宽高。
/// - `bgra`：紧密排列的 BGRA 字节。
///
/// # 返回
/// 图像；长度与宽高对不上时返回 `None`。
fn bgra_image(w: u32, h: u32, bgra: Vec<u8>) -> Option<Arc<RenderImage>> {
    let buffer = RgbaImage::from_raw(w, h, bgra)?;
    Some(Arc::new(RenderImage::new(vec![Frame::new(buffer)])))
}

impl Render for VideoEditorView {
    /// 渲染：标题信息、预览区、工具栏、时间轴、提示行、底部按钮。
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        for image in self.retired.drain(..) {
            let _ = window.drop_image(image);
        }
        if !self.focused {
            self.focused = true;
            window.focus(&self.focus, cx);
        }
        let vp = window.viewport_size();
        let vp_now = (f32::from(vp.width), f32::from(vp.height));
        if self.initial_size.is_none() {
            self.initial_size = Some(vp_now);
        }
        if let Some((vw, vh)) = self.pending_fit.take() {
            self.fit_window_once(window, vw, vh, vp_now);
        }
        let track = (f32::from(window.viewport_size().width) - PAD * 2.0).max(1.0);
        if (track - self.track_px).abs() > 0.5 {
            self.track_px = track;
            if let Some(view) = self.viewport.as_mut() {
                view.resize(track);
            }
            self.thumb_due = Some(Instant::now() + THUMB_DEBOUNCE);
            self.ensure_ticking(cx);
        }
        let i18n = crate::ocr_backend::i18n_for(self.locale);
        let dim = rgba(COLOR_DIM);
        let file_line = i18n.tr_with(
            "recording-video-editor-file",
            &Args::new().named("name", editor_file_label(&self.path)),
        );
        let info_line = match (&self.info, &self.probe_error) {
            (Some(info), _) => i18n.tr_with(
                "video-editor-info",
                &Args::new()
                    .named("duration", format_time(info.duration_ms))
                    .named("width", info.width.to_string())
                    .named("height", info.height.to_string())
                    .named("keyframes", info.keyframes.to_string()),
            ),
            (None, Some(reason)) => i18n.tr_with(
                "video-editor-probe-failed",
                &Args::new().named("reason", reason.as_str()),
            ),
            (None, None) => i18n.tr("video-editor-probing"),
        };
        let preview_body = match &self.preview {
            Some(image) => {
                let size = image.size(0);
                let (cw, ch) = self.preview_box.get();
                let fit = fit_contain(cw, ch, size.width.0 as f32, size.height.0 as f32);
                let mut frame = div().size_full().relative();
                if let Some(r) = fit {
                    frame = frame.child(
                        img(ImageSource::Render(Arc::clone(image)))
                            .absolute()
                            .left(px(r.x))
                            .top(px(r.y))
                            .w(px(r.w))
                            .h(px(r.h))
                            .object_fit(ObjectFit::Fill),
                    );
                }
                frame
            }
            None => div()
                .size_full()
                .flex()
                .items_center()
                .justify_center()
                .text_color(dim)
                .child(i18n.tr("video-editor-preview-loading")),
        };
        let mut root = div()
            .size_full()
            .bg(rgba(COLOR_BG))
            .text_color(rgba(COLOR_TEXT))
            .p(px(PAD))
            .flex()
            .flex_col()
            .gap_3()
            .track_focus(&self.focus)
            .on_key_down(cx.listener(|this, ev: &KeyDownEvent, _window, cx| {
                let m = ev.keystroke.modifiers;
                let ctrl_only = m.control && !(m.alt || m.platform || m.shift);
                if ctrl_only && ev.keystroke.key == "s" {
                    this.save_frame(cx);
                    cx.stop_propagation();
                } else if !(m.control || m.alt || m.platform)
                    && this.handle_key(&ev.keystroke.key, cx)
                {
                    cx.stop_propagation();
                }
            }))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .child(div().text_size(px(14.0)).child(file_line))
                    .child(div().text_size(px(12.0)).text_color(dim).child(info_line)),
            )
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .w_full()
                    .overflow_hidden()
                    .bg(rgba(COLOR_PREVIEW_BG))
                    .relative()
                    .child(self.measure_preview(cx))
                    .child(preview_body),
            );
        if self.list.is_some() {
            root = root
                .child(self.render_toolbar(cx))
                .child(self.render_timeline(cx))
                .child(self.render_note());
        }
        root.child(self.render_footer(cx))
            .child(self.drag_listeners(cx))
    }
}

impl VideoEditorView {
    /// 窗口级拖动监听层：移动 / 松开不依赖悬停命中，鼠标移出时间轴乃至窗口也不丢事件。
    fn drag_listeners(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let entity = cx.entity();
        canvas(
            |_, _, _| (),
            move |_, _, window, _| {
                let moves = entity.clone();
                window.on_mouse_event(move |ev: &MouseMoveEvent, phase, _window, app| {
                    if phase != DispatchPhase::Bubble {
                        return;
                    }
                    let x = f32::from(ev.position.x);
                    let pressed = ev.pressed_button == Some(MouseButton::Left);
                    moves.update(app, |v, cx| v.on_window_move(x, pressed, cx));
                });
                let ups = entity.clone();
                window.on_mouse_event(move |ev: &MouseUpEvent, phase, _window, app| {
                    if phase != DispatchPhase::Bubble || ev.button != MouseButton::Left {
                        return;
                    }
                    ups.update(app, |v, cx| v.on_window_up(cx));
                });
            },
        )
        .absolute()
        .size(px(1.0))
    }

    /// 铺满预览区的测量层：绘制时记录预览区尺寸，变化则触发重绘以重新适配。
    fn measure_preview(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let cell = Rc::clone(&self.preview_box);
        let entity = cx.entity();
        canvas(
            move |bounds, _window, app| {
                let now = (f32::from(bounds.size.width), f32::from(bounds.size.height));
                let old = cell.get();
                if (now.0 - old.0).abs() > 0.5 || (now.1 - old.1).abs() > 0.5 {
                    cell.set(now);
                    entity.update(app, |_, cx| cx.notify());
                }
            },
            |_, _, _, _| {},
        )
        .absolute()
        .size_full()
    }

    /// 探测完成后只调一次窗口大小：用户已手动调整过则跳过；位置保持左上角不动。
    fn fit_window_once(&mut self, window: &mut Window, vw: u32, vh: u32, vp_now: (f32, f32)) {
        let untouched = self
            .initial_size
            .is_none_or(|(w, h)| (w - vp_now.0).abs() < 1.0 && (h - vp_now.1).abs() < 1.0);
        if !untouched {
            return;
        }
        let Some(work) = current_work_area_logical(window) else {
            return;
        };
        if let Some((w, h)) = initial_window_size(vw as f32, vh as f32, work.0, work.1) {
            window.resize(size(px(w), px(h)));
        }
    }

    /// 渲染工具栏：播放 / 暂停、分割、删除、恢复，右侧是当前时间 / 保留总时长。
    fn render_toolbar(&self, cx: &mut Context<Self>) -> Div {
        let i18n = crate::ocr_backend::i18n_for(self.locale);
        let Some(list) = self.list.as_ref() else {
            return div();
        };
        let can_play = self.media.as_ref().is_some_and(|m| m.is_ready());
        let at = self.playhead_ms;
        let in_gap = list.gap_at(at).is_some();
        let can_split = list.segment_at(at).is_some();
        let can_delete =
            list.segments().len() > 1 && (self.selected.is_some() || list.segment_at(at).is_some());
        let play_label = if self.playing {
            i18n.tr("video-editor-pause")
        } else {
            i18n.tr("video-editor-play")
        };
        let time = i18n.tr_with(
            "video-editor-time",
            &Args::new()
                .named("now", format_time(list.source_to_kept(at)))
                .named("total", format_time(list.kept_ms())),
        );
        div()
            .flex_none()
            .flex()
            .items_center()
            .gap_2()
            .child(
                Button::new("video-editor-play")
                    .with_size(ComponentSize::Small)
                    .primary()
                    .disabled(!can_play)
                    .label(play_label)
                    .on_click(cx.listener(|this, _: &ClickEvent, _w, cx| this.toggle_play(cx))),
            )
            .child(
                Button::new("video-editor-split")
                    .with_size(ComponentSize::Small)
                    .disabled(!can_split)
                    .label(i18n.tr("video-editor-split"))
                    .on_click(cx.listener(|this, _: &ClickEvent, _w, cx| this.split(cx))),
            )
            .child(
                Button::new("video-editor-delete")
                    .with_size(ComponentSize::Small)
                    .disabled(!can_delete)
                    .label(i18n.tr("video-editor-delete"))
                    .on_click(cx.listener(|this, _: &ClickEvent, _w, cx| this.delete_selected(cx))),
            )
            .child(
                Button::new("video-editor-restore")
                    .with_size(ComponentSize::Small)
                    .disabled(!in_gap)
                    .label(i18n.tr("video-editor-restore"))
                    .on_click(
                        cx.listener(|this, _: &ClickEvent, _w, cx| this.restore_at_playhead(cx)),
                    ),
            )
            .child(
                Button::new("video-editor-save-frame")
                    .with_size(ComponentSize::Small)
                    .disabled(!self.can_save_frame())
                    .label(i18n.tr("video-editor-save-frame"))
                    .on_click(cx.listener(|this, _: &ClickEvent, _w, cx| this.save_frame(cx))),
            )
            .child(div().flex_1())
            .child(
                div()
                    .text_size(px(13.0))
                    .text_color(rgba(COLOR_DIM))
                    .child(time),
            )
    }

    /// 渲染提示行：切点不在关键帧上 / 播放引擎不可用（两者同时只显示一条，固定一行高度）。
    fn render_note(&self) -> Div {
        let i18n = crate::ocr_backend::i18n_for(self.locale);
        let off_key = self
            .list
            .as_ref()
            .is_some_and(|l| any_cut_off_keyframe(l, &self.keyframes));
        let no_engine = self.media.as_ref().is_some_and(|m| m.is_failed());
        let text = if no_engine {
            Some(i18n.tr("video-editor-no-engine"))
        } else if off_key {
            Some(i18n.tr("video-editor-off-keyframe"))
        } else {
            None
        };
        div()
            .flex_none()
            .h(px(16.0))
            .text_size(px(11.0))
            .text_color(rgba(COLOR_NOTE))
            .children(text)
    }

    /// 渲染底部：状态行（进度 / 完成 / 失败）与按钮。
    fn render_footer(&self, cx: &mut Context<Self>) -> Div {
        let i18n = crate::ocr_backend::i18n_for(self.locale);
        let can_export =
            self.list.as_ref().is_some_and(|l| l.can_export()) && self.export.is_none();
        let mut status = div()
            .flex_1()
            .text_size(px(12.0))
            .text_color(rgba(COLOR_DIM));
        if let Some(job) = &self.export {
            let text = match job.progress {
                Some((done, total)) if total > 0 => i18n.tr_with(
                    "video-editor-exporting",
                    &Args::new()
                        .named("percent", (done * PERCENT / total).min(PERCENT).to_string()),
                ),
                _ => i18n.tr("video-editor-exporting-unknown"),
            };
            status = status.child(text);
        } else {
            match &self.outcome {
                Some(Outcome::Done(path)) => {
                    let shown = path.clone();
                    status = status
                        .flex()
                        .items_center()
                        .gap_2()
                        .child(i18n.tr_with(
                            "video-editor-done",
                            &Args::new().named("name", editor_file_label(path)),
                        ))
                        .child(
                            Button::new("video-editor-reveal")
                                .with_size(ComponentSize::Small)
                                .label(i18n.tr("video-editor-reveal"))
                                .on_click(move |_, _window, _app| {
                                    if let Err(e) = snow_platform::shell::reveal_in_explorer(&shown)
                                    {
                                        tracing::warn!(error = %e, "在资源管理器中显示失败");
                                    }
                                }),
                        );
                }
                Some(Outcome::Failed(reason)) => {
                    status = status.text_color(rgba(COLOR_ERROR)).child(i18n.tr_with(
                        "video-editor-failed",
                        &Args::new().named("reason", reason.as_str()),
                    ));
                }
                Some(Outcome::FrameFailed(reason)) => {
                    status = status.text_color(rgba(COLOR_ERROR)).child(i18n.tr_with(
                        "video-editor-frame-failed",
                        &Args::new().named("reason", reason.as_str()),
                    ));
                }
                None => {}
            }
        }
        div()
            .flex_none()
            .flex()
            .items_center()
            .gap_2()
            .child(status)
            .child(
                Button::new("video-editor-export")
                    .with_size(ComponentSize::Small)
                    .primary()
                    .disabled(!can_export)
                    .label(i18n.tr("video-editor-export"))
                    .on_click(cx.listener(|this, _: &ClickEvent, _w, cx| this.start_export(cx))),
            )
            .child(
                Button::new("video-editor-close")
                    .with_size(ComponentSize::Small)
                    .label(i18n.tr("video-editor-close"))
                    .on_click(|_, window, _app| window.remove_window()),
            )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 文件名只取最后一段；无文件名时回落为路径本身。
    #[test]
    fn file_label_uses_file_name() {
        assert_eq!(
            editor_file_label(Path::new("C:/Videos/rec 1.mp4")),
            "rec 1.mp4"
        );
        assert_eq!(editor_file_label(Path::new("rec.gif")), "rec.gif");
        assert_eq!(editor_file_label(Path::new("..")), "..");
    }

    /// BGRA 转图像：长度对得上才成功。
    #[test]
    fn bgra_image_checks_size() {
        assert!(bgra_image(2, 2, vec![0; 16]).is_some());
        assert!(bgra_image(2, 2, vec![0; 15]).is_none());
    }

    /// 编辑窗用到的文案两种语言都存在且不同。
    #[test]
    fn editor_texts_localized() {
        let zh = crate::ocr_backend::i18n_for("zh-CN");
        let en = crate::ocr_backend::i18n_for("en-US");
        for id in [
            "recording-video-editor-title",
            "video-editor-probing",
            "video-editor-preview-loading",
            "video-editor-export",
            "video-editor-close",
            "video-editor-exporting-unknown",
            "video-editor-reveal",
            "video-editor-play",
            "video-editor-pause",
            "video-editor-split",
            "video-editor-delete",
            "video-editor-restore",
            "video-editor-off-keyframe",
            "video-editor-no-engine",
            "video-editor-save-frame",
            "video-editor-save-frame-title",
            "video-editor-save-frame-filter",
            "video-editor-frame-failed",
        ] {
            assert!(
                !zh.tr(id).contains("[!") && !en.tr(id).contains("[!"),
                "{id}"
            );
            assert_ne!(zh.tr(id), en.tr(id), "{id}");
        }
    }
}
