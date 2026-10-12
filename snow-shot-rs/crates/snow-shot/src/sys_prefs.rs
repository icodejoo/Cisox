//! 系统偏好读取:深色模式与界面语言(直接读当前用户注册表,不起子进程)。
//!
//! 早期实现经 `reg query` 子进程,每次约 80ms(终端管控软件注入会更慢),在主线程上调用会直接拖慢覆盖窗首帧;
//! 现改为 `RegGetValueW`,微秒级且无额外依赖。

use snow_platform::system_integration::{read_user_dword, read_user_string};

/// 系统个性化主题的注册表路径。
const REG_PATH_PERSONALIZE: &str = r"Software\Microsoft\Windows\CurrentVersion\Themes\Personalize";

/// 系统国际化设置的注册表路径。
const REG_PATH_INTERNATIONAL: &str = r"Control Panel\International";

/// 深色模式的注册表值名。
const REG_VALUE_APPS_USE_LIGHT_THEME: &str = "AppsUseLightTheme";

/// 界面语言的注册表值名。
const REG_VALUE_LOCALE_NAME: &str = "LocaleName";

/// 界面语言读取失败时的默认值。
const DEFAULT_LANGUAGE: &str = "en-US";

/// 系统是否使用深色应用主题(`AppsUseLightTheme` == 0)。
///
/// 返回:深色返回 `true`;读取失败或非 Windows 按浅色处理返回 `false`。结果不缓存(读取本身是微秒级)。
///
/// 示例:`let dark = system_prefers_dark();`
pub fn system_prefers_dark() -> bool {
    read_user_dword(REG_PATH_PERSONALIZE, REG_VALUE_APPS_USE_LIGHT_THEME).is_some_and(|v| v == 0)
}

/// 系统界面语言标记(如 `zh-CN`)。
///
/// 返回:语言标记;读取失败、值为空或非 Windows 返回 `en-US`。
///
/// 示例:`let lang = system_ui_language();`
pub fn system_ui_language() -> String {
    read_user_string(REG_PATH_INTERNATIONAL, REG_VALUE_LOCALE_NAME)
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| DEFAULT_LANGUAGE.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 界面语言永远非空（读不到时回退默认值）。
    #[test]
    fn ui_language_is_never_empty() {
        assert!(!system_ui_language().is_empty());
    }

    /// 在 Windows 上读到的是系统真实语言标记（形如 `xx` 或 `xx-YY`）。
    #[cfg(windows)]
    #[test]
    fn ui_language_looks_like_a_locale_tag() {
        let lang = system_ui_language();
        assert!(
            lang.chars().all(|c| c.is_ascii_alphanumeric() || c == '-'),
            "{lang}"
        );
    }

    /// 深色判定不 panic（结果取决于本机主题，不做断言）。
    #[test]
    fn prefers_dark_does_not_panic() {
        let _ = system_prefers_dark();
    }

    /// 在进程内读取：连读 200 轮应远小于 1 秒（`reg.exe` 子进程方案每轮 2 次、各约 80ms，需要 30 秒以上）。
    #[cfg(windows)]
    #[test]
    fn reads_are_in_process() {
        let start = std::time::Instant::now();
        for _ in 0..200 {
            let _ = system_ui_language();
            let _ = system_prefers_dark();
        }
        assert!(
            start.elapsed() < std::time::Duration::from_secs(1),
            "{:?}",
            start.elapsed()
        );
    }
}
