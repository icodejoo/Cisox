//! CPU 参考实现（BT.709 limited range）与 NV12 误差比较。

use crate::gpu::Nv12;

/// 单像素 BGRA -> (Y, Cb, Cr)，BT.709 limited，返回浮点（未取整）。
pub fn rgb_to_ycbcr(r: f64, g: f64, b: f64) -> (f64, f64, f64) {
    let y = 0.2126 * r + 0.7152 * g + 0.0722 * b; // 0..255
    let cb = (b - y) / 1.8556; // -127.5..127.5
    let cr = (r - y) / 1.5748;
    (16.0 + 219.0 * y / 255.0, 128.0 + 224.0 * cb / 255.0, 128.0 + 224.0 * cr / 255.0)
}

/// 参考 NV12：Y 逐像素；UV 提供两种采样位置。
pub struct RefNv12 {
    /// Y 平面。
    pub y: Vec<f64>,
    /// 色度居中（2x2 均值）。
    pub uv_center: Vec<f64>,
    /// 色度左对齐（偶数列、上下两行均值）。
    pub uv_left: Vec<f64>,
}

/// 由 BGRA 像素生成参考 NV12。
pub fn reference(bgra: &[u8], w: u32, h: u32) -> RefNv12 {
    let (w, h) = (w as usize, h as usize);
    let mut yy = vec![0.0; w * h];
    let mut cb = vec![0.0; w * h];
    let mut cr = vec![0.0; w * h];
    for i in 0..w * h {
        let (b, g, r) = (bgra[i * 4] as f64, bgra[i * 4 + 1] as f64, bgra[i * 4 + 2] as f64);
        let (y, u, v) = rgb_to_ycbcr(r, g, b);
        yy[i] = y;
        cb[i] = u;
        cr[i] = v;
    }
    let mut uc = vec![0.0; w * h / 2];
    let mut ul = vec![0.0; w * h / 2];
    for by in 0..h / 2 {
        for bx in 0..w / 2 {
            let p = |x: usize, y: usize, pl: &Vec<f64>| pl[y * w + x];
            for (k, pl) in [&cb, &cr].into_iter().enumerate() {
                let c = (p(2 * bx, 2 * by, pl)
                    + p(2 * bx + 1, 2 * by, pl)
                    + p(2 * bx, 2 * by + 1, pl)
                    + p(2 * bx + 1, 2 * by + 1, pl))
                    / 4.0;
                let l = (p(2 * bx, 2 * by, pl) + p(2 * bx, 2 * by + 1, pl)) / 2.0;
                uc[by * w + bx * 2 + k] = c;
                ul[by * w + bx * 2 + k] = l;
            }
        }
    }
    RefNv12 { y: yy, uv_center: uc, uv_left: ul }
}

/// 误差摘要。
#[derive(Clone, Debug, Default)]
pub struct Err8 {
    /// 最大绝对误差。
    pub max: f64,
    /// 平均绝对误差。
    pub mean: f64,
    /// PSNR（dB）。
    pub psnr: f64,
}

fn diff(a: &[u8], b: &[f64]) -> Err8 {
    let (mut mx, mut sum, mut sq) = (0.0f64, 0.0f64, 0.0f64);
    for (x, y) in a.iter().zip(b) {
        let d = (*x as f64 - y).abs();
        mx = mx.max(d);
        sum += d;
        sq += d * d;
    }
    let n = a.len() as f64;
    let mse = sq / n;
    Err8 {
        max: mx,
        mean: sum / n,
        psnr: if mse == 0.0 { 99.0 } else { 10.0 * (255.0f64 * 255.0 / mse).log10() },
    }
}

/// 比较结果。
pub struct Compare {
    /// 亮度误差。
    pub y: Err8,
    /// 色度误差（居中参考）。
    pub uv_center: Err8,
    /// 色度误差（左对齐参考）。
    pub uv_left: Err8,
}

/// 把 GPU 回读的 NV12 与参考比较。
pub fn compare(got: &Nv12, r: &RefNv12) -> Compare {
    Compare {
        y: diff(&got.y, &r.y),
        uv_center: diff(&got.uv, &r.uv_center),
        uv_left: diff(&got.uv, &r.uv_left),
    }
}

impl Compare {
    /// 序列化为 JSON 对象。
    pub fn json(&self) -> String {
        let e = |e: &Err8| format!("{{\"max\":{:.2},\"mean\":{:.3},\"psnr\":{:.2}}}", e.max, e.mean, e.psnr);
        format!(
            "{{\"y\":{},\"uv_center\":{},\"uv_left\":{}}}",
            e(&self.y),
            e(&self.uv_center),
            e(&self.uv_left)
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_colors_match_bt709_limited() {
        let near = |a: (f64, f64, f64), t: (f64, f64, f64)| {
            (a.0 - t.0).abs() < 0.6 && (a.1 - t.1).abs() < 0.6 && (a.2 - t.2).abs() < 0.6
        };
        assert!(near(rgb_to_ycbcr(0.0, 0.0, 0.0), (16.0, 128.0, 128.0)));
        assert!(near(rgb_to_ycbcr(255.0, 255.0, 255.0), (235.0, 128.0, 128.0)));
        assert!(near(rgb_to_ycbcr(255.0, 0.0, 0.0), (63.0, 102.0, 240.0)));
        assert!(near(rgb_to_ycbcr(0.0, 255.0, 0.0), (173.0, 42.0, 26.0)));
        assert!(near(rgb_to_ycbcr(0.0, 0.0, 255.0), (32.0, 240.0, 118.0)));
    }

    #[test]
    fn solid_frame_reference_is_flat() {
        let px: Vec<u8> = [0u8, 0, 255, 255].repeat(16 * 16);
        let r = reference(&px, 16, 16);
        assert!(r.y.iter().all(|v| (v - 63.0).abs() < 0.6));
        assert_eq!(r.uv_center.len(), 16 * 8);
        assert!(r.uv_center.iter().zip(&r.uv_left).all(|(a, b)| (a - b).abs() < 1e-9));
    }
}
