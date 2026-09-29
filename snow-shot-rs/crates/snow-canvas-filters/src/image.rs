//! 图像视图：以 `u32` 像素（0xAARRGGBB 预乘）为单位的只读/可写视图与矩形。

/// 只读 ARGB32 预乘图像视图；`stride` 以像素（u32）为单位。
#[derive(Clone, Copy, Debug)]
pub struct ImageRef<'a> {
    /// 像素数据。
    pub data: &'a [u32],
    /// 宽（像素）。
    pub width: i32,
    /// 高（像素）。
    pub height: i32,
    /// 行步长（像素）。
    pub stride: usize,
}

/// 可写 ARGB32 预乘图像视图；`stride` 以像素为单位。
#[derive(Debug)]
pub struct ImageMut<'a> {
    /// 像素数据。
    pub data: &'a mut [u32],
    /// 宽（像素）。
    pub width: i32,
    /// 高（像素）。
    pub height: i32,
    /// 行步长（像素）。
    pub stride: usize,
}

impl ImageMut<'_> {
    /// 以只读视图借用当前图像。
    pub fn as_ref(&self) -> ImageRef<'_> {
        ImageRef {
            data: self.data,
            width: self.width,
            height: self.height,
            stride: self.stride,
        }
    }
}

/// 只读 8 位遮罩视图；`stride` 以字节为单位。
#[derive(Clone, Copy, Debug)]
pub struct AlphaRef<'a> {
    /// 遮罩数据。
    pub data: &'a [u8],
    /// 宽。
    pub width: i32,
    /// 高。
    pub height: i32,
    /// 行步长（字节）。
    pub stride: usize,
}

/// 整数矩形（左上角 + 宽高，右/下边界为开区间外一格，语义同 Qt `QRect`）。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct Rect {
    /// 左。
    pub x: i32,
    /// 上。
    pub y: i32,
    /// 宽。
    pub w: i32,
    /// 高。
    pub h: i32,
}

impl Rect {
    /// 构造矩形。
    pub fn new(x: i32, y: i32, w: i32, h: i32) -> Self {
        Self { x, y, w, h }
    }
    /// 左边界。
    pub fn left(&self) -> i32 {
        self.x
    }
    /// 上边界。
    pub fn top(&self) -> i32 {
        self.y
    }
    /// 最右一列（含），同 Qt `right()`。
    pub fn right(&self) -> i32 {
        self.x + self.w - 1
    }
    /// 最下一行（含），同 Qt `bottom()`。
    pub fn bottom(&self) -> i32 {
        self.y + self.h - 1
    }
    /// 是否为空。
    pub fn is_empty(&self) -> bool {
        self.w <= 0 || self.h <= 0
    }
    /// 与另一矩形求交；无交集返回空矩形。
    pub fn intersected(&self, o: &Rect) -> Rect {
        if self.is_empty() || o.is_empty() {
            return Rect::default();
        }
        let l = self.x.max(o.x);
        let t = self.y.max(o.y);
        let r = self.right().min(o.right());
        let b = self.bottom().min(o.bottom());
        if l > r || t > b {
            Rect::default()
        } else {
            Rect::new(l, t, r - l + 1, b - t + 1)
        }
    }
    /// 是否完整包含另一矩形。
    pub fn contains(&self, o: &Rect) -> bool {
        !self.is_empty()
            && !o.is_empty()
            && o.x >= self.x
            && o.y >= self.y
            && o.right() <= self.right()
            && o.bottom() <= self.bottom()
    }
    /// 四边外扩（负值内缩），同 Qt `adjusted(-d,-d,d,d)`。
    pub fn adjusted(&self, dl: i32, dt: i32, dr: i32, db: i32) -> Rect {
        Rect::new(self.x + dl, self.y + dt, self.w + dr - dl, self.h + db - dt)
    }
}

/// 自有像素缓冲（`stride == width`）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OwnedImage {
    /// 像素数据。
    pub data: Vec<u32>,
    /// 宽。
    pub width: i32,
    /// 高。
    pub height: i32,
}

impl OwnedImage {
    /// 创建全 0 图像。
    pub fn new(width: i32, height: i32) -> Self {
        Self {
            data: vec![0; (width.max(0) as usize) * (height.max(0) as usize)],
            width,
            height,
        }
    }
    /// 只读视图。
    pub fn as_ref(&self) -> ImageRef<'_> {
        ImageRef {
            data: &self.data,
            width: self.width,
            height: self.height,
            stride: self.width as usize,
        }
    }
    /// 可写视图。
    pub fn as_mut(&mut self) -> ImageMut<'_> {
        ImageMut {
            data: &mut self.data,
            width: self.width,
            height: self.height,
            stride: self.width as usize,
        }
    }
}
