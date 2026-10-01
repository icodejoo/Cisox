//! 转换合成阶段的 Windows 实现：桌面图层 + 光标图层在**一次** `VideoProcessorBlt` 里直接输出 NV12。
//!
//! 上游的合成要串 4~5 次全分辨率 Blt 加一次全屏 compute；这里只有一次 Blt，
//! 光标是一张小图层（带 alpha），缩放（输出小于选区时）也在同一次 Blt 内完成。
//! 合成在设备 B 上进行：先在栅栏上等采集（设备 A）复制完，Blt 后再 Signal 告诉采集该槽可复用。

use std::collections::{HashMap, VecDeque};
use std::sync::Arc;

use snow_cursor::{AttachedCursorSample, CursorShape, CursorShapeState};
use snow_d3d11::{SharedDevice, Texture};
use windows::Win32::Graphics::Direct3D11::{D3D11_BIND_RENDER_TARGET, D3D11_BIND_SHADER_RESOURCE, ID3D11DeviceContext4};
use windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT_B8G8R8A8_UNORM;
use windows::core::Interface;

use crate::geom::{Rect, cursor_geometry, rgba_to_bgra};
use crate::pipeline::FrameComposer;
use crate::win::dda::GpuFrame;
use crate::win::hwenc::{HwContext, Surface};
use crate::win::span::{check_streams, tile_destination};
use crate::win::vp::{VideoBlitter, VpLayer};

/// 光标贴图缓存上限（按形状 ID，先进先出淘汰）。
const CURSOR_CACHE_LIMIT: usize = 16;

/// GPU 合成器。
pub struct GpuComposer {
    /// 合成设备 B。
    device: SharedDevice,
    /// 设备 B 的带栅栏上下文。
    context4: ID3D11DeviceContext4,
    /// 视频处理器。
    blitter: VideoBlitter,
    /// 编码表面池。
    hw: Arc<HwContext>,
    /// 选区尺寸。
    src_size: (u32, u32),
    /// 输出尺寸。
    out_size: (u32, u32),
    /// 已见过的光标形状（`Cached` 状态查表用）。
    shapes: HashMap<u64, CursorShape>,
    /// 光标贴图缓存。
    textures: HashMap<u64, Texture>,
    /// 贴图插入顺序（淘汰用）。
    order: VecDeque<u64>,
}

impl GpuComposer {
    /// 创建合成器；视频处理器/栅栏不满足要求时返回错误（调用方回落软编）。
    ///
    /// # 参数
    /// - `device`：合成设备 B。
    /// - `hw`：编码表面池。
    /// - `src_size`：选区尺寸。
    /// - `out_size`：输出尺寸。
    /// - `fps`：帧率（仅用于视频处理器内容描述）。
    pub fn new(device: SharedDevice, hw: Arc<HwContext>, src_size: (u32, u32), out_size: (u32, u32), fps: u32) -> Result<Self, String> {
        let blitter = VideoBlitter::new(device.device(), device.context(), src_size, out_size, fps)?;
        let context4: ID3D11DeviceContext4 = device.context().cast().map_err(|e| format!("合成上下文不支持栅栏: {e}"))?;
        Ok(Self {
            device,
            context4,
            blitter,
            hw,
            src_size,
            out_size,
            shapes: HashMap::new(),
            textures: HashMap::new(),
            order: VecDeque::new(),
        })
    }

    /// 检查视频处理器的输入流够不够拼接 `tiles` 块显示器（外加光标层）；不够返回回落原因。
    ///
    /// # 参数
    /// - `tiles`：显示器块数。
    pub fn ensure_tile_streams(&self, tiles: usize) -> Result<(), String> {
        check_streams(tiles, self.blitter.max_input_streams())
    }

    /// 解析光标形状（`Embedded` 入缓存，`Cached` 查表）。
    fn resolve_shape(&mut self, sample: &AttachedCursorSample) -> Option<CursorShape> {
        match &sample.shape {
            CursorShapeState::Embedded(shape) => {
                self.shapes.insert(shape.shape_id.get(), shape.clone());
                Some(shape.clone())
            }
            CursorShapeState::Cached(id) => self.shapes.get(&id.get()).cloned(),
            CursorShapeState::Unavailable => None,
        }
    }

    /// 取（或创建）光标贴图。
    fn cursor_texture(&mut self, shape: &CursorShape) -> Result<Texture, String> {
        let id = shape.shape_id.get();
        if let Some(t) = self.textures.get(&id) {
            return Ok(t.clone());
        }
        let expected = shape.width as usize * shape.height as usize * 4;
        if shape.shape_rgba.len() != expected {
            return Err("光标形状像素长度不符".into());
        }
        // 视频处理器对 BGRA 输入最通用（与桌面同格式），RGBA 形状在此换成 BGRA
        let texture = self
            .device
            .texture(shape.width, shape.height, DXGI_FORMAT_B8G8R8A8_UNORM, (D3D11_BIND_SHADER_RESOURCE | D3D11_BIND_RENDER_TARGET).0 as u32)
            .map_err(|e| format!("创建光标贴图失败: {e:#}"))?;
        let bgra = rgba_to_bgra(&shape.shape_rgba);
        {
            let _lock = self.device.lock();
            // SAFETY: bgra 长度已校验为 宽*高*4，行距 = 宽*4。
            unsafe {
                self.device.context().UpdateSubresource(texture.raw(), 0, None, bgra.as_ptr().cast(), shape.width * 4, 0);
            }
        }
        if self.order.len() >= CURSOR_CACHE_LIMIT
            && let Some(old) = self.order.pop_front()
        {
            self.textures.remove(&old);
        }
        self.textures.insert(id, texture.clone());
        self.order.push_back(id);
        Ok(texture)
    }
}

impl FrameComposer for GpuComposer {
    type Frame = GpuFrame;
    type Surface = Surface;

    /// 从编码表面池取一张表面。
    fn acquire_surface(&mut self) -> Result<Option<Surface>, String> {
        self.hw.allocate()
    }

    /// 桌面 + 光标一次 Blt 合成到表面；栅栏保证与采集复制、采集复用之间的顺序。
    fn compose(&mut self, frame: &GpuFrame, cursor: Option<&AttachedCursorSample>, surface: &mut Surface) -> Result<(), String> {
        let mut layers = match &frame.tiles {
            // 跨屏：每块显示器一层，目标矩形带偏移；空洞由视频处理器的黑色背景填充
            Some(tiles) => tiles
                .iter()
                .map(|tile| VpLayer {
                    texture: tile.slot.texture_b().clone(),
                    source: Rect::full((tile.rect.width, tile.rect.height)),
                    destination: tile_destination(tile.rect, self.src_size, self.out_size),
                    alpha: false,
                })
                .collect(),
            None => vec![VpLayer {
                texture: frame.slot.texture_b().clone(),
                source: Rect::full(self.src_size),
                destination: Rect::full(self.out_size),
                alpha: false,
            }],
        };
        if let Some(sample) = cursor
            && let Some(shape) = self.resolve_shape(sample)
            && let Some((source, destination)) = cursor_geometry(sample, &shape, self.src_size, self.out_size)
        {
            let texture = self.cursor_texture(&shape)?;
            layers.push(VpLayer { texture: texture.raw().clone(), source, destination, alpha: true });
        }
        let _lock = self.device.lock();
        let fail = |what: &str, e: windows::core::Error| format!("{what}: {e}");
        // 每个槽各自等采集复制完（跨屏时有多个槽）
        // SAFETY: 持有设备锁；栅栏与纹理都属于设备 B（或其共享视图）。
        unsafe {
            match &frame.tiles {
                Some(tiles) => {
                    for tile in tiles.iter() {
                        self.context4.Wait(tile.slot.fence_b(), tile.slot.value()).map_err(|e| fail("栅栏等待失败", e))?;
                    }
                }
                None => self.context4.Wait(frame.slot.fence_b(), frame.slot.value()).map_err(|e| fail("栅栏等待失败", e))?,
            }
        }
        self.blitter.blit(&layers, surface.texture(), surface.slice())?;
        // SAFETY: 同上；Signal 排在 Blt 之后，采集复用该槽前会等它。
        unsafe {
            match &frame.tiles {
                Some(tiles) => {
                    for tile in tiles.iter() {
                        let value = tile.slot.value();
                        self.context4.Signal(tile.slot.fence_b(), value + 1).map_err(|e| fail("栅栏 Signal 失败", e))?;
                        tile.slot.set_value(value + 1);
                    }
                }
                None => {
                    let value = frame.slot.value();
                    self.context4.Signal(frame.slot.fence_b(), value + 1).map_err(|e| fail("栅栏 Signal 失败", e))?;
                    frame.slot.set_value(value + 1);
                }
            }
            self.device.context().Flush();
        }
        Ok(())
    }

    /// 在用的编码表面数。
    fn in_flight(&self) -> usize {
        self.hw.outstanding()
    }
}

