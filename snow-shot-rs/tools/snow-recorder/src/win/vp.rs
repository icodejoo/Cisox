//! D3D11 VideoProcessor 多图层合成：桌面 + 光标在**一次** Blt 里直接输出 NV12。
//!
//! 与上游 `snow-d3d11::VideoProcessor::blit` 的区别：输入/输出 view 按纹理缓存复用（不再每帧新建），
//! 输入直接用原始纹理（采集侧的共享纹理不是上游 `Texture` 类型），设备锁与栅栏等待由调用方包住。

use std::collections::HashMap;
use std::mem::ManuallyDrop;

use windows::Win32::Foundation::RECT;
use windows::Win32::Graphics::Direct3D11::*;
use windows::Win32::Graphics::Dxgi::Common::*;
use windows::core::{BOOL, Interface};

use crate::geom::Rect;

/// 输入/输出 view 缓存上限（超过则清空重建，防止纹理换代后无限增长）。
const VIEW_CACHE_LIMIT: usize = 64;

/// 一个输入图层。
#[derive(Clone)]
pub struct VpLayer {
    /// 图层纹理。
    pub texture: ID3D11Texture2D,
    /// 源矩形（纹理内）。
    pub source: Rect,
    /// 目标矩形（输出内）。
    pub destination: Rect,
    /// 是否按像素 alpha 混合。
    pub alpha: bool,
}

/// 把矩形换成 Win32 `RECT`；空矩形或溢出返回错误。
fn native(rect: Rect) -> Result<RECT, String> {
    if rect.width == 0 || rect.height == 0 {
        return Err("空的视频矩形".into());
    }
    let right = i64::from(rect.x) + i64::from(rect.width);
    let bottom = i64::from(rect.y) + i64::from(rect.height);
    Ok(RECT {
        left: rect.x,
        top: rect.y,
        right: i32::try_from(right).map_err(|e| e.to_string())?,
        bottom: i32::try_from(bottom).map_err(|e| e.to_string())?,
    })
}

/// 输出格式对应的色彩空间（NV12 用限幅 BT.709，与编码器标记一致）。
fn output_color_space(format: DXGI_FORMAT) -> DXGI_COLOR_SPACE_TYPE {
    if format == DXGI_FORMAT_NV12 { DXGI_COLOR_SPACE_YCBCR_STUDIO_G22_LEFT_P709 } else { DXGI_COLOR_SPACE_RGB_FULL_G22_NONE_P709 }
}

/// 视频处理器封装（带 view 缓存）。调用方负责持有设备锁。
pub struct VideoBlitter {
    /// 视频设备接口。
    video_device: ID3D11VideoDevice,
    /// 视频上下文。
    context: ID3D11VideoContext1,
    /// 枚举器。
    enumerator: ID3D11VideoProcessorEnumerator,
    /// 处理器。
    processor: ID3D11VideoProcessor,
    /// 能力。
    caps: D3D11_VIDEO_PROCESSOR_CAPS,
    /// 输出尺寸。
    output_size: (u32, u32),
    /// 输入 view 缓存：按纹理指针，值里保留纹理以免指针被复用。
    input_views: HashMap<usize, (ID3D11Texture2D, ID3D11VideoProcessorInputView)>,
    /// 输出 view 缓存：按（纹理指针, 切片）。
    output_views: HashMap<(usize, u32), (ID3D11Texture2D, ID3D11VideoProcessorOutputView)>,
}

impl VideoBlitter {
    /// 创建处理器并检查所需的格式转换与多流/alpha 能力；不满足返回原因（调用方据此回落软编）。
    ///
    /// # 参数
    /// - `device`：D3D11 设备（需带 `VIDEO_SUPPORT`）。
    /// - `context`：该设备的立即上下文。
    /// - `input_size`：输入（选区）尺寸。
    /// - `output_size`：输出尺寸。
    /// - `fps`：帧率（仅用于内容描述）。
    pub fn new(device: &ID3D11Device, context: &ID3D11DeviceContext, input_size: (u32, u32), output_size: (u32, u32), fps: u32) -> Result<Self, String> {
        let video_device: ID3D11VideoDevice = device.cast().map_err(|e| format!("设备不支持视频处理: {e}"))?;
        let context: ID3D11VideoContext1 = context.cast().map_err(|e| format!("上下文不支持 VideoContext1: {e}"))?;
        let rate = DXGI_RATIONAL { Numerator: fps.max(1), Denominator: 1 };
        let desc = D3D11_VIDEO_PROCESSOR_CONTENT_DESC {
            InputFrameFormat: D3D11_VIDEO_FRAME_FORMAT_PROGRESSIVE,
            InputFrameRate: rate,
            InputWidth: input_size.0,
            InputHeight: input_size.1,
            OutputFrameRate: rate,
            OutputWidth: output_size.0,
            OutputHeight: output_size.1,
            Usage: D3D11_VIDEO_USAGE_PLAYBACK_NORMAL,
        };
        // SAFETY: desc 有效，接口来自同一设备。
        let enumerator = unsafe { video_device.CreateVideoProcessorEnumerator(&desc) }.map_err(|e| format!("创建视频处理器枚举失败: {e}"))?;
        let mut caps = D3D11_VIDEO_PROCESSOR_CAPS::default();
        // SAFETY: caps 是局部变量。
        unsafe { enumerator.GetVideoProcessorCaps(&mut caps) }.map_err(|e| e.to_string())?;
        if caps.MaxInputStreams < 2 {
            return Err("视频处理器不能合成两个输入流".into());
        }
        if caps.FeatureCaps & D3D11_VIDEO_PROCESSOR_FEATURE_CAPS_ALPHA_STREAM.0 as u32 == 0 {
            return Err("视频处理器缺少 alpha 合成能力".into());
        }
        // SAFETY: 枚举器有效。
        let processor = unsafe { video_device.CreateVideoProcessor(&enumerator, 0) }.map_err(|e| format!("创建视频处理器失败: {e}"))?;
        let blitter = Self {
            video_device,
            context,
            enumerator,
            processor,
            caps,
            output_size,
            input_views: HashMap::new(),
            output_views: HashMap::new(),
        };
        blitter.check_conversion(DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_FORMAT_B8G8R8A8_UNORM)?;
        blitter.check_conversion(DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_FORMAT_NV12)?;
        Ok(blitter)
    }

    /// 检查 `input -> output` 的格式转换是否被支持。
    pub fn check_conversion(&self, input: DXGI_FORMAT, output: DXGI_FORMAT) -> Result<(), String> {
        let enumerator: ID3D11VideoProcessorEnumerator1 = self.enumerator.cast().map_err(|e| e.to_string())?;
        // SAFETY: 参数都是值类型。
        let supported = unsafe {
            enumerator.CheckVideoProcessorFormatConversion(input, DXGI_COLOR_SPACE_RGB_FULL_G22_NONE_P709, output, output_color_space(output))
        }
        .map_err(|e| e.to_string())?;
        if supported.as_bool() { Ok(()) } else { Err(format!("不支持的视频格式转换: {input:?} -> {output:?}")) }
    }

    /// 取（或创建）输入 view。
    fn input_view(&mut self, texture: &ID3D11Texture2D) -> Result<ID3D11VideoProcessorInputView, String> {
        let key = texture.as_raw() as usize;
        if let Some((_, view)) = self.input_views.get(&key) {
            return Ok(view.clone());
        }
        if self.input_views.len() >= VIEW_CACHE_LIMIT {
            self.input_views.clear();
        }
        let desc = D3D11_VIDEO_PROCESSOR_INPUT_VIEW_DESC { ViewDimension: D3D11_VPIV_DIMENSION_TEXTURE2D, ..Default::default() };
        let mut view = None;
        // SAFETY: 纹理与枚举器同属一个设备。
        unsafe { self.video_device.CreateVideoProcessorInputView(texture, &self.enumerator, &desc, Some(&mut view)) }
            .map_err(|e| format!("创建输入 view 失败: {e}"))?;
        let view = view.ok_or("输入 view 为空")?;
        self.input_views.insert(key, (texture.clone(), view.clone()));
        Ok(view)
    }

    /// 取（或创建）输出 view。
    fn output_view(&mut self, texture: &ID3D11Texture2D, slice: u32, array_size: u32) -> Result<ID3D11VideoProcessorOutputView, String> {
        let key = (texture.as_raw() as usize, slice);
        if let Some((_, view)) = self.output_views.get(&key) {
            return Ok(view.clone());
        }
        if self.output_views.len() >= VIEW_CACHE_LIMIT {
            self.output_views.clear();
        }
        let desc = if array_size == 1 {
            D3D11_VIDEO_PROCESSOR_OUTPUT_VIEW_DESC {
                ViewDimension: D3D11_VPOV_DIMENSION_TEXTURE2D,
                Anonymous: D3D11_VIDEO_PROCESSOR_OUTPUT_VIEW_DESC_0 { Texture2D: D3D11_TEX2D_VPOV { MipSlice: 0 } },
            }
        } else {
            D3D11_VIDEO_PROCESSOR_OUTPUT_VIEW_DESC {
                ViewDimension: D3D11_VPOV_DIMENSION_TEXTURE2DARRAY,
                Anonymous: D3D11_VIDEO_PROCESSOR_OUTPUT_VIEW_DESC_0 {
                    Texture2DArray: D3D11_TEX2D_ARRAY_VPOV { MipSlice: 0, FirstArraySlice: slice, ArraySize: 1 },
                },
            }
        };
        let mut view = None;
        // SAFETY: 纹理与枚举器同属一个设备。
        unsafe { self.video_device.CreateVideoProcessorOutputView(texture, &self.enumerator, &desc, Some(&mut view)) }
            .map_err(|e| format!("创建输出 view 失败: {e}"))?;
        let view = view.ok_or("输出 view 为空")?;
        self.output_views.insert(key, (texture.clone(), view.clone()));
        Ok(view)
    }

    /// 把若干图层合成到输出纹理的指定切片（一次 `VideoProcessorBlt`）。调用方须持有设备锁。
    ///
    /// # 参数
    /// - `layers`：图层，后面的盖在前面上；第一层应覆盖整个输出。
    /// - `output`：输出纹理（NV12 帧池纹理）。
    /// - `slice`：数组切片号。
    pub fn blit(&mut self, layers: &[VpLayer], output: &ID3D11Texture2D, slice: u32) -> Result<(), String> {
        if layers.len() > self.caps.MaxInputStreams as usize {
            return Err("视频处理器输入流过多".into());
        }
        let mut output_desc = D3D11_TEXTURE2D_DESC::default();
        // SAFETY: desc 是局部变量。
        unsafe { output.GetDesc(&mut output_desc) };
        if output_desc.Width < self.output_size.0 || output_desc.Height < self.output_size.1 || slice >= output_desc.ArraySize {
            return Err("输出纹理尺寸或切片无效".into());
        }
        let output_view = self.output_view(output, slice, output_desc.ArraySize)?;
        let mut views = Vec::with_capacity(layers.len());
        for (index, layer) in layers.iter().enumerate() {
            let view = self.input_view(&layer.texture)?;
            let source = native(layer.source)?;
            let destination = native(layer.destination)?;
            let index = index as u32;
            // SAFETY: 处理器与流序号有效，矩形指针指向局部变量。
            unsafe {
                self.context.VideoProcessorSetStreamFrameFormat(&self.processor, index, D3D11_VIDEO_FRAME_FORMAT_PROGRESSIVE);
                self.context.VideoProcessorSetStreamAutoProcessingMode(&self.processor, index, false);
                self.context.VideoProcessorSetStreamColorSpace1(&self.processor, index, DXGI_COLOR_SPACE_RGB_FULL_G22_NONE_P709);
                self.context.VideoProcessorSetStreamSourceRect(&self.processor, index, true, Some(&source));
                self.context.VideoProcessorSetStreamDestRect(&self.processor, index, true, Some(&destination));
                self.context.VideoProcessorSetStreamAlpha(&self.processor, index, layer.alpha, 1.0);
                self.context.VideoProcessorSetStreamRotation(&self.processor, index, false, D3D11_VIDEO_PROCESSOR_ROTATION_IDENTITY);
            }
            views.push(view);
        }
        let target = native(Rect::full(self.output_size))?;
        // SAFETY: 处理器有效，指针指向局部变量。
        unsafe {
            self.context.VideoProcessorSetOutputTargetRect(&self.processor, true, Some(&target));
            self.context.VideoProcessorSetOutputColorSpace1(&self.processor, output_color_space(output_desc.Format));
            self.context.VideoProcessorSetOutputBackgroundColor(
                &self.processor,
                false,
                &D3D11_VIDEO_COLOR {
                    Anonymous: D3D11_VIDEO_COLOR_0 { RGBA: D3D11_VIDEO_COLOR_RGBA { R: 0.0, G: 0.0, B: 0.0, A: 1.0 } },
                },
            );
            self.context.VideoProcessorSetOutputAlphaFillMode(&self.processor, D3D11_VIDEO_PROCESSOR_ALPHA_FILL_MODE_OPAQUE, 0);
        }
        let mut streams: Vec<_> = views
            .into_iter()
            .map(|view| D3D11_VIDEO_PROCESSOR_STREAM { Enable: BOOL(1), pInputSurface: ManuallyDrop::new(Some(view)), ..Default::default() })
            .collect();
        // SAFETY: 处理器、输出 view、流数组都有效。
        let result = unsafe { self.context.VideoProcessorBlt(&self.processor, &output_view, 0, &streams) };
        // windows-rs 把含 COM 指针的 C 结构体字段标成 ManuallyDrop，需手动释放。
        for stream in &mut streams {
            // SAFETY: 每个流的 pInputSurface 只在上面构造一次。
            unsafe { ManuallyDrop::drop(&mut stream.pInputSurface) };
        }
        result.map_err(|e| format!("VideoProcessorBlt 失败: {e}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 矩形换算：空矩形与溢出被拒绝，正常矩形按 right/bottom 换算。
    #[test]
    fn native_rect_validation() {
        assert!(native(Rect { x: 0, y: 0, width: 0, height: 4 }).is_err());
        assert!(native(Rect { x: i32::MAX, y: 0, width: 2, height: 2 }).is_err());
        let r = native(Rect { x: -2, y: 3, width: 10, height: 20 }).unwrap();
        assert_eq!((r.left, r.top, r.right, r.bottom), (-2, 3, 8, 23));
    }

    /// 读回 NV12 纹理的 Y 平面（行主序，`宽*高` 字节）。
    fn read_luma(device: &snow_d3d11::SharedDevice, nv12: &ID3D11Texture2D, size: (u32, u32)) -> Option<Vec<u8>> {
        let desc = D3D11_TEXTURE2D_DESC {
            Width: size.0,
            Height: size.1,
            MipLevels: 1,
            ArraySize: 1,
            Format: DXGI_FORMAT_NV12,
            SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
            Usage: D3D11_USAGE_STAGING,
            CPUAccessFlags: D3D11_CPU_ACCESS_READ.0 as u32,
            ..Default::default()
        };
        let mut staging = None;
        // SAFETY: desc 有效，输出指针指向局部变量。
        unsafe { device.device().CreateTexture2D(&desc, None, Some(&mut staging)) }.ok()?;
        let staging = staging?;
        let _lock = device.lock();
        let ctx = device.context();
        // SAFETY: 两个纹理同属一个设备，持有设备锁；映射只读且随后解除。
        unsafe {
            ctx.CopyResource(&staging, nv12);
            let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
            ctx.Map(&staging, 0, D3D11_MAP_READ, 0, Some(&mut mapped)).ok()?;
            let mut luma = Vec::with_capacity((size.0 * size.1) as usize);
            for row in 0..size.1 as usize {
                let line = std::slice::from_raw_parts((mapped.pData as *const u8).add(row * mapped.RowPitch as usize), size.0 as usize);
                luma.extend_from_slice(line);
            }
            ctx.Unmap(&staging, 0);
            Some(luma)
        }
    }

    /// 真机：灰色桌面 + 白色不透明光标图层，一次 Blt 输出 NV12。
    /// 回读 Y 平面：光标区域接近限幅白（235），其余是灰对应的限幅亮度（约 126）；不带光标图层时整幅都是灰。
    /// 本机没有视频处理器或不支持所需格式时跳过。
    #[test]
    fn composites_desktop_and_cursor_into_nv12_when_supported() {
        use windows::Win32::Graphics::Dxgi::{CreateDXGIFactory1, IDXGIAdapter, IDXGIFactory1};
        let Ok(factory) = (unsafe { CreateDXGIFactory1::<IDXGIFactory1>() }) else { return };
        let Ok(adapter) = (unsafe { factory.EnumAdapters1(0) }) else { return };
        let Ok(base) = adapter.cast::<IDXGIAdapter>() else { return };
        let Ok(device) = snow_d3d11::SharedDevice::create(&base) else { return };
        let size = (64u32, 64u32);
        let mut blitter = match VideoBlitter::new(device.device(), device.context(), size, size, 30) {
            Ok(b) => b,
            Err(e) => {
                eprintln!("跳过（视频处理器不满足要求）: {e}");
                return;
            }
        };
        let binds = (D3D11_BIND_SHADER_RESOURCE | D3D11_BIND_RENDER_TARGET).0 as u32;
        let Ok(desktop) = device.texture(size.0, size.1, DXGI_FORMAT_B8G8R8A8_UNORM, binds) else { return };
        let Ok(cursor) = device.texture(16, 16, DXGI_FORMAT_B8G8R8A8_UNORM, binds) else { return };
        let Ok(output) = device.texture(size.0, size.1, DXGI_FORMAT_NV12, D3D11_BIND_RENDER_TARGET.0 as u32) else { return };
        let gray: Vec<u8> = (0..size.0 * size.1).flat_map(|_| [128u8, 128, 128, 255]).collect();
        let white: Vec<u8> = (0..16 * 16).flat_map(|_| [255u8, 255, 255, 255]).collect();
        {
            let _lock = device.lock();
            // SAFETY: 缓冲长度与 宽*高*4 一致，行距 = 宽*4。
            unsafe {
                device.context().UpdateSubresource(desktop.raw(), 0, None, gray.as_ptr().cast(), size.0 * 4, 0);
                device.context().UpdateSubresource(cursor.raw(), 0, None, white.as_ptr().cast(), 16 * 4, 0);
            }
        }
        let desktop_layer = VpLayer { texture: desktop.raw().clone(), source: Rect::full(size), destination: Rect::full(size), alpha: false };
        let cursor_layer = VpLayer {
            texture: cursor.raw().clone(),
            source: Rect::full((16, 16)),
            destination: Rect { x: 20, y: 20, width: 16, height: 16 },
            alpha: true,
        };
        let blit = |blitter: &mut VideoBlitter, layers: &[VpLayer]| {
            let _lock = device.lock();
            blitter.blit(layers, output.raw(), 0)
        };
        if let Err(e) = blit(&mut blitter, std::slice::from_ref(&desktop_layer)) {
            eprintln!("跳过（Blt 不可用）: {e}");
            return;
        }
        let Some(plain) = read_luma(&device, output.raw(), size) else { return };
        let background = i32::from(plain[10 * size.0 as usize + 10]);
        assert!((120..=132).contains(&background), "灰底亮度 {background}");
        assert!(plain.iter().all(|&y| (i32::from(y) - background).abs() <= 2), "纯桌面应整幅一致");
        blit(&mut blitter, &[desktop_layer, cursor_layer]).unwrap();
        let Some(with_cursor) = read_luma(&device, output.raw(), size) else { return };
        let inside = i32::from(with_cursor[28 * size.0 as usize + 28]);
        let outside = i32::from(with_cursor[10 * size.0 as usize + 10]);
        assert!(inside >= 225, "光标区域亮度 {inside}");
        assert!((outside - background).abs() <= 2, "光标区外应保持桌面亮度: {outside} vs {background}");
    }

    /// NV12 用限幅 BT.709，其余用全幅 RGB。
    #[test]
    fn color_space_by_format() {
        assert_eq!(output_color_space(DXGI_FORMAT_NV12), DXGI_COLOR_SPACE_YCBCR_STUDIO_G22_LEFT_P709);
        assert_eq!(output_color_space(DXGI_FORMAT_B8G8R8A8_UNORM), DXGI_COLOR_SPACE_RGB_FULL_G22_NONE_P709);
    }
}
