//! 对比结果的行、汇总与 CSV / 表格输出（纯函数，便于测试）。

use crate::runner::BackendRun;

/// 输出里必带的提示：合成样片不代表真实截图。
pub const DISCLAIMER: &str = "NOTE: synthetic samples do NOT represent real screenshots; use this only to compare the engines' relative behaviour.";

/// 一个用例在一个后端上的结果。
#[derive(Debug, Clone, PartialEq)]
pub struct Row {
    /// 用例名。
    pub image: String,
    /// 类别。
    pub category: String,
    /// 后端名。
    pub backend: String,
    /// 期望文本的规整后字符数。
    pub expected_chars: usize,
    /// 编辑距离（识别成功时）。
    pub distance: Option<usize>,
    /// 识别耗时（毫秒，识别成功时）。
    pub ms: Option<f64>,
    /// 识别文本（成功时）。
    pub actual: String,
    /// 失败原因（失败时）。
    pub error: Option<String>,
}

impl Row {
    /// 字符错误率；失败行为 `None`。
    pub fn cer(&self) -> Option<f64> {
        let d = self.distance?;
        Some(
            crate::cer::CerScore {
                distance: d,
                expected_chars: self.expected_chars,
            }
            .cer(),
        )
    }
}

/// 一个后端的汇总。
#[derive(Debug, Clone, PartialEq)]
pub struct Summary {
    /// 后端名。
    pub backend: String,
    /// 成功行数。
    pub ok: usize,
    /// 失败行数。
    pub failed: usize,
    /// 第一张（含冷启动）的耗时。
    pub first_ms: Option<f64>,
    /// 去掉第一张后的平均耗时（只有一张时回落为全部平均）。
    pub avg_ms_warm: Option<f64>,
    /// 各图 CER 的算术平均。
    pub avg_cer: Option<f64>,
    /// 总编辑距离 / 总期望字符数（长文本权重更大）。
    pub micro_cer: Option<f64>,
    /// 被测进程识别前的工作集（MiB）；本地后端为 `None`（worker 尚未启动）。
    pub mem_before_mib: Option<f64>,
    /// 全部识别后的工作集（MiB）。
    pub mem_after_mib: Option<f64>,
    /// 工作集峰值（MiB）。
    pub mem_peak_mib: Option<f64>,
    /// 后端不可用时的原因（此时没有任何行）。
    pub unavailable: Option<String>,
}

/// 字节换算成 MiB。
fn mib(bytes: u64) -> f64 {
    bytes as f64 / (1024.0 * 1024.0)
}

/// 算术平均；空切片为 `None`。
fn mean(values: &[f64]) -> Option<f64> {
    (!values.is_empty()).then(|| values.iter().sum::<f64>() / values.len() as f64)
}

/// 汇总一次后端运行。
///
/// # 参数
/// - `run`：该后端的运行结果。
///
/// # 示例
/// ```ignore
/// let summary = snow_ocr_compare::report::summarize(&run);
/// ```
pub fn summarize(run: &BackendRun) -> Summary {
    let ok_rows: Vec<&Row> = run.rows.iter().filter(|r| r.error.is_none()).collect();
    let times: Vec<f64> = ok_rows.iter().filter_map(|r| r.ms).collect();
    let warm = if times.len() > 1 {
        &times[1..]
    } else {
        &times[..]
    };
    let cers: Vec<f64> = ok_rows.iter().filter_map(|r| r.cer()).collect();
    let total_distance: usize = ok_rows.iter().filter_map(|r| r.distance).sum();
    let total_chars: usize = ok_rows.iter().map(|r| r.expected_chars).sum();
    Summary {
        backend: run.name.clone(),
        ok: ok_rows.len(),
        failed: run.rows.len() - ok_rows.len(),
        first_ms: times.first().copied(),
        avg_ms_warm: mean(warm),
        avg_cer: mean(&cers),
        micro_cer: (total_chars > 0).then(|| total_distance as f64 / total_chars as f64),
        mem_before_mib: run.memory.before_ws.map(mib),
        mem_after_mib: run.memory.after_ws.map(mib),
        mem_peak_mib: run.memory.peak_ws.map(mib),
        unavailable: run.unavailable.clone(),
    }
}

/// 转义 CSV 字段：含逗号、引号、换行时加引号并把引号翻倍；换行用字面量 `\n` 表示以保持单行。
///
/// # 参数
/// - `field`：原始字段。
///
/// # 示例
/// ```
/// assert_eq!(snow_ocr_compare::report::csv_escape("a,b"), "\"a,b\"");
/// assert_eq!(snow_ocr_compare::report::csv_escape("x\ny"), "x\\ny");
/// ```
pub fn csv_escape(field: &str) -> String {
    let flat = field.replace("\r\n", "\\n").replace(['\n', '\r'], "\\n");
    if flat.contains([',', '"']) {
        format!("\"{}\"", flat.replace('"', "\"\""))
    } else {
        flat
    }
}

/// 格式化可选小数；`None` 输出空串（CSV）。
fn opt(value: Option<f64>, digits: usize) -> String {
    value.map(|v| format!("{v:.digits$}")).unwrap_or_default()
}

/// 输出 CSV：第一行是以 `#` 开头的合成样片提示，随后是表头与逐图结果。
///
/// # 参数
/// - `runs`：各后端运行结果。
///
/// # 返回
/// CSV 文本；不可用的后端不产生数据行，原因写在 `# unavailable` 注释行。
pub fn to_csv(runs: &[BackendRun]) -> String {
    let mut out = format!("# {DISCLAIMER}\n");
    out.push_str("image,category,backend,expected_chars,distance,cer,accuracy,ms,error,actual\n");
    for run in runs {
        if let Some(why) = &run.unavailable {
            out.push_str(&format!(
                "# unavailable: {} ({})\n",
                run.name,
                csv_escape(why)
            ));
        }
        for r in &run.rows {
            let cer = r.cer();
            out.push_str(&format!(
                "{},{},{},{},{},{},{},{},{},{}\n",
                csv_escape(&r.image),
                csv_escape(&r.category),
                csv_escape(&r.backend),
                r.expected_chars,
                r.distance.map(|d| d.to_string()).unwrap_or_default(),
                opt(cer, 4),
                opt(cer.map(|c| (1.0 - c).max(0.0)), 4),
                opt(r.ms, 1),
                csv_escape(r.error.as_deref().unwrap_or("")),
                csv_escape(&r.actual),
            ));
        }
    }
    out
}

/// 渲染人读的汇总表（逐图结果 + 各后端平均值）。
///
/// # 参数
/// - `runs`：各后端运行结果。
pub fn render_table(runs: &[BackendRun]) -> String {
    let mut out = format!("{DISCLAIMER}\n\n");
    out.push_str(&format!(
        "{:<22} {:<12} {:>6} {:>5} {:>7} {:>9}  {}\n",
        "image", "backend", "chars", "dist", "CER", "ms", "error"
    ));
    for run in runs {
        for r in &run.rows {
            out.push_str(&format!(
                "{:<22} {:<12} {:>6} {:>5} {:>7} {:>9}  {}\n",
                r.image,
                r.backend,
                r.expected_chars,
                r.distance
                    .map(|d| d.to_string())
                    .unwrap_or_else(|| "-".into()),
                r.cer()
                    .map(|c| format!("{:.1}%", c * 100.0))
                    .unwrap_or_else(|| "-".into()),
                r.ms.map(|m| format!("{m:.1}"))
                    .unwrap_or_else(|| "-".into()),
                r.error.as_deref().unwrap_or(""),
            ));
        }
    }
    out.push_str("\nSummary\n");
    for run in runs {
        let s = summarize(run);
        match &s.unavailable {
            Some(why) => out.push_str(&format!("  {:<12} UNAVAILABLE: {why}\n", s.backend)),
            None => out.push_str(&format!(
                "  {:<12} ok={} failed={} first_ms={} avg_ms_warm={} avg_CER={} micro_CER={} mem_MiB(before/after/peak)={}/{}/{}\n",
                s.backend,
                s.ok,
                s.failed,
                opt(s.first_ms, 1),
                opt(s.avg_ms_warm, 1),
                s.avg_cer.map(|c| format!("{:.2}%", c * 100.0)).unwrap_or_default(),
                s.micro_cer.map(|c| format!("{:.2}%", c * 100.0)).unwrap_or_default(),
                opt(s.mem_before_mib, 1),
                opt(s.mem_after_mib, 1),
                opt(s.mem_peak_mib, 1),
            )),
        }
    }
    out.push_str(
        "\nMemory note: system = this process' working set (WinRT is in-process); \
         local-model = the snow-ocr-process worker's working set (separate process).\n",
    );
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runner::MemoryReport;

    /// 构造一行结果。
    fn row(
        image: &str,
        backend: &str,
        chars: usize,
        distance: Option<usize>,
        ms: Option<f64>,
    ) -> Row {
        Row {
            image: image.into(),
            category: "c".into(),
            backend: backend.into(),
            expected_chars: chars,
            distance,
            ms,
            actual: "t,\"x\"\nz".into(),
            error: distance.is_none().then(|| "boom".to_string()),
        }
    }

    /// 构造一次运行。
    fn run(rows: Vec<Row>, unavailable: Option<&str>) -> BackendRun {
        BackendRun {
            name: "system".into(),
            rows,
            memory: MemoryReport {
                before_ws: Some(100 * 1024 * 1024),
                after_ws: Some(130 * 1024 * 1024),
                peak_ws: Some(140 * 1024 * 1024),
            },
            unavailable: unavailable.map(str::to_string),
        }
    }

    /// 汇总：平均耗时去掉首张、CER 宏平均与微平均、失败行计数、内存换算。
    #[test]
    fn summary_math() {
        let r = run(
            vec![
                row("a", "system", 10, Some(1), Some(300.0)),
                row("b", "system", 30, Some(3), Some(100.0)),
                row("c", "system", 20, Some(0), Some(200.0)),
                row("d", "system", 20, None, None),
            ],
            None,
        );
        let s = summarize(&r);
        assert_eq!((s.ok, s.failed), (3, 1));
        assert_eq!(s.first_ms, Some(300.0));
        assert_eq!(s.avg_ms_warm, Some(150.0));
        assert!((s.avg_cer.expect("avg") - (0.1 + 0.1 + 0.0) / 3.0).abs() < 1e-9);
        assert!((s.micro_cer.expect("micro") - 4.0 / 60.0).abs() < 1e-9);
        assert_eq!(s.mem_after_mib, Some(130.0));
    }

    /// 只有一张时，“去首张平均”回落为该张耗时；没有成功行时全是 None。
    #[test]
    fn summary_edge_cases() {
        let one = summarize(&run(vec![row("a", "system", 5, Some(0), Some(50.0))], None));
        assert_eq!(one.avg_ms_warm, Some(50.0));
        let none = summarize(&run(vec![], Some("no engine")));
        assert_eq!(
            (none.avg_cer, none.micro_cer, none.first_ms),
            (None, None, None)
        );
        assert_eq!(none.unavailable.as_deref(), Some("no engine"));
    }

    /// CSV 转义：逗号与引号加引号，换行写成字面量。
    #[test]
    fn csv_escaping() {
        assert_eq!(csv_escape("plain"), "plain");
        assert_eq!(csv_escape("a,b"), "\"a,b\"");
        assert_eq!(csv_escape("say \"hi\""), "\"say \"\"hi\"\"\"");
        assert_eq!(csv_escape("l1\r\nl2"), "l1\\nl2");
    }

    /// CSV 首行是合成样片提示，表头固定，不可用后端写注释行、失败行带错误。
    #[test]
    fn csv_layout() {
        let csv = to_csv(&[run(
            vec![
                row("a", "system", 10, Some(2), Some(12.34)),
                row("b", "system", 4, None, None),
            ],
            None,
        )]);
        let lines: Vec<&str> = csv.lines().collect();
        assert!(lines[0].starts_with("# NOTE: synthetic"));
        assert_eq!(
            lines[1],
            "image,category,backend,expected_chars,distance,cer,accuracy,ms,error,actual"
        );
        assert!(lines[2].starts_with("a,c,system,10,2,0.2000,0.8000,12.3,,"));
        assert!(lines[3].starts_with("b,c,system,4,,,,,boom,"));
        let down = to_csv(&[BackendRun {
            name: "local-model".into(),
            rows: vec![],
            memory: MemoryReport::default(),
            unavailable: Some("no runtime".into()),
        }]);
        assert!(down.contains("# unavailable: local-model (no runtime)"));
    }

    /// 表格含提示、汇总与不可用标记。
    #[test]
    fn table_mentions_unavailable_and_disclaimer() {
        let table = render_table(&[
            run(vec![row("a", "system", 10, Some(2), Some(12.0))], None),
            BackendRun {
                name: "local-model".into(),
                rows: vec![],
                memory: MemoryReport::default(),
                unavailable: Some("no runtime".into()),
            },
        ]);
        assert!(table.contains("do NOT represent real screenshots"));
        assert!(table.contains("UNAVAILABLE: no runtime"));
        assert!(table.contains("avg_CER=20.00%"));
    }
}
