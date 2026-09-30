//! 贴图仓储（`pinned_windows_v2/`）容器层。
//!
//! 对照 C++ `pinnedwindowrepository.cpp` 的容器部分：`index.json` 清单（`format_version`
//! 硬锁 2，不匹配即整体丢弃并留档）、`pins/<id>/` 下的 payload 文件（`canvas_session.bin`
//! 等一律按不透明字节）、内存 hash 变更检测、清理不再引用的文件与目录。
//! 记录里的窗口几何、边框、DPI 等业务字段属于上层领域模型，这里只当不透明 JSON 保存。

use std::collections::hash_map::DefaultHasher;
use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::fs;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde::Serialize;
use serde_json::{Map, Value, json};

use crate::fsutil::{contained_path, safe_file_name, safe_user_file_name, write_atomic};
use crate::index::is_valid_uuid;
use crate::timeutil::{format_iso_utc_ms, now_utc_ms};

/// 清单格式版本（硬锁，不兼容即整体丢弃）。
pub const FORMAT_VERSION: i64 = 2;
/// 仓储目录名。
pub const DIRECTORY_NAME: &str = "pinned_windows_v2";
/// 清单文件名。
pub const MANIFEST_NAME: &str = "index.json";
/// 默认分组 ID。
pub const DEFAULT_GROUP_ID: &str = "default";
/// 默认分组名。
pub const DEFAULT_GROUP_NAME: &str = "Default";
/// 分组数量上限（含默认分组）。
pub const MAX_GROUPS: usize = 128;
/// 分组名最大长度（UTF-16 单元，同 `QString::size`）。
pub const MAX_GROUP_NAME_UNITS: usize = 16;
/// 单张源图上限：256 MiB。
pub const MAX_IMAGE_BYTES: usize = 256 * 1024 * 1024;
/// 单个 payload（含 `canvas_session.bin`）上限：32 MiB。
pub const MAX_PAYLOAD_BYTES: usize = 32 * 1024 * 1024;
/// 强调色索引上限（不含）。
const ACCENT_COUNT: i64 = 13;
/// 描述符中表示目录的键。
const KEY_DIRECTORY: &str = "directory";
/// 描述符中表示图片的键。
const KEY_IMAGE: &str = "image";
/// `ImageData` 源图的固定文件名。
const SOURCE_IMAGE_FILE: &str = "source.png";

/// 贴图来源类型。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SourceKind {
    /// 内存图像数据，落盘为 `source.png`。
    ImageData,
    /// 剪贴板文本，无图片文件。
    ClipboardText,
    /// 剪贴板图片文件，沿用原文件名。
    ClipboardImageFile,
}

impl SourceKind {
    /// 清单里的文本表示。
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ImageData => "image_data",
            Self::ClipboardText => "clipboard_text",
            Self::ClipboardImageFile => "clipboard_image_file",
        }
    }

    /// 从清单文本解析；未知取值返回 `None`。
    pub fn parse(text: &str) -> Option<Self> {
        match text {
            "image_data" => Some(Self::ImageData),
            "clipboard_text" => Some(Self::ClipboardText),
            "clipboard_image_file" => Some(Self::ClipboardImageFile),
            _ => None,
        }
    }
}

/// 贴图分组。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PinGroup {
    /// 分组 ID：`default` 或小写 UUID。
    pub id: String,
    /// 分组名。
    pub name: String,
    /// 是否内置（仅默认分组）。
    pub built_in: bool,
}

/// 贴图源图文件。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PinImage {
    /// 文件名；`ImageData` 固定为 `source.png`，`ClipboardImageFile` 为原文件名。
    pub file_name: String,
    /// 文件字节。
    pub bytes: Vec<u8>,
}

/// 一条贴图的全部 payload，字节内容对仓储完全不透明。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PinPayload {
    /// 源图；文本贴图为空。
    pub image: Option<PinImage>,
    /// 原始 HTML；空串表示没有。
    pub original_html: String,
    /// 原始纯文本；空串表示没有。
    pub original_text: String,
    /// 结果样式字节；空表示没有。
    pub result_style: Vec<u8>,
    /// 引擎会话字节（`canvas_session.bin`）；空表示没有。
    pub canvas_session: Vec<u8>,
    /// 识别结果字节；空表示没有。
    pub recognition_results: Vec<u8>,
}

/// 贴图仓储错误。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PinError {
    /// 仓储不可写。
    Unavailable(String),
    /// 输入不合法（ID、分组、文件名、体积上限等）。
    Invalid(String),
    /// 文件系统操作失败。
    Io(String),
    /// 被故障注入点中断（仅测试使用）。
    Crashed(PinCrashPoint),
}

impl fmt::Display for PinError {
    /// 输出可读的错误文本。
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unavailable(m) | Self::Invalid(m) | Self::Io(m) => f.write_str(m),
            Self::Crashed(p) => write!(f, "crashed at {p:?}"),
        }
    }
}

impl std::error::Error for PinError {}

/// 贴图仓储的故障注入点：钩子返回 `true` 时在此处模拟崩溃。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PinCrashPoint {
    /// payload 文件全部写完，清单尚未提交。
    AfterPayloadsWritten,
    /// 清单已提交，废弃文件与目录尚未清理。
    AfterManifestCommitted,
}

/// 打开贴图仓储的选项。
#[derive(Clone)]
pub struct PinOptions {
    /// 是否允许写盘。
    pub write_available: bool,
    /// 时钟（UTC 毫秒），仅用于损坏留档的文件名。
    pub clock: Arc<dyn Fn() -> i64 + Send + Sync>,
    /// 故障注入钩子（测试用）。
    pub fault_hook: Option<Arc<dyn Fn(PinCrashPoint) -> bool + Send + Sync>>,
}

impl Default for PinOptions {
    /// 默认：可写、系统时钟、无故障注入。
    fn default() -> Self {
        Self {
            write_available: true,
            clock: Arc::new(now_utc_ms),
            fault_hook: None,
        }
    }
}

/// payload 指纹：仅存内存，用来判断是否需要重写文件。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
struct Signature {
    kind_tag: u8,
    image: u64,
    file_name: u64,
    html: u64,
    text: u64,
    result_style: u64,
    canvas_session: u64,
    recognition: u64,
}

/// 对字节取哈希；空输入固定为 0（对照 `payloadHash`）。
fn hash_bytes(bytes: &[u8]) -> u64 {
    if bytes.is_empty() {
        return 0;
    }
    let mut hasher = DefaultHasher::new();
    bytes.hash(&mut hasher);
    hasher.finish()
}

/// 计算 payload 指纹。
fn signature(kind: SourceKind, payload: &PinPayload) -> Signature {
    let image = payload.image.as_ref();
    Signature {
        kind_tag: kind as u8 + 1,
        image: image.map_or(0, |i| hash_bytes(&i.bytes)),
        file_name: image.map_or(0, |i| hash_bytes(i.file_name.as_bytes())),
        html: hash_bytes(payload.original_html.as_bytes()),
        text: hash_bytes(payload.original_text.as_bytes()),
        result_style: hash_bytes(&payload.result_style),
        canvas_session: hash_bytes(&payload.canvas_session),
        recognition: hash_bytes(&payload.recognition_results),
    }
}

/// 一条已入库的贴图。
#[derive(Clone)]
struct Stored {
    /// 清单里的记录对象（`payloads` 字段以 `descriptor` 为准）。
    record: Map<String, Value>,
    kind: SourceKind,
    /// 从磁盘重载 payload 的描述符（`payloads` 对象）。
    descriptor: Map<String, Value>,
    signature: Signature,
    /// 尚未落盘的常驻 payload；已提交后释放。
    resident: Option<PinPayload>,
    payload_revision: u64,
}

/// 贴图仓储。
///
/// # 示例
/// ```no_run
/// use snow_history::pinned::{PinnedStore, PinOptions};
/// let mut store = PinnedStore::open(std::path::Path::new("D:/data"), PinOptions::default());
/// store.flush().unwrap();
/// println!("{}", store.record_ids().len());
/// ```
pub struct PinnedStore {
    root: PathBuf,
    options: PinOptions,
    records: BTreeMap<String, Stored>,
    groups: Vec<PinGroup>,
    active_group_id: String,
    next_accent: i64,
    next_preview_revision: u64,
    committed: BTreeMap<String, u64>,
    next_payload_revision: u64,
    dirty: bool,
    error: String,
    skipped: usize,
    payload_writes: u64,
}

/// 默认分组。
fn default_group() -> PinGroup {
    PinGroup {
        id: DEFAULT_GROUP_ID.to_string(),
        name: DEFAULT_GROUP_NAME.to_string(),
        built_in: true,
    }
}

/// 分组 ID 是否合法：`default` 或小写 UUID。
fn safe_group_id(id: &str) -> bool {
    id == DEFAULT_GROUP_ID || is_valid_uuid(id)
}

/// 名称是否已被占用（去空白、忽略大小写）。
fn group_name_in_use(groups: &[PinGroup], name: &str) -> bool {
    groups
        .iter()
        .any(|g| g.name.trim().to_lowercase() == name.to_lowercase())
}

/// 校验并追加一个自定义分组；不合法时静默跳过（对照构造函数里的分组解析）。
fn try_push_group(groups: &mut Vec<PinGroup>, id: &str, name: &str) {
    let (id, name) = (id.trim(), name.trim());
    if groups.len() >= MAX_GROUPS
        || !safe_group_id(id)
        || id == DEFAULT_GROUP_ID
        || name.is_empty()
        || name.encode_utf16().count() > MAX_GROUP_NAME_UNITS
        || group_name_in_use(groups, name)
        || groups.iter().any(|g| g.id == id)
    {
        return;
    }
    groups.push(PinGroup {
        id: id.to_string(),
        name: name.to_string(),
        built_in: false,
    });
}

/// 把 JSON 数字读成整数；接受 `2` 与 `2.0`（对照 `QJsonValue::toInt`）。
fn json_int(value: &Value) -> Option<i64> {
    value.as_i64().or_else(|| {
        value
            .as_f64()
            .filter(|f| f.fract() == 0.0 && f.abs() < 1e15)
            .map(|f| f as i64)
    })
}

/// 由 payload 生成描述符（对照 `payloadsToJson`）。
fn descriptor_for(id: &str, payload: &PinPayload) -> Map<String, Value> {
    let mut map = Map::new();
    map.insert(KEY_DIRECTORY.into(), json!(id));
    if let Some(image) = &payload.image {
        map.insert(KEY_IMAGE.into(), json!(image.file_name));
    }
    let entries = [
        ("html", !payload.original_html.is_empty(), "original.html"),
        ("text", !payload.original_text.is_empty(), "original.txt"),
        (
            "result_style",
            !payload.result_style.is_empty(),
            "result_style.bin",
        ),
        (
            "canvas_session",
            !payload.canvas_session.is_empty(),
            "canvas_session.bin",
        ),
        (
            "recognition_results",
            !payload.recognition_results.is_empty(),
            "recognition_results.bin",
        ),
    ];
    for (key, present, file) in entries {
        if present {
            map.insert(key.into(), json!(file));
        }
    }
    map
}

/// 校验描述符：目录名等于 id、各文件名安全、图片项存在（对照 `validatePayloads`）。
fn validate_descriptor(
    descriptor: &Map<String, Value>,
    dir: &Path,
    id: &str,
    kind: SourceKind,
) -> bool {
    if descriptor.get(KEY_DIRECTORY).and_then(Value::as_str) != Some(id) {
        return false;
    }
    for (key, value) in descriptor {
        if key == KEY_DIRECTORY {
            continue;
        }
        let Some(name) = value.as_str() else {
            return false;
        };
        if !safe_file_name(name)
            || (key == KEY_IMAGE && !safe_user_file_name(name))
            || (key == KEY_IMAGE && !dir.join(name).is_file())
        {
            return false;
        }
    }
    kind == SourceKind::ClipboardText || descriptor.get(KEY_IMAGE).is_some_and(Value::is_string)
}

/// 读取单个 payload 文件：缺文件读作空；超限或长度不符视为失败（对照 `readBlob`）。
fn read_blob(root: &Path, path: &Path, limit: usize) -> Result<Vec<u8>, PinError> {
    if !path.exists() {
        return Ok(Vec::new());
    }
    let fail = || PinError::Io(format!("Unable to read payload {}", path.display()));
    if !contained_path(root, path) {
        return Err(fail());
    }
    let size = fs::metadata(path).map_err(|_| fail())?.len();
    if size > limit as u64 {
        return Err(fail());
    }
    let bytes = fs::read(path).map_err(|_| fail())?;
    if bytes.len() as u64 == size {
        Ok(bytes)
    } else {
        Err(fail())
    }
}

/// 缩进 4 空格的 JSON 文本并补结尾换行（对照 `jsonBytes` 的 Indented 风格）。
fn pretty_bytes(value: &Value) -> Vec<u8> {
    let mut out = Vec::new();
    let formatter = serde_json::ser::PrettyFormatter::with_indent(b"    ");
    let mut serializer = serde_json::Serializer::with_formatter(&mut out, formatter);
    if value.serialize(&mut serializer).is_err() {
        return Vec::new();
    }
    out.push(b'\n');
    out
}

impl PinnedStore {
    /// 打开仓储并读取清单；不读取任何 payload。
    ///
    /// 清单损坏或版本不是 2 时：整体丢弃、记录错误，并把原文件复制为
    /// `index.json.corrupt.<UTC时间戳>` 留档。位于 upstream 数据目录时拒绝读写。
    ///
    /// # 参数
    /// - `config_dir`：数据根目录。
    /// - `options`：选项。
    pub fn open(config_dir: &Path, options: PinOptions) -> Self {
        let mut store = Self {
            root: config_dir.join(DIRECTORY_NAME),
            options,
            records: BTreeMap::new(),
            groups: vec![default_group()],
            active_group_id: DEFAULT_GROUP_ID.to_string(),
            next_accent: 0,
            next_preview_revision: 1,
            committed: BTreeMap::new(),
            next_payload_revision: 2,
            dirty: false,
            error: String::new(),
            skipped: 0,
            payload_writes: 0,
        };
        if snow_config::paths::is_upstream_location_resolved(config_dir) {
            store.options.write_available = false;
            store.error = "Refusing to use an upstream data directory".to_string();
            return store;
        }
        store.load_manifest();
        store
    }

    /// 全部记录 ID（升序）。
    pub fn record_ids(&self) -> Vec<String> {
        self.records.keys().cloned().collect()
    }

    /// 取记录的清单对象（`payloads` 为当前描述符）。
    pub fn record(&self, id: &str) -> Option<Map<String, Value>> {
        let stored = self.records.get(id)?;
        let mut record = stored.record.clone();
        record.insert("payloads".into(), Value::Object(stored.descriptor.clone()));
        Some(record)
    }

    /// 分组列表（默认分组在最前）。
    pub fn groups(&self) -> Vec<PinGroup> {
        self.groups.clone()
    }

    /// 当前激活的分组 ID。
    pub fn active_group_id(&self) -> String {
        self.active_group_id.clone()
    }

    /// 下一个“收起到顶部”强调色索引。
    pub fn next_hide_to_top_accent(&self) -> i64 {
        self.next_accent
    }

    /// 最近一次失败原因。
    pub fn last_error(&self) -> String {
        self.error.clone()
    }

    /// 打开时因不合法而被丢弃的记录数（诊断用）。
    pub fn skipped_records(&self) -> usize {
        self.skipped
    }

    /// 累计写过 payload 文件的记录次数（诊断用，用来验证变更检测）。
    pub fn payload_writes(&self) -> u64 {
        self.payload_writes
    }

    /// 是否存在尚未落盘的修改。
    pub fn is_dirty(&self) -> bool {
        self.dirty
    }

    /// 某记录的 `preview_source_revision`；记录不存在返回 `None`。
    pub fn preview_source_revision(&self, id: &str) -> Option<u64> {
        let text = self
            .records
            .get(id)?
            .record
            .get("preview_source_revision")?
            .as_str()?;
        text.parse().ok()
    }

    /// 新增或更新一条记录。
    ///
    /// `payload` 与已存指纹一致（或传 `None`）时不会重写 payload 文件，只更新清单。
    /// 新记录必须带 payload。任一 payload 超过 32 MiB（源图 256 MiB）会被拒绝。
    ///
    /// # 参数
    /// - `record`：记录的清单对象，需含 `id`、`group_id`、`source_kind`；`payloads` 字段会被忽略。
    /// - `payload`：完整 payload，或 `None` 表示沿用已存 payload。
    pub fn upsert(
        &mut self,
        record: Map<String, Value>,
        payload: Option<PinPayload>,
    ) -> Result<(), PinError> {
        self.check_writable()?;
        let invalid = |m: &str| PinError::Invalid(m.to_string());
        let id = record
            .get("id")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        let group = record
            .get("group_id")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let kind = record
            .get("source_kind")
            .and_then(Value::as_str)
            .and_then(SourceKind::parse);
        if !is_valid_uuid(&id) {
            return Err(invalid("invalid pin id"));
        }
        if !self.groups.iter().any(|g| g.id == group) {
            return Err(invalid("unknown pin group"));
        }
        let kind = kind.ok_or_else(|| invalid("unknown source kind"))?;
        let existing = self.records.get(&id);
        if payload.is_none() && existing.is_none() {
            return Err(invalid("a new pin requires a payload"));
        }
        if let Some(payload) = &payload {
            Self::check_payload(kind, payload)?;
        }
        let mut stored = match existing {
            Some(old) => old.clone(),
            None => Stored {
                record: Map::new(),
                kind,
                descriptor: Map::new(),
                signature: Signature::default(),
                resident: None,
                payload_revision: 0,
            },
        };
        let mut preview_changed = existing.is_none();
        if let Some(payload) = payload {
            let next_signature = signature(kind, &payload);
            if existing.is_none() || stored.signature != next_signature {
                let old = stored.signature;
                preview_changed = preview_changed
                    || (old.image, old.file_name, old.html, old.text, old.kind_tag)
                        != (
                            next_signature.image,
                            next_signature.file_name,
                            next_signature.html,
                            next_signature.text,
                            next_signature.kind_tag,
                        );
                stored.descriptor = descriptor_for(&id, &payload);
                stored.signature = next_signature;
                stored.resident = Some(payload);
                stored.payload_revision = self.next_payload_revision;
                self.next_payload_revision += 1;
            }
        }
        stored.kind = kind;
        let mut manifest = record;
        manifest.remove("payloads");
        let previous_revision = stored.record.get("preview_source_revision").cloned();
        stored.record = manifest;
        let revision = if preview_changed || previous_revision.is_none() {
            self.next_preview_revision += 1;
            json!(self.next_preview_revision.to_string())
        } else {
            previous_revision.unwrap_or_default()
        };
        stored
            .record
            .insert("preview_source_revision".into(), revision);
        self.records.insert(id, stored);
        self.dirty = true;
        Ok(())
    }

    /// 移除记录；其 payload 目录在下一次 `flush` 时清理。
    ///
    /// # 返回
    /// 记录存在并已移除返回 `true`。
    pub fn remove(&mut self, id: &str) -> bool {
        let removed = self.records.remove(id).is_some();
        self.dirty |= removed;
        removed
    }

    /// 设置分组与激活分组，规则同打开时的清单解析；不合法的分组被忽略。
    ///
    /// # 参数
    /// - `groups`：自定义分组（默认分组自动保留在最前，传入里的 `default` 会被忽略）。
    /// - `active`：激活分组 ID，不存在时保持默认分组。
    pub fn set_groups(&mut self, groups: &[PinGroup], active: &str) {
        let mut next = vec![default_group()];
        for group in groups {
            try_push_group(&mut next, &group.id, &group.name);
        }
        self.active_group_id = if next.iter().any(|g| g.id == active) {
            active.into()
        } else {
            DEFAULT_GROUP_ID.into()
        };
        self.groups = next;
        self.dirty = true;
    }

    /// 读取一条记录的完整 payload：未落盘的取常驻副本，否则从磁盘校验读取。
    ///
    /// 缺失的可选 payload 读作空；源图缺失、超限或长度不符则失败。
    pub fn load_payload(&self, id: &str) -> Result<Option<PinPayload>, PinError> {
        let Some(stored) = self.records.get(id) else {
            return Ok(None);
        };
        if let Some(resident) = &stored.resident {
            return Ok(Some(resident.clone()));
        }
        let dir = self.pin_dir(id);
        let name_of = |key: &str| stored.descriptor.get(key).and_then(Value::as_str);
        let mut payload = PinPayload::default();
        if stored.kind != SourceKind::ClipboardText {
            let name = name_of(KEY_IMAGE)
                .filter(|n| safe_user_file_name(n))
                .ok_or_else(|| PinError::Invalid("missing image payload".into()))?;
            let bytes = read_blob(&self.root, &dir.join(name), MAX_IMAGE_BYTES)?;
            if bytes.is_empty() {
                return Err(PinError::Io("Unable to read the pin image".into()));
            }
            payload.image = Some(PinImage {
                file_name: name.to_string(),
                bytes,
            });
        }
        let text = |key: &str| -> Result<String, PinError> {
            match name_of(key) {
                Some(n) if safe_file_name(n) => Ok(String::from_utf8_lossy(&read_blob(
                    &self.root,
                    &dir.join(n),
                    MAX_PAYLOAD_BYTES,
                )?)
                .into_owned()),
                _ => Ok(String::new()),
            }
        };
        let blob = |key: &str| -> Result<Vec<u8>, PinError> {
            match name_of(key) {
                Some(n) if safe_file_name(n) => {
                    read_blob(&self.root, &dir.join(n), MAX_PAYLOAD_BYTES)
                }
                _ => Ok(Vec::new()),
            }
        };
        payload.original_html = text("html")?;
        payload.original_text = text("text")?;
        payload.result_style = blob("result_style")?;
        payload.canvas_session = blob("canvas_session")?;
        payload.recognition_results = blob("recognition_results")?;
        Ok(Some(payload))
    }

    /// 落盘：写变更过的 payload → 原子提交清单 → 清理废弃文件与目录。
    ///
    /// 任何一步失败返回错误并保留 `dirty`，下次 `flush` 会整体重试。
    pub fn flush(&mut self) -> Result<(), PinError> {
        self.check_writable()?;
        if !self.dirty {
            return Ok(());
        }
        let result = self.snapshot_to_disk();
        match &result {
            Ok(()) => {
                self.error.clear();
                for (id, stored) in &mut self.records {
                    if self.committed.get(id) == Some(&stored.payload_revision) {
                        stored.resident = None;
                    }
                }
                self.dirty = false;
            }
            Err(PinError::Crashed(_)) => {}
            Err(error) => self.error = error.to_string(),
        }
        result
    }

    /// 显式清扫：删除 `pins/` 下未被清单引用的目录（崩溃遗留），返回删除数量。
    ///
    /// 不会在打开时自动执行；建议启动后由调用方调用一次。
    /// 清单损坏（`last_error()` 非空）时拒绝清扫，避免把所有贴图目录当孤儿删除。
    pub fn sweep_orphans(&mut self) -> Result<usize, PinError> {
        self.check_writable()?;
        // 高优 bug #1：清单损坏时 records 为空，清扫会把所有 pins/<id> 当孤儿删光
        if !self.error.is_empty() {
            return Err(PinError::Unavailable(
                "Refusing to sweep orphans: index is in an error state".into(),
            ));
        }
        let pins = self.root.join("pins");
        let Ok(entries) = fs::read_dir(&pins) else {
            return Ok(0);
        };
        let mut removed = 0;
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            let path = entry.path();
            if self.records.contains_key(&name) {
                continue;
            }
            if contained_path(&self.root, &path) && fs::remove_dir_all(&path).is_ok() {
                removed += 1;
            }
        }
        Ok(removed)
    }

    /// 某贴图的 payload 目录 `pins/<id>`。
    fn pin_dir(&self, id: &str) -> PathBuf {
        self.root.join("pins").join(id)
    }

    /// 写操作准入。
    fn check_writable(&self) -> Result<(), PinError> {
        if self.options.write_available {
            Ok(())
        } else {
            Err(PinError::Unavailable(
                "Pinned-window storage is not writable".into(),
            ))
        }
    }

    /// 触发故障注入点。
    fn hit(&self, point: PinCrashPoint) -> Result<(), PinError> {
        match &self.options.fault_hook {
            Some(hook) if hook(point) => Err(PinError::Crashed(point)),
            _ => Ok(()),
        }
    }

    /// 体积与文件名校验。
    fn check_payload(kind: SourceKind, payload: &PinPayload) -> Result<(), PinError> {
        let invalid = |m: &str| PinError::Invalid(m.to_string());
        let blobs = [
            &payload.result_style,
            &payload.canvas_session,
            &payload.recognition_results,
        ];
        if blobs.iter().any(|b| b.len() > MAX_PAYLOAD_BYTES)
            || payload.original_html.len() > MAX_PAYLOAD_BYTES
            || payload.original_text.len() > MAX_PAYLOAD_BYTES
        {
            return Err(invalid("payload exceeds the size limit"));
        }
        match (kind, &payload.image) {
            (SourceKind::ClipboardText, Some(_)) => Err(invalid("text pins carry no image")),
            (SourceKind::ClipboardText, None) => Ok(()),
            (_, None) => Err(invalid("image pins require an image")),
            (kind, Some(image)) => {
                let expected_name = kind == SourceKind::ImageData;
                if image.bytes.len() > MAX_IMAGE_BYTES
                    || !safe_user_file_name(&image.file_name)
                    || (expected_name && image.file_name != SOURCE_IMAGE_FILE)
                {
                    Err(invalid("invalid pin image"))
                } else {
                    Ok(())
                }
            }
        }
    }

    /// 读取并解析清单（对照构造函数）。
    fn load_manifest(&mut self) {
        let path = self.root.join(MANIFEST_NAME);
        if !path.exists() {
            return;
        }
        let Ok(bytes) = fs::read(&path) else {
            self.error = "Pinned-window index could not be read".into();
            return;
        };
        let object = match serde_json::from_slice::<Value>(&bytes) {
            Ok(Value::Object(object))
                if object.get("format_version").and_then(json_int) == Some(FORMAT_VERSION) =>
            {
                object
            }
            _ => {
                self.error = "Pinned-window index is malformed or unsupported".into();
                self.preserve_invalid_index(&path);
                return;
            }
        };
        if let Some(revision) = object
            .get("next_preview_source_revision")
            .and_then(Value::as_str)
            .and_then(|t| t.parse::<u64>().ok())
        {
            self.next_preview_revision = self.next_preview_revision.max(revision);
        }
        for value in object
            .get("groups")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            if let Some(group) = value.as_object() {
                let text = |key: &str| group.get(key).and_then(Value::as_str).unwrap_or_default();
                try_push_group(&mut self.groups, text("id"), text("name"));
            }
        }
        let accent = object
            .get("next_hide_to_top_accent")
            .and_then(json_int)
            .unwrap_or(0);
        self.next_accent = if (0..ACCENT_COUNT).contains(&accent) {
            accent
        } else {
            0
        };
        let active = object
            .get("active_group_id")
            .and_then(Value::as_str)
            .unwrap_or_default();
        if self.groups.iter().any(|g| g.id == active) {
            self.active_group_id = active.to_string();
        }
        for value in object
            .get("records")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            let stored = value.as_object().and_then(|o| self.parse_record(o));
            match stored {
                Some((id, stored)) => {
                    self.committed.insert(id.clone(), 1);
                    self.records.insert(id, stored);
                }
                None => self.skipped += 1,
            }
        }
    }

    /// 解析并校验单条记录；不合法返回 `None`（对照 `parseRecord` 的容器部分）。
    fn parse_record(&mut self, object: &Map<String, Value>) -> Option<(String, Stored)> {
        let id = object
            .get("id")?
            .as_str()
            .filter(|id| is_valid_uuid(id))?
            .to_string();
        let group = object.get("group_id")?.as_str()?;
        let kind = SourceKind::parse(object.get("source_kind")?.as_str()?)?;
        if !safe_group_id(group) || !self.groups.iter().any(|g| g.id == group) {
            return None;
        }
        let mut descriptor = object.get("payloads")?.as_object()?.clone();
        let dir = self.pin_dir(&id);
        if !validate_descriptor(&descriptor, &dir, &id, kind) {
            return None;
        }
        // 缺文件的可选 payload 从描述符里摘掉（图片项已在上面校验存在）。
        descriptor.retain(|key, value| {
            key == KEY_DIRECTORY
                || key == KEY_IMAGE
                || value.as_str().is_some_and(|name| dir.join(name).is_file())
        });
        let mut record = object.clone();
        record.remove("payloads");
        let valid_revision = record
            .get("preview_source_revision")
            .and_then(Value::as_str)
            .and_then(|t| t.parse::<u64>().ok())
            .filter(|n| *n != 0);
        let revision = valid_revision.unwrap_or_else(|| {
            self.next_preview_revision += 1;
            self.next_preview_revision
        });
        self.next_preview_revision = self.next_preview_revision.max(revision);
        record.insert(
            "preview_source_revision".into(),
            json!(revision.to_string()),
        );
        let stored = Stored {
            record,
            kind,
            descriptor,
            signature: Signature::default(),
            resident: None,
            payload_revision: 1,
        };
        Some((id, stored))
    }

    /// 把损坏的清单复制为 `index.json.corrupt.<时间戳>[.n]` 留档。
    fn preserve_invalid_index(&self, path: &Path) {
        let stamp = format_iso_utc_ms((self.options.clock)()).replace(['-', ':', '.'], "");
        let base = format!("{}.corrupt.{stamp}", path.display());
        let mut backup = PathBuf::from(&base);
        let mut suffix = 1;
        while backup.exists() {
            backup = PathBuf::from(format!("{base}.{suffix}"));
            suffix += 1;
        }
        let _ = fs::copy(path, backup);
    }

    /// 写单条记录的 payload 文件（对照 `writePayload`）。
    fn write_payload(&mut self, id: &str) -> Result<(), PinError> {
        let dir = self.pin_dir(id);
        let io_error = |m: &str| PinError::Io(m.to_string());
        fs::create_dir_all(&dir)
            .ok()
            .filter(|()| contained_path(&self.root, &dir))
            .ok_or_else(|| io_error("Unable to create the pin directory"))?;
        let stored = &self.records[id];
        let empty = PinPayload::default();
        let payload = stored.resident.as_ref().unwrap_or(&empty);
        let put = |name: &str, bytes: &[u8]| -> Result<(), PinError> {
            let path = dir.join(name);
            if !safe_file_name(name) || !contained_path(&self.root, &path) {
                return Err(io_error("Unsafe payload path"));
            }
            write_atomic(&path, bytes).map_err(|_| io_error("Unable to write a payload file"))
        };
        if stored.kind != SourceKind::ClipboardText {
            let name = stored
                .descriptor
                .get(KEY_IMAGE)
                .and_then(Value::as_str)
                .unwrap_or_default();
            match payload.image.as_ref().filter(|i| !i.bytes.is_empty()) {
                Some(image) => put(name, &image.bytes)?,
                None if !dir.join(name).is_file() => {
                    return Err(io_error("The pin image is missing"));
                }
                None => {}
            }
        }
        if !payload.original_html.is_empty() {
            put("original.html", payload.original_html.as_bytes())?;
        }
        if !payload.original_text.is_empty() {
            put("original.txt", payload.original_text.as_bytes())?;
        }
        let blobs = [
            ("result_style.bin", &payload.result_style),
            ("canvas_session.bin", &payload.canvas_session),
            ("recognition_results.bin", &payload.recognition_results),
        ];
        for (name, bytes) in blobs {
            if bytes.len() > MAX_PAYLOAD_BYTES {
                return Err(io_error("payload exceeds the size limit"));
            }
            if !bytes.is_empty() {
                put(name, bytes)?;
            }
        }
        self.payload_writes += 1;
        Ok(())
    }

    /// 删除记录目录里清单不再引用的文件（对照 `pruneObsoletePayloadFiles`）。
    fn prune_obsolete_files(&self, id: &str) {
        let Some(stored) = self.records.get(id) else {
            return;
        };
        let retained: BTreeSet<&str> = stored
            .descriptor
            .iter()
            .filter(|(key, _)| key.as_str() != KEY_DIRECTORY)
            .filter_map(|(_, v)| v.as_str())
            .filter(|name| safe_file_name(name))
            .collect();
        let Ok(entries) = fs::read_dir(self.pin_dir(id)) else {
            return;
        };
        for entry in entries.flatten() {
            let is_file = entry.file_type().is_ok_and(|t| t.is_file());
            let name = entry.file_name().to_string_lossy().into_owned();
            if is_file
                && !retained.contains(name.as_str())
                && contained_path(&self.root, &entry.path())
            {
                let _ = fs::remove_file(entry.path());
            }
        }
    }

    /// 完整提交流程（对照 `snapshotToDisk`）。
    fn snapshot_to_disk(&mut self) -> Result<(), PinError> {
        let pins = self.root.join("pins");
        fs::create_dir_all(&pins)
            .ok()
            .filter(|()| contained_path(&self.root, &pins))
            .ok_or_else(|| PinError::Io("Unable to create the pins directory".into()))?;
        let mut changed = Vec::new();
        let ids: Vec<String> = self.records.keys().cloned().collect();
        for id in &ids {
            let revision = self.records[id].payload_revision;
            if self.committed.get(id) != Some(&revision) {
                changed.push(id.clone());
                self.write_payload(id)?;
            }
        }
        self.hit(PinCrashPoint::AfterPayloadsWritten)?;
        let groups: Vec<Value> = self
            .groups
            .iter()
            .map(|g| json!({"id": g.id, "name": g.name, "built_in": g.built_in}))
            .collect();
        let records: Vec<Value> = self
            .records
            .values()
            .map(|stored| {
                let mut record = stored.record.clone();
                record.insert("payloads".into(), Value::Object(stored.descriptor.clone()));
                Value::Object(record)
            })
            .collect();
        let manifest = json!({
            "format_version": FORMAT_VERSION,
            "active_group_id": self.active_group_id,
            "next_hide_to_top_accent": self.next_accent,
            "next_preview_source_revision": self.next_preview_revision.to_string(),
            "groups": groups,
            "records": records,
        });
        write_atomic(&self.root.join(MANIFEST_NAME), &pretty_bytes(&manifest))
            .map_err(|_| PinError::Io("Pinned-window index could not be saved".into()))?;
        for id in &ids {
            let revision = self.records[id].payload_revision;
            self.committed.insert(id.clone(), revision);
        }
        self.hit(PinCrashPoint::AfterManifestCommitted)?;
        for id in &changed {
            self.prune_obsolete_files(id);
        }
        let obsolete: Vec<String> = self
            .committed
            .keys()
            .filter(|id| !self.records.contains_key(*id))
            .cloned()
            .collect();
        for id in obsolete {
            let dir = self.pin_dir(&id);
            let gone = !dir.exists()
                || (contained_path(&self.root, &dir) && fs::remove_dir_all(&dir).is_ok());
            if gone {
                self.committed.remove(&id);
            }
        }
        Ok(())
    }
}
