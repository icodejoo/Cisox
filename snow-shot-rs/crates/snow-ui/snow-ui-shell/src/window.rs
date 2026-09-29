//! 与 gpui 无关的窗口描述：`WindowSpec` 与位置解析。

use crate::error::ShellError;
use crate::geometry::{LogicalSize, PhysicalRect};
use crate::monitor::{MonitorInfo, MonitorTarget, Monitors};

/// 窗口位置的描述方式。
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Placement {
    /// 屏幕坐标下的物理像素矩形（覆盖窗精确铺屏用）。
    Physical(PhysicalRect),
    /// 铺满整块显示器（含任务栏区域）。
    FillMonitor(MonitorTarget),
    /// 在显示器工作区居中，尺寸为逻辑像素（按该显示器 DPI 换算）。
    Centered {
        /// 目标显示器。
        monitor: MonitorTarget,
        /// 逻辑尺寸。
        size: LogicalSize,
    },
}

/// 窗口规格：描述“要什么样的窗口”，不涉及任何 gpui 类型。
#[derive(Debug, Clone, PartialEq)]
pub struct WindowSpec {
    /// 窗口标题（无边框窗口仅用于系统识别）。
    pub title: String,
    /// 位置与尺寸。
    pub placement: Placement,
    /// 背景是否透明（DWM 级透明）。
    pub transparent: bool,
    /// 是否置顶。
    pub always_on_top: bool,
    /// 是否保留系统边框与标题栏。
    pub decorations: bool,
    /// 是否在任务栏显示。
    pub show_in_taskbar: bool,
    /// 创建时是否抢占焦点。
    pub focus: bool,
    /// 是否允许用户调整大小。
    pub resizable: bool,
}

/// 解析后的落点：具体显示器与物理像素矩形。
#[derive(Debug, Clone, PartialEq)]
pub struct ResolvedPlacement {
    /// 窗口落在的显示器。
    pub monitor: MonitorInfo,
    /// 屏幕坐标下的物理矩形。
    pub rect: PhysicalRect,
}

impl WindowSpec {
    /// 覆盖窗预设：铺满指定显示器、透明、置顶、无边框、不进任务栏、不抢焦点。
    ///
    /// ```rust
    /// use snow_ui_shell::monitor::MonitorTarget;
    /// use snow_ui_shell::window::WindowSpec;
    /// let spec = WindowSpec::overlay(MonitorTarget::Primary);
    /// assert!(spec.transparent && spec.always_on_top && !spec.show_in_taskbar);
    /// ```
    pub fn overlay(monitor: MonitorTarget) -> Self {
        Self {
            title: String::new(),
            placement: Placement::FillMonitor(monitor),
            transparent: true,
            always_on_top: true,
            decorations: false,
            show_in_taskbar: false,
            focus: false,
            resizable: false,
        }
    }

    /// 普通带边框窗口预设：主屏工作区居中、不透明、进任务栏、可调整大小。
    ///
    /// # 参数
    /// - `title`：窗口标题。
    /// - `size`：逻辑尺寸。
    ///
    /// ```rust
    /// use snow_ui_shell::geometry::LogicalSize;
    /// use snow_ui_shell::window::WindowSpec;
    /// let spec = WindowSpec::normal("设置", LogicalSize::new(800.0, 600.0));
    /// assert!(spec.decorations && spec.show_in_taskbar);
    /// ```
    pub fn normal(title: impl Into<String>, size: LogicalSize) -> Self {
        Self {
            title: title.into(),
            placement: Placement::Centered {
                monitor: MonitorTarget::Primary,
                size,
            },
            transparent: false,
            always_on_top: false,
            decorations: true,
            show_in_taskbar: true,
            focus: true,
            resizable: true,
        }
    }

    /// 校验选项组合是否可实现。
    ///
    /// 不进任务栏的窗口在 Windows 上是无系统边框的工具窗口，因此不能同时要求系统边框。
    ///
    /// # 返回
    /// 合法返回 `Ok(())`，否则 [`ShellError::InvalidArgument`]。
    ///
    /// ```rust
    /// use snow_ui_shell::monitor::MonitorTarget;
    /// use snow_ui_shell::window::WindowSpec;
    /// let mut spec = WindowSpec::overlay(MonitorTarget::Primary);
    /// assert!(spec.validate().is_ok());
    /// spec.decorations = true;
    /// assert!(spec.validate().is_err());
    /// ```
    pub fn validate(&self) -> Result<(), ShellError> {
        if self.decorations && !self.show_in_taskbar {
            return Err(ShellError::InvalidArgument(
                "带系统边框的窗口必须显示在任务栏".into(),
            ));
        }
        Ok(())
    }

    /// 把位置描述解析为具体显示器与物理矩形。
    ///
    /// # 参数
    /// - `monitors`：显示器快照。
    ///
    /// # 返回
    /// 落点；矩形为空、逻辑尺寸非正、找不到显示器时返回 [`ShellError::InvalidArgument`]。
    ///
    /// ```rust
    /// use snow_ui_shell::geometry::{PhysicalRect, ScaleFactor};
    /// use snow_ui_shell::monitor::{MonitorId, MonitorInfo, MonitorTarget, Monitors};
    /// use snow_ui_shell::window::WindowSpec;
    /// let r = PhysicalRect::new(0, 0, 1920, 1080);
    /// let ms = Monitors::from_list(vec![MonitorInfo {
    ///     id: MonitorId(1), name: String::new(), bounds: r, work_area: r,
    ///     scale: ScaleFactor::ONE, is_primary: true,
    /// }]);
    /// let placed = WindowSpec::overlay(MonitorTarget::Primary).resolve(&ms).unwrap();
    /// assert_eq!(placed.rect, r);
    /// ```
    pub fn resolve(&self, monitors: &Monitors) -> Result<ResolvedPlacement, ShellError> {
        self.validate()?;
        match self.placement {
            Placement::Physical(rect) => {
                if rect.is_empty() {
                    return Err(ShellError::InvalidArgument("窗口矩形为空".into()));
                }
                let monitor = monitors
                    .best_for_rect(rect)
                    .ok_or_else(|| ShellError::InvalidArgument("系统没有显示器".into()))?;
                Ok(ResolvedPlacement {
                    monitor: monitor.clone(),
                    rect,
                })
            }
            Placement::FillMonitor(target) => {
                let monitor = monitors.resolve(target)?;
                Ok(ResolvedPlacement {
                    monitor: monitor.clone(),
                    rect: monitor.bounds,
                })
            }
            Placement::Centered { monitor, size } => {
                if !(size.width > 0.0 && size.height > 0.0) {
                    return Err(ShellError::InvalidArgument("逻辑尺寸必须为正".into()));
                }
                let mon = monitors.resolve(monitor)?;
                let area = mon.work_area;
                let w = mon.scale.to_physical(size.width).min(area.width);
                let h = mon.scale.to_physical(size.height).min(area.height);
                let rect = PhysicalRect::new(
                    area.x + (area.width - w) / 2,
                    area.y + (area.height - h) / 2,
                    w,
                    h,
                );
                Ok(ResolvedPlacement {
                    monitor: mon.clone(),
                    rect,
                })
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::monitor::{MonitorId, tests::two_monitors};

    /// 覆盖窗铺满指定副屏，矩形等于显示器物理范围（含负坐标）。
    #[test]
    fn overlay_fills_secondary_monitor() {
        let ms = two_monitors();
        let spec = WindowSpec::overlay(MonitorTarget::Id(MonitorId(2)));
        let placed = spec.resolve(&ms).unwrap();
        assert_eq!(placed.rect, PhysicalRect::new(-1920, 100, 1920, 1080));
        assert_eq!(placed.monitor.id, MonitorId(2));
    }

    /// 居中窗口按目标显示器 DPI 换算尺寸并落在工作区中央。
    #[test]
    fn centered_uses_monitor_scale() {
        let ms = two_monitors();
        let spec = WindowSpec::normal("t", LogicalSize::new(800.0, 600.0));
        let placed = spec.resolve(&ms).unwrap();
        assert_eq!((placed.rect.width, placed.rect.height), (1200, 900));
        let area = placed.monitor.work_area;
        assert_eq!(placed.rect.x, area.x + (area.width - 1200) / 2);
        assert!(area.intersect(&placed.rect) == Some(placed.rect));
    }

    /// 居中窗口大于工作区时被夹到工作区大小。
    #[test]
    fn centered_clamped_to_work_area() {
        let ms = two_monitors();
        let spec = WindowSpec::normal("t", LogicalSize::new(99999.0, 99999.0));
        let placed = spec.resolve(&ms).unwrap();
        assert_eq!(placed.rect, placed.monitor.work_area);
    }

    /// 带边框却不进任务栏的组合被拒绝。
    #[test]
    fn decorations_require_taskbar() {
        let mut spec = WindowSpec::normal("t", LogicalSize::new(10.0, 10.0));
        assert!(spec.validate().is_ok());
        spec.show_in_taskbar = false;
        assert!(spec.resolve(&two_monitors()).is_err());
    }

    /// 物理矩形按重叠最多的显示器归属；空矩形/非法尺寸/无显示器报错。
    #[test]
    fn physical_and_errors() {
        let ms = two_monitors();
        let mut spec = WindowSpec::overlay(MonitorTarget::Primary);
        spec.placement = Placement::Physical(PhysicalRect::new(-1000, 200, 500, 500));
        assert_eq!(spec.resolve(&ms).unwrap().monitor.id, MonitorId(2));
        spec.placement = Placement::Physical(PhysicalRect::new(0, 0, 0, 10));
        assert!(spec.resolve(&ms).is_err());
        spec.placement = Placement::Centered {
            monitor: MonitorTarget::Primary,
            size: LogicalSize::new(0.0, 5.0),
        };
        assert!(spec.resolve(&ms).is_err());
        assert!(spec.resolve(&Monitors::default()).is_err());
    }
}
