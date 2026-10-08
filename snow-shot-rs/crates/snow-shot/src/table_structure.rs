//! 表格结构识别的纯逻辑：图像预处理、SLANet_plus 输出解码、与 OCR 文字框匹配、HTML / Markdown / TSV 输出。
//!
//! 不依赖界面与进程，全部可离屏测试。推理本身在 `snow-table` 工作进程里完成（见 `table_service`）。
//! 预处理、解码与匹配步骤对齐 RapidTable（Apache-2.0）对 SLANet_plus 的做法：
//! 最长边缩放到 488 → BGR 通道归一化 → 右下补零到 488x488；结构输出按词表逐步 argmax，
//! `<td` / `<td></td>` 步对应一个单元格框；文字框按 IoU（相同则按距离）归入单元格。

use image::{RgbaImage, imageops};
use serde::Deserialize;

/// 模型输入边长。
pub const MODEL_SIDE: usize = 488;
/// 通道数。
const CHANNELS: usize = 3;
/// 归一化均值（按 BGR 通道顺序，与 RapidTable 一致）。
const MEAN: [f32; CHANNELS] = [0.485, 0.456, 0.406];
/// 归一化标准差（按 BGR 通道顺序）。
const STD: [f32; CHANNELS] = [0.229, 0.224, 0.225];
/// 序列起始标记。
const TOKEN_BEGIN: &str = "sos";
/// 序列结束标记。
const TOKEN_END: &str = "eos";
/// 无跨度单元格标记。
const TOKEN_PLAIN_CELL: &str = "<td></td>";
/// 带跨度单元格的起始标记。
const TOKEN_CELL_OPEN: &str = "<td";
/// 被并入 [`TOKEN_PLAIN_CELL`] 的旧标记。
const TOKEN_CELL_LEGACY: &str = "<td>";
/// 行起始标记。
const TOKEN_ROW_OPEN: &str = "<tr>";
/// 行结束标记。
const TOKEN_ROW_CLOSE: &str = "</tr>";
/// 单元格框的坐标个数（四个角点）。
const BOX_COORDS: usize = 8;
/// IoU 小于它视为没有交集（与 RapidTable 的 `0.1**8` 相同）。
const MIN_IOU: f32 = 1e-8;

/// 预处理结果。
#[derive(Debug, Clone, PartialEq)]
pub struct PreparedInput {
    /// `1 x 3 x 488 x 488` 展平的 CHW 张量。
    pub tensor: Vec<f32>,
    /// 缩放比例（模型输入像素 / 原图像素）。
    pub scale: f32,
}

/// 把 RGBA 图预处理成模型输入。
///
/// # 参数
/// - `width` / `height`：图像尺寸。
/// - `rgba`：紧凑 RGBA 像素，长度须为 `宽 * 高 * 4`。
///
/// # 返回
/// 张量与缩放比例；尺寸为 0 或像素数不符返回 `None`。
///
/// ```ignore
/// let input = prepare_input(w, h, &rgba).unwrap();
/// assert_eq!(input.tensor.len(), 3 * 488 * 488);
/// ```
pub fn prepare_input(width: u32, height: u32, rgba: &[u8]) -> Option<PreparedInput> {
    let expected = (width as usize)
        .checked_mul(height as usize)?
        .checked_mul(4)?;
    if width == 0 || height == 0 || rgba.len() != expected {
        return None;
    }
    let scale = MODEL_SIDE as f32 / width.max(height) as f32;
    let resized_w = ((width as f32 * scale) as u32).clamp(1, MODEL_SIDE as u32);
    let resized_h = ((height as f32 * scale) as u32).clamp(1, MODEL_SIDE as u32);
    let source = RgbaImage::from_raw(width, height, rgba.to_vec())?;
    let resized = imageops::resize(
        &source,
        resized_w,
        resized_h,
        imageops::FilterType::Triangle,
    );
    let plane = MODEL_SIDE * MODEL_SIDE;
    let mut tensor = vec![0.0f32; CHANNELS * plane];
    for (x, y, px) in resized.enumerate_pixels() {
        // 通道 0 放 B、1 放 G、2 放 R（RapidTable 直接对 OpenCV 的 BGR 图用这组均值方差）
        let bgr = [px[2], px[1], px[0]];
        let at = y as usize * MODEL_SIDE + x as usize;
        for (c, value) in bgr.iter().enumerate() {
            tensor[c * plane + at] = (*value as f32 / 255.0 - MEAN[c]) / STD[c];
        }
    }
    Some(PreparedInput { tensor, scale })
}

/// 工作进程返回的一个输出张量。
#[derive(Debug, Clone, Deserialize, PartialEq)]
pub struct WorkerTensor {
    /// 输出名。
    pub name: String,
    /// 形状。
    pub shape: Vec<i64>,
    /// 展平的 `f32` 数据。
    pub data: Vec<f32>,
}

/// 工作进程的完整响应。
#[derive(Debug, Clone, Deserialize, PartialEq)]
pub struct WorkerOutput {
    /// 模型元数据里的结构词表（按行分隔）。
    pub character: String,
    /// 全部输出张量。
    pub outputs: Vec<WorkerTensor>,
}

/// 解析工作进程 stdout 的 JSON。
///
/// # 参数
/// - `bytes`：stdout 全部内容。
///
/// # 返回
/// 响应；格式不对返回解析器给出的原因（技术信息）。
pub fn parse_worker_output(bytes: &[u8]) -> Result<WorkerOutput, String> {
    serde_json::from_slice(bytes).map_err(|e| e.to_string())
}

/// 解码后的表格结构。
#[derive(Debug, Clone, PartialEq)]
pub struct Structure {
    /// 结构标记序列（不含起止标记）。
    pub tokens: Vec<String>,
    /// 每个单元格的框 `[x1, y1, x2, y2]`（原图像素）；模型给出全零占位框时为 `None`。
    pub cell_boxes: Vec<Option<[f32; 4]>>,
    /// 平均置信度。
    pub score: f32,
}

/// 构造完整词表：`sos` + 模型词表（补 `<td></td>`、去 `<td>`） + `eos`。
///
/// # 参数
/// - `character`：模型元数据里的词表文本。
pub fn build_vocab(character: &str) -> Vec<String> {
    let mut list: Vec<String> = character.lines().map(str::to_string).collect();
    if !list.iter().any(|t| t == TOKEN_PLAIN_CELL) {
        list.push(TOKEN_PLAIN_CELL.to_string());
    }
    list.retain(|t| t != TOKEN_CELL_LEGACY);
    let mut vocab = Vec::with_capacity(list.len() + 2);
    vocab.push(TOKEN_BEGIN.to_string());
    vocab.extend(list);
    vocab.push(TOKEN_END.to_string());
    vocab
}

/// 是否为一个单元格的起始标记（每个都对应一个框）。
fn is_cell_token(token: &str) -> bool {
    token == TOKEN_CELL_OPEN || token == TOKEN_PLAIN_CELL || token == TOKEN_CELL_LEGACY
}

/// 解码工作进程输出为表格结构。
///
/// # 参数
/// - `output`：工作进程响应（两个输出：框回归 `[1,N,8]` 与结构概率 `[1,N,词表长]`）。
/// - `scale`：预处理返回的缩放比例（用来把框还原到原图像素）。
///
/// # 返回
/// 结构；输出张量缺失或形状不符返回原因（技术信息）。
pub fn decode_structure(output: &WorkerOutput, scale: f32) -> Result<Structure, String> {
    let vocab = build_vocab(&output.character);
    let probs = output
        .outputs
        .iter()
        .find(|t| t.shape.len() == 3 && t.shape[2] as usize == vocab.len())
        .ok_or_else(|| format!("no output with vocabulary width {}", vocab.len()))?;
    let locs = output
        .outputs
        .iter()
        .find(|t| t.shape.len() == 3 && t.shape[2] as usize == BOX_COORDS)
        .ok_or("no box regression output")?;
    let steps = probs.shape[1].max(0) as usize;
    let width = vocab.len();
    if probs.data.len() < steps * width || locs.data.len() < steps * BOX_COORDS {
        return Err("output data is shorter than its shape".to_string());
    }
    let begin = 0usize;
    let end = vocab.len() - 1;
    let to_pixels = MODEL_SIDE as f32 / scale;
    let mut tokens = Vec::new();
    let mut cell_boxes = Vec::new();
    let mut score_sum = 0.0f32;
    for step in 0..steps {
        let row = &probs.data[step * width..(step + 1) * width];
        let (best, best_prob) =
            row.iter().enumerate().fold(
                (0usize, f32::MIN),
                |acc, (i, &p)| if p > acc.1 { (i, p) } else { acc },
            );
        if step > 0 && best == end {
            break;
        }
        if best == begin || best == end {
            continue;
        }
        let token = &vocab[best];
        if is_cell_token(token) {
            let raw = &locs.data[step * BOX_COORDS..(step + 1) * BOX_COORDS];
            cell_boxes.push(corner_box(raw, to_pixels));
        }
        tokens.push(token.clone());
        score_sum += best_prob;
    }
    let score = if tokens.is_empty() {
        0.0
    } else {
        score_sum / tokens.len() as f32
    };
    Ok(Structure {
        tokens,
        cell_boxes,
        score,
    })
}

/// 把 8 个归一化坐标（四个角点）换成原图像素的外接框；全零占位框返回 `None`。
fn corner_box(raw: &[f32], to_pixels: f32) -> Option<[f32; 4]> {
    if raw.iter().all(|v| *v == 0.0) {
        return None;
    }
    let xs = raw.iter().step_by(2).map(|v| v * to_pixels);
    let ys = raw.iter().skip(1).step_by(2).map(|v| v * to_pixels);
    let (min_x, max_x) = min_max(xs);
    let (min_y, max_y) = min_max(ys);
    Some([min_x, min_y, max_x, max_y])
}

/// 迭代器的最小与最大值。
fn min_max(values: impl Iterator<Item = f32>) -> (f32, f32) {
    values.fold((f32::MAX, f32::MIN), |(lo, hi), v| (lo.min(v), hi.max(v)))
}

/// 表格里的一个单元格。
#[derive(Debug, Clone, PartialEq)]
pub struct TableCell {
    /// 起始行（从 0 起）。
    pub row: usize,
    /// 起始列（从 0 起）。
    pub col: usize,
    /// 占几行。
    pub row_span: usize,
    /// 占几列。
    pub col_span: usize,
    /// 单元格框（原图像素）；没有则无法匹配文字。
    pub bbox: Option<[f32; 4]>,
    /// 单元格文字。
    pub text: String,
}

/// 还原出的表格。
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Table {
    /// 总行数。
    pub rows: usize,
    /// 总列数。
    pub cols: usize,
    /// 全部单元格（按出现顺序）。
    pub cells: Vec<TableCell>,
}

/// 读取标记里 `name="N"` 的数字（如 ` colspan="2"`）。
fn span_value(token: &str, name: &str) -> Option<usize> {
    let rest = token.split_once(name)?.1.strip_prefix('=')?;
    rest.trim_matches(|c: char| c == '"' || c == '\'' || c.is_whitespace())
        .parse::<usize>()
        .ok()
        .map(|v| v.max(1))
}

/// 由结构标记与单元格框还原行列（含跨行跨列）。
///
/// # 参数
/// - `tokens`：结构标记序列。
/// - `boxes`：按单元格出现顺序的框。
///
/// # 返回
/// 表格（文字尚未填充）。
pub fn layout_cells(tokens: &[String], boxes: &[Option<[f32; 4]>]) -> Table {
    let mut table = Table::default();
    let mut occupied = std::collections::HashSet::new();
    let (mut row, mut col) = (0usize, 0usize);
    let mut cell_index = 0usize;
    let mut i = 0usize;
    while i < tokens.len() {
        let token = tokens[i].as_str();
        if token == TOKEN_ROW_OPEN {
            col = 0;
        } else if token == TOKEN_ROW_CLOSE {
            row += 1;
        } else if token.starts_with(TOKEN_CELL_OPEN) {
            let (mut row_span, mut col_span) = (1usize, 1usize);
            if token != TOKEN_PLAIN_CELL {
                let mut j = i + 1;
                while j < tokens.len() && !tokens[j].starts_with('>') {
                    if let Some(v) = span_value(&tokens[j], "colspan") {
                        col_span = v;
                    } else if let Some(v) = span_value(&tokens[j], "rowspan") {
                        row_span = v;
                    }
                    j += 1;
                }
                i = j;
            }
            while occupied.contains(&(row, col)) {
                col += 1;
            }
            for r in row..row + row_span {
                for c in col..col + col_span {
                    occupied.insert((r, c));
                }
            }
            table.rows = table.rows.max(row + row_span);
            table.cols = table.cols.max(col + col_span);
            table.cells.push(TableCell {
                row,
                col,
                row_span,
                col_span,
                bbox: boxes.get(cell_index).copied().flatten(),
                text: String::new(),
            });
            cell_index += 1;
            col += col_span;
        }
        i += 1;
    }
    table
}

/// 一段 OCR 文字（原图像素坐标）。
#[derive(Debug, Clone, PartialEq)]
pub struct OcrPiece {
    /// 外接框 `[x1, y1, x2, y2]`。
    pub rect: [f32; 4],
    /// 文字。
    pub text: String,
}

/// 两个框的 IoU（无交集为 0）。
fn iou(a: &[f32; 4], b: &[f32; 4]) -> f32 {
    let area = |r: &[f32; 4]| (r[2] - r[0]).max(0.0) * (r[3] - r[1]).max(0.0);
    let left = a[0].max(b[0]);
    let right = a[2].min(b[2]);
    let top = a[1].max(b[1]);
    let bottom = a[3].min(b[3]);
    if left >= right || top >= bottom {
        return 0.0;
    }
    let inter = (right - left) * (bottom - top);
    inter / (area(a) + area(b) - inter)
}

/// RapidTable 用的 L1 距离（角点距离再加较小的一侧）。
fn corner_distance(a: &[f32; 4], b: &[f32; 4]) -> f32 {
    let head = (b[0] - a[0]).abs() + (b[1] - a[1]).abs();
    let tail = (b[2] - a[2]).abs() + (b[3] - a[3]).abs();
    head + tail + head.min(tail)
}

/// 把 OCR 文字块归入单元格并填好文字。
///
/// 每个文字块归到 IoU 最大（并列时距离最近）的单元格；与所有单元格都无交集的丢弃；
/// 完全在表格第一行上方的文字块（标题等）丢弃。
///
/// # 参数
/// - `table`：待填的表格。
/// - `pieces`：OCR 文字块，按阅读顺序。
pub fn fill_text(table: &mut Table, pieces: &[OcrPiece]) {
    let top = table
        .cells
        .iter()
        .filter_map(|c| c.bbox.map(|b| b[1]))
        .fold(f32::MAX, f32::min);
    let mut matched: Vec<Vec<usize>> = vec![Vec::new(); table.cells.len()];
    for (index, piece) in pieces.iter().enumerate() {
        if piece.rect[3] < top {
            continue;
        }
        let best = table
            .cells
            .iter()
            .enumerate()
            .filter_map(|(cell, c)| {
                c.bbox.map(|b| {
                    (
                        cell,
                        1.0 - iou(&piece.rect, &b),
                        corner_distance(&piece.rect, &b),
                    )
                })
            })
            .min_by(|a, b| {
                a.1.total_cmp(&b.1)
                    .then(a.2.total_cmp(&b.2))
                    .then(a.0.cmp(&b.0))
            });
        if let Some((cell, miss, _)) = best
            && miss < 1.0 - MIN_IOU
        {
            matched[cell].push(index);
        }
    }
    for (cell, indexes) in table.cells.iter_mut().zip(matched) {
        cell.text = join_pieces(indexes.iter().map(|&i| pieces[i].text.as_str()));
    }
}

/// 同一单元格里的多段文字：去掉每段开头的一个空格，段间补一个空格。
fn join_pieces<'a>(parts: impl Iterator<Item = &'a str>) -> String {
    let parts: Vec<&str> = parts.collect();
    let mut out = String::new();
    for (i, part) in parts.iter().enumerate() {
        let part = if parts.len() > 1 {
            part.strip_prefix(' ').unwrap_or(part)
        } else {
            part
        };
        if part.is_empty() {
            continue;
        }
        out.push_str(part);
        if i + 1 < parts.len() && !part.ends_with(' ') {
            out.push(' ');
        }
    }
    out.trim_end().to_string()
}

/// 展开成 `行 x 列` 的文字网格（跨行跨列的格子文字放在左上角，其余留空）。
fn grid(table: &Table, clean: fn(&str) -> String) -> Vec<Vec<String>> {
    let mut rows = vec![vec![String::new(); table.cols]; table.rows];
    for cell in &table.cells {
        if let Some(slot) = rows.get_mut(cell.row).and_then(|r| r.get_mut(cell.col)) {
            *slot = clean(&cell.text);
        }
    }
    rows
}

/// Markdown 单元格转义：竖线加反斜杠，换行变空格。
fn markdown_cell(text: &str) -> String {
    text.replace('|', "\\|").replace(['\r', '\n'], " ")
}

/// TSV 单元格清理：制表符与换行变空格。
fn tsv_cell(text: &str) -> String {
    text.replace(['\t', '\r', '\n'], " ")
}

/// HTML 转义。
fn html_escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

/// 输出 Markdown 表格（第一行作表头；Markdown 没有跨行跨列，被合并的格子留空）。
///
/// # 参数
/// - `table`：已填好文字的表格。
///
/// # 返回
/// Markdown 文本；空表格返回空串。
pub fn to_markdown(table: &Table) -> String {
    let rows = grid(table, markdown_cell);
    let Some(header) = rows.first() else {
        return String::new();
    };
    let line = |cells: &[String]| format!("| {} |", cells.join(" | "));
    let mut out = vec![line(header)];
    out.push(line(&vec!["---".to_string(); table.cols]));
    out.extend(rows.iter().skip(1).map(|r| line(r)));
    out.join("\n")
}

/// 输出 TSV（制表符分列，换行分行；被合并的格子留空）。
///
/// # 参数
/// - `table`：已填好文字的表格。
pub fn to_tsv(table: &Table) -> String {
    grid(table, tsv_cell)
        .iter()
        .map(|r| r.join("\t"))
        .collect::<Vec<_>>()
        .join("\n")
}

/// 输出 HTML 表格（保留 `rowspan` / `colspan`）。
///
/// # 参数
/// - `table`：已填好文字的表格。
pub fn to_html(table: &Table) -> String {
    let mut out = String::from("<table>\n");
    for row in 0..table.rows {
        out.push_str("<tr>");
        for cell in table.cells.iter().filter(|c| c.row == row) {
            out.push_str("<td");
            if cell.row_span > 1 {
                out.push_str(&format!(" rowspan=\"{}\"", cell.row_span));
            }
            if cell.col_span > 1 {
                out.push_str(&format!(" colspan=\"{}\"", cell.col_span));
            }
            out.push('>');
            out.push_str(&html_escape(&cell.text));
            out.push_str("</td>");
        }
        out.push_str("</tr>\n");
    }
    out.push_str("</table>");
    out
}

/// 表格的三种文本形式。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TableTexts {
    /// Markdown。
    pub markdown: String,
    /// TSV（可直接粘进电子表格）。
    pub tsv: String,
    /// HTML。
    pub html: String,
}

impl TableTexts {
    /// 由表格生成三种文本。
    ///
    /// # 参数
    /// - `table`：已填好文字的表格。
    pub fn from_table(table: &Table) -> Self {
        Self {
            markdown: to_markdown(table),
            tsv: to_tsv(table),
            html: to_html(table),
        }
    }
}

/// 一次结构解码 + 文字合并的完整纯函数流程（不含推理）。
///
/// # 参数
/// - `output`：工作进程响应。
/// - `scale`：预处理的缩放比例。
/// - `pieces`：OCR 文字块（原图像素坐标）。
///
/// # 返回
/// 填好文字的表格；没有解出任何单元格返回 `Ok(None)`（图里没有表格）。
pub fn assemble_table(
    output: &WorkerOutput,
    scale: f32,
    pieces: &[OcrPiece],
) -> Result<Option<Table>, String> {
    let structure = decode_structure(output, scale)?;
    let mut table = layout_cells(&structure.tokens, &structure.cell_boxes);
    if table.cells.is_empty() {
        return Ok(None);
    }
    fill_text(&mut table, pieces);
    Ok(Some(table))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 构造词表文本（与官方模型元数据同形：无 `<td>`、末尾没有 `<td></td>`）。
    fn character() -> String {
        [
            "<thead>",
            "</thead>",
            "<tbody>",
            "</tbody>",
            "<tr>",
            "</tr>",
            "<td",
            ">",
            "</td>",
            " colspan=\"2\"",
            " rowspan=\"2\"",
        ]
        .join("\n")
    }

    /// 生成一个输出：`steps` 里每项是 (token 文本, 框的 8 个归一化坐标)。
    fn output_for(steps: &[(&str, [f32; 8])]) -> WorkerOutput {
        let vocab = build_vocab(&character());
        let width = vocab.len();
        let mut probs = Vec::new();
        let mut locs = Vec::new();
        let mut all = vec![("sos", [0.0; 8])];
        all.extend_from_slice(steps);
        all.push(("eos", [0.0; 8]));
        for (token, coords) in &all {
            let target = vocab.iter().position(|t| t == token).expect("词表里有");
            let mut row = vec![0.01; width];
            row[target] = 0.9;
            probs.extend(row);
            locs.extend(coords);
        }
        let n = all.len() as i64;
        WorkerOutput {
            character: character(),
            outputs: vec![
                WorkerTensor {
                    name: "loc".into(),
                    shape: vec![1, n, 8],
                    data: locs,
                },
                WorkerTensor {
                    name: "probs".into(),
                    shape: vec![1, n, width as i64],
                    data: probs,
                },
            ],
        }
    }

    /// 归一化框：给定像素框和缩放（scale=1 时图边长 488）转成 8 个坐标。
    fn quad(x1: f32, y1: f32, x2: f32, y2: f32) -> [f32; 8] {
        let s = MODEL_SIDE as f32;
        [
            x1 / s,
            y1 / s,
            x2 / s,
            y1 / s,
            x2 / s,
            y2 / s,
            x1 / s,
            y2 / s,
        ]
    }

    /// 词表补 `<td></td>`、去 `<td>`，首尾是 sos / eos。
    #[test]
    fn vocab_layout() {
        let v = build_vocab(&character());
        assert_eq!(v.first().map(String::as_str), Some("sos"));
        assert_eq!(v.last().map(String::as_str), Some("eos"));
        assert!(v.iter().any(|t| t == "<td></td>"));
        assert!(!v.iter().any(|t| t == "<td>"));
        let with_legacy = build_vocab("<td>\n<tr>");
        assert_eq!(with_legacy, ["sos", "<tr>", "<td></td>", "eos"]);
    }

    /// 预处理：形状、缩放、补零与通道顺序（B 在第 0 通道）。
    #[test]
    fn prepare_input_shape_and_channels() {
        // 2x1 纯红图：最长边 2 → 放大到 488x244
        let rgba = [255, 0, 0, 255, 255, 0, 0, 255];
        let p = prepare_input(2, 1, &rgba).expect("可处理");
        assert_eq!(p.tensor.len(), 3 * MODEL_SIDE * MODEL_SIDE);
        assert!((p.scale - 244.0).abs() < 1e-3);
        let plane = MODEL_SIDE * MODEL_SIDE;
        let b = (0.0 - MEAN[0]) / STD[0];
        let r = (1.0 - MEAN[2]) / STD[2];
        assert!((p.tensor[0] - b).abs() < 1e-4, "通道 0 是 B");
        assert!((p.tensor[2 * plane] - r).abs() < 1e-4, "通道 2 是 R");
        // 高只有 244，之后的行是补零
        assert_eq!(p.tensor[250 * MODEL_SIDE], 0.0);
        assert!(prepare_input(0, 1, &[]).is_none());
        assert!(prepare_input(2, 2, &[0; 4]).is_none());
    }

    /// 解码：跳过 sos / eos，只给单元格标记配框，框按缩放还原成像素外接框。
    #[test]
    fn decode_scales_boxes_and_stops_at_eos() {
        let out = output_for(&[
            ("<tr>", [0.0; 8]),
            ("<td", quad(10.0, 20.0, 100.0, 60.0)),
            (">", [0.0; 8]),
            ("</td>", [0.0; 8]),
            ("<td></td>", [0.0; 8]),
            ("</tr>", [0.0; 8]),
        ]);
        let s = decode_structure(&out, 1.0).expect("可解码");
        assert_eq!(
            s.tokens,
            ["<tr>", "<td", ">", "</td>", "<td></td>", "</tr>"]
        );
        assert_eq!(s.cell_boxes.len(), 2);
        let b = s.cell_boxes[0].expect("有框");
        assert!((b[0] - 10.0).abs() < 1e-3 && (b[3] - 60.0).abs() < 1e-3);
        assert_eq!(s.cell_boxes[1], None, "全零占位框");
        assert!(s.score > 0.8);
        // scale 0.5：模型 488 对应原图 976
        let half = decode_structure(&out, 0.5).expect("可解码");
        assert!((half.cell_boxes[0].expect("有框")[0] - 20.0).abs() < 1e-3);
    }

    /// 缺输出或形状不符时报错而不是 panic。
    #[test]
    fn decode_rejects_bad_output() {
        let mut out = output_for(&[("<tr>", [0.0; 8])]);
        out.outputs.remove(1);
        assert!(decode_structure(&out, 1.0).is_err());
        let mut short = output_for(&[("<tr>", [0.0; 8])]);
        short.outputs[1].data.truncate(3);
        assert!(decode_structure(&short, 1.0).is_err());
    }

    /// 行列还原：colspan / rowspan 占位后后续格子顺延。
    #[test]
    fn layout_handles_spans() {
        let tokens: Vec<String> = [
            "<tr>",
            "<td",
            " colspan=\"2\"",
            ">",
            "</td>",
            "<td></td>",
            "</tr>",
            "<tr>",
            "<td",
            " rowspan=\"2\"",
            ">",
            "</td>",
            "<td></td>",
            "<td></td>",
            "</tr>",
            "<tr>",
            "<td></td>",
            "<td></td>",
            "</tr>",
        ]
        .iter()
        .map(|s| (*s).to_string())
        .collect();
        let t = layout_cells(&tokens, &[]);
        assert_eq!((t.rows, t.cols), (3, 3));
        let at = |r: usize, c: usize| t.cells.iter().find(|x| x.row == r && x.col == c);
        assert_eq!(at(0, 0).map(|c| c.col_span), Some(2));
        assert!(at(0, 2).is_some());
        assert_eq!(at(1, 0).map(|c| c.row_span), Some(2));
        // 第三行：第 0 列被上面的 rowspan 占住，所以从第 1 列开始
        assert!(at(2, 0).is_none());
        assert!(at(2, 1).is_some() && at(2, 2).is_some());
    }

    /// 匹配：按 IoU 归格、同格多段合并、表头上方的标题与无交集的文字丢弃。
    #[test]
    fn fill_matches_pieces() {
        let tokens: Vec<String> = [
            "<tr>",
            "<td></td>",
            "<td></td>",
            "</tr>",
            "<tr>",
            "<td></td>",
            "<td></td>",
            "</tr>",
        ]
        .iter()
        .map(|s| (*s).to_string())
        .collect();
        let boxes = [
            Some([0.0, 50.0, 100.0, 90.0]),
            Some([100.0, 50.0, 200.0, 90.0]),
            Some([0.0, 90.0, 100.0, 130.0]),
            Some([100.0, 90.0, 200.0, 130.0]),
        ];
        let mut t = layout_cells(&tokens, &boxes);
        let piece = |x1: f32, y1: f32, x2: f32, y2: f32, s: &str| OcrPiece {
            rect: [x1, y1, x2, y2],
            text: s.to_string(),
        };
        fill_text(
            &mut t,
            &[
                piece(10.0, 0.0, 190.0, 30.0, "标题"),
                piece(10.0, 55.0, 90.0, 85.0, "姓名"),
                piece(110.0, 55.0, 150.0, 85.0, "年龄"),
                piece(152.0, 55.0, 190.0, 85.0, " 岁"),
                piece(10.0, 95.0, 90.0, 125.0, "张三"),
                piece(300.0, 95.0, 400.0, 125.0, "表外"),
            ],
        );
        let texts: Vec<&str> = t.cells.iter().map(|c| c.text.as_str()).collect();
        assert_eq!(texts, ["姓名", "年龄 岁", "张三", ""]);
    }

    /// 三种输出：Markdown 转义竖线、TSV 去制表符、HTML 转义并保留跨度。
    #[test]
    fn renderers() {
        let t = Table {
            rows: 2,
            cols: 2,
            cells: vec![
                TableCell {
                    row: 0,
                    col: 0,
                    row_span: 1,
                    col_span: 2,
                    bbox: None,
                    text: "A|B".into(),
                },
                TableCell {
                    row: 1,
                    col: 0,
                    row_span: 1,
                    col_span: 1,
                    bbox: None,
                    text: "x\ty".into(),
                },
                TableCell {
                    row: 1,
                    col: 1,
                    row_span: 1,
                    col_span: 1,
                    bbox: None,
                    text: "<1&2>".into(),
                },
            ],
        };
        assert_eq!(
            to_markdown(&t),
            "| A\\|B |  |\n| --- | --- |\n| x\ty | <1&2> |"
        );
        assert_eq!(to_tsv(&t), "A|B\t\nx y\t<1&2>");
        let html = to_html(&t);
        assert!(html.contains("<td colspan=\"2\">A|B</td>"));
        assert!(html.contains("<td>&lt;1&amp;2&gt;</td>"));
        assert_eq!(to_markdown(&Table::default()), "");
        assert_eq!(TableTexts::from_table(&t).tsv, to_tsv(&t));
    }

    /// 端到端（纯函数部分）：假模型输出 + 假 OCR 框 → Markdown / TSV。
    #[test]
    fn assemble_end_to_end() {
        let out = output_for(&[
            ("<tr>", [0.0; 8]),
            ("<td></td>", quad(0.0, 0.0, 100.0, 40.0)),
            ("<td></td>", quad(100.0, 0.0, 200.0, 40.0)),
            ("</tr>", [0.0; 8]),
            ("<tr>", [0.0; 8]),
            ("<td></td>", quad(0.0, 40.0, 100.0, 80.0)),
            ("<td></td>", quad(100.0, 40.0, 200.0, 80.0)),
            ("</tr>", [0.0; 8]),
        ]);
        let pieces: Vec<OcrPiece> = [
            ("名称", [5.0, 5.0, 95.0, 35.0]),
            ("数量", [105.0, 5.0, 195.0, 35.0]),
            ("苹果", [5.0, 45.0, 95.0, 75.0]),
            ("3", [105.0, 45.0, 195.0, 75.0]),
        ]
        .iter()
        .map(|(t, r)| OcrPiece {
            rect: *r,
            text: (*t).to_string(),
        })
        .collect();
        let table = assemble_table(&out, 1.0, &pieces)
            .expect("可解码")
            .expect("有表格");
        assert_eq!(to_tsv(&table), "名称\t数量\n苹果\t3");
        assert_eq!(
            to_markdown(&table),
            "| 名称 | 数量 |\n| --- | --- |\n| 苹果 | 3 |"
        );
        let none = output_for(&[]);
        assert_eq!(assemble_table(&none, 1.0, &pieces), Ok(None));
    }

    /// 工作进程 JSON 可解析，坏 JSON 报错。
    #[test]
    fn worker_json() {
        let json = br#"{"character":"<tr>","outputs":[{"name":"a","shape":[1,1,8],"data":[0.0]}]}"#;
        assert_eq!(parse_worker_output(json).expect("可解析").outputs.len(), 1);
        assert!(parse_worker_output(b"nope").is_err());
    }
}
