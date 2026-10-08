//! 与系统集成的小功能：进程优先级、开机自启（当前用户的 `Run` 注册表项，不需要管理员权限）。
//!
//! 纯逻辑部分（优先级解析、`Run` 值的引号规则）可离屏测试；Windows 实现之外的平台返回「不支持」。

use std::path::Path;

/// 进程优先级。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PriorityLevel {
    /// 普通。
    Normal,
    /// 高于普通。
    AboveNormal,
    /// 高。
    High,
    /// 实时（需要管理员权限，否则系统会降级为高）。
    RealTime,
}

impl PriorityLevel {
    /// 由配置值解析（`normal` / `above_normal` / `high` / `real_time`）；未知值返回 `None`。
    ///
    /// # 参数
    /// - `text`：配置里的优先级名。
    ///
    /// ```
    /// use snow_platform::system_integration::PriorityLevel;
    /// assert_eq!(PriorityLevel::parse("above_normal"), Some(PriorityLevel::AboveNormal));
    /// assert_eq!(PriorityLevel::parse("idle"), None);
    /// ```
    pub fn parse(text: &str) -> Option<Self> {
        Some(match text {
            "normal" => Self::Normal,
            "above_normal" => Self::AboveNormal,
            "high" => Self::High,
            "real_time" => Self::RealTime,
            _ => return None,
        })
    }
}

/// 设置当前进程的优先级。
///
/// # 参数
/// - `level`：目标优先级。
///
/// # 返回
/// 成功 `Ok(())`；系统调用失败或平台不支持返回原因。
pub fn set_process_priority(level: PriorityLevel) -> Result<(), String> {
    imp::set_process_priority(level)
}

/// 生成开机自启的 `Run` 值：可执行文件路径加引号（路径含空格也安全）。
///
/// # 参数
/// - `exe`：可执行文件路径。
///
/// ```
/// use snow_platform::system_integration::run_command;
/// assert_eq!(run_command(std::path::Path::new(r"C:\Program Files\Cisox\cisox.exe")), r#""C:\Program Files\Cisox\cisox.exe""#);
/// ```
pub fn run_command(exe: &Path) -> String {
    format!("\"{}\"", exe.display())
}

/// 开启或关闭当前用户的开机自启。
///
/// # 参数
/// - `name`：注册表值名（通常是产品名）。
/// - `exe`：开启时写入的可执行文件路径。
/// - `enabled`：`true` 写入、`false` 删除（值本来就不存在也算成功）。
///
/// # 返回
/// 成功 `Ok(())`；注册表操作失败或平台不支持返回原因。
pub fn set_auto_start(name: &str, exe: &Path, enabled: bool) -> Result<(), String> {
    imp::set_auto_start(name, exe, enabled)
}

/// 当前用户的开机自启里是否已有该名字的值。
///
/// # 参数
/// - `name`：注册表值名。
pub fn is_auto_start_enabled(name: &str) -> bool {
    imp::is_auto_start_enabled(name)
}

#[cfg(windows)]
mod imp {
    use super::{PriorityLevel, run_command};
    use std::path::Path;
    use windows::Win32::Foundation::ERROR_FILE_NOT_FOUND;
    use windows::Win32::System::Registry::{
        HKEY, HKEY_CURRENT_USER, KEY_READ, KEY_SET_VALUE, REG_SZ, RegCloseKey, RegDeleteValueW, RegOpenKeyExW,
        RegQueryValueExW, RegSetValueExW,
    };
    use windows::Win32::System::Threading::{
        ABOVE_NORMAL_PRIORITY_CLASS, GetCurrentProcess, HIGH_PRIORITY_CLASS, NORMAL_PRIORITY_CLASS,
        PROCESS_CREATION_FLAGS, REALTIME_PRIORITY_CLASS, SetPriorityClass,
    };
    use windows::core::{HSTRING, PCWSTR};

    /// 开机自启所在的注册表子键。
    const RUN_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";

    /// 设置当前进程优先级。
    pub fn set_process_priority(level: PriorityLevel) -> Result<(), String> {
        let class: PROCESS_CREATION_FLAGS = match level {
            PriorityLevel::Normal => NORMAL_PRIORITY_CLASS,
            PriorityLevel::AboveNormal => ABOVE_NORMAL_PRIORITY_CLASS,
            PriorityLevel::High => HIGH_PRIORITY_CLASS,
            PriorityLevel::RealTime => REALTIME_PRIORITY_CLASS,
        };
        // SAFETY: 当前进程伪句柄恒有效；参数为纯值。
        unsafe { SetPriorityClass(GetCurrentProcess(), class) }.map_err(|e| format!("SetPriorityClass: {e}"))
    }

    /// 打开 `Run` 子键；`write` 决定访问权限。
    fn open_run_key(write: bool) -> Result<HKEY, String> {
        let mut key = HKEY::default();
        let access = if write { KEY_SET_VALUE | KEY_READ } else { KEY_READ };
        let path = HSTRING::from(RUN_KEY);
        // SAFETY: path 是以 0 结尾的宽字符串，key 是有效输出位置。
        let status = unsafe { RegOpenKeyExW(HKEY_CURRENT_USER, PCWSTR(path.as_ptr()), None, access, &mut key) };
        if status.is_err() {
            return Err(format!("打开 Run 注册表项失败: {status:?}"));
        }
        Ok(key)
    }

    /// 写入或删除自启值。
    pub fn set_auto_start(name: &str, exe: &Path, enabled: bool) -> Result<(), String> {
        let key = open_run_key(true)?;
        let value_name = HSTRING::from(name);
        let result = if enabled {
            let command: Vec<u16> = run_command(exe).encode_utf16().chain(std::iter::once(0)).collect();
            // SAFETY: command 是 UTF-16 缓冲，字节长度按其长度计算；key 已打开且有写权限。
            let status = unsafe {
                RegSetValueExW(
                    key,
                    PCWSTR(value_name.as_ptr()),
                    None,
                    REG_SZ,
                    Some(std::slice::from_raw_parts(command.as_ptr().cast::<u8>(), command.len() * 2)),
                )
            };
            if status.is_err() { Err(format!("写入自启项失败: {status:?}")) } else { Ok(()) }
        } else {
            // SAFETY: key 已打开且有写权限。
            let status = unsafe { RegDeleteValueW(key, PCWSTR(value_name.as_ptr())) };
            if status.is_err() && status != ERROR_FILE_NOT_FOUND {
                Err(format!("删除自启项失败: {status:?}"))
            } else {
                Ok(())
            }
        };
        // SAFETY: key 由上面成功打开。
        let _ = unsafe { RegCloseKey(key) };
        result
    }

    /// 自启值是否存在。
    pub fn is_auto_start_enabled(name: &str) -> bool {
        let Ok(key) = open_run_key(false) else {
            return false;
        };
        let value_name = HSTRING::from(name);
        // SAFETY: key 已打开；只查询是否存在，不取数据。
        let status = unsafe { RegQueryValueExW(key, PCWSTR(value_name.as_ptr()), None, None, None, None) };
        // SAFETY: key 由上面成功打开。
        let _ = unsafe { RegCloseKey(key) };
        status.is_ok()
    }
}

#[cfg(not(windows))]
mod imp {
    use super::PriorityLevel;
    use std::path::Path;

    /// 非 Windows 平台没有实现。
    pub fn set_process_priority(_level: PriorityLevel) -> Result<(), String> {
        Err("当前平台不支持设置进程优先级".into())
    }

    /// 非 Windows 平台没有实现。
    pub fn set_auto_start(_name: &str, _exe: &Path, _enabled: bool) -> Result<(), String> {
        Err("当前平台不支持开机自启".into())
    }

    /// 非 Windows 平台没有实现。
    pub fn is_auto_start_enabled(_name: &str) -> bool {
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 优先级解析覆盖四个取值，未知值无结果。
    #[test]
    fn priority_parsing() {
        assert_eq!(PriorityLevel::parse("normal"), Some(PriorityLevel::Normal));
        assert_eq!(PriorityLevel::parse("high"), Some(PriorityLevel::High));
        assert_eq!(PriorityLevel::parse("real_time"), Some(PriorityLevel::RealTime));
        assert_eq!(PriorityLevel::parse(""), None);
    }

    /// 设为普通优先级一定成功（不影响测试进程的其它行为）。
    #[cfg(windows)]
    #[test]
    fn setting_normal_priority_succeeds() {
        assert!(set_process_priority(PriorityLevel::Normal).is_ok());
    }

    /// 真实注册表往返：写入、读到、删除、再删除也成功；使用临时值名，不碰产品自己的自启项。
    #[cfg(windows)]
    #[test]
    fn auto_start_registry_roundtrip() {
        let name = format!("SnowShotTest-{}", std::process::id());
        let exe = std::path::Path::new(r"C:\Program Files\Cisox Test\cisox.exe");
        assert!(!is_auto_start_enabled(&name));
        set_auto_start(&name, exe, true).unwrap();
        assert!(is_auto_start_enabled(&name));
        set_auto_start(&name, exe, false).unwrap();
        assert!(!is_auto_start_enabled(&name));
        set_auto_start(&name, exe, false).unwrap();
    }
}
