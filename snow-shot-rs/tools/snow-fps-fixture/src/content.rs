//! 画面内容：序号条之外的运动背景（纯数据描述，由 GPU 后端执行），保证画面逐帧变化并制造编码负载。

use crate::seqbar::bar_height;

/// 条纹宽度（像素）。
const STRIPE_WIDTH: i32 = 32;
/// 条纹调色板长度。
const PALETTE_LEN: usize = 16;
/// 每帧条纹平移量（像素）。
const STRIPE_STEP: i32 = 7;
/// 噪声区域占宽/高的分母（约 1/4 边长）。
const NOISE_DIVISOR: u32 = 4;
/// 轻负载模式移动方块边长。
const BOX_SIZE: i32 = 240;
/// 轻负载模式方块每帧位移。
const BOX_STEP: i32 = 9;
/// xorshift 初始种子。
pub const NOISE_SEED: u32 = 0x9E37_79B9;
/// 轻负载背景色（RGBA 浮点）。
const LIGHT_BACKGROUND: [f32; 4] = [0.125, 0.125, 0.125, 1.0];
/// 轻负载方块色。
const LIGHT_BOX: [f32; 4] = [0.88, 0.63, 0.125, 1.0];

/// 背景负载模式。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Load {
    /// 只有序号条与一个移动方块（变化区域小）。
    Light,
    /// 全屏移动竖条纹。
    Stripes,
    /// 全屏条纹 + 逐帧随机噪声块（编码最重）。
    Noise,
}

impl Load {
    /// 解析命令行取值。
    ///
    /// # 参数
    /// - `text`：`light` / `stripes` / `noise`。
    ///
    /// # 返回
    /// 对应模式；未知取值返回 `None`。
    pub fn parse(text: &str) -> Option<Self> {
        match text {
            "light" => Some(Self::Light),
            "stripes" => Some(Self::Stripes),
            "noise" => Some(Self::Noise),
            _ => None,
        }
    }
}

/// 半开矩形 `(left, top, right, bottom)`，像素。
pub type Span = (i32, i32, i32, i32);

/// 同色的一批填充矩形。
#[derive(Clone, Debug, PartialEq)]
pub struct Fill {
    /// RGBA 浮点色。
    pub color: [f32; 4],
    /// 需要填充的矩形。
    pub rects: Vec<Span>,
}

/// 单帧背景的描述。
#[derive(Clone, Debug, PartialEq)]
pub struct Scene {
    /// 纯色填充（按绘制顺序，后者覆盖前者）。
    pub fills: Vec<Fill>,
    /// 需要逐帧上传随机噪声的区域 `(x, y, 宽, 高)`。
    pub noise: Option<(i32, i32, u32, u32)>,
}

/// 调色板第 `idx` 项（避开纯黑白以免与序号条混淆）。
fn palette(idx: usize) -> [f32; 4] {
    let i = (idx % PALETTE_LEN) as f32;
    [0.15 + 0.05 * i, 0.9 - 0.045 * i, 0.3 + 0.03 * ((i * 5.0) % PALETTE_LEN as f32), 1.0]
}

/// 构造某一帧的背景描述（不含序号条）。
///
/// # 参数
/// - `width`、`height`：画面尺寸。
/// - `load`：负载模式。
/// - `frame`：帧序号（决定运动相位，从 1 起）。
///
/// # 返回
/// 场景描述；矩形均落在序号条下方的画面内。
///
/// # 示例
/// ```
/// use snow_fps_fixture::content::{build_scene, Load};
/// let a = build_scene(640, 360, Load::Stripes, 1);
/// let b = build_scene(640, 360, Load::Stripes, 2);
/// assert_ne!(a, b);
/// ```
pub fn build_scene(width: u32, height: u32, load: Load, frame: u32) -> Scene {
    let (w, h) = (width as i32, height as i32);
    let top = bar_height(height) as i32;
    match load {
        Load::Light => {
            let size = BOX_SIZE.min(w).min(h - top).max(1);
            let span_x = (w - size).max(1);
            let span_y = (h - top - size).max(1);
            let f = frame as i32;
            let x = (f * BOX_STEP) % span_x;
            let y = top + (f * BOX_STEP / 2) % span_y;
            Scene {
                fills: vec![
                    Fill { color: LIGHT_BACKGROUND, rects: vec![(0, top, w, h)] },
                    Fill { color: LIGHT_BOX, rects: vec![(x, y, x + size, y + size)] },
                ],
                noise: None,
            }
        }
        Load::Stripes | Load::Noise => {
            let shift = frame as i32 * STRIPE_STEP;
            let phase = shift % STRIPE_WIDTH;
            let base = (shift / STRIPE_WIDTH) as usize;
            let mut fills: Vec<Fill> = (0..PALETTE_LEN).map(|i| Fill { color: palette(i), rects: Vec::new() }).collect();
            let count = (w + phase + STRIPE_WIDTH - 1) / STRIPE_WIDTH;
            for i in 0..count {
                let left = (i * STRIPE_WIDTH - phase).max(0);
                let right = ((i + 1) * STRIPE_WIDTH - phase).min(w);
                if right > left {
                    fills[(base + i as usize) % PALETTE_LEN].rects.push((left, top, right, h));
                }
            }
            fills.retain(|f| !f.rects.is_empty());
            let noise = (load == Load::Noise).then(|| {
                let (nw, nh) = (width / NOISE_DIVISOR, height / NOISE_DIVISOR);
                ((w - nw as i32), (h - nh as i32), nw, nh)
            });
            Scene { fills, noise }
        }
    }
}

/// 用 xorshift32 填充一块噪声像素（BGRA，alpha 置 0）。
///
/// # 参数
/// - `pixels`：待填充缓冲。
/// - `state`：随机状态（原地推进；为 0 时按种子重置）。
pub fn fill_noise(pixels: &mut [u32], state: &mut u32) {
    if *state == 0 {
        *state = NOISE_SEED;
    }
    for px in pixels {
        *state ^= *state << 13;
        *state ^= *state >> 17;
        *state ^= *state << 5;
        *px = *state & 0x00FF_FFFF;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 条纹矩形逐帧不同，且水平方向恰好无缝无重叠地铺满整宽。
    #[test]
    fn stripes_tile_width_exactly_and_move() {
        let (w, h) = (1000u32, 400u32);
        let top = bar_height(h) as i32;
        let mut prev = None;
        for frame in 1..200 {
            let scene = build_scene(w, h, Load::Stripes, frame);
            let mut cols = vec![0u8; w as usize];
            for f in &scene.fills {
                for &(l, t, r, b) in &f.rects {
                    assert!(t == top && b == h as i32 && l >= 0 && r <= w as i32 && l < r);
                    for c in l..r {
                        cols[c as usize] += 1;
                    }
                }
            }
            assert!(cols.iter().all(|&c| c == 1), "frame {frame}");
            assert!(prev.as_ref() != Some(&scene), "frame {frame} 与上一帧相同");
            prev = Some(scene);
        }
    }

    /// 噪声模式带噪声区域，落在画面右下角内；条纹模式没有。
    #[test]
    fn noise_region_in_bounds() {
        let s = build_scene(2560, 1440, Load::Noise, 5);
        assert_eq!(s.noise, Some((1920, 1080, 640, 360)));
        assert!(build_scene(2560, 1440, Load::Stripes, 5).noise.is_none());
    }

    /// 轻负载：方块逐帧移动且全部在序号条下方、画面内。
    #[test]
    fn light_box_moves_inside_frame() {
        let (w, h) = (640u32, 360u32);
        let top = bar_height(h) as i32;
        let mut seen = std::collections::HashSet::new();
        for frame in 1..300 {
            let scene = build_scene(w, h, Load::Light, frame);
            let (l, t, r, b) = scene.fills[1].rects[0];
            assert!(l >= 0 && t >= top && r <= w as i32 && b <= h as i32, "frame {frame}");
            seen.insert((l, t));
        }
        assert!(seen.len() > 100);
    }

    /// 极小画面不越界不崩溃。
    #[test]
    fn tiny_frame_is_safe() {
        for load in [Load::Light, Load::Stripes, Load::Noise] {
            let s = build_scene(40, 40, load, 3);
            assert!(!s.fills.is_empty());
        }
    }

    /// 噪声填充：状态推进、每次结果不同、alpha 位为 0；种子 0 会被重置。
    #[test]
    fn noise_fill_advances() {
        let mut state = 0u32;
        let (mut a, mut b) = (vec![0u32; 64], vec![0u32; 64]);
        fill_noise(&mut a, &mut state);
        fill_noise(&mut b, &mut state);
        assert_ne!(a, b);
        assert!(a.iter().all(|&p| p >> 24 == 0));
        assert_ne!(state, 0);
    }

    /// 模式解析。
    #[test]
    fn load_parse() {
        assert_eq!(Load::parse("noise"), Some(Load::Noise));
        assert_eq!(Load::parse("x"), None);
    }
}
