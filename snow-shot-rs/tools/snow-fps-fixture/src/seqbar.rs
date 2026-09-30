//! 帧序号色块条：把 32 位序号画成画面顶部一行黑白色块，并提供对应的解码（供测试与分析脚本对拍）。

/// 序号位数。
pub const SEQ_BITS: usize = 32;
/// 条带高度占画面高度的分母（条高 = 高 / 16，且不小于最小值）。
pub const BAR_HEIGHT_DIVISOR: u32 = 16;
/// 条带最小高度（像素）。
pub const BAR_MIN_HEIGHT: u32 = 32;
/// 位为 1 时的 BGRA 像素（白）。
pub const PIXEL_ONE: u32 = 0x00FF_FFFF;
/// 位为 0 时的 BGRA 像素（黑）。
pub const PIXEL_ZERO: u32 = 0x0000_0000;
/// 解码阈值（灰度 0..=255，中点）。
pub const DECODE_THRESHOLD: u8 = 128;

/// 计算条带高度。
///
/// # 参数
/// - `height`：画面高度（像素）。
///
/// # 返回
/// 条带高度，不超过画面高度。
///
/// # 示例
/// ```
/// assert_eq!(snow_fps_fixture::seqbar::bar_height(1440), 90);
/// ```
pub fn bar_height(height: u32) -> u32 {
    (height / BAR_HEIGHT_DIVISOR).max(BAR_MIN_HEIGHT).min(height)
}

/// 把序号展开为 32 位（高位在前）。
///
/// # 参数
/// - `seq`：帧序号。
///
/// # 示例
/// ```
/// let bits = snow_fps_fixture::seqbar::seq_bits(1);
/// assert!(bits[31] && !bits[0]);
/// ```
pub fn seq_bits(seq: u32) -> [bool; SEQ_BITS] {
    let mut bits = [false; SEQ_BITS];
    for (i, bit) in bits.iter_mut().enumerate() {
        *bit = (seq >> (SEQ_BITS - 1 - i)) & 1 == 1;
    }
    bits
}

/// 把灰度采样（每位一个值）还原成序号。
///
/// # 参数
/// - `samples`：至少 32 个灰度采样，高位在前；不足返回 `None`。
///
/// # 返回
/// 解出的序号。
///
/// # 示例
/// ```
/// let mut s = [0u8; 32];
/// s[31] = 255;
/// assert_eq!(snow_fps_fixture::seqbar::decode_samples(&s), Some(1));
/// ```
pub fn decode_samples(samples: &[u8]) -> Option<u32> {
    if samples.len() < SEQ_BITS {
        return None;
    }
    Some(
        samples[..SEQ_BITS]
            .iter()
            .fold(0u32, |acc, &v| (acc << 1) | u32::from(v >= DECODE_THRESHOLD)),
    )
}

/// 把序号条画进 BGRA 缓冲的顶部。
///
/// # 参数
/// - `pixels`：BGRA 像素缓冲（行主序，宽 `width`，至少 `height` 行）。
/// - `width`、`height`：画面尺寸；宽度不足 32 或缓冲过小时不绘制。
/// - `seq`：帧序号。
pub fn paint_bar(pixels: &mut [u32], width: u32, height: u32, seq: u32) {
    let w = width as usize;
    let bar_h = bar_height(height) as usize;
    if w < SEQ_BITS || pixels.len() < bar_h * w {
        return;
    }
    let bits = seq_bits(seq);
    for row in 0..bar_h {
        let line = &mut pixels[row * w..(row + 1) * w];
        for (x, px) in line.iter_mut().enumerate() {
            // 每位占 w/32 像素宽；余数并入最后一位
            let idx = (x * SEQ_BITS / w).min(SEQ_BITS - 1);
            *px = if bits[idx] { PIXEL_ONE } else { PIXEL_ZERO };
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 从缓冲取条带中线，每位取中心像素灰度。
    fn read_back(pixels: &[u32], width: u32, height: u32) -> Option<u32> {
        let row = (bar_height(height) / 2) as usize;
        let w = width as usize;
        let samples: Vec<u8> = (0..SEQ_BITS)
            .map(|i| {
                let x = (i * 2 + 1) * w / (SEQ_BITS * 2);
                (pixels[row * w + x] & 0xFF) as u8
            })
            .collect();
        decode_samples(&samples)
    }

    /// 多个典型序号画出再读回应一致。
    #[test]
    fn paint_then_decode_roundtrip() {
        let (w, h) = (640u32, 360u32);
        let mut buf = vec![0x0012_3456u32; (w * h) as usize];
        for seq in [0u32, 1, 2, 255, 256, 0xDEAD_BEEF, u32::MAX, 123_456] {
            paint_bar(&mut buf, w, h, seq);
            assert_eq!(read_back(&buf, w, h), Some(seq), "seq={seq}");
        }
    }

    /// 宽度不能被 32 整除时仍可读回。
    #[test]
    fn roundtrip_with_uneven_width() {
        let (w, h) = (1001u32, 200u32);
        let mut buf = vec![0u32; (w * h) as usize];
        paint_bar(&mut buf, w, h, 0x8000_0001);
        assert_eq!(read_back(&buf, w, h), Some(0x8000_0001));
    }

    /// 条带高度：按比例并受最小/最大值约束。
    #[test]
    fn bar_height_bounds() {
        assert_eq!(bar_height(1440), 90);
        assert_eq!(bar_height(720), 45);
        assert_eq!(bar_height(100), 32);
        assert_eq!(bar_height(10), 10);
    }

    /// 采样不足 32 个或阈值边界。
    #[test]
    fn decode_boundaries() {
        assert_eq!(decode_samples(&[255; 31]), None);
        assert_eq!(decode_samples(&[127; 32]), Some(0));
        assert_eq!(decode_samples(&[128; 32]), Some(u32::MAX));
    }

    /// 窄画面或缓冲过小不绘制也不越界。
    #[test]
    fn narrow_or_small_is_ignored() {
        let mut buf = vec![7u32; 16 * 16];
        paint_bar(&mut buf, 16, 16, 5);
        assert!(buf.iter().all(|&p| p == 7));
        let mut small = vec![7u32; 10];
        paint_bar(&mut small, 640, 360, 5);
        assert!(small.iter().all(|&p| p == 7));
    }
}
