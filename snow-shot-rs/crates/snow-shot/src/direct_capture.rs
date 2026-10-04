//! 直接截图：不进覆盖层，后台线程抓取区域后按输出方案复制 / 保存。

use crate::history_store::{HistoryRecorder, HistorySource};
use crate::overlay_view::{OutputSink, SystemOutput};
use crate::quick_actions::DirectOutputPlan;
use snow_history::capture_history::CaptureHistoryPolicy;
use std::path::PathBuf;
use std::sync::Arc;

/// 直接截图线程名称。
const DIRECT_THREAD_NAME: &str = "snow-direct-capture";

/// 一次直接截图的结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DirectResult {
    /// 图像宽（像素）。
    pub width: u32,
    /// 图像高（像素）。
    pub height: u32,
    /// 复制到剪贴板的结果；未执行为 `None`。
    pub copied: Option<Result<(), String>>,
    /// 保存文件的结果；未执行为 `None`。
    pub saved: Option<Result<PathBuf, String>>,
}

/// 直接截图写入历史所需的上下文（可跨线程）。
#[derive(Clone)]
pub struct DirectHistory {
    /// 后台写入器。
    pub recorder: Arc<HistoryRecorder>,
    /// 提交时的策略快照。
    pub policy: CaptureHistoryPolicy,
    /// 记录来源（当前显示器 / 前台窗口）。
    pub source: HistorySource,
}

impl DirectResult {
    /// 是否至少有一步输出成功（全部失败时不应写入历史）。
    pub fn any_success(&self) -> bool {
        matches!(self.copied, Some(Ok(_))) || matches!(self.saved, Some(Ok(_)))
    }

    /// 是否有任一步骤失败。
    pub fn has_failure(&self) -> bool {
        matches!(self.copied, Some(Err(_))) || matches!(self.saved, Some(Err(_)))
    }

    /// 汇总失败原因（用于提示与日志）；没有失败返回 `None`。
    pub fn failure_reason(&self) -> Option<String> {
        let mut reasons = Vec::new();
        if let Some(Err(e)) = &self.copied {
            reasons.push(e.clone());
        }
        if let Some(Err(e)) = &self.saved {
            reasons.push(e.clone());
        }
        (!reasons.is_empty()).then(|| reasons.join("; "))
    }
}

/// 按输出方案把图像交给输出通道：先复制，再保存；两步互不影响。
///
/// # 参数
/// - `plan`：输出方案。
/// - `sink`：输出通道（剪贴板 / 文件）。
/// - `width` / `height` / `rgba`：图像尺寸与 RGBA 像素。
///
/// # 返回
/// 各步骤结果。
///
/// ```ignore
/// let result = apply_direct_output(plan, &mut sink, w, h, &rgba);
/// ```
pub fn apply_direct_output(
    plan: DirectOutputPlan,
    sink: &mut dyn OutputSink,
    width: u32,
    height: u32,
    rgba: &[u8],
) -> DirectResult {
    DirectResult {
        width,
        height,
        copied: plan.copy.then(|| sink.copy_image(width, height, rgba)),
        saved: plan.save.then(|| sink.save_image(width, height, rgba)),
    }
}

/// 在后台线程抓取桌面区域并按方案输出，完成后调用 `on_done`（运行在该线程上，只应做投递）。
///
/// # 参数
/// - `region`：桌面物理坐标下的 `(x, y, 宽, 高)`。
/// - `plan`：输出方案。
/// - `save_dir`：保存目录。
/// - `history`：历史写入上下文；`None` 表示不写入。
/// - `on_done`：结果回调；采集失败时为错误说明。
///
/// # 返回
/// 线程创建失败返回 IO 错误（此时 `on_done` 不会被调用）。
///
/// ```ignore
/// spawn_direct_capture((0, 0, 100, 100), plan, dir, None, |r| println!("{:?}", r.is_ok()))?;
/// ```
pub fn spawn_direct_capture(
    region: (i32, i32, u32, u32),
    plan: DirectOutputPlan,
    save_dir: PathBuf,
    history: Option<DirectHistory>,
    on_done: impl FnOnce(Result<DirectResult, String>) + Send + 'static,
) -> std::io::Result<()> {
    std::thread::Builder::new()
        .name(DIRECT_THREAD_NAME.into())
        .spawn(move || {
            let result = snow_platform::capture::capture_display(Some(region)).map(|screen| {
                let rgba = screen.to_rgba();
                let mut sink = SystemOutput::new(save_dir);
                let result = apply_direct_output(plan, &mut sink, screen.width, screen.height, &rgba);
                if let Some(h) = history.as_ref().filter(|_| result.any_success()) {
                    h.recorder
                        .submit(h.policy.clone(), h.source, screen.width, screen.height, &rgba);
                }
                result
            });
            on_done(result);
        })
        .map(|_| ())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 记录调用的假输出通道，可按需让某一步失败。
    #[derive(Default)]
    struct Fake {
        /// 复制调用次数。
        copies: usize,
        /// 保存调用次数。
        saves: usize,
        /// 复制是否失败。
        fail_copy: bool,
    }

    impl OutputSink for Fake {
        /// 记录复制。
        fn copy_image(&mut self, _w: u32, _h: u32, _rgba: &[u8]) -> Result<(), String> {
            self.copies += 1;
            if self.fail_copy {
                Err("剪贴板被占用".into())
            } else {
                Ok(())
            }
        }

        /// 不使用。
        fn copy_text(&mut self, _text: &str) -> Result<(), String> {
            Ok(())
        }

        /// 记录保存。
        fn save_image(&mut self, _w: u32, _h: u32, _rgba: &[u8]) -> Result<PathBuf, String> {
            self.saves += 1;
            Ok(PathBuf::from("a.png"))
        }
    }

    /// 只复制：不触发保存。
    #[test]
    fn copy_only() {
        let mut sink = Fake::default();
        let r = apply_direct_output(
            DirectOutputPlan {
                copy: true,
                save: false,
            },
            &mut sink,
            1,
            1,
            &[0; 4],
        );
        assert_eq!((sink.copies, sink.saves), (1, 0));
        assert!(r.copied == Some(Ok(())) && r.saved.is_none() && !r.has_failure());
    }

    /// 复制 + 保存：两步都执行；复制失败不阻止保存，失败原因被汇总。
    #[test]
    fn copy_failure_does_not_block_save() {
        let mut sink = Fake {
            fail_copy: true,
            ..Fake::default()
        };
        let r = apply_direct_output(
            DirectOutputPlan {
                copy: true,
                save: true,
            },
            &mut sink,
            2,
            3,
            &[0; 24],
        );
        assert_eq!((sink.copies, sink.saves), (1, 1));
        assert_eq!((r.width, r.height), (2, 3));
        assert!(r.has_failure());
        assert_eq!(r.failure_reason().as_deref(), Some("剪贴板被占用"));
        assert_eq!(r.saved, Some(Ok(PathBuf::from("a.png"))));
    }

    /// 什么都不做的方案不触碰输出通道。
    #[test]
    fn empty_plan_touches_nothing() {
        let mut sink = Fake::default();
        let r = apply_direct_output(
            DirectOutputPlan {
                copy: false,
                save: false,
            },
            &mut sink,
            1,
            1,
            &[0; 4],
        );
        assert_eq!((sink.copies, sink.saves), (0, 0));
        assert!(r.failure_reason().is_none());
    }

    /// 只要有一步输出成功就应写入历史；全部失败或没有输出则不写。
    #[test]
    fn any_success_gates_history() {
        let ok = DirectResult {
            width: 1,
            height: 1,
            copied: Some(Err("busy".into())),
            saved: Some(Ok(PathBuf::from("a.png"))),
        };
        assert!(ok.any_success());
        let failed = DirectResult {
            width: 1,
            height: 1,
            copied: Some(Err("busy".into())),
            saved: None,
        };
        assert!(!failed.any_success());
        let none = DirectResult {
            width: 1,
            height: 1,
            copied: None,
            saved: None,
        };
        assert!(!none.any_success());
    }
}
