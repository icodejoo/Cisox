//! 进程级端到端测试：真实拉起 snow-translator 二进制，走 stdin/stdout 行协议。
//!
//! 离线用例不依赖模型；真实模型用例需要环境变量 `SNOW_TRANSLATOR_TEST_MODEL_DIR`（缺失则跳过），
//! 动态库位置由 `SNOW_ORT_DYLIB` 指定。

use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::time::{Duration, Instant};

use serde_json::{Value, json};

/// 环境变量：真实模型目录。
const ENV_MODEL_DIR: &str = "SNOW_TRANSLATOR_TEST_MODEL_DIR";
/// 等待进程退出的最长时间。
const EXIT_TIMEOUT: Duration = Duration::from_secs(10);

/// 被测进程句柄。
struct Proc {
    /// 子进程。
    child: Child,
    /// 写命令。
    stdin: Option<ChildStdin>,
    /// 读事件。
    stdout: BufReader<ChildStdout>,
}

impl Proc {
    /// 启动并吃掉 `ready` 事件。
    fn spawn() -> Self {
        let mut child = Command::new(env!("CARGO_BIN_EXE_snow-translator"))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn snow-translator");
        let stdin = child.stdin.take();
        let stdout = BufReader::new(child.stdout.take().expect("stdout"));
        let mut p = Self {
            child,
            stdin,
            stdout,
        };
        let ready = p.recv();
        assert_eq!(ready["evt"], "ready");
        assert_eq!(ready["protocol"], 1);
        p
    }

    /// 发送一行原始文本。
    fn send_raw(&mut self, line: &str) {
        let stdin = self.stdin.as_mut().expect("stdin open");
        writeln!(stdin, "{line}").expect("write");
        stdin.flush().expect("flush");
    }

    /// 发送 JSON 命令并读取一个事件。
    fn call(&mut self, cmd: Value) -> Value {
        self.send_raw(&cmd.to_string());
        self.recv()
    }

    /// 读取一个事件。
    fn recv(&mut self) -> Value {
        let mut line = String::new();
        self.stdout.read_line(&mut line).expect("read event");
        serde_json::from_str(line.trim()).unwrap_or_else(|e| panic!("bad event {line:?}: {e}"))
    }

    /// 等待进程退出并返回是否成功退出。
    fn wait_exit(&mut self) -> bool {
        let start = Instant::now();
        loop {
            if let Some(status) = self.child.try_wait().expect("try_wait") {
                return status.success();
            }
            if start.elapsed() > EXIT_TIMEOUT {
                let _ = self.child.kill();
                return false;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }
}

/// 未加载时的错误、坏命令、缺失目录都得到结构化错误，进程不崩。
#[test]
fn offline_error_paths() {
    let mut p = Proc::spawn();
    let r = p.call(json!({"cmd":"translate","id":1,"text":"hi"}));
    assert_eq!(
        (r["evt"].as_str(), r["kind"].as_str()),
        (Some("error"), Some("not_loaded"))
    );
    assert_eq!(r["id"], 1);

    p.send_raw("this is not json");
    assert_eq!(p.recv()["kind"], "bad_request");

    let r = p.call(json!({"cmd":"load","model_dir":"Z:/no/such/dir","src":"en","tgt":"zh-CN"}));
    assert_eq!(r["kind"], "model_missing");

    let r = p.call(json!({"cmd":"ping"}));
    assert_eq!(
        (r["evt"].as_str(), r["loaded"].as_bool()),
        (Some("pong"), Some(false))
    );
    assert!(p.child.try_wait().unwrap().is_none(), "进程应仍然存活");
}

/// stdin 关闭后进程自行退出；Unload 回复 unloaded 后退出。
#[test]
fn exits_on_eof_and_on_unload() {
    let mut p = Proc::spawn();
    p.stdin = None;
    assert!(p.wait_exit(), "stdin 关闭后应退出");

    let mut p = Proc::spawn();
    assert_eq!(p.call(json!({"cmd":"unload"}))["evt"], "unloaded");
    assert!(p.wait_exit(), "unload 后应退出");
}

/// 真实模型：加载、批量翻译、Unload 退出。
#[test]
fn real_model_roundtrip() {
    let Some(dir) = std::env::var_os(ENV_MODEL_DIR) else {
        eprintln!("skip: {ENV_MODEL_DIR} not set");
        return;
    };
    let dir = dir.to_string_lossy().replace('\\', "/");
    let mut p = Proc::spawn();
    let r = p.call(json!({"cmd":"load","model_dir":dir,"src":"en","tgt":"zh-CN"}));
    assert_eq!(r["evt"], "loaded", "{r}");

    let r = p.call(json!({"cmd":"translate","id":5,"texts":["Where is the nearest train station?","Thank you."]}));
    assert_eq!(r["evt"], "result", "{r}");
    let texts = r["texts"].as_array().expect("texts");
    assert_eq!(texts.len(), 2);
    let has_cjk = |s: &Value| {
        s.as_str()
            .unwrap_or_default()
            .chars()
            .any(|c| ('\u{4e00}'..='\u{9fff}').contains(&c))
    };
    assert!(texts.iter().all(has_cjk), "{texts:?}");

    let r = p.call(json!({"cmd":"load","model_dir":dir,"src":"fr","tgt":"zh-CN"}));
    assert_eq!(r["kind"], "unsupported_pair");

    assert_eq!(p.call(json!({"cmd":"unload"}))["evt"], "unloaded");
    assert!(p.wait_exit());
}
