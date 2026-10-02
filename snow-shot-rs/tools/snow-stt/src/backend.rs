//! 识别后端抽象：把“喂音频、取结果、收尾”从具体引擎里隔离出来，便于用 Fake 离屏测试。

use std::path::Path;

/// 后端产生的识别事件（与协议事件一一对应，但不含进程级状态）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SttEvent {
    /// 当前这句话的临时文本。
    Partial(String),
    /// 一句话的定稿文本。
    Final(String),
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

/// 在文件名列表里按前缀挑一个 onnx；`prefer_int8` 决定优先 int8 还是优先非 int8。
fn pick_onnx<'a>(names: &'a [String], prefix: &str, prefer_int8: bool) -> Option<&'a String> {
    let mut hits: Vec<&String> = names
        .iter()
        .filter(|n| {
            let l = n.to_ascii_lowercase();
            l.starts_with(prefix) && l.ends_with(".onnx")
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

/// 列出目录下的文件名并挑选模型文件。
///
/// # 参数
/// - `dir`：模型目录。
///
/// # 返回
/// 模型文件集合；目录不可读或缺文件时返回中文原因。
pub fn discover_model(dir: &Path) -> Result<ModelFiles, String> {
    let rd =
        std::fs::read_dir(dir).map_err(|e| format!("无法读取模型目录 {}: {e}", dir.display()))?;
    let names: Vec<String> = rd
        .filter_map(|e| e.ok())
        .filter_map(|e| e.file_name().into_string().ok())
        .collect();
    pick_model_files(dir, &names)
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
    }
}
