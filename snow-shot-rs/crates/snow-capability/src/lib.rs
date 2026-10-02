//! 平台能力注册表（Linux 降级的基础设施）。
//!
//! 所属阶段：P1。声明各平台的能力矩阵（ADR-7），不满足时返回降级/不可用状态而非 panic。

use std::collections::HashMap;

/// 本 crate 的阶段标记，用于骨架连通性测试。
pub const PHASE: &str = "P1";

/// 原因文案 key：功能尚未在该平台实现。
pub const REASON_NOT_IMPLEMENTED: &str = "capability-reason-not-implemented";

/// 原因文案 key：Wayland 下能力受限。
pub const REASON_WAYLAND_LIMITED: &str = "capability-reason-wayland-limited";

/// 运行平台。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Platform {
    /// Windows（当前主线）。
    Windows,
    /// macOS（延后）。
    MacOs,
    /// Linux 及其他类 Unix（延后）。
    Linux,
}

impl Platform {
    /// 编译期判定当前平台，非 Windows/macOS 一律归为 Linux。
    ///
    /// # 返回
    /// 当前目标平台。
    ///
    /// ```rust
    /// use snow_capability::Platform;
    /// let _p = Platform::current();
    /// ```
    pub fn current() -> Self {
        if cfg!(target_os = "windows") {
            Platform::Windows
        } else if cfg!(target_os = "macos") {
            Platform::MacOs
        } else {
            Platform::Linux
        }
    }
}

/// 平台能力项（与 ADR-7 矩阵逐行对应）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Capability {
    /// 屏幕采集。
    ScreenCapture,
    /// 透明置顶覆盖窗与点击穿透。
    OverlayClickThrough,
    /// 全局热键。
    GlobalHotkey,
    /// 托盘。
    Tray,
    /// 元素选择（无障碍树）。
    ElementPicker,
    /// 录屏。
    ScreenRecording,
    /// 选中文本抓取。
    SelectedTextGrab,
    /// 本地崩溃转储（方案 §10 T3）。
    CrashDump,
}

impl Capability {
    /// 全部能力项。
    pub const ALL: [Capability; 8] = [
        Capability::ScreenCapture,
        Capability::OverlayClickThrough,
        Capability::GlobalHotkey,
        Capability::Tray,
        Capability::ElementPicker,
        Capability::ScreenRecording,
        Capability::SelectedTextGrab,
        Capability::CrashDump,
    ];

    /// 稳定的字符串标识，可用于日志与文案 key 派生。
    ///
    /// # 返回
    /// 该能力的唯一标识。
    ///
    /// ```rust
    /// use snow_capability::Capability;
    /// assert_eq!(Capability::Tray.id(), "Tray");
    /// ```
    pub fn id(self) -> &'static str {
        match self {
            Capability::ScreenCapture => "ScreenCapture",
            Capability::OverlayClickThrough => "OverlayClickThrough",
            Capability::GlobalHotkey => "GlobalHotkey",
            Capability::Tray => "Tray",
            Capability::ElementPicker => "ElementPicker",
            Capability::ScreenRecording => "ScreenRecording",
            Capability::SelectedTextGrab => "SelectedTextGrab",
            Capability::CrashDump => "CrashDump",
        }
    }
}

/// 能力在某平台上的状态。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CapabilityStatus {
    /// 完整支持。
    Supported,
    /// 降级可用。
    Degraded {
        /// 原因文案 key。
        reason: &'static str,
    },
    /// 不可用（UI 应显示禁用态）。
    Unsupported {
        /// 原因文案 key。
        reason: &'static str,
    },
}

impl CapabilityStatus {
    /// 是否可用（完整支持或降级均算可用）。
    ///
    /// # 返回
    /// 可用返回 `true`。
    ///
    /// ```rust
    /// use snow_capability::{CapabilityStatus, REASON_NOT_IMPLEMENTED};
    /// assert!(CapabilityStatus::Supported.is_usable());
    /// assert!(!CapabilityStatus::Unsupported { reason: REASON_NOT_IMPLEMENTED }.is_usable());
    /// ```
    pub fn is_usable(self) -> bool {
        !matches!(self, CapabilityStatus::Unsupported { .. })
    }

    /// 取原因文案 key。
    ///
    /// # 返回
    /// 降级/不可用时为 `Some(key)`，完整支持为 `None`。
    ///
    /// ```rust
    /// use snow_capability::{CapabilityStatus, REASON_WAYLAND_LIMITED};
    /// let s = CapabilityStatus::Degraded { reason: REASON_WAYLAND_LIMITED };
    /// assert_eq!(s.reason(), Some(REASON_WAYLAND_LIMITED));
    /// ```
    pub fn reason(self) -> Option<&'static str> {
        match self {
            CapabilityStatus::Supported => None,
            CapabilityStatus::Degraded { reason } | CapabilityStatus::Unsupported { reason } => {
                Some(reason)
            }
        }
    }
}

/// 能力注册表：按平台给出默认表，运行时可覆盖。
#[derive(Debug, Clone)]
pub struct CapabilityRegistry {
    /// 能力到状态的映射。
    table: HashMap<Capability, CapabilityStatus>,
}

impl CapabilityRegistry {
    /// 构造指定平台的默认能力表。
    ///
    /// # 参数
    /// - `platform`：目标平台。
    ///
    /// # 返回
    /// 含该平台默认状态的注册表。
    ///
    /// ```rust
    /// use snow_capability::{Capability, CapabilityRegistry, CapabilityStatus, Platform};
    /// let reg = CapabilityRegistry::for_platform(Platform::Windows);
    /// assert_eq!(reg.query(Capability::Tray), CapabilityStatus::Supported);
    /// ```
    pub fn for_platform(platform: Platform) -> Self {
        let table = Capability::ALL
            .into_iter()
            .map(|cap| (cap, Self::default_status(platform, cap)))
            .collect();
        Self { table }
    }

    /// 构造当前平台的默认能力表。
    ///
    /// # 返回
    /// 当前平台的注册表。
    ///
    /// ```rust
    /// use snow_capability::CapabilityRegistry;
    /// let _reg = CapabilityRegistry::for_current_platform();
    /// ```
    pub fn for_current_platform() -> Self {
        Self::for_platform(Platform::current())
    }

    /// 查询能力状态，永不 panic。
    ///
    /// # 参数
    /// - `cap`：待查询能力。
    ///
    /// # 返回
    /// 能力状态；缺失时按不可用（未实现）处理。
    ///
    /// ```rust
    /// use snow_capability::{Capability, CapabilityRegistry, Platform};
    /// let reg = CapabilityRegistry::for_platform(Platform::Windows);
    /// assert!(reg.query(Capability::OverlayClickThrough).is_usable());
    /// ```
    pub fn query(&self, cap: Capability) -> CapabilityStatus {
        self.table
            .get(&cap)
            .copied()
            .unwrap_or(CapabilityStatus::Unsupported {
                reason: REASON_NOT_IMPLEMENTED,
            })
    }

    /// 覆盖某能力状态（运行时探测结果或测试注入）。
    ///
    /// # 参数
    /// - `cap`：目标能力。
    /// - `status`：新状态。
    ///
    /// ```rust
    /// use snow_capability::{Capability, CapabilityRegistry, CapabilityStatus, Platform};
    /// let mut reg = CapabilityRegistry::for_platform(Platform::Windows);
    /// reg.override_status(Capability::Tray, CapabilityStatus::Unsupported { reason: "x" });
    /// assert!(!reg.query(Capability::Tray).is_usable());
    /// ```
    pub fn override_status(&mut self, cap: Capability, status: CapabilityStatus) {
        self.table.insert(cap, status);
    }

    /// ADR-7 矩阵的默认状态：Windows 全支持；macOS 与 Linux 整列待实现（Unsupported）。
    fn default_status(platform: Platform, cap: Capability) -> CapabilityStatus {
        match (platform, cap) {
            (Platform::Windows, _) => CapabilityStatus::Supported,
            _ => CapabilityStatus::Unsupported {
                reason: REASON_NOT_IMPLEMENTED,
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    /// 阶段标记不应为空。
    #[test]
    fn phase_not_empty() {
        assert!(!PHASE.is_empty());
    }

    /// Windows 全部能力完整支持。
    #[test]
    fn windows_all_supported() {
        let reg = CapabilityRegistry::for_platform(Platform::Windows);
        for cap in Capability::ALL {
            assert_eq!(reg.query(cap), CapabilityStatus::Supported);
        }
    }

    /// macOS 全部不可用且带原因。
    #[test]
    fn macos_all_unsupported_with_reason() {
        let reg = CapabilityRegistry::for_platform(Platform::MacOs);
        for cap in Capability::ALL {
            let s = reg.query(cap);
            assert!(!s.is_usable());
            assert_eq!(s.reason(), Some(REASON_NOT_IMPLEMENTED));
        }
    }

    /// Linux 整列待实现（ADR-7）：查询不 panic，全部不可用且带统一原因。
    #[test]
    fn linux_all_unsupported_without_panic() {
        let reg = CapabilityRegistry::for_platform(Platform::Linux);
        for cap in Capability::ALL {
            let s = reg.query(cap);
            assert!(!s.is_usable());
            assert_eq!(s.reason(), Some(REASON_NOT_IMPLEMENTED));
        }
    }

    /// 原因 key 必须是合法 Fluent id，且在 en-US / zh-CN 目录里都有定义。
    #[test]
    fn reason_keys_are_valid_fluent_ids_with_messages() {
        let catalogs = [
            include_str!("../../snow-i18n/locales/en-US/capability.ftl"),
            include_str!("../../snow-i18n/locales/zh-CN/capability.ftl"),
        ];
        for key in [REASON_NOT_IMPLEMENTED, REASON_WAYLAND_LIMITED] {
            assert!(key.chars().all(|c| c.is_ascii_lowercase() || c == '-'));
            for text in catalogs {
                assert!(text.lines().any(|l| l.starts_with(&format!("{key} = "))));
            }
        }
    }

    /// 覆盖状态后查询应返回新值。
    #[test]
    fn override_takes_effect() {
        let mut reg = CapabilityRegistry::for_platform(Platform::Windows);
        let s = CapabilityStatus::Unsupported { reason: "test" };
        reg.override_status(Capability::Tray, s);
        assert_eq!(reg.query(Capability::Tray), s);
    }

    /// ALL 覆盖 8 项且 id 唯一。
    #[test]
    fn all_ids_unique() {
        let ids: HashSet<_> = Capability::ALL.iter().map(|c| c.id()).collect();
        assert_eq!(ids.len(), 8);
    }

    /// 当前平台的注册表可构造且全能力可查询。
    #[test]
    fn current_platform_queryable() {
        let reg = CapabilityRegistry::for_current_platform();
        for cap in Capability::ALL {
            let _ = reg.query(cap);
        }
    }
}
