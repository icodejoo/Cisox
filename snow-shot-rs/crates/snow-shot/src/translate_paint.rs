//! 截图翻译“原位替换”的取色与上色：采样行框外圈得到背景模型（纯色或线性渐变）、估计前景色与字重、
//! 用背景模型填充、把字形覆盖率位图叠到图上。全部是不依赖 GPUI 的纯像素函数，可离屏确定性测试。

use snow_platform::text_raster::TextBitmap;
use snow_ui::shell::geometry::PhysicalRect;
use std::collections::BTreeMap;

/// 采样背景时，填充区外圈条带的宽度（像素）。
const RING: i32 = 3;
/// 背景采样的颜色量化位移（每通道右移位数，用来找最常见的颜色）。
const BG_QUANT_SHIFT: u8 = 3;
/// 印证陡渐变时，上下条带中位色与左右两端中点允许的最大色差。
const GRAD_VERIFY_DIST: i32 = 36;
/// 判定“中位色附近”的最大色差（RGB 绝对差之和）。
const MEDIAN_NEAR: i32 = 90;
/// 中位色附近的像素至少占这个比例，才信任中位数。
const MEDIAN_NEAR_SHARE: f32 = 0.6;
/// 前景采样的颜色量化位移。
const FG_QUANT_SHIFT: u8 = 4;
/// 两个边缘颜色的距离不超过该值视为同一种纯色。
const FLAT_DIST: i32 = 24;
/// 两个边缘颜色的距离超过该值视为落在不同区域（除非上下条带印证它是陡渐变）。
const GRAD_MAX_DIST: i32 = 100;
/// 前景色与背景色的最小亮度差，不足则改用黑 / 白对比色。
const MIN_LUMA_DIFF: i32 = 90;
/// 判定框里存在“墨迹像素”所需的最小色差（RGB 绝对差之和）。
const MIN_INK_DISTANCE: i32 = 96;
/// 取前景色时，只统计色差不低于最大色差该比例的像素。
const INK_KEEP_RATIO: f32 = 0.75;
/// 估计笔画粗细时，低于最大色差该比例的色差视为噪声。
const STEM_NOISE_FLOOR: f32 = 0.12;
/// 行高小于它（像素）的极小字完全分不出字重，一律按常规。
const TINY_LINE_H: f32 = 12.0;
/// 笔画平均宽度占行高的比例达到它就判为粗体（行高不小于 `SMALL_LINE_H` 时用；按 Segoe UI / Arial / 微软雅黑 /
/// Times New Roman / Calibri / Verdana 的常规与粗体实测标定：常规多在 0.045–0.085，粗体多在 0.09–0.13）。
const BOLD_STEM_RATIO: f32 = 0.088;
/// 行高小于它（像素）的小字，笔画被像素栅格量化得厉害，常规与粗体几乎分不开，判粗体的门槛抬高。
const SMALL_LINE_H: f32 = 16.0;
/// 小字判粗体的比例门槛。
const BOLD_STEM_RATIO_SMALL: f32 = 0.12;
/// 外扩带里，覆盖率低于它的像素离背景太近，不用管。
const EDGE_T_MIN: f32 = 0.12;
/// 外扩带里，覆盖率高于它的像素已是纯前景色（原文行框本该盖住全部笔画，框外的纯前景色多半是邻近图形），不动。
const EDGE_T_MAX: f32 = 0.8;
/// 判定“落在背景→前景连线上”允许的偏离，占两色距离的比例。
const EDGE_RESIDUAL_RATIO: f32 = 0.06;
/// 上述偏离的下限（RGB 绝对差之和）。
const EDGE_MIN_RESIDUAL: f32 = 12.0;
/// 量墨迹范围时，像素的覆盖率至少达到它才算墨迹。
const INK_MEASURE_T: f32 = 0.3;
/// 量墨迹范围时，一行里“墨迹像素”占搜索宽度的比例超过它，说明是大块同色区域（与文字同色的图形 / 底色），不是文字行。
const MAX_INK_ROW_FRACTION: f32 = 0.9;
/// 量墨迹时，横向在行框左右各多看的像素数。
const INK_COLUMN_SLACK: i32 = 2;
/// 墨迹掩码里判定“实心块”的窗口边长（像素，奇数）。
const SOLID_SIDE: i32 = 7;
/// 判断文字行时，行框左右各去掉 `行框高度 / 该值` 的列（行框外扩出来的两端不参与）。
const CORE_MARGIN_DIV: i32 = 5;
/// 向两侧延伸墨迹左右边界时，容许的空隙占墨迹高度的比例（字间距以内的空隙继续延伸）。
const INK_GAP_RATIO: f32 = 0.25;
/// 上述空隙的像素下限。
const MIN_INK_GAP: i32 = 3;
/// 量墨迹范围时，一行里至少有这么多墨迹像素才算“有墨迹的行”。
const MIN_INK_ROW_PIXELS: usize = 2;
/// 什么样本都采不到时的背景色（白）。
const DEFAULT_BG: [u8; 3] = [255, 255, 255];
/// 背景与对比色之间亮度的分界。
const LIGHT_LUMA: i32 = 140;

/// 背景模型：线性渐变（纯色是渐变为 0 的特例），按像素位置给出背景色。
#[derive(Debug, Clone, PartialEq)]
pub struct BgModel {
    /// 填充区中心处的颜色。
    base: [f32; 3],
    /// 水平方向每像素的颜色变化。
    gx: [f32; 3],
    /// 垂直方向每像素的颜色变化。
    gy: [f32; 3],
    /// 渐变原点的 x。
    cx: f32,
    /// 渐变原点的 y。
    cy: f32,
}

impl BgModel {
    /// 纯色背景。
    ///
    /// # 参数
    /// - `c`：颜色。
    pub fn flat(c: [u8; 3]) -> Self {
        Self {
            base: c.map(f32::from),
            gx: [0.0; 3],
            gy: [0.0; 3],
            cx: 0.0,
            cy: 0.0,
        }
    }

    /// 某个像素位置的背景色。
    ///
    /// # 参数
    /// - `x` / `y`：图像坐标。
    pub fn at(&self, x: i32, y: i32) -> [u8; 3] {
        let (dx, dy) = (x as f32 - self.cx, y as f32 - self.cy);
        [0, 1, 2].map(|i| {
            (self.base[i] + self.gx[i] * dx + self.gy[i] * dy)
                .clamp(0.0, 255.0)
                .round() as u8
        })
    }

    /// 是否带渐变。
    pub fn is_gradient(&self) -> bool {
        self.gx
            .iter()
            .chain(&self.gy)
            .any(|g| g.abs() > f32::EPSILON)
    }

    /// 中心处的颜色。
    pub fn mean(&self) -> [u8; 3] {
        self.base.map(|v| v.round().clamp(0.0, 255.0) as u8)
    }
}

/// 亮度（0..=255 的整数近似）。
pub fn luma(c: [u8; 3]) -> i32 {
    (i32::from(c[0]) * 299 + i32::from(c[1]) * 587 + i32::from(c[2]) * 114) / 1000
}

/// 与背景形成对比的黑 / 白。
pub fn contrast_color(bg: [u8; 3]) -> [u8; 3] {
    if luma(bg) > LIGHT_LUMA {
        [0, 0, 0]
    } else {
        [255, 255, 255]
    }
}

/// RGB 绝对差之和。
fn color_distance(a: [u8; 3], b: [u8; 3]) -> i32 {
    (0..3)
        .map(|i| (i32::from(a[i]) - i32::from(b[i])).abs())
        .sum()
}

/// 浮点颜色的 RGB 绝对差之和。
fn dist_f(a: [f32; 3], b: [f32; 3]) -> i32 {
    (0..3).map(|i| (a[i] - b[i]).abs()).sum::<f32>().round() as i32
}

/// 取像素的 RGB。
fn pixel(rgba: &[u8], width: u32, x: i32, y: i32) -> [u8; 3] {
    let i = (y as usize * width as usize + x as usize) * 4;
    [rgba[i], rgba[i + 1], rgba[i + 2]]
}

/// 颜色量化成桶键。
fn quant_key(c: [u8; 3], shift: u8) -> u32 {
    c.iter().fold(0u32, |k, v| (k << 8) | u32::from(v >> shift))
}

/// 在一批颜色里找最常见的桶并返回桶内平均色；空输入返回 `None`。桶按键排序，平局取键小的，结果确定。
fn mode_color(colors: impl Iterator<Item = [u8; 3]>, shift: u8) -> Option<[f32; 3]> {
    let mut buckets: BTreeMap<u32, (u32, [u64; 3])> = BTreeMap::new();
    for c in colors {
        let e = buckets.entry(quant_key(c, shift)).or_insert((0, [0; 3]));
        e.0 += 1;
        for (s, v) in e.1.iter_mut().zip(c) {
            *s += u64::from(v);
        }
    }
    buckets
        .values()
        .fold(None::<&(u32, [u64; 3])>, |best, cur| match best {
            Some(b) if b.0 >= cur.0 => Some(b),
            _ => Some(cur),
        })
        .map(|(n, sum)| sum.map(|s| s as f32 / *n as f32))
}

/// 一批颜色的代表色：逐通道取中位数（渐变条带的中位数就是中点色）；但若中位色附近的像素不到六成
/// （条带横跨两块不同颜色的区域，中位数会是个谁都不是的混合色），改用众数色。
fn robust_color(colors: &[[u8; 3]]) -> Option<[f32; 3]> {
    if colors.is_empty() {
        return None;
    }
    let median = [0, 1, 2].map(|c| {
        let mut v: Vec<u8> = colors.iter().map(|p| p[c]).collect();
        v.sort_unstable();
        f32::from(v[v.len() / 2])
    });
    let near = colors
        .iter()
        .filter(|p| dist_f(p.map(f32::from), median) <= MEDIAN_NEAR)
        .count();
    if near as f32 >= colors.len() as f32 * MEDIAN_NEAR_SHARE {
        Some(median)
    } else {
        mode_color(colors.iter().copied(), BG_QUANT_SHIFT)
    }
}

/// 一个条带里没被障碍盖住的像素的代表色及像素数。
fn strip_color(
    rgba: &[u8],
    width: u32,
    image: PhysicalRect,
    strip: PhysicalRect,
    obstacles: &[PhysicalRect],
) -> Option<([f32; 3], usize)> {
    let clip = strip.intersect(&image)?;
    let near: Vec<&PhysicalRect> = obstacles
        .iter()
        .filter(|o| o.intersect(&clip).is_some())
        .collect();
    let colors = (clip.y..clip.bottom())
        .flat_map(|y| (clip.x..clip.right()).map(move |x| (x, y)))
        .filter(|&(x, y)| {
            !near
                .iter()
                .any(|o| x >= o.x && x < o.right() && y >= o.y && y < o.bottom())
        })
        .map(|(x, y)| pixel(rgba, width, x, y));
    let colors: Vec<[u8; 3]> = colors.collect();
    robust_color(&colors).map(|c| (c, colors.len()))
}

/// 一根轴（水平或垂直）上两端边缘色的归类。
enum Axis {
    /// 没有样本。
    Missing,
    /// 只有一端。
    Single([f32; 3]),
    /// 两端几乎同色。
    Flat([f32; 3]),
    /// 两端有差异但仍像同一块渐变。
    Grad([f32; 3], [f32; 3]),
    /// 两端差异大到不像同一块背景（文字紧贴某块区域的边）；保留两端颜色，供另一轴印证后改判为陡渐变。
    Split([f32; 3], [f32; 3]),
}

impl Axis {
    /// 两端都采到且像同一块背景时的中点色。
    fn two_sided_mid(&self) -> Option<[f32; 3]> {
        match self {
            Axis::Flat(m) => Some(*m),
            Axis::Grad(a, b) => Some([0, 1, 2].map(|i| (a[i] + b[i]) / 2.0)),
            _ => None,
        }
    }

    /// 任何可信的中点色（含只采到一端的）。
    fn any_mid(&self) -> Option<[f32; 3]> {
        match self {
            Axis::Single(c) => Some(*c),
            other => other.two_sided_mid(),
        }
    }
}

/// 归类一根轴的两端边缘色。
fn classify(a: Option<[f32; 3]>, b: Option<[f32; 3]>) -> Axis {
    match (a, b) {
        (Some(a), Some(b)) => {
            let d = dist_f(a, b);
            let mid = [0, 1, 2].map(|i| (a[i] + b[i]) / 2.0);
            if d <= FLAT_DIST {
                Axis::Flat(mid)
            } else if d <= GRAD_MAX_DIST {
                Axis::Grad(a, b)
            } else {
                Axis::Split(a, b)
            }
        }
        (Some(c), None) | (None, Some(c)) => Axis::Single(c),
        (None, None) => Axis::Missing,
    }
}

/// 采样填充区外圈得到背景模型。
///
/// 在填充区四边各取一条 3 像素宽的条带，只用没被 `obstacles`（别的行框）盖住的像素，各取众数色；
/// 左右（上下）两端同色 → 纯色，差异适中 → 沿该方向线性渐变，差异过大 → 当作碰到别的区域，忽略这一轴。
/// 四边都采不到样时，退回填充区里的众数色。
///
/// # 参数
/// - `rgba` / `width` / `height`：整张图（RGBA，行优先）。
/// - `fill`：要盖住的区域（图像坐标）。
/// - `obstacles`：不能参与采样的矩形（其他行框，已含外扩）。
///
/// # 返回
/// 背景模型。
///
/// ```ignore
/// let model = sample_background(&rgba, w, h, fill, &others);
/// let c = model.at(x, y);
/// ```
pub fn sample_background(
    rgba: &[u8],
    width: u32,
    height: u32,
    fill: PhysicalRect,
    obstacles: &[PhysicalRect],
) -> BgModel {
    let image = PhysicalRect::new(0, 0, width as i32, height as i32);
    let sample = |strip: PhysicalRect| strip_color(rgba, width, image, strip, obstacles);
    let edges = [
        sample(PhysicalRect::new(fill.x - RING, fill.y, RING, fill.height)),
        sample(PhysicalRect::new(fill.right(), fill.y, RING, fill.height)),
        sample(PhysicalRect::new(fill.x, fill.y - RING, fill.width, RING)),
        sample(PhysicalRect::new(fill.x, fill.bottom(), fill.width, RING)),
    ];
    let color = |e: Option<([f32; 3], usize)>| e.map(|(c, _)| c);
    let (mut h, v) = (
        classify(color(edges[0]), color(edges[1])),
        classify(color(edges[2]), color(edges[3])),
    );
    // 左右差异很大时，若上下条带的中位色恰好落在两端连线的中点上，说明是陡渐变而不是碰到了别的区域
    if let (Axis::Split(a, b), Some(vm)) = (&h, v.two_sided_mid())
        && dist_f(vm, [0, 1, 2].map(|i| (a[i] + b[i]) / 2.0)) <= GRAD_VERIFY_DIST
    {
        h = Axis::Grad(*a, *b);
    }
    let mut model = BgModel::flat(DEFAULT_BG);
    model.cx = fill.x as f32 + fill.width as f32 / 2.0;
    model.cy = fill.y as f32 + fill.height as f32 / 2.0;
    // 中心色：优先信左右两端都采到的轴（紧贴文字的同一水平带，最不容易混进别的内容），其次上下两端，
    // 最后才是只采到一端的轴
    let base = [&h, &v]
        .into_iter()
        .find_map(|a| a.two_sided_mid())
        .or_else(|| [&h, &v].into_iter().find_map(|a| a.any_mid()));
    let Some(base) = base else {
        // 两轴都不可信：取“得到最多像素支持”的那一类边缘色（相近的边缘色合并计数）；
        // 一个边缘都没有就用填充区里的众数色
        let found: Vec<([f32; 3], usize)> = edges.into_iter().flatten().collect();
        model.base = if found.is_empty() {
            let colors = (fill.y.max(0)..fill.bottom().min(height as i32))
                .flat_map(|y| (fill.x.max(0)..fill.right().min(width as i32)).map(move |x| (x, y)));
            mode_color(
                colors.map(|(x, y)| pixel(rgba, width, x, y)),
                BG_QUANT_SHIFT,
            )
            .unwrap_or(DEFAULT_BG.map(f32::from))
        } else {
            let support = |c: [f32; 3]| -> usize {
                found
                    .iter()
                    .filter(|(o, _)| dist_f(*o, c) <= FLAT_DIST * 2)
                    .map(|(_, n)| *n)
                    .sum()
            };
            found
                .iter()
                .max_by_key(|(c, _)| support(*c))
                .map_or(DEFAULT_BG.map(f32::from), |(c, _)| *c)
        };
        return model;
    };
    model.base = base;
    if let Axis::Grad(a, b) = h {
        model.gx = [0, 1, 2].map(|i| (b[i] - a[i]) / (fill.width + RING) as f32);
    }
    // 垂直渐变只在与中心色吻合时采用，避免上下条带跨到别的区域时把整行染歪
    if let Axis::Grad(a, b) = v
        && dist_f(base, [0, 1, 2].map(|i| (a[i] + b[i]) / 2.0)) <= FLAT_DIST * 2
    {
        model.gy = [0, 1, 2].map(|i| (b[i] - a[i]) / (fill.height + RING) as f32);
    }
    model
}

/// 抹掉一行原文：行框里整块涂成背景；行框外的外扩带（只为吃掉抗锯齿边缘）只改“像文字抗锯齿”的像素，
/// 即落在 背景色→前景色 连线上的像素，别的颜色（旁边的图形、边框线）原样保留，所以外扩带再宽也不会盖坏邻居。
///
/// # 参数
/// - `rgba` / `width` / `height`：整张图。
/// - `box_rect`：原文行框。
/// - `fill`：填充区（行框外扩后的范围）。
/// - `model`：背景模型。
/// - `fg`：原文前景色。
pub fn erase_text(
    rgba: &mut [u8],
    width: u32,
    height: u32,
    box_rect: PhysicalRect,
    fill: PhysicalRect,
    model: &BgModel,
    fg: [u8; 3],
) {
    let Some(clip) = fill.intersect(&PhysicalRect::new(0, 0, width as i32, height as i32)) else {
        return;
    };
    for y in clip.y..clip.bottom() {
        for x in clip.x..clip.right() {
            let inside =
                x >= box_rect.x && x < box_rect.right() && y >= box_rect.y && y < box_rect.bottom();
            let bg = model.at(x, y);
            if !inside && !looks_like_text_edge(pixel(rgba, width, x, y), bg, fg) {
                continue;
            }
            let i = (y as usize * width as usize + x as usize) * 4;
            rgba[i..i + 3].copy_from_slice(&bg);
            rgba[i + 3] = u8::MAX;
        }
    }
}

/// 像素落在 背景→前景 连线上时的覆盖率 `t`（0 = 背景，1 = 前景）；离连线太远（别的颜色）返回 `None`。
fn line_coverage(p: [u8; 3], bg: [u8; 3], fg: [u8; 3]) -> Option<f32> {
    let d = [0, 1, 2].map(|i| f32::from(p[i]) - f32::from(bg[i]));
    let f = [0, 1, 2].map(|i| f32::from(fg[i]) - f32::from(bg[i]));
    let denom: f32 = f.iter().map(|v| v * v).sum();
    if denom < 1.0 {
        return None;
    }
    let t = d.iter().zip(&f).map(|(a, b)| a * b).sum::<f32>() / denom;
    let residual: f32 = (0..3).map(|i| (d[i] - t * f[i]).abs()).sum();
    let reach: f32 = f.iter().map(|v| v.abs()).sum();
    (residual <= (reach * EDGE_RESIDUAL_RATIO).max(EDGE_MIN_RESIDUAL)).then_some(t)
}

/// 像素是不是文字抗锯齿的产物：落在 背景→前景 连线上，覆盖率在 `EDGE_T_MIN..=EDGE_T_MAX`。
fn looks_like_text_edge(p: [u8; 3], bg: [u8; 3], fg: [u8; 3]) -> bool {
    line_coverage(p, bg, fg).is_some_and(|t| (EDGE_T_MIN..=EDGE_T_MAX).contains(&t))
}

/// 在 `w × h` 的布尔掩码里去掉实心块：任一个 `SOLID_SIDE × SOLID_SIDE` 窗口整个都是真的，窗口中心就是实心；
/// 再把实心像素向四周扩 `SOLID_SIDE / 2` 像素一并去掉（吃掉块的边缘）。
fn remove_solid_blobs(mask: &[bool], w: usize, h: usize) -> Vec<bool> {
    let side = SOLID_SIDE as usize;
    let half = side / 2;
    // 积分图：window_sum(flags, x0, y0, x1, y1) = 区域内真值个数
    let integral = |flags: &dyn Fn(usize) -> bool| -> Vec<u32> {
        let mut sum = vec![0u32; (w + 1) * (h + 1)];
        for y in 0..h {
            for x in 0..w {
                sum[(y + 1) * (w + 1) + x + 1] = u32::from(flags(y * w + x))
                    + sum[y * (w + 1) + x + 1]
                    + sum[(y + 1) * (w + 1) + x]
                    - sum[y * (w + 1) + x];
            }
        }
        sum
    };
    let window = |sum: &[u32], x0: usize, y0: usize, x1: usize, y1: usize| -> u32 {
        sum[y1 * (w + 1) + x1] + sum[y0 * (w + 1) + x0]
            - sum[y0 * (w + 1) + x1]
            - sum[y1 * (w + 1) + x0]
    };
    if w < side || h < side || !mask.iter().any(|&m| m) {
        return mask.to_vec();
    }
    let ink_sum = integral(&|i| mask[i]);
    let mut solid = vec![false; w * h];
    for y in half..h - half {
        for x in half..w - half {
            let full = window(&ink_sum, x - half, y - half, x + half + 1, y + half + 1);
            solid[y * w + x] = full == (side * side) as u32;
        }
    }
    let solid_sum = integral(&|i| solid[i]);
    (0..w * h)
        .map(|i| {
            let (x, y) = (i % w, i / w);
            let (x0, y0) = (x.saturating_sub(half), y.saturating_sub(half));
            let (x1, y1) = ((x + half + 1).min(w), (y + half + 1).min(h));
            mask[i] && window(&solid_sum, x0, y0, x1, y1) == 0
        })
        .collect()
}

/// 量一行原文的实际墨迹范围：在 `search` 区域里找“落在背景→前景连线上、覆盖率过半”的像素，
/// 行方向取墨迹最多的那一段连续行（行与行之间哪怕只隔一行空白也分开，所以相邻行的墨迹接不进来），列方向取这些行里有墨迹的列。
///
/// OCR 行框与真实墨迹并不一致：PP-OCR 的 DB 检测会外扩（实测行框比墨迹高约 1.2–1.9 倍、上缘多出 3–9 像素），
/// 系统 OCR 在拉丁文上贴着墨迹、在中日韩上常常偏高或偏低，所以字号与基线必须以实测墨迹为准。
///
/// # 参数
/// - `rgba` / `width` / `height`：整张图。
/// - `search`：搜索范围（行框上下放宽、并避开邻行；横向只用 `anchor` 的范围）。
/// - `anchor`：这一行的 OCR 行框（横向范围，以及“中间一段列”的参照）。
/// - `bg` / `fg`：该行的背景模型与前景色。
///
/// # 返回
/// 墨迹外接矩形；没有足够的墨迹返回 `None`。
///
/// ```ignore
/// let ink = measure_ink(&rgba, w, h, search, box_rect, &bg, fg);
/// ```
pub fn measure_ink(
    rgba: &[u8],
    width: u32,
    height: u32,
    search: PhysicalRect,
    anchor: PhysicalRect,
    bg: &BgModel,
    fg: [u8; 3],
) -> Option<PhysicalRect> {
    let columns = PhysicalRect::new(
        anchor.x - INK_COLUMN_SLACK,
        search.y,
        anchor.width + 2 * INK_COLUMN_SLACK,
        search.height,
    );
    let clip = search.intersect(&columns)?.intersect(&PhysicalRect::new(
        0,
        0,
        width as i32,
        height as i32,
    ))?;
    if color_distance(fg, bg.mean()) < MIN_INK_DISTANCE {
        return None;
    }
    // 墨迹掩码：落在背景→前景连线上的像素，再去掉“实心块”（图标、圆点、色条：7×7 全是墨迹的地方及其周围 3 像素，
    // 文字笔画不会这么粗），否则与文字同色的图标会把行高撑大
    let (cw, ch) = (clip.width as usize, clip.height as usize);
    let raw: Vec<bool> = (clip.y..clip.bottom())
        .flat_map(|y| (clip.x..clip.right()).map(move |x| (x, y)))
        .map(|(x, y)| {
            line_coverage(pixel(rgba, width, x, y), bg.at(x, y), fg)
                .is_some_and(|t| t >= INK_MEASURE_T)
        })
        .collect();
    let mask = remove_solid_blobs(&raw, cw, ch);
    let is_ink = |x: i32, y: i32| mask[(y - clip.y) as usize * cw + (x - clip.x) as usize];
    // 判断“哪几行是文字行”只看中间一段列：行框左右两端常常外扩进旁边的图标 / 色块（它们的颜色可能恰好与文字相近）
    let margin = (anchor.height / CORE_MARGIN_DIV).max(2);
    let (core_l, core_r) = if clip.width > 4 * margin {
        (clip.x + margin, clip.right() - margin)
    } else {
        (clip.x, clip.right())
    };
    let counts: Vec<usize> = (clip.y..clip.bottom())
        .map(|y| (core_l..core_r).filter(|&x| is_ink(x, y)).count())
        .collect();
    // 连续的有墨迹行段，取墨迹总量最大的一段：(起行, 止行, 墨迹像素总数)
    let mut best: Option<(usize, usize, usize)> = None;
    let mut run: Option<(usize, usize, usize)> = None;
    for (i, &n) in counts.iter().enumerate() {
        if n < MIN_INK_ROW_PIXELS || n as f32 > MAX_INK_ROW_FRACTION * (core_r - core_l) as f32 {
            continue;
        }
        run = Some(match run {
            Some((start, end, sum)) if i <= end + 1 => (start, i, sum + n),
            Some(done) => {
                if best.is_none_or(|b| done.2 > b.2) {
                    best = Some(done);
                }
                (i, i, n)
            }
            None => (i, i, n),
        });
    }
    if let Some(done) = run
        && best.is_none_or(|b| done.2 > b.2)
    {
        best = Some(done);
    }
    let (first, last, _) = best?;
    let (top, bottom) = (clip.y + first as i32, clip.y + last as i32);
    // 左右边界：从中间一段里有墨迹的列出发，向两侧延伸，遇到比字间距大得多的空隙就停（隔开旁边的图标）
    let has_ink_col = |x: i32| (top..=bottom).any(|y| is_ink(x, y));
    let gap_limit = (((bottom - top + 1) as f32 * INK_GAP_RATIO) as i32).max(MIN_INK_GAP);
    let inner: Vec<i32> = (core_l..core_r).filter(|&x| has_ink_col(x)).collect();
    let (mut left, mut right) = (*inner.first()?, *inner.last()?);
    let mut gap = 0;
    for x in (clip.x..left).rev() {
        if has_ink_col(x) {
            left = x;
            gap = 0;
        } else {
            gap += 1;
            if gap > gap_limit {
                break;
            }
        }
    }
    gap = 0;
    for x in right + 1..clip.right() {
        if has_ink_col(x) {
            right = x;
            gap = 0;
        } else {
            gap += 1;
            if gap > gap_limit {
                break;
            }
        }
    }
    Some(PhysicalRect::new(
        left,
        top,
        right - left + 1,
        bottom - top + 1,
    ))
}

/// 文字的前景色与字重。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Ink {
    /// 文字颜色。
    pub fg: [u8; 3],
    /// 原文是否像粗体。
    pub bold: bool,
}

/// 估计一段原文的前景色与字重。
///
/// 前景色：在各行框内找与背景色差最大的那批像素，取其中最常见的颜色簇（不被抗锯齿的过渡色拉淡）；
/// 找不到墨迹或与背景太接近时用黑 / 白对比色。字重：把“笔画像素”按行连成水平线段，线段长度的中位数
/// 占行高的比例达到阈值判为粗体。
///
/// # 参数
/// - `rgba` / `width` / `height`：整张图。
/// - `boxes`：该段各行框（图像坐标）。
/// - `models`：与 `boxes` 一一对应的背景模型。
/// - `full_line_h`：算笔画粗细比例用的“满行高度”（含上下各 1 像素边距）；`None` 时用各行框高度的平均值。
///   行框是按墨迹量出来的时候要传它：缺下伸字母的行墨迹矮，直接用平均高度会把比例算大、把常规字误判成粗体。
///
/// # 返回
/// 前景色与是否粗体。
///
/// ```ignore
/// let ink = estimate_ink(&rgba, w, h, &boxes, &models, None);
/// ```
pub fn estimate_ink(
    rgba: &[u8],
    width: u32,
    height: u32,
    boxes: &[PhysicalRect],
    models: &[BgModel],
    full_line_h: Option<f32>,
) -> Ink {
    let image = PhysicalRect::new(0, 0, width as i32, height as i32);
    let rows = |r: PhysicalRect| {
        (r.y..r.bottom()).flat_map(move |y| (r.x..r.right()).map(move |x| (x, y)))
    };
    let clipped: Vec<(PhysicalRect, &BgModel)> = boxes
        .iter()
        .zip(models)
        .filter_map(|(b, m)| b.intersect(&image).map(|c| (c, m)))
        .collect();
    let dist = |x: i32, y: i32, m: &BgModel| color_distance(pixel(rgba, width, x, y), m.at(x, y));
    let max_dist = clipped
        .iter()
        .flat_map(|(r, m)| rows(*r).map(move |(x, y)| dist(x, y, m)))
        .max()
        .unwrap_or(0);
    let mean_bg = clipped.first().map_or(DEFAULT_BG, |(_, m)| m.mean());
    if max_dist < MIN_INK_DISTANCE {
        return Ink {
            fg: contrast_color(mean_bg),
            bold: false,
        };
    }
    let keep = (max_dist as f32 * INK_KEEP_RATIO) as i32;
    let strong = clipped.iter().flat_map(|(r, m)| {
        rows(*r)
            .filter(move |&(x, y)| dist(x, y, m) >= keep)
            .map(|(x, y)| pixel(rgba, width, x, y))
    });
    let mut fg = mode_color(strong, FG_QUANT_SHIFT)
        .map_or(contrast_color(mean_bg), |c| c.map(|v| v.round() as u8));
    if (luma(fg) - luma(mean_bg)).abs() < MIN_LUMA_DIFF {
        fg = contrast_color(mean_bg);
    }
    let (stroke_w, mean_h) = stem_ratio(rgba, width, &clipped, max_dist);
    let line_h = full_line_h.unwrap_or(mean_h).max(1.0);
    let ratio = stroke_w / line_h;
    let threshold = if line_h < SMALL_LINE_H {
        BOLD_STEM_RATIO_SMALL
    } else {
        BOLD_STEM_RATIO
    };
    Ink {
        fg,
        bold: line_h >= TINY_LINE_H && ratio >= threshold,
    }
}

/// 笔画平均宽度（像素）≈ 2 × 墨迹面积 / 墨迹周长。
///
/// 墨迹覆盖率取“与背景色差 / 最大色差”（抗锯齿像素按比例计入，所以是亚像素精度）；周长用覆盖率的总变差
/// （相邻像素覆盖率之差的绝对值之和）估计，每行框外多算一圈空白。
///
/// # 参数
/// - `rgba` / `width`：整张图。
/// - `lines`：各行框（已裁到图内）及其背景模型。
/// - `max_dist`：这些行框里与背景的最大色差。
///
/// # 返回
/// `(笔画平均宽度, 各行框平均高度)`；没有墨迹时宽度为 0。
fn stem_ratio(
    rgba: &[u8],
    width: u32,
    lines: &[(PhysicalRect, &BgModel)],
    max_dist: i32,
) -> (f32, f32) {
    let floor = max_dist as f32 * STEM_NOISE_FLOOR;
    let (mut area, mut variation, mut height_sum) = (0.0_f32, 0.0_f32, 0.0_f32);
    for (r, m) in lines {
        let cov = |x: i32, y: i32| -> f32 {
            if x < r.x || y < r.y || x >= r.right() || y >= r.bottom() {
                return 0.0;
            }
            // 低于噪声底的色差（背景模型的误差、压缩噪点）不算墨迹
            let d = color_distance(pixel(rgba, width, x, y), m.at(x, y)) as f32 - floor;
            (d / (max_dist as f32 - floor)).clamp(0.0, 1.0)
        };
        for y in r.y..=r.bottom() {
            for x in r.x..=r.right() {
                let c = cov(x, y);
                area += c;
                variation += (cov(x + 1, y) - c).abs() + (cov(x, y + 1) - c).abs();
            }
        }
        height_sum += r.height as f32;
    }
    let line_h = (height_sum / lines.len().max(1) as f32).max(1.0);
    if variation <= 0.0 {
        return (0.0, line_h);
    }
    (2.0 * area / variation, line_h)
}

/// 把字形覆盖率位图按前景色叠到图上（只写 `clip` 与图像的交集内的像素）。
///
/// # 参数
/// - `rgba` / `width` / `height`：整张图。
/// - `bitmap`：字形覆盖率位图。
/// - `origin`：位图左上角在图像里的位置。
/// - `fg`：前景色。
/// - `clip`：只允许写入的区域。
pub fn blend_bitmap(
    rgba: &mut [u8],
    width: u32,
    height: u32,
    bitmap: &TextBitmap,
    origin: (i32, i32),
    fg: [u8; 3],
    clip: PhysicalRect,
) {
    let Some(clip) = clip.intersect(&PhysicalRect::new(0, 0, width as i32, height as i32)) else {
        return;
    };
    for row in 0..bitmap.height as i32 {
        let y = origin.1 + row;
        if y < clip.y || y >= clip.bottom() {
            continue;
        }
        for col in 0..bitmap.width as i32 {
            let x = origin.0 + col;
            if x < clip.x || x >= clip.right() {
                continue;
            }
            let cov =
                u32::from(bitmap.coverage[(row as usize) * bitmap.width as usize + col as usize]);
            if cov == 0 {
                continue;
            }
            let i = (y as usize * width as usize + x as usize) * 4;
            for (c, v) in fg.iter().enumerate() {
                let dst = u32::from(rgba[i + c]);
                rgba[i + c] = ((dst * (255 - cov) + u32::from(*v) * cov + 127) / 255) as u8;
            }
            rgba[i + 3] = u8::MAX;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 单色底图。
    fn solid(w: u32, h: u32, rgb: [u8; 3]) -> Vec<u8> {
        (0..w * h)
            .flat_map(|_| [rgb[0], rgb[1], rgb[2], 255])
            .collect()
    }

    /// 在图上涂一块矩形。
    fn paint(img: &mut [u8], w: u32, r: PhysicalRect, c: [u8; 3]) {
        for y in r.y..r.bottom() {
            for x in r.x..r.right() {
                let i = (y as usize * w as usize + x as usize) * 4;
                img[i..i + 3].copy_from_slice(&c);
            }
        }
    }

    /// 读像素。
    fn px(img: &[u8], w: u32, x: i32, y: i32) -> [u8; 3] {
        pixel(img, w, x, y)
    }

    /// 纯色背景：框外一圈全是同一种颜色 → 平坦模型；文字压在框内不影响取色。
    #[test]
    fn background_flat_ignores_text_inside() {
        let mut img = solid(80, 40, [240, 200, 180]);
        paint(&mut img, 80, PhysicalRect::new(20, 15, 30, 8), [20, 20, 20]);
        let model = sample_background(&img, 80, 40, PhysicalRect::new(18, 13, 34, 12), &[]);
        assert!(!model.is_gradient());
        assert_eq!(model.mean(), [240, 200, 180]);
        assert_eq!(model.at(0, 0), [240, 200, 180]);
    }

    /// 渐变背景：左右两端颜色不同 → 沿水平线性渐变，框内按位置插值。
    #[test]
    fn background_horizontal_gradient() {
        let (w, h) = (100u32, 40u32);
        let mut img = solid(w, h, [0, 0, 0]);
        for y in 0..h as i32 {
            for x in 0..w as i32 {
                let t = x as f32 / (w - 1) as f32;
                let c = [(40.0 + 120.0 * t) as u8, 60, (200.0 - 100.0 * t) as u8];
                paint(&mut img, w, PhysicalRect::new(x, y, 1, 1), c);
            }
        }
        let fill = PhysicalRect::new(20, 15, 60, 10);
        let model = sample_background(&img, w, h, fill, &[]);
        assert!(model.is_gradient());
        for x in [22, 50, 78] {
            let want = px(&img, w, x, 20);
            let got = model.at(x, 20);
            assert!(
                color_distance(want, got) <= 12,
                "x={x} 期望 {want:?} 实际 {got:?}"
            );
        }
    }

    /// 别的行框盖住的像素不参与采样：相邻行的文字墨迹不会污染背景。
    #[test]
    fn background_skips_obstacles() {
        let mut img = solid(80, 60, [250, 250, 250]);
        // 上方紧邻一行深色“文字”（贴着填充区上沿），它的行框作为障碍传入；左右两侧也是同样的黑块
        let others = [
            PhysicalRect::new(10, 2, 60, 14),
            PhysicalRect::new(2, 16, 8, 12),
            PhysicalRect::new(70, 16, 8, 12),
        ];
        for o in others {
            paint(&mut img, 80, o, [0, 0, 0]);
        }
        let fill = PhysicalRect::new(10, 16, 60, 12);
        let model = sample_background(&img, 80, 60, fill, &others);
        assert!(!model.is_gradient());
        assert_eq!(model.mean(), [250, 250, 250]);
        // 不排除障碍：上、左、右三条带都被黑块占据，背景被拉黑
        let tainted = sample_background(&img, 80, 60, fill, &[]);
        assert!(luma(tainted.mean()) < 200);
    }

    /// 一侧紧贴另一块区域（左白右橙差异过大）：忽略这一轴，用上下两边的颜色。
    #[test]
    fn background_split_axis_is_ignored() {
        let mut img = solid(100, 40, [250, 130, 60]);
        paint(
            &mut img,
            100,
            PhysicalRect::new(0, 0, 19, 40),
            [255, 255, 255],
        );
        let fill = PhysicalRect::new(20, 15, 60, 10);
        let model = sample_background(&img, 100, 40, fill, &[]);
        // 左条带 x=17..20 里两列是白的、右条带全橙，差异过大：水平轴被忽略，只信上下两边的橙色
        assert!(!model.is_gradient());
        assert!(
            color_distance(model.at(50, 20), [250, 130, 60]) <= 40,
            "{:?}",
            model.at(50, 20)
        );
    }

    /// 四边都采不到样（填充区占满整图）：退回填充区里的众数色。
    #[test]
    fn background_falls_back_to_fill_mode() {
        let mut img = solid(40, 20, [10, 10, 30]);
        paint(&mut img, 40, PhysicalRect::new(5, 5, 8, 4), [250, 250, 250]);
        let model = sample_background(&img, 40, 20, PhysicalRect::new(0, 0, 40, 20), &[]);
        assert_eq!(model.mean(), [10, 10, 30]);
    }

    /// 前景色：浅底深字、深底浅字、无墨迹 / 太接近退回对比色；取最浓的那一簇而不是被抗锯齿拉淡。
    #[test]
    fn ink_colors() {
        let rect = PhysicalRect::new(10, 10, 20, 10);
        let ink_of = |img: &[u8], bg: [u8; 3]| {
            estimate_ink(img, 40, 30, &[rect], &[BgModel::flat(bg)], None)
        };
        let mut img = solid(40, 30, [240, 240, 240]);
        paint(&mut img, 40, PhysicalRect::new(12, 12, 16, 6), [20, 30, 40]);
        // 几个抗锯齿过渡像素
        paint(
            &mut img,
            40,
            PhysicalRect::new(11, 12, 1, 6),
            [130, 135, 140],
        );
        assert_eq!(ink_of(&img, [240, 240, 240]).fg, [20, 30, 40]);

        let mut dark = solid(40, 30, [10, 10, 30]);
        paint(
            &mut dark,
            40,
            PhysicalRect::new(12, 12, 16, 6),
            [250, 250, 250],
        );
        assert_eq!(ink_of(&dark, [10, 10, 30]).fg, [250, 250, 250]);

        let flat = solid(40, 30, [250, 250, 250]);
        assert_eq!(ink_of(&flat, [250, 250, 250]).fg, [0, 0, 0]);
        let mut weak = solid(40, 30, [100, 100, 100]);
        paint(
            &mut weak,
            40,
            PhysicalRect::new(12, 12, 16, 6),
            [130, 130, 130],
        );
        assert_eq!(ink_of(&weak, [100, 100, 100]).fg, [255, 255, 255]);
    }

    /// 字重：1 像素宽的竖笔画判为常规，3 像素宽的判为粗体。
    #[test]
    fn ink_boldness_from_stem_width() {
        let rect = PhysicalRect::new(2, 2, 60, 12);
        let draw = |stem: i32| {
            let mut img = solid(64, 16, [255, 255, 255]);
            for k in 0..8 {
                paint(
                    &mut img,
                    64,
                    PhysicalRect::new(4 + k * 7, 3, stem, 10),
                    [0, 0, 0],
                );
            }
            estimate_ink(
                &img,
                64,
                16,
                &[rect],
                &[BgModel::flat([255, 255, 255])],
                None,
            )
            .bold
        };
        assert!(!draw(1));
        assert!(draw(3));
    }

    /// 抹字：行框整块涂背景；外扩带只改“背景→前景连线上”的抗锯齿像素，别的颜色（图形、边框线）原样保留；
    /// 填充区外分毫不动，区域完全在图外也不崩。
    #[test]
    fn erase_text_is_selective_outside_the_box() {
        let (w, h) = (40u32, 24u32);
        let bg = [240, 240, 240];
        let mut img = solid(w, h, bg);
        let (box_rect, fill) = (
            PhysicalRect::new(10, 8, 20, 8),
            PhysicalRect::new(8, 6, 24, 12),
        );
        paint(&mut img, w, box_rect, [20, 20, 20]);
        // 外扩带里：一个灰色抗锯齿像素、一条绿色边框线
        paint(&mut img, w, PhysicalRect::new(9, 10, 1, 1), [130, 130, 130]);
        paint(&mut img, w, PhysicalRect::new(31, 6, 1, 12), [0, 160, 0]);
        // 填充区外一个红点
        paint(&mut img, w, PhysicalRect::new(2, 2, 1, 1), [255, 0, 0]);
        erase_text(
            &mut img,
            w,
            h,
            box_rect,
            fill,
            &BgModel::flat(bg),
            [20, 20, 20],
        );
        assert_eq!(px(&img, w, 15, 12), bg, "行框内整块涂成背景");
        assert_eq!(px(&img, w, 9, 10), bg, "外扩带里的抗锯齿像素被抹掉");
        assert_eq!(px(&img, w, 31, 10), [0, 160, 0], "外扩带里的别的颜色保留");
        assert_eq!(px(&img, w, 2, 2), [255, 0, 0], "填充区外不动");
        erase_text(
            &mut img,
            w,
            h,
            box_rect,
            PhysicalRect::new(100, 100, 5, 5),
            &BgModel::flat(bg),
            [0, 0, 0],
        );
    }

    /// 量墨迹：只认背景→前景连线上的像素；搜索范围比墨迹大得多也量得准，邻行与别的颜色不计入；
    /// 没有墨迹（纯色）返回 None。
    #[test]
    fn measure_ink_ignores_padding_and_foreign_colors() {
        let (w, h) = (80u32, 60u32);
        let mut img = solid(w, h, [240, 240, 240]);
        // 本行墨迹 (20..60, 25..38) 用细竖笔画模拟文字，下方隔开几行有邻行墨迹，右侧有个绿色图形
        for k in 0..10 {
            paint(
                &mut img,
                w,
                PhysicalRect::new(20 + k * 4, 25, 2, 13),
                [20, 20, 20],
            );
            paint(
                &mut img,
                w,
                PhysicalRect::new(20 + k * 4, 42, 2, 10),
                [20, 20, 20],
            );
        }
        paint(&mut img, w, PhysicalRect::new(66, 20, 8, 25), [0, 160, 0]);
        let bg = BgModel::flat([240, 240, 240]);
        let fg = [20, 20, 20];
        let near = measure_ink(
            &img,
            w,
            h,
            PhysicalRect::new(10, 12, 62, 28),
            PhysicalRect::new(14, 22, 52, 20),
            &bg,
            fg,
        );
        assert_eq!(near, Some(PhysicalRect::new(20, 25, 38, 13)));
        // 搜索范围更大、包含邻行：邻行隔着空隙，仍只取墨迹最多的本行（13 行 > 10 行）
        let wide = measure_ink(
            &img,
            w,
            h,
            PhysicalRect::new(10, 12, 70, 46),
            PhysicalRect::new(14, 22, 52, 20),
            &bg,
            fg,
        );
        assert_eq!(wide, Some(PhysicalRect::new(20, 25, 38, 13)));
        let blank = solid(w, h, [240; 3]);
        assert_eq!(
            measure_ink(
                &blank,
                w,
                h,
                PhysicalRect::new(0, 0, 80, 60),
                PhysicalRect::new(14, 22, 52, 20),
                &bg,
                fg
            ),
            None
        );
    }

    /// 量墨迹：与文字同色的实心图标（圆点）不计入；文字笔画（宽度远小于 7 像素）不受影响。
    #[test]
    fn measure_ink_drops_solid_blobs() {
        let (w, h) = (120u32, 60u32);
        let mut img = solid(w, h, [100, 60, 200]);
        let white = [255, 255, 255];
        // 左边一个与文字同色的白色实心块（比文字高），右边是细笔画“文字”
        paint(&mut img, w, PhysicalRect::new(10, 12, 24, 30), white);
        for k in 0..8 {
            paint(&mut img, w, PhysicalRect::new(50 + k * 6, 22, 2, 12), white);
        }
        let bg = BgModel::flat([100, 60, 200]);
        let ink = measure_ink(
            &img,
            w,
            h,
            PhysicalRect::new(6, 8, 108, 40),
            PhysicalRect::new(8, 14, 104, 26),
            &bg,
            white,
        );
        assert_eq!(ink, Some(PhysicalRect::new(50, 22, 44, 12)));
    }

    /// 叠字只在 clip 内落笔，位图伸出 clip 的部分一个像素也不动。
    #[test]
    fn blend_respects_clip() {
        let (w, h) = (30u32, 20u32);
        let mut img = solid(w, h, [1, 2, 3]);
        let region = PhysicalRect::new(5, 5, 10, 6);
        let bmp = TextBitmap {
            width: 12,
            height: 8,
            coverage: vec![255; 96],
        };
        blend_bitmap(&mut img, w, h, &bmp, (3, 3), [0, 0, 0], region);
        assert_eq!(px(&img, w, 5, 5), [0, 0, 0]);
        assert_eq!(px(&img, w, 4, 5), [1, 2, 3]);
        assert_eq!(px(&img, w, 14, 10), [0, 0, 0]);
        assert_eq!(px(&img, w, 15, 10), [1, 2, 3]);
        assert_eq!(px(&img, w, 14, 11), [1, 2, 3]);
    }

    /// 真实字体（GDI）标定过的字重判断：常规字重判常规、粗体判粗体（行高足够时）。
    #[cfg(windows)]
    #[test]
    fn ink_boldness_matches_real_fonts() {
        for family in ["Segoe UI", "Arial", "Microsoft YaHei UI"] {
            for px in [16u32, 24, 32] {
                for bold in [false, true] {
                    let b = snow_platform::text_raster::rasterize_text(
                        "Directly manage risk assess",
                        family,
                        px as f32,
                        bold,
                    )
                    .unwrap();
                    let (w, h) = (b.width as i32 + 8, b.height as i32 + 8);
                    let mut img = solid(w as u32, h as u32, [255, 255, 255]);
                    let (mut x0, mut y0, mut x1, mut y1) = (w, h, -1, -1);
                    for yy in 0..b.height as i32 {
                        for xx in 0..b.width as i32 {
                            let c = b.coverage[(yy * b.width as i32 + xx) as usize];
                            paint(
                                &mut img,
                                w as u32,
                                PhysicalRect::new(xx + 4, yy + 4, 1, 1),
                                [255 - c; 3],
                            );
                            if c > 48 {
                                (x0, x1) = (x0.min(xx + 4), x1.max(xx + 4));
                                (y0, y1) = (y0.min(yy + 4), y1.max(yy + 4));
                            }
                        }
                    }
                    let rect = PhysicalRect::new(x0 - 1, y0 - 1, x1 - x0 + 3, y1 - y0 + 3);
                    let ink = estimate_ink(
                        &img,
                        w as u32,
                        h as u32,
                        &[rect],
                        &[BgModel::flat([255, 255, 255])],
                        None,
                    );
                    assert_eq!(ink.bold, bold, "{family} {px}px bold={bold}");
                }
            }
        }
    }
}
