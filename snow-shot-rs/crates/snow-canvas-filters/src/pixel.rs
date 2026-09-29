//! 像素与数值辅助：QRgb(0xAARRGGBB, 预乘) 通道存取、混合与强度归一化。

/// 取 alpha 通道。
#[inline]
pub fn q_alpha(p: u32) -> i32 {
    ((p >> 24) & 0xff) as i32
}

/// 取红色通道。
#[inline]
pub fn q_red(p: u32) -> i32 {
    ((p >> 16) & 0xff) as i32
}

/// 取绿色通道。
#[inline]
pub fn q_green(p: u32) -> i32 {
    ((p >> 8) & 0xff) as i32
}

/// 取蓝色通道。
#[inline]
pub fn q_blue(p: u32) -> i32 {
    (p & 0xff) as i32
}

/// 由通道组装像素，各通道仅保留低 8 位（与 Qt `qRgba` 一致）。
#[inline]
pub fn q_rgba(r: i32, g: i32, b: i32, a: i32) -> u32 {
    (((a & 0xff) as u32) << 24)
        | (((r & 0xff) as u32) << 16)
        | (((g & 0xff) as u32) << 8)
        | ((b & 0xff) as u32)
}

/// 与 Qt `qRound(double)` 一致：`d >= 0 ? int(d + 0.5) : int(d - 0.5)`。
#[inline]
pub fn q_round(x: f64) -> i32 {
    if x >= 0.0 {
        (x + 0.5) as i32
    } else {
        (x - 0.5) as i32
    }
}

/// 预乘像素的常量权重混合：`current*(255-mix) + to*mix` 的双通道并行 /255 取整。
///
/// `mix` 取值 0..=255。
#[inline]
pub fn blend_premultiplied(current: u32, to: u32, mix: i32) -> u32 {
    let inverse = (255 - mix) as u64;
    let mix = mix as u64;
    let pair = |first: u32, second: u32| -> u32 {
        let mut value = first as u64 * inverse + second as u64 * mix + 0x007f_007f;
        value += 0x0001_0001 + ((value >> 8) & 0x00ff_00ff);
        ((value >> 8) & 0x00ff_00ff) as u32
    };
    let red_blue = pair(current & 0x00ff_00ff, to & 0x00ff_00ff);
    let alpha_green = pair((current >> 8) & 0x00ff_00ff, (to >> 8) & 0x00ff_00ff);
    red_blue | (alpha_green << 8)
}

/// 两个 0..=255 覆盖度相乘并 /255 四舍五入。
#[inline]
pub fn combine_coverage(first: i32, second: i32) -> i32 {
    (first * second + 127) / 255
}

/// 强度归一化到 [0,1]：NaN 视为 1，±inf 分别取 0/1。
pub fn normalized_strength(strength: f64) -> f64 {
    let s = if strength.is_nan() {
        1.0
    } else if !strength.is_finite() {
        if strength < 0.0 { 0.0 } else { 1.0 }
    } else {
        strength
    };
    s.clamp(0.0, 1.0)
}

/// 强度转 0..=255 的混合权重。
pub fn normalized_strength_mix(strength: f64) -> i32 {
    q_round(normalized_strength(strength) * 255.0).clamp(0, 255)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// q_round 与 Qt 一致：正负均远离零方向进位。
    #[test]
    fn q_round_matches_qt() {
        assert_eq!(q_round(0.5), 1);
        assert_eq!(q_round(-0.5), -1);
        assert_eq!(q_round(2.49), 2);
        assert_eq!(q_round(-2.51), -3);
    }

    /// 强度归一化：NaN 视为 1，无穷分别取 0/1，其余钳到 [0,1]。
    #[test]
    fn strength_normalization() {
        assert_eq!(normalized_strength(f64::NAN), 1.0);
        assert_eq!(normalized_strength(f64::INFINITY), 1.0);
        assert_eq!(normalized_strength(f64::NEG_INFINITY), 0.0);
        assert_eq!(normalized_strength(-3.0), 0.0);
        assert_eq!(normalized_strength_mix(0.5), 128);
        assert_eq!(normalized_strength_mix(2.0), 255);
    }

    /// 混合端点：mix=0 取当前值，mix=255 取目标值。
    #[test]
    fn blend_endpoints() {
        let (a, b) = (0x8040_2010, 0xff10_2030);
        assert_eq!(blend_premultiplied(a, b, 0), a);
        assert_eq!(blend_premultiplied(a, b, 255), b);
    }

    /// combine_coverage 端点。
    #[test]
    fn coverage_endpoints() {
        assert_eq!(combine_coverage(255, 255), 255);
        assert_eq!(combine_coverage(0, 255), 0);
        assert_eq!(combine_coverage(128, 255), 128);
    }
}
