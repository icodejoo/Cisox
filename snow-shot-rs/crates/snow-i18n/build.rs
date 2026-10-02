//! 构建脚本：扫描 `locales/<语言代码>/`，把全部 `.ftl` 与 `locale.toml` 编译进二进制。
//!
//! 新增语言只需新增目录、`locale.toml` 与 `.ftl`，无需改动 Rust 代码。
//! 只用标准库；`locale.toml` 只支持 `key = "字符串"` 与 `key = ["a", "b"]` 两种写法。

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::fs;
use std::path::{Path, PathBuf};

/// 语言元数据文件名。
const LOCALE_META_FILE: &str = "locale.toml";

/// 一份 `locale.toml` 的解析结果。
#[derive(Default)]
struct Meta {
    /// 标量字段（`code`、`native_name`）。
    scalars: BTreeMap<String, String>,
    /// 列表字段（`aliases`、`system_prefixes`）。
    lists: BTreeMap<String, Vec<String>>,
}

/// 取出一个带引号的字符串字面量的内容；不合法时 panic 并指明位置。
fn unquote(text: &str, ctx: &str) -> String {
    let t = text.trim();
    t.strip_prefix('"')
        .and_then(|s| s.strip_suffix('"'))
        .unwrap_or_else(|| panic!("{ctx}：需要双引号字符串，实际是 {t:?}"))
        .to_string()
}

/// 解析 `locale.toml`。
fn parse_meta(text: &str, path: &Path) -> Meta {
    let mut meta = Meta::default();
    for (no, raw) in text.lines().enumerate() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let ctx = format!("{}:{}", path.display(), no + 1);
        let (key, value) = line
            .split_once('=')
            .unwrap_or_else(|| panic!("{ctx}：应为 key = value"));
        let (key, value) = (key.trim().to_string(), value.trim());
        if let Some(inner) = value.strip_prefix('[').and_then(|v| v.strip_suffix(']')) {
            let items = inner
                .split(',')
                .filter(|s| !s.trim().is_empty())
                .map(|s| unquote(s, &ctx))
                .collect();
            meta.lists.insert(key, items);
        } else {
            meta.scalars.insert(key, unquote(value, &ctx));
        }
    }
    meta
}

/// 把字符串转成 Rust 字面量。
fn lit(s: &str) -> String {
    format!("{s:?}")
}

/// 把字符串列表转成 Rust 切片字面量。
fn lit_list(items: &[String]) -> String {
    let parts: Vec<String> = items.iter().map(|s| lit(s)).collect();
    format!("&[{}]", parts.join(", "))
}

fn main() {
    let root = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").expect("缺少 CARGO_MANIFEST_DIR"));
    let locales = root.join("locales");
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed={}", locales.display());

    let mut dirs: Vec<PathBuf> = fs::read_dir(&locales)
        .expect("读取 locales 目录失败")
        .map(|e| e.expect("读取目录项失败").path())
        .filter(|p| p.is_dir())
        .collect();
    dirs.sort();

    let mut resources = String::new();
    let mut infos = String::new();
    for dir in &dirs {
        let code = dir.file_name().unwrap().to_string_lossy().to_string();
        println!("cargo:rerun-if-changed={}", dir.display());
        let meta_path = dir.join(LOCALE_META_FILE);
        let text = fs::read_to_string(&meta_path)
            .unwrap_or_else(|_| panic!("语言目录 {code} 缺少 {LOCALE_META_FILE}"));
        println!("cargo:rerun-if-changed={}", meta_path.display());
        let meta = parse_meta(&text, &meta_path);
        let declared = meta
            .scalars
            .get("code")
            .unwrap_or_else(|| panic!("{}：缺少 code", meta_path.display()));
        assert_eq!(
            declared,
            &code,
            "{}：code 必须与目录名一致",
            meta_path.display()
        );
        let native = meta
            .scalars
            .get("native_name")
            .unwrap_or_else(|| panic!("{}：缺少 native_name", meta_path.display()));
        let empty = Vec::new();
        let aliases = meta.lists.get("aliases").unwrap_or(&empty);
        let prefixes = meta.lists.get("system_prefixes").unwrap_or(&empty);
        writeln!(
            infos,
            "    LocaleInfo {{ code: {}, config_value: {}, native_name: {}, aliases: {}, system_prefixes: {} }},",
            lit(&code),
            lit(&code.replace('-', "_")),
            lit(native),
            lit_list(aliases),
            lit_list(prefixes),
        )
        .unwrap();

        let mut files: Vec<PathBuf> = fs::read_dir(dir)
            .expect("读取语言目录失败")
            .map(|e| e.expect("读取目录项失败").path())
            .filter(|p| p.extension().is_some_and(|x| x == "ftl"))
            .collect();
        files.sort();
        for file in files {
            println!("cargo:rerun-if-changed={}", file.display());
            writeln!(
                resources,
                "    ({}, include_str!({})),",
                lit(&code),
                lit(&file.to_string_lossy())
            )
            .unwrap();
        }
    }
    let out = format!(
        "/// `(语言, ftl 文本)` 列表（构建时自动生成）。\npub const RESOURCES: &[(&str, &str)] = &[\n{resources}];\n\n/// 已发现的语言元数据（构建时自动生成）。\npub const LOCALES: &[LocaleInfo] = &[\n{infos}];\n"
    );
    let out_dir = PathBuf::from(std::env::var("OUT_DIR").expect("缺少 OUT_DIR"));
    fs::write(out_dir.join("embedded_generated.rs"), out).expect("写入生成文件失败");
}
