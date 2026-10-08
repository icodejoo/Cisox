//! 公式识别的纯函数部分（RapidLaTeXOCR 的 Rust 移植）：图像预处理、宽度迭代、自回归解码循环、
//! tokenizer 解码与 LaTeX 后处理。
//!
//! 模型推理由 [`LatexBackend`] 抽象（真实实现是 `snow-latex` 工作进程，见 `latex_service`），
//! 因此本模块可以用假张量离屏测试。预处理与后处理逐段对照上游 `rapid_latex_ocr` 的 Python 实现
//! （`PreProcess.pad` / `minmax_size` / `normalize`、`LaTeXOCR.loop_image_resizer` / `post_process`、
//! `Decoder.__call__`、`TokenizerCls.token2str`）。

use serde_json::Value;

/// 模型输入最大宽度（像素，与上游 `config.yaml` 一致）。
pub const MAX_WIDTH: u32 = 672;
/// 模型输入最大高度。
pub const MAX_HEIGHT: u32 = 192;
/// 模型输入最小边长。
pub const MIN_SIDE: u32 = 32;
/// 边长对齐的倍数。
const DIVISOR: u32 = 32;
/// 宽度迭代里中间高度的上限（正常输入远低于它，只防异常分类结果让高度失控）。
const MAX_ROUND_HEIGHT: u32 = 4096;
/// 宽度迭代的最多轮数。
const MAX_RESIZE_ROUNDS: usize = 10;
/// 起始 token（`[BOS]`）。
pub const BOS_TOKEN: i64 = 1;
/// 结束 token（`[EOS]`）。
pub const EOS_TOKEN: i64 = 2;
/// 解码的最大步数（也是单次喂给解码器的最大序列长度）。
pub const MAX_SEQ_LEN: usize = 512;
/// 二值化阈值。
const THRESHOLD: f64 = 128.0;
/// 归一化均值（灰度，0~1）。
const NORM_MEAN: f32 = 0.7931;
/// 归一化标准差（灰度，0~1）。
const NORM_STD: f32 = 0.1738;
/// 像素最大值。
const PIXEL_MAX: f32 = 255.0;
/// 字节级分词里代表空格的符号。
const SPACE_MARK: char = 'Ġ';

/// 公式识别过程中的失败原因。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LatexRunError {
    /// 输入图像尺寸与像素数对不上。
    InvalidImage,
    /// 图里没有可识别的内容（纯色图）。
    Blank,
    /// 推理后端出错（附原因）。
    Backend(String),
    /// tokenizer 文件不合法（附原因）。
    Tokenizer(String),
}

/// 单通道 8 位灰度图。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Gray {
    /// 宽。
    pub width: u32,
    /// 高。
    pub height: u32,
    /// 行优先像素。
    pub data: Vec<u8>,
}

impl Gray {
    /// 创建纯色图。
    fn filled(width: u32, height: u32, value: u8) -> Self {
        Self {
            width,
            height,
            data: vec![value; width as usize * height as usize],
        }
    }

    /// 读一个像素。
    fn at(&self, x: u32, y: u32) -> u8 {
        self.data[y as usize * self.width as usize + x as usize]
    }
}

/// 送进模型的 `1 x 1 x H x W` 浮点张量。
#[derive(Debug, Clone, PartialEq)]
pub struct LatexTensor {
    /// 形状 `[1, 1, H, W]`。
    pub shape: [usize; 4],
    /// 展平数据。
    pub data: Vec<f32>,
}

/// 推理后端：三个 ONNX 模型的最小调用面。
pub trait LatexBackend {
    /// 宽度分类器：返回整段输出，调用方取 argmax。
    ///
    /// # 参数
    /// - `input`：预处理后的张量。
    fn resize_logits(&mut self, input: &LatexTensor) -> Result<Vec<f32>, String>;

    /// 编码器：编码并把上下文留在后端里，供之后的 [`Self::step`] 使用。
    ///
    /// # 参数
    /// - `input`：预处理后的张量。
    fn encode(&mut self, input: &LatexTensor) -> Result<(), String>;

    /// 解码一步：输入已生成的整段 token，返回最后一个位置的 logits。
    ///
    /// # 参数
    /// - `tokens`：含起始 token 的序列。
    fn step(&mut self, tokens: &[i64]) -> Result<Vec<f32>, String>;
}

/// 把 RGBA 像素转成灰度平面（PIL 的 `L` 公式；忽略 alpha，截图本身不透明）。
///
/// # 参数
/// - `width` / `height`：图像尺寸。
/// - `rgba`：紧凑 RGBA 像素。
///
/// # 返回
/// 灰度图；尺寸为 0 或像素数不符返回 `None`。
pub fn gray_from_rgba(width: u32, height: u32, rgba: &[u8]) -> Option<Gray> {
    let count = (width as usize).checked_mul(height as usize)?;
    if count == 0 || rgba.len() != count.checked_mul(4)? {
        return None;
    }
    let data = rgba
        .chunks_exact(4)
        .map(|p| {
            ((u32::from(p[0]) * 19595 + u32::from(p[1]) * 38470 + u32::from(p[2]) * 7471 + 0x8000)
                >> 16) as u8
        })
        .collect();
    Some(Gray {
        width,
        height,
        data,
    })
}

/// 裁掉空白边并补齐到 32 的倍数（对应上游 `PreProcess.pad`）：统一成“深色字、浅色底”，
/// 找出内容外接框，再贴到 255 底的画布左上角。
///
/// # 参数
/// - `src`：灰度图。
///
/// # 返回
/// 处理后的灰度图；整图是纯色（找不到内容）返回 `None`。
pub fn pad_to_content(src: &Gray) -> Option<Gray> {
    let (min, max) = src
        .data
        .iter()
        .fold((u8::MAX, u8::MIN), |(lo, hi), v| (lo.min(*v), hi.max(*v)));
    if min == max {
        return None;
    }
    let span = f64::from(max - min);
    let data: Vec<f64> = src
        .data
        .iter()
        .map(|v| f64::from(*v - min) / span * 255.0)
        .collect();
    let dark_text = data.iter().sum::<f64>() / data.len() as f64 > THRESHOLD;
    let is_ink = |v: f64| {
        if dark_text {
            v < THRESHOLD
        } else {
            v > THRESHOLD
        }
    };
    let w = src.width as usize;
    let (mut x0, mut y0, mut x1, mut y1) = (usize::MAX, usize::MAX, 0usize, 0usize);
    for (i, v) in data.iter().enumerate() {
        if is_ink(*v) {
            let (x, y) = (i % w, i / w);
            x0 = x0.min(x);
            y0 = y0.min(y);
            x1 = x1.max(x);
            y1 = y1.max(y);
        }
    }
    if x0 == usize::MAX {
        return None;
    }
    let (cw, ch) = ((x1 - x0 + 1) as u32, (y1 - y0 + 1) as u32);
    let (pw, ph) = (
        cw.div_ceil(DIVISOR) * DIVISOR,
        ch.div_ceil(DIVISOR) * DIVISOR,
    );
    let mut out = Gray::filled(pw, ph, 255);
    for y in 0..ch as usize {
        for x in 0..cw as usize {
            let raw = data[(y0 + y) * w + x0 + x];
            let v = if dark_text { raw } else { 255.0 - raw };
            // PIL 的 F -> L：截断到 0..255 后取整（向零）
            out.data[y * pw as usize + x] = v.clamp(0.0, 255.0) as u8;
        }
    }
    Some(out)
}

/// 重采样滤波器。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Filter {
    /// 双线性（放大用）。
    Bilinear,
    /// Lanczos（缩小用）。
    Lanczos,
}

impl Filter {
    /// 滤波器支撑半径。
    fn support(self) -> f64 {
        match self {
            Self::Bilinear => 1.0,
            Self::Lanczos => 3.0,
        }
    }

    /// 滤波器核函数。
    fn kernel(self, x: f64) -> f64 {
        match self {
            Self::Bilinear => (1.0 - x.abs()).max(0.0),
            Self::Lanczos => {
                if x.abs() >= 3.0 {
                    0.0
                } else if x == 0.0 {
                    1.0
                } else {
                    let px = std::f64::consts::PI * x;
                    3.0 * px.sin() * (px / 3.0).sin() / (px * px)
                }
            }
        }
    }
}

/// 一维重采样的权重表：每个输出位置对应 `(起点, 权重)`（与 PIL 的 `precompute_coeffs` 同思路，缩小时拉宽支撑）。
fn coefficients(in_len: u32, out_len: u32, filter: Filter) -> Vec<(usize, Vec<f64>)> {
    let scale = f64::from(in_len) / f64::from(out_len);
    let filter_scale = scale.max(1.0);
    let support = filter.support() * filter_scale;
    (0..out_len)
        .map(|out| {
            let center = (f64::from(out) + 0.5) * scale;
            let start = ((center - support + 0.5).floor().max(0.0)) as usize;
            let end = ((center + support + 0.5).floor().min(f64::from(in_len))) as usize;
            let mut weights: Vec<f64> = (start..end.max(start + 1))
                .map(|x| filter.kernel((x as f64 - center + 0.5) / filter_scale))
                .collect();
            let sum: f64 = weights.iter().sum();
            if sum != 0.0 {
                weights.iter_mut().for_each(|w| *w /= sum);
            }
            (start, weights)
        })
        .collect()
}

/// 沿一个方向重采样（逐行 / 逐列），结果取整并夹到 0..255。
fn resample_pass(src: &Gray, out_len: u32, horizontal: bool, filter: Filter) -> Gray {
    let (in_len, lines) = if horizontal {
        (src.width, src.height)
    } else {
        (src.height, src.width)
    };
    let table = coefficients(in_len, out_len, filter);
    let (out_w, out_h) = if horizontal {
        (out_len, src.height)
    } else {
        (src.width, out_len)
    };
    let mut out = Gray::filled(out_w, out_h, 0);
    for line in 0..lines {
        for (o, (start, weights)) in table.iter().enumerate() {
            let sum: f64 = weights
                .iter()
                .enumerate()
                .map(|(k, w)| {
                    let i = (start + k).min(in_len as usize - 1) as u32;
                    let v = if horizontal {
                        src.at(i, line)
                    } else {
                        src.at(line, i)
                    };
                    w * f64::from(v)
                })
                .sum();
            let v = (sum + 0.5).floor().clamp(0.0, 255.0) as u8;
            let (x, y) = if horizontal {
                (o as u32, line)
            } else {
                (line, o as u32)
            };
            out.data[y as usize * out_w as usize + x as usize] = v;
        }
    }
    out
}

/// 缩放到指定尺寸（尺寸不变的方向不处理，与 PIL 一致）。
///
/// # 参数
/// - `src`：灰度图。
/// - `width` / `height`：目标尺寸（至少 1）。
/// - `filter`：滤波器。
pub fn resize_gray(src: &Gray, width: u32, height: u32, filter: Filter) -> Gray {
    let (width, height) = (width.max(1), height.max(1));
    let mut current = src.clone();
    if width != current.width {
        current = resample_pass(&current, width, true, filter);
    }
    if height != current.height {
        current = resample_pass(&current, height, false, filter);
    }
    current
}

/// 超过最大尺寸时等比缩小，小于最小尺寸时补白（对应上游 `PreProcess.minmax_size`）。
///
/// # 参数
/// - `src`：灰度图。
pub fn minmax_size(src: &Gray) -> Gray {
    let mut img = src.clone();
    let ratio = (f64::from(img.width) / f64::from(MAX_WIDTH))
        .max(f64::from(img.height) / f64::from(MAX_HEIGHT));
    if ratio > 1.0 {
        let w = ((f64::from(img.width) / ratio).floor() as u32).max(1);
        let h = ((f64::from(img.height) / ratio).floor() as u32).max(1);
        img = resize_gray(&img, w, h, Filter::Bilinear);
    }
    let (pw, ph) = (img.width.max(MIN_SIDE), img.height.max(MIN_SIDE));
    if (pw, ph) != (img.width, img.height) {
        let mut canvas = Gray::filled(pw, ph, 255);
        for y in 0..img.height {
            for x in 0..img.width {
                canvas.data[y as usize * pw as usize + x as usize] = img.at(x, y);
            }
        }
        img = canvas;
    }
    img
}

/// 灰度图转归一化张量（对应上游 `normalize` + `transpose_and_four_dim`）。
fn to_tensor(img: &Gray) -> LatexTensor {
    let mean = NORM_MEAN * PIXEL_MAX;
    let recip = (NORM_STD * PIXEL_MAX).recip();
    LatexTensor {
        shape: [1, 1, img.height as usize, img.width as usize],
        data: img
            .data
            .iter()
            .map(|v| (f32::from(*v) - mean) * recip)
            .collect(),
    }
}

/// 一轮预处理（对应上游 `LaTeXOCR.pre_process`）：缩放 → 限制尺寸 → 裁边补齐 → 归一化。
///
/// # 返回
/// 张量与补齐后图像的宽（宽度迭代用它判断是否收敛）。
fn pre_process(
    input: &Gray,
    ratio: f64,
    width: u32,
    height: u32,
) -> Result<(LatexTensor, u32), LatexRunError> {
    let filter = if ratio > 1.0 {
        Filter::Bilinear
    } else {
        Filter::Lanczos
    };
    let resized = resize_gray(input, width, height, filter);
    let padded = pad_to_content(&minmax_size(&resized)).ok_or(LatexRunError::Blank)?;
    Ok((to_tensor(&padded), padded.width))
}

/// 取最大值下标（并列取第一个）。
fn argmax(values: &[f32]) -> Option<usize> {
    values
        .iter()
        .enumerate()
        .fold(None, |best: Option<(usize, f32)>, (i, v)| match best {
            Some((_, b)) if *v <= b => best,
            _ => Some((i, *v)),
        })
        .map(|(i, _)| i)
}

/// 宽度迭代（对应上游 `loop_image_resizer`）：反复让分类器给出最合适的宽度，直到与补齐后的宽度一致。
///
/// # 参数
/// - `backend`：推理后端。
/// - `gray`：原图灰度。
///
/// # 返回
/// 送进编码器的最终张量。
pub fn prepare_tensor(
    backend: &mut dyn LatexBackend,
    gray: &Gray,
) -> Result<LatexTensor, LatexRunError> {
    let padded = pad_to_content(gray).ok_or(LatexRunError::Blank)?;
    let input = minmax_size(&padded);
    let (mut ratio, mut width, mut height) = (1.0f64, input.width, input.height);
    let mut last = None;
    for _ in 0..MAX_RESIZE_ROUNDS {
        height = ((f64::from(height) * ratio) as u32).clamp(1, MAX_ROUND_HEIGHT);
        let (tensor, padded_width) = pre_process(&input, ratio, width, height)?;
        let logits = backend
            .resize_logits(&tensor)
            .map_err(LatexRunError::Backend)?;
        let index = argmax(&logits).ok_or_else(|| {
            LatexRunError::Backend("the width classifier returned nothing".into())
        })?;
        width = (index as u32 + 1) * DIVISOR;
        last = Some(tensor);
        if width == padded_width {
            break;
        }
        ratio = f64::from(width) / f64::from(padded_width);
    }
    last.ok_or_else(|| LatexRunError::Backend("no width round ran".into()))
}

/// 自回归解码（对应上游 `Decoder.__call__`；上游以 1e-5 的温度采样，等价于取最大值，这里直接贪心）。
///
/// # 参数
/// - `backend`：已编码好上下文的后端。
///
/// # 返回
/// 生成的 token（不含起始 token，含遇到的结束 token）。
pub fn decode_loop(backend: &mut dyn LatexBackend) -> Result<Vec<i64>, LatexRunError> {
    let mut out = vec![BOS_TOKEN];
    for _ in 0..MAX_SEQ_LEN {
        let window = &out[out.len().saturating_sub(MAX_SEQ_LEN)..];
        let logits = backend.step(window).map_err(LatexRunError::Backend)?;
        let next = argmax(&logits)
            .ok_or_else(|| LatexRunError::Backend("the decoder returned nothing".into()))?
            as i64;
        out.push(next);
        if next == EOS_TOKEN {
            break;
        }
    }
    Ok(out.split_off(1))
}

/// LaTeX 词表：下标即 token id。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Tokenizer {
    /// id -> token 文本（空串表示该 id 没有词）。
    tokens: Vec<String>,
    /// 特殊 token 的 id（解码时丢弃）。
    specials: Vec<i64>,
}

impl Tokenizer {
    /// 从 `tokenizer.json` 文本解析词表（只读 `model.vocab` 与 `added_tokens`，不依赖 tokenizers 库）。
    ///
    /// # 参数
    /// - `json`：`tokenizer.json` 的内容。
    pub fn from_json(json: &str) -> Result<Self, LatexRunError> {
        let bad = |what: &str| LatexRunError::Tokenizer(what.to_string());
        let root: Value =
            serde_json::from_str(json).map_err(|e| LatexRunError::Tokenizer(e.to_string()))?;
        let vocab = root
            .pointer("/model/vocab")
            .and_then(Value::as_object)
            .ok_or_else(|| bad("model.vocab is missing"))?;
        let mut tokens: Vec<String> = Vec::new();
        for (text, id) in vocab {
            let id = id
                .as_u64()
                .ok_or_else(|| bad("a vocab id is not a number"))? as usize;
            if id > MAX_VOCAB_ID {
                return Err(bad("a vocab id is out of range"));
            }
            if tokens.len() <= id {
                tokens.resize(id + 1, String::new());
            }
            tokens[id] = text.clone();
        }
        let specials = root
            .get("added_tokens")
            .and_then(Value::as_array)
            .map(|list| {
                list.iter()
                    .filter(|t| t.get("special").and_then(Value::as_bool).unwrap_or(false))
                    .filter_map(|t| t.get("id").and_then(Value::as_i64))
                    .collect()
            })
            .unwrap_or_default();
        Ok(Self { tokens, specials })
    }

    /// 词表大小。
    pub fn len(&self) -> usize {
        self.tokens.len()
    }

    /// 词表是否为空。
    pub fn is_empty(&self) -> bool {
        self.tokens.is_empty()
    }

    /// token id 序列转文本：丢弃特殊 token，直接拼接，`Ġ` 还原成空格，去掉首尾空白
    /// （对应上游 `token2str`）。
    ///
    /// # 参数
    /// - `ids`：token id。
    pub fn decode(&self, ids: &[i64]) -> String {
        let mut text = String::new();
        for id in ids {
            if self.specials.contains(id) {
                continue;
            }
            if let Some(token) = usize::try_from(*id).ok().and_then(|i| self.tokens.get(i)) {
                text.push_str(token);
            }
        }
        text.replace(SPACE_MARK, " ").trim().to_string()
    }
}

/// 词表 id 的上限（防止异常文件撑爆内存）。
const MAX_VOCAB_ID: usize = 1_000_000;

/// 需要整体去掉空格的 LaTeX 命令名。
const TEXT_COMMANDS: [&str; 4] = ["operatorname", "mathrm", "text", "mathbf"];

/// 去掉 `\operatorname { .. }` 等命令里的全部空格（上游 `post_process` 的第一步）。
fn squeeze_text_commands(chars: &[char]) -> Vec<char> {
    let n = chars.len();
    let mut out = Vec::with_capacity(n);
    let mut i = 0;
    'scan: while i < n {
        if chars[i] == '\\' {
            for name in TEXT_COMMANDS {
                let name: Vec<char> = name.chars().collect();
                let after = i + 1 + name.len();
                if chars.get(i + 1..after) != Some(name.as_slice()) {
                    continue;
                }
                for take_space in [true, false] {
                    for take_star in [true, false] {
                        let mut p = after;
                        if take_space {
                            if p < n && chars[p].is_whitespace() {
                                p += 1;
                            } else {
                                continue;
                            }
                        }
                        if take_star {
                            if p < n && chars[p] == '*' {
                                p += 1;
                            } else {
                                continue;
                            }
                        }
                        if chars.get(p) == Some(&' ')
                            && chars.get(p + 1) == Some(&'{')
                            && let Some(close) = chars[p + 2..]
                                .iter()
                                .take_while(|c| **c != '\n')
                                .position(|c| *c == '}')
                        {
                            let end = p + 2 + close;
                            out.extend(chars[i..=end].iter().filter(|c| **c != ' '));
                            i = end + 1;
                            continue 'scan;
                        }
                    }
                }
            }
        }
        out.push(chars[i]);
        i += 1;
    }
    out
}

/// 字符是否属于上游的 `[\W_^\d]`（非字母数字下划线、下划线、脱字符或数字）。
fn is_non_letter(c: char) -> bool {
    !(c.is_alphanumeric() || c == '_') || c == '_' || c == '^' || c.is_numeric()
}

/// 字符是否属于上游的 `[a-zA-Z]`。
fn is_letter(c: char) -> bool {
    c.is_ascii_alphabetic()
}

/// 等价于上游的 `re.sub(r"(?!\\ )?(A)\s+?(B)", r"\1\2", ...)`：
/// 左右两个字符之间的空白（最短匹配）被去掉，只保留两端字符。
///
/// # 参数
/// - `guard_backslash_space`：为真时，起点是 `\` 且后面紧跟空格则不匹配（上游的 `(?!\\ )`）。
fn collapse_spaces(
    chars: &[char],
    first: fn(char) -> bool,
    second: fn(char) -> bool,
    guard_backslash_space: bool,
) -> Vec<char> {
    let n = chars.len();
    let mut out = Vec::with_capacity(n);
    let mut i = 0;
    while i < n {
        let guarded = guard_backslash_space && chars[i] == '\\' && chars.get(i + 1) == Some(&' ');
        if first(chars[i]) && !guarded && i + 1 < n && chars[i + 1].is_whitespace() {
            let mut k = i + 2;
            while k < n {
                if second(chars[k]) {
                    break;
                }
                if !chars[k].is_whitespace() {
                    k = n;
                    break;
                }
                k += 1;
            }
            if k < n {
                out.push(chars[i]);
                out.push(chars[k]);
                i = k + 1;
                continue;
            }
        }
        out.push(chars[i]);
        i += 1;
    }
    out
}

/// LaTeX 后处理：去掉模型输出里多余的空格（对应上游 `LaTeXOCR.post_process`）。
///
/// # 参数
/// - `text`：解码得到的文本。
///
/// ```ignore
/// assert_eq!(post_process("\\frac { a + b } { c }"), "\\frac{a+b}{c}");
/// ```
pub fn post_process(text: &str) -> String {
    let chars: Vec<char> = text.chars().collect();
    let mut current = squeeze_text_commands(&chars);
    loop {
        let mut next = collapse_spaces(&current, is_non_letter, is_non_letter, true);
        next = collapse_spaces(&next, is_non_letter, is_letter, true);
        next = collapse_spaces(&next, is_letter, is_non_letter, false);
        if next == current {
            return current.into_iter().collect();
        }
        current = next;
    }
}

/// 把 LaTeX 包成块级公式 `$$ ... $$`（识别结果窗里“带 $$ 复制”用）。
///
/// # 参数
/// - `latex`：纯 LaTeX。
pub fn wrap_display(latex: &str) -> String {
    format!("$$\n{latex}\n$$")
}

/// 整条流水线：灰度 → 宽度迭代 → 编码 → 解码循环 → 词表还原 → 后处理。
///
/// # 参数
/// - `backend`：推理后端。
/// - `tokenizer`：词表。
/// - `width` / `height` / `rgba`：图像。
///
/// # 返回
/// 纯 LaTeX 文本（可能为空串，表示模型没有产出内容）。
///
/// ```ignore
/// let latex = recognize(&mut backend, &tokenizer, w, h, &rgba)?;
/// ```
pub fn recognize(
    backend: &mut dyn LatexBackend,
    tokenizer: &Tokenizer,
    width: u32,
    height: u32,
    rgba: &[u8],
) -> Result<String, LatexRunError> {
    let gray = gray_from_rgba(width, height, rgba).ok_or(LatexRunError::InvalidImage)?;
    let tensor = prepare_tensor(backend, &gray)?;
    backend.encode(&tensor).map_err(LatexRunError::Backend)?;
    let ids = decode_loop(backend)?;
    Ok(post_process(&tokenizer.decode(&ids)))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 在 `w x h` 白底上画一块黑色矩形。
    fn ink_image(w: u32, h: u32, rect: (u32, u32, u32, u32)) -> Gray {
        let mut g = Gray::filled(w, h, 255);
        for y in rect.1..rect.1 + rect.3 {
            for x in rect.0..rect.0 + rect.2 {
                g.data[y as usize * w as usize + x as usize] = 0;
            }
        }
        g
    }

    /// 脚本化的假后端：宽度分类器按给定下标回答，之后按给定 token 序列解码。
    struct Fake {
        /// 宽度分类器每次回答的下标（用完后回答“与输入宽度一致”）。
        resize_answers: Vec<usize>,
        /// 收到的宽度分类器输入形状。
        resize_shapes: Vec<[usize; 4]>,
        /// 收到的编码器输入。
        encoded: Vec<LatexTensor>,
        /// 要依次生成的 token。
        script: Vec<i64>,
        /// 每次 `step` 收到的序列。
        seen: Vec<Vec<i64>>,
        /// 词表大小。
        vocab: usize,
    }

    impl Fake {
        /// 创建假后端。
        fn new(resize_answers: Vec<usize>, script: Vec<i64>) -> Self {
            Self {
                resize_answers,
                resize_shapes: Vec::new(),
                encoded: Vec::new(),
                script,
                seen: Vec::new(),
                vocab: 16,
            }
        }
    }

    impl LatexBackend for Fake {
        fn resize_logits(&mut self, input: &LatexTensor) -> Result<Vec<f32>, String> {
            self.resize_shapes.push(input.shape);
            let index = if self.resize_answers.is_empty() {
                input.shape[3] / 32 - 1
            } else {
                self.resize_answers.remove(0)
            };
            let mut logits = vec![0.0; 21];
            logits[index] = 1.0;
            Ok(logits)
        }

        fn encode(&mut self, input: &LatexTensor) -> Result<(), String> {
            self.encoded.push(input.clone());
            Ok(())
        }

        fn step(&mut self, tokens: &[i64]) -> Result<Vec<f32>, String> {
            self.seen.push(tokens.to_vec());
            let next = self
                .script
                .get(tokens.len() - 1)
                .copied()
                .unwrap_or(EOS_TOKEN);
            let mut logits = vec![0.0; self.vocab];
            logits[next as usize] = 5.0;
            Ok(logits)
        }
    }

    /// 裁边补齐：内容外接框向上取整到 32 的倍数，白底；纯色图没有内容。
    #[test]
    fn pad_crops_to_content_and_rounds_up() {
        let g = ink_image(200, 50, (20, 10, 100, 20));
        let p = pad_to_content(&g).expect("有内容");
        assert_eq!((p.width, p.height), (128, 32));
        assert_eq!(p.at(0, 0), 0, "左上角就是内容");
        assert_eq!(p.at(127, 31), 255, "补出来的边是白色");
        assert!(pad_to_content(&Gray::filled(10, 10, 7)).is_none());
    }

    /// 浅色字深色底会被反相成“深字浅底”。
    #[test]
    fn pad_inverts_light_on_dark() {
        let mut g = Gray::filled(64, 64, 0);
        for y in 10..20 {
            for x in 10..30 {
                g.data[y * 64 + x] = 255;
            }
        }
        let p = pad_to_content(&g).expect("有内容");
        assert_eq!((p.width, p.height), (32, 32));
        assert_eq!(p.at(0, 0), 0, "字变成深色");
        assert_eq!(p.at(31, 31), 255);
    }

    /// 超尺寸等比缩小，过小补白到最小边长。
    #[test]
    fn minmax_scales_down_and_pads_up() {
        let big = Gray::filled(1344, 384, 100);
        let m = minmax_size(&big);
        assert_eq!((m.width, m.height), (672, 192));
        assert!(m.data.iter().all(|v| *v == 100), "纯色缩放后仍是纯色");
        let tall = minmax_size(&Gray::filled(100, 960, 10));
        assert_eq!((tall.width, tall.height), (32, 192), "宽度不足 32 时补白");
        let tiny = minmax_size(&Gray::filled(10, 10, 5));
        assert_eq!((tiny.width, tiny.height), (32, 32));
        assert_eq!(tiny.at(0, 0), 5);
        assert_eq!(tiny.at(20, 20), 255);
    }

    /// 重采样：尺寸不变原样返回，放大时亮度范围不越界且保持左右次序。
    #[test]
    fn resize_keeps_identity_and_order() {
        let g = ink_image(40, 20, (0, 0, 20, 20));
        assert_eq!(resize_gray(&g, 40, 20, Filter::Lanczos), g);
        let up = resize_gray(&g, 80, 40, Filter::Bilinear);
        assert_eq!((up.width, up.height), (80, 40));
        assert_eq!(up.at(2, 20), 0);
        assert_eq!(up.at(78, 20), 255);
        let down = resize_gray(&g, 10, 10, Filter::Lanczos);
        assert!(down.at(1, 5) < 20 && down.at(8, 5) > 235);
    }

    /// 张量按上游公式归一化：白底 255 与黑字 0 各对应固定值。
    #[test]
    fn tensor_normalization() {
        let t = to_tensor(&Gray::filled(32, 32, 255));
        assert_eq!(t.shape, [1, 1, 32, 32]);
        let white = (255.0 - 0.7931 * 255.0) / (0.1738 * 255.0);
        assert!((t.data[0] - white).abs() < 1e-4, "{}", t.data[0]);
        let black = to_tensor(&Gray::filled(32, 32, 0)).data[0];
        assert!((black + 0.7931 / 0.1738).abs() < 1e-3, "{black}");
    }

    /// 宽度迭代：分类器认可当前宽度时一轮收敛；要求别的宽度时继续迭代，最终张量取最后一轮。
    #[test]
    fn width_loop_converges() {
        let g = ink_image(200, 50, (20, 10, 100, 20));
        let mut fake = Fake::new(vec![], vec![]);
        let t = prepare_tensor(&mut fake, &g).expect("成功");
        assert_eq!(fake.resize_shapes, vec![[1, 1, 32, 128]]);
        assert_eq!(t.shape, [1, 1, 32, 128]);

        let mut fake = Fake::new(vec![7], vec![]);
        let t = prepare_tensor(&mut fake, &g).expect("成功");
        assert_eq!(fake.resize_shapes.len(), 2, "第二轮收敛");
        assert_eq!(fake.resize_shapes[0], [1, 1, 32, 128]);
        assert_eq!(&t.shape, fake.resize_shapes.last().unwrap());
        assert_ne!(fake.resize_shapes[1][3], 0);
    }

    /// 宽度迭代最多 10 轮，永远不收敛也会停下。
    #[test]
    fn width_loop_is_bounded() {
        let g = ink_image(200, 50, (20, 10, 100, 20));
        let mut fake = Fake::new(vec![20; 12], vec![]);
        prepare_tensor(&mut fake, &g).expect("成功");
        assert!(fake.resize_shapes.len() <= MAX_RESIZE_ROUNDS);
    }

    /// 纯色图报 `Blank`，坏尺寸报 `InvalidImage`。
    #[test]
    fn blank_and_invalid_images() {
        let tok = Tokenizer::from_json(TOKENIZER_JSON).expect("词表");
        let mut fake = Fake::new(vec![], vec![]);
        let white = vec![255u8; 8 * 8 * 4];
        assert_eq!(
            recognize(&mut fake, &tok, 8, 8, &white),
            Err(LatexRunError::Blank)
        );
        assert_eq!(
            recognize(&mut fake, &tok, 0, 0, &[]),
            Err(LatexRunError::InvalidImage)
        );
        assert_eq!(
            recognize(&mut fake, &tok, 8, 8, &[0; 3]),
            Err(LatexRunError::InvalidImage)
        );
    }

    /// 测试用的最小 `tokenizer.json`。
    const TOKENIZER_JSON: &str = r#"{
        "added_tokens": [
            {"id": 0, "special": true, "content": "[PAD]"},
            {"id": 1, "special": true, "content": "[BOS]"},
            {"id": 2, "special": true, "content": "[EOS]"}
        ],
        "model": {"vocab": {"[PAD]": 0, "[BOS]": 1, "[EOS]": 2, "\\frac": 3, "{": 4, "a": 5, "}": 6, "Ġ": 7, "x": 8, "^": 9, "2": 10}}
    }"#;

    /// 词表解析与还原：特殊 token 丢弃，`Ġ` 变空格，未知 id 跳过，首尾空白去掉。
    #[test]
    fn tokenizer_decodes() {
        let tok = Tokenizer::from_json(TOKENIZER_JSON).expect("词表");
        assert_eq!(tok.len(), 11);
        assert!(!tok.is_empty());
        assert_eq!(tok.decode(&[1, 3, 4, 5, 6, 2, 0]), "\\frac{a}");
        assert_eq!(tok.decode(&[7, 8, 7, 9, 10, 99, -1]), "x ^2");
        assert_eq!(tok.decode(&[]), "");
        assert!(matches!(
            Tokenizer::from_json("{}"),
            Err(LatexRunError::Tokenizer(_))
        ));
        assert!(matches!(
            Tokenizer::from_json("nope"),
            Err(LatexRunError::Tokenizer(_))
        ));
        assert!(matches!(
            Tokenizer::from_json(r#"{"model":{"vocab":{"a":99999999}}}"#),
            Err(LatexRunError::Tokenizer(_))
        ));
    }

    /// 解码循环：逐步喂整段序列，遇到 EOS 停止；永不结束时被步数上限截住。
    #[test]
    fn decode_loop_stops_at_eos_and_at_limit() {
        let mut fake = Fake::new(vec![], vec![5, 8, 2]);
        let ids = decode_loop(&mut fake).expect("成功");
        assert_eq!(ids, vec![5, 8, 2]);
        assert_eq!(fake.seen, vec![vec![1], vec![1, 5], vec![1, 5, 8]]);

        let mut endless = Fake::new(vec![], vec![5; MAX_SEQ_LEN + 8]);
        let ids = decode_loop(&mut endless).expect("成功");
        assert_eq!(ids.len(), MAX_SEQ_LEN);
        assert!(endless.seen.last().unwrap().len() <= MAX_SEQ_LEN);
    }

    /// 整条流水线：假张量 -> `\frac{a}` -> 后处理。
    #[test]
    fn pipeline_with_fake_backend() {
        let tok = Tokenizer::from_json(TOKENIZER_JSON).expect("词表");
        let mut rgba = vec![255u8; 200 * 50 * 4];
        for y in 10..30usize {
            for x in 20..120usize {
                rgba[(y * 200 + x) * 4..(y * 200 + x) * 4 + 3].fill(0);
            }
        }
        let mut fake = Fake::new(vec![], vec![3, 4, 5, 6, 7, 8, 7, 9, 10, 2]);
        let latex = recognize(&mut fake, &tok, 200, 50, &rgba).expect("成功");
        assert_eq!(latex, "\\frac{a}x^2");
        assert_eq!(fake.encoded.len(), 1);
        assert_eq!(fake.encoded[0].shape, [1, 1, 32, 128]);
    }

    /// 后处理与上游 Python `post_process` 逐例一致（期望值由上游正则实现直接跑出）。
    #[test]
    fn post_process_matches_upstream() {
        let cases: &[(&str, &str)] = &[
            ("\\frac { a + b } { c ^ { 2 } }", "\\frac{a+b}{c^{2}}"),
            (
                "\\mathrm { d } x = \\operatorname { sin } \\theta \\, d y",
                "\\mathrm{d}x=\\operatorname{sin}\\theta\\,d y",
            ),
            ("x ^ { 2 } + y ^ { 2 } = r ^ { 2 }", "x^{2}+y^{2}=r^{2}"),
            (
                "\\sum _ { i = 1 } ^ { n } i = \\frac { n ( n + 1 ) } { 2 }",
                "\\sum_{i=1}^{n}i=\\frac{n(n+1)}{2}",
            ),
            ("a \\ b  c   d", "a\\ b c d"),
            ("\\text { hello world } + 1 2 3", "\\text{helloworld}+123"),
            ("E = m c ^ 2 \\\\ x y", "E=m c^2\\\\ x y"),
            ("", ""),
            ("  a b ", " a b "),
            (
                "\\left( \\begin{array} { c c } 1 & 2 \\\\ 3 & 4 \\end{array} \\right)",
                "\\left(\\begin{array}{c c}1&2\\\\ 3&4\\end{array}\\right)",
            ),
            (
                "f ( x ) = \\int _ { 0 } ^ { \\infty } e ^ { - t } d t",
                "f(x)=\\int_{0}^{\\infty}e^{-t}d t",
            ),
            ("a\n\n b  \t c", "a\nb c"),
            (
                "\\mathbf { A B } * \\text * { x y } z",
                "\\mathbf{AB}*\\text*{xy}z",
            ),
        ];
        for (input, want) in cases {
            assert_eq!(post_process(input), *want, "{input:?}");
        }
    }

    /// 块级包装。
    #[test]
    fn display_wrapper() {
        assert_eq!(wrap_display("x^2"), "$$\nx^2\n$$");
    }

    /// RGBA 转灰度用 PIL 的整数公式；尺寸校验。
    #[test]
    fn gray_conversion() {
        let g = gray_from_rgba(2, 1, &[255, 255, 255, 255, 0, 0, 0, 255]).expect("尺寸对");
        assert_eq!(g.data, vec![255, 0]);
        let red = gray_from_rgba(1, 1, &[255, 0, 0, 255]).expect("尺寸对");
        assert_eq!(red.data, vec![76]);
        assert!(gray_from_rgba(2, 2, &[0; 4]).is_none());
    }
}
