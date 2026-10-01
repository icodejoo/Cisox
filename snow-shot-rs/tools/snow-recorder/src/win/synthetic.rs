//! 合成采集源与无屏流水线基准（仅测试）。
//!
//! 用一个按固定节拍把噪声纹理复制进共享槽的"假采集"代替桌面复制，其余阶段（共享槽 + 栅栏、
//! VideoProcessor 合成、QSV 编码、封装）全部是真实实现。不依赖桌面/显示器（屏保、锁屏时也能跑），
//! 用来量出流水线自身的吞吐上限与阶段耗时，和夹具实测（含真实 DXGI 采集）对照。
//!
//! 基准默认不跑（耗时且占 GPU）；设环境变量 `SNOW_RECORDER_SYNTH_BENCH=1` 后运行
//! `cargo test --release synthetic_pipeline_benchmark`，结果打印到 stderr。

use std::sync::Arc;
use std::time::{Duration, Instant};

use snow_cursor::{AttachedCursorSample, CursorCompositionMode, CursorShape, CursorShapeState};
use snow_d3d11::SharedDevice;
use windows::Win32::Graphics::Direct3D11::*;
use windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT_B8G8R8A8_UNORM;
use windows::core::Interface;

use crate::pipeline::{CaptureDiag, CaptureFault, CaptureSource, CaptureStats, Captured};
use crate::win::dda::{GpuFrame, SharedSlot, copy_into_slot, create_device_pair, create_slot};

/// 共享槽数量。
const SLOTS: usize = 10;
/// 噪声内容的变体数（轮流复制，避免被缓存优化掉）。
const VARIANTS: usize = 4;

/// 合成采集源：按固定节拍产出"呈现时间恰好等距"的帧。
pub struct SyntheticCapture {
    /// 采集设备 A。
    device: SharedDevice,
    /// 设备 A 的带栅栏上下文。
    context4: ID3D11DeviceContext4,
    /// 共享槽池。
    slots: Vec<Arc<SharedSlot>>,
    /// 噪声内容（设备 A 上的纹理）。
    content: Vec<ID3D11Texture2D>,
    /// 复制区域（整幅）。
    crop: D3D11_BOX,
    /// 起点（第一次取帧时才确定，避免装配耗时被当成"迟到"而突发补帧）。
    origin: Option<Instant>,
    /// 帧间隔。
    period: Duration,
    /// 下一帧序号。
    seq: u64,
    /// 计数。
    stats: CaptureStats,
    /// 是否附带一个在画面里移动的合成光标。
    with_cursor: bool,
    /// 合成光标形状。
    cursor_shape: CursorShape,
    /// 画面尺寸。
    size: (u32, u32),
}

/// 生成 `len` 字节的伪随机噪声（BGRA，alpha 固定不透明）。
fn noise(len: usize, mut state: u64) -> Vec<u8> {
    let mut out = Vec::with_capacity(len);
    while out.len() < len {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        let b = state.to_le_bytes();
        out.extend_from_slice(&[b[0], b[1], b[2], 255]);
    }
    out.truncate(len);
    out
}

/// 生成"类桌面"的确定性内容（BGRA）：渐变底、文字状细条、网格线、随 `variant` 平移的色块和轻微噪点。
/// 用于画质对照（噪声内容对码率/PSNR 太极端）；同一 `variant` 总产出同样的像素。
fn natural(width: u32, height: u32, variant: u32) -> Vec<u8> {
    let (w, h) = (width as usize, height as usize);
    let mut out = vec![255u8; w * h * 4];
    let mut state = 0x2545_F491_4F6C_DD1Du64 ^ u64::from(variant + 1);
    let shift = (variant as usize) * 7;
    for y in 0..h {
        for x in 0..w {
            let i = (y * w + x) * 4;
            // 缓变渐变底
            let mut b = (40 + x * 120 / w) as i32;
            let mut g = (50 + y * 120 / h) as i32;
            let mut r = (70 + (x + y) * 90 / (w + h)) as i32;
            // 文字状细条：每 24 行一行"文字"，条宽随列伪随机
            if (y % 24) >= 6 && (y % 24) < 16 && ((x + shift) / 5 % 7) < 4 && !((x / 5 * 2654435761usize) >> 7).is_multiple_of(3) {
                b = 235;
                g = 235;
                r = 235;
            }
            // 网格线
            if x % 160 == 0 || y % 120 == 0 {
                b = 20;
                g = 20;
                r = 20;
            }
            // 平移色块
            if ((x + shift * 5) / 200 + y / 160).is_multiple_of(5) && (y % 160) > 100 {
                r = 200;
                g = 90;
                b = 60;
            }
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            let jitter = (state & 3) as i32 - 1;
            out[i] = (b + jitter).clamp(0, 255) as u8;
            out[i + 1] = (g + jitter).clamp(0, 255) as u8;
            out[i + 2] = (r + jitter).clamp(0, 255) as u8;
        }
    }
    out
}

impl SyntheticCapture {
    /// 创建合成采集源。
    ///
    /// # 参数
    /// - `a`：采集设备 A。
    /// - `b`：合成设备 B。
    /// - `size`：画面尺寸。
    /// - `fps`：出帧速率。
    /// - `with_cursor`：是否附带移动的合成光标。
    pub fn new(a: SharedDevice, b: &SharedDevice, size: (u32, u32), fps: u32, with_cursor: bool) -> Result<Self, String> {
        let slots = (0..SLOTS).map(|_| create_slot(&a, b, size).map(Arc::new)).collect::<Result<Vec<_>, _>>()?;
        let binds = (D3D11_BIND_SHADER_RESOURCE | D3D11_BIND_RENDER_TARGET).0 as u32;
        let mut content = Vec::new();
        for i in 0..VARIANTS {
            let texture = a.texture(size.0, size.1, DXGI_FORMAT_B8G8R8A8_UNORM, binds).map_err(|e| format!("{e:#}"))?;
            let pixels = if std::env::var("SNOW_RECORDER_SYNTH_CONTENT").as_deref() == Ok("natural") {
                natural(size.0, size.1, i as u32)
            } else {
                noise((size.0 * size.1 * 4) as usize, 0x9E37_79B9_7F4A_7C15 ^ (i as u64 + 1))
            };
            // 画质对照用：把各变体原始像素按序追加到参考文件（ffmpeg 据此重建参考序列）
            if let Ok(dir) = std::env::var("SNOW_RECORDER_SYNTH_KEEP") {
                use std::io::Write;
                let path = std::path::Path::new(&dir).join(format!("ref-{}x{}.bgra", size.0, size.1));
                let mut file = std::fs::OpenOptions::new().create(true).append(i > 0).write(true).truncate(i == 0).open(path).map_err(|e| e.to_string())?;
                file.write_all(&pixels).map_err(|e| e.to_string())?;
            }
            let _lock = a.lock();
            // SAFETY: 缓冲长度 = 宽*高*4，行距 = 宽*4。
            unsafe { a.context().UpdateSubresource(texture.raw(), 0, None, pixels.as_ptr().cast(), size.0 * 4, 0) };
            content.push(texture.raw().clone());
        }
        let context4: ID3D11DeviceContext4 = a.context().cast().map_err(|e| e.to_string())?;
        Ok(Self {
            device: a,
            context4,
            slots,
            content,
            crop: D3D11_BOX { left: 0, top: 0, front: 0, right: size.0, bottom: size.1, back: 1 },
            origin: None,
            period: Duration::from_secs_f64(1.0 / f64::from(fps)),
            seq: 0,
            stats: CaptureStats::default(),
            with_cursor,
            cursor_shape: CursorShape::from_rgba(2, 2, 24, 24, CursorCompositionMode::AlphaBlend, noise(24 * 24 * 4, 7)),
            size,
        })
    }
}

impl CaptureSource for SyntheticCapture {
    type Frame = GpuFrame;

    /// 等到下一个计划时刻，把噪声纹理复制进空闲槽。
    fn next(&mut self, timeout: Duration, want: bool) -> Result<Option<Captured<GpuFrame>>, CaptureFault> {
        let origin = *self.origin.get_or_insert_with(Instant::now);
        let due = origin + self.period.mul_f64(self.seq as f64);
        let now = Instant::now();
        if due > now {
            std::thread::sleep((due - now).min(timeout));
            if Instant::now() < due {
                return Ok(None);
            }
        }
        let variant = (self.seq as usize) % VARIANTS;
        self.seq += 1;
        if !want {
            return Ok(None);
        }
        let Some(slot) = self.slots.iter().find(|s| Arc::strong_count(s) == 1).cloned() else {
            self.stats.pool_drops += 1;
            return Ok(None);
        };
        copy_into_slot(&self.device, &self.context4, &slot, &self.content[variant], &self.crop)?;
        {
            let _lock = self.device.lock();
            // SAFETY: 持有设备锁。
            unsafe { self.device.context().Flush() };
        }
        self.stats.frames += 1;
        let cursor = self.with_cursor.then(|| {
            // 光标按序号在画面里来回扫（含越过边缘的位置，覆盖裁剪路径）
            let t = self.seq as i64;
            AttachedCursorSample {
                x: ((t * 7) % (i64::from(self.size.0) + 40) - 20) as i32,
                y: ((t * 5) % (i64::from(self.size.1) + 40) - 20) as i32,
                visible: true,
                shape: CursorShapeState::Embedded(self.cursor_shape.clone()),
            }
        });
        Ok(Some(Captured { frame: GpuFrame::single(slot), cursor, present: due, captured_at: Instant::now(), fresh: true, id: self.seq }))
    }

    /// 合成源不会失效。
    fn recreate(&mut self) -> Result<(), String> {
        Ok(())
    }

    /// 当前计数。
    fn stats(&self) -> CaptureStats {
        self.stats
    }

    /// 没有额外诊断。
    fn take_diag(&mut self) -> CaptureDiag {
        CaptureDiag::default()
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;
    use crate::pipeline::{self, PipelineConfig};
    use crate::settings::{AdapterInfo, ENV_ENCODER, EncoderPreference, parse_encoder_preference, select_encoder};
    use crate::win::compose::GpuComposer;
    use crate::win::hwenc::{DEFAULT_POOL_CAPACITY, HwConfig, HwContext, HwEncoder, QSV_ASYNC_DEPTH, QSV_PRESET};
    use crate::win::mfenc::{ENV_MF_QUALITY, MF_POOL_CAPACITY, MfEncoder, parse_rate_control};

    /// 噪声发生器：长度精确、alpha 不透明、不同种子内容不同。
    #[test]
    fn noise_is_sized_opaque_and_seeded() {
        let a = noise(4 * 10, 1);
        let b = noise(4 * 10, 2);
        assert_eq!(a.len(), 40);
        assert!(a.chunks(4).all(|p| p[3] == 255));
        assert_ne!(a, b);
        assert_eq!(noise(7, 1).len(), 7);
    }

    /// 读回成品：`(视频包数, 封装里的总时长秒)`；读不了返回错误。
    fn inspect(path: &std::path::Path) -> Result<(u64, f64), String> {
        let mut input = ffmpeg_next::format::input(path).map_err(|e| e.to_string())?;
        let packets = input.packets().filter(|(s, p)| s.parameters().medium() == ffmpeg_next::media::Type::Video && p.size() > 0).count() as u64;
        let seconds = input.duration() as f64 / f64::from(ffmpeg_next::ffi::AV_TIME_BASE);
        Ok((packets, seconds))
    }

    /// 合成基准默认质量参数。白噪声在 qp18 下 1080p 的 IDR 帧会超出 NVENC 输出缓冲，
    /// 首帧 lock bitstream 即报 invalid param(8)；24 起正常（真实画面不受影响）。
    const SYNTH_DEFAULT_QUALITY: u32 = 24;

    /// 解析质量参数覆盖值；缺省或非法取默认值。
    fn parse_synth_quality(raw: Option<&str>) -> u32 {
        raw.and_then(|v| v.trim().parse().ok()).unwrap_or(SYNTH_DEFAULT_QUALITY)
    }

    /// 合成基准的编码质量参数，可用 `SNOW_RECORDER_SYNTH_QUALITY` 覆盖。
    fn synth_quality() -> u32 {
        parse_synth_quality(std::env::var("SNOW_RECORDER_SYNTH_QUALITY").ok().as_deref())
    }

    /// 质量参数解析：缺省/非法回落默认，合法值原样使用。
    #[test]
    fn synth_quality_parses_with_fallback() {
        assert_eq!(parse_synth_quality(None), SYNTH_DEFAULT_QUALITY);
        assert_eq!(parse_synth_quality(Some("abc")), SYNTH_DEFAULT_QUALITY);
        assert_eq!(parse_synth_quality(Some(" 30 ")), 30);
    }

    /// 跑一档无屏流水线，返回（诊断摘要, 编码帧数, 期望帧数, 成品包数, 成品时长秒）。
    fn run_tier(size: (u32, u32), out: (u32, u32), fps: u32, seconds: f64, cursor: bool) -> Result<(String, u64, u64, u64, f64), String> {
        // 编码器偏好沿用环境变量；适配器按它挑（多显卡机器可用 SNOW_RECORDER_ENCODER=nvenc 指定独显）
        let preference = parse_encoder_preference(std::env::var(ENV_ENCODER).ok().as_deref()).unwrap_or(EncoderPreference::Auto);
        if std::env::var("SNOW_RECORDER_FFLOG").is_ok() {
            ffmpeg_next::util::log::set_level(ffmpeg_next::util::log::Level::Debug);
        }
        let (a, b) = create_device_pair(preference)?;
        let identity = b.identity();
        let adapter = AdapterInfo { vendor: identity.vendor, description: identity.description.clone() };
        let codec = select_encoder(&adapter, preference)?;
        let capture = SyntheticCapture::new(a, &b, size, fps, cursor)?;
        let use_mf = std::env::var("SNOW_RECORDER_SYNTH_ENCODER").as_deref() == Ok("mf");
        let keep = std::env::var("SNOW_RECORDER_SYNTH_KEEP").ok();
        let hw = Arc::new(if use_mf {
            HwContext::with_align(b.clone(), out, MF_POOL_CAPACITY, codec, 2, false)?
        } else {
            HwContext::new(b.clone(), out, DEFAULT_POOL_CAPACITY, codec)?
        });
        hw.prewarm(8)?;
        let composer = GpuComposer::new(b, Arc::clone(&hw), size, out, fps)?;
        let rate = parse_rate_control(std::env::var(ENV_MF_QUALITY).ok().as_deref());
        let tag = if use_mf { format!("mf-{rate:?}") } else { format!("{}-q{}", codec.ffmpeg_name(), synth_quality()) };
        let path: PathBuf = match &keep {
            Some(dir) => std::path::Path::new(dir).join(format!("{tag}-{}x{}-{}.mp4", out.0, out.1, fps)),
            None => std::env::temp_dir().join(format!("snow-synth-{}x{}-{}.mp4", size.0, size.1, fps)),
        };
        let running = if use_mf {
            let encoder = MfEncoder::open(Arc::clone(&hw), &path, out, fps, rate)?;
            pipeline::start(PipelineConfig { fps, show_cursor: cursor }, "synthetic+videoprocessor+media-foundation", capture, composer, encoder)?
        } else {
            let encoder = HwEncoder::open(
                Arc::clone(&hw),
                &HwConfig { path: path.clone(), width: out.0, height: out.1, fps, quality: synth_quality(), async_depth: QSV_ASYNC_DEPTH, preset: QSV_PRESET.into() },
            )?;
            pipeline::start(PipelineConfig { fps, show_cursor: cursor }, &format!("synthetic+videoprocessor+{}", codec.ffmpeg_name()), capture, composer, encoder)?
        };
        std::thread::sleep(Duration::from_secs_f64(seconds));
        let report = running.stop()?;
        let (packets, duration) = inspect(&path)?;
        if keep.is_none() {
            let _ = std::fs::remove_file(&path);
        }
        Ok((report.describe(), report.encoded_frames, (seconds * f64::from(fps)) as u64, packets, duration))
    }

    /// 最小编码烟测：合成源 -> VideoProcessor -> 当前适配器的硬件编码器 -> MP4，成品必须有视频包；
    /// 机器没有可用硬编（或编码器打不开）时打印原因并跳过，不算失败。
    #[test]
    fn synthetic_encode_smoke() {
        match run_tier((1280, 720), (1280, 720), 30, 1.5, false) {
            Ok((_, frames, _, packets, duration)) => {
                eprintln!("烟测通过：编码 {frames} 帧，成品 {packets} 包，{duration:.2}s");
                assert!(frames > 0 && packets > 0 && duration > 0.5);
            }
            Err(e) => eprintln!("烟测跳过（无可用硬件编码）: {e}"),
        }
    }

    /// 无屏流水线基准（默认跳过，见模块文档）：四档分辨率/帧率，各 6 秒。
    #[test]
    fn synthetic_pipeline_benchmark() {
        if std::env::var("SNOW_RECORDER_SYNTH_BENCH").is_err() {
            return;
        }
        // （选区尺寸, 输出尺寸, 帧率, 是否带光标）：含 1440p 缩放到 1080p 与光标图层的路径
        let tiers = [
            ((1920, 1080), (1920, 1080), 30, false),
            ((1920, 1080), (1920, 1080), 60, false),
            ((2560, 1440), (2560, 1440), 30, false),
            ((2560, 1440), (2560, 1440), 60, false),
            ((2560, 1440), (1920, 1080), 60, true),
            ((1920, 1080), (1920, 1080), 60, true),
        ];
        // 可选：SNOW_RECORDER_SYNTH_TIERS=0,3 只跑指定序号的档位
        let only: Option<Vec<usize>> = std::env::var("SNOW_RECORDER_SYNTH_TIERS")
            .ok()
            .map(|t| t.split(',').filter_map(|x| x.trim().parse().ok()).collect());
        for (idx, (size, out, fps, cursor)) in tiers.into_iter().enumerate() {
            if only.as_ref().is_some_and(|v| !v.contains(&idx)) {
                continue;
            }
            match run_tier(size, out, fps, 6.0, cursor) {
                Ok((text, frames, expected, packets, duration)) => eprintln!(
                    "== {}x{}->{}x{}@{} 光标={cursor}: 编码 {frames} 帧 / 期望 {expected}；成品包数 {packets}，封装时长 {duration:.2}s\n{text}",
                    size.0, size.1, out.0, out.1, fps
                ),
                Err(e) => eprintln!("== {}x{}->{}x{}@{}: 跳过/失败: {e}", size.0, size.1, out.0, out.1, fps),
            }
        }
    }
}
