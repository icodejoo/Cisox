//! 译文原位替换的离屏调试场景（仅测试编译）：用 GDI 在合成图上画出“彩色卡片 + 标题 + 正文 + 项目符号 + 深色小字”，
//! 按真实 OCR 行框的方式记下每行外接矩形，再走完整的段落整理与原位替换，把前后两张图写成 PNG 方便肉眼对照。
//!
//! 设置环境变量 `CISOX_TRANSLATE_DUMP=<目录>` 后运行 `cargo test -p snow-shot translate_scene` 会写出
//! `<标签>_original.png` 与 `<标签>_translated.png`；不设置时只做不变量断言。

use crate::ocr_service::OcrTextBox;
use crate::translate_layout::{LayoutMode, paragraphs_from_boxes};
use crate::translate_render::{SystemFont, plan_paragraphs, render_with_system_font};
use crate::translate_service::TranslatedParagraph;
use snow_platform::text_raster;
use snow_ui::shell::geometry::PhysicalRect;
use std::collections::HashMap;

/// 场景图宽度（像素）。
const SCENE_W: u32 = 902;
/// 场景图高度（像素）。
const SCENE_H: u32 = 500;
/// 原文用的字体族。
const SCENE_FONT: &str = "Segoe UI";
/// 判定字形“有墨迹”的最小覆盖率。
const INK_COVERAGE: u8 = 48;
/// 导出目录环境变量名。
const DUMP_ENV: &str = "CISOX_TRANSLATE_DUMP";
/// 白色。
const WHITE: [u8; 3] = [255, 255, 255];
/// 近黑色文字。
const DARK: [u8; 3] = [30, 30, 40];

/// 一行文字的画法。
#[derive(Clone, Copy)]
struct Style {
    /// 字号（像素）。
    px: f32,
    /// 是否粗体。
    bold: bool,
    /// 文字颜色。
    color: [u8; 3],
    /// OCR 行框相对墨迹外接矩形向外扩的像素数。
    pad: i32,
}

/// 调试场景：图像与对应的 OCR 行框。
pub struct Scene {
    /// 宽度。
    pub width: u32,
    /// 高度。
    pub height: u32,
    /// RGBA 像素（行优先）。
    pub rgba: Vec<u8>,
    /// OCR 行框（按画的先后顺序，即阅读顺序）；这里是“紧贴墨迹”的行框，见 [`Scene::restyled_boxes`]。
    pub boxes: Vec<OcrTextBox>,
    /// 与 `boxes` 一一对应的真值：原文真实墨迹范围与画它用的字号 / 粗体。
    pub truth: Vec<LineTruth>,
}

/// 一行原文的真值。
#[derive(Clone, Copy)]
pub struct LineTruth {
    /// 真实墨迹外接矩形（不含任何外扩）。
    pub ink: PhysicalRect,
    /// 画原文用的字号（像素）。
    pub px: f32,
    /// 是否粗体。
    pub bold: bool,
}

/// 模拟不同 OCR 后端的行框形状（按 `ocr_box_probe` 在真实样片上的实测归纳）。
#[derive(Clone, Copy, Debug)]
pub enum BoxStyle {
    /// 紧贴墨迹（再各外扩 1–2 像素）。
    Tight,
    /// PP-OCR 的 DB 外扩：行框高度约为墨迹的 `r` 倍，多出的部分六成在上方，左右各外扩约 `0.6 × (r - 1)` 倍墨迹高度。
    Unclip(f32),
    /// 系统 OCR 在中日韩上的样子：行框比墨迹高约三成、上缘切进墨迹 2 像素、下方留出空隙；没有置信度。
    SystemLoose,
}

impl Scene {
    /// 新建一张纯色画布。
    fn new(width: u32, height: u32, bg: [u8; 3]) -> Self {
        let rgba = (0..width * height)
            .flat_map(|_| [bg[0], bg[1], bg[2], 255])
            .collect();
        Self {
            width,
            height,
            rgba,
            boxes: Vec::new(),
            truth: Vec::new(),
        }
    }

    /// 按某个后端的行框形状，由真实墨迹重新生成全部行框（与 `boxes` 一一对应）。
    pub fn restyled_boxes(&self, style: BoxStyle) -> Vec<OcrTextBox> {
        let scale = |v: f32| v.round() as i32;
        self.boxes
            .iter()
            .zip(&self.truth)
            .map(|(b, t)| {
                let ink = t.ink;
                let (rect, confidence) = match style {
                    BoxStyle::Tight => (b.rect, b.confidence),
                    BoxStyle::Unclip(r) => {
                        let h = scale(ink.height as f32 * r);
                        let extra = h - ink.height;
                        let side = scale(0.6 * (r - 1.0) * ink.height as f32);
                        (
                            PhysicalRect::new(
                                ink.x - side,
                                ink.y - scale(extra as f32 * 0.6),
                                ink.width + 2 * side,
                                h,
                            ),
                            Some(0.9),
                        )
                    }
                    BoxStyle::SystemLoose => (
                        PhysicalRect::new(
                            ink.x - 1,
                            ink.y + 2,
                            ink.width + 2,
                            scale(ink.height as f32 * 1.3),
                        ),
                        None,
                    ),
                };
                OcrTextBox {
                    rect,
                    text: b.text.clone(),
                    confidence,
                }
            })
            .collect()
    }

    /// 写一个像素（越界忽略）。
    fn put(&mut self, x: i32, y: i32, c: [u8; 3]) {
        if x < 0 || y < 0 || x >= self.width as i32 || y >= self.height as i32 {
            return;
        }
        let i = (y as usize * self.width as usize + x as usize) * 4;
        self.rgba[i..i + 3].copy_from_slice(&c);
    }

    /// 线性插值两种颜色。
    fn lerp(a: [u8; 3], b: [u8; 3], t: f32) -> [u8; 3] {
        [0, 1, 2].map(|i| (f32::from(a[i]) + (f32::from(b[i]) - f32::from(a[i])) * t).round() as u8)
    }

    /// 画渐变矩形（`horizontal` 为 true 时沿水平方向渐变，否则沿垂直方向）。
    fn gradient(&mut self, r: (i32, i32, i32, i32), c0: [u8; 3], c1: [u8; 3], horizontal: bool) {
        let (x, y, w, h) = r;
        for yy in y..y + h {
            for xx in x..x + w {
                let t = if horizontal {
                    (xx - x) as f32 / w.max(1) as f32
                } else {
                    (yy - y) as f32 / h.max(1) as f32
                };
                self.put(xx, yy, Self::lerp(c0, c1, t));
            }
        }
    }

    /// 画纯色矩形。
    fn rect(&mut self, r: (i32, i32, i32, i32), c: [u8; 3]) {
        self.gradient(r, c, c, true);
    }

    /// 画实心圆（模拟头像 / 图标 / 插图）。
    fn disc(&mut self, cx: i32, cy: i32, radius: i32, c: [u8; 3]) {
        for y in cy - radius..=cy + radius {
            for x in cx - radius..=cx + radius {
                if (x - cx).pow(2) + (y - cy).pow(2) <= radius * radius {
                    self.put(x, y, c);
                }
            }
        }
    }

    /// 用 GDI 画一行文字并记下它的 OCR 行框（墨迹外接矩形各向外扩 `style.pad` 像素）；返回该行的墨迹宽度。
    fn text(&mut self, x: i32, y: i32, text: &str, style: Style) -> i32 {
        let Style {
            px,
            bold,
            color,
            pad,
        } = style;
        let bitmap = text_raster::rasterize_text(text, SCENE_FONT, px, bold).expect("GDI 画原文");
        let (bw, bh) = (bitmap.width as i32, bitmap.height as i32);
        let (mut x0, mut y0, mut x1, mut y1) = (bw, bh, -1, -1);
        for row in 0..bh {
            for col in 0..bw {
                let cov = bitmap.coverage[(row * bw + col) as usize];
                if cov == 0 {
                    continue;
                }
                let i = ((y + row).clamp(0, self.height as i32 - 1) as usize * self.width as usize
                    + (x + col).clamp(0, self.width as i32 - 1) as usize)
                    * 4;
                if (x + col) >= 0
                    && (y + row) >= 0
                    && (x + col) < self.width as i32
                    && (y + row) < self.height as i32
                {
                    for (c, v) in color.iter().enumerate() {
                        let dst = u32::from(self.rgba[i + c]);
                        self.rgba[i + c] =
                            ((dst * (255 - u32::from(cov)) + u32::from(*v) * u32::from(cov) + 127)
                                / 255) as u8;
                    }
                }
                if cov >= INK_COVERAGE {
                    x0 = x0.min(col);
                    x1 = x1.max(col);
                    y0 = y0.min(row);
                    y1 = y1.max(row);
                }
            }
        }
        if x1 >= 0 {
            self.truth.push(LineTruth {
                ink: PhysicalRect::new(x + x0, y + y0, x1 - x0 + 1, y1 - y0 + 1),
                px,
                bold,
            });
            self.boxes.push(OcrTextBox {
                rect: PhysicalRect::new(
                    x + x0 - pad,
                    y + y0 - pad,
                    x1 - x0 + 1 + 2 * pad,
                    y1 - y0 + 1 + 2 * pad,
                ),
                text: text.to_string(),
                confidence: Some(0.95),
            });
        }
        x1 - x0 + 1
    }
}

/// 按“截图里那种”版式搭出场景：渐变底、三张顶部卡片、居中标题、三张橙色卡片（含项目符号）、底部横幅与页脚小字。
pub fn build_scene() -> Scene {
    let mut s = Scene::new(SCENE_W, SCENE_H, [60, 120, 230]);
    s.gradient((0, 0, 902, 500), [90, 170, 245], [110, 60, 205], false);

    // 顶部三张卡片：紫色表头 + 白色正文区，卡片之间只隔十几像素
    let cards = [(12, 280), (307, 280), (603, 282)];
    for (n, (x, w)) in cards.iter().enumerate() {
        s.gradient(
            (*x, 28, *w, 42),
            [60, 40, 200 - n as u8 * 10],
            [150, 60, 210],
            true,
        );
        s.rect((*x, 70, *w, 86), WHITE);
        s.disc(x + 28, 49, 13, WHITE);
        // 插图：头像 + 身体（贴着正文区左侧，别压文字）
        s.disc(x + 55, 100, 16, [240, 190, 150]);
        s.rect((x + 30, 118, 52, 36), [40, 70, 160]);
    }
    s.text(
        62,
        36,
        "Business unit",
        Style {
            px: 22.0,
            bold: true,
            color: WHITE,
            pad: 2,
        },
    );
    s.text(
        375,
        30,
        "Risk management and",
        Style {
            px: 16.0,
            bold: false,
            color: WHITE,
            pad: 2,
        },
    );
    s.text(
        375,
        47,
        "compliance",
        Style {
            px: 16.0,
            bold: false,
            color: WHITE,
            pad: 2,
        },
    );
    s.text(
        663,
        36,
        "Internal audit",
        Style {
            px: 22.0,
            bold: true,
            color: WHITE,
            pad: 2,
        },
    );
    s.text(
        112,
        76,
        "Directly manage risk",
        Style {
            px: 14.0,
            bold: true,
            color: DARK,
            pad: 1,
        },
    );
    s.text(
        112,
        93,
        "Carry out daily activities,",
        Style {
            px: 14.0,
            bold: false,
            color: DARK,
            pad: 1,
        },
    );
    s.text(
        112,
        110,
        "identify, assess, control and",
        Style {
            px: 14.0,
            bold: false,
            color: DARK,
            pad: 1,
        },
    );
    s.text(
        112,
        127,
        "mitigate risk in their area.",
        Style {
            px: 14.0,
            bold: false,
            color: DARK,
            pad: 1,
        },
    );
    s.text(
        412,
        76,
        "Set policy",
        Style {
            px: 14.0,
            bold: true,
            color: DARK,
            pad: 1,
        },
    );
    s.text(
        412,
        93,
        "Monitor risk and ensure",
        Style {
            px: 14.0,
            bold: false,
            color: DARK,
            pad: 1,
        },
    );
    s.text(
        412,
        110,
        "internal compliance with",
        Style {
            px: 14.0,
            bold: false,
            color: DARK,
            pad: 1,
        },
    );
    s.text(
        412,
        127,
        "rules and controls.",
        Style {
            px: 14.0,
            bold: false,
            color: DARK,
            pad: 1,
        },
    );
    s.text(
        703,
        76,
        "Independent assurance",
        Style {
            px: 14.0,
            bold: true,
            color: DARK,
            pad: 1,
        },
    );
    s.text(
        703,
        93,
        "Internal controls are",
        Style {
            px: 14.0,
            bold: false,
            color: DARK,
            pad: 1,
        },
    );
    s.text(
        703,
        110,
        "effective in the",
        Style {
            px: 14.0,
            bold: false,
            color: DARK,
            pad: 1,
        },
    );
    s.text(
        703,
        127,
        "governance process.",
        Style {
            px: 14.0,
            bold: false,
            color: DARK,
            pad: 1,
        },
    );

    // 中部米色大面板 + 居中标题
    s.rect((12, 174, 868, 196), [253, 218, 198]);
    let title_w = 220;
    s.text(
        451 - title_w / 2,
        186,
        "What can you do",
        Style {
            px: 30.0,
            bold: false,
            color: [205, 95, 35],
            pad: 2,
        },
    );

    // 三张橙色卡片
    for (x, w) in [(28, 245), (290, 330), (637, 229)] {
        s.gradient((x, 228, w, 130), [245, 135, 65], [240, 120, 70], false);
        s.rect((x, 228, w, 2), [170, 90, 200]);
    }
    s.disc(78, 250, 13, [60, 90, 170]);
    s.text(
        103,
        238,
        "Identify",
        Style {
            px: 24.0,
            bold: true,
            color: DARK,
            pad: 2,
        },
    );
    s.text(
        46,
        285,
        "Identify the risks in your",
        Style {
            px: 18.0,
            bold: true,
            color: DARK,
            pad: 2,
        },
    );
    s.text(
        46,
        309,
        "(b) area of responsibility.",
        Style {
            px: 18.0,
            bold: true,
            color: DARK,
            pad: 2,
        },
    );
    s.disc(410, 250, 13, [60, 90, 170]);
    s.text(
        436,
        238,
        "Action",
        Style {
            px: 24.0,
            bold: true,
            color: DARK,
            pad: 2,
        },
    );
    s.text(
        303,
        266,
        "\u{2022} Monitor, control and reduce risk",
        Style {
            px: 17.0,
            bold: false,
            color: DARK,
            pad: 2,
        },
    );
    s.text(
        303,
        289,
        "\u{2022} Escalate when needed",
        Style {
            px: 17.0,
            bold: false,
            color: DARK,
            pad: 2,
        },
    );
    s.text(
        303,
        312,
        "\u{2022} Prevent incidents from",
        Style {
            px: 17.0,
            bold: false,
            color: DARK,
            pad: 2,
        },
    );
    s.text(
        320,
        330,
        "happening again",
        Style {
            px: 17.0,
            bold: false,
            color: DARK,
            pad: 2,
        },
    );
    s.disc(682, 250, 13, [60, 90, 170]);
    s.text(
        708,
        238,
        "Report",
        Style {
            px: 24.0,
            bold: true,
            color: DARK,
            pad: 2,
        },
    );
    s.text(
        670,
        285,
        "Report risk events",
        Style {
            px: 17.0,
            bold: false,
            color: DARK,
            pad: 2,
        },
    );
    s.text(
        670,
        305,
        "and incidents",
        Style {
            px: 17.0,
            bold: false,
            color: DARK,
            pad: 2,
        },
    );

    // 底部横幅
    s.gradient((18, 380, 862, 76), [70, 30, 175], [125, 40, 200], true);
    s.disc(66, 418, 16, [230, 230, 245]);
    s.text(
        90,
        402,
        "Action Risk Management",
        Style {
            px: 26.0,
            bold: true,
            color: WHITE,
            pad: 2,
        },
    );
    s.rect((485, 392, 2, 52), [200, 190, 230]);
    s.text(
        503,
        402,
        "Identify it. Act fast. Report.",
        Style {
            px: 26.0,
            bold: false,
            color: WHITE,
            pad: 2,
        },
    );

    // 页脚：深色底小字 + 居中字
    s.rect((0, 466, 902, 34), [22, 18, 48]);
    s.rect((0, 466, 116, 34), [10, 10, 14]);
    s.text(
        10,
        468,
        "digiplus (digipl) (digipl)",
        Style {
            px: 9.0,
            bold: false,
            color: WHITE,
            pad: 1,
        },
    );
    s.text(
        10,
        481,
        "(digipl) (digipl)",
        Style {
            px: 9.0,
            bold: false,
            color: WHITE,
            pad: 1,
        },
    );
    s.text(
        396,
        474,
        "Powered by fun.",
        Style {
            px: 20.0,
            bold: false,
            color: WHITE,
            pad: 2,
        },
    );
    s
}

/// 各段译文（按该段首行原文索引）。
fn translations() -> HashMap<&'static str, &'static str> {
    HashMap::from([
        ("Business unit", "商业单位"),
        ("Risk management and", "风险管理和合规"),
        ("Internal audit", "内部审计"),
        (
            "Directly manage risk",
            "直接管理风险 开展日常活动，识别、评估、控制和减轻其所负责领域的风险。",
        ),
        (
            "Set policy",
            "制定政策 监测风险并确保内部合规，遵守规则和控制。",
        ),
        (
            "Independent assurance",
            "独立提供保证和评估 内部控制在以下方面是有效的",
        ),
        ("governance process.", "治理流程。"),
        ("What can you do", "你能够做什么"),
        ("Identify", "识别"),
        (
            "Identify the risks in your",
            "辨别你身上的风险 (b) 有关领域。",
        ),
        ("Action", "动作"),
        (
            "\u{2022} Monitor, control and reduce risk",
            "\u{2022} 监测、控制和减轻风险 \u{2022} 需要时升级 \u{2022} 防止事件再次发生",
        ),
        ("Report", "报告"),
        ("Report risk events", "报告风险事件和事件"),
        ("Action Risk Management", "行动风险管理"),
        ("Identify it. Act fast. Report.", "识别它。动作快上报。"),
        (
            "digiplus (digipl) (digipl)",
            "digiplus (digipl) (digipl) (digipl)",
        ),
        ("Powered by fun.", "乘以乐趣。"),
    ])
}

/// 整理段落并配上译文（找不到译文的段落给占位，避免场景悄悄缺段）。
pub fn translate_scene(boxes: &[OcrTextBox]) -> Vec<TranslatedParagraph> {
    let table = translations();
    paragraphs_from_boxes(boxes, LayoutMode::SmartMerge)
        .into_iter()
        .map(|(source, box_indices)| {
            let first = boxes[box_indices[0]].text.as_str();
            let translated = table
                .get(first)
                .copied()
                .unwrap_or("（缺译文）")
                .to_string();
            TranslatedParagraph {
                source,
                translated,
                box_indices,
            }
        })
        .collect()
}

/// 把 RGBA 写成 PNG。
fn save_png(path: &std::path::Path, width: u32, height: u32, rgba: Vec<u8>) {
    image::RgbaImage::from_raw(width, height, rgba)
        .expect("RGBA 缓冲长度与尺寸一致")
        .save(path)
        .expect("写 PNG");
}

/// 场景走完整原位替换：不崩、每段都画了；填充区两两不相交、全图只有填充区里的像素被改动；
/// 设了导出目录时写出前后两张 PNG 并打印耗时。
#[cfg(windows)]
#[test]
fn translate_scene_renders_and_dumps() {
    let scene = build_scene();
    let pairs = translate_scene(&scene.boxes);
    assert!(
        pairs.iter().all(|p| p.translated != "（缺译文）"),
        "{pairs:#?}"
    );
    let started = std::time::Instant::now();
    let mut img = scene.rgba.clone();
    let drawn = render_with_system_font(&mut img, scene.width, scene.height, &scene.boxes, &pairs);
    let elapsed = started.elapsed();
    assert_eq!(drawn, pairs.len());
    assert_ne!(img, scene.rgba);

    let font = SystemFont::new();
    let plans = plan_paragraphs(
        &scene.rgba,
        scene.width,
        scene.height,
        &scene.boxes,
        &pairs,
        &|c, px, bold| font.advance(c, px, bold),
        &mut |t, px, bold| font.raster(t, px, bold),
    );
    let fills: Vec<PhysicalRect> = plans
        .iter()
        .flat_map(|p| p.slots.iter().map(|s| s.fill))
        .collect();
    // 允许被改动的范围：填充区、整块抹除区、译文落笔裁剪区的并集
    let touched: Vec<PhysicalRect> = plans
        .iter()
        .flat_map(|p| {
            p.slots.iter().map(|s| {
                s.clip
                    .union_bounds(&s.fill)
                    .union_bounds(&s.erase)
                    .union_bounds(&s.box_rect)
            })
        })
        .collect();
    // 标题与正文已在送译前切成两段：各自一个排版单元，字号各自推导，译文不跨单元
    let plan_of = |source: &str| {
        let n = pairs
            .iter()
            .position(|p| p.source == source)
            .expect("段存在");
        plans.iter().find(|p| p.pair_index == n).expect("段有计划")
    };
    let (head, body) = (
        plan_of("Report"),
        plan_of("Report risk events and incidents"),
    );
    assert_eq!((head.slots.len(), body.slots.len()), (1, 2));
    assert!(head.fit.font_px > body.fit.font_px, "标题字号应大于正文");
    assert!(head.fit.lines.iter().all(|l| l.slot < 1));
    assert_eq!(head.fit.lines[0].text, "报告");
    for (a, fa) in fills.iter().enumerate() {
        for fb in &fills[a + 1..] {
            assert!(fa.intersect(fb).is_none(), "填充区重叠：{fa:?} {fb:?}");
        }
    }
    for y in 0..scene.height as i32 {
        for x in 0..scene.width as i32 {
            let i = (y as usize * scene.width as usize + x as usize) * 4;
            let inside = touched
                .iter()
                .any(|f| x >= f.x && x < f.right() && y >= f.y && y < f.bottom());
            assert!(
                inside || img[i..i + 4] == scene.rgba[i..i + 4],
                "可改动范围外的像素 ({x},{y}) 被改动"
            );
        }
    }
    dump_styles(&scene, &pairs);
    if let Ok(dir) = std::env::var(DUMP_ENV) {
        eprintln!(
            "译文原位替换耗时 {elapsed:?}（{} 段，{} 行）",
            pairs.len(),
            fills.len()
        );
        let tag = std::env::var("CISOX_TRANSLATE_TAG").unwrap_or_else(|_| "scene".into());
        let dir = std::path::PathBuf::from(dir);
        std::fs::create_dir_all(&dir).expect("建导出目录");
        save_png(
            &dir.join(format!("{tag}_original.png")),
            scene.width,
            scene.height,
            scene.rgba.clone(),
        );
        save_png(
            &dir.join(format!("{tag}_translated.png")),
            scene.width,
            scene.height,
            img,
        );
    }
}

/// 某个字号 / 字重下，GDI 画出的一串字的墨迹高度（像素）。
#[cfg(windows)]
fn gdi_ink_height(text: &str, family: &str, px: f32, bold: bool) -> i32 {
    let b = text_raster::rasterize_text(text, family, px, bold).expect("GDI");
    let rows: Vec<usize> = (0..b.height as usize)
        .filter(|&y| {
            b.coverage[y * b.width as usize..(y + 1) * b.width as usize]
                .iter()
                .any(|&c| c >= INK_COVERAGE)
        })
        .collect();
    (rows.last().unwrap_or(&0) - rows.first().unwrap_or(&0) + 1) as i32
}

/// 四种行框形状（紧贴、PP-OCR 外扩 1.5 / 2.0、系统 OCR 偏松）下，译文满行墨迹高度与原文满行墨迹高度之比都在 0.9–1.1；
/// 段落仍按紧贴行框切分（只检验字号推导）。
#[cfg(windows)]
#[test]
fn translation_ink_height_matches_original_across_box_styles() {
    let scene = build_scene();
    let pairs = translate_scene(&scene.boxes);
    let font = SystemFont::new();
    let mut checked = 0;
    for style in [
        BoxStyle::Tight,
        BoxStyle::Unclip(1.5),
        BoxStyle::Unclip(2.0),
        BoxStyle::SystemLoose,
    ] {
        let boxes = scene.restyled_boxes(style);
        let plans = plan_paragraphs(
            &scene.rgba,
            scene.width,
            scene.height,
            &boxes,
            &pairs,
            &|c, px, bold| font.advance(c, px, bold),
            &mut |t, px, bold| font.raster(t, px, bold),
        );
        for plan in &plans {
            let pair = &pairs[plan.pair_index];
            let truth = scene.truth[pair.box_indices[0]];
            // 原文太小（栅格量化误差占主导）、混合字号的段不参与
            let sizes: Vec<f32> = pair
                .box_indices
                .iter()
                .map(|&i| scene.truth[i].px)
                .collect();
            if truth.px < 14.0 || sizes.iter().any(|&p| p != truth.px) {
                continue;
            }
            let original = gdi_ink_height("Hgdy", SCENE_FONT, truth.px, truth.bold);
            let translated = gdi_ink_height(
                "国",
                "Microsoft YaHei UI",
                plan.fit.font_px as f32,
                plan.ink.bold,
            );
            let ratio = translated as f32 / original as f32;
            eprintln!(
                "{style:?} {:?} 原文 {}px 满行墨迹 {original} 译文 {}px 墨迹 {translated} 比值 {ratio:.2}",
                pair.source.chars().take(18).collect::<String>(),
                truth.px,
                plan.fit.font_px
            );
            // 字重：段内各行同粗细时，判断应与真值一致（常规 / 粗体）
            if pair
                .box_indices
                .iter()
                .all(|&i| scene.truth[i].bold == truth.bold)
            {
                assert_eq!(plan.ink.bold, truth.bold, "{style:?} {} 字重", pair.source);
            }
            assert!(
                (0.9..=1.1).contains(&ratio),
                "{style:?} {}：原文满行墨迹 {original}，译文墨迹 {translated}，比值 {ratio:.2}",
                pair.source
            );
            checked += 1;
        }
    }
    assert!(checked >= 20, "参与检验的段太少：{checked}");
}

/// 设了导出目录时，把每种行框形状下的译文图写成 PNG（`<标签>_<形状>_translated.png`）。
#[cfg(windows)]
fn dump_styles(scene: &Scene, pairs: &[TranslatedParagraph]) {
    let Ok(dir) = std::env::var(DUMP_ENV) else {
        return;
    };
    let tag = std::env::var("CISOX_TRANSLATE_TAG").unwrap_or_else(|_| "scene".into());
    for (name, style) in [
        ("unclip15", BoxStyle::Unclip(1.5)),
        ("unclip20", BoxStyle::Unclip(2.0)),
        ("system", BoxStyle::SystemLoose),
    ] {
        let mut img = scene.rgba.clone();
        let boxes = scene.restyled_boxes(style);
        render_with_system_font(&mut img, scene.width, scene.height, &boxes, pairs);
        save_png(
            &std::path::PathBuf::from(&dir).join(format!("{tag}_{name}_translated.png")),
            scene.width,
            scene.height,
            img,
        );
    }
}
