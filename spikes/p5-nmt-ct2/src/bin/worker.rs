//! 翻译 worker：stdin/stdout JSON 行协议，首个翻译请求时才懒加载模型。
//! 请求 `{"id":1,"text":"..","beam":4}`；`{"cmd":"shutdown"}` 退出。
//! 响应 `{"id":1,"ok":true,"text":"..","ms":12}` 或 `{"id":1,"ok":false,"error":".."}`。
//! 启动参数：`worker <模型目录> [线程数]`。

use p5_nmt_ct2::{load, translate_one, working_set};
use serde::{Deserialize, Serialize};
use std::io::{BufRead, Write};
use std::time::Instant;

/// 单条请求。
#[derive(Deserialize)]
struct Req {
    id: Option<u64>,
    cmd: Option<String>,
    text: Option<String>,
    beam: Option<usize>,
}

/// 单条响应。
#[derive(Serialize, Default)]
struct Resp {
    id: u64,
    ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    text: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
    ms: u64,
    ws_mb: u64,
}

/// 入口：逐行读请求并应答。
fn main() -> anyhow::Result<()> {
    let a: Vec<String> = std::env::args().collect();
    let dir = a[1].clone();
    let threads: usize = a.get(2).and_then(|s| s.parse().ok()).unwrap_or(0);
    let mut tr = None;
    let mut out = std::io::stdout().lock();
    for line in std::io::stdin().lock().lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        let req: Req = match serde_json::from_str(&line) {
            Ok(r) => r,
            Err(e) => {
                let r = Resp { error: Some(format!("bad request: {e}")), ..Default::default() };
                writeln!(out, "{}", serde_json::to_string(&r)?)?;
                out.flush()?;
                continue;
            }
        };
        if req.cmd.as_deref() == Some("shutdown") {
            break;
        }
        let st = Instant::now();
        let mut resp = Resp { id: req.id.unwrap_or(0), ..Default::default() };
        let res = (|| -> anyhow::Result<String> {
            if tr.is_none() {
                tr = Some(load(&dir, threads)?);
            }
            translate_one(tr.as_ref().unwrap(), req.text.as_deref().unwrap_or(""), req.beam.unwrap_or(4))
        })();
        match res {
            Ok(t) => {
                resp.ok = true;
                resp.text = Some(t);
            }
            Err(e) => resp.error = Some(e.to_string()),
        }
        resp.ms = st.elapsed().as_millis() as u64;
        resp.ws_mb = (working_set().0 / 1048576) as u64;
        writeln!(out, "{}", serde_json::to_string(&resp)?)?;
        out.flush()?;
    }
    Ok(())
}
