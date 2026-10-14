//! 修饰键按住状态的纯逻辑：由虚拟键集合算出修饰键，并与「真实键态」对账。
//!
//! 键盘钩子可能漏收 KEYUP（系统高负载时 `LowLevelHooksTimeout` 跳过钩子、Win+L 锁屏、UAC 安全桌面、
//! 开始菜单吞键等），漏了就会让某个修饰键「永久按着」。这里的对账函数让调用方在鼠标事件到来时，
//! 用系统当前真实键态把这类卡住的键清掉。纯函数，不碰系统 API，方便单测。

use super::Modifiers;
use std::collections::BTreeSet;

/// 左 Win 虚拟键。
const VK_LWIN: u16 = 0x5B;
/// 右 Win 虚拟键。
const VK_RWIN: u16 = 0x5C;
/// 左 Shift 虚拟键。
const VK_LSHIFT: u16 = 0xA0;
/// 右 Shift 虚拟键。
const VK_RSHIFT: u16 = 0xA1;
/// 左 Ctrl 虚拟键。
const VK_LCONTROL: u16 = 0xA2;
/// 右 Ctrl 虚拟键。
const VK_RCONTROL: u16 = 0xA3;
/// 左 Alt 虚拟键。
const VK_LMENU: u16 = 0xA4;
/// 右 Alt 虚拟键。
const VK_RMENU: u16 = 0xA5;

/// 虚拟键是否是我们跟踪的修饰键。
///
/// # 参数
/// - `vk`：虚拟键码。
pub(super) fn is_modifier_key(vk: u16) -> bool {
    [VK_LWIN, VK_RWIN, VK_LSHIFT, VK_RSHIFT, VK_LCONTROL, VK_RCONTROL, VK_LMENU, VK_RMENU].contains(&vk)
}

/// 由按住的虚拟键集合算出修饰键集合（左右键任一按着即算）。
///
/// # 参数
/// - `held`：当前认为按住的虚拟键。
pub(super) fn modifiers_from_keys(held: &BTreeSet<u16>) -> Modifiers {
    let any = |keys: [u16; 2]| keys.iter().any(|k| held.contains(k));
    Modifiers {
        win: any([VK_LWIN, VK_RWIN]),
        ctrl: any([VK_LCONTROL, VK_RCONTROL]),
        alt: any([VK_LMENU, VK_RMENU]),
        shift: any([VK_LSHIFT, VK_RSHIFT]),
    }
}

/// 把「认为按住」的键集合与真实键态对账：物理上已松开的键（漏收 KEYUP 造成）会被剔除。
///
/// # 参数
/// - `held`：钩子跟踪出来的按住集合。
/// - `is_down`：查询某个虚拟键当前是否真的按着（如 `GetAsyncKeyState` 最高位）。
///
/// # 返回
/// 清理后的集合；与 `held` 相比变少即说明有键卡住。
pub(super) fn reconcile_held(held: &BTreeSet<u16>, is_down: impl Fn(u16) -> bool) -> BTreeSet<u16> {
    held.iter().copied().filter(|&vk| is_down(vk)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 造集合。
    fn set(keys: &[u16]) -> BTreeSet<u16> {
        keys.iter().copied().collect()
    }

    /// Win 修饰键。
    const WIN: Modifiers = Modifiers { win: true, ctrl: false, alt: false, shift: false };

    /// Win 卡住（跟踪里有、真实没按）：被清掉，且不再匹配 Win 手势。
    #[test]
    fn stuck_win_is_cleared_and_no_longer_matches() {
        let held = set(&[VK_LWIN]);
        assert_eq!(modifiers_from_keys(&held), WIN);
        let clean = reconcile_held(&held, |_| false);
        assert!(clean.is_empty());
        assert_ne!(modifiers_from_keys(&clean), WIN);
        assert!(modifiers_from_keys(&clean).is_empty());
    }

    /// Win 真按着：保留，仍然匹配。
    #[test]
    fn really_held_win_is_kept_and_matches() {
        let held = set(&[VK_RWIN]);
        let clean = reconcile_held(&held, |vk| vk == VK_RWIN);
        assert_eq!(clean, held);
        assert_eq!(modifiers_from_keys(&clean), WIN);
    }

    /// 多个修饰键部分卡住：只清掉卡住的，真按着的留下，修饰键组合随之变化。
    #[test]
    fn partially_stuck_modifiers_keep_only_real_ones() {
        let held = set(&[VK_LWIN, VK_LCONTROL, VK_RSHIFT]);
        let clean = reconcile_held(&held, |vk| vk == VK_LCONTROL);
        assert_eq!(clean, set(&[VK_LCONTROL]));
        assert_eq!(modifiers_from_keys(&clean), Modifiers { ctrl: true, ..Default::default() });
        assert_ne!(modifiers_from_keys(&clean), modifiers_from_keys(&held));
    }

    /// 空集合对账后仍为空，且不会去查键态。
    #[test]
    fn empty_set_stays_empty_without_queries() {
        let clean = reconcile_held(&BTreeSet::new(), |_| panic!("不应查询"));
        assert!(clean.is_empty());
    }

    /// 左右键任一按着都算该修饰键；非修饰键不在跟踪范围内。
    #[test]
    fn left_or_right_counts_and_non_modifiers_are_ignored() {
        assert!(modifiers_from_keys(&set(&[VK_RMENU])).alt);
        assert!(modifiers_from_keys(&set(&[VK_LSHIFT])).shift);
        assert!(is_modifier_key(VK_RCONTROL));
        assert!(!is_modifier_key(0x41));
    }
}
