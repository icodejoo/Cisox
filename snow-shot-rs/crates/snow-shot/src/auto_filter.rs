//! 自动滤镜：选区内的视觉区域检测（后台线程）与区域记录转换。
//!
//! 对应旧版 `ScreenshotAutoFilterController`：在选区截图上跑 `visual-region-detector`
//! （纯 Rust 后端），把结果按 7 类映射成引擎的 [`AutoFilterRegionRecord`]；之后点击 / 拖选
//! 命中区域并铺滤镜由引擎的 `AutoFilter` 工具完成。
//!
//! 本模块不依赖 GPUI，检测在一次性后台线程里完成：不常驻、可取消（取消后结果被丢弃，
//! 检测器本身不支持中途打断，线程跑完自行退出）。

use snow_draw_engine::DrawRect;
use snow_draw_engine_document::{AutoFilterRegion, AutoFilterRegionRecord};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, TryRecvError};
use visual_region_detector::{BgrImage, detect_regions};

/// 整数矩形 `[x0, y0, x1, y1]`（右下开区间，画布物理像素）。
pub type IntRect = [i32; 4];

/// 旧版 `kCategories`：检测类别名，顺序与旧版一致。
pub const CATEGORIES: [&str; 7] = [
    "text",
    "text_in_box",
    "image",
    "avatar",
    "icon",
    "message_box",
    "text_block",
];

/// 每像素字节数（RGBA / BGR 之前的输入）。
const RGBA_BPP: usize = 4;

/// 自动滤镜铺的滤镜类型。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum AutoFilterKind {
    /// 马赛克（默认）。
    #[default]
    Mosaic,
    /// 高斯模糊。
    Blur,
}

impl AutoFilterKind {
    /// 全部可选类型（下拉顺序）。
    pub const ALL: [AutoFilterKind; 2] = [AutoFilterKind::Mosaic, AutoFilterKind::Blur];

    /// 稳定标识（下拉选项值）。
    pub fn id(self) -> &'static str {
        match self {
            Self::Mosaic => "mosaic",
            Self::Blur => "blur",
        }
    }

    /// 由稳定标识还原；未知返回 `None`。
    ///
    /// ```
    /// use snow_shot::auto_filter::AutoFilterKind;
    /// assert_eq!(AutoFilterKind::from_id("blur"), Some(AutoFilterKind::Blur));
    /// assert_eq!(AutoFilterKind::from_id("x"), None);
    /// ```
    pub fn from_id(id: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|k| k.id() == id)
    }

    /// 下拉选项的文案消息 id。
    pub fn text_id(self) -> &'static str {
        match self {
            Self::Mosaic => "annot-autofilter-kind-mosaic",
            Self::Blur => "annot-autofilter-kind-blur",
        }
    }
}

/// 一个检测到的区域（源图像素坐标，已映射到 7 类之一）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DetectedRegion {
    /// 左上角 x。
    pub x: i32,
    /// 左上角 y。
    pub y: i32,
    /// 宽。
    pub w: i32,
    /// 高。
    pub h: i32,
    /// 类别名（取自 [`CATEGORIES`]）。
    pub category: &'static str,
}

/// 在 RGBA 图像上检测区域；类别不在 [`CATEGORIES`] 内的结果被丢弃（同旧版）。
///
/// # 参数
/// - `width` / `height`：图像像素尺寸。
/// - `rgba`：RGBA 像素，长度必须是 `width * height * 4`。
///
/// # 返回
/// 区域列表（检测器输出顺序）；图像非法或检测失败返回错误说明。
///
/// ```
/// use snow_shot::auto_filter::detect_rgba;
/// let blank = vec![255u8; 32 * 32 * 4];
/// assert!(detect_rgba(32, 32, &blank).unwrap().is_empty());
/// assert!(detect_rgba(0, 0, &[]).is_err());
/// ```
pub fn detect_rgba(width: u32, height: u32, rgba: &[u8]) -> Result<Vec<DetectedRegion>, String> {
    let expected = width as usize * height as usize * RGBA_BPP;
    if rgba.len() != expected {
        return Err(format!(
            "图像缓冲长度 {} 与尺寸 {width}x{height} 不符",
            rgba.len()
        ));
    }
    let bgr: Vec<[u8; 3]> = rgba
        .chunks_exact(RGBA_BPP)
        .map(|p| [p[2], p[1], p[0]])
        .collect();
    let image = BgrImage::from_bgr_pixels(width, height, bgr).map_err(|e| e.to_string())?;
    let regions = detect_regions(&image).map_err(|e| e.to_string())?;
    Ok(regions
        .into_iter()
        .filter_map(|r| {
            let category = CATEGORIES.iter().find(|c| **c == r.kind)?;
            Some(DetectedRegion {
                x: r.rect.x,
                y: r.rect.y,
                w: r.rect.w,
                h: r.rect.h,
                category,
            })
        })
        .collect())
}

/// 把检测结果换算成引擎区域记录（同旧版 `finish`）：按源范围与像素尺寸缩放，裁进源范围，丢弃空区域，编号从 1 起。
///
/// # 参数
/// - `source`：选区在画布上的范围。
/// - `pixels`：检测所用图像的像素尺寸。
/// - `regions`：检测结果（源图像素坐标）。
///
/// # 返回
/// 区域记录；源范围或像素尺寸为空返回 `None`。
///
/// ```
/// use snow_shot::auto_filter::{DetectedRegion, regions_to_record};
/// let r = DetectedRegion { x: 2, y: 2, w: 4, h: 4, category: "text" };
/// let rec = regions_to_record([10, 20, 110, 70], (100, 50), &[r]).unwrap();
/// assert_eq!(rec.regions[0].id, 1);
/// assert_eq!(rec.regions[0].bounds.min_x, 12.0);
/// ```
pub fn regions_to_record(
    source: IntRect,
    pixels: (u32, u32),
    regions: &[DetectedRegion],
) -> Option<AutoFilterRegionRecord> {
    let (sw, sh) = (
        f64::from(source[2] - source[0]),
        f64::from(source[3] - source[1]),
    );
    if sw <= 0.0 || sh <= 0.0 || pixels.0 == 0 || pixels.1 == 0 {
        return None;
    }
    let (sx, sy) = (sw / f64::from(pixels.0), sh / f64::from(pixels.1));
    let (ox, oy) = (f64::from(source[0]), f64::from(source[1]));
    let (right, bottom) = (f64::from(source[2]), f64::from(source[3]));
    let regions = regions
        .iter()
        .enumerate()
        .filter_map(|(i, r)| {
            let min_x = (ox + f64::from(r.x) * sx).max(ox);
            let min_y = (oy + f64::from(r.y) * sy).max(oy);
            let max_x = (ox + f64::from(r.x + r.w) * sx).min(right);
            let max_y = (oy + f64::from(r.y + r.h) * sy).min(bottom);
            (max_x > min_x && max_y > min_y).then(|| AutoFilterRegion {
                id: i as u64 + 1,
                bounds: DrawRect::new(min_x, min_y, max_x, max_y),
                category: r.category.to_string(),
            })
        })
        .collect();
    Some(AutoFilterRegionRecord {
        source_bounds: DrawRect::new(ox, oy, right, bottom),
        regions,
    })
}

/// 后台检测任务的结果。
type JobResult = Result<Vec<DetectedRegion>, String>;

/// 一次后台检测任务；丢弃或 [`AutoFilterJob::cancel`] 后结果不再被取用。
pub struct AutoFilterJob {
    /// 任务对应的选区范围。
    source: IntRect,
    /// 检测所用图像的像素尺寸。
    pixels: (u32, u32),
    /// 取消标记，工作线程开跑前与完成后各查一次。
    cancel: Arc<AtomicBool>,
    /// 结果通道。
    rx: Receiver<JobResult>,
}

impl AutoFilterJob {
    /// 起一个一次性后台线程做检测。
    ///
    /// # 参数
    /// - `source`：选区在画布上的范围。
    /// - `width` / `height`：图像像素尺寸。
    /// - `rgba`：选区截图（RGBA），所有权转入线程。
    ///
    /// ```
    /// use snow_shot::auto_filter::AutoFilterJob;
    /// let job = AutoFilterJob::spawn([0, 0, 16, 16], 16, 16, vec![255; 16 * 16 * 4]);
    /// assert_eq!(job.source(), [0, 0, 16, 16]);
    /// ```
    pub fn spawn(source: IntRect, width: u32, height: u32, rgba: Vec<u8>) -> Self {
        let cancel = Arc::new(AtomicBool::new(false));
        let (tx, rx) = mpsc::channel();
        let flag = cancel.clone();
        let spawned = std::thread::Builder::new()
            .name("auto-filter-detect".into())
            .spawn({
                let tx = tx.clone();
                move || {
                    if flag.load(Ordering::Acquire) {
                        return;
                    }
                    let result = detect_rgba(width, height, &rgba);
                    if !flag.load(Ordering::Acquire) {
                        let _ = tx.send(result);
                    }
                }
            });
        if let Err(e) = spawned {
            let _ = tx.send(Err(format!("启动检测线程失败: {e}")));
        }
        Self {
            source,
            pixels: (width, height),
            cancel,
            rx,
        }
    }

    /// 任务对应的选区范围。
    pub fn source(&self) -> IntRect {
        self.source
    }

    /// 取消任务：结果将被丢弃（线程不会被强杀，跑完自行退出）。
    pub fn cancel(&self) {
        self.cancel.store(true, Ordering::Release);
    }

    /// 非阻塞取结果；未完成返回 `None`。
    fn try_result(&self) -> Option<JobResult> {
        match self.rx.try_recv() {
            Ok(r) => Some(r),
            Err(TryRecvError::Empty) => None,
            Err(TryRecvError::Disconnected) => Some(Err("检测线程异常退出".into())),
        }
    }
}

impl Drop for AutoFilterJob {
    fn drop(&mut self) {
        self.cancel();
    }
}

/// 会话每一步需要调用方做的事。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionStep {
    /// 什么也不用做。
    Idle,
    /// 清掉旧区域记录（选区取消或换了范围）。
    Clear,
    /// 需要对该选区起一次检测（调用方裁图后调 [`AutoFilterSession::begin`]）；`clear` 为真时同时先清旧记录。
    Detect {
        /// 待检测的选区范围。
        source: IntRect,
        /// 是否先清掉旧记录。
        clear: bool,
    },
}

/// 一次轮询的结果。
#[derive(Debug, PartialEq)]
pub enum SessionPoll {
    /// 还在检测或没有任务。
    Pending,
    /// 检测完成，附带可直接写入引擎的记录。
    Done(Option<AutoFilterRegionRecord>),
    /// 检测失败（说明文案由调用方本地化）。
    Failed(String),
}

/// 自动滤镜会话：决定何时（重新）检测、持有后台任务。
#[derive(Default)]
pub struct AutoFilterSession {
    /// 进行中的任务。
    job: Option<AutoFilterJob>,
    /// 最近一次已处理（已检测、进行中或失败）的选区范围，避免同一范围反复起任务。
    handled: Option<IntRect>,
}

impl AutoFilterSession {
    /// 新建空会话。
    pub fn new() -> Self {
        Self::default()
    }

    /// 是否有任务在跑。
    pub fn is_busy(&self) -> bool {
        self.job.is_some()
    }

    /// 取消进行中的任务（离开工具时调用，已写入的记录保留）。
    pub fn cancel(&mut self) {
        self.job = None;
    }

    /// 完全重置（选区被重置时调用），下次进入工具会重新检测。
    pub fn reset(&mut self) {
        self.job = None;
        self.handled = None;
    }

    /// 决定下一步。
    ///
    /// 规则同旧版 `validate`：已有记录且范围一致不动；范围变了清掉重测；撤销掉记录后不会自动重测
    /// （`force` 为假时同一范围只处理一次），工具激活或按下时 `force` 为真，重新核对。
    ///
    /// # 参数
    /// - `selection`：当前选区；`None` 表示没有选区。
    /// - `recorded`：引擎里现有记录的源范围。
    /// - `force`：是否强制核对（工具激活 / 指针按下）。
    ///
    /// ```
    /// use snow_shot::auto_filter::{AutoFilterSession, SessionStep};
    /// let mut s = AutoFilterSession::new();
    /// let sel = [0, 0, 40, 30];
    /// assert_eq!(s.step(Some(sel), None, true), SessionStep::Detect { source: sel, clear: false });
    /// ```
    pub fn step(
        &mut self,
        selection: Option<IntRect>,
        recorded: Option<IntRect>,
        force: bool,
    ) -> SessionStep {
        let Some(sel) = selection else {
            self.reset();
            return if recorded.is_some() {
                SessionStep::Clear
            } else {
                SessionStep::Idle
            };
        };
        if recorded == Some(sel) {
            self.handled = Some(sel);
            return SessionStep::Idle;
        }
        if self.job.as_ref().is_some_and(|j| j.source() == sel) {
            return SessionStep::Idle;
        }
        if !force && self.handled == Some(sel) {
            return SessionStep::Idle;
        }
        self.job = None;
        self.handled = Some(sel);
        SessionStep::Detect {
            source: sel,
            clear: recorded.is_some(),
        }
    }

    /// 登记调用方起好的后台任务（替换并取消旧任务）。
    pub fn begin(&mut self, job: AutoFilterJob) {
        self.handled = Some(job.source());
        self.job = Some(job);
    }

    /// 轮询任务；完成后任务被消费。
    pub fn poll(&mut self) -> SessionPoll {
        let Some(job) = &self.job else {
            return SessionPoll::Pending;
        };
        let Some(result) = job.try_result() else {
            return SessionPoll::Pending;
        };
        let (source, pixels) = (job.source, job.pixels);
        self.job = None;
        match result {
            Ok(regions) => SessionPoll::Done(regions_to_record(source, pixels, &regions)),
            Err(e) => SessionPoll::Failed(e),
        }
    }
}

/// 拖选与点击的分界（逻辑同引擎 `POINTER_DRAG_THRESHOLD`，画布像素）。
const DRAG_THRESHOLD: f64 = 4.0;

/// 指针当前会命中的区域（同引擎自动滤镜工作流）：位移不足阈值按“点”取最小面积区域，
/// 否则取与起点到当前点矩形相交的全部区域。
///
/// # 参数
/// - `record`：区域记录。
/// - `start`：按下点；悬停（未按下）为 `None`。
/// - `point`：当前指针位置（画布坐标）。
///
/// # 返回
/// 命中区域的外框 `[x0, y0, x1, y1]`，供预览高亮。
///
/// ```
/// use snow_shot::auto_filter::{DetectedRegion, hit_regions, regions_to_record};
/// let r = DetectedRegion { x: 0, y: 0, w: 10, h: 10, category: "text" };
/// let rec = regions_to_record([0, 0, 20, 20], (20, 20), &[r]).unwrap();
/// assert_eq!(hit_regions(&rec, None, (5.0, 5.0)).len(), 1);
/// assert!(hit_regions(&rec, None, (15.0, 15.0)).is_empty());
/// ```
pub fn hit_regions(
    record: &AutoFilterRegionRecord,
    start: Option<(f64, f64)>,
    point: (f64, f64),
) -> Vec<[f64; 4]> {
    let rect = |r: &AutoFilterRegion| {
        [
            r.bounds.min_x,
            r.bounds.min_y,
            r.bounds.max_x,
            r.bounds.max_y,
        ]
    };
    match start {
        Some(s) if (point.0 - s.0).hypot(point.1 - s.1) >= DRAG_THRESHOLD => {
            let (x0, x1) = (s.0.min(point.0), s.0.max(point.0));
            let (y0, y1) = (s.1.min(point.1), s.1.max(point.1));
            record
                .regions
                .iter()
                .filter(|r| {
                    r.bounds.min_x < x1
                        && r.bounds.max_x > x0
                        && r.bounds.min_y < y1
                        && r.bounds.max_y > y0
                })
                .map(rect)
                .collect()
        }
        _ => record
            .region_at(snow_draw_engine::Point::new(point.0, point.1))
            .map(|r| vec![rect(r)])
            .unwrap_or_default(),
    }
}

/// 把引擎记录的源范围取整成 [`IntRect`]。
///
/// # 参数
/// - `record`：引擎里的区域记录。
pub fn record_source(record: &AutoFilterRegionRecord) -> IntRect {
    let b = record.source_bounds;
    [
        b.min_x.round() as i32,
        b.min_y.round() as i32,
        b.max_x.round() as i32,
        b.max_y.round() as i32,
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    /// 合成测试图宽（像素）。
    const SCENE_W: u32 = 640;
    /// 合成测试图高（像素）。
    const SCENE_H: u32 = 400;

    /// 合成测试图：白底，左上一块高频“照片”，下方两行由字形方块拼成的“文字”，右侧一个实心图标。
    fn scene() -> Vec<u8> {
        let w = SCENE_W;
        let mut px = vec![255u8; (w * SCENE_H * 4) as usize];
        let mut put = |x: u32, y: u32, c: [u8; 3]| {
            let i = ((y * w + x) * 4) as usize;
            px[i..i + 3].copy_from_slice(&c);
        };
        for y in 40..140 {
            for x in 40..200 {
                let v = ((x * 37 + y * 91 + (x ^ y) * 13) % 200) as u8 + 20;
                put(x, y, [v, 255 - v, (v / 2) + 60]);
            }
        }
        for line in 0..2u32 {
            let y0 = 200 + line * 26;
            let mut x = 40;
            for glyph in 0..40u32 {
                if glyph % 6 == 5 {
                    x += 14;
                    continue;
                }
                for dy in 0..12 {
                    for dx in 0..7 {
                        if (dx + dy + glyph) % 5 != 0 {
                            put(x + dx, y0 + dy, [30, 30, 30]);
                        }
                    }
                }
                x += 10;
            }
        }
        for y in 60..100 {
            for x in 400..440 {
                put(x, y, [20, 90, 220]);
            }
        }
        px
    }

    /// 合成测试图的整幅选区。
    fn scene_sel() -> IntRect {
        [0, 0, SCENE_W as i32, SCENE_H as i32]
    }

    /// 轮询到任务结束（最多 20 秒）。
    fn wait(session: &mut AutoFilterSession) -> SessionPoll {
        let end = Instant::now() + Duration::from_secs(20);
        loop {
            match session.poll() {
                SessionPoll::Pending if Instant::now() < end => {
                    std::thread::sleep(Duration::from_millis(5));
                }
                other => return other,
            }
        }
    }

    #[test]
    fn categories_match_legacy_order() {
        assert_eq!(
            CATEGORIES,
            [
                "text",
                "text_in_box",
                "image",
                "avatar",
                "icon",
                "message_box",
                "text_block"
            ]
        );
    }

    #[test]
    fn record_scales_clamps_and_numbers_from_one() {
        let regions = [
            DetectedRegion {
                x: 0,
                y: 0,
                w: 50,
                h: 25,
                category: "image",
            },
            DetectedRegion {
                x: 90,
                y: 40,
                w: 30,
                h: 30,
                category: "text",
            },
            DetectedRegion {
                x: 120,
                y: 60,
                w: 5,
                h: 5,
                category: "icon",
            },
        ];
        // 源范围是像素图的 2 倍高宽并偏移 (10, 20)
        let rec = regions_to_record([10, 20, 210, 120], (100, 50), &regions).unwrap();
        assert_eq!(rec.source_bounds, DrawRect::new(10.0, 20.0, 210.0, 120.0));
        assert_eq!(rec.regions.len(), 2, "完全在源范围外的区域被丢弃");
        assert_eq!(rec.regions[0].id, 1);
        assert_eq!(
            rec.regions[0].bounds,
            DrawRect::new(10.0, 20.0, 110.0, 70.0)
        );
        // 第二个区域右 / 下超出，被裁进源范围；编号保持检测序号
        assert_eq!(rec.regions[1].id, 2);
        assert_eq!(
            rec.regions[1].bounds,
            DrawRect::new(190.0, 100.0, 210.0, 120.0)
        );
        assert_eq!(rec.validate(), Ok(()));
    }

    #[test]
    fn record_rejects_empty_source() {
        assert!(regions_to_record([5, 5, 5, 9], (10, 10), &[]).is_none());
        assert!(regions_to_record([0, 0, 9, 9], (0, 10), &[]).is_none());
    }

    #[test]
    fn detect_rejects_bad_buffer_and_blank_is_empty() {
        assert!(detect_rgba(4, 4, &[0; 10]).is_err());
        assert!(
            detect_rgba(64, 64, &vec![200u8; 64 * 64 * 4])
                .unwrap()
                .is_empty()
        );
    }

    /// 区域 JSON 夹具：同一张合成图的检测结果与存档逐值一致（Rust 侧回归夹具）。
    ///
    /// 旧版黄金样本（C++ `snow_detect_visual_regions`）需要本机构建旧版才能导出，目前没有基线；
    /// 检测器本身与 OpenCV 4.12.0 的对拍由其 crate 内夹具负责。
    #[test]
    fn scene_regions_match_json_fixture() {
        let rgba = scene();
        let regions = detect_rgba(SCENE_W, SCENE_H, &rgba).unwrap();
        let actual: Vec<serde_json::Value> = regions
            .iter()
            .map(|r| serde_json::json!({"x": r.x, "y": r.y, "w": r.w, "h": r.h, "category": r.category}))
            .collect();
        if std::env::var_os("SNOW_UPDATE_FIXTURES").is_some() {
            let path = concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/tests/fixtures/auto_filter_scene.json"
            );
            std::fs::write(path, serde_json::to_string_pretty(&actual).unwrap() + "\n").unwrap();
        }
        let expected: Vec<serde_json::Value> =
            serde_json::from_str(include_str!("../tests/fixtures/auto_filter_scene.json")).unwrap();
        assert_eq!(actual, expected);
        assert!(
            !actual.is_empty(),
            "夹具场景必须能检出区域，否则夹具没有意义"
        );
        // 检测结果换算后必须能被引擎校验接受
        let rec = regions_to_record(
            [100, 100, 100 + SCENE_W as i32, 100 + SCENE_H as i32],
            (SCENE_W, SCENE_H),
            &regions,
        )
        .unwrap();
        assert_eq!(rec.validate(), Ok(()));
    }

    #[test]
    fn session_detects_then_goes_quiet() {
        let mut s = AutoFilterSession::new();
        let sel = scene_sel();
        assert_eq!(
            s.step(Some(sel), None, true),
            SessionStep::Detect {
                source: sel,
                clear: false
            }
        );
        s.begin(AutoFilterJob::spawn(sel, SCENE_W, SCENE_H, scene()));
        assert!(s.is_busy());
        // 同一范围进行中不重复起任务
        assert_eq!(s.step(Some(sel), None, true), SessionStep::Idle);
        let SessionPoll::Done(Some(rec)) = wait(&mut s) else {
            panic!("应检测完成");
        };
        assert!(!rec.regions.is_empty());
        assert!(!s.is_busy());
        // 记录一致：不动；记录被撤销：非强制不重测，强制才重测
        assert_eq!(
            s.step(Some(sel), Some(record_source(&rec)), true),
            SessionStep::Idle
        );
        assert_eq!(s.step(Some(sel), None, false), SessionStep::Idle);
        assert_eq!(
            s.step(Some(sel), None, true),
            SessionStep::Detect {
                source: sel,
                clear: false
            }
        );
    }

    #[test]
    fn session_clears_on_range_change_and_selection_loss() {
        let mut s = AutoFilterSession::new();
        let (a, b) = ([0, 0, 50, 50], [0, 0, 60, 60]);
        assert_eq!(
            s.step(Some(b), Some(a), false),
            SessionStep::Detect {
                source: b,
                clear: true
            }
        );
        assert_eq!(s.step(None, Some(a), false), SessionStep::Clear);
        assert_eq!(s.step(None, None, false), SessionStep::Idle);
    }

    #[test]
    fn cancelled_job_result_is_dropped() {
        let mut s = AutoFilterSession::new();
        s.begin(AutoFilterJob::spawn(scene_sel(), SCENE_W, SCENE_H, scene()));
        s.cancel();
        assert!(!s.is_busy());
        assert_eq!(s.poll(), SessionPoll::Pending);
    }

    #[test]
    fn invalid_image_reports_failure() {
        let mut s = AutoFilterSession::new();
        s.begin(AutoFilterJob::spawn([0, 0, 4, 4], 4, 4, vec![0; 3]));
        assert!(matches!(wait(&mut s), SessionPoll::Failed(_)));
    }

    #[test]
    fn kind_ids_round_trip() {
        for k in AutoFilterKind::ALL {
            assert_eq!(AutoFilterKind::from_id(k.id()), Some(k));
        }
    }
}
