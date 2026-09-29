//! 屏幕截取服务（Platform Screen Capture）。
//!
//! 提供基于原生系统 API（Windows GDI / BitBlt / DIB）的高性能整屏或多屏虚拟桌面捕获，
//! 输出 BGRA / RGBA 像素缓冲区。

/// 捕获的屏幕帧数据模型。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CapturedScreen {
    /// 图像物理宽度。
    pub width: u32,
    /// 图像物理高度。
    pub height: u32,
    /// 像素数据（默认为 32 位 BGRA 格式，4 字节每像素）。
    pub data: Vec<u8>,
}

impl CapturedScreen {
    /// 创建空白或纯色帧。
    ///
    /// # 参数
    /// - `width`: 宽度。
    /// - `height`: 高度。
    /// - `fill`: 初始填充色 `(r, g, b, a)`。
    ///
    /// # 返回
    /// 捕获帧模型。
    ///
    /// # 示例
    /// ```rust
    /// use snow_platform::capture::CapturedScreen;
    /// let screen = CapturedScreen::new_solid(100, 100, (255, 0, 0, 255));
    /// assert_eq!(screen.width, 100);
    /// assert_eq!(screen.data.len(), 100 * 100 * 4);
    /// ```
    pub fn new_solid(width: u32, height: u32, fill: (u8, u8, u8, u8)) -> Self {
        let count = (width * height) as usize;
        let mut data = Vec::with_capacity(count * 4);
        for _ in 0..count {
            // BGRA
            data.push(fill.2);
            data.push(fill.1);
            data.push(fill.0);
            data.push(fill.3);
        }
        Self {
            width,
            height,
            data,
        }
    }

    /// 转换为 RGBA 像素通道格式（交换 R 与 B）。
    ///
    /// # 返回
    /// RGBA 字节流。
    ///
    /// # 示例
    /// ```rust
    /// use snow_platform::capture::CapturedScreen;
    /// let screen = CapturedScreen::new_solid(2, 2, (10, 20, 30, 255));
    /// let rgba = screen.to_rgba();
    /// assert_eq!(&rgba[0..4], &[10, 20, 30, 255]);
    /// ```
    pub fn to_rgba(&self) -> Vec<u8> {
        let mut out = self.data.clone();
        for chunk in out.chunks_exact_mut(4) {
            let b = chunk[0];
            let r = chunk[2];
            chunk[0] = r;
            chunk[2] = b;
        }
        out
    }

    /// 提取选区内的局部子图像。
    ///
    /// # 参数
    /// - `x`: 选区左上角 X。
    /// - `y`: 选区左上角 Y。
    /// - `w`: 选区宽度。
    /// - `h`: 选区高度。
    ///
    /// # 返回
    /// 裁剪后的子画面，越界部分返回空或裁剪至有效边界。
    ///
    /// # 示例
    /// ```rust
    /// use snow_platform::capture::CapturedScreen;
    /// let screen = CapturedScreen::new_solid(100, 100, (255, 255, 255, 255));
    /// let sub = screen.crop(10, 10, 20, 20).unwrap();
    /// assert_eq!((sub.width, sub.height), (20, 20));
    /// ```
    pub fn crop(&self, x: i32, y: i32, w: u32, h: u32) -> Option<Self> {
        if w == 0 || h == 0 || x < 0 || y < 0 {
            return None;
        }
        let max_x = (x as u32).checked_add(w)?;
        let max_y = (y as u32).checked_add(h)?;
        if max_x > self.width || max_y > self.height {
            return None;
        }

        let mut sub = Vec::with_capacity((w * h * 4) as usize);
        let stride = (self.width * 4) as usize;
        let row_bytes = (w * 4) as usize;

        for row in 0..h {
            let start = ((y as u32 + row) as usize) * stride + (x as usize) * 4;
            let end = start + row_bytes;
            sub.extend_from_slice(&self.data[start..end]);
        }

        Some(Self {
            width: w,
            height: h,
            data: sub,
        })
    }
}

/// 执行屏幕捕获。
///
/// 若传入 `region`，则截取指定物理矩形区域；若为 `None`，则截取主显示器或全虚拟桌面。
///
/// # 参数
/// - `region`: 可选的 `(x, y, width, height)` 矩形。
///
/// # 返回
/// 捕获帧或错误说明。
///
/// # 示例
/// ```no_run
/// use snow_platform::capture::capture_display;
/// let screen = capture_display(Some((0, 0, 800, 600))).unwrap();
/// assert_eq!(screen.width, 800);
/// ```
pub fn capture_display(region: Option<(i32, i32, u32, u32)>) -> Result<CapturedScreen, String> {
    #[cfg(windows)]
    {
        win32_capture::capture_screen_win32(region)
    }

    #[cfg(not(windows))]
    {
        let (w, h) = region.map(|r| (r.2, r.3)).unwrap_or((1920, 1080));
        Ok(CapturedScreen::new_solid(w, h, (30, 30, 30, 255)))
    }
}

#[cfg(windows)]
mod win32_capture {
    use super::CapturedScreen;
    use windows::Win32::Graphics::Gdi::{
        BitBlt, CAPTUREBLT, CreateCompatibleBitmap, CreateCompatibleDC, DeleteDC, DeleteObject,
        GetDC, GetDIBits, ReleaseDC, SRCCOPY, SelectObject, BITMAPINFO, BITMAPINFOHEADER, BI_RGB,
        DIB_RGB_COLORS,
    };
    use windows::Win32::UI::WindowsAndMessaging::{
        GetSystemMetrics, SM_CXSCREEN, SM_CXVIRTUALSCREEN, SM_CYSCREEN, SM_CYVIRTUALSCREEN,
        SM_XVIRTUALSCREEN, SM_YVIRTUALSCREEN,
    };

    /// Windows 下使用 GDI 截取屏幕画面。
    pub fn capture_screen_win32(
        region: Option<(i32, i32, u32, u32)>,
    ) -> Result<CapturedScreen, String> {
        unsafe {
            let (x, y, width, height) = match region {
                Some((rx, ry, rw, rh)) => (rx, ry, rw as i32, rh as i32),
                None => {
                    let vx = GetSystemMetrics(SM_XVIRTUALSCREEN);
                    let vy = GetSystemMetrics(SM_YVIRTUALSCREEN);
                    let vw = GetSystemMetrics(SM_CXVIRTUALSCREEN);
                    let vh = GetSystemMetrics(SM_CYVIRTUALSCREEN);
                    if vw > 0 && vh > 0 {
                        (vx, vy, vw, vh)
                    } else {
                        (0, 0, GetSystemMetrics(SM_CXSCREEN), GetSystemMetrics(SM_CYSCREEN))
                    }
                }
            };

            if width <= 0 || height <= 0 {
                return Err("屏幕捕获尺寸非法".into());
            }

            let hdc_screen = GetDC(None);
            if hdc_screen.0.is_null() {
                return Err("获取桌面屏幕 DC 失败".into());
            }

            let hdc_mem = CreateCompatibleDC(Some(hdc_screen));
            if hdc_mem.0.is_null() {
                let _ = ReleaseDC(None, hdc_screen);
                return Err("创建兼容内存 DC 失败".into());
            }

            let hbitmap = CreateCompatibleBitmap(hdc_screen, width, height);
            if hbitmap.0.is_null() {
                let _ = DeleteDC(hdc_mem);
                let _ = ReleaseDC(None, hdc_screen);
                return Err("创建兼容位图失败".into());
            }

            let old_obj = SelectObject(hdc_mem, hbitmap.into());

            // 执行截屏拷贝（包含 CAPTUREBLT 以捕捉半透明与分层窗口）
            let rop = SRCCOPY | CAPTUREBLT;
            let bitblt_ok = BitBlt(hdc_mem, 0, 0, width, height, Some(hdc_screen), x, y, rop);

            let mut bmi = BITMAPINFO {
                bmiHeader: BITMAPINFOHEADER {
                    biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                    biWidth: width,
                    biHeight: -height, // 自顶向下位图
                    biPlanes: 1,
                    biBitCount: 32,
                    biCompression: BI_RGB.0,
                    biSizeImage: 0,
                    biXPelsPerMeter: 0,
                    biYPelsPerMeter: 0,
                    biClrUsed: 0,
                    biClrImportant: 0,
                },
                bmiColors: [windows::Win32::Graphics::Gdi::RGBQUAD::default()],
            };

            let pixel_count = (width * height) as usize;
            let mut data = vec![0u8; pixel_count * 4];

            let lines = GetDIBits(
                hdc_mem,
                hbitmap,
                0,
                height as u32,
                Some(data.as_mut_ptr() as _),
                &mut bmi,
                DIB_RGB_COLORS,
            );

            // 清理 GDI 对象
            SelectObject(hdc_mem, old_obj);
            let _ = DeleteObject(hbitmap.into());
            let _ = DeleteDC(hdc_mem);
            let _ = ReleaseDC(None, hdc_screen);

            if bitblt_ok.is_err() || lines == 0 {
                return Err("读取屏幕位图像素失败".into());
            }

            Ok(CapturedScreen {
                width: width as u32,
                height: height as u32,
                data,
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 验证固态画面创建与裁剪。
    #[test]
    fn screen_model_crop() {
        let screen = CapturedScreen::new_solid(100, 100, (255, 128, 0, 255));
        assert_eq!(screen.width, 100);
        assert_eq!(screen.height, 100);

        let sub = screen.crop(10, 10, 50, 50).unwrap();
        assert_eq!(sub.width, 50);
        assert_eq!(sub.height, 50);
        assert_eq!(sub.data.len(), 50 * 50 * 4);

        // 越界裁剪
        assert!(screen.crop(90, 90, 20, 20).is_none());
        assert!(screen.crop(-1, 0, 10, 10).is_none());
    }

    /// 验证通道转换。
    #[test]
    fn screen_channel_conversion() {
        let screen = CapturedScreen::new_solid(2, 2, (1, 2, 3, 255));
        let rgba = screen.to_rgba();
        assert_eq!(&rgba[0..4], &[1, 2, 3, 255]);
    }
}
