//! Windows 硬件实现：DXGI 桌面复制、D3D11 VideoProcessor 多图层合成、硬件 H.264 编码。
//!
//! 三个阶段各自实现 `pipeline` 里的 trait，由 [`assemble`] 在会话初始化时一次装配好。

pub mod aacsink;
pub mod assemble;
pub mod compose;
pub mod dda;
pub mod hwenc;
pub mod mfenc;
pub mod span;
pub mod wgc;
pub mod vp;
#[cfg(test)]
mod synthetic;
