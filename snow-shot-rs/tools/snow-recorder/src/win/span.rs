//! 跨显示器选区的硬件采集：每块相关显示器各开一路桌面复制，只复制落入选区的那一块进各自的共享槽，
//! 合成阶段再用 VideoProcessor 每块一层一次拼成 NV12。
//!
//! 设计要点（与 `dda.rs` 的单屏路径互不影响，单屏仍走 `DdaCapture`）：
//! - 所有相关显示器必须在同一块适配器上，共用一个采集设备 A 与一个合成设备 B；跨适配器回落软编。
//! - 槽按"选区裁剪块"的尺寸分配，不按整块显示器分配。
//! - 任一屏更新就出一帧，其余屏沿用各自上一块（同一个槽的引用计数），不额外复制。
//! - 选区含空洞（两屏不齐）时，空洞处由 VideoProcessor 的黑色背景填充。
//! - 单线程轮询各输出，不新增线程。
//! - 旋转屏回落软编；HDR 屏在首帧格式检查时报错回落。

use std::sync::Arc;
use std::time::{Duration, Instant};

use snow_cursor::{AttachedCursorSample, CursorProjector, CursorSampler, CursorShape, CursorShapeState, CursorTargetInfo};
use snow_d3d11::SharedDevice;
use windows::Win32::Graphics::Direct3D11::*;
use windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT_B8G8R8A8_UNORM;
use windows::Win32::Graphics::Dxgi::{
    DXGI_ERROR_ACCESS_LOST, DXGI_ERROR_WAIT_TIMEOUT, DXGI_OUTDUPL_FRAME_INFO, IDXGIAdapter, IDXGIOutput1, IDXGIOutputDuplication, IDXGIResource,
};
use windows::core::Interface;

use crate::frametrace::{Thread, TraceBuf, code};
use crate::geom::{Rect, scale_coordinate};
use crate::pipeline::{CaptureDiag, CaptureFault, CaptureSource, CaptureStats, Captured};
use crate::timeline::QpcAnchor;
use crate::win::dda::{
    COMPOSE_GPU_PRIORITY, DIAG_LIMIT, ENV_CAPTURE_GPU_PRIORITY, ENV_COMPOSE_GPU_PRIORITY, FrameGuard, GpuFrame, POLL_SLEEP, POOL_SIZE, SharedSlot, Tile,
    copy_into_slot, create_slot, env_priority, list_adapters, set_gpu_priority,
};

/// 一个显示器输出的几何与归属（规划用，不含 COM 对象）。
#[derive(Debug, Clone)]
pub struct OutputInfo {
    /// 所属适配器序号。
    pub adapter: usize,
    /// 适配器描述（回落原因文案用）。
    pub adapter_name: String,
    /// 输出在虚拟桌面里的矩形。
    pub rect: Rect,
    /// 是否被旋转。
    pub rotated: bool,
}

/// 跨屏规划里的一块：某个输出与选区的交集。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpanPart {
    /// 输出序号（对应传入的 `OutputInfo` 切片）。
    pub output: usize,
    /// 输出内的裁剪框（输出本地坐标）。
    pub crop: Rect,
    /// 块在选区内的矩形（选区坐标）。
    pub tile: Rect,
}

/// 跨屏规划结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpanPlan {
    /// 所有输出共同所在的适配器序号。
    pub adapter: usize,
    /// 各块，顺序同输出枚举顺序。
    pub parts: Vec<SpanPart>,
    /// 选区是否含空洞（未被任何显示器覆盖的部分）。
    pub has_hole: bool,
}

/// 两个矩形的交集；不相交返回 `None`。
fn intersect(a: Rect, b: Rect) -> Option<Rect> {
    let left = i64::from(a.x).max(i64::from(b.x));
    let top = i64::from(a.y).max(i64::from(b.y));
    let right = (i64::from(a.x) + i64::from(a.width)).min(i64::from(b.x) + i64::from(b.width));
    let bottom = (i64::from(a.y) + i64::from(a.height)).min(i64::from(b.y) + i64::from(b.height));
    if left >= right || top >= bottom {
        return None;
    }
    Some(Rect { x: left as i32, y: top as i32, width: (right - left) as u32, height: (bottom - top) as u32 })
}

/// 把选区按显示器切分成子矩形，并判定能否走跨屏硬件路径。
///
/// # 参数
/// - `region`：选区（虚拟桌面坐标）。
/// - `outputs`：全部输出（含所属适配器与旋转标记）。
///
/// # 返回
/// - `Ok(None)`：不是跨屏选区（某个输出完整包含，或相交的输出少于两块），调用方按单屏流程处理；
/// - `Ok(Some(规划))`：可走跨屏硬件路径；
/// - `Err(原因)`：跨屏但不支持（含旋转屏、跨适配器），调用方回落软编。
///
/// # 示例
/// ```ignore
/// let plan = plan_span(Rect { x: 1280, y: 0, width: 2560, height: 1440 }, &outputs)?;
/// ```
pub fn plan_span(region: Rect, outputs: &[OutputInfo]) -> Result<Option<SpanPlan>, String> {
    let mut parts: Vec<SpanPart> = Vec::new();
    for (index, output) in outputs.iter().enumerate() {
        let Some(tile_abs) = intersect(region, output.rect) else { continue };
        if tile_abs == region {
            return Ok(None);
        }
        // 镜像（克隆）显示的输出矩形相同，只取先出现的一块
        if parts.iter().any(|p| outputs[p.output].rect == output.rect) {
            continue;
        }
        parts.push(SpanPart {
            output: index,
            crop: Rect { x: tile_abs.x - output.rect.x, y: tile_abs.y - output.rect.y, ..tile_abs },
            tile: Rect { x: tile_abs.x - region.x, y: tile_abs.y - region.y, ..tile_abs },
        });
    }
    if parts.len() < 2 {
        return Ok(None);
    }
    if parts.iter().any(|p| outputs[p.output].rotated) {
        return Err("显示器被旋转，自建 GPU 采集不支持（跨屏选区回落软件路径）".into());
    }
    let mut adapters: Vec<(usize, &str)> = Vec::new();
    for part in &parts {
        let output = &outputs[part.output];
        if !adapters.iter().any(|(a, _)| *a == output.adapter) {
            adapters.push((output.adapter, output.adapter_name.as_str()));
        }
    }
    if adapters.len() > 1 {
        let names: Vec<&str> = adapters.iter().map(|(_, name)| *name).collect();
        return Err(format!("跨适配器选区暂不支持硬件路径（适配器: {}）", names.join(" / ")));
    }
    let covered: u64 = parts.iter().map(|p| u64::from(p.tile.width) * u64::from(p.tile.height)).sum();
    let has_hole = covered < u64::from(region.width) * u64::from(region.height);
    Ok(Some(SpanPlan { adapter: adapters[0].0, parts, has_hole }))
}

/// 块在输出画面里的目标矩形：相邻块共用同一条边界坐标（各自只取整一次），不会出现 1px 缝或重叠。
///
/// # 参数
/// - `tile`：块在选区内的矩形。
/// - `src_size`：选区尺寸。
/// - `out_size`：输出尺寸。
///
/// # 返回
/// 输出坐标系里的目标矩形（宽高至少为 1）。
///
/// # 示例
/// ```ignore
/// let dest = tile_destination(Rect { x: 1280, y: 0, width: 1280, height: 1440 }, (2560, 1440), (1920, 1080));
/// assert_eq!((dest.x, dest.width), (960, 960));
/// ```
pub fn tile_destination(tile: Rect, src_size: (u32, u32), out_size: (u32, u32)) -> Rect {
    let right = i32::try_from(i64::from(tile.x) + i64::from(tile.width)).unwrap_or(i32::MAX);
    let bottom = i32::try_from(i64::from(tile.y) + i64::from(tile.height)).unwrap_or(i32::MAX);
    let (l, r) = (scale_coordinate(tile.x, src_size.0, out_size.0), scale_coordinate(right, src_size.0, out_size.0));
    let (t, b) = (scale_coordinate(tile.y, src_size.1, out_size.1), scale_coordinate(bottom, src_size.1, out_size.1));
    Rect { x: l, y: t, width: (r - l).max(1) as u32, height: (b - t).max(1) as u32 }
}

/// 检查视频处理器的输入流是否够用：每块显示器一层，外加一层光标。
///
/// # 参数
/// - `tiles`：显示器块数。
/// - `max_streams`：处理器支持的最大输入流数。
///
/// # 返回
/// 够用返回 `Ok`；不够返回回落原因。
pub fn check_streams(tiles: usize, max_streams: u32) -> Result<(), String> {
    if tiles + 1 > max_streams as usize {
        return Err(format!("视频处理器输入流不足以拼接 {tiles} 块显示器"));
    }
    Ok(())
}

/// 一路输出的采集单元。
struct SpanOutput {
    /// 输出（重建复制用）。
    output: IDXGIOutput1,
    /// 桌面复制接口。
    duplication: IDXGIOutputDuplication,
    /// 该输出自己的槽池（槽尺寸 = 裁剪块尺寸）。
    slots: Vec<Arc<SharedSlot>>,
    /// 在输出里的裁剪框。
    crop: D3D11_BOX,
    /// 块在选区内的矩形。
    rect: Rect,
    /// 该输出最近一次复制的槽（未更新时沿用）。
    current: Option<Arc<SharedSlot>>,
}

/// 跨显示器选区采集器（同适配器）。
pub struct SpanCapture {
    /// 采集设备 A（各输出共用）。
    device: SharedDevice,
    /// 设备 A 的带栅栏上下文。
    context4: ID3D11DeviceContext4,
    /// 各输出的采集单元。
    outputs: Vec<SpanOutput>,
    /// 下一次从哪路输出开始轮询（轮转，避免某路饿死）。
    next_output: usize,
    /// 权限丢失的输出序号（`recreate` 只重建它）。
    lost: Option<usize>,
    /// 选区（虚拟桌面坐标）。
    region: (i32, i32, u32, u32),
    /// 光标采样器。
    sampler: CursorSampler,
    /// 光标投影器。
    projector: CursorProjector,
    /// 最近见过的光标形状（`Cached` 状态补全用）。
    retained_shape: Option<CursorShape>,
    /// 最近一张带桌面内容的帧（光标单独移动时复用）。
    latest: Option<Captured<GpuFrame>>,
    /// QPC 锚点。
    anchor: Option<QpcAnchor>,
    /// 计数。
    stats: CaptureStats,
    /// 诊断：相邻两轮轮询的间隔（毫秒）。
    acquire_gap_ms: Vec<f32>,
    /// 诊断：每帧复制耗时（毫秒）。
    copy_ms: Vec<f32>,
    /// 上一轮轮询的时刻。
    last_acquire: Option<Instant>,
    /// 帧追踪缓冲（未开启时是空操作）；`src` 列标注来源输出序号。
    trace: TraceBuf,
}

impl SpanCapture {
    /// 打开跨屏采集；不是跨屏选区返回 `Ok(None)`。
    ///
    /// # 参数
    /// - `region`：选区 `(x, y, 宽, 高)`（虚拟桌面坐标）。
    /// - `anchor`：QPC 与 `Instant` 的对应。
    ///
    /// # 返回
    /// `Ok(Some((采集器, 合成设备 B)))`；`Ok(None)` 表示应走单屏流程；`Err` 为不支持/失败的回落原因。
    pub fn open(region: (i32, i32, u32, u32), anchor: Option<QpcAnchor>) -> Result<Option<(Self, SharedDevice)>, String> {
        let adapters = list_adapters()?;
        let mut infos: Vec<OutputInfo> = Vec::new();
        let mut handles = Vec::new();
        for (adapter_index, (adapter, info)) in adapters.iter().enumerate() {
            let mut o = 0;
            // SAFETY: 只调用 DXGI 枚举接口。
            while let Ok(output) = unsafe { adapter.EnumOutputs(o) } {
                o += 1;
                // SAFETY: 同上。
                let Ok(desc) = (unsafe { output.GetDesc() }) else { continue };
                let r = desc.DesktopCoordinates;
                infos.push(OutputInfo {
                    adapter: adapter_index,
                    adapter_name: info.description.clone(),
                    rect: Rect { x: r.left, y: r.top, width: (r.right - r.left).max(0) as u32, height: (r.bottom - r.top).max(0) as u32 },
                    rotated: desc.Rotation.0 > 1,
                });
                handles.push(output);
            }
        }
        let rect = Rect { x: region.0, y: region.1, width: region.2, height: region.3 };
        let Some(plan) = plan_span(rect, &infos)? else { return Ok(None) };
        let base: IDXGIAdapter = adapters[plan.adapter].0.cast().map_err(|e| e.to_string())?;
        let compose = SharedDevice::create(&base).map_err(|e| format!("创建合成设备失败: {e:#}"))?;
        let device = SharedDevice::create(&base).map_err(|e| format!("创建采集设备失败: {e:#}"))?;
        set_gpu_priority(&compose, env_priority(ENV_COMPOSE_GPU_PRIORITY).unwrap_or(COMPOSE_GPU_PRIORITY));
        if let Some(priority) = env_priority(ENV_CAPTURE_GPU_PRIORITY) {
            set_gpu_priority(&device, priority);
        }
        let mut outputs = Vec::with_capacity(plan.parts.len());
        for (n, part) in plan.parts.iter().enumerate() {
            let output: IDXGIOutput1 = handles[part.output].cast().map_err(|e| format!("显示器 {n} 不支持桌面复制: {e}"))?;
            let slots = (0..POOL_SIZE).map(|_| create_slot(&device, &compose, (part.tile.width, part.tile.height)).map(Arc::new)).collect::<Result<Vec<_>, _>>()?;
            // SAFETY: 设备由 SharedDevice 持有，复制接口随采集器存续。
            let duplication = unsafe { output.DuplicateOutput(device.device()) }.map_err(|e| format!("显示器 {n} DuplicateOutput 失败: {e}"))?;
            let (left, top) = (part.crop.x as u32, part.crop.y as u32);
            outputs.push(SpanOutput {
                output,
                duplication,
                slots,
                crop: D3D11_BOX { left, top, front: 0, right: left + part.crop.width, bottom: top + part.crop.height, back: 1 },
                rect: part.tile,
                current: None,
            });
        }
        let context4: ID3D11DeviceContext4 = device.context().cast().map_err(|e| format!("上下文不支持栅栏: {e}"))?;
        let capture = Self {
            device,
            context4,
            outputs,
            next_output: 0,
            lost: None,
            region,
            sampler: CursorSampler::new().map_err(|e| format!("光标采样器不可用: {e}"))?,
            projector: CursorProjector::new(),
            retained_shape: None,
            latest: None,
            anchor,
            stats: CaptureStats::default(),
            acquire_gap_ms: Vec::new(),
            copy_ms: Vec::new(),
            last_acquire: None,
            trace: TraceBuf::new(Thread::Capture),
        };
        eprintln!("跨屏选区：{} 块显示器，空洞{}", capture.outputs.len(), if plan.has_hole { "（黑色填充）" } else { "无" });
        Ok(Some((capture, compose)))
    }

    /// 显示器块数（合成时每块一层）。
    pub fn tile_count(&self) -> usize {
        self.outputs.len()
    }

    /// 采样并投影当前光标（逻辑同单屏采集器；失败返回 `None`）。
    fn sample_cursor(&mut self) -> Option<AttachedCursorSample> {
        let snapshot = self.sampler.sample().ok()?;
        if let Some(shape) = snapshot.shape.shape() {
            self.retained_shape = Some(shape.clone());
        }
        let target = CursorTargetInfo { origin_x: self.region.0, origin_y: self.region.1, width: self.region.2, height: self.region.3 };
        let mut cursor = self.projector.project(&target, snapshot);
        if let Some(shape) = self.retained_shape.as_ref().filter(|s| cursor.shape.shape_id() == Some(s.shape_id)) {
            cursor.shape = CursorShapeState::Embedded(shape.clone());
        }
        Some(cursor)
    }

    /// 把设备 A 上已记录的命令提交给 GPU。
    fn flush(&self) {
        let _lock = self.device.lock();
        // SAFETY: 持有设备锁；Flush 只提交已记录的命令。
        unsafe { self.device.context().Flush() };
    }

    /// 处理第 `index` 路输出刚取到的一次更新：复制进该路的空闲槽，并和其余各路的最近一块组成一帧。
    fn on_acquired(&mut self, index: usize, info: DXGI_OUTDUPL_FRAME_INFO, resource: Option<IDXGIResource>, want: bool) -> Result<Option<Captured<GpuFrame>>, CaptureFault> {
        let captured_at = Instant::now();
        let duplication = self.outputs[index].duplication.clone();
        let guard = FrameGuard(&duplication);
        let src = u8::try_from(index).unwrap_or(u8::MAX);
        if !want {
            self.trace.acquire(src, captured_at, None, info.AccumulatedFrames, 0, code::ACQ_UNWANTED);
            return Ok(None);
        }
        if info.LastPresentTime == 0 {
            drop(guard);
            self.trace.acquire(src, captured_at, None, info.AccumulatedFrames, 0, code::ACQ_CURSOR_ONLY);
            let cursor = self.sample_cursor();
            // 只有光标移动：沿用上一张桌面图
            return Ok(self.latest.as_ref().map(|f| Captured { cursor, captured_at, fresh: false, ..f.clone() }));
        }
        self.stats.coalesced += u64::from(info.AccumulatedFrames.saturating_sub(1));
        let Some(resource) = resource else { return Ok(None) };
        let source: ID3D11Texture2D = resource.cast().map_err(|e| CaptureFault::Other(format!("桌面资源不是纹理: {e}")))?;
        let mut desc = D3D11_TEXTURE2D_DESC::default();
        // SAFETY: desc 是局部变量。
        unsafe { source.GetDesc(&mut desc) };
        if desc.Format != DXGI_FORMAT_B8G8R8A8_UNORM {
            return Err(CaptureFault::Other(format!("显示器 {index} 桌面格式 {:?} 不是 BGRA8（HDR 走软件路径）", desc.Format)));
        }
        let copy_started = Instant::now();
        let Some(slot) = self.outputs[index].slots.iter().find(|s| Arc::strong_count(s) == 1).cloned() else {
            self.stats.pool_drops += 1;
            if self.trace.enabled() {
                let present = self.anchor.map_or(captured_at, |a| a.to_instant(info.LastPresentTime).min(captured_at));
                self.trace.acquire(src, captured_at, Some(present), info.AccumulatedFrames, 0, code::ACQ_POOL_DROP);
            }
            return Ok(None);
        };
        copy_into_slot(&self.device, &self.context4, &slot, &source, &self.outputs[index].crop)?;
        if self.copy_ms.len() < DIAG_LIMIT {
            self.copy_ms.push(copy_started.elapsed().as_secs_f32() * 1000.0);
        }
        // 复制一记录完就立刻释放桌面帧，光标采样与 Flush 都放到释放之后
        drop(guard);
        self.flush();
        self.outputs[index].current = Some(slot);
        let tiles: Arc<[Tile]> = self.outputs.iter().filter_map(|o| o.current.as_ref().map(|slot| Tile { slot: Arc::clone(slot), rect: o.rect })).collect();
        let cursor = self.sample_cursor();
        let present = match self.anchor {
            Some(a) => a.to_instant(info.LastPresentTime),
            None => captured_at,
        };
        let frame = GpuFrame { slot: Arc::clone(&tiles[0].slot), tiles: Some(tiles) };
        let captured = Captured { frame, cursor, present: present.min(captured_at), captured_at, fresh: true, id: self.stats.frames + 1 };
        self.trace.acquire(src, captured_at, Some(captured.present), info.AccumulatedFrames, captured.id, code::ACQ_FRESH);
        self.stats.frames += 1;
        self.latest = Some(captured.clone());
        Ok(Some(captured))
    }
}

impl CaptureSource for SpanCapture {
    type Frame = GpuFrame;

    /// 零超时轮流取各路输出，任一路有更新就出帧；全部无更新则亚毫秒睡眠后重试，直到超时。
    fn next(&mut self, timeout: Duration, want: bool) -> Result<Option<Captured<GpuFrame>>, CaptureFault> {
        let deadline = Instant::now() + timeout;
        let count = self.outputs.len();
        loop {
            let now = Instant::now();
            if let Some(prev) = self.last_acquire.replace(now)
                && self.acquire_gap_ms.len() < DIAG_LIMIT
            {
                self.acquire_gap_ms.push(now.saturating_duration_since(prev).as_secs_f32() * 1000.0);
            }
            for step in 0..count {
                let index = (self.next_output + step) % count;
                let mut info = DXGI_OUTDUPL_FRAME_INFO::default();
                let mut resource: Option<IDXGIResource> = None;
                // SAFETY: 输出指针指向局部变量。
                let call = unsafe { self.outputs[index].duplication.AcquireNextFrame(0, &mut info, &mut resource) };
                match call {
                    Ok(()) => {
                        self.next_output = (index + 1) % count;
                        return self.on_acquired(index, info, resource, want);
                    }
                    Err(e) if e.code() == DXGI_ERROR_WAIT_TIMEOUT => {}
                    Err(e) if e.code() == DXGI_ERROR_ACCESS_LOST => {
                        self.lost = Some(index);
                        self.trace.lost(u8::try_from(index).unwrap_or(u8::MAX));
                        return Err(CaptureFault::Lost);
                    }
                    Err(e) => return Err(CaptureFault::Other(format!("显示器 {index} AcquireNextFrame 失败: {e}"))),
                }
            }
            if Instant::now() >= deadline {
                self.trace.idle(u8::MAX, timeout);
                return Ok(None);
            }
            std::thread::sleep(POLL_SLEEP);
        }
    }

    /// 只重建权限丢失的那一路，其余保持。
    fn recreate(&mut self) -> Result<(), String> {
        let Some(index) = self.lost.take() else { return Ok(()) };
        let unit = &mut self.outputs[index];
        // SAFETY: 设备与输出在采集器存续期间有效。
        let result = unsafe { unit.output.DuplicateOutput(self.device.device()) };
        match result {
            Ok(duplication) => {
                unit.duplication = duplication;
                unit.current = None;
                self.latest = None;
                Ok(())
            }
            Err(e) => {
                self.lost = Some(index);
                Err(format!("重建显示器 {index} 桌面复制失败: {e}"))
            }
        }
    }

    /// 当前计数。
    fn stats(&self) -> CaptureStats {
        self.stats
    }

    /// 取走诊断样本。
    fn take_diag(&mut self) -> CaptureDiag {
        CaptureDiag {
            samples: vec![
                ("采集取帧间隔ms", std::mem::take(&mut self.acquire_gap_ms)),
                ("采集复制ms", std::mem::take(&mut self.copy_ms)),
            ],
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 构造矩形。
    fn r(x: i32, y: i32, w: u32, h: u32) -> Rect {
        Rect { x, y, width: w, height: h }
    }

    /// 构造输出。
    fn out(adapter: usize, rect: Rect, rotated: bool) -> OutputInfo {
        OutputInfo { adapter, adapter_name: format!("GPU{adapter}"), rect, rotated }
    }

    /// 两块并排的 2560x1440 屏（同适配器）。
    fn dual() -> Vec<OutputInfo> {
        vec![out(0, r(0, 0, 2560, 1440), false), out(0, r(2560, 0, 2560, 1440), false)]
    }

    /// 选区跨接缝：按显示器切成两块，裁剪框是输出本地坐标，块偏移是选区坐标。
    #[test]
    fn splits_region_across_two_monitors() {
        let plan = plan_span(r(1280, 0, 2560, 1440), &dual()).unwrap().unwrap();
        assert_eq!(plan.parts.len(), 2);
        assert_eq!((plan.parts[0].crop, plan.parts[0].tile), (r(1280, 0, 1280, 1440), r(0, 0, 1280, 1440)));
        assert_eq!((plan.parts[1].crop, plan.parts[1].tile), (r(0, 0, 1280, 1440), r(1280, 0, 1280, 1440)));
        assert!(!plan.has_hole);
        assert_eq!(plan.adapter, 0);
    }

    /// 副屏在主屏左侧（负坐标）也能切分。
    #[test]
    fn splits_with_negative_origin() {
        let outputs = vec![out(0, r(-1920, 0, 1920, 1080), false), out(0, r(0, 0, 2560, 1440), false)];
        let plan = plan_span(r(-100, 10, 300, 200), &outputs).unwrap().unwrap();
        assert_eq!((plan.parts[0].crop, plan.parts[0].tile), (r(1820, 10, 100, 200), r(0, 0, 100, 200)));
        assert_eq!((plan.parts[1].crop, plan.parts[1].tile), (r(0, 10, 200, 200), r(100, 0, 200, 200)));
    }

    /// 单屏选区（被某个输出完整包含）或只和一块相交，都不是跨屏。
    #[test]
    fn single_monitor_is_not_span() {
        assert_eq!(plan_span(r(100, 100, 800, 600), &dual()).unwrap(), None);
        assert_eq!(plan_span(r(2560, 0, 2560, 1440), &dual()).unwrap(), None);
        assert_eq!(plan_span(r(2400, 1400, 100, 500), &dual()).unwrap(), None);
    }

    /// 两屏不齐：选区伸出较矮屏的下缘，判为含空洞但仍可走硬件。
    #[test]
    fn detects_hole_when_monitors_misaligned() {
        let outputs = vec![out(0, r(0, 0, 2560, 1440), false), out(0, r(2560, 0, 1920, 1080), false)];
        let plan = plan_span(r(2000, 0, 1000, 1440), &outputs).unwrap().unwrap();
        assert!(plan.has_hole);
        assert_eq!(plan.parts[1].tile, r(560, 0, 440, 1080));
        let aligned = plan_span(r(2000, 0, 1000, 1080), &outputs).unwrap().unwrap();
        assert!(!aligned.has_hole);
    }

    /// 镜像显示（两输出矩形相同）只取一块。
    #[test]
    fn mirrored_outputs_counted_once() {
        let outputs = vec![out(0, r(0, 0, 1920, 1080), false), out(0, r(0, 0, 1920, 1080), false), out(0, r(1920, 0, 1920, 1080), false)];
        let plan = plan_span(r(1000, 0, 1800, 1080), &outputs).unwrap().unwrap();
        assert_eq!(plan.parts.iter().map(|p| p.output).collect::<Vec<_>>(), vec![0, 2]);
    }

    /// 相关输出分属不同适配器：回落并给出带适配器名的原因。
    #[test]
    fn different_adapters_fall_back_with_reason() {
        let outputs = vec![out(0, r(0, 0, 2560, 1440), false), out(1, r(2560, 0, 2560, 1440), false)];
        let error = plan_span(r(1280, 0, 2560, 1440), &outputs).unwrap_err();
        assert_eq!(error, "跨适配器选区暂不支持硬件路径（适配器: GPU0 / GPU1）");
    }

    /// 不相关的输出在别的适配器上不影响判定（只看参与拼接的输出）。
    #[test]
    fn unrelated_adapter_is_ignored() {
        let mut outputs = dual();
        outputs.push(out(1, r(5120, 0, 1920, 1080), false));
        assert!(plan_span(r(1280, 0, 2560, 1440), &outputs).unwrap().is_some());
    }

    /// 参与拼接的输出中有旋转屏：整体回落；不参与的旋转屏不影响。
    #[test]
    fn rotated_participant_falls_back() {
        let mut outputs = dual();
        outputs[1].rotated = true;
        let error = plan_span(r(1280, 0, 2560, 1440), &outputs).unwrap_err();
        assert!(error.contains("旋转"), "{error}");
        assert!(plan_span(r(100, 0, 800, 600), &outputs).unwrap().is_none());
    }

    /// 目标矩形：同尺寸 1:1 带偏移；缩放时相邻块共用边界，无缝无叠。
    #[test]
    fn tile_destinations_share_edges() {
        assert_eq!(tile_destination(r(1280, 0, 1280, 1440), (2560, 1440), (2560, 1440)), r(1280, 0, 1280, 1440));
        // 5120x1440 缩到 1920x540，接缝 x=2000 不落在整数倍上
        let (src, out) = ((5120, 1440), (1920, 540));
        let left = tile_destination(r(0, 0, 2000, 1440), src, out);
        let right = tile_destination(r(2000, 0, 3120, 1440), src, out);
        assert_eq!(left.x as u32 + left.width, right.x as u32);
        assert_eq!(right.x as u32 + right.width, out.0);
        assert_eq!(left.x, 0);
    }

    /// 输入流检查：每块一层加光标一层；不够时给出回落文案。
    #[test]
    fn stream_check_reserves_cursor_layer() {
        assert!(check_streams(2, 3).is_ok());
        assert_eq!(check_streams(3, 3).unwrap_err(), "视频处理器输入流不足以拼接 3 块显示器");
    }
}
