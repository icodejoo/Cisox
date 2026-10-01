//! 对比运行器：后端抽象、逐图计时与内存采样。

use crate::cer;
use crate::report::Row;
use crate::samples::Case;
use snow_platform::process_mem::{ProcessMemory, current_process_memory};
use snow_platform::win_ocr;
use std::time::Instant;

/// 被测的识别后端。
pub trait Recognizer {
    /// 后端名（用于输出）。
    fn name(&self) -> &str;

    /// 识别一张图。
    ///
    /// # 参数
    /// - `case`：用例（取其 RGBA 像素）。
    ///
    /// # 返回
    /// 识别出的全文（行以 `\n` 分隔）；失败返回原因。
    fn recognize(&mut self, case: &Case) -> Result<String, String>;

    /// 被测进程当前的内存；进程尚未存在时为 `None`。
    fn memory(&self) -> Option<ProcessMemory>;
}

/// 一个后端的内存采样（字节，工作集）。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct MemoryReport {
    /// 第一张识别前。
    pub before_ws: Option<u64>,
    /// 全部识别后。
    pub after_ws: Option<u64>,
    /// 工作集峰值。
    pub peak_ws: Option<u64>,
}

/// 一个后端的完整运行结果。
#[derive(Debug, Clone, PartialEq)]
pub struct BackendRun {
    /// 后端名。
    pub name: String,
    /// 逐图结果（后端不可用时为空）。
    pub rows: Vec<Row>,
    /// 内存采样。
    pub memory: MemoryReport,
    /// 后端不可用的原因（第一张就失败时记录，后续图不再尝试）。
    pub unavailable: Option<String>,
}

/// 系统 OCR 后端（进程内的 WinRT）。
pub struct SystemRecognizer {
    /// 指定识别语言；`None` 表示按用户档语言。
    pub language: Option<String>,
}

impl Recognizer for SystemRecognizer {
    /// 名字固定为 `system`。
    fn name(&self) -> &str {
        "system"
    }

    /// 调用 `snow-platform::win_ocr`，行以 `\n` 连接。
    fn recognize(&mut self, case: &Case) -> Result<String, String> {
        let out = win_ocr::recognize(
            case.width,
            case.height,
            &case.rgba,
            self.language.as_deref(),
        )
        .map_err(|e| e.to_string())?;
        Ok(out
            .lines
            .iter()
            .map(|l| l.text.as_str())
            .collect::<Vec<_>>()
            .join("\n"))
    }

    /// 本进程的内存（WinRT 在进程内）。
    fn memory(&self) -> Option<ProcessMemory> {
        current_process_memory()
    }
}

/// 让一个后端依次识别全部用例，记录耗时、CER 与内存。
///
/// 第一张就失败视为后端不可用（缺运行时、缺语言包等），直接返回，不再硬跑后续图。
///
/// # 参数
/// - `rec`：被测后端。
/// - `cases`：用例列表。
///
/// # 示例
/// ```ignore
/// let run = run_backend(&mut SystemRecognizer { language: None }, &cases);
/// ```
pub fn run_backend(rec: &mut dyn Recognizer, cases: &[Case]) -> BackendRun {
    let name = rec.name().to_string();
    let before = rec.memory();
    let mut rows = Vec::with_capacity(cases.len());
    for (index, case) in cases.iter().enumerate() {
        let started = Instant::now();
        let outcome = rec.recognize(case);
        let ms = started.elapsed().as_secs_f64() * 1000.0;
        let expected_chars = cer::normalize(&case.expected).len();
        match outcome {
            Ok(actual) => rows.push(Row {
                image: case.name.clone(),
                category: case.category.clone(),
                backend: name.clone(),
                expected_chars,
                distance: Some(cer::score(&case.expected, &actual).distance),
                ms: Some(ms),
                actual,
                error: None,
            }),
            Err(why) if index == 0 => {
                return BackendRun {
                    name,
                    rows,
                    memory: MemoryReport::default(),
                    unavailable: Some(why),
                };
            }
            Err(why) => rows.push(Row {
                image: case.name.clone(),
                category: case.category.clone(),
                backend: name.clone(),
                expected_chars,
                distance: None,
                ms: None,
                actual: String::new(),
                error: Some(why),
            }),
        }
    }
    let after = rec.memory();
    BackendRun {
        name,
        rows,
        memory: MemoryReport {
            before_ws: before.map(|m| m.working_set),
            after_ws: after.map(|m| m.working_set),
            peak_ws: after.map(|m| m.peak_working_set),
        },
        unavailable: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 假后端：按脚本返回结果。
    struct Fake {
        /// 依次返回的结果。
        script: Vec<Result<String, String>>,
    }

    impl Recognizer for Fake {
        /// 固定名字。
        fn name(&self) -> &str {
            "fake"
        }
        /// 取脚本里的下一项。
        fn recognize(&mut self, _case: &Case) -> Result<String, String> {
            self.script.remove(0)
        }
        /// 不提供内存。
        fn memory(&self) -> Option<ProcessMemory> {
            None
        }
    }

    /// 构造用例。
    fn case(name: &str, expected: &str) -> Case {
        Case {
            name: name.into(),
            category: "c".into(),
            width: 1,
            height: 1,
            rgba: vec![0; 4],
            expected: expected.into(),
        }
    }

    /// 正常路径：逐图记录距离与耗时，中途失败只记该行。
    #[test]
    fn rows_record_distance_and_errors() {
        let mut rec = Fake {
            script: vec![Ok("abc".into()), Err("bad".into()), Ok("x y".into())],
        };
        let run = run_backend(
            &mut rec,
            &[case("a", "abd"), case("b", "zz"), case("c", "xy")],
        );
        assert!(run.unavailable.is_none());
        assert_eq!(run.rows.len(), 3);
        assert_eq!(run.rows[0].distance, Some(1));
        assert_eq!(run.rows[1].error.as_deref(), Some("bad"));
        assert_eq!(run.rows[2].distance, Some(0));
        assert!(run.rows[0].ms.is_some() && run.rows[1].ms.is_none());
    }

    /// 第一张就失败：标记后端不可用，不再继续，也不编造数据。
    #[test]
    fn first_failure_marks_backend_unavailable() {
        let mut rec = Fake {
            script: vec![Err("no runtime".into())],
        };
        let run = run_backend(&mut rec, &[case("a", "x"), case("b", "y")]);
        assert_eq!(run.unavailable.as_deref(), Some("no runtime"));
        assert!(run.rows.is_empty());
    }
}
