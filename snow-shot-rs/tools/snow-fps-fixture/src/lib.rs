//! 副屏专用帧率夹具的纯逻辑部分：参数解析、显示器/区域校验、帧日志与统计。
//!
//! 窗口与 GDI/DWM 相关代码在二进制 `main.rs` / `win.rs` 里，这里不含任何 Win32 调用，便于离线单测。

pub mod content;
pub mod seqbar;

use std::path::PathBuf;

use content::Load;

/// 单次运行的最长秒数（占屏测试每轮 ≤10 秒，留少量余量）。
pub const MAX_SECONDS: f64 = 12.0;
/// 默认运行秒数。
pub const DEFAULT_SECONDS: f64 = 8.0;
/// 帧日志 CSV 表头。
pub const CSV_HEADER: &str = "seq,submit_ns,flush_ns,unix_us";
/// 纳秒每秒。
const NANOS_PER_SEC: f64 = 1e9;

/// 矩形（屏幕坐标，像素）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Rect {
    /// 左上角 x。
    pub x: i32,
    /// 左上角 y。
    pub y: i32,
    /// 宽。
    pub w: u32,
    /// 高。
    pub h: u32,
}

impl Rect {
    /// 本矩形是否完整落在 `outer` 内。
    ///
    /// # 参数
    /// - `outer`：外层矩形。
    ///
    /// # 示例
    /// ```
    /// use snow_fps_fixture::Rect;
    /// let screen = Rect { x: 2560, y: 0, w: 2560, h: 1440 };
    /// assert!(Rect { x: 2560, y: 0, w: 1280, h: 720 }.inside(&screen));
    /// assert!(!Rect { x: 0, y: 0, w: 1280, h: 720 }.inside(&screen));
    /// ```
    pub fn inside(&self, outer: &Rect) -> bool {
        let (x, y) = (i64::from(self.x), i64::from(self.y));
        let (ox, oy) = (i64::from(outer.x), i64::from(outer.y));
        self.w > 0
            && self.h > 0
            && x >= ox
            && y >= oy
            && x + i64::from(self.w) <= ox + i64::from(outer.w)
            && y + i64::from(self.h) <= oy + i64::from(outer.h)
    }
}

/// 枚举到的显示器信息。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MonitorInfo {
    /// 设备名，如 `\\.\DISPLAY2`。
    pub device: String,
    /// 显示器矩形。
    pub rect: Rect,
    /// 是否主屏。
    pub primary: bool,
}

/// 运行模式。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    /// 创建窗口并按 vsync 出帧。
    Run,
    /// 仅列出显示器（不创建窗口）。
    List,
    /// 仅校验目标显示器（不创建窗口），不符退出码非 0。
    Check,
    /// 仅列出 DXGI 输出（适配器序号、输出序号、桌面坐标），不创建窗口。
    DxgiList,
}

/// 命令行选项。
#[derive(Clone, Debug, PartialEq)]
pub struct Options {
    /// 运行模式。
    pub mode: Mode,
    /// 绝对区域 `x,y,w,h`（缺省为整块显示器）。
    pub region: Option<Rect>,
    /// 相对显示器左上角的尺寸 `WxH`（与 `region` 二选一）。
    pub size: Option<(u32, u32)>,
    /// 运行秒数。
    pub seconds: f64,
    /// 每次绘制之间的 vsync 数（1 = 每个刷新周期一帧，2 = 半速）。
    pub divisor: u32,
    /// 背景负载。
    pub load: Load,
    /// 帧日志输出路径。
    pub log: Option<PathBuf>,
    /// 首帧提交后写入的就绪标记文件。
    pub ready: Option<PathBuf>,
    /// 显式允许占用主屏（默认拒绝，仅单屏机器等场景使用）。
    pub allow_primary: bool,
    /// 跨屏模式：窗口区域可横跨多块显示器（须同时给 `--allow-primary` 与 `--region`）。
    pub span: bool,
    /// 双窗口模式：跨屏区域按显示器切成多个窗口，各自按所在屏 vsync 出帧（只在 `--span` 下有效）。
    pub dual: bool,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            mode: Mode::Run,
            region: None,
            size: None,
            seconds: DEFAULT_SECONDS,
            divisor: 1,
            load: Load::Stripes,
            log: None,
            ready: None,
            allow_primary: false,
            span: false,
            dual: false,
        }
    }
}

/// 解析整数列表 `a,b,c,d`。
fn parse_ints<const N: usize>(text: &str) -> Option<[i64; N]> {
    let parts: Vec<i64> = text.split(',').map(|p| p.trim().parse().ok()).collect::<Option<_>>()?;
    parts.try_into().ok()
}

/// 解析命令行。
///
/// # 参数
/// - `args`：不含程序名的参数列表。
///
/// # 返回
/// 选项；非法输入返回中文原因。
///
/// # 示例
/// ```
/// use snow_fps_fixture::{parse_args, Mode};
/// let o = parse_args(&["--seconds".into(), "5".into(), "--divisor".into(), "2".into()]).unwrap();
/// assert_eq!(o.mode, Mode::Run);
/// assert_eq!(o.divisor, 2);
/// assert!(parse_args(&["--seconds".into(), "99".into()]).is_err());
/// ```
pub fn parse_args(args: &[String]) -> Result<Options, String> {
    let mut o = Options::default();
    let mut it = args.iter();
    while let Some(flag) = it.next() {
        let mut value = |name: &str| it.next().cloned().ok_or_else(|| format!("{name} 缺少取值"));
        match flag.as_str() {
            "--list" => o.mode = Mode::List,
            "--check" => o.mode = Mode::Check,
            "--dxgi-list" => o.mode = Mode::DxgiList,
            "--allow-primary" => o.allow_primary = true,
            "--span" => o.span = true,
            "--dual" => o.dual = true,
            "--region" => {
                let [x, y, w, h] = parse_ints::<4>(&value("--region")?).ok_or("--region 需为 x,y,w,h")?;
                let (Ok(x), Ok(y), Ok(w), Ok(h)) =
                    (i32::try_from(x), i32::try_from(y), u32::try_from(w), u32::try_from(h))
                else {
                    return Err("--region 数值越界".into());
                };
                o.region = Some(Rect { x, y, w, h });
            }
            "--size" => {
                let text = value("--size")?;
                let (w, h) = text.split_once(['x', 'X']).ok_or("--size 需为 WxH")?;
                let (Ok(w), Ok(h)) = (w.trim().parse::<u32>(), h.trim().parse::<u32>()) else {
                    return Err("--size 需为 WxH".into());
                };
                o.size = Some((w, h));
            }
            "--seconds" => {
                let s: f64 = value("--seconds")?.parse().map_err(|_| "--seconds 需为数字")?;
                if !(s > 0.0 && s <= MAX_SECONDS) {
                    return Err(format!("--seconds 必须在 (0, {MAX_SECONDS}] 内"));
                }
                o.seconds = s;
            }
            "--divisor" => {
                let d: u32 = value("--divisor")?.parse().map_err(|_| "--divisor 需为整数")?;
                if !(1..=4).contains(&d) {
                    return Err("--divisor 必须在 1..=4".into());
                }
                o.divisor = d;
            }
            "--load" => {
                let text = value("--load")?;
                o.load = Load::parse(&text).ok_or("--load 需为 light|stripes|noise|grid")?;
            }
            "--log" => o.log = Some(PathBuf::from(value("--log")?)),
            "--ready" => o.ready = Some(PathBuf::from(value("--ready")?)),
            other => return Err(format!("未知参数: {other}")),
        }
    }
    if o.region.is_some() && o.size.is_some() {
        return Err("--region 与 --size 不能同时使用".into());
    }
    if o.span && (!o.allow_primary || o.region.is_none()) {
        return Err("--span 必须同时给出 --allow-primary 与 --region".into());
    }
    if o.dual && !o.span {
        return Err("--dual 只能与 --span 同用".into());
    }
    Ok(o)
}

/// 在枚举结果中选出目标显示器（按属性，不看设备名与分辨率）。
///
/// 默认只接受唯一的非主屏；`allow_primary` 为真时改为选主屏（须显式开启）。
///
/// # 参数
/// - `monitors`：枚举到的全部显示器。
/// - `allow_primary`：是否允许占用主屏。
///
/// # 返回
/// 目标显示器；找不到、目标不唯一或主屏未获许可返回原因（调用方应中止）。
///
/// # 示例
/// ```
/// use snow_fps_fixture::{pick_target, MonitorInfo, Rect};
/// let m = |d: &str, x, p| MonitorInfo { device: d.into(), rect: Rect { x, y: 0, w: 2560, h: 1440 }, primary: p };
/// let list = [m(r"\.\DISPLAY2", 0, true), m(r"\.\DISPLAY1", 2560, false)];
/// assert_eq!(pick_target(&list, false).unwrap().rect.x, 2560);
/// assert!(pick_target(&list[..1], false).is_err());
/// assert!(pick_target(&list[..1], true).unwrap().primary);
/// ```
pub fn pick_target(monitors: &[MonitorInfo], allow_primary: bool) -> Result<&MonitorInfo, String> {
    if allow_primary {
        let mut primary = monitors.iter().filter(|m| m.primary);
        let m = primary.next().ok_or_else(|| "枚举不到主屏，中止".to_string())?;
        if primary.next().is_some() {
            return Err("存在多块主屏，无法确定目标，中止".into());
        }
        return Ok(m);
    }
    let mut secondary = monitors.iter().filter(|m| !m.primary);
    let m = secondary
        .next()
        .ok_or_else(|| "没有非主屏（只有一块屏），中止；单屏机器需显式传 --allow-primary".to_string())?;
    if secondary.next().is_some() {
        return Err("存在多块非主屏，无法确定目标，中止".into());
    }
    Ok(m)
}

/// 由选项和目标显示器算出窗口区域，并保证完整落在该显示器内。
///
/// # 参数
/// - `monitor`：目标显示器。
/// - `options`：命令行选项。
///
/// # 返回
/// 窗口矩形；越界返回原因。
pub fn resolve_region(monitor: &MonitorInfo, options: &Options) -> Result<Rect, String> {
    let rect = match (options.region, options.size) {
        (Some(r), _) => r,
        (None, Some((w, h))) => Rect { x: monitor.rect.x, y: monitor.rect.y, w, h },
        (None, None) => monitor.rect,
    };
    if !rect.inside(&monitor.rect) {
        return Err(format!("区域 {rect:?} 越出 {} 的范围 {:?}，中止", monitor.device, monitor.rect));
    }
    Ok(rect)
}

/// 跨屏模式下取窗口区域：必须显式给出，且被所有显示器的并集完整覆盖（不留黑洞）。
///
/// # 参数
/// - `monitors`：枚举到的全部显示器（互不重叠）。
/// - `options`：命令行选项（须含 `region`）。
///
/// # 返回
/// 窗口矩形；未给区域或区域有一部分不在任何显示器上返回原因。
///
/// # 示例
/// ```
/// use snow_fps_fixture::{parse_args, resolve_span_region, MonitorInfo, Rect};
/// let m = |x, p| MonitorInfo { device: "d".into(), rect: Rect { x, y: 0, w: 2560, h: 1440 }, primary: p };
/// let list = [m(0, true), m(2560, false)];
/// let args: Vec<String> = "--span --allow-primary --region 1280,0,2560,1440".split(' ').map(String::from).collect();
/// let o = parse_args(&args).unwrap();
/// assert_eq!(resolve_span_region(&list, &o).unwrap().w, 2560);
/// ```
pub fn resolve_span_region(monitors: &[MonitorInfo], options: &Options) -> Result<Rect, String> {
    let rect = options.region.ok_or_else(|| "跨屏模式必须给 --region".to_string())?;
    let covered: i64 = monitors
        .iter()
        .map(|m| {
            let w = (i64::from(rect.x) + i64::from(rect.w)).min(i64::from(m.rect.x) + i64::from(m.rect.w)) - i64::from(rect.x).max(i64::from(m.rect.x));
            let h = (i64::from(rect.y) + i64::from(rect.h)).min(i64::from(m.rect.y) + i64::from(m.rect.h)) - i64::from(rect.y).max(i64::from(m.rect.y));
            w.max(0) * h.max(0)
        })
        .sum();
    if rect.w == 0 || rect.h == 0 || covered != i64::from(rect.w) * i64::from(rect.h) {
        return Err(format!("区域 {rect:?} 没有被现有显示器完整覆盖，中止"));
    }
    Ok(rect)
}

/// 双窗口模式：按显示器边界把跨屏区域切成左右并排的子矩形（每块屏一个）。
///
/// # 参数
/// - `monitors`：枚举到的全部显示器。
/// - `region`：跨屏区域（调用方已用 [`resolve_span_region`] 校验过覆盖）。
///
/// # 返回
/// 按 x 升序、首尾相接、总宽等于区域宽的子矩形；只落在一块屏上时只有一个元素。
/// 区域在某块屏上高度不完整（上下错位的屏）、显示器重叠或有空洞时返回原因。
///
/// # 示例
/// ```
/// use snow_fps_fixture::{split_region_by_monitors, MonitorInfo, Rect};
/// let m = |x| MonitorInfo { device: "d".into(), rect: Rect { x, y: 0, w: 2560, h: 1440 }, primary: false };
/// let region = Rect { x: 1280, y: 0, w: 2560, h: 1440 };
/// let parts = split_region_by_monitors(&[m(0), m(2560)], region).unwrap();
/// assert_eq!(parts.len(), 2);
/// assert_eq!(parts[0], Rect { x: 1280, y: 0, w: 1280, h: 1440 });
/// ```
pub fn split_region_by_monitors(monitors: &[MonitorInfo], region: Rect) -> Result<Vec<Rect>, String> {
    let (rx0, rx1) = (i64::from(region.x), i64::from(region.x) + i64::from(region.w));
    let (ry0, ry1) = (i64::from(region.y), i64::from(region.y) + i64::from(region.h));
    let mut pieces: Vec<Rect> = Vec::new();
    for m in monitors {
        let (mx, my) = (i64::from(m.rect.x), i64::from(m.rect.y));
        let (x0, x1) = (rx0.max(mx), rx1.min(mx + i64::from(m.rect.w)));
        let (y0, y1) = (ry0.max(my), ry1.min(my + i64::from(m.rect.h)));
        if x1 <= x0 || y1 <= y0 {
            continue;
        }
        if y0 != ry0 || y1 != ry1 {
            return Err(format!("--dual 只支持左右并排的显示器：区域 {region:?} 在 {} 上高度不完整", m.device));
        }
        pieces.push(Rect { x: x0 as i32, y: region.y, w: (x1 - x0) as u32, h: region.h });
    }
    pieces.sort_by_key(|r| r.x);
    let mut cursor = rx0;
    for p in &pieces {
        if i64::from(p.x) != cursor {
            return Err(format!("区域 {region:?} 切分后不连续（显示器重叠或有空洞），中止"));
        }
        cursor += i64::from(p.w);
    }
    if pieces.is_empty() || cursor != rx1 {
        return Err(format!("区域 {region:?} 没有被现有显示器完整覆盖，中止"));
    }
    Ok(pieces)
}

/// 序号条在整个区域中的位置：只由第一块（主节拍）窗口绘制，其余窗口不画。
///
/// # 参数
/// - `pieces`：[`split_region_by_monitors`] 的切分结果（按 x 升序）。
/// - `region`：完整跨屏区域。
///
/// # 返回
/// `(相对区域左边缘的横向偏移, 序号条宽度, 区域总宽)`，单位像素；`pieces` 为空返回 `None`。
///
/// # 示例
/// ```
/// use snow_fps_fixture::{bar_crop, Rect};
/// let region = Rect { x: 1280, y: 0, w: 2560, h: 1440 };
/// let pieces = [Rect { x: 1280, y: 0, w: 1280, h: 1440 }, Rect { x: 2560, y: 0, w: 1280, h: 1440 }];
/// assert_eq!(bar_crop(&pieces, region), Some((0, 1280, 2560)));
/// ```
pub fn bar_crop(pieces: &[Rect], region: Rect) -> Option<(u32, u32, u32)> {
    let lead = pieces.first()?;
    Some(((lead.x - region.x) as u32, lead.w, region.w))
}

/// 双窗口模式下第 `index` 个窗口绘制序号条的范围。
///
/// # 参数
/// - `index`：窗口下标（0 为主节拍）。
/// - `piece`：该窗口的矩形。
///
/// # 返回
/// `(段偏移, 序号条总宽)`；非主节拍窗口返回总宽 0（不画序号条）。
pub fn bar_segment_for(index: usize, piece: Rect) -> (u32, u32) {
    if index == 0 { (0, piece.w) } else { (0, 0) }
}

/// 一帧的提交记录。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FrameRecord {
    /// 帧序号（从 1 起）。
    pub seq: u32,
    /// 提交完成（BitBlt 返回）时刻，相对起点纳秒。
    pub submit_ns: u64,
    /// `DwmFlush` 返回时刻，相对起点纳秒。
    pub flush_ns: u64,
    /// 提交时的 Unix 时间（微秒），用于与录制侧墙钟对账。
    pub unix_us: u64,
}

/// 把帧记录序列化为 CSV（含表头）。
///
/// # 参数
/// - `records`：帧记录。
pub fn frames_to_csv(records: &[FrameRecord]) -> String {
    let mut out = String::with_capacity(records.len() * 40 + CSV_HEADER.len() + 1);
    out.push_str(CSV_HEADER);
    out.push('\n');
    for r in records {
        out.push_str(&format!("{},{},{},{}\n", r.seq, r.submit_ns, r.flush_ns, r.unix_us));
    }
    out
}

/// 出帧统计。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Summary {
    /// 帧数。
    pub frames: usize,
    /// 首末帧提交间隔（秒）。
    pub span_s: f64,
    /// 平均出帧率（帧/秒）。
    pub fps: f64,
    /// 最大相邻帧间隔（毫秒）。
    pub max_gap_ms: f64,
    /// 相邻帧间隔超过 1.5 倍中位数的次数（错过的 vsync）。
    pub late_frames: usize,
}

/// 统计出帧节奏；少于 2 帧返回 `None`。
///
/// # 参数
/// - `records`：帧记录（按提交顺序）。
///
/// # 示例
/// ```
/// use snow_fps_fixture::{summarize, FrameRecord};
/// let rec: Vec<_> = (0..11).map(|i| FrameRecord { seq: i + 1, submit_ns: u64::from(i) * 16_666_667, flush_ns: 0, unix_us: 0 }).collect();
/// let s = summarize(&rec).unwrap();
/// assert!((s.fps - 60.0).abs() < 0.1);
/// ```
pub fn summarize(records: &[FrameRecord]) -> Option<Summary> {
    if records.len() < 2 {
        return None;
    }
    let mut gaps: Vec<f64> = records
        .windows(2)
        .map(|w| w[1].submit_ns.saturating_sub(w[0].submit_ns) as f64 / 1e6)
        .collect();
    let span_s = records[records.len() - 1].submit_ns.saturating_sub(records[0].submit_ns) as f64 / NANOS_PER_SEC;
    let max_gap_ms = gaps.iter().copied().fold(0.0, f64::max);
    let mut sorted = gaps.clone();
    sorted.sort_by(f64::total_cmp);
    let median = sorted[sorted.len() / 2];
    let late_frames = gaps.iter_mut().filter(|g| **g > median * 1.5).count();
    Some(Summary {
        frames: records.len(),
        span_s,
        fps: if span_s > 0.0 { (records.len() - 1) as f64 / span_s } else { 0.0 },
        max_gap_ms,
        late_frames,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 构造测试显示器。
    fn mon(device: &str, x: i32, primary: bool) -> MonitorInfo {
        MonitorInfo { device: device.into(), rect: Rect { x, y: 0, w: 2560, h: 1440 }, primary }
    }

    /// 拆分字符串为参数。
    fn args(s: &str) -> Vec<String> {
        s.split_whitespace().map(String::from).collect()
    }

    /// 默认只选唯一非主屏（不限分辨率）；只有主屏、多块非主屏都拒绝；开关开启才选主屏。
    #[test]
    fn target_selection_rules() {
        let ok = [mon(r"\.\DISPLAY2", 0, true), mon(r"\.\DISPLAY1", 2560, false)];
        assert_eq!(pick_target(&ok, false).map(|m| m.rect.x), Ok(2560));
        // 设备名重新编号后依旧可选
        let renamed = [mon(r"\.\DISPLAY7", 0, true), mon(r"\.\DISPLAY9", 2560, false)];
        assert!(pick_target(&renamed, false).is_ok());
        // 非主屏坐标/分辨率不再写死
        assert!(pick_target(&[mon(r"\.\DISPLAY2", 0, true), mon(r"\.\DISPLAY1", 1920, false)], false).is_ok());
        assert!(pick_target(&ok[..1], false).is_err());
        assert!(pick_target(&[mon("a", 0, true), mon("b", 2560, false), mon("c", 5120, false)], false).is_err());
        assert!(pick_target(&[], false).is_err());
    }

    /// 主屏只有在显式允许时才可选；允许时选主屏而非副屏。
    #[test]
    fn primary_requires_opt_in() {
        let single = [mon("a", 0, true)];
        assert!(pick_target(&single, false).is_err());
        assert!(pick_target(&single, true).unwrap().primary);
        let dual = [mon("a", 0, true), mon("b", 2560, false)];
        assert!(pick_target(&dual, true).unwrap().primary);
        assert!(pick_target(&dual, false).is_ok_and(|m| !m.primary));
        assert!(pick_target(&[mon("b", 2560, false)], true).is_err());
        assert!(parse_args(&args("--check --allow-primary")).unwrap().allow_primary);
        assert!(!parse_args(&args("--check")).unwrap().allow_primary);
    }

    /// 跨屏区域：被两块屏完整覆盖才放行，越出并集或缺开关都拒绝。
    #[test]
    fn span_region_must_be_covered() {
        let list = [mon("a", 0, true), mon("b", 2560, false)];
        let ok = parse_args(&args("--span --allow-primary --region 1280,0,2560,1440")).unwrap();
        assert_eq!(resolve_span_region(&list, &ok).unwrap(), Rect { x: 1280, y: 0, w: 2560, h: 1440 });
        let out = parse_args(&args("--span --allow-primary --region 3000,0,3000,1440")).unwrap();
        assert!(resolve_span_region(&list, &out).is_err());
        assert!(resolve_span_region(&list[..1], &ok).is_err());
        assert!(parse_args(&args("--span --region 0,0,10,10")).is_err());
        assert!(parse_args(&args("--span --allow-primary")).is_err());
    }

    /// 双窗口切分：跨两屏、含负坐标、单屏退化、上下错位/空洞拒绝；`--dual` 必须配 `--span`。
    #[test]
    fn dual_split_rules() {
        let at = |x: i32, y: i32| MonitorInfo { device: "d".into(), rect: Rect { x, y, w: 2560, h: 1440 }, primary: false };
        let region = Rect { x: 1280, y: 0, w: 2560, h: 1440 };
        let two = split_region_by_monitors(&[at(2560, 0), at(0, 0)], region).unwrap();
        assert_eq!(two, vec![Rect { x: 1280, y: 0, w: 1280, h: 1440 }, Rect { x: 2560, y: 0, w: 1280, h: 1440 }]);
        // 负坐标：副屏在主屏左侧
        let neg = split_region_by_monitors(&[at(-2560, 0), at(0, 0)], Rect { x: -1000, y: 0, w: 1500, h: 1440 }).unwrap();
        assert_eq!(neg, vec![Rect { x: -1000, y: 0, w: 1000, h: 1440 }, Rect { x: 0, y: 0, w: 500, h: 1440 }]);
        // 只落在一块屏上：退化为单窗口
        let one = split_region_by_monitors(&[at(0, 0), at(2560, 0)], Rect { x: 100, y: 10, w: 800, h: 600 }).unwrap();
        assert_eq!(one, vec![Rect { x: 100, y: 10, w: 800, h: 600 }]);
        // 上下错位的屏、有空洞、完全不在屏上都拒绝
        assert!(split_region_by_monitors(&[at(0, 0), at(2560, 500)], region).is_err());
        assert!(split_region_by_monitors(&[at(0, 0), at(3000, 0)], region).is_err());
        assert!(split_region_by_monitors(&[at(0, 0)], Rect { x: 5000, y: 0, w: 10, h: 10 }).is_err());
        assert!(parse_args(&args("--span --dual --allow-primary --region 0,0,10,10")).unwrap().dual);
        assert!(parse_args(&args("--dual --allow-primary --region 0,0,10,10")).is_err());
    }

    /// 序号条只由主节拍窗口绘制：分析区域落在主窗口内，其余窗口总宽为 0 不画。
    #[test]
    fn bar_drawn_by_leader_only() {
        let region = Rect { x: 1280, y: 0, w: 2560, h: 1440 };
        let pieces = [Rect { x: 1280, y: 0, w: 1280, h: 1440 }, Rect { x: 2560, y: 0, w: 1280, h: 1440 }];
        assert_eq!(bar_crop(&pieces, region), Some((0, 1280, 2560)));
        assert_eq!(bar_segment_for(0, pieces[0]), (0, 1280));
        assert_eq!(bar_segment_for(1, pieces[1]).1, 0);
        // 分析区域不越出主窗口，也不与其它窗口重叠
        let (x, w, _) = bar_crop(&pieces, region).unwrap();
        assert!(x + w <= (pieces[1].x - region.x) as u32);
        assert_eq!(bar_crop(&[], region), None);
    }

    /// 区域必须落在目标屏内；默认整屏；size 锚定左上角。
    #[test]
    fn region_must_stay_inside() {
        let m = mon(r"\\.\DISPLAY2", 2560, false);
        let full = resolve_region(&m, &Options::default()).unwrap();
        assert_eq!(full, m.rect);
        let sized = parse_args(&args("--size 1280x720")).unwrap();
        assert_eq!(resolve_region(&m, &sized).unwrap(), Rect { x: 2560, y: 0, w: 1280, h: 720 });
        let out = parse_args(&args("--region 0,0,1280,720")).unwrap();
        assert!(resolve_region(&m, &out).is_err());
        let edge = parse_args(&args("--region 3840,720,1281,720")).unwrap();
        assert!(resolve_region(&m, &edge).is_err());
        let zero = parse_args(&args("--region 2560,0,0,10")).unwrap();
        assert!(resolve_region(&m, &zero).is_err());
    }

    /// 参数解析：合法、非法与互斥。
    #[test]
    fn argument_parsing() {
        let o = parse_args(&args("--seconds 6 --divisor 2 --load noise --log a.csv --ready r.txt")).unwrap();
        assert_eq!((o.seconds, o.divisor, o.load), (6.0, 2, Load::Noise));
        assert_eq!(o.log, Some(PathBuf::from("a.csv")));
        assert_eq!(parse_args(&args("--check")).unwrap().mode, Mode::Check);
        for bad in ["--seconds 0", "--seconds 13", "--seconds x", "--divisor 0", "--divisor 5", "--load x", "--size 12", "--region 1,2,3", "--bogus", "--seconds", "--region 1,2,3,4 --size 1x1"] {
            assert!(parse_args(&args(bad)).is_err(), "{bad}");
        }
    }

    /// CSV 含表头与逐行记录。
    #[test]
    fn csv_output() {
        let csv = frames_to_csv(&[FrameRecord { seq: 1, submit_ns: 10, flush_ns: 20, unix_us: 30 }]);
        assert_eq!(csv, format!("{CSV_HEADER}\n1,10,20,30\n"));
    }

    /// 统计：均匀 60fps、缺帧与不足两帧。
    #[test]
    fn summary_math() {
        let mk = |ns: &[u64]| -> Vec<FrameRecord> {
            ns.iter().enumerate().map(|(i, &n)| FrameRecord { seq: i as u32 + 1, submit_ns: n, flush_ns: n, unix_us: 0 }).collect()
        };
        let even: Vec<u64> = (0..61).map(|i| i * 16_666_667).collect();
        let s = summarize(&mk(&even)).unwrap();
        assert!((s.fps - 60.0).abs() < 0.01 && s.late_frames == 0);
        let mut gappy = even.clone();
        for v in gappy.iter_mut().skip(30) {
            *v += 16_666_667;
        }
        let s = summarize(&mk(&gappy)).unwrap();
        assert_eq!(s.late_frames, 1);
        assert!(s.max_gap_ms > 33.0);
        assert!(summarize(&mk(&[1])).is_none());
    }
}
