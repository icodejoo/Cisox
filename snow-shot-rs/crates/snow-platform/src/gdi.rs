//! GDI 句柄的 RAII 守卫（仅 Windows 内部使用）：屏幕 DC、内存 DC、GDI 对象各自配对释放。

use windows::Win32::Graphics::Gdi::{
    CreateCompatibleDC, DeleteDC, DeleteObject, GetDC, HDC, HGDIOBJ, ReleaseDC,
};

/// 屏幕 DC（`GetDC(None)`），Drop 时 `ReleaseDC`。
pub(crate) struct ScreenDc(HDC);

impl ScreenDc {
    /// 获取整个屏幕的 DC。
    ///
    /// # 返回
    /// 守卫；系统拒绝（无桌面会话等）返回 `None`。
    pub(crate) fn acquire() -> Option<Self> {
        // SAFETY: 无前置条件；返回的 DC 由本守卫在 Drop 时释放。
        let dc = unsafe { GetDC(None) };
        (!dc.0.is_null()).then_some(Self(dc))
    }

    /// 底层 DC 句柄（生命周期不得超过守卫）。
    pub(crate) fn hdc(&self) -> HDC {
        self.0
    }
}

impl Drop for ScreenDc {
    /// 释放屏幕 DC。
    fn drop(&mut self) {
        // SAFETY: DC 由 `GetDC(None)` 取得，只释放一次。
        let _ = unsafe { ReleaseDC(None, self.0) };
    }
}

/// 与屏幕兼容的内存 DC，Drop 时 `DeleteDC`。
pub(crate) struct MemDc(HDC);

impl MemDc {
    /// 创建与 `screen` 兼容的内存 DC。
    ///
    /// # 返回
    /// 守卫；创建失败返回 `None`。
    pub(crate) fn compatible_with(screen: &ScreenDc) -> Option<Self> {
        // SAFETY: `screen` 持有有效的屏幕 DC。
        let dc = unsafe { CreateCompatibleDC(Some(screen.hdc())) };
        (!dc.0.is_null()).then_some(Self(dc))
    }

    /// 底层 DC 句柄（生命周期不得超过守卫）。
    pub(crate) fn hdc(&self) -> HDC {
        self.0
    }
}

impl Drop for MemDc {
    /// 删除内存 DC。
    fn drop(&mut self) {
        // SAFETY: DC 由 `CreateCompatibleDC` 创建，只删除一次；调用方须先把选入的对象换出。
        let _ = unsafe { DeleteDC(self.0) };
    }
}

/// 位图 / 字体等 GDI 对象，Drop 时 `DeleteObject`（调用方须先把它从 DC 里换出）。
pub(crate) struct GdiObject(HGDIOBJ);

impl GdiObject {
    /// 接管一个 GDI 对象句柄。
    ///
    /// # 参数
    /// - `handle`：`HBITMAP` / `HFONT` 等可转成 `HGDIOBJ` 的句柄。
    ///
    /// # 返回
    /// 守卫；句柄为空（创建失败）返回 `None`。
    pub(crate) fn new(handle: impl Into<HGDIOBJ>) -> Option<Self> {
        let handle = handle.into();
        (!handle.0.is_null()).then_some(Self(handle))
    }

    /// 底层对象句柄（生命周期不得超过守卫）。
    pub(crate) fn handle(&self) -> HGDIOBJ {
        self.0
    }
}

impl Drop for GdiObject {
    /// 删除 GDI 对象。
    fn drop(&mut self) {
        // SAFETY: 句柄由创建函数返回且只删除一次。
        let _ = unsafe { DeleteObject(self.0) };
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use windows::Win32::Graphics::Gdi::CreateCompatibleBitmap;

    /// 守卫链路（屏幕 DC -> 内存 DC -> 位图）可创建并释放；无桌面会话时跳过。
    #[test]
    fn guards_create_and_release() {
        let Some(screen) = ScreenDc::acquire() else {
            return;
        };
        let mem = MemDc::compatible_with(&screen).expect("内存 DC");
        // SAFETY: 屏幕 DC 有效，尺寸为正。
        let bitmap = unsafe { CreateCompatibleBitmap(screen.hdc(), 4, 4) };
        let object = GdiObject::new(bitmap).expect("位图");
        assert!(!object.handle().0.is_null());
        assert!(!mem.hdc().0.is_null());
    }

    /// 空句柄不会被接管。
    #[test]
    fn null_object_is_rejected() {
        assert!(GdiObject::new(HGDIOBJ::default()).is_none());
    }
}
