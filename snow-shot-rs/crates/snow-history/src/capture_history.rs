//! 截图历史仓储（`capture_history/`）容器层。
//!
//! 逐项对照 C++ `capturehistoryrepository.cpp`：`index.json`（写 2、读 1/2）、
//! `records/<uuid>/` 目录、`pending_deletions` 两阶段删除、体积与条数策略、
//! 读盘前的符号链接与路径包含校验。`canvas_history.json` 与 PNG 一律当不透明字节。
//! 本模块同步执行，并发与排队由调用方负责。

use std::collections::{BTreeMap, HashSet};
use std::fmt;
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use crate::fsutil::{contained_path, write_atomic};
use crate::index::{
    self, DesktopGeometry, Display, HistoryIndex, PendingDeletion, Point, Record, Rect,
    ResultImage, Selection,
};
use crate::timeutil::{MILLIS_PER_DAY, now_utc_ms, parse_iso_utc_ms};

/// 数据根下的历史目录名。
const HISTORY_DIR: &str = "capture_history";
/// 记录子目录名。
const RECORDS_DIR: &str = "records";
/// 索引文件名。
const INDEX_FILE: &str = "index.json";
/// 崩溃遗留的临时记录目录前缀。
const TEMP_PREFIX: &str = ".tmp-";
/// 读盘失败统一原因。
const READ_FAILED: &str = "Unable to read a capture-history payload";
/// 合法的 `source` 取值（对照 C++ `sourceText`）。
const SOURCES: [&str; 5] = [
    "copied_to_clipboard",
    "saved_to_file",
    "pinned_to_screen",
    "current_monitor",
    "focused_window",
];

/// 截图历史策略。
#[derive(Debug, Clone, PartialEq)]
pub struct CaptureHistoryPolicy {
    /// 是否启用；关闭后拒绝新发布，且不做任何淘汰。
    pub enabled: bool,
    /// 保留天数（1..=365）。
    pub retention_days: i32,
    /// 最大条数（1..=1000）。
    pub max_entries: i32,
    /// 磁盘上限，单位 MiB（128..=10240）。
    pub max_disk_mib: i32,
    /// 永久保留：绕过年龄、条数、体积限制。
    pub keep_permanently: bool,
}

impl CaptureHistoryPolicy {
    /// 保留天数下限。
    pub const MIN_RETENTION_DAYS: i32 = 1;
    /// 保留天数上限。
    pub const MAX_RETENTION_DAYS: i32 = 365;
    /// 条数下限。
    pub const MIN_ENTRIES: i32 = 1;
    /// 条数上限。
    pub const MAX_ENTRIES: i32 = 1000;
    /// 磁盘上限下限（MiB）。
    pub const MIN_DISK_MIB: i32 = 128;
    /// 磁盘上限上限（MiB）。
    pub const MAX_DISK_MIB: i32 = 10240;

    /// 各项取值是否都在合法范围内。
    pub fn is_valid(&self) -> bool {
        (Self::MIN_RETENTION_DAYS..=Self::MAX_RETENTION_DAYS).contains(&self.retention_days)
            && (Self::MIN_ENTRIES..=Self::MAX_ENTRIES).contains(&self.max_entries)
            && (Self::MIN_DISK_MIB..=Self::MAX_DISK_MIB).contains(&self.max_disk_mib)
    }
}

impl Default for CaptureHistoryPolicy {
    /// 默认：启用、7 天、100 条、1024 MiB、不永久保留。
    fn default() -> Self {
        Self {
            enabled: true,
            retention_days: 7,
            max_entries: 100,
            max_disk_mib: 1024,
            keep_permanently: false,
        }
    }
}

/// 历史占用统计。
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Usage {
    /// 记录条数。
    pub entry_count: i32,
    /// 全部记录的 `total_record_size` 之和。
    pub record_bytes: i64,
    /// `index.json` 字节数。
    pub index_bytes: i64,
    /// 待删除目录的记账字节数。
    pub pending_deletion_bytes: i64,
    /// 以上三项之和。
    pub total_bytes: i64,
}

/// 故障注入点：钩子返回 `true` 时在此处模拟崩溃，磁盘保持原样。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CrashPoint {
    /// 临时记录目录内文件全部写完，尚未改名。
    AfterTempFilesWritten,
    /// 临时目录已改名为记录目录，尚未提交索引。
    AfterRecordRenamed,
    /// 索引已提交（含 pending），尚未删除 pending 目录。
    AfterIndexCommittedWithPending,
    /// 某个 pending 目录已删除，尚未提交“摘除 pending”的索引。
    AfterPayloadRemoved,
}

/// 仓储操作错误。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HistoryError {
    /// 仓储不可写、已禁用或索引不健康。
    Unavailable(String),
    /// 草稿、策略或 ID 不合法。
    Invalid(String),
    /// 文件系统操作失败。
    Io(String),
    /// `revision` 与期望值不一致。
    StaleRevision,
    /// 被故障注入点中断（仅测试使用）。
    Crashed(CrashPoint),
}

impl fmt::Display for HistoryError {
    /// 输出可读的错误文本。
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unavailable(m) | Self::Invalid(m) | Self::Io(m) => f.write_str(m),
            Self::StaleRevision => f.write_str("stale_revision"),
            Self::Crashed(p) => write!(f, "crashed at {p:?}"),
        }
    }
}

impl std::error::Error for HistoryError {}

/// 打开仓储的选项。
#[derive(Clone)]
pub struct Options {
    /// 是否允许写盘；`false` 时所有写操作被拒绝。
    pub write_available: bool,
    /// 初始策略；非法时回退默认值。
    pub policy: CaptureHistoryPolicy,
    /// 时钟，返回 UTC 毫秒时间戳。
    pub clock: Arc<dyn Fn() -> i64 + Send + Sync>,
    /// 故障注入钩子（测试用）：返回 `true` 表示在该点模拟崩溃。
    pub fault_hook: Option<Arc<dyn Fn(CrashPoint) -> bool + Send + Sync>>,
}

impl Default for Options {
    /// 默认：可写、默认策略、系统时钟、无故障注入。
    fn default() -> Self {
        Self {
            write_available: true,
            policy: CaptureHistoryPolicy::default(),
            clock: Arc::new(now_utc_ms),
            fault_hook: None,
        }
    }
}

/// 调用方已编码好的一张图（PNG 字节不透明）。
#[derive(Debug, Clone)]
pub struct DraftImage {
    /// 像素宽。
    pub width: i64,
    /// 像素高。
    pub height: i64,
    /// PNG 编码字节。
    pub png: Vec<u8>,
}

/// 草稿中的单个显示器。
#[derive(Debug, Clone)]
pub struct DraftDisplay {
    /// 显示器截图。
    pub image: DraftImage,
    /// 稳定 ID。
    pub stable_id: String,
    /// 显示器名称。
    pub display_name: String,
    /// 截图在画布上的原点。
    pub source_canvas_origin: Option<Point>,
    /// 截图在画布上的矩形；存在时才写出 backing_scale 等字段。
    pub source_canvas_rect: Option<Rect>,
    /// 缩放比；缺省时按 C++ 规则推导。
    pub backing_scale: Option<f64>,
    /// 原生显示器 ID；缺省按 0。
    pub native_display_id: Option<i64>,
    /// 画布空间，`"points"` 或 `"pixels"`；缺省按 pixels。
    pub canvas_space: Option<String>,
}

/// 待发布的历史草稿。
#[derive(Debug, Clone)]
pub struct HistoryDraft {
    /// 记录 UUID（小写、无花括号）。
    pub id: String,
    /// 创建时间，`yyyy-MM-ddTHH:mm:ss.mmmZ`。
    pub created_utc: String,
    /// 来源，取值见 C++ `sourceText`。
    pub source: String,
    /// 画布边界。
    pub canvas_bounds: Rect,
    /// 选区。
    pub selection: Selection,
    /// `canvas_history.json` 原始字节，按不透明字节写盘。
    pub canvas_history: Vec<u8>,
    /// 显示器截图（1..=32 个）。
    pub displays: Vec<DraftDisplay>,
    /// 结果图。
    pub result: Option<DraftImage>,
    /// 是否为图片型记录（要求恰好 1 个显示器且带结果图）。
    pub content_image: bool,
    /// 滚动截图标记。
    pub scrolling: Option<bool>,
    /// 桌面几何。
    pub desktop_geometry: Option<DesktopGeometry>,
}

/// 内存中的一致快照：只有索引提交成功后才会替换生效。
#[derive(Clone, Default)]
struct Snapshot {
    /// 按创建时间降序排列的记录。
    records: Vec<Record>,
    /// 待删除目录及其记账字节数。
    pending: BTreeMap<String, i64>,
}

/// 截图历史仓储。
///
/// # 示例
/// ```no_run
/// use snow_history::capture_history::{CaptureHistoryRepository, Options};
/// let mut repo = CaptureHistoryRepository::open(std::path::Path::new("D:/data"), Options::default());
/// if repo.needs_maintenance() {
///     let _ = repo.maintenance();
/// }
/// println!("{}", repo.records().len());
/// ```
pub struct CaptureHistoryRepository {
    config_dir: PathBuf,
    root: PathBuf,
    records_root: PathBuf,
    index_path: PathBuf,
    options: Options,
    healthy: bool,
    snapshot: Snapshot,
    root_extra: index::Extra,
    revision: u64,
    usage: Usage,
    last_error: String,
}

/// 临时目录后缀计数器。
static TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);

/// 编码结果：记录本体与待写入的文件（文件名，字节）。
type Encoded = (Record, Vec<(String, Vec<u8>)>);

/// 校验 canvas：非空、不超上限、是 JSON 对象或数组（对照 C++ `validCanvas`）。
fn valid_canvas(bytes: &[u8]) -> bool {
    if bytes.is_empty() || bytes.len() as i64 > index::MAX_CANVAS_BYTES {
        return false;
    }
    let first = bytes
        .iter()
        .find(|b| !matches!(b, b' ' | b'\t' | b'\n' | b'\r'));
    matches!(first, Some(b'{' | b'['))
        && serde_json::from_slice::<serde::de::IgnoredAny>(bytes).is_ok()
}

/// 校验 `#RRGGBB` / `#AARRGGBB` 十六进制颜色。
fn valid_color(text: &str) -> bool {
    text.strip_prefix('#')
        .is_some_and(|hex| matches!(hex.len(), 6 | 8) && hex.bytes().all(|b| b.is_ascii_hexdigit()))
}

/// 记录创建时刻（毫秒）；解析失败按最小值。
fn created_ms(record: &Record) -> i64 {
    parse_iso_utc_ms(&record.created_utc).unwrap_or(i64::MIN)
}

/// 记录整体是否合法：索引层规则 + 选区规范化（对照 `parseRecord`）。
fn record_valid(record: &Record) -> bool {
    index::validate_record(record).is_ok()
        && serde_json::to_value(&record.selection)
            .ok()
            .and_then(|value| snow_config::selection::normalize_persisted_selection(&value))
            .is_some()
        && parse_iso_utc_ms(&record.created_utc).is_some()
}

/// 按创建时刻降序、同刻按 id 升序排序（对照 `sortRecords`）。
fn sort_records(records: &mut [Record]) {
    records.sort_by(|a, b| {
        created_ms(b)
            .cmp(&created_ms(a))
            .then_with(|| a.id.cmp(&b.id))
    });
}

/// 图片像素数记账：越界返回 `None`，否则累加并返回该图像素数（对照 `addImage`）。
fn add_pixels(pixels: &mut i64, image: &DraftImage) -> Option<i64> {
    let count = image.width.checked_mul(image.height)?;
    if image.width < 1
        || image.height < 1
        || count > index::MAX_PIXELS_PER_IMAGE
        || *pixels > index::MAX_PIXELS_PER_RECORD - count
    {
        return None;
    }
    *pixels += count;
    Some(count)
}

impl CaptureHistoryRepository {
    /// 打开仓储并读取索引；不扫描 `records/`、不读任何 payload。
    ///
    /// 目录若位于 upstream 数据位置，则拒绝读写并标记不可用。
    ///
    /// # 参数
    /// - `config_dir`：数据根目录（由 `snow-config` 的路径解析得到）。
    /// - `options`：选项；非法策略回退默认值。
    ///
    /// # 返回
    /// 仓储实例；索引损坏时 `records()` 为空且 `last_error()` 非空。
    pub fn open(config_dir: &Path, options: Options) -> Self {
        let root = config_dir.join(HISTORY_DIR);
        let mut options = options;
        if !options.policy.is_valid() {
            options.policy = CaptureHistoryPolicy::default();
        }
        let mut repo = Self {
            config_dir: config_dir.to_path_buf(),
            records_root: root.join(RECORDS_DIR),
            index_path: root.join(INDEX_FILE),
            root,
            options,
            healthy: true,
            snapshot: Snapshot::default(),
            root_extra: index::Extra::default(),
            revision: 0,
            usage: Usage::default(),
            last_error: String::new(),
        };
        if snow_config::paths::is_upstream_location_resolved(config_dir) {
            repo.healthy = false;
            repo.options.write_available = false;
            repo.last_error = "Refusing to use an upstream data directory".to_string();
            return repo;
        }
        repo.load_index();
        repo
    }

    /// 当前全部记录（创建时间降序）。
    pub fn records(&self) -> Vec<Record> {
        self.snapshot.records.clone()
    }

    /// 记录版本号：仅增删记录时递增，清理 payload 与策略变更不递增。
    pub fn revision(&self) -> u64 {
        self.revision
    }

    /// 占用统计。
    pub fn usage(&self) -> Usage {
        self.usage.clone()
    }

    /// 当前策略。
    pub fn policy(&self) -> CaptureHistoryPolicy {
        self.options.policy.clone()
    }

    /// 最近一次失败原因；成功清空后为空串。
    pub fn last_error(&self) -> String {
        self.last_error.clone()
    }

    /// 启动后是否需要维护（有待删除项或存在过期记录）。
    pub fn needs_maintenance(&self) -> bool {
        self.options.write_available
            && self.healthy
            && (!self.snapshot.pending.is_empty() || self.has_expired())
    }

    /// 维护：只按年龄淘汰（不按容量），并补删 pending 目录。
    ///
    /// # 返回
    /// 成功 `Ok(())`；不可写或清理失败返回错误。
    pub fn maintenance(&mut self) -> Result<(), HistoryError> {
        self.check_writable(false)?;
        let mut next = self.snapshot.clone();
        let count = next.records.len();
        self.prune(&mut next, false, None);
        if next.records.len() != count {
            self.commit(next, true)?;
        }
        self.cleanup()
    }

    /// 发布一条记录：先在临时目录写全，再改名，再提交索引，最后补删被淘汰目录。
    ///
    /// # 参数
    /// - `draft`：草稿，字节内容不会被重新序列化。
    ///
    /// # 返回
    /// 已入索引的记录；索引提交失败时新目录会被回滚删除，内存状态不变。
    pub fn publish(&mut self, draft: HistoryDraft) -> Result<Record, HistoryError> {
        if !self.options.write_available || !self.healthy || !self.options.policy.enabled {
            return Err(HistoryError::Unavailable(
                "Capture-history publication is unavailable".into(),
            ));
        }
        if self.snapshot.pending.contains_key(&draft.id)
            || self.snapshot.records.iter().any(|r| r.id == draft.id)
        {
            return Err(self.fail(HistoryError::Invalid(
                "The capture-history ID already exists".into(),
            )));
        }
        let policy = &self.options.policy;
        let quota = if policy.keep_permanently {
            index::MAX_STORED_BYTES
        } else {
            i64::from(policy.max_disk_mib) * index::MIB
        };
        let Some((record, files)) = Self::encode_draft(&draft, quota) else {
            return Err(self.fail(HistoryError::Invalid(
                "The capture-history draft is invalid or exceeds its quota".into(),
            )));
        };
        if fs::create_dir_all(&self.records_root).is_err()
            || !contained_path(&self.config_dir, &self.records_root)
        {
            return Err(self.fail(HistoryError::Io(
                "Unable to create the capture-history directory".into(),
            )));
        }
        let temp_path = self.records_root.join(format!(
            "{TEMP_PREFIX}{}-{}-{}",
            std::process::id(),
            TEMP_COUNTER.fetch_add(1, Ordering::Relaxed),
            now_utc_ms()
        ));
        let final_path = self.record_path(&record.id);
        if final_path.exists() || fs::create_dir(&temp_path).is_err() {
            return Err(self.fail(HistoryError::Io(
                "Unable to create a temporary history record".into(),
            )));
        }
        let wrote = files
            .iter()
            .all(|(name, bytes)| write_atomic(&temp_path.join(name), bytes).is_ok());
        if wrote {
            self.hit(CrashPoint::AfterTempFilesWritten)?;
        }
        if !wrote || fs::rename(&temp_path, &final_path).is_err() {
            let _ = fs::remove_dir_all(&temp_path);
            return Err(self.fail(HistoryError::Io(
                "Unable to write a capture-history record".into(),
            )));
        }
        self.hit(CrashPoint::AfterRecordRenamed)?;
        let mut next = self.snapshot.clone();
        next.records.push(record.clone());
        sort_records(&mut next.records);
        self.prune(&mut next, true, Some(&record.id));
        if let Err(error) = self.commit(next, true) {
            let _ = fs::remove_dir_all(&final_path);
            return Err(error);
        }
        self.hit(CrashPoint::AfterIndexCommittedWithPending)?;
        // 清理失败不影响发布结果，但故障注入的崩溃必须原样上抛。
        if let Err(crashed @ HistoryError::Crashed(_)) = self.cleanup() {
            return Err(crashed);
        }
        Ok(record)
    }

    /// 批量删除：先摘除索引并记入 pending，再删目录，最后提交索引。
    ///
    /// # 参数
    /// - `ids`：要删除的记录 ID；空或全部未命中时直接成功且不写盘。
    pub fn remove_many(&mut self, ids: &[String]) -> Result<(), HistoryError> {
        if ids.is_empty() {
            return Ok(());
        }
        self.check_writable(false)?;
        let requested: HashSet<&String> = ids.iter().collect();
        let mut next = self.snapshot.clone();
        let mut removed = false;
        next.records.retain(|record| {
            if requested.contains(&record.id) {
                next.pending
                    .insert(record.id.clone(), record.total_record_size);
                removed = true;
                false
            } else {
                true
            }
        });
        if !removed {
            return Ok(());
        }
        self.commit(next, true)?;
        self.cleanup()
    }

    /// 带版本号的删除/清空；`revision` 与期望不符返回 `StaleRevision`。
    ///
    /// # 参数
    /// - `ids`：要删除的 ID（`clear` 为 `true` 时忽略）。
    /// - `expected`：调用方看到的 `revision`。
    /// - `clear`：为 `true` 时清空全部历史。
    pub fn remove_if_revision(
        &mut self,
        ids: &[String],
        expected: u64,
        clear: bool,
    ) -> Result<(), HistoryError> {
        self.check_writable(clear)?;
        if self.revision != expected {
            return Err(HistoryError::StaleRevision);
        }
        if clear {
            self.clear()
        } else {
            self.remove_many(ids)
        }
    }

    /// 更新策略；启用状态、永久保留或保留天数变化时先做一次维护。
    ///
    /// # 参数
    /// - `policy`：新策略，必须满足 `is_valid()`。
    pub fn update_policy(&mut self, policy: CaptureHistoryPolicy) -> Result<(), HistoryError> {
        if !policy.is_valid() {
            return Err(HistoryError::Invalid(
                "The capture-history policy is invalid".into(),
            ));
        }
        self.check_writable(false)?;
        let previous = std::mem::replace(&mut self.options.policy, policy.clone());
        if policy.enabled
            && (!previous.enabled
                || previous.keep_permanently != policy.keep_permanently
                || previous.retention_days != policy.retention_days)
        {
            self.maintenance()
        } else {
            self.cleanup()
        }
    }

    /// 清空整个历史目录（含未受管的残留），并重置损坏的索引。
    pub fn clear(&mut self) -> Result<(), HistoryError> {
        self.check_writable(true)?;
        if self.root.exists()
            && (!contained_path(&self.config_dir, &self.root)
                || fs::remove_dir_all(&self.root).is_err())
        {
            return Err(self.fail(HistoryError::Io(
                "Unable to clear all managed capture-history data".into(),
            )));
        }
        let had_records = !self.snapshot.records.is_empty();
        self.root_extra = index::Extra::default();
        self.commit(Snapshot::default(), had_records)?;
        self.healthy = true;
        self.last_error.clear();
        Ok(())
    }

    /// 读取记录的 `canvas_history.json` 原始字节。
    ///
    /// 校验路径包含、长度等于 `canvas_byte_size`、内容是 JSON 对象/数组；
    /// 失败时上报读盘失败并移除该记录。
    pub fn load_canvas(&mut self, record: &Record) -> Option<Vec<u8>> {
        let path = self
            .record_path(&record.id)
            .join(&record.canvas_history_file);
        let bytes = if contained_path(&self.root, &path) {
            fs::File::open(&path).ok().and_then(|file| {
                let mut buffer = Vec::new();
                let limit = (index::MAX_CANVAS_BYTES + 1) as u64;
                file.take(limit)
                    .read_to_end(&mut buffer)
                    .ok()
                    .map(|_| buffer)
            })
        } else {
            None
        };
        match bytes {
            Some(bytes)
                if bytes.len() as i64 == record.canvas_byte_size && valid_canvas(&bytes) =>
            {
                Some(bytes)
            }
            _ => {
                self.report_read_failure(record, READ_FAILED);
                None
            }
        }
    }

    /// 读取记录内登记的图片文件（结果图或显示器图）的原始字节。
    ///
    /// # 参数
    /// - `record`：记录。
    /// - `file_name`：`record.result` 或 `record.displays` 中登记的 `image_file`。
    ///
    /// 先比对文件大小等于 `encoded_bytes` 再读取；失败时上报并移除记录。
    pub fn read_image_file(&mut self, record: &Record, file_name: &str) -> Option<Vec<u8>> {
        let expected = record
            .result
            .iter()
            .map(|r| (&r.image_file, r.encoded_bytes))
            .chain(
                record
                    .displays
                    .iter()
                    .map(|d| (&d.image_file, d.encoded_bytes)),
            )
            .find(|(name, _)| name.as_str() == file_name)
            .map(|(_, bytes)| bytes);
        let path = self.record_path(&record.id).join(file_name);
        let bytes = expected
            .filter(|_| contained_path(&self.root, &path))
            .and_then(|size| {
                let on_disk = fs::metadata(&path).ok()?.len();
                (i64::try_from(on_disk).ok()? == size).then_some(())?;
                fs::read(&path).ok()
            });
        if bytes.is_none() {
            self.report_read_failure(record, READ_FAILED);
        }
        bytes
    }

    /// 上报读盘失败：记录仍在且与当前完全一致时，记下原因并移除该记录。
    pub fn report_read_failure(&mut self, record: &Record, reason: &str) {
        if self.snapshot.records.iter().any(|r| r == record) {
            self.last_error = reason.to_string();
            let _ = self.remove_many(std::slice::from_ref(&record.id));
        }
    }

    /// 记录目录路径 `records/<id>`。
    fn record_path(&self, id: &str) -> PathBuf {
        self.records_root.join(id)
    }

    /// 触发故障注入点；钩子要求崩溃时返回 `Crashed`。
    fn hit(&self, point: CrashPoint) -> Result<(), HistoryError> {
        match &self.options.fault_hook {
            Some(hook) if hook(point) => Err(HistoryError::Crashed(point)),
            _ => Ok(()),
        }
    }

    /// 记录失败原因并原样返回错误。
    fn fail(&mut self, error: HistoryError) -> HistoryError {
        self.last_error = error.to_string();
        error
    }

    /// 写操作准入：不可写一律拒绝；索引不健康时只放行 clear。
    fn check_writable(&self, is_clear: bool) -> Result<(), HistoryError> {
        if !self.options.write_available || (!self.healthy && !is_clear) {
            return Err(HistoryError::Unavailable(
                "Capture-history storage is not writable".into(),
            ));
        }
        Ok(())
    }

    /// 读取并校验 `index.json`；任何一处不合法即整体作废（对照 `loadIndex`）。
    fn load_index(&mut self) {
        let bytes = match fs::read(&self.index_path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return,
            Err(_) => return self.index_failed(),
        };
        let Ok(parsed) = index::load_index(&bytes) else {
            return self.index_failed();
        };
        if index::validate_index(&parsed).is_err() || !parsed.records.iter().all(record_valid) {
            return self.index_failed();
        }
        let mut next = Snapshot {
            records: parsed.records,
            pending: parsed
                .pending_deletions
                .into_iter()
                .map(|p| (p.id, p.bytes))
                .collect(),
        };
        sort_records(&mut next.records);
        let has_records = !next.records.is_empty();
        self.root_extra = parsed.extra;
        self.install(next, bytes.len() as i64, has_records);
    }

    /// 标记索引损坏：保留全部文件，等待用户显式清空。
    fn index_failed(&mut self) {
        self.healthy = false;
        self.last_error =
            "Unable to read the capture-history index; clear history to reset it".to_string();
    }

    /// 装入已提交的快照并重算占用；仅记录变化时递增 revision。
    fn install(&mut self, next: Snapshot, index_bytes: i64, records_changed: bool) {
        let record_bytes: i64 = next.records.iter().map(|r| r.total_record_size).sum();
        let pending_bytes: i64 = next.pending.values().sum();
        self.usage = Usage {
            entry_count: i32::try_from(next.records.len()).unwrap_or(i32::MAX),
            record_bytes,
            index_bytes,
            pending_deletion_bytes: pending_bytes,
            total_bytes: record_bytes + index_bytes + pending_bytes,
        };
        if records_changed {
            self.revision += 1;
        }
        self.snapshot = next;
    }

    /// 原子提交索引，成功后才让快照生效；失败不改动任何内存状态。
    fn commit(&mut self, next: Snapshot, records_changed: bool) -> Result<(), HistoryError> {
        let document = HistoryIndex {
            format_version: index::INDEX_VERSION,
            records: next.records.clone(),
            pending_deletions: next
                .pending
                .iter()
                .map(|(id, bytes)| PendingDeletion {
                    id: id.clone(),
                    bytes: *bytes,
                    extra: index::Extra::default(),
                })
                .collect(),
            extra: self.root_extra.clone(),
        };
        let written = index::save_index(&document).ok().filter(|bytes| {
            fs::create_dir_all(&self.root).is_ok()
                && contained_path(&self.config_dir, &self.root)
                && write_atomic(&self.index_path, bytes).is_ok()
        });
        let Some(bytes) = written else {
            return Err(self.fail(HistoryError::Io(
                "Unable to commit the capture-history index".into(),
            )));
        };
        self.install(next, bytes.len() as i64, records_changed);
        Ok(())
    }

    /// 是否存在早于保留期的记录。
    fn has_expired(&self) -> bool {
        let policy = &self.options.policy;
        if !policy.enabled || policy.keep_permanently {
            return false;
        }
        let cutoff = (self.options.clock)() - i64::from(policy.retention_days) * MILLIS_PER_DAY;
        self.snapshot.records.iter().any(|r| created_ms(r) < cutoff)
    }

    /// 从最旧一端淘汰：过期的一律淘汰，`capacity` 为真时再按条数与体积淘汰。
    fn prune(&self, next: &mut Snapshot, capacity: bool, protected: Option<&str>) {
        let policy = &self.options.policy;
        if !policy.enabled || policy.keep_permanently {
            return;
        }
        let cutoff = (self.options.clock)() - i64::from(policy.retention_days) * MILLIS_PER_DAY;
        let mut bytes: i64 = next.records.iter().map(|r| r.total_record_size).sum();
        let mut count = next.records.len();
        let mut victims: HashSet<String> = HashSet::new();
        for record in next.records.iter().rev() {
            if Some(record.id.as_str()) == protected {
                continue;
            }
            let over_capacity = capacity
                && (count > policy.max_entries as usize
                    || bytes > i64::from(policy.max_disk_mib) * index::MIB);
            if created_ms(record) < cutoff || over_capacity {
                victims.insert(record.id.clone());
                next.pending
                    .insert(record.id.clone(), record.total_record_size);
                bytes -= record.total_record_size;
                count -= 1;
            }
        }
        next.records.retain(|r| !victims.contains(&r.id));
    }

    /// 补删 pending 目录：先删目录，再提交“摘除 pending”的索引（对照 `cleanup`）。
    fn cleanup(&mut self) -> Result<(), HistoryError> {
        let mut next = self.snapshot.clone();
        let mut removed = false;
        let mut success = true;
        for id in self.snapshot.pending.keys() {
            let path = self.record_path(id);
            let is_link = fs::symlink_metadata(&path).is_ok_and(|m| m.file_type().is_symlink());
            let gone = (!path.exists() && !is_link)
                || (contained_path(&self.root, &path) && fs::remove_dir_all(&path).is_ok());
            if gone {
                next.pending.remove(id);
                removed = true;
                self.hit(CrashPoint::AfterPayloadRemoved)?;
            } else {
                success = false;
            }
        }
        if removed {
            self.commit(next, false)?;
        }
        if success {
            Ok(())
        } else {
            Err(self.fail(HistoryError::Io(
                "Unable to delete some capture-history payloads".into(),
            )))
        }
    }

    /// 把草稿编码为记录与待写文件；任一校验失败返回 `None`（对照 `encodeDraft`）。
    fn encode_draft(draft: &HistoryDraft, quota: i64) -> Option<Encoded> {
        if draft.content_image && (draft.displays.len() != 1 || draft.result.is_none()) {
            return None;
        }
        let bounds = &draft.canvas_bounds;
        let rectangle = &draft.selection.rectangle;
        let selection = &draft.selection;
        let created = parse_iso_utc_ms(&draft.created_utc);
        if !index::is_valid_uuid(&draft.id)
            || created.is_none()
            || bounds.width < 1
            || bounds.height < 1
            || rectangle.width < 1
            || rectangle.height < 1
            || !valid_color(&selection.shadow_color)
            || !(0..=256).contains(&selection.corner_radius)
            || !(0..=64).contains(&selection.shadow_width)
            || !valid_canvas(&draft.canvas_history)
            || draft.displays.is_empty()
            || draft.displays.len() > index::MAX_DISPLAYS
            || !SOURCES.contains(&draft.source.as_str())
        {
            return None;
        }
        let canvas_bytes = draft.canvas_history.len() as i64;
        let mut record = Record {
            id: draft.id.clone(),
            created_utc: draft.created_utc.clone(),
            source: draft.source.clone(),
            canvas_bounds: bounds.clone(),
            selection: draft.selection.clone(),
            canvas_history_file: index::CANVAS_FILE.to_string(),
            canvas_byte_size: canvas_bytes,
            total_record_size: canvas_bytes,
            displays: Vec::new(),
            result: None,
            content_kind: draft.content_image.then(|| "image".to_string()),
            scrolling: draft.scrolling,
            desktop_geometry: draft.desktop_geometry.clone(),
            extra: index::Extra::default(),
        };
        let mut files = vec![(index::CANVAS_FILE.to_string(), draft.canvas_history.clone())];
        let mut pixels = 0_i64;
        // 逐张记账：像素上限、非空、体积配额，通过后登记文件并返回字节数。
        let mut add_image = |record: &mut Record, image: &DraftImage, name: &str| -> Option<i64> {
            add_pixels(&mut pixels, image)?;
            let length = i64::try_from(image.png.len()).ok()?;
            if length == 0 || record.total_record_size > quota - length {
                return None;
            }
            record.total_record_size += length;
            files.push((name.to_string(), image.png.clone()));
            Some(length)
        };
        if let Some(result) = &draft.result {
            let length = add_image(&mut record, result, index::RESULT_FILE)?;
            record.result = Some(ResultImage {
                image_file: index::RESULT_FILE.to_string(),
                width: result.width,
                height: result.height,
                encoded_bytes: length,
                extra: index::Extra::default(),
            });
        }
        for (i, display) in draft.displays.iter().enumerate() {
            let image = &display.image;
            let uses_points = display.canvas_space.as_deref() == Some("points");
            if let Some(origin) = &display.source_canvas_origin
                && (i128::from(origin.x) + i128::from(image.width) - 1 > i128::from(index::INT_MAX)
                    || i128::from(origin.y) + i128::from(image.height) - 1
                        > i128::from(index::INT_MAX))
            {
                return None;
            }
            if uses_points && display.source_canvas_rect.is_none() {
                return None;
            }
            let mut backing_scale = None;
            if let Some(rect) = &display.source_canvas_rect {
                if rect.width < 1
                    || rect.height < 1
                    || i128::from(rect.x) + i128::from(rect.width) > i128::from(index::INT_MAX)
                    || i128::from(rect.y) + i128::from(rect.height) > i128::from(index::INT_MAX)
                {
                    return None;
                }
                let scale = match display.backing_scale {
                    Some(scale) if scale > 0.0 => scale,
                    _ if uses_points => (image.width as f64 / rect.width as f64)
                        .max(image.height as f64 / rect.height as f64),
                    _ => 1.0,
                };
                if !scale.is_finite() || scale <= 0.0 {
                    return None;
                }
                backing_scale = Some(scale);
            }
            let name = format!("display_{i}.png");
            let length = add_image(&mut record, image, &name)?;
            let has_rect = display.source_canvas_rect.is_some();
            record.displays.push(Display {
                image_file: name,
                width: image.width,
                height: image.height,
                encoded_bytes: length,
                stable_id: display.stable_id.clone(),
                display_name: display.display_name.clone(),
                source_canvas_origin: display.source_canvas_origin.clone(),
                source_canvas_rect: display.source_canvas_rect.clone(),
                backing_scale,
                native_display_id: has_rect.then(|| display.native_display_id.unwrap_or(0)),
                canvas_space: has_rect
                    .then(|| if uses_points { "points" } else { "pixels" }.to_string()),
                extra: index::Extra::default(),
            });
        }
        (record.total_record_size <= quota && record_valid(&record)).then_some((record, files))
    }
}
