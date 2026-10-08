//! 描述符文件：桥接进程据此找到管道并取得令牌（`<数据根>/mcp/descriptor.json`）。

use crate::protocol::PROTOCOL_VERSION;
use crate::registry::Scope;
use serde_json::{Value, json};
use std::fs;
use std::path::{Path, PathBuf};

/// 描述符所在子目录名。
pub const DESCRIPTOR_DIR: &str = "mcp";
/// 描述符文件名。
pub const DESCRIPTOR_FILE: &str = "descriptor.json";
/// 写入时的临时文件名（写完再改名，避免读到半截）。
const DESCRIPTOR_TEMP_FILE: &str = "descriptor.json.tmp";

/// 描述符内容。
pub struct Descriptor<'a> {
    /// 管道完整名。
    pub pipe: &'a str,
    /// 访问令牌（只写文件，不进日志）。
    pub token: &'a str,
    /// 本次启用的代数标识（关闭时只删自己写的）。
    pub generation: &'a str,
    /// 已授权的权限域。
    pub scopes: &'a [Scope],
}

/// 描述符文件路径。
///
/// # 参数
/// - `data_root`：应用数据根目录。
pub fn descriptor_path(data_root: &Path) -> PathBuf {
    data_root.join(DESCRIPTOR_DIR).join(DESCRIPTOR_FILE)
}

/// 原子写入描述符。
///
/// # 参数
/// - `data_root`：应用数据根目录。
/// - `descriptor`：内容。
///
/// # 返回
/// 文件路径；失败返回原因（不含令牌）。
pub fn write(data_root: &Path, descriptor: &Descriptor<'_>) -> Result<PathBuf, String> {
    let path = descriptor_path(data_root);
    let dir = path.parent().map(Path::to_path_buf).unwrap_or_default();
    fs::create_dir_all(&dir).map_err(|e| format!("创建描述符目录失败: {e}"))?;
    let body = json!({
        "protocol": PROTOCOL_VERSION,
        "pipe": descriptor.pipe,
        "token": descriptor.token,
        "pid": std::process::id(),
        "generation": descriptor.generation,
        "scopes": descriptor.scopes.iter().map(|s| s.name()).collect::<Vec<_>>(),
    });
    let temp = dir.join(DESCRIPTOR_TEMP_FILE);
    fs::write(&temp, body.to_string()).map_err(|e| format!("写描述符失败: {e}"))?;
    fs::rename(&temp, &path).map_err(|e| {
        let _ = fs::remove_file(&temp);
        format!("替换描述符失败: {e}")
    })?;
    Ok(path)
}

/// 仅当代数一致时删除描述符（不误删后启动实例写的新文件）。
///
/// # 参数
/// - `path`：描述符路径。
/// - `generation`：本实例的代数标识。
pub fn remove_if_generation(path: &Path, generation: &str) {
    let owned = fs::read_to_string(path)
        .ok()
        .and_then(|text| serde_json::from_str::<Value>(&text).ok())
        .is_some_and(|v| v["generation"] == generation);
    if owned {
        let _ = fs::remove_file(path);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 写入含全部字段；代数不符不删，相符才删。
    #[test]
    fn write_then_remove_by_generation() {
        let root = std::env::temp_dir().join(format!("cisox-mcp-desc-{}", std::process::id()));
        let d = Descriptor {
            pipe: r"\\.\pipe\x",
            token: "tok",
            generation: "g1",
            scopes: Scope::DEFAULT_GRANTED,
        };
        let path = write(&root, &d).unwrap();
        let v: Value = serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(v["protocol"], PROTOCOL_VERSION);
        assert_eq!(v["token"], "tok");
        assert_eq!(v["scopes"], json!(["read_only", "capture"]));
        assert!(v["pid"].as_u64().unwrap() > 0);
        remove_if_generation(&path, "other");
        assert!(path.exists());
        remove_if_generation(&path, "g1");
        assert!(!path.exists());
        let _ = fs::remove_dir_all(&root);
    }
}
