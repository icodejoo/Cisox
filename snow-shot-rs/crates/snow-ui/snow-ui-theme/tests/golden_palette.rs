//! 对拍测试：与 C++ 黄金样本（tools/p1-reference-baselines/palette-gen/golden_dump.cpp 的输出）逐值比对。
//!
//! 黄金样本由 MSVC 编译基线目录中的 `fast_color_lite.cpp` + `palette_generate.cpp` 生成，
//! 所有比较均为**精确相等**（整数通道/十六进制字符串，无浮点容差）。

use snow_ui_theme::fast_color::FastColor;
use snow_ui_theme::palette::{generate_palette, map_palette};

/// C++ 黄金样本全文。
const GOLDEN: &str = include_str!("golden/palette_golden.txt");

/// 取出以 `prefix ` 开头的行，并去掉前缀后按空白切分。
fn rows(prefix: &str) -> Vec<Vec<&'static str>> {
    GOLDEN
        .lines()
        .filter_map(|l| l.strip_prefix(prefix).and_then(|r| r.strip_prefix(' ')))
        .map(|r| r.split_whitespace().collect())
        .collect()
}

/// 色板：亮色、暗色（#141414 背景）、暗色（#000000 背景）三种共 16 个基色全部一致。
#[test]
fn palette_matches_cpp() {
    let rows = rows("P");
    assert_eq!(rows.len(), 16 * 3, "黄金样本行数不符");
    for row in rows {
        let (base, mode, expect) = (row[0], row[1], &row[2..]);
        let actual = match mode {
            "light" => generate_palette(base, false, ""),
            "dark" => generate_palette(base, true, "#141414"),
            "dark000" => generate_palette(base, true, "#000000"),
            other => panic!("未知模式 {other}"),
        };
        assert_eq!(actual, expect, "色板不一致：{base} {mode}");
    }
}

/// 无效输入回退到默认蓝 #1677ff 的色板。
#[test]
fn invalid_input_falls_back() {
    let rows = rows("PBAD");
    assert_eq!(rows.len(), 3);
    for row in rows {
        let input = if row[0] == "<empty>" { "" } else { row[0] };
        assert_eq!(
            generate_palette(input, false, ""),
            &row[1..],
            "输入 {input:?}"
        );
    }
}

/// darken / lighten 与 C++ 一致（含把 HSV 饱和度当 HSL 饱和度的特殊实现）。
#[test]
fn darken_lighten_match_cpp() {
    let rows = rows("D");
    assert_eq!(rows.len(), 16 * 7);
    for row in rows {
        let base = FastColor::parse(row[0]);
        let amount: f64 = row[1].parse().unwrap();
        assert_eq!(
            base.darken(amount).to_hex_string(),
            row[2],
            "darken {row:?}"
        );
        assert_eq!(
            base.lighten(amount).to_hex_string(),
            row[3],
            "lighten {row:?}"
        );
    }
}

/// mix 与 C++ 一致。
#[test]
fn mix_matches_cpp() {
    let rows = rows("X");
    assert_eq!(rows.len(), 16 * 3 * 5);
    for row in rows {
        let a = FastColor::parse(row[0]);
        let b = FastColor::parse(row[1]);
        let amount: f64 = row[2].parse().unwrap();
        assert_eq!(a.mix(&b, amount).to_hex_string(), row[3], "mix {row:?}");
    }
}

/// 颜色字符串解析（hex / rgb / rgba / 百分比 / 非法输入）与 C++ 一致。
#[test]
fn parse_matches_cpp() {
    let mut count = 0;
    for line in GOLDEN.lines().filter(|l| l.starts_with("R [")) {
        let close = line.rfind("] ").expect("样本格式");
        let input = &line[3..close];
        let rest: Vec<&str> = line[close + 2..].split_whitespace().collect();
        let c = FastColor::parse(input);
        if rest[0] == "0" {
            assert!(!c.is_valid(), "应无效：{input:?}");
        } else {
            assert!(c.is_valid(), "应有效：{input:?}");
            assert_eq!(c.to_hex_string(), rest[1], "hex：{input:?}");
            assert_eq!(c.to_rgb_string(), rest[2], "rgb：{input:?}");
        }
        count += 1;
    }
    assert_eq!(count, 20);
}

/// 色阶映射表与 theme_color_utils.cpp 一致（亮色/暗色）。
#[test]
fn map_palette_tables() {
    let colors: Vec<String> = (0..10).map(|i| format!("c{i}")).collect();
    let light = map_palette(&colors, false);
    let dark = map_palette(&colors, true);
    assert_eq!(light[0], "");
    assert_eq!(dark[0], "");
    let light_idx = [0, 1, 2, 3, 4, 5, 6, 4, 5, 6];
    let dark_idx = [0, 1, 2, 3, 6, 5, 4, 6, 5, 4];
    for slot in 1..=10 {
        assert_eq!(light[slot], format!("c{}", light_idx[slot - 1]));
        assert_eq!(dark[slot], format!("c{}", dark_idx[slot - 1]));
    }
    assert!(
        map_palette(&colors[..6], false)
            .iter()
            .all(String::is_empty)
    );
}
