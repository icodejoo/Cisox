//! “默认模型”下拉的数据：扫描各功能的模型目录，列出已下载且能用的模型，并按选中值解析出实际路径。
//!
//! 一个文件夹 = 一个模型，一个文件 = 一个模型；显示名就是文件（夹）名。
//! OCR 只认清单里已知档位的文件夹（det / rec / dict 齐全），表格认含 `.onnx` 的文件夹或单个 `.onnx` 文件，
//! 公式认四个文件齐全的文件夹。选中项为空或已不存在时回退到首选项或第一个可用的。

use crate::latex_assets::check_model_dir;
use crate::ocr_assets::{find_model_dir, manifest};
use std::path::{Path, PathBuf};

/// 表格模型文件的扩展名（不含点）。
const ONNX_EXT: &str = "onnx";

/// 下拉里的一个可选模型。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PickOption {
    /// 写入配置的值（OCR 为档位键，其余为文件（夹）名）。
    pub value: String,
    /// 显示名（文件夹名或文件名）。
    pub label: String,
    /// 实际路径（OCR / 公式为模型文件夹，表格为 `.onnx` 文件）。
    pub path: PathBuf,
}

/// 读配置里的模型选择键（字符串，去首尾空白；不是字符串或没有时为空串，即自动）。
///
/// # 参数
/// - `document`：配置文档。
/// - `key`：模型选择键（如 `KEY_TABLE_MODEL`）。
pub fn selected_from_document(
    document: &snow_config::document::ConfigDocument,
    key: &str,
) -> String {
    match document.value(key) {
        serde_json::Value::String(s) => s.trim().to_string(),
        _ => String::new(),
    }
}

/// 文件（夹）名；取不到时为空串。
fn name_of(path: &Path) -> String {
    path.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default()
}

/// 读目录并按名字排序，跳过隐藏项；目录不存在为空。
fn sorted_entries(dir: &Path) -> Vec<PathBuf> {
    let mut paths: Vec<PathBuf> = std::fs::read_dir(dir)
        .map(|read| read.filter_map(Result::ok).map(|e| e.path()).collect())
        .unwrap_or_default();
    paths.retain(|p| !name_of(p).starts_with('.'));
    paths.sort();
    paths
}

/// 文件是否存在且非空。
fn non_empty_file(path: &Path) -> bool {
    std::fs::metadata(path).is_ok_and(|m| m.is_file() && m.len() > 0)
}

/// 路径是否 `.onnx` 文件（不分大小写）。
fn is_onnx(path: &Path) -> bool {
    path.extension()
        .is_some_and(|e| e.to_string_lossy().eq_ignore_ascii_case(ONNX_EXT))
}

/// 按值去重，先出现的留下。
fn dedupe(options: Vec<PickOption>) -> Vec<PickOption> {
    let mut seen = std::collections::HashSet::new();
    options
        .into_iter()
        .filter(|o| seen.insert(o.value.clone()))
        .collect()
}

/// OCR：清单里每个已知档位，只要在读取目录里找到文件齐全的文件夹就列出（值是档位键，显示名是文件夹名）。
///
/// # 参数
/// - `dirs`：OCR 模型读取目录（统一目录在前）。
///
/// # 返回
/// 与清单档位顺序一致的选项；认不出结构的文件夹不列。
pub fn ocr_options(dirs: &[PathBuf]) -> Vec<PickOption> {
    let Ok(manifest) = manifest() else {
        return Vec::new();
    };
    manifest
        .models
        .iter()
        .filter_map(|model| {
            let path = find_model_dir(dirs, model)?;
            Some(PickOption {
                value: model.kind.clone(),
                label: name_of(&path),
                path,
            })
        })
        .collect()
}

/// 表格：含非空 `.onnx` 的文件夹、或直接放在目录里的非空 `.onnx` 文件各算一个模型。
///
/// # 参数
/// - `dirs`：表格模型读取目录（统一目录在前）。
/// - `preferred_file`：文件夹里优先选用的模型文件名（如 `slanet-plus.onnx`）。
///
/// # 返回
/// 选项（值与显示名都是文件（夹）名；`path` 是 `.onnx` 文件）；同名只留先出现的。
pub fn table_options(dirs: &[PathBuf], preferred_file: &str) -> Vec<PickOption> {
    let mut options = Vec::new();
    for dir in dirs {
        for path in sorted_entries(dir) {
            let model_file = if path.is_dir() {
                let inside = path.join(preferred_file);
                if non_empty_file(&inside) {
                    Some(inside)
                } else {
                    sorted_entries(&path)
                        .into_iter()
                        .find(|p| is_onnx(p) && non_empty_file(p))
                }
            } else {
                (is_onnx(&path) && non_empty_file(&path)).then(|| path.clone())
            };
            if let Some(file) = model_file {
                let name = name_of(&path);
                options.push(PickOption {
                    value: name.clone(),
                    label: name,
                    path: file,
                });
            }
        }
    }
    dedupe(options)
}

/// 公式：四个文件齐全的文件夹算一个模型；读取目录本身四个文件齐全时也算一个（显示名是目录名）。
///
/// # 参数
/// - `dirs`：公式模型读取目录（自设目录在前）。
///
/// # 返回
/// 选项（值与显示名都是文件夹名；`path` 是模型文件夹）。
pub fn latex_options(dirs: &[PathBuf]) -> Vec<PickOption> {
    let mut options = Vec::new();
    for dir in dirs {
        let candidates = std::iter::once(dir.clone())
            .chain(sorted_entries(dir).into_iter().filter(|p| p.is_dir()));
        for path in candidates {
            if check_model_dir(&path).is_ok() {
                let name = name_of(&path);
                options.push(PickOption {
                    value: name.clone(),
                    label: name,
                    path,
                });
            }
        }
    }
    dedupe(options)
}

/// 按选中值解析：选中项存在就用它；为空或已不存在时回退到首选项（`prefer` 里靠前的优先），再回退到第一个可用的。
///
/// # 参数
/// - `options`：可用选项。
/// - `selected`：配置里的选中值（空白视为未选）。
/// - `prefer`：回退时优先的值。
///
/// # 返回
/// 选中的选项；没有任何可用选项返回 `None`。
///
/// # 示例
/// ```ignore
/// let chosen = pick(&options, "", &["slanet-plus"]);
/// ```
pub fn pick<'a>(
    options: &'a [PickOption],
    selected: &str,
    prefer: &[&str],
) -> Option<&'a PickOption> {
    let selected = selected.trim();
    options
        .iter()
        .find(|o| !selected.is_empty() && o.value == selected)
        .or_else(|| {
            prefer
                .iter()
                .find_map(|want| options.iter().find(|o| o.value == *want))
        })
        .or_else(|| options.first())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::latex_assets::MODEL_FILES;

    /// 唯一临时目录。
    fn temp_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "cisox-model-pick-{tag}-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("建目录");
        dir
    }

    /// 在目录里放四个公式模型文件（稀疏占位到下限大小）。
    fn put_latex(dir: &Path) {
        std::fs::create_dir_all(dir).expect("建");
        for f in MODEL_FILES {
            std::fs::File::create(dir.join(f.name))
                .and_then(|h| h.set_len(f.min_size))
                .expect("占位");
        }
    }

    /// OCR：只列文件齐全的已知档位，显示名是文件夹名，值是档位键；统一目录和旧位置都算，认不出的文件夹不列。
    #[test]
    fn ocr_lists_only_complete_known_models() {
        let a = temp_dir("ocr-a");
        let b = temp_dir("ocr-b");
        let m = manifest().expect("清单");
        let small = m.models.iter().find(|x| x.kind == "small").expect("small");
        let tiny = m
            .models
            .iter()
            .find(|x| x.kind == "extra_small")
            .expect("tiny");
        let put = |dir: &Path, model: &crate::ocr_assets::ModelSpec, skip_last: bool| {
            std::fs::create_dir_all(dir.join(&model.id)).expect("建");
            let n = model.files.len() - usize::from(skip_last);
            for f in &model.files[..n] {
                std::fs::write(dir.join(&model.id).join(&f.name), b"x").expect("写");
            }
        };
        put(&a, small, false);
        put(&b, tiny, true);
        std::fs::create_dir_all(a.join("mystery-folder")).expect("建");
        let options = ocr_options(&[a.clone(), b.clone()]);
        assert_eq!(options.len(), 1, "缺文件的档位和认不出的文件夹都不列");
        assert_eq!(options[0].value, "small");
        assert_eq!(options[0].label, small.id);
        put(&b, tiny, false);
        let values: Vec<_> = ocr_options(&[a.clone(), b.clone()])
            .into_iter()
            .map(|o| o.value)
            .collect();
        assert_eq!(values, vec!["extra_small", "small"], "按清单档位顺序");
        for d in [a, b] {
            let _ = std::fs::remove_dir_all(&d);
        }
    }

    /// 表格：含 onnx 的文件夹与单个 onnx 文件各算一个，空文件和无 onnx 的文件夹不算；同名只留先出现的。
    #[test]
    fn table_lists_folders_and_loose_onnx() {
        let a = temp_dir("table-a");
        let b = temp_dir("table-b");
        std::fs::create_dir_all(a.join("slanet-plus")).expect("建");
        std::fs::write(a.join("slanet-plus").join("slanet-plus.onnx"), b"x").expect("写");
        std::fs::create_dir_all(a.join("no-model")).expect("建");
        std::fs::write(a.join("no-model").join("readme.txt"), b"x").expect("写");
        std::fs::write(a.join("other.onnx"), b"x").expect("写");
        std::fs::write(a.join("empty.onnx"), b"").expect("写");
        std::fs::write(a.join(".hidden.onnx"), b"x").expect("写");
        std::fs::create_dir_all(b.join("slanet-plus")).expect("建");
        std::fs::write(b.join("slanet-plus").join("slanet-plus.onnx"), b"yy").expect("写");
        let options = table_options(&[a.clone(), b.clone()], "slanet-plus.onnx");
        let names: Vec<_> = options.iter().map(|o| o.label.as_str()).collect();
        assert_eq!(names, vec!["other.onnx", "slanet-plus"]);
        assert!(options[1].path.starts_with(&a), "同名取先出现的目录");
        assert!(options[1].path.ends_with("slanet-plus.onnx"));
        for d in [a, b] {
            let _ = std::fs::remove_dir_all(&d);
        }
    }

    /// 公式：四个文件齐全的文件夹才算；目录本身齐全也算一个。
    #[test]
    fn latex_lists_complete_folders() {
        let root = temp_dir("latex");
        put_latex(&root.join("good"));
        std::fs::create_dir_all(root.join("bad")).expect("建");
        std::fs::write(root.join("bad").join("encoder.onnx"), b"x").expect("写");
        let options = latex_options(std::slice::from_ref(&root));
        assert_eq!(options.len(), 1);
        assert_eq!(options[0].value, "good");
        put_latex(&root);
        let names: Vec<_> = latex_options(std::slice::from_ref(&root))
            .into_iter()
            .map(|o| o.label)
            .collect();
        assert_eq!(names.len(), 2, "目录自身齐全也算一个：{names:?}");
        let _ = std::fs::remove_dir_all(&root);
    }

    /// 解析：选中项优先；空或不存在时回退首选项，再回退第一个；没有选项为空。
    #[test]
    fn pick_prefers_selected_then_preferred_then_first() {
        let opt = |v: &str| PickOption {
            value: v.into(),
            label: v.into(),
            path: PathBuf::from(v),
        };
        let options = vec![opt("a"), opt("slanet-plus"), opt("z")];
        assert_eq!(
            pick(&options, "z", &["slanet-plus"]).map(|o| &o.value[..]),
            Some("z")
        );
        assert_eq!(
            pick(&options, "", &["slanet-plus"]).map(|o| &o.value[..]),
            Some("slanet-plus")
        );
        assert_eq!(
            pick(&options, "gone", &["nope"]).map(|o| &o.value[..]),
            Some("a")
        );
        assert_eq!(pick(&options, " z ", &[]).map(|o| &o.value[..]), Some("z"));
        assert!(pick(&[], "x", &["y"]).is_none());
    }
}
