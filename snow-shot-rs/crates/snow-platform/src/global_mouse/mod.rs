//! 全局鼠标手势（Windows 低级鼠标 / 键盘钩子）：按住修饰键（默认 Win）再按住鼠标键拖动，
//! 把「开始 / 更新 / 结束 / 取消」事件交给上层，由上层去做截图选区。
//!
//! 性能策略与旧版一致：键盘钩子常驻、只跟踪修饰键状态；**只有当前修饰键组合恰好匹配某条绑定、或手势进行中，
//! 才安装鼠标钩子**，平时系统里没有全局鼠标钩子。钩子回调只做轻量转发（`handler` 必须立即返回）。
//! 手势判定是纯逻辑，见 [`Gesture`]。其它平台 `start` 返回「不支持」。

mod gesture;

pub use gesture::{Binding, DragEvent, Gesture, Input, Modifiers, MouseKey, Outcome, Point};

/// 拖动事件处理函数；在钩子线程上同步调用，必须立即返回（只做转发）。
pub type DragHandler = Box<dyn Fn(DragEvent) + Send + Sync>;

/// 全局鼠标手势服务：丢弃即卸载全部钩子并结束线程。
pub struct GlobalMouseService {
    /// 平台实现句柄。
    inner: imp::Inner,
}

impl GlobalMouseService {
    /// 启动服务（创建钩子线程并安装键盘钩子）。
    ///
    /// # 参数
    /// - `handler`：拖动事件处理函数（在钩子线程上调用，勿阻塞）。
    ///
    /// # 返回
    /// 服务句柄；钩子安装失败或平台不支持返回原因。
    ///
    /// ```no_run
    /// use snow_platform::global_mouse::GlobalMouseService;
    /// let service = GlobalMouseService::start(Box::new(|event| println!("{event:?}"))).unwrap();
    /// service.set_bindings(Vec::new());
    /// ```
    pub fn start(handler: DragHandler) -> Result<Self, String> {
        Ok(Self { inner: imp::Inner::start(handler)? })
    }

    /// 替换全部绑定（配置变化时调用）；进行中的手势会被取消。
    ///
    /// # 参数
    /// - `bindings`：新的绑定列表。
    pub fn set_bindings(&self, bindings: Vec<Binding>) {
        self.inner.set_bindings(bindings);
    }

    /// 取消进行中的手势（上层放弃本次拖选时调用）。
    pub fn cancel(&self) {
        self.inner.cancel();
    }

    /// 是否接受 `SendInput` 注入的输入（默认不接受，避免被别的程序模拟的输入误触发）；仅供测试 / 自动化验证。
    ///
    /// # 参数
    /// - `accept`：`true` 接受注入输入。
    pub fn accept_injected_input(&self, accept: bool) {
        imp::set_accept_injected(accept);
    }
}

#[cfg(windows)]
mod imp {
    use super::{Binding, DragEvent, DragHandler, Gesture, Input, Modifiers, MouseKey, Point};
    use std::collections::BTreeSet;
    use std::sync::mpsc::channel;
    use std::sync::{Arc, Mutex, MutexGuard};
    use std::thread::JoinHandle;
    use windows::Win32::Foundation::{HINSTANCE, LPARAM, LRESULT, WPARAM};
    use windows::Win32::System::LibraryLoader::GetModuleHandleW;
    use windows::Win32::System::Threading::GetCurrentThreadId;
    use windows::Win32::UI::Input::KeyboardAndMouse::{
        GetAsyncKeyState, INPUT, INPUT_0, INPUT_KEYBOARD, KEYBDINPUT, KEYEVENTF_KEYUP, SendInput, VIRTUAL_KEY, VK_LBUTTON,
        VK_LCONTROL, VK_LMENU, VK_LSHIFT, VK_LWIN, VK_MBUTTON, VK_RBUTTON, VK_RCONTROL, VK_RMENU, VK_RSHIFT, VK_RWIN,
        VK_XBUTTON1, VK_XBUTTON2,
    };
    use windows::Win32::UI::WindowsAndMessaging::{
        CallNextHookEx, DispatchMessageW, GetMessageW, HHOOK, KBDLLHOOKSTRUCT, MSG, MSLLHOOKSTRUCT, PostThreadMessageW,
        SetWindowsHookExW, TranslateMessage, UnhookWindowsHookEx, WH_KEYBOARD_LL, WH_MOUSE_LL, WM_APP, WM_KEYDOWN,
        WM_KEYUP, WM_LBUTTONDOWN, WM_LBUTTONUP, WM_MBUTTONDOWN, WM_MBUTTONUP, WM_MOUSEMOVE, WM_QUIT, WM_RBUTTONDOWN,
        WM_RBUTTONUP, WM_SYSKEYDOWN, WM_SYSKEYUP, WM_XBUTTONDOWN, WM_XBUTTONUP,
    };

    /// 是否接受注入的输入（仅测试 / 自动化验证打开）。
    static ACCEPT_INJECTED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

    /// 设置是否接受注入的输入。
    pub fn set_accept_injected(accept: bool) {
        ACCEPT_INJECTED.store(accept, std::sync::atomic::Ordering::Relaxed);
    }

    /// 是否应当忽略这个带 `injected` 标志的输入。
    fn ignore(injected: bool) -> bool {
        injected && !ACCEPT_INJECTED.load(std::sync::atomic::Ordering::Relaxed)
    }

    /// 让钩子线程重新评估是否需要鼠标钩子的私有消息。
    const WM_REFRESH: u32 = WM_APP + 0x61;
    /// 让钩子线程取消进行中手势的私有消息。
    const WM_CANCEL: u32 = WM_APP + 0x62;
    /// 钩子回调里的低级键盘事件「来自 SendInput」标志。
    const LLKHF_INJECTED: u32 = 0x10;
    /// 钩子回调里的低级鼠标事件「来自 SendInput」标志。
    const LLMHF_INJECTED: u32 = 0x01;
    /// 抑制 Win 键松开时弹出开始菜单用的未分配虚拟键。
    const VK_NOOP: u16 = 0xE8;

    /// 钩子线程内共享的状态。
    struct State {
        /// 当前绑定。
        bindings: Vec<Binding>,
        /// 手势状态机。
        gesture: Gesture,
        /// 当前按住的修饰键对应的虚拟键。
        held_keys: BTreeSet<u16>,
        /// 事件处理函数。
        handler: Arc<dyn Fn(DragEvent) + Send + Sync>,
        /// 键盘钩子。
        keyboard_hook: Option<isize>,
        /// 鼠标钩子（只在需要时安装）。
        mouse_hook: Option<isize>,
    }

    /// 全局状态：钩子回调是无状态的函数指针，只能经此访问。
    static STATE: Mutex<Option<State>> = Mutex::new(None);

    /// 取状态锁（中毒时继续使用，钩子里不能 panic 也不能卡死）。
    fn lock() -> MutexGuard<'static, Option<State>> {
        STATE.lock().unwrap_or_else(|e| e.into_inner())
    }

    impl State {
        /// 由按住的虚拟键算出修饰键集合。
        fn modifiers(&self) -> Modifiers {
            let any = |keys: &[VIRTUAL_KEY]| keys.iter().any(|k| self.held_keys.contains(&k.0));
            Modifiers {
                win: any(&[VK_LWIN, VK_RWIN]),
                ctrl: any(&[VK_LCONTROL, VK_RCONTROL]),
                alt: any(&[VK_LMENU, VK_RMENU]),
                shift: any(&[VK_LSHIFT, VK_RSHIFT]),
            }
        }

        /// 是否需要鼠标钩子：手势进行中，或当前修饰键恰好匹配某条绑定。
        fn mouse_hook_needed(&self) -> bool {
            self.gesture.needs_mouse_input() || self.bindings.iter().any(|b| b.modifiers == self.modifiers())
        }
    }

    /// 平台实现句柄。
    pub struct Inner {
        /// 钩子线程。
        thread: Option<JoinHandle<()>>,
        /// 钩子线程的系统线程 ID。
        thread_id: u32,
    }

    impl Inner {
        /// 启动钩子线程。
        pub fn start(handler: DragHandler) -> Result<Self, String> {
            if lock().is_some() {
                return Err("全局鼠标手势服务已在运行".into());
            }
            let (tx, rx) = channel::<Result<u32, String>>();
            let handler: Arc<dyn Fn(DragEvent) + Send + Sync> = Arc::from(handler);
            let thread = std::thread::Builder::new()
                .name("platform-global-mouse".into())
                .spawn(move || run_thread(handler, tx))
                .map_err(|e| format!("创建钩子线程失败: {e}"))?;
            match rx.recv() {
                Ok(Ok(thread_id)) => Ok(Self { thread: Some(thread), thread_id }),
                Ok(Err(e)) => {
                    let _ = thread.join();
                    Err(e)
                }
                Err(_) => Err("钩子线程意外退出".into()),
            }
        }

        /// 替换绑定并通知线程重新评估。
        pub fn set_bindings(&self, bindings: Vec<Binding>) {
            if let Some(state) = lock().as_mut() {
                state.bindings = bindings;
            }
            self.post(WM_CANCEL);
        }

        /// 取消进行中的手势。
        pub fn cancel(&self) {
            self.post(WM_CANCEL);
        }

        /// 给钩子线程投递私有消息。
        fn post(&self, message: u32) {
            // SAFETY: 线程 ID 来自本服务创建的线程；失败只意味着线程已退出。
            let _ = unsafe { PostThreadMessageW(self.thread_id, message, WPARAM(0), LPARAM(0)) };
        }
    }

    impl Drop for Inner {
        /// 通知线程退出并等待它卸载全部钩子。
        fn drop(&mut self) {
            // SAFETY: 同 `post`。
            let _ = unsafe { PostThreadMessageW(self.thread_id, WM_QUIT, WPARAM(0), LPARAM(0)) };
            if let Some(thread) = self.thread.take() {
                let _ = thread.join();
            }
        }
    }

    /// 钩子线程主体：装键盘钩子、跑消息循环、退出时卸载。
    fn run_thread(handler: Arc<dyn Fn(DragEvent) + Send + Sync>, ready: std::sync::mpsc::Sender<Result<u32, String>>) {
        // SAFETY: 无前置条件。
        let hmod = unsafe { GetModuleHandleW(None) }.map(|m| HINSTANCE(m.0)).unwrap_or_default();
        // SAFETY: 回调是本模块的 `extern "system"` 函数；hmod 为本进程模块。
        let keyboard = unsafe { SetWindowsHookExW(WH_KEYBOARD_LL, Some(keyboard_proc), Some(hmod), 0) };
        let keyboard = match keyboard {
            Ok(h) => h,
            Err(e) => {
                let _ = ready.send(Err(format!("安装键盘钩子失败: {e}")));
                return;
            }
        };
        *lock() = Some(State {
            bindings: Vec::new(),
            gesture: Gesture::new(),
            held_keys: BTreeSet::new(),
            handler,
            keyboard_hook: Some(keyboard.0 as isize),
            mouse_hook: None,
        });
        // SAFETY: 无前置条件。
        let _ = ready.send(Ok(unsafe { GetCurrentThreadId() }));
        let mut msg = MSG::default();
        // SAFETY: msg 是有效输出缓冲；循环在 WM_QUIT（返回 0）或错误（-1）时结束。
        while unsafe { GetMessageW(&mut msg, None, 0, 0) }.0 > 0 {
            match msg.message {
                WM_REFRESH => sync_mouse_hook(),
                WM_CANCEL => {
                    let event = lock().as_mut().and_then(|s| s.gesture.handle(Input::Cancel, &[]).event);
                    dispatch(event);
                    sync_mouse_hook();
                }
                _ => {
                    // SAFETY: msg 来自 GetMessageW。
                    unsafe {
                        let _ = TranslateMessage(&msg);
                        DispatchMessageW(&msg);
                    }
                }
            }
        }
        if let Some(state) = lock().take() {
            for hook in [state.mouse_hook, state.keyboard_hook].into_iter().flatten() {
                // SAFETY: 钩子句柄由本线程安装。
                let _ = unsafe { UnhookWindowsHookEx(HHOOK(hook as *mut _)) };
            }
        }
    }

    /// 在锁外把事件交给处理函数。
    fn dispatch(event: Option<DragEvent>) {
        let Some(event) = event else {
            return;
        };
        let handler = lock().as_ref().map(|s| Arc::clone(&s.handler));
        if let Some(handler) = handler {
            handler(event);
        }
    }

    /// 按需要安装 / 卸载鼠标钩子。
    fn sync_mouse_hook() {
        let mut guard = lock();
        let Some(state) = guard.as_mut() else {
            return;
        };
        let needed = state.mouse_hook_needed();
        match (needed, state.mouse_hook) {
            (true, None) => {
                // SAFETY: 回调是本模块的 `extern "system"` 函数。
                let hmod = unsafe { GetModuleHandleW(None) }.map(|m| HINSTANCE(m.0)).unwrap_or_default();
                // SAFETY: 同上。
                match unsafe { SetWindowsHookExW(WH_MOUSE_LL, Some(mouse_proc), Some(hmod), 0) } {
                    Ok(h) => state.mouse_hook = Some(h.0 as isize),
                    Err(e) => tracing::warn!(error = %e, "安装鼠标钩子失败"),
                }
            }
            (false, Some(hook)) => {
                // SAFETY: 钩子句柄由本线程安装。
                let _ = unsafe { UnhookWindowsHookEx(HHOOK(hook as *mut _)) };
                state.mouse_hook = None;
            }
            _ => {}
        }
    }

    /// 虚拟键是否是我们关心的修饰键。
    fn is_modifier(vk: u16) -> bool {
        [VK_LWIN, VK_RWIN, VK_LCONTROL, VK_RCONTROL, VK_LMENU, VK_RMENU, VK_LSHIFT, VK_RSHIFT]
            .iter()
            .any(|k| k.0 == vk)
    }

    /// 低级键盘钩子：跟踪修饰键，并随之决定是否需要鼠标钩子。
    unsafe extern "system" fn keyboard_proc(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
        if code >= 0 {
            // SAFETY: code>=0 时 lparam 指向有效的 KBDLLHOOKSTRUCT。
            let info = unsafe { &*(lparam.0 as *const KBDLLHOOKSTRUCT) };
            let vk = info.vkCode as u16;
            if !ignore(info.flags.0 & LLKHF_INJECTED != 0) && is_modifier(vk) {
                let down = matches!(wparam.0 as u32, WM_KEYDOWN | WM_SYSKEYDOWN);
                let up = matches!(wparam.0 as u32, WM_KEYUP | WM_SYSKEYUP);
                if down || up {
                    if let Some(state) = lock().as_mut() {
                        if down {
                            state.held_keys.insert(vk);
                        } else {
                            state.held_keys.remove(&vk);
                        }
                    }
                    request_refresh();
                }
            }
        }
        // SAFETY: 原样转交下一个钩子。
        unsafe { CallNextHookEx(None, code, wparam, lparam) }
    }

    /// 请求本线程稍后（回到消息循环时）重新评估鼠标钩子，不在钩子回调里直接装 / 卸钩子。
    fn request_refresh() {
        // SAFETY: 给当前线程自己投递私有消息。
        let _ = unsafe { PostThreadMessageW(GetCurrentThreadId(), WM_REFRESH, WPARAM(0), LPARAM(0)) };
    }

    /// 除 `except` 外是否还有鼠标键按着。
    fn others_held(except: MouseKey) -> bool {
        [
            (MouseKey::Left, VK_LBUTTON),
            (MouseKey::Right, VK_RBUTTON),
            (MouseKey::Middle, VK_MBUTTON),
            (MouseKey::Back, VK_XBUTTON1),
            (MouseKey::Forward, VK_XBUTTON2),
        ]
        .iter()
        .filter(|(key, _)| *key != except)
        // SAFETY: 纯值参数。
        .any(|(_, vk)| unsafe { GetAsyncKeyState(i32::from(vk.0)) } as u16 & 0x8000 != 0)
    }

    /// 发一个无意义的按键，让系统认为 Win 键期间发生过别的操作，松开 Win 时不再弹开始菜单。
    fn suppress_start_menu() {
        let key = |flags| INPUT {
            r#type: INPUT_KEYBOARD,
            Anonymous: INPUT_0 {
                ki: KEYBDINPUT { wVk: VIRTUAL_KEY(VK_NOOP), wScan: 0, dwFlags: flags, time: 0, dwExtraInfo: 0 },
            },
        };
        let inputs = [key(Default::default()), key(KEYEVENTF_KEYUP)];
        // SAFETY: inputs 是有效数组，大小参数与结构体一致。
        let _ = unsafe { SendInput(&inputs, std::mem::size_of::<INPUT>() as i32) };
    }

    /// 低级鼠标钩子：把鼠标消息翻译成手势输入。
    unsafe extern "system" fn mouse_proc(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
        if code >= 0 {
            // SAFETY: code>=0 时 lparam 指向有效的 MSLLHOOKSTRUCT。
            let info = unsafe { &*(lparam.0 as *const MSLLHOOKSTRUCT) };
            if !ignore(info.flags & LLMHF_INJECTED != 0) {
                let pos = Point { x: info.pt.x, y: info.pt.y };
                let xbutton = || if (info.mouseData >> 16) & 0xFFFF == 1 { MouseKey::Back } else { MouseKey::Forward };
                let (press, release): (Option<MouseKey>, Option<MouseKey>) = match wparam.0 as u32 {
                    WM_LBUTTONDOWN => (Some(MouseKey::Left), None),
                    WM_RBUTTONDOWN => (Some(MouseKey::Right), None),
                    WM_MBUTTONDOWN => (Some(MouseKey::Middle), None),
                    WM_XBUTTONDOWN => (Some(xbutton()), None),
                    WM_LBUTTONUP => (None, Some(MouseKey::Left)),
                    WM_RBUTTONUP => (None, Some(MouseKey::Right)),
                    WM_MBUTTONUP => (None, Some(MouseKey::Middle)),
                    WM_XBUTTONUP => (None, Some(xbutton())),
                    _ => (None, None),
                };
                let moved = wparam.0 as u32 == WM_MOUSEMOVE;
                let input = match (press, release) {
                    (Some(button), _) => Some(Input::Press {
                        button,
                        pos,
                        modifiers: lock().as_ref().map(State::modifiers).unwrap_or_default(),
                        others_held: others_held(button),
                    }),
                    (_, Some(button)) => Some(Input::Release { button, pos }),
                    _ if moved => Some(Input::Move { pos }),
                    _ => None,
                };
                if let Some(input) = input {
                    let (outcome, win_gesture) = {
                        let mut guard = lock();
                        match guard.as_mut() {
                            Some(state) => {
                                let bindings = state.bindings.clone();
                                let outcome = state.gesture.handle(input, &bindings);
                                let win = matches!(outcome.event, Some(DragEvent::Begin { .. })) && state.modifiers().win;
                                (outcome, win)
                            }
                            None => (Default::default(), false),
                        }
                    };
                    if win_gesture {
                        suppress_start_menu();
                    }
                    let finished = matches!(outcome.event, Some(DragEvent::Finish { .. } | DragEvent::Cancel { .. }));
                    dispatch(outcome.event);
                    if finished || press.is_some() || release.is_some() {
                        sync_mouse_hook();
                    }
                    if outcome.consumed {
                        return LRESULT(1);
                    }
                }
            }
        }
        // SAFETY: 原样转交下一个钩子。
        unsafe { CallNextHookEx(None, code, wparam, lparam) }
    }
}

#[cfg(not(windows))]
mod imp {
    use super::{Binding, DragHandler};

    /// 非 Windows 平台没有实现。
    pub fn set_accept_injected(_accept: bool) {}

    /// 非 Windows 平台的占位实现。
    pub struct Inner;

    impl Inner {
        /// 非 Windows 平台不支持。
        pub fn start(_handler: DragHandler) -> Result<Self, String> {
            Err("当前平台不支持全局鼠标手势".into())
        }

        /// 无操作。
        pub fn set_bindings(&self, _bindings: Vec<Binding>) {}

        /// 无操作。
        pub fn cancel(&self) {}
    }
}

#[cfg(all(test, windows))]
mod real_hook_tests {
    use super::*;
    use std::sync::{Arc, Mutex};
    use std::time::Duration;
    use windows::Win32::UI::Input::KeyboardAndMouse::{
        INPUT, INPUT_0, INPUT_KEYBOARD, INPUT_MOUSE, KEYBDINPUT, KEYEVENTF_KEYUP, MOUSEEVENTF_LEFTDOWN, MOUSEEVENTF_LEFTUP,
        MOUSEINPUT, SendInput, VIRTUAL_KEY, VK_LCONTROL,
    };
    use windows::Win32::UI::WindowsAndMessaging::{GetSystemMetrics, SM_CXSCREEN, SM_CYSCREEN, SetCursorPos};

    /// 发一个键盘事件。
    fn key(vk: VIRTUAL_KEY, up: bool) {
        let input = INPUT {
            r#type: INPUT_KEYBOARD,
            Anonymous: INPUT_0 {
                ki: KEYBDINPUT { wVk: vk, wScan: 0, dwFlags: if up { KEYEVENTF_KEYUP } else { Default::default() }, time: 0, dwExtraInfo: 0 },
            },
        };
        // SAFETY: 单个有效 INPUT。
        unsafe { SendInput(&[input], std::mem::size_of::<INPUT>() as i32) };
    }

    /// 发一个鼠标左键事件。
    fn left(up: bool) {
        let input = INPUT {
            r#type: INPUT_MOUSE,
            Anonymous: INPUT_0 {
                mi: MOUSEINPUT {
                    dx: 0,
                    dy: 0,
                    mouseData: 0,
                    dwFlags: if up { MOUSEEVENTF_LEFTUP } else { MOUSEEVENTF_LEFTDOWN },
                    time: 0,
                    dwExtraInfo: 0,
                },
            },
        };
        // SAFETY: 单个有效 INPUT。
        unsafe { SendInput(&[input], std::mem::size_of::<INPUT>() as i32) };
    }

    /// 真实钩子：Ctrl + 左键拖动产生 开始 → 更新 → 结束；没按 Ctrl 的左键拖动不产生事件。
    /// 会移动真实光标并发送注入输入，默认忽略；需要时用 `--ignored` 手动跑。
    #[test]
    #[ignore = "会移动真实光标并注入输入；需要系统把注入的鼠标事件交给低级鼠标钩子（部分环境不会）"]
    fn ctrl_left_drag_produces_gesture_events() {
        let events = Arc::new(Mutex::new(Vec::<DragEvent>::new()));
        let sink = Arc::clone(&events);
        let service = GlobalMouseService::start(Box::new(move |e| sink.lock().unwrap().push(e))).unwrap();
        service.accept_injected_input(true);
        service.set_bindings(vec![Binding {
            action: "copy".into(),
            modifiers: Modifiers { ctrl: true, ..Default::default() },
            button: MouseKey::Left,
        }]);
        std::thread::sleep(Duration::from_millis(200));
        // 光标放到任务栏空白处（探索者进程、中等完整性级别）：钩子收不到发往更高权限窗口的输入（UIPI），
        // 测试不能落在管理员终端上
        // SAFETY: 纯值参数。
        let (sw, sh) = unsafe { (GetSystemMetrics(SM_CXSCREEN), GetSystemMetrics(SM_CYSCREEN)) };
        let (base_x, base_y) = (sw / 2, sh - 8);
        unsafe { SetCursorPos(base_x, base_y).ok() };
        // 先不按 Ctrl：左键拖动不应有事件（注意：这会在当前前台窗口产生一次真实点击，选在屏幕空白处）
        key(VK_LCONTROL, false);
        std::thread::sleep(Duration::from_millis(200));
        left(false);
        std::thread::sleep(Duration::from_millis(100));
        // SAFETY: 纯值参数。
        for dx in [20, 60, 120] {
            unsafe { SetCursorPos(base_x + dx, base_y).ok() };
            std::thread::sleep(Duration::from_millis(60));
        }
        left(true);
        std::thread::sleep(Duration::from_millis(200));
        key(VK_LCONTROL, true);
        std::thread::sleep(Duration::from_millis(200));
        drop(service);
        let got = events.lock().unwrap().clone();
        assert!(matches!(got.first(), Some(DragEvent::Begin { id: 1, action, .. }) if action == "copy"), "{got:?}");
        assert!(got.iter().any(|e| matches!(e, DragEvent::Update { .. })), "{got:?}");
        assert!(matches!(got.last(), Some(DragEvent::Finish { id: 1, pos }) if pos.x >= base_x + 100), "{got:?}");
    }
}
