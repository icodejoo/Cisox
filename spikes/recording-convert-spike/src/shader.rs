//! 方案 4：自建着色器。
//! - `PsConv`：像素着色器两次 draw（Y 平面 R8、UV 平面 R8G8），BT.709 limited。
//! - `CsConv`：compute 版，每线程处理一个 2x2 块，直接写 NV12 平面 UAV。

use std::collections::HashMap;
use std::ffi::c_void;

use windows::Win32::Graphics::Direct3D::Fxc::*;
use windows::Win32::Graphics::Direct3D::*;
use windows::Win32::Graphics::Direct3D11::*;
use windows::Win32::Graphics::Dxgi::Common::*;
use windows::core::{Interface, PCSTR, s};

use crate::bench::Conv;
use crate::gpu::{Gpu, Res, err};

/// 像素着色器源码：全屏三角形 + Y / UV 输出。
pub const HLSL_PS: &str = r#"
Texture2D<float4> src : register(t0);
SamplerState smp : register(s0);
struct VSOut { float4 pos : SV_Position; float2 uv : TEXCOORD0; };
VSOut vs(uint id : SV_VertexID) {
    VSOut o;
    o.uv = float2((id << 1) & 2, id & 2);
    o.pos = float4(o.uv * float2(2, -2) + float2(-1, 1), 0, 1);
    return o;
}
float ps_y(VSOut i) : SV_Target {
    float3 c = src.Sample(smp, i.uv).rgb;
    float y = dot(c, float3(0.2126, 0.7152, 0.0722));
    return (16.0 + 219.0 * y) / 255.0;
}
float2 ps_uv(VSOut i) : SV_Target {
    float2 uv = i.uv;
#if LEFT_SITING
    uint w, h;
    src.GetDimensions(w, h);
    uv.x -= 0.5 / w;   // 色度左对齐：取偶数列、上下两行均值
#endif
    float3 c = src.Sample(smp, uv).rgb;   // 半尺寸视口 + 双线性
    float y = dot(c, float3(0.2126, 0.7152, 0.0722));
    float cb = (c.b - y) / 1.8556;
    float cr = (c.r - y) / 1.5748;
    return float2(128.0 + 224.0 * cb, 128.0 + 224.0 * cr) / 255.0;
}
"#;

/// compute 着色器源码：每线程一个 2x2 块。
pub const HLSL_CS: &str = r#"
Texture2D<float4> src : register(t0);
RWTexture2D<unorm float> dstY : register(u0);
RWTexture2D<unorm float2> dstUV : register(u1);
cbuffer C : register(b0) { uint2 size; uint2 pad; };
static const float3 K = float3(0.2126, 0.7152, 0.0722);
[numthreads(16, 16, 1)]
void cs(uint3 id : SV_DispatchThreadID) {
    uint2 b = id.xy * 2;
    if (b.x >= size.x || b.y >= size.y) return;
    float3 c0 = src.Load(int3(b + uint2(0, 0), 0)).rgb;
    float3 c1 = src.Load(int3(b + uint2(1, 0), 0)).rgb;
    float3 c2 = src.Load(int3(b + uint2(0, 1), 0)).rgb;
    float3 c3 = src.Load(int3(b + uint2(1, 1), 0)).rgb;
    float y0 = dot(c0, K), y1 = dot(c1, K), y2 = dot(c2, K), y3 = dot(c3, K);
    dstY[b + uint2(0, 0)] = (16.0 + 219.0 * y0) / 255.0;
    dstY[b + uint2(1, 0)] = (16.0 + 219.0 * y1) / 255.0;
    dstY[b + uint2(0, 1)] = (16.0 + 219.0 * y2) / 255.0;
    dstY[b + uint2(1, 1)] = (16.0 + 219.0 * y3) / 255.0;
#if LEFT_SITING
    float3 a = (c0 + c2) * 0.5;
#else
    float3 a = (c0 + c1 + c2 + c3) * 0.25;
#endif
    float ya = dot(a, K);
    dstUV[id.xy] = float2(128.0 + 224.0 * (a.b - ya) / 1.8556, 128.0 + 224.0 * (a.r - ya) / 1.5748) / 255.0;
}
"#;

/// 给源码加上色度采样位置宏（左对齐 = MPEG-2/H.264 默认，居中 = JPEG 风格）。
pub fn with_siting(src: &str, left: bool) -> String {
    format!("#define LEFT_SITING {}
{src}", left as u8)
}

/// 编译 HLSL，失败时返回含编译器输出的错误。
pub fn compile(src: &str, entry: PCSTR, target: PCSTR) -> Res<Vec<u8>> {
    let mut code = None;
    let mut msgs = None;
    let r = unsafe {
        D3DCompile(
            src.as_ptr().cast::<c_void>(),
            src.len(),
            PCSTR::null(),
            None,
            None,
            entry,
            target,
            D3DCOMPILE_OPTIMIZATION_LEVEL3,
            0,
            &mut code,
            Some(&mut msgs),
        )
    };
    if let Err(e) = r {
        let m = msgs
            .map(|b| unsafe {
                String::from_utf8_lossy(std::slice::from_raw_parts(
                    b.GetBufferPointer().cast::<u8>(),
                    b.GetBufferSize(),
                ))
                .into_owned()
            })
            .unwrap_or_default();
        return err(format!("HLSL 编译失败: {e} {m}"));
    }
    let b = code.ok_or("无编译结果")?;
    Ok(unsafe { std::slice::from_raw_parts(b.GetBufferPointer().cast::<u8>(), b.GetBufferSize()) }
        .to_vec())
}

/// 像素着色器转换器。
pub struct PsConv {
    ctx: ID3D11DeviceContext,
    vs: ID3D11VertexShader,
    ps_y: ID3D11PixelShader,
    ps_uv: ID3D11PixelShader,
    smp: ID3D11SamplerState,
    rs: ID3D11RasterizerState,
    srvs: Vec<ID3D11ShaderResourceView>,
    dev: ID3D11Device,
    rtv: HashMap<usize, (ID3D11RenderTargetView, ID3D11RenderTargetView)>,
    size: (u32, u32),
}

impl PsConv {
    /// 创建：输出纹理需带 RENDER_TARGET；平面 RTV 需要驱动支持 NV12 plane view。
    pub fn new(gpu: &Gpu, size: (u32, u32), srcs: &[ID3D11Texture2D], left: bool) -> Res<Self> {
        let dev = &gpu.dev;
        let code = with_siting(HLSL_PS, left);
        let vs_b = compile(&code, s!("vs"), s!("vs_5_0"))?;
        let y_b = compile(&code, s!("ps_y"), s!("ps_5_0"))?;
        let uv_b = compile(&code, s!("ps_uv"), s!("ps_5_0"))?;
        let (mut vs, mut ps_y, mut ps_uv) = (None, None, None);
        unsafe {
            dev.CreateVertexShader(&vs_b, None, Some(&mut vs))?;
            dev.CreatePixelShader(&y_b, None, Some(&mut ps_y))?;
            dev.CreatePixelShader(&uv_b, None, Some(&mut ps_uv))?;
        }
        let sd = D3D11_SAMPLER_DESC {
            Filter: D3D11_FILTER_MIN_MAG_LINEAR_MIP_POINT,
            AddressU: D3D11_TEXTURE_ADDRESS_CLAMP,
            AddressV: D3D11_TEXTURE_ADDRESS_CLAMP,
            AddressW: D3D11_TEXTURE_ADDRESS_CLAMP,
            MaxLOD: f32::MAX,
            ..Default::default()
        };
        let mut smp = None;
        unsafe { dev.CreateSamplerState(&sd, Some(&mut smp))? };
        let rd = D3D11_RASTERIZER_DESC {
            FillMode: D3D11_FILL_SOLID,
            CullMode: D3D11_CULL_NONE,
            DepthClipEnable: true.into(),
            ..Default::default()
        };
        let mut rs = None;
        unsafe { dev.CreateRasterizerState(&rd, Some(&mut rs))? };
        let mut srvs = Vec::new();
        for t in srcs {
            let mut v = None;
            unsafe { dev.CreateShaderResourceView(t, None, Some(&mut v))? };
            srvs.push(v.ok_or("SRV 为空")?);
        }
        Ok(Self {
            ctx: gpu.ctx.clone(),
            vs: vs.ok_or("vs")?,
            ps_y: ps_y.ok_or("ps_y")?,
            ps_uv: ps_uv.ok_or("ps_uv")?,
            smp: smp.ok_or("smp")?,
            rs: rs.ok_or("rs")?,
            srvs,
            dev: gpu.dev.clone(),
            rtv: HashMap::new(),
            size,
        })
    }

    /// 取（或创建并缓存）目标纹理的 Y/UV 平面 RTV。
    fn views(&mut self, t: &ID3D11Texture2D) -> Res<(ID3D11RenderTargetView, ID3D11RenderTargetView)> {
        let key = t.as_raw() as usize;
        if let Some(v) = self.rtv.get(&key) {
            return Ok(v.clone());
        }
        let plane = |fmt| D3D11_RENDER_TARGET_VIEW_DESC {
            Format: fmt,
            ViewDimension: D3D11_RTV_DIMENSION_TEXTURE2D,
            Anonymous: D3D11_RENDER_TARGET_VIEW_DESC_0 { Texture2D: D3D11_TEX2D_RTV { MipSlice: 0 } },
        };
        let (mut a, mut b) = (None, None);
        unsafe {
            self.dev
                .CreateRenderTargetView(t, Some(&plane(DXGI_FORMAT_R8_UNORM)), Some(&mut a))
                .map_err(|e| format!("NV12 Y 平面 RTV: {e}"))?;
            self.dev
                .CreateRenderTargetView(t, Some(&plane(DXGI_FORMAT_R8G8_UNORM)), Some(&mut b))
                .map_err(|e| format!("NV12 UV 平面 RTV: {e}"))?;
        }
        let v = (a.ok_or("RTV 为空")?, b.ok_or("RTV 为空")?);
        self.rtv.insert(key, v.clone());
        Ok(v)
    }
}

impl Conv for PsConv {
    fn name(&self) -> String {
        "ps_shader".into()
    }
    fn run(&mut self, src: usize, dst: &ID3D11Texture2D) -> Res<()> {
        let (ry, ruv) = self.views(dst)?;
        let c = &self.ctx;
        let srv = self.srvs[src % self.srvs.len()].clone();
        unsafe {
            c.IASetPrimitiveTopology(D3D_PRIMITIVE_TOPOLOGY_TRIANGLELIST);
            c.IASetInputLayout(None);
            c.VSSetShader(&self.vs, None);
            c.RSSetState(&self.rs);
            c.PSSetShaderResources(0, Some(&[Some(srv)]));
            c.PSSetSamplers(0, Some(&[Some(self.smp.clone())]));
            let vp = |w: u32, h: u32| D3D11_VIEWPORT {
                TopLeftX: 0.0,
                TopLeftY: 0.0,
                Width: w as f32,
                Height: h as f32,
                MinDepth: 0.0,
                MaxDepth: 1.0,
            };
            // Y 平面
            c.OMSetRenderTargets(Some(&[Some(ry)]), None);
            c.RSSetViewports(Some(&[vp(self.size.0, self.size.1)]));
            c.PSSetShader(&self.ps_y, None);
            c.Draw(3, 0);
            // UV 平面（半尺寸）
            c.OMSetRenderTargets(Some(&[Some(ruv)]), None);
            c.RSSetViewports(Some(&[vp(self.size.0 / 2, self.size.1 / 2)]));
            c.PSSetShader(&self.ps_uv, None);
            c.Draw(3, 0);
            c.PSSetShaderResources(0, Some(&[None]));
            c.OMSetRenderTargets(Some(&[None]), None);
        }
        Ok(())
    }
}

/// compute 着色器转换器。
pub struct CsConv {
    ctx: ID3D11DeviceContext,
    cs: ID3D11ComputeShader,
    cb: ID3D11Buffer,
    srvs: Vec<ID3D11ShaderResourceView>,
    dev: ID3D11Device,
    uav: HashMap<usize, (ID3D11UnorderedAccessView, ID3D11UnorderedAccessView)>,
    size: (u32, u32),
}

impl CsConv {
    /// 创建：输出纹理需带 UNORDERED_ACCESS；平面 UAV 失败时返回错误（记录为不可行原因）。
    pub fn new(gpu: &Gpu, size: (u32, u32), srcs: &[ID3D11Texture2D], left: bool) -> Res<Self> {
        let dev = &gpu.dev;
        let b = compile(&with_siting(HLSL_CS, left), s!("cs"), s!("cs_5_0"))?;
        let mut cs = None;
        unsafe { dev.CreateComputeShader(&b, None, Some(&mut cs))? };
        let bd = D3D11_BUFFER_DESC {
            ByteWidth: 16,
            Usage: D3D11_USAGE_DEFAULT,
            BindFlags: D3D11_BIND_CONSTANT_BUFFER.0 as u32,
            ..Default::default()
        };
        let init = [size.0, size.1, 0, 0];
        let sdata = D3D11_SUBRESOURCE_DATA { pSysMem: init.as_ptr().cast(), ..Default::default() };
        let mut cb = None;
        unsafe { dev.CreateBuffer(&bd, Some(&sdata), Some(&mut cb))? };
        let mut srvs = Vec::new();
        for t in srcs {
            let mut v = None;
            unsafe { dev.CreateShaderResourceView(t, None, Some(&mut v))? };
            srvs.push(v.ok_or("SRV 为空")?);
        }
        Ok(Self { ctx: gpu.ctx.clone(), cs: cs.ok_or("cs")?, cb: cb.ok_or("cb")?, srvs, dev: gpu.dev.clone(), uav: HashMap::new(), size })
    }

    /// 取（或创建并缓存）目标纹理的 Y/UV 平面 UAV；失败即视为该驱动不支持。
    fn views(&mut self, t: &ID3D11Texture2D) -> Res<(ID3D11UnorderedAccessView, ID3D11UnorderedAccessView)> {
        let key = t.as_raw() as usize;
        if let Some(v) = self.uav.get(&key) {
            return Ok(v.clone());
        }
        let plane = |fmt| D3D11_UNORDERED_ACCESS_VIEW_DESC {
            Format: fmt,
            ViewDimension: D3D11_UAV_DIMENSION_TEXTURE2D,
            Anonymous: D3D11_UNORDERED_ACCESS_VIEW_DESC_0 { Texture2D: D3D11_TEX2D_UAV { MipSlice: 0 } },
        };
        let (mut a, mut c) = (None, None);
        unsafe {
            self.dev
                .CreateUnorderedAccessView(t, Some(&plane(DXGI_FORMAT_R8_UNORM)), Some(&mut a))
                .map_err(|e| format!("NV12 Y 平面 UAV: {e}"))?;
            self.dev
                .CreateUnorderedAccessView(t, Some(&plane(DXGI_FORMAT_R8G8_UNORM)), Some(&mut c))
                .map_err(|e| format!("NV12 UV 平面 UAV: {e}"))?;
        }
        let v = (a.ok_or("UAV 为空")?, c.ok_or("UAV 为空")?);
        self.uav.insert(key, v.clone());
        Ok(v)
    }
}

impl Conv for CsConv {
    fn name(&self) -> String {
        "cs_shader".into()
    }
    fn run(&mut self, src: usize, dst: &ID3D11Texture2D) -> Res<()> {
        let (uy, uuv) = self.views(dst)?;
        let c = &self.ctx;
        unsafe {
            c.CSSetShader(&self.cs, None);
            c.CSSetConstantBuffers(0, Some(&[Some(self.cb.clone())]));
            c.CSSetShaderResources(0, Some(&[Some(self.srvs[src % self.srvs.len()].clone())]));
            let uavs = [Some(uy), Some(uuv)];
            c.CSSetUnorderedAccessViews(0, 2, Some(uavs.as_ptr()), None);
            c.Dispatch((self.size.0 / 2).div_ceil(16), (self.size.1 / 2).div_ceil(16), 1);
            c.CSSetShaderResources(0, Some(&[None]));
            let none: [Option<ID3D11UnorderedAccessView>; 2] = [None, None];
            c.CSSetUnorderedAccessViews(0, 2, Some(none.as_ptr()), None);
            c.CSSetShader(None::<&ID3D11ComputeShader>, None);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hlsl_compiles() {
        for left in [false, true] {
            let ps = with_siting(HLSL_PS, left);
            assert!(compile(&ps, s!("vs"), s!("vs_5_0")).is_ok());
            assert!(compile(&ps, s!("ps_y"), s!("ps_5_0")).is_ok());
            assert!(compile(&ps, s!("ps_uv"), s!("ps_5_0")).is_ok());
            assert!(compile(&with_siting(HLSL_CS, left), s!("cs"), s!("cs_5_0")).is_ok());
        }
    }
}
