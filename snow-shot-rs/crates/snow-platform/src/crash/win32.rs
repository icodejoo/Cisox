//! Windows 实现：`MiniDumpWriteDump` 与顶层异常过滤器。

use std::fs::File;
use std::mem::forget;
use std::os::windows::io::AsRawHandle;
use std::path::Path;

use windows::Win32::Foundation::HANDLE;
use windows::Win32::System::Diagnostics::Debug::{
    EXCEPTION_POINTERS, MINIDUMP_EXCEPTION_INFORMATION, MINIDUMP_TYPE, MiniDumpWithThreadInfo,
    MiniDumpWithUnloadedModules, MiniDumpWriteDump, SetUnhandledExceptionFilter,
};
use windows::Win32::System::Threading::{
    GetCurrentProcess, GetCurrentProcessId, GetCurrentThreadId,
};

use super::{CONFIG, CrashInfo, CrashKind, HANDLING, UNNAMED_THREAD, now_secs, write_report};
use std::backtrace::Backtrace;

/// 过滤器返回值：处理完毕，直接终止进程（不弹系统崩溃对话框）。
const EXCEPTION_EXECUTE_HANDLER: i32 = 1;

/// 把当前进程写成 minidump。
///
/// # 参数
/// - `path`：输出文件
/// - `exception`：异常现场；panic 场景传 `None`
///
/// # 返回
/// 是否写入成功；失败时会删除残留的空文件。
pub(super) fn write_minidump(path: &Path, exception: Option<*const EXCEPTION_POINTERS>) -> bool {
    let Ok(file) = File::create(path) else {
        return false;
    };
    let info = exception.map(|pointers| MINIDUMP_EXCEPTION_INFORMATION {
        // SAFETY: 仅读取当前线程 ID，无前置条件。
        ThreadId: unsafe { GetCurrentThreadId() },
        ExceptionPointers: pointers as *mut EXCEPTION_POINTERS,
        ClientPointers: false.into(),
    });
    let dump_type = MINIDUMP_TYPE(MiniDumpWithThreadInfo.0 | MiniDumpWithUnloadedModules.0);
    let handle = HANDLE(file.as_raw_handle());
    // SAFETY: `handle` 在调用期间由 `file` 持有；`info` 在调用期间存活且布局符合 API 约定。
    let result = unsafe {
        MiniDumpWriteDump(
            GetCurrentProcess(),
            GetCurrentProcessId(),
            handle,
            dump_type,
            info.as_ref().map(std::ptr::from_ref),
            None,
            None,
        )
    };
    drop(file);
    if result.is_err() {
        let _ = std::fs::remove_file(path);
    }
    result.is_ok()
}

/// 顶层异常过滤器：写报告后终止进程；处理期间再次崩溃则直接终止，避免死循环。
///
/// # Safety
/// 仅由系统在未处理异常时调用，`info` 由系统保证有效。
unsafe extern "system" fn top_level_filter(info: *const EXCEPTION_POINTERS) -> i32 {
    if let (Some(guard), Some(config)) = (HANDLING.try_enter(), CONFIG.get()) {
        // 进程即将终止，不再释放标志，防止其他线程随后重入
        forget(guard);
        // SAFETY: 系统保证 `info` 及其 ExceptionRecord 在过滤器执行期间有效。
        let (code, address) = unsafe {
            info.as_ref()
                .and_then(|p| p.ExceptionRecord.as_ref())
                .map_or((0, 0), |r| {
                    (r.ExceptionCode.0 as u32, r.ExceptionAddress as u64)
                })
        };
        let crash = CrashInfo {
            kind: CrashKind::Exception { code, address },
            thread_name: std::thread::current()
                .name()
                .unwrap_or(UNNAMED_THREAD)
                .to_string(),
            backtrace: Backtrace::force_capture().to_string(),
        };
        let _ = write_report(
            config,
            &crash,
            now_secs(),
            Some(&|path| write_minidump(path, Some(info))),
        );
    }
    EXCEPTION_EXECUTE_HANDLER
}

/// 安装顶层未处理异常过滤器。
pub(super) fn install_exception_filter() {
    // SAFETY: 过滤器函数为 `'static`，签名与 API 约定一致。
    unsafe {
        SetUnhandledExceptionFilter(Some(top_level_filter));
    }
}
