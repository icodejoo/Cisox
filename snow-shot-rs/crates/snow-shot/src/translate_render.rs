//! 截图翻译的“原位替换”渲染：以 OCR 行框为锚点，把译文按原文的行序列流排回原位。
//!
//! 流程（全部是不依赖 GPUI 的纯函数，字宽与光栅化由调用方注入，所以都能离屏确定性测试）：
//! 1. 规划：每段先把行框外扩成互不重叠的填充区（[`crate::translate_typeset::plan_fills`]），再采样填充区外圈得到背景模型
//!    （纯色或线性渐变）、估计前景色与字重、推断对齐、从原文行高推出字号并流排。规划只读原图，所有取色都在改写之前完成。
//! 2. 上色：先把全部段落的填充区统一涂上背景，再统一画字，后涂的背景永远盖不到先画的文字。
//!
//! 真实运行时用 [`render_with_system_font`] 接系统字体（GDI，中日韩与拉丁文都能画）。

use crate::ocr_service::OcrTextBox;
use crate::translate_paint::{
    BgModel, Ink, blend_bitmap, erase_text, estimate_ink, measure_ink, sample_background,
};
use crate::translate_service::TranslatedParagraph;
use crate::translate_typeset::{
    Align, Ext, FULL_SPAN_EM, FontMetrics, SlotGeom, SlotLine, Typeset, fit_paragraph, glyph_span,
    infer_align, leading_bullet, line_height_estimate, mostly_cjk, pad_for, plan_fills, want_ext,
};
use snow_platform::text_raster::{self, DEFAULT_FONT_FAMILY, TextBitmap};
use snow_ui::shell::geometry::PhysicalRect;
use std::cell::RefCell;
use std::collections::{HashMap, HashSet};

/// 量字形度量用的参考字号（像素）。
const REF_FONT_PX: u32 = 64;
/// 实测墨迹失败时，本地模型（PP-OCR）行框高度与“满行墨迹高度”的比值：DB 检测会外扩，
/// 用 `snow-shot` 的 `ocr_box_probe` 在拉丁 / 中日韩样片上实测，行框约为墨迹的 1.2–1.9 倍，取 1.45。
const LOCAL_BOX_PER_INK: f32 = 1.45;
/// 实测墨迹失败时，系统 OCR 行框高度与“满行墨迹高度”的比值：拉丁文贴着墨迹（1.0），
/// 中日韩偏松（实测约 1.2），取 1.15。
const SYSTEM_BOX_PER_INK: f32 = 1.15;
/// 裁剪区向上放宽到基线之上“满行墨迹高度”的这个倍数（容纳译文的上伸部分）。
const CLIP_ABOVE_FULL: f32 = 0.8;
/// 裁剪区向下放宽到基线之下“满行墨迹高度”的这个倍数（容纳译文的下伸部分）。
const CLIP_BELOW_FULL: f32 = 0.3;
/// 整块抹除时，实测墨迹向外多抹的像素数（吃掉抗锯齿边缘）。
const ERASE_PAD: i32 = 2;
/// 量墨迹时，行框向外放宽的比例（相对行高；再受邻行限制）。
const INK_SEARCH_GROW_RATIO: f32 = 0.35;
/// 量墨迹时行框向外放宽的像素数下限。
const MIN_INK_SEARCH_GROW: i32 = 3;
/// 实测墨迹高度低于它（像素）视为噪点，改用行框高度估计。
const MIN_MEASURED_INK_H: i32 = 4;
/// 字号下限占起始字号的比例；缩到它仍放不下才截断。
const MIN_SCALE: f32 = 0.55;
/// 自适应字号的绝对下限（像素）。
const MIN_FIT_PX: u32 = 7;
/// 自适应字号的绝对上限（像素），防止异常大框撑出巨字。
const MAX_FIT_PX: u32 = 160;
/// 采样背景时，别的行框向外多排除的像素数（吃掉它们的抗锯齿边缘）。
const NEIGHBOR_MARGIN: i32 = 1;
/// 找“会挡住背景采样的邻近行框”时，把填充区向外放宽的像素数（要大于采样条带宽度加排除边距）。
const OBSTACLE_RADIUS: i32 = 8;
/// 判定字形位图“这一行有墨迹”的最小覆盖率。
const METRIC_INK: u8 = 64;
/// 量中日韩译文字形墨迹范围的探针（一个满格的汉字）。
const CJK_METRIC_PROBE: &str = "国";
/// 量拉丁译文字形墨迹范围的探针：大写、上伸、下伸字母都有。
const LATIN_METRIC_PROBE: &str = "Hgdy";
/// 只用来定基线的探针。
const BASELINE_PROBE: &str = "H";
/// 光栅化失败时，字形度量的兜底比例（相对字号）：基线、墨迹上缘、墨迹下缘。
const FALLBACK_METRICS: (f32, f32, f32) = (0.9, 0.85, 0.2);

/// 字宽函数：给定字符、整像素字号、是否粗体，返回前进宽度（像素）。
pub type AdvanceFn<'a> = dyn Fn(char, u32, bool) -> f32 + 'a;
/// 光栅函数：给定文字、整像素字号、是否粗体，画成覆盖率位图。
pub type RasterFn<'a> = dyn FnMut(&str, u32, bool) -> Option<TextBitmap> + 'a;

/// 位图里有墨迹的首尾行（含）；没有墨迹返回 `None`。
fn ink_rows(bitmap: &TextBitmap) -> Option<(i32, i32)> {
    let w = bitmap.width.max(1) as usize;
    let mut rows = bitmap
        .coverage
        .chunks(w)
        .enumerate()
        .filter(|(_, row)| row.iter().any(|&c| c >= METRIC_INK))
        .map(|(y, _)| y as i32);
    let first = rows.next()?;
    Some((first, rows.next_back().unwrap_or(first)))
}

/// 量某个字号 / 字重下的字形度量：用 “H” 定基线，用探针串（按译文语种选）定墨迹的上下缘。
fn measure_font(raster: &mut RasterFn, px: u32, bold: bool, cjk: bool) -> FontMetrics {
    let fallback = FontMetrics {
        ascent: px as f32 * FALLBACK_METRICS.0,
        ink_top: px as f32 * FALLBACK_METRICS.1,
        ink_bottom: px as f32 * FALLBACK_METRICS.2,
    };
    let Some((_, base_last)) = raster(BASELINE_PROBE, px, bold).and_then(|b| ink_rows(&b)) else {
        return fallback;
    };
    let baseline = (base_last + 1) as f32;
    let Some((first, last)) = raster(
        if cjk {
            CJK_METRIC_PROBE
        } else {
            LATIN_METRIC_PROBE
        },
        px,
        bold,
    )
    .and_then(|b| ink_rows(&b)) else {
        return fallback;
    };
    FontMetrics {
        ascent: baseline,
        ink_top: baseline - first as f32,
        ink_bottom: (last + 1) as f32 - baseline,
    }
}

/// 带缓存的字形度量 + 光栅化入口（同一字号 / 字重只量一次）。
struct FontCache<'r> {
    /// 调用方注入的光栅函数。
    raster: &'r mut RasterFn<'r>,
    /// 已量过的度量，键为（字号，是否粗体，是否中日韩）。
    cached: HashMap<(u32, bool, bool), FontMetrics>,
}

impl<'r> FontCache<'r> {
    /// 取某字号 / 字重 / 语种的字形度量。
    fn metrics(&mut self, px: u32, bold: bool, cjk: bool) -> FontMetrics {
        if let Some(m) = self.cached.get(&(px, bold, cjk)) {
            return *m;
        }
        let m = measure_font(&mut *self.raster, px, bold, cjk);
        self.cached.insert((px, bold, cjk), m);
        m
    }
}

/// 段内一行的落点：原文行框、实测墨迹、盖住它的填充区与背景模型。
#[derive(Debug, Clone, PartialEq)]
pub struct PlanSlot {
    /// 原文行框（图像坐标，已裁到图内）。
    pub box_rect: PhysicalRect,
    /// 原文实测的墨迹范围；量不出来时等于行框。
    pub ink: PhysicalRect,
    /// 原文基线所在的 y（图像坐标）；量不出来时为 `None`，译文改为在行带里居中。
    pub baseline: Option<f32>,
    /// 要盖住的填充区（行框略外扩，且不压到任何别的行框）。
    pub fill: PhysicalRect,
    /// 整块涂成背景的区域：量出了墨迹就是实测墨迹向外各 `ERASE_PAD` 像素（行框切掉墨迹边缘时，露在框外的几行也抹得到；
    /// 行框外扩出来的部分不整块涂，免得盖坏旁边的图标），量不出来就是行框。
    pub erase: PhysicalRect,
    /// 译文落笔的裁剪区：填充区再按原文基线向上下放宽到能放下整行字形（行框偏矮时），同样对邻行让位。
    pub clip: PhysicalRect,
    /// 译文落点的行带：行框横向范围 × 段内统一行高（比统一行高矮的行框往下补足）。
    pub band: PhysicalRect,
    /// 填充区的背景模型。
    pub bg: BgModel,
}

/// 规划过程中的一段草稿：已建好各行的填充区与背景模型，还没量墨迹。
struct Draft {
    /// 在 `pairs` 里的下标。
    n: usize,
    /// 各行行框在 `boxes` 里的下标。
    ixs: Vec<usize>,
    /// 段内“满行”行框高度估计。
    line_h: i32,
    /// 各行落点（`ink` / `baseline` 还是占位值）。
    slots: Vec<PlanSlot>,
    /// 用行框粗估的前景色（量墨迹时用）。
    rough_fg: [u8; 3],
}

/// 一段已规划好的替换。
#[derive(Debug, Clone, PartialEq)]
pub struct ParagraphPlan {
    /// 在 `pairs` 里的下标。
    pub pair_index: usize,
    /// 各行落点（与原文行框一一对应，顺序同 `box_indices`）。
    pub slots: Vec<PlanSlot>,
    /// 对齐方式。
    pub align: Align,
    /// 前景色与字重。
    pub ink: Ink,
    /// 译文是否以中日韩字为主（决定量字形度量用哪个探针）。
    pub cjk: bool,
    /// 译文排版。
    pub fit: Typeset,
}

/// 一段里有效的行框下标：存在、裁到图内非空、且没被前面的段占用；按原顺序，去重。
fn valid_indices(
    pair: &TranslatedParagraph,
    rects: &[PhysicalRect],
    claimed: &mut HashSet<usize>,
) -> Vec<usize> {
    pair.box_indices
        .iter()
        .copied()
        .filter(|ix| rects.get(*ix).is_some_and(|r| !r.is_empty()) && claimed.insert(*ix))
        .collect()
}

/// 规划全部段落的替换（只读原图，不改像素）。
///
/// # 参数
/// - `rgba` / `width` / `height`：原图（RGBA，行优先）。
/// - `boxes`：全部 OCR 行框（含不翻译的，它们也是障碍，不会被盖到）。
/// - `pairs`：逐段对照。
/// - `advance`：字宽函数。
/// - `raster`：光栅函数（用来量字形度量）。
///
/// # 返回
/// 每个有译文且有有效行框的段一份计划。
///
/// ```ignore
/// let plans = plan_paragraphs(&rgba, w, h, &boxes, &pairs, &advance, &mut raster);
/// ```
pub fn plan_paragraphs(
    rgba: &[u8],
    width: u32,
    height: u32,
    boxes: &[OcrTextBox],
    pairs: &[TranslatedParagraph],
    advance: &AdvanceFn,
    raster: &mut RasterFn,
) -> Vec<ParagraphPlan> {
    let image = PhysicalRect::new(0, 0, width as i32, height as i32);
    let rects: Vec<PhysicalRect> = boxes
        .iter()
        .map(|b| b.rect.intersect(&image).unwrap_or_default())
        .collect();
    let mut claimed = HashSet::new();
    let paragraphs: Vec<(usize, Vec<usize>)> = pairs
        .iter()
        .enumerate()
        .filter(|(_, p)| !p.translated.trim().is_empty())
        .map(|(n, p)| (n, valid_indices(p, &rects, &mut claimed)))
        .filter(|(_, ix)| !ix.is_empty())
        .collect();
    // 先把每段的行高、外扩量定下来，再统一求填充区：相邻行框（不论属于哪一段）之间对半让位
    let mut want = vec![Default::default(); rects.len()];
    let mut line_heights: HashMap<usize, i32> = HashMap::new();
    for (n, ixs) in &paragraphs {
        let heights: Vec<i32> = ixs.iter().map(|ix| rects[*ix].height).collect();
        let line_h = line_height_estimate(&heights).max(1);
        line_heights.insert(*n, line_h);
        for ix in ixs {
            want[*ix] = want_ext(rects[*ix], line_h, pad_for(line_h));
        }
    }
    let fills = plan_fills(&rects, &want, image);
    // 量墨迹的搜索范围：行框再放宽一些（同样对邻行让位），因为真实 OCR 的行框常常偏高 / 偏低、会切到墨迹
    let mut grow = vec![Ext::default(); rects.len()];
    for (n, ixs) in &paragraphs {
        let g = ((line_heights[n] as f32 * INK_SEARCH_GROW_RATIO).round() as i32)
            .max(MIN_INK_SEARCH_GROW);
        for ix in ixs {
            grow[*ix] = Ext {
                left: g,
                top: g,
                right: g,
                bottom: g,
            };
        }
    }
    let searches = plan_fills(&rects, &grow, image);
    // 置信度只有本地模型给；用它区分两种后端（只影响“量不出墨迹”时的兜底系数）
    let box_per_ink = if boxes.iter().any(|b| b.confidence.is_some()) {
        LOCAL_BOX_PER_INK
    } else {
        SYSTEM_BOX_PER_INK
    };
    let mut cache = FontCache {
        raster,
        cached: HashMap::new(),
    };
    // 第一步：每段先建好各行的填充区与背景模型，并用行框粗估前景色
    let mut drafts: Vec<Draft> = Vec::with_capacity(paragraphs.len());
    for (n, ixs) in paragraphs {
        let line_h = line_heights[&n];
        let box_rects: Vec<PhysicalRect> = ixs.iter().map(|ix| rects[*ix]).collect();
        let slots: Vec<PlanSlot> = ixs
            .iter()
            .map(|&ix| {
                let fill = fills[ix].unwrap_or(rects[ix]);
                let outer = PhysicalRect::new(
                    fill.x - OBSTACLE_RADIUS,
                    fill.y - OBSTACLE_RADIUS,
                    fill.width + 2 * OBSTACLE_RADIUS,
                    fill.height + 2 * OBSTACLE_RADIUS,
                );
                let others: Vec<PhysicalRect> = fills
                    .iter()
                    .enumerate()
                    .filter(|(j, _)| *j != ix)
                    .filter_map(|(_, f)| *f)
                    .filter(|f| f.intersect(&outer).is_some())
                    .map(|f| {
                        PhysicalRect::new(
                            f.x - NEIGHBOR_MARGIN,
                            f.y - NEIGHBOR_MARGIN,
                            f.width + 2 * NEIGHBOR_MARGIN,
                            f.height + 2 * NEIGHBOR_MARGIN,
                        )
                    })
                    .collect();
                let b = rects[ix];
                PlanSlot {
                    box_rect: b,
                    ink: b,
                    baseline: None,
                    fill,
                    erase: b,
                    clip: fill,
                    band: PhysicalRect::new(b.x, b.y, b.width, b.height.max(line_h)),
                    bg: sample_background(rgba, width, height, fill, &others),
                }
            })
            .collect();
        let models: Vec<BgModel> = slots.iter().map(|s| s.bg.clone()).collect();
        let rough_fg = estimate_ink(rgba, width, height, &box_rects, &models, None).fg;
        drafts.push(Draft {
            n,
            ixs,
            line_h,
            slots,
            rough_fg,
        });
    }
    // 第二步：量每一行的实际墨迹。第一遍在受邻行限制的搜索范围里量；若墨迹顶到了范围边缘（行框切进了墨迹，
    // 而被切掉的部分落在邻行的行框里），第二遍改以各行已量出的墨迹为障碍重新放宽范围再量一次
    let measure = |draft: &Draft, k: usize, search: PhysicalRect| -> Option<PhysicalRect> {
        let slot = &draft.slots[k];
        measure_ink(
            rgba,
            width,
            height,
            search,
            slot.box_rect,
            &slot.bg,
            draft.rough_fg,
        )
        .filter(|r| r.height >= MIN_MEASURED_INK_H)
    };
    let mut inks: Vec<Option<PhysicalRect>> = vec![None; rects.len()];
    for d in &drafts {
        for (k, &ix) in d.ixs.iter().enumerate() {
            inks[ix] = measure(d, k, searches[ix].unwrap_or(rects[ix]));
        }
    }
    let obstacles: Vec<PhysicalRect> = (0..rects.len())
        .map(|i| inks[i].unwrap_or(rects[i]))
        .collect();
    let regrown = plan_fills(&obstacles, &grow, image);
    let first_pass = inks.clone();
    for d in &drafts {
        for (k, &ix) in d.ixs.iter().enumerate() {
            let (Some(ink), Some(wide)) = (first_pass[ix], regrown[ix]) else {
                continue;
            };
            let search = searches[ix].unwrap_or(rects[ix]);
            if ink.y <= search.y || ink.bottom() >= search.bottom() {
                inks[ix] = measure(d, k, search.union_bounds(&wide)).or(inks[ix]);
            }
        }
    }
    // 行框偏矮（比如系统 OCR 切掉了下伸部分）时，填充区装不下整行译文字形：按原文基线把落笔的裁剪区向上下放宽，
    // 仍然对邻行的填充区让位
    let mut clip_ext = vec![Ext::default(); rects.len()];
    for d in &drafts {
        for (slot, &ix) in d.slots.iter().zip(&d.ixs) {
            let Some(ink) = inks[ix] else { continue };
            let span = glyph_span(&boxes[ix].text);
            let full = ink.height as f32 / span.height() * FULL_SPAN_EM;
            let baseline = ink.bottom() as f32 - span.below * (ink.height as f32 / span.height());
            let top = (baseline - CLIP_ABOVE_FULL * full).floor() as i32;
            let bottom = (baseline + CLIP_BELOW_FULL * full).ceil() as i32;
            clip_ext[ix] = Ext {
                left: 0,
                right: 0,
                top: (slot.fill.y - top).max(0),
                bottom: (bottom - slot.fill.bottom()).max(0),
            };
        }
    }
    let fill_rects: Vec<PhysicalRect> = (0..rects.len())
        .map(|i| fills[i].unwrap_or(rects[i]))
        .collect();
    let clips = plan_fills(&fill_rects, &clip_ext, image);
    // 第三步：按实测墨迹定基线、字号与对齐，并流排译文
    let mut plans = Vec::with_capacity(drafts.len());
    for Draft {
        n,
        ixs,
        line_h,
        mut slots,
        ..
    } in drafts
    {
        let models: Vec<BgModel> = slots.iter().map(|s| s.bg.clone()).collect();
        let mut full_heights: Vec<f32> = Vec::with_capacity(slots.len());
        for (slot, &ix) in slots.iter_mut().zip(&ixs) {
            let Some(ink) = inks[ix] else {
                full_heights.push(slot.box_rect.height as f32 / box_per_ink);
                continue;
            };
            // 实测墨迹高度按这一行文字的内容（有无上伸 / 下伸字母）还原成字号，再换算成“满行”墨迹高度
            let span = glyph_span(&boxes[ix].text);
            let em = ink.height as f32 / span.height();
            slot.ink = ink;
            // 量出了墨迹就只把墨迹周围整块抹掉；行框里别的部分（外扩出来的空白、旁边的图标）按“像文字抗锯齿才抹”处理
            slot.erase = PhysicalRect::new(
                ink.x - ERASE_PAD,
                ink.y - ERASE_PAD,
                ink.width + 2 * ERASE_PAD,
                ink.height + 2 * ERASE_PAD,
            );
            slot.clip = clips[ix].unwrap_or(slot.fill);
            slot.baseline = Some(ink.bottom() as f32 - span.below * em);
            full_heights.push(em * FULL_SPAN_EM);
        }
        let ink_boxes: Vec<PhysicalRect> = slots
            .iter()
            .map(|s| PhysicalRect::new(s.ink.x - 1, s.ink.y - 1, s.ink.width + 2, s.ink.height + 2))
            .collect();
        let ink_rects: Vec<PhysicalRect> = slots.iter().map(|s| s.ink).collect();
        let full_h = {
            full_heights.sort_by(f32::total_cmp);
            full_heights[full_heights.len() / 2]
        };
        // 字重按“满行高度”（+ 上下各 1 像素边距，与标定时一致）算比例，不受缺下伸字母的矮行影响
        let ink = estimate_ink(rgba, width, height, &ink_boxes, &models, Some(full_h + 2.0));
        let bullets: Vec<Option<char>> = ixs
            .iter()
            .map(|ix| leading_bullet(&boxes[*ix].text))
            .collect();
        let align = if bullets.iter().any(Option::is_some) {
            Align::Left
        } else {
            infer_align(&ink_rects, width as i32, line_h)
        };
        let geoms: Vec<SlotGeom> = slots
            .iter()
            .zip(&bullets)
            .map(|(s, bullet)| SlotGeom {
                avail: avail_width(s, align),
                height: s.clip.height as f32,
                bullet: *bullet,
            })
            .collect();
        // 起始字号：让译文“满行”字形墨迹高度等于原文“满行”墨迹高度（量出来的，不是行框高度）
        let cjk = mostly_cjk(&pairs[n].translated);
        let ink_ratio =
            (cache.metrics(REF_FONT_PX, ink.bold, cjk).ink_height() / REF_FONT_PX as f32).max(0.1);
        let start_px = ((full_h / ink_ratio).round() as u32).clamp(MIN_FIT_PX, MAX_FIT_PX);
        let min_px = ((start_px as f32 * MIN_SCALE).round() as u32)
            .max(MIN_FIT_PX)
            .min(start_px);
        let bold = ink.bold;
        let fit = fit_paragraph(
            &pairs[n].translated,
            &geoms,
            start_px,
            min_px,
            &mut |px| cache.metrics(px, bold, cjk),
            &|c, px| advance(c, px, bold),
        );
        plans.push(ParagraphPlan {
            pair_index: n,
            slots,
            align,
            ink,
            cjk,
            fit,
        });
    }
    plans
}

/// 一行里文字最多能占多宽（像素）：左对齐从原文墨迹左缘到填充区右缘，右对齐反之，居中取以墨迹中线为轴的对称宽度。
fn avail_width(slot: &PlanSlot, align: Align) -> f32 {
    let (ink, f) = (slot.ink, slot.fill);
    let cx = ink.x + ink.width / 2;
    (match align {
        Align::Left => f.right() - ink.x,
        Align::Right => ink.right() - f.x,
        Align::Center => 2 * (cx - f.x).min(f.right() - cx),
    })
    .max(1) as f32
}

/// 一行文字位图的左上角：垂直方向让译文基线对齐原文实测基线（量不出来时让“字形墨迹块”在行带里居中），
/// 水平方向以原文实测墨迹为准按对齐方式放，并夹在填充区内。
///
/// # 参数
/// - `slot`：这一行的落点。
/// - `align`：对齐方式。
/// - `metrics`：当前字号的字形度量。
/// - `bitmap_w`：文字位图宽度。
/// - `indent`：行首项目符号占掉的宽度（只对左对齐有意义）。
///
/// # 返回
/// 位图左上角（图像坐标）。
pub fn line_origin(
    slot: &PlanSlot,
    align: Align,
    metrics: &FontMetrics,
    bitmap_w: i32,
    indent: f32,
) -> (i32, i32) {
    let (b, band) = (slot.ink, slot.band);
    let cy = band.y as f32 + band.height as f32 / 2.0;
    let baseline = slot
        .baseline
        .unwrap_or(cy - metrics.ink_height() / 2.0 + metrics.ink_top);
    let mut y = (baseline - metrics.ascent).round() as i32;
    // 墨迹块整体夹进填充区的上下沿（填充区被相邻行挤窄时也尽量不裁字形）
    let top = y as f32 + metrics.ascent - metrics.ink_top;
    let bottom = y as f32 + metrics.ascent + metrics.ink_bottom;
    let (fill_top, fill_bottom) = (slot.clip.y as f32, slot.clip.bottom() as f32);
    if bottom - top <= fill_bottom - fill_top {
        if top < fill_top {
            y += (fill_top - top).ceil() as i32;
        } else if bottom > fill_bottom {
            y -= (bottom - fill_bottom).ceil() as i32;
        }
    }
    let x = match align {
        Align::Left => b.x + indent.round() as i32,
        Align::Center => b.x + b.width / 2 - bitmap_w / 2,
        Align::Right => b.right() - bitmap_w,
    };
    let hi = (slot.clip.right() - bitmap_w).max(slot.clip.x);
    (x.clamp(slot.fill.x, hi), y)
}

/// 画一行（含行首项目符号）。
fn draw_line(
    rgba: &mut [u8],
    width: u32,
    height: u32,
    plan: &ParagraphPlan,
    line: &SlotLine,
    cache: &mut FontCache,
) {
    let slot = &plan.slots[line.slot];
    let (px, bold) = (plan.fit.font_px, plan.ink.bold);
    let metrics = cache.metrics(px, bold, plan.cjk);
    if let Some(mark) = line.bullet
        && let Some(bitmap) = (cache.raster)(&mark.to_string(), px, bold)
    {
        let (x, y) = line_origin(slot, Align::Left, &metrics, bitmap.width as i32, 0.0);
        blend_bitmap(rgba, width, height, &bitmap, (x, y), plan.ink.fg, slot.clip);
    }
    if line.text.is_empty() {
        return;
    }
    let Some(bitmap) = (cache.raster)(&line.text, px, bold) else {
        return;
    };
    let align = if line.bullet.is_some() {
        Align::Left
    } else {
        plan.align
    };
    let (x, y) = line_origin(slot, align, &metrics, bitmap.width as i32, line.indent);
    blend_bitmap(rgba, width, height, &bitmap, (x, y), plan.ink.fg, slot.clip);
}

/// 把规划好的替换画到图上：先把全部填充区涂上背景，再统一画字。
///
/// # 参数
/// - `rgba` / `width` / `height`：原图，就地改写。
/// - `plans`：规划结果。
/// - `raster`：光栅函数。
pub fn paint_plans(
    rgba: &mut [u8],
    width: u32,
    height: u32,
    plans: &[ParagraphPlan],
    raster: &mut RasterFn,
) {
    for plan in plans {
        for slot in &plan.slots {
            erase_text(
                rgba,
                width,
                height,
                slot.erase,
                slot.fill
                    .union_bounds(&slot.erase)
                    .union_bounds(&slot.box_rect),
                &slot.bg,
                plan.ink.fg,
            );
        }
    }
    let mut cache = FontCache {
        raster,
        cached: HashMap::new(),
    };
    for plan in plans {
        for line in &plan.fit.lines {
            draw_line(rgba, width, height, plan, line, &mut cache);
        }
    }
}

/// 把译文原位替换进图像：以 OCR 行框为锚点，先规划再统一上色。
///
/// # 参数
/// - `rgba`：原图（RGBA，行优先），就地改写。
/// - `width` / `height`：图像尺寸。
/// - `boxes`：OCR 行框（图像坐标）。
/// - `pairs`：逐段对照。
/// - `advance`：字宽函数（像素）。
/// - `raster`：把文字按字号画成覆盖率位图。
///
/// # 返回
/// 实际替换的段数。
///
/// ```ignore
/// let drawn = render_translation(&mut rgba, w, h, &boxes, &pairs, &advance, &mut raster);
/// ```
pub fn render_translation(
    rgba: &mut [u8],
    width: u32,
    height: u32,
    boxes: &[OcrTextBox],
    pairs: &[TranslatedParagraph],
    advance: &AdvanceFn,
    raster: &mut RasterFn,
) -> usize {
    if rgba.len() < width as usize * height as usize * 4 {
        return 0;
    }
    let plans = plan_paragraphs(rgba, width, height, boxes, pairs, advance, raster);
    paint_plans(rgba, width, height, &plans, raster);
    plans.len()
}

/// 用系统字体（GDI）做原位替换，单字宽度带缓存。
///
/// # 参数
/// - `rgba` / `width` / `height` / `boxes` / `pairs`：同 [`render_translation`]。
///
/// # 返回
/// 实际替换的段数。
pub fn render_with_system_font(
    rgba: &mut [u8],
    width: u32,
    height: u32,
    boxes: &[OcrTextBox],
    pairs: &[TranslatedParagraph],
) -> usize {
    let font = SystemFont::new();
    render_translation(
        rgba,
        width,
        height,
        boxes,
        pairs,
        &|c, px, bold| font.advance(c, px, bold),
        &mut |text, px, bold| font.raster(text, px, bold),
    )
}

/// 系统字体（GDI）的字宽与光栅入口；字宽按（字符，字号，字重）缓存。
pub(crate) struct SystemFont {
    /// 已量过的字宽，键为（字符，整像素字号，是否粗体）。
    widths: RefCell<HashMap<(char, u32, bool), f32>>,
}

impl SystemFont {
    /// 新建（缓存为空）。
    pub(crate) fn new() -> Self {
        Self {
            widths: RefCell::new(HashMap::new()),
        }
    }

    /// 单字前进宽度（像素）；GDI 测不出来时按半个字号估。
    pub(crate) fn advance(&self, c: char, px: u32, bold: bool) -> f32 {
        if let Some(v) = self.widths.borrow().get(&(c, px, bold)) {
            return *v;
        }
        let measured =
            text_raster::measure_text(&c.to_string(), DEFAULT_FONT_FAMILY, px as f32, bold)
                .map_or(px as f32 * 0.5, |m| m.width as f32);
        self.widths.borrow_mut().insert((c, px, bold), measured);
        measured
    }

    /// 把文字画成覆盖率位图；失败返回 `None`。
    pub(crate) fn raster(&self, text: &str, px: u32, bold: bool) -> Option<TextBitmap> {
        text_raster::rasterize_text(text, DEFAULT_FONT_FAMILY, px as f32, bold).ok()
    }
}

/// RGBA（直通 alpha）转 GPUI 图像资源要求的预乘 BGRA。
///
/// # 参数
/// - `rgba`：RGBA 字节，长度应为 4 的倍数（多出的尾部字节忽略）。
///
/// # 返回
/// 等长的预乘 BGRA 缓冲。
///
/// ```ignore
/// assert_eq!(rgba_to_premultiplied_bgra(&[10, 20, 30, 255]), [30, 20, 10, 255]);
/// ```
pub fn rgba_to_premultiplied_bgra(rgba: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(rgba.len());
    for px in rgba.chunks_exact(4) {
        let a = u32::from(px[3]);
        let premul = |v: u8| ((u32::from(v) * a + 127) / 255) as u8;
        out.extend_from_slice(&[premul(px[2]), premul(px[1]), premul(px[0]), px[3]]);
    }
    out
}

/// 详情面板的摆放：依次尝试选区右侧、左侧、下方、上方，要整块在屏内且不压 `avoid` 里的矩形（按钮条、主工具栏）；
/// 都不行就叠在选区左上角内侧。
///
/// # 参数
/// - `selection`：选区（逻辑像素，已裁到本屏）。
/// - `size`：面板尺寸 `(宽, 高)`。
/// - `screen`：本屏范围。
/// - `avoid`：不能压住的矩形。
/// - `gap`：与选区的间距。
///
/// # 返回
/// 面板左上角。
///
/// ```ignore
/// let (x, y) = detail_panel_origin(sel, (400, 300), screen, &[bar], 8);
/// ```
pub fn detail_panel_origin(
    selection: PhysicalRect,
    size: (i32, i32),
    screen: PhysicalRect,
    avoid: &[PhysicalRect],
    gap: i32,
) -> (i32, i32) {
    let (w, h) = size;
    let clamp_x = |x: i32| x.min(screen.right() - w).max(screen.x);
    let clamp_y = |y: i32| y.min(screen.bottom() - h).max(screen.y);
    let fits = |x: i32, y: i32| {
        let r = PhysicalRect::new(x, y, w, h);
        x >= screen.x
            && y >= screen.y
            && r.right() <= screen.right()
            && r.bottom() <= screen.bottom()
            && avoid.iter().all(|a| r.intersect(a).is_none())
    };
    let candidates = [
        (selection.right() + gap, clamp_y(selection.y)),
        (selection.x - gap - w, clamp_y(selection.y)),
        (clamp_x(selection.x), selection.bottom() + gap),
        (clamp_x(selection.x), selection.y - gap - h),
    ];
    candidates
        .into_iter()
        .find(|(x, y)| fits(*x, *y))
        .unwrap_or((clamp_x(selection.x + gap), clamp_y(selection.y + gap)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::translate_typeset::is_cjk_like;

    /// 假字宽：中日韩占满字号，其余半个字号。
    fn fake_adv(c: char, px: u32, _bold: bool) -> f32 {
        if is_cjk_like(c) {
            px as f32
        } else {
            px as f32 * 0.5
        }
    }

    /// 假光栅：位图高 1.3 倍字号，基线在 1.0 倍字号处；中日韩字形上 0.85 / 下 0.15 倍字号，
    /// 大写与普通字母上 0.7、下 0，gyjpq 上 0.5 下 0.2；每个字形是左右各留 1 像素的实心块。
    fn fake_raster(text: &str, px: u32, _bold: bool) -> Option<TextBitmap> {
        let scale = |k: f32| (px as f32 * k).round() as i32;
        let (ascent, cell) = (scale(1.0), (px as f32 * 1.3).ceil() as i32);
        let width: i32 = text
            .chars()
            .map(|c| fake_adv(c, px, false).round() as i32)
            .sum();
        let width = width.max(1);
        let mut coverage = vec![0u8; (width * cell) as usize];
        let mut x = 0;
        for c in text.chars() {
            let w = fake_adv(c, px, false).round() as i32;
            let rows = if c.is_whitespace() {
                None
            } else if is_cjk_like(c) {
                Some((ascent - scale(0.85), ascent + scale(0.15)))
            } else if "gyjpq".contains(c) {
                Some((ascent - scale(0.5), ascent + scale(0.2)))
            } else {
                Some((ascent - scale(0.7), ascent))
            };
            if let Some((top, bottom)) = rows {
                for y in top.max(0)..bottom.min(cell) {
                    for xx in (x + 1)..(x + w - 1).max(x + 1) {
                        coverage[(y * width + xx) as usize] = 255;
                    }
                }
            }
            x += w;
        }
        Some(TextBitmap {
            width: width as u32,
            height: cell as u32,
            coverage,
        })
    }

    /// 造一个行框。
    fn text_box(x: i32, y: i32, w: i32, h: i32) -> OcrTextBox {
        OcrTextBox {
            rect: PhysicalRect::new(x, y, w, h),
            text: "x".into(),
            confidence: None,
        }
    }

    /// 造一段。
    fn para(translated: &str, indices: &[usize]) -> TranslatedParagraph {
        TranslatedParagraph {
            source: "s".into(),
            translated: translated.into(),
            box_indices: indices.to_vec(),
        }
    }

    /// 纯色底图。
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

    /// 读像素 RGB。
    fn px(img: &[u8], w: u32, x: i32, y: i32) -> [u8; 3] {
        let i = (y as usize * w as usize + x as usize) * 4;
        [img[i], img[i + 1], img[i + 2]]
    }

    /// 用假字体规划（不涉及真实字体）。
    fn plan(
        w: u32,
        h: u32,
        img: &[u8],
        boxes: &[OcrTextBox],
        pairs: &[TranslatedParagraph],
    ) -> Vec<ParagraphPlan> {
        let mut raster = fake_raster;
        plan_paragraphs(img, w, h, boxes, pairs, &fake_adv, &mut raster)
    }

    /// 原位替换：行框附近被背景盖住并画上译文色，填充区外分毫不动；空译文 / 无框 / 越界的段不计。
    #[test]
    fn render_replaces_in_place() {
        let (w, h) = (100u32, 50u32);
        let mut img = solid(w, h, [255, 255, 255]);
        paint(&mut img, w, PhysicalRect::new(10, 17, 40, 10), [30, 30, 30]);
        let before = img.clone();
        let boxes = [text_box(10, 15, 40, 14)];
        let pairs = [para("你好", &[0]), para("", &[0]), para("无框", &[5])];
        let mut raster = fake_raster;
        let drawn = render_translation(&mut img, w, h, &boxes, &pairs, &fake_adv, &mut raster);
        assert_eq!(drawn, 1);
        // 原墨迹右端（译文只有两个字，盖不到的地方）已变回背景
        assert_eq!(px(&img, w, 49, 20), [255, 255, 255]);
        // 画上了译文色
        let dark = (0..h as i32)
            .flat_map(|y| (0..w as i32).map(move |x| (x, y)))
            .filter(|&(x, y)| px(&img, w, x, y)[0] < 100)
            .count();
        assert!(dark > 100, "应画出译文，实际 {dark} 个深色像素");
        // 填充区（行框外扩 3 像素）之外不变
        for y in 0..h as i32 {
            for x in 0..w as i32 {
                if !(7..53).contains(&x) || !(12..32).contains(&y) {
                    assert_eq!(px(&img, w, x, y), px(&before, w, x, y), "({x},{y}) 被改动");
                }
            }
        }
    }

    /// 无效尺寸的缓冲区不处理。
    #[test]
    fn render_rejects_short_buffer() {
        let mut img = vec![0u8; 8];
        let mut raster = fake_raster;
        let drawn = render_translation(
            &mut img,
            10,
            10,
            &[text_box(0, 0, 5, 5)],
            &[para("a", &[0])],
            &fake_adv,
            &mut raster,
        );
        assert_eq!(drawn, 0);
    }

    /// 译文沿原文行框依次流排：段内统一字号、行按槽序放、填充区互不重叠且贴着各自行框。
    #[test]
    fn plan_flows_along_original_lines() {
        let (w, h) = (200u32, 100u32);
        let img = solid(w, h, [255, 255, 255]);
        let boxes = [
            text_box(10, 10, 100, 14),
            text_box(10, 28, 100, 14),
            text_box(10, 46, 60, 14),
        ];
        let pairs = [para("一二三四五六七八九十一二三四五六七", &[0, 1, 2])];
        let plans = plan(w, h, &img, &boxes, &pairs);
        let p = &plans[0];
        assert_eq!(p.align, Align::Left);
        // 假图里没有墨迹可量，退回行框高度 14 ÷ 系统 OCR 系数 1.15 ≈ 12（假字体汉字墨迹恰为 1 倍字号）
        assert_eq!(p.fit.font_px, 12);
        let slots: Vec<usize> = p.fit.lines.iter().map(|l| l.slot).collect();
        assert_eq!(slots, [0, 1, 2], "第一行放满再进下一行框");
        for (s, b) in p.slots.iter().zip(&boxes) {
            assert!(s.fill.x <= b.rect.x && s.fill.right() >= b.rect.right());
            assert!(
                b.rect.x - s.fill.x <= 5 && s.fill.bottom() - b.rect.bottom() <= 5,
                "只略外扩几像素"
            );
        }
        for a in 0..p.slots.len() {
            for b in a + 1..p.slots.len() {
                assert!(p.slots[a].fill.intersect(&p.slots[b].fill).is_none());
            }
        }
    }

    /// 相邻的两段（行框只隔 2 像素）：所有行的填充区跨段也两两不相交，而且都贴着各自行框。
    #[test]
    fn fills_never_overlap_across_paragraphs() {
        let (w, h) = (200u32, 100u32);
        let img = solid(w, h, [255, 255, 255]);
        let boxes = [
            text_box(10, 10, 120, 14),
            text_box(10, 26, 120, 14),
            text_box(10, 42, 120, 14),
            text_box(14, 58, 60, 14),
        ];
        let pairs = [
            para("一二三四五六七八九十", &[0, 1]),
            para("甲乙丙丁", &[2, 3]),
        ];
        let plans = plan(w, h, &img, &boxes, &pairs);
        let fills: Vec<PhysicalRect> = plans
            .iter()
            .flat_map(|p| p.slots.iter().map(|s| s.fill))
            .collect();
        assert_eq!(fills.len(), 4);
        for (a, fa) in fills.iter().enumerate() {
            for fb in &fills[a + 1..] {
                assert!(fa.intersect(fb).is_none(), "{fa:?} 与 {fb:?} 重叠");
            }
        }
    }

    /// 字号随原文行高走：行高翻倍，字号约翻倍；不同行高的段各用各的字号。
    #[test]
    fn plan_font_follows_line_height() {
        let (w, h) = (300u32, 120u32);
        let img = solid(w, h, [255, 255, 255]);
        let boxes = [text_box(10, 10, 120, 14), text_box(10, 60, 240, 28)];
        let pairs = [para("你好", &[0]), para("你好", &[1])];
        let plans = plan(w, h, &img, &boxes, &pairs);
        assert_eq!(plans.len(), 2);
        let (small, big) = (plans[0].fit.font_px, plans[1].fit.font_px);
        assert!(
            (big as f32 / small as f32 - 2.0).abs() < 0.2,
            "{small} {big}"
        );
    }

    /// 放不下就在不越出行框的前提下整段缩小字号；缩到下限仍放不下才截断。
    #[test]
    fn plan_shrinks_then_truncates() {
        let (w, h) = (200u32, 60u32);
        let img = solid(w, h, [255, 255, 255]);
        let boxes = [text_box(10, 10, 80, 20)];
        // 8 字 × 18 = 144 > 84：单行槽里要缩到 10 像素才放得下 → 先缩小，不截断
        let shrink = plan(w, h, &img, &boxes, &[para("一二三四五六七八", &[0])]);
        assert!(shrink[0].fit.font_px < 18 && !shrink[0].fit.truncated);
        // 太长：截断并带省略号
        let long = "很长很长的一段文字".repeat(10);
        let cut = plan(w, h, &img, &boxes, &[para(&long, &[0])]);
        assert!(cut[0].fit.truncated);
        assert!(cut[0].fit.lines[0].text.ends_with('…'));
    }

    /// 字形墨迹块始终落在填充区内：不同行高、不同字号下都不裁切（含下伸部分）。
    #[test]
    fn glyph_ink_stays_inside_fill() {
        for line_h in [10, 14, 20, 33, 60] {
            let (w, h) = (400u32, 200u32);
            let img = solid(w, h, [255, 255, 255]);
            let boxes = [
                text_box(20, 30, 300, line_h),
                text_box(20, 30 + line_h + 4, 300, line_h),
            ];
            let pairs = [para("Typography gyp 排版测试一二三四五六七八九十", &[0, 1])];
            let plans = plan(w, h, &img, &boxes, &pairs);
            let p = &plans[0];
            let mut raster = fake_raster;
            let metrics = measure_font(&mut raster, p.fit.font_px, false, p.cjk);
            for line in &p.fit.lines {
                let slot = &p.slots[line.slot];
                let bmp = fake_raster(&line.text, p.fit.font_px, false).unwrap();
                let (x, y) = line_origin(slot, p.align, &metrics, bmp.width as i32, line.indent);
                let top = y as f32 + metrics.ascent - metrics.ink_top;
                let bottom = y as f32 + metrics.ascent + metrics.ink_bottom;
                assert!(
                    top >= slot.fill.y as f32 - 1.0 && bottom <= slot.fill.bottom() as f32 + 1.0,
                    "行高 {line_h}：墨迹 {top}..{bottom} 超出填充区 {:?}",
                    slot.fill
                );
                assert!(x >= slot.fill.x && x + bmp.width as i32 <= slot.fill.right() + 1);
            }
        }
    }

    /// 居中的段：每行以自己行框的中线为轴居中；左对齐的段从各行框左缘起笔。
    #[test]
    fn alignment_places_text_by_original_lines() {
        let (w, h) = (400u32, 120u32);
        let img = solid(w, h, [255, 255, 255]);
        let boxes = [text_box(30, 10, 140, 14), text_box(50, 28, 100, 14)];
        let plans = plan(w, h, &img, &boxes, &[para("一二三四五六七八", &[0, 1])]);
        let p = &plans[0];
        assert_eq!(p.align, Align::Center);
        let mut raster = fake_raster;
        let m = measure_font(&mut raster, p.fit.font_px, false, p.cjk);
        let line = &p.fit.lines[0];
        let bmp = fake_raster(&line.text, p.fit.font_px, false).unwrap();
        let (x, _) = line_origin(&p.slots[0], p.align, &m, bmp.width as i32, 0.0);
        assert_eq!(x + bmp.width as i32 / 2, 100, "以行框中线 x=100 居中");
        // 缩进的续行（项目符号式）：不齐，按左对齐，各行从自己行框左缘起笔
        let boxes = [
            text_box(10, 10, 200, 14),
            text_box(40, 28, 100, 14),
            text_box(10, 46, 150, 14),
        ];
        let plans = plan(
            w,
            h,
            &img,
            &boxes,
            &[para(
                "一二三四五六七八九十一二三四五六七八九十一二三四五",
                &[0, 1, 2],
            )],
        );
        assert_eq!(plans[0].align, Align::Left);
    }

    /// 先全部填充、再统一画字：嵌套的行框（小框套在大框里）填充区必然重叠，小框的填充区不能盖掉大框已画的字。
    #[test]
    fn all_fills_precede_all_text() {
        let (w, h) = (200u32, 80u32);
        let mut img = solid(w, h, [255, 255, 255]);
        // A 是大框（红墨迹在上沿），B 套在 A 里面（B 没有墨迹）
        let boxes = [text_box(10, 10, 60, 24), text_box(30, 16, 20, 10)];
        paint(&mut img, w, PhysicalRect::new(10, 12, 60, 2), [200, 0, 0]);
        let pairs = [para("一二三", &[0]), para(".", &[1])];
        let mut raster = fake_raster;
        assert_eq!(
            render_translation(&mut img, w, h, &boxes, &pairs, &fake_adv, &mut raster),
            2
        );
        // “二”（x≈31..52）的中段落在 B 的填充区里（B 自己的字只有左端一个小点），仍是 A 的红色
        assert_eq!(px(&img, w, 45, 22), [200, 0, 0]);
    }

    /// 不翻译的行框是障碍：相邻段的填充区不会盖到它，它的原文保持原样。
    #[test]
    fn untouched_boxes_are_obstacles() {
        let (w, h) = (120u32, 60u32);
        let mut img = solid(w, h, [255, 255, 255]);
        let keep = PhysicalRect::new(10, 30, 60, 12);
        paint(&mut img, w, keep, [0, 128, 0]);
        let boxes = [text_box(10, 10, 60, 12), text_box(10, 30, 60, 12)];
        let mut raster = fake_raster;
        let drawn = render_translation(
            &mut img,
            w,
            h,
            &boxes,
            &[para("你好", &[0])],
            &fake_adv,
            &mut raster,
        );
        assert_eq!(drawn, 1);
        for y in keep.y..keep.bottom() {
            for x in keep.x..keep.right() {
                assert_eq!(px(&img, w, x, y), [0, 128, 0], "({x},{y})");
            }
        }
    }

    /// 渐变背景：行框两侧补出来的填充区颜色与原图渐变连续，不出现色块。
    #[test]
    fn gradient_background_has_no_seam() {
        let (w, h) = (140u32, 50u32);
        let mut img = solid(w, h, [0, 0, 0]);
        for x in 0..w as i32 {
            let t = x as f32 / (w - 1) as f32;
            paint(
                &mut img,
                w,
                PhysicalRect::new(x, 0, 1, h as i32),
                [(40.0 + 160.0 * t) as u8, 70, (220.0 - 120.0 * t) as u8],
            );
        }
        paint(
            &mut img,
            w,
            PhysicalRect::new(40, 20, 60, 8),
            [255, 255, 255],
        );
        let before = img.clone();
        let boxes = [text_box(40, 18, 60, 12)];
        let mut raster = fake_raster;
        render_translation(
            &mut img,
            w,
            h,
            &boxes,
            &[para("你", &[0])],
            &fake_adv,
            &mut raster,
        );
        let diff = |a: [u8; 3], b: [u8; 3]| {
            (0..3)
                .map(|i| (i32::from(a[i]) - i32::from(b[i])).abs())
                .sum::<i32>()
        };
        // 译文只有一个字（左端），其余位置只剩背景：与同列未被改写的原图渐变比较
        for x in [60, 80, 97] {
            let want = px(&before, w, x, 5);
            assert!(
                diff(px(&img, w, x, 24), want) <= 14,
                "x={x}：{:?} vs {want:?}",
                px(&img, w, x, 24)
            );
        }
    }

    /// 系统字体（GDI）走通：中英文都能画进框，框内像素确实变化；单字宽度累加与整行测量基本一致。
    #[cfg(windows)]
    #[test]
    fn render_with_system_font_draws_cjk_and_latin() {
        let (w, h) = (200u32, 60u32);
        let mut img = solid(w, h, [255, 255, 255]);
        paint(&mut img, w, PhysicalRect::new(10, 10, 140, 20), [0, 0, 0]);
        let boxes = [text_box(10, 10, 140, 20)];
        let pairs = [para("你好 world 翻译结果", &[0])];
        assert_eq!(render_with_system_font(&mut img, w, h, &boxes, &pairs), 1);
        let region = || (10..30).flat_map(|y| (10..150).map(move |x| (x, y)));
        let ink = region()
            .filter(|&(x, y)| px(&img, w, x, y)[0] < 128)
            .count();
        let blank = region()
            .filter(|&(x, y)| px(&img, w, x, y)[0] >= 128)
            .count();
        assert!(ink > 20, "应画出文字笔画");
        assert!(blank > ink, "原来整块深色应被背景色盖住，只剩笔画");
        let font = SystemFont::new();
        for (text, px_size, bold) in [
            ("Hello world 你好", 12u32, false),
            ("Typography", 16, true),
            ("风险管理和合规", 22, false),
        ] {
            let summed: f32 = text.chars().map(|c| font.advance(c, px_size, bold)).sum();
            let whole = text_raster::measure_text(text, DEFAULT_FONT_FAMILY, px_size as f32, bold)
                .unwrap()
                .width as f32;
            assert!(
                (summed - whole).abs() <= 2.0 + text.chars().count() as f32 * 0.25,
                "{text}：{summed} vs {whole}"
            );
        }
    }

    /// 真实字体下，译文比原文长得多：每一行实测宽度都不超出填充区，放不下的段落截断并带省略号。
    #[cfg(windows)]
    #[test]
    fn system_font_long_translation_fits_slots() {
        let (w, h) = (300u32, 120u32);
        let img = solid(w, h, [255, 255, 255]);
        let boxes = [
            text_box(10, 10, 200, 20),
            text_box(10, 34, 200, 20),
            text_box(10, 58, 120, 20),
        ];
        let font = SystemFont::new();
        let long = "Risk management and compliance require every business unit to identify, assess,                     control and report the risks it faces every day, without exception.";
        let pairs = [para(long, &[0, 1, 2])];
        let plans = plan_paragraphs(
            &img,
            w,
            h,
            &boxes,
            &pairs,
            &|c, px, bold| font.advance(c, px, bold),
            &mut |t, px, bold| font.raster(t, px, bold),
        );
        let p = &plans[0];
        assert!(p.fit.font_px >= MIN_FIT_PX);
        for line in &p.fit.lines {
            let slot = &p.slots[line.slot];
            let width = font
                .raster(&line.text, p.fit.font_px, p.ink.bold)
                .unwrap()
                .width as i32;
            assert!(
                width <= slot.fill.width + 1,
                "第 {} 行 {width}px 超出填充区 {}px：{}",
                line.slot,
                slot.fill.width,
                line.text
            );
        }
        let shown: usize = p.fit.lines.iter().map(|l| l.text.len()).sum();
        assert!(
            p.fit.truncated || shown >= long.len() - 4,
            "既没截断也没排全"
        );
    }

    /// RGBA 转预乘 BGRA：不透明只换通道顺序，半透明按 alpha 预乘。
    #[test]
    fn premultiplied_bgra_conversion() {
        assert_eq!(
            rgba_to_premultiplied_bgra(&[10, 20, 30, 255]),
            [30, 20, 10, 255]
        );
        assert_eq!(
            rgba_to_premultiplied_bgra(&[200, 100, 0, 128]),
            [0, 50, 100, 128]
        );
        assert_eq!(rgba_to_premultiplied_bgra(&[9, 9, 9, 0]), [0, 0, 0, 0]);
    }

    /// 详情面板摆放：右侧够就在右侧；右侧被按钮条占住或放不下依次换左 / 下 / 上；全不行叠在选区内。
    #[test]
    fn detail_panel_placement_cases() {
        let screen = PhysicalRect::new(0, 0, 1200, 700);
        let size = (400, 300);
        let sel = PhysicalRect::new(100, 100, 300, 200);
        // 右侧够：贴选区右缘，与选区顶对齐
        assert_eq!(detail_panel_origin(sel, size, screen, &[], 8), (408, 100));
        // 右侧被按钮条（选区右侧外缘底部对齐）压住：改放左侧（左侧也放不下，100 < 408）→ 下方
        let bar = PhysicalRect::new(408, 266, 150, 34);
        assert_eq!(
            detail_panel_origin(sel, size, screen, &[bar], 8),
            (100, 308)
        );
        // 下方被工具栏占住：改放上方（上方放不下 100 < 308）→ 都不行，叠在选区内
        let tb = PhysicalRect::new(100, 308, 300, 38);
        assert_eq!(
            detail_panel_origin(sel, size, screen, &[bar, tb], 8),
            (108, 108)
        );
        // 选区在屏幕右侧：左侧放得下
        let sel = PhysicalRect::new(800, 100, 300, 200);
        assert_eq!(detail_panel_origin(sel, size, screen, &[], 8), (392, 100));
        // 结果总在屏内
        let sel = PhysicalRect::new(0, 600, 1200, 100);
        let (x, y) = detail_panel_origin(sel, size, screen, &[], 8);
        assert!(x >= 0 && y >= 0 && x + 400 <= 1200 && y + 300 <= 700);
    }
}
