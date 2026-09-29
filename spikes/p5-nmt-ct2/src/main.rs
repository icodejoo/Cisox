//! P5 spike 基准：用法 `p5-nmt-ct2 <模型目录> <线程数> <beam> <重复次数>`。

use p5_nmt_ct2::{load, translate_one, working_set};
use std::time::Instant;

/// 固定测试句集（含长句、标点数字、空串）。
const SENTENCES: &[&str] = &[
    "今天天气很好。",
    "我明天下午三点要去北京参加一个重要的会议，请提前把资料准备好。",
    "订单号 A-20240915 的金额是 1,234.56 元，请在 3 天内付款！",
    "",
    "深度学习已经彻底改变了自然语言处理领域，机器翻译的质量在过去十年里有了显著提升，但是在低资源语言和专业术语上仍然存在很多挑战，研究人员正在探索更高效的模型压缩和量化方法，以便让这些模型能够在普通个人电脑上流畅运行。",
    "你好，世界。",
];

/// 取分位数（已排序，单位毫秒）。
fn pct(v: &[f64], p: f64) -> f64 {
    let i = ((v.len() as f64 - 1.0) * p).round() as usize;
    v[i]
}

/// 入口：加载、首译、重复测热调用并输出统计。
fn main() -> anyhow::Result<()> {
    let a: Vec<String> = std::env::args().collect();
    let dir = &a[1];
    let threads: usize = a.get(2).and_then(|s| s.parse().ok()).unwrap_or(0);
    let beam: usize = a.get(3).and_then(|s| s.parse().ok()).unwrap_or(4);
    let reps: usize = a.get(4).and_then(|s| s.parse().ok()).unwrap_or(10);
    let t0 = Instant::now();
    let t = load(dir, threads)?;
    let load_ms = t0.elapsed().as_secs_f64() * 1e3;
    let (ws_load, _) = working_set();
    let t1 = Instant::now();
    let first = translate_one(&t, SENTENCES[0], beam)?;
    let first_ms = t1.elapsed().as_secs_f64() * 1e3;
    println!("threads={threads} beam={beam} load={load_ms:.0}ms first={first_ms:.0}ms load+first={:.0}ms ws_after_load={:.0}MB", load_ms + first_ms, ws_load as f64 / 1048576.0);
    println!("first -> {first}");
    for (i, s) in SENTENCES.iter().enumerate() {
        let mut ms = Vec::new();
        let mut out = String::new();
        for _ in 0..reps {
            let t = {
                let st = Instant::now();
                out = translate_one(&t, s, beam)?;
                st.elapsed().as_secs_f64() * 1e3
            };
            ms.push(t);
        }
        ms.sort_by(|a, b| a.partial_cmp(b).unwrap());
        println!("[{i}] chars={} med={:.0}ms p95={:.0}ms | {out}", s.chars().count(), pct(&ms, 0.5), pct(&ms, 0.95));
    }
    let (cur, peak) = working_set();
    println!("ws_now={:.0}MB ws_peak={:.0}MB", cur as f64 / 1048576.0, peak as f64 / 1048576.0);
    Ok(())
}
