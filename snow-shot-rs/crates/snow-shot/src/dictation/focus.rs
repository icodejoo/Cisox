//! 可输入焦点检测：把前台焦点读数判定成“能键入”或“不能键入（以及原因）”。
//!
//! 读数由 `snow_platform::focus_probe` 采集，判定是纯函数 [`classify`]；采集接口抽成 [`FocusProbe`]，
//! 单测用 Fake 覆盖：可编辑、只读、无焦点、UIA 出错、权限更高、未知控件。
//! 原则是宁可多弹浮窗也不丢字：凡是“否”或“不确定”一律判为不能键入。

use snow_platform::focus_probe::{
    CONTROL_TYPE_DOCUMENT, CONTROL_TYPE_EDIT, FocusReading, UiaElement,
};

/// 不能键入的原因。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NoTypeReason {
    /// 没有前台窗口或没有焦点元素。
    NoFocus,
    /// 焦点控件不可编辑（只读、禁用、密码框、非输入类控件）。
    NotEditable,
    /// 目标进程权限高于本程序（UIPI 会静默吞掉注入的按键）。
    Elevated,
    /// 读不出来或判断依据不足（UIA 出错、读不到进程权限等）。
    Uncertain,
}

/// 判定结果。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    /// 可以键入。
    Type,
    /// 不能键入。
    NoType(NoTypeReason),
}

/// 焦点探测接口：真实实现读系统，测试用 Fake。
pub trait FocusProbe {
    /// 读取一次前台焦点。
    fn read(&mut self) -> FocusReading;
}

/// 系统探测（UI Automation + 令牌完整性级别）。
#[derive(Debug, Default)]
pub struct SystemProbe;

impl FocusProbe for SystemProbe {
    /// 读取系统焦点；可能阻塞（目标程序无响应时），应在后台线程调用。
    fn read(&mut self) -> FocusReading {
        snow_platform::focus_probe::read_focus()
    }
}

/// 读一次并判定。
///
/// # 参数
/// - `probe`：焦点探测。
pub fn probe_verdict(probe: &mut dyn FocusProbe) -> Verdict {
    classify(&probe.read())
}

/// 判定焦点元素本身是否可编辑；`caret` 为前台线程是否有系统插入符（传统控件的辅助证据）。
fn element_verdict(element: &UiaElement, caret: bool) -> Verdict {
    use NoTypeReason::NotEditable;
    if !element.enabled || element.password || element.value_read_only == Some(true) {
        return Verdict::NoType(NotEditable);
    }
    let focusable = element.has_keyboard_focus || element.keyboard_focusable;
    let input_type = matches!(
        element.control_type,
        CONTROL_TYPE_EDIT | CONTROL_TYPE_DOCUMENT
    );
    if !input_type {
        // 传统自绘控件常是 Pane/Custom，UIA 焦点元素也可能只是外层容器（不标记持有键盘焦点），
        // 此时只有系统插入符能证明前台线程在接收文字输入
        return if caret {
            Verdict::Type
        } else {
            Verdict::NoType(NotEditable)
        };
    }
    if !focusable {
        return Verdict::NoType(NotEditable);
    }
    match (
        element.value_read_only,
        element.has_text_pattern,
        element.control_type,
    ) {
        // ValuePattern 明确可写
        (Some(false), _, _) => Verdict::Type,
        // 没有 ValuePattern 但有 TextPattern 的编辑框（如 RichEdit）
        (None, true, CONTROL_TYPE_EDIT) => Verdict::Type,
        // 文档类控件只有 TextPattern 时分不清是可编辑文档还是只读网页，要有插入符才算
        (None, true, _) if caret => Verdict::Type,
        _ => Verdict::NoType(NoTypeReason::Uncertain),
    }
}

/// 把一次读数判定成能否键入。
///
/// 顺序：无前台 → 目标权限更高或读不出 → UIA 出错 → 无焦点元素 → 元素类型与状态。
///
/// # 参数
/// - `reading`：原始读数。
///
/// ```ignore
/// assert_eq!(classify(&reading_without_foreground()), Verdict::NoType(NoTypeReason::NoFocus));
/// ```
pub fn classify(reading: &FocusReading) -> Verdict {
    if reading.foreground.is_none() {
        return Verdict::NoType(NoTypeReason::NoFocus);
    }
    match reading.target_above_us {
        Some(true) => return Verdict::NoType(NoTypeReason::Elevated),
        None => return Verdict::NoType(NoTypeReason::Uncertain),
        Some(false) => {}
    }
    match &reading.uia {
        Err(_) => Verdict::NoType(NoTypeReason::Uncertain),
        Ok(None) => {
            if reading.caret {
                Verdict::Type
            } else {
                Verdict::NoType(NoTypeReason::NoFocus)
            }
        }
        Ok(Some(element)) => element_verdict(element, reading.caret),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 一个可编辑的 Edit 元素。
    fn edit() -> UiaElement {
        UiaElement {
            control_type: CONTROL_TYPE_EDIT,
            enabled: true,
            has_keyboard_focus: true,
            keyboard_focusable: true,
            password: false,
            value_read_only: Some(false),
            has_text_pattern: false,
        }
    }

    /// 基本读数：有前台、权限不高于本进程、没有插入符。
    fn reading(uia: Result<Option<UiaElement>, String>) -> FocusReading {
        FocusReading {
            foreground: Some(10),
            foreground_pid: 99,
            target_above_us: Some(false),
            caret: false,
            uia,
        }
    }

    /// Fake 探测：返回固定读数。
    struct FakeProbe(FocusReading);

    impl FocusProbe for FakeProbe {
        fn read(&mut self) -> FocusReading {
            self.0.clone()
        }
    }

    /// 可编辑的 Edit（ValuePattern 非只读）→ 能键入。
    #[test]
    fn editable_edit_is_typable() {
        let mut probe = FakeProbe(reading(Ok(Some(edit()))));
        assert_eq!(probe_verdict(&mut probe), Verdict::Type);
    }

    /// 只读 / 禁用 / 密码框 → 不可编辑，即使类型是 Edit。
    #[test]
    fn readonly_disabled_password_are_not_editable() {
        for mutate in [
            (|e: &mut UiaElement| e.value_read_only = Some(true)) as fn(&mut UiaElement),
            |e| e.enabled = false,
            |e| e.password = true,
        ] {
            let mut element = edit();
            mutate(&mut element);
            assert_eq!(
                classify(&reading(Ok(Some(element)))),
                Verdict::NoType(NoTypeReason::NotEditable)
            );
        }
    }

    /// 没有前台窗口 / 没有焦点元素（也没有插入符）→ 无焦点。
    #[test]
    fn no_focus_cases() {
        let mut r = reading(Ok(None));
        assert_eq!(classify(&r), Verdict::NoType(NoTypeReason::NoFocus));
        r.foreground = None;
        r.uia = Ok(Some(edit()));
        assert_eq!(classify(&r), Verdict::NoType(NoTypeReason::NoFocus));
    }

    /// UIA 出错 → 不确定，一律走浮窗；即使有插入符也不冒险。
    #[test]
    fn uia_error_is_uncertain() {
        let mut r = reading(Err("boom".into()));
        r.caret = true;
        assert_eq!(classify(&r), Verdict::NoType(NoTypeReason::Uncertain));
    }

    /// 目标权限更高 → 不可键入（UIPI）；读不出权限 → 不确定。
    #[test]
    fn elevated_and_unknown_integrity() {
        let mut r = reading(Ok(Some(edit())));
        r.target_above_us = Some(true);
        assert_eq!(classify(&r), Verdict::NoType(NoTypeReason::Elevated));
        r.target_above_us = None;
        assert_eq!(classify(&r), Verdict::NoType(NoTypeReason::Uncertain));
    }

    /// 游戏 / 自绘 UI 等未知控件：不是 Edit/Document 且没有插入符 → 不可键入；有插入符就放行，
    /// 即使 UIA 焦点元素是外层容器、没标记持有键盘焦点（WinForms 等实测如此）。
    #[test]
    fn unknown_controls() {
        let pane = UiaElement {
            control_type: 50033, // Pane
            value_read_only: None,
            ..edit()
        };
        assert_eq!(
            classify(&reading(Ok(Some(pane.clone())))),
            Verdict::NoType(NoTypeReason::NotEditable)
        );
        let mut r = reading(Ok(Some(pane.clone())));
        r.caret = true;
        assert_eq!(classify(&r), Verdict::Type);
        let container = UiaElement {
            has_keyboard_focus: false,
            keyboard_focusable: false,
            ..pane
        };
        r.uia = Ok(Some(container.clone()));
        assert_eq!(classify(&r), Verdict::Type);
        // 禁用 / 密码框即使有插入符也不键入
        for mutate in [
            (|e: &mut UiaElement| e.enabled = false) as fn(&mut UiaElement),
            |e| e.password = true,
        ] {
            let mut element = container.clone();
            mutate(&mut element);
            r.uia = Ok(Some(element));
            assert_eq!(classify(&r), Verdict::NoType(NoTypeReason::NotEditable));
        }
    }

    /// Edit 只有 TextPattern 也算可编辑；Document 只有 TextPattern 时需要插入符，否则判不确定（只读网页）。
    #[test]
    fn text_pattern_rules() {
        let rich = UiaElement {
            value_read_only: None,
            has_text_pattern: true,
            ..edit()
        };
        assert_eq!(classify(&reading(Ok(Some(rich)))), Verdict::Type);

        let page = UiaElement {
            control_type: CONTROL_TYPE_DOCUMENT,
            value_read_only: None,
            has_text_pattern: true,
            ..edit()
        };
        assert_eq!(
            classify(&reading(Ok(Some(page.clone())))),
            Verdict::NoType(NoTypeReason::Uncertain)
        );
        let mut r = reading(Ok(Some(page)));
        r.caret = true;
        assert_eq!(classify(&r), Verdict::Type);

        let bare = UiaElement {
            value_read_only: None,
            has_text_pattern: false,
            ..edit()
        };
        assert_eq!(
            classify(&reading(Ok(Some(bare)))),
            Verdict::NoType(NoTypeReason::Uncertain)
        );
    }

    /// 可写的 Document（浏览器 contenteditable）→ 能键入；不能获键盘焦点的 Edit → 不可。
    #[test]
    fn document_value_pattern_and_focusability() {
        let doc = UiaElement {
            control_type: CONTROL_TYPE_DOCUMENT,
            ..edit()
        };
        assert_eq!(classify(&reading(Ok(Some(doc)))), Verdict::Type);
        let dead = UiaElement {
            has_keyboard_focus: false,
            keyboard_focusable: false,
            ..edit()
        };
        assert_eq!(
            classify(&reading(Ok(Some(dead)))),
            Verdict::NoType(NoTypeReason::NotEditable)
        );
    }

    /// 没有焦点元素但前台线程有系统插入符（传统 Win32 编辑控件）→ 能键入。
    #[test]
    fn caret_without_uia_element() {
        let mut r = reading(Ok(None));
        r.caret = true;
        assert_eq!(classify(&r), Verdict::Type);
    }
}
