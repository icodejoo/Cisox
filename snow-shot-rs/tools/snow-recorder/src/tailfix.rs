//! 动画 WebP / APNG 尾段时长修补。
//!
//! FFmpeg 的 `libwebp_anim` 与 APNG 复用器不会把“最后一帧持续到录制结束”写进容器：
//! 最后一帧时长被取成前面各帧的平均值（WebP）或上一帧延迟（APNG），静止的尾段整段丢失。
//! 这里在成品发布前直接改写容器里最后一帧的时长，使总时长等于有效录制时长。

use std::path::Path;

use snow_recorder_protocol::MediaFormat;

/// WebP 时长字段最大值（24 位毫秒）。
const WEBP_MAX_DURATION_MS: u64 = 0x00FF_FFFF;
/// RIFF 头长度（`RIFF` + 大小 + `WEBP`）。
const RIFF_HEADER_LEN: usize = 12;
/// RIFF / PNG 块头长度（4 字节标识 + 4 字节长度）。
const CHUNK_HEADER_LEN: usize = 8;
/// ANMF 负载里时长字段的偏移（X/Y/宽/高各 3 字节之后）。
const ANMF_DURATION_OFFSET: usize = 12;
/// 时长字段字节数（24 位）。
const ANMF_DURATION_LEN: usize = 3;
/// PNG 文件签名。
const PNG_SIGNATURE: [u8; 8] = [0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];
/// fcTL 负载长度。
const FCTL_DATA_LEN: usize = 26;
/// fcTL 负载里 `delay_num` 的偏移。
const FCTL_DELAY_OFFSET: usize = 20;
/// PNG 块尾 CRC 长度。
const PNG_CRC_LEN: usize = 4;
/// 每秒毫秒数。
const MS_PER_SEC: u64 = 1000;
/// APNG `delay_den` 为 0 时按 100 处理（规范约定）。
const APNG_DEFAULT_DEN: u64 = 100;
/// APNG 改写延迟时依次尝试的分母，越靠前精度越高。
const APNG_DEN_CANDIDATES: [u64; 4] = [1000, 100, 10, 1];
/// CRC-32 多项式（反射形式）。
const CRC32_POLY: u32 = 0xEDB8_8320;

/// 修补失败原因（文件保持原样）。
#[derive(Debug, PartialEq, Eq)]
pub enum TailError {
    /// 容器结构损坏或不是预期格式。
    Malformed(&'static str),
    /// 读写文件失败。
    Io(String),
}

impl std::fmt::Display for TailError {
    /// 输出可读的失败原因。
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Malformed(why) => write!(f, "动图容器异常: {why}"),
            Self::Io(why) => write!(f, "读写动图失败: {why}"),
        }
    }
}

/// 一次修补的结果。
#[derive(Debug, PartialEq, Eq)]
pub struct TailReport {
    /// 修补前各帧时长之和（毫秒）。
    pub before_ms: u64,
    /// 修补后各帧时长之和（毫秒）。
    pub after_ms: u64,
}

/// 读取小端 24 位整数。
fn read_u24(bytes: &[u8]) -> u64 {
    bytes.iter().rev().fold(0u64, |acc, b| (acc << 8) | u64::from(*b))
}

/// 读取小端 32 位块长度；不足 4 字节返回 `None`。
fn read_u32_le(bytes: &[u8]) -> Option<usize> {
    let raw: [u8; 4] = bytes.get(..4)?.try_into().ok()?;
    usize::try_from(u32::from_le_bytes(raw)).ok()
}

/// 读取大端 32 位块长度；不足 4 字节返回 `None`。
fn read_u32_be(bytes: &[u8]) -> Option<usize> {
    let raw: [u8; 4] = bytes.get(..4)?.try_into().ok()?;
    usize::try_from(u32::from_be_bytes(raw)).ok()
}

/// 计算 CRC-32（PNG 块校验）。
///
/// # 参数
/// - `data`：参与校验的字节（块类型 + 块数据）。
fn crc32(data: &[u8]) -> u32 {
    let mut crc = u32::MAX;
    for byte in data {
        crc ^= u32::from(*byte);
        for _ in 0..8 {
            crc = if crc & 1 != 0 { (crc >> 1) ^ CRC32_POLY } else { crc >> 1 };
        }
    }
    !crc
}

/// 遍历 WebP 的 ANMF 块，返回每帧时长字段在文件中的偏移与毫秒值。
fn webp_frames(bytes: &[u8]) -> Result<Vec<(usize, u64)>, TailError> {
    if bytes.len() < RIFF_HEADER_LEN || &bytes[..4] != b"RIFF" || &bytes[8..12] != b"WEBP" {
        return Err(TailError::Malformed("不是 RIFF/WEBP"));
    }
    let mut frames = Vec::new();
    let mut pos = RIFF_HEADER_LEN;
    while pos + CHUNK_HEADER_LEN <= bytes.len() {
        let len = read_u32_le(&bytes[pos + 4..]).ok_or(TailError::Malformed("块长度"))?;
        let payload = pos + CHUNK_HEADER_LEN;
        let end = payload.checked_add(len).ok_or(TailError::Malformed("块长度溢出"))?;
        if end > bytes.len() {
            return Err(TailError::Malformed("块越界"));
        }
        if &bytes[pos..pos + 4] == b"ANMF" {
            let at = payload + ANMF_DURATION_OFFSET;
            let field = bytes
                .get(at..at + ANMF_DURATION_LEN)
                .ok_or(TailError::Malformed("ANMF 过短"))?;
            frames.push((at, read_u24(field)));
        }
        pos = end + (len & 1);
    }
    Ok(frames)
}

/// 动画 WebP 各帧时长之和（毫秒）。
///
/// # 参数
/// - `bytes`：完整的 WebP 文件内容。
///
/// # 返回
/// 总时长；容器损坏返回错误。
///
/// # 示例
/// ```ignore
/// let total = webp_total_ms(&std::fs::read("a.webp")?)?;
/// ```
#[cfg(test)]
pub fn webp_total_ms(bytes: &[u8]) -> Result<u64, TailError> {
    Ok(webp_frames(bytes)?.iter().map(|(_, ms)| ms).sum())
}

/// 把动画 WebP 的总时长补到 `target_ms`（只增不减）。
///
/// # 参数
/// - `bytes`：WebP 文件内容，就地修改。
/// - `target_ms`：目标总时长（毫秒）。
///
/// # 返回
/// 修补前后的总时长；出错时 `bytes` 不变。
///
/// # 示例
/// ```ignore
/// let report = extend_webp_tail(&mut bytes, 5000)?;
/// ```
pub fn extend_webp_tail(bytes: &mut [u8], target_ms: u64) -> Result<TailReport, TailError> {
    let frames = webp_frames(bytes)?;
    let before_ms: u64 = frames.iter().map(|(_, ms)| ms).sum();
    let Some(&(at, last_ms)) = frames.last() else {
        return Err(TailError::Malformed("没有 ANMF 帧"));
    };
    if target_ms <= before_ms {
        return Ok(TailReport { before_ms, after_ms: before_ms });
    }
    let new_last = (last_ms + (target_ms - before_ms)).min(WEBP_MAX_DURATION_MS);
    bytes[at..at + ANMF_DURATION_LEN].copy_from_slice(&new_last.to_le_bytes()[..ANMF_DURATION_LEN]);
    Ok(TailReport { before_ms, after_ms: before_ms - last_ms + new_last })
}

/// 一个 fcTL 块的位置与时长。
struct FctlInfo {
    /// 块起点（长度字段处）。
    chunk_start: usize,
    /// 该帧时长（毫秒）。
    delay_ms: u64,
}

/// 遍历 APNG 的 fcTL 块。
fn apng_frames(bytes: &[u8]) -> Result<Vec<FctlInfo>, TailError> {
    if bytes.len() < PNG_SIGNATURE.len() || bytes[..PNG_SIGNATURE.len()] != PNG_SIGNATURE {
        return Err(TailError::Malformed("不是 PNG"));
    }
    let mut frames = Vec::new();
    let mut pos = PNG_SIGNATURE.len();
    while pos + CHUNK_HEADER_LEN <= bytes.len() {
        let len = read_u32_be(&bytes[pos..]).ok_or(TailError::Malformed("块长度"))?;
        let data = pos + CHUNK_HEADER_LEN;
        let end = data
            .checked_add(len)
            .and_then(|v| v.checked_add(PNG_CRC_LEN))
            .ok_or(TailError::Malformed("块长度溢出"))?;
        if end > bytes.len() {
            return Err(TailError::Malformed("块越界"));
        }
        if &bytes[pos + 4..pos + 8] == b"fcTL" {
            if len != FCTL_DATA_LEN {
                return Err(TailError::Malformed("fcTL 长度"));
            }
            let at = data + FCTL_DELAY_OFFSET;
            let num = u64::from(u16::from_be_bytes([bytes[at], bytes[at + 1]]));
            let den = match u16::from_be_bytes([bytes[at + 2], bytes[at + 3]]) {
                0 => APNG_DEFAULT_DEN,
                d => u64::from(d),
            };
            frames.push(FctlInfo { chunk_start: pos, delay_ms: num * MS_PER_SEC / den });
        }
        pos = end;
    }
    Ok(frames)
}

/// APNG 各帧延迟之和（毫秒）。
///
/// # 参数
/// - `bytes`：完整的 PNG 文件内容。
///
/// # 返回
/// 总时长；容器损坏返回错误。
///
/// # 示例
/// ```ignore
/// let total = apng_total_ms(&std::fs::read("a.apng")?)?;
/// ```
#[cfg(test)]
pub fn apng_total_ms(bytes: &[u8]) -> Result<u64, TailError> {
    Ok(apng_frames(bytes)?.iter().map(|f| f.delay_ms).sum())
}

/// 把 APNG 的总时长补到 `target_ms`（只增不减），并重算被改块的 CRC。
///
/// # 参数
/// - `bytes`：PNG 文件内容，就地修改。
/// - `target_ms`：目标总时长（毫秒）。
///
/// # 返回
/// 修补前后的总时长；出错时 `bytes` 不变。
///
/// # 示例
/// ```ignore
/// let report = extend_apng_tail(&mut bytes, 5000)?;
/// ```
pub fn extend_apng_tail(bytes: &mut [u8], target_ms: u64) -> Result<TailReport, TailError> {
    let frames = apng_frames(bytes)?;
    let before_ms: u64 = frames.iter().map(|f| f.delay_ms).sum();
    let Some(last) = frames.last() else {
        return Err(TailError::Malformed("没有 fcTL 帧"));
    };
    if target_ms <= before_ms {
        return Ok(TailReport { before_ms, after_ms: before_ms });
    }
    let want_ms = last.delay_ms + (target_ms - before_ms);
    // 选精度最高且分子不越界的分母；都放不下就取最大可表示值
    let (num, den) = APNG_DEN_CANDIDATES
        .iter()
        .map(|&den| (want_ms * den / MS_PER_SEC, den))
        .find(|&(num, _)| num <= u64::from(u16::MAX))
        .unwrap_or((u64::from(u16::MAX), 1));
    let data = last.chunk_start + CHUNK_HEADER_LEN;
    // 上面的候选保证 num、den 都能放进 u16
    let num16 = u16::try_from(num).unwrap_or(u16::MAX);
    let den16 = u16::try_from(den).unwrap_or(1);
    bytes[data + FCTL_DELAY_OFFSET..data + FCTL_DELAY_OFFSET + 2].copy_from_slice(&num16.to_be_bytes());
    bytes[data + FCTL_DELAY_OFFSET + 2..data + FCTL_DELAY_OFFSET + 4]
        .copy_from_slice(&den16.to_be_bytes());
    let crc = crc32(&bytes[last.chunk_start + 4..data + FCTL_DATA_LEN]);
    let crc_at = data + FCTL_DATA_LEN;
    bytes[crc_at..crc_at + PNG_CRC_LEN].copy_from_slice(&crc.to_be_bytes());
    let new_ms = u64::from(num16) * MS_PER_SEC / u64::from(den16);
    Ok(TailReport { before_ms, after_ms: before_ms - last.delay_ms + new_ms })
}

/// 对成品文件做尾段修补；非 WebP/APNG 格式直接返回 `Ok(None)`。
///
/// # 参数
/// - `path`：成品文件路径（就地改写）。
/// - `format`：输出格式。
/// - `target_ms`：目标总时长（毫秒，即有效录制时长）。
///
/// # 返回
/// 实际修补的前后时长；无需处理返回 `None`。失败时文件不变。
///
/// # 示例
/// ```ignore
/// extend_file(Path::new("a.webp"), MediaFormat::Webp, 5000)?;
/// ```
pub fn extend_file(
    path: &Path,
    format: MediaFormat,
    target_ms: u64,
) -> Result<Option<TailReport>, TailError> {
    let extend: fn(&mut [u8], u64) -> Result<TailReport, TailError> = match format {
        MediaFormat::Webp => extend_webp_tail,
        MediaFormat::Apng => extend_apng_tail,
        MediaFormat::Mp4 | MediaFormat::Gif => return Ok(None),
    };
    let mut bytes = std::fs::read(path).map_err(|e| TailError::Io(e.to_string()))?;
    let report = extend(&mut bytes, target_ms)?;
    if report.after_ms != report.before_ms {
        std::fs::write(path, &bytes).map_err(|e| TailError::Io(e.to_string()))?;
    }
    Ok(Some(report))
}

#[cfg(test)]
mod tests {
    use super::*;
    use snow_screen_recorder::{
        ExportExecutionMode, ExportFormat, SoftwareH264Priority, StreamingEncoder,
        StreamingEncoderConfig, VideoCodec, VideoEncodeConfig,
    };

    /// 测试画布边长。
    const SIZE: u32 = 64;
    /// 测试帧率（时间基 1/FPS）。
    const FPS: u32 = 10;
    /// 有画面变化的帧号（槽序号）；之后静止到 `END_PTS`。
    const CHANGE_PTS: [u64; 6] = [0, 1, 2, 5, 6, 20];
    /// 录制结束槽序号：期望总时长 = END_PTS / FPS = 5 秒。
    const END_PTS: u64 = 50;
    /// 期望总时长（毫秒）。
    const EXPECTED_MS: u64 = END_PTS * MS_PER_SEC / FPS as u64;
    /// 容差：一帧 + 20ms。
    const TOLERANCE_MS: u64 = MS_PER_SEC / FPS as u64 + 20;

    /// 构造一张纯色 RGBA 图。
    fn solid(shade: u8) -> Vec<u8> {
        [shade, 255 - shade, shade / 2, 255].repeat((SIZE * SIZE) as usize)
    }

    /// 用真实编码器导出已知时间戳的帧序列，返回文件内容。
    fn export_sequence(format: ExportFormat, tag: &str) -> Vec<u8> {
        let path = std::env::temp_dir()
            .join(format!("snow-tailfix-{tag}-{}.{}", std::process::id(), format.file_extension()));
        let config = StreamingEncoderConfig {
            loop_animated_images: true,
            output_path: path.clone(),
            format,
            width: SIZE,
            height: SIZE,
            fps: FPS,
            codec: VideoCodec::H264,
            prefer_hardware_h264: false,
            execution_mode: ExportExecutionMode::SoftwareOnly,
            software_h264_priority: SoftwareH264Priority::X264First,
            video: VideoEncodeConfig::default(),
            encode_threads: 1,
            audio: None,
        };
        let mut encoder = StreamingEncoder::create(config).expect("创建编码器");
        for (i, pts) in CHANGE_PTS.iter().enumerate() {
            let shade = u8::try_from(i * 40).unwrap_or(200);
            encoder.push_rgba_frame_at_pts(*pts, &solid(shade)).expect("推帧");
        }
        encoder.finish_at_pts(END_PTS).expect("收尾");
        let bytes = std::fs::read(&path).expect("读成品");
        let _ = std::fs::remove_file(&path);
        bytes
    }

    /// 回归：WebP 成品经修补后总时长等于期望（±一帧），且修补前确实偏短。
    #[test]
    fn webp_total_duration_matches_recording() {
        let mut bytes = export_sequence(ExportFormat::Webp, "webp");
        let raw = webp_total_ms(&bytes).expect("解析");
        assert!(raw + TOLERANCE_MS < EXPECTED_MS, "未修补时应偏短，实际 {raw}ms");
        extend_webp_tail(&mut bytes, EXPECTED_MS).expect("修补");
        let fixed = webp_total_ms(&bytes).expect("再解析");
        assert!(fixed.abs_diff(EXPECTED_MS) <= TOLERANCE_MS, "修补后 {fixed}ms");
    }

    /// 回归：APNG 成品经修补后总时长等于期望（±一帧），CRC 有效，且修补前偏短。
    #[test]
    fn apng_total_duration_matches_recording() {
        let mut bytes = export_sequence(ExportFormat::Apng, "apng");
        let raw = apng_total_ms(&bytes).expect("解析");
        assert!(raw + TOLERANCE_MS < EXPECTED_MS, "未修补时应偏短，实际 {raw}ms");
        extend_apng_tail(&mut bytes, EXPECTED_MS).expect("修补");
        let fixed = apng_total_ms(&bytes).expect("再解析");
        assert!(fixed.abs_diff(EXPECTED_MS) <= TOLERANCE_MS, "修补后 {fixed}ms");
        assert!(png_crcs_valid(&bytes), "块 CRC 应全部有效");
    }

    /// 校验 PNG 中每个块的 CRC。
    fn png_crcs_valid(bytes: &[u8]) -> bool {
        let mut pos = PNG_SIGNATURE.len();
        while pos + CHUNK_HEADER_LEN <= bytes.len() {
            let Some(len) = read_u32_be(&bytes[pos..]) else { return false };
            let end = pos + CHUNK_HEADER_LEN + len;
            let Some(stored) = bytes.get(end..end + PNG_CRC_LEN).and_then(read_u32_be) else {
                return false;
            };
            if u32::try_from(stored) != Ok(crc32(&bytes[pos + 4..end])) {
                return false;
            }
            pos = end + PNG_CRC_LEN;
        }
        true
    }

    /// 手工构造 WebP：给定各帧时长。
    fn fake_webp(durations: &[u32]) -> Vec<u8> {
        let mut body = b"WEBP".to_vec();
        for d in durations {
            let mut payload = vec![0u8; 16];
            payload[ANMF_DURATION_OFFSET..ANMF_DURATION_OFFSET + 3]
                .copy_from_slice(&d.to_le_bytes()[..3]);
            body.extend_from_slice(b"ANMF");
            body.extend_from_slice(&16u32.to_le_bytes());
            body.extend_from_slice(&payload);
        }
        let mut out = b"RIFF".to_vec();
        out.extend_from_slice(&u32::try_from(body.len()).unwrap_or(0).to_le_bytes());
        out.extend_from_slice(&body);
        out
    }

    /// 目标不大于现有总时长时不修改。
    #[test]
    fn webp_not_shortened() {
        let mut bytes = fake_webp(&[100, 200]);
        let orig = bytes.clone();
        let r = extend_webp_tail(&mut bytes, 250).expect("ok");
        assert_eq!((r.before_ms, r.after_ms), (300, 300));
        assert_eq!(bytes, orig);
    }

    /// 超过 24 位上限时截断。
    #[test]
    fn webp_clamped_to_24_bits() {
        let mut bytes = fake_webp(&[100]);
        let r = extend_webp_tail(&mut bytes, u64::MAX / 2).expect("ok");
        assert_eq!(r.after_ms, WEBP_MAX_DURATION_MS);
    }

    /// 损坏或截断的输入返回错误且不 panic、不改动。
    #[test]
    fn corrupt_inputs_are_rejected() {
        let mut junk = b"not a webp at all".to_vec();
        assert!(extend_webp_tail(&mut junk, 1000).is_err());
        let mut truncated = fake_webp(&[100, 100]);
        truncated.truncate(truncated.len() - 5);
        let orig = truncated.clone();
        assert!(extend_webp_tail(&mut truncated, 1000).is_err());
        assert_eq!(truncated, orig);
        let mut png = b"\x89PNG\r\n\x1a\nxx".to_vec();
        assert!(extend_apng_tail(&mut png, 1000).is_err());
        assert!(apng_total_ms(&[]).is_err());
    }

    /// CRC-32 与标准测试向量一致。
    #[test]
    fn crc32_known_vector() {
        assert_eq!(crc32(b"123456789"), 0xCBF4_3926);
    }

    /// MP4/GIF 不做处理。
    #[test]
    fn non_animated_formats_are_skipped() {
        let p = Path::new("does-not-matter.mp4");
        assert_eq!(extend_file(p, MediaFormat::Mp4, 1000), Ok(None));
        assert_eq!(extend_file(p, MediaFormat::Gif, 1000), Ok(None));
    }
}
