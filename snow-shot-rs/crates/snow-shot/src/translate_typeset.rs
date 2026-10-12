//! 截图翻译“原位替换”的排版：以 OCR 行框为锚点把译文流排进各行（槽），不碰像素。
//!
//! 流程：行框外扩成填充区（相邻行框之间对半让位，保证互不重叠）→ 推断对齐方式 → 按项目符号把槽分组 →
//! 从原文行高推出字号，逐像素缩小直到译文放进全部槽，缩到下限仍放不下才截断加省略号。
//! 字宽、字形度量都由调用方注入，所以排版全部能离屏确定性测试。

use snow_ui::shell::geometry::PhysicalRect;
use std::ops::Range;

/// 外扩距离占行高的比例（只为吃掉抗锯齿边缘，别扩到框外的图形上）。
const PAD_FACTOR: f32 = 0.1;
/// “满行”行高相对中位数最多放大的倍数（缺下伸字母的矮行只会矮一点，更大的差距是混进来的标题）。
const MAX_LINE_GROWTH: f32 = 1.2;
/// “满行”行高取这个分位（0..1，见 [`line_height_estimate`]）。
const UPPER_QUARTILE: f32 = 0.75;
/// 外扩距离下限（像素）。
const MIN_PAD: i32 = 2;
/// 外扩距离上限（像素）。
const MAX_PAD: i32 = 3;
/// 判定几行“左 / 中 / 右对齐”时容许的偏差占行高的比例。
const ALIGN_TOL_FACTOR: f32 = 0.5;
/// 对齐偏差下限（像素）。
const MIN_ALIGN_TOL: i32 = 3;
/// 单行文字的中线与图像中线相差不超过图宽的该比例时，视为居中排版。
const CENTER_SNAP_RATIO: f32 = 0.02;
/// 居中判定的最小容差（像素）。
const MIN_CENTER_SNAP: i32 = 2;
/// 字形墨迹高度允许超出槽高度的像素数（取整误差）。
const INK_SLACK: f32 = 1.0;
/// 截断时补在末尾的省略号。
const ELLIPSIS: char = '…';
/// 不能出现在行首的标点（折行时并入上一个单元）。
const NO_LINE_START: &str = "，。、！？；：）】》」』”’…,.!?;:)]}";
/// 近似空格宽度用的字符。
const SPACE: char = ' ';
/// 无条件视为项目符号的字符。
const BULLET_MARKS: &str = "•·●○▪■◦‣∙";
/// 后面必须跟空白才视为项目符号的字符（避免把负号、破折号、脚注星号当成符号）。
const SPACED_BULLET_MARKS: &str = "*-–—";
/// 比较折行宽度时容许的浮点误差。
const WRAP_EPS: f32 = 0.01;

/// 一个行框向四个方向想外扩的像素数。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Ext {
    /// 向左。
    pub left: i32,
    /// 向上。
    pub top: i32,
    /// 向右。
    pub right: i32,
    /// 向下。
    pub bottom: i32,
}

/// 段内文字的对齐方式（由原文各行框的边界推断）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Align {
    /// 左对齐。
    Left,
    /// 居中。
    Center,
    /// 右对齐。
    Right,
}

/// 字体度量（像素，针对某个具体字号）。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FontMetrics {
    /// 基线到位图顶部的距离。
    pub ascent: f32,
    /// 常见字形墨迹顶部到基线的距离。
    pub ink_top: f32,
    /// 常见字形墨迹（含下伸部分）底部到基线的距离。
    pub ink_bottom: f32,
}

impl FontMetrics {
    /// 墨迹总高度（像素）。
    pub fn ink_height(&self) -> f32 {
        self.ink_top + self.ink_bottom
    }
}

/// 排版槽：段内一行原文对应的可用区域。
#[derive(Debug, Clone, PartialEq)]
pub struct SlotGeom {
    /// 本行可用于文字的宽度（像素）。
    pub avail: f32,
    /// 本行填充区的高度（像素）。
    pub height: f32,
    /// 原文本行以什么项目符号开头。
    pub bullet: Option<char>,
}

/// 排好的一行译文。
#[derive(Debug, Clone, PartialEq)]
pub struct SlotLine {
    /// 放进哪一个槽（段内行序号）。
    pub slot: usize,
    /// 这一行的文字（不含项目符号）。
    pub text: String,
    /// 这一行行首要补画的项目符号。
    pub bullet: Option<char>,
    /// 项目符号占掉的行首宽度（像素），文字从它之后开始。
    pub indent: f32,
}

/// 一段译文的排版结果。
#[derive(Debug, Clone, PartialEq)]
pub struct Typeset {
    /// 选定的字号（整像素，GDI 本来就按整数字号出字）。
    pub font_px: u32,
    /// 排好的各行。
    pub lines: Vec<SlotLine>,
    /// 是否因放不下被截断（有行带省略号）。
    pub truncated: bool,
}

/// 折行的最小单元：一个拉丁单词，或一个中日韩字符（连同并入的行首禁则标点）。
struct Unit {
    /// 单元文字。
    text: String,
    /// 与上一个单元之间是否隔着空白。
    space_before: bool,
}

/// 是否中日韩类字符（可在任意两字之间折行）。
pub fn is_cjk_like(c: char) -> bool {
    matches!(c as u32,
        0x2E80..=0x9FFF | 0xAC00..=0xD7AF | 0xF900..=0xFAFF | 0xFF00..=0xFFEF)
}

/// 把一行文字切成折行单元。
fn split_units(line: &str) -> Vec<Unit> {
    let mut units: Vec<Unit> = Vec::new();
    let mut word = String::new();
    let mut word_space = false;
    let mut pending_space = false;
    let flush = |units: &mut Vec<Unit>, word: &mut String, space: bool| {
        if !word.is_empty() {
            units.push(Unit {
                text: std::mem::take(word),
                space_before: space,
            });
        }
    };
    for c in line.chars() {
        if c.is_whitespace() {
            flush(&mut units, &mut word, word_space);
            pending_space = true;
        } else if is_cjk_like(c) || NO_LINE_START.contains(c) && word.is_empty() {
            flush(&mut units, &mut word, word_space);
            match units.last_mut() {
                Some(prev) if NO_LINE_START.contains(c) && !pending_space => prev.text.push(c),
                _ => units.push(Unit {
                    text: c.to_string(),
                    space_before: pending_space,
                }),
            }
            pending_space = false;
        } else {
            if word.is_empty() {
                word_space = pending_space;
                pending_space = false;
            }
            word.push(c);
        }
    }
    flush(&mut units, &mut word, word_space);
    units
}

/// 一段文字在给定字号下的宽度（像素）。
pub fn text_width(text: &str, font_px: f32, adv: &dyn Fn(char) -> f32) -> f32 {
    text.chars().map(adv).sum::<f32>() * font_px
}

/// 按宽度折行：中日韩逐字可断，拉丁文按词断，过长的词按字符硬切；`\n` 强制换行。
///
/// # 参数
/// - `text`：译文。
/// - `max_width`：可用宽度（像素）。
/// - `font_px`：字号（像素）。
/// - `adv`：单字前进宽度（以 1 像素字号为单位）。
///
/// # 返回
/// 折行后的各行。
///
/// ```ignore
/// let lines = wrap_text("你好世界", 40.0, 20.0, &|_| 1.0);
/// assert_eq!(lines, ["你好", "世界"]);
/// ```
pub fn wrap_text(
    text: &str,
    max_width: f32,
    font_px: f32,
    adv: &dyn Fn(char) -> f32,
) -> Vec<String> {
    wrap_with(text, &|_| max_width, font_px, adv)
}

/// 折行，但每一行的可用宽度可以不同（第 `n` 行用 `width_at(n)`，行序号从 0 起）。
///
/// # 参数
/// - `text` / `font_px` / `adv`：同 [`wrap_text`]。
/// - `width_at`：给定行序号，返回该行可用宽度。
///
/// # 返回
/// 折行后的各行（可能比槽多，由调用方判断放不放得下）。
pub fn wrap_with(
    text: &str,
    width_at: &dyn Fn(usize) -> f32,
    font_px: f32,
    adv: &dyn Fn(char) -> f32,
) -> Vec<String> {
    let space_w = text_width(&SPACE.to_string(), font_px, adv);
    let mut lines: Vec<String> = Vec::new();
    for raw in text.split('\n') {
        let mut line = String::new();
        let mut line_w = 0.0_f32;
        for unit in split_units(raw) {
            let max_width = width_at(lines.len());
            let sep = if !line.is_empty() && unit.space_before {
                space_w
            } else {
                0.0
            };
            let unit_w = text_width(&unit.text, font_px, adv);
            if line.is_empty() || line_w + sep + unit_w <= max_width + WRAP_EPS {
                if line.is_empty() && unit_w > max_width + WRAP_EPS {
                    // 单个词比整行还宽：按字符硬切
                    for c in unit.text.chars() {
                        let cw = text_width(&c.to_string(), font_px, adv);
                        if !line.is_empty() && line_w + cw > width_at(lines.len()) + WRAP_EPS {
                            lines.push(std::mem::take(&mut line));
                            line_w = 0.0;
                        }
                        line.push(c);
                        line_w += cw;
                    }
                } else {
                    if sep > 0.0 {
                        line.push(SPACE);
                    }
                    line.push_str(&unit.text);
                    line_w += sep + unit_w;
                }
            } else {
                lines.push(std::mem::take(&mut line));
                line = unit.text.clone();
                line_w = unit_w;
            }
        }
        lines.push(line);
    }
    lines
}

/// 估计段内“满行”的行框高度：取偏上的 75% 分位。
///
/// OCR 行框是贴着墨迹画的，没有下伸字母（g p y）的拉丁行会矮一截，所以不能直接取中位数，也不能取最大值
/// （个别特大的标题 / 图标会把它带偏）：取 75% 分位，并限制在中位数的 1.2 倍以内。空切片返回 0。
///
/// # 参数
/// - `heights`：各行框高度。
///
/// ```ignore
/// assert_eq!(line_height_estimate(&[12, 12, 15]), 14);
/// ```
pub fn line_height_estimate(heights: &[i32]) -> i32 {
    let mut v = heights.to_vec();
    v.sort_unstable();
    let ix = ((v.len().saturating_sub(1)) as f32 * UPPER_QUARTILE + 0.5) as usize;
    let upper = v.get(ix).copied().unwrap_or(0);
    let cap = (median_i32(&v) as f32 * MAX_LINE_GROWTH).round() as i32;
    upper.min(cap.max(median_i32(&v)))
}

/// 取中位数（偶数个时取偏大的那个）；空切片返回 0。
pub fn median_i32(values: &[i32]) -> i32 {
    let mut v = values.to_vec();
    v.sort_unstable();
    v.get(v.len() / 2).copied().unwrap_or(0)
}

/// 段内行高对应的外扩距离。
///
/// # 参数
/// - `line_h`：段内行框高度的中位数（像素）。
///
/// # 返回
/// 外扩像素数（夹在 2..=3）。
pub fn pad_for(line_h: i32) -> i32 {
    ((line_h as f32 * PAD_FACTOR).round() as i32).clamp(MIN_PAD, MAX_PAD)
}

/// 一行想外扩多少：左右各 `pad`；比段内统一行高 `band_h` 矮的行，往下补足（矮的行多半是缺了下伸字母，
/// 所以行框顶端才是它真正的行顶），这样整格字形都能放下；上下再各加 `pad`。
///
/// # 参数
/// - `rect`：该行行框。
/// - `band_h`：段内统一行高（见 [`line_height_estimate`]）。
/// - `pad`：外扩距离。
pub fn want_ext(rect: PhysicalRect, band_h: i32, pad: i32) -> Ext {
    let lack = (band_h - rect.height).max(0);
    Ext {
        left: pad,
        right: pad,
        top: pad,
        bottom: pad + lack,
    }
}

/// 在两个“水平范围（含各自想外扩的量）”重叠时，给上下方向的外扩设限：两框之间的空隙对半分。
fn limit_vertical(ri: &PhysicalRect, rj: &PhysicalRect, e: &mut Ext) {
    if rj.bottom() <= ri.y {
        e.top = e.top.min((ri.y - rj.bottom()) / 2);
    } else if rj.y >= ri.bottom() {
        e.bottom = e.bottom.min((rj.y - ri.bottom()) / 2);
    }
}

/// 左右方向的对应设限。
fn limit_horizontal(ri: &PhysicalRect, rj: &PhysicalRect, e: &mut Ext) {
    if rj.right() <= ri.x {
        e.left = e.left.min((ri.x - rj.right()) / 2);
    } else if rj.x >= ri.right() {
        e.right = e.right.min((rj.x - ri.right()) / 2);
    }
}

/// 两个行框本身相交时的处理：沿相交更“深”的那根轴，把相交带从中线一分为二，各自的填充区只留自己这半边
/// （两半合起来仍盖住两个框的全部范围，所以不会漏掉原文，也不会互相覆盖）；一个框完全套在另一个里面时，
/// 无从切分，只是朝对方的那一侧不再外扩。
fn cut_overlap(ri: &PhysicalRect, rj: &PhysicalRect, e: &mut Ext) {
    let (ci, cj) = (ri.center(), rj.center());
    let (dx, dy) = (cj.x - ci.x, cj.y - ci.y);
    if dy.abs() * (ri.width + rj.width) >= dx.abs() * (ri.height + rj.height) {
        if dy < 0 {
            // j 在上：相交带是 [ri.y, rj.bottom)
            e.top = if rj.y < ri.y {
                ri.y - (ri.y + rj.bottom()) / 2
            } else {
                0
            };
        } else {
            e.bottom = if rj.bottom() > ri.bottom() {
                (rj.y + ri.bottom()) / 2 - ri.bottom()
            } else {
                0
            };
        }
    } else if dx < 0 {
        e.left = if rj.x < ri.x {
            ri.x - (ri.x + rj.right()) / 2
        } else {
            0
        };
    } else {
        e.right = if rj.right() > ri.right() {
            (rj.x + ri.right()) / 2 - ri.right()
        } else {
            0
        };
    }
}

/// 两个闭开区间 `[a0, a1)` 与 `[b0, b1)` 是否相交。
fn ranges_touch(a: (i32, i32), b: (i32, i32)) -> bool {
    a.0 < b.1 && b.0 < a.1
}

/// 把每个行框按想外扩的量扩成填充区，并保证不压到任何别的行框、相邻填充区之间也不重叠：
/// 与某框相邻的方向上，空隙对半分（双方各让一半）；两框本身相交时，把相交带从中线切开各占一半
/// （见 [`cut_overlap`]，嵌套的框除外）。
///
/// # 参数
/// - `rects`：全部 OCR 行框（含不翻译的，它们也是障碍）。
/// - `want`：与 `rects` 一一对应，每个框想外扩的量（不画的框填 0）。
/// - `image`：图像范围，结果会裁到其内。
///
/// # 返回
/// 与 `rects` 一一对应的填充区；裁完为空的返回原框的裁剪结果或 `None`。
///
/// ```ignore
/// let fills = plan_fills(&rects, &want, PhysicalRect::new(0, 0, w, h));
/// ```
pub fn plan_fills(
    rects: &[PhysicalRect],
    want: &[Ext],
    image: PhysicalRect,
) -> Vec<Option<PhysicalRect>> {
    (0..rects.len())
        .map(|i| {
            let (ri, wi) = (rects[i], want[i]);
            if ri.is_empty() {
                return None;
            }
            let mut e = wi;
            for (j, rj) in rects.iter().enumerate() {
                if j == i || rj.is_empty() {
                    continue;
                }
                let wj = want[j];
                if ri.intersect(rj).is_some() {
                    cut_overlap(&ri, rj, &mut e);
                    continue;
                }
                let xi = (ri.x - wi.left, ri.right() + wi.right);
                let xj = (rj.x - wj.left, rj.right() + wj.right);
                let yi = (ri.y - wi.top, ri.bottom() + wi.bottom);
                let yj = (rj.y - wj.top, rj.bottom() + wj.bottom);
                if ranges_touch(xi, xj) {
                    limit_vertical(&ri, rj, &mut e);
                }
                if ranges_touch(yi, yj) {
                    limit_horizontal(&ri, rj, &mut e);
                }
            }
            PhysicalRect::new(
                ri.x - e.left,
                ri.y - e.top,
                ri.width + e.left + e.right,
                ri.height + e.top + e.bottom,
            )
            .intersect(&image)
        })
        .collect()
}

/// 由原文各行框的边界推断对齐方式。
///
/// 多行：左边界一致 → 左对齐；否则中线一致 → 居中；否则右边界一致 → 右对齐；其余按左对齐。
/// 单行无从比较：中线贴近图像中线视为居中，否则左对齐。
///
/// # 参数
/// - `rects`：该段各行框。
/// - `image_width`：图像宽度（像素）。
/// - `line_h`：段内行高（像素），用来定容差。
///
/// # 返回
/// 对齐方式。
///
/// ```ignore
/// assert_eq!(infer_align(&[a, b], 800, 20), Align::Left);
/// ```
pub fn infer_align(rects: &[PhysicalRect], image_width: i32, line_h: i32) -> Align {
    let spread = |f: &dyn Fn(&PhysicalRect) -> i32| -> i32 {
        let v: Vec<i32> = rects.iter().map(f).collect();
        v.iter().max().copied().unwrap_or(0) - v.iter().min().copied().unwrap_or(0)
    };
    if rects.len() < 2 {
        let Some(r) = rects.first() else {
            return Align::Left;
        };
        let snap = ((image_width as f32 * CENTER_SNAP_RATIO) as i32).max(MIN_CENTER_SNAP);
        let center = r.x + r.width / 2;
        return if (center - image_width / 2).abs() <= snap {
            Align::Center
        } else {
            Align::Left
        };
    }
    let tol = ((line_h as f32 * ALIGN_TOL_FACTOR) as i32).max(MIN_ALIGN_TOL);
    if spread(&|r| r.x) <= tol {
        Align::Left
    } else if spread(&|r| r.x + r.width / 2) <= tol {
        Align::Center
    } else if spread(&|r| r.right()) <= tol {
        Align::Right
    } else {
        Align::Left
    }
}

/// 拉丁文大写 / 上伸字母（b d f h k l t）顶端到基线的高度（相对字号）。
const LATIN_ASCENT_EM: f32 = 0.74;
/// 拉丁文小写 x 高度（相对字号）。
const LATIN_X_HEIGHT_EM: f32 = 0.5;
/// 拉丁文下伸字母（g j p q y）基线以下的深度（相对字号）。
const LATIN_DESCENT_EM: f32 = 0.22;
/// 中日韩字形墨迹顶端到基线的高度（相对字号）。
const CJK_ABOVE_EM: f32 = 0.86;
/// 中日韩字形墨迹在基线以下的深度（相对字号）。
const CJK_BELOW_EM: f32 = 0.10;
/// “满行”（既有上伸又有下伸字母）的墨迹高度（相对字号）。
pub const FULL_SPAN_EM: f32 = LATIN_ASCENT_EM + LATIN_DESCENT_EM;
/// 会冒出 x 高度之上的非字母字符（数字、括号、常见符号）。
const TALL_SYMBOLS: &str = "()[]{}/|?!&%$@#";
/// 会伸到基线之下的字母与符号。
const DESCENDERS: &str = "gjpqy()[]{}|/@$";
/// 只略微伸到基线之下的逗号、分号。
const SMALL_DESCENDERS: &str = ",;";
/// 逗号、分号在基线以下的深度（相对字号）。
const SMALL_DESCENT_EM: f32 = 0.10;

/// 一行文字的墨迹纵向范围（相对字号 em，基线上 / 下各多少）。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct GlyphSpan {
    /// 墨迹顶端到基线。
    pub above: f32,
    /// 墨迹底端到基线以下。
    pub below: f32,
}

impl GlyphSpan {
    /// 墨迹总高度（em）。
    pub fn height(&self) -> f32 {
        self.above + self.below
    }
}

/// 由一行文字的内容推断它的墨迹纵向范围：有大写 / 上伸字母或中日韩字，顶端到上伸高度，只有小写 x 高度字母就低一截；
/// 有下伸字母或中日韩字，底端到下伸深度，否则就在基线上。用来把“某行实测的墨迹高度”换算回字号。
///
/// # 参数
/// - `text`：这一行的原文。
///
/// # 返回
/// 纵向范围；文本里没有任何字母时按“满行”计。
///
/// ```ignore
/// assert!(glyph_span("Directly").height() > glyph_span("more").height());
/// ```
pub fn glyph_span(text: &str) -> GlyphSpan {
    let (mut tall, mut low, mut desc, mut comma, mut cjk) = (false, false, false, false, false);
    let cjk_text = mostly_cjk(text);
    for c in text.chars() {
        if is_cjk_like(c) {
            // 中日韩标点、以及夹在拉丁文里被 OCR 误认出来的零星汉字，不改变拉丁行的判断
            cjk |= cjk_text && c.is_alphanumeric();
        } else if c.is_uppercase()
            || c.is_ascii_digit()
            || "bdfhklt".contains(c)
            || TALL_SYMBOLS.contains(c)
        {
            tall = true;
        } else if c.is_alphabetic() {
            low = true;
        }
        if DESCENDERS.contains(c) {
            desc = true;
        } else if SMALL_DESCENDERS.contains(c) {
            comma = true;
        }
    }
    if !(tall || low || cjk) {
        return GlyphSpan {
            above: LATIN_ASCENT_EM,
            below: LATIN_DESCENT_EM,
        };
    }
    let above = if cjk {
        CJK_ABOVE_EM
    } else if tall {
        LATIN_ASCENT_EM
    } else {
        LATIN_X_HEIGHT_EM
    };
    let below = match (desc, cjk, comma) {
        (true, _, _) => LATIN_DESCENT_EM,
        (false, true, _) => CJK_BELOW_EM,
        (false, false, true) => SMALL_DESCENT_EM,
        _ => 0.0,
    };
    GlyphSpan { above, below }
}

/// 文本里中日韩字是否占字母类字符的多数（用来挑选量字形高度的探针）。
///
/// # 参数
/// - `text`：文本。
pub fn mostly_cjk(text: &str) -> bool {
    let (mut cjk, mut letters) = (0usize, 0usize);
    for c in text.chars().filter(|c| c.is_alphanumeric()) {
        letters += 1;
        if is_cjk_like(c) {
            cjk += 1;
        }
    }
    letters > 0 && cjk * 2 >= letters
}

/// 文本开头的项目符号（允许前导空白）；不是项目符号返回 `None`。
///
/// # 参数
/// - `text`：一行文字。
///
/// ```ignore
/// assert_eq!(leading_bullet("• 监测"), Some('•'));
/// assert_eq!(leading_bullet("-3 度"), None);
/// ```
pub fn leading_bullet(text: &str) -> Option<char> {
    let mut chars = text.trim_start().chars();
    let first = chars.next()?;
    if BULLET_MARKS.contains(first) {
        return Some(first);
    }
    (SPACED_BULLET_MARKS.contains(first) && chars.next().is_some_and(char::is_whitespace))
        .then_some(first)
}

/// 译文里把“• 甲 • 乙”这样的并排条目拆开：项目符号出现在开头或前面是空白时才算分隔，
/// 去掉分隔符并修剪空白；没有符号时整段原样返回（空文本返回空表）。
///
/// # 参数
/// - `text`：整段译文。
///
/// # 返回
/// 各条目文字（不含符号）。
///
/// ```ignore
/// assert_eq!(split_items("• 甲 • 乙"), ["甲", "乙"]);
/// ```
pub fn split_items(text: &str) -> Vec<String> {
    let chars: Vec<char> = text.chars().collect();
    let mut items: Vec<String> = Vec::new();
    let mut cur = String::new();
    for (i, c) in chars.iter().copied().enumerate() {
        let boundary = i == 0 || chars[i - 1].is_whitespace();
        let spaced = chars.get(i + 1).is_some_and(|n| n.is_whitespace());
        let is_mark =
            boundary && (BULLET_MARKS.contains(c) || (SPACED_BULLET_MARKS.contains(c) && spaced));
        if is_mark {
            items.push(std::mem::take(&mut cur));
        } else {
            cur.push(c);
        }
    }
    items.push(cur);
    items
        .into_iter()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect()
}

/// 把槽按项目符号分组：第 0 个槽和每个带项目符号的槽各开一组，直到下一个这样的槽之前。
///
/// # 参数
/// - `slots`：段内各槽。
///
/// # 返回
/// 各组的槽下标范围。
pub fn group_slots(slots: &[SlotGeom]) -> Vec<Range<usize>> {
    let mut starts: Vec<usize> = (0..slots.len())
        .filter(|&k| k == 0 || slots[k].bullet.is_some())
        .collect();
    starts.push(slots.len());
    starts.windows(2).map(|w| w[0]..w[1]).collect()
}

/// 一组（一个条目）要排的内容。
struct GroupJob {
    /// 它占用的槽下标范围。
    range: Range<usize>,
    /// 行首项目符号。
    bullet: Option<char>,
    /// 条目文字。
    text: String,
}

/// 把一组文字流进它的槽；放不下时：`truncate` 为假返回 `None`，为真则截断并以省略号收尾。
fn flow_group(
    job: &GroupJob,
    slots: &[SlotGeom],
    px: u32,
    advance: &dyn Fn(char, u32) -> f32,
    truncate: bool,
) -> Option<(Vec<SlotLine>, bool)> {
    let em = px as f32;
    let adv = |c: char| advance(c, px) / em;
    let indent = job
        .bullet
        .map_or(0.0, |b| advance(b, px) + advance(SPACE, px));
    let count = job.range.len();
    let width_at = |n: usize| {
        let slot = &slots[job.range.start + n.min(count - 1)];
        (slot.avail - if n == 0 { indent } else { 0.0 }).max(1.0)
    };
    let mut lines = wrap_with(&job.text, &width_at, em, &adv);
    let mut truncated = false;
    if lines.len() > count {
        if !truncate {
            return None;
        }
        lines.truncate(count);
        truncated = true;
        if let Some(last) = lines.last_mut() {
            let limit = width_at(count - 1);
            while !last.is_empty() && text_width(&format!("{last}{ELLIPSIS}"), em, &adv) > limit {
                last.pop();
            }
            last.push(ELLIPSIS);
        }
    }
    if lines.iter().all(String::is_empty) {
        lines.clear();
    }
    if lines.is_empty() && job.bullet.is_some() {
        lines.push(String::new());
    }
    let out = lines
        .into_iter()
        .enumerate()
        .map(|(n, text)| SlotLine {
            slot: job.range.start + n,
            text,
            bullet: if n == 0 { job.bullet } else { None },
            indent: if n == 0 && job.bullet.is_some() {
                indent
            } else {
                0.0
            },
        })
        .collect();
    Some((out, truncated))
}

/// 把一段译文排进它的各个槽：整段统一字号，从 `start_px` 逐像素缩小直到放下（并且字形墨迹高度不超过槽高），
/// 到 `min_px` 仍放不下就截断并加省略号。
///
/// 译文里的“• 甲 • 乙”按条目与带项目符号的槽一一对应；对不上就当一整段顺着全部槽排。
///
/// # 参数
/// - `text`：译文。
/// - `slots`：段内各槽（与原文行框一一对应）。
/// - `start_px` / `min_px`：起始字号与下限（整像素）。
/// - `metrics`：给定字号返回字体度量。
/// - `advance`：给定字符与字号返回前进宽度（像素）。
///
/// # 返回
/// 排版结果；`slots` 为空时返回空结果。
///
/// ```ignore
/// let fit = fit_paragraph("你好世界", &slots, 20, 10, &mut |px| metrics(px), &|c, px| width(c, px));
/// assert!(fit.font_px <= 20);
/// ```
pub fn fit_paragraph(
    text: &str,
    slots: &[SlotGeom],
    start_px: u32,
    min_px: u32,
    metrics: &mut dyn FnMut(u32) -> FontMetrics,
    advance: &dyn Fn(char, u32) -> f32,
) -> Typeset {
    if slots.is_empty() {
        return Typeset {
            font_px: start_px,
            lines: Vec::new(),
            truncated: false,
        };
    }
    let groups = group_slots(slots);
    let items = split_items(text);
    let jobs: Vec<GroupJob> = if items.len() == groups.len() {
        groups
            .into_iter()
            .zip(items)
            .map(|(range, text)| GroupJob {
                bullet: slots[range.start].bullet,
                range,
                text,
            })
            .collect()
    } else {
        vec![GroupJob {
            range: 0..slots.len(),
            bullet: slots[0].bullet,
            text: items.join(&SPACE.to_string()),
        }]
    };
    let heights: Vec<i32> = slots.iter().map(|s| s.height as i32).collect();
    let typical_h = median_i32(&heights) as f32;
    let min_px = min_px.max(1);
    let start_px = start_px.max(min_px);
    for px in (min_px..=start_px).rev() {
        if px > min_px && metrics(px).ink_height() > typical_h + INK_SLACK {
            continue;
        }
        if let Some(lines) = flow_jobs(&jobs, slots, px, advance, false) {
            return Typeset {
                font_px: px,
                lines: lines.0,
                truncated: lines.1,
            };
        }
    }
    let (lines, truncated) =
        flow_jobs(&jobs, slots, min_px, advance, true).unwrap_or((Vec::new(), false));
    Typeset {
        font_px: min_px,
        lines,
        truncated,
    }
}

/// 把全部条目流进各自的槽；任何一组放不下（且不允许截断）就返回 `None`。
fn flow_jobs(
    jobs: &[GroupJob],
    slots: &[SlotGeom],
    px: u32,
    advance: &dyn Fn(char, u32) -> f32,
    truncate: bool,
) -> Option<(Vec<SlotLine>, bool)> {
    let mut all = Vec::new();
    let mut truncated = false;
    for job in jobs {
        let (lines, cut) = flow_group(job, slots, px, advance, truncate)?;
        all.extend(lines);
        truncated |= cut;
    }
    Some((all, truncated))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 假字宽（以 1 像素字号为单位）：中日韩 1.0，其余 0.5。
    fn adv(c: char) -> f32 {
        if is_cjk_like(c) { 1.0 } else { 0.5 }
    }

    /// 假字宽（像素）：同上按字号放大。
    fn advance(c: char, px: u32) -> f32 {
        adv(c) * px as f32
    }

    /// 假字体度量：墨迹高 = 字号（顶 0.8、底 0.2）。
    fn metrics(px: u32) -> FontMetrics {
        FontMetrics {
            ascent: px as f32 * 0.9,
            ink_top: px as f32 * 0.8,
            ink_bottom: px as f32 * 0.2,
        }
    }

    /// 造一个槽。
    fn slot(avail: f32, height: f32) -> SlotGeom {
        SlotGeom {
            avail,
            height,
            bullet: None,
        }
    }

    /// 造一个矩形。
    fn r(x: i32, y: i32, w: i32, h: i32) -> PhysicalRect {
        PhysicalRect::new(x, y, w, h)
    }

    /// 中日韩逐字折行，行首禁则标点并入上一字。
    #[test]
    fn wrap_cjk_and_kinsoku() {
        assert_eq!(wrap_text("你好世界", 40.0, 20.0, &adv), ["你好", "世界"]);
        assert_eq!(
            wrap_text("你好世界。", 60.0, 20.0, &adv),
            ["你好世", "界。"]
        );
        assert_eq!(wrap_text("甲\n乙", 100.0, 20.0, &adv), ["甲", "乙"]);
    }

    /// 拉丁文按词折行，过长的词硬切。
    #[test]
    fn wrap_latin_words_and_hard_break() {
        assert_eq!(
            wrap_text("hello world", 80.0, 20.0, &adv),
            ["hello", "world"]
        );
        assert_eq!(wrap_text("a b c", 100.0, 20.0, &adv), ["a b c"]);
        assert_eq!(
            wrap_text("abcdefghij", 50.0, 20.0, &adv),
            ["abcde", "fghij"]
        );
    }

    /// 每一行宽度不同：第一行窄、后面的宽，字按行序号取各自的宽。
    #[test]
    fn wrap_with_per_line_widths() {
        let lines = wrap_with(
            "一二三四五六七八",
            &|n| if n == 0 { 40.0 } else { 80.0 },
            20.0,
            &adv,
        );
        assert_eq!(lines, ["一二", "三四五六", "七八"]);
    }

    /// 流排：第一行放满再进下一行，放得下时保持起始字号。
    #[test]
    fn fit_flows_line_by_line_and_keeps_start_size() {
        let slots = [slot(60.0, 24.0), slot(60.0, 24.0), slot(60.0, 24.0)];
        let fit = fit_paragraph("一二三四五六七", &slots, 20, 8, &mut metrics, &advance);
        assert_eq!(fit.font_px, 20);
        let texts: Vec<&str> = fit.lines.iter().map(|l| l.text.as_str()).collect();
        assert_eq!(texts, ["一二三", "四五六", "七"]);
        assert_eq!(
            fit.lines.iter().map(|l| l.slot).collect::<Vec<_>>(),
            [0, 1, 2]
        );
        assert!(!fit.truncated);
    }

    /// 行框不够：整段统一缩小字号（最小幅度）直到放下，不截断。
    #[test]
    fn fit_shrinks_minimally_to_fit_slots() {
        let slots = [slot(60.0, 40.0), slot(60.0, 40.0)];
        // 8 字 × 20 = 160 > 120：需缩到 15（每行 4 字 × 15 = 60）
        let fit = fit_paragraph("一二三四五六七八", &slots, 20, 8, &mut metrics, &advance);
        assert_eq!(fit.font_px, 15);
        assert_eq!(fit.lines.len(), 2);
        assert!(!fit.truncated);
    }

    /// 字形墨迹高度超过槽高时也要缩小，保证不裁切。
    #[test]
    fn fit_keeps_ink_inside_slot_height() {
        let slots = [slot(500.0, 14.0)];
        let fit = fit_paragraph("你好", &slots, 30, 6, &mut metrics, &advance);
        assert!(metrics(fit.font_px).ink_height() <= 14.0 + INK_SLACK);
        assert_eq!(fit.font_px, 15);
    }

    /// 缩到下限仍放不下：截断并以省略号收尾，且不超宽。
    #[test]
    fn fit_truncates_at_min_size() {
        let slots = [slot(30.0, 20.0)];
        let fit = fit_paragraph(
            &"很长的一段文字".repeat(10),
            &slots,
            20,
            10,
            &mut metrics,
            &advance,
        );
        assert_eq!(fit.font_px, 10);
        assert!(fit.truncated);
        assert_eq!(fit.lines.len(), 1);
        assert!(fit.lines[0].text.ends_with(ELLIPSIS));
        assert!(text_width(&fit.lines[0].text, 10.0, &adv) <= 30.0);
    }

    /// 项目符号：条目与带符号的槽一一对应，符号占行首宽度，续行没有符号。
    #[test]
    fn fit_maps_bullet_items_to_bullet_slots() {
        let b = |avail| SlotGeom {
            avail,
            height: 30.0,
            bullet: Some('•'),
        };
        let slots = [b(200.0), b(200.0), b(60.0), slot(60.0, 30.0)];
        let fit = fit_paragraph(
            "• 监测风险 • 升级 • 防止事件再次发生",
            &slots,
            20,
            8,
            &mut metrics,
            &advance,
        );
        let line = |slot: usize| fit.lines.iter().find(|l| l.slot == slot).unwrap();
        assert_eq!(line(0).text, "监测风险");
        assert_eq!(line(0).bullet, Some('•'));
        assert!(line(0).indent > 0.0);
        assert_eq!(line(1).text, "升级");
        assert_eq!(line(1).bullet, Some('•'));
        // 第三条放进第 2、3 槽，续行没有符号
        assert_eq!(line(3).bullet, None);
        assert_eq!(line(3).indent, 0.0);
        assert!(fit.lines.iter().map(|l| l.slot).collect::<Vec<_>>() == [0, 1, 2, 3]);
    }

    /// 条目数与带符号的槽数对不上：当整段顺着全部槽排，符号不会乱插。
    #[test]
    fn fit_falls_back_when_items_do_not_match() {
        let slots = [slot(100.0, 30.0), slot(100.0, 30.0)];
        let fit = fit_paragraph("• 甲 • 乙 • 丙", &slots, 20, 8, &mut metrics, &advance);
        let joined: String = fit.lines.iter().map(|l| l.text.as_str()).collect();
        assert_eq!(joined.replace(' ', ""), "甲乙丙");
        assert!(fit.lines.iter().all(|l| l.bullet.is_none()));
    }

    /// 由文字内容推断墨迹纵向范围：有上伸 + 下伸的满行最高，只有 x 高度字母的行最矮，中日韩按整字。
    #[test]
    fn glyph_span_by_content() {
        let full = glyph_span("Directly manage");
        let asc_only = glyph_span("Internal controls");
        let x_only = glyph_span("more users");
        assert!((full.height() - FULL_SPAN_EM).abs() < 1e-6);
        assert_eq!(asc_only.below, 0.0);
        // 只有逗号：略微伸下去，但远不到 g / p 的深度
        assert!(glyph_span("Monitor, control").below > 0.0);
        assert!(glyph_span("Monitor, control").below < glyph_span("Monitoring").below);
        assert!(asc_only.height() < full.height());
        assert!(x_only.height() < asc_only.height());
        assert!(glyph_span("监测风险").height() > 0.9);
        assert_eq!(glyph_span("•"), glyph_span("Hg"));
        assert!(mostly_cjk("监测 risk 风险"));
        assert!(!mostly_cjk("Hello 世"));
        assert!(!mostly_cjk("..."));
    }

    /// 项目符号识别：典型符号、后跟空白的短横线认；负号、没空格的星号不认。
    #[test]
    fn bullet_detection() {
        assert_eq!(leading_bullet("• 监测"), Some('•'));
        assert_eq!(leading_bullet("  ● x"), Some('●'));
        assert_eq!(leading_bullet("- item"), Some('-'));
        assert_eq!(leading_bullet("-3 degrees"), None);
        assert_eq!(leading_bullet("*note"), None);
        assert_eq!(leading_bullet("plain"), None);
        assert_eq!(split_items("• 甲 • 乙"), ["甲", "乙"]);
        assert_eq!(split_items("乔治·华盛顿"), ["乔治·华盛顿"]);
        assert_eq!(split_items("前言 • 条目"), ["前言", "条目"]);
        assert!(split_items("   ").is_empty());
    }

    /// 槽分组：第 0 槽与每个带符号的槽各开一组。
    #[test]
    fn slot_groups_follow_bullets() {
        let mut slots = vec![slot(1.0, 1.0); 5];
        slots[2].bullet = Some('•');
        slots[3].bullet = Some('•');
        assert_eq!(group_slots(&slots), vec![0..2, 2..3, 3..5]);
        assert!(group_slots(&[]).is_empty());
    }

    /// 对齐推断：左边界一致 → 左；中线一致 → 中；右边界一致 → 右；单行贴近图像中线 → 中。
    #[test]
    fn align_inference() {
        let left = [r(10, 0, 100, 20), r(10, 24, 60, 20), r(11, 48, 80, 20)];
        assert_eq!(infer_align(&left, 800, 20), Align::Left);
        let center = [r(100, 0, 200, 20), r(130, 24, 140, 20), r(150, 48, 100, 20)];
        assert_eq!(infer_align(&center, 800, 20), Align::Center);
        let right = [r(100, 0, 200, 20), r(160, 24, 140, 20), r(200, 48, 100, 20)];
        assert_eq!(infer_align(&right, 800, 20), Align::Right);
        // 缩进的续行（项目符号）：三种都不齐，按左对齐
        let bullet = [r(10, 0, 200, 20), r(40, 24, 100, 20), r(10, 48, 150, 20)];
        assert_eq!(infer_align(&bullet, 800, 20), Align::Left);
        assert_eq!(infer_align(&[r(340, 0, 120, 30)], 800, 30), Align::Center);
        assert_eq!(infer_align(&[r(20, 0, 120, 30)], 800, 30), Align::Left);
        assert_eq!(infer_align(&[], 800, 30), Align::Left);
    }

    /// 填充区只向外扩一点，相邻行框之间空隙对半分，互不重叠也不压到别的框。
    #[test]
    fn fills_split_gaps_and_never_overlap() {
        let image = r(0, 0, 200, 100);
        // 上下相隔 4 像素的两行、右边紧贴一个不翻译的框（空隙 3）
        let rects = [r(10, 10, 80, 12), r(10, 26, 80, 12), r(93, 10, 40, 12)];
        let want = [
            want_ext(rects[0], 12, 3),
            want_ext(rects[1], 12, 3),
            Ext::default(),
        ];
        let fills = plan_fills(&rects, &want, image);
        let (a, b) = (fills[0].unwrap(), fills[1].unwrap());
        assert_eq!(a.bottom(), 24, "空隙 4 对半分，各让 2");
        assert_eq!(b.y, 24);
        assert!(a.intersect(&b).is_none());
        // 右侧 3 像素空隙：让出 1，不压到不翻译的框
        assert_eq!(a.right(), 91);
        assert!(a.intersect(&rects[2]).is_none() && b.intersect(&rects[2]).is_none());
        // 左 / 上方向没有邻居，按想要的量扩，再裁到图像内
        assert_eq!((a.x, a.y), (7, 7));
    }

    /// 本身相交的两个框：相交带从中线切开各占一半，两块合起来恰好盖住两个框、互不重叠；框总被裁在图像内；
    /// 嵌套的框无从切分，只是不向对方那侧外扩。
    #[test]
    fn fills_handle_overlap_and_image_edges() {
        let image = r(0, 0, 50, 50);
        let rects = [r(0, 0, 30, 20), r(0, 15, 30, 20)];
        let want = [want_ext(rects[0], 20, 4), want_ext(rects[1], 20, 4)];
        let fills = plan_fills(&rects, &want, image);
        let (a, b) = (fills[0].unwrap(), fills[1].unwrap());
        assert_eq!((a.bottom(), b.y), (17, 17), "相交带 [15,20) 在中线 17 切开");
        assert!(a.intersect(&b).is_none());
        assert!(a.y == 0 && b.bottom() == 39 && a.x >= 0);
        // 左右并排相交：同样沿 x 切
        let rects = [r(0, 0, 30, 20), r(25, 0, 30, 20)];
        let want = [want_ext(rects[0], 20, 2), want_ext(rects[1], 20, 2)];
        let fills = plan_fills(&rects, &want, image);
        assert_eq!((fills[0].unwrap().right(), fills[1].unwrap().x), (27, 27));
        // 嵌套：小框完全在大框里，大框不被切，只是不向它那侧扩
        let rects = [r(5, 5, 40, 30), r(15, 12, 10, 8)];
        let want = [want_ext(rects[0], 30, 3), Ext::default()];
        let fills = plan_fills(&rects, &want, image);
        assert!(fills[0].unwrap().intersect(&rects[1]).is_some());
        assert!(plan_fills(&[r(0, 0, 0, 5)], &[Ext::default()], image)[0].is_none());
    }

    /// 较矮的行往下补到段内统一行高，再加外扩；“满行”高度取 75% 分位（限制在中位数的 1.2 倍内）。
    #[test]
    fn want_ext_levels_short_lines() {
        let e = want_ext(r(0, 10, 50, 8), 14, 2);
        assert_eq!((e.top, e.bottom, e.left, e.right), (2, 8, 2, 2));
        assert_eq!(line_height_estimate(&[12, 12, 15]), 14);
        assert_eq!(line_height_estimate(&[12, 13, 15]), 15);
        assert_eq!(line_height_estimate(&[15, 15, 15, 30]), 15);
        assert_eq!(line_height_estimate(&[27, 21, 17]), 25);
        assert_eq!(line_height_estimate(&[12]), 12);
        assert_eq!(line_height_estimate(&[]), 0);
        let e = want_ext(r(0, 10, 50, 20), 14, 2);
        assert_eq!((e.top, e.bottom), (2, 2));
        assert_eq!(pad_for(5), 2);
        assert_eq!(pad_for(20), 2);
        assert_eq!(pad_for(100), 3);
    }
}
