//! 数据根目录解析：实现已抽到零依赖的 `snow-data-paths`（托盘宿主也要用，见
//! `docs/design/tray-host-split.md` §3），这里原样重导出，`snow_config::paths::*` 路径不变。

pub use snow_data_paths::paths::*;
