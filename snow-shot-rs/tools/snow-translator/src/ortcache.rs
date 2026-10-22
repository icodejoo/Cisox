//! TR-23 实验：ORT 预优化模型缓存（预打包权重外置 / ORT 格式内存映射）的产出与加载配置。
//!
//! 结论见 `docs/research/translation-ort-external-weights.md`；本模块目前只给实验与探针用，不进主加载路径。

use std::fs;
use std::path::Path;

use crate::engine::{EngineError, SessionExtra, build_session_with};
use crate::manifest::{FILE_DECODER, FILE_DECODER_DATA, FILE_ENCODER, FILE_ENCODER_DATA, Manifest};
use crate::protocol::ErrorKind;

/// 会话配置键：把预打包的常量权重另存到外置数据文件。
const KEY_SAVE_PREPACKED: &str = "session.save_external_prepacked_constant_initializers";
/// 会话配置键：优化后模型的外置权重文件名。
const KEY_EXTERNAL_INIT_FILE: &str = "session.optimized_model_external_initializers_file_name";
/// 会话配置键：保存模型的格式（`ORT` 即 .ort flatbuffer）。
const KEY_SAVE_FORMAT: &str = "session.save_model_format";
/// 会话配置键：从文件路径加载 .ort 时用内存映射。
const KEY_MMAP_ORT: &str = "session.use_memory_mapped_ort_model";
/// 会话配置键：初始化器直接指向 .ort 字节（零拷贝）。
const KEY_ORT_BYTES_INIT: &str = "session.use_ort_model_bytes_for_initializers";
/// 开关值：开。
const ON: &str = "1";
/// 保存格式取值：ORT。
const FORMAT_ORT: &str = "ORT";

/// 预优化缓存的档位。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum OptVariant {
    /// 带预打包权重的优化 onnx + 外置数据。
    Prepacked,
    /// ORT 格式，加载时内存映射。
    OrtMmap,
}

impl OptVariant {
    /// 编码器/解码器缓存文件的扩展名。
    fn ext(self) -> &'static str {
        match self {
            Self::Prepacked => "onnx",
            Self::OrtMmap => "ort",
        }
    }

    /// 加载该档缓存时要带的会话配置。
    pub(crate) fn load_extra(self) -> SessionExtra {
        let config = match self {
            Self::Prepacked => Vec::new(),
            Self::OrtMmap => vec![(KEY_MMAP_ORT, ON), (KEY_ORT_BYTES_INIT, ON)],
        };
        SessionExtra {
            config,
            ..Default::default()
        }
    }
}

/// 把 `src` 模型离线优化一次，写成缓存目录 `out` 下的一套可直接加载的模型目录。
///
/// # 参数
/// - `src`：原模型目录（含 `model.json`）。
/// - `out`：输出目录（会创建）。
/// - `variant`：缓存档位。
/// - `intra_threads`：产出时用的线程数，限制 CPU 占用。
///
/// # 返回
/// 成功 `Ok(())`；任一步失败返回带类别的错误。
pub(crate) fn produce(
    src: &Path,
    out: &Path,
    variant: OptVariant,
    intra_threads: usize,
) -> Result<(), EngineError> {
    let io = |e: std::io::Error| EngineError::new(ErrorKind::LoadFailed, e.to_string());
    let manifest = Manifest::load(src)?;
    fs::create_dir_all(out).map_err(io)?;
    // 小文件（清单、分词器、配置）照搬，让 out 自成一个模型目录
    for entry in fs::read_dir(src).map_err(io)? {
        let entry = entry.map_err(io)?;
        let name = entry.file_name();
        let name_s = name.to_string_lossy();
        let big = name_s.ends_with(".onnx") || name_s.ends_with(".onnx_data");
        if entry.path().is_file() && !big {
            fs::copy(entry.path(), out.join(&name)).map_err(io)?;
        }
    }
    let mut json: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(out.join("model.json")).map_err(io)?)
            .map_err(|e| EngineError::new(ErrorKind::LoadFailed, e.to_string()))?;
    for key in [FILE_ENCODER, FILE_DECODER] {
        let dst_name = format!("{key}.{}", variant.ext());
        let dst = out.join(&dst_name);
        let mut extra = SessionExtra {
            intra_threads: Some(intra_threads),
            optimized_model_path: Some(dst.clone()),
            ..Default::default()
        };
        let data_name = format!("{dst_name}_data");
        match variant {
            OptVariant::Prepacked => {
                // 外置数据文件名只给文件名，ORT 把它放在优化模型同目录
                extra.config = vec![
                    (KEY_SAVE_PREPACKED, ON),
                    (
                        KEY_EXTERNAL_INIT_FILE,
                        Box::leak(data_name.clone().into_boxed_str()),
                    ),
                ];
            }
            OptVariant::OrtMmap => extra.config = vec![(KEY_SAVE_FORMAT, FORMAT_ORT)],
        }
        let _ = fs::remove_file(&dst);
        let _ = fs::remove_file(out.join(&data_name));
        let built = build_session_with(&manifest.resolve_file(src, key)?, &manifest.execution, &extra)
            .map(drop)
            // 产出后立刻按加载配置重载一次校验，重载不了的部件不进缓存
            .and_then(|()| build_session_with(&dst, &manifest.execution, &variant.load_extra()).map(drop));
        if let Err(e) = built {
            // 合并解码器的 If 子图共享初始化器，ORT 重存后会因重名而无法重载；该部件退回原文件
            eprintln!("PREP {variant:?} {key} 不可缓存，沿用原文件: {e:?}");
            let _ = fs::remove_file(&dst);
            let _ = fs::remove_file(out.join(&data_name));
            for k in [
                key,
                if key == FILE_ENCODER {
                    FILE_ENCODER_DATA
                } else {
                    FILE_DECODER_DATA
                },
            ] {
                let name = manifest.files.get(k).cloned().unwrap_or_default();
                fs::copy(src.join(&name), out.join(&name)).map_err(io)?;
                json["files"][k] = name.into();
            }
            continue;
        }
        json["files"][key] = dst_name.clone().into();
        let data_key = if key == FILE_ENCODER {
            FILE_ENCODER_DATA
        } else {
            FILE_DECODER_DATA
        };
        match variant {
            OptVariant::Prepacked => json["files"][data_key] = data_name.into(),
            OptVariant::OrtMmap => {
                if let Some(files) = json["files"].as_object_mut() {
                    files.remove(data_key);
                }
            }
        }
    }
    fs::write(out.join("model.json"), json.to_string()).map_err(io)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::{Engine, TranslateOptions};
    use crate::sysmem;
    use std::path::PathBuf;
    use std::time::Instant;

    /// 探针用的测试句子。
    const PROBE_SENTENCES: [&str; 3] = [
        "Hello world.",
        "Where is the nearest train station?",
        "The quick brown fox jumps over the lazy dog while the cat watches quietly from the window.",
    ];

    /// 探针：按环境变量加载一档并打印一行 JSON（A=现状，B=预打包，C=ORT mmap）。
    ///
    /// `SNOW_TR_PROBE_DIR` 模型目录；`SNOW_TR_PROBE_VARIANT` A/B/C；`SNOW_TR_PROBE_PREP=1` 只产出缓存；
    /// `SNOW_TR_PROBE_THREADS` 线程数（默认 4）。
    #[test]
    #[ignore = "TR-23 实验探针，需真实模型与 onnxruntime"]
    fn probe_variant() {
        let Some(dir) = std::env::var_os("SNOW_TR_PROBE_DIR").map(PathBuf::from) else {
            return;
        };
        let variant = std::env::var("SNOW_TR_PROBE_VARIANT").unwrap_or_else(|_| "A".into());
        let threads: usize = std::env::var("SNOW_TR_PROBE_THREADS")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(4);
        let opt = match variant.as_str() {
            "B" => Some(OptVariant::Prepacked),
            "C" => Some(OptVariant::OrtMmap),
            _ => None,
        };
        crate::engine::init_runtime().unwrap();
        let cache = dir.join(".opt").join(&variant);
        if std::env::var_os("SNOW_TR_PROBE_PREP").is_some() {
            let t = Instant::now();
            produce(&dir, &cache, opt.expect("A 没有缓存"), threads).unwrap();
            println!("PREP {variant} ms={}", t.elapsed().as_millis());
            return;
        }
        let (load_dir, mut extra) = match opt {
            Some(v) => (cache, v.load_extra()),
            None => (dir, SessionExtra::default()),
        };
        extra.intra_threads = Some(threads);
        let before = sysmem::snapshot();
        let t = Instant::now();
        let mut engine = Engine::load_with(&load_dir, None, &extra).unwrap();
        let load_ms = t.elapsed().as_millis();
        let loaded = sysmem::snapshot();
        // TR-24：`SNOW_TR_PROBE_WARM=1` 时先译一条极短句预热，并记录其耗时与内存
        let warm_ms = std::env::var_os("SNOW_TR_PROBE_WARM").map(|_| {
            let t = Instant::now();
            engine
                .translate("Hi.", "en", "zh-CN", &TranslateOptions::default())
                .unwrap();
            t.elapsed().as_millis()
        });
        let warmed = sysmem::snapshot();
        let mut outs = Vec::new();
        let mut first_ms = 0;
        for (i, s) in PROBE_SENTENCES.iter().enumerate() {
            let t = Instant::now();
            outs.push(
                engine
                    .translate(s, "en", "zh-CN", &TranslateOptions::default())
                    .unwrap(),
            );
            if i == 0 {
                first_ms = t.elapsed().as_millis();
            }
        }
        let after = sysmem::snapshot();
        println!(
            "PROBE {}",
            serde_json::json!({
                "variant": variant, "load_ms": load_ms, "first_translate_ms": first_ms,
                "ws_before": before.working_set, "ws_loaded": loaded.working_set, "priv_loaded": loaded.private_bytes,
                "ws_after": after.working_set, "priv_after": after.private_bytes, "outs": outs,
                "warm_ms": warm_ms, "ws_warmed": warmed.working_set, "priv_warmed": warmed.private_bytes,
            })
        );
    }
}
