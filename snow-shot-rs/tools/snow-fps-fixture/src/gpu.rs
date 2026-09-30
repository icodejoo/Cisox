//! GPU 出帧：D3D11 + DXGI flip-model 交换链，`Present(1, 0)` 跟随垂直同步。
//!
//! 画面全部由 GPU 完成：背景用 `ClearView` 批量填充矩形，序号条/噪声块由 CPU 生成小块像素后上传并拷贝，
//! 这样 2560x1440 下 CPU 与带宽占用都很低，不会成为 60fps 的瓶颈。

use snow_fps_fixture::content::{Fill, Scene, fill_noise};
use snow_fps_fixture::seqbar::{bar_height, paint_bar};
use windows::Win32::Foundation::{HMODULE, RECT};
use windows::Win32::Graphics::Direct3D::D3D_DRIVER_TYPE_HARDWARE;
use windows::Win32::Graphics::Direct3D11::{
    D3D11_BIND_SHADER_RESOURCE, D3D11_CREATE_DEVICE_BGRA_SUPPORT, D3D11_SDK_VERSION, D3D11_TEXTURE2D_DESC,
    D3D11_USAGE_DEFAULT, D3D11CreateDevice, ID3D11Device, ID3D11DeviceContext, ID3D11DeviceContext1,
    ID3D11RenderTargetView, ID3D11Resource, ID3D11Texture2D, ID3D11View,
};
use windows::Win32::Graphics::Dxgi::Common::{DXGI_ALPHA_MODE_IGNORE, DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_SAMPLE_DESC};
use windows::Win32::Graphics::Dxgi::{
    DXGI_SCALING_NONE, DXGI_SWAP_CHAIN_DESC1, DXGI_SWAP_EFFECT_FLIP_DISCARD, DXGI_USAGE_RENDER_TARGET_OUTPUT,
    IDXGIDevice1, IDXGIFactory2, IDXGISwapChain1,
};
use windows::core::Interface;

use crate::win::FixtureWindow;

/// 交换链缓冲数。
const BUFFER_COUNT: u32 = 2;
/// 最大排队帧数（越小延迟越低，且 Present 更贴近 vsync 节奏）。
const MAX_FRAME_LATENCY: u32 = 1;
/// BGRA 每像素字节数。
const BYTES_PER_PIXEL: u32 = 4;

/// 夹具渲染器。
pub struct Renderer {
    /// 设备（保持存活）。
    _device: ID3D11Device,
    /// 立即上下文。
    ctx: ID3D11DeviceContext,
    /// 带 ClearView 的 11.1 上下文。
    ctx1: ID3D11DeviceContext1,
    /// 交换链。
    swap: IDXGISwapChain1,
    /// 当前后备缓冲的渲染目标视图。
    rtv: ID3D11RenderTargetView,
    /// 后备缓冲资源。
    back: ID3D11Resource,
    /// 序号条纹理。
    bar_tex: ID3D11Texture2D,
    /// 序号条 CPU 缓冲。
    bar_px: Vec<u32>,
    /// 噪声纹理及其 CPU 缓冲（仅噪声模式）。
    noise: Option<(ID3D11Texture2D, Vec<u32>)>,
    /// 噪声随机状态。
    noise_state: u32,
    /// 画面宽。
    width: u32,
    /// 画面高。
    height: u32,
}

/// 创建一张默认用途的 BGRA 纹理。
fn make_texture(device: &ID3D11Device, width: u32, height: u32) -> Result<ID3D11Texture2D, String> {
    let desc = D3D11_TEXTURE2D_DESC {
        Width: width,
        Height: height,
        MipLevels: 1,
        ArraySize: 1,
        Format: DXGI_FORMAT_B8G8R8A8_UNORM,
        SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
        Usage: D3D11_USAGE_DEFAULT,
        BindFlags: D3D11_BIND_SHADER_RESOURCE.0 as u32,
        ..Default::default()
    };
    let mut tex = None;
    // SAFETY: desc 有效，输出指针指向局部 Option。
    unsafe { device.CreateTexture2D(&desc, None, Some(&mut tex)) }.map_err(|e| e.to_string())?;
    tex.ok_or_else(|| "CreateTexture2D 返回空".to_string())
}

impl Renderer {
    /// 为窗口创建渲染器。
    ///
    /// # 参数
    /// - `window`：夹具窗口。
    /// - `width`、`height`：画面尺寸（= 窗口客户区尺寸）。
    /// - `with_noise`：是否准备噪声纹理。
    ///
    /// # 返回
    /// 渲染器；D3D/DXGI 失败返回原因。
    pub fn new(window: &FixtureWindow, width: u32, height: u32, with_noise: bool) -> Result<Self, String> {
        let mut device = None;
        let mut ctx = None;
        // SAFETY: 输出指针均指向局部 Option；默认适配器、硬件驱动。
        unsafe {
            D3D11CreateDevice(
                None,
                D3D_DRIVER_TYPE_HARDWARE,
                HMODULE::default(),
                D3D11_CREATE_DEVICE_BGRA_SUPPORT,
                None,
                D3D11_SDK_VERSION,
                Some(&mut device),
                None,
                Some(&mut ctx),
            )
        }
        .map_err(|e| format!("D3D11CreateDevice: {e}"))?;
        let device = device.ok_or("设备为空")?;
        let ctx = ctx.ok_or("上下文为空")?;
        let ctx1: ID3D11DeviceContext1 = ctx.cast().map_err(|e| format!("需要 D3D11.1: {e}"))?;
        let dxgi_device: IDXGIDevice1 = device.cast().map_err(|e| e.to_string())?;
        // SAFETY: 简单的 DXGI 查询/设置调用。
        let factory: IDXGIFactory2 = unsafe {
            dxgi_device.SetMaximumFrameLatency(MAX_FRAME_LATENCY).map_err(|e| e.to_string())?;
            dxgi_device.GetAdapter().map_err(|e| e.to_string())?.GetParent().map_err(|e| e.to_string())?
        };
        let desc = DXGI_SWAP_CHAIN_DESC1 {
            Width: width,
            Height: height,
            Format: DXGI_FORMAT_B8G8R8A8_UNORM,
            SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
            BufferUsage: DXGI_USAGE_RENDER_TARGET_OUTPUT,
            BufferCount: BUFFER_COUNT,
            Scaling: DXGI_SCALING_NONE,
            SwapEffect: DXGI_SWAP_EFFECT_FLIP_DISCARD,
            AlphaMode: DXGI_ALPHA_MODE_IGNORE,
            ..Default::default()
        };
        // SAFETY: 窗口句柄在 window 存活期间有效；desc 有效。
        let swap = unsafe { factory.CreateSwapChainForHwnd(&device, window.hwnd(), &desc, None, None) }
            .map_err(|e| format!("CreateSwapChainForHwnd: {e}"))?;
        // SAFETY: 取缓冲 0 并创建视图。
        let (back, rtv) = unsafe {
            let back: ID3D11Texture2D = swap.GetBuffer(0).map_err(|e| e.to_string())?;
            let mut rtv = None;
            device.CreateRenderTargetView(&back, None, Some(&mut rtv)).map_err(|e| e.to_string())?;
            (back.cast::<ID3D11Resource>().map_err(|e| e.to_string())?, rtv.ok_or("RTV 为空")?)
        };
        let bar_h = bar_height(height);
        let bar_tex = make_texture(&device, width, bar_h)?;
        let noise = if with_noise {
            let (nw, nh) = (width / 4, height / 4);
            Some((make_texture(&device, nw, nh)?, vec![0u32; (nw * nh) as usize]))
        } else {
            None
        };
        Ok(Self {
            _device: device,
            ctx,
            ctx1,
            swap,
            rtv,
            back,
            bar_tex,
            bar_px: vec![0u32; (width * bar_h) as usize],
            noise,
            noise_state: 0,
            width,
            height,
        })
    }

    /// 绘制一帧并 `Present(1, 0)`（阻塞到合适的垂直同步）。
    ///
    /// # 参数
    /// - `scene`：背景描述。
    /// - `seq`：写入序号条的帧序号。
    ///
    /// # 返回
    /// 成功为 `Ok`；Present 失败（如设备丢失）返回原因。
    pub fn draw_and_present(&mut self, scene: &Scene, seq: u32) -> Result<(), String> {
        let view: ID3D11View = self.rtv.cast().map_err(|e| e.to_string())?;
        for Fill { color, rects } in &scene.fills {
            let rc: Vec<RECT> = rects.iter().map(|&(l, t, r, b)| RECT { left: l, top: t, right: r, bottom: b }).collect();
            // SAFETY: view 与 rc 在调用期间有效。
            unsafe { self.ctx1.ClearView(&view, color, Some(&rc)) };
        }
        // 序号条：CPU 画到小缓冲后上传，再拷贝到后备缓冲顶部
        paint_bar(&mut self.bar_px, self.width, self.height, seq);
        let bar_res: ID3D11Resource = self.bar_tex.cast().map_err(|e| e.to_string())?;
        // SAFETY: 缓冲大小 = 宽*条高*4，行距与之一致；资源均存活。
        unsafe {
            self.ctx.UpdateSubresource(&bar_res, 0, None, self.bar_px.as_ptr().cast(), self.width * BYTES_PER_PIXEL, 0);
            self.ctx.CopySubresourceRegion(&self.back, 0, 0, 0, 0, &bar_res, 0, None);
        }
        if let (Some((tex, px)), Some((x, y, nw, _))) = (self.noise.as_mut(), scene.noise) {
            fill_noise(px, &mut self.noise_state);
            let res: ID3D11Resource = tex.cast().map_err(|e| e.to_string())?;
            // SAFETY: 噪声缓冲大小 = nw*nh*4。
            unsafe {
                self.ctx.UpdateSubresource(&res, 0, None, px.as_ptr().cast(), nw * BYTES_PER_PIXEL, 0);
                self.ctx.CopySubresourceRegion(&self.back, 0, x as u32, y as u32, 0, &res, 0, None);
            }
        }
        // SAFETY: 交换链存活。
        unsafe { self.swap.Present(1, windows::Win32::Graphics::Dxgi::DXGI_PRESENT(0)) }.ok().map_err(|e| format!("Present: {e}"))
    }
}
