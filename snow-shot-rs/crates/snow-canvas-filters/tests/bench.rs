//! 4K 性能记录（默认忽略）：`cargo test -p snow-canvas-filters --release --test bench -- --ignored --nocapture`

use snow_canvas_filters::{
    ExecutionOptions, FILTER_BLUR, FILTER_EMBOSS, FILTER_GRAYSCALE, FILTER_INVERT, FILTER_MOSAIC,
    OwnedImage, Parameters, apply,
};
use std::time::Instant;

/// 生成确定性的预乘测试图。
fn image(w: i32, h: i32) -> OwnedImage {
    let mut img = OwnedImage::new(w, h);
    let mut s = 7u32;
    for p in img.data.iter_mut() {
        s ^= s << 13;
        s ^= s >> 17;
        s ^= s << 5;
        let a = 128 + (s & 0x7f);
        *p = (a << 24)
            | ((s >> 8) & 0xff).min(a) << 16
            | ((s >> 16) & 0xff).min(a) << 8
            | (s >> 24).min(a);
    }
    img
}

/// 4K 单线程耗时（标量 / AVX2，取 3 次最小值），输出 CSV 行。
#[test]
#[ignore]
fn bench_4k() {
    let (w, h) = (3840, 2160);
    let base = image(w, h);
    let items: [(&str, Parameters); 7] = [
        (
            "mosaic_b16",
            Parameters {
                filter_type: FILTER_MOSAIC,
                logical_block_size: 16.0,
                ..Parameters::default()
            },
        ),
        (
            "blur_s2",
            Parameters {
                filter_type: FILTER_BLUR,
                logical_sigma: 2.0,
                ..Parameters::default()
            },
        ),
        (
            "blur_s8",
            Parameters {
                filter_type: FILTER_BLUR,
                logical_sigma: 8.0,
                ..Parameters::default()
            },
        ),
        (
            "blur_s32",
            Parameters {
                filter_type: FILTER_BLUR,
                logical_sigma: 32.0,
                ..Parameters::default()
            },
        ),
        (
            "grayscale",
            Parameters {
                filter_type: FILTER_GRAYSCALE,
                ..Parameters::default()
            },
        ),
        (
            "invert",
            Parameters {
                filter_type: FILTER_INVERT,
                ..Parameters::default()
            },
        ),
        (
            "emboss_r1",
            Parameters {
                filter_type: FILTER_EMBOSS,
                logical_sampling_radius: 1.0,
                ..Parameters::default()
            },
        ),
    ];
    for (name, params) in items {
        for force_scalar in [true, false] {
            let mut best = f64::MAX;
            for _ in 0..3 {
                let mut img = base.clone();
                let start = Instant::now();
                apply(
                    &mut img.as_mut(),
                    &params,
                    &ExecutionOptions { force_scalar },
                );
                best = best.min(start.elapsed().as_secs_f64() * 1000.0);
            }
            println!(
                "bench,{name},{},{best:.3}",
                if force_scalar { "scalar" } else { "avx2" }
            );
        }
    }
}
