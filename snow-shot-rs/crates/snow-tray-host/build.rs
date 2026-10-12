//! 构建脚本：目标为 Windows 时，把 app.ico 与版本信息嵌入宿主 exe 资源（与 UI 进程用同一枚图标）。

/// 与 UI 进程共用的图标（仓库内相对路径）。
const ICON_PATH: &str = "../snow-shot/assets/app.ico";

/// 入口：非 Windows 目标直接通过。
fn main() {
    println!("cargo:rerun-if-changed={ICON_PATH}");
    println!("cargo:rerun-if-changed=build.rs");
    #[cfg(windows)]
    embed_windows_resources();
}

/// 嵌入图标；产品名取 PRODUCT_NAME 同值。
#[cfg(windows)]
fn embed_windows_resources() {
    // build.rs 里的 cfg(windows) 指宿主；目标不是 Windows 时跳过
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }
    let mut res = winresource::WindowsResource::new();
    res.set_icon(ICON_PATH);
    // 与 snow_data_paths::PRODUCT_NAME 保持同值（构建脚本不能依赖本工作区 crate）
    res.set("ProductName", "Cisox");
    res.set("FileDescription", "Cisox tray host");
    res.compile()
        .expect("嵌入 Windows 资源失败（需要 MSVC 环境里的 rc.exe）");
}
