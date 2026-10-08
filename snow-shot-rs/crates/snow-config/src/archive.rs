//! 配置归档：把设置导出为单个 zip 归档、从归档读回（对应旧版 `ConfigurationArchive`）。
//!
//! 归档含两个条目：`manifest.json`（格式标记、版本、可选的“已脱敏模型 id”）与
//! `config.json`（扁平 `"组/名"` 键值）。本模块自带最小 zip 读写（不新增依赖）：
//! 写出一律“存储”方式（不压缩）；读取只接受“存储”方式，旧版用 deflate 压缩的归档
//! 会返回 [`ArchiveError::UnsupportedCompression`]（与旧版格式的差异，见审计表 A06）。

use crate::normalize::normalize;
use crate::schema::{SCHEMA_VERSION_KEY, contains, current_version, parse_integer_version};
use serde_json::{Map, Value, json};
use std::collections::BTreeMap;
use std::fmt;
use std::fs;
use std::io::Write;
use std::path::Path;

/// 归档格式标记。
pub const ARCHIVE_FORMAT: &str = "snow-shot-configuration";
/// 当前归档格式版本。
pub const ARCHIVE_FORMAT_VERSION: i32 = 1;
/// 清单条目名。
const MANIFEST_ENTRY: &str = "manifest.json";
/// 配置条目名。
const CONFIG_ENTRY: &str = "config.json";
/// 归档内最多条目数。
const MAX_ENTRIES: usize = 16;
/// 清单条目大小上限（字节）。
const MAX_MANIFEST_BYTES: usize = 64 * 1024;
/// 配置条目大小上限（字节）。
const MAX_CONFIG_BYTES: usize = 8 * 1024 * 1024;
/// 自定义 AI 模型的配置键。
const CUSTOM_MODELS_KEY: &str = "api_configuration/custom_models";
/// 模型记录里的 id 字段。
const MODEL_ID_FIELD: &str = "id";
/// 模型记录里的密钥字段。
const MODEL_API_KEY_FIELD: &str = "api_key";
/// 模型记录里的地址字段。
const MODEL_BASE_URL_FIELD: &str = "base_url";
/// 清单字段：格式。
const FIELD_FORMAT: &str = "format";
/// 清单字段：格式版本。
const FIELD_FORMAT_VERSION: &str = "format_version";
/// 清单字段：配置 schema 版本。
const FIELD_SCHEMA_VERSION: &str = "schema_version";
/// 清单字段：已脱敏的模型 id。
const FIELD_REDACTED: &str = "redacted_credentials";
/// 导入时保持当前值的开机自启键（由系统事务单独提交）。
pub const KEY_AUTO_START: &str = "system/auto_start_at_boot";
/// 导入时保持当前值的管理员启动键。
pub const KEY_RUN_AS_ADMIN: &str = "system/launch_as_administrator";

/// zip 本地文件头签名。
const SIG_LOCAL: u32 = 0x0403_4b50;
/// zip 中央目录项签名。
const SIG_CENTRAL: u32 = 0x0201_4b50;
/// zip 目录结束记录签名。
const SIG_EOCD: u32 = 0x0605_4b50;
/// 本地文件头固定长度。
const LOCAL_HEADER_LEN: usize = 30;
/// 中央目录项固定长度。
const CENTRAL_HEADER_LEN: usize = 46;
/// 目录结束记录固定长度。
const EOCD_LEN: usize = 22;
/// zip 注释最大长度。
const MAX_COMMENT_LEN: usize = 65_535;
/// “存储”压缩方式。
const METHOD_STORED: u16 = 0;
/// deflate 压缩方式。
const METHOD_DEFLATE: u16 = 8;
/// 通用标志：UTF-8 文件名。
const FLAG_UTF8: u16 = 0x0800;
/// 通用标志：加密。
const FLAG_ENCRYPTED: u16 = 0x0001;
/// 写出时使用的“需要的最低版本”。
const ZIP_VERSION: u16 = 20;
/// 固定的 DOS 日期（1980-01-01），保证输出可复现。
const DOS_DATE: u16 = 0x0021;
/// 文件类型掩码（unix 模式位）。
const MODE_TYPE_MASK: u32 = 0o170_000;
/// 普通文件类型。
const MODE_REGULAR: u32 = 0o100_000;

/// 归档读写错误。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ArchiveError {
    /// 文件不是有效的配置归档（损坏、结构不符、超限）。
    Invalid,
    /// 是 zip 但格式标记不是配置归档。
    NotConfigArchive,
    /// 由更新版本创建（格式或 schema 版本更高）。
    TooNew,
    /// 归档内没有可用的设置。
    NoCompatibleSettings,
    /// 条目使用了本实现不支持的压缩方式（如旧版的 deflate）。
    UnsupportedCompression,
    /// 文件系统错误（携带说明）。
    Io(String),
}

impl ArchiveError {
    /// 对应的界面文案 id（`config_transfer.ftl`）。
    pub fn message_id(&self) -> &'static str {
        match self {
            Self::Invalid => "config-transfer-error-invalid",
            Self::NotConfigArchive => "config-transfer-error-not-archive",
            Self::TooNew => "config-transfer-error-too-new",
            Self::NoCompatibleSettings => "config-transfer-error-empty",
            Self::UnsupportedCompression => "config-transfer-error-compression",
            Self::Io(_) => "config-transfer-error-io",
        }
    }
}

impl fmt::Display for ArchiveError {
    /// 输出英文诊断文本（界面文案走 [`ArchiveError::message_id`]）。
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Invalid => f.write_str("not a valid configuration archive"),
            Self::NotConfigArchive => f.write_str("not a configuration archive"),
            Self::TooNew => f.write_str("archive was created by a newer version"),
            Self::NoCompatibleSettings => f.write_str("archive has no compatible settings"),
            Self::UnsupportedCompression => f.write_str("unsupported zip compression method"),
            Self::Io(text) => write!(f, "io error: {text}"),
        }
    }
}

impl std::error::Error for ArchiveError {}

/// 读取归档的结果。
#[derive(Debug, Clone, PartialEq)]
pub struct ArchiveContents {
    /// 通过规范化的扁平键值（不含 schema 版本键）。
    pub values: BTreeMap<String, Value>,
    /// 归档的 schema 版本。
    pub schema_version: i32,
    /// 导出时被清空密钥的自定义模型 id。
    pub redacted_credential_ids: Vec<String>,
}

/// 把当前设置打成归档字节。
///
/// # 参数
/// - `values`：扁平键值（`storage/schema_version` 会被忽略）
/// - `schema_version`：写入清单的 schema 版本
/// - `app_version`：写入清单的应用版本
/// - `created`：创建时间文本（建议 [`iso_utc_now`]）
/// - `redact_credentials`：为真时清空自定义模型的 `api_key` 并在清单里记录模型 id
///
/// # 返回
/// zip 字节。
///
/// # 示例
/// ```
/// use snow_config::archive::{read_archive_bytes, write_archive_bytes};
/// use std::collections::BTreeMap;
///
/// let mut values = BTreeMap::new();
/// values.insert("screenshot/image_quality".to_string(), serde_json::json!(80));
/// let bytes = write_archive_bytes(&values, 3, "1.0.0", "2026-01-01T00:00:00.000Z", false);
/// assert_eq!(read_archive_bytes(&bytes).unwrap().values.len(), 1);
/// ```
pub fn write_archive_bytes(
    values: &BTreeMap<String, Value>,
    schema_version: i32,
    app_version: &str,
    created: &str,
    redact_credentials: bool,
) -> Vec<u8> {
    let mut config = Map::new();
    for (key, value) in values {
        if key != SCHEMA_VERSION_KEY {
            config.insert(key.clone(), value.clone());
        }
    }
    let mut manifest = Map::new();
    if redact_credentials {
        let mut omitted = Vec::new();
        if let Some(Value::Array(models)) = config.get_mut(CUSTOM_MODELS_KEY) {
            for model in models.iter_mut() {
                if let Some(record) = model.as_object_mut() {
                    omitted.push(record.get(MODEL_ID_FIELD).cloned().unwrap_or(Value::Null));
                    record.insert(MODEL_API_KEY_FIELD.to_string(), json!(""));
                }
            }
        }
        manifest.insert(FIELD_REDACTED.to_string(), Value::Array(omitted));
    }
    manifest.insert(FIELD_FORMAT.to_string(), json!(ARCHIVE_FORMAT));
    manifest.insert(
        FIELD_FORMAT_VERSION.to_string(),
        json!(ARCHIVE_FORMAT_VERSION),
    );
    manifest.insert(FIELD_SCHEMA_VERSION.to_string(), json!(schema_version));
    manifest.insert("app_version".to_string(), json!(app_version));
    manifest.insert("created".to_string(), json!(created));
    let manifest_bytes = serde_json::to_vec(&Value::Object(manifest)).unwrap_or_default();
    let config_bytes = serde_json::to_vec(&Value::Object(config)).unwrap_or_default();
    build_zip(&[
        (MANIFEST_ENTRY, &manifest_bytes),
        (CONFIG_ENTRY, &config_bytes),
    ])
}

/// 把归档原子写到文件（同目录临时文件 + 改名覆盖，失败时清理临时文件）。
///
/// # 参数
/// - `path`：目标归档路径（父目录不存在时创建）
/// - `bytes`：[`write_archive_bytes`] 的结果
pub fn write_archive_file(path: &Path, bytes: &[u8]) -> Result<(), ArchiveError> {
    let io = |e: std::io::Error| ArchiveError::Io(e.to_string());
    let name = path.file_name().ok_or(ArchiveError::Invalid)?;
    if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
        fs::create_dir_all(parent).map_err(io)?;
    }
    let temporary = path.with_file_name(format!(".{}.part", name.to_string_lossy()));
    let result = (|| {
        let mut file = fs::File::create(&temporary)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        drop(file);
        fs::rename(&temporary, path)
    })();
    if let Err(e) = result {
        let _ = fs::remove_file(&temporary);
        return Err(io(e));
    }
    Ok(())
}

/// 读取并校验归档文件。
///
/// # 参数
/// - `path`：归档路径
///
/// # 返回
/// 校验通过的内容；失败原因见 [`ArchiveError`]。
pub fn read_archive_file(path: &Path) -> Result<ArchiveContents, ArchiveError> {
    let metadata = fs::metadata(path).map_err(|_| ArchiveError::Invalid)?;
    if metadata.len() > (MAX_CONFIG_BYTES + MAX_MANIFEST_BYTES) as u64 + 4096 {
        return Err(ArchiveError::Invalid);
    }
    let bytes = fs::read(path).map_err(|_| ArchiveError::Invalid)?;
    read_archive_bytes(&bytes)
}

/// 解析并校验归档字节：条目白名单、大小上限、CRC、格式与版本，再逐键规范化。
///
/// # 参数
/// - `bytes`：zip 字节
///
/// # 返回
/// 通过规范化的键值；未知键与非法值被丢弃，全部被丢弃时返回 [`ArchiveError::NoCompatibleSettings`]。
pub fn read_archive_bytes(bytes: &[u8]) -> Result<ArchiveContents, ArchiveError> {
    let entries = parse_zip(bytes)?;
    let manifest_bytes = entries.get(MANIFEST_ENTRY).ok_or(ArchiveError::Invalid)?;
    let config_bytes = entries.get(CONFIG_ENTRY).ok_or(ArchiveError::Invalid)?;
    let manifest = json_object(manifest_bytes)?;
    if manifest.get(FIELD_FORMAT).and_then(Value::as_str) != Some(ARCHIVE_FORMAT) {
        return Err(ArchiveError::NotConfigArchive);
    }
    let format_version = manifest
        .get(FIELD_FORMAT_VERSION)
        .and_then(parse_integer_version)
        .ok_or(ArchiveError::Invalid)?;
    if format_version > ARCHIVE_FORMAT_VERSION {
        return Err(ArchiveError::TooNew);
    }
    if format_version != ARCHIVE_FORMAT_VERSION {
        return Err(ArchiveError::Invalid);
    }
    let schema_version = manifest
        .get(FIELD_SCHEMA_VERSION)
        .and_then(parse_integer_version)
        .ok_or(ArchiveError::Invalid)?;
    if schema_version > current_version() {
        return Err(ArchiveError::TooNew);
    }
    let mut redacted = Vec::new();
    if let Some(list) = manifest.get(FIELD_REDACTED).and_then(Value::as_array) {
        for id in list {
            redacted.push(id.as_str().ok_or(ArchiveError::Invalid)?.to_string());
        }
    }
    let config = json_object(config_bytes)?;
    let mut values = BTreeMap::new();
    for (key, value) in &config {
        if key == SCHEMA_VERSION_KEY || !contains(key) {
            continue;
        }
        let normalized = normalize(key, value);
        if normalized.valid {
            values.insert(key.clone(), normalized.value);
        }
    }
    if values.is_empty() {
        return Err(ArchiveError::NoCompatibleSettings);
    }
    Ok(ArchiveContents {
        values,
        schema_version,
        redacted_credential_ids: redacted,
    })
}

/// 把脱敏模型的密钥从当前配置补回（id 与 `base_url` 都相同才补）。
///
/// # 参数
/// - `contents`：读取到的归档内容（原地修改）
/// - `current`：当前配置的扁平键值
pub fn preserve_omitted_credentials(
    contents: &mut ArchiveContents,
    current: &BTreeMap<String, Value>,
) {
    if contents.redacted_credential_ids.is_empty() {
        return;
    }
    let existing = current
        .get(CUSTOM_MODELS_KEY)
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let ids = &contents.redacted_credential_ids;
    let Some(Value::Array(models)) = contents.values.get_mut(CUSTOM_MODELS_KEY) else {
        return;
    };
    for model in models.iter_mut() {
        let Some(record) = model.as_object_mut() else {
            continue;
        };
        let id = record
            .get(MODEL_ID_FIELD)
            .and_then(Value::as_str)
            .unwrap_or_default();
        if !ids.iter().any(|candidate| candidate == id) {
            continue;
        }
        let base_url = record.get(MODEL_BASE_URL_FIELD).cloned();
        let previous = existing.iter().filter_map(Value::as_object).find(|old| {
            old.get(MODEL_ID_FIELD).and_then(Value::as_str) == Some(id)
                && old.get(MODEL_BASE_URL_FIELD) == base_url.as_ref()
        });
        if let Some(key) = previous.and_then(|old| old.get(MODEL_API_KEY_FIELD)) {
            record.insert(MODEL_API_KEY_FIELD.to_string(), key.clone());
        }
    }
}

/// 导入前把“系统事务单独提交”的两个启动项钉回当前值。
///
/// # 参数
/// - `contents`：读取到的归档内容（原地修改）
/// - `current`：当前配置的扁平键值
pub fn pin_startup_keys(contents: &mut ArchiveContents, current: &BTreeMap<String, Value>) {
    for key in [KEY_AUTO_START, KEY_RUN_AS_ADMIN] {
        if let Some(value) = current.get(key) {
            contents.values.insert(key.to_string(), value.clone());
        }
    }
}

/// 当前 UTC 时间的 ISO 8601 文本（毫秒），如 `2026-01-01T00:00:00.000Z`。
pub fn iso_utc_now() -> String {
    let since = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    iso_utc(since.as_secs(), since.subsec_millis())
}

/// 把 Unix 秒与毫秒格式化为 ISO 8601（UTC）。
fn iso_utc(seconds: u64, millis: u32) -> String {
    let days = (seconds / 86_400) as i64;
    let rest = seconds % 86_400;
    // 公历日期换算（Howard Hinnant 的 civil_from_days）。
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}.{millis:03}Z",
        rest / 3600,
        rest % 3600 / 60,
        rest % 60
    )
}

/// 解析 JSON 对象字节。
fn json_object(bytes: &[u8]) -> Result<Map<String, Value>, ArchiveError> {
    match serde_json::from_slice::<Value>(bytes) {
        Ok(Value::Object(map)) => Ok(map),
        _ => Err(ArchiveError::Invalid),
    }
}

/// 逐字节计算 CRC-32（IEEE），用 const 表避免新增依赖。
fn crc32(data: &[u8]) -> u32 {
    const TABLE: [u32; 256] = {
        let mut table = [0u32; 256];
        let mut i = 0;
        while i < 256 {
            let mut c = i as u32;
            let mut k = 0;
            while k < 8 {
                c = if c & 1 != 0 {
                    0xEDB8_8320 ^ (c >> 1)
                } else {
                    c >> 1
                };
                k += 1;
            }
            table[i] = c;
            i += 1;
        }
        table
    };
    let mut crc = 0xFFFF_FFFFu32;
    for byte in data {
        crc = TABLE[((crc ^ u32::from(*byte)) & 0xFF) as usize] ^ (crc >> 8);
    }
    !crc
}

/// 以小端写入 16 位。
fn put16(out: &mut Vec<u8>, value: u16) {
    out.extend_from_slice(&value.to_le_bytes());
}

/// 以小端写入 32 位。
fn put32(out: &mut Vec<u8>, value: u32) {
    out.extend_from_slice(&value.to_le_bytes());
}

/// 读取小端 16 位（调用方保证越界已检查）。
fn get16(data: &[u8], at: usize) -> Option<u16> {
    Some(u16::from_le_bytes(data.get(at..at + 2)?.try_into().ok()?))
}

/// 读取小端 32 位。
fn get32(data: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_le_bytes(data.get(at..at + 4)?.try_into().ok()?))
}

/// 用“存储”方式打包若干条目为 zip。
fn build_zip(files: &[(&str, &[u8])]) -> Vec<u8> {
    let mut out = Vec::new();
    let mut central = Vec::new();
    for (name, data) in files {
        let offset = out.len() as u32;
        let crc = crc32(data);
        let size = data.len() as u32;
        put32(&mut out, SIG_LOCAL);
        put16(&mut out, ZIP_VERSION);
        put16(&mut out, FLAG_UTF8);
        put16(&mut out, METHOD_STORED);
        put16(&mut out, 0);
        put16(&mut out, DOS_DATE);
        put32(&mut out, crc);
        put32(&mut out, size);
        put32(&mut out, size);
        put16(&mut out, name.len() as u16);
        put16(&mut out, 0);
        out.extend_from_slice(name.as_bytes());
        out.extend_from_slice(data);

        put32(&mut central, SIG_CENTRAL);
        put16(&mut central, ZIP_VERSION);
        put16(&mut central, ZIP_VERSION);
        put16(&mut central, FLAG_UTF8);
        put16(&mut central, METHOD_STORED);
        put16(&mut central, 0);
        put16(&mut central, DOS_DATE);
        put32(&mut central, crc);
        put32(&mut central, size);
        put32(&mut central, size);
        put16(&mut central, name.len() as u16);
        put16(&mut central, 0);
        put16(&mut central, 0);
        put16(&mut central, 0);
        put16(&mut central, 0);
        put32(&mut central, 0);
        put32(&mut central, offset);
        central.extend_from_slice(name.as_bytes());
    }
    let central_offset = out.len() as u32;
    let central_size = central.len() as u32;
    out.extend_from_slice(&central);
    put32(&mut out, SIG_EOCD);
    put16(&mut out, 0);
    put16(&mut out, 0);
    put16(&mut out, files.len() as u16);
    put16(&mut out, files.len() as u16);
    put32(&mut out, central_size);
    put32(&mut out, central_offset);
    put16(&mut out, 0);
    out
}

/// 解析 zip，只接受白名单条目，返回 `条目名 -> 内容`；任何结构异常都按无效处理。
fn parse_zip(data: &[u8]) -> Result<BTreeMap<String, Vec<u8>>, ArchiveError> {
    let invalid = || ArchiveError::Invalid;
    if data.len() < EOCD_LEN {
        return Err(invalid());
    }
    let search_from = data.len().saturating_sub(EOCD_LEN + MAX_COMMENT_LEN);
    let eocd = (search_from..=data.len() - EOCD_LEN)
        .rev()
        .find(|&at| {
            get32(data, at) == Some(SIG_EOCD)
                && get16(data, at + 20)
                    .is_some_and(|c| at + EOCD_LEN + usize::from(c) == data.len())
        })
        .ok_or_else(invalid)?;
    let count = usize::from(get16(data, eocd + 10).ok_or_else(invalid)?);
    let central_size = get32(data, eocd + 12).ok_or_else(invalid)? as usize;
    let central_offset = get32(data, eocd + 16).ok_or_else(invalid)? as usize;
    if count > MAX_ENTRIES
        || central_offset
            .checked_add(central_size)
            .is_none_or(|end| end > eocd)
    {
        return Err(invalid());
    }
    let mut result = BTreeMap::new();
    let mut at = central_offset;
    for _ in 0..count {
        if get32(data, at) != Some(SIG_CENTRAL) {
            return Err(invalid());
        }
        let flags = get16(data, at + 8).ok_or_else(invalid)?;
        let method = get16(data, at + 10).ok_or_else(invalid)?;
        let crc = get32(data, at + 16).ok_or_else(invalid)?;
        let compressed = get32(data, at + 20).ok_or_else(invalid)? as usize;
        let size = get32(data, at + 24).ok_or_else(invalid)? as usize;
        let name_len = usize::from(get16(data, at + 28).ok_or_else(invalid)?);
        let extra_len = usize::from(get16(data, at + 30).ok_or_else(invalid)?);
        let comment_len = usize::from(get16(data, at + 32).ok_or_else(invalid)?);
        let attributes = get32(data, at + 38).ok_or_else(invalid)?;
        let local_offset = get32(data, at + 42).ok_or_else(invalid)? as usize;
        let name_bytes = data
            .get(at + CENTRAL_HEADER_LEN..at + CENTRAL_HEADER_LEN + name_len)
            .ok_or_else(invalid)?;
        at += CENTRAL_HEADER_LEN + name_len + extra_len + comment_len;
        let mode = (attributes >> 16) & MODE_TYPE_MASK;
        if flags & FLAG_ENCRYPTED != 0 || (mode != 0 && mode != MODE_REGULAR) {
            return Err(invalid());
        }
        let name = std::str::from_utf8(name_bytes).map_err(|_| invalid())?;
        let limit = match name {
            MANIFEST_ENTRY => MAX_MANIFEST_BYTES,
            CONFIG_ENTRY => MAX_CONFIG_BYTES,
            _ => return Err(invalid()),
        };
        if result.contains_key(name) || size > limit {
            return Err(invalid());
        }
        match method {
            METHOD_STORED => {}
            METHOD_DEFLATE => return Err(ArchiveError::UnsupportedCompression),
            _ => return Err(invalid()),
        }
        if compressed != size || get32(data, local_offset) != Some(SIG_LOCAL) {
            return Err(invalid());
        }
        let local_name = usize::from(get16(data, local_offset + 26).ok_or_else(invalid)?);
        let local_extra = usize::from(get16(data, local_offset + 28).ok_or_else(invalid)?);
        let start = local_offset + LOCAL_HEADER_LEN + local_name + local_extra;
        let payload = data.get(start..start + size).ok_or_else(invalid)?;
        if crc32(payload) != crc {
            return Err(invalid());
        }
        result.insert(name.to_string(), payload.to_vec());
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::document::ConfigDocument;

    /// 构造两个键 + 一个带密钥的模型的快照。
    fn sample() -> BTreeMap<String, Value> {
        let mut values = ConfigDocument::from_bytes(None).values().clone();
        values.insert("screenshot/image_quality".into(), json!(77));
        values.insert(
            CUSTOM_MODELS_KEY.into(),
            json!([{
                "id": "0a1b2c3d-0000-4000-8000-000000000001",
                "name": "m",
                "base_url": "https://example.com/v1",
                "api_key": "secret",
                "model": "x",
                "supports_vision": false,
                "supports_reasoning": false
            }]),
        );
        values
    }

    /// 导出再导入，键值完全一致。
    #[test]
    fn round_trip() {
        let values = sample();
        let bytes = write_archive_bytes(&values, current_version(), "1.0", "t", false);
        let back = read_archive_bytes(&bytes).unwrap();
        assert_eq!(back.values["screenshot/image_quality"], json!(77));
        assert_eq!(back.schema_version, current_version());
        assert!(back.redacted_credential_ids.is_empty());
        assert_eq!(
            back.values[CUSTOM_MODELS_KEY][0]["api_key"],
            json!("secret")
        );
        assert!(!back.values.contains_key(SCHEMA_VERSION_KEY));
    }

    /// 脱敏导出：密钥清空、id 记入清单，导入后用当前配置补回。
    #[test]
    fn redaction_and_restore() {
        let values = sample();
        let bytes = write_archive_bytes(&values, current_version(), "1.0", "t", true);
        let mut back = read_archive_bytes(&bytes).unwrap();
        assert_eq!(back.values[CUSTOM_MODELS_KEY][0]["api_key"], json!(""));
        assert_eq!(back.redacted_credential_ids.len(), 1);
        preserve_omitted_credentials(&mut back, &values);
        assert_eq!(
            back.values[CUSTOM_MODELS_KEY][0]["api_key"],
            json!("secret")
        );
        // base_url 不同则不补。
        let mut changed = values.clone();
        changed.get_mut(CUSTOM_MODELS_KEY).unwrap()[0]["base_url"] = json!("https://other.com/v1");
        let mut again = read_archive_bytes(&bytes).unwrap();
        preserve_omitted_credentials(&mut again, &changed);
        assert_eq!(again.values[CUSTOM_MODELS_KEY][0]["api_key"], json!(""));
    }

    /// 启动项被钉回当前值。
    #[test]
    fn startup_keys_pinned() {
        let mut values = sample();
        values.insert(KEY_AUTO_START.into(), json!(true));
        let bytes = write_archive_bytes(&values, current_version(), "1.0", "t", false);
        let mut back = read_archive_bytes(&bytes).unwrap();
        let mut current = sample();
        current.insert(KEY_AUTO_START.into(), json!(false));
        pin_startup_keys(&mut back, &current);
        assert_eq!(back.values[KEY_AUTO_START], json!(false));
    }

    /// 重新打包任意条目，便于构造异常归档。
    fn archive_with(manifest: Value, config: Value) -> Vec<u8> {
        build_zip(&[
            (MANIFEST_ENTRY, &serde_json::to_vec(&manifest).unwrap()),
            (CONFIG_ENTRY, &serde_json::to_vec(&config).unwrap()),
        ])
    }

    /// 合法清单。
    fn manifest(format_version: i64, schema_version: i64) -> Value {
        json!({"format": ARCHIVE_FORMAT, "format_version": format_version, "schema_version": schema_version})
    }

    /// 版本过新与格式不符分别报对应错误。
    #[test]
    fn version_checks() {
        let cfg = json!({"screenshot/image_quality": 70});
        let newer_format = archive_with(manifest(2, 3), cfg.clone());
        assert_eq!(read_archive_bytes(&newer_format), Err(ArchiveError::TooNew));
        let newer_schema = archive_with(manifest(1, i64::from(current_version()) + 1), cfg.clone());
        assert_eq!(read_archive_bytes(&newer_schema), Err(ArchiveError::TooNew));
        let wrong = archive_with(
            json!({"format": "other", "format_version": 1, "schema_version": 3}),
            cfg.clone(),
        );
        assert_eq!(
            read_archive_bytes(&wrong),
            Err(ArchiveError::NotConfigArchive)
        );
        let zero = archive_with(manifest(0, 3), cfg);
        assert_eq!(read_archive_bytes(&zero), Err(ArchiveError::Invalid));
    }

    /// 未知键与非法值被丢弃；全丢弃则报无可用设置。
    #[test]
    fn unknown_and_invalid_values_dropped() {
        let cfg = json!({"screenshot/image_quality": 70, "nope/key": 1});
        let ok = read_archive_bytes(&archive_with(manifest(1, 3), cfg)).unwrap();
        assert_eq!(ok.values.len(), 1);
        let bad = json!({"screenshot/image_quality": 9999, "nope/key": 1});
        assert_eq!(
            read_archive_bytes(&archive_with(manifest(1, 3), bad)),
            Err(ArchiveError::NoCompatibleSettings)
        );
    }

    /// 损坏、截断、位翻转、多余条目、deflate 都被拒绝，且不 panic。
    #[test]
    fn corrupt_archives_rejected() {
        let good = write_archive_bytes(&sample(), current_version(), "1", "t", false);
        assert!(read_archive_bytes(&[]).is_err());
        assert!(read_archive_bytes(b"not a zip at all, definitely not").is_err());
        for cut in [10, good.len() / 2, good.len() - 1] {
            assert!(read_archive_bytes(&good[..cut]).is_err());
        }
        let mut flipped = good.clone();
        flipped[LOCAL_HEADER_LEN + MANIFEST_ENTRY.len() + 3] ^= 0xFF;
        assert_eq!(read_archive_bytes(&flipped), Err(ArchiveError::Invalid));
        let extra = build_zip(&[
            (MANIFEST_ENTRY, b"{}"),
            (CONFIG_ENTRY, b"{}"),
            ("evil.txt", b"x"),
        ]);
        assert_eq!(read_archive_bytes(&extra), Err(ArchiveError::Invalid));
        let dup = build_zip(&[(CONFIG_ENTRY, b"{}"), (CONFIG_ENTRY, b"{}")]);
        assert_eq!(read_archive_bytes(&dup), Err(ArchiveError::Invalid));
        let not_json = build_zip(&[(MANIFEST_ENTRY, b"zzz"), (CONFIG_ENTRY, b"{}")]);
        assert_eq!(read_archive_bytes(&not_json), Err(ArchiveError::Invalid));
        // 把方法改成 deflate（中央目录偏移 10）。
        let mut deflate = good.clone();
        let central = deflate
            .windows(4)
            .position(|w| w == SIG_CENTRAL.to_le_bytes())
            .unwrap();
        deflate[central + 10] = METHOD_DEFLATE as u8;
        assert_eq!(
            read_archive_bytes(&deflate),
            Err(ArchiveError::UnsupportedCompression)
        );
    }

    /// CRC 与时间格式的已知值。
    #[test]
    fn crc_and_time() {
        assert_eq!(crc32(b"123456789"), 0xCBF4_3926);
        assert_eq!(iso_utc(0, 0), "1970-01-01T00:00:00.000Z");
        assert_eq!(iso_utc(1_767_225_600, 7), "2026-01-01T00:00:00.007Z");
    }

    /// 文件读写往返；写入失败不留临时文件；用临时目录，不碰上游数据目录。
    #[test]
    fn file_round_trip() {
        let dir = std::env::temp_dir().join(format!("cisox-archive-test-{}", std::process::id()));
        let path = dir.join("sub").join("a.zip");
        let bytes = write_archive_bytes(&sample(), current_version(), "1", "t", false);
        write_archive_file(&path, &bytes).unwrap();
        assert!(read_archive_file(&path).is_ok());
        let leftovers = fs::read_dir(path.parent().unwrap()).unwrap().count();
        assert_eq!(leftovers, 1);
        assert_eq!(
            read_archive_file(&dir.join("missing.zip")),
            Err(ArchiveError::Invalid)
        );
        let _ = fs::remove_dir_all(&dir);
    }
}
