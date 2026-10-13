//! 工作进程共用的版本资源嵌入（各 worker 的 `build.rs` 用 `#[path]` 当模块引入，本文件不是独立 crate）。
//!
//! worker 是独立工作区，不想为这点事新增 `winresource` 依赖：这里只调系统 SDK 的 `rc.exe`
//! 编出 `.res`，再交给链接器；效果与主工作区宿主 / UI 的版本资源一致（`FileDescription` 等）。
//! 找不到 `rc.exe` 时只给构建警告、不让构建失败（产物照常生成，只是没有版本信息）。

use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

/// 产品名（与 `snow_data_paths::PRODUCT_NAME` 同值；构建脚本不能依赖本工作区的 crate）。
const VERSION_PRODUCT_NAME: &str = "Cisox";

/// 版本资源语言（美国英语）与代码页（Unicode），与 winresource 的默认值一致。
const VERSION_LANG_BLOCK: &str = "040904b0";

/// 把 `10.0.22621.0` 形式的 SDK 版本目录名拆成数字，用来按版本排序；不是版本号返回 `None`。
fn sdk_version_key(name: &str) -> Option<Vec<u32>> {
    name.split('.').map(|part| part.parse().ok()).collect()
}

/// 在 `Windows Kits\10\bin` 下找最新一版的 x64 `rc.exe`。
fn find_sdk_rc_exe() -> Option<PathBuf> {
    let kits = env::var_os("ProgramFiles(x86)")
        .or_else(|| env::var_os("ProgramFiles"))
        .map(|root| {
            PathBuf::from(root)
                .join("Windows Kits")
                .join("10")
                .join("bin")
        })?;
    fs::read_dir(&kits)
        .ok()?
        .filter_map(Result::ok)
        .filter_map(|entry| {
            let key = sdk_version_key(&entry.file_name().to_string_lossy())?;
            let rc = entry.path().join("x64").join("rc.exe");
            rc.is_file().then_some((key, rc))
        })
        .max_by(|a, b| a.0.cmp(&b.0))
        .map(|(_, rc)| rc)
}

/// 找 `rc.exe`：`RC_PATH` 环境变量 → Windows SDK 里最新的一版 → `PATH`。
///
/// `PATH` 放最后：别的工具也可能带一个同名 `rc.exe`（本机的 bun 就有一个，什么也不产出）。
fn find_rc_exe() -> Option<PathBuf> {
    if let Some(path) = env::var_os("RC_PATH") {
        return Some(PathBuf::from(path));
    }
    find_sdk_rc_exe().or_else(|| {
        env::var_os("PATH")
            .into_iter()
            .flat_map(|paths| env::split_paths(&paths).collect::<Vec<_>>())
            .map(|dir| dir.join("rc.exe"))
            .find(|candidate| candidate.is_file())
    })
}

/// 转义 `.rc` 字符串里的反斜杠与双引号。
fn rc_escape(text: &str) -> String {
    text.replace('\\', "\\\\").replace('"', "\"\"")
}

/// 给 worker 嵌入版本资源并让链接器带上它；非 Windows 目标直接跳过。
///
/// # 参数
/// - `description`：`FileDescription`（任务管理器里显示的进程描述），如 `Cisox recorder`。
/// - `original_filename`：`OriginalFilename`，如 `cisox-recorder.exe`。
pub fn embed_worker_version(description: &str, original_filename: &str) {
    println!("cargo:rerun-if-changed=../common/worker_version.rs");
    println!("cargo:rerun-if-env-changed=RC_PATH");
    if env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }
    let Some(rc_exe) = find_rc_exe() else {
        println!("cargo:warning=找不到 rc.exe，跳过版本资源（FileDescription 将为空）");
        return;
    };
    let out_dir = PathBuf::from(env::var_os("OUT_DIR").expect("OUT_DIR"));
    let version = env::var("CARGO_PKG_VERSION").unwrap_or_else(|_| "0.0.0".to_string());
    let [major, minor, patch] = ["MAJOR", "MINOR", "PATCH"]
        .map(|part| env::var(format!("CARGO_PKG_VERSION_{part}")).unwrap_or_else(|_| "0".into()));
    let numeric = format!("{major},{minor},{patch},0");
    let lang = VERSION_LANG_BLOCK;
    let text = format!(
        "#pragma code_page(65001)\n\
         1 VERSIONINFO\n\
         FILEVERSION {numeric}\n\
         PRODUCTVERSION {numeric}\n\
         FILEOS 0x40004\n\
         FILETYPE 1\n\
         FILESUBTYPE 0\n\
         FILEFLAGSMASK 0x3F\n\
         FILEFLAGS 0\n\
         {{\n\
         BLOCK \"StringFileInfo\"\n\
         {{\n\
         BLOCK \"{lang}\"\n\
         {{\n\
         VALUE \"FileDescription\", \"{description}\"\n\
         VALUE \"FileVersion\", \"{version}\"\n\
         VALUE \"OriginalFilename\", \"{original}\"\n\
         VALUE \"ProductName\", \"{product}\"\n\
         VALUE \"ProductVersion\", \"{version}\"\n\
         }}\n\
         }}\n\
         BLOCK \"VarFileInfo\"\n\
         {{\n\
         VALUE \"Translation\", 0x409, 0x04b0\n\
         }}\n\
         }}\n",
        description = rc_escape(description),
        original = rc_escape(original_filename),
        product = rc_escape(VERSION_PRODUCT_NAME),
        version = rc_escape(&version),
    );
    let rc_path = out_dir.join("worker_version.rc");
    let res_path = out_dir.join("worker_version.res");
    fs::write(&rc_path, text).expect("写版本资源脚本失败");
    let output = Command::new(&rc_exe)
        .arg("/nologo")
        .arg(format!("/fo{}", res_path.display()))
        .arg(&rc_path)
        .output()
        .unwrap_or_else(|e| panic!("无法运行 {}: {e}", rc_exe.display()));
    if !output.status.success() {
        panic!(
            "rc.exe 编译版本资源失败：{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }
    assert!(Path::new(&res_path).is_file(), "rc.exe 没有产出 .res");
    println!("cargo:rustc-link-arg-bins={}", res_path.display());
}
