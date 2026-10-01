//! 命令行入口：`gen` 生成合成样片，`run` 跑同图对比。

use snow_ocr_compare::cli::{Command, RunOptions, parse_args, usage};
use snow_ocr_compare::local::{LocalRecognizer, default_asset_root, env_exe_override};
use snow_ocr_compare::report::{DISCLAIMER, render_table, to_csv};
use snow_ocr_compare::runner::{BackendRun, MemoryReport, SystemRecognizer, run_backend};
use snow_ocr_compare::samples::{Case, load_dir, write_all};
use std::process::ExitCode;

/// 程序入口。
fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match parse_args(&args) {
        Ok(Command::Gen { out }) => match write_all(&out) {
            Ok(n) => {
                println!("wrote {n} synthetic samples to {}", out.display());
                println!("{DISCLAIMER}");
                ExitCode::SUCCESS
            }
            Err(e) => fail(&e),
        },
        Ok(Command::Run(options)) => match run(&options) {
            Ok(()) => ExitCode::SUCCESS,
            Err(e) => fail(&e),
        },
        Err(e) => {
            eprintln!("{e}\n\n{}", usage());
            ExitCode::from(2)
        }
    }
}

/// 打印错误并返回失败码。
fn fail(message: &str) -> ExitCode {
    eprintln!("error: {message}");
    ExitCode::FAILURE
}

/// 执行对比：先 system，再 local-model；任一后端不可用都只报告，不中断。
fn run(options: &RunOptions) -> Result<(), String> {
    let (cases, skipped) = load_dir(&options.dir)?;
    for note in &skipped {
        eprintln!("skipped: {note}");
    }
    if cases.is_empty() {
        return Err(format!(
            "no usable <name>.png + <name>.txt pairs in {}",
            options.dir.display()
        ));
    }
    let mut runs: Vec<BackendRun> = Vec::new();
    if options.system {
        let mut system = SystemRecognizer {
            language: options.language.clone(),
        };
        runs.push(run_backend(&mut system, &cases));
    }
    if options.local {
        runs.push(run_local(options, &cases));
    }
    println!("{}", render_table(&runs));
    if let Some(path) = &options.csv {
        std::fs::write(path, to_csv(&runs))
            .map_err(|e| format!("cannot write {}: {e}", path.display()))?;
        println!("csv written to {}", path.display());
    }
    Ok(())
}

/// 跑本地模型后端；资产缺失或 worker 起不来时返回“不可用”结果，不崩溃。
fn run_local(options: &RunOptions, cases: &[Case]) -> BackendRun {
    let unavailable = |why: String| BackendRun {
        name: "local-model".into(),
        rows: Vec::new(),
        memory: MemoryReport::default(),
        unavailable: Some(why),
    };
    let Some(root) = options.asset_dir.clone().or_else(default_asset_root) else {
        return unavailable("cannot determine the OCR asset directory (set --asset-dir)".into());
    };
    let exe = options.ocr_exe.clone().or_else(env_exe_override);
    match LocalRecognizer::open(&root, exe.as_deref(), &options.model, options.directml) {
        Ok(mut local) => run_backend(&mut local, cases),
        Err(why) => unavailable(format!("local-model unavailable: {why}")),
    }
}
