//! 覆盖窗：透明置顶窗口上的动态点击穿透（ADR-2b，`SetWindowRgn`）。
//!
//! 本模块不含 gpui 类型，只持有原生窗口句柄；句柄由 [`crate::ui`] 在创建 GPUI 窗口后取得。

use crate::error::{ShellError, require_capability};
use crate::geometry::{PhysicalPoint, PhysicalRect, Region};
use crate::native;
use snow_capability::{Capability, CapabilityRegistry};

/// 原生窗口句柄的整数形式（Windows 下为 `HWND`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct NativeWindowId(pub isize);

/// 读取鼠标当前的屏幕坐标（物理像素），供验证程序判定点击落点。
///
/// # 返回
/// 屏幕坐标；不支持的平台返回 `Unsupported`。
///
/// ```no_run
/// let p = snow_ui_shell::overlay::cursor_screen_position().unwrap();
/// println!("{}, {}", p.x, p.y);
/// ```
pub fn cursor_screen_position() -> Result<PhysicalPoint, ShellError> {
    native::cursor_pos()
}

/// 覆盖窗控制器：管理点击穿透区域与屏幕位置。
///
/// 区域之外的像素既不绘制也不接收点击（窗口形状与可点击形状始终一致）。
/// 区域用**窗口坐标**（见 [`crate::geometry`]）。选区变化时重新调用
/// [`OverlayWindow::set_hit_region`] 即可，无需每帧调用。
///
/// 请在创建窗口的 UI 线程上调用（`SetWindowRgn` 会向窗口同步发消息）。
#[derive(Debug)]
pub struct OverlayWindow {
    /// 原生句柄。
    id: NativeWindowId,
    /// 创建时校验点击穿透能力是否可用。
    caps_ok: Result<(), ShellError>,
    /// 最近一次成功设置的区域；`None` 表示整窗命中。
    region: Option<Region>,
}

impl OverlayWindow {
    /// 用原生句柄创建控制器，并按能力表判定点击穿透是否可用。
    ///
    /// # 参数
    /// - `id`：窗口原生句柄。
    /// - `caps`：能力注册表。
    ///
    /// # 返回
    /// 控制器。能力不可用时仍会返回，但后续 `set_hit_region` 返回 `Unsupported`（降级）。
    ///
    /// ```no_run
    /// use snow_capability::CapabilityRegistry;
    /// use snow_ui_shell::overlay::{NativeWindowId, OverlayWindow};
    /// let caps = CapabilityRegistry::for_current_platform();
    /// let overlay = OverlayWindow::from_native(NativeWindowId(0x1234), &caps);
    /// assert!(overlay.hit_region().is_none());
    /// ```
    pub fn from_native(id: NativeWindowId, caps: &CapabilityRegistry) -> Self {
        Self {
            id,
            caps_ok: require_capability(caps, Capability::OverlayClickThrough),
            region: None,
        }
    }

    /// 原生句柄。
    pub fn native_id(&self) -> NativeWindowId {
        self.id
    }

    /// 设置命中区域：区域内正常交互，区域外整体穿透。
    ///
    /// # 参数
    /// - `region`：窗口坐标系下的区域；空区域 = 整窗不可见且全部穿透。
    ///
    /// # 返回
    /// 成功 `Ok(())`；能力不可用返回 `Unsupported`，系统调用失败返回 `Platform`。
    ///
    /// ```no_run
    /// # use snow_capability::CapabilityRegistry;
    /// # use snow_ui_shell::overlay::{NativeWindowId, OverlayWindow};
    /// use snow_ui_shell::geometry::{PhysicalRect, Region};
    /// # let mut overlay = OverlayWindow::from_native(NativeWindowId(1), &CapabilityRegistry::for_current_platform());
    /// let selection = Region::from_rect(PhysicalRect::new(100, 100, 400, 300));
    /// overlay.set_hit_region(&selection).ok();
    /// ```
    pub fn set_hit_region(&mut self, region: &Region) -> Result<(), ShellError> {
        self.caps_ok.clone()?;
        native::set_window_region(self.id.0, Some(region))?;
        self.region = Some(region.clone());
        Ok(())
    }

    /// 清除命中区域，恢复整窗命中。
    pub fn clear_hit_region(&mut self) -> Result<(), ShellError> {
        self.caps_ok.clone()?;
        native::set_window_region(self.id.0, None)?;
        self.region = None;
        Ok(())
    }

    /// 最近一次成功设置的区域。
    pub fn hit_region(&self) -> Option<&Region> {
        self.region.as_ref()
    }

    /// 按已设置的区域判定窗口坐标点是否命中（未设置区域时视为命中）。
    ///
    /// ```no_run
    /// # use snow_capability::CapabilityRegistry;
    /// # use snow_ui_shell::overlay::{NativeWindowId, OverlayWindow};
    /// use snow_ui_shell::geometry::PhysicalPoint;
    /// # let overlay = OverlayWindow::from_native(NativeWindowId(1), &CapabilityRegistry::for_current_platform());
    /// assert!(overlay.hit_test(PhysicalPoint::new(0, 0)));
    /// ```
    pub fn hit_test(&self, p: PhysicalPoint) -> bool {
        self.region.as_ref().is_none_or(|r| r.contains(p))
    }

    /// 读取窗口外框矩形（屏幕坐标，物理像素）。
    pub fn screen_rect(&self) -> Result<PhysicalRect, ShellError> {
        native::window_rect(self.id.0)
    }

    /// 设置窗口外框矩形（屏幕坐标，物理像素）；不激活窗口。
    pub fn set_screen_rect(&self, rect: PhysicalRect) -> Result<(), ShellError> {
        if rect.is_empty() {
            return Err(ShellError::InvalidArgument("窗口矩形为空".into()));
        }
        native::set_window_rect(self.id.0, rect)
    }

    /// 切换置顶。
    pub fn set_always_on_top(&self, on_top: bool) -> Result<(), ShellError> {
        native::set_topmost(self.id.0, on_top)
    }

    /// 让窗口不出现在屏幕录制 / 截图里（录制区域窗自身不能进成片）。
    ///
    /// # 参数
    /// - `excluded`：`true` 排除，`false` 恢复。
    ///
    /// # 返回
    /// 成功 `Ok(())`；平台不支持或系统调用失败返回错误（调用方应记日志，窗口仍可用）。
    ///
    /// ```no_run
    /// # use snow_capability::CapabilityRegistry;
    /// # use snow_ui_shell::overlay::{NativeWindowId, OverlayWindow};
    /// # let overlay = OverlayWindow::from_native(NativeWindowId(1), &CapabilityRegistry::for_current_platform());
    /// overlay.set_capture_excluded(true).ok();
    /// ```
    pub fn set_capture_excluded(&self, excluded: bool) -> Result<(), ShellError> {
        native::set_capture_excluded(self.id.0, excluded)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use snow_capability::Platform;

    /// 能力不可用时设置区域直接降级为错误，不碰系统。
    #[test]
    fn unsupported_platform_degrades() {
        let caps = CapabilityRegistry::for_platform(Platform::MacOs);
        let mut w = OverlayWindow::from_native(NativeWindowId(0), &caps);
        let r = Region::from_rect(PhysicalRect::new(0, 0, 10, 10));
        assert!(matches!(
            w.set_hit_region(&r),
            Err(ShellError::Unsupported { .. })
        ));
        assert!(w.hit_region().is_none());
    }

    /// 无效句柄返回错误而不是 panic，且不污染已记录区域。
    #[test]
    fn invalid_handle_is_error_not_panic() {
        let caps = CapabilityRegistry::for_platform(Platform::Windows);
        let mut w = OverlayWindow::from_native(NativeWindowId(0), &caps);
        let r = Region::from_rect(PhysicalRect::new(0, 0, 10, 10));
        assert!(w.set_hit_region(&r).is_err());
        assert!(w.hit_region().is_none());
    }

    /// 命中判定：未设置区域全命中，设置后只命中区域内。
    #[test]
    fn hit_test_follows_region() {
        let caps = CapabilityRegistry::for_platform(Platform::Windows);
        let mut w = OverlayWindow::from_native(NativeWindowId(0), &caps);
        assert!(w.hit_test(PhysicalPoint::new(5, 5)));
        w.region = Some(Region::from_rect(PhysicalRect::new(0, 0, 10, 10)));
        assert!(w.hit_test(PhysicalPoint::new(5, 5)));
        assert!(!w.hit_test(PhysicalPoint::new(50, 50)));
    }

    /// 空矩形不下发系统调用。
    #[test]
    fn empty_rect_rejected() {
        let caps = CapabilityRegistry::for_platform(Platform::Windows);
        let w = OverlayWindow::from_native(NativeWindowId(0), &caps);
        assert!(matches!(
            w.set_screen_rect(PhysicalRect::new(0, 0, 0, 0)),
            Err(ShellError::InvalidArgument(_))
        ));
    }
}
