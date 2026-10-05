//! 截图历史的应用侧存取：写入策略、后台写入线程、分页读取、缩略图与删除。
//!
//! 存储格式完全沿用 `snow-history` 的 `capture_history/`（与旧版 index.json v2 兼容）。
//! 每次操作都在进程级互斥锁内重新打开仓储，保证写入线程与历史页之间不会互相覆盖索引。
//! 本模块不依赖 GPUI，全部逻辑可离屏单测。

use crate::screenshot_output::encode_png;
use serde_json::Value;
use snow_config::document::ConfigDocument;
use snow_history::capture_history::{
    CaptureHistoryPolicy, CaptureHistoryRepository, DraftDisplay, DraftImage, HistoryDraft, Options,
};
use snow_history::index::{Point, Record, Rect, Selection};
use snow_history::pin_id::new_uuid_v4;
use snow_history::timeutil::{format_iso_utc_ms, now_utc_ms, parse_iso_utc_ms};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Mutex};

/// 配置键：是否记录截图历史。
pub const KEY_ENABLED: &str = "capture_history/enabled";
/// 配置键：永久保留。
pub const KEY_KEEP_PERMANENTLY: &str = "capture_history/keep_permanently";
/// 配置键：保留天数。
pub const KEY_RETENTION_DAYS: &str = "capture_history/retention_days";
/// 配置键：最大条数。
pub const KEY_MAX_ENTRIES: &str = "capture_history/max_entries";
/// 配置键：磁盘上限（MiB）。
pub const KEY_MAX_DISK_MIB: &str = "capture_history/max_disk_mib";

/// 写入线程名称。
const WRITER_THREAD_NAME: &str = "snow-history-writer";
/// 相同内容的去重窗口（毫秒）：一次截图既复制又保存时只留一条。
pub const DEDUPE_WINDOW_MS: i64 = 10_000;
/// 空画布历史（旧版格式要求为 JSON 对象或数组）。
const EMPTY_CANVAS: &[u8] = b"{}";
/// 默认选区阴影色（与旧版默认一致）。
const DEFAULT_SHADOW_COLOR: &str = "#FF333333";
/// 单显示器记录使用的稳定 ID。
const DISPLAY_STABLE_ID: &str = "cisox-capture";
/// 单显示器记录使用的显示名（内部标识，不展示给用户）。
const DISPLAY_NAME: &str = "capture";
/// 缩略图字节数 / 像素。
const BYTES_PER_PIXEL: usize = 4;
/// FNV-1a 64 位偏移基数。
const FNV_OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
/// FNV-1a 64 位质数。
const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;
/// 每秒毫秒数。
const MILLIS_PER_SECOND: i64 = 1000;
/// 本地时区偏移取整粒度（15 分钟）。
const OFFSET_ROUND_SECS: i64 = 900;

/// 全进程共享的仓储访问锁（写入线程与历史页共用）。
static REPO_LOCK: Mutex<()> = Mutex::new(());

/// 历史记录的来源（对应 index.json 的 `source` 取值）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HistorySource {
    /// 复制到剪贴板。
    Copied,
    /// 保存为文件。
    Saved,
    /// 贴到屏幕。
    Pinned,
    /// 直接截图：当前显示器。
    CurrentMonitor,
    /// 直接截图：前台窗口。
    FocusedWindow,
}

impl HistorySource {
    /// 写入索引的来源字符串。
    ///
    /// # 返回
    /// 旧版约定的 `source` 取值。
    ///
    /// ```ignore
    /// assert_eq!(HistorySource::Copied.as_str(), "copied_to_clipboard");
    /// ```
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Copied => "copied_to_clipboard",
            Self::Saved => "saved_to_file",
            Self::Pinned => "pinned_to_screen",
            Self::CurrentMonitor => "current_monitor",
            Self::FocusedWindow => "focused_window",
        }
    }

    /// 由索引里的来源字符串还原；未知取值返回 `None`。
    pub fn parse(text: &str) -> Option<Self> {
        [
            Self::Copied,
            Self::Saved,
            Self::Pinned,
            Self::CurrentMonitor,
            Self::FocusedWindow,
        ]
        .into_iter()
        .find(|s| s.as_str() == text)
    }

    /// 该来源对应的 i18n 消息 id。
    pub fn message_id(self) -> &'static str {
        match self {
            Self::Copied => "history-source-copied",
            Self::Saved => "history-source-saved",
            Self::Pinned => "history-source-pinned",
            Self::CurrentMonitor => "history-source-monitor",
            Self::FocusedWindow => "history-source-window",
        }
    }
}

/// 从配置文档读取历史策略；值非法时回退各键默认值。
///
/// # 参数
/// - `document`：配置文档。
///
/// # 返回
/// 策略（`is_valid()` 恒为真）。
///
/// ```ignore
/// let policy = policy_from_document(store.document());
/// ```
pub fn policy_from_document(document: &ConfigDocument) -> CaptureHistoryPolicy {
    let defaults = CaptureHistoryPolicy::default();
    let int = |key: &str, fallback: i32| {
        document
            .value(key)
            .as_i64()
            .and_then(|v| i32::try_from(v).ok())
            .unwrap_or(fallback)
    };
    let flag = |key: &str, fallback: bool| match document.value(key) {
        Value::Bool(b) => b,
        _ => fallback,
    };
    let policy = CaptureHistoryPolicy {
        enabled: flag(KEY_ENABLED, defaults.enabled),
        keep_permanently: flag(KEY_KEEP_PERMANENTLY, defaults.keep_permanently),
        retention_days: int(KEY_RETENTION_DAYS, defaults.retention_days),
        max_entries: int(KEY_MAX_ENTRIES, defaults.max_entries),
        max_disk_mib: int(KEY_MAX_DISK_MIB, defaults.max_disk_mib),
    };
    if policy.is_valid() { policy } else { defaults }
}

/// 计算像素内容的 FNV-1a 指纹（带尺寸），用于短时间内的重复判断。
///
/// # 参数
/// - `width` / `height`：图像尺寸。
/// - `rgba`：像素。
pub fn content_fingerprint(width: u32, height: u32, rgba: &[u8]) -> u64 {
    let mut hash = FNV_OFFSET ^ (u64::from(width) << 32 | u64::from(height));
    for byte in rgba {
        hash = (hash ^ u64::from(*byte)).wrapping_mul(FNV_PRIME);
    }
    hash
}

/// 去重器：窗口期内内容指纹相同的写入只保留第一条。
#[derive(Debug, Default)]
pub struct Deduper {
    /// 最近一次写入的 `(指纹, 时间戳毫秒)`。
    last: Option<(u64, i64)>,
}

impl Deduper {
    /// 判断并登记：返回 `true` 表示应当写入。
    ///
    /// # 参数
    /// - `fingerprint`：内容指纹。
    /// - `now_ms`：当前 UTC 毫秒。
    ///
    /// ```ignore
    /// let mut d = Deduper::default();
    /// assert!(d.admit(1, 0));
    /// assert!(!d.admit(1, 5_000));
    /// ```
    pub fn admit(&mut self, fingerprint: u64, now_ms: i64) -> bool {
        if let Some((last, at)) = self.last
            && last == fingerprint
            && (now_ms - at).abs() < DEDUPE_WINDOW_MS
        {
            return false;
        }
        self.last = Some((fingerprint, now_ms));
        true
    }
}

/// 把整幅截图包装成历史草稿（单显示器、画布为空对象，结果图与显示图同一份 PNG）。
///
/// # 参数
/// - `source`：来源。
/// - `width` / `height` / `rgba`：图像。
/// - `now_ms`：创建时间（UTC 毫秒）。
///
/// # 返回
/// 草稿；像素缓冲非法时返回错误说明。
pub fn build_draft(
    source: HistorySource,
    width: u32,
    height: u32,
    rgba: &[u8],
    now_ms: i64,
) -> Result<HistoryDraft, String> {
    let png = encode_png(width, height, rgba)?;
    let (w, h) = (i64::from(width), i64::from(height));
    let bounds = Rect {
        x: 0,
        y: 0,
        width: w,
        height: h,
        extra: Default::default(),
    };
    let image = DraftImage {
        width: w,
        height: h,
        png,
    };
    Ok(HistoryDraft {
        id: new_uuid_v4(),
        created_utc: format_iso_utc_ms(now_ms),
        source: source.as_str().to_string(),
        canvas_bounds: bounds.clone(),
        selection: Selection {
            rectangle: bounds,
            corner_radius: 0,
            shadow_width: 0,
            shadow_color: DEFAULT_SHADOW_COLOR.to_string(),
            lock_aspect_ratio: false,
            lock_drag_aspect_ratio: false,
            geometry: None,
            regions: None,
            extra: Default::default(),
        },
        canvas_history: EMPTY_CANVAS.to_vec(),
        displays: vec![DraftDisplay {
            image: image.clone(),
            stable_id: DISPLAY_STABLE_ID.to_string(),
            display_name: DISPLAY_NAME.to_string(),
            source_canvas_origin: Some(Point {
                x: 0,
                y: 0,
                extra: Default::default(),
            }),
            source_canvas_rect: None,
            backing_scale: None,
            native_display_id: None,
            canvas_space: None,
        }],
        result: Some(image),
        content_image: true,
        scrolling: None,
        desktop_geometry: None,
    })
}

/// 分页结果。
#[derive(Debug, Clone, PartialEq)]
pub struct HistoryPage {
    /// 当前页记录（时间倒序）。
    pub records: Vec<Record>,
    /// 总条数。
    pub total: usize,
    /// 实际页码（从 0 起，已钳制到有效范围）。
    pub page: usize,
    /// 总页数（空列表为 1）。
    pub page_count: usize,
}

/// 计算分页区间。
///
/// # 参数
/// - `total`：总条数。
/// - `page`：期望页码（从 0 起，越界会被钳制）。
/// - `page_size`：每页条数（至少为 1）。
///
/// # 返回
/// `(实际页码, 起, 止, 总页数)`，区间左闭右开。
///
/// ```ignore
/// assert_eq!(paginate(25, 9, 10), (2, 20, 25, 3));
/// ```
pub fn paginate(total: usize, page: usize, page_size: usize) -> (usize, usize, usize, usize) {
    let size = page_size.max(1);
    let page_count = total.div_ceil(size).max(1);
    let page = page.min(page_count - 1);
    let start = (page * size).min(total);
    (page, start, (start + size).min(total), page_count)
}

/// 记录的图像尺寸 `(宽, 高)`：优先结果图，其次首个显示器图。
pub fn record_size(record: &Record) -> (i64, i64) {
    record
        .result
        .as_ref()
        .map(|r| (r.width, r.height))
        .or_else(|| record.displays.first().map(|d| (d.width, d.height)))
        .unwrap_or((0, 0))
}

/// 记录里用于预览 / 复制的图片文件名：优先结果图，其次首个显示器图。
pub fn record_image_file(record: &Record) -> Option<String> {
    record
        .result
        .as_ref()
        .map(|r| r.image_file.clone())
        .or_else(|| record.displays.first().map(|d| d.image_file.clone()))
}

/// 把 `yyyy-MM-ddTHH:mm:ss.mmmZ` 按固定时区偏移格式化为 `yyyy-MM-dd HH:mm:ss`。
///
/// # 参数
/// - `created_utc`：索引里的创建时间。
/// - `offset_secs`：本地相对 UTC 的偏移秒数。
///
/// # 返回
/// 格式化文本；解析失败原样返回。
///
/// ```ignore
/// assert_eq!(format_local("2026-01-01T00:00:00.000Z", 8 * 3600), "2026-01-01 08:00:00");
/// ```
pub fn format_local(created_utc: &str, offset_secs: i64) -> String {
    let Some(ms) = parse_iso_utc_ms(created_utc) else {
        return created_utc.to_string();
    };
    let secs = (ms / MILLIS_PER_SECOND + offset_secs).max(0) as u64;
    let t = snow_platform::local_time::from_unix_utc(secs);
    format!(
        "{:04}-{:02}-{:02} {:02}:{:02}:{:02}",
        t.year, t.month, t.day, t.hour, t.minute, t.second
    )
}

/// 当前本地时区相对 UTC 的偏移秒数（取整到 15 分钟；夏令时按当前状态估算）。
pub fn local_offset_secs() -> i64 {
    let local = snow_platform::local_time::now();
    let utc =
        snow_platform::local_time::from_unix_utc((now_utc_ms() / MILLIS_PER_SECOND).max(0) as u64);
    let to_secs = |t: snow_platform::local_time::LocalDateTime| {
        let days = days_from_civil(i64::from(t.year), i64::from(t.month), i64::from(t.day));
        days * 86_400 + i64::from(t.hour) * 3600 + i64::from(t.minute) * 60 + i64::from(t.second)
    };
    let diff = to_secs(local) - to_secs(utc);
    (diff as f64 / OFFSET_ROUND_SECS as f64).round() as i64 * OFFSET_ROUND_SECS
}

/// 公历日期换算为自 1970-01-01 起的天数。
fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let y = if month <= 2 { year - 1 } else { year };
    let era = y.div_euclid(400);
    let yoe = y.rem_euclid(400);
    let mp = (month + 9) % 12;
    let doy = (153 * mp + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// 缩略图（BGRA，可直接装入 GPUI 图像资源）。
#[derive(Debug, Clone, PartialEq)]
pub struct Thumbnail {
    /// 宽（像素）。
    pub width: u32,
    /// 高（像素）。
    pub height: u32,
    /// BGRA 像素。
    pub bgra: Vec<u8>,
}

/// 把 PNG 字节解码并缩放成不超过 `max_edge` 的缩略图（不放大）。
///
/// # 参数
/// - `png`：PNG 字节。
/// - `max_edge`：长边上限。
///
/// # 返回
/// 缩略图；解码失败返回错误说明。
pub fn make_thumbnail(png: &[u8], max_edge: u32) -> Result<Thumbnail, String> {
    let rgba = image::load_from_memory_with_format(png, image::ImageFormat::Png)
        .map_err(|e| format!("解码历史图片失败: {e}"))?
        .to_rgba8();
    let (w, h) = rgba.dimensions();
    let longest = w.max(h).max(1);
    let small = if longest > max_edge {
        let scale = f64::from(max_edge) / f64::from(longest);
        let nw = ((f64::from(w) * scale).round() as u32).max(1);
        let nh = ((f64::from(h) * scale).round() as u32).max(1);
        image::imageops::thumbnail(&rgba, nw, nh)
    } else {
        rgba
    };
    let (width, height) = small.dimensions();
    let mut bgra = small.into_raw();
    for px in bgra.chunks_exact_mut(BYTES_PER_PIXEL) {
        px.swap(0, 2);
    }
    Ok(Thumbnail {
        width,
        height,
        bgra,
    })
}

/// 把 PNG 字节解码为 RGBA 像素。
///
/// # 返回
/// `(宽, 高, RGBA)`；解码失败返回错误说明。
pub fn decode_rgba(png: &[u8]) -> Result<(u32, u32, Vec<u8>), String> {
    let rgba = image::load_from_memory_with_format(png, image::ImageFormat::Png)
        .map_err(|e| format!("解码历史图片失败: {e}"))?
        .to_rgba8();
    let (w, h) = rgba.dimensions();
    Ok((w, h, rgba.into_raw()))
}

/// 历史存取入口：绑定数据根与策略，所有操作在进程级锁内完成。
#[derive(Debug, Clone)]
pub struct HistoryStore {
    /// 应用数据根目录。
    data_root: PathBuf,
    /// 生效策略。
    policy: CaptureHistoryPolicy,
}

impl HistoryStore {
    /// 创建存取入口。
    ///
    /// # 参数
    /// - `data_root`：应用数据根（`capture_history/` 的父目录）。
    /// - `policy`：策略。
    pub fn new(data_root: &Path, policy: CaptureHistoryPolicy) -> Self {
        Self {
            data_root: data_root.to_path_buf(),
            policy,
        }
    }

    /// 在锁内打开仓储并执行操作。
    fn with_repo<T>(
        &self,
        clock: Option<Arc<dyn Fn() -> i64 + Send + Sync>>,
        f: impl FnOnce(&mut CaptureHistoryRepository) -> T,
    ) -> T {
        let _guard = REPO_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let mut options = Options {
            policy: self.policy.clone(),
            ..Options::default()
        };
        if let Some(clock) = clock {
            options.clock = clock;
        }
        let mut repo = CaptureHistoryRepository::open(&self.data_root, options);
        f(&mut repo)
    }

    /// 写入一条历史（按策略淘汰超限旧记录）。
    ///
    /// # 参数
    /// - `source` / `width` / `height` / `rgba`：来源与图像。
    /// - `now_ms`：当前 UTC 毫秒（测试可注入）。
    ///
    /// # 返回
    /// 新记录；历史关闭、草稿非法或写盘失败返回错误说明。
    pub fn record(
        &self,
        source: HistorySource,
        width: u32,
        height: u32,
        rgba: &[u8],
        now_ms: i64,
    ) -> Result<Record, String> {
        let draft = build_draft(source, width, height, rgba, now_ms)?;
        self.with_repo(Some(Arc::new(move || now_ms)), |repo| {
            repo.publish(draft).map_err(|e| e.to_string())
        })
    }

    /// 读取一页记录；同时顺带清理已过期记录。
    ///
    /// # 参数
    /// - `page`：页码（从 0 起，越界钳制）。
    /// - `page_size`：每页条数。
    pub fn list_page(&self, page: usize, page_size: usize) -> HistoryPage {
        self.with_repo(None, |repo| {
            if repo.needs_maintenance()
                && let Err(e) = repo.maintenance()
            {
                tracing::warn!(error = %e, "截图历史维护失败");
            }
            let all = repo.records();
            let (page, start, end, page_count) = paginate(all.len(), page, page_size);
            HistoryPage {
                total: all.len(),
                records: all[start..end].to_vec(),
                page,
                page_count,
            }
        })
    }

    /// 读取记录的预览图 PNG 字节（校验失败时仓储会自行移除坏记录）。
    pub fn read_png(&self, record: &Record) -> Option<Vec<u8>> {
        let file = record_image_file(record)?;
        self.with_repo(None, |repo| repo.read_image_file(record, &file))
    }

    /// 记录的 PNG 文件在磁盘上的路径（用于“在资源管理器中定位”）。
    pub fn png_path(&self, record: &Record) -> Option<PathBuf> {
        let file = record_image_file(record)?;
        Some(
            self.data_root
                .join("capture_history")
                .join("records")
                .join(&record.id)
                .join(file),
        )
    }

    /// 删除一条记录。
    pub fn remove(&self, id: &str) -> Result<(), String> {
        self.with_repo(None, |repo| {
            repo.remove_many(&[id.to_string()])
                .map_err(|e| e.to_string())
        })
    }

    /// 清空全部历史。
    pub fn clear(&self) -> Result<(), String> {
        self.with_repo(None, |repo| repo.clear().map_err(|e| e.to_string()))
    }
}

/// 写入结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RecordOutcome {
    /// 已写入，携带记录 ID。
    Recorded(String),
    /// 因历史关闭而跳过。
    Disabled,
    /// 因短时间内容重复而跳过。
    Duplicate,
}

/// 一次写入任务。
struct Job {
    /// 提交时的策略快照。
    policy: CaptureHistoryPolicy,
    /// 来源。
    source: HistorySource,
    /// 宽。
    width: u32,
    /// 高。
    height: u32,
    /// RGBA 像素。
    rgba: Vec<u8>,
}

/// 处理一次写入：先判开关，再去重，最后写盘（后台线程与测试共用）。
///
/// # 参数
/// - `data_root`：数据根。
/// - `deduper`：去重器。
/// - `source` / `width` / `height` / `rgba`：来源与图像。
/// - `policy`：策略。
/// - `now_ms`：当前 UTC 毫秒。
pub fn process_record(
    data_root: &Path,
    deduper: &mut Deduper,
    policy: &CaptureHistoryPolicy,
    source: HistorySource,
    (width, height, rgba): (u32, u32, &[u8]),
    now_ms: i64,
) -> Result<RecordOutcome, String> {
    if !policy.enabled {
        return Ok(RecordOutcome::Disabled);
    }
    if !deduper.admit(content_fingerprint(width, height, rgba), now_ms) {
        return Ok(RecordOutcome::Duplicate);
    }
    HistoryStore::new(data_root, policy.clone())
        .record(source, width, height, rgba, now_ms)
        .map(|r| RecordOutcome::Recorded(r.id))
}

/// 后台写入器：提交即返回，写盘在独立线程完成，失败只记日志。
pub struct HistoryRecorder {
    /// 任务通道发送端。
    tx: Sender<Job>,
}

impl HistoryRecorder {
    /// 启动写入线程。
    ///
    /// # 参数
    /// - `data_root`：数据根。
    /// - `on_recorded`：每次成功写入后在写入线程上调用（只应做投递，例如通知历史页刷新）。
    ///
    /// # 返回
    /// 写入器；线程创建失败返回 IO 错误。
    ///
    /// ```ignore
    /// let recorder = HistoryRecorder::start(root, || {})?;
    /// recorder.submit(policy, HistorySource::Copied, w, h, rgba);
    /// ```
    pub fn start(
        data_root: &Path,
        on_recorded: impl Fn() + Send + 'static,
    ) -> std::io::Result<Self> {
        let (tx, rx) = channel::<Job>();
        let root = data_root.to_path_buf();
        std::thread::Builder::new()
            .name(WRITER_THREAD_NAME.into())
            .spawn(move || writer_loop(&root, rx, on_recorded))?;
        Ok(Self { tx })
    }

    /// 提交一次写入（历史关闭时直接忽略，不拷贝像素）。
    ///
    /// # 参数
    /// - `policy`：当前策略快照。
    /// - `source`：来源。
    /// - `width` / `height` / `rgba`：图像（会被拷贝）。
    pub fn submit(
        &self,
        policy: CaptureHistoryPolicy,
        source: HistorySource,
        width: u32,
        height: u32,
        rgba: &[u8],
    ) {
        if !policy.enabled {
            return;
        }
        let job = Job {
            policy,
            source,
            width,
            height,
            rgba: rgba.to_vec(),
        };
        if self.tx.send(job).is_err() {
            tracing::warn!("截图历史写入线程已退出，丢弃本次写入");
        }
    }
}

/// 写入线程主循环：逐个处理任务，失败只告警。
fn writer_loop(root: &Path, rx: Receiver<Job>, on_recorded: impl Fn()) {
    let mut deduper = Deduper::default();
    while let Ok(job) = rx.recv() {
        let result = process_record(
            root,
            &mut deduper,
            &job.policy,
            job.source,
            (job.width, job.height, &job.rgba),
            now_utc_ms(),
        );
        match result {
            Ok(RecordOutcome::Recorded(id)) => {
                tracing::debug!(%id, "已写入截图历史");
                on_recorded();
            }
            Ok(outcome) => tracing::debug!(?outcome, "截图历史未写入"),
            Err(e) => tracing::warn!(error = %e, "写入截图历史失败"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 生成唯一的临时数据根（不触碰上游数据目录）。
    fn temp_root(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "cisox-history-test-{tag}-{}-{}",
            std::process::id(),
            new_uuid_v4()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// 测试基准时间：取当前时刻，避免记录被保留期淘汰。
    fn base_ms() -> i64 {
        now_utc_ms()
    }

    /// 纯色 RGBA 像素。
    fn solid(w: u32, h: u32, shade: u8) -> Vec<u8> {
        vec![shade; (w * h * 4) as usize]
    }

    /// 默认策略但可调上限。
    fn policy(max_entries: i32) -> CaptureHistoryPolicy {
        CaptureHistoryPolicy {
            max_entries,
            ..CaptureHistoryPolicy::default()
        }
    }

    /// 来源字符串可往返，且与旧版取值一致。
    #[test]
    fn source_roundtrip() {
        for s in [
            HistorySource::Copied,
            HistorySource::Saved,
            HistorySource::Pinned,
            HistorySource::CurrentMonitor,
            HistorySource::FocusedWindow,
        ] {
            assert_eq!(HistorySource::parse(s.as_str()), Some(s));
        }
        assert_eq!(HistorySource::Saved.as_str(), "saved_to_file");
        assert_eq!(HistorySource::parse("x"), None);
    }

    /// 分页：越界钳制、空列表一页、末页不满。
    #[test]
    fn paginate_clamps() {
        assert_eq!(paginate(0, 3, 10), (0, 0, 0, 1));
        assert_eq!(paginate(25, 9, 10), (2, 20, 25, 3));
        assert_eq!(paginate(25, 1, 10), (1, 10, 20, 3));
        assert_eq!(paginate(10, 0, 0), (0, 0, 1, 10));
    }

    /// 去重窗口内相同指纹被拒，不同指纹或过窗口放行。
    #[test]
    fn deduper_window() {
        let mut d = Deduper::default();
        assert!(d.admit(1, 0));
        assert!(!d.admit(1, DEDUPE_WINDOW_MS - 1));
        assert!(d.admit(2, 100));
        assert!(d.admit(2, 100 + DEDUPE_WINDOW_MS));
    }

    /// 指纹随尺寸与内容变化。
    #[test]
    fn fingerprint_differs() {
        let a = solid(2, 2, 1);
        assert_eq!(content_fingerprint(2, 2, &a), content_fingerprint(2, 2, &a));
        assert_ne!(
            content_fingerprint(2, 2, &a),
            content_fingerprint(2, 2, &solid(2, 2, 2))
        );
        assert_ne!(content_fingerprint(2, 2, &a), content_fingerprint(4, 1, &a));
    }

    /// 配置缺省与非法值回退到旧版默认策略。
    #[test]
    fn policy_defaults_from_document() {
        let doc = ConfigDocument::from_bytes(None);
        let p = policy_from_document(&doc);
        assert_eq!(p, CaptureHistoryPolicy::default());
        assert!(p.enabled && p.max_entries == 100 && p.retention_days == 7);
    }

    /// 写入后可分页读取，倒序且字段正确；关闭开关不写入。
    #[test]
    fn record_list_and_disabled() {
        let root = temp_root("list");
        let mut dedupe = Deduper::default();
        let p = policy(100);
        for i in 0..3u8 {
            let out = process_record(
                &root,
                &mut dedupe,
                &p,
                HistorySource::Copied,
                (4, 3, &solid(4, 3, i)),
                base_ms() + i64::from(i) * 1000,
            )
            .unwrap();
            assert!(matches!(out, RecordOutcome::Recorded(_)));
        }
        let off = CaptureHistoryPolicy {
            enabled: false,
            ..p.clone()
        };
        let out = process_record(
            &root,
            &mut dedupe,
            &off,
            HistorySource::Saved,
            (4, 3, &solid(4, 3, 9)),
            base_ms() + 100_000,
        )
        .unwrap();
        assert_eq!(out, RecordOutcome::Disabled);
        let store = HistoryStore::new(&root, p);
        let page = store.list_page(0, 2);
        assert_eq!((page.total, page.page_count, page.records.len()), (3, 2, 2));
        assert!(page.records[0].created_utc > page.records[1].created_utc);
        assert_eq!(record_size(&page.records[0]), (4, 3));
        let last = store.list_page(5, 2);
        assert_eq!((last.page, last.records.len()), (1, 1));
        let _ = std::fs::remove_dir_all(&root);
    }

    /// 同一内容短时间内只留一条。
    #[test]
    fn duplicate_is_skipped() {
        let root = temp_root("dup");
        let mut dedupe = Deduper::default();
        let p = policy(100);
        let img = solid(2, 2, 7);
        let first = process_record(
            &root,
            &mut dedupe,
            &p,
            HistorySource::Copied,
            (2, 2, &img),
            base_ms(),
        )
        .unwrap();
        let second = process_record(
            &root,
            &mut dedupe,
            &p,
            HistorySource::Saved,
            (2, 2, &img),
            base_ms() + 1000,
        )
        .unwrap();
        assert!(matches!(first, RecordOutcome::Recorded(_)));
        assert_eq!(second, RecordOutcome::Duplicate);
        assert_eq!(HistoryStore::new(&root, p).list_page(0, 10).total, 1);
        let _ = std::fs::remove_dir_all(&root);
    }

    /// 超过最大条数时淘汰最旧的。
    #[test]
    fn max_entries_prunes_oldest() {
        let root = temp_root("cap");
        let mut dedupe = Deduper::default();
        let p = policy(2);
        let mut ids = Vec::new();
        for i in 0..4u8 {
            if let RecordOutcome::Recorded(id) = process_record(
                &root,
                &mut dedupe,
                &p,
                HistorySource::Pinned,
                (2, 2, &solid(2, 2, i)),
                base_ms() + i64::from(i) * 1000,
            )
            .unwrap()
            {
                ids.push(id);
            }
        }
        let page = HistoryStore::new(&root, p).list_page(0, 10);
        assert_eq!(page.total, 2);
        assert_eq!(page.records[0].id, ids[3]);
        assert_eq!(page.records[1].id, ids[2]);
        let _ = std::fs::remove_dir_all(&root);
    }

    /// 删除单条后列表刷新；清空后为空；缩略图与解码可用。
    #[test]
    fn remove_clear_and_images() {
        let root = temp_root("del");
        let mut dedupe = Deduper::default();
        let p = policy(100);
        for i in 0..2u8 {
            process_record(
                &root,
                &mut dedupe,
                &p,
                HistorySource::CurrentMonitor,
                (40, 20, &solid(40, 20, i)),
                base_ms() + i64::from(i) * 1000,
            )
            .unwrap();
        }
        let store = HistoryStore::new(&root, p);
        let page = store.list_page(0, 10);
        let target = page.records[0].clone();
        let png = store.read_png(&target).unwrap();
        let thumb = make_thumbnail(&png, 10).unwrap();
        assert_eq!((thumb.width, thumb.height), (10, 5));
        assert_eq!(thumb.bgra.len(), 10 * 5 * 4);
        let (w, h, rgba) = decode_rgba(&png).unwrap();
        assert_eq!((w, h, rgba.len()), (40, 20, 40 * 20 * 4));
        assert!(store.png_path(&target).unwrap().exists());
        store.remove(&target.id).unwrap();
        let after = store.list_page(0, 10);
        assert_eq!(after.total, 1);
        assert!(after.records.iter().all(|r| r.id != target.id));
        store.clear().unwrap();
        assert_eq!(store.list_page(0, 10).total, 0);
        let _ = std::fs::remove_dir_all(&root);
    }

    /// 本地时间格式化按偏移换算。
    #[test]
    fn local_format_applies_offset() {
        assert_eq!(
            format_local("2026-01-01T00:00:00.000Z", 8 * 3600),
            "2026-01-01 08:00:00"
        );
        assert_eq!(
            format_local("2026-01-01T00:30:00.000Z", -3600),
            "2025-12-31 23:30:00"
        );
        assert_eq!(format_local("bad", 0), "bad");
        assert!(local_offset_secs().abs() <= 14 * 3600);
    }

    /// 后台写入器：提交后最终落盘并触发回调；关闭时不触发。
    #[test]
    fn recorder_writes_in_background() {
        let root = temp_root("bg");
        let (tx, rx) = channel::<()>();
        let tx = Mutex::new(tx);
        let recorder = HistoryRecorder::start(&root, move || {
            let _ = tx.lock().unwrap().send(());
        })
        .unwrap();
        recorder.submit(
            CaptureHistoryPolicy {
                enabled: false,
                ..CaptureHistoryPolicy::default()
            },
            HistorySource::Copied,
            2,
            2,
            &solid(2, 2, 1),
        );
        recorder.submit(
            CaptureHistoryPolicy::default(),
            HistorySource::Copied,
            2,
            2,
            &solid(2, 2, 2),
        );
        rx.recv_timeout(std::time::Duration::from_secs(10))
            .expect("应收到写入回调");
        assert_eq!(
            HistoryStore::new(&root, CaptureHistoryPolicy::default())
                .list_page(0, 10)
                .total,
            1
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    /// 来源文案在两种语言下都能解析（不会回显消息 id）。
    #[test]
    fn source_messages_resolve_in_both_locales() {
        for locale in ["en-US", "zh-CN"] {
            let i18n = crate::ocr_backend::i18n_for(locale);
            for s in [
                HistorySource::Copied,
                HistorySource::Saved,
                HistorySource::Pinned,
                HistorySource::CurrentMonitor,
                HistorySource::FocusedWindow,
            ] {
                assert_ne!(
                    i18n.tr(s.message_id()),
                    s.message_id(),
                    "{locale} 缺 {}",
                    s.message_id()
                );
            }
        }
    }
}
