//! 系统剪贴板服务（Platform Clipboard Service）。
//!
//! 提供图像与纯文本的原生系统剪贴板写入能力。在 Windows 平台通过
//! Win32 Clipboard API 实现高兼容性 DIB 位图与 Unicode 文本传输。

/// 将 RGBA 或 BGRA 图像写入系统剪贴板。
///
/// # 参数
/// - `width`: 宽度。
/// - `height`: 高度。
/// - `rgba_pixels`: 32 位 RGBA 像素切片（长度须为 width * height * 4）。
///
/// # 返回
/// 成功返回 `Ok(())`，失败返回错误说明。
///
/// # 示例
/// ```no_run
/// use snow_platform::clipboard::copy_image_to_clipboard;
/// let img = vec![255u8; 100 * 100 * 4];
/// copy_image_to_clipboard(100, 100, &img).ok();
/// ```
pub fn copy_image_to_clipboard(width: u32, height: u32, rgba_pixels: &[u8]) -> Result<(), String> {
    if (width * height * 4) as usize != rgba_pixels.len() {
        return Err("图像数据长度与尺寸不匹配".into());
    }

    #[cfg(windows)]
    {
        win32_clipboard::copy_image_win32(width, height, rgba_pixels)
    }

    #[cfg(not(windows))]
    {
        let _ = (width, height, rgba_pixels);
        Ok(())
    }
}

/// 将纯文本写入系统剪贴板。
///
/// # 参数
/// - `text`: 待写入的字符串切片。
///
/// # 返回
/// 成功返回 `Ok(())`，失败返回错误说明。
///
/// # 示例
/// ```no_run
/// use snow_platform::clipboard::copy_text_to_clipboard;
/// copy_text_to_clipboard("#1677FF").ok();
/// ```
pub fn copy_text_to_clipboard(text: &str) -> Result<(), String> {
    #[cfg(windows)]
    {
        win32_clipboard::copy_text_win32(text)
    }

    #[cfg(not(windows))]
    {
        let _ = text;
        Ok(())
    }
}

#[cfg(windows)]
mod win32_clipboard {
    use std::ptr;
    use windows::Win32::Graphics::Gdi::{BITMAPINFOHEADER, BI_RGB};
    use windows::Win32::System::Memory::{
        GlobalAlloc, GlobalLock, GlobalUnlock, GLOBAL_ALLOC_FLAGS, GMEM_MOVEABLE,
    };
    use windows::Win32::System::DataExchange::{
        CloseClipboard, EmptyClipboard, OpenClipboard, SetClipboardData,
    };

    const CF_DIB: u32 = 8;
    const CF_UNICODETEXT: u32 = 13;

    /// 将图像以 CF_DIB 格式写入 Win32 剪贴板。
    pub fn copy_image_win32(width: u32, height: u32, rgba: &[u8]) -> Result<(), String> {
        unsafe {
            let header_size = std::mem::size_of::<BITMAPINFOHEADER>();
            // DIB 要求每行扫描线 4 字节对齐（32bpp 天然对齐）
            let row_stride = (width * 4) as usize;
            let image_bytes_size = row_stride * (height as usize);
            let total_size = header_size + image_bytes_size;

            let h_mem = GlobalAlloc(GLOBAL_ALLOC_FLAGS(GMEM_MOVEABLE.0), total_size)
                .map_err(|e| format!("GlobalAlloc 失败: {e}"))?;

            let ptr = GlobalLock(h_mem) as *mut u8;
            if ptr.is_null() {
                return Err("GlobalLock 失败".into());
            }

            // 写入 BITMAPINFOHEADER
            let header = BITMAPINFOHEADER {
                biSize: header_size as u32,
                biWidth: width as i32,
                // 正高表示自底向上（传统 DIB 格式兼容性最佳）
                biHeight: height as i32,
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB.0,
                biSizeImage: image_bytes_size as u32,
                biXPelsPerMeter: 0,
                biYPelsPerMeter: 0,
                biClrUsed: 0,
                biClrImportant: 0,
            };
            ptr::copy_nonoverlapping(&header as *const _ as *const u8, ptr, header_size);

            let pixels_dest = ptr.add(header_size);

            // 自底向上翻转并转为 BGRA
            for row in 0..height {
                let src_row = (height - 1 - row) as usize;
                let src_offset = src_row * row_stride;
                let dest_offset = (row as usize) * row_stride;

                for col in 0..width as usize {
                    let s = src_offset + col * 4;
                    let d = dest_offset + col * 4;
                    let r = rgba[s];
                    let g = rgba[s + 1];
                    let b = rgba[s + 2];
                    let a = rgba[s + 3];

                    *pixels_dest.add(d) = b;
                    *pixels_dest.add(d + 1) = g;
                    *pixels_dest.add(d + 2) = r;
                    *pixels_dest.add(d + 3) = a;
                }
            }

            let _ = GlobalUnlock(h_mem);

            if OpenClipboard(None).is_err() {
                return Err("OpenClipboard 失败".into());
            }
            let _ = EmptyClipboard();
            let res = SetClipboardData(CF_DIB, Some(windows::Win32::Foundation::HANDLE(h_mem.0)));
            let _ = CloseClipboard();

            if res.is_err() {
                return Err("SetClipboardData CF_DIB 失败".into());
            }

            Ok(())
        }
    }

    /// 将文本以 CF_UNICODETEXT 写入 Win32 剪贴板。
    pub fn copy_text_win32(text: &str) -> Result<(), String> {
        unsafe {
            let wide: Vec<u16> = text.encode_utf16().chain(std::iter::once(0)).collect();
            let total_size = wide.len() * 2;

            let h_mem = GlobalAlloc(GLOBAL_ALLOC_FLAGS(GMEM_MOVEABLE.0), total_size)
                .map_err(|e| format!("GlobalAlloc 失败: {e}"))?;

            let ptr = GlobalLock(h_mem) as *mut u16;
            if ptr.is_null() {
                return Err("GlobalLock 失败".into());
            }

            ptr::copy_nonoverlapping(wide.as_ptr(), ptr, wide.len());
            let _ = GlobalUnlock(h_mem);

            if OpenClipboard(None).is_err() {
                return Err("OpenClipboard 失败".into());
            }
            let _ = EmptyClipboard();
            let res = SetClipboardData(CF_UNICODETEXT, Some(windows::Win32::Foundation::HANDLE(h_mem.0)));
            let _ = CloseClipboard();

            if res.is_err() {
                return Err("SetClipboardData CF_UNICODETEXT 失败".into());
            }

            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 校验数据有效性检查。
    #[test]
    fn invalid_buffer_size_rejected() {
        let bad_buf = vec![0u8; 10];
        assert!(copy_image_to_clipboard(10, 10, &bad_buf).is_err());
    }
}
