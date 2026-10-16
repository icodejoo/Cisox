//! 视频剪辑窗的小型纯逻辑：输出路径去重、时间格式化。
//!
//! 不依赖 GPUI，全部可离屏单测。

use crate::export_format::{EncodeSettings, ExportFormat, encode};
use snow_app_core::command::{CompressionLevel, PdfPageSize};
use std::path::{Path, PathBuf};

/// 输出文件名后缀（接在原文件名主干后）。
pub const TRIM_SUFFIX: &str = "_trim";
/// 输出容器扩展名（关键帧裁剪只产出 MP4）。
pub const OUTPUT_EXT: &str = "mp4";
/// 序号去重的最大尝试次数，防止异常文件系统下死循环。
const MAX_DEDUP_TRIES: u32 = 9999;
/// 毫秒每秒。
const MS_PER_SEC: u64 = 1000;
/// 帧率未知时按 30fps 逐帧。
const FALLBACK_FRAME_MS: u64 = 33;
/// 帧图片扩展名。
pub const FRAME_EXT: &str = "png";
/// PNG 编码质量参数（无损，仅占位）。
const PNG_QUALITY: u8 = 100;
/// 每像素字节数（BGRA / RGBA）。
const BYTES_PER_PIXEL: usize = 4;
/// 秒每分。
const SEC_PER_MIN: u64 = 60;
/// 分每小时。
const MIN_PER_HOUR: u64 = 60;

/// 生成裁剪输出路径：原文件同目录 `<原名>_trim.mp4`，已存在则加序号 `_trim_2`、`_trim_3`…
///
/// 绝不返回已存在的路径，因此不会覆盖原文件或旧导出。
///
/// # 参数
/// - `input`：原视频路径。
/// - `exists`：路径是否已存在（便于测试）。
///
/// # 示例
/// ```ignore
/// let out = trimmed_output_path(Path::new("C:/v/a.mp4"), |p| p.exists());
/// ```
pub fn trimmed_output_path(input: &Path, exists: impl Fn(&Path) -> bool) -> PathBuf {
    let dir = input.parent().unwrap_or_else(|| Path::new(""));
    let stem = input
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "video".to_string());
    let first = dir.join(format!("{stem}{TRIM_SUFFIX}.{OUTPUT_EXT}"));
    if !exists(&first) {
        return first;
    }
    for n in 2..=MAX_DEDUP_TRIES {
        let candidate = dir.join(format!("{stem}{TRIM_SUFFIX}_{n}.{OUTPUT_EXT}"));
        if !exists(&candidate) {
            return candidate;
        }
    }
    // 极端情况下回落到带时间戳的名字，仍不覆盖任何文件
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or_default();
    dir.join(format!("{stem}{TRIM_SUFFIX}_{nanos}.{OUTPUT_EXT}"))
}

/// 格式化时间：不足 1 小时为 `m:ss.mmm`，否则 `h:mm:ss.mmm`。
///
/// # 参数
/// - `ms`：毫秒。
///
/// # 示例
/// ```ignore
/// assert_eq!(format_time(65_432), "1:05.432");
/// ```
pub fn format_time(ms: u64) -> String {
    let millis = ms % MS_PER_SEC;
    let total_sec = ms / MS_PER_SEC;
    let sec = total_sec % SEC_PER_MIN;
    let total_min = total_sec / SEC_PER_MIN;
    let min = total_min % MIN_PER_HOUR;
    let hour = total_min / MIN_PER_HOUR;
    if hour > 0 {
        format!("{hour}:{min:02}:{sec:02}.{millis:03}")
    } else {
        format!("{total_min}:{sec:02}.{millis:03}")
    }
}

/// 帧时间标签：`mm-ss-mmm`（分钟不足两位补零，超过 99 分钟照常增位），可直接用于文件名。
///
/// # 参数
/// - `ms`：帧在原视频中的毫秒位置。
///
/// # 示例
/// ```ignore
/// assert_eq!(frame_time_label(65_432), "01-05-432");
/// ```
pub fn frame_time_label(ms: u64) -> String {
    let total_sec = ms / MS_PER_SEC;
    format!(
        "{:02}-{:02}-{:03}",
        total_sec / SEC_PER_MIN,
        total_sec % SEC_PER_MIN,
        ms % MS_PER_SEC
    )
}

/// 帧图片默认文件名：`<视频名>_<mm-ss-mmm>.png`。
///
/// # 参数
/// - `video`：视频路径（取主干名，取不到时用 `video`）。
/// - `ms`：帧位置（毫秒）。
///
/// # 示例
/// ```ignore
/// assert_eq!(frame_file_name(Path::new("C:/v/rec.mp4"), 1500), "rec_00-01-500.png");
/// ```
pub fn frame_file_name(video: &Path, ms: u64) -> String {
    let stem = video
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "video".to_string());
    format!("{stem}_{}.{FRAME_EXT}", frame_time_label(ms))
}

/// 去重后的帧图片路径：位于 `dir`，重名时在扩展名前加序号 `_2`、`_3`…，绝不返回已存在的路径。
///
/// # 参数
/// - `dir`：目标目录。
/// - `file_name`：期望文件名（含扩展名）。
/// - `exists`：路径是否已存在（便于测试）。
///
/// # 示例
/// ```ignore
/// let p = unique_frame_path(Path::new("C:/v"), "a_00-00-000.png", |p| p.exists());
/// ```
pub fn unique_frame_path(dir: &Path, file_name: &str, exists: impl Fn(&Path) -> bool) -> PathBuf {
    let first = dir.join(file_name);
    if !exists(&first) {
        return first;
    }
    let name = Path::new(file_name);
    let stem = name
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    let ext = name
        .extension()
        .map(|e| e.to_string_lossy().into_owned())
        .unwrap_or_else(|| FRAME_EXT.to_string());
    for n in 2..=MAX_DEDUP_TRIES {
        let candidate = dir.join(format!("{stem}_{n}.{ext}"));
        if !exists(&candidate) {
            return candidate;
        }
    }
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or_default();
    dir.join(format!("{stem}_{nanos}.{ext}"))
}

/// 紧密排列的 BGRA 转 RGBA（交换 B / R 通道，保留 G / A）。
///
/// # 参数
/// - `width`、`height`：宽高。
/// - `bgra`：`宽 * 高 * 4` 字节、行与行之间无填充。
///
/// # 返回
/// RGBA 字节；长度与宽高对不上时返回错误说明。
///
/// # 示例
/// ```ignore
/// assert_eq!(bgra_to_rgba(1, 1, &[1, 2, 3, 4]).unwrap(), vec![3, 2, 1, 4]);
/// ```
pub fn bgra_to_rgba(width: u32, height: u32, bgra: &[u8]) -> Result<Vec<u8>, String> {
    let expected = width as usize * height as usize * BYTES_PER_PIXEL;
    if width == 0 || height == 0 || bgra.len() != expected {
        return Err(format!(
            "frame buffer size mismatch: {}x{} needs {expected} bytes, got {}",
            width,
            height,
            bgra.len()
        ));
    }
    let mut rgba = bgra.to_vec();
    for px in rgba.chunks_exact_mut(BYTES_PER_PIXEL) {
        px.swap(0, 2);
    }
    Ok(rgba)
}

/// 把预览的 BGRA 帧编码为 PNG（无损，复用截图导出的编码器）。
///
/// # 参数
/// - `width`、`height`：宽高。
/// - `bgra`：紧密排列的 BGRA 字节。
///
/// # 返回
/// PNG 文件字节；缓冲不合法或编码失败返回错误说明。
///
/// # 示例
/// ```ignore
/// let png = encode_frame_png(1, 1, &[0, 0, 255, 255])?;
/// ```
pub fn encode_frame_png(width: u32, height: u32, bgra: &[u8]) -> Result<Vec<u8>, String> {
    let rgba = bgra_to_rgba(width, height, bgra)?;
    let settings = EncodeSettings {
        quality: PNG_QUALITY,
        compression: CompressionLevel::Medium,
        pdf_page: PdfPageSize::ImageSize,
        pdf_title: String::new(),
        created: snow_platform::local_time::now(),
    };
    encode(ExportFormat::Png, width, height, &rgba, &settings)
}

/// 逐帧步长（毫秒）。
///
/// # 参数
/// - `fps_milli`：帧率 x1000（30fps 为 30000）；未知为 0。
///
/// # 返回
/// 一帧的毫秒数（至少 1）；帧率未知时回落为 33。
///
/// # 示例
/// ```ignore
/// assert_eq!(frame_step_ms(30_000), 33);
/// ```
pub fn frame_step_ms(fps_milli: u32) -> u64 {
    if fps_milli == 0 {
        return FALLBACK_FRAME_MS;
    }
    (MS_PER_SEC * MS_PER_SEC / fps_milli as u64).max(1)
}

/// 窗口左右内边距（与时间轴留白一致）。
const WINDOW_PAD: f32 = 20.0;
/// 预览区以外的固定高度估值（边距、信息行、工具栏、时间轴、提示行、底部按钮及间距）。
pub const PREVIEW_CHROME_H: f32 = 310.0;
/// 窗口最小逻辑宽度（放得下工具栏）。
pub const MIN_WINDOW_W: f32 = 640.0;
/// 窗口最小逻辑高度（固定区域加一小块预览）。
pub const MIN_WINDOW_H: f32 = PREVIEW_CHROME_H + 160.0;
/// 窗口最大占工作区的比例。
pub const MAX_WORK_AREA_RATIO: f32 = 0.8;

/// 预览画面的目标绘制矩形（相对预览区左上角，逻辑像素）。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FitRect {
    /// 左边距。
    pub x: f32,
    /// 上边距。
    pub y: f32,
    /// 宽。
    pub w: f32,
    /// 高。
    pub h: f32,
}

/// contain 适配：整张画面按原宽高比缩放进容器并居中；不放大超过 1:1。
///
/// - `cw` / `ch`：容器宽高。
/// - `vw` / `vh`：视频宽高。
///
/// 任一尺寸非正（或非有限）时返回 `None`。
///
/// ```ignore
/// let r = fit_contain(800.0, 600.0, 1920.0, 1080.0).unwrap();
/// assert_eq!((r.w, r.h), (800.0, 450.0));
/// ```
pub fn fit_contain(cw: f32, ch: f32, vw: f32, vh: f32) -> Option<FitRect> {
    let ok = |v: f32| v.is_finite() && v > 0.0;
    if !(ok(cw) && ok(ch) && ok(vw) && ok(vh)) {
        return None;
    }
    let scale = (cw / vw).min(ch / vh).min(1.0);
    let (w, h) = (vw * scale, vh * scale);
    Some(FitRect {
        x: (cw - w) / 2.0,
        y: (ch - h) / 2.0,
        w,
        h,
    })
}

/// 由视频尺寸与显示器工作区（逻辑像素）算窗口初始逻辑尺寸 `(宽, 高)`。
///
/// 预览区宽高比约等于视频宽高比；整体不超过工作区的 80%，不小于最小尺寸；
/// 视频比可用空间小时按原尺寸（不放大）。视频尺寸无效时返回 `None`。
///
/// ```ignore
/// let (w, h) = initial_window_size(1920.0, 1080.0, 1920.0, 1040.0).unwrap();
/// assert!(w <= 1920.0 * 0.8 && h <= 1040.0 * 0.8);
/// ```
pub fn initial_window_size(vw: f32, vh: f32, work_w: f32, work_h: f32) -> Option<(f32, f32)> {
    let ok = |v: f32| v.is_finite() && v > 0.0;
    if !(ok(vw) && ok(vh) && ok(work_w) && ok(work_h)) {
        return None;
    }
    let max_w = (work_w * MAX_WORK_AREA_RATIO).max(MIN_WINDOW_W.min(work_w));
    let max_h = (work_h * MAX_WORK_AREA_RATIO).max(MIN_WINDOW_H.min(work_h));
    let pw_max = (max_w - WINDOW_PAD * 2.0).max(1.0);
    let ph_max = (max_h - PREVIEW_CHROME_H).max(1.0);
    let scale = (pw_max / vw).min(ph_max / vh).min(1.0);
    let w = (vw * scale + WINDOW_PAD * 2.0)
        .round()
        .clamp(MIN_WINDOW_W, max_w.max(MIN_WINDOW_W));
    let h = (vh * scale + PREVIEW_CHROME_H)
        .round()
        .clamp(MIN_WINDOW_H, max_h.max(MIN_WINDOW_H));
    Some((w, h))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 输出路径：先 `_trim`，已存在则依次加序号，不会返回已存在的路径。
    #[test]
    fn output_path_dedup() {
        let input = Path::new("C:/v/rec.mp4");
        let none = |_: &Path| false;
        assert_eq!(
            trimmed_output_path(input, none),
            PathBuf::from("C:/v").join("rec_trim.mp4")
        );
        let taken = [
            PathBuf::from("C:/v").join("rec_trim.mp4"),
            PathBuf::from("C:/v").join("rec_trim_2.mp4"),
        ];
        let out = trimmed_output_path(input, |p| taken.iter().any(|t| t == p));
        assert_eq!(out, PathBuf::from("C:/v").join("rec_trim_3.mp4"));
        // 非 mp4 输入也输出 mp4，且不会等于输入
        let gif = trimmed_output_path(Path::new("a.gif"), none);
        assert_eq!(gif, PathBuf::from("a_trim.mp4"));
    }

    /// 时间格式：分秒毫秒与小时进位。
    #[test]
    fn time_format() {
        assert_eq!(format_time(0), "0:00.000");
        assert_eq!(format_time(65_432), "1:05.432");
        assert_eq!(format_time(3_600_000 + 61_001), "1:01:01.001");
    }

    /// 逐帧步长：按帧率换算，未知时回落 33ms。
    #[test]
    fn frame_step() {
        assert_eq!(frame_step_ms(30_000), 33);
        assert_eq!(frame_step_ms(60_000), 16);
        assert_eq!(frame_step_ms(0), 33);
        assert_eq!(frame_step_ms(u32::MAX), 1);
    }

    /// 帧时间标签：分秒毫秒补零，超过一小时按总分钟数增位。
    #[test]
    fn frame_label_and_name() {
        assert_eq!(frame_time_label(0), "00-00-000");
        assert_eq!(frame_time_label(65_432), "01-05-432");
        assert_eq!(frame_time_label(3_600_000 + 1), "60-00-001");
        assert_eq!(
            frame_file_name(Path::new("C:/v/rec 1.mp4"), 1500),
            "rec 1_00-01-500.png"
        );
        assert_eq!(frame_file_name(Path::new(".."), 0), "video_00-00-000.png");
    }

    /// 帧图片去重：不覆盖已有文件，序号递增。
    #[test]
    fn frame_path_dedup() {
        let dir = Path::new("D:/out");
        assert_eq!(
            unique_frame_path(dir, "a_00-00-000.png", |_| false),
            dir.join("a_00-00-000.png")
        );
        let taken = [dir.join("a_00-00-000.png"), dir.join("a_00-00-000_2.png")];
        let out = unique_frame_path(dir, "a_00-00-000.png", |p| taken.iter().any(|t| t == p));
        assert_eq!(out, dir.join("a_00-00-000_3.png"));
    }

    /// BGRA 转 RGBA：通道顺序、行跨度与尺寸校验，并能编码成同尺寸 PNG。
    #[test]
    fn bgra_to_rgba_and_png() {
        // 2x2，每个像素 B,G,R,A 各不相同
        let bgra: Vec<u8> = (0..4u8)
            .flat_map(|i| [10 + i, 20 + i, 30 + i, 200 + i])
            .collect();
        let rgba = bgra_to_rgba(2, 2, &bgra).unwrap();
        for i in 0..4usize {
            let px = &rgba[i * 4..i * 4 + 4];
            assert_eq!(
                px,
                [30 + i as u8, 20 + i as u8, 10 + i as u8, 200 + i as u8]
            );
        }
        assert!(bgra_to_rgba(2, 2, &bgra[..15]).is_err());
        assert!(bgra_to_rgba(0, 2, &[]).is_err());
        // 非正方形：行跨度必须是 宽*4
        let wide = vec![7u8; 3 * 2 * 4];
        assert_eq!(bgra_to_rgba(3, 2, &wide).unwrap().len(), 24);
        assert!(bgra_to_rgba(2, 3, &wide[..23]).is_err());
        // 编码后尺寸一致
        let png = encode_frame_png(2, 2, &bgra).unwrap();
        assert!(encode_frame_png(2, 2, &bgra[..15]).is_err());
        let decoded = image::load_from_memory_with_format(&png, image::ImageFormat::Png)
            .unwrap()
            .to_rgba8();
        assert_eq!(decoded.dimensions(), (2, 2));
        assert_eq!(decoded.into_raw(), rgba);
    }

    #[test]
    fn fit_contain_cases() {
        // 横屏：宽受限
        let r = fit_contain(800.0, 600.0, 1920.0, 1080.0).unwrap();
        assert_eq!((r.w, r.h, r.x, r.y), (800.0, 450.0, 0.0, 75.0));
        // 竖屏：高受限，水平居中
        let r = fit_contain(800.0, 600.0, 1080.0, 1920.0).unwrap();
        assert_eq!((r.h, r.y), (600.0, 0.0));
        assert!((r.w - 337.5).abs() < 1e-3 && (r.x - 231.25).abs() < 1e-3);
        // 超宽
        let r = fit_contain(800.0, 600.0, 4000.0, 100.0).unwrap();
        assert_eq!((r.w, r.h), (800.0, 20.0));
        // 比容器小：不放大，居中
        let r = fit_contain(800.0, 600.0, 400.0, 300.0).unwrap();
        assert_eq!((r.w, r.h, r.x, r.y), (400.0, 300.0, 200.0, 150.0));
        // 无效尺寸
        assert!(fit_contain(0.0, 600.0, 10.0, 10.0).is_none());
        assert!(fit_contain(800.0, 0.0, 10.0, 10.0).is_none());
        assert!(fit_contain(800.0, 600.0, 0.0, 10.0).is_none());
        assert!(fit_contain(f32::NAN, 600.0, 10.0, 10.0).is_none());
    }

    #[test]
    fn initial_window_size_cases() {
        let (ww, wh) = (1920.0, 1040.0);
        let in_range = |(w, h): (f32, f32)| {
            assert!(w >= MIN_WINDOW_W && h >= MIN_WINDOW_H, "{w}x{h}");
            assert!(
                w <= ww * MAX_WORK_AREA_RATIO && h <= wh * MAX_WORK_AREA_RATIO,
                "{w}x{h}"
            );
        };
        // 4:3 / 16:9 / 9:16 / 4K：预览宽高比约等于视频
        for (vw, vh) in [
            (640.0, 480.0),
            (1280.0, 720.0),
            (1080.0, 1920.0),
            (3840.0, 2160.0),
        ] {
            let s = initial_window_size(vw, vh, ww, wh).unwrap();
            in_range(s);
            let ratio = (s.0 - WINDOW_PAD * 2.0) / (s.1 - PREVIEW_CHROME_H);
            let want = vw / vh;
            // 竖屏会被最小宽度夹住，仅检查非竖屏
            if vw >= vh {
                assert!((ratio - want).abs() / want < 0.02, "{vw}x{vh} -> {ratio}");
            }
        }
        // 小视频不放大：窗口取最小尺寸
        assert_eq!(
            initial_window_size(100.0, 60.0, ww, wh),
            Some((MIN_WINDOW_W, MIN_WINDOW_H))
        );
        // 中等视频按原尺寸
        let s = initial_window_size(1000.0, 400.0, ww, wh).unwrap();
        assert_eq!(s, (1040.0, 710.0));
        // 极端宽高比
        in_range(initial_window_size(8000.0, 100.0, ww, wh).unwrap());
        in_range(initial_window_size(100.0, 8000.0, ww, wh).unwrap());
        // 无效输入
        assert!(initial_window_size(0.0, 100.0, ww, wh).is_none());
        assert!(initial_window_size(100.0, 100.0, 0.0, wh).is_none());
    }
}
