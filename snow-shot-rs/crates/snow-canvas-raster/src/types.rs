//! 光栅化器的输入配置与输出数据结构。

use snow_draw_engine_display::DisplayItemId;

/// 分块边长下限（像素）。
pub const MIN_TILE_SIZE: u32 = 16;
/// 分块边长上限（像素）。
pub const MAX_TILE_SIZE: u32 = 1024;
/// 默认分块边长（P0-V2 spike 实测 128/256 无显著差异，512 明显更差）。
pub const DEFAULT_TILE_SIZE: u32 = 256;
/// 设备像素比下限。
pub const MIN_DEVICE_PIXEL_RATIO: f32 = 0.25;
/// 设备像素比上限。
pub const MAX_DEVICE_PIXEL_RATIO: f32 = 8.0;
/// 单边物理像素上限，防止异常 surface 尺寸导致巨量内存分配。
pub const MAX_SURFACE_PIXELS: u32 = 16384;

/// 光栅化器配置。
///
/// # 示例
/// ```
/// use snow_canvas_raster::RasterConfig;
/// let cfg = RasterConfig { tile_size: 256, device_pixel_ratio: 2.0 };
/// assert_eq!(cfg.normalized().tile_size, 256);
/// ```
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RasterConfig {
    /// 分块边长（像素），会被夹到 `MIN_TILE_SIZE..=MAX_TILE_SIZE`。
    pub tile_size: u32,
    /// 设备像素比：逻辑坐标乘以它得到物理像素（2x 屏为 2.0）。
    pub device_pixel_ratio: f32,
}

impl Default for RasterConfig {
    fn default() -> Self {
        Self {
            tile_size: DEFAULT_TILE_SIZE,
            device_pixel_ratio: 1.0,
        }
    }
}

impl RasterConfig {
    /// 返回夹到合法范围后的配置（非有限的像素比回落为 1.0）。
    pub fn normalized(self) -> Self {
        let dpr = if self.device_pixel_ratio.is_finite() {
            self.device_pixel_ratio
                .clamp(MIN_DEVICE_PIXEL_RATIO, MAX_DEVICE_PIXEL_RATIO)
        } else {
            1.0
        };
        Self {
            tile_size: self.tile_size.clamp(MIN_TILE_SIZE, MAX_TILE_SIZE),
            device_pixel_ratio: dpr,
        }
    }
}

/// 分块坐标（列、行），同时是上层缓存纹理的键。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct TileKey {
    /// 列号（从左到右）。
    pub col: u32,
    /// 行号（从上到下）。
    pub row: u32,
}

/// 一个待上传的脏块。
///
/// 像素为 **预乘 alpha 的 RGBA8**（tiny-skia 原生格式），行紧密排列。
/// 上传到 BGRA 纹理只需交换 R/B 通道。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RasterTile {
    /// 分块坐标。
    pub key: TileKey,
    /// 块左上角在画布上的物理像素 x。
    pub x: u32,
    /// 块左上角在画布上的物理像素 y。
    pub y: u32,
    /// 块宽（边缘块可能小于分块边长）。
    pub w: u32,
    /// 块高。
    pub h: u32,
    /// 预乘 RGBA8 像素，长度 `w * h * 4`。
    pub rgba: Vec<u8>,
}

/// 由其他 crate 负责绘制、本光栅化器只登记位置的元素种类。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DeferredKind {
    /// 文字元素，由 `snow-canvas-text` 绘制。
    Text,
    /// 滤镜元素（马赛克/模糊/灰度/反色/浮雕/智能擦除），由 `snow-canvas-filters` 绘制。
    Filter,
}

/// 本帧脏区内被跳过的“待外部绘制”元素。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DeferredItem {
    /// 引擎元素 id。
    pub id: DisplayItemId,
    /// 元素种类。
    pub kind: DeferredKind,
    /// 元素在物理像素空间的近似包围盒 `[min_x, min_y, max_x, max_y]`。
    pub bounds: [f32; 4],
}

/// 一次 `apply_patch` 的输出。
#[derive(Clone, Debug, Default, PartialEq)]
pub struct RasterOutput {
    /// 内容有变化的块；同一 `TileKey` 的旧图应被上层丢弃。
    pub tiles: Vec<RasterTile>,
    /// 之前提交过、现在已全透明或已失效的块；上层应释放对应纹理。
    pub released: Vec<TileKey>,
    /// 本帧被脏区触及的全部块（含全透明块）；上层据此叠加文字、滤镜后再判断块是否为空。
    pub touched_tiles: Vec<TileKey>,
    /// 脏区内被跳过、需外部绘制的元素（文字、滤镜）。
    pub deferred: Vec<DeferredItem>,
    /// 当前画布物理尺寸（宽，高）。
    pub surface_size: (u32, u32),
    /// 本帧是否整幅重绘（首帧、reset、相机/尺寸变化）。
    pub full_redraw: bool,
}

/// 光栅化错误。出错后内部状态作废，调用方应 `reset()` 并以 `None` 游标重新拉取 patch。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RasterError {
    /// 尚未收到 reset patch 就收到了增量 patch。
    NotInitialized,
    /// 增量 patch 的基线版本与本地场景版本不一致。
    RevisionMismatch {
        /// 本地场景版本。
        expected: u64,
        /// patch 声明的基线版本。
        got: u64,
    },
    /// patch 的替换区间越界。
    InvalidOp,
    /// surface 物理尺寸超出上限。
    SurfaceTooLarge {
        /// 物理宽。
        width: u32,
        /// 物理高。
        height: u32,
    },
}

impl std::fmt::Display for RasterError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotInitialized => write!(f, "未收到 reset patch，无法应用增量 patch"),
            Self::RevisionMismatch { expected, got } => {
                write!(f, "patch 基线版本不匹配：本地 {expected}，patch {got}")
            }
            Self::InvalidOp => write!(f, "patch 替换区间越界"),
            Self::SurfaceTooLarge { width, height } => {
                write!(f, "surface 尺寸过大：{width}x{height}")
            }
        }
    }
}

impl std::error::Error for RasterError {}
