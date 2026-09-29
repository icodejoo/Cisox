//! 对照测试（约定 7）：真实 `.ts` 的 Qt 渲染结果 与 转换后 Fluent 渲染结果逐条一致。

use snow_i18n::convert::{convert_catalog, identity_catalog, message_id};
use snow_i18n::ts::{TsCatalog, TsStatus, parse_ts};
use snow_i18n::{Args, I18n};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// 旧版产品名，Qt 原文里的字面量。
const LEGACY: &str = "Snow Shot";

/// 复数测试取值。
const PLURAL_SAMPLES: [i64; 5] = [0, 1, 2, 5, 1234];

/// 仓库根目录。
fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../..")
}

/// 收集并解析全部真实 `.ts`。
fn load_all() -> Vec<(String, TsCatalog)> {
    let mut files = Vec::new();
    let dirs = [
        repo_root().join("snow_shot/i18n"),
        repo_root().join("ant_design_qt/packages/ant_design_qt/i18n"),
    ];
    for d in dirs {
        walk(&d, &mut files);
    }
    assert!(
        files.len() >= 32,
        "应至少找到 32 个 .ts，实际 {}",
        files.len()
    );
    files
        .into_iter()
        .map(|f| {
            let text = std::fs::read_to_string(&f).unwrap();
            let name = f.file_name().unwrap().to_string_lossy().to_string();
            (name, parse_ts(&text).unwrap())
        })
        .collect()
}

/// 递归找 `.ts`。
fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
    for e in std::fs::read_dir(dir).unwrap() {
        let p = e.unwrap().path();
        if p.is_dir() {
            walk(&p, out);
        } else if p.extension().is_some_and(|x| x == "ts") {
            out.push(p);
        }
    }
}

/// 生成第 i 个位置参数的测试值。
fn sample_arg(i: usize) -> String {
    format!("<参数{i}>")
}

/// 按 Qt 语义渲染：`%n` 与 `%N`（含 `%LN`）替换。
fn qt_render(text: &str, n: Option<i64>) -> String {
    let chars: Vec<char> = text.chars().collect();
    let mut out = String::new();
    let mut i = 0;
    while i < chars.len() {
        if chars[i] == '%' {
            let mut j = i + 1;
            if chars.get(j) == Some(&'L') {
                j += 1;
            }
            if chars.get(j) == Some(&'n') {
                out.push_str(&n.unwrap_or(0).to_string());
                i = j + 1;
                continue;
            }
            let digits: String = chars[j..]
                .iter()
                .take(2)
                .take_while(|c| c.is_ascii_digit())
                .collect();
            if !digits.is_empty() && !digits.starts_with('0') {
                out.push_str(&sample_arg(digits.parse().unwrap()));
                i = j + digits.len();
                continue;
            }
        }
        out.push(chars[i]);
        i += 1;
    }
    out
}

/// 构造全部位置参数 `%1..%99`。
fn all_args() -> Args {
    (1..=99u8).fold(Args::new(), |a, i| a.arg(i, sample_arg(i.into())))
}

/// Qt 对复数形式的选择：英文 n==1 取第一项，其余语言（中文）单形式。
fn qt_form(forms: &[String], n: i64) -> &str {
    if forms.len() >= 2 && n != 1 {
        &forms[1]
    } else {
        &forms[0]
    }
}

/// 按语言把目录转成 ftl 资源，返回（语言→资源文本列表）。
fn build_resources(cats: &[(String, TsCatalog)]) -> BTreeMap<String, Vec<String>> {
    let mut map: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for (_, c) in cats {
        map.entry(c.language.replace('_', "-"))
            .or_default()
            .push(convert_catalog(c).ftl);
    }
    map
}

/// 全部真实条目：产品名取旧值时渲染结果必须与 Qt 完全一致。
#[test]
fn all_real_messages_match_qt() {
    let cats = load_all();
    let res = build_resources(&cats);
    let mut checked = 0usize;
    for (lang, texts) in &res {
        let pairs: Vec<(&str, &str)> = texts.iter().map(|t| (lang.as_str(), t.as_str())).collect();
        let i18n = I18n::from_resources(lang, lang, LEGACY, &pairs).unwrap();
        for (file, c) in cats
            .iter()
            .filter(|(_, c)| c.language.replace('_', "-") == *lang)
        {
            for ctx in &c.contexts {
                for m in ctx
                    .messages
                    .iter()
                    .filter(|m| m.status == TsStatus::Finished)
                {
                    let id = message_id(&ctx.name, &m.source);
                    let samples: &[i64] = if m.numerus { &PLURAL_SAMPLES } else { &[0] };
                    for &n in samples {
                        let args = if m.numerus {
                            all_args().count(n)
                        } else {
                            all_args()
                        };
                        let got = i18n.tr_checked(&id, &args).unwrap_or_else(|e| {
                            panic!("{file} [{}] {:?}: {e}", ctx.name, m.source)
                        });
                        let want = if m.numerus {
                            qt_render(qt_form(&m.translations, n), Some(n))
                        } else {
                            qt_render(&m.translations[0], None)
                        };
                        assert_eq!(got, want, "{file} [{}] {:?} n={n}", ctx.name, m.source);
                        checked += 1;
                    }
                }
            }
        }
    }
    assert!(checked >= 6000, "对照条数过少：{checked}");
}

/// 换成新产品名后，任何语言的任何文案都不应再含旧产品名。
#[test]
fn product_name_is_injected_not_baked() {
    let cats = load_all();
    for (lang, texts) in build_resources(&cats) {
        for t in &texts {
            assert!(
                !t.lines().any(|l| !l.starts_with('#') && l.contains(LEGACY)),
                "{lang} 的 ftl 仍含旧产品名"
            );
        }
    }
    let ftl = "k = { $product } 已就绪\n";
    let i = I18n::from_resources("zh-CN", "en-US", "Cisox", &[("zh-CN", ftl)]).unwrap();
    assert_eq!(i.tr("k"), "Cisox 已就绪");
}

/// 挑选覆盖各形态的条目做显式断言：占位符、复数、`&`、花括号、首尾空白、多行。
#[test]
fn curated_shapes() {
    let cats = load_all();
    let res = build_resources(&cats);
    let refs = |lang: &str| -> Vec<(String, String)> {
        res[lang]
            .iter()
            .map(|t| (lang.to_string(), t.clone()))
            .collect()
    };
    let make = |lang: &str| {
        let owned = refs(lang);
        let pairs: Vec<(&str, &str)> = owned
            .iter()
            .map(|(l, t)| (l.as_str(), t.as_str()))
            .collect();
        I18n::from_resources(lang, lang, "Cisox", &pairs).unwrap()
    };
    let (zh, en) = (make("zh-CN"), make("en-US"));
    // 找一条含 `%n` 的复数条目（en 有 one/other 两形式）。
    let plural_src = "%n screenshot(s)";
    let (ctx, _) = cats
        .iter()
        .flat_map(|(_, c)| c.contexts.iter())
        .find_map(|c| {
            c.messages
                .iter()
                .find(|m| m.source == plural_src)
                .map(|m| (c.name.clone(), m.clone()))
        })
        .expect("应存在复数样本");
    let id = message_id(&ctx, plural_src);
    assert_eq!(en.tr_with(&id, &Args::new().count(1)), "1 screenshot");
    assert_eq!(en.tr_with(&id, &Args::new().count(3)), "3 screenshots");
    assert!(zh.tr_with(&id, &Args::new().count(3)).contains('3'));
    // `&` 应按字面量保留。
    let amp = "Screen & System Audio Recording";
    let ctx = find_ctx(&cats, amp);
    assert_eq!(en.tr(&message_id(&ctx, amp)), amp);
    // 花括号应按字面量保留。
    let brace = "{text} represents the current watermark text; timestamp formats such as {YYYY-MM-DD_HH-mm-ss} are supported";
    let ctx = find_ctx(&cats, brace);
    assert_eq!(en.tr(&message_id(&ctx, brace)), brace);
    // 产品名与 %1 混合：旧文案 "Snow Shot" 应变成当前产品名。
    let prod = "Another Snow Shot instance owns the MCP endpoint.";
    let ctx = find_ctx(&cats, prod);
    assert_eq!(
        en.tr(&message_id(&ctx, prod)),
        "Another Cisox instance owns the MCP endpoint."
    );
    // 首部空白应保留。
    let lead = " Recovery failed: %1";
    let ctx = find_ctx(&cats, lead);
    assert_eq!(
        en.tr_with(&message_id(&ctx, lead), &Args::new().arg(1, "X")),
        " Recovery failed: X"
    );
}

/// 找到含指定源文的上下文名。
fn find_ctx(cats: &[(String, TsCatalog)], source: &str) -> String {
    cats.iter()
        .flat_map(|(_, c)| c.contexts.iter())
        .find(|c| c.messages.iter().any(|m| m.source == source))
        .map(|c| c.name.clone())
        .unwrap_or_else(|| panic!("找不到源文：{source}"))
}

/// 已提交的 `locales/` 必须与从 `.ts` 重新转换的结果一致，且内置语料可解析、id 不重复。
#[test]
fn committed_locales_are_fresh_and_embedded_loads() {
    let cats = load_all();
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("locales");
    let mut modules: BTreeMap<String, bool> = BTreeMap::new();
    for (file, c) in &cats {
        let stem = file.trim_end_matches(".ts");
        let stem = stem
            .strip_suffix(&format!("_{}", c.language))
            .unwrap_or(stem);
        let module = stem.strip_prefix("snow_shot_").unwrap_or(stem).to_string();
        modules.insert(
            module.clone(),
            modules.get(&module).copied().unwrap_or(false) || c.language == "en_US",
        );
        let path = root
            .join(c.language.replace('_', "-"))
            .join(format!("{module}.ftl"));
        let disk =
            std::fs::read_to_string(&path).unwrap_or_else(|_| panic!("缺少 {}", path.display()));
        assert_eq!(
            disk.replace("\r\n", "\n"),
            convert_catalog(c).ftl,
            "{} 已过期，请重新运行转换工具",
            path.display()
        );
    }
    for (module, has_en) in &modules {
        if !has_en {
            let (_, base) = cats
                .iter()
                .find(|(f, _)| f.contains(module.as_str()))
                .unwrap();
            let path = root.join("en-US").join(format!("{module}.ftl"));
            let disk = std::fs::read_to_string(&path).unwrap();
            assert_eq!(
                disk.replace("\r\n", "\n"),
                convert_catalog(&identity_catalog(base, "en_US")).ftl
            );
        }
    }
    for lang in ["en-US", "zh-CN", "zh-TW"] {
        let i = I18n::embedded(lang, "en-US", "Cisox").unwrap();
        let id = message_id(
            "AdministratorLaunch",
            "Administrator authorization was declined.",
        );
        assert!(i.has(&id), "{lang} 内置语料缺少样本");
    }
}
