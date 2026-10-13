//! 宿主 exe 导入表闸门：读 PE 文件的静态导入与延迟导入，检查 DLL 在白名单内
//! （方案 `docs/design/tray-host-split.md` §3 的第三层守卫、§12.6）。
//!
//! 依赖闭包规则管的是 Cargo 依赖图，管不到“链接进来的 crate 引用了哪些系统 DLL”；而宿主的目标是
//! 导入表里没有 D3D / DXGI / DWrite / UIA / 图像编解码 / 输入法这类会在进程启动时被加载、
//! 涨内存的 DLL。这里直接从二进制层面验证，不需要 `dumpbin`（不依赖 MSVC 环境），CI 与本机都能跑。
//!
//! 白名单以阶段 2 实际构建出的宿主导入表定稿（见 [`ALLOWED_EXACT`] 的说明）。

use std::fmt;

/// 一定允许的系统 DLL（小写，精确匹配）。
///
/// 阶段 2 实际构建出的宿主导入表（release，2026-10-12）：
/// - `kernel32` / `user32` / `shell32` / `ntdll` / `advapi32`：进程与句柄、窗口与热键、托盘图标、Rust 运行时、
///   命名管道 ACL（令牌 / SID）；
/// - `gdi32` / `comctl32`：`tray-icon` / `muda` 的托盘窗口与菜单（研究文档 §3.3：比纯 Win32 多一个 `comctl32`）；
/// - `imm32`：`ImmDisableIME(-1)` 本身；`bcryptprimitives`：Rust 标准库的随机数；
/// - `vcruntime140`：动态 CRT（`+crt-static` 只能全局设置，会连带 UI exe，没有采纳；研究文档 §3.2 实测
///   静态 / 动态 CRT 的提交无差别）；
/// - `oleaut32`：`windows` crate 的错误类型（`windows::core::Error` 的 `GetErrorInfo`）。`snow-platform`
///   的单实例 / 崩溃转储与 `snow-shell-core` 的消息循环都用它；全部改写成 `windows-sys` 才能去掉，
///   研究文档 §3.3 实测整体只差约 0.1MB 提交，留给阶段 6（有数据才做）。
///
/// 阶段 0 探针的白名单里没有 `oleaut32`，实测构建后加入；`dwmapi`（前台全屏探测）与 `bcrypt`（随机数）
/// 已通过运行时加载 / 改用 `ProcessPrng` 去掉，不在这里放行。
pub const ALLOWED_EXACT: [&str; 11] = [
    "kernel32.dll",
    "user32.dll",
    "shell32.dll",
    "gdi32.dll",
    "comctl32.dll",
    "imm32.dll",
    "ntdll.dll",
    "advapi32.dll",
    "bcryptprimitives.dll",
    "vcruntime140.dll",
    "oleaut32.dll",
];

/// 一定允许的系统 DLL 前缀（小写）：Windows API 集合转发器（`api-ms-win-*`）。
pub const ALLOWED_PREFIXES: [&str; 1] = ["api-ms-win-"];

/// 明确禁止的 DLL 前缀（小写）：图形、输入法、自动化、图像编解码、媒体——宿主绝不能把它们拉进进程。
pub const FORBIDDEN_PREFIXES: [&str; 16] = [
    "d3d",
    "dxgi",
    "dcomp",
    "dwrite",
    "d2d1",
    "dwmapi",
    "uiautomationcore",
    "oleacc",
    "windowscodecs",
    "gdiplus",
    "mfplat",
    "mf.",
    "icuuc",
    "icu.",
    "opengl32",
    "winmm",
];

/// PE 解析失败的原因。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ImportError {
    /// 不是 PE 文件（缺 MZ / PE 签名）。
    NotPe,
    /// 文件被截断或头部字段越界。
    Truncated,
    /// 不是 64 位 PE（宿主只发 x64）。
    Unsupported32Bit,
}

impl fmt::Display for ImportError {
    /// 输出面向日志的错误描述。
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ImportError::NotPe => write!(f, "不是有效的 PE 文件"),
            ImportError::Truncated => write!(f, "PE 文件被截断或头部字段越界"),
            ImportError::Unsupported32Bit => write!(f, "只支持 64 位 PE"),
        }
    }
}

impl std::error::Error for ImportError {}

/// 一个 PE 文件的导入 DLL 清单（小写，去重保序）。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PeImports {
    /// 静态导入：进程启动时就会被加载。
    pub static_dlls: Vec<String>,
    /// 延迟导入：第一次调用时才加载（宿主不应有，有也要在白名单内）。
    pub delay_dlls: Vec<String>,
}

/// 检查结果。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ImportReport {
    /// 命中禁止前缀的 DLL（原因更严重）。
    pub forbidden: Vec<String>,
    /// 不在白名单里、也没命中禁止前缀的 DLL（需要人工判断后更新白名单，或去掉对应依赖）。
    pub unlisted: Vec<String>,
}

impl ImportReport {
    /// 是否通过。
    pub fn is_clean(&self) -> bool {
        self.forbidden.is_empty() && self.unlisted.is_empty()
    }
}

/// 读 `u16`（小端）；越界返回 `Truncated`。
fn u16_at(bytes: &[u8], at: usize) -> Result<u16, ImportError> {
    bytes
        .get(at..at + 2)
        .map(|b| u16::from_le_bytes([b[0], b[1]]))
        .ok_or(ImportError::Truncated)
}

/// 读 `u32`（小端）；越界返回 `Truncated`。
fn u32_at(bytes: &[u8], at: usize) -> Result<u32, ImportError> {
    bytes
        .get(at..at + 4)
        .map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
        .ok_or(ImportError::Truncated)
}

/// 一个节的映射信息。
#[derive(Debug, Clone, Copy)]
struct Section {
    /// 内存里的起始 RVA。
    virtual_address: u32,
    /// 内存里的大小。
    virtual_size: u32,
    /// 文件里的起始偏移。
    raw_pointer: u32,
    /// 文件里的大小。
    raw_size: u32,
}

/// RVA 换算成文件偏移；不在任何节里返回 `None`。
fn rva_to_offset(sections: &[Section], rva: u32) -> Option<usize> {
    sections.iter().find_map(|s| {
        let size = s.virtual_size.max(s.raw_size);
        (rva >= s.virtual_address && rva < s.virtual_address + size)
            .then(|| (rva - s.virtual_address + s.raw_pointer) as usize)
    })
}

/// 读以 NUL 结尾的 ASCII 字符串（上限 256 字节）。
fn c_string_at(bytes: &[u8], at: usize) -> Result<String, ImportError> {
    let tail = bytes.get(at..).ok_or(ImportError::Truncated)?;
    let end = tail
        .iter()
        .take(256)
        .position(|b| *b == 0)
        .ok_or(ImportError::Truncated)?;
    Ok(String::from_utf8_lossy(&tail[..end]).to_ascii_lowercase())
}

/// 往清单里加一个名字（去重保序）。
fn push_unique(list: &mut Vec<String>, name: String) {
    if !list.contains(&name) {
        list.push(name);
    }
}

/// 数据目录项：静态导入表。
const DIR_IMPORT: usize = 1;

/// 数据目录项：延迟导入表。
const DIR_DELAY_IMPORT: usize = 13;

/// 静态导入描述符大小（字节）。
const IMPORT_DESCRIPTOR_SIZE: usize = 20;

/// 延迟导入描述符大小（字节）。
const DELAY_DESCRIPTOR_SIZE: usize = 32;

/// 解析 PE 文件的导入 DLL 清单。
///
/// # 参数
/// - `bytes`：PE 文件全部内容。
///
/// # 返回
/// 静态与延迟导入的 DLL 名（小写）；格式不对返回 [`ImportError`]。
///
/// ```no_run
/// let bytes = std::fs::read("cisox.exe").unwrap();
/// let imports = workspace_guard::imports::parse_imports(&bytes).unwrap();
/// println!("{:?}", imports.static_dlls);
/// ```
pub fn parse_imports(bytes: &[u8]) -> Result<PeImports, ImportError> {
    if bytes.get(0..2) != Some(b"MZ") {
        return Err(ImportError::NotPe);
    }
    let pe = u32_at(bytes, 0x3c)? as usize;
    if bytes.get(pe..pe + 4) != Some(b"PE\0\0") {
        return Err(ImportError::NotPe);
    }
    let coff = pe + 4;
    let section_count = usize::from(u16_at(bytes, coff + 2)?);
    let optional_size = usize::from(u16_at(bytes, coff + 16)?);
    let optional = coff + 20;
    if u16_at(bytes, optional)? != 0x20b {
        return Err(ImportError::Unsupported32Bit);
    }
    let dir_count = u32_at(bytes, optional + 108)? as usize;
    let directories = optional + 112;
    let section_table = optional + optional_size;
    let mut sections = Vec::with_capacity(section_count);
    for i in 0..section_count {
        let at = section_table + i * 40;
        sections.push(Section {
            virtual_size: u32_at(bytes, at + 8)?,
            virtual_address: u32_at(bytes, at + 12)?,
            raw_size: u32_at(bytes, at + 16)?,
            raw_pointer: u32_at(bytes, at + 20)?,
        });
    }
    let directory = |index: usize| -> Result<Option<u32>, ImportError> {
        if index >= dir_count {
            return Ok(None);
        }
        let rva = u32_at(bytes, directories + index * 8)?;
        Ok((rva != 0).then_some(rva))
    };
    let mut imports = PeImports::default();
    if let Some(rva) = directory(DIR_IMPORT)? {
        let mut at = rva_to_offset(&sections, rva).ok_or(ImportError::Truncated)?;
        loop {
            // 描述符里 Name 在偏移 12；全零描述符结束
            let name_rva = u32_at(bytes, at + 12)?;
            if name_rva == 0 && u32_at(bytes, at)? == 0 {
                break;
            }
            let name_at = rva_to_offset(&sections, name_rva).ok_or(ImportError::Truncated)?;
            push_unique(&mut imports.static_dlls, c_string_at(bytes, name_at)?);
            at += IMPORT_DESCRIPTOR_SIZE;
        }
    }
    if let Some(rva) = directory(DIR_DELAY_IMPORT)? {
        let mut at = rva_to_offset(&sections, rva).ok_or(ImportError::Truncated)?;
        loop {
            // 延迟导入描述符：Attributes(0) / DllNameRVA(4)；全零结束
            let name_rva = u32_at(bytes, at + 4)?;
            if name_rva == 0 && u32_at(bytes, at)? == 0 {
                break;
            }
            let name_at = rva_to_offset(&sections, name_rva).ok_or(ImportError::Truncated)?;
            push_unique(&mut imports.delay_dlls, c_string_at(bytes, name_at)?);
            at += DELAY_DESCRIPTOR_SIZE;
        }
    }
    Ok(imports)
}

/// 一个 DLL 名是否命中禁止前缀。
fn is_forbidden(name: &str) -> bool {
    FORBIDDEN_PREFIXES.iter().any(|p| name.starts_with(p))
}

/// 一个 DLL 名是否在白名单内。
fn is_allowed(name: &str) -> bool {
    ALLOWED_EXACT.contains(&name) || ALLOWED_PREFIXES.iter().any(|p| name.starts_with(p))
}

/// 按白名单 / 黑名单检查导入清单（静态与延迟导入一并检查）。
///
/// # 参数
/// - `imports`：[`parse_imports`] 的结果。
///
/// # 返回
/// 检查结果；[`ImportReport::is_clean`] 为真表示通过。
///
/// ```
/// use workspace_guard::imports::{PeImports, check_imports};
/// let imports = PeImports {
///     static_dlls: vec!["kernel32.dll".into(), "d3d11.dll".into()],
///     delay_dlls: vec![],
/// };
/// let report = check_imports(&imports);
/// assert_eq!(report.forbidden, vec!["d3d11.dll"]);
/// ```
pub fn check_imports(imports: &PeImports) -> ImportReport {
    let mut report = ImportReport::default();
    for name in imports.static_dlls.iter().chain(&imports.delay_dlls) {
        if is_forbidden(name) {
            push_unique(&mut report.forbidden, name.clone());
        } else if !is_allowed(name) {
            push_unique(&mut report.unlisted, name.clone());
        }
    }
    report
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 在 `buf` 的 `at` 处写小端 u16。
    fn put16(buf: &mut [u8], at: usize, v: u16) {
        buf[at..at + 2].copy_from_slice(&v.to_le_bytes());
    }

    /// 在 `buf` 的 `at` 处写小端 u32。
    fn put32(buf: &mut [u8], at: usize, v: u32) {
        buf[at..at + 4].copy_from_slice(&v.to_le_bytes());
    }

    /// 造一个最小的 PE64：一个 `.idata` 节（RVA 0x1000，文件偏移 0x400），放静态与延迟导入表。
    fn synthetic_pe(statics: &[&str], delays: &[&str]) -> Vec<u8> {
        let mut buf = vec![0u8; 0x400 + 0x1000];
        buf[0..2].copy_from_slice(b"MZ");
        put32(&mut buf, 0x3c, 0x80);
        buf[0x80..0x84].copy_from_slice(b"PE\0\0");
        let coff = 0x84;
        put16(&mut buf, coff, 0x8664);
        put16(&mut buf, coff + 2, 1);
        put16(&mut buf, coff + 16, 0xf0);
        let optional = coff + 20;
        put16(&mut buf, optional, 0x20b);
        put32(&mut buf, optional + 108, 16);
        // 节表
        let section = optional + 0xf0;
        buf[section..section + 6].copy_from_slice(b".idata");
        put32(&mut buf, section + 8, 0x1000);
        put32(&mut buf, section + 12, 0x1000);
        put32(&mut buf, section + 16, 0x1000);
        put32(&mut buf, section + 20, 0x400);
        // 节内布局：静态描述符 @0x000，延迟描述符 @0x200，名字 @0x400 起
        let base = 0x400usize;
        let mut name_cursor = 0x400usize;
        let mut place_name = |buf: &mut Vec<u8>, name: &str| -> u32 {
            let rva = 0x1000 + name_cursor as u32;
            let at = base + name_cursor;
            buf[at..at + name.len()].copy_from_slice(name.as_bytes());
            name_cursor += name.len() + 1;
            rva
        };
        for (i, name) in statics.iter().enumerate() {
            let rva = place_name(&mut buf, name);
            put32(&mut buf, base + i * IMPORT_DESCRIPTOR_SIZE + 12, rva);
            // 让描述符第一个字段非零，避免与结束标记混淆
            put32(&mut buf, base + i * IMPORT_DESCRIPTOR_SIZE, 1);
        }
        for (i, name) in delays.iter().enumerate() {
            let rva = place_name(&mut buf, name);
            put32(&mut buf, base + 0x200 + i * DELAY_DESCRIPTOR_SIZE, 1);
            put32(&mut buf, base + 0x200 + i * DELAY_DESCRIPTOR_SIZE + 4, rva);
        }
        let directories = optional + 112;
        if !statics.is_empty() {
            put32(&mut buf, directories + DIR_IMPORT * 8, 0x1000);
        }
        if !delays.is_empty() {
            put32(&mut buf, directories + DIR_DELAY_IMPORT * 8, 0x1200);
        }
        buf
    }

    /// 解析合成 PE：静态与延迟导入分开列出，名字转小写、去重保序。
    #[test]
    fn parses_static_and_delay_imports() {
        let pe = synthetic_pe(
            &["KERNEL32.dll", "USER32.dll", "kernel32.dll"],
            &["Dbghelp.dll"],
        );
        let imports = parse_imports(&pe).unwrap();
        assert_eq!(imports.static_dlls, vec!["kernel32.dll", "user32.dll"]);
        assert_eq!(imports.delay_dlls, vec!["dbghelp.dll"]);
    }

    /// 没有导入表的 PE：清单为空，不是错误。
    #[test]
    fn empty_import_tables_are_fine() {
        let imports = parse_imports(&synthetic_pe(&[], &[])).unwrap();
        assert_eq!(imports, PeImports::default());
    }

    /// 格式错误：非 PE、截断、32 位 PE。
    #[test]
    fn malformed_files_are_rejected() {
        assert_eq!(parse_imports(b"hello"), Err(ImportError::NotPe));
        assert_eq!(parse_imports(b""), Err(ImportError::NotPe));
        let mut pe = synthetic_pe(&["kernel32.dll"], &[]);
        pe.truncate(0x90);
        assert_eq!(parse_imports(&pe), Err(ImportError::Truncated));
        let mut pe32 = synthetic_pe(&["kernel32.dll"], &[]);
        put16(&mut pe32, 0x84 + 20, 0x10b);
        assert_eq!(parse_imports(&pe32), Err(ImportError::Unsupported32Bit));
        let mut bad_sig = synthetic_pe(&[], &[]);
        bad_sig[0x80] = b'X';
        assert_eq!(parse_imports(&bad_sig), Err(ImportError::NotPe));
    }

    /// 白名单：精确名与 api-ms-win-* 前缀放行；大小写已在解析时统一。
    #[test]
    fn allowed_imports_pass() {
        let imports = PeImports {
            static_dlls: vec![
                "kernel32.dll".into(),
                "user32.dll".into(),
                "api-ms-win-crt-runtime-l1-1-0.dll".into(),
                "api-ms-win-core-synch-l1-2-0.dll".into(),
            ],
            delay_dlls: vec![],
        };
        assert!(check_imports(&imports).is_clean());
    }

    /// 禁止前缀：图形、输入法 / 自动化、图像编解码、媒体，静态与延迟导入都抓。
    #[test]
    fn forbidden_imports_are_reported_even_when_delay_loaded() {
        let imports = PeImports {
            static_dlls: vec![
                "kernel32.dll".into(),
                "d3d11.dll".into(),
                "dxgi.dll".into(),
                "uiautomationcore.dll".into(),
            ],
            delay_dlls: vec!["windowscodecs.dll".into(), "dwrite.dll".into()],
        };
        let report = check_imports(&imports);
        assert_eq!(
            report.forbidden,
            vec![
                "d3d11.dll",
                "dxgi.dll",
                "uiautomationcore.dll",
                "windowscodecs.dll",
                "dwrite.dll"
            ]
        );
        assert!(report.unlisted.is_empty());
        assert!(!report.is_clean());
    }

    /// 既不在白名单也不在黑名单的 DLL 单独列出（需要人工判断）。
    #[test]
    fn unknown_imports_are_listed_separately() {
        let imports = PeImports {
            static_dlls: vec!["kernel32.dll".into(), "mystery.dll".into()],
            delay_dlls: vec![],
        };
        let report = check_imports(&imports);
        assert_eq!(report.unlisted, vec!["mystery.dll"]);
        assert!(report.forbidden.is_empty());
    }

    /// 金丝雀：解析真实链接器产物——本测试可执行文件自己，必然导入 kernel32。
    /// 否则“宿主导入表干净”可能只是解析器什么都没读到。
    #[cfg(windows)]
    #[test]
    fn parses_a_real_linker_output() {
        let exe = std::env::current_exe().unwrap();
        let imports = parse_imports(&std::fs::read(exe).unwrap()).unwrap();
        assert!(
            imports.static_dlls.iter().any(|d| d == "kernel32.dll"),
            "{:?}",
            imports.static_dlls
        );
    }
}
