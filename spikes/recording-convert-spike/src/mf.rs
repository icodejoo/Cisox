//! 方案 2：Media Foundation。
//! - `MfConv`：Video Processor MFT（CLSID_VideoProcessorMFT），RGB32 零拷贝（IMFDXGIDeviceManager + DXGI surface buffer）-> NV12。
//! - `probe`：枚举硬件 H.264 编码 MFT，记录其可用输入类型，并直接尝试 SetInputType(RGB32)。
//! - `e2e`：VPMFT -> 硬件 H.264 编码 MFT（异步事件驱动）的端到端。

use std::ffi::c_void;
use std::mem::ManuallyDrop;
use std::time::Instant;

use windows::Win32::Graphics::Direct3D11::*;
use windows::Win32::Media::MediaFoundation::*;
use windows::Win32::System::Com::*;
use windows::core::{GUID, Interface};

use crate::bench::{Conv, ConvResult, bench_convert, cpu_ms, gpu_mem_mb, peak_ws_mb};
use crate::gpu::*;

const GUID_NAMES: [(&str, GUID); 8] = [
    ("NV12", MFVideoFormat_NV12),
    ("RGB32", MFVideoFormat_RGB32),
    ("ARGB32", MFVideoFormat_ARGB32),
    ("YUY2", MFVideoFormat_YUY2),
    ("IYUV", MFVideoFormat_IYUV),
    ("I420", MFVideoFormat_I420),
    ("YV12", MFVideoFormat_YV12),
    ("H264", MFVideoFormat_H264),
];

fn guid_name(g: &GUID) -> String {
    GUID_NAMES
        .iter()
        .find(|(_, x)| x == g)
        .map(|(n, _)| n.to_string())
        .unwrap_or_else(|| format!("{g:?}"))
}

/// 初始化 COM 与 MF（进程内幂等）。
fn init_mf() -> Res<()> {
    unsafe {
        let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
        MFStartup(MF_VERSION, MFSTARTUP_FULL)?;
    }
    Ok(())
}

/// 打包两个 u32 为 MF 的 64 位属性（高位在前）。
fn pack(a: u32, b: u32) -> u64 {
    ((a as u64) << 32) | b as u64
}

/// 构造视频媒体类型。
fn video_type(sub: GUID, w: u32, h: u32, fps: u32, limited: bool) -> Res<IMFMediaType> {
    let mt = unsafe { MFCreateMediaType() }?;
    unsafe {
        mt.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)?;
        mt.SetGUID(&MF_MT_SUBTYPE, &sub)?;
        mt.SetUINT64(&MF_MT_FRAME_SIZE, pack(w, h))?;
        mt.SetUINT64(&MF_MT_FRAME_RATE, pack(fps, 1))?;
        mt.SetUINT64(&MF_MT_PIXEL_ASPECT_RATIO, pack(1, 1))?;
        mt.SetUINT32(&MF_MT_INTERLACE_MODE, MFVideoInterlace_Progressive.0 as u32)?;
        if limited {
            mt.SetUINT32(&MF_MT_VIDEO_NOMINAL_RANGE, MFNominalRange_16_235.0 as u32)?;
            mt.SetUINT32(&MF_MT_YUV_MATRIX, MFVideoTransferMatrix_BT709.0 as u32)?;
            mt.SetUINT32(&MF_MT_VIDEO_PRIMARIES, MFVideoPrimaries_BT709.0 as u32)?;
            mt.SetUINT32(&MF_MT_TRANSFER_FUNCTION, MFVideoTransFunc_709.0 as u32)?;
        } else {
            mt.SetUINT32(&MF_MT_VIDEO_NOMINAL_RANGE, MFNominalRange_0_255.0 as u32)?;
        }
    }
    Ok(mt)
}

/// 创建 DXGI 设备管理器。
fn dxgi_manager(gpu: &Gpu) -> Res<IMFDXGIDeviceManager> {
    let mut token = 0u32;
    let mut mgr = None;
    unsafe {
        MFCreateDXGIDeviceManager(&mut token, &mut mgr)?;
        let m = mgr.ok_or("无 DXGI 管理器")?;
        m.ResetDevice(&gpu.dev, token)?;
        Ok(m)
    }
}

/// 把纹理包成 MF 样本（零拷贝 DXGI surface buffer）。
fn sample_for(tex: &ID3D11Texture2D) -> Res<IMFSample> {
    unsafe {
        let buf = MFCreateDXGISurfaceBuffer(&ID3D11Texture2D::IID, tex, 0, false)?;
        let s = MFCreateSample()?;
        s.AddBuffer(&buf)?;
        s.SetSampleDuration(166_666)?;
        Ok(s)
    }
}

/// 从输出样本取纹理与子资源序号。
fn sample_texture(s: &IMFSample) -> Res<(ID3D11Texture2D, u32)> {
    unsafe {
        let b = s.GetBufferByIndex(0)?;
        let d: IMFDXGIBuffer = b.cast()?;
        let mut p: *mut c_void = std::ptr::null_mut();
        d.GetResource(&ID3D11Texture2D::IID, &mut p)?;
        let t = ID3D11Texture2D::from_raw(p);
        Ok((t, d.GetSubresourceIndex()?))
    }
}

/// Video Processor MFT 转换器。
pub struct MfConv {
    mft: IMFTransform,
    alloc: IMFVideoSampleAllocatorEx,
    in_samples: Vec<IMFSample>,
    in_tex: Vec<ID3D11Texture2D>,
    t: i64,
    out_flags: u32,
    _mgr: IMFDXGIDeviceManager,
}

impl MfConv {
    /// 创建（输出样本池 4 张）。
    pub fn new(gpu: &Gpu, size: (u32, u32), srcs: &[ID3D11Texture2D]) -> Res<Self> {
        Self::with_pool(gpu, size, srcs, 4)
    }

    /// 创建并指定输出样本池大小。
    pub fn with_pool(gpu: &Gpu, size: (u32, u32), srcs: &[ID3D11Texture2D], pool: u32) -> Res<Self> {
        init_mf()?;
        let mgr = dxgi_manager(gpu)?;
        let mft: IMFTransform =
            unsafe { CoCreateInstance(&CLSID_VideoProcessorMFT, None, CLSCTX_INPROC_SERVER) }?;
        unsafe { mft.ProcessMessage(MFT_MESSAGE_SET_D3D_MANAGER, mgr.as_raw() as usize) }
            .map_err(|e| format!("SET_D3D_MANAGER: {e}"))?;
        let (w, h) = size;
        let in_t = video_type(MFVideoFormat_RGB32, w, h, 60, false)?;
        unsafe { mft.SetInputType(0, &in_t, 0) }.map_err(|e| format!("SetInputType(RGB32): {e}"))?;
        let out_t = video_type(MFVideoFormat_NV12, w, h, 60, true)?;
        unsafe { mft.SetOutputType(0, &out_t, 0) }.map_err(|e| format!("SetOutputType(NV12): {e}"))?;
        let mut a: Option<IMFVideoSampleAllocatorEx> = None;
        unsafe {
            MFCreateVideoSampleAllocatorEx(
                &IMFVideoSampleAllocatorEx::IID,
                (&mut a as *mut Option<IMFVideoSampleAllocatorEx>).cast(),
            )?;
        }
        let alloc = a.ok_or("无 sample allocator")?;
        unsafe {
            alloc.SetDirectXManager(&mgr)?;
            let mut attrs = None;
            MFCreateAttributes(&mut attrs, 2)?;
            let attrs = attrs.ok_or("attrs")?;
            attrs.SetUINT32(&MF_SA_D3D11_BINDFLAGS, D3D11_BIND_RENDER_TARGET.0 as u32)?;
            attrs.SetUINT32(&MF_SA_D3D11_USAGE, D3D11_USAGE_DEFAULT.0 as u32)?;
            alloc.InitializeSampleAllocatorEx(pool, pool, &attrs, &out_t)?;
            mft.ProcessMessage(MFT_MESSAGE_NOTIFY_BEGIN_STREAMING, 0)?;
            mft.ProcessMessage(MFT_MESSAGE_NOTIFY_START_OF_STREAM, 0)?;
        }
        let in_samples = srcs.iter().map(sample_for).collect::<Res<Vec<_>>>()?;
        let out_flags = unsafe { mft.GetOutputStreamInfo(0) }?.dwFlags;
        Ok(Self { mft, alloc, in_samples, in_tex: srcs.to_vec(), t: 0, out_flags, _mgr: mgr })
    }

    /// 转换一帧并返回输出样本。
    fn convert_sample(&mut self, src: usize) -> Res<IMFSample> {
        let s = &self.in_samples[src % self.in_samples.len()];
        unsafe {
            s.SetSampleTime(self.t)?;
            self.t += 166_666;
            self.mft.ProcessInput(0, s, 0).map_err(|e| format!("ProcessInput: {e} (t={}, stream flags 0x{:x})", self.t, self.out_flags))?;
            let provides = self.out_flags & (MFT_OUTPUT_STREAM_PROVIDES_SAMPLES.0 as u32) != 0;
            let out = if provides { None } else { Some(self.alloc.AllocateSample()?) };
            let mut odb = [MFT_OUTPUT_DATA_BUFFER {
                dwStreamID: 0,
                pSample: ManuallyDrop::new(out.clone()),
                dwStatus: 0,
                pEvents: ManuallyDrop::new(None),
            }];
            let mut status = 0u32;
            let r = self.mft.ProcessOutput(0, &mut odb, &mut status);
            let got = ManuallyDrop::take(&mut odb[0].pSample);
            ManuallyDrop::drop(&mut odb[0].pEvents);
            // 按 MFT 约定：继续取输出直到 NEED_MORE_INPUT，才能接受下一帧输入
            if r.is_ok() {
                loop {
                    let mut o2 = [MFT_OUTPUT_DATA_BUFFER {
                        dwStreamID: 0,
                        pSample: ManuallyDrop::new(None),
                        dwStatus: 0,
                        pEvents: ManuallyDrop::new(None),
                    }];
                    let mut st2 = 0u32;
                    let r2 = self.mft.ProcessOutput(0, &mut o2, &mut st2);
                    ManuallyDrop::drop(&mut o2[0].pSample);
                    ManuallyDrop::drop(&mut o2[0].pEvents);
                    if r2.is_err() {
                        break;
                    }
                }
            }
            r.map_err(|e| format!("ProcessOutput: {e} (stream flags 0x{:x})", self.out_flags))?;
            got.or(out).ok_or_else(|| "无输出样本".into())
        }
    }

    /// 转换第 `i` 个源并返回输出纹理（仅用于自校验；要求子资源 0）。
    pub fn convert_once(&mut self, i: usize) -> Res<ID3D11Texture2D> {
        let out = self.convert_sample(i)?;
        let (t, sub) = sample_texture(&out)?;
        if sub != 0 {
            return err(format!("输出样本在数组切片 {sub}，自校验要求 0"));
        }
        let _ = &self.in_tex;
        Ok(t)
    }
}

impl Conv for MfConv {
    fn name(&self) -> String {
        "mf_vpmft".into()
    }
    fn run(&mut self, src: usize, _dst: &ID3D11Texture2D) -> Res<()> {
        self.convert_sample(src)?;
        Ok(())
    }
}

/// VPMFT 纯转换基准（输出进 MF 自己的样本池，`dsts` 仅占位）。
pub fn bench_mf(gpu: &Gpu, m: &mut MfConv, warmup: usize, frames: usize) -> Res<ConvResult> {
    let dummy = nv12_ring(gpu, 64, 64, 1, D3D11_BIND_RENDER_TARGET.0 as u32)?;
    bench_convert(gpu, m, &dummy, warmup, frames)
}

fn hr(e: &windows::core::Error) -> String {
    format!("0x{:08x}", e.code().0 as u32)
}

/// 枚举 MFT 并探测输入类型。
pub fn probe() -> Res<String> {
    init_mf()?;
    let mut items = Vec::new();
    // 硬件 + 软件 H.264 编码器
    for (label, flags) in [("hardware", MFT_ENUM_FLAG_HARDWARE), ("software", MFT_ENUM_FLAG_SYNCMFT)] {
        let out_info = MFT_REGISTER_TYPE_INFO { guidMajorType: MFMediaType_Video, guidSubtype: MFVideoFormat_H264 };
        let mut arr: *mut Option<IMFActivate> = std::ptr::null_mut();
        let mut n = 0u32;
        let r = unsafe {
            MFTEnumEx(
                MFT_CATEGORY_VIDEO_ENCODER,
                flags | MFT_ENUM_FLAG_SORTANDFILTER,
                None,
                Some(&out_info),
                &mut arr,
                &mut n,
            )
        };
        if let Err(e) = r {
            items.push(format!("{{\"class\":\"{label}\",\"error\":\"{}\"}}", hr(&e)));
            continue;
        }
        for i in 0..n as usize {
            let Some(act) = (unsafe { (*arr.add(i)).clone() }) else { continue };
            let mut len = 0u32;
            let mut buf = [0u16; 256];
            let name = unsafe {
                act.GetString(&MFT_FRIENDLY_NAME_Attribute, &mut buf, Some(&mut len))
                    .map(|_| String::from_utf16_lossy(&buf[..len as usize]))
                    .unwrap_or_default()
            };
            let r = (|| -> Res<String> {
                let mft: IMFTransform = unsafe { act.ActivateObject() }?;
                let attrs = unsafe { mft.GetAttributes() }.ok();
                let is_async = attrs
                    .as_ref()
                    .map(|a| unsafe { a.GetUINT32(&MF_TRANSFORM_ASYNC) }.unwrap_or(0) != 0)
                    .unwrap_or(false);
                let d3d11 = attrs
                    .as_ref()
                    .map(|a| unsafe { a.GetUINT32(&MF_SA_D3D11_AWARE) }.unwrap_or(0) != 0)
                    .unwrap_or(false);
                if let Some(a) = &attrs {
                    let _ = unsafe { a.SetUINT32(&MF_TRANSFORM_ASYNC_UNLOCK, 1) };
                }
                let out_t = video_type(MFVideoFormat_H264, 2560, 1440, 60, true)?;
                unsafe { out_t.SetUINT32(&MF_MT_AVG_BITRATE, 20_000_000)? };
                let set_out = unsafe { mft.SetOutputType(0, &out_t, 0) };
                let mut avail = Vec::new();
                let mut tries = Vec::new();
                for k in 0..32 {
                    match unsafe { mft.GetInputAvailableType(0, k) } {
                        Ok(t) => {
                            let g = unsafe { t.GetGUID(&MF_MT_SUBTYPE) }.unwrap_or_default();
                            let nm = guid_name(&g);
                            avail.push(format!("\"{nm}\""));
                            // 直接用驱动给出的类型做“仅测试”设置，看它本身是否可被接受
                            let r = unsafe { mft.SetInputType(0, &t, MFT_SET_TYPE_TEST_ONLY.0 as u32) };
                            tries.push(format!(
                                "{{\"type\":\"avail:{nm}\",\"ok\":{},\"hr\":\"{}\"}}",
                                r.is_ok(),
                                r.as_ref().err().map(hr).unwrap_or_default()
                            ));
                        }
                        Err(_) => break,
                    }
                }
                for (nm, g, lim) in [
                    ("RGB32", MFVideoFormat_RGB32, false),
                    ("RGB32_limited_attrs", MFVideoFormat_RGB32, true),
                    ("ARGB32", MFVideoFormat_ARGB32, false),
                    ("NV12", MFVideoFormat_NV12, true),
                ] {
                    let t = video_type(g, 2560, 1440, 60, lim)?;
                    let r = unsafe { mft.SetInputType(0, &t, MFT_SET_TYPE_TEST_ONLY.0 as u32) };
                    tries.push(format!(
                        "{{\"type\":\"{nm}\",\"ok\":{},\"hr\":\"{}\"}}",
                        r.is_ok(),
                        r.as_ref().err().map(hr).unwrap_or_default()
                    ));
                }
                Ok(format!(
                    "\"async\":{is_async},\"d3d11_aware\":{d3d11},\"set_output_h264_ok\":{},\"available_inputs\":[{}],\"set_input_tries\":[{}]",
                    set_out.is_ok(),
                    avail.join(","),
                    tries.join(",")
                ))
            })();
            items.push(format!(
                "{{\"class\":\"{label}\",\"name\":\"{}\",{}}}",
                name.replace('"', "'"),
                match r {
                    Ok(s) => s,
                    Err(e) => format!("\"error\":\"{}\"", e.to_string().replace('"', "'")),
                }
            ));
        }
    }
    // VPMFT 可用输入类型
    let vp = (|| -> Res<String> {
        let mft: IMFTransform = unsafe { CoCreateInstance(&CLSID_VideoProcessorMFT, None, CLSCTX_INPROC_SERVER) }?;
        let mut v = Vec::new();
        for k in 0..64 {
            match unsafe { mft.GetInputAvailableType(0, k) } {
                Ok(t) => v.push(format!("\"{}\"", guid_name(&unsafe { t.GetGUID(&MF_MT_SUBTYPE) }.unwrap_or_default()))),
                Err(_) => break,
            }
        }
        Ok(v.join(","))
    })();
    Ok(format!(
        "{{\"kind\":\"mf-probe\",\"encoders\":[{}],\"vpmft_available_inputs\":[{}]}}",
        items.join(","),
        vp.unwrap_or_default()
    ))
}

/// VPMFT -> 硬件 H.264 编码 MFT 端到端（异步事件驱动），单线程。`direct` 为真时跳过 VPMFT，直接把 BGRA 喂给编码 MFT（驱动自带 ARGB32 输入）。
pub fn e2e(res: &str, direct: bool, noise: bool, warmup: usize, frames: usize) -> Res<String> {
    let size = if res == "1080" { (1920, 1080) } else { (2560, 1440) };
    init_mf()?;
    let gpu = Gpu::new()?;
    let src = SourcePool::new(&gpu, size.0, size.1, 8, false, noise)?;
    let mut conv = if direct { None } else { Some(MfConv::new(&gpu, size, &src.tex)?) };
    let direct_samples = src.tex.iter().map(sample_for).collect::<Res<Vec<_>>>()?;
    // 编码 MFT
    let out_info = MFT_REGISTER_TYPE_INFO { guidMajorType: MFMediaType_Video, guidSubtype: MFVideoFormat_H264 };
    let mut arr: *mut Option<IMFActivate> = std::ptr::null_mut();
    let mut n = 0u32;
    unsafe {
        MFTEnumEx(MFT_CATEGORY_VIDEO_ENCODER, MFT_ENUM_FLAG_HARDWARE | MFT_ENUM_FLAG_SORTANDFILTER, None, Some(&out_info), &mut arr, &mut n)?;
    }
    if n == 0 {
        return err("没有硬件 H.264 编码 MFT");
    }
    let act = unsafe { (*arr).clone() }.ok_or("activate")?;
    let enc: IMFTransform = unsafe { act.ActivateObject() }?;
    let attrs = unsafe { enc.GetAttributes() }?;
    unsafe {
        attrs.SetUINT32(&MF_TRANSFORM_ASYNC_UNLOCK, 1)?;
        let _ = attrs.SetUINT32(&MF_LOW_LATENCY, 1);
    }
    let mgr = dxgi_manager(&gpu)?;
    unsafe { enc.ProcessMessage(MFT_MESSAGE_SET_D3D_MANAGER, mgr.as_raw() as usize) }
        .map_err(|e| format!("编码 MFT SET_D3D_MANAGER: {e}"))?;
    let out_t = video_type(MFVideoFormat_H264, size.0, size.1, 60, true)?;
    unsafe {
        out_t.SetUINT32(&MF_MT_AVG_BITRATE, 40_000_000)?;
        out_t.SetUINT32(&MF_MT_MPEG2_PROFILE, 100)?;
        enc.SetOutputType(0, &out_t, 0).map_err(|e| format!("编码 SetOutputType: {e}"))?;
    }
    let in_t = if direct {
        // 取驱动自带的 ARGB32 类型，补全尺寸等属性后设置
        let mut found = None;
        for k in 0..32 {
            let Ok(t) = (unsafe { enc.GetInputAvailableType(0, k) }) else { break };
            if unsafe { t.GetGUID(&MF_MT_SUBTYPE) }.ok() == Some(MFVideoFormat_ARGB32) {
                found = Some(t);
                break;
            }
        }
        let t = found.ok_or("编码 MFT 没有 ARGB32 输入类型")?;
        unsafe {
            t.SetUINT64(&MF_MT_FRAME_SIZE, pack(size.0, size.1))?;
            t.SetUINT64(&MF_MT_FRAME_RATE, pack(60, 1))?;
            t.SetUINT64(&MF_MT_PIXEL_ASPECT_RATIO, pack(1, 1))?;
            t.SetUINT32(&MF_MT_INTERLACE_MODE, MFVideoInterlace_Progressive.0 as u32)?;
        }
        t
    } else {
        video_type(MFVideoFormat_NV12, size.0, size.1, 60, true)?
    };
    unsafe { enc.SetInputType(0, &in_t, 0) }.map_err(|e| format!("编码 SetInputType({}): {e}", if direct { "ARGB32" } else { "NV12" }))?;
    let evgen: IMFMediaEventGenerator = enc.cast()?;
    unsafe {
        enc.ProcessMessage(MFT_MESSAGE_NOTIFY_BEGIN_STREAMING, 0)?;
        enc.ProcessMessage(MFT_MESSAGE_NOTIFY_START_OF_STREAM, 0)?;
    }
    let info = unsafe { enc.GetOutputStreamInfo(0) }?;
    let provides = info.dwFlags & (MFT_OUTPUT_STREAM_PROVIDES_SAMPLES.0 as u32) != 0;
    let total = warmup + frames;
    let (mut fed, mut got) = (0usize, 0usize);
    let (mut packets, mut bytes) = (0u64, 0u64);
    let mut ft: Vec<f64> = Vec::new();
    let mut last = Instant::now();
    let (mut wall, mut cpu0) = (Instant::now(), 0.0);
    let mut draining = false;
    while got < total {
        let ev = unsafe { evgen.GetEvent(MEDIA_EVENT_GENERATOR_GET_EVENT_FLAGS(0)) }?;
        let ty = unsafe { ev.GetType() }?;
        if ty == METransformNeedInput.0 as u32 {
            if fed < total {
                if fed == warmup {
                    wall = Instant::now();
                    cpu0 = cpu_ms();
                    last = Instant::now();
                }
                let out = match conv.as_mut() {
                    Some(c) => c.convert_sample(fed)?,
                    None => direct_samples[fed % direct_samples.len()].clone(),
                };
                unsafe { out.SetSampleTime(fed as i64 * 166_666)? };
                unsafe { enc.ProcessInput(0, &out, 0)? };
                fed += 1;
            } else if !draining {
                draining = true;
                unsafe { enc.ProcessMessage(MFT_MESSAGE_COMMAND_DRAIN, 0)? };
            }
        } else if ty == METransformHaveOutput.0 as u32 {
            let mut odb = [MFT_OUTPUT_DATA_BUFFER {
                dwStreamID: 0,
                pSample: ManuallyDrop::new(None),
                dwStatus: 0,
                pEvents: ManuallyDrop::new(None),
            }];
            if !provides {
                return err("编码 MFT 不自行分配输出样本（未实现外部分配）");
            }
            let mut st = 0u32;
            let r = unsafe { enc.ProcessOutput(0, &mut odb, &mut st) };
            if let Err(e) = &r {
                if e.code().0 as u32 == 0xC00D6D61 {
                    // MF_E_TRANSFORM_STREAM_CHANGE：按约定重新协商输出类型后继续
                    let t = unsafe { enc.GetOutputAvailableType(0, 0) }?;
                    unsafe { enc.SetOutputType(0, &t, 0) }?;
                    continue;
                }
            }
            r.map_err(|e| format!("编码 ProcessOutput: {e}"))?;
            if let Some(s) = ManuallyDrop::into_inner(std::mem::replace(&mut odb[0].pSample, ManuallyDrop::new(None))) {
                let len = unsafe { s.GetTotalLength() }.unwrap_or(0);
                if got >= warmup {
                    bytes += len as u64;
                    packets += 1;
                    ft.push(last.elapsed().as_secs_f64() * 1000.0);
                    last = Instant::now();
                }
                got += 1;
            }
        } else if ty == METransformDrainComplete.0 as u32 {
            break;
        }
    }
    let secs = wall.elapsed().as_secs_f64();
    let used = cpu_ms() - cpu0;
    let m = gpu_mem_mb(&gpu);
    Ok(format!(
        "{{\"kind\":\"mf-e2e\",\"ok\":true,\"res\":\"{res}\",\"method\":\"{}\",\"content\":\"{}\",\"frames\":{packets},\"frame_interval_ms\":{},\"fps\":{:.1},\"proc_cpu_pct\":{:.1},\"peak_ws_mb\":{:.0},\"gpu_local_mb\":{:.0},\"gpu_nonlocal_mb\":{:.0},\"bytes\":{bytes}}}",
        if direct { "mf_hw_h264_mft_direct_argb32" } else { "mf_vpmft+hw_h264_mft" },
        if noise { "noisy" } else { "desktop" },
        Stats::from(&ft).json(),
        packets as f64 / secs,
        used / (secs * 1000.0) * 100.0,
        peak_ws_mb(),
        m.0,
        m.1
    ))
}
