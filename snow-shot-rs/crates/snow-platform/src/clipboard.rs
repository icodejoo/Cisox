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

/// 图像里是否存在不完全不透明的像素（需要走带 alpha 的剪贴板格式）。
///
/// # 参数
/// - `rgba`：RGBA 像素，长度应为 4 的倍数。
///
/// # 返回
/// 任一像素 alpha 小于 255 时为 `true`。
///
/// ```
/// use snow_platform::clipboard::has_transparency;
/// assert!(!has_transparency(&[1, 2, 3, 255]));
/// assert!(has_transparency(&[1, 2, 3, 0]));
/// ```
pub fn has_transparency(rgba: &[u8]) -> bool {
    rgba.chunks_exact(BYTES_PER_PIXEL)
        .any(|px| px[3] != u8::MAX)
}

/// 把带透明通道的图像写入系统剪贴板：同时放 `CF_DIBV5`（带 alpha 的位图）与已编码的 `PNG`。
///
/// 旧的 `CF_DIB` 会被多数程序忽略 alpha，透明区会露出底色；`PNG` 是浏览器、Office、
/// 聊天软件等普遍读取的带透明格式。
///
/// # 参数
/// - `width` / `height`：图像尺寸。
/// - `rgba_pixels`：RGBA 像素。
/// - `png`：同一图像编码好的 PNG 字节（由调用方编码）。
///
/// # 返回
/// 成功返回 `Ok(())`，失败返回错误说明。
///
/// # 示例
/// ```no_run
/// use snow_platform::clipboard::copy_image_with_png_to_clipboard;
/// let png = vec![0u8; 8];
/// copy_image_with_png_to_clipboard(1, 1, &[0, 0, 0, 0], &png).ok();
/// ```
pub fn copy_image_with_png_to_clipboard(
    width: u32,
    height: u32,
    rgba_pixels: &[u8],
    png: &[u8],
) -> Result<(), String> {
    if expected_rgba_len(width, height) != Some(rgba_pixels.len()) {
        return Err("图像数据长度与尺寸不匹配".into());
    }

    #[cfg(windows)]
    {
        win32_clipboard::copy_image_with_png_win32(width, height, rgba_pixels, png)
    }

    #[cfg(not(windows))]
    {
        let _ = (width, height, rgba_pixels, png);
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

/// `BITMAPV5HEADER` 字节数。
#[cfg_attr(not(windows), allow(dead_code))]
const DIBV5_HEADER_SIZE: usize = 124;

/// 由 RGBA 像素构造 CF_DIBV5 数据（`BITMAPV5HEADER` + 自底向上、非预乘的 BGRA 像素，带 alpha 掩码）。
///
/// 长度不匹配或尺寸溢出时返回 `None`。
#[cfg_attr(not(windows), allow(dead_code))]
fn build_dibv5(width: u32, height: u32, rgba: &[u8]) -> Option<Vec<u8>> {
    let image_size = expected_rgba_len(width, height).filter(|&n| n == rgba.len())?;
    let size_image = u32::try_from(image_size).ok()?;
    let width_i32 = i32::try_from(width).ok()?;
    let height_i32 = i32::try_from(height).ok()?;
    let row_stride = width as usize * BYTES_PER_PIXEL;
    let mut out = Vec::with_capacity(DIBV5_HEADER_SIZE.checked_add(image_size)?);
    out.extend_from_slice(&(DIBV5_HEADER_SIZE as u32).to_le_bytes());
    out.extend_from_slice(&width_i32.to_le_bytes());
    out.extend_from_slice(&height_i32.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&32u16.to_le_bytes());
    out.extend_from_slice(&3u32.to_le_bytes()); // BI_BITFIELDS
    out.extend_from_slice(&size_image.to_le_bytes());
    out.extend_from_slice(&[0u8; 16]); // 分辨率、调色板字段
    out.extend_from_slice(&0x00FF_0000u32.to_le_bytes()); // 红色掩码
    out.extend_from_slice(&0x0000_FF00u32.to_le_bytes()); // 绿色掩码
    out.extend_from_slice(&0x0000_00FFu32.to_le_bytes()); // 蓝色掩码
    out.extend_from_slice(&0xFF00_0000u32.to_le_bytes()); // alpha 掩码
    out.extend_from_slice(&0x7352_4742u32.to_le_bytes()); // LCS_sRGB
    out.extend_from_slice(&[0u8; 36]); // CIEXYZTRIPLE
    out.extend_from_slice(&[0u8; 12]); // 三个伽马
    out.extend_from_slice(&4u32.to_le_bytes()); // LCS_GM_IMAGES
    out.extend_from_slice(&[0u8; 12]); // 配置文件偏移 / 大小 / 保留
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
    use super::{build_dib, build_dibv5};
    use std::time::Duration;
    use windows::Win32::Foundation::{GlobalFree, HANDLE, HGLOBAL};
    use windows::Win32::System::DataExchange::{
        CloseClipboard, EmptyClipboard, GetClipboardData, IsClipboardFormatAvailable,
        OpenClipboard, RegisterClipboardFormatW, SetClipboardData,
    };
    use windows::Win32::System::Memory::{
        GMEM_MOVEABLE, GlobalAlloc, GlobalLock, GlobalSize, GlobalUnlock,
    };

    /// 剪贴板格式：设备无关位图。
    const CF_DIB: u32 = 8;
    /// 剪贴板格式：带 alpha 的 V5 位图。
    const CF_DIBV5: u32 = 17;
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

    /// 一次性写入多种格式：打开、清空、依次设置；任一失败返回错误（已设置的格式保留）。
    fn set_clipboard_multi(items: &[(u32, &[u8], &str)]) -> Result<(), String> {
        let mut blocks = Vec::with_capacity(items.len());
        for (_, bytes, _) in items {
            blocks.push(GlobalBlock::from_bytes(bytes)?);
        }
        open_clipboard_with_retry()?;
        let mut outcome = Ok(());
        unsafe {
            let _ = EmptyClipboard();
            for ((format, _, label), block) in items.iter().zip(blocks.iter_mut()) {
                let handle = block.0.expect("句柄有效");
                if SetClipboardData(*format, Some(HANDLE(handle.0))).is_ok() {
                    block.release_ownership();
                } else {
                    outcome = Err(format!("SetClipboardData {label} 失败"));
                    break;
                }
            }
            let _ = CloseClipboard();
        }
        outcome
    }

    /// 将带透明通道的图像以 CF_DIBV5 + PNG 写入 Win32 剪贴板。
    pub fn copy_image_with_png_win32(
        width: u32,
        height: u32,
        rgba: &[u8],
        png: &[u8],
    ) -> Result<(), String> {
        let dib = build_dibv5(width, height, rgba).ok_or("图像尺寸无效")?;
        let png_format = unsafe { RegisterClipboardFormatW(windows::core::w!("PNG")) };
        if png_format == 0 {
            return Err("注册 PNG 剪贴板格式失败".into());
        }
        set_clipboard_multi(&[(CF_DIBV5, &dib, "CF_DIBV5"), (png_format, png, "PNG")])
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
            let handle =
                GetClipboardData(format).map_err(|e| format!("GetClipboardData 失败: {e}"))?;
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

        /// 真实剪贴板往返：写入带透明的图后，CF_DIBV5 与 PNG 两种格式都可用且 PNG 内容原样。
        /// 会覆盖用户剪贴板，默认忽略；需要时用 `--ignored` 手动跑。
        #[test]
        #[ignore = "会覆盖用户的剪贴板"]
        fn real_clipboard_has_dibv5_and_png() {
            let rgba = [10u8, 20, 30, 0, 40, 50, 60, 255];
            let png = vec![137u8, 80, 78, 71, 1, 2, 3, 4];
            copy_image_with_png_win32(2, 1, &rgba, &png).unwrap();
            let png_format = unsafe { RegisterClipboardFormatW(windows::core::w!("PNG")) };
            open_clipboard_with_retry().unwrap();
            let (has_v5, has_png, png_back) = unsafe {
                let has_v5 = IsClipboardFormatAvailable(CF_DIBV5).is_ok();
                let has_png = IsClipboardFormatAvailable(png_format).is_ok();
                let back = has_png
                    .then(|| read_global_bytes(png_format).ok().flatten())
                    .flatten();
                let _ = CloseClipboard();
                (has_v5, has_png, back)
            };
            assert!(has_v5 && has_png);
            // 全局内存大小可能按分配粒度取整，只比较前缀
            assert!(png_back.is_some_and(|bytes| bytes.starts_with(&png)));
        }

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
        assert!(build_dibv5(u32::MAX, u32::MAX, &[0u8; 4]).is_none());
        assert!(copy_image_with_png_to_clipboard(u32::MAX, u32::MAX, &[0u8; 4], &[]).is_err());
    }

    /// 透明检测：全不透明为 false，任意一个半透明 / 透明像素为 true，空缓冲为 false。
    #[test]
    fn transparency_detection() {
        assert!(!has_transparency(&[]));
        assert!(!has_transparency(&[1, 2, 3, 255, 4, 5, 6, 255]));
        assert!(has_transparency(&[1, 2, 3, 255, 4, 5, 6, 254]));
    }

    /// DIBV5 头：124 字节、BI_BITFIELDS、alpha 掩码在最高字节；像素自底向上且保留非预乘 alpha。
    #[test]
    fn build_dibv5_header_and_pixels() {
        let rgba = [
            255, 0, 0, 255, 0, 255, 0, 128, //
            0, 0, 255, 0, 1, 2, 3, 9,
        ];
        let dib = build_dibv5(2, 2, &rgba).unwrap();
        assert_eq!(dib.len(), DIBV5_HEADER_SIZE + 16);
        let word = |at: usize| u32::from_le_bytes(dib[at..at + 4].try_into().unwrap());
        assert_eq!(word(0), 124);
        assert_eq!(word(16), 3, "BI_BITFIELDS");
        assert_eq!(word(40), 0x00FF_0000);
        assert_eq!(word(52), 0xFF00_0000);
        assert_eq!(word(56), 0x7352_4742, "sRGB");
        let px = &dib[DIBV5_HEADER_SIZE..];
        // 自底向上：先写原第 1 行；BGRA
        assert_eq!(&px[0..4], &[255, 0, 0, 0]);
        assert_eq!(&px[4..8], &[3, 2, 1, 9]);
        assert_eq!(&px[8..12], &[0, 0, 255, 255]);
        assert_eq!(&px[12..16], &[0, 255, 0, 128]);
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
