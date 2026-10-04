//! Cisox 扩展配置项：不属于 Qt 版 238 项 schema 的新增键。
//!
//! 现有 238 项与 C++ 逐项对齐、不得改动；本地翻译（NMT）相关的新增项集中在这里，放在
//! `screenshot_translation/` 分组下；OCR 后端选择放在 `text_recognition/` 分组下；语音转文字放在 `dictation/` 分组下。文档层对未知字段是“保留”的，因此 Qt 版读到这些键只会忽略，
//! 不会破坏磁盘 JSON 的兼容性；Rust 侧则把它们当作正式配置项（有默认值、类型与范围校验）。

use crate::schema::{IntRange, SchemaEntry, ValueKind, entry};
use serde_json::json;

/// 扩展项数量（`schema::entries()` 在原 238 项之后追加的条目数）。
pub const EXTENSION_ENTRY_COUNT: usize = 26;

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

/// 输入框翻译浮窗的全局热键（默认不绑定，未绑定时不注册）。
pub const KEY_TRANSLATE_INPUT_HOTKEY: &str = "global_shortcuts/translate_input";

/// 语音转文字「切换式」全局热键（按一下开始、再按一下结束；默认不绑定）。
pub const KEY_DICTATION_TOGGLE_HOTKEY: &str = "global_shortcuts/dictation_toggle";
/// 语音转文字「按住说话」全局热键（按下开始、松开结束；默认不绑定）。
pub const KEY_DICTATION_HOLD_HOTKEY: &str = "global_shortcuts/dictation_hold";
/// 语音转文字引擎后端：`local-model` 本地模型（snow-stt），`system` 系统语音（尚未实现）。
pub const KEY_DICTATION_BACKEND: &str = "dictation/backend";
/// 语音转文字触发模式：`both` 两个热键都生效，`toggle` 只切换式，`hold` 只按住说话。
pub const KEY_DICTATION_TRIGGER_MODE: &str = "dictation/trigger_mode";
/// 语音转文字模型目录；空串表示 `<数据根>/models/stt`。
pub const KEY_DICTATION_MODEL_DIR: &str = "dictation/model_dir";
/// 语音转文字的语言提示（单词，如 `auto`、`zh-en`）。
pub const KEY_DICTATION_LANGUAGE: &str = "dictation/language";
/// 语音转文字推理线程数。
pub const KEY_DICTATION_THREADS: &str = "dictation/threads";
/// 语音转文字识别模式：`streaming` 边说边出字，`offline` 按 VAD 切句后整句识别（更准、延迟更高）。
pub const KEY_DICTATION_RECOGNITION_MODE: &str = "dictation/mode";
/// 语音转文字语言维度：`zh` 中文、`en` 英文、`bilingual` 中英混合。
pub const KEY_DICTATION_LANGUAGE_DIMENSION: &str = "dictation/language_dimension";
/// 语音转文字所选模型 ID；空串表示该维度与模式的默认模型，非空表示用户选了备选。
pub const KEY_DICTATION_MODEL_ID: &str = "dictation/model_id";
/// SenseVoice 是否启用逆文本规整（带标点与数字格式化），仅对该模型生效。
pub const KEY_DICTATION_SENSEVOICE_ITN: &str = "dictation/sensevoice_itn";
/// 语音转文字定稿句是否同时翻译（级联已接入，见 `dictation/translate.rs`）。
pub const KEY_DICTATION_TRANSLATE_ENABLED: &str = "dictation/translate_enabled";
/// 语音转文字翻译目标语言：`auto` 自动取另一语种，`zh-Hans` 简体中文，`en` 英文。
pub const KEY_DICTATION_TRANSLATE_TARGET: &str = "dictation/translate_target";
/// 语音转文字输出方式：`auto` 有可输入焦点就键入、否则弹浮窗，`type` 只键入，`overlay` 只浮窗。
pub const KEY_DICTATION_OUTPUT_MODE: &str = "dictation/output_mode";
/// 键入时是否同时显示浮窗。
pub const KEY_DICTATION_TYPE_WITH_OVERLAY: &str = "dictation/type_with_overlay";
/// 单次语音转文字最长秒数（0 为不限），超时自动结束。
pub const KEY_DICTATION_MAX_SECONDS: &str = "dictation/max_seconds";

/// 语音转文字后端取值：本地模型。
pub const DICTATION_BACKEND_LOCAL_MODEL: &str = "local-model";
/// 语音转文字后端取值：系统语音（占位，尚未实现）。
pub const DICTATION_BACKEND_SYSTEM: &str = "system";
/// 语音转文字后端白名单。
const DICTATION_BACKEND_VALUES: &[&str] =
    &[DICTATION_BACKEND_LOCAL_MODEL, DICTATION_BACKEND_SYSTEM];
/// 触发模式取值：两个热键都生效。
pub const DICTATION_MODE_BOTH: &str = "both";
/// 触发模式取值：只启用切换式热键。
pub const DICTATION_MODE_TOGGLE: &str = "toggle";
/// 触发模式取值：只启用按住说话热键。
pub const DICTATION_MODE_HOLD: &str = "hold";
/// 触发模式白名单。
const DICTATION_MODE_VALUES: &[&str] = &[
    DICTATION_MODE_BOTH,
    DICTATION_MODE_TOGGLE,
    DICTATION_MODE_HOLD,
];
/// 识别模式取值：流式。
pub const DICTATION_RECOGNITION_STREAMING: &str = "streaming";
/// 识别模式取值：离线（VAD 切句）。
pub const DICTATION_RECOGNITION_OFFLINE: &str = "offline";
/// 识别模式白名单。
const DICTATION_RECOGNITION_VALUES: &[&str] = &[
    DICTATION_RECOGNITION_STREAMING,
    DICTATION_RECOGNITION_OFFLINE,
];
/// 语言维度取值：中文。
pub const DICTATION_DIMENSION_ZH: &str = "zh";
/// 语言维度取值：英文。
pub const DICTATION_DIMENSION_EN: &str = "en";
/// 语言维度取值：中英混合。
pub const DICTATION_DIMENSION_BILINGUAL: &str = "bilingual";
/// 语言维度白名单。
const DICTATION_DIMENSION_VALUES: &[&str] = &[
    DICTATION_DIMENSION_ZH,
    DICTATION_DIMENSION_EN,
    DICTATION_DIMENSION_BILINGUAL,
];
/// 翻译目标取值：自动。
pub const DICTATION_TARGET_AUTO: &str = "auto";
/// 翻译目标取值：简体中文。
pub const DICTATION_TARGET_ZH_HANS: &str = "zh-Hans";
/// 翻译目标取值：英文。
pub const DICTATION_TARGET_EN: &str = "en";
/// 翻译目标白名单。
const DICTATION_TARGET_VALUES: &[&str] = &[
    DICTATION_TARGET_AUTO,
    DICTATION_TARGET_ZH_HANS,
    DICTATION_TARGET_EN,
];
/// 输出方式取值：自动。
pub const DICTATION_OUTPUT_AUTO: &str = "auto";
/// 输出方式取值：只键入。
pub const DICTATION_OUTPUT_TYPE: &str = "type";
/// 输出方式取值：只浮窗。
pub const DICTATION_OUTPUT_OVERLAY: &str = "overlay";
/// 输出方式白名单。
const DICTATION_OUTPUT_VALUES: &[&str] = &[
    DICTATION_OUTPUT_AUTO,
    DICTATION_OUTPUT_TYPE,
    DICTATION_OUTPUT_OVERLAY,
];
/// 语音语言提示默认值。
pub const DEFAULT_DICTATION_LANGUAGE: &str = "auto";
/// 推理线程数默认值（低占用优先）。
pub const DEFAULT_DICTATION_THREADS: i32 = 2;
/// 推理线程数上限。
pub const MAX_DICTATION_THREADS: i32 = 8;
/// 单次最长秒数默认值。
pub const DEFAULT_DICTATION_MAX_SECONDS: i32 = 120;
/// 单次最长秒数上限（1 小时）。
pub const MAX_DICTATION_MAX_SECONDS: i32 = 3600;
/// 单次最长秒数的界面步长。
const DICTATION_MAX_SECONDS_STEP: i32 = 10;

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
        entry(KEY_TRANSLATE_INPUT_HOTKEY, json!([]), ValueKind::StringList, None, &[], Some(2)),
        entry(KEY_DICTATION_TOGGLE_HOTKEY, json!([]), ValueKind::StringList, None, &[], Some(2)),
        entry(KEY_DICTATION_HOLD_HOTKEY, json!([]), ValueKind::StringList, None, &[], Some(2)),
        entry(KEY_DICTATION_BACKEND, json!(DICTATION_BACKEND_LOCAL_MODEL), ValueKind::String, None, DICTATION_BACKEND_VALUES, None),
        entry(KEY_DICTATION_TRIGGER_MODE, json!(DICTATION_MODE_BOTH), ValueKind::String, None, DICTATION_MODE_VALUES, None),
        entry(KEY_DICTATION_RECOGNITION_MODE, json!(DICTATION_RECOGNITION_STREAMING), ValueKind::String, None, DICTATION_RECOGNITION_VALUES, None),
        entry(KEY_DICTATION_LANGUAGE_DIMENSION, json!(DICTATION_DIMENSION_BILINGUAL), ValueKind::String, None, DICTATION_DIMENSION_VALUES, None),
        entry(KEY_DICTATION_MODEL_ID, json!(""), ValueKind::String, None, &[], None),
        entry(KEY_DICTATION_SENSEVOICE_ITN, json!(true), ValueKind::Boolean, None, &[], None),
        entry(KEY_DICTATION_TRANSLATE_ENABLED, json!(false), ValueKind::Boolean, None, &[], None),
        entry(KEY_DICTATION_TRANSLATE_TARGET, json!(DICTATION_TARGET_AUTO), ValueKind::String, None, DICTATION_TARGET_VALUES, None),
        entry(KEY_DICTATION_MODEL_DIR, json!(""), ValueKind::String, None, &[], None),
        entry(KEY_DICTATION_LANGUAGE, json!(DEFAULT_DICTATION_LANGUAGE), ValueKind::String, None, &[], None),
        entry(
            KEY_DICTATION_THREADS,
            json!(DEFAULT_DICTATION_THREADS),
            ValueKind::Integer,
            Some(IntRange { min: 1, max: MAX_DICTATION_THREADS, step: 1 }),
            &[],
            None,
        ),
        entry(KEY_DICTATION_OUTPUT_MODE, json!(DICTATION_OUTPUT_AUTO), ValueKind::String, None, DICTATION_OUTPUT_VALUES, None),
        entry(KEY_DICTATION_TYPE_WITH_OVERLAY, json!(false), ValueKind::Boolean, None, &[], None),
        entry(
            KEY_DICTATION_MAX_SECONDS,
            json!(DEFAULT_DICTATION_MAX_SECONDS),
            ValueKind::Integer,
            Some(IntRange { min: 0, max: MAX_DICTATION_MAX_SECONDS, step: DICTATION_MAX_SECONDS_STEP }),
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
            assert!(
                item.key.starts_with("screenshot_translation/")
                    || item.key == KEY_OCR_BACKEND
                    || item.key == KEY_TRANSLATE_INPUT_HOTKEY
                    || item.key == KEY_DICTATION_TOGGLE_HOTKEY
                    || item.key == KEY_DICTATION_HOLD_HOTKEY
                    || item.key.starts_with("dictation/"),
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

    /// 输入框翻译热键：默认未绑定，可写入并读回，超过 2 个绑定被拒绝；旧配置缺该键时补默认。
    #[test]
    fn translate_input_hotkey_default_and_round_trip() {
        let mut doc = ConfigDocument::from_bytes(None);
        assert_eq!(doc.value(KEY_TRANSLATE_INPUT_HOTKEY), json!([]));
        assert!(crate::schema::is_extension_key(KEY_TRANSLATE_INPUT_HOTKEY));
        doc.set_value(KEY_TRANSLATE_INPUT_HOTKEY, json!(["Ctrl+Alt+T"])).expect("合法热键");
        let reloaded = ConfigDocument::from_bytes(Some(&doc.to_bytes()));
        let shown = reloaded.value(KEY_TRANSLATE_INPUT_HOTKEY).to_string();
        assert!(shown.contains("Ctrl+Alt+T"), "{shown}");
        // 超过上限的绑定不会原样保留（拒绝或截断）
        let _ = doc.set_value(KEY_TRANSLATE_INPUT_HOTKEY, json!(["F1", "F2", "F3"]));
        let kept = doc.value(KEY_TRANSLATE_INPUT_HOTKEY);
        assert!(kept.as_array().is_some_and(|a| a.len() <= 2), "{kept}");

        // 旧配置（没有该键）读到默认的空绑定
        let fresh = ConfigDocument::from_bytes(None);
        let mut legacy: serde_json::Value = serde_json::from_slice(&fresh.to_bytes()).expect("json");
        legacy["global_shortcuts"].as_object_mut().expect("分组").remove("translate_input");
        let old = ConfigDocument::from_bytes(Some(&serde_json::to_vec(&legacy).expect("序列化")));
        assert_eq!(old.value(KEY_TRANSLATE_INPUT_HOTKEY), json!([]));
    }

    /// 语音转文字：默认值、白名单与范围校验；两个热键默认不绑定。
    #[test]
    fn dictation_defaults_and_validation() {
        let doc = ConfigDocument::from_bytes(None);
        assert_eq!(doc.value(KEY_DICTATION_TOGGLE_HOTKEY), json!([]));
        assert_eq!(doc.value(KEY_DICTATION_HOLD_HOTKEY), json!([]));
        assert_eq!(doc.value(KEY_DICTATION_BACKEND), json!(DICTATION_BACKEND_LOCAL_MODEL));
        assert_eq!(doc.value(KEY_DICTATION_TRIGGER_MODE), json!(DICTATION_MODE_BOTH));
        assert_eq!(doc.value(KEY_DICTATION_OUTPUT_MODE), json!(DICTATION_OUTPUT_AUTO));
        assert_eq!(doc.value(KEY_DICTATION_TYPE_WITH_OVERLAY), json!(false));
        assert_eq!(doc.value(KEY_DICTATION_MODEL_DIR), json!(""));
        assert_eq!(doc.value(KEY_DICTATION_THREADS), json!(DEFAULT_DICTATION_THREADS));
        assert_eq!(doc.value(KEY_DICTATION_MAX_SECONDS), json!(DEFAULT_DICTATION_MAX_SECONDS));
        for (key, good, bad) in [
            (KEY_DICTATION_BACKEND, DICTATION_BACKEND_SYSTEM, "cloud"),
            (KEY_DICTATION_TRIGGER_MODE, DICTATION_MODE_HOLD, "double"),
            (KEY_DICTATION_OUTPUT_MODE, DICTATION_OUTPUT_OVERLAY, "paste"),
        ] {
            assert!(normalize(key, &json!(good)).valid, "{key}");
            assert!(!normalize(key, &json!(bad)).valid, "{key}");
            assert!(!normalize(key, &json!(1)).valid, "{key}");
        }
        assert!(normalize(KEY_DICTATION_THREADS, &json!(1)).valid);
        assert!(normalize(KEY_DICTATION_THREADS, &json!(MAX_DICTATION_THREADS)).valid);
        assert!(!normalize(KEY_DICTATION_THREADS, &json!(0)).valid);
        assert!(!normalize(KEY_DICTATION_THREADS, &json!(MAX_DICTATION_THREADS + 1)).valid);
        assert!(normalize(KEY_DICTATION_MAX_SECONDS, &json!(0)).valid);
        assert!(!normalize(KEY_DICTATION_MAX_SECONDS, &json!(MAX_DICTATION_MAX_SECONDS + 1)).valid);
        assert!(!normalize(KEY_DICTATION_TYPE_WITH_OVERLAY, &json!("yes")).valid);
    }

    /// 语音转文字：热键可读写；旧配置（没有 dictation 分组与这两个热键）读到默认值。
    #[test]
    fn dictation_round_trip_and_legacy_fill() {
        let mut doc = ConfigDocument::from_bytes(None);
        doc.set_value(KEY_DICTATION_TOGGLE_HOTKEY, json!(["Ctrl+Alt+D"])).expect("合法热键");
        doc.set_value(KEY_DICTATION_HOLD_HOTKEY, json!(["F9"])).expect("合法热键");
        doc.set_value(KEY_DICTATION_OUTPUT_MODE, json!(DICTATION_OUTPUT_TYPE)).expect("合法输出方式");
        doc.set_value(KEY_DICTATION_MODEL_DIR, json!("D:/models/stt")).expect("目录");
        let reloaded = ConfigDocument::from_bytes(Some(&doc.to_bytes()));
        assert!(reloaded.value(KEY_DICTATION_TOGGLE_HOTKEY).to_string().contains("Ctrl+Alt+D"));
        assert!(reloaded.value(KEY_DICTATION_HOLD_HOTKEY).to_string().contains("F9"));
        assert_eq!(reloaded.value(KEY_DICTATION_OUTPUT_MODE), json!(DICTATION_OUTPUT_TYPE));
        assert_eq!(reloaded.value(KEY_DICTATION_MODEL_DIR), json!("D:/models/stt"));

        let fresh = ConfigDocument::from_bytes(None);
        let mut legacy: serde_json::Value = serde_json::from_slice(&fresh.to_bytes()).expect("json");
        legacy.as_object_mut().expect("根").remove("dictation");
        legacy["global_shortcuts"].as_object_mut().expect("分组").remove("dictation_toggle");
        legacy["global_shortcuts"].as_object_mut().expect("分组").remove("dictation_hold");
        let old = ConfigDocument::from_bytes(Some(&serde_json::to_vec(&legacy).expect("序列化")));
        assert_eq!(old.value(KEY_DICTATION_TOGGLE_HOTKEY), json!([]));
        assert_eq!(old.value(KEY_DICTATION_HOLD_HOTKEY), json!([]));
        assert_eq!(old.value(KEY_DICTATION_BACKEND), json!(DICTATION_BACKEND_LOCAL_MODEL));
        assert_eq!(old.value(KEY_DICTATION_THREADS), json!(DEFAULT_DICTATION_THREADS));
    }
}
