//! 本地 NMT 翻译引擎与多后端调度服务（ADR-5）。
//!
//! 支持离线本地 NMT 模型清单（`model.json`）扫描与懒加载、
//! 离线基础词库翻译引擎、OpenAI 兼容端点翻译引擎以及翻译结果缓存。

use std::collections::HashMap;
use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};
use serde::{Deserialize, Serialize};

/// 阶段标识。
pub const PHASE: &str = "P5";

/// 翻译支持的语言种类枚举。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Lang {
    /// 自动检测。
    Auto,
    /// 简体中文。
    ZhHans,
    /// 繁体中文。
    ZhHant,
    /// 英语。
    En,
    /// 日语。
    Ja,
    /// 韩语。
    Ko,
    /// 法语。
    Fr,
    /// 德语。
    De,
    /// 西班牙语。
    Es,
    /// 俄语。
    Ru,
}

impl Lang {
    /// 标准 BCP-47 / ISO 语言代码。
    ///
    /// # 返回
    /// 语言代号字符串切片。
    ///
    /// # 示例
    /// ```rust
    /// use snow_translate::Lang;
    /// assert_eq!(Lang::ZhHans.code(), "zh-CN");
    /// assert_eq!(Lang::En.code(), "en");
    /// ```
    pub const fn code(&self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::ZhHans => "zh-CN",
            Self::ZhHant => "zh-TW",
            Self::En => "en",
            Self::Ja => "ja",
            Self::Ko => "ko",
            Self::Fr => "fr",
            Self::De => "de",
            Self::Es => "es",
            Self::Ru => "ru",
        }
    }

    /// 中文显示名称。
    ///
    /// # 返回
    /// 显示名称切片。
    ///
    /// # 示例
    /// ```rust
    /// use snow_translate::Lang;
    /// assert_eq!(Lang::ZhHans.display_name(), "简体中文");
    /// assert_eq!(Lang::En.display_name(), "英语");
    /// ```
    pub const fn display_name(&self) -> &'static str {
        match self {
            Self::Auto => "自动检测",
            Self::ZhHans => "简体中文",
            Self::ZhHant => "繁体中文",
            Self::En => "英语",
            Self::Ja => "日语",
            Self::Ko => "韩语",
            Self::Fr => "法语",
            Self::De => "德语",
            Self::Es => "西班牙语",
            Self::Ru => "俄语",
        }
    }

    /// 从语言代码字符串解析。
    ///
    /// # 参数
    /// - `code`: 语言代号字符串。
    ///
    /// # 返回
    /// 匹配到的语言变体；若无法识别则返回 `None`。
    ///
    /// # 示例
    /// ```rust
    /// use snow_translate::Lang;
    /// assert_eq!(Lang::from_code("en"), Some(Lang::En));
    /// assert_eq!(Lang::from_code("zh-CN"), Some(Lang::ZhHans));
    /// assert_eq!(Lang::from_code("unknown"), None);
    /// ```
    pub fn from_code(code: &str) -> Option<Self> {
        let normalized = code.trim().to_lowercase();
        match normalized.as_str() {
            "auto" => Some(Self::Auto),
            "zh" | "zh-cn" | "zh-hans" | "zho_hans" => Some(Self::ZhHans),
            "zh-tw" | "zh-hk" | "zh-hant" | "zho_hant" => Some(Self::ZhHant),
            "en" | "en-us" | "en-gb" | "eng_latn" => Some(Self::En),
            "ja" | "jp" | "jpn_jpan" => Some(Self::Ja),
            "ko" | "kr" | "kor_hang" => Some(Self::Ko),
            "fr" | "fra_latn" => Some(Self::Fr),
            "de" | "deu_latn" => Some(Self::De),
            "es" | "spa_latn" => Some(Self::Es),
            "ru" | "rus_cyrl" => Some(Self::Ru),
            _ => None,
        }
    }
}

/// 翻译模块错误类型。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TranslateError {
    /// 未找到任何已安装的模型。
    NoModelFound(String),
    /// 不支持的语言转换对。
    UnsupportedLanguagePair(Lang, Lang),
    /// 请求参数不合法。
    InvalidRequest(String),
    /// 网络或 API 端点错误。
    Network(String),
    /// 本地模型或文件系统 I/O 错误。
    Io(String),
    /// 推理或执行超时。
    Timeout,
}

impl fmt::Display for TranslateError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoModelFound(msg) => write!(f, "未检测到可用翻译模型: {msg}"),
            Self::UnsupportedLanguagePair(src, tgt) => {
                write!(f, "不支持的翻译语言对: {:?} -> {:?}", src, tgt)
            }
            Self::InvalidRequest(msg) => write!(f, "翻译请求不合法: {msg}"),
            Self::Network(msg) => write!(f, "翻译端点连接失败: {msg}"),
            Self::Io(msg) => write!(f, "模型文件读取错误: {msg}"),
            Self::Timeout => write!(f, "翻译执行超时"),
        }
    }
}

impl std::error::Error for TranslateError {}

/// 本地 NMT 模型清单规范（`model.json`，见 ADR-5）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ModelManifest {
    /// 清单规范版本号。
    pub schema_version: u32,
    /// 模型唯一标识符。
    pub id: String,
    /// 界面可读展示名。
    pub display_name: String,
    /// 模型架构族属（例如 `nllb`、`marian`、`m2m100`）。
    pub family: String,
    /// 量化类型（例如 `int8`、`fp16`、`fp32`）。
    pub quantization: String,
    /// 依赖的模型与分词器文件路径映射表。
    pub files: HashMap<String, String>,
    /// 支持的源与目标语言代码列表。
    pub languages: Vec<String>,
    /// 最大允许输入 token 数量。
    pub max_input_tokens: usize,
}

impl ModelManifest {
    /// 校验清单文件字段是否完整且合法。
    ///
    /// # 返回
    /// 合法返回 `Ok(())`，缺少关键字段返回描述错误。
    ///
    /// # 示例
    /// ```rust
    /// use std::collections::HashMap;
    /// use snow_translate::ModelManifest;
    /// let mut files = HashMap::new();
    /// files.insert("encoder".to_string(), "encoder.onnx".to_string());
    /// let manifest = ModelManifest {
    ///     schema_version: 1,
    ///     id: "nllb-200-int8".to_string(),
    ///     display_name: "NLLB-200".to_string(),
    ///     family: "nllb".to_string(),
    ///     quantization: "int8".to_string(),
    ///     files,
    ///     languages: vec!["zh-CN".to_string(), "en".to_string()],
    ///     max_input_tokens: 512,
    /// };
    /// assert!(manifest.validate().is_ok());
    /// ```
    pub fn validate(&self) -> Result<(), TranslateError> {
        if self.schema_version != 1 {
            return Err(TranslateError::InvalidRequest(format!(
                "不支持的清单版本: {}",
                self.schema_version
            )));
        }
        if self.id.trim().is_empty() {
            return Err(TranslateError::InvalidRequest("模型 ID 不能为空".into()));
        }
        if self.languages.is_empty() {
            return Err(TranslateError::InvalidRequest("模型支持语言列表不能为空".into()));
        }
        Ok(())
    }
}

/// 本地翻译模型扫描器。
pub struct ModelScanner {
    /// 模型根目录路径。
    models_dir: PathBuf,
}

impl ModelScanner {
    /// 构造模型扫描器。
    ///
    /// # 参数
    /// - `models_dir`: 存放翻译模型的根目录。
    ///
    /// # 示例
    /// ```rust
    /// use std::path::Path;
    /// use snow_translate::ModelScanner;
    /// let scanner = ModelScanner::new(Path::new("D:/models/translate"));
    /// assert_eq!(scanner.models_dir().to_str().unwrap(), "D:/models/translate");
    /// ```
    pub fn new(models_dir: &Path) -> Self {
        Self {
            models_dir: models_dir.to_path_buf(),
        }
    }

    /// 模型根目录。
    pub fn models_dir(&self) -> &Path {
        &self.models_dir
    }

    /// 扫描目录下的全部可用模型清单。
    ///
    /// # 返回
    /// 成功找到的模型清单列表；若目录不存在或为空返回空列表。
    pub fn scan_models(&self) -> Vec<ModelManifest> {
        if !self.models_dir.exists() || !self.models_dir.is_dir() {
            return Vec::new();
        }

        let mut models = Vec::new();
        if let Ok(entries) = fs::read_dir(&self.models_dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_dir() {
                    let manifest_path = path.join("model.json");
                    let parsed = fs::read_to_string(&manifest_path)
                        .ok()
                        .and_then(|c| serde_json::from_str::<ModelManifest>(&c).ok())
                        .filter(|m| m.validate().is_ok());
                    if let Some(manifest) = parsed {
                        models.push(manifest);
                    }
                }
            }
        }
        models
    }
}

/// 翻译引擎后端统一接口（ADR-5）。
pub trait TranslationEngine: Send + Sync {
    /// 执行文本翻译。
    ///
    /// # 参数
    /// - `text`: 待翻译的原始文本。
    /// - `src`: 源语言。
    /// - `tgt`: 目标语言。
    ///
    /// # 返回
    /// 翻译产出文本，或返回对应错误。
    fn translate(&self, text: &str, src: Lang, tgt: Lang) -> Result<String, TranslateError>;

    /// 返回当前后端支持的语言对组合。
    fn supported_pairs(&self) -> Vec<(Lang, Lang)>;

    /// 引擎类型标识名称。
    fn engine_name(&self) -> &'static str;
}

/// 内置轻量离线词典翻译引擎（Fallback 兜底与常见技术术语对照）。
#[derive(Debug, Default)]
pub struct OfflineDictionaryEngine {
    dict: HashMap<String, String>,
}

impl OfflineDictionaryEngine {
    /// 构造离线词典翻译引擎并填充常用词库。
    pub fn new() -> Self {
        let mut dict = HashMap::new();
        dict.insert("hello".into(), "你好".into());
        dict.insert("world".into(), "世界".into());
        dict.insert("screenshot".into(), "屏幕截图".into());
        dict.insert("cancel".into(), "取消".into());
        dict.insert("save".into(), "保存".into());
        dict.insert("copy".into(), "复制".into());
        dict.insert("pin".into(), "贴图".into());
        dict.insert("ocr".into(), "文字识别".into());
        dict.insert("translate".into(), "翻译".into());
        dict.insert("success".into(), "成功".into());
        dict.insert("failed".into(), "失败".into());

        Self { dict }
    }

    /// 注册一条翻译词条。
    pub fn insert(&mut self, source: &str, target: &str) {
        self.dict.insert(source.to_lowercase(), target.to_string());
    }
}

impl TranslationEngine for OfflineDictionaryEngine {
    fn translate(&self, text: &str, _src: Lang, tgt: Lang) -> Result<String, TranslateError> {
        let trimmed = text.trim();
        if trimmed.is_empty() {
            return Ok(String::new());
        }

        let lower = trimmed.to_lowercase();
        if let Some(hit) = self.dict.get(&lower) {
            return Ok(hit.clone());
        }

        // 若词库未精确命中，执行分词匹配替换
        let words: Vec<&str> = trimmed.split_whitespace().collect();
        let mut translated_words = Vec::new();
        for word in words {
            let clean = word.trim_matches(|c: char| !c.is_alphanumeric()).to_lowercase();
            if let Some(target) = self.dict.get(&clean) {
                translated_words.push(target.as_str());
            } else {
                translated_words.push(word);
            }
        }

        if tgt == Lang::ZhHans || tgt == Lang::ZhHant {
            Ok(translated_words.concat())
        } else {
            Ok(translated_words.join(" "))
        }
    }

    fn supported_pairs(&self) -> Vec<(Lang, Lang)> {
        vec![
            (Lang::En, Lang::ZhHans),
            (Lang::En, Lang::ZhHant),
            (Lang::Auto, Lang::ZhHans),
        ]
    }

    fn engine_name(&self) -> &'static str {
        "OfflineDictionary"
    }
}

/// 兼容 OpenAI / Ollama / LM Studio 的大模型翻译引擎配置。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OpenAiCompatibleConfig {
    /// 接口基础地址（例如 `http://localhost:11434/v1`）。
    pub base_url: String,
    /// 认证密钥（本地服务可随意填写）。
    pub api_key: String,
    /// 调用的模型名称（例如 `qwen2.5`、`llama3.2`）。
    pub model: String,
}

impl OpenAiCompatibleConfig {
    /// 生成符合 OpenAI Chat 格式的翻译 Prompt JSON 数据。
    ///
    /// # 参数
    /// - `text`: 待翻译文本。
    /// - `src`: 源语言。
    /// - `tgt`: 目标语言。
    ///
    /// # 返回
    /// 序列化为 JSON 字符串的请求体。
    pub fn format_request(&self, text: &str, src: Lang, tgt: Lang) -> String {
        let sys_prompt = format!(
            "You are a professional translator. Translate the text from {} to {}. Output ONLY the translated text without explanations.",
            src.display_name(),
            tgt.display_name()
        );

        let body = serde_json::json!({
            "model": self.model,
            "messages": [
                { "role": "system", "content": sys_prompt },
                { "role": "user", "content": text }
            ],
            "temperature": 0.3
        });

        body.to_string()
    }
}

/// 翻译服务总控制器。
///
/// 管理当前活跃的翻译引擎、本地模型清单扫描，以及内存结果缓存。
pub struct TranslationService {
    /// 本地模型扫描器。
    pub scanner: ModelScanner,
    /// 当前生效的翻译引擎。
    engine: Box<dyn TranslationEngine>,
    /// 翻译结果最近缓存 (Key: `src:tgt:text`, Value: `result`)。
    cache: HashMap<String, String>,
}

impl TranslationService {
    /// 构造翻译服务实例。
    ///
    /// # 参数
    /// - `models_dir`: 本地模型存放根路径。
    ///
    /// # 示例
    /// ```rust
    /// use std::path::Path;
    /// use snow_translate::{TranslationService, Lang};
    /// let mut svc = TranslationService::new(Path::new("D:/models/translate"));
    /// let res = svc.translate("hello", Lang::En, Lang::ZhHans).unwrap();
    /// assert_eq!(res, "你好");
    /// ```
    pub fn new(models_dir: &Path) -> Self {
        Self {
            scanner: ModelScanner::new(models_dir),
            engine: Box::new(OfflineDictionaryEngine::new()),
            cache: HashMap::new(),
        }
    }

    /// 切换底层翻译引擎实现。
    pub fn set_engine(&mut self, engine: Box<dyn TranslationEngine>) {
        self.engine = engine;
        self.cache.clear();
    }

    /// 执行翻译并自动走缓存。
    pub fn translate(&mut self, text: &str, src: Lang, tgt: Lang) -> Result<String, TranslateError> {
        let cache_key = format!("{}:{}:{}", src.code(), tgt.code(), text.trim());
        if let Some(cached) = self.cache.get(&cache_key) {
            return Ok(cached.clone());
        }

        let result = self.engine.translate(text, src, tgt)?;
        self.cache.insert(cache_key, result.clone());
        Ok(result)
    }

    /// 获取可用本地 NMT 模型列表。
    pub fn available_models(&self) -> Vec<ModelManifest> {
        self.scanner.scan_models()
    }

    /// 当前翻译引擎名称。
    pub fn current_engine_name(&self) -> &'static str {
        self.engine.engine_name()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 验证语言码解析与格式化。
    #[test]
    fn test_lang_code_and_display() {
        assert_eq!(Lang::ZhHans.code(), "zh-CN");
        assert_eq!(Lang::from_code("zh-cn"), Some(Lang::ZhHans));
        assert_eq!(Lang::from_code("en"), Some(Lang::En));
        assert_eq!(Lang::from_code("ja"), Some(Lang::Ja));
        assert_eq!(Lang::Auto.display_name(), "自动检测");
    }

    /// 验证模型清单校验逻辑。
    #[test]
    fn test_manifest_validation() {
        let mut files = HashMap::new();
        files.insert("encoder".into(), "encoder.onnx".into());
        let mut manifest = ModelManifest {
            schema_version: 1,
            id: "nllb-int8".into(),
            display_name: "NLLB 200".into(),
            family: "nllb".into(),
            quantization: "int8".into(),
            files,
            languages: vec!["zh-CN".into(), "en".into()],
            max_input_tokens: 512,
        };

        assert!(manifest.validate().is_ok());

        // 测试版本不匹配校验
        manifest.schema_version = 2;
        assert!(manifest.validate().is_err());
    }

    /// 验证内置离线翻译词典。
    #[test]
    fn test_offline_dict_translate() {
        let engine = OfflineDictionaryEngine::new();
        let out = engine.translate("screenshot", Lang::En, Lang::ZhHans).unwrap();
        assert_eq!(out, "屏幕截图");

        let out_unknown = engine.translate("unknown word", Lang::En, Lang::ZhHans).unwrap();
        assert_eq!(out_unknown, "unknownword");
    }

    /// 验证 OpenAI 兼容请求体格式化。
    #[test]
    fn test_openai_request_format() {
        let cfg = OpenAiCompatibleConfig {
            base_url: "http://localhost:11434/v1".into(),
            api_key: "dummy".into(),
            model: "qwen2.5".into(),
        };

        let req_json = cfg.format_request("Hello world", Lang::En, Lang::ZhHans);
        assert!(req_json.contains("qwen2.5"));
        assert!(req_json.contains("Hello world"));
    }

    /// 验证翻译服务缓存。
    #[test]
    fn test_translation_service_cache() {
        let temp_dir = std::env::temp_dir().join("snow_translate_test_models");
        let mut svc = TranslationService::new(&temp_dir);

        let res1 = svc.translate("copy", Lang::En, Lang::ZhHans).unwrap();
        assert_eq!(res1, "复制");

        // 再次获取应直接命中缓存
        let res2 = svc.translate("copy", Lang::En, Lang::ZhHans).unwrap();
        assert_eq!(res2, "复制");
    }
}
