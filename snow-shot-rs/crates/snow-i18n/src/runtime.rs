//! Fluent 运行时：多语言加载、回退链与 `tr` 查询。

use fluent_bundle::concurrent::FluentBundle;
use fluent_bundle::{FluentArgs, FluentResource, FluentValue};
use std::fmt;
use unic_langid::LanguageIdentifier;

use crate::convert::PRODUCT_VAR;

/// 运行时错误。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum I18nError {
    /// 语言标记无法解析。
    BadLocale(String),
    /// `.ftl` 存在语法错误。
    Parse {
        /// 语言。
        lang: String,
        /// 错误条数。
        count: usize,
    },
    /// 同一语言内消息 id 重复。
    DuplicateId {
        /// 语言。
        lang: String,
        /// 重复的 id 数。
        count: usize,
    },
    /// 消息不存在（仅 [`I18n::tr_checked`] 返回）。
    Missing(String),
    /// 格式化出现错误（仅 [`I18n::tr_checked`] 返回）。
    Format {
        /// 消息 id。
        id: String,
        /// 首个错误描述。
        detail: String,
    },
}

impl fmt::Display for I18nError {
    /// 输出中文错误描述。
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::BadLocale(l) => write!(f, "无法解析语言标记：{l}"),
            Self::Parse { lang, count } => write!(f, "{lang} 的 ftl 有 {count} 处语法错误"),
            Self::DuplicateId { lang, count } => write!(f, "{lang} 有 {count} 个重复的消息 id"),
            Self::Missing(id) => write!(f, "缺少消息：{id}"),
            Self::Format { id, detail } => write!(f, "消息 {id} 格式化失败：{detail}"),
        }
    }
}

impl std::error::Error for I18nError {}

/// 参数值。
#[derive(Debug, Clone, PartialEq)]
enum ArgValue {
    Str(String),
    Int(i64),
    Float(f64),
}

/// 消息参数集合（不暴露 Fluent 类型）。
///
/// # 示例
/// ```
/// use snow_i18n::Args;
/// let args = Args::new().arg(1, "a.png").count(3);
/// ```
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Args {
    items: Vec<(String, ArgValue)>,
}

impl Args {
    /// 创建空参数集。
    pub fn new() -> Self {
        Self::default()
    }

    /// 设置第 `index` 个位置参数（对应 Qt 的 `%index`，变量名 `argN`）。
    pub fn arg(self, index: u8, value: impl fmt::Display) -> Self {
        self.named(&format!("arg{index}"), value.to_string())
    }

    /// 设置复数计数（对应 Qt 的 `%n`，变量名 `n`）。
    pub fn count(mut self, n: i64) -> Self {
        self.items.push(("n".to_string(), ArgValue::Int(n)));
        self
    }

    /// 设置字符串命名参数。
    pub fn named(mut self, name: &str, value: impl Into<String>) -> Self {
        self.items
            .push((name.to_string(), ArgValue::Str(value.into())));
        self
    }

    /// 设置浮点命名参数。
    pub fn float(mut self, name: &str, value: f64) -> Self {
        self.items.push((name.to_string(), ArgValue::Float(value)));
        self
    }

    /// 按值的类型设置命名参数（整数与浮点保持数值类型，供复数选择；其余按字符串）。
    ///
    /// # 参数
    /// - `name`：变量名，如 `n`、`arg1`。
    /// - `value`：整数、浮点、`&str` 或 `String`。
    ///
    /// # 示例
    /// ```
    /// use snow_i18n::Args;
    /// let args = Args::new().set("n", 3).set("arg1", "a.png");
    /// ```
    pub fn set(self, name: &str, value: impl IntoArg) -> Self {
        value.apply(self, name)
    }
}

/// 可作为消息参数的值类型（供 [`Args::set`] 与 `t!` 宏使用）。
pub trait IntoArg {
    /// 把自身以 `name` 写入参数集。
    fn apply(self, args: Args, name: &str) -> Args;
}

/// 为整数类型实现 [`IntoArg`]。
macro_rules! impl_int_arg {
    ($($t:ty),*) => {$(
        impl IntoArg for $t {
            /// 以整数写入。
            fn apply(self, mut args: Args, name: &str) -> Args {
                args.items.push((name.to_string(), ArgValue::Int(self as i64)));
                args
            }
        }
    )*};
}
impl_int_arg!(i8, i16, i32, i64, isize, u8, u16, u32, usize);

impl IntoArg for f64 {
    /// 以浮点写入。
    fn apply(self, args: Args, name: &str) -> Args {
        args.float(name, self)
    }
}

impl IntoArg for f32 {
    /// 以浮点写入。
    fn apply(self, args: Args, name: &str) -> Args {
        args.float(name, f64::from(self))
    }
}

impl IntoArg for &str {
    /// 以字符串写入。
    fn apply(self, args: Args, name: &str) -> Args {
        args.named(name, self)
    }
}

impl IntoArg for String {
    /// 以字符串写入。
    fn apply(self, args: Args, name: &str) -> Args {
        args.named(name, self)
    }
}

impl IntoArg for &String {
    /// 以字符串写入。
    fn apply(self, args: Args, name: &str) -> Args {
        args.named(name, self.as_str())
    }
}

/// 查询译文的宏，形态固定以便 `snow-i18n-tool extract` 识别。
///
/// - `t!(i18n, "id")`：无参。
/// - `t!(i18n, "id", n = 3, arg1 = "x")`：带命名参数（`n` 为复数计数，`argN` 对应 Qt 的 `%N`）。
///
/// id 必须是字符串字面量；缺失时走运行时降级（`[!缺失:id]`），编译期不检查。
///
/// # 示例
/// ```
/// use snow_i18n::{t, I18n};
/// let ftl = "hi = 你好 { $arg1 }";
/// let i = I18n::from_resources("zh-CN", "en-US", "Cisox", &[("zh-CN", ftl)]).unwrap();
/// assert_eq!(t!(i, "hi", arg1 = "小明"), "你好 小明");
/// ```
#[macro_export]
macro_rules! t {
    ($i18n:expr, $id:literal $(,)?) => {
        $i18n.tr($id)
    };
    ($i18n:expr, $id:literal, $($name:ident = $val:expr),+ $(,)?) => {
        $i18n.tr_with($id, &$crate::Args::new()$(.set(stringify!($name), $val))+)
    };
}

/// 单个语言的 bundle。
struct Locale {
    bundle: FluentBundle<FluentResource>,
}

/// 多语言消息目录，带回退链。
pub struct I18n {
    chain: Vec<Locale>,
    product: String,
}

impl fmt::Debug for I18n {
    /// 仅输出回退链长度与产品名。
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("I18n")
            .field("chain_len", &self.chain.len())
            .field("product", &self.product)
            .finish()
    }
}

/// 解析并规范化语言标记（接受 `zh_CN` 与 `zh-CN`）。
fn parse_lang(s: &str) -> Result<LanguageIdentifier, I18nError> {
    s.replace('_', "-")
        .parse()
        .map_err(|_| I18nError::BadLocale(s.to_string()))
}

impl I18n {
    /// 用内置语料创建。
    ///
    /// # 参数
    /// - `locale`：首选语言，如 `zh-CN`。
    /// - `fallback`：回退语言，如 `en-US`。
    /// - `product`：产品名，作为变量 `product` 注入（取自 `snow_app_core::PRODUCT_NAME`）。
    ///
    /// # 返回
    /// 回退链为 `locale` → `fallback`；语言没有语料时该环被跳过。
    ///
    /// # 示例
    /// ```
    /// let i18n = snow_i18n::I18n::embedded("zh-CN", "en-US", "Cisox").unwrap();
    /// assert!(i18n.tr("no-such-id").contains("no-such-id"));
    /// ```
    pub fn embedded(locale: &str, fallback: &str, product: &str) -> Result<Self, I18nError> {
        Self::from_resources(locale, fallback, product, crate::embedded::RESOURCES)
    }

    /// 用给定 `.ftl` 文本创建。
    ///
    /// # 参数
    /// - `locale` / `fallback` / `product`：同 [`I18n::embedded`]。
    /// - `resources`：`(语言, ftl 文本)` 列表，同一语言可有多份。
    ///
    /// # 返回
    /// 成功返回实例；语法错误、id 重复或语言标记非法返回 [`I18nError`]。
    pub fn from_resources(
        locale: &str,
        fallback: &str,
        product: &str,
        resources: &[(&str, &str)],
    ) -> Result<Self, I18nError> {
        let mut wanted = vec![parse_lang(locale)?];
        let fb = parse_lang(fallback)?;
        if !wanted.contains(&fb) {
            wanted.push(fb);
        }
        let mut chain = Vec::new();
        for lang in wanted {
            let mut bundle = FluentBundle::new_concurrent(vec![lang.clone()]);
            bundle.set_use_isolating(false);
            let mut found = false;
            for (l, text) in resources {
                if parse_lang(l)? != lang {
                    continue;
                }
                found = true;
                let res = FluentResource::try_new((*text).to_string()).map_err(|(_, e)| {
                    I18nError::Parse {
                        lang: lang.to_string(),
                        count: e.len(),
                    }
                })?;
                bundle
                    .add_resource(res)
                    .map_err(|e| I18nError::DuplicateId {
                        lang: lang.to_string(),
                        count: e.len(),
                    })?;
            }
            if found {
                chain.push(Locale { bundle });
            }
        }
        Ok(Self {
            chain,
            product: product.to_string(),
        })
    }

    /// 转换参数并注入产品名。
    fn to_fluent<'a>(&'a self, args: &'a Args) -> FluentArgs<'a> {
        let mut fa = FluentArgs::new();
        for (k, v) in &args.items {
            let val = match v {
                ArgValue::Str(s) => FluentValue::from(s.as_str()),
                ArgValue::Int(i) => FluentValue::from(*i),
                ArgValue::Float(x) => FluentValue::from(*x),
            };
            fa.set(k.as_str(), val);
        }
        fa.set(PRODUCT_VAR, FluentValue::from(self.product.as_str()));
        fa
    }

    /// 查询无参消息；缺失时返回降级文案，不会 panic。
    ///
    /// # 参数
    /// - `id`：消息 id。
    ///
    /// # 返回
    /// 译文；找不到时返回 `[!缺失:id]`。
    pub fn tr(&self, id: &str) -> String {
        self.tr_with(id, &Args::new())
    }

    /// 查询带参消息；缺失或格式化出错时返回降级文案，不会 panic。
    ///
    /// # 参数
    /// - `id`：消息 id。
    /// - `args`：参数集合。
    ///
    /// # 返回
    /// 译文；找不到时返回 `[!缺失:id]`，格式化部分失败时返回 Fluent 的尽力结果。
    ///
    /// # 示例
    /// ```
    /// use snow_i18n::{Args, I18n};
    /// let ftl = "hi = 你好 { $arg1 }，欢迎使用 { $product }";
    /// let i = I18n::from_resources("zh-CN", "en-US", "Cisox", &[("zh-CN", ftl)]).unwrap();
    /// assert_eq!(i.tr_with("hi", &Args::new().arg(1, "小明")), "你好 小明，欢迎使用 Cisox");
    /// ```
    pub fn tr_with(&self, id: &str, args: &Args) -> String {
        match self.lookup(id, args) {
            Some((text, _)) => text,
            None => format!("[!缺失:{id}]"),
        }
    }

    /// 严格查询：缺失或格式化有错时返回错误，供测试与门禁使用。
    ///
    /// # 参数
    /// - `id`：消息 id。
    /// - `args`：参数集合。
    ///
    /// # 返回
    /// 译文，或 [`I18nError::Missing`] / [`I18nError::Format`]。
    pub fn tr_checked(&self, id: &str, args: &Args) -> Result<String, I18nError> {
        match self.lookup(id, args) {
            None => Err(I18nError::Missing(id.to_string())),
            Some((_, Some(detail))) => Err(I18nError::Format {
                id: id.to_string(),
                detail,
            }),
            Some((text, None)) => Ok(text),
        }
    }

    /// 回退链中是否存在该消息。
    pub fn has(&self, id: &str) -> bool {
        self.chain.iter().any(|l| {
            l.bundle
                .get_message(id)
                .is_some_and(|m| m.value().is_some())
        })
    }

    /// 沿回退链查找并格式化，返回（文本, 首个格式化错误）。
    fn lookup(&self, id: &str, args: &Args) -> Option<(String, Option<String>)> {
        let fa = self.to_fluent(args);
        for l in &self.chain {
            let Some(pattern) = l.bundle.get_message(id).and_then(|m| m.value()) else {
                continue;
            };
            let mut errors = Vec::new();
            let text = l
                .bundle
                .format_pattern(pattern, Some(&fa), &mut errors)
                .into_owned();
            return Some((text, errors.first().map(ToString::to_string)));
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 测试用语料。
    const ZH: &str = "a = 你好 { $arg1 }\nb = { $product } 已就绪\nc =\n    { $n ->\n       *[other] { $n } 项\n    }\n";
    /// 测试用英文语料。
    const EN: &str = "a = Hello { $arg1 }\nonly-en = English only\nc =\n    { $n ->\n        [one] { $n } item\n       *[other] { $n } items\n    }\n";

    /// 构造中英实例。
    fn make() -> I18n {
        I18n::from_resources("zh_CN", "en-US", "Cisox", &[("zh-CN", ZH), ("en-US", EN)]).unwrap()
    }

    /// 首选语言优先，产品名自动注入。
    #[test]
    fn prefers_locale_and_injects_product() {
        let i = make();
        assert_eq!(i.tr_with("a", &Args::new().arg(1, "X")), "你好 X");
        assert_eq!(i.tr("b"), "Cisox 已就绪");
    }

    /// 首选语言缺失时回退到 en-US。
    #[test]
    fn falls_back() {
        assert_eq!(make().tr("only-en"), "English only");
    }

    /// 缺失 key 返回可诊断文案而不 panic。
    #[test]
    fn missing_is_diagnosable() {
        let i = make();
        assert_eq!(i.tr("nope"), "[!缺失:nope]");
        assert_eq!(
            i.tr_checked("nope", &Args::new()),
            Err(I18nError::Missing("nope".into()))
        );
        assert!(!i.has("nope") && i.has("a"));
    }

    /// 复数按语言规则选择；数值不加千分位。
    #[test]
    fn plural_and_number() {
        let en = I18n::from_resources("en-US", "en-US", "P", &[("en-US", EN)]).unwrap();
        assert_eq!(en.tr_with("c", &Args::new().count(1)), "1 item");
        assert_eq!(en.tr_with("c", &Args::new().count(1234)), "1234 items");
        assert_eq!(make().tr_with("c", &Args::new().count(1)), "1 项");
    }

    /// 宏展开：无参、带参、复数与尾逗号。
    #[test]
    fn macro_expands() {
        let i = make();
        assert_eq!(crate::t!(i, "b"), "Cisox 已就绪");
        assert_eq!(crate::t!(i, "a", arg1 = "X"), "你好 X");
        assert_eq!(crate::t!(i, "a", arg1 = String::from("Y"),), "你好 Y");
        assert_eq!(crate::t!(i, "c", n = 5), "5 项");
        assert_eq!(crate::t!(i, "nope"), "[!缺失:nope]");
    }

    /// 参数缺失走 tr_checked 时应报格式化错误。
    #[test]
    fn checked_reports_format_error() {
        assert!(matches!(
            make().tr_checked("a", &Args::new()),
            Err(I18nError::Format { .. })
        ));
    }

    /// 非法语言标记与语法错误应返回错误。
    #[test]
    fn rejects_bad_input() {
        assert!(I18n::from_resources("!!", "en", "P", &[]).is_err());
        assert!(I18n::from_resources("en", "en", "P", &[("en", "= broken")]).is_err());
    }

    /// 实例应可跨线程共享。
    #[test]
    fn is_send_sync() {
        fn assert_ss<T: Send + Sync>() {}
        assert_ss::<I18n>();
    }
}
