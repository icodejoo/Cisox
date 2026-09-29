//! 显示器模型与查询（纯逻辑；枚举由平台后端提供）。
//!
//! 坐标约定见 [`crate::geometry`]：屏幕坐标为虚拟桌面物理像素。

use crate::error::ShellError;
use crate::geometry::{PhysicalPoint, PhysicalRect, ScaleFactor};

/// 显示器标识（Windows 下为 `HMONITOR` 数值，与 GPUI 的 `DisplayId` 同源）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct MonitorId(pub u64);

/// 显示器选择方式。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum MonitorTarget {
    /// 主显示器。
    #[default]
    Primary,
    /// 指定显示器。
    Id(MonitorId),
}

/// 单个显示器的几何与 DPI 信息。
#[derive(Debug, Clone, PartialEq)]
pub struct MonitorInfo {
    /// 显示器标识。
    pub id: MonitorId,
    /// 设备名（如 `\\.\DISPLAY1`），仅用于日志与诊断。
    pub name: String,
    /// 整屏范围（屏幕坐标，物理像素）。
    pub bounds: PhysicalRect,
    /// 工作区（去掉任务栏，屏幕坐标，物理像素）。
    pub work_area: PhysicalRect,
    /// 该显示器的 DPI 缩放比。
    pub scale: ScaleFactor,
    /// 是否主显示器。
    pub is_primary: bool,
}

/// 显示器集合（一次枚举的快照，热插拔后需重新枚举）。
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Monitors {
    /// 全部显示器。
    list: Vec<MonitorInfo>,
}

impl Monitors {
    /// 由列表构造。
    ///
    /// ```rust
    /// use snow_ui_shell::monitor::Monitors;
    /// assert!(Monitors::from_list(vec![]).primary().is_none());
    /// ```
    pub fn from_list(list: Vec<MonitorInfo>) -> Self {
        Self { list }
    }

    /// 枚举当前系统显示器；不支持的平台返回错误而非 panic。
    ///
    /// # 返回
    /// 显示器快照，或 [`ShellError`]。
    ///
    /// ```no_run
    /// let monitors = snow_ui_shell::monitor::Monitors::enumerate().unwrap();
    /// println!("{} 块显示器", monitors.all().len());
    /// ```
    pub fn enumerate() -> Result<Self, ShellError> {
        crate::native::enumerate_monitors().map(Self::from_list)
    }

    /// 全部显示器。
    pub fn all(&self) -> &[MonitorInfo] {
        &self.list
    }

    /// 主显示器；没有标记时退回第一块。
    pub fn primary(&self) -> Option<&MonitorInfo> {
        self.list
            .iter()
            .find(|m| m.is_primary)
            .or(self.list.first())
    }

    /// 按标识查找。
    pub fn by_id(&self, id: MonitorId) -> Option<&MonitorInfo> {
        self.list.iter().find(|m| m.id == id)
    }

    /// 按选择方式解析为具体显示器。
    ///
    /// # 返回
    /// 显示器信息；找不到返回 [`ShellError::InvalidArgument`]。
    pub fn resolve(&self, target: MonitorTarget) -> Result<&MonitorInfo, ShellError> {
        match target {
            MonitorTarget::Primary => self.primary(),
            MonitorTarget::Id(id) => self.by_id(id),
        }
        .ok_or_else(|| ShellError::InvalidArgument(format!("找不到显示器: {target:?}")))
    }

    /// 包含某个屏幕坐标点的显示器。
    pub fn at_point(&self, p: PhysicalPoint) -> Option<&MonitorInfo> {
        self.list.iter().find(|m| m.bounds.contains(p))
    }

    /// 与矩形重叠面积最大的显示器；无重叠时取最近（按中心点距离）的显示器。
    ///
    /// ```rust
    /// use snow_ui_shell::geometry::{PhysicalRect, ScaleFactor};
    /// use snow_ui_shell::monitor::{MonitorId, MonitorInfo, Monitors};
    /// let m = |id, x| MonitorInfo {
    ///     id: MonitorId(id), name: String::new(),
    ///     bounds: PhysicalRect::new(x, 0, 100, 100), work_area: PhysicalRect::new(x, 0, 100, 100),
    ///     scale: ScaleFactor::ONE, is_primary: id == 1,
    /// };
    /// let ms = Monitors::from_list(vec![m(1, 0), m(2, 100)]);
    /// let hit = ms.best_for_rect(PhysicalRect::new(90, 0, 40, 10)).unwrap();
    /// assert_eq!(hit.id, MonitorId(2));
    /// ```
    pub fn best_for_rect(&self, rect: PhysicalRect) -> Option<&MonitorInfo> {
        let overlap = |m: &MonitorInfo| {
            m.bounds
                .intersect(&rect)
                .map_or(0i64, |i| i.width as i64 * i.height as i64)
        };
        let best = self.list.iter().max_by_key(|m| overlap(m))?;
        if overlap(best) > 0 {
            return Some(best);
        }
        let c = rect.center();
        self.list.iter().min_by_key(|m| {
            let mc = m.bounds.center();
            let (dx, dy) = ((mc.x - c.x) as i64, (mc.y - c.y) as i64);
            dx * dx + dy * dy
        })
    }

    /// 虚拟桌面外接矩形（所有显示器的并集外框）。
    pub fn virtual_bounds(&self) -> Option<PhysicalRect> {
        let mut it = self.list.iter();
        let first = it.next()?.bounds;
        Some(it.fold(first, |acc, m| acc.union_bounds(&m.bounds)))
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// 构造测试用显示器。
    pub(crate) fn mon(id: u64, bounds: PhysicalRect, scale: f32, primary: bool) -> MonitorInfo {
        MonitorInfo {
            id: MonitorId(id),
            name: format!("M{id}"),
            bounds,
            work_area: PhysicalRect::new(bounds.x, bounds.y, bounds.width, bounds.height - 40),
            scale: ScaleFactor::new(scale),
            is_primary: primary,
        }
    }

    /// 两块显示器：主屏 150%，副屏在左侧 100%。
    pub(crate) fn two_monitors() -> Monitors {
        Monitors::from_list(vec![
            mon(1, PhysicalRect::new(0, 0, 2880, 1620), 1.5, true),
            mon(2, PhysicalRect::new(-1920, 100, 1920, 1080), 1.0, false),
        ])
    }

    /// 主屏解析、按 id 查找、找不到时报错。
    #[test]
    fn resolve_targets() {
        let ms = two_monitors();
        assert_eq!(ms.resolve(MonitorTarget::Primary).unwrap().id, MonitorId(1));
        assert_eq!(
            ms.resolve(MonitorTarget::Id(MonitorId(2))).unwrap().id,
            MonitorId(2)
        );
        assert!(ms.resolve(MonitorTarget::Id(MonitorId(9))).is_err());
        assert!(Monitors::default().resolve(MonitorTarget::Primary).is_err());
    }

    /// 点与矩形归属显示器，含负坐标副屏。
    #[test]
    fn locate_monitor() {
        let ms = two_monitors();
        assert_eq!(
            ms.at_point(PhysicalPoint::new(-10, 200)).unwrap().id,
            MonitorId(2)
        );
        assert_eq!(
            ms.at_point(PhysicalPoint::new(10, 10)).unwrap().id,
            MonitorId(1)
        );
        assert!(ms.at_point(PhysicalPoint::new(-10, 0)).is_none());
        // 跨屏矩形取重叠更多者
        let r = PhysicalRect::new(-100, 200, 400, 100);
        assert_eq!(ms.best_for_rect(r).unwrap().id, MonitorId(1));
        // 完全在屏外时取最近者
        let far = PhysicalRect::new(-5000, 300, 100, 100);
        assert_eq!(ms.best_for_rect(far).unwrap().id, MonitorId(2));
    }

    /// 虚拟桌面外框。
    #[test]
    fn virtual_bounds_union() {
        let ms = two_monitors();
        assert_eq!(
            ms.virtual_bounds(),
            Some(PhysicalRect::new(-1920, 0, 1920 + 2880, 1620))
        );
        assert_eq!(Monitors::default().virtual_bounds(), None);
    }
}
