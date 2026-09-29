# p0-v1-overlay-window

This is a spike to test creating a pure GPUI overlay window for multi-monitor desktop applications.

## Goals (6-point validation)
1. **GPUI 官方原生建窗 (gpui-pre):** ✅ 验证通过。使用 `cx.open_window` 与 `gpui_platform` 能够稳定建窗。
2. **窗口全局透明 (WindowBackgroundAppearance::Transparent):** ✅ 验证通过。源码中 GPUI 在 Windows 上通过 `set_window_composition_attribute` (ACCENT_ENABLE_TRANSPARENTGRADIENT = 2) 实现了真正的 DWM 级别透明。
3. **无边框顶层 (WindowKind::PopUp, titlebar: None):** ✅ 验证通过。正确创建为无边框窗口。
4. **点击穿透 (WM_NCHITTEST / HTTRANSPARENT):** ✅ 验证通过。无需抛弃 GPUI 转向纯 Win32 建窗，我们通过获取 `raw_window_handle`，使用 `SetWindowLongPtrW` 注入自定义的 `subclass_proc` 成功拦截了 `WM_NCHITTEST`，对于不需要响应点击的区域返回 `HTTRANSPARENT`，实现了精准的局部点击穿透。
5. **多显示器与混合 DPI 跨屏铺满:** ✅ 验证通过。`cx.displays()` 能够正确获取所有屏幕（如主副屏），并针对每个 Display bounds 单独建窗铺满。
6. **在透明层上渲染带色块区域且响应该区域的点击:** ✅ 验证通过。`subclass.rs` 中预留了坐标区域判断（如 100~500 范围），在范围内的由 GPUI 正常处理（拦截跳过穿透），可以渲染并响应点击。

## Technical Findings
- **混合方案的必要性**：起初误以为需要完全手写 Win32 建窗并嵌入 GPUI 作为子视图。验证发现 **完全不需要**。直接用 GPUI 建窗，并通过 `window.window_handle()` 获取 HWND 进行 Subclassing，即可在不破坏 GPUI 事件循环的情况下实现高级 Win32 特性（如局部点击穿透）。
- **`windows` Crate 破坏性更新**：新版本 `windows` crate 将 `HWND` 内部的 `isize` 替换为了 `*mut c_void`，这导致哈希表和原生 API 强转时会出现类型不匹配。在 `subclass.rs` 中需显式通过 `hwnd.0 as isize` 和 `as *const ()` 等进行指针转换处理。
- **PowerShell 环境陷阱**：在自动化脚本中，部分 `$PROFILE` 配置会强制修改工作目录（如自动 `Set-Location`），导致后续路径执行失败，必须严格使用绝对路径或在同一命令内显式切入目录。

## Usage
Run the overlay test:
```ps1
cargo run --bin p0-v1-overlay-window
```
