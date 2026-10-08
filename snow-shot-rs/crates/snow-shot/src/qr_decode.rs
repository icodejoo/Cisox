//! 二维码解码：对选区 RGBA 像素做灰度化后交给纯 Rust 的 `rqrr` 识别，可一次识别多个码。
//!
//! 不依赖 GPUI，便于离屏测试；识别在调用线程同步完成（选区级图片通常是毫秒级）。

use rqrr::PreparedImage;

/// 灰度换算：BT.601 整数近似的权重分母。
const LUMA_SHIFT: u32 = 8;

/// 把 RGBA 像素解码出所有二维码内容。
///
/// # 参数
/// - `width` / `height`：图像宽高（像素）。
/// - `rgba`：行优先的 RGBA 像素，长度须为 `width * height * 4`。
///
/// # 返回
/// 按检测顺序排列的二维码文本；没有码、解码失败或尺寸与像素数不符时返回空列表。
///
/// ```ignore
/// let texts = decode_qr_codes(w, h, &rgba);
/// ```
pub fn decode_qr_codes(width: u32, height: u32, rgba: &[u8]) -> Vec<String> {
    let (w, h) = (width as usize, height as usize);
    if w == 0 || h == 0 || rgba.len() != w * h * 4 {
        return Vec::new();
    }
    let mut prepared = PreparedImage::prepare_from_greyscale(w, h, |x, y| {
        let i = (y * w + x) * 4;
        let luma =
            77 * u32::from(rgba[i]) + 150 * u32::from(rgba[i + 1]) + 29 * u32::from(rgba[i + 2]);
        (luma >> LUMA_SHIFT) as u8
    });
    prepared
        .detect_grids()
        .into_iter()
        .filter_map(|grid| grid.decode().ok().map(|(_, content)| content))
        .collect()
}

/// 测试共用的二维码样本（也供覆盖窗接线测试使用）。
#[cfg(test)]
pub(crate) mod test_support {
    /// 固定样本：由独立编码器生成的 25x25 码（版本 2、纠错 M），内容见 [`SAMPLE_TEXT`]。
    const SAMPLE_ROWS: [&str; 25] = [
        "#######.....##..#.#######",
        "#.....#...#.#####.#.....#",
        "#.###.#.##..#.#...#.###.#",
        "#.###.#.#.##.###..#.###.#",
        "#.###.#.#..###..#.#.###.#",
        "#.....#.#..#..##..#.....#",
        "#######.#.#.#.#.#.#######",
        "........#..##.#.#........",
        "#.#####..#...#....#####..",
        "#.####.##...##...#.#...#.",
        ".#.##.#..########..#.#.##",
        "..#.#...##.#..###.##....#",
        "##.#.######.####.##.#.###",
        "#...##.#..#.#...#..#.#.#.",
        "#...####....##.#..####.##",
        "#..#.#...##.#.#######...#",
        "#.#.#.#.....###.#####.#..",
        "........#.#.##.##...##...",
        "#######..##.#...#.#.#.###",
        "#.....#.##..#.#.#...##.##",
        "#.###.#.####.########.#..",
        "#.###.#.#..#..##.##.#####",
        "#.###.#.##...#.#.....##.#",
        "#.....#..#.#..#.##.###..#",
        "#######.#######..########",
    ];
    /// 样本二维码的内容。
    pub(crate) const SAMPLE_TEXT: &str = "https://example.com/cisox";

    /// 把样本矩阵渲染成带静区的 RGBA 图（每模块 `scale` 像素，黑码白底）。
    pub(crate) fn render_sample(scale: usize, quiet: usize) -> (u32, u32, Vec<u8>) {
        let side = (SAMPLE_ROWS.len() + quiet * 2) * scale;
        let mut rgba = vec![255u8; side * side * 4];
        for (r, row) in SAMPLE_ROWS.iter().enumerate() {
            for (c, ch) in row.chars().enumerate() {
                if ch != '#' {
                    continue;
                }
                for dy in 0..scale {
                    for dx in 0..scale {
                        let (x, y) = ((c + quiet) * scale + dx, (r + quiet) * scale + dy);
                        let i = (y * side + x) * 4;
                        rgba[i..i + 3].fill(0);
                    }
                }
            }
        }
        (side as u32, side as u32, rgba)
    }
}

#[cfg(test)]
mod tests {
    use super::test_support::{SAMPLE_TEXT, render_sample};
    use super::*;

    /// 标准样本能解出原文。
    #[test]
    fn decodes_sample() {
        let (w, h, rgba) = render_sample(6, 4);
        assert_eq!(decode_qr_codes(w, h, &rgba), vec![SAMPLE_TEXT.to_string()]);
    }

    /// 不同缩放比例都能解出。
    #[test]
    fn decodes_at_other_scales() {
        let (w, h, rgba) = render_sample(3, 4);
        assert_eq!(decode_qr_codes(w, h, &rgba), vec![SAMPLE_TEXT.to_string()]);
    }

    /// 纯白图没有码。
    #[test]
    fn blank_image_has_no_code() {
        assert!(decode_qr_codes(64, 64, &vec![255u8; 64 * 64 * 4]).is_empty());
    }

    /// 像素数与尺寸不符、尺寸为零时返回空列表而不 panic。
    #[test]
    fn invalid_dimensions_are_empty() {
        assert!(decode_qr_codes(10, 10, &[0u8; 8]).is_empty());
        assert!(decode_qr_codes(0, 0, &[]).is_empty());
    }
}
