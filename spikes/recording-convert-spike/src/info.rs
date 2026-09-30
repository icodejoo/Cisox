//! 环境/能力探测：适配器、Feature Level、格式支持、VideoProcessor 能力、显示器枚举。

use windows::Win32::Graphics::Direct3D11::*;
use windows::Win32::Graphics::Dxgi::Common::*;
use windows::Win32::Graphics::Dxgi::*;
use windows::Win32::Graphics::Gdi::{GetMonitorInfoW, MONITORINFO};
use windows::core::Interface;

use crate::gpu::*;

fn fmt_support(dev: &ID3D11Device, f: DXGI_FORMAT) -> (u32, u32) {
    let s1 = unsafe { dev.CheckFormatSupport(f) }.unwrap_or(0);
    let mut d = D3D11_FEATURE_DATA_FORMAT_SUPPORT2 { InFormat: f, OutFormatSupport2: 0 };
    let _ = unsafe {
        dev.CheckFeatureSupport(
            D3D11_FEATURE_FORMAT_SUPPORT2,
            (&mut d as *mut D3D11_FEATURE_DATA_FORMAT_SUPPORT2).cast(),
            std::mem::size_of::<D3D11_FEATURE_DATA_FORMAT_SUPPORT2>() as u32,
        )
    };
    (s1, d.OutFormatSupport2)
}

/// 输出设备与能力信息 JSON。
pub fn info() -> Res<String> {
    let gpu = Gpu::new()?;
    let d = unsafe { gpu.adapter.GetDesc1() }?;
    let umd = unsafe { gpu.adapter.CheckInterfaceSupport(&IDXGIDevice::IID) }
        .map(|v| {
            format!("{}.{}.{}.{}", (v >> 48) & 0xffff, (v >> 32) & 0xffff, (v >> 16) & 0xffff, v & 0xffff)
        })
        .unwrap_or_default();
    let mut items = Vec::new();
    for (name, f) in [
        ("NV12", DXGI_FORMAT_NV12),
        ("R8_UNORM", DXGI_FORMAT_R8_UNORM),
        ("R8G8_UNORM", DXGI_FORMAT_R8G8_UNORM),
        ("B8G8R8A8_UNORM", DXGI_FORMAT_B8G8R8A8_UNORM),
    ] {
        let (s1, s2) = fmt_support(&gpu.dev, f);
        items.push(format!(
            "\"{name}\":{{\"render_target\":{},\"video_processor_output\":{},\"video_encoder\":{},\"typed_uav_store\":{},\"typed_uav_load\":{}}}",
            s1 & D3D11_FORMAT_SUPPORT_RENDER_TARGET.0 as u32 != 0,
            s1 & D3D11_FORMAT_SUPPORT_VIDEO_PROCESSOR_OUTPUT.0 as u32 != 0,
            s1 & D3D11_FORMAT_SUPPORT_VIDEO_ENCODER.0 as u32 != 0,
            s2 & D3D11_FORMAT_SUPPORT2_UAV_TYPED_STORE.0 as u32 != 0,
            s2 & D3D11_FORMAT_SUPPORT2_UAV_TYPED_LOAD.0 as u32 != 0
        ));
    }
    // VideoProcessor 能力
    let vdev: ID3D11VideoDevice = gpu.dev.cast()?;
    let desc = D3D11_VIDEO_PROCESSOR_CONTENT_DESC {
        InputFrameFormat: D3D11_VIDEO_FRAME_FORMAT_PROGRESSIVE,
        InputFrameRate: DXGI_RATIONAL { Numerator: 60, Denominator: 1 },
        InputWidth: 2560,
        InputHeight: 1440,
        OutputFrameRate: DXGI_RATIONAL { Numerator: 60, Denominator: 1 },
        OutputWidth: 2560,
        OutputHeight: 1440,
        Usage: D3D11_VIDEO_USAGE_PLAYBACK_NORMAL,
    };
    let en = unsafe { vdev.CreateVideoProcessorEnumerator(&desc) }?;
    let mut caps = D3D11_VIDEO_PROCESSOR_CAPS::default();
    unsafe { en.GetVideoProcessorCaps(&mut caps) }?;
    // NV12 + VIDEO_ENCODER 纹理能否创建
    let rt = D3D11_BIND_RENDER_TARGET.0 as u32;
    let ve = gpu.tex(2560, 1440, DXGI_FORMAT_NV12, rt | D3D11_BIND_VIDEO_ENCODER.0 as u32, None);
    let uav = gpu.tex(2560, 1440, DXGI_FORMAT_NV12, rt | D3D11_BIND_UNORDERED_ACCESS.0 as u32, None);
    let srv_rt = gpu.tex(2560, 1440, DXGI_FORMAT_NV12, rt | D3D11_BIND_SHADER_RESOURCE.0 as u32, None);
    let name = gpu.name.replace('"', "'");
    Ok(format!(
        "{{\"kind\":\"info\",\"adapter\":\"{name}\",\"vendor_id\":\"0x{:x}\",\"device_id\":\"0x{:x}\",\"dedicated_vram_mb\":{},\"shared_sysmem_mb\":{},\"umd_version\":\"{umd}\",\"feature_level\":\"0x{:x}\",\"formats\":{{{}}},\"vp\":{{\"max_input_streams\":{},\"feature_caps\":\"0x{:x}\",\"rate_conversion_caps\":{}}},\"nv12_tex\":{{\"rt_video_encoder\":{},\"rt_uav\":{},\"rt_srv\":{}}}}}",
        d.VendorId,
        d.DeviceId,
        d.DedicatedVideoMemory / 1048576,
        d.SharedSystemMemory / 1048576,
        gpu.feature_level,
        items.join(","),
        caps.MaxInputStreams,
        caps.FeatureCaps,
        caps.RateConversionCapsCount,
        ve.is_ok(),
        uav.is_ok(),
        srv_rt.is_ok()
    ))
}

/// 枚举全部显示器；按属性（非主屏且范围 2560,0,2560x1440）选出副屏序号。
pub fn dxgi_list() -> Res<String> {
    let factory: IDXGIFactory1 = unsafe { CreateDXGIFactory1() }?;
    let mut items = Vec::new();
    let mut secondary: Option<(u32, u32)> = None;
    let mut ai = 0;
    while let Ok(a) = unsafe { factory.EnumAdapters1(ai) } {
        let mut oi = 0;
        while let Ok(o) = unsafe { a.EnumOutputs(oi) } {
            let d = unsafe { o.GetDesc() }?;
            let mut mi = MONITORINFO { cbSize: std::mem::size_of::<MONITORINFO>() as u32, ..Default::default() };
            let primary = unsafe { GetMonitorInfoW(d.Monitor, &mut mi) }.as_bool() && (mi.dwFlags & 1) != 0;
            let r = d.DesktopCoordinates;
            let (w, h) = (r.right - r.left, r.bottom - r.top);
            if !primary && r.left == 2560 && r.top == 0 && w == 2560 && h == 1440 && secondary.is_none() {
                secondary = Some((ai, oi));
            }
            items.push(format!(
                "{{\"adapter\":{ai},\"output\":{oi},\"left\":{},\"top\":{},\"width\":{w},\"height\":{h},\"primary\":{primary}}}",
                r.left, r.top
            ));
            oi += 1;
        }
        ai += 1;
    }
    Ok(format!(
        "{{\"kind\":\"dxgi-list\",\"outputs\":[{}],\"secondary\":{}}}",
        items.join(","),
        secondary
            .map(|(a, o)| format!("{{\"adapter\":{a},\"output\":{o}}}"))
            .unwrap_or_else(|| "null".into())
    ))
}
