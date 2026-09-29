//! 滤镜参数与执行选项。

/// 滤镜类型：马赛克。
pub const FILTER_MOSAIC: u32 = 0;
/// 滤镜类型：高斯模糊（三次盒式近似）。
pub const FILTER_BLUR: u32 = 1;
/// 滤镜类型：灰度。
pub const FILTER_GRAYSCALE: u32 = 2;
/// 滤镜类型：反相。
pub const FILTER_INVERT: u32 = 3;
/// 滤镜类型：浮雕。
pub const FILTER_EMBOSS: u32 = 4;

/// 滤镜参数（对应 C++ `Parameters`）。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Parameters {
    /// 滤镜类型，取 `FILTER_*` 常量。
    pub filter_type: u32,
    /// 强度 0..=1（灰度/反相/浮雕使用）。
    pub strength: f64,
    /// 马赛克逻辑块大小。
    pub logical_block_size: f64,
    /// 模糊逻辑 sigma。
    pub logical_sigma: f64,
    /// 浮雕逻辑采样半径。
    pub logical_sampling_radius: f64,
    /// 设备像素比。
    pub device_pixel_ratio: f64,
    /// 马赛克网格原点 X（图像像素）。
    pub grid_origin_x: f64,
    /// 马赛克网格原点 Y（图像像素）。
    pub grid_origin_y: f64,
}

impl Default for Parameters {
    /// 与 C++ 默认值一致。
    fn default() -> Self {
        Self {
            filter_type: 0,
            strength: 1.0,
            logical_block_size: 1.0,
            logical_sigma: 0.0,
            logical_sampling_radius: 0.0,
            device_pixel_ratio: 1.0,
            grid_origin_x: 0.0,
            grid_origin_y: 0.0,
        }
    }
}

/// 执行选项。
#[derive(Clone, Copy, Debug, Default)]
pub struct ExecutionOptions {
    /// 强制标量路径（禁用 SIMD）。
    pub force_scalar: bool,
}

/// 高斯模糊计划（对应 C++ `GaussianBlurPlan`）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GaussianBlurPlan {
    /// 降采样倍数。
    pub reduction_factor: i32,
    /// 盒式遍数。
    pub pass_count: i32,
    /// 各遍半径。
    pub radii: [i32; 3],
    /// 物理支撑半径。
    pub physical_support_radius: i32,
}
