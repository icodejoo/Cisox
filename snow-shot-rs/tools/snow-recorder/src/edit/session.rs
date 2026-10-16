//! 常驻编辑会话：打开一次文件，之后多次取缩略图 / 预览帧，窗口关闭即退出。
//!
//! 线程模型：stdin 读取线程（在 `main.rs`）把行送进通道；本模块的调度循环（主线程）解析命令，
//! 解码工作线程独占 `VideoSource`。每条 `Thumbs` / `Frame` / `Close` 都会让"代数"加一，
//! 工作线程在每张帧前后比对代数，旧请求立刻放弃、不再回报（取消语义，无锁、不阻塞调度）。
//! 播放由 UI 进程用系统媒体引擎完成，会话不做播放。
//!
//! 帧文件放在 `snow_recorder_protocol::session_scratch_dir(pid)` 下，格式为纯 BGRA：
//! 缩略图 `t<id>-<index>.bgra`（保留当前与上一批，更早的批次删除），
//! 预览帧 `f<seq>.bgra`（只保留最近 `FRAME_KEEP` 个），会话结束整目录删除。

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver};

use snow_recorder_protocol::{
    Command, Event, FRAME_KEEP, ProbeInfo, frame_file_name, session_scratch_dir, thumb_file_name,
};

use super::EditError;
use super::source::VideoSource;
use super::thumbs::{FfmpegRenderer, Fit, FrameRenderer, write_pixels};

/// 关键帧表单行最多条目数（超出则均匀抽稀，单行约 40 KB 以内）。
pub const KEYFRAME_MAX: usize = 4096;
/// 缩略图保留的批次数（当前批 + 上一批，让界面切换时旧图仍可读）。
const THUMB_BATCHES_KEPT: usize = 2;
/// 进程退出码：正常。
const EXIT_OK: i32 = 0;
/// 进程退出码：失败。
const EXIT_FAILED: i32 = 1;

/// 事件出口。
pub type EmitFn = Box<dyn Fn(&Event) + Send + Sync>;

/// 工作线程要做的一件事。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Job {
    /// 取一批缩略图。
    Thumbs {
        /// 批次号。
        id: u64,
        /// 尺寸约束（框内等比）。
        fit: Fit,
        /// 取帧时间（毫秒）。
        times_ms: Vec<u64>,
    },
    /// 取一张预览帧。
    Frame {
        /// 请求序号。
        seq: u64,
        /// 时间（毫秒）。
        at_ms: u64,
        /// 最大宽度。
        max_w: u32,
    },
}

/// 带代数的任务；代数与全局代数不一致即视为已取消。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Ticket {
    /// 提交时的代数。
    pub generation: u64,
    /// 任务。
    pub job: Job,
}

/// 把关键帧表均匀抽稀到不超过 `max` 条（保留首尾）。
///
/// # 参数
/// - `ms`：升序关键帧时间。
/// - `max`：最大条数（至少按 2 处理）。
///
/// # 返回
/// 抽稀后的升序列表；条数不超限时原样返回。
pub fn decimate_keyframes(ms: Vec<u64>, max: usize) -> Vec<u64> {
    let max = max.max(2);
    if ms.len() <= max {
        return ms;
    }
    let last = ms.len() - 1;
    let steps = max - 1;
    (0..=steps).map(|i| ms[i * last / steps]).collect()
}

/// 解码工作者：执行任务、写帧文件、回报事件，并在每一步检查是否已被取消。
pub struct Worker<R: FrameRenderer> {
    /// 取帧后端。
    renderer: R,
    /// 帧文件目录。
    dir: PathBuf,
    /// 全局代数（调度线程递增）。
    generation: Arc<AtomicU64>,
    /// 事件出口。
    emit: EmitFn,
    /// 最近几批缩略图文件，超出保留数的最旧批次会被删除。
    thumb_history: VecDeque<Vec<PathBuf>>,
}

impl<R: FrameRenderer> Worker<R> {
    /// 创建工作者。
    ///
    /// # 参数
    /// - `renderer`：取帧后端。
    /// - `dir`：帧文件目录（需已存在）。
    /// - `generation`：与调度端共享的代数。
    /// - `emit`：事件出口。
    pub fn new(renderer: R, dir: PathBuf, generation: Arc<AtomicU64>, emit: EmitFn) -> Self {
        Self {
            renderer,
            dir,
            generation,
            emit,
            thumb_history: VecDeque::new(),
        }
    }

    /// 该代数的任务是否仍有效。
    fn live(&self, generation: u64) -> bool {
        self.generation.load(Ordering::SeqCst) == generation
    }

    /// 执行一个任务；中途被取消则立刻返回，已取消的任务不再产生任何事件。
    ///
    /// # 参数
    /// - `ticket`：带代数的任务。
    pub fn run(&mut self, ticket: Ticket) {
        if !self.live(ticket.generation) {
            return;
        }
        match ticket.job {
            Job::Thumbs { id, fit, times_ms } => self.thumbs(ticket.generation, id, fit, &times_ms),
            Job::Frame { seq, at_ms, max_w } => {
                self.frame(ticket.generation, seq, at_ms, max_w);
            }
        }
    }

    /// 渐进取一批缩略图。
    fn thumbs(&mut self, generation: u64, id: u64, fit: Fit, times_ms: &[u64]) {
        self.thumb_history.push_back(Vec::new());
        while self.thumb_history.len() > THUMB_BATCHES_KEPT {
            for old in self.thumb_history.pop_front().unwrap_or_default() {
                let _ = std::fs::remove_file(old);
            }
        }
        for (index, &at) in times_ms.iter().enumerate() {
            if !self.live(generation) {
                return;
            }
            let shot = match self.renderer.render(at, fit) {
                Ok(s) => s,
                Err(e) => {
                    eprintln!("缩略图取帧失败（跳过）: {e}");
                    continue;
                }
            };
            if !self.live(generation) {
                return;
            }
            let path = self.dir.join(thumb_file_name(id, index));
            if let Err(e) = write_pixels(&path, &shot.pixels) {
                eprintln!("缩略图写文件失败（跳过）: {e}");
                continue;
            }
            if let Some(batch) = self.thumb_history.back_mut() {
                batch.push(path.clone());
            }
            (self.emit)(&Event::ThumbReady {
                id,
                index: index as u64,
                at_ms: shot.at_ms,
                w: shot.w,
                h: shot.h,
                path,
            });
        }
    }

    /// 取一张预览帧，并删掉超出保留数的旧文件。
    fn frame(&mut self, generation: u64, seq: u64, at_ms: u64, max_w: u32) {
        let shot = match self.renderer.render(at_ms, Fit::MaxWidth(max_w)) {
            Ok(s) => s,
            Err(e) => {
                eprintln!("预览取帧失败（跳过）: {e}");
                return;
            }
        };
        if !self.live(generation) {
            return;
        }
        let path = self.dir.join(frame_file_name(seq));
        if let Err(e) = write_pixels(&path, &shot.pixels) {
            eprintln!("预览写文件失败（跳过）: {e}");
            return;
        }
        if let Some(old) = seq.checked_sub(FRAME_KEEP) {
            let _ = std::fs::remove_file(self.dir.join(frame_file_name(old)));
        }
        (self.emit)(&Event::FrameReady {
            seq,
            at_ms: shot.at_ms,
            w: shot.w,
            h: shot.h,
            path,
        });
    }
}

/// 打开视频并扫描。
///
/// # 参数
/// - `input`：输入视频。
///
/// # 返回
/// `(取帧后端, 探测信息, 关键帧毫秒表)`。
pub fn open_renderer(input: &Path) -> Result<(FfmpegRenderer, ProbeInfo, Vec<u64>), EditError> {
    let mut src = VideoSource::open(input)?;
    let (info, keys) = src.scan()?;
    let ms = keys
        .into_iter()
        .map(|k| u64::try_from(src.pts_to_ms(k)).unwrap_or(0))
        .collect();
    Ok((FfmpegRenderer::new(src), info, ms))
}

/// 回报打开结果：先 `ProbeResult`，再 `Keyframes`（超长时抽稀）。
///
/// # 参数
/// - `info`：探测信息。
/// - `keys`：关键帧毫秒表。
/// - `emit`：事件出口。
pub fn announce(info: ProbeInfo, keys: Vec<u64>, emit: &dyn Fn(&Event)) {
    emit(&Event::ProbeResult(info));
    emit(&Event::Keyframes {
        ms: decimate_keyframes(keys, KEYFRAME_MAX),
    });
}

/// 工作线程主体：打开文件、回报、然后按任务循环；队列里积压的旧任务只取最新一个。
fn worker_main(
    input: &Path,
    dir: PathBuf,
    generation: Arc<AtomicU64>,
    jobs: &Receiver<Ticket>,
    emit: EmitFn,
) {
    let (renderer, info, keys) = match open_renderer(input) {
        Ok(v) => v,
        Err(e) => {
            emit(&Event::Error {
                reason: e.to_string(),
            });
            let _ = std::fs::remove_dir_all(&dir);
            std::process::exit(EXIT_FAILED);
        }
    };
    announce(info, keys, emit.as_ref());
    let mut worker = Worker::new(renderer, dir, generation, emit);
    while let Ok(mut ticket) = jobs.recv() {
        while let Ok(newer) = jobs.try_recv() {
            ticket = newer;
        }
        worker.run(ticket);
    }
}

/// 把协议命令转成工作线程任务；非取帧命令返回 `None`。
///
/// # 参数
/// - `cmd`：协议命令。
pub fn job_of(cmd: &Command) -> Option<Job> {
    match cmd {
        Command::Thumbs {
            id,
            width,
            height,
            times_ms,
        } => Some(Job::Thumbs {
            id: *id,
            fit: Fit::Within {
                w: *width,
                h: *height,
            },
            times_ms: times_ms.clone(),
        }),
        Command::Frame { seq, at_ms, max_w } => Some(Job::Frame {
            seq: *seq,
            at_ms: *at_ms,
            max_w: *max_w,
        }),
        _ => None,
    }
}

/// 运行编辑会话直到关闭；不会返回（以退出码结束进程）。
///
/// # 参数
/// - `input`：要打开的视频。
/// - `lines`：命令行通道；`None` 或断开表示 stdin 关闭。
/// - `emit`：事件出口（生产环境写 stdout）。
///
/// # 说明
/// 收到 `Close` 或 stdin 关闭：取消在途任务、等工作线程收尾、整目录删除帧文件，然后正常退出。
/// 会话期间不接受录制命令（`Start` 等被忽略）。
pub fn run(input: &Path, lines: &Receiver<Option<String>>, emit: EmitFn) -> ! {
    let dir = session_scratch_dir(std::process::id());
    if let Err(e) = std::fs::create_dir_all(&dir) {
        emit(&Event::Error {
            reason: format!("创建会话目录失败: {e}"),
        });
        std::process::exit(EXIT_FAILED);
    }
    let generation = Arc::new(AtomicU64::new(0));
    let (tx, jobs) = mpsc::channel::<Ticket>();
    let handle = {
        let (input, dir, generation) = (input.to_path_buf(), dir.clone(), Arc::clone(&generation));
        std::thread::spawn(move || worker_main(&input, dir, generation, &jobs, emit))
    };
    while let Ok(Some(line)) = lines.recv() {
        if line.trim().is_empty() {
            continue;
        }
        let cmd = match Command::parse(line.trim_start_matches('\u{feff}')) {
            Ok(c) => c,
            Err(e) => {
                eprintln!("忽略无法解析的命令: {e}");
                continue;
            }
        };
        if matches!(cmd, Command::Close) {
            break;
        }
        match job_of(&cmd) {
            Some(job) => {
                let g = generation.fetch_add(1, Ordering::SeqCst) + 1;
                let _ = tx.send(Ticket { generation: g, job });
            }
            None => eprintln!("编辑会话中忽略命令: {cmd:?}"),
        }
    }
    // 取消在途任务，再让工作线程因通道关闭而退出
    generation.fetch_add(1, Ordering::SeqCst);
    drop(tx);
    let _ = handle.join();
    let _ = std::fs::remove_dir_all(&dir);
    std::process::exit(EXIT_OK);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::edit::testclip::{self, Clip};
    use crate::edit::thumbs::Rendered;
    use std::sync::Mutex;

    /// 假取帧后端：每次返回 2x2 像素，并在指定调用次数时执行钩子。
    struct Fake {
        /// 已调用次数。
        calls: usize,
        /// 第 n 次调用（从 1 起）时执行的钩子。
        hook: Option<(usize, Box<dyn FnMut() + Send>)>,
    }

    impl FrameRenderer for Fake {
        /// 返回固定像素。
        fn render(&mut self, ms: u64, _fit: Fit) -> Result<Rendered, EditError> {
            self.calls += 1;
            if let Some((n, f)) = self.hook.as_mut()
                && *n == self.calls
            {
                f();
            }
            Ok(Rendered {
                at_ms: ms,
                w: 2,
                h: 2,
                pixels: vec![7; 16],
            })
        }
    }

    /// 收集事件的出口。
    fn collector() -> (Arc<Mutex<Vec<Event>>>, EmitFn) {
        let log = Arc::new(Mutex::new(Vec::new()));
        let sink = Arc::clone(&log);
        (log, Box::new(move |e| sink.lock().unwrap().push(e.clone())))
    }

    /// 建一个用假后端的工作者。
    fn fake_worker(
        name: &str,
        hook: Option<(usize, Box<dyn FnMut() + Send>)>,
        generation: &Arc<AtomicU64>,
    ) -> (Worker<Fake>, Arc<Mutex<Vec<Event>>>, PathBuf) {
        let dir = testclip::temp_dir(name);
        let (log, emit) = collector();
        let w = Worker::new(
            Fake { calls: 0, hook },
            dir.clone(),
            Arc::clone(generation),
            emit,
        );
        (w, log, dir)
    }

    /// 抽稀：不超限原样；超限保留首尾且条数正确、仍升序。
    #[test]
    fn decimate_keeps_ends() {
        let small: Vec<u64> = (0..10).collect();
        assert_eq!(decimate_keyframes(small.clone(), 4096), small);
        let big: Vec<u64> = (0..10_000).map(|i| i * 10).collect();
        let out = decimate_keyframes(big.clone(), 100);
        assert_eq!(out.len(), 100);
        assert_eq!((out[0], out[99]), (0, *big.last().unwrap()));
        assert!(out.windows(2).all(|w| w[0] <= w[1]));
    }

    /// 命令转任务：缩略图用框内缩放，预览带最大宽，其它命令不产生任务。
    #[test]
    fn job_mapping() {
        let t = Command::Thumbs {
            id: 2,
            width: 8,
            height: 4,
            times_ms: vec![1],
        };
        assert_eq!(
            job_of(&t),
            Some(Job::Thumbs {
                id: 2,
                fit: Fit::Within { w: 8, h: 4 },
                times_ms: vec![1]
            })
        );
        assert!(job_of(&Command::Close).is_none());
    }

    /// 批内被新代数打断：之后的缩略图不再回报，旧 id 不再出现。
    #[test]
    fn newer_request_cancels_batch() {
        let generation = Arc::new(AtomicU64::new(1));
        let g2 = Arc::clone(&generation);
        let hook: Box<dyn FnMut() + Send> = Box::new(move || {
            g2.store(2, Ordering::SeqCst);
        });
        let (mut w, log, dir) = fake_worker("sess-cancel", Some((2, hook)), &generation);
        w.run(Ticket {
            generation: 1,
            job: Job::Thumbs {
                id: 1,
                fit: Fit::MaxWidth(0),
                times_ms: vec![0, 10, 20, 30],
            },
        });
        {
            let events = log.lock().unwrap();
            // 第 1 张正常回报；第 2 张渲染时被打断，其后一律不回报
            assert_eq!(events.len(), 1, "{events:?}");
            assert!(matches!(
                events[0],
                Event::ThumbReady {
                    id: 1,
                    index: 0,
                    ..
                }
            ));
        }
        // 已过期代数的任务一开始就被丢弃
        w.run(Ticket {
            generation: 1,
            job: Job::Frame {
                seq: 5,
                at_ms: 0,
                max_w: 0,
            },
        });
        assert_eq!(log.lock().unwrap().len(), 1);
        // 新代数的任务照常执行
        w.run(Ticket {
            generation: 2,
            job: Job::Frame {
                seq: 6,
                at_ms: 40,
                max_w: 0,
            },
        });
        assert!(matches!(
            log.lock().unwrap().last(),
            Some(Event::FrameReady {
                seq: 6,
                at_ms: 40,
                ..
            })
        ));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 预览帧文件只保留最近 `FRAME_KEEP` 个。
    #[test]
    fn frame_files_are_pruned() {
        let generation = Arc::new(AtomicU64::new(1));
        let (mut w, _log, dir) = fake_worker("sess-prune", None, &generation);
        for seq in 0..20u64 {
            w.run(Ticket {
                generation: 1,
                job: Job::Frame {
                    seq,
                    at_ms: 0,
                    max_w: 0,
                },
            });
        }
        let count = std::fs::read_dir(&dir).unwrap().count() as u64;
        assert_eq!(count, FRAME_KEEP);
        assert!(!dir.join(frame_file_name(3)).exists());
        assert!(dir.join(frame_file_name(4)).exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 缩略图只保留当前与上一批。
    #[test]
    fn thumb_batches_are_pruned() {
        let generation = Arc::new(AtomicU64::new(1));
        let (mut w, _log, dir) = fake_worker("sess-batches", None, &generation);
        for id in 1..=3u64 {
            w.run(Ticket {
                generation: 1,
                job: Job::Thumbs {
                    id,
                    fit: Fit::MaxWidth(0),
                    times_ms: vec![0, 1],
                },
            });
        }
        assert!(!dir.join(thumb_file_name(1, 0)).exists());
        assert!(dir.join(thumb_file_name(2, 1)).exists());
        assert!(dir.join(thumb_file_name(3, 1)).exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 集成：真实样片 Open -> Frame -> Thumbs，事件顺序、文件大小正确，取消后旧批次不再回报。
    #[test]
    fn session_end_to_end() {
        let dir = testclip::temp_dir("sess-e2e");
        let clip_path = dir.join("clip.mp4");
        let clip = Clip::default();
        testclip::make_clip(&clip_path, &clip).unwrap();
        let (renderer, info, keys) = open_renderer(&clip_path).unwrap();
        let frames_dir = dir.join("frames");
        std::fs::create_dir_all(&frames_dir).unwrap();
        let (log, emit) = collector();
        // 打开阶段的事件
        announce(info, keys, &*emit);
        {
            let events = log.lock().unwrap();
            assert!(matches!(events[0], Event::ProbeResult(i) if i.frames == 72));
            assert!(matches!(&events[1], Event::Keyframes { ms } if ms == &[0, 960, 1920]));
        }
        log.lock().unwrap().clear();
        let generation = Arc::new(AtomicU64::new(1));
        let mut w = Worker::new(renderer, frames_dir, Arc::clone(&generation), emit);
        w.run(Ticket {
            generation: 1,
            job: Job::Frame {
                seq: 1,
                at_ms: 500,
                max_w: 32,
            },
        });
        w.run(Ticket {
            generation: 1,
            job: Job::Thumbs {
                id: 7,
                fit: Fit::Within { w: 16, h: 16 },
                times_ms: vec![0, 1000, 2000],
            },
        });
        // 旧代数的批次：不再回报
        generation.store(2, Ordering::SeqCst);
        w.run(Ticket {
            generation: 1,
            job: Job::Thumbs {
                id: 8,
                fit: Fit::MaxWidth(0),
                times_ms: vec![0],
            },
        });
        let events = log.lock().unwrap().clone();
        assert_eq!(events.len(), 4, "{events:?}");
        let mut sizes = Vec::new();
        for e in &events {
            match e {
                Event::FrameReady { w, h, path, .. } | Event::ThumbReady { w, h, path, .. } => {
                    let len = std::fs::metadata(path).unwrap().len();
                    assert_eq!(len, u64::from(*w) * u64::from(*h) * 4);
                    sizes.push((*w, *h));
                }
                other => panic!("意外事件 {other:?}"),
            }
        }
        assert_eq!(sizes, [(32, 24), (16, 12), (16, 12), (16, 12)]);
        assert!(
            !events
                .iter()
                .any(|e| matches!(e, Event::ThumbReady { id: 8, .. }))
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
