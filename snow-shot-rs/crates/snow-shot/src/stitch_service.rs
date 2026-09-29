//! 滚动截长图图像拼接服务（Scrolling Screenshot Stitch Service）。
//!
//! 接收滚动过程中截取的连续切片帧，通过行匹配算法估算垂直位移与重叠区域，
//! 实时合成扩展超长垂直位图，并输出拼接完成的完整长截图。

use snow_platform::capture::CapturedScreen;

/// 滚动拼接方向。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum StitchDirection {
    /// 自上向下滚动（默认）。
    #[default]
    TopToBottom,
    /// 自下向上滚动。
    BottomToTop,
}

/// 滚动截长图状态机与像素合成器。
pub struct StitchService {
    /// 目标图像宽度（以首帧为准）。
    pub width: u32,
    /// 当前已累积拼接的图像高度。
    pub height: u32,
    /// 当前拼接合并后的连续 RGBA 像素数据。
    pub rgba: Vec<u8>,
    /// 已接收切片帧数。
    pub frame_count: usize,
    /// 滚动方向。
    pub direction: StitchDirection,
    /// 允许拼接的最大总高度（防止内存溢出，默认 32,768 像素）。
    pub max_height: u32,
}

impl StitchService {
    /// 构造新的长图拼接服务。
    ///
    /// # 参数
    /// - `direction`: 滚动拼接方向。
    ///
    /// # 示例
    /// ```rust
    /// use snow_shot::stitch_service::{StitchService, StitchDirection};
    /// let svc = StitchService::new(StitchDirection::TopToBottom);
    /// assert_eq!(svc.frame_count, 0);
    /// assert_eq!(svc.height, 0);
    /// ```
    pub fn new(direction: StitchDirection) -> Self {
        Self {
            width: 0,
            height: 0,
            rgba: Vec::new(),
            frame_count: 0,
            direction,
            max_height: 32768,
        }
    }

    /// 追加一帧新的切片图像并执行垂直重叠拼接。
    ///
    /// # 参数
    /// - `frame_w`: 切片宽度。
    /// - `frame_h`: 切片高度。
    /// - `frame_rgba`: 切片 RGBA 像素数据。
    ///
    /// # 返回
    /// 拼接成功返回当前累计的总高度；若尺寸不符或超出上限则返回错误。
    pub fn append_slice(
        &mut self,
        frame_w: u32,
        frame_h: u32,
        frame_rgba: &[u8],
    ) -> Result<u32, String> {
        if frame_w == 0 || frame_h == 0 || frame_rgba.len() < (frame_w * frame_h * 4) as usize {
            return Err("切片图像尺寸非法或数据不完整".to_string());
        }

        // 首帧初始化画布
        if self.frame_count == 0 {
            self.width = frame_w;
            self.height = frame_h;
            self.rgba = frame_rgba[..(frame_w * frame_h * 4) as usize].to_vec();
            self.frame_count = 1;
            return Ok(self.height);
        }

        if frame_w != self.width {
            return Err(format!(
                "切片宽度不匹配: 期望 {} 实际 {}",
                self.width, frame_w
            ));
        }

        // 计算当前画布底部与新切片顶部的重叠高度
        let overlap = self.estimate_vertical_overlap(frame_w, frame_h, frame_rgba);
        let non_overlap_h = frame_h.saturating_sub(overlap);

        if non_overlap_h == 0 {
            // 完全重叠，忽略冗余帧
            return Ok(self.height);
        }

        if self.height + non_overlap_h > self.max_height {
            return Err(format!("拼接总高度超出上限: {}", self.max_height));
        }

        // 追加新增行
        let start_byte = (overlap * frame_w * 4) as usize;
        let end_byte = (frame_h * frame_w * 4) as usize;
        self.rgba.extend_from_slice(&frame_rgba[start_byte..end_byte]);
        self.height += non_overlap_h;
        self.frame_count += 1;

        Ok(self.height)
    }

    /// 估算当前画布底端与新切片顶端的重叠行数。
    fn estimate_vertical_overlap(&self, frame_w: u32, frame_h: u32, frame_rgba: &[u8]) -> u32 {
        let max_search_h = (self.height.min(frame_h) / 2).clamp(10, 100);
        let stride = (frame_w * 4) as usize;

        // 寻找像素差最小的重叠行
        let mut best_overlap = 0;
        let mut min_diff = u64::MAX;

        for test_overlap in (10..=max_search_h).step_by(2) {
            let mut diff = 0u64;
            let canvas_start = (self.height - test_overlap) as usize * stride;
            let sample_rows = test_overlap.min(10) as usize;

            for r in 0..sample_rows {
                let c_row = canvas_start + r * stride;
                let f_row = r * stride;
                for c in (0..stride).step_by(16) {
                    let d = (self.rgba[c_row + c] as i32 - frame_rgba[f_row + c] as i32).unsigned_abs() as u64;
                    diff += d;
                }
            }

            if diff < min_diff {
                min_diff = diff;
                best_overlap = test_overlap;
            }
        }

        if min_diff < 500 {
            best_overlap
        } else {
            0
        }
    }

    /// 完成拼接并导出完整的截屏图像对象。
    pub fn finish(&self) -> Result<CapturedScreen, String> {
        if self.frame_count == 0 || self.height == 0 || self.width == 0 {
            return Err("拼接尚未包含任何有效帧".to_string());
        }

        Ok(CapturedScreen {
            width: self.width,
            height: self.height,
            data: self.to_bgra(),
        })
    }

    /// 将 RGBA 转换为 CapturedScreen 使用的 BGRA 格式。
    fn to_bgra(&self) -> Vec<u8> {
        let mut bgra = self.rgba.clone();
        for chunk in bgra.chunks_exact_mut(4) {
            chunk.swap(0, 2);
        }
        bgra
    }

    /// 取消并重置拼接器状态。
    pub fn reset(&mut self) {
        self.width = 0;
        self.height = 0;
        self.rgba.clear();
        self.frame_count = 0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 验证首帧初始化与多帧垂直拼接。
    #[test]
    fn test_stitch_service_append() {
        let mut svc = StitchService::new(StitchDirection::TopToBottom);

        // 第 1 帧: 100 x 50
        let frame1 = vec![255; 100 * 50 * 4];
        let h1 = svc.append_slice(100, 50, &frame1).expect("Append frame 1 failed");
        assert_eq!(h1, 50);
        assert_eq!(svc.frame_count, 1);

        // 第 2 帧: 100 x 50
        let frame2 = vec![128; 100 * 50 * 4];
        let h2 = svc.append_slice(100, 50, &frame2).expect("Append frame 2 failed");
        assert!(h2 >= 50);
        assert_eq!(svc.frame_count, 2);

        let finished = svc.finish().expect("Finish failed");
        assert_eq!(finished.width, 100);
        assert_eq!(finished.height, h2);
    }

    /// 验证宽度不匹配时报错。
    #[test]
    fn test_stitch_service_mismatched_width() {
        let mut svc = StitchService::new(StitchDirection::TopToBottom);
        let _ = svc.append_slice(100, 50, &vec![0; 100 * 50 * 4]);
        let err = svc.append_slice(120, 50, &vec![0; 120 * 50 * 4]).unwrap_err();
        assert!(err.contains("切片宽度不匹配"));
    }
}
