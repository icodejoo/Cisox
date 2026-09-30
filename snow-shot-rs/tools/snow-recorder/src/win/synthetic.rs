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
            let pixels = noise((size.0 * size.1 * 4) as usize, 0x9E37_79B9_7F4A_7C15 ^ (i as u64 + 1));
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
        Ok(Some(Captured { frame: GpuFrame { slot }, cursor, present: due, captured_at: Instant::now(), fresh: true }))
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
    use crate::win::compose::GpuComposer;
    use crate::win::hwenc::{DEFAULT_POOL_CAPACITY, HwConfig, HwContext, HwEncoder, QSV_ASYNC_DEPTH, QSV_PRESET};

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

    /// 跑一档无屏流水线，返回（诊断摘要, 编码帧数, 期望帧数, 成品包数, 成品时长秒）。
    fn run_tier(size: (u32, u32), out: (u32, u32), fps: u32, seconds: f64, cursor: bool) -> Result<(String, u64, u64, u64, f64), String> {
        let (a, b) = create_device_pair()?;
        let capture = SyntheticCapture::new(a, &b, size, fps, cursor)?;
        let hw = Arc::new(HwContext::new(b.clone(), out, DEFAULT_POOL_CAPACITY)?);
        hw.prewarm(8)?;
        let composer = GpuComposer::new(b, Arc::clone(&hw), size, out, fps)?;
        let path: PathBuf = std::env::temp_dir().join(format!("snow-synth-{}x{}-{}.mp4", size.0, size.1, fps));
        let encoder = HwEncoder::open(
            Arc::clone(&hw),
            &HwConfig { path: path.clone(), width: out.0, height: out.1, fps, quality: 18, async_depth: QSV_ASYNC_DEPTH, preset: QSV_PRESET.into() },
        )?;
        let running = pipeline::start(PipelineConfig { fps, show_cursor: cursor }, "synthetic+videoprocessor+qsv", capture, composer, encoder)?;
        std::thread::sleep(Duration::from_secs_f64(seconds));
        let report = running.stop()?;
        let (packets, duration) = inspect(&path)?;
        let _ = std::fs::remove_file(&path);
        Ok((report.describe(), report.encoded_frames, (seconds * f64::from(fps)) as u64, packets, duration))
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
        for (size, out, fps, cursor) in tiers {
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
