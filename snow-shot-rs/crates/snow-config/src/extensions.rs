//! Cisox 扩展配置项：不属于 Qt 版 238 项 schema 的新增键。
//!
//! 现有 238 项与 C++ 逐项对齐、不得改动；本地翻译（NMT）相关的新增项集中在这里，全部放在
//! `screenshot_translation/` 分组下。文档层对未知字段是“保留”的，因此 Qt 版读到这些键只会忽略，
//! 不会破坏磁盘 JSON 的兼容性；Rust 侧则把它们当作正式配置项（有默认值、类型与范围校验）。

use crate::schema::{IntRange, SchemaEntry, ValueKind, entry};
use serde_json::json;

/// 扩展项数量（`schema::entries()` 在原 238 项之后追加的条目数）。
pub const EXTENSION_ENTRY_COUNT: usize = 6;

/// 翻译后端：`local` 本地 NMT worker，`openai` OpenAI 兼容通道。
pub const KEY_TRANSLATION_BACKEND: &str = "screenshot_translation/backend";
/// 本地模型根目录；空串表示 `<数据根>/models/translate`。
pub const KEY_LOCAL_MODELS_DIR: &str = "screenshot_translation/local_models_dir";
/// 本地默认模型 ID；空串表示自动选择第一个支持所选语言对的模型。
pub const KEY_LOCAL_MODEL_ID: &str = "screenshot_translation/local_model_id";
/// 本地 worker 空闲多少秒后卸载（进程退出，内存全部回收）。
pub const KEY_LOCAL_IDLE_SECONDS: &str = "screenshot_translation/local_idle_unload_seconds";
/// 本地解码束宽：1 为贪心，越大质量越好、翻译期间内存越高。
pub const KEY_LOCAL_NUM_BEAMS: &str = "screenshot_translation/local_num_beams";
/// 本地低内存模式：强制贪心解码，并在每次请求后收缩内存。
pub const KEY_LOCAL_LOW_MEMORY: &str = "screenshot_translation/local_low_memory";

/// 后端取值：本地 NMT。
pub const BACKEND_LOCAL: &str = "local";
/// 后端取值：OpenAI 兼容通道。
pub const BACKEND_OPENAI: &str = "openai";
/// 后端白名单。
const BACKEND_VALUES: &[&str] = &[BACKEND_LOCAL, BACKEND_OPENAI];
/// 空闲卸载秒数默认值。
pub const DEFAULT_IDLE_SECONDS: i32 = 120;
/// 空闲卸载秒数下限。
pub const MIN_IDLE_SECONDS: i32 = 10;
/// 空闲卸载秒数上限（1 小时）。
pub const MAX_IDLE_SECONDS: i32 = 3600;
/// 空闲卸载秒数的界面步长。
const IDLE_SECONDS_STEP: i32 = 10;
/// 束宽默认值（模型卡推荐 4）。
pub const DEFAULT_NUM_BEAMS: i32 = 4;
/// 束宽上限（与 worker 的 `MAX_BEAMS` 一致）。
pub const MAX_NUM_BEAMS: i32 = 8;

/// 返回全部扩展条目（追加在原 238 项之后）。
pub(crate) fn extension_entries() -> Vec<SchemaEntry> {
    vec![
        entry(
            KEY_TRANSLATION_BACKEND,
            json!(BACKEND_LOCAL),
            ValueKind::String,
            None,
            BACKEND_VALUES,
            None,
        ),
        entry(
            KEY_LOCAL_MODELS_DIR,
            json!(""),
            ValueKind::String,
            None,
            &[],
            None,
        ),
        entry(
            KEY_LOCAL_MODEL_ID,
            json!(""),
            ValueKind::String,
            None,
            &[],
            None,
        ),
        entry(
            KEY_LOCAL_IDLE_SECONDS,
            json!(DEFAULT_IDLE_SECONDS),
            ValueKind::Integer,
            Some(IntRange {
                min: MIN_IDLE_SECONDS,
                max: MAX_IDLE_SECONDS,
                step: IDLE_SECONDS_STEP,
            }),
            &[],
            None,
        ),
        entry(
            KEY_LOCAL_NUM_BEAMS,
            json!(DEFAULT_NUM_BEAMS),
            ValueKind::Integer,
            Some(IntRange {
                min: 1,
                max: MAX_NUM_BEAMS,
                step: 1,
            }),
            &[],
            None,
        ),
        entry(
            KEY_LOCAL_LOW_MEMORY,
            json!(false),
            ValueKind::Boolean,
            None,
            &[],
            None,
        ),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::document::ConfigDocument;
    use crate::normalize::normalize;
    use crate::schema::{CORE_ENTRY_COUNT, core_entries, entries, entry_for};

    /// 扩展项恰好追加在核心 238 项之后，数量与常量一致，且不与核心键重名。
    #[test]
    fn extensions_follow_core_entries() {
        assert_eq!(CORE_ENTRY_COUNT, 238);
        assert_eq!(entries().len(), CORE_ENTRY_COUNT + EXTENSION_ENTRY_COUNT);
        assert_eq!(core_entries().len(), CORE_ENTRY_COUNT);
        let core: std::collections::HashSet<_> = core_entries().iter().map(|e| e.key).collect();
        for item in &entries()[CORE_ENTRY_COUNT..] {
            assert!(!core.contains(item.key), "{}", item.key);
            assert!(item.key.starts_with("screenshot_translation/"), "{}", item.key);
            assert!(crate::schema::is_extension_key(item.key));
        }
        assert!(!crate::schema::is_extension_key("screenshot_translation/model"));
    }

    /// 默认值本身合法且是规范化不动点。
    #[test]
    fn defaults_are_fixed_points() {
        for item in extension_entries() {
            let out = normalize(item.key, &item.default);
            assert!(out.valid && !out.changed, "{}", item.key);
        }
    }

    /// 后端白名单、整数范围与类型校验。
    #[test]
    fn normalization_ranges() {
        assert!(normalize(KEY_TRANSLATION_BACKEND, &json!("openai")).valid);
        assert!(normalize(KEY_TRANSLATION_BACKEND, &json!(" local ")).valid);
        assert!(!normalize(KEY_TRANSLATION_BACKEND, &json!("cloud")).valid);
        assert!(!normalize(KEY_TRANSLATION_BACKEND, &json!(1)).valid);
        for (key, low, high) in [
            (KEY_LOCAL_IDLE_SECONDS, MIN_IDLE_SECONDS, MAX_IDLE_SECONDS),
            (KEY_LOCAL_NUM_BEAMS, 1, MAX_NUM_BEAMS),
        ] {
            assert!(normalize(key, &json!(low)).valid, "{key}");
            assert!(normalize(key, &json!(high)).valid, "{key}");
            assert!(!normalize(key, &json!(low - 1)).valid, "{key}");
            assert!(!normalize(key, &json!(high + 1)).valid, "{key}");
            assert!(!normalize(key, &json!("x")).valid, "{key}");
        }
        assert!(normalize(KEY_LOCAL_LOW_MEMORY, &json!(true)).valid);
        assert!(!normalize(KEY_LOCAL_LOW_MEMORY, &json!("true")).valid);
        assert!(normalize(KEY_LOCAL_MODELS_DIR, &json!("  D:/models  ")).changed);
        assert!(!normalize(KEY_LOCAL_MODEL_ID, &json!(3)).valid);
    }

    /// 缺失时补默认；文档写入非法值被拒绝；合法值落盘后可读回。
    #[test]
    fn document_round_trip() {
        let mut doc = ConfigDocument::from_bytes(None);
        assert_eq!(doc.value(KEY_LOCAL_NUM_BEAMS), json!(DEFAULT_NUM_BEAMS));
        assert_eq!(doc.value(KEY_TRANSLATION_BACKEND), json!(BACKEND_LOCAL));
        assert!(doc.set_value(KEY_LOCAL_NUM_BEAMS, json!(99)).is_err());
        doc.set_value(KEY_LOCAL_NUM_BEAMS, json!(2)).expect("合法束宽");
        doc.set_value(KEY_LOCAL_MODEL_ID, json!("opus-mt-en-zh-int8")).expect("模型 ID");
        let reloaded = ConfigDocument::from_bytes(Some(&doc.to_bytes()));
        assert_eq!(reloaded.value(KEY_LOCAL_NUM_BEAMS), json!(2));
        assert_eq!(reloaded.value(KEY_LOCAL_MODEL_ID), json!("opus-mt-en-zh-int8"));
    }

    /// 扩展项都有 schema 条目，控件所需的范围信息齐全。
    #[test]
    fn schema_lookup_works() {
        assert_eq!(
            entry_for(KEY_LOCAL_IDLE_SECONDS).and_then(|e| e.range).map(|r| r.max),
            Some(MAX_IDLE_SECONDS)
        );
        assert_eq!(entry_for(KEY_TRANSLATION_BACKEND).map(|e| e.allowed.len()), Some(2));
    }
}
