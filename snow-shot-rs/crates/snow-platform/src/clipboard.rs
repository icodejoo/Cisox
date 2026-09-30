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
    if expected_rgba_len(width, height) != Some(rgba_pixels.len()) {
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

/// 读取系统剪贴板里的图像（CF_DIB，系统会自动从 CF_BITMAP 等格式合成）。
///
/// # 返回
/// - `Ok(Some((宽, 高, RGBA)))`：剪贴板里有可解析的图像。
/// - `Ok(None)`：剪贴板里没有图像。
/// - `Err(..)`：剪贴板无法打开，或图像格式不受支持 / 数据损坏。
///
/// # 示例
/// ```no_run
/// use snow_platform::clipboard::read_image_from_clipboard;
/// if let Ok(Some((w, h, rgba))) = read_image_from_clipboard() {
///     assert_eq!(rgba.len(), (w * h * 4) as usize);
/// }
/// ```
pub fn read_image_from_clipboard() -> Result<Option<(u32, u32, Vec<u8>)>, String> {
    #[cfg(windows)]
    {
        match win32_clipboard::read_dib_win32()? {
            Some(dib) => crate::dib::dib_to_rgba(&dib).map(Some),
            None => Ok(None),
        }
    }

    #[cfg(not(windows))]
    {
        Ok(None)
    }
}

/// BITMAPINFOHEADER 固定字节数。
const DIB_HEADER_SIZE: usize = 40;
/// 每像素字节数（32bpp）。
const BYTES_PER_PIXEL: usize = 4;

/// 计算 RGBA 缓冲的期望字节数；乘法溢出时返回 `None`。
fn expected_rgba_len(width: u32, height: u32) -> Option<usize> {
    (width as usize)
        .checked_mul(height as usize)?
        .checked_mul(BYTES_PER_PIXEL)
}

/// 由 RGBA 像素构造 CF_DIB 数据（BITMAPINFOHEADER + 自底向上的 BGRA 像素）。
///
/// 长度不匹配或尺寸溢出时返回 `None`。
#[cfg_attr(not(windows), allow(dead_code))]
fn build_dib(width: u32, height: u32, rgba: &[u8]) -> Option<Vec<u8>> {
    let image_size = expected_rgba_len(width, height).filter(|&n| n == rgba.len())?;
    let size_image = u32::try_from(image_size).ok()?;
    let width_i32 = i32::try_from(width).ok()?;
    let height_i32 = i32::try_from(height).ok()?;
    let row_stride = width as usize * BYTES_PER_PIXEL;
    let mut out = Vec::with_capacity(DIB_HEADER_SIZE.checked_add(image_size)?);
    out.extend_from_slice(&(DIB_HEADER_SIZE as u32).to_le_bytes());
    out.extend_from_slice(&width_i32.to_le_bytes());
    // 正高表示自底向上（传统 DIB 格式兼容性最佳）
    out.extend_from_slice(&height_i32.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&32u16.to_le_bytes());
    out.extend_from_slice(&0u32.to_le_bytes()); // BI_RGB
    out.extend_from_slice(&size_image.to_le_bytes());
    out.extend_from_slice(&[0u8; 16]); // 分辨率与调色板字段
    if row_stride > 0 {
        for row in rgba.chunks_exact(row_stride).rev() {
            for px in row.chunks_exact(BYTES_PER_PIXEL) {
                out.extend_from_slice(&[px[2], px[1], px[0], px[3]]);
            }
        }
    }
    Some(out)
}

#[cfg(windows)]
mod win32_clipboard {
    use super::build_dib;
    use std::time::Duration;
    use windows::Win32::Foundation::{GlobalFree, HANDLE, HGLOBAL};
    use windows::Win32::System::DataExchange::{
        CloseClipboard, EmptyClipboard, GetClipboardData, IsClipboardFormatAvailable,
        OpenClipboard, SetClipboardData,
    };
    use windows::Win32::System::Memory::{
        GMEM_MOVEABLE, GlobalAlloc, GlobalLock, GlobalSize, GlobalUnlock,
    };

    /// 剪贴板格式：设备无关位图。
    const CF_DIB: u32 = 8;
    /// 剪贴板格式：UTF-16 文本。
    const CF_UNICODETEXT: u32 = 13;
    /// 打开剪贴板的最大尝试次数（剪贴板常被其他进程短暂占用）。
    const OPEN_ATTEMPTS: u32 = 8;
    /// 两次尝试之间的等待毫秒数。
    const OPEN_RETRY_MS: u64 = 15;

    /// 全局内存句柄的 RAII 包装：未转交系统前 Drop 时自动释放。
    struct GlobalBlock(Option<HGLOBAL>);

    impl GlobalBlock {
        /// 分配并写入字节，写入完成后已解锁。
        fn from_bytes(bytes: &[u8]) -> Result<Self, String> {
            unsafe {
                let handle = GlobalAlloc(GMEM_MOVEABLE, bytes.len().max(1))
                    .map_err(|e| format!("GlobalAlloc 失败: {e}"))?;
                let block = Self(Some(handle));
                let dest = GlobalLock(handle) as *mut u8;
                if dest.is_null() {
                    return Err("GlobalLock 失败".into());
                }
                std::ptr::copy_nonoverlapping(bytes.as_ptr(), dest, bytes.len());
                let _ = GlobalUnlock(handle);
                Ok(block)
            }
        }

        /// 转交所有权给系统（`SetClipboardData` 成功后调用），此后 Drop 不再释放。
        fn release_ownership(&mut self) -> HGLOBAL {
            self.0.take().expect("句柄已转交")
        }
    }

    impl Drop for GlobalBlock {
        /// 未转交时释放全局内存。
        fn drop(&mut self) {
            if let Some(handle) = self.0.take() {
                unsafe {
                    let _ = GlobalFree(Some(handle));
                }
            }
        }
    }

    /// 带重试地打开剪贴板。
    fn open_clipboard_with_retry() -> Result<(), String> {
        for attempt in 0..OPEN_ATTEMPTS {
            if unsafe { OpenClipboard(None) }.is_ok() {
                return Ok(());
            }
            if attempt + 1 < OPEN_ATTEMPTS {
                std::thread::sleep(Duration::from_millis(OPEN_RETRY_MS));
            }
        }
        Err("OpenClipboard 失败".into())
    }

    /// 数据已备好后写入剪贴板：打开、清空、设置；失败时释放句柄。
    fn set_clipboard(format: u32, bytes: &[u8], label: &str) -> Result<(), String> {
        let mut block = GlobalBlock::from_bytes(bytes)?;
        open_clipboard_with_retry()?;
        let result = unsafe {
            let _ = EmptyClipboard();
            let handle = block.0.expect("句柄有效");
            let set = SetClipboardData(format, Some(HANDLE(handle.0)));
            if set.is_ok() {
                block.release_ownership();
            }
            let _ = CloseClipboard();
            set
        };
        result
            .map(|_| ())
            .map_err(|_| format!("SetClipboardData {label} 失败"))
    }

    /// 将图像以 CF_DIB 格式写入 Win32 剪贴板。
    pub fn copy_image_win32(width: u32, height: u32, rgba: &[u8]) -> Result<(), String> {
        let dib = build_dib(width, height, rgba).ok_or("图像尺寸无效")?;
        set_clipboard(CF_DIB, &dib, "CF_DIB")
    }

    /// 读取剪贴板里的 CF_DIB 原始字节；没有该格式返回 `None`。
    pub fn read_dib_win32() -> Result<Option<Vec<u8>>, String> {
        open_clipboard_with_retry()?;
        // SAFETY: 剪贴板已在上面打开，读取完成后立即关闭。
        unsafe {
            let out = if IsClipboardFormatAvailable(CF_DIB).is_ok() {
                read_global_bytes(CF_DIB)
            } else {
                Ok(None)
            };
            let _ = CloseClipboard();
            out
        }
    }

    /// 在剪贴板已打开的前提下读取指定格式的全局内存字节。
    ///
    /// # Safety
    /// 调用方必须已成功 `OpenClipboard`，且在本函数返回前不关闭剪贴板。
    unsafe fn read_global_bytes(format: u32) -> Result<Option<Vec<u8>>, String> {
        // SAFETY: 剪贴板已由调用方打开；句柄归系统所有，只读拷贝后立即解锁。
        unsafe {
            let handle = GetClipboardData(format).map_err(|e| format!("GetClipboardData 失败: {e}"))?;
            let global = HGLOBAL(handle.0);
            let size = GlobalSize(global);
            if size == 0 {
                return Ok(None);
            }
            let src = GlobalLock(global) as *const u8;
            if src.is_null() {
                return Err("GlobalLock 失败".into());
            }
            let bytes = std::slice::from_raw_parts(src, size).to_vec();
            let _ = GlobalUnlock(global);
            Ok(Some(bytes))
        }
    }

    /// 将文本以 CF_UNICODETEXT 写入 Win32 剪贴板。
    pub fn copy_text_win32(text: &str) -> Result<(), String> {
        let bytes: Vec<u8> = text
            .encode_utf16()
            .chain(std::iter::once(0))
            .flat_map(u16::to_le_bytes)
            .collect();
        set_clipboard(CF_UNICODETEXT, &bytes, "CF_UNICODETEXT")
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        /// 未转交的句柄 Drop 时可正常释放，转交后不再释放。
        #[test]
        fn global_block_drop_and_release() {
            drop(GlobalBlock::from_bytes(&[1, 2, 3]).unwrap());
            let mut block = GlobalBlock::from_bytes(&[]).unwrap();
            let handle = block.release_ownership();
            assert!(block.0.is_none());
            unsafe {
                let _ = GlobalFree(Some(handle));
            }
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

    /// 巨大尺寸不会因乘法溢出 panic 或误通过校验。
    #[test]
    fn huge_dimensions_do_not_overflow() {
        assert!(copy_image_to_clipboard(u32::MAX, u32::MAX, &[0u8; 4]).is_err());
        assert!(build_dib(u32::MAX, u32::MAX, &[0u8; 4]).is_none());
    }

    /// DIB 头正确，像素被翻转行序并转为 BGRA。
    #[test]
    fn build_dib_flips_rows_and_swaps_channels() {
        // 2x2：第 0 行 R,G；第 1 行 B,W(alpha=9)
        let rgba = [
            255, 0, 0, 255, 0, 255, 0, 255, //
            0, 0, 255, 255, 1, 2, 3, 9,
        ];
        let dib = build_dib(2, 2, &rgba).unwrap();
        assert_eq!(dib.len(), DIB_HEADER_SIZE + 16);
        assert_eq!(u32::from_le_bytes(dib[0..4].try_into().unwrap()), 40);
        assert_eq!(i32::from_le_bytes(dib[8..12].try_into().unwrap()), 2);
        let px = &dib[DIB_HEADER_SIZE..];
        // 自底向上：先写原第 1 行
        assert_eq!(&px[0..4], &[255, 0, 0, 255]); // B(0,0,255) -> BGRA
        assert_eq!(&px[4..8], &[3, 2, 1, 9]);
        assert_eq!(&px[8..12], &[0, 0, 255, 255]); // R -> BGRA
    }
}
