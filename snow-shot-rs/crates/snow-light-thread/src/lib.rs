//! 轻量后台线程。
//!
//! Rust 标准库在 Windows 上给每条线程调一次 `SetThreadStackGuarantee`，为“栈溢出时还能打印报错”
//! 预留 20KB 已提交的栈。托盘宿主常驻 4 条后台线程（托盘、热键、单实例控制管道、鼠标钩子），
//! 本机实测（`docs/design/tray-host-split.md` §8 阶段 6）直接 `CreateThread` 每条线程少约 14KB 私有提交。
//!
//! 取舍：线程栈溢出时没有标准库那条友好报错，改由进程级崩溃处理（`snow-platform::crash`）落转储。
//! 其余行为与 `std::thread` 对齐：闭包 panic 不会带走进程（被捕获并丢弃），`join` 等待线程结束，
//! 丢弃句柄不等于结束线程（线程分离，句柄关闭）。
//!
//! 非 Windows 平台退回 `std::thread`，接口不变。

/// 轻量线程句柄；丢弃时分离线程，[`LightThread::join`] 等它结束。
pub struct LightThread {
    /// 平台线程句柄。
    inner: imp::Handle,
}

impl LightThread {
    /// 启动一条后台线程。
    ///
    /// # 参数
    /// - `name`：线程名（调试器 / 转储里可见；Windows 上用 `SetThreadDescription`）。
    /// - `body`：线程主体；panic 会被捕获，不影响进程。
    ///
    /// # 返回
    /// 线程句柄；创建失败返回系统错误。
    ///
    /// ```rust
    /// use snow_light_thread::LightThread;
    /// let (tx, rx) = std::sync::mpsc::channel();
    /// let t = LightThread::spawn("demo", move || tx.send(7).unwrap()).unwrap();
    /// assert_eq!(rx.recv().unwrap(), 7);
    /// t.join();
    /// ```
    pub fn spawn(name: &str, body: impl FnOnce() + Send + 'static) -> std::io::Result<LightThread> {
        Ok(LightThread {
            inner: imp::spawn(name, Box::new(body))?,
        })
    }

    /// 等线程结束（线程主体 panic 过也照常返回）。
    ///
    /// ```rust
    /// let t = snow_light_thread::LightThread::spawn("demo", || {}).unwrap();
    /// t.join();
    /// ```
    pub fn join(self) {
        imp::join(self.inner);
    }
}

#[cfg(windows)]
mod imp {
    use std::ffi::c_void;
    use std::panic::{AssertUnwindSafe, catch_unwind};
    use windows_sys::Win32::Foundation::{CloseHandle, HANDLE};
    use windows_sys::Win32::System::Threading::{
        CreateThread, GetCurrentThread, INFINITE, SetThreadDescription, WaitForSingleObject,
    };

    /// 线程句柄（关闭时自动释放）。
    pub struct Handle(HANDLE);

    // 线程句柄是内核对象句柄，可在线程间转移
    unsafe impl Send for Handle {}

    impl Drop for Handle {
        /// 关闭句柄（线程自己继续跑，相当于分离）。
        fn drop(&mut self) {
            // SAFETY: 句柄由本对象独占持有，只关闭一次。
            unsafe {
                CloseHandle(self.0);
            }
        }
    }

    /// 传给线程入口的负载：线程名与主体。
    struct Start {
        /// 线程名。
        name: Vec<u16>,
        /// 线程主体。
        body: Box<dyn FnOnce() + Send>,
    }

    /// 线程入口：设名字，运行主体，吞掉 panic。
    unsafe extern "system" fn entry(param: *mut c_void) -> u32 {
        // SAFETY: `param` 来自 `spawn` 里 `Box::into_raw`，每条线程只收回一次。
        let start = unsafe { Box::from_raw(param.cast::<Start>()) };
        let Start { name, body } = *start;
        // SAFETY: 伪句柄 + 以 0 结尾的宽字符串；失败只影响调试时的线程名。
        unsafe {
            SetThreadDescription(GetCurrentThread(), name.as_ptr());
        }
        let _ = catch_unwind(AssertUnwindSafe(body));
        0
    }

    /// 创建线程。
    pub fn spawn(name: &str, body: Box<dyn FnOnce() + Send>) -> std::io::Result<Handle> {
        let start = Box::new(Start {
            name: name.encode_utf16().chain(Some(0)).collect(),
            body,
        });
        let param = Box::into_raw(start);
        // SAFETY: 入口函数签名正确；`param` 的所有权交给新线程，失败时在下面收回。
        let handle = unsafe {
            CreateThread(
                std::ptr::null(),
                0,
                Some(entry),
                param.cast::<c_void>(),
                0,
                std::ptr::null_mut(),
            )
        };
        if handle.is_null() {
            let error = std::io::Error::last_os_error();
            // SAFETY: 线程没创建出来，负载仍归本函数所有。
            drop(unsafe { Box::from_raw(param) });
            return Err(error);
        }
        Ok(Handle(handle))
    }

    /// 等线程结束。
    pub fn join(handle: Handle) {
        // SAFETY: 句柄在 `handle` 存活期间有效。
        unsafe {
            WaitForSingleObject(handle.0, INFINITE);
        }
    }
}

#[cfg(not(windows))]
mod imp {
    /// 线程句柄（标准库线程）。
    pub struct Handle(std::thread::JoinHandle<()>);

    /// 创建线程。
    pub fn spawn(name: &str, body: Box<dyn FnOnce() + Send>) -> std::io::Result<Handle> {
        std::thread::Builder::new()
            .name(name.to_string())
            .spawn(body)
            .map(Handle)
    }

    /// 等线程结束。
    pub fn join(handle: Handle) {
        let _ = handle.0.join();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};

    /// 主体在另一条线程运行，`join` 返回时已执行完。
    #[test]
    fn runs_body_on_another_thread_and_join_waits() {
        let done = Arc::new(AtomicBool::new(false));
        let flag = done.clone();
        let main_id = std::thread::current().id();
        let (tx, rx) = std::sync::mpsc::channel();
        let t = LightThread::spawn("light-test", move || {
            tx.send(std::thread::current().id()).unwrap();
            std::thread::sleep(std::time::Duration::from_millis(50));
            flag.store(true, Ordering::SeqCst);
        })
        .unwrap();
        assert_ne!(rx.recv().unwrap(), main_id);
        t.join();
        assert!(done.load(Ordering::SeqCst));
    }

    /// 主体 panic 不会带走进程，`join` 照常返回。
    #[test]
    fn panic_in_body_is_contained() {
        let t = LightThread::spawn("light-panic", || panic!("测试预期的 panic")).unwrap();
        t.join();
    }

    /// 丢弃句柄只分离线程，线程照常跑完。
    #[test]
    fn dropping_handle_detaches() {
        let (tx, rx) = std::sync::mpsc::channel();
        drop(LightThread::spawn("light-detach", move || tx.send(1u8).unwrap()).unwrap());
        assert_eq!(
            rx.recv_timeout(std::time::Duration::from_secs(5)).unwrap(),
            1
        );
    }
}
