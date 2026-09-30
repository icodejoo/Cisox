//! 覆盖窗性能探针：统计每帧构建耗时、帧间隔与鼠标移动处理耗时。
//!
//! 开销极小（每帧一次 `Instant::now` 与一次 push），关闭覆盖窗时输出一行汇总日志，
//! 用于在不同分辨率下取实测数据。

use std::time::{Duration, Instant};

/// 每条序列最多保留的样本数（超出后丢弃最旧的）。
const MAX_SAMPLES: usize = 8192;
/// 微秒 / 秒换算。
const MICROS_PER_MILLI: f64 = 1000.0;

/// 一条序列的统计结果（单位：微秒）。
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct SeriesStats {
    /// 样本数。
    pub count: usize,
    /// 平均值。
    pub avg_us: f64,
    /// 95 分位。
    pub p95_us: u64,
    /// 最大值。
    pub max_us: u64,
}

impl SeriesStats {
    /// 由样本计算统计值；空样本返回全 0。
    ///
    /// # 参数
    /// - `samples`：微秒样本。
    ///
    /// ```ignore
    /// let s = SeriesStats::from_samples(&[1, 2, 3, 4, 100]);
    /// assert_eq!(s.max_us, 100);
    /// ```
    pub fn from_samples(samples: &[u64]) -> Self {
        if samples.is_empty() {
            return Self::default();
        }
        let mut sorted = samples.to_vec();
        sorted.sort_unstable();
        let sum: u64 = sorted.iter().sum();
        let p95_index = ((sorted.len() as f64) * 0.95).ceil() as usize;
        Self {
            count: sorted.len(),
            avg_us: sum as f64 / sorted.len() as f64,
            p95_us: sorted[p95_index.clamp(1, sorted.len()) - 1],
            max_us: *sorted.last().unwrap_or(&0),
        }
    }

    /// 格式化成 `n=.. avg=..ms p95=..ms max=..ms`。
    pub fn describe(&self) -> String {
        format!(
            "n={} avg={:.2}ms p95={:.2}ms max={:.2}ms",
            self.count,
            self.avg_us / MICROS_PER_MILLI,
            self.p95_us as f64 / MICROS_PER_MILLI,
            self.max_us as f64 / MICROS_PER_MILLI
        )
    }
}

/// 覆盖窗帧探针。
#[derive(Debug, Default)]
pub struct FrameProbe {
    /// 每帧 `render` 构建元素树耗时（微秒）。
    render_build: Vec<u64>,
    /// 相邻两次 `render` 的间隔（微秒），连续动画时约等于帧时间。
    render_interval: Vec<u64>,
    /// 鼠标移动处理（含放大镜取样）耗时（微秒）。
    move_handler: Vec<u64>,
    /// 标注更新（引擎 + 光栅化 + 合成）耗时（微秒）。
    annotation_compute: Vec<u64>,
    /// 标注脏块转图像资源（通道交换 + 装箱）耗时（微秒）。
    annotation_upload: Vec<u64>,
    /// 标注更新累计输出的脏块数。
    tiles_total: u64,
    /// 标注更新累计输出的像素字节数。
    bytes_total: u64,
    /// 上一次 `render` 开始的时刻。
    last_render: Option<Instant>,
}

/// 向序列追加样本，超上限时丢弃最旧的一半以摊薄开销。
fn push_capped(series: &mut Vec<u64>, value: u64) {
    if series.len() >= MAX_SAMPLES {
        series.drain(..MAX_SAMPLES / 2);
    }
    series.push(value);
}

impl FrameProbe {
    /// 创建空探针。
    pub fn new() -> Self {
        Self::default()
    }

    /// 标记一次 `render` 开始，返回起点供 [`FrameProbe::render_end`] 使用。
    pub fn render_start(&mut self) -> Instant {
        let now = Instant::now();
        if let Some(prev) = self.last_render.replace(now) {
            push_capped(&mut self.render_interval, micros(now.duration_since(prev)));
        }
        now
    }

    /// 标记一次 `render` 的元素树构建结束。
    pub fn render_end(&mut self, started: Instant) {
        push_capped(&mut self.render_build, micros(started.elapsed()));
    }

    /// 记录一次鼠标移动处理耗时。
    pub fn record_move(&mut self, elapsed: Duration) {
        push_capped(&mut self.move_handler, micros(elapsed));
    }

    /// 记录一次标注更新。
    ///
    /// # 参数
    /// - `compute`：引擎 + 光栅化 + 合成耗时。
    /// - `upload`：脏块转图像资源耗时。
    /// - `tiles` / `bytes`：本次输出的脏块数与像素字节数。
    pub fn record_annotation(&mut self, compute: Duration, upload: Duration, tiles: usize, bytes: usize) {
        push_capped(&mut self.annotation_compute, micros(compute));
        push_capped(&mut self.annotation_upload, micros(upload));
        self.tiles_total += tiles as u64;
        self.bytes_total += bytes as u64;
    }

    /// 标注统计：`(更新耗时, 转图像耗时, 平均每次脏块数, 平均每次字节数)`。
    pub fn annotation_summary(&self) -> (SeriesStats, SeriesStats, f64, f64) {
        let n = self.annotation_compute.len().max(1) as f64;
        (
            SeriesStats::from_samples(&self.annotation_compute),
            SeriesStats::from_samples(&self.annotation_upload),
            self.tiles_total as f64 / n,
            self.bytes_total as f64 / n,
        )
    }

    /// 已记录的渲染次数。
    pub fn render_count(&self) -> usize {
        self.render_build.len()
    }

    /// 三条序列的统计：`(构建耗时, 帧间隔, 鼠标移动处理)`。
    pub fn summary(&self) -> (SeriesStats, SeriesStats, SeriesStats) {
        (
            SeriesStats::from_samples(&self.render_build),
            SeriesStats::from_samples(&self.render_interval),
            SeriesStats::from_samples(&self.move_handler),
        )
    }

    /// 汇总成一行日志文本。
    pub fn describe(&self) -> String {
        let (build, interval, moves) = self.summary();
        let mut text = format!(
            "render_build[{}] frame_interval[{}] move_handler[{}]",
            build.describe(),
            interval.describe(),
            moves.describe()
        );
        if !self.annotation_compute.is_empty() {
            let (compute, upload, tiles, bytes) = self.annotation_summary();
            text.push_str(&format!(
                " annotation_compute[{}] annotation_upload[{}] tiles_per_update={tiles:.1} kib_per_update={:.0}",
                compute.describe(),
                upload.describe(),
                bytes / 1024.0
            ));
        }
        text
    }
}

/// `Duration` 转微秒（饱和）。
fn micros(d: Duration) -> u64 {
    u64::try_from(d.as_micros()).unwrap_or(u64::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 空样本统计为 0；有样本时平均、分位、最大值正确。
    #[test]
    fn stats_math() {
        assert_eq!(SeriesStats::from_samples(&[]), SeriesStats::default());
        let s = SeriesStats::from_samples(&[10, 20, 30, 40, 1000]);
        assert_eq!(s.count, 5);
        assert_eq!(s.max_us, 1000);
        assert_eq!(s.p95_us, 1000);
        assert!((s.avg_us - 220.0).abs() < 1e-9);
        let many: Vec<u64> = (1..=100).collect();
        assert_eq!(SeriesStats::from_samples(&many).p95_us, 95);
    }

    /// 探针：首帧没有间隔样本，之后每帧各记一条。
    #[test]
    fn probe_records_series() {
        let mut p = FrameProbe::new();
        let t = p.render_start();
        p.render_end(t);
        let t = p.render_start();
        p.render_end(t);
        p.record_move(Duration::from_micros(50));
        let (build, interval, moves) = p.summary();
        assert_eq!((build.count, interval.count, moves.count), (2, 1, 1));
        assert_eq!(moves.max_us, 50);
        assert_eq!(p.render_count(), 2);
        assert!(p.describe().contains("frame_interval"));
    }

    /// 标注统计：平均每次脏块数与字节数、耗时序列。
    #[test]
    fn annotation_series() {
        let mut p = FrameProbe::new();
        p.record_annotation(Duration::from_micros(400), Duration::from_micros(100), 2, 2048);
        p.record_annotation(Duration::from_micros(600), Duration::from_micros(300), 4, 4096);
        let (compute, upload, tiles, bytes) = p.annotation_summary();
        assert_eq!((compute.count, upload.count), (2, 2));
        assert_eq!(compute.max_us, 600);
        assert!((tiles - 3.0).abs() < 1e-9 && (bytes - 3072.0).abs() < 1e-9);
        assert!(p.describe().contains("annotation_compute"));
        assert!(!FrameProbe::new().describe().contains("annotation"));
    }

    /// 样本数达到上限后不会无限增长。
    #[test]
    fn samples_are_capped() {
        let mut p = FrameProbe::new();
        for _ in 0..(MAX_SAMPLES * 3) {
            p.record_move(Duration::from_micros(1));
        }
        assert!(p.summary().2.count <= MAX_SAMPLES);
    }
}
