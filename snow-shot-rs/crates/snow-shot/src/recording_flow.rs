//! 录屏流程编排：选区确认 → 拉起录制进程 → 录制窗（倒计时 / 控制条）→ 完成 / 出错收尾。
//!
//! 所有 GPUI 对象只在主线程使用；录制进程的事件经 `wake` 回调投递 [`UiEvent::RecorderPoll`] 回到主线程。

use crate::app_runtime::UiEvent;
use crate::settings_state::SharedConfig;
use crate::recording::client::{ProcessRecorderLink, locate_recorder_exe};
use crate::recording::audio::restrict_to_format;
use crate::recording::output::build_recording_config;
use crate::recording::{AutoPlan, RecordingAreaView, RecordingFormat, RecordingState};
use crate::screenshot_output::home_directory;
use snow_capability::CapabilityRegistry;
use snow_platform::local_time;
use snow_ui::shell::geometry::{PhysicalPoint, PhysicalRect, Region};
use snow_ui::shell::inbox::MainThreadInbox;
use snow_ui::shell::monitor::{MonitorInfo, MonitorTarget};
use snow_ui::shell::window::WindowSpec;
use snow_ui::ui::{AppContext, Entity, ShellContext, ShellWindow};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

/// 自动化验证环境变量：`x,y,w,h,秒数[,格式[,暂停起点秒:暂停秒数]]`，设置后录屏跳过选区，直接开始。
pub const ENV_RECORDING_AUTOTEST: &str = "SNOW_RECORDING_AUTOTEST";
/// 自动化验证时覆盖输出目录的环境变量（避免污染用户视频目录）。
pub const ENV_RECORDING_AUTOTEST_DIR: &str = "SNOW_RECORDING_AUTOTEST_DIR";
/// 调试：设置后录制窗不从捕获中排除（用作“排除是否生效”的对照）。
pub const ENV_KEEP_VISIBLE: &str = "SNOW_RECORDING_KEEP_VISIBLE";
/// 录制窗刷新间隔。
const TICK_INTERVAL: Duration = Duration::from_millis(250);
/// 自动化参数的最少字段数（区域 4 + 秒数 1）。
const AUTOTEST_MIN_FIELDS: usize = 5;

/// 自动化验证参数。
#[derive(Debug, Clone, PartialEq)]
pub struct AutotestSpec {
    /// 录制区域（虚拟桌面物理坐标）。
    pub region: PhysicalRect,
    /// 输出格式覆盖（缺省用配置）。
    pub format: Option<RecordingFormat>,
    /// 自动化计划。
    pub plan: AutoPlan,
}

/// 解析自动化参数；字段缺失、非数字或尺寸非法返回 `None`。
///
/// # 参数
/// - `text`：环境变量原始值。
///
/// # 示例
/// ```ignore
/// let s = parse_autotest("0,0,800,600,5,gif,2:3").unwrap();
/// assert_eq!(s.plan.stop_after_secs, 5);
/// ```
pub fn parse_autotest(text: &str) -> Option<AutotestSpec> {
    let fields: Vec<&str> = text.split(',').map(str::trim).collect();
    if fields.len() < AUTOTEST_MIN_FIELDS {
        return None;
    }
    let x: i32 = fields[0].parse().ok()?;
    let y: i32 = fields[1].parse().ok()?;
    let w: i32 = fields[2].parse().ok().filter(|v| *v > 0)?;
    let h: i32 = fields[3].parse().ok().filter(|v| *v > 0)?;
    let stop_after_secs: u64 = fields[4].parse().ok().filter(|v| *v > 0)?;
    let format = fields
        .get(5)
        .filter(|f| !f.is_empty())
        .map(|f| RecordingFormat::from_config(f));
    let pause = match fields.get(6) {
        Some(spec) if !spec.is_empty() => {
            let (at, len) = spec.split_once(':')?;
            Some((at.parse().ok()?, len.parse().ok()?))
        }
        _ => None,
    };
    Some(AutotestSpec {
        region: PhysicalRect::new(x, y, w, h),
        format,
        plan: AutoPlan {
            stop_after_secs,
            pause,
        },
    })
}

/// 选出包含区域左上角的显示器；都不包含时取主显示器，再不行取第一个。
///
/// # 参数
/// - `monitors`：显示器列表。
/// - `region`：录制区域（虚拟桌面物理坐标）。
pub fn monitor_for_region(monitors: &[MonitorInfo], region: PhysicalRect) -> Option<&MonitorInfo> {
    let corner = PhysicalPoint::new(region.x, region.y);
    monitors
        .iter()
        .find(|m| m.bounds.contains(corner))
        .or_else(|| monitors.iter().find(|m| m.is_primary))
        .or_else(|| monitors.first())
}

/// 正在进行的一次录制。
struct ActiveRecording {
    /// 录制窗。
    window: ShellWindow,
    /// 录制窗视图。
    view: Entity<RecordingAreaView>,
    /// 已应用到窗口的命中区域（避免重复设置）。
    applied_hit: Vec<PhysicalRect>,
    /// 完成后是否在资源管理器中定位文件。
    reveal: bool,
}

/// 录屏宿主：持有当前录制窗并处理其生命周期。
pub struct RecordingHost {
    /// 平台能力表（设置命中区域时需要）。
    caps: CapabilityRegistry,
    /// 主线程收件箱。
    inbox: MainThreadInbox<UiEvent>,
    /// 共享配置存储（与设置页同一份）。
    config: SharedConfig,
    /// 当前录制。
    active: Option<ActiveRecording>,
}

impl RecordingHost {
    /// 创建宿主。
    ///
    /// # 参数
    /// - `caps`：能力表。
    /// - `inbox`：主线程收件箱。
    /// - `config`：共享配置存储。
    pub fn new(caps: CapabilityRegistry, inbox: MainThreadInbox<UiEvent>, config: SharedConfig) -> Self {
        Self {
            caps,
            inbox,
            config,
            active: None,
        }
    }

    /// 是否有录制窗仍在运行（未关闭）。
    ///
    /// # 参数
    /// - `cx`：外壳上下文。
    pub fn is_busy(&self, cx: &ShellContext) -> bool {
        self.active
            .as_ref()
            .is_some_and(|a| cx.is_window_open(&a.window))
    }

    /// 开始一次录制：解析配置、拉起录制进程、打开录制窗。
    ///
    /// # 参数
    /// - `cx`：外壳上下文。
    /// - `region`：录制区域（虚拟桌面物理坐标）。
    /// - `monitor`：区域所在显示器。
    /// - `autotest`：自动化参数（验收用）；给出时不打开资源管理器。
    pub fn begin(
        &mut self,
        cx: &mut ShellContext,
        region: PhysicalRect,
        monitor: &MonitorInfo,
        autotest: Option<&AutotestSpec>,
    ) {
        if self.is_busy(cx) {
            tracing::info!("已有录制在进行，忽略新的录制请求");
            return;
        }
        let built = build_recording_config(
            self.config.borrow().document(),
            region,
            home_directory().as_deref(),
            local_time::now(),
        );
        let mut config = match built {
            Ok(c) => c,
            Err(e) => {
                tracing::error!(error = %e, "无法生成录制配置");
                return;
            }
        };
        if let Some(format) = autotest.and_then(|a| a.format) {
            // 自动化指定格式时，扩展名跟随格式
            config.format = format;
            config.output_path.set_extension(format.extension());
            config.audio = restrict_to_format(std::mem::take(&mut config.audio), format);
        }
        if autotest.is_some()
            && let Some(dir) = std::env::var_os(ENV_RECORDING_AUTOTEST_DIR).filter(|d| !d.is_empty())
            && let Some(name) = config.output_path.file_name().map(|n| n.to_owned())
        {
            config.output_path = PathBuf::from(dir).join(name);
        }
        tracing::info!(
            region = ?config.region,
            format = ?config.format,
            fps = config.fps,
            countdown = config.countdown_secs,
            output = %config.output_path.display(),
            "开始录屏"
        );
        let countdown = config.countdown_secs;
        let mut view = RecordingAreaView::new(config, monitor.bounds, monitor.scale.value());
        view.set_locale(crate::app_runtime::ui_prefs_from_document(self.config.borrow().document()).locale);
        if let Some(a) = autotest {
            view.set_auto_plan(a.plan);
        }
        self.attach_recorder(&mut view, countdown);

        let mut spec = WindowSpec::overlay(MonitorTarget::Id(monitor.id));
        spec.focus = false;
        let opened = cx.open_window(&spec, move |_window, app| app.new(|_| view));
        match opened {
            Ok((window, view)) => {
                self.configure_window(&window);
                self.active = Some(ActiveRecording {
                    window,
                    view: view.clone(),
                    applied_hit: Vec::new(),
                    reveal: autotest.is_none(),
                });
                self.spawn_ticker(cx, view);
                self.sync(cx);
            }
            Err(e) => tracing::error!(error = %e, "打开录制窗失败"),
        }
    }

    /// 拉起录制进程并接入会话；失败则让会话直接进入错误状态（用户能在窗口里看到原因）。
    fn attach_recorder(&self, view: &mut RecordingAreaView, countdown: u32) {
        let Some(exe) = locate_recorder_exe() else {
            tracing::error!("找不到录制进程 snow-recorder（可用环境变量 SNOW_RECORDER_EXE 指定）");
            view.session
                .abort("找不到录制进程 snow-recorder，请先构建 scripts/build-snow-recorder.ps1".to_string());
            return;
        };
        let inbox = self.inbox.clone();
        let wake = Arc::new(move || {
            inbox.push(UiEvent::RecorderPoll);
        });
        match ProcessRecorderLink::spawn(&exe, wake) {
            Ok(link) => view.session.begin(Box::new(link), countdown),
            Err(e) => {
                tracing::error!(error = %e, "启动录制进程失败");
                view.session.abort(e);
            }
        }
    }

    /// 录制窗自身不进成片；命中区域稍后由 [`RecordingHost::sync`] 设置。
    fn configure_window(&self, window: &ShellWindow) {
        if std::env::var_os(ENV_KEEP_VISIBLE).is_some() {
            tracing::warn!("调试：录制窗保持可被捕获（不排除）");
            return;
        }
        match window.overlay(&self.caps) {
            Ok(overlay) => {
                if let Err(e) = overlay.set_capture_excluded(true) {
                    tracing::warn!(error = %e, "录制窗未能排除在捕获之外，控制条可能进入成片");
                }
            }
            Err(e) => tracing::warn!(error = %e, "无法取得录制窗原生句柄"),
        }
    }

    /// 启动定时器：周期性向收件箱投递 `RecordingTick`，窗口消失后自动退出。
    fn spawn_ticker(&self, cx: &mut ShellContext, view: Entity<RecordingAreaView>) {
        let inbox = self.inbox.clone();
        let weak = view.downgrade();
        cx.app()
            .spawn(async move |acx| {
                loop {
                    acx.background_executor().timer(TICK_INTERVAL).await;
                    if weak.update(acx, |_, _| ()).is_err() || !inbox.push(UiEvent::RecordingTick) {
                        return;
                    }
                }
            })
            .detach();
    }

    /// 推进录制窗状态、同步命中区域，并处理完成 / 出错 / 取消的收尾。
    ///
    /// # 参数
    /// - `cx`：外壳上下文。
    pub fn sync(&mut self, cx: &mut ShellContext) {
        let Some(active) = self.active.as_mut() else {
            return;
        };
        let now = Instant::now();
        let (layout_rects, over, finished) = active.view.update(cx.app(), |v, vcx| {
            if v.advance(now) {
                vcx.notify();
            }
            let finished = match v.session.state() {
                RecordingState::Finished { file_path, .. } => Some(file_path.clone()),
                _ => None,
            };
            (v.layout().hit_rects(), v.is_over(now), finished)
        });
        if layout_rects != active.applied_hit {
            let mut region = Region::new();
            for rect in &layout_rects {
                region.union_rect(*rect);
            }
            match active.window.overlay(&self.caps) {
                Ok(mut overlay) => match overlay.set_hit_region(&region) {
                    Ok(()) => active.applied_hit = layout_rects,
                    Err(e) => {
                        tracing::warn!(error = %e, "设置录制窗命中区域失败");
                        active.applied_hit = layout_rects;
                    }
                },
                Err(e) => tracing::warn!(error = %e, "无法取得录制窗句柄"),
            }
        }
        if !over {
            return;
        }
        let window = active.window;
        let reveal = active.reveal;
        self.active = None;
        window.close(cx.app());
        match finished {
            Some(path) => {
                tracing::info!(path = %path.display(), "录屏完成");
                if reveal && let Err(e) = snow_platform::shell::reveal_in_explorer(&path) {
                    tracing::warn!(error = %e, "无法在资源管理器中定位录制文件");
                }
            }
            None => tracing::info!("录制窗已关闭（取消或出错）"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use snow_ui::shell::geometry::ScaleFactor;
    use snow_ui::shell::monitor::MonitorId;

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

    /// 自动化参数解析：最小形态、带格式、带暂停计划。
    #[test]
    fn autotest_parsing() {
        let s = parse_autotest("10,20,800,600,5").unwrap();
        assert_eq!(s.region, PhysicalRect::new(10, 20, 800, 600));
        assert_eq!(s.plan, AutoPlan { stop_after_secs: 5, pause: None });
        assert_eq!(s.format, None);
        let s = parse_autotest("0,0,100,100,6,gif,2:3").unwrap();
        assert_eq!(s.format, Some(RecordingFormat::Gif));
        assert_eq!(s.plan.pause, Some((2, 3)));
        // webm 归一化为默认格式
        assert_eq!(parse_autotest("0,0,10,10,1,webm").unwrap().format, Some(RecordingFormat::Mp4));
    }

    /// 非法自动化参数被拒绝而不是 panic。
    #[test]
    fn autotest_rejects_bad_input() {
        for bad in ["", "1,2,3", "a,b,c,d,e", "0,0,0,10,5", "0,0,10,10,0", "0,0,10,10,5,mp4,2", "0,0,10,10,5,mp4,x:y"] {
            assert_eq!(parse_autotest(bad), None, "应拒绝 {bad:?}");
        }
    }

    /// 区域所在显示器：按左上角判定，副屏为负原点时同样成立；越界回退主屏。
    #[test]
    fn monitor_selection() {
        let left = monitor(1, PhysicalRect::new(-1920, 0, 1920, 1080), false);
        let main = monitor(2, PhysicalRect::new(0, 0, 2560, 1440), true);
        let list = [left.clone(), main.clone()];
        assert_eq!(monitor_for_region(&list, PhysicalRect::new(-100, 10, 50, 50)).map(|m| m.id), Some(left.id));
        assert_eq!(monitor_for_region(&list, PhysicalRect::new(100, 10, 50, 50)).map(|m| m.id), Some(main.id));
        assert_eq!(monitor_for_region(&list, PhysicalRect::new(9999, 9999, 5, 5)).map(|m| m.id), Some(main.id));
        assert!(monitor_for_region(&[], PhysicalRect::new(0, 0, 1, 1)).is_none());
    }
}
