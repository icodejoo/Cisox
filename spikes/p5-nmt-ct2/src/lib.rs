//! P5 spike 公共代码：翻译器封装与进程内存读取。

use ct2rs::{Config, Translator, TranslationOptions};

/// Windows 进程内存计数结构（与 PROCESS_MEMORY_COUNTERS 布局一致）。
#[repr(C)]
#[derive(Default)]
struct ProcessMemoryCounters {
    cb: u32,
    page_fault_count: u32,
    peak_working_set_size: usize,
    working_set_size: usize,
    quota_peak_paged_pool_usage: usize,
    quota_paged_pool_usage: usize,
    quota_peak_non_paged_pool_usage: usize,
    quota_non_paged_pool_usage: usize,
    pagefile_usage: usize,
    peak_pagefile_usage: usize,
}

extern "system" {
    fn GetCurrentProcess() -> isize;
    fn K32GetProcessMemoryInfo(p: isize, c: *mut ProcessMemoryCounters, cb: u32) -> i32;
}

/// 返回 (当前工作集, 峰值工作集)，单位字节。
pub fn working_set() -> (usize, usize) {
    let mut c = ProcessMemoryCounters::default();
    c.cb = std::mem::size_of::<ProcessMemoryCounters>() as u32;
    // SAFETY: 结构体布局与系统定义一致，句柄为当前进程伪句柄
    unsafe { K32GetProcessMemoryInfo(GetCurrentProcess(), &mut c, c.cb) };
    (c.working_set_size, c.peak_working_set_size)
}

/// 加载翻译器；threads 为 0 表示由 CT2 自行决定。
pub fn load(dir: &str, threads: usize) -> anyhow::Result<Translator> {
    let cfg = Config {
        num_threads_per_replica: threads,
        ..Config::default()
    };
    Translator::new(dir, &cfg)
}

/// 翻译单句，beam 为束宽（1 即贪心）。
pub fn translate_one(t: &Translator, text: &str, beam: usize) -> anyhow::Result<String> {
    let opts = TranslationOptions {
        beam_size: beam,
        ..TranslationOptions::<String, String>::default()
    };
    let res = t.translate_batch(&[text], &opts, None)?;
    Ok(res.into_iter().next().map(|(s, _)| s).unwrap_or_default())
}
