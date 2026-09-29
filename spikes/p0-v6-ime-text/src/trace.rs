//! Win32 消息层插桩：日志落盘 + 子类化窗口过程，记录原始 IME 消息。
//! 不引入新依赖，Win32 函数用手写 FFI 声明（仅 x64 Windows）。

use std::ffi::c_void;
use std::fs::File;
use std::io::Write;
use std::sync::atomic::{AtomicBool, AtomicIsize, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

/// 日志文件句柄（全局，进程内唯一）
static LOG_FILE: Mutex<Option<File>> = Mutex::new(None);
/// 进程启动基准时间，用于日志相对时间戳
static START: OnceLock<Instant> = OnceLock::new();
/// 被子类化窗口的原窗口过程
static PREV_PROC: AtomicIsize = AtomicIsize::new(0);
/// 目标窗口句柄
static HOOK_HWND: AtomicIsize = AtomicIsize::new(0);
/// 是否已安装钩子
static HOOKED: AtomicBool = AtomicBool::new(false);

const GWLP_WNDPROC: i32 = -4;
const WM_SETFOCUS: u32 = 0x0007;
const WM_KILLFOCUS: u32 = 0x0008;
const WM_KEYDOWN: u32 = 0x0100;
const WM_KEYUP: u32 = 0x0101;
const WM_CHAR: u32 = 0x0102;
const WM_INPUTLANGCHANGE: u32 = 0x0051;
const WM_IME_STARTCOMPOSITION: u32 = 0x010D;
const WM_IME_ENDCOMPOSITION: u32 = 0x010E;
const WM_IME_COMPOSITION: u32 = 0x010F;
const WM_IME_SETCONTEXT: u32 = 0x0281;
const WM_IME_NOTIFY: u32 = 0x0282;
const WM_IME_CHAR: u32 = 0x0286;
/// 输入法处理中的按键伪虚拟键码
const VK_PROCESSKEY: usize = 0xE5;
const GCS_COMPSTR: u32 = 0x0008;
const GCS_RESULTSTR: u32 = 0x0800;
/// GCS_* 标志位名称表，用于解码 lParam
const GCS_FLAGS: &[(u32, &str)] = &[
    (0x0001, "COMPREADSTR"),
    (0x0002, "COMPREADATTR"),
    (0x0004, "COMPREADCLAUSE"),
    (0x0008, "COMPSTR"),
    (0x0010, "COMPATTR"),
    (0x0020, "COMPCLAUSE"),
    (0x0080, "CURSORPOS"),
    (0x0100, "DELTASTART"),
    (0x0200, "RESULTREADSTR"),
    (0x0400, "RESULTREADCLAUSE"),
    (0x0800, "RESULTSTR"),
    (0x1000, "RESULTCLAUSE"),
];
/// gpui 在 Windows 上注册的窗口类名
const GPUI_WINDOW_CLASS: &str = "Zed::Window";

#[link(name = "user32")]
unsafe extern "system" {
    fn EnumThreadWindows(
        tid: u32,
        cb: unsafe extern "system" fn(isize, isize) -> i32,
        lparam: isize,
    ) -> i32;
    fn GetClassNameW(h: isize, buf: *mut u16, n: i32) -> i32;
    fn SetWindowLongPtrW(h: isize, idx: i32, v: isize) -> isize;
    fn CallWindowProcW(prev: isize, h: isize, msg: u32, w: usize, l: isize) -> isize;
    fn GetFocus() -> isize;
    fn GetKeyboardLayout(tid: u32) -> isize;
}
#[link(name = "kernel32")]
unsafe extern "system" {
    fn GetCurrentThreadId() -> u32;
}
#[link(name = "imm32")]
unsafe extern "system" {
    fn ImmGetContext(h: isize) -> isize;
    fn ImmReleaseContext(h: isize, himc: isize) -> i32;
    fn ImmGetCompositionStringW(himc: isize, idx: u32, buf: *mut c_void, len: u32) -> i32;
    fn ImmGetOpenStatus(himc: isize) -> i32;
}

/// 初始化日志文件（覆盖写）。
/// - `path`: 日志文件路径，父目录会自动创建
pub fn init_log(path: &str) {
    START.get_or_init(Instant::now);
    if let Some(dir) = std::path::Path::new(path).parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    if let Ok(f) = File::create(path) {
        *LOG_FILE.lock().unwrap() = Some(f);
    }
}

/// 写一行带时间戳和线程号的日志，并立即落盘。
/// - `msg`: 日志正文
pub fn log(msg: impl AsRef<str>) {
    let ms = START.get().map(|s| s.elapsed().as_millis()).unwrap_or(0);
    let tid = unsafe { GetCurrentThreadId() };
    if let Some(f) = LOG_FILE.lock().unwrap().as_mut() {
        let _ = writeln!(f, "[{ms:>7}ms T{tid}] {}", msg.as_ref());
        let _ = f.flush();
    }
}

/// 把 GCS_* 标志位解码成可读字符串。
fn decode_gcs(l: u32) -> String {
    let names: Vec<&str> = GCS_FLAGS
        .iter()
        .filter(|(bit, _)| l & bit != 0)
        .map(|(_, n)| *n)
        .collect();
    format!("0x{l:X}=[{}]", names.join("|"))
}

/// 读取 IME 上下文里的合成串，返回 (API 返回的字节数, 文本)。
fn read_comp_string(himc: isize, kind: u32) -> (i32, String) {
    unsafe {
        let bytes = ImmGetCompositionStringW(himc, kind, std::ptr::null_mut(), 0);
        if bytes <= 0 {
            return (bytes, String::new());
        }
        let mut buf = vec![0u16; bytes as usize / 2];
        ImmGetCompositionStringW(himc, kind, buf.as_mut_ptr() as *mut c_void, bytes as u32);
        (bytes, String::from_utf16_lossy(&buf))
    }
}

/// 给出窗口当前 IME 上下文摘要（是否关联、是否打开、是否持有焦点）。
fn ime_status(h: isize) -> String {
    unsafe {
        let himc = ImmGetContext(h);
        let focus = GetFocus();
        let hkl = GetKeyboardLayout(0);
        if himc == 0 {
            return format!(
                "himc=NULL(未关联IME上下文) focus_is_hwnd={} hkl=0x{hkl:X}",
                focus == h
            );
        }
        let open = ImmGetOpenStatus(himc);
        ImmReleaseContext(h, himc);
        format!(
            "himc=0x{himc:X} open={open} focus_is_hwnd={} hkl=0x{hkl:X}",
            focus == h
        )
    }
}

/// 子类化后的窗口过程：先记录 IME 相关消息，再转交给 gpui 原过程。
unsafe extern "system" fn hook_proc(h: isize, msg: u32, w: usize, l: isize) -> isize {
    let is_key = matches!(msg, WM_KEYDOWN | WM_KEYUP) && w == VK_PROCESSKEY;
    let interesting = is_key
        || matches!(
            msg,
            WM_IME_STARTCOMPOSITION
                | WM_IME_ENDCOMPOSITION
                | WM_IME_COMPOSITION
                | WM_IME_SETCONTEXT
                | WM_IME_NOTIFY
                | WM_IME_CHAR
                | WM_INPUTLANGCHANGE
                | WM_SETFOCUS
                | WM_KILLFOCUS
                | WM_CHAR
        );
    if interesting {
        match msg {
            WM_IME_COMPOSITION => {
                let himc = unsafe { ImmGetContext(h) };
                let (cb, comp) = if himc != 0 {
                    read_comp_string(himc, GCS_COMPSTR)
                } else {
                    (-1, String::new())
                };
                let (rb, res) = if himc != 0 {
                    read_comp_string(himc, GCS_RESULTSTR)
                } else {
                    (-1, String::new())
                };
                if himc != 0 {
                    unsafe { ImmReleaseContext(h, himc) };
                }
                log(format!(
                    "WM_IME_COMPOSITION 前 lParam={} wParam=0x{w:X} himc_null={} COMPSTR({cb}B)={comp:?} RESULTSTR({rb}B)={res:?}",
                    decode_gcs(l as u32),
                    himc == 0
                ));
            }
            WM_IME_STARTCOMPOSITION => {
                log(format!("WM_IME_STARTCOMPOSITION 前 {}", ime_status(h)))
            }
            WM_IME_ENDCOMPOSITION => log("WM_IME_ENDCOMPOSITION 前"),
            WM_IME_SETCONTEXT => log(format!(
                "WM_IME_SETCONTEXT active={} lParam=0x{:X}",
                w != 0,
                l as u32
            )),
            WM_IME_NOTIFY => log(format!("WM_IME_NOTIFY wParam=0x{w:X} lParam=0x{:X}", l as u32)),
            WM_IME_CHAR => log(format!("WM_IME_CHAR char={:?}", char::from_u32(w as u32))),
            WM_CHAR => log(format!(
                "WM_CHAR char={:?} (0x{w:X})",
                char::from_u32(w as u32)
            )),
            WM_KEYDOWN | WM_KEYUP => log(format!(
                "WM_KEY{} VK_PROCESSKEY(0xE5) lParam=0x{:X}",
                if msg == WM_KEYDOWN { "DOWN" } else { "UP" },
                l as u32
            )),
            WM_INPUTLANGCHANGE => log(format!("WM_INPUTLANGCHANGE hkl=0x{l:X}")),
            WM_SETFOCUS => log(format!("WM_SETFOCUS {}", ime_status(h))),
            WM_KILLFOCUS => log("WM_KILLFOCUS"),
            _ => {}
        }
    }
    let ret = unsafe { CallWindowProcW(PREV_PROC.load(Ordering::SeqCst), h, msg, w, l) };
    if matches!(
        msg,
        WM_IME_STARTCOMPOSITION | WM_IME_ENDCOMPOSITION | WM_IME_COMPOSITION
    ) {
        log(format!("  └ 原过程(gpui)返回 {ret}"));
    }
    ret
}

/// 枚举回调：收集本线程里类名为 gpui 窗口类的第一个窗口。
unsafe extern "system" fn enum_cb(h: isize, _l: isize) -> i32 {
    let mut buf = [0u16; 64];
    let n = unsafe { GetClassNameW(h, buf.as_mut_ptr(), 64) };
    let name = String::from_utf16_lossy(&buf[..n.max(0) as usize]);
    if name == GPUI_WINDOW_CLASS && HOOK_HWND.load(Ordering::SeqCst) == 0 {
        HOOK_HWND.store(h, Ordering::SeqCst);
    }
    1
}

/// 在主线程尝试安装窗口过程钩子，成功（或已装）返回 true。
pub fn install_hook() -> bool {
    if HOOKED.load(Ordering::SeqCst) {
        return true;
    }
    unsafe { EnumThreadWindows(GetCurrentThreadId(), enum_cb, 0) };
    let h = HOOK_HWND.load(Ordering::SeqCst);
    if h == 0 {
        return false;
    }
    let prev = unsafe { SetWindowLongPtrW(h, GWLP_WNDPROC, hook_proc as *const () as isize) };
    PREV_PROC.store(prev, Ordering::SeqCst);
    HOOKED.store(prev != 0, Ordering::SeqCst);
    log(format!(
        "钩子安装 hwnd=0x{h:X} prev_proc=0x{prev:X} ok={} | {}",
        prev != 0,
        ime_status(h)
    ));
    prev != 0
}

/// 启动后台巡检任务：反复尝试装钩子，装好后 IME 上下文状态变化时记日志。
/// - `smoke`: 为 true 时装好钩子并稳定 2 秒后写自检结果并退出进程
pub fn spawn_watcher(cx: &mut gpui_kit::App, smoke: bool) {
    cx.spawn(async move |cx| {
        let mut last = String::new();
        let mut ticks_after_hook = 0u32;
        loop {
            cx.background_executor()
                .timer(Duration::from_millis(100))
                .await;
            if !install_hook() {
                continue;
            }
            let s = ime_status(HOOK_HWND.load(Ordering::SeqCst));
            if s != last {
                log(format!("IME上下文状态变化: {s}"));
                last = s;
            }
            ticks_after_hook += 1;
            if smoke && ticks_after_hook > 20 {
                log(format!(
                    "SMOKE 自检通过: hooked={} 最终状态 {last}",
                    HOOKED.load(Ordering::SeqCst)
                ));
                cx.update(|cx| cx.quit());
                break;
            }
        }
    })
    .detach();
}
