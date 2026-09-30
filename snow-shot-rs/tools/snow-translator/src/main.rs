//! Snow Shot 翻译工作进程：stdin/stdout JSON 行协议，空闲卸载 = 进程退出。

mod beam;
mod checksum;
mod engine;
mod manifest;
mod protocol;
mod sysmem;
mod text;
mod worker;

use std::io::{self, BufReader, Write};

use protocol::{
    ErrorKind, Event, LineRead, MAX_LINE_BYTES, PROTOCOL_VERSION, parse_command, read_bounded_line,
    write_event,
};
use worker::{Outcome, Worker, load_engine};

/// 发送事件，管道断开时返回 `false`（宿主已走，直接退出）。
fn send(out: &mut impl Write, event: &Event) -> bool {
    write_event(out, event).is_ok()
}

/// 主循环：读命令、分发、写事件；stdin 关闭即退出。
fn main() {
    let stdin = io::stdin();
    let mut reader = BufReader::new(stdin.lock());
    let stdout = io::stdout();
    let mut out = stdout.lock();
    let mut worker = Worker::new(load_engine);

    if !send(
        &mut out,
        &Event::Ready {
            protocol: PROTOCOL_VERSION,
            pid: std::process::id(),
        },
    ) {
        return;
    }
    loop {
        let line = match read_bounded_line(&mut reader, MAX_LINE_BYTES) {
            Ok(LineRead::Line(l)) => l,
            Ok(LineRead::Eof) | Err(_) => return,
            Ok(LineRead::TooLong) => {
                let e = Event::Error {
                    id: None,
                    kind: ErrorKind::BadRequest,
                    message: format!("line exceeds {MAX_LINE_BYTES} bytes"),
                };
                if !send(&mut out, &e) {
                    return;
                }
                continue;
            }
        };
        if line.trim().is_empty() {
            continue;
        }
        let outcome = match parse_command(&line) {
            Ok(cmd) => worker.handle(cmd),
            Err(message) => Outcome {
                events: vec![Event::Error {
                    id: None,
                    kind: ErrorKind::BadRequest,
                    message,
                }],
                exit: false,
            },
        };
        for e in &outcome.events {
            if !send(&mut out, e) {
                return;
            }
        }
        if outcome.exit {
            return;
        }
    }
}
