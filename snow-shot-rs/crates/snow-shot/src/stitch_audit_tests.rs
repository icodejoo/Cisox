//! 拼接库 D3 实证测试（合成帧，离屏，纯 CPU）。
//!
//! 对应 `stitch-lib-audit.md` §6 的用例清单。每条用例都会打印一行
//! `AUDIT|编号|PASS/FAIL|原始数字`，便于 `--nocapture` 汇总成表；
//! “必须逐字节拼回”的用例用断言把关，“允许拒绝但禁止错位”的用例只断言不出现错位并记录实测数字。

use crate::stitch_service::{FrameOutcome, StitchService, frame_fingerprint};
use snow_stitch_images::{
    Frame, MotionStage, PixelFormat, StitchAxis, StitchBranch, StitchDecision, StitchError,
    StitchOptions, Stitcher,
};

/// 标准帧宽。
const W: u32 = 1280;
/// 标准帧高。
const FH: u32 = 800;
/// 每像素字节数。
const BPP: usize = 4;
/// 0.3 倍帧高的步长（重叠 70%）。
const STEP_30: u32 = FH * 3 / 10;
/// 页脚 / 页眉的固定位图噪声盐。
const FIXED_SALT: u32 = 0x51ED;

/// 文档像素函数：坐标 → BGRA。
type PixelFn<'a> = &'a dyn Fn(u32, u32) -> [u8; 4];

/// 二维整数哈希（与库测试同款混合，带盐）。
fn hash(x: u32, y: u32, salt: u32) -> u32 {
    let mut h = x.wrapping_mul(0xc2b2_ae35) ^ y.wrapping_mul(0x27d4_eb2d) ^ salt.wrapping_mul(0x1656_67b1);
    h ^= h >> 16;
    h = h.wrapping_mul(0x7feb_352d);
    h ^= h >> 15;
    h
}

/// 三通道噪声像素。
fn noise_px(x: u32, y: u32, salt: u32) -> [u8; 4] {
    let h = hash(x, y, salt);
    [(h >> 24) as u8, (h >> 16) as u8, (h >> 8) as u8, 255]
}

/// 默认“文档”：三通道噪声。
fn doc(x: u32, y: u32) -> [u8; 4] {
    noise_px(x, y, 0)
}

/// 渲染文档第 `y0` 行起、高 `h`、宽 `w` 的区域（BGRA）。
fn render(w: u32, y0: u32, h: u32, f: PixelFn<'_>) -> Vec<u8> {
    let mut out = Vec::with_capacity(w as usize * h as usize * BPP);
    for y in 0..h {
        for x in 0..w {
            out.extend_from_slice(&f(x, y + y0));
        }
    }
    out
}

/// 构造一帧（Rgba8 标记，字节顺序原样）。
fn frame_of(w: u32, h: u32, bytes: Vec<u8>) -> Frame {
    Frame::new(w, h, PixelFormat::Rgba8, bytes).expect("合成帧尺寸自洽")
}

/// 直接驱动库：逐帧推入，返回拼接器与每帧决策。
fn raw_run(frames: Vec<Frame>, options: StitchOptions) -> (Stitcher, Vec<Option<StitchDecision>>) {
    let mut stitcher = Stitcher::new(options).expect("options");
    let mut decisions = Vec::new();
    for frame in frames {
        decisions.push(stitcher.push(frame).expect("push 不应报硬错误"));
    }
    (stitcher, decisions)
}

/// 记录决策的默认竖向选项。
fn opts() -> StitchOptions {
    StitchOptions {
        record_decisions: true,
        ..StitchOptions::default()
    }
}

/// 竖向按滚动位置序列生成整套帧。
fn vertical_frames(w: u32, fh: u32, scrolls: &[u32], f: PixelFn<'_>) -> Vec<Frame> {
    scrolls
        .iter()
        .map(|&s| frame_of(w, fh, render(w, s, fh, f)))
        .collect()
}

/// 决策序列的分支缩写（首帧为 F）。
fn branches(decisions: &[Option<StitchDecision>]) -> String {
    decisions
        .iter()
        .map(|d| match d {
            None => "F",
            Some(d) => match d.branch {
                StitchBranch::Append => "A",
                StitchBranch::Prepend => "P",
                StitchBranch::Contained => "C",
                StitchBranch::Skip => "S",
                StitchBranch::NoMovement => "N",
            },
        })
        .collect::<Vec<_>>()
        .join("")
}

/// 决策序列里的未推进阶段（NoMovement 时的 stage）。
fn stages(decisions: &[Option<StitchDecision>]) -> String {
    let list: Vec<String> = decisions
        .iter()
        .flatten()
        .filter(|d| d.branch == StitchBranch::NoMovement)
        .map(|d| {
            let stage = d
                .motion_diagnostics
                .as_ref()
                .map_or("none".to_string(), |m| format!("{:?}", m.stage));
            format!("{stage}/{:?}", d.motion)
        })
        .collect();
    if list.is_empty() {
        "-".into()
    } else {
        list.join(",")
    }
}

/// 取当前画布的像素与高度。
fn canvas(stitcher: &Stitcher) -> (u32, Vec<u8>) {
    let image = stitcher.image().expect("画布应存在");
    (image.height(), image.pixels().to_vec())
}

/// 打印一行审计结果并返回是否通过。
fn report(id: &str, pass: bool, detail: &str) -> bool {
    println!("AUDIT|{id}|{}|{detail}", if pass { "PASS" } else { "FAIL" });
    pass
}

/// 画布是否等于文档 `[base, base+h)` 的逐字节前缀。
fn is_doc_prefix(bytes: &[u8], h: u32, base: u32, f: PixelFn<'_>) -> bool {
    bytes == render(W, base, h, f).as_slice()
}

/// 统计画布里有多少行与给定行逐字节相同。
fn count_row(bytes: &[u8], w: u32, row: &[u8]) -> usize {
    let stride = w as usize * BPP;
    bytes.chunks_exact(stride).filter(|r| *r == row).count()
}

/// 用例 1：重复帧不追加；近似重复也不追加。
#[test]
fn audit_01_duplicate_frames() {
    let scrolls = [0, 0, 0, 300, 300];
    let (st, d) = raw_run(vertical_frames(W, FH, &scrolls, &doc), opts());
    let (h, bytes) = canvas(&st);
    let exact = is_doc_prefix(&bytes, h, 0, &doc);
    let ok = h == FH + 300 && exact && branches(&d) == "FSSAS";
    report("1", ok, &format!("branches={} height={h} expect={} bytes_equal={exact}", branches(&d), FH + 300));
    assert!(ok);

    // 近似重复：右下角 1 像素抖动
    let f0 = render(W, 0, FH, &doc);
    let mut f1 = f0.clone();
    let last = f1.len() - 4;
    f1[last] ^= 0x10;
    let (st, d) = raw_run(
        vec![frame_of(W, FH, f0), frame_of(W, FH, f1)],
        opts(),
    );
    let (h2, _) = canvas(&st);
    let ok2 = h2 == FH;
    report("1b", ok2, &format!("branches={} height={h2} stages={}", branches(&d), stages(&d)));
    assert!(ok2);
}

/// 用例 2a：重叠 70%（步长 0.3 倍帧高），12 帧全部追加并逐字节拼回。
#[test]
fn audit_02a_large_overlap() {
    let scrolls: Vec<u32> = (0..12).map(|i| i * STEP_30).collect();
    let (st, d) = raw_run(vertical_frames(W, FH, &scrolls, &doc), opts());
    let (h, bytes) = canvas(&st);
    let exact = is_doc_prefix(&bytes, h, 0, &doc);
    let offsets: Vec<i32> = d.iter().flatten().filter_map(|x| x.accepted_offset).collect();
    let all_240 = offsets.iter().all(|o| o.unsigned_abs() == STEP_30);
    let ok = exact && all_240 && h == FH + 11 * STEP_30 && branches(&d) == "FAAAAAAAAAAA";
    report("2a", ok, &format!("branches={} height={h} offsets_abs_all_{STEP_30}={all_240} bytes_equal={exact}", branches(&d)));
    assert!(ok);
}

/// 用例 2b：步长 0.5 倍帧高应全部追加；0.7 倍应被拒绝，且不产生错位拼接。
#[test]
fn audit_02b_step_limits() {
    let half = FH / 2;
    let scrolls: Vec<u32> = (0..6).map(|i| i * half).collect();
    let (st, d) = raw_run(vertical_frames(W, FH, &scrolls, &doc), opts());
    let (h, bytes) = canvas(&st);
    let exact = is_doc_prefix(&bytes, h, 0, &doc);
    let ok = exact && branches(&d) == "FAAAAA";
    report("2b-0.5", ok, &format!("branches={} height={h} bytes_equal={exact}", branches(&d)));
    assert!(ok);

    let fast = FH * 7 / 10;
    let scrolls: Vec<u32> = (0..6).map(|i| i * fast).collect();
    let (st, d) = raw_run(vertical_frames(W, FH, &scrolls, &doc), opts());
    let (h, bytes) = canvas(&st);
    let prefix = is_doc_prefix(&bytes, h, 0, &doc);
    let grew = h > FH;
    let ok = prefix;
    report("2b-0.7", ok, &format!("branches={} height={h} (frame={FH}) prefix_ok={prefix} grew={grew} stages={}", branches(&d), stages(&d)));
    assert!(ok, "0.7 倍步长不允许产生错位内容");
}

/// 用例 3：只有 G / 只有 R 通道有纹理仍能拼回（含“BGRA 直接当 RGBA 喂库”的对照与适配层修正）。
#[test]
fn audit_03_single_channel_texture() {
    let g_only = |x: u32, y: u32| {
        let n = noise_px(x, y, 0);
        [128, n[1], 128, 255]
    };
    // RGBA 顺序：下标 0 是 R
    let r_only_rgba = |x: u32, y: u32| {
        let n = noise_px(x, y, 0);
        [n[0], 128, 128, 255]
    };
    // BGRA 顺序：下标 2 是 R
    let r_only_bgra = |x: u32, y: u32| {
        let n = noise_px(x, y, 0);
        [128, 128, n[2], 255]
    };
    let scrolls: Vec<u32> = (0..8).map(|i| i * STEP_30).collect();
    let expected_h = FH + 7 * STEP_30;
    for (id, f) in [("3-G", &g_only as PixelFn<'_>), ("3-R-rgba", &r_only_rgba as PixelFn<'_>)] {
        let (st, d) = raw_run(vertical_frames(W, FH, &scrolls, f), opts());
        let (h, bytes) = canvas(&st);
        let exact = is_doc_prefix(&bytes, h, 0, f);
        let ok = exact && h == expected_h;
        report(id, ok, &format!("branches={} height={h} bytes_equal={exact} offsets={:?}", branches(&d), offsets(&d)));
        assert!(ok, "{id}");
    }
    // 对照：BGRA 直接当 RGBA 喂库，R 权重被当成 B（77 -> 29），红色纹理会被判为无特征
    let (st, d) = raw_run(vertical_frames(W, FH, &scrolls, &r_only_bgra), opts());
    let (h, _) = canvas(&st);
    report(
        "3-R-bgra-raw",
        h == expected_h,
        &format!("已知限制(库按 RGBA 计亮度): branches={} height={h} expected={expected_h}", branches(&d)),
    );
    // 适配层：push_captured 先对调 R/B，红色纹理应当能拼
    let mut svc = StitchService::new();
    for &s in &scrolls {
        svc.push_captured(snow_platform::capture::CapturedScreen {
            width: W,
            height: FH,
            data: render(W, s, FH, &r_only_bgra),
        })
        .expect("push");
    }
    let (_, _, rgba) = svc.export_all_rgba().expect("导出");
    let mut expected = render(W, 0, svc.height(), &r_only_bgra);
    crate::stitch_service::swap_red_blue(&mut expected);
    let ok = svc.height() == expected_h && rgba == expected;
    report("3-R-adapter", ok, &format!("height={} expected={expected_h} bytes_equal={}", svc.height(), rgba == expected));
    assert!(ok);
}

/// 用例 4：周期 40 行的重复条纹；允许拒绝，禁止错位（以画布高度对照期望值）。
#[test]
fn audit_04_periodic_stripes() {
    const P: u32 = 40;
    let stripes = |x: u32, y: u32| noise_px(x, y % P, 3);
    for (id, step) in [("4-multiple", STEP_30), ("4-nonmultiple", STEP_30 + 13)] {
        let scrolls: Vec<u32> = (0..8).map(|i| i * step).collect();
        let (st, d) = raw_run(vertical_frames(W, FH, &scrolls, &stripes), opts());
        let (h, bytes) = canvas(&st);
        let expected = FH + 7 * step;
        // 纯周期内容下任意高度的前缀都“像”文档，所以额外用高度判定是否拼对
        let prefix = is_doc_prefix(&bytes, h, 0, &stripes);
        let correct = h == expected;
        let ok = prefix && h <= expected;
        report(id, ok, &format!("step={step} branches={} height={h} expected={expected} correct={correct} stages={}", branches(&d), stages(&d)));
        assert!(ok, "{id}: 不允许比期望更高（多贴内容）");
    }
}

/// 用例 5：纯色 / 低纹理。
#[test]
fn audit_05_flat_content() {
    let white = |_x: u32, _y: u32| [255, 255, 255, 255];
    let (st, d) = raw_run(vertical_frames(W, FH, &[0; 5], &white), opts());
    let (h, _) = canvas(&st);
    let ok = h == FH;
    report("5a", ok, &format!("branches={} height={h} stages={}", branches(&d), stages(&d)));
    assert!(ok);

    // 全白 + 文档第 1000 行处一条 2 像素黑线，随滚动移动
    let line = |_x: u32, y: u32| if (1000..1002).contains(&y) { [0, 0, 0, 255] } else { [255, 255, 255, 255] };
    let scrolls: Vec<u32> = (0..8).map(|i| i * STEP_30).collect();
    let (st, d) = raw_run(vertical_frames(W, FH, &scrolls, &line), opts());
    let (h, bytes) = canvas(&st);
    let prefix = is_doc_prefix(&bytes, h, 0, &line);
    let expected = FH + 7 * STEP_30;
    let ok = prefix && h <= expected;
    report("5b", ok, &format!("branches={} height={h} expected={expected} prefix_ok={prefix} stages={}", branches(&d), stages(&d)));
    assert!(ok);

    // 竖向线性渐变（每行一个灰度，水平方向不变）
    let gradient = |_x: u32, y: u32| {
        let v = ((y as u64 * 255) / 8000) as u8;
        [v, v, v, 255]
    };
    let (st, d) = raw_run(vertical_frames(W, FH, &scrolls, &gradient), opts());
    let (h, bytes) = canvas(&st);
    let prefix = is_doc_prefix(&bytes, h, 0, &gradient);
    let ok = prefix && h <= expected;
    report("5c", ok, &format!("branches={} height={h} expected={expected} prefix_ok={prefix} stages={}", branches(&d), stages(&d)));
    assert!(ok);
}

/// 用例 6a：反向滚动走 Prepend 并逐字节拼回。
#[test]
fn audit_06a_reverse_scroll() {
    let scrolls = [1600, 1300, 1000, 700];
    let (st, d) = raw_run(vertical_frames(W, FH, &scrolls, &doc), opts());
    let (h, bytes) = canvas(&st);
    let exact = is_doc_prefix(&bytes, h, 700, &doc);
    let ok = exact && h == 900 + FH && branches(&d) == "FPPP";
    report("6a", ok, &format!("branches={} height={h} expect={} bytes_equal={exact}", branches(&d), 900 + FH));
    assert!(ok);
}

/// 用例 6b：往返滚动出现 Contained，最终逐字节拼回。
#[test]
fn audit_06b_round_trip() {
    let scrolls = [0, 300, 600, 300, 600, 900];
    let (st, d) = raw_run(vertical_frames(W, FH, &scrolls, &doc), opts());
    let (h, bytes) = canvas(&st);
    let exact = is_doc_prefix(&bytes, h, 0, &doc);
    let has_contained = d.iter().flatten().any(|x| x.branch == StitchBranch::Contained);
    let ok = exact && has_contained && h == 900 + FH;
    report("6b", ok, &format!("branches={} height={h} bytes_equal={exact} contained={has_contained}", branches(&d)));
    assert!(ok);
}

/// 用例 6c：横向滚动与竖向对称。
#[test]
fn audit_06c_horizontal() {
    let (fw, fh) = (800u32, 400u32);
    let step = fw * 3 / 10;
    let mut frames = Vec::new();
    for i in 0..8u32 {
        let mut bytes = Vec::with_capacity((fw * fh) as usize * BPP);
        for y in 0..fh {
            for x in 0..fw {
                bytes.extend_from_slice(&doc(x + i * step, y));
            }
        }
        frames.push(frame_of(fw, fh, bytes));
    }
    let options = StitchOptions {
        axis: StitchAxis::Horizontal,
        record_decisions: true,
        ..StitchOptions::default()
    };
    let (st, d) = raw_run(frames, options);
    let image = st.image().expect("画布");
    let total_w = fw + 7 * step;
    let mut expected = Vec::new();
    for y in 0..fh {
        for x in 0..total_w {
            expected.extend_from_slice(&doc(x, y));
        }
    }
    let exact = image.width() == total_w && image.height() == fh && image.pixels() == expected.as_slice();
    report("6c", exact, &format!("branches={} size={}x{} expect={total_w}x{fh} bytes_equal={exact}", branches(&d), image.width(), image.height()));
    assert!(exact);
}

/// 用例 7：小尺寸与尺寸变化。
#[test]
fn audit_07_small_and_mismatched_sizes() {
    // (a) 首帧 4x4
    let mut s = Stitcher::new(opts()).expect("options");
    let e = s.push(frame_of(4, 4, render(4, 0, 4, &doc))).unwrap_err();
    let a = matches!(e, StitchError::InvalidFirstGeometry { .. });
    report("7a", a, &format!("err={e}"));
    assert!(a);
    // (b) 5x5 两帧不同
    let mut s = Stitcher::new(opts()).expect("options");
    s.push(frame_of(5, 5, render(5, 0, 5, &doc))).expect("first");
    let r = s.push(frame_of(5, 5, render(5, 1, 5, &doc)));
    report("7b", r.is_ok(), &format!("result_ok={} branch={:?}", r.is_ok(), r.as_ref().ok().and_then(|d| d.as_ref().map(|d| d.branch))));
    assert!(r.is_ok());
    // (c) 第二帧尺寸不同
    let mut s = Stitcher::new(opts()).expect("options");
    s.push(frame_of(40, 40, render(40, 0, 40, &doc))).expect("first");
    let e = s.push(frame_of(41, 40, render(41, 0, 40, &doc))).unwrap_err();
    let c = matches!(e, StitchError::ViewportMismatch { .. });
    report("7c", c, &format!("err={e}"));
    assert!(c);
    // (d) 帧高 6，步长 1
    let scrolls: Vec<u32> = (0..6).collect();
    let (st, d) = raw_run(vertical_frames(32, 6, &scrolls, &doc), opts());
    let (h, _) = {
        let img = st.image().expect("画布");
        (img.height(), ())
    };
    report("7d", true, &format!("branches={} height={h} (no panic, overflow-checks on)", branches(&d)));
}

/// 固定页眉 / 页脚 / 干扰共用：在滚动内容上叠一块固定位图。
fn with_fixed(scroll_doc: PixelFn<'_>, header: u32, footer: u32) -> impl Fn(u32, u32, u32) -> [u8; 4] + '_ {
    move |x, y_in_frame, scroll| {
        if y_in_frame < header {
            noise_px(x, y_in_frame, FIXED_SALT)
        } else if y_in_frame >= FH - footer {
            noise_px(x, y_in_frame - (FH - footer), FIXED_SALT + 1)
        } else {
            scroll_doc(x, y_in_frame + scroll)
        }
    }
}

/// 用带固定区的帧序列跑一遍，返回画布高度、画布字节与决策。
fn fixed_run(header: u32, footer: u32, step: u32, count: u32) -> (u32, Vec<u8>, Vec<Option<StitchDecision>>) {
    let f = with_fixed(&doc, header, footer);
    let frames: Vec<Frame> = (0..count)
        .map(|i| {
            let s = i * step;
            let mut bytes = Vec::with_capacity((W * FH) as usize * BPP);
            for y in 0..FH {
                for x in 0..W {
                    bytes.extend_from_slice(&f(x, y, s));
                }
            }
            frame_of(W, FH, bytes)
        })
        .collect();
    let (st, d) = raw_run(frames, opts());
    let (h, bytes) = canvas(&st);
    (h, bytes, d)
}

/// 用例 8a：固定页眉（12% 帧高），步长 0.3 倍，10 帧；页眉只出现一次且正文连续。
#[test]
fn audit_08a_fixed_header() {
    let header = FH * 12 / 100;
    let (h, bytes, d) = fixed_run(header, 0, STEP_30, 10);
    let header_row0: Vec<u8> = (0..W).flat_map(|x| noise_px(x, 0, FIXED_SALT)).collect();
    let occurrences = count_row(&bytes, W, &header_row0);
    // 期望：前 header 行为页眉，之后为文档 y 行
    let mut expected = Vec::new();
    for y in 0..h {
        for x in 0..W {
            expected.extend_from_slice(&if y < header { noise_px(x, y, FIXED_SALT) } else { doc(x, y) });
        }
    }
    let exact = bytes == expected;
    let ok = occurrences == 1 && exact;
    report("8a", ok, &format!("branches={} height={h} expect={} header_occurrences={occurrences} bytes_equal={exact}", branches(&d), FH + 9 * STEP_30));
    assert!(ok);
}

/// 用例 8b：固定页脚 20% / 30% 帧高；20% 必须只出现一次，30% 记录实测。
#[test]
fn audit_08b_fixed_footer() {
    for (id, pct) in [("8b-20", 20u32), ("8b-30", 30u32)] {
        let footer = FH * pct / 100;
        let (h, bytes, d) = fixed_run(0, footer, STEP_30, 10);
        let footer_row0: Vec<u8> = (0..W).flat_map(|x| noise_px(x, 0, FIXED_SALT + 1)).collect();
        let occurrences = count_row(&bytes, W, &footer_row0);
        report(id, pct != 20 || occurrences == 1, &format!("branches={} height={h} footer_occurrences={occurrences} (expect 1)", branches(&d)));
        if pct == 20 {
            assert_eq!(occurrences, 1, "20% 页脚应只出现一次");
        }
    }
}

/// 用例 8c：右上移动光标块 + 固定 8px 滚动条不影响位移估计（比较时排除这两块）。
#[test]
fn audit_08c_cursor_and_scrollbar() {
    let scrolls: Vec<u32> = (0..10).map(|i| i * STEP_30).collect();
    let frames: Vec<Frame> = scrolls
        .iter()
        .enumerate()
        .map(|(i, &s)| {
            let mut bytes = render(W, s, FH, &doc);
            for y in 0..FH {
                for x in (W - 8)..W {
                    let at = (y * W + x) as usize * BPP;
                    bytes[at..at + 4].copy_from_slice(&[90, 90, 90, 255]);
                }
            }
            let (cx, cy) = (W - 60, 10 + i as u32 * 20);
            for y in cy..cy + 16 {
                for x in cx..cx + 16 {
                    let at = (y * W + x) as usize * BPP;
                    bytes[at..at + 4].copy_from_slice(&[255, 255, 255, 255]);
                }
            }
            frame_of(W, FH, bytes)
        })
        .collect();
    let (st, d) = raw_run(frames, opts());
    let (h, bytes) = canvas(&st);
    // 只比较左侧 0..W-70 列（避开光标与滚动条）
    let cmp_w = (W - 70) as usize;
    let expected = render(W, 0, h, &doc);
    let mut equal = true;
    for y in 0..h as usize {
        let a = &bytes[y * W as usize * BPP..y * W as usize * BPP + cmp_w * BPP];
        let b = &expected[y * W as usize * BPP..y * W as usize * BPP + cmp_w * BPP];
        if a != b {
            equal = false;
            break;
        }
    }
    let ok = equal && h == FH + 9 * STEP_30;
    report("8c", ok, &format!("branches={} height={h} expect={} masked_equal={equal}", branches(&d), FH + 9 * STEP_30));
    assert!(ok);
}

/// 用例 8d：大位移（0.6 倍）+ 高页眉（20%）；记录页眉是否被重复贴入。
#[test]
fn audit_08d_big_shift_with_header() {
    let header = FH * 20 / 100;
    let step = FH * 6 / 10;
    let (h, bytes, d) = fixed_run(header, 0, step, 6);
    let header_row0: Vec<u8> = (0..W).flat_map(|x| noise_px(x, 0, FIXED_SALT)).collect();
    let occurrences = count_row(&bytes, W, &header_row0);
    report("8d", true, &format!("branches={} height={h} expected_if_all_ok={} header_occurrences={occurrences} stages={}", branches(&d), FH + 5 * step, stages(&d)));
    assert!(bytes.len() == h as usize * W as usize * BPP);
}

/// 用例 9b：适配层高度上限生效，返回明确结果而非 OOM。
#[test]
fn audit_09b_height_cap() {
    let mut svc = StitchService::with_max_height(2000);
    let mut limit = 0;
    for i in 0..20u32 {
        let out = svc
            .push_frame(W, FH, render(W, i * STEP_30, FH, &doc))
            .expect("push");
        if matches!(out, FrameOutcome::LimitReached { .. }) {
            limit += 1;
        }
        assert!(svc.height() <= 2000);
    }
    let ok = limit > 0 && svc.height() <= 2000;
    report("9b", ok, &format!("cap=2000 final_height={} limit_hits={limit}", svc.height()));
    assert!(ok);
}

/// 用例 10a：不开决策记录时失败是静默的（证明适配层必须开）；开了则可见。
#[test]
fn audit_10a_failure_visibility() {
    let far = |i: u32| frame_of(W, FH, render(W, 7000 * i, FH, &doc));
    let mut silent = Stitcher::new(StitchOptions::default()).expect("options");
    silent.push(far(0)).expect("first");
    let silent_result = silent.push(far(1)).expect("push");
    let a = silent_result.is_none();
    report("10a-a", a, &format!("record_decisions=false push()=Ok({}) height={:?}", if a { "None" } else { "Some" }, silent.image_dimensions()));
    assert!(a);

    let (_, d) = raw_run(vec![far(0), far(1)], opts());
    let b = d[1].as_ref().is_some_and(|x| x.branch == StitchBranch::NoMovement);
    let stage = d[1].as_ref().and_then(|x| x.motion_diagnostics.as_ref()).map(|m| m.stage);
    report("10a-b", b, &format!("record_decisions=true branch={} stage={stage:?}", branches(&d)));
    assert!(b);
    assert!(matches!(
        stage,
        Some(MotionStage::NoMatches | MotionStage::LowConfidence | MotionStage::SceneCut | MotionStage::EmptyDescriptors | MotionStage::NoCandidates)
    ));

    // 适配层把它变成可见事件
    let mut svc = StitchService::new();
    svc.push_frame(W, FH, render(W, 0, FH, &doc)).expect("first");
    let out = svc.push_frame(W, FH, render(W, 7000, FH, &doc)).expect("push");
    let visible = matches!(out, FrameOutcome::Rejected(_));
    report("10a-adapter", visible, &format!("outcome={out:?}"));
    assert!(visible);
}

/// 用例 10b：中途插入内容跳变帧，之后能否恢复；结果不得错位。
#[test]
fn audit_10b_content_jump() {
    let mut scrolls = vec![0u32, 240, 480];
    let junk_index = scrolls.len();
    scrolls.extend([720, 960, 1200]);
    let mut frames = vertical_frames(W, FH, &scrolls, &doc);
    // 在第 junk_index 位置插入毫无关联的帧
    frames.insert(junk_index, frame_of(W, FH, render(W, 50_000, FH, &doc)));
    let (st, d) = raw_run(frames, opts());
    let (h, bytes) = canvas(&st);
    let prefix = is_doc_prefix(&bytes, h, 0, &doc);
    let recovered = h == FH + 1200;
    let junk_rejected = d[junk_index].as_ref().is_some_and(|x| x.branch == StitchBranch::NoMovement);
    let ok = prefix && junk_rejected;
    report("10b", ok, &format!("branches={} height={h} expected_if_recovered={} recovered={recovered} prefix_ok={prefix} junk_rejected={junk_rejected}", branches(&d), FH + 1200));
    assert!(ok);
}

/// 用例 10c：Rgb8 / Rgba8 / BGRA 标成 Rgba8 三种输入的拼接高度一致，BGRA 字节序保持。
#[test]
fn audit_10c_pixel_formats() {
    let scrolls: Vec<u32> = (0..6).map(|i| i * STEP_30).collect();
    let rgba_doc = |x: u32, y: u32| {
        let n = noise_px(x, y, 0);
        [n[2], n[1], n[0], 255]
    };
    // Rgba8（真 RGBA）
    let rgba_frames = vertical_frames(W, FH, &scrolls, &rgba_doc);
    let (st_rgba, _) = raw_run(rgba_frames, opts());
    let (h_rgba, b_rgba) = canvas(&st_rgba);
    // Rgb8
    let rgb_frames: Vec<Frame> = scrolls
        .iter()
        .map(|&s| {
            let mut bytes = Vec::with_capacity((W * FH) as usize * 3);
            for y in 0..FH {
                for x in 0..W {
                    let p = rgba_doc(x, y + s);
                    bytes.extend_from_slice(&p[..3]);
                }
            }
            Frame::new(W, FH, PixelFormat::Rgb8, bytes).expect("rgb")
        })
        .collect();
    let (st_rgb, _) = raw_run(rgb_frames, opts());
    let h_rgb = st_rgb.image().expect("画布").height();
    // BGRA（noise_px 本身就是 BGRA 顺序）标成 Rgba8
    let (st_bgra, _) = raw_run(vertical_frames(W, FH, &scrolls, &doc), opts());
    let (h_bgra, b_bgra) = canvas(&st_bgra);
    let bgra_exact = is_doc_prefix(&b_bgra, h_bgra, 0, &doc);
    let rgba_exact = is_doc_prefix(&b_rgba, h_rgba, 0, &rgba_doc);
    let ok = h_rgba == h_rgb && h_rgb == h_bgra && bgra_exact && rgba_exact;
    report("10c", ok, &format!("heights rgba={h_rgba} rgb={h_rgb} bgra={h_bgra} rgba_exact={rgba_exact} bgra_exact={bgra_exact}"));
    assert!(ok);
}

/// 用例 10d：带行距（padding）的来源与紧凑来源结果逐字节相同。
#[test]
fn audit_10d_strided_input() {
    let scrolls: Vec<u32> = (0..5).map(|i| i * STEP_30).collect();
    let pad = 16usize;
    let strided: Vec<Frame> = scrolls
        .iter()
        .map(|&s| {
            let stride = W as usize * BPP + pad;
            let mut storage = vec![0xEEu8; stride * FH as usize];
            for y in 0..FH {
                for x in 0..W {
                    let at = y as usize * stride + x as usize * BPP;
                    storage[at..at + 4].copy_from_slice(&doc(x, y + s));
                }
            }
            Frame::from_strided(W, FH, PixelFormat::Rgba8, stride, &storage).expect("strided")
        })
        .collect();
    let (st_a, _) = raw_run(strided, opts());
    let (st_b, _) = raw_run(vertical_frames(W, FH, &scrolls, &doc), opts());
    let (ha, ba) = canvas(&st_a);
    let (hb, bb) = canvas(&st_b);
    let ok = ha == hb && ba == bb;
    report("10d", ok, &format!("heights strided={ha} packed={hb} bytes_equal={}", ba == bb));
    assert!(ok);
}

/// 读取环境变量里的整数，缺省用默认值。
fn env_u32(name: &str, default: u32) -> u32 {
    std::env::var(name).ok().and_then(|v| v.parse().ok()).unwrap_or(default)
}

/// 探针 9a / 9c：长序列峰值内存与每帧耗时（`--ignored`，单独进程运行，模式由 `STITCH_PROBE_MODE` 指定）。
///
/// 模式：`stream`（适配层增量 push + 分块导出）、`finish`（库 `push` + `finish()`）、
/// `batch`（库 `stitch(&frames)` 批量入口）。
#[test]
#[ignore = "长序列性能探针，需单独进程运行"]
fn probe_09a_peak_memory_and_timing() {
    use snow_platform::process_mem::current_process_memory;
    let mode = std::env::var("STITCH_PROBE_MODE").unwrap_or_else(|_| "stream".into());
    let (w, fh) = (env_u32("STITCH_PROBE_W", 1920), env_u32("STITCH_PROBE_H", 1080));
    let frames = env_u32("STITCH_PROBE_FRAMES", 40);
    let step = fh * 3 / 10;
    let base = current_process_memory().expect("内存快照");
    let mut times_ms: Vec<f64> = Vec::new();
    let started = std::time::Instant::now();
    let digest;
    let height;
    let mut mid_peak = 0u64;
    match mode.as_str() {
        "stream" => {
            let mut svc = StitchService::new();
            for i in 0..frames {
                let px = render(w, i * step, fh, &doc);
                let t = std::time::Instant::now();
                svc.push_frame(w, fh, px).expect("push");
                times_ms.push(t.elapsed().as_secs_f64() * 1000.0);
            }
            height = svc.height();
            let mut acc = 0u64;
            mid_peak = current_process_memory().map_or(0, |m| m.peak_working_set);
            for (top, rows) in svc.export_plan() {
                let part = svc.export_rows(top, rows).expect("part");
                acc = acc.rotate_left(7) ^ frame_fingerprint(&part);
            }
            digest = acc;
        }
        "finish" => {
            let mut st = Stitcher::new(opts()).expect("options");
            for i in 0..frames {
                let f = frame_of(w, fh, render(w, i * step, fh, &doc));
                let t = std::time::Instant::now();
                st.push(f).expect("push");
                st.clear_decisions();
                times_ms.push(t.elapsed().as_secs_f64() * 1000.0);
            }
            let result = st.finish().expect("finish");
            height = result.image.height();
            digest = frame_fingerprint(result.image.pixels());
        }
        "batch" => {
            let all: Vec<Frame> = (0..frames)
                .map(|i| frame_of(w, fh, render(w, i * step, fh, &doc)))
                .collect();
            let t = std::time::Instant::now();
            let result = snow_stitch_images::stitch(&all, opts()).expect("stitch");
            times_ms.push(t.elapsed().as_secs_f64() * 1000.0);
            height = result.image.height();
            digest = frame_fingerprint(result.image.pixels());
        }
        other => panic!("未知探针模式: {other}"),
    }
    let end = current_process_memory().expect("内存快照");
    let total_ms = started.elapsed().as_secs_f64() * 1000.0;
    times_ms.sort_by(|a, b| a.total_cmp(b));
    let pct = |p: f64| times_ms.get(((times_ms.len() as f64 - 1.0) * p) as usize).copied().unwrap_or(0.0);
    let canvas_mb = (w as f64 * height as f64 * 4.0) / 1048576.0;
    let mb = |v: u64| v as f64 / 1048576.0;
    println!(
        "PROBE|mode={mode}|frames={frames}|frame={w}x{fh}|canvas={w}x{height} ({canvas_mb:.1} MiB)|threads={}|peak_after_push={:.1} MiB|peak_ws={:.1} MiB|peak_minus_base={:.1} MiB|peak_commit={:.1} MiB|ws_end={:.1} MiB|push_p50={:.1} ms|push_p95={:.1} ms|total={total_ms:.0} ms|digest={digest:016x}",
        std::env::var("RAYON_NUM_THREADS").unwrap_or_else(|_| "default".into()),
        mb(mid_peak),
        mb(end.peak_working_set),
        mb(end.peak_working_set.saturating_sub(base.peak_working_set)),
        mb(end.peak_private_bytes),
        mb(end.working_set),
        pct(0.5),
        pct(0.95),
    );
}

/// 决策序列里被接受的位移列表（诊断用）。
fn offsets(decisions: &[Option<StitchDecision>]) -> Vec<i32> {
    decisions.iter().flatten().filter_map(|d| d.accepted_offset).collect()
}
