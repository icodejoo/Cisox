//! 转换合成阶段的 Windows 实现：桌面图层 + 光标图层在**一次** `VideoProcessorBlt` 里直接输出 NV12。
//!
//! 上游的合成要串 4~5 次全分辨率 Blt 加一次全屏 compute；这里只有一次 Blt，
//! 光标是一张小图层（带 alpha），缩放（输出小于选区时）也在同一次 Blt 内完成。
//! 合成在设备 B 上进行：先在栅栏上等采集（设备 A）复制完，Blt 后再 Signal 告诉采集该槽可复用。

use std::collections::{HashMap, VecDeque};
use std::sync::Arc;

use snow_cursor::{AttachedCursorSample, CursorShape, CursorShapeState};
use snow_d3d11::{SharedDevice, Texture};
use windows::Win32::Graphics::Direct3D11::{
    D3D11_BIND_RENDER_TARGET, D3D11_BIND_SHADER_RESOURCE, ID3D11DeviceContext4,
};
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

/// 按形状 ID 同步登记并以 FIFO 淘汰，重复形状不重复占位。
fn cache_insert<T>(
    cache: &mut HashMap<u64, T>,
    order: &mut VecDeque<u64>,
    id: u64,
    value: T,
) -> Option<u64> {
    let mut evicted = None;
    if !cache.contains_key(&id) {
        if order.len() >= CURSOR_CACHE_LIMIT
            && let Some(old) = order.pop_front()
        {
            cache.remove(&old);
            evicted = Some(old);
        }
        order.push_back(id);
    }
    cache.insert(id, value);
    evicted
}

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
    /// 每帧图层暂存（复用容量，避免每帧分配）。
    layers: Vec<VpLayer>,
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
    pub fn new(
        device: SharedDevice,
        hw: Arc<HwContext>,
        src_size: (u32, u32),
        out_size: (u32, u32),
        fps: u32,
    ) -> Result<Self, String> {
        let blitter =
            VideoBlitter::new(device.device(), device.context(), src_size, out_size, fps)?;
        let context4: ID3D11DeviceContext4 = device
            .context()
            .cast()
            .map_err(|e| format!("合成上下文不支持栅栏: {e}"))?;
        Ok(Self {
            device,
            context4,
            blitter,
            hw,
            src_size,
            out_size,
            shapes: HashMap::new(),
            textures: HashMap::new(),
            layers: Vec::new(),
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
                let id = shape.shape_id.get();
                if let Some(old) =
                    cache_insert(&mut self.shapes, &mut self.order, id, shape.clone())
                {
                    self.textures.remove(&old);
                }
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
            .texture(
                shape.width,
                shape.height,
                DXGI_FORMAT_B8G8R8A8_UNORM,
                (D3D11_BIND_SHADER_RESOURCE | D3D11_BIND_RENDER_TARGET).0 as u32,
            )
            .map_err(|e| format!("创建光标贴图失败: {e:#}"))?;
        let bgra = rgba_to_bgra(&shape.shape_rgba);
        {
            let _lock = self.device.lock();
            // SAFETY: bgra 长度已校验为 宽*高*4，行距 = 宽*4。
            unsafe {
                self.device.context().UpdateSubresource(
                    texture.raw(),
                    0,
                    None,
                    bgra.as_ptr().cast(),
                    shape.width * 4,
                    0,
                );
            }
        }
        self.textures.insert(id, texture.clone());
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
    fn compose(
        &mut self,
        frame: &GpuFrame,
        cursor: Option<&AttachedCursorSample>,
        surface: &mut Surface,
    ) -> Result<(), String> {
        let mut layers = std::mem::take(&mut self.layers);
        layers.clear();
        match &frame.tiles {
            // 跨屏：每块显示器一层，目标矩形带偏移；空洞由视频处理器的黑色背景填充
            Some(tiles) => layers.extend(tiles.iter().map(|tile| VpLayer {
                texture: tile.slot.texture_b().clone(),
                source: Rect::full((tile.rect.width, tile.rect.height)),
                destination: tile_destination(tile.rect, self.src_size, self.out_size),
                alpha: false,
            })),
            None => layers.push(VpLayer {
                texture: frame.slot.texture_b().clone(),
                source: Rect::full(self.src_size),
                destination: Rect::full(self.out_size),
                alpha: false,
            }),
        }
        if let Some(sample) = cursor
            && let Some(shape) = self.resolve_shape(sample)
            && let Some((source, destination)) =
                cursor_geometry(sample, &shape, self.src_size, self.out_size)
        {
            let texture = self.cursor_texture(&shape)?;
            layers.push(VpLayer {
                texture: texture.raw().clone(),
                source,
                destination,
                alpha: true,
            });
        }
        let _lock = self.device.lock();
        let fail = |what: &str, e: windows::core::Error| format!("{what}: {e}");
        // 每个槽各自等采集复制完（跨屏时有多个槽）
        // SAFETY: 持有设备锁；栅栏与纹理都属于设备 B（或其共享视图）。
        unsafe {
            for slot in frame.slots() {
                self.context4
                    .Wait(slot.fence_b(), slot.value())
                    .map_err(|e| fail("栅栏等待失败", e))?;
            }
        }
        let blit = self
            .blitter
            .blit(&layers, surface.texture(), surface.slice());
        // 图层持有的纹理引用随即释放，暂存容量留给下一帧
        layers.clear();
        self.layers = layers;
        blit?;
        // SAFETY: 同上；Signal 排在 Blt 之后，采集复用该槽前会等它。
        unsafe {
            for slot in frame.slots() {
                let value = slot.value();
                self.context4
                    .Signal(slot.fence_b(), value + 1)
                    .map_err(|e| fail("栅栏 Signal 失败", e))?;
                slot.set_value(value + 1);
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

#[cfg(test)]
mod cache_tests {
    use super::*;

    /// 隐藏形状也受容量限制，重复 ID 不占新槽，淘汰顺序可重复。
    #[test]
    fn cursor_shape_cache_is_bounded_and_fifo() {
        let mut shapes = HashMap::new();
        let mut order = VecDeque::new();
        for id in 0..CURSOR_CACHE_LIMIT as u64 {
            assert_eq!(cache_insert(&mut shapes, &mut order, id, id), None);
        }
        cache_insert(&mut shapes, &mut order, 0, 99);
        assert_eq!(order.len(), CURSOR_CACHE_LIMIT);
        assert_eq!(cache_insert(&mut shapes, &mut order, 100, 100), Some(0));
        assert!(!shapes.contains_key(&0));
        assert_eq!(shapes.len(), CURSOR_CACHE_LIMIT);
        for id in 101..1000 {
            cache_insert(&mut shapes, &mut order, id, id);
            assert_eq!(shapes.len(), CURSOR_CACHE_LIMIT);
        }
    }
}
