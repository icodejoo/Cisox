//! 内置语料：`build.rs` 扫描 `locales/*/*.ftl` 与 `locales/*/locale.toml` 生成，新增文件无需改代码。

use crate::locales::LocaleInfo;

include!(concat!(env!("OUT_DIR"), "/embedded_generated.rs"));
