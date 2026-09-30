//! snow-ocr-process 二进制协议 v4(小端;字符串 = u32 字节长度 + UTF-8)。

/// 二进制协议魔数
pub const MAGIC: [u8; 4] = *b"SOCR";
/// 协议版本号
pub const VERSION: u16 = 4;
/// 帧头的总字节长度
pub const HEADER_LEN: usize = 20;
/// 载荷的最大字节限制 (1 MiB)
pub const MAX_PAYLOAD: usize = 1024 * 1024;
/// 共享内存槽头的字节长度
pub const SLOT_HEADER_LEN: usize = 32;
/// 共享内存槽的魔数
pub const SLOT_MAGIC: u32 = 0x544f4c53;
/// 允许处理的最大像素数限制 (4K 屏幕)
pub const MAX_PIXELS: usize = 3840 * 2160;

/// 槽状态:像素已就绪
const SLOT_STATE_READY: u32 = 1;
/// 每像素字节数(BGRA/RGBA)
const BYTES_PER_PIXEL: u32 = 4;
/// Complete 状态码:失败
const STATUS_FAILED: u8 = 0;
/// Complete 状态码:成功
const STATUS_SUCCESS: u8 = 1;
/// Complete 状态码:取消
const STATUS_CANCELLED: u8 = 2;
/// 一行 OCR 结果的最小字节数(空文本长度 4 + 置信度 4 + 8 个 f32 共 32)
const MIN_LINE_LEN: usize = 4 + 4 + 32;

/// 协议的帧类型枚举。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u16)]
pub enum Kind {
    /// 握手请求
    Hello = 1,
    /// 握手就绪
    Ready = 2,
    /// 提交任务
    Submit = 3,
    /// 取消任务
    Cancel = 4,
    /// 任务完成
    Complete = 5,
    /// 关机请求
    Shutdown = 6,
    /// 关机确认
    ShutdownAck = 7,
    /// 准备会话
    PrepareSession = 8,
    /// 会话已就绪
    SessionReady = 9,
    /// 释放会话
    ReleaseSession = 10,
    /// 会话已释放
    SessionReleased = 11,
    /// 挂载缓冲区
    AttachBuffer = 12,
    /// 缓冲区已挂载
    BufferAttached = 13,
    /// 触发识别
    Recognize = 14,
    /// 图像已消费
    ImageConsumed = 15,
    /// 卸载缓冲区
    DetachBuffer = 16,
    /// 缓冲区已卸载
    BufferDetached = 17,
    /// 丢弃图像
    DiscardImage = 18,
}

impl Kind {
    /// 从 `u16` 数值解析为 `Kind`。
    ///
    /// # 参数
    /// - `v`: 底层无符号整数值。
    ///
    /// # 返回值
    /// 数值有效返回 `Some(Kind)`,否则 `None`。
    ///
    /// # 示例
    /// ```
    /// use snow_ocr_protocol::Kind;
    /// assert_eq!(Kind::from_u16(1), Some(Kind::Hello));
    /// assert_eq!(Kind::from_u16(999), None);
    /// ```
    pub fn from_u16(v: u16) -> Option<Kind> {
        match v {
            1 => Some(Self::Hello),
            2 => Some(Self::Ready),
            3 => Some(Self::Submit),
            4 => Some(Self::Cancel),
            5 => Some(Self::Complete),
            6 => Some(Self::Shutdown),
            7 => Some(Self::ShutdownAck),
            8 => Some(Self::PrepareSession),
            9 => Some(Self::SessionReady),
            10 => Some(Self::ReleaseSession),
            11 => Some(Self::SessionReleased),
            12 => Some(Self::AttachBuffer),
            13 => Some(Self::BufferAttached),
            14 => Some(Self::Recognize),
            15 => Some(Self::ImageConsumed),
            16 => Some(Self::DetachBuffer),
            17 => Some(Self::BufferDetached),
            18 => Some(Self::DiscardImage),
            _ => None,
        }
    }

    /// 将 `Kind` 转为 `u16` 数值。
    ///
    /// # 返回值
    /// 类型对应的数值。
    ///
    /// # 示例
    /// ```
    /// use snow_ocr_protocol::Kind;
    /// assert_eq!(Kind::Hello.as_u16(), 1);
    /// ```
    pub fn as_u16(self) -> u16 {
        self as u16
    }
}

/// 一个完整的协议帧。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Frame {
    /// 帧类型。
    pub kind: Kind,
    /// 操作 ID。
    pub id: u64,
    /// 帧载荷。
    pub payload: Vec<u8>,
}

/// 协议编解码错误。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProtocolError {
    /// 不支持的版本号。
    BadVersion(u16),
    /// 未知的帧类型(或未知状态码)。
    BadKind(u16),
    /// 载荷(或槽像素)超出上限。
    PayloadTooLarge(usize),
    /// 数据不足或被截断。
    Truncated,
    /// 字符串不是合法 UTF-8。
    InvalidUtf8,
    /// 解码完成后仍有多余字节。
    TrailingBytes,
    /// 流在帧边界干净结束。
    Eof,
    /// 底层 IO 错误(文本)。
    Io(String),
}

impl std::fmt::Display for ProtocolError {
    /// 输出中文错误描述。
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::BadVersion(v) => write!(f, "错误的版本号: {v}"),
            Self::BadKind(k) => write!(f, "未知的帧类型: {k}"),
            Self::PayloadTooLarge(l) => write!(f, "载荷过大: {l}"),
            Self::Truncated => write!(f, "数据被截断"),
            Self::InvalidUtf8 => write!(f, "非法的 UTF-8 字符串"),
            Self::TrailingBytes => write!(f, "解析完毕后仍有未消费的数据"),
            Self::Eof => write!(f, "流已结束"),
            Self::Io(err) => write!(f, "IO 错误: {err}"),
        }
    }
}

impl std::error::Error for ProtocolError {}

/// Ready 应答载荷。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Ready {
    /// 启动是否成功。
    pub success: bool,
    /// 能力位。
    pub capability: u8,
    /// 执行提供者标识。
    pub provider: String,
    /// 运行时版本。
    pub runtime_version: String,
    /// 协议号。
    pub protocol: u32,
}

/// 单行 OCR 识别结果。
#[derive(Debug, Clone, PartialEq)]
pub struct OcrLine {
    /// 文本内容。
    pub text: String,
    /// 置信度。
    pub score: f32,
    /// 四边形四个顶点 [x, y]。
    pub quad: [[f32; 2]; 4],
}

/// 任务完成结果。
#[derive(Debug, Clone, PartialEq)]
pub enum CompleteResult {
    /// 成功,携带识别行。
    Success(Vec<OcrLine>),
    /// 失败,携带错误描述。
    Failed(String),
    /// 取消,携带说明。
    Cancelled(String),
}

// ======================== 私有辅助 ========================

/// 追加 u32 长度前缀字符串。
fn write_string(buf: &mut Vec<u8>, s: &str) {
    buf.extend_from_slice(&(s.len() as u32).to_le_bytes());
    buf.extend_from_slice(s.as_bytes());
}

/// 从头部取出 N 个字节。
fn take<'a>(payload: &mut &'a [u8], n: usize) -> Result<&'a [u8], ProtocolError> {
    if payload.len() < n {
        return Err(ProtocolError::Truncated);
    }
    let (head, rest) = payload.split_at(n);
    *payload = rest;
    Ok(head)
}

/// 读取 u32 长度前缀字符串。
fn read_string(payload: &mut &[u8]) -> Result<String, ProtocolError> {
    let len = read_u32(payload)? as usize;
    let bytes = take(payload, len)?;
    String::from_utf8(bytes.to_vec()).map_err(|_| ProtocolError::InvalidUtf8)
}

/// 读取 u8。
fn read_u8(payload: &mut &[u8]) -> Result<u8, ProtocolError> {
    take(payload, 1)?.first().copied().ok_or(ProtocolError::Truncated)
}

/// 读取 u32。
fn read_u32(payload: &mut &[u8]) -> Result<u32, ProtocolError> {
    let mut arr = [0u8; 4];
    arr.copy_from_slice(take(payload, 4)?);
    Ok(u32::from_le_bytes(arr))
}

/// 读取 u64。
fn read_u64(payload: &mut &[u8]) -> Result<u64, ProtocolError> {
    let mut arr = [0u8; 8];
    arr.copy_from_slice(take(payload, 8)?);
    Ok(u64::from_le_bytes(arr))
}

/// 读取 f32。
fn read_f32(payload: &mut &[u8]) -> Result<f32, ProtocolError> {
    let mut arr = [0u8; 4];
    arr.copy_from_slice(take(payload, 4)?);
    Ok(f32::from_le_bytes(arr))
}

/// 要求载荷已被完全消费。
fn ensure_consumed(payload: &[u8]) -> Result<(), ProtocolError> {
    if payload.is_empty() {
        Ok(())
    } else {
        Err(ProtocolError::TrailingBytes)
    }
}

// ======================== 帧编解码 ========================

/// 编码整帧(帧头 + 载荷)。
///
/// # 参数
/// - `kind`: 帧类型。
/// - `id`: 操作 ID。
/// - `payload`: 载荷,不得超过 [`MAX_PAYLOAD`]。
///
/// # 返回值
/// 完整帧字节;载荷超限返回 `PayloadTooLarge`。
///
/// # 示例
/// ```
/// use snow_ocr_protocol::{encode_frame, Kind};
/// let buf = encode_frame(Kind::Hello, 1, &[0; 10]).unwrap();
/// assert_eq!(buf.len(), 30);
/// ```
pub fn encode_frame(kind: Kind, id: u64, payload: &[u8]) -> Result<Vec<u8>, ProtocolError> {
    if payload.len() > MAX_PAYLOAD {
        return Err(ProtocolError::PayloadTooLarge(payload.len()));
    }
    let mut buf = Vec::with_capacity(HEADER_LEN + payload.len());
    buf.extend_from_slice(&MAGIC);
    buf.extend_from_slice(&VERSION.to_le_bytes());
    buf.extend_from_slice(&kind.as_u16().to_le_bytes());
    buf.extend_from_slice(&id.to_le_bytes());
    buf.extend_from_slice(&(payload.len() as u32).to_le_bytes());
    buf.extend_from_slice(payload);
    Ok(buf)
}

/// 带魔数重同步的帧读取器。
///
/// 逐字节读取,建议传入带缓冲的 Reader(如 `BufReader`)。
pub struct FrameReader<R: std::io::Read> {
    /// 底层输入流。
    inner: R,
}

impl<R: std::io::Read> FrameReader<R> {
    /// 构造读取器。
    ///
    /// # 参数
    /// - `inner`: 任意 `std::io::Read`。
    ///
    /// # 返回值
    /// 新的读取器。
    ///
    /// # 示例
    /// ```
    /// use snow_ocr_protocol::FrameReader;
    /// let data = [0u8; 0];
    /// let _reader = FrameReader::new(&data[..]);
    /// ```
    pub fn new(inner: R) -> Self {
        Self { inner }
    }

    /// 读取下一帧,遇到垃圾字节自动重同步到下一个魔数。
    ///
    /// # 返回值
    /// 成功返回 `Frame`;帧边界干净结束返回 `Eof`;帧中间结束返回 `Truncated`;
    /// 版本/类型/长度非法返回对应错误;其余 IO 错误返回 `Io`。
    ///
    /// # 示例
    /// ```
    /// use snow_ocr_protocol::{encode_frame, FrameReader, Kind};
    /// let bytes = encode_frame(Kind::Hello, 7, &[]).unwrap();
    /// let mut reader = FrameReader::new(&bytes[..]);
    /// assert_eq!(reader.read_frame().unwrap().id, 7);
    /// ```
    pub fn read_frame(&mut self) -> Result<Frame, ProtocolError> {
        self.sync_magic()?;

        let mut rest = [0u8; HEADER_LEN - MAGIC.len()];
        self.read_exact_retrying(&mut rest)?;

        let mut p: &[u8] = &rest;
        let mut ver = [0u8; 2];
        ver.copy_from_slice(take(&mut p, 2)?);
        let version = u16::from_le_bytes(ver);
        if version != VERSION {
            return Err(ProtocolError::BadVersion(version));
        }
        let mut kb = [0u8; 2];
        kb.copy_from_slice(take(&mut p, 2)?);
        let kind_val = u16::from_le_bytes(kb);
        let kind = Kind::from_u16(kind_val).ok_or(ProtocolError::BadKind(kind_val))?;
        let id = read_u64(&mut p)?;
        let payload_len = read_u32(&mut p)? as usize;
        if payload_len > MAX_PAYLOAD {
            return Err(ProtocolError::PayloadTooLarge(payload_len));
        }

        let mut payload = vec![0u8; payload_len];
        self.read_exact_retrying(&mut payload)?;
        Ok(Frame { kind, id, payload })
    }

    /// 逐字节滑动查找魔数;魔数仅首字节 `S` 可重叠,失配时按 `S` 重新起算。
    fn sync_magic(&mut self) -> Result<(), ProtocolError> {
        let mut matched = 0usize;
        loop {
            let mut b = [0u8; 1];
            match self.inner.read(&mut b) {
                Ok(0) => {
                    return Err(if matched == 0 {
                        ProtocolError::Eof
                    } else {
                        ProtocolError::Truncated
                    });
                }
                Ok(_) => {
                    let byte = b[0];
                    if MAGIC.get(matched) == Some(&byte) {
                        matched += 1;
                        if matched == MAGIC.len() {
                            return Ok(());
                        }
                    } else if byte == MAGIC[0] {
                        matched = 1;
                    } else {
                        matched = 0;
                    }
                }
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
                Err(e) => return Err(ProtocolError::Io(e.to_string())),
            }
        }
    }

    /// 读满缓冲区;提前结束返回 `Truncated`,中断则重试。
    fn read_exact_retrying(&mut self, buf: &mut [u8]) -> Result<(), ProtocolError> {
        let mut read = 0;
        while read < buf.len() {
            match self.inner.read(&mut buf[read..]) {
                Ok(0) => return Err(ProtocolError::Truncated),
                Ok(n) => read += n,
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
                Err(e) => return Err(ProtocolError::Io(e.to_string())),
            }
        }
        Ok(())
    }
}

// ======================== 载荷构造 ========================

/// 构造 Hello 载荷。
///
/// # 参数
/// - `capability_cache_dir`: 能力缓存目录。
///
/// # 返回值
/// 载荷字节。
///
/// # 示例
/// ```
/// use snow_ocr_protocol::hello_payload;
/// assert_eq!(hello_payload("ab"), [2, 0, 0, 0, b'a', b'b']);
/// ```
pub fn hello_payload(capability_cache_dir: &str) -> Vec<u8> {
    let mut buf = Vec::new();
    write_string(&mut buf, capability_cache_dir);
    buf
}

/// 构造 AttachBuffer 载荷。
///
/// # 参数
/// - `path`: 共享缓冲区路径。
/// - `total_bytes`: 总字节数。
///
/// # 返回值
/// 载荷字节。
///
/// # 示例
/// ```
/// use snow_ocr_protocol::attach_buffer_payload;
/// let _pl = attach_buffer_payload("/tmp/mem", 1024);
/// ```
pub fn attach_buffer_payload(path: &str, total_bytes: u64) -> Vec<u8> {
    let mut buf = Vec::new();
    write_string(&mut buf, path);
    buf.extend_from_slice(&total_bytes.to_le_bytes());
    buf
}

/// 构造 PrepareSession 载荷。
///
/// # 参数
/// - `directml`: 是否启用 DirectML。
/// - `resize_policy`: 缩放策略(0=max,1=min)。
/// - `detector`: 检测模型路径。
/// - `recognizer`: 识别模型路径。
/// - `dictionary`: 字典路径。
///
/// # 返回值
/// 载荷字节。
///
/// # 示例
/// ```
/// use snow_ocr_protocol::prepare_session_payload;
/// let _pl = prepare_session_payload(true, 0, "det", "rec", "dic");
/// ```
pub fn prepare_session_payload(
    directml: bool,
    resize_policy: u8,
    detector: &str,
    recognizer: &str,
    dictionary: &str,
) -> Vec<u8> {
    let mut buf = vec![u8::from(directml), resize_policy];
    write_string(&mut buf, detector);
    write_string(&mut buf, recognizer);
    write_string(&mut buf, dictionary);
    buf
}

/// 构造 Submit 载荷。
///
/// # 参数
/// - `generation`: 世代号。
/// - `width`: 图像宽。
/// - `height`: 图像高。
/// - `stride`: 行跨度(字节)。
/// - `sequence`: 序列号。
///
/// # 返回值
/// 载荷字节。
///
/// # 示例
/// ```
/// use snow_ocr_protocol::submit_payload;
/// assert_eq!(submit_payload(1, 2, 3, 8, 4).len(), 28);
/// ```
pub fn submit_payload(
    generation: u64,
    width: u32,
    height: u32,
    stride: u32,
    sequence: u64,
) -> Vec<u8> {
    let mut buf = Vec::with_capacity(28);
    buf.extend_from_slice(&generation.to_le_bytes());
    buf.extend_from_slice(&width.to_le_bytes());
    buf.extend_from_slice(&height.to_le_bytes());
    buf.extend_from_slice(&stride.to_le_bytes());
    buf.extend_from_slice(&sequence.to_le_bytes());
    buf
}

// ======================== 载荷解码 ========================

/// 构造 Ready 载荷(供假 worker/测试使用)。
///
/// # 参数
/// - `ready`: Ready 内容。
///
/// # 返回值
/// 载荷字节。
///
/// # 示例
/// ```
/// use snow_ocr_protocol::{ready_payload, decode_ready, Ready};
/// let r = Ready { success: true, capability: 1, provider: "P".into(), runtime_version: "1".into(), protocol: 4 };
/// assert_eq!(decode_ready(&ready_payload(&r)).unwrap(), r);
/// ```
pub fn ready_payload(ready: &Ready) -> Vec<u8> {
    let mut buf = vec![u8::from(ready.success), ready.capability];
    write_string(&mut buf, &ready.provider);
    write_string(&mut buf, &ready.runtime_version);
    buf.extend_from_slice(&ready.protocol.to_le_bytes());
    buf
}

/// 解码 Ready 载荷。
///
/// # 参数
/// - `payload`: 载荷字节。
///
/// # 返回值
/// 解码后的 `Ready`;截断/多余/非 UTF-8 返回相应错误。
///
/// # 示例
/// ```
/// use snow_ocr_protocol::decode_ready;
/// assert!(decode_ready(&[1]).is_err());
/// ```
pub fn decode_ready(payload: &[u8]) -> Result<Ready, ProtocolError> {
    let mut p = payload;
    let success = read_u8(&mut p)? != 0;
    let capability = read_u8(&mut p)?;
    let provider = read_string(&mut p)?;
    let runtime_version = read_string(&mut p)?;
    let protocol = read_u32(&mut p)?;
    ensure_consumed(p)?;
    Ok(Ready {
        success,
        capability,
        provider,
        runtime_version,
        protocol,
    })
}

/// 解码 SessionReady 载荷(恰 1 字节)。
///
/// # 参数
/// - `payload`: 载荷字节。
///
/// # 返回值
/// 是否成功;多余字节返回 `TrailingBytes`。
///
/// # 示例
/// ```
/// use snow_ocr_protocol::decode_session_ready;
/// assert_eq!(decode_session_ready(&[1]).unwrap(), true);
/// ```
pub fn decode_session_ready(payload: &[u8]) -> Result<bool, ProtocolError> {
    let mut p = payload;
    let success = read_u8(&mut p)? != 0;
    ensure_consumed(p)?;
    Ok(success)
}

/// 构造 ImageConsumed 载荷。
///
/// # 参数
/// - `generation`: 世代号。
/// - `sequence`: 序列号。
///
/// # 返回值
/// 载荷字节。
///
/// # 示例
/// ```
/// use snow_ocr_protocol::image_consumed_payload;
/// assert_eq!(image_consumed_payload(1, 2).len(), 16);
/// ```
pub fn image_consumed_payload(generation: u64, sequence: u64) -> Vec<u8> {
    let mut buf = Vec::with_capacity(16);
    buf.extend_from_slice(&generation.to_le_bytes());
    buf.extend_from_slice(&sequence.to_le_bytes());
    buf
}

/// 解码 ImageConsumed 载荷。
///
/// # 参数
/// - `payload`: 载荷字节。
///
/// # 返回值
/// `(generation, sequence)`。
///
/// # 示例
/// ```
/// use snow_ocr_protocol::{decode_image_consumed, image_consumed_payload};
/// assert_eq!(decode_image_consumed(&image_consumed_payload(10, 20)).unwrap(), (10, 20));
/// ```
pub fn decode_image_consumed(payload: &[u8]) -> Result<(u64, u64), ProtocolError> {
    let mut p = payload;
    let generation = read_u64(&mut p)?;
    let sequence = read_u64(&mut p)?;
    ensure_consumed(p)?;
    Ok((generation, sequence))
}

/// 构造 Complete 载荷,与 [`decode_complete`] 互逆。
///
/// # 参数
/// - `result`: 完成结果。
///
/// # 返回值
/// 载荷字节。
///
/// # 示例
/// ```
/// use snow_ocr_protocol::{complete_payload, decode_complete, CompleteResult};
/// let r = CompleteResult::Failed("x".into());
/// assert_eq!(decode_complete(&complete_payload(&r)).unwrap(), r);
/// ```
pub fn complete_payload(result: &CompleteResult) -> Vec<u8> {
    let mut buf = Vec::new();
    match result {
        CompleteResult::Failed(msg) => {
            buf.push(STATUS_FAILED);
            write_string(&mut buf, msg);
        }
        CompleteResult::Cancelled(msg) => {
            buf.push(STATUS_CANCELLED);
            write_string(&mut buf, msg);
        }
        CompleteResult::Success(lines) => {
            buf.push(STATUS_SUCCESS);
            write_string(&mut buf, "");
            buf.extend_from_slice(&(lines.len() as u32).to_le_bytes());
            for line in lines {
                write_string(&mut buf, &line.text);
                buf.extend_from_slice(&line.score.to_le_bytes());
                for pt in &line.quad {
                    buf.extend_from_slice(&pt[0].to_le_bytes());
                    buf.extend_from_slice(&pt[1].to_le_bytes());
                }
            }
        }
    }
    buf
}

/// 解码 Complete 载荷。
///
/// # 参数
/// - `payload`: 载荷字节。
///
/// # 返回值
/// 完成结果;未知状态码返回 `BadKind(status)`。
///
/// # 示例
/// ```
/// use snow_ocr_protocol::{decode_complete, CompleteResult};
/// let r = decode_complete(&[2, 0, 0, 0, 0]).unwrap();
/// assert_eq!(r, CompleteResult::Cancelled(String::new()));
/// ```
pub fn decode_complete(payload: &[u8]) -> Result<CompleteResult, ProtocolError> {
    let mut p = payload;
    let status = read_u8(&mut p)?;
    match status {
        STATUS_FAILED | STATUS_CANCELLED => {
            let msg = read_string(&mut p)?;
            ensure_consumed(p)?;
            Ok(if status == STATUS_FAILED {
                CompleteResult::Failed(msg)
            } else {
                CompleteResult::Cancelled(msg)
            })
        }
        STATUS_SUCCESS => {
            let _reserved = read_string(&mut p)?;
            let count = read_u32(&mut p)? as usize;
            let mut lines = Vec::with_capacity(count.min(p.len() / MIN_LINE_LEN));
            for _ in 0..count {
                let text = read_string(&mut p)?;
                let score = read_f32(&mut p)?;
                let mut quad = [[0.0f32; 2]; 4];
                for pt in &mut quad {
                    pt[0] = read_f32(&mut p)?;
                    pt[1] = read_f32(&mut p)?;
                }
                lines.push(OcrLine { text, score, quad });
            }
            ensure_consumed(p)?;
            Ok(CompleteResult::Success(lines))
        }
        other => Err(ProtocolError::BadKind(u16::from(other))),
    }
}

// ======================== 图像槽 ========================

/// 向映射文件头 32 字节写槽头并置就绪,须在像素写好之后调用。
///
/// 布局:sequence u64@0、state u32@8(=1)、width u32@12、height u32@16、
/// stride u32@20、byte_count u32@24、magic u32@28。
///
/// # 参数
/// - `slot`: 映射内存,长度至少 [`SLOT_HEADER_LEN`]。
/// - `sequence`: 序列号。
/// - `width`: 图像宽。
/// - `height`: 图像高。
///
/// # 返回值
/// 成功返回 `Ok(())`;slot 过短返回 `Truncated`;宽高为 0、像素数超过
/// [`MAX_PIXELS`] 或 stride*height 溢出 u32 返回 `PayloadTooLarge`。
///
/// # 示例
/// ```
/// use snow_ocr_protocol::write_slot_header;
/// let mut mem = vec![0u8; 32];
/// write_slot_header(&mut mem, 1, 100, 100).unwrap();
/// ```
pub fn write_slot_header(
    slot: &mut [u8],
    sequence: u64,
    width: u32,
    height: u32,
) -> Result<(), ProtocolError> {
    let header = slot
        .get_mut(..SLOT_HEADER_LEN)
        .ok_or(ProtocolError::Truncated)?;
    if width == 0 || height == 0 {
        return Err(ProtocolError::PayloadTooLarge(0));
    }
    let pixels = (width as usize)
        .checked_mul(height as usize)
        .filter(|&n| n <= MAX_PIXELS)
        .ok_or(ProtocolError::PayloadTooLarge(MAX_PIXELS + 1))?;
    let stride = width
        .checked_mul(BYTES_PER_PIXEL)
        .ok_or(ProtocolError::PayloadTooLarge(pixels))?;
    let byte_count = stride
        .checked_mul(height)
        .ok_or(ProtocolError::PayloadTooLarge(pixels))?;

    header[0..8].copy_from_slice(&sequence.to_le_bytes());
    header[8..12].copy_from_slice(&SLOT_STATE_READY.to_le_bytes());
    header[12..16].copy_from_slice(&width.to_le_bytes());
    header[16..20].copy_from_slice(&height.to_le_bytes());
    header[20..24].copy_from_slice(&stride.to_le_bytes());
    header[24..28].copy_from_slice(&byte_count.to_le_bytes());
    header[28..32].copy_from_slice(&SLOT_MAGIC.to_le_bytes());
    Ok(())
}

/// BGRA 转 RGBA(交换 R/B,alpha 不变)。
///
/// # 参数
/// - `dst`: 目标缓冲,长度须与 `src` 相等。
/// - `src`: BGRA 源数据,长度须为 4 的倍数。
///
/// # 返回值
/// 成功 `Ok(())`;长度不等或不是 4 的倍数返回 `Truncated`。
///
/// # 示例
/// ```
/// use snow_ocr_protocol::bgra_to_rgba;
/// let mut d = [0; 4];
/// bgra_to_rgba(&mut d, &[1, 2, 3, 4]).unwrap();
/// assert_eq!(d, [3, 2, 1, 4]);
/// ```
pub fn bgra_to_rgba(dst: &mut [u8], src: &[u8]) -> Result<(), ProtocolError> {
    if dst.len() != src.len() || !src.len().is_multiple_of(4) {
        return Err(ProtocolError::Truncated);
    }
    for (d, s) in dst.chunks_exact_mut(4).zip(src.chunks_exact(4)) {
        d.copy_from_slice(&[s[2], s[1], s[0], s[3]]);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 读取槽头中指定偏移的 u32。
    fn u32_at(buf: &[u8], off: usize) -> u32 {
        u32::from_le_bytes(buf[off..off + 4].try_into().unwrap())
    }

    /// 黄金字节:Hello 载荷与整帧。
    #[test]
    fn golden_hello() {
        assert_eq!(hello_payload("ab"), vec![2, 0, 0, 0, b'a', b'b']);
        let frame = encode_frame(Kind::Hello, 0, &hello_payload("")).unwrap();
        let expected = vec![
            0x53, 0x4f, 0x43, 0x52, 0x04, 0x00, 0x01, 0x00, // 魔数、版本、kind
            0, 0, 0, 0, 0, 0, 0, 0, // id
            0x04, 0x00, 0x00, 0x00, // 载荷长度
            0x00, 0x00, 0x00, 0x00, // 空字符串
        ];
        assert_eq!(frame, expected);
    }

    /// 黄金字节:attach/prepare/submit 载荷。
    #[test]
    fn golden_other_payloads() {
        assert_eq!(
            attach_buffer_payload("a", 1),
            vec![1, 0, 0, 0, b'a', 1, 0, 0, 0, 0, 0, 0, 0]
        );
        assert_eq!(
            prepare_session_payload(true, 1, "D", "R", "X"),
            vec![
                1, 1, 1, 0, 0, 0, b'D', 1, 0, 0, 0, b'R', 1, 0, 0, 0, b'X'
            ]
        );
        let mut expected = Vec::new();
        expected.extend_from_slice(&2u64.to_le_bytes());
        expected.extend_from_slice(&100u32.to_le_bytes());
        expected.extend_from_slice(&200u32.to_le_bytes());
        expected.extend_from_slice(&400u32.to_le_bytes());
        expected.extend_from_slice(&5u64.to_le_bytes());
        assert_eq!(submit_payload(2, 100, 200, 400, 5), expected);
    }

    /// Ready 往返。
    #[test]
    fn ready_roundtrip() {
        let ready = Ready {
            success: true,
            capability: 9,
            provider: "prov".into(),
            runtime_version: "1.0".into(),
            protocol: 4,
        };
        assert_eq!(decode_ready(&ready_payload(&ready)).unwrap(), ready);
    }

    /// Complete 三种结果往返。
    #[test]
    fn complete_roundtrip() {
        let cases = [
            CompleteResult::Failed("fail".into()),
            CompleteResult::Cancelled("cancel".into()),
            CompleteResult::Success(vec![
                OcrLine {
                    text: "你好".into(),
                    score: 0.9,
                    quad: [[0.0, 1.0], [2.0, 3.0], [4.0, 5.0], [6.0, 7.0]],
                },
                OcrLine {
                    text: "b".into(),
                    score: 0.5,
                    quad: [[8.0, 9.0], [10.0, 11.0], [12.0, 13.0], [14.0, 15.0]],
                },
            ]),
        ];
        for case in cases {
            assert_eq!(decode_complete(&complete_payload(&case)).unwrap(), case);
        }
    }

    /// ImageConsumed 往返与 SessionReady。
    #[test]
    fn image_consumed_and_session_ready() {
        assert_eq!(
            decode_image_consumed(&image_consumed_payload(42, 84)).unwrap(),
            (42, 84)
        );
        assert!(decode_session_ready(&[1]).unwrap());
        assert!(!decode_session_ready(&[0]).unwrap());
    }

    /// 垃圾字节与半截魔数后仍能解出帧。
    #[test]
    fn reader_resync() {
        let frame = encode_frame(Kind::Submit, 123, &[1, 2, 3]).unwrap();
        for garbage in [&b"SOC"[..], b"SOSOC", b"xx SO SOC ", b"S"] {
            let mut data = garbage.to_vec();
            data.extend_from_slice(&frame);
            let got = FrameReader::new(data.as_slice()).read_frame().unwrap();
            assert_eq!(got.kind, Kind::Submit);
            assert_eq!(got.id, 123);
            assert_eq!(got.payload, vec![1, 2, 3]);
        }
    }

    /// 连续两帧后干净 EOF;帧中间截断。
    #[test]
    fn reader_two_frames_eof_and_truncated() {
        let f = encode_frame(Kind::Hello, 1, &[]).unwrap();
        let mut data = f.clone();
        data.extend_from_slice(&f);
        let mut reader = FrameReader::new(data.as_slice());
        assert_eq!(reader.read_frame().unwrap().id, 1);
        assert_eq!(reader.read_frame().unwrap().id, 1);
        assert_eq!(reader.read_frame().unwrap_err(), ProtocolError::Eof);

        let mut cut = FrameReader::new(&f[..10]);
        assert_eq!(cut.read_frame().unwrap_err(), ProtocolError::Truncated);
        let with_payload = encode_frame(Kind::Hello, 1, &[9; 8]).unwrap();
        let mut cut2 = FrameReader::new(&with_payload[..HEADER_LEN + 3]);
        assert_eq!(cut2.read_frame().unwrap_err(), ProtocolError::Truncated);
    }

    /// 错误版本、未知 kind、超限长度、超限编码。
    #[test]
    fn reject_bad_frames() {
        let mut f = encode_frame(Kind::Hello, 1, &[]).unwrap();
        f[4] = 99;
        assert_eq!(
            FrameReader::new(f.as_slice()).read_frame().unwrap_err(),
            ProtocolError::BadVersion(99)
        );

        let mut f = encode_frame(Kind::Hello, 1, &[]).unwrap();
        f[6] = 255;
        assert_eq!(
            FrameReader::new(f.as_slice()).read_frame().unwrap_err(),
            ProtocolError::BadKind(255)
        );

        let mut f = encode_frame(Kind::Hello, 1, &[]).unwrap();
        f[16..20].copy_from_slice(&((MAX_PAYLOAD as u32) + 1).to_le_bytes());
        assert_eq!(
            FrameReader::new(f.as_slice()).read_frame().unwrap_err(),
            ProtocolError::PayloadTooLarge(MAX_PAYLOAD + 1)
        );

        let big = vec![0u8; MAX_PAYLOAD + 1];
        assert_eq!(
            encode_frame(Kind::Hello, 0, &big).unwrap_err(),
            ProtocolError::PayloadTooLarge(MAX_PAYLOAD + 1)
        );
    }

    /// 解码:截断、多余字节、非 UTF-8、未知状态、行数虚报。
    #[test]
    fn reject_bad_payloads() {
        let mut p = complete_payload(&CompleteResult::Failed("x".into()));
        p.push(0);
        assert_eq!(decode_complete(&p).unwrap_err(), ProtocolError::TrailingBytes);

        assert_eq!(
            decode_session_ready(&[1, 0]).unwrap_err(),
            ProtocolError::TrailingBytes
        );
        assert_eq!(decode_session_ready(&[]).unwrap_err(), ProtocolError::Truncated);
        assert_eq!(
            decode_image_consumed(&[0; 15]).unwrap_err(),
            ProtocolError::Truncated
        );
        assert_eq!(
            decode_image_consumed(&[0; 17]).unwrap_err(),
            ProtocolError::TrailingBytes
        );

        assert_eq!(
            decode_complete(&[0, 1, 0, 0, 0, 0xff]).unwrap_err(),
            ProtocolError::InvalidUtf8
        );
        assert_eq!(
            decode_ready(&[1, 0, 1, 0, 0, 0, 0xff]).unwrap_err(),
            ProtocolError::InvalidUtf8
        );
        assert_eq!(decode_ready(&[1, 0, 1, 0, 0, 0]).unwrap_err(), ProtocolError::Truncated);
        assert_eq!(decode_complete(&[7]).unwrap_err(), ProtocolError::BadKind(7));

        // 声明 u32::MAX 行但无数据:不得爆内存,只返回 Truncated。
        let mut lie = vec![STATUS_SUCCESS, 0, 0, 0, 0];
        lie.extend_from_slice(&u32::MAX.to_le_bytes());
        assert_eq!(decode_complete(&lie).unwrap_err(), ProtocolError::Truncated);
    }

    /// 槽头各偏移字段。
    #[test]
    fn slot_header_layout() {
        let mut buf = [0u8; 40];
        write_slot_header(&mut buf, 123, 100, 200).unwrap();
        assert_eq!(u64::from_le_bytes(buf[0..8].try_into().unwrap()), 123);
        assert_eq!(u32_at(&buf, 8), 1);
        assert_eq!(u32_at(&buf, 12), 100);
        assert_eq!(u32_at(&buf, 16), 200);
        assert_eq!(u32_at(&buf, 20), 400);
        assert_eq!(u32_at(&buf, 24), 80000);
        assert_eq!(u32_at(&buf, 28), SLOT_MAGIC);
    }

    /// 槽头非法参数。
    #[test]
    fn slot_header_rejects() {
        let mut buf = [0u8; 32];
        assert!(write_slot_header(&mut buf, 1, 0, 10).is_err());
        assert!(write_slot_header(&mut buf, 1, 10, 0).is_err());
        assert!(write_slot_header(&mut buf, 1, 3841, 2160).is_err());
        assert!(write_slot_header(&mut buf, 1, u32::MAX, u32::MAX).is_err());
        let mut small = [0u8; 31];
        assert_eq!(
            write_slot_header(&mut small, 1, 10, 10).unwrap_err(),
            ProtocolError::Truncated
        );
        // 恰好 MAX_PIXELS 合法。
        assert!(write_slot_header(&mut buf, 1, 3840, 2160).is_ok());
    }

    /// BGRA 转 RGBA 逐像素与长度错误。
    #[test]
    fn bgra_rgba() {
        let src = [1, 2, 3, 4, 10, 20, 30, 40];
        let mut dst = [0u8; 8];
        bgra_to_rgba(&mut dst, &src).unwrap();
        assert_eq!(dst, [3, 2, 1, 4, 30, 20, 10, 40]);

        let mut d3 = [0u8; 3];
        assert_eq!(bgra_to_rgba(&mut d3, &[1, 2, 3]).unwrap_err(), ProtocolError::Truncated);
        let mut d4 = [0u8; 4];
        assert_eq!(bgra_to_rgba(&mut d4, &src).unwrap_err(), ProtocolError::Truncated);
    }

    /// Kind 数值互转覆盖全部变体。
    #[test]
    fn kind_roundtrip() {
        for v in 1..=18u16 {
            assert_eq!(Kind::from_u16(v).unwrap().as_u16(), v);
        }
        assert_eq!(Kind::from_u16(0), None);
        assert_eq!(Kind::from_u16(19), None);
    }
}
