//! 命令行参数解析（纯函数，便于测试）。

use std::path::PathBuf;

/// 默认本地模型类型。
const DEFAULT_MODEL: &str = "small";

/// 子命令。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Command {
    /// 生成合成样片。
    Gen {
        /// 输出目录。
        out: PathBuf,
    },
    /// 同图对比。
    Run(RunOptions),
}

/// `run` 子命令的选项。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunOptions {
    /// 图片目录（`<名>.png` + `<名>.txt`）。
    pub dir: PathBuf,
    /// CSV 输出路径。
    pub csv: Option<PathBuf>,
    /// 把每个后端识别出的文本逐图写入该目录（`<图名>.<后端>.txt`）。
    pub dump: Option<PathBuf>,
    /// 是否跑系统 OCR。
    pub system: bool,
    /// 是否跑本地模型。
    pub local: bool,
    /// 系统 OCR 的识别语言（BCP-47）；`None` 为用户档语言。
    pub language: Option<String>,
    /// 本地模型资产根目录。
    pub asset_dir: Option<PathBuf>,
    /// 指定 `snow-ocr-process` 可执行文件。
    pub ocr_exe: Option<PathBuf>,
    /// 本地模型类型键。
    pub model: String,
    /// 本地模型是否请求 DirectML。
    pub directml: bool,
}

/// 用法说明。
pub fn usage() -> &'static str {
    "usage:\n  snow-ocr-compare gen --out <dir>\n  snow-ocr-compare run --dir <dir> [--csv <file>] \
     [--backends system,local] [--lang <bcp47>]\n                       [--asset-dir <dir>] \
     [--ocr-exe <file>] [--model small] [--directml] [--dump <dir>]"
}

/// 解析命令行参数（不含程序名）。
///
/// # 参数
/// - `args`：参数列表。
///
/// # 返回
/// 子命令；参数不合法时返回原因。
///
/// # 示例
/// ```
/// use snow_ocr_compare::cli::{Command, parse_args};
/// let cmd = parse_args(&["gen".into(), "--out".into(), "x".into()]).unwrap();
/// assert!(matches!(cmd, Command::Gen { .. }));
/// ```
pub fn parse_args(args: &[String]) -> Result<Command, String> {
    let (sub, rest) = args.split_first().ok_or("missing subcommand")?;
    match sub.as_str() {
        "gen" => parse_gen(rest),
        "run" => parse_run(rest),
        other => Err(format!("unknown subcommand {other}")),
    }
}

/// 把参数倒序放进栈，便于按序弹出。
fn arg_stack(rest: &[String]) -> Vec<String> {
    rest.iter().rev().cloned().collect()
}

/// 解析 `gen` 的选项。
fn parse_gen(rest: &[String]) -> Result<Command, String> {
    let mut out = None;
    let mut pending = arg_stack(rest);
    while let Some(arg) = pending.pop() {
        match arg.as_str() {
            "--out" => out = Some(pending.pop().ok_or("--out needs a value")?),
            other => return Err(format!("unknown option {other}")),
        }
    }
    Ok(Command::Gen {
        out: PathBuf::from(out.ok_or("gen needs --out <dir>")?),
    })
}

/// 解析 `run` 的选项。
fn parse_run(rest: &[String]) -> Result<Command, String> {
    let mut opts = RunOptions {
        dir: PathBuf::new(),
        csv: None,
        dump: None,
        system: true,
        local: true,
        language: None,
        asset_dir: None,
        ocr_exe: None,
        model: DEFAULT_MODEL.to_string(),
        directml: false,
    };
    let mut have_dir = false;
    let mut pending = arg_stack(rest);
    while let Some(arg) = pending.pop() {
        let mut take = |flag: &str| pending.pop().ok_or(format!("{flag} needs a value"));
        match arg.as_str() {
            "--dir" => {
                opts.dir = PathBuf::from(take("--dir")?);
                have_dir = true;
            }
            "--csv" => opts.csv = Some(PathBuf::from(take("--csv")?)),
            "--dump" => opts.dump = Some(PathBuf::from(take("--dump")?)),
            "--lang" => opts.language = Some(take("--lang")?),
            "--asset-dir" => opts.asset_dir = Some(PathBuf::from(take("--asset-dir")?)),
            "--ocr-exe" => opts.ocr_exe = Some(PathBuf::from(take("--ocr-exe")?)),
            "--model" => opts.model = take("--model")?,
            "--directml" => opts.directml = true,
            "--backends" => {
                let list = take("--backends")?;
                let names: Vec<&str> = list.split(',').map(str::trim).collect();
                if let Some(bad) = names.iter().find(|n| !matches!(**n, "system" | "local")) {
                    return Err(format!("unknown backend {bad} (use system,local)"));
                }
                opts.system = names.contains(&"system");
                opts.local = names.contains(&"local");
            }
            other => return Err(format!("unknown option {other}")),
        }
    }
    if !have_dir {
        return Err("run needs --dir <dir>".to_string());
    }
    Ok(Command::Run(opts))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 把字符串切片转成参数列表。
    fn args(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| s.to_string()).collect()
    }

    /// gen 需要 --out。
    #[test]
    fn gen_requires_out() {
        assert_eq!(
            parse_args(&args(&["gen", "--out", "d"])),
            Ok(Command::Gen {
                out: PathBuf::from("d")
            })
        );
        assert!(parse_args(&args(&["gen"])).is_err());
        assert!(parse_args(&args(&["gen", "--out"])).is_err());
    }

    /// run 默认两个后端都开，选项被正确解析。
    #[test]
    fn run_defaults_and_options() {
        let Ok(Command::Run(o)) = parse_args(&args(&["run", "--dir", "d"])) else {
            panic!("应解析成功")
        };
        assert!(o.system && o.local && !o.directml);
        assert_eq!(o.model, "small");
        let full = args(&[
            "run",
            "--dir",
            "d",
            "--backends",
            "system",
            "--lang",
            "en-US",
            "--csv",
            "o.csv",
            "--directml",
        ]);
        let Ok(Command::Run(o)) = parse_args(&full) else {
            panic!("应解析成功")
        };
        assert!(o.system && !o.local && o.directml);
        assert_eq!(o.language.as_deref(), Some("en-US"));
        assert_eq!(o.csv, Some(PathBuf::from("o.csv")));
        assert_eq!(o.dump, None);
    }

    /// `--dump` 取目录；缺少取值被拒绝。
    #[test]
    fn dump_option_takes_a_directory() {
        let Ok(Command::Run(o)) = parse_args(&args(&["run", "--dir", "d", "--dump", "out"])) else {
            panic!("应解析成功")
        };
        assert_eq!(o.dump, Some(PathBuf::from("out")));
        assert!(parse_args(&args(&["run", "--dir", "d", "--dump"])).is_err());
    }

    /// 非法输入给出原因。
    #[test]
    fn invalid_inputs_are_rejected() {
        assert!(parse_args(&[]).is_err());
        assert!(parse_args(&args(&["run"])).is_err());
        assert!(parse_args(&args(&["run", "--dir", "d", "--backends", "cloud"])).is_err());
        assert!(parse_args(&args(&["run", "--dir", "d", "--nope"])).is_err());
        assert!(parse_args(&args(&["bogus"])).is_err());
    }
}
