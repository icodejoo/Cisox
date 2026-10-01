//! 系统 OCR 与本地 PP-OCR 的同图对比工具库。
//!
//! 注意：内置合成样片只用于对比两个引擎的相对表现，**不代表真实截图**，
//! 不能据此直接推断真实场景下的识别质量。

pub mod cer;
pub mod cli;
pub mod local;
pub mod report;
pub mod runner;
pub mod samples;

/// 复用主程序的 OCR 资产定位（同一份源码，不复制）。
#[path = "../../../crates/snow-shot/src/ocr_assets.rs"]
#[allow(dead_code)]
pub mod ocr_assets;

/// 复用主程序的 worker 客户端（同一份源码，不复制）。
#[path = "../../../crates/snow-shot/src/ocr_client.rs"]
#[allow(dead_code)]
pub mod ocr_client;
