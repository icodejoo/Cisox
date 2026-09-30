//! 端到端：转换 + h264_qsv 编码（复用上游 gpu.rs 的映射方式：D3D11VA 帧池 -> 派生 QSV 帧上下文 -> av_hwframe_map）。

use std::collections::VecDeque;
use std::ffi::c_void;
use std::ptr;
use std::time::Instant;

use ffmpeg::ffi::*;
use ffmpeg_next as ffmpeg;
use windows::Win32::Graphics::Direct3D11::{ID3D11Multithread, ID3D11Texture2D};
use windows::core::Interface;

use crate::bench::{Conv, cpu_ms, gpu_mem_mb, peak_ws_mb};
use crate::gpu::*;

fn check(code: i32, what: &str) -> Res<()> {
    if code < 0 {
        err(format!("{what}: {}", ffmpeg::Error::from(code)))
    } else {
        Ok(())
    }
}

/// AVBufferRef 所有者。
struct Buf(*mut AVBufferRef);
impl Buf {
    fn new(p: *mut AVBufferRef, what: &str) -> Res<Self> {
        if p.is_null() { err(format!("{what} 分配失败")) } else { Ok(Self(p)) }
    }
    fn reference(&self) -> *mut AVBufferRef {
        unsafe { av_buffer_ref(self.0) }
    }
}
impl Drop for Buf {
    fn drop(&mut self) {
        unsafe { av_buffer_unref(&mut self.0) };
    }
}

unsafe extern "C" fn lock_dev(opaque: *mut c_void) {
    unsafe { (&*opaque.cast::<ID3D11Multithread>()).Enter() };
}
unsafe extern "C" fn unlock_dev(opaque: *mut c_void) {
    unsafe { (&*opaque.cast::<ID3D11Multithread>()).Leave() };
}

/// QSV 参数。
#[derive(Clone, Debug)]
pub struct QsvOpts {
    /// async_depth。
    pub async_depth: u32,
    /// preset（medium/veryfast 等）。
    pub preset: String,
    /// 帧池纹理 BindFlags（可含 VIDEO_ENCODER / UNORDERED_ACCESS）。
    pub bind: u32,
    /// 质量参数（global_quality）。
    pub quality: u32,
}

/// 转换 + QSV 编码流水线。
pub struct QsvPipe {
    _device: Buf,
    native: Buf,
    _qsv: Buf,
    mapped: Buf,
    enc: ffmpeg::encoder::video::Encoder,
    w: u32,
    h: u32,
    keep: VecDeque<ffmpeg::frame::Video>,
    keep_n: usize,
    /// 已输出字节数。
    pub bytes: u64,
    /// 已输出包数。
    pub packets: u64,
}

impl QsvPipe {
    /// 创建流水线，失败返回具体原因（如 QSV 不可用）。
    pub fn new(gpu: &Gpu, w: u32, h: u32, fps: u32, o: &QsvOpts) -> Res<Self> {
        ffmpeg::init()?;
        let device = Buf::new(unsafe { av_hwdevice_ctx_alloc(AVHWDeviceType::AV_HWDEVICE_TYPE_D3D11VA) }, "hwdevice")?;
        unsafe {
            let c = (*device.0).data.cast::<AVHWDeviceContext>();
            let d = (*c).hwctx.cast::<AVD3D11VADeviceContext>();
            (*d).device = gpu.dev.clone().into_raw().cast();
            (*d).device_context = gpu.ctx.clone().into_raw().cast();
            let owner = Box::into_raw(Box::new(gpu.mt.clone())).cast::<c_void>();
            (*d).lock_ctx = owner;
            (*d).lock = Some(lock_dev);
            (*d).unlock = Some(unlock_dev);
            check(av_hwdevice_ctx_init(device.0), "hwdevice init")?;
        }
        let native = Buf::new(unsafe { av_hwframe_ctx_alloc(device.0) }, "hwframes")?;
        unsafe {
            let c = (*native.0).data.cast::<AVHWFramesContext>();
            (*c).format = AVPixelFormat::AV_PIX_FMT_D3D11;
            (*c).sw_format = AVPixelFormat::AV_PIX_FMT_NV12;
            (*c).width = (w.div_ceil(16) * 16) as i32;
            (*c).height = (h.div_ceil(16) * 16) as i32;
            (*c).initial_pool_size = 0;
            let d = (*c).hwctx.cast::<AVD3D11VAFramesContext>();
            (*d).BindFlags = o.bind;
            check(av_hwframe_ctx_init(native.0), "hwframes init")?;
        }
        let mut q = ptr::null_mut();
        check(
            unsafe {
                av_hwdevice_ctx_create_derived(&mut q, AVHWDeviceType::AV_HWDEVICE_TYPE_QSV, device.0, 0)
            },
            "derive QSV device",
        )?;
        let qsv = Buf::new(q, "qsv device")?;
        let mut m = ptr::null_mut();
        check(
            unsafe {
                av_hwframe_ctx_create_derived(
                    &mut m,
                    AVPixelFormat::AV_PIX_FMT_QSV,
                    qsv.0,
                    native.0,
                    AV_HWFRAME_MAP_READ as i32 | AV_HWFRAME_MAP_DIRECT as i32,
                )
            },
            "derive QSV frames",
        )?;
        let mapped = Buf::new(m, "qsv frames")?;
        let codec = ffmpeg::encoder::find_by_name("h264_qsv").ok_or("找不到 h264_qsv")?;
        let mut v = ffmpeg::codec::Context::new_with_codec(codec).encoder().video()?;
        v.set_width(w);
        v.set_height(h);
        v.set_format(ffmpeg::format::Pixel::QSV);
        v.set_time_base((1, fps as i32));
        v.set_frame_rate(Some((fps as i32, 1)));
        v.set_gop(fps * 2);
        unsafe {
            let c = v.as_mut_ptr();
            (*c).hw_device_ctx = qsv.reference();
            (*c).hw_frames_ctx = mapped.reference();
            (*c).max_b_frames = 0;
            (*c).color_range = AVColorRange::AVCOL_RANGE_MPEG;
            (*c).colorspace = AVColorSpace::AVCOL_SPC_BT709;
            (*c).color_primaries = AVColorPrimaries::AVCOL_PRI_BT709;
            (*c).color_trc = AVColorTransferCharacteristic::AVCOL_TRC_BT709;
        }
        let mut opts = ffmpeg::Dictionary::new();
        opts.set("bf", "0");
        opts.set("preset", &o.preset);
        opts.set("look_ahead", "0");
        opts.set("async_depth", &o.async_depth.to_string());
        opts.set("global_quality", &o.quality.to_string());
        let enc = v.open_as_with(codec, opts)?;
        Ok(Self {
            _device: device,
            native,
            _qsv: qsv,
            mapped,
            enc,
            w,
            h,
            keep: VecDeque::new(),
            keep_n: o.async_depth as usize + 4,
            bytes: 0,
            packets: 0,
        })
    }

    /// 从帧池取一张 NV12 帧（纹理 + 帧对象）。
    fn alloc(&self) -> Res<(ffmpeg::frame::Video, ID3D11Texture2D)> {
        let mut f = ffmpeg::frame::Video::empty();
        check(unsafe { av_hwframe_get_buffer(self.native.0, f.as_mut_ptr(), 0) }, "get_buffer")?;
        f.set_width(self.w);
        f.set_height(self.h);
        let raw = unsafe { (*f.as_ptr()).data[0].cast::<c_void>() };
        let tex = unsafe { ID3D11Texture2D::from_raw_borrowed(&raw) }.cloned().ok_or("帧无纹理")?;
        Ok((f, tex))
    }

    /// 取一张帧池纹理（不编码），用于正确性校验等。
    pub fn alloc_texture(&self) -> Res<(ffmpeg::frame::Video, ID3D11Texture2D)> {
        self.alloc()
    }

    fn submit(&mut self, native: ffmpeg::frame::Video, pts: i64) -> Res<()> {
        let mut out = ffmpeg::frame::Video::empty();
        unsafe {
            (*out.as_mut_ptr()).format = AVPixelFormat::AV_PIX_FMT_QSV as i32;
            (*out.as_mut_ptr()).hw_frames_ctx = self.mapped.reference();
            check(
                av_hwframe_map(
                    out.as_mut_ptr(),
                    native.as_ptr(),
                    AV_HWFRAME_MAP_READ as i32 | AV_HWFRAME_MAP_DIRECT as i32,
                ),
                "hwframe_map",
            )?;
            check(av_frame_copy_props(out.as_mut_ptr(), native.as_ptr()), "copy_props")?;
        }
        out.set_pts(Some(pts));
        self.enc.send_frame(&out)?;
        self.keep.push_back(native);
        while self.keep.len() > self.keep_n {
            self.keep.pop_front();
        }
        self.drain();
        Ok(())
    }

    fn drain(&mut self) {
        let mut p = ffmpeg::Packet::empty();
        while self.enc.receive_packet(&mut p).is_ok() {
            self.bytes += p.size() as u64;
            self.packets += 1;
        }
    }

    /// 结束：送 EOF 并取完剩余包。
    pub fn finish(&mut self) -> Res<()> {
        self.enc.send_eof()?;
        self.drain();
        Ok(())
    }
}

/// 端到端结果。
pub struct E2eResult {
    /// 每帧转换 CPU 耗时。
    pub conv_cpu: Stats,
    /// 转换 GPU 耗时。
    pub conv_gpu: Stats,
    /// 送帧（send_frame + 取包）耗时。
    pub send: Stats,
    /// 每帧总耗时（转换+送帧，含池取帧）。
    pub total: Stats,
    /// 吞吐 fps（整段墙钟，含 flush 之前）。
    pub fps: f64,
    /// 进程 CPU（单核当量 %）。
    pub proc_cpu_pct: f64,
    /// 峰值工作集 MB。
    pub peak_ws: f64,
    /// 显存 local / non-local MB。
    pub mem: (f64, f64),
    /// 输出包数。
    pub packets: u64,
    /// 输出字节。
    pub bytes: u64,
}

/// 运行端到端基准：前 `warmup` 帧丢弃。
pub fn bench_e2e(gpu: &Gpu, pipe: &mut QsvPipe, conv: &mut dyn Conv, warmup: usize, frames: usize) -> Res<E2eResult> {
    let mut timer = GpuTimer::new(&gpu.dev, 8)?;
    let (mut gpu_ms, mut c_cpu, mut send, mut total) = (Vec::new(), Vec::new(), Vec::new(), Vec::new());
    let (mut wall, mut cpu0) = (Instant::now(), 0.0);
    let mut mem = (0.0f64, 0.0f64);
    for i in 0..warmup + frames {
        let m = i >= warmup;
        if i == warmup {
            gpu.wait_idle()?;
            wall = Instant::now();
            cpu0 = cpu_ms();
        }
        let t0 = Instant::now();
        let (f, tex) = pipe.alloc()?;
        if m {
            timer.begin(&gpu.ctx, &mut gpu_ms);
        }
        let t1 = Instant::now();
        conv.run(i, &tex)?;
        let t2 = Instant::now();
        if m {
            timer.end(&gpu.ctx);
        }
        pipe.submit(f, i as i64)?;
        let t3 = Instant::now();
        if m {
            c_cpu.push((t2 - t1).as_secs_f64() * 1000.0);
            send.push((t3 - t2).as_secs_f64() * 1000.0);
            total.push((t3 - t0).as_secs_f64() * 1000.0);
        }
        if i % 100 == 0 {
            let mm = gpu_mem_mb(gpu);
            mem = (mem.0.max(mm.0), mem.1.max(mm.1));
        }
    }
    pipe.finish()?;
    let secs = wall.elapsed().as_secs_f64();
    let used = cpu_ms() - cpu0;
    timer.drain(&gpu.ctx, true, &mut gpu_ms);
    let mm = gpu_mem_mb(gpu);
    mem = (mem.0.max(mm.0), mem.1.max(mm.1));
    Ok(E2eResult {
        conv_cpu: Stats::from(&c_cpu),
        conv_gpu: Stats::from(&gpu_ms),
        send: Stats::from(&send),
        total: Stats::from(&total),
        fps: frames as f64 / secs,
        proc_cpu_pct: used / (secs * 1000.0) * 100.0,
        peak_ws: peak_ws_mb(),
        mem,
        packets: pipe.packets,
        bytes: pipe.bytes,
    })
}

impl E2eResult {
    /// 序列化为 JSON 片段（不含外层花括号）。
    pub fn json(&self) -> String {
        format!(
            "\"conv_cpu_ms\":{},\"conv_gpu_ms\":{},\"send_ms\":{},\"frame_total_ms\":{},\"fps\":{:.1},\"proc_cpu_pct\":{:.1},\"peak_ws_mb\":{:.0},\"gpu_local_mb\":{:.0},\"gpu_nonlocal_mb\":{:.0},\"packets\":{},\"bytes\":{}",
            self.conv_cpu.json(),
            self.conv_gpu.json(),
            self.send.json(),
            self.total.json(),
            self.fps,
            self.proc_cpu_pct,
            self.peak_ws,
            self.mem.0,
            self.mem.1,
            self.packets,
            self.bytes
        )
    }
}
