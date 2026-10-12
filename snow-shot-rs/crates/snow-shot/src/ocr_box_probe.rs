//! 真实 OCR 行框与墨迹关系的探测（仅测试编译，默认 `#[ignore]`，需要本机已装 OCR 运行时与模型）。
//!
//! 用法：`CISOX_OCR_PROBE_IMAGE=<png> cargo test -p snow-shot ocr_box_probe -- --ignored --nocapture`，
//! 对系统 OCR 与本地模型 OCR 各识别一遍，逐行打印“行框高度 / 行框内实际墨迹高度”，
//! 用来标定字号推导里的行框外扩系数（见 `translate_render::BOX_HEIGHT_PER_INK`）。

use crate::ocr_backend::{LocalModelOcr, OcrEngine, OcrInput, SystemOcr};
use crate::ocr_service::{OcrRequestConfig, OcrService, OcrTextBox};
use std::sync::Arc;

/// 图片路径环境变量。
const IMAGE_ENV: &str = "CISOX_OCR_PROBE_IMAGE";
/// 判定墨迹像素的最小色差占最大色差的比例。
const INK_RATIO: f32 = 0.5;
/// 找墨迹时，行框上下各放宽的像素数。
const GROW: i32 = 2;

/// 行框附近的墨迹范围 `(左, 上, 右, 下)`（含边界）：在行框左右范围内、上下各放宽 `GROW` 像素的区域里找墨迹，
/// 纵向只取与行框中线所在行连续相接的那一段（避免把相邻行算进来）；没有墨迹返回 `None`。
fn ink_extent(
    rgba: &[u8],
    (width, height): (u32, u32),
    b: &OcrTextBox,
) -> Option<(i32, i32, i32, i32)> {
    let r = b.rect;
    let px = |x: i32, y: i32| {
        let i = (y as usize * width as usize + x as usize) * 4;
        [rgba[i], rgba[i + 1], rgba[i + 2]]
    };
    let (y0, y1) = ((r.y - GROW).max(0), (r.bottom() + GROW).min(height as i32));
    // 背景：区域上下两条边与左右两条边像素的逐通道中位数
    let mut edge: Vec<[u8; 3]> = Vec::new();
    for x in r.x..r.right() {
        edge.push(px(x, y0));
        edge.push(px(x, y1 - 1));
    }
    let bg = [0, 1, 2].map(|c| {
        let mut v: Vec<u8> = edge.iter().map(|p| p[c]).collect();
        v.sort_unstable();
        v[v.len() / 2]
    });
    let dist = |p: [u8; 3]| -> i32 {
        (0..3)
            .map(|i| (i32::from(p[i]) - i32::from(bg[i])).abs())
            .sum()
    };
    let max = (r.y..r.bottom())
        .flat_map(|y| (r.x..r.right()).map(move |x| (x, y)))
        .map(|(x, y)| dist(px(x, y)))
        .max()?;
    if max < 90 {
        return None;
    }
    let thr = (max as f32 * INK_RATIO) as i32;
    let has_ink = |y: i32| (r.x..r.right()).any(|x| dist(px(x, y)) >= thr);
    let mid = r.y + r.height / 2;
    let (mut top, mut bottom) = (mid, mid);
    if !has_ink(mid) {
        // 中线行恰好没墨迹：向上下各找最近的有墨迹行
        top = (y0..mid).rev().find(|&y| has_ink(y))?;
        bottom = top;
    }
    while top > y0 && has_ink(top - 1) {
        top -= 1;
    }
    while bottom + 1 < y1 && has_ink(bottom + 1) {
        bottom += 1;
    }
    let mut cols = (r.x..r.right()).filter(|&x| (top..=bottom).any(|y| dist(px(x, y)) >= thr));
    let (left, right) = (cols.clone().next()?, cols.next_back()?);
    Some((left, top, right, bottom))
}

/// 打印一个后端的行框 / 墨迹统计。
fn report(name: &str, rgba: &[u8], size: (u32, u32), boxes: &[OcrTextBox]) {
    let mut ratios: Vec<f32> = Vec::new();
    for b in boxes {
        let Some((left, top, right, bottom)) = ink_extent(rgba, size, b) else {
            continue;
        };
        let ink_h = bottom - top + 1;
        let ratio = b.rect.height as f32 / ink_h as f32;
        ratios.push(ratio);
        eprintln!(
            "[{name}] box={}x{} ink_h={:3} h_ratio={ratio:.2} w_ratio={:.2} top_gap={:2} bottom_gap={:2} left_gap={:2} {:?}",
            b.rect.width,
            b.rect.height,
            ink_h,
            b.rect.width as f32 / (right - left + 1) as f32,
            top - b.rect.y,
            b.rect.bottom() - 1 - bottom,
            left - b.rect.x,
            b.text.chars().take(20).collect::<String>()
        );
    }
    ratios.sort_by(f32::total_cmp);
    if !ratios.is_empty() {
        eprintln!(
            "[{name}] 行框高/墨迹高：min={:.2} median={:.2} max={:.2}（{} 行）",
            ratios[0],
            ratios[ratios.len() / 2],
            ratios[ratios.len() - 1],
            ratios.len()
        );
    }
}

/// 两个后端各识别一遍并打印统计。
#[test]
#[ignore = "需要本机 OCR 运行时与模型，手动运行"]
fn ocr_box_probe() {
    let Ok(path) = std::env::var(IMAGE_ENV) else {
        eprintln!("未设置 {IMAGE_ENV}，跳过");
        return;
    };
    let img = image::open(&path).expect("读图").to_rgba8();
    let (w, h) = img.dimensions();
    let rgba = img.into_raw();
    let input = OcrInput {
        width: w,
        height: h,
        rgba: &rgba,
    };
    match SystemOcr.recognize(&input) {
        Ok(r) => report("system", &rgba, (w, h), &r.boxes),
        Err(e) => eprintln!("system OCR 失败：{e:?}"),
    }
    let data_root = std::path::PathBuf::from(std::env::var("LOCALAPPDATA").expect("LOCALAPPDATA"))
        .join("Cisox");
    let service = Arc::new(OcrService::new(&data_root));
    let config = OcrRequestConfig {
        model_kind: "small".into(),
        directml: false,
        resize_policy: 0,
        resident: false,
    };
    match LocalModelOcr::new(service.clone(), config).recognize(&input) {
        Ok(r) => report("local", &rgba, (w, h), &r.boxes),
        Err(e) => eprintln!("local OCR 失败：{e:?}"),
    }
    service.shutdown();
}

/// 真实 OCR 回归：对合成场景的原图分别跑系统 OCR 与本地模型，用 OCR 真实给出的行框走字号推导，
/// 译文满行墨迹高度与原文满行墨迹高度之比应在 0.85–1.15（OCR 偶有误识 / 误切行，所以比纯合成测试略宽）。
#[test]
#[ignore = "需要本机 OCR 运行时与模型，手动运行"]
fn real_ocr_font_size_regression() {
    use crate::translate_layout::{LayoutMode, paragraphs_from_boxes};
    use crate::translate_render::{SystemFont, plan_paragraphs};
    use crate::translate_render_scene::build_scene;
    use crate::translate_service::TranslatedParagraph;
    use snow_platform::text_raster;

    let scene = build_scene();
    let input = OcrInput {
        width: scene.width,
        height: scene.height,
        rgba: &scene.rgba,
    };
    let data_root = std::path::PathBuf::from(std::env::var("LOCALAPPDATA").expect("LOCALAPPDATA"))
        .join("Cisox");
    let service = Arc::new(OcrService::new(&data_root));
    let config = OcrRequestConfig {
        model_kind: "small".into(),
        directml: false,
        resize_policy: 0,
        resident: false,
    };
    let local = LocalModelOcr::new(service.clone(), config);
    let engines: [(&str, &dyn OcrEngine); 2] = [("system", &SystemOcr), ("local", &local)];
    let font = SystemFont::new();
    let ink_height = |text: &str, family: &str, px: f32, bold: bool| -> i32 {
        let b = text_raster::rasterize_text(text, family, px, bold).expect("GDI");
        let w = b.width as usize;
        let rows: Vec<usize> = (0..b.height as usize)
            .filter(|&y| b.coverage[y * w..(y + 1) * w].iter().any(|&c| c >= 48))
            .collect();
        (rows.last().unwrap_or(&0) - rows.first().unwrap_or(&0) + 1) as i32
    };
    for (name, engine) in engines {
        let boxes = engine.recognize(&input).expect("OCR").boxes;
        let pairs: Vec<TranslatedParagraph> = paragraphs_from_boxes(&boxes, LayoutMode::SmartMerge)
            .into_iter()
            .map(|(source, box_indices)| TranslatedParagraph {
                source,
                translated: "合".into(),
                box_indices,
            })
            .collect();
        let plans = plan_paragraphs(
            &scene.rgba,
            scene.width,
            scene.height,
            &boxes,
            &pairs,
            &|c, px, bold| font.advance(c, px, bold),
            &mut |t, px, bold| font.raster(t, px, bold),
        );
        let mut checked = 0;
        for plan in &plans {
            let first = &plan.slots[0];
            // 与这一行重叠最多的真值行
            let Some((truth_ix, overlap)) = scene
                .truth
                .iter()
                .enumerate()
                .map(|(i, t)| {
                    let o = t
                        .ink
                        .intersect(&first.box_rect)
                        .map_or(0, |r| r.width * r.height);
                    (i, o)
                })
                .max_by_key(|(_, o)| *o)
            else {
                continue;
            };
            let truth = &scene.truth[truth_ix];
            // OCR 读错（如 “appemng agal”、大小写读错的 “prOCeSS.”）的行，行框和字形类别都不可信，不检验
            let norm = |t: &str| -> String { t.chars().filter(|c| c.is_alphanumeric()).collect() };
            let source = norm(&pairs[plan.pair_index].source);
            let expected = norm(&scene.boxes[truth_ix].text);
            if overlap == 0
                || truth.px < 14.0
                || !(source.contains(&expected) || expected.contains(&source))
            {
                continue;
            }
            let original = ink_height("Hgdy", "Segoe UI", truth.px, truth.bold);
            let translated = ink_height(
                "国",
                "Microsoft YaHei UI",
                plan.fit.font_px as f32,
                plan.ink.bold,
            );
            let ratio = translated as f32 / original as f32;
            eprintln!(
                "[{name}] {:?} 原文 {}px 满行墨迹 {original} 译文 {}px 墨迹 {translated} 比值 {ratio:.2}",
                pairs[plan.pair_index]
                    .source
                    .chars()
                    .take(18)
                    .collect::<String>(),
                truth.px,
                plan.fit.font_px
            );
            assert!(
                (0.85..=1.15).contains(&ratio),
                "[{name}] 比值 {ratio:.2} 超出范围"
            );
            checked += 1;
        }
        assert!(checked >= 6, "[{name}] 参与检验的段太少：{checked}");
    }
    service.shutdown();
}
