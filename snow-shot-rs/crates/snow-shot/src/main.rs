//! 程序入口：子模式分发与单实例（P1 后续任务填充）。
//!
//! 所属阶段：P1。当前仅打印产品名与版本后退出（降级态）。

use snow_app_core::PRODUCT_NAME;

/// 生成启动横幅文本。
fn banner() -> String {
    format!("{} {}", PRODUCT_NAME, env!("CARGO_PKG_VERSION"))
}

/// 程序入口。
fn main() {
    println!("{}", banner());
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 横幅应包含产品名。
    #[test]
    fn banner_contains_product_name() {
        assert!(banner().starts_with(PRODUCT_NAME));
    }
}
