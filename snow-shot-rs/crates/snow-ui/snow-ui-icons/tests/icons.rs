//! snow-ui-icons 集成测试：资源完整性、模型规则、光栅化、缓存、回归基线。
//!
//! 注意：本机无 Qt 环境，缺少 C++ 可执行黄金样本；回归基线来自本 crate 自身
//! （resvg 0.45.1 + tiny-skia 0.11.4）的渲染输出，用于防退化，不代表已与 C++ 逐像素对照。

use std::collections::BTreeSet;
use std::fs;
use std::path::PathBuf;

use snow_ui_icons::{
    IconColors, IconFit, IconPalette, IconRef, IconRenderer, IconRequest, IconTheme, Rgba,
    icon_count, icon_names, mask_layers, template_svg,
};

/// 期望的模板数量（三主题合计）。
const EXPECTED_TEMPLATES: usize = 829;
/// 期望的资源文件总数（icons/ 原始 + templates/ 模板）。
const EXPECTED_FILES: usize = 1658;

/// ant_design_qt 图标资源根目录。
fn resource_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../../../ant_design_qt/packages/ant_design_icons_qt/resources")
}

/// 读取某目录下全部 svg 的 (名称, 内容)。
fn read_dir_svgs(sub: &str, theme: IconTheme) -> Vec<(String, String)> {
    let dir = resource_root().join(sub).join(theme.dir());
    let mut v: Vec<_> = fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.extension().is_some_and(|x| x == "svg"))
        .map(|p| {
            (
                p.file_stem().unwrap().to_string_lossy().into_owned(),
                fs::read_to_string(&p).unwrap(),
            )
        })
        .collect();
    v.sort();
    v
}

/// 解析 C++ 生成的 antd_icons.cpp，取出其中规范化后的 SVG：(variant, name) -> 文本。
fn cpp_golden_svgs() -> std::collections::BTreeMap<(String, String), String> {
    let path = resource_root().join("../src/antd_icons.cpp");
    let text = fs::read_to_string(path).unwrap();
    let head = "{std::string_view(\"antd\"),";
    let mut map = std::collections::BTreeMap::new();
    let mut pos = 0;
    while let Some(i) = text[pos..].find(head) {
        let mut cur = pos + i + head.len();
        // 取下一个 `std::string_view("...")` 的字符串内容（C++ 生成物会随行宽折行）
        let take = |cur: &mut usize| {
            let start =
                text[*cur..].find("std::string_view(\"").unwrap() + "std::string_view(\"".len();
            let n = text[*cur + start..].find("\")").unwrap();
            let v = text[*cur + start..*cur + start + n].to_owned();
            *cur += start + n + 2;
            v
        };
        let variant = take(&mut cur);
        let name = take(&mut cur);
        let lit = text[cur..].find("std::string_view(").unwrap() + "std::string_view(".len();
        cur += lit;
        let mut svg = String::new();
        loop {
            let rest = text[cur..].trim_start();
            cur = text.len() - rest.len();
            if !rest.starts_with("R\"") {
                break;
            }
            let open = rest.find('(').unwrap();
            let delim = &rest[2..open];
            let close = format!("){delim}\"");
            let end = rest.find(&close).unwrap();
            svg.push_str(&rest[open + 1..end]);
            cur += end + close.len();
        }
        map.insert((variant, name), svg);
        pos = cur;
    }
    map
}

/// 规范化后的模板必须与 C++ 生成器产物（antd_icons.cpp）逐字节一致，共 829 条。
#[test]
fn normalized_templates_match_cpp_golden() {
    let golden = cpp_golden_svgs();
    assert_eq!(golden.len(), EXPECTED_TEMPLATES);
    for theme in IconTheme::ALL {
        for name in icon_names(theme) {
            let g = golden
                .get(&(theme.dir().to_owned(), name.to_owned()))
                .unwrap_or_else(|| panic!("C++ 缺少 {}/{name}", theme.dir()));
            assert_eq!(
                template_svg(theme, name).unwrap(),
                g,
                "{}/{name} 与 C++ 产物不一致",
                theme.dir()
            );
        }
    }
}

/// FNV-1a 64 位哈希，用于回归基线（避免引入哈希依赖）。
fn fnv1a(data: &[u8]) -> u64 {
    data.iter().fold(0xcbf29ce484222325u64, |h, b| {
        (h ^ *b as u64).wrapping_mul(0x100000001b3)
    })
}

/// 资源数量、名称集合、字节内容都应与 ant_design_qt 原目录一致。
#[test]
fn resources_match_source_dirs() {
    assert_eq!(icon_count(), EXPECTED_TEMPLATES);
    let mut total_files = 0;
    for theme in IconTheme::ALL {
        let templates = read_dir_svgs("templates", theme);
        let raw = read_dir_svgs("icons", theme);
        total_files += templates.len() + raw.len();
        let a: BTreeSet<_> = templates.iter().map(|(n, _)| n.clone()).collect();
        let b: BTreeSet<_> = raw.iter().map(|(n, _)| n.clone()).collect();
        assert_eq!(
            a,
            b,
            "{} 主题 icons 与 templates 名称集合不一致",
            theme.dir()
        );
        let embedded: Vec<_> = icon_names(theme).collect();
        assert_eq!(embedded.len(), templates.len());
        for (name, content) in &templates {
            let bmp = IconRenderer::new()
                .render(
                    &IconRef::new(theme, name.as_str()),
                    &IconRequest::square(8, 1.0),
                )
                .unwrap();
            assert!(!bmp.is_fallback, "{}/{name} 解析失败", theme.dir());
            assert!(!content.is_empty());
        }
    }
    assert_eq!(total_files, EXPECTED_FILES);
}

/// 全部图标在 24px 下都能解析并产生可见像素。
#[test]
fn every_icon_renders_visible_pixels() {
    let r = IconRenderer::with_limits(0, 0, 0);
    for theme in IconTheme::ALL {
        for name in icon_names(theme) {
            let b = r
                .render(&IconRef::new(theme, name), &IconRequest::square(24, 1.0))
                .unwrap();
            assert!(!b.is_fallback, "{}/{name}", theme.dir());
            assert!(
                b.data.chunks_exact(4).any(|p| p[3] > 0),
                "{}/{name} 全透明",
                theme.dir()
            );
        }
    }
}

/// 不存在的图标降级为占位图标，不 panic。
#[test]
fn missing_icon_falls_back() {
    let r = IconRenderer::new();
    let icon = IconRef::new(IconTheme::Outlined, "definitely-not-exist");
    assert!(!icon.exists());
    let b = r.render(&icon, &IconRequest::square(24, 1.0)).unwrap();
    assert!(b.is_fallback);
    assert!(b.data.chunks_exact(4).any(|p| p[3] > 0));
    assert!(IconRef::from_path("bogus/setting").is_none());
    assert_eq!(
        IconRef::from_path("filled/camera").unwrap().theme,
        IconTheme::Filled
    );
}

/// 颜色解析规则与 C++ 一致。
#[test]
fn color_resolution_rules() {
    let p = IconPalette::default();
    let none = IconColors::default();
    assert_eq!(p.resolve(IconTheme::Outlined, &none, false).primary, p.text);
    assert_eq!(
        p.resolve(IconTheme::TwoTone, &none, false).primary,
        p.primary
    );
    assert_eq!(
        p.resolve(IconTheme::TwoTone, &none, false).secondary,
        p.two_tone_secondary
    );
    assert_eq!(
        p.resolve(IconTheme::Filled, &none, true).primary,
        p.text_disabled
    );
    // 仅覆盖主色时，双色图标副色由主色派生（更浅、更淡）
    let red = Rgba::rgb(0xD4, 0x38, 0x0D);
    let r = p.resolve(IconTheme::TwoTone, &IconColors::primary(red), false);
    assert_eq!(r.primary, red);
    assert_ne!(r.secondary, p.two_tone_secondary);
    assert!(
        r.secondary.r > 0xE0 && r.secondary.g > 0xD0 && r.secondary.b > 0xD0,
        "{:?}",
        r.secondary
    );
    // 显式副色优先
    let sec = Rgba::rgb(1, 2, 3);
    assert_eq!(
        p.resolve(IconTheme::TwoTone, &IconColors::two_tone(red, sec), false)
            .secondary,
        sec
    );
    assert_eq!(
        Rgba::from_hex("#1677ff80"),
        Some(Rgba::new(0x16, 0x77, 0xFF, 0x80))
    );
    assert_eq!(Rgba::from_hex("1677ff"), None);
    assert_eq!(Rgba::rgb(0x16, 0x77, 0xFF).hex_rgb(), "#1677ff");
}

/// DPR、非法尺寸与超大尺寸的行为。
#[test]
fn size_and_dpr_rules() {
    let r = IconRenderer::new();
    let icon = IconRef::new(IconTheme::Outlined, "setting");
    let b = r.render(&icon, &IconRequest::square(24, 1.5)).unwrap();
    assert_eq!((b.width, b.height), (36, 36));
    assert_eq!(b.data.len(), 36 * 36 * 4);
    let z = r.render(&icon, &IconRequest::square(0, 1.0)).unwrap();
    assert_eq!((z.width, z.height), (16, 16));
    let clamp = r.render(&icon, &IconRequest::square(10, 100.0)).unwrap();
    assert_eq!(clamp.width, 80);
    let nan = r.render(&icon, &IconRequest::square(10, f32::NAN)).unwrap();
    assert_eq!(nan.width, 10);
    assert!(r.render(&icon, &IconRequest::square(20000, 1.0)).is_none());
    let mut wide = IconRequest::square(20, 1.0);
    wide.width = 40;
    wide.fit = IconFit::Stretch;
    assert_eq!(r.render(&icon, &wide).unwrap().width, 40);
}

/// 缓存命中、按颜色/尺寸分键、单色不因副色重复、LRU 淘汰。
#[test]
fn cache_behaviour() {
    let r = IconRenderer::new();
    let mono = IconRef::new(IconTheme::Filled, "camera");
    let req = IconRequest::square(24, 1.0);
    r.render(&mono, &req).unwrap();
    r.render(&mono, &req).unwrap();
    assert_eq!((r.stats().hits, r.stats().misses), (1, 1));
    // 单色图标的副色覆盖不产生新条目
    let with_sec = mono
        .clone()
        .with_colors(IconColors::default().with_secondary(Rgba::rgb(9, 9, 9)));
    r.render(&with_sec, &req).unwrap();
    assert_eq!(r.stats().hits, 2);
    // 主色不同 / 尺寸不同 → 新条目
    let red = mono
        .clone()
        .with_colors(IconColors::primary(Rgba::rgb(255, 0, 0)));
    r.render(&red, &req).unwrap();
    r.render(&mono, &IconRequest::square(32, 1.0)).unwrap();
    assert_eq!(r.stats().entries, 3);

    let small = IconRenderer::with_limits(u64::MAX, 2, u64::MAX);
    for n in ["camera", "heart", "x"] {
        small
            .render(&IconRef::new(IconTheme::Filled, n), &req)
            .unwrap();
    }
    assert_eq!(small.stats().entries, 2);
    assert_eq!(small.stats().evictions, 1);
    // 超过单张上限的不入缓存
    let tiny = IconRenderer::with_limits(u64::MAX, 8, 16);
    tiny.render(&mono, &req).unwrap();
    assert_eq!(tiny.stats().entries, 0);
    // 换调色板会清空缓存
    r.set_palette(IconPalette {
        text: Rgba::rgb(0, 0, 0),
        ..IconPalette::default()
    });
    assert_eq!(r.stats().entries, 0);
}

/// 单色主色带透明度：整体乘 alpha，与 C++ DestinationIn 一致。
#[test]
fn mono_alpha_applies() {
    let r = IconRenderer::new();
    let solid = IconRef::new(IconTheme::Filled, "camera")
        .with_colors(IconColors::primary(Rgba::rgb(255, 0, 0)));
    let half = IconRef::new(IconTheme::Filled, "camera")
        .with_colors(IconColors::primary(Rgba::new(255, 0, 0, 128)));
    let req = IconRequest::square(32, 1.0);
    let a = r.render(&solid, &req).unwrap();
    let b = r.render(&half, &req).unwrap();
    let ia = a.data.chunks_exact(4).map(|p| p[3] as u32).max().unwrap();
    let ib = b.data.chunks_exact(4).map(|p| p[3] as u32).max().unwrap();
    assert_eq!(ia, 255);
    assert!((ib as i32 - 128).abs() <= 1, "半透明 alpha 峰值 {ib}");
}

/// 双色图标：主色与副色应分别落在预期颜色上。
#[test]
fn two_tone_uses_both_colors() {
    let r = IconRenderer::new();
    let icon = IconRef::new(IconTheme::TwoTone, "bell").with_colors(IconColors::two_tone(
        Rgba::rgb(255, 0, 0),
        Rgba::rgb(0, 0, 255),
    ));
    let b = r.render(&icon, &IconRequest::square(64, 1.0)).unwrap();
    let opaque = |f: fn(&[u8]) -> bool| b.data.chunks_exact(4).any(|p| p[3] == 255 && f(p));
    assert!(
        opaque(|p| p[0] == 255 && p[1] == 0 && p[2] == 0),
        "缺少主色像素"
    );
    assert!(
        opaque(|p| p[0] == 0 && p[1] == 0 && p[2] == 255),
        "缺少副色像素"
    );
}

/// 分层：双色图标拆出两层，单色图标只有主色层，缺失返回 None。
#[test]
fn mask_layers_split() {
    let mut two = 0;
    for name in icon_names(IconTheme::TwoTone) {
        let l = mask_layers(IconTheme::TwoTone, name).unwrap();
        let sec = l.secondary.expect("双色图标应有副色层");
        assert!(
            l.primary.contains("<path") && sec.contains("<path"),
            "{name}"
        );
        assert!(!l.primary.contains("__ADQT") && !sec.contains("__ADQT"));
        two += 1;
    }
    assert_eq!(two, 150);
    assert!(
        mask_layers(IconTheme::Outlined, "setting")
            .unwrap()
            .secondary
            .is_none()
    );
    assert!(mask_layers(IconTheme::Outlined, "nope").is_none());
}

/// 回归基线条目：(路径, 逻辑边长, dpr, 覆盖色, 预乘 RGBA 的 FNV-1a 哈希)。
type Baseline = (&'static str, u32, f32, IconColors, u64);

/// 基线用例覆盖 filled/outlined/twotone、单色/双色/自定义色/2x DPR。
fn baseline_cases() -> Vec<Baseline> {
    let d = IconColors::default();
    let red = IconColors::primary(Rgba::rgb(0xD4, 0x38, 0x0D));
    let duo = IconColors::two_tone(Rgba::rgb(0x13, 0xC2, 0xC2), Rgba::rgb(0xE6, 0xFF, 0xFB));
    vec![
        ("filled/camera", 32, 1.0, d, 0),
        ("filled/heart", 32, 1.0, red, 0),
        ("filled/twitch", 32, 1.0, d, 0),
        ("filled/x", 24, 2.0, d, 0),
        ("outlined/setting", 32, 1.0, d, 0),
        ("outlined/aim", 32, 1.0, red, 0),
        ("outlined/account-book", 24, 2.0, d, 0),
        ("outlined/search", 16, 1.0, d, 0),
        ("twotone/bell", 32, 1.0, d, 0),
        ("twotone/setting", 32, 1.0, red, 0),
        ("twotone/api", 48, 1.0, duo, 0),
        ("twotone/warning", 24, 2.0, d, 0),
    ]
}

/// 渲染一条基线用例并返回哈希与位图。
fn run_baseline(c: &Baseline) -> (u64, snow_ui_icons::IconBitmap) {
    let icon = IconRef::from_path(c.0).unwrap().with_colors(c.3);
    let b = IconRenderer::new()
        .render(&icon, &IconRequest::square(c.1, c.2))
        .unwrap();
    (fnv1a(&b.data), b)
}

/// 辅助：打印当前哈希，用于更新基线（`cargo test -- --ignored --nocapture`）。
#[test]
#[ignore]
fn print_baselines() {
    for c in baseline_cases() {
        println!("{} => {:#018x}", c.0, run_baseline(&c).0);
    }
}

/// 基线回归：渲染输出的哈希不得变化，且必须有可见像素、非降级。
#[test]
fn regression_baselines() {
    for c in baseline_cases() {
        let (h, b) = run_baseline(&c);
        assert!(!b.is_fallback, "{}", c.0);
        assert!(b.data.chunks_exact(4).any(|p| p[3] > 0), "{}", c.0);
        assert_eq!(
            h,
            BASELINE_HASHES.iter().find(|(p, _)| *p == c.0).unwrap().1,
            "{} 渲染漂移",
            c.0
        );
    }
}

/// 基线哈希表（由 print_baselines 生成；渲染库升级导致漂移时需人工确认后重新生成）。
const BASELINE_HASHES: &[(&str, u64)] = &[
    ("filled/camera", 0xe542fcf8cc6f596f),
    ("filled/heart", 0x2411f3a38c71dade),
    ("filled/twitch", 0xd9c698b3f255de0d),
    ("filled/x", 0xe99aa2c4c5c259e1),
    ("outlined/setting", 0x2a6d1ca4e3897c37),
    ("outlined/aim", 0x91e0c9d3e265a1e4),
    ("outlined/account-book", 0xf41523f1eeb4f235),
    ("outlined/search", 0x72ee46521073d5cf),
    ("twotone/bell", 0x6f3d2321456a3726),
    ("twotone/setting", 0x184140e68bb95cab),
    ("twotone/api", 0xc35ea08a3cb03a98),
    ("twotone/warning", 0x93c6f67ab5e658f6),
];
