//! 发布形态（GUI 子系统，无控制台窗口）下的控制台附着。
//!
//! release 构建使用 `windows_subsystem = "windows"`，进程没有控制台，`println!` / `eprintln!`
//! 会被系统静默丢弃。从终端调用 `--cmd` 子命令时，通过附着父进程控制台让输出仍然可见。

/// 附着到父进程的控制台，并在标准输出 / 错误没有有效句柄时重新绑定到该控制台。
///
/// 标准流已被重定向到文件或管道时保持原样，只补齐缺失的流。
///
/// # 返回
/// 成功附着（或已附着）返回 `true`；父进程没有控制台（例如从资源管理器启动）返回 `false`。
///
/// ```no_run
/// // 只有从终端启动的命令行子命令才需要看得到输出
/// let attached = snow_platform::console::attach_parent_console();
/// println!("attached = {attached}");
/// ```
pub fn attach_parent_console() -> bool {
    imp::attach()
}

#[cfg(windows)]
mod imp {
    use windows::Win32::Foundation::GENERIC_WRITE;
    use windows::Win32::Storage::FileSystem::{
        CreateFileW, FILE_ATTRIBUTE_NORMAL, FILE_SHARE_WRITE, OPEN_EXISTING,
    };
    use windows::Win32::System::Console::{
        ATTACH_PARENT_PROCESS, AttachConsole, GetStdHandle, STD_ERROR_HANDLE, STD_HANDLE,
        STD_OUTPUT_HANDLE, SetStdHandle,
    };
    use windows::core::w;

    /// 若指定标准流没有有效句柄，就绑定到控制台输出缓冲。
    fn rebind_if_missing(which: STD_HANDLE) {
        // SAFETY: 只读取 / 设置进程标准句柄；CONOUT$ 句柄交给系统随进程持有。
        unsafe {
            let missing = match GetStdHandle(which) {
                Ok(handle) => handle.is_invalid() || handle.0.is_null(),
                Err(_) => true,
            };
            if !missing {
                return;
            }
            match CreateFileW(
                w!("CONOUT$"),
                GENERIC_WRITE.0,
                FILE_SHARE_WRITE,
                None,
                OPEN_EXISTING,
                FILE_ATTRIBUTE_NORMAL,
                None,
            ) {
                Ok(console) => {
                    if let Err(e) = SetStdHandle(which, console) {
                        tracing::warn!(error = %e, "绑定标准输出到控制台失败");
                    }
                }
                Err(e) => tracing::warn!(error = %e, "打开 CONOUT$ 失败"),
            }
        }
    }

    /// 附着并补齐标准流。
    pub fn attach() -> bool {
        // SAFETY: 无指针参数；已附着 / 无父控制台时返回错误。
        if unsafe { AttachConsole(ATTACH_PARENT_PROCESS) }.is_err() {
            return false;
        }
        rebind_if_missing(STD_OUTPUT_HANDLE);
        rebind_if_missing(STD_ERROR_HANDLE);
        true
    }
}

#[cfg(not(windows))]
mod imp {
    /// 非 Windows 平台没有子系统区分，无需附着。
    pub fn attach() -> bool {
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 重复调用不会崩溃（测试进程可能有也可能没有父控制台，只要求不 panic）。
    #[test]
    fn attach_is_idempotent() {
        let first = attach_parent_console();
        let second = attach_parent_console();
        // 第一次附着成功后，第二次因“已附着”返回 false；无论哪种都不应 panic
        assert!(!(second && !first));
    }
}
