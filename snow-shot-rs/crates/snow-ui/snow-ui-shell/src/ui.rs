//! GPUI 门面与运行时：本 crate 里**唯一**对外暴露 gpui 类型的模块。
//!
//! 其余模块（几何、显示器、窗口规格、覆盖窗、热键、托盘）完全与 gpui 无关。
//! 上层视图 crate 通过 `snow_ui_shell::ui::*` 取得精选的 GPUI 子集，
//! 自身不依赖也不书写 `gpui::` 路径（守卫友好）。上游 API 变动只需改这里。

use crate::error::ShellError;
use crate::geometry::PhysicalRect;
use crate::monitor::Monitors;
use crate::native;
use crate::overlay::{NativeWindowId, OverlayWindow};
use crate::window::{ResolvedPlacement, WindowSpec};
use gpui_kit::{
    AnyWindowHandle, DisplayId, TitlebarOptions, WindowBackgroundAppearance, WindowBounds,
    WindowKind, WindowOptions,
};
use raw_window_handle::{HasWindowHandle, RawWindowHandle};
use crate::inbox::MainThreadInbox;
use snow_capability::CapabilityRegistry;

// ---- 精选 GPUI 子集（视图层使用）----
pub use gpui_kit::{
    Animation, AnimationExt,
    Anchor, AnyElement, App, AppContext, Bounds, ClickEvent, Context, CursorStyle, Div, Element,
    ElementId, ElementInputHandler, Entity, EntityInputHandler, FocusHandle, Focusable, FontWeight,
    PathBuilder,
    Hsla, ImageSource, InteractiveElement, IntoElement, KeyDownEvent, KeyUpEvent, MouseButton, MouseDownEvent,
    MouseMoveEvent, MouseUpEvent, ObjectFit, ParentElement, Pixels, Point, QuitMode, Render,
    RenderImage, RenderOnce, Rgba, SharedString, Size, StatefulInteractiveElement, Styled,
    StyledImage, Task, TextAlign, TextRun, UTF16Selection, UnderlineStyle, ViewElement, WeakEntity,
    Window, actions,
    DispatchPhase, ScrollDelta, ScrollStrategy, ScrollWheelEvent, ShapedLine, UniformListScrollHandle, canvas, component, div, hsla, img, point,
    px, rgb, rgba, size, uniform_list,
};
pub use gpui_kit::prelude::FluentBuilder;
/// 锚定弹层宿主（触发元素 + 延迟绘制的弹出内容，自带窗口边缘避让与遮挡）。
pub use gpui_kit::base::Popup;
/// 应用资源源与图标资源检查（antd / 自绘图标路径约定见 `assets` 模块）。
pub use crate::assets::{ANTD_ICON_PREFIX, AppAssets, OWN_ICON_PREFIX, icon_asset_exists};
/// 组件库的“取消”动作（Esc）：向焦点所在的下拉浮层派发即可关闭它。
pub use gpui_kit::base::actions::Cancel;

/// 用窗口当前文本样式（字体、字重）量出单行文字的宽度。
///
/// # 参数
/// - `window`：当前窗口（提供文本系统与默认文本样式）。
/// - `text`：不含换行的文字。
/// - `font_px`：字号（逻辑像素）。
///
/// # 返回
/// 文字宽度（逻辑像素）。
pub fn measure_text_width(window: &Window, text: &str, font_px: f32) -> f32 {
    let run = window.text_style().to_run(text.len());
    let shaped = window.text_system().shape_line(
        SharedString::from(text.to_string()),
        px(font_px),
        &[run],
        None,
    );
    f32::from(shaped.width)
}

/// 窗口当前文本样式的字体族名（宽度缓存按它失效）。
pub fn window_font_family(window: &Window) -> String {
    window.text_style().font_family.to_string()
}

/// 读取虚拟列表当前的纵向滚动偏移（逻辑像素，向下滚动为负）。
///
/// # 参数
/// - `handle`：已通过 `track_scroll` 绑定到列表的滚动句柄
///
/// # 返回
/// 纵向偏移；用于判断列表是否发生了滚动。
pub fn uniform_list_offset_y(handle: &UniformListScrollHandle) -> f32 {
    f32::from(handle.0.borrow().base_handle.offset().y)
}

pub use crate::selection::{
    DEFAULT_EDGE_TOLERANCE, DEFAULT_HANDLE_SIZE, DEFAULT_MINIMUM_SELECTION_SIZE,
    SelectionDragMode, SelectionState, bounded_selection_rect, dragged_selection_rect,
    handle_rects, hit_test_drag_mode, marquee_selection_rect, selection_size_label,
};
pub use crate::pinned_geometry::{
    PinnedDragHandle, ScaleAnchor, anchored_scale_rect, handle_rects as pinned_handle_rects,
    hit_test_handle as hit_test_pinned_handle, proportional_resize_rect, scaled_size,
    step_opacity, step_zoom,
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

    /// 读取窗口外框（屏幕物理像素）。
    ///
    /// # 返回
    /// 外框矩形；取不到原生句柄或窗口已销毁返回错误。
    pub fn rect(&self) -> Result<PhysicalRect, ShellError> {
        native::window_rect(self.native_hwnd()?)
    }

    /// 设置窗口外框（屏幕物理像素），不激活、不改 Z 序。
    ///
    /// 注意：会同步触发窗口尺寸 / 位置消息，**不能**在 GPUI 视图回调（App 被借用）里直接调用，
    /// 应放进 `cx.spawn` 的异步任务里。
    ///
    /// # 参数
    /// - `rect`：目标外框。
    pub fn set_rect(&self, rect: PhysicalRect) -> Result<(), ShellError> {
        native::set_window_rect(self.native_hwnd()?, rect)
    }

    /// 把窗口提到同一置顶层级的最上面，不抢焦点；同样不能在 App 被借用时直接调用。
    ///
    /// # 参数
    /// - `topmost`：窗口是否属于置顶层（决定提到哪一层的最上面，并同步更新置顶状态）。
    pub fn raise(&self, topmost: bool) -> Result<(), ShellError> {
        native::bring_to_top(self.native_hwnd()?, topmost)
    }

    /// 设置窗口原生标题栏的深浅色（不影响窗口内容）。
    ///
    /// # 参数
    /// - `dark`：`true` 深色标题栏，`false` 浅色。
    ///
    /// # 返回
    /// 取不到原生句柄或平台调用失败返回错误。
    ///
    /// ```ignore
    /// window.set_dark_title(true)?;
    /// ```
    pub fn set_dark_title(&self, dark: bool) -> Result<(), ShellError> {
        native::set_window_dark_title(self.native_hwnd()?, dark)
    }

    /// 显示或隐藏窗口（显示时不激活）；与 [`ShellWindow::set_rect`] 一样不能在 App 被借用时直接调用。
    ///
    /// # 参数
    /// - `visible`：`true` 显示，`false` 隐藏。
    pub fn set_visible(&self, visible: bool) -> Result<(), ShellError> {
        native::set_window_visible(self.native_hwnd()?, visible)
    }

    /// 设置窗口对输入透明：开启后鼠标点击穿过本窗口，窗口也不再抢焦点。
    ///
    /// # 参数
    /// - `transparent`：`true` 穿透，`false` 恢复。
    ///
    /// # 返回
    /// 取不到原生句柄或平台调用失败返回错误（失败时调用方应保持“未穿透”状态）。
    ///
    /// ```ignore
    /// window.set_input_transparent(true)?;
    /// ```
    pub fn set_input_transparent(&self, transparent: bool) -> Result<(), ShellError> {
        native::set_input_transparent(self.native_hwnd()?, transparent)
    }

    /// 设置整窗不透明度，用于让不支持逐像素透明的窗口也呈现半透明。
    ///
    /// # 参数
    /// - `alpha`：`0` 全透明 ~ `255` 不透明。
    ///
    /// # 返回
    /// 取不到原生句柄或平台调用失败返回错误。若同时要鼠标穿透，须在 [`Self::set_input_transparent`] 之后调用。
    ///
    /// ```ignore
    /// window.set_input_transparent(true)?;
    /// window.set_window_alpha(140)?;
    /// ```
    pub fn set_window_alpha(&self, alpha: u8) -> Result<(), ShellError> {
        native::set_window_alpha(self.native_hwnd()?, alpha)
    }

    /// 原生窗口句柄整数值；取不到返回 `Platform` 错误。
    fn native_hwnd(&self) -> Result<isize, ShellError> {
        self.native
            .map(|id| id.0)
            .ok_or_else(|| ShellError::Platform("无法获取原生窗口句柄".into()))
    }
}

/// 设置进程内弹出菜单（托盘右键菜单等）的深浅色，下次弹出时生效。
///
/// # 参数
/// - `dark`：`Some(true)` 深色、`Some(false)` 浅色、`None` 跟随系统。
///
/// # 返回
/// 平台不支持或调用失败返回错误。
///
/// ```ignore
/// set_popup_menu_dark(Some(true))?;
/// ```
pub fn set_popup_menu_dark(dark: Option<bool>) -> Result<(), ShellError> {
    native::set_popup_menu_dark(dark)
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

    /// 在 GPUI 主线程上消费收件箱：每个事件回调一次 `handler`，收件箱关闭后循环结束。
    ///
    /// 其它线程（热键、托盘、IPC）只需 `inbox.push(..)`，事件会在主线程上被分发，
    /// `handler` 内可安全创建窗口、退出应用。
    ///
    /// # 参数
    /// - `inbox`：事件收件箱（与生产者线程共享克隆）。
    /// - `handler`：主线程事件处理函数。
    ///
    /// ```no_run
    /// use snow_ui_shell::inbox::MainThreadInbox;
    /// let inbox = MainThreadInbox::new();
    /// let tx = inbox.clone();
    /// snow_ui_shell::ui::run_resident(move |cx| {
    ///     cx.run_inbox(inbox, |cx, ev: u32| if ev == 0 { cx.quit() });
    ///     tx.push(0);
    /// });
    /// ```
    pub fn run_inbox<T: 'static>(
        &mut self,
        inbox: MainThreadInbox<T>,
        mut handler: impl FnMut(&mut ShellContext, T) + 'static,
    ) {
        self.app
            .spawn(async move |cx| {
                while let Some(event) = inbox.recv().await {
                    cx.update(|app| handler(&mut ShellContext { app }, event));
                }
            })
            .detach();
    }

    /// 窗口是否仍然打开。
    ///
    /// # 参数
    /// - `window`：之前 `open_window` 返回的窗口句柄。
    pub fn is_window_open(&self, window: &ShellWindow) -> bool {
        let id = window.handle.window_id();
        self.app.windows().iter().any(|w| w.window_id() == id)
    }

    /// 把窗口带到前台并激活；窗口已关闭时静默忽略。
    ///
    /// # 参数
    /// - `window`：要激活的窗口。
    pub fn activate_window(&mut self, window: &ShellWindow) {
        let _ = window
            .handle
            .update(self.app, |_, window, _| window.activate_window());
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
            // 逻辑坐标经 gpui 换算可能有 1px 取整误差，需按物理像素精确落位。
            // 不能在此同步调用 SetWindowPos：它会同步触发 gpui 的 WM_SIZE / WM_MOVE 回调，
            // 而此刻 App 正处于可变借用中，gpui 会记 `RefCell already borrowed` 且收不到通知。
            // 因此推迟到本次 update 结束之后再落位。
            let rect = placed.rect;
            let popup = !spec.show_in_taskbar;
            let topmost = (spec.always_on_top != popup).then_some(spec.always_on_top);
            let borderless = !spec.decorations;
            let focus = spec.focus;
            self.app
                .spawn(async move |_cx| {
                    if let Err(e) = finalize_native_placement(id, rect, topmost, borderless, focus) {
                        tracing::warn!(error = %e, "窗口物理落位失败");
                    }
                })
                .detach();
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

/// 在 App 未被借用时把窗口精确落到物理矩形；已经吻合则不触碰窗口，避免多余的尺寸消息。
///
/// # 参数
/// - `id`：原生窗口句柄。
/// - `rect`：目标物理矩形。
/// - `topmost`：需要显式切换置顶时给出目标状态。
/// - `borderless`：为真时去掉系统边框样式，使客户区等于窗口矩形。
/// - `focus`：为真时强制抢到前台键盘焦点（后台 IPC / 热键触发的窗口默认拿不到前台）。
fn finalize_native_placement(
    id: NativeWindowId,
    rect: PhysicalRect,
    topmost: Option<bool>,
    borderless: bool,
    focus: bool,
) -> Result<(), ShellError> {
    if borderless {
        native::strip_window_frame(id.0)?;
    }
    if native::window_rect(id.0).ok() != Some(rect) {
        native::set_window_rect(id.0, rect)?;
    }
    if let Some(flag) = topmost {
        native::set_topmost(id.0, flag)?;
    }
    if focus && let Err(e) = native::force_foreground(id.0) {
        // 抢焦点失败不影响窗口显示，只是键盘可能暂时无响应，必须留痕便于排查
        tracing::warn!(error = %e, "窗口未能取得前台键盘焦点");
    }
    Ok(())
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

/// 内置图标资源源：gpui-component 控件按路径（icons/*.svg）加载 SVG，须在 Application 上注册。
///
/// 在 gpui-kit 默认包（104 个 Lucide 图标，约 48KB）之上叠加 antd 单色层与自绘图标，见 `assets` 模块。
pub(crate) fn application_with_assets() -> gpui_kit::Application {
    gpui_kit::application().with_assets(crate::assets::AppAssets)
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
    application_with_assets().run(move |app| {
        gpui_kit::init(app);
        setup(&mut ShellContext { app });
    });
}

/// 启动常驻型 GPUI 应用主循环（阻塞直到 `quit`）。
///
/// 与 [`run`] 的区别：退出模式为 [`QuitMode::Explicit`]，关闭所有窗口不会结束进程，
/// 只有显式调用 [`ShellContext::quit`] 才会退出（托盘常驻应用使用）。
///
/// # 参数
/// - `setup`：应用就绪后的回调，在此创建托盘、热键、收件箱循环。
///
/// ```no_run
/// snow_ui_shell::ui::run_resident(|cx| cx.quit());
/// ```
pub fn run_resident(setup: impl FnOnce(&mut ShellContext) + 'static) {
    native::ensure_dpi_awareness();
    application_with_assets().run(move |app| {
        gpui_kit::init(app);
        app.set_quit_mode(QuitMode::Explicit);
        setup(&mut ShellContext { app });
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    /// gpui-component 内部用到的图标路径（Checkbox 对勾、下拉箭头、清除按钮等）。
    const COMPONENT_ICON_PATHS: &[&str] = &[
        "icons/check.svg",
        "icons/close.svg",
        "icons/chevron-right.svg",
        "icons/chevron-left.svg",
        "icons/chevron-down.svg",
        "icons/chevron-up.svg",
        "icons/chevrons-up-down.svg",
        "icons/search.svg",
        "icons/plus.svg",
        "icons/minus.svg",
        "icons/loader.svg",
        "icons/calendar.svg",
        "icons/circle-x.svg",
        "icons/circle-check.svg",
        "icons/info.svg",
        "icons/triangle-alert.svg",
        "icons/eye.svg",
        "icons/eye-off.svg",
        "icons/ellipsis.svg",
        "icons/copy.svg",
        "icons/inbox.svg",
    ];

    /// 资源源能加载组件用到的每个图标，且内容是合法 SVG。
    #[test]
    fn assets_cover_component_icons() {
        use gpui_kit::AssetSource;
        let src = gpui_kit::assets::Assets;
        for path in COMPONENT_ICON_PATHS {
            let data = src
                .load(path)
                .unwrap_or_else(|e| panic!("{path} 加载失败: {e}"))
                .unwrap_or_else(|| panic!("{path} 为空"));
            assert!(data.starts_with(b"<svg"), "{path} 不是 svg");
        }
    }

    /// 未知或空路径不会返回内容；list 能列出 icons 前缀下的条目。
    #[test]
    fn assets_unknown_path_and_list() {
        use gpui_kit::AssetSource;
        let src = gpui_kit::assets::Assets;
        assert!(src.load("").unwrap().is_none());
        assert!(src.load("icons/__no_such_icon__.svg").map_or(true, |d| d.is_none()));
        let listed = src.list("icons/").unwrap();
        assert!(listed.iter().any(|p| p.as_ref() == "icons/check.svg"));
    }

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
