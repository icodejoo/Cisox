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

/// 读取当前用户注册表里的 `REG_DWORD` 值（直接调系统 API，不起 `reg.exe` 子进程）。
///
/// # 参数
/// - `subkey`：`HKEY_CURRENT_USER` 下的子键路径，如 `Control Panel\International`。
/// - `value`：值名。
///
/// # 返回
/// 值存在且类型为 `REG_DWORD` 时返回数值；不存在、类型不符、读取失败或平台不支持返回 `None`。
///
/// ```no_run
/// use snow_platform::system_integration::read_user_dword;
/// let light = read_user_dword(r"Software\Microsoft\Windows\CurrentVersion\Themes\Personalize", "AppsUseLightTheme");
/// ```
pub fn read_user_dword(subkey: &str, value: &str) -> Option<u32> {
    imp::read_user_dword(subkey, value)
}

/// 读取当前用户注册表里的 `REG_SZ` 字符串值（直接调系统 API，不起 `reg.exe` 子进程）。
///
/// # 参数
/// - `subkey`：`HKEY_CURRENT_USER` 下的子键路径。
/// - `value`：值名。
///
/// # 返回
/// 值存在且类型为 `REG_SZ` 时返回去掉结尾 NUL 的字符串；不存在、类型不符、读取失败或平台不支持返回 `None`。
///
/// ```no_run
/// use snow_platform::system_integration::read_user_string;
/// let locale = read_user_string(r"Control Panel\International", "LocaleName");
/// ```
pub fn read_user_string(subkey: &str, value: &str) -> Option<String> {
    imp::read_user_string(subkey, value)
}

#[cfg(windows)]
mod imp {
    use super::{PriorityLevel, run_command};
    use std::path::Path;
    use windows::Win32::Foundation::ERROR_FILE_NOT_FOUND;
    use windows::Win32::System::Registry::{
        HKEY, HKEY_CURRENT_USER, KEY_READ, KEY_SET_VALUE, REG_ROUTINE_FLAGS, REG_SZ,
        RRF_RT_REG_DWORD, RRF_RT_REG_SZ, RegCloseKey, RegDeleteValueW, RegGetValueW, RegOpenKeyExW,
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
        unsafe { SetPriorityClass(GetCurrentProcess(), class) }
            .map_err(|e| format!("SetPriorityClass: {e}"))
    }

    /// 已打开的 `Run` 子键，Drop 时关闭。
    struct RunKey(HKEY);

    impl RunKey {
        /// 打开 `Run` 子键；`write` 决定访问权限。
        fn open(write: bool) -> Result<Self, String> {
            let mut key = HKEY::default();
            let access = if write {
                KEY_SET_VALUE | KEY_READ
            } else {
                KEY_READ
            };
            let path = HSTRING::from(RUN_KEY);
            // SAFETY: path 是以 0 结尾的宽字符串，key 是有效输出位置。
            let status = unsafe {
                RegOpenKeyExW(
                    HKEY_CURRENT_USER,
                    PCWSTR(path.as_ptr()),
                    None,
                    access,
                    &mut key,
                )
            };
            if status.is_err() {
                return Err(format!("打开 Run 注册表项失败: {status:?}"));
            }
            Ok(Self(key))
        }
    }

    impl Drop for RunKey {
        /// 关闭子键。
        fn drop(&mut self) {
            // SAFETY: 子键由 `RegOpenKeyExW` 成功打开，只关闭一次。
            let _ = unsafe { RegCloseKey(self.0) };
        }
    }

    /// 写入或删除自启值。
    pub fn set_auto_start(name: &str, exe: &Path, enabled: bool) -> Result<(), String> {
        let key = RunKey::open(true)?;
        let value_name = HSTRING::from(name);
        if enabled {
            let command: Vec<u16> = crate::file_dialog::to_wide(&run_command(exe));
            // SAFETY: command 是 UTF-16 缓冲，字节长度按其长度计算；key 已打开且有写权限。
            let status = unsafe {
                RegSetValueExW(
                    key.0,
                    PCWSTR(value_name.as_ptr()),
                    None,
                    REG_SZ,
                    Some(std::slice::from_raw_parts(
                        command.as_ptr().cast::<u8>(),
                        command.len() * 2,
                    )),
                )
            };
            if status.is_err() {
                Err(format!("写入自启项失败: {status:?}"))
            } else {
                Ok(())
            }
        } else {
            // SAFETY: key 已打开且有写权限。
            let status = unsafe { RegDeleteValueW(key.0, PCWSTR(value_name.as_ptr())) };
            if status.is_err() && status != ERROR_FILE_NOT_FOUND {
                Err(format!("删除自启项失败: {status:?}"))
            } else {
                Ok(())
            }
        }
    }

    /// 自启值是否存在。
    pub fn is_auto_start_enabled(name: &str) -> bool {
        let Ok(key) = RunKey::open(false) else {
            return false;
        };
        let value_name = HSTRING::from(name);
        // SAFETY: key 已打开；只查询是否存在，不取数据。
        let status =
            unsafe { RegQueryValueExW(key.0, PCWSTR(value_name.as_ptr()), None, None, None, None) };
        status.is_ok()
    }

    /// `RegGetValueW` 的薄封装：先问长度再读数据，返回原始字节；失败返回 `None`。
    fn get_value(subkey: &str, value: &str, flags: REG_ROUTINE_FLAGS) -> Option<Vec<u8>> {
        let subkey = HSTRING::from(subkey);
        let value = HSTRING::from(value);
        let mut size = 0u32;
        // SAFETY: 两个名字是以 0 结尾的宽字符串；只问长度时数据指针为空，size 是有效输出位置。
        let status = unsafe {
            RegGetValueW(
                HKEY_CURRENT_USER,
                PCWSTR(subkey.as_ptr()),
                PCWSTR(value.as_ptr()),
                flags,
                None,
                None,
                Some(&mut size),
            )
        };
        if status.is_err() {
            return None;
        }
        let mut data = vec![0u8; size as usize];
        // SAFETY: data 有 size 字节可写；size 在调用里会被更新为实际写入的字节数。
        let status = unsafe {
            RegGetValueW(
                HKEY_CURRENT_USER,
                PCWSTR(subkey.as_ptr()),
                PCWSTR(value.as_ptr()),
                flags,
                None,
                Some(data.as_mut_ptr().cast()),
                Some(&mut size),
            )
        };
        if status.is_err() {
            return None;
        }
        data.truncate(size as usize);
        Some(data)
    }

    /// 读取 `REG_DWORD`。
    pub fn read_user_dword(subkey: &str, value: &str) -> Option<u32> {
        let data = get_value(subkey, value, RRF_RT_REG_DWORD)?;
        Some(u32::from_ne_bytes(data.get(..4)?.try_into().ok()?))
    }

    /// 读取 `REG_SZ`（`RegGetValueW` 保证结尾有 NUL，这里去掉）。
    pub fn read_user_string(subkey: &str, value: &str) -> Option<String> {
        let data = get_value(subkey, value, RRF_RT_REG_SZ)?;
        let wide: Vec<u16> = data
            .chunks_exact(2)
            .map(|pair| u16::from_ne_bytes([pair[0], pair[1]]))
            .take_while(|&unit| unit != 0)
            .collect();
        String::from_utf16(&wide).ok()
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

    /// 非 Windows 平台没有注册表。
    pub fn read_user_dword(_subkey: &str, _value: &str) -> Option<u32> {
        None
    }

    /// 非 Windows 平台没有注册表。
    pub fn read_user_string(_subkey: &str, _value: &str) -> Option<String> {
        None
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
        assert_eq!(
            PriorityLevel::parse("real_time"),
            Some(PriorityLevel::RealTime)
        );
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

    /// `REG_SZ` 读取：写入临时自启值后能读回完整命令行（含空格与引号），删除后读不到。
    #[cfg(windows)]
    #[test]
    fn read_user_string_roundtrip() {
        let name = format!("SnowShotReadTest-{}", std::process::id());
        let exe = std::path::Path::new(r"C:\Program Files\Cisox Test\cisox.exe");
        let run_key = r"Software\Microsoft\Windows\CurrentVersion\Run";
        assert_eq!(read_user_string(run_key, &name), None);
        set_auto_start(&name, exe, true).unwrap();
        assert_eq!(read_user_string(run_key, &name), Some(run_command(exe)));
        // 类型不符（REG_SZ 当 DWORD 读）返回 None，不会把字符串当数值
        assert_eq!(read_user_dword(run_key, &name), None);
        set_auto_start(&name, exe, false).unwrap();
        assert_eq!(read_user_string(run_key, &name), None);
    }

    /// 子键或值不存在时两个读取函数都返回 `None`，不 panic。
    #[test]
    fn read_user_missing_is_none() {
        let key = r"Software\SnowShotNoSuchKey-5f1c";
        assert_eq!(read_user_dword(key, "Nope"), None);
        assert_eq!(read_user_string(key, "Nope"), None);
    }

    /// 系统自带的界面语言值（`LocaleName`）在任何 Windows 用户下都存在且非空。
    #[cfg(windows)]
    #[test]
    fn read_user_string_reads_system_locale() {
        let locale = read_user_string(r"Control Panel\International", "LocaleName");
        assert!(locale.is_some_and(|l| !l.is_empty()));
    }
}
