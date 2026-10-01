//! 确定性合成样片：用系统 GDI 画字（经 `snow-platform::text_raster`），配期望文本，存为 PNG + TXT。
//!
//! 同一份规格在同一台机器上渲染结果逐字节一致（噪声用固定种子的 xorshift）；
//! 不同机器的字体版本可能让像素略有差异，因此样片放在被忽略的目录，不入库。
//! 合成图只能拿来比较引擎的相对表现，不代表真实截图。

use snow_platform::text_raster::{DEFAULT_FONT_FAMILY, rasterize_text};
use std::path::{Path, PathBuf};

/// 图像四周留白（像素）。
const MARGIN: u32 = 24;
/// 行距相对字号的比例。
const LINE_GAP_RATIO: f32 = 0.6;

/// 背景类型。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Background {
    /// 纯色。
    Solid([u8; 3]),
    /// 自左上到右下的线性渐变。
    Gradient([u8; 3], [u8; 3]),
}

/// 一张样片的规格。
#[derive(Debug, Clone, Copy)]
pub struct SampleSpec {
    /// 文件名（不含扩展名），前缀 `类别-` 用于汇总分组。
    pub name: &'static str,
    /// 期望文本，每个元素画一行。
    pub lines: &'static [&'static str],
    /// 字号（像素）。
    pub px: f32,
    /// 前景色 RGB。
    pub fg: [u8; 3],
    /// 背景。
    pub bg: Background,
    /// 噪声幅度（0 表示无噪声），对每个像素加 ±幅度 的灰度扰动。
    pub noise: u8,
}

/// 浅底。
const WHITE: [u8; 3] = [255, 255, 255];
/// 深字。
const INK: [u8; 3] = [20, 20, 20];
/// 深底。
const DARK: [u8; 3] = [30, 32, 36];
/// 浅字。
const PALE: [u8; 3] = [230, 230, 230];

/// 内置样片规格（覆盖中文、英文、混排、数字符号、小/大字号、深浅底、噪声与渐变）。
pub const SPECS: &[SampleSpec] = &[
    SampleSpec {
        name: "zh-plain",
        lines: &[
            "你好，这是一段中文识别测试。",
            "截图软件需要高性能与低内存占用。",
        ],
        px: 28.0,
        fg: INK,
        bg: Background::Solid(WHITE),
        noise: 0,
    },
    SampleSpec {
        name: "en-plain",
        lines: &[
            "The quick brown fox jumps over the lazy dog.",
            "Snow Shot captures screens quickly.",
        ],
        px: 28.0,
        fg: INK,
        bg: Background::Solid(WHITE),
        noise: 0,
    },
    SampleSpec {
        name: "mixed-plain",
        lines: &[
            "使用 Rust 和 GPUI 构建 Snow Shot 应用。",
            "Version 1.2.3 已发布，请更新 OK。",
        ],
        px: 28.0,
        fg: INK,
        bg: Background::Solid(WHITE),
        noise: 0,
    },
    SampleSpec {
        name: "symbols-plain",
        lines: &[
            "Total: $1,234.56 (12% off)",
            "ID: A-0042 / 2026-10-01 12:30:45",
            "mail@example.com #tag *bold* [x] {y} <z>",
        ],
        px: 26.0,
        fg: INK,
        bg: Background::Solid(WHITE),
        noise: 0,
    },
    SampleSpec {
        name: "small-10px",
        lines: &[
            "Small text at ten pixels: 0123456789",
            "小字号十像素中文识别测试一二三",
        ],
        px: 10.0,
        fg: INK,
        bg: Background::Solid(WHITE),
        noise: 0,
    },
    SampleSpec {
        name: "small-12px",
        lines: &[
            "Small text at twelve pixels: 0123456789",
            "小字号十二像素中文识别测试一二三",
        ],
        px: 12.0,
        fg: INK,
        bg: Background::Solid(WHITE),
        noise: 0,
    },
    SampleSpec {
        name: "large-64px",
        lines: &["大字 Big 123"],
        px: 64.0,
        fg: INK,
        bg: Background::Solid(WHITE),
        noise: 0,
    },
    SampleSpec {
        name: "dark-mixed",
        lines: &[
            "深色背景浅色字 Dark mode 2026",
            "Terminal output: build finished in 3.2s",
        ],
        px: 26.0,
        fg: PALE,
        bg: Background::Solid(DARK),
        noise: 0,
    },
    SampleSpec {
        name: "noise-mixed",
        lines: &["带噪声的背景 Noisy background", "识别 recognition 12345"],
        px: 26.0,
        fg: INK,
        bg: Background::Solid([245, 245, 240]),
        noise: 14,
    },
    SampleSpec {
        name: "gradient-mixed",
        lines: &[
            "渐变背景 Gradient background",
            "从浅蓝到浅粉 light blue to pink",
        ],
        px: 26.0,
        fg: INK,
        bg: Background::Gradient([200, 220, 245], [250, 215, 230]),
        noise: 0,
    },
    SampleSpec {
        name: "gradient-noise-dark",
        lines: &["深色渐变加噪声 Dark gradient", "Status: OK 状态正常 100%"],
        px: 24.0,
        fg: PALE,
        bg: Background::Gradient([20, 24, 40], [60, 40, 70]),
        noise: 10,
    },
];

/// 渲染出的图像。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Rendered {
    /// 宽（像素）。
    pub width: u32,
    /// 高（像素）。
    pub height: u32,
    /// 紧凑 RGBA 像素。
    pub rgba: Vec<u8>,
}

/// 固定种子的 xorshift32 伪随机数（保证噪声可复现）。
struct Xorshift(u32);

impl Xorshift {
    /// 以名字的 FNV-1a 哈希作种子（避免种子为 0）。
    fn from_name(name: &str) -> Self {
        let hash = name.bytes().fold(0x811c_9dc5_u32, |h, b| {
            (h ^ u32::from(b)).wrapping_mul(0x0100_0193)
        });
        Self(hash.max(1))
    }

    /// 取下一个值。
    fn next(&mut self) -> u32 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 17;
        x ^= x << 5;
        self.0 = x;
        x
    }
}

/// 期望文本：各行以 `\n` 连接。
///
/// # 参数
/// - `spec`：样片规格。
///
/// # 示例
/// ```
/// let spec = &snow_ocr_compare::samples::SPECS[0];
/// assert!(snow_ocr_compare::samples::expected_text(spec).contains('\n'));
/// ```
pub fn expected_text(spec: &SampleSpec) -> String {
    spec.lines.join("\n")
}

/// 某像素处的背景色。
///
/// # 参数
/// - `bg`：背景类型。
/// - `(x, y)`：像素坐标。
/// - `(w, h)`：图像尺寸。
pub fn background_at(bg: Background, x: u32, y: u32, w: u32, h: u32) -> [u8; 3] {
    match bg {
        Background::Solid(c) => c,
        Background::Gradient(from, to) => {
            let span = (w + h).saturating_sub(2).max(1);
            let t = (x + y) as f32 / span as f32;
            std::array::from_fn(|i| {
                (f32::from(from[i]) + (f32::from(to[i]) - f32::from(from[i])) * t).round() as u8
            })
        }
    }
}

/// 把前景按覆盖率混到背景上。
///
/// # 参数
/// - `bg` / `fg`：背景与前景色。
/// - `coverage`：覆盖率 0..=255。
pub fn blend(bg: [u8; 3], fg: [u8; 3], coverage: u8) -> [u8; 3] {
    let a = u32::from(coverage);
    std::array::from_fn(|i| {
        ((u32::from(bg[i]) * (255 - a) + u32::from(fg[i]) * a + 127) / 255) as u8
    })
}

/// 渲染一张样片。
///
/// # 参数
/// - `spec`：样片规格。
///
/// # 返回
/// RGBA 图；字体光栅化失败（非 Windows 等）返回错误说明。
///
/// # 示例
/// ```ignore
/// let img = snow_ocr_compare::samples::render(&SPECS[0])?;
/// assert_eq!(img.rgba.len(), (img.width * img.height * 4) as usize);
/// ```
pub fn render(spec: &SampleSpec) -> Result<Rendered, String> {
    let bitmaps = spec
        .lines
        .iter()
        .map(|line| rasterize_text(line, DEFAULT_FONT_FAMILY, spec.px, false))
        .collect::<Result<Vec<_>, _>>()?;
    let gap = (spec.px * LINE_GAP_RATIO).round() as u32;
    let width = bitmaps.iter().map(|b| b.width).max().unwrap_or(1) + 2 * MARGIN;
    let height = bitmaps.iter().map(|b| b.height).sum::<u32>()
        + gap * bitmaps.len().saturating_sub(1) as u32
        + 2 * MARGIN;
    let mut rgba = vec![255u8; width as usize * height as usize * 4];
    for y in 0..height {
        for x in 0..width {
            let c = background_at(spec.bg, x, y, width, height);
            let at = (y as usize * width as usize + x as usize) * 4;
            rgba[at..at + 3].copy_from_slice(&c);
        }
    }
    let mut top = MARGIN;
    for bmp in &bitmaps {
        for y in 0..bmp.height {
            for x in 0..bmp.width {
                let cov = bmp.coverage[(y * bmp.width + x) as usize];
                let at = (((top + y) * width + MARGIN + x) * 4) as usize;
                let bg = [rgba[at], rgba[at + 1], rgba[at + 2]];
                rgba[at..at + 3].copy_from_slice(&blend(bg, spec.fg, cov));
            }
        }
        top += bmp.height + gap;
    }
    if spec.noise > 0 {
        add_noise(&mut rgba, spec.noise, &mut Xorshift::from_name(spec.name));
    }
    Ok(Rendered {
        width,
        height,
        rgba,
    })
}

/// 给每个像素加同一幅度的灰度扰动（三通道共用，保持灰度感）。
fn add_noise(rgba: &mut [u8], amplitude: u8, rng: &mut Xorshift) {
    let span = u32::from(amplitude) * 2 + 1;
    for px in rgba.chunks_exact_mut(4) {
        let delta = (rng.next() % span) as i32 - i32::from(amplitude);
        for channel in &mut px[..3] {
            *channel = (i32::from(*channel) + delta).clamp(0, 255) as u8;
        }
    }
}

/// 把全部内置样片写成 `<name>.png` 与 `<name>.txt`（期望文本）。
///
/// # 参数
/// - `dir`：输出目录（不存在则创建）。
///
/// # 返回
/// 写出的样片数量。
///
/// # 示例
/// ```ignore
/// let n = snow_ocr_compare::samples::write_all(Path::new("target/ocr-samples"))?;
/// ```
pub fn write_all(dir: &Path) -> Result<usize, String> {
    std::fs::create_dir_all(dir).map_err(|e| format!("无法创建目录 {}: {e}", dir.display()))?;
    for spec in SPECS {
        let img = render(spec)?;
        let png = dir.join(format!("{}.png", spec.name));
        image::RgbaImage::from_raw(img.width, img.height, img.rgba)
            .ok_or_else(|| "像素长度与尺寸不符".to_string())?
            .save(&png)
            .map_err(|e| format!("无法写入 {}: {e}", png.display()))?;
        let txt = dir.join(format!("{}.txt", spec.name));
        std::fs::write(&txt, expected_text(spec))
            .map_err(|e| format!("无法写入 {}: {e}", txt.display()))?;
    }
    Ok(SPECS.len())
}

/// 一个待对比的用例：图像与期望文本。
#[derive(Debug, Clone)]
pub struct Case {
    /// 用例名（文件名不含扩展名）。
    pub name: String,
    /// 类别（名字里第一个 `-` 之前的部分）。
    pub category: String,
    /// 宽。
    pub width: u32,
    /// 高。
    pub height: u32,
    /// 紧凑 RGBA 像素。
    pub rgba: Vec<u8>,
    /// 期望文本。
    pub expected: String,
}

/// 取名字的类别：第一个 `-` 之前的部分，没有 `-` 则整个名字。
///
/// # 示例
/// ```
/// assert_eq!(snow_ocr_compare::samples::category_of("zh-plain"), "zh");
/// assert_eq!(snow_ocr_compare::samples::category_of("shot"), "shot");
/// ```
pub fn category_of(name: &str) -> &str {
    name.split('-').next().unwrap_or(name)
}

/// 读取目录里成对的 `<名>.png` 与 `<名>.txt`（用户自备真实截图也走这里）。
///
/// # 参数
/// - `dir`：图片目录。
///
/// # 返回
/// `(按名字排序的用例, 因缺 txt 或读取失败被跳过的说明)`。
///
/// # 示例
/// ```ignore
/// let (cases, skipped) = snow_ocr_compare::samples::load_dir(Path::new("target/ocr-samples"))?;
/// ```
pub fn load_dir(dir: &Path) -> Result<(Vec<Case>, Vec<String>), String> {
    let mut pngs: Vec<PathBuf> = std::fs::read_dir(dir)
        .map_err(|e| format!("无法读取目录 {}: {e}", dir.display()))?
        .filter_map(|entry| entry.ok().map(|e| e.path()))
        .filter(|p| {
            p.extension()
                .is_some_and(|ext| ext.eq_ignore_ascii_case("png"))
        })
        .collect();
    pngs.sort();
    let (mut cases, mut skipped) = (Vec::new(), Vec::new());
    for png in pngs {
        let name = png
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default();
        let Ok(expected) = std::fs::read_to_string(png.with_extension("txt")) else {
            skipped.push(format!("{name}: 缺少期望文本 {name}.txt"));
            continue;
        };
        match image::open(&png) {
            Ok(img) => {
                let rgba = img.to_rgba8();
                let (width, height) = rgba.dimensions();
                let category = category_of(&name).to_string();
                cases.push(Case {
                    name,
                    category,
                    width,
                    height,
                    rgba: rgba.into_raw(),
                    expected,
                });
            }
            Err(e) => skipped.push(format!("{name}: 无法解码 PNG: {e}")),
        }
    }
    Ok((cases, skipped))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 规格名唯一，且都带类别前缀，期望文本非空。
    #[test]
    fn specs_are_well_formed() {
        let mut names: Vec<_> = SPECS.iter().map(|s| s.name).collect();
        names.sort();
        names.dedup();
        assert_eq!(names.len(), SPECS.len());
        assert!(
            SPECS
                .iter()
                .all(|s| s.name.contains('-') && !expected_text(s).trim().is_empty())
        );
        assert!(SPECS.iter().any(|s| s.px <= 12.0) && SPECS.iter().any(|s| s.px >= 60.0));
    }

    /// 渐变端点与中点；纯色恒定。
    #[test]
    fn background_gradient_endpoints() {
        let g = Background::Gradient([0, 0, 0], [200, 100, 50]);
        assert_eq!(background_at(g, 0, 0, 11, 11), [0, 0, 0]);
        assert_eq!(background_at(g, 10, 10, 11, 11), [200, 100, 50]);
        assert_eq!(background_at(g, 5, 5, 11, 11), [100, 50, 25]);
        assert_eq!(
            background_at(Background::Solid([1, 2, 3]), 9, 9, 20, 20),
            [1, 2, 3]
        );
    }

    /// 混合：覆盖率 0 取背景，255 取前景。
    #[test]
    fn blend_extremes() {
        assert_eq!(blend([10, 20, 30], [200, 210, 220], 0), [10, 20, 30]);
        assert_eq!(blend([10, 20, 30], [200, 210, 220], 255), [200, 210, 220]);
    }

    /// 噪声可复现：同名同种子同结果，不同名结果不同，且不越界。
    #[test]
    fn noise_is_deterministic() {
        let mut a = vec![128u8; 4 * 64];
        let mut b = a.clone();
        let mut c = a.clone();
        add_noise(&mut a, 12, &mut Xorshift::from_name("x"));
        add_noise(&mut b, 12, &mut Xorshift::from_name("x"));
        add_noise(&mut c, 12, &mut Xorshift::from_name("y"));
        assert_eq!(a, b);
        assert_ne!(a, c);
        assert!(a.iter().all(|v| (116..=140).contains(v) || *v == 128));
    }

    /// 类别取第一个 `-` 之前。
    #[test]
    fn category_split() {
        assert_eq!(category_of("gradient-noise-dark"), "gradient");
        assert_eq!(category_of("plain"), "plain");
    }

    /// 同一规格渲染两次逐字节一致，尺寸与像素长度匹配（依赖 Windows GDI）。
    #[cfg(windows)]
    #[test]
    fn render_is_deterministic() {
        for spec in [&SPECS[0], &SPECS[8]] {
            let (a, b) = (render(spec).expect("渲染"), render(spec).expect("渲染"));
            assert_eq!(a, b);
            assert_eq!(a.rgba.len(), (a.width * a.height * 4) as usize);
            assert!(a.width > 2 * MARGIN && a.height > 2 * MARGIN);
        }
    }

    /// 写出再读回：数量、期望文本与像素尺寸一致，缺 txt 的图被跳过（依赖 Windows GDI）。
    #[cfg(windows)]
    #[test]
    fn write_then_load_round_trip() {
        let dir = std::env::temp_dir().join(format!("snow-ocr-compare-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(write_all(&dir).expect("写出"), SPECS.len());
        std::fs::remove_file(dir.join("zh-plain.txt")).expect("删 txt");
        let (cases, skipped) = load_dir(&dir).expect("读取");
        assert_eq!(cases.len(), SPECS.len() - 1);
        assert_eq!(skipped.len(), 1);
        let en = cases
            .iter()
            .find(|c| c.name == "en-plain")
            .expect("en-plain");
        assert_eq!(en.expected, expected_text(&SPECS[1]));
        let direct = render(&SPECS[1]).expect("渲染");
        assert_eq!((en.width, en.height), (direct.width, direct.height));
        assert_eq!(en.rgba, direct.rgba);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
