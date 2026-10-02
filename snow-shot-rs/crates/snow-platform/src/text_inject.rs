//! 向前台窗口注入键盘输入：逐字符发送 Unicode 字符、退格。
//!
//! Windows 用 `SendInput`，字符走 `KEYEVENTF_UNICODE`（`wVk=0, wScan=UTF-16 码元`），
//! 不依赖键盘布局与输入法。注意：`SendInput` 被 UIPI 拦截（目标权限更高）时不会报错，
//! 调用方要自行用 [`crate::focus_probe`] 探测；已按下的修饰键会干扰合成事件，
//! 发送前应检查 [`physical_modifiers_held`]。

/// 一次按键动作。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyStroke {
    /// 键入一个字符（超出基本平面的字符会拆成两个 UTF-16 码元）。
    Char(char),
    /// 退格（`VK_BACK`）。
    Backspace,
}

/// 退格键的虚拟键码。
pub const VK_BACK_CODE: u16 = 0x08;

/// 底层键盘事件（与平台无关的中间表示，便于离屏测试）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KeyEvent {
    /// 虚拟键码；Unicode 事件为 0。
    pub vk: u16,
    /// 扫描码字段；Unicode 事件放 UTF-16 码元。
    pub scan: u16,
    /// 是否为 Unicode 事件。
    pub unicode: bool,
    /// 是否为抬起事件。
    pub key_up: bool,
}

/// 把按键动作展开成底层事件序列（每个键先按下再抬起）。
///
/// # 参数
/// - `strokes`：按键动作。
///
/// # 返回
/// 按顺序的键盘事件。
///
/// ```
/// use snow_platform::text_inject::{encode, KeyStroke};
/// let ev = encode(&[KeyStroke::Backspace, KeyStroke::Char('好')]);
/// assert_eq!(ev.len(), 4);
/// assert!(ev[2].unicode && ev[2].scan == '好' as u16 && !ev[2].key_up);
/// ```
pub fn encode(strokes: &[KeyStroke]) -> Vec<KeyEvent> {
    let mut out = Vec::with_capacity(strokes.len() * 2);
    for stroke in strokes {
        match stroke {
            KeyStroke::Backspace => {
                for key_up in [false, true] {
                    out.push(KeyEvent {
                        vk: VK_BACK_CODE,
                        scan: 0,
                        unicode: false,
                        key_up,
                    });
                }
            }
            KeyStroke::Char(ch) => {
                let mut units = [0u16; 2];
                for unit in ch.encode_utf16(&mut units) {
                    for key_up in [false, true] {
                        out.push(KeyEvent {
                            vk: 0,
                            scan: *unit,
                            unicode: true,
                            key_up,
                        });
                    }
                }
            }
        }
    }
    out
}

/// 把一批按键动作用一次 `SendInput` 发出（回删与重打放同一次调用，不会被用户按键打断）。
///
/// # 参数
/// - `strokes`：按键动作；空切片直接成功。
///
/// # 返回
/// 全部事件注入成功返回 `Ok`；被其它输入线程阻塞或部分失败返回说明。非 Windows 恒返回错误。
pub fn send_strokes(strokes: &[KeyStroke]) -> Result<(), String> {
    if strokes.is_empty() {
        return Ok(());
    }
    #[cfg(windows)]
    {
        win::send(&encode(strokes))
    }
    #[cfg(not(windows))]
    {
        Err("当前平台不支持键入".to_string())
    }
}

/// 当前是否有修饰键（Ctrl / Alt / Shift / Win）在物理上被按住。
///
/// 非 Windows 恒为 `false`。
pub fn physical_modifiers_held() -> bool {
    #[cfg(windows)]
    {
        win::modifiers_held()
    }
    #[cfg(not(windows))]
    {
        false
    }
}

/// 当前前台窗口句柄（数值）；没有前台窗口返回 `None`。
pub fn foreground_window() -> Option<isize> {
    #[cfg(windows)]
    {
        win::foreground()
    }
    #[cfg(not(windows))]
    {
        None
    }
}

#[cfg(windows)]
mod win {
    use super::KeyEvent;
    use windows::Win32::UI::Input::KeyboardAndMouse::{
        GetAsyncKeyState, INPUT, INPUT_0, INPUT_KEYBOARD, KEYBD_EVENT_FLAGS, KEYBDINPUT,
        KEYEVENTF_KEYUP, KEYEVENTF_UNICODE, SendInput, VIRTUAL_KEY, VK_CONTROL, VK_LWIN, VK_MENU,
        VK_RWIN, VK_SHIFT,
    };
    use windows::Win32::UI::WindowsAndMessaging::GetForegroundWindow;

    /// `GetAsyncKeyState` 返回值的“当前按下”位。
    const KEY_DOWN_MASK: u16 = 0x8000;

    /// 一次注入。
    pub(super) fn send(events: &[KeyEvent]) -> Result<(), String> {
        let inputs: Vec<INPUT> = events
            .iter()
            .map(|e| {
                let mut flags = KEYBD_EVENT_FLAGS(0);
                if e.unicode {
                    flags |= KEYEVENTF_UNICODE;
                }
                if e.key_up {
                    flags |= KEYEVENTF_KEYUP;
                }
                INPUT {
                    r#type: INPUT_KEYBOARD,
                    Anonymous: INPUT_0 {
                        ki: KEYBDINPUT {
                            wVk: VIRTUAL_KEY(e.vk),
                            wScan: e.scan,
                            dwFlags: flags,
                            time: 0,
                            dwExtraInfo: 0,
                        },
                    },
                }
            })
            .collect();
        // SAFETY: inputs 是有效的 INPUT 切片，cbSize 为单个 INPUT 的大小。
        let sent = unsafe { SendInput(&inputs, std::mem::size_of::<INPUT>() as i32) };
        if sent as usize == inputs.len() {
            Ok(())
        } else {
            Err(format!(
                "SendInput 只注入了 {sent}/{} 个事件（可能被其它输入线程阻塞）",
                inputs.len()
            ))
        }
    }

    /// 修饰键是否被按住。
    pub(super) fn modifiers_held() -> bool {
        [VK_CONTROL, VK_MENU, VK_SHIFT, VK_LWIN, VK_RWIN]
            .iter()
            .any(|vk| {
                // SAFETY: 只读查询按键状态。
                let state = unsafe { GetAsyncKeyState(i32::from(vk.0)) };
                (state as u16) & KEY_DOWN_MASK != 0
            })
    }

    /// 前台窗口句柄。
    pub(super) fn foreground() -> Option<isize> {
        // SAFETY: 只读查询前台窗口。
        let hwnd = unsafe { GetForegroundWindow() };
        (!hwnd.0.is_null()).then_some(hwnd.0 as isize)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 退格展开为按下 + 抬起，不带 Unicode 标志。
    #[test]
    fn backspace_encodes_vk_pair() {
        let ev = encode(&[KeyStroke::Backspace]);
        assert_eq!(
            ev,
            vec![
                KeyEvent {
                    vk: VK_BACK_CODE,
                    scan: 0,
                    unicode: false,
                    key_up: false
                },
                KeyEvent {
                    vk: VK_BACK_CODE,
                    scan: 0,
                    unicode: false,
                    key_up: true
                },
            ]
        );
    }

    /// 基本平面字符一个码元；补充平面字符（emoji）拆成代理对，两个码元各按下抬起。
    #[test]
    fn chars_encode_utf16_units() {
        let ev = encode(&[KeyStroke::Char('a')]);
        assert_eq!(ev.len(), 2);
        assert!(
            ev.iter()
                .all(|e| e.unicode && e.vk == 0 && e.scan == 'a' as u16)
        );
        assert!(!ev[0].key_up && ev[1].key_up);

        let ev = encode(&[KeyStroke::Char('😀')]);
        assert_eq!(ev.len(), 4);
        let mut units = [0u16; 2];
        '😀'.encode_utf16(&mut units);
        assert_eq!([ev[0].scan, ev[2].scan], units);
    }

    /// 空序列不产生事件，发送空序列直接成功（不触碰系统）。
    #[test]
    fn empty_is_noop() {
        assert!(encode(&[]).is_empty());
        assert!(send_strokes(&[]).is_ok());
    }
}
