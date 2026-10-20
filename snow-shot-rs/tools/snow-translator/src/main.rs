//! Snow Shot 翻译工作进程：stdin/stdout JSON 行协议，空闲卸载 = 进程退出。

mod beam;
mod cancellation;
mod chat;
mod engine;
mod manifest;
mod protocol;
mod sysmem;
mod text;
mod worker;
mod zh_punct;

use std::io::{self, BufRead, BufReader, Write};
use std::sync::{Arc, mpsc};

use protocol::{
    ErrorKind, Event, LineRead, MAX_LINE_BYTES, PROTOCOL_VERSION, parse_command, read_bounded_line,
    write_event,
};
use worker::{Outcome, Worker, error_event, load_engine};

/// 发送事件，管道断开时返回 `false`（宿主已走，直接退出）。
fn send(out: &mut impl Write, event: &Event) -> bool {
    write_event(out, event).is_ok()
}

/// 独立读取命令，EOF 即取消正在执行的推理；有界队列满时终止异常宿主管道。
fn read_commands(
    reader: &mut impl BufRead,
    state: &cancellation::Cancellation,
    tx: &mpsc::SyncSender<Result<protocol::Command, String>>,
) {
    loop {
        let message = match read_bounded_line(reader, MAX_LINE_BYTES) {
            Ok(LineRead::Line(line)) if line.trim().is_empty() => continue,
            Ok(LineRead::Line(line)) => parse_command(&line),
            Ok(LineRead::TooLong) => Err(format!("line exceeds {MAX_LINE_BYTES} bytes")),
            Ok(LineRead::Eof) | Err(_) => {
                state.cancel();
                return;
            }
        };
        if tx.try_send(message).is_err() {
            state.cancel();
            return;
        }
    }
}

/// 主循环执行推理，独立读取线程随时处理管道关闭。
fn main() {
    let state = Arc::new(cancellation::Cancellation::default());
    let (tx, rx) = mpsc::sync_channel(8);
    let reader_state = Arc::clone(&state);
    std::thread::spawn(move || {
        read_commands(&mut BufReader::new(io::stdin().lock()), &reader_state, &tx)
    });
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
        let Ok(command) = rx.recv() else {
            return;
        };
        if state.is_canceled() {
            return;
        }
        let outcome = match command {
            Ok(cmd) => cancellation::scope(Arc::clone(&state), || worker.handle(cmd)),
            Err(message) => Outcome {
                events: vec![error_event(None, ErrorKind::BadRequest, message)],
                exit: false,
            },
        };
        if state.is_canceled() {
            return;
        }
        for e in &outcome.events {
            if !send(&mut out, e) {
                return;
            }
        }
        if outcome.exit {
            return;
        }
        // 结果已发出并 flush，再做内存收缩，下一条命令处理前完成
        cancellation::scope(Arc::clone(&state), || worker.after_send());
    }
}

#[cfg(test)]
mod tests {
    /// 推理进行中由独立读取线程观测 EOF，取消传播到下一步解码。
    #[test]
    fn stdin_eof_interrupts_inference_in_progress() {
        use crate::chat::{DecodeParams, LogitsSource, greedy_decode};
        /// 首步等待 stdin 线程处理 EOF 的假模型。
        struct Model {
            entered: mpsc::Sender<()>,
            resume: mpsc::Receiver<()>,
            calls: usize,
        }
        impl LogitsSource for Model {
            /// 通知首步已开始，等待读取线程关闭管道。
            fn forward(
                &mut self,
                _: &[u32],
                _: usize,
            ) -> Result<&[f32], crate::engine::EngineError> {
                self.calls += 1;
                self.entered.send(()).unwrap();
                self.resume.recv().unwrap();
                Ok(&[0.0, 1.0])
            }
        }
        let state = Arc::new(cancellation::Cancellation::default());
        let (entered_tx, entered_rx) = mpsc::channel();
        let (resume_tx, resume_rx) = mpsc::channel();
        let reader_state = Arc::clone(&state);
        let reader = std::thread::spawn(move || {
            entered_rx.recv().unwrap();
            let (tx, _rx) = mpsc::sync_channel(8);
            read_commands(&mut std::io::Cursor::new(b""), &reader_state, &tx);
            resume_tx.send(()).unwrap();
        });
        let mut model = Model {
            entered: entered_tx,
            resume: resume_rx,
            calls: 0,
        };
        let result = cancellation::scope(state, || {
            greedy_decode(
                &mut model,
                &[1],
                &DecodeParams {
                    eos: 0,
                    max_new: 100,
                    repetition_penalty: 1.0,
                },
            )
        });
        reader.join().unwrap();
        assert!(result.is_err());
        assert_eq!(model.calls, 1);
    }

    use super::*;
    /// EOF 的读取不依赖推理线程消费下一条命令。
    #[test]
    fn eof_cancels_without_command_consumption() {
        let state = cancellation::Cancellation::default();
        let (tx, rx) = mpsc::sync_channel(8);
        read_commands(
            &mut std::io::Cursor::new(b"{\"cmd\":\"ping\"}\n"),
            &state,
            &tx,
        );
        assert!(state.is_canceled());
        assert_eq!(rx.recv().unwrap().unwrap(), protocol::Command::Ping);
    }
}
