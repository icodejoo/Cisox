//! 依赖闭包黑名单：保证托盘宿主及其共用 crate 的传递依赖里没有 GPUI、图形、异步运行时等重东西
//! （方案：`docs/design/tray-host-split.md` §3 的第二层守卫）。
//!
//! 纯读 `Cargo.lock`，不起 cargo 进程：解析 `[[package]]` 与 `dependencies`，从守卫根出发做
//! 广度优先遍历，闭包里出现黑名单包就报告，并附上从根到它的依赖链，方便定位是谁引进来的。

use std::collections::btree_map::Entry;
use std::collections::{BTreeMap, BTreeSet, VecDeque};

/// 锁文件名（位于 workspace 根）。
pub const LOCK_FILE: &str = "Cargo.lock";

/// 必须存在、且闭包必须干净的守卫根（缺失说明改名或误删，按错误处理）。
pub const HOST_CLOSURE_ROOTS: [&str; 3] =
    ["snow-host-protocol", "snow-shell-core", "snow-data-paths"];

/// 可选的守卫根：crate 出现在锁文件里就检查，还没建出来（阶段 2 的 `snow-tray-host`）就跳过。
pub const HOST_CLOSURE_OPTIONAL_ROOTS: [&str; 1] = ["snow-tray-host"];

/// 闭包里禁止出现的包名（精确匹配）。
///
/// 前半是方案 §3 列的清单；`snow-capability` 是阶段 1 的约束（宿主不链能力表，`snow-shell-core`
/// 用字符串标识代替它）。
pub const FORBIDDEN_EXACT: [&str; 15] = [
    "snow-app-core",
    "snow-capability",
    "snow-config",
    "snow-i18n",
    "snow-history",
    "snow-translate",
    "tokio",
    "async-std",
    "smol",
    "futures-executor",
    "ort",
    "image",
    "tiny-skia",
    "resvg",
    "tracing-subscriber",
];

/// 闭包里禁止出现的包名前缀。
pub const FORBIDDEN_PREFIXES: [&str; 4] = ["gpui", "snow-ui", "snow-canvas-", "fluent"];

/// 只在非 Windows 平台编译的包：遍历时不沿着指向它们的边继续走。
///
/// `Cargo.lock` 不区分目标平台，而宿主只在 Windows 上构建（ADR-7）。`tray-icon` / `muda` 在 Linux 上
/// 会拉进 GTK 栈（`gtk` → `glib` → `futures-executor`），这些包永远不会进 Windows 二进制，
/// 不能算宿主的依赖。每一项都对应一个具体的 `[target.'cfg(target_os = "linux")'.dependencies]` 入口；
/// 新增条目前先确认它确实只被非 Windows 目标依赖。
pub const NON_WINDOWS_ONLY: [&str; 2] = ["gtk", "libappindicator"];

/// 锁文件里的一个包。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LockPackage {
    /// 包名。
    pub name: String,
    /// 版本。
    pub version: String,
    /// 依赖项（`name` 或 `name version`，来源后缀已去掉）。
    pub dependencies: Vec<DepRef>,
}

/// 对另一个包的引用；同名多版本共存时锁文件会带版本号。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DepRef {
    /// 被依赖包名。
    pub name: String,
    /// 被依赖包版本；锁文件里只有一个版本时省略。
    pub version: Option<String>,
}

/// 一条违规：守卫根的闭包里出现了黑名单包。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClosureViolation {
    /// 守卫根。
    pub root: String,
    /// 出现的黑名单包（`名称 版本`）。
    pub package: String,
    /// 违规原因（命中的规则）。
    pub rule: String,
    /// 从根到违规包的依赖链（包名）。
    pub chain: Vec<String>,
}

/// 检查失败的原因（区别于“有违规”）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClosureError {
    /// 必须存在的守卫根不在锁文件里。
    MissingRoot(String),
}

impl std::fmt::Display for ClosureError {
    /// 输出面向测试失败信息的描述。
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ClosureError::MissingRoot(name) => {
                write!(f, "守卫根 {name} 不在 Cargo.lock 里（改名或误删了？）")
            }
        }
    }
}

impl std::error::Error for ClosureError {}

/// 取形如 `key = "value"` 的行的值。
fn string_value<'a>(line: &'a str, key: &str) -> Option<&'a str> {
    let rest = line.strip_prefix(key)?.trim_start().strip_prefix('=')?;
    rest.trim().strip_prefix('"')?.strip_suffix('"')
}

/// 解析依赖项字符串：`name`、`name version`、`name version (source)`。
fn parse_dep(entry: &str) -> DepRef {
    let mut parts = entry.split_whitespace();
    DepRef {
        name: parts.next().unwrap_or_default().to_string(),
        version: parts.next().map(str::to_string),
    }
}

/// 解析 `Cargo.lock` 里的全部 `[[package]]`。
///
/// # 参数
/// - `text`：锁文件内容。
///
/// # 返回
/// 包列表（保持文件中的顺序）；`[metadata]`、`[[patch.unused]]` 等其他段落被忽略。
///
/// # 示例
/// ```
/// use workspace_guard::closure::parse_lock;
/// let lock = "[[package]]\nname = \"a\"\nversion = \"1.0.0\"\ndependencies = [\n \"b\",\n]\n";
/// let packages = parse_lock(lock);
/// assert_eq!(packages[0].dependencies[0].name, "b");
/// ```
pub fn parse_lock(text: &str) -> Vec<LockPackage> {
    let mut out: Vec<LockPackage> = Vec::new();
    let mut current: Option<LockPackage> = None;
    let mut in_deps = false;
    let mut flush = |current: &mut Option<LockPackage>| {
        if let Some(pkg) = current.take() {
            out.push(pkg);
        }
    };
    for raw in text.lines() {
        let line = raw.trim();
        if line == "[[package]]" {
            flush(&mut current);
            current = Some(LockPackage {
                name: String::new(),
                version: String::new(),
                dependencies: Vec::new(),
            });
            in_deps = false;
            continue;
        }
        if line.starts_with('[') && !in_deps {
            // 其他段落（metadata、patch.unused）：当前包到此结束
            flush(&mut current);
            continue;
        }
        let Some(pkg) = current.as_mut() else {
            continue;
        };
        if in_deps {
            if line.starts_with(']') {
                in_deps = false;
            } else if let Some(entry) = line.trim_end_matches(',').trim().strip_prefix('"')
                && let Some(entry) = entry.strip_suffix('"')
            {
                pkg.dependencies.push(parse_dep(entry));
            }
        } else if let Some(name) = string_value(line, "name") {
            pkg.name = name.to_string();
        } else if let Some(version) = string_value(line, "version") {
            pkg.version = version.to_string();
        } else if let Some(rest) = line.strip_prefix("dependencies") {
            let rest = rest.trim_start().strip_prefix('=').map(str::trim);
            // `dependencies = []`（单行空列表）不进入多行状态
            if rest == Some("[") {
                in_deps = true;
            }
        }
    }
    flush(&mut current);
    out
}

/// 包名命中的黑名单规则；不在黑名单返回 `None`。
///
/// # 参数
/// - `name`：包名。
///
/// # 示例
/// ```
/// use workspace_guard::closure::forbidden_rule;
/// assert!(forbidden_rule("gpui-pre").is_some());
/// assert!(forbidden_rule("tokio").is_some());
/// assert!(forbidden_rule("serde").is_none());
/// ```
pub fn forbidden_rule(name: &str) -> Option<String> {
    if FORBIDDEN_EXACT.contains(&name) {
        return Some(format!("禁止依赖 {name}"));
    }
    FORBIDDEN_PREFIXES
        .iter()
        .find(|prefix| name.starts_with(**prefix))
        .map(|prefix| format!("禁止依赖 {prefix}* 系列（{name}）"))
}

/// 包的唯一键：`(名称, 版本)`。
type PackageKey = (String, String);

/// 从某个根出发的传递闭包（含根本身）。
///
/// 返回每个包到其 BFS 前驱的映射，用于还原依赖链；根的前驱为 `None`。
fn closure_from(
    packages: &[LockPackage],
    root: &LockPackage,
) -> BTreeMap<PackageKey, Option<PackageKey>> {
    let mut by_name: BTreeMap<&str, Vec<&LockPackage>> = BTreeMap::new();
    for pkg in packages {
        by_name.entry(pkg.name.as_str()).or_default().push(pkg);
    }
    let key = |pkg: &LockPackage| (pkg.name.clone(), pkg.version.clone());
    let mut parent: BTreeMap<PackageKey, Option<PackageKey>> = BTreeMap::new();
    let mut queue: VecDeque<&LockPackage> = VecDeque::new();
    parent.insert(key(root), None);
    queue.push_back(root);
    while let Some(pkg) = queue.pop_front() {
        for dep in &pkg.dependencies {
            if NON_WINDOWS_ONLY.contains(&dep.name.as_str()) {
                continue;
            }
            let Some(candidates) = by_name.get(dep.name.as_str()) else {
                continue;
            };
            for target in candidates {
                if dep.version.as_deref().is_some_and(|v| v != target.version) {
                    continue;
                }
                if let Entry::Vacant(slot) = parent.entry(key(target)) {
                    slot.insert(Some(key(pkg)));
                    queue.push_back(target);
                }
            }
        }
    }
    parent
}

/// 还原从根到 `target` 的依赖链（只含包名）。
fn chain_to(parent: &BTreeMap<PackageKey, Option<PackageKey>>, target: &PackageKey) -> Vec<String> {
    let mut chain = vec![target.0.clone()];
    let mut cursor = parent.get(target).cloned().flatten();
    while let Some(key) = cursor {
        chain.push(key.0.clone());
        cursor = parent.get(&key).cloned().flatten();
    }
    chain.reverse();
    chain
}

/// 检查指定守卫根的闭包，返回全部违规。
///
/// # 参数
/// - `packages`：[`parse_lock`] 的结果。
/// - `roots`：守卫根包名；同名多版本的根都会检查。
///
/// # 返回
/// 违规列表（按根、再按包名排序）。
pub fn check_roots(packages: &[LockPackage], roots: &[&str]) -> Vec<ClosureViolation> {
    let mut violations = Vec::new();
    for root_name in roots {
        for root in packages.iter().filter(|p| p.name == *root_name) {
            let parent = closure_from(packages, root);
            let seen: BTreeSet<&PackageKey> = parent.keys().collect();
            for key in seen {
                if let Some(rule) = forbidden_rule(&key.0) {
                    violations.push(ClosureViolation {
                        root: (*root_name).to_string(),
                        package: format!("{} {}", key.0, key.1),
                        rule,
                        chain: chain_to(&parent, key),
                    });
                }
            }
        }
    }
    violations
}

/// 对宿主依赖闭包规则做完整检查：必需根必须存在，可选根存在才检查。
///
/// # 参数
/// - `lock_text`：`Cargo.lock` 内容。
///
/// # 返回
/// 违规列表（空表示通过）；必需根缺失返回 [`ClosureError`]。
///
/// # 示例
/// ```
/// use workspace_guard::closure::{ClosureError, check_host_closure};
/// assert!(matches!(check_host_closure(""), Err(ClosureError::MissingRoot(_))));
/// ```
pub fn check_host_closure(lock_text: &str) -> Result<Vec<ClosureViolation>, ClosureError> {
    let packages = parse_lock(lock_text);
    let has = |name: &str| packages.iter().any(|p| p.name == name);
    for root in HOST_CLOSURE_ROOTS {
        if !has(root) {
            return Err(ClosureError::MissingRoot(root.to_string()));
        }
    }
    let mut roots: Vec<&str> = HOST_CLOSURE_ROOTS.to_vec();
    roots.extend(HOST_CLOSURE_OPTIONAL_ROOTS.iter().filter(|r| has(r)));
    Ok(check_roots(&packages, &roots))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    /// 构造一个包的锁文件片段。
    fn pkg(name: &str, version: &str, deps: &[&str]) -> String {
        let list: String = deps.iter().map(|d| format!(" \"{d}\",\n")).collect();
        format!(
            "[[package]]\nname = \"{name}\"\nversion = \"{version}\"\nsource = \"registry+x\"\ndependencies = [\n{list}]\n\n"
        )
    }

    /// 解析：名称、版本、依赖（含带版本与来源的写法）、空依赖、忽略其他段落。
    #[test]
    fn parses_packages_and_dependencies() {
        let lock = format!(
            "# 注释\nversion = 4\n\n{}[[package]]\nname = \"leaf\"\nversion = \"0.1.0\"\ndependencies = []\n\n{}[metadata]\nname = \"ignored\"\n",
            pkg(
                "root",
                "1.0.0",
                &["a", "b 2.0.0", "c 3.0.0 (registry+https://x)"]
            ),
            pkg("tail", "9.9.9", &["leaf"]),
        );
        let packages = parse_lock(&lock);
        let names: Vec<_> = packages.iter().map(|p| p.name.as_str()).collect();
        assert_eq!(names, vec!["root", "leaf", "tail"]);
        let root = &packages[0];
        assert_eq!(root.version, "1.0.0");
        assert_eq!(
            root.dependencies,
            vec![
                DepRef {
                    name: "a".into(),
                    version: None
                },
                DepRef {
                    name: "b".into(),
                    version: Some("2.0.0".into())
                },
                DepRef {
                    name: "c".into(),
                    version: Some("3.0.0".into())
                },
            ]
        );
        assert!(packages[1].dependencies.is_empty());
    }

    /// 闭包是传递的：根 → 中间 → 黑名单包，违规带完整依赖链。
    #[test]
    fn transitive_violation_reports_chain() {
        let lock = [
            pkg("host", "0.1.0", &["mid"]),
            pkg("mid", "1.0.0", &["gpui-pre"]),
            pkg("gpui-pre", "0.3.7", &[]),
            pkg("unrelated", "1.0.0", &["tokio"]),
            pkg("tokio", "1.0.0", &[]),
        ]
        .concat();
        let packages = parse_lock(&lock);
        let found = check_roots(&packages, &["host"]);
        assert_eq!(found.len(), 1, "{found:?}");
        assert_eq!(found[0].package, "gpui-pre 0.3.7");
        assert_eq!(found[0].chain, vec!["host", "mid", "gpui-pre"]);
        assert!(found[0].rule.contains("gpui"));
    }

    /// 同名多版本：只沿被依赖的那个版本走，另一个版本里的黑名单依赖不算。
    #[test]
    fn follows_exact_versions() {
        let lock = [
            pkg("host", "0.1.0", &["lib 1.0.0"]),
            pkg("lib", "1.0.0", &[]),
            pkg("lib", "2.0.0", &["tokio"]),
            pkg("tokio", "1.0.0", &[]),
        ]
        .concat();
        let packages = parse_lock(&lock);
        assert!(check_roots(&packages, &["host"]).is_empty());
        let lock2 = lock.replace("\"lib 1.0.0\"", "\"lib 2.0.0\"");
        let found = check_roots(&parse_lock(&lock2), &["host"]);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].chain, vec!["host", "lib", "tokio"]);
    }

    /// 只在非 Windows 平台编译的入口不计入闭包；同样的包经别的路径进来仍然会被抓到。
    #[test]
    fn non_windows_only_edges_are_skipped() {
        let lock = [
            pkg("host", "0.1.0", &["tray", "gtk"]),
            pkg("tray", "1.0.0", &["libappindicator"]),
            pkg("libappindicator", "1.0.0", &["glib"]),
            pkg("gtk", "1.0.0", &["glib"]),
            pkg("glib", "1.0.0", &["futures-executor"]),
            pkg("futures-executor", "1.0.0", &[]),
        ]
        .concat();
        assert!(check_roots(&parse_lock(&lock), &["host"]).is_empty());
        let direct = lock.replace(
            " \"gtk\",
",
            " \"glib\",
",
        );
        let found = check_roots(&parse_lock(&direct), &["host"]);
        assert_eq!(found.len(), 1, "{found:?}");
        assert_eq!(found[0].chain, vec!["host", "glib", "futures-executor"]);
    }

    /// 环形依赖不会死循环。
    #[test]
    fn handles_cycles() {
        let lock = [pkg("a", "1.0.0", &["b"]), pkg("b", "1.0.0", &["a"])].concat();
        assert!(check_roots(&parse_lock(&lock), &["a"]).is_empty());
    }

    /// 黑名单规则：精确名与前缀，near-miss 不误报。
    #[test]
    fn rule_matching() {
        for name in [
            "gpui",
            "gpui-pre-windows",
            "gpui-component",
            "snow-ui-shell",
            "snow-ui",
            "snow-canvas-raster",
            "snow-app-core",
            "snow-capability",
            "snow-config",
            "snow-i18n",
            "snow-history",
            "snow-translate",
            "tokio",
            "async-std",
            "smol",
            "futures-executor",
            "ort",
            "image",
            "tiny-skia",
            "resvg",
            "fluent-bundle",
            "fluent",
            "tracing-subscriber",
        ] {
            assert!(forbidden_rule(name).is_some(), "{name} 应被禁止");
        }
        for name in [
            "serde",
            "serde_json",
            "tracing",
            "tray-icon",
            "global-hotkey",
            "windows",
            "snow-shell-core",
            "snow-host-protocol",
            "snow-data-paths",
            "snow-platform",
            "image-webp",
            "tokio-util",
            "smolstr",
            "sort",
        ] {
            assert!(forbidden_rule(name).is_none(), "{name} 不应被禁止");
        }
    }

    /// 必需根缺失是错误；可选根缺失被跳过；可选根存在时同样检查。
    #[test]
    fn required_and_optional_roots() {
        assert_eq!(
            check_host_closure(&pkg("snow-host-protocol", "0.1.0", &[])),
            Err(ClosureError::MissingRoot("snow-shell-core".into()))
        );
        let required = [
            pkg("snow-host-protocol", "0.1.0", &["serde"]),
            pkg("snow-shell-core", "0.1.0", &["tray-icon"]),
            pkg("snow-data-paths", "0.1.0", &[]),
            pkg("serde", "1.0.0", &[]),
            pkg("tray-icon", "0.25.1", &[]),
        ]
        .concat();
        assert_eq!(check_host_closure(&required), Ok(vec![]));
        let with_host = format!(
            "{required}{}{}",
            pkg("snow-tray-host", "0.1.0", &["snow-ui-shell"]),
            pkg("snow-ui-shell", "0.1.0", &[])
        );
        let found = check_host_closure(&with_host).unwrap();
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].root, "snow-tray-host");
        assert_eq!(found[0].chain, vec!["snow-tray-host", "snow-ui-shell"]);
    }

    /// 真实锁文件：守卫根都在，闭包零违规。
    #[test]
    fn real_lockfile_host_closure_is_clean() {
        let path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../..")
            .join(LOCK_FILE);
        let text = std::fs::read_to_string(&path).expect("读取 Cargo.lock");
        let found = check_host_closure(&text).unwrap_or_else(|e| panic!("{e}"));
        assert!(
            found.is_empty(),
            "宿主依赖闭包里出现禁用的包:\n{}",
            found
                .iter()
                .map(|v| format!("  [{}] {} ← {}", v.root, v.package, v.chain.join(" → ")))
                .collect::<Vec<_>>()
                .join("\n")
        );
    }

    /// 金丝雀：解析器与遍历在真实锁文件上确实能走通——`snow-ui-shell` 的闭包里必然有 gpui，
    /// `snow-shell-core` 的闭包里必然有托盘库。否则"零违规"可能只是解析出了空闭包。
    #[test]
    fn real_lockfile_canaries() {
        let path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../..")
            .join(LOCK_FILE);
        let packages = parse_lock(&std::fs::read_to_string(&path).expect("读取 Cargo.lock"));
        let shell = check_roots(&packages, &["snow-ui-shell"]);
        assert!(
            shell.iter().any(|v| v.package.starts_with("gpui-pre ")),
            "snow-ui-shell 的闭包里应该能走到 gpui-pre"
        );
        let core = packages
            .iter()
            .find(|p| p.name == "snow-shell-core")
            .expect("snow-shell-core 在锁文件里");
        assert!(core.dependencies.iter().any(|d| d.name == "tray-icon"));
        let parent = closure_from(&packages, core);
        assert!(parent.keys().any(|(name, _)| name == "global-hotkey"));
    }
}
