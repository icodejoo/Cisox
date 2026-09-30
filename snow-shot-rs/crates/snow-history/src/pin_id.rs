//! 贴图 ID 生成：小写、无花括号的随机 v4 UUID 文本，并保证避开已有 ID。
//!
//! 随机源用标准库 `RandomState`（由操作系统随机数播种）叠加进程内计数器与纳秒时钟，
//! 不引入任何第三方依赖。

use std::collections::hash_map::RandomState;
use std::hash::{BuildHasher, Hasher};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

/// 为避开已有 ID 最多尝试的次数（v4 UUID 碰撞概率可忽略，仅作死循环保护）。
const MAX_UNIQUE_ATTEMPTS: usize = 64;
/// UUID 版本位：高 16 位里的版本号 4。
const UUID_VERSION_MASK: u64 = 0xFFFF_FFFF_FFFF_0FFF;
/// UUID 版本 4 的取值（写入第 7 字节高半字节）。
const UUID_VERSION_4: u64 = 0x0000_0000_0000_4000;
/// RFC 4122 变体位：最高两位为 `10`。
const UUID_VARIANT_MASK: u64 = 0x3FFF_FFFF_FFFF_FFFF;
/// RFC 4122 变体位的取值。
const UUID_VARIANT_RFC4122: u64 = 0x8000_0000_0000_0000;

/// 进程内单调计数器，保证同一时刻连续生成的两个种子不同。
static SEED_COUNTER: AtomicU64 = AtomicU64::new(0);

/// 取一个 64 位随机数。
fn random_u64() -> u64 {
    let mut hasher = RandomState::new().build_hasher();
    hasher.write_u64(SEED_COUNTER.fetch_add(1, Ordering::Relaxed));
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    hasher.write_u128(nanos);
    hasher.finish()
}

/// 把两个 64 位随机数格式化为 v4 UUID 文本（写入版本位与变体位）。
///
/// # 参数
/// - `hi`：高 64 位随机数。
/// - `lo`：低 64 位随机数。
///
/// # 返回
/// 形如 `xxxxxxxx-xxxx-4xxx-yxxx-xxxxxxxxxxxx` 的小写文本。
///
/// ```
/// use snow_history::pin_id::format_uuid_v4;
/// let id = format_uuid_v4(0x0123_4567_89ab_cdef, 0xfedc_ba98_7654_3210);
/// assert_eq!(id, "01234567-89ab-4def-bedc-ba9876543210");
/// ```
pub fn format_uuid_v4(hi: u64, lo: u64) -> String {
    let hi = (hi & UUID_VERSION_MASK) | UUID_VERSION_4;
    let lo = (lo & UUID_VARIANT_MASK) | UUID_VARIANT_RFC4122;
    format!(
        "{:08x}-{:04x}-{:04x}-{:04x}-{:012x}",
        hi >> 32,
        (hi >> 16) & 0xFFFF,
        hi & 0xFFFF,
        lo >> 48,
        lo & 0x0000_FFFF_FFFF_FFFF
    )
}

/// 生成一个随机 v4 UUID 文本。
///
/// ```
/// use snow_history::index::is_valid_uuid;
/// use snow_history::pin_id::new_uuid_v4;
/// assert!(is_valid_uuid(&new_uuid_v4()));
/// ```
pub fn new_uuid_v4() -> String {
    format_uuid_v4(random_u64(), random_u64())
}

/// 生成不与已有 ID 冲突的贴图 ID。
///
/// # 参数
/// - `is_taken`：判断某 ID 是否已存在（例如查询贴图仓储的 `record_ids`）。
///
/// # 返回
/// 未被占用的 ID；连续多次碰撞（理论上不会发生）返回错误。
///
/// ```
/// use snow_history::pin_id::new_unique_pin_id;
/// let id = new_unique_pin_id(|_| false).unwrap();
/// assert_eq!(id.len(), 36);
/// ```
pub fn new_unique_pin_id(is_taken: impl Fn(&str) -> bool) -> Result<String, String> {
    for _ in 0..MAX_UNIQUE_ATTEMPTS {
        let id = new_uuid_v4();
        if !is_taken(&id) {
            return Ok(id);
        }
    }
    Err("多次生成贴图 ID 均与已有 ID 冲突".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::index::is_valid_uuid;
    use std::cell::Cell;
    use std::collections::HashSet;

    /// 固定输入得到固定文本，且版本位为 4、变体位为 8/9/a/b。
    #[test]
    fn format_sets_version_and_variant() {
        let id = format_uuid_v4(u64::MAX, u64::MAX);
        assert_eq!(id, "ffffffff-ffff-4fff-bfff-ffffffffffff");
        let zero = format_uuid_v4(0, 0);
        assert_eq!(zero, "00000000-0000-4000-8000-000000000000");
        // 全零随机数也不会产生被仓储拒绝的全零 UUID
        assert!(is_valid_uuid(&zero));
    }

    /// 连续生成 2000 个 ID：全部合法且互不相同。
    #[test]
    fn generated_ids_are_valid_and_distinct() {
        let mut seen = HashSet::new();
        for _ in 0..2000 {
            let id = new_uuid_v4();
            assert!(is_valid_uuid(&id), "非法 UUID: {id}");
            assert_eq!(id.as_bytes()[14], b'4');
            assert!(matches!(id.as_bytes()[19], b'8' | b'9' | b'a' | b'b'));
            assert!(seen.insert(id));
        }
    }

    /// 前几次生成的 ID 被判定为已占用时会重试，最终返回未占用的 ID。
    #[test]
    fn unique_id_retries_on_collision() {
        let calls = Cell::new(0);
        let id = new_unique_pin_id(|_| {
            calls.set(calls.get() + 1);
            calls.get() < 3
        })
        .unwrap();
        assert_eq!(calls.get(), 3);
        assert!(is_valid_uuid(&id));
    }

    /// 始终冲突时返回错误而不是死循环。
    #[test]
    fn unique_id_gives_up() {
        assert!(new_unique_pin_id(|_| true).is_err());
    }
}
