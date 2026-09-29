//! GPUI 门面与运行时：本 crate 里**唯一**对外暴露 gpui 类型的模块。
//!
//! 其余模块（几何、显示器、窗口规格、覆盖窗、热键、托盘）完全与 gpui 无关。
//! 上层视图 crate 通过 `snow_ui_shell::ui::*` 取得精选的 GPUI 子集，
//! 自身不依赖也不书写 `gpui::` 路径（守卫友好）。上游 API 变动只需改这里。

use crate::error::ShellError;
use crate::monitor::Monitors;
use crate::native;
use crate::overlay::{NativeWindowId, OverlayWindow};
use crate::window::{ResolvedPlacement, WindowSpec};
use gpui_kit::{
    AnyWindowHandle, DisplayId, TitlebarOptions, WindowBackgroundAppearance, WindowBounds,
    WindowKind, WindowOptions,
};
use raw_window_handle::{HasWindowHandle, RawWindowHandle};
use snow_capability::CapabilityRegistry;

// ---- 精选 GPUI 子集（视图层使用）----
pub use gpui_kit::{
    AnyElement, App, AppContext, Bounds, ClickEvent, Context, CursorStyle, ElementId,
    ElementInputHandler, Entity, EntityInputHandler, FocusHandle, Focusable, FontWeight, Hsla,
    InteractiveElement, IntoElement, MouseButton, ParentElement, Pixels, Point, Render, RenderOnce,
    Rgba, SharedString, Size, StatefulInteractiveElement, Styled, TextAlign, TextRun,
    UTF16Selection, UnderlineStyle, Window, actions, component, div, hsla, point, px, rgb, rgba,
    size,
};

/// 已打开的窗口句柄（不暴露 gpui 类型的部分见各方法）。
#[derive(Debug, Clone, Copy)]
pub struct ShellWindow {
    /// GPUI 窗口句柄。
    handle: AnyWindowHandle,
    /// 原生窗口句柄；取不到时为 `None`。
    native: Option<NativeWindowId>,
}

impl ShellWindow {
    /// 原生窗口句柄（Windows 为 `HWND`）。
    pub fn native_id(&self) -> Option<NativeWindowId> {
        self.native
    }

    /// 取覆盖窗控制器，用于设置点击穿透区域。
    ///
    /// # 参数
    /// - `caps`：能力表。
    ///
    /// # 返回
    /// 控制器；拿不到原生句柄时返回 `Platform` 错误。
    pub fn overlay(&self, caps: &CapabilityRegistry) -> Result<OverlayWindow, ShellError> {
        let id = self
            .native
            .ok_or_else(|| ShellError::Platform("无法获取原生窗口句柄".into()))?;
        Ok(OverlayWindow::from_native(id, caps))
    }

    /// 关闭窗口；窗口已关闭时静默忽略。
    pub fn close(&self, cx: &mut App) {
        let _ = self
            .handle
            .update(cx, |_, window, _| window.remove_window());
    }

    /// GPUI 窗口句柄（仅供视图层内部使用）。
    pub fn gpui_handle(&self) -> AnyWindowHandle {
        self.handle
    }
}

/// 启动期上下文：在 [`run`] 的回调里创建窗口、启动服务。
pub struct ShellContext<'a> {
    /// GPUI 应用上下文。
    app: &'a mut App,
}

impl ShellContext<'_> {
    /// 底层 GPUI 应用上下文（视图层需要 `cx` 时使用）。
    pub fn app(&mut self) -> &mut App {
        self.app
    }

    /// 枚举显示器快照。
    pub fn monitors(&self) -> Result<Monitors, ShellError> {
        Monitors::enumerate()
    }

    /// 退出应用主循环。
    pub fn quit(&mut self) {
        self.app.quit();
    }

    /// 延时退出（验证程序的兜底保护，防止遗留进程）。
    ///
    /// # 参数
    /// - `delay`：多久之后调用退出。
    pub fn quit_after(&mut self, delay: std::time::Duration) {
        self.app
            .spawn(async move |cx| {
                cx.background_executor().timer(delay).await;
                cx.update(|app| app.quit());
            })
            .detach();
    }

    /// 最后一个窗口关闭时自动退出应用。
    pub fn quit_on_last_window_closed(&mut self) {
        self.app
            .on_window_closed(|app, _| {
                if app.windows().is_empty() {
                    app.quit();
                }
            })
            .detach();
    }

    /// 按 [`WindowSpec`] 创建窗口，并把物理位置精确落到目标矩形。
    ///
    /// # 参数
    /// - `spec`：窗口规格。
    /// - `build`：构造根视图，`Window`/`App` 可用于创建 `Entity`。
    ///
    /// # 返回
    /// 窗口句柄与根视图实体；显示器解析失败或建窗失败返回 [`ShellError`]。
    ///
    /// ```no_run
    /// use snow_ui_shell::ui::{self, AppContext, Context, IntoElement, ParentElement, Render, Window, div};
    /// use snow_ui_shell::monitor::MonitorTarget;
    /// use snow_ui_shell::window::WindowSpec;
    /// struct Hello;
    /// impl Render for Hello {
    ///     fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
    ///         div().child("hello")
    ///     }
    /// }
    /// ui::run(|cx| {
    ///     let spec = WindowSpec::overlay(MonitorTarget::Primary);
    ///     let _ = cx.open_window(&spec, |_w, app| app.new(|_| Hello));
    /// });
    /// ```
    pub fn open_window<V: Render + 'static>(
        &mut self,
        spec: &WindowSpec,
        build: impl FnOnce(&mut Window, &mut App) -> Entity<V>,
    ) -> Result<(ShellWindow, Entity<V>), ShellError> {
        let monitors = Monitors::enumerate()?;
        let placed = spec.resolve(&monitors)?;
        let options = window_options(spec, &placed);
        let mut native_id = None;
        let (handle, entity) = gpui_kit::open_window(options, self.app, |window, app| {
            native_id = native_id_of(window);
            build(window, app)
        })
        .map_err(|e| ShellError::Platform(format!("创建窗口失败: {e}")))?;
        if let Some(id) = native_id {
            // 逻辑坐标经 gpui 换算可能有 1px 取整误差，这里按物理像素精确落位
            native::set_window_rect(id.0, placed.rect)?;
            let popup = !spec.show_in_taskbar;
            if spec.always_on_top != popup {
                native::set_topmost(id.0, spec.always_on_top)?;
            }
        }
        Ok((
            ShellWindow {
                handle,
                native: native_id,
            },
            entity,
        ))
    }
}

/// 由窗口规格与落点生成 GPUI 窗口选项。
///
/// 逻辑坐标与 GPUI 在 Windows 上的约定一致：原点 = 物理坐标 / 目标显示器缩放比。
fn window_options(spec: &WindowSpec, placed: &ResolvedPlacement) -> WindowOptions {
    let s = placed.monitor.scale.value();
    let r = placed.rect;
    let bounds = Bounds {
        origin: point(px(r.x as f32 / s), px(r.y as f32 / s)),
        size: size(px(r.width as f32 / s), px(r.height as f32 / s)),
    };
    let titlebar = spec.decorations.then(|| TitlebarOptions {
        title: Some(spec.title.clone().into()),
        appears_transparent: false,
        traffic_light_position: None,
    });
    WindowOptions {
        window_bounds: Some(WindowBounds::Windowed(bounds)),
        titlebar,
        focus: spec.focus,
        show: true,
        // PopUp = 工具窗口 + 置顶，天然不进任务栏（gpui-pre-windows）
        kind: if spec.show_in_taskbar {
            WindowKind::Normal
        } else {
            WindowKind::PopUp
        },
        is_movable: spec.decorations,
        is_resizable: spec.resizable,
        is_minimizable: spec.decorations,
        display_id: Some(DisplayId::new(placed.monitor.id.0)),
        window_background: if spec.transparent {
            WindowBackgroundAppearance::Transparent
        } else {
            WindowBackgroundAppearance::Opaque
        },
        ..Default::default()
    }
}

/// 从 GPUI 窗口取原生句柄。
fn native_id_of(window: &Window) -> Option<NativeWindowId> {
    // `Window` 有同名固有方法（返回 gpui 句柄），须用完全限定语法调用 trait 方法
    match HasWindowHandle::window_handle(window).ok()?.as_raw() {
        RawWindowHandle::Win32(h) => Some(NativeWindowId(h.hwnd.get())),
        _ => None,
    }
}

/// 启动 GPUI 应用主循环（阻塞直到应用退出）。
///
/// 会先把进程设为 Per-Monitor-V2 DPI 感知（已设置则忽略），再初始化 GPUI 与组件库。
///
/// # 参数
/// - `setup`：应用就绪后的回调，在此创建窗口、托盘、热键。
///
/// ```no_run
/// snow_ui_shell::ui::run(|cx| {
///     let monitors = cx.monitors().unwrap();
///     println!("{} 块显示器", monitors.all().len());
///     cx.quit();
/// });
/// ```
pub fn run(setup: impl FnOnce(&mut ShellContext) + 'static) {
    native::ensure_dpi_awareness();
    gpui_kit::application().run(move |app| {
        gpui_kit::init(app);
        setup(&mut ShellContext { app });
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::geometry::{PhysicalRect, ScaleFactor};
    use crate::monitor::{MonitorId, MonitorInfo, MonitorTarget};

    /// 覆盖窗规格映射为 PopUp + 透明 + 无标题栏，且显示器 id 透传。
    #[test]
    fn overlay_options_mapping() {
        let mon = MonitorInfo {
            id: MonitorId(77),
            name: String::new(),
            bounds: PhysicalRect::new(-1920, 0, 1920, 1080),
            work_area: PhysicalRect::new(-1920, 0, 1920, 1040),
            scale: ScaleFactor::new(1.5),
            is_primary: false,
        };
        let spec = WindowSpec::overlay(MonitorTarget::Id(MonitorId(77)));
        let placed = ResolvedPlacement {
            rect: mon.bounds,
            monitor: mon,
        };
        let o = window_options(&spec, &placed);
        assert_eq!(o.kind, WindowKind::PopUp);
        assert!(o.titlebar.is_none() && !o.focus);
        assert_eq!(o.window_background, WindowBackgroundAppearance::Transparent);
        assert_eq!(o.display_id.map(u64::from), Some(77));
        let Some(WindowBounds::Windowed(b)) = o.window_bounds else {
            panic!("应为窗口化边界");
        };
        assert_eq!(b.origin.x.as_f32(), -1280.0);
        assert_eq!(b.size.width.as_f32(), 1280.0);
    }

    /// 普通窗口映射为 Normal + 标题栏。
    #[test]
    fn normal_options_mapping() {
        let mon = MonitorInfo {
            id: MonitorId(1),
            name: String::new(),
            bounds: PhysicalRect::new(0, 0, 1000, 800),
            work_area: PhysicalRect::new(0, 0, 1000, 800),
            scale: ScaleFactor::ONE,
            is_primary: true,
        };
        let spec = WindowSpec::normal("设置", crate::geometry::LogicalSize::new(400.0, 300.0));
        let placed = ResolvedPlacement {
            rect: PhysicalRect::new(300, 250, 400, 300),
            monitor: mon,
        };
        let o = window_options(&spec, &placed);
        assert_eq!(o.kind, WindowKind::Normal);
        assert_eq!(
            o.titlebar.and_then(|t| t.title).map(|t| t.to_string()),
            Some("设置".to_string())
        );
        assert_eq!(o.window_background, WindowBackgroundAppearance::Opaque);
    }
}
