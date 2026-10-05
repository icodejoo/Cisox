//! 贴图管理器：贴图窗口的创建、启动恢复、容量淘汰与生命周期登记。
//!
//! 每张贴图是一个独立的置顶无边框窗口（`PinnedWindowView`），持久化经 `PinShared` 的
//! `PinnedStore`（崩溃安全提交）。窗口关闭即从存储移除；进程退出不动存储，下次启动自动恢复。

use crate::capture_flow::pick_monitor;
use crate::pinned_model::{
    PinGeometry, flatten_alpha_on_white, initial_clipboard_rect, visible_rect,
};
use crate::pinned_shared::PinShared;
use crate::pinned_view::{PinInit, PinOp, PinnedWindowView, frame_from_rgba};
use crate::screenshot_output::encode_png;
use crate::settings_state::SharedConfig;
use snow_history::timeutil::now_utc_ms;
use snow_platform::clipboard::read_image_from_clipboard;
use snow_ui::shell::geometry::PhysicalRect;
use snow_ui::shell::monitor::Monitors;
use snow_ui::shell::overlay::cursor_screen_position;
use snow_ui::shell::window::{Placement, WindowSpec};
use snow_ui::ui::{Entity, ShellContext, ShellWindow};
use std::collections::BTreeMap;
use std::path::Path;
use std::rc::Rc;
use std::time::Duration;

/// 贴图脚本自动化环境变量：值为 JSON 操作数组文件路径，对新创建的贴图逐步执行（验收用）。
pub const ENV_PIN_AUTOTEST: &str = "SNOW_PIN_AUTOTEST";
/// 贴图窗口标题（无边框，仅供系统识别）。
const PIN_WINDOW_TITLE: &str = "Cisox Pin";
/// 自动化脚本首步之前等待窗口稳定的时间。
const AUTOTEST_START_DELAY: Duration = Duration::from_millis(1500);
/// 自动化脚本两步之间的间隔。
const AUTOTEST_STEP_INTERVAL: Duration = Duration::from_millis(600);

/// 已打开的一张贴图窗口。
struct PinWindow {
    /// 原生窗口句柄。
    window: ShellWindow,
    /// 视图实体。
    view: Entity<PinnedWindowView>,
}

/// 贴图窗口管理器。
pub struct PinnedManager {
    /// 共享上下文（仓储 / 配置）。
    shared: Rc<PinShared>,
    /// 当前打开的贴图窗口（键为贴图 ID）。
    windows: BTreeMap<String, PinWindow>,
}

/// 显示器物理范围列表，主显示器排在最前（供 [`visible_rect`] 使用）。
///
/// # 参数
/// - `monitors`：显示器快照。
///
/// ```ignore
/// let rects = monitor_rects(&cx.monitors()?);
/// ```
pub fn monitor_rects(monitors: &Monitors) -> Vec<PhysicalRect> {
    let mut list: Vec<_> = monitors.all().iter().collect();
    list.sort_by_key(|m| !m.is_primary);
    list.into_iter().map(|m| m.bounds).collect()
}

/// 解析自动化脚本 JSON（操作数组）。
///
/// # 参数
/// - `text`：JSON 文本。
///
/// # 返回
/// 操作列表；格式不对返回错误说明。
///
/// ```ignore
/// let ops = parse_pin_ops(r#"[{"op":"close"}]"#)?;
/// assert_eq!(ops.len(), 1);
/// ```
pub fn parse_pin_ops(text: &str) -> Result<Vec<PinOp>, String> {
    // Windows 下常见的 UTF-8 BOM 会让 serde_json 报错，先去掉
    serde_json::from_str(text.trim_start_matches('\u{feff}'))
        .map_err(|e| format!("贴图自动化脚本无效: {e}"))
}

impl PinnedManager {
    /// 打开数据根下的贴图仓储并创建管理器。
    ///
    /// # 参数
    /// - `data_root`：数据根目录。
    /// - `config`：共享配置。
    /// - `on_closed`：某张贴图窗口关闭后的通知（在主线程调用，参数为贴图 ID）。
    ///
    /// ```ignore
    /// let mgr = PinnedManager::new(&data_root, config, Box::new(|id| println!("{id}")));
    /// assert_eq!(mgr.pin_count(), 0);
    /// ```
    pub fn new(data_root: &Path, config: SharedConfig, on_closed: Box<dyn Fn(&str)>) -> Self {
        Self {
            shared: PinShared::open(data_root, config, on_closed),
            windows: BTreeMap::new(),
        }
    }

    /// 当前打开的贴图窗口数。
    pub fn pin_count(&self) -> usize {
        self.windows.len()
    }

    /// 共享上下文（测试与探针用）。
    pub fn shared(&self) -> &Rc<PinShared> {
        &self.shared
    }

    /// 窗口已关闭：回收对应的窗口句柄与视图实体。
    ///
    /// # 参数
    /// - `id`：贴图 ID。
    pub fn forget(&mut self, id: &str) {
        if self.windows.remove(id).is_some() {
            tracing::debug!(id, remaining = self.windows.len(), "贴图窗口已回收");
        }
    }

    /// 由选区像素创建贴图，并在给定屏幕位置原位打开。
    ///
    /// # 参数
    /// - `cx`：外壳上下文。
    /// - `width` / `height`：图像尺寸。
    /// - `rgba`：不透明 RGBA 像素（含标注合成结果）。
    /// - `rect`：窗口外框（屏幕物理像素）。
    ///
    /// # 返回
    /// 新贴图 ID；像素非法、建窗失败返回错误说明。
    ///
    /// ```ignore
    /// let id = manager.create_from_rgba(cx, 200, 100, rgba, PhysicalRect::new(50, 50, 200, 100))?;
    /// ```
    pub fn create_from_rgba(
        &mut self,
        cx: &mut ShellContext,
        width: u32,
        height: u32,
        rgba: Vec<u8>,
        rect: PhysicalRect,
    ) -> Result<String, String> {
        let zoom = rect.width as f32 / width.max(1) as f32;
        let geometry = PinGeometry::new(rect, zoom, 1.0, true);
        self.create(cx, width, height, rgba, geometry)
    }

    /// 把剪贴板里的图像贴到光标所在显示器的中央。
    ///
    /// # 返回
    /// 新贴图 ID；剪贴板没有图像 / 读取失败 / 建窗失败返回错误说明。
    pub fn create_from_clipboard(&mut self, cx: &mut ShellContext) -> Result<String, String> {
        let (width, height, mut rgba) =
            read_image_from_clipboard()?.ok_or_else(|| "剪贴板里没有图像".to_string())?;
        flatten_alpha_on_white(&mut rgba);
        let monitors = cx.monitors().map_err(|e| e.to_string())?;
        let cursor = cursor_screen_position().ok();
        let work_area = pick_monitor(&monitors, cursor)
            .map(|m| m.work_area)
            .ok_or_else(|| "系统没有可用显示器".to_string())?;
        let (rect, zoom) = initial_clipboard_rect(width, height, work_area);
        let geometry = PinGeometry::new(rect, zoom, 1.0, true);
        self.create(cx, width, height, rgba, geometry)
    }

    /// 把一张已有图像（如截图历史里的）贴到光标所在显示器的中央。
    ///
    /// # 参数
    /// - `cx`：外壳上下文。
    /// - `width` / `height`：图像尺寸。
    /// - `rgba`：RGBA 像素（透明部分会铺白底）。
    ///
    /// # 返回
    /// 新贴图 ID；取不到显示器或建窗失败返回错误说明。
    ///
    /// ```ignore
    /// let id = manager.create_from_image(cx, 200, 100, rgba)?;
    /// ```
    pub fn create_from_image(
        &mut self,
        cx: &mut ShellContext,
        width: u32,
        height: u32,
        mut rgba: Vec<u8>,
    ) -> Result<String, String> {
        flatten_alpha_on_white(&mut rgba);
        let monitors = cx.monitors().map_err(|e| e.to_string())?;
        let cursor = cursor_screen_position().ok();
        let work_area = pick_monitor(&monitors, cursor)
            .map(|m| m.work_area)
            .ok_or_else(|| "系统没有可用显示器".to_string())?;
        let (rect, zoom) = initial_clipboard_rect(width, height, work_area);
        let geometry = PinGeometry::new(rect, zoom, 1.0, true);
        self.create(cx, width, height, rgba, geometry)
    }

    /// 创建贴图：分配 ID、先落盘（崩溃安全）再开窗、按容量策略淘汰最老的。
    fn create(
        &mut self,
        cx: &mut ShellContext,
        width: u32,
        height: u32,
        rgba: Vec<u8>,
        geometry: PinGeometry,
    ) -> Result<String, String> {
        let id = self.shared.new_id()?;
        let created_ms = now_utc_ms();
        let png = encode_png(width, height, &rgba)?;
        let payload_bytes = png.len() as u64;
        if let Err(e) = self
            .shared
            .persist(&id, &geometry, created_ms, payload_bytes, Some(png))
        {
            // 落盘失败不阻止贴图显示，但重启后无法恢复，必须留痕
            tracing::error!(id = %id, error = %e, "贴图落盘失败，重启后将无法恢复");
        }
        let frame = frame_from_rgba(width, height, rgba)?;
        let init = PinInit {
            id: id.clone(),
            frame,
            geometry,
            created_ms,
            payload_bytes,
            session: Vec::new(),
            dpr: 1.0,
        };
        self.open_window(cx, init)?;
        for victim in self.shared.evict(Some(&id)) {
            tracing::info!(id = %victim, "贴图超出容量策略，已淘汰");
            self.close_evicted(cx, &victim);
        }
        self.start_autotest(cx, &id);
        Ok(id)
    }

    /// 打开贴图窗口并登记。
    fn open_window(&mut self, cx: &mut ShellContext, init: PinInit) -> Result<(), String> {
        let geometry = init.geometry;
        let id = init.id.clone();
        let spec = WindowSpec {
            title: PIN_WINDOW_TITLE.to_string(),
            placement: Placement::Physical(geometry.rect()),
            transparent: true,
            always_on_top: geometry.topmost,
            decorations: false,
            show_in_taskbar: false,
            focus: false,
            resizable: false,
        };
        let shared = Rc::clone(&self.shared);
        let (window, view) = cx
            .open_window(&spec, move |window, app| {
                PinnedWindowView::create(window, app, shared, init)
            })
            .map_err(|e| format!("打开贴图窗口失败: {e}"))?;
        view.update(cx.app(), |v, _| v.set_window(window));
        tracing::info!(
            id = %id,
            rect = ?geometry.rect(),
            topmost = geometry.topmost,
            hwnd = ?window.native_id().map(|h| h.0),
            "贴图窗口已打开"
        );
        self.windows.insert(id, PinWindow { window, view });
        Ok(())
    }

    /// 关闭被淘汰的贴图窗口（存储已由淘汰逻辑移除）。
    fn close_evicted(&mut self, cx: &mut ShellContext, id: &str) {
        let Some(pin) = self.windows.remove(id) else {
            return;
        };
        let _ = pin.window.gpui_handle().update(cx.app(), |_, window, app| {
            pin.view.update(app, |v, _| v.close(window, false));
        });
    }

    /// 启动时恢复已持久化的贴图：先清扫孤儿并按策略淘汰，再逐张读回、校正到可见区域后开窗。
    ///
    /// # 返回
    /// 成功恢复的窗口数。
    ///
    /// ```ignore
    /// let restored = manager.restore_all(cx);
    /// ```
    pub fn restore_all(&mut self, cx: &mut ShellContext) -> usize {
        if !self.shared.policy().enabled {
            tracing::info!("贴图历史已关闭，跳过恢复");
            return 0;
        }
        self.shared.sweep_orphans();
        for victim in self.shared.evict(None) {
            tracing::info!(id = %victim, "启动时按容量策略淘汰贴图");
        }
        let monitors = cx.monitors().unwrap_or_else(|e| {
            tracing::warn!(error = %e, "枚举显示器失败，按保存的位置恢复贴图");
            Monitors::default()
        });
        let monitor_bounds = monitor_rects(&monitors);
        let mut restored = 0;
        for (index, id) in self.shared.record_ids().into_iter().enumerate() {
            self.shared.adopt_id(&id);
            let pin = match self.shared.load(&id) {
                Ok(pin) => pin,
                Err(e) => {
                    tracing::error!(id = %id, error = %e, "贴图恢复失败（记录保留在存储中）");
                    continue;
                }
            };
            let saved = pin.geometry.unwrap_or_else(|| {
                PinGeometry::new(
                    PhysicalRect::new(0, 0, pin.width as i32, pin.height as i32),
                    1.0,
                    1.0,
                    true,
                )
            });
            let mut geometry = saved;
            let rect = visible_rect(saved.rect(), &monitor_bounds, index);
            geometry.x = rect.x;
            geometry.y = rect.y;
            geometry.width = rect.width;
            geometry.height = rect.height;
            let frame = match frame_from_rgba(pin.width, pin.height, pin.rgba) {
                Ok(f) => f,
                Err(e) => {
                    tracing::error!(id = %id, error = %e, "贴图底图构造失败");
                    continue;
                }
            };
            let init = PinInit {
                id: id.clone(),
                frame,
                geometry,
                created_ms: pin.created_ms,
                payload_bytes: pin.payload_bytes,
                session: pin.canvas_session,
                dpr: monitors
                    .best_for_rect(rect)
                    .map_or(1.0, |m| m.scale.value()),
            };
            match self.open_window(cx, init) {
                Ok(()) => restored += 1,
                Err(e) => tracing::error!(id = %id, error = %e, "贴图恢复开窗失败"),
            }
        }
        tracing::info!(restored, "贴图恢复完成");
        restored
    }

    /// 把全部打开窗口的最新状态写盘（退出前调用，防抖中的改动不丢）。
    pub fn persist_all(&mut self, cx: &mut ShellContext) {
        for pin in self.windows.values() {
            pin.view.update(cx.app(), |v, _| {
                v.persist_now();
                // 渲染次数用来佐证“空闲无重绘”：只有交互 / 标注变化才会增加
                tracing::info!(id = %v.id(), renders = v.render_count(), "贴图退出前状态");
            });
        }
    }

    /// 环境变量指定了自动化脚本时，对新贴图逐步执行（验收用）。
    fn start_autotest(&self, cx: &mut ShellContext, id: &str) {
        let Ok(path) = std::env::var(ENV_PIN_AUTOTEST) else {
            return;
        };
        let ops = match std::fs::read_to_string(&path)
            .map_err(|e| e.to_string())
            .and_then(|text| parse_pin_ops(&text))
        {
            Ok(ops) => ops,
            Err(e) => {
                tracing::error!(path = %path, error = %e, "贴图自动化脚本无效");
                return;
            }
        };
        let Some(pin) = self.windows.get(id) else {
            return;
        };
        let (window, view) = (pin.window, pin.view.clone());
        let id = id.to_string();
        tracing::info!(id = %id, count = ops.len(), "贴图自动化开始");
        cx.app()
            .spawn(async move |acx| {
                acx.background_executor().timer(AUTOTEST_START_DELAY).await;
                for op in ops {
                    let ran = window.gpui_handle().update(acx, |_, window, app| {
                        view.update(app, |v, cx| v.run_autotest_op(&op, window, cx));
                    });
                    if ran.is_err() {
                        tracing::info!(id = %id, "贴图窗口已关闭，自动化提前结束");
                        return;
                    }
                    acx.background_executor()
                        .timer(AUTOTEST_STEP_INTERVAL)
                        .await;
                }
                tracing::info!(id = %id, "贴图自动化结束");
            })
            .detach();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use snow_ui::shell::geometry::ScaleFactor;
    use snow_ui::shell::monitor::{MonitorId, MonitorInfo};

    /// 构造测试显示器。
    fn monitor(id: u64, bounds: PhysicalRect, primary: bool) -> MonitorInfo {
        MonitorInfo {
            id: MonitorId(id),
            name: String::new(),
            bounds,
            work_area: bounds,
            scale: ScaleFactor::ONE,
            is_primary: primary,
        }
    }

    /// 显示器范围列表把主显示器排在最前，其余保持相对顺序。
    #[test]
    fn primary_monitor_first() {
        let monitors = Monitors::from_list(vec![
            monitor(1, PhysicalRect::new(2560, 0, 2560, 1440), false),
            monitor(2, PhysicalRect::new(0, 0, 2560, 1440), true),
            monitor(3, PhysicalRect::new(-1920, 0, 1920, 1080), false),
        ]);
        let rects = monitor_rects(&monitors);
        assert_eq!(rects[0], PhysicalRect::new(0, 0, 2560, 1440));
        assert_eq!(rects.len(), 3);
        assert!(monitor_rects(&Monitors::default()).is_empty());
    }

    /// 自动化脚本解析：合法数组通过，坏 JSON / 未知操作被拒绝。
    #[test]
    fn autotest_script_parsing() {
        let ops = parse_pin_ops(
            r#"[{"op":"wheel","steps":1.5,"ctrl":true},{"op":"undo"},{"op":"menu"}]"#,
        )
        .unwrap();
        assert_eq!(ops.len(), 3);
        assert!(parse_pin_ops("not json").is_err());
        assert!(parse_pin_ops(r#"[{"op":"explode"}]"#).is_err());
        assert!(parse_pin_ops("[]").unwrap().is_empty());
        // 带 UTF-8 BOM 的脚本文件也能解析
        assert_eq!(
            parse_pin_ops("\u{feff}[{\"op\":\"close\"}]").unwrap(),
            vec![PinOp::Close]
        );
    }
}
