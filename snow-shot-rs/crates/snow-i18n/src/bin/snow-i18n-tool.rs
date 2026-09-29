//! 命令行工具：`.ts` → `.ftl` 转换与多语言 id 对齐检查。
//!
//! 用法：
//! - `snow-i18n-tool convert <输出目录> <.ts 文件或目录>...`
//! - `snow-i18n-tool check <locales 目录>`

use snow_i18n::convert::{
    ConvertStats, IssueKind, convert_catalog, ftl_message_ids, identity_catalog, missing_ids,
};
use snow_i18n::ts::{TsCatalog, parse_ts};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

/// 合成源语言目录时使用的语言标记。
const SOURCE_LANG: &str = "en_US";

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

/// 程序入口。
fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let result = match args.first().map(String::as_str) {
        Some("convert") if args.len() >= 3 => {
            run_convert(Path::new(&args[1]), &args[2..]).map(|()| true)
        }
        Some("check") if args.len() == 2 => run_check(Path::new(&args[1])),
        _ => Err("用法：convert <输出目录> <ts...> | check <locales 目录>".to_string()),
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
