//! Cisox 扩展配置项：不属于 Qt 版 238 项 schema 的新增键。
//!
//! 现有 238 项与 C++ 逐项对齐、不得改动；本地翻译（NMT）相关的新增项集中在这里，放在
//! `screenshot_translation/` 分组下；OCR 后端选择放在 `text_recognition/` 分组下。文档层对未知字段是“保留”的，因此 Qt 版读到这些键只会忽略，
//! 不会破坏磁盘 JSON 的兼容性；Rust 侧则把它们当作正式配置项（有默认值、类型与范围校验）。

use crate::schema::{IntRange, SchemaEntry, ValueKind, entry};
use serde_json::json;

/// 扩展项数量（`schema::entries()` 在原 238 项之后追加的条目数）。
pub const EXTENSION_ENTRY_COUNT: usize = 9;

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

/// 本地多包路由模式：`single` 指定包、`specialized_first` 专用包优先、`mixed_split` 混合拆分。
pub const KEY_LOCAL_ROUTE_MODE: &str = "screenshot_translation/local_route_mode";
/// 本地最多同时常驻内存的翻译包个数（要加载新包时先卸载空闲的旧包）。
pub const KEY_LOCAL_MAX_RESIDENT: &str = "screenshot_translation/local_max_resident_models";

/// OCR 后端：`system` 系统原生 OCR，`local-model` 本地模型（snow-ocr-process）。
pub const KEY_OCR_BACKEND: &str = "text_recognition/backend";
/// OCR 后端取值：系统原生 OCR。
pub const OCR_BACKEND_SYSTEM: &str = "system";
/// OCR 后端取值：本地模型。
pub const OCR_BACKEND_LOCAL_MODEL: &str = "local-model";
/// OCR 后端白名单。
const OCR_BACKEND_VALUES: &[&str] = &[OCR_BACKEND_SYSTEM, OCR_BACKEND_LOCAL_MODEL];

/// 全新安装与配置损坏时的 OCR 后端默认值：所有平台一致为 `local-model`。
///
/// 待办（P1）：系统后端真正可用、并与 PP-OCR 做完同图对比后，再把新用户默认值切到 `system`
/// （已决定的方向不变）；在那之前默认走一个未实现的后端只会靠回落兜底，没有收益。
/// 老用户（磁盘配置里没有该键）由文档层迁移为 `local-model`，与此默认值无关。
///
/// # 返回
/// 后端取值字符串。
///
/// # 示例
/// ```
/// assert_eq!(snow_config::extensions::default_ocr_backend(), "local-model");
/// ```
pub const fn default_ocr_backend() -> &'static str {
    OCR_BACKEND_LOCAL_MODEL
}

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
/// 束宽默认值（评测推荐 2：与清单 `m2m100` 缺省一致，应用侧默认值不再覆盖清单）。
pub const DEFAULT_NUM_BEAMS: i32 = 2;
/// 束宽上限（与 worker 的 `MAX_BEAMS` 一致）。
pub const MAX_NUM_BEAMS: i32 = 8;

/// 路由模式取值：用户指定的包一律照用。
pub const ROUTE_SINGLE: &str = "single";
/// 路由模式取值：未指定包时，优先选显式声明语言对的专用包（默认）。
pub const ROUTE_SPECIALIZED_FIRST: &str = "specialized_first";
/// 路由模式取值：文本拆成单语片段，英文走专用包、其余走通用包。
pub const ROUTE_MIXED_SPLIT: &str = "mixed_split";
/// 路由模式白名单。
const ROUTE_MODE_VALUES: &[&str] = &[ROUTE_SINGLE, ROUTE_SPECIALIZED_FIRST, ROUTE_MIXED_SPLIT];
/// 最大同时常驻包数默认值（低内存优先：同一时刻只留一个模型在内存）。
pub const DEFAULT_MAX_RESIDENT: i32 = 1;
/// 最大同时常驻包数上限。
pub const MAX_MAX_RESIDENT: i32 = 4;

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
        entry(KEY_LOCAL_LOW_MEMORY, json!(false), ValueKind::Boolean, None, &[], None),
        entry(KEY_LOCAL_ROUTE_MODE, json!(ROUTE_SPECIALIZED_FIRST), ValueKind::String, None, ROUTE_MODE_VALUES, None),
        entry(
            KEY_LOCAL_MAX_RESIDENT,
            json!(DEFAULT_MAX_RESIDENT),
            ValueKind::Integer,
            Some(IntRange { min: DEFAULT_MAX_RESIDENT, max: MAX_MAX_RESIDENT, step: 1 }),
            &[],
            None,
        ),
        entry(KEY_OCR_BACKEND, json!(default_ocr_backend()), ValueKind::String, None, OCR_BACKEND_VALUES, None),
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
            assert!(
                item.key.starts_with("screenshot_translation/") || item.key == KEY_OCR_BACKEND,
                "{}",
                item.key
            );
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
            (KEY_LOCAL_MAX_RESIDENT, 1, MAX_MAX_RESIDENT),
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

    /// 路由模式：默认专用包优先；三个合法值通过，未知值与非字符串被拒绝，写入后可读回。
    #[test]
    fn route_mode_whitelist_and_default() {
        let mut doc = ConfigDocument::from_bytes(None);
        assert_eq!(doc.value(KEY_LOCAL_ROUTE_MODE), json!(ROUTE_SPECIALIZED_FIRST));
        assert_eq!(doc.value(KEY_LOCAL_MAX_RESIDENT), json!(DEFAULT_MAX_RESIDENT));
        for mode in [ROUTE_SINGLE, ROUTE_SPECIALIZED_FIRST, ROUTE_MIXED_SPLIT] {
            assert!(normalize(KEY_LOCAL_ROUTE_MODE, &json!(mode)).valid, "{mode}");
        }
        assert!(!normalize(KEY_LOCAL_ROUTE_MODE, &json!("auto")).valid);
        assert!(!normalize(KEY_LOCAL_ROUTE_MODE, &json!(2)).valid);
        assert!(doc.set_value(KEY_LOCAL_ROUTE_MODE, json!("auto")).is_err());
        doc.set_value(KEY_LOCAL_ROUTE_MODE, json!(ROUTE_MIXED_SPLIT)).expect("合法模式");
        doc.set_value(KEY_LOCAL_MAX_RESIDENT, json!(2)).expect("合法常驻数");
        let reloaded = ConfigDocument::from_bytes(Some(&doc.to_bytes()));
        assert_eq!(reloaded.value(KEY_LOCAL_ROUTE_MODE), json!(ROUTE_MIXED_SPLIT));
        assert_eq!(reloaded.value(KEY_LOCAL_MAX_RESIDENT), json!(2));
    }

    /// 缺失时补默认；文档写入非法值被拒绝；合法值落盘后可读回。
    #[test]
    fn document_round_trip() {
        let mut doc = ConfigDocument::from_bytes(None);
        assert_eq!(doc.value(KEY_LOCAL_NUM_BEAMS), json!(DEFAULT_NUM_BEAMS));
        // 默认束宽与清单 m2m100 缺省一致，应用侧不再用 4 覆盖清单
        assert_eq!(DEFAULT_NUM_BEAMS, 2);
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
        assert_eq!(entry_for(KEY_OCR_BACKEND).map(|e| e.allowed.len()), Some(2));
    }

    /// OCR 后端白名单：合法值通过（含首尾空白修剪），未知值与非字符串被拒绝，写入后可读回。
    #[test]
    fn ocr_backend_normalization_and_round_trip() {
        assert!(normalize(KEY_OCR_BACKEND, &json!("system")).valid);
        assert!(normalize(KEY_OCR_BACKEND, &json!(" local-model ")).changed);
        assert!(!normalize(KEY_OCR_BACKEND, &json!("remote-api")).valid);
        assert!(!normalize(KEY_OCR_BACKEND, &json!(true)).valid);
        let mut doc = ConfigDocument::from_bytes(None);
        assert_eq!(doc.value(KEY_OCR_BACKEND), json!(default_ocr_backend()));
        assert!(doc.set_value(KEY_OCR_BACKEND, json!("cloud")).is_err());
        doc.set_value(KEY_OCR_BACKEND, json!(OCR_BACKEND_LOCAL_MODEL)).expect("合法后端");
        let reloaded = ConfigDocument::from_bytes(Some(&doc.to_bytes()));
        assert_eq!(reloaded.value(KEY_OCR_BACKEND), json!(OCR_BACKEND_LOCAL_MODEL));
    }

    /// 配置损坏（RecoveredDefaults）回落到默认值，同样是 local-model。
    #[test]
    fn ocr_backend_corrupt_config_uses_default() {
        let doc = ConfigDocument::from_bytes(Some(b"{not json"));
        assert_eq!(doc.compatibility(), crate::document::Compatibility::RecoveredDefaults);
        assert_eq!(doc.value(KEY_OCR_BACKEND), json!(OCR_BACKEND_LOCAL_MODEL));
    }

    /// 旧配置迁移：磁盘文件存在但没有该键 = 老用户 -> local-model；全新安装默认 local-model；已有值原样保留。
    #[test]
    fn ocr_backend_migration_for_existing_users() {
        let fresh = ConfigDocument::from_bytes(None);
        assert_eq!(fresh.value(KEY_OCR_BACKEND), json!(OCR_BACKEND_LOCAL_MODEL));

        // 取一份全新文档，抹掉后端键，模拟升级前写出的旧配置文件
        let mut legacy: serde_json::Value = serde_json::from_slice(&fresh.to_bytes()).expect("json");
        legacy["text_recognition"].as_object_mut().expect("分组").remove("backend");
        let bytes = serde_json::to_vec(&legacy).expect("序列化");
        let migrated = ConfigDocument::from_bytes(Some(&bytes));
        assert_eq!(migrated.value(KEY_OCR_BACKEND), json!(OCR_BACKEND_LOCAL_MODEL));
        assert!(migrated.is_dirty(), "迁移结果应落盘");
        let text = String::from_utf8(migrated.to_bytes()).expect("utf8");
        assert!(text.contains("\"backend\": \"local-model\""), "{text}");

        // 已经写了值的配置不被迁移覆盖
        let mut explicit = legacy.clone();
        explicit["text_recognition"]["backend"] = json!(OCR_BACKEND_SYSTEM);
        let kept = ConfigDocument::from_bytes(Some(&serde_json::to_vec(&explicit).expect("序列化")));
        assert_eq!(kept.value(KEY_OCR_BACKEND), json!(OCR_BACKEND_SYSTEM));
    }
}
