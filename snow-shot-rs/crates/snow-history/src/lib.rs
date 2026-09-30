//! 截图历史与贴图仓储的容器层（文件 I/O、路径校验、体积记账、两阶段删除）。
//!
//! 所属阶段：P3。序列化内容（`canvas_history.json`、`canvas_session.bin`）一律按不透明字节处理。

pub mod capture_history;
pub mod fsutil;
pub mod index;
pub mod pin_id;
pub mod pinned;
pub mod timeutil;
