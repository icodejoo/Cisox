//! 识别后端抽象：把“喂音频、取结果、收尾”从具体引擎里隔离出来，便于用 Fake 离屏测试。

use std::path::Path;

use snow_stt_protocol::ModelKind;

/// 后端产生的识别事件（与协议事件一一对应，但不含进程级状态）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SttEvent {
    /// 当前这句话的临时文本。
    Partial(String),
    /// 一句话的定稿文本。
    Final(String),
    /// 后端自身失败（原因为单行文本）；会话据此发 ERROR 并结束。
    Failed(String),
}

/// 流式识别后端。输入固定为 16kHz 单声道 f32（范围 -1..1）。
pub trait SttBackend {
    /// 推入一块音频（约 320ms），只累积，不要求立即出结果。
    ///
    /// # 参数
    /// - `pcm`：16kHz 单声道样本。
    fn feed(&mut self, pcm: &[f32]);

    /// 解码已喂入的音频并取出这段时间产生的事件；没有变化时返回空。
    ///
    /// # 返回
    /// 按产生顺序排列的事件。
    fn poll(&mut self) -> Vec<SttEvent>;

    /// 结束输入并冲刷尾部，返回剩余事件（通常是最后一句 `Final`）。
    fn finish(&mut self) -> Vec<SttEvent>;
}

/// 流式 transducer 模型目录里挑出来的四个文件（绝对路径文本）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelFiles {
    /// 编码器 onnx。
    pub encoder: String,
    /// 解码器 onnx。
    pub decoder: String,
    /// 连接器 onnx。
    pub joiner: String,
    /// 词表。
    pub tokens: String,
}

/// 在文件名列表里按前缀挑一个 onnx。
fn pick_onnx<'a>(names: &'a [String], prefix: &str, prefer_int8: bool) -> Option<&'a String> {
    pick_onnx_by(names, |l| l.starts_with(prefix), prefer_int8)
}

/// 在文件名列表里挑一个满足条件的 onnx；`prefer_int8` 决定优先 int8 还是优先非 int8。
fn pick_onnx_by(
    names: &[String],
    pred: impl Fn(&str) -> bool,
    prefer_int8: bool,
) -> Option<&String> {
    let mut hits: Vec<&String> = names
        .iter()
        .filter(|n| {
            let l = n.to_ascii_lowercase();
            l.ends_with(".onnx") && pred(&l)
        })
        .collect();
    hits.sort();
    let is_int8 = |n: &&String| n.to_ascii_lowercase().contains("int8");
    hits.iter()
        .copied()
        .find(|n| is_int8(n) == prefer_int8)
        .or_else(|| hits.first().copied())
}

/// 从模型目录的文件名中挑出 encoder/decoder/joiner/tokens。
///
/// 规则：encoder 与 joiner 优先 int8，decoder 优先非 int8，词表固定为 `tokens.txt`。
///
/// # 参数
/// - `dir`：模型目录。
/// - `names`：目录下的文件名（不含路径）。
///
/// # 返回
/// 四个文件的完整路径；缺任何一个返回中文原因。
pub fn pick_model_files(dir: &Path, names: &[String]) -> Result<ModelFiles, String> {
    let full = |n: &String| dir.join(n).to_string_lossy().into_owned();
    let need = |found: Option<&String>, what: &str| {
        found
            .map(full)
            .ok_or_else(|| format!("模型目录缺少 {what}: {}", dir.display()))
    };
    let tokens = names.iter().find(|n| n.eq_ignore_ascii_case("tokens.txt"));
    Ok(ModelFiles {
        encoder: need(pick_onnx(names, "encoder", true), "encoder*.onnx")?,
        decoder: need(pick_onnx(names, "decoder", false), "decoder*.onnx")?,
        joiner: need(pick_onnx(names, "joiner", true), "joiner*.onnx")?,
        tokens: need(tokens, "tokens.txt")?,
    })
}

/// 按模型类型挑出的文件集合（绝对路径文本）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KindFiles {
    /// transducer（流式或离线）：encoder/decoder/joiner/tokens。
    Transducer(ModelFiles),
    /// 单模型文件（Paraformer/SenseVoice/Zipformer-CTC/NeMo-CTC）。
    Single {
        /// 模型 onnx。
        model: String,
        /// 词表。
        tokens: String,
    },
    /// Whisper：编码器、解码器与词表。
    Whisper {
        /// 编码器 onnx。
        encoder: String,
        /// 解码器 onnx。
        decoder: String,
        /// 词表。
        tokens: String,
    },
    /// Moonshine：四件套与词表。
    Moonshine {
        /// 预处理 onnx。
        preprocessor: String,
        /// 编码器 onnx。
        encoder: String,
        /// 无缓存解码器 onnx。
        uncached_decoder: String,
        /// 带缓存解码器 onnx。
        cached_decoder: String,
        /// 词表。
        tokens: String,
    },
}

/// 按模型类型从文件名列表里挑文件。
///
/// 规则：transducer 同 [`pick_model_files`]；Paraformer/SenseVoice/Zipformer-CTC/NeMo-CTC 取
/// `model.int8.onnx`（优先）或 `model.onnx` 加 `tokens.txt`；Whisper 取 `*encoder*`/`*decoder*`
/// （均优先 int8）加 `tokens.txt` 或 `*-tokens.txt`；Moonshine 取
/// preprocess/encode/uncached_decode/cached_decode 加 `tokens.txt`。
///
/// # 参数
/// - `dir`：模型目录。
/// - `names`：目录下的文件名（不含路径）。
/// - `kind`：模型类型。
///
/// # 返回
/// 文件集合；缺文件时返回中文原因。
pub fn pick_kind_files(dir: &Path, names: &[String], kind: ModelKind) -> Result<KindFiles, String> {
    let full = |n: &String| dir.join(n).to_string_lossy().into_owned();
    let need = |found: Option<&String>, what: &str| {
        found
            .map(full)
            .ok_or_else(|| format!("模型目录缺少 {what}: {}", dir.display()))
    };
    let exact = |want: &str| names.iter().find(|n| n.eq_ignore_ascii_case(want));
    match kind {
        ModelKind::OnlineTransducer | ModelKind::OfflineTransducer => {
            pick_model_files(dir, names).map(KindFiles::Transducer)
        }
        ModelKind::OfflineParaformer
        | ModelKind::OfflineSenseVoice
        | ModelKind::OfflineZipformerCtc
        | ModelKind::OfflineNemoCtc => {
            let model = exact("model.int8.onnx").or_else(|| exact("model.onnx"));
            Ok(KindFiles::Single {
                model: need(model, "model.int8.onnx 或 model.onnx")?,
                tokens: need(exact("tokens.txt"), "tokens.txt")?,
            })
        }
        ModelKind::OfflineWhisper => {
            let tokens = names.iter().find(|n| {
                let l = n.to_ascii_lowercase();
                l == "tokens.txt" || l.ends_with("-tokens.txt")
            });
            Ok(KindFiles::Whisper {
                encoder: need(
                    pick_onnx_by(names, |l| l.contains("encoder"), true),
                    "*encoder*.onnx",
                )?,
                decoder: need(
                    pick_onnx_by(names, |l| l.contains("decoder"), true),
                    "*decoder*.onnx",
                )?,
                tokens: need(tokens, "*tokens.txt")?,
            })
        }
        ModelKind::OfflineMoonshine => Ok(KindFiles::Moonshine {
            preprocessor: need(pick_onnx(names, "preprocess", false), "preprocess*.onnx")?,
            encoder: need(pick_onnx(names, "encode", true), "encode*.onnx")?,
            uncached_decoder: need(
                pick_onnx(names, "uncached_decode", true),
                "uncached_decode*.onnx",
            )?,
            cached_decoder: need(
                pick_onnx(names, "cached_decode", true),
                "cached_decode*.onnx",
            )?,
            tokens: need(exact("tokens.txt"), "tokens.txt")?,
        }),
    }
}

/// 列出目录下的文件名。
fn list_names(dir: &Path) -> Result<Vec<String>, String> {
    let rd =
        std::fs::read_dir(dir).map_err(|e| format!("无法读取模型目录 {}: {e}", dir.display()))?;
    Ok(rd
        .filter_map(|e| e.ok())
        .filter_map(|e| e.file_name().into_string().ok())
        .collect())
}

/// 列出目录并按模型类型挑选文件。
///
/// # 参数
/// - `dir`：模型目录。
/// - `kind`：模型类型。
///
/// # 返回
/// 文件集合；目录不可读或缺文件时返回中文原因。
pub fn discover(dir: &Path, kind: ModelKind) -> Result<KindFiles, String> {
    pick_kind_files(dir, &list_names(dir)?, kind)
}

/// 列出目录下的文件名并挑选流式 transducer 模型文件（`discover` 对 `online-transducer` 的包装）。
///
/// # 参数
/// - `dir`：模型目录。
///
/// # 返回
/// 模型文件集合；目录不可读或缺文件时返回中文原因。
pub fn discover_model(dir: &Path) -> Result<ModelFiles, String> {
    pick_model_files(dir, &list_names(dir)?)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 把字符串切片转成文件名列表。
    fn names(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn picks_int8_encoder_and_fp32_decoder() {
        let n = names(&[
            "encoder-epoch-99-avg-1.onnx",
            "encoder-epoch-99-avg-1.int8.onnx",
            "decoder-epoch-99-avg-1.onnx",
            "decoder-epoch-99-avg-1.int8.onnx",
            "joiner-epoch-99-avg-1.onnx",
            "joiner-epoch-99-avg-1.int8.onnx",
            "tokens.txt",
        ]);
        let m = pick_model_files(Path::new("M"), &n).unwrap();
        assert!(m.encoder.ends_with("encoder-epoch-99-avg-1.int8.onnx"));
        assert!(m.decoder.ends_with("decoder-epoch-99-avg-1.onnx"));
        assert!(m.joiner.ends_with("joiner-epoch-99-avg-1.int8.onnx"));
        assert!(m.tokens.ends_with("tokens.txt"));
    }

    #[test]
    fn falls_back_when_only_one_variant() {
        let n = names(&["encoder.onnx", "decoder.onnx", "joiner.onnx", "tokens.txt"]);
        assert!(pick_model_files(Path::new("M"), &n).is_ok());
    }

    #[test]
    fn reports_missing_file() {
        let n = names(&["encoder.onnx", "decoder.onnx", "tokens.txt"]);
        let e = pick_model_files(Path::new("M"), &n).unwrap_err();
        assert!(e.contains("joiner"), "{e}");
        let e = pick_model_files(Path::new("M"), &[]).unwrap_err();
        assert!(e.contains("encoder"), "{e}");
    }

    #[test]
    fn missing_dir_is_error() {
        assert!(discover_model(Path::new("Z:/definitely/not/here")).is_err());
        assert!(
            discover(
                Path::new("Z:/definitely/not/here"),
                ModelKind::OfflineMoonshine
            )
            .is_err()
        );
    }

    #[test]
    fn single_model_prefers_int8() {
        let n = names(&["model.onnx", "model.int8.onnx", "tokens.txt"]);
        for k in [
            ModelKind::OfflineParaformer,
            ModelKind::OfflineSenseVoice,
            ModelKind::OfflineZipformerCtc,
            ModelKind::OfflineNemoCtc,
        ] {
            let Ok(KindFiles::Single { model, tokens }) = pick_kind_files(Path::new("M"), &n, k)
            else {
                panic!("应为单模型");
            };
            assert!(model.ends_with("model.int8.onnx"), "{model}");
            assert!(tokens.ends_with("tokens.txt"));
        }
        let n = names(&["model.onnx", "tokens.txt"]);
        assert!(pick_kind_files(Path::new("M"), &n, ModelKind::OfflineParaformer).is_ok());
    }

    #[test]
    fn single_model_reports_missing() {
        let kind = ModelKind::OfflineSenseVoice;
        let e = pick_kind_files(Path::new("M"), &names(&["tokens.txt"]), kind).unwrap_err();
        assert!(e.contains("model"), "{e}");
        let e = pick_kind_files(Path::new("M"), &names(&["model.onnx"]), kind).unwrap_err();
        assert!(e.contains("tokens"), "{e}");
    }

    #[test]
    fn whisper_picks_int8_pair_and_tokens() {
        let n = names(&[
            "base-encoder.onnx",
            "base-encoder.int8.onnx",
            "base-decoder.onnx",
            "base-decoder.int8.onnx",
            "base-tokens.txt",
        ]);
        let Ok(KindFiles::Whisper {
            encoder,
            decoder,
            tokens,
        }) = pick_kind_files(Path::new("M"), &n, ModelKind::OfflineWhisper)
        else {
            panic!("应为 whisper");
        };
        assert!(encoder.ends_with("base-encoder.int8.onnx"));
        assert!(decoder.ends_with("base-decoder.int8.onnx"));
        assert!(tokens.ends_with("base-tokens.txt"));
        let only_enc = names(&["base-encoder.onnx", "base-tokens.txt"]);
        let e = pick_kind_files(Path::new("M"), &only_enc, ModelKind::OfflineWhisper).unwrap_err();
        assert!(e.contains("decoder"), "{e}");
    }

    #[test]
    fn moonshine_picks_four_files() {
        let n = names(&[
            "preprocess.onnx",
            "encode.int8.onnx",
            "uncached_decode.int8.onnx",
            "cached_decode.int8.onnx",
            "tokens.txt",
        ]);
        let Ok(KindFiles::Moonshine {
            preprocessor,
            encoder,
            uncached_decoder,
            cached_decoder,
            ..
        }) = pick_kind_files(Path::new("M"), &n, ModelKind::OfflineMoonshine)
        else {
            panic!("应为 moonshine");
        };
        assert!(preprocessor.ends_with("preprocess.onnx"));
        assert!(encoder.ends_with("encode.int8.onnx"));
        assert!(uncached_decoder.ends_with("uncached_decode.int8.onnx"));
        // cached_decode 不能误匹配 uncached_decode
        assert!(cached_decoder.ends_with("cached_decode.int8.onnx"));
        assert!(!cached_decoder.contains("uncached"));
        let e = pick_kind_files(
            Path::new("M"),
            &names(&["tokens.txt"]),
            ModelKind::OfflineMoonshine,
        )
        .unwrap_err();
        assert!(e.contains("preprocess"), "{e}");
    }

    #[test]
    fn transducer_kinds_share_rules() {
        let n = names(&["encoder.onnx", "decoder.onnx", "joiner.onnx", "tokens.txt"]);
        for k in [ModelKind::OnlineTransducer, ModelKind::OfflineTransducer] {
            assert!(matches!(
                pick_kind_files(Path::new("M"), &n, k),
                Ok(KindFiles::Transducer(_))
            ));
        }
    }
}
