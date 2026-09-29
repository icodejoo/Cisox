//! 命令行工具：`.ts` → `.ftl` 转换与多语言 id 对齐检查。
//!
//! 用法：
//! - `snow-i18n-tool convert <输出目录> <.ts 文件或目录>...`
//! - `snow-i18n-tool check <locales 目录> [--src <源码目录>]... [--exclude <路径子串>]... [--strict-refs]`
//! - `snow-i18n-tool extract <源码目录>... [--locales <目录>] [--exclude <路径子串>]... [--strict-refs]`
//!
//! `--strict-refs`：代码引用了基准语言（en-US）没有的 id 时失败；孤儿 id 只报告数量。

use snow_i18n::convert::{
    ConvertStats, IssueKind, convert_catalog, ftl_message_ids, identity_catalog, missing_ids,
};
use snow_i18n::extract::{base_ids, compare_refs, scan};
use snow_i18n::ts::{TsCatalog, parse_ts};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

/// 合成源语言目录时使用的语言标记。
const SOURCE_LANG: &str = "en_US";

/// 引用对齐的基准语言目录名。
const BASE_LANG: &str = "en-US";

/// 默认的 locales 目录（相对 workspace 根）。
const DEFAULT_LOCALES: &str = "crates/snow-i18n/locales";

/// 命令行用法。
const USAGE: &str = "用法：convert <输出目录> <ts...> | check <locales 目录> [--src 目录]... [--exclude 子串]... [--strict-refs] | extract <源码目录...> [--locales 目录] [--exclude 子串]... [--strict-refs]";

/// 引用扫描相关选项。
#[derive(Default)]
struct RefOpts {
    /// 位置参数。
    positional: Vec<String>,
    /// 源码目录（`--src`）。
    src: Vec<PathBuf>,
    /// 排除子串。
    excludes: Vec<String>,
    /// locales 目录（`--locales`）。
    locales: Option<PathBuf>,
    /// 是否严格。
    strict: bool,
}

/// 解析选项；未知 `--` 选项报错。
fn parse_opts(args: &[String]) -> Result<RefOpts, String> {
    let mut o = RefOpts::default();
    let mut it = args.iter();
    while let Some(a) = it.next() {
        let mut value = |name: &str| it.next().cloned().ok_or(format!("{name} 缺少参数"));
        match a.as_str() {
            "--src" => o.src.push(PathBuf::from(value("--src")?)),
            "--exclude" => o.excludes.push(value("--exclude")?),
            "--locales" => o.locales = Some(PathBuf::from(value("--locales")?)),
            "--strict-refs" => o.strict = true,
            s if s.starts_with("--") => return Err(format!("未知选项 {s}")),
            _ => o.positional.push(a.clone()),
        }
    }
    Ok(o)
}

/// 扫描源码并与基准语言对齐；返回是否通过（非严格模式恒通过）。
fn run_refs(
    locales: &Path,
    src: &[PathBuf],
    excludes: &[String],
    strict: bool,
) -> Result<bool, String> {
    let base = base_ids(locales, BASE_LANG).map_err(|e| format!("{}: {e}", locales.display()))?;
    let refs = scan(src, excludes).map_err(|e| e.to_string())?;
    let report = compare_refs(&refs, &base);
    for (id, file, line) in &report.missing {
        println!("缺失：{file}:{line} 引用了 {BASE_LANG} 中不存在的 id：{id}");
    }
    println!(
        "引用 {} 处，缺失 {} 处；{BASE_LANG} 共 {} 个 id，未被引用（孤儿）{} 个",
        report.total_refs,
        report.missing.len(),
        base.len(),
        report.orphans.len()
    );
    let ok = report.missing.is_empty() || !strict;
    println!(
        "{}",
        if ok {
            "引用检查通过"
        } else {
            "引用检查失败"
        }
    );
    Ok(ok)
}

/// 递归收集 `.ts` 文件。
fn collect_ts(path: &Path, out: &mut Vec<PathBuf>) -> std::io::Result<()> {
    if path.is_dir() {
        let mut entries: Vec<_> = std::fs::read_dir(path)?.collect::<Result<_, _>>()?;
        entries.sort_by_key(|e| e.path());
        for e in entries {
            collect_ts(&e.path(), out)?;
        }
    } else if path.extension().is_some_and(|e| e == "ts") {
        out.push(path.to_path_buf());
    }
    Ok(())
}

/// 由文件名推导模块名：去掉语言后缀与 `snow_shot_` 前缀。
fn module_name(path: &Path, lang: &str) -> String {
    let stem = path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("unknown");
    let stem = stem.strip_suffix(&format!("_{lang}")).unwrap_or(stem);
    stem.strip_prefix("snow_shot_").unwrap_or(stem).to_string()
}

/// 把 `zh_CN` 规范为 `zh-CN`。
fn lang_tag(lang: &str) -> String {
    lang.replace('_', "-")
}

/// 写出一份 ftl 并累计统计。
fn emit(
    out_dir: &Path,
    module: &str,
    cat: &TsCatalog,
    total: &mut ConvertStats,
    kinds: &mut BTreeMap<String, usize>,
) -> std::io::Result<()> {
    let res = convert_catalog(cat);
    let dir = out_dir.join(lang_tag(&cat.language));
    std::fs::create_dir_all(&dir)?;
    std::fs::write(dir.join(format!("{module}.ftl")), &res.ftl)?;
    total.total += res.stats.total;
    total.converted += res.stats.converted;
    total.plural += res.stats.plural;
    total.product_replaced += res.stats.product_replaced;
    total.with_args += res.stats.with_args;
    for i in &res.issues {
        *kinds.entry(format!("{:?}", i.kind)).or_default() += 1;
        if matches!(
            i.kind,
            IssueKind::PlaceholderMismatch | IssueKind::NeedsManualPlural
        ) {
            eprintln!("需人工：{:?} [{}] {}", i.kind, i.context, i.source);
        }
    }
    Ok(())
}

/// 执行 convert 子命令。
fn run_convert(out_dir: &Path, inputs: &[String]) -> Result<(), String> {
    let mut files = Vec::new();
    for i in inputs {
        collect_ts(Path::new(i), &mut files).map_err(|e| format!("{i}: {e}"))?;
    }
    let mut by_module: BTreeMap<String, Vec<TsCatalog>> = BTreeMap::new();
    for f in &files {
        let text = std::fs::read_to_string(f).map_err(|e| format!("{}: {e}", f.display()))?;
        let cat = parse_ts(&text).map_err(|e| format!("{}: {e}", f.display()))?;
        by_module
            .entry(module_name(f, &cat.language))
            .or_default()
            .push(cat);
    }
    let mut total = ConvertStats::default();
    let mut kinds = BTreeMap::new();
    for (module, mut cats) in by_module {
        if !cats.iter().any(|c| c.language == SOURCE_LANG) {
            let synthesized = identity_catalog(&cats[0], SOURCE_LANG);
            println!("模块 {module} 缺少 {SOURCE_LANG}，已用源文合成");
            cats.push(synthesized);
        }
        for cat in &cats {
            emit(out_dir, &module, cat, &mut total, &mut kinds).map_err(|e| e.to_string())?;
        }
    }
    println!(
        "文件 {} 个；统计 {total:?}；问题分布 {kinds:?}",
        files.len()
    );
    Ok(())
}

/// 执行 check 子命令：同一模块各语言的 id 必须一致。
fn run_check(root: &Path) -> Result<bool, String> {
    let mut by_module: BTreeMap<String, Vec<(String, BTreeSet<String>)>> = BTreeMap::new();
    for lang_dir in std::fs::read_dir(root).map_err(|e| e.to_string())? {
        let lang_dir = lang_dir.map_err(|e| e.to_string())?.path();
        let Some(lang) = lang_dir
            .file_name()
            .and_then(|n| n.to_str())
            .map(str::to_string)
        else {
            continue;
        };
        for f in std::fs::read_dir(&lang_dir).map_err(|e| e.to_string())? {
            let f = f.map_err(|e| e.to_string())?.path();
            if f.extension().is_some_and(|e| e == "ftl") {
                let text = std::fs::read_to_string(&f).map_err(|e| e.to_string())?;
                let module = f
                    .file_stem()
                    .and_then(|s| s.to_str())
                    .unwrap_or("")
                    .to_string();
                by_module
                    .entry(module)
                    .or_default()
                    .push((lang.clone(), ftl_message_ids(&text)));
            }
        }
    }
    let mut ok = true;
    for (module, sets) in &by_module {
        for (lang, miss) in missing_ids(sets) {
            ok = false;
            println!(
                "模块 {module}：{lang} 缺少 {} 条，如 {}",
                miss.len(),
                miss[0]
            );
        }
    }
    println!(
        "{}",
        if ok {
            "对齐检查通过"
        } else {
            "对齐检查失败"
        }
    );
    Ok(ok)
}

/// check 子命令：id 对齐，带 `--src` 时再做引用检查（`--strict-refs` 使缺失失败）。
fn run_check_cmd(args: &[String]) -> Result<bool, String> {
    let o = parse_opts(args)?;
    let [root] = o.positional.as_slice() else {
        return Err(USAGE.to_string());
    };
    if o.strict && o.src.is_empty() {
        return Err("--strict-refs 需要至少一个 --src".to_string());
    }
    let aligned = run_check(Path::new(root))?;
    if o.src.is_empty() {
        return Ok(aligned);
    }
    let refs_ok = run_refs(Path::new(root), &o.src, &o.excludes, o.strict)?;
    Ok(aligned && refs_ok)
}

/// extract 子命令。
fn run_extract_cmd(args: &[String]) -> Result<bool, String> {
    let o = parse_opts(args)?;
    if o.positional.is_empty() {
        return Err(USAGE.to_string());
    }
    let src: Vec<PathBuf> = o.positional.iter().map(PathBuf::from).collect();
    let locales = o.locales.unwrap_or_else(|| PathBuf::from(DEFAULT_LOCALES));
    run_refs(&locales, &src, &o.excludes, o.strict)
}

/// 程序入口。
fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let result = match args.first().map(String::as_str) {
        Some("convert") if args.len() >= 3 => {
            run_convert(Path::new(&args[1]), &args[2..]).map(|()| true)
        }
        Some("check") => run_check_cmd(&args[1..]),
        Some("extract") => run_extract_cmd(&args[1..]),
        _ => Err(USAGE.to_string()),
    };
    match result {
        Ok(true) => ExitCode::SUCCESS,
        Ok(false) => ExitCode::from(1),
        Err(e) => {
            eprintln!("{e}");
            ExitCode::from(2)
        }
    }
}
