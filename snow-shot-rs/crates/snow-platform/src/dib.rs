//! DIB 解析：把剪贴板里的 CF_DIB 字节解析为自顶向下的 RGBA 像素。

/// DIB 最小头大小 (BITMAPINFOHEADER 大小)
const HEADER_SIZE_MIN: u32 = 40;

/// 无压缩格式 (BI_RGB)
const BI_RGB: u32 = 0;

/// 位段压缩格式 (BI_BITFIELDS)
const BI_BITFIELDS: u32 = 3;

/// 允许的最大宽高
const MAX_DIMENSION: u32 = 32768;

/// 允许的最大像素总数 (宽和高的乘积上限)
const MAX_PIXELS: usize = 256 * 1024 * 1024 / 4;

/// 标准 R 通道掩码
const MASK_R_STANDARD: u32 = 0x00FF_0000;

/// 标准 G 通道掩码
const MASK_G_STANDARD: u32 = 0x0000_FF00;

/// 标准 B 通道掩码
const MASK_B_STANDARD: u32 = 0x0000_00FF;

/// 从字节切片读取小端序 u32
fn read_u32(slice: &[u8]) -> u32 {
    u32::from_le_bytes([slice[0], slice[1], slice[2], slice[3]])
}

/// 从字节切片读取小端序 i32
fn read_i32(slice: &[u8]) -> i32 {
    i32::from_le_bytes([slice[0], slice[1], slice[2], slice[3]])
}

/// 从字节切片读取小端序 u16
fn read_u16(slice: &[u8]) -> u16 {
    u16::from_le_bytes([slice[0], slice[1]])
}

/// 将 DIB 字节数据解析为 RGBA 格式
///
/// 从输入的 CF_DIB 字节流中读取头信息，解析出对应位深和压缩格式的图像，
/// 最终返回自顶向下的 RGBA (R, G, B, A) 数据，并纠正空 alpha 通道的情况。
///
/// # 参数
///
/// * `dib` - 包含 DIB 数据的字节切片，至少应包含 40 字节的信息头
///
/// # 返回
///
/// 成功时返回 `Ok((width, height, rgba_data))`，其中：
/// * `width` - 图像宽度
/// * `height` - 图像高度
/// * `rgba_data` - 自顶向下的 RGBA 像素数据，通道顺序 R,G,B,A，长度等于宽*高*4
///
/// 失败时返回 `Err(String)`，包含错误描述信息。
///
/// # 示例
///
/// ```ignore
/// use snow_platform::dib::dib_to_rgba;
///
/// let dib_bytes = [ /* 你的 DIB 数据 */ ];
/// match dib_to_rgba(&dib_bytes) {
///     Ok((w, h, rgba)) => {
///         println!("解析成功: {}x{}", w, h);
///     }
///     Err(e) => {
///         println!("解析失败: {}", e);
///     }
/// }
/// ```
pub fn dib_to_rgba(dib: &[u8]) -> Result<(u32, u32, Vec<u8>), String> {
    if dib.len() < HEADER_SIZE_MIN as usize {
        return Err("DIB 数据长度不足".into());
    }

    let bi_size = read_u32(&dib[0..4]);
    if bi_size < HEADER_SIZE_MIN {
        return Err("不支持的 biSize".into());
    }

    let width = read_i32(&dib[4..8]);
    let height = read_i32(&dib[8..12]);
    let planes = read_u16(&dib[12..14]);
    let bitcount = read_u16(&dib[14..16]);
    let compression = read_u32(&dib[16..20]);
    let _size_image = read_u32(&dib[20..24]);
    let _xppm = read_i32(&dib[24..28]);
    let _yppm = read_i32(&dib[28..32]);
    let clr_used = read_u32(&dib[32..36]);
    let _clr_important = read_u32(&dib[36..40]);

    if planes != 1 {
        return Err("不支持的 DIB 格式".into());
    }

    if bitcount != 24 && bitcount != 32 {
        return Err("不支持的 DIB 格式".into());
    }

    if compression != BI_RGB && compression != BI_BITFIELDS {
        return Err("不支持的 DIB 格式".into());
    }

    if bitcount == 24 && compression == BI_BITFIELDS {
        return Err("不支持的 DIB 格式".into());
    }

    if compression == BI_BITFIELDS {
        if dib.len() < 52 {
            return Err("不支持的 DIB 格式".into());
        }
        let r_mask = read_u32(&dib[40..44]);
        let g_mask = read_u32(&dib[44..48]);
        let b_mask = read_u32(&dib[48..52]);
        if r_mask != MASK_R_STANDARD || g_mask != MASK_G_STANDARD || b_mask != MASK_B_STANDARD {
            return Err("不支持的 DIB 格式".into());
        }
    }

    if width <= 0 || height == 0 {
        return Err("无效的宽高".into());
    }

    let w_u32 = width as u32;
    let h_u32 = height.unsigned_abs();

    if w_u32 > MAX_DIMENSION || h_u32 > MAX_DIMENSION {
        return Err("宽高超出限制".into());
    }

    let w_usize = w_u32 as usize;
    let h_usize = h_u32 as usize;

    let total_pixels = w_usize.checked_mul(h_usize).ok_or("总像素数计算溢出")?;
    if total_pixels > MAX_PIXELS {
        return Err("总像素数超出限制".into());
    }

    let stride = if bitcount == 24 {
        w_usize
            .checked_mul(3)
            .and_then(|v| v.checked_add(3))
            .map(|v| (v / 4) * 4)
            .ok_or("行跨度计算溢出")?
    } else {
        w_usize.checked_mul(4).ok_or("行跨度计算溢出")?
    };

    let mut pixel_offset = usize::try_from(bi_size).map_err(|_| "biSize 转换错误")?;
    if compression == BI_BITFIELDS {
        if bi_size == HEADER_SIZE_MIN {
            pixel_offset = pixel_offset.checked_add(12).ok_or("像素偏移溢出")?;
        } else if bi_size < 52 {
            return Err("不支持的 DIB 格式".into());
        }
    }

    let clr_used_bytes = usize::try_from(clr_used)
        .map_err(|_| "clrUsed 转换错误")?
        .checked_mul(4)
        .ok_or("调色板大小计算溢出")?;

    pixel_offset = pixel_offset
        .checked_add(clr_used_bytes)
        .ok_or("像素偏移计算溢出")?;

    let total_bytes = stride.checked_mul(h_usize).ok_or("像素字节数计算溢出")?;
    if pixel_offset
        .checked_add(total_bytes)
        .ok_or("总尺寸计算溢出")?
        > dib.len()
    {
        return Err("DIB 数据截断".into());
    }

    let mut rgba = Vec::with_capacity(total_pixels.checked_mul(4).ok_or("预分配容量溢出")?);
    let mut all_alpha_zero = true;
    let is_bottom_up = height > 0;

    for y in 0..h_usize {
        let src_y = if is_bottom_up { h_usize - 1 - y } else { y };
        let row_offset = pixel_offset + src_y * stride;
        let row = &dib[row_offset..row_offset + stride];

        for x in 0..w_usize {
            if bitcount == 32 {
                let px = x * 4;
                let b = row[px];
                let g = row[px + 1];
                let r = row[px + 2];
                let a = row[px + 3];

                if a != 0 {
                    all_alpha_zero = false;
                }

                rgba.push(r);
                rgba.push(g);
                rgba.push(b);
                rgba.push(a);
            } else {
                let px = x * 3;
                let b = row[px];
                let g = row[px + 1];
                let r = row[px + 2];

                rgba.push(r);
                rgba.push(g);
                rgba.push(b);
                rgba.push(255);
            }
        }
    }

    if bitcount == 32 && all_alpha_zero {
        for chunk in rgba.chunks_exact_mut(4) {
            chunk[3] = 255;
        }
    }

    Ok((w_u32, h_u32, rgba))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 构建测试用 DIB 头的辅助函数
    fn build_test_header(
        width: i32,
        height: i32,
        bitcount: u16,
        compression: u32,
        clr_used: u32,
        bi_size: u32,
    ) -> Vec<u8> {
        let mut header = Vec::with_capacity(bi_size as usize);
        header.extend_from_slice(&bi_size.to_le_bytes());
        header.extend_from_slice(&width.to_le_bytes());
        header.extend_from_slice(&height.to_le_bytes());
        header.extend_from_slice(&1u16.to_le_bytes());
        header.extend_from_slice(&bitcount.to_le_bytes());
        header.extend_from_slice(&compression.to_le_bytes());
        header.extend_from_slice(&0u32.to_le_bytes());
        header.extend_from_slice(&0i32.to_le_bytes());
        header.extend_from_slice(&0i32.to_le_bytes());
        header.extend_from_slice(&clr_used.to_le_bytes());
        header.extend_from_slice(&0u32.to_le_bytes());

        while header.len() < bi_size as usize {
            header.push(0);
        }

        header
    }

    /// 32 位自底向上：翻转行序并把 BGRA 转为 RGBA，保留 alpha。
    #[test]
    fn test_32bit_bottom_up() {
        let mut dib = build_test_header(2, 2, 32, BI_RGB, 0, HEADER_SIZE_MIN);
        // 底行
        let row1 = vec![255, 0, 0, 255, 3, 2, 1, 9];
        // 顶行
        let row0 = vec![0, 0, 255, 255, 0, 255, 0, 255];
        dib.extend_from_slice(&row1);
        dib.extend_from_slice(&row0);

        let (w, h, rgba) = dib_to_rgba(&dib).unwrap();
        assert_eq!(w, 2);
        assert_eq!(h, 2);
        let expected = vec![255, 0, 0, 255, 0, 255, 0, 255, 0, 0, 255, 255, 1, 2, 3, 9];
        assert_eq!(rgba, expected);
    }

    /// 24 位：行填充字节被忽略，alpha 恒为 255。
    #[test]
    fn test_24bit_with_padding() {
        // 宽 1，填充字节用非零值以验证被忽略
        let mut dib1 = build_test_header(1, 2, 24, BI_RGB, 0, HEADER_SIZE_MIN);
        dib1.extend_from_slice(&[255, 0, 0, 0xAB]);
        dib1.extend_from_slice(&[0, 255, 0, 0xCD]);

        let (w, h, rgba) = dib_to_rgba(&dib1).unwrap();
        assert_eq!(w, 1);
        assert_eq!(h, 2);
        let expected1 = vec![0, 255, 0, 255, 0, 0, 255, 255];
        assert_eq!(rgba, expected1);

        // 宽 3
        let mut dib2 = build_test_header(3, -1, 24, BI_RGB, 0, HEADER_SIZE_MIN);
        dib2.extend_from_slice(&[255, 0, 0, 0, 255, 0, 0, 0, 255, 0xAB, 0xCD, 0xEF]);

        let (w2, h2, rgba2) = dib_to_rgba(&dib2).unwrap();
        assert_eq!(w2, 3);
        assert_eq!(h2, 1);
        let expected2 = vec![0, 0, 255, 255, 0, 255, 0, 255, 255, 0, 0, 255];
        assert_eq!(rgba2, expected2);
    }

    /// 负高度表示自顶向下，不翻转。
    #[test]
    fn test_negative_height() {
        let mut dib = build_test_header(1, -2, 32, BI_RGB, 0, HEADER_SIZE_MIN);
        dib.extend_from_slice(&[255, 0, 0, 255]);
        dib.extend_from_slice(&[0, 255, 0, 255]);

        let (w, h, rgba) = dib_to_rgba(&dib).unwrap();
        assert_eq!(w, 1);
        assert_eq!(h, 2);
        let expected = vec![0, 0, 255, 255, 0, 255, 0, 255];
        assert_eq!(rgba, expected);
    }

    /// 32 位且 alpha 全为 0 时视为无 alpha，补成 255。
    #[test]
    fn test_all_zero_alpha() {
        let mut dib = build_test_header(2, -1, 32, BI_RGB, 0, HEADER_SIZE_MIN);
        dib.extend_from_slice(&[255, 0, 0, 0, 0, 255, 0, 0]);

        let (_, _, rgba) = dib_to_rgba(&dib).unwrap();
        let expected = vec![0, 0, 255, 255, 0, 255, 0, 255];
        assert_eq!(rgba, expected);
    }

    /// 含非零 alpha 时原样保留。
    #[test]
    fn test_mixed_alpha() {
        let mut dib = build_test_header(2, -1, 32, BI_RGB, 0, HEADER_SIZE_MIN);
        dib.extend_from_slice(&[255, 0, 0, 0, 0, 255, 0, 100]);

        let (_, _, rgba) = dib_to_rgba(&dib).unwrap();
        let expected = vec![0, 0, 255, 0, 0, 255, 0, 100];
        assert_eq!(rgba, expected);
    }

    /// BI_BITFIELDS 标准掩码可解析。
    #[test]
    fn test_bitfields_standard_mask() {
        let mut dib = build_test_header(1, -1, 32, BI_BITFIELDS, 0, HEADER_SIZE_MIN);
        dib.extend_from_slice(&MASK_R_STANDARD.to_le_bytes());
        dib.extend_from_slice(&MASK_G_STANDARD.to_le_bytes());
        dib.extend_from_slice(&MASK_B_STANDARD.to_le_bytes());
        dib.extend_from_slice(&[255, 0, 0, 100]);

        assert!(dib_to_rgba(&dib).is_ok());
    }

    /// BI_BITFIELDS 非标准掩码被拒绝。
    #[test]
    fn test_bitfields_non_standard_mask() {
        let mut dib = build_test_header(1, -1, 32, BI_BITFIELDS, 0, HEADER_SIZE_MIN);
        dib.extend_from_slice(&0xFF00_0000u32.to_le_bytes());
        dib.extend_from_slice(&MASK_R_STANDARD.to_le_bytes());
        dib.extend_from_slice(&MASK_G_STANDARD.to_le_bytes());
        dib.extend_from_slice(&[255, 0, 0, 100]);

        assert!(dib_to_rgba(&dib).is_err());
    }

    /// 像素数据被截断时返回错误。
    #[test]
    fn test_truncation() {
        let mut dib = build_test_header(1, -1, 32, BI_RGB, 0, HEADER_SIZE_MIN);
        dib.extend_from_slice(&[255, 0, 0]);

        assert!(dib_to_rgba(&dib).is_err());
    }

    /// 宽或高为 0 时返回错误。
    #[test]
    fn test_invalid_width_height() {
        let dib1 = build_test_header(0, 1, 32, BI_RGB, 0, HEADER_SIZE_MIN);
        assert!(dib_to_rgba(&dib1).is_err());

        let dib2 = build_test_header(1, 0, 32, BI_RGB, 0, HEADER_SIZE_MIN);
        assert!(dib_to_rgba(&dib2).is_err());
    }

    /// 不支持的位深（16 位）被拒绝。
    #[test]
    fn test_invalid_bitcount() {
        let dib = build_test_header(1, 1, 16, BI_RGB, 0, HEADER_SIZE_MIN);
        assert!(dib_to_rgba(&dib).is_err());
    }

    /// 超大宽高返回错误而不是 panic。
    #[test]
    fn test_i32_max_dimensions() {
        let dib = build_test_header(i32::MAX, i32::MAX, 32, BI_RGB, 0, HEADER_SIZE_MIN);
        assert!(dib_to_rgba(&dib).is_err());
    }

    /// 高度为 i32::MIN 时返回错误而不是 panic。
    #[test]
    fn test_i32_min_height() {
        let dib = build_test_header(1, i32::MIN, 32, BI_RGB, 0, HEADER_SIZE_MIN);
        assert!(dib_to_rgba(&dib).is_err());
    }
}
