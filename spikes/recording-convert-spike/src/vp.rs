//! 方案 1：D3D11 VideoProcessor。
//! - `VpBase`：复刻上游 snow-d3d11 `VideoProcessor::blit` 的写法（每帧重建 view、重设全部 stream 状态、每层都检查格式转换）。
//! - `VpOpt`：单 pass、view 与状态一次性预建，每帧只剩 `VideoProcessorBlt`。

use std::collections::HashMap;
use std::mem::ManuallyDrop;

use windows::Win32::Foundation::RECT;
use windows::Win32::Graphics::Direct3D11::*;
use windows::Win32::Graphics::Dxgi::Common::*;
use windows::core::{BOOL, Interface};

use crate::bench::Conv;
use crate::gpu::{Gpu, Res, err};

fn cs_for(fmt: DXGI_FORMAT) -> DXGI_COLOR_SPACE_TYPE {
    if fmt == DXGI_FORMAT_NV12 {
        DXGI_COLOR_SPACE_YCBCR_STUDIO_G22_LEFT_P709
    } else {
        DXGI_COLOR_SPACE_RGB_FULL_G22_NONE_P709
    }
}

fn rect(w: u32, h: u32) -> RECT {
    RECT { left: 0, top: 0, right: w as i32, bottom: h as i32 }
}

/// 创建 VideoProcessor 与枚举器。
fn create_vp(
    gpu: &Gpu,
    input: (u32, u32),
    output: (u32, u32),
    fps: u32,
) -> Res<(ID3D11VideoDevice, ID3D11VideoContext1, ID3D11VideoProcessorEnumerator, ID3D11VideoProcessor)> {
    let vdev: ID3D11VideoDevice = gpu.dev.cast()?;
    let vctx: ID3D11VideoContext1 = gpu.ctx.cast()?;
    let rate = DXGI_RATIONAL { Numerator: fps, Denominator: 1 };
    let desc = D3D11_VIDEO_PROCESSOR_CONTENT_DESC {
        InputFrameFormat: D3D11_VIDEO_FRAME_FORMAT_PROGRESSIVE,
        InputFrameRate: rate,
        InputWidth: input.0,
        InputHeight: input.1,
        OutputFrameRate: rate,
        OutputWidth: output.0,
        OutputHeight: output.1,
        Usage: D3D11_VIDEO_USAGE_PLAYBACK_NORMAL,
    };
    let en = unsafe { vdev.CreateVideoProcessorEnumerator(&desc) }?;
    let p = unsafe { vdev.CreateVideoProcessor(&en, 0) }?;
    Ok((vdev, vctx, en, p))
}

/// 检查枚举器是否支持某个格式转换。
fn check_conv(en: &ID3D11VideoProcessorEnumerator, i: DXGI_FORMAT, o: DXGI_FORMAT) -> Res<()> {
    let e1: ID3D11VideoProcessorEnumerator1 = en.cast()?;
    let ok = unsafe {
        e1.CheckVideoProcessorFormatConversion(
            i,
            DXGI_COLOR_SPACE_RGB_FULL_G22_NONE_P709,
            o,
            cs_for(o),
        )
    }?
    .as_bool();
    if ok { Ok(()) } else { err(format!("不支持的转换 {i:?}->{o:?}")) }
}

fn out_desc() -> D3D11_VIDEO_PROCESSOR_OUTPUT_VIEW_DESC {
    D3D11_VIDEO_PROCESSOR_OUTPUT_VIEW_DESC {
        ViewDimension: D3D11_VPOV_DIMENSION_TEXTURE2D,
        Anonymous: D3D11_VIDEO_PROCESSOR_OUTPUT_VIEW_DESC_0 {
            Texture2D: D3D11_TEX2D_VPOV { MipSlice: 0 },
        },
    }
}

fn in_desc() -> D3D11_VIDEO_PROCESSOR_INPUT_VIEW_DESC {
    D3D11_VIDEO_PROCESSOR_INPUT_VIEW_DESC {
        ViewDimension: D3D11_VPIV_DIMENSION_TEXTURE2D,
        ..Default::default()
    }
}

/// 设置单流状态（所有 stream 级与 output 级状态）。
fn set_states(
    vctx: &ID3D11VideoContext1,
    p: &ID3D11VideoProcessor,
    index: u32,
    src: &RECT,
    dst: &RECT,
    alpha: bool,
    out_fmt: DXGI_FORMAT,
    target: &RECT,
) {
    unsafe {
        vctx.VideoProcessorSetStreamFrameFormat(p, index, D3D11_VIDEO_FRAME_FORMAT_PROGRESSIVE);
        vctx.VideoProcessorSetStreamAutoProcessingMode(p, index, false);
        vctx.VideoProcessorSetStreamColorSpace1(p, index, DXGI_COLOR_SPACE_RGB_FULL_G22_NONE_P709);
        vctx.VideoProcessorSetStreamSourceRect(p, index, true, Some(src));
        vctx.VideoProcessorSetStreamDestRect(p, index, true, Some(dst));
        vctx.VideoProcessorSetStreamAlpha(p, index, alpha, 1.0);
        vctx.VideoProcessorSetStreamRotation(p, index, false, D3D11_VIDEO_PROCESSOR_ROTATION_IDENTITY);
        vctx.VideoProcessorSetOutputTargetRect(p, true, Some(target));
        vctx.VideoProcessorSetOutputColorSpace1(p, cs_for(out_fmt));
        vctx.VideoProcessorSetOutputBackgroundColor(
            p,
            false,
            &D3D11_VIDEO_COLOR {
                Anonymous: D3D11_VIDEO_COLOR_0 {
                    RGBA: D3D11_VIDEO_COLOR_RGBA { R: 0.0, G: 0.0, B: 0.0, A: 1.0 },
                },
            },
        );
        vctx.VideoProcessorSetOutputAlphaFillMode(p, D3D11_VIDEO_PROCESSOR_ALPHA_FILL_MODE_OPAQUE, 0);
    }
}

/// 提交一次 Blt（多个输入视图）。
fn blt(
    vctx: &ID3D11VideoContext1,
    p: &ID3D11VideoProcessor,
    out: &ID3D11VideoProcessorOutputView,
    views: Vec<ID3D11VideoProcessorInputView>,
) -> Res<()> {
    let mut streams: Vec<_> = views
        .into_iter()
        .map(|v| D3D11_VIDEO_PROCESSOR_STREAM {
            Enable: BOOL(1),
            pInputSurface: ManuallyDrop::new(Some(v)),
            ..Default::default()
        })
        .collect();
    let r = unsafe { vctx.VideoProcessorBlt(p, out, 0, &streams) };
    for s in &mut streams {
        unsafe { ManuallyDrop::drop(&mut s.pInputSurface) };
    }
    r?;
    Ok(())
}

/// 上游写法的基线：每次 blit 都重建 view / 重设状态。
pub struct VpBase {
    dev: ID3D11Device,
    mt: ID3D11Multithread,
    vdev: ID3D11VideoDevice,
    vctx: ID3D11VideoContext1,
    en: ID3D11VideoProcessorEnumerator,
    p: ID3D11VideoProcessor,
    out_size: (u32, u32),
    srcs: Vec<ID3D11Texture2D>,
}

impl VpBase {
    /// 创建：`in_size` 输入尺寸，`out_size` 输出尺寸；`srcs` 供 `Conv::run` 按序号取用。
    pub fn new(
        gpu: &Gpu,
        in_size: (u32, u32),
        out_size: (u32, u32),
        fps: u32,
        srcs: Vec<ID3D11Texture2D>,
    ) -> Res<Self> {
        let (vdev, vctx, en, p) = create_vp(gpu, in_size, out_size, fps)?;
        let mut caps = D3D11_VIDEO_PROCESSOR_CAPS::default();
        unsafe { en.GetVideoProcessorCaps(&mut caps) }?;
        check_conv(&en, DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_FORMAT_B8G8R8A8_UNORM)?;
        check_conv(&en, DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_FORMAT_NV12)?;
        Ok(Self { dev: gpu.dev.clone(), mt: gpu.mt.clone(), vdev, vctx, en, p, out_size, srcs })
    }

    /// 上游 `blit` 的复刻：多层合成到 `output`（层：纹理、是否 alpha）。
    pub fn blit(&self, layers: &[(&ID3D11Texture2D, bool)], output: &ID3D11Texture2D) -> Res<()> {
        let l: Vec<_> = layers.iter().map(|(t, a)| (*t, *a, None)).collect();
        self.blit_rects(&l, output)
    }

    /// 同 `blit`，每层可指定目标矩形（None 表示铺满输出），用于把光标/覆盖层作为小图层并入同一次 Blt。
    pub fn blit_rects(
        &self,
        layers: &[(&ID3D11Texture2D, bool, Option<RECT>)],
        output: &ID3D11Texture2D,
    ) -> Res<()> {
        unsafe { self.mt.Enter() };
        let r = self.blit_locked(layers, output);
        unsafe { self.mt.Leave() };
        r
    }

    fn blit_locked(
        &self,
        layers: &[(&ID3D11Texture2D, bool, Option<RECT>)],
        output: &ID3D11Texture2D,
    ) -> Res<()> {
        let owner = unsafe { output.GetDevice() }?;
        if owner != self.dev {
            return err("输出属于其它设备");
        }
        let mut od = D3D11_TEXTURE2D_DESC::default();
        unsafe { output.GetDesc(&mut od) };
        if od.Width < self.out_size.0 || od.Height < self.out_size.1 {
            return err("输出尺寸不足");
        }
        let mut ov = None;
        unsafe {
            self.vdev.CreateVideoProcessorOutputView(output, &self.en, &out_desc(), Some(&mut ov))
        }?;
        let ov = ov.ok_or("输出 view 为空")?;
        let mut views = Vec::with_capacity(layers.len());
        let target = rect(self.out_size.0, self.out_size.1);
        for (i, (tex, alpha, dst_rect)) in layers.iter().enumerate() {
            let mut d = D3D11_TEXTURE2D_DESC::default();
            unsafe { tex.GetDesc(&mut d) };
            check_conv(&self.en, d.Format, od.Format)?;
            let mut v = None;
            unsafe {
                self.vdev.CreateVideoProcessorInputView(*tex, &self.en, &in_desc(), Some(&mut v))
            }?;
            views.push(v.ok_or("输入 view 为空")?);
            set_states(
                &self.vctx,
                &self.p,
                i as u32,
                &rect(d.Width, d.Height),
                dst_rect.as_ref().unwrap_or(&target),
                *alpha,
                od.Format,
                &target,
            );
        }
        blt(&self.vctx, &self.p, &ov, views)?;
        // 上游 device.check()：查询设备移除原因
        unsafe { self.dev.GetDeviceRemovedReason() }?;
        Ok(())
    }
}

impl Conv for VpBase {
    fn name(&self) -> String {
        "vp_base".into()
    }
    fn run(&mut self, src: usize, dst: &ID3D11Texture2D) -> Res<()> {
        let s = self.srcs[src % self.srcs.len()].clone();
        self.blit(&[(&s, false)], dst)
    }
}

/// 优化写法：视图与状态预建，每帧仅 `VideoProcessorBlt`。
pub struct VpOpt {
    vdev: ID3D11VideoDevice,
    en: ID3D11VideoProcessorEnumerator,
    vctx: ID3D11VideoContext1,
    p: ID3D11VideoProcessor,
    in_views: Vec<ID3D11VideoProcessorInputView>,
    out_views: HashMap<usize, ID3D11VideoProcessorOutputView>,
    tag: String,
}

impl VpOpt {
    /// 创建并预建输入 view；输出 view 首次见到某纹理时创建并缓存；`tag` 用于结果标注。
    pub fn new(
        gpu: &Gpu,
        size: (u32, u32),
        out_size: (u32, u32),
        fps: u32,
        srcs: &[ID3D11Texture2D],
        tag: &str,
    ) -> Res<Self> {
        let (vdev, vctx, en, p) = create_vp(gpu, size, out_size, fps)?;
        check_conv(&en, DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_FORMAT_NV12)?;
        let mut in_views = Vec::new();
        for t in srcs {
            let mut v = None;
            unsafe { vdev.CreateVideoProcessorInputView(t, &en, &in_desc(), Some(&mut v)) }?;
            in_views.push(v.ok_or("输入 view 为空")?);
        }
        let (r_in, r_out) = (rect(size.0, size.1), rect(out_size.0, out_size.1));
        set_states(&vctx, &p, 0, &r_in, &r_out, false, DXGI_FORMAT_NV12, &r_out);
        Ok(Self { vdev, en, vctx, p, in_views, out_views: HashMap::new(), tag: tag.into() })
    }
}

impl Conv for VpOpt {
    fn name(&self) -> String {
        format!("vp_opt{}", self.tag)
    }
    fn run(&mut self, src: usize, dst: &ID3D11Texture2D) -> Res<()> {
        let v = self.in_views[src % self.in_views.len()].clone();
        let key = dst.as_raw() as usize;
        if !self.out_views.contains_key(&key) {
            let mut o = None;
            unsafe { self.vdev.CreateVideoProcessorOutputView(dst, &self.en, &out_desc(), Some(&mut o)) }?;
            self.out_views.insert(key, o.ok_or("输出 view 为空")?);
        }
        blt(&self.vctx, &self.p, &self.out_views[&key], vec![v])
    }
}

/// 单次 Blt 多图层：桌面 + 全屏覆盖层（alpha）+ 小图层（光标/高亮，带目标矩形），直接输出 NV12。
pub struct VpMulti {
    tile: bool,
    base: VpBase,
    overlay: ID3D11Texture2D,
    small: ID3D11Texture2D,
    srcs: Vec<ID3D11Texture2D>,
}

impl VpMulti {
    /// 创建；覆盖层与小图层初始为全透明。`tile` 为真时覆盖层只用 256x256 脏区小图（带目标矩形），否则全屏。
    pub fn new(gpu: &Gpu, size: (u32, u32), srcs: &[ID3D11Texture2D], tile: bool) -> Res<Self> {
        let base = VpBase::new(gpu, size, size, 60, vec![])?;
        let bind = (D3D11_BIND_SHADER_RESOURCE | D3D11_BIND_RENDER_TARGET).0 as u32;
        let zero = vec![0u8; (size.0 * size.1 * 4) as usize];
        let (ow, oh) = if tile { (256, 256) } else { size };
        let overlay = gpu.tex(ow, oh, DXGI_FORMAT_B8G8R8A8_UNORM, bind, Some(&zero[..(ow * oh * 4) as usize]))?;
        let small = gpu.tex(64, 64, DXGI_FORMAT_B8G8R8A8_UNORM, bind, Some(&zero[..64 * 64 * 4]))?;
        Ok(Self { tile, base, overlay, small, srcs: srcs.to_vec() })
    }
}

impl Conv for VpMulti {
    fn name(&self) -> String {
        if self.tile { "vp_multi_tile".into() } else { "vp_multi".into() }
    }
    fn run(&mut self, src: usize, dst: &ID3D11Texture2D) -> Res<()> {
        let s = self.srcs[src % self.srcs.len()].clone();
        let x = 400 + (src as i32 % 64) * 8;
        let r = RECT { left: x, top: 300, right: x + 64, bottom: 364 };
        let o = if self.tile { Some(RECT { left: 64, top: 64, right: 320, bottom: 320 }) } else { None };
        self.base.blit_rects(&[(&s, false, None), (&self.overlay, true, o), (&self.small, true, Some(r))], dst)
    }
}
