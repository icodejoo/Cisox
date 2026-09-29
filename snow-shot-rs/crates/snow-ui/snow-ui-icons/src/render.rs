//! 光栅化与缓存：resvg 渲染 SVG 为预乘 RGBA，带字节上限的 LRU。

use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard};

use resvg::{tiny_skia, usvg};

use crate::assets::{PRIMARY_PLACEHOLDER, SECONDARY_PLACEHOLDER, find_template};
use crate::model::{IconFit, IconPalette, IconRef, IconTheme, ResolvedColors};

/// 单边像素上限（与 C++ 一致）。
const MAX_DIMENSION: u64 = 16384;
/// 单次瞬时光栅字节上限（与 C++ 一致）。
const MAX_TRANSIENT_BYTES: u64 = 64 * 1024 * 1024;
/// 默认缓存总字节上限。
const DEFAULT_CACHE_BYTES: u64 = 2 * 1024 * 1024;
/// 默认缓存条目上限。
const DEFAULT_MAX_ENTRIES: usize = 512;
/// 默认单张缓存字节上限。
const DEFAULT_MAX_RASTER_BYTES: u64 = 256 * 1024;
/// 非法尺寸降级值（逻辑像素）。
const FALLBACK_SIZE: u32 = 16;
/// DPR 下限。
const DPR_MIN: f32 = 0.25;
/// DPR 上限。
const DPR_MAX: f32 = 8.0;
/// 每像素字节数。
const BYTES_PER_PIXEL: u64 = 4;

/// 图标不存在或解析失败时使用的占位图标（方框加问号）。
const FALLBACK_SVG: &str = concat!(
    r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 1024 1024" fill="__ADQT_SLOT_PRIMARY__">"#,
    r#"<path fill-rule="evenodd" d="M128 128h768v768H128zM192 192v640h640V192z"/>"#,
    r#"<path d="M480 704h64v64h-64zM448 400a64 64 0 1 1 96 55c-24 14-32 24-32 52h-64c0-48 18-70 48-88 12-8 16-14 16-24a32 32 0 0 0-64 0z"/>"#,
    r#"</svg>"#
);

/// 渲染请求。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct IconRequest {
    /// 逻辑宽度，0 视为 16。
    pub width: u32,
    /// 逻辑高度，0 视为 16。
    pub height: u32,
    /// 设备像素比，非正或 NaN 视为 1.0，其余夹到 0.25~8。
    pub dpr: f32,
    /// 缩放适配方式。
    pub fit: IconFit,
    /// 是否禁用态（使用禁用色）。
    pub disabled: bool,
}

impl IconRequest {
    /// 构造正方形请求。
    ///
    /// # 参数
    /// - `size`: 逻辑边长。
    /// - `dpr`: 设备像素比。
    pub fn square(size: u32, dpr: f32) -> Self {
        Self {
            width: size,
            height: size,
            dpr,
            fit: IconFit::Contain,
            disabled: false,
        }
    }
}

/// 光栅结果：预乘 RGBA8，按行紧密排列。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IconBitmap {
    /// 物理像素宽。
    pub width: u32,
    /// 物理像素高。
    pub height: u32,
    /// 预乘 RGBA 数据（长度 = width*height*4）。
    pub data: Arc<[u8]>,
    /// 是否为占位降级图标。
    pub is_fallback: bool,
}

impl IconBitmap {
    /// 转为非预乘（straight）RGBA，供需要直通 alpha 的下游使用。
    pub fn to_straight_rgba(&self) -> Vec<u8> {
        let mut out = self.data.to_vec();
        for px in out.chunks_exact_mut(4) {
            let a = px[3] as u32;
            if a != 0 && a != 255 {
                for c in &mut px[..3] {
                    *c = ((*c as u32 * 255 + a / 2) / a).min(255) as u8;
                }
            }
        }
        out
    }
}

/// 缓存统计快照。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct CacheStats {
    /// 当前条目数。
    pub entries: usize,
    /// 当前占用字节。
    pub bytes: u64,
    /// 命中次数。
    pub hits: u64,
    /// 未命中次数。
    pub misses: u64,
    /// 淘汰次数。
    pub evictions: u64,
}

/// 缓存键：图标 + 生效颜色 + 物理尺寸 + 适配方式。
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct Key {
    theme: IconTheme,
    name: String,
    width: u32,
    height: u32,
    fit: IconFit,
    primary: u32,
    secondary: u32,
}

/// 缓存条目。
struct Entry {
    bitmap: IconBitmap,
    tick: u64,
}

/// 缓存内部状态。
struct Inner {
    map: HashMap<Key, Entry>,
    bytes: u64,
    tick: u64,
    stats: CacheStats,
    palette: IconPalette,
}

/// 图标渲染器：线程安全，内部带 LRU 缓存。
///
/// ```
/// use snow_ui_icons::{IconRef, IconRenderer, IconRequest, IconTheme};
/// let r = IconRenderer::new();
/// let icon = IconRef::new(IconTheme::TwoTone, "bell");
/// let a = r.render(&icon, &IconRequest::square(32, 1.0)).unwrap();
/// let b = r.render(&icon, &IconRequest::square(32, 1.0)).unwrap();
/// assert_eq!(a, b);
/// assert_eq!(r.stats().hits, 1);
/// ```
pub struct IconRenderer {
    inner: Mutex<Inner>,
    cache_bytes: u64,
    max_entries: usize,
    max_raster_bytes: u64,
}

impl Default for IconRenderer {
    fn default() -> Self {
        Self::new()
    }
}

impl IconRenderer {
    /// 以默认上限（2 MiB / 512 条 / 单张 256 KiB）和默认调色板创建。
    pub fn new() -> Self {
        Self::with_limits(
            DEFAULT_CACHE_BYTES,
            DEFAULT_MAX_ENTRIES,
            DEFAULT_MAX_RASTER_BYTES,
        )
    }

    /// 自定义缓存上限创建。
    ///
    /// # 参数
    /// - `cache_bytes`: 总字节上限。
    /// - `max_entries`: 条目上限。
    /// - `max_raster_bytes`: 单张可入缓存的字节上限。
    pub fn with_limits(cache_bytes: u64, max_entries: usize, max_raster_bytes: u64) -> Self {
        Self {
            inner: Mutex::new(Inner {
                map: HashMap::new(),
                bytes: 0,
                tick: 0,
                stats: CacheStats::default(),
                palette: IconPalette::default(),
            }),
            cache_bytes,
            max_entries,
            max_raster_bytes,
        }
    }

    /// 设置调色板；变更后清空缓存以回收旧条目。
    pub fn set_palette(&self, palette: IconPalette) {
        let mut g = self.lock();
        if g.palette != palette {
            g.palette = palette;
            g.map.clear();
            g.bytes = 0;
        }
    }

    /// 清空缓存。
    pub fn clear(&self) {
        let mut g = self.lock();
        g.map.clear();
        g.bytes = 0;
    }

    /// 读取缓存统计。
    pub fn stats(&self) -> CacheStats {
        let g = self.lock();
        CacheStats {
            entries: g.map.len(),
            bytes: g.bytes,
            ..g.stats
        }
    }

    /// 渲染图标；图标不存在或解析失败时返回占位图标，不会 panic。
    ///
    /// # 参数
    /// - `icon`: 图标引用。
    /// - `req`: 尺寸/DPR/适配/禁用态。
    ///
    /// # 返回
    /// 预乘 RGBA 位图；仅当请求尺寸超出安全上限时为 `None`。
    pub fn render(&self, icon: &IconRef, req: &IconRequest) -> Option<IconBitmap> {
        let (w, h) = physical_size(req)?;
        let palette = self.lock().palette;
        let colors = palette.resolve(icon.theme, &icon.colors, req.disabled);
        let mono = !icon.theme.is_two_tone();
        let key = Key {
            theme: icon.theme,
            name: icon.name.clone(),
            width: w,
            height: h,
            fit: req.fit,
            primary: colors.primary.packed(),
            // 单色图标不消费副色，避免重复缓存
            secondary: if mono { 0 } else { colors.secondary.packed() },
        };
        {
            let mut g = self.lock();
            g.tick += 1;
            let tick = g.tick;
            if let Some(e) = g.map.get_mut(&key) {
                e.tick = tick;
                let b = e.bitmap.clone();
                g.stats.hits += 1;
                return Some(b);
            }
            g.stats.misses += 1;
        }

        let bitmap = find_template(icon.theme, &icon.name)
            .and_then(|t| rasterize(t, &colors, w, h, req.fit, mono, false))
            .or_else(|| rasterize(FALLBACK_SVG, &colors, w, h, req.fit, true, true))?;
        self.insert(key, &bitmap);
        Some(bitmap)
    }

    /// 在容量允许时插入缓存并按 LRU 淘汰。
    fn insert(&self, key: Key, bitmap: &IconBitmap) {
        let cost = bitmap.data.len() as u64;
        if cost > self.max_raster_bytes || cost > self.cache_bytes {
            return;
        }
        let mut g = self.lock();
        g.tick += 1;
        let tick = g.tick;
        if g.map
            .insert(
                key,
                Entry {
                    bitmap: bitmap.clone(),
                    tick,
                },
            )
            .is_none()
        {
            g.bytes += cost;
        }
        while g.bytes > self.cache_bytes || g.map.len() > self.max_entries {
            let Some(oldest) = g
                .map
                .iter()
                .min_by_key(|(_, e)| e.tick)
                .map(|(k, _)| k.clone())
            else {
                break;
            };
            if let Some(e) = g.map.remove(&oldest) {
                g.bytes -= e.bitmap.data.len() as u64;
                g.stats.evictions += 1;
            }
        }
    }

    /// 取锁；中毒时沿用内部数据（缓存无不变式风险）。
    fn lock(&self) -> MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }
}

/// 计算物理像素尺寸；超出安全上限返回 `None`。
fn physical_size(req: &IconRequest) -> Option<(u32, u32)> {
    let (lw, lh) = if req.width == 0 || req.height == 0 {
        (FALLBACK_SIZE, FALLBACK_SIZE)
    } else {
        (req.width, req.height)
    };
    let dpr = if req.dpr.is_finite() && req.dpr > 0.0 {
        req.dpr.clamp(DPR_MIN, DPR_MAX)
    } else {
        1.0
    };
    let phys = |l: u32| ((l as f64 * dpr as f64).round() as u64).max(1);
    let (w, h) = (phys(lw), phys(lh));
    if w > MAX_DIMENSION || h > MAX_DIMENSION || w * h * BYTES_PER_PIXEL > MAX_TRANSIENT_BYTES {
        return None;
    }
    Some((w as u32, h as u32))
}

/// 代入颜色、解析并渲染一份 SVG 模板。
fn rasterize(
    template: &str,
    colors: &ResolvedColors,
    w: u32,
    h: u32,
    fit: IconFit,
    mono: bool,
    is_fallback: bool,
) -> Option<IconBitmap> {
    let svg = template
        .replace(PRIMARY_PLACEHOLDER, &colors.primary.hex_rgb())
        .replace(SECONDARY_PLACEHOLDER, &colors.secondary.hex_rgb());
    let tree = usvg::Tree::from_str(&svg, &usvg::Options::default()).ok()?;
    let size = tree.size();
    let (vw, vh) = (size.width(), size.height());
    if !(vw > 0.0 && vh > 0.0) {
        return None;
    }
    let (fw, fh) = (w as f32, h as f32);
    let (sx, sy, tx, ty) = match fit {
        IconFit::Stretch => (fw / vw, fh / vh, 0.0, 0.0),
        IconFit::Contain => {
            let s = (fw / vw).min(fh / vh);
            (s, s, (fw - vw * s) / 2.0, (fh - vh * s) / 2.0)
        }
    };
    let mut pixmap = tiny_skia::Pixmap::new(w, h)?;
    let transform = tiny_skia::Transform::from_row(sx, 0.0, 0.0, sy, tx, ty);
    resvg::render(&tree, transform, &mut pixmap.as_mut());
    let mut data = pixmap.take();
    // 单色图标主色透明度：C++ 以 DestinationIn 整体乘 alpha，SVG 内颜色不含 alpha
    if mono && colors.primary.a < 255 {
        let a = colors.primary.a as u32;
        for c in &mut data {
            *c = ((*c as u32 * a + 127) / 255) as u8;
        }
    }
    Some(IconBitmap {
        width: w,
        height: h,
        data: data.into(),
        is_fallback,
    })
}
