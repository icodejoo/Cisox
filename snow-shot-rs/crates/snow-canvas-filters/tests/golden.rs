//! 黄金样本对拍：用 MSVC 编译的真实 C++ 内核输出（tools/p1-reference-baselines/canvas-filters）
//! 与 Rust 实现逐字节比较。输入由固定种子 xorshift32 复现，黄金文件只存输出。

use snow_canvas_filters::pen_mask::{rasterize_capsule_segment, rasterize_capsule_segment_scalar};
use snow_canvas_filters::{
    AlphaRef, ExecutionOptions, OwnedImage, Parameters, Rect, apply, apply_masked, apply_rect,
    apply_region, blend_over_source, sampling_radius_pixels,
};
use std::collections::HashMap;
use std::fs;

/// 样本目录。
const DIR: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../../tools/p1-reference-baselines/canvas-filters"
);
/// `_fs1` 记录“与 `_fs0` 输出相同”的标记长度。
const SAME_AS_FS0: u32 = 0xFFFF_FFFF;

/// xorshift32 随机数（与 C++ 侧一致）。
struct Rng(u32);

impl Rng {
    /// 用种子构造（0 换成固定常量）。
    fn new(seed: u32) -> Self {
        Self(if seed == 0 { 0x9E37_79B9 } else { seed })
    }
    /// 下一个随机数。
    fn next(&mut self) -> u32 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 17;
        self.0 ^= self.0 << 5;
        self.0
    }
}

/// 生成合法预乘 ARGB32 图像。
fn make_image(w: i32, h: i32, seed: u32) -> OwnedImage {
    let mut image = OwnedImage::new(w, h);
    let mut rng = Rng::new(seed);
    for pixel in image.data.iter_mut() {
        let a0 = rng.next();
        let raw = (a0 & 0xff) as i32;
        let kind = (a0 >> 8) & 3;
        let a = if kind < 2 {
            255
        } else if kind == 2 {
            raw
        } else if raw & 1 != 0 {
            255
        } else {
            0
        };
        let c = rng.next();
        let r = ((c & 0xff) as i32).min(a);
        let g = (((c >> 8) & 0xff) as i32).min(a);
        let b = (((c >> 16) & 0xff) as i32).min(a);
        *pixel = ((a as u32) << 24) | ((r as u32) << 16) | ((g as u32) << 8) | b as u32;
    }
    image
}

/// 生成 Alpha8 遮罩（行距按 4 字节对齐）。
fn make_mask(w: i32, h: i32, seed: u32) -> (Vec<u8>, usize) {
    let stride = ((w as usize) + 3) & !3;
    let mut data = vec![0u8; stride * h as usize];
    let mut rng = Rng::new(seed);
    for y in 0..h as usize {
        for x in 0..w as usize {
            let v = rng.next();
            let kind = (v >> 8) & 3;
            data[y * stride + x] = match kind {
                0 => 0,
                1 => 255,
                _ => (v & 0xff) as u8,
            };
        }
    }
    (data, stride)
}

/// 像素转小端字节。
fn image_bytes(image: &OwnedImage) -> Vec<u8> {
    image.data.iter().flat_map(|p| p.to_le_bytes()).collect()
}

/// 用例行的令牌游标。
struct Tokens<'a>(std::str::SplitWhitespace<'a>);

impl Tokens<'_> {
    /// 取下一个整数。
    fn i(&mut self) -> i32 {
        self.0
            .next()
            .expect("缺少令牌")
            .parse()
            .expect("整数解析失败")
    }
    /// 取下一个无符号整数。
    fn u(&mut self) -> u32 {
        self.0
            .next()
            .expect("缺少令牌")
            .parse()
            .expect("整数解析失败")
    }
    /// 取下一个浮点数。
    fn f(&mut self) -> f64 {
        self.0
            .next()
            .expect("缺少令牌")
            .parse()
            .expect("浮点解析失败")
    }
    /// 读 "type strength block sigma radius dpr ox oy fs"。
    fn params(&mut self) -> (Parameters, ExecutionOptions) {
        let filter_type = self.u();
        let strength = self.f();
        let block = self.f();
        let sigma = self.f();
        let radius = self.f();
        let dpr = self.f();
        let ox = self.f();
        let oy = self.f();
        let fs = self.i();
        (
            Parameters {
                filter_type,
                strength,
                logical_block_size: block,
                logical_sigma: sigma,
                logical_sampling_radius: radius,
                device_pixel_ratio: dpr,
                grid_origin_x: ox,
                grid_origin_y: oy,
            },
            ExecutionOptions {
                force_scalar: fs != 0,
            },
        )
    }
}

/// 追加 i32 小端字节。
fn push_i32(out: &mut Vec<u8>, v: i32) {
    out.extend_from_slice(&v.to_le_bytes());
}

/// 用例执行结果：Rust 输出，`pen` 用例额外给出标量输出。
struct Outcome {
    out: Vec<u8>,
    pen_scalar: Option<Vec<u8>>,
}

/// 执行一行用例（对应 C++ `runCase`）。
fn run_case(line: &str) -> (String, Outcome) {
    let mut t = Tokens(line.split_whitespace());
    let name = t.0.next().unwrap().to_string();
    let kind = t.0.next().unwrap().to_string();
    let mut out = Vec::new();
    let mut pen_scalar = None;
    match kind.as_str() {
        "apply" => {
            let (w, h, seed) = (t.i(), t.i(), t.u());
            let (p, o) = t.params();
            let mut image = make_image(w, h, seed);
            apply(&mut image.as_mut(), &p, &o);
            out = image_bytes(&image);
        }
        "masked" => {
            let (w, h, seed, mask_seed) = (t.i(), t.i(), t.u(), t.u());
            let (mx, my, mw, mh) = (t.i(), t.i(), t.i(), t.i());
            let rect = Rect::new(t.i(), t.i(), t.i(), t.i());
            let (p, o) = t.params();
            let source = make_image(w, h, seed);
            let mut destination = make_image(w, h, seed.wrapping_add(1_000_003));
            let (mask_data, stride) = make_mask(mw, mh, mask_seed);
            let mask = AlphaRef {
                data: &mask_data,
                width: mw,
                height: mh,
                stride,
            };
            let ok = apply_masked(
                source.as_ref(),
                &mut destination.as_mut(),
                mask,
                mx,
                my,
                rect,
                &p,
                &o,
            );
            out.push(ok as u8);
            out.extend(image_bytes(&destination));
        }
        "rect" => {
            let (w, h, seed) = (t.i(), t.i(), t.u());
            let rect = Rect::new(t.i(), t.i(), t.i(), t.i());
            let opacity = t.f();
            let (p, o) = t.params();
            let source = make_image(w, h, seed);
            let mut destination = make_image(w, h, seed.wrapping_add(1_000_003));
            let ok = apply_rect(
                source.as_ref(),
                &mut destination.as_mut(),
                rect,
                opacity,
                &p,
                &o,
            );
            out.push(ok as u8);
            out.extend(image_bytes(&destination));
        }
        "region" => {
            let (w, h, seed, n) = (t.i(), t.i(), t.u(), t.i());
            let rects: Vec<Rect> = (0..n)
                .map(|_| Rect::new(t.i(), t.i(), t.i(), t.i()))
                .collect();
            let (p, o) = t.params();
            let source = make_image(w, h, seed);
            let mut destination = make_image(w, h, seed.wrapping_add(1_000_003));
            let ok = apply_region(source.as_ref(), &mut destination.as_mut(), &rects, &p, &o);
            out.push(ok as u8);
            out.extend(image_bytes(&destination));
        }
        "blend" => {
            let (w, h, seed) = (t.i(), t.i(), t.u());
            let opacity = t.f();
            let mut filtered = make_image(w, h, seed);
            let source = make_image(w, h, seed.wrapping_add(1_000_003));
            blend_over_source(&mut filtered.as_mut(), source.as_ref(), opacity);
            out = image_bytes(&filtered);
        }
        "plan" => {
            let p = Parameters {
                filter_type: 1,
                logical_sigma: t.f(),
                device_pixel_ratio: t.f(),
                ..Parameters::default()
            };
            let plan = snow_canvas_filters::blur::make_gaussian_blur_plan(&p);
            push_i32(&mut out, plan.reduction_factor);
            for r in plan.radii {
                push_i32(&mut out, r);
            }
            push_i32(&mut out, plan.physical_support_radius);
            push_i32(&mut out, sampling_radius_pixels(&p));
        }
        "samp" => {
            let p = Parameters {
                filter_type: t.u(),
                logical_sigma: t.f(),
                logical_sampling_radius: t.f(),
                device_pixel_ratio: t.f(),
                ..Parameters::default()
            };
            push_i32(&mut out, sampling_radius_pixels(&p));
        }
        "pen" => {
            let (size, seed, stride) = (t.i(), t.u(), t.i() as usize);
            let (bx, ex, by, ey) = (t.i(), t.i(), t.i(), t.i());
            let (ax, ay, bpx, bpy, outer) = (t.f(), t.f(), t.f(), t.f(), t.f());
            let (tl, tt) = (t.i(), t.i());
            let mut alpha = vec![0u8; stride * size as usize];
            let mut rng = Rng::new(seed);
            for v in alpha.iter_mut() {
                *v = ((rng.next() >> 16) & 0xff) as u8;
            }
            let mut scalar = alpha.clone();
            let executed = rasterize_capsule_segment(
                &mut alpha, stride, tl, tt, bx, ex, by, ey, ax, ay, bpx, bpy, outer,
            );
            assert!(executed, "本机应支持 AVX2");
            rasterize_capsule_segment_scalar(
                &mut scalar,
                stride,
                tl,
                tt,
                bx,
                ex,
                by,
                ey,
                ax,
                ay,
                bpx,
                bpy,
                outer,
            );
            out = alpha;
            pen_scalar = Some(scalar);
        }
        other => panic!("未知用例类型 {other}"),
    }
    (name, Outcome { out, pen_scalar })
}

/// 读取 golden.bin：名称 -> 输出字节（`_fs1` 标记已解析为对应 `_fs0`）。
fn load_golden() -> HashMap<String, Vec<u8>> {
    let bytes = fs::read(format!("{DIR}/golden.bin")).expect("缺少 golden.bin");
    let mut map: HashMap<String, Vec<u8>> = HashMap::new();
    let mut pos = 0usize;
    let read_u32 = |pos: &mut usize| {
        let v = u32::from_le_bytes(bytes[*pos..*pos + 4].try_into().unwrap());
        *pos += 4;
        v
    };
    while pos < bytes.len() {
        let name_len = read_u32(&mut pos) as usize;
        let name = String::from_utf8(bytes[pos..pos + name_len].to_vec()).unwrap();
        pos += name_len;
        let data_len = read_u32(&mut pos);
        let data = if data_len == SAME_AS_FS0 {
            let twin = format!(
                "{}_fs0",
                name.strip_suffix("_fs1").expect("标记记录必须以 _fs1 结尾")
            );
            map[&twin].clone()
        } else {
            let d = bytes[pos..pos + data_len as usize].to_vec();
            pos += data_len as usize;
            d
        };
        map.insert(name, data);
    }
    map
}

/// 全部用例逐字节对拍（画笔标量路径单独统计）。
#[test]
fn matches_cpp_golden_byte_for_byte() {
    let golden = load_golden();
    let cases = fs::read_to_string(format!("{DIR}/cases.txt")).expect("缺少 cases.txt");
    let mut failures = Vec::new();
    let mut pen_scalar_diffs = Vec::new();
    let mut total = 0;
    for line in cases
        .lines()
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
    {
        let (name, outcome) = run_case(line);
        total += 1;
        let expected = golden
            .get(&name)
            .unwrap_or_else(|| panic!("golden 缺少 {name}"));
        if &outcome.out != expected {
            let first = outcome.out.iter().zip(expected).position(|(a, b)| a != b);
            failures.push(format!(
                "{name}: 长度 {}/{} 首个差异字节 {:?}",
                outcome.out.len(),
                expected.len(),
                first
            ));
        }
        if let Some(scalar) = outcome.pen_scalar
            && &scalar != expected
        {
            let diff = scalar.iter().zip(expected).filter(|(a, b)| a != b).count();
            pen_scalar_diffs.push(format!("{name}: {diff} 字节不同"));
        }
    }
    println!("用例总数 {total}，失败 {}", failures.len());
    println!(
        "画笔标量路径与 C++ AVX2 主体不同的用例 {}: {:?}",
        pen_scalar_diffs.len(),
        pen_scalar_diffs
    );
    assert!(
        failures.is_empty(),
        "逐字节对拍失败:\n{}",
        failures.join("\n")
    );
}

/// 较大/极端形状图像上，AVX2 路径与标量路径逐字节一致（含遮罩/矩形/区域）。
#[test]
fn avx2_matches_scalar_on_larger_images() {
    let shapes = [(257, 131), (1000, 7), (8, 300), (65, 65)];
    for (index, &(w, h)) in shapes.iter().enumerate() {
        let seed = 900 + index as u32;
        let (mask_data, stride) = make_mask(w, h, seed + 1);
        let mask = AlphaRef {
            data: &mask_data,
            width: w,
            height: h,
            stride,
        };
        for filter_type in 0..=4u32 {
            let params = Parameters {
                filter_type,
                strength: 0.73,
                logical_block_size: 9.0,
                logical_sigma: 6.5,
                logical_sampling_radius: 2.0,
                ..Parameters::default()
            };
            let rect = Rect::new(3, 2, w - 7, h - 5);
            let run = |force_scalar: bool| {
                let opts = ExecutionOptions { force_scalar };
                let mut whole = make_image(w, h, seed);
                apply(&mut whole.as_mut(), &params, &opts);
                let source = make_image(w, h, seed);
                let mut masked = make_image(w, h, seed + 2);
                let ok = apply_masked(
                    source.as_ref(),
                    &mut masked.as_mut(),
                    mask,
                    0,
                    0,
                    rect,
                    &params,
                    &opts,
                );
                let mut in_rect = make_image(w, h, seed + 2);
                let ok_rect = apply_rect(
                    source.as_ref(),
                    &mut in_rect.as_mut(),
                    rect,
                    0.6,
                    &params,
                    &opts,
                );
                let mut in_region = make_image(w, h, seed + 2);
                let ok_region = apply_region(
                    source.as_ref(),
                    &mut in_region.as_mut(),
                    &[rect],
                    &params,
                    &opts,
                );
                (whole, masked, in_rect, in_region, ok, ok_rect, ok_region)
            };
            assert_eq!(
                run(false),
                run(true),
                "形状 {w}x{h} 类型 {filter_type} AVX2 与标量不一致"
            );
        }
    }
}
