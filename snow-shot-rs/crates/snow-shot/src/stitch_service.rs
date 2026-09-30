//! 滚动截图拼接适配层（Scrolling Stitch Adapter）。
//!
//! 复用主仓库 `snow-stitch-images`（ORB 特征 + 区块分类的位移估计），本层只做三件事：
//! 1. 把匹配结果“说出来”：开启决策记录，每帧都返回 [`FrameOutcome`]，失败不再静默；
//! 2. 守住资源边界：画布高度上限、不克隆输入帧、分块导出（避免 `finish()` 的双份峰值）；
//! 3. 提供重复帧指纹，采集线程可在送入拼接前就丢掉静止帧。

use snow_platform::capture::CapturedScreen;
use snow_stitch_images::{
    Frame, MotionOutcome, MotionStage, PixelFormat, StitchBranch, StitchDecision, StitchOptions,
    Stitcher,
};

/// 每像素字节数（BGRA / RGBA）。
pub const BYTES_PER_PIXEL: usize = 4;
/// 画布高度上限（行）；到限后不再接收新帧。
pub const MAX_CANVAS_ROWS: u32 = 32768;
/// 分块导出时每块的字节数上限（限制单次拷贝与 PNG 编码的峰值内存）。
pub const EXPORT_PART_BYTES: usize = 32 * 1024 * 1024;
/// 分块导出时每块的最少行数（避免宽图被切得过碎）。
pub const EXPORT_PART_MIN_ROWS: u32 = 256;
/// 分块导出时每块的最大行数（限制单张 PNG 的高度）。
pub const EXPORT_PART_ROWS: u32 = 16384;
/// 库默认的单次最大位移占帧高比例（超过则该帧被库拒绝）。
pub const MAX_MOTION_RATIO: f32 = 0.6;
/// 建议的单次滚动占帧高比例上限（留出余量，UI 提示用）。
pub const RECOMMENDED_STEP_RATIO: f32 = 0.5;
/// 连续被拒绝多少帧后向用户提示。
pub const REJECT_WARN_STREAK: u32 = 3;
/// 帧指纹的乘法常数（64 位混合）。
const FINGERPRINT_MULTIPLIER: u64 = 0x9E37_79B1_85EB_CA87;
/// 帧指纹的初始种子。
const FINGERPRINT_SEED: u64 = 0xCBF2_9CE4_8422_2325;

/// 帧被拒绝（未拼入）的原因。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RejectReason {
    /// 画面几乎没有可用特征（纯色 / 低纹理）。
    NoFeatures,
    /// 特征点无法互相匹配（内容不连续）。
    NoMatches,
    /// 匹配置信度不足（重复纹理、动画干扰等）。
    LowConfidence,
    /// 画面整体跳变（页面切换）。
    SceneCut,
    /// 单次位移超过库允许的比例（滚太快）。
    TooFast {
        /// 估计的位移（行，带符号）。
        offset: i32,
    },
    /// 其它未归类的失败。
    Unknown,
}

impl RejectReason {
    /// 面向用户的简短提示。
    ///
    /// # 返回
    /// 一句中文提示。
    ///
    /// ```ignore
    /// assert!(RejectReason::TooFast { offset: 900 }.hint().contains("太快"));
    /// ```
    pub fn hint(&self) -> &'static str {
        match self {
            Self::NoFeatures => "画面内容太少，无法对齐，请换一个有文字或图案的区域",
            Self::NoMatches => "前后画面对不上，请放慢滚动",
            Self::LowConfidence => "对齐把握不足（重复纹理或动画），请放慢滚动",
            Self::SceneCut => "画面整体变化，可能切换了页面",
            Self::TooFast { .. } => "滚动太快，请放慢（每次不超过半屏）",
            Self::Unknown => "该帧未能拼入",
        }
    }
}

/// 一帧送入拼接后的处理结果。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FrameOutcome {
    /// 首帧，已作为画布起点。
    Started {
        /// 当前画布高度（行）。
        height: u32,
    },
    /// 向下追加了新内容。
    Appended {
        /// 本帧新增的行数。
        growth: u32,
        /// 追加后的画布高度。
        height: u32,
        /// 库估计的位移（行，带符号）。
        offset: i32,
    },
    /// 向上前置了新内容（用户反向滚动）。
    Prepended {
        /// 本帧新增的行数。
        growth: u32,
        /// 前置后的画布高度。
        height: u32,
        /// 库估计的位移（行，带符号）。
        offset: i32,
    },
    /// 新帧内容已被画布包含（往回滚动到已拼过的区域）。
    Contained {
        /// 库估计的位移（行，带符号）。
        offset: i32,
    },
    /// 与上一帧逐字节相同，未送入库。
    Duplicate,
    /// 库判定无位移（停止滚动或位移过小）。
    NoChange,
    /// 匹配失败，帧未拼入。
    Rejected(RejectReason),
    /// 画布已到高度上限，帧未拼入。
    LimitReached {
        /// 当前画布高度。
        height: u32,
    },
}

impl FrameOutcome {
    /// 这一帧是否让画布内容有了进展（含首帧、追加、前置、被包含）。
    pub fn is_progress(&self) -> bool {
        matches!(
            self,
            Self::Started { .. }
                | Self::Appended { .. }
                | Self::Prepended { .. }
                | Self::Contained { .. }
        )
    }
}

/// 累计统计（探针与验收用）。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct StitchStats {
    /// 送入的帧数（含重复与被拒）。
    pub input: u32,
    /// 追加次数。
    pub appended: u32,
    /// 前置次数。
    pub prepended: u32,
    /// 被包含次数。
    pub contained: u32,
    /// 重复帧次数。
    pub duplicates: u32,
    /// 无位移次数。
    pub no_change: u32,
    /// 被拒绝次数。
    pub rejected: u32,
    /// 因高度上限未拼入的次数。
    pub limit_hits: u32,
}

/// 计算一帧像素的 64 位指纹（8 字节一组的乘法混合，速度约内存带宽量级）。
///
/// # 参数
/// - `bytes`：帧像素。
///
/// # 返回
/// 指纹；内容不同时冲突概率可忽略（仅用于“是否与上一帧相同”的快速判断）。
///
/// ```ignore
/// assert_eq!(frame_fingerprint(&[1, 2, 3]), frame_fingerprint(&[1, 2, 3]));
/// assert_ne!(frame_fingerprint(&[1, 2, 3]), frame_fingerprint(&[1, 2, 4]));
/// ```
pub fn frame_fingerprint(bytes: &[u8]) -> u64 {
    let mut hash = FINGERPRINT_SEED ^ bytes.len() as u64;
    let mut chunks = bytes.chunks_exact(8);
    for chunk in &mut chunks {
        let mut word = [0u8; 8];
        word.copy_from_slice(chunk);
        hash = (hash ^ u64::from_le_bytes(word))
            .wrapping_mul(FINGERPRINT_MULTIPLIER)
            .rotate_left(29);
    }
    for &byte in chunks.remainder() {
        hash = (hash ^ u64::from(byte)).wrapping_mul(FINGERPRINT_MULTIPLIER);
    }
    hash ^ (hash >> 32)
}

/// 把库的一条决策翻译成对外的帧结果。
///
/// # 参数
/// - `decision`：`record_decisions` 记录下来的决策。
///
/// # 返回
/// 对应的 [`FrameOutcome`]（不含高度上限与首帧，那两类由调用方处理）。
pub fn classify_decision(decision: &StitchDecision) -> FrameOutcome {
    let height = decision.after.canvas_height;
    let offset = decision.accepted_offset.unwrap_or(0);
    match decision.branch {
        StitchBranch::Skip => FrameOutcome::Duplicate,
        StitchBranch::Append => FrameOutcome::Appended {
            growth: decision.growth,
            height,
            offset,
        },
        StitchBranch::Prepend => FrameOutcome::Prepended {
            growth: decision.growth,
            height,
            offset,
        },
        StitchBranch::Contained => FrameOutcome::Contained { offset },
        StitchBranch::NoMovement => classify_rejection(decision),
    }
}

/// 翻译“未推进”的决策：区分无位移、滚太快与各类匹配失败。
fn classify_rejection(decision: &StitchDecision) -> FrameOutcome {
    if let Some(MotionOutcome::Motion { offset }) = decision.motion {
        return FrameOutcome::Rejected(RejectReason::TooFast { offset });
    }
    if decision.motion == Some(MotionOutcome::NoMotion) {
        return FrameOutcome::NoChange;
    }
    let stage = decision.motion_diagnostics.as_ref().map(|d| d.stage);
    match stage {
        Some(MotionStage::IdenticalInterior | MotionStage::SelectedNoMotion) => {
            FrameOutcome::NoChange
        }
        Some(MotionStage::EmptyDescriptors) => FrameOutcome::Rejected(RejectReason::NoFeatures),
        Some(MotionStage::NoMatches | MotionStage::NoCandidates) => {
            FrameOutcome::Rejected(RejectReason::NoMatches)
        }
        Some(MotionStage::LowConfidence) => FrameOutcome::Rejected(RejectReason::LowConfidence),
        Some(MotionStage::SceneCut) => FrameOutcome::Rejected(RejectReason::SceneCut),
        _ => FrameOutcome::Rejected(RejectReason::Unknown),
    }
}

/// 把总高度按 `max_rows` 切成若干段（起始行, 行数）。
///
/// # 参数
/// - `total`：总行数。
/// - `max_rows`：每段最大行数（0 视为不分段）。
///
/// # 返回
/// 依次覆盖 `0..total` 的分段；`total` 为 0 返回空。
///
/// ```ignore
/// assert_eq!(plan_parts(10, 4), vec![(0, 4), (4, 4), (8, 2)]);
/// ```
pub fn plan_parts(total: u32, max_rows: u32) -> Vec<(u32, u32)> {
    if total == 0 {
        return Vec::new();
    }
    if max_rows == 0 {
        return vec![(0, total)];
    }
    let mut parts = Vec::new();
    let mut top = 0;
    while top < total {
        let rows = max_rows.min(total - top);
        parts.push((top, rows));
        top += rows;
    }
    parts
}

/// 按宽度计算分块导出每块的行数：每块不超过 [`EXPORT_PART_BYTES`]，并夹在 [`EXPORT_PART_MIN_ROWS`, `EXPORT_PART_ROWS`] 内。
///
/// # 参数
/// - `width`：图像宽度（像素）。
///
/// ```ignore
/// assert_eq!(export_part_rows(1920), 4369);
/// ```
pub fn export_part_rows(width: u32) -> u32 {
    let row_bytes = (width as usize).saturating_mul(BYTES_PER_PIXEL).max(1);
    let rows = (EXPORT_PART_BYTES / row_bytes).min(EXPORT_PART_ROWS as usize) as u32;
    rows.max(EXPORT_PART_MIN_ROWS)
}

/// 原地对调每个 4 字节像素的第 0 与第 2 字节（BGRA 与 RGBA 互转）。
///
/// # 参数
/// - `pixels`：4 字节像素缓冲（长度非 4 的倍数时忽略尾部）。
///
/// ```ignore
/// let mut px = [1u8, 2, 3, 4];
/// swap_red_blue(&mut px);
/// assert_eq!(px, [3, 2, 1, 4]);
/// ```
pub fn swap_red_blue(pixels: &mut [u8]) {
    for pixel in pixels.chunks_exact_mut(BYTES_PER_PIXEL) {
        pixel.swap(0, 2);
    }
}

/// 计算 BGRA/RGBA 缓冲字节数；乘法溢出返回 `None`。
fn frame_len(width: u32, height: u32) -> Option<usize> {
    (width as usize)
        .checked_mul(height as usize)?
        .checked_mul(BYTES_PER_PIXEL)
}

/// 滚动截图拼接服务（适配层）。
pub struct StitchService {
    /// 库的增量拼接器（首帧到来时才创建）。
    stitcher: Option<Stitcher>,
    /// 画布宽度（首帧决定）。
    width: u32,
    /// 单帧高度（首帧决定）。
    frame_height: u32,
    /// 当前画布高度。
    height: u32,
    /// 画布高度上限。
    max_height: u32,
    /// 上一输入帧的指纹。
    last_fingerprint: Option<u64>,
    /// 累计统计。
    stats: StitchStats,
    /// 连续被拒绝的帧数。
    reject_streak: u32,
    /// 最近一次被拒绝的原因。
    last_reject: Option<RejectReason>,
}

impl Default for StitchService {
    /// 默认服务（高度上限 [`MAX_CANVAS_ROWS`]）。
    fn default() -> Self {
        Self::new()
    }
}

impl StitchService {
    /// 创建拼接服务。
    ///
    /// # 示例
    /// ```ignore
    /// let svc = StitchService::new();
    /// assert_eq!(svc.height(), 0);
    /// ```
    pub fn new() -> Self {
        Self::with_max_height(MAX_CANVAS_ROWS)
    }

    /// 指定画布高度上限创建服务。
    ///
    /// # 参数
    /// - `max_height`：画布高度上限（行）。
    pub fn with_max_height(max_height: u32) -> Self {
        Self {
            stitcher: None,
            width: 0,
            frame_height: 0,
            height: 0,
            max_height,
            last_fingerprint: None,
            stats: StitchStats::default(),
            reject_streak: 0,
            last_reject: None,
        }
    }

    /// 送入一帧 RGBA 像素；像素缓冲被移动，不会被克隆。
    ///
    /// 内部与导出一律是 RGBA；采集得到的 BGRA 请走 [`StitchService::push_captured`]。
    ///
    /// # 参数
    /// - `width` / `height`：帧尺寸（须与首帧一致，均为物理像素）。
    /// - `pixels`：紧凑排列的 4 字节像素，长度须为 `宽 * 高 * 4`。
    ///
    /// # 返回
    /// 这一帧的处理结果；尺寸非法、与首帧不一致或库报硬错误时返回 `Err`。
    ///
    /// ```ignore
    /// let outcome = svc.push_frame(w, h, bgra)?;
    /// if let FrameOutcome::Rejected(reason) = outcome { println!("{}", reason.hint()); }
    /// ```
    pub fn push_frame(
        &mut self,
        width: u32,
        height: u32,
        pixels: Vec<u8>,
    ) -> Result<FrameOutcome, String> {
        if frame_len(width, height) != Some(pixels.len()) || width == 0 || height == 0 {
            return Err("切片图像尺寸非法或数据不完整".to_string());
        }
        self.stats.input += 1;
        let fingerprint = frame_fingerprint(&pixels);
        if self.stitcher.is_some() {
            if width != self.width || height != self.frame_height {
                return Err(format!(
                    "切片尺寸与首帧不一致: 期望 {}x{} 实际 {width}x{height}",
                    self.width, self.frame_height
                ));
            }
            if self.last_fingerprint == Some(fingerprint) {
                self.stats.duplicates += 1;
                return Ok(FrameOutcome::Duplicate);
            }
            if self.would_exceed_limit() {
                self.last_fingerprint = Some(fingerprint);
                self.stats.limit_hits += 1;
                return Ok(FrameOutcome::LimitReached {
                    height: self.height,
                });
            }
        }
        let frame = Frame::new(width, height, PixelFormat::Rgba8, pixels)
            .map_err(|e| format!("构造帧失败: {e}"))?;
        self.last_fingerprint = Some(fingerprint);
        self.push_to_library(frame)
    }

    /// 送入采集帧（BGRA）：接管其像素缓冲，原地把 R/B 对调成 RGBA 再交给库。
    ///
    /// 之所以要对调：库按 RGBA 计算亮度（R 权重 77、B 权重 29），直接把 BGRA 当 RGBA 喂会让
    /// “只有红色有纹理”的内容对比度只剩三分之一而被判为无特征（见 D3 用例 3-R）。
    ///
    /// # 参数
    /// - `screen`：采集结果（BGRA，紧凑排列）。
    ///
    /// # 返回
    /// 同 [`StitchService::push_frame`]。
    pub fn push_captured(&mut self, mut screen: CapturedScreen) -> Result<FrameOutcome, String> {
        swap_red_blue(&mut screen.data);
        self.push_frame(screen.width, screen.height, screen.data)
    }

    /// 是否已到达高度上限（再来一帧最坏情况会超限）。
    fn would_exceed_limit(&self) -> bool {
        let worst_growth = (self.frame_height as f32 * MAX_MOTION_RATIO).ceil() as u32 + 1;
        self.height.saturating_add(worst_growth) > self.max_height
    }

    /// 把帧交给库并翻译结果。
    fn push_to_library(&mut self, frame: Frame) -> Result<FrameOutcome, String> {
        let first = self.stitcher.is_none();
        if first {
            let options = StitchOptions {
                record_decisions: true,
                ..StitchOptions::default()
            };
            self.stitcher = Some(Stitcher::new(options).map_err(|e| format!("创建拼接器失败: {e}"))?);
            self.width = frame.width();
            self.frame_height = frame.height();
        }
        let Some(stitcher) = self.stitcher.as_mut() else {
            return Err("拼接器未初始化".to_string());
        };
        let decision = stitcher
            .push(frame)
            .map_err(|e| format!("拼接失败: {e}"))?;
        stitcher.clear_decisions();
        if let Some((_, canvas_height)) = stitcher.image_dimensions() {
            self.height = canvas_height;
        }
        if first {
            return Ok(FrameOutcome::Started {
                height: self.height,
            });
        }
        let outcome = match decision {
            Some(decision) => classify_decision(&decision),
            None => FrameOutcome::Rejected(RejectReason::Unknown),
        };
        self.record(outcome);
        Ok(outcome)
    }

    /// 更新统计与连续拒绝计数。
    fn record(&mut self, outcome: FrameOutcome) {
        match outcome {
            FrameOutcome::Appended { .. } => self.stats.appended += 1,
            FrameOutcome::Prepended { .. } => self.stats.prepended += 1,
            FrameOutcome::Contained { .. } => self.stats.contained += 1,
            FrameOutcome::NoChange => self.stats.no_change += 1,
            FrameOutcome::Rejected(_) => self.stats.rejected += 1,
            _ => {}
        }
        match outcome {
            FrameOutcome::Rejected(reason) => {
                self.reject_streak += 1;
                self.last_reject = Some(reason);
            }
            outcome if outcome.is_progress() => self.reject_streak = 0,
            _ => {}
        }
    }

    /// 画布宽度（尚无首帧为 0）。
    pub fn width(&self) -> u32 {
        self.width
    }

    /// 当前画布高度（尚无首帧为 0）。
    pub fn height(&self) -> u32 {
        self.height
    }

    /// 是否还没有任何帧。
    pub fn is_empty(&self) -> bool {
        self.stitcher.is_none()
    }

    /// 累计统计。
    pub fn stats(&self) -> StitchStats {
        self.stats
    }

    /// 画布高度上限。
    pub fn max_height(&self) -> u32 {
        self.max_height
    }

    /// 连续被拒绝达到阈值时，返回最近一次的原因（用于提示用户）。
    ///
    /// # 返回
    /// 需要提示时返回原因，否则 `None`。
    pub fn attention(&self) -> Option<RejectReason> {
        if self.reject_streak >= REJECT_WARN_STREAK {
            self.last_reject
        } else {
            None
        }
    }

    /// 导出画布的一段行（字节顺序与送入一致，紧凑排列）。
    ///
    /// # 参数
    /// - `top`：起始行。
    /// - `rows`：行数（须大于 0，且 `top + rows` 不超过画布高度）。
    ///
    /// # 返回
    /// `rows * 宽 * 4` 字节；范围非法或库报错返回 `Err`。
    pub fn export_rows(&self, top: u32, rows: u32) -> Result<Vec<u8>, String> {
        let stitcher = self.stitcher.as_ref().ok_or("尚无可导出的内容")?;
        let end = top.checked_add(rows).ok_or("导出范围溢出")?;
        if rows == 0 || end > self.height {
            return Err(format!("导出范围 {top}..{end} 超出画布高度 {}", self.height));
        }
        let len = frame_len(self.width, rows).ok_or("导出缓冲大小溢出")?;
        let mut out = vec![0u8; len];
        stitcher
            .copy_rows(top, rows, &mut out)
            .map_err(|e| format!("导出失败: {e}"))?;
        Ok(out)
    }

    /// 导出整张画布（只多一份连续副本，不走库的 `finish()`）。
    ///
    /// # 返回
    /// `(宽, 高, RGBA 像素)`；空画布返回 `Err`。
    pub fn export_all_rgba(&self) -> Result<(u32, u32, Vec<u8>), String> {
        let data = self.export_rows(0, self.height)?;
        Ok((self.width, self.height, data))
    }

    /// 按内存上限规划分块导出范围（见 [`export_part_rows`]）。
    pub fn export_plan(&self) -> Vec<(u32, u32)> {
        plan_parts(self.height, export_part_rows(self.width))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use snow_stitch_images::{MotionDiagnostics, ReferenceMode, RegionDiagnostics, StitchProgressState};

    /// 生成带哈希噪声的“文档”某行某列的 BGRA 像素。
    fn doc_pixel(x: u32, y: u32) -> [u8; 4] {
        let mut hash = x.wrapping_mul(0xc2b2_ae35) ^ y.wrapping_mul(0x27d4_eb2d);
        hash ^= hash >> 16;
        hash = hash.wrapping_mul(0x7feb_352d);
        hash ^= hash >> 15;
        [(hash >> 24) as u8, (hash >> 16) as u8, (hash >> 8) as u8, 255]
    }

    /// 截取文档第 `scroll` 行起、高 `h` 的一帧。
    fn frame_at(w: u32, h: u32, scroll: u32) -> Vec<u8> {
        let mut out = Vec::with_capacity((w * h * 4) as usize);
        for y in 0..h {
            for x in 0..w {
                out.extend_from_slice(&doc_pixel(x, y + scroll));
            }
        }
        out
    }

    /// 构造一条决策（测试翻译逻辑用）。
    fn decision(
        branch: StitchBranch,
        motion: Option<MotionOutcome>,
        stage: Option<MotionStage>,
        offset: Option<i32>,
    ) -> StitchDecision {
        let state = StitchProgressState {
            viewport_position: 0,
            max_viewport_position: 0,
            canvas_height: 640,
            processed_count: 1,
            accepted_count: 1,
        };
        StitchDecision {
            input_index: 1,
            previous_raw_index: 0,
            exact_duplicate: branch == StitchBranch::Skip,
            reference_mode: ReferenceMode::Synthetic,
            motion,
            confidence: Some(0.9),
            accepted_offset: offset,
            branch,
            before: state,
            after: state,
            growth: 40,
            canvas_band_height: None,
            synthetic_reference_band_height: None,
            motion_diagnostics: stage.map(|stage| MotionDiagnostics {
                stage,
                reference_keypoints: 0,
                incoming_keypoints: 0,
                mutual_matches: 0,
                direct_similarity: 0.0,
                selected_offset: None,
                candidates: Vec::new(),
                regions: RegionDiagnostics::default(),
            }),
        }
    }

    /// 指纹：相同内容一致，任意一字节不同即不同。
    #[test]
    fn fingerprint_detects_single_byte_change() {
        let a = frame_at(64, 32, 0);
        let mut b = a.clone();
        assert_eq!(frame_fingerprint(&a), frame_fingerprint(&b));
        let last = b.len() - 1;
        b[last] ^= 1;
        assert_ne!(frame_fingerprint(&a), frame_fingerprint(&b));
        assert_ne!(frame_fingerprint(&[]), frame_fingerprint(&[0]));
    }

    /// 分块行数：随宽度反比、夹在上下限内，且每块不超过字节上限。
    #[test]
    fn part_rows_follow_width_and_bounds() {
        assert_eq!(export_part_rows(1920), 4369);
        assert_eq!(export_part_rows(1), EXPORT_PART_ROWS);
        assert_eq!(export_part_rows(100_000), EXPORT_PART_MIN_ROWS);
        assert_eq!(export_part_rows(0), EXPORT_PART_ROWS);
        for w in [320u32, 1280, 1920, 3840, 7680] {
            let rows = export_part_rows(w);
            assert!((EXPORT_PART_MIN_ROWS..=EXPORT_PART_ROWS).contains(&rows));
            assert!(rows as usize * w as usize * 4 <= EXPORT_PART_BYTES || rows == EXPORT_PART_MIN_ROWS);
        }
    }

    /// 分块规划：整除、余数、零与越界参数。
    #[test]
    fn plan_parts_covers_all_rows() {
        assert_eq!(plan_parts(10, 4), vec![(0, 4), (4, 4), (8, 2)]);
        assert_eq!(plan_parts(8, 4), vec![(0, 4), (4, 4)]);
        assert_eq!(plan_parts(3, 100), vec![(0, 3)]);
        assert_eq!(plan_parts(0, 4), Vec::<(u32, u32)>::new());
        assert_eq!(plan_parts(5, 0), vec![(0, 5)]);
    }

    /// 决策翻译：各分支与拒绝原因。
    #[test]
    fn classify_maps_branches_and_reasons() {
        let d = decision(StitchBranch::Skip, None, None, None);
        assert_eq!(classify_decision(&d), FrameOutcome::Duplicate);
        let d = decision(StitchBranch::Append, Some(MotionOutcome::Motion { offset: -40 }), None, Some(-40));
        assert_eq!(
            classify_decision(&d),
            FrameOutcome::Appended { growth: 40, height: 640, offset: -40 }
        );
        let d = decision(StitchBranch::Prepend, None, None, Some(40));
        assert!(matches!(classify_decision(&d), FrameOutcome::Prepended { .. }));
        let d = decision(StitchBranch::Contained, None, None, Some(30));
        assert_eq!(classify_decision(&d), FrameOutcome::Contained { offset: 30 });
        let d = decision(StitchBranch::NoMovement, Some(MotionOutcome::Motion { offset: 700 }), None, None);
        assert_eq!(
            classify_decision(&d),
            FrameOutcome::Rejected(RejectReason::TooFast { offset: 700 })
        );
        let d = decision(StitchBranch::NoMovement, Some(MotionOutcome::NoMotion), None, None);
        assert_eq!(classify_decision(&d), FrameOutcome::NoChange);
        for (stage, expected) in [
            (MotionStage::EmptyDescriptors, RejectReason::NoFeatures),
            (MotionStage::NoMatches, RejectReason::NoMatches),
            (MotionStage::NoCandidates, RejectReason::NoMatches),
            (MotionStage::LowConfidence, RejectReason::LowConfidence),
            (MotionStage::SceneCut, RejectReason::SceneCut),
            (MotionStage::InputTooSmall, RejectReason::Unknown),
        ] {
            let d = decision(StitchBranch::NoMovement, Some(MotionOutcome::Indeterminate), Some(stage), None);
            assert_eq!(classify_decision(&d), FrameOutcome::Rejected(expected), "{stage:?}");
        }
        let d = decision(StitchBranch::NoMovement, Some(MotionOutcome::Indeterminate), None, None);
        assert_eq!(classify_decision(&d), FrameOutcome::Rejected(RejectReason::Unknown));
    }

    /// 进展判定与提示文案非空。
    #[test]
    fn outcome_progress_and_hints() {
        assert!(FrameOutcome::Started { height: 1 }.is_progress());
        assert!(FrameOutcome::Contained { offset: 1 }.is_progress());
        assert!(!FrameOutcome::Duplicate.is_progress());
        assert!(!FrameOutcome::NoChange.is_progress());
        assert!(!FrameOutcome::Rejected(RejectReason::Unknown).is_progress());
        for r in [
            RejectReason::NoFeatures,
            RejectReason::NoMatches,
            RejectReason::LowConfidence,
            RejectReason::SceneCut,
            RejectReason::TooFast { offset: 1 },
            RejectReason::Unknown,
        ] {
            assert!(!r.hint().is_empty());
        }
    }

    /// 非法输入：零尺寸、长度不符、与首帧尺寸不一致都返回错误而不是 panic。
    #[test]
    fn invalid_frames_are_errors() {
        let mut svc = StitchService::new();
        assert!(svc.push_frame(0, 10, Vec::new()).is_err());
        assert!(svc.push_frame(10, 10, vec![0; 10]).is_err());
        assert!(svc.push_frame(u32::MAX, u32::MAX, vec![0; 4]).is_err());
        assert!(svc.push_frame(64, 64, frame_at(64, 64, 0)).is_ok());
        assert!(svc.push_frame(32, 64, frame_at(32, 64, 0)).is_err());
        assert!(svc.export_rows(0, 0).is_err());
        assert!(svc.export_rows(0, 65).is_err());
        assert!(StitchService::new().export_all_rgba().is_err());
    }

    /// 端到端小样：三帧慢滚，结果逐字节等于文档前缀，重复帧不追加。
    #[test]
    fn small_sequence_stitches_exactly() {
        let (w, h) = (320, 200);
        let mut svc = StitchService::new();
        let scrolls = [0u32, 60, 60, 120];
        let mut outcomes = Vec::new();
        for s in scrolls {
            outcomes.push(svc.push_frame(w, h, frame_at(w, h, s)).expect("push"));
        }
        assert!(matches!(outcomes[0], FrameOutcome::Started { .. }));
        assert!(matches!(outcomes[1], FrameOutcome::Appended { growth: 60, .. }), "{outcomes:?}");
        assert_eq!(outcomes[2], FrameOutcome::Duplicate);
        assert!(matches!(outcomes[3], FrameOutcome::Appended { growth: 60, .. }), "{outcomes:?}");
        assert_eq!(svc.height(), h + 120);
        let (_, _, all) = svc.export_all_rgba().expect("export");
        assert_eq!(all, frame_at(w, h + 120, 0));
        assert_eq!(svc.stats().duplicates, 1);
    }

    /// 高度上限：到限后不再拼入并返回 LimitReached，画布不超限。
    #[test]
    fn height_limit_stops_stitching() {
        let (w, h) = (320, 200);
        let mut svc = StitchService::with_max_height(420);
        let mut limit_seen = false;
        for i in 0..10u32 {
            if matches!(svc.push_frame(w, h, frame_at(w, h, i * 60)).expect("push"), FrameOutcome::LimitReached { .. }) {
                limit_seen = true;
            }
            assert!(svc.height() <= 420, "height {}", svc.height());
        }
        assert!(limit_seen);
        assert!(svc.stats().limit_hits > 0);
    }

    /// 失败要可见：内容完全不连续的帧被报告为 Rejected，并在连续多帧后触发提示。
    #[test]
    fn discontinuous_frames_are_reported() {
        let (w, h) = (320, 200);
        let mut svc = StitchService::new();
        svc.push_frame(w, h, frame_at(w, h, 0)).expect("first");
        let mut rejected = 0;
        for i in 1..=4u32 {
            // 每帧取相距很远的文档位置，前后毫无重叠
            let out = svc
                .push_frame(w, h, frame_at(w, h, 5000 * i))
                .expect("push");
            if matches!(out, FrameOutcome::Rejected(_)) {
                rejected += 1;
            }
        }
        assert!(rejected >= REJECT_WARN_STREAK, "只报告了 {rejected} 次");
        assert!(svc.attention().is_some());
        assert_eq!(svc.height(), h, "不连续的帧不应被拼入");
    }

    /// 分块导出与整体导出内容一致。
    #[test]
    fn parts_equal_whole() {
        let (w, h) = (320, 200);
        let mut svc = StitchService::new();
        for s in [0u32, 60, 120] {
            svc.push_frame(w, h, frame_at(w, h, s)).expect("push");
        }
        let (_, _, whole) = svc.export_all_rgba().expect("all");
        let mut joined = Vec::new();
        for (top, rows) in plan_parts(svc.height(), 128) {
            joined.extend(svc.export_rows(top, rows).expect("part"));
        }
        assert_eq!(whole, joined);
    }
}

#[cfg(test)]
mod capture_order_tests {
    use super::*;

    /// R/B 对调：逐像素、alpha 不变，尾部不足 4 字节的部分忽略，再调一次还原。
    #[test]
    fn swap_red_blue_roundtrip() {
        let mut px = vec![1u8, 2, 3, 4, 5, 6, 7, 8, 9];
        swap_red_blue(&mut px);
        assert_eq!(px, vec![3, 2, 1, 4, 7, 6, 5, 8, 9]);
        swap_red_blue(&mut px);
        assert_eq!(px, vec![1, 2, 3, 4, 5, 6, 7, 8, 9]);
        swap_red_blue(&mut []);
    }

    /// 采集入口会把 BGRA 转成 RGBA：导出的第 0 字节是原来的第 2 字节。
    #[test]
    fn push_captured_converts_bgra_to_rgba() {
        let (w, h) = (16u32, 16u32);
        let mut data = Vec::new();
        for i in 0..(w * h) {
            data.extend_from_slice(&[i as u8, 100, 200, 255]);
        }
        let mut svc = StitchService::new();
        let screen = CapturedScreen { width: w, height: h, data };
        assert!(matches!(
            svc.push_captured(screen),
            Ok(FrameOutcome::Started { .. })
        ));
        let (_, _, rgba) = svc.export_all_rgba().expect("导出");
        assert_eq!(&rgba[0..4], &[200, 100, 0, 255]);
        assert_eq!(&rgba[4..8], &[200, 100, 1, 255]);
    }
}
