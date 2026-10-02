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
/// 评测机上的 ORT 1.28.0 动态库（环境变量都没设时使用）。
const DEFAULT_ORT_DYLIB: &str =
    "E:/workspaces/Cisox/build/mt-quant/ort128/onnxruntime/capi/onnxruntime.dll";
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
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_snow-translator"));
        // 未显式指定动态库时，回落到评测机上的 ORT 1.28.0（存在才设置）
        if std::env::var_os("SNOW_ORT_DYLIB").is_none()
            && std::env::var_os("ORT_DYLIB_PATH").is_none()
            && std::path::Path::new(DEFAULT_ORT_DYLIB).is_file()
        {
            cmd.env("SNOW_ORT_DYLIB", DEFAULT_ORT_DYLIB);
        }
        let mut child = cmd
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

/// 环境变量：NLLB 模型包目录（外部数据 int4，14 语言）。
const ENV_NLLB_DIR: &str = "SNOW_TRANSLATOR_NLLB_DIR";
/// 评测机上的 NLLB 模型包目录。
const DEFAULT_NLLB_DIR: &str = "E:/models/translate-eval/nllb600m-main14-ccm-int4-ext";
/// 字节换算 MiB。
const MIB: f64 = 1024.0 * 1024.0;

/// 从外部读取子进程内存计数（Windows）。
#[cfg(windows)]
mod procmem {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
    use std::thread::JoinHandle;
    use std::time::Duration;

    /// Win32 `PROCESS_MEMORY_COUNTERS`。
    #[repr(C)]
    struct Counters {
        cb: u32,
        page_fault_count: u32,
        peak_working_set: usize,
        working_set: usize,
        quota_peak_paged: usize,
        quota_paged: usize,
        quota_peak_non_paged: usize,
        quota_non_paged: usize,
        private_usage: usize,
        peak_private_usage: usize,
    }

    #[link(name = "kernel32")]
    unsafe extern "system" {
        /// 读取进程内存计数。
        fn K32GetProcessMemoryInfo(process: isize, counters: *mut Counters, cb: u32) -> i32;
    }

    /// 内存读数（字节）。
    #[derive(Debug, Clone, Copy, Default)]
    pub struct Mem {
        /// 当前工作集。
        pub working_set: u64,
        /// 进程启动以来的峰值工作集。
        pub peak_working_set: u64,
        /// 私有内存（commit，与 Python 评测的 pagefile 口径一致）。
        pub private: u64,
    }

    /// 读取句柄对应进程的内存读数，失败返回全 0。
    pub fn read(handle: isize) -> Mem {
        let mut c = Counters {
            cb: size_of::<Counters>() as u32,
            page_fault_count: 0,
            peak_working_set: 0,
            working_set: 0,
            quota_peak_paged: 0,
            quota_paged: 0,
            quota_peak_non_paged: 0,
            quota_non_paged: 0,
            private_usage: 0,
            peak_private_usage: 0,
        };
        // SAFETY: c 是大小已声明的合法输出缓冲，handle 来自仍存活的子进程。
        let ok = unsafe { K32GetProcessMemoryInfo(handle, &mut c, c.cb) };
        if ok == 0 {
            return Mem::default();
        }
        Mem {
            working_set: c.working_set as u64,
            peak_working_set: c.peak_working_set as u64,
            private: c.private_usage as u64,
        }
    }

    /// 后台采样线程：每 10ms 读一次工作集，记录最大值。
    pub struct Sampler {
        /// 停止标志。
        stop: Arc<AtomicBool>,
        /// 采样到的最大工作集。
        max: Arc<AtomicU64>,
        /// 线程句柄。
        thread: Option<JoinHandle<()>>,
    }

    impl Sampler {
        /// 开始采样。
        pub fn start(handle: isize) -> Self {
            let stop = Arc::new(AtomicBool::new(false));
            let max = Arc::new(AtomicU64::new(0));
            let (s, m) = (stop.clone(), max.clone());
            let thread = std::thread::spawn(move || {
                while !s.load(Ordering::Relaxed) {
                    m.fetch_max(read(handle).working_set, Ordering::Relaxed);
                    std::thread::sleep(Duration::from_millis(10));
                }
            });
            Self {
                stop,
                max,
                thread: Some(thread),
            }
        }

        /// 停止采样并返回采样期间的最大工作集（字节）。
        pub fn finish(mut self) -> u64 {
            self.stop.store(true, Ordering::Relaxed);
            if let Some(t) = self.thread.take() {
                let _ = t.join();
            }
            self.max.load(Ordering::Relaxed)
        }
    }
}

/// 真实 NLLB（14 语言 int4 外部数据）在真实 worker 进程里的译文一致性与内存/延迟实测。
///
/// 对每个语言对各起一个全新的 worker 进程：加载 → 先后两遍逐句翻译 → 卸载，
/// 译文与纯 ORT 束搜索（purebeam.py，beam=2）的参考逐字对比，并打印内存与延迟。
/// 需要模型包与 ORT 1.28 动态库，缺失时跳过；用法：
/// `cargo test --release --test e2e real_nllb -- --ignored --nocapture`（机器安静时测内存与延迟）。
#[cfg(windows)]
#[test]
#[ignore = "需要 NLLB 模型包；机器安静时运行以获得可信的内存与延迟读数"]
fn real_nllb_parity_and_memory() {
    use std::os::windows::io::AsRawHandle;

    let dir = std::env::var_os(ENV_NLLB_DIR)
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| std::path::PathBuf::from(DEFAULT_NLLB_DIR));
    if !dir.join("model.json").is_file() {
        eprintln!("skip: {} not found (set {ENV_NLLB_DIR})", dir.display());
        return;
    }
    let dir = dir.to_string_lossy().replace('\\', "/");
    let fixture: Value =
        serde_json::from_str(include_str!("fixtures/nllb_parity.json")).expect("parity fixture");

    let (mut total, mut exact_reseg, mut exact_remap) = (0, 0, 0);
    let mut diffs: Vec<String> = Vec::new();
    for pair in fixture["pairs"].as_array().expect("pairs") {
        let (src, tgt) = (pair["src"].as_str().unwrap(), pair["tgt"].as_str().unwrap());
        let mut p = Proc::spawn();
        let handle = p.child.as_raw_handle() as isize;
        let idle = procmem::read(handle);

        let started = Instant::now();
        let r = p.call(json!({"cmd":"load","model_dir":dir,"src":src,"tgt":tgt}));
        assert_eq!(r["evt"], "loaded", "{r}");
        let load_wall = started.elapsed();
        let after_load = procmem::read(handle);

        let sampler = procmem::Sampler::start(handle);
        let mut first_ms = 0.0;
        let (mut pass1_ms, mut hot_ms, mut count) = (0.0, 0.0, 0usize);
        let mut first_pass: Vec<String> = Vec::new();
        for pass in 0..2 {
            for (i, case) in pair["cases"].as_array().unwrap().iter().enumerate() {
                let text = case["text"].as_str().unwrap();
                let t0 = Instant::now();
                let r = p.call(json!({"cmd":"translate","id":1,"texts":[text]}));
                let ms = t0.elapsed().as_secs_f64() * 1000.0;
                assert_eq!(r["evt"], "result", "{r}");
                let got = r["texts"][0].as_str().unwrap().to_string();
                if pass == 0 {
                    if i == 0 {
                        first_ms = ms;
                    }
                    pass1_ms += ms;
                    first_pass.push(got.clone());
                    let (reseg, remap) = (
                        case["reseg_text"].as_str().unwrap(),
                        case["remap_text"].as_str().unwrap(),
                    );
                    total += 1;
                    exact_reseg += usize::from(got == reseg);
                    exact_remap += usize::from(got == remap);
                    if got != reseg {
                        diffs.push(format!(
                            "{src}->{tgt} #{}\n  rust  : {got}\n  python: {reseg}",
                            case["flores_index"]
                        ));
                    }
                } else {
                    hot_ms += ms;
                    count += 1;
                    assert_eq!(got, first_pass[i], "同一进程内两遍译文必须一致");
                }
            }
        }
        let max_ws = sampler.finish();
        let end = procmem::read(handle);
        let n = pair["cases"].as_array().unwrap().len() as f64;
        eprintln!(
            "[{src}->{tgt}] 空闲 {:.0} | 加载后 WS {:.0} 私有 {:.0} 加载峰值 {:.0} | 翻译期峰值 WS {:.0}（采样）进程峰值 {:.0} | 结束 WS {:.0} 私有 {:.0} MiB | 加载 {:.2}s | 首句 {:.0}ms 第一遍均值 {:.0}ms 热均值 {:.0}ms",
            idle.working_set as f64 / MIB,
            after_load.working_set as f64 / MIB,
            after_load.private as f64 / MIB,
            after_load.peak_working_set as f64 / MIB,
            max_ws as f64 / MIB,
            end.peak_working_set as f64 / MIB,
            end.working_set as f64 / MIB,
            end.private as f64 / MIB,
            load_wall.as_secs_f64(),
            first_ms,
            pass1_ms / n,
            hot_ms / count as f64,
        );
        // 内存映射生效的粗略护栏：加载后工作集远低于权重全驻留的 540 MiB 基线
        assert!(
            (after_load.working_set as f64 / MIB) < 450.0,
            "加载后工作集过高，外部数据可能没有走内存映射"
        );
        assert_eq!(p.call(json!({"cmd":"unload"}))["evt"], "unloaded");
        assert!(p.wait_exit());
    }
    eprintln!(
        "译文一致性：{total} 句，与同分词的纯 ORT 参考逐字一致 {exact_reseg}，与 remap 分词参考逐字一致 {exact_remap}"
    );
    for d in &diffs {
        eprintln!("差异 {d}");
    }
    assert_eq!(exact_reseg, total, "与纯 ORT 束搜索参考不一致，见上方差异");
}

/// 评测结果目录（只读）：推荐解码（beam=2、lp=2.0、min-ratio 0.7）的 purebeam 输出。
const EVAL_NEW_DIR: &str = "E:/workspaces/Cisox/materials/translate/results/nllb600m-pruned-main14-ccm-int4-dec-c3_b2_lp2_min0.7";
/// 评测结果目录（只读）：旧配置（beam=2、lp=1.0、无最小长度）的 purebeam 输出。
const EVAL_OLD_DIR: &str =
    "E:/workspaces/Cisox/materials/translate/results/nllb600m-pruned-main14-ccm-int4-dec-c1_b2_lp1";
/// 复核的评测句数（取 src.txt 前 N 句）。
const DECODE_CHECK_SENTENCES: usize = 10;
/// 偏短阈值：译文字数 / 参考字数低于它算偏短。
const SHORT_RATIO: f64 = 0.7;

/// 读取评测目录里某语向的某个文本文件，取前 N 行（缺失返回 `None`）。
fn read_eval_lines(root: &str, pair: &str, name: &str) -> Option<Vec<String>> {
    let text = std::fs::read_to_string(format!("{root}/{pair}/{name}")).ok()?;
    Some(
        text.lines()
            .take(DECODE_CHECK_SENTENCES)
            .map(str::to_string)
            .collect(),
    )
}

/// 偏短句数：译文字数 / 参考字数 < [`SHORT_RATIO`]。
fn count_short(hyps: &[String], refs: &[String]) -> usize {
    hyps.iter()
        .zip(refs)
        .filter(|(h, r)| (h.chars().count() as f64) < SHORT_RATIO * r.chars().count() as f64)
        .count()
}

/// 译文里是否还有紧跟中日文字符的半角标点（全角后处理漏网）。
fn has_halfwidth_after_cjk(s: &str) -> bool {
    let cs: Vec<char> = s.chars().collect();
    cs.windows(2).any(|w| {
        ('\u{3040}'..='\u{9fff}').contains(&w[0]) && matches!(w[1], ',' | '.' | '?' | '!' | ';')
    })
}

/// 解码配置端到端复核：eng→zho、fra→zho、eng→jpn 各 10 句（评测 src.txt 前 10 句）。
///
/// 与评测环境用 purebeam.py 同参数（beam=2、lp=2.0、min-ratio 0.7，整行一次解码）的译文对比并列出差异，
/// 比较偏短句数相对旧配置的变化，并断言全角标点已生效。模型包或评测目录缺失时跳过。
#[cfg(windows)]
#[test]
#[ignore = "需要 NLLB 模型包与评测结果目录；占用 CPU，机器安静时运行"]
fn real_nllb_decode_config_check() {
    let dir = std::env::var_os(ENV_NLLB_DIR)
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| std::path::PathBuf::from(DEFAULT_NLLB_DIR));
    if !dir.join("model.json").is_file() {
        eprintln!("skip: {} not found (set {ENV_NLLB_DIR})", dir.display());
        return;
    }
    let dir = dir.to_string_lossy().replace('\\', "/");
    let (mut total, mut exact, mut short_rust, mut short_old, mut short_new_eval) = (0, 0, 0, 0, 0);
    for (pair, src, tgt) in [
        ("eng_Latn-zho_Hans", "en", "zh-CN"),
        ("fra_Latn-zho_Hans", "fr", "zh-CN"),
        ("eng_Latn-jpn_Jpan", "en", "ja"),
    ] {
        let (Some(srcs), Some(refs), Some(new_hyp), Some(old_hyp)) = (
            read_eval_lines(EVAL_NEW_DIR, pair, "src.txt"),
            read_eval_lines(EVAL_NEW_DIR, pair, "ref.txt"),
            read_eval_lines(EVAL_NEW_DIR, pair, "hyp_fullwidth.txt"),
            read_eval_lines(EVAL_OLD_DIR, pair, "hyp_fullwidth.txt"),
        ) else {
            eprintln!("skip: 评测结果目录缺少 {pair}");
            return;
        };
        let mut p = Proc::spawn();
        let r = p.call(json!({"cmd":"load","model_dir":dir,"src":src,"tgt":tgt}));
        assert_eq!(r["evt"], "loaded", "{r}");
        let mut got = Vec::new();
        for text in &srcs {
            // 不带 num_beams：使用清单缺省（beam=2）
            let r = p.call(json!({"cmd":"translate","id":1,"texts":[text]}));
            assert_eq!(r["evt"], "result", "{r}");
            got.push(r["texts"][0].as_str().unwrap().to_string());
        }
        assert_eq!(p.call(json!({"cmd":"unload"}))["evt"], "unloaded");
        assert!(p.wait_exit());

        for (i, g) in got.iter().enumerate() {
            total += 1;
            exact += usize::from(*g == new_hyp[i]);
            if *g != new_hyp[i] {
                eprintln!(
                    "差异 {pair} #{i}\n  rust   : {g}\n  purebeam: {}",
                    new_hyp[i]
                );
            }
            assert!(!has_halfwidth_after_cjk(g), "全角标点未生效: {g}");
        }
        let (s_rust, s_old, s_new) = (
            count_short(&got, &refs),
            count_short(&old_hyp, &refs),
            count_short(&new_hyp, &refs),
        );
        eprintln!(
            "[{pair}] 偏短句数（前 {DECODE_CHECK_SENTENCES} 句）：旧配置 purebeam {s_old}，新配置 purebeam {s_new}，Rust 新默认 {s_rust}"
        );
        short_rust += s_rust;
        short_old += s_old;
        short_new_eval += s_new;
    }
    eprintln!(
        "合计 {total} 句：与新配置 purebeam 逐字一致 {exact}；偏短 旧 {short_old} / 新 purebeam {short_new_eval} / Rust {short_rust}"
    );
    assert!(short_rust <= short_old, "偏短句数不应多于旧配置");
}

/// 环境变量：Hy-MT2 模型包目录（`scripts/make_hymt2_pack.py` 的产物）。
const ENV_HYMT_DIR: &str = "SNOW_TRANSLATOR_HYMT_DIR";
/// 评测机上的 Hy-MT2 模型包目录。
const DEFAULT_HYMT_DIR: &str = "E:/models/translate-eval/hymt2-1.8b-int4-pack";
/// 纯 ORT 参考结果目录（`ort_gen.py --version hymt2-pack-check --limit 2` 的输出，只读）。
const HYMT_REF_DIR: &str = "E:/workspaces/Cisox/materials/translate/results/hymt2-pack-check";
/// 对拍的语向：(参考目录名, 应用源语言, 应用目标语言)。
const HYMT_PAIRS: [(&str, &str, &str); 2] = [
    ("eng_Latn-zho_Hans", "en", "zh-CN"),
    ("zho_Hans-eng_Latn", "zh-CN", "en"),
];
/// 对拍句数（只跑 2 句，避免长时间占满 CPU）。
const HYMT_SENTENCES: usize = 2;
/// Win32 `BELOW_NORMAL_PRIORITY_CLASS`。
#[cfg(windows)]
const BELOW_NORMAL_PRIORITY_CLASS: u32 = 0x4000;

/// 读取纯 ORT 参考的前 N 句原文与译文（`src.txt` 与 `hyp.partial.jsonl` 的 `raw`，去首尾空白）。
fn read_hymt_ref(pair: &str) -> Option<(Vec<String>, Vec<String>)> {
    let srcs = std::fs::read_to_string(format!("{HYMT_REF_DIR}/{pair}/src.txt")).ok()?;
    let rows = std::fs::read_to_string(format!("{HYMT_REF_DIR}/{pair}/hyp.partial.jsonl")).ok()?;
    let hyps: Vec<String> = rows
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| {
            let v: Value = serde_json::from_str(l).expect("参考 jsonl");
            v["raw"].as_str().unwrap_or_default().trim().to_string()
        })
        .collect();
    let srcs: Vec<String> = srcs.lines().map(str::to_string).collect();
    (srcs.len() >= HYMT_SENTENCES && hyps.len() >= HYMT_SENTENCES).then_some((srcs, hyps))
}

/// 真实 Hy-MT2（int4 外部数据）在真实 worker 进程里的译文对拍与内存实测。
///
/// 每个语向各起一个 worker（降到 BelowNormal 优先级，模型包清单限制 4 线程）：加载 → 翻译评测前 2 句 →
/// 与 `ort_gen.py` 的输出逐字对比，打印加载后工作集与翻译期峰值。模型包或参考缺失时跳过。
/// 用法：`cargo test --release --test e2e real_hymt2 -- --ignored --nocapture`。
#[cfg(windows)]
#[test]
#[ignore = "需要 Hy-MT2 模型包与纯 ORT 参考；每句约 10 秒以上，机器安静时运行"]
fn real_hymt2_parity_and_memory() {
    use std::os::windows::io::AsRawHandle;

    #[link(name = "kernel32")]
    unsafe extern "system" {
        /// 设置进程优先级类。
        fn SetPriorityClass(process: isize, class: u32) -> i32;
    }

    let dir = std::env::var_os(ENV_HYMT_DIR)
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| std::path::PathBuf::from(DEFAULT_HYMT_DIR));
    if !dir.join("model.json").is_file() {
        eprintln!("skip: {} not found (set {ENV_HYMT_DIR})", dir.display());
        return;
    }
    let dir = dir.to_string_lossy().replace('\\', "/");
    let (mut total, mut exact) = (0, 0);
    for (pair, src, tgt) in HYMT_PAIRS {
        let Some((srcs, refs)) = read_hymt_ref(pair) else {
            eprintln!("skip: no python reference for {pair} under {HYMT_REF_DIR}");
            return;
        };
        let mut p = Proc::spawn();
        let handle = p.child.as_raw_handle() as isize;
        // SAFETY: handle 来自仍存活的子进程，创建时即带全部访问权限。
        assert_ne!(
            unsafe { SetPriorityClass(handle, BELOW_NORMAL_PRIORITY_CLASS) },
            0
        );
        let idle = procmem::read(handle);
        let started = Instant::now();
        let r = p.call(json!({"cmd":"load","model_dir":dir,"src":src,"tgt":tgt}));
        assert_eq!(r["evt"], "loaded", "{r}");
        let load_wall = started.elapsed();
        let after_load = procmem::read(handle);
        let sampler = procmem::Sampler::start(handle);
        let mut ms_list = Vec::new();
        for i in 0..HYMT_SENTENCES {
            let t0 = Instant::now();
            // num_beams 对本族无效，传 4 验证被忽略而不是报错
            let r = p.call(json!({"cmd":"translate","id":1,"texts":[srcs[i]],"num_beams":4}));
            ms_list.push(t0.elapsed().as_secs_f64() * 1000.0);
            assert_eq!(r["evt"], "result", "{r}");
            let got = r["texts"][0].as_str().unwrap().trim().to_string();
            total += 1;
            if got == refs[i] {
                exact += 1;
            } else {
                eprintln!("差异 {pair} #{i}\n  rust  : {got}\n  python: {}", refs[i]);
            }
        }
        let max_ws = sampler.finish();
        let end = procmem::read(handle);
        eprintln!(
            "[{src}->{tgt}] 空闲 {:.0} | 加载后 WS {:.0} 私有 {:.0} 加载峰值 {:.0} | 翻译期峰值 WS {:.0}（采样）进程峰值 {:.0} | 结束 WS {:.0} 私有 {:.0} MiB | 加载 {:.2}s | 每句 {:?} ms",
            idle.working_set as f64 / MIB,
            after_load.working_set as f64 / MIB,
            after_load.private as f64 / MIB,
            after_load.peak_working_set as f64 / MIB,
            max_ws as f64 / MIB,
            end.peak_working_set as f64 / MIB,
            end.working_set as f64 / MIB,
            end.private as f64 / MIB,
            load_wall.as_secs_f64(),
            ms_list.iter().map(|m| m.round() as u64).collect::<Vec<_>>(),
        );
        assert_eq!(p.call(json!({"cmd":"unload"}))["evt"], "unloaded");
        assert!(p.wait_exit());
    }
    eprintln!("Hy-MT2 对拍：{total} 句，与 ort_gen.py 逐字一致 {exact}");
    assert_eq!(exact, total, "与纯 ORT 参考不一致，见上方差异");
}
